//! Existing offline protocol, with mutable hot state and append-only review history in separate lanes.

use super::{
    cards,
    model::{CardSnapshot, Mutation, SchedulerConfig},
    workspaces,
};
use crate::{AppState, auth::require_mutation, database::scoped, error::ApiError};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SubsecRound, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub installation_id: Uuid,
    pub platform: String,
    pub app_version: Option<String>,
    #[serde(default)]
    pub is_automation: bool,
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation_id: String,
    pub entity_id: Uuid,
    pub client_updated_at: DateTime<Utc>,
    pub entity_type: String,
    pub action: String,
    pub payload: Value,
}

#[derive(Deserialize, ts_rs::TS)]
pub struct Push {
    #[serde(flatten)]
    pub client: Client,
    pub operations: Vec<Operation>,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Pull {
    #[serde(flatten)]
    pub client: Client,
    pub after_hot_change_id: i64,
    pub limit: i64,
    #[serde(default)]
    pub include_media_assets: bool,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct History {
    #[serde(flatten)]
    pub client: Client,
    pub after_review_sequence_id: i64,
    pub limit: i64,
}

#[derive(Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct HistoryImport {
    #[serde(flatten)]
    pub client: Client,
    pub review_events: Vec<Value>,
}

#[must_use]
pub fn replica_id(seed: &str) -> Uuid {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0_u8; 16];
    for (target, source) in bytes.iter_mut().zip(digest.iter()) {
        *target = *source;
    }
    if let Some(version) = bytes.get_mut(6) {
        *version = (*version & 0x0f) | 0x50;
    }
    if let Some(variant) = bytes.get_mut(8) {
        *variant = (*variant & 0x3f) | 0x80;
    }
    Uuid::from_bytes(bytes)
}

fn invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "SYNC_INVALID_INPUT",
        "Cloud sync failed. Try again.",
    )
}
fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, ApiError> {
    serde_json::from_value(value.clone()).map_err(|_| invalid())
}
/// Decode a required timestamp from a protocol payload.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub fn date(value: &Value, key: &str) -> Result<DateTime<Utc>, ApiError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .parse::<DateTime<Utc>>()
        .map(|timestamp| timestamp.trunc_subsecs(3))
        .map_err(|_| invalid())
}
/// Decode a required UUID from a protocol payload.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub fn id(value: &Value, key: &str) -> Result<Uuid, ApiError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())
}
/// Read a nonempty protocol text field.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(invalid)
}

#[must_use]
pub fn encode_cursor(values: &[Value]) -> String {
    URL_SAFE_NO_PAD.encode(json!({"values":values}).to_string())
}
/// Decode the existing opaque JSON paging cursor.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub fn decode_cursor(value: &str) -> Result<Vec<Value>, ApiError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::bad_request("cursor is invalid"))?;
    let value: Value =
        serde_json::from_slice(&decoded).map_err(|_| ApiError::bad_request("cursor is invalid"))?;
    value
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| ApiError::bad_request("cursor is invalid"))
}

/// Serialize mutable changes with the workspace hot-log cursor.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn lock_hot(tx: &mut Transaction<'_, Postgres>, workspace: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO sync.workspace_sync_metadata(workspace_id) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(workspace)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "SELECT workspace_id FROM sync.workspace_sync_metadata WHERE workspace_id=$1 FOR UPDATE",
    )
    .bind(workspace)
    .fetch_one(&mut **tx)
    .await?;
    Ok(())
}

/// Record a canonical mutable snapshot change in the hot lane.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn record_hot(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    entity_type: &str,
    entity: Uuid,
    mutation: &Mutation,
) -> Result<i64, ApiError> {
    sqlx::query_scalar("INSERT INTO sync.hot_changes(workspace_id,entity_type,entity_id,action,replica_id,operation_id,client_updated_at) VALUES($1,$2,$3,'upsert',$4,$5,$6) RETURNING change_id").bind(workspace).bind(entity_type).bind(entity.to_string()).bind(mutation.replica_id).bind(&mutation.operation_id).bind(mutation.client_updated_at).fetch_one(&mut **tx).await.map_err(Into::into)
}

/// Read the last hot change associated with an entity.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn latest_hot(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    kind: &str,
    entity: Uuid,
) -> Result<Option<i64>, ApiError> {
    sqlx::query_scalar("SELECT change_id FROM sync.hot_changes WHERE workspace_id=$1 AND entity_type=$2 AND entity_id=$3 ORDER BY change_id DESC LIMIT 1").bind(workspace).bind(kind).bind(entity.to_string()).fetch_optional(&mut **tx).await.map_err(Into::into)
}

