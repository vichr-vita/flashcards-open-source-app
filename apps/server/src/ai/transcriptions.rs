use super::{chat, connection, private_json, tools};
use crate::{AppState, auth, core, database::scoped, error::ApiError};
use axum::{
    extract::{Multipart, State, multipart::MultipartRejection},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use reqwest::multipart::{Form, Part};
use serde_json::{Value, json};
use uuid::Uuid;

struct Upload {
    bytes: Vec<u8>,
    name: String,
    media_type: String,
    session: Option<Uuid>,
    workspace: Option<Uuid>,
}

fn bad(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

async fn upload(mut multipart: Multipart) -> Result<Upload, ApiError> {
    let mut file = None;
    let mut source = None;
    let mut fields = json!({});
    while let Some(field) = multipart.next_field().await.map_err(|_| {
        bad(
            "CHAT_TRANSCRIPTION_INVALID_MULTIPART",
            "Invalid multipart form data",
        )
    })? {
        let name = field.name().unwrap_or_default().to_owned();
        if name == "file" && file.is_none() {
            let file_name = field
                .file_name()
                .map(str::to_owned)
                .ok_or_else(|| bad("CHAT_TRANSCRIPTION_FILE_REQUIRED", "file is required"))?;
            let media = field
                .content_type()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let bytes = field
                .bytes()
                .await
                .map_err(|_| {
                    bad(
                        "CHAT_TRANSCRIPTION_INVALID_MULTIPART",
                        "Invalid multipart form data",
                    )
                })?
                .to_vec();
            file = Some((file_name, media, bytes));
        } else if matches!(name.as_str(), "source" | "workspaceId" | "sessionId")
            && fields.get(&name).is_none()
        {
            let value = field.text().await.map_err(|_| {
                bad(
                    "CHAT_TRANSCRIPTION_INVALID_MULTIPART",
                    "Invalid multipart form data",
                )
            })?;
            if name == "source" {
                source = Some(value.clone());
            }
            if let Some(map) = fields.as_object_mut() {
                map.insert(name, json!(value.trim()));
            }
        }
    }
    let (name, media_type, bytes) =
        file.ok_or_else(|| bad("CHAT_TRANSCRIPTION_FILE_REQUIRED", "file is required"))?;
    if bytes.is_empty() {
        return Err(bad(
            "CHAT_TRANSCRIPTION_FILE_EMPTY",
            "file must not be empty",
        ));
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    if !matches!(
        media_type.as_str(),
        "audio/mp4"
            | "audio/m4a"
            | "audio/x-m4a"
            | "audio/wav"
            | "audio/wave"
            | "audio/x-wav"
            | "audio/webm"
    ) && !matches!(extension.as_deref(), Some("m4a" | "wav" | "webm"))
    {
        return Err(bad(
            "CHAT_TRANSCRIPTION_FILE_UNSUPPORTED",
            "Unsupported audio file type. Use m4a, wav, or webm.",
        ));
    }
    if !matches!(source.as_deref(), Some("android" | "ios" | "web")) {
        return Err(bad(
            "CHAT_TRANSCRIPTION_SOURCE_INVALID",
            "source must be either android, ios, or web",
        ));
    }
    if fields.get("sessionId").and_then(Value::as_str) == Some("")
        && let Some(map) = fields.as_object_mut()
    {
        map.remove("sessionId");
    }
    Ok(Upload {
        bytes,
        name,
        media_type,
        session: chat::uuid_field(&fields, "sessionId")?,
        workspace: chat::uuid_field(&fields, "workspaceId")?,
    })
}

fn provider_error(status: StatusCode, value: &Value, own_key: bool) -> ApiError {
    let error = value.get("error").unwrap_or(value);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if own_key && !message.is_empty() && status.is_client_error() {
        let code = error
            .get("code")
            .or_else(|| error.get("type"))
            .and_then(Value::as_str);
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "OWN_OPENAI_KEY_PROVIDER_ERROR",
            code.map_or_else(|| message.to_owned(), |code| format!("{code}: {message}")),
        );
    }
    let lower = message.to_ascii_lowercase();
    if matches!(status.as_u16(), 400 | 415 | 422 | 500)
        && [
            "corrupted",
            "unsupported",
            "processing failed",
            "unprocessable",
        ]
        .iter()
        .any(|word| lower.contains(word))
    {
        return bad(
            "CHAT_TRANSCRIPTION_INVALID_AUDIO",
            "We couldn’t process that recording. Please try again.",
        );
    }
    let (status, code) = match status.as_u16() {
        401 | 403 => (
            StatusCode::SERVICE_UNAVAILABLE,
            "CHAT_TRANSCRIPTION_PROVIDER_AUTH_FAILED",
        ),
        402 | 429 => (
            StatusCode::TOO_MANY_REQUESTS,
            "CHAT_TRANSCRIPTION_RATE_LIMITED",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "CHAT_TRANSCRIPTION_UNAVAILABLE",
        ),
    };
    ApiError::new(
        status,
        code,
        "AI audio transcription is temporarily unavailable on this server. Try again later.",
    )
}

async fn usage(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    headers: &HeaderMap,
    own_key: bool,
    result: &Value,
) -> Result<(), ApiError> {
    let tier = tools::tier(state, user).await?;
    let counters = result.get("usage").unwrap_or(&Value::Null);
    let duration = counters.get("type").and_then(Value::as_str) == Some("duration");
    let mut tx = scoped(&state.pool, &user.to_string(), Some(&workspace.to_string())).await?;
    sqlx::query("INSERT INTO ai.usage_events(usage_event_id,user_id,workspace_id,occurred_at,surface,provider,model_id,request_id,tier_at_call,input_tokens,output_tokens,audio_seconds,user_supplied_key) VALUES($1,$2,$3,now(),'dictation','openai','gpt-4o-transcribe',$4,$5,$6,$7,$8,$9)")
        .bind(Uuid::new_v4()).bind(user.to_string()).bind(workspace).bind(headers.get("x-request-id").and_then(|value|value.to_str().ok())).bind(tier).bind(if duration{None}else{counters.get("input_tokens").and_then(Value::as_i64)}).bind(if duration{None}else{counters.get("output_tokens").and_then(Value::as_i64)}).bind(if duration{counters.get("seconds").and_then(Value::as_f64)}else{None}).bind(own_key).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(super) async fn transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let own_key = chat::api_key(&headers, false)?;
    let upload = upload(multipart.map_err(|_| {
        bad(
            "CHAT_TRANSCRIPTION_INVALID_MULTIPART",
            "Invalid multipart form data",
        )
    })?)
    .await?;
    let workspace =
        core::resolve_workspace(&state, &identity.user_id.to_string(), upload.workspace).await?;
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    let mut session = chat::session(
        &mut tx,
        identity.user_id,
        workspace,
        upload.session,
        true,
        None,
    )
    .await?;
    chat::recover(&mut tx, &mut session).await?;
    tx.commit().await?;
    let key = own_key
        .clone()
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHAT_TRANSCRIPTION_NOT_CONFIGURED",
                "AI audio transcription is not configured on this server.",
            )
        })?;
    let result = send(&state, upload, key, own_key.is_some()).await?;
    usage(
        &state,
        identity.user_id,
        workspace,
        &headers,
        own_key.is_some(),
        &result,
    )
    .await?;
    let text = result
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHAT_TRANSCRIPTION_UNAVAILABLE",
                "AI audio transcription is temporarily unavailable on this server. Try again later.",
            )
        })?;
    Ok(private_json(
        json!({"text":text,"sessionId":session.session_id}),
    ))
}

async fn send(
    state: &AppState,
    upload: Upload,
    key: String,
    own_key: bool,
) -> Result<Value, ApiError> {
    let mut part = Part::bytes(upload.bytes).file_name(upload.name);
    if !upload.media_type.is_empty() {
        part = part.mime_str(&upload.media_type).map_err(|_| {
            bad(
                "CHAT_TRANSCRIPTION_FILE_UNSUPPORTED",
                "Unsupported audio file type. Use m4a, wav, or webm.",
            )
        })?;
    }
    let response = connection::client()?
        .post(connection::api_url(&state.config,"/audio/transcriptions"))
        .bearer_auth(key)
        .multipart(
            Form::new()
                .part("file", part)
                .text("model", "gpt-4o-transcribe"),
        )
        .send()
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHAT_TRANSCRIPTION_UNAVAILABLE",
                "AI audio transcription is temporarily unavailable on this server. Try again later.",
            )
        })?;
    let status = response.status();
    let result = response.json::<Value>().await.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "CHAT_TRANSCRIPTION_FAILED",
            "AI audio transcription is temporarily unavailable on this server. Try again later.",
        )
    })?;
    if !status.is_success() {
        return Err(provider_error(status, &result, own_key));
    }
    Ok(result)
}
