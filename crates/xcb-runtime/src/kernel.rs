use crate::{
    Error, Result, attachments, auth,
    config::Config,
    digest, exports, hooks, judge, new_id, now_ms, panes, private,
    process::Pin,
    runner::{self, Observer, Outcome, Progress, RunInput},
    store::Store,
    summary,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
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
    policy::{RouteCandidate, Terminal, next_route, should_continue},
    session::{Message, MessageProvenance, Role, Session, State, Subagent},
    ui::{Intent, RoutePreview, Update},
};

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
            Pin::load(store.root(), *provider).is_ok_and(|pin| runner::provider_admitted(&pin))
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

async fn configured_judge_continuation(
    root: &Path,
    config: &crate::config::JudgeConfig,
    input: ContinuationInput<'_>,
) -> Result<bool> {
    let judge = judge::resolve(root, config)?.ok_or(Error::Unavailable("judge key missing"))?;
    judge_continuation(judge.as_ref(), input).await
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

fn workspace_lease(store: &Store, session: &Session) -> Result<File> {
    let directory = private::directory(&store.root().join("workspace-runs"))?;
    let path = directory.join(format!("{}.lock", digest(session.workspace.as_bytes())));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    private::check_file(&file, 4096)?;
    match file.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(Error::Conflict("workspace has an active writer"));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    // Inspect durable custody only after excluding competing launchers. An
    // earlier owner may have released this lock with an unsettled run between
    // a pre-lock check and acquisition; the filesystem lock alone is no proof
    // that its provider and effects have stopped.
    for run in store.unsettled_runs()? {
        if let Some(id) = run.session
            && store
                .session(&id)?
                .is_some_and(|active| active.workspace == session.workspace)
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
    if !runner::provider_admitted(&pin) {
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
    )
    .await
}

async fn execute_mode(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    mode: ExecutionMode,
    cancel: watch::Receiver<bool>,
    observer: Observer,
) -> Result<Outcome> {
    let pane_generation = mode.pane_generation();
    let config = Config::load(store.root())?.0;
    let session = store
        .session(&session_id)?
        .ok_or(Error::Unavailable("session not found"))?;
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
                "aiCharts local idle export failed: {error}"
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

async fn execute_inner(
    store: Arc<Store>,
    session_id: Id,
    text: String,
    attachments: Vec<xcb_core::session::Attachment>,
    mode: ExecutionMode,
    mut cancel: watch::Receiver<bool>,
    observer: Observer,
) -> Result<Outcome> {
    let pane_generation = mode.pane_generation();
    let supervise = mode.supervise();
    let started = now_ms();
    let mut consecutive = 0u32;
    let mut previous_output = None;
    let mut tried = BTreeSet::new();
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
            id: new_id("m"),
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
        let current = store.append_message(&session_id, session.revision, &message)?;
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
        if !supervise || pane_generation || *cancel.borrow() {
            return Ok(outcome);
        }
        let current = store
            .session(&session_id)?
            .ok_or(Error::Unavailable("session not found"))?;
        if current.account != session.account || current.model.key() != session.model.key() {
            return Ok(outcome);
        }
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
        let continue_turn = if deterministic_continue && current_config.extensions.judge.enabled {
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
            deterministic_continue
        };
        if *cancel.borrow()
            || now_ms().saturating_sub(started)
                >= current_config.extensions.auto_continue.max_elapsed_ms
        {
            return Ok(outcome);
        }
        if continue_turn {
            consecutive += 1;
            previous_output = Some(output_digest);
            observer(Progress::Notice(format!(
                "Auto-continue {consecutive}/{} · same task and permissions",
                current_config.extensions.auto_continue.max_consecutive
            )));
            text = "Continue the existing task from the last confirmed checkpoint. Do not repeat completed effects, expand the task, or answer for the user. Stop if approval or missing input is required.".into();
            attachments = vec![];
            role = Role::System;
            continue;
        }
        if current_config.auto_failover
            && outcome.facts.terminal == Terminal::Failed
            && matches!(
                outcome.facts.failure,
                Some(
                    xcb_core::policy::Failure::AccountQuota | xcb_core::policy::Failure::ModelQuota
                )
            )
        {
            let view = summary::snapshot(&store, Some(&session_id), &current_config, now_ms())?;
            let source = RouteCandidate {
                account: current.account.clone(),
                model: current.model.clone(),
                admitted: true,
                quota_fresh: true,
                available: false,
            };
            let admitted_providers: BTreeSet<_> = Provider::ALL
                .into_iter()
                .filter(|provider| {
                    Pin::load(store.root(), *provider)
                        .is_ok_and(|pin| runner::provider_admitted(&pin))
                })
                .collect();
            let mut candidates = Vec::new();
            for model in &view.models {
                for account in &view.accounts {
                    if account.provider != model.provider
                        || account.busy
                        || !account.enabled
                        || account.authentication_required
                        || account.quota_blocked_until_ms.is_some()
                        || candidates.len() >= 256
                    {
                        continue;
                    }
                    candidates.push(RouteCandidate {
                        account: account.id.clone(),
                        model: model.clone(),
                        admitted: admitted_providers.contains(&model.provider)
                            && auth::has_credentials(&store, &account.id)?,
                        quota_fresh: account.remaining_percent.is_some(),
                        available: account
                            .remaining_percent
                            .is_some_and(|remaining| remaining > 0.0),
                    });
                }
            }
            let checkpointed = !outcome.text.is_empty()
                || outcome.facts.effects == xcb_core::policy::EffectState::None;
            let eligible = eligible_failover_routes(&source, &candidates, &tried, &outcome);
            if eligible.is_empty() {
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
            if *cancel.borrow()
                || now_ms().saturating_sub(started)
                    >= current_config.extensions.auto_continue.max_elapsed_ms
            {
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

async fn cancellation_requested(cancel: &mut watch::Receiver<bool>) {
    // Sender loss also ends supervision, just as it ends an active runner.
    let _ = cancel.wait_for(|cancelled| *cancelled).await;
}

fn eligible_failover_routes(
    source: &RouteCandidate,
    candidates: &[RouteCandidate],
    tried: &BTreeSet<String>,
    outcome: &Outcome,
) -> Vec<usize> {
    let checkpointed =
        !outcome.text.is_empty() || outcome.facts.effects == xcb_core::policy::EffectState::None;
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
    let mut view = summary::snapshot(store, current, config, now_ms())?;
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

fn start(
    store: Arc<Store>,
    id: Id,
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
    let observer: Observer = Arc::new(move |event| match event {
        Progress::Text { thinking, text } if !pane => {
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
            }
        }
        Progress::Notice(message) => queue(&outbox, Update::Notice(message)),
        _ => (),
    });
    let task = tokio::spawn(async move {
        let result = execute(
            store,
            id.clone(),
            text,
            attachments,
            pane,
            cancelled,
            observer,
        )
        .await;
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
                    Err(error) => queue(&outbox, Update::Notice(error.to_string())),
                    _ => (),
                }
                if !quit && let Some((generated, prompt)) = pending_pane_at_boundary(&store, &id, &mut pending_pane, &config, &outbox) {
                    let task = start(store.clone(), generated.clone(), prompt, vec![], true, outbox.clone(), completed.clone());
                    active.insert(generated, task);
                }
                publish(&store, current.as_ref(), &config, &active, &outbox)?;
            },
            _ = ticker.tick() => {
                for _ in 0..16 {
                    let intent = match input.try_recv() { Ok(intent) => intent, Err(TryRecvError::Empty) => break, Err(TryRecvError::Disconnected) => Intent::Quit };
                    if matches!(intent, Intent::Quit) { quit = true; pending_pane = None; for task in active.values() { let _ = task.cancel.send(true); } break; }
                    let handled: Result<()> = (|| {
                        match intent {
                            Intent::Refresh => reload_config(store.root(), &mut config, &mut config_stamp, &outbox),
                            Intent::Submit { text, attachments, .. } => {
                                let prepared: Result<Id> = (|| {
                                    if current.is_none() { current = Some(new_session(&store, &workspace, &config, None, None, None)?.id); }
                                    let id = current.clone().expect("selected session");
                                    if active.contains_key(&id) || active.len() >= 16 { return Err(Error::Conflict("a turn is still running; your draft was restored to the composer")); }
                                    let session = store.session(&id)?.ok_or(Error::Unavailable("session not found"))?;
                                    ready(&store, &session)?;
                                    Ok(id)
                                })();
                                match prepared {
                                    Ok(id) => { active.insert(id.clone(), start(store.clone(), id, text, attachments, false, outbox.clone(), completed.clone())); }
                                    Err(error) => {
                                        queue(&outbox, Update::Draft { text, attachments });
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
                            Intent::Habitat(_) => return Err(Error::Unavailable("persistent backlog and schedules are available from plain xcb chat")),
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
                                else { let generated = generation_session(&store, &session, &config)?; let task = start(store.clone(), generated.id.clone(), pane_prompt(&request)?, vec![], true, outbox.clone(), completed.clone()); active.insert(generated.id, task); }
                            }
                            Intent::AttachPath(path) => { let image = attachments::from_path(store.root(), Path::new(&path))?; queue(&outbox, Update::Attachment(image)); }
                            Intent::AttachRgba { width, height, bytes } => { let image = attachments::from_rgba(store.root(), width, height, bytes)?; queue(&outbox, Update::Attachment(image)); }
                            Intent::Extension { name, enabled } => {
                                let (mut fresh, revision) = Config::load(store.root())?;
                                match name.as_str() { "auto-continue" => fresh.extensions.auto_continue.enabled = enabled, "gobstopper" => fresh.extensions.gobstopper.enabled = enabled, "usage" => fresh.extensions.usage = enabled, "hooks" => fresh.extensions.hooks = enabled, "aicharts-export" => fresh.extensions.aicharts_export = enabled, "aicharts" | "aicharts-upload" => return Err(Error::Unavailable("automatic posting awaits a supported enrolled aiCharts ingress; local exports remain available")), _ => return Err(Error::Unavailable("unknown built-in extension")) }
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
    fn workspace_lease_excludes_concurrent_and_unsettled_writers_across_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
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
                quota_fresh: true,
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

    #[test]
    fn quota_availability_preserves_affinity_and_never_reselects_blocked_onboarding_default() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
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
        let base = directory.path().canonicalize().unwrap();
        let store = Store::open(&base.join("state")).unwrap();
        let workspace = crate::private::directory(&base.join("workspace")).unwrap();
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("swe-test").unwrap(),
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
        let base = directory.path().canonicalize().unwrap();
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
            let base = directory.path().canonicalize().unwrap();
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
                            Update::Draft { text, attachments } => {
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
        let state = directory.path().canonicalize().unwrap().join("state");
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
        let state = directory.path().canonicalize().unwrap().join("state");
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
        let state = directory.path().canonicalize().unwrap().join("state");
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
            "devin/swe-2-high"
        );
    }

    #[test]
    fn new_session_prefers_usable_matching_accounts_before_onboarding_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
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
            id: Id::new("swe-test").unwrap(),
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
        let state = directory.path().canonicalize().unwrap().join("state");
        let store = Arc::new(Store::open(&state).unwrap());
        let other = Store::open(&state).unwrap();
        let account = store
            .add_account(Provider::Devin, "Subscription", 1, None)
            .unwrap();
        let model = ModelChoice {
            provider: Provider::Devin,
            id: Id::new("swe-test").unwrap(),
            label: "Synthetic".into(),
            mode: xcb_core::models::Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        };
        let workspace =
            crate::private::directory(&directory.path().canonicalize().unwrap().join("workspace"))
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
        let state = directory.path().canonicalize().unwrap().join("state");
        let store = Arc::new(Store::open(&state).unwrap());
        let workspace =
            crate::private::directory(&directory.path().canonicalize().unwrap().join("workspace"))
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
