//! Human-managed keys retain the deployed Crockford identifiers and SHA-256 secret hashes.
use crate::{core::workspaces::create_workspace_in_tx, database::scoped, error::ApiError};
use axum::http::StatusCode;
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Subcommand;
use color_eyre::eyre::{Result, eyre};
use rand::{RngCore as _, rngs::OsRng};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use sqlx::{FromRow, PgPool};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Subcommand)]
pub enum AgentKeyCommand {
    /// Issue a key for the existing private account. Its secret is printed once.
    Issue { label: String },
    /// List connections without exposing key identifiers or secrets.
    List,
    /// Immediately revoke one connection without affecting browser sessions.
    Revoke { connection_id: Uuid },
}

#[derive(FromRow)]
struct KeyRow {
    connection_id: Uuid,
    user_id: String,
    key_hash: String,
    revoked_at: Option<DateTime<Utc>>,
}

pub(super) struct AgentConnection {
    pub user: Uuid,
    pub id: Uuid,
    pub selected: Option<Uuid>,
}

fn invalid_key() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "AGENT_API_KEY_INVALID",
        "Invalid API key",
    )
}

fn parse_key(value: &str) -> Result<(String, String), ApiError> {
    let normalized: String = value
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_uppercase)
        .collect();
    let (id, secret) = normalized
        .strip_prefix("FCA_")
        .and_then(|value| value.split_once('_'))
        .ok_or_else(invalid_key)?;
    if id.len() != 8
        || secret.len() != 26
        || !id
            .bytes()
            .chain(secret.bytes())
            .all(|byte| CROCKFORD.contains(&byte))
    {
        return Err(invalid_key());
    }
    Ok((id.to_owned(), secret.to_owned()))
}

fn secret_hash(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

/// Authenticate each request so revocation applies immediately. Browser workspace selection
/// never supplies the default for an agent connection.
pub(super) async fn authenticate(pool: &PgPool, key: &str) -> Result<AgentConnection, ApiError> {
    let (id, secret) = parse_key(key)?;
    let row: KeyRow = sqlx::query_as(
        "SELECT connection_id,user_id,key_hash,revoked_at FROM auth.authenticate_agent_api_key($1)",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(invalid_key)?;
    let hash = secret_hash(&secret);
    if row.revoked_at.is_some() || !bool::from(row.key_hash.as_bytes().ct_eq(hash.as_bytes())) {
        return Err(invalid_key());
    }
    let user = row.user_id.parse::<Uuid>().map_err(|_| invalid_key())?;
    let selected = recover_selection(pool, &row).await?;
    Ok(AgentConnection {
        user,
        id: row.connection_id,
        selected,
    })
}

async fn recover_selection(pool: &PgPool, row: &KeyRow) -> Result<Option<Uuid>, ApiError> {
    let mut tx = scoped(pool, &row.user_id, None).await?;
    // This is the same lifecycle lock used by browser workspace creation. Re-read the key
    // after it is acquired so concurrent workspace recovery cannot provision two defaults.
    sqlx::query("SELECT user_id FROM org.user_settings WHERE user_id=$1 FOR UPDATE")
        .bind(&row.user_id)
        .fetch_one(&mut *tx)
        .await?;
    let selected: Option<Uuid> = sqlx::query_scalar(
        "SELECT selected_workspace_id FROM auth.agent_api_keys WHERE connection_id=$1 AND revoked_at IS NULL FOR UPDATE",
    ).bind(row.connection_id).fetch_optional(&mut *tx).await?.ok_or_else(invalid_key)?;
    let workspaces: Vec<Uuid> = sqlx::query_scalar(
        "SELECT w.workspace_id FROM org.workspaces w JOIN org.workspace_memberships m ON m.workspace_id=w.workspace_id WHERE m.user_id=$1 ORDER BY w.created_at,w.workspace_id",
    ).bind(&row.user_id).fetch_all(&mut *tx).await?;
    let recovered = if selected.is_some_and(|id| workspaces.contains(&id)) {
        selected
    } else if workspaces.is_empty() {
        Some(create_workspace_in_tx(&mut tx, &row.user_id, "Personal").await?)
    } else if workspaces.len() == 1 {
        workspaces.first().copied()
    } else {
        None
    };
    sqlx::query("UPDATE auth.agent_api_keys SET selected_workspace_id=$2,last_used_at=CASE WHEN last_used_at IS NULL OR last_used_at<now()-interval '5 minutes' THEN now() ELSE last_used_at END WHERE connection_id=$1 AND (selected_workspace_id IS DISTINCT FROM $2 OR last_used_at IS NULL OR last_used_at<now()-interval '5 minutes')")
        .bind(row.connection_id).bind(recovered).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(recovered)
}

#[derive(FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionRecord {
    connection_id: Uuid,
    label: String,
    #[serde(serialize_with = "serialize_time")]
    created_at: DateTime<Utc>,
    #[serde(serialize_with = "serialize_optional_time")]
    last_used_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_optional_time")]
    revoked_at: Option<DateTime<Utc>>,
}

fn serialize_time<S: serde::Serializer>(
    time: &DateTime<Utc>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

#[allow(
    clippy::ref_option,
    reason = "Serde serialize_with passes the declared optional field by reference."
)]
fn serialize_optional_time<S: serde::Serializer>(
    time: &Option<DateTime<Utc>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    time.map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .serialize(serializer)
}

