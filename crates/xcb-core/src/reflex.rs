//! Reflexes: small learned decisions that improve with use.
//!
//! A reflex is a fast, cheap decision (which model tier, how a turn ended)
//! split into three parts with separate lifecycles:
//!
//! - the **program**, an ALGAL organism owned by the runtime, which turns
//!   features, evidence and parameters into a decision and a receipt;
//! - the **parameters**, versioned logistic [`Head`]s that this module fits
//!   and promotes; they are data, so learning never changes the program digest;
//! - the **ledger**, observations with delayed labels, owned by the runtime.
//!
//! This module is pure: feature extraction over text, fitting, evaluation and
//! the promotion rule. It performs no IO and has no clock.

use crate::{
    Error, Result,
    policy::{EffectState, Terminal, TurnFacts},
    session::State,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_FEATURES: usize = 32;
pub const MAX_HEADS: usize = 4;
pub const MAX_EXAMPLES: usize = 4096;
const MAX_NAME: usize = 48;
const MAX_WEIGHT: f64 = 64.0;

pub type Features = BTreeMap<String, f64>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reflex {
    /// Frontier or standard model tier for a new task.
    Route,
    /// How a settled worker turn ended, including whether it stopped short.
    Settle,
}

impl Reflex {
    pub const ALL: [Self; 2] = [Self::Route, Self::Settle];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Route => "route",
            Self::Settle => "settle",
        }
    }
}

impl std::str::FromStr for Reflex {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "route" => Ok(Self::Route),
            "settle" => Ok(Self::Settle),
            _ => Err(Error::Invalid("reflex")),
        }
    }
}

/// A logistic decision head: `p = σ(bias + Σ weight·feature)`, decided at
/// `p ≥ threshold`. The threshold is policy (the cost of each error), not a
/// learned value, so fitting preserves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    pub bias: f64,
    pub weights: BTreeMap<String, f64>,
    pub threshold: f64,
}

impl Head {
    pub fn validate(&self) -> Result<()> {
        if self.weights.is_empty() || self.weights.len() > MAX_FEATURES {
            return Err(Error::Limit("reflex features"));
        }
        if !(self.threshold > 0.0 && self.threshold < 1.0) {
            return Err(Error::Invalid("reflex threshold"));
        }
        for (name, weight) in &self.weights {
            valid_name(name)?;
            bounded(*weight)?;
        }
        bounded(self.bias)
    }

    pub fn logit(&self, features: &Features) -> f64 {
        self.bias
            + self
                .weights
                .iter()
                .map(|(name, weight)| weight * features.get(name).copied().unwrap_or(0.0))
                .sum::<f64>()
    }

    pub fn probability(&self, features: &Features) -> f64 {
        sigmoid(self.logit(features))
    }

    /// The decision boundary in logit space, which ALGAL `expr` programs
    /// compare against because the expression language has no `exp`.
    pub fn threshold_logit(&self) -> f64 {
        (self.threshold / (1.0 - self.threshold)).ln()
    }

    pub fn decide(&self, features: &Features) -> bool {
        self.logit(features) >= self.threshold_logit()
    }
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAX_NAME
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(Error::Invalid("reflex name"));
    }
    Ok(())
}

fn bounded(value: f64) -> Result<()> {
    if value.is_finite() && value.abs() <= MAX_WEIGHT {
        Ok(())
    } else {
        Err(Error::Invalid("reflex weight"))
    }
}

pub fn sigmoid(z: f64) -> f64 {
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// One promoted generation of a reflex's parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    pub reflex: Reflex,
    /// Monotonic generation. Version 0 is the shipped prior.
    pub version: u32,
    pub parent: Option<u32>,
    pub heads: BTreeMap<String, Head>,
    /// Labeled examples the generation was fitted on (0 for the prior).
    pub trained_on: u32,
    /// Holdout evidence that justified promotion, per head.
    #[serde(default)]
    pub evidence: BTreeMap<String, Comparison>,
}

