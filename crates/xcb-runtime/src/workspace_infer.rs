//! Pure workspace inference for the global thread: which project directory a
//! prompt's task runs in, and why. No I/O and no clock; the store supplies a
//! snapshot of known workspaces and the caller's cues.
use serde::{Deserialize, Serialize};

/// How long a doubtful TUI binding waits before its first dispatch.
pub const WORKSPACE_HOLD_MS: u64 = 8_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingSource {
    Explicit,
    Target,
    Mention,
    Focus,
    Continuation,
    Launch,
    Recent,
    Moved,
    Inherited,
}
impl BindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Target => "target",
            Self::Mention => "mention",
            Self::Focus => "focus",
            Self::Continuation => "continuation",
            Self::Launch => "launch",
            Self::Recent => "recent",
            Self::Moved => "moved",
            Self::Inherited => "inherited",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingConfidence {
    High,
    Medium,
    Low,
}
impl BindingConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

/// Who created the task. Persisted so relay-only continuation and the
/// user-origin restriction on moves stay checkable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingOrigin {
    Tui,
    Cli,
    Relay,
    Worker,
    Program,
    Daemon,
    Schedule,
}
impl BindingOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Cli => "cli",
            Self::Relay => "relay",
            Self::Worker => "worker",
            Self::Program => "program",
            Self::Daemon => "daemon",
            Self::Schedule => "schedule",
        }
    }
}

/// Why a thread task runs in its workspace. Stored in the task payload and
/// its receipt; `None` on a task means its project view bound it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub source: BindingSource,
    pub confidence: BindingConfidence,
    pub origin: BindingOrigin,
    /// Short label, at most 160 bytes.
    pub reason: String,
    /// At most 4 canonical paths the resolver also considered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
}
impl WorkspaceBinding {
    pub(crate) fn validate(&self) -> crate::Result<()> {
        xcb_core::bounded_text(&self.reason, 160)?;
        if self.alternatives.len() > 4
            || self
                .alternatives
                .iter()
                .any(|path| path.len() > 4096 || !std::path::Path::new(path).is_absolute())
        {
            return Err(xcb_core::Error::Invalid("workspace binding").into());
        }
        Ok(())
    }
}

/// One entry of the known-workspace registry, as read at intake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownWorkspace {
    pub path: String,
    pub name: String,
    pub repo: Option<String>,
    pub last_used_ms: u64,
    /// Holds other known projects and is not a repository; never inferred.
    pub container: bool,
    /// Admitted by an explicit `xcb workspaces add` or `/workspace add`.
    pub explicit_add: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Cues<'a> {
    /// Relay/CLI/schedule/daemon/program/view workspace: validated, never snapped.
    pub explicit: Option<&'a str>,
    /// Workspace of the task the prompt addresses: validated, never snapped.
    pub target: Option<&'a str>,
    /// TUI session focus.
    pub focus: Option<&'a str>,
    /// The launch directory's snapped root, when admitted and not a container.
    pub launch_hint: Option<&'a str>,
    /// Workspace and `updated_at` of the last thread task (relay-origin only
    /// for `infer_only`).
    pub last_thread_task: Option<(&'a str, u64)>,
    /// Snapped prompt roots, or why a token was rejected.
    pub prompt_roots: Vec<Result<String, String>>,
    /// Offer unregistered roots in `Ask` (TUI and CLI); never for relay.
    pub allow_admit: bool,
    /// Allow the launch and recent rungs; false for relay `@infer`.
    pub allow_guess: bool,
    /// Relay `@infer`.
    pub infer_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Bound {
        workspace: String,
        binding: WorkspaceBinding,
        hold: bool,
    },
    Ask {
        candidates: Vec<String>,
        new_roots: Vec<String>,
        reason: String,
    },
}

/// Absolute and `~/` path tokens named by a prompt. The foundation stub
/// names none; the intake ladder replaces it.
pub fn path_tokens(_text: &str) -> Vec<String> {
    Vec::new()
}

