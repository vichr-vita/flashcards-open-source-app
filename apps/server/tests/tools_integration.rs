//! Shared tools exercised through real MCP HTTP and the preserved disposable database.
use axum::http::HeaderMap;
use color_eyre::eyre::{Result, ensure, eyre};
use lingvichr::{AppState, Config, ai, auth, core, database};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use std::fmt::Write as _;
use std::{future::IntoFuture as _, path::PathBuf, process::Command, sync::Arc};
use uuid::Uuid;

struct Tools {
    client: reqwest::Client,
    url: String,
    key: String,
    workspace: Uuid,
    session: String,
    csrf: String,
    installation: Uuid,
}
impl Tools {
    async fn call(&self, name: &str, args: Value) -> Result<Value> {
        let response=self.client.post(&self.url).header("host","localhost:3000").header("authorization",format!("Bearer {}",self.key)).header("accept","application/json, text/event-stream").json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await?;
        let status = response.status();
        let value: Value = response.json().await?;
        ensure!(status.is_success(), "MCP HTTP failed: {status} {value}");
        let text = value
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("MCP tool result missing: {value}"))?;
        Ok(serde_json::from_str(text)?)
    }
    async fn data(&self, name: &str, args: Value) -> Result<Value> {
        let result = self.call(name, args).await?;
        ensure!(
            result.get("ok") == Some(&json!(true)),
            "Tool failed: {result}"
        );
        result
            .get("data")
            .cloned()
            .ok_or_else(|| eyre!("Tool data missing"))
    }
    async fn pull(&self) -> Result<Value> {
        let base = self
            .url
            .strip_suffix("/v1/mcp")
            .ok_or_else(|| eyre!("fixture URL"))?;
        Ok(self.client.post(format!("{base}/v1/workspaces/{}/sync/pull",self.workspace)).header("origin","http://localhost:3000").header("cookie",format!("session={}",self.session)).header("x-csrf-token",&self.csrf).json(&json!({"installationId":self.installation,"platform":"web","appVersion":"tools-integration","afterHotChangeId":0,"limit":500})).send().await?.error_for_status()?.json().await?)
    }
    async fn sql(&self, write: bool, sql: &str) -> Result<Value> {
        self.data(
            if write { "sql_execute" } else { "sql_query" },
            json!({"workspaceId":self.workspace,"sql":sql}),
        )
        .await
    }
    async fn invalid_sql(&self, write: bool, sql: &str) -> Result<()> {
        let result = self
            .call(
                if write { "sql_execute" } else { "sql_query" },
                json!({"workspaceId":self.workspace,"sql":sql}),
            )
            .await?;
        ensure!(
            result.pointer("/error/code") == Some(&json!("QUERY_INVALID_SQL")),
            "SQL unexpectedly accepted: {sql}: {result}"
        );
        Ok(())
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "The boundary test covers the published SQL grammar and atomic storage behavior through one authenticated HTTP fixture."
)]
async fn sql_contract(tools: &Tools, owner: &PgPool) -> Result<Uuid> {
    let discovery = tools.sql(false, "SHOW TABLES; DESCRIBE cards").await?;
    ensure!(
        discovery.get("statementCount") == Some(&json!(2)),
        "Read batch lost discovery"
    );
    let inserted=tools.sql(true,"INSERT INTO cards (front_text, back_text, tags) VALUES ('a', 'Answer A', ('english','core')), ('A', 'Answer B', ARRAY['english']), ('á', 'Answer C', '{core}'), ('b', 'Answer D', ()) RETURNING card_id,front_text,metadata").await?;
    ensure!(
        inserted.get("affectedCount") == Some(&json!(4)),
        "INSERT rows lost: {inserted}"
    );
    let card = inserted
        .pointer("/rows/0/card_id")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Created card id missing"))?
        .parse::<Uuid>()?;
    ensure!(
        inserted
            .pointer("/rows/0/metadata/source/createdAt")
            .is_some_and(Value::is_string),
        "Server-created card source metadata missing"
    );
    let ordered = tools
        .sql(
            false,
            "SELECT front_text FROM cards ORDER BY front_text ASC",
        )
        .await?;
    let names = ordered
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("Rows missing"))?
        .iter()
        .filter_map(|row| row.get("front_text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    ensure!(
        names == vec!["a", "A", "á", "b"],
        "JavaScript locale ordering changed: {ordered}"
    );
    let filtered=tools.sql(false,"SELECT COUNT(*) AS amount, SUM(reps) AS total FROM cards WHERE (tags OVERLAP ('core') AND LOWER(front_text) = 'a') OR front_text = 'b'").await?;
    ensure!(
        filtered.pointer("/rows/0/amount").and_then(Value::as_i64) == Some(2),
        "Parenthesized boolean filter changed: {filtered}"
    );
    let grouped=tools.sql(false,"SELECT tag,COUNT(*) AS amount FROM cards UNNEST tags AS tag GROUP BY tag ORDER BY amount DESC,tag ASC").await?;
    ensure!(
        grouped.get("totalRowCount") == Some(&json!(2))
            && grouped.pointer("/rows/0/amount") == Some(&json!(2)),
        "UNNEST/group aggregation changed: {grouped}"
    );
    let exact = tools
        .sql(
            false,
            "SELECT card_id FROM cards WHERE tags = ('core','english')",
        )
        .await?;
    ensure!(
        exact.get("rowCount") == Some(&json!(1)),
        "Tag set equality changed"
    );
    let nulls = tools
        .sql(
            false,
            "SELECT card_id FROM cards WHERE due_at IS NULL AND LOWER(tags) NOT IN ('core')",
        )
        .await?;
    ensure!(
        nulls.get("rowCount") == Some(&json!(0)),
        "Negated array IN widened rows"
    );
    let escaped=tools.sql(true,"INSERT INTO cards(front_text,back_text,tags) VALUES('Quote ''and''; (x)','Literal \\n stays literal',('core')) RETURNING front_text,back_text").await?;
    ensure!(
        escaped.pointer("/rows/0/front_text") == Some(&json!("Quote 'and'; (x)"))
            && escaped.pointer("/rows/0/back_text") == Some(&json!("Literal \\n stays literal")),
        "Quoted semicolon/backslash transport changed: {escaped}"
    );
    let old_hot: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(change_id),0) FROM sync.hot_changes WHERE workspace_id=$1",
    )
    .bind(tools.workspace)
    .fetch_one(owner)
    .await?;
    tools.invalid_sql(true,"UPDATE cards SET back_text='should roll back' WHERE front_text='a'; INSERT INTO decks(tags) VALUES(('core'))").await?;
    let persisted = tools
        .sql(false, "SELECT back_text FROM cards WHERE front_text='a'")
        .await?;
    ensure!(
        persisted.pointer("/rows/0/back_text") == Some(&json!("Answer A")),
        "Failed batch partially persisted"
    );
    let hot: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(change_id),0) FROM sync.hot_changes WHERE workspace_id=$1",
    )
    .bind(tools.workspace)
    .fetch_one(owner)
    .await?;
    ensure!(hot == old_hot, "Failed batch advanced hot cursor");
    for sql in [
        "SELECT * FROM content.cards",
        "SELECT * FROM cards JOIN org.user_settings ON TRUE",
        "SELECT pg_sleep(1) FROM cards",
        "SELECT * FROM cards WHERE metadata = '{}'",
        "SELECT * FROM cards ORDER BY fsrs_card_state",
        "SELECT * FROM cards WHERE tags LIKE '%core%'",
        "SELECT * FROM cards WHERE front_text NOT IN ('a')",
        "SELECT * FROM cards; DROP TABLE content.cards",
    ] {
        tools.invalid_sql(false, sql).await?;
    }
    for sql in [
        "UPDATE cards SET reps=42 WHERE front_text='a'",
        "INSERT INTO cards(card_id,front_text,back_text) VALUES('00000000-0000-0000-0000-000000000001','q','a')",
        "DELETE FROM cards",
        "INSERT INTO review_events(rating) VALUES(2)",
    ] {
        tools.invalid_sql(true, sql).await?;
    }
    let deck = tools
        .sql(
            true,
            "INSERT INTO decks(name,tags) VALUES('Core',('core')) RETURNING *",
        )
        .await?;
    ensure!(
        deck.pointer("/rows/0/tags") == Some(&json!(["core"])),
        "Deck is not a saved tag filter"
    );
    let batch=tools.sql(true,"UPDATE cards SET tags=('english'),effort_level='medium' WHERE front_text='b' RETURNING tags; DELETE FROM cards WHERE front_text='Quote ''and''; (x)' RETURNING front_text").await?;
    ensure!(
        batch.get("affectedCountTotal") == Some(&json!(2))
            && batch.pointer("/statements/0/rows/0/tags") == Some(&json!(["english", "medium"])),
        "Ordered batch/legacy effort shim changed: {batch}"
    );
    let deleted: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT deleted_at FROM content.cards WHERE workspace_id=$1 AND front_text=$2",
    )
    .bind(tools.workspace)
    .bind("Quote 'and'; (x)")
    .fetch_one(owner)
    .await?;
    ensure!(
        deleted.is_some(),
        "SQL deletion removed the tombstone contract"
    );
    let mut values = String::new();
    for index in 0..101 {
        if !values.is_empty() {
            values.push(',');
        }
        write!(values, "('bounded-{index}','a',('bounded'))")?;
    }
    tools
        .invalid_sql(
            true,
            &format!("INSERT INTO cards(front_text,back_text,tags) VALUES{values}"),
        )
        .await?;
    let count = tools
        .sql(
            false,
            "SELECT COUNT(*) FROM cards WHERE tags OVERLAP ('bounded')",
        )
        .await?;
    ensure!(
        count.pointer("/rows/0/count") == Some(&json!(0)),
        "Oversized write partially committed"
    );
    let facts:Vec<(String,Option<String>,i64)>=sqlx::query_as("SELECT event_name,platform,count(*) FROM analytics.product_events WHERE workspace_id=$1 GROUP BY event_name,platform ORDER BY event_name").bind(tools.workspace).fetch_all(owner).await?;
    ensure!(
        facts
            == vec![
                ("card_created".into(), Some("agent".into()), 5),
                ("card_deleted".into(), Some("agent".into()), 1),
                ("card_updated".into(), Some("agent".into()), 1),
                ("deck_created".into(), Some("agent".into()), 1)
            ],
        "Authoring facts counted failed/scheduling writes or guessed replica platform: {facts:?}"
    );
    Ok(card)
}

