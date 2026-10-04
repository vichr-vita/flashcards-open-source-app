//! Real HTTP and runtime-role `PostgreSQL` checks for persisted feedback and analytics decisions.
use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use color_eyre::eyre::{Result, eyre};
use lingvichr::{AppState, Config, auth, core, metadata};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;
#[path = "support/anonymous.rs"]
mod anonymous;
#[path = "support/catalog.rs"]
mod catalog;
#[path = "support/migrations.rs"]
mod migrations;

fn set(value: &mut Value, key: &str, field: Value) -> Result<()> {
    value
        .as_object_mut()
        .ok_or_else(|| eyre!("Fixture object required"))?
        .insert(key.into(), field);
    Ok(())
}

async fn call(
    client: &Client,
    base: &str,
    path: &str,
    method: Method,
    token: &str,
    csrf: &str,
    body: Option<Value>,
) -> Result<(u16, Value)> {
    let mut request = client
        .request(method, format!("{base}{path}"))
        .header("cookie", format!("session={token}"))
        .header("origin", "http://localhost:3000")
        .header("x-csrf-token", csrf)
        .header("x-client-platform", "web")
        .header("x-client-version", "1.29.0");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let value = response.json().await?;
    Ok((status, value))
}

async fn verify(state: AppState, owner: &PgPool, user: Uuid, token: &str) -> Result<()> {
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("session={token}").parse()?);
    let identity = auth::authenticate(&state, &headers)
        .await
        .map_err(|error| eyre!("{}", error.message))?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(
        axum::serve(
            listener,
            core::router().merge(metadata::router()).with_state(state),
        )
        .into_future(),
    );
    let client = Client::new();
    let csrf = &identity.csrf_token;
    let result = verify_http(&client, &base, owner, user, token, csrf).await;
    server.abort();
    result
}

async fn verify_http(
    client: &Client,
    base: &str,
    owner: &PgPool,
    user: Uuid,
    token: &str,
    csrf: &str,
) -> Result<()> {
    verify_visitor(client, base, token, csrf).await?;
    verify_feedback(client, base, owner, user, token, csrf).await?;
    verify_analytics(client, base, owner, user, token, csrf).await?;
    anonymous::verify(client, base, owner, token).await?;
    catalog::verify(client, base, owner, user, token, csrf).await?;
    Ok(())
}