async fn lock_access(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended($1::text||':'||$2::text,0::bigint))",
    )
    .bind(user)
    .bind(workspace.to_string())
    .execute(&mut **tx)
    .await?;
    workspaces::assert_access(tx, workspace).await?;
    sqlx::query("SELECT workspace_id FROM org.workspaces WHERE workspace_id=$1 FOR KEY SHARE")
        .bind(workspace)
        .fetch_one(&mut **tx)
        .await?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "The immutable replica row has distinct identity columns that must be checked together."
)]
async fn replica_row(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    replica: Uuid,
    kind: &str,
    installation: Option<Uuid>,
    key: Option<&str>,
    platform: &str,
    version: Option<&str>,
) -> Result<(), ApiError> {
    let row=sqlx::query("INSERT INTO sync.workspace_replicas(replica_id,workspace_id,user_id,actor_kind,installation_id,actor_key,platform,app_version) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(replica_id) DO UPDATE SET user_id=EXCLUDED.user_id,app_version=EXCLUDED.app_version,last_seen_at=now() WHERE sync.workspace_replicas.workspace_id=EXCLUDED.workspace_id AND sync.workspace_replicas.actor_kind=EXCLUDED.actor_kind AND sync.workspace_replicas.installation_id IS NOT DISTINCT FROM EXCLUDED.installation_id AND sync.workspace_replicas.actor_key IS NOT DISTINCT FROM EXCLUDED.actor_key AND sync.workspace_replicas.platform=EXCLUDED.platform RETURNING replica_id").bind(replica).bind(workspace).bind(user).bind(kind).bind(installation).bind(key).bind(platform).bind(version).fetch_optional(&mut **tx).await?;
    if row.is_none() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "SYNC_REPLICA_CONFLICT",
            "workspace replica identity conflicts with existing sync metadata",
        ));
    }
    Ok(())
}

/// Reuse the immutable identity of a server action in a workspace.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn ensure_system_replica(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    kind: &str,
    key: &str,
) -> Result<Uuid, ApiError> {
    if !matches!(
        kind,
        "workspace_seed" | "workspace_reset" | "agent_connection" | "ai_chat"
    ) {
        return Err(ApiError::internal());
    }
    lock_access(tx, user, workspace).await?;
    let replica = replica_id(&format!("{workspace}:{kind}:{key}"));
    let platform = if kind == "workspace_seed" || kind == "workspace_reset" {
        "system"
    } else {
        "web"
    };
    replica_row(
        tx,
        user,
        workspace,
        replica,
        kind,
        None,
        Some(key),
        platform,
        Some("server"),
    )
    .await?;
    Ok(replica)
}

/// Claim an installation and reuse its immutable workspace replica.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn ensure_client(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    client: &Client,
) -> Result<Uuid, ApiError> {
    if !matches!(client.platform.as_str(), "web" | "ios" | "android")
        || client.app_version.as_ref().is_some_and(String::is_empty)
    {
        return Err(invalid());
    }
    lock_access(tx, user, workspace).await?;
    let claim =
        sqlx::query("SELECT claim_status,is_automation FROM sync.claim_installation($1,$2,$3,$4)")
            .bind(client.installation_id)
            .bind(&client.platform)
            .bind(user)
            .bind(&client.app_version)
            .fetch_one(&mut **tx)
            .await?;
    if claim.try_get::<String, _>("claim_status")? == "platform_mismatch" {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "SYNC_INSTALLATION_PLATFORM_MISMATCH",
            "installationId is already registered with a different platform",
        ));
    }
    if client.is_automation && !claim.try_get::<bool, _>("is_automation")? {
        sqlx::query("UPDATE sync.installations SET is_automation=true WHERE installation_id=$1")
            .bind(client.installation_id)
            .execute(&mut **tx)
            .await?;
    }
    let replica = replica_id(&format!("{workspace}:{}", client.installation_id));
    replica_row(
        tx,
        user,
        workspace,
        replica,
        "client_installation",
        Some(client.installation_id),
        None,
        &client.platform,
        client.app_version.as_deref(),
    )
    .await?;
    Ok(replica)
}

