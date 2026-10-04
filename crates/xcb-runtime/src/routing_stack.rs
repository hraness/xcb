//! The routing preference stack: the owner's model policy as ordered route
//! patterns per task tier, a `never` list, and the providers that serve only
//! as a fallback. Defaults ship in code; `routing` in `config.json` overrides
//! them. [`crate::routing`] applies the stack after every account, quota,
//! cooldown and admission filter: the stack orders eligible routes, it never
//! widens eligibility.
//!
//! A route pattern is `provider/model-glob[/effort]`. The provider segment is
//! a provider name or `*`. The model glob (`*` any run, `?` one character)
//! matches the model's normalized identity: its id and, when the catalog
//! resolved an alias, the resolved id, both lowercased with `.` as `-`, with
//! and without the provider name prefix (`claude-`), so `opus*` matches
//! `opus`, `opus[1m]` and the resolved `claude-opus-5-5`. The effort segment
//! must equal the model's effort exactly, and an absent effort segment matches
//! every effort.
//!
//! Future extension: a later change adds a per-route `delegate` flag. The
//! string form stays; a route may then also be written as an object
//! `{ "route": "codex/gpt-*-astra/ultra", "delegate": true }`. Keep the
//! parser's string arm unchanged when that arm is added.

use crate::Result;
use serde::{Deserialize, Serialize};
use xcb_core::{Id, Provider, models::ModelChoice};

/// Effort tokens a pattern may name, strongest first.
pub const EFFORTS: [&str; 8] = [
    "ultra", "xhigh", "max", "high", "medium", "low", "minimal", "none",
];
/// Longest list `routing` accepts per tier and for `never`.
pub const MAX_PATTERNS: usize = 64;
const MAX_PATTERN_BYTES: usize = 128;

/// A pinned model that `routing.never` excludes. The pin is refused instead
/// of silently widened.
pub const EXCLUDED_BY_NEVER: &str =
    "model is excluded by routing.never in config.json; xcb routing show lists the patterns";

/// The task tier a stack applies to. Assignment rules live in
/// [`crate::routing`]; the order here is the order `xcb routing show` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Buildout,
    Meaty,
    Default,
    Mechanical,
}

impl Tier {
    pub const ALL: [Self; 4] = [Self::Buildout, Self::Meaty, Self::Default, Self::Mechanical];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buildout => "buildout",
            Self::Meaty => "meaty",
            Self::Default => "default",
            Self::Mechanical => "mechanical",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One parsed route pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutePattern {
    provider: String,
    model: String,
    effort: Option<String>,
}

impl RoutePattern {
    /// Parses `provider/model-glob[/effort]`; every rejection names the
    /// segment at fault.
    pub fn parse(text: &str) -> Result<Self> {
        if text.is_empty() || text.len() > MAX_PATTERN_BYTES {
            return Err(xcb_core::Error::Invalid("routing pattern length").into());
        }
        let mut segments = text.split('/');
        let (Some(provider), Some(model)) = (segments.next(), segments.next()) else {
            return Err(xcb_core::Error::Invalid(
                "routing pattern; write provider/model-glob or provider/model-glob/effort",
            )
            .into());
        };
        let effort = segments.next();
        if segments.next().is_some() {
            return Err(xcb_core::Error::Invalid(
                "routing pattern; write provider/model-glob or provider/model-glob/effort",
            )
            .into());
        }
        if provider != "*" && provider.parse::<Provider>().is_err() {
            return Err(xcb_core::Error::Invalid(
                "routing pattern provider; use claude, codex or *",
            )
            .into());
        }
        if model.is_empty()
            || !model.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_.[]*?".contains(&byte)
            })
        {
            return Err(xcb_core::Error::Invalid(
                "routing pattern model glob; use lowercase letters, digits, - _ . [ ] * ?",
            )
            .into());
        }
        if effort.is_some_and(|effort| effort != "*" && !EFFORTS.contains(&effort)) {
            return Err(xcb_core::Error::Invalid(
                "routing pattern effort; use ultra, xhigh, max, high, medium, low, minimal, none or *",
            )
            .into());
        }
        Ok(Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
            effort: effort.filter(|effort| *effort != "*").map(str::to_owned),
        })
    }

    pub fn matches(&self, model: &ModelChoice) -> bool {
        glob_matches(&self.provider, model.provider.as_str())
            && self
                .effort
                .as_deref()
                .is_none_or(|effort| effort == effort_of(model))
            && matching_identities(model)
                .iter()
                .any(|identity| glob_matches(&self.model, identity))
    }
}

