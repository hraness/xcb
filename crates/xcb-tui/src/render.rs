use crate::{App, EditorKind, Modal, view_context};
use ratatui::{
    Frame,
    buffer::CellWidth,
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, StyledGrapheme, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use ratatui_textarea::TextArea;
use std::{
    collections::{HashMap, VecDeque, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
};
use xcb_core::{
    Id, display_text,
    panes::{Node, Source},
    session::{Message, MessageProvenance, Role, State},
};

fn status_color(state: State) -> Color {
    match state {
        State::Working => Color::Cyan,
        State::Idle => Color::Green,
        State::NeedsAnswer | State::NeedsApproval | State::NeedsAction | State::Limited => {
            Color::Yellow
        }
        State::Failed | State::Uncertain => Color::Red,
        State::Cancelled => Color::DarkGray,
    }
}
fn status_symbol(state: State) -> &'static str {
    match state {
        State::Idle => "○",
        State::Working => "●",
        State::NeedsAnswer => "?",
        State::NeedsAction => "!",
        State::NeedsApproval => "✓?",
        State::Limited => "↓",
        State::Failed => "×",
        State::Cancelled => "–",
        State::Uncertain => "!?",
    }
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn clean(text: &str) -> String {
    display_text(text, 256 * 1024)
}

/// `display_text` keeps `\t` but the terminal renders control characters as
/// nothing, so transcripts silently dropped every tab. Expand each tab to the
/// next 4-column stop (in display cells, `column` cells already emitted) so
/// wrapped rows match what the user sees.
fn expand_tabs_at(line: &str, column: u16) -> String {
    if !line.contains('\t') {
        return line.to_owned();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = column;
    for ch in line.chars() {
        if ch == '\t' {
            let pad = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', pad as usize));
            column += pad;
        } else {
            if !ch.is_control() {
                let mut buf = [0; 4];
                column = column.saturating_add(ch.encode_utf8(&mut buf).cell_width());
            }
            out.push(ch);
        }
    }
    out
}
fn expand_tabs(line: &str) -> String {
    expand_tabs_at(line, 0)
}
/// Owned transcript rows for one block of text: sanitized by `clean`, split,
/// and tab-expanded so no cell vanishes at render time.
fn body_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    let text = clean(text);
    text.lines()
        .map(|line| Line::from(Span::styled(expand_tabs(line), style)))
        .collect()
}

/// Pre-wrap styled lines at `width` cells, replicating ratatui's
/// `WordWrapper { trim: false }` (see ratatui-widgets `reflow.rs`). Scroll
/// offsets then index the exact rows the terminal shows — `line.width()`
/// underestimates word-wrapped rows, which used to hide the newest tail.
fn wrap_rows(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if width == 0 {
        return out;
    }
    for line in lines {
        wrap_row(
            &mut out,
            line.styled_graphemes(Style::default()),
            line.alignment,
            width,
        );
    }
    out
}

fn wrap_row<'a>(
    out: &mut Vec<Line<'static>>,
    graphemes: impl Iterator<Item = StyledGrapheme<'a>>,
    alignment: Option<Alignment>,
    width: u16,
) {
    let mut pending_line: Vec<StyledGrapheme<'a>> = Vec::new();
    let mut pending_word: Vec<StyledGrapheme<'a>> = Vec::new();
    let mut pending_whitespace: VecDeque<StyledGrapheme<'a>> = VecDeque::new();
    let mut line_width = 0u16;
    let mut word_width = 0u16;
    let mut whitespace_width = 0u16;
    let mut non_whitespace_previous = false;
    let emitted_before = out.len();

    for grapheme in graphemes {
        let is_whitespace = grapheme.is_whitespace();
        let symbol_width = grapheme.symbol.cell_width();
        // ignore symbols wider than line limit
        if symbol_width > width {
            continue;
        }
        let word_found = non_whitespace_previous && is_whitespace;
        // With trim == false the only flush trigger besides a finished word is
        // the current word (plus leading whitespace) overflowing the line.
        // Widths saturate: overlong words/whitespace runs must not overflow u16.
        let untrimmed_overflow = pending_line.is_empty()
            && word_width
                .saturating_add(whitespace_width)
                .saturating_add(symbol_width)
                > width;
        if word_found || untrimmed_overflow {
            pending_line.extend(pending_whitespace.drain(..));
            line_width = line_width.saturating_add(whitespace_width);
            pending_line.append(&mut pending_word);
            line_width = line_width.saturating_add(word_width);
            whitespace_width = 0;
            word_width = 0;
        }
        let line_full = line_width >= width;
        let pending_word_overflow = symbol_width > 0
            && line_width
                .saturating_add(whitespace_width)
                .saturating_add(word_width)
                >= width;
        if line_full || pending_word_overflow {
            let mut remaining_width = width.saturating_sub(line_width);
            out.push(grapheme_row(std::mem::take(&mut pending_line), alignment));
            line_width = 0;
            // remove whitespace up to the end of line
            while let Some(front) = pending_whitespace.front() {
                let front_width = front.symbol.cell_width();
                if front_width > remaining_width {
                    break;
                }
                whitespace_width = whitespace_width.saturating_sub(front_width);
                remaining_width -= front_width;
                pending_whitespace.pop_front();
            }
            // don't count first whitespace toward next word
            if is_whitespace && pending_whitespace.is_empty() {
                continue;
            }
        }
        if is_whitespace {
            whitespace_width = whitespace_width.saturating_add(symbol_width);
            pending_whitespace.push_back(grapheme);
        } else {
            word_width = word_width.saturating_add(symbol_width);
            pending_word.push(grapheme);
        }
        non_whitespace_previous = !is_whitespace;
    }
    // append remaining text parts; trim == false keeps trailing whitespace
    pending_line.extend(pending_whitespace.drain(..));
    pending_line.append(&mut pending_word);
    if !pending_line.is_empty() {
        out.push(grapheme_row(pending_line, alignment));
    } else if out.len() == emitted_before {
        out.push(Line::default());
    }
}

/// Group wrapped graphemes back into same-style spans; rows keep the source
/// line's alignment so right/center lines offset exactly as `Paragraph` did.
fn grapheme_row(graphemes: Vec<StyledGrapheme<'_>>, alignment: Option<Alignment>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for grapheme in graphemes {
        match spans.last_mut() {
            Some(last) if last.style == grapheme.style => {
                last.content.to_mut().push_str(grapheme.symbol);
            }
            _ => spans.push(Span::styled(grapheme.symbol.to_owned(), grapheme.style)),
        }
    }
    Line {
        style: Style::default(),
        alignment,
        spans,
    }
}