/// Append an idempotent history event and record progress facts only on insertion.
///
/// # Errors
/// Returns a contract error for invalid input or inaccessible data, or a database error.
pub async fn append_review(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    replica: Uuid,
    event: &Value,
) -> Result<bool, ApiError> {
    let event_id = id(event, "reviewEventId")?;
    let card_id = id(event, "cardId")?;
    let client_id = text(event, "clientEventId")?;
    let rating = event
        .get("rating")
        .and_then(Value::as_i64)
        .filter(|rating| (0..=3).contains(rating))
        .ok_or_else(invalid)?;
    let rating = i16::try_from(rating).map_err(|_| invalid())?;
    let reviewed_at = date(event, "reviewedAtClient")?;
    let server = event
        .get("reviewedAtServer")
        .filter(|v| !v.is_null())
        .map(|_| date(event, "reviewedAtServer"))
        .transpose()?
        .unwrap_or_else(Utc::now);
    let timezone = event
        .get("reviewedTimeZone")
        .filter(|v| !v.is_null())
        .map(|v| v.as_str().filter(|s| !s.is_empty()).ok_or_else(invalid))
        .transpose()?;
    if let Some(timezone) = timezone {
        let valid: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_timezone_names WHERE name=$1)")
                .bind(timezone)
                .fetch_one(&mut **tx)
                .await?;
        if !valid {
            return Err(invalid());
        }
    }
    let conflict: Option<Uuid> = sqlx::query_scalar(
        "SELECT workspace_id FROM sync.find_conflicting_workspace_id('review_event',$1) LIMIT 1",
    )
    .bind(event_id.to_string())
    .fetch_optional(&mut **tx)
    .await?;
    if conflict.is_some_and(|id| id != workspace) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "SYNC_ENTITY_WORKSPACE_CONFLICT",
            "Review identity belongs to another workspace",
        ));
    }
    let result=sqlx::query("INSERT INTO content.review_events(review_event_id,workspace_id,card_id,replica_id,client_event_id,rating,reviewed_at_client,reviewed_at_server,reviewed_by_user_id,reviewed_time_zone,reviewed_local_date) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,CASE WHEN $10::text IS NULL THEN NULL ELSE ($7::timestamptz AT TIME ZONE $10)::date END) ON CONFLICT DO NOTHING RETURNING review_event_id").bind(event_id).bind(workspace).bind(card_id).bind(replica).bind(client_id).bind(rating).bind(reviewed_at).bind(server).bind(user).bind(timezone).fetch_optional(&mut **tx).await?;
    if result.is_some() {
        crate::progress::record_review_facts(
            tx,
            crate::progress::ReviewFact {
                user_id: user,
                workspace_id: workspace,
                event_id,
                replica_id: replica,
                rating: u8::try_from(rating).map_err(|_| invalid())?,
                reviewed_at_client: reviewed_at,
                reviewed_at_server: server,
                time_zone: timezone,
            },
        )
        .await?;
    }
    Ok(result.is_some())
}

