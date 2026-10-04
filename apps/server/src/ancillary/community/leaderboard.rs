//! Snapshot reads preserve viewer tie rules and disclose only opaque public identities.

use super::{anonymous_name, ensure_profile, friend_labels, locale, stamp};
use crate::{AppState, auth::authenticate, database::scoped, error::ApiError};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::get,
};
use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;

const WINDOWS: [&str; 5] = [
    "last_24_hours",
    "last_3_days",
    "last_7_days",
    "last_30_days",
    "all_time",
];
const RATING: &str = "qualified_reviews_v1";
const STREAK: &str = "streak_days_v1";

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/me/progress/leaderboard", get(rating))
        .route("/v1/me/progress/leaderboards/streak", get(streak))
        .route(
            "/v1/me/progress/leaderboards/profiles/{profile}",
            get(profile),
        )
}
fn metric(streak: bool, locale_hint: &str) -> Result<Value, ApiError> {
    let copy: Value = serde_json::from_str(include_str!("../leaderboard-copy.json"))
        .map_err(|_| ApiError::internal())?;
    let copy = copy
        .get(if streak { "streak" } else { "rating" })
        .and_then(|copy| copy.get(locale(locale_hint)))
        .ok_or_else(ApiError::internal)?;
    Ok(
        json!({"metricVersion":if streak {STREAK} else {RATING},"title":copy.get("title"),"description":copy.get("description")}),
    )
}
fn non_ready(status: &str, metric: &Value, streak: bool) -> Value {
    if streak {
        json!({"status":status,"metric":metric})
    } else {
        json!({"status":status,"metric":metric,"defaultWindowKey":WINDOWS.first(),"windows":[]})
    }
}
struct Participant {
    id: Uuid,
    count: i64,
    rank: usize,
    viewer: bool,
}
fn insert(value: &mut Value, key: &str, property: Value) -> Result<(), ApiError> {
    value
        .as_object_mut()
        .ok_or_else(ApiError::internal)?
        .insert(key.to_owned(), property);
    Ok(())
}
fn rank(mut entries: Vec<(Uuid, i64)>, viewer: Uuid, streak: bool) -> Vec<Participant> {
    let count = entries
        .iter()
        .find(|(id, _)| *id == viewer)
        .map_or(0, |(_, count)| *count);
    entries.retain(|(id, _)| *id != viewer);
    let position = entries
        .iter()
        .position(|(_, other)| {
            if streak {
                *other <= count
            } else {
                *other < count
            }
        })
        .unwrap_or(entries.len());
    entries.insert(position, (viewer, count));
    entries
        .into_iter()
        .enumerate()
        .map(|(index, (id, count))| Participant {
            id,
            count,
            rank: index.saturating_add(1),
            viewer: id == viewer,
        })
        .collect()
}
fn ranked_rows(
    participants: &[Participant],
    labels: &BTreeMap<Uuid, String>,
    locale_hint: &str,
    streak: bool,
) -> Result<(Value, Value), ApiError> {
    let viewer = participants
        .iter()
        .find(|participant| participant.viewer)
        .ok_or_else(ApiError::internal)?;
    let top = participants.len().min(3);
    let mut compact = Vec::new();
    let mut full = Vec::new();
    let mut previous = 0_usize;
    for p in participants {
        let mut row = json!({"kind":if p.viewer {"viewer"} else {"participant"},"publicProfileId":p.id,"anonymousDisplayName":anonymous_name(p.id,locale_hint)?,"rank":p.rank});
        insert(
            &mut row,
            if streak {
                "streakDays"
            } else {
                "qualifiedReviewCount"
            },
            json!(p.count),
        )?;
        if let Some(label) = labels.get(&p.id) {
            insert(&mut row, "friendDisplayName", json!(label))?;
        }
        full.push(row.clone());
        let near_viewer = if viewer.rank > top {
            p.rank.abs_diff(viewer.rank) <= 1
        } else {
            viewer.rank == top && p.rank == viewer.rank.saturating_add(1)
        };
        if p.rank <= top
            || near_viewer
            || p.rank == participants.len()
            || labels.contains_key(&p.id)
        {
            if previous != 0 && p.rank > previous.saturating_add(1) {
                compact.push(json!({"kind":"gap"}));
            }
            insert(
                &mut row,
                "kind",
                json!(if p.viewer {
                    "viewer"
                } else if p.rank <= top {
                    "top"
                } else {
                    "neighbor"
                }),
            )?;
            compact.push(row);
            previous = p.rank;
        }
    }
    Ok((json!(compact), json!(full)))
}
fn viewer_json(participants: &[Participant], streak: bool) -> Result<Value, ApiError> {
    let p = participants
        .iter()
        .find(|p| p.viewer)
        .ok_or_else(ApiError::internal)?;
    let mut value = json!({"publicProfileId":p.id,"displayName":"You","rank":p.rank});
    insert(
        &mut value,
        if streak {
            "streakDays"
        } else {
            "qualifiedReviewCount"
        },
        json!(p.count),
    )?;
    Ok(value)
}
async fn entries(
    tx: &mut Transaction<'_, Postgres>,
    snapshot: Uuid,
    streak: bool,
) -> Result<Vec<(Uuid, i64)>, ApiError> {
    let query = if streak {
        "SELECT public_profile_id,streak_days::bigint AS count FROM community.streak_leaderboard_snapshot_entries WHERE snapshot_id=$1 ORDER BY base_sort_position ASC"
    } else {
        "SELECT public_profile_id,qualified_review_count::bigint AS count FROM community.leaderboard_snapshot_entries WHERE snapshot_id=$1 ORDER BY base_sort_position ASC"
    };
    sqlx::query(query)
        .bind(snapshot)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|row| Ok((row.try_get("public_profile_id")?, row.try_get("count")?)))
        .collect()
}
async fn rating(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (viewer, enabled, locale) = ensure_profile(&mut tx, &user).await?;
    let metric = metric(false, &locale)?;
    if !enabled {
        tx.commit().await?;
        return Ok(Json(non_ready("participation_disabled", &metric, false)));
    }
    let headers=sqlx::query("SELECT DISTINCT ON(window_key) window_key,snapshot_id,generated_at,as_of_server_hour FROM community.leaderboard_snapshots WHERE metric_version=$1 ORDER BY window_key,as_of_server_hour DESC").bind(RATING).fetch_all(&mut *tx).await?;
    let headers: BTreeMap<String, _> = headers
        .into_iter()
        .map(|row| Ok((row.try_get("window_key")?, row)))
        .collect::<Result<_, sqlx::Error>>()?;
    if WINDOWS.iter().any(|key| !headers.contains_key(*key)) {
        tx.commit().await?;
        return Ok(Json(non_ready("snapshot_unavailable", &metric, false)));
    }
    let labels = friend_labels(&mut tx).await?;
    let now = Utc::now();
    let next = now
        .with_minute(0)
        .and_then(|time| time.with_second(0))
        .and_then(|time| time.with_nanosecond(0))
        .and_then(|time| time.checked_add_signed(Duration::hours(1)))
        .ok_or_else(ApiError::internal)?;
    let mut windows = Vec::new();
    let mut best = None;
    for key in WINDOWS {
        let header = headers.get(key).ok_or_else(ApiError::internal)?;
        let snapshot: Uuid = header.try_get("snapshot_id")?;
        let participants = rank(entries(&mut tx, snapshot, false).await?, viewer, false);
        let viewer = viewer_json(&participants, false)?;
        let rank = viewer
            .get("rank")
            .and_then(Value::as_u64)
            .ok_or_else(ApiError::internal)?;
        if best.is_none_or(|(_, best_rank)| rank < best_rank) {
            best = Some((key, rank));
        }
        let (rows, ranking_rows) = ranked_rows(&participants, &labels, &locale, false)?;
        windows.push(json!({"windowKey":key,"snapshotId":snapshot,"snapshotGeneratedAt":stamp(header.try_get("generated_at")?),"asOfServerHour":stamp(header.try_get("as_of_server_hour")?),"nextRefreshAfter":stamp(next),"participantCount":participants.len(),"viewer":viewer,"rows":rows,"rankingRows":ranking_rows}));
    }
    tx.commit().await?;
    Ok(Json(
        json!({"status":"ready","metric":metric,"defaultWindowKey":best.map_or("last_24_hours",|(key,_)|key),"windows":windows}),
    ))
}
async fn streak(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (viewer, enabled, locale) = ensure_profile(&mut tx, &user).await?;
    let metric = metric(true, &locale)?;
    if !enabled {
        tx.commit().await?;
        return Ok(Json(non_ready("participation_disabled", &metric, true)));
    }
    let header=sqlx::query("SELECT snapshot_id,generated_at,as_of_utc_date FROM community.streak_leaderboard_snapshots WHERE metric_version=$1 ORDER BY as_of_utc_date DESC LIMIT 1").bind(STREAK).fetch_optional(&mut *tx).await?;
    let Some(header) = header else {
        tx.commit().await?;
        return Ok(Json(non_ready("snapshot_unavailable", &metric, true)));
    };
    let snapshot: Uuid = header.try_get("snapshot_id")?;
    let generated: DateTime<Utc> = header.try_get("generated_at")?;
    let date: NaiveDate = header.try_get("as_of_utc_date")?;
    let noon = generated
        .with_hour(12)
        .and_then(|time| time.with_minute(0))
        .and_then(|time| time.with_second(0))
        .and_then(|time| time.with_nanosecond(0))
        .ok_or_else(ApiError::internal)?;
    let next = if noon > generated {
        noon
    } else {
        noon.checked_add_signed(Duration::days(1))
            .ok_or_else(ApiError::internal)?
    };
    let participants = rank(entries(&mut tx, snapshot, true).await?, viewer, true);
    let labels = friend_labels(&mut tx).await?;
    let (rows, ranking_rows) = ranked_rows(&participants, &labels, &locale, true)?;
    tx.commit().await?;
    Ok(Json(
        json!({"status":"ready","metric":metric,"snapshotId":snapshot,"snapshotGeneratedAt":stamp(generated),"asOfUtcDate":date.to_string(),"nextRefreshAfter":stamp(next),"participantCount":participants.len(),"viewer":viewer_json(&participants,true)?,"rows":rows,"rankingRows":ranking_rows}),
    ))
}
async fn profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(target): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?.user_id.to_string();
    let Ok(target) = target.parse::<Uuid>() else {
        return Ok(Json(json!({"status":"profile_unavailable"})));
    };
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (_, enabled, locale) = ensure_profile(&mut tx, &user).await?;
    if !enabled {
        tx.commit().await?;
        return Ok(Json(json!({"status":"participation_disabled"})));
    }
    let now = Utc::now();
    let summary=sqlx::query("SELECT public_profile_id,joined_at,total_cards::bigint,activity_date::date,review_count::bigint FROM community.read_leaderboard_profile_summary($1::uuid,$2,$3,$4::timestamptz)").bind(target).bind(RATING).bind(STREAK).bind(now).fetch_all(&mut *tx).await?;
    let Some(first) = summary.first() else {
        tx.commit().await?;
        return Ok(Json(json!({"status":"profile_unavailable"})));
    };
    if summary.len() != 30 {
        return Err(ApiError::internal());
    }
    let joined: DateTime<Utc> = first.try_get("joined_at")?;
    let total: i64 = first.try_get("total_cards")?;
    let days: Vec<Value> = summary
        .iter()
        .map(|row| {
            let date: NaiveDate = row.try_get("activity_date")?;
            let count: i64 = row.try_get("review_count")?;
            Ok(json!({"date":date.to_string(),"reviewCount":count}))
        })
        .collect::<Result<_, sqlx::Error>>()?;
    let streak:i64=sqlx::query_scalar("WITH latest AS(SELECT snapshot_id FROM community.streak_leaderboard_snapshots WHERE metric_version=$1 ORDER BY as_of_utc_date DESC LIMIT 1) SELECT e.streak_days::bigint FROM latest JOIN community.streak_leaderboard_snapshot_entries e USING(snapshot_id) WHERE e.public_profile_id=$2 LIMIT 1").bind(STREAK).bind(target).fetch_optional(&mut *tx).await?.unwrap_or(0);
    let placements=sqlx::query("WITH latest AS(SELECT DISTINCT ON(window_key) window_key,snapshot_id FROM community.leaderboard_snapshots WHERE metric_version=$1 ORDER BY window_key,as_of_server_hour DESC) SELECT l.window_key,e.base_sort_position::bigint AS rank FROM latest l JOIN community.leaderboard_snapshot_entries e USING(snapshot_id) WHERE e.public_profile_id=$2").bind(RATING).bind(target).fetch_all(&mut *tx).await?;
    let placements: BTreeMap<String, i64> = placements
        .into_iter()
        .map(|row| Ok((row.try_get("window_key")?, row.try_get("rank")?)))
        .collect::<Result<_, sqlx::Error>>()?;
    let mut best = None;
    for key in WINDOWS {
        if let Some(rank) = placements.get(key)
            && best.is_none_or(|(_, best_rank)| *rank < best_rank)
        {
            best = Some((key, *rank));
        }
    }
    let labels = friend_labels(&mut tx).await?;
    tx.commit().await?;
    let mut response = json!({"status":"ready","publicProfileId":target,"anonymousDisplayName":anonymous_name(target,&locale)?,"isFriend":labels.contains_key(&target),"metrics":{"currentStreakDays":streak,"bestRatingPlacement":best.map(|(key,rank)|json!({"windowKey":key,"rank":rank}))},"reviewActivity":{"dateBasis":"profile_local_day_with_utc_fallback","days":days},"stats":{"joinedAt":stamp(joined),"totalCards":total},"generatedAt":stamp(now)});
    if let Some(label) = labels.get(&target) {
        insert(&mut response, "friendDisplayName", json!(label))?;
    }
    Ok(Json(response))
}