/// The provenance boundary label a thinking/assistant turn leads with, when
/// the run, model, or account changed since the previous labelled turn.
fn push_boundary(
    lines: &mut Vec<Line<'static>>,
    message: &Message,
    previous: Option<&MessageProvenance>,
) {
    if let Some(provenance) = message.provenance.as_ref() {
        let label = provenance.boundary_label(previous);
        if !label.is_empty() {
            lines.push(Line::from(Span::styled(label, muted())));
        }
    }
}

/// Rows a persisted message contributes to the transcript body.
fn message_lines(
    app: &App,
    message: &Message,
    previous: Option<&MessageProvenance>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match message.role {
        Role::User => render_user_turn(&mut lines, &message.text, message.attachments.len()),
        // Reasoning always precedes the response it produced and shares the
        // transcript's boundary tracking.
        Role::Thinking => {
            push_boundary(&mut lines, message, previous);
            if app.show_thinking {
                lines.push(Line::from(Span::styled("▾ thinking", muted())));
                lines.extend(body_lines(&message.text, muted()));
            } else {
                lines.push(Line::from(Span::styled("▸ thinking", muted())));
            }
        }
        Role::Assistant => {
            push_boundary(&mut lines, message, previous);
            lines.extend(body_lines(&message.text, Style::default()));
            lines.push(Line::default());
        }
        // A finished tool call is a compact cell in the story.
        Role::Tool => render_tool_turn(&mut lines, &message.text, app.show_activity),
        Role::System => (),
    }
    lines
}

/// The boundary context a message's provenance label was computed against.
/// Only thinking/assistant turns emit labels, so other roles share one key.
fn provenance_fp(provenance: Option<&MessageProvenance>) -> u64 {
    let mut hasher = DefaultHasher::new();
    if let Some(provenance) = provenance {
        provenance.account.as_str().hash(&mut hasher);
        provenance.model.key().hash(&mut hasher);
        provenance.run.as_ref().map(Id::as_str).hash(&mut hasher);
    }
    hasher.finish()
}
/// `previous` advances only across thinking/assistant turns with provenance,
/// matching the boundary tracking the renderer always used.
fn next_boundary<'a>(
    previous: Option<&'a MessageProvenance>,
    message: &'a Message,
) -> Option<&'a MessageProvenance> {
    if matches!(message.role, Role::Thinking | Role::Assistant) {
        message.provenance.as_ref().or(previous)
    } else {
        previous
    }
}
/// Key identifying a message's wrapped rows: id, content shape, boundary
/// context, and the toggles that change its rendered shape. Persisted
/// messages are append-only, but context elision and retries can rewrite a
/// body in place — hashing the length alone would miss a same-length edit,
/// while hashing every byte costs a 512KiB scan per frame on big
/// transcripts. Sampling the head and tail keeps the key O(1) yet still
/// catches any realistic rewrite.
fn message_key(app: &App, message: &Message, previous: Option<&MessageProvenance>) -> u64 {
    let mut hasher = DefaultHasher::new();
    (message.role as u8).hash(&mut hasher);
    message.text.len().hash(&mut hasher);
    let bytes = message.text.as_bytes();
    bytes[..64.min(bytes.len())].hash(&mut hasher);
    bytes[bytes.len().saturating_sub(64)..].hash(&mut hasher);
    message.attachments.len().hash(&mut hasher);
    if matches!(message.role, Role::Thinking | Role::Assistant)
        && let Some(provenance) = message.provenance.as_ref()
    {
        provenance.account.as_str().hash(&mut hasher);
        provenance.model.key().hash(&mut hasher);
        provenance.run.as_ref().map(Id::as_str).hash(&mut hasher);
        provenance_fp(previous).hash(&mut hasher);
    }
    app.show_thinking.hash(&mut hasher);
    app.show_activity.hash(&mut hasher);
    hasher.finish()
}

struct MessageRows {
    key: u64,
    rows: Vec<Line<'static>>,
}

