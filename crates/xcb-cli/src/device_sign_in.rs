//! Terminal assistance for Codex's one-time device sign-in challenge.
//! Reusable provider credentials never reach this module.
use std::io::{self, Write};
#[cfg(any(target_os = "macos", target_os = "linux", test))]
use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Stdio;
use tokio::sync::mpsc;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tokio::{io::AsyncWriteExt, process::Command};
use xcb_runtime::runner::DeviceLoginPrompt;

const DEVICE_URL: &str = "https://auth.openai.com/codex/device";

fn valid_prompt(prompt: &DeviceLoginPrompt) -> bool {
    let code = prompt.code.as_bytes();
    let digits = code.len() == 10
        && code[4] == b'-'
        && code[..4].iter().all(u8::is_ascii_digit)
        && code[5..].iter().all(u8::is_ascii_digit);
    let letters = code.len() == 9
        && code[4] == b'-'
        && code[..4].iter().all(u8::is_ascii_uppercase)
        && code[5..].iter().all(u8::is_ascii_uppercase);
    prompt.url == DEVICE_URL && (digits || letters)
}

async fn wait_for_enter() -> bool {
    let mut retry = false;
    loop {
        let Ok(mut reader) = crate::terminal_input::Reader::new(false, 1024) else {
            return false;
        };
        if retry {
            eprint!("Press Enter to open the sign-in page: ");
            let _ = io::stderr().flush();
        }
        match reader.read_line().await {
            Ok(Some(line)) if line.is_empty() => return true,
            Ok(Some(line)) if line.as_str() == "skip" => return false,
            Ok(Some(_)) => retry = true,
            Ok(None) | Err(_) => return false,
        }
    }
}

trait Effects {
    async fn copy_code(&self, code: &str) -> bool;
    async fn open_url(&self, url: &str) -> bool;
}

struct SystemEffects;

/// An OS helper may fail or stall; sign-in still works with the printed URL.
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run_helper(executable: &str, args: &[&str], input: Option<&str>) -> bool {
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        if let Some(input) = input {
            let Some(mut stdin) = child.stdin.take() else {
                return false;
            };
            if stdin.write_all(input.as_bytes()).await.is_err() {
                return false;
            }
            drop(stdin);
        }
        child.wait().await.is_ok_and(|status| status.success())
    })
    .await;
    match result {
        Ok(true) => true,
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            false
        }
    }
}

impl Effects for SystemEffects {
    async fn copy_code(&self, code: &str) -> bool {
        #[cfg(target_os = "macos")]
        {
            run_helper("/usr/bin/pbcopy", &[], Some(code)).await
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = code;
            false
        }
    }

    async fn open_url(&self, url: &str) -> bool {
        #[cfg(target_os = "macos")]
        {
            run_helper("/usr/bin/open", &[url], None).await
        }
        #[cfg(target_os = "linux")]
        {
            run_helper("/usr/bin/xdg-open", &[url], None).await
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = url;
            false
        }
    }
}

async fn assist<E: Effects>(
    prompt: DeviceLoginPrompt,
    effects: &E,
    enter: impl Future<Output = bool>,
) {
    if !valid_prompt(&prompt) {
        return;
    }
    eprintln!("Codex sign-in code: {}", prompt.code);
    if effects.copy_code(&prompt.code).await {
        eprintln!("Copied the code to your clipboard.");
    } else {
        eprintln!("Copy the code above to enter on the sign-in page.");
    }
    eprint!(
        "Press Enter to open {} (type skip to continue manually): ",
        prompt.url
    );
    let _ = io::stderr().flush();
    if enter.await {
        eprintln!();
        if !effects.open_url(&prompt.url).await {
            eprintln!(
                "Open {} in your browser and enter the code above.",
                prompt.url
            );
        }
    } else {
        eprintln!(
            "\nOpen {} in your browser when you're ready to sign in.",
            prompt.url
        );
    }
}

