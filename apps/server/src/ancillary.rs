//! Browser package, community, and optional-service contracts retained by the private installation.

mod community;
mod media;
mod packages;

use crate::AppState;
use axum::Router;

/// Routes for the existing browser HTTP/JSON contracts.
pub fn router() -> Router<AppState> {
    packages::router()
        .merge(community::router())
        .merge(media::router())
}
