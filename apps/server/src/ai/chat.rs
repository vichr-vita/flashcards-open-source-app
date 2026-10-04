use super::{attachments, connection, live, private_json, worker};
use crate::{AppState, auth, core, database::scoped, error::ApiError};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Page {
    session_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    limit: Option<String>,
    before: Option<String>,
}

#[derive(FromRow)]
pub(super) struct ChatSession {
    pub session_id: Uuid,
    pub status: String,
    pub active_run_id: Option<Uuid>,
    pub active_run_heartbeat_at: Option<DateTime<Utc>>,
    pub composer_suggestions: Value,
    pub main_content_invalidation_version: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow, Clone)]
pub(super) struct ChatItem {
    pub item_id: Uuid,
    pub item_order: i64,
    pub state: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

pub(super) struct Job {
    pub user: Uuid,
    pub workspace: Uuid,
    pub session: Uuid,
    pub run: Uuid,
    pub reference: Option<connection::Reference>,
    pub api_key: Option<String>,
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "CHAT_SESSION_NOT_FOUND",
        "Chat session not found",
    )
}

pub(super) fn uuid_field(body: &Value, name: &str) -> Result<Option<Uuid>, ApiError> {
    body.get(name)
        .map(|value| {
            value
                .as_str()
                .and_then(|value| Uuid::parse_str(value).ok())
                .ok_or_else(|| ApiError::bad_request(format!("{name} must be a UUID")))
        })
        .transpose()
}

fn text<'a>(body: &'a Value, name: &str) -> Result<&'a str, ApiError> {
    body.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("{name} must be a non-empty string")))
}

fn suggestions(locale: Option<&str>) -> Result<Value, ApiError> {
    let catalog: Value = serde_json::from_str(include_str!("initial-suggestions.json"))
        .map_err(|_| ApiError::internal())?;
    let language = locale.unwrap_or("en").trim().replace('_', "-");
    let mut parsed = language
        .parse::<icu_locale::Locale>()
        .map_err(|_| ApiError::bad_request("uiLocale is invalid"))?;
    icu_locale::LocaleCanonicalizer::new_extended().canonicalize(&mut parsed);
    let language = parsed.to_string();
    let base = parsed.id.language.to_string();
    let region = parsed.id.region.map(|region| region.to_string());
    let script = parsed.id.script.map(|script| script.to_string());
    let fallback = match (base.as_str(), region.as_deref(), script.as_deref()) {
        ("nb", _, _) => "no",
        ("es", Some("ES"), _) => "es-ES",
        ("es", Some("MX"), _) => "es-MX",
        ("zh", _, Some("Hans")) | ("zh", Some("SG"), _) => "zh-Hans",
        ("zh", Some("CN"), _) => "zh-CN",
        (_, _, _) => base.as_str(),
    };
    let localized = catalog
        .get(&language)
        .or_else(|| catalog.get(fallback))
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("uiLocale is invalid"))?;
    Ok(Value::Array(localized.iter().enumerate().map(|(index, text)| json!({"id":format!("initial-{}",index.saturating_add(1)),"text":text,"source":"initial","assistantItemId":null})).collect()))
}

pub(super) fn config() -> Value {
    json!({"provider":{"id":"openai","label":"OpenAI"},"model":{"id":"gpt-6-sol","label":"GPT-6 Sol","badgeLabel":"GPT-6 Sol · Medium"},"reasoning":{"effort":"medium","label":"Medium"},"features":{"modelPickerEnabled":false,"dictationEnabled":true,"attachmentsEnabled":true},"liveUrl":std::env::var("CHAT_LIVE_URL").ok()})
}

