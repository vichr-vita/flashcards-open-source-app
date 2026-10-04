use super::{CatalogCard, CatalogMedia, Input, counts, error, invalid, stamp};
use crate::core::cards::js_space;
use crate::{
    core::{CardSnapshot, Mutation, mutate_card_in_tx, sync::record_hot},
    error::ApiError,
};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

fn conflict(code: &str, message: &str) -> ApiError {
    error(StatusCode::CONFLICT, code, message)
}

pub(super) async fn assert_available(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    input: &Input,
    cards: &[CatalogCard],
    media: &[CatalogMedia],
) -> Result<(), ApiError> {
    let replica:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sync.workspace_replicas WHERE workspace_id=$1 AND replica_id=$2)").bind(workspace).bind(input.last_modified_by_replica_id).fetch_one(&mut **tx).await?;
    if !replica {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "CATALOG_PACKAGE_INSTALL_REPLICA_INVALID",
            "lastModifiedByReplicaId must reference a workspace replica for this workspace.",
        ));
    }
    let used:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM content.cards WHERE workspace_id=$1 AND metadata->'source'->>'importId'=$2)").bind(workspace).bind(&input.install_id).fetch_one(&mut **tx).await?;
    if used {
        return Err(conflict(
            "CATALOG_PACKAGE_INSTALL_ID_ALREADY_EXISTS",
            "Catalog package install id already exists in this workspace.",
        ));
    }
    let operations: Vec<String> = (0..cards.len())
        .map(|i| format!("{}:card:{i}", input.operation_id_prefix))
        .chain((0..media.len()).map(|i| format!("{}:media:{i}", input.operation_id_prefix)))
        .collect();
    if operations.iter().any(|id| id.len() > 1024) {
        return Err(invalid());
    }
    let used:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM content.cards WHERE workspace_id=$1 AND last_operation_id=ANY($2::text[]) UNION ALL SELECT 1 FROM content.media_assets WHERE workspace_id=$1 AND last_operation_id=ANY($2::text[]))").bind(workspace).bind(&operations).fetch_one(&mut **tx).await?;
    if used {
        return Err(conflict(
            "CATALOG_PACKAGE_INSTALL_OPERATION_ALREADY_EXISTS",
            "Catalog package install operation id already exists in this workspace.",
        ));
    }
    Ok(())
}

fn metadata(card: &CatalogCard, package: &Value, input: &Input) -> Result<Value, ApiError> {
    let source = card.metadata.get("source").ok_or_else(|| {
        conflict(
            "CATALOG_PACKAGE_CARD_METADATA_INVALID",
            "Catalog package card metadata is invalid.",
        )
    })?;
    if card.metadata.get("version").and_then(Value::as_u64) != Some(1)
        || !(source.is_null() || source.is_object())
    {
        return Err(conflict(
            "CATALOG_PACKAGE_CARD_METADATA_INVALID",
            "Catalog package card metadata is invalid.",
        ));
    }
    if source.is_object() {
        for key in [
            "label",
            "author",
            "comment",
            "createdAt",
            "importedAt",
            "importId",
        ] {
            if source
                .get(key)
                .is_none_or(|value| !(value.is_null() || value.is_string()))
            {
                return Err(conflict(
                    "CATALOG_PACKAGE_CARD_METADATA_INVALID",
                    "Catalog package card source is invalid.",
                ));
            }
        }
    }
    let mut values = serde_json::Map::new();
    for (key, fallback) in [
        ("label", &package["title"]),
        (
            "author",
            package
                .pointer("/author/displayName")
                .ok_or_else(ApiError::internal)?,
        ),
        ("comment", &package["summary"]),
    ] {
        let value = source.get(key).filter(|v| !v.is_null()).unwrap_or(fallback);
        if !value.is_string() {
            return Err(conflict(
                "CATALOG_PACKAGE_CARD_METADATA_INVALID",
                "Catalog package card source is invalid.",
            ));
        }
        values.insert(key.into(), value.clone());
    }
    let created = source
        .get("createdAt")
        .and_then(Value::as_str)
        .or_else(|| package["publishedAt"].as_str())
        .or_else(|| package["createdAt"].as_str())
        .ok_or_else(ApiError::internal)?;
    let created = created.parse::<DateTime<Utc>>().map_err(|_| {
        conflict(
            "CATALOG_PACKAGE_CARD_METADATA_INVALID",
            "Catalog package source createdAt is invalid.",
        )
    })?;
    values.insert("createdAt".into(), json!(stamp(created)));
    values.insert("importedAt".into(), json!(stamp(input.installed_at)));
    values.insert("importId".into(), json!(input.install_id));
    Ok(json!({"version":1,"source":values}))
}

