//! Claude's browser handoff and optional code entry. Provider output and the
//! reusable token stay in the runtime; this terminal receives only login events.
use tokio::sync::mpsc;
use xcb_runtime::auth::ClaudeLoginEvent;
use zeroize::Zeroizing;

pub async fn login(
    store: &xcb_runtime::store::Store,
    account: &xcb_core::Id,
    pin: &xcb_runtime::process::Pin,
    browser: bool,
    machine: bool,
) -> xcb_runtime::Result<()> {
    use xcb_runtime::auth;
    check_login_context(account, browser, machine)?;
    let message = if browser {
        "Starting full Claude sign-in for browser access. This requires a dedicated xcb Keychain entry for refresh credentials; xcb caches the access token in its private state folder."
    } else {
        "Starting Claude sign-in. xcb will show its sign-in page and keep the token in its own state folder."
    };
    eprintln!(
        "{} {message}",
        crate::ux::Style::stderr().symbol(crate::ux::Symbol::Next)
    );
    let (cancel, receiver) = tokio::sync::watch::channel(false);
    let mut stop = crate::stop::Stop::install()?;
    let (events, prompts) = mpsc::channel(8);
    let (codes, input) = mpsc::channel(1);
    let login = async {
        if browser {
            auth::login_claude_browser_with_interaction(
                store, account, pin, receiver, events, input,
            )
            .await
        } else {
            auth::login_with_interaction(store, account, pin, receiver, events, input).await
        }
    };
    let assistance = serve(prompts, codes);
    tokio::pin!(login, assistance);
    tokio::select! {
        result = &mut login => result,
        complete = &mut assistance => {
            if !complete { let _ = cancel.send(true); }
            login.await
        },
        _ = stop.recv() => { let _ = cancel.send(true); login.await },
    }
}

