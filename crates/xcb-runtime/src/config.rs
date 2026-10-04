use crate::{Error, Result, digest, private, routing_stack::RoutingConfig};
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
/// certify its precision. `confirm` never acts while settle is off or only
/// observing, and routing has no `auto` mode.
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
    /// How long an account stays at a known usage limit after a provider
    /// refused a turn for its quota without reporting when the quota resets
    /// (every Codex `usageLimitExceeded` and Devin resource-exhaustion error,
    /// and a Claude rejection without a reset). A provider-reported reset
    /// observed later replaces it. The default of 30 minutes trades one
    /// wasted turn per half hour per account against sidelining the account:
    /// Claude and Codex meter in five-hour and weekly windows that rarely
    /// reopen sooner, while Devin reports no comparable window at all.
    /// Range: one minute to seven days.
    pub quota_limit_cooldown_ms: u64,
    /// How many unsettled runs one account may hold at once. The default of
    /// 1 keeps the account dedicated to a single task; raising it runs several
    /// provider conversations on the same subscription, sharing its quota and
    /// rate limits and multiplying each. Sign-in, probes and account removal
    /// still hold the account exclusively: they are refused while any run is
    /// unsettled, and no new run starts while one is in flight.
    /// Range: 1 to 32.
    pub max_runs_per_account: u32,
    /// Upper bound for concurrent managed tasks across all accounts. The
    /// supervisor starts at one and ramps toward this bound when telemetry and
    /// routing remain healthy; pressure or an unreadable policy backs it down.
    /// Range: 1 to 64.
    pub max_active_runs: u32,
    /// Whether the supervisor should adapt its active target instead of
    /// launching directly up to `max_active_runs`.
    pub adaptive_parallelism: bool,
    /// The routing preference stack: ordered route patterns per task tier,
    /// routes never used, and providers that serve only as a fallback. See
    /// [`crate::routing_stack`].
    pub routing: RoutingConfig,
    /// Host pressure protection for managed workers. Explicitly enabled
    /// after checking telemetry with `xcb resources` on this machine.
    pub resources: crate::host_resources::ResourcePolicy,
    /// Explicitly registered host tool servers, shared by every provider.
    #[serde(skip_serializing_if = "crate::capabilities::CapabilityConfig::is_empty")]
    pub capabilities: crate::capabilities::CapabilityConfig,
    pub extensions: Extensions,
}
pub const DEFAULT_QUOTA_LIMIT_COOLDOWN_MS: u64 = 1_800_000;
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
            quota_limit_cooldown_ms: DEFAULT_QUOTA_LIMIT_COOLDOWN_MS,
            max_runs_per_account: 1,
            max_active_runs: 4,
            adaptive_parallelism: true,
            routing: RoutingConfig::default(),
            resources: crate::host_resources::ResourcePolicy::default(),
            capabilities: crate::capabilities::CapabilityConfig::default(),
            extensions: Extensions::default(),
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        self.capabilities.validate()?;
        self.resources.validate().map_err(|message| Error::Guided {
            message,
            next: Some("check resources in config.json".into()),
        })?;
        let context = &self.extensions.gobstopper;
        let continuation = &self.extensions.auto_continue;
        if self.version != 1
            || !(1_000..=3_600_000).contains(&self.turn_timeout_ms)
            || !(60_000..=604_800_000).contains(&self.quota_limit_cooldown_ms)
            || !(1..=32).contains(&self.max_runs_per_account)
            || !(1..=64).contains(&self.max_active_runs)
            || self.favorites.len() > 128
            || context.floor_tokens < 1024
            || context.floor_tokens >= context.trigger_tokens
            || context.trigger_tokens > 1_000_000
            || context.min_savings_tokens > context.trigger_tokens
            || !(1000..=3_600_000).contains(&context.min_interval_ms)
            || !(1..=16).contains(&continuation.max_consecutive)
            || !(1000..=3_600_000).contains(&continuation.max_elapsed_ms)
            || self.extensions.reflexes.route == ReflexMode::Auto
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
        self.routing.validate()
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
        if bytes.len() > 64 * 1024 {
            return Err(xcb_core::Error::Limit("configuration bytes").into());
        }
        let path = root.join("config.json");
        match revision {
            Some(revision) => private::replace(&path, &bytes, revision),
            None => private::create(&path, &bytes),
        }
    }
}

#[cfg(test)]
mod routing_config_tests {
    use super::*;

