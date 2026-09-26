use crate::{Error, Result};
use rusqlite::{Connection, params};
use xcb_core::{
    session::Message,
    ui::{TranscriptContext, TranscriptPage},
};

/// Keep metadata independent of transcript revisions and provider leases.
pub(crate) fn title(value: &str) -> Result<String> {
    xcb_core::bounded_text(value, 4096)?;
    let clean = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let clean = xcb_core::display_text(&clean, 160);
    xcb_core::label(&clean, 160)?;
    Ok(clean)
}

pub(crate) fn page(
    db: &Connection,
    context: TranscriptContext,
    before: Option<u64>,
    limit: usize,
) -> Result<TranscriptPage> {
    if !(1..=512).contains(&limit) || before == Some(0) {
        return Err(xcb_core::Error::Invalid("transcript page").into());
    }
    let before = before
        .map(|value| {
            i64::try_from(value).map_err(|_| xcb_core::Error::Invalid("transcript cursor"))
        })
        .transpose()?;
    let (column, id) = match &context {
        TranscriptContext::Conversation(id) => ("conversation", id),
        TranscriptContext::Session(id) => ("session", id),
    };
    // Managed conversations attribute messages to tasks; the task's
    // workspace labels the message in the thread. Direct sessions have no
    // task column.
    let sql = match &context {
        TranscriptContext::Conversation(_) => {
            "SELECT m.sequence,m.payload,t.workspace FROM messages m LEFT JOIN tasks t ON t.id=m.task WHERE m.conversation=?1 AND (?2 IS NULL OR m.sequence<?2) ORDER BY m.sequence DESC LIMIT ?3".to_owned()
        }
        TranscriptContext::Session(_) => format!(
            "SELECT sequence,payload,NULL FROM messages WHERE {column}=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT ?3"
        ),
    };
    let mut query = db.prepare(&sql)?;
    let rows = query.query_map(params![id.as_str(), before, (limit + 1) as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut messages = Vec::with_capacity(limit);
    let mut sequences = Vec::with_capacity(limit);
    let mut workspaces = std::collections::BTreeMap::new();
    let mut first_sequence = None;
    let mut bytes = 0usize;
    let mut has_older = false;
    for row in rows {
        if messages.len() == limit {
            row?;
            has_older = true;
            break;
        }
        let (sequence, payload, workspace) = row?;
        bytes += payload.len();
        if bytes > 8 * 1024 * 1024 {
            return Err(xcb_core::Error::Limit("transcript page").into());
        }
        let message: Message = serde_json::from_str(&payload)?;
        message.validate()?;
        let sequence =
            u64::try_from(sequence).map_err(|_| Error::Conflict("invalid transcript sequence"))?;
        first_sequence = Some(sequence);
        if let Some(workspace) = workspace {
            workspaces.insert(sequence, workspace);
        }
        sequences.push(sequence);
        messages.push(message);
    }
    messages.reverse();
    sequences.reverse();
    Ok(TranscriptPage {
        context,
        messages,
        first_sequence,
        has_older,
        workspaces,
        sequences,
    })
}
