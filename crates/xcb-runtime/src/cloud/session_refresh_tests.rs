//! Synthetic refresh requests only: no Convex client, credentials or network.
use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENDPOINT: &str = "https://fixture.invalid";

fn issued(generation: u64, due: bool) -> CloudSession {
    let claims = json!({
        "iss": "https://fixture.invalid/auth", "sub": "fixture-user|fixture-session",
        "aud": "convex", "nonce": generation,
        "exp": if due { 1 } else { now_ms() / 1000 + 3600 },
    });
    let mut session = CloudSession::issue(
        format!(
            "e30.{}.synthetic",
            encode_base64url(&serde_json::to_vec(&claims).unwrap())
        ),
        format!("synthetic-refresh-{generation}"),
        now_ms(),
    );
    session.deployment_url = Some(ENDPOINT.into());
    session
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    session: CloudSession,
    binding: custody::SessionBinding,
}

impl Fixture {
    fn new(due: bool) -> Self {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap().join("state");
        let device = DeviceIdentity::generate().unwrap();
        custody::store_device(&root, &device, "daemon", "fixture", &device.device).unwrap();
        custody::store_account_key(&root, &AccountKey::generate(), 1).unwrap();
        custody::store_link(
            &root,
            &RelayLink {
                deployment_url: ENDPOINT.into(),
                boot_generation: 1,
            },
        )
        .unwrap();
        let session = issued(0, due);
        custody::store_session(&root, &session).unwrap();
        let binding = custody::SessionBinding::capture(&root, ENDPOINT, &session).unwrap();
        Self {
            _temp: temp,
            root,
            session,
            binding,
        }
    }

    fn session_path(&self) -> PathBuf {
        self.root.join("cloud/session.json")
    }
    fn lock_path(&self) -> PathBuf {
        self.root.join("cloud/session.refresh.lock")
    }
    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.session_path()).unwrap()
    }
}

#[tokio::test]
async fn concurrent_refreshers_spend_once_and_waiter_adopts_the_published_session() {
    let f = Fixture::new(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered, started) = tokio::sync::oneshot::channel();
    let first = refresh_custody(
        &f.root,
        &f.binding,
        &f.session,
        false,
        async |current: &CloudSession| {
            assert_eq!(current.refresh_token, f.session.refresh_token);
            calls.fetch_add(1, Ordering::SeqCst);
            entered.send(()).unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(issued(1, false))
        },
    );
    let second = async {
        started.await.unwrap();
        refresh_custody(
            &f.root,
            &f.binding,
            &f.session,
            false,
            async |_: &CloudSession| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(issued(2, false))
            },
        )
        .await
    };
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.token, second.token);
    assert_eq!(
        custody::load_session(&f.root).unwrap().unwrap().token,
        first.token
    );
}

#[tokio::test]
async fn fresh_but_stale_memory_and_forced_recovery_adopt_without_another_rotation() {
    for force in [false, true] {
        let f = Fixture::new(false);
        let newer = issued(1, false);
        custody::store_session(&f.root, &newer).unwrap();
        let adopted = refresh_custody(
            &f.root,
            &f.binding,
            &f.session,
            force,
            async |_: &CloudSession| panic!("a peer already published a fresh session"),
        )
        .await
        .unwrap();
        assert_eq!(adopted.token, newer.token);
        assert_eq!(adopted.refresh_token, newer.refresh_token);
    }
}

