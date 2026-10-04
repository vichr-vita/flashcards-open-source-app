use crate::{AppState, auth, database, error::ApiError};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Parameters {
    time_zone: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ReviewHistoryWatermark {
    pub workspace_id: Uuid,
    #[ts(type = "number")]
    pub review_sequence_id: i64,
}

#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StreakFreeze {
    pub available_credits: u32,
    pub capacity: u32,
    pub balance_units: u32,
    pub units_per_credit: u32,
    pub earned_units_per_streak_day: u32,
    pub next_credit_progress_units: u32,
    pub next_credit_required_units: u32,
}

struct Evaluation {
    current: u32,
    longest: u32,
    balance: u32,
    days: BTreeMap<NaiveDate, &'static str>,
}

pub struct ReviewFact<'a> {
    pub user_id: &'a str,
    pub workspace_id: Uuid,
    pub event_id: Uuid,
    pub replica_id: Uuid,
    pub rating: u8,
    pub reviewed_at_client: DateTime<Utc>,
    pub reviewed_at_server: DateTime<Utc>,
    pub time_zone: Option<&'a str>,
}

/// Called once for each newly inserted review, inside its owning transaction.
///
/// # Errors
/// Returns an error if the timezone is invalid or a database operation fails.
pub async fn record_review_facts(
    tx: &mut Transaction<'_, Postgres>,
    input: ReviewFact<'_>,
) -> Result<(), ApiError> {
    let fallback: Option<String> =
        sqlx::query_scalar("SELECT progress_time_zone FROM org.user_settings WHERE user_id = $1")
            .bind(input.user_id)
            .fetch_one(&mut **tx)
            .await?;
    let timezone = input.time_zone.or(fallback.as_deref());
    if let Some(timezone) = timezone {
        let canonical = timezone_name(tx, timezone).await?;
        let source = if input.time_zone.is_some() {
            "client"
        } else {
            "user_settings"
        };
        let date: NaiveDate = sqlx::query_scalar("SELECT timezone($1, $2::timestamptz)::date")
            .bind(&canonical)
            .bind(input.reviewed_at_client)
            .fetch_one(&mut **tx)
            .await?;
        sqlx::query("UPDATE content.review_events SET reviewed_time_zone = $2, reviewed_local_date = $3, reviewed_time_zone_source = $4 WHERE review_event_id = $1 AND reviewed_by_user_id = $5")
            .bind(input.event_id).bind(&canonical).bind(date).bind(source).bind(input.user_id).execute(&mut **tx).await?;
        sqlx::query(include_str!("sql/progress-active-upsert.sql"))
            .bind(input.user_id)
            .bind(date.to_string())
            .bind(input.reviewed_at_client)
            .bind(&canonical)
            .bind(source)
            .execute(&mut **tx)
            .await?;
    }
    sqlx::query("INSERT INTO community.public_profiles(user_id, public_profile_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(input.user_id).bind(Uuid::new_v4()).execute(&mut **tx).await?;
    sqlx::query("INSERT INTO community.public_review_activity_facts(review_event_id, metric_version, public_profile_id, reviewed_by_user_id, rating, reviewed_at_client, reviewed_at_server, is_countable, exclusion_reason) SELECT $1, 'qualified_reviews_v1', public_profile_id, user_id, $3, $4, $5, $3 <> 0, CASE WHEN $3 = 0 THEN 'again' ELSE NULL END FROM community.public_profiles WHERE user_id = $2 ON CONFLICT(review_event_id, metric_version) DO NOTHING")
        .bind(input.event_id).bind(input.user_id).bind(i16::from(input.rating)).bind(input.reviewed_at_client).bind(input.reviewed_at_server).execute(&mut **tx).await?;
    Ok(())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/progress/summary", get(summary))
        .route("/v1/me/progress/review-schedule", get(schedule))
        .route("/v1/me/progress/series", get(series))
}

async fn timezone_name(
    tx: &mut Transaction<'_, Postgres>,
    timezone: &str,
) -> Result<String, ApiError> {
    let timezone = timezone.trim();
    if timezone.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROGRESS_TIMEZONE_REQUIRED",
            "timeZone is required",
        ));
    }
    sqlx::query_scalar("SELECT name FROM pg_timezone_names WHERE lower(name) = lower($1) LIMIT 1")
        .bind(timezone)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "PROGRESS_TIMEZONE_INVALID",
                "timeZone must be a valid IANA timezone",
            )
        })
}