async fn review_contract(tools: &Tools, owner: &PgPool, card: Uuid) -> Result<()> {
    let next = tools
        .data(
            "next_review_card",
            json!({"workspaceId":tools.workspace,"tags":[]}),
        )
        .await?;
    ensure!(
        next.get("card") == Some(&Value::Null),
        "Explicit empty tags widened queue"
    );
    let revealed = tools
        .data(
            "reveal_answer",
            json!({"workspaceId":tools.workspace,"cardId":card}),
        )
        .await?;
    ensure!(
        revealed.get("backText") == Some(&json!("Answer A")),
        "Answer reveal changed"
    );
    let review_id = Uuid::new_v4();
    let args = json!({"workspaceId":tools.workspace,"cardId":card,"reviewId":review_id,"rating":"Good","reviewedTimeZone":"Europe/Prague"});
    let result = tools.data("submit_review", args.clone()).await?;
    ensure!(
        result.get("reviewId") == Some(&json!(review_id))
            && result.get("reps") == Some(&json!(1))
            && result.get("frontText").is_none(),
        "Authoritative review result changed: {result}"
    );
    let retry = tools.call("submit_review", args).await?;
    ensure!(
        retry.pointer("/error/code") == Some(&json!("REVIEW_EVENT_CONFLICT"))
            && retry.pointer("/error/details/reviewSchedule/reps") == Some(&json!(1)),
        "Retry lost schedule: {retry}"
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM content.review_events WHERE workspace_id=$1 AND client_event_id=$2",
    )
    .bind(tools.workspace)
    .bind(format!("agent-review:{review_id}"))
    .fetch_one(owner)
    .await?;
    ensure!(events == 1, "Retry added another review fact");
    let other = tools
        .sql(false, "SELECT card_id FROM cards WHERE front_text='b'")
        .await?;
    let other = other
        .pointer("/rows/0/card_id")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Second card missing"))?;
    let mismatch=tools.call("submit_review",json!({"workspaceId":tools.workspace,"cardId":other,"reviewId":review_id,"rating":"Easy","reviewedTimeZone":"Europe/Prague"})).await?;
    ensure!(
        mismatch.pointer("/error/code") == Some(&json!("REVIEW_ID_CARD_MISMATCH")),
        "Cross-card retry dropped review silently"
    );
    sqlx::query("UPDATE content.cards SET fsrs_last_reviewed_at=now()+interval '1 day' WHERE workspace_id=$1 AND card_id=$2").bind(tools.workspace).bind(card).execute(owner).await?;
    let stale=tools.call("submit_review",json!({"workspaceId":tools.workspace,"cardId":card,"reviewId":Uuid::new_v4(),"rating":"Good","reviewedTimeZone":"Europe/Prague"})).await?;
    ensure!(
        stale.pointer("/error/code") == Some(&json!("REVIEW_STALE")),
        "Future client review was overwritten"
    );
    let facts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM community.public_review_activity_facts f JOIN content.review_events e ON e.review_event_id=f.review_event_id WHERE e.workspace_id=$1 AND f.metric_version='qualified_reviews_v1' AND f.is_countable",
    )
    .bind(tools.workspace)
    .fetch_one(owner)
    .await?;
    ensure!(facts == 1, "Review retries changed qualified facts");
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "Billing candidates and usage rows must interact through the same live status endpoint to verify cache-independent derivation."
)]
async fn billing_contract(tools: &Tools, owner: &PgPool, user: Uuid) -> Result<()> {
    let free = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        free.pointer("/entitlement/tier") == Some(&json!("free"))
            && free.pointer("/usage/remainingMessages") == Some(&Value::Null),
        "Signed-in free cap was invented"
    );
    let baseline = tools.pull().await?;
    ensure!(
        baseline.pointer("/entitlement/tier") == Some(&json!("free")),
        "Sync pull entitlement missing"
    );
    let baseline_count:i64=sqlx::query_scalar("SELECT count(*) FROM analytics.product_events WHERE user_id=$1 AND event_name='entitlement_changed'").bind(user).fetch_one(owner).await?;
    ensure!(
        baseline_count == 0,
        "Initial cache invented an entitlement transition"
    );
    let grant = Uuid::new_v4();
    sqlx::query("INSERT INTO billing.grants(grant_id,user_id,tier,source,reason,expires_at) VALUES($1,$2,'premium','admin_grant','Disposable integration',now()+interval '2 days')").bind(grant).bind(user.to_string()).execute(owner).await?;
    for (surface, request, own, input, output) in [
        ("chat", "funded-turn", false, 10_i64, 2_i64),
        ("chat", "funded-turn", false, 10, 2),
        ("chat", "own-turn", true, 500, 500),
        ("dictation", "dictation", false, 50, 0),
    ] {
        sqlx::query("INSERT INTO ai.usage_events(usage_event_id,user_id,workspace_id,occurred_at,surface,provider,model_id,request_id,tier_at_call,input_tokens,output_tokens,user_supplied_key) VALUES($1,$2,$3,now(),$4,'openai','fixture',$5,'premium',$6,$7,$8)").bind(Uuid::new_v4()).bind(user.to_string()).bind(tools.workspace).bind(surface).bind(request).bind(input).bind(output).bind(own).execute(owner).await?;
    }
    sqlx::query("INSERT INTO ai.usage_events(usage_event_id,user_id,occurred_at,surface,provider,model_id,request_id,tier_at_call,input_tokens,output_tokens,user_supplied_key) VALUES($1,$2,date_trunc('month',now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'-interval '1 day','chat','openai','fixture','old-month','premium',9999,9999,false)").bind(Uuid::new_v4()).bind(user.to_string()).execute(owner).await?;
    let paid = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        paid.pointer("/entitlement/tier") == Some(&json!("premium"))
            && paid.pointer("/usage/usedMessages") == Some(&json!(1))
            && paid.pointer("/usage/ownKeyMessages") == Some(&json!(1))
            && paid.pointer("/usage/usedWeightedTokens") == Some(&json!(94))
            && paid.pointer("/usage/remainingMessages") == Some(&json!(999)),
        "Usage facts/caps changed: {paid}"
    );
    let cached: String =
        sqlx::query_scalar("SELECT tier FROM billing.entitlement_snapshots WHERE user_id=$1")
            .bind(user.to_string())
            .fetch_one(owner)
            .await?;
    ensure!(
        cached == "free",
        "Read-only MCP status wrote the entitlement cache"
    );
    let refreshed = tools.pull().await?;
    ensure!(
        refreshed.pointer("/entitlement/tier") == Some(&json!("premium")),
        "Sync pull did not refresh paid tier"
    );
    tools.pull().await?;
    let transitions:Vec<(Option<String>,Value,bool)>=sqlx::query_as("SELECT platform,event_properties,occurred_at=server_received_at FROM analytics.product_events WHERE user_id=$1 AND event_name='entitlement_changed'").bind(user).fetch_all(owner).await?;
    ensure!(
        transitions.len() == 1,
        "Entitlement refresh/retry emitted duplicate transitions"
    );
    ensure!(
        transitions.first()
            == Some(&(
                None,
                json!({"from_tier":"free","to_tier":"premium","from_status":"none","to_status":"active","source":"grant"}),
                true
            )),
        "Entitlement fact attribution changed: {transitions:?}"
    );
    sqlx::query("UPDATE billing.grants SET revoked_at=now() WHERE grant_id=$1")
        .bind(grant)
        .execute(owner)
        .await?;
    sqlx::query("INSERT INTO billing.entitlement_snapshots(user_id,tier,status,until,is_trial,will_renew,source,computed_at) VALUES($1,'lifetime','active',NULL,false,false,'grant',now()) ON CONFLICT(user_id) DO UPDATE SET tier='lifetime',status='active'").bind(user.to_string()).execute(owner).await?;
    let uncached = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        uncached.pointer("/entitlement/tier") == Some(&json!("free")),
        "Stale entitlement cache replaced actual grants"
    );
    let active = Uuid::new_v4();
    let grace = Uuid::new_v4();
    for (id, status) in [(active, "active"), (grace, "in_grace")] {
        sqlx::query("INSERT INTO billing.purchases(purchase_id,provider,provider_purchase_id,kind,user_id,tier,status,until,environment) VALUES($1,'apple',$2,'subscription',$3,'premium',$4,now()+interval '1 day','sandbox')").bind(id).bind(id.to_string()).bind(user.to_string()).bind(status).execute(owner).await?;
    }
    let active_status = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        active_status.pointer("/entitlement/status") == Some(&json!("active")),
        "Unknown grace end beat active paid access"
    );
    sqlx::query("UPDATE billing.purchases SET until=now()-interval '1 day' WHERE purchase_id=$1")
        .bind(active)
        .execute(owner)
        .await?;
    let grace_status = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        grace_status.pointer("/entitlement/status") == Some(&json!("in_grace"))
            && grace_status.pointer("/entitlement/until") == Some(&Value::Null),
        "Unknown grace end lost entitlement"
    );
    let lifetime = Uuid::new_v4();
    sqlx::query("INSERT INTO billing.grants(grant_id,user_id,tier,source,reason) VALUES($1,$2,'lifetime','gift','Disposable integration')").bind(lifetime).bind(user.to_string()).execute(owner).await?;
    let best = tools.data("get_usage_limits", json!({})).await?;
    ensure!(
        best.pointer("/entitlement/tierRank") == Some(&json!(30))
            && best.pointer("/entitlement/tierDisplayName") == Some(&json!("Lifetime")),
        "Highest tier did not win"
    );
    let unknown = Uuid::new_v4();
    sqlx::query("INSERT INTO billing.grants(grant_id,user_id,tier,source,reason) VALUES($1,$2,'unrecognized_future_tier','gift','Disposable integration')").bind(unknown).bind(user.to_string()).execute(owner).await?;
    let failed_pull = tools.pull().await?;
    ensure!(
        failed_pull.get("entitlement").is_none() && failed_pull.get("changes").is_some(),
        "Bad billing state hid core changes or invented entitlement"
    );
    let failure = tools.call("get_usage_limits", json!({})).await?;
    ensure!(
        failure.get("ok") == Some(&json!(false)),
        "Unknown billing tier silently published a fallback entitlement"
    );
    sqlx::query("DELETE FROM billing.grants WHERE grant_id=$1")
        .bind(unknown)
        .execute(owner)
        .await?;
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "The real HTTP fixture owns one disposable singleton account and always cleans its unrelated billing/usage facts after validation."
)]
async fn restricted_sql_review_and_billing_preserve_production_contracts() -> Result<()> {
    let Ok(owner_url) = std::env::var("CORE_TEST_DATABASE_URL") else {
        return Ok(());
    };
    let owner = PgPool::connect(&owner_url).await?;
    ensure!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM auth.local_account")
            .fetch_one(&owner)
            .await?
            == 0,
        "Tools fixture requires a free disposable singleton account"
    );
    let user = Uuid::new_v4();
    let user_text = user.to_string();
    sqlx::query(
        "INSERT INTO org.user_settings(user_id,progress_time_zone) VALUES($1,'Europe/Prague')",
    )
    .bind(&user_text)
    .execute(&owner)
    .await?;
    sqlx::query("INSERT INTO auth.local_account(user_id,webauthn_user_handle) VALUES($1,$1)")
        .bind(&user_text)
        .execute(&owner)
        .await?;
    let state = AppState {
        pool: PgPool::connect(&std::env::var("CORE_TEST_BACKEND_URL")?).await?,
        auth_pool: PgPool::connect(&std::env::var("CORE_TEST_AUTH_URL")?).await?,
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
            local_mcp_user_id: Some(user),
        }),
    };
    let mut tx = database::scoped(&state.pool, &user_text, None)
        .await
        .map_err(|error| eyre!(error.message))?;
    let workspace =
        core::workspaces::create_workspace_in_tx(&mut tx, &user_text, "Shared tools integration")
            .await
            .map_err(|error| eyre!(error.message))?;
    tx.commit().await?;
    let session = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let session_hash = format!("{:x}", Sha256::digest(session.as_bytes()));
    sqlx::query("INSERT INTO auth.local_sessions(session_hash,refresh_hash,user_id,expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '2 hours')").bind(&session_hash).bind(format!("{:x}",Sha256::digest(Uuid::new_v4().as_bytes()))).bind(&user_text).execute(&owner).await?;
    let mut headers = HeaderMap::new();
    headers.insert("cookie", format!("session={session}").parse()?);
    let identity = auth::authenticate(&state, &headers)
        .await
        .map_err(|error| eyre!(error.message))?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1/mcp", listener.local_addr()?);
    let server = tokio::spawn(
        axum::serve(
            listener,
            ai::router().merge(core::router()).with_state(state),
        )
        .into_future(),
    );
    let validation = async {
        let issued = Command::new(env!("CARGO_BIN_EXE_lingvichr"))
            .args(["agent-key", "issue", "Shared tools integration"])
            .env("DATABASE_URL", &owner_url)
            .output()?;
        ensure!(
            issued.status.success(),
            "Agent key fixture failed: {}",
            String::from_utf8_lossy(&issued.stderr)
        );
        let key: Value = serde_json::from_slice(&issued.stdout)?;
        let key = key
            .get("apiKey")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Key missing"))?
            .to_owned();
        let tools = Tools {
            client: reqwest::Client::new(),
            url,
            key,
            workspace,
            session: session.into(),
            csrf: identity.csrf_token,
            installation: Uuid::new_v4(),
        };
        let card = sql_contract(&tools, &owner).await?;
        review_contract(&tools, &owner, card).await?;
        billing_contract(&tools, &owner, user).await?;
        Ok::<(), color_eyre::eyre::Report>(())
    }
    .await;
    server.abort();
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=$1")
        .bind(workspace)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
        .bind(user)
        .execute(&owner)
        .await?;
    for query in [
        "DELETE FROM ai.usage_events WHERE user_id=$1",
        "DELETE FROM billing.grants WHERE user_id=$1",
        "DELETE FROM billing.purchases WHERE user_id=$1",
        "DELETE FROM billing.entitlement_snapshots WHERE user_id=$1",
        "DELETE FROM org.user_settings WHERE user_id=$1",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(&user_text)
            .execute(&owner)
            .await?;
    }
    validation
}
