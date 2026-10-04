//! Published catalog reads and atomic workspace installs preserve the existing durable replay ledger.
mod install;
mod markdown;
mod public;

use crate::{AppState, error::ApiError};
use axum::{
    Router,
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::{Postgres, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/catalog", get(public::snapshot))
        .route("/v1/catalog/packages", get(public::packages))
        .route(
            "/v1/catalog/packages/{slug}",
            get(public::unconfigured_media),
        )
        .route(
            "/v1/catalog/package-versions/{version}",
            get(public::version),
        )
        .route(
            "/v1/catalog/package-versions/{version}/cards",
            get(public::cards),
        )
        .route(
            "/v1/catalog/package-versions/{version}/media-assets/{key}/download",
            get(public::unconfigured_media),
        )
        .route(
            "/v1/catalog/package-versions/{version}/media-assets/{key}/download-url",
            get(public::unconfigured_media),
        )
        .route(
            "/v1/catalog/collections/{collection}/cover/download",
            get(public::unconfigured_media),
        )
        .route(
            "/v1/catalog/collections/{collection}/cover/download-url",
            get(public::unconfigured_media),
        )
        .route(
            "/v1/workspaces/{workspace}/catalog/package-versions/{version}/install/preview",
            post(install::preview),
        )
        .route(
            "/v1/workspaces/{workspace}/catalog/package-versions/{version}/install",
            post(install::confirm),
        )
}

fn error(status: StatusCode, code: &str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}
fn invalid() -> ApiError {
    error(
        StatusCode::BAD_REQUEST,
        "CATALOG_PACKAGE_INSTALL_INVALID_INPUT",
        "Catalog package install input is invalid.",
    )
}
fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn version_value(row: &PgRow) -> Result<Value, ApiError> {
    Ok(serde_json::json!({
        "packageVersionId":row.try_get::<Uuid,_>("package_version_id")?,
        "packageId":row.try_get::<Uuid,_>("package_id")?,
        "versionNumber":row.try_get::<i32,_>("version_number")?,
        "slug":row.try_get::<String,_>("slug")?, "title":row.try_get::<String,_>("title")?,
        "summary":row.try_get::<String,_>("summary")?, "description":row.try_get::<String,_>("description")?,
        "languageTags":row.try_get::<Vec<String>,_>("language_tags")?, "license":row.try_get::<String,_>("license")?,
        "contentWarning":row.try_get::<Option<String>,_>("content_warning")?,
        "coverPackageMediaKey":row.try_get::<Option<String>,_>("cover_package_media_key")?,
        "cardCount":row.try_get::<i32,_>("card_count")?, "createdAt":stamp(row.try_get("created_at")?),
        "publishedAt":row.try_get::<Option<DateTime<Utc>>,_>("published_at")?.map(stamp),
        "author":{"authorId":row.try_get::<Uuid,_>("author_id")?, "slug":row.try_get::<String,_>("author_slug")?,
            "displayName":row.try_get::<String,_>("author_display_name")?}
    }))
}

async fn load_version(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    lock: bool,
) -> Result<PgRow, ApiError> {
    let query = if lock {
        include_str!("catalog/version-lock.sql")
    } else {
        include_str!("catalog/version.sql")
    };
    let row = sqlx::query(query)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            error(
                StatusCode::NOT_FOUND,
                "CATALOG_PACKAGE_VERSION_NOT_FOUND",
                "Catalog package version not found.",
            )
        })?;
    if row.try_get::<String, _>("version_status")? != "published" {
        return Err(error(
            StatusCode::CONFLICT,
            "CATALOG_PACKAGE_VERSION_NOT_PUBLISHED",
            "Catalog package version must be published before installation.",
        ));
    }
    Ok(row)
}
