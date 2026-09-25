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
    assert!(matches!(app.modal, Some(Modal::Help { .. })));
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
    assert_eq!(
        app.composer.text(),
        "?",
        "a second ? types the character it could not send while help was open"
    );

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
    // Plain End belongs to the composer while a draft exists: it moves the
    // cursor to the end of the line and leaves the transcript untouched.
    app.composer
        .textarea
        .move_cursor(ratatui_textarea::CursorMove::Head);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        &tx,
    );
    assert!(app.paused.get());
    assert_eq!(app.scroll.get(), 30);
    assert_eq!(app.composer.textarea.cursor(), (0, "draft stays".len()));
    assert_eq!(app.composer.text(), "draft stays");
    // Shift-End (or Ctrl-End) resumes tail-following even with a draft.
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT)),
        &tx,
    );
    assert!(!app.paused.get());
    assert_eq!(app.scroll.get(), 0);
    // An empty composer leaves plain End to the transcript.
    app.paused.set(true);
    app.scroll.set(30);
    app.composer.set_text("");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        &tx,
    );
    assert!(!app.paused.get());
    assert_eq!(app.scroll.get(), 0);
}

#[test]
fn ctrl_c_closes_a_dialog_before_cancelling_a_live_turn() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.view.state = xcb_core::session::State::Working;
    app.modal = Some(Modal::Help { scroll: 0 });
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.modal.is_none());
    assert!(rx.try_recv().is_err(), "closing help must not cancel work");
    assert!(app.handle(ctrl_c(), &tx));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));

    app.composer.set_text("/sessions");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.modal.is_none());
    assert!(
        rx.try_recv().is_err(),
        "closing a picker must not cancel work"
    );
}

#[test]
fn ctrl_c_in_an_idle_dialog_closes_it_and_quits_on_a_second_press() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.modal = Some(Modal::Help { scroll: 0 });
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    // Idle: the first Ctrl-C closes the dialog instead of quitting, so an
    // editor or help screen never silently drops state on the way out.
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.modal.is_none());
    assert_eq!(app.notice, "Dialog closed. Press Ctrl-C again to quit.");
    assert!(rx.try_recv().is_err());

    assert!(!app.handle(ctrl_c(), &tx));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));
}

#[test]
fn ctrl_c_in_the_prompt_editor_returns_its_text_to_the_composer() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.modal = Some(Modal::Editor {
        title: "Prompt editor".into(),
        textarea: Box::new(ratatui_textarea::TextArea::from(vec![
            "edited in the dialog".to_owned(),
        ])),
        kind: xcb_tui::EditorKind::Prompt,
        error: None,
    });
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(app.modal.is_none());
    assert_eq!(app.composer.text(), "edited in the dialog");
    assert!(app.notice.contains("Ctrl-C again"));
    assert!(rx.try_recv().is_err());
}

#[test]
fn shift_tab_inserts_nothing_in_the_composer_or_the_prompt_editor() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("draft");
    let back_tab = || Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    app.handle(back_tab(), &tx);
    assert_eq!(app.composer.text(), "draft");
    app.modal = Some(Modal::Editor {
        title: "Prompt editor".into(),
        textarea: Box::new(ratatui_textarea::TextArea::from(vec![
            "edited in the dialog".to_owned(),
        ])),
        kind: xcb_tui::EditorKind::Prompt,
        error: None,
    });
    app.handle(back_tab(), &tx);
    let Some(Modal::Editor { textarea, .. }) = &app.modal else {
        panic!("the prompt editor stays open");
    };
    assert_eq!(textarea.lines(), ["edited in the dialog"]);
    assert!(rx.try_recv().is_err());
}

#[test]
fn an_oversized_paste_is_rejected_with_a_notice() {
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    let huge = "x".repeat(256 * 1024 + 1);
    assert!(app.handle(Event::Paste(huge), &tx));
    assert!(app.composer.text().is_empty());
    assert_eq!(
        app.notice,
        "Paste exceeds 256 KiB; attach a file or trim it"
    );

    // A paste that pushes an existing draft over the bound is refused whole.
    let mut composer = Composer::default();
    composer.handle(Event::Paste("seed".into()));
    let too_much = "y".repeat(256 * 1024);
    assert!(matches!(
        composer.handle(Event::Paste(too_much)),
        ComposerAction::Rejected(_)
    ));
    assert_eq!(composer.text(), "seed");
}

#[test]
fn ctrl_c_moves_the_draft_to_ctrl_r_history() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("precious draft");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(app.composer.text().is_empty());
    assert_eq!(
        app.notice,
        "Draft cleared (Ctrl-R restores). Press Ctrl-C again to quit."
    );
    assert!(rx.try_recv().is_err());

    // Ctrl-R opens the history picker with the cleared draft on top.
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        &tx
    ));
    match &app.modal {
        Some(Modal::HistorySearch { matches, query, .. }) => {
            assert!(query.is_empty());
            assert_eq!(matches[0], "precious draft");
        }
        _ => panic!("history search"),
    }
    // Enter restores it into the composer.
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert_eq!(app.composer.text(), "precious draft");
    assert!(app.modal.is_none());
}

