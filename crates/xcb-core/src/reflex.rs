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
        Reflex::Settle => &[SETTLE_UNFINISHED, SETTLE_CONFIRM],
    }
}

/// Route head used when typed judge evidence is present.
pub const ROUTE_JUDGED: &str = "judged";
/// Route head used from deterministic features alone.
pub const ROUTE_PLAIN: &str = "plain";
/// Settle head: probability that an idle, completed turn stopped short, so
/// the operator's next move would be "continue".
pub const SETTLE_UNFINISHED: &str = "unfinished";
/// Settle head: probability that an idle, completed turn is waiting for the
/// operator to confirm a step the worker proposed, so the next move would be
/// "yes, go ahead".
pub const SETTLE_CONFIRM: &str = "confirm";

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

/// The shipped generation-0 parameters. The route priors reproduce the
/// routing xcb had before reflexes existed. The settle priors are fitted on
/// generic end-of-turn cues and ship in observe mode (see the config).
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
        // Fitted on one operator's 2,428 end-of-turn → next-message pairs
        // from Claude Code, Codex and Devin transcripts (L2 logistic, C=0.5),
        // using only the generic cues below. The same fit on the first half
        // ranks the second half at AUC 0.79 (`unfinished`) and 0.76
        // (`confirm`); the earlier hand-set prior ranked 0.58. Thresholds trade recall for
        // precision: acting on a false "unfinished" spends a turn, and acting
        // on a false "confirm" approves something the operator did not want.
        Reflex::Settle => BTreeMap::from([
            (
                SETTLE_UNFINISHED.to_owned(),
                head(
                    -3.206,
                    0.65,
                    &[
                        ("ask", -0.267),
                        ("awaiting", 0.63),
                        ("blocked", -0.076),
                        ("brief", 0.855),
                        ("done_claim", -0.122),
                        ("ends_colon", 1.128),
                        ("ends_mid", -0.247),
                        ("ends_question", -0.308),
                        ("future_self", 0.26),
                        ("no_tools", 1.201),
                        ("offer_more", 0.274),
                        ("open_checklist", -0.02),
                        ("progressive", 1.177),
                        ("promise_next", 0.109),
                        ("question", -1.151),
                        ("remaining_work", 0.502),
                        ("report", -0.275),
                        ("risk", -0.234),
                        ("tools", 2.627),
                        ("user_act", -0.919),
                        ("words", -0.849),
                    ],
                ),
            ),
            (
                SETTLE_CONFIRM.to_owned(),
                head(
                    -3.398,
                    0.45,
                    &[
                        ("ask", 2.004),
                        ("awaiting", -0.506),
                        ("brief", -0.178),
                        ("done_claim", 0.168),
                        ("ends_mid", 0.473),
                        ("no_tools", -0.086),
                        ("offer_more", -0.192),
                        ("progressive", -1.367),
                        ("promise_next", 0.42),
                        ("question", 1.3),
                        ("report", -0.334),
                        ("risk", 0.482),
                        ("tools", 0.485),
                        ("user_act", -0.159),
                        ("words", -0.015),
                    ],
                ),
            ),
        ]),
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

/// The worker asks the operator to decide or confirm something.
const ASK: &[&str] = &[
    "may i",
    "should i",
    "shall i",
    "do you want",
    "would you like",
    "want me to",
    "ok to proceed",
    "okay to proceed",
    "can i proceed",
    "reply with",
    "say the word",
    "approve",
    "approval",
    "authorize",
    "authorization",
    "confirm",
    "permission",
    "go-ahead",
    "sign-off",
    "your call",
    "your decision",
    "which option",
    "option a",
    "option b",
    "prefer",
];
/// The operator has to act outside the conversation (sign in, run a
/// command, paste a value) before the worker can go on.
const USER_ACT: &[&str] = &[
    "please run",
    "please sign",
    "please log",
    "please enter",
    "please click",
    "please add",
    "please set",
    "please paste",
    "please install",
    "you'll need to",
    "you need to",
    "you will need to",
    "on your side",
    "on your end",
    "your side",
    "then reply",
    "then tell me",
    "once you've",
    "once you have",
    "when you've",
    "when you have",
    "sign in",
    "log in",
    "device code",
    "passkey",
    "2fa",
    "one-time",
    "verification code",
    "in your browser",
    "run this",
    "run the following",
    "paste the",
];
/// Irreversible, costly or credential-bearing steps. A feature for both
/// heads and, in the runtime, a hard veto on automatic confirmation.
pub const RISK: &[&str] = &[
    "delete",
    "drop",
    "destroy",
    "force-push",
    "force push",
    "wipe",
    "truncate",
    "production data",
    "irreversible",
    "purchase",
    "payment",
    "pay",
    "charge",
    "billing",
    "credit card",
    "secret",
    "credential",
    "token",
    "password",
    "publish",
    "rotate",
    "revoke",
    "transfer",
    "wire",
];
const FUTURE_SELF: &[&str] = &["i'll", "i will", "next i", "then i"];
const PROGRESSIVE_SUBJECTS: &[&str] = &["i'm", "we're", "now"];

/// Lowercased last `chars` characters with typographic apostrophes folded,
/// so "I’ll" and "I'll" are the same cue.
fn tail(text: &str, chars: usize) -> String {
    let count = text.chars().count();
    normalize(
        &text
            .chars()
            .skip(count.saturating_sub(chars))
            .collect::<String>(),
    )
}

