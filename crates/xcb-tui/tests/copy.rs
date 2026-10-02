//! Terminal text is public copy (AGENTS.md, STYLE.md): help, command
//! descriptions, notices, dialogs and empty states name what the reader gets,
//! never the delivery vocabulary of the repository.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::mpsc::sync_channel;
use xcb_core::{
    Id, Provider,
    session::State,
    ui::{AccountRow, BacklogRow, ConversationRow, InboxRow, ProgramRow, ProjectRow, TaskRow},
    usage::Estimate,
};
use xcb_tui::{App, Modal, SLASH_COMMANDS, render};

/// Internal words from the AGENTS.md table, matched as whole words in any case.
const INTERNAL: &[&str] = &[
    "admission",
    "admissions",
    "admit",
    "admits",
    "admitted",
    "qualification",
    "qualified",
    "unqualified",
    "custody",
    "settle",
    "settles",
    "settled",
    "unsettled",
    "settlement",
    "joined",
    "receipt",
    "receipts",
    "bounded",
    "promoted",
    "surface",
    "eligible",
    "lease",
    "leased",
];

/// Literals that never reach the screen.
const NOT_SHOWN: &[&str] = &[
    // A private lock file inside the input-recovery directory.
    ".admission.lock",
];

fn leaks(text: &str) -> Vec<&str> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| {
            INTERNAL.contains(&word.to_lowercase().as_str()) || *word == "XCB" || *word == "Xcb"
        })
        .collect()
}

fn assert_public(label: &str, text: &str) {
    let found = leaks(text);
    assert!(found.is_empty(), "{label} uses {found:?}: {text}");
}

