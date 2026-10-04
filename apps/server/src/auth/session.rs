use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

use crate::{AppState, config::Config, error::ApiError};

#[derive(Clone, Debug)]
pub struct Identity {
    pub user_id: Uuid,
    pub session_id: Uuid,
    pub csrf_token: String,
}

pub(super) fn error(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> ApiError {
    ApiError::new(status, code, message)
}

pub(super) fn unavailable(_: impl std::fmt::Display) -> ApiError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "AUTH_UNAVAILABLE",
        "Authentication is temporarily unavailable. Try again.",
    )
}

pub(super) fn token_valid(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

pub(super) fn new_token() -> Result<String, rand::Error> {
    let mut bytes = [0_u8; 32];
    OsRng.try_fill_bytes(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

pub(super) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|entry| {
            let (key, value) = entry.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

pub(super) fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn csrf_token(token: &str, config: &Config) -> Result<String, ApiError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(config.csrf_secret.as_bytes()).map_err(unavailable)?;
    mac.update(token.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// The security-definer function supports the existing backend role without credential-table access.
///
/// # Errors
/// Refuses absent, expired or revoked session cookies and alternate authorization schemes.
/// Returns an availability error when the database or the CSRF configuration cannot be used.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Identity, ApiError> {
    if header_text(headers, "authorization").is_some() {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "LOCAL_SESSION_REQUIRED",
            "Sign in again.",
        ));
    }
    let token = cookie(headers, "session")
        .filter(|value| token_valid(value))
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "LOCAL_SESSION_REQUIRED",
                "Sign in again.",
            )
        })?;
    let user_id = sqlx::query_scalar::<_, Option<String>>("SELECT auth.verify_local_session($1)")
        .bind(hash_token(token))
        .fetch_one(&state.pool)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "LOCAL_SESSION_REQUIRED",
                "Sign in again.",
            )
        })?;
    let digest = Sha256::digest(token.as_bytes());
    let session_bytes = digest
        .get(..16)
        .ok_or_else(|| unavailable("session digest"))?;
    Ok(Identity {
        user_id: Uuid::parse_str(&user_id).map_err(unavailable)?,
        session_id: Uuid::from_slice(session_bytes).map_err(unavailable)?,
        csrf_token: csrf_token(token, &state.config)?,
    })
}

pub(super) fn allowed_origin(
    config: &Config,
    headers: &HeaderMap,
    auth_only: bool,
) -> Result<(), ApiError> {
    if header_text(headers, "sec-fetch-site")
        .is_some_and(|site| site.eq_ignore_ascii_case("cross-site"))
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "ORIGIN_NOT_ALLOWED",
            "Cross-site browser requests are not allowed",
        ));
    }
    let origin = if let Some(origin) = header_text(headers, "origin") {
        origin.to_owned()
    } else if !auth_only {
        let referer = header_text(headers, "referer").ok_or_else(|| {
            error(
                StatusCode::FORBIDDEN,
                "ORIGIN_NOT_ALLOWED",
                "Missing Origin or Referer header",
            )
        })?;
        Url::parse(referer)
            .map_err(|_| {
                error(
                    StatusCode::FORBIDDEN,
                    "ORIGIN_NOT_ALLOWED",
                    "Invalid Referer header",
                )
            })?
            .origin()
            .ascii_serialization()
    } else {
        return Err(error(
            StatusCode::FORBIDDEN,
            "ORIGIN_NOT_ALLOWED",
            "Origin is not allowed",
        ));
    };
    if origin != config.auth_origin && (auth_only || !config.allowed_origins.contains(&origin)) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "ORIGIN_NOT_ALLOWED",
            "Origin is not allowed for session request",
        ));
    }
    Ok(())
}

/// Verify a session and the existing browser Origin and HMAC CSRF contract before a mutation.
///
/// # Errors
/// Returns authentication errors from `authenticate`, or refuses an untrusted origin,
/// cross-site request, missing CSRF header or token that does not match the current session.
pub async fn require_mutation(state: &AppState, headers: &HeaderMap) -> Result<Identity, ApiError> {
    let identity = authenticate(state, headers).await?;
    allowed_origin(&state.config, headers, false)?;
    let supplied = header_text(headers, "x-csrf-token").ok_or_else(|| {
        error(
            StatusCode::FORBIDDEN,
            "CSRF_REQUIRED",
            "Missing X-CSRF-Token header",
        )
    })?;
    if !bool::from(identity.csrf_token.as_bytes().ct_eq(supplied.as_bytes())) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "SESSION_CSRF_TOKEN_INVALID",
            "Invalid X-CSRF-Token header",
        ));
    }
    Ok(identity)
}

pub(super) fn login_browser<'a>(
    config: &Config,
    headers: &'a HeaderMap,
) -> Result<&'a str, ApiError> {
    allowed_origin(config, headers, true)?;
    let browser = cookie(headers, "local_login_csrf").filter(|token| token_valid(token));
    let supplied = header_text(headers, "x-csrf-token");
    match (browser, supplied) {
        (Some(browser), Some(supplied))
            if bool::from(browser.as_bytes().ct_eq(supplied.as_bytes())) =>
        {
            Ok(browser)
        }
        _ => Err(error(
            StatusCode::FORBIDDEN,
            "LOGIN_CSRF_INVALID",
            "Sign-in page expired. Reload to try again.",
        )),
    }
}

pub(super) fn secure_headers(response: &mut Response) {
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-robots-tag", "noindex, nofollow, noarchive"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        (
            "permissions-policy",
            "publickey-credentials-get=(self), publickey-credentials-create=(self)",
        ),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
}

pub(super) fn redirect_response(url: &Url) -> Result<Response, ApiError> {
    let mut response = StatusCode::FOUND.into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(url.as_str()).map_err(unavailable)?,
    );
    Ok(response)
}

