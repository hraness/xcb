use crate::{
    Error, Result, auth,
    config::{Config, ReflexMode},
    judge, now_ms,
    offers::OfferState,
    process::Pin,
    reflex,
    routing_stack::{self, EXCLUDED_BY_NEVER, Tier},
    runner,
    store::{ModelCatalog, Store},
    summary, task_classifier,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
    policy::Failure,
    ui::AccountRow,
    usage::QuotaSpendingPressure,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    Routine,
    Balanced,
    Complex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub quality: u16,
    pub relative_cost: u16,
    pub relative_latency: u16,
    pub pareto_layer: u8,
    pub recognized: bool,
    pub free_offer: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfiledModel {
    pub key: String,
    pub label: String,
    pub provider: Provider,
    pub profile: ModelProfile,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteDecision {
    pub requirements: xcb_core::session::TaskRequirements,
    pub account: Id,
    pub model: ModelChoice,
    pub profile: ModelProfile,
    pub reason: String,
    /// The preference-stack tier the task was assigned (see [`assign_tier`]).
    pub tier: Tier,
    /// One-based position of the stack pattern the route matched; `None`
    /// when no pattern in the tier matched and the profile order decided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_position: Option<usize>,
    /// The route reflex decision behind the tier choice, when reflexes run.
    /// Callers that own a durable subject record it as an observation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reflex: Option<reflex::Decision>,
}

/// No admitted, enabled, credentialed account exists for the request; waiting
/// cannot fix this, the user must add or reconnect one.
pub const NO_CONNECTED_ACCOUNT: &str = "no eligible account; add or reconnect one (xcb accounts add <provider>, xcb doctor --provider <provider>, xcb accounts login <account>)";
/// Connected accounts exist but every route is busy, quota-blocked or
/// excluded for now; a later retry may succeed.
pub const NO_ELIGIBLE_ROUTE: &str =
    "no eligible admitted route; connect an account, finish active work, or wait for quota reset";
/// An otherwise matching admitted route has observed quota exhaustion, and no
/// eligible fallback exists. This does not claim that busy routes lack quota.
pub const NO_QUOTA_AVAILABLE_ROUTE: &str = "usage limits block a matching admitted route; no eligible fallback is available; wait for the reported quota reset or connect another account";

pub struct RouteRequest<'a> {
    pub requirements: xcb_core::session::TaskRequirements,
    pub task: &'a str,
    pub required_provider: Option<Provider>,
    pub preferred_provider: Option<Provider>,
    /// An exact observed model key restricts the route to that model alone.
    pub required_model: Option<&'a str>,
    pub excluded_routes: &'a BTreeSet<String>,
    pub excluded_accounts: &'a BTreeSet<Id>,
    pub account: Option<&'a Id>,
}

/// Ranked quota replacements and requirements discovered while judging them.
/// Persist requirements even if no replacement is available.
#[derive(Debug, Clone)]
pub struct FailoverRoutes {
    pub requirements: xcb_core::session::TaskRequirements,
    pub routes: Vec<FailoverRoute>,
}

#[derive(Debug, Clone)]
pub struct FailoverRoute {
    pub account: Id,
    pub model: ModelChoice,
}

/// What [`failover_routes`] needs to rank replacements for a route that
/// settled with a usage limit.
pub struct FailoverRequest<'a> {
    pub requirements: xcb_core::session::TaskRequirements,
    pub task: &'a str,
    /// The account and model the limited turn ran on.
    pub account: &'a Id,
    pub model: &'a ModelChoice,
    /// [`Failure::AccountQuota`] excludes every model on `account`;
    /// [`Failure::ModelQuota`] excludes only `model` there.
    pub failure: Failure,
    /// Routes this task already ran, as `<account>/<model key>`.
    pub tried: &'a BTreeSet<String>,
    /// Accounts that reported an account-wide limit earlier in this task.
    pub limited_accounts: &'a BTreeSet<Id>,
    /// A hard provider constraint, such as an opening "Use Claude" directive.
    pub required_provider: Option<Provider>,
    /// An exact model key the task is pinned to.
    pub required_model: Option<&'a str>,
    /// An explicitly selected account remains binding during quota failover.
    pub required_account: Option<&'a Id>,
}

#[derive(Clone)]
struct Candidate {
    account: Id,
    model: ModelChoice,
    profile: ModelProfile,
    utility: i32,
    quota_pressure: Option<QuotaSpendingPressure>,
    /// When a session last ran on this account (0 when none is recorded), so
    /// equal-score routes rotate across accounts instead of always picking
    /// the same one.
    last_used_ms: u64,
    /// Zero-based index of the first preference-stack pattern of the task's
    /// tier this route matches; `None` sorts after every match.
    stack: Option<usize>,
    /// Family version, newest first among routes one pattern matches.
    version: Vec<u64>,
}

impl Candidate {
    fn stack_rank(&self) -> usize {
        self.stack.unwrap_or(usize::MAX)
    }
}

/// Everything the router knows after ranking: the ordered shortlist and the
/// evidence a reason or a warning is written from.
struct Ranking {
    requirements: xcb_core::session::TaskRequirements,
    unavailable_reason: &'static str,
    candidates: Vec<Candidate>,
    classification: task_classifier::Classification,
    reflex: Option<reflex::Decision>,
    class: TaskClass,
    tier: Tier,
    models: Vec<ModelChoice>,
    catalog: ModelCatalog,
    connected: Vec<AccountRow>,
    offers: OfferState,
    now: u64,
}

fn effort(model: &ModelChoice) -> String {
    routing_stack::effort_of(model)
}

fn identity(model: &ModelChoice) -> String {
    routing_stack::identity(model)
}

/// Base (effort-independent) quality of a recognized model family.
pub(crate) fn family_quality(model: &ModelChoice) -> Option<u16> {
    let (quality, _, _, recognized) = family_profile(&identity(model));
    recognized.then_some(quality)
}

/// The numeric version of a versioned family name: `<prefix><digits>(-<digits>)*<suffix>`
/// followed by a family boundary, such as `gpt-6-1-sol` for `("gpt-", "-sol")`
/// or `claude-fable-5-2` for `("claude-fable-", "")`. A new release of a known
/// family is recognized without a table change.
fn family_version(identity: &str, prefix: &str, suffix: &str) -> Option<Vec<u64>> {
    let numeric =
        |segment: &str| !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit());
    let rest = identity.strip_prefix(prefix)?;
    let (version, tail) = if suffix.is_empty() {
        // The leading numeric segments are the version; a variant such as
        // `-fast` or a context suffix such as `[1m]` may follow.
        let main = &rest[..rest.find('[').unwrap_or(rest.len())];
        let mut end = 0;
        let mut cursor = 0;
        for segment in main.split('-') {
            if !numeric(segment) {
                break;
            }
            end = cursor + segment.len();
            cursor = end + 1;
        }
        (&rest[..end], &rest[end..])
    } else {
        rest.split_once(suffix)?
    };
    if !(tail.is_empty() || tail.starts_with('-') || tail.starts_with('[')) {
        return None;
    }
    let segments: Vec<u64> = version
        .split('-')
        .map(|segment| numeric(segment).then(|| segment.parse().ok()).flatten())
        .collect::<Option<_>>()?;
    (!segments.is_empty()).then_some(segments)
}

fn family_profile(identity: &str) -> (u16, u16, u16, bool) {
    let family = |name: &str| {
        identity == name
            || identity
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('-') || suffix.starts_with('['))
    };
    let versioned = |prefix: &str, suffix: &str| {
        family_version(identity, prefix, suffix)
            .or_else(|| family_version(identity, &format!("claude-{prefix}"), suffix))
    };
    if let Some(version) = family_version(identity, "gpt-", "-astra")
        && version >= vec![6]
    {
        (100, 92, 72, true)
    } else if family("claude-opus-5") || family("opus-5") {
        (98, 96, 82, true)
    } else if let Some(version) = versioned("fable-", "")
        && version >= vec![5, 1]
    {
        (97, 82, 68, true)
    } else if family("swe-2") {
        (96, 28, 45, true)
    } else if family("claude-sonnet-5") || family("sonnet-5") {
        (92, 55, 50, true)
    } else if let Some(version) = family_version(identity, "gpt-", "-sol")
        && version >= vec![5, 6]
    {
        // Sol 6.x sits between Sol 5.6 and Astra; a later major keeps the
        // 6.x profile until it is measured.
        if version >= vec![6] {
            (94, 60, 55, true)
        } else {
            (89, 45, 52, true)
        }
    } else if family("swe-1-7") {
        (82, 22, 30, true)
    } else if family("claude-haiku") || family("haiku") || family("gpt-5-6-luna") {
        (68, 10, 12, true)
    } else {
        (72, 60, 55, false)
    }
}

fn base_profile(model: &ModelChoice, offers: &OfferState, now: u64) -> ModelProfile {
    // A display label is not model identity. Resolved aliases take precedence
    // over their requested name, and family matching must respect boundaries.
    let identity = identity(model);
    let (mut quality, mut cost, mut latency, recognized) = family_profile(&identity);
    match effort(model).as_str() {
        "ultra" => {
            quality += 6;
            cost += 20;
            latency += 22;
        }
        "xhigh" => {
            quality += 4;
            cost += 14;
            latency += 16;
        }
        "max" => {
            quality += 5;
            cost += 17;
            latency += 20;
        }
        "high" => {
            quality += 2;
            cost += 8;
            latency += 8;
        }
        "low" => {
            quality = quality.saturating_sub(9);
            cost = cost.saturating_sub(8);
            latency = latency.saturating_sub(10);
        }
        "minimal" | "none" => {
            quality = quality.saturating_sub(15);
            cost = cost.saturating_sub(12);
            latency = latency.saturating_sub(15);
        }
        _ => (),
    }
    if identity.ends_with("-priority") || identity.ends_with("-fast") {
        cost = cost.saturating_add(8);
        latency = latency.saturating_sub(18);
    }
    let free_offer = offers
        .offer_for(model, now)
        .map(|offer| offer.terms.clone());
    // The pricing page describes a conditional promotion, not the account's
    // authenticated entitlement. Keep the observation visible without turning
    // unknown account pricing into zero or changing admission/ranking.
    ModelProfile {
        quality: quality.min(120),
        relative_cost: cost.min(120),
        relative_latency: latency.min(120),
        pareto_layer: 0,
        recognized,
        free_offer,
    }
}

fn dominates(left: &ModelProfile, right: &ModelProfile) -> bool {
    left.quality >= right.quality
        && left.relative_cost <= right.relative_cost
        && left.relative_latency <= right.relative_latency
        && (left.quality > right.quality
            || left.relative_cost < right.relative_cost
            || left.relative_latency < right.relative_latency)
}

fn assign_pareto_layers(rows: &mut [ProfiledModel]) {
    let mut remaining: BTreeSet<usize> = (0..rows.len()).collect();
    let mut layer = 1u8;
    while !remaining.is_empty() && layer <= 8 {
        let frontier: Vec<_> = remaining
            .iter()
            .copied()
            .filter(|index| {
                !remaining.iter().copied().any(|other| {
                    other != *index && dominates(&rows[other].profile, &rows[*index].profile)
                })
            })
            .collect();
        if frontier.is_empty() {
            break;
        }
        for index in frontier {
            rows[index].profile.pareto_layer = layer;
            remaining.remove(&index);
        }
        layer = layer.saturating_add(1);
    }
    for index in remaining {
        rows[index].profile.pareto_layer = 9;
    }
}

pub fn classify_task(task: &str) -> TaskClass {
    let lower = task.to_ascii_lowercase();
    let words: Vec<_> = lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let contains = |cue: &str| {
        let cue: Vec<_> = cue.split_whitespace().collect();
        words.windows(cue.len()).any(|window| window == cue)
    };
    if [
        "architecture",
        "migration",
        "security",
        "race",
        "concurrency",
        "redesign",
        "root cause",
        "adversarial",
        "refactor",
    ]
    .iter()
    .any(|cue| contains(cue))
    {
        TaskClass::Complex
    } else if [
        "format",
        "rename",
        "typo",
        "status",
        "summarize",
        "explain",
        "docs",
        "documentation",
    ]
    .iter()
    .any(|cue| contains(cue))
    {
        TaskClass::Routine
    } else {
        TaskClass::Balanced
    }
}

/// Only an initial affirmative directive can impose a hard provider choice.
/// Mentions in explanations, quotes, comparisons, and negative instructions
/// remain task content rather than silently changing routing authority.
pub fn explicit_provider_intent(task: &str) -> Option<Provider> {
    let lower = task.trim_start().to_ascii_lowercase();
    let directive = lower.strip_prefix("please ").unwrap_or(&lower);
    let remainder = directive.strip_prefix("use ")?.trim_start();
    Provider::SUPPORTED.into_iter().find(|provider| {
        remainder
            .strip_prefix(provider.as_str())
            .is_some_and(|tail| {
                tail.is_empty()
                    || tail.starts_with(['.', ',', ':', ';', '\n'])
                    || [" to ", " for ", " and "]
                        .iter()
                        .any(|prefix| tail.starts_with(prefix))
            })
    })
}

