//! One bounded terminal line, shared by interactive sign-in and ordinary prompts.
//! Unix reads bypass stdio buffering so the next prompt retains queued input.
use std::io;
use zeroize::{Zeroize, Zeroizing};

pub struct Reader {
    hidden: bool,
    max_bytes: usize,
    #[cfg(unix)]
    stop: crate::stop::Stop,
    #[cfg(unix)]
    mode: Option<unix::Mode>,
}

impl Reader {
    pub fn new(hidden: bool, max_bytes: usize) -> io::Result<Self> {
        if !(1..=8192).contains(&max_bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Terminal input limit must be between 1 and 8192 bytes",
            ));
        }
        // Install handlers before enabling terminal-generated SIGINT. A caller
        // can print its prompt after new(): pasted secrets are already hidden.
        #[cfg(unix)]
        let stop = crate::stop::Stop::install()?;
        #[cfg(unix)]
        let mode = Some(unix::Mode::enter(hidden)?);
        Ok(Self {
            hidden,
            max_bytes,
            #[cfg(unix)]
            stop,
            #[cfg(unix)]
            mode,
        })
    }

    /// Restore the exact original tty settings before returning a line, error,
    /// or EOF, and when this future is dropped. The guard is moved into the
    /// future synchronously, so dropping an unpolled read also restores it.
    pub fn read_line(
        &mut self,
    ) -> impl Future<Output = io::Result<Option<Zeroizing<String>>>> + '_ {
        #[cfg(unix)]
        let mode = self
            .mode
            .take()
            .map(Ok)
            .unwrap_or_else(|| unix::Mode::enter(self.hidden));
        async move {
            #[cfg(unix)]
            {
                let mut mode = mode?;
                let result = unix::read_line(self.max_bytes, &mut self.stop).await;
                mode.restore()?;
                result
            }
            #[cfg(not(unix))]
            {
                // Keep ordinary stdin behavior on other platforms. Do not
                // leave an uncancellable background input thread after login.
                use std::io::Read;
                let _ = self.hidden;
                let mut stdin = io::stdin().lock();
                let mut line = Line::new(self.max_bytes);
                loop {
                    let mut byte = Zeroizing::new([0_u8; 1]);
                    match stdin.read(&mut *byte)? {
                        0 => return line.finish(true),
                        _ if matches!(byte[0], b'\n' | b'\r') => return line.finish(false),
                        _ => line.push(byte[0]),
                    }
                }
            }
        }
    }
}

struct Line {
    bytes: Zeroizing<Vec<u8>>,
    max_bytes: usize,
    invalid: bool,
}

