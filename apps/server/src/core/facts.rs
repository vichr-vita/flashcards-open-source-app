//! Facts buffered by product transactions and emitted only after their wrappers commit.
use super::model::Mutation;
use crate::{
    AppState,
    database::scoped,
    error::ApiError,
    metadata::{ServerFact, server_fact},
};
use chrono::{DateTime, SecondsFormat, SubsecRound, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Pending {
    name: &'static str,
    keys: Vec<String>,
    replica: Uuid,
    client_at: DateTime<Utc>,
    server_anchor: Option<DateTime<Utc>>,
    properties: Value,
}
#[derive(Default)]
pub struct Buffer {
    pending: Vec<Pending>,
    creation_source: Option<&'static str>,
}

/// Read only authored fields and tombstone state, so scheduling and provenance changes count no edit.
pub async fn content(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    kind: &str,
    id: Uuid,
) -> Result<Option<Value>, ApiError> {
    let query = match kind {
        "card" => {
            "SELECT jsonb_build_object('front',front_text,'back',back_text,'tags',to_jsonb(tags),'deleted',deleted_at IS NOT NULL) FROM content.cards WHERE workspace_id=$1 AND card_id=$2"
        }
        "deck" => {
            "SELECT jsonb_build_object('name',name,'tags',filter_definition->'tags','deleted',deleted_at IS NOT NULL) FROM content.decks WHERE workspace_id=$1 AND deck_id=$2"
        }
        _ => return Ok(None),
    };
    sqlx::query_scalar(query)
        .bind(workspace)
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Into::into)
}
fn tags<'a>(row: &'a Value, kind: &str) -> Vec<&'a str> {
    let mut tags = row
        .get("tags")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    tags.sort_unstable();
    if kind == "deck" {
        tags.dedup();
    }
    tags
}
impl Buffer {
    /// Imports name their own authoring channel, while retaining stored replica attribution.
    pub const fn declare_creation_source(&mut self, source: &'static str) {
        self.creation_source = Some(source);
    }

