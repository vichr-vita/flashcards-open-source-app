use super::{
    chat::{self, Job},
    connection, output, tools,
};
use crate::{AppState, database::scoped, error::ApiError};
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use reqwest::Response;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{FromRow, Postgres, Transaction};
use std::{env, time::Duration};
use uuid::Uuid;

#[derive(FromRow)]
struct Claim {
    assistant_item_id: Uuid,
    worker_claimed_at: DateTime<Utc>,
    timezone: String,
    model_id: String,
    reasoning_effort: String,
    request_id: String,
}

#[derive(Default)]
struct Progress {
    content: Vec<Value>,
    replay: Vec<Value>,
    response_index: u64,
}

pub(super) fn assert_provider_available(
    reference: Option<&connection::Reference>,
    key: Option<&str>,
) -> Result<(), ApiError> {
    if reference.is_none()
        && key.is_none()
        && !env::var("OPENAI_API_KEY").is_ok_and(|key| !key.trim().is_empty())
    {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "AI_NOT_CONFIGURED",
            "Connect ChatGPT in AI settings or add an OpenAI API key.",
        ));
    }
    Ok(())
}

async fn lock_session(tx: &mut Transaction<'_, Postgres>, session: Uuid) -> Result<(), ApiError> {
    sqlx::query("SELECT session_id FROM ai.chat_sessions WHERE session_id=$1 FOR UPDATE")
        .bind(session)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn claim(state: &AppState, job: &Job) -> Result<Option<Claim>, ApiError> {
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    lock_session(&mut tx, job.session).await?;
    let claimed=sqlx::query_as("UPDATE ai.chat_runs r SET status='running',worker_claimed_at=clock_timestamp(),worker_heartbeat_at=clock_timestamp(),started_at=coalesce(started_at,now()),updated_at=now() WHERE run_id=$1 AND status='queued' AND cancel_requested_at IS NULL AND EXISTS(SELECT 1 FROM ai.chat_sessions s WHERE s.session_id=r.session_id AND s.active_run_id=r.run_id) RETURNING assistant_item_id,worker_claimed_at,timezone,model_id,reasoning_effort,request_id").bind(job.run).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    Ok(claimed)
}

async fn heartbeat(state: &AppState, job: &Job, claim: &Claim) -> Result<bool, ApiError> {
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    lock_session(&mut tx, job.session).await?;
    let alive=sqlx::query("UPDATE ai.chat_runs r SET worker_heartbeat_at=clock_timestamp(),updated_at=now() WHERE run_id=$1 AND worker_claimed_at=$2 AND status='running' AND cancel_requested_at IS NULL AND EXISTS(SELECT 1 FROM ai.chat_sessions s WHERE s.session_id=r.session_id AND s.active_run_id=r.run_id)").bind(job.run).bind(claim.worker_claimed_at).execute(&mut *tx).await?.rows_affected()==1;
    if alive {
        sqlx::query("UPDATE ai.chat_sessions SET active_run_heartbeat_at=clock_timestamp() WHERE session_id=$1 AND active_run_id=$2").bind(job.session).bind(job.run).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(alive)
}

async fn persist(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    progress: &Progress,
) -> Result<(), ApiError> {
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    let updated=sqlx::query("UPDATE ai.chat_items i SET payload=i.payload || $3::jsonb,updated_at=now() WHERE item_id=$1 AND state='in_progress' AND EXISTS(SELECT 1 FROM ai.chat_runs r JOIN ai.chat_sessions s ON s.session_id=r.session_id WHERE r.run_id=$2 AND r.assistant_item_id=i.item_id AND r.status='running' AND r.cancel_requested_at IS NULL AND r.worker_claimed_at=$4 AND s.active_run_id=r.run_id)")
        .bind(claim.assistant_item_id).bind(job.run).bind(json!({"role":"assistant","content":progress.content,"openaiItems":progress.replay})).bind(claim.worker_claimed_at).execute(&mut *tx).await?;
    tx.commit().await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "CHAT_RUN_INTERRUPTED",
            "This response is no longer active.",
        ));
    }
    Ok(())
}