fn token(length: usize) -> Result<String> {
    let mut bytes = vec![0u8; length];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| eyre!("Could not generate an agent key: {error}"))?;
    bytes
        .iter()
        .map(|byte| {
            CROCKFORD
                .get(usize::from(byte & 31))
                .copied()
                .map(char::from)
                .ok_or_else(|| eyre!("Invalid key alphabet"))
        })
        .collect()
}

async fn issue(pool: &PgPool, user: &str, label: &str) -> Result<serde_json::Value> {
    let label = label.trim();
    if label.is_empty() || label.encode_utf16().count() > 120 {
        return Err(eyre!(
            "Connection label must contain between 1 and 120 characters"
        ));
    }
    let id = token(8)?;
    let secret = token(26)?;
    let mut tx = scoped(pool, user, None)
        .await
        .map_err(|error| eyre!(error.message))?;
    let connection: ConnectionRecord = sqlx::query_as(
        "INSERT INTO auth.agent_api_keys (connection_id,user_id,label,key_id,key_hash) VALUES ($1,$2,$3,$4,$5) RETURNING connection_id,label,created_at,last_used_at,revoked_at",
    ).bind(Uuid::new_v4()).bind(user).bind(label).bind(&id).bind(secret_hash(&secret)).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(serde_json::json!({"apiKey":format!("fca_{id}_{secret}"),"connection":connection}))
}

/// Run owner-only key administration against the existing singleton account. Issuing a key
/// stores only its hash, and list/revoke never print a secret.
///
/// # Errors
/// Returns an error for a missing private account, invalid label, missing connection or database failure.
pub async fn agent_key_command(pool: &PgPool, command: AgentKeyCommand) -> Result<()> {
    let user: String = sqlx::query_scalar("SELECT user_id FROM auth.local_account WHERE singleton")
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| eyre!("Local account does not exist; agent keys cannot create it"))?;
    let output = match command {
        AgentKeyCommand::Issue { label } => issue(pool, &user, &label).await?,
        AgentKeyCommand::List => {
            let mut tx = scoped(pool, &user, None)
                .await
                .map_err(|error| eyre!(error.message))?;
            let connections: Vec<ConnectionRecord> = sqlx::query_as("SELECT connection_id,label,created_at,last_used_at,revoked_at FROM auth.agent_api_keys WHERE user_id=$1 ORDER BY created_at DESC,connection_id DESC")
                .bind(&user).fetch_all(&mut *tx).await?;
            tx.commit().await?;
            serde_json::json!({"connections":connections})
        }
        AgentKeyCommand::Revoke { connection_id } => {
            let mut tx = scoped(pool, &user, None)
                .await
                .map_err(|error| eyre!(error.message))?;
            let connection: ConnectionRecord = sqlx::query_as("UPDATE auth.agent_api_keys SET revoked_at=COALESCE(revoked_at,now()) WHERE user_id=$1 AND connection_id=$2 RETURNING connection_id,label,created_at,last_used_at,revoked_at")
                .bind(&user).bind(connection_id).fetch_optional(&mut *tx).await?.ok_or_else(|| eyre!("Agent connection not found"))?;
            tx.commit().await?;
            serde_json::to_value(connection)?
        }
    };
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}
