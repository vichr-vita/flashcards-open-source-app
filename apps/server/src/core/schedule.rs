//! Exact FSRS-6 transitions ported from the deployed TypeScript scheduler.
//! Floating arithmetic, UTF-16 mash input, and JS truncation are deliberate parity requirements.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::suboptimal_flops,
    clippy::imprecise_flops,
    clippy::manual_midpoint,
    reason = "This module mirrors the bounded floating-point FSRS and JavaScript Alea formulas; persisted integer inputs are validated before scheduling."
)]

use super::model::{CardSnapshot, SchedulerConfig};
use crate::error::ApiError;
use chrono::{DateTime, Duration, Utc};

const W: [f64; 21] = [
    0.212, 1.2931, 2.3065, 8.2956, 6.4133, 0.8334, 3.0194, 0.001, 1.8722, 0.1666, 0.796, 1.4835,
    0.0614, 0.2629, 1.6483, 0.6014, 1.8729, 0.5425, 0.0912, 0.0658, 0.1542,
];
const DECAY: f64 = -0.1542;
const S_MIN: f64 = 0.001;

#[derive(Clone, Copy)]
struct Memory {
    stability: f64,
    difficulty: f64,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub due_at: DateTime<Utc>,
    pub reps: i32,
    pub lapses: i32,
    pub fsrs_card_state: String,
    pub fsrs_step_index: Option<i32>,
    pub fsrs_stability: f64,
    pub fsrs_difficulty: f64,
    pub fsrs_last_reviewed_at: DateTime<Utc>,
    pub fsrs_scheduled_days: i32,
}

fn w(index: usize) -> f64 {
    W.get(index).copied().unwrap_or_default()
}
fn round8(value: f64) -> f64 {
    format!("{value:.8}").parse().unwrap_or(value)
}
fn initial(grade: f64) -> Memory {
    let index = (grade - 1.0) as usize;
    Memory {
        stability: w(index).max(0.1),
        difficulty: difficulty_initial(grade).clamp(1.0, 10.0),
    }
}
fn difficulty_initial(grade: f64) -> f64 {
    round8(w(4) - ((grade - 1.0) * w(5)).exp() + 1.0)
}
fn difficulty_next(difficulty: f64, grade: f64) -> f64 {
    let delta = -w(6) * (grade - 3.0);
    let damped = round8(delta * (10.0 - difficulty) / 9.0);
    round8(w(7) * difficulty_initial(4.0) + (1.0 - w(7)) * (difficulty + damped)).clamp(1.0, 10.0)
}
fn short_weights(config: &SchedulerConfig) -> (f64, f64) {
    if config.relearning_steps_minutes.len() <= 1 {
        return (w(17), w(18));
    }
    let count = config.relearning_steps_minutes.len() as f64;
    let ceiling = round8(-(w(11).ln() + (2.0_f64.powf(w(13)) - 1.0).ln() + w(14) * 0.3) / count)
        .clamp(0.01, 2.0);
    (w(17).clamp(0.0, ceiling), w(18).clamp(0.0, ceiling))
}
fn short_memory(memory: Memory, grade: f64, config: &SchedulerConfig) -> Memory {
    let (w17, w18) = short_weights(config);
    let sinc = memory.stability.powf(-w(19)) * (w17 * (grade - 3.0 + w18)).exp();
    let masked = if grade >= 3.0 { sinc.max(1.0) } else { sinc };
    Memory {
        stability: round8((memory.stability * masked).clamp(S_MIN, 36_500.0)),
        difficulty: difficulty_next(memory.difficulty, grade),
    }
}
fn review_memory(memory: Memory, elapsed: f64, grade: f64, config: &SchedulerConfig) -> Memory {
    let factor = round8((DECAY.recip() * 0.9_f64.ln()).exp() - 1.0);
    let retrievability = round8((1.0 + factor * elapsed / memory.stability).powf(DECAY));
    let hard = if grade == 2.0 { w(15) } else { 1.0 };
    let easy = if grade == 4.0 { w(16) } else { 1.0 };
    let success = round8(
        (memory.stability
            * (1.0
                + w(8).exp()
                    * (11.0 - memory.difficulty)
                    * memory.stability.powf(-w(9))
                    * (((1.0 - retrievability) * w(10)).exp() - 1.0)
                    * hard
                    * easy))
            .clamp(S_MIN, 36_500.0),
    );
    let failure = round8(
        (w(11)
            * memory.difficulty.powf(-w(12))
            * ((memory.stability + 1.0).powf(w(13)) - 1.0)
            * ((1.0 - retrievability) * w(14)).exp())
        .clamp(S_MIN, 36_500.0),
    );
    let stability = if grade == 1.0 {
        let (w17, w18) = short_weights(config);
        round8(memory.stability / (w17 * w18).exp()).clamp(S_MIN, failure)
    } else {
        success
    };
    Memory {
        stability,
        difficulty: difficulty_next(memory.difficulty, grade),
    }
}

