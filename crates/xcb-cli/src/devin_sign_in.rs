//! Guided Devin sign-in. Native auth owns its callback and browser handoff;
//! xcb owns the isolated profile and publishes only the selected account token.
use xcb_core::Id;
use xcb_runtime::{Result, process::Pin, store::Store};

pub async fn login(store: &Store, account: &Id, pin: &Pin) -> Result<()> {
    let mut stop = crate::stop::Stop::install()?;
    let (cancel, cancelled) = tokio::sync::watch::channel(false);
    eprintln!("Sign in to Devin using the browser page shown below.");
    eprintln!(
        "If the browser does not open, copy the sign-in link from this terminal into your browser."
    );
    eprintln!("Choose the account you want to connect. Press Ctrl+C to cancel.");
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