async fn install_media(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    input: &Input,
    assets: &[CatalogMedia],
) -> Result<(Vec<Value>, BTreeMap<String, Uuid>), ApiError> {
    let mut result = Vec::new();
    let mut map = BTreeMap::new();
    for (index, asset) in assets.iter().enumerate() {
        let id = Uuid::new_v4();
        let mutation = Mutation {
            client_updated_at: input.client_updated_at,
            replica_id: input.last_modified_by_replica_id,
            operation_id: format!("{}:media:{index}", input.operation_id_prefix),
        };
        sqlx::query("INSERT INTO content.media_assets(media_asset_id,workspace_id,media_blob_id,source_url,created_at,client_updated_at,last_modified_by_replica_id,last_operation_id) VALUES($1,$2,$3,NULL,$4,$5,$6,$7)")
            .bind(id).bind(workspace).bind(asset.media_blob_id).bind(input.installed_at).bind(input.client_updated_at).bind(mutation.replica_id).bind(&mutation.operation_id).execute(&mut **tx).await?;
        record_hot(tx, workspace, "media_asset", id, &mutation).await?;
        result.push(json!({"packageMediaAssetId":asset.package_media_asset_id,"packageMediaKey":asset.package_media_key,"mediaAssetId":id}));
        map.insert(asset.package_media_key.clone(), id);
    }
    Ok((result, map))
}

fn snapshot(
    card: &CatalogCard,
    package: &Value,
    input: &Input,
    created_at: DateTime<Utc>,
    media: &BTreeMap<String, Uuid>,
) -> Result<CardSnapshot, ApiError> {
    if card
        .media_asset_keys
        .iter()
        .any(|key| !media.contains_key(key))
    {
        return Err(conflict(
            "CATALOG_PACKAGE_INSTALL_MEDIA_ASSET_NOT_FOUND",
            "Catalog package card references missing package media asset keys.",
        ));
    }
    let mut tags = Vec::new();
    let mut seen = BTreeSet::new();
    for tag in card
        .tags
        .iter()
        .map(|v| v.trim_matches(js_space))
        .filter(|v| !input.remove_tags.iter().any(|r| r == v))
        .chain(input.add_import_tag.then_some(input.import_tag.as_str()))
    {
        if seen.insert(tag.to_owned()) {
            tags.push(tag.into());
        }
    }
    Ok(CardSnapshot {
        card_id: Uuid::new_v4(),
        front_text: super::super::markdown::rewrite(&card.front_text, media)?,
        back_text: super::super::markdown::rewrite(&card.back_text, media)?,
        card_type: card.card_type.clone(),
        metadata: metadata(card, package, input)?,
        tags,
        created_at,
        due_at: None,
        reps: 0,
        lapses: 0,
        fsrs_card_state: "new".into(),
        fsrs_step_index: None,
        fsrs_stability: None,
        fsrs_difficulty: None,
        fsrs_last_reviewed_at: None,
        fsrs_scheduled_days: None,
        deleted_at: None,
    })
}

pub(super) async fn install(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    package: Value,
    input: &Input,
    cards: &[CatalogCard],
    media: &[CatalogMedia],
) -> Result<Value, ApiError> {
    let counts = counts(cards)?;
    if input
        .remove_tags
        .iter()
        .any(|removed| !counts.iter().any(|(tag, _)| tag == removed))
    {
        return Err(invalid());
    }
    let (installed_media, map) = install_media(tx, workspace, input, media).await?;
    let mut installed_cards = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let offset = cards
            .len()
            .checked_sub(index)
            .and_then(|v| v.checked_sub(1))
            .and_then(|v| i64::try_from(v).ok())
            .ok_or_else(ApiError::internal)?;
        let created_at = input
            .installed_at
            .checked_sub_signed(Duration::milliseconds(offset))
            .ok_or_else(invalid)?;
        let snapshot = snapshot(card, &package, input, created_at, &map)?;
        let id = snapshot.card_id;
        let mutation = Mutation {
            client_updated_at: input.client_updated_at,
            replica_id: input.last_modified_by_replica_id,
            operation_id: format!("{}:card:{index}", input.operation_id_prefix),
        };
        mutate_card_in_tx(tx, workspace, snapshot, &mutation).await?;
        installed_cards.push(json!({"packageCardId":card.package_card_id,"stableCardKey":card.stable_card_key,"ordinal":card.ordinal,"cardId":id}));
    }
    Ok(
        json!({"packageVersion":package,"installedCards":installed_cards,"installedMediaAssets":installed_media,
        "summary":{"cardCount":cards.len(),"mediaAssetCount":media.len(),"installId":input.install_id,"installedAt":stamp(input.installed_at),
            "keptTagCount":counts.len().checked_sub(input.remove_tags.len()).ok_or_else(invalid)?,"removedTagCount":input.remove_tags.len(),"importTag":input.add_import_tag.then_some(&input.import_tag)}}),
    )
}
