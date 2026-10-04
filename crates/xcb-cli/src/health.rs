//! Account health for `xcb accounts` and `xcb doctor`: whether each account
//! can take a task now and, when you can fix it, the one command that does.

use crate::table::{cell, fit, width};
use xcb_core::{Id, Provider, ui::AccountRow};
use xcb_runtime::{Result, auth, config::Config, store::Store, summary};

/// Where one account stands, in the order the table and doctor check it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Enabled, signed in and not at a known usage limit; `busy` while a run
    /// holds the account.
    Ready { busy: bool },
    /// Enabled, but xcb has no sign-in for it, or the provider rejected the
    /// stored one.
    SignIn,
    /// Enabled and signed in, but at a known usage limit until `until_ms`.
    Limited { until_ms: u64 },
    /// Turned off with `xcb accounts disable`.
    Off,
}

impl Health {
    pub fn classify(row: &AccountRow, signed_in: bool, now: u64) -> Self {
        if !row.enabled {
            return Self::Off;
        }
        if !signed_in || row.authentication_required {
            return Self::SignIn;
        }
        if let Some(until_ms) = row.quota_blocked_until_ms.filter(|until| *until > now) {
            return Self::Limited { until_ms };
        }
        Self::Ready { busy: row.busy }
    }

    /// Ready or busy: the account works; a busy one frees up when its run ends.
    pub fn works(self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// One account and its health.
pub struct Account {
    pub row: AccountRow,
    pub health: Health,
}

/// The accounts view: every account with its health, and the measured pool
/// runway (`seconds`, measured pools, total pools) when there is one.
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub runway: Option<(f64, usize, usize)>,
}

pub fn load(store: &Store, config: &Config, now: u64) -> Result<Accounts> {
    let view = summary::snapshot(store, None, config, now)?;
    let mut accounts: Vec<Account> = view
        .accounts
        .iter()
        .map(|row| {
            // An unreadable sign-in is one to redo, not a reason to hide the
            // whole list.
            let signed_in = auth::has_credentials(store, &row.id).unwrap_or(false);
            Account {
                row: row.clone(),
                health: Health::classify(row, signed_in, now),
            }
        })
        .collect();
    sort(&mut accounts);
    let runway = view
        .total_runway_seconds
        .map(|seconds| (seconds, view.runway_coverage.0, view.runway_coverage.1));
    Ok(Accounts { accounts, runway })
}

/// Providers in the order xcb names them: Claude, Codex.
pub const PROVIDERS: [Provider; 2] = [Provider::Claude, Provider::Codex];

fn provider_rank(provider: Provider) -> usize {
    PROVIDERS
        .iter()
        .position(|candidate| *candidate == provider)
        .unwrap_or(PROVIDERS.len())
}

/// Table order: by provider, then by name.
pub fn sort(accounts: &mut [Account]) {
    accounts.sort_by(|a, b| {
        provider_rank(a.row.provider)
            .cmp(&provider_rank(b.row.provider))
            .then_with(|| a.row.name.cmp(&b.row.name))
            .then_with(|| a.row.id.cmp(&b.row.id))
    });
}

/// The one command that signs an account in again.
pub fn sign_in_step(_provider: Provider, id: &Id) -> String {
    format!("xcb accounts login {id}")
}

/// How long until `until`: `~45m`, `~8h 15m`, `~2d`.
pub fn wait(now: u64, until: u64) -> String {
    let minutes = until.saturating_sub(now).div_ceil(60_000).max(1);
    if minutes >= 60 * 24 {
        format!("~{}d", minutes / (60 * 24))
    } else if minutes >= 60 {
        format!("~{}h {}m", minutes / 60, minutes % 60)
    } else {
        format!("~{minutes}m")
    }
}

/// The plan as the table shows it: the label with a redundant trailing
/// "subscription" dropped (every plan is one), and `–` when nothing is left.
fn plan(label: &str) -> String {
    let trimmed = label.trim();
    let short = trimmed
        .strip_suffix(" subscription")
        .or_else(|| trimmed.strip_suffix(" Subscription"))
        .unwrap_or(trimmed);
    if short.is_empty() || short.eq_ignore_ascii_case("subscription") {
        "–".to_owned()
    } else {
        short.to_owned()
    }
}

/// The STATUS cell: the state, then usage when xcb has measured it. `full`
/// adds when the usage window resets.
fn status(account: &Account, now: u64, full: bool) -> String {
    let row = &account.row;
    let usage = || {
        row.remaining_percent.map(|percent| {
            let reset = row
                .resets_at_ms
                .filter(|at| full && *at > now)
                .map(|at| format!(", resets in {}", wait(now, at)))
                .unwrap_or_default();
            format!(" · {percent:.0}% left{reset}")
        })
    };
    match account.health {
        Health::Ready { busy } => format!(
            "{}{}",
            if busy { "busy" } else { "ready" },
            usage().unwrap_or_default()
        ),
        Health::SignIn => "needs sign-in".to_owned(),
        Health::Limited { until_ms } => format!("limited · retry in {}", wait(now, until_ms)),
        Health::Off => "off".to_owned(),
    }
}

/// Widest the table may get.
const TABLE_COLUMNS: usize = 100;
/// Ids are cut to ten characters plus `…`; any unique prefix resolves.
const ID_COLUMNS: usize = 11;
const PROVIDER_COLUMNS: usize = 8;
const GAP: &str = "  ";

/// The `xcb accounts` table: one row per account and a short key. Columns
/// size to their contents and the whole table stays within 100 columns;
/// the account name gives up space first.
pub fn table(accounts: &[Account], default: Option<&Id>, now: u64) -> String {
    let plans: Vec<String> = accounts
        .iter()
        .map(|account| plan(&account.row.subscription))
        .collect();
    let plan_columns = plans
        .iter()
        .map(|plan| width(plan))
        .max()
        .unwrap_or(0)
        .clamp(4, 24);
    let name_natural = accounts
        .iter()
        .map(|account| width(&fit(&account.row.name, usize::MAX)))
        .max()
        .unwrap_or(0)
        .max("ACCOUNT".len());
    // marker + id + gap + name + gap + provider + gap + plan + gap + status
    let fixed = 2 + ID_COLUMNS + GAP.len() * 4 + PROVIDER_COLUMNS + plan_columns;
    let layout = |full: bool| {
        let statuses: Vec<String> = accounts
            .iter()
            .map(|account| status(account, now, full))
            .collect();
        let status_columns = statuses.iter().map(|s| width(s)).max().unwrap_or(0);
        let room = TABLE_COLUMNS.saturating_sub(fixed + status_columns);
        (statuses, room)
    };
    let (mut statuses, mut room) = layout(true);
    if room < name_natural.min(24) {
        // Reset times are the first detail to go.
        (statuses, room) = layout(false);
    }
    let name_columns = name_natural.min(room).max(16);
    let mut out = format!(
        "  {}{GAP}{}{GAP}{}{GAP}{}{GAP}STATUS\n",
        cell("ID", ID_COLUMNS),
        cell("ACCOUNT", name_columns),
        cell("PROVIDER", PROVIDER_COLUMNS),
        cell("PLAN", plan_columns),
    );
    for ((account, plan), status) in accounts.iter().zip(&plans).zip(&statuses) {
        let row = &account.row;
        out.push_str(&format!(
            "{}{}{GAP}{}{GAP}{}{GAP}{}{GAP}{status}\n",
            if default == Some(&row.id) { "> " } else { "  " },
            cell(row.id.as_str(), ID_COLUMNS),
            cell(&row.name, name_columns),
            cell(row.provider.as_str(), PROVIDER_COLUMNS),
            cell(plan, plan_columns),
        ));
    }
    out.push_str(
        "\n> default account · shortened ids work in accounts commands · --json prints full ids\n",
    );
    out
}

/// The fix for the first enabled account that needs signing in, the
/// default account first.
pub fn first_sign_in(accounts: &[Account], default: Option<&Id>) -> Option<String> {
    let needs = |account: &&Account| account.health == Health::SignIn;
    accounts
        .iter()
        .filter(needs)
        .find(|account| default == Some(&account.row.id))
        .or_else(|| accounts.iter().find(needs))
        .map(|account| sign_in_step(account.row.provider, &account.row.id))
}

/// Account counts for one provider.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub ready: usize,
    pub busy: usize,
    pub sign_in: usize,
    pub limited: usize,
    pub off: usize,
}

