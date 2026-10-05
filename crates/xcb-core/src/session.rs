use crate::{
    Error, Id, MAX_TEXT_BYTES, Result, bounded_text, label,
    models::ModelChoice,
    policy::{Failure, Terminal, TurnFacts},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Working,
    NeedsAnswer,
    NeedsAction,
    NeedsApproval,
    Limited,
    Failed,
    Cancelled,
    Uncertain,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::NeedsAnswer => "needs answer",
            Self::NeedsAction => "needs action",
            Self::NeedsApproval => "needs approval",
            Self::Limited => "usage limit",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Uncertain => "needs recovery",
        }
    }
    pub fn attention(self) -> bool {
        matches!(
            self,
            Self::NeedsAnswer | Self::NeedsAction | Self::NeedsApproval | Self::Uncertain
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Thinking,
    Tool,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attachment {
    pub digest: String,
    pub media_type: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
}
impl Attachment {
    pub fn validate(&self) -> Result<()> {
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || !matches!(
                self.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/webp"
            )
            || self.bytes == 0
            || self.bytes > 10 * 1024 * 1024
            || self.width == 0
            || self.height == 0
            || self.width > 8192
            || self.height > 8192
            || u64::from(self.width) * u64::from(self.height) > 32_000_000
        {
            return Err(Error::Invalid("image attachment"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageProvenance {
    pub account: Id,
    pub model: ModelChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Id>,
}
impl MessageProvenance {
    pub fn boundary_label(&self, previous: Option<&Self>) -> String {
        let account_changed = previous.map(|p| p.account != self.account).unwrap_or(false);
        let model_changed = previous
            .map(|p| p.model.key() != self.model.key())
            .unwrap_or(true);
        let run_changed = previous.map(|p| p.run != self.run).unwrap_or(true);
        if !account_changed && !model_changed && !run_changed {
            return String::new();
        }
        let mut parts = Vec::new();
        if account_changed {
            parts.push("↷".to_string());
        }
        if model_changed {
            parts.push(self.model.key());
        }
        if run_changed {
            if let Some(run) = &self.run {
                parts.push(run.as_str().to_string());
            } else if !account_changed && !model_changed {
                parts.push("↷".to_string());
            }
        }
        parts.join("/")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: Id,
    pub role: Role,
    pub text: String,
    pub at_ms: u64,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<MessageProvenance>,
}
impl Message {
    pub fn validate(&self) -> Result<()> {
        bounded_text(&self.text, MAX_TEXT_BYTES)?;
        if self.attachments.len() > 8 {
            return Err(Error::Limit("attachments"));
        }
        for image in &self.attachments {
            image.validate()?;
        }
        if let Some(provenance) = &self.provenance {
            provenance.model.validate()?;
        }
        Ok(())
    }
}

/// Explicit user route restrictions; automatic choices are not pins.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePins {
    pub provider: Option<crate::Provider>,
    pub account: Option<Id>,
    pub model: Option<String>,
}
impl RoutePins {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.account.is_none()
    }
}

/// Hard execution capabilities; once required they survive turns and reroutes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRequirements {
    #[serde(default)]
    pub signed_in_browser: bool,
    /// Operating native desktop applications, beyond browser-page controls.
    #[serde(default)]
    pub desktop: bool,
    /// A native Codex tool was used; retain that provider without inferring
    /// whether the operation needed a signed-in page or a desktop application.
    #[serde(default)]
    pub codex_native: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub native_execution: bool,
}
impl TaskRequirements {
    pub fn is_empty(&self) -> bool {
        !self.requires_codex() && !self.native_execution
    }
    pub fn requires_codex(self) -> bool {
        self.signed_in_browser || self.desktop || self.codex_native
    }
    pub fn merge(self, other: Self) -> Self {
        Self {
            signed_in_browser: self.signed_in_browser || other.signed_in_browser,
            desktop: self.desktop || other.desktop,
            codex_native: self.codex_native || other.codex_native,
            native_execution: self.native_execution || other.native_execution,
        }
    }
    pub fn allows(self, provider: crate::Provider) -> bool {
        !self.requires_codex() || provider == crate::Provider::Codex
    }
}

#[cfg(test)]
mod task_requirement_tests {
    use super::*;

    #[test]
    fn native_execution_survives_merge_and_round_trip_without_pinning_codex() {
        let native: TaskRequirements =
            serde_json::from_str(r#"{"native_execution":true}"#).unwrap();
        assert!(!native.is_empty());
        assert!(!native.requires_codex());
        for provider in crate::Provider::ALL {
            assert!(native.allows(provider));
        }
        let merged = native.merge(Default::default());
        let saved = serde_json::to_value(merged).unwrap();
        assert_eq!(saved["native_execution"], true);
        let restored: TaskRequirements = serde_json::from_value(saved).unwrap();
        assert_eq!(restored, native);
    }

    #[test]
    fn legacy_execution_records_do_not_gain_a_native_execution_field() {
        let legacy: TaskRequirements = serde_json::from_str(
            r#"{"signed_in_browser":true,"desktop":false,"codex_native":false}"#,
        )
        .unwrap();
        assert!(!legacy.native_execution);
        assert!(
            serde_json::to_value(legacy)
                .unwrap()
                .get("native_execution")
                .is_none()
        );
    }

    #[test]
    fn requirement_merge_is_monotonic_for_every_execution_and_provider_combination() {
        let requirements = |bits: u8| TaskRequirements {
            signed_in_browser: bits & 1 != 0,
            desktop: bits & 2 != 0,
            codex_native: bits & 4 != 0,
            native_execution: bits & 8 != 0,
        };
        for left in 0..16 {
            for right in 0..16 {
                let merged = requirements(left).merge(requirements(right));
                assert_eq!(merged, requirements(left | right));
                let restored: TaskRequirements =
                    serde_json::from_value(serde_json::to_value(merged).unwrap()).unwrap();
                assert_eq!(merged, restored);
                for provider in crate::Provider::ALL {
                    if !requirements(left).allows(provider) {
                        assert!(!merged.allows(provider));
                    }
                }
            }
        }
    }

    #[test]
    fn native_execution_preserves_independent_desktop_constraints() {
        let native: TaskRequirements =
            serde_json::from_str(r#"{"native_execution":true}"#).unwrap();
        let desktop = TaskRequirements {
            desktop: true,
            ..Default::default()
        };
        let merged = native.merge(desktop);
        assert_eq!(
            serde_json::to_value(merged).unwrap()["native_execution"],
            true
        );
        assert!(merged.allows(crate::Provider::Codex));
        assert!(!merged.allows(crate::Provider::Claude));
        assert!(!merged.allows(crate::Provider::Devin));
    }

    #[test]
    fn legacy_browser_requirements_merge_without_inventing_desktop_intent() {
        let old: TaskRequirements = serde_json::from_str(r#"{"signed_in_browser":true}"#).unwrap();
        assert!(old.signed_in_browser && !old.desktop && !old.codex_native);
        let native = TaskRequirements {
            codex_native: true,
            ..Default::default()
        };
        let merged = old.merge(native).merge(Default::default());
        let saved: TaskRequirements =
            serde_json::from_value(serde_json::to_value(merged).unwrap()).unwrap();
        assert!(saved.signed_in_browser && saved.codex_native && !saved.desktop);
        assert!(saved.allows(crate::Provider::Codex));
        assert!(!saved.allows(crate::Provider::Claude));
        assert!(!saved.allows(crate::Provider::Devin));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    #[serde(default, skip_serializing_if = "RoutePins::is_empty")]
    pub route_pins: RoutePins,
    #[serde(default, skip_serializing_if = "TaskRequirements::is_empty")]
    pub requirements: TaskRequirements,
    pub id: Id,
    pub account: Id,
    pub model: ModelChoice,
    pub workspace: String,
    pub title: String,
    pub pane: Id,
    pub state: State,
    /// The managed task this session was created for, recorded atomically at
    /// creation so startup reconciliation can prove managed custody of a
    /// session that never reached `prepare` (an orphan). `None` means the
    /// session is unmanaged/direct and must never be swept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_task: Option<Id>,
    pub revision: u64,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
}
impl Session {
    pub fn validate(&self) -> Result<()> {
        self.model.validate()?;
        bounded_text(&self.workspace, 4096)?;
        label(&self.title, 160)?;
        if self.workspace.is_empty() || self.last_active_at_ms < self.created_at_ms {
            return Err(Error::Invalid("session"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subagent {
    pub id: Id,
    pub label: String,
    pub state: State,
    pub model: Option<String>,
}

pub fn classify(text: &str, facts: &TurnFacts) -> State {
    if !facts.joined || facts.effects == crate::policy::EffectState::Uncertain {
        return State::Uncertain;
    }
    if facts.terminal == Terminal::Cancelled {
        return State::Cancelled;
    }
    if facts.failure == Some(Failure::Authentication) {
        return State::NeedsAction;
    }
    if matches!(
        facts.failure,
        Some(Failure::AccountQuota | Failure::ModelQuota)
    ) {
        return State::Limited;
    }
    if facts.pending_attention {
        return State::NeedsApproval;
    }
    if facts.terminal == Terminal::Failed {
        return State::Failed;
    }
    let lower = text
        .chars()
        .rev()
        .take(1200)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    if [
        "please sign in",
        "please log in",
        "verification code",
        "scan the qr",
        "paste your",
        "attach the",
        "run this manually",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
    {
        return State::NeedsAction;
    }
    if [
        "please approve",
        "please confirm",
        "do you approve",
        "your approval",
        "your consent",
    ]
    .iter()
    .any(|cue| lower.contains(cue))
    {
        return State::NeedsApproval;
    }
    if lower.trim_end().ends_with('?')
        || [
            "question for you",
            "need your input",
            "which do you prefer",
            "what would you like",
            "should i keep",
            "should i proceed",
            "would you like me",
        ]
        .iter()
        .any(|cue| lower.contains(cue))
    {
        return State::NeedsAnswer;
    }
    State::Idle
}
