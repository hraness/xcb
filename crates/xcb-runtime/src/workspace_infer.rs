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

/// A thread task updated within this window can be continued by a follow-up.
pub const CONTINUE_WINDOW_MS: u64 = 6 * 60 * 60 * 1000;
/// The recent rung offers registry entries used within this window.
const RECENT_WINDOW_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const MAX_PATH_TOKENS: usize = 8;
const MAX_CANDIDATES: usize = 8;
const MAX_ALTERNATIVES: usize = 4;
const MAX_REASON_BYTES: usize = 160;
/// A prompt this short that names no project reads as a follow-up.
const SHORT_PROMPT_WORDS: usize = 12;
const MIN_NAME_CHARS: usize = 3;
/// Names too common to mean a project when they appear in prose.
const STOPLIST: [&str; 11] = [
    "site", "docs", "app", "web", "api", "cli", "core", "test", "main", "src", "lib",
];

/// Two workspaces overlap when either contains the other; their tasks
/// serialize. The flock itself stays per exact path.
pub(crate) fn workspaces_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (std::path::Path::new(a), std::path::Path::new(b));
    a.starts_with(b) || b.starts_with(a)
}

/// A short message whose whole intent is "keep going".
pub(crate) fn continue_like(text: &str) -> bool {
    let lower = text
        .trim()
        .trim_end_matches(['.', '!'])
        .trim()
        .to_ascii_lowercase();
    let lower = lower.strip_prefix("please ").unwrap_or(&lower);
    let lower = lower.strip_suffix(" please").unwrap_or(lower);
    matches!(
        lower,
        "continue"
            | "keep going"
            | "go on"
            | "go ahead"
            | "proceed"
            | "carry on"
            | "finish it"
            | "finish"
            | "keep at it"
            | "don't stop"
            | "dont stop"
            | "you stopped"
            | "you stopped early"
            | "continue where you left off"
            | "continue the work"
            | "resume"
            | "next"
    )
}

const OPENERS: [char; 6] = ['"', '\'', '`', '(', '[', '<'];
const CLOSERS: [char; 12] = ['.', ',', ';', ':', '!', '?', ')', ']', '>', '"', '\'', '`'];

fn path_like(word: &str) -> bool {
    word == "~" || word.starts_with('/') || word.starts_with("~/")
}

/// Absolute and `~/` path tokens named by a prompt: a token that leads its
/// line, follows `in`, `at`, `cd` or `under`, or is written `@/path`.
/// Trailing punctuation is stripped; at most 8 distinct tokens. `~` stays
/// unexpanded: the store expands and snaps each token.
pub fn path_tokens(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut after_cue = false;
        for (index, raw) in line.split_whitespace().enumerate() {
            let word = raw.trim_start_matches(OPENERS);
            let (marked, word) = match word.strip_prefix('@') {
                Some(rest) => (true, rest),
                None => (false, word),
            };
            let cued = index == 0 || marked || after_cue;
            after_cue = matches!(
                raw.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_ascii_lowercase()
                    .as_str(),
                "in" | "at" | "cd" | "under"
            );
            let path = word.trim_end_matches(CLOSERS);
            if !cued || !path_like(path) || path.len() > 4096 {
                continue;
            }
            if !tokens.iter().any(|token| token == path) {
                tokens.push(path.to_owned());
                if tokens.len() == MAX_PATH_TOKENS {
                    return tokens;
                }
            }
        }
    }
    tokens
}

