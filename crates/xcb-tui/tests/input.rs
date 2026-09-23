use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::sync_channel;
use xcb_core::session::Attachment;
use xcb_tui::{
    App, Modal,
    composer::{Composer, ComposerAction},
};

#[test]
fn bracketed_paste_preserves_multiline_text_without_submitting() {
    let mut composer = Composer::default();
    assert!(matches!(
        composer.handle(Event::Paste("first\nsecond".into())),
        ComposerAction::None
    ));
    assert_eq!(composer.text(), "first\nsecond");
    let submit = composer.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(matches!(submit, ComposerAction::Submit(ref text) if text == "first\nsecond"));
}

#[test]
fn alt_enter_is_a_newline_and_unicode_editing_is_safe() {
    let mut composer = Composer::default();
    composer.handle(Event::Paste("λ東京".into()));
    composer.handle(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)));
    composer.handle(Event::Paste("next".into()));
    assert_eq!(composer.text(), "λ東京\nnext");
    composer.handle(Event::Key(KeyEvent::new(
        KeyCode::Backspace,
        KeyModifiers::NONE,
    )));
    assert_eq!(composer.text(), "λ東京\nnex");
}

#[test]
fn cancelling_never_submits_and_clipboard_has_its_own_action() {
    let mut composer = Composer::default();
    composer.handle(Event::Paste("do not send".into()));
    assert!(matches!(
        composer.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Cancel
    ));
    assert!(composer.text().is_empty());
    assert!(matches!(
        composer.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Clipboard
    ));
}

#[test]
fn help_and_tail_navigation_do_not_modify_the_draft() {
    let (tx, _rx) = sync_channel(1);
    let mut app = App::default();
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        &tx,
    );
    assert!(matches!(app.modal, Some(Modal::Help)));
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
        &tx,
    );
    assert!(
        !app.paused.get(),
        "the help modal must capture unrelated keys"
    );
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        &tx,
    );
    assert!(app.modal.is_none(), "? must close help as advertised");

    app.composer.set_text("draft stays");
    // With no rendered geometry yet, PageUp pins the viewport at line 0.
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
        &tx,
    );
    assert!(app.paused.get());
    assert_eq!(app.scroll.get(), 0);
    // A paused viewport resumes tail-following once PageDown passes the tail.
    app.scroll.set(30);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
        &tx,
    );
    assert!(!app.paused.get());
    assert_eq!(app.scroll.get(), 0);
    app.paused.set(true);
    app.scroll.set(30);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        &tx,
    );
    assert!(!app.paused.get());
    assert_eq!(app.scroll.get(), 0);
    assert_eq!(app.composer.text(), "draft stays");
}

#[test]
fn ctrl_c_cancels_the_run_even_while_a_dialog_is_open() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.view.state = xcb_core::session::State::Working;
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        &tx,
    );
    assert!(matches!(app.modal, Some(Modal::Help)));
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)),
        "Ctrl-C inside a dialog must reach the kernel as a cancel"
    );
    assert!(
        matches!(app.modal, Some(Modal::Help)),
        "the dialog stays open; Esc still closes it"
    );
    assert!(app.notice.contains("Stopping"));

    // The same holds for a picker dialog.
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx,
    );
    assert!(app.modal.is_none());
    app.composer.set_text("/sessions");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx,
    );
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
}

#[test]
fn a_rejected_submission_restores_text_and_attachments() {
    let mut app = App::default();
    let image = Attachment {
        digest: "b".repeat(64),
        media_type: "image/png".into(),
        bytes: 1024,
        width: 320,
        height: 200,
    };
    assert!(app.apply(xcb_core::ui::Update::Draft {
        text: "rejected draft".into(),
        attachments: vec![image.clone()],
    }));
    assert_eq!(app.composer.text(), "rejected draft");
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.attachments[0].digest, image.digest);

    // A draft the user typed meanwhile is never clobbered, and attachments
    // merge without duplicating a digest.
    app.composer.set_text("newer draft");
    assert!(app.apply(xcb_core::ui::Update::Draft {
        text: "older rejected".into(),
        attachments: vec![image.clone()],
    }));
    assert_eq!(app.composer.text(), "newer draft");
    assert_eq!(app.attachments.len(), 1);
}