/// Render-side wrap cache for the transcript. Persisted messages are
/// append-only with immutable text, so their wrapped rows survive unchanged
/// across frames keyed by (id, text length, boundary context, toggles); only
/// the streaming tail — echoes, `app.thinking`, `app.stream`, live tool cells —
/// re-wraps, and only when its inputs changed. The textarea scroll mirrors
/// live here too (see `place_textarea_cursor`).
#[derive(Default)]
pub(crate) struct RenderCache {
    width: u16,
    context: Option<Id>,
    message_count: usize,
    rows: HashMap<Id, MessageRows>,
    tail_key: u64,
    tail_rows: Vec<Line<'static>>,
    composer_scroll: (u16, u16),
    editor_scroll: (u16, u16),
    /// True while an editor modal is open; a fresh editor resets its mirror.
    editor_open: bool,
    thinking_key: u64,
    thinking_width: u16,
    thinking_rows: Vec<Line<'static>>,
}
impl RenderCache {
    /// Drop rows when the wrap width, conversation context, or message list
    /// shape changed underneath the cache.
    fn prepare(&mut self, app: &App, width: u16) {
        let context = view_context(&app.view);
        if self.width != width
            || self.context != context
            || app.view.messages.len() < self.message_count
        {
            self.rows.clear();
            self.tail_key = 0;
            self.thinking_key = 0;
            self.width = width;
            self.context = context;
        }
        self.message_count = app.view.messages.len();
        // Entries belong to the live message list; wholesale replacement is
        // the only way to outgrow it, so a hard clear is a sufficient bound.
        if self.rows.len() > self.message_count.saturating_add(64) {
            self.rows.clear();
            self.tail_key = 0;
            self.thinking_key = 0;
        }
    }
    /// Ensure `message`'s wrapped rows exist and return their row count.
    fn ensure_rows(
        &mut self,
        app: &App,
        message: &Message,
        previous: Option<&MessageProvenance>,
        width: u16,
    ) -> usize {
        let key = message_key(app, message, previous);
        let entry = self
            .rows
            .entry(message.id.clone())
            .or_insert_with(|| MessageRows {
                key: u64::MAX,
                rows: Vec::new(),
            });
        if entry.key != key {
            entry.key = key;
            entry.rows = wrap_rows(&message_lines(app, message, previous), width);
        }
        entry.rows.len()
    }
    /// Rows the streaming tail currently wraps to, re-wrapped only when any
    /// input that shapes it changed.
    fn tail_rows(&mut self, app: &App, width: u16) -> &[Line<'static>] {
        let mut hasher = DefaultHasher::new();
        app.stream.len().hash(&mut hasher);
        app.thinking.len().hash(&mut hasher);
        app.show_thinking.hash(&mut hasher);
        app.show_activity.hash(&mut hasher);
        (app.view.state as u8).hash(&mut hasher);
        app.view.remote_active.hash(&mut hasher);
        app.view.messages.len().hash(&mut hasher);
        app.view.activity.len().hash(&mut hasher);
        if let Some(last) = app.view.activity.last() {
            last.hash(&mut hasher);
        }
        let mut echo_signature = 0usize;
        for (text, attachments) in app.pending_echoes() {
            echo_signature += text.len() + attachments;
        }
        echo_signature.hash(&mut hasher);
        let key = hasher.finish();
        if key != self.tail_key {
            self.tail_key = key;
            self.tail_rows = wrap_rows(&tail_lines(app), width);
        }
        &self.tail_rows
    }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Compact elapsed time for the working badge: `12s`, `3m 7s`, `1h 4m`.
fn elapsed_label(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App, ticks: u64) {
    let area = frame.area();
    if area.width < 24 || area.height < 7 {
        frame.render_widget(
            Paragraph::new("xcb · enlarge the terminal\nCtrl-C cancels · Ctrl-D exits"),
            area,
        );
        return;
    }
    let attachment_height = (app.attachments.len() as u16).min(3);
    let input_height = (app.composer.textarea.lines().len() as u16)
        .saturating_add(2)
        .clamp(3, 8)
        .min(area.height.saturating_sub(4 + attachment_height));
    let notice = app.view.pane_error.as_deref().unwrap_or(&app.notice);
    // Long notices wrap to the viewport instead of truncating, borrowing up
    // to two rows from the transcript without starving the composer or chrome.
    let notice_budget = area
        .height
        .saturating_sub(4 + attachment_height + input_height)
        .max(1);
    let notice_height = (wrap_rows(
        &[Line::from(expand_tabs(&clean(notice)))],
        area.width.max(1),
    )
    .len() as u16)
        .clamp(1, 3)
        .min(notice_budget);
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(notice_height),
            Constraint::Length(attachment_height),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);
    let mut project = app
        .view
        .session
        .as_ref()
        .map(|session| session.workspace.as_str())
        .or_else(|| {
            app.view.conversation.as_ref().and_then(|id| {
                app.view
                    .conversations
                    .iter()
                    .find(|conversation| &conversation.id == id)
                    .map(|conversation| conversation.workspace.as_str())
            })
        })
        .or_else(|| app.view.tasks.first().map(|task| task.workspace.as_str()))
        .and_then(|workspace| std::path::Path::new(workspace).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "local workspace".into());
    if app.view.session.is_none()
        && let Some(title) = app.view.conversation.as_ref().and_then(|id| {
            app.view
                .conversations
                .iter()
                .find(|conversation| &conversation.id == id)
                .map(|conversation| conversation.title.clone())
        })
    {
        project = title;
    }
    // Header stays quiet until throughput is actually measured.
    let rate = match (app.view.tokens_per_second, app.view.share_percent) {
        (Some(rate), Some(share)) => format!("{rate:.1} tok/s · {share:.0}% local"),
        (Some(rate), None) => format!("{rate:.1} tok/s"),
        (None, Some(share)) => format!("{share:.0}% local"),
        (None, None) => String::new(),
    };
    let header = Layout::horizontal([
        Constraint::Min(8),
        Constraint::Length((rate.len() as u16).min(area.width / 2)),
    ])
    .split(parts[0]);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("xcb", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!(" · {}", clean(&project)), muted()),
        ])),
        header[0],
    );
    let heat = match app.view.share_percent {
        Some(percent) if percent >= 50.0 => Color::LightRed,
        Some(percent) if percent >= 20.0 => Color::Yellow,
        Some(_) => Color::Cyan,
        None => Color::DarkGray,
    };
    frame.render_widget(
        Paragraph::new(rate)
            .alignment(Alignment::Right)
            .style(Style::default().fg(heat)),
        header[1],
    );
    app.scroll_top.set(0);
    app.scroll_tail.set(0);
    render_node(frame, &app.view.pane.root, parts[1], app);
    frame.render_widget(
        Paragraph::new(expand_tabs(&clean(notice)))
            .style(Style::default().fg(Color::Yellow))
            .wrap(Wrap { trim: false }),
        parts[2],
    );
    if !app.attachments.is_empty() {
        let visible = app
            .attachments
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(|attachment| {
                let kind = attachment
                    .media_type
                    .strip_prefix("image/")
                    .unwrap_or(&attachment.media_type);
                Line::from(format!(
                    "[image:{} {}×{} · {} KiB]",
                    display_text(kind, 32),
                    attachment.width,
                    attachment.height,
                    attachment.bytes.div_ceil(1024)
                ))
            });
        frame.render_widget(
            Paragraph::new(Text::from_iter(visible)).style(muted()),
            parts[3],
        );
    }
    let attachment_hint = if app.attachments.is_empty() {
        String::new()
    } else {
        format!(
            " {} attached · Alt-Backspace removes last ",
            app.attachments.len()
        )
    };
    app.composer.textarea.set_block(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(status_color(app.view.state)))
            .title(attachment_hint),
    );
    app.composer
        .textarea
        .set_cursor_line_style(Style::default());
    app.composer.textarea.set_placeholder_text(
        if app.managed_mode() && app.view.state == State::Working {
            "Describe new work or ask for status · /tasks"
        } else if app.view.remote_active {
            "Running in another terminal · your draft is kept here"
        } else if app.view.state == State::Working {
            "Type a follow-up while the agent works"
        } else {
            "Message · / for commands · Ctrl-V pastes"
        },
    );
    frame.render_widget(&app.composer.textarea, parts[4]);
    if let Some((matches, selected)) = app.slash_menu() {
        render_slash_menu(frame, &matches, selected, parts[4]);
    }
    // A live run owned by a sibling terminal is normal parallel work, not a
    // session needing recovery.
    let badge_state = if app.view.remote_active {
        State::Working
    } else {
        app.view.state
    };
    let mut color = status_color(badge_state);
    if badge_state.attention() && !app.view.reduced_motion && (ticks / 16).is_multiple_of(2) {
        color = Color::LightYellow;
    }
    let status = if matches!(badge_state, State::Working) {
        let elapsed = app
            .working_since
            .map(|since| elapsed_label(since.elapsed().as_secs()))
            .unwrap_or_else(|| "0s".into());
        let frame = if app.view.reduced_motion {
            "●"
        } else {
            SPINNER[(ticks / 3) as usize % SPINNER.len()]
        };
        let label = if app.view.remote_active {
            "working elsewhere"
        } else {
            "Working"
        };
        format!(" {frame} {label} · {elapsed} ")
    } else {
        format!(" {} {} ", status_symbol(badge_state), badge_state.label())
    };
    let footer = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(status.chars().count() as u16),
    ])
    .split(parts[5]);
    let model = app
        .view
        .session
        .as_ref()
        .map(|session| {
            format!(
                "{} · {} · ? help",
                session.model.provider, session.model.label
            )
        })
        .or_else(|| {
            (!app.view.tasks.is_empty()).then(|| {
                // The footer speaks in wire phases: queued work is waiting
                // for a route, only dispatched workers count as running.
                let running = app
                    .view
                    .tasks
                    .iter()
                    .filter(|task| task.state == State::Working && !crate::task_queued(task))
                    .count();
                let queued = app
                    .view
                    .tasks
                    .iter()
                    .filter(|task| crate::task_queued(task))
                    .count();
                let waiting = app
                    .view
                    .tasks
                    .iter()
                    .filter(|task| task.state.attention())
                    .count();
                // The freshest running worker's `model · account` identifies
                // the route the swarm is actually using.
                let routed = app
                    .view
                    .tasks
                    .iter()
                    .filter(|task| crate::task_status(task) == "running" && task.route.is_some())
                    .max_by_key(|task| task.updated_at_ms)
                    .and_then(|task| task.route.clone());
                format!(
                    "{running} running{} · {waiting} needs you · {} chats{} · /t tasks · ? help",
                    if queued > 0 {
                        format!(" · {queued} queued")
                    } else {
                        String::new()
                    },
                    app.view.conversations.len(),
                    routed
                        .map(|route| format!(" · {route}"))
                        .unwrap_or_default(),
                )
            })
        })
        .or_else(|| {
            app.view
                .extensions
                .iter()
                .any(|(name, _)| name == "algal supervisor")
                .then(|| "global dispatcher · / for commands · ? help".into())
        })
        .or_else(|| {
            app.view
                .pending_route
                .as_ref()
                .map(|route| format!("{} · {} · ? help", route.model, route.account))
        })
        .unwrap_or_else(|| "Choose an account with /accounts · ? help".into());
    frame.render_widget(
        Paragraph::new(clean(&model)).style(Style::default().add_modifier(Modifier::BOLD)),
        footer[0],
    );
    frame.render_widget(
        Paragraph::new(status).alignment(Alignment::Right).style(
            Style::default()
                .bg(color)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        footer[1],
    );
    let managed = app
        .view
        .extensions
        .iter()
        .any(|(name, _)| name == "algal supervisor");
    let shift_enter = app.keyboard_enhanced;
    // The hardware cursor belongs to whatever captures typing: an open modal
    // (its own cursor), otherwise the composer.
    if let Some(modal) = &mut app.modal {
        let mut cache = app.render_cache.borrow_mut();
        render_modal(frame, modal, area, managed, shift_enter, &mut cache);
    } else {
        let mut cache = app.render_cache.borrow_mut();
        cache.editor_open = false;
        place_textarea_cursor(
            frame,
            &app.composer.textarea,
            parts[4],
            &mut cache.composer_scroll,
        );
    }
}

