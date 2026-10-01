//! Browser setup validates the selected account before starting the bridge.
//! This does not acquire scopes or admit browser effects. The actual extension
//! must still connect successfully before the CLI publishes its configuration.
use crate::{Error, Result};
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use serde::Deserialize;
use std::{future::Future, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio_rustls::TlsConnector;
use zeroize::Zeroizing;

const HOST: &str = "api.anthropic.com";
const PATH: &str = "/api/oauth/validate";
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    MissingScope,
    Rejected,
    Forbidden,
    RateLimited,
    ServiceUnavailable,
    InvalidResponse,
    Transport,
    TimedOut,
}

impl Failure {
    fn error(self) -> Error {
        Error::Unavailable(match self {
            Self::MissingScope => {
                "the saved Claude sign-in lacks browser permission; run `xcb accounts login <account> --browser`, then connect again. Normal Claude tasks can still use this sign-in"
            }
            Self::Rejected => "Claude rejected the saved sign-in while checking browser access",
            Self::Forbidden => "Claude refused browser access for the saved sign-in",
            Self::RateLimited => {
                "Claude temporarily limited browser sign-in checks; try again later"
            }
            Self::ServiceUnavailable => {
                "Claude's browser sign-in service is temporarily unavailable; try again later"
            }
            Self::InvalidResponse => {
                "Claude's browser sign-in service returned an invalid response"
            }
            Self::Transport => {
                "could not reach Claude's browser sign-in service; check the connection and try again"
            }
            Self::TimedOut => "Claude's browser sign-in check timed out; try again later",
        })
    }
}

pub(super) async fn check(token: Zeroizing<String>) -> Result<()> {
    within_deadline(DEADLINE, exchange(&token)).await
}

async fn within_deadline(
    deadline: Duration,
    check: impl Future<Output = std::result::Result<(), Failure>>,
) -> Result<()> {
    tokio::time::timeout(deadline, check)
        .await
        .unwrap_or(Err(Failure::TimedOut))
        .map_err(Failure::error)
}

fn request(token: &str) -> std::result::Result<Zeroizing<String>, Failure> {
    if !crate::auth::valid_token(token) {
        return Err(Failure::Rejected);
    }
    // Do not inherit endpoint overrides, proxies, headers, or ambient tokens.
    // Keep both the token and its complete wire representation zeroizing.
    Ok(Zeroizing::new(format!(
        "POST {PATH} HTTP/1.1\r\nhost: {HOST}\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\naccept: application/json\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnull"
    )))
}

async fn exchange(token: &str) -> std::result::Result<(), Failure> {
    let request = request(token)?;
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let addresses = tokio::net::lookup_host((HOST, 443))
        .await
        .map_err(|_| Failure::Transport)?;
    let mut stream = None;
    for address in addresses.take(8) {
        if let Ok(tcp) = TcpStream::connect(address).await {
            stream = Some(tcp);
            break;
        }
    }
    let mut tls = connector
        .connect(
            ServerName::try_from(HOST).expect("fixed browser authentication host"),
            stream.ok_or(Failure::Transport)?,
        )
        .await
        .map_err(|_| Failure::Transport)?;
    tls.write_all(request.as_bytes())
        .await
        .map_err(|_| Failure::Transport)?;
    tls.flush().await.map_err(|_| Failure::Transport)?;
    read_and_classify(&mut tls).await
}

async fn read_and_classify(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::result::Result<(), Failure> {
    // The shared reader bounds headers, chunks and the body (256 KiB). Its
    // errors never cross this boundary, including any future dynamic text.
    let (status, body) = crate::jev::read_response(stream).await.map_err(|error| {
        if matches!(error, Error::Io(_)) {
            Failure::Transport
        } else {
            Failure::InvalidResponse
        }
    })?;
    classify(status, &Zeroizing::new(body))
}

#[derive(Deserialize)]
struct Validation<'a> {
    #[serde(borrow)]
    account_uuid: Option<&'a str>,
    #[serde(borrow)]
    error: Option<ApiError<'a>>,
}

#[derive(Deserialize)]
struct ApiError<'a> {
    #[serde(rename = "type", borrow)]
    kind: &'a str,
    #[serde(borrow)]
    message: &'a str,
}

fn classify(status: u16, body: &[u8]) -> std::result::Result<(), Failure> {
    let response = serde_json::from_slice::<Validation<'_>>(body).ok();
    match status {
        200 => {
            if response
                .and_then(|response| response.account_uuid)
                .is_some_and(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok())
            {
                Ok(())
            } else {
                Err(Failure::InvalidResponse)
            }
        }
        401 => Err(Failure::Rejected),
        403 => {
            if response
                .and_then(|response| response.error)
                .is_some_and(|error| {
                    error.kind == "permission_error" && missing_scope(error.message)
                })
            {
                Err(Failure::MissingScope)
            } else {
                Err(Failure::Forbidden)
            }
        }
        429 => Err(Failure::RateLimited),
        500..=599 => Err(Failure::ServiceUnavailable),
        _ => Err(Failure::InvalidResponse),
    }
}