async fn finish(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    result: Result<(), ApiError>,
) -> Result<(), ApiError> {
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    lock_session(&mut tx, job.session).await?;
    let error = result.err();
    let updated=sqlx::query("UPDATE ai.chat_runs SET status=$3,last_error_message=$4,finished_at=now(),worker_heartbeat_at=now(),updated_at=now() WHERE run_id=$1 AND worker_claimed_at=$2 AND status='running' AND cancel_requested_at IS NULL")
        .bind(job.run).bind(claim.worker_claimed_at).bind(if error.is_some(){"failed"}else{"completed"}).bind(error.as_ref().map(|error|error.message.as_str())).execute(&mut *tx).await?;
    if updated.rows_affected() == 1 {
        chat::terminal_item(
            &mut tx,
            claim.assistant_item_id,
            if error.is_some() {
                "error"
            } else {
                "completed"
            },
            if error.is_some() {
                "failed"
            } else {
                "completed"
            },
            error.as_ref().map(|error| error.message.as_str()),
        )
        .await?;
        sqlx::query("UPDATE ai.chat_sessions SET status='idle',active_run_id=NULL,active_run_heartbeat_at=NULL,updated_at=now() WHERE session_id=$1 AND active_run_id=$2").bind(job.session).bind(job.run).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

fn replay_item(item: &Value, user_key: bool) -> Option<Value> {
    let mut item = item.clone();
    let map = item.as_object_mut()?;
    map.remove("id");
    match map.get("type").and_then(Value::as_str)? {
        "reasoning" => {
            if map
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return None;
            }
            map.insert("user_supplied_key".to_owned(), json!(user_key));
        }
        "function_call" | "function_call_output" | "message" => {}
        _ => return None,
    }
    Some(item)
}

fn input_item(item: &Value, user_key: bool) -> Option<Value> {
    if item.get("type").and_then(Value::as_str) == Some("reasoning")
        && item
            .get("user_supplied_key")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            != user_key
    {
        return None;
    }
    let mut item = replay_item(item, user_key)?;
    item.as_object_mut()?.remove("user_supplied_key");
    Some(item)
}

fn input_parts(content: &[Value], assistant: bool) -> Vec<Value> {
    content.iter().filter_map(|part|match part.get("type").and_then(Value::as_str) {
        Some("text")=>Some(json!({"type":if assistant{"output_text"}else{"input_text"},"text":part.get("text")})),
        Some("image")=>Some(json!({"type":"input_image","detail":"auto","image_url":format!("data:{};base64,{}",part.get("mediaType").and_then(Value::as_str).unwrap_or_default(),part.get("base64Data").and_then(Value::as_str).unwrap_or_default())})),
        Some("file")=>Some(json!({"type":"input_file","filename":part.get("fileName"),"file_data":format!("data:{};base64,{}",part.get("mediaType").and_then(Value::as_str).unwrap_or_default(),part.get("base64Data").and_then(Value::as_str).unwrap_or_default())})),
        Some("card"|"tool_call"|"reasoning_summary")=>Some(json!({"type":if assistant{"output_text"}else{"input_text"},"text":part.to_string()})),
        _=>None,
    }).collect()
}

async fn history(state: &AppState, job: &Job) -> Result<Vec<Value>, ApiError> {
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    let items = chat::items(&mut tx, job.session).await?;
    tx.commit().await?;
    let user_key = job.reference.is_some() || job.api_key.is_some();
    let mut kept = Vec::new();
    let mut size = 0_usize;
    for item in items
        .iter()
        .rev()
        .filter(|item| item.state != "in_progress")
    {
        let assistant = item.payload.get("role").and_then(Value::as_str) == Some("assistant");
        let mapped = if assistant {
            item.payload
                .get("openaiItems")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| input_item(item, user_key))
                        .collect::<Vec<_>>()
                })
        } else {
            None
        };
        let mapped=mapped.unwrap_or_else(||vec![json!({"type":"message","role":if assistant{"assistant"}else{"user"},"content":input_parts(item.payload.get("content").and_then(Value::as_array).map_or(&[][..],Vec::as_slice),assistant)})]);
        let length = mapped
            .iter()
            .map(|item| item.to_string().len())
            .fold(0_usize, usize::saturating_add);
        if !kept.is_empty() && size.saturating_add(length) > 220_000 {
            break;
        }
        size = size.saturating_add(length);
        kept.push(mapped);
    }
    kept.reverse();
    Ok(kept.into_iter().flatten().collect())
}

