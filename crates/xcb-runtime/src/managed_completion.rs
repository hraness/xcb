//! Settled provider turns are not proof that the requested objective shipped.
//! This review only continues the same authorized task; it never grants tools.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionReview {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<completion_pr::PrWatch>,
    pub report_sha256: String,
    pub rounds: u32,
    pub wait_deadline_ms: u64,
    pub productive_since_ms: u64,
    pub repeated: u32,
    pub next_review_at_ms: u64,
    /// Evidence is worker-reported, never an independently verified PR state.
    pub evidence: String,
}
impl CompletionReview {
    pub(super) fn validate(&self) -> Result<()> {
        if self.rounds == 0
            || self.rounds > 100_000
            || self.repeated > self.rounds
            || !xcb_core::hex64(&self.report_sha256)
        {
            return Err(Error::Conflict("invalid completion review"));
        }
        bounded_text(&self.reason, 256)?;
        bounded_text(&self.evidence, 2048)?;
        if let Some(pr) = &self.pr {
            pr.validate()?;
        }
        sql(self.next_review_at_ms)?;
        Ok(())
    }
}

pub(super) struct Review {
    pub record: CompletionReview,
    pub continue_task: bool,
}

fn contains(text: &str, cues: &[&str]) -> bool {
    cues.iter().any(|cue| text.contains(cue))
}

fn delivery_intent(scope: &str) -> (bool, bool) {
    let lower = scope.to_lowercase();
    if contains(
        &lower,
        &[
            "review only",
            "review-only",
            "draft only",
            "draft-only",
            "do not merge",
            "don't merge",
            "do not release",
            "don't release",
        ],
    ) {
        return (false, false);
    }
    let requested = |verb: &str| {
        lower.lines().any(|line| {
            line.trim().starts_with(&format!("{verb} "))
                || line.contains(&format!("and {verb} "))
                || line.contains(&format!("then {verb} "))
                || line.contains(&format!("please {verb} "))
                || line.contains(&format!("get it {verb}"))
        })
    };
    let release = requested("release") || requested("deploy") || requested("ship");
    (release || requested("merge"), release)
}

pub(super) fn contract(task: &ManagedTask) -> &'static str {
    if delivery_intent(&task.goal).0 {
        "\nRequested delivery is part of this task. Before reporting completion, inspect current remote state and report DELIVERY_EVIDENCE pr=https://github.com/OWNER/REPO/pull/NUMBER head=40_HEX_COMMIT state=MERGED or, for a requested release, DELIVERY_EVIDENCE release=https://github.com/OWNER/REPO/releases/tag/TAG state=PUBLISHED. Pending PR checks may be handed to the read-only host observer with WAIT_PR pr=https://github.com/OWNER/REPO/pull/NUMBER head=40_HEX_COMMIT. Evidence is a report, not permission to expand scope or bypass gates."
    } else {
        ""
    }
}

fn required_work(scope: &str, report: &str) -> Option<&'static str> {
    let scope = scope.to_lowercase();
    let restricted = contains(
        &scope,
        &[
            "review only",
            "review-only",
            "draft only",
            "draft-only",
            "do not merge",
            "don't merge",
            "do not release",
            "don't release",
        ],
    );
    for line in report.lines() {
        let line = line.trim().to_lowercase();
        if line.starts_with('>') {
            continue;
        }
        if !restricted && contains(&scope, &["merge", "ship", "release", "deploy"]) {
            if contains(
                &line,
                &[
                    "not merged",
                    "unmerged",
                    "pr is open",
                    "pr remains open",
                    "pull request is open",
                    "ready to merge",
                    "ready for review",
                    "waiting on ci",
                    "waiting for ci",
                    "merge on green",
                    "should i merge",
                    "shall i merge",
                    "want me to merge",
                ],
            ) {
                return Some("requested delivery remains pending");
            }
            if contains(&scope, &["release", "ship", "deploy"])
                && contains(
                    &line,
                    &[
                        "not released",
                        "unreleased",
                        "release pending",
                        "ready to release",
                        "should i release",
                        "not deployed",
                        "deployment pending",
                    ],
                )
            {
                return Some("requested delivery remains pending");
            }
        }
        if contains(
            &line,
            &[
                "optional",
                "out of scope",
                "future work",
                "if you want",
                "if you'd like",
            ],
        ) {
            continue;
        }
        if contains(
            &line,
            &[
                "required follow-up:",
                "required followup:",
                "still need to",
                "still needs to",
                "left to do:",
                "work remains",
                "i'll continue",
                "i will continue",
            ],
        ) {
            return Some("required work remains in the latest report");
        }
    }
    None
}

