use ratatui::{Terminal, backend::TestBackend};
use xcb_core::{
    Id, Provider,
    models::{Mode, ModelChoice},
    panes::{Node, Pane, Source},
    session::{Attachment, Message, MessageProvenance, Role, Session, State},
};
use xcb_tui::{App, render};

fn app() -> App {
    let mut app = App::default();
    app.view.session = Some(Session {
        id: Id::new("session").unwrap(),
        account: Id::new("personal").unwrap(),
        model: ModelChoice {
            provider: Provider::Devin,
            id: Id::new("gpt-6-astra-max").unwrap(),
            label: "Astra Max".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        },
        workspace: "/project".into(),
        title: "Example".into(),
        pane: Id::new("focus").unwrap(),
        state: State::NeedsAnswer,
        revision: 1,
        created_at_ms: 1,
        last_active_at_ms: 2,
    });
    app.view.state = State::NeedsAnswer;
    app
}

#[test]
fn every_preset_and_narrow_terminal_retains_model_and_status_chrome() {
    for width in [40, 80, 120] {
        for pane in Pane::presets() {
            let mut app = app();
            app.view.pane = pane;
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal
                .draw(|frame| render::draw(frame, &mut app, 0))
                .unwrap();
            let contents: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(contents.contains("Astra Max"));
            assert!(contents.contains("needs answer"));
            assert!(!contents.contains("Context:"));
        }
    }
}

#[test]
fn tool_activity_is_hidden_until_explicitly_revealed() {
    let mut app = app();
    app.view.pane = Pane::presets().remove(2);
    app.view.activity = vec!["private tool detail".into()];
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let hidden: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(hidden.contains("Tool activity hidden"));
    assert!(!hidden.contains("private tool detail"));
    app.show_activity = true;
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(shown.contains("private tool detail"));
}

#[test]
fn attachment_chips_help_and_paused_follow_state_are_visible() {
    let mut app = app();
    app.attachments.push(Attachment {
        digest: "a".repeat(64),
        media_type: "image/png".into(),
        bytes: 2048,
        width: 640,
        height: 480,
    });
    app.stream = "streaming line\n".repeat(50);
    app.paused.set(true);
    app.scroll.set(10);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("[image:png 640×480 · 2 KiB]"));
    assert!(contents.contains("paused · End follows"));
    assert!(contents.contains("? help"));
    assert!(contents.contains("? needs answer"));
}

#[test]
fn tiny_terminal_and_large_text_cannot_panic_the_renderer() {
    for (width, height) in [
        (0, 0),
        (1, 1),
        (20, 5),
        (24, 7),
        (30, 8),
        (40, 8),
        (200, 60),
    ] {
        let mut app = app();
        // Three attachments shrink the composer budget to zero at these
        // heights; a 70k-row body stresses every saturating offset.
        for _ in 0..3 {
            app.attachments.push(Attachment {
                digest: "a".repeat(64),
                media_type: "image/png".into(),
                bytes: 2048,
                width: 640,
                height: 480,
            });
        }
        app.stream = "x\n".repeat(70_000);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut app, 0))
            .unwrap();
    }
}

#[test]
fn a_paused_viewport_is_pinned_while_output_streams() {
    let mut app = app();
    app.stream = (1..=60).map(|line| format!("line-{line:03}\n")).collect();
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert!(
        app.scroll_top() > 0,
        "tail-follow starts at the bottom of the transcript"
    );
    assert!(!app.paused.get());

    // Pause at an absolute line index, then keep streaming: the viewport must
    // not drift (the audit watched it wander line-052 → line-062).
    app.scroll.set(40);
    app.paused.set(true);
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert_eq!(app.scroll_top(), 40);
    app.stream.push_str(
        &(61..=120)
            .map(|line| format!("line-{line:03}\n"))
            .collect::<String>(),
    );
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert_eq!(
        app.scroll_top(),
        40,
        "a growing tail must not move a pinned viewport"
    );
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("line-041"));
    assert!(contents.contains("paused · End follows"));

    // PageUp bases its step on the last rendered top line, then End follows.
    app.handle(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        )),
        &std::sync::mpsc::sync_channel(1).0,
    );
    assert_eq!(app.scroll.get(), 30);
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert_eq!(app.scroll_top(), 30);
    app.handle(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::End,
            crossterm::event::KeyModifiers::NONE,
        )),
        &std::sync::mpsc::sync_channel(1).0,
    );
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert!(app.scroll_top() > 40, "End resumes following the tail");
}

