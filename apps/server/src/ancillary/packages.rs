//! Portable ZIP packages keep card persistence on the canonical sync mutation boundary.

use crate::{
    AppState,
    auth::require_mutation,
    core::{CardSnapshot, Mutation, facts, mutate_card_in_tx, workspaces::assert_access},
    database::scoped,
    error::ApiError,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};
use icu_collator::{Collator, CollatorBorrowed, options::CollatorOptions};
use icu_locale::locale;
use pulldown_cmark::{Event, Parser, Tag};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    sync::OnceLock,
};
use uuid::Uuid;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const HTTP_LIMIT: usize = 4_000_000;
const JSON_LIMIT: u64 = 83_886_080;
const MEDIA_LIMIT: u64 = 16_777_216;
const MEDIA_TOTAL_LIMIT: usize = 67_108_864;
const CARD_LIMIT: usize = 5_000;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/workspaces/{workspace}/packages/import/preview",
            post(import_preview),
        )
        .route(
            "/v1/workspaces/{workspace}/packages/import",
            post(import_confirm),
        )
        .route(
            "/v1/workspaces/{workspace}/packages/export/preview",
            post(export_preview),
        )
        .route(
            "/v1/workspaces/{workspace}/packages/export",
            post(export_package),
        )
        .layer(DefaultBodyLimit::max(4_200_000))
}

