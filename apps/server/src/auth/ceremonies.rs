use super::session::{
    browser_cookies, create_session, error, hash_token, login_browser, secure_headers, set_cookie,
    token_valid, unavailable,
};
use crate::{AppState, config::Config, error::ApiError};
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, Postgres, Transaction};
use std::time::Duration;
use url::Url;
use webauthn_rs_core::{
    WebauthnCore,
    proto::{
        AttestationConveyancePreference, AttestationFormat, AuthenticationState,
        AuthenticatorTransport, COSEAlgorithm, COSEKey, Credential, ParsedAttestation,
        PublicKeyCredential, RegisterPublicKeyCredential, RegisteredExtensions, RegistrationState,
        ResidentKeyRequirement, UserVerificationPolicy,
    },
};

#[derive(FromRow)]
struct Account {
    user_id: String,
    webauthn_user_handle: String,
    retry_after: Option<i64>,
}

#[derive(FromRow)]
struct StoredPasskey {
    credential_id: String,
    public_key: Vec<u8>,
    counter: i64,
    transports: Vec<String>,
    device_type: String,
    backed_up: bool,
}

#[derive(FromRow)]
struct StoredChallenge {
    grant_hash: Option<String>,
    unexpired: bool,
    library_state: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientData {
    challenge: String,
    #[serde(default)]
    cross_origin: bool,
    top_origin: Option<String>,
}

enum Attempt<T> {
    Valid(T),
    Invalid,
    Throttled(i64),
}

fn json_body(body: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    body.map(|Json(value)| value).map_err(|rejection| {
        let status = rejection.status();
        if status == StatusCode::UNSUPPORTED_MEDIA_TYPE {
            error(status, "JSON_REQUIRED", "JSON is required")
        } else if status == StatusCode::PAYLOAD_TOO_LARGE {
            error(status, "BODY_TOO_LARGE", "Request body is too large")
        } else {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid request",
            )
        }
    })
}

fn failure<T>(attempt: Attempt<T>, enrollment: bool) -> Result<T, Box<Response>> {
    match attempt {
        Attempt::Valid(value) => Ok(value),
        Attempt::Invalid => {
            let body = if enrollment {
                json!({"error":"This link has expired. Use a new setup link.","code":"ENROLLMENT_INVALID"})
            } else {
                json!({"error":"Passkey sign-in failed. Try again."})
            };
            let mut response = (StatusCode::UNAUTHORIZED, Json(body)).into_response();
            secure_headers(&mut response);
            Err(Box::new(response))
        }
        Attempt::Throttled(seconds) => {
            let mut response = (StatusCode::TOO_MANY_REQUESTS, Json(json!({"error":format!("Too many attempts. Try again in {seconds} seconds."),"code":"LOGIN_THROTTLED"}))).into_response();
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert("retry-after", value);
            }
            secure_headers(&mut response);
            Err(Box::new(response))
        }
    }
}

fn core(config: &Config) -> Result<WebauthnCore, ApiError> {
    let origin = Url::parse(&config.auth_origin).map_err(unavailable)?;
    // Direct core access is needed to import the existing COSE public-key rows. All ceremonies
    // require UV, exact origin/RP, same-origin client data, current counters and immutable backup eligibility.
    Ok(WebauthnCore::new_unsafe_experts_only(
        "lingvichr",
        &config.rp_id,
        vec![origin],
        Duration::from_mins(1),
        Some(false),
        Some(false),
    ))
}

async fn lock_account(tx: &mut Transaction<'_, Postgres>) -> Result<Option<Account>, ApiError> {
    sqlx::query_as("SELECT user_id,webauthn_user_handle,CASE WHEN locked_until>clock_timestamp() THEN GREATEST(1,CEIL(EXTRACT(EPOCH FROM locked_until-clock_timestamp())))::bigint END AS retry_after FROM auth.local_account WHERE singleton FOR UPDATE")
        .fetch_optional(&mut **tx).await.map_err(unavailable)
}

