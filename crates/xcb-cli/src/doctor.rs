//! `xcb doctor`: provider builds, account health and unfinished work, then
//! one next step. It exits 0 only when an account can take a task and no
//! check failed or needs attention; the `Doctor` help says so.

use crate::health::{self, Account, Counts, Health};
use crate::{human_bytes, provider_name, table, ux};
use serde_json::{Value, json};
use std::path::Path;
use xcb_core::Provider;
use xcb_runtime::{
    Error, Result, config::Config, judge, now_ms, private, process, runner, store::Store,
};

/// A provider build as doctor found it.
enum Build {
    /// A pinned build: `native` when xcb can run it, `stale` when the
    /// provider didn't answer now and this is the last checked build.
    Pinned {
        version: String,
        native: bool,
        stale: bool,
    },
    /// No build; the sentence says why.
    Missing(String),
}

/// Check tallies for the summary line and the exit code.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub passed: usize,
    pub warnings: usize,
    pub problems: usize,
    /// Accounts that can take a task now (ready or busy with a run).
    pub ready_accounts: usize,
}

impl Tally {
    /// The last line: the kit's check count, except that doctor never says
    /// every check passed while no account can take a task.
    pub fn summary(self) -> String {
        if self.problems == 0 && self.warnings == 0 && self.ready_accounts == 0 {
            "No account can take a task yet.".to_owned()
        } else {
            hraness_cli_kit::style::check_summary(self.passed, self.warnings, self.problems)
        }
    }

    /// 0 when an account can take a task and nothing failed or needs
    /// attention; 1 otherwise.
    pub fn exit_code(self) -> i32 {
        i32::from(!(self.problems == 0 && self.warnings == 0 && self.ready_accounts > 0))
    }
}

/// The detail doctor's JSON has always carried for a pinned build.
fn build_detail(provider: Provider, native: bool) -> &'static str {
    if native && provider == Provider::Devin {
        "pinned · xcb accounts refresh <account> loads the model list after you connect an account"
    } else if native {
        "pinned · checked again before each run"
    } else {
        "pinned · xcb can't run this build yet"
    }
}

/// How to add the first account for a provider.
fn add_account_hint(provider: Provider) -> String {
    match provider {
        Provider::Devin => "after devin auth login, add it with xcb accounts import-devin --source <credentials.toml>".to_owned(),
        provider => format!("xcb setup {provider} adds one"),
    }
}

/// One provider's account lines under its build line, and what they add to
/// the tally and the next step.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AccountSection {
    pub lines: Vec<String>,
    pub passed: usize,
    pub warnings: usize,
    pub ready: usize,
    /// The first account fix, in the order doctor ranks them.
    pub sign_in: Option<String>,
    pub refresh: Option<String>,
}

