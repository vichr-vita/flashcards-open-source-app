use super::session::{
    RedirectQuery, authenticate, new_token, redirect_response, redirect_url, secure_headers,
    set_cookie, unavailable,
};
use crate::{AppState, error::ApiError};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue},
    response::{Html, IntoResponse, Response},
};
use serde_json::json;

async fn render(
    state: &AppState,
    headers: &HeaderMap,
    query: &RedirectQuery,
    enrollment: bool,
) -> Result<Response, ApiError> {
    let default = enrollment
        .then(|| state.config.allowed_origins.first())
        .flatten()
        .map(String::as_str);
    let redirect = redirect_url(&state.config, query.redirect_uri.as_deref().or(default))?;
    if !enrollment {
        match authenticate(state, headers).await {
            Ok(_) => {
                let mut response = redirect_response(&redirect)?;
                secure_headers(&mut response);
                return Ok(response);
            }
            Err(error) if error.status == axum::http::StatusCode::UNAUTHORIZED => {}
            Err(error) => return Err(error),
        }
    }
    let browser = new_token().map_err(unavailable)?;
    let nonce = new_token().map_err(unavailable)?;
    let config = serde_json::to_string(
        &json!({"csrfToken": browser, "redirectUri": redirect.as_str(), "enrollment": enrollment}),
    )
    .map_err(unavailable)?
    .replace('<', "\\u003c");
    // This template is copied verbatim from the deployed loginPage.ts, including styles and copy.
    let html = include_str!("login.html")
        .replace(
            "{{TITLE}}",
            if enrollment {
                "Set up a passkey"
            } else {
                "Sign in"
            },
        )
        .replace(
            "{{BUTTON}}",
            if enrollment {
                "Create passkey"
            } else {
                "Sign in with passkey"
            },
        )
        .replace("{{NONCE}}", &nonce)
        .replace("{{CONFIG}}", &config);
    let mut response = Html(html).into_response();
    set_cookie(
        &mut response,
        &state.config,
        "local_login_csrf",
        &browser,
        true,
        true,
        false,
    )?;
    let csp = format!(
        "default-src 'none'; img-src data:; style-src 'nonce-{nonce}'; script-src 'nonce-{nonce}'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'"
    );
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_str(&csp).map_err(unavailable)?,
    );
    secure_headers(&mut response);
    Ok(response)
}

pub(super) async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedirectQuery>,
) -> Result<Response, ApiError> {
    render(&state, &headers, &query, false).await
}

pub(super) async fn enroll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedirectQuery>,
) -> Result<Response, ApiError> {
    render(&state, &headers, &query, true).await
}