async fn passkeys(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
) -> Result<Vec<StoredPasskey>, ApiError> {
    sqlx::query_as("SELECT credential_id,public_key,counter,transports,device_type,backed_up FROM auth.local_passkeys WHERE user_id=$1")
        .bind(user).fetch_all(&mut **tx).await.map_err(unavailable)
}

fn credential(key: &StoredPasskey) -> Result<Credential, ApiError> {
    let value: serde_cbor_2::Value =
        serde_cbor_2::from_slice(&key.public_key).map_err(unavailable)?;
    let transports = key
        .transports
        .iter()
        .map(|transport| {
            serde_json::from_value::<AuthenticatorTransport>(json!(transport)).map_err(unavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Credential {
        cred_id: URL_SAFE_NO_PAD
            .decode(&key.credential_id)
            .map_err(unavailable)?
            .into(),
        cred: COSEKey::try_from(&value).map_err(unavailable)?,
        counter: u32::try_from(key.counter).map_err(unavailable)?,
        transports: Some(transports),
        user_verified: true,
        backup_eligible: key.device_type == "multiDevice",
        backup_state: key.backed_up,
        registration_policy: UserVerificationPolicy::Required,
        extensions: RegisteredExtensions::default(),
        attestation: ParsedAttestation::default(),
        attestation_format: AttestationFormat::None,
    })
}

async fn options_limit(
    tx: &mut Transaction<'_, Postgres>,
    account: &Account,
) -> Result<Option<i64>, ApiError> {
    if account.retry_after.is_some() {
        return Ok(account.retry_after);
    }
    sqlx::query("DELETE FROM auth.local_webauthn_challenges WHERE expires_at<=clock_timestamp()")
        .execute(&mut **tx)
        .await
        .map_err(unavailable)?;
    sqlx::query("DELETE FROM auth.local_enrollment_grants WHERE expires_at<=clock_timestamp()")
        .execute(&mut **tx)
        .await
        .map_err(unavailable)?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth.local_webauthn_challenges WHERE user_id=$1")
            .bind(&account.user_id)
            .fetch_one(&mut **tx)
            .await
            .map_err(unavailable)?;
    Ok((count >= 32).then_some(120))
}

async fn save_challenge<T: Serialize + Sync>(
    tx: &mut Transaction<'_, Postgres>,
    account: &Account,
    challenge: &str,
    browser: &str,
    ceremony: &str,
    grant: Option<&str>,
    state: &T,
) -> Result<(), ApiError> {
    sqlx::query("INSERT INTO auth.local_webauthn_challenges (challenge_hash,user_id,browser_hash,ceremony,grant_hash,expires_at,library_state) VALUES ($1,$2,$3,$4,$5,clock_timestamp()+interval '2 minutes',$6)")
        .bind(hash_token(challenge)).bind(&account.user_id).bind(hash_token(browser)).bind(ceremony).bind(grant)
        .bind(serde_json::to_value(state).map_err(unavailable)?).execute(&mut **tx).await.map_err(unavailable)?;
    Ok(())
}

async fn authentication_options_inner(
    state: &AppState,
    browser: &str,
) -> Result<Attempt<Value>, ApiError> {
    let mut tx = state.auth_pool.begin().await.map_err(unavailable)?;
    let Some(account) = lock_account(&mut tx).await? else {
        return Ok(Attempt::Invalid);
    };
    if let Some(seconds) = options_limit(&mut tx, &account).await? {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Throttled(seconds));
    }
    let keys = passkeys(&mut tx, &account.user_id).await?;
    if keys.is_empty() {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Invalid);
    }
    let verifier = core(&state.config)?;
    let credentials = keys.iter().map(credential).collect::<Result<Vec<_>, _>>()?;
    let builder = verifier
        .new_challenge_authenticate_builder(credentials, Some(UserVerificationPolicy::Required))
        .map_err(unavailable)?;
    let (options, library_state) = verifier
        .generate_challenge_authenticate(builder)
        .map_err(unavailable)?;
    let challenge = URL_SAFE_NO_PAD.encode(options.public_key.challenge.as_ref());
    save_challenge(
        &mut tx,
        &account,
        &challenge,
        browser,
        "authentication",
        None,
        &library_state,
    )
    .await?;
    tx.commit().await.map_err(unavailable)?;
    let mut options = serde_json::to_value(options.public_key).map_err(unavailable)?;
    if let Some(object) = options.as_object_mut() {
        object.insert("allowCredentials".to_owned(), json!(keys.iter().map(|key| json!({"id":key.credential_id,"type":"public-key","transports":key.transports})).collect::<Vec<_>>()));
    }
    Ok(Attempt::Valid(options))
}