impl Counts {
    pub fn of<'a>(accounts: impl IntoIterator<Item = &'a Account>) -> Self {
        let mut counts = Self::default();
        for account in accounts {
            match account.health {
                Health::Ready { busy: false } => counts.ready += 1,
                Health::Ready { busy: true } => counts.busy += 1,
                Health::SignIn => counts.sign_in += 1,
                Health::Limited { .. } => counts.limited += 1,
                Health::Off => counts.off += 1,
            }
        }
        counts
    }

    pub fn total(self) -> usize {
        self.ready + self.busy + self.sign_in + self.limited + self.off
    }

    /// `2 accounts ready` or `4 accounts: 1 ready, 1 needs sign-in, …`.
    pub fn summary(self) -> String {
        let accounts = |count: usize| {
            if count == 1 {
                "1 account".to_owned()
            } else {
                format!("{count} accounts")
            }
        };
        let parts: Vec<String> = [
            (self.ready, "ready"),
            (self.busy, "busy"),
            (self.sign_in, "need sign-in"),
            (self.limited, "at a usage limit"),
            (self.off, "turned off"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, what)| {
            let what = if what == "need sign-in" && count == 1 {
                "needs sign-in"
            } else {
                what
            };
            format!("{count} {what}")
        })
        .collect();
        match parts.as_slice() {
            [] => "no accounts yet".to_owned(),
            [only] if self.total() > 0 => {
                // "2 accounts ready", "1 account turned off"
                let (_, what) = only.split_once(' ').unwrap_or(("", only));
                format!("{} {what}", accounts(self.total()))
            }
            _ => format!("{}: {}", accounts(self.total()), parts.join(", ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::usage::Estimate;

    const NOW: u64 = 1_000_000_000;

    fn row(id: &str, provider: Provider, name: &str, plan: &str) -> AccountRow {
        AccountRow {
            id: Id::new(id).unwrap(),
            provider,
            name: name.into(),
            email: None,
            subscription: plan.into(),
            remaining_percent: None,
            resets_at_ms: None,
            quota_blocked_until_ms: None,
            runway: Estimate::unknown("quota_or_burn_unmeasured"),
            busy: false,
            active_runs: 0,
            enabled: true,
            authentication_required: false,
        }
    }

    #[test]
    fn health_puts_off_first_then_sign_in_then_limits() {
        let mut account = row("a_1", Provider::Codex, "me@example.com", "Pro");
        assert_eq!(
            Health::classify(&account, true, NOW),
            Health::Ready { busy: false }
        );
        assert_eq!(Health::classify(&account, false, NOW), Health::SignIn);
        account.authentication_required = true;
        assert_eq!(Health::classify(&account, true, NOW), Health::SignIn);
        account.authentication_required = false;
        account.quota_blocked_until_ms = Some(NOW + 60_000);
        assert_eq!(
            Health::classify(&account, true, NOW),
            Health::Limited {
                until_ms: NOW + 60_000
            }
        );
        // A limit that already passed no longer counts.
        account.quota_blocked_until_ms = Some(NOW - 1);
        account.busy = true;
        assert_eq!(
            Health::classify(&account, true, NOW),
            Health::Ready { busy: true }
        );
        account.enabled = false;
        assert_eq!(Health::classify(&account, false, NOW), Health::Off);
    }

    #[test]
    fn waits_read_like_the_rest_of_xcb() {
        assert_eq!(wait(NOW, NOW + 45 * 60_000), "~45m");
        assert_eq!(wait(NOW, NOW + (8 * 60 + 15) * 60_000), "~8h 15m");
        assert_eq!(wait(NOW, NOW + 3 * 24 * 60 * 60_000), "~3d");
        assert_eq!(wait(NOW, NOW + 1), "~1m");
    }

    #[test]
    fn counts_read_as_one_sentence() {
        let mut counts = Counts::default();
        assert_eq!(counts.summary(), "no accounts yet");
        counts.ready = 2;
        assert_eq!(counts.summary(), "2 accounts ready");
        counts = Counts {
            off: 1,
            ..Counts::default()
        };
        assert_eq!(counts.summary(), "1 account turned off");
        counts = Counts {
            ready: 1,
            busy: 0,
            sign_in: 1,
            limited: 1,
            off: 2,
        };
        assert_eq!(
            counts.summary(),
            "5 accounts: 1 ready, 1 needs sign-in, 1 at a usage limit, 2 turned off"
        );
        counts.sign_in = 2;
        assert!(counts.summary().contains("2 need sign-in"));
    }

    fn account(row: AccountRow, health: Health) -> Account {
        Account { row, health }
    }

    #[test]
    fn table_aligns_by_display_width_and_fits_100_columns() {
        let mut measured = row(
            "a_1234567890abcdef1234567890abcdef",
            Provider::Claude,
            "me@example.com",
            "Max",
        );
        measured.remaining_percent = Some(62.4);
        measured.resets_at_ms = Some(NOW + (3 * 60 + 10) * 60_000);
        let rows = vec![
            account(
                row(
                    "a_64b454c1aaaaaaaaaaaaaaaaaaaaaaaa",
                    Provider::Codex,
                    "user@example.com",
                    "ChatGPT subscription",
                ),
                Health::SignIn,
            ),
            account(
                row(
                    "a_7042a73eaaaaaaaaaaaaaaaaaaaaaaaa",
                    Provider::Codex,
                    "работа@例え.jp",
                    "ChatGPT subscription",
                ),
                Health::Limited {
                    until_ms: NOW + (8 * 60 + 15) * 60_000,
                },
            ),
            account(measured, Health::Ready { busy: false }),
            account(
                row(
                    "a_deadbeefaaaaaaaaaaaaaaaaaaaaaaaa",
                    Provider::Devin,
                    "devin/a_deadbee",
                    "Imported subscription",
                ),
                Health::Off,
            ),
            account(
                row(
                    "a_00000000aaaaaaaaaaaaaaaaaaaaaaaa",
                    Provider::Claude,
                    "claude/a_0000000",
                    "Subscription",
                ),
                Health::Ready { busy: true },
            ),
        ];
        let default = Id::new("a_1234567890abcdef1234567890abcdef").unwrap();
        let text = table(&rows, Some(&default), NOW);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "  ID           ACCOUNT           PROVIDER  PLAN      STATUS"
        );
        assert_eq!(
            lines[1],
            "  a_64b454c1…  user@example.com  codex     ChatGPT   needs sign-in"
        );
        assert_eq!(
            lines[2],
            "  a_7042a73e…  работа@例え.jp    codex     ChatGPT   limited · retry in ~8h 15m"
        );
        assert_eq!(
            lines[3],
            "> a_12345678…  me@example.com    claude    Max       ready · 62% left, resets in ~3h 10m"
        );
        assert_eq!(
            lines[4],
            "  a_deadbeef…  devin/a_deadbee   devin     Imported  off"
        );
        assert_eq!(
            lines[5],
            "  a_00000000…  claude/a_0000000  claude    –         busy"
        );
        // The STATUS column starts at the same display column on every row,
        // including the one with wide characters.
        let status_column = width(lines[0].split("STATUS").next().unwrap());
        for (line, status) in
            lines[1..=5]
                .iter()
                .zip(["needs sign-in", "limited", "ready", "off", "busy"])
        {
            let start = line.rfind(&format!("  {status}")).unwrap() + 2;
            assert_eq!(width(&line[..start]), status_column, "{line}");
            assert!(width(line) <= 100, "{line}");
        }
        assert!(!text.contains("unmeasured"));
    }

    #[test]
    fn long_names_give_up_space_before_the_table_passes_100_columns() {
        let mut long = row(
            "a_1234567890abcdef1234567890abcdef",
            Provider::Claude,
            "a.very.long.email.address.for.work@subdomain.example.com",
            "Claude Max 20x plan with extras",
        );
        long.remaining_percent = Some(5.0);
        long.resets_at_ms = Some(NOW + 60 * 60_000);
        let text = table(&[account(long, Health::Ready { busy: false })], None, NOW);
        for line in text.lines() {
            assert!(width(line) <= 100, "{} columns: {line}", width(line));
        }
        assert!(text.contains("a.very.long.email"));
        assert!(text.contains('…'));
    }

    #[test]
    fn devin_sign_in_keeps_the_same_account() {
        let id = Id::new("a_devin").unwrap();
        assert_eq!(
            sign_in_step(Provider::Devin, &id),
            "xcb accounts login a_devin"
        );
        assert_eq!(
            sign_in_step(Provider::Codex, &id),
            "xcb accounts login a_devin"
        );
    }
}
