//! Claude's browser handoff and optional code entry. Provider output and the
//! reusable token stay in the runtime; this terminal receives only login events.
use super::code_sign_in::read_code;
use tokio::sync::mpsc;
use xcb_runtime::auth::ClaudeLoginEvent;
use zeroize::Zeroizing;

pub async fn login(
    store: &xcb_runtime::store::Store,
    account: &xcb_core::Id,
    pin: &xcb_runtime::process::Pin,
    browser: bool,
    machine: bool,
    cancel: tokio::sync::watch::Sender<bool>,
    receiver: tokio::sync::watch::Receiver<bool>,
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

/// False means input ended before sign-in; the owner must request cancellation
/// and await the supervised login's physical cleanup.
pub async fn serve(
    mut events: mpsc::Receiver<ClaudeLoginEvent>,
    codes: mpsc::Sender<Zeroizing<String>>,
) -> bool {
    while let Some(event) = events.recv().await {
        match event {
            ClaudeLoginEvent::AuthorizationUrl(url) => {
                // The provider launches its own browser handoff whose
                // redirect_uri targets the loopback listener; the printed
                // link is the manual fallback variant whose page cannot
                // reach it, so it is shown but not auto-opened.
                eprintln!("Claude sign-in page: {url}");
            }
            ClaudeLoginEvent::CodeRequested => {
                eprintln!("Finish signing in in your browser.");
                let Some(code) = read_code().await else {
                    return false;
                };
                if codes.send(code).await.is_err() {
                    return true;
                }
                eprintln!("Code submitted. Finishing Claude sign-in; Ctrl+C cancels.");
                // The exchange can sit behind a late provider input mount or a
                // slow endpoint; keep a heartbeat so a wait never looks dead.
                tokio::spawn(async {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
                        eprintln!("Still finishing Claude sign-in; Ctrl+C cancels.");
                    }
                });
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