fn input_error(message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "WORKSPACE_PACKAGE_IMPORT_INPUT_INVALID",
        message,
    )
}
fn zip_error(message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "WORKSPACE_PACKAGE_IMPORT_PREVIEW_ZIP_INVALID",
        message,
    )
}
fn schema_error(message: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "WORKSPACE_PACKAGE_IMPORT_PREVIEW_CARDS_JSON_INVALID",
        message,
    )
}
fn too_large() -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "WORKSPACE_PACKAGE_IMPORT_PREVIEW_TOO_LARGE",
        "Workspace package ZIP exceeds the supported decoded size or item limit.",
    )
}
fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Source {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    imported_at: Option<String>,
    #[serde(default)]
    import_id: Option<String>,
}
#[derive(Deserialize, Serialize)]
struct Metadata {
    version: u8,
    source: Option<Source>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PortableCard {
    front_text: String,
    back_text: String,
    tags: Vec<String>,
    card_type: String,
    metadata: Metadata,
}
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Package {
    format_version: u8,
    #[serde(flatten)]
    metadata: PackageMetadata,
    cards: Vec<PortableCard>,
}
struct Decoded {
    package: Package,
    media: BTreeSet<String>,
    references: BTreeSet<String>,
}

fn portable_path(value: &str) -> bool {
    value.starts_with("media/")
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}
fn destinations(markdown: &str) -> impl Iterator<Item = String> + '_ {
    Parser::new(markdown).filter_map(|event| match event {
        Event::Start(Tag::Image { dest_url, .. } | Tag::Link { dest_url, .. }) => {
            Some(dest_url.into_string())
        }
        _ => None,
    })
}
fn media_candidate(value: &str) -> bool {
    value == "media"
        || value.starts_with("media/")
        || value.starts_with("media\\")
        || value.starts_with("/media")
        || value.starts_with("./media")
        || value.starts_with("../media")
}
fn bounded_read(reader: impl Read, limit: u64) -> Result<Vec<u8>, ApiError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.checked_add(1).ok_or_else(ApiError::internal)?)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            zip_error(format!(
                "Workspace package ZIP entry cannot be read: {error}"
            ))
        })?;
    if u64::try_from(bytes.len()).map_err(|_| too_large())? > limit {
        return Err(too_large());
    }
    Ok(bytes)
}
fn decode(bytes: &[u8]) -> Result<Decoded, ApiError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| zip_error(format!("Workspace package ZIP is invalid: {error}")))?;
    if archive.len() > 10_001 {
        return Err(too_large());
    }
    reject_collapsed_entries(bytes, archive.central_directory_start(), archive.len())?;
    let mut media = BTreeSet::new();
    let mut folded_paths = BTreeSet::new();
    let mut json_bytes = None;
    let mut total_media = 0_usize;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| zip_error(error.to_string()))?;
        let path = entry.name().to_owned();
        if entry.encrypted()
            || !matches!(
                entry.compression(),
                CompressionMethod::Stored | CompressionMethod::Deflated
            )
        {
            return Err(zip_error(
                "Workspace package ZIP entry uses unsupported compression or encryption.",
            ));
        }
        if path == "cards.json" {
            if json_bytes.is_some() {
                return Err(zip_error(
                    "Workspace package ZIP must contain exactly one cards.json entry.",
                ));
            }
            json_bytes = Some(bounded_read(entry, JSON_LIMIT)?);
        } else {
            if !portable_path(&path) || !folded_paths.insert(path.to_ascii_lowercase()) {
                return Err(zip_error(format!(
                    "Workspace package ZIP contains a duplicate or unsafe media path: {path}"
                )));
            }
            let data = bounded_read(entry, MEDIA_LIMIT)?;
            total_media = total_media.checked_add(data.len()).ok_or_else(too_large)?;
            if total_media > MEDIA_TOTAL_LIMIT {
                return Err(too_large());
            }
            media.insert(path);
        }
    }
    let json_bytes = json_bytes.ok_or_else(|| {
        zip_error("Workspace package ZIP must contain exactly one cards.json entry.")
    })?;
    parse_package(&json_bytes, media)
}
// ZipArchive stores entries by name and collapses duplicates. Count the bounded central
// directory independently so duplicate cards.json entries cannot disappear before validation.
fn reject_collapsed_entries(bytes: &[u8], start: u64, expected: usize) -> Result<(), ApiError> {
    let mut cursor = Cursor::new(bytes);
    cursor.set_position(start);
    let mut count = 0_usize;
    loop {
        let mut signature = [0; 4];
        cursor
            .read_exact(&mut signature)
            .map_err(|_| zip_error("Truncated ZIP central directory."))?;
        if signature != [80, 75, 1, 2] {
            break;
        }
        let mut header = [0; 42];
        cursor
            .read_exact(&mut header)
            .map_err(|_| zip_error("Truncated ZIP central directory entry."))?;
        let length = |range: std::ops::Range<usize>| -> Result<i64, ApiError> {
            let value: [u8; 2] = header
                .get(range)
                .ok_or_else(|| zip_error("Invalid ZIP directory entry."))?
                .try_into()
                .map_err(|_| zip_error("Invalid ZIP directory length."))?;
            Ok(i64::from(u16::from_le_bytes(value)))
        };
        let skipped = length(24..26)?
            .checked_add(length(26..28)?)
            .and_then(|value| value.checked_add(length(28..30).ok()?))
            .ok_or_else(too_large)?;
        cursor
            .seek(SeekFrom::Current(skipped))
            .map_err(|_| zip_error("Invalid ZIP directory entry length."))?;
        count = count.checked_add(1).ok_or_else(too_large)?;
        if count > 10_001 {
            return Err(too_large());
        }
    }
    if count != expected {
        return Err(zip_error(
            "Workspace package ZIP contains duplicate entry names.",
        ));
    }
    Ok(())
}

