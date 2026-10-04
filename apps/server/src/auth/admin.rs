use super::session::{hash_token, new_token};
use crate::config::Config;
use chrono::{DateTime, Utc};
use clap::Subcommand;
use color_eyre::eyre::{Result, eyre};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Transaction};
use std::io::{IsTerminal, stdin, stdout};
use url::Url;
use uuid::Uuid;

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    Bootstrap,
    Status,
    AddPasskey,
    ResetPasskeys,
    RevokePasskey { credential_id: String },
    RevokeSessions,
}

async fn lock_account(tx: &mut Transaction<'_, Postgres>) -> Result<String> {
    sqlx::query_scalar("SELECT user_id FROM auth.local_account WHERE singleton FOR UPDATE")
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| eyre!("Local account does not exist; recovery cannot recreate it"))
}

async fn grant(tx: &mut Transaction<'_, Postgres>, config: &Config, user: &str) -> Result<String> {
    let token = new_token()?;
    sqlx::query("DELETE FROM auth.local_enrollment_grants")
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO auth.local_enrollment_grants (grant_hash,user_id,expires_at) VALUES ($1,$2,clock_timestamp()+interval '10 minutes')")
        .bind(hash_token(&token)).bind(user).execute(&mut **tx).await?;
    let mut url = Url::parse(&config.auth_origin)?.join("/enroll")?;
    url.set_fragment(Some(&format!("enroll={token}")));
    Ok(url.to_string())
}

fn print_enrollment(url: &str) {
    println!(
        "Open this private, single-use link within 10 minutes:\n{url}\nKeep it out of shared logs. Save the passkey, then sign in."
    );
}

fn replica_id(workspace: Uuid) -> Result<Uuid> {
    let digest = Sha256::digest(format!("{workspace}:workspace_seed:workspace-seed").as_bytes());
    let mut bytes: [u8; 16] = digest
        .get(..16)
        .ok_or_else(|| eyre!("Invalid replica digest"))?
        .try_into()?;
    if let Some(version) = bytes.get_mut(6) {
        *version = (*version & 0x0f) | 0x50;
    }
    if let Some(variant) = bytes.get_mut(8) {
        *variant = (*variant & 0x3f) | 0x80;
    }
    Ok(Uuid::from_bytes(bytes))
}

async fn bootstrap(pool: &PgPool, config: &Config) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(165,1)")
        .execute(&mut *tx)
        .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM auth.local_account)")
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(eyre!(
            "Local account already exists; use add-passkey or reset-passkeys"
        ));
    }
    let user = Uuid::new_v4();
    let workspace = Uuid::new_v4();
    let replica = replica_id(workspace)?;
    let user_text = user.to_string();
    sqlx::query("SELECT set_config('app.user_id',$1,true),set_config('app.workspace_id',$2,true)")
        .bind(&user_text)
        .bind(workspace.to_string())
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO org.user_settings (user_id,email) VALUES ($1,NULL)")
        .bind(&user_text)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO org.workspaces (workspace_id,name,fsrs_client_updated_at,fsrs_last_modified_by_replica_id,fsrs_last_operation_id) VALUES ($1,'Personal',clock_timestamp(),$2,$3)")
        .bind(workspace).bind(replica).bind(format!("bootstrap-workspace-{workspace}")).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO org.workspace_memberships (workspace_id,user_id,role) VALUES ($1,$2,'owner')",
    )
    .bind(workspace)
    .bind(&user_text)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO sync.workspace_replicas (replica_id,workspace_id,user_id,actor_kind,actor_key,platform,app_version,last_seen_at) VALUES ($1,$2,$3,'workspace_seed','workspace-seed','system','server-bootstrap',clock_timestamp())")
        .bind(replica).bind(workspace).bind(&user_text).execute(&mut *tx).await?;
    sqlx::query("UPDATE org.user_settings SET workspace_id=$1 WHERE user_id=$2")
        .bind(workspace)
        .bind(&user_text)
        .execute(&mut *tx)
        .await?;
    // New handles are opaque 32-byte values. Existing accounts and their handles are never rewritten.
    sqlx::query("INSERT INTO auth.local_account (user_id,webauthn_user_handle) VALUES ($1,$2)")
        .bind(&user_text)
        .bind(new_token()?)
        .execute(&mut *tx)
        .await?;
    let url = grant(&mut tx, config, &user_text).await?;
    tx.commit().await?;
    println!("Account created: {user}");
    print_enrollment(&url);
    Ok(())
}

