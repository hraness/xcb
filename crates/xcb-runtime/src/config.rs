use crate::{Error, Result, digest, private};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use xcb_core::{
    Id,
    models::{Preference, default_preferences},
    policy::AutoContinue,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextPolicy {
    pub enabled: bool,
    pub trigger_tokens: u64,
    pub floor_tokens: u64,
    pub min_interval_ms: u64,
    pub min_savings_tokens: u64,
}
impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_tokens: 250_000,
            floor_tokens: 40_000,
            min_interval_ms: 300_000,
            min_savings_tokens: 4_096,
        }
    }
}

/// Judge (jev-style judgment API) policy. Disabled by default: routing asks
/// send bounded prompt state to an external service, so use is opt-in.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JudgeConfig {
    pub enabled: bool,
    pub model: Option<Id>,
    pub endpoint: Option<String>,
}

/// How a reflex participates in decisions. `Observe` records decisions and
/// labels and learns from them without acting; `Active` lets the reflex's
/// decision drive routing or continuation within every deterministic gate.
/// `Auto` observes until the operator's own labels certify a head (see
/// `xcb_core::reflex::certify`), then acts on the turns it scores above the
/// certified threshold, and returns to observing if its precision falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflexMode {
    Off,
    Observe,
    Active,
    Auto,
}

/// Reflex policy. Routing is active by default because its shipped
/// parameters reproduce the prior classifier exactly. Continuing a
/// stopped-short report and answering a worker's request for confirmation
/// are `auto` by default: each acts only once the operator's own replies
/// certify its precision, and `confirm` acts only while settle is not off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReflexConfig {
    pub route: ReflexMode,
    pub settle: ReflexMode,
    /// `active`: a completed turn the settle reflex categorizes as `confirm`
    /// is answered with a standing go-ahead, unless the request carries a
    /// risk or hand-off cue. `auto` does so once the head is certified.
    /// `observe` and `off` never answer.
    pub confirm: ReflexMode,
    /// Fit and promote new parameter generations from local labels.
    pub learn: bool,
}
impl Default for ReflexConfig {
    fn default() -> Self {
        Self {
            route: ReflexMode::Active,
            settle: ReflexMode::Auto,
            confirm: ReflexMode::Auto,
            learn: true,
        }
    }
}
impl ReflexConfig {
    pub fn mode(&self, reflex: xcb_core::reflex::Reflex) -> ReflexMode {
        match reflex {
            xcb_core::reflex::Reflex::Route => self.route,
            xcb_core::reflex::Reflex::Settle => self.settle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extensions {
    pub auto_continue: AutoContinue,
    pub gobstopper: ContextPolicy,
    pub usage: bool,
    pub aicharts_upload: bool,
    pub aicharts_export: bool,
    pub hooks: bool,
    pub judge: JudgeConfig,
    pub reflexes: ReflexConfig,
}
impl Default for Extensions {
    fn default() -> Self {
        Self {
            auto_continue: AutoContinue::default(),
            gobstopper: ContextPolicy::default(),
            usage: true,
            aicharts_upload: false,
            aicharts_export: false,
            hooks: false,
            judge: JudgeConfig::default(),
            reflexes: ReflexConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub default_account: Option<Id>,
    pub favorites: Vec<Preference>,
    pub pane: Id,
    pub reduced_motion: bool,
    pub auto_failover: bool,
    /// Independent deadline for one provider turn, including initialization.
    pub turn_timeout_ms: u64,
    pub extensions: Extensions,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            default_account: None,
            favorites: default_preferences(),
            pane: Id::new("focus").expect("static pane"),
            reduced_motion: false,
            auto_failover: true,
            turn_timeout_ms: 1_800_000,
            extensions: Extensions::default(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        let context = &self.extensions.gobstopper;
        let continuation = &self.extensions.auto_continue;
        if self.version != 1
            || !(1_000..=3_600_000).contains(&self.turn_timeout_ms)
            || self.favorites.len() > 128
            || context.floor_tokens < 1024
            || context.floor_tokens >= context.trigger_tokens
            || context.trigger_tokens > 1_000_000
            || context.min_savings_tokens > context.trigger_tokens
            || !(1000..=3_600_000).contains(&context.min_interval_ms)
            || !(1..=16).contains(&continuation.max_consecutive)
            || !(1000..=3_600_000).contains(&continuation.max_elapsed_ms)
            || self
                .extensions
                .judge
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| endpoint.len() > 1024 || endpoint.is_empty())
        {
            return Err(xcb_core::Error::Invalid("configuration").into());
        }
        let keys: BTreeSet<_> = self
            .favorites
            .iter()
            .map(|fav| (fav.provider, &fav.model, &fav.effort))
            .collect();
        if keys.len() != self.favorites.len() {
            return Err(xcb_core::Error::Invalid("duplicate favorite").into());
        }
        Ok(())
    }
    pub fn load(root: &Path) -> Result<(Self, Option<String>)> {
        let path = root.join("config.json");
        match private::read(&path, 64 * 1024) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes).map_err(|_| {
                    Error::Unavailable(
                        "config.json is incompatible with this xcb build; update xcb or check the configuration file",
                    )
                })?;
                config.validate()?;
                Ok((config, Some(digest(bytes))))
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::default(), None))
            }
            Err(error) => Err(error),
        }
    }
    pub fn save(&self, root: &Path, revision: Option<&str>) -> Result<()> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let path = root.join("config.json");
        match revision {
            Some(revision) => private::replace(&path, &bytes, revision),
            None => private::create(&path, &bytes),
        }
    }
}
