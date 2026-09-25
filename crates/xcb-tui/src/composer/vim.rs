//! Vim-style modal editing for the composer, toggled by `/vim`.
//!
//! Insert mode keeps the ordinary editor keymap (`Composer::emacs_key`); Esc
//! enters Normal. Normal supports counts (`3w`, `d2w`, `2dd`), motions
//! (`h l j k` and arrows, `w b e W B E`, `0 ^ $`, `gg G`, `{ }`,
//! `f F t T` with `;` `,`), operators (`d c y`, doubled for whole lines,
//! `D C Y`, `x X s S`, `r`), linewise or charwise `p`/`P`, `J`, `u`/`Ctrl-R`
//! undo-redo, and `i a A I o O` back to insert. Enter still sends the prompt,
//! which is the composer's contract in every mode; Esc in Normal keeps the
//! app-level meaning (interrupt running work) unless a count or operator is
//! pending, in which case it only aborts the pending command.
//!
//! The cursor stays a between-grapheme caret, so motions that land "on" a
//! character in Vim land the caret at that character's start; inclusive
//! operator motions (`e`, `$`, `f`/`F`) extend their range to cover it.

use super::{Composer, ComposerAction, INPUT_TOO_LARGE};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui_textarea::CursorMove;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Insert,
    Normal,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

/// A command that consumed one key and waits for the next.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Pending {
    #[default]
    None,
    /// `g`, waiting for `g` (document top).
    G,
    /// `r`, waiting for the replacement character.
    Replace,
    /// `f`/`F`/`t`/`T`, waiting for the target character.
    Find { backward: bool, till: bool },
}

#[derive(Clone, Copy)]
struct Find {
    ch: char,
    backward: bool,
    till: bool,
}

#[derive(Clone, Copy, Default)]
pub struct Vim {
    pub mode: Mode,
    pending: Pending,
    /// Operator waiting for its motion, e.g. the first `d` of `dd`.
    op: Option<Op>,
    /// Digits accumulated before the operator (`2d`).
    op_count: usize,
    /// Digits accumulated after the operator or for a bare motion.
    count: usize,
    last_find: Option<Find>,
}

impl Vim {
    fn clear_command(&mut self) {
        self.pending = Pending::None;
        self.op = None;
        self.op_count = 0;
        self.count = 0;
    }
    /// `2d` + `3w` applies w six times.
    fn take_count(&mut self) -> usize {
        let count = self.op_count.max(1) * self.count.max(1);
        self.op_count = 0;
        self.count = 0;
        count
    }
    /// Sending a prompt always lands back in a clean Insert state.
    pub(super) fn sent(&mut self) {
        *self = Vim::default();
    }
}

/// Word classes for `w`/`b`/`e`: keyword runs (letters, digits, `_`),
/// punctuation runs, and blanks. `W`/`B`/`E` treat every non-blank as one class.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Keyword,
    Punct,
}
fn class(grapheme: &str, big: bool) -> Class {
    if grapheme.chars().all(char::is_whitespace) {
        return Class::Blank;
    }
    if big || grapheme.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
        Class::Keyword
    } else {
        Class::Punct
    }
}

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |i| i + 1)
}
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |i| at + i)
}
fn first_non_blank(text: &str, at: usize) -> usize {
    let start = line_start(text, at);
    start
        + text[start..line_end(text, at)]
            .chars()
            .take_while(|ch| ch.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
}
fn row_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count()
}
/// Byte bounds of line `row` as (start, end not including the newline).
fn line_bounds(text: &str, row: usize) -> (usize, usize) {
    let mut start = 0;
    for _ in 0..row {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return (text.len(), text.len()),
        }
    }
    (start, line_end(text, start))
}
fn last_row(text: &str) -> usize {
    text.matches('\n').count()
}
/// Whole-line byte range covering `first_row..=last_row`, absorbing the
/// separator newline the way `dd` does at the last line.
fn rows_range(text: &str, first_row: usize, last: usize) -> (usize, usize) {
    let last = last.min(last_row(text));
    let (mut start, _) = line_bounds(text, first_row);
    let (_, mut end) = line_bounds(text, last);
    if end < text.len() {
        end += 1;
    } else {
        start = start.saturating_sub(1);
    }
    (start, end)
}

