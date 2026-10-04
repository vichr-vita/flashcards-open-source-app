use super::{Input, error, stamp};
use crate::error::ApiError;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;
mod shape;

fn invalid_stored() -> ApiError {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "CATALOG_PACKAGE_INSTALL_STORED_RESULT_INVALID",
        "Stored catalog package install result cannot be replayed safely. Repair its durable record before retrying.",
    )
}

pub(super) async fn load(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    version: Uuid,
    input: &Input,
) -> Result<Option<Value>, ApiError> {
    let row=sqlx::query("SELECT * FROM sync.catalog_package_install_idempotency WHERE workspace_id=$1 AND install_id=$2").bind(workspace).bind(&input.install_id).fetch_optional(&mut **tx).await?;
    let Some(row) = row else { return Ok(None) };
    let import = input.add_import_tag.then_some(input.import_tag.as_str());
    if row.try_get::<Uuid, _>("package_version_id")? != version
        || row.try_get::<DateTime<Utc>, _>("installed_at")? != input.installed_at
        || row.try_get::<DateTime<Utc>, _>("client_updated_at")? != input.client_updated_at
        || row.try_get::<Uuid, _>("last_modified_by_replica_id")?
            != input.last_modified_by_replica_id
        || row.try_get::<String, _>("operation_id_prefix")? != input.operation_id_prefix
        || row.try_get::<bool, _>("add_import_tag")? != input.add_import_tag
        || row.try_get::<Option<String>, _>("import_tag")?.as_deref() != import
        || row.try_get::<Vec<String>, _>("remove_tags")? != input.remove_tags
    {
        return Err(error(
            StatusCode::CONFLICT,
            "CATALOG_PACKAGE_INSTALL_IDEMPOTENCY_CONFLICT",
            "Catalog package install idempotency key was already used with a different normalized request. Use a new installId for an explicit repeat import.",
        ));
    }
    let result: Value = row.try_get("install_result")?;
    shape::validate(&result)?;
    let cards = result
        .get("installedCards")
        .and_then(Value::as_array)
        .ok_or_else(invalid_stored)?;
    let media = result
        .get("installedMediaAssets")
        .and_then(Value::as_array)
        .ok_or_else(invalid_stored)?;
    let summary = result.get("summary").ok_or_else(invalid_stored)?;
    if summary["installId"].as_str() != Some(input.install_id.as_str())
        || summary["installedAt"].as_str() != Some(stamp(input.installed_at).as_str())
        || summary["importTag"].as_str() != import
        || result
            .pointer("/packageVersion/packageVersionId")
            .and_then(Value::as_str)
            != Some(version.to_string().as_str())
        || summary["cardCount"].as_u64() != u64::try_from(cards.len()).ok()
        || result
            .pointer("/packageVersion/cardCount")
            .and_then(Value::as_u64)
            != u64::try_from(cards.len()).ok()
        || summary["mediaAssetCount"].as_u64() != u64::try_from(media.len()).ok()
        || summary["removedTagCount"].as_u64() != u64::try_from(input.remove_tags.len()).ok()
    {
        return Err(invalid_stored());
    }
    for card in cards {
        for key in ["packageCardId", "cardId"] {
            card.get(key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<Uuid>().ok())
                .ok_or_else(invalid_stored)?;
        }
        if card["ordinal"].as_u64().is_none_or(|v| v == 0) || !card["stableCardKey"].is_string() {
            return Err(invalid_stored());
        }
    }
    for asset in media {
        for key in ["packageMediaAssetId", "mediaAssetId"] {
            asset
                .get(key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<Uuid>().ok())
                .ok_or_else(invalid_stored)?;
        }
        if !asset["packageMediaKey"].is_string() {
            return Err(invalid_stored());
        }
    }
    Ok(Some(result))
}
pub(super) async fn store(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    version: Uuid,
    input: &Input,
    result: &Value,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO sync.catalog_package_install_idempotency(workspace_id,install_id,package_version_id,installed_at,client_updated_at,last_modified_by_replica_id,operation_id_prefix,add_import_tag,import_tag,remove_tags,install_result) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
        .bind(workspace).bind(&input.install_id).bind(version).bind(input.installed_at).bind(input.client_updated_at)
        .bind(input.last_modified_by_replica_id).bind(&input.operation_id_prefix).bind(input.add_import_tag)
        .bind(input.add_import_tag.then_some(&input.import_tag)).bind(&input.remove_tags).bind(result).execute(&mut **tx).await?;
    Ok(())
}
