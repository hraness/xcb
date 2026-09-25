use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::sync_channel;
use xcb_tui::{
    App,
    composer::{Composer, ComposerAction, VimMode},
};

fn key(c: &mut Composer, code: KeyCode) -> ComposerAction {
    c.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}
fn ch(c: &mut Composer, chr: char) -> ComposerAction {
    key(c, KeyCode::Char(chr))
}
fn keys(c: &mut Composer, seq: &str) -> ComposerAction {
    let mut action = ComposerAction::None;
    for chr in seq.chars() {
        action = ch(c, chr);
    }
    action
}
fn esc(c: &mut Composer) -> ComposerAction {
    key(c, KeyCode::Esc)
}
/// Vim freshly on (resetting leftover state), text loaded, Esc into Normal.
/// The caret lands on the last character; prefix motions with `gg`/`0`.
fn normal(c: &mut Composer, text: &str) {
    if c.vim_mode().is_some() {
        c.toggle_vim();
    }
    assert!(c.toggle_vim());
    c.set_text(text);
    assert!(matches!(esc(c), ComposerAction::None));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
}

#[test]
fn esc_toggles_normal_and_insert_without_cancelling_the_draft() {
    let mut c = Composer::default();
    assert_eq!(c.vim_mode(), None);
    assert!(c.toggle_vim());
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    c.set_text("draft");
    assert!(matches!(esc(&mut c), ComposerAction::None));
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    assert_eq!(c.text(), "draft");
    // Esc in normal mode keeps the app-level meaning: interrupt work.
    assert!(matches!(esc(&mut c), ComposerAction::Cancel));
    // Every insert entry returns to insert mode.
    c.set_text("x");
    for chr in ['i', 'a', 'A', 'I'] {
        esc(&mut c);
        assert_eq!(c.vim_mode(), Some(VimMode::Normal));
        ch(&mut c, chr);
        assert_eq!(c.vim_mode(), Some(VimMode::Insert), "{chr} enters insert");
    }
    assert!(!c.toggle_vim());
    assert_eq!(c.vim_mode(), None);
    // Vim off: Esc cancels again, exactly like the default keymap.
    assert!(matches!(esc(&mut c), ComposerAction::Cancel));
}

#[test]
fn insert_mode_keeps_the_readline_keymap() {
    let mut c = Composer::default();
    c.toggle_vim();
    for chr in "one two".chars() {
        ch(&mut c, chr);
    }
    assert_eq!(c.text(), "one two");
    c.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    )));
    c.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(c.text(), "");
    c.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('y'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(c.text(), "one two");
    // Ctrl-C still clears the draft and reports Cancel.
    assert!(matches!(
        c.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Cancel
    ));
    assert!(c.text().is_empty());
}

#[test]
fn word_and_line_motions_land_on_character_starts() {
    let mut c = Composer::default();
    normal(&mut c, "one two\nthree four");
    keys(&mut c, "gg");
    assert_eq!(c.textarea.cursor(), (0, 0));
    ch(&mut c, 'w');
    assert_eq!(c.textarea.cursor(), (0, 4));
    ch(&mut c, 'e');
    assert_eq!(c.textarea.cursor(), (0, 6));
    ch(&mut c, 'b');
    assert_eq!(c.textarea.cursor(), (0, 4));
    ch(&mut c, '0');
    assert_eq!(c.textarea.cursor(), (0, 0));
    ch(&mut c, '$');
    assert_eq!(c.textarea.cursor(), (0, 6));
    ch(&mut c, 'w');
    // `w` crosses the newline into the next word, like Vim.
    assert_eq!(c.textarea.cursor(), (1, 0));
    keys(&mut c, "2e");
    assert_eq!(c.textarea.cursor(), (1, 9));
    keys(&mut c, "0w");
    assert_eq!(c.textarea.cursor(), (1, 6));
    // Big words treat punctuation as part of the run.
    normal(&mut c, "foo.bar baz");
    keys(&mut c, "0w");
    assert_eq!(c.textarea.cursor(), (0, 3));
    keys(&mut c, "0W");
    assert_eq!(c.textarea.cursor(), (0, 8));
    ch(&mut c, 'B');
    assert_eq!(c.textarea.cursor(), (0, 0));
    ch(&mut c, 'E');
    assert_eq!(c.textarea.cursor(), (0, 6));
}