/// `*` matches any run of characters, `?` exactly one; everything else is
/// literal. Iterative with backtracking to the last `*`, so a pattern cannot
/// take more than linear time in the text per star.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(b'?') => {
                p += 1;
                t += 1;
            }
            Some(byte) if *byte == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|byte| *byte == b'*')
}

/// The effort a model runs at: its own effort, or `medium` when absent.
pub(crate) fn effort_of(model: &ModelChoice) -> String {
    model
        .effort
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            EFFORTS
                .into_iter()
                .find(|level| model.id.as_str().ends_with(&format!("-{level}")))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "medium".into())
}

/// The normalized identity the profile table keys on: the resolved id when
/// the catalog resolved an alias, else the id, lowercased with `.` as `-`.
pub(crate) fn identity(model: &ModelChoice) -> String {
    normalize(
        model
            .resolved
            .as_ref()
            .map(Id::as_str)
            .unwrap_or(model.id.as_str()),
    )
}

fn normalize(raw: &str) -> String {
    raw.to_ascii_lowercase().replace('.', "-")
}

fn family_name(_model: &ModelChoice, identity: &str) -> String {
    identity.to_owned()
}

/// Every string a model glob is tried against.
fn matching_identities(model: &ModelChoice) -> Vec<String> {
    let mut identities = Vec::new();
    let prefix = format!("{}-", model.provider);
    for raw in std::iter::once(model.id.as_str()).chain(model.resolved.as_ref().map(Id::as_str)) {
        let name = family_name(model, &normalize(raw));
        if let Some(short) = name.strip_prefix(&prefix) {
            identities.push(short.to_owned());
        }
        identities.push(name);
    }
    identities
}

/// The numeric segments of a model's family name (`gpt-6-1-sol` → `[6, 1]`,
/// `claude-fable-5-1` → `[5, 1]`), so that among models one pattern matches
/// the newest version wins without a configuration change. A bracketed
/// suffix such as `[1m]` is not a version.
pub fn version(model: &ModelChoice) -> Vec<u64> {
    let identity = identity(model);
    let name = family_name(model, &identity);
    let name = name.split('[').next().unwrap_or_default();
    name.split('-')
        .filter_map(|segment| segment.parse::<u64>().ok())
        .collect()
}

/// The ordered route patterns of each tier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TierPatterns {
    pub buildout: Vec<String>,
    pub meaty: Vec<String>,
    pub r#default: Vec<String>,
    pub mechanical: Vec<String>,
}

impl Default for TierPatterns {
    fn default() -> Self {
        let list = |patterns: &[&str]| patterns.iter().map(|p| (*p).to_owned()).collect();
        Self {
            buildout: list(&["codex/gpt-*-astra/ultra", "claude/*fable*/max"]),
            meaty: list(&["codex/gpt-*-astra/max", "claude/*fable*/max"]),
            r#default: list(&["codex/gpt-*-sol/ultra", "claude/opus*/max"]),
            mechanical: list(&["codex/gpt-*-sol/max", "claude/opus*/max"]),
        }
    }
}

/// `routing` in `config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConfig {
    /// Routes never used: not by automatic routing, not by failover, and not
    /// by an explicit `--model` or route pin.
    pub never: Vec<String>,
    /// Providers whose routes are considered only when no route on another
    /// provider can take the task now.
    pub fallback_providers: Vec<Provider>,
    pub tiers: TierPatterns,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            never: Vec::new(),
            fallback_providers: Vec::new(),
            tiers: TierPatterns::default(),
        }
    }
}

impl RoutingConfig {
    pub fn validate(&self) -> Result<()> {
        for list in [&self.never].into_iter().chain(self.tier_lists()) {
            if list.len() > MAX_PATTERNS {
                return Err(xcb_core::Error::Limit("routing patterns").into());
            }
            for pattern in list {
                RoutePattern::parse(pattern)?;
            }
        }
        if self.fallback_providers.len() > Provider::SUPPORTED.len() {
            return Err(xcb_core::Error::Limit("routing fallback providers").into());
        }
        Ok(())
    }