/// Whether `key` appears in `prose` as a whole word. Letters, digits, `-`,
/// `_` and an inner `.` join a word; a sentence-ending `.` does not.
fn mentions(prose: &str, key: &str) -> bool {
    let joins = |c: char| c.is_alphanumeric() || matches!(c, '-' | '_');
    let mut from = 0;
    while let Some(offset) = prose[from..].find(key) {
        let start = from + offset;
        let end = start + key.len();
        let before = prose[..start].chars().next_back();
        let mut after = prose[end..].chars();
        let next = after.next();
        let before_ok = before.is_none_or(|c| !joins(c) && c != '.' && c != '/');
        let after_ok = match next {
            None => true,
            Some('.') => after.next().is_none_or(|c| !joins(c)),
            Some(c) => !joins(c) && c != '/',
        };
        if before_ok && after_ok {
            return true;
        }
        from = start + prose[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// A reason label, clipped to its byte bound on a character boundary.
fn clip(mut reason: String) -> String {
    if reason.len() > MAX_REASON_BYTES {
        let mut end = MAX_REASON_BYTES;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
    }
    reason
}

struct Ladder<'a> {
    cues: &'a Cues<'a>,
    /// Sorted by path so registry row order never matters.
    known: Vec<&'a KnownWorkspace>,
    now_ms: u64,
}

impl<'a> Ladder<'a> {
    fn entry(&self, path: &str) -> Option<&'a KnownWorkspace> {
        self.known
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok()
            .map(|index| self.known[index])
    }

    fn name(&self, path: &str) -> String {
        match self.entry(path) {
            Some(entry) => entry.name.clone(),
            None => std::path::Path::new(path)
                .file_name()
                .map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into()),
        }
    }

    fn container(&self, path: &str) -> bool {
        self.entry(path).is_some_and(|entry| entry.container)
    }

    /// Focus and target may be a container only when a human added it.
    fn usable(&self, path: &str) -> bool {
        self.entry(path)
            .is_none_or(|entry| !entry.container || entry.explicit_add)
    }

    fn focus(&self) -> Option<&'a str> {
        self.cues.focus.filter(|path| self.usable(path))
    }

    fn bound(
        &self,
        workspace: &str,
        source: BindingSource,
        confidence: BindingConfidence,
        reason: String,
        alternatives: Vec<String>,
    ) -> Resolution {
        Resolution::Bound {
            workspace: workspace.to_owned(),
            binding: WorkspaceBinding {
                source,
                confidence,
                origin: BindingOrigin::Tui,
                reason: clip(reason),
                alternatives,
            },
            hold: confidence == BindingConfidence::Low,
        }
    }

    /// A prompt path or name mention beats the focus, but a binding that
    /// overrides it is at most medium, says so and is held.
    fn mention(
        &self,
        workspace: &str,
        confidence: BindingConfidence,
        reason: String,
        alternatives: Vec<String>,
    ) -> Resolution {
        let overridden = self.focus().filter(|focus| *focus != workspace);
        let (confidence, reason) = match overridden {
            Some(focus) => (
                if confidence == BindingConfidence::High {
                    BindingConfidence::Medium
                } else {
                    confidence
                },
                format!("{reason} (overrides focus {})", self.name(focus)),
            ),
            None => (confidence, reason),
        };
        let mut resolution = self.bound(
            workspace,
            BindingSource::Mention,
            confidence,
            reason,
            alternatives,
        );
        if overridden.is_some()
            && let Resolution::Bound { hold, .. } = &mut resolution
        {
            *hold = true;
        }
        resolution
    }

    fn ask(
        &self,
        mut candidates: Vec<String>,
        new_roots: Vec<String>,
        reason: String,
    ) -> Resolution {
        candidates.sort();
        candidates.dedup();
        candidates.truncate(MAX_CANDIDATES);
        Resolution::Ask {
            candidates,
            new_roots: if self.cues.allow_admit && !self.cues.infer_only {
                new_roots
            } else {
                vec![]
            },
            reason: clip(reason),
        }
    }

    /// Rung 3: exactly one registered root binds; anything else asks.
    fn prompt_path(&self) -> Option<Resolution> {
        let mut roots: Vec<&str> = Vec::new();
        for root in self.cues.prompt_roots.iter().flatten() {
            if !self.container(root) && !roots.contains(&root.as_str()) {
                roots.push(root);
            }
        }
        roots.sort_unstable();
        match roots.as_slice() {
            [] => None,
            [root] if self.entry(root).is_some() => Some(self.mention(
                root,
                BindingConfidence::High,
                format!("path in `{}`", self.name(root)),
                vec![],
            )),
            [root] => Some(self.ask(
                vec![],
                vec![(*root).to_owned()],
                format!("`{root}` is not a known project; add it to use it"),
            )),
            several => {
                let (known, new): (Vec<&str>, Vec<&str>) =
                    several.iter().partition(|root| self.entry(root).is_some());
                Some(self.ask(
                    known.into_iter().map(str::to_owned).collect(),
                    new.into_iter().map(str::to_owned).collect(),
                    "the prompt names several project directories".into(),
                ))
            }
        }
    }

    /// Every non-container entry the prompt names, and the word that named
    /// it. Path-like tokens are not prose.
    fn named(&self, text: &str) -> Vec<(&'a KnownWorkspace, String)> {
        let prose = text
            .split_whitespace()
            .filter(|raw| {
                let word = raw.trim_start_matches(OPENERS);
                !path_like(word.strip_prefix('@').unwrap_or(word))
            })
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        self.known
            .iter()
            .filter(|entry| !entry.container)
            .filter_map(|entry| {
                let tail = entry
                    .repo
                    .as_deref()
                    .and_then(|repo| repo.rsplit('/').next());
                [Some(entry.name.as_str()), tail]
                    .into_iter()
                    .flatten()
                    .map(str::to_lowercase)
                    .filter(|key| {
                        key.chars().count() >= MIN_NAME_CHARS
                            && !STOPLIST.contains(&key.as_str())
                            && mentions(&prose, key)
                    })
                    .min()
                    .map(|key| (*entry, key))
            })
            .collect()
    }

    /// Rung 4: one repository group binds; several ask.
    fn name_mention(&self, named: &[(&'a KnownWorkspace, String)]) -> Option<Resolution> {
        let group =
            |entry: &KnownWorkspace| entry.repo.clone().unwrap_or_else(|| entry.path.clone());
        let mut groups: Vec<String> = named.iter().map(|(entry, _)| group(entry)).collect();
        groups.sort();
        groups.dedup();
        let paths =
            || -> Vec<String> { named.iter().map(|(entry, _)| entry.path.clone()).collect() };
        match groups.len() {
            0 => None,
            1 => {
                let word = named
                    .iter()
                    .map(|(_, word)| word.as_str())
                    .min()
                    .unwrap_or("");
                let reason = format!("named `{word}`");
                if let [(only, _)] = named {
                    return Some(self.mention(&only.path, BindingConfidence::High, reason, vec![]));
                }
                if self.cues.infer_only {
                    return Some(self.ask(
                        paths(),
                        vec![],
                        format!("`{word}` names several checkouts"),
                    ));
                }
                let in_group = |path: Option<&'a str>| -> Option<&'a str> {
                    path.filter(|path| named.iter().any(|(entry, _)| entry.path == *path))
                };
                if let Some(pick) =
                    in_group(self.focus()).or_else(|| in_group(self.cues.launch_hint))
                {
                    return Some(self.mention(pick, BindingConfidence::Medium, reason, vec![]));
                }
                let recent = named.iter().map(|(entry, _)| *entry).max_by(|a, b| {
                    a.last_used_ms
                        .cmp(&b.last_used_ms)
                        .then_with(|| b.path.cmp(&a.path))
                })?;
                let alternatives = named
                    .iter()
                    .map(|(entry, _)| entry.path.clone())
                    .filter(|path| *path != recent.path)
                    .take(MAX_ALTERNATIVES)
                    .collect();
                Some(self.mention(
                    &recent.path,
                    BindingConfidence::Low,
                    format!("{reason}, most recent checkout"),
                    alternatives,
                ))
            }
            _ => Some(self.ask(paths(), vec![], "the prompt names several projects".into())),
        }
    }

    fn recent(&self, at_ms: u64, window: u64) -> bool {
        self.now_ms.saturating_sub(at_ms) < window
    }

    /// Rung 6: continue the last thread task's workspace on a cue.
    fn continuation(&self, text: &str, named_nothing: bool) -> Option<Resolution> {
        let (workspace, at_ms) = self.cues.last_thread_task?;
        if !self.recent(at_ms, CONTINUE_WINDOW_MS)
            || self.entry(workspace).is_none_or(|entry| entry.container)
        {
            return None;
        }
        let resume = xcb_core::reflex::route_features(text, false, false)
            .get("resume")
            .is_some_and(|value| *value > 0.0);
        let short = !self.cues.infer_only
            && named_nothing
            && text.split_whitespace().count() <= SHORT_PROMPT_WORDS;
        (continue_like(text) || resume || short).then(|| {
            self.bound(
                workspace,
                BindingSource::Continuation,
                BindingConfidence::Medium,
                format!("continuing in `{}`", self.name(workspace)),
                vec![],
            )
        })
    }

    /// Rung 8: the last thread task within 6 h, else the most recently used
    /// non-container entry within 30 days.
    fn most_recent(&self) -> Option<Resolution> {
        let mut entries: Vec<&KnownWorkspace> = self
            .known
            .iter()
            .copied()
            .filter(|entry| !entry.container)
            .collect();
        entries.sort_by(|a, b| {
            b.last_used_ms
                .cmp(&a.last_used_ms)
                .then_with(|| a.path.cmp(&b.path))
        });
        let pick = self
            .cues
            .last_thread_task
            .filter(|(workspace, at_ms)| {
                self.recent(*at_ms, CONTINUE_WINDOW_MS)
                    && self.entry(workspace).is_some_and(|entry| !entry.container)
            })
            .map(|(workspace, _)| workspace)
            .or_else(|| {
                entries
                    .first()
                    .filter(|entry| self.recent(entry.last_used_ms, RECENT_WINDOW_MS))
                    .map(|entry| entry.path.as_str())
            })?;
        let alternatives = entries
            .iter()
            .filter(|entry| entry.path != pick && self.recent(entry.last_used_ms, RECENT_WINDOW_MS))
            .take(MAX_ALTERNATIVES)
            .map(|entry| entry.path.clone())
            .collect();
        Some(self.bound(
            pick,
            BindingSource::Recent,
            BindingConfidence::Low,
            "most recent project".into(),
            alternatives,
        ))
    }

    /// Rung 9: up to 8 candidates ranked by focus, hint and recency.
    fn which(&self) -> Resolution {
        let mut entries: Vec<&KnownWorkspace> = self
            .known
            .iter()
            .copied()
            .filter(|entry| !entry.container)
            .collect();
        let rank = |entry: &KnownWorkspace| {
            (
                Some(entry.path.as_str()) != self.focus(),
                Some(entry.path.as_str()) != self.cues.launch_hint,
                std::cmp::Reverse(entry.last_used_ms),
            )
        };
        entries.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.path.cmp(&b.path)));
        let rejected: Vec<&str> = self
            .cues
            .prompt_roots
            .iter()
            .filter_map(|root| root.as_ref().err().map(String::as_str))
            .collect();
        let reason = match rejected.first() {
            Some(why) => format!("which project? a path in the prompt was refused: {why}"),
            None => "which project?".into(),
        };
        Resolution::Ask {
            candidates: entries
                .into_iter()
                .take(MAX_CANDIDATES)
                .map(|entry| entry.path.clone())
                .collect(),
            new_roots: vec![],
            reason: clip(reason),
        }
    }
}