#[test]
fn a_run_owned_by_another_terminal_is_not_rendered_as_recovery() {
    let mut app = app();
    app.view.state = State::Working;
    app.view.session.as_mut().unwrap().state = State::Working;
    app.view.remote_active = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("working elsewhere"));
    assert!(!contents.contains("needs recovery"));

    // The same session without a live owner still reports recovery.
    app.view.remote_active = false;
    app.view.state = State::Uncertain;
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("needs recovery"));
}

fn provenance(run: Option<&str>) -> MessageProvenance {
    MessageProvenance {
        account: Id::new("personal").unwrap(),
        model: ModelChoice {
            provider: Provider::Devin,
            id: Id::new("gpt-6-astra-max").unwrap(),
            label: "Astra Max".into(),
            mode: Mode::Fixed,
            resolved: None,
            effort: None,
            observed_at_ms: 1,
        },
        run: run.map(|value| Id::new(value).unwrap()),
    }
}

#[test]
fn response_and_thinking_provenance_boundaries_are_visible_without_repetition() {
    let mut app = app();
    app.view.pane = Pane::focus();
    app.show_thinking = true;
    app.show_history = true;
    app.view.messages = vec![
        Message {
            id: Id::new("m1").unwrap(),
            role: Role::Assistant,
            text: "first".into(),
            attachments: vec![],
            at_ms: 1,
            provenance: Some(provenance(Some("r1"))),
        },
        Message {
            id: Id::new("m2").unwrap(),
            role: Role::Assistant,
            text: "second".into(),
            attachments: vec![],
            at_ms: 2,
            provenance: Some(provenance(Some("r1"))),
        },
        Message {
            id: Id::new("m3").unwrap(),
            role: Role::Thinking,
            text: "thought".into(),
            attachments: vec![],
            at_ms: 3,
            provenance: Some(provenance(Some("r2"))),
        },
        Message {
            id: Id::new("m4").unwrap(),
            role: Role::Assistant,
            text: "third".into(),
            attachments: vec![],
            at_ms: 4,
            provenance: Some(provenance(Some("r2"))),
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    // One boundary label per run change, shared across thinking and responses.
    assert_eq!(contents.matches("r1").count(), 1);
    assert_eq!(contents.matches("r2").count(), 1);
    assert!(contents.contains("first"));
    assert!(contents.contains("second"));
    assert!(contents.contains("third"));
    assert!(contents.contains("thought"));
    // Reasoning renders before the response it produced.
    let thought = contents.find("thought").unwrap();
    let third = contents.find("third").unwrap();
    assert!(thought < third);
}

#[test]
fn slash_typeahead_menu_renders_above_the_composer() {
    let mut app = app();
    app.composer.set_text("/ac");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("/accounts"));
    assert!(contents.contains("pick the billing account"));
    assert!(contents.contains("commands"));
}

#[test]
fn slash_typeahead_menu_stays_hidden_for_plain_text() {
    let mut app = app();
    app.composer.set_text("hello world");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(!contents.contains("Tab completes"));
}
#[test]
fn footer_previews_the_pending_route_without_a_session() {
    let mut app = App::default();
    app.view.pending_route = Some(xcb_core::ui::RoutePreview {
        account: "pilot@example.com".into(),
        provider: Provider::Claude,
        model: "claude/default/high".into(),
    });
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("claude/default/high · pilot@example.com"));
    assert!(!contents.contains("Choose an account"));
}