pub(super) async fn session(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    workspace: Uuid,
    requested: Option<Uuid>,
    create: bool,
    locale: Option<&str>,
) -> Result<ChatSession, ApiError> {
    let existing = sqlx::query_as::<_, ChatSession>("SELECT session_id,status,active_run_id,active_run_heartbeat_at,composer_suggestions,main_content_invalidation_version,updated_at FROM ai.chat_sessions WHERE user_id=$1 AND workspace_id=$2 AND ($3::uuid IS NULL OR session_id=$3) ORDER BY created_at DESC,session_id DESC LIMIT 1 FOR UPDATE")
        .bind(user.to_string()).bind(workspace).bind(requested).fetch_optional(&mut **tx).await?;
    if let Some(existing) = existing {
        return Ok(existing);
    }
    if !create {
        return Err(not_found());
    }
    let id = requested.unwrap_or_else(Uuid::new_v4);
    let initial = suggestions(locale)?;
    let inserted = sqlx::query("INSERT INTO ai.chat_sessions(session_id,user_id,workspace_id,status,composer_suggestions) VALUES($1,$2,$3,'idle',$4) ON CONFLICT(session_id) DO NOTHING")
        .bind(id).bind(user.to_string()).bind(workspace).bind(&initial).execute(&mut **tx).await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "CHAT_SESSION_ID_CONFLICT",
            "This chat session ID belongs to another workspace.",
        ));
    }
    let generation: Uuid = sqlx::query_scalar("INSERT INTO ai.chat_composer_suggestion_generations(session_id,source,suggestions) VALUES($1,'initial',$2) RETURNING generation_id").bind(id).bind(&initial).fetch_one(&mut **tx).await?;
    sqlx::query("UPDATE ai.chat_sessions SET active_composer_suggestion_generation_id=$2 WHERE session_id=$1").bind(id).bind(generation).execute(&mut **tx).await?;
    sqlx::query_as("SELECT session_id,status,active_run_id,active_run_heartbeat_at,composer_suggestions,main_content_invalidation_version,updated_at FROM ai.chat_sessions WHERE session_id=$1").bind(id).fetch_one(&mut **tx).await.map_err(Into::into)
}

pub(super) async fn items(
    tx: &mut Transaction<'_, Postgres>,
    session: Uuid,
) -> Result<Vec<ChatItem>, ApiError> {
    sqlx::query_as("SELECT item_id,item_order,state,payload,created_at FROM ai.chat_items WHERE session_id=$1 ORDER BY item_order").bind(session).fetch_all(&mut **tx).await.map_err(Into::into)
}

pub(super) fn wire_content(content: &Value) -> Value {
    Value::Array(content.as_array().map_or_else(Vec::new, |content| {
        content
            .iter()
            .map(|part| {
                let mut part = part.clone();
                if let Some(map) = part.as_object_mut() {
                    if matches!(
                        map.get("type").and_then(Value::as_str),
                        Some("image" | "file")
                    ) {
                        map.insert("base64Data".to_owned(), json!(""));
                    }
                    if map.get("type").and_then(Value::as_str) == Some("card") {
                        map.insert("effortLevel".to_owned(), json!("fast"));
                    }
                }
                part
            })
            .collect()
    }))
}

pub(super) async fn recover(
    tx: &mut Transaction<'_, Postgres>,
    session: &mut ChatSession,
) -> Result<(), ApiError> {
    let Some(run) = session.active_run_id else {
        return Ok(());
    };
    let interrupted: Option<Uuid> = sqlx::query_scalar("UPDATE ai.chat_runs SET status='interrupted',finished_at=now(),last_error_message='AI response was interrupted. Send another message to continue.',updated_at=now() WHERE run_id=$1 AND status IN ('queued','running') AND coalesce(worker_heartbeat_at,created_at)<now()-interval '30 seconds' RETURNING assistant_item_id").bind(run).fetch_optional(&mut **tx).await?;
    if let Some(item) = interrupted {
        terminal_item(
            tx,
            item,
            "error",
            "interrupted",
            Some("AI response was interrupted. Send another message to continue."),
        )
        .await?;
        sqlx::query("UPDATE ai.chat_sessions SET status='interrupted',active_run_id=NULL,active_run_heartbeat_at=NULL,updated_at=now() WHERE session_id=$1 AND active_run_id=$2").bind(session.session_id).bind(run).execute(&mut **tx).await?;
        "interrupted".clone_into(&mut session.status);
        session.active_run_id = None;
        session.active_run_heartbeat_at = None;
    }
    Ok(())
}

