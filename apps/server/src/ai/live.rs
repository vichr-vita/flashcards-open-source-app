use super::chat::{self, ChatItem};
use crate::{AppState, auth, config::Config, core, database::scoped, error::ApiError};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse as _, Response,
        sse::{Event, KeepAlive, Sse},
    },
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{collections::HashMap, convert::Infallible, time::Duration};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Authorization {
    version: u8,
    user_id: Uuid,
    workspace_id: Uuid,
    session_id: Uuid,
    run_id: Uuid,
    expires_at: i64,
    #[serde(default)]
    trace_context: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Request {
    session_id: Option<Uuid>,
    run_id: Option<Uuid>,
    after_cursor: Option<String>,
}

fn invalid() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "CHAT_LIVE_AUTH_INVALID",
        "Chat live auth token is invalid",
    )
}

fn signature(config: &Config, payload: &[u8]) -> Result<String, ApiError> {
    let mut derivation = Hmac::<Sha256>::new_from_slice(config.csrf_secret.as_bytes())
        .map_err(|_| ApiError::internal())?;
    derivation.update(b"local-chat-live-v1");
    let secret =
        derivation
            .finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut output, byte| {
                use std::fmt::Write as _;
                let _ = write!(&mut output, "{byte:02x}");
                output
            });
    let mut signer =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| ApiError::internal())?;
    signer.update(payload);
    Ok(URL_SAFE_NO_PAD.encode(signer.finalize().into_bytes()))
}

pub(super) fn envelope(
    config: &Config,
    user: Uuid,
    workspace: Uuid,
    session: Uuid,
    run: Uuid,
) -> Result<Value, ApiError> {
    let expires_at = Utc::now().timestamp_millis().saturating_add(600_000);
    let authorization = Authorization {
        version: 1,
        user_id: user,
        workspace_id: workspace,
        session_id: session,
        run_id: run,
        expires_at,
        trace_context: None,
    };
    let payload = URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&authorization).map_err(|_| ApiError::internal())?);
    let signature = signature(config, payload.as_bytes())?;
    let url = std::env::var("CHAT_LIVE_URL").map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHAT_LIVE_UNAVAILABLE",
            "AI live stream is unavailable for the active run.",
        )
    })?;
    let parsed = url::Url::parse(&url).map_err(|_| ApiError::internal())?;
    if parsed.scheme() != "https"
        && !(config.allow_http
            && parsed.scheme() == "http"
            && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1")))
    {
        return Err(ApiError::internal());
    }
    Ok(
        json!({"url":url,"authorization":format!("Live {payload}.{signature}"),"expiresAt":expires_at}),
    )
}

fn verify(
    config: &Config,
    headers: &HeaderMap,
    session: Uuid,
    run: Uuid,
) -> Result<Authorization, ApiError> {
    use subtle::ConstantTimeEq as _;
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Live "))
        .ok_or_else(invalid)?;
    let (payload, actual) = token.trim().split_once('.').ok_or_else(invalid)?;
    if payload.len() > 8192 || actual.len() > 100 {
        return Err(invalid());
    }
    let expected = signature(config, payload.as_bytes())?;
    if !bool::from(expected.as_bytes().ct_eq(actual.as_bytes())) {
        return Err(invalid());
    }
    let authorization: Authorization =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).map_err(|_| invalid())?)
            .map_err(|_| invalid())?;
    if authorization.expires_at <= Utc::now().timestamp_millis() {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "CHAT_LIVE_AUTH_EXPIRED",
            "Chat live auth token expired",
        ));
    }
    if authorization.version != 1
        || authorization.session_id != session
        || authorization.run_id != run
    {
        return Err(invalid());
    }
    Ok(authorization)
}

struct Emitter {
    sender: mpsc::Sender<Result<Event, Infallible>>,
    session: Uuid,
    run: Uuid,
    epoch: Uuid,
    sequence: u64,
}

impl Emitter {
    async fn send(&mut self, mut payload: Value) -> bool {
        self.sequence = self.sequence.saturating_add(1);
        let Some(map) = payload.as_object_mut() else {
            return false;
        };
        map.insert("sessionId".to_owned(), json!(self.session));
        map.insert("conversationScopeId".to_owned(), json!(self.session));
        map.insert("runId".to_owned(), json!(self.run));
        map.insert("streamEpoch".to_owned(), json!(self.epoch));
        map.insert("sequenceNumber".to_owned(), json!(self.sequence));
        let Some(kind) = map.get("type").and_then(Value::as_str) else {
            return false;
        };
        let event = Event::default().event(kind).data(payload.to_string());
        self.sender.send(Ok(event)).await.is_ok()
    }
}

