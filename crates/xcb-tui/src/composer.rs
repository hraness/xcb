use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui_textarea::TextArea;
use std::collections::VecDeque;

const MAX_INPUT: usize = 256 * 1024;
/// Notice shown when a paste would push the draft past `MAX_INPUT`. The paste
/// is refused whole rather than silently truncated or dropped.
pub const PASTE_TOO_LARGE: &str = "Paste exceeds 256 KiB; attach a file or trim it";

pub enum ComposerAction {
    None,
    Submit(String),
    Cancel,
    Quit,
    Clipboard,
    History,
    Editor,
    /// Input was refused; the text says why and is shown as a notice.
    Rejected(&'static str),
}

#[derive(Default)]
pub struct Composer {
    pub textarea: TextArea<'static>,
    history: VecDeque<String>,
    history_index: Option<usize>,
    draft: String,
}
impl Composer {
    pub fn text(&self) -> String {
        self.textarea.lines().join("\n")
    }
    pub fn set_text(&mut self, text: &str) {
        if text.len() <= MAX_INPUT {
            self.textarea = TextArea::from(text.lines().map(str::to_owned));
            if text.ends_with('\n') {
                self.textarea = TextArea::from(text.split('\n').map(str::to_owned));
            }
            self.textarea
                .move_cursor(ratatui_textarea::CursorMove::Bottom);
            self.textarea.move_cursor(ratatui_textarea::CursorMove::End);
        }
    }
    /// Remember `text` at the front of the Ctrl-R history. Blank text and a
    /// repeat of the latest entry are skipped.
    pub fn remember(&mut self, text: &str) {
        if !text.trim().is_empty() && self.history.front().map(String::as_str) != Some(text) {
            self.history.push_front(text.to_owned());
            self.history.truncate(200);
        }
    }
    /// Clear the draft, keeping a non-blank one recoverable through Ctrl-R.
    pub fn clear_to_history(&mut self) {
        let text = self.text();
        self.remember(&text);
        self.set_text("");
        self.history_index = None;
    }
    pub fn handle(&mut self, event: Event) -> ComposerAction {
        match event {
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if text.len() > MAX_INPUT || self.text().len() + text.len() > MAX_INPUT {
                    return ComposerAction::Rejected(PASTE_TOO_LARGE);
                }
                self.textarea
                    .insert_str(xcb_core::display_text(&text, MAX_INPUT));
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    // The app intercepts Ctrl-C before this point — it cancels
                    // a live turn, clears a draft, then quits an idle empty
                    // composer. The arm below is the composer's own fallback.
                    KeyCode::Char('c') if ctrl => {
                        self.clear_to_history();
                        return ComposerAction::Cancel;
                    }
                    KeyCode::Esc => return ComposerAction::Cancel,
                    KeyCode::Char('d') if ctrl && self.text().is_empty() => {
                        return ComposerAction::Quit;
                    }
                    KeyCode::Char('v') if ctrl => return ComposerAction::Clipboard,
                    KeyCode::Char('r') if ctrl => return ComposerAction::History,
                    KeyCode::Char('g') if ctrl => return ComposerAction::Editor,
                    KeyCode::Enter
                        if key
                            .modifiers
                            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                    {
                        if self.text().len() < MAX_INPUT {
                            self.textarea.insert_newline();
                        }
                    }
                    KeyCode::Char('j') if ctrl => {
                        if self.text().len() < MAX_INPUT {
                            self.textarea.insert_newline();
                        }
                    }
                    KeyCode::Enter => {
                        let text = self.text();
                        self.remember(&text);
                        self.set_text("");
                        self.history_index = None;
                        return ComposerAction::Submit(text);
                    }
                    KeyCode::Up if self.textarea.cursor().0 == 0 => {
                        if self.history_index.is_none() {
                            self.draft = self.text();
                        }
                        let next = self.history_index.map_or(0, |value| value + 1);
                        if let Some(text) = self.history.get(next).cloned() {
                            self.set_text(&text);
                            self.history_index = Some(next);
                        }
                    }
                    KeyCode::Down if self.history_index.is_some() => {
                        let index = self.history_index.expect("checked history index");
                        let (next, text) = if index == 0 {
                            (None, self.draft.clone())
                        } else {
                            (Some(index - 1), self.history[index - 1].clone())
                        };
                        self.set_text(&text);
                        self.history_index = next;
                    }
                    _ => {
                        if self.text().len() < MAX_INPUT || !matches!(key.code, KeyCode::Char(_)) {
                            self.textarea.input(key);
                        }
                    }
                }
            }
            _ => (),
        }
        ComposerAction::None
    }
    pub fn history(&self) -> impl Iterator<Item = &String> {
        self.history.iter()
    }
}
