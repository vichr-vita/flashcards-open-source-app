use crate::{AppState, auth, database, error::ApiError};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use chrono::{DateTime, Duration, Utc};
use icu_locale::{Locale, LocaleCanonicalizer};
use regex::Regex;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, Transaction};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};
use uuid::Uuid;
mod anonymous;
pub mod server;

#[derive(Deserialize)]
struct Contract {
    events: BTreeMap<String, Spec>,
    surfaces: BTreeSet<String>,
    networks: BTreeSet<String>,
    retired: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spec {
    server_only: bool,
    requires_screen: bool,
    #[serde(default)]
    identity_free: bool,
    properties: BTreeMap<String, Property>,
}
#[derive(Deserialize)]
struct Property {
    kind: String,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    values: Vec<String>,
    pattern: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Context {
    os_version: Option<String>,
    device_model: Option<String>,
    device_locale: Option<String>,
    timezone: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Batch {
    client_sent_at: String,
    anonymous_id: Option<Uuid>,
    session_id: Option<Uuid>,
    context: Option<Context>,
    #[serde(default)]
    is_automation: bool,
    events: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientEvent {
    event_id: Uuid,
    event_name: String,
    client_occurred_at: String,
    network_state: Option<String>,
    ui_locale: Option<String>,
    screen: Option<String>,
    properties: Option<Value>,
    experiment_assignments: Option<Value>,
}
struct ValidEvent {
    event: ClientEvent,
    occurred_at: DateTime<Utc>,
    client_occurred_at: DateTime<Utc>,
    properties: Value,
    experiments: Value,
}

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .merge(anonymous::router())
        .route("/v1/analytics/events", post(ingest))
        .layer(DefaultBodyLimit::max(65_536))
}

fn contract() -> Result<&'static Contract, ApiError> {
    static CONTRACT: OnceLock<Result<Contract, String>> = OnceLock::new();
    CONTRACT
        .get_or_init(|| {
            serde_json::from_str(include_str!("analytics-contract.json"))
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| {
            tracing::error!(%error,"Invalid bundled analytics catalog");
            ApiError::internal()
        })
}
fn invalid_batch() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "ANALYTICS_INVALID_BATCH",
        "Analytics batch rejected: the request envelope does not match the analytics contract.",
    )
}
fn utc(value: &str) -> Option<DateTime<Utc>> {
    value
        .ends_with('Z')
        .then(|| {
            DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|date| date.with_timezone(&Utc))
        })
        .flatten()
}
fn experiment_token(value: &str) -> bool {
    Regex::new(r"^[a-z0-9](?:[a-z0-9_-]{0,62}[a-z0-9])?$").is_ok_and(|regex| regex.is_match(value))
}

fn properties(value: Option<&Value>, spec: &Spec) -> Result<Value, &'static str> {
    let empty = Map::new();
    let map = match value {
        None | Some(Value::Null) => &empty,
        Some(value) => value.as_object().ok_or("invalid_property")?,
    };
    if map.len() > 25 {
        return Err("too_many_properties");
    }
    if !map.keys().all(|key| spec.properties.contains_key(key)) {
        return Err("unknown_property");
    }
    for (name, property) in &spec.properties {
        let Some(value) = map.get(name) else {
            if property.optional {
                continue;
            }
            return Err("invalid_property");
        };
        let valid = match property.kind.as_str() {
            "enum" => value
                .as_str()
                .is_some_and(|value| property.values.iter().any(|option| option == value)),
            "nonNegativeInteger" => value.as_f64().is_some_and(|value| {
                value.is_finite()
                    && (0.0..=9_007_199_254_740_991.0).contains(&value)
                    && value.fract() == 0.0
            }),
            "string" => value.as_str().is_some_and(|value| {
                !value.is_empty()
                    && value.encode_utf16().count() <= 200
                    && property
                        .pattern
                        .as_deref()
                        .and_then(|pattern| Regex::new(pattern).ok())
                        .is_some_and(|regex| regex.is_match(value))
            }),
            _ => false,
        };
        if !valid {
            return Err("invalid_property");
        }
    }
    if map.contains_key("install_journey_id")
        && !map.get("package_version_id").is_some_and(Value::is_string)
    {
        return Err("invalid_property");
    }
    Ok(Value::Object(map.clone()))
}

fn experiments(value: Option<&Value>) -> Result<Value, &'static str> {
    let empty = Map::new();
    let map = match value {
        None | Some(Value::Null) => &empty,
        Some(value) => value.as_object().ok_or("invalid_experiment_assignments")?,
    };
    if map.len() > 25
        || !map.iter().all(|(key, value)| {
            experiment_token(key) && value.as_str().is_some_and(experiment_token)
        })
    {
        return Err("invalid_experiment_assignments");
    }
    Ok(Value::Object(map.clone()))
}