#[test]
fn delete_change_and_yank_operators_take_motions_and_counts() {
    let mut c = Composer::default();
    normal(&mut c, "one two\nthree four");
    keys(&mut c, "ggdw");
    assert_eq!(c.text(), "two\nthree four");
    keys(&mut c, "de");
    assert_eq!(c.text(), "\nthree four");
    c.set_text("one two\nthree four");
    keys(&mut c, "gg0dw");
    assert_eq!(c.text(), "two\nthree four");
    // `db` at the buffer start is a no-op; `D` clears to the line end.
    keys(&mut c, "db");
    assert_eq!(c.text(), "two\nthree four");
    ch(&mut c, 'D');
    assert_eq!(c.text(), "\nthree four");
    // Whole-line forms.
    normal(&mut c, "a\nb\nc\nd");
    keys(&mut c, "ggdd");
    assert_eq!(c.text(), "b\nc\nd");
    keys(&mut c, "2dd");
    assert_eq!(c.text(), "d");
    normal(&mut c, "a\nb\nc");
    keys(&mut c, "ggdj");
    assert_eq!(c.text(), "c");
    normal(&mut c, "a\nb\nc");
    keys(&mut c, "kdk");
    assert_eq!(c.text(), "c");
    normal(&mut c, "a\nb\nc");
    keys(&mut c, "ggdG");
    assert_eq!(c.text(), "");
    normal(&mut c, "a\nb\nc");
    keys(&mut c, "dgg");
    assert_eq!(c.text(), "");
    // `cw` is `ce`: the gap after the word survives.
    normal(&mut c, "one two");
    keys(&mut c, "0cw");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    ch(&mut c, 'x');
    assert_eq!(c.text(), "x two");
    // `cc` leaves one blank line open for typing.
    normal(&mut c, "a\nb\nc");
    keys(&mut c, "kcc");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    assert_eq!(c.text(), "a\n\nc");
    ch(&mut c, 'z');
    assert_eq!(c.text(), "a\nz\nc");
    // `C` changes to end of line only.
    normal(&mut c, "keep rest\nnext");
    keys(&mut c, "gg03l");
    ch(&mut c, 'C');
    assert_eq!(c.text(), "kee\nnext");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
}

#[test]
fn cut_substitute_replace_and_join() {
    let mut c = Composer::default();
    normal(&mut c, "abcd");
    keys(&mut c, "0x");
    assert_eq!(c.text(), "bcd");
    keys(&mut c, "2x");
    assert_eq!(c.text(), "d");
    c.set_text("abcd");
    keys(&mut c, "0ll");
    ch(&mut c, 'X');
    assert_eq!(c.text(), "acd");
    ch(&mut c, 'r');
    ch(&mut c, 'z');
    assert_eq!(c.text(), "azd");
    assert_eq!(c.vim_mode(), Some(VimMode::Normal));
    ch(&mut c, 's');
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    ch(&mut c, 'q');
    assert_eq!(c.text(), "aqd");
    // `J` joins lines with one space and eats the indent.
    normal(&mut c, "a\n  b");
    keys(&mut c, "ggJ");
    assert_eq!(c.text(), "a b");
    normal(&mut c, "a\nb\nc\nd");
    keys(&mut c, "gg3J");
    assert_eq!(c.text(), "a b c\nd");
}

#[test]
fn yank_and_put_are_linewise_or_charwise_by_source() {
    let mut c = Composer::default();
    normal(&mut c, "one two\nthree four");
    keys(&mut c, "ggywp");
    assert_eq!(c.text(), "oone ne two\nthree four");
    // A line yank puts on a fresh line and lands on it.
    normal(&mut c, "one two\nthree four");
    keys(&mut c, "ggyyp");
    assert_eq!(c.text(), "one two\none two\nthree four");
    assert_eq!(c.textarea.cursor(), (1, 0));
    normal(&mut c, "one two\nthree four");
    keys(&mut c, "ggyyjP");
    assert_eq!(c.text(), "one two\none two\nthree four");
    // `dd` then `p` reorders lines.
    normal(&mut c, "a\nb");
    keys(&mut c, "ggddp");
    assert_eq!(c.text(), "b\na");
    // `xp` transposes two characters.
    normal(&mut c, "abc");
    keys(&mut c, "0xp");
    assert_eq!(c.text(), "bac");
}

#[test]
fn find_motions_repeat_and_drive_operators() {
    let mut c = Composer::default();
    normal(&mut c, "a;b;c");
    keys(&mut c, "0f;");
    assert_eq!(c.textarea.cursor(), (0, 1));
    ch(&mut c, ';');
    assert_eq!(c.textarea.cursor(), (0, 3));
    ch(&mut c, ',');
    assert_eq!(c.textarea.cursor(), (0, 1));
    // Operators over finds: `df;` eats the hit, `dt;` stops short of it.
    normal(&mut c, "a;b;c");
    keys(&mut c, "0df;");
    assert_eq!(c.text(), "b;c");
    normal(&mut c, "a;b;c");
    keys(&mut c, "0dt;");
    assert_eq!(c.text(), ";b;c");
    normal(&mut c, "a;b;c");
    keys(&mut c, "0d2f;");
    assert_eq!(c.text(), "c");
    // Backward finds are inclusive of the char under the caret.
    normal(&mut c, "a;b");
    keys(&mut c, "dF;");
    assert_eq!(c.text(), "a");
    normal(&mut c, "a;b");
    keys(&mut c, "dT;");
    assert_eq!(c.text(), "a;");
}

