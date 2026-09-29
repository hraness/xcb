use crate::{Error, Id, Provider, Result};
use serde::{Deserialize, Serialize};

pub const COUNTER_LIMIT: u64 = 1_000_000_000_000;
pub const QUOTA_FRESH_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counters {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: Option<u64>,
}
impl Counters {
    pub fn total(self) -> Result<u64> {
        if [self.input, self.cache_read, self.cache_write, self.output]
            .into_iter()
            .any(|n| n > COUNTER_LIMIT)
            || self.reasoning.is_some_and(|n| n > self.output)
        {
            return Err(Error::Invalid("token counters"));
        }
        Ok(self.input + self.cache_read + self.cache_write + self.output)
    }
    pub fn dominates(self, prior: Self) -> bool {
        self.input >= prior.input
            && self.cache_read >= prior.cache_read
            && self.cache_write >= prior.cache_write
            && self.output >= prior.output
            && match (self.reasoning, prior.reasoning) {
                (Some(now), Some(old)) => now >= old,
                (None, None) => true,
                _ => false,
            }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuotaPoint {
    pub pool: Id,
    pub window: Id,
    pub used_percent: f64,
    pub resets_at_ms: u64,
    pub observed_at_ms: u64,
}
impl QuotaPoint {
    pub fn validate(&self) -> Result<()> {
        if !self.used_percent.is_finite()
            || !(0.0..=100.0).contains(&self.used_percent)
            || self.resets_at_ms <= self.observed_at_ms
        {
            return Err(Error::Invalid("quota observation"));
        }
        Ok(())
    }
    pub fn fresh(&self, now: u64) -> bool {
        self.validate().is_ok()
            && now >= self.observed_at_ms
            && now - self.observed_at_ms <= QUOTA_FRESH_MS
            && now < self.resets_at_ms
    }
}

/// The windows that carry account-scope quota for each provider. Claude uses
/// `five_hour`/`seven_day`; Codex's account-level ChatGPT windows arrive as
/// `codex.primary`/`codex.secondary`; Devin has no account-scope meter. The
/// same vocabulary gates admission (`quota_blocked_until`) and feeds the
/// read-only usage projection so a window that can block a lease is the same
/// window a projection reports.
pub fn account_windows(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Claude => &["five_hour", "seven_day"],
        Provider::Codex => &["codex.primary", "codex.secondary"],
        Provider::Devin => &[],
    }
}

/// The synthetic account-wide window that records a provider usage limit
/// whose reset the provider did not report (a Codex `usageLimitExceeded`, a
/// Devin resource-exhaustion error, or a Claude rejection without a reset).
/// Its points are always 100% used and reset at the end of a policy-chosen
/// cooldown. The name never collides with a provider-reported window, and a
/// newer observation of any real account-wide window supersedes it.
pub fn limit_window(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude.limit",
        Provider::Codex => "codex.limit",
        Provider::Devin => "devin.limit",
    }
}

fn latest_point<'a>(
    points: &'a [QuotaPoint],
    pool: &Id,
    window: &str,
    now: u64,
) -> Option<&'a QuotaPoint> {
    points
        .iter()
        .filter(|point| {
            &point.pool == pool
                && point.window.as_str() == window
                && point.validate().is_ok()
                && point.observed_at_ms <= now
        })
        // Equal-time conflicting observations cannot enter the Store. Be
        // conservative if this pure helper receives such a slice anyway.
        .max_by(|a, b| {
            a.observed_at_ms
                .cmp(&b.observed_at_ms)
                .then_with(|| a.used_percent.total_cmp(&b.used_percent))
                .then(a.resets_at_ms.cmp(&b.resets_at_ms))
        })
}

/// The latest observation in each known account-wide window is authoritative
/// until its reported reset, even after percentage telemetry becomes stale.
/// Callers must bind `pool` to the current account credential generation;
/// model-specific and other-provider windows are deliberately not inferred
/// to have account scope.
///
/// A [`limit_window`] point is a fallback for a limit with no reported reset:
/// it blocks until its cooldown ends unless a real account-wide window was
/// observed after it, in which case the real windows alone decide, so a later
/// provider-reported reset (shorter or longer) or a later meter showing
/// capacity always supersedes the cooldown. Between the cooldown and a real
/// block that both apply, the later reset wins, as between real windows.
pub fn quota_blocked_until(
    points: &[QuotaPoint],
    pool: &Id,
    provider: Provider,
    now: u64,
) -> Option<u64> {
    let latest: Vec<&QuotaPoint> = account_windows(provider)
        .iter()
        .filter_map(|window| latest_point(points, pool, window, now))
        .collect();
    let reported = latest
        .iter()
        .filter(|point| point.used_percent == 100.0 && now < point.resets_at_ms)
        .map(|point| point.resets_at_ms)
        .max();
    let newest_reported = latest.iter().map(|point| point.observed_at_ms).max();
    let cooldown = latest_point(points, pool, limit_window(provider), now)
        .filter(|point| {
            point.used_percent == 100.0
                && now < point.resets_at_ms
                && newest_reported.is_none_or(|observed| observed <= point.observed_at_ms)
        })
        .map(|point| point.resets_at_ms);
    reported.max(cooldown)
}

