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
    let mut query = db.prepare(&format!(
        "SELECT sequence,payload FROM messages WHERE {column}=?1 AND (?2 IS NULL OR sequence<?2) ORDER BY sequence DESC LIMIT ?3"
    ))?;
    let rows = query.query_map(params![id.as_str(), before, (limit + 1) as i64], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut messages = Vec::with_capacity(limit);
    let mut first_sequence = None;
    let mut bytes = 0usize;
    let mut has_older = false;
    for row in rows {
        if messages.len() == limit {
            row?;
            has_older = true;
            break;
        }
        let (sequence, payload) = row?;
        bytes += payload.len();
        if bytes > 8 * 1024 * 1024 {
            return Err(xcb_core::Error::Limit("transcript page").into());
        }
        let message: Message = serde_json::from_str(&payload)?;
        message.validate()?;
        first_sequence = Some(
            u64::try_from(sequence).map_err(|_| Error::Conflict("invalid transcript sequence"))?,
        );
        messages.push(message);
    }
    messages.reverse();
    Ok(TranscriptPage {
        context,
        messages,
        first_sequence,
        has_older,
    })
}