#[test]
fn removing_an_attachment_retains_the_prompt() {
    let (tx, _rx) = sync_channel(1);
    let mut app = App::default();
    app.composer.set_text("keep this prompt");
    app.attachments.push(Attachment {
        digest: "a".repeat(64),
        media_type: "image/png".into(),
        bytes: 2048,
        width: 640,
        height: 480,
    });

    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
        &tx,
    );

    assert!(app.attachments.is_empty());
    assert_eq!(app.composer.text(), "keep this prompt");
}

fn view_for(session: &str) -> xcb_core::ui::View {
    xcb_core::ui::View {
        session: Some(xcb_core::session::Session {
            id: xcb_core::Id::new(session).unwrap(),
            account: xcb_core::Id::new("personal").unwrap(),
            model: xcb_core::models::ModelChoice {
                provider: xcb_core::Provider::Devin,
                id: xcb_core::Id::new("gpt-6-astra-max").unwrap(),
                label: "Astra Max".into(),
                mode: xcb_core::models::Mode::Fixed,
                resolved: None,
                effort: None,
                observed_at_ms: 1,
            },
            workspace: "/project".into(),
            title: format!("Session {session}"),
            pane: xcb_core::Id::new("focus").unwrap(),
            state: xcb_core::session::State::Idle,
            revision: 1,
            created_at_ms: 1,
            last_active_at_ms: 2,
        }),
        ..Default::default()
    }
}

#[test]
fn initial_context_preserves_a_partially_typed_quit_command() {
    let managed = xcb_core::ui::View {
        conversation: Some(xcb_core::Id::new("c_resumed").unwrap()),
        extensions: vec![("algal supervisor".into(), "on".into())],
        ..Default::default()
    };
    for view in [view_for("s_resumed"), managed] {
        let mut app = App::default();
        let (tx, rx) = sync_channel(4);
        for character in "/q".chars() {
            assert!(app.handle(
                Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                &tx,
            ));
        }
        assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
        for character in "uit".chars() {
            assert!(app.handle(
                Event::Key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                &tx,
            ));
        }
        assert!(!app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        ));
        assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));
        assert!(rx.try_recv().is_err(), "no partial command becomes a task");
    }
}

#[test]
fn initial_context_preserves_early_text_and_attachment_submission() {
    let managed = xcb_core::ui::View {
        conversation: Some(xcb_core::Id::new("c_new").unwrap()),
        extensions: vec![("algal supervisor".into(), "on".into())],
        ..Default::default()
    };
    for view in [view_for("s_new"), managed] {
        let mut app = App::default();
        let (tx, rx) = sync_channel(4);
        assert!(app.handle(Event::Paste("Review this image".into()), &tx));
        let image = Attachment {
            digest: "a".repeat(64),
            media_type: "image/png".into(),
            bytes: 1024,
            width: 32,
            height: 32,
        };
        assert!(app.apply(xcb_core::ui::Update::Attachment(image.clone())));
        assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
        assert_eq!(app.composer.text(), "Review this image");
        assert_eq!(app.attachments.len(), 1);
        assert!(app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx,
        ));
        match rx.try_recv().unwrap() {
            xcb_core::ui::Intent::Submit {
                text, attachments, ..
            } => {
                assert_eq!(text, "Review this image");
                assert_eq!(attachments.len(), 1);
                assert_eq!(attachments[0].digest, image.digest);
            }
            _ => panic!("the complete original input must be submitted"),
        }
    }
}