    fn tier_lists(&self) -> [&Vec<String>; 4] {
        [
            &self.tiers.buildout,
            &self.tiers.meaty,
            &self.tiers.r#default,
            &self.tiers.mechanical,
        ]
    }

    pub fn patterns(&self, tier: Tier) -> &[String] {
        match tier {
            Tier::Buildout => &self.tiers.buildout,
            Tier::Meaty => &self.tiers.meaty,
            Tier::Default => &self.tiers.r#default,
            Tier::Mechanical => &self.tiers.mechanical,
        }
    }

    /// A parsed pattern list; patterns that fail to parse are skipped, which
    /// a validated configuration never has.
    fn parsed(patterns: &[String]) -> impl Iterator<Item = RoutePattern> + '_ {
        patterns
            .iter()
            .filter_map(|pattern| RoutePattern::parse(pattern).ok())
    }

    /// `true` when a `never` pattern matches the model.
    pub fn excluded(&self, model: &ModelChoice) -> bool {
        Self::parsed(&self.never).any(|pattern| pattern.matches(model))
    }

    pub fn is_fallback(&self, provider: Provider) -> bool {
        self.fallback_providers.contains(&provider)
    }

    /// Zero-based index of the first pattern in the tier that matches the
    /// model; `None` when no pattern does.
    pub fn position(&self, tier: Tier, model: &ModelChoice) -> Option<usize> {
        Self::parsed(self.patterns(tier)).position(|pattern| pattern.matches(model))
    }
}

/// A tier's patterns with the observed models each one matches, newest
/// version first: what `xcb routing show` prints.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternMatches {
    pub pattern: String,
    /// One-based position in the list.
    pub position: usize,
    pub models: Vec<String>,
}