#[test]
fn empty_subagent_and_extension_lists_collapse() {
    let mut app = app();
    app.view.pane = Pane::focus();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(!contents.contains("Subagents"));
    assert!(!contents.contains("No subagent activity"));

    // A live subagent expands the widget again.
    app.view.subagents = vec![xcb_core::session::Subagent {
        id: Id::new("worker").unwrap(),
        label: "auditing".into(),
        state: State::Working,
        model: None,
    }];
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("Subagents"));
    assert!(contents.contains("auditing"));
}

#[test]
fn a_submitted_prompt_echoes_in_the_transcript_immediately() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(4);
    let mut app = app();
    app.composer.set_text("ship the fix");
    app.handle(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        )),
        &tx,
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("You"));
    assert!(contents.contains("ship the fix"));
}

#[test]
fn tool_calls_render_as_compact_cells_and_expand_on_ctrl_u() {
    let mut app = app();
    app.view.pane = Pane::focus();
    app.view.messages = vec![
        Message {
            id: Id::new("u1").unwrap(),
            role: Role::User,
            text: "run it".into(),
            at_ms: 1,
            attachments: vec![],
            provenance: None,
        },
        Message {
            id: Id::new("t1").unwrap(),
            role: Role::Tool,
            text: "workspace_exec: {\"stdout\":\"hi there\"}".into(),
            at_ms: 2,
            attachments: vec![],
            provenance: None,
        },
        Message {
            id: Id::new("a1").unwrap(),
            role: Role::Assistant,
            text: "done".into(),
            at_ms: 3,
            attachments: vec![],
            provenance: None,
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    // Collapsed: the call narrates the turn without dumping its output.
    assert!(contents.contains("workspace_exec"));
    assert!(!contents.contains("hi there"));

    app.show_activity = true;
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("hi there"));
}

#[test]
fn a_tool_in_flight_shows_at_the_tail_until_its_cell_lands() {
    let mut app = app();
    app.view.state = State::Working;
    app.view.session.as_mut().unwrap().state = State::Working;
    app.view.messages = vec![
        Message {
            id: Id::new("u1").unwrap(),
            role: Role::User,
            text: "run it".into(),
            at_ms: 1,
            attachments: vec![],
            provenance: None,
        },
        Message {
            id: Id::new("t1").unwrap(),
            role: Role::Tool,
            text: "workspace_exec: ok".into(),
            at_ms: 2,
            attachments: vec![],
            provenance: None,
        },
    ];
    app.view.activity = vec!["workspace_exec".into(), "workspace_write".into()];
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    // The settled call renders once; only the still-running call trails.
    assert!(contents.contains("workspace_exec"));
    assert!(contents.contains("workspace_write …"));
    assert!(!contents.contains("workspace_exec …"));
}

#[test]
fn the_working_badge_shows_elapsed_time_and_respects_reduced_motion() {
    let mut app = app();
    app.view.state = State::Working;
    app.view.session.as_mut().unwrap().state = State::Working;
    app.working_since = Some(std::time::Instant::now() - std::time::Duration::from_millis(90_500));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("Working · 1m 30s"));
    assert!(!contents.contains("unmeasured"));

    app.view.reduced_motion = true;
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("● Working · 1m 30s"));
}

#[test]
fn global_conversation_shows_managed_tasks_instead_of_provider_chrome() {
    let mut app = app();
    app.view.session = None;
    app.view.state = State::NeedsAnswer;
    app.view
        .extensions
        .insert(0, ("algal supervisor".into(), "on".into()));
    app.view.tasks = vec![xcb_core::ui::TaskRow {
        id: Id::new("t_login").unwrap(),
        title: "Fix login redirect".into(),
        state: State::NeedsAnswer,
        detail: "the worker needs your input".into(),
        route: Some("claude/default/high · user@example.com".into()),
        workspace: "/project".into(),
        updated_at_ms: 1,
    }];
    app.view.pane = Pane::focus();
    let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("Tasks · /tasks"));
    assert!(contents.contains("Fix login redirect"));
    assert!(contents.contains("1 needs you"));
    assert!(!contents.contains("Choose an account"));
}