impl Params {
    pub fn validate(&self) -> Result<()> {
        if self.heads.is_empty() || self.heads.len() > MAX_HEADS {
            return Err(Error::Limit("reflex heads"));
        }
        let expected: BTreeSet<&str> = heads_for(self.reflex).iter().copied().collect();
        if self
            .heads
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != expected
        {
            return Err(Error::Invalid("reflex heads"));
        }
        for (name, head) in &self.heads {
            valid_name(name)?;
            head.validate()?;
        }
        if self.parent.is_some_and(|parent| parent >= self.version) {
            return Err(Error::Invalid("reflex lineage"));
        }
        Ok(())
    }

    pub fn head(&self, name: &str) -> Result<&Head> {
        self.heads.get(name).ok_or(Error::Invalid("reflex head"))
    }
}

pub fn heads_for(reflex: Reflex) -> &'static [&'static str] {
    match reflex {
        Reflex::Route => &[ROUTE_JUDGED, ROUTE_PLAIN],
        Reflex::Settle => &[SETTLE_UNFINISHED],
    }
}

/// Route head used when typed judge evidence is present.
pub const ROUTE_JUDGED: &str = "judged";
/// Route head used from deterministic features alone.
pub const ROUTE_PLAIN: &str = "plain";
/// Settle head: probability that an idle, completed turn stopped short.
pub const SETTLE_UNFINISHED: &str = "unfinished";

fn head(bias: f64, threshold: f64, weights: &[(&str, f64)]) -> Head {
    Head {
        bias,
        threshold,
        weights: weights
            .iter()
            .map(|(name, weight)| ((*name).to_owned(), *weight))
            .collect(),
    }
}

/// The shipped generation-0 parameters. Each prior reproduces the behavior
/// xcb had before reflexes existed, so enabling a reflex changes nothing until
/// local evidence earns a promotion.
pub fn prior(reflex: Reflex) -> Params {
    let heads = match reflex {
        Reflex::Route => BTreeMap::from([
            // ALGAL examples/model-router.algal.json (306 first prompts,
            // 8-fold CV AUC 0.739), threshold p ≥ 0.35.
            (
                ROUTE_JUDGED.to_owned(),
                head(
                    -1.8027,
                    0.35,
                    &[
                        ("words", 0.4530),
                        ("imperative", 1.5943),
                        ("resume", 0.6881),
                        ("verbs", 0.4405),
                        ("difficulty", -0.0755),
                        ("scope", 0.8716),
                        ("ambiguity", -0.1508),
                        ("stakes", -0.4787),
                        ("frontier", 0.5851),
                    ],
                ),
            ),
            // The keyword fallback: frontier exactly when the task carries a
            // complex cue. Zero weights leave room for learning.
            (
                ROUTE_PLAIN.to_owned(),
                head(
                    -1.0,
                    0.5,
                    &[
                        ("complex_cue", 2.0),
                        ("routine_cue", 0.0),
                        ("words", 0.0),
                        ("imperative", 0.0),
                        ("resume", 0.0),
                        ("verbs", 0.0),
                    ],
                ),
            ),
        ]),
        Reflex::Settle => BTreeMap::from([(
            SETTLE_UNFINISHED.to_owned(),
            head(
                -1.5,
                0.6,
                &[
                    ("promise_next", 1.6),
                    ("awaiting", 2.2),
                    ("remaining_work", 1.0),
                    ("open_checklist", 1.2),
                    ("ends_colon", 1.5),
                    ("ends_mid", 0.8),
                    ("done_claim", -1.2),
                    ("offer_more", -0.8),
                    ("words", -0.3),
                    ("effects", 0.0),
                ],
            ),
        )]),
    };
    Params {
        reflex,
        version: 0,
        parent: None,
        heads,
        trained_on: 0,
        evidence: BTreeMap::new(),
    }
}

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

/// The action verbs of ALGAL's model router, in its order.
pub const VERBS: &[&str] = &[
    "run",
    "create",
    "fix",
    "add",
    "build",
    "make",
    "take",
    "resume",
    "continue",
    "check",
    "review",
    "audit",
    "deploy",
    "merge",
    "push",
    "migrate",
    "refactor",
    "test",
    "write",
    "update",
    "remove",
    "delete",
    "install",
    "setup",
    "set",
    "configure",
    "read",
    "search",
    "find",
    "analyze",
    "improve",
    "change",
    "edit",
    "implement",
    "design",
    "ship",
    "launch",
    "clean",
    "move",
    "rename",
    "upgrade",
    "inspect",
    "verify",
    "publish",
    "integrate",
    "combine",
    "unify",
    "port",
    "rewrite",
    "rework",
    "overhaul",
    "assess",
    "evaluate",
    "compare",
    "investigate",
    "debug",
    "diagnose",
    "trace",
    "profile",
    "optimize",
    "benchmark",
    "scrape",
    "extract",
    "sync",
    "import",
    "export",
    "generate",
];