pub async fn serve(mut prompts: mpsc::Receiver<DeviceLoginPrompt>) {
    if let Some(prompt) = prompts.recv().await {
        assist(prompt, &SystemEffects, wait_for_enter()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeEffects {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }
    impl Effects for FakeEffects {
        async fn copy_code(&self, code: &str) -> bool {
            self.calls.lock().unwrap().push(format!("copy:{code}"));
            !self.fail
        }
        async fn open_url(&self, url: &str) -> bool {
            self.calls.lock().unwrap().push(format!("open:{url}"));
            !self.fail
        }
    }
    fn prompt() -> DeviceLoginPrompt {
        DeviceLoginPrompt {
            url: DEVICE_URL.to_owned(),
            code: "ABCD-EFGH".to_owned(),
        }
    }
    #[tokio::test]
    async fn assistance_copies_only_device_code_and_opens_after_enter() {
        let effects = FakeEffects::default();
        assist(prompt(), &effects, async { true }).await;
        assert_eq!(
            *effects.calls.lock().unwrap(),
            ["copy:ABCD-EFGH", &format!("open:{DEVICE_URL}")]
        );
    }
    #[tokio::test]
    async fn cancellation_does_not_open_and_failed_copy_keeps_manual_path() {
        let effects = FakeEffects {
            fail: true,
            ..Default::default()
        };
        assist(prompt(), &effects, async { false }).await;
        assert_eq!(*effects.calls.lock().unwrap(), ["copy:ABCD-EFGH"]);
    }
    #[tokio::test]
    async fn unexpected_url_or_secret_like_code_has_no_effects() {
        let effects = FakeEffects::default();
        for challenge in [
            DeviceLoginPrompt {
                url: "https://other.invalid".to_owned(),
                code: "ABCD-EFGH".to_owned(),
            },
            DeviceLoginPrompt {
                url: DEVICE_URL.to_owned(),
                code: "secret\nclipboard".to_owned(),
            },
        ] {
            assist(challenge, &effects, async { true }).await;
        }
        assert!(effects.calls.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn login_completion_drops_pending_enter_without_opening() {
        let effects = FakeEffects::default();
        {
            let task = assist(prompt(), &effects, std::future::pending());
            tokio::pin!(task);
            tokio::select! { _ = &mut task => panic!("Enter wait unexpectedly finished"), _ = tokio::time::sleep(Duration::from_millis(10)) => {} }
        }
        assert_eq!(*effects.calls.lock().unwrap(), ["copy:ABCD-EFGH"]);
    }
    #[test]
    fn challenge_formats_match_device_codes_only() {
        assert!(valid_prompt(&prompt()));
        for code in ["1234-56789", "WXYZ-ABCD"] {
            assert!(valid_prompt(&DeviceLoginPrompt {
                url: DEVICE_URL.to_owned(),
                code: code.to_owned()
            }));
        }
        for code in [
            "1234-5678",
            "abcd-efgh",
            "sk-ant-oat01-token",
            "ABC1-EFGH",
            "ABCD-EFGH\n",
        ] {
            assert!(!valid_prompt(&DeviceLoginPrompt {
                url: DEVICE_URL.to_owned(),
                code: code.to_owned()
            }));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminal_enter_child() {
        if std::env::var_os("XCB_DEVICE_ENTER_CHILD").is_none() {
            return;
        }
        println!("DEVICE_READY");
        io::stdout().flush().unwrap();
        let open = wait_for_enter().await;
        println!("DEVICE_RESULT:{}", if open { "open" } else { "skip" });
    }

    #[cfg(unix)]
    #[test]
    fn canonical_terminal_requires_empty_enter_and_eof_skips() {
        use std::{path::Path, process::Command};
        let python = ["/usr/bin/python3", "/usr/local/bin/python3"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .expect("Python 3 is required for terminal regression tests");
        let script = r#"
import os, pty, select, subprocess, sys, time
master, slave = pty.openpty()
child = subprocess.Popen([sys.argv[1], '--exact', 'device_sign_in::tests::terminal_enter_child', '--nocapture'], stdin=slave, stdout=slave, stderr=slave)
os.close(slave)
data = bytearray()
def receive(seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if select.select([master], [], [], .02)[0]:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            data.extend(chunk)
try:
    deadline = time.monotonic() + 10
    while b'DEVICE_READY' not in data and time.monotonic() < deadline:
        receive(.1)
    assert b'DEVICE_READY' in data, data
    if sys.argv[2] == 'enter':
        os.write(master, b'x')
        receive(.1)
        assert b'DEVICE_RESULT' not in data, data
        os.write(master, b'\n')
        receive(.1)
        assert b'DEVICE_RESULT' not in data, data
        os.write(master, b'\n')
        expected = b'DEVICE_RESULT:open'
    elif sys.argv[2] == 'eof':
        os.write(master, b'\x04')
        expected = b'DEVICE_RESULT:skip'
    else:
        os.write(master, b'skip\n')
        expected = b'DEVICE_RESULT:skip'
    while expected not in data and time.monotonic() < deadline:
        receive(.1)
    assert expected in data, data
    child.wait(timeout=2)
    assert child.returncode == 0, data
finally:
    os.close(master)
    if child.poll() is None:
        child.kill()
        child.wait()
"#;
        for mode in ["enter", "eof", "skip"] {
            let output = Command::new(python)
                .env("XCB_DEVICE_ENTER_CHILD", "1")
                .args(["-c", script])
                .arg(std::env::current_exe().unwrap())
                .arg(mode)
                .output()
                .unwrap();
            assert!(output.status.success(), "{mode}: {output:?}");
        }
    }
}