/// Closes pending tool snapshots and replay pairs before a stopped turn enters history.
pub(super) async fn terminal_item(
    tx: &mut Transaction<'_, Postgres>,
    item: Uuid,
    state: &str,
    provider_status: &str,
    message: Option<&str>,
) -> Result<(), ApiError> {
    let Some(mut payload) = sqlx::query_scalar::<_, Value>(
        "SELECT payload FROM ai.chat_items WHERE item_id=$1 AND state='in_progress' FOR UPDATE",
    )
    .bind(item)
    .fetch_optional(&mut **tx)
    .await?
    else {
        return Ok(());
    };
    let tool_error=json!({"ok":false,"error":{"name":"CHAT_RUN_INTERRUPTED","message":message.unwrap_or("This tool call ended before its result was available.")},"instructions":"The result of this call is unknown. Read the current workspace before retrying a mutation."}).to_string();
    if let Some(content) = payload.get_mut("content").and_then(Value::as_array_mut) {
        for part in &mut *content {
            if part.get("type").and_then(Value::as_str) == Some("tool_call")
                && part.get("status").and_then(Value::as_str) == Some("started")
                && let Some(map) = part.as_object_mut()
            {
                map.insert("status".to_owned(), json!("completed"));
                map.insert("providerStatus".to_owned(), json!(provider_status));
                if map.get("output").is_none_or(Value::is_null) {
                    map.insert("output".to_owned(), json!(tool_error));
                }
            }
        }
        if let Some(message) = message {
            content.push(json!({"type":"text","text":message}));
        }
    }
    if let Some(replay) = payload.get_mut("openaiItems").and_then(Value::as_array_mut) {
        let pending: Vec<_> = replay
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("function_call"))
            .filter_map(|part| part.get("call_id").and_then(Value::as_str))
            .filter(|id| {
                !replay.iter().any(|part| {
                    part.get("type").and_then(Value::as_str) == Some("function_call_output")
                        && part.get("call_id").and_then(Value::as_str) == Some(*id)
                })
            })
            .map(str::to_owned)
            .collect();
        replay.extend(
            pending
                .iter()
                .map(|id| json!({"type":"function_call_output","call_id":id,"output":tool_error})),
        );
    }
    sqlx::query("UPDATE ai.chat_items SET state=$2,payload=$3,updated_at=now() WHERE item_id=$1 AND state='in_progress'").bind(item).bind(state).bind(payload).execute(&mut **tx).await?;
    Ok(())
}

