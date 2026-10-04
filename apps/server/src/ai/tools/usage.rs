//! Read-only entitlement derivation and calendar-month usage from preserved billing facts.
use crate::{
    AppState,
    error::ApiError,
    metadata::{ServerFact, server_fact},
};
use axum::http::StatusCode;
use chrono::{DateTime, Datelike, Months, SecondsFormat, SubsecRound, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use uuid::Uuid;

const INPUTS: &str = "SELECT jsonb_build_object('purchases',COALESCE((SELECT jsonb_agg(to_jsonb(p)) FROM billing.purchases p WHERE user_id=$1 AND invalidated_at IS NULL AND account_deleted_at IS NULL),'[]'::jsonb),'grants',COALESCE((SELECT jsonb_agg(to_jsonb(g)) FROM billing.grants g WHERE user_id=$1),'[]'::jsonb),'cached',(SELECT to_jsonb(s) FROM billing.entitlement_snapshots s WHERE user_id=$1))";
#[derive(Deserialize)]
struct PurchaseInput {
    purchase_id: String,
    tier: String,
    status: String,
    until: Option<DateTime<Utc>>,
    grace_until: Option<DateTime<Utc>>,
    is_trial: bool,
    will_renew: bool,
}
#[derive(Deserialize)]
struct GrantInput {
    grant_id: String,
    tier: String,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}
#[derive(Deserialize)]
struct Cached {
    tier: String,
    status: String,
    until: Option<DateTime<Utc>>,
    is_trial: bool,
    will_renew: bool,
    source: String,
}
#[derive(Deserialize)]
struct Inputs {
    purchases: Vec<PurchaseInput>,
    grants: Vec<GrantInput>,
    cached: Option<Cached>,
}
struct Candidate {
    id: String,
    tier: String,
    status: String,
    until: Option<DateTime<Utc>>,
    is_trial: bool,
    will_renew: bool,
    purchase: bool,
}
struct Resolved {
    candidate: Option<Candidate>,
}
fn rank(tier: &str) -> Result<i32, ApiError> {
    match tier {
        "free" => Ok(10),
        "premium" => Ok(20),
        "lifetime" => Ok(30),
        _ => Err(ApiError::internal()),
    }
}
fn duration(candidate: &Candidate) -> i64 {
    candidate.until.map_or(
        if candidate.status == "active" {
            i64::MAX
        } else {
            i64::MIN
        },
        |date| date.timestamp_millis(),
    )
}
impl Resolved {
    fn tier(&self) -> &str {
        self.candidate
            .as_ref()
            .map_or("free", |row| row.tier.as_str())
    }
    fn status(&self) -> &str {
        self.candidate
            .as_ref()
            .map_or("none", |row| row.status.as_str())
    }
    fn until(&self) -> Option<DateTime<Utc>> {
        self.candidate
            .as_ref()
            .and_then(|row| row.until)
            .map(|date| date.trunc_subsecs(3))
    }
    fn source(&self) -> &str {
        self.candidate.as_ref().map_or(
            "none",
            |row| if row.purchase { "purchase" } else { "grant" },
        )
    }
    fn is_trial(&self) -> bool {
        self.candidate.as_ref().is_some_and(|row| row.is_trial)
    }
    fn will_renew(&self) -> bool {
        self.candidate.as_ref().is_some_and(|row| row.will_renew)
    }
    fn matches(&self, cached: &Cached) -> bool {
        self.tier() == cached.tier
            && self.status() == cached.status
            && self.until().map(|date| date.timestamp_millis())
                == cached.until.map(|date| date.timestamp_millis())
            && self.is_trial() == cached.is_trial
            && self.will_renew() == cached.will_renew
            && self.source() == cached.source
    }
    fn wire(&self) -> Result<Value, ApiError> {
        let tier = self.tier();
        let tier_rank = rank(tier)?;
        Ok(
            json!({"tier":tier,"tierRank":tier_rank,"tierDisplayName":match tier{"free"=>"Free","premium"=>"Premium",_=>"Lifetime"},"status":self.status(),"until":self.until().map(|date|date.to_rfc3339_opts(SecondsFormat::Millis,true)),"isTrial":self.is_trial(),"willRenew":self.will_renew(),"limits":{"aiMonthlyMessages":if tier_rank>=20{json!(1000)}else{Value::Null},"aiMonthlyWeightedTokens":null}}),
        )
    }
}

fn resolve(inputs: &Inputs, now: DateTime<Utc>) -> Result<Resolved, ApiError> {
    let mut candidates = Vec::new();
    for purchase in &inputs.purchases {
        let tier_rank = rank(&purchase.tier)?;
        if !matches!(
            purchase.status.as_str(),
            "active" | "in_grace" | "expired" | "revoked"
        ) {
            return Err(ApiError::internal());
        }
        if matches!(purchase.status.as_str(), "expired" | "revoked") {
            continue;
        }
        let until = if purchase.status == "in_grace" {
            purchase.grace_until
        } else {
            purchase.until
        };
        if until.is_some_and(|date| date <= now) {
            continue;
        }
        candidates.push((
            tier_rank,
            Candidate {
                id: purchase.purchase_id.clone(),
                tier: purchase.tier.clone(),
                status: purchase.status.clone(),
                until,
                is_trial: purchase.is_trial,
                will_renew: purchase.will_renew,
                purchase: true,
            },
        ));
    }
    for grant in &inputs.grants {
        let tier_rank = rank(&grant.tier)?;
        if grant.revoked_at.is_some() || grant.expires_at.is_some_and(|date| date <= now) {
            continue;
        }
        candidates.push((
            tier_rank,
            Candidate {
                id: grant.grant_id.clone(),
                tier: grant.tier.clone(),
                status: "active".into(),
                until: grant.expires_at,
                is_trial: false,
                will_renew: false,
                purchase: false,
            },
        ));
    }
    candidates.sort_by(|(a_rank, a), (b_rank, b)| {
        a_rank
            .cmp(b_rank)
            .then_with(|| (a.status == "active").cmp(&(b.status == "active")))
            .then_with(|| duration(a).cmp(&duration(b)))
            .then_with(|| a.will_renew.cmp(&b.will_renew))
            .then_with(|| b.is_trial.cmp(&a.is_trial))
            .then_with(|| a.purchase.cmp(&b.purchase))
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(Resolved {
        candidate: candidates.pop().map(|(_, row)| row),
    })
}
fn inputs(value: Value) -> Result<Inputs, ApiError> {
    serde_json::from_value(value).map_err(|_| ApiError::internal())
}

pub(super) async fn entitlement(state: &AppState, user: Uuid) -> Result<Value, ApiError> {
    let now = Utc::now().trunc_subsecs(3);
    let value = sqlx::query_scalar::<_, Value>(INPUTS)
        .bind(user.to_string())
        .fetch_one(&state.pool)
        .await?;
    resolve(&inputs(value)?, now)?.wire()
}

/// The mutable snapshot is only a cache. Resolve again behind its lock and emit a transition only
/// when this write replaced a known previous state, after commit.
pub(super) async fn refresh(state: &AppState, user: Uuid) -> Result<Value, ApiError> {
    let now = Utc::now().trunc_subsecs(3);
    let user_text = user.to_string();
    let current = inputs(
        sqlx::query_scalar::<_, Value>(INPUTS)
            .bind(&user_text)
            .fetch_one(&state.pool)
            .await?,
    )?;
    let resolved = resolve(&current, now)?;
    if current
        .cached
        .as_ref()
        .is_some_and(|cached| resolved.matches(cached))
    {
        return resolved.wire();
    }
    let mut tx = state.pool.begin().await?;
    let previous: Option<(String, String)> = sqlx::query_as(
        "SELECT tier,status FROM billing.entitlement_snapshots WHERE user_id=$1 FOR UPDATE",
    )
    .bind(&user_text)
    .fetch_optional(&mut *tx)
    .await?;
    let current = inputs(
        sqlx::query_scalar::<_, Value>(INPUTS)
            .bind(&user_text)
            .fetch_one(&mut *tx)
            .await?,
    )?;
    let resolved = resolve(&current, now)?;
    let written = if current
        .cached
        .as_ref()
        .is_some_and(|cached| resolved.matches(cached))
    {
        false
    } else {
        sqlx::query("INSERT INTO billing.entitlement_snapshots(user_id,tier,status,until,is_trial,will_renew,source,computed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT(user_id) DO UPDATE SET tier=EXCLUDED.tier,status=EXCLUDED.status,until=EXCLUDED.until,is_trial=EXCLUDED.is_trial,will_renew=EXCLUDED.will_renew,source=EXCLUDED.source,computed_at=EXCLUDED.computed_at WHERE billing.entitlement_snapshots.computed_at<EXCLUDED.computed_at")
            .bind(&user_text).bind(resolved.tier()).bind(resolved.status()).bind(resolved.until()).bind(resolved.is_trial()).bind(resolved.will_renew()).bind(resolved.source()).bind(now).execute(&mut *tx).await?.rows_affected()>0
    };
    tx.commit().await?;
    if written
        && let Some((from_tier, from_status)) = previous
        && (from_tier != resolved.tier() || from_status != resolved.status())
    {
        if rank(&from_tier).is_ok()
            && matches!(from_status.as_str(), "none" | "active" | "in_grace")
        {
            let time = now.to_rfc3339_opts(SecondsFormat::Millis, true);
            server_fact(state,ServerFact{name:"entitlement_changed",stable_keys:&[&user_text,&time],user_id:user,subject_user_id:Some(user),workspace_id:None,occurred_at:now,received_at:now,platform:None,properties:json!({"from_tier":from_tier,"to_tier":resolved.tier(),"from_status":from_status,"to_status":resolved.status(),"source":resolved.source()}),details:None}).await;
        } else {
            tracing::warn!(user_id=%user,"Entitlement cache held an unknown previous state; its transition fact was omitted");
        }
    }
    resolved.wire()
}

pub(super) async fn tier(state: &AppState, user: Uuid) -> Result<String, ApiError> {
    entitlement(state, user)
        .await?
        .get("tier")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(ApiError::internal)
}

pub(super) async fn usage(state: &AppState, user: Uuid) -> Result<Value, ApiError> {
    let entitlement = entitlement(state, user).await?;
    let now = Utc::now();
    let start = now
        .with_day(1)
        .and_then(|date| date.date_naive().and_hms_opt(0, 0, 0))
        .ok_or_else(ApiError::internal)?
        .and_utc();
    let end = start
        .checked_add_months(Months::new(1))
        .ok_or_else(ApiError::internal)?;
    let row=sqlx::query("SELECT COUNT(DISTINCT request_id) FILTER(WHERE surface='chat' AND user_supplied_key=false)::bigint AS used_messages,COUNT(DISTINCT request_id) FILTER(WHERE surface='chat' AND user_supplied_key=true)::bigint AS own_messages,COALESCE(SUM(CASE WHEN user_supplied_key=false THEN COALESCE(input_tokens,0)+6*COALESCE(output_tokens,0) ELSE 0 END),0)::bigint AS weighted_tokens FROM ai.usage_events WHERE user_id=$1 AND occurred_at>=$2 AND occurred_at<$3").bind(user.to_string()).bind(start).bind(end).fetch_one(&state.pool).await?;
    let consumed: i64 = row.try_get("used_messages")?;
    let own: i64 = row.try_get("own_messages")?;
    let weighted: i64 = row.try_get("weighted_tokens")?;
    let remaining = entitlement
        .get("limits")
        .and_then(|limits| limits.get("aiMonthlyMessages"))
        .and_then(Value::as_i64)
        .map(|limit| limit.saturating_sub(consumed).max(0));
    Ok(
        json!({"accountKind":"account","entitlement":entitlement,"usage":{"monthStartsAt":start.to_rfc3339_opts(SecondsFormat::Millis,true),"monthEndsAt":end.to_rfc3339_opts(SecondsFormat::Millis,true),"usedMessages":consumed,"remainingMessages":remaining,"ownKeyMessages":own,"usedWeightedTokens":weighted,"remainingWeightedTokens":null,"weightedOutputTokenMultiplier":6}}),
    )
}

pub(super) async fn assert_allowance(state: &AppState, user: Uuid) -> Result<(), ApiError> {
    let status = usage(state, user).await?;
    if status
        .get("usage")
        .and_then(|value| value.get("remainingMessages"))
        .and_then(Value::as_i64)
        == Some(0)
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "AI_LIMIT_REACHED",
            "Your free monthly AI limit is used up on this device. Create an account to keep going.",
        ));
    }
    Ok(())
}
