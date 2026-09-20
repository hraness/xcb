use crate::{Error, Id, Result};
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

/// The latest observation in each known Claude account-wide window is authoritative
/// until its reported reset, even after percentage telemetry becomes stale. Callers
/// must bind `pool` to the current account credential generation; model-specific and
/// other-provider windows are deliberately not inferred to have account scope.
pub fn quota_blocked_until(points: &[QuotaPoint], pool: &Id, now: u64) -> Option<u64> {
    ["five_hour", "seven_day"]
        .into_iter()
        .filter_map(|window| {
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
                .filter(|point| point.used_percent == 100.0 && now < point.resets_at_ms)
                .map(|point| point.resets_at_ms)
        })
        .max()
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
            quota_blocked_until(&points, &pool, 500_000),
            Some(9_000_000)
        );
        points.push(point("five_hour", 20.0, 499_999, 2_000_000));
        assert_eq!(
            quota_blocked_until(&points, &pool, 500_000),
            Some(9_000_000)
        );
        points.push(point("seven_day", 10.0, 500_000, 10_000_000));
        points.reverse();
        assert_eq!(quota_blocked_until(&points, &pool, 500_000), None);
    }

    #[test]
    fn quota_availability_has_exact_observed_and_reset_boundaries() {
        let p = point("five_hour", 100.0, 10, 20);
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, 9),
            None
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, 10),
            Some(20)
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, 19),
            Some(20)
        );
        assert_eq!(
            quota_blocked_until(std::slice::from_ref(&p), &p.pool, 20),
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
        assert_eq!(quota_blocked_until(&points, &pool, 2), None);
        assert_eq!(
            quota_blocked_until(&[point("seven_day", f64::NAN, 1, 1000)], &pool, 2),
            None
        );
    }
}