fn normalize(text: &str) -> String {
    text.replace(['\u{2019}', '\u{2018}'], "'").to_lowercase()
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

/// Stems that veto answering a go-ahead request anywhere in the report, so a
/// destructive plan cannot hide above a short question: deletion, secrets and
/// production.
const VETO_ANYWHERE: &[&str] = &[
    "delet",
    "drop",
    "destroy",
    "wipe",
    "wiping",
    "truncat",
    "purg",
    "force-push",
    "force push",
    "push --force",
    "push -f",
    "reset --hard",
    "rm -",
    "credential",
    "password",
    "secret",
    "private key",
    "api key",
    "ssh key",
    "production",
    "prod",
    "spend",
    "spent",
    "purchas",
    "billing",
];

/// Stems that veto only in the paragraph that asks, where they describe the
/// step being proposed; across a whole work report they are ordinary words.
const VETO_IN_ASK: &[&str] = &[
    "deploy",
    "releas",
    "publish",
    "migrat",
    "push to main",
    "push to master",
    "tag",
    "live",
    "remov",
    "overwrit",
    "revert",
    "rollback",
    "roll back",
    "pay",
    "charg",
    "invoice",
    "subscri",
    "upgrad",
    "token",
    "access",
    "permission",
    "sudo",
    "send",
    "email",
    "post",
    "tweet",
    "invite",
    "share",
    "public",
];

/// Word endings a stem may carry, so "deleting" and "tokens" match but
/// "dropdown", "payload" and "tokenizer" do not.
const VETO_ENDINGS: &[&str] = &[
    "", "s", "es", "e", "ed", "d", "ing", "ion", "ions", "al", "ped", "ping", "ged", "ging",
    "ment", "ments", "be", "bed", "bing", "ption", "ptions", "y", "ies", "ly", "ten",
];

fn stem_matches(text: &str, stem: &str) -> bool {
    text.match_indices(stem).any(|(start, _)| {
        let ending: String = text[start + stem.len()..]
            .chars()
            .take_while(|c| c.is_alphanumeric())
            .collect();
        // A stem ending in punctuation ("rm -") takes any flag after it.
        let open_ended = !stem.ends_with(char::is_alphanumeric);
        !text[..start]
            .chars()
            .next_back()
            .is_some_and(char::is_alphanumeric)
            && (open_ended || VETO_ENDINGS.contains(&ending.as_str()))
    })
}

/// Whether a worker's request for a go-ahead names anything xcb must not
/// approve on the operator's behalf: deletion, secrets or production anywhere
/// in the report, or a deploy, release, migration, payment, access change or
/// message to people in the paragraph that asks. Deliberately
/// over-inclusive; a veto only leaves the question for the operator.
pub fn confirm_vetoed(text: &str) -> bool {
    let text = tail(text, 16 * 1024);
    let last = last_paragraph(&text);
    // A question followed by a list of options asks in the question's
    // paragraph, not the list's, so both are read as the ask.
    let asks = [last, question_paragraph(&text).unwrap_or(last)];
    VETO_ANYWHERE.iter().any(|stem| stem_matches(&text, stem))
        || asks.iter().any(|ask| {
            VETO_IN_ASK.iter().any(|stem| stem_matches(ask, stem))
                || any(ask, RISK)
                || any(ask, USER_ACT)
        })
}

/// The last paragraph that contains a question mark.
fn question_paragraph(text: &str) -> Option<&str> {
    paragraphs(text)
        .into_iter()
        .rev()
        .find(|paragraph| paragraph.contains('?'))
}

fn flag(value: bool) -> f64 {
    f64::from(u8::from(value))
}

/// The last paragraph, separated by a blank line.
fn last_paragraph(text: &str) -> &str {
    paragraphs(text).pop().unwrap_or("")
}

/// Paragraphs in order, separated by lines that are blank or whitespace.
fn paragraphs(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut current_start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() {
            if let Some(start) = current_start.take() {
                found.push(&text[start..offset]);
            }
        } else if current_start.is_none() {
            current_start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(start) = current_start {
        found.push(&text[start..]);
    }
    found
}

/// "I'm running…", "we're waiting…", "now building…": the worker describes
/// work still under way.
fn progressive(paragraph: &str) -> bool {
    let words: Vec<&str> = paragraph
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|word| !word.is_empty())
        .collect();
    words.windows(2).enumerate().any(|(index, pair)| {
        let subject = PROGRESSIVE_SUBJECTS.contains(&pair[0])
            || (pair[0] == "am" && index > 0 && words[index - 1] == "i");
        subject && pair[1].len() > 4 && pair[1].ends_with("ing")
    })
}

/// A markdown heading or a bold line on its own: a structured report, which
/// usually closes a turn rather than pausing one.
fn report(text: &str) -> bool {
    text.lines().any(|line| {
        let start = line.trim_start();
        let whole = line.trim();
        ["# ", "## ", "### "]
            .iter()
            .any(|prefix| start.starts_with(prefix))
            || (whole.len() > 4
                && whole.starts_with("**")
                && whole.ends_with("**")
                && !whole[2..whole.len() - 2].contains('*'))
    })
}

/// Deterministic features of a settled worker response. Cues come from the
/// end of the report (the last 1,200 characters, matching
/// [`crate::session::classify`]; the last 300 for asks and risks; the last
/// paragraph for questions and work in progress), plus how many tool calls the
/// turn made: in the mined history a turn with no tool calls was followed by
/// "continue" 15% of the time, and one with 40 or more 45% of the time.
pub fn settle_features(text: &str, facts: &TurnFacts, tool_calls: u32) -> Features {
    let end = tail(text, 1200);
    let close = tail(text, 300);
    let paragraph = normalize(last_paragraph(text));
    let trimmed = text.trim_end();
    let last = trimmed.chars().next_back();
    let open = end.matches("- [ ]").count().min(5);
    let words = text.split_whitespace().take(400).count();
    let calls = tool_calls.min(128);
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
        ("brief".into(), flag(words < 60)),
        ("progressive".into(), flag(progressive(&paragraph))),
        ("future_self".into(), flag(any(&close, FUTURE_SELF))),
        ("question".into(), flag(paragraph.contains('?'))),
        ("ends_question".into(), flag(last == Some('?'))),
        ("ask".into(), flag(any(&close, ASK))),
        ("user_act".into(), flag(any(&end, USER_ACT))),
        ("risk".into(), flag(any(&close, RISK))),
        ("report".into(), flag(report(text))),
        ("tools".into(), f64::from(calls).ln_1p() / 128f64.ln_1p()),
        ("no_tools".into(), flag(calls == 0)),
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

/// How a settled turn ended. `Done`, `StoppedShort` and `Confirm` refine the idle state
/// that [`crate::session::classify`] reports; every other category is the
/// deterministic state it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Done,
    StoppedShort,
    /// The worker proposed a step and is waiting for the operator's go-ahead.
    Confirm,
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
            Self::Confirm => "confirm",
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
            Self::Confirm,
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
    /// Share of decisions at the threshold that were right; absent when the
    /// head never fired.
    #[serde(default)]
    pub precision: Option<f64>,
    /// Share of positives the head fired on; absent without positives.
    #[serde(default)]
    pub recall: Option<f64>,
}

pub fn evaluate(head: &Head, examples: &[Example]) -> Metrics {
    let mut loss = 0.0;
    let mut correct = 0.0;
    let mut total = 0.0;
    let mut scored = Vec::with_capacity(examples.len());
    let (mut fired, mut hits, mut positive_weight) = (0.0, 0.0, 0.0);
    for example in examples {
        let p = head.probability(&example.features).clamp(1e-6, 1.0 - 1e-6);
        let w = example.weight;
        loss -= w * if example.label {
            p.ln()
        } else {
            (1.0 - p).ln()
        };
        let decided = head.decide(&example.features);
        correct += w * f64::from(u8::from(decided == example.label));
        total += w;
        if decided {
            fired += w;
        }
        if example.label {
            positive_weight += w;
            if decided {
                hits += w;
            }
        }
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
        precision: (fired > 0.0).then(|| round(hits / fired)),
        recall: (positive_weight > 0.0).then(|| round(hits / positive_weight)),
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

/// Labels a challenger must be scored on, after it was fitted, before it
/// can replace the active head.
pub const TRIAL_LABELS: u32 = 48;
/// A training pass runs after every this-many new labels.
pub const TRAIN_EVERY: u32 = 16;
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

/// The promotion rule, applied prospectively. A challenger is fitted, then
/// both it and the active head score the next labels as they arrive, which
/// neither was fitted on. The challenger replaces the active head after
/// [`TRIAL_LABELS`] such labels only if it has lower log-loss and no material
/// accuracy or ranking (AUC) regression.
///
/// In a replay of one operator's history, learning from a weak prior
/// (AUC 0.58 with no learning) reached a prequential AUC of 0.767 with forward
/// trials. With the earlier fixed hashed holdout it reached 0.748, and
/// judging on the newest labels already seen reached 0.699. Operator
/// behavior drifts, and only a forward trial measures a head on the labels it
/// will actually face.
pub fn compare(current: &Head, candidate: &Head, fresh: &[Example]) -> Comparison {
    let current_metrics = evaluate(current, fresh);
    let candidate_metrics = evaluate(candidate, fresh);
    let (promoted, reason) = if current_metrics.n < TRIAL_LABELS {
        (
            false,
            format!(
                "trial: {} of {TRIAL_LABELS} fresh labels",
                current_metrics.n
            ),
        )
    } else if candidate_metrics.log_loss > current_metrics.log_loss - MIN_LOG_LOSS_GAIN {
        (false, "no fresh log-loss improvement".to_owned())
    } else if candidate_metrics.accuracy < current_metrics.accuracy - MAX_ACCURACY_LOSS {
        (false, "fresh accuracy regressed".to_owned())
    } else if let (Some(current), Some(candidate)) = (current_metrics.auc, candidate_metrics.auc)
        && candidate < current - MAX_AUC_LOSS
    {
        (false, "fresh ranking (AUC) regressed".to_owned())
    } else {
        (true, "lower log-loss on fresh labels".to_owned())
    };
    Comparison {
        current: current_metrics,
        candidate: candidate_metrics,
        promoted,
        reason,
    }
}

/// True once a trial has scored enough fresh labels to be decided.
pub fn trial_complete(comparison: &Comparison) -> bool {
    comparison.current.n >= TRIAL_LABELS
}

/// The newest `MAX_EXAMPLES` examples, which bound every fit.
pub fn recent(examples: &[Example]) -> &[Example] {
    &examples[examples.len().saturating_sub(MAX_EXAMPLES)..]
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Replay {
    /// Every example scored by the head active when it arrived, before its
    /// label was learned from: the prequential measure of the whole process.
    pub prequential: Metrics,
    pub promotions: u32,
    pub trials: u32,
    pub head: Head,
    /// The trial that produced the final head, when any promoted.
    pub evidence: Option<Comparison>,
    /// Each example's probability from the head active when it arrived.
    #[serde(skip)]
    pub predicted: Vec<f64>,
}

/// Replays labeled history in order through the same learning loop the
/// ledger runs live: decide, learn the label, fit a challenger from `prior`
/// every [`TRAIN_EVERY`] labels, and promote it only after it wins a
/// forward trial. Pure and deterministic, so a history can be backtested
/// before its result is adopted.
pub fn replay(
    prior: &Head,
    start: &Head,
    examples: &[Example],
    options: FitOptions,
) -> Result<Replay> {
    prior.validate()?;
    start.validate()?;
    let mut head = start.clone();
    let mut challenger: Option<(Head, usize)> = None;
    let (mut promotions, mut trials) = (0, 0);
    let mut evidence = None;
    let mut predicted = Vec::with_capacity(examples.len());
    for (index, example) in examples.iter().enumerate() {
        predicted.push((head.probability(&example.features), example));
        let seen = index + 1;
        if let Some((candidate, from)) = &challenger
            && seen - from >= TRIAL_LABELS as usize
        {
            let verdict = compare(&head, candidate, &examples[*from..seen]);
            if verdict.promoted {
                head = candidate.clone();
                promotions += 1;
                evidence = Some(verdict);
            }
            challenger = None;
        }
        if challenger.is_none() && seen.is_multiple_of(TRAIN_EVERY as usize) {
            let candidate = fit(prior, recent(&examples[..seen]), options)?;
            if candidate != head {
                trials += 1;
                challenger = Some((candidate, seen));
            }
        }
    }
    Ok(Replay {
        prequential: prequential(&predicted, head.threshold),
        promotions,
        trials,
        head,
        evidence,
        predicted: predicted.iter().map(|(p, _)| *p).collect(),
    })
}

fn prequential(predicted: &[(f64, &Example)], threshold: f64) -> Metrics {
    // Score each example with the probability it was given at the time,
    // through a head whose logit reproduces that probability exactly.
    let examples: Vec<Example> = predicted
        .iter()
        .map(|(p, example)| {
            let p = p.clamp(1e-9, 1.0 - 1e-9);
            Example {
                id: example.id.clone(),
                features: Features::from([("p".to_owned(), (p / (1.0 - p)).ln())]),
                label: example.label,
                weight: example.weight,
            }
        })
        .collect();
    let identity = Head {
        bias: 0.0,
        weights: BTreeMap::from([("p".to_owned(), 1.0)]),
        threshold,
    };
    evaluate(&identity, &examples)
}

// ---------------------------------------------------------------------------
// Certification
// ---------------------------------------------------------------------------

/// Operator-labeled turns a certificate is judged on, newest first. At the
/// fire rates seen in practice (one turn in six), this many turns bound
/// precision to within about 0.05.
pub const CERTIFY_WINDOW: usize = 1500;
/// Label weight the head must have fired on inside the window.
pub const CERTIFY_MIN_FIRED: f64 = 30.0;
/// One-sided z for the precision lower bound: 99% for any one threshold.
/// Up to seven nested thresholds are tried and the test repeats on every
/// training pass, so the chance of certifying a head whose true precision is
/// under the floor is higher than 1%; the floor is a guardrail, and the
/// held-out turns keep testing it (see [`certify`]).
const CERTIFY_Z: f64 = 2.33;
/// How far a certified head's precision may sag before it is withdrawn.
const CERTIFY_MARGIN: f64 = 0.05;
const CERTIFY_STEP: f64 = 0.05;

/// The precision a settle head must show before `auto` lets it act, or
/// `None` for heads that never act on their own. Answering "yes" for the
/// operator is held to a higher bar than asking a worker to carry on.
pub fn precision_floor(head: &str) -> Option<f64> {
    match head {
        SETTLE_UNFINISHED => Some(0.75),
        SETTLE_CONFIRM => Some(0.85),
        _ => None,
    }
}

/// Whether a head could act on a turn with these features at all. The
/// runtime never answers a request that carries a risk or hand-off cue, so
/// such turns say nothing about the precision of the answers it gives. The
/// runtime also vetoes requests by their text ([`confirm_vetoed`]), which the
/// ledger does not keep, so certification still scores some requests the
/// runtime would decline; that only makes the estimate conservative when
/// those requests are the ones operators decline too.
pub fn actionable(head: &str, features: &Features) -> bool {
    head != SETTLE_CONFIRM
        || (features.get("risk") == Some(&0.0) && features.get("user_act") == Some(&0.0))
}

/// Whether a label came from the operator (a reply, an explicit label or
/// imported history) rather than from xcb watching its own action. Only
/// these can certify a head: once a head acts, the labels its own
/// continuations earn would confirm it.
pub fn operator_label(source: &str) -> bool {
    source.starts_with("user_") || ["explicit", "import", "v1"].contains(&source)
}

/// Evidence that a head may act without the operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    pub certified: bool,
    /// Probability the head must reach before it acts; at least its own
    /// decision threshold.
    pub threshold: f64,
    pub floor: f64,
    /// Operator-labeled examples scored.
    pub window: u32,
    /// Label weight of the examples the head would have acted on.
    pub fired: f64,
    pub precision: Option<f64>,
    /// Lower confidence bound on `precision`.
    pub lower: Option<f64>,
    pub reason: String,
    /// The head the replay ended with. Acting scores turns with this head,
    /// so the head that acts is the one whose precision was measured,
    /// whatever generation is active or was rolled back to.
    pub head: Head,
}

/// Decides whether a head has earned the right to act. The retained history
/// is replayed from `prior` through the live learning loop (see [`replay`]),
/// so every example is scored by a head that had not learned from it. On the
/// newest [`CERTIFY_WINDOW`] operator-labeled examples it could act on (see
/// [`actionable`]), the lowest acting
/// threshold whose precision is confidently above the head's floor wins. A
/// head that was certified keeps its threshold while its measured precision
/// stays within [`CERTIFY_MARGIN`] of the floor, so the verdict does not
/// flap; below that it is withdrawn. Once a head acts, only the turns held
/// for the operator add fired evidence, so withdrawal lags a real decline.
pub fn certify(
    name: &str,
    prior: &Head,
    examples: &[Example],
    operator: &[bool],
    previous: Option<&Certificate>,
    options: FitOptions,
) -> Result<Certificate> {
    let Some(floor) = precision_floor(name) else {
        return Err(Error::Invalid("reflex head"));
    };
    if examples.len() != operator.len() {
        return Err(Error::Invalid("certification evidence"));
    }
    let replayed = replay(prior, prior, examples, options)?;
    let mut scored: Vec<(f64, &Example)> = replayed
        .predicted
        .iter()
        .zip(examples)
        .zip(operator)
        .filter(|((_, example), counted)| **counted && actionable(name, &example.features))
        .map(|((p, example), _)| (*p, example))
        .collect();
    scored.drain(..scored.len().saturating_sub(CERTIFY_WINDOW));
    let at = |threshold: f64| {
        let (mut fired, mut hits) = (0.0, 0.0);
        for (p, example) in &scored {
            if *p >= threshold {
                fired += example.weight;
                if example.label {
                    hits += example.weight;
                }
            }
        }
        (fired, hits)
    };
    let verdict = |threshold: f64, certified: bool, reason: String| {
        let (fired, hits) = at(threshold);
        Certificate {
            certified,
            threshold,
            floor,
            window: scored.len() as u32,
            fired,
            precision: (fired > 0.0).then(|| hits / fired),
            lower: (fired > 0.0).then(|| wilson_lower(hits, fired, CERTIFY_Z)),
            reason,
            head: replayed.head.clone(),
        }
    };
    let base = prior.threshold;
    let mut threshold = base;
    while threshold < 1.0 - CERTIFY_STEP / 2.0 {
        let (fired, hits) = at(threshold);
        if fired < CERTIFY_MIN_FIRED {
            break;
        }
        if wilson_lower(hits, fired, CERTIFY_Z) >= floor {
            return Ok(verdict(
                threshold,
                true,
                format!("precision confidently at or above {floor:.2}"),
            ));
        }
        threshold += CERTIFY_STEP;
    }
    if let Some(previous) = previous.filter(|previous| previous.certified) {
        let (fired, hits) = at(previous.threshold);
        if fired >= CERTIFY_MIN_FIRED && hits / fired >= floor - CERTIFY_MARGIN {
            return Ok(verdict(
                previous.threshold,
                true,
                "certified; precision still within the margin".to_owned(),
            ));
        }
    }
    let (fired, _) = at(base);
    let reason = if fired < CERTIFY_MIN_FIRED {
        format!("collecting evidence: fired on {fired:.0} of {CERTIFY_MIN_FIRED:.0} labeled turns")
    } else {
        format!("precision not yet confidently at {floor:.2}")
    };
    Ok(verdict(base, false, reason))
}

/// Wilson score lower bound for `hits` of `n` (weights allowed).
fn wilson_lower(hits: f64, n: f64, z: f64) -> f64 {
    if n <= 0.0 {
        return 0.0;
    }
    let p = hits / n;
    let z2 = z * z;
    let centre = p + z2 / (2.0 * n);
    let spread = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ((centre - spread) / (1.0 + z2 / n)).max(0.0)
}

// ---------------------------------------------------------------------------
// Replies
// ---------------------------------------------------------------------------

/// What the operator's next message says about the turn before it: the
/// label source for settle heads. Ported from the categorizer used to mine
/// the shipped priors. Of 2,428 follow-ups, 30% were "continue" and 7.5% were
/// approvals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    /// "continue", "keep going", "finish the rest".
    Continue,
    /// "push it", "open a PR", "merge it": finish the delivery.
    Deliver,
    /// "yes", "go ahead", "approved".
    Approve,
    /// "done", "signed in": the operator finished a step the worker handed off.
    HandoffDone,
    /// "are you done?", "status?".
    Status,
    /// "no", "wait", "that's wrong".
    Correction,
    /// "limits are back": a quota, not a judgment about the turn.
    QuotaResume,
    /// Anything else: a new request.
    Other,
}

const REPLY_PREFIXES: &[&str] = &["ok ", "okay ", "ok, ", "okay, ", "please ", "pls "];
const CONTINUE_REPLIES: &[&str] = &[
    "continue",
    "keep going",
    "keep working",
    "keep pushing",
    "keep at it",
    "go on",
    "carry on",
    "proceed",
    "resume",
    "finish it",
    "finish what you started",
    "finish up",
    "finish the rest",
    "finish the remaining",
    "next",
    "onward",
    "let's keep going",
    "lets keep going",
    "let's continue",
    "lets continue",
    "do the next step",
    "do the remaining",
    "let's do the next",
    "lets do the next",
    "work on the next",
    "work on the remaining",
    "you stopped",
    "don't stop",
    "dont stop",
];
const APPROVE_REPLIES: &[&str] = &[
    "yes",
    "yep",
    "yeah",
    "y",
    "sure",
    "approve",
    "approved",
    "go ahead",
    "do it",
    "sounds good",
    "lgtm",
    "ship it",
    "confirm",
    "confirmed",
    "i approve",
    "i authorize",
    "i confirm",
    "i acknowledge",
    "authorized",
    "granted",
    "please do",
    "go for it",
    "agreed",
];
const HANDOFF_REPLIES: &[&str] = &[
    "done",
    "ready",
    "finished",
    "completed",
    "it's done",
    "it's set",
    "signed in",
    "logged in",
    "i signed",
    "i logged",
    "i just signed",
    "i just logged",
    "i added",
    "i set",
    "i created",
    "i installed",
    "i ran",
    "i enabled",
    "i updated",
    "i clicked",
    "i granted",
    "i restarted",
    "i connected",
    "i configured",
];
const DELIVER_REPLIES: &[&str] = &[
    "push it",
    "push them",
    "push this",
    "open a pr",
    "open prs",
    "open the pr",
    "merge it",
    "merge them",
    "merge this",
    "merge the pr",
    "get it merged",
    "get it deployed",
    "get it shipped",
    "get it live",
    "deploy it",
    "deploy this",
    "commit and push",
    "land it",
    "land this",
];
const STATUS_REPLIES: &[&str] = &[
    "are you done",
    "are you finished",
    "are you still",
    "did you finish",
    "did you ship",
    "did you merge",
    "did you push",
    "did you deploy",
    "is it done",
    "is this done",
    "is it merged",
    "is it live",
    "what's the status",
    "whats the status",
    "status",
    "where are we",
    "any update",
    "any progress",
    "how's it going",
];
const CORRECTION_REPLIES: &[&str] = &[
    "no",
    "nope",
    "nah",
    "wait",
    "stop",
    "don't",
    "dont",
    "actually",
    "hmm",
    "that's wrong",
    "that's not",
    "not this",
    "not that",
    "not what",
    "wrong",
    "why did",
    "why are",
    "why is",
    "why not",
    "you didn't",
    "you forgot",
    "you missed",
];
const QUOTA_REPLIES: &[&str] = &[
    "limits are back",
    "limits are gone",
    "limits reset",
    "limit reset",
    "usage is back",
    "usage reset",
    "quota reset",
    "credits are back",
    "credits restored",
];

fn starts_with_phrase(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| {
        text.strip_prefix(phrase)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '\''))
    })
}

