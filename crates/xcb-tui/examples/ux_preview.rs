//! Render an isolated six-session preview as terminal cell JSON for visual review.
use ratatui::{Terminal, backend::TestBackend};
use xcb_core::{
    Id,
    session::State,
    ui::{AgentRow, TranscriptContext},
};
use xcb_tui::{App, render};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    for (index, (title, state, response)) in [
        (
            "PeopleBlade · contact search",
            State::Working,
            "Adding search across people and notes",
        ),
        (
            "xcb · terminal UX",
            State::Working,
            "Checking soft wrap and pane navigation",
        ),
        (
            "Jungle · People category",
            State::NeedsAnswer,
            "Rename Relationships to People?",
        ),
        ("Slopcamera · export", State::Idle, "Export checks passed"),
        (
            "Wordcell · document search",
            State::Idle,
            "Search results now include page titles",
        ),
        (
            "Act60.me · guide",
            State::NeedsApproval,
            "Review the updated eligibility section",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        app.view.agents.push(AgentRow {
            context: TranscriptContext::Conversation(Id::new(format!("preview_{index}"))?),
            task: None,
            title: title.into(),
            workspace: format!("/preview/{index}"),
            state,
            activity: state.label().into(),
            model: Some("codex/gpt-6.1-sol".into()),
            response: response.into(),
            category: None,
            updated_at_ms: now,
        });
    }
    app.composer.set_text("In projects, rename the Relationships category to People. Keep the existing links and show me the updated navigation when it's ready.");
    let mut terminal = Terminal::new(TestBackend::new(92, 38))?;
    terminal.draw(|frame| render::draw(frame, &mut app, 8))?;
    let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell|
        serde_json::json!({"text":cell.symbol(),"fg":format!("{:?}",cell.fg),"bg":format!("{:?}",cell.bg)})
    ).collect();
    println!(
        "{}",
        serde_json::json!({"width":92,"height":38,"cells":cells})
    );
    Ok(())
}