pub(super) async fn authentication_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let browser = login_browser(&state.config, &headers)?;
    let body = json_body(body)?;
    if !body.is_object() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "Invalid request",
        ));
    }
    match failure(authentication_options_inner(&state, browser).await?, false) {
        Ok(options) => {
            let mut response = Json(options).into_response();
            secure_headers(&mut response);
            Ok(response)
        }
        Err(response) => Ok(*response),
    }
}

async fn grant_valid(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    grant: &str,
) -> Result<bool, ApiError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM auth.local_enrollment_grants WHERE grant_hash=$1 AND user_id=$2 AND expires_at>clock_timestamp())")
        .bind(grant).bind(user).fetch_one(&mut **tx).await.map_err(unavailable)
}

async fn registration_options_inner(
    state: &AppState,
    browser: &str,
    grant: &str,
) -> Result<Attempt<Value>, ApiError> {
    let mut tx = state.auth_pool.begin().await.map_err(unavailable)?;
    let Some(account) = lock_account(&mut tx).await? else {
        return Ok(Attempt::Invalid);
    };
    if let Some(seconds) = options_limit(&mut tx, &account).await? {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Throttled(seconds));
    }
    let grant_hash = hash_token(grant);
    if !grant_valid(&mut tx, &account.user_id, &grant_hash).await? {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Invalid);
    }
    let keys = passkeys(&mut tx, &account.user_id).await?;
    if keys.len() >= 16 {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Invalid);
    }
    let user_handle = URL_SAFE_NO_PAD
        .decode(&account.webauthn_user_handle)
        .map_err(unavailable)?;
    let exclude = keys
        .iter()
        .map(|key| {
            URL_SAFE_NO_PAD
                .decode(&key.credential_id)
                .map(Into::into)
                .map_err(unavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let verifier = core(&state.config)?;
    let builder = verifier
        .new_challenge_register_builder(&user_handle, "Personal", "lingvichr")
        .map_err(unavailable)?
        .attestation(AttestationConveyancePreference::None)
        .user_verification_policy(UserVerificationPolicy::Required)
        .credential_algorithms(vec![COSEAlgorithm::ES256, COSEAlgorithm::RS256])
        .require_resident_key(true)
        .reject_synchronised_authenticators(false)
        .exclude_credentials(Some(exclude));
    let (mut options, library_state) = verifier
        .generate_challenge_register(builder)
        .map_err(unavailable)?;
    if let Some(selection) = options.public_key.authenticator_selection.as_mut() {
        selection.resident_key = Some(ResidentKeyRequirement::Required);
    }
    let challenge = URL_SAFE_NO_PAD.encode(options.public_key.challenge.as_ref());
    save_challenge(
        &mut tx,
        &account,
        &challenge,
        browser,
        "registration",
        Some(&grant_hash),
        &library_state,
    )
    .await?;
    tx.commit().await.map_err(unavailable)?;
    let mut options = serde_json::to_value(options.public_key).map_err(unavailable)?;
    if let Some(object) = options.as_object_mut() {
        object.insert("excludeCredentials".to_owned(), json!(keys.iter().map(|key| json!({"id":key.credential_id,"type":"public-key","transports":key.transports})).collect::<Vec<_>>()));
    }
    Ok(Attempt::Valid(options))
}

pub(super) async fn registration_options(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let browser = login_browser(&state.config, &headers)?;
    let body = json_body(body)?;
    let grant = body
        .get("grant")
        .and_then(Value::as_str)
        .filter(|grant| token_valid(grant))
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid setup request",
            )
        })?;
    match failure(
        registration_options_inner(&state, browser, grant).await?,
        true,
    ) {
        Ok(options) => {
            let mut response = Json(options).into_response();
            secure_headers(&mut response);
            Ok(response)
        }
        Err(response) => Ok(*response),
    }
}