async fn revoke_sessions(tx: &mut Transaction<'_, Postgres>) -> Result<()> {
    sqlx::query("DELETE FROM auth.local_sessions")
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM auth.local_webauthn_challenges")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn enrollment(pool: &PgPool, config: &Config, reset: bool) -> Result<()> {
    let mut tx = pool.begin().await?;
    let user = lock_account(&mut tx).await?;
    if reset {
        sqlx::query("DELETE FROM auth.local_passkeys")
            .execute(&mut *tx)
            .await?;
        revoke_sessions(&mut tx).await?;
        sqlx::query(
            "UPDATE auth.local_account SET failed_attempts=0,locked_until=NULL WHERE singleton",
        )
        .execute(&mut *tx)
        .await?;
    }
    let url = grant(&mut tx, config, &user).await?;
    tx.commit().await?;
    print_enrollment(&url);
    if reset {
        println!("Old passkeys and all sessions revoked.");
    }
    Ok(())
}

#[derive(FromRow)]
struct PasskeyStatus {
    credential_id: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

async fn status(pool: &PgPool) -> Result<()> {
    let account: Option<(String,i64)> = sqlx::query_as("SELECT user_id,(SELECT count(*) FROM auth.local_sessions WHERE refresh_expires_at>now()) AS sessions FROM auth.local_account")
        .fetch_optional(pool).await?;
    let Some((user, sessions)) = account else {
        println!("No local account.");
        return Ok(());
    };
    println!("Account: {user}\nActive sessions: {sessions}");
    let keys: Vec<PasskeyStatus> = sqlx::query_as(
        "SELECT credential_id,created_at,last_used_at FROM auth.local_passkeys ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    for key in keys {
        println!(
            "Passkey: {}\nCreated: {}\nLast used: {}",
            key.credential_id,
            key.created_at.to_rfc3339(),
            key.last_used_at
                .map_or_else(|| "never".to_owned(), |time| time.to_rfc3339())
        );
    }
    Ok(())
}

/// Administrative recovery never creates a replacement for an existing identity.
///
/// # Errors
/// Returns an error for non-interactive output, missing accounts, conflicting credentials,
/// invalid configuration, or a database failure. Failed transactions preserve the existing identity.
pub async fn account_command(
    pool: &PgPool,
    config: &Config,
    command: AccountCommand,
) -> Result<()> {
    if !stdin().is_terminal() || !stdout().is_terminal() {
        return Err(eyre!(
            "Use an interactive SSH terminal; do not redirect enrollment links into logs"
        ));
    }
    match command {
        AccountCommand::Bootstrap => bootstrap(pool, config).await,
        AccountCommand::Status => status(pool).await,
        AccountCommand::AddPasskey => enrollment(pool, config, false).await,
        AccountCommand::ResetPasskeys => enrollment(pool, config, true).await,
        AccountCommand::RevokePasskey { credential_id } => {
            let mut tx = pool.begin().await?;
            lock_account(&mut tx).await?;
            if sqlx::query("DELETE FROM auth.local_passkeys WHERE credential_id=$1")
                .bind(credential_id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                == 0
            {
                return Err(eyre!("Passkey does not exist"));
            }
            revoke_sessions(&mut tx).await?;
            sqlx::query("DELETE FROM auth.local_enrollment_grants")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            println!("Passkey and all sessions revoked.");
            Ok(())
        }
        AccountCommand::RevokeSessions => {
            let mut tx = pool.begin().await?;
            lock_account(&mut tx).await?;
            revoke_sessions(&mut tx).await?;
            tx.commit().await?;
            println!("All sessions revoked.");
            Ok(())
        }
    }
}