/// `ratatui-textarea` keeps its scroll offset private inside `Viewport`;
/// mirror the widget's `next_scroll_top` rule so the computed cursor cell
/// tracks what was rendered. Two textareas share this rule (composer and the
/// editor modal), each keeping its previous top in the render cache.
fn next_scroll_top(prev_top: u16, cursor: u16, len: u16) -> u16 {
    if cursor < prev_top {
        cursor
    } else if prev_top.saturating_add(len) <= cursor {
        cursor.saturating_add(1).saturating_sub(len)
    } else {
        prev_top
    }
}

/// Display-cell column of `col` characters into `line`, matching the
/// textarea's own `DisplayTextBuilder` (tabs expand to 4-column stops,
/// control characters render as nothing).
fn cursor_cells(line: &str, col: usize) -> u16 {
    let mut width = 0u16;
    for ch in line.chars().take(col) {
        width = if ch == '\t' {
            width.saturating_add(4 - width % 4)
        } else if ch.is_control() {
            width
        } else {
            let mut buf = [0; 4];
            width.saturating_add(ch.encode_utf8(&mut buf).cell_width())
        };
    }
    width
}

/// Place the terminal's hardware cursor on the textarea's cursor cell so IME
/// candidate windows and the cursor shape land where typing happens.
fn place_textarea_cursor(
    frame: &mut Frame<'_>,
    textarea: &TextArea<'static>,
    area: Rect,
    scroll: &mut (u16, u16),
) {
    let inner = textarea.block().map_or(area, |block| block.inner(area));
    if inner.is_empty() {
        return;
    }
    let (row, col) = textarea.cursor();
    let cursor_row = u16::try_from(row).unwrap_or(u16::MAX);
    // The widget scrolls horizontally by character column, not cells (our
    // textareas never enable line numbers, so no width correction applies).
    let cursor_col = u16::try_from(col).unwrap_or(u16::MAX);
    let top_row = next_scroll_top(scroll.0, cursor_row, inner.height);
    let top_col = next_scroll_top(scroll.1, cursor_col, inner.width);
    *scroll = (top_row, top_col);
    let line = textarea.lines().get(row).map_or("", String::as_str);
    let cell = cursor_cells(line, col);
    let x = inner.x.saturating_add(cell.saturating_sub(top_col));
    let y = inner.y.saturating_add(cursor_row.saturating_sub(top_row));
    frame.set_cursor_position(Position::new(
        x.min(inner.right().saturating_sub(1)),
        y.min(inner.bottom().saturating_sub(1)),
    ));
}

