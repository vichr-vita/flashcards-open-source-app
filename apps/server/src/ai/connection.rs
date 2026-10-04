use crate::{config::Config, error::ApiError};
use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use reqwest::{Client, Response, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    env,
    os::unix::fs::{DirBuilderExt as _, MetadataExt as _},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::Mutex,
};
use uuid::Uuid;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const ISSUER: &str = "https://auth.openai.com";
const CODEX: &str = "https://chatgpt.com/backend-api/codex";
const VERIFY_URL: &str = "https://auth.openai.com/codex/device";
const FILE_LIMIT: u64 = 1_048_576;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Model {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub supported_reasoning_efforts: Option<Vec<String>>,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection {
    id: Uuid,
    account_id: String,
    email: Option<String>,
    plan: Option<String>,
    id_token: String,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    models: Vec<Model>,
    model_id: String,
    #[serde(default)]
    reasoning_effort: Option<String>,
    #[serde(skip)]
    legacy_catalog: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    user_id: Uuid,
    provider: Provider,
    connection: Option<Connection>,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Provider {
    Api,
    Chatgpt,
}

#[derive(Clone)]
pub(super) struct Reference {
    pub connection_id: Uuid,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
}

struct Pending {
    user_id: Uuid,
    device_id: String,
    user_code: String,
    expires_at: i64,
    next_poll_at: i64,
    interval_ms: i64,
}

#[derive(Default)]
struct LoginRuntime {
    pending: Option<Pending>,
    error: Option<String>,
}

// The private server owns one connection. Keep refresh, polling and disconnect in one lock.
static LOGIN: OnceLock<Mutex<LoginRuntime>> = OnceLock::new();
static HTTP: OnceLock<Client> = OnceLock::new();

fn error(status: StatusCode, code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(status, code, message)
}

fn storage_error() -> ApiError {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "CHATGPT_STORAGE_UNAVAILABLE",
        "Cannot read or save the private ChatGPT connection file.",
    )
}

fn path(config: &Config) -> Result<PathBuf, ApiError> {
    config
        .chatgpt_connection_dir
        .as_ref()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("connection.json"))
        .ok_or_else(|| {
            error(
                StatusCode::CONFLICT,
                "CHATGPT_NOT_CONFIGURED",
                "Set CHATGPT_CONNECTION_DIR to a private absolute directory on this server.",
            )
        })
}

fn fixture(config: &Config) -> bool {
    config.allow_http
        && env::var("NODE_ENV").is_ok_and(|value| value == "development")
        && env::var("LOCAL_CHATGPT_FIXTURE").is_ok_and(|value| value == "true")
}

pub(super) fn codex_url(config: &Config, suffix: &str) -> String {
    if fixture(config) {
        format!("http://127.0.0.1:19402/backend-api/codex{suffix}")
    } else {
        format!("{CODEX}{suffix}")
    }
}

pub(super) fn api_url(config: &Config, suffix: &str) -> String {
    if fixture(config) {
        format!("http://127.0.0.1:19402/v1{suffix}")
    } else {
        format!("https://api.openai.com/v1{suffix}")
    }
}

pub(super) fn client() -> Result<Client, ApiError> {
    if let Some(client) = HTTP.get() {
        return Ok(client.clone());
    }
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(150))
        .build()
        .map_err(|_| ApiError::internal())?;
    let _ = HTTP.set(client.clone());
    Ok(client)
}

fn no_follow() -> Result<i32, ApiError> {
    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).map_err(|_| storage_error())
}

fn private(metadata: &std::fs::Metadata) -> bool {
    metadata.mode().trailing_zeros() >= 6 && metadata.uid() == rustix::process::geteuid().as_raw()
}

