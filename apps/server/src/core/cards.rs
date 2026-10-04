//! Canonical card writes, query paging, and review scheduling.

use super::{
    model::{Card, CardSnapshot, Mutation, SchedulerConfig},
    sync, workspaces,
};
use crate::{
    AppState,
    auth::{authenticate, require_mutation},
    database::scoped,
    error::ApiError,
};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use chrono::{DateTime, SubsecRound, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder, Row, Transaction};
use std::collections::HashSet;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

const CARD_READ: &str = include_str!("card.sql");

/// Read the complete canonical snapshot in the caller’s scoped transaction.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn card_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    card: Uuid,
) -> Result<Option<Card>, ApiError> {
    let value = sqlx::query_scalar::<_, Value>(CARD_READ)
        .bind(workspace)
        .bind(card)
        .fetch_optional(&mut **tx)
        .await?;
    value
        .map(|v| serde_json::from_value(v).map_err(|_| ApiError::internal()))
        .transpose()
}

/// Read one card visible to the authenticated workspace member.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn get_card(
    state: &AppState,
    user: &str,
    workspace: Uuid,
    card: Uuid,
) -> Result<Card, ApiError> {
    let mut tx = scoped(&state.pool, user, Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let card = card_in_tx(&mut tx, workspace, card).await?.ok_or_else(|| {
        ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "CARD_NOT_FOUND",
            "Card not found",
        )
    })?;
    tx.commit().await?;
    Ok(card)
}

/// Ordered mutable writes hold the workspace hot-log lock through transaction commit.
/// Apply a complete snapshot only when its timestamp and actor identity win the LWW order.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn mutate_card_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    snapshot: CardSnapshot,
    mutation: &Mutation,
) -> Result<(Card, bool, Option<i64>), ApiError> {
    write_card_in_tx(tx, workspace, snapshot, mutation, false).await
}

/// Persist a server-owned action even if an offline client snapshot carries a future timestamp.
///
/// # Errors
/// Rejects invalid snapshots or conflicting identities and propagates database failures.
pub async fn overwrite_card_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    snapshot: CardSnapshot,
    mutation: &Mutation,
) -> Result<(Card, bool, Option<i64>), ApiError> {
    write_card_in_tx(tx, workspace, snapshot, mutation, true).await
}