async fn mutate_deck(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    payload: &Value,
    mutation: &Mutation,
) -> Result<(bool, Option<i64>), ApiError> {
    let deck = id(payload, "deckId")?;
    let name = text(payload, "name")?;
    let created = date(payload, "createdAt")?;
    let deleted = payload
        .get("deletedAt")
        .filter(|v| !v.is_null())
        .map(|_| date(payload, "deletedAt"))
        .transpose()?;
    let filter = payload.get("filterDefinition").ok_or_else(invalid)?;
    if filter.get("version").and_then(Value::as_i64) != Some(2) {
        return Err(invalid());
    }
    let tags = filter
        .get("tags")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = tag.as_str().ok_or_else(invalid)?;
        if seen.insert(tag.to_owned()) {
            normalized.push(tag);
        }
    }
    if let Some(levels) = filter.get("effortLevels") {
        for level in levels.as_array().ok_or_else(invalid)? {
            let level = level
                .as_str()
                .filter(|level| matches!(*level, "fast" | "medium" | "long"))
                .ok_or_else(invalid)?;
            if level != "fast" && seen.insert(level.to_owned()) {
                normalized.push(level);
            }
        }
    }
    let filter = json!({"version":2,"tags":normalized});
    lock_hot(tx, workspace).await?;
    let previous=sqlx::query("SELECT client_updated_at,last_modified_by_replica_id,last_operation_id FROM content.decks WHERE workspace_id=$1 AND deck_id=$2 FOR UPDATE").bind(workspace).bind(deck).fetch_optional(&mut **tx).await?;
    if let Some(row) = previous {
        let stored = (
            row.try_get::<DateTime<Utc>, _>("client_updated_at")?,
            row.try_get::<Uuid, _>("last_modified_by_replica_id")?
                .to_string(),
            row.try_get::<String, _>("last_operation_id")?,
        );
        if (
            mutation.client_updated_at,
            mutation.replica_id.to_string(),
            mutation.operation_id.as_str(),
        ) <= (stored.0, stored.1, stored.2.as_str())
        {
            return Ok((false, latest_hot(tx, workspace, "deck", deck).await?));
        }
    } else {
        let conflict: Option<Uuid> = sqlx::query_scalar(
            "SELECT workspace_id FROM sync.find_conflicting_workspace_id('deck',$1) LIMIT 1",
        )
        .bind(deck.to_string())
        .fetch_optional(&mut **tx)
        .await?;
        if conflict.is_some_and(|id| id != workspace) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "SYNC_ENTITY_WORKSPACE_CONFLICT",
                "Deck identity belongs to another workspace",
            ));
        }
    }
    sqlx::query("INSERT INTO content.decks(deck_id,workspace_id,name,filter_definition,created_at,deleted_at,client_updated_at,last_modified_by_replica_id,last_operation_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(deck_id) DO UPDATE SET name=EXCLUDED.name,filter_definition=EXCLUDED.filter_definition,deleted_at=EXCLUDED.deleted_at,client_updated_at=EXCLUDED.client_updated_at,last_modified_by_replica_id=EXCLUDED.last_modified_by_replica_id,last_operation_id=EXCLUDED.last_operation_id,updated_at=now() WHERE content.decks.workspace_id=EXCLUDED.workspace_id").bind(deck).bind(workspace).bind(name).bind(filter).bind(created).bind(deleted).bind(mutation.client_updated_at).bind(mutation.replica_id).bind(&mutation.operation_id).execute(&mut **tx).await?;
    Ok((
        true,
        Some(record_hot(tx, workspace, "deck", deck, mutation).await?),
    ))
}

async fn mutate_settings(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    payload: Value,
    mutation: &Mutation,
) -> Result<(bool, Option<i64>), ApiError> {
    let config: SchedulerConfig = parse(&payload)?;
    config.validate()?;
    lock_hot(tx, workspace).await?;
    let row=sqlx::query("SELECT fsrs_client_updated_at,fsrs_last_modified_by_replica_id,fsrs_last_operation_id FROM org.workspaces WHERE workspace_id=$1 FOR UPDATE").bind(workspace).fetch_one(&mut **tx).await?;
    let stored = (
        row.try_get::<DateTime<Utc>, _>("fsrs_client_updated_at")?,
        row.try_get::<Uuid, _>("fsrs_last_modified_by_replica_id")?
            .to_string(),
        row.try_get::<String, _>("fsrs_last_operation_id")?,
    );
    if (
        mutation.client_updated_at,
        mutation.replica_id.to_string(),
        mutation.operation_id.as_str(),
    ) <= (stored.0, stored.1, stored.2.as_str())
    {
        return Ok((
            false,
            latest_hot(tx, workspace, "workspace_scheduler_settings", workspace).await?,
        ));
    }
    sqlx::query("UPDATE org.workspaces SET fsrs_desired_retention=$2,fsrs_learning_steps_minutes=$3,fsrs_relearning_steps_minutes=$4,fsrs_maximum_interval_days=$5,fsrs_enable_fuzz=$6,fsrs_client_updated_at=$7,fsrs_last_modified_by_replica_id=$8,fsrs_last_operation_id=$9,fsrs_updated_at=now() WHERE workspace_id=$1").bind(workspace).bind(config.desired_retention).bind(json!(config.learning_steps_minutes)).bind(json!(config.relearning_steps_minutes)).bind(config.maximum_interval_days).bind(config.enable_fuzz).bind(mutation.client_updated_at).bind(mutation.replica_id).bind(&mutation.operation_id).execute(&mut **tx).await?;
    Ok((
        true,
        Some(
            record_hot(
                tx,
                workspace,
                "workspace_scheduler_settings",
                workspace,
                mutation,
            )
            .await?,
        ),
    ))
}