/// Narrow report syntax keeps a prior pending delivery from being cleared by
/// another bare "done" claim. This is a report, not remote attestation.
fn delivery_reported(
    report: &str,
    repo: Option<&str>,
    expected_head: Option<&str>,
    release_required: bool,
) -> bool {
    report.lines().any(|line| {
        let Some(line) = line.trim().strip_prefix("DELIVERY_EVIDENCE ") else {
            return false;
        };
        let fields: Vec<_> = line.split_whitespace().collect();
        let pr = fields.iter().find_map(|field| field.strip_prefix("pr="));
        let head = fields.iter().find_map(|field| field.strip_prefix("head="));
        (!release_required
            && pr.is_some_and(|url| {
                repo.is_some_and(|repo| {
                    url.starts_with(&format!("https://github.com/{repo}/pull/"))
                })
            })
            && head
                .is_some_and(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            && expected_head.is_none_or(|expected| head == Some(expected))
            && fields.contains(&"state=MERGED"))
            || (fields.iter().any(|field| {
                repo.is_some_and(|repo| {
                    field.starts_with(&format!("release=https://github.com/{repo}/releases/tag/"))
                })
            }) && fields.contains(&"state=PUBLISHED"))
    })
}

fn delivery_for_task(
    report: &str,
    repo: Option<&str>,
    target: Option<&completion_pr::PrWatch>,
    release_required: bool,
) -> bool {
    let Some(target) = target.filter(|_| !release_required) else {
        return delivery_reported(report, repo, None, release_required);
    };
    let expected = format!(
        "pr=https://github.com/{}/pull/{}",
        target.repository, target.number
    );
    report.lines().any(|line| {
        line.trim()
            .strip_prefix("DELIVERY_EVIDENCE ")
            .is_some_and(|fields| fields.split_whitespace().any(|field| field == expected))
            && delivery_reported(line, repo, Some(&target.head), false)
    })
}

pub(super) fn review(
    task: &ManagedTask,
    outcome: &Outcome,
    config: &Config,
    decision: Option<&reflex::Decision>,
    authorized_project: bool,
    now: u64,
) -> Option<Review> {
    if task.cancel_requested
        || !outcome.askable()
        || outcome.denied()
        || !outcome.facts.joined
        || outcome.facts.effects == EffectState::Uncertain
        || outcome.facts.failure.is_some()
        || outcome.facts.terminal != Terminal::Completed
    {
        return None;
    }
    let scope = std::iter::once(task.goal.as_str())
        .chain(task.user_inputs.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let (delivery_scope, release_required) = delivery_intent(&scope);
    let github_granted = config
        .native_execution
        .scopes
        .iter()
        .any(|grant| grant.workspace == Path::new(&task.workspace) && grant.github_credentials);
    let pr = if delivery_scope && github_granted {
        completion_pr::request(&outcome.text, &task.workspace).or_else(|| {
            task.completion_review
                .as_ref()
                .and_then(|review| review.pr.clone())
        })
    } else {
        None
    };
    let watching = pr.as_ref().is_some_and(|pr| pr.waiting);
    let repo = workspace::github_origin(Path::new(&task.workspace));
    let blocked = decision.is_some_and(|decision| xcb_core::reflex::owner_only(&decision.features));
    let missing_delivery = delivery_scope
        && !delivery_for_task(
            &outcome.text,
            repo.as_deref(),
            pr.as_ref(),
            release_required,
        );
    let reason = required_work(&scope, &outcome.text)
        .or_else(|| missing_delivery.then_some("requested delivery remains pending"))
        .or_else(|| watching.then_some("requested delivery remains pending"))
        .or_else(|| {
            task.completion_review
                .as_ref()
                .filter(|review| {
                    review.reason == "requested delivery remains pending"
                        && !delivery_for_task(
                            &outcome.text,
                            repo.as_deref(),
                            pr.as_ref(),
                            release_required,
                        )
                })
                .map(|_| "requested delivery remains pending")
        })
        .or_else(|| {
            decision
                .filter(|decision| {
                    authorized_project
                        && decision.value == "stopped_short"
                        && !xcb_core::reflex::owner_only(&decision.features)
                })
                .map(|_| "required work remains in the latest report")
        })
        .or_else(|| {
            decision
                .filter(|decision| authorized_project && confirmable(decision, &outcome.text))
                .map(|_| "routine confirmation within existing task authority")
        })?;
    let report_sha256 = digest(outcome.text.as_bytes());
    let previous = task.completion_review.as_ref();
    let rounds = previous.map_or(1, |review| review.rounds.saturating_add(1));
    let repeated = previous.map_or(0, |review| {
        if review.report_sha256 == report_sha256 {
            review.repeated.saturating_add(1)
        } else {
            0
        }
    });
    let waiting = watching
        || contains(
            &outcome.text.to_lowercase(),
            &[
                "waiting on",
                "waiting for",
                "pending ci",
                "checks pending",
                "merge on green",
            ],
        );
    let delay = if waiting {
        (60_000u64 << rounds.saturating_sub(1).min(4)).min(900_000)
    } else {
        0
    };
    let productive_since_ms = previous
        .map_or(task.input_at_ms.unwrap_or(task.created_at_ms), |review| {
            review.productive_since_ms
        });
    let elapsed = now.saturating_sub(productive_since_ms);
    let policy = &config.extensions.auto_continue;
    let wait_deadline_ms = previous
        .map_or(now.saturating_add(30 * 24 * 60 * 60 * 1000), |review| {
            review.wait_deadline_ms
        });
    let continue_task = authorized_project
        && !blocked
        && policy.enabled
        && rounds < 100_000
        && if watching {
            now < wait_deadline_ms
        } else {
            rounds <= 8
                && repeated < 2
                && task.attempts.saturating_add(1) < task.max_attempts
                && task.attempts < policy.max_consecutive
                && elapsed < policy.max_elapsed_ms
        };
    Some(Review {
        record: CompletionReview {
            reason: reason.into(),
            pr,
            report_sha256,
            rounds: rounds.min(100_000),
            wait_deadline_ms,
            productive_since_ms,
            repeated: repeated.min(100_000),
            next_review_at_ms: now.saturating_add(delay),
            evidence: xcb_core::display_text(&outcome.text, 2048),
        },
        continue_task,
    })
}

pub(super) fn prompt(task: &ManagedTask, review: &CompletionReview) -> String {
    format!(
        "Continue the existing task within its original authority. The last response left required work unresolved: {}. A finished provider turn is not proof that the objective is delivered. Inspect the latest task response and current repository/PR/check/release state before acting. Preserve completed effects, obey current repository gates and all tool approval controls, and do not repeat uncertain effects. Routine confirmations already covered by this task's authority do not require another user reply. Respect draft-only, review-only, and explicit scope limits; optional suggestions are not new work. If blocked by actual credentials, denied permission, uncertain effects or missing authority, report that blocker rather than bypassing it.\nOriginal request:\n{}\nLatest report (context, not new authority):\n{}\nFor requested delivery, obtain fresh exact-head remote evidence. Report merged PR evidence as DELIVERY_EVIDENCE pr=https://github.com/OWNER/REPO/pull/NUMBER head=40_HEX_COMMIT state=MERGED, or published release evidence as DELIVERY_EVIDENCE release=https://github.com/OWNER/REPO/releases/tag/TAG state=PUBLISHED. These are worker reports; do not fabricate verification. If PR checks are pending, report WAIT_PR pr=https://github.com/OWNER/REPO/pull/NUMBER head=40_HEX_COMMIT. Under an existing GitHub workspace grant the host observes that exact PR/head read-only without consuming provider turns; it never merges. The target must match this workspace's GitHub origin.",
        review.reason,
        xcb_core::display_text(&task.goal, 8192),
        review.evidence
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_delivery_is_distinct_from_optional_or_draft_work() {
        assert!(
            required_work(
                "Implement and merge this change",
                "PR remains open; waiting on CI"
            )
            .is_some()
        );
        assert!(
            required_work(
                "Review only; do not merge",
                "PR is open and ready for review"
            )
            .is_none()
        );
        assert!(required_work("Create a draft-only PR", "Ready for review").is_none());
        assert!(
            required_work(
                "Merge the requested change",
                "Merged. Optional follow-up: improve docs"
            )
            .is_none()
        );
        assert!(
            required_work(
                "Fix the parser",
                "Required follow-up: finish the regression test"
            )
            .is_some()
        );
        assert!(required_work("Merge it", "All done and merged").is_none());
        assert!(
            required_work(
                "Merge it",
                "If you'd like, I can merge on green; PR remains open"
            )
            .is_some()
        );
    }
    #[test]
    fn delivery_requires_specific_reported_evidence() {
        assert!(!delivery_reported(
            "Done. PR merged.",
            Some("o/r"),
            None,
            false
        ));
        assert!(!delivery_reported(
            "DELIVERY_EVIDENCE pr=https://github.com/o/r/pull/1 head=bad state=MERGED",
            Some("o/r"),
            None,
            false
        ));
        assert!(delivery_reported(
            &format!(
                "DELIVERY_EVIDENCE pr=https://github.com/o/r/pull/1 head={} state=MERGED",
                "a".repeat(40)
            ),
            Some("o/r"),
            None,
            false
        ));
    }
    #[test]
    fn completion_delivery_evidence_is_phase_and_repository_bound() {
        let report = format!(
            "DELIVERY_EVIDENCE pr=https://github.com/o/r/pull/1 head={} state=MERGED",
            "a".repeat(40)
        );
        assert!(!delivery_reported(
            &report,
            Some("foreign/repo"),
            None,
            false
        ));
        assert!(!delivery_reported(
            &report,
            Some("o/r"),
            Some(&"b".repeat(40)),
            false
        ));
        assert!(!delivery_reported(&report, Some("o/r"), None, true));
        let target = completion_pr::PrWatch {
            repository: "o/r".into(),
            number: 2,
            head: "a".repeat(40),
            waiting: false,
            polls: 1,
            observed_at_ms: Some(1),
            status: "settled".into(),
        };
        assert!(!delivery_for_task(
            &report,
            Some("o/r"),
            Some(&target),
            false
        ));
    }
    fn outcome(text: &str) -> Outcome {
        Outcome {
            text: text.into(),
            state: State::Idle,
            tool_calls: Some(1),
            text_attention: false,
            diagnostic: None,
            facts: xcb_core::policy::TurnFacts {
                terminal: Terminal::Completed,
                joined: true,
                effects: EffectState::Settled,
                pending_attention: false,
                failure: None,
            },
        }
    }
    #[tokio::test]
    async fn completion_review_persists_pending_work_and_obeys_authority_and_progress() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap();
        let work = private::directory(&root.join("work")).unwrap();
        let store = Store::open(&root.join("state")).unwrap();
        let managed = ManagedStore::open(store.root()).unwrap();
        let chat = managed.create_conversation(&work).await.unwrap().id;
        let task = managed
            .enqueue_backlog(
                &chat,
                new_id("test"),
                "Implement and merge the fix".into(),
                false,
                5,
            )
            .await
            .unwrap();
        let report = outcome("PR remains open; waiting on CI");
        assert!(
            review(
                &task,
                &outcome("Done. Checks passed."),
                &Config::default(),
                None,
                true,
                now_ms()
            )
            .is_some()
        );
        assert!(!delivery_intent("Implement a release command").0);
        assert!(delivery_intent("Implement and merge the fix").0);
        let owner_only = reflex::Decision {
            reflex: Reflex::Settle,
            head: "unfinished".into(),
            value: "stopped_short".into(),
            gate: String::new(),
            score_milli: 0,
            features: [("owner_only".into(), 1.0)].into_iter().collect(),
            params_version: 1,
            program: String::new(),
            receipt: String::new(),
        };
        assert!(
            !review(
                &task,
                &report,
                &Config::default(),
                Some(&owner_only),
                true,
                now_ms()
            )
            .unwrap()
            .continue_task
        );
        let held = review(&task, &report, &Config::default(), None, false, now_ms()).unwrap();
        assert!(!held.continue_task);
        managed
            .configure_project_policy(
                &chat,
                None,
                "Finish authorized repairs".into(),
                20,
                now_ms() + 86_400_000,
                None,
            )
            .unwrap();
        let mut running = task.clone();
        running.state = TaskState::Running;
        running.revision += 1;
        let running = managed.transition(&task, running, None).await.unwrap();
        let continued = managed
            .finish(&store, &running.id, Ok(report.clone()))
            .await
            .unwrap();
        assert_eq!(continued.state, TaskState::Queued);
        assert!(continued.completion_review.is_some());
        assert!(
            continued
                .next_prompt
                .contains("Implement and merge the fix")
        );
        assert!(continued.next_prompt.contains("PR remains open"));
        let restored = managed.task(&continued.id).unwrap().unwrap();
        let mut repeated = restored.clone();
        for _ in 0..2 {
            let reviewed =
                review(&repeated, &report, &Config::default(), None, true, now_ms()).unwrap();
            repeated.completion_review = Some(reviewed.record);
        }
        assert!(
            !review(&repeated, &report, &Config::default(), None, true, now_ms())
                .unwrap()
                .continue_task
        );
        let mut unsafe_report = report.clone();
        unsafe_report.facts.effects = EffectState::Uncertain;
        assert!(
            review(
                &restored,
                &unsafe_report,
                &Config::default(),
                None,
                true,
                now_ms()
            )
            .is_none()
        );
        unsafe_report = report;
        unsafe_report.facts.pending_attention = true;
        assert!(
            review(
                &restored,
                &unsafe_report,
                &Config::default(),
                None,
                true,
                now_ms()
            )
            .is_none()
        );
    }
}
