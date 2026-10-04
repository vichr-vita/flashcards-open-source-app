//! Real owner CLI, stateless MCP transport and scoped SQL/review storage in one disposable fixture.
use color_eyre::eyre::{Result, ensure, eyre};
use lingvichr::{AppState, Config, ai, core, database};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use std::{future::IntoFuture as _, path::PathBuf, process::Command, sync::Arc};
use uuid::Uuid;

struct Mcp {
    client: Client,
    url: String,
    key: String,
}
impl Mcp {
    fn request(&self) -> RequestBuilder {
        self.request_headers("localhost:3000", "application/json, text/event-stream")
    }
    fn request_headers(&self, host: &str, accept: &str) -> RequestBuilder {
        self.client
            .post(&self.url)
            .header("host", host)
            .header("authorization", format!("Bearer {}", self.key))
            .header("accept", accept)
    }
    async fn rpc(&self, method: &str, params: &Value) -> Result<Value> {
        let response = self
            .request()
            .json(&json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            .send()
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "MCP transport returned {}",
            response.status()
        );
        ensure!(
            response
                .headers()
                .get("cache-control")
                .is_some_and(|value| value == "no-store")
                && response.headers().get("x-request-id").is_some(),
            "Private response headers missing"
        );
        Ok(response.json().await?)
    }
    async fn tool(&self, name: &str, args: &Value) -> Result<Value> {
        let value = self
            .rpc("tools/call", &json!({"name":name,"arguments":args}))
            .await?;
        let text = value
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Tool text missing: {value}"))?;
        Ok(serde_json::from_str(text)?)
    }
    async fn data(&self, name: &str, args: &Value) -> Result<Value> {
        let value = self.tool(name, args).await?;
        ensure!(
            value.get("ok") == Some(&json!(true)),
            "Tool failed: {value}"
        );
        value
            .get("data")
            .cloned()
            .ok_or_else(|| eyre!("Tool data missing"))
    }
}

fn cli(database_url: &str, args: &[&str]) -> Result<Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_lingvichr"))
        .arg("agent-key")
        .args(args)
        .env("DATABASE_URL", database_url)
        .output()?;
    ensure!(
        output.status.success(),
        "Owner agent-key command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn config(owner: Option<Uuid>) -> Config {
    Config {
        backend_origin: "http://localhost:3000".into(),
        auth_origin: "http://localhost:3000".into(),
        rp_id: "localhost".into(),
        cookie_domain: "localhost".into(),
        allowed_origins: vec!["http://localhost:3000".into()],
        csrf_secret: "disposable-test-secret-with-at-least-32-bytes".into(),
        allow_http: true,
        chatgpt_connection_dir: None,
        web_dir: PathBuf::new(),
        local_mcp_user_id: owner,
    }
}

async fn owner_binding(state: &AppState, key: &str) -> Result<()> {
    for owner in [None, Some(Uuid::new_v4())] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/v1/mcp", listener.local_addr()?);
        let bound = AppState {
            config: Arc::new(config(owner)),
            ..state.clone()
        };
        let server =
            tokio::spawn(axum::serve(listener, ai::router().with_state(bound)).into_future());
        let response = Client::new()
            .post(url)
            .header("host", "localhost:3000")
            .header("authorization", format!("Bearer {key}"))
            .header("accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .send()
            .await;
        server.abort();
        ensure!(
            response?.status()
                == if owner.is_some() {
                    StatusCode::UNAUTHORIZED
                } else {
                    StatusCode::NOT_FOUND
                },
            "MCP owner/enable binding lost"
        );
    }
    Ok(())
}