#[test]
fn drafts_and_attachments_are_scoped_per_session() {
    let mut app = App::default();
    let image = |digest: &str| Attachment {
        digest: digest.repeat(8),
        media_type: "image/png".into(),
        bytes: 1024,
        width: 320,
        height: 200,
    };
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_one")))));
    app.composer.set_text("draft for one");
    app.attachments.push(image("aaaaaaa1"));

    // Switching sessions must not carry the draft or its images across.
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_two")))));
    assert_eq!(app.composer.text(), "");
    assert!(app.attachments.is_empty());
    app.composer.set_text("draft for two");
    app.attachments.push(image("bbbbbbb2"));

    // Each session gets its own draft back, attachments included.
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_one")))));
    assert_eq!(app.composer.text(), "draft for one");
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.attachments[0].digest, "aaaaaaa1".repeat(8));
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_two")))));
    assert_eq!(app.composer.text(), "draft for two");
    assert_eq!(app.attachments[0].digest, "bbbbbbb2".repeat(8));
}

#[test]
fn drafts_are_scoped_across_concurrent_control_conversations() {
    let mut app = App::default();
    let view = |id: &str| xcb_core::ui::View {
        conversation: Some(xcb_core::Id::new(id).unwrap()),
        conversations: vec![xcb_core::ui::ConversationRow {
            id: xcb_core::Id::new(id).unwrap(),
            title: id.into(),
            workspace: "/project".into(),
            updated_at_ms: 1,
        }],
        extensions: vec![("algal supervisor".into(), "on".into())],
        ..Default::default()
    };
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view("c_one")))));
    app.composer.set_text("first chat draft");
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view("c_two")))));
    assert_eq!(app.composer.text(), "");
    app.composer.set_text("second chat draft");
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view("c_one")))));
    assert_eq!(app.composer.text(), "first chat draft");
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view("c_two")))));
    assert_eq!(app.composer.text(), "second chat draft");
}

#[test]
fn an_attachment_in_flight_lands_in_the_session_that_requested_it() {
    let mut app = App::default();
    let (tx, _rx) = sync_channel(1);
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_one")))));
    // /attach sends the intent and records which session owns the image.
    app.composer.set_text("/attach /tmp/pic.png");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx,
    );
    let image = Attachment {
        digest: "c".repeat(64),
        media_type: "image/png".into(),
        bytes: 512,
        width: 16,
        height: 16,
    };
    // The user switches sessions before the image arrives.
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_two")))));
    assert!(app.apply(xcb_core::ui::Update::Attachment(image.clone())));
    assert!(
        app.attachments.is_empty(),
        "the image must not leak into the new session"
    );
    // Switching back restores the draft the image belongs to.
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_one")))));
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.attachments[0].digest, image.digest);
}

#[test]
fn unchanged_views_and_foreign_deltas_do_not_mark_a_repaint() {
    let mut app = App::default();
    assert!(!app.take_dirty(), "nothing drawn yet, nothing to repaint");

    let view = view_for("s_one");
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view.clone()))));
    assert!(app.take_dirty(), "a new snapshot must repaint");
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
    assert!(
        !app.take_dirty(),
        "an identical refresh must not trigger a rebuild"
    );

    // Stream text for a session that is not focused changes nothing on screen.
    assert!(app.apply(xcb_core::ui::Update::Delta {
        session: xcb_core::Id::new("s_elsewhere").unwrap(),
        thinking: false,
        text: "ignored".into(),
    }));
    assert!(!app.take_dirty());
    assert!(app.apply(xcb_core::ui::Update::Delta {
        session: xcb_core::Id::new("s_one").unwrap(),
        thinking: false,
        text: "kept".into(),
    }));
    assert!(app.take_dirty(), "a focused delta must repaint");
    assert_eq!(app.stream, "kept");

    // A changed snapshot repaints again.
    let mut changed = view_for("s_one");
    changed.state = xcb_core::session::State::Working;
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(changed))));
    assert!(app.take_dirty());

    // Pointer motion, drags, and focus notifications cannot change the view —
    // they must not schedule a repaint.
    let (tx, _rx) = std::sync::mpsc::sync_channel(4);
    for event in [
        Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 4,
            row: 4,
            modifiers: KeyModifiers::NONE,
        }),
        Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        }),
        Event::FocusGained,
        Event::FocusLost,
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            crossterm::event::KeyEventKind::Release,
        )),
    ] {
        let label = format!("{event:?}");
        assert!(app.handle(event, &tx), "event must not quit");
        assert!(!app.take_dirty(), "{label} must not mark a repaint");
    }
    // A wheel turn scrolls the transcript and a key press types — both paint.
    assert!(app.handle(
        Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }),
        &tx,
    ));
    assert!(app.take_dirty(), "a wheel scroll repaints the viewport");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        &tx,
    ));
    assert!(app.take_dirty(), "a key press repaints");
}

