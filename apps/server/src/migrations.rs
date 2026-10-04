use color_eyre::eyre::{Result, eyre};
use regex::{Captures, Regex};
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use std::path::{Path, PathBuf};

pub struct Roles {
    pub backend: String,
    pub auth: String,
    pub reporting: String,
}

impl Roles {
    fn validate(&self) -> Result<()> {
        for role in [&self.backend, &self.auth, &self.reporting] {
            if role.is_empty()
                || role.len() > 63
                || !role
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                || role.as_bytes().first().is_none_or(u8::is_ascii_digit)
            {
                return Err(eyre!(
                    "Database role names must be simple PostgreSQL identifiers"
                ));
            }
        }
        Ok(())
    }
}

/// Reuse installed filename history; numeric prefixes are deliberately not `SQLx` versions.
///
/// # Errors
/// Returns an error for invalid role names, unavailable files, or failed database operations.
pub async fn apply(database_url: &str, root: &Path, roles: &Roles) -> Result<()> {
    roles.validate()?;
    let mut connection = PgConnection::connect(database_url).await?;
    // A dedicated connection releases the session lock even when a migration fails.
    sqlx::query("SELECT pg_advisory_lock(1818848868, 1835624306)")
        .execute(&mut connection)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS public.schema_migrations (filename TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())").execute(&mut connection).await?;
    let role_pattern = Regex::new(r"\b(backend_app|auth_app|reporting_readonly)\b")?;
    for directory in ["migrations", "rust-migrations"] {
        let directory = root.join(directory);
        if !directory.exists() {
            continue;
        }
        for path in sql_files(&directory).await? {
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| eyre!("Migration filenames must be UTF-8"))?;
            let applied: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM public.schema_migrations WHERE filename = $1)",
            )
            .bind(filename)
            .fetch_one(&mut connection)
            .await?;
            if applied {
                continue;
            }
            let source = tokio::fs::read_to_string(&path).await?;
            let sql = remap(&source, &role_pattern, roles);
            let mut tx = connection.begin().await?;
            // Sources are reviewed repository files. Role substitutions accept only identifiers.
            sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO public.schema_migrations(filename) VALUES ($1)")
                .bind(filename)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            println!("Applied {filename}");
        }
    }
    for path in sql_files(&root.join("views")).await? {
        let source = tokio::fs::read_to_string(&path).await?;
        let mut tx = connection.begin().await?;
        sqlx::raw_sql(AssertSqlSafe(remap(&source, &role_pattern, roles)))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    connection.close().await?;
    Ok(())
}

async fn sql_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(directory).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "sql") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn remap(source: &str, pattern: &Regex, roles: &Roles) -> String {
    pattern
        .replace_all(source, |captures: &Captures<'_>| {
            match captures.get(1).map(|matched| matched.as_str()) {
                Some("backend_app") => roles.backend.as_str(),
                Some("auth_app") => roles.auth.as_str(),
                Some("reporting_readonly") => roles.reporting.as_str(),
                _ => "",
            }
            .to_owned()
        })
        .into_owned()
}