#[tokio::test]
async fn forced_recovery_rotates_only_the_same_failing_session() {
    let f = Fixture::new(false);
    let calls = AtomicUsize::new(0);
    let fresh = refresh_custody(
        &f.root,
        &f.binding,
        &f.session,
        true,
        async |current: &CloudSession| {
            assert_eq!(current.token, f.session.token);
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(issued(1, false))
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_ne!(fresh.token, f.session.token);
    custody::store_session(&f.root, &issued(2, true)).unwrap();
    assert!(
        refresh_custody(
            &f.root,
            &f.binding,
            &fresh,
            true,
            async |_: &CloudSession| {
                panic!("forced recovery must not rotate a different failing session")
            }
        )
        .await
        .is_err(),
        "an expired replacement must not poison the socket"
    );
}

#[tokio::test]
async fn refresh_failure_preserves_exact_bytes_and_releases_the_lock() {
    let f = Fixture::new(true);
    let original = f.bytes();
    assert!(
        refresh_custody(
            &f.root,
            &f.binding,
            &f.session,
            false,
            async |_: &CloudSession| { Err(Error::Unavailable("synthetic refresh failure")) }
        )
        .await
        .is_err()
    );
    assert_eq!(f.bytes(), original);
    custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_cleared_session_is_never_recreated_after_the_auth_await() {
    let f = Fixture::new(true);
    assert!(
        refresh_custody(
            &f.root,
            &f.binding,
            &f.session,
            false,
            async |_: &CloudSession| {
                tokio::task::yield_now().await;
                custody::clear_session(&f.root).unwrap();
                Ok(issued(1, false))
            }
        )
        .await
        .is_err()
    );
    assert!(!f.session_path().exists());
}

#[test]
fn clear_cannot_interleave_between_the_publication_guard_and_rename() {
    let f = Fixture::new(true);
    let snapshot = custody::session_snapshot(&f.root).unwrap();
    let root = f.root.clone();
    let (start, started) = std::sync::mpsc::channel();
    let (finished, done) = std::sync::mpsc::channel();
    let clear = std::thread::spawn(move || {
        started.recv().unwrap();
        custody::clear_session(&root).unwrap();
        finished.send(()).unwrap();
    });
    let checks = std::cell::Cell::new(0);
    custody::replace_session(&f.root, &snapshot, &issued(1, false), || {
        checks.set(checks.get() + 1);
        if checks.get() == 2 {
            start.send(()).unwrap();
            assert!(
                matches!(
                    done.recv_timeout(Duration::from_millis(30)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ),
                "clear must wait until publication completes"
            );
        }
        Ok(())
    })
    .unwrap();
    done.recv_timeout(Duration::from_secs(2)).unwrap();
    clear.join().unwrap();
    assert!(
        !f.session_path().exists(),
        "logout wins after the in-flight publication finishes"
    );
}

#[tokio::test]
async fn changed_or_identically_recreated_sessions_are_not_overwritten() {
    for same_bytes in [false, true] {
        let f = Fixture::new(true);
        let replacement = if same_bytes {
            f.session.clone()
        } else {
            issued(9, false)
        };
        assert!(
            refresh_custody(
                &f.root,
                &f.binding,
                &f.session,
                false,
                async |_: &CloudSession| {
                    tokio::task::yield_now().await;
                    custody::clear_session(&f.root).unwrap();
                    custody::store_session(&f.root, &replacement).unwrap();
                    Ok(issued(1, false))
                }
            )
            .await
            .is_err()
        );
        assert_eq!(
            custody::load_session(&f.root).unwrap().unwrap().token,
            replacement.token
        );
    }
}

#[tokio::test]
async fn same_inode_content_and_private_metadata_drift_reject_publication() {
    for change in ["contents", "mode", "links"] {
        let f = Fixture::new(true);
        let original = f.bytes();
        let mut expected = original.clone();
        if change == "contents" {
            expected.push(b' ');
        }
        assert!(
            refresh_custody(
                &f.root,
                &f.binding,
                &f.session,
                false,
                async |_: &CloudSession| {
                    match change {
                        "contents" => std::fs::write(f.session_path(), &expected).unwrap(),
                        "mode" => std::fs::set_permissions(
                            f.session_path(),
                            std::fs::Permissions::from_mode(0o644),
                        )
                        .unwrap(),
                        "links" => std::fs::hard_link(
                            f.session_path(),
                            f.root.join("cloud/duplicate-session.json"),
                        )
                        .unwrap(),
                        _ => unreachable!(),
                    }
                    Ok(issued(1, false))
                }
            )
            .await
            .is_err()
        );
        assert_eq!(f.bytes(), expected);
    }
}

#[tokio::test]
async fn endpoint_device_account_and_user_changes_fail_before_spending_a_token() {
    for change in ["endpoint", "device", "account", "user"] {
        let f = Fixture::new(true);
        match change {
            "endpoint" => custody::store_link(
                &f.root,
                &RelayLink {
                    deployment_url: "https://other.invalid".into(),
                    boot_generation: 1,
                },
            )
            .unwrap(),
            "device" => {
                let device = DeviceIdentity::generate().unwrap();
                custody::store_device(&f.root, &device, "daemon", "fixture", &device.device)
                    .unwrap();
            }
            "account" => custody::store_account_key(&f.root, &AccountKey::generate(), 1).unwrap(),
            "user" => {
                let mut other = issued(1, false);
                let claims = json!({"iss":"https://fixture.invalid/auth", "sub":"other-user|session", "aud":"convex", "exp":now_ms()/1000+3600});
                other.token = format!(
                    "e30.{}.synthetic",
                    encode_base64url(&serde_json::to_vec(&claims).unwrap())
                );
                custody::store_session(&f.root, &other).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            refresh_custody(
                &f.root,
                &f.binding,
                &f.session,
                false,
                async |_: &CloudSession| { panic!("changed identity must fail before auth") }
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn custody_changes_during_auth_do_not_publish_into_another_identity() {
    for change in ["endpoint", "device", "account", "account-removed"] {
        let f = Fixture::new(true);
        let original = f.bytes();
        assert!(
            refresh_custody(
                &f.root,
                &f.binding,
                &f.session,
                false,
                async |_: &CloudSession| {
                    match change {
                        "endpoint" => custody::store_link(
                            &f.root,
                            &RelayLink {
                                deployment_url: "https://other.invalid".into(),
                                boot_generation: 1,
                            },
                        )
                        .unwrap(),
                        "device" => {
                            let device = DeviceIdentity::generate().unwrap();
                            custody::store_device(
                                &f.root,
                                &device,
                                "daemon",
                                "fixture",
                                &device.device,
                            )
                            .unwrap();
                        }
                        "account" => {
                            custody::store_account_key(&f.root, &AccountKey::generate(), 2).unwrap()
                        }
                        "account-removed" => {
                            std::fs::remove_file(f.root.join("cloud/account.json")).unwrap()
                        }
                        _ => unreachable!(),
                    }
                    Ok(issued(1, false))
                }
            )
            .await
            .is_err()
        );
        assert_eq!(f.bytes(), original);
    }
}

#[tokio::test]
async fn a_refresh_response_cannot_change_auth_identity_or_endpoint() {
    for field in ["iss", "sub", "aud", "deployment"] {
        let f = Fixture::new(true);
        let original = f.bytes();
        assert!(refresh_custody(&f.root, &f.binding, &f.session, false, async |_: &CloudSession| {
            let mut fresh = issued(1, false);
            if field == "deployment" {
                fresh.deployment_url = Some("https://other.invalid".into());
            } else {
                let mut claims = json!({"iss":"https://fixture.invalid/auth", "sub":"fixture-user|fixture-session", "aud":"convex", "exp":now_ms()/1000+3600});
                claims[field] = json!("unrelated-identity");
                fresh.token = format!("e30.{}.synthetic", encode_base64url(&serde_json::to_vec(&claims).unwrap()));
            }
            Ok(fresh)
        }).await.is_err());
        assert_eq!(f.bytes(), original);
    }
}

#[tokio::test]
async fn legacy_linked_sessions_refresh_and_boot_generation_changes_are_allowed() {
    let mut f = Fixture::new(true);
    f.session.deployment_url = None;
    custody::store_session(&f.root, &f.session).unwrap();
    f.binding = custody::SessionBinding::capture(&f.root, ENDPOINT, &f.session).unwrap();
    custody::store_link(
        &f.root,
        &RelayLink {
            deployment_url: ENDPOINT.into(),
            boot_generation: 99,
        },
    )
    .unwrap();
    let fresh = refresh_custody(
        &f.root,
        &f.binding,
        &f.session,
        false,
        async |_: &CloudSession| Ok(issued(1, false)),
    )
    .await
    .unwrap();
    assert_eq!(fresh.deployment_url.as_deref(), Some(ENDPOINT));
}

#[tokio::test]
async fn interrupted_enrollment_supports_missing_keys_but_requires_a_known_origin() {
    let f = Fixture::new(true);
    for name in ["device.json", "account.json", "relay.json"] {
        std::fs::remove_file(f.root.join("cloud").join(name)).unwrap();
    }
    let binding = custody::SessionBinding::capture(&f.root, ENDPOINT, &f.session).unwrap();
    let fresh = refresh_custody(
        &f.root,
        &binding,
        &f.session,
        false,
        async |_: &CloudSession| Ok(issued(1, false)),
    )
    .await
    .unwrap();
    assert_eq!(fresh.deployment_url.as_deref(), Some(ENDPOINT));
    let mut legacy = f.session.clone();
    legacy.deployment_url = None;
    assert!(
        matches!(custody::SessionBinding::capture(&f.root, ENDPOINT, &legacy), Err(Error::Conflict(message)) if message.contains("xcb link"))
    );
    assert!(
        custody::SessionBinding::capture(&f.root, "https://other.invalid", &f.session).is_err()
    );
}

#[tokio::test]
async fn optional_enrollment_records_cannot_appear_mid_refresh() {
    let f = Fixture::new(true);
    std::fs::remove_file(f.root.join("cloud/account.json")).unwrap();
    let binding = custody::SessionBinding::capture(&f.root, ENDPOINT, &f.session).unwrap();
    let original = f.bytes();
    assert!(
        refresh_custody(
            &f.root,
            &binding,
            &f.session,
            false,
            async |_: &CloudSession| {
                custody::store_account_key(&f.root, &AccountKey::generate(), 1).unwrap();
                Ok(issued(1, false))
            }
        )
        .await
        .is_err()
    );
    assert_eq!(f.bytes(), original);
}

#[tokio::test]
async fn loaded_device_and_account_keys_must_match_the_clients_original_intent() {
    let f = Fixture::new(true);
    let device = custody::load_device(&f.root).unwrap().unwrap();
    let (account, version) = custody::load_account_key(&f.root).unwrap().unwrap();
    f.binding
        .expect_keys(&f.root, &device, &account, version)
        .unwrap();
    assert!(
        f.binding
            .expect_keys(
                &f.root,
                &DeviceIdentity::generate().unwrap(),
                &account,
                version
            )
            .is_err()
    );
    assert!(
        f.binding
            .expect_keys(&f.root, &device, &AccountKey::generate(), version)
            .is_err()
    );
    assert!(
        f.binding
            .expect_keys(&f.root, &device, &account, version + 1)
            .is_err()
    );
}

#[tokio::test]
async fn stable_lock_is_private_bounded_and_reused_after_explicit_release() {
    let f = Fixture::new(true);
    let held = custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
    let before = std::fs::metadata(f.lock_path()).unwrap();
    assert_eq!(before.mode() & 0o777, 0o600);
    assert_eq!(before.len(), 0);
    let start = std::time::Instant::now();
    assert!(
        custody::session_refresh_lock(&f.root, Duration::from_millis(20))
            .await
            .is_err()
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(held);
    custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
    let after = std::fs::metadata(f.lock_path()).unwrap();
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
}

#[tokio::test]
async fn lock_symlinks_and_non_private_modes_are_rejected() {
    let f = Fixture::new(true);
    std::os::unix::fs::symlink(f.session_path(), f.lock_path()).unwrap();
    assert!(
        custody::session_refresh_lock(&f.root, Duration::from_millis(20))
            .await
            .is_err()
    );
    std::fs::remove_file(f.lock_path()).unwrap();
    crate::private::create(&f.lock_path(), b"").unwrap();
    std::fs::set_permissions(f.lock_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        custody::session_refresh_lock(&f.root, Duration::from_millis(20))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn lock_identity_changes_while_waiting_are_rejected() {
    let f = Fixture::new(true);
    let held = custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
    let waiting = custody::session_refresh_lock(&f.root, Duration::from_millis(100));
    tokio::pin!(waiting);
    tokio::select! {
        biased;
        _ = &mut waiting => panic!("the first holder still owns the lock"),
        _ = std::future::ready(()) => (),
    }
    std::fs::rename(f.lock_path(), f.root.join("cloud/retired-lock")).unwrap();
    crate::private::create(&f.lock_path(), b"").unwrap();
    assert!(waiting.await.is_err());
    drop(held);
}

#[tokio::test]
async fn lock_identity_or_mode_changes_during_auth_reject_publication() {
    for replace in [false, true] {
        let f = Fixture::new(true);
        let original = f.bytes();
        assert!(
            refresh_custody(
                &f.root,
                &f.binding,
                &f.session,
                false,
                async |_: &CloudSession| {
                    if replace {
                        std::fs::rename(f.lock_path(), f.root.join("cloud/retired-lock")).unwrap();
                        crate::private::create(&f.lock_path(), b"").unwrap();
                    } else {
                        std::fs::set_permissions(
                            f.lock_path(),
                            std::fs::Permissions::from_mode(0o644),
                        )
                        .unwrap();
                    }
                    Ok(issued(1, false))
                }
            )
            .await
            .is_err()
        );
        assert_eq!(f.bytes(), original);
    }
}

#[tokio::test]
async fn cancelling_a_refresh_releases_its_lock_without_publishing() {
    let f = Fixture::new(true);
    let original = f.bytes();
    let root = f.root.clone();
    let binding = f.binding.clone();
    let session = f.session.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        refresh_custody(
            &root,
            &binding,
            &session,
            false,
            async |_: &CloudSession| {
                entered.send(()).unwrap();
                std::future::pending::<Result<CloudSession>>().await
            },
        )
        .await
    });
    started.await.unwrap();
    task.abort();
    assert!(task.await.is_err());
    custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
    assert_eq!(f.bytes(), original);
}