async fn read(config: &Config, user: Uuid) -> Result<Saved, ApiError> {
    let file = match tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(no_follow()?)
        .open(path(config)?)
        .await
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Saved {
                user_id: user,
                provider: Provider::Api,
                connection: None,
            });
        }
        Err(_) => return Err(storage_error()),
    };
    let metadata = file.metadata().await.map_err(|_| storage_error())?;
    if !metadata.is_file() || !private(&metadata) || metadata.len() > FILE_LIMIT {
        return Err(storage_error());
    }
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT.saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| storage_error())?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHATGPT_STORAGE_INVALID",
            "The private ChatGPT connection file is invalid.",
        )
    })?;
    let mut saved: Saved = serde_json::from_value(value.clone()).map_err(|_| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "CHATGPT_STORAGE_INVALID",
            "The private ChatGPT connection file is invalid.",
        )
    })?;
    if saved.user_id != user {
        return Err(error(
            StatusCode::FORBIDDEN,
            "CHATGPT_OWNER_MISMATCH",
            "This server's ChatGPT connection belongs to another account.",
        ));
    }
    if let Some(connection) = &mut saved.connection {
        if connection.account_id.is_empty()
            || connection.id_token.is_empty()
            || connection.access_token.is_empty()
            || connection.refresh_token.is_empty()
        {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "CHATGPT_STORAGE_INVALID",
                "The private ChatGPT connection file is invalid.",
            ));
        }
        connection.legacy_catalog = value
            .get("connection")
            .is_some_and(|connection| connection.get("reasoningEffort").is_none())
            || connection
                .models
                .iter()
                .any(|model| model.supported_reasoning_efforts.is_none());
    }
    Ok(saved)
}

async fn prepare_directory(directory: &Path) -> Result<(), ApiError> {
    let directory = directory.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || !private(&metadata) {
            return Err(std::io::Error::other("Invalid private directory"));
        }
        Ok(())
    })
    .await
    .map_err(|_| storage_error())?
    .map_err(|_| storage_error())
}

async fn save(config: &Config, saved: &Saved) -> Result<(), ApiError> {
    let path = path(config)?;
    let directory = path.parent().ok_or_else(storage_error)?;
    prepare_directory(directory).await?;
    let temporary = directory.join(format!("connection.json.{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec(saved).map_err(|_| storage_error())?;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temporary, &path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(storage_error());
    }
    Ok(())
}

async fn post(
    config: &Config,
    suffix: &str,
    body: &Value,
    form: bool,
) -> Result<Response, ApiError> {
    let base = if fixture(config) {
        "http://127.0.0.1:19402"
    } else {
        ISSUER
    };
    let request = client()?
        .post(format!("{base}{suffix}"))
        .timeout(Duration::from_secs(15));
    let request = if form {
        request.form(body)
    } else {
        request.json(body)
    };
    request.send().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_AUTH_UNAVAILABLE",
            "Cannot reach ChatGPT sign-in. Try again.",
        )
    })
}

async fn response_json(response: Response) -> Result<Value, ApiError> {
    if response
        .content_length()
        .is_some_and(|length| length > FILE_LIMIT)
    {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_RESPONSE_INVALID",
            "ChatGPT returned an invalid response. Try again.",
        ));
    }
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_RESPONSE_INVALID",
            "ChatGPT returned an invalid response. Try again.",
        )
    })? {
        bytes.extend_from_slice(&chunk);
        if u64::try_from(bytes.len()).map_err(|_| ApiError::internal())? > FILE_LIMIT {
            return Err(error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_RESPONSE_INVALID",
                "ChatGPT returned an invalid response. Try again.",
            ));
        }
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_RESPONSE_INVALID",
            "ChatGPT returned an invalid response. Try again.",
        )
    })
}

fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_RESPONSE_INVALID",
                "ChatGPT returned an invalid response. Try again.",
            )
        })
}

fn claims(token: &str) -> Result<Value, ApiError> {
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_TOKEN_INVALID",
                "ChatGPT returned an invalid token.",
            )
        })
}

