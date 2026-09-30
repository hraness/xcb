use crate::{
    Error, Result, attachments, auth,
    config::{Config, ReflexMode},
    digest, exports, hooks, judge, managed, new_id, now_ms, panes, private,
    process::Pin,
    reflex, routing,
    runner::{self, Observer, Outcome, Progress, RunInput},
    store::Store,
    summary,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use xcb_core::{
    Id, Provider,
    models::{ModelChoice, Preference},
    panes::Pane,
    policy::{
        EffectState, Failure, RouteCandidate, Terminal, TurnFacts, failover_permitted, next_route,
        should_continue,
    },
    session::{Message, MessageProvenance, Role, Session, State, Subagent},
    ui::{AccountRow, Intent, RoutePreview, Update, View},
};

/// Shown in a direct session when the provider completed a turn without a
/// reply or file changes, so the empty turn does not read as an answer.
const NO_REPLY_NOTICE: &str = "The turn ended without a reply or file changes. Send another message to continue, or choose another model with /model.";

pub fn choose_model(
    store: &Store,
    provider: Provider,
    requested: Option<&str>,
    config: &Config,
) -> Result<ModelChoice> {
    let mut choices: Vec<_> = store
        .models()?
        .into_iter()
        .filter(|choice| choice.provider == provider)
        .collect();
    xcb_core::models::sort_choices(&mut choices, &config.favorites);
    if let Some(requested) = requested {
        let matches: Vec<_> = choices
            .into_iter()
            .filter(|choice| {
                choice.key() == requested
                    || choice.id.as_str() == requested
                    || choice.label == requested
            })
            .collect();
        return match matches.as_slice() {
            [choice] if config.routing.excluded(choice) => {
                Err(xcb_core::Error::Invalid(crate::routing_stack::EXCLUDED_BY_NEVER).into())
            }
            [choice] => Ok(choice.clone()),
            [] => Err(Error::Unavailable(
                "model not observed; refresh the catalog",
            )),
            _ => Err(Error::Unavailable(
                "model is ambiguous; use its full provider/model/effort key",
            )),
        };
    }
    choices.into_iter().next().ok_or(Error::Unavailable(
        "no observed models; run xcb models refresh for this provider",
    ))
}

/// Picks an account/model route for `--model auto` through the judge: asks a
/// `choice` question over admitted, signable routes. Fails honestly when the
/// extension is off, no key is configured, or the judge rejects the batch —
/// `auto` never silently degrades to a deterministic pick, and an explicit
/// `--model` bypasses the judge entirely.
pub async fn auto_route(
    store: &Store,
    config: &Config,
    prompt: &str,
    account: Option<&Id>,
) -> Result<(Id, ModelChoice)> {
    let judge = judge::resolve(store.root(), &config.extensions.judge)?.ok_or(
        Error::Unavailable("--model auto needs the judge: xcb judge token && xcb judge enable"),
    )?;
    let view = summary::snapshot(store, None, config, now_ms())?;
    let admitted_providers: BTreeSet<_> = Provider::ALL
        .into_iter()
        .filter(|provider| {
            Pin::load(store.root(), *provider)
                .is_ok_and(|pin| runner::provider_admitted(store.root(), &pin))
        })
        .collect();
    let mut candidates: Vec<(Id, ModelChoice, Option<f64>)> = Vec::new();
    for model in &view.models {
        for view_account in &view.accounts {
            if view_account.provider != model.provider
                || view_account.busy
                || !view_account.enabled
                || view_account.authentication_required
                || view_account.quota_blocked_until_ms.is_some()
                || account.is_some_and(|id| id != &view_account.id)
                || candidates.len() >= 16
            {
                continue;
            }
            let admitted = admitted_providers.contains(&model.provider);
            if !admitted || !auth::has_credentials(store, &view_account.id)? {
                continue;
            }
            candidates.push((
                view_account.id.clone(),
                model.clone(),
                view_account.remaining_percent,
            ));
        }
    }
    pick_route(
        judge.as_ref(),
        "Choose the model route for a new coding task.",
        prompt,
        candidates,
    )
    .await
}

/// Asks the judge to pick one route out of the given candidates. Zero
/// candidates is an honest error; one skips the call entirely.
async fn pick_route(
    judge: &dyn judge::Judge,
    context: &str,
    task: &str,
    candidates: Vec<(Id, ModelChoice, Option<f64>)>,
) -> Result<(Id, ModelChoice)> {
    if candidates.len() == 1 {
        let (account, model, _) = candidates.into_iter().next().expect("one candidate");
        return Ok((account, model));
    }
    if candidates.is_empty() || candidates.len() > 64 {
        return Err(Error::Unavailable(
            "no eligible admitted routes; connect an enabled account, finish active turns, or wait for reported quota resets",
        ));
    }
    let mut criteria = std::collections::BTreeMap::new();
    for (rank, (_, model, quota)) in candidates.iter().enumerate() {
        let mut description = format!("{} · {}", model.provider, model.label);
        if let Some(remaining) = quota {
            description.push_str(&format!(" · {remaining:.0}% quota remaining"));
        }
        criteria.insert(format!("route_{rank}"), Some(description));
    }
    let state = serde_json::json!({
        "context": context,
        "task": xcb_core::display_text(task, 8192),
    });
    let mut questions = judge::JudgeQuestions::new();
    questions.insert(
        "route".to_owned(),
        judge::JudgeQuestion::Choice {
            instructions: "Pick the route most likely to complete the task well; descriptions include the account's remaining quota.".to_owned(),
            criteria,
        },
    );
    let answers = judge.ask(&state, &questions).await?;
    let rank = answers
        .answers
        .get("route")
        .and_then(|answer| answer.choice())
        .and_then(|(pick, _)| {
            pick.strip_prefix("route_")
                .and_then(|rest| rest.parse::<usize>().ok())
        })
        .ok_or(Error::Unavailable("judge returned no route"))?;
    candidates
        .into_iter()
        .nth(rank)
        .map(|(account, model, _)| (account, model))
        .ok_or(Error::Unavailable("judge route out of range"))
}

const JUDGE_CONTINUE_THRESHOLD: f64 = 0.7;

struct ContinuationInput<'a> {
    policy: &'a xcb_core::policy::AutoContinue,
    original_task: &'a str,
    last_response: &'a str,
    facts: &'a xcb_core::policy::TurnFacts,
    consecutive: u32,
    elapsed_ms: u64,
    repeated: bool,
}

async fn judge_continuation(
    judge: &dyn judge::Judge,
    input: ContinuationInput<'_>,
) -> Result<bool> {
    if !should_continue(
        input.policy,
        input.facts,
        input.consecutive,
        input.elapsed_ms,
        input.repeated,
    ) {
        return Ok(false);
    }
    let state = serde_json::json!({
        "context": "All deterministic continuation safety checks passed. Decide only whether the same task remains unfinished and can proceed without user input.",
        "original_task": xcb_core::display_text(input.original_task, 8192),
        "last_response": xcb_core::display_text(input.last_response, 8192),
        "turn": {
            "terminal": input.facts.terminal,
            "consecutive_continuations": input.consecutive,
            "elapsed_ms": input.elapsed_ms,
        },
    });
    let mut questions = judge::JudgeQuestions::new();
    questions.insert(
        "continue_task".to_owned(),
        judge::JudgeQuestion::Noul {
            instructions: "Should xcb automatically continue this exact coding task from the last confirmed checkpoint? Answer true only when the response plainly leaves unfinished work that can proceed without approval, clarification, missing input, repeated effects, or task expansion.".to_owned(),
            criteria: Some(judge::NoulCriteria {
                r#true: Some("The same task is clearly unfinished and safe to resume now.".to_owned()),
                r#false: Some("The task is complete, ambiguous, blocked, repetitive, or needs the user.".to_owned()),
            }),
        },
    );
    let answers = judge.ask(&state, &questions).await?;
    Ok(answers
        .answers
        .get("continue_task")
        .and_then(judge::JudgeAnswer::noul)
        .is_some_and(|probability| probability >= JUDGE_CONTINUE_THRESHOLD))
}

/// A settle head's verdict on a completed direct turn: `Some((head,
/// decision))` when the turn stopped short of the task or asked to confirm a
/// routine step and the head may act. This is the envelope the managed
/// supervisor applies (`task_should_continue_inbox`): the turn is joined
/// with no failure, no denied request and no step only the user can take;
/// it is not a repeat; the continuation budget holds; the `confirm` head
/// also needs no risk cue and the veto list clear; under `auto` the head
/// must be certified by the operator's labels, and about one turn in ten is
/// left to them as unbiased evidence.
async fn settle_continuation(
    store: &Store,
    config: &Config,
    outcome: &Outcome,
    session: &Id,
    budget: ContinuationBudget,
) -> Option<(&'static str, reflex::Decision)> {
    let policy = &config.extensions.auto_continue;
    if !policy.enabled
        || budget.repeated
        || budget.consecutive >= policy.max_consecutive
        || budget.elapsed_ms >= policy.max_elapsed_ms
        || !outcome.askable()
        || outcome.denied()
        || outcome.facts.terminal != Terminal::Completed
        || !outcome.facts.joined
        || outcome.facts.effects == EffectState::Uncertain
        || outcome.facts.failure.is_some()
    {
        return None;
    }
    let decision = managed::settle_decision(store, config, outcome).await?;
    if xcb_core::reflex::owner_only(&decision.features) {
        return None;
    }
    let reflexes = &config.extensions.reflexes;
    let head = match decision.value.as_str() {
        "stopped_short" => xcb_core::reflex::SETTLE_UNFINISHED,
        "confirm" if managed::confirmable(&decision, &outcome.text) => {
            xcb_core::reflex::SETTLE_CONFIRM
        }
        _ => return None,
    };
    let acts = managed::head_acts(store.root(), reflexes, head, &decision.features)
        && !(managed::head_mode(reflexes, head) == ReflexMode::Auto
            && managed::held_turn(head, session, budget.turn));
    acts.then_some((head, decision))
}

/// Where a direct session stands in its continuation budget when a turn
/// settles: the turn number within this call, automatic turns in a row,
/// time since the call started, and whether the output repeats the last.
#[derive(Clone, Copy)]
struct ContinuationBudget {
    turn: u64,
    consecutive: u32,
    elapsed_ms: u64,
    repeated: bool,
}

async fn configured_judge_continuation(
    root: &Path,
    config: &crate::config::JudgeConfig,
    input: ContinuationInput<'_>,
) -> Result<bool> {
    let judge = judge::resolve(root, config)?.ok_or(Error::Unavailable("judge key missing"))?;
    judge_continuation(judge.as_ref(), input).await
}

/// Hard route constraints checked after resolving explicit model aliases.
#[derive(Clone, Copy, Default)]
pub struct SessionRoutePolicy {
    pub requirements: xcb_core::session::TaskRequirements,
    pub required_provider: Option<Provider>,
}

/// `managed_task` marks the session with the owning managed task atomically
/// at creation, so reconciliation can prove custody of an orphan if the
/// supervisor dies before `prepare` admits it. Direct/interactive sessions
/// pass `None` and are never swept.
pub fn new_session(
    store: &Store,
    workspace: &Path,
    config: &Config,
    account: Option<&Id>,
    model: Option<&str>,
    managed_task: Option<&Id>,
) -> Result<Session> {
    new_session_with_policy(
        store,
        workspace,
        config,
        account,
        model,
        managed_task,
        SessionRoutePolicy::default(),
    )
}

/// Resolve the established account/model aliases, then check hard route
/// requirements before persisting any session or launching a provider.
pub fn new_session_with_policy(
    store: &Store,
    workspace: &Path,
    config: &Config,
    account: Option<&Id>,
    model: Option<&str>,
    managed_task: Option<&Id>,
    policy: SessionRoutePolicy,
) -> Result<Session> {
    // An explicit model chooses its provider when no account was supplied.
    // The saved default is a preference, not a cross-provider override.
    let requested_provider = if account.is_none() {
        model
            .map(|requested| {
                let matches: Vec<_> = store
                    .models()?
                    .into_iter()
                    .filter(|choice| {
                        choice.key() == requested
                            || choice.id.as_str() == requested
                            || choice.label == requested
                    })
                    .collect();
                match matches.as_slice() {
                    [] => Err(Error::Unavailable(
                        "model not observed; refresh the catalog",
                    )),
                    [choice] => Ok(choice.provider),
                    choices
                        if choices
                            .iter()
                            .all(|choice| choice.provider == choices[0].provider) =>
                    {
                        Ok(choices[0].provider)
                    }
                    _ => Err(Error::Unavailable(
                        "model is ambiguous; use its full provider/model/effort key",
                    )),
                }
            })
            .transpose()?
    } else {
        None
    };
    let accounts = store.accounts()?;
    let now = now_ms();
    let mut unavailable_accounts = BTreeSet::new();
    for candidate in &accounts {
        if candidate.enabled
            && requested_provider.is_none_or(|provider| candidate.provider == provider)
            && (store.quota_blocked_until(&candidate.id, now)?.is_some()
                || store.authentication_required(&candidate.id)?)
        {
            unavailable_accounts.insert(candidate.id.clone());
        }
    }
    let compatible = |candidate: &&crate::store::Account| {
        candidate.enabled
            && !unavailable_accounts.contains(&candidate.id)
            && requested_provider.is_none_or(|provider| candidate.provider == provider)
    };
    let id = match account {
        Some(id) => {
            let selected = store.account(id)?;
            if !selected.enabled {
                return Err(Error::Unavailable("selected account is disabled"));
            }
            store.require_quota_available(id, now)?;
            store.require_authenticated_account(id)?;
            id.clone()
        }
        None => usable_account(store, requested_provider, None, config)?
            // Keep account setup possible before sign-in, while never choosing
            // an unsigned/busy account over a connected idle matching route.
            .or_else(|| {
                accounts
                    .iter()
                    .filter(compatible)
                    .find(|candidate| config.default_account.as_ref() == Some(&candidate.id))
                    .or_else(|| accounts.iter().find(compatible))
                    .map(|candidate| candidate.id.clone())
            })
            .ok_or(Error::Unavailable(
                "no eligible account for this provider; connect an enabled account or wait for its reported quota reset",
            ))?,
    };
    let account = store.account(&id)?;
    let model = choose_model(store, account.provider, model, config)?;
    if !policy.requirements.allows(model.provider) {
        return Err(Error::Conflict(
            "signed-in browser tasks require Codex; remove the incompatible provider, account, or model pin",
        ));
    }
    if policy
        .required_provider
        .is_some_and(|provider| provider != model.provider)
    {
        return Err(Error::Conflict(
            "explicit provider conflicts with the selected account or model",
        ));
    }
    let session = match managed_task {
        Some(task) => store.create_managed_session(&id, model, workspace, now_ms(), task)?,
        None => store.create_session(&id, model, workspace, now_ms())?,
    };
    store.select_pane(&session.id, &config.pane)?;
    store
        .session(&session.id)?
        .ok_or(Error::Unavailable("session not found"))
}