async fn write_card_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    mut snapshot: CardSnapshot,
    mutation: &Mutation,
    force: bool,
) -> Result<(Card, bool, Option<i64>), ApiError> {
    snapshot.validate()?;
    snapshot.created_at = snapshot.created_at.trunc_subsecs(3);
    snapshot.due_at = snapshot.due_at.map(|timestamp| timestamp.trunc_subsecs(3));
    snapshot.fsrs_last_reviewed_at = snapshot
        .fsrs_last_reviewed_at
        .map(|timestamp| timestamp.trunc_subsecs(3));
    snapshot.deleted_at = snapshot
        .deleted_at
        .map(|timestamp| timestamp.trunc_subsecs(3));
    let mutation = Mutation {
        client_updated_at: mutation.client_updated_at.trunc_subsecs(3),
        replica_id: mutation.replica_id,
        operation_id: mutation.operation_id.clone(),
    };
    snapshot.card_type = snapshot.card_type.trim().to_owned();
    if snapshot.card_type.is_empty() {
        snapshot.card_type = "basic".into();
    }
    let mut seen = HashSet::new();
    snapshot.tags.retain(|tag| seen.insert(tag.clone()));
    sync::lock_hot(tx, workspace).await?;
    sqlx::query(
        "SELECT card_id FROM content.cards WHERE workspace_id=$1 AND card_id=$2 FOR UPDATE",
    )
    .bind(workspace)
    .bind(snapshot.card_id)
    .fetch_optional(&mut **tx)
    .await?;
    let old = card_in_tx(tx, workspace, snapshot.card_id).await?;
    if let Some(ref previous) = old {
        let incoming = (
            mutation.client_updated_at,
            mutation.replica_id.to_string(),
            mutation.operation_id.as_bytes(),
        );
        let stored = (
            previous.client_updated_at,
            previous.last_modified_by_replica_id.to_string(),
            previous.last_operation_id.as_bytes(),
        );
        if !force && incoming <= stored {
            let change = sync::latest_hot(tx, workspace, "card", snapshot.card_id).await?;
            return Ok((previous.clone(), false, change));
        }
    } else {
        let conflict: Option<Uuid> = sqlx::query_scalar(
            "SELECT workspace_id FROM sync.find_conflicting_workspace_id('card',$1) LIMIT 1",
        )
        .bind(snapshot.card_id.to_string())
        .fetch_optional(&mut **tx)
        .await?;
        if conflict.is_some_and(|id| id != workspace) {
            return Err(ApiError::new(
                axum::http::StatusCode::CONFLICT,
                "SYNC_ENTITY_WORKSPACE_CONFLICT",
                "Card identity belongs to another workspace",
            ));
        }
    }
    sqlx::query("INSERT INTO content.cards (card_id,workspace_id,front_text,back_text,card_type,metadata,tags,due_at,created_at,reps,lapses,fsrs_card_state,fsrs_step_index,fsrs_stability,fsrs_difficulty,fsrs_last_reviewed_at,fsrs_scheduled_days,deleted_at,client_updated_at,last_modified_by_replica_id,last_operation_id) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21) ON CONFLICT(card_id) DO UPDATE SET front_text=EXCLUDED.front_text,back_text=EXCLUDED.back_text,card_type=EXCLUDED.card_type,metadata=EXCLUDED.metadata,tags=EXCLUDED.tags,due_at=EXCLUDED.due_at,reps=EXCLUDED.reps,lapses=EXCLUDED.lapses,fsrs_card_state=EXCLUDED.fsrs_card_state,fsrs_step_index=EXCLUDED.fsrs_step_index,fsrs_stability=EXCLUDED.fsrs_stability,fsrs_difficulty=EXCLUDED.fsrs_difficulty,fsrs_last_reviewed_at=EXCLUDED.fsrs_last_reviewed_at,fsrs_scheduled_days=EXCLUDED.fsrs_scheduled_days,deleted_at=EXCLUDED.deleted_at,client_updated_at=EXCLUDED.client_updated_at,last_modified_by_replica_id=EXCLUDED.last_modified_by_replica_id,last_operation_id=EXCLUDED.last_operation_id,updated_at=now() WHERE content.cards.workspace_id=EXCLUDED.workspace_id")
        .bind(snapshot.card_id).bind(workspace).bind(&snapshot.front_text).bind(&snapshot.back_text).bind(&snapshot.card_type).bind(&snapshot.metadata).bind(&snapshot.tags).bind(snapshot.due_at).bind(snapshot.created_at).bind(snapshot.reps).bind(snapshot.lapses).bind(&snapshot.fsrs_card_state).bind(snapshot.fsrs_step_index).bind(snapshot.fsrs_stability).bind(snapshot.fsrs_difficulty).bind(snapshot.fsrs_last_reviewed_at).bind(snapshot.fsrs_scheduled_days).bind(snapshot.deleted_at).bind(mutation.client_updated_at).bind(mutation.replica_id).bind(&mutation.operation_id).execute(&mut **tx).await?;
    let change = sync::record_hot(tx, workspace, "card", snapshot.card_id, &mutation).await?;
    let card = card_in_tx(tx, workspace, snapshot.card_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    Ok((card, true, Some(change)))
}

/// Read the persisted scheduler settings for a workspace.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn scheduler_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
) -> Result<SchedulerConfig, ApiError> {
    let config:Value=sqlx::query_scalar("SELECT jsonb_build_object('algorithm',fsrs_algorithm,'desiredRetention',fsrs_desired_retention,'learningStepsMinutes',fsrs_learning_steps_minutes,'relearningStepsMinutes',fsrs_relearning_steps_minutes,'maximumIntervalDays',fsrs_maximum_interval_days,'enableFuzz',fsrs_enable_fuzz) FROM org.workspaces WHERE workspace_id=$1").bind(workspace).fetch_one(&mut **tx).await?;
    serde_json::from_value(config).map_err(|_| ApiError::internal())
}