#[test]
fn empty_global_conversation_has_quiet_dispatcher_chrome() {
    let mut app = app();
    app.view.session = None;
    app.view
        .extensions
        .insert(0, ("algal supervisor".into(), "on".into()));
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("global dispatcher"));
    assert!(contents.contains("Describe work or ask about the running task swarm"));
    assert!(contents.contains("Message · / for commands"));
    assert!(!contents.contains("usage: unmeasured"));
}

#[test]
fn managed_work_does_not_advertise_direct_session_followups() {
    let mut app = app();
    app.view.session = None;
    app.view.state = State::Working;
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(contents.contains("Describe new work or ask for status"));
    assert!(!contents.contains("Type a follow-up"));
}

/// A pane whose transcript column is 1/`columns` of the terminal width — word
/// wrap accuracy only matters at widths the built-in presets never produce.
fn narrow_transcript_pane(columns: u32) -> Pane {
    let mut children = vec![Node::Widget {
        source: Source::Responses,
        lines: None,
    }];
    while (children.len() as u32) < columns {
        children.push(Node::Spacer { lines: 0 });
    }
    Pane {
        version: 1,
        id: Id::new("narrow").unwrap(),
        title: "narrow".into(),
        root: Node::Row { children },
    }
}

fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn wrapped_tail_follow_shows_the_newest_row_at_word_boundaries() {
    let mut app = app();
    app.view.pane = narrow_transcript_pane(3);
    // At width 10 "abcde fghijk lmnopq" wraps to three rows, not the two a
    // cell count suggests — the old estimate hid the newest row at the tail.
    app.stream = (1..=20)
        .map(|line| format!("fill-{line:02}\n"))
        .collect::<String>()
        + "abcde fghijk lmnopq";
    let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents = buffer_text(&terminal);
    assert!(
        contents.contains("lmnopq"),
        "the newest wrapped row must be visible while following:\n{contents}"
    );
}

