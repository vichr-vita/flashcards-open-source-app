//! Community writes retain the existing security-definer invitations and account privacy scope.

mod leaderboard;

use crate::{
    AppState,
    auth::{authenticate, require_mutation},
    database::scoped,
    error::ApiError,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore as _;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/me/community/profile",
            get(profile).patch(update_profile),
        )
        .route("/v1/me/community/friend-invitations", post(create_invite))
        .route(
            "/v1/community/friend-invitations/{token}",
            get(preview_invite),
        )
        .route(
            "/v1/me/community/friend-invitations/{token}/accept",
            post(accept_invite),
        )
        .route("/v1/me/review-platform-summary", get(mobile_reviews))
        .merge(leaderboard::router())
}
pub(super) fn stamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WordPools {
    prefix_pool: Vec<String>,
    adjective_pool: Vec<String>,
    noun_pool: Vec<String>,
    separator: String,
}

pub(super) fn locale(value: &str) -> String {
    let tag = value
        .trim()
        .replace('_', "-")
        .trim_start_matches("b+")
        .replace('+', "-")
        .to_lowercase();
    let language = tag.split('-').next().unwrap_or_default();
    match language {
        "zh" if tag.contains("hans") || tag.ends_with("cn") || tag.ends_with("sg") => {
            "zh-Hans".into()
        }
        "es" if tag.ends_with("mx") || tag.ends_with("419") => "es-MX".into(),
        "es" => "es-ES".into(),
        "ar" | "de" | "en" | "fr" | "hi" | "ja" | "pt" | "ru" => language.into(),
        _ => "en".into(),
    }
}
pub(super) fn anonymous_name(profile: Uuid, locale_hint: &str) -> Result<String, ApiError> {
    let pools: BTreeMap<String, WordPools> =
        serde_json::from_str(include_str!("anonymous-names.json"))
            .map_err(|_| ApiError::internal())?;
    let locale = locale(locale_hint);
    let key = match locale.as_str() {
        "zh-Hans" => "zhHans",
        "es-MX" | "es-ES" => "es",
        other => other,
    };
    let pool = pools.get(key).ok_or_else(ApiError::internal)?;
    let hash = Sha256::digest(profile.to_string().as_bytes());
    let mut chunks = hash.as_chunks::<4>().0.iter();
    let mut words = Vec::new();
    for values in [&pool.prefix_pool, &pool.adjective_pool, &pool.noun_pool] {
        let bytes = *chunks.next().ok_or_else(ApiError::internal)?;
        let hash_value =
            usize::try_from(u32::from_be_bytes(bytes)).map_err(|_| ApiError::internal())?;
        let index = hash_value
            .checked_rem(values.len())
            .ok_or_else(ApiError::internal)?;
        words.push(values.get(index).ok_or_else(ApiError::internal)?.as_str());
    }
    Ok(words.join(&pool.separator))
}
pub(super) async fn ensure_profile(
    tx: &mut Transaction<'_, Postgres>,
    user: &str,
) -> Result<(Uuid, bool, String), ApiError> {
    let locale: String =
        sqlx::query_scalar("SELECT locale FROM org.user_settings WHERE user_id=$1")
            .bind(user)
            .fetch_one(&mut **tx)
            .await?;
    for _ in 0..24 {
        let row=sqlx::query("WITH inserted AS (INSERT INTO community.public_profiles(user_id,public_profile_id) VALUES($1,$2) ON CONFLICT DO NOTHING RETURNING public_profile_id,leaderboard_participation_enabled) SELECT * FROM inserted UNION ALL SELECT public_profile_id,leaderboard_participation_enabled FROM community.public_profiles WHERE user_id=$1 AND NOT EXISTS(SELECT 1 FROM inserted) LIMIT 1").bind(user).bind(Uuid::new_v4()).fetch_optional(&mut **tx).await?;
        if let Some(row) = row {
            return Ok((
                row.try_get("public_profile_id")?,
                row.try_get("leaderboard_participation_enabled")?,
                locale,
            ));
        }
    }
    Err(ApiError::internal())
}
pub(super) async fn friend_labels(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<BTreeMap<Uuid, String>, ApiError> {
    let rows=sqlx::query("SELECT friend_public_profile_id,friend_display_name FROM community.read_current_user_leaderboard_friend_labels() ORDER BY friend_public_profile_id").fetch_all(&mut **tx).await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get("friend_public_profile_id")?,
                row.try_get("friend_display_name")?,
            ))
        })
        .collect()
}
async fn profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (profile, enabled, locale) = ensure_profile(&mut tx, &user).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"publicProfileId":profile,"anonymousDisplayName":anonymous_name(profile,&locale)?,"leaderboardParticipationEnabled":enabled,"linkedAccountRequiredForLeaderboard":false}),
    ))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Participation {
    leaderboard_participation_enabled: bool,
}
async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Participation>,
) -> Result<Json<Value>, ApiError> {
    let user = require_mutation(&state, &headers)
        .await?
        .user_id
        .to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let (profile, _, locale) = ensure_profile(&mut tx, &user).await?;
    sqlx::query("UPDATE community.public_profiles SET leaderboard_participation_enabled=$2,updated_at=now() WHERE user_id=$1").bind(&user).bind(body.leaderboard_participation_enabled).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"publicProfileId":profile,"anonymousDisplayName":anonymous_name(profile,&locale)?,"leaderboardParticipationEnabled":body.leaderboard_participation_enabled,"linkedAccountRequiredForLeaderboard":false}),
    ))
}
fn display_name(value: Option<&Value>, field: &str) -> Result<String, ApiError> {
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "FRIEND_INVITATION_DISPLAY_NAME_INVALID",
            format!(
                "{field} must be 1 to 30 characters after trimming and contain no control characters."
            ),
        )
    };
    let value = value.and_then(Value::as_str).ok_or_else(invalid)?;
    let trimmed = value.trim();
    if value.chars().any(|c| c <= '\u{001f}' || c == '\u{007f}')
        || !(1..=30).contains(&trimmed.chars().count())
    {
        return Err(invalid());
    }
    Ok(trimmed.to_owned())
}
fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
async fn create_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user_id = require_mutation(&state, &headers).await?.user_id;
    let user = user_id.to_string();
    let name = display_name(body.get("inviteeDisplayName"), "inviteeDisplayName")?;
    let mut tx = scoped(&state.pool, &user, None).await?;
    ensure_profile(&mut tx, &user).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0::bigint))")
        .bind(format!("community.friend_invitations:{user}"))
        .execute(&mut *tx)
        .await?;
    let active:i64=sqlx::query_scalar("SELECT count(*) FROM community.friend_invitations WHERE inviter_user_id=$1 AND accepted_at IS NULL AND expires_at>now()").bind(&user).fetch_one(&mut *tx).await?;
    if active >= 20 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "FRIEND_INVITATION_LIMIT_REACHED",
            "You already have 20 active friend invitation links. Wait for one to expire or be accepted before creating another.",
        ));
    }
    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let invitation_id = Uuid::new_v4();
    let (expires, created): (DateTime<Utc>,DateTime<Utc>) = sqlx::query_as("INSERT INTO community.friend_invitations(friend_invitation_id,inviter_user_id,invite_token_hash,invitee_display_name_for_inviter,expires_at) VALUES($1,$2,$3,$4,now()+interval '2 days') RETURNING expires_at,created_at").bind(invitation_id).bind(&user).bind(token_hash(&token)).bind(name).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    let stable_key = invitation_id.to_string();
    crate::metadata::server_fact(
        &state,
        crate::metadata::ServerFact {
            name: "friend_invitation_created",
            stable_keys: &[&stable_key],
            user_id,
            subject_user_id: None,
            workspace_id: None,
            occurred_at: created,
            received_at: created,
            platform: None,
            properties: json!({}),
            details: None,
        },
    )
    .await;
    Ok(Json(
        json!({"inviteUrl":format!("{}/invite/{token}",state.config.backend_origin),"expiresAt":stamp(expires)}),
    ))
}
async fn preview_invite(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT invitation_status,expires_at FROM community.preview_friend_invitation($1)",
    )
    .bind(token_hash(&token))
    .fetch_one(&state.pool)
    .await?;
    let status: String = row.try_get("invitation_status")?;
    match status.as_str() {
        "inactive" => Ok(Json(json!({"status":"inactive"}))),
        "active" => {
            let expires: DateTime<Utc> = row.try_get("expires_at")?;
            Ok(Json(json!({"status":"active","expiresAt":stamp(expires)})))
        }
        _ => Err(ApiError::internal()),
    }
}
async fn accept_invite(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user_id = require_mutation(&state, &headers).await?.user_id;
    let user = user_id.to_string();
    let name = display_name(body.get("inviterDisplayName"), "inviterDisplayName")?;
    let mut tx = scoped(&state.pool, &user, None).await?;
    ensure_profile(&mut tx, &user).await?;
    let row=sqlx::query("SELECT acceptance_status,inviter_public_profile_id,invitee_public_profile_id FROM community.accept_friend_invitation($1,$2)").bind(token_hash(&token)).bind(name).fetch_one(&mut *tx).await?;
    let status: String = row.try_get("acceptance_status")?;
    let response = match status.as_str() {
        "accepted" => json!({"status":"accepted"}),
        "already_friends" => {
            let profile: Uuid = row.try_get("inviter_public_profile_id")?;
            let existing:String=sqlx::query_scalar("SELECT friend_display_name FROM community.friendships WHERE viewer_user_id=$1 AND friend_public_profile_id=$2 LIMIT 1").bind(&user).bind(profile).fetch_one(&mut *tx).await?;
            json!({"status":"already_friends","existingFriendDisplayName":existing})
        }
        "inactive" | "already_accepted" => json!({"status":"inactive"}),
        "self" => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "FRIEND_INVITATION_SELF",
                "This is your own invitation link.",
            ));
        }
        _ => return Err(ApiError::internal()),
    };
    let friendship = if status == "accepted" {
        sqlx::query("SAVEPOINT friendship_analytics_read")
            .execute(&mut *tx)
            .await?;
        let profile: Uuid = row.try_get("inviter_public_profile_id")?;
        let fact = sqlx::query("SELECT friend_user_id,created_from_invitation_id,created_at FROM community.friendships WHERE viewer_user_id=$1 AND friend_public_profile_id=$2 LIMIT 1")
            .bind(&user).bind(profile).fetch_optional(&mut *tx).await;
        if fact.is_err() {
            sqlx::query("ROLLBACK TO SAVEPOINT friendship_analytics_read")
                .execute(&mut *tx)
                .await?;
            tracing::warn!(user_id = %user_id, "Committed friendship analytics read was unavailable");
        }
        sqlx::query("RELEASE SAVEPOINT friendship_analytics_read")
            .execute(&mut *tx)
            .await?;
        fact.ok().flatten()
    } else {
        None
    };
    tx.commit().await?;
    if let Some(friendship) = friendship {
        emit_friendship_facts(&state, user_id, &friendship).await;
    }
    Ok(Json(response))
}

