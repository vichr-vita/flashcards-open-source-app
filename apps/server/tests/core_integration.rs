//! `PostgreSQL`-backed compatibility checks. The URLs must point at a disposable migrated database.

use axum::http::HeaderMap;
use chrono::{DateTime, Duration, Utc};
use color_eyre::eyre::{Result, ensure, eyre};
use lingvichr::{AppState, Config, auth, core, database, error::ApiError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{future::IntoFuture, path::PathBuf, sync::Arc};
use uuid::Uuid;

macro_rules! check_eq {
    ($left:expr, $right:expr $(,)?) => {
        ensure!($left == $right, "contract mismatch: {:?} != {:?}", $left, $right)
    };
    ($left:expr, $right:expr, $($context:tt)+) => {
        ensure!($left == $right, $($context)+)
    };
}

fn contract(error: ApiError) -> color_eyre::eyre::Report {
    let ApiError { code, message, .. } = error;
    eyre!("{code}: {message}")
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    csrf: &str,
    body: &Value,
) -> Result<Value> {
    let response = client
        .post(url)
        .header("cookie", format!("session={token}"))
        .header("origin", "http://localhost:3000")
        .header("x-csrf-token", csrf)
        .json(body)
        .send()
        .await?;
    let status = response.status();
    let value: Value = response.json().await?;
    if !status.is_success() {
        return Err(eyre!("HTTP {status}: {value}"));
    }
    Ok(value)
}

fn snapshot(id: Uuid, at: DateTime<Utc>) -> Result<core::CardSnapshot> {
    Ok(serde_json::from_value(
        json!({"cardId":id,"frontText":"Question","backText":"Answer","cardType":"basic","metadata":{"version":1,"source":null},"tags":["core"],"dueAt":null,"createdAt":at,"reps":0,"lapses":0,"fsrsCardState":"new","fsrsStepIndex":null,"fsrsStability":null,"fsrsDifficulty":null,"fsrsLastReviewedAt":null,"fsrsScheduledDays":null,"deletedAt":null}),
    )?)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "This integration deliberately exercises one real database/session lifecycle and cleans up its shared fixture once."
)]
async fn preserved_postgres_sync_and_fsrs_contract() -> Result<()> {
    let Ok(owner_url) = std::env::var("CORE_TEST_DATABASE_URL") else {
        return Ok(());
    };
    let backend_url = std::env::var("CORE_TEST_BACKEND_URL")?;
    let auth_url = std::env::var("CORE_TEST_AUTH_URL")?;
    let owner = PgPool::connect(&owner_url).await?;
    let user: Option<String> = sqlx::query_scalar("SELECT user_id FROM auth.local_account")
        .fetch_optional(&owner)
        .await?;
    let created_account = user.is_none();
    let user = user.unwrap_or_else(|| Uuid::new_v4().to_string());
    if created_account {
        sqlx::query(
            "INSERT INTO org.user_settings(user_id,progress_time_zone) VALUES($1,'Europe/Prague')",
        )
        .bind(&user)
        .execute(&owner)
        .await?;
        sqlx::query("INSERT INTO auth.local_account(user_id,webauthn_user_handle) VALUES($1,$1)")
            .bind(&user)
            .execute(&owner)
            .await?;
    }
    let pool = PgPool::connect(&backend_url).await?;
    let auth_pool = PgPool::connect(&auth_url).await?;
    let state = AppState {
        pool,
        auth_pool,
        config: Arc::new(Config {
            backend_origin: "http://localhost:3000".into(),
            auth_origin: "http://localhost:3000".into(),
            rp_id: "localhost".into(),
            cookie_domain: "localhost".into(),
            allowed_origins: vec!["http://localhost:3000".into()],
            csrf_secret: "disposable-test-secret-with-at-least-32-bytes".into(),
            allow_http: true,
            chatgpt_connection_dir: None,
            web_dir: PathBuf::new(),
            local_mcp_user_id: None,
        }),
    };
    let token = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let session_hash = format!("{:x}", Sha256::digest(token.as_bytes()));
    let refresh_hash = format!("{:x}", Sha256::digest(Uuid::new_v4().as_bytes()));
    sqlx::query("INSERT INTO auth.local_sessions(session_hash,refresh_hash,user_id,expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '2 hours') ON CONFLICT(session_hash) DO UPDATE SET user_id=EXCLUDED.user_id,expires_at=EXCLUDED.expires_at,refresh_expires_at=EXCLUDED.refresh_expires_at").bind(&session_hash).bind(refresh_hash).bind(&user).execute(&owner).await?;
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("session={token}").parse()?);
    let identity = auth::authenticate(&state, &headers)
        .await
        .map_err(contract)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(
        axum::serve(
            listener,
            core::router()
                .merge(lingvichr::progress::router())
                .with_state(state.clone()),
        )
        .into_future(),
    );
    let client = reqwest::Client::new();
    let created = post(
        &client,
        &format!("{base}/v1/workspaces"),
        token,
        &identity.csrf_token,
        &json!({"name":"Core integration"}),
    )
    .await?;
    let workspace = created
        .pointer("/workspace/workspaceId")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("workspace identity absent"))?
        .parse::<Uuid>()?;
    let validation = async {
    let installation = Uuid::new_v4();
    let card = Uuid::new_v4();
    let now = "2026-03-01T00:00:00.000Z".parse::<DateTime<Utc>>()?;
    let payload = serde_json::to_value(snapshot(card, now)?)?;
    let operation = json!({"operationId":"create-card","entityType":"card","entityId":card,"action":"upsert","clientUpdatedAt":now,"payload":payload});
    let body = json!({"installationId":installation,"platform":"web","appVersion":"integration","operations":[operation]});
    let url = format!("{base}/v1/workspaces/{workspace}/sync/push");
    let pushed = post(&client, &url, token, &identity.csrf_token, &body).await?;
    check_eq!(
        pushed.pointer("/operations/0/status"),
        Some(&json!("applied"))
    );
    let duplicate = post(&client, &url, token, &identity.csrf_token, &body).await?;
    check_eq!(
        duplicate.pointer("/operations/0/status"),
        Some(&json!("duplicate"))
    );
    check_eq!(
        duplicate.pointer("/operations/0/resultingHotChangeId"),
        pushed.pointer("/operations/0/resultingHotChangeId")
    );
    let mut outdated = body.clone();
    outdated
        .pointer_mut("/operations/0/operationId")
        .ok_or_else(|| eyre!("missing fixture"))?
        .clone_from(&json!("stale"));
    outdated
        .pointer_mut("/operations/0/clientUpdatedAt")
        .ok_or_else(|| eyre!("missing fixture"))?
        .clone_from(&json!("2026-02-28T00:00:00.000Z"));
    let ignored = post(&client, &url, token, &identity.csrf_token, &outdated).await?;
    check_eq!(
        ignored.pointer("/operations/0/status"),
        Some(&json!("ignored"))
    );
    check_eq!(
        ignored.pointer("/operations/0/resultingHotChangeId"),
        pushed.pointer("/operations/0/resultingHotChangeId")
    );
    let mut tx = database::scoped(&state.pool, &user, Some(&workspace.to_string()))
        .await
        .map_err(contract)?;
    let replica = core::sync::ensure_client(
        &mut tx,
        &user,
        workspace,
        &core::sync::Client {
            installation_id: installation,
            platform: "web".into(),
            app_version: Some("integration".into()),
            is_automation: false,
        },
    )
    .await
    .map_err(contract)?;
    check_eq!(
        replica,
        core::sync::replica_id(&format!("{workspace}:{installation}"))
    );
    tx.commit().await?;
    let vectors: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/fsrs-full-vectors.json"))?;
    for vector in vectors {
        let card = Uuid::new_v4();
        let settings: core::SchedulerConfig = serde_json::from_value(
            vector
                .get("settings")
                .cloned()
                .ok_or_else(|| eyre!("settings absent"))?,
        )?;
        sqlx::query("UPDATE org.workspaces SET fsrs_desired_retention=$2,fsrs_learning_steps_minutes=$3,fsrs_relearning_steps_minutes=$4,fsrs_maximum_interval_days=$5,fsrs_enable_fuzz=$6 WHERE workspace_id=$1").bind(workspace).bind(settings.desired_retention).bind(json!(settings.learning_steps_minutes)).bind(json!(settings.relearning_steps_minutes)).bind(settings.maximum_interval_days).bind(settings.enable_fuzz).execute(&owner).await?;
        let reviews = vector
            .get("reviews")
            .and_then(Value::as_array)
            .ok_or_else(|| eyre!("reviews absent"))?;
        let first = reviews
            .first()
            .and_then(|review| review.get("at"))
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("timestamp absent"))?
            .parse::<DateTime<Utc>>()?;
        let initial = first
            .checked_sub_signed(Duration::seconds(1))
            .ok_or_else(|| eyre!("timestamp overflow"))?;
        let mut tx = database::scoped(&state.pool, &user, Some(&workspace.to_string()))
            .await
            .map_err(contract)?;
        core::mutate_card_in_tx(
            &mut tx,
            workspace,
            snapshot(card, initial)?,
            &core::Mutation {
                client_updated_at: initial,
                replica_id: replica,
                operation_id: Uuid::new_v4().to_string(),
            },
        )
        .await
        .map_err(contract)?;
        tx.commit().await?;
        for review in reviews {
            let at = review
                .get("at")
                .and_then(Value::as_str)
                .ok_or_else(|| eyre!("timestamp absent"))?
                .parse::<DateTime<Utc>>()?;
            let rating = u8::try_from(
                review
                    .get("rating")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| eyre!("rating absent"))?,
            )?;
            core::submit_review(&state, &user, workspace, card, rating, at, "integration")
                .await
                .map_err(contract)?;
        }
        let actual = serde_json::to_value(
            core::get_card(&state, &user, workspace, card)
                .await
                .map_err(contract)?,
        )?;
        let expected = vector
            .get("expected")
            .and_then(Value::as_object)
            .ok_or_else(|| eyre!("expected absent"))?;
        for (key, value) in expected {
            if value.is_number() {
                check_eq!(
                    actual.get(key).and_then(Value::as_f64),
                    value.as_f64(),
                    "{} {key}",
                    vector
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                );
            } else {
                check_eq!(
                    actual.get(key),
                    Some(value),
                    "{} {key}",
                    vector
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                );
            }
        }
    }
        core::submit_review(&state,&user,workspace,card,2,Utc::now(),"integration").await.map_err(contract)?;
        let progress:Value=client.get(format!("{base}/v1/me/progress/summary?timeZone=Europe%2FPrague")).header("cookie",format!("session={token}")).send().await?.error_for_status()?.json().await?;
        check_eq!(progress.pointer("/summary/hasReviewedToday"),Some(&json!(true)));
        let today=sqlx::query_scalar::<_,String>("SELECT (now() AT TIME ZONE 'Europe/Prague')::date::text").fetch_one(&owner).await?;
        let series:Value=client.get(format!("{base}/v1/me/progress/series?timeZone=Europe%2FPrague&from={today}&to={today}")).header("cookie",format!("session={token}")).send().await?.error_for_status()?.json().await?;
        ensure!(series.pointer("/dailyReviews/0/reviewCount").and_then(Value::as_i64).is_some_and(|value|value>=1),"today's review missing from progress series");
        let me:Value=client.get(format!("{base}/v1/me")).header("cookie",format!("session={token}")).send().await?.error_for_status()?.json().await?;
        check_eq!(me.get("userId"),Some(&json!(user)));
        check_eq!(me.get("selectedWorkspaceId"),Some(&json!(workspace)));
        let page=post(&client,&format!("{base}/v1/workspaces/{workspace}/cards/query"),token,&identity.csrf_token,&json!({"searchText":null,"cursor":null,"limit":1,"sorts":[{"key":"dueAt","direction":"asc"}],"filter":{"tags":["core"]}})).await?;
        check_eq!(page.get("totalCount"),Some(&json!(16)));
        let next=post(&client,&format!("{base}/v1/workspaces/{workspace}/cards/query"),token,&identity.csrf_token,&json!({"searchText":null,"cursor":page.get("nextCursor"),"limit":1,"sorts":[{"key":"dueAt","direction":"asc"}],"filter":{"tags":["core"]}})).await?;
        ensure!(page.pointer("/cards/0/cardId")!=next.pointer("/cards/0/cardId"),"keyset cursor returned same card twice");
        let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM content.review_events WHERE workspace_id=$1")
            .bind(workspace)
            .fetch_one(&owner)
            .await?;
    let facts:i64=sqlx::query_scalar("SELECT count(*) FROM community.public_review_activity_facts f JOIN content.review_events r USING(review_event_id) WHERE r.workspace_id=$1").bind(workspace).fetch_one(&owner).await?;
    check_eq!(count, facts);
    let answered:i64=sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE workspace_id=$1 AND event_name='review_answered'").bind(workspace).fetch_one(&owner).await?;
    check_eq!(answered,count);
    let authored:i64=sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE workspace_id=$1 AND event_name IN('card_created','card_updated')").bind(workspace).fetch_one(&owner).await?;
    check_eq!(authored,1);
    let history=post(&client,&format!("{base}/v1/workspaces/{workspace}/sync/review-history/pull"),token,&identity.csrf_token,&json!({"installationId":installation,"platform":"web","appVersion":null,"afterReviewSequenceId":0,"limit":500})).await?;
    check_eq!(
        history
            .get("reviewEvents")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(usize::try_from(count)?)
    );
    let pull=post(&client,&format!("{base}/v1/workspaces/{workspace}/sync/pull"),token,&identity.csrf_token,&json!({"installationId":installation,"platform":"web","appVersion":null,"afterHotChangeId":0,"limit":500})).await?;
    ensure!(
        pull.get("changes")
            .and_then(Value::as_array)
            .is_some_and(|changes| changes
                .iter()
                .all(|change| change.get("entityType") != Some(&json!("review_event"))))
    );

        let mut deleted=core::get_card(&state,&user,workspace,card).await.map_err(contract)?.snapshot;
        let deleted_at=Utc::now().checked_add_signed(Duration::seconds(1)).ok_or_else(||eyre!("timestamp overflow"))?;
        deleted.deleted_at=Some(deleted_at);
        let tombstone=post(&client,&url,token,&identity.csrf_token,&json!({"installationId":installation,"platform":"web","appVersion":null,"operations":[{"operationId":"delete-card","entityType":"card","entityId":card,"action":"upsert","clientUpdatedAt":deleted_at,"payload":deleted}]})).await?;
        check_eq!(tombstone.pointer("/operations/0/status"),Some(&json!("applied")));
        let visible=post(&client,&format!("{base}/v1/workspaces/{workspace}/cards/query"),token,&identity.csrf_token,&json!({"limit":100,"sorts":[],"filter":null,"cursor":null,"searchText":null})).await?;
        check_eq!(visible.get("totalCount"),Some(&json!(15)));
        let bootstrap=post(&client,&format!("{base}/v1/workspaces/{workspace}/sync/bootstrap"),token,&identity.csrf_token,&json!({"installationId":installation,"platform":"web","appVersion":null,"mode":"pull","limit":100,"cursor":null})).await?;
        ensure!(bootstrap.get("entries").and_then(Value::as_array).is_some_and(|entries|entries.iter().any(|entry|entry.get("entityId")==Some(&json!(card)) && entry.pointer("/payload/deletedAt").is_some_and(|value|!value.is_null()))),"canonical bootstrap dropped tombstone");
        check_eq!(bootstrap.get("remoteIsEmpty"),Some(&json!(false)));
        let imported=post(&client,&format!("{base}/v1/workspaces/{workspace}/sync/review-history/import"),token,&identity.csrf_token,&json!({"installationId":installation,"platform":"web","appVersion":null,"reviewEvents":history.get("reviewEvents")})).await?;
        check_eq!(imported.get("importedCount"),Some(&json!(0)));
        check_eq!(imported.get("duplicateCount"),Some(&json!(count)));
        let reset=post(&client,&format!("{base}/v1/workspaces/{workspace}/reset-progress"),token,&identity.csrf_token,&json!({"confirmationText":"reset all progress for all cards in this workspace"})).await?;
        check_eq!(reset.get("cardsResetCount"),Some(&json!(15)));
        let remaining:i64=sqlx::query_scalar("SELECT count(*) FROM content.review_events WHERE workspace_id=$1").bind(workspace).fetch_one(&owner).await?;
        check_eq!(remaining,count);
        let decisions:Vec<(String,Option<String>,Value)>=sqlx::query_as("SELECT event_name,platform,event_properties FROM analytics.product_events WHERE workspace_id=$1 AND event_name IN('card_deleted','study_progress_reset') ORDER BY event_name").bind(workspace).fetch_all(&owner).await?;
        check_eq!(decisions,vec![("card_deleted".to_owned(),Some("web".to_owned()),json!({})),("study_progress_reset".to_owned(),None,json!({}))]);
        let statuses:Vec<String>=sqlx::query_scalar("SELECT fsrs_card_state FROM content.cards WHERE workspace_id=$1 AND deleted_at IS NULL").bind(workspace).fetch_all(&owner).await?;
        ensure!(statuses.iter().all(|value|value=="new"),"workspace reset failed to clear persisted schedule");
    Ok::<(),color_eyre::eyre::Report>(())
    }.await;
    server.abort();
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=$1")
        .bind(workspace)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM auth.local_sessions WHERE session_hash=$1")
        .bind(session_hash)
        .execute(&owner)
        .await?;
    if created_account {
        sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
            .bind(user.parse::<Uuid>()?)
            .execute(&owner)
            .await?;
        sqlx::query("DELETE FROM billing.entitlement_snapshots WHERE user_id=$1")
            .bind(&user)
            .execute(&owner)
            .await?;
        sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
            .bind(user)
            .execute(&owner)
            .await?;
    }
    validation
}