fn validate_fields(map: &Map<String, Value>) -> Result<(), &'static str> {
    let server = [
        "email",
        "userId",
        "user_id",
        "subjectUserId",
        "subject_user_id",
        "origin",
        "trustLevel",
        "trust_level",
        "identityState",
        "identity_state",
        "schemaVersion",
        "schema_version",
        "backfillId",
        "backfill_id",
        "serverReceivedAt",
        "server_received_at",
        "occurredAt",
        "occurred_at",
        "ingestedAt",
        "ingested_at",
        "workspaceId",
        "workspace_id",
        "guestSessionId",
        "guest_session_id",
        "authTransport",
        "auth_transport",
        "platform",
        "appVersion",
        "app_version",
        "country",
        "requestId",
        "request_id",
        "details",
    ];
    if map.keys().any(|key| server.contains(&key.as_str())) {
        return Err("server_owned_field");
    }
    let client = [
        "eventId",
        "eventName",
        "clientOccurredAt",
        "networkState",
        "uiLocale",
        "screen",
        "properties",
        "experimentAssignments",
    ];
    if !map.keys().all(|key| client.contains(&key.as_str())) {
        return Err("unknown_field");
    }
    Ok(())
}

fn validate(
    raw: &Value,
    catalog: &Contract,
    sent: DateTime<Utc>,
    now: DateTime<Utc>,
    seen: &BTreeSet<Uuid>,
) -> Result<ValidEvent, &'static str> {
    let map = raw.as_object().ok_or("invalid_event")?;
    if serde_json::to_vec(raw).map_err(|_| "invalid_event")?.len() > 4096 {
        return Err("event_too_large");
    }
    validate_fields(map)?;
    let mut event: ClientEvent =
        serde_json::from_value(raw.clone()).map_err(|_| "invalid_event")?;
    if seen.contains(&event.event_id) {
        return Err("duplicate_event_id");
    }
    let spec = catalog.events.get(&event.event_name).ok_or_else(|| {
        if catalog.retired.contains(&event.event_name) {
            "retired_event_name"
        } else {
            "unknown_event_name"
        }
    })?;
    if spec.server_only {
        return Err("server_only_event");
    }
    if spec.identity_free {
        return Err("invalid_event");
    }
    if spec.requires_screen && event.screen.is_none() {
        return Err("missing_screen");
    }
    if event
        .screen
        .as_ref()
        .is_some_and(|value| !catalog.surfaces.contains(value))
        || event
            .network_state
            .as_ref()
            .is_some_and(|value| !catalog.networks.contains(value))
    {
        return Err("invalid_event");
    }
    if let Some(value) = &mut event.ui_locale {
        if value.is_empty()
            || value.trim() != value
            || value.encode_utf16().count() > 64
            || value.contains('_')
        {
            return Err("invalid_event");
        }
        let mut locale: Locale = value.parse().map_err(|_| "invalid_event")?;
        LocaleCanonicalizer::new_extended().canonicalize(&mut locale);
        *value = locale.to_string();
        if value.len() > 64 {
            return Err("invalid_event");
        }
    }
    let properties = properties(event.properties.as_ref(), spec)?;
    let experiments = experiments(event.experiment_assignments.as_ref())?;
    let client_occurred_at = utc(&event.client_occurred_at).ok_or("invalid_event")?;
    let elapsed = sent.signed_duration_since(client_occurred_at);
    if elapsed < Duration::zero() || elapsed > Duration::days(30) {
        return Err("occurred_at_out_of_window");
    }
    let occurred_at = now
        .checked_sub_signed(elapsed)
        .ok_or("occurred_at_out_of_window")?;
    Ok(ValidEvent {
        event,
        occurred_at,
        client_occurred_at,
        properties,
        experiments,
    })
}

