//! Existing account identity and preference contract for the private browser installation.

use crate::{
    AppState,
    auth::{authenticate, require_mutation},
    database::scoped,
    error::ApiError,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Deserialize, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub accent_color: String,
    pub review_reaction_animations_enabled: bool,
    pub analytics_consent: Option<String>,
    pub product_analytics_enabled: Option<bool>,
}

pub(super) async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let identity = authenticate(&state, &headers).await?;
    let user = identity.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let selected:Option<uuid::Uuid>=sqlx::query_scalar("SELECT s.workspace_id FROM org.user_settings s JOIN org.workspace_memberships m ON m.workspace_id=s.workspace_id AND m.user_id=s.user_id WHERE s.user_id=$1").bind(&user).fetch_optional(&mut *tx).await?.flatten();
    if selected.is_none() {
        let earliest:Option<uuid::Uuid>=sqlx::query_scalar("SELECT w.workspace_id FROM org.workspaces w JOIN org.workspace_memberships m ON m.workspace_id=w.workspace_id AND m.user_id=$1 ORDER BY w.created_at ASC,w.workspace_id ASC LIMIT 1").bind(&user).fetch_optional(&mut *tx).await?;
        let workspace = if let Some(workspace) = earliest {
            workspace
        } else {
            super::workspaces::create_workspace_in_tx(&mut tx, &user, "Personal").await?
        };
        sqlx::query("UPDATE org.user_settings SET workspace_id=$2 WHERE user_id=$1")
            .bind(&user)
            .bind(workspace)
            .execute(&mut *tx)
            .await?;
    }
    let mut value:Value=sqlx::query_scalar("SELECT jsonb_build_object('userId',user_id,'selectedWorkspaceId',workspace_id,'authTransport','session','profile',jsonb_build_object('email',email,'locale',locale,'createdAt',to_char(created_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')),'preferences',jsonb_build_object('accentColor',accent_color,'reviewReactionAnimationsEnabled',review_reaction_animations_enabled,'analyticsConsent',analytics_consent,'productAnalyticsEnabled',product_analytics_enabled)) FROM org.user_settings WHERE user_id=$1").bind(&user).fetch_optional(&mut *tx).await?.ok_or_else(||ApiError::new(StatusCode::GONE,"ACCOUNT_DELETED","Local account no longer exists"))?;
    value
        .as_object_mut()
        .ok_or_else(ApiError::internal)?
        .insert("csrfToken".into(), json!(identity.csrf_token));
    tx.commit().await?;
    Ok(Json(value))
}

pub(super) async fn preferences(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Expected a JSON object"))?;
    let fields = [
        "accentColor",
        "reviewReactionAnimationsEnabled",
        "analyticsConsent",
        "productAnalyticsEnabled",
        "analyticsConsentOrigin",
        "productAnalyticsEnabledOrigin",
    ];
    if let Some(key) = object.keys().find(|key| !fields.contains(&key.as_str())) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ACCOUNT_PREFERENCES_FIELD_UNKNOWN",
            format!("Unexpected preference field: {key}"),
        ));
    }
    if ![
        "accentColor",
        "reviewReactionAnimationsEnabled",
        "analyticsConsent",
        "productAnalyticsEnabled",
    ]
    .iter()
    .any(|key| object.contains_key(*key))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ACCOUNT_PREFERENCES_FIELD_REQUIRED",
            "At least one preference field is required",
        ));
    }
    let color = object
        .get("accentColor")
        .map(|v| {
            let color = v.as_str().ok_or_else(|| {
                ApiError::bad_request("accentColor must be an opaque RGB color in #RRGGBB format")
            })?;
            if color.len() != 7
                || !color.starts_with('#')
                || !color.bytes().skip(1).all(|c| c.is_ascii_hexdigit())
            {
                return Err(ApiError::bad_request(
                    "accentColor must be an opaque RGB color in #RRGGBB format",
                ));
            }
            Ok(color.to_uppercase())
        })
        .transpose()?;
    let animation = object
        .get("reviewReactionAnimationsEnabled")
        .map(|v| {
            v.as_bool().ok_or_else(|| {
                ApiError::bad_request("reviewReactionAnimationsEnabled must be a boolean")
            })
        })
        .transpose()?;
    let analytics = object
        .get("productAnalyticsEnabled")
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| ApiError::bad_request("productAnalyticsEnabled must be a boolean"))
        })
        .transpose()?;
    let consent = object
        .get("analyticsConsent")
        .map(|v| {
            v.as_str()
                .filter(|s| matches!(*s, "granted" | "declined"))
                .ok_or_else(|| {
                    ApiError::bad_request("analyticsConsent must be granted or declined")
                })
        })
        .transpose()?;
    let origin = |field: &str| -> Result<bool, ApiError> {
        match object.get(field).and_then(Value::as_str) {
            None if !object.contains_key(field) => Ok(false),
            Some("user_action") => Ok(false),
            Some("reconciliation") => Ok(true),
            _ => Err(ApiError::bad_request(format!(
                "{field} must be user_action or reconciliation"
            ))),
        }
    };
    let consent_reconcile = origin("analyticsConsentOrigin")?;
    let analytics_reconcile = origin("productAnalyticsEnabledOrigin")?;
    let mut tx = scoped(&state.pool, &user, None).await?;
    let preferences:Value=sqlx::query_scalar("UPDATE org.user_settings SET review_reaction_animations_enabled=COALESCE($2,review_reaction_animations_enabled),analytics_consent=CASE WHEN $3::text IS NULL THEN analytics_consent WHEN $4 AND $3='granted' AND analytics_consent='declined' THEN analytics_consent ELSE $3 END,product_analytics_enabled=CASE WHEN $5::boolean IS NULL THEN product_analytics_enabled WHEN $6 AND $5 AND product_analytics_enabled IS FALSE THEN product_analytics_enabled ELSE $5 END,accent_color=COALESCE($7,accent_color) WHERE user_id=$1 RETURNING jsonb_build_object('accentColor',accent_color,'reviewReactionAnimationsEnabled',review_reaction_animations_enabled,'analyticsConsent',analytics_consent,'productAnalyticsEnabled',product_analytics_enabled)").bind(&user).bind(animation).bind(consent).bind(consent_reconcile).bind(analytics).bind(analytics_reconcile).bind(color).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"preferences":preferences})))
}

pub(super) async fn delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_mutation(&state, &headers).await?;
    Err(ApiError::new(
        StatusCode::CONFLICT,
        "LOCAL_ACCOUNT_ADMIN_REQUIRED",
        "Manage the local account over SSH.",
    ))
}
