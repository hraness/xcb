//! Durable, bounded recovery for explicit settled provider refusals.
use serde::{Deserialize, Serialize};
use xcb_core::Id;

pub const RECOVERY_HORIZON_MS: u64 = 35 * 24 * 60 * 60 * 1000;
pub const MAX_RECOVERY_FAILURES: u32 = 4096;

/// Deterministic jitter is persisted with each decision, so restart never
/// redraws an earlier retry. Different tasks/accounts do not retry in lockstep.
pub(crate) fn delay_ms(key: &str, failures: u32) -> u64 {
    let base = (30_000u64 << failures.saturating_sub(1).min(6)).min(1_800_000);
    let hash = key
        .bytes()
        .chain(failures.to_le_bytes())
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
    base / 2 + hash % (base / 2 + 1)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryExclusion {
    pub route: String,
    pub account: Option<Id>,
    pub until_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetry {
    pub failures: u32,
    pub started_at_ms: u64,
    pub deadline_ms: u64,
    pub next_eligible_at_ms: u64,
    pub exclusions: Vec<RetryExclusion>,
}
impl TaskRetry {
    pub(crate) fn after(previous: Option<&Self>, key: &str, now: u64) -> Self {
        let mut next = previous.cloned().unwrap_or(Self {
            failures: 0,
            started_at_ms: now,
            deadline_ms: now.saturating_add(RECOVERY_HORIZON_MS),
            next_eligible_at_ms: now,
            exclusions: vec![],
        });
        next.failures = next.failures.saturating_add(1);
        next.next_eligible_at_ms = now.saturating_add(delay_ms(key, next.failures));
        next
    }
    pub(crate) fn permits(&self, now: u64) -> bool {
        now < self.deadline_ms && self.failures < MAX_RECOVERY_FAILURES
    }
    pub(crate) fn valid(&self) -> bool {
        self.failures > 0
            && self.failures <= MAX_RECOVERY_FAILURES
            && self.deadline_ms == self.started_at_ms.saturating_add(RECOVERY_HORIZON_MS)
            && self.next_eligible_at_ms >= self.started_at_ms
            && self.exclusions.len() <= 16
            && self.exclusions.iter().all(|e| {
                !e.route.is_empty() && e.route.len() <= 1024 && e.until_ms >= self.started_at_ms
            })
    }
}

/// Evidence about this account's provider availability, independent of quota,
/// authentication and custody. A due retry permits one trial, not all callers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountRecovery {
    pub consecutive_failures: u32,
    pub observed_at_ms: u64,
    pub next_eligible_at_ms: u64,
    pub last_success_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_backoff_is_bounded_jittered_and_month_scale() {
        assert_ne!(delay_ms("task-a", 4), delay_ms("task-b", 4));
        for failures in 1..=MAX_RECOVERY_FAILURES {
            let delay = delay_ms("account", failures);
            assert!((15_000..=1_800_000).contains(&delay));
        }
        let retry = TaskRetry::after(None, "task", 100);
        assert!(retry.permits(100 + 30 * 24 * 60 * 60 * 1000));
        assert!(!retry.permits(retry.deadline_ms));
        assert!(retry.valid());
    }
}
