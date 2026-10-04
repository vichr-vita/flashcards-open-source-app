//! Run the real migration CLI over populated installed-schema rows.
use color_eyre::eyre::{Result, eyre};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

async fn snapshot(owner: &PgPool, workspace: Uuid) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('workspace',(SELECT to_jsonb(w) FROM org.workspaces w WHERE workspace_id=$1),'cards',(SELECT jsonb_agg(to_jsonb(c) ORDER BY card_id) FROM content.cards c WHERE workspace_id=$1),'media',(SELECT jsonb_agg(to_jsonb(m) ORDER BY media_asset_id) FROM content.media_assets m WHERE workspace_id=$1),'replicas',(SELECT jsonb_agg(to_jsonb(r) ORDER BY replica_id) FROM sync.workspace_replicas r WHERE workspace_id=$1),'hot',(SELECT jsonb_agg(to_jsonb(h) ORDER BY change_id) FROM sync.hot_changes h WHERE workspace_id=$1),'installs',(SELECT jsonb_agg(to_jsonb(i) ORDER BY install_id) FROM sync.catalog_package_install_idempotency i WHERE workspace_id=$1),'account',(SELECT jsonb_agg(to_jsonb(a) ORDER BY user_id) FROM auth.local_account a),'sessions',(SELECT jsonb_agg(to_jsonb(s) ORDER BY session_hash) FROM auth.local_sessions s),'credentials',(SELECT jsonb_agg(to_jsonb(c) ORDER BY credential_id) FROM auth.local_passkeys c))")
        .bind(workspace).fetch_one(owner).await?)
}
pub async fn verify(owner: &PgPool, workspace: Uuid) -> Result<()> {
    let before = snapshot(owner, workspace).await?;
    let ledger: Vec<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT filename,applied_at FROM public.schema_migrations ORDER BY filename",
    )
    .fetch_all(owner)
    .await?;
    assert!(
        ledger
            .iter()
            .filter(|(file, _)| file.starts_with("0028_"))
            .count()
            >= 2
    );
    assert!(
        ledger
            .iter()
            .filter(|(file, _)| file.starts_with("0069_"))
            .count()
            >= 2
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for _ in 0..2 {
        let directory = root.clone();
        let database_url = std::env::var("CORE_TEST_DATABASE_URL")?;
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_lingvichr"))
                .arg("migrate")
                .env("MIGRATION_DATABASE_URL", database_url)
                .current_dir(directory)
                .output()
        })
        .await??;
        if !output.status.success() {
            return Err(eyre!(
                "Migration CLI failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        assert_eq!(
            snapshot(owner, workspace).await?,
            before,
            "Migration changed installed records or sync cursors"
        );
        let after: Vec<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT filename,applied_at FROM public.schema_migrations ORDER BY filename",
        )
        .fetch_all(owner)
        .await?;
        for entry in &ledger {
            assert!(
                after.contains(entry),
                "Installed migration was replayed or replaced: {}",
                entry.0
            );
        }
        assert!(
            after
                .iter()
                .any(|(name, _)| name == "0001_webauthn_library_state.sql")
        );
    }
    Ok(())
}