fn tokens(tokens: &Value, previous: Option<&Connection>) -> Result<Connection, ApiError> {
    let id_token = tokens
        .get("id_token")
        .and_then(Value::as_str)
        .or_else(|| previous.map(|connection| connection.id_token.as_str()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_TOKEN_INVALID",
                "ChatGPT returned an invalid token.",
            )
        })?;
    let access_token = required(tokens, "access_token")?;
    let refresh_token = tokens
        .get("refresh_token")
        .and_then(Value::as_str)
        .or_else(|| previous.map(|connection| connection.refresh_token.as_str()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_TOKEN_INVALID",
                "ChatGPT returned an invalid token.",
            )
        })?;
    let id = claims(id_token)?;
    let auth = id.get("https://api.openai.com/auth").ok_or_else(|| {
        error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_TOKEN_INVALID",
            "ChatGPT returned an unsupported account token.",
        )
    })?;
    let account_id = required(auth, "chatgpt_account_id")?;
    let expires_at = claims(access_token)?
        .get("exp")
        .and_then(Value::as_i64)
        .filter(|expiry| *expiry > 0)
        .and_then(|expiry| expiry.checked_mul(1000))
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_TOKEN_INVALID",
                "ChatGPT returned an unsupported account token.",
            )
        })?;
    if auth
        .get("chatgpt_account_is_fedramp")
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_TOKEN_INVALID",
            "ChatGPT returned an unsupported account token.",
        ));
    }
    if previous.is_some_and(|connection| connection.account_id != account_id) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "CHATGPT_RECONNECT_REQUIRED",
            "The ChatGPT account changed. Connect again in AI settings.",
        ));
    }
    Ok(Connection {
        id: previous.map_or_else(Uuid::new_v4, |connection| connection.id),
        account_id: account_id.to_owned(),
        email: id
            .get("email")
            .and_then(Value::as_str)
            .or_else(|| {
                id.get("https://api.openai.com/profile")
                    .and_then(|profile| profile.get("email"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned),
        plan: auth
            .get("chatgpt_plan_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
        id_token: id_token.to_owned(),
        access_token: access_token.to_owned(),
        refresh_token: refresh_token.to_owned(),
        expires_at,
        models: previous.map_or_else(Vec::new, |connection| connection.models.clone()),
        model_id: previous.map_or_else(String::new, |connection| connection.model_id.clone()),
        reasoning_effort: previous.and_then(|connection| connection.reasoning_effort.clone()),
        legacy_catalog: previous.is_some_and(|connection| connection.legacy_catalog),
    })
}

fn headers(connection: &Connection) -> Result<HeaderMap, ApiError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {}", connection.access_token)
            .parse()
            .map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        "chatgpt-account-id",
        connection
            .account_id
            .parse()
            .map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        "originator",
        "codex_cli_rs".parse().map_err(|_| ApiError::internal())?,
    );
    Ok(headers)
}

async fn refresh(
    config: &Config,
    connection: &Connection,
    force: bool,
) -> Result<Option<Connection>, ApiError> {
    if !force && connection.expires_at >= Utc::now().timestamp_millis().saturating_add(60_000) {
        return Ok(None);
    }
    let response = post(config, "/oauth/token", &json!({"grant_type":"refresh_token","client_id":CLIENT_ID,"refresh_token":connection.refresh_token}), false).await?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "CHATGPT_RECONNECT_REQUIRED",
            "ChatGPT sign-in expired. Connect again in AI settings.",
        ));
    }
    tokens(&response_json(response).await?, Some(connection)).map(Some)
}

