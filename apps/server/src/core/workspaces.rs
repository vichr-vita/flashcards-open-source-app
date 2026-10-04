//! Workspace lifecycle keeps account selection, memberships, and replica roots transactional.

use super::{cards, model::Mutation, sync};
use crate::{
    AppState,
    auth::{authenticate, require_mutation},
    database::scoped,
    error::ApiError,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

const DELETE_CONFIRM: &str = "delete workspace";
const RESET_CONFIRM: &str = "reset all progress for all cards in this workspace";

/// Check membership using the preserved security-definer contract.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn assert_access(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
) -> Result<(), ApiError> {
    let allowed: bool = sqlx::query_scalar("SELECT security.user_has_workspace_access($1)")
        .bind(workspace)
        .fetch_one(&mut **tx)
        .await?;
    if !allowed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "WORKSPACE_NOT_FOUND",
            "Workspace not found",
        ));
    }
    Ok(())
}

async fn assert_owner(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    sole: bool,
    reset: bool,
) -> Result<(), ApiError> {
    assert_access(tx, workspace).await?;
    let owner: bool = sqlx::query_scalar("SELECT security.current_user_is_workspace_owner($1)")
        .bind(workspace)
        .fetch_one(&mut **tx)
        .await?;
    if !owner {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "WORKSPACE_OWNER_REQUIRED",
            "Only workspace owners can manage this workspace",
        ));
    }
    if sole {
        let single: bool =
            sqlx::query_scalar("SELECT security.current_user_is_sole_workspace_member($1)")
                .bind(workspace)
                .fetch_one(&mut **tx)
                .await?;
        if !single {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                if reset {
                    "WORKSPACE_RESET_SHARED"
                } else {
                    "WORKSPACE_DELETE_SHARED"
                },
                "This workspace still has multiple members.",
            ));
        }
    }
    Ok(())
}

async fn lock_lifecycle(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
) -> Result<(), ApiError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,1::bigint))")
        .bind(workspace.to_string())
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended($1::text||':'||$2::text,0::bigint))",
    )
    .bind(user)
    .bind(workspace.to_string())
    .execute(&mut **tx)
    .await?;
    assert_access(tx, workspace).await
}

/// Resolve an explicit workspace or the account’s selected workspace.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn resolve_workspace(
    state: &AppState,
    user: &str,
    explicit: Option<Uuid>,
) -> Result<Uuid, ApiError> {
    let mut tx = scoped(&state.pool, user, None).await?;
    let id = if let Some(id) = explicit {
        id
    } else {
        sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT workspace_id FROM org.user_settings WHERE user_id=$1",
        )
        .bind(user)
        .fetch_optional(&mut *tx)
        .await?
        .flatten()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "WORKSPACE_SELECTION_REQUIRED",
                "Select a workspace before using this endpoint",
            )
        })?
    };
    assert_access(&mut tx, id).await?;
    tx.commit().await?;
    Ok(id)
}

async fn summary(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
) -> Result<Value, ApiError> {
    sqlx::query_scalar("SELECT jsonb_build_object('workspaceId',w.workspace_id,'name',w.name,'createdAt',to_char(w.created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),'isSelected',w.workspace_id IS NOT DISTINCT FROM s.workspace_id) FROM org.workspaces w JOIN org.user_settings s ON s.user_id=$1 WHERE w.workspace_id=$2").bind(user).bind(workspace).fetch_one(&mut **tx).await.map_err(Into::into)
}