fn parse_package(json_bytes: &[u8], media: BTreeSet<String>) -> Result<Decoded, ApiError> {
    let value: Value = serde_json::from_slice(json_bytes).map_err(|error| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_PREVIEW_CARDS_JSON_MALFORMED",
            format!("Workspace package cards.json is malformed JSON: {error}"),
        )
    })?;
    if value
        .get("cards")
        .and_then(Value::as_array)
        .is_none_or(|cards| {
            cards.iter().any(|card| {
                card.get("metadata")
                    .is_none_or(|metadata| metadata.get("source").is_none())
            })
        })
    {
        return Err(schema_error(
            "Workspace package cards.json requires complete card metadata.",
        ));
    }
    let mut package: Package = serde_json::from_value(value).map_err(|error| {
        schema_error(format!("Workspace package cards.json is invalid: {error}"))
    })?;
    if package.format_version != 1 || package.cards.iter().any(|card| card.metadata.version != 1) {
        return Err(schema_error(
            "Workspace package formatVersion and metadata.version must be 1.",
        ));
    }
    if package.cards.len() > CARD_LIMIT {
        return Err(too_large());
    }
    let mut references = BTreeSet::new();
    let mut folded_references = BTreeSet::new();
    for card in &mut package.cards {
        card.front_text = card.front_text.trim().to_owned();
        card.back_text = card.back_text.trim().to_owned();
        card.card_type = card.card_type.trim().to_owned();
        if card.card_type.is_empty() {
            card.card_type = "basic".into();
        }
        if card.front_text.is_empty() {
            return Err(schema_error("frontText must not be empty."));
        }
        for tag in &mut card.tags {
            *tag = tag.trim().to_owned();
            if tag.is_empty() {
                return Err(schema_error("tags[] must not be empty."));
            }
        }
        for destination in destinations(&card.front_text).chain(destinations(&card.back_text)) {
            if !media_candidate(&destination) {
                continue;
            }
            if !portable_path(&destination) {
                return Err(schema_error(format!(
                    "Workspace package cards.json contains unsafe media reference: {destination}"
                )));
            }
            if !references.contains(&destination)
                && !folded_references.insert(destination.to_ascii_lowercase())
            {
                return Err(schema_error(
                    "Workspace package cards.json contains duplicate media paths.",
                ));
            }
            if !media.contains(&destination) {
                return Err(zip_error(format!(
                    "Workspace package cards.json references media files missing from ZIP: {destination}"
                )));
            }
            references.insert(destination);
        }
    }
    Ok(Decoded {
        package,
        media,
        references,
    })
}
async fn decode_async(bytes: Vec<u8>) -> Result<Decoded, ApiError> {
    tokio::task::spawn_blocking(move || decode(&bytes))
        .await
        .map_err(|_| ApiError::internal())?
}
fn tag_counts(cards: &[PortableCard]) -> Result<Vec<Value>, ApiError> {
    static COLLATOR: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();
    let collator = COLLATOR
        .get_or_init(|| Collator::try_new(locale!("en-US").into(), CollatorOptions::default()).ok())
        .as_ref()
        .ok_or_else(ApiError::internal)?;
    let mut counts = BTreeMap::<String, usize>::new();
    for tag in cards.iter().flat_map(|card| &card.tags) {
        let count = counts.entry(tag.clone()).or_default();
        *count = count.saturating_add(1);
    }
    let mut pairs: Vec<_> = counts.into_iter().collect();
    pairs.sort_by(|(a, ac), (b, bc)| {
        bc.cmp(ac)
            .then_with(|| collator.compare(&a.to_lowercase(), &b.to_lowercase()))
            .then_with(|| collator.compare(a, b))
    });
    Ok(pairs
        .into_iter()
        .map(|(tag, cards_count)| json!({"tag":tag,"cardsCount":cards_count}))
        .collect())
}
async fn workspace_tx<'a>(
    state: &'a AppState,
    user: &str,
    workspace: Uuid,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    let mut tx = scoped(&state.pool, user, Some(&workspace.to_string())).await?;
    assert_access(&mut tx, workspace).await?;
    Ok(tx)
}
pub(super) async fn import_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    bytes: Bytes,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| {
            value
                .split(';')
                .next()
                .is_none_or(|mime| mime.trim() != "application/zip")
        })
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "WORKSPACE_PACKAGE_IMPORT_PREVIEW_CONTENT_TYPE_UNSUPPORTED",
            "Workspace package import preview requires application/zip.",
        ));
    }
    if bytes.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_PREVIEW_ZIP_EMPTY",
            "Workspace package ZIP must not be empty.",
        ));
    }
    if bytes.len() > HTTP_LIMIT {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "WORKSPACE_PACKAGE_IMPORT_PREVIEW_BODY_TOO_LARGE",
            "Workspace package preview body is too large.",
        ));
    }
    let mut tx = workspace_tx(&state, &user, workspace).await?;
    let tags:Vec<String>=sqlx::query_scalar("SELECT DISTINCT unnest(tags) FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL").bind(workspace).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let decoded = decode_async(bytes.to_vec()).await?;
    let day = Utc::now().format("%Y-%m-%d");
    let mut suffix = 0_u32;
    let suggested = loop {
        let candidate = format!("import:{day}-{suffix}");
        if !tags.contains(&candidate) {
            break candidate;
        }
        suffix = suffix.checked_add(1).ok_or_else(ApiError::internal)?;
    };
    let counts = tag_counts(&decoded.package.cards)?;
    let kept: Vec<_> = counts
        .iter()
        .filter_map(|count| count.get("tag").cloned())
        .collect();
    let warnings:Vec<_>=decoded.references.iter().filter(|path| !path.rsplit('.').next().is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(),"avif"|"gif"|"jpeg"|"jpg"|"png"|"svg"|"webp"))).map(|path|json!({"code":"WORKSPACE_PACKAGE_IMPORT_MEDIA_TYPE_UNSUPPORTED","message":"Referenced package media may not be supported by the import confirmation flow.","mediaPath":path})).collect();
    let metadata = &decoded.package.metadata;
    Ok(Json(
        json!({"sourceKind":"zip","packageMetadata":{"label":metadata.label,"author":metadata.author,"comment":metadata.comment,"createdAt":metadata.created_at,"sourceUrl":metadata.source_url},"cardCount":decoded.package.cards.len(),"tagCounts":counts,"referencedMediaCount":decoded.references.len(),"packageMediaFileCount":decoded.media.len(),"warnings":warnings,"defaultOptions":{"addImportTag":true,"suggestedImportTag":suggested,"keptTags":kept,"removedTags":[]}}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportOptions {
    add_import_tag: bool,
    import_tag: String,
    remove_tags: Vec<String>,
    imported_at: DateTime<Utc>,
    import_id: String,
    client_updated_at: DateTime<Utc>,
    last_modified_by_replica_id: Uuid,
    operation_id_prefix: String,
}

async fn parse_multipart(mut multipart: Multipart) -> Result<(Vec<u8>, ImportOptions), ApiError> {
    let mut file = None;
    let mut options = None;
    while let Some(field) = multipart.next_field().await.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_MULTIPART_INVALID",
            "Invalid workspace package multipart body.",
        )
    })? {
        match field.name() {
            Some("file") => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|_| input_error("Invalid package file."))?;
                if bytes.len() > HTTP_LIMIT {
                    return Err(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "WORKSPACE_PACKAGE_IMPORT_FILE_TOO_LARGE",
                        "Workspace package file is too large.",
                    ));
                }
                file = Some(bytes.to_vec());
            }
            Some("options") => {
                let text = field
                    .text()
                    .await
                    .map_err(|_| input_error("Invalid options field."))?;
                let value: Value = serde_json::from_str(&text).map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "WORKSPACE_PACKAGE_IMPORT_OPTIONS_INVALID_JSON",
                        "options must be valid JSON.",
                    )
                })?;
                options = Some(serde_json::from_value::<ImportOptions>(value).map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "WORKSPACE_PACKAGE_IMPORT_OPTIONS_INVALID",
                        "Invalid workspace package import options.",
                    )
                })?);
            }
            _ => {}
        }
    }
    let file = file.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_FILE_REQUIRED",
            "file is required.",
        )
    })?;
    if file.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_FILE_EMPTY",
            "file must not be empty.",
        ));
    }
    let options = options.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_OPTIONS_REQUIRED",
            "options is required.",
        )
    })?;
    Ok((file, options))
}