const RESUME_CUES: &[&str] = &[
    "session named",
    "session called",
    "take over",
    "pick up",
    "continue the work",
];

/// Deterministic prompt features for the route reflex. They preserve ALGAL's
/// literal-space tokenization, which lowercases for membership but does not
/// strip punctuation, and counts distinct original spellings before capping.
pub fn route_features(task: &str, complex_cue: bool, routine_cue: bool) -> Features {
    let words: Vec<_> = task.split(' ').filter(|word| !word.is_empty()).collect();
    let first = words.first().copied().unwrap_or("").to_lowercase();
    let lower = task.to_lowercase();
    let imperative = VERBS.contains(&first.as_str());
    let resume = ["resume", "continue"].contains(&first.as_str())
        || RESUME_CUES.iter().any(|cue| lower.contains(cue));
    let verbs = words
        .iter()
        .copied()
        .filter(|word| VERBS.contains(&word.to_lowercase().as_str()))
        .collect::<BTreeSet<_>>()
        .len()
        .min(8);
    Features::from([
        ("words".into(), words.len().min(400) as f64 / 400.0),
        ("imperative".into(), flag(imperative)),
        ("resume".into(), flag(resume)),
        ("verbs".into(), verbs as f64 / 8.0),
        ("complex_cue".into(), flag(complex_cue)),
        ("routine_cue".into(), flag(routine_cue)),
    ])
}

/// Adds typed judge evidence to route features. Scores arrive on the judge's
/// criterion-index scale and are divided by five exactly as the fitted ALGAL
/// head was trained.
pub fn with_judge_evidence(
    mut features: Features,
    difficulty: f64,
    scope: f64,
    ambiguity: f64,
    stakes: f64,
    frontier: f64,
) -> Features {
    for (name, value) in [
        ("difficulty", difficulty / 5.0),
        ("scope", scope / 5.0),
        ("ambiguity", ambiguity / 5.0),
        ("stakes", stakes / 5.0),
        ("frontier", frontier),
    ] {
        features.insert(name.into(), value);
    }
    features
}

const PROMISE_NEXT: &[&str] = &[
    "next, i'll",
    "next i'll",
    "next i will",
    "i'll now",
    "i will now",
    "now i'll",
    "let me now",
    "then i'll",
    "i'll continue",
    "continuing with",
    "moving on to",
    "next step is",
    "next steps:",
    "i'll start",
    "i'll proceed",
];
const REMAINING_WORK: &[&str] = &[
    "remaining:",
    "still need",
    "still needs",
    "left to do",
    "not yet",
    "todo",
    "to-do",
    "haven't yet",
    "have not yet",
    "yet to",
    "partially",
    "in progress",
    "work in progress",
    "follow-up",
    "unfinished",
];
/// The worker parked itself on an external event (CI, a watcher, a
/// background job) and ended its turn. In local transcripts this is the most
/// common response followed by the user typing "continue".
const AWAITING: &[&str] = &[
    "waiting on",
    "waiting for",
    "i'm waiting",
    "still waiting",
    "waits on",
    "wait until",
    "will report",
    "watcher will",
    "when it reports",
    "when the loop reports",
    "as soon as its",
    "as soon as the",
    "on its completion",
    "once it finishes",
    "once ci",
    "on success i'll",
    "i'll pick up",
    "i'll merge on green",
    "nothing else can proceed",
    "nothing else can run",
    "nothing else is independent",
    "until that returns",
];
const DONE_CLAIM: &[&str] = &[
    "done",
    "complete",
    "completed",
    "finished",
    "all tests pass",
    "tests pass",
    "merged",
    "shipped",
    "deployed",
    "landed",
    "all set",
    "ready for review",
];
const OFFER_MORE: &[&str] = &[
    "let me know if",
    "if you want",
    "if you'd like",
    "happy to",
    "i can also",
    "want me to",
];
const BLOCKED: &[&str] = &[
    "i can't",
    "i cannot",
    "unable to",
    "i'm blocked",
    "i am blocked",
    "blocked on",
    "blocked by",
    "permission denied",
    "not authorized",
];

fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(chars))
        .collect::<String>()
        .to_lowercase()
}

fn any(text: &str, cues: &[&str]) -> bool {
    cues.iter().any(|cue| contains_word(text, cue))
}

/// Cue matching respects word boundaries so "done" does not match "abandoned".
fn contains_word(text: &str, cue: &str) -> bool {
    text.match_indices(cue).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + cue.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn flag(value: bool) -> f64 {
    f64::from(u8::from(value))
}

/// Deterministic features of a settled worker response. Only the final 1,200
/// characters carry cues, matching [`crate::session::classify`]: the end of a
/// report says whether the worker stopped or intends to go on.
pub fn settle_features(text: &str, facts: &TurnFacts) -> Features {
    let end = tail(text, 1200);
    let trimmed = text.trim_end();
    let last = trimmed.chars().next_back();
    let open = end.matches("- [ ]").count().min(5);
    let words = text.split_whitespace().take(400).count();
    Features::from([
        ("promise_next".into(), flag(any(&end, PROMISE_NEXT))),
        ("awaiting".into(), flag(any(&end, AWAITING))),
        ("remaining_work".into(), flag(any(&end, REMAINING_WORK))),
        ("open_checklist".into(), open as f64 / 5.0),
        ("ends_colon".into(), flag(last == Some(':'))),
        (
            "ends_mid".into(),
            flag(last.is_some_and(|c| c.is_alphanumeric() || c == ',')),
        ),
        ("done_claim".into(), flag(any(&end, DONE_CLAIM))),
        ("offer_more".into(), flag(any(&end, OFFER_MORE))),
        ("blocked".into(), flag(any(&end, BLOCKED))),
        ("words".into(), words as f64 / 400.0),
        (
            "effects".into(),
            flag(facts.effects == EffectState::Settled),
        ),
        (
            "limit".into(),
            flag(matches!(
                facts.terminal,
                Terminal::TokenLimit | Terminal::TurnLimit
            )),
        ),
    ])
}

/// How a settled turn ended. `Done` and `StoppedShort` refine the idle state
/// that [`crate::session::classify`] reports; every other category is the
/// deterministic state it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Done,
    StoppedShort,
    Interrupted,
    Question,
    NeedsAction,
    NeedsApproval,
    Blocked,
    Limited,
    Failed,
    Cancelled,
    Uncertain,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::StoppedShort => "stopped_short",
            Self::Interrupted => "interrupted",
            Self::Question => "question",
            Self::NeedsAction => "needs_action",
            Self::NeedsApproval => "needs_approval",
            Self::Blocked => "blocked",
            Self::Limited => "limited",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Done,
            Self::StoppedShort,
            Self::Interrupted,
            Self::Question,
            Self::NeedsAction,
            Self::NeedsApproval,
            Self::Blocked,
            Self::Limited,
            Self::Failed,
            Self::Cancelled,
            Self::Uncertain,
        ]
        .into_iter()
        .find(|category| category.as_str() == value)
    }

    /// Categories whose remaining work is the worker's, not the user's.
    pub fn unfinished(self) -> bool {
        matches!(self, Self::StoppedShort | Self::Interrupted)
    }
}

/// The deterministic state name the settle program receives.
pub fn state_name(state: State) -> &'static str {
    match state {
        State::Idle => "idle",
        State::Working => "working",
        State::NeedsAnswer => "needs_answer",
        State::NeedsAction => "needs_action",
        State::NeedsApproval => "needs_approval",
        State::Limited => "limited",
        State::Failed => "failed",
        State::Cancelled => "cancelled",
        State::Uncertain => "uncertain",
    }
}