#[test]
fn mouse_capture_is_off_until_slash_mouse_toggles_it() {
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    assert!(!app.mouse_capture);
    assert_eq!(app.take_mouse_toggle(), None);

    app.composer.set_text("/mouse");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.mouse_capture);
    assert_eq!(app.take_mouse_toggle(), Some(true));
    assert!(app.notice.contains("Mouse capture on"));
    assert_eq!(app.take_mouse_toggle(), None, "consumed once");

    app.composer.set_text("/mouse");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(!app.mouse_capture);
    assert_eq!(app.take_mouse_toggle(), Some(false));
    assert!(app.notice.contains("Mouse capture off"));
}

#[test]
fn the_next_keypress_dismisses_a_notice() {
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("/zzz");
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.notice.contains("Unknown command"));
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        &tx
    ));
    assert!(app.notice.is_empty());
    assert_eq!(app.composer.text(), "x");
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
fn alt_backspace_edits_words_without_removing_attachments() {
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
    assert_eq!(app.attachments.len(), 1);
    assert_eq!(app.composer.text(), "keep this ");
    app.composer.set_text("/detach all");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(app.attachments.is_empty());
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
            managed_task: None,
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
    use xcb_core::ui::TranscriptContext;
    let managed = xcb_core::ui::View {
        conversation: Some(xcb_core::Id::new("c_new").unwrap()),
        extensions: vec![("algal supervisor".into(), "on".into())],
        ..Default::default()
    };
    for view in [view_for("s_new"), managed] {
        let expected_context = view.conversation.clone().map_or_else(
            || TranscriptContext::Session(view.session.as_ref().unwrap().id.clone()),
            TranscriptContext::Conversation,
        );
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
            xcb_core::ui::Intent::SubmitTo {
                context,
                text,
                attachments,
                ..
            } => {
                assert_eq!(context, expected_context);
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
            messages: 0,
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
fn remote_turn_cancellation_explains_ownership_after_closing_dialogs() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.view.remote_active = true;
    app.view.state = xcb_core::session::State::Working;
    app.modal = Some(Modal::Help { scroll: 0 });
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    app.handle(ctrl_c(), &tx);
    assert!(app.modal.is_none());
    assert!(rx.try_recv().is_err());
    app.handle(ctrl_c(), &tx);
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert!(app.notice.contains("another terminal"));
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
    app.composer.set_text("/qui");
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
    assert!(app.slash_menu().is_some_and(|(items, _)| items.is_empty()));
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
fn ctrl_c_clears_a_draft_before_cancelling_a_live_turn_then_quits_when_idle() {
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    let mut view = view_for("s_one");
    view.state = xcb_core::session::State::Working;
    app.apply(xcb_core::ui::Update::View(Box::new(view)));
    app.composer.set_text("keep this draft");
    let ctrl_c = || Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.composer.text().is_empty());
    assert!(rx.try_recv().is_err());
    assert!(app.composer.history().any(|text| text == "keep this draft"));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));

    app.composer.set_text("another draft");
    app.modal = Some(Modal::Help { scroll: 0 });
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(app.modal.is_none());
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Cancel)));
    assert_eq!(
        app.composer.text(),
        "another draft",
        "Esc cancellation retains the draft"
    );

    app.apply(xcb_core::ui::Update::View(Box::new(view_for("s_one"))));
    assert!(app.handle(ctrl_c(), &tx));
    assert!(app.composer.text().is_empty());
    assert!(rx.try_recv().is_err());
    assert!(!app.handle(ctrl_c(), &tx));
    assert!(matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Quit)));

    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    picker_key(&mut app, &tx, KeyCode::Esc);
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
        revision: 1,
        title: "Fix login".into(),
        state: xcb_core::session::State::Working,
        status: Some("running".into()),
        detail: "worker is running".into(),
        route: Some("claude/default/high".into()),
        route_reason: None,
        settle: None,
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
    for command in [
        "/agents",
        "/attention",
        "/backlog",
        "/cancel",
        "/detach",
        "/editor",
        "/history",
        "/queue",
        "/rename",
        "/reply",
        "/steer",
        "/tasks",
    ] {
        assert!(
            names.contains(&command),
            "missing managed command {command}"
        );
    }
    assert!(!names.contains(&"/model"));
    assert!(!names.contains(&"/pane"));
}

#[test]
fn exact_command_aliases_win_over_new_command_prefixes() {
    let mut app = App::default();
    for (alias, command) in [
        ("/a", "/accounts"),
        ("/s", "/sessions"),
        ("/r", "/reload"),
        ("/b", "/backlog"),
    ] {
        app.composer.set_text(alias);
        let matches = app.slash_matches();
        assert_eq!(
            matches.len(),
            1,
            "{alias} must have one unambiguous meaning"
        );
        assert_eq!(matches[0].name, command);
    }
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.composer.set_text("/s");
    assert_eq!(app.slash_matches()[0].name, "/sessions");
    app.composer.set_text("/at");
    assert_eq!(app.slash_matches()[0].name, "/attach");
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
            messages: 0,
            updated_at_ms: 2,
        },
        xcb_core::ui::ConversationRow {
            id: xcb_core::Id::new("c_second").unwrap(),
            title: "Second".into(),
            workspace: "/two".into(),
            messages: 0,
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
    picker_key(&mut app, &tx, KeyCode::Down);
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Conversation(id)) if id.as_str() == "c_second")
    );
}

