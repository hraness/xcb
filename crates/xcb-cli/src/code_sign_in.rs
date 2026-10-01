//! One hidden, bounded code reader shared by provider browser sign-in flows.
use zeroize::Zeroizing;

#[cfg(unix)]
pub async fn read_code() -> Option<Zeroizing<String>> {
    use std::io::{self, Write};
    let mut retry = false;
    loop {
        let mut input = super::terminal_input::Reader::new(true, 4096).ok()?;
        if retry {
            eprint!("Waiting for browser sign-in. Paste a code if one is shown: ");
        } else {
            eprint!("Paste the sign-in code here and press Enter (input hidden; Ctrl+C cancels): ");
        }
        let _ = io::stderr().flush();
        let code = input.read_line().await.ok()??;
        eprintln!();
        let value = code.trim();
        if value.is_empty() {
            retry = true;
            continue;
        }
        if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return None;
        }
        return Some(Zeroizing::new(value.to_owned()));
    }
}

#[cfg(not(unix))]
pub async fn read_code() -> Option<Zeroizing<String>> {
    None
}
