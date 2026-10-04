use super::invalid_feedback;
use crate::{AppState, auth, database, error::ApiError};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    feedback_prompt_event_id: Option<Uuid>,
    feedback_submission_id: Option<Uuid>,
    workspace_id: Option<Uuid>,
    installation_id: Option<Uuid>,
    platform: String,
    app_version: Option<String>,
    locale: Option<String>,
    timezone: Option<String>,
    event_type: Option<String>,
    trigger: Option<String>,
    message: Option<String>,
    created_at_client: DateTime<Utc>,
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/feedback/state", get(state))
        .route("/v1/feedback/prompt-events", post(prompt))
        .route("/v1/feedback/submissions", post(submission))
        .layer(DefaultBodyLimit::max(65_536))
}

fn input(body: &Value, submission: bool) -> Result<Input, ApiError> {
    let keys = if submission {
        [
            "feedbackSubmissionId",
            "workspaceId",
            "installationId",
            "platform",
            "appVersion",
            "locale",
            "timezone",
            "trigger",
            "message",
            "createdAtClient",
        ]
        .as_slice()
    } else {
        [
            "feedbackPromptEventId",
            "workspaceId",
            "installationId",
            "platform",
            "appVersion",
            "locale",
            "timezone",
            "eventType",
            "createdAtClient",
        ]
        .as_slice()
    };
    let object = body.as_object().ok_or_else(invalid_feedback)?;
    if object.len() != keys.len() || !keys.iter().all(|key| object.contains_key(*key)) {
        return Err(invalid_feedback());
    }
    let mut value: Input = serde_json::from_value(body.clone()).map_err(|_| invalid_feedback())?;
    if !["web", "ios", "android"].contains(&value.platform.as_str())
        || !body
            .get("createdAtClient")
            .and_then(Value::as_str)
            .is_some_and(|value| value.ends_with('Z'))
    {
        return Err(invalid_feedback());
    }
    for text in [
        &mut value.app_version,
        &mut value.locale,
        &mut value.timezone,
    ]
    .into_iter()
    .flatten()
    {
        *text = text.trim().to_owned();
        if text.is_empty() {
            return Err(invalid_feedback());
        }
    }
    if submission {
        if value.feedback_submission_id.is_none()
            || !value
                .trigger
                .as_deref()
                .is_some_and(|trigger| ["automatic", "settings"].contains(&trigger))
        {
            return Err(invalid_feedback());
        }
        let message = value.message.as_mut().ok_or_else(invalid_feedback)?;
        *message = message.trim().to_owned();
        if message.is_empty() || message.encode_utf16().count() > 5000 {
            return Err(invalid_feedback());
        }
    } else if value.feedback_prompt_event_id.is_none()
        || !value.event_type.as_deref().is_some_and(|kind| {
            ["automatic_prompt_shown", "automatic_prompt_dismissed"].contains(&kind)
        })
    {
        return Err(invalid_feedback());
    }
    Ok(value)
}

async fn load(tx: &mut Transaction<'_, Postgres>, user: &str) -> Result<Value, ApiError> {
    let row=sqlx::query("SELECT MAX(created_at_server) FILTER(WHERE event_type='automatic_prompt_shown') AS last_prompt, (SELECT MAX(created_at_server) FROM support.feedback_submissions WHERE user_id=$1) AS last_submission FROM support.feedback_prompt_events WHERE user_id=$1").bind(user).fetch_one(&mut **tx).await?;
    let prompt: Option<DateTime<Utc>> = row.try_get("last_prompt")?;
    let submitted: Option<DateTime<Utc>> = row.try_get("last_submission")?;
    let next = prompt
        .into_iter()
        .chain(submitted)
        .max()
        .and_then(|date| date.checked_add_signed(Duration::days(30)));
    Ok(
        json!({"automaticPromptCooldownDays":30,"lastAutomaticPromptShownAt":prompt,"lastFeedbackSubmittedAt":submitted,"nextAutomaticPromptAt":next}),
    )
}