async fn verify_visitor(client: &Client, base: &str, token: &str, csrf: &str) -> Result<()> {
    let unauth = client
        .get(format!("{base}/v1/analytics/visitor"))
        .send()
        .await?;
    assert_eq!(unauth.status(), 403);
    let (status, visitor) = call(
        client,
        base,
        "/v1/analytics/visitor",
        Method::GET,
        token,
        csrf,
        None,
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(visitor, json!({"consentRequired":true,"visitorId":null}));
    let granted = client
        .post(format!("{base}/v1/analytics/visitor"))
        .header("origin", "http://localhost:3000")
        .json(&json!({"granted":true}))
        .send()
        .await?;
    assert!(granted.status().is_success());
    let cookie = granted
        .headers()
        .get("set-cookie")
        .ok_or_else(|| eyre!("Visitor cookie missing"))?
        .to_str()?
        .split(';')
        .next()
        .ok_or_else(|| eyre!("Cookie empty"))?
        .to_owned();
    let returning: Value = client
        .get(format!("{base}/v1/analytics/visitor"))
        .header("origin", "http://localhost:3000")
        .header("cookie", &cookie)
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(returning.get("consentRequired"), Some(&json!(false)));
    let declined = client
        .post(format!("{base}/v1/analytics/visitor"))
        .header("origin", "http://localhost:3000")
        .header("cookie", cookie)
        .json(&json!({"granted":false}))
        .send()
        .await?;
    assert!(
        declined
            .headers()
            .get("set-cookie")
            .ok_or_else(|| eyre!("Deletion cookie missing"))?
            .to_str()?
            .contains("Max-Age=0")
    );

    Ok(())
}

async fn verify_feedback(
    client: &Client,
    base: &str,
    owner: &PgPool,
    user: Uuid,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let prompt_id = Uuid::new_v4();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let common = json!({"workspaceId":null,"installationId":null,"platform":"web","appVersion":"1.29.0","locale":"en","timezone":"Europe/Prague","createdAtClient":now});
    let mut prompt = common.clone();
    set(&mut prompt, "feedbackPromptEventId", json!(prompt_id))?;
    set(&mut prompt, "eventType", json!("automatic_prompt_shown"))?;
    let (status, first) = call(
        client,
        base,
        "/v1/feedback/prompt-events",
        Method::POST,
        token,
        csrf,
        Some(prompt.clone()),
    )
    .await?;
    assert_eq!(status, 200, "{first}");
    let (status, replay) = call(
        client,
        base,
        "/v1/feedback/prompt-events",
        Method::POST,
        token,
        csrf,
        Some(prompt),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(first, replay);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM support.feedback_prompt_events WHERE user_id=$1")
            .bind(user.to_string())
            .fetch_one(owner)
            .await?;
    assert_eq!(count, 1);
    let submission_id = Uuid::new_v4();
    let mut submission = common;
    set(
        &mut submission,
        "feedbackSubmissionId",
        json!(submission_id),
    )?;
    set(&mut submission, "trigger", json!("settings"))?;
    set(
        &mut submission,
        "message",
        json!(" Keep the cards usable. "),
    )?;
    let (status, stored) = call(
        client,
        base,
        "/v1/feedback/submissions",
        Method::POST,
        token,
        csrf,
        Some(submission.clone()),
    )
    .await?;
    assert_eq!(status, 200, "{stored}");
    set(&mut submission, "workspaceId", json!(Uuid::new_v4()))?;
    set(
        &mut submission,
        "message",
        json!("Retry must retain original feedback"),
    )?;
    let (status, replay) = call(
        client,
        base,
        "/v1/feedback/submissions",
        Method::POST,
        token,
        csrf,
        Some(submission),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(stored, replay);
    let message: String = sqlx::query_scalar(
        "SELECT message FROM support.feedback_submissions WHERE feedback_submission_id=$1",
    )
    .bind(submission_id)
    .fetch_one(owner)
    .await?;
    assert_eq!(message, "Keep the cards usable.");
    let delivery:String=sqlx::query_scalar("SELECT email_notification_status FROM support.feedback_submissions WHERE feedback_submission_id=$1").bind(submission_id).fetch_one(owner).await?;
    assert_eq!(delivery, "failed");
    assert!(
        stored
            .pointer("/feedbackState/nextAutomaticPromptAt")
            .is_some_and(Value::is_string)
    );

    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "One batch fixture verifies rejected claims, replay, and opt-out against the real database."
)]
async fn verify_analytics(
    client: &Client,
    base: &str,
    owner: &PgPool,
    user: Uuid,
    token: &str,
    csrf: &str,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let event = json!({"eventId":"0194da77-8100-7001-8111-000000000001","eventName":"screen_viewed","clientOccurredAt":now,"networkState":"wifi","uiLocale":"iw-IL","screen":"review","properties":{},"experimentAssignments":{}});
    let mut forged = event.clone();
    set(
        &mut forged,
        "eventId",
        json!("0194da77-8100-7001-8111-000000000002"),
    )?;
    set(&mut forged, "userId", json!(user))?;
    let mut private_text = event.clone();
    set(
        &mut private_text,
        "eventId",
        json!("0194da77-8100-7001-8111-000000000003"),
    )?;
    set(
        &mut private_text,
        "properties",
        json!({"card_text":"Must never persist in telemetry"}),
    )?;
    let batch = json!({"clientSentAt":now,"anonymousId":Uuid::new_v4(),"sessionId":Uuid::new_v4(),"context":{"deviceLocale":"he-IL","timezone":"Europe/Prague"},"events":[event.clone(),forged,private_text]});
    let (status, accepted) = call(
        client,
        base,
        "/v1/analytics/events",
        Method::POST,
        token,
        csrf,
        Some(batch.clone()),
    )
    .await?;
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted.get("accepted"), Some(&json!(1)), "{accepted}");
    assert_eq!(
        accepted.pointer("/rejected/0/reason"),
        Some(&json!("server_owned_field"))
    );
    assert_eq!(
        accepted.pointer("/rejected/1/reason"),
        Some(&json!("unknown_property"))
    );
    let (status, _) = call(
        client,
        base,
        "/v1/analytics/events",
        Method::POST,
        token,
        csrf,
        Some(batch),
    )
    .await?;
    assert_eq!(status, 200);
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE user_id=$1")
            .bind(user)
            .fetch_one(owner)
            .await?;
    assert_eq!(count, 1);
    let locale: String =
        sqlx::query_scalar("SELECT ui_locale FROM analytics.product_events WHERE user_id=$1")
            .bind(user)
            .fetch_one(owner)
            .await?;
    assert_eq!(locale, "he-IL");
    let (status, preferences) = call(
        client,
        base,
        "/v1/me/preferences",
        Method::PATCH,
        token,
        csrf,
        Some(json!({"productAnalyticsEnabled":false})),
    )
    .await?;
    assert_eq!(status, 200, "{preferences}");
    let mut dropped = event;
    set(
        &mut dropped,
        "eventId",
        json!("0194da77-8100-7001-8111-000000000004"),
    )?;
    let (status, accepted) = call(
        client,
        base,
        "/v1/analytics/events",
        Method::POST,
        token,
        csrf,
        Some(json!({"clientSentAt":now,"events":[dropped]})),
    )
    .await?;
    assert_eq!(status, 200);
    assert_eq!(accepted.get("accepted"), Some(&json!(1)), "{accepted}");
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE user_id=$1")
            .bind(user)
            .fetch_one(owner)
            .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn consent_feedback_and_analytics_contract() -> Result<()> {
    let _ = tracing_subscriber::fmt().try_init();
    let Ok(owner_url) = std::env::var("CORE_TEST_DATABASE_URL") else {
        return Ok(());
    };
    let owner = PgPool::connect(&owner_url).await?;
    let user = Uuid::new_v4();
    let token = URL_SAFE_NO_PAD.encode(Sha256::digest(Uuid::new_v4().as_bytes()));
    sqlx::query("INSERT INTO org.user_settings(user_id) VALUES($1)")
        .bind(user.to_string())
        .execute(&owner)
        .await?;
    sqlx::query("INSERT INTO auth.local_account(user_id,webauthn_user_handle) VALUES($1,$2)")
        .bind(user.to_string())
        .bind(Uuid::new_v4().to_string())
        .execute(&owner)
        .await?;
    sqlx::query("INSERT INTO auth.local_sessions(session_hash,refresh_hash,user_id,expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '2 hours')").bind(format!("{:x}",Sha256::digest(token.as_bytes()))).bind(format!("{:x}",Sha256::digest(Uuid::new_v4().as_bytes()))).bind(user.to_string()).execute(&owner).await?;
    let state = AppState {
        pool: PgPool::connect(&std::env::var("CORE_TEST_BACKEND_URL")?).await?,
        auth_pool: PgPool::connect(&std::env::var("CORE_TEST_AUTH_URL")?).await?,
        config: Arc::new(Config {
            backend_origin: "http://localhost:3000".into(),
            auth_origin: "http://localhost:3000".into(),
            rp_id: "localhost".into(),
            cookie_domain: "localhost".into(),
            allowed_origins: vec!["http://localhost:3000".into()],
            csrf_secret: "disposable-metadata-secret-with-at-least-32-bytes".into(),
            allow_http: true,
            chatgpt_connection_dir: None,
            web_dir: PathBuf::new(),
            local_mcp_user_id: None,
        }),
    };
    let fixture_owner = owner.clone();
    let result =
        tokio::spawn(async move { verify(state, &fixture_owner, user, &token).await }).await;
    sqlx::query("DELETE FROM analytics.product_events WHERE event_id=$1")
        .bind(anonymous::EVENT.parse::<Uuid>()?)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM analytics.installation_profiles WHERE user_id=$1")
        .bind(user)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM analytics.identity_links WHERE user_id=$1")
        .bind(user)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
        .bind(user)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
        .bind(user.to_string())
        .execute(&owner)
        .await?;
    match result {
        Ok(result) => result,
        Err(error) => Err(eyre!("Fixture failed: {error}")),
    }
}
