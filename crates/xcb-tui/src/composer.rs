use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_textarea::{CursorMove, DataCursor, TextArea};
use std::{collections::VecDeque, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_INPUT: usize = 256 * 1024;
pub const MAX_HISTORY: usize = 200;
pub const MAX_HISTORY_BYTES: usize = 1024 * 1024;
/// Refuse a paste whole rather than silently truncating it.
pub const PASTE_TOO_LARGE: &str = "Paste exceeds 256 KiB; attach a file or trim it";
pub const INPUT_TOO_LARGE: &str = "Input exceeds 256 KiB; attach a file or trim it";

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

#[derive(Default)]
pub struct Composer {
    pub textarea: TextArea<'static>,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
    kill_buffer: String,
}
impl Composer {
    pub fn text(&self) -> String {
        self.textarea.lines().join("\n")
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
                    .expect("nonempty bounded history")
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
    fn can_navigate_history(&self) -> bool {
        if self.textarea.selection_range().is_some() {
            return false;
        }
        let text = self.text();
        text.is_empty()
            || ((self.cursor_offset() == 0 || self.cursor_offset() == text.len())
                && self.history_index.and_then(|index| self.history.get(index)) == Some(&text))
    }
    fn cursor_offset(&self) -> usize {
        let DataCursor(row, column) = self.textarea.cursor();
        self.position_offset((row, column))
    }
    fn position_offset(&self, (row, column): (usize, usize)) -> usize {
        let lines = self.textarea.lines();
        lines[..row]
            .iter()
            .map(|line| line.len() + 1)
            .sum::<usize>()
            + lines[row]
                .char_indices()
                .nth(column)
                .map_or(lines[row].len(), |(i, _)| i)
    }
    fn move_to(&mut self, offset: usize) {
        let text = self.text();
        let prefix = &text[..offset.min(text.len())];
        let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count();
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
            let from_end = self.textarea.lines()[row].chars().count() - column;
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
    fn snap_cursor(&mut self) {
        let text = self.text();
        let offset = self.cursor_offset();
        let safe = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .take_while(|i| *i <= offset)
            .last()
            .unwrap_or(0);
        if safe != offset {
            self.move_to(safe);
        }
    }
    fn delete_range(&mut self, range: Range<usize>, kill: bool) {
        if let Some((start, end)) = self.textarea.selection_range() {
            if kill {
                self.kill_buffer =
                    self.text()[self.position_offset(start)..self.position_offset(end)].to_owned();
            }
            self.textarea.delete_str(0);
            return;
        }
        if range.is_empty() {
            return;
        }
        let text = self.text();
        if kill {
            self.kill_buffer = text[range.clone()].to_owned();
        }
        self.move_to(range.start);
        self.textarea.delete_str(text[range].chars().count());
    }
    fn insert(&mut self, text: &str) -> bool {
        let selected = self.textarea.selection_range().map_or(0, |(start, end)| {
            self.position_offset(end) - self.position_offset(start)
        });
        if (self.text().len() - selected).saturating_add(text.len()) > MAX_INPUT {
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
    pub fn handle(&mut self, event: Event) -> ComposerAction {
        let before = self.text();
        match event {
            Event::Paste(text) => {
                if text.len() > MAX_INPUT || !self.insert(&clean_text(&text)) {
                    return ComposerAction::Rejected(PASTE_TOO_LARGE);
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let alt = key.modifiers.contains(KeyModifiers::ALT);
                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                self.snap_cursor();
                let at = self.cursor_offset();
                match key.code {
                    KeyCode::Char('c') if ctrl => {
                        self.clear_to_history();
                        return ComposerAction::Cancel;
                    }
                    KeyCode::Esc => return ComposerAction::Cancel,
                    KeyCode::Char('d') if ctrl && before.is_empty() => return ComposerAction::Quit,
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
                    KeyCode::Enter => {
                        self.remember(&before);
                        self.set_text("");
                        return ComposerAction::Submit(before);
                    }
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
                    KeyCode::Left | KeyCode::Char('b')
                        if key.code == KeyCode::Left || ctrl || alt =>
                    {
                        let next = if alt || (ctrl && key.code == KeyCode::Left) {
                            previous_word(&before, at)
                        } else {
                            previous_grapheme(&before, at)
                        };
                        self.prepare_selection(shift);
                        self.move_to(next);
                    }
                    KeyCode::Right | KeyCode::Char('f')
                        if key.code == KeyCode::Right || ctrl || alt =>
                    {
                        let next = if alt || (ctrl && key.code == KeyCode::Right) {
                            next_word(&before, at)
                        } else {
                            next_grapheme(&before, at)
                        };
                        self.prepare_selection(shift);
                        self.move_to(next);
                    }
                    KeyCode::Home | KeyCode::Char('a') if key.code == KeyCode::Home || ctrl => {
                        self.prepare_selection(shift);
                        self.move_to(before[..at].rfind('\n').map_or(0, |i| i + 1));
                    }
                    KeyCode::End | KeyCode::Char('e') if key.code == KeyCode::End || ctrl => {
                        self.prepare_selection(shift);
                        self.move_to(before[at..].find('\n').map_or(before.len(), |i| at + i));
                    }
                    KeyCode::Char('u') if ctrl => {
                        let start = before[..at].rfind('\n').map_or(0, |i| i + 1);
                        self.delete_range(
                            if start == at {
                                at.saturating_sub(1)..at
                            } else {
                                start..at
                            },
                            true,
                        );
                    }
                    KeyCode::Char('k') if ctrl => {
                        let end = before[at..].find('\n').map_or(before.len(), |i| at + i);
                        self.delete_range(
                            at..if at == end {
                                (end + 1).min(before.len())
                            } else {
                                end
                            },
                            true,
                        );
                    }
                    KeyCode::Char('w') if ctrl => {
                        self.delete_range(previous_word(&before, at)..at, true)
                    }
                    KeyCode::Backspace if alt || ctrl => {
                        self.delete_range(previous_word(&before, at)..at, true)
                    }
                    KeyCode::Char('h') if ctrl && alt => {
                        self.delete_range(previous_word(&before, at)..at, true)
                    }
                    KeyCode::Char('d') if alt => {
                        self.delete_range(at..next_word(&before, at), true)
                    }
                    KeyCode::Delete if ctrl || alt => {
                        self.delete_range(at..next_word(&before, at), true)
                    }
                    KeyCode::Backspace | KeyCode::Char('h')
                        if key.code == KeyCode::Backspace || ctrl =>
                    {
                        self.delete_range(previous_grapheme(&before, at)..at, false)
                    }
                    KeyCode::Delete | KeyCode::Char('d') if key.code == KeyCode::Delete || ctrl => {
                        self.delete_range(at..next_grapheme(&before, at), false)
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
                    _ => {
                        let old = self.textarea.clone();
                        self.textarea.input(key);
                        if self.text().len() > MAX_INPUT {
                            self.textarea = old;
                            return ComposerAction::Rejected(INPUT_TOO_LARGE);
                        }
                        self.snap_cursor();
                    }
                }
            }
            _ => (),
        }
        if self.text() != before {
            self.reset_history();
        }
        ComposerAction::None
    }
}

fn clean_text(text: &str) -> String {
    xcb_core::display_text(&text.replace("\r\n", "\n").replace('\r', "\n"), MAX_INPUT)
}
fn previous_grapheme(text: &str, at: usize) -> usize {
    text[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}
fn next_grapheme(text: &str, at: usize) -> usize {
    at + text[at..].graphemes(true).next().map_or(0, str::len)
}
// Unicode word boundaries keep CJK, punctuation and combining sequences intact.
fn previous_word(text: &str, at: usize) -> usize {
    let prefix = text[..at].trim_end_matches(char::is_whitespace);
    let mut pieces = word_pieces(prefix).into_iter().rev();
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
    let mut pieces = word_pieces(&suffix[skipped..]).into_iter();
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

fn word_pieces(text: &str) -> Vec<(usize, &str)> {
    let mut pieces = Vec::new();
    for (offset, word) in text.split_word_bound_indices() {
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
    }
    pieces
}