pub(super) fn set_cookie(
    response: &mut Response,
    config: &Config,
    name: &str,
    token: &str,
    http_only: bool,
    login: bool,
    clear: bool,
) -> Result<(), ApiError> {
    let secure = if config.allow_http { "" } else { "; Secure" };
    let private = if http_only { "; HttpOnly" } else { "" };
    let domain = if login || config.cookie_domain.is_empty() {
        String::new()
    } else {
        format!("; Domain={}", config.cookie_domain)
    };
    let same_site = if login { "Strict" } else { "Lax" };
    let max_age = if clear {
        0
    } else if login {
        600
    } else {
        2_592_000
    };
    let value = format!(
        "{name}={token}; Path=/; Max-Age={max_age}; SameSite={same_site}{secure}{private}{domain}"
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(&value).map_err(unavailable)?,
    );
    Ok(())
}

pub(super) fn browser_cookies(
    response: &mut Response,
    config: &Config,
    session: &str,
    refresh: &str,
    clear: bool,
) -> Result<(), ApiError> {
    set_cookie(response, config, "session", session, true, false, clear)?;
    set_cookie(response, config, "refresh", refresh, true, false, clear)?;
    set_cookie(
        response,
        config,
        "logged_in",
        if clear { "" } else { "1" },
        false,
        false,
        clear,
    )
}

pub(super) async fn create_session(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: &str,
) -> Result<(String, String), ApiError> {
    let session = new_token().map_err(unavailable)?;
    let refresh = new_token().map_err(unavailable)?;
    sqlx::query("DELETE FROM auth.local_sessions WHERE refresh_expires_at <= clock_timestamp()")
        .execute(&mut **tx)
        .await
        .map_err(unavailable)?;
    sqlx::query("INSERT INTO auth.local_sessions (session_hash, refresh_hash, user_id, expires_at, refresh_expires_at) VALUES ($1,$2,$3,clock_timestamp()+interval '15 minutes',clock_timestamp()+interval '30 days')")
        .bind(hash_token(&session)).bind(hash_token(&refresh)).bind(user_id).execute(&mut **tx).await.map_err(unavailable)?;
    Ok((session, refresh))
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    allowed_origin(&state.config, &headers, true).or_else(|_| {
        // Refresh is also requested by the same-site app, unlike the passkey ceremonies.
        if header_text(&headers, "origin").is_none() {
            return Err(error(
                StatusCode::FORBIDDEN,
                "ORIGIN_NOT_ALLOWED",
                "Origin is not allowed",
            ));
        }
        allowed_origin(&state.config, &headers, false)
    })?;
    let session = cookie(&headers, "session").filter(|token| token_valid(token));
    let refresh = cookie(&headers, "refresh").filter(|token| token_valid(token));
    let valid = if let (Some(session), Some(refresh)) = (session, refresh) {
        sqlx::query("UPDATE auth.local_sessions SET expires_at=LEAST(clock_timestamp()+interval '15 minutes',refresh_expires_at) WHERE session_hash=$1 AND refresh_hash=$2 AND refresh_expires_at>clock_timestamp()")
            .bind(hash_token(session)).bind(hash_token(refresh)).execute(&state.auth_pool).await.map_err(unavailable)?.rows_affected() == 1
    } else {
        false
    };
    let mut response = if valid {
        Json(json!({"ok":true})).into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Sign in again."})),
        )
            .into_response()
    };
    browser_cookies(
        &mut response,
        &state.config,
        session.unwrap_or_default(),
        refresh.unwrap_or_default(),
        !valid,
    )?;
    secure_headers(&mut response);
    Ok(response)
}

#[derive(Deserialize)]
pub(super) struct RedirectQuery {
    pub redirect_uri: Option<String>,
}

pub(super) fn redirect_url(config: &Config, value: Option<&str>) -> Result<Url, ApiError> {
    let url = value
        .and_then(|value| Url::parse(value).ok())
        .filter(|url| {
            url.username().is_empty()
                && url.password().is_none()
                && config
                    .allowed_origins
                    .contains(&url.origin().ascii_serialization())
        })
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REDIRECT",
                "Invalid redirect_uri",
            )
        })?;
    Ok(url)
}

async fn revoke(pool: &PgPool, headers: &HeaderMap) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM auth.local_sessions WHERE session_hash=$1 OR refresh_hash=$2")
        .bind(hash_token(cookie(headers, "session").unwrap_or_default()))
        .bind(hash_token(cookie(headers, "refresh").unwrap_or_default()))
        .execute(pool)
        .await
        .map_err(unavailable)?;
    Ok(())
}

async fn logout_response(
    state: &AppState,
    headers: &HeaderMap,
    query: &RedirectQuery,
    deleted: bool,
) -> Result<Response, ApiError> {
    let mut url = redirect_url(&state.config, query.redirect_uri.as_deref())?;
    allowed_origin(&state.config, headers, false)?;
    revoke(&state.auth_pool, headers).await?;
    url.query_pairs_mut().append_pair("logged_out", "1");
    if deleted {
        url.query_pairs_mut().append_pair("account_deleted", "1");
    }
    let mut response = redirect_response(&url)?;
    browser_cookies(&mut response, &state.config, "", "", true)?;
    set_cookie(
        &mut response,
        &state.config,
        "local_login_csrf",
        "",
        true,
        true,
        true,
    )?;
    secure_headers(&mut response);
    Ok(response)
}

pub(super) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedirectQuery>,
) -> Result<Response, ApiError> {
    logout_response(&state, &headers, &query, false).await
}

pub(super) async fn logout_local(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedirectQuery>,
) -> Result<Response, ApiError> {
    logout_response(&state, &headers, &query, true).await
}
