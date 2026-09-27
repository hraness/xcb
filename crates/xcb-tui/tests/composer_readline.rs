use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use xcb_tui::composer::{Composer, ComposerAction, MAX_HISTORY_BYTES, MAX_INPUT};

fn key(composer: &mut Composer, code: KeyCode, modifiers: KeyModifiers) -> ComposerAction {
    composer.handle(Event::Key(KeyEvent::new(code, modifiers)))
}
fn ctrl(composer: &mut Composer, ch: char) {
    key(composer, KeyCode::Char(ch), KeyModifiers::CONTROL);
}
#[test]
fn grapheme_movement_deletion_and_yank_keep_combining_and_emoji_intact() {
    let mut composer = Composer::default();
    composer.set_text("e\u{301}👩🏽‍💻🇵🇷");
    ctrl(&mut composer, 'b');
    key(&mut composer, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(composer.text(), "e\u{301}🇵🇷");
    ctrl(&mut composer, 'a');
    ctrl(&mut composer, 'f');
    assert_eq!(composer.textarea.cursor(), (0, 2));
    ctrl(&mut composer, 'k');
    assert_eq!(composer.text(), "e\u{301}");
    composer.set_text("");
    ctrl(&mut composer, 'y');
    assert_eq!(composer.text(), "🇵🇷");
    key(&mut composer, KeyCode::Backspace, KeyModifiers::NONE);
    assert_eq!(composer.text(), "");
    composer.set_text("e\u{301}👩🏽‍💻");
    key(&mut composer, KeyCode::Left, KeyModifiers::SHIFT);
    ctrl(&mut composer, 'k');
    assert_eq!(composer.text(), "e\u{301}");
    ctrl(&mut composer, 'y');
    assert_eq!(composer.text(), "e\u{301}👩🏽‍💻");
}
#[test]
fn shift_tab_leaves_the_draft_and_cursor_unchanged() {
    let mut composer = Composer::default();
    composer.set_text("keep\nthis draft");
    let cursor = composer.textarea.cursor();
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
        key(&mut composer, KeyCode::BackTab, modifiers);
    }
    assert_eq!(composer.text(), "keep\nthis draft");
    assert_eq!(composer.textarea.cursor(), cursor);
}
#[test]
fn readline_words_line_kills_and_vertical_keys_have_editing_meaning() {
    let mut composer = Composer::default();
    composer.set_text("one two\nthree four");
    ctrl(&mut composer, 'p');
    assert_eq!(composer.textarea.cursor().0, 0);
    ctrl(&mut composer, 'n');
    assert_eq!(composer.textarea.cursor().0, 1);
    ctrl(&mut composer, 'e');
    ctrl(&mut composer, 'w');
    assert_eq!(composer.text(), "one two\nthree ");
    ctrl(&mut composer, 'y');
    assert_eq!(composer.text(), "one two\nthree four");
    key(&mut composer, KeyCode::Left, KeyModifiers::CONTROL);
    key(&mut composer, KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(composer.text(), "one two\nthree ");
    ctrl(&mut composer, 'u');
    assert_eq!(composer.text(), "one two\n");
    ctrl(&mut composer, 'u');
    assert_eq!(composer.text(), "one two");
    ctrl(&mut composer, 'y');
    assert_eq!(composer.text(), "one two\n");
    composer.set_text("alpha beta");
    key(&mut composer, KeyCode::Char('b'), KeyModifiers::ALT);
    key(&mut composer, KeyCode::Char('f'), KeyModifiers::ALT);
    key(&mut composer, KeyCode::Backspace, KeyModifiers::ALT);
    assert_eq!(composer.text(), "alpha ");
    composer.set_text("foo.bar");
    key(&mut composer, KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(composer.textarea.cursor(), (0, 4));
    key(&mut composer, KeyCode::Char('b'), KeyModifiers::ALT);
    assert_eq!(composer.textarea.cursor(), (0, 3));
}
#[test]
fn history_requires_an_unchanged_recall_at_an_absolute_boundary() {
    let mut composer = Composer::default();
    composer.remember("older\nbody");
    composer.remember("newer\nbody");
    composer.set_text("draft");
    key(&mut composer, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(composer.text(), "draft");
    assert!(composer.recall_history(0));
    ctrl(&mut composer, 'b');
    key(&mut composer, KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(composer.text(), "newer\nbody");
    assert_eq!(composer.textarea.cursor().0, 0);
    ctrl(&mut composer, 'a');
    ctrl(&mut composer, 'p');
    assert_eq!(composer.text(), "older\nbody");
    ctrl(&mut composer, 'n');
    assert_eq!(composer.text(), "newer\nbody");
    ctrl(&mut composer, 'n');
    assert_eq!(composer.text(), "draft");
    assert!(composer.previous_history());
    key(&mut composer, KeyCode::Char('!'), KeyModifiers::NONE);
    ctrl(&mut composer, 'n');
    assert_eq!(composer.text(), "newer\nbody!");
    composer.set_text("replacement");
    assert!(!composer.next_history());
    assert_eq!(composer.text(), "replacement");
}
#[test]
fn history_and_all_insertion_paths_enforce_byte_limits() {
    let mut composer = Composer::default();
    for index in 0..10 {
        composer.remember(&format!("{index}{}", "x".repeat(MAX_INPUT - 1)));
    }
    assert!(composer.history().map(String::len).sum::<usize>() <= MAX_HISTORY_BYTES);
    composer.set_text(&"x".repeat(MAX_INPUT - 1));
    assert!(matches!(
        key(&mut composer, KeyCode::Char('λ'), KeyModifiers::NONE),
        ComposerAction::Rejected(_)
    ));
    assert_eq!(composer.text().len(), MAX_INPUT - 1);
    assert!(matches!(
        composer.handle(Event::Paste("more".into())),
        ComposerAction::Rejected(_)
    ));
    assert_eq!(composer.text().len(), MAX_INPUT - 1);
    composer.set_text("before\r\nafter\u{1b}\u{202e}");
    assert_eq!(composer.text(), "before\nafter");
}
#[test]
fn a_full_size_draft_still_edits_correctly() {
    let none = KeyModifiers::NONE;
    let row = "0123456789 abcdefghij, klmnop.\n";
    let tail = "e\u{301}👩\u{200d}💻 end";
    let rows = row.repeat((MAX_INPUT - 1 - tail.len()) / row.len());
    let pad = "x".repeat(MAX_INPUT - 1 - rows.len() - tail.len());
    let text = format!("{pad}{rows}{tail}");
    assert_eq!(text.len(), MAX_INPUT - 1);
    let mut composer = Composer::default();
    composer.set_text(&text);

    // The last byte fits; anything more is refused whole and changes nothing.
    assert!(matches!(
        key(&mut composer, KeyCode::Char('!'), none),
        ComposerAction::None
    ));
    assert_eq!(composer.text(), format!("{text}!"));
    for refused in [
        key(&mut composer, KeyCode::Char('?'), none),
        key(&mut composer, KeyCode::Enter, KeyModifiers::ALT),
        composer.handle(Event::Paste("?".into())),
    ] {
        assert!(matches!(refused, ComposerAction::Rejected(_)));
    }
    assert_eq!(composer.text().len(), MAX_INPUT);

    // Deleting at the tail keeps combining marks and emoji sequences whole.
    key(&mut composer, KeyCode::Backspace, none);
    ctrl(&mut composer, 'w');
    ctrl(&mut composer, 'w');
    assert_eq!(composer.text(), format!("{pad}{rows}e\u{301}"));
    key(&mut composer, KeyCode::Backspace, none);
    assert_eq!(composer.text(), format!("{pad}{rows}"));
    ctrl(&mut composer, 'y');
    assert_eq!(composer.text(), format!("{pad}{rows}👩\u{200d}💻 "));

    // Edits on another line land where the cursor moved.
    key(&mut composer, KeyCode::Up, none);
    key(&mut composer, KeyCode::Home, none);
    key(&mut composer, KeyCode::Char('Z'), none);
    ctrl(&mut composer, 'e');
    ctrl(&mut composer, 'w');
    ctrl(&mut composer, 'w');
    let body = format!("{pad}{rows}");
    let last_row = body[..body.len() - 1].rfind('\n').unwrap() + 1;
    let expected = format!(
        "{}Z0123456789 abcdefghij, \n👩\u{200d}💻 ",
        &body[..last_row]
    );
    assert_eq!(composer.text(), expected);
    assert!(
        matches!(key(&mut composer, KeyCode::Enter, none), ComposerAction::Submit(sent) if sent == expected)
    );
    assert!(composer.text().is_empty());
}

/// Per-keystroke cost on near-limit drafts, many short lines and one long
/// line. Run with `cargo test -p xcb-tui --test composer_readline --
/// --ignored --nocapture composer_keystroke_benchmark`; timings print.
#[test]
#[ignore]
fn composer_keystroke_benchmark() {
    let row = format!("{}\n", "word ".repeat(12).trim_end());
    let lines = row.repeat((MAX_INPUT - 4096) / row.len());
    let long = "word ".repeat((MAX_INPUT - 4096) / 5);
    for (shape, text) in [("multi-line", lines), ("single-line", long)] {
        let mut composer = Composer::default();
        composer.set_text(&text);
        let mut time = |label: &str, keys: &[(KeyCode, KeyModifiers)]| {
            let started = std::time::Instant::now();
            for _ in 0..100 {
                for (code, modifiers) in keys {
                    key(&mut composer, *code, *modifiers);
                }
            }
            let per_key = started.elapsed() / (100 * keys.len() as u32);
            println!(
                "{shape} {} KiB · {label}: {per_key:?}/key",
                text.len() / 1024
            );
        };
        let none = KeyModifiers::NONE;
        time(
            "type + backspace",
            &[(KeyCode::Char('a'), none), (KeyCode::Backspace, none)],
        );
        time(
            "left + right",
            &[(KeyCode::Left, none), (KeyCode::Right, none)],
        );
        time(
            "word left + right",
            &[
                (KeyCode::Char('b'), KeyModifiers::ALT),
                (KeyCode::Char('f'), KeyModifiers::ALT),
            ],
        );
        time("home + end", &[(KeyCode::Home, none), (KeyCode::End, none)]);
        time(
            "kill word + yank",
            &[
                (KeyCode::Char('w'), KeyModifiers::CONTROL),
                (KeyCode::Char('y'), KeyModifiers::CONTROL),
            ],
        );
        assert_eq!(composer.text(), text);
    }
}

#[test]
fn cursor_positions_beyond_u16_remain_correct() {
    let mut composer = Composer::default();
    composer.set_text(&format!("{}é", "x".repeat(70_000)));
    ctrl(&mut composer, 'b');
    assert_eq!(composer.textarea.cursor(), (0, 70_000));
    key(&mut composer, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(composer.text().len(), 70_000);
    composer.set_text(&format!("{}é", "\n".repeat(70_000)));
    ctrl(&mut composer, 'a');
    assert_eq!(composer.textarea.cursor(), (70_000, 0));
    key(&mut composer, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(composer.text().len(), 70_000);
}