#[test]
fn managed_session_picker_offers_a_new_conversation() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.conversations = vec![xcb_core::ui::ConversationRow {
        id: xcb_core::Id::new("c_first").unwrap(),
        title: "First".into(),
        workspace: "/one".into(),
        messages: 3,
        updated_at_ms: 2,
    }];
    app.composer.set_text("/s");
    picker_key(&mut app, &tx, KeyCode::Enter);
    match &app.modal {
        Some(Modal::Picker { items, .. }) => {
            assert_eq!(items[0].label, "＋ new conversation");
            assert!(items[1].label.contains("First · 3 msgs"));
        }
        _ => panic!("conversation picker"),
    }
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(matches!(
        rx.try_recv(),
        Ok(xcb_core::ui::Intent::NewSession)
    ));
}

#[test]
fn task_inspect_opens_a_scrollable_modal_with_the_full_route() {
    let (tx, _rx) = sync_channel(4);
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.tasks = vec![xcb_core::ui::TaskRow {
        id: xcb_core::Id::new("t_one").unwrap(),
        revision: 1,
        title: "Fix login".into(),
        state: xcb_core::session::State::Working,
        status: Some("running".into()),
        detail: "worker is running · quota leader".into(),
        route: Some("devin/swe-2-high · a_01234567".into()),
        route_reason: Some("learned workspace preference for devin".into()),
        settle: None,
        workspace: "/project".into(),
        updated_at_ms: display_now_ms_minus(60_000),
    }];
    app.composer.set_text("/tasks");
    picker_key(&mut app, &tx, KeyCode::Enter);
    // The picker label itself carries the wire phase and the route.
    match &app.modal {
        Some(Modal::Picker { items, .. }) => {
            assert!(items[0].label.contains("running"), "{}", items[0].label);
            assert!(
                items[0].label.contains("devin/swe-2-high · a_01234567"),
                "{}",
                items[0].label
            );
        }
        _ => panic!("task picker"),
    }
    picker_key(&mut app, &tx, KeyCode::Enter);
    let (lines, scroll) = match &mut app.modal {
        Some(Modal::Inspect {
            title,
            lines,
            scroll,
        }) => {
            assert!(title.contains("t_one"));
            (lines.clone(), scroll)
        }
        _ => panic!("task inspect modal"),
    };
    let body = lines.join("\n");
    assert!(body.contains("Fix login"));
    assert!(body.contains("running"));
    assert!(body.contains("devin/swe-2-high · a_01234567"));
    assert!(body.contains("learned workspace preference for devin"));
    assert!(body.contains("/project"));
    assert!(body.contains("worker is running · quota leader"));
    // A one-line notice would have dropped all of this; nothing was posted.
    assert!(app.notice.is_empty());
    assert_eq!(*scroll, 0);
    picker_key(&mut app, &tx, KeyCode::End);
    match &app.modal {
        Some(Modal::Inspect { scroll, .. }) => assert_eq!(*scroll, u16::MAX),
        _ => panic!("inspect modal"),
    }
    picker_key(&mut app, &tx, KeyCode::Home);
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(app.modal.is_none());
}

fn display_now_ms_minus(ms: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
        .saturating_sub(ms)
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
    let submitted_id = match rx.try_recv().unwrap() {
        xcb_core::ui::Intent::Submit { id, text, .. } => {
            assert_eq!(text, "ship it");
            id
        }
        _ => panic!("submitted prompt identity"),
    };
    assert_eq!(app.pending_echoes().count(), 1, "echo is instant");

    // The kernel binds a session before the message lands — the echo follows.
    let mut view = view_for("s_one");
    assert!(app.apply(Update::View(Box::new(view.clone()))));
    assert_eq!(app.pending_echoes().count(), 1);

    // Once the persisted message arrives the echo reconciles — no duplicates.
    view.messages.push(Message {
        id: submitted_id,
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
            app.modal = Some(Modal::Help { scroll: 0 });
            app.handle(
                Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                &tx,
            );
            assert!(app.modal.is_none());
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
fn managed_cancellation_reports_an_exact_task_request_not_settlement() {
    use xcb_core::ui::{HabitatCommand, Intent};
    let (tx, rx) = sync_channel(1);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::CancelTask { id, expected_revision: 7 })) if id.as_str() == "task_one")
    );
    assert!(app.notice.contains("Cancellation requested"));
    assert!(app.notice.contains("held"));
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
    use xcb_core::ui::{HabitatCommand, Intent};
    let (tx, rx) = sync_channel(2);
    let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
    app.composer.set_text("keep this draft");
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::CancelTask { id, expected_revision: 7 })) if id.as_str() == "task_one")
    );
    assert_eq!(app.composer.text(), "keep this draft");
    app.modal = Some(Modal::Help { scroll: 0 });
    assert!(app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &tx
    ));
    assert!(app.modal.is_none());
    assert!(rx.try_recv().is_err());
    assert_eq!(app.composer.text(), "keep this draft");
}

