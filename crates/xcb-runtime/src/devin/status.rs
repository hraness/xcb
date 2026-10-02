//! Account-level Devin quota over the provider's own Connect-RPC surface.
//! `SeatManagementService/GetUserStatus` is the same call the native
//! `devin auth status` makes; the host speaks it directly with the account's
//! stored session token so the sandboxed provider process never sees the
//! credential or the response. The request and the parsed response fields are
//! a fixed, verified subset — unknown wire fields are skipped, never guessed.
use crate::{Error, Result};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// The only endpoint xcb speaks; the credential file contract pins this host.
const API_SERVER: &str = "server.codeium.com";
const PATH: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
/// xcb-identified requests get a trimmed response that still carries the full
/// plan status; only the hosted CLI identity receives the model catalog.
const IDE_NAME: &str = "xcb";

/// A provider-reported remaining-quota window and its reset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlanWindow {
    /// Provider-reported remaining percentage (0-100); callers convert to the
    /// store's `used_percent` convention.
    pub remaining_percent: f64,
    pub resets_at_ms: u64,
}

/// The subset of `GetUserStatusResponse` xcb reads: `user_status` account
/// email and `plan_status` daily/weekly quota windows plus the plan's display
/// name.
#[derive(Debug, Default)]
pub(crate) struct UserStatus {
    pub daily: Option<PlanWindow>,
    pub weekly: Option<PlanWindow>,
    pub plan_name: Option<String>,
    pub email: Option<String>,
}

fn encode_varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        out.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            return;
        }
    }
}

fn encode_string(field: u32, value: &str, out: &mut Vec<u8>) {
    encode_varint(u64::from(field << 3 | 2), out);
    encode_varint(value.len() as u64, out);
    out.extend_from_slice(value.as_bytes());
}