fn client_data(bytes: &[u8]) -> Option<ClientData> {
    let data: ClientData = serde_json::from_slice(bytes).ok()?;
    (!data.challenge.is_empty() && data.challenge.len() <= 128).then_some(data)
}

async fn consume(
    tx: &mut Transaction<'_, Postgres>,
    account: &Account,
    browser: &str,
    data: Option<&ClientData>,
    ceremony: &str,
) -> Result<Option<StoredChallenge>, ApiError> {
    let Some(data) = data else { return Ok(None) };
    sqlx::query_as("DELETE FROM auth.local_webauthn_challenges WHERE challenge_hash=$1 AND browser_hash=$2 AND ceremony=$3 AND user_id=$4 RETURNING grant_hash,expires_at>clock_timestamp() AS unexpired,library_state")
        .bind(hash_token(&data.challenge)).bind(hash_token(browser)).bind(ceremony).bind(&account.user_id)
        .fetch_optional(&mut **tx).await.map_err(unavailable)
}

async fn reject<T>(
    mut tx: Transaction<'_, Postgres>,
    account: &Account,
) -> Result<Attempt<T>, ApiError> {
    let failures: i32 = sqlx::query_scalar("UPDATE auth.local_account SET failed_attempts=CASE WHEN locked_until IS NOT NULL THEN 1 ELSE failed_attempts+1 END,locked_until=CASE WHEN (CASE WHEN locked_until IS NOT NULL THEN 1 ELSE failed_attempts+1 END)>=5 THEN clock_timestamp()+interval '60 seconds' ELSE NULL END WHERE singleton RETURNING failed_attempts")
        .fetch_one(&mut *tx).await.map_err(unavailable)?;
    tx.commit().await.map_err(unavailable)?;
    let _ = account;
    Ok(if failures >= 5 {
        Attempt::Throttled(60)
    } else {
        Attempt::Invalid
    })
}

async fn authentication_verify_inner(
    state: &AppState,
    browser: &str,
    response: PublicKeyCredential,
) -> Result<Attempt<(String, String)>, ApiError> {
    let mut tx = state.auth_pool.begin().await.map_err(unavailable)?;
    let Some(account) = lock_account(&mut tx).await? else {
        return Ok(Attempt::Invalid);
    };
    let data = client_data(response.response.client_data_json.as_ref());
    let challenge = consume(&mut tx, &account, browser, data.as_ref(), "authentication").await?;
    if let Some(seconds) = account.retry_after {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Throttled(seconds));
    }
    if data
        .as_ref()
        .is_none_or(|data| data.cross_origin || data.top_origin.is_some())
    {
        return reject(tx, &account).await;
    }
    let Some(challenge) = challenge.filter(|row| row.unexpired) else {
        return reject(tx, &account).await;
    };
    let Some(library_state) = challenge
        .library_state
        .and_then(|value| serde_json::from_value::<AuthenticationState>(value).ok())
    else {
        return reject(tx, &account).await;
    };
    let expected_handle = URL_SAFE_NO_PAD
        .decode(&account.webauthn_user_handle)
        .map_err(unavailable)?;
    if response
        .get_user_unique_id()
        .is_some_and(|handle| handle != expected_handle)
    {
        return reject(tx, &account).await;
    }
    let keys = passkeys(&mut tx, &account.user_id).await?;
    if !keys.iter().any(|key| key.credential_id == response.id) {
        return reject(tx, &account).await;
    }
    // Reload under the account lock. State saved before another successful assertion has stale counters.
    let mut library_state = library_state;
    library_state
        .set_allowed_credentials(keys.iter().map(credential).collect::<Result<Vec<_>, _>>()?);
    let verifier = core(&state.config)?;
    let Ok(result) = verifier.authenticate_credential(&response, &library_state) else {
        return reject(tx, &account).await;
    };
    if !result.user_verified() {
        return reject(tx, &account).await;
    }
    sqlx::query("UPDATE auth.local_passkeys SET counter=$1,backed_up=$2,last_used_at=clock_timestamp() WHERE credential_id=$3 AND user_id=$4")
        .bind(i64::from(result.counter())).bind(result.backup_state()).bind(&response.id).bind(&account.user_id)
        .execute(&mut *tx).await.map_err(unavailable)?;
    sqlx::query(
        "UPDATE auth.local_account SET failed_attempts=0,locked_until=NULL WHERE singleton",
    )
    .execute(&mut *tx)
    .await
    .map_err(unavailable)?;
    let tokens = create_session(&mut tx, &account.user_id).await?;
    tx.commit().await.map_err(unavailable)?;
    Ok(Attempt::Valid(tokens))
}