async fn emit_friendship_facts(state: &AppState, accepter: Uuid, row: &sqlx::postgres::PgRow) {
    let read = || -> Result<_, ApiError> {
        let inviter: String = row.try_get("friend_user_id")?;
        let inviter = inviter.parse::<Uuid>().map_err(|_| ApiError::internal())?;
        let invitation: Uuid = row.try_get("created_from_invitation_id")?;
        let created: DateTime<Utc> = row.try_get("created_at")?;
        Ok((inviter, invitation, created))
    };
    let Ok((inviter, invitation, created)) = read() else {
        tracing::warn!(user_id = %accepter, "Committed friendship analytics fields were unavailable");
        return;
    };
    let invitation = invitation.to_string();
    for user in [accepter, inviter] {
        let viewer = user.to_string();
        crate::metadata::server_fact(
            state,
            crate::metadata::ServerFact {
                name: "friendship_created",
                stable_keys: &[&invitation, &viewer],
                user_id: user,
                subject_user_id: None,
                workspace_id: None,
                occurred_at: created,
                received_at: created,
                platform: None,
                properties: json!({}),
                details: None,
            },
        )
        .await;
    }
}
async fn mobile_reviews(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = authenticate(&state, &headers).await?.user_id.to_string();
    let mut tx = scoped(&state.pool, &user, None).await?;
    let mobile: bool = sqlx::query_scalar("SELECT content.current_user_has_mobile_review_event()")
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"hasMobileReviewEvent":mobile})))
}