// ---------------------------------------------------------------------------
// Learning
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Example {
    pub id: String,
    pub features: Features,
    pub label: bool,
    /// Label confidence in (0, 1]: explicit feedback is 1, inferred behavior less.
    pub weight: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitOptions {
    /// Strength of the pull toward the parent generation, in pseudo-examples.
    pub prior_strength: f64,
    pub iterations: u32,
    pub learning_rate: f64,
}

impl Default for FitOptions {
    fn default() -> Self {
        Self {
            prior_strength: 24.0,
            iterations: 600,
            learning_rate: 0.5,
        }
    }
}

/// Maximum a-posteriori logistic regression anchored on `prior`: full-batch
/// gradient descent on weighted log-loss plus a Gaussian penalty centred on
/// the parent weights. Few examples barely move the head; many examples
/// dominate it. The feature set and threshold are the parent's.
/// Deterministic for a given input order.
pub fn fit(prior: &Head, examples: &[Example], options: FitOptions) -> Result<Head> {
    prior.validate()?;
    if examples.len() > MAX_EXAMPLES {
        return Err(Error::Limit("reflex examples"));
    }
    let names: Vec<&String> = prior.weights.keys().collect();
    let rows: Vec<(Vec<f64>, f64, f64)> = examples
        .iter()
        .filter(|example| example.weight > 0.0 && example.weight <= 1.0)
        .map(|example| {
            (
                names
                    .iter()
                    .map(|name| {
                        example
                            .features
                            .get(*name)
                            .copied()
                            .filter(|value| value.is_finite())
                            .unwrap_or(0.0)
                    })
                    .collect(),
                f64::from(u8::from(example.label)),
                example.weight,
            )
        })
        .collect();
    let mut bias = prior.bias;
    let mut weights: Vec<f64> = names.iter().map(|name| prior.weights[*name]).collect();
    let total: f64 = rows.iter().map(|(_, _, weight)| weight).sum();
    if total <= 0.0 {
        return Ok(prior.clone());
    }
    let scale = total + options.prior_strength;
    for _ in 0..options.iterations {
        let mut grad_bias = options.prior_strength * (bias - prior.bias);
        let mut grad: Vec<f64> = names
            .iter()
            .zip(&weights)
            .map(|(name, weight)| options.prior_strength * (weight - prior.weights[*name]))
            .collect();
        for (x, y, w) in &rows {
            let z = bias + x.iter().zip(&weights).map(|(a, b)| a * b).sum::<f64>();
            let error = w * (sigmoid(z) - y);
            grad_bias += error;
            for (g, value) in grad.iter_mut().zip(x) {
                *g += error * value;
            }
        }
        bias -= options.learning_rate * grad_bias / scale;
        for (weight, g) in weights.iter_mut().zip(&grad) {
            *weight -= options.learning_rate * g / scale;
        }
    }
    let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
    let fitted = Head {
        bias: round(bias).clamp(-MAX_WEIGHT, MAX_WEIGHT),
        weights: names
            .into_iter()
            .cloned()
            .zip(
                weights
                    .into_iter()
                    .map(|w| round(w).clamp(-MAX_WEIGHT, MAX_WEIGHT)),
            )
            .collect(),
        threshold: prior.threshold,
    };
    fitted.validate()?;
    Ok(fitted)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub n: u32,
    pub positives: u32,
    pub log_loss: f64,
    pub accuracy: f64,
    /// Ranking quality; absent when only one class is present.
    pub auc: Option<f64>,
}

pub fn evaluate(head: &Head, examples: &[Example]) -> Metrics {
    let mut loss = 0.0;
    let mut correct = 0.0;
    let mut total = 0.0;
    let mut scored = Vec::with_capacity(examples.len());
    for example in examples {
        let p = head.probability(&example.features).clamp(1e-6, 1.0 - 1e-6);
        let w = example.weight;
        loss -= w * if example.label {
            p.ln()
        } else {
            (1.0 - p).ln()
        };
        correct += w * f64::from(u8::from(head.decide(&example.features) == example.label));
        total += w;
        scored.push((p, example.label));
    }
    let positives = examples.iter().filter(|example| example.label).count();
    let round = |value: f64| (value * 10_000.0).round() / 10_000.0;
    Metrics {
        n: examples.len() as u32,
        positives: positives as u32,
        log_loss: if total > 0.0 {
            round(loss / total)
        } else {
            0.0
        },
        accuracy: if total > 0.0 {
            round(correct / total)
        } else {
            0.0
        },
        auc: auc(&scored).map(round),
    }
}

/// Mann–Whitney AUC with ties counted as one half.
fn auc(scored: &[(f64, bool)]) -> Option<f64> {
    let positives: Vec<f64> = scored.iter().filter(|s| s.1).map(|s| s.0).collect();
    let negatives: Vec<f64> = scored.iter().filter(|s| !s.1).map(|s| s.0).collect();
    if positives.is_empty() || negatives.is_empty() {
        return None;
    }
    let mut wins = 0.0;
    for p in &positives {
        for n in &negatives {
            wins += if p > n {
                1.0
            } else if p == n {
                0.5
            } else {
                0.0
            };
        }
    }
    Some(wins / (positives.len() * negatives.len()) as f64)
}

/// A stable 1-in-5 holdout assignment. An example never moves between the
/// training and holdout sets as the ledger grows, so a candidate is always
/// judged on examples it has never been fitted on.
pub fn is_holdout(id: &str) -> bool {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash.is_multiple_of(5)
}

pub const MIN_HOLDOUT: u32 = 12;
/// Each class needs this many held-out examples before a comparison can
/// promote; a handful of positives measures noise, not discrimination.
pub const MIN_HOLDOUT_CLASS: u32 = 5;
const MAX_AUC_LOSS: f64 = 0.02;
const MIN_LOG_LOSS_GAIN: f64 = 0.002;
const MAX_ACCURACY_LOSS: f64 = 0.02;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    pub current: Metrics,
    pub candidate: Metrics,
    pub promoted: bool,
    pub reason: String,
}

