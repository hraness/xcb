//! Scratch live probe against the local anonymous Convex deployment.
//! Run: XCB_SMOKE_ROOT=/path/to/state XCB_COMMAND=<publicId> cargo test -p xcb-runtime --test ws_probe -- --ignored --nocapture

#![allow(clippy::all)]

use xcb_runtime::cloud::controller::Controller;
use xcb_runtime::cloud::custody;

#[tokio::test]
#[ignore]
async fn ws_probe() {
    let root = std::env::var("XCB_SMOKE_ROOT").expect("XCB_SMOKE_ROOT");
    let root = std::path::Path::new(&root);
    let device = custody::load_device(root).expect("device").expect("device");
    let link = custody::load_link(root).expect("link").expect("link");
    let session = custody::load_session(root)
        .expect("session")
        .expect("session");
    let (key, version) = custody::load_account_key(root).expect("key").expect("key");
    let mut controller =
        Controller::open(device, key, version, session, &link.deployment_url, root)
            .await
            .expect("controller");

    let public_id = std::env::var("XCB_COMMAND").expect("XCB_COMMAND");
    let command = controller.command(&public_id).await.expect("command");
    println!("command {public_id} => {:?}", command.state);
    match controller.open_result(&command) {
        Ok(result) => println!("result: {}", String::from_utf8_lossy(&result)),
        Err(e) => println!("no result: {e}"),
    }
}