fn credential_guidance(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "connect this Claude account with xcb accounts login <account>",
        Provider::Codex => {
            "connect this Codex account with xcb accounts login <account>, or explicitly import auth.json with xcb accounts import-codex --source <path>"
        }
        Provider::Devin => {
            "connect this Devin account by piping a token into xcb accounts token <account>, or copy a CLI sign-in into a new account with xcb accounts import-devin --source <absolute credentials.toml path>"
        }
    }
}

/// Select among this provider's usable accounts without mutating any custody.
/// Rebinding and launch still validate session revisions and exclusive leases.
fn usable_account(
    store: &Store,
    provider: Option<Provider>,
    current: Option<&Id>,
    config: &Config,
) -> Result<Option<Id>> {
    let held: BTreeSet<_> = store
        .unsettled_runs()?
        .into_iter()
        .map(|run| run.account)
        .collect();
    let now = now_ms();
    let mut accounts = store.accounts()?;
    let mut remaining: BTreeMap<Id, f64> = BTreeMap::new();
    for account in &accounts {
        if let Some(percent) = store.remaining_percent(&account.quota_pool, now)? {
            remaining.insert(account.id.clone(), percent);
        }
    }
    // The session's account stays sticky across a model switch. Otherwise
    // usable accounts with measured quota order by remaining headroom —
    // unmeasured accounts cannot claim availability and sort behind them,
    // with the configured default leading that tail.
    accounts.sort_by(|left, right| {
        let rank = |account: &crate::store::Account| {
            if current == Some(&account.id) {
                0
            } else if remaining.contains_key(&account.id) {
                1
            } else if config.default_account.as_ref() == Some(&account.id) {
                2
            } else {
                3
            }
        };
        rank(left).cmp(&rank(right)).then_with(|| {
            remaining
                .get(&right.id)
                .copied()
                .partial_cmp(&remaining.get(&left.id).copied())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    for account in accounts {
        if provider.is_none_or(|provider| account.provider == provider)
            && account.enabled
            && !store.authentication_required(&account.id)?
            && !held.contains(&account.id)
            && store.quota_blocked_until(&account.id, now_ms())?.is_none()
            && auth::has_credentials(store, &account.id)?
        {
            return Ok(Some(account.id));
        }
    }
    Ok(None)
}

fn model_account(
    store: &Store,
    provider: Provider,
    current: Option<&Id>,
    config: &Config,
) -> Result<Id> {
    usable_account(store, Some(provider), current, config)?.ok_or(Error::Unavailable(
        "no connected idle account for this provider; connect an enabled account, finish active turns, or wait for reported quota resets",
    ))
}

fn workspace_lease(store: &Store, session: &Session) -> Result<private::ExclusiveLock> {
    let directory = private::directory(&store.root().join("workspace-runs"))?;
    let path = directory.join(format!("{}.lock", digest(session.workspace.as_bytes())));
    let file = crate::os::owner_only(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false),
    )
    .open(path)?;
    private::check_file(&file, 4096)?;
    match file.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::Conflict("workspace has an active writer"));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let file = private::ExclusiveLock::held(file);
    // Inspect durable custody only after excluding competing launchers. An
    // earlier owner may have released this lock with an unsettled run between
    // a pre-lock check and acquisition; the filesystem lock alone is no proof
    // that its provider and effects have stopped. A run in a nested or
    // enclosing directory writes the same files, so it blocks too.
    for run in store.unsettled_runs()? {
        if let Some(id) = run.session
            && store.session(&id)?.is_some_and(|active| {
                crate::workspace_infer::workspaces_overlap(&active.workspace, &session.workspace)
            })
        {
            return Err(Error::Conflict("workspace has an unsettled writer"));
        }
    }
    Ok(file)
}

fn ready(store: &Store, session: &Session) -> Result<()> {
    if !store.account(&session.account)?.enabled {
        return Err(Error::Unavailable("selected account is disabled"));
    }
    store.require_quota_available(&session.account, now_ms())?;
    store.require_authenticated_account(&session.account)?;
    let pin = Pin::load(store.root(), session.model.provider)?;
    if !runner::provider_admitted(store.root(), &pin) {
        return Err(Error::Unavailable(
            "native execution for this provider/runtime is not qualified; run xcb doctor",
        ));
    }
    if !auth::has_credentials(store, &session.account)? {
        return Err(Error::Unavailable(credential_guidance(
            session.model.provider,
        )));
    }
    if store
        .unsettled_runs()?
        .iter()
        .any(|run| run.account == session.account)
    {
        return Err(Error::Conflict("account has an unsettled run"));
    }
    Ok(())
}

async fn fire_hooks(
    store: &Store,
    config: &Config,
    event: hooks::Event,
    session: &Session,
    state: State,
    observer: &Observer,
) {
    if !config.extensions.hooks {
        return;
    }
    match hooks::fire(
        store.root(),
        event,
        &hooks::HookInput::new(event, session, state),
    )
    .await
    {
        Ok(notices) => notices
            .into_iter()
            .for_each(|notice| observer(Progress::Notice(notice))),
        Err(error) => observer(Progress::Notice(format!("Hook dispatch failed: {error}"))),
    }
}

#[derive(Clone, Copy)]
enum ExecutionMode {
    Direct,
    Managed,
    Pane,
}
impl ExecutionMode {
    fn pane_generation(self) -> bool {
        matches!(self, Self::Pane)
    }
    fn supervise(self) -> bool {
        matches!(self, Self::Direct)
    }
}

pub async fn execute(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    pane_generation: bool,
    cancel: watch::Receiver<bool>,
    observer: Observer,
) -> Result<Outcome> {
    execute_mode(
        store,
        session_id,
        text,
        attachments,
        if pane_generation {
            ExecutionMode::Pane
        } else {
            ExecutionMode::Direct
        },
        cancel,
        observer,
        None,
    )
    .await
}

pub async fn execute_once(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    cancel: watch::Receiver<bool>,
    observer: Observer,
) -> Result<Outcome> {
    execute_mode(
        store,
        session_id,
        text,
        attachments,
        ExecutionMode::Managed,
        cancel,
        observer,
        None,
    )
    .await
}

struct UiSubmission {
    id: Id,
    outbox: Arc<Mutex<Outbox>>,
    accepted: std::sync::atomic::AtomicBool,
}
impl UiSubmission {
    fn accepted(&self) -> bool {
        self.accepted.load(std::sync::atomic::Ordering::Acquire)
    }
    fn acknowledge(&self, session: &Id) {
        if !self
            .accepted
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            queue(
                &self.outbox,
                Update::Submitted {
                    id: self.id.clone(),
                    context: xcb_core::ui::TranscriptContext::Session(session.clone()),
                },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_mode(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    mode: ExecutionMode,
    cancel: watch::Receiver<bool>,
    observer: Observer,
    submission: Option<&UiSubmission>,
) -> Result<Outcome> {
    let pane_generation = mode.pane_generation();
    let config = Config::load(store.root())?.0;
    let mut session = store
        .session(&session_id)?
        .ok_or(Error::Unavailable("session not found"))?;
    if !session.requirements.allows(session.model.provider) {
        let prior = store.latest_settled_outcome(&session_id)?;
        if !prior.as_ref().is_some_and(|outcome| {
            signed_in_browser_handoff_permitted(outcome, pane_generation, *cancel.borrow())
        }) {
            return Err(Error::Unavailable(
                "signed-in browser handoff needs a current joined turn with settled effects and no pending approval",
            ));
        }
        let excluded_routes = BTreeSet::new();
        let excluded_accounts = BTreeSet::new();
        let decision = routing::smart_route(
            &store,
            &config,
            routing::RouteRequest {
                requirements: session.requirements,
                task: &text,
                required_provider: session.route_pins.provider,
                preferred_provider: Some(Provider::Codex),
                required_model: session.route_pins.model.as_deref(),
                excluded_routes: &excluded_routes,
                excluded_accounts: &excluded_accounts,
                account: session.route_pins.account.as_ref(),
            },
        )
        .await?;
        if *cancel.borrow() {
            return Err(Error::Unavailable(
                "cancelled before signed-in browser handoff",
            ));
        }
        store.rebind(
            &session_id,
            session.revision,
            &decision.account,
            decision.model,
        )?;
        session = store
            .session(&session_id)?
            .ok_or(Error::Unavailable("session not found"))?;
        observer(Progress::Notice(format!(
            "Signed-in browser access: continuing on Codex · {}",
            session.model.label
        )));
    }
    let _workspace = workspace_lease(&store, &session)?;
    fire_hooks(
        &store,
        &config,
        hooks::Event::SessionStart,
        &session,
        session.state,
        &observer,
    )
    .await;
    let result = execute_inner(
        store.clone(),
        session_id.clone(),
        text,
        attachments,
        mode,
        cancel,
        observer.clone(),
        submission,
    )
    .await;
    if let Some(session) = store.session(&session_id)? {
        let config = Config::load(store.root())?.0;
        let state = result
            .as_ref()
            .map_or(State::Uncertain, |outcome| outcome.state);
        if config.extensions.aicharts_export
            && config.extensions.usage
            && let Ok(ref outcome) = result
            && runner::should_idle_export(pane_generation, &outcome.facts, outcome.state)
            && let Err(error) = exports::export_session(&store, &session_id)
        {
            observer(Progress::Notice(format!(
                "aicharts local idle export failed: {error}"
            )));
        }
        fire_hooks(
            &store,
            &config,
            hooks::Event::SessionEnd,
            &session,
            state,
            &observer,
        )
        .await;
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn execute_inner(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    mode: ExecutionMode,
    mut cancel: watch::Receiver<bool>,
    observer: Observer,
    submission: Option<&UiSubmission>,
) -> Result<Outcome> {
    let pane_generation = mode.pane_generation();
    let supervise = mode.supervise();
    let started = now_ms();
    let mut consecutive = 0u32;
    // Turns this call ran, for the reflex ledger's observation subject.
    let mut turn = 0u64;
    let mut previous_output = None;
    let mut tried = BTreeSet::new();
    // Accounts that reported an account-wide usage limit during this task.
    // A later failover never returns to them, even on another model.
    let mut limited_accounts: BTreeSet<Id> = BTreeSet::new();
    let original_task = text.clone();
    let mut text = text;
    let mut attachments = attachments;
    let mut role = Role::User;
    loop {
        if *cancel.borrow() {
            return Err(Error::Unavailable("cancelled before the next turn"));
        }
        let config = Config::load(store.root())?.0;
        let session = store
            .session(&session_id)?
            .ok_or(Error::Unavailable("session not found"))?;
        ready(&store, &session)?;
        tried.insert(format!("{}/{}", session.account, session.model.key()));
        let message = Message {
            id: submission
                .filter(|input| !input.accepted())
                .map_or_else(|| new_id("m"), |input| input.id.clone()),
            role,
            text,
            attachments,
            at_ms: now_ms(),
            provenance: Some(MessageProvenance {
                account: session.account.clone(),
                model: session.model.clone(),
                run: None,
            }),
        };
        let current = append_input(&store, &session, &message, submission)?;
        fire_hooks(
            &store,
            &config,
            hooks::Event::TurnStart,
            &current,
            State::Working,
            &observer,
        )
        .await;
        let result = runner::run(
            store.clone(),
            RunInput {
                session: current.clone(),
                message,
                config: config.clone(),
                pane_generation,
            },
            cancel.clone(),
            observer.clone(),
        )
        .await;
        let state = result
            .as_ref()
            .map_or(State::Uncertain, |outcome| outcome.state);
        fire_hooks(
            &store,
            &config,
            hooks::Event::TurnEnd,
            &current,
            state,
            &observer,
        )
        .await;
        let outcome = result?;
        // Capability discovery is a handoff, including one-shot managed turns.
        // Never report its provider notice as successful task completion.
        let durable = store
            .session(&session_id)?
            .ok_or(Error::Unavailable("session not found"))?;
        if !durable.requirements.allows(session.model.provider) {
            if !supervise {
                return Ok(outcome);
            }
            if !signed_in_browser_handoff_permitted(&outcome, pane_generation, *cancel.borrow()) {
                return Err(Error::Unavailable(
                    "signed-in browser handoff is blocked until the prior provider is joined and all effects and attention are settled",
                ));
            }
            let excluded_routes = BTreeSet::new();
            let excluded_accounts = BTreeSet::new();
            let decision = routing::smart_route(
                &store,
                &config,
                routing::RouteRequest {
                    requirements: durable.requirements,
                    task: &original_task,
                    required_provider: durable.route_pins.provider,
                    preferred_provider: Some(Provider::Codex),
                    required_model: durable.route_pins.model.as_deref(),
                    excluded_routes: &excluded_routes,
                    excluded_accounts: &excluded_accounts,
                    account: durable.route_pins.account.as_ref(),
                },
            )
            .await?;
            if *cancel.borrow() {
                return Err(Error::Unavailable(
                    "cancelled before signed-in browser handoff",
                ));
            }
            store.rebind(
                &session_id,
                durable.revision,
                &decision.account,
                decision.model.clone(),
            )?;
            observer(Progress::Notice(format!(
                "Signed-in browser access: continuing the same task on Codex · {}",
                decision.model.label
            )));
            text = "The previous provider discovered that this task requires the user's existing signed-in browser and has stopped cleanly. Continue the original user task from the complete conversation and current workspace. Do not repeat completed effects; inspect relevant state first. Use the available signed-in browser capability for the requested account operations, and retain all existing task scope and permissions.".into();
            attachments = vec![];
            role = Role::System;
            continue;
        }
        if !supervise || pane_generation || *cancel.borrow() {
            return Ok(outcome);
        }
        let current = store
            .session(&session_id)?
            .ok_or(Error::Unavailable("session not found"))?;
        if current.account != session.account || current.model.key() != session.model.key() {
            return Ok(outcome);
        }
        turn += 1;
        let current_config = Config::load(store.root())?.0;
        let output_digest = digest(&outcome.text);
        let repeat = previous_output.as_ref() == Some(&output_digest);
        let elapsed_ms = now_ms().saturating_sub(started);
        let deterministic_continue = should_continue(
            &current_config.extensions.auto_continue,
            &outcome.facts,
            consecutive,
            elapsed_ms,
            repeat,
        );
        // A completed turn the settle reflex reads as stopped short, or as a
        // routine request for a go-ahead, continues the way the managed
        // supervisor continues it; the deterministic gate covers limits.
        let settled = if deterministic_continue {
            None
        } else {
            tokio::select! {
                biased;
                _ = cancellation_requested(&mut cancel) => return Ok(outcome),
                settled = settle_continuation(
                    &store,
                    &current_config,
                    &outcome,
                    &session_id,
                    ContinuationBudget { turn, consecutive, elapsed_ms, repeated: repeat },
                ) => settled,
            }
        };
        if let Some((head, decision)) = &settled {
            if let Ok(reflexes) = reflex::ReflexStore::open(store.root()) {
                let _ = reflexes.observe(&format!("{}#{turn}", session_id.as_str()), decision);
            }
            observer(Progress::Notice(format!(
                "Settle reflex `{head}` continues the task on its own"
            )));
        }
        let continue_turn = if (deterministic_continue || settled.is_some())
            && current_config.extensions.judge.enabled
        {
            let judgment = tokio::select! {
                biased;
                _ = cancellation_requested(&mut cancel) => return Ok(outcome),
                judgment = configured_judge_continuation(
                store.root(),
                &current_config.extensions.judge,
                ContinuationInput {
                    policy: &current_config.extensions.auto_continue,
                    original_task: &original_task,
                    last_response: &outcome.text,
                    facts: &outcome.facts,
                    consecutive,
                    elapsed_ms,
                    repeated: repeat,
                },
                ) => judgment,
            };
            match judgment {
                Ok(decision) => {
                    observer(Progress::Notice(
                        if decision {
                            "Judge advised continuing the same task"
                        } else {
                            "Judge stopped automatic continuation"
                        }
                        .to_owned(),
                    ));
                    decision
                }
                Err(error) => {
                    observer(Progress::Notice(format!(
                        "Judge continuation unavailable ({error}); stopping"
                    )));
                    false
                }
            }
        } else {
            deterministic_continue || settled.is_some()
        };
        if *cancel.borrow() {
            return Ok(outcome);
        }
        // The judge may have taken time: the elapsed budget is checked again
        // here, for continuation only. Failover is bounded by the routes
        // tried and by cancellation, so a long turn that then hits a usage
        // limit still moves to another account.
        let step = supervision_step(
            &current_config,
            &outcome,
            continue_turn,
            now_ms().saturating_sub(started),
        );
        if step == Supervision::Continue {
            consecutive += 1;
            previous_output = Some(output_digest);
            observer(Progress::Notice(format!(
                "Auto-continue {consecutive}/{} · same task and permissions",
                current_config.extensions.auto_continue.max_consecutive
            )));
            text = match &settled {
                Some((head, decision)) => {
                    xcb_core::reflex::continuation_prompt(Some(head), Some(&decision.features))
                }
                None => "Continue the existing task from the last confirmed checkpoint. Do not repeat completed effects, expand the task, or answer for the user. Stop if approval or missing input is required.".into(),
            };
            attachments = vec![];
            role = Role::System;
            continue;
        }
        if let Supervision::Failover(failure) = step {
            if failure == Failure::AccountQuota {
                limited_accounts.insert(current.account.clone());
            }
            let now = now_ms();
            let view = summary::snapshot(&store, Some(&session_id), &current_config, now)?;
            let checkpointed = checkpointed(&outcome);
            let limit = usage_limit_label(&view, &current.account, &current.model, failure);
            if let Some(reason) = failover_blocked_reason(&outcome.facts, &tried, checkpointed) {
                observer(Progress::Notice(format!("{limit} · {reason}")));
                return Ok(outcome);
            }
            let source = RouteCandidate {
                account: current.account.clone(),
                model: current.model.clone(),
                admitted: true,
                quota_clear: false,
                available: false,
            };
            let admitted_providers: BTreeSet<_> = Provider::ALL
                .into_iter()
                .filter(|provider| {
                    Pin::load(store.root(), *provider)
                        .is_ok_and(|pin| runner::provider_admitted(store.root(), &pin))
                })
                .collect();
            let credentialed: BTreeSet<Id> = view
                .accounts
                .iter()
                .filter(|account| auth::has_credentials(&store, &account.id).unwrap_or(false))
                .map(|account| account.id.clone())
                .collect();
            // An opening "Use <provider>" directive pins the provider for the
            // whole task; failover never widens past it.
            let required_provider = current
                .route_pins
                .provider
                .or_else(|| routing::explicit_provider_intent(&original_task));
            // The router applies the same eligibility as automatic routing
            // (a Devin account without a meter, or one whose last reading
            // aged out, is a target) and orders the routes; `next_route`
            // stays the safety gate over facts read from the account view.
            let ranked = tokio::select! {
                biased;
                _ = cancellation_requested(&mut cancel) => return Ok(outcome),
                ranked = routing::failover_routes(
                    &store,
                    &current_config,
                    routing::FailoverRequest {
                    requirements: current.requirements,
                        task: &original_task,
                        account: &current.account,
                        model: &current.model,
                        failure,
                        tried: &tried,
                        limited_accounts: &limited_accounts,
                        required_provider,
                        required_model: current.route_pins.model.as_deref(),
                        required_account: current.route_pins.account.as_ref(),
                    },
                ) => ranked?,
            };
            let mut candidates: Vec<_> = ranked
                .into_iter()
                .filter_map(|route| {
                    let row = view.accounts.iter().find(|row| row.id == route.account)?;
                    let admitted = admitted_providers.contains(&route.model.provider)
                        && credentialed.contains(&row.id);
                    Some(failover_candidate(row, route.model, admitted))
                })
                .collect();
            let eligible = eligible_failover_routes(&source, &candidates, &tried, &outcome);
            if eligible.is_empty() {
                observer(Progress::Notice(failover_unavailable_notice(
                    &FailoverNoticeInput {
                        view: &view,
                        account: &current.account,
                        model: &current.model,
                        failure,
                        tried: &tried,
                        limited_accounts: &limited_accounts,
                        admitted: &admitted_providers,
                        credentialed: &credentialed,
                        required_provider,
                        now,
                    },
                )));
                return Ok(outcome);
            }
            // Ask the judge to rank the routes `next_route` could pick; on any
            // failure — no key, unreadable vault, bad endpoint — the
            // deterministic order stands and the run keeps its contract.
            let failover_judge =
                match judge::resolve(store.root(), &current_config.extensions.judge) {
                    Ok(judge) => judge,
                    Err(error) => {
                        observer(Progress::Notice(format!(
                            "Judge unavailable ({error}); deterministic route order"
                        )));
                        None
                    }
                };
            if let Some(judge) = failover_judge
                && eligible.len() > 1
            {
                let mut criteria = std::collections::BTreeMap::new();
                for (rank, index) in eligible.iter().enumerate() {
                    let candidate = &candidates[*index];
                    criteria.insert(
                        format!("route_{rank}"),
                        Some(format!(
                            "{} · {} · {}",
                            candidate.model.provider, candidate.model.label, candidate.account
                        )),
                    );
                }
                let state = serde_json::json!({
                    "context": "A coding task lost its current route to a provider usage limit. Choose the best remaining route for the task; all listed routes are admitted and have quota.",
                    "task": xcb_core::display_text(&original_task, 8192),
                    "failure": format!("{:?}", outcome.facts.failure),
                });
                let mut questions = judge::JudgeQuestions::new();
                questions.insert(
                    "route".to_owned(),
                    judge::JudgeQuestion::Choice {
                        instructions: "Pick the route most likely to complete the task well."
                            .to_owned(),
                        criteria,
                    },
                );
                let judgment = tokio::select! {
                    biased;
                    _ = cancellation_requested(&mut cancel) => return Ok(outcome),
                    judgment = judge.ask(&state, &questions) => judgment,
                };
                match judgment {
                    Ok(answers) => {
                        if let Some((pick, _)) = answers
                            .answers
                            .get("route")
                            .and_then(|answer| answer.choice())
                            && let Some(index) = pick
                                .strip_prefix("route_")
                                .and_then(|rest| rest.parse::<usize>().ok())
                                .and_then(|rank| eligible.get(rank))
                        {
                            let chosen = candidates.remove(*index);
                            observer(Progress::Notice(
                                "Judge selected an admitted failover route".to_owned(),
                            ));
                            candidates.insert(0, chosen);
                        }
                    }
                    Err(error) => observer(Progress::Notice(format!(
                        "Judge routing unavailable ({error}); deterministic order"
                    ))),
                }
            }
            if *cancel.borrow() {
                return Ok(outcome);
            }
            if let Some(target) =
                next_route(&source, &candidates, &tried, &outcome.facts, checkpointed)
            {
                store.rebind(
                    &session_id,
                    current.revision,
                    &target.account,
                    target.model.clone(),
                )?;
                observer(Progress::Notice(format!(
                    "Usage limit: continuing the checkpoint on {} · {}",
                    target.model.provider, target.model.label
                )));
                text = "The prior provider reached a usage limit. Continue from the confirmed conversation and current workspace. Check current files before changing them; do not blindly replay prior operations. Stay within the existing task and ask for missing information.".into();
                attachments = vec![];
                role = Role::System;
                continue;
            }
        }
        return Ok(outcome);
    }
}

fn signed_in_browser_handoff_permitted(
    outcome: &Outcome,
    pane_generation: bool,
    cancelled: bool,
) -> bool {
    !cancelled
        && !pane_generation
        && outcome.facts.joined
        && outcome.facts.effects != EffectState::Uncertain
        && !outcome.facts.pending_attention
        && outcome.facts.failure.is_none()
        && matches!(
            outcome.facts.terminal,
            Terminal::Completed | Terminal::TurnLimit
        )
        && outcome.state == State::Idle
}

fn append_input(
    store: &Store,
    session: &Session,
    message: &Message,
    submission: Option<&UiSubmission>,
) -> Result<Session> {
    let current = store.append_message(&session.id, session.revision, message)?;
    if let Some(input) = submission
        && message.id == input.id
    {
        input.acknowledge(&session.id);
    }
    Ok(current)
}

async fn cancellation_requested(cancel: &mut watch::Receiver<bool>) {
    // Sender loss also ends supervision, just as it ends an active runner.
    let _ = cancel.wait_for(|cancelled| *cancelled).await;
}

/// What supervision does after a settled turn, decided from the turn alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Supervision {
    Continue,
    Failover(Failure),
    Stop,
}

/// Continuation keeps its own elapsed budget. Failover has none: it is
/// bounded by the sixteen routes `next_route` allows and by cancellation, so
/// a turn that ran past the continuation budget and then hit a usage limit
/// still moves to another account.
fn supervision_step(
    config: &Config,
    outcome: &Outcome,
    continue_turn: bool,
    elapsed_ms: u64,
) -> Supervision {
    if continue_turn && elapsed_ms < config.extensions.auto_continue.max_elapsed_ms {
        return Supervision::Continue;
    }
    match outcome.facts.failure {
        Some(failure @ (Failure::AccountQuota | Failure::ModelQuota))
            if config.auto_failover && outcome.facts.terminal == Terminal::Failed =>
        {
            Supervision::Failover(failure)
        }
        _ => Supervision::Stop,
    }
}

/// A turn left something to continue from: answer text, or no effects at all.
fn checkpointed(outcome: &Outcome) -> bool {
    !outcome.text.is_empty() || outcome.facts.effects == EffectState::None
}

/// The facts the failover gate reads about one route, taken from the account
/// view the terminal shows. The router already applied the same rules; the
/// gate re-reads them so a candidate can never be admitted by construction.
fn failover_candidate(account: &AccountRow, model: ModelChoice, admitted: bool) -> RouteCandidate {
    RouteCandidate {
        account: account.id.clone(),
        model,
        admitted: admitted && !account.authentication_required,
        quota_clear: account.quota_blocked_until_ms.is_none()
            && account
                .remaining_percent
                .is_none_or(|remaining| remaining > 0.0),
        available: account.enabled && !account.busy,
    }
}

/// Why a usage-limited turn cannot move at all, before any route is ranked.
fn failover_blocked_reason(
    facts: &TurnFacts,
    tried: &BTreeSet<String>,
    checkpointed: bool,
) -> Option<&'static str> {
    if failover_permitted(facts, tried, checkpointed) {
        return None;
    }
    Some(
        if !facts.joined || facts.effects == EffectState::Uncertain {
            "xcb could not confirm how the run ended, so it keeps this account and does not switch"
        } else if facts.pending_attention {
            "the provider is waiting for an answer, so xcb does not switch"
        } else if !checkpointed {
            "the turn changed files without a reply, so xcb does not continue it on another account"
        } else if tried.len() >= 16 {
            "16 routes already ran this task, so xcb stops here"
        } else {
            "the turn did not settle with a usage limit"
        },
    )
}

fn usage_limit_label(view: &View, account: &Id, model: &ModelChoice, failure: Failure) -> String {
    let name = view
        .accounts
        .iter()
        .find(|row| &row.id == account)
        .map_or_else(|| account.to_string(), |row| row.name.clone());
    match failure {
        Failure::ModelQuota => format!(
            "Usage limit for {} on {} · {name}",
            model.label, model.provider
        ),
        _ => format!("Usage limit on {} · {name}", model.provider),
    }
}

/// Everything the no-target notice is written from.
pub struct FailoverNoticeInput<'a> {
    pub view: &'a View,
    pub account: &'a Id,
    pub model: &'a ModelChoice,
    pub failure: Failure,
    /// Routes this task already ran, as `<account>/<model key>`.
    pub tried: &'a BTreeSet<String>,
    pub limited_accounts: &'a BTreeSet<Id>,
    /// Providers whose pinned build xcb can run.
    pub admitted: &'a BTreeSet<Provider>,
    /// Accounts with usable credentials.
    pub credentialed: &'a BTreeSet<Id>,
    pub required_provider: Option<Provider>,
    pub now: u64,
}

fn wait_label(until: u64, now: u64) -> String {
    let minutes = until.saturating_sub(now).div_ceil(60_000).max(1);
    if minutes >= 60 * 24 {
        format!("{}d", minutes / (60 * 24))
    } else if minutes >= 60 {
        format!("{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// The notice shown when a usage limit stopped a turn and no other account
/// can take the task now: which account hit the limit, why each other
/// account was passed over, and the earliest known reset.
pub fn failover_unavailable_notice(input: &FailoverNoticeInput<'_>) -> String {
    let FailoverNoticeInput {
        view,
        account,
        model,
        failure,
        tried,
        limited_accounts,
        admitted,
        credentialed,
        required_provider,
        now,
    } = input;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut order: Vec<&'static str> = Vec::new();
    let mut others = 0usize;
    for row in view.accounts.iter().filter(|row| &row.id != *account) {
        others += 1;
        let models: Vec<_> = view
            .models
            .iter()
            .filter(|choice| choice.provider == row.provider)
            .collect();
        let all_tried = !models.is_empty()
            && models
                .iter()
                .all(|choice| tried.contains(&format!("{}/{}", row.id, choice.key())));
        let reason = if !row.enabled {
            "disabled"
        } else if row.authentication_required || !credentialed.contains(&row.id) {
            "signed out"
        } else if !admitted.contains(&row.provider) {
            "on a provider build xcb has not checked"
        } else if required_provider.is_some_and(|provider| provider != row.provider) {
            "outside the pinned provider"
        } else if row.busy {
            "busy with another task"
        } else if row.quota_blocked_until_ms.is_some()
            || row
                .remaining_percent
                .is_some_and(|remaining| remaining <= 0.0)
            || limited_accounts.contains(&row.id)
        {
            "at a usage limit"
        } else if all_tried {
            "already tried on this task"
        } else if models.is_empty() {
            "without a recently seen model"
        } else {
            "not able to take the task now"
        };
        if !counts.contains_key(reason) {
            order.push(reason);
        }
        *counts.entry(reason).or_default() += 1;
    }
    let mut notice = usage_limit_label(view, account, model, *failure);
    if others == 0 {
        notice.push_str(" · no other account is signed in");
    } else {
        notice.push_str(" · no other account is able to take the task now");
        for reason in order {
            notice.push_str(&format!(" · {} {reason}", counts[reason]));
        }
    }
    let reset = view
        .accounts
        .iter()
        .filter_map(|row| row.quota_blocked_until_ms)
        .filter(|until| until > now)
        .min()
        .or_else(|| {
            view.accounts
                .iter()
                .find(|row| &row.id == *account)
                .and_then(|row| row.resets_at_ms)
                .filter(|until| until > now)
        });
    match reset {
        Some(until) => notice.push_str(&format!(
            " · earliest known reset in ~{}",
            wait_label(until, *now)
        )),
        None => notice.push_str(" · no reset time is known"),
    }
    notice
}

fn eligible_failover_routes(
    source: &RouteCandidate,
    candidates: &[RouteCandidate],
    tried: &BTreeSet<String>,
    outcome: &Outcome,
) -> Vec<usize> {
    let checkpointed = self::checkpointed(outcome);
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            next_route(
                source,
                std::slice::from_ref(*candidate),
                tried,
                &outcome.facts,
                checkpointed,
            )
            .is_some()
        })
        .map(|(index, _)| index)
        .take(16)
        .collect()
}

#[derive(Default)]
struct Activity {
    tools: Vec<String>,
    subagents: BTreeMap<Id, Subagent>,
    phase: Option<String>,
}
struct Active {
    cancel: watch::Sender<bool>,
    task: JoinHandle<()>,
    activity: Arc<Mutex<Activity>>,
    pane: bool,
}

/// Updates queued for the terminal. `updates` is a guaranteed FIFO — notices,
/// state snapshots, and stream resets must arrive — while `deltas` coalesces
/// streamed text per session and stream kind so a burst can never drop part of
/// a response. A bounded channel can slow delivery, never corrupt it: the next
/// published `View` is always a full snapshot of the settled transcript.
#[derive(Default)]
struct Outbox {
    updates: VecDeque<Update>,
    deltas: BTreeMap<(Id, bool), String>,
}

/// Bound on queued guaranteed updates; the channel itself holds 256, so this
/// only engages when the display has stopped draining entirely.
const MAX_QUEUED_UPDATES: usize = 1024;

fn queue(outbox: &Mutex<Outbox>, update: Update) {
    let Ok(mut outbox) = outbox.lock() else {
        return;
    };
    if outbox.updates.len() >= MAX_QUEUED_UPDATES {
        // Prefer dropping the oldest buffered snapshot: every View is a full
        // snapshot, so an older one carries no unique information.
        if let Some(stale) = outbox
            .updates
            .iter()
            .position(|update| matches!(update, Update::View(_)))
        {
            outbox.updates.remove(stale);
        } else {
            outbox.updates.pop_front();
        }
    }
    outbox.updates.push_back(update);
}

fn flush(outbox: &Mutex<Outbox>, output: &SyncSender<Update>) {
    let Ok(mut outbox) = outbox.lock() else {
        return;
    };
    while let Some(update) = outbox.updates.pop_front() {
        match output.try_send(update) {
            Ok(()) => (),
            Err(TrySendError::Full(update)) => {
                outbox.updates.push_front(update);
                return;
            }
            Err(TrySendError::Disconnected(_)) => {
                outbox.updates.clear();
                outbox.deltas.clear();
                return;
            }
        }
    }
    for ((session, thinking), text) in outbox.deltas.iter_mut() {
        if text.is_empty() {
            continue;
        }
        let delta = Update::Delta {
            session: session.clone(),
            thinking: *thinking,
            text: std::mem::take(text),
        };
        match output.try_send(delta) {
            Ok(()) => (),
            Err(TrySendError::Full(Update::Delta { text: kept, .. }))
            | Err(TrySendError::Disconnected(Update::Delta { text: kept, .. })) => {
                *text = kept;
            }
            Err(_) => (),
        }
    }
    outbox.deltas.retain(|_, text| !text.is_empty());
}

fn publish(
    store: &Store,
    current: Option<&Id>,
    config: &Config,
    active: &BTreeMap<Id, Active>,
    outbox: &Mutex<Outbox>,
) -> Result<()> {
    let now = now_ms();
    let mut view = summary::snapshot(store, current, config, now)?;
    if let Some(active) = current.and_then(|id| active.get(id)) {
        view.state = State::Working;
        if let Ok(activity) = active.activity.lock() {
            view.activity = activity.tools.clone();
            view.subagents = activity.subagents.values().cloned().collect();
        }
    } else if view.state == State::Working {
        // A session marked working without a task in this process may be owned
        // by a live sibling terminal; only an unowned run needs recovery.
        if let Some(id) = current {
            view.remote_active = store.remote_active(id)?;
        }
        if !view.remote_active {
            view.state = State::Uncertain;
        }
    }
    for row in &mut view.agents {
        let xcb_core::ui::TranscriptContext::Session(id) = &row.context else {
            continue;
        };
        if let Some(active) = active.get(id) {
            row.state = State::Working;
            row.activity = active
                .activity
                .lock()
                .ok()
                .and_then(|activity| activity.phase.clone())
                .unwrap_or_else(|| "working".into());
        } else if Some(id) == current {
            row.state = view.state;
            row.activity = view.state.label().into();
        }
    }
    crate::agent_overview::sort(&mut view.agents, now);
    if current.is_none() {
        view.pending_route = usable_account(store, None, None, config)
            .ok()
            .flatten()
            .and_then(|id| store.account(&id).ok())
            .and_then(|account| {
                choose_model(store, account.provider, None, config)
                    .ok()
                    .map(|model| RoutePreview {
                        account: account.name(),
                        provider: account.provider,
                        model: model.key(),
                    })
            });
    }
    queue(outbox, Update::View(Box::new(view)));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn start(
    store: Arc<Store>,
    id: Id,
    submission: Option<Id>,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    pane: bool,
    outbox: Arc<Mutex<Outbox>>,
    finished: mpsc::Sender<(Id, Result<Outcome>)>,
) -> Active {
    let (cancel, cancelled) = watch::channel(false);
    let activity = Arc::new(Mutex::new(Activity::default()));
    let activity_copy = activity.clone();
    let session_id = id.clone();
    let submission_outbox = outbox.clone();
    let observer: Observer = Arc::new(move |event| match event {
        Progress::Text { thinking, text } if !pane => {
            if let Ok(mut activity) = activity_copy.lock() {
                activity.phase = Some(
                    if thinking {
                        "thinking"
                    } else {
                        "writing response"
                    }
                    .into(),
                );
            }
            if let Ok(mut outbox) = outbox.lock() {
                let buffered = outbox
                    .deltas
                    .entry((session_id.clone(), thinking))
                    .or_default();
                let remaining = xcb_core::MAX_TEXT_BYTES.saturating_sub(buffered.len());
                buffered.push_str(&xcb_core::display_text(&text, remaining));
            }
        }
        Progress::Tool(name) => {
            if let Ok(mut activity) = activity_copy.lock() {
                activity.phase = Some(xcb_core::display_text(&format!("running tool {name}"), 320));
                if activity.tools.len() >= 128 {
                    activity.tools.remove(0);
                }
                activity.tools.push(name);
            }
        }
        Progress::Subagent(mut agent) => {
            if let Ok(mut activity) = activity_copy.lock()
                && (activity.subagents.len() < 64 || activity.subagents.contains_key(&agent.id))
            {
                if let Some(previous) = activity.subagents.get(&agent.id) {
                    if agent.label == "Subagent" {
                        agent.label.clone_from(&previous.label);
                    }
                    if agent.model.is_none() {
                        agent.model.clone_from(&previous.model);
                    }
                }
                activity.subagents.insert(agent.id.clone(), agent);
                activity.phase = Some("running subagent".into());
            }
        }
        Progress::Notice(message) => queue(&outbox, Update::Notice(message)),
        _ => (),
    });
    let submission = submission.map(|id| UiSubmission {
        id,
        outbox: submission_outbox,
        accepted: std::sync::atomic::AtomicBool::new(false),
    });
    let task = tokio::spawn(async move {
        let result = execute_mode(
            store,
            id.clone(),
            text.clone(),
            attachments.clone(),
            if pane {
                ExecutionMode::Pane
            } else {
                ExecutionMode::Direct
            },
            cancelled,
            observer,
            submission.as_ref(),
        )
        .await;
        if let Some(submission) = &submission
            && !submission.accepted()
            && let Err(error) = &result
        {
            queue(
                &submission.outbox,
                Update::SubmitRejected {
                    id: submission.id.clone(),
                    context: Some(xcb_core::ui::TranscriptContext::Session(id.clone())),
                    text,
                    attachments,
                    reason: error.to_string(),
                },
            );
        }
        let _ = finished.send((id, result)).await;
    });
    Active {
        cancel,
        task,
        activity,
        pane,
    }
}

/// mtime probe for `config.json`: the kernel's own publish cadence picks up
/// writes from sibling terminals, keeping it the single refresh path — no
/// TUI-side `Intent::Refresh` timer is needed.
fn config_modified(root: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(root.join("config.json"))
        .and_then(|meta| meta.modified())
        .ok()
}

fn reload_config(
    root: &Path,
    config: &mut Config,
    stamp: &mut Option<std::time::SystemTime>,
    outbox: &Mutex<Outbox>,
) {
    *stamp = config_modified(root);
    match Config::load(root) {
        Ok((fresh, _)) => *config = fresh,
        Err(error) => queue(
            outbox,
            Update::Notice(format!("Configuration reload rejected: {error}")),
        ),
    }
}

pub async fn serve(
    store: Arc<Store>,
    workspace: PathBuf,
    mut current: Option<Id>,
    input: Receiver<Intent>,
    output: SyncSender<Update>,
) -> Result<()> {
    let mut config = Config::load(store.root())?.0;
    let mut config_stamp = config_modified(store.root());
    let mut active: BTreeMap<Id, Active> = BTreeMap::new();
    let outbox = Arc::new(Mutex::new(Outbox::default()));
    let (completed, mut completions) = mpsc::channel::<(Id, Result<Outcome>)>(16);
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    let mut activity_published = tokio::time::Instant::now();
    let mut pending_pane: Option<(Id, String)> = None;
    let mut quit = false;
    publish(&store, current.as_ref(), &config, &active, &outbox)?;
    loop {
        flush(&outbox, &output);
        tokio::select! {
            done = completions.recv() => if let Some((id, result)) = done {
                let was_pane = active.remove(&id).is_some_and(|active| active.pane);
                if let Ok(mut queued) = outbox.lock() {
                    queued.deltas.retain(|(session, _), _| session != &id);
                }
                queue(&outbox, Update::ClearStream(id.clone()));
                match result {
                    Ok(outcome) if was_pane && outcome.facts.terminal == Terminal::Completed => {
                        let text = outcome.text.trim().strip_prefix("```json").or_else(|| outcome.text.trim().strip_prefix("```" )).unwrap_or(outcome.text.trim()).trim().trim_end_matches("```").trim();
                        match Pane::parse(text.as_bytes()) { Ok(pane) => queue(&outbox, Update::PaneCandidate(pane)), Err(error) => queue(&outbox, Update::Notice(format!("Generated pane rejected: {error}. The current pane is unchanged."))) }
                    }
                    Ok(outcome) if outcome.facts.terminal != Terminal::Completed => queue(&outbox, Update::Notice(format!("Turn stopped: {}", outcome.state.label()))),
                    Ok(outcome) if !was_pane && xcb_core::policy::no_reply(&outcome.text, &outcome.facts) => queue(&outbox, Update::Notice(NO_REPLY_NOTICE.into())),
                    Err(error) => queue(&outbox, Update::Notice(error.to_string())),
                    _ => (),
                }
                if !quit && let Some((generated, prompt)) = pending_pane_at_boundary(&store, &id, &mut pending_pane, &config, &outbox) {
                    let task = start(store.clone(), generated.clone(), None, prompt, vec![], true, outbox.clone(), completed.clone());
                    active.insert(generated, task);
                }
                publish(&store, current.as_ref(), &config, &active, &outbox)?;
            },
            _ = ticker.tick() => {
                for _ in 0..16 {
                    let intent = match input.try_recv() { Ok(intent) => intent, Err(TryRecvError::Empty) => break, Err(TryRecvError::Disconnected) => Intent::Quit };
                    if matches!(intent, Intent::Quit) { quit = true; pending_pane = None; for task in active.values() { let _ = task.cancel.send(true); } break; }
                    let submit_context = match &intent {
                        Intent::SubmitTo { context, .. } => Some(context.clone()),
                        _ => None,
                    };
                    let handled: Result<()> = (|| {
                        match intent {
                            Intent::Rename { context, expected_title, title } => {
                                let xcb_core::ui::TranscriptContext::Session(id) = context else {
                                    return Err(Error::Unavailable("choose a direct session to rename"));
                                };
                                store.rename_session(&id, &expected_title, &title)?;
                            }
                            Intent::TranscriptPage { context, before_sequence, request } => {
                                let result = match &context {
                                    xcb_core::ui::TranscriptContext::Session(id) if current.as_ref() == Some(id) => store.transcript_page(id, Some(before_sequence), 128),
                                    _ => Err(Error::Conflict("transcript context changed")),
                                };
                                match result {
                                    Ok(page) => queue(&outbox, Update::TranscriptPage { request, page }),
                                    Err(error) => queue(&outbox, Update::TranscriptPageRejected { context, request, reason: error.to_string() }),
                                }
                            }
                            Intent::Refresh => reload_config(store.root(), &mut config, &mut config_stamp, &outbox),
                            Intent::Submit { id: submission, text, attachments }
                            | Intent::SubmitTo { id: submission, text, attachments, .. } => {
                                let prepared: Result<Id> = (|| {
                                    if let Some(expected) = &submit_context
                                        && current.as_ref().map(|id| xcb_core::ui::TranscriptContext::Session(id.clone())).as_ref() != Some(expected) {
                                        return Err(Error::Conflict("session changed before submission"));
                                    }
                                    if current.is_none() { current = Some(new_session(&store, &workspace, &config, None, None, None)?.id); }
                                    let id = current.clone().expect("selected session");
                                    if active.contains_key(&id) || active.len() >= 16 { return Err(Error::Conflict("a turn is still running; your draft was restored to the composer")); }
                                    let session = store.session(&id)?.ok_or(Error::Unavailable("session not found"))?;
                                    ready(&store, &session)?;
                                    Ok(id)
                                })();
                                match prepared {
                                    Ok(id) => { active.insert(id.clone(), start(store.clone(), id, Some(submission), text, attachments, false, outbox.clone(), completed.clone())); }
                                    Err(error) => {
                                        queue(&outbox, Update::SubmitRejected {
                                            id: submission,
                                            context: submit_context.or_else(|| current.clone().map(xcb_core::ui::TranscriptContext::Session)),
                                            text, attachments, reason: error.to_string(),
                                        });
                                        return Err(error);
                                    }
                                }
                            }
                            Intent::Cancel => {
                                pending_pane = None;
                                if let Some(task) = current.as_ref().and_then(|id| active.get(id)) {
                                    let _ = task.cancel.send(true);
                                } else if let Some(id) = &current && store.remote_active(id)? {
                                    queue(&outbox, Update::Notice("This turn is running in another terminal; cancel it there.".into()));
                                }
                            }
                            Intent::Conversation(_) => return Err(Error::Unavailable("managed conversations are available from plain xcb chat")),
                            Intent::Habitat(_) | Intent::HabitatAt { .. } => return Err(Error::Unavailable("persistent backlog and schedules are available from plain xcb chat")),
                            Intent::Focus(_) | Intent::MoveTask { .. } | Intent::ReleaseHold { .. } | Intent::AddWorkspace { .. } | Intent::NewProjectView { .. } => return Err(Error::Unavailable("projects are available from plain xcb chat")),
                            Intent::Resume(id) => { if store.session(&id)?.is_none() { return Err(Error::Unavailable("session not found")); } current = Some(id); }
                            Intent::NewSession => current = Some(new_session(&store, &workspace, &config, None, None, None)?.id),
                            Intent::Account(account) => {
                                if current.as_ref().is_some_and(|id| active.contains_key(id)) { return Err(Error::Conflict("stop or finish the turn before changing accounts")); }
                                store.require_quota_available(&account, now_ms())?;
                                store.require_authenticated_account(&account)?;
                                let provider = store.account(&account)?.provider;
                                let model = choose_model(&store, provider, None, &config)?;
                                if let Some(id) = &current { let session = store.session(id)?.ok_or(Error::Unavailable("session not found"))?; store.rebind(id, session.revision, &account, model)?; }
                                else { current = Some(new_session(&store, &workspace, &config, Some(&account), None, None)?.id); }
                            }
                            Intent::Model(key) => {
                                let matches: Vec<_> = store.models()?.into_iter().filter(|model| model.key() == key || model.id.as_str() == key).collect();
                                if matches.len() != 1 { return Err(Error::Unavailable("choose one exact observed model and effort")); }
                                let model = matches[0].clone();
                                if current.as_ref().is_some_and(|id| active.contains_key(id)) { return Err(Error::Conflict("finish the turn before changing models")); }
                                let previous = current.as_ref().map(|id| store.session(id)).transpose()?.flatten();
                                let account = model_account(&store, model.provider, previous.as_ref().map(|session| &session.account), &config)?;
                                if let Some(id) = &current { let session = store.session(id)?.ok_or(Error::Unavailable("session not found"))?; store.rebind(id, session.revision, &account, model)?; }
                                else { current = Some(new_session(&store, &workspace, &config, Some(&account), Some(&model.key()), None)?.id); }
                            }
                            Intent::SetDefault => {
                                let session = current.as_ref().and_then(|id| store.session(id).ok().flatten()).ok_or(Error::Unavailable("select a session first"))?;
                                let (mut fresh, revision) = Config::load(store.root())?;
                                fresh.default_account = Some(session.account);
                                fresh.favorites.retain(|favorite| favorite.provider != session.model.provider || favorite.model != session.model.id || favorite.effort != session.model.effort);
                                fresh.favorites.insert(0, Preference { provider: session.model.provider, model: session.model.id, effort: session.model.effort });
                                fresh.save(store.root(), revision.as_deref())?; config = fresh;
                            }
                            Intent::Pane(id) => { panes::load(store.root(), &id)?; if let Some(session) = &current { store.select_pane(session, &id)?; } else { let (mut fresh, revision) = Config::load(store.root())?; fresh.pane = id; fresh.save(store.root(), revision.as_deref())?; config = fresh; } }
                            Intent::SavePane { pane, expected } => { panes::save(store.root(), &pane, expected.as_deref())?; if let Some(session) = &current { store.select_pane(session, &pane.id)?; } else { let (mut fresh, revision) = Config::load(store.root())?; fresh.pane = pane.id; fresh.save(store.root(), revision.as_deref())?; config = fresh; } }
                            Intent::GeneratePane(request) => {
                                let session = current.as_ref().and_then(|id| store.session(id).ok().flatten()).ok_or(Error::Unavailable("select an account and session before generating a pane"))?;
                                if active.values().any(|task| task.pane) || active.len() >= 16 { return Err(Error::Conflict("pane generation is already running")); }
                                if active.contains_key(&session.id) { pending_pane = Some((session.id, request)); queue(&outbox, Update::Notice("Pane generation queued for the account's next idle boundary. Editing and hot reload remain available.".into())); }
                                else { let generated = generation_session(&store, &session, &config)?; let task = start(store.clone(), generated.id.clone(), None, pane_prompt(&request)?, vec![], true, outbox.clone(), completed.clone()); active.insert(generated.id, task); }
                            }
                            Intent::AttachPath(path) => { let image = attachments::from_path(store.root(), Path::new(&path))?; queue(&outbox, Update::Attachment(image)); }
                            Intent::AttachRgba { width, height, bytes } => { let image = attachments::from_rgba(store.root(), width, height, bytes)?; queue(&outbox, Update::Attachment(image)); }
                            Intent::Extension { name, enabled } => {
                                let (mut fresh, revision) = Config::load(store.root())?;
                                match name.as_str() { "auto-continue" => fresh.extensions.auto_continue.enabled = enabled, "gobstopper" => fresh.extensions.gobstopper.enabled = enabled, "usage" => fresh.extensions.usage = enabled, "hooks" => fresh.extensions.hooks = enabled, "aicharts-export" => fresh.extensions.aicharts_export = enabled, "aicharts" | "aicharts-upload" => return Err(Error::Unavailable("automatic posting awaits a supported enrolled aicharts ingress; local exports remain available")), _ => return Err(Error::Unavailable("unknown built-in extension")) }
                                fresh.save(store.root(), revision.as_deref())?; config = fresh;
                            }
                            Intent::Quit => (),
                        }
                        Ok(())
                    })();
                    if let Err(error) = handled { queue(&outbox, Update::Notice(error.to_string())); }
                    publish(&store, current.as_ref(), &config, &active, &outbox)?;
                    activity_published = tokio::time::Instant::now();
                }
                // Durable state can change in another terminal even while this
                // one is idle. Full View updates retain the current session and
                // let the UI fingerprint suppress unchanged redraws; they do not
                // reset drafts, scroll positions, notices or open pickers.
                let refresh_after = if active.is_empty() { Duration::from_secs(1) } else { Duration::from_millis(250) };
                if activity_published.elapsed() >= refresh_after {
                    // A config written by a sibling terminal (xcb plugins, an
                    // edited file) lands on this same cadence.
                    if config_modified(store.root()) != config_stamp {
                        reload_config(store.root(), &mut config, &mut config_stamp, &outbox);
                    }
                    publish(&store, current.as_ref(), &config, &active, &outbox)?;
                    activity_published = tokio::time::Instant::now();
                }
            }
        }
        if quit && active.is_empty() {
            break;
        }
    }
    for (_, task) in active {
        let _ = task.cancel.send(true);
        let _ = task.task.await;
    }
    queue(&outbox, Update::Stopped);
    flush(&outbox, &output);
    Ok(())
}

/// An unrelated session finishing does not establish the queued account's idle
/// boundary. Preparation failures are notices, never errors that stop serve and
/// detach other active turns.
fn pending_pane_at_boundary(
    store: &Store,
    finished: &Id,
    pending: &mut Option<(Id, String)>,
    config: &Config,
    outbox: &Mutex<Outbox>,
) -> Option<(Id, String)> {
    if !pending
        .as_ref()
        .is_some_and(|(session, _)| session == finished)
    {
        return None;
    }
    let (id, request) = pending.take().expect("matching queued pane");
    let prepared: Result<(Id, String)> = (|| {
        let prompt = pane_prompt(&request)?;
        let source = store
            .session(&id)?
            .ok_or(Error::Unavailable("session not found"))?;
        let generated = generation_session(store, &source, config)?;
        Ok((generated.id, prompt))
    })();
    match prepared {
        Ok(prepared) => Some(prepared),
        Err(error) => {
            queue(
                outbox,
                Update::Notice(format!("Queued pane generation could not start: {error}")),
            );
            None
        }
    }
}

fn generation_session(store: &Store, source: &Session, config: &Config) -> Result<Session> {
    ready(store, source)?;
    new_session(
        store,
        Path::new(&source.workspace),
        config,
        Some(&source.account),
        Some(&source.model.key()),
        None,
    )
}
fn pane_prompt(request: &str) -> Result<String> {
    xcb_core::bounded_text(request, 4096)?;
    let preset = serde_json::to_string(&Pane::focus())?;
    Ok(format!(
        "Generate one xcb pane as JSON only. No markdown fences, code, commands, paths, or hooks. Schema: version 1, id (ASCII letters/digits/-/_), title, root. Nodes: column or row with 1..12 children; widget with source and optional lines (1..80); text with value; spacer with lines. Sources: last_user, responses, thinking, subagents, accounts, models, usage, activity, extensions. Maximum depth 8 and 96 nodes. Use a new id, never replace an existing preset. Example: {preset}\nUser's desired pane: {request}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn explicit_session_policy_preserves_aliases_and_rejects_conflicts_before_persistence() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = xcb_core::canonical(directory.path())
            .unwrap()
            .join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let store = Store::open(&workspace.parent().unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 2, None)
            .unwrap();
        auth::store_token(
            &store,
            &account.id,
            b"sk-ant-oat01-syntheticToken000000000000",
        )
        .unwrap();
        let model = route_candidate(0).1;
        store
            .set_models(Provider::Claude, std::slice::from_ref(&model))
            .unwrap();
        let config = Config::default();
        for policy in [
            SessionRoutePolicy {
                requirements: xcb_core::session::TaskRequirements {
                    signed_in_browser: true,
                },
                required_provider: None,
            },
            SessionRoutePolicy {
                requirements: Default::default(),
                required_provider: Some(Provider::Codex),
            },
        ] {
            assert!(
                new_session_with_policy(
                    &store,
                    &workspace,
                    &config,
                    Some(&account.id),
                    Some(model.id.as_str()),
                    None,
                    policy
                )
                .is_err()
            );
            assert!(store.sessions(16).unwrap().is_empty());
            assert!(store.unsettled_runs().unwrap().is_empty());
        }
        let key = model.key();
        for alias in [model.id.as_str(), model.label.as_str(), key.as_str()] {
            let session = new_session_with_policy(
                &store,
                &workspace,
                &config,
                Some(&account.id),
                Some(alias),
                None,
                SessionRoutePolicy {
                    requirements: Default::default(),
                    required_provider: Some(Provider::Claude),
                },
            )
            .unwrap();
            assert_eq!(session.model.key(), model.key());
        }
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn signed_in_browser_handoff_requires_joined_known_effects_and_never_routes_a_policy_denial() {
        let mut outcome = quota_outcome(Failure::Policy);
        outcome.state = State::Idle;
        outcome.facts.terminal = Terminal::Completed;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.failure = None;
        assert!(signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.terminal = Terminal::TurnLimit;
        assert!(signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.joined = false;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.joined = true;
        outcome.facts.effects = EffectState::Uncertain;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.effects = EffectState::None;
        assert!(signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.pending_attention = true;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.facts.pending_attention = false;
        outcome.state = State::NeedsAnswer;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, false));
        outcome.state = State::Idle;
        assert!(!signed_in_browser_handoff_permitted(&outcome, false, true));
    }

    #[test]
    fn ui_submission_acknowledges_only_the_exact_durable_input() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let session = store
            .create_session(&account.id, route_candidate(0).1, &workspace, 1)
            .unwrap();
        let previous = Message {
            id: new_id("m"),
            role: Role::User,
            text: "Repeat this prompt".into(),
            at_ms: 2,
            attachments: vec![],
            provenance: None,
        };
        let current = store
            .append_message(&session.id, session.revision, &previous)
            .unwrap();
        let outbox = Arc::new(Mutex::new(Outbox::default()));
        let submission = UiSubmission {
            id: new_id("m"),
            outbox: outbox.clone(),
            accepted: std::sync::atomic::AtomicBool::new(false),
        };
        let message = Message {
            id: submission.id.clone(),
            at_ms: 3,
            ..previous.clone()
        };
        assert!(append_input(&store, &session, &message, Some(&submission)).is_err());
        assert!(!submission.accepted());
        assert!(outbox.lock().unwrap().updates.is_empty());
        let current = append_input(&store, &current, &message, Some(&submission)).unwrap();
        assert!(submission.accepted());
        let update = outbox.lock().unwrap().updates.pop_front().unwrap();
        assert!(matches!(update, Update::Submitted { id, context }
            if id == submission.id && context == xcb_core::ui::TranscriptContext::Session(session.id.clone())));
        let reopened = Store::open(store.root()).unwrap();
        let persisted = reopened.messages(&session.id, 128).unwrap();
        assert_eq!(persisted.len(), 2);
        assert_eq!(persisted[1].id, submission.id);
        assert_eq!(persisted[0].text, persisted[1].text);
        let continuation = Message {
            id: new_id("m"),
            role: Role::System,
            text: "Continue".into(),
            at_ms: 4,
            attachments: vec![],
            provenance: None,
        };
        append_input(&store, &current, &continuation, Some(&submission)).unwrap();
        assert!(
            outbox.lock().unwrap().updates.is_empty(),
            "continuations cannot acknowledge another input"
        );
    }

    #[tokio::test]
    async fn ui_async_submission_failure_restores_exact_id_and_context() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let session = new_id("missing_session");
        let submission = new_id("input");
        let outbox = Arc::new(Mutex::new(Outbox::default()));
        let (finished, mut completion) = mpsc::channel(1);
        let active = start(
            store,
            session.clone(),
            Some(submission.clone()),
            "Retain async draft".into(),
            vec![],
            false,
            outbox.clone(),
            finished,
        );
        active.task.await.unwrap();
        assert!(completion.recv().await.unwrap().1.is_err());
        let mut queue = outbox.lock().unwrap();
        assert!(
            matches!(queue.updates.pop_front().unwrap(), Update::SubmitRejected {
            id, context: Some(xcb_core::ui::TranscriptContext::Session(context)), text, attachments, reason
        } if id == submission && context == session && text == "Retain async draft" && attachments.is_empty() && reason.contains("session not found"))
        );
        assert!(queue.updates.is_empty());
    }

    #[tokio::test]
    async fn ui_navigation_burst_rejects_stale_direct_submission_before_effects() {
        use xcb_core::ui::TranscriptContext;
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Arc::new(Store::open(&base.join("state")).unwrap());
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let first = store
            .create_session(&account.id, route_candidate(0).1, &workspace, 1)
            .unwrap();
        let second = store
            .create_session(&account.id, route_candidate(0).1, &workspace, 2)
            .unwrap();
        let submission = new_id("m");
        let image = xcb_core::session::Attachment {
            digest: "a".repeat(64),
            media_type: "image/png".into(),
            bytes: 512,
            width: 16,
            height: 16,
        };
        let (commands, input) = sync_channel(8);
        let (output, updates) = sync_channel(1);
        commands.send(Intent::Resume(second.id.clone())).unwrap();
        commands
            .send(Intent::SubmitTo {
                context: TranscriptContext::Session(first.id.clone()),
                id: submission.clone(),
                text: "Keep this in the original session".into(),
                attachments: vec![image.clone()],
            })
            .unwrap();
        let task = tokio::spawn(serve(
            store.clone(),
            workspace,
            Some(first.id.clone()),
            input,
            output,
        ));
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                while let Ok(update) = updates.try_recv() {
                    if let Update::SubmitRejected {
                        id,
                        context,
                        text,
                        attachments,
                        reason,
                    } = update
                    {
                        assert_eq!(id, submission);
                        assert_eq!(context, Some(TranscriptContext::Session(first.id.clone())));
                        assert_eq!(text, "Keep this in the original session");
                        assert_eq!(attachments, vec![image.clone()]);
                        assert!(reason.contains("session changed"));
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        commands.send(Intent::Quit).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for session in [first, second] {
            assert!(store.messages(&session.id, 128).unwrap().is_empty());
            assert_eq!(
                store.session(&session.id).unwrap().unwrap().revision,
                session.revision
            );
        }
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn workspace_lease_excludes_concurrent_and_unsettled_writers_across_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let first = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let second = store
            .add_account(Provider::Claude, "Test", 2, None)
            .unwrap();
        let model = route_candidate(0).1;
        let first_session = store
            .create_session(&first.id, model.clone(), &workspace, 1)
            .unwrap();
        let second_session = store
            .create_session(&second.id, model, &workspace, 1)
            .unwrap();
        let lease = workspace_lease(&store, &first_session).unwrap();
        assert!(matches!(
            workspace_lease(&store, &second_session),
            Err(Error::Conflict("workspace has an active writer"))
        ));
        drop(lease);
        drop(workspace_lease(&store, &second_session).unwrap());
        let run = store
            .prepare_run(&first_session.id, first_session.revision, 2)
            .unwrap();
        assert!(matches!(
            workspace_lease(&store, &second_session),
            Err(Error::Conflict("workspace has an unsettled writer"))
        ));
        store.settle(&run, State::Idle, 3).unwrap();
        drop(workspace_lease(&store, &second_session).unwrap());
    }

    #[tokio::test]
    async fn supervision_cancellation_waits_for_true_or_sender_loss() {
        let (sender, mut receiver) = watch::channel(false);
        sender.send(false).unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                cancellation_requested(&mut receiver)
            )
            .await
            .is_err()
        );
        sender.send(true).unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            cancellation_requested(&mut receiver),
        )
        .await
        .unwrap();
        let (sender, mut receiver) = watch::channel(false);
        drop(sender);
        tokio::time::timeout(
            Duration::from_secs(1),
            cancellation_requested(&mut receiver),
        )
        .await
        .unwrap();
    }

    #[test]
    fn failover_judge_sees_only_safe_routes_and_checkpoints() {
        use xcb_core::policy::{EffectState, Failure, TurnFacts};
        let candidate = |index| {
            let (account, model, _) = route_candidate(index);
            RouteCandidate {
                account,
                model,
                admitted: true,
                quota_clear: true,
                available: true,
            }
        };
        let source = candidate(0);
        let mut same_account = candidate(1);
        same_account.account = source.account.clone();
        let candidates = vec![same_account, candidate(2)];
        let tried = BTreeSet::new();
        let mut outcome = Outcome {
            tool_calls: Some(0),
            text_attention: false,
            diagnostic: None,
            text: "Saved the migration; remaining tests need to run".into(),
            state: State::Failed,
            facts: TurnFacts {
                terminal: Terminal::Failed,
                joined: true,
                effects: EffectState::Settled,
                pending_attention: false,
                failure: Some(Failure::AccountQuota),
            },
        };
        assert_eq!(
            eligible_failover_routes(&source, &candidates, &tried, &outcome),
            vec![1]
        );
        outcome.facts.failure = Some(Failure::ModelQuota);
        assert_eq!(
            eligible_failover_routes(&source, &candidates, &tried, &outcome),
            vec![0, 1]
        );
        outcome.text.clear();
        assert!(eligible_failover_routes(&source, &candidates, &tried, &outcome).is_empty());
        outcome.facts.effects = EffectState::None;
        assert_eq!(
            eligible_failover_routes(&source, &candidates, &tried, &outcome),
            vec![0, 1]
        );
        outcome.facts.pending_attention = true;
        assert!(eligible_failover_routes(&source, &candidates, &tried, &outcome).is_empty());
        outcome.facts.pending_attention = false;
        outcome.facts.terminal = Terminal::Completed;
        assert!(eligible_failover_routes(&source, &candidates, &tried, &outcome).is_empty());
    }

    fn account_row(index: usize, provider: Provider) -> AccountRow {
        AccountRow {
            id: Id::new(format!("a{index}")).unwrap(),
            provider,
            name: format!("{provider}/a{index}"),
            email: None,
            subscription: "Max".into(),
            remaining_percent: None,
            resets_at_ms: None,
            quota_blocked_until_ms: None,
            runway: xcb_core::usage::Estimate::unknown("quota_or_burn_unmeasured"),
            busy: false,
            enabled: true,
            authentication_required: false,
        }
    }

    fn quota_outcome(failure: Failure) -> Outcome {
        Outcome {
            tool_calls: Some(0),
            text_attention: false,
            diagnostic: None,
            text: "Saved the migration; remaining tests need to run".into(),
            state: State::Failed,
            facts: TurnFacts {
                terminal: Terminal::Failed,
                joined: true,
                effects: EffectState::Settled,
                pending_attention: false,
                failure: Some(failure),
            },
        }
    }

    /// The gate reads the same facts as automatic routing: an account without
    /// a meter (Devin) or with an aged-out reading has no known limit and is
    /// a target; a recorded block, a zero reading, a held account, a
    /// signed-out account or an unchecked provider build is not.
    #[test]
    fn failover_candidate_treats_unmeasured_accounts_as_clear() {
        let model = route_candidate(0).1;
        let devin = account_row(1, Provider::Devin);
        let candidate = failover_candidate(&devin, model.clone(), true);
        assert!(candidate.admitted && candidate.quota_clear && candidate.available);
        let stale = AccountRow {
            remaining_percent: None,
            resets_at_ms: None,
            ..account_row(2, Provider::Claude)
        };
        assert!(failover_candidate(&stale, model.clone(), true).quota_clear);
        let measured = AccountRow {
            remaining_percent: Some(12.5),
            ..account_row(3, Provider::Claude)
        };
        assert!(failover_candidate(&measured, model.clone(), true).quota_clear);
        let blocked = AccountRow {
            quota_blocked_until_ms: Some(u64::MAX),
            ..account_row(4, Provider::Claude)
        };
        assert!(!failover_candidate(&blocked, model.clone(), true).quota_clear);
        let exhausted = AccountRow {
            remaining_percent: Some(0.0),
            ..account_row(5, Provider::Claude)
        };
        assert!(!failover_candidate(&exhausted, model.clone(), true).quota_clear);
        let busy = AccountRow {
            busy: true,
            ..account_row(6, Provider::Claude)
        };
        assert!(!failover_candidate(&busy, model.clone(), true).available);
        let disabled = AccountRow {
            enabled: false,
            ..account_row(7, Provider::Claude)
        };
        assert!(!failover_candidate(&disabled, model.clone(), true).available);
        let signed_out = AccountRow {
            authentication_required: true,
            ..account_row(8, Provider::Claude)
        };
        assert!(!failover_candidate(&signed_out, model.clone(), true).admitted);
        assert!(!failover_candidate(&devin, model, false).admitted);
    }

    /// Continuation keeps its elapsed budget; failover does not share it. A
    /// settled usage limit after a long turn still moves, while an uncertain,
    /// unjoined or attention-pending turn never reaches the router.
    #[test]
    fn failover_ignores_the_continuation_budget_but_not_the_safety_gates() {
        let mut config = Config::default();
        config.extensions.auto_continue.max_elapsed_ms = 60_000;
        let limited = quota_outcome(Failure::AccountQuota);
        assert_eq!(
            supervision_step(&config, &limited, false, 3_600_000),
            Supervision::Failover(Failure::AccountQuota)
        );
        assert_eq!(
            supervision_step(&config, &quota_outcome(Failure::ModelQuota), false, 59_999),
            Supervision::Failover(Failure::ModelQuota)
        );
        let mut token_limit = quota_outcome(Failure::AccountQuota);
        token_limit.facts.terminal = Terminal::TokenLimit;
        token_limit.facts.failure = None;
        assert_eq!(
            supervision_step(&config, &token_limit, true, 59_999),
            Supervision::Continue
        );
        assert_eq!(
            supervision_step(&config, &token_limit, true, 60_000),
            Supervision::Stop,
            "the continuation budget still bounds continuation"
        );
        let mut transport = quota_outcome(Failure::Transport);
        transport.facts.failure = Some(Failure::Transport);
        assert_eq!(
            supervision_step(&config, &transport, false, 1),
            Supervision::Stop
        );
        config.auto_failover = false;
        assert_eq!(
            supervision_step(&config, &limited, false, 1),
            Supervision::Stop
        );
        // The gates before ranking: every refusal names its reason.
        let tried = BTreeSet::new();
        assert!(failover_blocked_reason(&limited.facts, &tried, true).is_none());
        let uncertain = TurnFacts {
            effects: EffectState::Uncertain,
            ..limited.facts.clone()
        };
        assert!(
            failover_blocked_reason(&uncertain, &tried, true)
                .unwrap()
                .contains("could not confirm")
        );
        let unjoined = TurnFacts {
            joined: false,
            ..limited.facts.clone()
        };
        assert!(
            failover_blocked_reason(&unjoined, &tried, true)
                .unwrap()
                .contains("could not confirm")
        );
        let attention = TurnFacts {
            pending_attention: true,
            ..limited.facts.clone()
        };
        assert!(
            failover_blocked_reason(&attention, &tried, true)
                .unwrap()
                .contains("waiting for an answer")
        );
        assert!(
            failover_blocked_reason(&limited.facts, &tried, false)
                .unwrap()
                .contains("without a reply")
        );
        let sixteen: BTreeSet<_> = (0..16).map(|n| format!("a{n}/claude/m")).collect();
        assert!(
            failover_blocked_reason(&limited.facts, &sixteen, true)
                .unwrap()
                .contains("16 routes")
        );
    }

    /// With no eligible route the terminal still learns which account hit the
    /// limit, why each other account was passed over, and the earliest reset.
    #[test]
    fn no_target_notice_names_the_limit_the_reasons_and_the_earliest_reset() {
        let now = 1_000_000_000;
        let model = route_candidate(0).1;
        let mut view = View {
            models: vec![model.clone(), route_candidate(1).1],
            accounts: vec![
                AccountRow {
                    remaining_percent: Some(0.0),
                    resets_at_ms: Some(now + 3 * 3_600_000),
                    quota_blocked_until_ms: Some(now + 3 * 3_600_000),
                    ..account_row(0, Provider::Claude)
                },
                AccountRow {
                    quota_blocked_until_ms: Some(now + 2 * 3_600_000 + 5 * 60_000),
                    ..account_row(1, Provider::Claude)
                },
                AccountRow {
                    authentication_required: true,
                    ..account_row(2, Provider::Codex)
                },
                account_row(3, Provider::Devin),
                AccountRow {
                    busy: true,
                    ..account_row(4, Provider::Claude)
                },
                account_row(5, Provider::Claude),
            ],
            ..View::default()
        };
        let tried: BTreeSet<_> = view
            .models
            .iter()
            .map(|choice| format!("a5/{}", choice.key()))
            .collect();
        let limited_accounts = BTreeSet::from([Id::new("a0").unwrap()]);
        let admitted = BTreeSet::from([Provider::Claude, Provider::Codex]);
        let credentialed: BTreeSet<_> = view
            .accounts
            .iter()
            .filter(|row| !row.authentication_required)
            .map(|row| row.id.clone())
            .collect();
        let notice = failover_unavailable_notice(&FailoverNoticeInput {
            view: &view,
            account: &Id::new("a0").unwrap(),
            model: &model,
            failure: Failure::AccountQuota,
            tried: &tried,
            limited_accounts: &limited_accounts,
            admitted: &admitted,
            credentialed: &credentialed,
            required_provider: None,
            now,
        });
        assert_eq!(
            notice,
            "Usage limit on claude · claude/a0 · no other account is able to take the task now · 1 at a usage limit · 1 signed out · 1 on a provider build xcb has not checked · 1 busy with another task · 1 already tried on this task · earliest known reset in ~2h 5m"
        );
        for internal in ["lease", "custody", "eligible", "admitted", "credential"] {
            assert!(!notice.contains(internal), "{notice}");
        }
        // A model-specific limit names the model; a pinned provider explains
        // the accounts outside it; a reset falls back to the account's own
        // window; a single account says so.
        let pinned = failover_unavailable_notice(&FailoverNoticeInput {
            view: &view,
            account: &Id::new("a0").unwrap(),
            model: &model,
            failure: Failure::ModelQuota,
            tried: &BTreeSet::new(),
            limited_accounts: &BTreeSet::new(),
            admitted: &Provider::ALL.into_iter().collect(),
            credentialed: &credentialed,
            required_provider: Some(Provider::Devin),
            now,
        });
        assert!(pinned.starts_with("Usage limit for Model 0 on claude · claude/a0 · "));
        assert!(
            pinned.contains(" · 3 outside the pinned provider"),
            "{pinned}"
        );
        assert!(
            pinned.contains(" · 1 without a recently seen model"),
            "{pinned}"
        );
        view.accounts.truncate(1);
        view.accounts[0].quota_blocked_until_ms = None;
        let alone = failover_unavailable_notice(&FailoverNoticeInput {
            view: &view,
            account: &Id::new("a0").unwrap(),
            model: &model,
            failure: Failure::AccountQuota,
            tried: &BTreeSet::new(),
            limited_accounts: &BTreeSet::new(),
            admitted: &admitted,
            credentialed: &credentialed,
            required_provider: None,
            now,
        });
        assert_eq!(
            alone,
            "Usage limit on claude · claude/a0 · no other account is signed in · earliest known reset in ~3h 0m"
        );
    }

    #[test]
    fn quota_availability_preserves_affinity_and_never_reselects_blocked_onboarding_default() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("work")).unwrap();
        let blocked = store
            .add_account(Provider::Claude, "Test", 1, None)
            .unwrap();
        let fallback = store
            .add_account(Provider::Claude, "Test", 2, None)
            .unwrap();
        for account in [&blocked, &fallback] {
            auth::store_token(
                &store,
                &account.id,
                b"sk-ant-oat01-syntheticToken000000000000",
            )
            .unwrap();
        }
        let config = Config {
            default_account: Some(blocked.id.clone()),
            ..Config::default()
        };
        let model = route_candidate(0).1;
        store
            .set_models(Provider::Claude, std::slice::from_ref(&model))
            .unwrap();
        assert_eq!(
            model_account(&store, Provider::Claude, Some(&fallback.id), &config).unwrap(),
            fallback.id
        );
        let original = new_session(
            &store,
            &workspace,
            &config,
            Some(&blocked.id),
            Some(&model.key()),
            None,
        )
        .unwrap();
        let now = now_ms();
        let run = store
            .prepare_probe(&blocked.id, None, now - 400_000)
            .unwrap();
        store
            .record_account_quota(
                &run,
                &xcb_core::usage::QuotaPoint {
                    pool: blocked.quota_pool.clone(),
                    window: Id::new("seven_day").unwrap(),
                    used_percent: 100.0,
                    observed_at_ms: now - 400_000,
                    resets_at_ms: now + 9_000_000,
                },
            )
            .unwrap();
        store.settle(&run, State::Idle, now).unwrap();
        assert_eq!(
            model_account(&store, Provider::Claude, Some(&blocked.id), &config).unwrap(),
            fallback.id
        );
        assert_eq!(
            new_session(&store, &workspace, &config, None, Some(&model.key()), None)
                .unwrap()
                .account,
            fallback.id
        );
        assert!(
            new_session(
                &store,
                &workspace,
                &config,
                Some(&blocked.id),
                Some(&model.key()),
                None
            )
            .unwrap_err()
            .to_string()
            .contains("quota exhausted")
        );
        assert!(
            ready(&store, &original)
                .unwrap_err()
                .to_string()
                .contains("quota exhausted")
        );
        assert_eq!(
            store.session(&original.id).unwrap().unwrap().account,
            blocked.id
        );
        store.set_account_enabled(&fallback.id, false).unwrap();
        assert!(
            new_session(&store, &workspace, &config, None, Some(&model.key()), None)
                .unwrap_err()
                .to_string()
                .contains("reported quota reset")
        );
        assert!(store.unsettled_runs().unwrap().is_empty());
    }

    #[test]
    fn credential_hints_follow_each_providers_connection_method() {
        assert!(credential_guidance(Provider::Claude).contains("accounts login"));
        assert!(credential_guidance(Provider::Codex).contains("accounts import-codex"));
        let devin = credential_guidance(Provider::Devin);
        assert!(devin.contains("accounts token"));
        assert!(devin.contains("accounts import-devin"));
        assert!(!devin.contains("accounts login"));
    }

    #[test]
    fn queued_pane_waits_for_its_session_and_reports_start_failure_without_stopping_views() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("synthetic-test").unwrap(),
            label: "Synthetic".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let first = store.add_account(Provider::Devin, "Test", 1, None).unwrap();
        let second = store.add_account(Provider::Devin, "Test", 1, None).unwrap();
        let a = store
            .create_session(&first.id, model.clone(), &workspace, 2)
            .unwrap();
        let b = store
            .create_session(&second.id, model, &workspace, 2)
            .unwrap();
        let a_run = store.prepare_run(&a.id, a.revision, 3).unwrap();
        let b_run = store.prepare_run(&b.id, b.revision, 3).unwrap();
        let config = Config::default();
        let outbox = Mutex::new(Outbox::default());
        let mut pending = Some((a.id.clone(), "compact view".into()));

        store.settle(&b_run, State::Idle, 4).unwrap();
        assert!(pending_pane_at_boundary(&store, &b.id, &mut pending, &config, &outbox).is_none());
        assert_eq!(pending.as_ref().map(|(id, _)| id), Some(&a.id));
        assert!(outbox.lock().unwrap().updates.is_empty());
        assert_eq!(store.sessions(64).unwrap().len(), 2);
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);

        store.settle(&a_run, State::Idle, 5).unwrap();
        store.set_account_enabled(&first.id, false).unwrap();
        assert!(pending_pane_at_boundary(&store, &a.id, &mut pending, &config, &outbox).is_none());
        assert!(pending.is_none());
        assert!(outbox.lock().unwrap().updates.iter().any(|update| matches!(update, Update::Notice(text) if text.contains("Queued pane generation could not start") && text.contains("disabled"))));
        assert_eq!(store.sessions(64).unwrap().len(), 2);
        publish(&store, Some(&a.id), &config, &BTreeMap::new(), &outbox).unwrap();
        assert!(matches!(
            outbox.lock().unwrap().updates.back(),
            Some(Update::View(_))
        ));
    }

    #[test]
    fn publish_previews_the_pending_route_until_a_session_is_bound() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let account = store
            .add_account(Provider::Devin, "Subscription", 1, None)
            .unwrap();
        crate::devin::auth::store_token(&store, &account.id, b"synthetic-token").unwrap();
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("swe-2-high").unwrap(),
            label: "SWE-2 High".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        store
            .set_models(Provider::Devin, std::slice::from_ref(&model))
            .unwrap();
        let config = Config::default();
        let outbox = Mutex::new(Outbox::default());

        publish(&store, None, &config, &BTreeMap::new(), &outbox).unwrap();
        let view = match outbox.lock().unwrap().updates.pop_front() {
            Some(Update::View(view)) => *view,
            _ => panic!("publish emits a full view"),
        };
        let route = view.pending_route.expect("pending route preview");
        assert_eq!(route.provider, Provider::Devin);
        assert_eq!(route.account, account.name());
        assert_eq!(route.model, "devin/swe-2-high");

        // A bound session replaces the preview with the committed route.
        let session = store
            .create_session(&account.id, model, &workspace, 2)
            .unwrap();
        publish(
            &store,
            Some(&session.id),
            &config,
            &BTreeMap::new(),
            &outbox,
        )
        .unwrap();
        let view = match outbox.lock().unwrap().updates.pop_front() {
            Some(Update::View(view)) => *view,
            _ => panic!("publish emits a full view"),
        };
        assert!(view.pending_route.is_none());
        assert!(view.session.is_some());
    }

    #[tokio::test]
    async fn disabled_resumed_accounts_restore_drafts_before_any_turn_for_every_provider() {
        for provider in Provider::ALL {
            let directory = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(directory.path()).unwrap();
            let store = Arc::new(Store::open(&base.join("state")).unwrap());
            let workspace = crate::private::directory(&base.join("workspace")).unwrap();
            let account = store.add_account(provider, "Test", 1, None).unwrap();
            let session = store
                .create_session(
                    &account.id,
                    ModelChoice {
                        provider,
                        id: Id::new("synthetic-model").unwrap(),
                        label: "Synthetic".into(),
                        mode: xcb_core::models::Mode::Fixed,
                        resolved: None,
                        effort: None,
                        observed_at_ms: 1,
                    },
                    &workspace,
                    2,
                )
                .unwrap();
            let (commands, input) = sync_channel(8);
            let (output, updates) = sync_channel(32);
            let task = tokio::spawn(serve(
                store.clone(),
                workspace,
                Some(session.id.clone()),
                input,
                output,
            ));
            view_matching(&updates, |view| {
                view.session
                    .as_ref()
                    .is_some_and(|current| current.id == session.id)
            })
            .await;
            let other = Store::open(store.root()).unwrap();
            other.set_account_enabled(&account.id, false).unwrap();
            let image = xcb_core::session::Attachment {
                digest: "a".repeat(64),
                media_type: "image/png".into(),
                bytes: 512,
                width: 16,
                height: 16,
            };
            commands
                .send(Intent::Submit {
                    id: Id::new("m_retained").unwrap(),
                    text: "retained task".into(),
                    attachments: vec![image.clone()],
                })
                .unwrap();
            tokio::time::timeout(Duration::from_secs(4), async {
                let mut draft = false;
                let mut notice = false;
                loop {
                    while let Ok(update) = updates.try_recv() {
                        match update {
                            Update::SubmitRejected {
                                id,
                                context,
                                text,
                                attachments,
                                reason,
                            } => {
                                assert_eq!(id.as_str(), "m_retained");
                                assert_eq!(
                                    context,
                                    Some(xcb_core::ui::TranscriptContext::Session(
                                        session.id.clone()
                                    ))
                                );
                                assert!(reason.contains("selected account is disabled"));
                                assert_eq!(text, "retained task");
                                assert_eq!(attachments, vec![image.clone()]);
                                draft = true;
                            }
                            Update::Notice(text)
                                if text.contains("selected account is disabled") =>
                            {
                                notice = true
                            }
                            Update::Stopped => panic!("disabled account stopped the terminal"),
                            _ => (),
                        }
                    }
                    if draft && notice {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("disabled account must return its draft and diagnosis");
            assert!(!task.is_finished());
            assert!(store.messages(&session.id, 128).unwrap().is_empty());
            assert!(store.unsettled_runs().unwrap().is_empty());
            commands.send(Intent::Quit).unwrap();
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
    }

    #[test]
    fn model_account_prefers_usable_current_then_default_and_preserves_busy_custody() {
        let directory = tempfile::tempdir().unwrap();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Store::open(&state).unwrap();
        let current = store
            .add_account(Provider::Devin, "Subscription", 1, None)
            .unwrap();
        let default = store
            .add_account(Provider::Devin, "Subscription", 2, None)
            .unwrap();
        let fallback = store
            .add_account(Provider::Devin, "Subscription", 3, None)
            .unwrap();
        let unsigned = store
            .add_account(Provider::Devin, "Subscription", 4, None)
            .unwrap();
        let disabled = store
            .add_account(Provider::Devin, "Subscription", 5, None)
            .unwrap();
        let other = store
            .add_account(Provider::Claude, "Subscription", 6, None)
            .unwrap();
        for account in [&current, &default, &fallback, &disabled] {
            crate::devin::auth::store_token(&store, &account.id, b"synthetic-token").unwrap();
        }
        store.set_account_enabled(&disabled.id, false).unwrap();
        let mut config = Config {
            default_account: Some(default.id.clone()),
            ..Config::default()
        };
        assert_eq!(
            model_account(&store, Provider::Devin, Some(&current.id), &config).unwrap(),
            current.id
        );
        assert_eq!(
            model_account(&store, Provider::Devin, Some(&other.id), &config).unwrap(),
            default.id
        );
        assert_eq!(
            model_account(&store, Provider::Devin, Some(&unsigned.id), &config).unwrap(),
            default.id
        );
        let current_run = store.prepare_probe(&current.id, None, 7).unwrap();
        assert_eq!(
            model_account(&store, Provider::Devin, Some(&current.id), &config).unwrap(),
            default.id
        );
        let default_run = store.prepare_probe(&default.id, None, 8).unwrap();
        assert_eq!(
            model_account(&store, Provider::Devin, Some(&current.id), &config).unwrap(),
            fallback.id
        );
        config.default_account = Some(unsigned.id);
        assert_eq!(
            model_account(&store, Provider::Devin, None, &config).unwrap(),
            fallback.id
        );
        store.set_account_enabled(&fallback.id, false).unwrap();
        assert!(model_account(&store, Provider::Devin, Some(&current.id), &config).is_err());
        assert_eq!(store.unsettled_runs().unwrap().len(), 2);
        store.settle(&current_run, State::Idle, 9).unwrap();
        store.settle(&default_run, State::Idle, 10).unwrap();
    }

    #[test]
    fn usable_account_prefers_the_most_remaining_quota() {
        let directory = tempfile::tempdir().unwrap();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Store::open(&state).unwrap();
        let spent = store
            .add_account(Provider::Claude, "Subscription", 1, None)
            .unwrap();
        let frugal = store
            .add_account(Provider::Claude, "Subscription", 2, None)
            .unwrap();
        let unmeasured = store
            .add_account(Provider::Claude, "Subscription", 3, None)
            .unwrap();
        for account in [&spent, &frugal, &unmeasured] {
            auth::store_token(
                &store,
                &account.id,
                b"sk-ant-oat01-syntheticToken000000000000",
            )
            .unwrap();
        }
        let now = now_ms();
        for (account, used) in [(&spent, 80.0), (&frugal, 20.0)] {
            store
                .record_quota(&xcb_core::usage::QuotaPoint {
                    pool: account.quota_pool.clone(),
                    window: Id::new("five_hour").unwrap(),
                    used_percent: used,
                    observed_at_ms: now,
                    resets_at_ms: now + 9_000_000,
                })
                .unwrap();
        }
        let config = Config {
            default_account: Some(spent.id.clone()),
            ..Config::default()
        };
        // The account with the most measured headroom wins, even over the
        // configured default and never-measured accounts.
        assert_eq!(
            usable_account(&store, Some(Provider::Claude), None, &config).unwrap(),
            Some(frugal.id.clone())
        );
        // A bound session's account stays sticky across a model switch.
        assert_eq!(
            usable_account(&store, Some(Provider::Claude), Some(&spent.id), &config).unwrap(),
            Some(spent.id.clone())
        );
        // A fresh 100% observation drops the leader behind the next measured
        // account; unmeasured accounts still cannot claim availability.
        store
            .record_quota(&xcb_core::usage::QuotaPoint {
                pool: frugal.quota_pool.clone(),
                window: Id::new("five_hour").unwrap(),
                used_percent: 100.0,
                observed_at_ms: now + 1,
                resets_at_ms: now + 9_000_000,
            })
            .unwrap();
        assert_eq!(
            usable_account(&store, Some(Provider::Claude), None, &config).unwrap(),
            Some(spent.id.clone())
        );
    }

    #[test]
    fn choose_model_defaults_to_the_high_effort_non_premium_route() {
        let directory = tempfile::tempdir().unwrap();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Store::open(&state).unwrap();
        let fixed = |provider: Provider, model: &str, effort: Option<&str>| ModelChoice {
            provider,
            id: Id::new(model).unwrap(),
            label: model.into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: effort.map(|value| Id::new(value).unwrap()),
            observed_at_ms: 1,
        };
        store
            .set_models(
                Provider::Claude,
                &[
                    fixed(Provider::Claude, "claude-fable-5-1", Some("max")),
                    fixed(Provider::Claude, "default", Some("high")),
                    fixed(Provider::Claude, "sonnet", Some("max")),
                ],
            )
            .unwrap();
        store
            .set_models(
                Provider::Codex,
                &[
                    fixed(Provider::Codex, "gpt-6-astra", Some("ultra")),
                    fixed(Provider::Codex, "gpt-6-astra", Some("high")),
                ],
            )
            .unwrap();
        store
            .set_models(
                Provider::Devin,
                &[
                    fixed(Provider::Devin, "gpt-6-astra-max", None),
                    fixed(Provider::Devin, "swe-2-high", None),
                ],
            )
            .unwrap();
        let config = Config::default();
        assert_eq!(
            choose_model(&store, Provider::Claude, None, &config)
                .unwrap()
                .key(),
            "claude/default/high"
        );
        assert_eq!(
            choose_model(&store, Provider::Codex, None, &config)
                .unwrap()
                .key(),
            "codex/gpt-6-astra/high"
        );
        assert_eq!(
            choose_model(&store, Provider::Devin, None, &config)
                .unwrap()
                .key(),
            "devin/gpt-6-astra-max"
        );
        // A pinned model that routing.never excludes is refused, not widened.
        let refused = choose_model(&store, Provider::Devin, Some("devin/swe-2-high"), &config)
            .unwrap_err()
            .to_string();
        assert!(refused.contains("routing.never"), "{refused}");
        let mut allowed = Config::default();
        allowed.routing.never.clear();
        assert_eq!(
            choose_model(&store, Provider::Devin, Some("devin/swe-2-high"), &allowed)
                .unwrap()
                .key(),
            "devin/swe-2-high"
        );
    }

    #[test]
    fn new_session_prefers_usable_matching_accounts_before_onboarding_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let base = xcb_core::canonical(directory.path()).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let unsigned = store
            .add_account(Provider::Devin, "Subscription", 1, None)
            .unwrap();
        let connected = store
            .add_account(Provider::Devin, "Subscription", 2, None)
            .unwrap();
        let claude = store
            .add_account(Provider::Claude, "Subscription", 3, None)
            .unwrap();
        let config = Config {
            default_account: Some(unsigned.id.clone()),
            ..Config::default()
        };
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("synthetic-test").unwrap(),
            label: "Synthetic".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        store
            .set_models(Provider::Devin, std::slice::from_ref(&model))
            .unwrap();
        crate::devin::auth::store_token(&store, &connected.id, b"synthetic-token").unwrap();
        let chosen =
            new_session(&store, &workspace, &config, None, Some(&model.key()), None).unwrap();
        assert_eq!(chosen.account, connected.id);
        let other_default = Config {
            default_account: Some(claude.id),
            ..config.clone()
        };
        assert_eq!(
            new_session(
                &store,
                &workspace,
                &other_default,
                None,
                Some(&model.key()),
                None
            )
            .unwrap()
            .account,
            connected.id
        );
        let run = store.prepare_probe(&connected.id, None, 4).unwrap();
        assert_eq!(
            new_session(&store, &workspace, &config, None, Some(&model.key()), None)
                .unwrap()
                .account,
            unsigned.id
        );
        assert_eq!(store.unsettled_runs().unwrap().len(), 1);
        store.settle(&run, State::Idle, 5).unwrap();
        crate::devin::auth::store_token(&store, &unsigned.id, b"synthetic-token").unwrap();
        assert_eq!(
            new_session(&store, &workspace, &config, None, Some(&model.key()), None)
                .unwrap()
                .account,
            unsigned.id
        );
    }

    async fn view_matching(
        updates: &Receiver<Update>,
        expected: impl Fn(&xcb_core::ui::View) -> bool,
    ) -> xcb_core::ui::View {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                while let Ok(update) = updates.try_recv() {
                    match update {
                        Update::View(view) if expected(&view) => return *view,
                        Update::View(_) => (),
                        _ => panic!("idle polling must publish only full views"),
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("idle terminal did not observe durable state")
    }

    #[tokio::test]
    async fn idle_terminal_observes_other_terminal_state_without_changing_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Arc::new(Store::open(&state).unwrap());
        let other = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Devin, "Subscription", 1, None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("synthetic-test").unwrap(),
            label: "Synthetic".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let workspace = crate::private::directory(
            &xcb_core::canonical(directory.path())
                .unwrap()
                .join("workspace"),
        )
        .unwrap();
        let session = store
            .create_session(&account.id, model.clone(), &workspace, 2)
            .unwrap();
        let (commands, input) = sync_channel(8);
        let (output, updates) = sync_channel(32);
        let task = tokio::spawn(serve(
            store.clone(),
            workspace,
            Some(session.id.clone()),
            input,
            output,
        ));
        let first = view_matching(&updates, |view| {
            view.session.as_ref().is_some_and(|s| s.id == session.id)
        })
        .await;
        assert_eq!(first.state, State::Idle);
        let run = other.prepare_run(&session.id, session.revision, 3).unwrap();
        let busy = view_matching(&updates, |view| view.remote_active).await;
        assert_eq!(busy.session.unwrap().id, session.id);
        assert!(
            busy.accounts
                .iter()
                .any(|row| row.id == account.id && row.busy)
        );
        other.settle(&run, State::Idle, 4).unwrap();
        let imported = other
            .add_account(Provider::Codex, "Subscription", 5, None)
            .unwrap();
        other
            .set_models(Provider::Devin, std::slice::from_ref(&model))
            .unwrap();
        let settled = view_matching(&updates, |view| {
            !view.remote_active
                && view.state == State::Idle
                && view.accounts.iter().any(|row| row.id == imported.id)
                && view.models.iter().any(|choice| choice.key() == model.key())
        })
        .await;
        assert_eq!(settled.session.unwrap().id, session.id);
        assert!(!settled.accounts.iter().any(|row| row.busy));
        commands.send(Intent::Quit).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// Saturating the bounded UI channel must never lose data: guaranteed
    /// updates arrive in order and coalesced stream text reassembles whole.
    #[test]
    fn a_saturated_channel_still_converges_on_the_full_update() {
        let (tx, rx) = sync_channel(4);
        let outbox = Mutex::new(Outbox::default());
        let session = new_id("s");
        for index in 0..10 {
            queue(&outbox, Update::Notice(format!("notice {index}")));
        }
        {
            let mut queued = outbox.lock().unwrap();
            let buffered = queued.deltas.entry((session.clone(), false)).or_default();
            buffered.push_str("first ");
            buffered.push_str("second");
        }

        let mut notices = Vec::new();
        let mut streamed = String::new();
        for _ in 0..128 {
            flush(&outbox, &tx);
            while let Ok(update) = rx.try_recv() {
                match update {
                    Update::Notice(text) => notices.push(text),
                    Update::Delta { text, .. } => streamed.push_str(&text),
                    _ => (),
                }
            }
            {
                let queued = outbox.lock().unwrap();
                if queued.updates.is_empty() && queued.deltas.is_empty() {
                    break;
                }
            }
        }
        assert!(rx.try_recv().is_err());
        assert_eq!(
            notices,
            (0..10)
                .map(|index| format!("notice {index}"))
                .collect::<Vec<_>>(),
            "guaranteed updates arrive complete and in order"
        );
        assert_eq!(streamed, "first second");
    }

    /// Once the display is gone the queue must not grow without bound.
    #[test]
    fn a_disconnected_display_stops_queued_work() {
        let (tx, rx) = sync_channel(1);
        let outbox = Mutex::new(Outbox::default());
        queue(&outbox, Update::Notice("one".into()));
        flush(&outbox, &tx);
        queue(&outbox, Update::Notice("two".into()));
        drop(rx);
        flush(&outbox, &tx);
        let queued = outbox.lock().unwrap();
        assert!(queued.updates.is_empty());
    }

    struct PickJudge(String);
    impl judge::Judge for PickJudge {
        fn ask<'a>(
            &'a self,
            _state: &'a serde_json::Value,
            questions: &'a judge::JudgeQuestions,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<judge::JudgeAnswers>> + Send + 'a>,
        > {
            let pick = self.0.clone();
            let checked = questions.contains_key("route")
                && questions.iter().all(|(_, question)| match question {
                    judge::JudgeQuestion::Choice { criteria, .. } => {
                        criteria.values().all(|description| {
                            description
                                .as_ref()
                                .is_some_and(|d| d.contains("% quota remaining"))
                        })
                    }
                    _ => false,
                });
            Box::pin(async move {
                if !checked {
                    return Err(Error::Unavailable("missing route question"));
                }
                let mut answers = std::collections::BTreeMap::new();
                answers.insert(
                    "route".to_owned(),
                    judge::JudgeAnswer::Choice {
                        choice: pick,
                        confidence: 0.9,
                        probabilities: std::collections::BTreeMap::new(),
                    },
                );
                Ok(judge::JudgeAnswers {
                    answers,
                    model: None,
                })
            })
        }
    }

    struct ContinueJudge(f64);
    impl judge::Judge for ContinueJudge {
        fn ask<'a>(
            &'a self,
            state: &'a serde_json::Value,
            questions: &'a judge::JudgeQuestions,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<judge::JudgeAnswers>> + Send + 'a>,
        > {
            let probability = self.0;
            let checked = judge::check_state(state).is_ok()
                && judge::check_questions(questions).is_ok()
                && questions.contains_key("continue_task");
            Box::pin(async move {
                if !checked {
                    return Err(Error::Unavailable("invalid continuation question"));
                }
                let mut answers = std::collections::BTreeMap::new();
                answers.insert(
                    "continue_task".to_owned(),
                    judge::JudgeAnswer::Noul(probability),
                );
                Ok(judge::JudgeAnswers {
                    answers,
                    model: None,
                })
            })
        }
    }

    fn route_candidate(index: usize) -> (Id, ModelChoice, Option<f64>) {
        (
            Id::new(format!("a{index}")).unwrap(),
            ModelChoice {
                provider: Provider::Claude,
                id: Id::new(format!("model-{index}")).unwrap(),
                label: format!("Model {index}"),
                mode: xcb_core::models::Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: 1,
            },
            Some(42.0),
        )
    }

    #[tokio::test]
    async fn pick_route_maps_the_choice_back_to_a_candidate() {
        let candidates = vec![route_candidate(0), route_candidate(1), route_candidate(2)];
        let (account, model) =
            pick_route(&PickJudge("route_1".to_owned()), "ctx", "task", candidates)
                .await
                .unwrap();
        assert_eq!(account.as_str(), "a1");
        assert_eq!(model.id.as_str(), "model-1");
    }

    #[tokio::test]
    async fn pick_route_skips_the_call_for_a_single_candidate() {
        let (account, _) = pick_route(
            &PickJudge("route_9".to_owned()),
            "ctx",
            "task",
            vec![route_candidate(0)],
        )
        .await
        .unwrap();
        assert_eq!(account.as_str(), "a0");
        assert!(
            pick_route(&PickJudge("route_0".into()), "ctx", "task", vec![])
                .await
                .is_err()
        );
        assert!(
            pick_route(
                &PickJudge("route_7".into()),
                "ctx",
                "task",
                vec![route_candidate(0), route_candidate(1)],
            )
            .await
            .is_err(),
            "out-of-range judge answers are rejected"
        );
    }

    #[tokio::test]
    async fn continuation_judgment_is_bounded_and_cannot_bypass_safety() {
        let policy = xcb_core::policy::AutoContinue::default();
        let facts = xcb_core::policy::TurnFacts {
            terminal: Terminal::TokenLimit,
            joined: true,
            effects: xcb_core::policy::EffectState::Settled,
            pending_attention: false,
            failure: None,
        };
        assert!(
            judge_continuation(
                &ContinueJudge(JUDGE_CONTINUE_THRESHOLD),
                ContinuationInput {
                    policy: &policy,
                    original_task: &"task".repeat(100_000),
                    last_response: &"response".repeat(100_000),
                    facts: &facts,
                    consecutive: 0,
                    elapsed_ms: 1_000,
                    repeated: false,
                },
            )
            .await
            .unwrap()
        );
        assert!(
            !judge_continuation(
                &ContinueJudge(JUDGE_CONTINUE_THRESHOLD - 0.01),
                ContinuationInput {
                    policy: &policy,
                    original_task: "task",
                    last_response: "response",
                    facts: &facts,
                    consecutive: 0,
                    elapsed_ms: 1_000,
                    repeated: false,
                },
            )
            .await
            .unwrap()
        );
        assert!(
            !judge_continuation(
                &ContinueJudge(1.0),
                ContinuationInput {
                    policy: &policy,
                    original_task: "task",
                    last_response: "response",
                    facts: &xcb_core::policy::TurnFacts {
                        pending_attention: true,
                        ..facts
                    },
                    consecutive: 0,
                    elapsed_ms: 1_000,
                    repeated: false,
                },
            )
            .await
            .unwrap()
        );
    }

    /// The kernel's own publish cadence is the single refresh path: a config
    /// written by another terminal lands on it with no `Intent::Refresh` in
    /// flight, and a quiet window never publishes twice on the same change.
    #[tokio::test]
    async fn the_kernel_republishes_config_writes_without_a_client_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let state = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = Arc::new(Store::open(&state).unwrap());
        let workspace = crate::private::directory(
            &xcb_core::canonical(directory.path())
                .unwrap()
                .join("workspace"),
        )
        .unwrap();
        let (commands, input) = sync_channel(8);
        let (output, updates) = sync_channel(64);
        let task = tokio::spawn(serve(store.clone(), workspace, None, input, output));
        view_matching(&updates, |view| !view.reduced_motion).await;

        // A sibling terminal writes config.json; no intent is sent.
        let (mut fresh, revision) = Config::load(&state).unwrap();
        fresh.reduced_motion = true;
        fresh.save(&state, revision.as_deref()).unwrap();
        view_matching(&updates, |view| view.reduced_motion).await;

        // The quiet window that follows publishes only on the ~1s idle
        // cadence: roughly once, never a duplicate burst.
        tokio::time::sleep(Duration::from_millis(1_300)).await;
        let mut views = 0usize;
        while let Ok(update) = updates.try_recv() {
            assert!(
                matches!(update, Update::View(_)),
                "idle polling publishes only full views"
            );
            views += 1;
        }
        assert!(
            views <= 2,
            "a quiet window must not duplicate publishes: {views}"
        );
        commands.send(Intent::Quit).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