fn utility(class: TaskClass, profile: &ModelProfile) -> i32 {
    let quality = i32::from(profile.quality);
    let cost = i32::from(profile.relative_cost);
    let latency = i32::from(profile.relative_latency);
    let layer = i32::from(profile.pareto_layer.max(1));
    let base = match class {
        TaskClass::Routine => quality * 2 - cost * 3 - latency * 2,
        TaskClass::Balanced => quality * 4 - cost * 2 - latency,
        TaskClass::Complex => quality * 6 - cost - latency / 2,
    };
    base - (layer - 1) * 25
}

fn route_utility(
    class: TaskClass,
    profile: &ModelProfile,
    is_favorite: bool,
    is_preferred_provider: bool,
    quota_pressure: Option<&QuotaSpendingPressure>,
) -> i32 {
    utility(class, profile)
        + if is_favorite { 20 } else { 0 }
        + if is_preferred_provider { 30 } else { 0 }
        // At ten percentage points per hour the expiry preference reaches
        // its 200-point cap. It can outweigh ordinary cost preferences,
        // never the separately enforced quality tier or route constraints.
        // Unknown capacity receives no measured-budget preference.
        + quota_pressure
            .map(|pressure| (pressure.percent_per_hour * 20.0).round().clamp(0.0, 200.0) as i32)
            .unwrap_or(0)
}

pub fn profile_models(
    models: &[ModelChoice],
    offers: &OfferState,
    now: u64,
    task: &str,
) -> Vec<ProfiledModel> {
    let class = classify_task(task);
    profile_models_for_class(
        models,
        offers,
        now,
        class,
        task_classifier::substantial(task) || class == TaskClass::Complex,
        |_| None,
    )
}

/// `stack` is the preference-stack position of a model for the task's tier.
/// A model the stack names is always in the per-provider shortlist, ahead of
/// the quality and utility order that fills the rest of it.
fn profile_models_for_class(
    models: &[ModelChoice],
    offers: &OfferState,
    now: u64,
    class: TaskClass,
    frontier: bool,
    stack: impl Fn(&ModelChoice) -> Option<usize>,
) -> Vec<ProfiledModel> {
    let mut by_provider: BTreeMap<Provider, Vec<(usize, ProfiledModel)>> = BTreeMap::new();
    for model in models.iter().filter(|model| model.mode == Mode::Fixed) {
        by_provider.entry(model.provider).or_default().push((
            stack(model).unwrap_or(usize::MAX),
            ProfiledModel {
                key: model.key(),
                label: model.label.clone(),
                provider: model.provider,
                profile: base_profile(model, offers, now),
            },
        ));
    }
    let mut rows = Vec::new();
    for (_, mut provider) in by_provider {
        provider.sort_by(|(left_stack, left), (right_stack, right)| {
            left_stack
                .cmp(right_stack)
                .then_with(|| {
                    quality_priority(frontier, &right.profile)
                        .cmp(&quality_priority(frontier, &left.profile))
                })
                .then_with(|| utility(class, &right.profile).cmp(&utility(class, &left.profile)))
                .then_with(|| left.key.cmp(&right.key))
        });
        rows.extend(provider.into_iter().take(12).map(|(_, row)| row));
    }
    assign_pareto_layers(&mut rows);
    rows.sort_by(|left, right| {
        left.profile
            .pareto_layer
            .cmp(&right.profile.pareto_layer)
            .then_with(|| utility(class, &right.profile).cmp(&utility(class, &left.profile)))
            .then_with(|| left.provider.cmp(&right.provider))
            .then_with(|| left.key.cmp(&right.key))
    });
    rows
}

fn favorite(config: &Config, model: &ModelChoice) -> bool {
    config.favorites.iter().any(|item| {
        item.provider == model.provider && item.model == model.id && item.effort == model.effort
    })
}

fn selectable_model(model: &ModelChoice) -> bool {
    model.mode == Mode::Fixed
        && match model.provider {
            Provider::Codex => crate::codex::QUALIFIED_MODELS.contains(&model.id.as_str()),
            Provider::Claude => true,
            Provider::Devin => false,
        }
}

fn eligible_profiles(
    models: &[ModelChoice],
    offers: &OfferState,
    now: u64,
    task: &str,
    frontier: bool,
    eligible: impl Fn(&ModelChoice) -> bool,
    stack: impl Fn(&ModelChoice) -> Option<usize>,
) -> BTreeMap<String, ModelProfile> {
    // Exclusions must precede the bounded shortlist. Otherwise exhausting the
    // first twelve models makes every later catalog entry unreachable.
    let models: Vec<_> = models
        .iter()
        .filter(|model| selectable_model(model) && eligible(model))
        .cloned()
        .collect();
    profile_models_for_class(&models, offers, now, classify_task(task), frontier, stack)
        .into_iter()
        .map(|row| (row.key, row.profile))
        .collect()
}

fn admitted_providers(store: &Store) -> BTreeSet<Provider> {
    Provider::SUPPORTED
        .into_iter()
        .filter(|provider| {
            Pin::load(store.root(), *provider)
                .is_ok_and(|pin| runner::provider_admitted(store.root(), &pin))
        })
        .collect()
}

fn validate_requirement_pins(
    store: &Store,
    requirements: xcb_core::session::TaskRequirements,
    provider: Option<Provider>,
    model: Option<&str>,
    account: Option<&Id>,
) -> Result<()> {
    let incompatible_model = model
        .map(|key| {
            store.models().map(|models| {
                !models.iter().any(|model| {
                    (model.key() == key || model.id.as_str() == key || model.label == key)
                        && requirements.allows(model.provider)
                })
            })
        })
        .transpose()?
        .unwrap_or(false);
    if requirements.requires_codex() {
        let pinned_provider = match account {
            Some(id) => match store.account(id) {
                Ok(metadata) => Some(metadata.provider),
                // Storage/deserialization diagnostics can contain private
                // account values. Pin validation exposes a fixed public error.
                Err(_) => return Err(Error::Unavailable("pinned account could not be read")),
            },
            None => None,
        };
        if provider.is_some_and(|provider| !requirements.allows(provider))
            || incompatible_model
            || pinned_provider.is_some_and(|provider| !requirements.allows(provider))
        {
            return Err(Error::Conflict(
                "this task requires Codex; remove the incompatible provider, account, or model pin",
            ));
        }
    }
    Ok(())
}

pub async fn smart_route(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
) -> Result<RouteDecision> {
    crate::native_backend::require_execution(config, request.requirements)?;
    let mut admitted = admitted_providers(store);
    if request.requirements.native_execution {
        admitted.retain(|provider| {
            config.native_execution.scopes.iter().any(|scope| {
                scope.providers.contains(provider)
                    && crate::native_backend::require_provider_qualification(
                        store.root(),
                        *provider,
                        scope.github_credentials,
                    )
                    .is_ok()
            })
        });
    }
    route_with_admitted(store, config, request, &admitted).await
}

/// The routes that could replace a usage-limited one, best first: the same
/// filters and ranking as [`smart_route`] (supported build, signed in, idle,
/// no known usage limit, preference stack, quality tier, favorites, usage
/// pace, then least-recently-used account), with the failed route, every
/// route the task already tried, and any account that reported an
/// account-wide limit left out. The preference stack decides first, so after
/// an account limit the next pick is the same stack pattern on another
/// account, then the tier's next pattern. Among routes at the same stack
/// position, routes are grouped to keep the subscription rotation close to
/// the work: the same model on another account first (quality preserved),
/// then the same provider's other models, then other providers. A pinned
/// provider or model is a hard constraint and never widens. An empty list
/// `routes` means nothing can take the task now; the caller explains why from the
/// account view.
pub async fn failover_routes(
    store: &Store,
    config: &Config,
    request: FailoverRequest<'_>,
) -> Result<FailoverRoutes> {
    crate::native_backend::require_execution(config, request.requirements)?;
    let mut admitted = admitted_providers(store);
    if request.requirements.native_execution {
        admitted.retain(|provider| {
            config.native_execution.scopes.iter().any(|scope| {
                scope.providers.contains(provider)
                    && crate::native_backend::require_provider_qualification(
                        store.root(),
                        *provider,
                        scope.github_credentials,
                    )
                    .is_ok()
            })
        });
    }
    failover_routes_with_admitted(store, config, request, &admitted).await
}

async fn failover_routes_with_admitted(
    store: &Store,
    config: &Config,
    request: FailoverRequest<'_>,
    admitted: &BTreeSet<Provider>,
) -> Result<FailoverRoutes> {
    let FailoverRequest {
        requirements,
        task,
        account,
        model,
        failure,
        tried,
        limited_accounts,
        required_provider,
        required_model,
        required_account,
    } = request;
    if required_provider.is_some_and(|provider| provider != model.provider) {
        return Ok(FailoverRoutes {
            requirements,
            routes: vec![],
        });
    }
    // The kernel records tried routes as `<account>/<model key>`; the router
    // excludes `<model key> · <account>`. Account ids never contain `/`.
    let mut excluded_routes: BTreeSet<String> = tried
        .iter()
        .filter_map(|key| key.split_once('/'))
        .map(|(account, model)| format!("{model} · {account}"))
        .collect();
    excluded_routes.insert(format!("{} · {}", model.key(), account));
    let mut excluded_accounts = limited_accounts.clone();
    if failure == Failure::AccountQuota {
        excluded_accounts.insert(account.clone());
    }
    let ranking = match rank_with_admitted(
        store,
        config,
        RouteRequest {
            requirements,
            task,
            required_provider,
            preferred_provider: None,
            required_model,
            excluded_routes: &excluded_routes,
            excluded_accounts: &excluded_accounts,
            account: required_account,
        },
        admitted,
    )
    .await
    {
        Ok(ranking) => ranking,
        Err(Error::Unavailable(_)) => {
            return Ok(FailoverRoutes {
                requirements,
                routes: vec![],
            });
        }
        Err(error) => return Err(error),
    };
    Ok(failover_ranking(ranking, model))
}

fn failover_ranking(ranking: Ranking, model: &ModelChoice) -> FailoverRoutes {
    let mut candidates = ranking.candidates;
    // Stable: within each group the router's order (utility, Pareto layer,
    // least recently used account) is kept.
    let current_key = model.key();
    candidates.sort_by_key(|candidate| {
        (
            candidate.stack_rank(),
            candidate.model.provider != model.provider,
            candidate.model.key() != current_key,
        )
    });
    FailoverRoutes {
        requirements: ranking.requirements,
        routes: candidates
            .into_iter()
            .map(|candidate| FailoverRoute {
                account: candidate.account,
                model: candidate.model,
            })
            .collect(),
    }
}

async fn route_with_admitted(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
    admitted: &BTreeSet<Provider>,
) -> Result<RouteDecision> {
    let required_model = request.required_model;
    let excluded_routes = request.excluded_routes;
    let excluded_accounts = request.excluded_accounts;
    let Ranking {
        requirements,
        unavailable_reason,
        candidates,
        classification,
        reflex,
        class,
        tier,
        models,
        catalog,
        connected,
        offers,
        now,
    } = rank_with_admitted(store, config, request, admitted).await?;
    let candidate = candidates
        .into_iter()
        .next()
        .ok_or(Error::Unavailable(unavailable_reason))?;
    let warning = quota_degradation_warning(
        &models,
        &catalog,
        &connected,
        &offers,
        now,
        &candidate,
        classification.frontier,
        required_model,
        excluded_routes,
        excluded_accounts,
    );
    // A person reads this. Raw classifier scores and reflex generations stay
    // in the `reflex` decision, which callers record as an observation.
    let quota_reason = match &candidate.quota_pressure {
        Some(pressure) => format!(
            " · subscription budget {:.1}% in {} · resets in {}m · pace {:.2} points/h",
            pressure.remaining_percent,
            pressure.window,
            pressure.resets_at_ms.saturating_sub(now).div_ceil(60_000),
            pressure.percent_per_hour,
        ),
        None => " · quota timing unmeasured".into(),
    };
    let stack_position = candidate.stack.map(|index| index + 1);
    let stack_reason = match stack_position {
        Some(position) => format!(" · tier {tier} · stack #{position}"),
        None => format!(" · tier {tier} · no stack match"),
    };
    let reason = format!(
        "{}{} · {} tier · {} task · Pareto P{} · quality {} · relative cost {} · relative latency {}{}{}{}",
        warning.unwrap_or_default(),
        format_args!(
            "{}{}",
            classification.reason(),
            if requirements.requires_codex() {
                if requirements.desktop {
                    " · desktop access requires Codex"
                } else if requirements.signed_in_browser {
                    " · signed-in browser requires Codex"
                } else {
                    " · native computer tools require Codex"
                }
            } else {
                ""
            }
        ),
        if classification.frontier {
            "frontier"
        } else {
            "standard"
        },
        match class {
            TaskClass::Routine => "routine",
            TaskClass::Balanced => "balanced",
            TaskClass::Complex => "complex",
        },
        candidate.profile.pareto_layer,
        candidate.profile.quality,
        candidate.profile.relative_cost,
        candidate.profile.relative_latency,
        stack_reason,
        quota_reason,
        candidate
            .profile
            .free_offer
            .as_ref()
            .map(|_| " · public promotion, not confirmed for this account")
            .unwrap_or(""),
    );
    Ok(RouteDecision {
        requirements,
        account: candidate.account,
        model: candidate.model,
        profile: candidate.profile,
        reason,
        tier,
        stack_position,
        reflex,
    })
}