fn managed_fixture(state: xcb_core::session::State) -> App {
    use xcb_core::{Id, ui::BacklogRow};
    let mut app = App::default();
    app.view.extensions = vec![("algal supervisor".into(), "on".into())];
    app.view.conversation = Some(Id::new("conversation_one").unwrap());
    app.view.state = state;
    app.view.managed_cancel_available = true;
    app.view.backlog = vec![BacklogRow {
        id: Id::new("task_one").unwrap(),
        conversation: Id::new("conversation_one").unwrap(),
        title: "First task".into(),
        prompt: "Original task".into(),
        summary: "Current question".into(),
        status: if state == xcb_core::session::State::NeedsAnswer {
            "needs input"
        } else {
            "running"
        }
        .into(),
        state,
        deferred: false,
        priority: 5,
        revision: 7,
        updated_at_ms: 1,
    }];
    app
}

#[test]
fn reverse_history_search_matches_the_full_prompt_and_escape_restores_the_draft() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    let full = format!(
        "{}\nneedle in the second paragraph\nlast line",
        "opening ".repeat(90)
    );
    app.composer.remember(&full);
    app.composer.remember("a newer unrelated prompt");
    app.composer.set_text("unfinished draft");
    let ctrl_r = || Event::Key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    app.handle(ctrl_r(), &tx);
    app.handle(Event::Paste("needle".into()), &tx);
    assert!(
        matches!(&app.modal, Some(Modal::HistorySearch { matches, query, .. }) if query == "needle" && matches == &vec![full.clone()])
    );
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert_eq!(app.composer.text(), "unfinished draft");
    app.handle(ctrl_r(), &tx);
    app.handle(Event::Paste("needle".into()), &tx);
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert_eq!(app.composer.text(), full);
    assert!(app.modal.is_none());
    assert!(
        rx.try_recv().is_err(),
        "accepting history must never submit it"
    );
}

#[test]
fn readline_editing_keys_do_not_open_panels_or_toggle_tools() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("one1\ntwo2");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert_eq!(app.composer.textarea.cursor(), (0, 4));
    assert!(app.modal.is_none());
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert_eq!(app.composer.text(), "\ntwo2");
    assert!(!app.show_activity);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert_eq!(app.composer.text(), "one1\ntwo2");
    picker_key(&mut app, &tx, KeyCode::F(4));
    assert!(app.show_activity);
    assert!(rx.try_recv().is_err());
}

#[test]
fn unknown_slash_prefix_navigation_is_safe_and_picker_paste_is_a_filter() {
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.composer.set_text("/thereisnosuchcommand");
    assert!(app.slash_menu().is_some_and(|(items, _)| items.is_empty()));
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Tab] {
        picker_key(&mut app, &tx, code);
    }
    assert_eq!(app.composer.text(), "/thereisnosuchcommand");
    app.composer.set_text("/accounts");
    app.view.accounts = vec![picker_account("personal", xcb_core::Provider::Claude, true)];
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.handle(Event::Paste("person\u{1b}".into()), &tx);
    assert!(matches!(&app.modal, Some(Modal::Picker { query, .. }) if query == "person"));
    assert!(rx.try_recv().is_err());
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(xcb_core::ui::Intent::Account(id)) if id.as_str() == "personal")
    );
}

#[test]
fn selected_target_guidance_and_tab_queue_have_distinct_intents() {
    use xcb_core::ui::{HabitatCommand, Intent};
    let (tx, rx) = sync_channel(8);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    app.composer.set_text("/steer task_one");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("Guide task_one"))
    );
    app.composer.set_text("Focus on the regression");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::Steer { task, text, .. })) if task.as_str() == "task_one" && text == "Focus on the regression")
    );
    app.composer.set_text("Start independent work");
    picker_key(&mut app, &tx, KeyCode::Tab);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::EnqueueIn { conversation, prompt, deferred: false, .. })) if conversation.as_str() == "conversation_one" && prompt == "Start independent work")
    );
    assert!(app.composer.text().is_empty());
    assert!(rx.try_recv().is_err());
}