    pub fn content(
        &mut self,
        kind: &str,
        id: Uuid,
        before: Option<&Value>,
        after: &Value,
        mutation: &Mutation,
    ) {
        let deleted = after.get("deleted") == Some(&json!(true));
        let was_deleted = before.is_some_and(|row| row.get("deleted") == Some(&json!(true)));
        let mut actions = Vec::new();
        if before.is_none() {
            actions.push("created");
        }
        if deleted && !was_deleted {
            actions.push("deleted");
        }
        if !deleted
            && let Some(before) = before
            && (before.get("front") != after.get("front")
                || before.get("back") != after.get("back")
                || before.get("name") != after.get("name")
                || tags(before, kind) != tags(after, kind))
        {
            actions.push("updated");
        }
        for action in actions {
            let name = match (kind, action) {
                ("card", "created") => "card_created",
                ("card", "updated") => "card_updated",
                ("card", "deleted") => "card_deleted",
                ("deck", "created") => "deck_created",
                ("deck", "updated") => "deck_updated",
                ("deck", "deleted") => "deck_deleted",
                _ => continue,
            };
            let mut keys = vec![id.to_string()];
            if action == "updated" {
                keys.push(mutation.operation_id.clone());
            }
            self.pending.push(Pending {
                name,
                keys,
                replica: mutation.replica_id,
                client_at: mutation.client_updated_at,
                server_anchor: None,
                properties: json!({}),
            });
        }
    }
    /// The persisted row supplies every fact; an imported server time remains an untrusted claim.
    pub async fn review(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        event: Uuid,
        client_supplied: bool,
    ) -> Result<(), ApiError> {
        let row=sqlx::query("SELECT replica_id,rating,reviewed_at_client,reviewed_at_server FROM content.review_events WHERE review_event_id=$1").bind(event).fetch_one(&mut **tx).await?;
        let rating: &str = match row.try_get::<i16, _>("rating")? {
            0 => "again",
            1 => "hard",
            2 => "good",
            3 => "easy",
            _ => return Err(ApiError::internal()),
        };
        self.pending.push(Pending {
            name: "review_answered",
            keys: vec![event.to_string()],
            replica: row.try_get("replica_id")?,
            client_at: row.try_get("reviewed_at_client")?,
            server_anchor: if client_supplied {
                None
            } else {
                Some(row.try_get("reviewed_at_server")?)
            },
            properties: json!({"rating":rating}),
        });
        Ok(())
    }
    /// Resolve immutable attribution once for a committed batch, with one shared bounded drain.
    pub async fn emit(
        self,
        state: &AppState,
        user: Uuid,
        workspace: Uuid,
        ai_chat_platform: Option<&str>,
    ) {
        if self.pending.is_empty() {
            return;
        }
        let start = Instant::now();
        let replicas = self
            .pending
            .iter()
            .map(|fact| fact.replica)
            .collect::<Vec<_>>();
        let lookup = async {
            let mut tx =
                scoped(&state.pool, &user.to_string(), Some(&workspace.to_string())).await?;
            let rows=sqlx::query("SELECT r.replica_id,r.actor_kind,r.platform,COALESCE(i.is_automation,false) AS is_automation FROM sync.workspace_replicas r LEFT JOIN sync.installations i ON i.installation_id=r.installation_id WHERE r.replica_id=ANY($1)").bind(replicas).fetch_all(&mut *tx).await?;
            let result = rows
                .into_iter()
                .map(|row| {
                    Ok((
                        row.try_get::<Uuid, _>("replica_id")?,
                        (
                            row.try_get::<String, _>("actor_kind")?,
                            row.try_get::<String, _>("platform")?,
                            row.try_get::<bool, _>("is_automation")?,
                        ),
                    ))
                })
                .collect::<Result<HashMap<_, _>, sqlx::Error>>()?;
            tx.commit().await?;
            Ok::<_, ApiError>(result)
        };
        let attribution = if let Ok(Ok(rows)) =
            tokio::time::timeout(Duration::from_secs(2), lookup).await
        {
            rows
        } else {
            tracing::warn!(user_id=%user,workspace_id=%workspace,"Committed fact replica attribution could not be resolved");
            HashMap::new()
        };
        let recorded = Utc::now().trunc_subsecs(3);
        for pending in self.pending {
            if start.elapsed() >= Duration::from_secs(4) {
                tracing::warn!(user_id=%user,workspace_id=%workspace,"Committed facts exceeded their shared reporting budget");
                break;
            }
            let actor = attribution.get(&pending.replica);
            if actor.is_some_and(|(_, _, automation)| *automation) {
                continue;
            }
            let source = actor.and_then(|(kind, _, _)| match kind.as_str() {
                "client_installation" => Some("app"),
                "ai_chat" => Some("ai_chat"),
                "agent_connection" => Some("agent"),
                _ => None,
            });
            let source = if pending.name == "card_created" {
                self.creation_source.or(source)
            } else {
                source
            };
            let platform = actor.and_then(|(kind, platform, _)| match kind.as_str() {
                "client_installation" if matches!(platform.as_str(), "web" | "ios" | "android") => {
                    Some(platform.as_str())
                }
                "agent_connection" => Some("agent"),
                "ai_chat" if pending.name == "review_answered" => ai_chat_platform,
                _ => None,
            });
            let mut properties = pending.properties;
            if matches!(pending.name, "card_created" | "review_answered")
                && let Some(source) = source
                && let Some(object) = properties.as_object_mut()
            {
                object.insert("source".into(), json!(source));
            }
            let anchor = pending.server_anchor.unwrap_or(recorded).trunc_subsecs(3);
            let occurred = plausible(pending.client_at, anchor);
            let keys = pending.keys.iter().map(String::as_str).collect::<Vec<_>>();
            let fact = ServerFact {
                name: pending.name,
                stable_keys: &keys,
                user_id: user,
                subject_user_id: Some(user),
                workspace_id: Some(workspace),
                occurred_at: occurred,
                received_at: anchor,
                platform,
                properties,
                details: None,
            };
            if tokio::time::timeout(Duration::from_secs(4), server_fact(state, fact))
                .await
                .is_err()
            {
                tracing::warn!(event_name = pending.name, "Committed fact write timed out");
            }
        }
    }
}
fn plausible(client: DateTime<Utc>, anchor: DateTime<Utc>) -> DateTime<Utc> {
    let client = client.trunc_subsecs(3);
    if client > anchor || anchor.signed_duration_since(client) > TimeDelta::days(30) {
        anchor
    } else {
        client
    }
}

pub async fn decision(
    state: &AppState,
    user: &str,
    workspace: Uuid,
    name: &str,
    at: DateTime<Utc>,
) {
    let Ok(user) = user.parse::<Uuid>() else {
        return;
    };
    let workspace_text = workspace.to_string();
    let instant = at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let keys = if name == "study_progress_reset" {
        vec![workspace_text.as_str(), instant.as_str()]
    } else {
        vec![workspace_text.as_str()]
    };
    server_fact(
        state,
        ServerFact {
            name,
            stable_keys: &keys,
            user_id: user,
            subject_user_id: None,
            workspace_id: Some(workspace),
            occurred_at: at,
            received_at: at,
            platform: None,
            properties: json!({}),
            details: None,
        },
    )
    .await;
}