/// The preference-stack tier of a task. Signals that call for more capability
/// are checked first, then the routine keyword class, then the default:
///
/// - `buildout`: the judge answered, says the task warrants a frontier model,
///   and rated scope and difficulty at least 4 on their 1–5 scales; or,
///   without a judge answer, the prompt is substantial (at least 400 words or
///   8 KiB) and carries a complex keyword cue (architecture, migration,
///   security, race, concurrency, redesign, root cause, adversarial,
///   refactor).
/// - `meaty`: the classifier says frontier (the judge's answer, a substantial
///   prompt, or an active route reflex; without a judge, a complex keyword
///   cue), or the judge's kind is `resume`.
/// - `mechanical`: a routine keyword cue (format, rename, typo, status,
///   summarize, explain, docs, documentation) and none of the above.
/// - `default`: everything else.
pub(crate) fn assign_tier(
    class: TaskClass,
    classification: &task_classifier::Classification,
) -> Tier {
    let judged_frontier = classification.judged && classification.frontier;
    let big = |score: Option<f64>| score.is_some_and(|score| score >= 4.0);
    if judged_frontier && big(classification.scope) && big(classification.difficulty)
        || !classification.judged && classification.substantial && class == TaskClass::Complex
    {
        Tier::Buildout
    } else if classification.frontier || classification.kind.as_deref() == Some("resume") {
        Tier::Meaty
    } else if class == TaskClass::Routine {
        Tier::Mechanical
    } else {
        Tier::Default
    }
}

/// Rank eligible routes and retain discovered requirements even when none
/// remain. Callers must preserve those requirements before a later retry.
async fn rank_with_admitted(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
    admitted: &BTreeSet<Provider>,
) -> Result<Ranking> {
    let backend = if config.extensions.judge.enabled && !request.requirements.requires_codex() {
        judge::resolve(store.root(), &config.extensions.judge)
            .ok()
            .flatten()
    } else {
        None
    };
    rank_with_judge(store, config, request, admitted, backend.as_deref()).await
}

async fn rank_with_judge(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
    admitted: &BTreeSet<Provider>,
    backend: Option<&dyn judge::Judge>,
) -> Result<Ranking> {
    let RouteRequest {
        mut requirements,
        task,
        required_provider,
        preferred_provider,
        required_model,
        excluded_routes,
        excluded_accounts,
        account: account_hint,
    } = request;
    validate_requirement_pins(
        store,
        requirements,
        required_provider,
        required_model,
        account_hint,
    )?;
    let now = now_ms();
    let offers = crate::offers::load(store.root()).unwrap_or_default();
    // A model pairs only with accounts whose own catalog (or, before an
    // account reports one, the provider-wide fallback) contains it.
    let catalog = store.model_catalog()?;
    let models = catalog.union();
    let view = summary::snapshot(store, None, config, now)?;
    // A connected account is admitted, enabled, credentialed and not waiting
    // for reconnection. Without one, no wait or quota reset can help: the
    // user must add or reconnect an account, and the supervisor says so.
    let connected: Vec<AccountRow> = view
        .accounts
        .iter()
        .filter(|account| {
            admitted.contains(&account.provider)
                && requirements.allows(account.provider)
                && required_provider.is_none_or(|provider| provider == account.provider)
                && account.enabled
                && !account.authentication_required
                && account_hint.is_none_or(|hint| hint == &account.id)
                && auth::has_credentials(store, &account.id).unwrap_or(false)
        })
        .cloned()
        .collect();
    // The sessions view is ordered by recent activity, so the newest session
    // per account is the account's last use; an absent account ranks first.
    let mut last_used: BTreeMap<Id, u64> = BTreeMap::new();
    for session in &view.sessions {
        let entry = last_used.entry(session.account.clone()).or_default();
        *entry = (*entry).max(session.last_active_at_ms);
    }
    let accounts: Vec<_> = connected
        .iter()
        .filter(|account| {
            account.active_runs < config.max_runs_per_account
                && account.quota_blocked_until_ms.is_none()
                && account
                    .remaining_percent
                    .is_none_or(|remaining| remaining > 0.0)
                && !excluded_accounts.contains(&account.id)
        })
        .collect();
    let unavailable_reason = || {
        unavailable_route_reason(
            &models,
            &catalog,
            &connected,
            now,
            required_model,
            excluded_routes,
            excluded_accounts,
        )
    };
    if accounts.is_empty() {
        return Err(Error::Unavailable(unavailable_reason()));
    }
    let quota_pressure: BTreeMap<_, _> = accounts
        .iter()
        .map(|account| {
            Ok((
                account.id.clone(),
                store.quota_spending_pressure(&account.id, now)?,
            ))
        })
        .collect::<Result<_>>()?;
    let class = classify_task(task);
    let mut classification = task_classifier::classify(
        task,
        backend,
        class == TaskClass::Complex,
        class == TaskClass::Routine,
    )
    .await;
    requirements.signed_in_browser |= classification.signed_in_browser == Some(true);
    requirements.desktop |= classification.desktop == Some(true);
    validate_requirement_pins(
        store,
        requirements,
        required_provider,
        required_model,
        account_hint,
    )?;
    let reflex = route_reflex(store.root(), config, &mut classification).await;
    let stack = &config.routing;
    // A pinned model the owner excluded is refused, never widened.
    if let Some(key) = required_model
        && models
            .iter()
            .any(|model| model.key() == key && stack.excluded(model))
    {
        return Err(xcb_core::Error::Invalid(EXCLUDED_BY_NEVER).into());
    }
    let tier = assign_tier(class, &classification);
    let profile_by_key = eligible_profiles(
        &models,
        &offers,
        now,
        task,
        classification.frontier && !requirements.requires_codex(),
        |model| {
            requirements.allows(model.provider)
                && required_model.is_none_or(|key| model.key() == key)
                && !stack.excluded(model)
                && accounts.iter().any(|account| {
                    catalog.offers(&account.id, account.provider, model)
                        && !excluded_routes.contains(&format!("{} · {}", model.key(), account.id))
                })
        },
        |model| stack.position(tier, model),
    );
    let mut candidates = Vec::new();
    for model in &models {
        let Some(profile) = profile_by_key.get(&model.key()).cloned() else {
            continue;
        };
        let position = stack.position(tier, model);
        for account in &accounts {
            let route_key = format!("{} · {}", model.key(), account.id);
            if !catalog.offers(&account.id, account.provider, model)
                || excluded_routes.contains(&route_key)
            {
                continue;
            }
            let score = route_utility(
                class,
                &profile,
                favorite(config, model),
                preferred_provider == Some(model.provider),
                quota_pressure.get(&account.id).and_then(Option::as_ref),
            );
            candidates.push(Candidate {
                account: account.id.clone(),
                model: model.clone(),
                profile: profile.clone(),
                utility: score,
                quota_pressure: quota_pressure.get(&account.id).cloned().flatten(),
                last_used_ms: last_used.get(&account.id).copied().unwrap_or(0),
                stack: position,
                version: position
                    .map(|_| routing_stack::version(model))
                    .unwrap_or_default(),
            });
        }
    }
    // Browser and desktop operations prefer Astra independently of coding-task
    // tier preferences, but never widen eligibility or override exact pins.
    if requirements.requires_codex()
        && candidates
            .iter()
            .any(|candidate| candidate.model.id.as_str().contains("-astra"))
    {
        candidates.retain(|candidate| candidate.model.id.as_str().contains("-astra"));
    }
    // A fallback provider's routes count only when nothing else can take the
    // task now.
    if candidates
        .iter()
        .any(|candidate| !stack.is_fallback(candidate.model.provider))
    {
        candidates.retain(|candidate| !stack.is_fallback(candidate.model.provider));
    }
    // The preference stack decides quality for the routes it names. Among
    // the rest, an expensive/favored route or an external judgment cannot
    // displace the strongest known eligible quality tier when the task
    // demands frontier.
    let (matched, mut unmatched): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|candidate| candidate.stack.is_some());
    retain_quality_tier(
        &mut unmatched,
        classification.frontier && !requirements.requires_codex(),
    );
    let mut candidates = matched;
    candidates.extend(unmatched);
    // The stack position decides first, then the newest family version among
    // routes one pattern matches. Score, then Pareto layer, decide the rest;
    // among routes they leave tied the account that ran a session longest
    // ago goes first, so subscriptions rotate instead of the lowest id always
    // winning. Quality tiers, favorites, pins and usage pace are all inside
    // the score and are never overridden by recency.
    candidates.sort_by(|left, right| {
        left.stack_rank()
            .cmp(&right.stack_rank())
            .then_with(|| right.version.cmp(&left.version))
            .then_with(|| right.utility.cmp(&left.utility))
            .then_with(|| left.profile.pareto_layer.cmp(&right.profile.pareto_layer))
            .then_with(|| left.last_used_ms.cmp(&right.last_used_ms))
            .then_with(|| left.model.key().cmp(&right.model.key()))
            .then_with(|| left.account.cmp(&right.account))
    });
    candidates.dedup_by(|left, right| {
        left.account == right.account && left.model.key() == right.model.key()
    });
    let mut provider_counts: BTreeMap<Provider, usize> = BTreeMap::new();
    candidates.retain(|candidate| {
        let count = provider_counts.entry(candidate.model.provider).or_default();
        if *count >= 3 {
            false
        } else {
            *count += 1;
            true
        }
    });
    candidates.truncate(8);
    let unavailable_reason = unavailable_reason();
    Ok(Ranking {
        requirements,
        unavailable_reason,
        candidates,
        classification,
        reflex,
        class,
        tier,
        models,
        catalog,
        connected,
        offers,
        now,
    })
}

/// Runs the route reflex over the classifier's evidence. In `active` mode its
/// decision sets the tier; in `observe` mode it is recorded beside the
/// unchanged classifier decision. Any reflex failure keeps the classifier's
/// decision: learned policy can refine routing, never break it.
async fn route_reflex(
    root: &std::path::Path,
    config: &Config,
    classification: &mut task_classifier::Classification,
) -> Option<reflex::Decision> {
    let mode = config.extensions.reflexes.route;
    if mode == ReflexMode::Off {
        return None;
    }
    let store = reflex::ReflexStore::open(root).ok()?;
    let decision = store
        .decide(
            xcb_core::reflex::Reflex::Route,
            &classification.features,
            classification.evidence(),
            false,
        )
        .await
        .ok()?;
    if mode == ReflexMode::Active {
        // A replaced program cannot demote the substantial-prompt floor.
        classification.frontier = decision.value == "frontier" || classification.substantial;
    }
    Some(decision)
}

/// Unknown identities never acquire an invented frontier rank. When none of
/// the observed routes has a known profile, ordinary utility remains the fallback.
fn quality_priority(frontier: bool, profile: &ModelProfile) -> (bool, u16) {
    if frontier && profile.recognized {
        (true, profile.quality)
    } else {
        (false, 0)
    }
}

fn retain_quality_tier(candidates: &mut Vec<Candidate>, frontier: bool) {
    if frontier
        && let Some(quality) = candidates
            .iter()
            .filter(|candidate| candidate.profile.recognized)
            .map(|candidate| candidate.profile.quality)
            .max()
    {
        candidates.retain(|candidate| {
            candidate.profile.recognized && candidate.profile.quality == quality
        });
    }
}

/// `connected` has already applied provider/account constraints, admission,
/// enabled state and current credential health. Do not infer a quota failure
/// merely from an excluded, busy, unknown or unobserved route.
fn unavailable_route_reason(
    models: &[ModelChoice],
    catalog: &ModelCatalog,
    connected: &[AccountRow],
    now: u64,
    required_model: Option<&str>,
    excluded_routes: &BTreeSet<String>,
    excluded_accounts: &BTreeSet<Id>,
) -> &'static str {
    if connected.is_empty() {
        NO_CONNECTED_ACCOUNT
    } else if models.iter().any(|model| {
        selectable_model(model)
            && required_model.is_none_or(|key| model.key() == key)
            && quota_blocks_model(
                model,
                catalog,
                connected,
                now,
                excluded_routes,
                excluded_accounts,
            )
    }) {
        NO_QUOTA_AVAILABLE_ROUTE
    } else {
        NO_ELIGIBLE_ROUTE
    }
}

