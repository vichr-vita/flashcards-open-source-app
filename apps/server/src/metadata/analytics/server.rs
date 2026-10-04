use super::{contract, properties};
use crate::{AppState, error::ApiError};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct ServerFact<'a> {
    pub name: &'a str,
    pub stable_keys: &'a [&'a str],
    pub user_id: Uuid,
    pub subject_user_id: Option<Uuid>,
    pub workspace_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub platform: Option<&'a str>,
    pub properties: Value,
    pub details: Option<Value>,
}

async fn store(state: &AppState, fact: &ServerFact<'_>) -> Result<(), ApiError> {
    let spec = contract()?
        .events
        .get(fact.name)
        .filter(|spec| spec.server_only)
        .ok_or_else(ApiError::internal)?;
    let properties = properties(Some(&fact.properties), spec).map_err(|_| ApiError::internal())?;
    let key =
        std::iter::once("flashcards-open-source-app:product-analytics:server-derived-event-id:v1")
            .chain(std::iter::once(fact.name))
            .chain(fact.stable_keys.iter().copied())
            .collect::<Vec<_>>()
            .join(":");
    let digest = Sha256::digest(key.as_bytes());
    let bytes: [u8; 16] = digest
        .get(..16)
        .ok_or_else(ApiError::internal)?
        .try_into()
        .map_err(|_| ApiError::internal())?;
    let id = Uuid::from_bytes(bytes);
    sqlx::query("INSERT INTO analytics.product_events(event_id,schema_version,event_name,origin,server_received_at,occurred_at,user_id,subject_user_id,trust_level,workspace_id,platform,event_properties,experiment_assignments,details) VALUES($1,1,$2,'server',$3,$4,$5,$6,'server_derived',$7,$8,$9,'{}'::jsonb,$10) ON CONFLICT(event_id) DO NOTHING")
        .bind(id).bind(fact.name).bind(fact.received_at).bind(fact.occurred_at).bind(fact.user_id).bind(fact.subject_user_id).bind(fact.workspace_id).bind(fact.platform).bind(properties).bind(&fact.details).execute(&state.pool).await?;
    Ok(())
}

/// Record only a committed, server-observed operation; analytics failures never fail its response.
pub async fn server_fact(state: &AppState, fact: ServerFact<'_>) {
    match tokio::time::timeout(std::time::Duration::from_secs(4), store(state, &fact)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            event_name = fact.name,
            error_code = error.code,
            "Server-derived analytics write failed"
        ),
        Err(_) => tracing::warn!(
            event_name = fact.name,
            "Server-derived analytics write timed out"
        ),
    }
}