/// Shared by the browser and AI workspace tools. Statistics do not change the browser list shape.
/// Read accessible workspaces and statistics for the AI tools.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn list_workspaces(state: &AppState, user: &str) -> Result<Value, ApiError> {
    let mut tx = scoped(&state.pool, user, None).await?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT w.workspace_id FROM org.workspaces w JOIN org.workspace_memberships m ON m.workspace_id=w.workspace_id AND m.user_id=$1 ORDER BY w.created_at ASC,w.workspace_id ASC LIMIT 100").bind(user).fetch_all(&mut *tx).await?;
    let mut rows = Vec::new();
    for id in ids {
        sqlx::query("SELECT set_config('app.workspace_id',$1,true)")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        let mut value = summary(&mut tx, user, id).await?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let activity:Option<String>=sqlx::query_scalar("SELECT to_char(GREATEST((SELECT max(reviewed_at_server) FROM content.review_events WHERE workspace_id=$1),(SELECT max(updated_at) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL)) AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')").bind(id).fetch_one(&mut *tx).await?;
        let object = value.as_object_mut().ok_or_else(ApiError::internal)?;
        object.insert("cardCount".into(), json!(count));
        object.insert("lastActivityAt".into(), json!(activity));
        rows.push(value);
    }
    tx.commit().await?;
    Ok(json!({"workspaces":rows}))
}

#[derive(Deserialize)]
pub struct Page {
    pub limit: i64,
    pub cursor: Option<String>,
}

pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Json<Value>, ApiError> {
    if !(1..=100).contains(&page.limit) {
        return Err(ApiError::bad_request("limit must be between 1 and 100"));
    }
    let user = authenticate(&state, &headers).await?;
    let user = user.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (after_time, after_id) = if let Some(cursor) = &page.cursor {
        let values = sync::decode_cursor(cursor)?;
        if values.len() != 2 {
            return Err(ApiError::bad_request(
                "cursor does not match the requested workspaces order",
            ));
        }
        let mut values = values.iter();
        let time = values
            .next()
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?
            .parse::<DateTime<Utc>>()
            .map_err(|_| ApiError::bad_request("cursor is invalid"))?;
        let id = values
            .next()
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request("cursor is invalid"))?
            .parse::<Uuid>()
            .map_err(|_| ApiError::bad_request("cursor is invalid"))?;
        (Some(time), Some(id))
    } else {
        (None, None)
    };
    let rows=sqlx::query("SELECT w.workspace_id,w.created_at FROM org.workspaces w JOIN org.workspace_memberships m ON m.workspace_id=w.workspace_id AND m.user_id=$1 WHERE $2::timestamptz IS NULL OR w.created_at>$2 OR (w.created_at=$2 AND w.workspace_id>$3) ORDER BY w.created_at ASC,w.workspace_id ASC LIMIT $4").bind(&user).bind(after_time).bind(after_id).bind(page.limit.checked_add(1).ok_or_else(ApiError::internal)?).fetch_all(&mut *tx).await?;
    let limit = usize::try_from(page.limit).map_err(|_| ApiError::bad_request("Invalid limit"))?;
    let more = rows.len() > limit;
    let mut workspaces = Vec::new();
    let mut next = None;
    for row in rows.iter().take(limit) {
        let id: Uuid = row.try_get("workspace_id")?;
        workspaces.push(summary(&mut tx, &user, id).await?);
        if more {
            next = Some(sync::encode_cursor(&[
                json!(row.try_get::<DateTime<Utc>, _>("created_at")?),
                json!(id),
            ]));
        }
    }
    tx.commit().await?;
    Ok(Json(json!({"workspaces":workspaces,"nextCursor":next})))
}

#[derive(Deserialize)]
pub struct Name {
    pub name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Confirmation {
    pub confirmation_text: String,
}

/// Seed settings and their immutable actor together, with deferred foreign keys checked at commit.
/// Create a workspace, owner membership, scheduler seed and replica root together.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn create_workspace_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    name: &str,
) -> Result<Uuid, ApiError> {
    if name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    sqlx::query("SELECT user_id FROM org.user_settings WHERE user_id=$1 FOR UPDATE")
        .bind(user)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::GONE,
                "ACCOUNT_DELETED",
                "Local account no longer exists",
            )
        })?;
    let id = Uuid::new_v4();
    let replica = sync::replica_id(&format!("{id}:workspace_seed:workspace-seed"));
    let now = Utc::now();
    let operation = format!("bootstrap-workspace-{id}");
    sqlx::query("SELECT set_config('app.workspace_id',$1,true)")
        .bind(id.to_string())
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO org.workspaces(workspace_id,name,fsrs_client_updated_at,fsrs_last_modified_by_replica_id,fsrs_last_operation_id) VALUES($1,$2,$3,$4,$5)").bind(id).bind(name.trim()).bind(now).bind(replica).bind(&operation).execute(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO org.workspace_memberships(workspace_id,user_id,role) VALUES($1,$2,'owner')",
    )
    .bind(id)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    sync::ensure_system_replica(tx, user, id, "workspace_seed", "workspace-seed").await?;
    sync::lock_hot(tx, id).await?;
    sync::record_hot(
        tx,
        id,
        "workspace_scheduler_settings",
        id,
        &Mutation {
            client_updated_at: now,
            replica_id: replica,
            operation_id: operation,
        },
    )
    .await?;
    Ok(id)
}