/// A completed tool call renders as a compact `• name` cell so the transcript
/// narrates the work; Ctrl-U expands the first lines of its output inline.
fn render_tool_turn(lines: &mut Vec<Line<'static>>, text: &str, expanded: bool) {
    let (name, output) = text.split_once(": ").unwrap_or((text, ""));
    lines.push(Line::from(vec![
        Span::styled("• ", muted()),
        Span::styled(
            expand_tabs_at(&clean(name), 2),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    if !expanded {
        return;
    }
    let output: Vec<String> = clean(output)
        .lines()
        .take(9)
        .map(|line| display_text(line, 160))
        .collect();
    for line in output.iter().take(8) {
        // Expanded cells are indented two cells; tabs keep stopping on the
        // same four-column grid the rest of the transcript uses.
        lines.push(Line::from(Span::styled(
            format!("  {}", expand_tabs_at(line, 2)),
            muted(),
        )));
    }
    if output.len() > 8 {
        lines.push(Line::from(Span::styled("  …", muted())));
    }
}

fn render_user_turn(lines: &mut Vec<Line<'static>>, text: &str, attachments: usize) {
    lines.push(Line::from(Span::styled(
        "You",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.extend(body_lines(text, Style::default()));
    if attachments > 0 {
        lines.push(Line::from(Span::styled(
            format!("{attachments} attached image(s)"),
            muted(),
        )));
    }
    lines.push(Line::default());
}

fn preferred_height(node: &Node, app: &App) -> Constraint {
    match node {
        // List widgets collapse entirely when they have nothing to show.
        Node::Widget { source, .. }
            if matches!(source, Source::Subagents)
                && app.view.subagents.is_empty()
                && app.view.tasks.is_empty()
                || matches!(source, Source::Extensions) && app.view.extensions.is_empty() =>
        {
            Constraint::Length(0)
        }
        Node::Widget {
            lines: Some(lines), ..
        }
        | Node::Spacer { lines } => Constraint::Length(*lines),
        Node::Text { value } => Constraint::Length(value.lines().count().clamp(1, 6) as u16),
        _ => Constraint::Min(1),
    }
}

fn render_node(frame: &mut Frame<'_>, node: &Node, area: Rect, app: &App) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    match node {
        Node::Column { children } | Node::Row { children } => {
            let horizontal = matches!(node, Node::Row { .. });
            let constraints: Vec<_> = children
                .iter()
                .map(|node| {
                    if horizontal {
                        Constraint::Ratio(1, children.len() as u32)
                    } else {
                        preferred_height(node, app)
                    }
                })
                .collect();
            let chunks = Layout::default()
                .direction(if horizontal {
                    Direction::Horizontal
                } else {
                    Direction::Vertical
                })
                .constraints(constraints)
                .split(area);
            for (child, chunk) in children.iter().zip(chunks.iter()) {
                render_node(frame, child, *chunk, app);
            }
        }
        Node::Widget { source, .. } => render_source(frame, *source, area, app),
        Node::Text { value } => frame.render_widget(
            Paragraph::new(expand_tabs(&clean(value))).wrap(Wrap { trim: false }),
            area,
        ),
        Node::Spacer { .. } => (),
    }
}

fn render_source(frame: &mut Frame<'_>, source: Source, area: Rect, app: &App) {
    // Scrollable sources render wrapped row slices; everything else stays a
    // wrapped Paragraph (it never scrolls and is bounded).
    match source {
        Source::Responses => return render_responses(frame, area, app),
        Source::Thinking => return render_thinking(frame, area, app),
        _ => (),
    }
    let mut lines = Vec::new();
    match source {
        Source::LastUser => {
            lines.push(Line::from(Span::styled(
                "You",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            match app
                .view
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::User)
            {
                Some(message) => {
                    lines.extend(body_lines(&message.text, Style::default()));
                    if !message.attachments.is_empty() {
                        lines.push(Line::from(Span::styled(
                            format!("{} attached image(s)", message.attachments.len()),
                            muted(),
                        )));
                    }
                }
                None => lines.push(Line::from(
                    "Bring your accounts. Choose your models. Make the terminal yours.",
                )),
            }
        }
        Source::Subagents => {
            lines.push(Line::from(Span::styled(
                if app.view.tasks.is_empty() {
                    "Subagents"
                } else {
                    "Tasks · /tasks"
                },
                muted(),
            )));
            for task in &app.view.tasks {
                let project = std::path::Path::new(&task.workspace)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("workspace");
                let status = crate::task_status(task);
                // A queued task is not running yet: a hollow marker and the
                // wire phase keep it visibly distinct from a live worker.
                let symbol = if crate::task_queued(task) {
                    "◌"
                } else {
                    status_symbol(task.state)
                };
                let mut tail = format!(" · {status} · {project} · {}", clean(&task.detail));
                if let Some(route) = &task.route {
                    tail.push_str(&format!(" · {}", clean(route)));
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{symbol} "),
                        Style::default().fg(status_color(task.state)),
                    ),
                    Span::styled(
                        clean(&task.title),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(tail, muted()),
                ]));
            }
            if app.view.tasks.is_empty() && app.view.subagents.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No subagent activity reported",
                    muted(),
                )));
            }
            for agent in &app.view.subagents {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("[{}] ", agent.state.label()),
                        Style::default().fg(status_color(agent.state)),
                    ),
                    Span::raw(clean(&agent.label)),
                    Span::styled(
                        agent
                            .model
                            .as_ref()
                            .map(|model| format!(" · {model}"))
                            .unwrap_or_default(),
                        muted(),
                    ),
                ]));
            }
        }
        Source::Accounts => {
            lines.push(Line::from(Span::styled(
                "Accounts · /accounts",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for account in &app.view.accounts {
                let remaining = account
                    .quota_block_label(crate::display_now_ms())
                    .unwrap_or_else(|| {
                        account
                            .remaining_percent
                            .map(|remaining| format!("{remaining:.0}% left"))
                            .unwrap_or_else(|| "quota unknown".into())
                    });
                let time = account
                    .runway
                    .seconds()
                    .map(|seconds| format!(" · ~{:.1}h", seconds / 3600.0))
                    .unwrap_or_default();
                lines.push(Line::from(format!(
                    "{} {} · {} · {remaining}{time}{}{}",
                    if account.busy { "*" } else { " " },
                    clean(&account.name),
                    account.provider,
                    if account.enabled { "" } else { " · disabled" },
                    if account.authentication_required {
                        " · reconnect required"
                    } else {
                        ""
                    }
                )));
            }
            if let Some(seconds) = app.view.total_runway_seconds {
                lines.push(Line::from(Span::styled(
                    format!(
                        "~{:.1}h measured pool runway · {}/{} pools",
                        seconds / 3600.0,
                        app.view.runway_coverage.0,
                        app.view.runway_coverage.1
                    ),
                    muted(),
                )));
            }
        }
        Source::Models => {
            lines.push(Line::from(Span::styled(
                "Models · favorites first",
                muted(),
            )));
            for model in app.view.models.iter().take(24) {
                lines.push(Line::from(format!(
                    "{} · {}",
                    model.provider,
                    clean(&model.label)
                )));
            }
        }
        Source::Usage => {
            lines.push(Line::from("Observed output velocity · rolling 60s"));
            lines.push(Line::from(
                app.view
                    .tokens_per_second
                    .map(|rate| format!("{rate:.1} tok/s"))
                    .unwrap_or_else(|| "Unmeasured; waiting for comparable samples".into()),
            ));
            lines.push(Line::from(
                app.view
                    .share_percent
                    .map(|share| format!("{share:.0}% of measured local throughput"))
                    .unwrap_or_else(|| "Local throughput share unavailable".into()),
            ));
        }
        Source::Activity => {
            lines.push(Line::from(Span::styled(
                if app.show_activity {
                    "Tool activity · Ctrl-U hides"
                } else {
                    "Tool activity hidden · Ctrl-U reveals"
                },
                muted(),
            )));
            if app.show_activity {
                lines.extend(
                    app.view
                        .activity
                        .iter()
                        .rev()
                        .take(32)
                        .rev()
                        .flat_map(|text| body_lines(text, Style::default())),
                );
            }
        }
        Source::Extensions => {
            lines.extend(
                app.view
                    .extensions
                    .iter()
                    .map(|(name, state)| Line::from(format!("{name}: {state}"))),
            );
        }
        Source::Responses | Source::Thinking => unreachable!("handled by the scrolled path"),
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

/// The transcript tail: optimistic echoes, in-flight thinking/stream text,
/// and tool cells that have not persisted yet. Rebuilt (and re-wrapped) when
/// any of those inputs change; everything before it hits the message cache.
fn tail_lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (text, attachments) in app.pending_echoes() {
        render_user_turn(&mut lines, text, attachments);
    }
    if !app.thinking.is_empty() {
        if app.show_thinking {
            lines.push(Line::from(Span::styled("▾ thinking", muted())));
            lines.extend(body_lines(&app.thinking, muted()));
        } else {
            lines.push(Line::from(Span::styled("▸ thinking", muted())));
        }
    }
    if !app.stream.is_empty() {
        lines.extend(body_lines(&app.stream, Style::default()));
    }
    // Tools started but not yet persisted keep narrating the turn at
    // the tail until their result cell lands — but only while a run
    // is live; stale activity stays hidden behind Ctrl-U's detail.
    // `activity` only lists the current run's calls, so count this
    // turn's settled cells.
    if matches!(app.view.state, State::Working) || app.view.remote_active {
        let messages = &app.view.messages;
        let turn_start = messages
            .iter()
            .rposition(|message| message.role == Role::User)
            .map(|index| index + 1)
            .unwrap_or(0);
        let settled = messages[turn_start..]
            .iter()
            .filter(|message| message.role == Role::Tool)
            .count();
        for name in app.view.activity.iter().skip(settled) {
            lines.push(Line::from(vec![
                Span::styled("• ", muted()),
                Span::raw(expand_tabs_at(&clean(name), 2)),
                Span::styled(" …", muted()),
            ]));
        }
    }
    lines
}

fn hint_line(app: &App) -> Line<'static> {
    let hint = if app
        .view
        .extensions
        .iter()
        .any(|(name, _)| name == "algal supervisor")
    {
        "Describe work or ask about the running task swarm"
    } else {
        "/help for commands · /pane to change this view"
    };
    Line::from(Span::styled(hint, muted()))
}

/// The heading row stays pinned; only the body scrolls, so the paused
/// marker is always visible no matter where the viewport sits.
fn render_heading(frame: &mut Frame<'_>, heading: Line<'static>, area: Rect) {
    frame.render_widget(
        Paragraph::new(Text::from(vec![heading])),
        Rect { height: 1, ..area },
    );
}

/// Shared scroll bookkeeping for transcript-like sources: tail-follows while
/// live, pins to `app.scroll` while paused, and reports both edges so wheel /
/// paging keys know the real bounds — now in wrapped rows, not estimates.
fn scroll_window(app: &App, content_height: usize, viewport: u16) -> usize {
    let tail = content_height.saturating_sub(viewport as usize);
    // While paused the viewport stays on the absolute row index in `scroll`;
    // a growing tail cannot drift it. Otherwise it follows.
    let top = if app.paused.get() {
        tail.min(app.scroll.get() as usize)
    } else {
        tail
    };
    app.scroll_top.set(
        app.scroll_top
            .get()
            .max(u32::try_from(top).unwrap_or(u32::MAX)),
    );
    app.scroll_tail.set(
        app.scroll_tail
            .get()
            .max(u32::try_from(tail).unwrap_or(u32::MAX)),
    );
    top
}

/// Column offset an aligned wrapped row starts at — the same rule
/// `Paragraph`'s `render_line` applies (`reflow::get_line_offset`).
fn line_offset(line_width: u16, area_width: u16, alignment: Alignment) -> u16 {
    match alignment {
        Alignment::Center => (area_width / 2).saturating_sub(line_width / 2),
        Alignment::Right => area_width.saturating_sub(line_width),
        Alignment::Left => 0,
    }
}

/// Render one already-wrapped row exactly as `Paragraph`'s `render_line` does:
/// every grapheme writes its symbol into a single cell at the running offset.
/// `Buffer::set_stringn` would instead drop a grapheme that cannot fully fit
/// the remaining width, while `Paragraph` keeps it — a wide glyph may
/// straddle the last column, and dropping it would lose visible content.
/// Writes at or beyond `area.width` are clipped rather than bleeding into the
/// neighbouring pane, which `Paragraph` would paint over.
fn render_row(frame: &mut Frame<'_>, area: Rect, y: u16, row: &Line<'static>) {
    let mut x = line_offset(
        u16::try_from(row.width()).unwrap_or(u16::MAX),
        area.width,
        row.alignment.unwrap_or(Alignment::Left),
    );
    let buffer = frame.buffer_mut();
    for StyledGrapheme { symbol, style } in row.styled_graphemes(Style::default()) {
        let width = symbol.cell_width();
        if width == 0 {
            continue;
        }
        if x < area.width {
            let symbol = if symbol.is_empty() { " " } else { symbol };
            let position = Position::new(area.left().saturating_add(x), y);
            if let Some(cell) = buffer.cell_mut(position) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
        x = x.saturating_add(width);
    }
}

/// Render the overlap between `rows` (starting at absolute row `index`) and
/// the viewport [`top`, `top + area.height`). Rows are already wrapped to
/// `area.width`, so each renders as one terminal row — no Paragraph wrap, no
/// `u16` scroll offset, no full-transcript copy.
fn render_rows(
    frame: &mut Frame<'_>,
    area: Rect,
    rows: &[Line<'static>],
    index: usize,
    top: usize,
) {
    let bottom = top + area.height as usize;
    let end = index + rows.len();
    if end <= top || index >= bottom {
        return;
    }
    let first = top.saturating_sub(index);
    let last = (bottom - index).min(rows.len());
    for (offset, row) in rows[first..last].iter().enumerate() {
        let y = area.y.saturating_add((index + first + offset - top) as u16);
        // Rows that fit left-aligned cannot straddle the last column, so
        // `Line::render` produces the same cells `render_line` would — take
        // its faster span-wise path. Only rows with an overhanging grapheme
        // or a non-left alignment need per-grapheme fidelity.
        let fits = row.alignment.unwrap_or(Alignment::Left) == Alignment::Left
            && row.width() <= area.width as usize;
        if fits {
            frame.render_widget(
                row,
                Rect {
                    y,
                    height: 1,
                    ..area
                },
            );
        } else {
            render_row(frame, area, y, row);
        }
    }
}

fn render_responses(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let heading = if app.show_history {
        "▾ Transcript · Ctrl-O collapses history"
    } else {
        "▸ Transcript · Ctrl-O expands history"
    };
    // The paused marker leads so terminal width cannot truncate it.
    render_heading(
        frame,
        Line::from(Span::styled(
            if app.paused.get() {
                format!("↑ paused · End follows · {heading}")
            } else {
                heading.into()
            },
            muted(),
        )),
        area,
    );
    let body_area = Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(1),
        ..area
    };
    if body_area.height == 0 || body_area.width == 0 {
        return;
    }
    let width = body_area.width.max(1);
    let mut cache = app.render_cache.borrow_mut();
    cache.prepare(app, width);

    let messages = &app.view.messages;
    // Collapsed shows the latest turn: everything since the last user
    // message, or the trailing message when no prompt exists yet.
    let start = if app.show_history {
        0
    } else {
        messages
            .iter()
            .rposition(|message| message.role == Role::User)
            .unwrap_or_else(|| messages.len().saturating_sub(1))
            .min(messages.len())
    };

    // First pass: ensure every rendered message's wrapped rows exist (cache
    // hits are just a lookup) and count the exact wrapped height.
    cache.tail_rows(app, width);
    let mut total = cache.tail_rows.len();
    let mut previous = None;
    for message in &messages[start..] {
        total += cache.ensure_rows(app, message, previous, width);
        previous = next_boundary(previous, message);
    }

    // An entirely empty transcript shows the hint instead of a tail.
    let hint_rows = if total == 0 {
        let rows = wrap_rows(&[hint_line(app)], width);
        total = rows.len();
        rows
    } else {
        Vec::new()
    };

    let top = scroll_window(app, total, body_area.height);
    let bottom = top + body_area.height as usize;

    // Second pass: every entry exists now, so plain lookups can hand out row
    // slices and only the visible window is pushed to the buffer.
    let mut index = 0usize;
    for message in &messages[start..] {
        if let Some(entry) = cache.rows.get(&message.id) {
            render_rows(frame, body_area, &entry.rows, index, top);
            index += entry.rows.len();
        }
        if index >= bottom {
            break;
        }
    }
    let tail: &[Line<'static>] = if hint_rows.is_empty() {
        &cache.tail_rows
    } else {
        &hint_rows
    };
    render_rows(frame, body_area, tail, index, top);
}

fn render_thinking(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let heading = if app.show_thinking {
        "▾ Thinking · Ctrl-T collapses"
    } else {
        "▸ Thinking · Ctrl-T expands"
    };
    render_heading(
        frame,
        Line::from(Span::styled(
            if app.paused.get() {
                format!("↑ paused · End follows · {heading}")
            } else {
                heading.into()
            },
            muted(),
        )),
        area,
    );
    let body_area = Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(1),
        ..area
    };
    if body_area.height == 0 || body_area.width == 0 {
        return;
    }
    let width = body_area.width.max(1);
    // The body is keyed like the transcript tail: live thinking only
    // appends, so its length is the signature; the persisted fallback
    // samples the message like `message_key` does.
    let latest = app
        .view
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Thinking);
    let mut hasher = DefaultHasher::new();
    app.show_thinking.hash(&mut hasher);
    app.thinking.len().hash(&mut hasher);
    if let Some(message) = latest {
        message.id.as_str().hash(&mut hasher);
        message.text.len().hash(&mut hasher);
        let bytes = message.text.as_bytes();
        bytes[..64.min(bytes.len())].hash(&mut hasher);
        bytes[bytes.len().saturating_sub(64)..].hash(&mut hasher);
        provenance_fp(message.provenance.as_ref()).hash(&mut hasher);
    }
    let key = hasher.finish();
    let mut cache = app.render_cache.borrow_mut();
    if cache.thinking_key != key || cache.thinking_width != width {
        cache.thinking_key = key;
        cache.thinking_width = width;
        let mut body = Vec::new();
        if app.show_thinking {
            let text = if app.thinking.is_empty() {
                latest
                    .map(|message| message.text.as_str())
                    .unwrap_or("No thinking text reported.")
            } else {
                &app.thinking
            };
            if app.thinking.is_empty()
                && let Some(provenance) = latest.and_then(|message| message.provenance.as_ref())
            {
                body.push(Line::from(Span::styled(
                    provenance.boundary_label(None),
                    muted(),
                )));
            }
            body.extend(body_lines(text, muted()));
        }
        cache.thinking_rows = wrap_rows(&body, width);
    }
    let top = scroll_window(app, cache.thinking_rows.len(), body_area.height);
    render_rows(frame, body_area, &cache.thinking_rows, 0, top);
}