/// Append a review and persist its exact FSRS transition in one transaction.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn submit_review(
    state: &AppState,
    user: &str,
    workspace: Uuid,
    card_id: Uuid,
    rating: u8,
    reviewed_at: DateTime<Utc>,
    actor_key: &str,
) -> Result<Card, ApiError> {
    let mut tx = scoped(&state.pool, user, Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let replica =
        sync::ensure_system_replica(&mut tx, user, workspace, "ai_chat", actor_key).await?;
    sync::lock_hot(&mut tx, workspace).await?;
    sqlx::query("SELECT card_id FROM content.cards WHERE workspace_id=$1 AND card_id=$2 AND deleted_at IS NULL FOR UPDATE").bind(workspace).bind(card_id).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::bad_request("Card not found"))?;
    let mut snapshot = card_in_tx(&mut tx, workspace, card_id)
        .await?
        .ok_or_else(ApiError::internal)?
        .snapshot;
    let schedule = super::schedule::compute_review_schedule(
        &snapshot,
        &scheduler_in_tx(&mut tx, workspace).await?,
        rating,
        reviewed_at,
    )?;
    snapshot.due_at = Some(schedule.due_at);
    snapshot.reps = schedule.reps;
    snapshot.lapses = schedule.lapses;
    snapshot.fsrs_card_state = schedule.fsrs_card_state;
    snapshot.fsrs_step_index = schedule.fsrs_step_index;
    snapshot.fsrs_stability = Some(schedule.fsrs_stability);
    snapshot.fsrs_difficulty = Some(schedule.fsrs_difficulty);
    snapshot.fsrs_last_reviewed_at = Some(schedule.fsrs_last_reviewed_at);
    snapshot.fsrs_scheduled_days = Some(schedule.fsrs_scheduled_days);
    let event = Uuid::new_v4();
    sync::append_review(&mut tx,user,workspace,replica,&json!({"reviewEventId":event,"cardId":card_id,"clientEventId":event.to_string(),"rating":rating,"reviewedAtClient":reviewed_at})).await?;
    let mutation = Mutation {
        client_updated_at: reviewed_at,
        replica_id: replica,
        operation_id: event.to_string(),
    };
    let (card, _, _) = overwrite_card_in_tx(&mut tx, workspace, snapshot, &mutation).await?;
    let mut facts = super::facts::Buffer::default();
    facts.review(&mut tx, event, false).await?;
    tx.commit().await?;
    facts
        .emit(
            state,
            user.parse().map_err(|_| ApiError::internal())?,
            workspace,
            None,
        )
        .await;
    Ok(card)
}

#[derive(Debug, Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Sort {
    pub key: String,
    pub direction: String,
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CardQuery {
    #[serde(default)]
    pub search_text: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    pub limit: i64,
    #[serde(default)]
    pub sorts: Vec<Sort>,
    #[serde(default)]
    pub filter: Option<Value>,
}

fn column(key: &str) -> Result<&'static str, ApiError> {
    match key {
        "frontText" => Ok("lower(front_text)"),
        "backText" => Ok("lower(back_text)"),
        "tags" => Ok("lower(array_to_string(tags, ', '))"),
        "dueAt" => Ok("due_at"),
        "reps" => Ok("reps"),
        "lapses" => Ok("lapses"),
        "createdAt" => Ok("created_at"),
        "cardId" => Ok("card_id"),
        _ => Err(ApiError::bad_request("sorts key is unsupported")),
    }
}

