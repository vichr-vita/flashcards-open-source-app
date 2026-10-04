use crate::error::ApiError;
use sqlx::{PgPool, Postgres, Transaction};

/// Set RLS context inside the same transaction as every domain query.
///
/// # Errors
/// Returns a database error if beginning the transaction or setting its context fails.
pub async fn scoped(
    pool: &PgPool,
    user_id: &str,
    workspace_id: Option<&str>,
) -> Result<Transaction<'static, Postgres>, ApiError> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "SELECT set_config('app.user_id', $1, true), set_config('app.workspace_id', $2, true)",
    )
    .bind(user_id)
    .bind(workspace_id.unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    Ok(tx)
}