fn modal_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(110);
    let height = area.height.saturating_sub(2);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// Slash-command typeahead: a compact popup anchored above the composer that
/// lists matching commands with their usage and summary.
fn render_slash_menu(
    frame: &mut Frame<'_>,
    matches: &[&crate::SlashCommand],
    selected: usize,
    composer: Rect,
) {
    let height = (matches.len() as u16 + 2).min(10).min(composer.y);
    if height < 3 {
        return;
    }
    let area = Rect::new(
        composer.x,
        composer.y - height,
        composer.width.min(64),
        height,
    );
    frame.render_widget(Clear, area);
    let items: Vec<_> = matches
        .iter()
        .map(|command| {
            let summary = if command.alias.is_empty() {
                format!("  {}", command.summary)
            } else {
                format!("  {} · {}", command.alias, command.summary)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} {}", command.name, command.args)
                        .trim_end()
                        .to_owned(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(summary, muted()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(selected));
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::bordered()
                    .title(" commands ")
                    .title_bottom(" ↑↓ choose · Tab completes · Enter runs · Esc hides "),
            )
            .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
            .highlight_symbol("› "),
        area,
        &mut state,
    );
}

fn render_modal(
    frame: &mut Frame<'_>,
    modal: &mut Modal,
    area: Rect,
    managed: bool,
    shift_enter: bool,
    cache: &mut RenderCache,
) {
    let area = modal_area(area);
    frame.render_widget(Clear, area);
    match modal {
        Modal::Help => {
            cache.editor_open = false;
            let cancel = if managed {
                "Esc requests cancellation for managed work · Esc also closes dialogs and the / menu"
            } else {
                "Esc stops the running turn · Esc also closes dialogs and the / menu"
            };
            let quit = if managed {
                "Ctrl-C requests cancellation, clears a draft (Ctrl-R restores), then detaches · Ctrl-D detaches on empty"
            } else {
                "Ctrl-C stops a live turn, clears a draft (Ctrl-R restores), quits when idle · Ctrl-D quits on empty"
            };
            let newline = if shift_enter {
                "Enter send · Shift-Enter, Alt-Enter or Ctrl-J newline"
            } else {
                "Enter send · Alt-Enter or Ctrl-J newline (Shift-Enter needs the kitty keyboard protocol)"
            };
            let mode_keys = if managed {
                "Ctrl-P managed tasks · Ctrl-R prompt history · Ctrl-G editor"
            } else {
                "Ctrl-P models · Ctrl-R prompt history · Ctrl-G editor"
            };
            let lifecycle = if managed {
                "Closing xcb never implies worker settlement; background tasks keep running"
            } else {
                "Direct sessions stay bound to this terminal; use plain `xcb` for managed work"
            };
            let commands = if managed {
                [
                    "/tasks /t · /new /n · /attach <path> · /sessions /s",
                    "/help /h · /quit /q · /exit /e",
                    "`remember: …` saves a preference · ask `agent messages`",
                ]
            } else {
                [
                    "/tasks /t · /new /n · /model /m · /accounts /a · /sessions /s · /pane /p",
                    "/pane [edit|generate …] · /attach <path> · /default /d · /help /h",
                    "/plugin <name> on|off · /reload /r · /quit /q · /exit /e",
                ]
            };
            let block = Block::bordered()
                .title(" Keyboard & commands ")
                .title_bottom(" Esc closes · ? closes and types ? when ? opened it ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(
                    [
                        newline,
                        "Ctrl-V paste text/image · Alt-Backspace remove last attachment · pastes over 256 KiB are refused",
                        "PageUp older · PageDown newer · Shift-End or Ctrl-End follows newest (plain End edits a draft)",
                        "/mouse turns wheel scrolling on; while on, hold Shift (Option on macOS) to select text",
                        "Ctrl-T thinking · Ctrl-O history · Ctrl-U tool output · these and Ctrl-G/P/L/R replace readline keys",
                        "? on an empty line or F1 opens this help · Ctrl-A/E line ends · Ctrl-W/K delete word/line end",
                        mode_keys,
                        cancel,
                        quit,
                        lifecycle,
                        "Pickers: ↑↓ or Ctrl-P/N move · PgUp/PgDn page · Home/End ends",
                        "Ctrl-U clears the filter · Enter selects · Esc closes",
                        "",
                        "Type / for the command menu — arrows choose, Tab completes, Enter runs.",
                        commands[0],
                        commands[1],
                        commands[2],
                    ]
                    .join("\n"),
                )
                .wrap(Wrap { trim: false }),
                inner,
            );
        }
        Modal::Picker {
            title,
            query,
            items,
            selected,
        } => {
            cache.editor_open = false;
            let title = format!(" {title} · {query} ");
            let block = Block::bordered()
                .title(title.clone())
                .title_bottom(" Type to filter · Enter selects · Esc closes ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            // The query lives in the title row; the hardware cursor tracks
            // its end so typed input lands where the user is looking.
            frame.set_cursor_position(Position::new(
                area.x
                    .saturating_add(title.cell_width())
                    .min(area.right().saturating_sub(2)),
                area.y,
            ));
            let visible: Vec<_> = items
                .iter()
                .filter(|item| item.label.to_lowercase().contains(&query.to_lowercase()))
                .map(|item| ListItem::new(clean(&item.label)))
                .collect();
            *selected = (*selected).min(visible.len().saturating_sub(1));
            let mut state =
                ListState::default().with_selected((!visible.is_empty()).then_some(*selected));
            frame.render_stateful_widget(
                List::new(visible)
                    .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White))
                    .highlight_symbol("› "),
                inner,
                &mut state,
            );
        }
        Modal::Inspect {
            title,
            lines,
            scroll,
        } => {
            cache.editor_open = false;
            let block = Block::bordered()
                .title(format!(" {title} "))
                .title_bottom(" ↑↓ scroll · PgUp/PgDn page · Home/End ends · Esc closes ");
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let body: Vec<Line<'static>> = lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let style = if index == 0 {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    };
                    Line::from(Span::styled(expand_tabs(line), style))
                })
                .collect();
            let rows = wrap_rows(&body, inner.width.max(1));
            let max_scroll = rows.len().saturating_sub(inner.height as usize);
            *scroll = (*scroll).min(u16::try_from(max_scroll).unwrap_or(u16::MAX));
            render_rows(frame, inner, &rows, 0, *scroll as usize);
        }
        Modal::Editor {
            title,
            textarea,
            kind,
            error,
        } => {
            // A prompt editor clones the composer textarea (scroll state
            // included), so it inherits the composer's mirrored top; a pane
            // editor is a fresh textarea starting at the origin.
            if !cache.editor_open {
                cache.editor_scroll = if matches!(kind, EditorKind::Prompt) {
                    cache.composer_scroll
                } else {
                    (0, 0)
                };
                cache.editor_open = true;
            }
            textarea.set_block(
                Block::bordered()
                    .title(format!(" {title} "))
                    .title_bottom(" Ctrl-S saves · Esc cancels · no code is executed "),
            );
            textarea.set_cursor_line_style(Style::default());
            frame.render_widget(&**textarea, area);
            place_textarea_cursor(frame, textarea, area, &mut cache.editor_scroll);
            if let Some(error) = error {
                frame.render_widget(
                    Paragraph::new(clean(error)).style(Style::default().fg(Color::Yellow)),
                    Rect::new(
                        area.x + 1,
                        area.y + area.height.saturating_sub(2),
                        area.width.saturating_sub(2),
                        1,
                    ),
                );
            }
        }
    }
}