fn screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(110, 40)).unwrap();
    terminal.draw(|frame| render::draw(frame, app, 0)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

/// Everything a dialog or notice can show, including rows below the fold.
fn shown(app: &mut App) -> String {
    let notice = app.notice.clone();
    let mut text = format!("{notice}\n{}", screen(app));
    match &app.modal {
        Some(Modal::Picker { title, items, .. }) => {
            text.push_str(title);
            for item in items {
                text.push('\n');
                text.push_str(&item.label);
            }
        }
        Some(Modal::Inspect { title, lines, .. }) => {
            text.push_str(title);
            text.push('\n');
            text.push_str(&lines.join("\n"));
        }
        Some(Modal::Editor { title, .. }) => text.push_str(title),
        _ => (),
    }
    text
}

fn id(value: &str) -> Id {
    Id::new(value).unwrap()
}

fn account(authentication_required: bool) -> AccountRow {
    AccountRow {
        id: id("a_fixture"),
        provider: Provider::Claude,
        name: "fixture@example.com".into(),
        email: None,
        subscription: "Max".into(),
        remaining_percent: None,
        resets_at_ms: None,
        quota_blocked_until_ms: None,
        authentication_required,
        runway: Estimate::unknown("unmeasured"),
        busy: false,
        active_runs: 0,
        enabled: true,
    }
}

/// A managed chat with one of everything a command can list or inspect.
fn managed() -> App {
    let mut app = App::default();
    let conversation = id("conversation_one");
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.conversation = Some(conversation.clone());
    app.view.managed_cancel_available = true;
    app.view.conversations = vec![ConversationRow {
        id: conversation.clone(),
        title: "Parser".into(),
        workspace: "/work/parser".into(),
        messages: 1,
        updated_at_ms: 1,
    }];
    app.view.accounts = vec![account(true)];
    app.view.backlog = [
        ("task_one", State::NeedsApproval, "needs input"),
        ("task_two", State::Idle, "completed"),
        ("task_three", State::NeedsAnswer, "needs input"),
    ]
    .into_iter()
    .map(|(task, state, status)| BacklogRow {
        id: id(task),
        conversation: conversation.clone(),
        workspace: "/work/parser".into(),
        title: "Fix the parser".into(),
        prompt: "Fix the parser".into(),
        summary: "Waiting".into(),
        status: status.into(),
        state,
        deferred: false,
        priority: 5,
        revision: 3,
        updated_at_ms: 1,
    })
    .collect();
    app.view.tasks = vec![TaskRow {
        id: id("task_one"),
        revision: 3,
        title: "Fix the parser".into(),
        state: State::Working,
        status: Some("running".into()),
        detail: "editing".into(),
        route: Some("claude/default · a_fixture".into()),
        route_reason: None,
        settle: None,
        workspace: "/work/parser".into(),
        binding: None,
        hold_until_ms: None,
        moved_from: None,
        updated_at_ms: 1,
    }];
    app.view.inbox = vec![InboxRow {
        id: id("event_one"),
        task: id("task_one"),
        conversation: conversation.clone(),
        sequence: 1,
        kind: "steering".into(),
        text: "Keep the old flag".into(),
        status: "queued".into(),
        created_at_ms: 1,
        updated_at_ms: 1,
        receipt: None,
    }];
    app.view.programs = vec![ProgramRow {
        parent: id("program_one"),
        phase: "waiting".into(),
        calls: 1,
        max_calls: 3,
        child: Some(id("task_one")),
        child_status: Some("needs approval".into()),
        receipt: None,
    }];
    app.view.projects = vec![ProjectRow {
        workspace: "/work/parser".into(),
        name: "parser".into(),
        goal: "Keep the parser green".into(),
        enabled: true,
        remaining_tasks: 2,
        expires_at_ms: u64::MAX,
        required_provider: None,
        revision: 1,
        status: "active".into(),
    }];
    app
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let (tx, _rx) = sync_channel(64);
    app.handle(Event::Key(KeyEvent::new(code, modifiers)), &tx);
}

#[test]
fn help_and_command_descriptions_use_public_words() {
    let mut app = App::default();
    let mut table = String::new();
    for command in SLASH_COMMANDS {
        table.push_str(&format!(
            "{} {} {} {}\n",
            command.name, command.alias, command.args, command.summary
        ));
    }
    assert_public("the command table", &table);
    assert_public("help", &app.shortcut_lines().join("\n"));
    // The rendered overlay, scrolled through to its end.
    for scroll in (0..200).step_by(10) {
        app.modal = Some(Modal::Help { scroll });
        assert_public("rendered help", &screen(&mut app));
    }
}

#[test]
fn notices_and_dialogs_from_every_command_use_public_words() {
    const ARGUMENTS: &[&str] = &[
        "",
        " zz",
        " all",
        " task_one",
        " task_one keep going",
        " grant 0 0",
        " grant 2 4 Keep it green",
        " pause",
        " resume",
        " add",
        " go",
        " move task_one elsewhere",
        " every 1 x",
        " search",
        " search parser",
        " complete task_one done",
        " reconcile task_one",
        " reconcile zz",
        " run task_one",
        " edit task_one",
        " program_one",
        " event_one",
        " filter parser",
        " on",
        " x y z",
    ];
    for make in [App::default as fn() -> App, managed] {
        for command in SLASH_COMMANDS {
            for arguments in ARGUMENTS {
                let mut app = make();
                app.composer
                    .set_text(&format!("{}{arguments}", command.name));
                key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                let label = format!("{}{arguments}", command.name);
                assert_public(&label, &shown(&mut app));
                // Open whatever the command listed, then act on it.
                if matches!(app.modal, Some(Modal::Picker { .. })) {
                    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                    assert_public(&format!("{label} → first row"), &shown(&mut app));
                    for action in ['s', 'a', 'x'] {
                        key(&mut app, KeyCode::Char(action), KeyModifiers::NONE);
                        assert_public(&format!("{label} → {action}"), &shown(&mut app));
                    }
                }
            }
        }
    }
}

#[test]
fn ctrl_c_escape_and_empty_states_use_public_words() {
    for make in [App::default as fn() -> App, managed] {
        for state in [State::Idle, State::Working, State::NeedsAnswer] {
            let mut app = make();
            app.view.state = state;
            assert_public("the screen", &shown(&mut app));
            app.composer.set_text("draft");
            for code in [KeyCode::Char('c'), KeyCode::Char('c')] {
                key(&mut app, code, KeyModifiers::CONTROL);
                assert_public("Ctrl-C", &shown(&mut app));
            }
            app.modal = Some(Modal::Help { scroll: 0 });
            key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_public("Ctrl-C in a dialog", &shown(&mut app));
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_public("Esc", &shown(&mut app));
        }
    }
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    assert_public("the no-accounts screen", &shown(&mut app));
}

/// String literals outside comments and `#[cfg(test)]` items, escapes
/// turned into spaces. Char literals and lifetimes are skipped.
fn literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let at = |index: usize, text: &str| {
        text.chars()
            .enumerate()
            .all(|(offset, ch)| chars.get(index + offset) == Some(&ch))
    };
    let mut found = Vec::new();
    let (mut index, mut depth) = (0, 0usize);
    let (mut test_item, mut skipping) = (false, false);
    while index < chars.len() {
        if at(index, "//") {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if at(index, "/*") {
            while index < chars.len() && !at(index, "*/") {
                index += 1;
            }
            index += 2;
            continue;
        }
        if at(index, "#[cfg(test)]") {
            test_item = true;
            index += "#[cfg(test)]".len();
            continue;
        }
        match chars[index] {
            '"' => {
                let mut literal = String::new();
                index += 1;
                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' {
                        literal.push(' ');
                        index += 2;
                    } else {
                        literal.push(chars[index]);
                        index += 1;
                    }
                }
                if !skipping && !test_item {
                    found.push(literal);
                }
            }
            '\'' if chars.get(index + 1) == Some(&'\\') => {
                index += 3;
                while index < chars.len() && chars[index] != '\'' {
                    index += 1;
                }
            }
            '\'' if chars.get(index + 2) == Some(&'\'') => index += 2,
            '{' if test_item || skipping => {
                test_item = false;
                skipping = true;
                depth += 1;
            }
            '}' if skipping => {
                depth -= 1;
                skipping = depth > 0;
            }
            ';' if test_item => test_item = false,
            _ => (),
        }
        index += 1;
    }
    found
}