fn previous_grapheme_offset(text: &str, at: usize) -> usize {
    text[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}
fn next_grapheme_offset(text: &str, at: usize) -> usize {
    at + text[at..].graphemes(true).next().map_or(0, str::len)
}

/// Start of the next word (`w`): skip the rest of the current class run and
/// any blanks; past the end stays at the text end.
fn word_forward(text: &str, at: usize, big: bool) -> usize {
    let gs: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    let mut i = gs.partition_point(|(offset, _)| *offset < at);
    if i >= gs.len() {
        return text.len();
    }
    let here = class(gs[i].1, big);
    if here != Class::Blank {
        while i < gs.len() && class(gs[i].1, big) == here {
            i += 1;
        }
    }
    while i < gs.len() && class(gs[i].1, big) == Class::Blank {
        i += 1;
    }
    gs.get(i).map_or(text.len(), |(offset, _)| *offset)
}
/// Start of the current or previous word (`b`).
fn word_backward(text: &str, at: usize, big: bool) -> usize {
    let gs: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    let mut i = gs.partition_point(|(offset, _)| *offset < at);
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && class(gs[i].1, big) == Class::Blank {
        i -= 1;
    }
    let target = class(gs[i].1, big);
    while i > 0 && class(gs[i - 1].1, big) == target {
        i -= 1;
    }
    gs[i].0
}
/// Start of the last grapheme of the current or next word (`e`).
fn word_end(text: &str, at: usize, big: bool) -> usize {
    let gs: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
    if gs.is_empty() {
        return 0;
    }
    // Index of the grapheme under the caret, then step at least one forward.
    let mut i = gs.partition_point(|(offset, _)| *offset <= at).max(1) - 1;
    i += 1;
    while i < gs.len() && class(gs[i].1, big) == Class::Blank {
        i += 1;
    }
    if i >= gs.len() {
        return gs.last().map_or(0, |(offset, _)| *offset);
    }
    let target = class(gs[i].1, big);
    while i + 1 < gs.len() && class(gs[i + 1].1, big) == target {
        i += 1;
    }
    gs[i].0
}
/// Next/previous blank-line boundary (`{`/`}`).
fn para_forward(text: &str, at: usize) -> usize {
    let mut row = row_of(text, at) + 1;
    while row < last_row(text) {
        let (s, e) = line_bounds(text, row);
        if text[s..e].trim().is_empty() {
            return s;
        }
        row += 1;
    }
    text.len()
}
fn para_backward(text: &str, at: usize) -> usize {
    let mut row = row_of(text, at);
    while row > 0 {
        row -= 1;
        let (s, e) = line_bounds(text, row);
        if text[s..e].trim().is_empty() {
            return s;
        }
    }
    0
}
/// The offset `f`/`F`/`t`/`T` searches: strictly after the caret going
/// forward, strictly before it going backward, confined to the line. A hit
/// inside a grapheme cluster (a combining mark, a flag's second half) snaps
/// to the cluster start so the caret never splits it.
fn find_char(text: &str, at: usize, find: Find) -> Option<usize> {
    let (start, end) = (line_start(text, at), line_end(text, at));
    let line = &text[start..end];
    // `i` is line-relative; returns the absolute start of its grapheme.
    let snap = |i: usize| -> usize {
        start
            + line
                .grapheme_indices(true)
                .map(|(g, _)| g)
                .take_while(|g| *g <= i)
                .last()
                .unwrap_or(0)
    };
    if find.backward {
        line[..at - start].rfind(find.ch).map(snap)
    } else {
        if at >= end {
            return None;
        }
        let from = at + text[at..end].graphemes(true).next().map_or(1, str::len);
        line[from - start..]
            .find(find.ch)
            .map(|i| snap(i + from - start))
    }
}

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Down,
    Up,
    LineStart,
    FirstNonBlank,
    LineEnd,
    WordForward { big: bool },
    WordBackward { big: bool },
    WordEnd { big: bool },
    DocStart,
    DocEnd,
    ParaForward,
    ParaBackward,
    Find(Find),
}