    fn state(directory: &tempfile::TempDir) -> std::path::PathBuf {
        private::directory(&xcb_core::canonical(directory.path()).unwrap().join("state")).unwrap()
    }

    fn load(json: &str) -> Result<Config> {
        let directory = tempfile::tempdir().unwrap();
        let root = state(&directory);
        private::create(&root.join("config.json"), json.as_bytes()).unwrap();
        Config::load(&root).map(|(config, _)| config)
    }

    #[test]
    fn routing_defaults_apply_when_the_key_is_absent_or_partial() {
        let absent = load(r#"{"turn_timeout_ms": 5000}"#).unwrap();
        assert_eq!(
            absent.routing,
            crate::routing_stack::RoutingConfig::default()
        );
        assert_eq!(absent.routing.never, ["devin/swe-*"]);
        assert_eq!(
            absent.routing.fallback_providers,
            [xcb_core::Provider::Devin]
        );
        let partial =
            load(r#"{"routing": {"never": [], "tiers": {"meaty": ["claude/*fable*/max"]}}}"#)
                .unwrap();
        assert!(partial.routing.never.is_empty());
        assert_eq!(partial.routing.tiers.meaty, ["claude/*fable*/max"]);
        assert_eq!(
            partial.routing.tiers.buildout,
            crate::routing_stack::RoutingConfig::default()
                .tiers
                .buildout
        );
        assert_eq!(
            partial.routing.fallback_providers,
            [xcb_core::Provider::Devin]
        );
    }

    #[test]
    fn routing_rejects_malformed_patterns_unknown_keys_and_long_lists() {
        let message = |json: &str| load(json).unwrap_err().to_string();
        assert!(message(r#"{"routing": {"never": ["devin"]}}"#).contains("provider/model-glob"));
        assert!(
            message(r#"{"routing": {"tiers": {"default": ["openai/gpt-*/high"]}}}"#)
                .contains("provider")
        );
        assert!(
            message(r#"{"routing": {"tiers": {"default": ["codex/gpt-*/turbo"]}}}"#)
                .contains("effort")
        );
        assert!(
            message(r#"{"routing": {"fallback_providers": ["openai"]}}"#).contains("incompatible")
        );
        assert!(message(r#"{"routing": {"tiers": {"huge": []}}}"#).contains("incompatible"));
        assert!(message(r#"{"routing": {"delegate": true}}"#).contains("incompatible"));
        let long = format!(
            r#"{{"routing": {{"never": [{}]}}}}"#,
            std::iter::repeat_n("\"devin/swe-*\"", 65)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(message(&long).contains("routing patterns"));
    }

    #[test]
    fn routing_round_trips_through_save_and_load() {
        let directory = tempfile::tempdir().unwrap();
        let root = state(&directory);
        let mut config = Config::default();
        config.routing.never.push("codex/gpt-5.6-sol/low".into());
        config.save(&root, None).unwrap();
        let (loaded, _) = Config::load(&root).unwrap();
        assert_eq!(loaded.routing, config.routing);
        let saved: serde_json::Value =
            serde_json::from_slice(&private::read(&root.join("config.json"), 64 * 1024).unwrap())
                .unwrap();
        assert!(
            saved.get("capabilities").is_none(),
            "ordinary saves retain the legacy configuration schema"
        );
    }

    #[test]
    fn oversized_valid_configuration_does_not_replace_a_readable_file() {
        let directory = tempfile::tempdir().unwrap();
        let root = state(&directory);
        let config = Config::default();
        config.save(&root, None).unwrap();
        let (mut large, revision) = Config::load(&root).unwrap();
        large.favorites = (0..128)
            .map(|index| Preference {
                provider: xcb_core::Provider::Codex,
                model: Id::new(format!("{index:03}{}", "a".repeat(157))).unwrap(),
                effort: Some(Id::new("a".repeat(160)).unwrap()),
            })
            .collect();
        let patterns: Vec<_> = (0..64)
            .map(|index| format!("codex/{index:02}{}", "a".repeat(120)))
            .collect();
        large.routing.never = patterns.clone();
        large.routing.tiers.default = patterns;
        large.validate().unwrap();
        assert!(serde_json::to_vec_pretty(&large).unwrap().len() > 64 * 1024);
        assert!(large.save(&root, revision.as_deref()).is_err());
        assert_eq!(Config::load(&root).unwrap().1, revision);
    }
}