#[test]
fn answer_target_retains_the_observed_question_revision_and_rejected_draft() {
    use xcb_core::ui::{HabitatCommand, Intent, Update};
    let (tx, rx) = sync_channel(8);
    let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
    app.composer.set_text("/attention");
    picker_key(&mut app, &tx, KeyCode::Enter);
    picker_key(&mut app, &tx, KeyCode::Enter);
    picker_key(&mut app, &tx, KeyCode::Char('a'));
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("Answer task_one"))
    );
    // A concurrent refresh changes the question after the user chose it.
    app.view.backlog[0].revision = 8;
    app.view.backlog[0].summary = "Replacement question".into();
    app.composer.set_text("Answer to the original question");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let operation = match rx.try_recv().unwrap() {
        Intent::Habitat(HabitatCommand::Reply {
            id,
            expected_revision,
            reply,
            text,
        }) => {
            assert_eq!(id.as_str(), "task_one");
            assert_eq!(expected_revision, 7);
            assert_eq!(text, "Answer to the original question");
            reply
        }
        _ => panic!("reply must carry the selected question revision"),
    };
    app.take_dirty();
    app.apply(Update::HabitatDraft {
        context: xcb_core::Id::new("conversation_one").unwrap(),
        task: Some(xcb_core::Id::new("task_one").unwrap()),
        operation,
        text: "Answer to the original question".into(),
    });
    assert!(
        app.take_dirty(),
        "rejected answer must repaint the restored draft"
    );
    assert_eq!(app.composer.text(), "Answer to the original question");
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("task_one"))
    );
}

#[test]
fn accepted_answer_returns_to_guidance_and_the_next_input_steers_the_same_task() {
    use xcb_core::ui::{HabitatCommand, Intent, Update};
    let (tx, rx) = sync_channel(8);
    let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
    select_answer_target(&mut app, &tx, "task_one");
    let operation = submit_selected_answer(&mut app, &tx, &rx, "task_one", 7, "Use option A");
    assert!(app.composer.text().is_empty());

    for (ack_operation, ack_task) in [
        (
            xcb_core::Id::new("unmatched_operation").unwrap(),
            "task_one",
        ),
        (operation.clone(), "different_task"),
    ] {
        app.apply(Update::HabitatAccepted {
            context: xcb_core::Id::new("conversation_one").unwrap(),
            task: Some(xcb_core::Id::new(ack_task).unwrap()),
            operation: ack_operation,
            text: "Use option A".into(),
        });
        assert!(
            app.composer_target_label()
                .is_some_and(|label| label.starts_with("Answer task_one")),
            "an unmatched acknowledgement must not change the answer target"
        );
    }

    app.take_dirty();
    app.apply(Update::HabitatAccepted {
        context: xcb_core::Id::new("conversation_one").unwrap(),
        task: Some(xcb_core::Id::new("task_one").unwrap()),
        operation,
        text: "Use option A".into(),
    });
    assert!(
        app.take_dirty(),
        "accepted answer must repaint its new target"
    );
    assert!(app.composer.text().is_empty());
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.starts_with("Guide task_one"))
    );
    assert!(
        rx.try_recv().is_err(),
        "acknowledgement must not send input"
    );
    app.composer.set_text("Also check the regression");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::Steer { task, text, .. })) if task.as_str() == "task_one" && text == "Also check the regression")
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn accepted_answer_preserves_a_newer_draft_and_its_answer_revision() {
    use xcb_core::ui::Update;
    let (tx, rx) = sync_channel(8);
    let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
    select_answer_target(&mut app, &tx, "task_one");
    let operation = submit_selected_answer(&mut app, &tx, &rx, "task_one", 7, "First answer");
    app.handle(Event::Paste("Newer answer draft".into()), &tx);
    app.take_dirty();
    app.apply(Update::HabitatAccepted {
        context: xcb_core::Id::new("conversation_one").unwrap(),
        task: Some(xcb_core::Id::new("task_one").unwrap()),
        operation,
        text: "First answer".into(),
    });
    assert!(app.take_dirty(), "accepted answer notice must repaint");
    assert_eq!(app.composer.text(), "Newer answer draft");
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.starts_with("Answer task_one"))
    );
    assert!(rx.try_recv().is_err());
    submit_selected_answer(&mut app, &tx, &rx, "task_one", 7, "Newer answer draft");
    assert!(rx.try_recv().is_err());
}

#[test]
fn accepted_answer_preserves_a_newer_answer_selection_or_displayed_context() {
    use xcb_core::{Id, ui::Update};
    for scenario in ["new_question", "new_task", "other_conversation"] {
        let (tx, rx) = sync_channel(8);
        let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
        add_second_task(&mut app);
        select_answer_target(&mut app, &tx, "task_one");
        let operation = submit_selected_answer(&mut app, &tx, &rx, "task_one", 7, "First answer");
        let (next_task, next_revision) = match scenario {
            "new_question" => {
                app.view.backlog[0].revision = 8;
                app.view.backlog[0].summary = "New question".into();
                ("task_one", 8)
            }
            "new_task" => ("task_two", 11),
            _ => {
                let mut next_view = app.view.clone();
                next_view.conversation = Some(Id::new("conversation_two").unwrap());
                app.apply(Update::View(Box::new(next_view)));
                ("task_one", 7)
            }
        };
        select_answer_target(&mut app, &tx, next_task);
        assert!(app.composer.text().is_empty());
        app.take_dirty();
        app.apply(Update::HabitatAccepted {
            context: Id::new("conversation_one").unwrap(),
            task: Some(Id::new("task_one").unwrap()),
            operation,
            text: "First answer".into(),
        });
        assert!(app.take_dirty(), "accepted answer notice must repaint");
        assert!(
            app.composer_target_label()
                .is_some_and(|label| label.starts_with(&format!("Answer {next_task}"))),
            "accepted old answer must preserve {scenario} selection"
        );
        assert!(app.composer.text().is_empty());
        assert!(rx.try_recv().is_err());
        submit_selected_answer(&mut app, &tx, &rx, next_task, next_revision, "Next answer");
        assert!(rx.try_recv().is_err());
    }
}