async fn prepare_card(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    payload: &Value,
) -> Result<CardSnapshot, ApiError> {
    let mut snapshot: CardSnapshot = parse(payload)?;
    if !payload
        .as_object()
        .ok_or_else(invalid)?
        .contains_key("metadata")
    {
        let existing = cards::card_in_tx(tx, workspace, snapshot.card_id).await?;
        snapshot.metadata = existing.map_or_else(|| json!({"version":1,"source":{"label":null,"author":null,"comment":null,"createdAt":snapshot.created_at.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"importedAt":null,"importId":null}}),|card|card.snapshot.metadata);
    }
    if !payload
        .as_object()
        .ok_or_else(invalid)?
        .contains_key("cardType")
        && let Some(existing) = cards::card_in_tx(tx, workspace, snapshot.card_id).await?
    {
        snapshot.card_type = existing.snapshot.card_type;
    }
    if let Some(effort) = payload.get("effortLevel").and_then(Value::as_str)
        && matches!(effort, "medium" | "long")
        && !snapshot.tags.iter().any(|tag| tag == effort)
    {
        snapshot.tags.push(effort.into());
    }
    Ok(snapshot)
}

#[allow(
    clippy::too_many_lines,
    reason = "Operation validation, its idempotency gate, the typed write and collected facts share one ordered atomic branch."
)]
async fn apply_operation(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    replica: Uuid,
    operation: &Operation,
    facts: &mut super::facts::Buffer,
) -> Result<Value, ApiError> {
    let base = |status: &str, change: Option<i64>, error: Option<&str>| json!({"operationId":operation.operation_id,"entityType":operation.entity_type,"entityId":operation.entity_id,"status":status,"resultingHotChangeId":change,"error":error});
    if operation.operation_id.is_empty()
        || !matches!(
            operation.entity_type.as_str(),
            "card" | "deck" | "workspace_scheduler_settings" | "review_event" | "media_asset"
        )
        || operation.action
            != if operation.entity_type == "review_event" {
                "append"
            } else {
                "upsert"
            }
    {
        return Err(invalid());
    }
    let existing:Option<Option<i64>>=sqlx::query_scalar("SELECT resulting_hot_change_id FROM sync.applied_operations_current WHERE workspace_id=$1 AND replica_id=$2 AND operation_id=$3 ORDER BY applied_at DESC LIMIT 1").bind(workspace).bind(replica).bind(&operation.operation_id).fetch_optional(&mut **tx).await?;
    if let Some(change) = existing {
        return Ok(base("duplicate", change, None));
    }
    let mutation = Mutation {
        client_updated_at: operation.client_updated_at,
        replica_id: replica,
        operation_id: operation.operation_id.clone(),
    };
    let identity_key = match operation.entity_type.as_str() {
        "card" => "cardId",
        "deck" => "deckId",
        "review_event" => "reviewEventId",
        "media_asset" => "mediaAssetId",
        _ => "",
    };
    if (!identity_key.is_empty() && id(&operation.payload, identity_key)? != operation.entity_id)
        || (operation.entity_type == "workspace_scheduler_settings"
            && operation.entity_id != workspace)
    {
        return Ok(base(
            "rejected",
            None,
            Some("entityId must match the payload identity"),
        ));
    }
    let before =
        super::facts::content(tx, workspace, &operation.entity_type, operation.entity_id).await?;
    let (applied, change) = match operation.entity_type.as_str() {
        "card" => {
            let snapshot = prepare_card(tx, workspace, &operation.payload).await?;
            let (_, applied, change) =
                cards::mutate_card_in_tx(tx, workspace, snapshot, &mutation).await?;
            (applied, change)
        }
        "deck" => mutate_deck(tx, workspace, &operation.payload, &mutation).await?,
        "workspace_scheduler_settings" => {
            mutate_settings(tx, workspace, operation.payload.clone(), &mutation).await?
        }
        "media_asset" => {
            return Ok(base(
                "rejected",
                None,
                Some(
                    "media_asset sync writes are not accepted; use the media upload API to create or update media assets.",
                ),
            ));
        }
        _ => {
            if date(&operation.payload, "reviewedAtClient")? != operation.client_updated_at {
                return Err(invalid());
            }
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM content.cards WHERE workspace_id=$1 AND card_id=$2)",
            )
            .bind(workspace)
            .bind(id(&operation.payload, "cardId")?)
            .fetch_one(&mut **tx)
            .await?;
            if !exists {
                return Ok(base(
                    "rejected",
                    None,
                    Some("review_event payload.cardId must reference an existing card"),
                ));
            }
            (
                append_review(tx, user, workspace, replica, &operation.payload).await?,
                None,
            )
        }
    };
    if applied {
        if let Some(after) =
            super::facts::content(tx, workspace, &operation.entity_type, operation.entity_id)
                .await?
        {
            facts.content(
                &operation.entity_type,
                operation.entity_id,
                before.as_ref(),
                &after,
                &mutation,
            );
        }
        if operation.entity_type == "review_event" {
            facts.review(tx, operation.entity_id, false).await?;
        }
    }
    sqlx::query("INSERT INTO sync.applied_operations_current(workspace_id,replica_id,operation_id,operation_type,entity_type,entity_id,client_updated_at,resulting_hot_change_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(workspace).bind(replica).bind(&operation.operation_id).bind(&operation.action).bind(&operation.entity_type).bind(operation.entity_id.to_string()).bind(operation.client_updated_at).bind(change).execute(&mut **tx).await?;
    Ok(base(
        if applied { "applied" } else { "ignored" },
        change,
        None,
    ))
}