/// Account lines for one provider. `runnable` is whether xcb can run the
/// provider's build; `has_models` whether any of its models are known.
pub fn account_section(
    style: ux::Style,
    provider: Provider,
    runnable: bool,
    accounts: &[&Account],
    has_models: bool,
    now: u64,
) -> AccountSection {
    let mut section = AccountSection::default();
    let counts = Counts::of(accounts.iter().copied());
    let symbol = |symbol| style.symbol(symbol);
    if accounts.is_empty() {
        if runnable {
            section.lines.push(format!(
                "  {} no accounts yet · {}",
                symbol(ux::Symbol::Off),
                add_account_hint(provider)
            ));
        }
        return section;
    }
    if !runnable {
        section.lines.push(format!(
            "  {} {} can't take tasks until xcb can run {}",
            symbol(ux::Symbol::Off),
            if accounts.len() == 1 {
                "1 account".to_owned()
            } else {
                format!("{} accounts", accounts.len())
            },
            provider_name(provider)
        ));
        return section;
    }
    let works = counts.ready + counts.busy;
    let missing_models = works > 0 && !has_models;
    let summary_symbol = if counts.sign_in > 0 || missing_models {
        ux::Symbol::Warn
    } else if works > 0 {
        ux::Symbol::Ok
    } else {
        ux::Symbol::Off
    };
    section
        .lines
        .push(format!("  {} {}", symbol(summary_symbol), counts.summary()));
    for account in accounts {
        let name = table::fit(&account.row.name, 80);
        match account.health {
            Health::SignIn => {
                let step = health::sign_in_step(provider, &account.row.id);
                section.lines.push(format!(
                    "    {} {name} needs sign-in {} {step}",
                    symbol(ux::Symbol::Warn),
                    symbol(ux::Symbol::Next)
                ));
                section.warnings += 1;
                section.sign_in.get_or_insert(step);
            }
            Health::Limited { until_ms } => section.lines.push(format!(
                "    {} {name} is at its usage limit; retry in {}",
                symbol(ux::Symbol::Off),
                health::wait(now, until_ms)
            )),
            Health::Ready { .. } | Health::Off => {}
        }
    }
    if missing_models {
        let first = accounts
            .iter()
            .find(|account| account.health.works())
            .map(|account| account.row.id.clone());
        if let Some(id) = first {
            let step = format!("xcb accounts refresh {id}");
            section.lines.push(format!(
                "    {} no {} models loaded yet {} {step}",
                symbol(ux::Symbol::Warn),
                provider_name(provider),
                symbol(ux::Symbol::Next)
            ));
            section.warnings += 1;
            section.refresh = Some(step);
        }
    }
    if counts.sign_in == 0 && works > 0 && !missing_models {
        section.passed += 1;
    }
    section.ready = works;
    section
}