pub(super) async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Name>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let id = create_workspace_in_tx(&mut tx, &user, &body.name).await?;
    sqlx::query("UPDATE org.user_settings SET workspace_id=$2 WHERE user_id=$1")
        .bind(&user)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let summary = summary(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"workspace":summary}))))
}

pub(super) async fn select(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&id.to_string())).await?;
    assert_access(&mut tx, id).await?;
    sqlx::query("UPDATE org.user_settings SET workspace_id=$2 WHERE user_id=$1")
        .bind(&user)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let summary = summary(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(json!({"workspace":summary})))
}

pub(super) async fn rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Name>,
) -> Result<Json<Value>, ApiError> {
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request("name must not be empty"));
    }
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&id.to_string())).await?;
    assert_owner(&mut tx, id, false, false).await?;
    sqlx::query("UPDATE org.workspaces SET name=$2 WHERE workspace_id=$1")
        .bind(id)
        .bind(body.name.trim())
        .execute(&mut *tx)
        .await?;
    let summary = summary(&mut tx, &user, id).await?;
    tx.commit().await?;
    Ok(Json(json!({"workspace":summary})))
}

async fn preview(state: &AppState, user: &str, id: Uuid, reset: bool) -> Result<Value, ApiError> {
    let mut tx = scoped(&state.pool, user, Some(&id.to_string())).await?;
    assert_owner(&mut tx, id, true, reset).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM org.workspaces WHERE workspace_id=$1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let cards: i64 = if reset {
        sqlx::query_scalar("SELECT count(*) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL AND (due_at IS NOT NULL OR reps<>0 OR lapses<>0 OR fsrs_card_state<>'new' OR fsrs_step_index IS NOT NULL OR fsrs_stability IS NOT NULL OR fsrs_difficulty IS NOT NULL OR fsrs_last_reviewed_at IS NOT NULL OR fsrs_scheduled_days IS NOT NULL)").bind(id).fetch_one(&mut *tx).await?
    } else {
        sqlx::query_scalar(
            "SELECT count(*) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?
    };
    let workspace_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM org.workspace_memberships WHERE user_id=$1")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(if reset {
        json!({"workspaceId":id,"workspaceName":name,"cardsToResetCount":cards,"confirmationText":RESET_CONFIRM})
    } else {
        json!({"workspaceId":id,"workspaceName":name,"activeCardCount":cards,"confirmationText":DELETE_CONFIRM,"isLastAccessibleWorkspace":workspace_count==1})
    })
}
pub(super) async fn delete_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?;
    Ok(Json(
        preview(&state, &user.user_id.to_string(), id, false).await?,
    ))
}
pub(super) async fn reset_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?;
    Ok(Json(
        preview(&state, &user.user_id.to_string(), id, true).await?,
    ))
}