pub(super) async fn push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<Push>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&workspace.to_string())).await?;
    let replica = ensure_client(&mut tx, &user, workspace, &input.client).await?;
    lock_hot(&mut tx, workspace).await?;
    let mut facts = super::facts::Buffer::default();
    let mut operations = Vec::with_capacity(input.operations.len());
    for operation in &input.operations {
        operations.push(
            apply_operation(&mut tx, &user, workspace, replica, operation, &mut facts).await?,
        );
    }
    tx.commit().await?;
    facts
        .emit(
            &state,
            user.parse().map_err(|_| ApiError::internal())?,
            workspace,
            None,
        )
        .await;
    Ok(Json(json!({"operations":operations})))
}

async fn max_hot(tx: &mut Transaction<'_, Postgres>, workspace: Uuid) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "SELECT COALESCE(max(change_id),0) FROM sync.hot_changes WHERE workspace_id=$1",
    )
    .bind(workspace)
    .fetch_one(&mut **tx)
    .await
    .map_err(Into::into)
}

async fn entities(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    media: bool,
) -> Result<Vec<(i32, String, String, Value)>, ApiError> {
    let rows = sqlx::query(include_str!("entities.sql"))
        .bind(workspace)
        .bind(media)
        .fetch_all(&mut **tx)
        .await?;
    rows.iter()
        .map(|row| {
            Ok((
                row.try_get("entity_rank")?,
                row.try_get("entity_type")?,
                row.try_get("entity_id")?,
                row.try_get("payload")?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(Into::into)
}
fn entry(kind: &str, id: &str, payload: &Value) -> Value {
    json!({"entityType":kind,"entityId":id,"action":"upsert","payload":payload})
}

pub(super) async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<Pull>,
) -> Result<Json<Value>, ApiError> {
    if input.after_hot_change_id < 0 || !(1..=500).contains(&input.limit) {
        return Err(invalid());
    }
    let identity = require_mutation(&state, &headers).await?;
    let user = identity.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, Some(&workspace.to_string())).await?;
    ensure_client(&mut tx, &user, workspace, &input.client).await?;
    lock_hot(&mut tx, workspace).await?;
    let floor:i64=sqlx::query_scalar("SELECT min_available_hot_change_id FROM sync.workspace_sync_metadata WHERE workspace_id=$1").bind(workspace).fetch_one(&mut *tx).await?;
    if input.after_hot_change_id > 0 && input.after_hot_change_id < floor {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "SYNC_BOOTSTRAP_REQUIRED",
            "Cloud sync requires a fresh bootstrap.",
        ));
    }
    let rows=sqlx::query("SELECT change_id,entity_type,entity_id FROM (SELECT DISTINCT ON(entity_type,entity_id) change_id,entity_type,entity_id FROM sync.hot_changes WHERE workspace_id=$1 AND change_id>$2 AND ($4 OR entity_type<>'media_asset') ORDER BY entity_type,entity_id,change_id DESC) latest ORDER BY change_id LIMIT $3").bind(workspace).bind(input.after_hot_change_id).bind(input.limit.checked_add(1).ok_or_else(ApiError::internal)?).bind(input.include_media_assets).fetch_all(&mut *tx).await?;
    let limit = usize::try_from(input.limit).map_err(|_| invalid())?;
    let more = rows.len() > limit;
    let all = entities(&mut tx, workspace, input.include_media_assets).await?;
    let mut changes = Vec::new();
    let mut next = input.after_hot_change_id;
    for row in rows.iter().take(limit) {
        let kind: String = row.try_get("entity_type")?;
        let id: String = row.try_get("entity_id")?;
        let change: i64 = row.try_get("change_id")?;
        let payload = all
            .iter()
            .find(|(_, k, i, _)| *k == kind && *i == id)
            .map(|(_, _, _, p)| p.clone())
            .ok_or_else(ApiError::internal)?;
        let mut value = entry(&kind, &id, &payload);
        value
            .as_object_mut()
            .ok_or_else(ApiError::internal)?
            .insert("changeId".into(), json!(change));
        changes.push(value);
        next = change;
    }
    if !more && !input.include_media_assets {
        next = next.max(max_hot(&mut tx, workspace).await?);
    }
    tx.commit().await?;
    let mut result = json!({"changes":changes,"nextHotChangeId":next,"hasMore":more});
    match crate::ai::entitlement(&state, identity.user_id).await {
        Ok(entitlement) => {
            result
                .as_object_mut()
                .ok_or_else(ApiError::internal)?
                .insert("entitlement".into(), entitlement);
        }
        Err(error) => {
            tracing::warn!(code=error.code,user_id=%identity.user_id,workspace_id=%workspace,"Sync pull entitlement resolution failed");
        }
    }
    Ok(Json(result))
}