fn mash(n: &mut f64, text: &str) -> f64 {
    let mut next = *n;
    for code in text.encode_utf16() {
        next += f64::from(code);
        let mut h = 0.025_196_032_824_169_38 * next;
        next = f64::from(h as u32);
        h -= next;
        h *= next;
        next = f64::from(h as u32);
        h -= next;
        next += h * 4_294_967_296.0;
    }
    *n = next;
    // JavaScript >>> 0 wraps modulo 2^32 rather than saturating at u32::MAX.
    f64::from(next.trunc().rem_euclid(4_294_967_296.0) as u32) * 2.328_306_436_538_696_3e-10
}
fn random(seed: &str) -> f64 {
    let mut n = 4_022_871_197.0;
    let mut s0 = mash(&mut n, " ");
    let mut s1 = mash(&mut n, " ");
    let mut s2 = mash(&mut n, " ");
    s0 -= mash(&mut n, seed);
    if s0 < 0.0 {
        s0 += 1.0;
    }
    s1 -= mash(&mut n, seed);
    if s1 < 0.0 {
        s1 += 1.0;
    }
    s2 -= mash(&mut n, seed);
    if s2 < 0.0 {
        s2 += 1.0;
    }
    let _ = s1 + s2; // Seed all three state slots, as the original Alea constructor does.
    let t = 2_091_639.0 * s0 + 2.328_306_436_538_696_3e-10;
    t - f64::from(t as i32)
}
fn interval(stability: f64, elapsed: f64, config: &SchedulerConfig, seed: &str) -> i32 {
    let factor = round8((DECAY.recip() * 0.9_f64.ln()).exp() - 1.0);
    let modifier = round8((config.desired_retention.powf(DECAY.recip()) - 1.0) / factor);
    let maximum = f64::from(config.maximum_interval_days);
    let raw = (stability * modifier).round().clamp(1.0, maximum);
    if !config.enable_fuzz || raw < 3.0 {
        return raw as i32;
    }
    let mut delta = 1.0;
    for (start, end, factor) in [
        (2.5, 7.0, 0.15),
        (7.0, 20.0, 0.1),
        (20.0, f64::INFINITY, 0.05),
    ] {
        delta += factor * (raw.min(end) - start).max(0.0);
    }
    let mut min = (raw - delta).round().max(2.0);
    let max = (raw + delta).round().min(maximum);
    if raw > elapsed {
        min = min.max(elapsed + 1.0);
    }
    min = min.min(max);
    (random(seed) * (max - min + 1.0) + min).floor() as i32
}

fn learning_step(
    card: &CardSnapshot,
    grade: u8,
    config: &SchedulerConfig,
) -> Result<(Option<i32>, i32), ApiError> {
    let steps = if card.fsrs_card_state == "relearning" || card.fsrs_card_state == "review" {
        &config.relearning_steps_minutes
    } else {
        &config.learning_steps_minutes
    };
    let first = steps.first().copied().ok_or_else(ApiError::internal)?;
    let current = card.fsrs_step_index.unwrap_or_default();
    let strategy = if card.fsrs_card_state == "learning" && grade > 2 {
        current + 1
    } else {
        current
    };
    if card.fsrs_card_state == "review" || grade == 1 {
        return Ok((Some(first), 0));
    }
    if grade == 2 {
        let hard = steps.get(1).map_or_else(
            || (f64::from(first) * 1.5).round(),
            |second| ((f64::from(first) + f64::from(*second)) / 2.0).round(),
        );
        return Ok((Some(hard as i32), strategy));
    }
    if grade == 4 {
        return Ok((None, 0));
    }
    let next = strategy + 1;
    Ok((
        usize::try_from(next)
            .ok()
            .and_then(|idx| steps.get(idx).copied()),
        next,
    ))
}

