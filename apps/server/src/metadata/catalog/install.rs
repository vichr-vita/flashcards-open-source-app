mod persistence;
mod replay;

use super::{error, invalid, load_version, stamp, version_value};
use crate::core::cards::js_space;
use crate::{
    AppState, auth::require_mutation, core::workspaces::assert_access, database::scoped,
    error::ApiError,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, SubsecRound, Utc};
use icu_collator::{Collator, options::CollatorOptions};
use icu_locale::locale;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Input {
    install_id: String,
    installed_at: DateTime<Utc>,
    client_updated_at: DateTime<Utc>,
    last_modified_by_replica_id: Uuid,
    operation_id_prefix: String,
    #[serde(default)]
    #[serde(rename = "installJourneyId")]
    _install_journey_id: Option<Uuid>,
    #[serde(default)]
    add_import_tag: bool,
    #[serde(default)]
    import_tag: String,
    #[serde(default)]
    remove_tags: Vec<String>,
}
impl Input {
    fn normalize(mut self) -> Result<Self, ApiError> {
        self.install_id = self.install_id.trim_matches(js_space).into();
        self.import_tag = self.import_tag.trim_matches(js_space).into();
        self.installed_at = self.installed_at.trunc_subsecs(3);
        self.client_updated_at = self.client_updated_at.trunc_subsecs(3);
        if self.install_id.is_empty()
            || self.install_id.encode_utf16().count() > 128
            || self.operation_id_prefix.is_empty()
            || self.operation_id_prefix.len() > 1007
            || self.operation_id_prefix.trim_matches(js_space) != self.operation_id_prefix
            || !self
                .operation_id_prefix
                .bytes()
                .all(|b| (32..=126).contains(&b))
            || self.add_import_tag && self.import_tag.is_empty()
        {
            return Err(invalid());
        }
        let mut seen = BTreeSet::new();
        for tag in &mut self.remove_tags {
            *tag = tag.trim_matches(js_space).to_owned();
            if tag.is_empty() || !seen.insert(tag.clone()) {
                return Err(invalid());
            }
        }
        self.remove_tags
            .sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        Ok(self)
    }
}

#[derive(FromRow)]
pub(super) struct CatalogCard {
    package_card_id: Uuid,
    stable_card_key: String,
    ordinal: i32,
    front_text: String,
    back_text: String,
    card_type: String,
    metadata: Value,
    tags: Vec<String>,
    media_asset_keys: Vec<String>,
}
#[derive(FromRow)]
pub(super) struct CatalogMedia {
    package_media_asset_id: Uuid,
    package_media_key: String,
    media_blob_id: Uuid,
}

async fn cards(
    tx: &mut Transaction<'_, Postgres>,
    version: Uuid,
) -> Result<Vec<CatalogCard>, ApiError> {
    Ok(sqlx::query_as("SELECT package_card_id,stable_card_key,ordinal,front_text,back_text,card_type,metadata,tags,media_asset_keys FROM catalog.package_cards WHERE package_version_id=$1 ORDER BY ordinal,package_card_id").bind(version).fetch_all(&mut **tx).await?)
}
fn counts(cards: &[CatalogCard]) -> Result<Vec<(String, usize)>, ApiError> {
    let mut counts = BTreeMap::<String, usize>::new();
    for card in cards {
        for tag in &card.tags {
            let tag = tag.trim_matches(js_space);
            if tag.is_empty() {
                return Err(invalid());
            }
            let count = counts.entry(tag.into()).or_default();
            *count = count.checked_add(1).ok_or_else(ApiError::internal)?;
        }
    }
    let mut result: Vec<_> = counts.into_iter().collect();
    let collator = Collator::try_new(locale!("en-US").into(), CollatorOptions::default())
        .map_err(|_| ApiError::internal())?;
    result.sort_by(|(lt, lc), (rt, rc)| {
        rc.cmp(lc)
            .then_with(|| collator.compare(&lt.to_lowercase(), &rt.to_lowercase()))
            .then_with(|| collator.compare(lt, rt))
    });
    Ok(result)
}