fn picker_account(
    id: &str,
    provider: xcb_core::Provider,
    enabled: bool,
) -> xcb_core::ui::AccountRow {
    xcb_core::ui::AccountRow {
        id: xcb_core::Id::new(id).unwrap(),
        provider,
        name: id.into(),
        email: None,
        subscription: "subscription".into(),
        remaining_percent: None,
        resets_at_ms: None,
        quota_blocked_until_ms: None,
        authentication_required: false,
        runway: xcb_core::usage::Estimate::Unknown {
            reason: "unknown".into(),
        },
        busy: false,
        enabled,
    }
}

fn picker_key(
    app: &mut App,
    tx: &std::sync::mpsc::SyncSender<xcb_core::ui::Intent>,
    code: KeyCode,
) {
    assert!(app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), tx));
}

#[test]
fn account_picker_labels_disabled_rows_and_only_submits_enabled_accounts() {
    for provider in [
        xcb_core::Provider::Claude,
        xcb_core::Provider::Codex,
        xcb_core::Provider::Devin,
    ] {
        let (tx, rx) = sync_channel(2);
        let mut app = App::default();
        app.view.accounts = vec![
            picker_account("disabled", provider, false),
            picker_account("enabled", provider, true),
        ];
        app.composer.set_text("/accounts");
        picker_key(&mut app, &tx, KeyCode::Enter);
        let Some(Modal::Picker { items, .. }) = &app.modal else {
            panic!("account picker")
        };
        assert!(items[0].label.ends_with(" · disabled"));
        assert!(!items[1].label.contains(" · disabled"));
        app.composer.set_text("keep my draft");
        picker_key(&mut app, &tx, KeyCode::Enter);
        assert!(
            rx.try_recv().is_err(),
            "disabled account must not reach the kernel"
        );
        assert!(matches!(app.modal, Some(Modal::Picker { .. })));
        assert!(app.notice.contains("disabled"));
        assert_eq!(app.composer.text(), "keep my draft");
        picker_key(&mut app, &tx, KeyCode::Down);
        picker_key(&mut app, &tx, KeyCode::Enter);
        assert!(
            matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Account(id)) if id.as_str() == "enabled")
        );
        assert!(app.modal.is_none());
        assert_eq!(app.composer.text(), "keep my draft");
    }
}

#[test]
fn account_picker_rechecks_disabled_or_removed_accounts_after_refresh() {
    for removed in [false, true] {
        let (tx, rx) = sync_channel(2);
        let mut app = App::default();
        app.view.accounts = vec![picker_account("selected", xcb_core::Provider::Claude, true)];
        app.composer.set_text("/accounts");
        picker_key(&mut app, &tx, KeyCode::Enter);
        let mut refreshed = app.view.clone();
        if removed {
            refreshed.accounts.clear();
        } else {
            refreshed.accounts[0].enabled = false;
        }
        assert!(app.apply(xcb_core::ui::Update::View(Box::new(refreshed))));
        picker_key(&mut app, &tx, KeyCode::Enter);
        assert!(
            rx.try_recv().is_err(),
            "stale account action must not reach the kernel"
        );
        assert!(matches!(app.modal, Some(Modal::Picker { .. })));
        assert!(app.notice.contains(if removed {
            "no longer available"
        } else {
            "disabled"
        }));
    }
}

