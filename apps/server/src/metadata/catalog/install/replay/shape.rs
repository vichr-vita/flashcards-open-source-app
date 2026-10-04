//! Validate durable Node-era result records before replaying them unchanged.
use super::invalid_stored;
use crate::error::ApiError;
use chrono::DateTime;
use serde_json::Value;
use uuid::Uuid;

fn object(value: &Value, keys: &[&str]) -> Result<(), ApiError> {
    let map = value.as_object().ok_or_else(invalid_stored)?;
    if map.len() != keys.len() || keys.iter().any(|key| !map.contains_key(*key)) {
        return Err(invalid_stored());
    }
    Ok(())
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value, ApiError> {
    value.get(key).ok_or_else(invalid_stored)
}
fn strings(value: &Value, keys: &[&str]) -> Result<(), ApiError> {
    for key in keys {
        if !field(value, key)?.is_string() {
            return Err(invalid_stored());
        }
    }
    Ok(())
}
fn ids(value: &Value, keys: &[&str]) -> Result<(), ApiError> {
    for key in keys {
        field(value, key)?
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok())
            .ok_or_else(invalid_stored)?;
    }
    Ok(())
}
fn integer(value: &Value, key: &str, minimum: u64) -> Result<(), ApiError> {
    if field(value, key)?.as_u64().is_none_or(|v| v < minimum) {
        return Err(invalid_stored());
    }
    Ok(())
}
fn datetime(value: &Value) -> Result<(), ApiError> {
    value
        .as_str()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .ok_or_else(invalid_stored)?;
    Ok(())
}
fn version(value: &Value) -> Result<(), ApiError> {
    object(
        value,
        &[
            "packageVersionId",
            "packageId",
            "versionNumber",
            "slug",
            "title",
            "summary",
            "description",
            "languageTags",
            "license",
            "contentWarning",
            "coverPackageMediaKey",
            "cardCount",
            "createdAt",
            "publishedAt",
            "author",
        ],
    )?;
    ids(value, &["packageVersionId", "packageId"])?;
    strings(
        value,
        &["slug", "title", "summary", "description", "license"],
    )?;
    integer(value, "versionNumber", 1)?;
    integer(value, "cardCount", 0)?;
    let languages = field(value, "languageTags")?
        .as_array()
        .ok_or_else(invalid_stored)?;
    if languages.iter().any(|v| !v.is_string()) {
        return Err(invalid_stored());
    }
    for key in ["contentWarning", "coverPackageMediaKey"] {
        let value = field(value, key)?;
        if !value.is_null() && !value.is_string() {
            return Err(invalid_stored());
        }
    }
    datetime(field(value, "createdAt")?)?;
    let published = field(value, "publishedAt")?;
    if !published.is_null() {
        datetime(published)?;
    }
    let author = field(value, "author")?;
    object(author, &["authorId", "slug", "displayName"])?;
    ids(author, &["authorId"])?;
    strings(author, &["slug", "displayName"])
}
pub(super) fn validate(value: &Value) -> Result<(), ApiError> {
    object(
        value,
        &[
            "packageVersion",
            "installedCards",
            "installedMediaAssets",
            "summary",
        ],
    )?;
    version(field(value, "packageVersion")?)?;
    for card in field(value, "installedCards")?
        .as_array()
        .ok_or_else(invalid_stored)?
    {
        object(
            card,
            &["packageCardId", "stableCardKey", "ordinal", "cardId"],
        )?;
        ids(card, &["packageCardId", "cardId"])?;
        strings(card, &["stableCardKey"])?;
        integer(card, "ordinal", 1)?;
    }
    for asset in field(value, "installedMediaAssets")?
        .as_array()
        .ok_or_else(invalid_stored)?
    {
        object(
            asset,
            &["packageMediaAssetId", "packageMediaKey", "mediaAssetId"],
        )?;
        ids(asset, &["packageMediaAssetId", "mediaAssetId"])?;
        strings(asset, &["packageMediaKey"])?;
    }
    let summary = field(value, "summary")?;
    object(
        summary,
        &[
            "cardCount",
            "mediaAssetCount",
            "installId",
            "installedAt",
            "keptTagCount",
            "removedTagCount",
            "importTag",
        ],
    )?;
    strings(summary, &["installId"])?;
    datetime(field(summary, "installedAt")?)?;
    for key in [
        "cardCount",
        "mediaAssetCount",
        "keptTagCount",
        "removedTagCount",
    ] {
        integer(summary, key, 0)?;
    }
    let tag = field(summary, "importTag")?;
    if !tag.is_null() && !tag.is_string() {
        return Err(invalid_stored());
    }
    Ok(())
}
