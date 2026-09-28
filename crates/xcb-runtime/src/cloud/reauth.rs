//! Explicit, key-preserving relay sign-in renewal. Ordinary refresh keeps
//! its full subject binding; this separate protocol proves a new session
//! belongs to the same user and signs with the existing device key.
//!
//! The private journal is an intent, a recovery record, and the relay's
//! pause request. No lock survives an email/terminal wait. Once a signed
//! commit may have reached the server, old credentials cannot run again
//! until that exact operation is reconciled or a verified renewal wins.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{Error, Result, private};

use super::client::RelayClient;
use super::crypto::{encode_base64url, sign_canonical};
use super::custody::{self, CloudSession, SessionIdentity};
use super::{link, relay_gate, wire};

const MAX_JOURNAL: usize = 64 * 1024;
const TRANSITION_WAIT: Duration = Duration::from_secs(35);
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn changed() -> Error {
    Error::Conflict("relay sign-in state changed; credentials were not replaced")
}

fn protocol() -> Error {
    Error::Protocol("invalid relay reauthentication response")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn subject(identity: &SessionIdentity) -> Result<(&str, &str)> {
    let (user, session) = identity.subject.split_once('|').ok_or_else(changed)?;
    if !opaque(user) || !opaque(session) {
        return Err(changed());
    }
    Ok((user, session))
}

fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Persist both content and filesystem identity. An identical replacement
/// after clear/relink is still a different object, including after a crash.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FilePin {
    digest: String,
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanos: i64,
}

