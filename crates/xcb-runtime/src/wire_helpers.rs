//! Bounded wire helpers shared by the provider codecs, the process frame
//! reader, and the broker relays. Every check fails closed; each denial
//! reason stays caller-owned so the codecs keep their exact protocol-error
//! text in diagnostics.
use crate::{Error, Result};
use serde_json::Value;
use std::io::{BufRead, Write};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use xcb_core::usage::COUNTER_LIMIT;

pub(crate) fn require(ok: bool, reason: &'static str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Protocol(reason))
    }
}

/// Bounded provider string; `reason` is the caller's exact bound denial.
pub(crate) fn text<'a>(value: &'a Value, max: usize, reason: &'static str) -> Result<&'a str> {
    value
        .as_str()
        .filter(|s| s.len() <= max)
        .ok_or(Error::Protocol(reason))
}

/// Non-empty, control-free identifier built on `text`; both denials are the
/// caller's exact reasons.
pub(crate) fn identity(
    value: &Value,
    text_reason: &'static str,
    identity_reason: &'static str,
) -> Result<String> {
    let value = text(value, 160, text_reason)?;
    require(
        !value.is_empty() && !value.chars().any(char::is_control),
        identity_reason,
    )?;
    Ok(value.into())
}

/// RPC id key: a bounded string or an integer, serialized verbatim for
/// set/map identity. The reasons preserve each codec's denial text.
pub(crate) fn rpc_key(
    value: &Value,
    text_reason: &'static str,
    identity_reason: &'static str,
    rpc_reason: &'static str,
) -> Result<String> {
    if value.is_string() {
        identity(value, text_reason, identity_reason)?;
    } else {
        require(value.as_i64().is_some(), rpc_reason)?;
    }
    Ok(serde_json::to_string(value)?)
}

/// Cumulative counter bounded by the shared telemetry limit.
pub(crate) fn counter(value: &Value, reason: &'static str) -> Result<u64> {
    value
        .as_u64()
        .filter(|n| *n <= COUNTER_LIMIT)
        .ok_or(Error::Protocol(reason))
}

/// `null` maps to zero; anything else must be a bounded counter.
pub(crate) fn counter_or_null(value: &Value, reason: &'static str) -> Result<u64> {
    if value.is_null() {
        Ok(0)
    } else {
        counter(value, reason)
    }
}

/// Bounded string object with a caller-owned bound denial.
pub(crate) fn object<'a>(
    value: &'a Value,
    reason: &'static str,
) -> Result<&'a serde_json::Map<String, Value>> {
    value
        .as_object()
        .filter(|v| v.len() <= 256)
        .ok_or(Error::Protocol(reason))
}

/// Object admitted field set under the shared 256-key bound. Non-object,
/// oversized and unknown-field denials each keep the caller's exact reason so
/// a codec can map more than one shape onto one message.
pub(crate) fn closed(
    value: &Value,
    keys: &[&str],
    object_reason: &'static str,
    bound_reason: &'static str,
    field_reason: &'static str,
) -> Result<()> {
    let object = value.as_object().ok_or(Error::Protocol(object_reason))?;
    require(object.len() <= 256, bound_reason)?;
    require(
        object.keys().all(|key| keys.contains(&key.as_str())),
        field_reason,
    )
}

/// Bounded text of a required object field.
pub(crate) fn field_text<'a>(
    value: &'a Value,
    key: &str,
    max: usize,
    missing: &'static str,
    bound: &'static str,
) -> Result<&'a str> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::Protocol(missing))?;
    if text.len() > max {
        return Err(Error::Protocol(bound));
    }
    Ok(text)
}

/// Required u64 field.
pub(crate) fn field_counter(value: &Value, key: &str, reason: &'static str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(Error::Protocol(reason))
}

/// Absent or null u64 field maps to zero; anything else must be a counter.
pub(crate) fn optional_field_counter(
    value: &Value,
    key: &str,
    reason: &'static str,
) -> Result<u64> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(value) => value.as_u64().ok_or(Error::Protocol(reason)),
    }
}

/// Bounded newline-delimited frame from an async buffered reader. `buffer`
/// retains partial bytes across calls so a reader selecting between channels
/// never loses data; EOF mid-frame is `incomplete`, never a short frame.
pub(crate) async fn frame<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    max: usize,
    bound: &'static str,
    incomplete: &'static str,
) -> Result<Option<Vec<u8>>> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if buffer.is_empty() {
                Ok(None)
            } else {
                Err(Error::Protocol(incomplete))
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |end| end + 1);
        if buffer.len() + count > max {
            return Err(Error::Protocol(bound));
        }
        buffer.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(std::mem::take(buffer)));
        }
    }
}

/// Sync twin of `frame` for the blocking stdio relays.
pub(crate) fn frame_sync<R: BufRead>(
    reader: &mut R,
    max: usize,
    bound: &'static str,
    incomplete: &'static str,
) -> Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if out.is_empty() {
                Ok(None)
            } else {
                Err(Error::Protocol(incomplete))
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |end| end + 1);
        if out.len() + count > max {
            return Err(Error::Protocol(bound));
        }
        out.extend_from_slice(&available[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(out));
        }
    }
}

/// One bounded JSON value as a newline-delimited frame on an async writer.
pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
    max: usize,
    reason: &'static str,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() > max {
        return Err(Error::Protocol(reason));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Sync twin of `write_frame` for the blocking stdio relays; the caller owns
/// the bound, which host-authored frames never approach.
pub(crate) fn write_frame_sync<W: Write>(
    writer: &mut W,
    value: &Value,
    max: usize,
    reason: &'static str,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() > max {
        return Err(Error::Protocol(reason));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}