/// (caret target, inclusive for operators, linewise for operators).
/// `Down`/`Up` have no byte target; dispatch handles them before this.
/// `count` is at least 1; `counted` marks whether digits were really typed —
/// bare `G` goes to the last line while `1G` goes to line one.
fn motion_target(
    text: &str,
    at: usize,
    motion: Motion,
    count: usize,
    counted: bool,
) -> (usize, bool, bool) {
    let mut target = at;
    match motion {
        Motion::Left => {
            for _ in 0..count {
                target = previous_grapheme_offset(text, target);
            }
            (target, false, false)
        }
        Motion::Right => {
            for _ in 0..count {
                target = next_grapheme_offset(text, target);
            }
            (target, false, false)
        }
        Motion::LineStart => (line_start(text, at), false, false),
        Motion::FirstNonBlank => (first_non_blank(text, at), false, false),
        Motion::LineEnd => {
            let start = line_start(text, at);
            let end = line_end(text, at);
            // Land on the last grapheme of the line, like the block cursor.
            let last = text[start..end]
                .grapheme_indices(true)
                .next_back()
                .map_or(end, |(i, _)| start + i);
            (last, true, false)
        }
        Motion::WordForward { big } => {
            for _ in 0..count {
                target = word_forward(text, target, big);
            }
            (target, false, false)
        }
        Motion::WordBackward { big } => {
            for _ in 0..count {
                target = word_backward(text, target, big);
            }
            (target, false, false)
        }
        Motion::WordEnd { big } => {
            for _ in 0..count {
                target = word_end(text, target, big);
            }
            (target, true, false)
        }
        // `gg` without a count is line one; `G`/`<n>gg` is the last/nth line.
        Motion::DocStart => {
            let row = if counted { count - 1 } else { 0 }.min(last_row(text));
            let (s, _) = line_bounds(text, row);
            (first_non_blank(text, s), false, true)
        }
        Motion::DocEnd => {
            let row = if counted { count - 1 } else { last_row(text) }.min(last_row(text));
            let (s, _) = line_bounds(text, row);
            (first_non_blank(text, s), false, true)
        }
        Motion::ParaForward => (para_forward(text, at), false, false),
        Motion::ParaBackward => (para_backward(text, at), false, false),
        Motion::Find(find) => {
            let mut hit = None;
            for _ in 0..count.max(1) {
                match find_char(text, target, find) {
                    Some(offset) => {
                        hit = Some(offset);
                        target = offset;
                    }
                    None => break,
                }
            }
            match hit {
                None => (at, false, false),
                // `f`/`F`/`t`/`T` are all inclusive motions; `t`/`T` just land
                // one char short of the found character. For a backward
                // motion the "far end" is the caret, so inclusive reaches the
                // character under it — `dF;` removes `;` *and* `b`.
                Some(hit) if find.backward => (
                    if find.till {
                        next_grapheme_offset(text, hit)
                    } else {
                        hit
                    },
                    true,
                    false,
                ),
                Some(hit) => (
                    if find.till {
                        previous_grapheme_offset(text, hit)
                    } else {
                        hit
                    },
                    true,
                    false,
                ),
            }
        }
        Motion::Down | Motion::Up => (at, false, true),
    }
}

