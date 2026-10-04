//! Typed persisted card and scheduler contracts shared by sync and AI actions.

use crate::error::ApiError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CardSnapshot {
    pub card_id: Uuid,
    pub front_text: String,
    pub back_text: String,
    #[serde(default = "basic")]
    pub card_type: String,
    #[serde(default = "empty_metadata")]
    pub metadata: Value,
    pub tags: Vec<String>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "serialize_timestamp")]
    pub created_at: DateTime<Utc>,
    pub reps: i32,
    pub lapses: i32,
    pub fsrs_card_state: String,
    pub fsrs_step_index: Option<i32>,
    pub fsrs_stability: Option<f64>,
    pub fsrs_difficulty: Option<f64>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub fsrs_last_reviewed_at: Option<DateTime<Utc>>,
    pub fsrs_scheduled_days: Option<i32>,
    #[serde(serialize_with = "serialize_optional_timestamp")]
    pub deleted_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    #[serde(flatten)]
    pub snapshot: CardSnapshot,
    #[serde(default = "fast")]
    pub effort_level: String,
    #[serde(serialize_with = "serialize_timestamp")]
    pub client_updated_at: DateTime<Utc>,
    pub last_modified_by_replica_id: Uuid,
    pub last_operation_id: String,
    #[serde(serialize_with = "serialize_timestamp")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SchedulerConfig {
    pub algorithm: String,
    pub desired_retention: f64,
    pub learning_steps_minutes: Vec<i32>,
    pub relearning_steps_minutes: Vec<i32>,
    pub maximum_interval_days: i32,
    pub enable_fuzz: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            algorithm: "fsrs-6".into(),
            desired_retention: 0.9,
            learning_steps_minutes: vec![1, 10],
            relearning_steps_minutes: vec![10],
            maximum_interval_days: 36_500,
            enable_fuzz: true,
        }
    }
}

impl SchedulerConfig {
    /// Validate the persisted protocol invariants.
    ///
    /// # Errors
    /// Returns an invalid-input error when settings or scheduling fields are inconsistent.
    pub fn validate(&self) -> Result<(), ApiError> {
        if self.algorithm != "fsrs-6"
            || !self.desired_retention.is_finite()
            || !(0.0..1.0).contains(&self.desired_retention)
            || self.desired_retention <= 0.0
            || self.maximum_interval_days < 1
            || self.learning_steps_minutes.is_empty()
            || self.relearning_steps_minutes.is_empty()
            || self
                .learning_steps_minutes
                .iter()
                .chain(&self.relearning_steps_minutes)
                .any(|step| *step < 1)
        {
            return Err(ApiError::bad_request(
                "Invalid workspace scheduler settings",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct Mutation {
    pub client_updated_at: DateTime<Utc>,
    pub replica_id: Uuid,
    pub operation_id: String,
}

#[must_use]
pub fn fast() -> String {
    "fast".into()
}

#[must_use]
pub fn basic() -> String {
    "basic".into()
}

#[must_use]
pub fn empty_metadata() -> Value {
    serde_json::json!({"version": 1, "source": null})
}

impl CardSnapshot {
    /// Validate the persisted protocol invariants.
    ///
    /// # Errors
    /// Returns an invalid-input error when settings or scheduling fields are inconsistent.
    pub fn validate(&self) -> Result<(), ApiError> {
        let metadata = self
            .metadata
            .as_object()
            .ok_or_else(|| ApiError::bad_request("metadata must be an object"))?;
        if metadata.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(ApiError::bad_request("metadata.version must be 1"));
        }
        let source = metadata
            .get("source")
            .ok_or_else(|| ApiError::bad_request("metadata.source is required"))?;
        if !source.is_null() {
            let source = source.as_object().ok_or_else(|| {
                ApiError::bad_request("metadata.source must be an object or null")
            })?;
            for key in [
                "label",
                "author",
                "comment",
                "createdAt",
                "importedAt",
                "importId",
            ] {
                if !source
                    .get(key)
                    .is_some_and(|value| value.is_null() || value.is_string())
                {
                    return Err(ApiError::bad_request(format!(
                        "metadata.source.{key} must be a string or null"
                    )));
                }
            }
        }
        let incomplete = self.fsrs_stability.is_none()
            || self.fsrs_difficulty.is_none()
            || self.fsrs_last_reviewed_at.is_none()
            || self.fsrs_scheduled_days.is_none();
        let dirty_new = self.due_at.is_some()
            || self.fsrs_stability.is_some()
            || self.fsrs_difficulty.is_some()
            || self.fsrs_last_reviewed_at.is_some()
            || self.fsrs_step_index.is_some()
            || self.fsrs_scheduled_days.is_some();
        if self.front_text.is_empty()
            || self.reps < 0
            || self.lapses < 0
            || self.fsrs_step_index.is_some_and(|step| step < 0)
            || self.fsrs_scheduled_days.is_some_and(|days| days < 0)
            || self.fsrs_stability.is_some_and(|v| !v.is_finite())
            || self.fsrs_difficulty.is_some_and(|v| !v.is_finite())
        {
            return Err(ApiError::bad_request("Invalid card snapshot"));
        }
        match self.fsrs_card_state.as_str() {
            "new" if !dirty_new => {}
            "review" if !incomplete && self.fsrs_step_index.is_none() => {}
            "learning" | "relearning" if !incomplete && self.fsrs_step_index.is_some() => {}
            _ => {
                return Err(ApiError::bad_request(
                    "Persisted FSRS card state is inconsistent",
                ));
            }
        }
        Ok(())
    }
}

fn serialize_timestamp<S: serde::Serializer>(
    value: &DateTime<Utc>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}
#[allow(
    clippy::ref_option,
    reason = "Serde serialize_with requires a reference to the complete field value."
)]
fn serialize_optional_timestamp<S: serde::Serializer>(
    value: &Option<DateTime<Utc>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value
        .map(|stamp| stamp.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .serialize(serializer)
}