/// A measured pace for spending one account's remaining quota before reset.
/// Percentage points are a routing heuristic, not comparable token or money
/// capacities across subscription plans. This never establishes entitlement.
#[derive(Debug, Clone, PartialEq)]
pub struct QuotaSpendingPressure {
    pub window: Id,
    pub remaining_percent: f64,
    pub resets_at_ms: u64,
    pub percent_per_hour: f64,
}

/// The tightest fresh account-wide window limits the spending pace. Taking
/// the minimum keeps an imminent short-window reset from draining a scarce
/// weekly allowance. Every remaining percentage stays paired with its own
/// reset; model-scoped meters are not inferred to be account-wide budgets.
/// A still-active but stale known window makes the pace unknown, while a
/// passed reset contributes nothing until a new observation arrives.
pub fn quota_spending_pressure(
    points: &[QuotaPoint],
    pool: &Id,
    provider: Provider,
    now: u64,
) -> Option<QuotaSpendingPressure> {
    let mut pressure: Option<QuotaSpendingPressure> = None;
    for window in account_windows(provider) {
        let Some(point) = points
            .iter()
            .filter(|point| {
                &point.pool == pool
                    && point.window.as_str() == *window
                    && point.validate().is_ok()
                    && point.observed_at_ms <= now
            })
            .max_by(|a, b| {
                a.observed_at_ms
                    .cmp(&b.observed_at_ms)
                    .then_with(|| a.used_percent.total_cmp(&b.used_percent))
                    .then(a.resets_at_ms.cmp(&b.resets_at_ms))
            })
        else {
            continue;
        };
        if point.resets_at_ms <= now {
            continue;
        }
        if !point.fresh(now) {
            return None;
        }
        let remaining_percent = 100.0 - point.used_percent;
        // A one-minute floor prevents a nearly elapsed reset from creating
        // an unbounded value or magnifying clock jitter.
        let hours = (point.resets_at_ms - now).max(60_000) as f64 / 3_600_000.0;
        let next = QuotaSpendingPressure {
            window: point.window.clone(),
            remaining_percent,
            resets_at_ms: point.resets_at_ms,
            percent_per_hour: remaining_percent / hours,
        };
        if pressure
            .as_ref()
            .is_none_or(|current| next.percent_per_hour < current.percent_per_hour)
        {
            pressure = Some(next);
        }
    }
    pressure
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Estimate {
    Known { seconds: f64 },
    Unknown { reason: String },
}
impl Estimate {
    pub fn unknown(reason: &str) -> Self {
        Self::Unknown {
            reason: reason.to_owned(),
        }
    }
    pub fn seconds(&self) -> Option<f64> {
        match self {
            Self::Known { seconds } => Some(*seconds),
            Self::Unknown { .. } => None,
        }
    }
}

pub fn runway(samples: &[QuotaPoint], now: u64) -> Estimate {
    if samples.len() < 2 || samples.len() > 128 {
        return Estimate::unknown("insufficient_samples");
    }
    let first = &samples[0];
    let last = &samples[samples.len() - 1];
    if !last.fresh(now) {
        return Estimate::unknown("stale_quota");
    }
    for (index, sample) in samples.iter().enumerate() {
        if sample.validate().is_err()
            || sample.pool != first.pool
            || sample.window != first.window
            || sample.resets_at_ms != first.resets_at_ms
        {
            return Estimate::unknown("quota_window_changed");
        }
        if index > 0 {
            let previous = &samples[index - 1];
            if sample.observed_at_ms <= previous.observed_at_ms
                || sample.used_percent < previous.used_percent
            {
                return Estimate::unknown("nonmonotonic_quota");
            }
            if sample.observed_at_ms - previous.observed_at_ms > QUOTA_FRESH_MS {
                return Estimate::unknown("sample_gap");
            }
        }
    }
    let elapsed = last.observed_at_ms - first.observed_at_ms;
    let delta = last.used_percent - first.used_percent;
    if elapsed < 10_000 || delta <= 0.0 {
        return Estimate::unknown("insufficient_burn");
    }
    let seconds = (100.0 - last.used_percent) * (elapsed as f64 / 1000.0) / delta;
    if !seconds.is_finite() {
        return Estimate::unknown("invalid_estimate");
    }
    if seconds > (last.resets_at_ms - now) as f64 / 1000.0 {
        return Estimate::unknown("resets_before_exhaustion");
    }
    Estimate::Known { seconds }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VelocitySample {
    pub at_ms: u64,
    pub output_tokens: u64,
}

pub fn velocity(samples: &[VelocitySample], now: u64, horizon_ms: u64) -> Option<f64> {
    if samples.len() < 2 || samples.len() > 2048 || !(1000..=900_000).contains(&horizon_ms) {
        return None;
    }
    let last = samples.last()?;
    if now < last.at_ms || now - last.at_ms > 90_000 {
        return None;
    }
    for pair in samples.windows(2) {
        if pair[1].at_ms <= pair[0].at_ms
            || pair[1].output_tokens < pair[0].output_tokens
            || pair[1].output_tokens > COUNTER_LIMIT
        {
            return None;
        }
    }
    let first = samples
        .iter()
        .find(|sample| sample.at_ms >= now.saturating_sub(horizon_ms))?;
    let elapsed = last.at_ms.checked_sub(first.at_ms)?;
    if elapsed == 0 {
        return None;
    }
    Some((last.output_tokens - first.output_tokens) as f64 * 1000.0 / elapsed as f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Heat {
    Unknown,
    Cool,
    Warm,
    Hot,
}

pub fn throughput_share(own: Option<f64>, total: Option<f64>) -> (Option<f64>, Heat) {
    let (Some(own), Some(total)) = (own, total) else {
        return (None, Heat::Unknown);
    };
    if !own.is_finite() || !total.is_finite() || own < 0.0 || total <= 0.0 || own > total {
        return (None, Heat::Unknown);
    }
    let share = own / total * 100.0;
    (
        Some(share),
        if share >= 50.0 {
            Heat::Hot
        } else if share >= 20.0 {
            Heat::Warm
        } else {
            Heat::Cool
        },
    )
}

#[cfg(test)]
mod quota_availability_tests {
    use super::*;

    fn point(window: &str, used: f64, observed: u64, reset: u64) -> QuotaPoint {
        QuotaPoint {
            pool: Id::new("bound").unwrap(),
            window: Id::new(window).unwrap(),
            used_percent: used,
            observed_at_ms: observed,
            resets_at_ms: reset,
        }
    }

    #[test]
    fn quota_availability_outlives_telemetry_and_uses_latest_per_window_max_reset() {
        let pool = Id::new("bound").unwrap();
        let mut points = vec![
            point("seven_day", 100.0, 1, 9_000_000),
            point("five_hour", 100.0, 2, 2_000_000),
        ];
        assert!(!points[0].fresh(500_000));
        assert_eq!(
            quota_blocked_until(&points, &pool, Provider::Claude, 500_000),
            Some(9_000_000)
        );
        points.push(point("five_hour", 20.0, 499_999, 2_000_000));
        assert_eq!(
            quota_blocked_until(&points, &pool, Provider::Claude, 500_000),
            Some(9_000_000)
        );
        points.push(point("seven_day", 10.0, 500_000, 10_000_000));
        points.reverse();
        assert_eq!(
            quota_blocked_until(&points, &pool, Provider::Claude, 500_000),
            None
        );
    }

    #[test]
    fn quota_availability_has_exact_observed_and_reset_boundaries() {
        let p = point("five_hour", 100.0, 10, 20);
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, Provider::Claude, 9),
            None
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, Provider::Claude, 10),
            Some(20)
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, Provider::Claude, 19),
            Some(20)
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, Provider::Claude, 20),
            None
        );
    }

    #[test]
    fn quota_availability_never_infers_pool_model_or_provider_scope() {
        let pool = Id::new("bound").unwrap();
        let mut foreign = point("five_hour", 100.0, 1, 1000);
        foreign.pool = Id::new("foreign").unwrap();
        let points = [
            foreign,
            point("seven_day_opus", 100.0, 1, 1000),
            point("seven_day_sonnet", 100.0, 1, 1000),
            point("primary", 100.0, 1, 1000),
            point("five_hour", 99.99, 1, 1000),
        ];
        assert_eq!(
            quota_blocked_until(&points, &pool, Provider::Claude, 2),
            None
        );
        assert_eq!(
            quota_blocked_until(
                &[point("seven_day", f64::NAN, 1, 1000)],
                &pool,
                Provider::Claude,
                2
            ),
            None
        );
        // Claude window names never scope a Codex account and vice versa.
        assert_eq!(
            quota_blocked_until(
                &[point("codex.primary", 100.0, 1, 1000)],
                &pool,
                Provider::Claude,
                2
            ),
            None
        );
        assert_eq!(
            quota_blocked_until(
                &[point("codex.primary", 100.0, 1, 1000)],
                &pool,
                Provider::Codex,
                2
            ),
            Some(1000)
        );
        assert_eq!(
            quota_blocked_until(
                &[point("five_hour", 100.0, 1, 1000)],
                &pool,
                Provider::Codex,
                2
            ),
            None
        );
        assert_eq!(
            quota_blocked_until(
                &[point("five_hour", 100.0, 1, 1000)],
                &pool,
                Provider::Devin,
                2
            ),
            None
        );
    }

    #[test]
    fn unknown_reset_cooldown_blocks_until_it_ends_for_every_provider() {
        let pool = Id::new("bound").unwrap();
        for provider in [Provider::Claude, Provider::Codex, Provider::Devin] {
            let cooldown = point(limit_window(provider), 100.0, 10, 1_810);
            let points = std::slice::from_ref(&cooldown);
            assert_eq!(quota_blocked_until(points, &pool, provider, 9), None);
            assert_eq!(
                quota_blocked_until(points, &pool, provider, 10),
                Some(1_810)
            );
            assert_eq!(
                quota_blocked_until(points, &pool, provider, 1_809),
                Some(1_810)
            );
            assert_eq!(quota_blocked_until(points, &pool, provider, 1_810), None);
            // A cooldown never crosses providers, pools, or a partial meter.
            for other in [Provider::Claude, Provider::Codex, Provider::Devin] {
                if other != provider {
                    assert_eq!(quota_blocked_until(points, &pool, other, 10), None);
                }
            }
            let mut foreign = cooldown.clone();
            foreign.pool = Id::new("foreign").unwrap();
            assert_eq!(
                quota_blocked_until(std::slice::from_ref(&foreign), &pool, provider, 10),
                None
            );
            let mut partial = cooldown.clone();
            partial.used_percent = 99.0;
            assert_eq!(
                quota_blocked_until(std::slice::from_ref(&partial), &pool, provider, 10),
                None
            );
        }
        // The cooldown does not feed the spending pace: it is not a meter.
        assert_eq!(
            quota_spending_pressure(
                &[point("codex.limit", 100.0, 10, 1_810)],
                &pool,
                Provider::Codex,
                10
            ),
            None
        );
    }

    #[test]
    fn later_reported_windows_supersede_the_cooldown_in_both_directions() {
        let pool = Id::new("bound").unwrap();
        let cooldown = point("codex.limit", 100.0, 100, 2_000);
        // An older exhausted window with a later reset still wins, as between
        // real windows; an older one with an earlier reset yields the cooldown.
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.secondary", 100.0, 50, 9_000)],
                &pool,
                Provider::Codex,
                150
            ),
            Some(9_000)
        );
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.primary", 100.0, 50, 1_000)],
                &pool,
                Provider::Codex,
                150
            ),
            Some(2_000)
        );
        // A newer provider-reported reset supersedes the cooldown even when
        // it is shorter, and a newer meter showing capacity clears it.
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.primary", 100.0, 101, 1_000)],
                &pool,
                Provider::Codex,
                150
            ),
            Some(1_000)
        );
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.primary", 100.0, 101, 1_000)],
                &pool,
                Provider::Codex,
                1_000
            ),
            None
        );
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.primary", 40.0, 101, 9_000)],
                &pool,
                Provider::Codex,
                150
            ),
            None
        );
        // Same-instant reports do not supersede; a newer cooldown replaces an
        // older one; and a model-scoped window never supersedes a cooldown.
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.primary", 40.0, 100, 9_000)],
                &pool,
                Provider::Codex,
                150
            ),
            Some(2_000)
        );
        assert_eq!(
            quota_blocked_until(
                &[cooldown.clone(), point("codex.limit", 100.0, 120, 3_000)],
                &pool,
                Provider::Codex,
                150
            ),
            Some(3_000)
        );
        assert_eq!(
            quota_blocked_until(
                &[
                    point("claude.limit", 100.0, 100, 2_000),
                    point("seven_day_opus", 10.0, 120, 9_000),
                ],
                &pool,
                Provider::Claude,
                150
            ),
            Some(2_000)
        );
    }
}