/// Apply one rating using persisted state. Runtime reads never reconstruct state from history.
///
/// # Errors
/// Rejects invalid persisted state, invalid ratings, backwards review times and timestamp overflow.
#[allow(
    clippy::too_many_lines,
    clippy::option_if_let_else,
    reason = "Keep the four established FSRS transition branches adjacent to their exact numeric formulas for parity review."
)]
pub fn compute_review_schedule(
    card: &CardSnapshot,
    config: &SchedulerConfig,
    rating: u8,
    now: DateTime<Utc>,
) -> Result<Schedule, ApiError> {
    card.validate()?;
    config.validate()?;
    if rating > 3 {
        return Err(ApiError::bad_request("rating must be between 0 and 3"));
    }
    let grade = rating + 1;
    let elapsed = match card.fsrs_last_reviewed_at {
        Some(last) if last > now => {
            return Err(ApiError::bad_request("Review timestamp moved backwards"));
        }
        Some(last) => now
            .date_naive()
            .signed_duration_since(last.date_naive())
            .num_days() as f64,
        None => 0.0,
    };
    let reps = card.reps.checked_add(1).ok_or_else(ApiError::internal)?;
    let lapses = if rating == 0 && card.fsrs_card_state == "review" {
        card.lapses.checked_add(1).ok_or_else(ApiError::internal)?
    } else {
        card.lapses
    };
    let memory = match (card.fsrs_stability, card.fsrs_difficulty) {
        (Some(stability), Some(difficulty)) => Some(Memory {
            stability,
            difficulty,
        }),
        _ => None,
    };
    let product = memory.map_or(0.0, |m| m.difficulty * m.stability);
    let seed = format!(
        "{}_{}_{}",
        now.timestamp_millis(),
        reps,
        if product == 0.0 {
            "0".into()
        } else {
            product.to_string()
        }
    );
    let short = card.fsrs_card_state != "review" || rating == 0;
    let (next, days, minutes, step, state) = if short {
        let next = if card.fsrs_card_state == "new" {
            initial(f64::from(grade))
        } else {
            let previous = memory.ok_or_else(ApiError::internal)?;
            if card.fsrs_card_state == "review" {
                review_memory(previous, elapsed, f64::from(grade), config)
            } else {
                short_memory(previous, f64::from(grade), config)
            }
        };
        let (minutes, step) = learning_step(card, grade, config)?;
        match minutes {
            Some(minutes) => (
                next,
                0,
                Some(minutes),
                Some(step),
                if card.fsrs_card_state == "review" {
                    "relearning"
                } else if card.fsrs_card_state == "new" {
                    "learning"
                } else {
                    card.fsrs_card_state.as_str()
                },
            ),
            None => (
                next,
                interval(next.stability, elapsed, config, &seed),
                None,
                None,
                "review",
            ),
        }
    } else {
        let previous = memory.ok_or_else(ApiError::internal)?;
        let hard = review_memory(previous, elapsed, 2.0, config);
        let good = review_memory(previous, elapsed, 3.0, config);
        let easy = review_memory(previous, elapsed, 4.0, config);
        let hard_interval = interval(hard.stability, elapsed, config, &seed).min(interval(
            good.stability,
            elapsed,
            config,
            &seed,
        ));
        let good_interval = interval(good.stability, elapsed, config, &seed).max(hard_interval + 1);
        let easy_interval = interval(easy.stability, elapsed, config, &seed).max(good_interval + 1);
        let (next, days) = match rating {
            1 => (hard, hard_interval),
            2 => (good, good_interval),
            _ => (easy, easy_interval),
        };
        (next, days, None, None, "review")
    };
    let duration = minutes.map_or_else(
        || Duration::days(i64::from(days)),
        |value| Duration::minutes(i64::from(value)),
    );
    let due_at = now
        .checked_add_signed(duration)
        .ok_or_else(ApiError::internal)?;
    Ok(Schedule {
        due_at,
        reps,
        lapses,
        fsrs_card_state: state.into(),
        fsrs_step_index: step,
        fsrs_stability: next.stability,
        fsrs_difficulty: next.difficulty,
        fsrs_last_reviewed_at: now,
        fsrs_scheduled_days: days,
    })
}