fn request_body(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    input: &[Value],
    summary: bool,
) -> Value {
    let now = Utc::now();
    let local = claim.timezone.parse::<chrono_tz::Tz>().map_or_else(
        |_| now.to_rfc3339(),
        |timezone| now.with_timezone(&timezone).to_rfc3339(),
    );
    let instructions = format!(
        "{}\n\nCard web URL: {}/cards/<cardId>\n\nCurrent datetime - UTC: {} | User local ({}): {}",
        include_str!("system-instructions.txt").trim(),
        state.config.backend_origin,
        now.to_rfc3339(),
        claim.timezone,
        local
    );
    let model = job
        .reference
        .as_ref()
        .map_or(claim.model_id.as_str(), |reference| {
            reference.model_id.as_str()
        });
    let effort = job
        .reference
        .as_ref()
        .map_or(Some(claim.reasoning_effort.as_str()), |reference| {
            reference.reasoning_effort.as_deref()
        });
    let mut body = json!({"model":model,"instructions":instructions,"input":input,"reasoning":{"summary":"auto"},"include":["reasoning.encrypted_content"],"tools":if summary{json!([])}else{tools::definitions()},"store":false,"stream":true});
    if let Some(effort) = effort
        && let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut)
    {
        reasoning.insert("effort".to_owned(), json!(effort));
    }
    if job.reference.is_none()
        && let Some(map) = body.as_object_mut()
    {
        map.insert("max_output_tokens".to_owned(), json!(32_000));
        map.insert(
            "safety_identifier".to_owned(),
            json!(format!(
                "v1_{}",
                URL_SAFE_NO_PAD.encode(Sha256::digest(job.user.to_string().as_bytes()))
            )),
        );
    }
    body
}

async fn send(state: &AppState, job: &Job, body: &Value) -> Result<Response, ApiError> {
    if let Some(reference) = &job.reference {
        for force in [false, true] {
            let headers =
                connection::credentials(&state.config, job.user, reference, force).await?;
            let response = connection::client()?
                .post(connection::codex_url(&state.config, "/responses"))
                .headers(headers)
                .header("accept", "text/event-stream")
                .json(body)
                .send()
                .await
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "CHATGPT_UNAVAILABLE",
                        "Cannot reach ChatGPT. Try again.",
                    )
                })?;
            if response.status() == StatusCode::UNAUTHORIZED && !force {
                continue;
            }
            if response.status().is_success() {
                return Ok(response);
            }
            let message = match response.status() {
                StatusCode::TOO_MANY_REQUESTS => {
                    "Your ChatGPT usage limit was reached. Try later or select API in AI settings."
                }
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                    "ChatGPT needs authorization. Connect again in AI settings."
                }
                _ => "ChatGPT could not complete this request. Try again or check AI settings.",
            };
            return Err(ApiError::new(
                response.status(),
                "CHATGPT_REQUEST_FAILED",
                message,
            ));
        }
        return Err(ApiError::internal());
    }
    let key = job
        .api_key
        .clone()
        .or_else(|| env::var("OPENAI_API_KEY").ok())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "AI_NOT_CONFIGURED",
                "Add an OpenAI API key or connect ChatGPT in AI settings.",
            )
        })?;
    let response = connection::client()?
        .post(connection::api_url(&state.config, "/responses"))
        .bearer_auth(key)
        .header("accept", "text/event-stream")
        .json(body)
        .send()
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "OPENAI_UNAVAILABLE",
                "Cannot reach OpenAI. Try again.",
            )
        })?;
    if !response.status().is_success() {
        return Err(ApiError::new(
            response.status(),
            "OPENAI_REQUEST_FAILED",
            "OpenAI could not complete this request. Check the API key and usage limit in AI settings.",
        ));
    }
    Ok(response)
}

fn position(part: &Value, id: &str) -> bool {
    part.get("streamPosition")
        .and_then(|position| position.get("itemId"))
        .and_then(Value::as_str)
        == Some(id)
}

fn append_text(progress: &mut Progress, event: &Value, reasoning: bool) {
    let id = event
        .get("item_id")
        .and_then(Value::as_str)
        .unwrap_or("assistant-text");
    let delta = event
        .get("delta")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let kind = if reasoning {
        "reasoning_summary"
    } else {
        "text"
    };
    let field = if reasoning { "summary" } else { "text" };
    if let Some(part) = progress
        .content
        .iter_mut()
        .find(|part| part.get("type").and_then(Value::as_str) == Some(kind) && position(part, id))
    {
        let previous = part.get(field).and_then(Value::as_str).unwrap_or_default();
        let text = format!("{previous}{delta}");
        if let Some(map) = part.as_object_mut() {
            map.insert(field.to_owned(), json!(text));
        }
    } else {
        let mut part = json!({"type":kind,"streamPosition":{"itemId":id,"responseIndex":progress.response_index,"outputIndex":event.get("output_index").and_then(Value::as_u64).unwrap_or_default(),"contentIndex":event.get("content_index").and_then(Value::as_u64),"sequenceNumber":event.get("sequence_number").and_then(Value::as_u64)}});
        if let Some(map) = part.as_object_mut() {
            map.insert(field.to_owned(), json!(delta));
        }
        progress.content.push(part);
    }
}