/// Categorizes an operator reply from its opening words.
pub fn categorize_reply(text: &str) -> Reply {
    let lower: String = normalize(text.trim()).chars().take(300).collect();
    if any(&lower, QUOTA_REPLIES) {
        return Reply::QuotaResume;
    }
    let mut head = lower.trim_start_matches(|c: char| !c.is_alphanumeric());
    while let Some(rest) = REPLY_PREFIXES
        .iter()
        .find_map(|prefix| head.strip_prefix(prefix))
    {
        head = rest.trim_start();
    }
    if starts_with_phrase(head, CONTINUE_REPLIES) || head.trim_end_matches(['.', '!']) == "go" {
        Reply::Continue
    } else if starts_with_phrase(head, APPROVE_REPLIES) {
        Reply::Approve
    } else if starts_with_phrase(head, HANDOFF_REPLIES) {
        Reply::HandoffDone
    } else if any(&lower, DELIVER_REPLIES) {
        Reply::Deliver
    } else if starts_with_phrase(head, STATUS_REPLIES) {
        Reply::Status
    } else if starts_with_phrase(head, CORRECTION_REPLIES) {
        Reply::Correction
    } else {
        Reply::Other
    }
}

impl Reply {
    /// Settle labels this reply implies for the turn it answers, as
    /// `(head, label, weight)`. A continue or approval is direct evidence
    /// (weight 1). The other head reads a reply as a weaker negative (0.5).
    /// Status questions and quota notices say nothing about the turn.
    pub fn settle_labels(self) -> &'static [(&'static str, bool, f64)] {
        match self {
            Self::Continue | Self::Deliver => {
                &[(SETTLE_UNFINISHED, true, 1.0), (SETTLE_CONFIRM, false, 0.5)]
            }
            Self::Approve => &[(SETTLE_CONFIRM, true, 1.0), (SETTLE_UNFINISHED, false, 0.5)],
            Self::HandoffDone | Self::Correction | Self::Other => &[
                (SETTLE_UNFINISHED, false, 0.5),
                (SETTLE_CONFIRM, false, 0.5),
            ],
            Self::Status | Self::QuotaResume => &[],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Deliver => "deliver",
            Self::Approve => "approve",
            Self::HandoffDone => "handoff_done",
            Self::Status => "status",
            Self::Correction => "correction",
            Self::QuotaResume => "quota_resume",
            Self::Other => "other",
        }
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
        let short = settle_features("Updated the parser. Next, I'll wire the CLI:", &facts, 3);
        assert_eq!(short["promise_next"], 1.0);
        assert_eq!(short["ends_colon"], 1.0);
        assert_eq!(short["future_self"], 1.0);
        assert_eq!(short["no_tools"], 0.0);
        assert!((short["tools"] - 4f64.ln() / 129f64.ln()).abs() < 1e-12);
        let done = settle_features("All tests pass and the PR is merged.", &facts, 0);
        assert_eq!(done["done_claim"], 1.0);
        assert_eq!(done["promise_next"], 0.0);
        assert_eq!(done["no_tools"], 1.0);
        // Word boundaries: "abandoned" is not a done claim.
        let abandoned = settle_features("the branch was abandoned.", &facts, 0);
        assert_eq!(abandoned["done_claim"], 0.0);
        // Typographic apostrophes are the same cue.
        let curly = settle_features("Tests are green. I’m waiting on CI", &facts, 12);
        assert_eq!(curly["awaiting"], 1.0);
        assert_eq!(curly["progressive"], 1.0);
        assert!(settle_features("I am waiting.", &facts, 0)["progressive"] == 1.0);
        assert!(settle_features("Nothing is pending.", &facts, 0)["progressive"] == 0.0);
        let ask = settle_features(
            "## Summary\n\nThe migration is staged.\n\nShould I run it against production data?",
            &facts,
            20,
        );
        assert_eq!(ask["ask"], 1.0);
        assert_eq!(ask["risk"], 1.0);
        assert_eq!(ask["question"], 1.0);
        assert_eq!(ask["ends_question"], 1.0);
        assert_eq!(ask["report"], 1.0);
        let handoff = settle_features(
            "You'll need to sign in with the device code, then reply here.",
            &facts,
            2,
        );
        assert_eq!(handoff["user_act"], 1.0);
        assert!(settle_features("x", &facts, 0).len() <= MAX_FEATURES);
    }

    #[test]
    fn settle_priors_separate_stopped_short_confirm_and_done() {
        let facts = facts(Terminal::Completed, EffectState::Settled);
        let params = prior(Reflex::Settle);
        let unfinished = params.head(SETTLE_UNFINISHED).unwrap();
        let confirm = params.head(SETTLE_CONFIRM).unwrap();
        let working = settle_features(
            "Pushed the branch; I'm waiting on CI and will merge on green",
            &facts,
            60,
        );
        assert!(
            unfinished.decide(&working),
            "{} {working:?}",
            unfinished.probability(&working)
        );
        assert!(!confirm.decide(&working));
        let asks = settle_features(
            "The fix is ready on a branch. Should I open the PR and merge it?",
            &facts,
            8,
        );
        assert!(confirm.decide(&asks), "{}", confirm.probability(&asks));
        assert!(!unfinished.decide(&asks));
        let report = settle_features(
            "## Summary\n\n- Fixed the parser\n- Added tests\n\nAll tests pass and the PR is merged. Let me know if you want anything else.",
            &facts,
            0,
        );
        assert!(
            !unfinished.decide(&report),
            "{}",
            unfinished.probability(&report)
        );
        assert!(!confirm.decide(&report), "{}", confirm.probability(&report));
    }

    #[test]
    fn confirm_veto_reads_the_whole_report_and_inflections() {
        assert!(!confirm_vetoed(
            "The fix is ready on the branch. Should I open the PR and merge it?"
        ));
        for risky in [
            "Should I go ahead with deleting the old rows?",
            "Want me to rotate the tokens?",
            "Shall I deploy it?",
            "Ready to publish the package. Proceed?",
            "Should I email the list?",
        ] {
            assert!(confirm_vetoed(risky), "{risky}");
        }
        let hidden = format!(
            "Plan: drop legacy_users.\n\n{}\n\nShall I proceed?",
            "detail ".repeat(80)
        );
        assert!(confirm_vetoed(&hidden));
        for risky in [
            "Should I push this to prod?",
            "Ship it live?",
            "Tag the release?",
            "Should I rm -rf the build dir?",
            "Should I go ahead with wiping the cache?",
            "The config was overwritten. Restore it?",
            "I spent the remaining credits. Continue?",
        ] {
            assert!(confirm_vetoed(risky), "{risky}");
        }
        // Stems match at word starts with ordinary endings only, and broad
        // stems only in the asking paragraph.
        for safe in [
            "Should I update the undeployable flag docs?",
            "Fixed the dropdown payload and the tokenizer. Should I open the PR?",
            "Removed the dead helper and updated the public API.\n\nShould I open the PR and merge it?",
        ] {
            assert!(!confirm_vetoed(safe), "{safe}");
        }
        // A question followed by its options asks in the question's
        // paragraph, which is read as the ask too.
        assert!(confirm_vetoed(
            "Ready. Should I deploy it now?\n\n- yes\n- wait for review"
        ));
        assert!(!confirm_vetoed(
            "Ready. Should I open the PR?\n\n- yes\n- wait for review"
        ));
    }

    fn stopped_short(id: usize, label: bool) -> Example {
        example(
            id,
            settle_features(
                "Parser updated. Next, I'll wire the CLI:",
                &facts(Terminal::Completed, EffectState::None),
                60,
            ),
            label,
        )
    }

    #[test]
    fn certification_needs_confident_operator_precision() {
        let prior = prior(Reflex::Settle);
        let head = &prior.heads[SETTLE_UNFINISHED];
        assert!(head.probability(&stopped_short(0, true).features) >= head.threshold);
        let precise: Vec<_> = (0..400).map(|i| stopped_short(i, i % 10 != 0)).collect();
        let everyone = vec![true; precise.len()];
        let options = FitOptions::default();
        let certified =
            certify(SETTLE_UNFINISHED, head, &precise, &everyone, None, options).unwrap();
        assert!(certified.certified, "{certified:?}");
        assert!(certified.lower.unwrap() >= 0.75);
        assert_eq!(certified.threshold, head.threshold);
        // Too little evidence, or evidence xcb produced by acting, certifies
        // nothing.
        let few = certify(
            SETTLE_UNFINISHED,
            head,
            &precise[..20],
            &everyone[..20],
            None,
            options,
        )
        .unwrap();
        assert!(
            !few.certified && few.reason.starts_with("collecting"),
            "{few:?}"
        );
        let machine = vec![false; precise.len()];
        let unearned = certify(SETTLE_UNFINISHED, head, &precise, &machine, None, options).unwrap();
        assert!(!unearned.certified);
        assert_eq!(unearned.window, 0);
        // Precision just under the floor never certifies a new head, but a
        // certified one keeps its certificate within the margin and loses
        // it below.
        let sagging: Vec<_> = (0..1000).map(|i| stopped_short(i, i % 50 < 37)).collect();
        let all = vec![true; sagging.len()];
        let fresh = certify(SETTLE_UNFINISHED, head, &sagging, &all, None, options).unwrap();
        assert!(!fresh.certified, "{fresh:?}");
        let kept = certify(
            SETTLE_UNFINISHED,
            head,
            &sagging,
            &all,
            Some(&certified),
            options,
        )
        .unwrap();
        assert!(kept.certified, "{kept:?}");
        let poor: Vec<_> = (0..1000).map(|i| stopped_short(i, i % 2 == 0)).collect();
        let lost = certify(
            SETTLE_UNFINISHED,
            head,
            &poor,
            &all,
            Some(&certified),
            options,
        )
        .unwrap();
        assert!(!lost.certified, "{lost:?}");
        // Deterministic.
        assert_eq!(
            certified,
            certify(SETTLE_UNFINISHED, head, &precise, &everyone, None, options).unwrap()
        );
        // Heads that never act cannot be certified.
        assert!(certify(ROUTE_PLAIN, head, &precise, &everyone, None, options).is_err());
    }

    #[test]
    fn confirm_is_certified_only_on_requests_it_could_answer() {
        let prior = prior(Reflex::Settle);
        let head = &prior.heads[SETTLE_CONFIRM];
        let facts = facts(Terminal::Completed, EffectState::None);
        let safe = settle_features("The fix is ready. Should I open the PR?", &facts, 12);
        let risky = settle_features("Should I drop the production tables?", &facts, 12);
        assert!(actionable(SETTLE_CONFIRM, &safe));
        assert!(!actionable(SETTLE_CONFIRM, &risky));
        assert!(actionable(SETTLE_UNFINISHED, &risky));
        // Only risky requests, all approved: nothing the head could answer
        // was scored.
        let examples: Vec<_> = (0..400).map(|i| example(i, risky.clone(), true)).collect();
        let counted = vec![true; examples.len()];
        let certificate = certify(
            SETTLE_CONFIRM,
            head,
            &examples,
            &counted,
            None,
            FitOptions::default(),
        )
        .unwrap();
        assert_eq!((certificate.window, certificate.certified), (0, false));
        assert!(operator_label("user_continue") && operator_label("import"));
        assert!(
            !operator_label("continuation_outcome") && !operator_label("cancelled_continuation")
        );
    }

    #[test]
    fn replies_categorize_by_their_opening_words() {
        for (text, reply) in [
            ("continue", Reply::Continue),
            ("ok keep going", Reply::Continue),
            ("Please continue where you left off.", Reply::Continue),
            ("go", Reply::Continue),
            ("yes", Reply::Approve),
            ("Yes please, go ahead", Reply::Approve),
            ("ok, do it", Reply::Approve),
            ("done", Reply::HandoffDone),
            ("I just signed in", Reply::HandoffDone),
            ("looks right, push it and open a PR", Reply::Deliver),
            ("are you done?", Reply::Status),
            ("no, use the other table", Reply::Correction),
            ("wait", Reply::Correction),
            ("limits are back, keep going", Reply::QuotaResume),
            ("add a dark mode toggle", Reply::Other),
            ("yesterday's build failed", Reply::Other),
            ("gone fishing", Reply::Other),
            ("nobody reviewed it", Reply::Other),
        ] {
            assert_eq!(categorize_reply(text), reply, "{text}");
        }
        assert_eq!(
            Reply::Approve.settle_labels()[0],
            (SETTLE_CONFIRM, true, 1.0)
        );
        assert!(Reply::Status.settle_labels().is_empty());
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
    fn promotion_requires_a_won_forward_trial() {
        let current = prior(Reflex::Route).heads[ROUTE_PLAIN].clone();
        let examples: Vec<_> = (0..120)
            .map(|i| {
                let imperative = i % 2 == 0;
                let task = if imperative { "build it" } else { "what is it" };
                example(i, route_features(task, false, false), imperative)
            })
            .collect();
        let candidate = fit(&current, &examples[..60], FitOptions::default()).unwrap();
        let early = compare(&current, &candidate, &examples[60..70]);
        assert!(!early.promoted && !trial_complete(&early), "{early:?}");
        let verdict = compare(&current, &candidate, &examples[60..]);
        assert!(verdict.promoted && trial_complete(&verdict), "{verdict:?}");
        // The plain prior never fires without a complexity cue.
        assert_eq!(verdict.current.precision, None);
        assert_eq!(verdict.current.recall, Some(0.0));
        // The current head never loses to itself.
        assert!(!compare(&candidate, &candidate, &examples[60..]).promoted);
    }

    #[test]
    fn replay_learns_forward_and_is_deterministic() {
        let prior = prior(Reflex::Route).heads[ROUTE_PLAIN].clone();
        let examples: Vec<_> = (0..400)
            .map(|i| {
                let imperative = i % 3 != 0;
                let task = if imperative { "build it" } else { "what is it" };
                example(i, route_features(task, false, false), imperative)
            })
            .collect();
        let replayed = replay(&prior, &prior, &examples, FitOptions::default()).unwrap();
        assert!(replayed.promotions >= 1, "{replayed:?}");
        assert!(replayed.trials >= replayed.promotions);
        assert!(
            replayed
                .head
                .decide(&route_features("build it", false, false))
        );
        assert!(replayed.evidence.as_ref().is_some_and(|e| e.promoted));
        assert_eq!(replayed.prequential.n, 400);
        assert_eq!(
            replayed,
            replay(&prior, &prior, &examples, FitOptions::default()).unwrap()
        );
        // Evidence that agrees with the head never earns a promotion.
        let agreeing: Vec<_> = (0..200)
            .map(|i| {
                let complex = i % 2 == 0;
                example(i, route_features("fix it", complex, false), complex)
            })
            .collect();
        let steady = replay(&prior, &prior, &agreeing, FitOptions::default()).unwrap();
        assert!(steady.head.decide(&route_features("fix it", true, false)));
        assert!(!steady.head.decide(&route_features("fix it", false, false)));
    }

    #[test]
    fn params_reject_foreign_heads_and_unbounded_weights() {
        let mut params = prior(Reflex::Settle);
        params
            .heads
            .insert("extra".into(), params.heads[SETTLE_UNFINISHED].clone());
        assert!(params.validate().is_err());
        let mut params = prior(Reflex::Settle);
        params.heads.remove(SETTLE_CONFIRM);
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
