use crate::{App, composer::ComposerAction};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    buffer::CellWidth,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use xcb_core::{
    display_text,
    session::State,
    ui::{AgentRow, TranscriptContext},
};

const CARD_HEIGHT: u16 = 6;
const MAX_AGENTS: usize = 128;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Scope {
    #[default]
    Project,
    All,
}

struct PaintedCard {
    area: Rect,
    row: AgentRow,
}

pub(crate) struct AgentGrid {
    visible: bool,
    scope: Scope,
    focused: bool,
    selected: Option<TranscriptContext>,
    offset: usize,
    columns: usize,
    page_rows: usize,
    area: Rect,
    cards: Vec<PaintedCard>,
    order: Vec<TranscriptContext>,
}

impl Default for AgentGrid {
    fn default() -> Self {
        Self {
            visible: true,
            scope: Scope::Project,
            focused: false,
            selected: None,
            offset: 0,
            columns: 1,
            page_rows: 1,
            area: Rect::default(),
            cards: Vec::new(),
            order: Vec::new(),
        }
    }
}

fn workspace(app: &App) -> Option<&str> {
    app.view
        .session
        .as_ref()
        .map(|session| session.workspace.as_str())
        .or_else(|| {
            let current = app.view.conversation.as_ref()?;
            app.view
                .conversations
                .iter()
                .find(|row| &row.id == current)
                .map(|row| row.workspace.as_str())
        })
        .or_else(|| {
            let context = app
                .view
                .conversation
                .as_ref()
                .map(|id| TranscriptContext::Conversation(id.clone()))?;
            app.view
                .agents
                .iter()
                .find(|row| row.context == context)
                .map(|row| row.workspace.as_str())
        })
        .filter(|path| !path.is_empty())
}

fn rows(app: &App) -> Vec<&AgentRow> {
    let project = workspace(app);
    app.view
        .agents
        .iter()
        .filter(|row| {
            app.agent_grid.scope == Scope::All || project.is_some_and(|path| path == row.workspace)
        })
        .take(MAX_AGENTS)
        .collect()
}

fn columns(width: u16) -> usize {
    usize::from((width.saturating_add(1) / 33).clamp(1, 4))
}

pub(crate) fn grid_height(app: &App, transcript: Rect, total_height: u16) -> u16 {
    if !app.agent_grid.visible || app.view.agents.is_empty() {
        return 0;
    }
    let budget = (total_height / 2).min(transcript.height.saturating_sub(3));
    if budget == 0 {
        return 0;
    }
    let count = rows(app).len();
    if count == 0 || budget < CARD_HEIGHT + 1 {
        return 1;
    }
    let needed = count.div_ceil(columns(transcript.width));
    let visible = usize::from((budget - 1) / CARD_HEIGHT).min(needed);
    1 + visible as u16 * CARD_HEIGHT
}

pub(crate) fn is_visible(app: &App) -> bool {
    app.agent_grid.visible && !app.view.agents.is_empty()
}

pub(crate) fn clear_geometry(app: &mut App) {
    app.agent_grid.area = Rect::default();
    app.agent_grid.cards.clear();
}

fn clean_line(text: &str, bound: usize) -> String {
    display_text(text, bound).replace(['\n', '\r', '\t'], " ")
}

fn ellipsis(text: &str, width: u16) -> String {
    if text.cell_width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut cells = 0;
    for grapheme in text.graphemes(true) {
        let next = grapheme.cell_width();
        if cells + next > width - 1 {
            break;
        }
        result.push_str(grapheme);
        cells += next;
    }
    result.push('…');
    result
}

fn category_color(row: &AgentRow) -> Color {
    match row.category.as_deref() {
        Some("done" | "idle") => return Color::Green,
        Some("failed" | "uncertain" | "needs recovery") => return Color::Red,
        Some("cancelled") => return Color::DarkGray,
        Some(
            "question" | "confirm" | "needs_action" | "needs_approval" | "blocked" | "limited"
            | "needs answer" | "needs action" | "needs approval" | "usage limit",
        ) => return Color::Yellow,
        Some("stopped_short" | "interrupted" | "working") => return Color::Cyan,
        _ => (),
    }
    state_color(row.state)
}