pub(super) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Confirmation>,
) -> Result<Json<Value>, ApiError> {
    if body.confirmation_text != DELETE_CONFIRM {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_DELETE_CONFIRMATION_INVALID",
            "Type the confirmation text exactly",
        ));
    }
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&id.to_string())).await?;
    sqlx::query("SELECT user_id FROM org.user_settings WHERE user_id=$1 FOR UPDATE")
        .bind(&user)
        .fetch_one(&mut *tx)
        .await?;
    lock_lifecycle(&mut tx, &user, id).await?;
    assert_owner(&mut tx, id, true, false).await?;
    let cards: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let selected: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM org.user_settings WHERE user_id=$1")
            .bind(&user)
            .fetch_one(&mut *tx)
            .await?;
    let replacement = if selected == Some(id) || selected.is_none() {
        let next:Option<Uuid>=sqlx::query_scalar("SELECT w.workspace_id FROM org.workspaces w JOIN org.workspace_memberships m ON m.workspace_id=w.workspace_id AND m.user_id=$1 WHERE w.workspace_id<>$2 ORDER BY w.created_at ASC,w.workspace_id ASC LIMIT 1").bind(&user).bind(id).fetch_optional(&mut *tx).await?;
        if let Some(next) = next {
            next
        } else {
            create_workspace_in_tx(&mut tx, &user, "Personal").await?
        }
    } else {
        selected.ok_or_else(ApiError::internal)?
    };
    sqlx::query("UPDATE org.user_settings SET workspace_id=$2 WHERE user_id=$1")
        .bind(&user)
        .bind(replacement)
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.workspace_id',$1,true)")
        .bind(id.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('app.workspace_id',$1,true)")
        .bind(replacement.to_string())
        .execute(&mut *tx)
        .await?;
    let summary = summary(&mut tx, &user, replacement).await?;
    tx.commit().await?;
    super::facts::decision(&state, &user, id, "workspace_deleted", Utc::now()).await;
    Ok(Json(
        json!({"ok":true,"deletedWorkspaceId":id,"deletedCardsCount":cards,"workspace":summary}),
    ))
}

pub(super) async fn reset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Confirmation>,
) -> Result<Json<Value>, ApiError> {
    if body.confirmation_text != RESET_CONFIRM {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_RESET_PROGRESS_CONFIRMATION_INVALID",
            "Type the confirmation text exactly",
        ));
    }
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&id.to_string())).await?;
    lock_lifecycle(&mut tx, &user, id).await?;
    assert_owner(&mut tx, id, true, true).await?;
    let replica =
        sync::ensure_system_replica(&mut tx, &user, id, "workspace_reset", "reset-progress")
            .await?;
    sync::lock_hot(&mut tx, id).await?;
    let ids:Vec<Uuid>=sqlx::query_scalar("SELECT card_id FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL AND (due_at IS NOT NULL OR reps<>0 OR lapses<>0 OR fsrs_card_state<>'new' OR fsrs_step_index IS NOT NULL OR fsrs_stability IS NOT NULL OR fsrs_difficulty IS NOT NULL OR fsrs_last_reviewed_at IS NOT NULL OR fsrs_scheduled_days IS NOT NULL) ORDER BY card_id FOR UPDATE").bind(id).fetch_all(&mut *tx).await?;
    for card_id in &ids {
        let mut snapshot = cards::card_in_tx(&mut tx, id, *card_id)
            .await?
            .ok_or_else(ApiError::internal)?
            .snapshot;
        snapshot.due_at = None;
        snapshot.reps = 0;
        snapshot.lapses = 0;
        snapshot.fsrs_card_state = "new".into();
        snapshot.fsrs_step_index = None;
        snapshot.fsrs_stability = None;
        snapshot.fsrs_difficulty = None;
        snapshot.fsrs_last_reviewed_at = None;
        snapshot.fsrs_scheduled_days = None;
        cards::overwrite_card_in_tx(
            &mut tx,
            id,
            snapshot,
            &Mutation {
                client_updated_at: Utc::now(),
                replica_id: replica,
                operation_id: Uuid::new_v4().to_string(),
            },
        )
        .await?;
    }
    tx.commit().await?;
    if !ids.is_empty() {
        super::facts::decision(&state, &user, id, "study_progress_reset", Utc::now()).await;
    }
    Ok(Json(
        json!({"ok":true,"workspaceId":id,"cardsResetCount":ids.len()}),
    ))
}
