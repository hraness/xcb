//! Entirely synthetic relay and private temporary state: no auth servers.
use super::super::crypto::{AccountKey, DeviceIdentity};
use super::super::custody::RelayLink;
use super::*;
use std::os::unix::fs::PermissionsExt;

const ENDPOINT: &str = "https://reauth-fixture.invalid";

fn issued(session_id: &str) -> CloudSession {
    let claims = json!({
        "iss": "https://reauth-fixture.invalid/auth", "aud": "convex",
        "sub": format!("fixture-user|{session_id}"), "exp": now_ms() / 1000 + 3600,
    });
    let mut session = CloudSession::issue(
        format!(
            "e30.{}.synthetic",
            encode_base64url(&serde_json::to_vec(&claims).unwrap())
        ),
        format!("private-refresh-{session_id}"),
        now_ms(),
    );
    session.deployment_url = Some(ENDPOINT.into());
    session
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    session: CloudSession,
    device: DeviceIdentity,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().canonicalize().unwrap().join("state");
        let device = DeviceIdentity::generate().unwrap();
        custody::store_device(&root, &device, "controller", "fixture", &device.device).unwrap();
        custody::store_account_key(&root, &AccountKey::generate(), 1).unwrap();
        custody::store_link(
            &root,
            &RelayLink {
                deployment_url: ENDPOINT.into(),
                boot_generation: 5,
            },
        )
        .unwrap();
        let session = issued("original-session");
        custody::store_session(&root, &session).unwrap();
        Self {
            _temp: temp,
            root,
            session,
            device,
        }
    }

    fn bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.root.join("cloud").join(name)).unwrap()
    }

    fn rpc(&self, session: &CloudSession) -> FakeRelay {
        FakeRelay {
            root: self.root.clone(),
            identity: SessionIdentity::of(session).unwrap(),
            verify: *self.device.signing.verifying_key(),
            peer: json!({
                "deviceId": self.device.device, "deviceClass": "controller", "keyVersion": 1,
                "label": "fixture", "online": false, "status": "active",
                "signingPublicKey": encode_base64url(&self.device.public.verify_key_spki),
                "agreementPublicKey": encode_base64url(&self.device.public.agreement_key_spki),
            }),
            queries: Vec::new(),
            begins: 0,
            finishes: 0,
            revision: 0,
            challenge: None,
            committed: None,
            lose_finish_once: false,
            challenge_patch: None,
            result_patch: None,
            before_begin_return: None,
            after_finish: None,
            finish_wait: None,
        }
    }
}