impl FilePin {
    fn read(path: &Path) -> Result<(Self, Zeroizing<Vec<u8>>)> {
        let file = private::open_file(path, MAX_JOURNAL as u64)?;
        let mut bytes = Zeroizing::new(Vec::new());
        (&file)
            .take(MAX_JOURNAL as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_JOURNAL {
            return Err(Error::PrivateState);
        }
        private::check_file(&file, MAX_JOURNAL as u64)?;
        private::same_file(path, &file)?;
        let meta = file.metadata()?;
        Ok((
            Self {
                digest: crate::digest(&bytes),
                device: meta.dev(),
                inode: meta.ino(),
                changed_seconds: meta.ctime(),
                changed_nanos: meta.ctime_nsec(),
            },
            bytes,
        ))
    }

    fn capture(root: &Path, name: &str) -> Result<Self> {
        Self::read(&custody::path(root, name)?).map(|(pin, _)| pin)
    }

    fn check(&self, root: &Path, name: &str) -> Result<()> {
        if &Self::capture(root, name)? != self {
            return Err(changed());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Original {
    root: PathBuf,
    endpoint: String,
    identity: SessionIdentity,
    device_id: String,
    device_class: String,
    key_version: u64,
    device: FilePin,
    account: FilePin,
}

impl Original {
    fn capture(root: &Path) -> Result<Self> {
        let device = custody::load_device(root)?.ok_or_else(changed)?;
        let (_, key_version) = custody::load_account_key(root)?.ok_or_else(changed)?;
        let session = custody::load_session(root)?.ok_or_else(changed)?;
        let relay = custody::load_link(root)?.ok_or_else(changed)?;
        let (device_pin, bytes) = FilePin::read(&custody::path(root, custody::name::DEVICE)?)?;
        let record: Value = serde_json::from_slice(&bytes).map_err(Error::Json)?;
        let device_class = record
            .get("deviceClass")
            .and_then(Value::as_str)
            .filter(|value| matches!(*value, wire::CONTROLLER_CLASS | wire::EXECUTOR_CLASS))
            .ok_or_else(changed)?
            .to_owned();
        let original = Self {
            root: root.to_owned(),
            endpoint: relay.deployment_url,
            identity: SessionIdentity::of(&session)?,
            device_id: device.device,
            device_class,
            key_version,
            device: device_pin,
            account: FilePin::capture(root, custody::name::ACCOUNT)?,
        };
        subject(&original.identity)?;
        original.check_keys(root)?;
        original.check_session(root, None)?;
        Ok(original)
    }

    fn check_keys(&self, root: &Path) -> Result<()> {
        if root != self.root {
            return Err(changed());
        }
        self.device.check(root, custody::name::DEVICE)?;
        self.account.check(root, custody::name::ACCOUNT)?;
        let relay = custody::load_link(root)?.ok_or_else(changed)?;
        if !custody::same_endpoint(&relay.deployment_url, &self.endpoint) {
            return Err(changed());
        }
        Ok(())
    }

    fn check_session(&self, root: &Path, next: Option<&SessionIdentity>) -> Result<()> {
        let session = custody::load_session(root)?.ok_or_else(changed)?;
        let identity = SessionIdentity::of(&session)?;
        if (identity != self.identity && Some(&identity) != next)
            || session
                .deployment_url
                .as_ref()
                .is_some_and(|url| !custody::same_endpoint(url, &self.endpoint))
        {
            return Err(changed());
        }
        Ok(())
    }

    fn check_new_identity(&self, identity: &SessionIdentity) -> Result<()> {
        if identity.issuer != self.identity.issuer
            || identity.audience != self.identity.audience
            || subject(identity)?.0 != subject(&self.identity)?.0
            || identity.subject == self.identity.subject
        {
            return Err(Error::Conflict(
                "sign in with the same relay account; credentials were not replaced",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Phase {
    AwaitingOtp,
    Prepared,
    Committing,
    ServerCommitted,
    RenewalRequired,
    Committed,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Journal {
    version: u32,
    operation: String,
    phase: Phase,
    original: Original,
    /// A superseded unresolved operation keeps the relay parked through OTP.
    prior_transition: bool,
    next_session: Option<CloudSession>,
    next_identity: Option<SessionIdentity>,
    publication: Option<FilePin>,
    challenge: Option<Challenge>,
    signature: Option<String>,
    result: Option<ReauthResult>,
}

impl Journal {
    fn blocks_relay(&self) -> bool {
        matches!(
            self.phase,
            Phase::Prepared | Phase::Committing | Phase::ServerCommitted | Phase::RenewalRequired
        ) || (self.phase == Phase::AwaitingOtp && self.prior_transition)
    }

    fn check(&self, root: &Path) -> Result<()> {
        if self.version != 1
            || uuid::Uuid::parse_str(&self.operation).is_err()
            || self.operation.len() > 36
        {
            return Err(Error::PrivateState);
        }
        self.original.check_keys(root)?;
        self.original
            .check_session(root, self.next_identity.as_ref())
    }
}

struct Snapshot {
    journal: Journal,
    pin: FilePin,
}

impl Snapshot {
    fn load(root: &Path) -> Result<Option<Self>> {
        let path = custody::path(root, custody::name::REAUTH)?;
        if private::open_file_maybe_vanished(&path, MAX_JOURNAL as u64)?.is_none() {
            return Ok(None);
        }
        let (pin, bytes) = FilePin::read(&path)?;
        let journal: Journal = serde_json::from_slice(&bytes).map_err(Error::Json)?;
        journal.check(root)?;
        Ok(Some(Self { journal, pin }))
    }

    fn check(&self, root: &Path) -> Result<()> {
        self.pin.check(root, custody::name::REAUTH)?;
        self.journal.check(root)
    }

    fn replace(&self, root: &Path, journal: Journal) -> Result<Self> {
        self.replace_checked(root, journal, || Ok(()))
    }

    fn replace_checked(
        &self,
        root: &Path,
        journal: Journal,
        check: impl Fn() -> Result<()>,
    ) -> Result<Self> {
        let mutation = custody::mutation_lock(root)?;
        let bytes = Zeroizing::new(serde_json::to_vec(&journal).map_err(Error::Json)?);
        if bytes.len() > MAX_JOURNAL {
            return Err(Error::PrivateState);
        }
        private::replace_guarded(
            &custody::path(root, custody::name::REAUTH)?,
            &bytes,
            &self.pin.digest,
            || {
                check()?;
                mutation.check()?;
                self.check(root)?;
                journal.check(root)
            },
        )?;
        Self::load(root)?.ok_or_else(changed)
    }
}

/// Opaque intent captured before the code request or terminal wait.
pub struct Intent(Snapshot);

impl Intent {
    pub fn endpoint(&self) -> &str {
        &self.0.journal.original.endpoint
    }
}

/// The relay host only resumes an auth-disabled worker for a validated
/// committed generation. A changed token file alone is never that signal.
pub(crate) enum RelayReauthState {
    Ready { generation: Option<String> },
    Pending,
}

pub(crate) fn relay_state(root: &Path) -> Result<RelayReauthState> {
    let Some(snapshot) = Snapshot::load(root)? else {
        return Ok(RelayReauthState::Ready { generation: None });
    };
    if snapshot.journal.blocks_relay() {
        return Ok(RelayReauthState::Pending);
    }
    if snapshot.journal.phase != Phase::Committed {
        return Ok(RelayReauthState::Ready { generation: None });
    }
    let journal = &snapshot.journal;
    let identity = journal.next_identity.as_ref().ok_or_else(changed)?;
    let current = custody::load_session(root)?.ok_or_else(changed)?;
    let result = journal.result.as_ref().ok_or_else(changed)?;
    journal.original.check_new_identity(identity)?;
    result.check_identity(&journal.original, identity)?;
    if SessionIdentity::of(&current)? != *identity || journal.next_session.is_some() {
        return Err(changed());
    }
    Ok(RelayReauthState::Ready {
        generation: Some(journal.operation.clone()),
    })
}

/// Called under the refresh ownership lock, before any old token is spent.
pub(super) fn permit_refresh(root: &Path) -> Result<()> {
    if matches!(relay_state(root)?, RelayReauthState::Pending) {
        return Err(Error::Conflict(
            "relay sign-in renewal is unfinished; run xcb link --reauth to continue",
        ));
    }
    Ok(())
}

/// Capture a new intent or resume an OTP wait. The original endpoint wins;
/// a caller must reject conflicting flags/environment before requesting OTP.
pub fn prepare(root: &Path) -> Result<Intent> {
    let mutation = custody::mutation_lock(root)?;
    let existing = Snapshot::load(root)?;
    if let Some(snapshot) = existing.as_ref()
        && snapshot.journal.phase == Phase::AwaitingOtp
    {
        snapshot.check(root)?;
        return Ok(Intent(existing.ok_or_else(changed)?));
    }
    if existing.as_ref().is_some_and(|snapshot| {
        !matches!(
            snapshot.journal.phase,
            Phase::Committed | Phase::RenewalRequired
        )
    }) {
        return Err(Error::Conflict(
            "relay sign-in renewal needs reconciliation before another code",
        ));
    }
    let original = Original::capture(root)?;
    let journal = Journal {
        version: 1,
        operation: uuid::Uuid::now_v7().to_string(),
        phase: Phase::AwaitingOtp,
        original,
        prior_transition: existing
            .as_ref()
            .is_some_and(|snapshot| snapshot.journal.phase == Phase::RenewalRequired),
        next_session: None,
        next_identity: None,
        publication: None,
        challenge: None,
        signature: None,
        result: None,
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&journal).map_err(Error::Json)?);
    let path = custody::path(root, custody::name::REAUTH)?;
    mutation.check()?;
    if let Some(snapshot) = existing {
        private::replace_guarded(&path, &bytes, &snapshot.pin.digest, || {
            mutation.check()?;
            snapshot.check(root)?;
            journal.check(root)
        })?;
    } else {
        journal.check(root)?;
        private::create(&path, &bytes)?;
    }
    Ok(Intent(Snapshot::load(root)?.ok_or_else(changed)?))
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Challenge {
    #[serde(deserialize_with = "wire::de::u64")]
    auth_epoch: u64,
    auth_session_id: String,
    #[serde(deserialize_with = "wire::de::u64")]
    binding_revision: u64,
    challenge_id: String,
    contract: String,
    device_class: String,
    device_id: String,
    #[serde(deserialize_with = "wire::de::u64")]
    expires_at: u64,
    #[serde(deserialize_with = "wire::de::u64")]
    key_version: u64,
    nonce: String,
    user_id: String,
}

impl Challenge {
    fn value(&self) -> Result<Value> {
        serde_json::to_value(self).map_err(Error::Json)
    }

    fn check(&self, original: &Original, identity: &SessionIdentity) -> Result<()> {
        let (user, session) = subject(identity)?;
        if self.contract != wire::DEVICE_REAUTH_CONTRACT
            || self.device_id != original.device_id
            || self.device_class != original.device_class
            || self.key_version != original.key_version
            || self.user_id != user
            || self.auth_session_id != session
            || !wire::is_device_id(&self.device_id)
            || self.auth_epoch == 0
            || self.auth_epoch > MAX_SAFE_INTEGER
            || self.key_version == 0
            || self.key_version > MAX_SAFE_INTEGER
            || self.binding_revision >= MAX_SAFE_INTEGER
            || self.expires_at == 0
            || self.expires_at > MAX_SAFE_INTEGER
            || self.challenge_id.len() != 32
            || super::crypto::decode_base64url(&self.challenge_id, 32)?.len() != 24
            || self.nonce.len() != 43
            || super::crypto::decode_base64url(&self.nonce, 43)?.len() != 32
        {
            return Err(protocol());
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReauthResult {
    #[serde(deserialize_with = "wire::de::u64")]
    auth_epoch: u64,
    auth_session_id: String,
    #[serde(deserialize_with = "wire::de::u64")]
    binding_revision: u64,
    challenge_id: String,
    device_class: String,
    device_id: String,
    #[serde(deserialize_with = "wire::de::u64")]
    key_version: u64,
    user_id: String,
}

impl ReauthResult {
    fn check_identity(&self, original: &Original, identity: &SessionIdentity) -> Result<()> {
        let (user, session) = subject(identity)?;
        if self.user_id != user
            || self.auth_session_id != session
            || self.device_id != original.device_id
            || self.device_class != original.device_class
            || self.key_version != original.key_version
            || self.auth_epoch == 0
            || self.auth_epoch > MAX_SAFE_INTEGER
            || self.binding_revision == 0
            || self.binding_revision > MAX_SAFE_INTEGER
            || self.challenge_id.len() != 32
            || super::crypto::decode_base64url(&self.challenge_id, 32)?.len() != 24
        {
            return Err(protocol());
        }
        Ok(())
    }

    fn check(&self, challenge: &Challenge) -> Result<()> {
        if self.auth_epoch != challenge.auth_epoch
            || self.auth_session_id != challenge.auth_session_id
            || self.binding_revision != challenge.binding_revision + 1
            || self.challenge_id != challenge.challenge_id
            || self.device_class != challenge.device_class
            || self.device_id != challenge.device_id
            || self.key_version != challenge.key_version
            || self.user_id != challenge.user_id
        {
            return Err(protocol());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
enum ReauthStatus {
    Committed { result: ReauthResult },
    Pending,
    Expired,
    Superseded,
    Unknown,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CurrentSubject {
    user_id: String,
    #[serde(deserialize_with = "wire::de::u64")]
    auth_epoch: u64,
    status: String,
    verified_at: Option<Value>,
}

trait ReauthRpc {
    async fn query(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value>;
    async fn mutation(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value>;
}

impl ReauthRpc for RelayClient {
    async fn query(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        RelayClient::query(self, path, args).await
    }

    async fn mutation(&mut self, path: &str, args: Vec<(&str, Value)>) -> Result<Value> {
        RelayClient::mutation(self, path, args).await
    }
}

async fn verified_owner(
    root: &Path,
    snapshot: &Snapshot,
    identity: &SessionIdentity,
    rpc: &mut impl ReauthRpc,
) -> Result<u64> {
    snapshot.check(root)?;
    let original = &snapshot.journal.original;
    original.check_new_identity(identity)?;
    let owner: CurrentSubject =
        serde_json::from_value(rpc.query("auth:currentSubject", vec![]).await?)
            .map_err(Error::Json)?;
    if owner.user_id != subject(identity)?.0
        || owner.status != "active"
        || owner.auth_epoch == 0
        || owner.auth_epoch > MAX_SAFE_INTEGER
        || owner.verified_at.as_ref().is_some_and(|at| {
            wire::json_u64(at).is_none_or(|value| value == 0 || value > MAX_SAFE_INTEGER)
        })
    {
        return Err(protocol());
    }
    let rows: Vec<wire::DeviceRow> =
        serde_json::from_value(rpc.query("relayDevices:list", vec![]).await?)
            .map_err(Error::Json)?;
    let matching: Vec<_> = rows
        .iter()
        .filter(|row| row.device_id == original.device_id)
        .collect();
    let [row] = matching.as_slice() else {
        return Err(Error::Conflict(
            "this sign-in does not own the existing linked device",
        ));
    };
    let device = custody::load_device(root)?.ok_or_else(changed)?;
    if row.status != "active"
        || row.device_class != original.device_class
        || row.key_version != original.key_version
        || row.signing_public_key != encode_base64url(&device.public.verify_key_spki)
        || row.agreement_public_key != encode_base64url(&device.public.agreement_key_spki)
    {
        return Err(Error::Conflict(
            "the relay device no longer matches this machine; credentials were not replaced",
        ));
    }
    snapshot.check(root)?;
    Ok(owner.auth_epoch)
}

/// Public output contains no tokens, signatures, or account key material.
pub struct ReauthOutcome {
    pub device: String,
    pub device_class: String,
    pub generation: String,
}

/// A bare retry can report a completed, still-current renewal after stdout
/// or the process was interrupted. Supplying an email requests a new OTP;
/// a due/expired session also proceeds to a new explicit renewal.
pub fn completed(root: &Path) -> Result<Option<ReauthOutcome>> {
    let Some(snapshot) = Snapshot::load(root)? else {
        return Ok(None);
    };
    if snapshot.journal.phase != Phase::Committed {
        return Ok(None);
    }
    if !matches!(
        relay_state(root)?,
        RelayReauthState::Ready {
            generation: Some(_)
        }
    ) {
        return Err(changed());
    }
    if custody::load_session(root)?
        .ok_or_else(changed)?
        .due_for_refresh(now_ms())
    {
        return Ok(None);
    }
    Ok(Some(ReauthOutcome {
        device: snapshot.journal.original.device_id,
        device_class: snapshot.journal.original.device_class,
        generation: snapshot.journal.operation,
    }))
}

/// `client` must be the separate OTP client, never an existing authenticated
/// controller/worker. The new credentials remain private until server proof
/// and exact local publication both succeed.
pub async fn complete(
    root: &Path,
    intent: Intent,
    session: CloudSession,
    client: &mut RelayClient,
) -> Result<ReauthOutcome> {
    if !custody::same_endpoint(&client.deployment_url, intent.endpoint())
        || session
            .deployment_url
            .as_ref()
            .is_none_or(|url| !custody::same_endpoint(url, intent.endpoint()))
    {
        return Err(changed());
    }
    intent.0.check(root)?;
    intent
        .0
        .journal
        .original
        .check_new_identity(&SessionIdentity::of(&session)?)?;
    client.authenticate(&session.token).await;
    complete_with(root, intent, session, client).await
}

async fn complete_with(
    root: &Path,
    intent: Intent,
    session: CloudSession,
    rpc: &mut impl ReauthRpc,
) -> Result<ReauthOutcome> {
    let snapshot = intent.0;
    if snapshot.journal.phase != Phase::AwaitingOtp
        || session
            .deployment_url
            .as_ref()
            .is_none_or(|url| !custody::same_endpoint(url, &snapshot.journal.original.endpoint))
    {
        return Err(changed());
    }
    let identity = SessionIdentity::of(&session)?;
    verified_owner(root, &snapshot, &identity, rpc).await?;
    let mut journal = snapshot.journal.clone();
    journal.phase = Phase::Prepared;
    journal.next_session = Some(session);
    journal.next_identity = Some(identity);
    let prepared = snapshot.replace(root, journal)?;
    commit_with(root, prepared, rpc).await
}

async fn commit_with(
    root: &Path,
    mut snapshot: Snapshot,
    rpc: &mut impl ReauthRpc,
) -> Result<ReauthOutcome> {
    let transition = relay_gate::transition(root, TRANSITION_WAIT).await?;
    let refresh = custody::session_refresh_lock(root, TRANSITION_WAIT).await?;
    let check = || {
        transition.check()?;
        refresh.check()
    };
    check()?;
    snapshot.check(root)?;
    let identity = snapshot.journal.next_identity.clone().ok_or_else(changed)?;
    let epoch = verified_owner(root, &snapshot, &identity, rpc).await?;
    if snapshot.journal.phase == Phase::Prepared {
        snapshot.journal.original.check_session(root, None)?;
        let publication = FilePin::capture(root, custody::name::SESSION)?;
        let challenge: Challenge = serde_json::from_value(
            rpc.mutation(
                "relayDevices:beginReauth",
                vec![("deviceId", json!(snapshot.journal.original.device_id))],
            )
            .await?,
        )
        .map_err(Error::Json)?;
        challenge.check(&snapshot.journal.original, &identity)?;
        if challenge.auth_epoch != epoch
            || challenge.expires_at <= now_ms()
            || challenge.expires_at > now_ms().saturating_add(330_000)
        {
            return Err(protocol());
        }
        check()?;
        publication.check(root, custody::name::SESSION)?;
        let device = custody::load_device(root)?.ok_or_else(changed)?;
        let signature = encode_base64url(&sign_canonical(&device.signing, &challenge.value()?)?);
        let mut journal = snapshot.journal.clone();
        journal.phase = Phase::Committing;
        journal.publication = Some(publication);
        journal.challenge = Some(challenge);
        journal.signature = Some(signature);
        // The signed operation and new token pair are durable BEFORE finish.
        snapshot = snapshot.replace_checked(root, journal, check)?;
    }
    if !matches!(
        snapshot.journal.phase,
        Phase::Committing | Phase::ServerCommitted
    ) {
        return Err(changed());
    }
    let challenge = snapshot.journal.challenge.clone().ok_or_else(changed)?;
    challenge.check(&snapshot.journal.original, &identity)?;
    if challenge.auth_epoch != epoch {
        return Err(changed());
    }
    let signature = snapshot.journal.signature.as_ref().ok_or_else(changed)?;
    check_publication(root, &snapshot)?;
    let device = custody::load_device(root)?.ok_or_else(changed)?;
    if !super::crypto::verify_canonical(
        device.signing.verifying_key(),
        &challenge.value()?,
        &super::crypto::decode_base64url(signature, 128)?,
    ) {
        return Err(protocol());
    }
    let status: ReauthStatus = serde_json::from_value(
        rpc.query(
            "relayDevices:reauthStatus",
            vec![
                ("challengeId", json!(challenge.challenge_id)),
                ("deviceId", json!(challenge.device_id)),
            ],
        )
        .await?,
    )
    .map_err(Error::Json)?;
    let result = match status {
        ReauthStatus::Committed { result } => result,
        ReauthStatus::Pending => {
            check()?;
            snapshot.check(root)?;
            check_publication(root, &snapshot)?;
            serde_json::from_value(
                rpc.mutation(
                    "relayDevices:finishReauth",
                    vec![
                        ("challengeId", json!(challenge.challenge_id)),
                        ("deviceId", json!(challenge.device_id)),
                        ("signature", json!(signature)),
                    ],
                )
                .await?,
            )
            .map_err(Error::Json)?
        }
        ReauthStatus::Expired | ReauthStatus::Superseded | ReauthStatus::Unknown => {
            // A new explicit invocation may sign a fresh challenge with these
            // still-verified credentials and the current server revision.
            let mut journal = snapshot.journal.clone();
            journal.phase = Phase::Prepared;
            journal.prior_transition = true;
            journal.challenge = None;
            journal.signature = None;
            journal.result = None;
            journal.publication = None;
            snapshot.replace_checked(root, journal, check)?;
            return Err(Error::Conflict(
                "relay sign-in renewal changed; rerun xcb link --reauth to continue",
            ));
        }
    };
    result.check(&challenge)?;
    result.check_identity(&snapshot.journal.original, &identity)?;
    check()?;
    let mut journal = snapshot.journal.clone();
    journal.phase = Phase::ServerCommitted;
    journal.result = Some(result);
    snapshot = snapshot.replace_checked(root, journal, check)?;
    publish(root, snapshot, check)
}

fn session_bytes(session: &CloudSession) -> Result<Zeroizing<Vec<u8>>> {
    let mut value = serde_json::to_value(session).map_err(Error::Json)?;
    value["revision"] = json!(0);
    Ok(Zeroizing::new(
        serde_json::to_vec(&value).map_err(Error::Json)?,
    ))
}

fn check_publication(root: &Path, snapshot: &Snapshot) -> Result<()> {
    if snapshot.journal.phase == Phase::ServerCommitted {
        let next = snapshot.journal.next_session.as_ref().ok_or_else(changed)?;
        if FilePin::capture(root, custody::name::SESSION)?.digest
            == crate::digest(&session_bytes(next)?)
        {
            return Ok(());
        }
    }
    snapshot
        .journal
        .publication
        .as_ref()
        .ok_or_else(changed)?
        .check(root, custody::name::SESSION)
}

fn publish(
    root: &Path,
    snapshot: Snapshot,
    check: impl Fn() -> Result<()>,
) -> Result<ReauthOutcome> {
    let journal = &snapshot.journal;
    let session = journal.next_session.as_ref().ok_or_else(changed)?;
    let publication = journal.publication.as_ref().ok_or_else(changed)?;
    let next_bytes = session_bytes(session)?;
    let current = FilePin::capture(root, custody::name::SESSION)?;
    if current.digest != crate::digest(&next_bytes) {
        publication.check(root, custody::name::SESSION)?;
        let original = custody::session_snapshot(root)?;
        custody::replace_session(root, &original, session, || {
            check()?;
            snapshot.check(root)?;
            publication.check(root, custody::name::SESSION)
        })?;
    }
    // Crash after session rename is recognizable by the exact intended
    // bytes. Clearing/relinking cancels or changes the source journal, so
    // this recovery can never create a missing session or a new intention.
    let mut committed = journal.clone();
    committed.phase = Phase::Committed;
    committed.next_session = None;
    committed.publication = None;
    committed.challenge = None;
    committed.signature = None;
    let committed = snapshot.replace_checked(root, committed, || {
        check()?;
        if FilePin::capture(root, custody::name::SESSION)?.digest != crate::digest(&next_bytes) {
            return Err(changed());
        }
        Ok(())
    })?;
    if !matches!(
        relay_state(root)?,
        RelayReauthState::Ready {
            generation: Some(_)
        }
    ) {
        return Err(changed());
    }
    Ok(ReauthOutcome {
        device: committed.journal.original.device_id,
        device_class: committed.journal.original.device_class,
        generation: committed.journal.operation,
    })
}

fn expired_auth(error: &Error) -> bool {
    matches!(
        error,
        Error::Protocol("auth refresh rejected" | "relay unauthenticated")
    )
}

/// Rotate only the journal's new session, never the old session.json token.
/// It keeps the exact new subject; a seven-day session needs another OTP.
fn same_pending(expected: &Journal, current: &Journal) -> Result<()> {
    if current.operation != expected.operation
        || current.original != expected.original
        || current.next_identity != expected.next_identity
        || !matches!(
            current.phase,
            Phase::Prepared | Phase::Committing | Phase::ServerCommitted
        )
    {
        return Err(changed());
    }
    Ok(())
}

async fn refresh_pending_with<F>(
    root: &Path,
    expected: &Journal,
    force: bool,
    renew: F,
) -> Result<Option<CloudSession>>
where
    F: AsyncFnOnce(&CloudSession) -> Result<CloudSession>,
{
    let transition = relay_gate::transition(root, TRANSITION_WAIT).await?;
    let refresh = custody::session_refresh_lock(root, TRANSITION_WAIT).await?;
    let snapshot = Snapshot::load(root)?.ok_or_else(changed)?;
    same_pending(expected, &snapshot.journal)?;
    let current = snapshot.journal.next_session.as_ref().ok_or_else(changed)?;
    let previous = expected.next_session.as_ref().ok_or_else(changed)?;
    let replaced =
        current.token != previous.token || current.refresh_token != previous.refresh_token;
    if (!force || replaced) && !current.due_for_refresh(now_ms()) {
        return Ok(Some(current.clone()));
    }
    if force && replaced {
        return Err(Error::Conflict(
            "replacement relay renewal needs refresh; retry the operation",
        ));
    }
    let identity = snapshot
        .journal
        .next_identity
        .as_ref()
        .ok_or_else(changed)?;
    let published = if snapshot.journal.phase == Phase::ServerCommitted
        && FilePin::capture(root, custody::name::SESSION)?.digest
            == crate::digest(&session_bytes(current)?)
    {
        Some(FilePin::capture(root, custody::name::SESSION)?)
    } else {
        None
    };
    let check = || {
        transition.check()?;
        refresh.check()
    };
    check()?;
    snapshot.check(root)?;
    match renew(current).await {
        Ok(fresh) => {
            if SessionIdentity::of(&fresh)? != *identity
                || fresh.deployment_url.as_ref().is_none_or(|url| {
                    !custody::same_endpoint(url, &snapshot.journal.original.endpoint)
                })
            {
                return Err(changed());
            }
            let mut journal = snapshot.journal.clone();
            journal.next_session = Some(fresh.clone());
            if let Some(pin) = published.as_ref() {
                journal.publication = Some(pin.clone());
            }
            snapshot.replace_checked(root, journal, || {
                check()?;
                if let Some(pin) = published.as_ref() {
                    pin.check(root, custody::name::SESSION)?;
                }
                Ok(())
            })?;
            Ok(Some(fresh))
        }
        Err(error) if expired_auth(&error) => {
            let mut journal = snapshot.journal.clone();
            journal.phase = Phase::RenewalRequired;
            journal.prior_transition = true;
            snapshot.replace_checked(root, journal, check)?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn refresh_pending(
    root: &Path,
    expected: &Journal,
    client: &mut RelayClient,
    force: bool,
) -> Result<Option<CloudSession>> {
    if !custody::same_endpoint(&client.deployment_url, &expected.original.endpoint) {
        return Err(changed());
    }
    let fresh = refresh_pending_with(root, expected, force, async |current: &CloudSession| {
        client.clear_auth().await;
        link::refresh_session(client, current).await
    })
    .await?;
    if let Some(fresh) = fresh.as_ref() {
        client.authenticate(&fresh.token).await;
    }
    Ok(fresh)
}

#[cfg(test)]
#[path = "reauth_tests.rs"]
mod tests;

/// Reconcile the same signed operation before another OTP or old refresh.
/// None means an OTP is needed (including a journal session's total expiry).
pub async fn resume(root: &Path) -> Result<Option<ReauthOutcome>> {
    let Some(snapshot) = Snapshot::load(root)? else {
        return Ok(None);
    };
    if matches!(
        snapshot.journal.phase,
        Phase::AwaitingOtp | Phase::RenewalRequired | Phase::Committed
    ) {
        return Ok(None);
    }
    let session = snapshot.journal.next_session.as_ref().ok_or_else(changed)?;
    let expected = snapshot.journal.clone();
    let mut client = RelayClient::connect(&snapshot.journal.original.endpoint).await?;
    let attached = if session.due_for_refresh(now_ms()) {
        let Some(fresh) = refresh_pending(root, &expected, &mut client, false).await? else {
            return Ok(None);
        };
        fresh
    } else {
        let current = Snapshot::load(root)?.ok_or_else(changed)?;
        same_pending(&expected, &current.journal)?;
        let current = current.journal.next_session.ok_or_else(changed)?;
        client.authenticate(&current.token).await;
        current
    };
    let snapshot = Snapshot::load(root)?.ok_or_else(changed)?;
    same_pending(&expected, &snapshot.journal)?;
    let mut attempt = snapshot.journal.clone();
    // Forced recovery belongs to the token actually attached to this
    // connection. A sibling's newer pair was not the one that failed.
    attempt.next_session = Some(attached);
    match commit_with(root, snapshot, &mut client).await {
        Ok(outcome) => Ok(Some(outcome)),
        Err(error) if expired_auth(&error) => {
            if refresh_pending(root, &attempt, &mut client, true)
                .await?
                .is_none()
            {
                return Ok(None);
            }
            let snapshot = Snapshot::load(root)?.ok_or_else(changed)?;
            same_pending(&attempt, &snapshot.journal)?;
            commit_with(root, snapshot, &mut client).await.map(Some)
        }
        Err(error) => Err(error),
    }
}
