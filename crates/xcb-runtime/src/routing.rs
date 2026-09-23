use crate::{
    Error, Result, auth, config::Config, judge, now_ms, offers::OfferState, process::Pin, runner,
    store::Store, summary, task_classifier,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
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
    pub account: Id,
    pub model: ModelChoice,
    pub profile: ModelProfile,
    pub reason: String,
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
    pub task: &'a str,
    pub required_provider: Option<Provider>,
    pub preferred_provider: Option<Provider>,
    /// An exact observed model key restricts the route to that model alone.
    pub required_model: Option<&'a str>,
    pub excluded_routes: &'a BTreeSet<String>,
    pub excluded_accounts: &'a BTreeSet<Id>,
    pub account: Option<&'a Id>,
}

#[derive(Clone)]
struct Candidate {
    account: Id,
    model: ModelChoice,
    profile: ModelProfile,
    utility: i32,
}

fn effort(model: &ModelChoice) -> String {
    model
        .effort
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| {
            [
                "ultra", "xhigh", "max", "high", "medium", "low", "minimal", "none",
            ]
            .into_iter()
            .find(|level| model.id.as_str().ends_with(&format!("-{level}")))
            .map(str::to_owned)
        })
        .unwrap_or_else(|| "medium".into())
}

fn base_profile(model: &ModelChoice, offers: &OfferState, now: u64) -> ModelProfile {
    // A display label is not model identity. Resolved aliases take precedence
    // over their requested name, and family matching must respect boundaries.
    let identity = model
        .resolved
        .as_ref()
        .map(Id::as_str)
        .unwrap_or(model.id.as_str())
        .to_ascii_lowercase()
        .replace('.', "-");
    let family = |name: &str| {
        identity == name
            || identity
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('-') || suffix.starts_with('['))
    };
    let (mut quality, mut cost, mut latency, recognized): (u16, u16, u16, bool) =
        if family("gpt-6-astra") {
            (100, 92, 72, true)
        } else if family("claude-opus-5") || family("opus-5") {
            (98, 96, 82, true)
        } else if family("claude-fable-5-1") || family("fable-5-1") {
            (97, 82, 68, true)
        } else if family("swe-2") {
            (96, 28, 45, true)
        } else if family("claude-sonnet-5") || family("sonnet-5") {
            (92, 55, 50, true)
        } else if family("gpt-5-6-sol") {
            (89, 45, 52, true)
        } else if family("swe-1-7") {
            (82, 22, 30, true)
        } else if family("claude-haiku") || family("haiku") || family("gpt-5-6-luna") {
            (68, 10, 12, true)
        } else {
            (72, 60, 55, false)
        };
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
    Provider::ALL.into_iter().find(|provider| {
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
    remaining: Option<f64>,
) -> i32 {
    utility(class, profile)
        + if is_favorite { 20 } else { 0 }
        + if is_preferred_provider { 30 } else { 0 }
        + remaining
            .map(|value| (value / 2.0).round() as i32)
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
    )
}

fn profile_models_for_class(
    models: &[ModelChoice],
    offers: &OfferState,
    now: u64,
    class: TaskClass,
    frontier: bool,
) -> Vec<ProfiledModel> {
    let mut by_provider: BTreeMap<Provider, Vec<ProfiledModel>> = BTreeMap::new();
    for model in models.iter().filter(|model| model.mode == Mode::Fixed) {
        by_provider
            .entry(model.provider)
            .or_default()
            .push(ProfiledModel {
                key: model.key(),
                label: model.label.clone(),
                provider: model.provider,
                profile: base_profile(model, offers, now),
            });
    }
    let mut rows = Vec::new();
    for (_, mut provider) in by_provider {
        provider.sort_by(|left, right| {
            quality_priority(frontier, &right.profile)
                .cmp(&quality_priority(frontier, &left.profile))
                .then_with(|| utility(class, &right.profile).cmp(&utility(class, &left.profile)))
                .then_with(|| left.key.cmp(&right.key))
        });
        rows.extend(provider.into_iter().take(12));
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
            Provider::Devin => model.effort.is_none(),
            Provider::Claude => true,
        }
}