#[test]
fn remote_turn_cancellation_explains_ownership_in_composer_and_dialogs() {
    for dialog in [false, true] {
        let (tx, rx) = sync_channel(4);
        let mut app = App::default();
        app.view.remote_active = true;
        app.view.state = xcb_core::session::State::Working;
        if dialog {
            app.modal = Some(Modal::Help);
        }
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            &tx,
        );
        assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
        assert_eq!(
            app.notice,
            "This turn is running in another terminal; cancel it there."
        );
        assert_eq!(app.modal.is_some(), dialog);
    }
}

#[test]
fn known_quota_block_repaints_and_labels_account_even_when_usage_is_stale() {
    let (tx, _rx) = sync_channel(2);
    let mut app = App::default();
    let mut view = xcb_core::ui::View {
        accounts: vec![picker_account("limited", xcb_core::Provider::Claude, true)],
        ..xcb_core::ui::View::default()
    };
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view.clone()))));
    assert!(app.take_dirty());
    view.accounts[0].quota_blocked_until_ms = Some(u64::MAX);
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view.clone()))));
    assert!(
        app.take_dirty(),
        "known exhaustion must repaint without fresh usage"
    );
    app.composer.set_text("/accounts");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let Some(Modal::Picker { items, .. }) = &app.modal else {
        panic!("account picker")
    };
    assert!(items[0].label.contains("quota limited"));
    assert!(items[0].label.contains("retry in"));
    assert!(!items[0].label.contains("quota unknown"));
    app.take_dirty();
    view.accounts[0].quota_blocked_until_ms = None;
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
    assert!(
        app.take_dirty(),
        "reset expiration must repaint without a usage sample"
    );
}

#[test]
fn slash_typeahead_lists_navigates_and_runs_commands() {
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();

    // A bare "/" lists the whole registry; a prefix narrows it.
    app.composer.set_text("/");
    let (matches, selected) = app.slash_menu().expect("menu for bare /");
    assert_eq!(selected, 0);
    assert_eq!(matches.len(), xcb_tui::SLASH_COMMANDS.len());
    app.composer.set_text("/se");
    let (matches, _) = app.slash_menu().unwrap();
    assert_eq!(matches[0].name, "/sessions");

    // Enter runs the highlighted argument-free command through the real
    // dispatch path: /sessions opens the session picker.
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(app.modal, Some(Modal::Picker { .. })),
        "sessions picker"
    );
    assert!(app.composer.text().is_empty());
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx,
    );
    assert!(app.modal.is_none());
    assert!(rx.try_recv().is_err());

    // Arrow keys and Ctrl-N/P wrap through the matches.
    app.composer.set_text("/");
    picker_key(&mut app, &tx, KeyCode::Up);
    let (matches, selected) = app.slash_menu().unwrap();
    assert_eq!(selected, matches.len() - 1);
    let count = matches.len();
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert_eq!(app.slash_menu().unwrap().1, 0);
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert_eq!(app.slash_menu().unwrap().1, count - 1);

    // Tab completes argument-requiring commands and closes the menu.
    app.composer.set_text("/at");
    picker_key(&mut app, &tx, KeyCode::Tab);
    assert_eq!(app.composer.text(), "/attach ");
    assert!(app.slash_menu().is_none());

    // Esc hides the menu without canceling the run; editing reopens it.
    app.composer.set_text("/re");
    assert!(app.slash_menu().is_some());
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(app.slash_menu().is_none());
    assert!(rx.try_recv().is_err());
    picker_key(&mut app, &tx, KeyCode::Char('l'));
    assert_eq!(app.composer.text(), "/rel");
    assert!(app.slash_menu().is_some());

    // Typing a space closes the menu; Enter then submits arguments normally.
    picker_key(&mut app, &tx, KeyCode::Char(' '));
    assert!(app.slash_menu().is_none());
}

