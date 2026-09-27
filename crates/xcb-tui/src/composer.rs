use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::{CursorMove, DataCursor, TextArea};
use std::{collections::VecDeque, ops::Range};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

mod vim;
pub use vim::Mode as VimMode;

pub const MAX_INPUT: usize = 256 * 1024;
pub const MAX_HISTORY: usize = 200;
pub const MAX_HISTORY_BYTES: usize = 1024 * 1024;
/// Refuse a paste whole rather than silently truncating it.
pub const PASTE_TOO_LARGE: &str = "Paste exceeds 256 KiB; attach a file or trim it";
pub const INPUT_TOO_LARGE: &str = "Input exceeds 256 KiB; attach a file or trim it";

/// The most any key the editor library handles itself can add (a tab stop or
/// a newline), so the fallback path only snapshots for rollback near the limit.
const FALLBACK_GROWTH: usize = 64;

#[cfg(test)]
thread_local! {
    /// Whole-draft joins, so tests can prove ordinary keystrokes never pay
    /// for one.
    pub(crate) static JOINS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub enum ComposerAction {
    None,
    Submit(String),
    Cancel,
    Quit,
    Clipboard,
    History,
    Editor,
    Rejected(&'static str),
}

/// A draft position: a row and a byte offset into that row's line. Keys work
/// on the cursor's line; graphemes and word pieces never span a newline.
type Pos = (usize, usize);

#[derive(Default)]
pub struct Composer {
    pub textarea: TextArea<'static>,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
    kill_buffer: String,
    /// Whether `kill_buffer` holds whole lines, which makes `p`/`P` open a
    /// line instead of splicing text at the cursor. Vim linewise yanks set it;
    /// every other kill clears it.
    kill_linewise: bool,
    vim: Option<vim::Vim>,
}
impl Composer {
    /// The whole draft. Joining costs the full draft, so per-key paths use
    /// the cheaper accessors below instead.
    pub fn text(&self) -> String {
        #[cfg(test)]
        JOINS.with(|joins| joins.set(joins.get() + 1));
        self.textarea.lines().join("\n")
    }
    /// Byte length `text()` would return, without joining.
    pub fn len(&self) -> usize {
        let lines = self.textarea.lines();
        lines.iter().map(String::len).sum::<usize>() + lines.len().saturating_sub(1)
    }
    pub fn is_empty(&self) -> bool {
        self.textarea.is_empty()
    }
    /// The draft's first line: slash commands only ever need this much.
    pub fn first_line(&self) -> &str {
        self.line(0)
    }
    /// The whole draft when it is a single line of at most `max` bytes.
    pub fn short_text(&self, max: usize) -> Option<&str> {
        match self.textarea.lines() {
            [line] if line.len() <= max => Some(line),
            _ => None,
        }
    }
    pub fn set_text(&mut self, text: &str) {
        if text.len() <= MAX_INPUT {
            self.replace_text(&clean_text(text));
            self.reset_history();
        }
    }
    fn replace_text(&mut self, text: &str) {
        self.textarea = TextArea::from(text.split('\n').map(str::to_owned));
        self.textarea.move_cursor(CursorMove::Bottom);
        self.textarea.move_cursor(CursorMove::End);
    }
    fn reset_history(&mut self) {
        self.history_index = None;
        self.draft.clear();
    }
    /// Remember a complete prompt, newest first. Both count and total bytes are bounded.
    pub fn remember(&mut self, text: &str) {
        if text.len() > MAX_INPUT {
            return;
        }
        let text = clean_text(text);
        if !text.trim().is_empty() && self.history.front() != Some(&text) {
            self.history.push_front(text);
            self.history.truncate(MAX_HISTORY);
            let mut bytes: usize = self.history.iter().map(String::len).sum();
            while bytes > MAX_HISTORY_BYTES {
                bytes -= self
                    .history
                    .pop_back()
                    .expect("history over its byte limit is nonempty")
                    .len();
            }
            self.reset_history();
        }
    }
    /// Replace history from a newest-first snapshot; reject oversized individual entries.
    pub fn restore_history(&mut self, history: impl IntoIterator<Item = String>) {
        self.history.clear();
        let mut bytes = 0;
        for text in history.into_iter().take(MAX_HISTORY) {
            if text.len() > MAX_INPUT {
                continue;
            }
            let text = clean_text(&text);
            if !text.trim().is_empty() && bytes + text.len() <= MAX_HISTORY_BYTES {
                bytes += text.len();
                self.history.push_back(text);
            }
        }
        self.reset_history();
    }
    pub fn clear_to_history(&mut self) {
        self.remember(&self.text());
        self.set_text("");
    }
    pub fn history(&self) -> impl Iterator<Item = &String> {
        self.history.iter()
    }
    /// Explicit recall (including Ctrl-R selection) saves the draft for subsequent Down.
    pub fn recall_history(&mut self, index: usize) -> bool {
        let Some(text) = self.history.get(index).cloned() else {
            return false;
        };
        if self.history_index.is_none() {
            self.draft = self.text();
        }
        self.replace_text(&text);
        self.history_index = Some(index);
        true
    }
    pub fn previous_history(&mut self) -> bool {
        self.recall_history(self.history_index.map_or(0, |index| index + 1))
    }
    pub fn next_history(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };
        if index == 0 {
            self.replace_text(&self.draft.clone());
            self.reset_history();
            true
        } else {
            self.recall_history(index - 1)
        }
    }
    /// Whether the draft is exactly `text`, compared line by line.
    fn text_equals(&self, text: &str) -> bool {
        text.len() == self.len()
            && text
                .split('\n')
                .eq(self.textarea.lines().iter().map(String::as_str))
    }
    fn can_navigate_history(&self) -> bool {
        if self.textarea.selection_range().is_some() {
            return false;
        }
        if self.is_empty() {
            return true;
        }
        // Only an unchanged recall browses on; the index is cleared by any
        // edit, so a fresh draft never reaches the comparison.
        let Some(entry) = self.history_index.and_then(|index| self.history.get(index)) else {
            return false;
        };
        let (row, byte) = self.pos();
        let last = self.textarea.lines().len() - 1;
        let edge = (row, byte) == (0, 0) || (row == last && byte == self.line(row).len());
        edge && self.text_equals(entry)
    }
    fn line(&self, row: usize) -> &str {
        self.textarea.lines().get(row).map_or("", String::as_str)
    }
    /// The editor library's `(row, character column)` as a byte position.
    fn char_pos(&self, (row, column): (usize, usize)) -> Pos {
        (row, byte_of_column(self.line(row), column))
    }
    fn pos(&self) -> Pos {
        let DataCursor(row, column) = self.textarea.cursor();
        self.char_pos((row, column))
    }
    fn cursor_offset(&self) -> usize {
        let (row, byte) = self.pos();
        self.textarea.lines()[..row]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + byte
    }
    /// The position of an absolute byte offset into `text()`.
    fn offset_pos(&self, mut offset: usize) -> Pos {
        let lines = self.textarea.lines();
        for (row, line) in lines.iter().enumerate() {
            if offset <= line.len() {
                return (row, offset);
            }
            offset -= line.len() + 1;
        }
        let row = lines.len() - 1;
        (row, lines[row].len())
    }
    fn move_to(&mut self, offset: usize) {
        self.jump(self.offset_pos(offset));
    }
    fn jump(&mut self, (row, byte): Pos) {
        let column = self.line(row)[..byte].chars().count();
        // Jump uses u16 coordinates, while valid 256 KiB input can exceed either axis.
        self.textarea.move_cursor(CursorMove::Jump(
            row.min(u16::MAX as usize) as u16,
            column.min(u16::MAX as usize) as u16,
        ));
        if row > u16::MAX as usize {
            self.textarea.move_cursor(CursorMove::Bottom);
            for _ in row + 1..self.textarea.lines().len() {
                self.textarea.move_cursor(CursorMove::Up);
            }
            self.textarea.move_cursor(CursorMove::Head);
            for _ in 0..column {
                self.textarea.move_cursor(CursorMove::Forward);
            }
        } else if column > u16::MAX as usize {
            let from_end = self.line(row).chars().count() - column;
            if from_end < column - u16::MAX as usize {
                self.textarea.move_cursor(CursorMove::End);
                for _ in 0..from_end {
                    self.textarea.move_cursor(CursorMove::Back);
                }
            } else {
                for _ in u16::MAX as usize..column {
                    self.textarea.move_cursor(CursorMove::Forward);
                }
            }
        }
    }
    /// Keep the cursor off the inside of a grapheme cluster and return where
    /// it rests. Clusters never span a newline, so the cursor's line decides.
    fn snap_cursor(&mut self) -> Pos {
        let (row, byte) = self.pos();
        let safe = grapheme_floor(self.line(row), byte);
        if safe != byte {
            self.jump((row, safe));
        }
        (row, safe)
    }
    fn previous_grapheme_pos(&self, (row, byte): Pos) -> Pos {
        if byte > 0 {
            (row, grapheme_before(self.line(row), byte))
        } else if row > 0 {
            (row - 1, self.line(row - 1).len())
        } else {
            (0, 0)
        }
    }
    fn next_grapheme_pos(&self, (row, byte): Pos) -> Pos {
        let line = self.line(row);
        if byte < line.len() {
            (row, grapheme_after(line, byte))
        } else if row + 1 < self.textarea.lines().len() {
            (row + 1, 0)
        } else {
            (row, byte)
        }
    }
    /// Start of the word before `pos`, skipping back over blank lines.
    fn previous_word_pos(&self, (mut row, mut end): Pos) -> Pos {
        loop {
            let prefix = self.line(row)[..end].trim_end_matches(char::is_whitespace);
            if !prefix.is_empty() {
                return (row, previous_word(prefix, prefix.len()));
            }
            if row == 0 {
                return (0, 0);
            }
            row -= 1;
            end = self.line(row).len();
        }
    }
    /// End of the word after `pos`, skipping forward over blank lines.
    fn next_word_pos(&self, (mut row, mut start): Pos) -> Pos {
        let last = self.textarea.lines().len() - 1;
        loop {
            let rest = &self.line(row)[start..];
            if !rest.trim_start_matches(char::is_whitespace).is_empty() {
                return (row, start + next_word(rest, 0));
            }
            if row == last {
                return (row, self.line(row).len());
            }
            row += 1;
            start = 0;
        }
    }
    /// The draft between two positions, `start <= end`.
    fn span_text(&self, (r1, b1): Pos, (r2, b2): Pos) -> String {
        let lines = self.textarea.lines();
        if r1 == r2 {
            return lines[r1][b1..b2].to_owned();
        }
        let mut text = lines[r1][b1..].to_owned();
        for line in &lines[r1 + 1..r2] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&lines[r2][..b2]);
        text
    }
    fn span_bytes(&self, (r1, b1): Pos, (r2, b2): Pos) -> usize {
        let lines = self.textarea.lines();
        if r1 == r2 {
            return b2 - b1;
        }
        lines[r1].len() - b1
            + lines[r1 + 1..r2]
                .iter()
                .map(|line| line.len() + 1)
                .sum::<usize>()
            + 1
            + b2
    }
    /// Characters between two positions, counting each newline as one, the
    /// unit the editor library deletes in.
    fn span_chars(&self, (r1, b1): Pos, (r2, b2): Pos) -> usize {
        let lines = self.textarea.lines();
        if r1 == r2 {
            return lines[r1][b1..b2].chars().count();
        }
        lines[r1][b1..].chars().count()
            + lines[r1 + 1..r2]
                .iter()
                .map(|line| line.chars().count() + 1)
                .sum::<usize>()
            + 1
            + lines[r2][..b2].chars().count()
    }
    fn delete_span(&mut self, start: Pos, end: Pos, kill: bool) {
        if let Some((from, to)) = self.textarea.selection_range() {
            if kill {
                self.kill_buffer = self.span_text(self.char_pos(from), self.char_pos(to));
                self.kill_linewise = false;
            }
            self.textarea.delete_str(0);
            return;
        }
        if start >= end {
            return;
        }
        if kill {
            self.kill_buffer = self.span_text(start, end);
            self.kill_linewise = false;
        }
        let chars = self.span_chars(start, end);
        self.jump(start);
        self.textarea.delete_str(chars);
    }
    fn delete_range(&mut self, range: Range<usize>, kill: bool) {
        self.delete_span(
            self.offset_pos(range.start),
            self.offset_pos(range.end),
            kill,
        );
    }
    fn insert(&mut self, text: &str) -> bool {
        let selected = self.textarea.selection_range().map_or(0, |(from, to)| {
            self.span_bytes(self.char_pos(from), self.char_pos(to))
        });
        if (self.len() - selected).saturating_add(text.len()) > MAX_INPUT {
            return false;
        }
        self.textarea.insert_str(text);
        true
    }
    fn prepare_selection(&mut self, extend: bool) {
        if extend {
            if !self.textarea.is_selecting() {
                self.textarea.start_selection();
            }
        } else {
            self.textarea.cancel_selection();
        }
    }
    /// `/vim` toggles modal editing for this composer. Returns the new state.
    pub fn toggle_vim(&mut self) -> bool {
        self.vim = if self.vim.is_some() {
            None
        } else {
            Some(vim::Vim::default())
        };
        self.vim.is_some()
    }
    /// `Some(mode)` while Vim editing is on, for the mode indicator.
    pub fn vim_mode(&self) -> Option<VimMode> {
        self.vim.map(|vim| vim.mode)
    }
    /// Shared by plain Enter in both keymaps: record, clear, hand the text back.
    fn submit(&mut self) -> ComposerAction {
        let text = self.text();
        self.remember(&text);
        self.set_text("");
        // A send always returns Vim editing to insert mode.
        if let Some(vim) = &mut self.vim {
            vim.sent();
        }
        ComposerAction::Submit(text)
    }
    /// The default keymap: Emacs-style readline editing, from the snapped
    /// cursor position `at`. Vim's insert mode delegates here so the ordinary
    /// keys behave identically.
    fn emacs_key(&mut self, key: KeyEvent, at: Pos) -> ComposerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                self.clear_to_history();
                return ComposerAction::Cancel;
            }
            KeyCode::Esc => return ComposerAction::Cancel,
            KeyCode::Char('d') if ctrl && self.is_empty() => return ComposerAction::Quit,
            KeyCode::Char('v') if ctrl => return ComposerAction::Clipboard,
            KeyCode::Char('r') if ctrl => return ComposerAction::History,
            KeyCode::Char('g') if ctrl => return ComposerAction::Editor,
            KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                if !self.insert("\n") {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
            }
            KeyCode::Char('j') if ctrl => {
                if !self.insert("\n") {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Up | KeyCode::Char('p') if key.code == KeyCode::Up || ctrl => {
                if !shift && self.can_navigate_history() && self.previous_history() {
                    return ComposerAction::None;
                }
                self.prepare_selection(shift);
                self.textarea.move_cursor(CursorMove::Up);
                self.snap_cursor();
            }
            KeyCode::Down | KeyCode::Char('n') if key.code == KeyCode::Down || ctrl => {
                if !shift && self.can_navigate_history() && self.next_history() {
                    return ComposerAction::None;
                }
                self.prepare_selection(shift);
                self.textarea.move_cursor(CursorMove::Down);
                self.snap_cursor();
            }
            KeyCode::Left | KeyCode::Char('b') if key.code == KeyCode::Left || ctrl || alt => {
                let next = if alt || (ctrl && key.code == KeyCode::Left) {
                    self.previous_word_pos(at)
                } else {
                    self.previous_grapheme_pos(at)
                };
                self.prepare_selection(shift);
                self.jump(next);
            }
            KeyCode::Right | KeyCode::Char('f') if key.code == KeyCode::Right || ctrl || alt => {
                let next = if alt || (ctrl && key.code == KeyCode::Right) {
                    self.next_word_pos(at)
                } else {
                    self.next_grapheme_pos(at)
                };
                self.prepare_selection(shift);
                self.jump(next);
            }
            KeyCode::Home | KeyCode::Char('a') if key.code == KeyCode::Home || ctrl => {
                self.prepare_selection(shift);
                self.jump((at.0, 0));
            }
            KeyCode::End | KeyCode::Char('e') if key.code == KeyCode::End || ctrl => {
                self.prepare_selection(shift);
                self.jump((at.0, self.line(at.0).len()));
            }
            KeyCode::Char('u') if ctrl => {
                // At a line start, Ctrl-U joins the line onto the previous one.
                let start = if at.1 == 0 {
                    self.previous_grapheme_pos(at)
                } else {
                    (at.0, 0)
                };
                self.delete_span(start, at, true);
            }
            KeyCode::Char('k') if ctrl => {
                // At a line end, Ctrl-K joins the next line onto this one.
                let end = if at.1 == self.line(at.0).len() {
                    self.next_grapheme_pos(at)
                } else {
                    (at.0, self.line(at.0).len())
                };
                self.delete_span(at, end, true);
            }
            KeyCode::Char('w') if ctrl => self.delete_span(self.previous_word_pos(at), at, true),
            KeyCode::Backspace if alt || ctrl => {
                self.delete_span(self.previous_word_pos(at), at, true)
            }
            KeyCode::Char('h') if ctrl && alt => {
                self.delete_span(self.previous_word_pos(at), at, true)
            }
            KeyCode::Char('d') if alt => self.delete_span(at, self.next_word_pos(at), true),
            KeyCode::Delete if ctrl || alt => self.delete_span(at, self.next_word_pos(at), true),
            KeyCode::Backspace | KeyCode::Char('h') if key.code == KeyCode::Backspace || ctrl => {
                self.delete_span(self.previous_grapheme_pos(at), at, false)
            }
            KeyCode::Delete | KeyCode::Char('d') if key.code == KeyCode::Delete || ctrl => {
                self.delete_span(at, self.next_grapheme_pos(at), false)
            }
            KeyCode::Char('y') if ctrl => {
                if !self.insert(&self.kill_buffer.clone()) {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
            }
            KeyCode::Char(ch) if !ctrl && !alt => {
                if !self.insert(&clean_text(&ch.to_string())) {
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
            }
            // TextArea reads Shift-Tab as Tab and would insert one.
            KeyCode::BackTab => {}
            _ => {
                let rollback =
                    (self.len() + FALLBACK_GROWTH > MAX_INPUT).then(|| self.textarea.clone());
                self.textarea.input(key);
                if self.len() > MAX_INPUT {
                    match rollback {
                        Some(old) => self.textarea = old,
                        None => {
                            self.textarea.undo();
                        }
                    }
                    return ComposerAction::Rejected(INPUT_TOO_LARGE);
                }
                self.snap_cursor();
            }
        }
        ComposerAction::None
    }
    pub fn handle(&mut self, event: Event) -> ComposerAction {
        let action = match event {
            Event::Paste(text) => {
                if text.len() > MAX_INPUT || !self.insert(&clean_text(&text)) {
                    return ComposerAction::Rejected(PASTE_TOO_LARGE);
                }
                ComposerAction::None
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let at = self.snap_cursor();
                if self.vim.is_some() {
                    vim::key(self, key, at)
                } else {
                    self.emacs_key(key, at)
                }
            }
            _ => ComposerAction::None,
        };
        // Any edit ends history browsing; a recall keeps the index it just
        // set because the draft still equals that entry.
        if self.history_index.is_some_and(|index| {
            !self
                .history
                .get(index)
                .is_some_and(|entry| self.text_equals(entry))
        }) {
            self.reset_history();
        }
        action
    }
}

fn clean_text(text: &str) -> String {
    xcb_core::display_text(&text.replace("\r\n", "\n").replace('\r', "\n"), MAX_INPUT)
}
/// Byte offset of a character column, clamped to the line end.
fn byte_of_column(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(i, _)| i)
}
/// The grapheme boundary at or before `at` within one line. Cursor-local, so
/// the cost does not grow with the line.
fn grapheme_floor(line: &str, at: usize) -> usize {
    if GraphemeCursor::new(at, line.len(), true)
        .is_boundary(line, 0)
        .unwrap_or(true)
    {
        at
    } else {
        grapheme_before(line, at)
    }
}
fn grapheme_before(line: &str, at: usize) -> usize {
    GraphemeCursor::new(at, line.len(), true)
        .prev_boundary(line, 0)
        .ok()
        .flatten()
        .unwrap_or(0)
}
fn grapheme_after(line: &str, at: usize) -> usize {
    GraphemeCursor::new(at, line.len(), true)
        .next_boundary(line, 0)
        .ok()
        .flatten()
        .unwrap_or(line.len())
}
// Unicode word boundaries keep CJK, punctuation and combining sequences intact.
fn previous_word(text: &str, at: usize) -> usize {
    let prefix = text[..at].trim_end_matches(char::is_whitespace);
    // Segment backwards, so the cost is the words crossed, not the line.
    let mut pieces = prefix
        .split_word_bound_indices()
        .rev()
        .flat_map(|(offset, word)| separator_runs(offset, word).into_iter().rev());
    let Some((mut start, piece)) = pieces.next() else {
        return 0;
    };
    if separator(piece) {
        for (index, piece) in pieces {
            if !separator(piece) {
                break;
            }
            start = index;
        }
    }
    start
}
fn next_word(text: &str, at: usize) -> usize {
    let suffix = &text[at..];
    let skipped = suffix.len() - suffix.trim_start_matches(char::is_whitespace).len();
    let mut pieces = suffix[skipped..]
        .split_word_bound_indices()
        .flat_map(|(offset, word)| separator_runs(offset, word));
    let Some((_, piece)) = pieces.next() else {
        return text.len();
    };
    let mut end = at + skipped + piece.len();
    if separator(piece) {
        for (index, piece) in pieces {
            if !separator(piece) {
                break;
            }
            end = at + skipped + index + piece.len();
        }
    }
    end
}
fn separator(piece: &str) -> bool {
    piece
        .chars()
        .all(|ch| "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?".contains(ch))
}

