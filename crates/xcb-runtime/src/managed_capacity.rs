//! Admission targets are advisory. The launch path keeps every account,
//! project, workspace, quota and process-custody check authoritative.
use super::*;
use crate::auth;

pub(super) fn capacity_target(
    current: usize,
    ceiling: usize,
    active: usize,
    demand: usize,
    free_slots: usize,
    pressure: bool,
    caution: bool,
) -> (usize, &'static str) {
    let current = current.clamp(DEFAULT_ACTIVE_TARGET, ceiling);
    if pressure {
        return ((current / 2).max(DEFAULT_ACTIVE_TARGET), "host-pressure");
    }
    let available = active
        .saturating_add(free_slots)
        .max(DEFAULT_ACTIVE_TARGET)
        .min(ceiling);
    if available < current {
        return (available, "account-capacity");
    }
    if demand == 0 {
        return (
            current.min(active.max(DEFAULT_ACTIVE_TARGET)),
            "no-ready-work",
        );
    }
    if caution {
        return (current, "host-warning");
    }
    if active < current {
        return (current, "waiting-for-existing-target");
    }
    let next = current
        .saturating_add(1)
        .min(available)
        .min(active.saturating_add(demand));
    (
        next,
        if next > current {
            "ready-work-and-capacity"
        } else {
            "at-capacity"
        },
    )
}

impl Supervisor {
    /// Count independent launch candidates and a conservative account-slot
    /// upper bound, using only local data. Unknown quota cannot justify growth;
    /// it remains subject to the existing routing policy at launch time.
    pub(super) fn capacity_observation(
        &self,
        tasks: &[ManagedTask],
        config: &Config,
        now: u64,
    ) -> Result<(usize, usize)> {
        let mut workspaces: Vec<&str> = self
            .active_workspaces
            .values()
            .map(String::as_str)
            .collect();
        let mut demand = 0;
        let mut planners = 0;
        for task in tasks {
            if task.state != TaskState::Queued
                || task.deferred
                || task.cancel_requested
                || task.program_waiting
                || task.hold_until_ms.is_some_and(|until| until > now)
                || task
                    .retry
                    .as_ref()
                    .is_some_and(|retry| retry.next_eligible_at_ms > now)
                || task.completion_review.as_ref().is_some_and(|review| {
                    review.next_review_at_ms > now
                        || review.pr.as_ref().is_some_and(|pr| pr.waiting)
                })
                || self.active.contains_key(&task.id)
                || self
                    .launch_attempts
                    .get(&task.id)
                    .is_some_and(|attempt| !attempt.due(task.revision))
                || workspaces
                    .iter()
                    .any(|workspace| workspaces_overlap(workspace, &task.workspace))
                || self.managed.project_dispatch_block(task)?.is_some()
                || self.herd_capacity_block(task)?.is_some()
                || workspace_busy(&self.store, &task.workspace)?
            {
                continue;
            }
            workspaces.push(&task.workspace);
            demand += 1;
            planners += usize::from(task.program.is_some());
        }
        if demand == 0 {
            return Ok((0, 0));
        }
        let mut active_runs: BTreeMap<Id, u32> = BTreeMap::new();
        for run in self.store.unsettled_runs()? {
            *active_runs.entry(run.account).or_default() += 1;
        }
        let mut slots = planners;
        for account in self.store.accounts()? {
            let active = active_runs.get(&account.id).copied().unwrap_or(0);
            if !account.enabled
                || self.store.authentication_required(&account.id)?
                || !auth::has_credentials(&self.store, &account.id).unwrap_or(false)
                || self.store.quota_blocked_until(&account.id, now)?.is_some()
                || !self
                    .store
                    .remaining_percent(&account.quota_pool, now)?
                    .is_some_and(|remaining| remaining > 0.0)
                || !self
                    .store
                    .account_recovery_available(&account.id, now, active)?
            {
                continue;
            }
            let account_limit = if self
                .store
                .account_recovery(&account.id)?
                .is_some_and(|health| health.consecutive_failures > 0)
            {
                1
            } else {
                config.max_runs_per_account
            };
            slots = slots.saturating_add(account_limit.saturating_sub(active) as usize);
        }
        Ok((demand, slots))
    }

    pub(super) fn save_capacity_status(&self, status: &serde_json::Value) -> Result<()> {
        let path = self.managed.root().join("adaptive-capacity.json");
        let bytes = serde_json::to_vec(status)?;
        match private::read(&path, 4096) {
            Ok(previous) => private::replace(&path, &bytes, &digest(previous)),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                private::create(&path, &bytes)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sleeping_and_overlapping_work_does_not_inflate_demand() {
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let workspace = private::directory(&base.join("work")).unwrap();
        let managed = Arc::new(ManagedStore::open(&base.join("state")).unwrap());
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let conversation = managed.create_conversation(&workspace).await.unwrap();
        let task = managed
            .enqueue_backlog(
                &conversation.id,
                new_id("m"),
                "Inspect fixture".into(),
                false,
                5,
            )
            .await
            .unwrap();
        let supervisor = Supervisor::new(managed, store);
        let now = now_ms();
        let config = Config::default();
        assert_eq!(
            supervisor
                .capacity_observation(&[task.clone(), task.clone()], &config, now)
                .unwrap(),
            (1, 0)
        );
        for mode in 0..6 {
            let mut sleeping = task.clone();
            match mode {
                0 => sleeping.deferred = true,
                1 => sleeping.cancel_requested = true,
                2 => sleeping.state = TaskState::NeedsInput,
                3 => sleeping.program_waiting = true,
                4 => sleeping.hold_until_ms = Some(now + 60_000),
                _ => sleeping.retry = Some(crate::retry::TaskRetry::after(None, "fixture", now)),
            }
            assert_eq!(
                supervisor
                    .capacity_observation(&[sleeping], &config, now)
                    .unwrap(),
                (0, 0)
            );
        }
    }

    #[test]
    fn grows_one_slot_only_for_occupied_target_with_ready_work_and_capacity() {
        assert_eq!(
            capacity_target(1, 8, 1, 7, 7, false, false),
            (2, "ready-work-and-capacity")
        );
        assert_eq!(
            capacity_target(4, 8, 3, 7, 7, false, false),
            (4, "waiting-for-existing-target")
        );
        assert_eq!(
            capacity_target(4, 8, 4, 7, 0, false, false),
            (4, "at-capacity")
        );
        assert_eq!(
            capacity_target(4, 4, 4, 7, 7, false, false),
            (4, "at-capacity")
        );
    }

    #[test]
    fn pressure_capacity_loss_and_idle_back_down_without_killing_workers() {
        assert_eq!(
            capacity_target(8, 8, 8, 5, 5, true, false),
            (4, "host-pressure")
        );
        assert_eq!(
            capacity_target(4, 8, 2, 5, 0, false, false),
            (2, "account-capacity")
        );
        assert_eq!(
            capacity_target(4, 8, 0, 0, 8, false, false),
            (1, "no-ready-work")
        );
        assert_eq!(
            capacity_target(4, 8, 4, 5, 5, false, true),
            (4, "host-warning")
        );
        assert_eq!(
            capacity_target(1, 8, 0, 8, 0, false, false),
            (1, "waiting-for-existing-target")
        );
    }
}
