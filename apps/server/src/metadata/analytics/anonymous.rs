use super::{contract, properties, utc};
use crate::{AppState, error::ApiError};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use chrono::{Duration, Utc};
use icu_locale::{Locale, LocaleCanonicalizer};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    event_id: Uuid,
    event_name: String,
    client_occurred_at: String,
    client_sent_at: String,
    anonymous_id: Option<Uuid>,
    ui_locale: Option<String>,
    device_locale: Option<String>,
    screen: Option<String>,
    properties: Option<Value>,
}
pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/analytics/anonymous-events", post(collect))
        .route("/v1/analytics/catalog-install-events", post(collect))
        .layer(DefaultBodyLimit::max(8192))
}
fn invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "ANONYMOUS_ANALYTICS_INVALID_EVENT",
        "Anonymous analytics event does not match the collector contract.",
    )
}
fn locale(value: Option<String>) -> Result<Option<String>, ApiError> {
    value
        .map(|value| {
            if value.trim() != value
                || value.is_empty()
                || value.contains('_')
                || value.encode_utf16().count() > 64
            {
                return Err(invalid());
            }
            let mut locale: Locale = value.parse().map_err(|_| invalid())?;
            LocaleCanonicalizer::new_extended().canonicalize(&mut locale);
            let value = locale.to_string();
            if value.len() > 64 {
                return Err(invalid());
            }
            Ok(value)
        })
        .transpose()
}
fn automated(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
    else {
        return true;
    };
    let value = value.to_lowercase().replace("cubot", " ");
    [
        "bot",
        "crawl",
        "spider",
        "slurp",
        "headless",
        "playwright",
        "puppeteer",
        "selenium",
        "webdriver",
        "lighthouse",
        "phantomjs",
        "curl",
        "wget",
        "python-requests",
        "python-urllib",
        "aiohttp",
        "httpx",
        "node-fetch",
        "undici",
        "axios",
        "go-http-client",
        "okhttp",
        "java/",
        "libwww",
        "scrapy",
        "facebookexternalhit",
        "embedly",
    ]
    .iter()
    .any(|token| value.contains(token))
}
fn origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let value = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Url::parse(v).ok())
        .map(|v| v.origin().ascii_serialization());
    if value.is_none_or(|value| {
        !state.config.allowed_origins.contains(&value) && value != state.config.auth_origin
    }) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "ANONYMOUS_ANALYTICS_ORIGIN_NOT_ALLOWED",
            "Origin is not allowed for anonymous analytics.",
        ));
    }
    Ok(())
}
async fn collect(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    origin(&state, &headers)?;
    let mut input: Input = serde_json::from_value(body).map_err(|_| invalid())?;
    let catalog = contract()?;
    let spec = catalog
        .events
        .get(&input.event_name)
        .filter(|spec| !spec.server_only)
        .ok_or_else(invalid)?;
    if input.event_id.get_version_num() != 7
        || spec.requires_screen && input.screen.is_none()
        || input
            .screen
            .as_ref()
            .is_some_and(|value| !catalog.surfaces.contains(value))
    {
        return Err(invalid());
    }
    if spec.identity_free && input.anonymous_id.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ANONYMOUS_ANALYTICS_IDENTITY_NOT_ALLOWED",
            "This anonymous event must carry no identity.",
        ));
    }
    if let Some(values) = input.properties.as_mut().and_then(Value::as_object_mut) {
        for key in ["install_journey_id", "package_version_id"] {
            if let Some(value) = values
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_lowercase)
            {
                values.insert(key.into(), json!(value));
            }
        }
    }
    let properties = properties(input.properties.as_ref(), spec).map_err(|_| invalid())?;
    let anonymous = input.anonymous_id.or_else(|| {
        properties
            .get("install_journey_id")
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok())
    });
    if spec.identity_free && anonymous.is_some() {
        return Err(invalid());
    }
    let now = Utc::now();
    let sent = utc(&input.client_sent_at).ok_or_else(invalid)?;
    let occurred = utc(&input.client_occurred_at).ok_or_else(invalid)?;
    let elapsed = sent.signed_duration_since(occurred);
    if elapsed < Duration::zero() || elapsed > Duration::days(30) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "ANONYMOUS_ANALYTICS_EVENT_TIME_INVALID",
            "Anonymous analytics clientOccurredAt is outside the accepted clock window.",
        ));
    }
    let corrected = now.checked_sub_signed(elapsed).ok_or_else(invalid)?;
    let ui = locale(input.ui_locale)?;
    let device = locale(input.device_locale)?;
    // The private Node service supplies no verified direct source IP or GeoLite country; keep both absent.
    sqlx::query("INSERT INTO analytics.product_events(event_id,schema_version,event_name,origin,client_occurred_at,client_sent_at,server_received_at,occurred_at,trust_level,anonymous_id,platform,ui_locale,device_locale,screen,event_properties,experiment_assignments,request_id,automated_client) VALUES($1,1,$2,'client',$3,$4,$5,$6,'anonymous_client',$7,'web',$8,$9,$10,$11,'{}'::jsonb,$12,$13) ON CONFLICT(event_id) DO NOTHING")
        .bind(input.event_id).bind(&input.event_name).bind(occurred).bind(sent).bind(now).bind(corrected).bind(anonymous).bind(ui).bind(device).bind(input.screen).bind(properties).bind(headers.get("x-request-id").and_then(|v|v.to_str().ok())).bind(automated(&headers)).execute(&state.pool).await?;
    Ok(Json(json!({"accepted":true})))
}