/// Resolve the workspace for one prompt with the deterministic ladder:
/// explicit, target, prompt path, name mention, focus, continuation, launch
/// hint, most recent, else ask. Pure: the store supplies the snapshot and
/// the clock. Inference never admits a directory. `binding.origin` is a
/// placeholder the store overwrites from the intake cues, and the store
/// applies `hold` only to TUI prompts.
pub fn resolve(text: &str, cues: &Cues, known: &[KnownWorkspace], now_ms: u64) -> Resolution {
    let mut sorted: Vec<&KnownWorkspace> = known.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    sorted.dedup_by(|a, b| a.path == b.path);
    let ladder = Ladder {
        cues,
        known: sorted,
        now_ms,
    };
    if let Some(workspace) = cues.explicit {
        return ladder.bound(
            workspace,
            BindingSource::Explicit,
            BindingConfidence::High,
            "named directory".into(),
            vec![],
        );
    }
    if let Some(workspace) = cues.target.filter(|path| ladder.usable(path)) {
        return ladder.bound(
            workspace,
            BindingSource::Target,
            BindingConfidence::High,
            "addressed task".into(),
            vec![],
        );
    }
    if let Some(resolution) = ladder.prompt_path() {
        return resolution;
    }
    let named = ladder.named(text);
    if let Some(resolution) = ladder.name_mention(&named) {
        return resolution;
    }
    if let Some(workspace) = ladder.focus() {
        return ladder.bound(
            workspace,
            BindingSource::Focus,
            BindingConfidence::High,
            "focus".into(),
            vec![],
        );
    }
    if let Some(resolution) = ladder.continuation(text, named.is_empty()) {
        return resolution;
    }
    if cues.allow_guess && !cues.infer_only {
        if let Some(workspace) = cues.launch_hint.filter(|path| !ladder.container(path)) {
            return ladder.bound(
                workspace,
                BindingSource::Launch,
                BindingConfidence::Medium,
                "launch dir".into(),
                vec![],
            );
        }
        if let Some(resolution) = ladder.most_recent() {
            return resolution;
        }
    }
    ladder.which()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 100 * 24 * 60 * 60 * 1000;
    const HOUR: u64 = 60 * 60 * 1000;

    fn entry(path: &str, repo: Option<&str>, last_used_ms: u64) -> KnownWorkspace {
        KnownWorkspace {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            repo: repo.map(Into::into),
            last_used_ms,
            container: false,
            explicit_add: false,
        }
    }

    fn container(path: &str, explicit_add: bool) -> KnownWorkspace {
        KnownWorkspace {
            container: true,
            explicit_add,
            ..entry(path, None, NOW - HOUR)
        }
    }

    /// `/w/xcb`, `/w/gobstopper` and `/w/algal`, used an hour apart.
    fn registry() -> Vec<KnownWorkspace> {
        vec![
            entry("/w/xcb", Some("hraness/xcb"), NOW - HOUR),
            entry("/w/gobstopper", Some("hraness/gobstopper"), NOW - 2 * HOUR),
            entry("/w/algal", Some("hraness/algal"), NOW - 3 * HOUR),
        ]
    }

    fn tui<'a>() -> Cues<'a> {
        Cues {
            allow_admit: true,
            allow_guess: true,
            ..Cues::default()
        }
    }

    fn relay<'a>() -> Cues<'a> {
        Cues {
            infer_only: true,
            ..Cues::default()
        }
    }

    fn bound(resolution: Resolution) -> (String, WorkspaceBinding, bool) {
        match resolution {
            Resolution::Bound {
                workspace,
                binding,
                hold,
            } => (workspace, binding, hold),
            Resolution::Ask { reason, .. } => panic!("asked: {reason}"),
        }
    }

    fn asked(resolution: Resolution) -> (Vec<String>, Vec<String>, String) {
        match resolution {
            Resolution::Ask {
                candidates,
                new_roots,
                reason,
            } => (candidates, new_roots, reason),
            Resolution::Bound { workspace, .. } => panic!("bound {workspace}"),
        }
    }

    fn roots(paths: &[&str]) -> Vec<Result<String, String>> {
        paths.iter().map(|path| Ok((*path).to_owned())).collect()
    }

    #[test]
    fn path_tokens_take_leading_cued_and_marked_paths() {
        assert_eq!(
            path_tokens("/w/xcb/src/lib.rs:12: error\nfix it in ~/w/algal, then cd /w/a."),
            ["/w/xcb/src/lib.rs:12", "~/w/algal", "/w/a"]
        );
        assert_eq!(
            path_tokens("look @/w/xcb and under (/w/b)!"),
            ["/w/xcb", "/w/b"]
        );
        // Mid-sentence paths and URLs are prose, not directions.
        assert!(path_tokens("compare /w/a with https://x.test/w").is_empty());
        assert_eq!(path_tokens("cd ~ and cd /"), ["~", "/"]);
        let many: String = (0..12).map(|i| format!("in /w/{i} ")).collect();
        assert_eq!(path_tokens(&many).len(), 8);
        assert_eq!(path_tokens("in /w/a in /w/a"), ["/w/a"]);
    }

    #[test]
    fn continue_like_and_overlap_helpers() {
        assert!(continue_like("Please continue."));
        assert!(!continue_like("continue the migration in xcb"));
        assert!(workspaces_overlap("/r", "/r/sub"));
        assert!(workspaces_overlap("/r/sub", "/r"));
        assert!(workspaces_overlap("/r", "/r"));
        assert!(!workspaces_overlap("/r", "/rust"));
        assert!(!workspaces_overlap("/r/a", "/r/b"));
    }

    #[test]
    fn rung1_explicit_binds_high_even_over_a_prompt_path() {
        let cues = Cues {
            explicit: Some("/x/sub"),
            target: Some("/w/algal"),
            focus: Some("/w/algal"),
            prompt_roots: roots(&["/w/xcb"]),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve("fix /w/xcb", &cues, &registry(), NOW));
        assert_eq!(workspace, "/x/sub");
        assert_eq!(binding.source, BindingSource::Explicit);
        assert_eq!(binding.confidence, BindingConfidence::High);
        assert!(!hold);
    }

    #[test]
    fn rung2_target_binds_high_over_a_name_mention() {
        let cues = Cues {
            target: Some("/w/algal"),
            ..tui()
        };
        let (workspace, binding, hold) =
            bound(resolve("also bump gobstopper", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Target);
        assert_eq!(binding.reason, "addressed task");
        assert!(!hold);
    }

    #[test]
    fn rung3_one_registered_prompt_root_binds_high() {
        let cues = Cues {
            prompt_roots: roots(&["/w/algal", "/w/algal"]),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve(
            "/w/algal/src/x.rs:3 panics",
            &cues,
            &registry(),
            NOW,
        ));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Mention);
        assert_eq!(binding.confidence, BindingConfidence::High);
        assert!(!hold);
    }

    #[test]
    fn rung4_one_name_mention_binds_high() {
        for text in [
            "Bump Gobstopper to 0.5",
            "ship @gobstopper.",
            "the gobstopper, please",
        ] {
            let (workspace, binding, hold) = bound(resolve(text, &tui(), &registry(), NOW));
            assert_eq!(workspace, "/w/gobstopper", "{text}");
            assert_eq!(binding.source, BindingSource::Mention);
            assert_eq!(binding.confidence, BindingConfidence::High);
            assert_eq!(binding.reason, "named `gobstopper`");
            assert!(!hold);
        }
        // Whole words only.
        assert!(matches!(
            resolve(
                "run gobstoppers and xcb.sh checks now then more words to be long enough here",
                &Cues {
                    allow_guess: false,
                    ..tui()
                },
                &registry(),
                NOW
            ),
            Resolution::Ask { .. }
        ));
    }

    #[test]
    fn rung5_focus_binds_high() {
        let cues = Cues {
            focus: Some("/w/algal"),
            launch_hint: Some("/w/xcb"),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve("run the tests", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Focus);
        assert_eq!(binding.confidence, BindingConfidence::High);
        assert!(!hold);
    }

    #[test]
    fn continuation_within_window() {
        let cues = Cues {
            last_thread_task: Some(("/w/algal", NOW - 5 * HOUR)),
            launch_hint: Some("/w/xcb"),
            ..tui()
        };
        for text in [
            "continue",
            "pick up where the last session stopped and finish the remaining migration steps carefully",
        ] {
            let (workspace, binding, hold) = bound(resolve(text, &cues, &registry(), NOW));
            assert_eq!(workspace, "/w/algal", "{text}");
            assert_eq!(binding.source, BindingSource::Continuation);
            assert_eq!(binding.confidence, BindingConfidence::Medium);
            assert_eq!(binding.reason, "continuing in `algal`");
            assert!(!hold);
        }
    }

    #[test]
    fn stale_continuation_falls_through() {
        let cues = Cues {
            last_thread_task: Some(("/w/algal", NOW - 7 * HOUR)),
            launch_hint: Some("/w/xcb"),
            ..tui()
        };
        let (workspace, binding, _) = bound(resolve("continue", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/xcb");
        assert_eq!(binding.source, BindingSource::Launch);
    }

    #[test]
    fn short_prompt_continuation_tui_only() {
        let last = Some(("/w/algal", NOW - HOUR));
        let cues = Cues {
            last_thread_task: last,
            ..tui()
        };
        let (workspace, binding, _) = bound(resolve("run the tests", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Continuation);
        // A long prompt without a cue is not a continuation.
        let long =
            "write a migration plan for the schema and the registry and the picker and the docs";
        let (_, binding, _) = bound(resolve(long, &cues, &registry(), NOW));
        assert_eq!(binding.source, BindingSource::Recent);
        // Relay never takes the short-prompt clause.
        let cues = Cues {
            last_thread_task: last,
            ..relay()
        };
        assert!(matches!(
            resolve("run the tests", &cues, &registry(), NOW),
            Resolution::Ask { .. }
        ));
    }

    #[test]
    fn rung7_launch_hint_binds_medium() {
        let cues = Cues {
            launch_hint: Some("/w/gobstopper"),
            ..tui()
        };
        let long =
            "write a migration plan for the schema and the registry and the picker and the docs";
        let (workspace, binding, hold) = bound(resolve(long, &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/gobstopper");
        assert_eq!(binding.source, BindingSource::Launch);
        assert_eq!(binding.confidence, BindingConfidence::Medium);
        assert_eq!(binding.reason, "launch dir");
        assert!(!hold);
    }

    #[test]
    fn rung8_most_recent_is_low_held_and_lists_alternatives() {
        let (workspace, binding, hold) = bound(resolve("tidy things up", &tui(), &registry(), NOW));
        assert_eq!(workspace, "/w/xcb");
        assert_eq!(binding.source, BindingSource::Recent);
        assert_eq!(binding.confidence, BindingConfidence::Low);
        assert_eq!(binding.reason, "most recent project");
        assert_eq!(binding.alternatives, ["/w/gobstopper", "/w/algal"]);
        assert!(hold);
        // A thread task within 6 h beats registry recency.
        let cues = Cues {
            last_thread_task: Some(("/w/algal", NOW - 7 * HOUR)),
            ..tui()
        };
        assert_eq!(bound(resolve("tidy", &cues, &registry(), NOW)).0, "/w/xcb");
        // Nothing used within 30 days: ask.
        let stale: Vec<_> = registry()
            .into_iter()
            .map(|mut entry| {
                entry.last_used_ms = 1;
                entry
            })
            .collect();
        assert!(matches!(
            resolve("tidy things up", &tui(), &stale, NOW),
            Resolution::Ask { .. }
        ));
    }

    #[test]
    fn rung9_asks_with_candidates_ranked_by_hint_then_recency() {
        let mut known = registry();
        known.push(container("/w", false));
        let cues = Cues {
            launch_hint: Some("/w/algal"),
            allow_guess: false,
            ..tui()
        };
        let (candidates, new_roots, reason) = asked(resolve("tidy up", &cues, &known, NOW));
        assert_eq!(candidates, ["/w/algal", "/w/xcb", "/w/gobstopper"]);
        assert!(new_roots.is_empty());
        assert_eq!(reason, "which project?");
    }

    #[test]
    fn two_prompt_roots_ask_never_pick() {
        let cues = Cues {
            prompt_roots: roots(&["/w/xcb", "/w/algal"]),
            focus: Some("/w/xcb"),
            ..tui()
        };
        let (candidates, new_roots, _) = asked(resolve(
            "in /w/xcb and in /w/algal",
            &cues,
            &registry(),
            NOW,
        ));
        assert_eq!(candidates, ["/w/algal", "/w/xcb"]);
        assert!(new_roots.is_empty());
    }

    #[test]
    fn unregistered_prompt_root_asks_with_new_root() {
        let cues = Cues {
            prompt_roots: roots(&["/w/new"]),
            focus: Some("/w/xcb"),
            ..tui()
        };
        let (_, new_roots, reason) = asked(resolve("cd /w/new", &cues, &registry(), NOW));
        assert_eq!(new_roots, ["/w/new"]);
        assert!(reason.contains("not a known project"), "{reason}");
        // Without admission rights the same prompt asks without offering it.
        let cues = Cues {
            allow_admit: false,
            ..cues
        };
        assert!(
            asked(resolve("cd /w/new", &cues, &registry(), NOW))
                .1
                .is_empty()
        );
    }

    #[test]
    fn container_candidate_is_never_inferred() {
        let mut known = registry();
        known.push(KnownWorkspace {
            name: "documents".into(),
            ..container("/w", false)
        });
        let cues = Cues {
            prompt_roots: roots(&["/w"]),
            last_thread_task: Some(("/w", NOW - HOUR)),
            launch_hint: Some("/w"),
            ..tui()
        };
        let resolution = resolve("go through documents", &cues, &known, NOW);
        let (workspace, binding, _) = bound(resolution);
        assert_ne!(workspace, "/w");
        assert_eq!(binding.source, BindingSource::Recent);
        let cues = Cues {
            allow_guess: false,
            ..cues
        };
        let (candidates, _, _) = asked(resolve("go through documents", &cues, &known, NOW));
        assert!(!candidates.contains(&"/w".to_owned()));
    }

    #[test]
    fn focus_and_target_containers_skipped_unless_explicit_add() {
        let mut known = registry();
        known.push(container("/w", false));
        let cues = Cues {
            target: Some("/w"),
            focus: Some("/w"),
            allow_guess: false,
            ..tui()
        };
        assert!(matches!(
            resolve("tidy up", &cues, &known, NOW),
            Resolution::Ask { .. }
        ));
        known.pop();
        known.push(container("/w", true));
        let (workspace, binding, _) = bound(resolve("tidy up", &cues, &known, NOW));
        assert_eq!(workspace, "/w");
        assert_eq!(binding.source, BindingSource::Target);
        let cues = Cues {
            target: None,
            ..cues
        };
        let (workspace, binding, _) = bound(resolve("tidy up", &cues, &known, NOW));
        assert_eq!(workspace, "/w");
        assert_eq!(binding.source, BindingSource::Focus);
    }

    #[test]
    fn home_and_filesystem_root_tokens_are_rejected_with_evidence() {
        assert_eq!(path_tokens("cd ~ then cd /"), ["~", "/"]);
        let cues = Cues {
            prompt_roots: vec![
                Err("workspace is not allowed: home".into()),
                Err("workspace is not allowed: filesystem root".into()),
            ],
            allow_guess: false,
            ..tui()
        };
        let (candidates, new_roots, reason) =
            asked(resolve("cd ~ then cd /", &cues, &registry(), NOW));
        assert_eq!(candidates.len(), 3);
        assert!(new_roots.is_empty());
        assert!(
            reason.contains("workspace is not allowed: home"),
            "{reason}"
        );
    }

    #[test]
    fn mention_overrides_focus_is_medium_held_and_explained() {
        let cues = Cues {
            focus: Some("/w/xcb"),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve("bump gobstopper", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/gobstopper");
        assert_eq!(binding.source, BindingSource::Mention);
        assert_eq!(binding.confidence, BindingConfidence::Medium);
        assert_eq!(binding.reason, "named `gobstopper` (overrides focus xcb)");
        assert!(hold);
        // Naming the focused project keeps it high and unheld.
        let (_, binding, hold) = bound(resolve("bump xcb", &cues, &registry(), NOW));
        assert_eq!(binding.confidence, BindingConfidence::High);
        assert!(!hold);
    }

    #[test]
    fn path_overrides_focus_is_medium_and_held() {
        let cues = Cues {
            focus: Some("/w/xcb"),
            prompt_roots: roots(&["/w/algal"]),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve(
            "/w/algal/src/x.rs:3: panic",
            &cues,
            &registry(),
            NOW,
        ));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Mention);
        assert_eq!(binding.confidence, BindingConfidence::Medium);
        assert!(
            binding.reason.ends_with("(overrides focus xcb)"),
            "{}",
            binding.reason
        );
        assert!(hold);
    }

    #[test]
    fn stoplist_names_never_bind() {
        let known = vec![
            entry("/w/site", None, NOW - HOUR),
            entry("/w/api", Some("hraness/api"), NOW - HOUR),
            entry("/w/ui", None, NOW - HOUR),
        ];
        let cues = Cues {
            allow_guess: false,
            ..tui()
        };
        for text in ["fix the site", "update the api docs", "tweak ui colors"] {
            assert!(
                matches!(resolve(text, &cues, &known, NOW), Resolution::Ask { .. }),
                "{text}"
            );
        }
    }

    #[test]
    fn worktree_group_tie_prefers_focus_then_recent_with_hold_and_alternatives() {
        let mut known = registry();
        known.push(entry("/w/xcb-wt-a", Some("hraness/xcb"), NOW - 5 * HOUR));
        known.push(entry(
            "/w/xcb-wt-b",
            Some("hraness/xcb"),
            NOW - 30 * 60 * 1000,
        ));
        // Every checkout's name differs; the repo tail `xcb` names the group.
        let (workspace, binding, hold) = bound(resolve("fix xcb", &tui(), &known, NOW));
        assert_eq!(workspace, "/w/xcb-wt-b");
        assert_eq!(binding.confidence, BindingConfidence::Low);
        assert_eq!(binding.alternatives, ["/w/xcb", "/w/xcb-wt-a"]);
        assert!(hold);
        let cues = Cues {
            focus: Some("/w/xcb-wt-a"),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve("fix xcb", &cues, &known, NOW));
        assert_eq!(workspace, "/w/xcb-wt-a");
        assert_eq!(binding.confidence, BindingConfidence::Medium);
        assert!(!hold);
        let cues = Cues {
            launch_hint: Some("/w/xcb"),
            ..tui()
        };
        let (workspace, binding, hold) = bound(resolve("fix xcb", &cues, &known, NOW));
        assert_eq!(workspace, "/w/xcb");
        assert_eq!(binding.confidence, BindingConfidence::Medium);
        assert!(!hold);
        // Relay binds only a single-path match.
        assert!(matches!(
            resolve("fix xcb", &relay(), &known, NOW),
            Resolution::Ask { .. }
        ));
        // Two repository groups ask.
        assert!(matches!(
            resolve("port gobstopper to algal", &tui(), &known, NOW),
            Resolution::Ask { .. }
        ));
    }

    #[test]
    fn relay_infer_only_disables_guesses_new_roots_and_short_prompt_continuation() {
        let cues = Cues {
            launch_hint: Some("/w/xcb"),
            last_thread_task: Some(("/w/algal", NOW - HOUR)),
            prompt_roots: roots(&["/w/new"]),
            ..relay()
        };
        let (_, new_roots, _) = asked(resolve("in /w/new run it", &cues, &registry(), NOW));
        assert!(new_roots.is_empty());
        let cues = Cues {
            prompt_roots: vec![],
            ..cues
        };
        assert!(matches!(
            resolve("tidy up", &cues, &registry(), NOW),
            Resolution::Ask { .. }
        ));
        // An explicit cue still continues the relay's own last task.
        let (workspace, binding, _) = bound(resolve("continue", &cues, &registry(), NOW));
        assert_eq!(workspace, "/w/algal");
        assert_eq!(binding.source, BindingSource::Continuation);
        // A registered single root and a unique name still bind.
        assert_eq!(
            bound(resolve("bump gobstopper", &cues, &registry(), NOW)).0,
            "/w/gobstopper"
        );
    }

    #[test]
    fn relay_infer_short_prompt_without_cue_asks() {
        let cues = Cues {
            last_thread_task: Some(("/w/algal", NOW - HOUR)),
            ..relay()
        };
        let (candidates, _, reason) = asked(resolve("run the tests", &cues, &registry(), NOW));
        assert_eq!(reason, "which project?");
        assert_eq!(candidates.len(), 3);
    }

    #[test]
    fn resolution_is_independent_of_registry_row_order() {
        let mut known = registry();
        known.push(entry("/w/xcb-wt", Some("hraness/xcb"), NOW - HOUR));
        known.push(entry("/w/zeta", None, NOW - HOUR));
        let prompts = ["fix xcb", "tidy up", "bump gobstopper", "run it"];
        let cues = tui();
        let expected: Vec<_> = prompts
            .iter()
            .map(|text| resolve(text, &cues, &known, NOW))
            .collect();
        let mut reversed = known.clone();
        reversed.reverse();
        let mut rotated = known.clone();
        rotated.rotate_left(2);
        for order in [reversed, rotated] {
            for (text, expected) in prompts.iter().zip(&expected) {
                assert_eq!(&resolve(text, &cues, &order, NOW), expected, "{text}");
            }
        }
        let cues = Cues {
            allow_guess: false,
            ..tui()
        };
        let mut order = known.clone();
        let first = resolve("tidy up", &cues, &order, NOW);
        order.reverse();
        assert_eq!(resolve("tidy up", &cues, &order, NOW), first);
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
        assert_eq!(clip("é".repeat(100)).len(), 160);
    }
}