fn encoded(value: Option<&Value>, max: usize) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| {
        !text.is_empty()
            && text.len() <= max
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    })
}

fn envelope(body: &Value, registration: bool) -> bool {
    let Some(response) = body.get("response") else {
        return false;
    };
    body.get("type").and_then(Value::as_str) == Some("public-key")
        && encoded(body.get("id"), 1400)
        && body.get("rawId") == body.get("id")
        && body
            .get("clientExtensionResults")
            .is_some_and(Value::is_object)
        && encoded(response.get("clientDataJSON"), 16384)
        && if registration {
            encoded(response.get("attestationObject"), 16384)
                && response.get("transports").is_none_or(|value| {
                    value.as_array().is_some_and(|values| {
                        values.len() <= 10
                            && values.iter().all(|value| {
                                matches!(
                                    value.as_str(),
                                    Some(
                                        "ble"
                                            | "cable"
                                            | "hybrid"
                                            | "internal"
                                            | "nfc"
                                            | "smart-card"
                                            | "usb"
                                    )
                                )
                            })
                    })
                })
        } else {
            encoded(response.get("authenticatorData"), 16384)
                && encoded(response.get("signature"), 16384)
                && response
                    .get("userHandle")
                    .is_none_or(|value| value.is_null() || encoded(Some(value), 128))
        }
}

pub(super) async fn authentication_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let browser = login_browser(&state.config, &headers)?;
    let body = json_body(body)?;
    if !envelope(&body, false) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "Invalid passkey response",
        ));
    }
    let credential: PublicKeyCredential = serde_json::from_value(body).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "Invalid passkey response",
        )
    })?;
    match failure(
        authentication_verify_inner(&state, browser, credential).await?,
        false,
    ) {
        Ok((session, refresh)) => {
            let mut response = Json(json!({"ok":true})).into_response();
            browser_cookies(&mut response, &state.config, &session, &refresh, false)?;
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
        Err(response) => Ok(*response),
    }
}

fn attested_public_key(response: &RegisterPublicKeyCredential) -> Option<Vec<u8>> {
    let value: serde_cbor_2::Value =
        serde_cbor_2::from_slice(response.response.attestation_object.as_ref()).ok()?;
    let serde_cbor_2::Value::Map(map) = value else {
        return None;
    };
    let serde_cbor_2::Value::Bytes(data) =
        map.get(&serde_cbor_2::Value::Text("authData".to_owned()))?
    else {
        return None;
    };
    let length = data.get(53..55)?;
    let length: [u8; 2] = length.try_into().ok()?;
    let offset = 55_usize.checked_add(usize::from(u16::from_be_bytes(length)))?;
    let remaining = data.get(offset..)?;
    let mut decoder = serde_cbor_2::Deserializer::from_slice(remaining);
    let _: serde_cbor_2::Value = Deserialize::deserialize(&mut decoder).ok()?;
    remaining
        .get(..decoder.byte_offset())
        .filter(|bytes| bytes.len() <= 4096)
        .map(<[u8]>::to_vec)
}