pub fn pattern_matches(patterns: &[String], models: &[ModelChoice]) -> Vec<PatternMatches> {
    patterns
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let mut matched: Vec<_> = RoutePattern::parse(text)
                .map(|pattern| {
                    models
                        .iter()
                        .filter(|model| pattern.matches(model))
                        .collect()
                })
                .unwrap_or_default();
            matched.sort_by(|left, right| {
                version(right)
                    .cmp(&version(left))
                    .then_with(|| left.key().cmp(&right.key()))
            });
            PatternMatches {
                pattern: text.clone(),
                position: index + 1,
                models: matched.into_iter().map(ModelChoice::key).collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::models::Mode;

    fn model(
        provider: Provider,
        id: &str,
        resolved: Option<&str>,
        effort: Option<&str>,
    ) -> ModelChoice {
        ModelChoice {
            provider,
            id: Id::new(id).unwrap(),
            label: id.into(),
            mode: Mode::Fixed,
            resolved: resolved.map(|value| Id::new(value).unwrap()),
            effort: effort.map(|value| Id::new(value).unwrap()),
            observed_at_ms: 1,
        }
    }

    #[test]
    fn globs_match_runs_and_single_characters() {
        assert!(glob_matches("gpt-*-sol", "gpt-5-6-sol"));
        assert!(glob_matches("gpt-*-sol", "gpt-6-sol"));
        assert!(!glob_matches("gpt-*-sol", "gpt-6-sol-max"));
        assert!(glob_matches("*fable*", "claude-fable-5-1"));
        assert!(glob_matches("opus*", "opus[1m]"));
        assert!(!glob_matches("opus*", "claude-opus-5"));
        assert!(glob_matches("swe-?", "swe-2"));
        assert!(!glob_matches("swe-?", "swe-20"));
        assert!(glob_matches("*", ""));
        assert!(!glob_matches("a", ""));
    }

    #[test]
    fn patterns_match_normalized_identities_and_aliases() {
        let sol = RoutePattern::parse("codex/gpt-*-sol/ultra").unwrap();
        assert!(sol.matches(&model(Provider::Codex, "gpt-5.6-sol", None, Some("ultra"))));
        assert!(sol.matches(&model(Provider::Codex, "gpt-6.1-sol", None, Some("ultra"))));
        assert!(!sol.matches(&model(Provider::Codex, "gpt-5.6-sol", None, Some("max"))));
        assert!(!sol.matches(&model(Provider::Codex, "gpt-6-astra", None, Some("ultra"))));
        let opus = RoutePattern::parse("claude/opus*/max").unwrap();
        assert!(opus.matches(&model(Provider::Claude, "opus", None, Some("max"))));
        assert!(opus.matches(&model(Provider::Claude, "opus[1m]", None, Some("max"))));
        assert!(opus.matches(&model(
            Provider::Claude,
            "default",
            Some("claude-opus-5-5"),
            Some("max")
        )));
        assert!(!opus.matches(&model(Provider::Claude, "opus", None, Some("high"))));
        assert!(!opus.matches(&model(
            Provider::Claude,
            "default",
            Some("claude-sonnet-5"),
            Some("max")
        )));
        let fable = RoutePattern::parse("claude/*fable*/max").unwrap();
        assert!(fable.matches(&model(
            Provider::Claude,
            "claude-fable-5-1",
            None,
            Some("max")
        )));
        assert!(
            RoutePattern::parse("*/gpt-*-astra/*")
                .unwrap()
                .matches(&model(Provider::Codex, "gpt-6-astra", None, Some("high")))
        );
    }

    #[test]
    fn versions_order_newest_first_and_ignore_context_suffixes() {
        let v = |id| version(&model(Provider::Codex, id, None, None));
        assert!(v("gpt-6.1-sol") > v("gpt-6-sol"));
        assert!(v("gpt-6-sol") > v("gpt-5.6-sol"));
        assert_eq!(v("gpt-5.6-sol"), vec![5, 6]);
        assert_eq!(
            version(&model(Provider::Claude, "opus[1m]", None, Some("max"))),
            version(&model(Provider::Claude, "opus", None, Some("max")))
        );
    }

    #[test]
    fn malformed_patterns_name_the_segment() {
        let message = |text: &str| RoutePattern::parse(text).unwrap_err().to_string();
        assert!(message("codex").contains("provider/model-glob"));
        assert!(message("a/b/c/d").contains("provider/model-glob"));
        assert!(message("openai/gpt-*/high").contains("provider"));
        assert!(message("codex/GPT-*/high").contains("model glob"));
        assert!(message("codex//high").contains("model glob"));
        assert!(message("codex/gpt-*/turbo").contains("effort"));
        assert!(message("").contains("length"));
        assert!(RoutePattern::parse("codex/gpt-*-sol").is_ok());
        assert!(RoutePattern::parse("*/*/*").is_ok());
    }

    #[test]
    fn default_config_validates_and_limits_hold() {
        let config = RoutingConfig::default();
        config.validate().unwrap();
        assert!(config.never.is_empty());
        assert!(config.fallback_providers.is_empty());
        assert!(!config.excluded(&model(Provider::Codex, "gpt-6-astra-max", None, None)));
        assert!(!config.is_fallback(Provider::Claude));
        assert!(!config.is_fallback(Provider::Codex));
        assert_eq!(
            config.position(
                Tier::Default,
                &model(Provider::Codex, "gpt-6.1-sol", None, Some("ultra"))
            ),
            Some(0)
        );
        assert_eq!(
            config.position(
                Tier::Default,
                &model(Provider::Claude, "opus", None, Some("max"))
            ),
            Some(1)
        );
        assert_eq!(
            config.position(
                Tier::Default,
                &model(Provider::Codex, "gpt-6-astra", None, Some("ultra"))
            ),
            None
        );
        let mut long = RoutingConfig::default();
        long.tiers.meaty = vec!["codex/gpt-*/high".into(); MAX_PATTERNS + 1];
        assert!(long.validate().is_err());
        let mut bad = RoutingConfig::default();
        bad.never.push("devin/swe-*".into());
        assert!(bad.validate().is_err());
    }

    #[test]
    fn pattern_matches_list_newest_first() {
        let models = [
            model(Provider::Codex, "gpt-5.6-sol", None, Some("ultra")),
            model(Provider::Codex, "gpt-6.1-sol", None, Some("ultra")),
            model(Provider::Codex, "gpt-6.1-sol", None, Some("max")),
        ];
        let listed = pattern_matches(&["codex/gpt-*-sol/ultra".into()], &models);
        assert_eq!(listed[0].position, 1);
        assert_eq!(
            listed[0].models,
            ["codex/gpt-6.1-sol/ultra", "codex/gpt-5.6-sol/ultra"]
        );
    }
}
