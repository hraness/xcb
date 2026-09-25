//! Local custody for cloud state: `<state-root>/cloud/` held at 0700 with
//! every file at 0600, written atomically through `private::create` /
//! `private::replace`. Never a world-readable byte.
//!
//! Layout:
//! - `device.json` — the enrolled device identity (signing + agreement
//!   scalars, public keys, device id, bound label/class).
//! - `account.json` — the account AES key and its key version.
//! - `session.json` — the Convex Auth session token pair.
//! - `relay.json` — deployment URL, auth epoch view, boot generation.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::{Error, Result, private};

use super::crypto::{AccountKey, DeviceIdentity};

/// Maximum bytes any custody file may hold.
const MAX_FILE_BYTES: usize = 64 * 1024;

/// The pinned file names — this module owns them all.
mod name {
    pub const DEVICE: &str = "device.json";
    pub const ACCOUNT: &str = "account.json";
    pub const SESSION: &str = "session.json";
    pub const RELAY: &str = "relay.json";
}

/// The cloud custody directory, created (or verified) private on access.
pub fn cloud_dir(state_root: &Path) -> Result<PathBuf> {
    private::directory(&state_root.join("cloud"))
}

fn path(state_root: &Path, file: &str) -> Result<PathBuf> {
    Ok(cloud_dir(state_root)?.join(file))
}

fn write(state_root: &Path, file: &str, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(Error::Json)?;
    let target = path(state_root, file)?;
    if target.exists() {
        // Digest-pinned replace is stronger than blind overwrite; custody
        // records are small enough that the read is negligible.
        let current = private::read(&target, MAX_FILE_BYTES)?;
        private::replace(&target, &bytes, &crate::digest(&current))
    } else {
        private::create(&target, &bytes)
    }
}

fn read(state_root: &Path, file: &str) -> Result<Option<Value>> {
    let target = path(state_root, file)?;
    match private::open_file_maybe_vanished(&target, MAX_FILE_BYTES as u64)? {
        None => Ok(None),
        Some(_) => {
            let bytes = private::read(&target, MAX_FILE_BYTES)?;
            Ok(Some(serde_json::from_slice(&bytes).map_err(Error::Json)?))
        }
    }
}

// Device identity -------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceRecord {
    agreement_scalar: String,
    device: String,
    device_class: String,
    label: String,
    public_id: String,
    revision: u64,
    signing_scalar: String,
}

/// Persist a freshly enrolled device identity.
pub fn store_device(
    state_root: &Path,
    device: &DeviceIdentity,
    device_class: &str,
    label: &str,
    public_id: &str,
) -> Result<()> {
    let record = DeviceRecord {
        agreement_scalar: super::crypto::encode_base64url(&device.agreement.to_bytes()),
        device: device.device.clone(),
        device_class: device_class.to_string(),
        label: label.to_string(),
        public_id: public_id.to_string(),
        revision: 0,
        signing_scalar: super::crypto::encode_base64url(&device.signing.to_bytes()),
    };
    write(
        state_root,
        name::DEVICE,
        &serde_json::to_value(record).map_err(Error::Json)?,
    )
}

/// Load the enrolled device identity, or `None` when the machine has never
/// linked.
pub fn load_device(state_root: &Path) -> Result<Option<DeviceIdentity>> {
    let Some(value) = read(state_root, name::DEVICE)? else {
        return Ok(None);
    };
    let record: DeviceRecord = serde_json::from_value(value).map_err(Error::Json)?;
    let signing_bytes = super::crypto::decode_base64url(&record.signing_scalar, 128)?;
    let agreement_bytes = super::crypto::decode_base64url(&record.agreement_scalar, 128)?;
    let signing = p256::ecdsa::SigningKey::from_slice(&signing_bytes)
        .map_err(|_| Error::from(xcb_core::Error::Invalid("device signing key")))?;
    let agreement = p256::SecretKey::from_slice(&agreement_bytes)
        .map_err(|_| Error::from(xcb_core::Error::Invalid("device agreement key")))?;
    let identity = DeviceIdentity::from_scalars(signing, agreement)?;
    if identity.device != record.device {
        return Err(Error::Conflict("device id does not match its stored keys"));
    }
    Ok(Some(identity))
}

// Account key -------------------------------------------------------------------------

/// Persist the account key and its version.
pub fn store_account_key(state_root: &Path, key: &AccountKey, key_version: u64) -> Result<()> {
    write(
        state_root,
        name::ACCOUNT,
        &json!({
            "accountKey": super::crypto::encode_base64url(&key.0[..]),
            "keyVersion": key_version,
            "revision": 0u64,
        }),
    )
}