fn tool_part(progress: &mut Progress, item: &Value, index: u64) {
    let id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if progress.content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("tool_call")
            && part.get("id").and_then(Value::as_str) == Some(id)
    }) {
        return;
    }
    progress.content.push(json!({"type":"tool_call","id":id,"name":item.get("name"),"status":"started","input":null,"output":null,"streamPosition":{"itemId":item.get("id"),"responseIndex":progress.response_index,"outputIndex":index,"contentIndex":null,"sequenceNumber":null}}));
}

fn reasoning_part(progress: &mut Progress, item: &Value, index: u64) {
    let id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    if !progress.content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("reasoning_summary") && position(part, id)
    }) {
        progress.content.push(json!({"type":"reasoning_summary","summary":"","streamPosition":{"itemId":id,"responseIndex":progress.response_index,"outputIndex":index,"contentIndex":null,"sequenceNumber":null}}));
    }
}

async fn event(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    progress: &mut Progress,
    value: &Value,
) -> Result<Option<Value>, ApiError> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            append_text(progress, value, false);
            persist(state, job, claim, progress).await?;
        }
        Some("response.reasoning_summary_text.delta") => {
            append_text(progress, value, true);
            persist(state, job, claim, progress).await?;
        }
        Some("response.output_item.added" | "response.output_item.done") => {
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("reasoning")
            {
                reasoning_part(
                    progress,
                    item,
                    value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                );
                persist(state, job, claim, progress).await?;
            }
            if let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("function_call")
            {
                tool_part(
                    progress,
                    item,
                    value
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                );
                persist(state, job, claim, progress).await?;
            }
            if value.get("type").and_then(Value::as_str) == Some("response.output_item.done")
                && let Some(item) = value.get("item")
                && item.get("type").and_then(Value::as_str) == Some("reasoning")
            {
                if let Some(part) = progress.content.iter_mut().find(|part| {
                    part.get("type").and_then(Value::as_str) == Some("reasoning_summary")
                        && position(
                            part,
                            item.get("id").and_then(Value::as_str).unwrap_or_default(),
                        )
                }) && let Some(map) = part.as_object_mut()
                {
                    map.insert("providerStatus".to_owned(), json!("completed"));
                }
                persist(state, job, claim, progress).await?;
            }
        }
        Some("response.completed" | "response.incomplete") => {
            return value.get("response").cloned().map(Some).ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "AI_RESPONSE_INVALID",
                    "The AI provider returned an invalid response.",
                )
            });
        }
        Some("response.failed" | "error") => {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "AI_RESPONSE_FAILED",
                "The AI provider could not complete this response. Try again.",
            ));
        }
        _ => {}
    }
    Ok(None)
}

async fn stream(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    progress: &mut Progress,
    mut response: Response,
) -> Result<Value, ApiError> {
    let mut pending = Vec::<u8>::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "AI_STREAM_INTERRUPTED",
            "The AI response was interrupted. Try again.",
        )
    })? {
        pending.extend_from_slice(&chunk);
        if pending.len() > 4_194_304 {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "AI_RESPONSE_TOO_LARGE",
                "The AI provider returned an oversized response.",
            ));
        }
        loop {
            let delimiter = pending
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| (position, 2))
                .or_else(|| {
                    pending
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|position| (position, 4))
                });
            let Some((end, length)) = delimiter else {
                break;
            };
            let packet = String::from_utf8(pending.drain(..end.saturating_add(length)).collect())
                .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "AI_RESPONSE_INVALID",
                    "The AI provider returned invalid text.",
                )
            })?;
            let data = packet
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let value: Value = serde_json::from_str(&data).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "AI_RESPONSE_INVALID",
                    "The AI provider returned an invalid response.",
                )
            })?;
            if let Some(completed) = event(state, job, claim, progress, &value).await? {
                return Ok(completed);
            }
        }
    }
    Err(ApiError::new(
        StatusCode::BAD_GATEWAY,
        "AI_STREAM_INTERRUPTED",
        "The AI response ended before it completed. Try again.",
    ))
}

