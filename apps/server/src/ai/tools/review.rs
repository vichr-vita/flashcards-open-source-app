//! Online review tools reuse the authoritative scheduler and append-only review facts.
use super::ToolActor;
use crate::{
    AppState,
    core::{cards, model::Mutation, schedule, sync, workspaces},
    database::scoped,
    error::ApiError,
};
use axum::http::StatusCode;
use chrono::{SecondsFormat, SubsecRound, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub(super) fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "REVIEW_INPUT_INVALID", message)
}
fn id(args: &Value, key: &str) -> Result<Uuid, ApiError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{key}: Required UUID")))?
        .trim_matches(crate::core::cards::js_space)
        .parse()
        .map_err(|_| invalid(format!("{key}: Invalid UUID")))
}
fn normalized(tag: &str) -> String {
    cards::normalize_key(tag)
}
async fn filter(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    args: &Value,
) -> Result<Option<Vec<String>>, ApiError> {
    if args.get("tags").is_some() && args.get("deckId").is_some() {
        return Err(invalid("Provide either tags or deckId, not both"));
    }
    let deck = args.get("deckId").is_some();
    let requested = if deck {
        let deck_id = id(args, "deckId")?;
        let filter:Value=sqlx::query_scalar("SELECT filter_definition FROM content.decks WHERE workspace_id=$1 AND deck_id=$2 AND deleted_at IS NULL").bind(workspace).bind(deck_id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::new(StatusCode::NOT_FOUND,"DECK_NOT_FOUND","Deck not found"))?;
        filter
            .get("tags")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(ApiError::internal)?
    } else if let Some(tags) = args.get("tags") {
        tags.as_array()
            .cloned()
            .ok_or_else(|| invalid("tags: Expected an array"))?
    } else {
        return Ok(None);
    };
    if deck && requested.is_empty() {
        return Ok(None);
    }
    if requested.len() > 100 {
        return Err(invalid("tags: Must contain at most 100 items"));
    }
    let requested = requested
        .iter()
        .map(|tag| {
            tag.as_str()
                .map(|value| value.trim_matches(crate::core::cards::js_space))
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| invalid("tags: Expected nonempty strings"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let stored:Vec<String>=sqlx::query_scalar("SELECT DISTINCT tag FROM content.cards c CROSS JOIN LATERAL unnest(c.tags) tag WHERE workspace_id=$1 AND deleted_at IS NULL").bind(workspace).fetch_all(&mut **tx).await?;
    let mut result = Vec::new();
    let mut missing = Vec::new();
    for name in requested {
        let names = stored
            .iter()
            .filter(|tag| normalized(tag) == normalized(&name))
            .cloned()
            .collect::<Vec<_>>();
        if names.is_empty() && !deck {
            missing.push(name.clone());
        }
        result.extend(names);
        if deck {
            result.push(name);
        }
    }
    if !missing.is_empty() {
        return Err(invalid(format!(
            "Unknown tags: {}",
            missing
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|name| seen.insert(name.clone()));
    Ok(Some(result))
}

pub(super) async fn next(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    args: &Value,
) -> Result<Value, ApiError> {
    let mut tx = scoped(&state.pool, &user.to_string(), Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let tags = filter(&mut tx, workspace, args).await?;
    let card:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('cardId',card_id,'frontText',front_text) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL AND ($2::text[] IS NULL OR tags && $2) AND (due_at<=now() OR due_at IS NULL) ORDER BY CASE WHEN due_at<=now() AND fsrs_last_reviewed_at BETWEEN now()-INTERVAL '1 hour' AND now() THEN 0 WHEN due_at<=now() THEN 1 ELSE 2 END,due_at ASC NULLS LAST,created_at ASC,card_id ASC LIMIT 1").bind(workspace).bind(tags).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    Ok(json!({"workspaceId":workspace,"card":card}))
}
pub(super) async fn reveal(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    args: &Value,
) -> Result<Value, ApiError> {
    let card_id = id(args, "cardId")?;
    let card = cards::get_card(state, &user.to_string(), workspace, card_id).await?;
    if card.snapshot.deleted_at.is_some() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "CARD_NOT_FOUND",
            "Card not found",
        ));
    }
    Ok(json!({"workspaceId":workspace,"cardId":card_id,"backText":card.snapshot.back_text}))
}

#[allow(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    clippy::arithmetic_side_effects,
    reason = "The existing intervalSeconds contract represents millisecond differences as a JavaScript double."
)]
fn schedule_details(card: &crate::core::model::CardSnapshot) -> Value {
    let seconds = card
        .due_at
        .zip(card.fsrs_last_reviewed_at)
        .filter(|(due, last)| due >= last)
        .map(|(due, last)| due.signed_duration_since(last).num_milliseconds() as f64 / 1000.0);
    json!({"cardId":card.card_id,"dueAt":card.due_at.map(|date|date.to_rfc3339_opts(SecondsFormat::Millis,true)),"intervalSeconds":seconds,"scheduledDays":card.fsrs_scheduled_days,"state":card.fsrs_card_state,"reps":card.reps,"lapses":card.lapses})
}

#[allow(
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "Keep duplicate, skew, event, schedule, and progress updates in one transaction; intervalSeconds retains millisecond precision."
)]
pub(super) async fn submit(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    args: &Value,
    actor: &ToolActor,
) -> Result<Value, ApiError> {
    let card_id = id(args, "cardId")?;
    let review_id = id(args, "reviewId")?;
    let rating = args
        .get("rating")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("rating: Required"))?;
    let grade = match rating {
        "Again" => 0,
        "Hard" => 1,
        "Good" => 2,
        "Easy" => 3,
        _ => return Err(invalid("rating: Expected Again, Hard, Good, or Easy")),
    };
    let timezone = args
        .get("reviewedTimeZone")
        .and_then(Value::as_str)
        .map(|value| value.trim_matches(crate::core::cards::js_space))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("reviewedTimeZone: Required IANA timezone"))?;
    let mut tx = scoped(&state.pool, &user.to_string(), Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let valid: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
            .bind(timezone)
            .fetch_one(&mut *tx)
            .await?;
    if !valid {
        return Err(invalid("reviewedTimeZone: Must be a valid IANA timezone"));
    }
    let replica = sync::ensure_system_replica(
        &mut tx,
        &user.to_string(),
        workspace,
        &actor.kind,
        &actor.key,
    )
    .await?;
    sqlx::query("UPDATE sync.workspace_replicas SET app_version=$2 WHERE replica_id=$1")
        .bind(replica)
        .bind(&actor.app_version)
        .execute(&mut *tx)
        .await?;
    sync::lock_hot(&mut tx, workspace).await?;
    sqlx::query("SELECT card_id FROM content.cards WHERE workspace_id=$1 AND card_id=$2 AND deleted_at IS NULL FOR UPDATE").bind(workspace).bind(card_id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::new(StatusCode::NOT_FOUND,"CARD_NOT_FOUND","Card not found"))?;
    let mut snapshot = cards::card_in_tx(&mut tx, workspace, card_id)
        .await?
        .ok_or_else(ApiError::internal)?
        .snapshot;
    let client_event = format!("agent-review:{review_id}");
    let previous:Option<Uuid>=sqlx::query_scalar("SELECT card_id FROM content.review_events WHERE workspace_id=$1 AND replica_id=$2 AND client_event_id=$3").bind(workspace).bind(replica).bind(&client_event).fetch_optional(&mut *tx).await?;
    if let Some(previous) = previous {
        return Err(if previous == card_id {
            ApiError::new(StatusCode::CONFLICT,"REVIEW_EVENT_CONFLICT","This review was already recorded. The card's current schedule is in details.reviewSchedule.").with_details(json!({"reviewSchedule":schedule_details(&snapshot)}))
        } else {
            ApiError::new(
                StatusCode::CONFLICT,
                "REVIEW_ID_CARD_MISMATCH",
                format!(
                    "This reviewId already identifies the recorded review of card {previous}, so no review was recorded for card {card_id}. Generate a new reviewId for this review; reuse a reviewId only to retry the same card's submission."
                ),
            )
        });
    }
    let now = Utc::now().trunc_subsecs(3);
    if snapshot
        .fsrs_last_reviewed_at
        .is_some_and(|last| last >= now)
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "REVIEW_STALE",
            "The card's stored review time is at or after the current server time, so it cannot be reviewed again until server time passes that instant.",
        ));
    }
    let scheduled = schedule::compute_review_schedule(
        &snapshot,
        &cards::scheduler_in_tx(&mut tx, workspace).await?,
        grade,
        now,
    )?;
    let event = Uuid::new_v4();
    let inserted=sync::append_review(&mut tx,&user.to_string(),workspace,replica,&json!({"reviewEventId":event,"cardId":card_id,"clientEventId":client_event,"rating":grade,"reviewedAtClient":now.to_rfc3339_opts(SecondsFormat::Millis,true),"reviewedAtServer":now.to_rfc3339_opts(SecondsFormat::Millis,true),"reviewedTimeZone":timezone})).await?;
    if !inserted {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "REVIEW_EVENT_CONFLICT",
            "This review was already recorded.",
        ));
    }
    snapshot.due_at = Some(scheduled.due_at);
    snapshot.reps = scheduled.reps;
    snapshot.lapses = scheduled.lapses;
    snapshot.fsrs_card_state = scheduled.fsrs_card_state.clone();
    snapshot.fsrs_step_index = scheduled.fsrs_step_index;
    snapshot.fsrs_stability = Some(scheduled.fsrs_stability);
    snapshot.fsrs_difficulty = Some(scheduled.fsrs_difficulty);
    snapshot.fsrs_last_reviewed_at = Some(now);
    snapshot.fsrs_scheduled_days = Some(scheduled.fsrs_scheduled_days);
    cards::overwrite_card_in_tx(
        &mut tx,
        workspace,
        snapshot,
        &Mutation {
            client_updated_at: now,
            replica_id: replica,
            operation_id: client_event,
        },
    )
    .await?;
    let mut facts = crate::core::facts::Buffer::default();
    facts.review(&mut tx, event, false).await?;
    tx.commit().await?;
    facts
        .emit(
            state,
            user,
            workspace,
            if actor.kind == "ai_chat" {
                Some("web")
            } else {
                None
            },
        )
        .await;
    Ok(
        json!({"workspaceId":workspace,"cardId":card_id,"reviewId":review_id,"reviewEventId":event,"rating":rating,"reviewedAt":now.to_rfc3339_opts(SecondsFormat::Millis,true),"dueAt":scheduled.due_at.to_rfc3339_opts(SecondsFormat::Millis,true),"intervalSeconds":scheduled.due_at.signed_duration_since(now).num_milliseconds() as f64/1000.0,"scheduledDays":scheduled.fsrs_scheduled_days,"state":scheduled.fsrs_card_state,"reps":scheduled.reps,"lapses":scheduled.lapses}),
    )
}