pub fn load_account_key(state_root: &Path) -> Result<Option<(AccountKey, u64)>> {
    let Some(value) = read(state_root, name::ACCOUNT)? else {
        return Ok(None);
    };
    let raw = value
        .get("accountKey")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::from(xcb_core::Error::Invalid("account key record")))?;
    let key_version = value
        .get("keyVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::from(xcb_core::Error::Invalid("account key version")))?;
    let bytes = super::crypto::decode_base64url(raw, 64)?;
    if bytes.len() != super::crypto::ACCOUNT_KEY_BYTES {
        return Err(Error::from(xcb_core::Error::Invalid("account key length")));
    }
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(&bytes);
    Ok(Some((AccountKey(key), key_version)))
}

// Auth session ---------------------------------------------------------------------------

/// The Convex Auth token pair the device presents on authenticated calls.
/// `token_expiry_ms` is decoded from the JWT payload at issue time; when
/// the claim is unreadable a conservative assumed lifetime applies.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSession {
    pub token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub token_expiry_ms: Option<u64>,
}

/// Refresh leads expiry by this much — a token inside the window is
/// already spent as far as the lane is concerned.
pub const REFRESH_LEAD_MS: u64 = 120_000;
/// Assumed token lifetime when the JWT carries no readable `exp`.
const ASSUMED_TOKEN_TTL_MS: u64 = 30 * 60 * 1000;

/// Decode the JWT `exp` claim (seconds) from `token` without verifying —
/// this schedules refresh, it does not authenticate anything. An
/// undecodable token gets `now + ASSUMED_TOKEN_TTL_MS` so refresh still
/// happens on a bounded horizon.
pub fn session_expiry_ms(token: &str, now_ms: u64) -> u64 {
    let Some(payload) = token.split('.').nth(1) else {
        return now_ms + ASSUMED_TOKEN_TTL_MS;
    };
    let Ok(raw) = super::crypto::decode_base64url(payload, 8 * 1024) else {
        return now_ms + ASSUMED_TOKEN_TTL_MS;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return now_ms + ASSUMED_TOKEN_TTL_MS;
    };
    value
        .get("exp")
        .and_then(super::wire::json_u64)
        .map(|seconds| seconds.saturating_mul(1000))
        .unwrap_or(now_ms + ASSUMED_TOKEN_TTL_MS)
}

impl CloudSession {
    /// Build a session from an issued token pair, decoding expiry.
    pub fn issue(token: String, refresh_token: String, now_ms: u64) -> Self {
        Self {
            token_expiry_ms: Some(session_expiry_ms(&token, now_ms)),
            token,
            refresh_token,
        }
    }

    /// True when the token expires inside the refresh lead (or already
    /// did, or its expiry was never decodable).
    pub fn due_for_refresh(&self, now_ms: u64) -> bool {
        match self.token_expiry_ms {
            Some(expiry) => expiry <= now_ms + REFRESH_LEAD_MS,
            None => true,
        }
    }
}

pub fn store_session(state_root: &Path, session: &CloudSession) -> Result<()> {
    write(
        state_root,
        name::SESSION,
        &json!({
            "token": session.token,
            "refreshToken": session.refresh_token,
            "tokenExpiryMs": session.token_expiry_ms,
            "revision": 0u64,
        }),
    )
}

pub fn load_session(state_root: &Path) -> Result<Option<CloudSession>> {
    let Some(value) = read(state_root, name::SESSION)? else {
        return Ok(None);
    };
    serde_json::from_value(value).map(Some).map_err(Error::Json)
}

pub fn clear_session(state_root: &Path) -> Result<()> {
    let target = path(state_root, name::SESSION)?;
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    Ok(())
}

// Relay configuration ---------------------------------------------------------------------

/// Persisted relay linkage: the deployment URL this machine enrolled against
/// and the last-seen boot generation.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayLink {
    pub deployment_url: String,
    pub boot_generation: u64,
}

pub fn store_link(state_root: &Path, link: &RelayLink) -> Result<()> {
    write(
        state_root,
        name::RELAY,
        &json!({
            "bootGeneration": link.boot_generation,
            "deploymentUrl": link.deployment_url,
            "revision": 0u64,
        }),
    )
}

pub fn load_link(state_root: &Path) -> Result<Option<RelayLink>> {
    let Some(value) = read(state_root, name::RELAY)? else {
        return Ok(None);
    };
    serde_json::from_value(value).map(Some).map_err(Error::Json)
}