async fn load_models(config: &Config, mut connection: Connection) -> Result<Connection, ApiError> {
    let response = client()?
        .get(codex_url(config, "/models?client_version=0.159.3"))
        .headers(headers(&connection)?)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|_| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_MODELS_UNAVAILABLE",
                "Cannot load ChatGPT models. Try connecting again.",
            )
        })?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_MODELS_UNAVAILABLE",
            "Cannot load models for this ChatGPT account. Try connecting again.",
        ));
    }
    let catalog = response_json(response).await?;
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_MODELS_UNAVAILABLE",
                "ChatGPT returned an invalid model list.",
            )
        })?;
    let mut parsed = Vec::new();
    for model in models {
        if model
            .get("visibility")
            .and_then(Value::as_str)
            .is_some_and(|visibility| visibility != "list")
        {
            continue;
        }
        let efforts = model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |levels| {
                levels
                    .iter()
                    .filter_map(|level| level.get("effort").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            });
        if efforts
            .iter()
            .any(|effort| effort.is_empty() || effort.len() > 100)
        {
            return Err(error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_MODELS_UNAVAILABLE",
                "ChatGPT returned an invalid model list.",
            ));
        }
        let default = model
            .get("default_reasoning_level")
            .and_then(Value::as_str)
            .filter(|effort| efforts.iter().any(|supported| supported == effort))
            .map(str::to_owned)
            .or_else(|| efforts.first().cloned());
        parsed.push(Model {
            id: required(model, "slug")?.to_owned(),
            name: required(model, "display_name")?.to_owned(),
            supported_reasoning_efforts: Some(efforts),
            default_reasoning_effort: default,
        });
    }
    let selected = parsed
        .iter()
        .find(|model| model.id == connection.model_id)
        .or_else(|| parsed.first())
        .ok_or_else(|| {
            error(
                StatusCode::CONFLICT,
                "CHATGPT_MODELS_UNAVAILABLE",
                "No chat models are available for this ChatGPT account.",
            )
        })?;
    connection.model_id.clone_from(&selected.id);
    connection.reasoning_effort = connection
        .reasoning_effort
        .filter(|effort| {
            selected
                .supported_reasoning_efforts
                .as_ref()
                .is_some_and(|supported| supported.contains(effort))
        })
        .or_else(|| selected.default_reasoning_effort.clone());
    connection.models = parsed;
    connection.legacy_catalog = false;
    Ok(connection)
}

async fn upgrade(config: &Config, saved: &mut Saved) -> Result<(), ApiError> {
    if !saved
        .connection
        .as_ref()
        .is_some_and(|connection| connection.legacy_catalog)
    {
        return Ok(());
    }
    if let Some(connection) = &saved.connection
        && let Some(refreshed) = refresh(config, connection, false).await?
    {
        saved.connection = Some(refreshed);
        save(config, saved).await?;
    }
    if let Some(connection) = saved.connection.take() {
        saved.connection = Some(load_models(config, connection).await?);
        save(config, saved).await?;
    }
    Ok(())
}

async fn poll(config: &Config, user: Uuid, runtime: &mut LoginRuntime) {
    let now = Utc::now().timestamp_millis();
    let Some(attempt) = &mut runtime.pending else {
        return;
    };
    if attempt.user_id != user {
        return;
    }
    if now >= attempt.expires_at {
        runtime.pending = None;
        runtime.error = Some("Sign-in expired. Start again.".to_owned());
        return;
    }
    if now < attempt.next_poll_at {
        return;
    }
    attempt.next_poll_at = now.saturating_add(attempt.interval_ms);
    let attempt_body = json!({"device_auth_id":attempt.device_id,"user_code":attempt.user_code});
    let result = async {
        let response = post(config, "/api/accounts/deviceauth/token", &attempt_body, false).await?;
        if matches!(response.status(), StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) { return Ok(false); }
        if !response.status().is_success() { return Err(error(StatusCode::BAD_GATEWAY, "CHATGPT_LOGIN_FAILED", "ChatGPT sign-in failed. Start again.")); }
        let code = response_json(response).await?;
        let exchanged = post(config, "/oauth/token", &json!({"grant_type":"authorization_code","client_id":CLIENT_ID,"code":required(&code,"authorization_code")?,"code_verifier":required(&code,"code_verifier")?,"redirect_uri":format!("{ISSUER}/deviceauth/callback")}), true).await?;
        if !exchanged.status().is_success() { return Err(error(StatusCode::BAD_GATEWAY, "CHATGPT_LOGIN_FAILED", "ChatGPT sign-in failed. Start again.")); }
        let connection = load_models(config, tokens(&response_json(exchanged).await?, None)?).await?;
        let mut saved = read(config, user).await?; saved.provider = Provider::Chatgpt; saved.connection = Some(connection); save(config, &saved).await?;
        Ok::<bool, ApiError>(true)
    }.await;
    match result {
        Ok(false) => {}
        Ok(true) => {
            runtime.pending = None;
            runtime.error = None;
        }
        Err(error) => {
            runtime.pending = None;
            runtime.error = Some(error.message);
        }
    }
}