fn missing_scope(message: &str) -> bool {
    matches!(
        message,
        "OAuth token does not meet scope requirement any_of(user:profile, user:office, user:ccr_inference, user:chrome_bridge)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCOPE_ERROR: &str = "OAuth token does not meet scope requirement any_of(user:profile, user:office, user:ccr_inference, user:chrome_bridge)";
    const UUID: &str = "92581133-a918-4d59-964b-6b323aa6a9ae";
    const TOKEN: &str = "sk-ant-oat01-synthetic_private_token_123";

    fn scope_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type":"error",
            "error":{"type":"permission_error","message":SCOPE_ERROR}
        }))
        .unwrap()
    }

    #[test]
    fn scope_failure_requires_exact_status_kind_and_observed_message() {
        assert_eq!(classify(403, &scope_body()), Err(Failure::MissingScope));
        assert_eq!(classify(401, &scope_body()), Err(Failure::Rejected));
        for (kind, message) in [
            ("authentication_error", SCOPE_ERROR.to_owned()),
            ("permission_error", "not allowed".into()),
            (
                "permission_error",
                format!("untrusted prefix {SCOPE_ERROR}"),
            ),
            ("permission_error", format!("{SCOPE_ERROR} {TOKEN}")),
        ] {
            let body =
                serde_json::to_vec(&json!({"error":{"type":kind,"message":message}})).unwrap();
            assert_eq!(classify(403, &body), Err(Failure::Forbidden));
        }
        assert_eq!(classify(403, b"not JSON"), Err(Failure::Forbidden));
    }

    #[test]
    fn successful_validation_requires_200_and_a_well_formed_account_uuid() {
        let success = serde_json::to_vec(&json!({"account_uuid":UUID})).unwrap();
        assert_eq!(classify(200, &success), Ok(()));
        for status in [201, 204, 301, 307] {
            assert_eq!(classify(status, &success), Err(Failure::InvalidResponse));
        }
        for body in [
            json!({}),
            json!({"account_uuid":null}),
            json!({"account_uuid":12}),
            json!({"account_uuid":""}),
            json!({"account_uuid":"private@example.invalid"}),
            json!({"account_uuid":format!(" {UUID}")}),
            json!({"account_uuid":UUID.replace('-', "")}),
        ] {
            assert_eq!(
                classify(200, &serde_json::to_vec(&body).unwrap()),
                Err(Failure::InvalidResponse)
            );
        }
    }

    #[test]
    fn errors_carry_only_static_diagnostics_and_never_change_inference_authentication() {
        let private = format!("{TOKEN} private@example.invalid https://private.invalid/{UUID}");
        let body = serde_json::to_vec(
            &json!({"account_uuid":private,"error":{"type":"permission_error","message":private}}),
        )
        .unwrap();
        for (status, expected) in [
            (200, Failure::InvalidResponse),
            (401, Failure::Rejected),
            (403, Failure::Forbidden),
            (429, Failure::RateLimited),
            (503, Failure::ServiceUnavailable),
            (307, Failure::InvalidResponse),
        ] {
            let failure = classify(status, &body).unwrap_err();
            assert_eq!(failure, expected);
            let error = failure.error();
            assert!(matches!(error, Error::Unavailable(_)));
            for text in [error.to_string(), format!("{error:?}")] {
                for secret in [
                    TOKEN,
                    UUID,
                    "private@example.invalid",
                    "https://private.invalid",
                ] {
                    assert!(!text.contains(secret));
                }
            }
            assert_ne!(error.to_string(), crate::category::AUTHENTICATION);
        }
        let error = Failure::MissingScope.error();
        assert!(
            error
                .to_string()
                .contains("xcb accounts login <account> --browser")
        );
        assert_ne!(error.to_string(), crate::category::AUTHENTICATION);
    }

    #[test]
    fn request_uses_only_canonical_destination_and_rejects_header_injection() {
        let request = request(TOKEN).unwrap();
        assert!(
            request.starts_with("POST /api/oauth/validate HTTP/1.1\r\nhost: api.anthropic.com\r\n")
        );
        assert!(request.contains(&format!("authorization: Bearer {TOKEN}\r\n")));
        assert!(request.ends_with("content-length: 4\r\nconnection: close\r\n\r\nnull"));
        assert_eq!(
            super::request(&format!("{TOKEN}\r\nx-private: value")).unwrap_err(),
            Failure::Rejected
        );
    }

    #[tokio::test]
    async fn deadline_and_transport_failures_have_closed_diagnostics() {
        let error = within_deadline(Duration::ZERO, std::future::pending())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), Failure::TimedOut.error().to_string());
        let error = within_deadline(Duration::from_secs(1), async { Err(Failure::Transport) })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), Failure::Transport.error().to_string());
    }

    #[tokio::test]
    async fn response_framing_is_bounded_and_does_not_echo_headers_or_bodies() {
        let success = format!("{{\"account_uuid\":\"{UUID}\"}}");
        for response in [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nx-private: {TOKEN}\r\n\r\n{success}",
                success.len()
            ),
            format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{success}\r\n0\r\n\r\n",
                success.len()
            ),
        ] {
            assert_eq!(read_and_classify(&mut response.as_bytes()).await, Ok(()));
        }
        for response in [
            format!("HTTP/1.1 200 OK\r\nContent-Length: 262145\r\nx-private: {TOKEN}\r\n\r\n"),
            format!("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{TOKEN}"),
            format!(
                "HTTP/1.1 200 OK\r\nx-private: {TOKEN}\r\n\r\n{}",
                "x".repeat(262145)
            ),
            format!("private invalid status {TOKEN}\r\n\r\n"),
        ] {
            let failure = read_and_classify(&mut response.as_bytes())
                .await
                .unwrap_err();
            assert_eq!(failure, Failure::InvalidResponse);
            assert!(!format!("{:?}", failure.error()).contains(TOKEN));
        }
    }

    #[tokio::test]
    async fn maximal_chunk_after_an_existing_byte_cannot_overflow_the_bound() {
        let response = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\n{:x}\r\n",
            usize::MAX
        );
        assert_eq!(
            read_and_classify(&mut response.as_bytes()).await,
            Err(Failure::InvalidResponse)
        );
    }
}