fn envelope(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    session: &ChatSession,
    all: Vec<ChatItem>,
    page: Option<(usize, Option<i64>)>,
) -> Result<Value, ApiError> {
    let live_cursor = all
        .iter()
        .rev()
        .find(|item| item.state != "in_progress")
        .map(|item| item.item_order.to_string());
    let (selected, has_older) = if let Some((limit, before)) = page {
        let mut filtered: Vec<_> = all
            .into_iter()
            .filter(|item| before.is_none_or(|before| item.item_order < before))
            .collect();
        let has_older = filtered.len() > limit;
        filtered.drain(..filtered.len().saturating_sub(limit));
        (filtered, has_older)
    } else {
        (all, false)
    };
    let oldest = selected.first().map(|item| item.item_order.to_string());
    let messages: Vec<_> = selected.iter().map(|item| json!({"role":item.payload.get("role"),"content":wire_content(item.payload.get("content").unwrap_or(&Value::Null)),"timestamp":item.created_at.timestamp_millis(),"isError":item.state=="error","isStopped":item.state=="cancelled","cursor":item.item_order.to_string(),"itemId":if item.payload.get("role").and_then(Value::as_str)==Some("assistant"){Some(item.item_id)}else{None}})).collect();
    let active = match session.active_run_id {
        Some(run) if session.status == "running" => Some(
            json!({"runId":run,"status":"running","live":{"cursor":live_cursor,"stream":live::envelope(&state.config,user,workspace,session.session_id,run)?},"lastHeartbeatAt":session.active_run_heartbeat_at.map(|heartbeat|heartbeat.timestamp_millis())}),
        ),
        _ => None,
    };
    let mut conversation = json!({"messages":messages,"updatedAt":session.updated_at.timestamp_millis(),"mainContentInvalidationVersion":session.main_content_invalidation_version});
    if page.is_some()
        && let Some(map) = conversation.as_object_mut()
    {
        map.insert("hasOlder".to_owned(), json!(has_older));
        map.insert("oldestCursor".to_owned(), json!(oldest));
    }
    Ok(
        json!({"sessionId":session.session_id,"conversationScopeId":session.session_id,"conversation":conversation,"composerSuggestions":session.composer_suggestions,"chatConfig":config(),"activeRun":active}),
    )
}

pub(super) async fn history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<Page>,
) -> Result<Response, ApiError> {
    let identity = auth::authenticate(&state, &headers).await?;
    let workspace =
        core::resolve_workspace(&state, &identity.user_id.to_string(), query.workspace_id).await?;
    let page = query
        .limit
        .map(|limit| {
            let limit = limit.parse::<usize>().unwrap_or(7).clamp(1, 50);
            let before = query
                .before
                .map(|before| {
                    before
                        .parse::<i64>()
                        .ok()
                        .filter(|before| *before >= 0)
                        .ok_or_else(|| ApiError::bad_request("Invalid before cursor"))
                })
                .transpose()?;
            Ok::<_, ApiError>((limit, before))
        })
        .transpose()?;
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    let mut session = session(
        &mut tx,
        identity.user_id,
        workspace,
        query.session_id,
        query.session_id.is_none(),
        None,
    )
    .await?;
    recover(&mut tx, &mut session).await?;
    let all = items(&mut tx, session.session_id).await?;
    tx.commit().await?;
    Ok(private_json(envelope(
        &state,
        identity.user_id,
        workspace,
        &session,
        all,
        page,
    )?))
}

pub(super) async fn new(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let workspace = core::resolve_workspace(
        &state,
        &identity.user_id.to_string(),
        uuid_field(&body, "workspaceId")?,
    )
    .await?;
    let requested = uuid_field(&body, "sessionId")?;
    let locale = body
        .get("uiLocale")
        .map(|_| text(&body, "uiLocale"))
        .transpose()?;
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    let mut current = session(
        &mut tx,
        identity.user_id,
        workspace,
        requested,
        true,
        locale,
    )
    .await?;
    if requested.is_none() {
        recover(&mut tx, &mut current).await?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ai.chat_items WHERE session_id=$1")
                .bind(current.session_id)
                .fetch_one(&mut *tx)
                .await?;
        if count > 0 || current.status != "idle" {
            if let Some(run) = current.active_run_id {
                cancel(&mut tx, current.session_id, run).await?;
            }
            current = session(
                &mut tx,
                identity.user_id,
                workspace,
                Some(Uuid::new_v4()),
                true,
                locale,
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(private_json(
        json!({"ok":true,"sessionId":current.session_id,"composerSuggestions":current.composer_suggestions,"chatConfig":config()}),
    ))
}

fn content(body: &Value) -> Result<Vec<Value>, ApiError> {
    let parts = body
        .get("content")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| ApiError::bad_request("content must be a non-empty array"))?;
    let mut parsed = Vec::new();
    for part in parts {
        let mut part = part.clone();
        match text(&part, "type")? {
            "text" => {
                let _ = text(&part, "text")?;
            }
            "image" | "file" => attachments::validate(&mut part)?,
            "card" => {
                let _ = text(&part, "cardId")?;
                if !["frontText", "backText"]
                    .iter()
                    .all(|key| part.get(key).is_some_and(Value::is_string))
                    || !part
                        .get("tags")
                        .and_then(Value::as_array)
                        .is_some_and(|tags| {
                            tags.iter()
                                .all(|tag| tag.as_str().is_some_and(|tag| !tag.is_empty()))
                        })
                {
                    return Err(ApiError::bad_request("Invalid card content"));
                }
            }
            "tool_call" => {
                let _ = text(&part, "id")?;
                let _ = text(&part, "name")?;
                if !matches!(
                    part.get("status").and_then(Value::as_str),
                    Some("started" | "completed")
                ) {
                    return Err(ApiError::bad_request("Invalid tool call status"));
                }
            }
            _ => return Err(ApiError::bad_request("content.type is invalid")),
        }
        parsed.push(part);
    }
    Ok(parsed)
}

pub(super) fn api_key(headers: &HeaderMap, subscription: bool) -> Result<Option<String>, ApiError> {
    if subscription {
        return Ok(None);
    }
    headers.get("x-openai-api-key").map(|value| value.to_str().ok().map(str::trim).filter(|key| !key.is_empty() && key.len() <= 512).map(str::to_owned).ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST,"OPENAI_API_KEY_INVALID","The x-openai-api-key header must be a non-empty OpenAI API key of at most 512 characters."))).transpose()
}