async fn start(
    state: &AppState,
    headers: &HeaderMap,
    params: &Parameters,
) -> Result<
    (
        Transaction<'static, Postgres>,
        String,
        String,
        Vec<Uuid>,
        NaiveDate,
    ),
    ApiError,
> {
    let identity = auth::authenticate(state, headers).await?;
    let user_id = identity.user_id.to_string();
    let mut tx = database::scoped(&state.pool, &user_id, None).await?;
    let raw = params.time_zone.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROGRESS_TIMEZONE_REQUIRED",
            "timeZone is required",
        )
    })?;
    let canonical = timezone_name(&mut tx, raw).await?;
    sqlx::query("UPDATE org.user_settings SET progress_time_zone = $2 WHERE user_id = $1 AND progress_time_zone IS DISTINCT FROM $2").bind(&user_id).bind(&canonical).execute(&mut *tx).await?;
    let workspaces: Vec<Uuid> = sqlx::query_scalar("SELECT m.workspace_id FROM org.workspace_memberships m JOIN org.workspaces w USING(workspace_id) WHERE m.user_id = $1 ORDER BY w.created_at, w.workspace_id").bind(&user_id).fetch_all(&mut *tx).await?;
    let today: NaiveDate = sqlx::query_scalar("SELECT timezone($1, clock_timestamp())::date")
        .bind(&canonical)
        .fetch_one(&mut *tx)
        .await?;
    Ok((tx, user_id, canonical, workspaces, today))
}

async fn active_dates(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
) -> Result<BTreeSet<NaiveDate>, ApiError> {
    let dates: Vec<NaiveDate> = sqlx::query_scalar("SELECT local_date FROM progress.user_active_review_days WHERE reviewed_by_user_id = $1 ORDER BY local_date").bind(user_id).fetch_all(&mut **tx).await?;
    Ok(dates.into_iter().collect())
}

async fn scope(tx: &mut Transaction<'_, Postgres>, workspace: Uuid) -> Result<(), ApiError> {
    sqlx::query("SELECT set_config('app.workspace_id', $1, true)")
        .bind(workspace.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn watermark(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
) -> Result<ReviewHistoryWatermark, ApiError> {
    let sequence: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(review_sequence), 0) FROM content.review_events WHERE workspace_id = $1").bind(workspace).fetch_one(&mut **tx).await?;
    Ok(ReviewHistoryWatermark {
        workspace_id: workspace,
        review_sequence_id: sequence,
    })
}

async fn summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Parameters>,
) -> Result<Json<Value>, ApiError> {
    let (mut tx, user_id, _, workspaces, today) = start(&state, &headers, &params).await?;
    let mut watermarks = Vec::new();
    for workspace in workspaces {
        scope(&mut tx, workspace).await?;
        watermarks.push(watermark(&mut tx, workspace).await?);
    }
    watermarks.sort_by_key(|mark| mark.workspace_id);
    let dates = active_dates(&mut tx, &user_id).await?;
    let evaluation = evaluate(&dates, today)?;
    let response = json!({"timeZone": params.time_zone.as_deref().map(str::trim), "generatedAt": Utc::now(), "reviewHistoryWatermarks": watermarks, "summary": {"currentStreakDays":evaluation.current,"longestStreakDays":evaluation.longest,"hasReviewedToday":dates.contains(&today),"lastReviewedOn":dates.last().map(ToString::to_string),"activeReviewDays":dates.len(),"streakFreeze":freeze(evaluation.balance)}});
    tx.commit().await?;
    Ok(Json(response))
}

