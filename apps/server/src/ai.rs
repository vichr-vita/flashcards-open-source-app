//! Browser AI settings, durable conversations and the private machine-tool surface.
mod admin;
mod attachments;
mod chat;
mod connection;
mod live;
mod mcp;
mod output;
mod suggestions;
mod tools;
mod transcriptions;
mod worker;

pub use admin::{AgentKeyCommand, agent_key_command};

use crate::{AppState, auth, error::ApiError};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, header},
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use serde_json::Value;

pub(crate) async fn entitlement(state: &AppState, user: uuid::Uuid) -> Result<Value, ApiError> {
    tools::entitlement(state, user).await
}

#[must_use = "The AI routes must be mounted on the server"]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/ai/settings", get(settings).post(update_settings))
        .route("/v1/ai/settings/chatgpt/start", post(start_login))
        .route("/v1/chat/new", post(chat::new))
        .route("/v1/chat", get(chat::history).post(chat::start))
        .route("/v1/chat/stop", post(chat::stop))
        .route("/v1/chat/live", get(live::stream))
        .route("/v1/chat/transcriptions", post(transcriptions::transcribe))
        .merge(mcp::router())
        .merge(tools::router())
}

fn private_json(value: Value) -> Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn settings(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let identity = auth::authenticate(&state, &headers).await?;
    Ok(private_json(
        connection::settings(&state.config, identity.user_id).await?,
    ))
}

async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    connection::update(&state.config, identity.user_id, &body).await?;
    Ok(private_json(
        connection::settings(&state.config, identity.user_id).await?,
    ))
}

async fn start_login(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    connection::start(&state.config, identity.user_id).await?;
    Ok(private_json(
        connection::settings(&state.config, identity.user_id).await?,
    ))
}