#[test]
fn slash_typeahead_runs_quit_and_ignores_unknown_commands() {
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    app.composer.set_text("/qu");
    assert!(
        !app.handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &tx
        ),
        "/quit exits the event loop"
    );
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));

    let mut app = App::default();
    app.composer.set_text("/zzz");
    assert!(app.slash_menu().is_none());
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.notice.contains("Unknown command"));
    assert!(rx.try_recv().is_err());
}

fn catalog_model(
    provider: xcb_core::Provider,
    id: &str,
    effort: Option<&str>,
) -> xcb_core::models::ModelChoice {
    xcb_core::models::ModelChoice {
        provider,
        id: xcb_core::Id::new(id).unwrap(),
        label: id.into(),
        mode: xcb_core::models::Mode::Fixed,
        resolved: None,
        effort: effort.map(|value| xcb_core::Id::new(value).unwrap()),
        observed_at_ms: 1,
    }
}

#[test]
fn model_picker_filters_to_the_bound_sessions_provider() {
    let (tx, _rx) = sync_channel(4);
    let models = vec![
        catalog_model(xcb_core::Provider::Devin, "swe-2-high", None),
        catalog_model(xcb_core::Provider::Claude, "sonnet", Some("high")),
        catalog_model(xcb_core::Provider::Codex, "gpt-6-astra", Some("high")),
    ];

    // A bound session only lists its own provider's catalog.
    let mut app = App::default();
    let mut view = view_for("s_one");
    view.models = models.clone();
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
    app.composer.set_text("/model");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let Some(Modal::Picker { items, .. }) = &app.modal else {
        panic!("model picker")
    };
    assert_eq!(items.len(), 1);
    assert!(items[0].label.starts_with("devin"));

    // Without a session the whole observed catalog is offered.
    let mut app = App::default();
    let view = xcb_core::ui::View {
        models,
        ..xcb_core::ui::View::default()
    };
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
    app.composer.set_text("/model");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let Some(Modal::Picker { items, .. }) = &app.modal else {
        panic!("model picker")
    };
    assert_eq!(items.len(), 3);
}

#[test]
fn ctrl_c_cancels_a_live_turn_then_clears_a_draft_then_quits() {
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    let mut view = view_for("s_one");
    view.state = xcb_core::session::State::Working;
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));

    // While a turn runs, Ctrl-C cancels it and never touches the draft.
    app.composer.set_text("keep this draft");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert_eq!(app.composer.text(), "keep this draft");
    assert!(app.notice.contains("Stopping"));

    // Esc inside a dialog closes the dialog first; once closed, Esc stops the
    // live turn.
    app.modal = Some(Modal::Help);
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.modal.is_none());
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));

    // Idle with a draft: Ctrl-C clears it and warns once.
    let mut view = view_for("s_one");
    view.state = xcb_core::session::State::Idle;
    assert!(app.apply(xcb_core::ui::Update::View(Box::new(view))));
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(app.composer.text().is_empty());
    assert!(app.notice.contains("Ctrl-C again to quit"));
    assert!(rx.try_recv().is_err());

    // Idle and empty: Ctrl-C quits.
    assert!(!app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));

    // Idle Esc is a quiet no-op — no stale "stopping" notice, no intent.
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.notice.is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn single_letter_aliases_dispatch_the_full_command() {
    // /m opens the model picker; /m <query> quick-switches the model.
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("/m");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    app.modal = None;
    app.composer.set_text("/m sonnet");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Model(query)) if query == "sonnet"));

    // /a opens the account picker.
    app.view.accounts = vec![picker_account("only", xcb_core::Provider::Claude, true)];
    app.composer.set_text("/a");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    match &app.modal {
        Some(Modal::Picker { title, .. }) => assert!(title.contains("Accounts")),
        _ => panic!("account picker"),
    }
    app.modal = None;

    // /q quits.
    app.composer.set_text("/q");
    assert!(!app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));

    // /s opens the session picker.
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("/s");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    match &app.modal {
        Some(Modal::Picker { title, .. }) => assert_eq!(title, "Direct provider sessions"),
        _ => panic!("session picker"),
    }

    app.modal = None;
    app.view.tasks = vec![xcb_core::ui::TaskRow {
        id: xcb_core::Id::new("t_one").unwrap(),
        title: "Fix login".into(),
        state: xcb_core::session::State::Working,
        detail: "worker is running".into(),
        route: Some("claude/default/high".into()),
        workspace: "/project".into(),
        updated_at_ms: 1,
    }];
    app.composer.set_text("/t");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    match &app.modal {
        Some(Modal::Picker { title, .. }) => assert_eq!(title, "Managed tasks"),
        _ => panic!("task picker"),
    }
}