impl Line {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Zeroizing::new(Vec::with_capacity(max_bytes.min(128))),
            max_bytes,
            invalid: false,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.invalid {
            return;
        }
        if byte.is_ascii_control() || self.bytes.len() == self.max_bytes {
            // Finish consuming this line without retaining rejected input.
            // Do not flush the tty, which would also discard queued prompts.
            self.invalid = true;
            self.bytes.zeroize();
        } else {
            self.bytes.push(byte);
        }
    }

    fn finish(self, eof: bool) -> io::Result<Option<Zeroizing<String>>> {
        let text = std::str::from_utf8(&self.bytes).ok();
        if self.invalid || text.is_none_or(|text| text.chars().any(char::is_control)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Terminal input is invalid or exceeds its length limit",
            ));
        }
        let text = text.expect("UTF-8 was checked above");
        if eof && text.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Zeroizing::new(text.to_owned())))
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::{Errno, read},
        termios::{
            InputModes, LocalModes, OptionalActions, SpecialCodeIndex, Termios, tcgetattr,
            tcsetattr,
        },
    };
    use std::time::Duration;

    pub(super) struct Mode(Option<Termios>);

    impl Mode {
        pub(super) fn enter(hidden: bool) -> io::Result<Self> {
            let stdin = io::stdin();
            let original = match tcgetattr(&stdin) {
                Ok(original) => original,
                // Pipes and redirected files do not have terminal settings.
                Err(Errno::NOTTY) => return Ok(Self(None)),
                Err(error) => return Err(error.into()),
            };
            let mut normalized = original.clone();
            normalized
                .local_modes
                .insert(LocalModes::ICANON | LocalModes::ISIG);
            normalized.local_modes.remove(LocalModes::IEXTEN);
            #[cfg(not(any(
                target_os = "aix",
                target_os = "cygwin",
                target_os = "haiku",
                target_os = "nto",
                target_os = "redox",
            )))]
            normalized.local_modes.remove(LocalModes::EXTPROC);
            #[cfg(not(any(target_os = "cygwin", target_os = "nto", target_os = "redox")))]
            normalized.local_modes.remove(LocalModes::PENDIN);
            normalized.input_modes.remove(
                InputModes::IGNCR
                    | InputModes::INLCR
                    | InputModes::ISTRIP
                    | InputModes::IXON
                    | InputModes::IXOFF,
            );
            normalized.input_modes.insert(InputModes::ICRNL);
            normalized.special_codes[SpecialCodeIndex::VINTR] = 3;
            normalized.special_codes[SpecialCodeIndex::VEOF] = 4;
            normalized.special_codes[SpecialCodeIndex::VERASE] = 127;
            normalized.special_codes[SpecialCodeIndex::VKILL] = 21;
            if hidden {
                normalized
                    .local_modes
                    .remove(LocalModes::ECHO | LocalModes::ECHONL);
                #[cfg(not(any(target_os = "cygwin", target_os = "nto", target_os = "redox")))]
                normalized.local_modes.remove(LocalModes::ECHOPRT);
            } else {
                normalized
                    .local_modes
                    .insert(LocalModes::ECHO | LocalModes::ECHOE | LocalModes::ECHOK);
                normalized.local_modes.remove(LocalModes::ECHONL);
            }
            let guard = Self(Some(original));
            tcsetattr(&stdin, OptionalActions::Now, &normalized)?;
            Ok(guard)
        }

        pub(super) fn restore(&mut self) -> io::Result<()> {
            if let Some(original) = &self.0 {
                tcsetattr(io::stdin(), OptionalActions::Now, original)?;
                self.0 = None;
            }
            Ok(())
        }
    }

    impl Drop for Mode {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }

    async fn readable() -> io::Result<()> {
        let stdin = io::stdin();
        loop {
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            match poll(&mut fds, Some(&Timespec::default())) {
                Ok(0) | Err(Errno::INTR) => {}
                Ok(_) => {
                    let events = fds[0].revents();
                    if events.intersects(PollFlags::IN | PollFlags::HUP) {
                        return Ok(());
                    }
                    return Err(io::Error::other("Terminal input is unavailable"));
                }
                Err(error) => return Err(error.into()),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub(super) async fn read_line(
        max_bytes: usize,
        stop: &mut crate::stop::Stop,
    ) -> io::Result<Option<Zeroizing<String>>> {
        let stdin = io::stdin();
        let mut line = Line::new(max_bytes);
        loop {
            tokio::select! {
                biased;
                _ = stop.recv() => return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Terminal input cancelled",
                )),
                result = readable() => result?,
            }
            // A direct one-byte read never steals bytes from a subsequent
            // prompt. It is only called after readiness, with no other reader.
            let mut byte = Zeroizing::new([0_u8; 1]);
            match read(&stdin, &mut *byte) {
                Ok(0) => return line.finish(true),
                Ok(_) if matches!(byte[0], b'\n' | b'\r') => return line.finish(false),
                Ok(_) => line.push(byte[0]),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(error) => return Err(error.into()),
            }
            // Let Tokio deliver stop signals even when input is continuously
            // ready (for example a long redirected, rejected line).
            tokio::task::yield_now().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_bounds_and_validation_do_not_include_rejected_input() {
        for bytes in [b"too-long-line".as_slice(), b"a\x1b[A", b"\xff"] {
            let mut line = Line::new(8);
            for byte in bytes {
                line.push(*byte);
            }
            let error = line.finish(false).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                error.to_string(),
                "Terminal input is invalid or exceeds its length limit"
            );
        }
        let mut line = Line::new(8);
        for byte in b"12345678" {
            line.push(*byte);
        }
        assert_eq!(line.finish(false).unwrap().unwrap().as_str(), "12345678");
        assert!(Line::new(8).finish(true).unwrap().is_none());
        assert!(Line::new(8).finish(false).unwrap().unwrap().is_empty());
        let mut line = Line::new(32);
        for byte in "a résumé".as_bytes() {
            line.push(*byte);
        }
        assert_eq!(line.finish(false).unwrap().unwrap().as_str(), "a résumé");
        let mut line = Line::new(32);
        for byte in "a\u{85}b".as_bytes() {
            line.push(*byte);
        }
        assert_eq!(
            line.finish(false).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn limits_are_validated_before_touching_stdin() {
        for limit in [0, 8193, usize::MAX] {
            assert!(matches!(
                Reader::new(true, limit),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_reader_child() {
        use std::io::Write;
        let Some(case) = std::env::var_os("XCB_TERMINAL_READER_CHILD") else {
            return;
        };
        let case = case.to_str().unwrap();
        let mut reader =
            Reader::new(case != "visible", if case == "overflow" { 8 } else { 64 }).unwrap();
        println!("READER_READY");
        io::stdout().flush().unwrap();
        match case {
            "cancel" => {
                let input = reader.read_line();
                tokio::pin!(input);
                tokio::select! {
                    _ = &mut input => panic!("Input completed before cancellation"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {},
                }
            }
            "unpolled" => drop(reader.read_line()),
            "ctrlc" => assert_eq!(
                reader.read_line().await.unwrap_err().kind(),
                io::ErrorKind::Interrupted
            ),
            "eof" => assert!(reader.read_line().await.unwrap().is_none()),
            "empty" => assert!(reader.read_line().await.unwrap().unwrap().is_empty()),
            "invalid" | "invalid_utf8" | "overflow" => assert_eq!(
                reader.read_line().await.unwrap_err().kind(),
                io::ErrorKind::InvalidData
            ),
            "queued_error" => {
                assert_eq!(
                    reader.read_line().await.unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
                assert_eq!(
                    Reader::new(true, 64)
                        .unwrap()
                        .read_line()
                        .await
                        .unwrap()
                        .unwrap()
                        .as_str(),
                    "queued-input"
                );
            }
            "queued" => {
                assert_eq!(
                    reader.read_line().await.unwrap().unwrap().as_str(),
                    "fixture code#state"
                );
                assert_eq!(
                    Reader::new(true, 64)
                        .unwrap()
                        .read_line()
                        .await
                        .unwrap()
                        .unwrap()
                        .as_str(),
                    "queued-input"
                );
            }
            "unicode" => assert_eq!(
                reader.read_line().await.unwrap().unwrap().as_str(),
                "fixture résumé"
            ),
            _ => assert_eq!(
                reader.read_line().await.unwrap().unwrap().as_str(),
                "fixture code#state"
            ),
        }
        // Keep the Reader alive until after its future finished: restoration
        // must not depend on dropping the caller's Reader.
        println!("READER_RESULT:ok");
        io::stdout().flush().unwrap();
        // The parent checks restoration before releasing this fixture. An
        // arbitrary sleep could expire before the parent observes the tty.
        let acknowledgment = std::env::var_os("XCB_TERMINAL_READER_ACK").unwrap();
        let acknowledgment = std::path::Path::new(&acknowledgment);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !acknowledgment.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "Parent did not acknowledge"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read(acknowledgment).unwrap(), b".");
        drop(reader);
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_ttys_restore_exact_modes_and_preserve_queued_lines() {
        use std::{path::Path, process::Command};
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("Python 3 is required for terminal regression tests");
        let script = r#"
import fcntl, os, pty, select, subprocess, sys, tempfile, termios, time, tty
case, initial = sys.argv[2:]
master, slave = pty.openpty()
if initial == 'raw':
    tty.setraw(slave)
attrs = termios.tcgetattr(slave)
attrs[3] |= termios.ECHONL
if initial == 'raw':
    attrs[0] &= ~termios.ICRNL
    attrs[0] |= termios.IGNCR | termios.INLCR
    attrs[3] &= ~(termios.ISIG | termios.ICANON)
termios.tcsetattr(slave, termios.TCSANOW, attrs)
original = termios.tcgetattr(slave)
def controlling_tty():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    os.tcsetpgrp(0, os.getpgrp())
ack_dir = tempfile.TemporaryDirectory(prefix='xcb-reader-ack-')
ack_path = os.path.join(ack_dir.name, 'ack')
child_env = dict(os.environ, XCB_TERMINAL_READER_ACK=ack_path)
child = subprocess.Popen([sys.argv[1], '--exact', 'terminal_input::tests::terminal_reader_child', '--nocapture'], stdin=slave, stdout=slave, stderr=slave, preexec_fn=controlling_tty, env=child_env)
data = bytearray()
def receive(seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], .01)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            data.extend(chunk)
try:
    deadline = time.monotonic() + 8
    while b'READER_READY' not in data and time.monotonic() < deadline:
        receive(.01)
    assert b'READER_READY' in data, data
    if case != 'unpolled':
        normalized = termios.tcgetattr(slave)
        assert normalized[3] & termios.ICANON, normalized
        assert normalized[3] & termios.ISIG, normalized
        assert normalized[0] & termios.ICRNL, normalized
        assert not normalized[0] & (termios.IGNCR | termios.INLCR), normalized
        if case == 'visible':
            assert normalized[3] & termios.ECHO, normalized
        else:
            assert not normalized[3] & (termios.ECHO | termios.ECHONL), normalized
    inputs = {
        'code': b'fixture code#state\r',
        'visible': b'fixture code#state\r',
        'backspace': b'fixture codX\x7fe#state\r',
        'empty': b'\r',
        'eof': b'\x04',
        'ctrlc': b'\x03',
        'invalid': b'fixture\x1b[A\r',
        'invalid_utf8': b'\xff\r',
        'overflow': b'too-long-line\r',
        'queued': b'fixture code#state\rqueued-input\r',
        'queued_error': b'fixture\x1b[A\rqueued-input\r',
        'unicode': 'fixture résumé\r'.encode(),
    }
    if case in inputs:
        os.write(master, inputs[case])
    while b'READER_RESULT:ok' not in data and time.monotonic() < deadline:
        receive(.01)
    assert b'READER_RESULT:ok' in data, data
    assert termios.tcgetattr(slave) == original, 'Original terminal settings not restored'
    if case == 'visible':
        assert b'fixture code#state' in data, data
    else:
        for secret in [b'fixture code#state', b'queued-input', b'too-long-line']:
            assert secret not in data, data
    with open(ack_path + '.tmp', 'xb') as acknowledgment:
        acknowledgment.write(b'.')
    os.replace(ack_path + '.tmp', ack_path)
    # Darwin's controlling-tty session leader waits for pending output to
    # drain while exiting. Keep reading the harness's trailing result text.
    exit_deadline = time.monotonic() + 3
    while child.poll() is None and time.monotonic() < exit_deadline:
        receive(.02)
    child.wait(timeout=2)
    assert child.returncode == 0, data
finally:
    os.close(master)
    os.close(slave)
    if child.poll() is None:
        child.kill()
        child.wait(timeout=2)
    ack_dir.cleanup()
"#;
        for initial in ["canonical", "raw"] {
            for case in [
                "code",
                "visible",
                "backspace",
                "empty",
                "eof",
                "ctrlc",
                "cancel",
                "unpolled",
                "invalid",
                "invalid_utf8",
                "overflow",
                "queued",
                "queued_error",
                "unicode",
            ] {
                let output = Command::new(python)
                    .args(["-c", script])
                    .arg(std::env::current_exe().unwrap())
                    .arg(case)
                    .arg(initial)
                    .env("XCB_TERMINAL_READER_CHILD", case)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{initial}/{case}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}