/// `GetUserStatusRequest { metadata: { ide_name, ide_version, api_key,
/// extension_version } }` — the four fields the service requires.
fn request_body(token: &str, version: &str) -> Vec<u8> {
    let mut metadata = Vec::with_capacity(64 + token.len());
    encode_string(1, IDE_NAME, &mut metadata);
    encode_string(2, version, &mut metadata);
    encode_string(3, token, &mut metadata);
    encode_string(7, version, &mut metadata);
    let mut body = Vec::with_capacity(metadata.len() + 8);
    encode_varint(1 << 3 | 2, &mut body);
    encode_varint(metadata.len() as u64, &mut body);
    body.extend_from_slice(&metadata);
    body
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self
                .bytes
                .get(self.offset)
                .ok_or(Error::Protocol("Devin status truncated"))?;
            self.offset += 1;
            if shift == 63 && byte > 1 {
                return Err(Error::Protocol("Devin status varint"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Read one field: its number and value. Fixed-width fields are skipped;
    /// group wire types are rejected outright.
    fn field(&mut self) -> Result<Option<(u32, Field<'a>)>> {
        if self.offset >= self.bytes.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let (number, wire) = (key >> 3, key & 0x7);
        if number == 0 {
            return Err(Error::Protocol("Devin status field"));
        }
        let fixed = |reader: &mut Self, width: usize| -> Result<Field<'a>> {
            reader.offset = reader
                .offset
                .checked_add(width)
                .filter(|end| *end <= reader.bytes.len())
                .ok_or(Error::Protocol("Devin status truncated"))?;
            Ok(Field::Fixed)
        };
        let field = match wire {
            0 => Field::Varint(self.varint()?),
            1 => fixed(self, 8)?,
            2 => {
                let length = usize::try_from(self.varint()?)
                    .map_err(|_| Error::Protocol("Devin status length"))?;
                let end = self
                    .offset
                    .checked_add(length)
                    .filter(|end| *end <= self.bytes.len())
                    .ok_or(Error::Protocol("Devin status truncated"))?;
                let payload = &self.bytes[self.offset..end];
                self.offset = end;
                Field::Bytes(payload)
            }
            5 => fixed(self, 4)?,
            _ => return Err(Error::Protocol("Devin status wire type")),
        };
        Ok(Some((number as u32, field)))
    }
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn message(bytes: &[u8], number: u32) -> Option<&[u8]> {
    let mut reader = Reader { bytes, offset: 0 };
    loop {
        match reader.field() {
            Ok(Some((field, Field::Bytes(payload)))) if field == number => return Some(payload),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}

fn varint_field(bytes: &[u8], number: u32) -> Option<u64> {
    let mut reader = Reader { bytes, offset: 0 };
    while let Ok(Some((field, value))) = reader.field() {
        if field == number
            && let Field::Varint(value) = value
        {
            return Some(value);
        }
    }
    None
}

fn text_field(bytes: &[u8], number: u32) -> Option<String> {
    let payload = message(bytes, number)?;
    let text = std::str::from_utf8(payload).ok()?;
    (text.len() <= 64 && text.chars().all(|c| !c.is_control()) && text.trim() == text)
        .then(|| text.to_owned())
}

fn window(bytes: &[u8], percent_field: u32, reset_field: u32, now: u64) -> Option<PlanWindow> {
    // A reset timestamp proves the window exists. Proto3 elides zero-valued
    // scalars, so a present reset with no remaining-percent field is an
    // exhausted window (0% remaining), not an unreported one.
    let resets_at_ms = varint_field(bytes, reset_field)?.checked_mul(1000)?;
    if resets_at_ms <= now {
        return None;
    }
    let remaining = varint_field(bytes, percent_field).unwrap_or(0);
    if remaining > 100 {
        return None;
    }
    Some(PlanWindow {
        remaining_percent: remaining as f64,
        resets_at_ms,
    })
}

/// Parse `GetUserStatusResponse`: `user_status` is field 1 and `plan_status`
/// is field 13 inside it. Within `PlanStatus`, fields 14/15 carry the daily
/// and weekly remaining percentages, 17/18 the reset times (unix seconds),
/// and `plan_status.1` the plan record whose field 2 is the display name.
/// `user_status` field 7 is the account email.
fn parse_user_status(bytes: &[u8], now: u64) -> Result<UserStatus> {
    let user_status = message(bytes, 1).ok_or(Error::Protocol("Devin user status"))?;
    let plan_status = message(user_status, 13).ok_or(Error::Protocol("Devin plan status"))?;
    Ok(UserStatus {
        daily: window(plan_status, 14, 17, now),
        weekly: window(plan_status, 15, 18, now),
        plan_name: message(plan_status, 1).and_then(|plan| text_field(plan, 2)),
        email: text_field(user_status, 7).filter(|email| email.contains('@')),
    })
}

async fn post_status(token: &str, version: &str) -> Result<(u16, Vec<u8>)> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let addresses = tokio::net::lookup_host((API_SERVER, 443)).await?;
    let mut stream = None;
    for address in addresses.take(8) {
        if let Ok(tcp) = TcpStream::connect(address).await {
            stream = Some(tcp);
            break;
        }
    }
    let mut tls = connector
        .connect(
            ServerName::try_from(API_SERVER).expect("fixed identity host"),
            stream.ok_or(Error::Unavailable("Devin status connection failed"))?,
        )
        .await?;
    let body = request_body(token, version);
    let request = format!(
        "POST {PATH} HTTP/1.1\r\nhost: {API_SERVER}\r\ncontent-type: application/proto\r\nconnect-protocol-version: 1\r\naccept: application/proto\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len(),
    );
    tls.write_all(request.as_bytes()).await?;
    tls.write_all(&body).await?;
    tls.flush().await?;
    crate::jev::read_response(&mut tls).await
}

/// Fetch the account's plan status. A non-200 response or an unreadable
/// payload surfaces `Unavailable`/`Protocol` rather than a fabricated meter.
pub(crate) async fn user_status(token: &str, version: &str, now: u64) -> Result<UserStatus> {
    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        post_status(token, version),
    )
    .await
    .map_err(|_| Error::Unavailable("Devin status request timed out"))??;
    match status {
        200 => parse_user_status(&body, now),
        401 | 403 => Err(Error::Unavailable(crate::category::AUTHENTICATION)),
        _ => Err(Error::Unavailable("Devin status request failed")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_varint(number: u32, value: u64, out: &mut Vec<u8>) {
        encode_varint(u64::from(number << 3), out);
        encode_varint(value, out);
    }

    fn field_message(number: u32, payload: &[u8], out: &mut Vec<u8>) {
        encode_varint(u64::from(number << 3 | 2), out);
        encode_varint(payload.len() as u64, out);
        out.extend_from_slice(payload);
    }

    fn fixture(now: u64) -> Vec<u8> {
        let mut plan = Vec::new();
        encode_string(2, "Max", &mut plan);
        let mut plan_status = Vec::new();
        field_message(1, &plan, &mut plan_status);
        field_varint(14, 40, &mut plan_status);
        field_varint(15, 72, &mut plan_status);
        field_varint(17, now / 1000 + 3600, &mut plan_status);
        field_varint(18, now / 1000 + 604_800, &mut plan_status);
        let mut user_status = Vec::new();
        encode_string(7, "acct@example.com", &mut user_status);
        field_message(13, &plan_status, &mut user_status);
        let mut response = Vec::new();
        field_message(1, &user_status, &mut response);
        response
    }

    #[test]
    fn parses_plan_status_windows_and_name() {
        let now = 1_800_000_000_000;
        let status = parse_user_status(&fixture(now), now).unwrap();
        assert_eq!(status.daily.unwrap().remaining_percent, 40.0);
        assert_eq!(status.daily.unwrap().resets_at_ms, now + 3_600_000);
        assert_eq!(status.weekly.unwrap().remaining_percent, 72.0);
        assert_eq!(status.weekly.unwrap().resets_at_ms, now + 604_800_000);
        assert_eq!(status.plan_name.as_deref(), Some("Max"));
        assert_eq!(status.email.as_deref(), Some("acct@example.com"));
    }

    #[test]
    fn absent_percent_with_a_reset_is_an_exhausted_window() {
        let now = 1_800_000_000_000;
        // Proto3 elides zero-valued scalars: reset present, percent absent.
        let mut plan_status = Vec::new();
        field_varint(17, now / 1000 + 3600, &mut plan_status);
        field_varint(18, now / 1000 + 604_800, &mut plan_status);
        let mut user_status = Vec::new();
        field_message(13, &plan_status, &mut user_status);
        let mut response = Vec::new();
        field_message(1, &user_status, &mut response);
        let status = parse_user_status(&response, now).unwrap();
        assert_eq!(status.daily.unwrap().remaining_percent, 0.0);
        assert_eq!(status.weekly.unwrap().remaining_percent, 0.0);
    }

    #[test]
    fn absent_windows_and_past_resets_are_not_meters() {
        let now = 1_800_000_000_000;
        // No resets at all: the windows are genuinely unreported.
        let mut user_status = Vec::new();
        field_message(13, &[], &mut user_status);
        let mut response = Vec::new();
        field_message(1, &user_status, &mut response);
        let status = parse_user_status(&response, now).unwrap();
        assert!(status.daily.is_none());
        assert!(status.weekly.is_none());
        assert!(status.plan_name.is_none());
        // An already-elapsed reset is stale, not a meter.
        let mut plan_status = Vec::new();
        field_varint(14, 0, &mut plan_status);
        field_varint(17, now / 1000 - 1, &mut plan_status);
        let mut user_status = Vec::new();
        field_message(13, &plan_status, &mut user_status);
        let mut response = Vec::new();
        field_message(1, &user_status, &mut response);
        assert!(parse_user_status(&response, now).unwrap().daily.is_none());
    }

    #[test]
    fn malformed_and_missing_status_are_errors() {
        assert!(parse_user_status(&[], 0).is_err());
        assert!(parse_user_status(&[0x0a, 0x02, 0xff, 0x00], 0).is_err());
        let mut response = Vec::new();
        field_message(1, b"\x08\x01", &mut response); // user_status without plan_status
        assert!(parse_user_status(&response, 0).is_err());
    }

    #[test]
    fn request_body_carries_identity_and_token() {
        let body = request_body("tok-value", "9.9.9");
        // field 1 metadata { 1:"xcb", 2:"9.9.9", 3:"tok-value", 7:"9.9.9" }
        let expected: &[u8] = &[
            0x0a, 0x1e, 0x0a, 0x03, b'x', b'c', b'b', 0x12, 0x05, b'9', b'.', b'9', b'.', b'9',
            0x1a, 0x09, b't', b'o', b'k', b'-', b'v', b'a', b'l', b'u', b'e', 0x3a, 0x05, b'9',
            b'.', b'9', b'.', b'9',
        ];
        assert_eq!(body, expected);
    }
}
