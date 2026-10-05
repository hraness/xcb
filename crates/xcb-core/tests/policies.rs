use std::collections::BTreeSet;
use xcb_core::models::{Mode, ModelChoice, default_preferences, sort_choices};
use xcb_core::panes::{Pane, PaneSlot};
use xcb_core::policy::{
    EffectState, Failure, RouteCandidate, Terminal, TurnFacts, failover_permitted, next_route,
    no_reply,
};
use xcb_core::usage::{Estimate, QuotaPoint, runway};
use xcb_core::{Id, Provider};

fn route(account: &str, model: &str) -> RouteCandidate {
    RouteCandidate {
        account: Id::new(account).unwrap(),
        model: ModelChoice {
            provider: Provider::Devin,
            id: Id::new(model).unwrap(),
            label: model.into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        },
        admitted: true,
        quota_clear: true,
        available: true,
    }
}
fn facts(failure: Failure) -> TurnFacts {
    facts_with_terminal(Terminal::Failed, Some(failure))
}
fn facts_with_terminal(terminal: Terminal, failure: Option<Failure>) -> TurnFacts {
    TurnFacts {
        terminal,
        joined: true,
        effects: EffectState::Settled,
        pending_attention: false,
        failure,
    }
}

#[test]
fn account_limits_do_not_retry_the_same_accounts_other_models() {
    let current = route("personal", "swe-2-high");
    let candidates = [
        route("personal", "gpt-6-astra-max"),
        route("work", "gpt-6-astra-max"),
    ];
    assert_eq!(
        next_route(
            &current,
            &candidates,
            &BTreeSet::new(),
            &facts(Failure::AccountQuota),
            true
        )
        .unwrap()
        .account
        .as_str(),
        "work"
    );
    assert_eq!(
        next_route(
            &current,
            &candidates,
            &BTreeSet::new(),
            &facts(Failure::ModelQuota),
            true
        )
        .unwrap()
        .account
        .as_str(),
        "personal"
    );
}

#[test]
fn switching_is_not_a_recovery_for_policy_auth_or_transport_failures() {
    let current = route("personal", "swe-2-high");
    let candidates = [route("work", "gpt-6-astra-max")];
    for failure in [
        Failure::Policy,
        Failure::Authentication,
        Failure::Transport,
        Failure::Unknown,
    ] {
        assert!(
            next_route(
                &current,
                &candidates,
                &BTreeSet::new(),
                &facts(failure),
                true
            )
            .is_none()
        );
    }
    assert!(
        next_route(
            &current,
            &candidates,
            &BTreeSet::new(),
            &facts(Failure::AccountQuota),
            false
        )
        .is_none()
    );
    let pending = TurnFacts {
        effects: EffectState::Uncertain,
        ..facts(Failure::AccountQuota)
    };
    assert!(next_route(&current, &candidates, &BTreeSet::new(), &pending, true).is_none());
}

#[test]
fn unadmitted_limited_busy_or_already_tried_targets_are_excluded() {
    let current = route("personal", "swe-2-high");
    let target = route("work", "gpt-6-astra-max");
    for invalid in [
        RouteCandidate {
            admitted: false,
            ..target.clone()
        },
        RouteCandidate {
            quota_clear: false,
            ..target.clone()
        },
        RouteCandidate {
            available: false,
            ..target.clone()
        },
    ] {
        assert!(
            next_route(
                &current,
                &[invalid],
                &BTreeSet::new(),
                &facts(Failure::AccountQuota),
                true
            )
            .is_none()
        );
    }
    let tried = BTreeSet::from([format!("{}/{}", target.account, target.model.key())]);
    assert!(
        next_route(
            &current,
            &[target],
            &tried,
            &facts(Failure::AccountQuota),
            true
        )
        .is_none()
    );
}

/// An account without a usage meter, or whose last reading has aged out, is
/// still a failover target: `quota_clear` is a statement about known limits,
/// not about measurement freshness. The caller sets it from the same facts
/// automatic routing uses, so the gate and the router agree on eligibility.
#[test]
fn an_unmeasured_account_is_a_failover_target_and_a_known_limit_is_not() {
    let current = route("personal", "swe-2-high");
    let unmeasured = route("devin-work", "swe-2-high");
    let limited = RouteCandidate {
        quota_clear: false,
        ..route("codex-work", "gpt-6-astra-max")
    };
    let ordered = [limited, unmeasured];
    let chosen = next_route(
        &current,
        &ordered,
        &BTreeSet::new(),
        &facts(Failure::AccountQuota),
        true,
    )
    .unwrap();
    assert_eq!(chosen.account.as_str(), "devin-work");
}