pub fn check_login_context(
    account: &xcb_core::Id,
    browser: bool,
    machine: bool,
) -> xcb_runtime::Result<()> {
    use xcb_runtime::Error;
    if browser && !cfg!(target_os = "macos") {
        return Err(Error::Unavailable(
            "full Claude browser sign-in requires macOS Keychain; use Codex browser tools on this platform",
        ));
    }
    if !crate::terminal_available() || machine {
        return Err(Error::guided(
            "Claude browser sign-in requires a terminal",
            format!(
                "xcb accounts login {account}{}",
                if browser { " --browser" } else { "" }
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
async fn read_code() -> Option<Zeroizing<String>> {
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::{Errno, read},
        termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcsetattr},
    };
    use std::{
        io::{self, Write},
        time::Duration,
    };

    // Dropping this future on successful browser sign-in or cancellation also
    // restores echo. No blocking input thread survives the login.
    struct Echo(Termios);
    impl Drop for Echo {
        fn drop(&mut self) {
            let _ = tcsetattr(io::stdin(), OptionalActions::Now, &self.0);
        }
    }
    let stdin = io::stdin();
    let original = tcgetattr(&stdin).ok()?;
    let mut hidden = original.clone();
    hidden.local_modes.remove(LocalModes::ECHO);
    tcsetattr(&stdin, OptionalActions::Now, &hidden).ok()?;
    let _echo = Echo(original);
    let mut code = Zeroizing::new(Vec::new());
    loop {
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        match poll(&mut fds, Some(&Timespec::default())) {
            Ok(0) | Err(Errno::INTR) => {}
            Ok(_) if fds[0].revents().contains(PollFlags::IN) => {
                let mut bytes = Zeroizing::new([0_u8; 1024]);
                match read(&stdin, &mut *bytes) {
                    Ok(0) => return None,
                    Ok(length) => {
                        for byte in &bytes[..length] {
                            if matches!(*byte, b'\n' | b'\r') {
                                eprintln!();
                                if code.is_empty() {
                                    eprint!(
                                        "Waiting for browser sign-in. Paste a code if one is shown: "
                                    );
                                    let _ = io::stderr().flush();
                                    continue;
                                }
                                return String::from_utf8(code.to_vec()).ok().map(Zeroizing::new);
                            }
                            if !byte.is_ascii_graphic() || code.len() >= 4096 {
                                return None;
                            }
                            code.push(*byte);
                        }
                    }
                    Err(Errno::INTR | Errno::AGAIN) => {}
                    Err(_) => return None,
                }
            }
            Ok(_) | Err(_) => return None,
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[cfg(not(unix))]
async fn read_code() -> Option<Zeroizing<String>> {
    None
}

/// False means input ended before sign-in; the owner must request cancellation
/// and await the supervised login's physical cleanup.
pub async fn serve(
    mut events: mpsc::Receiver<ClaudeLoginEvent>,
    codes: mpsc::Sender<Zeroizing<String>>,
) -> bool {
    use std::io::{self, Write};

    while let Some(event) = events.recv().await {
        match event {
            ClaudeLoginEvent::AuthorizationUrl(url) => {
                eprintln!("Claude sign-in page: {url}");
                if super::device_sign_in::open_browser(&url).await {
                    eprintln!(
                        "Sent the page to your browser. If it does not appear, open the link above."
                    );
                } else {
                    eprintln!("Open the link above in your browser to sign in.");
                }
            }
            ClaudeLoginEvent::CodeRequested => {
                eprintln!("Finish signing in in your browser.");
                eprint!(
                    "If Claude shows a code, paste it here and press Enter (input hidden; Ctrl+C cancels): "
                );
                let _ = io::stderr().flush();
                let Some(code) = read_code().await else {
                    return false;
                };
                if codes.send(code).await.is_err() {
                    return true;
                }
            }
        }
    }
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{self, Write};

    #[tokio::test]
    async fn code_input_child() {
        let Some(mode) = std::env::var_os("XCB_CLAUDE_CODE_CHILD") else {
            return;
        };
        println!("CODE_READY");
        io::stdout().flush().unwrap();
        if mode == "cancel" {
            {
                let input = read_code();
                tokio::pin!(input);
                tokio::select! {
                    _ = &mut input => panic!("Input completed before cancellation"),
                    _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {},
                }
            }
            println!("CODE_RESULT:cancel");
        } else {
            let code = read_code().await;
            if mode == "code" {
                assert_eq!(
                    code.as_deref().map(|code| code.as_str()),
                    Some("fixture-code#state")
                );
                println!("CODE_RESULT:ok");
            } else {
                assert!(code.is_none());
                println!("CODE_RESULT:eof");
            }
        }
    }

    #[test]
    fn code_input_is_hidden_and_restores_echo_on_completion_eof_and_cancellation() {
        use std::{path::Path, process::Command};
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("Python 3 is required for terminal regression tests");
        let script = r#"
import os, pty, select, subprocess, sys, termios, time
master, slave = pty.openpty()
original = termios.tcgetattr(slave)
child = subprocess.Popen([sys.argv[1], '--exact', 'claude_sign_in::tests::code_input_child', '--nocapture'], stdin=slave, stdout=slave, stderr=slave)
data = bytearray()
def receive(seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], .02)[0]:
            chunk = os.read(master, 65536)
            if not chunk:
                break
            data.extend(chunk)
try:
    deadline = time.monotonic() + 10
    while (b'CODE_READY' not in data or termios.tcgetattr(slave)[3] & termios.ECHO) and time.monotonic() < deadline:
        receive(.02)
    assert b'CODE_READY' in data, data
    assert not (termios.tcgetattr(slave)[3] & termios.ECHO), data
    mode = sys.argv[2]
    if mode == 'code':
        os.write(master, b'\n')
        receive(.05)
        assert b'CODE_RESULT' not in data, data
        os.write(master, b'fixture-code#state\n')
        expected = b'CODE_RESULT:ok'
    elif mode == 'eof':
        os.write(master, b'\x04')
        expected = b'CODE_RESULT:eof'
    else:
        expected = b'CODE_RESULT:cancel'
    while expected not in data and time.monotonic() < deadline:
        receive(.02)
    assert expected in data, data
    assert b'fixture-code#state' not in data, data
    child.wait(timeout=2)
    assert child.returncode == 0, data
    assert termios.tcgetattr(slave) == original, 'Terminal settings were not restored'
finally:
    os.close(master)
    os.close(slave)
    if child.poll() is None:
        child.kill()
        child.wait(timeout=2)
"#;
        for mode in ["code", "eof", "cancel"] {
            let output = Command::new(python)
                .args(["-c", script])
                .arg(std::env::current_exe().unwrap())
                .arg(mode)
                .env("XCB_CLAUDE_CODE_CHILD", mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
