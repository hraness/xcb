//! `xcb link` — enroll this machine as a relay device. Terminal email OTP
//! through `auth:signIn`, then register → beginBind → finishBind under the
//! `xcb.relay.v1:device-bind` contract, then key custody: the first device
//! mints the account key; later devices open a key wrap an enrolled device
//! posted for them.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::{Error, Result};

use super::client::RelayClient;
use super::crypto::{
    AccountKey, DeviceIdentity, PeerDevice, encode_base64url, open_key_wrap, seal_key_wrap,
    sign_canonical,
};
use super::custody::{self, CloudSession, RelayLink};
use super::wire;

fn protocol(what: &'static str) -> Error {
    Error::Protocol(what)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

const AUTH_PROVIDER: &str = "xcb-otp-v1";

/// Phase 1: request an OTP code for `email`. `invite` is the bootstrap or
/// issued identity-invite token when closed sign-up gates first admission.
/// Returns true when a code was requested; the relay also returns true when
/// the request was refused silently (anti-enumeration).
pub async fn request_code(
    client: &mut RelayClient,
    email: &str,
    invite: Option<&str>,
) -> Result<bool> {
    let mut params = json!({
        "email": email,
        "flow": "request_code",
    });
    if let Some(token) = invite {
        params["invite"] = json!(token);
    }
    let result = client
        .action(
            "auth:signIn",
            vec![
                ("provider", Value::String(AUTH_PROVIDER.to_string())),
                ("params", params),
            ],
        )
        .await?;
    // `{tokens: null}` means code requested (or silently rejected).
    Ok(result.get("tokens").is_some_and(Value::is_null))
}

/// Phase 2: verify the emailed code. On success returns the session token
/// pair — the caller stores it in custody.
pub async fn verify_code(
    client: &mut RelayClient,
    email: &str,
    code: &str,
) -> Result<CloudSession> {
    let result = client
        .action(
            "auth:signIn",
            vec![
                ("provider", Value::String(AUTH_PROVIDER.to_string())),
                (
                    "params",
                    json!({
                        "code": code,
                        "email": email,
                        "flow": "verify_code",
                    }),
                ),
            ],
        )
        .await?;
    let tokens = result
        .get("tokens")
        .filter(|tokens| !tokens.is_null())
        .ok_or(protocol("auth:signIn rejected the code"))?;
    let token = tokens
        .get("token")
        .and_then(Value::as_str)
        .ok_or(protocol("auth:signIn missing token"))?;
    let refresh_token = tokens
        .get("refreshToken")
        .and_then(Value::as_str)
        .ok_or(protocol("auth:signIn missing refreshToken"))?;
    Ok(CloudSession::issue(
        token.to_string(),
        refresh_token.to_string(),
        now_ms(),
    ))
}

/// Refresh an expired session token via the auth:signIn refresh lane.
pub async fn refresh_session(
    client: &mut RelayClient,
    session: &CloudSession,
) -> Result<CloudSession> {
    let result = client
        .action(
            "auth:signIn",
            vec![("refreshToken", Value::String(session.refresh_token.clone()))],
        )
        .await?;
    let tokens = result
        .get("tokens")
        .filter(|tokens| !tokens.is_null())
        .ok_or(protocol("auth refresh rejected"))?;
    let token = tokens
        .get("token")
        .and_then(Value::as_str)
        .ok_or(protocol("auth refresh missing token"))?;
    let refresh_token = tokens
        .get("refreshToken")
        .and_then(Value::as_str)
        .unwrap_or(&session.refresh_token)
        .to_string();
    Ok(CloudSession::issue(
        token.to_string(),
        refresh_token,
        now_ms(),
    ))
}

/// Refresh `session` when it is inside the expiry lead, persisting and
/// re-authenticating the client in place. A no-op for a fresh token.
pub async fn refresh_if_due(
    client: &mut RelayClient,
    state_root: &Path,
    session: &mut CloudSession,
) -> Result<()> {
    if !session.due_for_refresh(now_ms()) {
        return Ok(());
    }
    let fresh = refresh_session(client, session).await?;
    custody::store_session(state_root, &fresh)?;
    client.authenticate(&fresh.token).await;
    *session = fresh;
    Ok(())
}

/// Register a device identity on the authenticated session. Idempotent for
/// a pending row with matching keys; a row in another state rejects.
async fn register_device(
    client: &mut RelayClient,
    device: &DeviceIdentity,
    device_class: &str,
    label: &str,
) -> Result<()> {
    let registered = client
        .mutation(
            "relayDevices:register",
            vec![
                (
                    "agreementPublicKey",
                    json!(encode_base64url(&device.public.agreement_key_spki)),
                ),
                ("deviceClass", json!(device_class)),
                ("deviceId", json!(device.device)),
                ("label", json!(label)),
                (
                    "signingPublicKey",
                    json!(encode_base64url(&device.public.verify_key_spki)),
                ),
            ],
        )
        .await?;
    if registered.get("deviceId").and_then(Value::as_str) != Some(device.device.as_str()) {
        return Err(protocol("register returned the wrong deviceId"));
    }
    Ok(())
}

/// Bind a pending device to this auth session: sign the server's bind
/// challenge with the device signing key.
async fn bind_device(client: &mut RelayClient, device: &DeviceIdentity) -> Result<()> {
    let begin = client
        .mutation(
            "relayDevices:beginBind",
            vec![("deviceId", json!(device.device))],
        )
        .await?;
    let challenge_id = begin
        .get("challengeId")
        .and_then(Value::as_str)
        .ok_or(protocol("beginBind missing challengeId"))?;
    let nonce = begin
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or(protocol("beginBind missing nonce"))?;
    let signature = sign_canonical(
        &device.signing,
        &json!({
            "challengeId": challenge_id,
            "contract": wire::DEVICE_BIND_CONTRACT,
            "nonce": nonce,
        }),
    )?;
    client
        .mutation(
            "relayDevices:finishBind",
            vec![
                ("challengeId", json!(challenge_id)),
                ("deviceId", json!(device.device)),
                ("signature", json!(encode_base64url(&signature))),
            ],
        )
        .await?;
    Ok(())
}

/// Register + bind a fresh device identity on the authenticated session.
/// The device id (derived from the signing key) is the public id.
pub async fn enroll_device(
    client: &mut RelayClient,
    device: &DeviceIdentity,
    device_class: &str,
    label: &str,
) -> Result<String> {
    register_device(client, device, device_class, label).await?;
    bind_device(client, device).await?;
    Ok(device.device.clone())
}

/// Fetch this account's device rows — active and pending — as the typed
/// wire shape.
pub async fn device_rows(client: &mut RelayClient) -> Result<Vec<wire::DeviceRow>> {
    let rows = client.query("relayDevices:list", vec![]).await?;
    let list = rows.as_array().ok_or(protocol("devices list shape"))?;
    let mut out = Vec::with_capacity(list.len());
    for row in list {
        out.push(serde_json::from_value(row.clone()).map_err(Error::Json)?);
    }
    Ok(out)
}

/// Fetch this account's enrolled devices as `deviceId → PeerDevice` — the
/// public keys needed to verify envelopes and open key wraps. Every row
/// parses through the typed wire shape before its keys are imported.
pub async fn peers(client: &mut RelayClient) -> Result<BTreeMap<String, PeerDevice>> {
    let mut out = BTreeMap::new();
    for device in device_rows(client).await? {
        out.insert(
            device.device_id.clone(),
            PeerDevice {
                device: device.device_id,
                verify_key_spki: super::crypto::decode_base64url(&device.signing_public_key, 128)?,
                agreement_key_spki: super::crypto::decode_base64url(
                    &device.agreement_public_key,
                    128,
                )?,
            },
        );
    }
    Ok(out)
}

/// Post a key wrap so `recipient` can join the account. `sender` must be an
/// enrolled device holding the account key.
pub async fn admit_device(
    client: &mut RelayClient,
    sender: &DeviceIdentity,
    recipient: &PeerDevice,
    account_key: &AccountKey,
    key_version: u64,
) -> Result<()> {
    let envelope = seal_key_wrap(sender, recipient, account_key, key_version)?;
    client
        .mutation(
            "relayEnvelopes:postKeyEnvelope",
            vec![("envelope", envelope)],
        )
        .await?;
    Ok(())
}

/// Collect and open the first key wrap addressed to this device. The relay
/// returns bare envelopes; the sender's public keys come from the device
/// list. Returns `None` when no wrap has been posted yet.
pub async fn collect_key_wrap(
    client: &mut RelayClient,
    device: &DeviceIdentity,
) -> Result<Option<(AccountKey, u64)>> {
    let envelopes = client
        .query("relayEnvelopes:myKeyEnvelopes", vec![])
        .await?;
    let list = envelopes
        .as_array()
        .ok_or(protocol("myKeyEnvelopes shape"))?;
    if list.is_empty() {
        return Ok(None);
    }
    let peers = peers(client).await?;
    for envelope in list {
        let sender_id = envelope
            .get("sender")
            .and_then(Value::as_str)
            .ok_or(protocol("key wrap missing sender"))?;
        let sender = peers
            .get(sender_id)
            .ok_or(protocol("key wrap from unknown sender"))?;
        if let Ok(key) = open_key_wrap(envelope, device, sender) {
            let version = envelope
                .get("keyVersion")
                .and_then(wire::json_u64)
                .unwrap_or(1);
            return Ok(Some((key, version)));
        }
    }
    Ok(None)
}

/// Which account-key path the link settled through.
#[derive(Debug)]
pub enum KeyOutcome {
    /// This device minted the first account key.
    Minted,
    /// The device opened a posted key wrap.
    Adopted { key_version: u64 },
    /// A controller device still needs an enrolled device to wrap for it.
    AwaitingWrap,
}

pub struct LinkOutcome {
    pub device: DeviceIdentity,
    pub key_outcome: KeyOutcome,
    /// The device id the relay enrolled (equals `device.device`).
    pub public_id: String,
}

/// Is this auth session already bound to a device? `myKeyEnvelopes` is the
/// cheapest `requireDevice` query — anything except the relay's
/// `unauthenticated` code still propagates.
async fn session_bound(client: &mut RelayClient) -> Result<bool> {
    match client.query("relayEnvelopes:myKeyEnvelopes", vec![]).await {
        Ok(_) => Ok(true),
        Err(Error::Protocol("relay unauthenticated")) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Retire a device row: the subject-level revoke mutation.
async fn revoke_device(client: &mut RelayClient, device_id: &str) -> Result<()> {
    client
        .mutation("relayDevices:revoke", vec![("deviceId", json!(device_id))])
        .await?;
    Ok(())
}

fn bound_elsewhere() -> Error {
    Error::Conflict(
        "this sign-in is bound to a device missing from local custody — \
         clear the stored session and link again",
    )
}

/// The full link tail: after `verify_code`, enroll the device and settle
/// key custody — mint an account key when this is the first executor,
/// otherwise require an existing wrap addressed to this device.
///
/// Resumable: the device identity is persisted before enrollment, and
/// server state drives the retry branch — an auth session binds exactly
/// one device, so a retry that generated fresh keys would leave the
/// session with two bindings, which `requireDevice` rejects.
pub async fn finish_link(
    client: &mut RelayClient,
    state_root: &Path,
    deployment_url: &str,
    device_class: &str,
    label: &str,
) -> Result<LinkOutcome> {
    let mut device = match custody::load_device(state_root)? {
        Some(device) => device,
        None => {
            if session_bound(client).await? {
                return Err(bound_elsewhere());
            }
            let device = DeviceIdentity::generate()?;
            custody::store_device(state_root, &device, device_class, label, &device.device)?;
            device
        }
    };

    let status = device_rows(client)
        .await?
        .into_iter()
        .find(|row| row.device_id == device.device)
        .map(|row| row.status);
    match status.as_deref() {
        Some("active") => {
            if !session_bound(client).await? {
                // The device is bound to a session custody lost — its id
                // never rebinds, so retire it and enroll a fresh identity.
                revoke_device(client, &device.device).await?;
                if session_bound(client).await? {
                    return Err(bound_elsewhere());
                }
                device = DeviceIdentity::generate()?;
                custody::store_device(state_root, &device, device_class, label, &device.device)?;
                enroll_device(client, &device, device_class, label).await?;
            }
        }
        Some("pending") => {
            if session_bound(client).await? {
                return Err(bound_elsewhere());
            }
            bind_device(client, &device).await?;
        }
        Some(_) => return Err(protocol("unexpected device status")),
        None => {
            if session_bound(client).await? {
                return Err(bound_elsewhere());
            }
            enroll_device(client, &device, device_class, label).await?;
        }
    }
    let public_id = device.device.clone();

    let key_outcome = match collect_key_wrap(client, &device).await? {
        Some((key, version)) => {
            custody::store_account_key(state_root, &key, version)?;
            KeyOutcome::Adopted {
                key_version: version,
            }
        }
        None => {
            // Mint only on a genuinely empty account — the peer list
            // already includes this device (it just registered), so
            // "alone" means it is the only entry. A second executor that
            // minted its own key would split the fleet's encryption
            // domain — any device that isn't alone waits for a wrap.
            let alone = peers(client).await?.keys().all(|id| id == &device.device);
            if alone && device_class == wire::EXECUTOR_CLASS {
                let key = AccountKey::generate();
                custody::store_account_key(state_root, &key, 1)?;
                KeyOutcome::Minted
            } else {
                // A device without a wrap cannot decrypt fleet content;
                // leave custody unset so the caller can prompt for
                // admission by an enrolled device (`xcb remote admit`).
                KeyOutcome::AwaitingWrap
            }
        }
    };

    custody::store_device(state_root, &device, device_class, label, &public_id)?;
    custody::store_link(
        state_root,
        &RelayLink {
            boot_generation: 1,
            deployment_url: deployment_url.to_string(),
        },
    )?;

    Ok(LinkOutcome {
        device,
        key_outcome,
        public_id,
    })
}