/// The shared gate answers exactly when `next_route` could answer for some
/// candidate, so a caller can skip ranking and explain the refusal instead.
#[test]
fn failover_gate_matches_next_route_preconditions() {
    let current = route("personal", "swe-2-high");
    let target = route("work", "gpt-6-astra-max");
    let none = BTreeSet::new();
    let sixteen: BTreeSet<_> = (0..16).map(|n| format!("account-{n}/model")).collect();
    let uncertain = TurnFacts {
        effects: EffectState::Uncertain,
        ..facts(Failure::AccountQuota)
    };
    let unjoined = TurnFacts {
        joined: false,
        ..facts(Failure::AccountQuota)
    };
    let attention = TurnFacts {
        pending_attention: true,
        ..facts(Failure::ModelQuota)
    };
    for (label, facts, tried, checkpointed) in [
        (
            "settled account limit",
            facts(Failure::AccountQuota),
            &none,
            true,
        ),
        (
            "settled model limit",
            facts(Failure::ModelQuota),
            &none,
            true,
        ),
        ("uncertain effects", uncertain, &none, true),
        ("processes not exited", unjoined, &none, true),
        ("waiting for an answer", attention, &none, true),
        ("no checkpoint", facts(Failure::AccountQuota), &none, false),
        (
            "sixteen routes tried",
            facts(Failure::AccountQuota),
            &sixteen,
            true,
        ),
        ("transport failure", facts(Failure::Transport), &none, true),
        (
            "token limit",
            facts_with_terminal(Terminal::TokenLimit, Some(Failure::AccountQuota)),
            &none,
            true,
        ),
    ] {
        let permitted = failover_permitted(&facts, tried, checkpointed);
        let chosen = next_route(
            &current,
            std::slice::from_ref(&target),
            tried,
            &facts,
            checkpointed,
        );
        assert_eq!(permitted, chosen.is_some(), "{label}");
        assert_eq!(
            permitted,
            label.starts_with("settled"),
            "{label} permitted={permitted}"
        );
    }
}

#[test]
fn quota_failover_only_happens_at_failed_terminal_boundary() {
    let current = route("personal", "swe-2-high");
    let candidates = [route("work", "gpt-6-astra-max")];
    for terminal in [
        Terminal::TokenLimit,
        Terminal::TurnLimit,
        Terminal::Completed,
        Terminal::Cancelled,
    ] {
        for failure in [Some(Failure::AccountQuota), Some(Failure::ModelQuota)] {
            let stale = facts_with_terminal(terminal, failure);
            assert!(
                next_route(&current, &candidates, &BTreeSet::new(), &stale, true).is_none(),
                "{terminal:?} with {failure:?} should not trigger failover",
            );
        }
    }
    let failed_no_failure = facts_with_terminal(Terminal::Failed, None);
    assert!(
        next_route(
            &current,
            &candidates,
            &BTreeSet::new(),
            &failed_no_failure,
            true
        )
        .is_none()
    );
}

#[test]
fn only_a_completed_turn_without_text_or_effects_reports_no_reply() {
    let silent = TurnFacts {
        effects: EffectState::None,
        ..facts_with_terminal(Terminal::Completed, None)
    };
    for text in ["", " \n\t"] {
        assert!(no_reply(text, &silent));
        let reported = silent.reported(text);
        assert_eq!(reported.failure, Some(Failure::NoReply));
        assert_eq!(reported.terminal, Terminal::Completed);
        assert_eq!(
            serde_json::to_value(&reported).unwrap()["failure"],
            "no_reply"
        );
    }
    // Answer text, settled file changes, another terminal, or a recorded
    // failure each say how the turn ended; none of them reports no_reply.
    let answered = silent.reported("Fixed the subtraction.");
    assert_eq!(answered.failure, None);
    let changed = TurnFacts {
        effects: EffectState::Settled,
        ..silent.clone()
    };
    assert!(!no_reply("", &changed));
    assert_eq!(changed.reported("").failure, None);
    for terminal in [
        Terminal::TokenLimit,
        Terminal::TurnLimit,
        Terminal::Cancelled,
        Terminal::Failed,
    ] {
        let stopped = TurnFacts {
            terminal,
            ..silent.clone()
        };
        assert!(!no_reply("", &stopped), "{terminal:?}");
    }
    let failed = TurnFacts {
        failure: Some(Failure::Transport),
        ..silent.clone()
    };
    assert_eq!(failed.reported("").failure, Some(Failure::Transport));
    // Deriving the report never rewrites the recorded facts.
    assert_eq!(silent.failure, None);
}

#[test]
fn favorites_precede_provider_modes_and_other_models() {
    let mut choices = vec![
        route("a", "other").model,
        route("a", "swe-2-high").model,
        route("a", "gpt-6-astra-max").model,
        route("a", "gpt-5-6-sol-max").model,
    ];
    sort_choices(&mut choices, &default_preferences());
    // SWE models are no longer favorites: the built-in routing stack never
    // uses them, so they sort with the other unlisted models.
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect::<Vec<_>>(),
        ["gpt-5-6-sol-max", "gpt-6-astra-max", "other", "swe-2-high"]
    );
    assert!(
        default_preferences()
            .iter()
            .all(|favorite| !favorite.model.as_str().starts_with("swe-"))
    );
}

#[test]
fn failed_hot_reload_preserves_the_previous_pane() {
    let previous = Pane::focus();
    let mut slot = PaneSlot::new(previous.clone()).unwrap();
    assert!(!slot.reload(b"{incomplete"));
    assert_eq!(slot.current, previous);
    assert!(slot.error.is_some());
    let mut other = previous.clone();
    other.id = Id::new("other").unwrap();
    assert!(!slot.reload(&serde_json::to_vec(&other).unwrap()));
    assert!(slot.reload(&serde_json::to_vec(&previous).unwrap()));
    assert!(slot.error.is_none());
}

#[test]
fn mismatched_reset_windows_never_generate_runway() {
    let point = QuotaPoint {
        pool: Id::new("paid").unwrap(),
        window: Id::new("weekly").unwrap(),
        used_percent: 10.0,
        observed_at_ms: 1_000,
        resets_at_ms: 100_000,
    };
    let next = QuotaPoint {
        used_percent: 90.0,
        observed_at_ms: 61_000,
        resets_at_ms: 200_000,
        ..point.clone()
    };
    assert!(matches!(
        runway(&[point, next], 61_000),
        Estimate::Unknown { .. }
    ));
}