async fn usage(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    response: &Value,
    model: &str,
) -> Result<(), ApiError> {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let tier = tools::tier(state, job.user).await?;
    let mut tx = scoped(
        &state.pool,
        &job.user.to_string(),
        Some(&job.workspace.to_string()),
    )
    .await?;
    sqlx::query("INSERT INTO ai.usage_events(usage_event_id,user_id,workspace_id,occurred_at,surface,provider,model_id,request_id,tier_at_call,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,user_supplied_key) VALUES($1,$2,$3,now(),'chat','openai',$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(Uuid::new_v4()).bind(job.user.to_string()).bind(job.workspace).bind(model).bind(&claim.request_id).bind(tier).bind(usage.get("input_tokens").and_then(Value::as_i64)).bind(usage.get("output_tokens").and_then(Value::as_i64)).bind(usage.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_i64)).bind(usage.pointer("/input_tokens_details/cache_write_tokens").and_then(Value::as_i64)).bind(usage.pointer("/output_tokens_details/reasoning_tokens").and_then(Value::as_i64)).bind(job.reference.is_some()||job.api_key.is_some()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn execute_tool(
    state: &AppState,
    job: &Job,
    claim: &Claim,
    progress: &mut Progress,
    item: &Value,
    index: u64,
) -> Result<Value, ApiError> {
    if !heartbeat(state, job, claim).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "CHAT_RUN_INTERRUPTED",
            "This response is no longer active.",
        ));
    }
    tool_part(progress, item, index);
    persist(state, job, claim, progress).await?;
    let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
    let args = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let result = match serde_json::from_str::<Value>(args) {
        Ok(args) => {
            tools::execute(
                state,
                job.user,
                Some(job.workspace),
                name,
                &args,
                &claim.timezone,
            )
            .await
        }
        Err(_) => Err(ApiError::bad_request(
            "Tool arguments must be exactly one JSON object.",
        )),
    };
    let succeeded = result.is_ok();
    let result = match result {
        Ok(mut value) => {
            let map = value.as_object_mut().ok_or_else(ApiError::internal)?;
            map.insert("ok".to_owned(), json!(true));
            map.insert("tool".to_owned(), json!(name));
            if matches!(name, "sql_query" | "sql_execute")
                && let Ok(args) = serde_json::from_str::<Value>(args)
            {
                map.insert(
                    "sql".to_owned(),
                    args.get("sql").cloned().unwrap_or(Value::Null),
                );
            }
            value
        }
        Err(error) => {
            json!({"ok":false,"tool":name,"error":{"name":"HttpError","message":error.message},"code":error.code,"details":error.details,"instructions":"Correct the tool arguments using get_guide and retry. Do not claim the operation succeeded. For a duplicate review, use details.reviewSchedule and do not submit another review ID."})
        }
    };
    let output = output::cap(&result);
    let id = item
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(part) = progress.content.iter_mut().find(|part| {
        part.get("type").and_then(Value::as_str) == Some("tool_call")
            && part.get("id").and_then(Value::as_str) == Some(id)
    }) && let Some(map) = part.as_object_mut()
    {
        map.insert("status".to_owned(), json!("completed"));
        map.insert("input".to_owned(), json!(args));
        map.insert("output".to_owned(), json!(output));
    }
    if succeeded
        && matches!(name, "sql_execute" | "submit_review")
        && result
            .get("data")
            .and_then(|data| data.get("workspaceId"))
            .and_then(Value::as_str)
            .is_none_or(|workspace| workspace == job.workspace.to_string())
    {
        let mut tx = scoped(
            &state.pool,
            &job.user.to_string(),
            Some(&job.workspace.to_string()),
        )
        .await?;
        sqlx::query("UPDATE ai.chat_sessions SET main_content_invalidation_version=main_content_invalidation_version+1,updated_at=now() WHERE session_id=$1").bind(job.session).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    let output = json!({"type":"function_call_output","call_id":id,"output":output});
    progress.replay.push(output.clone());
    persist(state, job, claim, progress).await?;
    Ok(output)
}