#[test]
fn global_command_menu_only_shows_conversation_and_task_controls() {
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.composer.set_text("/");
    let names: Vec<_> = app
        .slash_matches()
        .iter()
        .map(|command| command.name)
        .collect();
    assert_eq!(
        names,
        vec![
            "/attach",
            "/exit",
            "/help",
            "/new",
            "/quit",
            "/sessions",
            "/tasks",
        ]
    );
    assert!(!names.contains(&"/model"));
    assert!(!names.contains(&"/pane"));
}

#[test]
fn managed_session_picker_switches_control_conversations() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.conversation = Some(xcb_core::Id::new("c_first").unwrap());
    app.view.conversations = vec![
        xcb_core::ui::ConversationRow {
            id: xcb_core::Id::new("c_first").unwrap(),
            title: "First".into(),
            workspace: "/one".into(),
            updated_at_ms: 2,
        },
        xcb_core::ui::ConversationRow {
            id: xcb_core::Id::new("c_second").unwrap(),
            title: "Second".into(),
            workspace: "/two".into(),
            updated_at_ms: 1,
        },
    ];
    app.composer.set_text("/s");
    picker_key(&mut app, &tx, KeyCode::Enter);
    match &app.modal {
        Some(Modal::Picker { title, .. }) => assert_eq!(title, "Control conversations"),
        _ => panic!("conversation picker"),
    }
    picker_key(&mut app, &tx, KeyCode::Down);
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Conversation(id)) if id.as_str() == "c_second")
    );
}

#[test]
fn the_wheel_scrolls_the_transcript_never_the_composer() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("a draft the wheel must not touch");
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(app.handle(wheel(MouseEventKind::ScrollUp), &tx));
    assert!(app.paused.get(), "wheel up pauses the transcript");
    assert_eq!(app.composer.text(), "a draft the wheel must not touch");
    assert!(app.handle(wheel(MouseEventKind::ScrollDown), &tx));
    assert!(!app.paused.get(), "wheel back to the tail resumes follow");
    assert!(rx.try_recv().is_err());
}

#[test]
fn the_wheel_moves_picker_selection() {
    use crossterm::event::{MouseEvent, MouseEventKind};
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    app.view.accounts = (0..6)
        .map(|index| picker_account(&format!("acct{index}"), xcb_core::Provider::Claude, true))
        .collect();
    app.composer.set_text("/a");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(app.handle(wheel(MouseEventKind::ScrollDown), &tx));
    match &app.modal {
        Some(Modal::Picker { selected, .. }) => assert_eq!(*selected, 3),
        _ => panic!("picker open"),
    }
    assert!(app.handle(wheel(MouseEventKind::ScrollUp), &tx));
    match &app.modal {
        Some(Modal::Picker { selected, .. }) => assert_eq!(*selected, 0),
        _ => panic!("picker open"),
    }
}

