use super::{chat::Job, connection};
use crate::{AppState, database::scoped, error::ApiError};
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

fn plain(parts: &[Value]) -> String {
    parts
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("text") => part.get("text").and_then(Value::as_str),
            Some("file") => part.get("fileName").and_then(Value::as_str),
            Some("tool_call") => part.get("output").and_then(Value::as_str),
            _ => None,
        })
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn record(state: &AppState, job: &Job, response: &Value) -> Result<(), ApiError> {
    let tier = super::tools::tier(state, job.user).await?;
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    let request: String = sqlx::query_scalar("SELECT request_id FROM ai.chat_runs WHERE run_id=$1")
        .bind(job.run)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO ai.usage_events(usage_event_id,user_id,workspace_id,occurred_at,surface,provider,model_id,request_id,tier_at_call,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,user_supplied_key) VALUES($1,$2,$3,now(),'composer_suggestion','openai','gpt-6-sol',$4,$5,$6,$7,$8,$9,$10,false)")
        .bind(Uuid::new_v4()).bind(job.user.to_string()).bind(job.workspace).bind(request).bind(tier).bind(usage.get("input_tokens").and_then(Value::as_i64)).bind(usage.get("output_tokens").and_then(Value::as_i64)).bind(usage.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_i64)).bind(usage.pointer("/input_tokens_details/cache_write_tokens").and_then(Value::as_i64)).bind(usage.pointer("/output_tokens_details/reasoning_tokens").and_then(Value::as_i64)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

fn parse(response: &Value, assistant: Uuid) -> Result<Value, ApiError> {
    let text = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<String>();
    let text = text.trim();
    let object = serde_json::from_str::<Value>(text)
        .or_else(|_| {
            let object = text
                .find('{')
                .and_then(|start| text.rfind('}').and_then(|end| text.get(start..=end)))
                .unwrap_or_default();
            serde_json::from_str(object)
        })
        .map_err(|_| ApiError::internal())?;
    let rows = object
        .get("suggestions")
        .and_then(Value::as_array)
        .ok_or_else(ApiError::internal)?;
    let mut suggestions = Vec::new();
    let mut seen = Vec::<String>::new();
    for text in rows.iter().filter_map(Value::as_str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || text.encode_utf16().count() > 80 || seen.contains(&text) {
            continue;
        }
        seen.push(text.clone());
        suggestions.push(json!({"id":format!("{assistant}-{}",suggestions.len().saturating_add(1)),"text":text,"source":"assistant_follow_up","assistantItemId":assistant}));
        if suggestions.len() == 2 {
            break;
        }
    }
    Ok(json!(suggestions))
}

/// Follow-ups use the platform key only on platform-funded turns, matching the deployed billing rule.
pub(super) async fn generate(
    state: &AppState,
    job: &Job,
    assistant: Uuid,
    content: &[Value],
) -> Result<(), ApiError> {
    if job.reference.is_some() || job.api_key.is_some() {
        return Ok(());
    }
    let Some(key) = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
    else {
        return Ok(());
    };
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    let (turn, locale): (Value, Option<String>) =
        sqlx::query_as("SELECT turn_input,ui_locale FROM ai.chat_runs WHERE run_id=$1")
            .bind(job.run)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    let user = plain(turn.as_array().map_or(&[][..], Vec::as_slice));
    let answer = plain(content);
    if user.is_empty() || answer.is_empty() {
        return Ok(());
    }
    let prompt = format!(
        "Generate exactly two short follow-up messages that the user may send next.\nReturn strict JSON only in this shape: {{\"suggestions\":[\"...\",\"...\"]}}.\nEach suggestion must be plain text, concise, and suitable for a mobile composer.\nEach suggestion must be under 60 characters.\nWrite both suggestions in this UI locale: {}.\nDo not copy the assistant reply verbatim.\nDo not add markdown, numbering, or explanations.\n\nLatest user message:\n{user}\n\nAssistant reply:\n{answer}",
        locale.as_deref().unwrap_or("en")
    );
    let body = json!({"model":"gpt-6-sol","reasoning":{"effort":"none"},"store":false,"safety_identifier":format!("v1_{}",URL_SAFE_NO_PAD.encode(Sha256::digest(job.user.to_string().as_bytes()))),"input":[{"type":"message","role":"system","content":[{"type":"input_text","text":"You write short user follow-up suggestions for a mobile AI chat composer."}]},{"type":"message","role":"user","content":[{"type":"input_text","text":prompt}]}]});
    let response = connection::client()?
        .post(connection::api_url(&state.config, "/responses"))
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "AI_SUGGESTIONS_FAILED",
                "Follow-up suggestions are unavailable.",
            )
        })?
        .error_for_status()
        .map_err(|_| ApiError::internal())?
        .json::<Value>()
        .await
        .map_err(|_| ApiError::internal())?;
    record(state, job, &response).await?;
    let suggestions = parse(&response, assistant)?;
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ai.chat_runs r JOIN ai.chat_sessions s USING(session_id) WHERE r.run_id=$1 AND r.status='running' AND r.cancel_requested_at IS NULL AND s.active_run_id=r.run_id)").bind(job.run).fetch_one(&mut *tx).await?;
    if active {
        let generation:Uuid=sqlx::query_scalar("INSERT INTO ai.chat_composer_suggestion_generations(session_id,source,assistant_item_id,suggestions) VALUES($1,'assistant_follow_up',$2,$3) RETURNING generation_id").bind(job.session).bind(assistant).bind(&suggestions).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE ai.chat_sessions SET composer_suggestions=$2,active_composer_suggestion_generation_id=$3 WHERE session_id=$1 AND active_run_id=$4").bind(job.session).bind(suggestions).bind(generation).bind(job.run).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