fn absorb_output(
    progress: &mut Progress,
    input: &mut Vec<Value>,
    output: &[Value],
    user_key: bool,
) {
    for item in output {
        if let Some(replay) = replay_item(item, user_key) {
            progress.replay.push(replay.clone());
            if let Some(input_item) = input_item(&replay, user_key) {
                input.push(input_item);
            }
        }
        if item.get("type").and_then(Value::as_str) == Some("message")
            && !progress.content.iter().any(|part| {
                part.get("type").and_then(Value::as_str) == Some("text")
                    && position(
                        part,
                        item.get("id").and_then(Value::as_str).unwrap_or_default(),
                    )
            })
        {
            for text in item
                .get("content")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
            {
                append_text(
                    progress,
                    &json!({"item_id":item.get("id"),"delta":text.get("text")}),
                    false,
                );
            }
        }
    }
}

async fn generate(state: &AppState, job: &Job, claim: &Claim) -> Result<(), ApiError> {
    let mut input = history(state, job).await?;
    let mut progress = Progress::default();
    for step in 0..=30 {
        progress.response_index = u64::try_from(step).map_err(|_| ApiError::internal())?;
        let summary = step == 30;
        if summary {
            input.push(json!({"role":"developer","content":"The tool-call budget for this turn is exhausted. Summarize what was completed, explain any remaining work, and ask the user to continue."}));
        }
        let body = request_body(state, job, claim, &input, summary);
        let completed = stream(
            state,
            job,
            claim,
            &mut progress,
            send(state, job, &body).await?,
        )
        .await?;
        let model = job
            .reference
            .as_ref()
            .map_or(claim.model_id.as_str(), |reference| {
                reference.model_id.as_str()
            });
        usage(state, job, claim, &completed, model).await?;
        let output = completed
            .get("output")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "AI_RESPONSE_INVALID",
                    "The AI provider returned an invalid response.",
                )
            })?;
        absorb_output(
            &mut progress,
            &mut input,
            output,
            job.reference.is_some() || job.api_key.is_some(),
        );
        persist(state, job, claim, &progress).await?;
        if completed.get("status").and_then(Value::as_str) == Some("incomplete") {
            if has_visible_text(&progress) {
                return Ok(());
            }
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "AI_RESPONSE_INCOMPLETE",
                "The response reached its output limit. Send another message to continue.",
            ));
        }
        let calls: Vec<_> = output
            .iter()
            .enumerate()
            .filter(|(_, item)| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .collect();
        if calls.is_empty() {
            if let Err(error) =
                super::suggestions::generate(state, job, claim.assistant_item_id, &progress.content)
                    .await
            {
                tracing::warn!(code=%error.code,"Chat follow-up suggestions failed");
            }
            return Ok(());
        }
        if summary {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "AI_TOOL_LIMIT",
                "The tool-call limit was reached. Send another message to continue.",
            ));
        }
        for (index, item) in calls {
            input.push(
                execute_tool(
                    state,
                    job,
                    claim,
                    &mut progress,
                    item,
                    u64::try_from(index).map_err(|_| ApiError::internal())?,
                )
                .await?,
            );
        }
        if context_full(&input) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "AI_CONTEXT_TOO_LARGE",
                "This turn reached its context limit. Send another message to continue.",
            ));
        }
    }
    Ok(())
}

fn has_visible_text(progress: &Progress) -> bool {
    progress.content.iter().any(|part| {
        part.get("type").and_then(Value::as_str) == Some("text")
            && part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
    })
}

fn context_full(input: &[Value]) -> bool {
    input
        .iter()
        .map(|item| item.to_string().len())
        .fold(0_usize, usize::saturating_add)
        > 800_000
}

pub(super) async fn run(state: AppState, job: Job) {
    let claim = match claim(&state, &job).await {
        Ok(Some(claim)) => claim,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(code=%error.code,run=%job.run,"Unable to claim chat run");
            return;
        }
    };
    let generation = generate(&state, &job, &claim);
    tokio::pin!(generation);
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(5));
    let deadline = tokio::time::sleep(Duration::from_mins(9));
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            result=&mut generation=>break result,
            ()=&mut deadline=>break Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT,"AI_RUN_TIMEOUT","This response reached its time limit. Send another message to continue.")),
            _=heartbeat_interval.tick()=>match heartbeat(&state,&job,&claim).await{Ok(true)=>{},Ok(false)=>return,Err(error)=>break Err(error)},
        }
    };
    if let Err(error) = finish(&state, &job, &claim, result).await {
        tracing::error!(code=%error.code,run=%job.run,"Unable to finalize chat run");
    }
}
