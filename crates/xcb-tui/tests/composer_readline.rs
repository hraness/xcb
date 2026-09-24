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