async fn schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Parameters>,
) -> Result<Json<Value>, ApiError> {
    let (mut tx, _, canonical, workspaces, _) = start(&state, &headers, &params).await?;
    let generated = Utc::now();
    let mappings = [
        ("new", "new_count"),
        ("today", "today_count"),
        ("days1To7", "days_1_to_7_count"),
        ("days8To30", "days_8_to_30_count"),
        ("days31To90", "days_31_to_90_count"),
        ("days91To360", "days_91_to_360_count"),
        ("years1To2", "years_1_to_2_count"),
        ("later", "later_count"),
    ];
    let mut counts: BTreeMap<&str, i64> = mappings.iter().map(|(key, _)| (*key, 0)).collect();
    let mut watermarks = Vec::new();
    for workspace in workspaces {
        scope(&mut tx, workspace).await?;
        let row = sqlx::query(include_str!("sql/progress-schedule.sql"))
            .bind(workspace)
            .bind(&canonical)
            .bind(generated)
            .fetch_one(&mut *tx)
            .await?;
        for (key, column) in mappings {
            let count: i32 = row.try_get(column)?;
            let target = counts.get_mut(key).ok_or_else(ApiError::internal)?;
            *target = target
                .checked_add(i64::from(count))
                .ok_or_else(ApiError::internal)?;
        }
        watermarks.push(watermark(&mut tx, workspace).await?);
    }
    let mut total = 0_i64;
    let mut buckets = Vec::new();
    for (key, _) in mappings {
        let count = counts.get(key).copied().ok_or_else(ApiError::internal)?;
        total = total.checked_add(count).ok_or_else(ApiError::internal)?;
        buckets.push(json!({"key":key,"count":count}));
    }
    watermarks.sort_by_key(|mark| mark.workspace_id);
    tx.commit().await?;
    Ok(Json(
        json!({"timeZone":params.time_zone.as_deref().map(str::trim),"generatedAt":generated,"totalCards":total,"buckets":buckets,"reviewHistoryWatermarks":watermarks}),
    ))
}

fn date_param(value: Option<&str>, field: &str) -> Result<NaiveDate, ApiError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("PROGRESS_{}_REQUIRED", field.to_uppercase()),
                format!("{field} is required"),
            )
        })?;
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == value)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("PROGRESS_{}_INVALID", field.to_uppercase()),
                format!("{field} must be a YYYY-MM-DD date"),
            )
        })
}