/// Apply an operator to the whole-line byte range `[start, end)`, keeping the
/// yanked text linewise so `p`/`P` re-open a line instead of splicing.
fn linewise_op(c: &mut Composer, v: &mut Vim, op: Op, start: usize, end: usize) -> ComposerAction {
    let text = c.text();
    if start >= end {
        return ComposerAction::None;
    }
    // The range may carry a separator newline at either edge; the register
    // holds just the line contents so `p` re-opens exactly the yanked lines.
    let content = text[start..end].trim_matches('\n').to_owned();
    // The first line the range actually covers: a leading `start` sits on the
    // separator before it (last-line deletes absorb that side).
    let first = if text.as_bytes().get(start) == Some(&b'\n') {
        start + 1
    } else {
        start
    };
    match op {
        Op::Yank => {
            c.kill_buffer = content;
            c.kill_linewise = true;
            c.move_to(first_non_blank(&text, first.min(text.len())));
        }
        Op::Delete => {
            c.delete_range(start..end, false);
            c.kill_buffer = content;
            c.kill_linewise = true;
            let after = c.text();
            c.move_to(first_non_blank(&after, first.min(after.len())));
        }
        Op::Change => {
            c.delete_range(start..end, false);
            c.kill_buffer = content;
            c.kill_linewise = true;
            let pos = start.min(c.text().len());
            // Leave one open line behind, like `cc`.
            if pos < c.text().len() || pos > 0 {
                if !c.insert("\n") {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
                c.move_to(pos);
            }
            v.mode = Mode::Insert;
        }
    }
    ComposerAction::None
}

/// Apply an operator over a motion from `at` to `target`. `inclusive` covers
/// the character at the far endpoint — the motion's landing char for forward
/// moves (`de`, `df;`), the char under the caret for backward ones (`dF;`).
fn apply_op(c: &mut Composer, v: &mut Vim, op: Op, at: usize, target: usize, inclusive: bool) {
    let text = c.text();
    let (start, end) = if target >= at {
        (
            at,
            if inclusive {
                next_grapheme_offset(&text, target)
            } else {
                target
            },
        )
    } else {
        (
            target,
            if inclusive {
                next_grapheme_offset(&text, at)
            } else {
                at
            },
        )
    };
    if start >= end {
        return;
    }
    match op {
        Op::Yank => {
            c.kill_buffer = text[start..end].to_owned();
            c.kill_linewise = false;
            c.move_to(start);
        }
        Op::Delete => {
            c.delete_range(start..end, true);
        }
        Op::Change => {
            c.delete_range(start..end, true);
            v.mode = Mode::Insert;
        }
    }
}

fn dispatch_motion(c: &mut Composer, v: &mut Vim, motion: Motion) -> ComposerAction {
    let counted = v.count > 0 || v.op_count > 0;
    let count = v.take_count();
    let text = c.text();
    let at = c.cursor_offset();
    if let Some(op) = v.op.take() {
        match motion {
            // `dj`/`dk` span the current line plus `count` beyond it.
            Motion::Down => {
                let row = row_of(&text, at);
                let (s, e) = rows_range(&text, row, row + count);
                return linewise_op(c, v, op, s, e);
            }
            Motion::Up => {
                let row = row_of(&text, at);
                let (s, e) = rows_range(&text, row.saturating_sub(count), row);
                return linewise_op(c, v, op, s, e);
            }
            // `cw`/`cW` behave like `ce`/`cE`: the trailing gap survives.
            Motion::WordForward { big } if op == Op::Change => {
                let (end_of, _, _) =
                    motion_target(&text, at, Motion::WordEnd { big }, count, counted);
                apply_op(c, v, op, at, end_of, true);
                return ComposerAction::None;
            }
            _ => {
                let (target, inclusive, linewise) =
                    motion_target(&text, at, motion, count, counted);
                if linewise {
                    // `dgg`/`dG` cover whole lines from here to the target.
                    let r1 = row_of(&text, at.min(target));
                    let r2 = row_of(&text, at.max(target));
                    let (s, e) = rows_range(&text, r1, r2);
                    return linewise_op(c, v, op, s, e);
                }
                if target == at && !inclusive {
                    return ComposerAction::None;
                }
                apply_op(c, v, op, at, target, inclusive);
                return ComposerAction::None;
            }
        }
    }
    match motion {
        Motion::Down => {
            for _ in 0..count {
                c.textarea.move_cursor(CursorMove::Down);
            }
            c.snap_cursor();
        }
        Motion::Up => {
            for _ in 0..count {
                c.textarea.move_cursor(CursorMove::Up);
            }
            c.snap_cursor();
        }
        _ => {
            let (target, _, _) = motion_target(&text, at, motion, count, counted);
            c.move_to(target);
        }
    }
    ComposerAction::None
}

fn paste(c: &mut Composer, v: &mut Vim, before_cursor: bool) -> ComposerAction {
    v.clear_command();
    if c.kill_buffer.is_empty() {
        return ComposerAction::None;
    }
    let text = c.text();
    let at = c.cursor_offset();
    if c.kill_linewise {
        if before_cursor {
            let start = line_start(&text, at);
            c.move_to(start);
            if !c.insert(&format!("{}\n", c.kill_buffer)) {
                return ComposerAction::Rejected(INPUT_TOO_LARGE);
            }
            c.move_to(start);
        } else {
            let end = line_end(&text, at);
            c.move_to(end);
            if text.is_empty() {
                if !c.insert(&c.kill_buffer.clone()) {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
            } else if !c.insert(&format!("\n{}", c.kill_buffer)) {
                return ComposerAction::Rejected(INPUT_TOO_LARGE);
            }
            let after = c.text();
            let landing = if text.is_empty() { 0 } else { end + 1 };
            c.move_to(first_non_blank(&after, landing));
        }
    } else {
        let pos = if before_cursor {
            at
        } else {
            next_grapheme_offset(&text, at)
        };
        c.move_to(pos);
        if !c.insert(&c.kill_buffer.clone()) {
            return ComposerAction::Rejected(INPUT_TOO_LARGE);
        }
        // Land on the last pasted grapheme, like the block cursor.
        let after = c.text();
        c.move_to(previous_grapheme_offset(&after, pos + c.kill_buffer.len()));
    }
    ComposerAction::None
}

/// Join the next line into the current one with a single space, `count`
/// times. `J` joins two lines minimum; `<n>J` joins n+1.
fn join_lines(c: &mut Composer, v: &mut Vim) -> ComposerAction {
    let joins = v.take_count().max(2) - 1;
    for _ in 0..joins {
        let text = c.text();
        let at = c.cursor_offset();
        let end = line_end(&text, at);
        if end >= text.len() {
            break;
        }
        let (ns, ne) = line_bounds(&text, row_of(&text, end) + 1);
        let ws = text[ns..ne].len() - text[ns..ne].trim_start().len();
        c.delete_range(end..ns + ws, false);
        if ns + ws < ne {
            c.move_to(end);
            c.insert(" ");
        }
    }
    ComposerAction::None
}

/// Open a blank line below (`o`) or above (`O`) and enter Insert.
fn open_line(c: &mut Composer, v: &mut Vim, below: bool) -> ComposerAction {
    v.clear_command();
    let text = c.text();
    let at = c.cursor_offset();
    let pos = if below {
        line_end(&text, at)
    } else {
        line_start(&text, at)
    };
    c.move_to(pos);
    if !c.insert("\n") {
        return ComposerAction::Rejected(INPUT_TOO_LARGE);
    }
    // `o` leaves the caret after the newline; `O` pulls it back onto the
    // fresh line above the old one.
    if !below {
        c.move_to(pos);
    }
    v.mode = Mode::Insert;
    ComposerAction::None
}

/// Normal mode's caret sits on a grapheme like a block cursor, never in the
/// virtual slot after a line's last character or on the newline itself.
fn snap_block(c: &mut Composer) {
    let text = c.text();
    let at = c.cursor_offset();
    if at == line_end(&text, at) && at > line_start(&text, at) {
        c.move_to(previous_grapheme_offset(&text, at));
    }
}

pub(super) fn key(c: &mut Composer, key: KeyEvent, before: &str, at: usize) -> ComposerAction {
    let mut v = c.vim.expect("vim routing checked by caller");
    if v.mode == Mode::Insert {
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            v.mode = Mode::Normal;
            c.vim = Some(v);
            // Esc lands the caret on the last typed character, like Vim.
            if at > line_start(before, at) {
                c.move_to(previous_grapheme_offset(before, at));
            }
            return ComposerAction::None;
        }
        return c.emacs_key(key, before, at);
    }
    let action = normal(c, &mut v, key, before, at);
    c.vim = Some(v);
    if v.mode == Mode::Normal {
        snap_block(c);
    }
    action
}

fn normal(c: &mut Composer, v: &mut Vim, key: KeyEvent, before: &str, at: usize) -> ComposerAction {
    // Shift+<letter> arrives as the capital with SHIFT held; normalize so
    // every arm can test `plain` uniformly.
    let key = match key {
        KeyEvent {
            code: KeyCode::Char(ch),
            modifiers,
            ..
        } if modifiers == KeyModifiers::SHIFT && ch.is_ascii_alphabetic() => {
            KeyEvent::new(KeyCode::Char(ch.to_ascii_uppercase()), KeyModifiers::NONE)
        }
        _ => key,
    };
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let plain = key.modifiers.is_empty();

    // States that consume the next key outright.
    match v.pending {
        Pending::Replace => {
            v.clear_command();
            if let KeyCode::Char(ch) = key.code {
                let text = c.text();
                if plain && at < line_end(&text, at) {
                    c.delete_range(at..next_grapheme_offset(&text, at), false);
                    c.insert(&ch.to_string());
                    c.move_to(at);
                }
            }
            return ComposerAction::None;
        }
        Pending::Find { backward, till } => {
            v.pending = Pending::None;
            if let KeyCode::Char(ch) = key.code
                && plain
            {
                let find = Find { ch, backward, till };
                v.last_find = Some(find);
                return dispatch_motion(c, v, Motion::Find(find));
            }
            v.clear_command();
            return ComposerAction::None;
        }
        Pending::G => {
            v.pending = Pending::None;
            if key.code == KeyCode::Char('g') && plain {
                return dispatch_motion(c, v, Motion::DocStart);
            }
            v.clear_command();
            return ComposerAction::None;
        }
        Pending::None => {}
    }

    // Count prefixes accumulate for both `3w` and `d3w`; a bare `0` is a motion.
    if let KeyCode::Char(digit @ '0'..='9') = key.code
        && plain
    {
        if digit == '0' && v.count == 0 {
            return dispatch_motion(c, v, Motion::LineStart);
        }
        v.count = (v.count * 10 + digit as usize - '0' as usize).min(9999);
        return ComposerAction::None;
    }

    // Operators: doubled means whole lines, otherwise wait for a motion.
    if let KeyCode::Char(ch @ ('d' | 'c' | 'y')) = key.code
        && plain
    {
        let op = match ch {
            'd' => Op::Delete,
            'c' => Op::Change,
            _ => Op::Yank,
        };
        if v.op == Some(op) {
            v.op = None;
            let count = v.take_count();
            let text = c.text();
            let row = row_of(&text, c.cursor_offset());
            let (s, e) = rows_range(&text, row, row + count - 1);
            return linewise_op(c, v, op, s, e);
        }
        if v.op.is_some() {
            // `dy`-style operator pairs are not motions; abort the command.
            v.clear_command();
            return ComposerAction::None;
        }
        v.op = Some(op);
        v.op_count = v.count;
        v.count = 0;
        return ComposerAction::None;
    }

    let motion = match key.code {
        KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace if !ctrl => Some(Motion::Left),
        KeyCode::Char('l') | KeyCode::Right if !ctrl => Some(Motion::Right),
        KeyCode::Char('j') | KeyCode::Down if !ctrl => Some(Motion::Down),
        KeyCode::Char('k') | KeyCode::Up if !ctrl => Some(Motion::Up),
        KeyCode::Char('w') if plain => Some(Motion::WordForward { big: false }),
        KeyCode::Char('W') if plain => Some(Motion::WordForward { big: true }),
        KeyCode::Char('b') if plain => Some(Motion::WordBackward { big: false }),
        KeyCode::Char('B') if plain => Some(Motion::WordBackward { big: true }),
        KeyCode::Char('e') if plain => Some(Motion::WordEnd { big: false }),
        KeyCode::Char('E') if plain => Some(Motion::WordEnd { big: true }),
        KeyCode::Char('^') if plain => Some(Motion::FirstNonBlank),
        KeyCode::Char('$') if plain => Some(Motion::LineEnd),
        KeyCode::Char('G') if plain => Some(Motion::DocEnd),
        KeyCode::Char('{') if plain => Some(Motion::ParaBackward),
        KeyCode::Char('}') if plain => Some(Motion::ParaForward),
        KeyCode::Home if plain => Some(Motion::LineStart),
        KeyCode::End if plain => Some(Motion::LineEnd),
        _ => None,
    };
    if let Some(motion) = motion {
        return dispatch_motion(c, v, motion);
    }

    match key.code {
        KeyCode::Esc => {
            // A pending command dies quietly; bare Esc keeps the app-level
            // meaning — interrupt running work.
            if v.op.is_some() || v.count > 0 {
                v.clear_command();
                ComposerAction::None
            } else {
                ComposerAction::Cancel
            }
        }
        KeyCode::Enter if plain => {
            // Sends land back in a clean Insert state.
            v.sent();
            c.submit(before)
        }
        // Composer-level services that stay live in normal mode.
        KeyCode::Char('v') if ctrl => ComposerAction::Clipboard,
        KeyCode::Char('g') if ctrl => ComposerAction::Editor,
        KeyCode::Char('d') if ctrl && before.is_empty() => ComposerAction::Quit,
        // These keys can complete a pending operator (`df;`, `dgg`, `d;`).
        KeyCode::Char(';') if plain => match v.last_find {
            Some(find) => dispatch_motion(c, v, Motion::Find(find)),
            None => {
                v.clear_command();
                ComposerAction::None
            }
        },
        KeyCode::Char(',') if plain => match v.last_find {
            Some(mut find) => {
                find.backward = !find.backward;
                dispatch_motion(c, v, Motion::Find(find))
            }
            None => {
                v.clear_command();
                ComposerAction::None
            }
        },
        KeyCode::Char('f') if plain => {
            v.pending = Pending::Find {
                backward: false,
                till: false,
            };
            ComposerAction::None
        }
        KeyCode::Char('F') if plain => {
            v.pending = Pending::Find {
                backward: true,
                till: false,
            };
            ComposerAction::None
        }
        KeyCode::Char('t') if plain => {
            v.pending = Pending::Find {
                backward: false,
                till: true,
            };
            ComposerAction::None
        }
        KeyCode::Char('T') if plain => {
            v.pending = Pending::Find {
                backward: true,
                till: true,
            };
            ComposerAction::None
        }
        KeyCode::Char('g') if plain => {
            v.pending = Pending::G;
            ComposerAction::None
        }
        // Anything else cannot follow an operator; the command aborts and the
        // key is consumed, like `dx` in Vim.
        _ if v.op.is_some() => {
            v.clear_command();
            ComposerAction::None
        }
        // `u`/`Ctrl-R` ride the textarea's undo stack.
        KeyCode::Char('u') if plain => {
            v.clear_command();
            c.textarea.undo();
            ComposerAction::None
        }
        KeyCode::Char('r') if ctrl => {
            v.clear_command();
            c.textarea.redo();
            ComposerAction::None
        }
        KeyCode::Char('r') if plain => {
            v.pending = Pending::Replace;
            ComposerAction::None
        }
        KeyCode::Char('i') if plain => {
            v.clear_command();
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('a') if plain => {
            v.clear_command();
            let text = c.text();
            c.move_to(next_grapheme_offset(&text, at).min(line_end(&text, at)));
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('A') if plain => {
            v.clear_command();
            let text = c.text();
            c.move_to(line_end(&text, at));
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('I') if plain => {
            v.clear_command();
            let text = c.text();
            c.move_to(first_non_blank(&text, at));
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('o') if plain => open_line(c, v, true),
        KeyCode::Char('O') if plain => open_line(c, v, false),
        // `s` deletes the next `count` graphemes on the line, then inserts.
        KeyCode::Char('s') if plain => {
            let count = v.take_count();
            let text = c.text();
            let end = line_end(&text, at);
            let mut to = at;
            for _ in 0..count {
                to = next_grapheme_offset(&text, to).min(end);
            }
            c.delete_range(at..to, true);
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('S') if plain => {
            let count = v.take_count();
            let text = c.text();
            let row = row_of(&text, at);
            let (s, e) = rows_range(&text, row, row + count - 1);
            linewise_op(c, v, Op::Change, s, e)
        }
        // `x`/`X` cut `count` graphemes forward/back, clipped to the line.
        KeyCode::Char('x') | KeyCode::Delete if !ctrl => {
            let count = v.take_count();
            let text = c.text();
            let end = line_end(&text, at);
            let mut to = at;
            for _ in 0..count {
                to = next_grapheme_offset(&text, to).min(end);
            }
            c.delete_range(at..to, true);
            ComposerAction::None
        }
        KeyCode::Char('X') if plain => {
            let count = v.take_count();
            let text = c.text();
            let mut from = at;
            for _ in 0..count {
                from = previous_grapheme_offset(&text, from);
            }
            c.delete_range(from..at, true);
            ComposerAction::None
        }
        // `D`/`C`/`Y` are `d$`, `c$`, and linewise `yy`.
        KeyCode::Char('D') if plain => {
            v.clear_command();
            let text = c.text();
            let end = line_end(&text, at);
            c.delete_range(at..end, true);
            ComposerAction::None
        }
        KeyCode::Char('C') if plain => {
            v.clear_command();
            let text = c.text();
            let end = line_end(&text, at);
            c.delete_range(at..end, true);
            v.mode = Mode::Insert;
            ComposerAction::None
        }
        KeyCode::Char('Y') if plain => {
            let count = v.take_count();
            let text = c.text();
            let row = row_of(&text, at);
            let (s, e) = rows_range(&text, row, row + count - 1);
            linewise_op(c, v, Op::Yank, s, e)
        }
        KeyCode::Char('J') if plain => join_lines(c, v),
        KeyCode::Char('p') if plain => paste(c, v, false),
        KeyCode::Char('P') if plain => paste(c, v, true),
        // `/` re-enters insert already holding the slash, which opens the
        // command menu exactly as typing it does.
        KeyCode::Char('/') if plain => {
            v.clear_command();
            v.mode = Mode::Insert;
            c.emacs_key(key, before, at)
        }
        _ => {
            v.clear_command();
            ComposerAction::None
        }
    }
}