async fn references(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    input: &Input,
) -> Result<(), ApiError> {
    if let Some(workspace) = input.workspace_id {
        let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM org.workspace_memberships WHERE user_id=$1 AND workspace_id=$2)").bind(user).bind(workspace).fetch_one(&mut **tx).await?;
        if !allowed {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "FEEDBACK_WORKSPACE_FORBIDDEN",
                "workspaceId must reference a workspace accessible to the authenticated user.",
            ));
        }
    }
    if let Some(installation) = input.installation_id {
        let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sync.installations WHERE user_id=$1 AND installation_id=$2)").bind(user).bind(installation).fetch_one(&mut **tx).await?;
        if !allowed {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "FEEDBACK_INSTALLATION_FORBIDDEN",
                "installationId must reference an installation owned by the authenticated user.",
            ));
        }
    }
    Ok(())
}

async fn state(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let identity = auth::authenticate(&state, &headers).await?;
    let mut tx = database::scoped(&state.pool, &identity.user_id.to_string(), None).await?;
    let value = load(&mut tx, &identity.user_id.to_string()).await?;
    tx.commit().await?;
    Ok(Json(json!({"feedbackState":value})))
}

async fn prompt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let input = input(&body, false)?;
    let id = input
        .feedback_prompt_event_id
        .ok_or_else(invalid_feedback)?;
    let user = identity.user_id.to_string();
    let mut tx = database::scoped(&state.pool, &user, None).await?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM support.feedback_prompt_events WHERE feedback_prompt_event_id=$1 AND user_id=$2)").bind(id).bind(&user).fetch_one(&mut *tx).await?;
    if !exists {
        references(&mut tx, &user, &input).await?;
        sqlx::query("INSERT INTO support.feedback_prompt_events(feedback_prompt_event_id,user_id,workspace_id,installation_id,platform,app_version,locale,timezone,event_type,created_at_client) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(feedback_prompt_event_id) DO NOTHING")
            .bind(id).bind(&user).bind(input.workspace_id).bind(input.installation_id).bind(&input.platform).bind(&input.app_version).bind(&input.locale).bind(&input.timezone).bind(&input.event_type).bind(input.created_at_client).execute(&mut *tx).await?;
        let visible:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM support.feedback_prompt_events WHERE feedback_prompt_event_id=$1 AND user_id=$2)").bind(id).bind(&user).fetch_one(&mut *tx).await?;
        if !visible {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "FEEDBACK_PROMPT_EVENT_ID_CONFLICT",
                "feedbackPromptEventId is already used by another feedback prompt event.",
            ));
        }
    }
    let value = load(&mut tx, &user).await?;
    tx.commit().await?;
    Ok(Json(json!({"feedbackState":value})))
}

async fn submission(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let input = input(&body, true)?;
    let id = input.feedback_submission_id.ok_or_else(invalid_feedback)?;
    let user = identity.user_id.to_string();
    let mut tx = database::scoped(&state.pool, &user, None).await?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM support.feedback_submissions WHERE feedback_submission_id=$1 AND user_id=$2)").bind(id).bind(&user).fetch_one(&mut *tx).await?;
    if !exists {
        references(&mut tx, &user, &input).await?;
        // No email transport is configured on this private installation. Keep the stored feedback and explicit delivery failure.
        sqlx::query("INSERT INTO support.feedback_submissions(feedback_submission_id,user_id,email,workspace_id,installation_id,platform,app_version,locale,timezone,trigger,message,created_at_client,country,email_notification_status,email_notification_error) VALUES($1,$2,NULL,$3,$4,$5,$6,$7,$8,$9,$10,$11,NULL,'failed','Email delivery is not configured.') ON CONFLICT(feedback_submission_id) DO NOTHING")
            .bind(id).bind(&user).bind(input.workspace_id).bind(input.installation_id).bind(&input.platform).bind(&input.app_version).bind(&input.locale).bind(&input.timezone).bind(&input.trigger).bind(&input.message).bind(input.created_at_client).execute(&mut *tx).await?;
    }
    let created:Option<DateTime<Utc>>=sqlx::query_scalar("SELECT created_at_server FROM support.feedback_submissions WHERE feedback_submission_id=$1 AND user_id=$2").bind(id).bind(&user).fetch_optional(&mut *tx).await?;
    let created = created.ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "FEEDBACK_SUBMISSION_ID_CONFLICT",
            "feedbackSubmissionId is already used by another feedback submission.",
        )
    })?;
    let value = load(&mut tx, &user).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"feedbackSubmissionId":id,"createdAtServer":created,"feedbackState":value}),
    ))
}