async fn transport_contract(mcp: &Mcp) -> Result<()> {
    let initialize = mcp.rpc("initialize",&json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"integration","version":"1"}})).await?;
    ensure!(
        initialize.pointer("/result/protocolVersion") == Some(&json!("2025-06-18"))
            && initialize.pointer("/result/serverInfo/title") == Some(&json!("lingvichr")),
        "Initialization contract changed: {initialize}"
    );
    let tools = mcp.rpc("tools/list", &json!({})).await?;
    let tools = tools
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .ok_or_else(|| eyre!("MCP tool inventory missing"))?;
    ensure!(
        tools.len() == 8
            && tools
                .iter()
                .all(|tool| tool.get("inputSchema").is_some_and(Value::is_object)
                    && tool.get("outputSchema").is_some_and(Value::is_object)),
        "Shared tool contracts missing"
    );
    let notification = mcp
        .request()
        .json(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
        .send()
        .await?;
    ensure!(
        notification.status() == StatusCode::ACCEPTED && notification.bytes().await?.is_empty(),
        "Notification response changed"
    );
    let version = mcp
        .request()
        .header("mcp-protocol-version", "invalid")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await?;
    ensure!(
        version.status() == StatusCode::BAD_REQUEST,
        "Unknown protocol version accepted"
    );
    let accept = mcp
        .request_headers("localhost:3000", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await?;
    ensure!(
        accept.status() == StatusCode::NOT_ACCEPTABLE,
        "Streamable accept guard lost"
    );
    transport_guards(mcp).await
}

async fn transport_guards(mcp: &Mcp) -> Result<()> {
    let origin = mcp
        .request()
        .header("origin", "https://other.example")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await?;
    ensure!(
        origin.status() == StatusCode::FORBIDDEN,
        "Cross-origin MCP accepted"
    );
    let host = mcp
        .request_headers("attacker.example", "application/json, text/event-stream")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await?;
    ensure!(
        host.status() == StatusCode::FORBIDDEN,
        "MCP host binding lost"
    );
    let cookies = mcp
        .client
        .post(&mcp.url)
        .header("host", "localhost:3000")
        .header("cookie", "session=anything")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
        .send()
        .await?;
    ensure!(
        cookies.status() == StatusCode::UNAUTHORIZED
            && cookies
                .headers()
                .get("www-authenticate")
                .is_some_and(|value| value == "Bearer"),
        "Browser cookies authenticated MCP"
    );
    let method = mcp
        .client
        .get(&mcp.url)
        .header("host", "localhost:3000")
        .header("authorization", format!("Bearer {}", mcp.key))
        .send()
        .await?;
    ensure!(
        method.status() == StatusCode::METHOD_NOT_ALLOWED
            && method
                .headers()
                .get("allow")
                .is_some_and(|value| value == "POST"),
        "Private MCP method guard changed"
    );
    Ok(())
}

async fn authoring_and_review(
    mcp: &Mcp,
    owner: &PgPool,
    workspace: Uuid,
    connection: Uuid,
) -> Result<()> {
    let card = mcp.data("sql_execute",&json!({"workspaceId":workspace,"sql":"INSERT INTO cards (front_text, back_text, tags) VALUES ('Question?', 'Answer', ('mcp', 'example')) RETURNING card_id"})).await?;
    let card = card
        .pointer("/rows/0/card_id")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("Inserted card missing: {card}"))?
        .parse::<Uuid>()?;
    let replica: (String,String,String,String) = sqlx::query_as("SELECT actor_kind,actor_key,platform,app_version FROM sync.workspace_replicas WHERE replica_id=(SELECT last_modified_by_replica_id FROM content.cards WHERE workspace_id=$1 AND card_id=$2)").bind(workspace).bind(card).fetch_one(owner).await?;
    ensure!(
        replica
            == (
                "agent_connection".to_owned(),
                connection.to_string(),
                "web".to_owned(),
                format!("agent:{connection}")
            ),
        "MCP actor identity changed: {replica:?}"
    );
    let next = mcp
        .data("next_review_card", &json!({"workspaceId":workspace}))
        .await?;
    ensure!(
        next.pointer("/card/cardId") == Some(&json!(card))
            && next.pointer("/card/backText").is_none(),
        "Question tool leaked answer or lost card: {next}"
    );
    let answer = mcp
        .data(
            "reveal_answer",
            &json!({"workspaceId":workspace,"cardId":card}),
        )
        .await?;
    ensure!(
        answer.get("backText") == Some(&json!("Answer")),
        "Answer tool lost stored content"
    );
    let review_id = Uuid::new_v4();
    let args = json!({"workspaceId":workspace,"cardId":card,"reviewId":review_id,"rating":"Good","reviewedTimeZone":"Europe/Prague"});
    let review = mcp.data("submit_review", &args).await?;
    ensure!(
        review.get("reviewId") == Some(&json!(review_id)) && review.get("reps") == Some(&json!(1)),
        "Review scheduler failed: {review}"
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One owner account fixture exercises CLI secrets, transport authentication and real scoped tool storage, with one cleanup boundary."
)]
async fn owner_keys_and_stateless_mcp_preserve_contracts() -> Result<()> {
    let Ok(owner_url) = std::env::var("CORE_TEST_DATABASE_URL") else {
        return Ok(());
    };
    let owner = PgPool::connect(&owner_url).await?;
    ensure!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM auth.local_account")
            .fetch_one(&owner)
            .await?
            == 0,
        "MCP fixture requires a free disposable singleton account"
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
        config: Arc::new(config(Some(user))),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/v1/mcp", listener.local_addr()?);
    let server =
        tokio::spawn(axum::serve(listener, ai::router().with_state(state.clone())).into_future());
    let validation = async {
        let issued = cli(&owner_url, &["issue", "Integration client"])?;
        let key = issued
            .get("apiKey")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Issued key missing"))?
            .to_owned();
        let connection = issued
            .pointer("/connection/connectionId")
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("Issued connection missing"))?
            .parse::<Uuid>()?;
        let listed = cli(&owner_url, &["list"])?;
        ensure!(
            listed
                .get("connections")
                .and_then(Value::as_array)
                .is_some_and(|connections| connections.len() == 1)
                && !listed.to_string().contains("apiKey")
                && !listed.to_string().contains("keyHash"),
            "List revealed a secret or lost connection"
        );
        let mcp = Mcp {
            client: Client::new(),
            url,
            key,
        };
        transport_contract(&mcp).await?;
        owner_binding(&state,&mcp.key).await?;
        let first: Uuid = sqlx::query_scalar(
            "SELECT selected_workspace_id FROM auth.agent_api_keys WHERE connection_id=$1",
        )
        .bind(connection)
        .fetch_one(&owner)
        .await?;
        let mut tx = database::scoped(&state.pool, &user_text, None)
            .await
            .map_err(|error| eyre!(error.message))?;
        let second =
            core::workspaces::create_workspace_in_tx(&mut tx, &user_text, "Browser selected")
                .await
                .map_err(|error| eyre!(error.message))?;
        tx.commit().await?;
        sqlx::query("UPDATE org.user_settings SET workspace_id=$2 WHERE user_id=$1")
            .bind(&user_text)
            .bind(second)
            .execute(&owner)
            .await?;
        let workspaces = mcp.data("list_workspaces", &json!({})).await?;
        ensure!(
            workspaces
                .get("workspaces")
                .and_then(Value::as_array)
                .is_some_and(|rows| rows
                    .iter()
                    .any(|row| row.get("workspaceId") == Some(&json!(first))
                        && row.get("isSelected") == Some(&json!(true)))),
            "Browser selection replaced key selection: {workspaces}"
        );
        sqlx::query(
            "UPDATE auth.agent_api_keys SET selected_workspace_id=NULL WHERE connection_id=$1",
        )
        .bind(connection)
        .execute(&owner)
        .await?;
        let missing = mcp
            .tool("sql_query", &json!({"sql":"SELECT COUNT(*) FROM cards"}))
            .await?;
        ensure!(
            missing.pointer("/error/code") == Some(&json!("WORKSPACE_SELECTION_REQUIRED"))
                && missing
                    .pointer("/error/details/workspaces")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| rows.len() == 2),
            "Missing key selection fell back to browser or lost remediation: {missing}"
        );
        authoring_and_review(&mcp, &owner, second, connection).await?;
        // This is a deployed-format row inserted independently of the new CLI's issuer.
        sqlx::query("INSERT INTO auth.agent_api_keys(connection_id,user_id,label,key_id,key_hash) VALUES($1,$2,'Legacy fixture','01234567','06f469c97c14e84c74853bb96aa79305eb4f6635291bf1202c4fdadb82706204')")
            .bind(Uuid::new_v4()).bind(&user_text).execute(&owner).await?;
        let legacy = Mcp {client:mcp.client.clone(),url:mcp.url.clone(),key:"fca_01234567_AAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()};
        legacy.rpc("ping",&json!({})).await?;
        let parts: Vec<_> = mcp.key.split('_').collect();
        let id = parts
            .get(1)
            .ok_or_else(|| eyre!("Key id fixture missing"))?;
        let secret = parts
            .get(2)
            .ok_or_else(|| eyre!("Key secret fixture missing"))?;
        let stored: String =
            sqlx::query_scalar("SELECT key_hash FROM auth.agent_api_keys WHERE key_id=$1")
                .bind(id)
                .fetch_one(&owner)
                .await?;
        ensure!(
            stored == format!("{:x}", Sha256::digest(secret.as_bytes())),
            "Existing SHA-256 key format changed"
        );
        let normalized = Mcp {
            client: mcp.client.clone(),
            url: mcp.url.clone(),
            key: mcp.key.to_lowercase().replace('_', "_-"),
        };
        normalized.rpc("ping", &json!({})).await?;
        cli(&owner_url, &["revoke", &connection.to_string()])?;
        let rejected = mcp
            .request()
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .send()
            .await?;
        ensure!(
            rejected.status() == StatusCode::UNAUTHORIZED,
            "Revoked key still authenticated"
        );
        Ok::<(), color_eyre::eyre::Report>(())
    }
    .await;
    server.abort();
    let workspaces: Vec<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM org.workspace_memberships WHERE user_id=$1")
            .bind(&user_text)
            .fetch_all(&owner)
            .await?;
    sqlx::query("DELETE FROM org.workspaces WHERE workspace_id=ANY($1)")
        .bind(workspaces)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM analytics.product_events WHERE user_id=$1")
        .bind(user)
        .execute(&owner)
        .await?;
    sqlx::query("DELETE FROM org.user_settings WHERE user_id=$1")
        .bind(user_text)
        .execute(&owner)
        .await?;
    validation
}