fn select_answer_target(
    app: &mut App,
    tx: &std::sync::mpsc::SyncSender<xcb_core::ui::Intent>,
    task: &str,
) {
    app.composer.set_text("/attention");
    picker_key(app, tx, KeyCode::Enter);
    app.handle(Event::Paste(task.into()), tx);
    picker_key(app, tx, KeyCode::Enter);
    picker_key(app, tx, KeyCode::Char('a'));
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.starts_with(&format!("Answer {task}")))
    );
}

fn submit_selected_answer(
    app: &mut App,
    tx: &std::sync::mpsc::SyncSender<xcb_core::ui::Intent>,
    rx: &std::sync::mpsc::Receiver<xcb_core::ui::Intent>,
    task: &str,
    revision: u64,
    answer: &str,
) -> xcb_core::Id {
    use xcb_core::ui::{HabitatCommand, Intent};
    app.composer.set_text(answer);
    picker_key(app, tx, KeyCode::Enter);
    match rx.try_recv().unwrap() {
        Intent::Habitat(HabitatCommand::Reply {
            id,
            expected_revision,
            reply,
            text,
        }) => {
            assert_eq!(id.as_str(), task);
            assert_eq!(expected_revision, revision);
            assert_eq!(text, answer);
            reply
        }
        _ => panic!("expected an answer for {task}"),
    }
}

#[test]
fn ambiguous_managed_cancel_requires_selecting_an_exact_task() {
    use xcb_core::ui::{HabitatCommand, Intent};
    let (tx, rx) = sync_channel(8);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    let mut second = app.view.backlog[0].clone();
    second.id = xcb_core::Id::new("task_two").unwrap();
    second.revision = 11;
    app.view.backlog.push(second);
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    assert!(rx.try_recv().is_err());
    picker_key(&mut app, &tx, KeyCode::Down);
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Habitat(HabitatCommand::CancelTask { id, expected_revision: 11 })) if id.as_str() == "task_two")
    );
}

fn text_message(id: &str, text: &str) -> xcb_core::session::Message {
    xcb_core::session::Message {
        id: xcb_core::Id::new(id).unwrap(),
        role: xcb_core::session::Role::Assistant,
        text: text.into(),
        at_ms: 1,
        attachments: Vec::new(),
        provenance: None,
    }
}

#[test]
fn transcript_pages_are_context_bound_and_clear_display_preserves_messages() {
    use xcb_core::{
        Id,
        ui::{Intent, TranscriptContext, TranscriptPage, Update},
    };
    let (tx, rx) = sync_channel(8);
    let mut app = App::default();
    let mut view = view_for("session_one");
    let context = TranscriptContext::Session(Id::new("session_one").unwrap());
    view.messages = vec![text_message("recent", "recent body")];
    view.transcript = Some(TranscriptPage {
        context: context.clone(),
        messages: view.messages.clone(),
        first_sequence: Some(50),
        has_older: true,
    });
    app.apply(Update::View(Box::new(view)));
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert!(matches!(
        app.modal,
        Some(Modal::Transcript { has_more: true, .. })
    ));
    picker_key(&mut app, &tx, KeyCode::Char('p'));
    let request = match rx.try_recv().unwrap() {
        Intent::TranscriptPage {
            context: actual,
            before_sequence: 50,
            request,
        } => {
            assert_eq!(actual, context);
            request
        }
        _ => panic!("request older transcript page"),
    };
    let page = TranscriptPage {
        context: context.clone(),
        messages: vec![text_message("old", "older needle")],
        first_sequence: Some(1),
        has_older: false,
    };
    app.apply(Update::TranscriptPage {
        request: Id::new("unrelated").unwrap(),
        page: page.clone(),
    });
    assert!(
        matches!(&app.modal, Some(Modal::Transcript { lines, .. }) if !lines.iter().any(|line| line == "older needle"))
    );
    app.apply(Update::TranscriptPage { request, page });
    assert!(
        matches!(&app.modal, Some(Modal::Transcript { lines, has_more: false, .. }) if lines.iter().any(|line| line == "older needle"))
    );
    picker_key(&mut app, &tx, KeyCode::F(3));
    app.handle(Event::Paste("needle".into()), &tx);
    assert!(
        matches!(&app.modal, Some(Modal::Transcript { query, matches, .. }) if query == "needle" && matches.len() == 1)
    );
    picker_key(&mut app, &tx, KeyCode::Esc);
    picker_key(&mut app, &tx, KeyCode::Esc);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
        &tx,
    );
    assert_eq!(app.view.messages.len(), 1);
    assert_eq!(app.transcript_messages().count(), 0);
    assert!(
        rx.try_recv().is_err(),
        "clear display must not mutate persisted history"
    );
}