pub(super) async fn preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((workspace, version)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers).await?;
    let mut tx = scoped(
        &state.pool,
        &user.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    assert_access(&mut tx, workspace).await?;
    let row = load_version(&mut tx, version, false).await?;
    let package = version_value(&row)?;
    let cards = cards(&mut tx, version).await?;
    let counts = counts(&cards)?;
    let media: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM catalog.package_media_assets WHERE package_version_id=$1",
    )
    .bind(version)
    .fetch_one(&mut *tx)
    .await?;
    let tags:Vec<String>=sqlx::query_scalar("SELECT DISTINCT unnest(tags) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL").bind(workspace).fetch_all(&mut *tx).await?;
    let date = Utc::now().format("%Y-%m-%d");
    let mut suffix = 0_u64;
    let import_tag = loop {
        let tag = format!("import:{date}-{suffix}");
        if !tags.contains(&tag) {
            break tag;
        }
        suffix = suffix.checked_add(1).ok_or_else(ApiError::internal)?;
    };
    let result = json!({"packageVersion":package,"summary":{"cardCount":package.get("cardCount"),"mediaAssetCount":media},
        "tagCounts":counts.iter().map(|(tag,count)|json!({"tag":tag,"cardsCount":count})).collect::<Vec<_>>(),
        "defaultOptions":{"addImportTag":true,"suggestedImportTag":import_tag,"keptTags":counts.iter().map(|(tag,_)|tag).collect::<Vec<_>>(),"removedTags":[]}});
    tx.commit().await?;
    Ok(Json(result))
}

pub(super) async fn confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((workspace, version)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers).await?;
    let input = serde_json::from_value::<Input>(body)
        .map_err(|_| invalid())?
        .normalize()?;
    let mut tx = scoped(
        &state.pool,
        &user.user_id.to_string(),
        Some(&workspace.to_string()),
    )
    .await?;
    assert_access(&mut tx, workspace).await?;
    crate::core::sync::lock_hot(&mut tx, workspace).await?;
    if let Some(result) = replay::load(&mut tx, workspace, version, &input).await? {
        tx.commit().await?;
        record_install(&state, user.user_id, workspace, &result).await;
        return Ok(Json(result));
    }
    let row = load_version(&mut tx, version, true).await?;
    let package = version_value(&row)?;
    let cards = cards(&mut tx, version).await?;
    if cards.is_empty() {
        return Err(error(
            StatusCode::CONFLICT,
            "CATALOG_PACKAGE_VERSION_EMPTY",
            "Catalog package version contains no cards.",
        ));
    }
    let media:Vec<CatalogMedia>=sqlx::query_as("SELECT package_media_asset_id,package_media_key,media_blob_id FROM catalog.package_media_assets WHERE package_version_id=$1 ORDER BY package_media_key").bind(version).fetch_all(&mut *tx).await?;
    persistence::assert_available(&mut tx, workspace, &input, &cards, &media).await?;
    let result = persistence::install(&mut tx, workspace, package, &input, &cards, &media).await?;
    replay::store(&mut tx, workspace, version, &input, &result).await?;
    tx.commit().await?;
    record_install(&state, user.user_id, workspace, &result).await;
    Ok(Json(result))
}

async fn record_install(state: &AppState, user: Uuid, workspace: Uuid, result: &Value) {
    let Some(package) = result
        .pointer("/packageVersion/packageId")
        .and_then(Value::as_str)
        .and_then(|v| v.parse::<Uuid>().ok())
    else {
        return;
    };
    let slug =
        sqlx::query_scalar::<_, String>("SELECT slug FROM catalog.packages WHERE package_id=$1")
            .bind(package)
            .fetch_optional(&state.pool)
            .await;
    let Ok(Some(slug)) = slug else {
        tracing::warn!(%package,"Catalog install analytics skipped: current package slug unavailable");
        return;
    };
    let Some(install_id) = result.pointer("/summary/installId").and_then(Value::as_str) else {
        return;
    };
    let workspace_key = workspace.to_string();
    let now = Utc::now();
    crate::metadata::server_fact(state,crate::metadata::ServerFact {
        name:"catalog_deck_installed",stable_keys:&[&workspace_key,install_id],user_id:user,subject_user_id:Some(user),workspace_id:Some(workspace),occurred_at:now,received_at:now,platform:None,
        properties:json!({"package_slug":slug,"card_count":result.pointer("/summary/cardCount"),"package_version_id":result.pointer("/packageVersion/packageVersionId")}),details:None
    }).await;
}