struct RunInput<'a> {
    user: Uuid,
    workspace: Uuid,
    request: &'a str,
    timezone: &'a str,
    locale: Option<&'a str>,
    content: &'a [Value],
}

async fn cost_policy(
    tx: &mut Transaction<'_, Postgres>,
    turn: &RunInput<'_>,
) -> Result<(i64, i64), ApiError> {
    sqlx::query_as("WITH chat_activity AS (SELECT count(*)::bigint AS turns FROM ai.chat_sessions s JOIN ai.chat_runs r USING(session_id) WHERE s.user_id=$1 AND s.workspace_id=$2 AND r.created_at>=now()-interval '7 days'), daily_reviews AS (SELECT timezone($3,e.reviewed_at_server)::date AS day,count(*) AS reviews FROM sync.workspace_replicas r JOIN content.review_events e USING(workspace_id,replica_id) WHERE r.workspace_id=$2 AND r.user_id=$1 AND r.actor_kind='client_installation' AND e.reviewed_at_server>=now()-interval '7 days' GROUP BY day) SELECT turns,(SELECT count(*)::bigint FROM daily_reviews WHERE reviews>=3) FROM chat_activity")
        .bind(turn.user.to_string()).bind(turn.workspace).bind(turn.timezone).fetch_one(&mut **tx).await.map_err(Into::into)
}