/// Resolve the workspace for one prompt. This foundation stub implements
/// only explicit > target > focus > launch hint, then asks; the intake
/// ladder replaces it. `binding.origin` is a placeholder the store
/// overwrites from the intake cues.
pub fn resolve(_text: &str, cues: &Cues, known: &[KnownWorkspace], _now_ms: u64) -> Resolution {
    let bound = |workspace: &str, source, confidence, reason: &str| Resolution::Bound {
        workspace: workspace.to_owned(),
        binding: WorkspaceBinding {
            source,
            confidence,
            origin: BindingOrigin::Tui,
            reason: reason.to_owned(),
            alternatives: vec![],
        },
        hold: false,
    };
    // A container is usable as a focus or target only when a human added it.
    let usable = |path: &str| {
        known
            .iter()
            .find(|entry| entry.path == path)
            .is_none_or(|entry| !entry.container || entry.explicit_add)
    };
    if let Some(workspace) = cues.explicit {
        return bound(
            workspace,
            BindingSource::Explicit,
            BindingConfidence::High,
            "named directory",
        );
    }
    if let Some(workspace) = cues.target.filter(|path| usable(path)) {
        return bound(
            workspace,
            BindingSource::Target,
            BindingConfidence::High,
            "addressed task",
        );
    }
    if let Some(workspace) = cues.focus.filter(|path| usable(path)) {
        return bound(
            workspace,
            BindingSource::Focus,
            BindingConfidence::High,
            "focus",
        );
    }
    if cues.allow_guess
        && let Some(workspace) = cues.launch_hint
    {
        return bound(
            workspace,
            BindingSource::Launch,
            BindingConfidence::Medium,
            "launch directory",
        );
    }
    let mut candidates: Vec<_> = known
        .iter()
        .filter(|entry| !entry.container)
        .map(|entry| entry.path.clone())
        .collect();
    candidates.sort();
    candidates.truncate(8);
    Resolution::Ask {
        candidates,
        new_roots: vec![],
        reason: "which project?".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_ladder_prefers_explicit_then_target_focus_and_hint() {
        let known = vec![KnownWorkspace {
            path: "/c".into(),
            name: "c".into(),
            repo: None,
            last_used_ms: 1,
            container: true,
            explicit_add: false,
        }];
        let mut cues = Cues {
            explicit: Some("/a"),
            target: Some("/b"),
            allow_guess: true,
            ..Cues::default()
        };
        let workspace = |resolution: Resolution| match resolution {
            Resolution::Bound { workspace, .. } => Some(workspace),
            Resolution::Ask { .. } => None,
        };
        assert_eq!(
            workspace(resolve("x", &cues, &known, 0)).as_deref(),
            Some("/a")
        );
        cues.explicit = None;
        assert_eq!(
            workspace(resolve("x", &cues, &known, 0)).as_deref(),
            Some("/b")
        );
        cues.target = Some("/c");
        cues.focus = Some("/d");
        assert_eq!(
            workspace(resolve("x", &cues, &known, 0)).as_deref(),
            Some("/d")
        );
        cues.focus = None;
        cues.launch_hint = Some("/e");
        assert_eq!(
            workspace(resolve("x", &cues, &known, 0)).as_deref(),
            Some("/e")
        );
        cues.allow_guess = false;
        assert!(matches!(
            resolve("x", &cues, &known, 0),
            Resolution::Ask { candidates, .. } if candidates.is_empty()
        ));
    }

    #[test]
    fn binding_is_closed_and_bounded() {
        let binding: WorkspaceBinding = serde_json::from_value(serde_json::json!({
            "source": "explicit", "confidence": "high", "origin": "relay", "reason": "named directory"
        }))
        .unwrap();
        assert!(binding.validate().is_ok());
        assert!(
            serde_json::from_value::<WorkspaceBinding>(serde_json::json!({
                "source": "explicit", "confidence": "high", "reason": "x"
            }))
            .is_err(),
            "origin is required"
        );
        let mut wide = binding.clone();
        wide.alternatives = vec!["/a".into(); 5];
        assert!(wide.validate().is_err());
        wide.alternatives = vec!["relative".into()];
        assert!(wide.validate().is_err());
        wide.alternatives.clear();
        wide.reason = "x".repeat(161);
        assert!(wide.validate().is_err());
    }
}
