use super::*;
use ratatui::backend::TestBackend;
use std::sync::mpsc::sync_channel;
use xcb_core::ui::{AgentRow, ConversationRow};

fn fixture() -> App {
    let id = Id::new("overview-project").unwrap();
    let mut app = App::default();
    app.view.conversation = Some(id.clone());
    app.view.conversations.push(ConversationRow {
        id: id.clone(),
        title: "Project agent".into(),
        workspace: "/overview-project".into(),
        messages: 1,
        updated_at_ms: 1,
    });
    app.view.agents.push(AgentRow {
        context: TranscriptContext::Conversation(id),
        task: Some(Id::new("overview-task").unwrap()),
        title: "Project agent".into(),
        workspace: "/overview-project".into(),
        model: Some("example-model".into()),
        state: State::Working,
        activity: "thinking".into(),
        response: "Previous answer".into(),
        category: Some("completed".into()),
        updated_at_ms: 1,
    });
    app
}

#[test]
fn every_visible_background_agent_field_invalidates_the_view_fingerprint() {
    let original = fixture().view;
    let mutate: &[fn(&mut AgentRow)] = &[
        |row| row.context = TranscriptContext::Session(Id::new("other").unwrap()),
        |row| row.task = None,
        |row| row.title.push('x'),
        |row| row.workspace.push('x'),
        |row| row.model = Some("other-model".into()),
        |row| row.state = State::NeedsAnswer,
        |row| row.activity = "working".into(),
        |row| row.response = "A new answer".into(),
        |row| row.category = Some("question".into()),
        |row| row.updated_at_ms += 1,
    ];
    for change in mutate {
        let mut view = original.clone();
        change(&mut view.agents[0]);
        assert_ne!(fingerprint_at(&original, 0), fingerprint_at(&view, 0));
    }
    let mut app = fixture();
    app.view_fingerprint = fingerprint_at(&app.view, display_now_ms());
    let mut next = app.view.clone();
    next.agents[0].response = "Background worker finished".into();
    app.apply(Update::View(Box::new(next)));
    assert!(app.take_dirty());
}

#[test]
fn overview_is_available_in_managed_command_search() {
    let mut app = fixture();
    app.view
        .extensions
        .push(("algal supervisor".into(), "on".into()));
    app.composer.set_text("/over");
    assert!(
        app.slash_matches()
            .iter()
            .any(|command| command.name == "/overview")
    );
}

#[test]
fn modal_owns_grid_keys_and_clicks_without_dispatching_or_editing_the_draft() {
    let mut app = fixture();
    app.composer.set_text("Keep my existing draft");
    let (tx, rx) = sync_channel(8);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut app, 0))
        .unwrap();
    app.modal = Some(Modal::Inspect {
        title: "Pending approval".into(),
        lines: vec!["The overview cannot approve this operation".into()],
        scroll: 0,
    });
    for event in [
        Event::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 4,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    ] {
        app.handle(event, &tx);
    }
    assert_eq!(app.composer.text(), "Keep my existing draft");
    assert!(rx.try_recv().is_err());
}