async fn registration_verify_inner(
    state: &AppState,
    browser: &str,
    grant: &str,
    response: RegisterPublicKeyCredential,
    transports: Vec<String>,
) -> Result<Attempt<()>, ApiError> {
    let mut tx = state.auth_pool.begin().await.map_err(unavailable)?;
    let Some(account) = lock_account(&mut tx).await? else {
        return Ok(Attempt::Invalid);
    };
    let data = client_data(response.response.client_data_json.as_ref());
    let challenge = consume(&mut tx, &account, browser, data.as_ref(), "registration").await?;
    if let Some(seconds) = account.retry_after {
        tx.commit().await.map_err(unavailable)?;
        return Ok(Attempt::Throttled(seconds));
    }
    if data
        .as_ref()
        .is_none_or(|data| data.cross_origin || data.top_origin.is_some())
    {
        return reject(tx, &account).await;
    }
    let grant_hash = hash_token(grant);
    let Some(challenge) = challenge
        .filter(|row| row.unexpired && row.grant_hash.as_deref() == Some(grant_hash.as_str()))
    else {
        return reject(tx, &account).await;
    };
    if !grant_valid(&mut tx, &account.user_id, &grant_hash).await?
        || passkeys(&mut tx, &account.user_id).await?.len() >= 16
    {
        return reject(tx, &account).await;
    }
    let Some(library_state) = challenge
        .library_state
        .and_then(|value| serde_json::from_value::<RegistrationState>(value).ok())
    else {
        return reject(tx, &account).await;
    };
    let verifier = core(&state.config)?;
    let Ok(key) = verifier.register_credential(&response, &library_state, None) else {
        return reject(tx, &account).await;
    };
    if !key.user_verified || URL_SAFE_NO_PAD.encode(key.cred_id.as_ref()) != response.id {
        return reject(tx, &account).await;
    }
    let Some(public_key) = attested_public_key(&response) else {
        return reject(tx, &account).await;
    };
    let inserted = sqlx::query("INSERT INTO auth.local_passkeys (credential_id,user_id,public_key,counter,transports,device_type,backed_up) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING")
        .bind(&response.id).bind(&account.user_id).bind(public_key).bind(i64::from(key.counter)).bind(transports)
        .bind(if key.backup_eligible { "multiDevice" } else { "singleDevice" }).bind(key.backup_state).execute(&mut *tx).await.map_err(unavailable)?;
    if inserted.rows_affected() != 1 {
        return reject(tx, &account).await;
    }
    sqlx::query("DELETE FROM auth.local_enrollment_grants WHERE grant_hash=$1")
        .bind(&grant_hash)
        .execute(&mut *tx)
        .await
        .map_err(unavailable)?;
    sqlx::query(
        "UPDATE auth.local_account SET failed_attempts=0,locked_until=NULL WHERE singleton",
    )
    .execute(&mut *tx)
    .await
    .map_err(unavailable)?;
    tx.commit().await.map_err(unavailable)?;
    Ok(Attempt::Valid(()))
}

pub(super) async fn registration_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, ApiError> {
    let browser = login_browser(&state.config, &headers)?;
    let body = json_body(body)?;
    let grant = body
        .get("grant")
        .and_then(Value::as_str)
        .filter(|grant| token_valid(grant))
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid setup request",
            )
        })?;
    let credential = body
        .get("credential")
        .filter(|value| envelope(value, true))
        .ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid passkey response",
            )
        })?;
    let transports = credential
        .get("response")
        .and_then(|response| response.get("transports"))
        .and_then(Value::as_array)
        .map(|transports| {
            transports
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let credential: RegisterPublicKeyCredential = serde_json::from_value(credential.clone())
        .map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                "INVALID_REQUEST",
                "Invalid passkey response",
            )
        })?;
    match failure(
        registration_verify_inner(&state, browser, grant, credential, transports).await?,
        true,
    ) {
        Ok(()) => {
            let mut response = Json(json!({"ok":true})).into_response();
            secure_headers(&mut response);
            Ok(response)
        }
        Err(response) => Ok(*response),
    }
}
