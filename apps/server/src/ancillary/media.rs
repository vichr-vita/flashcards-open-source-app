//! Registry reads remain usable when the private host has no configured object storage.
//!
//! Object writes fail before changing writer leases, upload sessions, or blob references.
//! This private installation deliberately has no S3 bucket; adding a storage provider requires
//! preserving the stored admission/fencing functions rather than bypassing their ownership.

use crate::{
    AppState,
    auth::{authenticate, require_mutation},
    core::workspaces::assert_access,
    database::scoped,
    error::ApiError,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/workspaces/{workspace}/media-assets/{media}", get(read))
        .route(
            "/v1/workspaces/{workspace}/media-assets/{media}/download-url",
            get(download),
        )
        .route(
            "/v1/workspaces/{workspace}/media-assets/images",
            post(image),
        )
        .route(
            "/v1/workspaces/{workspace}/media-assets/upload-sessions",
            post(create),
        )
        .route(
            "/v1/workspaces/{workspace}/media-assets/upload-sessions/{session}/parts",
            post(parts),
        )
        .route(
            "/v1/workspaces/{workspace}/media-assets/upload-sessions/{session}/complete",
            post(complete),
        )
        .route(
            "/v1/workspaces/{workspace}/media-assets/upload-sessions/{session}/abort",
            post(abort),
        )
}
fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}
fn unavailable() -> ApiError {
    tracing::warn!(
        "MEDIA_ASSETS_S3_BUCKET_NAME is required for media asset storage on the private installation"
    );
    ApiError::internal()
}
async fn workspace<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    id: Uuid,
    mutation: bool,
) -> Result<Transaction<'a, Postgres>, ApiError> {
    let identity = if mutation {
        require_mutation(state, headers).await?
    } else {
        authenticate(state, headers).await?
    };
    let mut tx = scoped(
        &state.pool,
        &identity.user_id.to_string(),
        Some(&id.to_string()),
    )
    .await?;
    assert_access(&mut tx, id).await?;
    Ok(tx)
}
async fn asset(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    id: Uuid,
) -> Result<Value, ApiError> {
    let row=sqlx::query("SELECT a.media_asset_id,a.workspace_id,b.mime_type,b.size_bytes,b.sha256,a.source_url,a.created_at,a.client_updated_at,a.last_modified_by_replica_id,a.last_operation_id,a.updated_at,a.deleted_at FROM content.media_assets a JOIN content.media_blobs b USING(media_blob_id) WHERE a.workspace_id=$1 AND a.media_asset_id=$2 LIMIT 1").bind(workspace).bind(id).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::new(StatusCode::NOT_FOUND,"MEDIA_ASSET_NOT_FOUND","Media asset not found."))?;
    let deleted: Option<DateTime<Utc>> = row.try_get("deleted_at")?;
    Ok(
        json!({"mediaAssetId":id,"workspaceId":workspace,"mimeType":row.try_get::<String,_>("mime_type")?,"sizeBytes":row.try_get::<i64,_>("size_bytes")?,"sha256":row.try_get::<String,_>("sha256")?,"sourceUrl":row.try_get::<Option<String>,_>("source_url")?,"createdAt":stamp(row.try_get("created_at")?),"clientUpdatedAt":stamp(row.try_get("client_updated_at")?),"lastModifiedByReplicaId":row.try_get::<Uuid,_>("last_modified_by_replica_id")?,"lastOperationId":row.try_get::<String,_>("last_operation_id")?,"updatedAt":stamp(row.try_get("updated_at")?),"deletedAt":deleted.map(stamp)}),
    )
}
async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, media)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, false).await?;
    let value = asset(&mut tx, id, media).await?;
    tx.commit().await?;
    Ok(Json(json!({"mediaAsset":value})))
}
async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, media)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, false).await?;
    asset(&mut tx, id, media).await?;
    tx.commit().await?;
    Err(unavailable())
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Upload {
    media_asset_id: Uuid,
    mime_type: String,
    size_bytes: i64,
    sha256: String,
    part_size_bytes: i64,
    part_count: i64,
    source_url: Option<String>,
    created_at: DateTime<Utc>,
    client_updated_at: DateTime<Utc>,
    last_modified_by_replica_id: Uuid,
    last_operation_id: String,
}
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<Upload>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, true).await?;
    let _ = (
        body.media_asset_id,
        body.source_url,
        body.created_at,
        body.client_updated_at,
    );
    let expected_parts = body
        .size_bytes
        .checked_add(body.part_size_bytes)
        .and_then(|value| value.checked_sub(1))
        .and_then(|value| value.checked_div(body.part_size_bytes));
    if body.mime_type.trim().is_empty()
        || body.size_bytes <= 0
        || body.part_size_bytes < 1
        || body.part_size_bytes > 5_368_709_120
        || body.size_bytes > 5_368_709_120
        || body.part_count > 1 && body.part_size_bytes < 5_242_880
        || !(1..=10_000).contains(&body.part_count)
        || expected_parts != Some(body.part_count)
        || body.sha256.len() != 64
        || !body.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || body.last_operation_id.is_empty()
        || body.last_operation_id.len() > 1_024
        || body.last_operation_id.trim() != body.last_operation_id
        || !body
            .last_operation_id
            .bytes()
            .all(|b| (32..=126).contains(&b))
    {
        return Err(ApiError::bad_request("Invalid media asset upload input."));
    }
    let replica:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sync.workspace_replicas WHERE workspace_id=$1 AND replica_id=$2)").bind(id).bind(body.last_modified_by_replica_id).fetch_one(&mut *tx).await?;
    if !replica {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MEDIA_ASSET_REPLICA_INVALID",
            "lastModifiedByReplicaId must reference a workspace replica accessible to the authenticated user.",
        ));
    }
    tx.commit().await?;
    Err(unavailable())
}
async fn image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let tx = workspace(&state, &headers, id, true).await?;
    tx.commit().await?;
    Err(unavailable())
}
async fn session(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    session: Uuid,
) -> Result<sqlx::postgres::PgRow, ApiError> {
    sqlx::query("SELECT media_asset_id,mime_type,size_bytes,media_blob_sha256,part_count,completed_at,aborted_at FROM content.media_upload_sessions WHERE workspace_id=$1 AND media_upload_session_id=$2 LIMIT 1").bind(id).bind(session).fetch_optional(&mut **tx).await?.ok_or_else(||ApiError::new(StatusCode::NOT_FOUND,"MEDIA_ASSET_UPLOAD_SESSION_NOT_FOUND",format!("Media asset upload session not found. sessionId={session}")))
}
fn validate_parts(body: &Value, complete: bool) -> Result<Vec<i64>, ApiError> {
    let parts = body
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("parts must be an array"))?;
    if parts.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MEDIA_ASSET_PARTS_REQUIRED",
            "parts must contain at least one part",
        ));
    }
    if parts.len() > if complete { 10_000 } else { 100 } {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            if complete {
                "MEDIA_ASSET_PART_COUNT_INVALID"
            } else {
                "MEDIA_ASSET_PART_URL_BATCH_TOO_LARGE"
            },
            "parts exceeds the supported request limit",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut numbers = Vec::new();
    for part in parts {
        let number = part
            .get("partNumber")
            .and_then(Value::as_i64)
            .filter(|number| (1..=10_000).contains(number))
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "MEDIA_ASSET_PART_NUMBER_INVALID",
                    "partNumber must be an integer from 1 to 10000",
                )
            })?;
        if !seen.insert(number) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "MEDIA_ASSET_DUPLICATE_PART_NUMBER",
                "parts must not contain duplicate partNumber values",
            ));
        }
        let sha = part
            .get("sha256")
            .and_then(Value::as_str)
            .filter(|sha| sha.len() == 64 && sha.bytes().all(|byte| byte.is_ascii_hexdigit()));
        if sha.is_none() {
            return Err(ApiError::bad_request("sha256 must be a hex SHA-256 digest"));
        }
        if complete
            && part.get("eTag").and_then(Value::as_str).is_none_or(|etag| {
                etag.trim().is_empty() || etag.len() > 256 || etag.chars().any(char::is_control)
            })
        {
            return Err(ApiError::bad_request("eTag must be a nonempty valid ETag"));
        }
        numbers.push(number);
    }
    Ok(numbers)
}
async fn parts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, session_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, true).await?;
    let parts = validate_parts(&body, false)?;
    let row = session(&mut tx, id, session_id).await?;
    let count: i64 = row.try_get("part_count")?;
    if parts.iter().any(|part| *part > count) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MEDIA_ASSET_PART_NUMBER_INVALID",
            "partNumber exceeds the upload session partCount",
        ));
    }
    tx.commit().await?;
    Err(unavailable())
}
async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, session_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, true).await?;
    let parts = validate_parts(&body, true)?;
    let row = session(&mut tx, id, session_id).await?;
    let count: i64 = row.try_get("part_count")?;
    if i64::try_from(parts.len()).map_err(|_| ApiError::internal())? != count
        || parts.iter().any(|part| *part > count)
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "MEDIA_ASSET_PART_SEQUENCE_INVALID",
            "parts must contain every partNumber from 1 through the upload session partCount",
        ));
    }
    let completed: Option<DateTime<Utc>> = row.try_get("completed_at")?;
    if completed.is_some() {
        let media = asset(&mut tx, id, row.try_get("media_asset_id")?).await?;
        let mime: String = row.try_get("mime_type")?;
        let bytes: i64 = row.try_get("size_bytes")?;
        let sha: String = row.try_get("media_blob_sha256")?;
        if media.get("mimeType") != Some(&json!(mime))
            || media.get("sizeBytes") != Some(&json!(bytes))
            || media.get("sha256") != Some(&json!(sha))
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "MEDIA_ASSET_UPLOAD_SESSION_STATE_CONFLICT",
                "Completed media asset upload session conflicts with current immutable blob identity.",
            ));
        }
        tx.commit().await?;
        return Ok(Json(json!({"mediaAsset":media,"applied":false})));
    }
    tx.commit().await?;
    Err(unavailable())
}
async fn abort(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, session_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = workspace(&state, &headers, id, true).await?;
    let row = session(&mut tx, id, session_id).await?;
    let aborted: Option<DateTime<Utc>> = row.try_get("aborted_at")?;
    if let Some(aborted) = aborted {
        tx.commit().await?;
        return Ok(Json(
            json!({"sessionId":session_id,"abortedAt":stamp(aborted)}),
        ));
    }
    tx.commit().await?;
    Err(unavailable())
}
