use super::{error, markdown, stamp, version_value};
use crate::{AppState, error::ApiError};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{Row, postgres::PgRow};
use std::collections::HashMap;
use uuid::Uuid;

pub(super) async fn snapshot() -> ApiError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "CATALOG_DUMP_POINTER_UNAVAILABLE",
        "Public catalog snapshot is unavailable.",
    )
}
pub(super) async fn unconfigured_media() -> ApiError {
    ApiError::internal()
}

fn limit(query: &HashMap<String, String>, default: i64) -> Result<i64, ApiError> {
    let Some(value) = query.get("limit") else {
        return Ok(default);
    };
    let number = value
        .parse::<i64>()
        .ok()
        .filter(|n| (1..=100).contains(n) && n.to_string() == *value);
    number.ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "CATALOG_PUBLIC_LIMIT_INVALID",
            "limit must be an integer between 1 and 100",
        )
    })
}
fn optional(query: &HashMap<String, String>, key: &str) -> Result<Option<String>, ApiError> {
    query
        .get(key)
        .map(|v| {
            let value = v.trim().to_lowercase();
            if value.is_empty() {
                Err(error(
                    StatusCode::BAD_REQUEST,
                    "CATALOG_PUBLIC_QUERY_INVALID",
                    "Query must not be empty",
                ))
            } else {
                Ok(value)
            }
        })
        .transpose()
}
fn not_found() -> ApiError {
    error(
        StatusCode::NOT_FOUND,
        "CATALOG_PUBLIC_PACKAGE_VERSION_NOT_FOUND",
        "Published catalog package version not found.",
    )
}

async fn load(state: &AppState, id: Uuid) -> Result<PgRow, ApiError> {
    let row = sqlx::query(include_str!("version.sql"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(not_found)?;
    if row.try_get::<String, _>("version_status")? != "published"
        || row.try_get::<String, _>("package_status")? != "published"
        || row
            .try_get::<Option<DateTime<Utc>>, _>("delisted_at")?
            .is_some()
        || row
            .try_get::<Option<DateTime<Utc>>, _>("package_delisted_at")?
            .is_some()
    {
        return Err(not_found());
    }
    Ok(row)
}

pub(super) async fn version(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = load(&state, id).await?;
    let mut value = version_value(&row)?;
    if let Some(map) = value.as_object_mut() {
        for key in [
            "description",
            "license",
            "contentWarning",
            "coverPackageMediaKey",
            "createdAt",
        ] {
            map.remove(key);
        }
    }
    markdown::safe_value(&value)?;
    Ok(Json(json!({"catalogPackageVersion":value})))
}

pub(super) async fn cards(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let size = limit(&query, 25)?;
    load(&state, id).await?;
    let rows=sqlx::query("SELECT ordinal,front_text,back_text,card_type,tags,media_asset_keys FROM catalog.package_cards WHERE package_version_id=$1 ORDER BY ordinal ASC LIMIT $2")
        .bind(id).bind(size).fetch_all(&state.pool).await?;
    let values=rows.iter().map(|row| {
        let front: String=row.try_get("front_text")?;
        let back: String=row.try_get("back_text")?;
        markdown::safe_markdown(&front)?; markdown::safe_markdown(&back)?;
        let value=json!({"ordinal":row.try_get::<i32,_>("ordinal")?,"frontText":front,"backText":back,
            "cardType":row.try_get::<String,_>("card_type")?,"tags":row.try_get::<Vec<String>,_>("tags")?,
            "mediaAssetKeys":row.try_get::<Vec<String>,_>("media_asset_keys")?});
        for key in ["cardType","tags","mediaAssetKeys"] { markdown::safe_value(value.get(key).ok_or_else(ApiError::internal)?)?; }
        Ok(value)
    }).collect::<Result<Vec<_>,ApiError>>()?;
    Ok(Json(json!({"packageVersionId":id,"cards":values})))
}

fn summary(row: &PgRow) -> Result<Value, ApiError> {
    let mut latest = version_value(row)?;
    let map = latest.as_object_mut().ok_or_else(ApiError::internal)?;
    map.remove("author");
    map.remove("createdAt");
    map.insert(
        "slug".into(),
        json!(row.try_get::<String, _>("package_slug")?),
    );
    map.insert("status".into(), json!("published"));
    map.insert("updatedAt".into(), json!(stamp(row.try_get("updated_at")?)));
    for (key, column) in [
        ("educationalSubject", "educational_subject"),
        ("educationalFramework", "educational_framework"),
        ("educationalLevel", "educational_level"),
    ] {
        map.insert(key.into(), json!(row.try_get::<Option<String>, _>(column)?));
    }
    let mut value = latest.clone();
    let map = value.as_object_mut().ok_or_else(ApiError::internal)?;
    for key in [
        "packageVersionId",
        "versionNumber",
        "cardCount",
        "updatedAt",
        "publishedAt",
    ] {
        map.remove(key);
    }
    map.insert("author".into(),json!({"authorId":row.try_get::<Uuid,_>("author_id")?,"slug":row.try_get::<String,_>("author_slug")?,
        "displayName":row.try_get::<String,_>("author_display_name")?,"bio":row.try_get::<Option<String>,_>("author_bio")?,
        "websiteUrl":row.try_get::<Option<String>,_>("author_website_url")?}));
    map.insert("latestVersion".into(), latest);
    markdown::safe_value(&value)?;
    Ok(value)
}

pub(super) async fn packages(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    if query.contains_key("topicTag") {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "CATALOG_PUBLIC_TOPIC_TAG_REMOVED",
            "topicTag was removed; omit topicTag from public catalog list requests.",
        ));
    }
    let size = limit(&query, 50)?;
    let search = optional(&query, "q")?.map(|value| {
        format!(
            "%{}%",
            value
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let language = optional(&query, "languageTag")?;
    let rows = sqlx::query(include_str!("packages.sql"))
        .bind(search)
        .bind(language)
        .bind(size)
        .fetch_all(&state.pool)
        .await?;
    Ok(Json(
        json!({"catalogPackages":rows.iter().map(summary).collect::<Result<Vec<_>,_>>()?}),
    ))
}