fn quota_blocks_model(
    model: &ModelChoice,
    catalog: &ModelCatalog,
    connected: &[AccountRow],
    now: u64,
    excluded_routes: &BTreeSet<String>,
    excluded_accounts: &BTreeSet<Id>,
) -> bool {
    connected.iter().any(|account| {
        catalog.offers(&account.id, account.provider, model)
            && !excluded_accounts.contains(&account.id)
            && !excluded_routes.contains(&format!("{} · {}", model.key(), account.id))
            && (account
                .quota_blocked_until_ms
                .is_some_and(|until| until > now)
                || account
                    .remaining_percent
                    .is_some_and(|remaining| remaining <= 0.0))
    })
}

#[allow(clippy::too_many_arguments)]
fn quota_degradation_warning(
    models: &[ModelChoice],
    catalog: &ModelCatalog,
    connected: &[AccountRow],
    offers: &OfferState,
    now: u64,
    selected: &Candidate,
    frontier: bool,
    required_model: Option<&str>,
    excluded_routes: &BTreeSet<String>,
    excluded_accounts: &BTreeSet<Id>,
) -> Option<String> {
    if !frontier {
        return None;
    }
    let blocked = models
        .iter()
        .filter(|model| {
            selectable_model(model)
                && required_model.is_none_or(|key| model.key() == key)
                && quota_blocks_model(
                    model,
                    catalog,
                    connected,
                    now,
                    excluded_routes,
                    excluded_accounts,
                )
        })
        .filter_map(|model| {
            let profile = base_profile(model, offers, now);
            (profile.recognized
                && (!selected.profile.recognized || profile.quality > selected.profile.quality))
                .then_some((model, profile.quality))
        })
        .max_by_key(|(_, quality)| *quality)?;
    Some(format!(
        "Warning: usage limits block higher-ranked {} · using best eligible route · ",
        xcb_core::display_text(&blocked.0.key(), 96)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::models::Mode;

    #[test]
    fn browser_pin_lookup_exposes_only_fixed_errors_for_missing_and_corrupt_accounts() {
        let root = tempfile::tempdir().unwrap();
        let root_path = xcb_core::canonical(root.path()).unwrap();
        let store = Store::open(&root_path.join("state")).unwrap();
        let selected = store
            .add_account(Provider::Codex, "fixture", 1, None)
            .unwrap();
        let requirements = xcb_core::session::TaskRequirements {
            signed_in_browser: true,
            ..Default::default()
        };
        assert!(
            validate_requirement_pins(&store, requirements, None, None, Some(&selected.id)).is_ok()
        );
        let missing = Id::new("missing-account").unwrap();
        let missing_error =
            validate_requirement_pins(&store, requirements, None, None, Some(&missing))
                .unwrap_err();
        assert!(matches!(
            missing_error,
            Error::Unavailable("pinned account could not be read")
        ));
        let db = rusqlite::Connection::open(store.root().join("xcb.sqlite")).unwrap();
        db.execute(
            "UPDATE accounts SET payload = ?1 WHERE id = ?2",
            rusqlite::params![
                r#"{"provider":"private-corrupt-account-value"}"#,
                selected.id.as_str()
            ],
        )
        .unwrap();
        assert!(store.account(&selected.id).is_err());
        let corrupt_error =
            validate_requirement_pins(&store, requirements, None, None, Some(&selected.id))
                .unwrap_err();
        assert!(matches!(
            corrupt_error,
            Error::Unavailable("pinned account could not be read")
        ));
        assert!(
            !corrupt_error
                .to_string()
                .contains("private-corrupt-account-value")
        );
    }

    struct DesktopFailoverJudge;
    impl judge::Judge for DesktopFailoverJudge {
        fn ask<'a>(
            &'a self,
            _: &'a serde_json::Value,
            _: &'a judge::JudgeQuestions,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<judge::JudgeAnswers>> + Send + 'a>,
        > {
            Box::pin(async {
                Ok(judge::JudgeAnswers {
                    model: None,
                    answers: BTreeMap::from([(
                        "desktop".into(),
                        judge::JudgeAnswer::Choice {
                            choice: "required".into(),
                            confidence: 1.0,
                            probabilities: BTreeMap::from([("required".into(), 1.0)]),
                        },
                    )]),
                })
            })
        }
    }

    #[tokio::test]
    async fn desktop_discovered_during_failover_persists_even_without_a_candidate() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let root_path = xcb_core::canonical(root.path()).unwrap();
        let store = Store::open(&root_path.join("state")).unwrap();
        let workspace = root_path.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let codex = account(&store, Provider::Codex);
        let claude = account(&store, Provider::Claude);
        let codex_model = model(Provider::Codex, "gpt-6-astra", None, Some("ultra"));
        let claude_model = model(Provider::Claude, "claude-opus-4-8", None, None);
        store.set_models(Provider::Codex, &[codex_model]).unwrap();
        store
            .set_models(Provider::Claude, std::slice::from_ref(&claude_model))
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let admitted = Provider::SUPPORTED.into_iter().collect();
        let routes = BTreeSet::new();
        let accounts = BTreeSet::new();
        for unavailable in [false, true] {
            let excluded = if unavailable {
                BTreeSet::from([codex.clone()])
            } else {
                BTreeSet::new()
            };
            let session = store
                .create_session(&claude, claude_model.clone(), &workspace, now_ms())
                .unwrap();
            assert_eq!(session.requirements, Default::default());
            let ranking = rank_with_judge(
                &store,
                &config,
                RouteRequest {
                    requirements: session.requirements,
                    task: "inspect the current application",
                    required_provider: None,
                    preferred_provider: None,
                    required_model: None,
                    excluded_routes: &routes,
                    excluded_accounts: &excluded,
                    account: None,
                },
                &admitted,
                Some(&DesktopFailoverJudge),
            )
            .await
            .unwrap();
            let ranked = failover_ranking(ranking, &claude_model);
            assert_eq!(ranked.routes.is_empty(), unavailable);
            assert!(
                ranked
                    .routes
                    .iter()
                    .all(|route| route.model.provider == Provider::Codex)
            );
            store
                .require_session_capabilities(&session.id, ranked.requirements)
                .unwrap();
            let saved = store.session(&session.id).unwrap().unwrap().requirements;
            assert!(saved.desktop && !saved.signed_in_browser && !saved.codex_native);
            let resumed = route_with_admitted(
                &store,
                &config,
                RouteRequest {
                    requirements: saved,
                    task: "continue",
                    required_provider: None,
                    preferred_provider: Some(Provider::Claude),
                    required_model: None,
                    excluded_routes: &routes,
                    excluded_accounts: &accounts,
                    account: None,
                },
                &admitted,
            )
            .await
            .unwrap();
            assert_eq!(resumed.model.provider, Provider::Codex);
            assert!(resumed.requirements.desktop);
        }
    }

    #[tokio::test]
    async fn computer_requirements_requirement_filters_prefers_and_survives_model_fallback() {
        for requirements in [
            xcb_core::session::TaskRequirements {
                signed_in_browser: true,
                ..Default::default()
            },
            xcb_core::session::TaskRequirements {
                desktop: true,
                ..Default::default()
            },
            xcb_core::session::TaskRequirements {
                codex_native: true,
                ..Default::default()
            },
        ] {
            use crate::authentication_tests::account;
            let root = tempfile::tempdir().unwrap();
            let root_path = xcb_core::canonical(root.path()).unwrap();
            let store = Store::open(&root_path.join("state")).unwrap();
            let codex = account(&store, Provider::Codex);
            account(&store, Provider::Claude);
            let astra = model(Provider::Codex, "gpt-6-astra", None, Some("ultra"));
            let sol = model(Provider::Codex, "gpt-6-sol", None, Some("high"));
            store
                .set_models(Provider::Codex, &[astra.clone(), sol.clone()])
                .unwrap();
            store
                .set_models(
                    Provider::Claude,
                    &[model(Provider::Claude, "claude-opus-4-8", None, None)],
                )
                .unwrap();
            let routes = BTreeSet::new();
            let accounts = BTreeSet::new();
            let admitted = Provider::SUPPORTED.into_iter().collect();
            let request = || RouteRequest {
                requirements,
                task: "read this dashboard",
                required_provider: None,
                preferred_provider: Some(Provider::Claude),
                required_model: None,
                excluded_routes: &routes,
                excluded_accounts: &accounts,
                account: None,
            };
            let first = route_with_admitted(&store, &Config::default(), request(), &admitted)
                .await
                .unwrap();
            assert_eq!(first.account, codex);
            assert_eq!(first.model.key(), astra.key());
            assert_eq!(first.requirements, requirements);
            let excluded = [format!("{} · {}", astra.key(), codex)].into();
            let fallback = route_with_admitted(
                &store,
                &Config::default(),
                RouteRequest {
                    excluded_routes: &excluded,
                    ..request()
                },
                &admitted,
            )
            .await
            .unwrap();
            assert_eq!(fallback.model.key(), sol.key());
            assert_eq!(fallback.requirements, requirements);
            assert!(
                route_with_admitted(
                    &store,
                    &Config::default(),
                    RouteRequest {
                        required_provider: Some(Provider::Claude),
                        ..request()
                    },
                    &admitted
                )
                .await
                .is_err()
            );
            let empty = Store::open(&root_path.join("empty")).unwrap();
            account(&empty, Provider::Claude);
            empty
                .set_models(
                    Provider::Claude,
                    &[model(Provider::Claude, "claude-opus-4-8", None, None)],
                )
                .unwrap();
            assert!(
                route_with_admitted(&empty, &Config::default(), request(), &admitted)
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn computer_requirements_metadata_is_monotonic_backwards_compatible_and_guards_rebind() {
        for requirements in [
            xcb_core::session::TaskRequirements {
                signed_in_browser: true,
                ..Default::default()
            },
            xcb_core::session::TaskRequirements {
                desktop: true,
                ..Default::default()
            },
            xcb_core::session::TaskRequirements {
                codex_native: true,
                ..Default::default()
            },
        ] {
            use crate::authentication_tests::{account, model};
            let root = tempfile::tempdir().unwrap();
            let root_path = xcb_core::canonical(root.path()).unwrap();
            let workspace = root_path.join("workspace");
            std::fs::create_dir(&workspace).unwrap();
            let store = Store::open(&root_path.join("state")).unwrap();
            let codex = account(&store, Provider::Codex);
            let claude = account(&store, Provider::Claude);
            let session = store
                .create_session(&codex, model(Provider::Codex), &workspace, now_ms())
                .unwrap();
            let mut old = serde_json::to_value(&session).unwrap();
            old.as_object_mut().unwrap().remove("requirements");
            old.as_object_mut().unwrap().remove("route_pins");
            assert!(
                serde_json::from_value::<xcb_core::session::Session>(old)
                    .unwrap()
                    .requirements
                    .is_empty()
            );
            let required = store
                .require_session_capabilities(&session.id, requirements)
                .unwrap();
            assert_eq!(required.revision, session.revision);
            assert!(
                store
                    .require_session_capabilities(&session.id, Default::default())
                    .unwrap()
                    .requirements
                    == requirements
            );
            assert!(
                store
                    .rebind(
                        &session.id,
                        required.revision,
                        &claude,
                        model(Provider::Claude)
                    )
                    .is_err()
            );
            assert_eq!(store.session(&session.id).unwrap().unwrap().account, codex);
        }
    }

    #[tokio::test]
    async fn no_fallback_reports_observed_quota_only_within_requested_routes() {
        use crate::authentication_tests::{account, fail_authentication};
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let codex = account(&store, Provider::Codex);
        let frontier = model(Provider::Codex, "gpt-6-astra", None, Some("ultra"));
        let key = frontier.key();
        store.set_models(Provider::Codex, &[frontier]).unwrap();
        let now = now_ms().saturating_sub(1);
        let quota = xcb_core::usage::QuotaPoint {
            pool: store.account(&codex).unwrap().quota_pool,
            window: Id::new("codex.primary").unwrap(),
            used_percent: 100.0,
            observed_at_ms: now,
            resets_at_ms: now + 60_000,
        };
        store.record_quota(&quota).unwrap();
        let config = Config::default();
        let prompt = "details ".repeat(400);
        let routes = BTreeSet::new();
        let accounts = BTreeSet::new();
        let admitted = [Provider::Codex, Provider::Claude].into();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: &prompt,
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &routes,
            excluded_accounts: &accounts,
            account: None,
        };
        async fn reason(
            store: &Store,
            config: &Config,
            request: RouteRequest<'_>,
            admitted: &BTreeSet<Provider>,
        ) -> &'static str {
            match route_with_admitted(store, config, request, admitted).await {
                Err(Error::Unavailable(reason)) => reason,
                _ => panic!("expected unavailable route"),
            }
        }
        // No available account: the early return retains observed exhaustion.
        assert_eq!(
            reason(&store, &config, request(), &admitted).await,
            NO_QUOTA_AVAILABLE_ROUTE
        );
        // A connected account without an observed model cannot provide a
        // fallback. The empty-candidate return retains the same diagnosis.
        let claude = account(&store, Provider::Claude);
        assert_eq!(
            reason(&store, &config, request(), &admitted).await,
            NO_QUOTA_AVAILABLE_ROUTE
        );
        assert_eq!(
            reason(
                &store,
                &config,
                RouteRequest {
                    requirements: Default::default(),
                    required_model: Some(&key),
                    account: Some(&codex),
                    ..request()
                },
                &admitted
            )
            .await,
            NO_QUOTA_AVAILABLE_ROUTE
        );
        for constrained in [
            RouteRequest {
                requirements: Default::default(),
                required_model: Some("codex/unobserved-model"),
                ..request()
            },
            RouteRequest {
                requirements: Default::default(),
                required_provider: Some(Provider::Claude),
                ..request()
            },
            RouteRequest {
                requirements: Default::default(),
                account: Some(&claude),
                ..request()
            },
        ] {
            assert_eq!(
                reason(&store, &config, constrained, &admitted).await,
                NO_ELIGIBLE_ROUTE
            );
        }
        let excluded_routes = BTreeSet::from([format!("{key} · {codex}")]);
        assert_eq!(
            reason(
                &store,
                &config,
                RouteRequest {
                    requirements: Default::default(),
                    excluded_routes: &excluded_routes,
                    ..request()
                },
                &admitted
            )
            .await,
            NO_ELIGIBLE_ROUTE
        );
        let excluded_accounts = BTreeSet::from([codex.clone()]);
        assert_eq!(
            reason(
                &store,
                &config,
                RouteRequest {
                    requirements: Default::default(),
                    excluded_accounts: &excluded_accounts,
                    ..request()
                },
                &admitted
            )
            .await,
            NO_ELIGIBLE_ROUTE
        );
        assert_eq!(
            reason(&store, &config, request(), &[Provider::Claude].into()).await,
            NO_ELIGIBLE_ROUTE
        );
        // Authentication failure wins over old quota evidence. A new usable
        // observation permits the synthetic failure turn, then exhaustion is
        // observed again; the account is still not a connected route.
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                used_percent: 0.0,
                observed_at_ms: now_ms(),
                ..quota.clone()
            })
            .unwrap();
        fail_authentication(&store, &codex);
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                window: Id::new("codex.secondary").unwrap(),
                observed_at_ms: now_ms(),
                ..quota
            })
            .unwrap();
        assert_eq!(
            reason(&store, &config, request(), &admitted).await,
            NO_ELIGIBLE_ROUTE
        );
    }

    /// The reason is for a person: task class, capability tier and the
    /// relative profile, without raw classifier scores or reflex generations.
    /// A public promotion only annotates the route; it never changes which
    /// route wins or its quality, cost and latency.
    #[tokio::test]
    async fn route_reason_is_readable_and_a_public_promotion_never_changes_the_route() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let claude = account(&store, Provider::Claude);
        store
            .set_models(
                Provider::Claude,
                &[
                    model(Provider::Claude, "swe-2-high", None, None),
                    model(Provider::Claude, "gpt-6-astra-medium", None, None),
                    model(Provider::Claude, "swe-1-7-fast", None, None),
                ],
            )
            .unwrap();
        let config = Config::default();
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let admitted = [Provider::Claude].into();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: "In add.js the add function subtracts; change it so it adds.",
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: Some(&claude),
        };
        let plain = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        for fact in [
            " tier · ",
            " task · Pareto P",
            " · quality ",
            " · relative cost ",
            " · relative latency ",
        ] {
            assert!(plain.reason.contains(fact), "{}", plain.reason);
        }
        for internal in ["score", "route reflex", " g0", "(score)", "observed"] {
            assert!(!plain.reason.contains(internal), "{}", plain.reason);
        }
        assert!(
            plain.reflex.is_some(),
            "the reflex decision is still returned"
        );
        let now = now_ms();
        let offers = OfferState {
            version: 1,
            checked_at_ms: now,
            next_check_ms: now + 1,
            source_sha256: "a".repeat(64),
            offers: vec![crate::offers::ModelOffer {
                provider: Provider::Devin,
                model_prefix: "swe-2-".into(),
                surface: "devin_cli".into(),
                kind: crate::offers::OfferKind::Free,
                terms: "Free use of SWE-2 Free in Devin Desktop and CLI through October 10, 2026"
                    .into(),
                valid_until_ms: 1_791_676_800_000,
                source: "https://devin.ai/pricing".into(),
            }],
        };
        crate::private::create(
            &store.root().join("offers.json"),
            &serde_json::to_vec(&offers).unwrap(),
        )
        .unwrap();
        let promoted = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert_eq!(promoted.account, plain.account);
        assert_eq!(promoted.model.key(), plain.model.key());
        assert_eq!(promoted.profile.quality, plain.profile.quality);
        assert_eq!(promoted.profile.relative_cost, plain.profile.relative_cost);
        assert_eq!(
            promoted.profile.relative_latency,
            plain.profile.relative_latency
        );
        assert_eq!(promoted.profile.pareto_layer, plain.profile.pareto_layer);
        // Before the promotion ends it is named, never as free use.
        assert!(promoted.reason.starts_with(&plain.reason));
        if promoted.profile.free_offer.is_some() {
            assert!(
                promoted
                    .reason
                    .ends_with(" · public promotion, not confirmed for this account")
            );
        }
        assert!(!promoted.reason.contains("free"));
    }

    #[tokio::test]
    async fn large_prompt_selects_best_known_quality_before_cost_favorites_and_shortlist() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let claude = account(&store, Provider::Claude);
        let mut models: Vec<_> = (0..20)
            .map(|n| model(Provider::Claude, &format!("swe-2-variant-{n}"), None, None))
            .collect();
        models.push(model(Provider::Claude, "gpt-6-astra-ultra", None, None));
        store.set_models(Provider::Claude, &models).unwrap();
        let mut config = Config::default();
        config.routing.never.clear();
        config.favorites.insert(
            0,
            xcb_core::models::Preference {
                provider: Provider::Claude,
                model: models[0].id.clone(),
                effort: None,
            },
        );
        let prompt = "details ".repeat(400);
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let admitted = [Provider::Claude].into();
        let request = |required_model| RouteRequest {
            requirements: Default::default(),
            task: &prompt,
            required_provider: Some(Provider::Claude),
            preferred_provider: Some(Provider::Claude),
            required_model,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: Some(&claude),
        };
        let selected = route_with_admitted(&store, &config, request(None), &admitted)
            .await
            .unwrap();
        assert_eq!(selected.model.id.as_str(), "gpt-6-astra-ultra");
        assert!(selected.reason.contains("large prompt"));
        let key = models[0].key();
        let explicit = route_with_admitted(&store, &config, request(Some(&key)), &admitted)
            .await
            .unwrap();
        assert_eq!(explicit.model.key(), key);
        assert!(!explicit.reason.contains("Warning"));
        // Once the exact route has been excluded, fallback remains reachable.
        let excluded = BTreeSet::from([format!("{} · {}", selected.model.key(), claude)]);
        let next = route_with_admitted(
            &store,
            &config,
            RouteRequest {
                requirements: Default::default(),
                excluded_routes: &excluded,
                ..request(None)
            },
            &admitted,
        )
        .await
        .unwrap();
        assert!(next.model.id.as_str().starts_with("swe-2-"));
    }

    #[tokio::test]
    async fn quota_warning_requires_quota_evidence_from_connected_admitted_routes() {
        use crate::authentication_tests::{account, fail_authentication};
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let codex = account(&store, Provider::Codex);
        account(&store, Provider::Claude);
        store
            .set_models(
                Provider::Codex,
                &[model(Provider::Codex, "gpt-6-astra", None, Some("ultra"))],
            )
            .unwrap();
        store
            .set_models(
                Provider::Claude,
                &[model(Provider::Claude, "gpt-6-astra-medium", None, None)],
            )
            .unwrap();
        let config = Config::default();
        let prompt = "details ".repeat(400);
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: &prompt,
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: None,
        };
        let admitted = [Provider::Codex, Provider::Claude].into();
        let work =
            crate::private::directory(&xcb_core::canonical(root.path()).unwrap().join("work"))
                .unwrap();
        let session = store
            .create_session(
                &codex,
                model(Provider::Codex, "gpt-6-astra", None, Some("ultra")),
                &work,
                now_ms(),
            )
            .unwrap();
        let run = store
            .prepare_run(&session.id, session.revision, now_ms())
            .unwrap();
        let busy = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert_eq!(busy.model.provider, Provider::Claude);
        assert!(!busy.reason.contains("Warning"));
        store
            .settle(&run, xcb_core::session::State::Idle, now_ms())
            .unwrap();
        let now = now_ms().saturating_sub(1);
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                pool: store.account(&codex).unwrap().quota_pool,
                window: Id::new("codex.primary").unwrap(),
                used_percent: 100.0,
                observed_at_ms: now,
                resets_at_ms: now + 60_000,
            })
            .unwrap();
        let limited = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert!(limited.reason.starts_with("Warning: usage limits"));
        assert!(limited.reason.contains("codex/gpt-6-astra/ultra"));
        let constrained = route_with_admitted(
            &store,
            &config,
            RouteRequest {
                requirements: Default::default(),
                required_provider: Some(Provider::Claude),
                ..request()
            },
            &admitted,
        )
        .await
        .unwrap();
        assert!(!constrained.reason.contains("Warning"));
        let unadmitted =
            route_with_admitted(&store, &config, request(), &[Provider::Claude].into())
                .await
                .unwrap();
        assert!(!unadmitted.reason.contains("Warning"));
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                pool: store.account(&codex).unwrap().quota_pool,
                window: Id::new("codex.primary").unwrap(),
                used_percent: 0.0,
                observed_at_ms: now_ms(),
                resets_at_ms: now + 60_000,
            })
            .unwrap();
        fail_authentication(&store, &codex);
        let unauthenticated = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert!(!unauthenticated.reason.contains("Warning"));
    }

    /// Three equal accounts of one provider take turns: each pick is the one
    /// whose last session is oldest, so the fourth pick wraps to the first.
    #[tokio::test]
    async fn equal_routes_rotate_to_the_least_recently_used_account() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let work = crate::private::directory(&base.join("work")).unwrap();
        let accounts: BTreeSet<_> = (0..3).map(|_| account(&store, Provider::Claude)).collect();
        let sonnet = model(Provider::Claude, "claude-sonnet-5", None, None);
        store
            .set_models(Provider::Claude, std::slice::from_ref(&sonnet))
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let none = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: "fix a test",
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &none,
            excluded_accounts: &no_accounts,
            account: None,
        };
        let admitted = [Provider::Claude].into();
        let base_ms = now_ms();
        let mut picks = Vec::new();
        for turn in 0..4u64 {
            let decision = route_with_admitted(&store, &config, request(), &admitted)
                .await
                .unwrap();
            assert_eq!(decision.model.key(), sonnet.key());
            // Starting a session on the chosen account records its use.
            store
                .create_session(
                    &decision.account,
                    sonnet.clone(),
                    &work,
                    base_ms + turn * 1_000,
                )
                .unwrap();
            picks.push(decision.account);
        }
        let first_three: BTreeSet<_> = picks[..3].iter().cloned().collect();
        assert_eq!(first_three, accounts, "{picks:?}");
        assert_eq!(picks[3], picks[0], "the rotation wraps: {picks:?}");
    }

    /// Failover keeps the subscription rotation close to the work: the same
    /// model on another account, then the same provider's other models, then
    /// other providers. The failed route, tried routes and an account-wide
    /// limit are left out; a model-specific limit keeps the failed account's
    /// other models.
    #[tokio::test]
    async fn failover_prefers_same_model_then_same_provider_then_other_providers() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let first = account(&store, Provider::Claude);
        let second = account(&store, Provider::Claude);
        let codex = account(&store, Provider::Codex);
        let opus = model(Provider::Claude, "claude-opus-5", None, None);
        let sonnet = model(Provider::Claude, "claude-sonnet-5", None, None);
        let sol = model(Provider::Codex, "gpt-5.6-sol", None, None);
        store
            .set_models(Provider::Claude, &[opus.clone(), sonnet.clone()])
            .unwrap();
        store
            .set_models(Provider::Codex, std::slice::from_ref(&sol))
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let none = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let admitted = [Provider::Claude, Provider::Codex].into();
        let request = |failure, tried, required_provider| FailoverRequest {
            requirements: Default::default(),
            task: "fix a test",
            account: &first,
            model: &opus,
            failure,
            tried,
            limited_accounts: &no_accounts,
            required_provider,
            required_model: None,
            required_account: None,
        };
        let keys = |routes: &[FailoverRoute]| -> Vec<String> {
            routes
                .iter()
                .map(|route| format!("{}/{}", route.account, route.model.key()))
                .collect()
        };
        let opus_key = opus.key();
        let mut model_pin = request(Failure::AccountQuota, &none, None);
        model_pin.required_model = Some(&opus_key);
        let pinned = failover_routes_with_admitted(&store, &config, model_pin, &admitted)
            .await
            .unwrap()
            .routes;
        assert_eq!(keys(&pinned), vec![format!("{second}/{opus_key}")]);
        let mut account_pin = request(Failure::ModelQuota, &none, None);
        account_pin.required_account = Some(&first);
        let pinned = failover_routes_with_admitted(&store, &config, account_pin, &admitted)
            .await
            .unwrap()
            .routes;
        assert!(!pinned.is_empty());
        assert!(pinned.iter().all(|route| route.account == first));
        let mut exact_pin = request(Failure::AccountQuota, &none, None);
        exact_pin.required_account = Some(&first);
        exact_pin.required_model = Some(&opus_key);
        assert!(
            failover_routes_with_admitted(&store, &config, exact_pin, &admitted)
                .await
                .unwrap()
                .routes
                .is_empty()
        );
        let account_limit = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::AccountQuota, &none, None),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert_eq!(
            keys(&account_limit),
            [
                format!("{second}/{}", opus.key()),
                format!("{second}/{}", sonnet.key()),
                format!("{codex}/{}", sol.key()),
            ]
        );
        let model_limit = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::ModelQuota, &none, None),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        let listed = keys(&model_limit);
        assert_eq!(listed[0], format!("{second}/{}", opus.key()));
        assert_eq!(listed.len(), 4, "{listed:?}");
        assert!(listed[1..3].contains(&format!("{first}/{}", sonnet.key())));
        assert!(listed[1..3].contains(&format!("{second}/{}", sonnet.key())));
        assert_eq!(listed[3], format!("{codex}/{}", sol.key()));
        assert!(!listed.contains(&format!("{first}/{}", opus.key())));
        // A route this task already ran is never offered again.
        let tried = BTreeSet::from([format!("{second}/{}", opus.key())]);
        let after_tried = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::AccountQuota, &tried, None),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert_eq!(
            keys(&after_tried),
            [
                format!("{second}/{}", sonnet.key()),
                format!("{codex}/{}", sol.key()),
            ]
        );
        // A pinned provider is a hard constraint: it never widens.
        let pinned = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::AccountQuota, &none, Some(Provider::Claude)),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert!(
            pinned
                .iter()
                .all(|route| route.model.provider == Provider::Claude),
            "{:?}",
            keys(&pinned)
        );
        let tried_all: BTreeSet<_> = [&opus, &sonnet]
            .into_iter()
            .map(|choice| format!("{second}/{}", choice.key()))
            .collect();
        let exhausted = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::AccountQuota, &tried_all, Some(Provider::Claude)),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert!(exhausted.is_empty(), "{:?}", keys(&exhausted));
        let mismatched = failover_routes_with_admitted(
            &store,
            &config,
            request(Failure::AccountQuota, &none, Some(Provider::Codex)),
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert!(mismatched.is_empty(), "{:?}", keys(&mismatched));
    }

    /// An account without a usage meter (Devin) or whose last reading aged
    /// out is a failover target; a fresh reading at 100% is not, and an
    /// account that reported an account-wide limit earlier in the task stays
    /// out even on another model.
    #[tokio::test]
    async fn failover_targets_unmeasured_accounts_and_skips_known_limits() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let limited = account(&store, Provider::Claude);
        let stale = account(&store, Provider::Claude);
        let exhausted = account(&store, Provider::Claude);
        let codex = account(&store, Provider::Codex);
        let sonnet = model(Provider::Claude, "claude-sonnet-5", None, None);
        let replacement = model(Provider::Codex, "gpt-6-astra", None, Some("high"));
        store
            .set_models(Provider::Claude, std::slice::from_ref(&sonnet))
            .unwrap();
        store
            .set_models(Provider::Codex, std::slice::from_ref(&replacement))
            .unwrap();
        let now = now_ms();
        let quota = |account: &Id, used_percent, observed_at_ms| xcb_core::usage::QuotaPoint {
            pool: store.account(account).unwrap().quota_pool,
            window: Id::new("seven_day").unwrap(),
            used_percent,
            observed_at_ms,
            resets_at_ms: now + 3_600_000,
        };
        // Half used, read ten minutes ago: no percentage is fresh enough to
        // count, and nothing says the account is limited.
        store
            .record_quota(&quota(&stale, 50.0, now - 600_000))
            .unwrap();
        store
            .record_quota(&quota(&exhausted, 100.0, now - 1))
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        // This test is about usage meters, not the preference stack: Codex
        // is an ordinary target and its observed model is allowed.
        config.routing.never.clear();
        config.routing.fallback_providers.clear();
        let none = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let admitted = [Provider::Claude, Provider::Codex].into();
        let request = |limited_accounts| FailoverRequest {
            requirements: Default::default(),
            task: "fix a test",
            account: &limited,
            model: &sonnet,
            failure: Failure::AccountQuota,
            tried: &none,
            limited_accounts,
            required_provider: None,
            required_model: None,
            required_account: None,
        };
        let routes =
            failover_routes_with_admitted(&store, &config, request(&no_accounts), &admitted)
                .await
                .unwrap()
                .routes;
        let listed: Vec<_> = routes
            .iter()
            .map(|route| format!("{}/{}", route.account, route.model.key()))
            .collect();
        assert_eq!(
            listed,
            [
                format!("{stale}/{}", sonnet.key()),
                format!("{codex}/{}", replacement.key()),
            ]
        );
        let seen_limited = BTreeSet::from([stale.clone()]);
        let routes =
            failover_routes_with_admitted(&store, &config, request(&seen_limited), &admitted)
                .await
                .unwrap()
                .routes;
        assert_eq!(routes.len(), 1, "{routes:?}");
        assert_eq!(routes[0].account, codex);
    }

    #[test]
    fn unknown_model_labels_cannot_displace_the_known_quality_tier() {
        let build = |id, utility| {
            let model = model(Provider::Claude, id, None, None);
            Candidate {
                profile: base_profile(&model, &OfferState::default(), 1),
                model,
                account: Id::new("account").unwrap(),
                utility,
                quota_pressure: None,
                last_used_ms: 0,
                stack: None,
                version: Vec::new(),
            }
        };
        let mut candidates = vec![build("future-model", 10000), build("claude-haiku", -100)];
        retain_quality_tier(&mut candidates, true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].model.id.as_str(), "claude-haiku");
    }

    #[tokio::test]
    async fn authentication_health_selects_other_account_without_crossing_provider_constraint() {
        use crate::authentication_tests::{account, fail_authentication, model};
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let first = account(&store, Provider::Codex);
        let second = account(&store, Provider::Codex);
        account(&store, Provider::Claude);
        store
            .set_models(Provider::Codex, &[model(Provider::Codex)])
            .unwrap();
        store
            .set_models(Provider::Claude, &[model(Provider::Claude)])
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        config.default_account = Some(first.clone());
        fail_authentication(&store, &first);
        let none = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: "Use Codex. Fix a test",
            required_provider: Some(Provider::Codex),
            preferred_provider: None,
            required_model: None,
            excluded_routes: &none,
            excluded_accounts: &excluded_accounts,
            account: None,
        };
        let admitted = [Provider::Codex, Provider::Claude].into();
        let route = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert_eq!(route.account, second);
        assert_eq!(route.model.provider, Provider::Codex);
        let work = store.root().parent().unwrap().join("synthetic-work");
        let selected = crate::kernel::new_session(
            &store,
            &work,
            &config,
            None,
            Some("codex/gpt-5.6-sol"),
            None,
        )
        .unwrap();
        assert_eq!(selected.account, second);
        assert!(
            crate::kernel::new_session(
                &store,
                &work,
                &config,
                Some(&first),
                Some("codex/gpt-5.6-sol"),
                None
            )
            .is_err()
        );
        fail_authentication(&store, &second);
        assert!(
            route_with_admitted(&store, &config, request(), &admitted)
                .await
                .is_err()
        );
        assert!(
            crate::kernel::new_session(
                &store,
                &work,
                &config,
                None,
                Some("codex/gpt-5.6-sol"),
                None
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn no_connected_account_is_distinct_from_a_temporary_route_shortage() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let config = Config::default();
        let routes = BTreeSet::new();
        let accounts = BTreeSet::new();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: "fix a test",
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &routes,
            excluded_accounts: &accounts,
            account: None,
        };
        let admitted: BTreeSet<Provider> = [Provider::Claude].into();
        // No accounts at all: waiting cannot help.
        let Err(Error::Unavailable(reason)) =
            route_with_admitted(&store, &config, request(), &admitted).await
        else {
            panic!("empty routing must fail");
        };
        assert_eq!(reason, NO_CONNECTED_ACCOUNT);
        // An account without credentials is still not connected.
        store
            .add_account(Provider::Claude, "Synthetic", 1, None)
            .unwrap();
        let Err(Error::Unavailable(reason)) =
            route_with_admitted(&store, &config, request(), &admitted).await
        else {
            panic!("uncredentialed routing must fail");
        };
        assert_eq!(reason, NO_CONNECTED_ACCOUNT);
        // A connected account that is busy right now is a temporary shortage.
        let busy = account(&store, Provider::Claude);
        let work =
            crate::private::directory(&store.root().parent().unwrap().join("synthetic-work"))
                .unwrap();
        let session = store
            .create_session(
                &busy,
                model(Provider::Claude, "synthetic", None, None),
                &work,
                1,
            )
            .unwrap();
        store
            .prepare_run(&session.id, session.revision, crate::now_ms())
            .unwrap();
        let Err(Error::Unavailable(reason)) =
            route_with_admitted(&store, &config, request(), &admitted).await
        else {
            panic!("busy routing must fail");
        };
        assert_eq!(reason, NO_ELIGIBLE_ROUTE);
    }

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
    fn public_promotion_does_not_claim_an_unverified_account_is_free() {
        let offers = OfferState {
            version: 1,
            checked_at_ms: 1,
            next_check_ms: 2,
            source_sha256: "a".repeat(64),
            offers: vec![crate::offers::ModelOffer {
                provider: Provider::Devin,
                model_prefix: "swe-2-".into(),
                surface: "devin_cli".into(),
                kind: crate::offers::OfferKind::Free,
                terms: "Free use of SWE-2 Free in Devin Desktop and CLI through October 10, 2026"
                    .into(),
                valid_until_ms: 1_791_676_800_000,
                source: "https://devin.ai/pricing".into(),
            }],
        };
        let swe = base_profile(
            &model(Provider::Devin, "swe-2-high", None, None),
            &offers,
            2,
        );
        let opus = base_profile(
            &model(
                Provider::Claude,
                "default",
                Some("claude-opus-5"),
                Some("high"),
            ),
            &offers,
            2,
        );
        let without_offer = base_profile(
            &model(Provider::Devin, "swe-2-high", None, None),
            &OfferState::default(),
            2,
        );
        assert_eq!(swe.relative_cost, without_offer.relative_cost);
        assert!(swe.relative_cost > 0);
        assert!(swe.free_offer.is_some());
        assert_eq!(
            utility(TaskClass::Balanced, &swe),
            utility(TaskClass::Balanced, &without_offer)
        );
        assert!(opus.relative_cost > swe.relative_cost);
        assert!(opus.quality >= swe.quality);
    }

    #[test]
    fn pareto_layers_peel_dominated_models_deterministically() {
        let mut rows = vec![
            ProfiledModel {
                key: "a".into(),
                label: "a".into(),
                provider: Provider::Claude,
                profile: ModelProfile {
                    quality: 90,
                    relative_cost: 20,
                    relative_latency: 20,
                    pareto_layer: 0,
                    recognized: true,
                    free_offer: None,
                },
            },
            ProfiledModel {
                key: "b".into(),
                label: "b".into(),
                provider: Provider::Codex,
                profile: ModelProfile {
                    quality: 80,
                    relative_cost: 30,
                    relative_latency: 30,
                    pareto_layer: 0,
                    recognized: true,
                    free_offer: None,
                },
            },
            ProfiledModel {
                key: "c".into(),
                label: "c".into(),
                provider: Provider::Claude,
                profile: ModelProfile {
                    quality: 95,
                    relative_cost: 40,
                    relative_latency: 10,
                    pareto_layer: 0,
                    recognized: true,
                    free_offer: None,
                },
            },
        ];
        assign_pareto_layers(&mut rows);
        assert_eq!(rows[0].profile.pareto_layer, 1);
        assert_eq!(rows[2].profile.pareto_layer, 1);
        assert_eq!(rows[1].profile.pareto_layer, 2);
    }

    #[test]
    fn task_class_changes_quality_cost_tradeoff() {
        let efficient = ModelProfile {
            quality: 80,
            relative_cost: 5,
            relative_latency: 5,
            pareto_layer: 1,
            recognized: true,
            free_offer: None,
        };
        let frontier = ModelProfile {
            quality: 105,
            relative_cost: 100,
            relative_latency: 80,
            pareto_layer: 1,
            recognized: true,
            free_offer: None,
        };
        assert!(utility(TaskClass::Routine, &efficient) > utility(TaskClass::Routine, &frontier));
        assert!(utility(TaskClass::Complex, &frontier) > utility(TaskClass::Complex, &efficient));
    }

    #[test]
    fn quota_spending_pressure_and_soft_provider_preference_break_route_ties() {
        let profile = ModelProfile {
            quality: 90,
            relative_cost: 40,
            relative_latency: 40,
            pareto_layer: 1,
            recognized: true,
            free_offer: None,
        };
        let pressure = |pace| QuotaSpendingPressure {
            window: Id::new("seven_day").unwrap(),
            remaining_percent: 30.0,
            resets_at_ms: 10_800_001,
            percent_per_hour: pace,
        };
        let low = route_utility(
            TaskClass::Balanced,
            &profile,
            false,
            false,
            Some(&pressure(0.5)),
        );
        let high = route_utility(
            TaskClass::Balanced,
            &profile,
            false,
            false,
            Some(&pressure(4.5)),
        );
        let preferred = route_utility(
            TaskClass::Balanced,
            &profile,
            false,
            true,
            Some(&pressure(0.5)),
        );
        assert!(high > low);
        assert!(preferred > low);
        assert_eq!(
            route_utility(TaskClass::Balanced, &profile, false, false, None),
            utility(TaskClass::Balanced, &profile)
        );
        assert_eq!(
            route_utility(
                TaskClass::Balanced,
                &profile,
                false,
                false,
                Some(&pressure(6000.0))
            ),
            utility(TaskClass::Balanced, &profile) + 200
        );
    }

    fn record_spending_windows(
        store: &Store,
        account: &Id,
        windows: &[(&str, f64, u64)],
        now: u64,
    ) {
        let run = store.prepare_probe(account, None, now).unwrap();
        for (window, remaining, hours) in windows {
            store
                .record_account_quota(
                    &run,
                    &xcb_core::usage::QuotaPoint {
                        pool: store.account(account).unwrap().quota_pool,
                        window: Id::new(*window).unwrap(),
                        used_percent: 100.0 - remaining,
                        observed_at_ms: now,
                        resets_at_ms: now + hours * 3_600_000,
                    },
                )
                .unwrap();
        }
        store
            .settle(&run, xcb_core::session::State::Idle, now)
            .unwrap();
    }

    #[tokio::test]
    async fn quota_spending_pressure_routes_before_reset_and_preserves_constraints() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let soon = account(&store, Provider::Codex);
        let later = account(&store, Provider::Codex);
        let choice = model(Provider::Codex, "gpt-5.6-sol", None, None);
        let key = choice.key();
        store.set_models(Provider::Codex, &[choice]).unwrap();
        let now = now_ms().saturating_sub(1000);
        record_spending_windows(
            &store,
            &soon,
            &[("codex.primary", 80.0, 3), ("codex.secondary", 30.0, 3)],
            now,
        );
        record_spending_windows(
            &store,
            &later,
            &[("codex.primary", 100.0, 3), ("codex.secondary", 35.0, 144)],
            now,
        );
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let request = || RouteRequest {
            requirements: Default::default(),
            task: "Fix a failing assertion",
            required_provider: Some(Provider::Codex),
            preferred_provider: None,
            required_model: Some(&key),
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: None,
        };
        let admitted = [Provider::Codex].into();
        let selected = route_with_admitted(&store, &config, request(), &admitted)
            .await
            .unwrap();
        assert_eq!(selected.account, soon);
        assert!(
            selected
                .reason
                .contains("subscription budget 30.0% in codex.secondary")
        );
        // A tight overlapping weekly allowance wins over the near reset of
        // the short window; percentages and deadlines cannot be mixed.
        record_spending_windows(&store, &soon, &[("codex.secondary", 1.0, 168)], now + 1);
        assert_eq!(
            route_with_admitted(&store, &config, request(), &admitted)
                .await
                .unwrap()
                .account,
            later
        );
        assert_eq!(
            route_with_admitted(
                &store,
                &config,
                RouteRequest {
                    requirements: Default::default(),
                    account: Some(&soon),
                    ..request()
                },
                &admitted
            )
            .await
            .unwrap()
            .account,
            soon
        );
        let held = store.prepare_probe(&later, None, now + 2).unwrap();
        assert_eq!(
            route_with_admitted(&store, &config, request(), &admitted)
                .await
                .unwrap()
                .account,
            soon
        );
        store
            .settle(&held, xcb_core::session::State::Idle, now + 3)
            .unwrap();
        record_spending_windows(&store, &later, &[("codex.secondary", 0.0, 144)], now + 4);
        assert_eq!(
            route_with_admitted(&store, &config, request(), &admitted)
                .await
                .unwrap()
                .account,
            soon
        );
    }

    #[tokio::test]
    async fn quota_spending_pressure_crosses_providers_without_lowering_frontier_quality() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let claude = account(&store, Provider::Claude);
        let codex = account(&store, Provider::Codex);
        store
            .set_models(
                Provider::Claude,
                &[model(Provider::Claude, "claude-opus-5", None, None)],
            )
            .unwrap();
        store
            .set_models(
                Provider::Codex,
                &[model(Provider::Codex, "gpt-6-astra", None, None)],
            )
            .unwrap();
        record_spending_windows(
            &store,
            &claude,
            &[("five_hour", 80.0, 3), ("seven_day", 30.0, 3)],
            now_ms(),
        );
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let request = |task| RouteRequest {
            requirements: Default::default(),
            task,
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: None,
        };
        let admitted = [Provider::Claude, Provider::Codex].into();
        assert_eq!(
            route_with_admitted(
                &store,
                &config,
                request("Fix a failing assertion"),
                &admitted
            )
            .await
            .unwrap()
            .account,
            claude
        );
        let complex = route_with_admitted(
            &store,
            &config,
            request("Review security architecture and concurrency"),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(complex.account, codex);
        assert!(complex.reason.contains("quota timing unmeasured"));
        assert_eq!(
            route_with_admitted(
                &store,
                &config,
                RouteRequest {
                    requirements: Default::default(),
                    required_provider: Some(Provider::Codex),
                    ..request("Fix a failing assertion")
                },
                &admitted
            )
            .await
            .unwrap()
            .account,
            codex
        );
    }

    #[test]
    fn failover_filters_before_bounding_and_layering_the_catalog() {
        let models: Vec<_> = (0..20)
            .map(|index| {
                model(
                    Provider::Claude,
                    &format!("swe-2-variant-{index:02}"),
                    None,
                    None,
                )
            })
            .collect();
        let profiles = eligible_profiles(
            &models,
            &OfferState::default(),
            2,
            "implement a fix",
            false,
            |m| m.id.as_str() == "swe-2-variant-19",
            |_| None,
        );
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles["claude/swe-2-variant-19"].pareto_layer, 1);
        assert!(
            eligible_profiles(
                &models,
                &OfferState::default(),
                2,
                "task",
                false,
                |_| false,
                |_| None
            )
            .is_empty()
        );
    }

    #[test]
    fn provider_model_shape_remains_part_of_eligibility() {
        assert!(selectable_model(&model(
            Provider::Codex,
            "gpt-6-astra",
            None,
            Some("high")
        )));
        assert!(!selectable_model(&model(
            Provider::Codex,
            "gpt-6-astra-future",
            None,
            Some("high")
        )));
        assert!(!selectable_model(&model(
            Provider::Devin,
            "swe-2-high",
            None,
            Some("high")
        )));
        let mut adaptive = model(Provider::Claude, "swe-2-high", None, None);
        adaptive.mode = Mode::Adaptive;
        assert!(!selectable_model(&adaptive));
    }

    #[test]
    fn profile_identity_and_effort_do_not_come_from_display_labels() {
        let offers = OfferState::default();
        let mut unknown = model(Provider::Claude, "new-model", None, None);
        unknown.label = "GPT-6 Astra Ultra (Max plan)".into();
        let profile = base_profile(&unknown, &offers, 2);
        assert!(!profile.recognized);
        assert_eq!(profile.quality, 72);
        let near_match = model(Provider::Claude, "swe-20-high", None, None);
        assert!(!base_profile(&near_match, &offers, 2).recognized);
        let resolved = model(
            Provider::Claude,
            "claude-opus-5",
            Some("claude-sonnet-5"),
            Some("low"),
        );
        assert_eq!(base_profile(&resolved, &offers, 2).quality, 83);
        assert_eq!(
            effort(&model(Provider::Claude, "swe-2-xhigh", None, None)),
            "xhigh"
        );
    }

    #[test]
    fn task_cues_match_words_instead_of_incidental_substrings() {
        assert_eq!(
            classify_task("trace the information flow"),
            TaskClass::Balanced
        );
        assert_eq!(classify_task("fix a race condition"), TaskClass::Complex);
        assert_eq!(classify_task("find the root\ncause"), TaskClass::Complex);
        assert_eq!(classify_task("format this file"), TaskClass::Routine);
        assert_eq!(
            classify_task("update security documentation"),
            TaskClass::Complex
        );
    }

    #[test]
    fn only_initial_affirmative_provider_directives_are_hard_constraints() {
        for (task, provider) in [
            ("Use Claude.", Provider::Claude),
            ("  Please use Codex to fix this", Provider::Codex),
            ("use claude for this task", Provider::Claude),
            ("use claude", Provider::Claude),
        ] {
            assert_eq!(explicit_provider_intent(task), Some(provider), "{task}");
        }
        for task in [
            "Do not use Claude",
            "Don't use Codex for this",
            "Explain when to use Devin",
            "use devin for this task",
            "The docs say use Claude.",
            "use codexish tools",
            "use Claude's API",
            "use codex models in the API",
            "\"Use Claude.\" is the example",
        ] {
            assert_eq!(explicit_provider_intent(task), None, "{task}");
        }
    }

    #[tokio::test]
    async fn routes_a_model_only_to_accounts_whose_catalog_contains_it() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let first = account(&store, Provider::Claude);
        let second = account(&store, Provider::Claude);
        let shared = model(Provider::Claude, "swe-2-variant", None, None);
        let exclusive = model(Provider::Claude, "gpt-6-astra-ultra", None, None);
        // An upgraded host: one provider-wide list, no account's own yet.
        store
            .set_models(Provider::Claude, std::slice::from_ref(&shared))
            .unwrap();
        store
            .set_account_models(&first, &[shared.clone(), exclusive.clone()])
            .unwrap();
        let mut config = Config::default();
        config.routing.never.clear();
        let prompt = "details ".repeat(400);
        let admitted = [Provider::Claude].into();
        let no_routes = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let route = |excluded_routes, excluded_accounts, required_model| {
            let store = &store;
            let config = &config;
            let prompt = &prompt;
            let admitted = &admitted;
            async move {
                route_with_admitted(
                    store,
                    config,
                    RouteRequest {
                        requirements: Default::default(),
                        task: prompt,
                        required_provider: Some(Provider::Claude),
                        preferred_provider: None,
                        required_model,
                        excluded_routes,
                        excluded_accounts,
                        account: None,
                    },
                    admitted,
                )
                .await
            }
        };
        // The stronger model belongs only to the first account's plan.
        let best = route(&no_routes, &no_accounts, None).await.unwrap();
        assert_eq!(
            (best.account.clone(), best.model.key()),
            (first.clone(), exclusive.key())
        );
        // With the first account unavailable, the second account (which has
        // no list of its own yet) routes with the provider-wide list and is
        // never paired with the first account's exclusive model.
        let without_first = BTreeSet::from([first.clone()]);
        let fallback = route(&no_routes, &without_first, None).await.unwrap();
        assert_eq!(
            (fallback.account.clone(), fallback.model.key()),
            (second.clone(), shared.key())
        );
        let key = exclusive.key();
        assert!(route(&no_routes, &without_first, Some(&key)).await.is_err());
        // Once the second account reports its own list, that list decides.
        store
            .set_account_models(&second, std::slice::from_ref(&shared))
            .unwrap();
        let excluded = BTreeSet::from([format!("{} · {}", exclusive.key(), first)]);
        let next = route(&excluded, &no_accounts, None).await.unwrap();
        assert_eq!(next.model.key(), shared.key());
        assert!(route(&excluded, &no_accounts, Some(&key)).await.is_err());
    }

    #[test]
    fn versioned_family_profiles_recognize_new_releases() {
        let quality =
            |provider, id| family_quality(&model(provider, id, None, None)).expect("recognized");
        assert_eq!(quality(Provider::Codex, "gpt-5.6-sol"), 89);
        assert_eq!(quality(Provider::Codex, "gpt-6-sol"), 94);
        assert_eq!(quality(Provider::Codex, "gpt-6.1-sol"), 94);
        assert_eq!(quality(Provider::Claude, "gpt-6-1-sol-max"), 94);
        assert_eq!(quality(Provider::Codex, "gpt-6-astra"), 100);
        assert_eq!(quality(Provider::Codex, "gpt-7-astra"), 100);
        assert_eq!(quality(Provider::Claude, "claude-fable-5-1"), 97);
        assert_eq!(quality(Provider::Claude, "claude-fable-5-2[1m]"), 97);
        assert_eq!(quality(Provider::Claude, "fable-6"), 97);
        for unrecognized in ["gpt-6-solar", "gpt-5-sol", "fable-5", "gpt-sol"] {
            assert_eq!(
                family_quality(&model(Provider::Codex, unrecognized, None, None)),
                None,
                "{unrecognized}"
            );
        }
        assert!(family_version("gpt-6-1-sol", "gpt-", "-sol").unwrap() > vec![6]);
        assert_eq!(
            family_version("claude-fable-5-1[1m]", "claude-fable-", ""),
            Some(vec![5, 1])
        );
    }

    /// The tier table: judged answers and prompt shape decide, the routine
    /// cue only when nothing asks for more.
    #[tokio::test]
    async fn tiers_follow_the_documented_table() {
        let judged = |frontier, kind: &str, difficulty, scope| task_classifier::Classification {
            signed_in_browser: None,
            desktop: None,
            frontier,
            source: "test",
            score_milli: None,
            kind: Some(kind.into()),
            features: xcb_core::reflex::route_features("task", false, false),
            judged: true,
            substantial: false,
            difficulty: Some(difficulty),
            scope: Some(scope),
        };
        let tier = |class, classification: &task_classifier::Classification| {
            assign_tier(class, classification)
        };
        assert_eq!(
            tier(TaskClass::Balanced, &judged(true, "implement", 4.0, 4.0)),
            Tier::Buildout
        );
        assert_eq!(
            tier(TaskClass::Balanced, &judged(true, "implement", 4.0, 3.0)),
            Tier::Meaty
        );
        assert_eq!(
            tier(TaskClass::Balanced, &judged(true, "implement", 3.0, 5.0)),
            Tier::Meaty
        );
        assert_eq!(
            tier(TaskClass::Balanced, &judged(false, "resume", 2.0, 2.0)),
            Tier::Meaty
        );
        assert_eq!(
            tier(TaskClass::Routine, &judged(false, "implement", 2.0, 2.0)),
            Tier::Mechanical
        );
        assert_eq!(
            tier(TaskClass::Balanced, &judged(false, "implement", 2.0, 2.0)),
            Tier::Default
        );
        // Without a judge: the routine cue, a complex cue, and prompt size.
        async fn plain(task: &str) -> Tier {
            let class = classify_task(task);
            let classification = task_classifier::classify(
                task,
                None,
                class == TaskClass::Complex,
                class == TaskClass::Routine,
            )
            .await;
            assign_tier(class, &classification)
        }
        assert_eq!(plain("fix a typo in the docs").await, Tier::Mechanical);
        assert_eq!(plain("fix a test").await, Tier::Default);
        assert_eq!(plain("redesign the scheduler").await, Tier::Meaty);
        let long = "details ".repeat(400);
        assert_eq!(plain(&long).await, Tier::Meaty);
        let long_complex = format!("redesign the architecture. {long}");
        assert_eq!(plain(&long_complex).await, Tier::Buildout);
        assert_eq!(
            plain(&format!("fix a typo. {long}")).await,
            Tier::Meaty,
            "a substantial prompt is never mechanical"
        );
    }

    /// Codex ids are limited to the qualified catalog, so the newest-version
    /// rule is exercised on Claude (Fable 5.2 over 5.1) here and on Codex ids
    /// in the pattern tests.
    fn stack_models(store: &Store) {
        store
            .set_models(
                Provider::Codex,
                &[
                    model(Provider::Codex, "gpt-5.6-sol", None, Some("ultra")),
                    model(Provider::Codex, "gpt-5.6-sol", None, Some("max")),
                    model(Provider::Codex, "gpt-6-astra", None, Some("ultra")),
                    model(Provider::Codex, "gpt-6-astra", None, Some("max")),
                ],
            )
            .unwrap();
        store
            .set_models(
                Provider::Claude,
                &[
                    model(Provider::Claude, "claude-fable-5-1", None, Some("max")),
                    model(Provider::Claude, "claude-fable-5-2", None, Some("max")),
                    model(Provider::Claude, "opus[1m]", None, Some("max")),
                    model(
                        Provider::Claude,
                        "default",
                        Some("claude-opus-5-5"),
                        Some("high"),
                    ),
                    model(
                        Provider::Claude,
                        "sonnet",
                        Some("claude-sonnet-5"),
                        Some("max"),
                    ),
                ],
            )
            .unwrap();
    }

    /// The stack outranks inferred quality: a default-tier task goes to the
    /// newest Sol at ultra although Astra profiles higher, a build-out goes
    /// to Astra at ultra ahead of Fable, and when every Astra account is at
    /// a limit the build-out goes to Fable at max.
    #[tokio::test]
    async fn preference_stack_orders_eligible_routes_and_the_newest_version_wins() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let codex = account(&store, Provider::Codex);
        let claude = account(&store, Provider::Claude);
        stack_models(&store);
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let none = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let admitted = [Provider::Codex, Provider::Claude].into();
        let request = |task, excluded_accounts| RouteRequest {
            requirements: Default::default(),
            task,
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &none,
            excluded_accounts,
            account: None,
        };
        let plain = route_with_admitted(
            &store,
            &config,
            request("fix a test", &no_accounts),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(plain.model.key(), "codex/gpt-5.6-sol/ultra");
        assert_eq!(plain.tier, Tier::Default);
        assert_eq!(plain.stack_position, Some(1));
        assert!(plain.reason.contains(" · tier default · stack #1"));
        let without_codex = BTreeSet::from([codex.clone()]);
        let second = route_with_admitted(
            &store,
            &config,
            request("fix a test", &without_codex),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(second.model.key(), "claude/opus[1m]/max");
        assert_eq!(second.account, claude);
        assert_eq!(second.stack_position, Some(2));
        let routine = route_with_admitted(
            &store,
            &config,
            request("fix a typo", &no_accounts),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(routine.model.key(), "codex/gpt-5.6-sol/max");
        assert_eq!(routine.tier, Tier::Mechanical);
        let buildout_prompt = format!("redesign the architecture. {}", "details ".repeat(400));
        let buildout = route_with_admitted(
            &store,
            &config,
            request(&buildout_prompt, &no_accounts),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(buildout.model.key(), "codex/gpt-6-astra/ultra");
        assert_eq!(buildout.tier, Tier::Buildout);
        assert!(buildout.reason.contains(" · tier buildout · stack #1"));
        let now = now_ms().saturating_sub(1);
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                pool: store.account(&codex).unwrap().quota_pool,
                window: Id::new("codex.primary").unwrap(),
                used_percent: 100.0,
                observed_at_ms: now,
                resets_at_ms: now + 60_000,
            })
            .unwrap();
        let fable = route_with_admitted(
            &store,
            &config,
            request(&buildout_prompt, &no_accounts),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(
            fable.model.key(),
            "claude/claude-fable-5-2/max",
            "the newest Fable wins within the pattern"
        );
        assert_eq!(fable.stack_position, Some(2));
        // A route the stack does not name still runs when nothing named is
        // eligible; the reason says so.
        let mut sparse = config.clone();
        sparse.routing.tiers.r#default = vec!["codex/gpt-*-astra/ultra".into()];
        let unmatched = route_with_admitted(
            &store,
            &sparse,
            request("fix a test", &no_accounts),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(unmatched.model.provider, Provider::Claude);
        assert_eq!(unmatched.stack_position, None);
        assert!(
            unmatched
                .reason
                .contains(" · tier default · no stack match")
        );
    }

    /// After an account limit the next pick is the same stack pattern on the
    /// other account, then the tier's next pattern; the rest of the tier's
    /// unmatched routes follow.
    #[tokio::test]
    async fn failover_follows_the_stack_across_accounts_before_the_next_pattern() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&xcb_core::canonical(root.path()).unwrap().join("state")).unwrap();
        let first = account(&store, Provider::Codex);
        let second = account(&store, Provider::Codex);
        let claude = account(&store, Provider::Claude);
        stack_models(&store);
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        let none = BTreeSet::new();
        let admitted = [Provider::Codex, Provider::Claude].into();
        let prompt = format!("redesign the architecture. {}", "details ".repeat(400));
        let astra = model(Provider::Codex, "gpt-6-astra", None, Some("ultra"));
        let limited = BTreeSet::from([first.clone()]);
        let routes = failover_routes_with_admitted(
            &store,
            &config,
            FailoverRequest {
                requirements: Default::default(),
                task: &prompt,
                account: &first,
                model: &astra,
                failure: Failure::AccountQuota,
                tried: &none,
                limited_accounts: &limited,
                required_provider: None,
                required_model: None,
                required_account: None,
            },
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        let keys: Vec<_> = routes
            .iter()
            .map(|route| format!("{}/{}", route.account, route.model.key()))
            .collect();
        assert_eq!(
            keys[0],
            format!("{second}/codex/gpt-6-astra/ultra"),
            "{keys:?}"
        );
        assert_eq!(
            keys[1],
            format!("{claude}/claude/claude-fable-5-2/max"),
            "{keys:?}"
        );
        assert_eq!(
            keys[2],
            format!("{claude}/claude/claude-fable-5-1/max"),
            "{keys:?}"
        );
        assert!(keys.len() > 2, "{keys:?}");
        assert!(!keys.iter().any(|key| key.starts_with(&format!("{first}/"))));
    }

    /// Devin is a fallback provider: its routes are used only when no other
    /// provider can take the task, and its SWE models never.
    #[tokio::test]
    async fn fallback_provider_is_used_only_when_nothing_else_is_eligible_and_never_excludes_pins()
    {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(root.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let work = crate::private::directory(&base.join("work")).unwrap();
        let codex = account(&store, Provider::Codex);
        let claude = account(&store, Provider::Claude);
        let sol = model(Provider::Codex, "gpt-5.6-sol", None, Some("ultra"));
        store
            .set_models(Provider::Codex, std::slice::from_ref(&sol))
            .unwrap();
        store
            .set_models(
                Provider::Claude,
                &[
                    model(Provider::Claude, "swe-2-high", None, None),
                    model(Provider::Claude, "swe-2-max", None, None),
                    model(Provider::Claude, "gpt-5-6-sol-max", None, None),
                    model(Provider::Claude, "gpt-6-astra-medium", None, None),
                ],
            )
            .unwrap();
        let mut config = Config::default();
        config.extensions.judge.enabled = false;
        config.routing.fallback_providers = vec![Provider::Claude];
        config.routing.never = vec!["claude/swe-*".into()];
        let none = BTreeSet::new();
        let no_accounts = BTreeSet::new();
        let admitted = [Provider::Codex, Provider::Claude].into();
        let request = |required_model| RouteRequest {
            requirements: Default::default(),
            task: "fix a test",
            required_provider: None,
            preferred_provider: None,
            required_model,
            excluded_routes: &none,
            excluded_accounts: &no_accounts,
            account: None,
        };
        let plain = route_with_admitted(&store, &config, request(None), &admitted)
            .await
            .unwrap();
        assert_eq!(plain.account, codex);
        // Codex busy: the fallback provider takes the task, ranked by the
        // same tier patterns (the default tier names Sol), never SWE.
        let session = store
            .create_session(&codex, sol.clone(), &work, now_ms())
            .unwrap();
        let run = store
            .prepare_run(&session.id, session.revision, now_ms())
            .unwrap();
        let fallback = route_with_admitted(&store, &config, request(None), &admitted)
            .await
            .unwrap();
        assert_eq!(fallback.account, claude);
        assert_eq!(fallback.model.key(), "claude/gpt-5-6-sol-max");
        assert_eq!(
            fallback.stack_position, None,
            "Claude never matches a codex/ pattern"
        );
        let failover = failover_routes_with_admitted(
            &store,
            &config,
            FailoverRequest {
                requirements: Default::default(),
                task: "fix a test",
                account: &codex,
                model: &sol,
                failure: Failure::AccountQuota,
                tried: &none,
                limited_accounts: &no_accounts,
                required_provider: None,
                required_model: None,
                required_account: None,
            },
            &admitted,
        )
        .await
        .unwrap()
        .routes;
        assert!(
            failover
                .iter()
                .all(|route| !route.model.id.as_str().starts_with("swe-")),
            "{failover:?}"
        );
        store
            .settle(&run, xcb_core::session::State::Idle, now_ms())
            .unwrap();
        // A pin on an excluded model is refused, not widened.
        let refused = route_with_admitted(
            &store,
            &config,
            request(Some("claude/swe-2-high")),
            &admitted,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(refused.contains("routing.never"), "{refused}");
        let pinned = route_with_admitted(
            &store,
            &config,
            request(Some("claude/gpt-6-astra-medium")),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(pinned.model.key(), "claude/gpt-6-astra-medium");
        // Without the fallback rule Claude competes on the profile order and
        // an allowed SWE model is an ordinary route again.
        config.routing.fallback_providers.clear();
        config.routing.never.clear();
        let open = route_with_admitted(
            &store,
            &config,
            request(Some("claude/swe-2-high")),
            &admitted,
        )
        .await
        .unwrap();
        assert_eq!(open.model.key(), "claude/swe-2-high");
    }
}