#[test]
fn cjk_and_wide_grapheme_lines_wrap_on_cell_boundaries() {
    let mut app = app();
    app.view.pane = narrow_transcript_pane(3);
    // 19 cells wide but three wrapped rows (4 + 10 + 6): a two-cell estimate
    // drops the tail row.
    app.stream = (1..=20)
        .map(|line| format!("fill-{line:02}\n"))
        .collect::<String>()
        + "あい うえおかきくけこ";
    let mut terminal = Terminal::new(TestBackend::new(30, 12)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents = buffer_text(&terminal);
    // Two-cell graphemes occupy a symbol cell plus a padding cell, so the
    // buffer spells the row "く け こ".
    assert!(
        contents.contains("く け こ"),
        "the tail row of a wide-grapheme line must be visible:\n{contents}"
    );
    // Emoji measure two cells per grapheme too; the stream's last row shows.
    app.stream = (1..=20)
        .map(|line| format!("fill-{line:02}\n"))
        .collect::<String>()
        + "ok 🙂";
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert!(buffer_text(&terminal).contains("🙂"));
}

#[test]
fn tail_follow_reaches_rows_beyond_the_u16_scroll_limit() {
    let mut app = app();
    let mut stream = "x\n".repeat(69_999);
    stream.push_str("LAST\n");
    app.stream = stream;
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert!(
        app.scroll_tail() > u16::MAX as u32,
        "the tail must index beyond the old u16 scroll ceiling"
    );
    let contents = buffer_text(&terminal);
    assert!(
        contents.contains("LAST"),
        "the newest row is still rendered past 65,535 wrapped rows"
    );
}

#[test]
fn tabs_expand_to_the_next_four_column_stop() {
    let mut app = app();
    app.view.pane = Pane::focus();
    app.show_history = true;
    app.show_activity = true;
    app.view.messages = vec![
        Message {
            id: Id::new("m1").unwrap(),
            role: Role::Assistant,
            text: "a\tb\tmid\tx".into(),
            attachments: vec![],
            at_ms: 1,
            provenance: None,
        },
        Message {
            id: Id::new("t1").unwrap(),
            role: Role::Tool,
            text: "exec: out\tput".into(),
            attachments: vec![],
            at_ms: 2,
            provenance: None,
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let contents = buffer_text(&terminal);
    // "a\tb\tmid\tx" lands on stops 4/8/12; the expanded tool cell indents two.
    assert!(
        contents.contains("a   b   mid x"),
        "tabs must expand: {contents}"
    );
    assert!(
        contents.contains("out   put"),
        "tool cells expand tabs too: {contents}"
    );
}

#[test]
fn hardware_cursor_tracks_the_composer_cell() {
    use ratatui::layout::Position;
    let mut app = app();
    app.composer.set_text("hello");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    // The composer occupies rows 20..23; its TOP|BOTTOM borders leave the text
    // row at y=21 and the cursor five cells in.
    assert_eq!(
        terminal.get_cursor_position().unwrap(),
        Position::new(5, 21)
    );
    // Two lines grow the composer to rows 19..23; the cursor sits on line 2.
    app.composer.set_text("ab\ncd");
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    assert_eq!(
        terminal.get_cursor_position().unwrap(),
        Position::new(2, 21)
    );
}

#[test]
fn hardware_cursor_tracks_modal_editor_and_picker_cells() {
    use ratatui::layout::Position;
    use ratatui_textarea::{CursorMove, TextArea};
    use xcb_tui::{EditorKind, Modal, PickAction, PickItem};
    let mut app = app();
    let mut textarea = TextArea::from(vec!["hello".to_string()]);
    textarea.move_cursor(CursorMove::End);
    app.modal = Some(Modal::Editor {
        title: "Prompt".into(),
        textarea: Box::new(textarea),
        kind: EditorKind::Prompt,
        error: None,
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    // The 76x22 modal sits at (2,1); its border leaves text at (3,2) and the
    // end-of-line cursor lands on cell 8 of that row.
    assert_eq!(terminal.get_cursor_position().unwrap(), Position::new(8, 2));

    app.modal = Some(Modal::Picker {
        title: "Accounts · select an account".into(),
        query: "en".into(),
        items: vec![PickItem {
            label: "personal".into(),
            action: PickAction::Account(Id::new("personal").unwrap()),
        }],
        selected: 0,
    });
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    // The title " Accounts · select an account · en " measures 35 cells; the
    // cursor tracks its end on the border row.
    assert_eq!(
        terminal.get_cursor_position().unwrap(),
        Position::new(37, 1)
    );
}

/// The application-side wrapper must produce exactly the rows ratatui's
/// `WordWrapper { trim: false }` would — this renders the same body through
/// both paths and compares every cell of the transcript column.
#[test]
fn transcript_rows_match_ratatui_word_wrap() {
    use ratatui::layout::Rect;
    use ratatui::widgets::{Paragraph, Wrap};
    let cases: Vec<String> = vec![
        "abcde fghijk lmnopq".into(),
        "あい うえおかきくけこ".into(),
        "aaaaa  bb ccc dd".into(),
        "   leading   spaces   ".into(),
        "trailing   ".into(),
        "one".into(),
        "word ".repeat(31),
        "x".repeat(47),
        "🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂🙂".into(),
        "ends with  two  spaces  ".into(),
        "m i x e d  spaces  everywhere".into(),
        "🙂🙂 🙂🙂🙂🙂🙂".into(),
        "word,another.yet-more;stuff".into(),
        "ながいことばがながいことばがながいことば".into(),
    ];
    // `columns` splits a 30-wide terminal into that many equal columns; the
    // transcript takes the first, so the wrap width is 30/columns.
    for columns in [30u32, 15, 10, 6, 5, 3, 2, 1] {
        let width = (30 / columns) as u16;
        for case in &cases {
            let body = format!("{case}\nZ");
            let mut app = app();
            app.view.pane = narrow_transcript_pane(columns);
            app.stream = body.clone();
            let mut terminal = Terminal::new(TestBackend::new(30, 200)).unwrap();
            terminal
                .draw(|frame| render::draw(frame, &mut app, 0))
                .unwrap();
            // Transcript body rows start one row under the widget's heading.
            let got: Vec<String> = (2..200)
                .map(|y| {
                    (0..width)
                        .map(|x| {
                            terminal
                                .backend()
                                .buffer()
                                .cell((x, y))
                                .unwrap()
                                .symbol()
                                .to_owned()
                        })
                        .collect::<String>()
                })
                .collect();
            // Reference: ratatui's own Paragraph wrapper on the same text. The
            // buffer is wider than the rect so over-wide unbreakable rows do
            // not hit buffer bounds; we compare only inside the wrap width.
            let mut reference = Terminal::new(TestBackend::new(64, 200)).unwrap();
            reference
                .draw(|frame| {
                    frame.render_widget(
                        Paragraph::new(body.clone()).wrap(Wrap { trim: false }),
                        Rect::new(0, 0, width, 198),
                    );
                })
                .unwrap();
            let want: Vec<String> = (0..198)
                .map(|y| {
                    (0..width)
                        .map(|x| {
                            reference
                                .backend()
                                .buffer()
                                .cell((x, y))
                                .unwrap()
                                .symbol()
                                .to_owned()
                        })
                        .collect::<String>()
                })
                .collect();
            let last_row = want
                .iter()
                .position(|row| row.starts_with('Z'))
                .expect("the sentinel row must render");
            assert_eq!(
                &got[..=last_row],
                &want[..=last_row],
                "wrapped rows diverge at width {width} for case {case:?}"
            );
        }
    }
}

/// Micro-benchmark for the transcript hot path: 200 frames over a 128-message
/// transcript of 4 KiB each, with history expanded so every message takes part.
/// Run with `cargo test -p xcb-tui --test render -- --ignored --nocapture
/// transcript_frame_benchmark`; the elapsed times print to stdout.
#[test]
#[ignore]
fn transcript_frame_benchmark() {
    let mut app = app();
    app.show_history = true;
    app.view.pane = Pane::focus();
    let mut body = String::new();
    let mut word = 0;
    while body.len() < 4096 - 16 {
        body.push_str(&format!("word{word} "));
        word += 1;
        if word % 12 == 0 {
            body.push('\n');
        }
    }
    assert!(body.len() >= 4000 && body.len() <= 4096);
    app.view.messages = (0..128)
        .map(|index| Message {
            id: Id::new(format!("m{index}")).unwrap(),
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            text: body.clone(),
            attachments: vec![],
            at_ms: index as u64,
            provenance: None,
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    // Warm-up frame so caches (if any) are populated before timing steady state.
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    let started = std::time::Instant::now();
    for tick in 0..200u64 {
        terminal
            .draw(|frame| render::draw(frame, &mut app, tick))
            .unwrap();
    }
    let steady = started.elapsed();
    // A streaming tail changes every frame; only the tail should re-wrap.
    let started = std::time::Instant::now();
    for tick in 0..200u64 {
        app.stream.push_str("delta token ");
        terminal
            .draw(|frame| render::draw(frame, &mut app, tick))
            .unwrap();
    }
    let streaming = started.elapsed();
    println!(
        "transcript benchmark: 200 steady frames {:?} ({:?}/frame); 200 streaming frames {:?} ({:?}/frame)",
        steady,
        steady / 200,
        streaming,
        streaming / 200
    );
}