pub async fn run(
    root: &Path,
    store: &Store,
    config: &Config,
    only: Option<Provider>,
    executable: Option<&Path>,
    as_json: bool,
) -> Result<i32> {
    if executable.is_some() && only.is_none() {
        return Err(Error::Unavailable("--executable requires --provider"));
    }
    let home = private::directory(&root.join("metadata-home"))?;
    private::directory(&home.join("tmp"))?;
    let style = ux::Style::stdout();
    let now = now_ms();
    let say = |line: &str| {
        if !as_json {
            println!("{line}");
        }
    };
    // Every build first: a provider that isn't installed only matters when
    // nothing else is, or when it has accounts waiting for it.
    let mut builds = Vec::new();
    let mut reports: Vec<Value> = Vec::new();
    for provider in only.map_or_else(|| health::PROVIDERS.to_vec(), |provider| vec![provider]) {
        match process::inspect(provider, executable, &home).await {
            Ok(mut pin) => {
                pin.save(root)?;
                let native = runner::provider_admitted(store.root(), &pin);
                reports.push(json!({"provider":provider,"version":pin.version,"sha256":pin.sha256,"nativeCandidate":native,"detail":build_detail(provider, native)}));
                let version = pin.version.clone();
                builds.push((
                    provider,
                    Build::Pinned {
                        version,
                        native,
                        stale: false,
                    },
                    Some(pin),
                ));
            }
            Err(error) => match process::Pin::load(root, provider) {
                Ok(pin) => {
                    let native = runner::provider_admitted(store.root(), &pin);
                    reports.push(json!({"provider":provider,"version":pin.version,"sha256":pin.sha256,"nativeCandidate":native,"storedPin":true,"detail":build_detail(provider, native)}));
                    builds.push((
                        provider,
                        Build::Pinned {
                            version: pin.version,
                            native,
                            stale: true,
                        },
                        None,
                    ));
                }
                Err(_) => {
                    reports.push(json!({"provider":provider,"error":error.to_string()}));
                    builds.push((provider, Build::Missing(ux::sentence(&error)), None));
                }
            },
        }
    }
    let any_found = builds
        .iter()
        .any(|(_, build, _)| matches!(build, Build::Pinned { .. }));
    let everyone = health::load(store, config, now)?;
    let mut tally = Tally::default();
    let mut account_reports = Vec::new();
    let (mut sign_in, mut refresh) = (None, None);
    let (mut missing_with_accounts, mut unsupported_with_accounts) = (None, None);
    let mut first_ready = None;
    for (index, (provider, build, pin)) in builds.iter().enumerate() {
        let provider = *provider;
        let accounts: Vec<&Account> = everyone
            .accounts
            .iter()
            .filter(|account| account.row.provider == provider)
            .collect();
        let enabled = accounts
            .iter()
            .filter(|account| account.health != Health::Off)
            .count();
        let matters = !any_found || enabled > 0;
        let runnable = matches!(build, Build::Pinned { native: true, .. });
        match build {
            Build::Pinned {
                version,
                native: true,
                stale,
            } => {
                say(&format!(
                    "{} {provider} {version}: ready{}",
                    style.symbol(ux::Symbol::Ok),
                    if *stale {
                        format!(
                            " (last checked build; {} didn't answer now)",
                            provider_name(provider)
                        )
                    } else {
                        String::new()
                    }
                ));
                tally.passed += 1;
                first_ready.get_or_insert(provider);
            }
            Build::Pinned { version, stale, .. } => {
                say(&format!(
                    "{} {provider} {version}: found, but xcb can't run this build yet{}",
                    style.symbol(if matters {
                        ux::Symbol::Warn
                    } else {
                        ux::Symbol::Off
                    }),
                    if *stale {
                        format!(
                            " (last checked build; {} didn't answer now)",
                            provider_name(provider)
                        )
                    } else {
                        String::new()
                    }
                ));
                if matters {
                    tally.warnings += 1;
                    if enabled > 0 {
                        unsupported_with_accounts.get_or_insert(provider);
                    }
                }
            }
            Build::Missing(sentence) => {
                say(&format!(
                    "{} {provider}: {sentence}",
                    style.symbol(if matters {
                        ux::Symbol::Fail
                    } else {
                        ux::Symbol::Off
                    })
                ));
                if matters {
                    tally.problems += 1;
                    if enabled > 0 {
                        missing_with_accounts.get_or_insert(provider);
                    }
                }
            }
        }
        // Devin's model list needs an account's own sign-in; doctor only
        // pins its build.
        if let Some(pin) = pin
            && runnable
            && provider != Provider::Devin
        {
            match runner::probe(store, pin, None).await {
                Ok(models) => store.set_models(provider, &models)?,
                Err(error) => {
                    tally.warnings += 1;
                    reports[index]["modelListError"] = json!(ux::sentence(&error));
                    say(&format!(
                        "  {} xcb couldn't list {} models: {}",
                        style.symbol(ux::Symbol::Warn),
                        provider_name(provider),
                        ux::sentence(&error)
                    ));
                }
            }
        }
        let has_models = store
            .models()?
            .iter()
            .any(|model| model.provider == provider);
        let section = account_section(style, provider, runnable, &accounts, has_models, now);
        for line in &section.lines {
            say(line);
        }
        tally.passed += section.passed;
        tally.warnings += section.warnings;
        tally.ready_accounts += section.ready;
        if sign_in.is_none() {
            sign_in = section.sign_in.clone();
        }
        if refresh.is_none() {
            refresh = section.refresh.clone();
        }
        let counts = Counts::of(accounts.iter().copied());
        account_reports.push(json!({
            "provider": provider,
            "ready": counts.ready,
            "busy": counts.busy,
            "needsSignIn": counts.sign_in,
            "limited": counts.limited,
            "off": counts.off,
            "signIn": accounts.iter().filter(|account| account.health == Health::SignIn).map(|account| json!({"id": account.row.id, "next": health::sign_in_step(provider, &account.row.id)})).collect::<Vec<_>>(),
            "limitedUntilMs": accounts.iter().filter_map(|account| match account.health { Health::Limited { until_ms } => Some(until_ms), _ => None }).collect::<Vec<_>>(),
        }));
    }
    // Reviewed-builds catalog state and builds parked on it.
    let catalog_status = xcb_runtime::catalog::status(root);
    let pending_admissions: Vec<_> = health::PROVIDERS
        .iter()
        .filter_map(|provider| {
            process::pending_build(root, *provider).map(
                |build| json!({"provider":provider,"version":build.version,"sha256":build.sha256}),
            )
        })
        .collect();
    let judge_key = judge::judge_token(store.root())?.map(|(_, source)| source);
    if let Some(source) = judge_key {
        judge::check_key_target(source, &config.extensions.judge)?;
    }
    let judge_key_name = match judge_key {
        Some(judge::JudgeKeySource::Env) => "env",
        Some(judge::JudgeKeySource::Vault) => "vault",
        None => "none",
    };
    // Remove only launch folders already marked safe to delete after their
    // run finished; the parent exiting doesn't prove the provider stopped.
    let sweep = runner::reclaim_launch_artifacts(root, true)?;
    let (judge_model, judge_endpoint) =
        xcb_runtime::jev::effective_target(&config.extensions.judge)?;
    let judge_status = json!({
        "enabled": config.extensions.judge.enabled,
        "key": judge_key_name,
        "model": judge_model,
        "endpoint": judge_endpoint,
    });
    // This device's link record; reachability belongs to `xcb fleet`.
    let remote_status = {
        use xcb_runtime::cloud::custody;
        match (custody::load_device(root)?, custody::load_link(root)?) {
            (Some(device), Some(link)) => {
                let session = custody::load_session(root)?;
                let approved = custody::load_account_key(root)?.is_some();
                json!({
                    "linked": true,
                    "device": device.device,
                    "relay": link.deployment_url,
                    "admitted": approved,
                    "sessionDueForRefresh": session
                        .as_ref()
                        .map(|session| session.due_for_refresh(now_ms()))
                        .unwrap_or(true),
                })
            }
            _ => json!({"linked": false}),
        }
    };
    let unsettled = store.unsettled_runs()?;
    tally.warnings += pending_admissions.len() + unsettled.len();
    if !sweep.unprovable.is_empty() {
        tally.warnings += 1;
    }
    let sandbox = cfg!(target_os = "linux").then(|| xcb_runtime::sandbox::linux_sandbox(root));
    if let Some(status) = &sandbox {
        if status.admitted && status.qualified {
            tally.passed += 1;
        } else {
            tally.warnings += 1;
        }
    }
    // One next step, in order of what blocks the first task.
    let no_accounts = builds.iter().all(|(provider, _, _)| {
        everyone
            .accounts
            .iter()
            .all(|account| account.row.provider != *provider)
    });
    let first_off = everyone
        .accounts
        .iter()
        .find(|account| {
            account.health == Health::Off
                && builds
                    .iter()
                    .any(|(provider, _, _)| *provider == account.row.provider)
        })
        .map(|account| account.row.id.clone());
    let next = if !any_found {
        // Suggest the most common provider first.
        [Provider::Claude, Provider::Codex, Provider::Devin]
            .into_iter()
            .find(|provider| builds.iter().any(|(checked, _, _)| checked == provider))
            .map(install_step)
    } else if let Some(provider) = first_ready.filter(|_| no_accounts) {
        Some(format!("xcb setup {provider}"))
    } else if !unsettled.is_empty() {
        Some("xcb recover".to_owned())
    } else if let Some(step) = sign_in {
        Some(step)
    } else if let Some(provider) = missing_with_accounts {
        Some(install_step(provider))
    } else if let Some(provider) = unsupported_with_accounts {
        Some(format!(
            "install a supported {} build (xcb.sh/docs/providers lists them), then run xcb doctor",
            provider_name(provider)
        ))
    } else if refresh.is_some() {
        refresh
    } else {
        first_off
            .filter(|_| tally.ready_accounts == 0)
            .map(|id| format!("xcb accounts enable {id}"))
    };
    if as_json {
        let mut report = json!({"version":1,"providers":reports,"unsettledRuns":unsettled});
        report["accounts"] = json!(account_reports);
        report["judge"] = judge_status;
        report["remote"] = remote_status;
        report["catalog"] = json!({
            "reviewedBuilds": catalog_status.builds,
            "denied": catalog_status.denied,
            "ageSeconds": catalog_status.age_secs,
            "pendingAdmissions": pending_admissions,
        });
        report["launchArtifacts"] = json!({
            "reclaimed": sweep.reclaimed,
            "reclaimedBytes": sweep.reclaimed_bytes,
            "liveHeld": sweep.live,
            "unreclaimable": sweep.unprovable.len(),
            "unreclaimableBytes": sweep.unprovable_bytes,
            "remedy": if sweep.unprovable.is_empty() {
                Value::Null
            } else {
                json!("kept because xcb can't confirm these runs finished; --yes doesn't remove them")
            },
        });
        if let Some(status) = &sandbox {
            report["sandbox"] = json!({"backend":"bwrap","candidate":status.candidate,"admitted":status.admitted,"unprivilegedUsernsClone":status.unprivileged_userns_clone,"maxUserNamespaces":status.max_user_namespaces,"qualified":status.qualified});
        }
        report["checks"] = json!({
            "passed": tally.passed,
            "warnings": tally.warnings,
            "problems": tally.problems,
            "accountsReady": tally.ready_accounts,
        });
        report["next"] = json!(next);
        crate::print_json(report)?;
        return Ok(tally.exit_code());
    }
    match remote_status.get("linked").and_then(|v| v.as_bool()) {
        Some(true) => println!(
            "{} remote: linked · device {} · relay {}{}",
            style.symbol(ux::Symbol::On),
            remote_status["device"].as_str().unwrap_or("?"),
            remote_status["relay"].as_str().unwrap_or("?"),
            if remote_status["admitted"].as_bool().unwrap_or(false) {
                ""
            } else {
                " · waiting for a linked device to approve it (xcb remote admit)"
            },
        ),
        _ => println!(
            "{} remote: not linked (xcb link connects this machine)",
            style.symbol(ux::Symbol::Off)
        ),
    }
    let catalog_age = match catalog_status.age_secs {
        Some(secs) if secs < 120 => format!("refreshed {secs}s ago"),
        Some(secs) if secs < 7200 => format!("refreshed {}m ago", secs / 60),
        Some(secs) => format!("refreshed {}h ago", secs / 3600),
        None => "not fetched yet".to_owned(),
    };
    println!(
        "{} catalog: {} reviewed builds{} · {catalog_age}",
        style.symbol(ux::Symbol::On),
        catalog_status.builds,
        if catalog_status.denied > 0 {
            format!(" · {} denied", catalog_status.denied)
        } else {
            String::new()
        },
    );
    for pending in &pending_admissions {
        println!(
            "{} {}: {} is waiting for review before xcb runs it",
            style.symbol(ux::Symbol::Warn),
            pending["provider"].as_str().unwrap_or("provider"),
            pending["version"].as_str().unwrap_or("discovered build"),
        );
    }
    if let Some(status) = &sandbox {
        println!("{}", sandbox_line(style, status));
    }
    println!(
        "{} judge: {} · key {judge_key_name} · {judge_endpoint}",
        if config.extensions.judge.enabled {
            style.symbol(ux::Symbol::On)
        } else {
            style.symbol(ux::Symbol::Off)
        },
        if config.extensions.judge.enabled {
            "enabled"
        } else {
            "disabled"
        },
    );
    if sweep.reclaimed > 0 {
        println!(
            "{} launch folders: removed {} finished {} ({})",
            style.symbol(ux::Symbol::Ok),
            sweep.reclaimed,
            if sweep.reclaimed == 1 {
                "directory"
            } else {
                "directories"
            },
            human_bytes(sweep.reclaimed_bytes),
        );
    }
    if !sweep.unprovable.is_empty() {
        println!(
            "{} launch folders: kept {} {} ({}) because xcb can't confirm their runs finished.",
            style.symbol(ux::Symbol::Warn),
            sweep.unprovable.len(),
            if sweep.unprovable.len() == 1 {
                "directory"
            } else {
                "directories"
            },
            human_bytes(sweep.unprovable_bytes),
        );
        println!("  Inspect the recorded runs with xcb recover before removing them.");
    }
    for run in &unsettled {
        println!(
            "{} run {} hasn't finished; xcb keeps its account held until it confirms how the run ended",
            style.symbol(ux::Symbol::Warn),
            run.id
        );
    }
    println!("\n{}", tally.summary());
    if let Some(next) = next {
        ux::next(&next);
    }
    Ok(tally.exit_code())
}

