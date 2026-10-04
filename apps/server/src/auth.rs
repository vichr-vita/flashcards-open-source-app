mod admin;
mod ceremonies;
mod page;
mod session;

pub use admin::{AccountCommand, account_command};
pub use session::{Identity, authenticate, require_mutation};

use crate::AppState;
use axum::{
    Router, middleware,
    routing::{get, post},
};

/// Local browser authentication keeps the deployed HTTP endpoints and opaque cookie contract.
#[must_use = "The authentication routes must be mounted on the server"]
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/login", get(page::login))
        .route("/enroll", get(page::enroll))
        .route("/logout", get(session::logout))
        .route("/logout-local", get(session::logout_local))
        .route(
            "/robots.txt",
            get(|| async { "User-agent: *\nDisallow: /\n" }),
        )
        .route("/api/refresh-session", post(session::refresh))
        .route(
            "/api/webauthn/authentication/options",
            post(ceremonies::authentication_options),
        )
        .route(
            "/api/webauthn/authentication/verify",
            post(ceremonies::authentication_verify),
        )
        .route(
            "/api/webauthn/registration/options",
            post(ceremonies::registration_options),
        )
        .route(
            "/api/webauthn/registration/verify",
            post(ceremonies::registration_verify),
        )
        .layer(axum::extract::DefaultBodyLimit::max(32_768))
        .layer(middleware::from_fn(
            |request: axum::extract::Request, next: middleware::Next| async move {
                let mut response = next.run(request).await;
                session::secure_headers(&mut response);
                response
            },
        ))
}