#[cfg(test)]
mod quota_spending_pressure_tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const HOUR: u64 = 3_600_000;

    fn point(window: &str, remaining: f64, hours: u64) -> QuotaPoint {
        QuotaPoint {
            pool: Id::new("bound").unwrap(),
            window: Id::new(window).unwrap(),
            used_percent: 100.0 - remaining,
            observed_at_ms: NOW,
            resets_at_ms: NOW + hours * HOUR,
        }
    }

    #[test]
    fn quota_spending_pressure_pairs_resets_and_respects_overlapping_windows() {
        let soon = point("seven_day", 30.0, 3);
        let later = point("seven_day", 35.0, 144);
        let pool = soon.pool.clone();
        let pressure = |points: &[QuotaPoint]| {
            quota_spending_pressure(points, &pool, Provider::Claude, NOW).unwrap()
        };
        assert!(
            pressure(std::slice::from_ref(&soon)).percent_per_hour
                > pressure(&[later]).percent_per_hour
        );
        let weekly = point("seven_day", 10.0, 100);
        let short = point("five_hour", 90.0, 1);
        let result = pressure(&[short, weekly.clone()]);
        assert_eq!(result.window, weekly.window);
        assert_eq!(result.remaining_percent, 10.0);
        assert_eq!(result.resets_at_ms, weekly.resets_at_ms);
        assert_eq!(result.percent_per_hour, 0.1);
        let short = point("five_hour", 1.0, 2);
        assert_eq!(pressure(&[soon, short]).percent_per_hour, 0.5);
    }

    #[test]
    fn quota_spending_pressure_requires_fresh_latest_known_window_evidence() {
        let fresh = point("five_hour", 80.0, 3);
        let mut stale = point("seven_day", 20.0, 100);
        stale.observed_at_ms = NOW - QUOTA_FRESH_MS - 1;
        let pool = fresh.pool.clone();
        assert_eq!(
            quota_spending_pressure(
                &[fresh.clone(), stale.clone()],
                &pool,
                Provider::Claude,
                NOW
            ),
            None
        );
        let latest = point("seven_day", 10.0, 100);
        let mut points = vec![fresh, stale, latest];
        let expected = quota_spending_pressure(&points, &pool, Provider::Claude, NOW);
        assert_eq!(expected.as_ref().unwrap().percent_per_hour, 0.1);
        points.reverse();
        assert_eq!(
            quota_spending_pressure(&points, &pool, Provider::Claude, NOW),
            expected
        );
        assert_eq!(
            quota_spending_pressure(
                &[point("five_hour", 80.0, 3)],
                &pool,
                Provider::Claude,
                NOW - 1
            ),
            None
        );
        assert_eq!(
            quota_spending_pressure(&points, &pool, Provider::Claude, NOW + 100 * HOUR),
            None
        );
    }

    #[test]
    fn quota_spending_pressure_rejects_unmeasured_and_foreign_scopes() {
        let pool = Id::new("bound").unwrap();
        let mut foreign = point("five_hour", 99.0, 1);
        foreign.pool = Id::new("other").unwrap();
        let mut invalid = point("seven_day", 80.0, 1);
        invalid.used_percent = f64::NAN;
        let points = [
            foreign,
            invalid,
            point("seven_day_opus", 99.0, 1),
            point("codex.primary", 40.0, 2),
            point("codex.secondary", 20.0, 100),
        ];
        assert_eq!(
            quota_spending_pressure(&points, &pool, Provider::Claude, NOW),
            None
        );
        assert_eq!(
            quota_spending_pressure(&points, &pool, Provider::Devin, NOW),
            None
        );
        let codex = quota_spending_pressure(&points, &pool, Provider::Codex, NOW).unwrap();
        assert_eq!(codex.window.as_str(), "codex.secondary");
        assert_eq!(codex.percent_per_hour, 0.2);
    }

    #[test]
    fn quota_spending_pressure_has_a_one_minute_floor_and_exact_reset_boundary() {
        let mut point = point("five_hour", 100.0, 1);
        point.resets_at_ms = NOW + 1;
        let result = quota_spending_pressure(
            std::slice::from_ref(&point),
            &point.pool,
            Provider::Claude,
            NOW,
        )
        .unwrap();
        assert_eq!(result.percent_per_hour, 6000.0);
        assert_eq!(
            quota_spending_pressure(
                std::slice::from_ref(&point),
                &point.pool,
                Provider::Claude,
                NOW + 1
            ),
            None
        );
        point.used_percent = 100.0;
        assert_eq!(
            quota_spending_pressure(
                std::slice::from_ref(&point),
                &point.pool,
                Provider::Claude,
                NOW
            )
            .unwrap()
            .percent_per_hour,
            0.0
        );
    }
}