fn add_second_task(app: &mut App) {
    let mut second = app.view.backlog[0].clone();
    second.id = xcb_core::Id::new("task_two").unwrap();
    second.title = "Second task".into();
    second.revision = 11;
    app.view.backlog.push(second);
}

#[test]
fn disappearing_selected_task_never_redirects_cancellation_to_another_task() {
    let (tx, rx) = sync_channel(4);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    add_second_task(&mut app);
    app.composer.set_text("/steer task_one");
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.view.backlog.retain(|row| row.id.as_str() == "task_two");
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert!(
        rx.try_recv().is_err(),
        "an explicit stale target must not fall back to the remaining task"
    );
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("task_one"))
    );
    assert!(app.notice.contains("outside") || app.notice.contains("changed"));
}

#[test]
fn rejected_direct_submission_cannot_fill_a_newly_selected_task_composer() {
    use xcb_core::ui::{Intent, TranscriptContext, Update};
    let (tx, rx) = sync_channel(4);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    add_second_task(&mut app);
    app.composer.set_text("Earlier ordinary work");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let submitted = match rx.try_recv().unwrap() {
        Intent::SubmitTo {
            id,
            context: TranscriptContext::Conversation(context),
            ..
        } => {
            assert_eq!(context.as_str(), "conversation_one");
            id
        }
        _ => panic!("ordinary submit"),
    };
    app.composer.set_text("/steer task_two");
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.apply(Update::SubmitRejected {
        id: submitted,
        context: Some(TranscriptContext::Conversation(
            xcb_core::Id::new("conversation_one").unwrap(),
        )),
        text: "Earlier ordinary work".into(),
        attachments: Vec::new(),
        reason: "Fixture rejection".into(),
    });
    assert!(app.composer.text().is_empty());
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("task_two"))
    );
    app.composer.set_text("/drafts");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.iter().any(|item| item.label.contains("Earlier ordinary work")))
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn rejected_guidance_keeps_the_original_target_out_of_a_newly_selected_task() {
    use xcb_core::ui::{HabitatCommand, Intent, Update};
    let (tx, rx) = sync_channel(4);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    add_second_task(&mut app);
    app.composer.set_text("/steer task_one");
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.composer.set_text("Guidance for first task");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let event = match rx.try_recv().unwrap() {
        Intent::Habitat(HabitatCommand::Steer { event, .. }) => event,
        _ => panic!("task guidance"),
    };
    app.composer.set_text("/steer task_two");
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.apply(Update::HabitatDraft {
        context: xcb_core::Id::new("conversation_one").unwrap(),
        task: Some(xcb_core::Id::new("task_one").unwrap()),
        operation: event,
        text: "Guidance for first task".into(),
    });
    assert!(app.composer.text().is_empty());
    assert!(
        app.composer_target_label()
            .is_some_and(|label| label.contains("task_two"))
    );
    app.composer.set_text("/drafts");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.iter().any(|item| item.label.contains("Guidance for first task")))
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn rejected_guidance_uses_its_original_context_after_navigation() {
    use xcb_core::{
        Id,
        ui::{HabitatCommand, Intent, Update},
    };
    let (tx, rx) = sync_channel(4);
    let mut app = managed_fixture(xcb_core::session::State::Working);
    let original_view = app.view.clone();
    app.composer.set_text("/steer task_one");
    picker_key(&mut app, &tx, KeyCode::Enter);
    app.composer
        .set_text("Guidance belonging to conversation one");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let event = match rx.try_recv().unwrap() {
        Intent::Habitat(HabitatCommand::Steer { event, .. }) => event,
        _ => panic!("task guidance"),
    };
    let mut next_view = original_view.clone();
    next_view.conversation = Some(Id::new("conversation_two").unwrap());
    next_view.backlog[0].conversation = Id::new("conversation_two").unwrap();
    next_view.backlog[0].id = Id::new("task_two").unwrap();
    app.apply(Update::View(Box::new(next_view)));

    // The runtime reports its current context, which may have advanced since send.
    app.apply(Update::HabitatDraft {
        context: Id::new("conversation_two").unwrap(),
        task: Some(Id::new("task_one").unwrap()),
        operation: event,
        text: "Guidance belonging to conversation one".into(),
    });
    assert!(app.composer.text().is_empty());
    app.composer.set_text("/drafts");
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(
        matches!(&app.modal, Some(Modal::Picker { items, .. }) if items.iter().any(|item| item.label.contains("conversation_one") && item.label.contains("Guidance belonging")))
    );
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert!(app.composer.text().is_empty());
    assert!(app.notice.contains("conversation_one"));
    assert!(app.modal.is_none());

    app.apply(Update::View(Box::new(original_view)));
    app.composer.set_text("/drafts");
    picker_key(&mut app, &tx, KeyCode::Enter);
    picker_key(&mut app, &tx, KeyCode::Enter);
    assert_eq!(
        app.composer.text(),
        "Guidance belonging to conversation one"
    );
    assert!(rx.try_recv().is_err(), "recovery must never send the input");
}