async fn series(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Parameters>,
) -> Result<Json<Value>, ApiError> {
    let (mut tx, user_id, canonical, workspaces, today) = start(&state, &headers, &params).await?;
    let from = date_param(params.from.as_deref(), "from")?;
    let to = date_param(params.to.as_deref(), "to")?;
    if from > to {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROGRESS_RANGE_INVALID",
            "from must be less than or equal to to",
        ));
    }
    if to.signed_duration_since(from).num_days() >= 366 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "PROGRESS_RANGE_TOO_LARGE",
            "Date range must include at most 366 days",
        ));
    }
    let mut counts = BTreeMap::<NaiveDate, BTreeMap<&str, i64>>::new();
    let mut watermarks = Vec::new();
    let columns = [
        ("reviewCount", "review_count"),
        ("againCount", "again_count"),
        ("hardCount", "hard_count"),
        ("goodCount", "good_count"),
        ("easyCount", "easy_count"),
    ];
    for workspace in workspaces {
        scope(&mut tx, workspace).await?;
        sqlx::query(include_str!("sql/progress-backfill.sql"))
            .bind(&user_id)
            .bind(&canonical)
            .bind(workspace)
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(include_str!("sql/progress-series.sql"))
            .bind(workspace)
            .bind(&user_id)
            .bind(&canonical)
            .bind(from.to_string())
            .bind(to.to_string())
            .fetch_all(&mut *tx)
            .await?;
        for row in rows {
            let date: String = row.try_get("review_date")?;
            let date = date_param(Some(&date), "date")?;
            let day = counts.entry(date).or_default();
            for (key, column) in columns {
                let count: i32 = row.try_get(column)?;
                let current = day.entry(key).or_default();
                *current = current
                    .checked_add(i64::from(count))
                    .ok_or_else(ApiError::internal)?;
            }
        }
        watermarks.push(watermark(&mut tx, workspace).await?);
    }
    let dates = active_dates(&mut tx, &user_id).await?;
    let evaluation = evaluate(&dates, today)?;
    let mut daily_reviews = Vec::new();
    let mut streak_days = Vec::new();
    let mut date = from;
    while date <= to {
        let mut day = serde_json::Map::new();
        day.insert("date".into(), json!(date.to_string()));
        for (key, _) in columns {
            day.insert(
                key.into(),
                json!(
                    counts
                        .get(&date)
                        .and_then(|day| day.get(key))
                        .copied()
                        .unwrap_or_default()
                ),
            );
        }
        daily_reviews.push(Value::Object(day));
        let status = if dates.contains(&date) {
            "reviewed"
        } else {
            evaluation
                .days
                .get(&date)
                .copied()
                .unwrap_or(if date >= today { "pending" } else { "missed" })
        };
        streak_days.push(json!({"date":date.to_string(),"state":status}));
        date = date.succ_opt().ok_or_else(ApiError::internal)?;
    }
    watermarks.sort_by_key(|mark| mark.workspace_id);
    tx.commit().await?;
    Ok(Json(
        json!({"timeZone":params.time_zone.as_deref().map(str::trim),"from":from.to_string(),"to":to.to_string(),"generatedAt":Utc::now(),"dailyReviews":daily_reviews,"streakDays":streak_days,"reviewHistoryWatermarks":watermarks}),
    ))
}

fn evaluate(dates: &BTreeSet<NaiveDate>, today: NaiveDate) -> Result<Evaluation, ApiError> {
    let mut result = Evaluation {
        current: 0,
        longest: 0,
        balance: 20,
        days: BTreeMap::new(),
    };
    let mut active = false;
    let mut date = dates
        .first()
        .copied()
        .filter(|date| *date <= today)
        .unwrap_or(today);
    while date <= today {
        let status = if dates.contains(&date) {
            if !active {
                result.balance = 20;
                result.current = 0;
            }
            result.balance = result.balance.saturating_add(1).min(20);
            result.current = result
                .current
                .checked_add(1)
                .ok_or_else(ApiError::internal)?;
            active = true;
            "reviewed"
        } else if date == today {
            "pending"
        } else if active && result.balance >= 10 {
            result.balance = result
                .balance
                .checked_sub(10)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(ApiError::internal)?;
            result.current = result
                .current
                .checked_add(1)
                .ok_or_else(ApiError::internal)?;
            "frozen"
        } else {
            result.balance = 20;
            result.current = 0;
            active = false;
            "missed"
        };
        result.longest = result.longest.max(result.current);
        result.days.insert(date, status);
        date = date.succ_opt().ok_or_else(ApiError::internal)?;
    }
    Ok(result)
}

fn freeze(balance: u32) -> StreakFreeze {
    let available = balance.checked_div(10).unwrap_or_default().min(2);
    StreakFreeze {
        available_credits: available,
        capacity: 2,
        balance_units: balance,
        units_per_credit: 10,
        earned_units_per_streak_day: 1,
        next_credit_progress_units: if available == 2 {
            0
        } else {
            balance.checked_rem(10).unwrap_or_default()
        },
        next_credit_required_units: 10,
    }
}