fn diff(previous: &[Value], current: &[Value], item: &ChatItem) -> Vec<Value> {
    let mut events = Vec::new();
    for (index, part) in current.iter().enumerate() {
        let kind = part.get("type").and_then(Value::as_str);
        if kind == Some("text") {
            let old = previous
                .get(index)
                .filter(|old| old.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|old| old.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
            let delta = text.strip_prefix(old).unwrap_or(text);
            if !delta.is_empty() {
                events.push(json!({"type":"assistant_delta","text":delta,"cursor":item.item_order.to_string(),"itemId":item.item_id}));
            }
        }
        if kind == Some("tool_call") {
            let id = part.get("id").and_then(Value::as_str).unwrap_or_default();
            let old = previous.iter().find(|old| {
                old.get("type").and_then(Value::as_str) == Some("tool_call")
                    && old.get("id").and_then(Value::as_str) == Some(id)
            });
            let output_index = part
                .get("streamPosition")
                .and_then(|position| position.get("outputIndex"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if old.is_none() {
                events.push(json!({"type":"assistant_tool_call","toolCallId":id,"name":part.get("name"),"status":"started","input":null,"output":null,"cursor":item.item_order.to_string(),"itemId":item.item_id,"outputIndex":output_index}));
            }
            if old != Some(part) && part.get("status").and_then(Value::as_str) == Some("completed")
            {
                events.push(json!({"type":"assistant_tool_call","toolCallId":id,"name":part.get("name"),"status":"completed","input":part.get("input"),"output":part.get("output"),"providerStatus":part.get("providerStatus"),"cursor":item.item_order.to_string(),"itemId":item.item_id,"outputIndex":output_index}));
            }
        }
        if kind == Some("reasoning_summary") {
            let id = part
                .get("streamPosition")
                .and_then(|position| position.get("itemId"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let old = previous.iter().find(|old| {
                old.get("type").and_then(Value::as_str) == Some("reasoning_summary")
                    && old
                        .get("streamPosition")
                        .and_then(|position| position.get("itemId"))
                        .and_then(Value::as_str)
                        == Some(id)
            });
            let output_index = part
                .get("streamPosition")
                .and_then(|position| position.get("outputIndex"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if old.is_none() {
                events.push(json!({"type":"assistant_reasoning_started","reasoningId":id,"cursor":item.item_order.to_string(),"itemId":item.item_id,"outputIndex":output_index}));
            }
            if old != Some(part)
                && part
                    .get("summary")
                    .and_then(Value::as_str)
                    .is_some_and(|summary| !summary.is_empty())
            {
                events.push(json!({"type":"assistant_reasoning_summary","reasoningId":id,"summary":part.get("summary"),"cursor":item.item_order.to_string(),"itemId":item.item_id,"outputIndex":output_index}));
            }
            if part.get("providerStatus").and_then(Value::as_str) == Some("completed")
                && old.is_none_or(|old| {
                    old.get("providerStatus").and_then(Value::as_str) != Some("completed")
                })
            {
                events.push(json!({"type":"assistant_reasoning_done","reasoningId":id,"cursor":item.item_order.to_string(),"itemId":item.item_id,"outputIndex":output_index}));
            }
        }
    }
    events
}

type RunPollState = (String, Uuid, Option<String>, Option<String>, i64);

async fn poll(
    state: AppState,
    authorization: Authorization,
    after: Option<i64>,
    attach: Option<(Uuid, i64)>,
    emitter: &mut Emitter,
) -> Result<(), ApiError> {
    let started = tokio::time::Instant::now();
    let mut previous = HashMap::<Uuid, Vec<Value>>::new();
    loop {
        if emitter.sender.is_closed() {
            return Ok(());
        }
        let mut tx = scoped(
            &state.pool,
            &authorization.user_id.to_string(),
            Some(&authorization.workspace_id.to_string()),
        )
        .await?;
        let mut session = chat::session(
            &mut tx,
            authorization.user_id,
            authorization.workspace_id,
            Some(authorization.session_id),
            false,
            None,
        )
        .await?;
        chat::recover(&mut tx, &mut session).await?;
        let run:Option<RunPollState> = sqlx::query_as("SELECT status,assistant_item_id,last_error_message,live_attach_client_id,live_attach_seq FROM ai.chat_runs WHERE run_id=$1 AND session_id=$2").bind(authorization.run_id).bind(authorization.session_id).fetch_optional(&mut *tx).await?;
        let all = chat::items(&mut tx, authorization.session_id).await?;
        tx.commit().await?;
        let Some((status, assistant, error, client, seq)) = run else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "CHAT_RUN_NOT_FOUND",
                "Chat run not found",
            ));
        };
        let superseded = attach.is_some_and(|(owner, owned_seq)| {
            client.as_deref() == Some(owner.to_string().as_str()) && seq > owned_seq
        });
        if superseded || started.elapsed() > Duration::from_mins(9) {
            let _=emitter.send(json!({"type":"run_terminal","outcome":"reset_required","cursor":null,"message":"Reload the conversation to continue."})).await;
            return Ok(());
        }
        let current = all.iter().find(|item| item.item_id == assistant);
        if let Some(item) =
            current.filter(|item| after.is_none_or(|cursor| item.item_order > cursor))
        {
            let content = item
                .payload
                .get("content")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let old = previous.get(&assistant).map_or(&[][..], Vec::as_slice);
            for event in diff(old, &content, item) {
                if !emitter.send(event).await {
                    return Ok(());
                }
            }
            previous.insert(assistant, content);
            if item.state != "in_progress" {
                let _=emitter.send(json!({"type":"assistant_message_done","cursor":item.item_order.to_string(),"itemId":item.item_id,"content":chat::wire_content(item.payload.get("content").unwrap_or(&Value::Null)),"isError":item.state=="error","isStopped":item.state=="cancelled"})).await;
            }
        }
        if !matches!(status.as_str(), "queued" | "running") {
            let outcome = match status.as_str() {
                "completed" => "completed",
                "cancelled" => "stopped",
                "failed" => "error",
                _ => "reset_required",
            };
            let cursor = current.map(|item| item.item_order.to_string());
            let _=emitter.send(json!({"type":"composer_suggestions_updated","cursor":cursor,"suggestions":session.composer_suggestions})).await;
            let _=emitter.send(json!({"type":"run_terminal","outcome":outcome,"cursor":cursor,"message":error,"assistantItemId":assistant,"isError":status=="failed"||status=="interrupted","isStopped":status=="cancelled"})).await;
            return Ok(());
        }
        tokio::select! { ()=emitter.sender.closed()=>return Ok(()), ()=tokio::time::sleep(Duration::from_millis(750))=>{} }
    }
}

async fn attach(
    state: &AppState,
    authorization: &Authorization,
    headers: &HeaderMap,
) -> Result<Option<(Uuid, i64)>, ApiError> {
    let mut tx = scoped(
        &state.pool,
        &authorization.user_id.to_string(),
        Some(&authorization.workspace_id.to_string()),
    )
    .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ai.chat_runs WHERE run_id=$1 AND session_id=$2)",
    )
    .bind(authorization.run_id)
    .bind(authorization.session_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "CHAT_RUN_NOT_FOUND",
            "Chat run not found",
        ));
    }
    let attach_client = headers
        .get("x-chat-live-client-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value.trim()).ok())
        .filter(|id| {
            id.get_variant() == uuid::Variant::RFC4122 && (1..=8).contains(&id.get_version_num())
        });
    let attach = if let Some(client) = attach_client {
        let seq:i64=sqlx::query_scalar("UPDATE ai.chat_runs SET live_attach_client_id=$2,live_attach_seq=live_attach_seq+1 WHERE run_id=$1 RETURNING live_attach_seq").bind(authorization.run_id).bind(client.to_string()).fetch_one(&mut *tx).await?;
        Some((client, seq))
    } else {
        None
    };
    tx.commit().await?;
    Ok(attach)
}

pub(super) async fn stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(request): Query<Request>,
) -> Result<Response, ApiError> {
    let session = request.session_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "CHAT_LIVE_SESSION_ID_REQUIRED",
            "AI live stream request is missing sessionId.",
        )
    })?;
    let run = request.run_id.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "CHAT_LIVE_RUN_ID_REQUIRED",
            "AI live stream request is missing runId.",
        )
    })?;
    let after = request
        .after_cursor
        .map(|cursor| {
            cursor
                .parse::<i64>()
                .ok()
                .filter(|cursor| *cursor >= 0)
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "CHAT_LIVE_AFTER_CURSOR_INVALID",
                        "AI live stream request has an invalid afterCursor.",
                    )
                })
        })
        .transpose()?;
    let authorization = verify(&state.config, &headers, session, run)?;
    let mut cookie_headers = headers.clone();
    cookie_headers.remove("authorization");
    let identity = auth::authenticate(&state, &cookie_headers).await?;
    if authorization.user_id != identity.user_id {
        return Err(ApiError::forbidden("This chat belongs to another account."));
    }
    let _ = core::resolve_workspace(
        &state,
        &identity.user_id.to_string(),
        Some(authorization.workspace_id),
    )
    .await?;
    let attach = attach(&state, &authorization, &headers).await?;
    let (sender, receiver) = mpsc::channel(32);
    let mut emitter = Emitter {
        sender,
        session,
        run,
        epoch: Uuid::new_v4(),
        sequence: 0,
    };
    tokio::spawn(async move {
        if let Err(error) = poll(state, authorization, after, attach, &mut emitter).await {
            tracing::error!(code=%error.code,"Chat live polling failed");
            let _=emitter.send(json!({"type":"run_terminal","outcome":"reset_required","cursor":null,"message":"The stream was interrupted. Reload the conversation to continue."})).await;
        }
    });
    let mut response = Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response.headers_mut().insert(
        "cache-control",
        "no-store".parse().map_err(|_| ApiError::internal())?,
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        "no".parse().map_err(|_| ApiError::internal())?,
    );
    Ok(response)
}