async fn insert_run(
    tx: &mut Transaction<'_, Postgres>,
    current: &mut ChatSession,
    turn: &RunInput<'_>,
) -> Result<Uuid, ApiError> {
    let (turns, review_days) = cost_policy(tx, turn).await?;
    let low_cost = turns >= 20 && review_days < 2;
    sqlx::query("INSERT INTO ai.chat_items(session_id,item_kind,state,payload) VALUES($1,'message','completed',$2)").bind(current.session_id).bind(json!({"role":"user","content":turn.content})).execute(&mut **tx).await?;
    let assistant: Uuid = sqlx::query_scalar("INSERT INTO ai.chat_items(session_id,item_kind,state,payload) VALUES($1,'message','in_progress',$2) RETURNING item_id").bind(current.session_id).bind(json!({"role":"assistant","content":[]})).fetch_one(&mut **tx).await?;
    let run: Uuid = sqlx::query_scalar("INSERT INTO ai.chat_runs(session_id,assistant_item_id,status,request_id,model_id,reasoning_effort,timezone,turn_input,ui_locale,initiating_auth_is_signed_in,client_platform,chat_turns_last_7d,good_review_days_last_7d,ai_cost_mode) VALUES($1,$2,'queued',$3,$4,$5,$6,$7,$8,true,'web',$9,$10,$11) RETURNING run_id")
            .bind(current.session_id).bind(assistant).bind(turn.request).bind(if low_cost {"gpt-6-luna"} else {"gpt-6-sol"}).bind(if low_cost {"high"} else {"medium"}).bind(turn.timezone).bind(json!(turn.content)).bind(turn.locale).bind(i32::try_from(turns).map_err(|_|ApiError::internal())?).bind(i32::try_from(review_days).map_err(|_|ApiError::internal())?).bind(if low_cost {"low_cost"}else{"normal"}).fetch_one(&mut **tx).await?;
    sqlx::query("UPDATE ai.chat_composer_suggestion_generations SET invalidated_at=now(),invalidated_reason='run_started' WHERE generation_id=$1 AND invalidated_at IS NULL").bind(sqlx::query_scalar::<_,Option<Uuid>>("SELECT active_composer_suggestion_generation_id FROM ai.chat_sessions WHERE session_id=$1").bind(current.session_id).fetch_one(&mut **tx).await?).execute(&mut **tx).await?;
    sqlx::query("UPDATE ai.chat_sessions SET status='running',active_run_id=$2,active_run_heartbeat_at=now(),composer_suggestions='[]',active_composer_suggestion_generation_id=NULL,updated_at=now() WHERE session_id=$1").bind(current.session_id).bind(run).execute(&mut **tx).await?;
    "running".clone_into(&mut current.status);
    current.active_run_id = Some(run);
    current.active_run_heartbeat_at = Some(Utc::now());
    current.composer_suggestions = json!([]);
    current.updated_at = Utc::now();
    Ok(run)
}

struct SubmittedTurn {
    session: Option<Uuid>,
    workspace: Option<Uuid>,
    request: String,
    timezone: String,
    locale: Option<String>,
    content: Vec<Value>,
}

fn parse_turn(body: &Value) -> Result<SubmittedTurn, ApiError> {
    for field in [
        "messages",
        "model",
        "selectedModel",
        "selectedModelId",
        "devicePlatform",
        "chatSessionId",
        "codeInterpreterContainerId",
        "userContext",
        "totalCards",
        "codeInterpreterContainer",
        "vendor",
        "thinking",
        "thinkingLevel",
    ] {
        if body.get(field).is_some() {
            return Err(ApiError::bad_request(format!(
                "Unsupported request field: {field}"
            )));
        }
    }
    let request = text(body, "clientRequestId")?;
    let _: axum::http::HeaderValue = request
        .parse()
        .map_err(|_| ApiError::bad_request("clientRequestId is invalid"))?;
    let timezone = text(body, "timezone")?;
    if timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(ApiError::bad_request("timezone is invalid"));
    }
    let locale = body
        .get("uiLocale")
        .map(|_| text(body, "uiLocale"))
        .transpose()?;
    let _ = suggestions(locale)?;
    Ok(SubmittedTurn {
        session: uuid_field(body, "sessionId")?,
        workspace: uuid_field(body, "workspaceId")?,
        request: request.to_owned(),
        timezone: timezone.to_owned(),
        locale: locale.map(str::to_owned),
        content: content(body)?,
    })
}