/// The next step when a provider isn't installed.
fn install_step(provider: Provider) -> String {
    format!(
        "install {}, or run xcb doctor --provider {provider} --executable <absolute path>",
        provider_name(provider)
    )
}

/// The Linux sandbox check. Without a working bwrap and a current sandbox
/// test result for this machine, xcb starts no provider here.
fn sandbox_line(style: ux::Style, status: &xcb_runtime::sandbox::LinuxSandbox) -> String {
    let userns = match (status.unprivileged_userns_clone, status.max_user_namespaces) {
        (Some(false), _) | (_, Some(0)) => " · user namespaces are restricted",
        _ => "",
    };
    let (symbol, detail) = match &status.candidate {
        Some(path) if status.admitted && status.qualified => (
            ux::Symbol::Ok,
            format!("bwrap at {} passed xcb's sandbox checks", path.display()),
        ),
        Some(path) if status.admitted => (
            ux::Symbol::Warn,
            format!(
                "bwrap at {} works, but this machine has no current sandbox test result, so xcb won't start providers here",
                path.display()
            ),
        ),
        Some(path) => (
            ux::Symbol::Warn,
            format!(
                "bwrap at {} didn't pass xcb's checks, so xcb won't start providers here",
                path.display()
            ),
        ),
        None => (
            ux::Symbol::Warn,
            "bwrap isn't installed, so xcb won't start providers here".to_owned(),
        ),
    };
    format!("{} sandbox: {detail}{userns}", style.symbol(symbol))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{Id, ui::AccountRow, usage::Estimate};

    const NOW: u64 = 2_000_000_000;

    fn account(id: &str, provider: Provider, name: &str, health: Health) -> Account {
        Account {
            row: AccountRow {
                id: Id::new(id).unwrap(),
                provider,
                name: name.into(),
                email: None,
                subscription: "Subscription".into(),
                remaining_percent: None,
                resets_at_ms: None,
                quota_blocked_until_ms: None,
                runway: Estimate::unknown("quota_or_burn_unmeasured"),
                busy: false,
                enabled: health != Health::Off,
                authentication_required: false,
            },
            health,
        }
    }

    /// The live report behind this: doctor said "All 3 checks passed" while
    /// a Codex account needed reconnecting.
    #[test]
    fn an_account_that_needs_sign_in_is_a_warning_with_its_one_fix() {
        let accounts = [
            account(
                "a_ready",
                Provider::Codex,
                "me@example.com",
                Health::Ready { busy: false },
            ),
            account(
                "a_signin",
                Provider::Codex,
                "old@example.com",
                Health::SignIn,
            ),
            account(
                "a_limit",
                Provider::Codex,
                "work@example.com",
                Health::Limited {
                    until_ms: NOW + (8 * 60 + 15) * 60_000,
                },
            ),
            account("a_off1", Provider::Codex, "codex/a_off1", Health::Off),
            account("a_off2", Provider::Codex, "codex/a_off2", Health::Off),
        ];
        let refs: Vec<&Account> = accounts.iter().collect();
        let section = account_section(ux::Style::PLAIN, Provider::Codex, true, &refs, true, NOW);
        assert_eq!(
            section.lines,
            [
                "  ⚠ 5 accounts: 1 ready, 1 needs sign-in, 1 at a usage limit, 2 turned off",
                "    ⚠ old@example.com needs sign-in → xcb accounts login a_signin",
                "    ○ work@example.com is at its usage limit; retry in ~8h 15m",
            ]
        );
        assert_eq!(section.warnings, 1);
        assert_eq!(section.passed, 0);
        assert_eq!(section.ready, 1);
        assert_eq!(
            section.sign_in.as_deref(),
            Some("xcb accounts login a_signin")
        );
        let tally = Tally {
            passed: 3,
            warnings: section.warnings,
            problems: 0,
            ready_accounts: section.ready,
        };
        assert_eq!(tally.summary(), "1 warning.");
        assert_eq!(tally.exit_code(), 1);
    }

    #[test]
    fn devin_shows_the_import_hint_only_without_an_account() {
        let none = account_section(ux::Style::PLAIN, Provider::Devin, true, &[], false, NOW);
        assert_eq!(
            none.lines,
            [
                "  ○ no accounts yet · after devin auth login, add it with xcb accounts import-devin --source <credentials.toml>"
            ]
        );
        assert_eq!((none.warnings, none.passed, none.ready), (0, 0, 0));
        let imported = [account(
            "a_devin",
            Provider::Devin,
            "devin/a_devin",
            Health::Ready { busy: false },
        )];
        let refs: Vec<&Account> = imported.iter().collect();
        let with_models =
            account_section(ux::Style::PLAIN, Provider::Devin, true, &refs, true, NOW);
        assert_eq!(with_models.lines, ["  ✓ 1 account ready"]);
        assert_eq!(
            (with_models.warnings, with_models.passed, with_models.ready),
            (0, 1, 1)
        );
        assert!(!with_models.lines.concat().contains("import"));
        // Signed in but no model list yet: the one fix is a refresh.
        let without = account_section(ux::Style::PLAIN, Provider::Devin, true, &refs, false, NOW);
        assert_eq!(
            without.lines,
            [
                "  ⚠ 1 account ready",
                "    ⚠ no Devin models loaded yet → xcb accounts refresh a_devin",
            ]
        );
        assert_eq!(without.warnings, 1);
        assert_eq!(
            without.refresh.as_deref(),
            Some("xcb accounts refresh a_devin")
        );
    }

    #[test]
    fn accounts_for_a_build_xcb_cannot_run_wait_without_extra_warnings() {
        let accounts = [account(
            "a_x",
            Provider::Codex,
            "me@example.com",
            Health::SignIn,
        )];
        let refs: Vec<&Account> = accounts.iter().collect();
        let section = account_section(ux::Style::PLAIN, Provider::Codex, false, &refs, false, NOW);
        assert_eq!(
            section.lines,
            ["  ○ 1 account can't take tasks until xcb can run Codex"]
        );
        assert_eq!((section.warnings, section.passed, section.ready), (0, 0, 0));
    }

    #[test]
    fn doctor_never_says_everything_passed_when_no_account_can_work() {
        let idle = Tally {
            passed: 3,
            ..Tally::default()
        };
        assert_eq!(idle.summary(), "No account can take a task yet.");
        assert_eq!(idle.exit_code(), 1);
        let healthy = Tally {
            ready_accounts: 2,
            ..idle
        };
        assert_eq!(healthy.summary(), "All 3 checks passed.");
        assert_eq!(healthy.exit_code(), 0);
        let broken = Tally {
            problems: 1,
            ..healthy
        };
        assert_eq!(broken.exit_code(), 1);
    }
}