#[test]
fn undo_redo_open_line_and_pending_abort() {
    let mut c = Composer::default();
    normal(&mut c, "one two");
    keys(&mut c, "0dw");
    assert_eq!(c.text(), "two");
    ch(&mut c, 'u');
    assert_eq!(c.text(), "one two");
    c.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(c.text(), "two");
    // `o` opens below, `O` above; both enter insert.
    normal(&mut c, "abc");
    ch(&mut c, 'o');
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    ch(&mut c, 'x');
    assert_eq!(c.text(), "abc\nx");
    esc(&mut c);
    ch(&mut c, 'O');
    ch(&mut c, 'y');
    assert_eq!(c.text(), "abc\ny\nx");
    // A pending operator or count aborts on Esc without touching the text.
    normal(&mut c, "abc");
    ch(&mut c, 'd');
    esc(&mut c);
    ch(&mut c, 'w');
    assert_eq!(c.text(), "abc");
    assert_eq!(c.textarea.cursor(), (0, 2));
    normal(&mut c, "abc");
    keys(&mut c, "03");
    esc(&mut c);
    ch(&mut c, 'l');
    assert_eq!(c.textarea.cursor(), (0, 1));
    // `dx` aborts the operator and consumes the key.
    normal(&mut c, "abc");
    keys(&mut c, "dx");
    assert_eq!(c.text(), "abc");
}

#[test]
fn enter_submits_from_normal_and_returns_to_insert() {
    let mut c = Composer::default();
    normal(&mut c, "send me");
    let action = key(&mut c, KeyCode::Enter);
    assert!(matches!(action, ComposerAction::Submit(ref text) if text == "send me"));
    assert_eq!(c.text(), "");
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
}

#[test]
fn normal_mode_keeps_composer_services_and_slash_commands() {
    let mut c = Composer::default();
    normal(&mut c, "");
    assert!(matches!(
        c.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Clipboard
    ));
    assert!(matches!(
        c.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Editor
    ));
    assert!(matches!(
        c.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL
        ))),
        ComposerAction::Quit
    ));
    // `/` in normal mode re-enters insert already holding the slash.
    c.set_text("draft");
    ch(&mut c, '/');
    assert_eq!(c.vim_mode(), Some(VimMode::Insert));
    assert_eq!(c.text(), "draft/");
}

#[test]
fn app_level_slash_vim_toggles_and_survives_submission() {
    let (tx, _rx) = sync_channel(8);
    let mut app = App::default();
    app.handle(Event::Paste("/vim".into()), &tx);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx,
    );
    assert_eq!(app.composer.vim_mode(), Some(VimMode::Insert));
    app.handle(Event::Paste("work".into()), &tx);
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &tx,
    );
    assert_eq!(app.composer.vim_mode(), Some(VimMode::Normal));
    // Enter submits; the app's initial view keeps the draft, but the mode
    // reset already happened inside the composer.
    app.handle(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &tx,
    );
    assert_eq!(app.composer.vim_mode(), Some(VimMode::Insert));
}

#[test]
fn motions_are_grapheme_safe() {
    let mut c = Composer::default();
    normal(&mut c, "e\u{301} x 🇵🇷");
    // `w` lands on the next word start, never mid-cluster.
    keys(&mut c, "0w");
    assert_eq!(c.textarea.cursor(), (0, 3));
    keys(&mut c, "dw");
    assert_eq!(c.text(), "e\u{301} 🇵🇷");
    normal(&mut c, "a😀b");
    keys(&mut c, "0x");
    assert_eq!(c.text(), "😀b");
    ch(&mut c, 'x');
    assert_eq!(c.text(), "b");
    // A find hit inside a cluster lands on the cluster, never mid-grapheme.
    normal(&mut c, "q e\u{301} z");
    keys(&mut c, "0f\u{301}");
    assert_eq!(c.textarea.cursor(), (0, 2));
    // `d` to that hit removes through the whole cluster, splitting nothing.
    normal(&mut c, "q e\u{301} z");
    keys(&mut c, "0df\u{301}");
    assert_eq!(c.text(), " z");
}
