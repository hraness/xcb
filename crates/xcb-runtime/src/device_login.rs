//! Transient output from the official device login, never a credential log.

#[cfg(any(unix, test))]
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const CODEX_DEVICE_URL: &str = "https://auth.openai.com/codex/device";
#[cfg(any(unix, test))]
const DETECT_LIMIT: usize = 16 * 1024;
#[cfg(any(unix, test))]
const OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLoginPrompt {
    pub url: String,
    pub code: String,
}

#[cfg(any(unix, test))]
#[derive(Default, Clone, Copy)]
enum Escape {
    #[default]
    Text,
    Start,
    Csi,
    Osc,
    OscEnd,
}

#[cfg(any(unix, test))]
#[derive(Default)]
struct Detector {
    text: Vec<u8>,
    seen: usize,
    escape: [Escape; 2],
    pending: [Vec<u8>; 2],
    finished: bool,
}

#[cfg(any(unix, test))]
impl Detector {
    #[cfg(test)]
    fn observe(&mut self, bytes: &[u8]) -> Option<DeviceLoginPrompt> {
        self.observe_stream(bytes, 0)
    }

    fn observe_stream(&mut self, bytes: &[u8], stream: usize) -> Option<DeviceLoginPrompt> {
        if self.finished {
            return None;
        }
        self.seen = self.seen.saturating_add(bytes.len());
        if self.seen > DETECT_LIMIT {
            self.finished = true;
            self.text.clear();
            self.pending.iter_mut().for_each(Vec::clear);
            return None;
        }
        for &byte in bytes {
            self.escape[stream] = match self.escape[stream] {
                Escape::Text if byte == 0x1b => Escape::Start,
                Escape::Text => {
                    if byte == b'\n' || byte == b'\r' || byte == b'\t' || byte >= 0x20 {
                        self.pending[stream].push(byte);
                        if byte == b'\n' {
                            self.text.append(&mut self.pending[stream]);
                        }
                    }
                    Escape::Text
                }
                Escape::Start if byte == b'[' => Escape::Csi,
                Escape::Start if byte == b']' => Escape::Osc,
                Escape::Start => Escape::Text,
                Escape::Csi if (0x40..=0x7e).contains(&byte) => Escape::Text,
                Escape::Csi => Escape::Csi,
                Escape::Osc if byte == 7 => Escape::Text,
                Escape::Osc if byte == 0x1b => Escape::OscEnd,
                Escape::Osc => Escape::Osc,
                Escape::OscEnd if byte == b'\\' => Escape::Text,
                Escape::OscEnd => Escape::Osc,
            };
        }
        let text = std::str::from_utf8(&self.text).ok()?;
        // Only complete lines qualify: a chunk boundary cannot turn an
        // unfinished URL or code into a recognized prompt.
        let complete = text.rsplit_once('\n').map(|(complete, _)| complete)?;
        let url_seen = complete.lines().any(|line| line.trim() == CODEX_DEVICE_URL);
        if !url_seen {
            return None;
        }
        let heading = complete.lines().position(|line| {
            line.trim().starts_with("2. Enter this one-time code ")
                || line.trim() == "2. Enter this one-time code"
        })?;
        let code = complete
            .lines()
            .skip(heading + 1)
            .map(str::trim)
            .find(|line| code_shape(line))?;
        self.finished = true;
        let prompt = DeviceLoginPrompt {
            url: CODEX_DEVICE_URL.into(),
            code: code.into(),
        };
        self.text.clear();
        self.pending.iter_mut().for_each(Vec::clear);
        Some(prompt)
    }
}

#[cfg(any(unix, test))]
fn code_shape(code: &str) -> bool {
    let Some((left, right)) = code.split_once('-') else {
        return false;
    };
    (left.len() == 4
        && right.len() == 5
        && left
            .bytes()
            .chain(right.bytes())
            .all(|byte| byte.is_ascii_digit()))
        || (left.len() == 4
            && right.len() == 4
            && left
                .bytes()
                .chain(right.bytes())
                .all(|byte| byte.is_ascii_uppercase()))
}