pub(super) async fn settings(config: &Config, user: Uuid) -> Result<Value, ApiError> {
    if config.chatgpt_connection_dir.is_none() {
        return Ok(
            json!({"enabled":false,"provider":"api","connection":null,"login":null,"error":null}),
        );
    }
    let mut runtime = LOGIN
        .get_or_init(|| Mutex::new(LoginRuntime::default()))
        .lock()
        .await;
    poll(config, user, &mut runtime).await;
    let mut saved = read(config, user).await?;
    if saved.provider == Provider::Chatgpt {
        upgrade(config, &mut saved).await?;
    }
    let connection = saved.connection.map(|connection| json!({"email":connection.email,"plan":connection.plan,"modelId":connection.model_id,"reasoningEffort":connection.reasoning_effort,"models":connection.models}));
    let login = runtime.pending.as_ref().filter(|pending| pending.user_id == user).map(|pending| json!({"verificationUrl":VERIFY_URL,"userCode":pending.user_code,"expiresAt":pending.expires_at}));
    Ok(
        json!({"enabled":true,"provider":saved.provider,"connection":connection,"login":login,"error":runtime.error}),
    )
}

pub(super) async fn start(config: &Config, user: Uuid) -> Result<(), ApiError> {
    let mut runtime = LOGIN
        .get_or_init(|| Mutex::new(LoginRuntime::default()))
        .lock()
        .await;
    let _ = read(config, user).await?;
    let now = Utc::now().timestamp_millis();
    if runtime
        .pending
        .as_ref()
        .is_some_and(|pending| pending.user_id == user && pending.expires_at > now)
    {
        return Ok(());
    }
    let response = post(
        config,
        "/api/accounts/deviceauth/usercode",
        &json!({"client_id":CLIENT_ID}),
        false,
    )
    .await?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "CHATGPT_LOGIN_FAILED",
            "Enable device-code login in ChatGPT security settings, then try again.",
        ));
    }
    let code = response_json(response).await?;
    let user_code = code
        .get("user_code")
        .or_else(|| code.get("usercode"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "CHATGPT_LOGIN_FAILED",
                "ChatGPT returned an invalid sign-in code.",
            )
        })?;
    let interval = code
        .get("interval")
        .and_then(|interval| {
            interval.as_i64().or_else(|| {
                interval
                    .as_str()
                    .and_then(|value| value.parse::<i64>().ok())
            })
        })
        .unwrap_or(5)
        .clamp(1, 60)
        .saturating_mul(1000);
    runtime.pending = Some(Pending {
        user_id: user,
        device_id: required(&code, "device_auth_id")?.to_owned(),
        user_code: user_code.to_owned(),
        expires_at: now.saturating_add(900_000),
        next_poll_at: now.saturating_add(1000),
        interval_ms: interval,
    });
    runtime.error = None;
    drop(runtime);
    Ok(())
}

fn validate_settings(body: &Value) -> Result<(), ApiError> {
    let valid_text = |value: &Value, maximum| {
        value
            .as_str()
            .is_some_and(|text| !text.is_empty() && text.encode_utf16().count() <= maximum)
    };
    let valid = body.as_object().is_some_and(|object| {
        object
            .keys()
            .all(|key| matches!(key.as_str(), "action" | "modelId" | "reasoningEffort"))
            && matches!(
                object.get("action").and_then(Value::as_str),
                Some("cancel" | "disconnect" | "api" | "chatgpt")
            )
            && object
                .get("modelId")
                .is_none_or(|value| valid_text(value, 200))
            && object
                .get("reasoningEffort")
                .is_none_or(|value| valid_text(value, 100))
    });
    if !valid {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "AI_SETTINGS_INVALID",
            "Choose an AI provider or connection action.",
        ));
    }
    Ok(())
}