fn state_color(state: State) -> Color {
    match state {
        State::NeedsAnswer | State::NeedsAction | State::NeedsApproval | State::Limited => {
            Color::Yellow
        }
        State::Failed | State::Uncertain => Color::Red,
        State::Cancelled => Color::DarkGray,
        State::Working => Color::Cyan,
        State::Idle => Color::Green,
    }
}

fn status(row: &AgentRow, ticks: u64, reduced_motion: bool) -> String {
    let activity = clean_line(&row.activity, 120);
    let label = if activity.is_empty() {
        row.state.label().to_owned()
    } else {
        activity
    };
    if row.state == State::Working {
        let marker = if reduced_motion || label.to_ascii_lowercase().starts_with("queued") {
            "●"
        } else {
            SPINNER[(ticks / 3) as usize % SPINNER.len()]
        };
        format!("{marker} {label}")
    } else {
        label
    }
}

pub(crate) fn render(frame: &mut Frame<'_>, app: &mut App, area: Rect, ticks: u64) {
    clear_geometry(app);
    if area.height == 0 || area.width == 0 || !app.agent_grid.visible {
        return;
    }
    let items: Vec<_> = rows(app).into_iter().cloned().collect();
    let count = items.len();
    let compact = area.height < CARD_HEIGHT + 1;
    let cols = if compact { 1 } else { columns(area.width) };
    let page_rows = usize::from(area.height.saturating_sub(1) / CARD_HEIGHT).max(1);
    let total_rows = count.div_ceil(cols);
    let grid = &mut app.agent_grid;
    let order: Vec<_> = items.iter().map(|row| row.context.clone()).collect();
    if (order != grid.order || cols != grid.columns)
        && let Some(anchor) = grid.order.get(grid.offset * grid.columns)
        && let Some(index) = order.iter().position(|context| context == anchor)
    {
        grid.offset = index / cols;
    }
    grid.order = order;
    grid.area = area;
    grid.columns = cols;
    grid.page_rows = page_rows;
    grid.offset = grid.offset.min(total_rows.saturating_sub(page_rows));
    if grid
        .selected
        .as_ref()
        .is_none_or(|selected| !items.iter().any(|row| &row.context == selected))
    {
        grid.selected = items.first().map(|row| row.context.clone());
    }
    if grid.focused
        && let Some(index) = items
            .iter()
            .position(|row| Some(&row.context) == grid.selected.as_ref())
    {
        grid.offset = grid.offset.min(index / cols);
        if index / cols >= grid.offset + page_rows {
            grid.offset = (index / cols + 1).saturating_sub(page_rows);
        }
    }
    frame.render_widget(Clear, area);
    let scope = if grid.scope == Scope::All {
        "all"
    } else {
        "project"
    };
    let range = if total_rows > page_rows {
        format!(
            " · rows {}–{}/{} ↕",
            grid.offset + 1,
            (grid.offset + page_rows).min(total_rows),
            total_rows
        )
    } else {
        String::new()
    };
    let control = if grid.focused {
        "Enter reference · Esc chat"
    } else {
        "F6 browse"
    };
    let heading = if count == 0 {
        format!("Agents · {scope} · no agents · /overview all")
    } else {
        format!("Agents · {scope} · {count}{range} · {control}")
    };
    let header = Rect::new(area.x, area.y, area.width, 1);
    frame.render_widget(
        Paragraph::new(heading).style(Style::default().add_modifier(if grid.focused {
            Modifier::BOLD
        } else {
            Modifier::DIM
        })),
        header,
    );
    if compact {
        let row = if grid.focused {
            items
                .iter()
                .find(|row| Some(&row.context) == grid.selected.as_ref())
        } else {
            items.get(grid.offset)
        };
        if let Some(row) = row {
            let prefix = if area.width >= 60 {
                format!("Agents · {scope} · {}/{count} · ", grid.offset + 1)
            } else {
                "Agents · ".into()
            };
            let status = ellipsis(
                &status(row, ticks, app.view.reduced_motion),
                (area.width / 3).max(7),
            );
            let suffix = format!(" · {status} · F6");
            let title_width = area
                .width
                .saturating_sub(prefix.cell_width() + suffix.cell_width());
            let title = ellipsis(&clean_line(&row.title, 160), title_width);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, Style::default().add_modifier(Modifier::DIM)),
                    Span::raw(title),
                    Span::styled(suffix, Style::default().fg(state_color(row.state))),
                ])),
                header,
            );
            grid.cards.push(PaintedCard {
                area: header,
                row: row.clone(),
            });
        }
        return;
    }
    let cell_width = (area.width.saturating_sub(cols.saturating_sub(1) as u16)) / cols as u16;
    let first = grid.offset * cols;
    for (index, row) in items.iter().enumerate().skip(first).take(page_rows * cols) {
        let column = (index - first) % cols;
        let line = (index - first) / cols;
        let x = area.x + column as u16 * (cell_width + 1);
        let width = if column + 1 == cols {
            area.right().saturating_sub(x)
        } else {
            cell_width
        };
        let card = Rect::new(
            x,
            area.y + 1 + line as u16 * CARD_HEIGHT,
            width,
            CARD_HEIGHT,
        );
        let selected = grid.focused && grid.selected.as_ref() == Some(&row.context);
        let border = if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(Line::from(Span::styled(
                clean_line(&row.title, 160),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        let inner = block.inner(card);
        frame.render_widget(block, card);
        frame.render_widget(
            Paragraph::new(clean_line(
                row.model.as_deref().unwrap_or("not routed yet"),
                160,
            ))
            .style(Style::default().add_modifier(Modifier::DIM)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let mut state = status(row, ticks, app.view.reduced_motion);
        if let Some(category) = row
            .category
            .as_deref()
            .filter(|category| !category.is_empty())
        {
            let category = match category {
                "idle" | "done" => "completed".to_owned(),
                "needs answer" | "question" => "question".to_owned(),
                "confirm" => "confirmation".to_owned(),
                _ => clean_line(category, 100).replace('_', " "),
            };
            if !state.contains(&category) {
                state.push_str(" · ");
                state.push_str(&category);
            }
        }
        frame.render_widget(
            Paragraph::new(state).style(Style::default().fg(state_color(row.state))),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
        let response = if row.response.trim().is_empty() {
            "No response yet".to_owned()
        } else {
            clean_line(&row.response, 2048)
        };
        frame.render_widget(
            Paragraph::new(response)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(category_color(row))),
            Rect::new(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(2),
            ),
        );
        grid.cards.push(PaintedCard {
            area: card,
            row: row.clone(),
        });
    }
}

fn reference(row: &AgentRow) -> String {
    let target = match &row.context {
        TranscriptContext::Conversation(id) => format!("@conversation:{id}"),
        TranscriptContext::Session(id) => format!("@session:{id}"),
    };
    let task = row
        .task
        .as_ref()
        .map(|id| format!(" @task:{id}"))
        .unwrap_or_default();
    let title = serde_json::to_string(&clean_line(&row.title, 160)).expect("string serializes");
    let workspace =
        serde_json::to_string(&clean_line(&row.workspace, 4096)).expect("string serializes");
    let preview =
        serde_json::to_string(&clean_line(&row.response, 512)).expect("string serializes");
    format!(" {target}{task} ({title}; workspace: {workspace}; response snapshot: {preview}) ")
}

impl App {
    pub(crate) fn overview_focused(&self) -> bool {
        self.agent_grid.focused
    }

    pub(crate) fn overview_animating(&self) -> bool {
        self.agent_grid.visible
            && !self.view.reduced_motion
            && rows(self).into_iter().any(|row| {
                row.state == State::Working
                    && !row.activity.to_ascii_lowercase().starts_with("queued")
            })
    }

    pub(crate) fn overview_command(&mut self, argument: &str) {
        match argument.trim() {
            "" | "show" => self.agent_grid.visible = true,
            "hide" => {
                self.agent_grid.visible = false;
                self.agent_grid.focused = false;
                clear_geometry(self);
            }
            "project" | "all" => {
                self.agent_grid.scope = if argument.trim() == "all" {
                    Scope::All
                } else {
                    Scope::Project
                };
                self.agent_grid.visible = true;
                self.agent_grid.offset = 0;
                self.agent_grid.selected = None;
                self.agent_grid.order.clear();
                clear_geometry(self);
            }
            _ => self.notice = "/overview [project|all|hide|show]".into(),
        }
        self.dirty = true;
    }

    fn insert_agent_reference(&mut self, row: AgentRow) {
        // Use the painted identity, not the row now occupying its screen slot.
        if !rows(self)
            .iter()
            .any(|current| current.context == row.context && current.task == row.task)
        {
            self.notice =
                "That agent changed. The overview will refresh; your draft is unchanged.".into();
            return;
        }
        match self.composer.handle(Event::Paste(reference(&row))) {
            ComposerAction::Rejected(reason) => self.notice = reason.into(),
            ComposerAction::None => {
                self.agent_grid.focused = false;
                self.notice = "Agent reference added to your draft. Enter sends when ready.".into();
            }
            _ => unreachable!("a paste cannot submit input"),
        }
        self.dirty = true;
    }

    pub(crate) fn overview_event(&mut self, event: &Event) -> bool {
        if matches!(event, Event::Resize(_, _)) {
            clear_geometry(self);
            return false;
        }
        if self.modal.is_some() {
            return false;
        }
        if let Event::Mouse(mouse) = event {
            if self.slash_menu().is_some() {
                return true;
            }
            if !self.mouse_capture
                || !self.agent_grid.visible
                || !self
                    .agent_grid
                    .area
                    .contains(Position::new(mouse.column, mouse.row))
            {
                return false;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let count = rows(self).len();
                    let grid = &mut self.agent_grid;
                    let maximum = count.div_ceil(grid.columns).saturating_sub(grid.page_rows);
                    grid.offset = if mouse.kind == MouseEventKind::ScrollUp {
                        grid.offset.saturating_sub(1)
                    } else {
                        (grid.offset + 1).min(maximum)
                    };
                    // Wheel browsing leaves editing in the composer. A prior
                    // keyboard selection must not snap the viewport back.
                    grid.focused = false;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(row) = self
                        .agent_grid
                        .cards
                        .iter()
                        .find(|card| card.area.contains(Position::new(mouse.column, mouse.row)))
                        .map(|card| card.row.clone())
                    {
                        self.insert_agent_reference(row);
                    }
                }
                _ => (),
            }
            return true;
        }
        let Event::Key(key) = event else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.code == KeyCode::F(6) && key.modifiers.is_empty() {
            self.agent_grid.visible = true;
            if rows(self).is_empty() {
                self.agent_grid.focused = false;
                self.notice = "No agents in this view. /overview all shows other projects.".into();
                return true;
            }
            self.agent_grid.focused = !self.agent_grid.focused;
            if self.agent_grid.focused {
                let _ = self.slash_menu();
                self.slash_dismissed.set(true);
                if !self
                    .agent_grid
                    .cards
                    .iter()
                    .any(|card| Some(&card.row.context) == self.agent_grid.selected.as_ref())
                {
                    self.agent_grid.selected = self
                        .agent_grid
                        .cards
                        .first()
                        .map(|card| card.row.context.clone());
                }
            }
            return true;
        }
        if !self.agent_grid.focused {
            return false;
        }
        if key.code == KeyCode::Esc {
            self.agent_grid.focused = false;
            return true;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.agent_grid.focused = false;
            return false;
        }
        let items: Vec<_> = rows(self).into_iter().cloned().collect();
        let current = items
            .iter()
            .position(|row| Some(&row.context) == self.agent_grid.selected.as_ref())
            .unwrap_or(0);
        if key.code == KeyCode::Enter {
            if let Some(row) = self
                .agent_grid
                .cards
                .iter()
                .find(|card| Some(&card.row.context) == self.agent_grid.selected.as_ref())
                .map(|card| card.row.clone())
            {
                self.insert_agent_reference(row);
            } else {
                self.notice =
                    "The overview changed. Let it refresh before inserting a reference.".into();
            }
            return true;
        }
        let columns = self.agent_grid.columns;
        let page = columns * self.agent_grid.page_rows;
        let next = match key.code {
            KeyCode::Left => current.saturating_sub(1),
            KeyCode::Right => current.saturating_add(1),
            KeyCode::Up => current.saturating_sub(columns),
            KeyCode::Down => current.saturating_add(columns),
            KeyCode::PageUp => current.saturating_sub(page),
            KeyCode::PageDown => current.saturating_add(page),
            KeyCode::Home => 0,
            KeyCode::End => items.len().saturating_sub(1),
            _ => {
                self.agent_grid.focused = false;
                return false;
            }
        };
        self.agent_grid.selected = items
            .get(next.min(items.len().saturating_sub(1)))
            .map(|row| row.context.clone());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::mpsc::sync_channel;
    use xcb_core::{
        Id,
        panes::{Node, Pane, Source},
        ui::ConversationRow,
    };

    fn id(value: &str) -> Id {
        Id::new(value).unwrap()
    }

    fn fixture(count: usize) -> App {
        let mut app = App::default();
        app.view.conversation = Some(id("conversation_0"));
        app.view.conversations.push(ConversationRow {
            id: id("conversation_0"),
            title: "Current project".into(),
            workspace: "/project".into(),
            messages: 0,
            updated_at_ms: 1,
        });
        app.view.pane = Pane {
            version: 1,
            id: id("only-responses"),
            title: "Responses".into(),
            root: Node::Widget {
                source: Source::Responses,
                lines: None,
            },
        };
        app.view.agents = (0..count)
            .map(|index| AgentRow {
                context: TranscriptContext::Conversation(id(&format!("conversation_{index}"))),
                task: Some(id(&format!("task_{index}"))),
                title: format!("Agent {index}"),
                workspace: "/project".into(),
                model: Some("Astra".into()),
                state: State::Working,
                activity: "thinking".into(),
                response: format!("Response {index}"),
                category: Some("done".into()),
                updated_at_ms: 1,
            })
            .collect();
        app
    }

    fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::render::draw(frame, app, 12))
            .unwrap();
        terminal
    }

    fn text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn grid_fits_half_the_terminal_preserves_input_and_grows_with_rows() {
        let mut one = fixture(1);
        draw(&mut one, 120, 40);
        assert_eq!(one.agent_grid.area.height, 7);
        let mut many = fixture(128);
        many.composer
            .set_text("draft\nwith\nmany\nlines\nof\ninput");
        let terminal = draw(&mut many, 120, 40);
        assert!(many.agent_grid.area.height <= 20);
        assert!(many.viewport_height.get() >= 3);
        assert!(text(&terminal).contains("input"));
        assert!(text(&terminal).contains("rows 1–3/43"));
        assert_eq!(many.agent_grid.cards.len(), 9);
        let narrow = draw(&mut many, 32, 18);
        assert_eq!(many.agent_grid.area.height, 1);
        assert!(text(&narrow).contains("Agents"));
        assert!(many.viewport_height.get() >= 3);
    }

    #[test]
    fn tiny_sizes_and_unicode_are_safe_and_clear_old_hit_rectangles() {
        let mut app = fixture(4);
        app.view.agents[0].title = "界面👨‍👩‍👧‍👦e\u{301}\u{1b}[31m\u{202e}title".into();
        app.view.agents[0].response = "界🌱\t response\n".repeat(1000);
        for (width, height) in [(200, 50), (80, 24), (24, 7), (1, 1), (0, 0)] {
            let terminal = draw(&mut app, width, height);
            let content = text(&terminal);
            assert!(!content.contains('\u{1b}'));
            assert!(!content.contains('\u{202e}'));
            assert!(app.agent_grid.area.height <= height / 2);
            if width < 24 || height < 7 {
                assert!(app.agent_grid.cards.is_empty());
            }
        }
    }

    #[test]
    fn default_scope_never_widens_when_project_identity_is_missing() {
        let mut app = fixture(3);
        app.view.agents[1].workspace = "/other".into();
        assert_eq!(rows(&app).len(), 2);
        app.view.conversations.clear();
        assert_eq!(
            rows(&app).len(),
            2,
            "focused row identifies an older project"
        );
        app.view.agents.remove(0);
        assert!(rows(&app).is_empty());
        let screen = draw(&mut app, 80, 24);
        assert!(text(&screen).contains("no agents"));
        app.overview_command("all");
        assert_eq!(rows(&app).len(), 2);
        app.overview_command("hide");
        assert_eq!(grid_height(&app, Rect::new(0, 0, 80, 20), 24), 0);
        assert!(!app.overview_animating());
    }

    #[test]
    fn keyboard_selection_is_stable_and_enter_adds_reference_without_dispatch() {
        let mut app = fixture(20);
        let (tx, rx) = sync_channel(4);
        app.composer.set_text("Please inspect ");
        draw(&mut app, 80, 24);
        app.handle(key(KeyCode::F(6)), &tx);
        app.handle(key(KeyCode::Down), &tx);
        assert_eq!(
            app.agent_grid.selected,
            Some(TranscriptContext::Conversation(id("conversation_2")))
        );
        app.view.agents.reverse();
        draw(&mut app, 80, 24);
        assert_eq!(
            app.agent_grid.selected,
            Some(TranscriptContext::Conversation(id("conversation_2")))
        );
        app.handle(key(KeyCode::Enter), &tx);
        assert!(app.composer.text().starts_with("Please inspect "));
        assert!(
            app.composer
                .text()
                .contains("@conversation:conversation_2 @task:task_2")
        );
        assert!(
            app.composer
                .text()
                .contains("response snapshot: \"Response 2\"")
        );
        assert!(!app.overview_focused());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn clicking_uses_the_painted_identity_and_rejects_a_changed_task() {
        let mut app = fixture(6);
        app.mouse_capture = true;
        draw(&mut app, 80, 24);
        let card = app.agent_grid.cards[0].area;
        app.view.agents.reverse();
        assert!(app.overview_event(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.x + 1,
            card.y + 1
        )));
        assert!(app.composer.text().contains("@conversation:conversation_0"));
        app.composer.set_text("retained");
        app.view
            .agents
            .iter_mut()
            .find(|row| row.context == TranscriptContext::Conversation(id("conversation_0")))
            .unwrap()
            .task = Some(id("different_task"));
        app.overview_event(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            card.x + 1,
            card.y + 1,
        ));
        assert_eq!(app.composer.text(), "retained");
        assert!(app.notice.contains("agent changed"));
    }

    #[test]
    fn keyboard_reference_never_retargets_a_removed_or_replaced_selection() {
        for changed_task in [false, true] {
            let mut app = fixture(2);
            app.composer.set_text("keep");
            draw(&mut app, 80, 24);
            app.overview_event(&key(KeyCode::F(6)));
            if changed_task {
                app.view.agents[0].task = Some(id("new_task"));
            } else {
                app.view.agents.remove(0);
            }
            assert!(app.overview_event(&key(KeyCode::Enter)));
            assert_eq!(app.composer.text(), "keep");
        }
        let mut app = fixture(1);
        draw(&mut app, 80, 24);
        app.overview_event(&key(KeyCode::F(6)));
        app.overview_event(&Event::Resize(40, 15));
        app.overview_event(&key(KeyCode::Enter));
        assert!(app.composer.text().is_empty());
    }

    #[test]
    fn slash_commands_paint_above_the_grid_and_block_click_through() {
        let mut app = fixture(30);
        app.mouse_capture = true;
        app.composer.set_text("/");
        let screen = draw(&mut app, 80, 20);
        let content = text(&screen);
        let popup_title = format!(" commands · {} matches ", app.slash_matches().len());
        assert!(content.contains(&popup_title));
        assert!(content.contains(app.slash_matches()[0].name));
        assert!(app.overview_event(&mouse(MouseEventKind::Down(MouseButton::Left), 3, 6)));
        assert_eq!(app.composer.text(), "/");
        assert!(app.overview_event(&key(KeyCode::F(6))));
        assert!(app.slash_menu().is_none());
        let screen = draw(&mut app, 80, 20);
        // The empty transcript also says "/help for commands · /pane".
        // Check the command popup itself, not that unrelated chat hint.
        assert!(!text(&screen).contains(&popup_title));
        app.overview_event(&key(KeyCode::Esc));
        app.composer.handle(key(KeyCode::Char('o')));
        assert!(app.slash_menu().is_some());
    }

    #[test]
    fn pointer_scroll_is_local_and_resize_invalidates_clicks() {
        let mut app = fixture(30);
        app.mouse_capture = true;
        draw(&mut app, 80, 24);
        let area = app.agent_grid.area;
        let transcript = app.scroll.get();
        app.overview_event(&mouse(MouseEventKind::ScrollDown, area.x + 2, area.y + 2));
        assert_eq!(app.agent_grid.offset, 1);
        assert_eq!(app.scroll.get(), transcript);
        assert!(!app.overview_event(&mouse(
            MouseEventKind::ScrollDown,
            area.x,
            area.bottom() + 1
        )));
        app.overview_event(&Event::Resize(40, 12));
        assert!(!app.overview_event(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y + 2
        )));
        assert!(app.composer.text().is_empty());
        draw(&mut app, 40, 12);
        assert_eq!(app.agent_grid.area.height, 1);
        app.overview_event(&mouse(MouseEventKind::ScrollDown, 1, 1));
        let screen = draw(&mut app, 40, 12);
        assert!(text(&screen).contains("Agent 3"));
    }

    #[test]
    fn scrolling_clamps_on_removal_and_retains_visible_anchor_on_reorder() {
        let mut app = fixture(30);
        app.mouse_capture = true;
        draw(&mut app, 80, 24);
        app.overview_event(&mouse(MouseEventKind::ScrollDown, 2, 2));
        draw(&mut app, 80, 24);
        let anchor = app.agent_grid.cards[0].row.context.clone();
        app.view.agents.rotate_right(2);
        draw(&mut app, 80, 24);
        assert_eq!(app.agent_grid.cards[0].row.context, anchor);
        app.view.agents.truncate(1);
        draw(&mut app, 80, 24);
        assert_eq!(app.agent_grid.offset, 0);
        assert_eq!(app.agent_grid.cards.len(), 1);
    }

    #[test]
    fn width_changes_preserve_the_first_visible_identity() {
        let mut app = fixture(30);
        app.mouse_capture = true;
        draw(&mut app, 160, 40);
        app.overview_event(&mouse(MouseEventKind::ScrollDown, 2, 2));
        draw(&mut app, 160, 40);
        let anchor = app.agent_grid.cards[0].row.context.clone();
        app.overview_event(&Event::Resize(32, 40));
        draw(&mut app, 32, 40);
        assert_eq!(app.agent_grid.cards[0].row.context, anchor);
    }

    #[test]
    fn reference_insertion_respects_cursor_selection_and_input_limit() {
        let mut app = fixture(1);
        app.composer.set_text("before AFTER tail");
        app.composer
            .textarea
            .move_cursor(ratatui_textarea::CursorMove::Jump(0, 7));
        app.composer.textarea.start_selection();
        app.composer
            .textarea
            .move_cursor(ratatui_textarea::CursorMove::Forward);
        app.composer
            .textarea
            .move_cursor(ratatui_textarea::CursorMove::Forward);
        app.insert_agent_reference(app.view.agents[0].clone());
        assert!(app.composer.text().starts_with("before  @conversation:"));
        assert!(app.composer.text().ends_with(" TER tail"));
        app.composer
            .set_text(&"x".repeat(crate::composer::MAX_INPUT));
        app.insert_agent_reference(app.view.agents[0].clone());
        assert_eq!(app.composer.text().len(), crate::composer::MAX_INPUT);
        assert!(app.notice.contains("256 KiB"));
    }

    #[test]
    fn focus_release_escape_modals_and_global_shortcuts_remain_safe() {
        let mut app = fixture(1);
        draw(&mut app, 80, 24);
        app.overview_event(&key(KeyCode::F(6)));
        let mut released = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        released.kind = KeyEventKind::Release;
        assert!(!app.overview_event(&Event::Key(released)));
        assert!(app.composer.text().is_empty());
        assert!(app.overview_event(&key(KeyCode::Esc)));
        assert!(!app.overview_focused());
        app.overview_event(&key(KeyCode::F(6)));
        assert!(!app.overview_event(&key(KeyCode::F(1))));
        app.modal = Some(crate::Modal::Help { scroll: 0 });
        app.mouse_capture = true;
        assert!(!app.overview_event(&mouse(MouseEventKind::Down(MouseButton::Left), 2, 3)));
        assert!(!app.overview_event(&key(KeyCode::F(6))));
        let screen = draw(&mut app, 80, 24);
        assert!(text(&screen).contains("Keyboard & commands"));
    }

    #[test]
    fn state_and_response_categories_have_labels_and_distinct_colors() {
        let mut app = fixture(1);
        let screen = draw(&mut app, 80, 24);
        let content = text(&screen);
        assert!(content.contains("thinking · completed"));
        let response_cell = &screen.backend().buffer()[(1, 5)];
        assert_eq!(response_cell.fg, Color::Green);
        assert!(app.overview_animating());
        app.view.reduced_motion = true;
        assert!(!app.overview_animating());
        let screen = draw(&mut app, 80, 24);
        assert!(text(&screen).contains("● thinking"));
        app.view.reduced_motion = false;
        app.view.agents[0].activity = "queued".into();
        assert!(!app.overview_animating());
    }

    #[test]
    fn a_working_direct_session_keeps_its_last_response_category_color() {
        let mut app = fixture(1);
        for (category, color) in [
            ("idle", Color::Green),
            ("needs answer", Color::Yellow),
            ("needs approval", Color::Yellow),
            ("needs action", Color::Yellow),
            ("usage limit", Color::Yellow),
            ("needs recovery", Color::Red),
            ("failed", Color::Red),
            ("cancelled", Color::DarkGray),
        ] {
            app.view.agents[0].category = Some(category.into());
            assert_eq!(category_color(&app.view.agents[0]), color);
            assert_eq!(state_color(app.view.agents[0].state), Color::Cyan);
        }
    }

    #[test]
    fn many_rows_are_bounded_and_page_navigation_reaches_the_last_agent() {
        let mut app = fixture(140);
        draw(&mut app, 160, 40);
        assert_eq!(rows(&app).len(), 128);
        app.overview_event(&key(KeyCode::F(6)));
        app.overview_event(&key(KeyCode::End));
        draw(&mut app, 160, 40);
        assert_eq!(
            app.agent_grid.selected,
            Some(TranscriptContext::Conversation(id("conversation_127")))
        );
        assert!(
            app.agent_grid
                .cards
                .iter()
                .any(|card| card.row.context == app.agent_grid.selected.clone().unwrap())
        );
        app.overview_event(&key(KeyCode::PageUp));
        assert_eq!(
            app.agent_grid.selected,
            Some(TranscriptContext::Conversation(id("conversation_115")))
        );
        app.overview_event(&key(KeyCode::Home));
        draw(&mut app, 160, 40);
        assert_eq!(app.agent_grid.offset, 0);
    }
}