/// One word-bound segment split where separators meet other text, with
/// offsets relative to the segmented text.
fn separator_runs(offset: usize, word: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut previous = None;
    for (index, grapheme) in word.grapheme_indices(true) {
        let kind = separator(grapheme);
        if previous.is_some_and(|last| last != kind) {
            pieces.push((offset + start, &word[start..index]));
            start = index;
        }
        previous = Some(kind);
    }
    pieces.push((offset + start, &word[start..]));
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    // The whole-text algorithms the line-local motions replaced; the
    // line-local versions must land on exactly the same offsets.
    fn reference_snap(text: &str, offset: usize) -> usize {
        text.grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .take_while(|i| *i <= offset)
            .last()
            .unwrap_or(0)
    }
    fn reference_previous_grapheme(text: &str, at: usize) -> usize {
        text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(i, _)| i)
    }
    fn reference_next_grapheme(text: &str, at: usize) -> usize {
        at + text[at..].graphemes(true).next().map_or(0, str::len)
    }
    fn reference_pieces(text: &str) -> Vec<(usize, &str)> {
        text.split_word_bound_indices()
            .flat_map(|(offset, word)| separator_runs(offset, word))
            .collect()
    }
    fn reference_previous_word(text: &str, at: usize) -> usize {
        let prefix = text[..at].trim_end_matches(char::is_whitespace);
        let mut pieces = reference_pieces(prefix).into_iter().rev();
        let Some((mut start, piece)) = pieces.next() else {
            return 0;
        };
        if separator(piece) {
            for (index, piece) in pieces {
                if !separator(piece) {
                    break;
                }
                start = index;
            }
        }
        start
    }
    fn reference_next_word(text: &str, at: usize) -> usize {
        let suffix = &text[at..];
        let skipped = suffix.len() - suffix.trim_start_matches(char::is_whitespace).len();
        let mut pieces = reference_pieces(&suffix[skipped..]).into_iter();
        let Some((_, piece)) = pieces.next() else {
            return text.len();
        };
        let mut end = at + skipped + piece.len();
        if separator(piece) {
            for (index, piece) in pieces {
                if !separator(piece) {
                    break;
                }
                end = at + skipped + index + piece.len();
            }
        }
        end
    }

    /// Deterministic drafts mixing combining marks, emoji sequences, flags,
    /// CJK, Indic conjuncts, punctuation runs, blanks and newlines.
    fn samples() -> Vec<String> {
        const ATOMS: &[&str] = &[
            "a",
            "Z",
            "9",
            "_",
            " ",
            "  ",
            "\t",
            "\n",
            "\n\n",
            " \n ",
            ".",
            "--",
            "::",
            "'",
            "é",
            "e\u{301}",
            "👩\u{200d}💻",
            "👍🏽",
            "🇵🇷",
            "🇺",
            "界",
            "かな",
            "ह\u{93f}",
            "क\u{94d}ष",
            "\u{200d}",
            "can't",
            "3.14",
            "a.b",
            "foo/bar",
            "(x)",
        ];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize
        };
        (0..600)
            .map(|_| {
                (0..next() % 28)
                    .map(|_| ATOMS[next() % ATOMS.len()])
                    .collect()
            })
            .collect()
    }
    fn offset_of(composer: &Composer, (row, byte): Pos) -> usize {
        composer.textarea.lines()[..row]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + byte
    }

    #[test]
    fn line_local_motions_match_the_whole_text_algorithms() {
        for sample in samples() {
            let mut composer = Composer::default();
            composer.set_text(&sample);
            let text = composer.text();
            let at = |offset| composer.offset_pos(offset);
            for (offset, _) in text.char_indices().chain([(text.len(), ' ')]) {
                let (row, byte) = at(offset);
                assert_eq!(
                    offset_of(&composer, (row, grapheme_floor(composer.line(row), byte))),
                    reference_snap(&text, offset),
                    "snap {text:?} at {offset}"
                );
            }
            let boundaries = text
                .grapheme_indices(true)
                .map(|(i, _)| i)
                .chain([text.len()]);
            for offset in boundaries {
                let pos = at(offset);
                for (moved, expected, motion) in [
                    (
                        composer.previous_grapheme_pos(pos),
                        reference_previous_grapheme(&text, offset),
                        "previous grapheme",
                    ),
                    (
                        composer.next_grapheme_pos(pos),
                        reference_next_grapheme(&text, offset),
                        "next grapheme",
                    ),
                    (
                        composer.previous_word_pos(pos),
                        reference_previous_word(&text, offset),
                        "previous word",
                    ),
                    (
                        composer.next_word_pos(pos),
                        reference_next_word(&text, offset),
                        "next word",
                    ),
                ] {
                    assert_eq!(
                        offset_of(&composer, moved),
                        expected,
                        "{motion} {text:?} at {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn spans_measure_the_same_text_a_join_would_slice() {
        for sample in samples().into_iter().take(120) {
            let mut composer = Composer::default();
            composer.set_text(&sample);
            let text = composer.text();
            let offsets: Vec<_> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            for (index, &start) in offsets.iter().enumerate() {
                for &end in &offsets[index..] {
                    let (from, to) = (composer.offset_pos(start), composer.offset_pos(end));
                    assert_eq!(composer.span_text(from, to), text[start..end]);
                    assert_eq!(composer.span_bytes(from, to), end - start);
                    assert_eq!(
                        composer.span_chars(from, to),
                        text[start..end].chars().count()
                    );
                }
            }
            assert_eq!(composer.len(), text.len());
            assert!(composer.text_equals(&text));
        }
    }

    #[test]
    fn keystrokes_on_a_full_draft_never_join_the_whole_draft() {
        let row = format!("{}\n", "word ".repeat(12).trim_end());
        let mut composer = Composer::default();
        composer.set_text(&row.repeat((MAX_INPUT - 64) / row.len()));
        let keys = [
            (KeyCode::Char('x'), KeyModifiers::NONE),
            (KeyCode::Backspace, KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
            (KeyCode::Left, KeyModifiers::NONE),
            (KeyCode::Right, KeyModifiers::NONE),
            (KeyCode::Char('b'), KeyModifiers::ALT),
            (KeyCode::Char('f'), KeyModifiers::ALT),
            (KeyCode::Home, KeyModifiers::NONE),
            (KeyCode::End, KeyModifiers::NONE),
            (KeyCode::Char('w'), KeyModifiers::CONTROL),
            (KeyCode::Char('y'), KeyModifiers::CONTROL),
            (KeyCode::Char('k'), KeyModifiers::CONTROL),
            (KeyCode::Char('u'), KeyModifiers::CONTROL),
            (KeyCode::Delete, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Tab, KeyModifiers::NONE),
        ];
        JOINS.with(|joins| joins.set(0));
        for (code, modifiers) in keys {
            composer.handle(Event::Key(KeyEvent::new(code, modifiers)));
        }
        composer.handle(Event::Paste("pasted".into()));
        assert_eq!(JOINS.with(std::cell::Cell::get), 0);
    }
}