/// Multiplex both official CLI streams without spawning detached readers.
/// Detection and forwarding are bounded independently; unknown prompt formats
/// still reach the terminal verbatim. The caller owns cancellation and join.
#[cfg(any(unix, test))]
pub(crate) async fn forward<O, E, W>(
    mut stdout: O,
    mut stderr: E,
    mut terminal: W,
    sender: tokio::sync::mpsc::Sender<DeviceLoginPrompt>,
) -> std::io::Result<()>
where
    O: AsyncRead + Unpin,
    E: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut stdout_open, mut stderr_open) = (true, true);
    let (mut out_buffer, mut err_buffer) = ([0_u8; 4096], [0_u8; 4096]);
    // Separate ANSI state per pipe; complete prompt evidence is merged below.
    let mut detector = Detector::default();
    let mut total = 0_usize;
    while stdout_open || stderr_open {
        let (bytes, stream) = tokio::select! {
            result = stdout.read(&mut out_buffer), if stdout_open => {
                let count = result?;
                stdout_open = count != 0;
                (&out_buffer[..count], 0)
            }
            result = stderr.read(&mut err_buffer), if stderr_open => {
                let count = result?;
                stderr_open = count != 0;
                (&err_buffer[..count], 1)
            }
        };
        total = total.saturating_add(bytes.len());
        if total > OUTPUT_LIMIT {
            return Err(std::io::Error::other(
                "Codex sign-in output exceeded its limit",
            ));
        }
        terminal.write_all(bytes).await?;
        terminal.flush().await?;
        if let Some(prompt) = detector.observe_stream(bytes, stream) {
            // The manual output remains usable even if the UI went away or
            // did not reserve room for the single prompt notification.
            let _ = sender.try_send(prompt);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(code: &str) -> String {
        format!(
            "1. Open this link\n   {CODEX_DEVICE_URL}\n2. Enter this one-time code (expires in 15 minutes)\n   {code}\n"
        )
    }

    #[test]
    fn split_chunks_and_ansi_emit_once_after_complete_lines() {
        let mut detector = Detector::default();
        let input = output("AAAA-BBBB").replace(
            CODEX_DEVICE_URL,
            &format!("\x1b[94m{CODEX_DEVICE_URL}\x1b[0m"),
        );
        let mut prompts = Vec::new();
        for byte in input.bytes() {
            if let Some(prompt) = detector.observe(&[byte]) {
                prompts.push(prompt);
            }
        }
        assert_eq!(
            prompts,
            vec![DeviceLoginPrompt {
                url: CODEX_DEVICE_URL.into(),
                code: "AAAA-BBBB".into()
            }]
        );
        assert!(detector.observe(output("1234-56789").as_bytes()).is_none());
    }

    #[test]
    fn rejects_unknown_urls_codes_and_unterminated_candidates() {
        for code in [
            "AAAA-BBB",
            "AAAA-BBBBB",
            "aaaa-bbbb",
            "1234-5678",
            "12345-67890",
            "AAAA-BBBB-token",
            "access_token",
        ] {
            assert!(
                Detector::default()
                    .observe(output(code).as_bytes())
                    .is_none(),
                "{code}"
            );
        }
        for url in [
            "https://evil.example/codex/device",
            "https://auth.openai.com/codex/device/evil",
            "https://auth.openai.com/codex/device?next=evil",
        ] {
            assert!(
                Detector::default()
                    .observe(
                        output("AAAA-BBBB")
                            .replace(CODEX_DEVICE_URL, url)
                            .as_bytes()
                    )
                    .is_none()
            );
        }
        let mut detector = Detector::default();
        assert!(
            detector
                .observe(output("AAAA-BBBB").trim_end().as_bytes())
                .is_none()
        );
        assert!(detector.observe(b"X\n").is_none());
        assert!(
            Detector::default()
                .observe(output("1234-56789").as_bytes())
                .is_some()
        );
    }

    #[test]
    fn detection_is_bounded_and_requires_prompt_heading() {
        let mut detector = Detector::default();
        assert!(detector.observe(&vec![b'x'; DETECT_LIMIT + 1]).is_none());
        assert!(detector.text.is_empty());
        assert!(detector.observe(output("AAAA-BBBB").as_bytes()).is_none());
        assert!(
            Detector::default()
                .observe(format!("{CODEX_DEVICE_URL}\nAAAA-BBBB\n").as_bytes())
                .is_none()
        );
    }

    #[test]
    fn streams_keep_independent_ansi_and_partial_line_state() {
        let mut detector = Detector::default();
        assert!(detector.observe_stream(b"\x1b[", 0).is_none());
        assert!(
            detector
                .observe_stream(b"2. Enter this one-time code\nAAAA-BB", 1)
                .is_none()
        );
        assert!(
            detector
                .observe_stream(format!("94m{CODEX_DEVICE_URL}\x1b[0m\n").as_bytes(), 0)
                .is_none()
        );
        assert_eq!(
            detector.observe_stream(b"BB\n", 1).unwrap().code,
            "AAAA-BBBB"
        );
    }

    #[test]
    fn only_the_code_following_the_official_heading_qualifies() {
        let input = format!("ZZZZ-YYYY\n{}", output("AAAA-BBBB"));
        assert_eq!(
            Detector::default().observe(input.as_bytes()).unwrap().code,
            "AAAA-BBBB"
        );
        let input = output("AAAA-BBBB").replace(
            "2. Enter this one-time code",
            "Unrelated text: Enter this one-time code",
        );
        assert!(Detector::default().observe(input.as_bytes()).is_none());
        let input = format!("AAAA-BBBB\n{CODEX_DEVICE_URL}\n2. Enter this one-time code\n");
        assert!(Detector::default().observe(input.as_bytes()).is_none());
    }

    #[tokio::test]
    async fn unknown_format_stays_manual_and_closed_ui_does_not_break_forwarding() {
        for input in [
            "provider output with an unknown login format\n".to_owned(),
            output("AAAA-BBBB"),
        ] {
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            drop(rx);
            let mut terminal = Vec::new();
            forward(input.as_bytes(), &b""[..], &mut terminal, tx)
                .await
                .unwrap();
            assert_eq!(terminal, input.as_bytes());
        }
    }

    #[tokio::test]
    async fn stderr_and_mixed_output_forward_verbatim_and_join() {
        for stderr_only in [true, false] {
            let input = output("AAAA-BBBB");
            let split = input.find("2.").unwrap();
            let (out, err) = if stderr_only {
                ("", input.as_str())
            } else {
                (&input[..split], &input[split..])
            };
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let mut terminal = Vec::new();
            forward(out.as_bytes(), err.as_bytes(), &mut terminal, tx)
                .await
                .unwrap();
            assert_eq!(rx.recv().await.unwrap().code, "AAAA-BBBB");
            assert!(rx.recv().await.is_none());
            assert_eq!(terminal.len(), input.len());
            assert!(
                String::from_utf8(terminal)
                    .unwrap()
                    .contains(CODEX_DEVICE_URL)
            );
        }
    }

    #[tokio::test]
    async fn forwarding_refuses_unbounded_output_and_propagates_write_failure() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let input = vec![b'x'; OUTPUT_LIMIT + 1];
        let mut terminal = Vec::new();
        assert!(
            forward(input.as_slice(), &b""[..], &mut terminal, tx)
                .await
                .is_err()
        );
        assert!(terminal.len() <= OUTPUT_LIMIT);
        let (writer, reader) = tokio::io::duplex(1);
        drop(reader);
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        assert!(forward(&b"x"[..], &b""[..], writer, tx).await.is_err());
    }

    #[tokio::test]
    async fn cancelling_forward_drops_both_readers_and_prompt_sender() {
        let (mut stdout_writer, stdout_reader) = tokio::io::duplex(8);
        let (mut stderr_writer, stderr_reader) = tokio::io::duplex(8);
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let mut terminal = Vec::new();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                forward(stdout_reader, stderr_reader, &mut terminal, tx),
            )
            .await
            .is_err()
        );
        assert!(stdout_writer.write_all(b"x").await.is_err());
        assert!(stderr_writer.write_all(b"x").await.is_err());
        assert!(rx.recv().await.is_none());
    }
}