fn eligible_profiles(
    models: &[ModelChoice],
    offers: &OfferState,
    now: u64,
    task: &str,
    frontier: bool,
    eligible: impl Fn(&ModelChoice) -> bool,
) -> BTreeMap<String, ModelProfile> {
    // Exclusions must precede the bounded shortlist. Otherwise exhausting the
    // first twelve models makes every later catalog entry unreachable.
    let models: Vec<_> = models
        .iter()
        .filter(|model| selectable_model(model) && eligible(model))
        .cloned()
        .collect();
    profile_models_for_class(&models, offers, now, classify_task(task), frontier)
        .into_iter()
        .map(|row| (row.key, row.profile))
        .collect()
}

pub async fn smart_route(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
) -> Result<RouteDecision> {
    let admitted: BTreeSet<_> = Provider::ALL
        .into_iter()
        .filter(|provider| {
            Pin::load(store.root(), *provider).is_ok_and(|pin| runner::provider_admitted(&pin))
        })
        .collect();
    route_with_admitted(store, config, request, &admitted).await
}

async fn route_with_admitted(
    store: &Store,
    config: &Config,
    request: RouteRequest<'_>,
    admitted: &BTreeSet<Provider>,
) -> Result<RouteDecision> {
    let RouteRequest {
        task,
        required_provider,
        preferred_provider,
        required_model,
        excluded_routes,
        excluded_accounts,
        account: account_hint,
    } = request;
    let now = now_ms();
    let offers = crate::offers::load(store.root()).unwrap_or_default();
    let models = store.models()?;
    let view = summary::snapshot(store, None, config, now)?;
    // A connected account is admitted, enabled, credentialed and not waiting
    // for reconnection. Without one, no wait or quota reset can help: the
    // user must add or reconnect an account, and the supervisor says so.
    let connected: Vec<_> = view
        .accounts
        .iter()
        .filter(|account| {
            admitted.contains(&account.provider)
                && required_provider.is_none_or(|provider| provider == account.provider)
                && account.enabled
                && !account.authentication_required
                && account_hint.is_none_or(|hint| hint == &account.id)
                && auth::has_credentials(store, &account.id).unwrap_or(false)
        })
        .collect();
    let accounts: Vec<_> = connected
        .iter()
        .copied()
        .filter(|account| {
            !account.busy
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
    let class = classify_task(task);
    let backend = if config.extensions.judge.enabled {
        judge::resolve(store.root(), &config.extensions.judge)
            .ok()
            .flatten()
    } else {
        None
    };
    let classification =
        task_classifier::classify(task, backend.as_deref(), class == TaskClass::Complex).await;
    let profile_by_key = eligible_profiles(
        &models,
        &offers,
        now,
        task,
        classification.frontier,
        |model| {
            required_model.is_none_or(|key| model.key() == key)
                && accounts.iter().any(|account| {
                    account.provider == model.provider
                        && !excluded_routes.contains(&format!("{} · {}", model.key(), account.id))
                })
        },
    );
    let mut candidates = Vec::new();
    for model in &models {
        let Some(profile) = profile_by_key.get(&model.key()).cloned() else {
            continue;
        };
        for account in &accounts {
            let route_key = format!("{} · {}", model.key(), account.id);
            if account.provider != model.provider || excluded_routes.contains(&route_key) {
                continue;
            }
            let score = route_utility(
                class,
                &profile,
                favorite(config, model),
                preferred_provider == Some(model.provider),
                account.remaining_percent,
            );
            candidates.push(Candidate {
                account: account.id.clone(),
                model: model.clone(),
                profile: profile.clone(),
                utility: score,
            });
        }
    }
    // An expensive/favored route or an external judgment cannot displace the
    // strongest known eligible quality tier when the task demands frontier.
    retain_quality_tier(&mut candidates, classification.frontier);
    candidates.sort_by(|left, right| {
        right
            .utility
            .cmp(&left.utility)
            .then_with(|| left.profile.pareto_layer.cmp(&right.profile.pareto_layer))
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
    if candidates.is_empty() {
        return Err(Error::Unavailable(unavailable_reason()));
    }
    let candidate = candidates.remove(0);
    let warning = quota_degradation_warning(
        &models,
        &connected,
        &offers,
        now,
        &candidate,
        classification.frontier,
        required_model,
        excluded_routes,
        excluded_accounts,
    );
    let reason = format!(
        "{}{} · {} task · Pareto P{} · quality {} · relative cost {} · relative latency {}{}",
        warning.unwrap_or_default(),
        classification.reason(),
        match class {
            TaskClass::Routine => "routine",
            TaskClass::Balanced => "balanced",
            TaskClass::Complex => "complex",
        },
        candidate.profile.pareto_layer,
        candidate.profile.quality,
        candidate.profile.relative_cost,
        candidate.profile.relative_latency,
        candidate
            .profile
            .free_offer
            .as_ref()
            .map(|_| " · public promotion; account eligibility unverified")
            .unwrap_or(""),
    );
    Ok(RouteDecision {
        account: candidate.account,
        model: candidate.model,
        profile: candidate.profile,
        reason,
    })
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
    connected: &[&xcb_core::ui::AccountRow],
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
            && quota_blocks_model(model, connected, now, excluded_routes, excluded_accounts)
    }) {
        NO_QUOTA_AVAILABLE_ROUTE
    } else {
        NO_ELIGIBLE_ROUTE
    }
}

fn quota_blocks_model(
    model: &ModelChoice,
    connected: &[&xcb_core::ui::AccountRow],
    now: u64,
    excluded_routes: &BTreeSet<String>,
    excluded_accounts: &BTreeSet<Id>,
) -> bool {
    connected.iter().any(|account| {
        account.provider == model.provider
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
    connected: &[&xcb_core::ui::AccountRow],
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
                && quota_blocks_model(model, connected, now, excluded_routes, excluded_accounts)
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

    #[tokio::test]
    async fn no_fallback_reports_observed_quota_only_within_requested_routes() {
        use crate::authentication_tests::{account, fail_authentication};
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
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
                result => panic!("expected unavailable route, got {result:?}"),
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
                required_model: Some("codex/unobserved-model"),
                ..request()
            },
            RouteRequest {
                required_provider: Some(Provider::Claude),
                ..request()
            },
            RouteRequest {
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

    #[tokio::test]
    async fn large_prompt_selects_best_known_quality_before_cost_favorites_and_shortlist() {
        use crate::authentication_tests::account;
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let devin = account(&store, Provider::Devin);
        let mut models: Vec<_> = (0..20)
            .map(|n| model(Provider::Devin, &format!("swe-2-variant-{n}"), None, None))
            .collect();
        models.push(model(Provider::Devin, "gpt-6-astra-ultra", None, None));
        store.set_models(Provider::Devin, &models).unwrap();
        let mut config = Config::default();
        config.favorites.insert(
            0,
            xcb_core::models::Preference {
                provider: Provider::Devin,
                model: models[0].id.clone(),
                effort: None,
            },
        );
        let prompt = "details ".repeat(400);
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let admitted = [Provider::Devin].into();
        let request = |required_model| RouteRequest {
            task: &prompt,
            required_provider: Some(Provider::Devin),
            preferred_provider: Some(Provider::Devin),
            required_model,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: Some(&devin),
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
        let excluded = BTreeSet::from([format!("{} · {}", selected.model.key(), devin)]);
        let next = route_with_admitted(
            &store,
            &config,
            RouteRequest {
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
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let codex = account(&store, Provider::Codex);
        account(&store, Provider::Devin);
        store
            .set_models(
                Provider::Codex,
                &[model(Provider::Codex, "gpt-6-astra", None, Some("ultra"))],
            )
            .unwrap();
        store
            .set_models(
                Provider::Devin,
                &[model(Provider::Devin, "swe-2-high", None, None)],
            )
            .unwrap();
        let config = Config::default();
        let prompt = "details ".repeat(400);
        let excluded = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let request = || RouteRequest {
            task: &prompt,
            required_provider: None,
            preferred_provider: None,
            required_model: None,
            excluded_routes: &excluded,
            excluded_accounts: &excluded_accounts,
            account: None,
        };
        let admitted = [Provider::Codex, Provider::Devin].into();
        let work =
            crate::private::directory(&root.path().canonicalize().unwrap().join("work")).unwrap();
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
        assert_eq!(busy.model.provider, Provider::Devin);
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
                required_provider: Some(Provider::Devin),
                ..request()
            },
            &admitted,
        )
        .await
        .unwrap();
        assert!(!constrained.reason.contains("Warning"));
        let unadmitted = route_with_admitted(&store, &config, request(), &[Provider::Devin].into())
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

    #[test]
    fn unknown_model_labels_cannot_displace_the_known_quality_tier() {
        let build = |id, utility| {
            let model = model(Provider::Devin, id, None, None);
            Candidate {
                profile: base_profile(&model, &OfferState::default(), 1),
                model,
                account: Id::new("account").unwrap(),
                utility,
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
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
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
        let store = Store::open(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let config = Config::default();
        let routes = BTreeSet::new();
        let accounts = BTreeSet::new();
        let request = || RouteRequest {
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
                provider: Provider::Devin,
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
    fn fresh_remaining_usage_and_soft_provider_preference_break_route_ties() {
        let profile = ModelProfile {
            quality: 90,
            relative_cost: 40,
            relative_latency: 40,
            pareto_layer: 1,
            recognized: true,
            free_offer: None,
        };
        let low = route_utility(TaskClass::Balanced, &profile, false, false, Some(10.0));
        let high = route_utility(TaskClass::Balanced, &profile, false, false, Some(90.0));
        let preferred = route_utility(TaskClass::Balanced, &profile, false, true, Some(10.0));
        assert!(high > low);
        assert!(preferred > low);
        assert_eq!(
            route_utility(TaskClass::Balanced, &profile, false, false, None),
            utility(TaskClass::Balanced, &profile)
        );
    }

    #[test]
    fn failover_filters_before_bounding_and_layering_the_catalog() {
        let models: Vec<_> = (0..20)
            .map(|index| {
                model(
                    Provider::Devin,
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
        );
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles["devin/swe-2-variant-19"].pareto_layer, 1);
        assert!(
            eligible_profiles(&models, &OfferState::default(), 2, "task", false, |_| false)
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
        let mut adaptive = model(Provider::Devin, "swe-2-high", None, None);
        adaptive.mode = Mode::Adaptive;
        assert!(!selectable_model(&adaptive));
    }

    #[test]
    fn profile_identity_and_effort_do_not_come_from_display_labels() {
        let offers = OfferState::default();
        let mut unknown = model(Provider::Devin, "new-model", None, None);
        unknown.label = "GPT-6 Astra Ultra (Max plan)".into();
        let profile = base_profile(&unknown, &offers, 2);
        assert!(!profile.recognized);
        assert_eq!(profile.quality, 72);
        let near_match = model(Provider::Devin, "swe-20-high", None, None);
        assert!(!base_profile(&near_match, &offers, 2).recognized);
        let resolved = model(
            Provider::Claude,
            "claude-opus-5",
            Some("claude-sonnet-5"),
            Some("low"),
        );
        assert_eq!(base_profile(&resolved, &offers, 2).quality, 83);
        assert_eq!(
            effort(&model(Provider::Devin, "swe-2-xhigh", None, None)),
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
            ("use devin for this task", Provider::Devin),
            ("use claude", Provider::Claude),
        ] {
            assert_eq!(explicit_provider_intent(task), Some(provider), "{task}");
        }
        for task in [
            "Do not use Claude",
            "Don't use Codex for this",
            "Explain when to use Devin",
            "The docs say use Claude.",
            "use codexish tools",
            "use Claude's API",
            "use codex models in the API",
            "\"Use Claude.\" is the example",
        ] {
            assert_eq!(explicit_provider_intent(task), None, "{task}");
        }
    }
}