pub(super) async fn import_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> Result<Json<Value>, ApiError> {
    let user_id = require_mutation(&state, &headers).await?.user_id;
    let user = user_id.to_string();
    let mut tx = workspace_tx(&state, &user, workspace).await?;
    let multipart = multipart.map_err(|_| {
        ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "WORKSPACE_PACKAGE_IMPORT_CONTENT_TYPE_UNSUPPORTED",
            "Workspace package import requires multipart/form-data.",
        )
    })?;
    let (file, options) = parse_multipart(multipart).await?;
    let prefix = &options.operation_id_prefix;
    if prefix.is_empty()
        || prefix.len() > 1_013
        || prefix.trim() != prefix
        || !prefix.bytes().all(|b| (32..=126).contains(&b))
        || options.import_id.trim().is_empty()
        || options.add_import_tag && options.import_tag.trim().is_empty()
    {
        return Err(input_error(
            "Invalid workspace package import identifiers or tag.",
        ));
    }
    let replica_exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sync.workspace_replicas WHERE workspace_id=$1 AND replica_id=$2)").bind(workspace).bind(options.last_modified_by_replica_id).fetch_one(&mut *tx).await?;
    if !replica_exists {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_IMPORT_REPLICA_INVALID",
            "lastModifiedByReplicaId must reference a workspace replica for this workspace.",
        ));
    }
    let decoded = decode_async(file).await?;
    let source_tags: BTreeSet<_> = decoded
        .package
        .cards
        .iter()
        .flat_map(|card| &card.tags)
        .cloned()
        .collect();
    let mut removed = BTreeSet::new();
    for tag in &options.remove_tags {
        let tag = tag.trim();
        if tag.is_empty() || !source_tags.contains(tag) || !removed.insert(tag.to_owned()) {
            return Err(input_error(
                "removeTags must contain unique exact package tag values.",
            ));
        }
    }
    if !decoded.references.is_empty() {
        return Err(ApiError::internal());
    }
    let kept = source_tags.difference(&removed).count();
    let mut facts = facts::Buffer::default();
    facts.declare_creation_source("package_import");
    let cards = persist_cards(
        &mut tx,
        workspace,
        decoded.package,
        &options,
        &removed,
        &mut facts,
    )
    .await?;
    tx.commit().await?;
    facts.emit(&state, user_id, workspace, None).await;
    let workspace_key = workspace.to_string();
    let now = Utc::now().trunc_subsecs(3);
    crate::metadata::server_fact(
        &state,
        crate::metadata::ServerFact {
            name: "workspace_package_imported",
            stable_keys: &[&workspace_key, options.import_id.trim()],
            user_id,
            subject_user_id: Some(user_id),
            workspace_id: Some(workspace),
            occurred_at: now,
            received_at: now,
            platform: None,
            properties: json!({"card_count": cards.len()}),
            details: None,
        },
    )
    .await;
    let batch_count = cards.len().div_ceil(100);
    Ok(Json(
        json!({"cards":cards,"importedMediaAssets":[],"summary":{"cardCount":cards.len(),"cardBatchCount":batch_count,"referencedMediaCount":0,"importedMediaAssetCount":0,"appliedMediaAssetCount":0,"keptTagCount":kept,"removedTagCount":removed.len(),"importTag":if options.add_import_tag {Some(options.import_tag.trim())} else {None}}}),
    ))
}