pub(super) async fn update(config: &Config, user: Uuid, body: &Value) -> Result<(), ApiError> {
    validate_settings(body)?;
    let mut runtime = LOGIN
        .get_or_init(|| Mutex::new(LoginRuntime::default()))
        .lock()
        .await;
    let action = body
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("action is required"))?;
    let mut saved = read(config, user).await?;
    match action {
        "cancel" => {
            runtime.pending = None;
            runtime.error = None;
        }
        "disconnect" => {
            runtime.pending = None;
            runtime.error = None;
            saved.connection = None;
            save(config, &saved).await?;
        }
        "api" => {
            saved.provider = Provider::Api;
            save(config, &saved).await?;
        }
        "chatgpt" => {
            upgrade(config, &mut saved).await?;
            let connection = saved.connection.as_mut().ok_or_else(|| {
                error(
                    StatusCode::CONFLICT,
                    "CHATGPT_RECONNECT_REQUIRED",
                    "Connect ChatGPT first.",
                )
            })?;
            let model_id = body
                .get("modelId")
                .map(|value| {
                    value
                        .as_str()
                        .ok_or_else(|| ApiError::bad_request("modelId must be a string"))
                })
                .transpose()?
                .unwrap_or(&connection.model_id);
            let selected = connection
                .models
                .iter()
                .find(|model| model.id == model_id)
                .ok_or_else(|| {
                    error(
                        StatusCode::BAD_REQUEST,
                        "CHATGPT_MODEL_INVALID",
                        "Select an available ChatGPT model.",
                    )
                })?;
            let supported = selected
                .supported_reasoning_efforts
                .as_deref()
                .unwrap_or_default();
            let effort = body
                .get("reasoningEffort")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|effort| supported.iter().any(|supported| supported == effort))
                        .map(str::to_owned)
                        .ok_or_else(|| {
                            error(
                                StatusCode::BAD_REQUEST,
                                "CHATGPT_REASONING_EFFORT_INVALID",
                                "Select a reasoning effort supported by this ChatGPT model.",
                            )
                        })
                })
                .transpose()?;
            connection.reasoning_effort = effort
                .or_else(|| {
                    connection
                        .reasoning_effort
                        .clone()
                        .filter(|effort| supported.contains(effort))
                })
                .or_else(|| selected.default_reasoning_effort.clone());
            connection.model_id.clone_from(&selected.id);
            saved.provider = Provider::Chatgpt;
            save(config, &saved).await?;
        }
        _ => return Err(ApiError::bad_request("action is invalid")),
    }
    drop(runtime);
    Ok(())
}

pub(super) async fn reference(config: &Config, user: Uuid) -> Result<Option<Reference>, ApiError> {
    if config.chatgpt_connection_dir.is_none() {
        return Ok(None);
    }
    let _runtime = LOGIN
        .get_or_init(|| Mutex::new(LoginRuntime::default()))
        .lock()
        .await;
    let mut saved = read(config, user).await?;
    if saved.provider == Provider::Api {
        return Ok(None);
    }
    upgrade(config, &mut saved).await?;
    let connection = saved.connection.ok_or_else(|| {
        error(
            StatusCode::CONFLICT,
            "CHATGPT_RECONNECT_REQUIRED",
            "Reconnect ChatGPT or select API in AI settings.",
        )
    })?;
    Ok(Some(Reference {
        connection_id: connection.id,
        model_id: connection.model_id,
        reasoning_effort: connection.reasoning_effort,
    }))
}

pub(super) async fn credentials(
    config: &Config,
    user: Uuid,
    reference: &Reference,
    force: bool,
) -> Result<HeaderMap, ApiError> {
    let _runtime = LOGIN
        .get_or_init(|| Mutex::new(LoginRuntime::default()))
        .lock()
        .await;
    let mut saved = read(config, user).await?;
    let connection = saved
        .connection
        .as_ref()
        .filter(|connection| connection.id == reference.connection_id)
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "CHATGPT_RECONNECT_REQUIRED",
                "ChatGPT was disconnected. Connect again in AI settings.",
            )
        })?;
    if let Some(refreshed) = refresh(config, connection, force).await? {
        saved.connection = Some(refreshed);
        save(config, &saved).await?;
    }
    headers(saved.connection.as_ref().ok_or_else(ApiError::internal)?)
}