#[test]
fn submitted_prompts_echo_instantly_then_reconcile_with_the_view() {
    use xcb_core::session::{Message, Role};
    use xcb_core::ui::Update;
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    let enter = || Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    app.composer.set_text("ship it");
    assert!(app.handle(enter(), &tx));
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Submit { text, .. }) if text == "ship it")
    );
    assert_eq!(app.pending_echoes().count(), 1, "echo is instant");

    // The kernel binds a session before the message lands — the echo follows.
    let mut view = view_for("s_one");
    assert!(app.apply(Update::View(Box::new(view.clone()))));
    assert_eq!(app.pending_echoes().count(), 1);

    // Once the persisted message arrives the echo reconciles — no duplicates.
    view.messages.push(Message {
        id: xcb_core::Id::new("m1").unwrap(),
        role: Role::User,
        text: "ship it".into(),
        at_ms: 1,
        attachments: vec![],
        provenance: None,
    });
    assert!(app.apply(Update::View(Box::new(view))));
    assert_eq!(app.pending_echoes().count(), 0);

    // A rejected submission retracts its echo when the draft returns.
    app.composer.set_text("nope");
    assert!(app.handle(enter(), &tx));
    assert!(rx.try_recv().is_ok());
    assert_eq!(app.pending_echoes().count(), 1);
    assert!(app.apply(Update::Draft {
        text: "nope".into(),
        attachments: vec![],
    }));
    assert_eq!(app.pending_echoes().count(), 0);
    assert_eq!(app.composer.text(), "nope");
}

#[test]
fn rejected_cancel_never_claims_that_cancellation_was_sent() {
    for modal in [false, true] {
        let (tx, rx) = sync_channel(1);
        tx.send(xcb_core::ui::Intent::Refresh).unwrap();
        let mut app = App::default();
        app.view.state = xcb_core::session::State::Working;
        if modal {
            app.modal = Some(Modal::Help);
        }
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            &tx,
        );
        assert_eq!(
            app.notice,
            "The command queue is full or closed. Nothing was submitted."
        );
        assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Refresh)));
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn rejected_attachment_does_not_block_the_next_prompt() {
    let (tx, rx) = sync_channel(1);
    tx.send(xcb_core::ui::Intent::Refresh).unwrap();
    let mut app = App::default();
    app.composer.set_text("/attach /tmp/image.png");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(app.notice.contains("Nothing was submitted"));
    rx.try_recv().unwrap();
    app.composer.set_text("Continue the task");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Submit { text, .. }) if text == "Continue the task")
    );
}

#[test]
fn managed_cancellation_reports_a_request_not_confirmed_settlement() {
    let (tx, rx) = sync_channel(1);
    let mut app = App::default();
    app.view.state = xcb_core::session::State::Working;
    app.view.managed_cancel_available = true;
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert_eq!(
        app.notice,
        "Cancellation requested for this conversation; check the task status for settlement."
    );
}

#[test]
fn work_in_another_conversation_does_not_intercept_ctrl_c() {
    let (tx, rx) = sync_channel(2);
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.state = xcb_core::session::State::Working;
    app.view.managed_cancel_available = false;
    app.composer.set_text("my draft");
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.composer.text().is_empty());
    assert!(rx.try_recv().is_err());
    assert!(!app.handle(ctrl_c(), &tx));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));
}

#[test]
fn managed_task_waiting_for_input_can_be_cancelled_without_losing_the_draft() {
    let (tx, rx) = sync_channel(2);
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.state = xcb_core::session::State::NeedsAnswer;
    app.view.managed_cancel_available = true;
    app.composer.set_text("keep this draft");
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert_eq!(app.composer.text(), "keep this draft");
    app.modal = Some(Modal::Help);
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert_eq!(app.composer.text(), "keep this draft");
}