/// The promotion rule: a candidate replaces the current head only on held-out
/// evidence with enough of both classes, lower log-loss, and no material
/// accuracy or ranking (AUC) regression. Anything else keeps the current head.
pub fn compare(current: &Head, candidate: &Head, holdout: &[Example]) -> Comparison {
    let current_metrics = evaluate(current, holdout);
    let candidate_metrics = evaluate(candidate, holdout);
    let negatives = current_metrics.n - current_metrics.positives;
    let (promoted, reason) = if current_metrics.n < MIN_HOLDOUT {
        (false, format!("needs {MIN_HOLDOUT} held-out labels"))
    } else if current_metrics.positives.min(negatives) < MIN_HOLDOUT_CLASS {
        (
            false,
            format!("needs {MIN_HOLDOUT_CLASS} held-out labels of each class"),
        )
    } else if candidate_metrics.log_loss > current_metrics.log_loss - MIN_LOG_LOSS_GAIN {
        (false, "no held-out log-loss improvement".to_owned())
    } else if candidate_metrics.accuracy < current_metrics.accuracy - MAX_ACCURACY_LOSS {
        (false, "held-out accuracy regressed".to_owned())
    } else if let (Some(current), Some(candidate)) = (current_metrics.auc, candidate_metrics.auc)
        && candidate < current - MAX_AUC_LOSS
    {
        (false, "held-out ranking (AUC) regressed".to_owned())
    } else {
        (true, "lower held-out log-loss".to_owned())
    };
    Comparison {
        current: current_metrics,
        candidate: candidate_metrics,
        promoted,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Failure;

    fn facts(terminal: Terminal, effects: EffectState) -> TurnFacts {
        TurnFacts {
            terminal,
            joined: true,
            effects,
            pending_attention: false,
            failure: None::<Failure>,
        }
    }

    #[test]
    fn priors_validate_and_reproduce_prior_behavior() {
        for reflex in Reflex::ALL {
            prior(reflex).validate().unwrap();
        }
        let route = prior(Reflex::Route);
        let plain = route.head(ROUTE_PLAIN).unwrap();
        for complex in [false, true] {
            for routine in [false, true] {
                let features = route_features("fix the thing", complex, routine);
                assert_eq!(plain.decide(&features), complex);
            }
        }
        // The fitted threshold p ≥ 0.35 is the historical logit -0.619.
        let judged = route.head(ROUTE_JUDGED).unwrap();
        assert!((judged.threshold_logit() - -0.619).abs() < 0.001);
    }

    #[test]
    fn route_features_keep_algal_tokenization() {
        let features = route_features("Fix fix, the bug and take over", false, false);
        assert_eq!(features["imperative"], 1.0);
        assert_eq!(features["resume"], 1.0);
        // "Fix" and "fix," are distinct spellings; only "Fix" and "take" are verbs.
        assert_eq!(features["verbs"], 2.0 / 8.0);
        assert_eq!(features["words"], 7.0 / 400.0);
    }

    #[test]
    fn settle_features_read_the_end_of_the_report() {
        let facts = facts(Terminal::Completed, EffectState::Settled);
        let short = settle_features("Updated the parser. Next, I'll wire the CLI:", &facts);
        assert_eq!(short["promise_next"], 1.0);
        assert_eq!(short["ends_colon"], 1.0);
        let done = settle_features("All tests pass and the PR is merged.", &facts);
        assert_eq!(done["done_claim"], 1.0);
        assert_eq!(done["promise_next"], 0.0);
        // Word boundaries: "abandoned" is not a done claim.
        let abandoned = settle_features("the branch was abandoned.", &facts);
        assert_eq!(abandoned["done_claim"], 0.0);
        let unfinished = prior(Reflex::Settle);
        let head = unfinished.head(SETTLE_UNFINISHED).unwrap();
        assert!(head.decide(&short));
        assert!(!head.decide(&done));
    }

    fn example(id: usize, features: Features, label: bool) -> Example {
        Example {
            id: format!("e{id}"),
            features,
            label,
            weight: 1.0,
        }
    }

    #[test]
    fn fitting_moves_toward_evidence_and_is_anchored_by_the_prior() {
        let prior = prior(Reflex::Route).heads[ROUTE_PLAIN].clone();
        // Evidence: imperative prompts go frontier regardless of cues.
        let examples: Vec<_> = (0..200)
            .map(|i| {
                let imperative = i % 2 == 0;
                let task = if imperative { "build it" } else { "what is it" };
                example(i, route_features(task, false, false), imperative)
            })
            .collect();
        let few = fit(&prior, &examples[..4], FitOptions::default()).unwrap();
        let many = fit(&prior, &examples, FitOptions::default()).unwrap();
        assert!(many.weights["imperative"] > few.weights["imperative"]);
        assert!(few.weights["imperative"] < 1.0);
        assert!(many.decide(&route_features("build it", false, false)));
        assert!(!many.decide(&route_features("what is it", false, false)));
        assert_eq!(many.threshold, prior.threshold);
        // Deterministic.
        assert_eq!(many, fit(&prior, &examples, FitOptions::default()).unwrap());
        let metrics = evaluate(&many, &examples);
        assert_eq!(metrics.accuracy, 1.0);
        assert_eq!(metrics.auc, Some(1.0));
    }

    #[test]
    fn promotion_requires_heldout_evidence_from_both_classes() {
        let current = prior(Reflex::Route).heads[ROUTE_PLAIN].clone();
        let examples: Vec<_> = (0..40)
            .map(|i| {
                let imperative = i % 2 == 0;
                let task = if imperative { "build it" } else { "what is it" };
                example(i, route_features(task, false, false), imperative)
            })
            .collect();
        let candidate = fit(&current, &examples, FitOptions::default()).unwrap();
        assert!(!compare(&current, &candidate, &examples[..6]).promoted);
        let one_class: Vec<_> = examples.iter().filter(|e| e.label).cloned().collect();
        assert!(!compare(&current, &candidate, &one_class).promoted);
        let verdict = compare(&current, &candidate, &examples);
        assert!(verdict.promoted, "{verdict:?}");
        // The current head never loses to itself.
        assert!(!compare(&candidate, &candidate, &examples).promoted);
    }

    #[test]
    fn holdout_assignment_is_stable_and_roughly_one_fifth() {
        let held = (0..1000)
            .filter(|i| is_holdout(&format!("obs_{i}")))
            .count();
        assert!((150..=250).contains(&held), "{held}");
        assert_eq!(is_holdout("obs_7"), is_holdout("obs_7"));
    }

    #[test]
    fn params_reject_foreign_heads_and_unbounded_weights() {
        let mut params = prior(Reflex::Settle);
        params
            .heads
            .insert("extra".into(), params.heads[SETTLE_UNFINISHED].clone());
        assert!(params.validate().is_err());
        let mut params = prior(Reflex::Settle);
        params
            .heads
            .get_mut(SETTLE_UNFINISHED)
            .unwrap()
            .weights
            .insert("words".into(), f64::NAN);
        assert!(params.validate().is_err());
        let mut params = prior(Reflex::Settle);
        params.parent = Some(0);
        assert!(params.validate().is_err());
    }
}