struct FakeRelay {
    root: PathBuf,
    identity: SessionIdentity,
    verify: p256::ecdsa::VerifyingKey,
    peer: Value,
    queries: Vec<String>,
    begins: usize,
    finishes: usize,
    revision: u64,
    challenge: Option<Challenge>,
    committed: Option<ReauthResult>,
    lose_finish_once: bool,
    challenge_patch: Option<(&'static str, Value)>,
    result_patch: Option<(&'static str, Value)>,
    before_begin_return: Option<MutationHook>,
    after_finish: Option<MutationHook>,
    finish_wait: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
}

type MutationHook = Box<dyn FnOnce(&Path)>;

impl ReauthRpc for FakeRelay {
    async fn query(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        self.queries.push(path.into());
        match path {
            "auth:currentSubject" => Ok(json!({
                "userId": subject(&self.identity)?.0, "authEpoch": 1, "status": "active", "verifiedAt": null,
            })),
            "relayDevices:list" => Ok(json!([self.peer.clone()])),
            "relayDevices:reauthStatus" => {
                let challenge = args
                    .iter()
                    .find(|(key, _)| *key == "challengeId")
                    .unwrap()
                    .1
                    .as_str()
                    .unwrap();
                if let Some(result) = &self.committed
                    && result.challenge_id == challenge
                    && result.auth_session_id == subject(&self.identity)?.1
                {
                    return Ok(json!({"status":"committed","result":result}));
                }
                Ok(json!({"status":"pending"}))
            }
            _ => panic!("unexpected synthetic query {path}"),
        }
    }

    async fn mutation(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        match path {
            "relayDevices:beginReauth" => {
                self.begins += 1;
                let challenge = Challenge {
                    auth_epoch: 1,
                    auth_session_id: subject(&self.identity)?.1.into(),
                    binding_revision: self.revision,
                    challenge_id: encode_base64url(&[self.begins as u8; 24]),
                    contract: wire::DEVICE_REAUTH_CONTRACT.into(),
                    device_class: "controller".into(),
                    device_id: self.peer["deviceId"].as_str().unwrap().into(),
                    expires_at: now_ms() + 300_000,
                    key_version: 1,
                    nonce: encode_base64url(&[4; 32]),
                    user_id: subject(&self.identity)?.0.into(),
                };
                let mut value = challenge.value()?;
                self.challenge = Some(challenge);
                if let Some((field, value_new)) = &self.challenge_patch {
                    value[*field] = value_new.clone();
                }
                if let Some(change) = self.before_begin_return.take() {
                    change(&self.root);
                }
                Ok(value)
            }
            "relayDevices:finishReauth" => {
                self.finishes += 1;
                let challenge = self.challenge.as_ref().unwrap();
                let signature = args
                    .iter()
                    .find(|(key, _)| *key == "signature")
                    .unwrap()
                    .1
                    .as_str()
                    .unwrap();
                assert!(super::super::crypto::verify_canonical(
                    &self.verify,
                    &challenge.value()?,
                    &super::super::crypto::decode_base64url(signature, 128)?
                ));
                assert_eq!(self.revision, challenge.binding_revision);
                self.revision += 1;
                let result = ReauthResult {
                    auth_epoch: challenge.auth_epoch,
                    auth_session_id: challenge.auth_session_id.clone(),
                    binding_revision: self.revision,
                    challenge_id: challenge.challenge_id.clone(),
                    device_class: challenge.device_class.clone(),
                    device_id: challenge.device_id.clone(),
                    key_version: challenge.key_version,
                    user_id: challenge.user_id.clone(),
                };
                let mut value = serde_json::to_value(&result).unwrap();
                self.committed = Some(result);
                if let Some(change) = self.after_finish.take() {
                    change(&self.root);
                }
                if let Some((entered, release)) = self.finish_wait.take() {
                    entered.notify_one();
                    let _ = release.await;
                }
                if self.lose_finish_once {
                    self.lose_finish_once = false;
                    return Err(Error::Unavailable("synthetic lost response"));
                }
                if let Some((field, value_new)) = &self.result_patch {
                    value[*field] = value_new.clone();
                }
                Ok(value)
            }
            _ => panic!("unexpected synthetic mutation {path}"),
        }
    }
}

#[tokio::test]
async fn renews_only_the_session_and_leaves_a_secret_free_validated_receipt() {
    let f = Fixture::new();
    let unchanged: Vec<_> = [
        custody::name::DEVICE,
        custody::name::ACCOUNT,
        custody::name::RELAY,
    ]
    .into_iter()
    .map(|name| (name, f.bytes(name)))
    .collect();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    let intent = prepare(&f.root).unwrap();
    permit_refresh(&f.root).unwrap();
    let outcome = complete_with(&f.root, intent, fresh.clone(), &mut rpc)
        .await
        .unwrap();
    assert_eq!(outcome.device, f.device.device);
    assert_eq!(rpc.finishes, 1);
    assert_eq!(
        custody::load_session(&f.root).unwrap().unwrap().token,
        fresh.token
    );
    for (name, bytes) in unchanged {
        assert_eq!(f.bytes(name), bytes, "{name}");
    }
    let receipt = String::from_utf8(f.bytes(custody::name::REAUTH)).unwrap();
    assert!(!receipt.contains(&fresh.token));
    assert!(!receipt.contains(&fresh.refresh_token));
    assert!(!receipt.contains(&f.session.refresh_token));
    assert!(matches!(
        relay_state(&f.root).unwrap(),
        RelayReauthState::Ready {
            generation: Some(_)
        }
    ));
    permit_refresh(&f.root).unwrap();
}

#[tokio::test]
async fn lost_finish_response_reconciles_the_same_operation_without_second_effect() {
    let f = Fixture::new();
    let old = f.bytes(custody::name::SESSION);
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    rpc.lose_finish_once = true;
    assert!(
        complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
            .await
            .is_err()
    );
    assert_eq!(f.bytes(custody::name::SESSION), old);
    assert!(permit_refresh(&f.root).is_err());
    let pending = Snapshot::load(&f.root).unwrap().unwrap();
    let challenge = pending
        .journal
        .challenge
        .as_ref()
        .unwrap()
        .challenge_id
        .clone();
    assert!(matches!(pending.journal.phase, Phase::Committing));
    commit_with(&f.root, pending, &mut rpc).await.unwrap();
    assert_eq!((rpc.begins, rpc.finishes), (1, 1));
    assert_eq!(
        Snapshot::load(&f.root)
            .unwrap()
            .unwrap()
            .journal
            .result
            .unwrap()
            .challenge_id,
        challenge
    );
}

#[tokio::test]
async fn explicit_clear_during_otp_cancels_intent_without_a_server_call() {
    let f = Fixture::new();
    let intent = prepare(&f.root).unwrap();
    let guard = custody::session_refresh_lock(&f.root, Duration::from_millis(50))
        .await
        .unwrap();
    drop(guard); // OTP holds no refresh or mutation lock.
    custody::clear_session(&f.root).unwrap();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    assert!(
        complete_with(&f.root, intent, fresh, &mut rpc)
            .await
            .is_err()
    );
    assert!(rpc.queries.is_empty());
    assert!(custody::load_session(&f.root).unwrap().is_none());
    assert!(Snapshot::load(&f.root).unwrap().is_none());
}

#[tokio::test]
async fn clear_after_server_commit_never_resurrects_credentials_or_journal() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    rpc.after_finish = Some(Box::new(|root| custody::clear_session(root).unwrap()));
    assert!(
        complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
            .await
            .is_err()
    );
    assert_eq!(rpc.finishes, 1);
    assert!(custody::load_session(&f.root).unwrap().is_none());
    assert!(Snapshot::load(&f.root).unwrap().is_none());
}

#[tokio::test]
async fn same_bytes_recreated_during_finish_are_a_different_publication_target() {
    let f = Fixture::new();
    let old = f.bytes(custody::name::SESSION);
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    rpc.after_finish = Some(Box::new(|root| {
        let path = custody::path(root, custody::name::SESSION).unwrap();
        let bytes = private::read(&path, MAX_JOURNAL).unwrap();
        std::fs::remove_file(&path).unwrap();
        private::create(&path, &bytes).unwrap();
    }));
    assert!(
        complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
            .await
            .is_err()
    );
    assert_eq!(f.bytes(custody::name::SESSION), old);
    assert!(permit_refresh(&f.root).is_err());
}

#[tokio::test]
async fn ordinary_refresh_during_otp_is_accepted_but_full_subject_changes_are_not() {
    let f = Fixture::new();
    let intent = prepare(&f.root).unwrap();
    let mut rotated = f.session.clone();
    rotated.refresh_token = "private-rotated-original".into();
    custody::store_session(&f.root, &rotated).unwrap();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    complete_with(&f.root, intent, fresh, &mut rpc)
        .await
        .unwrap();

    let f = Fixture::new();
    let intent = prepare(&f.root).unwrap();
    custody::store_session(&f.root, &issued("other-local-session")).unwrap();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    assert!(
        complete_with(&f.root, intent, fresh, &mut rpc)
            .await
            .is_err()
    );
    assert_eq!(rpc.finishes, 0);
}

#[tokio::test]
async fn foreign_owner_issuer_audience_and_peer_identity_are_rejected() {
    for field in ["sub", "iss", "aud"] {
        let f = Fixture::new();
        let mut claims = json!({"iss":"https://reauth-fixture.invalid/auth","aud":"convex","sub":"fixture-user|new-session","exp":now_ms()/1000+3600});
        claims[field] = json!(if field == "sub" {
            "another-user|new-session"
        } else {
            "another-origin"
        });
        let mut fresh = issued("new-session");
        fresh.token = format!(
            "e30.{}.synthetic",
            encode_base64url(&serde_json::to_vec(&claims).unwrap())
        );
        let mut rpc = f.rpc(&fresh);
        assert!(
            complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(rpc.begins, 0);
    }
    for field in [
        "deviceId",
        "deviceClass",
        "status",
        "signingPublicKey",
        "agreementPublicKey",
        "keyVersion",
    ] {
        let f = Fixture::new();
        let fresh = issued("new-session");
        let mut rpc = f.rpc(&fresh);
        rpc.peer[field] = if field == "keyVersion" {
            json!(2)
        } else {
            json!("changed")
        };
        assert!(
            complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(rpc.begins, 0);
    }
}

#[tokio::test]
async fn forged_challenge_fields_and_unsafe_numbers_never_reach_finish() {
    for (field, value) in [
        ("contract", json!(wire::DEVICE_BIND_CONTRACT)),
        ("authSessionId", json!("another-session")),
        ("userId", json!("another-user")),
        ("deviceClass", json!("daemon")),
        ("deviceId", json!("0".repeat(32))),
        ("keyVersion", json!(2)),
        ("bindingRevision", json!(MAX_SAFE_INTEGER)),
        ("authEpoch", json!(0)),
        ("nonce", json!("bad")),
        ("challengeId", json!("bad")),
        ("expiresAt", json!(1)),
        ("expiresAt", json!(MAX_SAFE_INTEGER + 1)),
        ("extra", json!(true)),
    ] {
        let f = Fixture::new();
        let old = f.bytes(custody::name::SESSION);
        let fresh = issued("new-session");
        let mut rpc = f.rpc(&fresh);
        rpc.challenge_patch = Some((field, value));
        assert!(
            complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(rpc.finishes, 0, "{field}");
        assert_eq!(f.bytes(custody::name::SESSION), old);
    }
}

#[tokio::test]
async fn changed_commit_proof_cannot_publish_the_new_session() {
    for (field, value) in [
        ("authSessionId", json!("other")),
        ("bindingRevision", json!(7)),
        ("keyVersion", json!(2)),
        ("userId", json!("other")),
        ("challengeId", json!("bad")),
    ] {
        let f = Fixture::new();
        let old = f.bytes(custody::name::SESSION);
        let fresh = issued("new-session");
        let mut rpc = f.rpc(&fresh);
        rpc.result_patch = Some((field, value));
        assert!(
            complete_with(&f.root, prepare(&f.root).unwrap(), fresh, &mut rpc)
                .await
                .is_err()
        );
        assert_eq!(f.bytes(custody::name::SESSION), old);
        assert!(permit_refresh(&f.root).is_err());
    }
}

#[tokio::test]
async fn seven_day_pending_session_can_be_superseded_by_another_verified_otp() {
    let f = Fixture::new();
    let first = issued("expired-pending-session");
    let mut rpc = f.rpc(&first);
    rpc.lose_finish_once = true;
    assert!(
        complete_with(&f.root, prepare(&f.root).unwrap(), first, &mut rpc)
            .await
            .is_err()
    );
    let snapshot = Snapshot::load(&f.root).unwrap().unwrap();
    let renewed = refresh_pending_with(
        &f.root,
        &snapshot.journal,
        true,
        async |current: &CloudSession| {
            assert_eq!(
                current.refresh_token,
                "private-refresh-expired-pending-session"
            );
            Err(Error::Protocol("auth refresh rejected"))
        },
    )
    .await
    .unwrap();
    assert!(renewed.is_none());
    let intent = prepare(&f.root).unwrap();
    assert!(permit_refresh(&f.root).is_err()); // Still parked during the new OTP.
    let next = issued("replacement-session");
    rpc.identity = SessionIdentity::of(&next).unwrap();
    complete_with(&f.root, intent, next.clone(), &mut rpc)
        .await
        .unwrap();
    assert_eq!((rpc.begins, rpc.finishes, rpc.revision), (2, 2, 2));
    assert_eq!(
        custody::load_session(&f.root).unwrap().unwrap().token,
        next.token
    );
}

async fn server_committed_fixture(f: &Fixture, fresh: &CloudSession) -> (Snapshot, FakeRelay) {
    let mut rpc = f.rpc(fresh);
    rpc.lose_finish_once = true;
    assert!(
        complete_with(&f.root, prepare(&f.root).unwrap(), fresh.clone(), &mut rpc)
            .await
            .is_err()
    );
    let snapshot = Snapshot::load(&f.root).unwrap().unwrap();
    let mut journal = snapshot.journal.clone();
    journal.phase = Phase::ServerCommitted;
    journal.result = rpc.committed.clone();
    (snapshot.replace(&f.root, journal).unwrap(), rpc)
}

#[tokio::test]
async fn crash_after_local_publish_finishes_receipt_without_another_remote_effect() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let (snapshot, mut rpc) = server_committed_fixture(&f, &fresh).await;
    assert!(
        publish(&f.root, snapshot, || {
            if custody::load_session(&f.root)?.unwrap().token == fresh.token {
                return Err(Error::Unavailable("synthetic crash before receipt"));
            }
            Ok(())
        })
        .is_err()
    );
    assert_eq!(
        custody::load_session(&f.root).unwrap().unwrap().token,
        fresh.token
    );
    assert!(permit_refresh(&f.root).is_err());
    let pending = Snapshot::load(&f.root).unwrap().unwrap();
    assert!(matches!(pending.journal.phase, Phase::ServerCommitted));
    commit_with(&f.root, pending, &mut rpc).await.unwrap();
    assert_eq!((rpc.begins, rpc.finishes), (1, 1));
    assert!(completed(&f.root).unwrap().is_some());
}

#[tokio::test]
async fn post_publish_pending_refresh_updates_only_the_same_new_subject() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let (snapshot, mut rpc) = server_committed_fixture(&f, &fresh).await;
    assert!(
        publish(&f.root, snapshot, || {
            if custody::load_session(&f.root)?.unwrap().token == fresh.token {
                return Err(Error::Unavailable("synthetic crash before receipt"));
            }
            Ok(())
        })
        .is_err()
    );
    let pending = Snapshot::load(&f.root).unwrap().unwrap();
    let mut rotated = fresh.clone();
    rotated.refresh_token = "private-new-session-rotated".into();
    let renewed = refresh_pending_with(
        &f.root,
        &pending.journal,
        true,
        async |current: &CloudSession| {
            assert_eq!(current.token, fresh.token);
            Ok(rotated.clone())
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(renewed.refresh_token, rotated.refresh_token);
    // Local publication still holds the earlier exact new token pair until
    // the same committed operation is reconciled under its fresh journal.
    assert_eq!(
        custody::load_session(&f.root)
            .unwrap()
            .unwrap()
            .refresh_token,
        fresh.refresh_token
    );
    commit_with(&f.root, Snapshot::load(&f.root).unwrap().unwrap(), &mut rpc)
        .await
        .unwrap();
    assert_eq!(
        custody::load_session(&f.root)
            .unwrap()
            .unwrap()
            .refresh_token,
        rotated.refresh_token
    );
    assert_eq!(rpc.finishes, 1);
}

#[tokio::test]
async fn cancellation_during_finish_releases_locks_and_keeps_recovery_intent() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let mut rpc = f.rpc(&fresh);
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let (_release, held) = tokio::sync::oneshot::channel();
    rpc.finish_wait = Some((entered.clone(), held));
    let mut work = Box::pin(complete_with(
        &f.root,
        prepare(&f.root).unwrap(),
        fresh,
        &mut rpc,
    ));
    tokio::select! {
        _ = entered.notified() => {}
        _ = &mut work => panic!("synthetic finish must remain in flight"),
    }
    drop(work);
    assert_eq!(rpc.finishes, 1);
    assert!(permit_refresh(&f.root).is_err());
    let pending = Snapshot::load(&f.root).unwrap().unwrap();
    commit_with(&f.root, pending, &mut rpc).await.unwrap();
    assert_eq!(rpc.finishes, 1);
}

#[tokio::test]
async fn pending_refresh_rejects_another_operation_and_new_subject_before_publication() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let (snapshot, _) = server_committed_fixture(&f, &fresh).await;
    let mut wrong = snapshot.journal.clone();
    wrong.operation = uuid::Uuid::now_v7().to_string();
    assert!(
        refresh_pending_with(&f.root, &wrong, true, async |_: &CloudSession| {
            panic!("a replacement operation must not spend a token")
        })
        .await
        .is_err()
    );
    let before = f.bytes(custody::name::REAUTH);
    assert!(
        refresh_pending_with(
            &f.root,
            &snapshot.journal,
            true,
            async |_: &CloudSession| { Ok(issued("another-session")) }
        )
        .await
        .is_err()
    );
    assert_eq!(f.bytes(custody::name::REAUTH), before);
    assert_eq!(
        custody::load_session(&f.root).unwrap().unwrap().token,
        f.session.token
    );
}

#[tokio::test]
async fn failed_attached_pending_token_adopts_a_siblings_newer_pair() {
    let f = Fixture::new();
    let fresh = issued("new-session");
    let (snapshot, _) = server_committed_fixture(&f, &fresh).await;
    let expected = snapshot.journal.clone();
    let mut newer = fresh.clone();
    newer.refresh_token = "private-sibling-rotated-pair".into();
    let mut journal = snapshot.journal.clone();
    journal.next_session = Some(newer.clone());
    snapshot.replace(&f.root, journal).unwrap();
    let adopted = refresh_pending_with(&f.root, &expected, true, async |_: &CloudSession| {
        panic!("a sibling's untried token must not be force-rotated")
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(adopted.refresh_token, newer.refresh_token);
}

#[test]
fn journal_privacy_and_custody_are_checked_before_reading_credentials() {
    let f = Fixture::new();
    prepare(&f.root).unwrap();
    let path = custody::path(&f.root, custody::name::REAUTH).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Snapshot::load(&f.root).is_err());
    assert!(relay_state(&f.root).is_err());
}

#[test]
fn reviewed_typescript_vector_matches_rust_canonicalization_and_verification() {
    let vector: Value = serde_json::from_str(include_str!("device-reauth.vector.json")).unwrap();
    let message: Challenge = serde_json::from_value(vector["message"].clone()).unwrap();
    assert_eq!(
        super::super::canonical::canonicalize(&message.value().unwrap()).unwrap(),
        vector["canonical"].as_str().unwrap()
    );
    let scalar =
        super::super::crypto::decode_base64url(vector["signingScalar"].as_str().unwrap(), 128)
            .unwrap();
    let signing = p256::ecdsa::SigningKey::from_slice(&scalar).unwrap();
    let signature =
        super::super::crypto::decode_base64url(vector["signature"].as_str().unwrap(), 128).unwrap();
    assert!(super::super::crypto::verify_canonical(
        signing.verifying_key(),
        &message.value().unwrap(),
        &signature
    ));
    let mut changed = message.value().unwrap();
    changed["authSessionId"] = json!("other-session");
    assert!(!super::super::crypto::verify_canonical(
        signing.verifying_key(),
        &changed,
        &signature
    ));
}