#[test]
fn every_string_the_terminal_can_show_uses_public_words() {
    const SOURCES: &[(&str, &str)] = &[
        ("agent_grid.rs", include_str!("../src/agent_grid.rs")),
        ("composer.rs", include_str!("../src/composer.rs")),
        ("composer/vim.rs", include_str!("../src/composer/vim.rs")),
        (
            "external_editor.rs",
            include_str!("../src/external_editor.rs"),
        ),
        (
            "input_recovery.rs",
            include_str!("../src/input_recovery.rs"),
        ),
        ("interaction.rs", include_str!("../src/interaction.rs")),
        ("lib.rs", include_str!("../src/lib.rs")),
        ("markdown.rs", include_str!("../src/markdown.rs")),
        ("projects.rs", include_str!("../src/projects.rs")),
        ("recovery_ui.rs", include_str!("../src/recovery_ui.rs")),
        ("render.rs", include_str!("../src/render.rs")),
    ];
    let mut checked = 0;
    for (file, source) in SOURCES {
        for literal in literals(source) {
            if !NOT_SHOWN.contains(&literal.as_str()) {
                assert_public(file, &literal);
                checked += 1;
            }
        }
    }
    // The scan must keep seeing the crate's copy, not an empty slice of it.
    assert!(checked > 500, "only {checked} literals scanned");
    assert!(
        literals(include_str!("../src/lib.rs"))
            .iter()
            .any(|literal| literal.contains("Unknown command")),
        "notices are among the scanned literals"
    );
}
