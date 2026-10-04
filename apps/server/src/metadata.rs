//! Feedback, analytics consent, and published catalog contracts for the private browser.
mod analytics;
mod catalog;
mod feedback;
pub(crate) use analytics::server::{ServerFact, server_fact};

use crate::{AppState, error::ApiError};
use axum::{
    Json, Router,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[must_use = "The metadata routes must be mounted on the server"]
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(feedback::router())
        .merge(analytics::router())
        .merge(catalog::router())
        .route("/v1/analytics/visitor", get(visitor).post(visitor_consent))
}

fn allowed_browser(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(header::REFERER)
                .and_then(|value| value.to_str().ok())
        });
    let origin = origin
        .and_then(|value| Url::parse(value).ok())
        .map(|url| url.origin().ascii_serialization());
    if origin
        .as_ref()
        .is_some_and(|origin| state.config.allowed_origins.contains(origin))
    {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "Origin is not allowed for the analytics visitor identity",
        ))
    }
}

fn visitor_id(headers: &HeaderMap) -> Option<Uuid> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            (name == "analytics_visitor")
                .then(|| Uuid::parse_str(value.trim()).ok())
                .flatten()
        })
}

fn visitor_response(
    state: &AppState,
    consent: bool,
    id: Option<Uuid>,
    clear: bool,
) -> Result<Response, ApiError> {
    let mut response = Json(json!({"consentRequired":consent,"visitorId":id})).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if id.is_some() || clear {
        let max_age = if clear { 0 } else { 34_128_000 };
        let value = id.map_or_else(String::new, |id| id.to_string());
        let secure = if state.config.allow_http {
            ""
        } else {
            "; Secure"
        };
        let cookie = format!(
            "analytics_visitor={value}; Domain={}; Path=/; Max-Age={max_age}; SameSite=Lax{secure}",
            state.config.cookie_domain
        );
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(|_| ApiError::internal())?,
        );
    }
    Ok(response)
}

async fn visitor(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    allowed_browser(&state, &headers)?;
    let id = visitor_id(&headers);
    // This private installation has no GeoLite source; preserve the deployed unresolved-country rule.
    visitor_response(&state, id.is_none(), id, false)
}

async fn visitor_consent(
    axum::extract::State(state): axum::extract::State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    allowed_browser(&state, &headers)?;
    let granted = body
        .get("granted")
        .and_then(Value::as_bool)
        .ok_or_else(|| ApiError::bad_request("granted must be a boolean"))?;
    let id = granted.then(|| visitor_id(&headers).unwrap_or_else(Uuid::new_v4));
    visitor_response(&state, true, id, !granted)
}

fn invalid_feedback() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "FEEDBACK_INVALID_INPUT",
        "Feedback request is invalid.",
    )
}