async fn remote_empty(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    media: bool,
) -> Result<bool, ApiError> {
    sqlx::query_scalar("SELECT NOT (EXISTS(SELECT 1 FROM content.cards WHERE workspace_id=$1) OR EXISTS(SELECT 1 FROM content.decks WHERE workspace_id=$1) OR EXISTS(SELECT 1 FROM content.review_events WHERE workspace_id=$1) OR ($2 AND EXISTS(SELECT 1 FROM content.media_assets WHERE workspace_id=$1)))").bind(workspace).bind(media).fetch_one(&mut **tx).await.map_err(Into::into)
}

async fn bootstrap_push(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Uuid,
    replica: Uuid,
    body: &Value,
    media: bool,
    facts: &mut super::facts::Buffer,
) -> Result<Json<Value>, ApiError> {
    let entries = body
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let media = media
        || entries
            .iter()
            .any(|e| e.get("entityType").and_then(Value::as_str) == Some("media_asset"));
    if !remote_empty(tx, workspace, media).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "SYNC_BOOTSTRAP_NOT_EMPTY",
            "Cloud bootstrap requires an empty remote workspace",
        ));
    }
    for value in entries {
        let payload = value.get("payload").ok_or_else(invalid)?;
        let operation = Operation {
            operation_id: text(payload, "lastOperationId")?.into(),
            entity_id: id(value, "entityId")?,
            client_updated_at: date(payload, "clientUpdatedAt")?,
            entity_type: text(value, "entityType")?.into(),
            action: text(value, "action")?.into(),
            payload: payload.clone(),
        };
        if operation.entity_type == "media_asset" {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "SYNC_MEDIA_ASSET_WRITE_REJECTED",
                "media_asset sync writes are not accepted; use the media upload API to create or update media assets.",
            ));
        }
        let result = apply_operation(tx, user, workspace, replica, &operation, facts).await?;
        if result.get("status").and_then(Value::as_str) == Some("rejected") {
            return Err(invalid());
        }
    }
    let max = max_hot(tx, workspace).await?;
    Ok(Json(
        json!({"mode":"push","appliedEntriesCount":entries.len(),"bootstrapHotChangeId":max}),
    ))
}