async fn persist_cards(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    package: Package,
    options: &ImportOptions,
    removed: &BTreeSet<String>,
    facts: &mut facts::Buffer,
) -> Result<Vec<crate::core::Card>, ApiError> {
    let mut cards = Vec::new();
    let created_at = Utc::now();
    let prefix = &options.operation_id_prefix;
    for (index, mut card) in package.cards.into_iter().enumerate() {
        let original = card.metadata.source.take();
        let metadata = &package.metadata;
        let source = Source {
            label: original
                .as_ref()
                .and_then(|s| s.label.clone())
                .or_else(|| metadata.label.clone()),
            author: original
                .as_ref()
                .and_then(|s| s.author.clone())
                .or_else(|| metadata.author.clone()),
            comment: original
                .as_ref()
                .and_then(|s| s.comment.clone())
                .or_else(|| metadata.comment.clone()),
            created_at: original
                .as_ref()
                .and_then(|s| s.created_at.clone())
                .or_else(|| metadata.created_at.clone()),
            imported_at: Some(stamp(options.imported_at)),
            import_id: Some(options.import_id.trim().to_owned()),
        };
        let mut tags = Vec::new();
        for tag in card.tags {
            if !removed.contains(&tag) && !tags.contains(&tag) {
                tags.push(tag);
            }
        }
        if options.add_import_tag && !tags.iter().any(|tag| tag == options.import_tag.trim()) {
            tags.push(options.import_tag.trim().to_owned());
        }
        let snapshot = CardSnapshot {
            card_id: Uuid::new_v4(),
            front_text: card.front_text,
            back_text: card.back_text,
            card_type: card.card_type,
            metadata: serde_json::to_value(Metadata {
                version: 1,
                source: Some(source),
            })
            .map_err(|_| ApiError::internal())?,
            tags,
            due_at: None,
            created_at,
            reps: 0,
            lapses: 0,
            fsrs_card_state: "new".into(),
            fsrs_step_index: None,
            fsrs_stability: None,
            fsrs_difficulty: None,
            fsrs_last_reviewed_at: None,
            fsrs_scheduled_days: None,
            deleted_at: None,
        };
        let mutation = Mutation {
            client_updated_at: options.client_updated_at,
            replica_id: options.last_modified_by_replica_id,
            operation_id: format!("{prefix}:card:{index}"),
        };
        let persisted = mutate_card_in_tx(tx, workspace, snapshot, &mutation)
            .await?
            .0;
        let id = persisted.snapshot.card_id;
        let after = facts::content(tx, workspace, "card", id)
            .await?
            .ok_or_else(ApiError::internal)?;
        facts.content(
            "card",
            id,
            None,
            &after,
            &Mutation {
                client_updated_at: persisted.client_updated_at,
                replica_id: persisted.last_modified_by_replica_id,
                operation_id: persisted.last_operation_id.clone(),
            },
        );
        cards.push(persisted);
    }
    Ok(cards)
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Selection {
    AllActiveCards,
    TagFilters {
        #[serde(rename = "includeTags")]
        include_tags: Vec<String>,
        #[serde(rename = "excludeTags")]
        exclude_tags: Vec<String>,
    },
    ExplicitCardIds {
        #[serde(rename = "cardIds")]
        card_ids: Vec<Uuid>,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TagPolicy {
    additional_removed_tags: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportInput {
    selection: Selection,
    tag_policy: TagPolicy,
    package_metadata: PackageMetadata,
}
fn export_input(value: Value) -> Result<ExportInput, ApiError> {
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_PACKAGE_EXPORT_REQUEST_INVALID",
            "Invalid workspace package export request.",
        )
    };
    let metadata = value
        .get("packageMetadata")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    if ["label", "author", "comment", "createdAt", "sourceUrl"]
        .iter()
        .any(|key| {
            metadata
                .get(*key)
                .is_none_or(|value| !value.is_null() && !value.is_string())
        })
    {
        return Err(invalid());
    }
    serde_json::from_value(value).map_err(|_| invalid())
}
struct PreparedExport {
    package: Package,
    counts: Vec<Value>,
    removed: Vec<Value>,
    media_count: usize,
    media_bytes: i64,
}
fn export_error(
    suffix: &str,
    message: impl Into<String>,
    download: bool,
    status: StatusCode,
) -> ApiError {
    ApiError::new(
        status,
        format!(
            "WORKSPACE_PACKAGE_EXPORT_{}_{suffix}",
            if download { "PACKAGE" } else { "PREVIEW" }
        ),
        message,
    )
}
struct ExportSelection {
    include: Vec<String>,
    exclude: Vec<String>,
    ids: Option<Vec<Uuid>>,
}
fn export_selection(
    selection: Selection,
    removed_tags: &[String],
    download: bool,
) -> Result<ExportSelection, ApiError> {
    let (include, exclude, ids) = match selection {
        Selection::AllActiveCards => (Vec::new(), Vec::new(), None),
        Selection::TagFilters {
            include_tags,
            exclude_tags,
        } => (include_tags, exclude_tags, None),
        Selection::ExplicitCardIds { card_ids } => {
            let unique: BTreeSet<_> = card_ids.iter().collect();
            if card_ids.is_empty() || unique.len() != card_ids.len() {
                return Err(export_error(
                    "INPUT_INVALID",
                    "selection.cardIds must contain unique UUIDs.",
                    download,
                    StatusCode::BAD_REQUEST,
                ));
            }
            (Vec::new(), Vec::new(), Some(card_ids))
        }
    };
    if include
        .iter()
        .chain(&exclude)
        .chain(removed_tags)
        .any(|tag| tag.trim().is_empty())
    {
        return Err(export_error(
            "INPUT_INVALID",
            "Tags must not be empty.",
            download,
            StatusCode::BAD_REQUEST,
        ));
    }
    let include: Vec<_> = include.iter().map(|tag| tag.trim().to_owned()).collect();
    let exclude: Vec<_> = exclude.iter().map(|tag| tag.trim().to_owned()).collect();
    Ok(ExportSelection {
        include,
        exclude,
        ids,
    })
}

async fn prepare_export(
    state: &AppState,
    user: &str,
    workspace: Uuid,
    mut input: ExportInput,
    download: bool,
) -> Result<PreparedExport, ApiError> {
    let mut tx = workspace_tx(state, user, workspace).await?;
    let ExportSelection {
        include,
        exclude,
        ids,
    } = export_selection(
        input.selection,
        &input.tag_policy.additional_removed_tags,
        download,
    )?;
    let rows=sqlx::query("SELECT card_id,front_text,back_text,card_type,metadata,tags FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL AND (cardinality($2::text[])=0 OR tags && $2) AND (cardinality($3::text[])=0 OR NOT tags && $3) AND ($4::uuid[] IS NULL OR card_id=ANY($4)) ORDER BY CASE WHEN $4 IS NOT NULL THEN array_position($4,card_id) END,created_at DESC,card_id ASC LIMIT 5001").bind(workspace).bind(include).bind(exclude).bind(&ids).fetch_all(&mut *tx).await?;
    if rows.len() > CARD_LIMIT || ids.as_ref().is_some_and(|ids| ids.len() > CARD_LIMIT) {
        return Err(export_error(
            "SELECTION_TOO_LARGE",
            "Workspace package export selection is too large. selectedCardLimit=5000",
            download,
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    if ids.as_ref().is_some_and(|ids| ids.len() != rows.len()) {
        return Err(export_error(
            "CARD_NOT_FOUND",
            "Workspace package export selection contains unavailable cards.",
            download,
            StatusCode::NOT_FOUND,
        ));
    }
    let mut cards = Vec::new();
    let mut media_ids = BTreeSet::new();
    for row in rows {
        let front_text: String = row.try_get("front_text")?;
        let back_text: String = row.try_get("back_text")?;
        for destination in destinations(&front_text).chain(destinations(&back_text)) {
            if destination.to_ascii_lowercase().starts_with("fcasset:") {
                let id=destination.strip_prefix("fcasset:").filter(|id|!id.is_empty() && id.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'.'|b'_'|b'-'))).ok_or_else(||ApiError::new(StatusCode::CONFLICT,"WORKSPACE_PACKAGE_EXPORT_MANAGED_MEDIA_NOT_READY","Workspace package export requires valid ready managed media references."))?;
                media_ids.insert(id.parse::<Uuid>().map_err(|_| {
                    export_error(
                        "MEDIA_ASSET_ID_INVALID",
                        "Workspace package references invalid media asset ids.",
                        download,
                        StatusCode::BAD_REQUEST,
                    )
                })?);
            }
        }
        let metadata: Value = row.try_get("metadata")?;
        cards.push(PortableCard {
            front_text,
            back_text,
            tags: row.try_get("tags")?,
            card_type: row.try_get("card_type")?,
            metadata: serde_json::from_value(metadata).map_err(|_| ApiError::internal())?,
        });
    }
    let media_bytes = export_media_bytes(&mut tx, workspace, &media_ids, download).await?;
    tx.commit().await?;
    let counts = tag_counts(&cards)?;
    let removed: Vec<_> = counts
        .iter()
        .filter(|count| {
            count.get("tag").and_then(Value::as_str).is_some_and(|tag| {
                tag.starts_with("import:")
                    || input
                        .tag_policy
                        .additional_removed_tags
                        .iter()
                        .any(|removed| removed.trim() == tag)
            })
        })
        .cloned()
        .collect();
    for card in &mut cards {
        card.tags.retain(|tag| {
            !removed
                .iter()
                .any(|count| count.get("tag").and_then(Value::as_str) == Some(tag))
        });
    }
    normalize_export_metadata(&mut input.package_metadata, download)?;
    Ok(PreparedExport {
        package: Package {
            format_version: 1,
            metadata: input.package_metadata,
            cards,
        },
        counts,
        removed,
        media_count: media_ids.len(),
        media_bytes,
    })
}
async fn export_media_bytes(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    media_ids: &BTreeSet<Uuid>,
    download: bool,
) -> Result<i64, ApiError> {
    let mut media_bytes = 0_i64;
    if !media_ids.is_empty() {
        let media_ids: Vec<_> = media_ids.iter().copied().collect();
        let media_rows=sqlx::query("SELECT a.media_asset_id,b.media_blob_id,b.size_bytes FROM content.media_assets a JOIN content.media_blobs b ON b.media_blob_id=a.media_blob_id WHERE a.workspace_id=$1 AND a.media_asset_id=ANY($2::uuid[]) AND a.deleted_at IS NULL").bind(workspace).bind(&media_ids).fetch_all(&mut **tx).await?;
        if media_rows.len() != media_ids.len() {
            return Err(export_error(
                "MEDIA_ASSET_UNAVAILABLE",
                "Workspace package references unavailable media assets.",
                download,
                StatusCode::BAD_REQUEST,
            ));
        }
        let mut blobs = BTreeSet::new();
        for row in media_rows {
            if blobs.insert(row.try_get::<Uuid, _>("media_blob_id")?) {
                media_bytes = media_bytes
                    .checked_add(row.try_get("size_bytes")?)
                    .ok_or_else(ApiError::internal)?;
            }
        }
        if download {
            return Err(ApiError::internal());
        }
    }
    Ok(media_bytes)
}

fn normalize_export_metadata(
    metadata: &mut PackageMetadata,
    download: bool,
) -> Result<(), ApiError> {
    for value in [
        &mut metadata.label,
        &mut metadata.author,
        &mut metadata.comment,
        &mut metadata.source_url,
    ]
    .into_iter()
    .flatten()
    {
        *value = value.trim().to_owned();
        if value.is_empty() {
            return Err(export_error(
                "INPUT_INVALID",
                "Package metadata must not be empty.",
                download,
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    if metadata.label.is_none() {
        metadata.label = Some("Workspace export".into());
    }
    metadata.created_at = Some(if let Some(value) = metadata.created_at.as_ref() {
        stamp(value.parse::<DateTime<Utc>>().map_err(|_| {
            export_error(
                "INPUT_INVALID",
                "createdAt must be a valid ISO timestamp.",
                download,
                StatusCode::BAD_REQUEST,
            )
        })?)
    } else {
        stamp(Utc::now())
    });
    Ok(())
}

pub(super) async fn export_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let export = prepare_export(&state, &user, workspace, export_input(input)?, false).await?;
    Ok(Json(
        json!({"selectedCardCount":export.package.cards.len(),"availableTagCounts":export.counts,"tagsSelectedForRemoval":export.removed,"referencedMediaCount":export.media_count,"approximateReferencedMediaBytes":export.media_bytes,"defaultPackageMetadata":export.package.metadata}),
    ))
}
pub(super) async fn export_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(workspace): Path<Uuid>,
    Json(input): Json<Value>,
) -> Result<Response, ApiError> {
    let user_id = require_mutation(&state, &headers).await?.user_id;
    let user = user_id.to_string();
    let export = prepare_export(&state, &user, workspace, export_input(input)?, true).await?;
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, ApiError> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "cards.json",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .map_err(|_| ApiError::internal())?;
        serde_json::to_writer_pretty(&mut writer, &export.package)
            .map_err(|_| ApiError::internal())?;
        writer.write_all(b"\n").map_err(|_| ApiError::internal())?;
        writer
            .finish()
            .map(Cursor::into_inner)
            .map_err(|_| ApiError::internal())
    })
    .await
    .map_err(|_| ApiError::internal())??;
    let workspace_key = workspace.to_string();
    let now = Utc::now().trunc_subsecs(3);
    let instant = stamp(now);
    crate::metadata::server_fact(
        &state,
        crate::metadata::ServerFact {
            name: "workspace_package_exported",
            stable_keys: &[&workspace_key, &instant],
            user_id,
            subject_user_id: Some(user_id),
            workspace_id: Some(workspace),
            occurred_at: now,
            received_at: now,
            platform: None,
            properties: json!({}),
            details: None,
        },
    )
    .await;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"flashcards.zip\"",
            ),
        ],
        bytes,
    )
        .into_response())
}
