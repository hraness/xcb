use crate::{Id, models::ModelChoice};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Terminal {
    Completed,
    TokenLimit,
    TurnLimit,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    None,
    Settled,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    AccountQuota,
    ModelQuota,
    Authentication,
    Policy,
    Transport,
    Unknown,
    /// Reported, never recorded: the provider completed the turn without
    /// answer text or workspace effects (see [`no_reply`]). Recorded facts
    /// keep `failure: None`, so continuation can still act on the turn and
    /// earlier builds can still read the settled-outcome record.
    NoReply,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnFacts {
    pub terminal: Terminal,
    pub joined: bool,
    pub effects: EffectState,
    pub pending_attention: bool,
    pub failure: Option<Failure>,
}

impl TurnFacts {
    /// The facts a result reports for a turn that ended with `text`: a turn
    /// that [`no_reply`] describes reports [`Failure::NoReply`].
    pub fn reported(&self, text: &str) -> Self {
        let mut facts = self.clone();
        if no_reply(text, self) {
            facts.failure = Some(Failure::NoReply);
        }
        facts
    }
}

/// A turn the provider reported as completed that produced no answer text
/// and no workspace effects. Nothing tells the caller whether the task was
/// done, so no surface may present it as a finished answer.
pub fn no_reply(text: &str, facts: &TurnFacts) -> bool {
    facts.terminal == Terminal::Completed
        && facts.failure.is_none()
        && facts.effects == EffectState::None
        && text.trim().is_empty()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoContinue {
    pub enabled: bool,
    pub max_consecutive: u32,
    pub max_elapsed_ms: u64,
}
impl Default for AutoContinue {
    fn default() -> Self {
        Self {
            enabled: true,
            max_consecutive: 3,
            max_elapsed_ms: 600_000,
        }
    }
}

pub fn should_continue(
    policy: &AutoContinue,
    facts: &TurnFacts,
    consecutive: u32,
    elapsed_ms: u64,
    repeated: bool,
) -> bool {
    policy.enabled
        && (1..=16).contains(&policy.max_consecutive)
        && (1000..=3_600_000).contains(&policy.max_elapsed_ms)
        && consecutive < policy.max_consecutive
        && elapsed_ms < policy.max_elapsed_ms
        && !repeated
        && facts.joined
        && facts.effects != EffectState::Uncertain
        && !facts.pending_attention
        && facts.failure.is_none()
        && matches!(facts.terminal, Terminal::TokenLimit | Terminal::TurnLimit)
}

#[derive(Debug, Clone)]
pub struct RouteCandidate {
    pub account: Id,
    pub model: ModelChoice,
    pub admitted: bool,
    pub quota_fresh: bool,
    pub available: bool,
}

pub fn next_route<'a>(
    current: &RouteCandidate,
    ordered: &'a [RouteCandidate],
    tried: &BTreeSet<String>,
    facts: &TurnFacts,
    checkpointed: bool,
) -> Option<&'a RouteCandidate> {
    if !facts.joined
        || facts.effects == EffectState::Uncertain
        || !checkpointed
        || facts.pending_attention
        || ordered.len() > 256
        || tried.len() >= 16
    {
        return None;
    }
    if facts.terminal != Terminal::Failed {
        return None;
    }
    let failure = facts.failure?;
    if !matches!(failure, Failure::AccountQuota | Failure::ModelQuota) {
        return None;
    }
    ordered.iter().find(|candidate| {
        let key = format!("{}/{}", candidate.account, candidate.model.key());
        candidate.admitted
            && candidate.quota_fresh
            && candidate.available
            && !tried.contains(&key)
            && !(candidate.account == current.account && candidate.model == current.model)
            && (failure != Failure::AccountQuota || candidate.account != current.account)
    })
}
