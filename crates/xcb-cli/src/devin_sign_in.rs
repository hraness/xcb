//! Guided Devin sign-in. Native auth owns its PKCE link and code exchange;
//! xcb owns the isolated profile and publishes only the selected account token.
use xcb_core::Id;
use xcb_runtime::{Result, process::Pin, store::Store};

pub async fn login(store: &Store, account: &Id, pin: &Pin) -> Result<()> {
    let mut stop = crate::stop::Stop::install()?;
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    eprintln!("Open the Devin sign-in link printed below in your browser.");
    eprintln!("Choose your account, then copy the sign-in code and paste it here.");
    eprintln!("Press Ctrl+C to cancel.");
    let login = xcb_runtime::devin::auth::login_with_cancel(store, account, pin, cancelled);
    tokio::pin!(login);
    tokio::select! {
        result = &mut login => result,
        _ = stop.recv() => {
            let _ = cancel.send(true);
            login.await
        }
    }
}