pub(crate) const fn js_space(value: char) -> bool {
    matches!(value, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
pub(crate) fn normalize_key(value: &str) -> String {
    value
        .nfc()
        .collect::<String>()
        .trim_matches(js_space)
        .to_lowercase()
}

fn filters(
    builder: &mut QueryBuilder<Postgres>,
    workspace: Uuid,
    body: &CardQuery,
) -> Result<(), ApiError> {
    builder
        .push(" WHERE workspace_id=")
        .push_bind(workspace)
        .push(" AND deleted_at IS NULL");
    if let Some(search) = &body.search_text {
        let normalized = normalize_key(search);
        let mut tokens: Vec<String> = normalized
            .split(js_space)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect();
        if tokens.is_empty() {
            return Err(ApiError::bad_request("searchText must not be empty"));
        }
        if tokens.len() > 5 {
            let tail = tokens.iter().skip(4).cloned().collect::<Vec<_>>().join(" ");
            tokens.truncate(4);
            tokens.push(tail);
        }
        for token in tokens {
            let pattern = format!("%{token}%");
            builder.push(" AND (lower(normalize(front_text || ' ' || back_text,NFC)) LIKE ").push_bind(pattern.clone()).push(" OR EXISTS(SELECT 1 FROM unnest(tags) AS tag WHERE lower(normalize(tag,NFC)) LIKE ").push_bind(pattern).push("))");
        }
    }
    if let Some(filter) = &body.filter {
        let tag_values = filter
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| ApiError::bad_request("filter.tags must be an array"))?;
        let tags: Vec<String> = tag_values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(normalize_key)
                    .ok_or_else(|| ApiError::bad_request("tags must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|tag| !tag.is_empty())
            .collect();
        if !tags.is_empty() {
            builder.push(" AND EXISTS(SELECT 1 FROM unnest(tags) AS tag WHERE lower(btrim(normalize(tag,NFC), E'\\u0009\\u000A\\u000B\\u000C\\u000D\\u0020\\u00A0\\u1680\\u2000\\u2001\\u2002\\u2003\\u2004\\u2005\\u2006\\u2007\\u2008\\u2009\\u200A\\u2028\\u2029\\u202F\\u205F\\u3000\\uFEFF')) = ANY(").push_bind(tags).push("))");
        }
    }
    Ok(())
}

fn bind_cursor(
    builder: &mut QueryBuilder<Postgres>,
    key: &str,
    value: &Value,
) -> Result<(), ApiError> {
    match key {
        "reps" | "lapses" => {
            builder.push_bind(
                value
                    .as_i64()
                    .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?,
            );
        }
        "dueAt" | "createdAt" => {
            let stamp = value
                .as_str()
                .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?
                .parse::<DateTime<Utc>>()
                .map_err(|_| ApiError::bad_request("cursor is invalid"))?;
            builder.push_bind(stamp);
        }
        "cardId" => {
            let id = value
                .as_str()
                .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?
                .parse::<Uuid>()
                .map_err(|_| ApiError::bad_request("cursor is invalid"))?;
            builder.push_bind(id);
        }
        _ => {
            builder.push_bind(
                value
                    .as_str()
                    .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?
                    .to_owned(),
            );
        }
    }
    Ok(())
}

fn apply_cursor(query: &mut QueryBuilder<Postgres>, body: &CardQuery) -> Result<(), ApiError> {
    if let Some(cursor) = &body.cursor {
        let values = sync::decode_cursor(cursor)?;
        if values.len() != body.sorts.len() {
            return Err(ApiError::bad_request("cursor is invalid"));
        }
        query.push(" AND (");
        for (index, (sort, value)) in body.sorts.iter().zip(&values).enumerate() {
            if index > 0 {
                query.push(" OR ");
            }
            query.push("(");
            for (prior, prior_value) in body.sorts.iter().zip(&values).take(index) {
                query
                    .push(column(&prior.key)?)
                    .push(" IS NOT DISTINCT FROM ");
                if prior_value.is_null() {
                    query.push("NULL");
                } else {
                    bind_cursor(query, &prior.key, prior_value)?;
                }
                query.push(" AND ");
            }
            let expression = column(&sort.key)?;
            if sort.key == "dueAt" && value.is_null() {
                query.push(if sort.direction == "asc" {
                    "due_at IS NOT NULL"
                } else {
                    "FALSE"
                });
            } else {
                query
                    .push("(")
                    .push(expression)
                    .push(if sort.direction == "asc" {
                        " > "
                    } else {
                        " < "
                    });
                bind_cursor(query, &sort.key, value)?;
                if sort.key == "dueAt" && sort.direction == "desc" {
                    query.push(" OR due_at IS NULL");
                }
                query.push(")");
            }
            query.push(")");
        }
        query.push(")");
    }
    Ok(())
}

/// Return a filtered card page with a stable cursor and total count.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn query_cards(
    state: &AppState,
    user: &str,
    workspace: Uuid,
    mut body: CardQuery,
) -> Result<Value, ApiError> {
    if !(1..=100).contains(&body.limit) || body.sorts.len() > 3 {
        return Err(ApiError::bad_request("Invalid card page limit or sorts"));
    }
    let mut keys = HashSet::new();
    for sort in &body.sorts {
        column(&sort.key)?;
        if !matches!(sort.direction.as_str(), "asc" | "desc") || !keys.insert(sort.key.clone()) {
            return Err(ApiError::bad_request("Invalid card sort"));
        }
    }
    if !keys.contains("createdAt") {
        body.sorts.push(Sort {
            key: "createdAt".into(),
            direction: "desc".into(),
        });
    }
    body.sorts.push(Sort {
        key: "cardId".into(),
        direction: "asc".into(),
    });
    let mut tx = scoped(&state.pool, user, Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let mut count = QueryBuilder::new("SELECT count(*)::bigint FROM content.cards");
    filters(&mut count, workspace, &body)?;
    let total: i64 = count.build_query_scalar().fetch_one(&mut *tx).await?;
    let mut query = QueryBuilder::new("SELECT card_id FROM content.cards");
    filters(&mut query, workspace, &body)?;
    apply_cursor(&mut query, &body)?;
    query.push(" ORDER BY ");
    for (index, sort) in body.sorts.iter().enumerate() {
        if index > 0 {
            query.push(",");
        }
        query
            .push(column(&sort.key)?)
            .push(if sort.direction == "asc" {
                " ASC"
            } else {
                " DESC"
            });
        if sort.key == "dueAt" {
            query.push(if sort.direction == "asc" {
                " NULLS FIRST"
            } else {
                " NULLS LAST"
            });
        }
    }
    query
        .push(" LIMIT ")
        .push_bind(body.limit.checked_add(1).ok_or_else(ApiError::internal)?);
    let mut ids: Vec<Uuid> = query.build_query_scalar().fetch_all(&mut *tx).await?;
    let limit = usize::try_from(body.limit).map_err(|_| ApiError::bad_request("Invalid limit"))?;
    let more = ids.len() > limit;
    ids.truncate(limit);
    let mut cards = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(card) = card_in_tx(&mut tx, workspace, id).await? {
            cards.push(card);
        }
    }
    let cursor = if more {
        cards.last().map(|card| {
            let values: Vec<Value> = body
                .sorts
                .iter()
                .map(|sort| match sort.key.as_str() {
                    "frontText" => json!(card.snapshot.front_text.to_lowercase()),
                    "backText" => json!(card.snapshot.back_text.to_lowercase()),
                    "tags" => json!(card.snapshot.tags.join(", ").to_lowercase()),
                    "dueAt" => json!(card.snapshot.due_at),
                    "reps" => json!(card.snapshot.reps),
                    "lapses" => json!(card.snapshot.lapses),
                    "createdAt" => json!(card.snapshot.created_at),
                    _ => json!(card.snapshot.card_id),
                })
                .collect();
            sync::encode_cursor(&values)
        })
    } else {
        None
    };
    tx.commit().await?;
    Ok(json!({"cards":cards,"nextCursor":cursor,"totalCount":total}))
}

pub(super) async fn query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(body): Json<CardQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers).await?;
    Ok(Json(
        query_cards(&state, &user.user_id.to_string(), workspace, body).await?,
    ))
}

pub(super) async fn tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?;
    let mut tx = scoped(
        &state.pool,
        &user.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL",
    )
    .bind(workspace)
    .fetch_one(&mut *tx)
    .await?;
    let rows=sqlx::query("SELECT tag,count(*)::bigint AS cards_count FROM content.cards CROSS JOIN LATERAL unnest(tags) AS tag WHERE workspace_id=$1 AND deleted_at IS NULL GROUP BY tag ORDER BY cards_count DESC,lower(tag) ASC,tag ASC").bind(workspace).fetch_all(&mut *tx).await?;
    let tags:Vec<Value>=rows.iter().map(|row|Ok(json!({"tag":row.try_get::<String,_>("tag")?,"cardsCount":row.try_get::<i64,_>("cards_count")?}))).collect::<Result<_,sqlx::Error>>()?;
    tx.commit().await?;
    Ok(Json(json!({"tags":tags,"totalCards":total})))
}
