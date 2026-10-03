//! Guided Devin sign-in. Native auth owns its PKCE link and code exchange;
//! xcb owns the isolated profile and publishes only the selected account token.
use xcb_core::Id;
use xcb_runtime::devin::auth::DevinLoginEvent;
use xcb_runtime::{Result, process::Pin, store::Store};

async fn serve(
    mut events: tokio::sync::mpsc::Receiver<DevinLoginEvent>,
    codes: tokio::sync::mpsc::Sender<zeroize::Zeroizing<String>>,
) -> bool {
    while let Some(event) = events.recv().await {
        match event {
            DevinLoginEvent::AuthorizationUrl(url) => {
                eprintln!("Devin sign-in page: {url}");
                if super::device_sign_in::open_browser(&url).await {
                    eprintln!(
                        "Sent the page to your browser. If it does not appear, open the link above."
                    );
                } else {
                    eprintln!("Open the link above in your browser to sign in.");
                }
            }
            DevinLoginEvent::CodeRequested => {
                eprintln!("Choose your account in the browser, then copy the sign-in code.");
                let Some(code) = super::code_sign_in::read_code().await else {
                    return false;
                };
                if codes.send(code).await.is_err() {
                    return true;
                }
                eprintln!("Code submitted. Finishing Devin sign-in; Ctrl+C cancels.");
            }
        }
    }
    true
}

pub async fn login(
    store: &Store,
    account: &Id,
    pin: &Pin,
    cancel: tokio::sync::watch::Sender<bool>,
    cancelled: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let (events, prompts) = tokio::sync::mpsc::channel(8);
    let (codes, input) = tokio::sync::mpsc::channel(1);
    let login = xcb_runtime::devin::auth::login_with_interaction(
        store, account, pin, cancelled, events, input,
    );
    let assistance = serve(prompts, codes);
    tokio::pin!(login, assistance);
    tokio::select! {
        result = &mut login => result,
        complete = &mut assistance => {
            if !complete { let _ = cancel.send(true); }
            login.await
        }
    }
}