async fn store(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
    workspace: Option<Uuid>,
    batch: &Batch,
    event: &ValidEvent,
    now: DateTime<Utc>,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let platform = headers
        .get("x-client-platform")
        .and_then(|value| value.to_str().ok())
        .filter(|value| ["web", "ios", "android"].contains(value));
    let version = headers
        .get("x-client-version")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| {
            Regex::new(r"^[0-9]{1,4}(?:\.[0-9]{1,4}){0,2}$")
                .is_ok_and(|regex| regex.is_match(value))
        });
    let context = batch.context.as_ref();
    sqlx::query("INSERT INTO analytics.product_events(event_id,schema_version,event_name,origin,client_occurred_at,client_sent_at,server_received_at,occurred_at,user_id,subject_user_id,auth_transport,trust_level,workspace_id,anonymous_id,session_id,platform,app_version,os_version,device_model,device_locale,timezone,ui_locale,network_state,screen,event_properties,experiment_assignments,request_id) VALUES($1,1,$2,'client',$3,$4,$5,$6,$7::uuid,$7::uuid,'session','authenticated_client',$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22) ON CONFLICT(event_id) DO NOTHING")
        .bind(event.event.event_id).bind(&event.event.event_name).bind(event.client_occurred_at).bind(utc(&batch.client_sent_at)).bind(now).bind(event.occurred_at).bind(user).bind(workspace).bind(batch.anonymous_id).bind(batch.session_id).bind(platform).bind(version)
        .bind(context.and_then(|value|value.os_version.as_deref())).bind(context.and_then(|value|value.device_model.as_deref())).bind(context.and_then(|value|value.device_locale.as_deref())).bind(context.and_then(|value|value.timezone.as_deref()))
        .bind(&event.event.ui_locale).bind(&event.event.network_state).bind(&event.event.screen).bind(&event.properties).bind(&event.experiments).bind(headers.get("x-request-id").and_then(|value|value.to_str().ok())).execute(&mut **tx).await?;
    Ok(())
}

async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let identity = auth::require_mutation(&state, &headers).await?;
    let batch: Batch = serde_json::from_value(body).map_err(|_| invalid_batch())?;
    if batch.events.len() > 50
        || batch.context.as_ref().is_some_and(|context| {
            [
                &context.os_version,
                &context.device_model,
                &context.device_locale,
                &context.timezone,
            ]
            .iter()
            .any(|value| {
                value
                    .as_ref()
                    .is_some_and(|value| value.encode_utf16().count() > 200)
            })
        })
    {
        return Err(invalid_batch());
    }
    let sent = utc(&batch.client_sent_at).ok_or_else(invalid_batch)?;
    let now = Utc::now();
    let catalog = contract()?;
    let mut seen = BTreeSet::new();
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for raw in &batch.events {
        let id = raw
            .get("eventId")
            .and_then(Value::as_str)
            .map(str::to_lowercase);
        match validate(raw, catalog, sent, now, &seen) {
            Ok(event) => {
                seen.insert(event.event.event_id);
                if event.event.event_id.get_version_num() == 7 {
                    accepted.push(event);
                } else {
                    rejected.push(json!({"eventId":id,"reason":"invalid_event"}));
                }
            }
            Err(reason) => rejected.push(json!({"eventId":id,"reason":reason})),
        }
    }
    let user = identity.user_id.to_string();
    let mut tx = database::scoped(&state.pool, &user, None).await?;
    let enabled: Option<bool> = sqlx::query_scalar(
        "SELECT product_analytics_enabled FROM org.user_settings WHERE user_id=$1",
    )
    .bind(&user)
    .fetch_one(&mut *tx)
    .await?;
    if enabled != Some(false) && !batch.is_automation && !accepted.is_empty() {
        let workspace: Option<Uuid> =
            sqlx::query_scalar("SELECT workspace_id FROM org.user_settings WHERE user_id=$1")
                .bind(&user)
                .fetch_one(&mut *tx)
                .await?;
        for event in &accepted {
            store(&mut tx, &user, workspace, &batch, event, now, &headers).await?;
        }
        if let Some(anonymous) = batch.anonymous_id {
            sqlx::query("INSERT INTO analytics.identity_links(link_id,anonymous_id,user_id,source) VALUES($1,$2,$3::uuid,'authenticated_client') ON CONFLICT(anonymous_id,user_id) DO NOTHING").bind(Uuid::new_v4()).bind(anonymous).bind(&user).execute(&mut *tx).await?;
            let platform = headers
                .get("x-client-platform")
                .and_then(|value| value.to_str().ok())
                .filter(|value| ["web", "ios", "android"].contains(value));
            if let Some(platform) = platform {
                let context = batch.context.as_ref();
                sqlx::query(include_str!("installation-profile.sql"))
                    .bind(anonymous)
                    .bind(platform)
                    .bind(&user)
                    .bind(
                        headers
                            .get("x-client-version")
                            .and_then(|value| value.to_str().ok())
                            .filter(|value| {
                                Regex::new(r"^[0-9]{1,4}(?:\.[0-9]{1,4}){0,2}$")
                                    .is_ok_and(|regex| regex.is_match(value))
                            }),
                    )
                    .bind(context.and_then(|value| value.os_version.as_deref()))
                    .bind(context.and_then(|value| value.device_locale.as_deref()))
                    .bind(context.and_then(|value| value.timezone.as_deref()))
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(Json(json!({"accepted":accepted.len(),"rejected":rejected})))
}