pub(super) async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let client: Client = parse(&body)?;
    let direction = text(&body, "mode")?;
    let media = body
        .get("includeMediaAssets")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&workspace.to_string())).await?;
    let replica = ensure_client(&mut tx, &user, workspace, &client).await?;
    lock_hot(&mut tx, workspace).await?;
    if direction == "push" {
        let mut facts = super::facts::Buffer::default();
        let result =
            bootstrap_push(&mut tx, &user, workspace, replica, &body, media, &mut facts).await?;
        tx.commit().await?;
        facts
            .emit(
                &state,
                user.parse().map_err(|_| ApiError::internal())?,
                workspace,
                None,
            )
            .await;
        return Ok(result);
    }
    if direction != "pull" {
        return Err(invalid());
    }
    let limit = body
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|v| (1..=1000).contains(v))
        .ok_or_else(invalid)?;
    let limit = usize::try_from(limit).map_err(|_| invalid())?;
    let (max, rank, after) = if let Some(cursor) = body.get("cursor").and_then(Value::as_str) {
        let values = decode_cursor(cursor)?;
        let mut values = values.iter();
        (
            values.next().and_then(Value::as_i64).ok_or_else(invalid)?,
            values.next().and_then(Value::as_i64).ok_or_else(invalid)?,
            values
                .next()
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
                .to_owned(),
        )
    } else {
        (max_hot(&mut tx, workspace).await?, -1, String::new())
    };
    let empty = remote_empty(&mut tx, workspace, media).await?;
    let all = entities(&mut tx, workspace, media).await?;
    let visible: Vec<_> = all
        .into_iter()
        .filter(|(r, _, id, _)| i64::from(*r) > rank || i64::from(*r) == rank && *id > after)
        .collect();
    let more = visible.len() > limit;
    let cursor = if more {
        visible
            .iter()
            .take(limit)
            .next_back()
            .map(|(r, _, id, _)| encode_cursor(&[json!(max), json!(r), json!(id)]))
    } else {
        None
    };
    let entries: Vec<_> = visible
        .into_iter()
        .take(limit)
        .map(|(_, kind, id, payload)| entry(&kind, &id, &payload))
        .collect();
    tx.commit().await?;
    Ok(Json(
        json!({"mode":"pull","entries":entries,"nextCursor":cursor,"hasMore":more,"bootstrapHotChangeId":max,"remoteIsEmpty":empty}),
    ))
}

pub(super) async fn history_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<History>,
) -> Result<Json<Value>, ApiError> {
    if input.after_review_sequence_id < 0 || !(1..=500).contains(&input.limit) {
        return Err(invalid());
    }
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&workspace.to_string())).await?;
    ensure_client(&mut tx, &user, workspace, &input.client).await?;
    let rows=sqlx::query("SELECT review_sequence,jsonb_strip_nulls(jsonb_build_object('reviewEventId',review_event_id,'workspaceId',workspace_id,'cardId',card_id,'replicaId',replica_id,'clientEventId',client_event_id,'rating',rating,'reviewedAtClient',to_char(reviewed_at_client AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),'reviewedAtServer',to_char(reviewed_at_server AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'),'reviewedTimeZone',reviewed_time_zone)) AS payload FROM content.review_events WHERE workspace_id=$1 AND review_sequence>$2 ORDER BY review_sequence LIMIT $3").bind(workspace).bind(input.after_review_sequence_id).bind(input.limit.checked_add(1).ok_or_else(ApiError::internal)?).fetch_all(&mut *tx).await?;
    let limit = usize::try_from(input.limit).map_err(|_| invalid())?;
    let more = rows.len() > limit;
    let mut next = input.after_review_sequence_id;
    let mut events = Vec::new();
    for row in rows.iter().take(limit) {
        next = row.try_get("review_sequence")?;
        events.push(row.try_get::<Value, _>("payload")?);
    }
    tx.commit().await?;
    Ok(Json(
        json!({"reviewEvents":events,"nextReviewSequenceId":next,"hasMore":more}),
    ))
}

pub(super) async fn history_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<HistoryImport>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, Some(&workspace.to_string())).await?;
    let replica = ensure_client(&mut tx, &user, workspace, &input.client).await?;
    let mut facts = super::facts::Buffer::default();
    let mut imported = 0_usize;
    let mut duplicate = 0_usize;
    for event in &input.review_events {
        if id(event, "workspaceId")? != workspace {
            return Err(invalid());
        }
        if append_review(&mut tx, &user, workspace, replica, event).await? {
            facts
                .review(&mut tx, id(event, "reviewEventId")?, true)
                .await?;
            imported = imported.saturating_add(1);
        } else {
            duplicate = duplicate.saturating_add(1);
        }
    }
    let next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(review_sequence),0) FROM content.review_events WHERE workspace_id=$1",
    )
    .bind(workspace)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    facts
        .emit(
            &state,
            user.parse().map_err(|_| ApiError::internal())?,
            workspace,
            None,
        )
        .await;
    Ok(Json(
        json!({"importedCount":imported,"duplicateCount":duplicate,"nextReviewSequenceId":next}),
    ))
}