#[test]
fn submitted_acknowledgement_matches_identity_across_context_navigation() {
    use xcb_core::ui::{Intent, TranscriptContext, Update};
    let (tx, rx) = sync_channel(4);
    let mut app = App::default();
    app.apply(Update::View(Box::new(view_for("session_a"))));
    app.composer.set_text("identical text");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let first = match rx.try_recv().unwrap() {
        Intent::SubmitTo {
            id,
            context: TranscriptContext::Session(context),
            ..
        } => {
            assert_eq!(context.as_str(), "session_a");
            id
        }
        _ => panic!("first submit"),
    };
    app.apply(Update::View(Box::new(view_for("session_b"))));
    app.composer.set_text("identical text");
    picker_key(&mut app, &tx, KeyCode::Enter);
    let second = match rx.try_recv().unwrap() {
        Intent::SubmitTo {
            id,
            context: TranscriptContext::Session(context),
            ..
        } => {
            assert_eq!(context.as_str(), "session_b");
            id
        }
        _ => panic!("second submit"),
    };
    assert_ne!(first, second);
    app.apply(Update::Submitted {
        id: first,
        context: TranscriptContext::Session(xcb_core::Id::new("session_a").unwrap()),
    });
    assert_eq!(
        app.pending_echoes().count(),
        1,
        "the current equal-text submission still awaits acknowledgement"
    );
    app.apply(Update::View(Box::new(view_for("session_a"))));
    assert_eq!(app.pending_echoes().count(), 0);
    app.apply(Update::View(Box::new(view_for("session_b"))));
    assert_eq!(app.pending_echoes().count(), 1);
    app.apply(Update::Submitted {
        id: second,
        context: TranscriptContext::Session(xcb_core::Id::new("session_b").unwrap()),
    });
    assert_eq!(app.pending_echoes().count(), 0);
}

#[test]
fn attention_shortcuts_preserve_drafts_and_conversation_navigation_requires_empty_input() {
    use xcb_core::{
        Id,
        ui::{ConversationRow, Intent},
    };
    let (tx, rx) = sync_channel(4);
    let mut app = managed_fixture(xcb_core::session::State::NeedsAnswer);
    app.view.conversations = ["conversation_one", "conversation_two"]
        .into_iter()
        .map(|name| ConversationRow {
            id: Id::new(name).unwrap(),
            title: name.into(),
            workspace: "/project".into(),
            messages: 0,
            updated_at_ms: 1,
        })
        .collect();
    app.composer.set_text("keep my current draft");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)),
        &tx,
    );
    assert_eq!(app.composer.text(), "keep my current draft");
    assert!(rx.try_recv().is_err());
    picker_key(&mut app, &tx, KeyCode::F(2));
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert_eq!(app.composer.text(), "keep my current draft");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT)),
        &tx,
    );
    assert!(matches!(app.modal, Some(Modal::Picker { .. })));
    picker_key(&mut app, &tx, KeyCode::Esc);
    assert_eq!(app.composer.text(), "keep my current draft");
    app.composer.set_text("");
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)),
        &tx,
    );
    assert!(
        matches!(rx.try_recv(), Ok(Intent::Conversation(id)) if id.as_str() == "conversation_two")
    );
}

#[test]
fn navigation_burst_cannot_retarget_submission_before_the_new_view_arrives() {
    use xcb_core::{
        Id,
        ui::{ConversationRow, HabitatCommand, Intent, TranscriptContext},
    };
    for submit in [KeyCode::Enter, KeyCode::Tab] {
        let (tx, rx) = sync_channel(4);
        let mut app = managed_fixture(xcb_core::session::State::Working);
        app.view.conversations = ["conversation_one", "conversation_two"]
            .into_iter()
            .map(|name| ConversationRow {
                id: Id::new(name).unwrap(),
                title: name.into(),
                workspace: "/project".into(),
                messages: 0,
                updated_at_ms: 1,
            })
            .collect();
        app.handle(
            Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)),
            &tx,
        );
        assert!(
            matches!(rx.try_recv(), Ok(Intent::Conversation(id)) if id.as_str() == "conversation_two")
        );
        // A terminal burst can send more keys before the asynchronous View arrives.
        app.handle(
            Event::Paste("Work for the visible conversation".into()),
            &tx,
        );
        picker_key(&mut app, &tx, submit);
        match rx.try_recv().unwrap() {
            Intent::SubmitTo {
                context: TranscriptContext::Conversation(context),
                text,
                ..
            } if submit == KeyCode::Enter => {
                assert_eq!(context.as_str(), "conversation_one");
                assert_eq!(text, "Work for the visible conversation");
            }
            Intent::Habitat(HabitatCommand::EnqueueIn {
                conversation,
                prompt,
                ..
            }) if submit == KeyCode::Tab => {
                assert_eq!(conversation.as_str(), "conversation_one");
                assert_eq!(prompt, "Work for the visible conversation");
            }
            _ => panic!("submitted input must carry the observed conversation identity"),
        }
    }
}