pub(super) async fn start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let turn = parse_turn(&body)?;
    let reference = connection::reference(&state.config, identity.user_id).await?;
    let api_key = api_key(&headers, reference.is_some())?;
    let workspace =
        core::resolve_workspace(&state, &identity.user_id.to_string(), turn.workspace).await?;
    let requested = turn.session;
    let request = turn.request.as_str();
    let timezone = turn.timezone.as_str();
    let locale = turn.locale.as_deref();
    let content = turn.content;
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    let mut current = session(
        &mut tx,
        identity.user_id,
        workspace,
        requested,
        true,
        locale,
    )
    .await?;
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT run_id,status FROM ai.chat_runs WHERE session_id=$1 AND request_id=$2",
    )
    .bind(current.session_id)
    .bind(request)
    .fetch_optional(&mut *tx)
    .await?;
    let (run, launch, deduplicated) = if let Some((run, status)) = existing {
        (run, status == "queued", true)
    } else {
        recover(&mut tx, &mut current).await?;
        if current.status == "running" {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "CHAT_SESSION_CONFLICT",
                "This chat already has a running response.",
            ));
        }
        worker::assert_provider_available(reference.as_ref(), api_key.as_deref())?;
        if reference.is_none() && api_key.is_none() {
            super::tools::assert_allowance(&state, identity.user_id).await?;
        }
        let run = insert_run(
            &mut tx,
            &mut current,
            &RunInput {
                user: identity.user_id,
                workspace,
                request,
                timezone,
                locale,
                content: &content,
            },
        )
        .await?;
        (run, true, false)
    };
    let all = items(&mut tx, current.session_id).await?;
    let mut result = envelope(&state, identity.user_id, workspace, &current, all, None)?;
    tx.commit().await?;
    let map = result.as_object_mut().ok_or_else(ApiError::internal)?;
    map.insert("accepted".to_owned(), json!(true));
    if deduplicated {
        map.insert("deduplicated".to_owned(), json!(true));
    }
    if launch {
        let state = state.clone();
        let job = Job {
            user: identity.user_id,
            workspace,
            session: current.session_id,
            run,
            reference,
            api_key,
        };
        tokio::spawn(async move {
            worker::run(state, job).await;
        });
    }
    let mut response = private_json(result);
    response.headers_mut().insert(
        "x-chat-request-id",
        request
            .parse()
            .map_err(|_| ApiError::bad_request("clientRequestId is invalid"))?,
    );
    Ok(response)
}

async fn cancel(
    tx: &mut Transaction<'_, Postgres>,
    session: Uuid,
    run: Uuid,
) -> Result<bool, ApiError> {
    let assistant: Option<Uuid> = sqlx::query_scalar("UPDATE ai.chat_runs SET cancel_requested_at=now(),status='cancelled',finished_at=now(),updated_at=now() WHERE run_id=$1 AND session_id=$2 AND status IN ('queued','running') RETURNING assistant_item_id").bind(run).bind(session).fetch_optional(&mut **tx).await?;
    let Some(assistant) = assistant else {
        return Ok(false);
    };
    terminal_item(
        tx,
        assistant,
        "cancelled",
        "cancelled",
        Some("Stopped by you."),
    )
    .await?;
    sqlx::query("UPDATE ai.chat_sessions SET status='idle',active_run_id=NULL,active_run_heartbeat_at=NULL,updated_at=now() WHERE session_id=$1 AND active_run_id=$2").bind(session).bind(run).execute(&mut **tx).await?;
    Ok(true)
}

pub(super) async fn stop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let workspace = core::resolve_workspace(
        &state,
        &identity.user_id.to_string(),
        uuid_field(&body, "workspaceId")?,
    )
    .await?;
    let requested = uuid_field(&body, "sessionId")?
        .ok_or_else(|| ApiError::bad_request("sessionId is required"))?;
    let requested_run = uuid_field(&body, "runId")?;
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    let current = session(
        &mut tx,
        identity.user_id,
        workspace,
        Some(requested),
        false,
        None,
    )
    .await?;
    let run = requested_run.or(current.active_run_id);
    let stopped = if let Some(run) = run.filter(|run| Some(*run) == current.active_run_id) {
        cancel(&mut tx, current.session_id, run).await?
    } else {
        false
    };
    tx.commit().await?;
    Ok(private_json(
        json!({"sessionId":current.session_id,"conversationScopeId":current.session_id,"runId":run,"stopped":stopped,"stillRunning":current.status=="running"&&!stopped}),
    ))
}
