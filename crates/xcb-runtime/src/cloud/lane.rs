//! The supervisor's relay lane. One `RelayLane` per daemon boot: it holds
//! the boot authority tuple, connects presence, polls nonterminal commands
//! addressed to this device, and drives each through
//! claim → effect_started → execute → settle under authority fencing.
//!
//! Execution is caller-supplied: the lane opens the encrypted command
//! payload, hands the plaintext to a `CommandHandler`, and seals whatever
//! the handler returns. ManagedStore wiring lives above this layer so the
//! lane stays a pure protocol state machine.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::{Error, Result};

use super::client::RelayClient;
use super::crypto::{AccountKey, DeviceIdentity, PeerDevice, open_envelope, seal_envelope};
use super::custody::{self, CloudSession};
use super::link;
use super::wire::{self, AuthorityTuple, CommandRow, CommandState};

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

fn protocol(what: &'static str) -> Error {
    Error::Protocol(what)
}

/// Plaintext command bound: ≤8KiB before sealing, per
/// `docs/plans/remote-access.md`.
pub const MAX_COMMAND_PLAINTEXT: usize = 8 * 1024;
/// Projection plaintext bound: ≤32KiB before sealing.
pub const MAX_PROJECTION_PLAINTEXT: usize = 32 * 1024;
/// Wire ciphertext bound the backend enforces.
const CIPHERTEXT_CHARS: usize = 64 * 1024 * 2;

/// Everything a boot needs out of custody.
pub struct LaneKeys {
    pub device: DeviceIdentity,
    pub account_key: AccountKey,
    pub key_version: u64,
    pub session: CloudSession,
    pub deployment_url: String,
    pub boot_generation: u64,
}

/// Load custody and bump the boot generation — the persisted counter is
/// what fences stale claimants after a crash.
pub fn load_lane_keys(state_root: &Path) -> Result<Option<LaneKeys>> {
    let (Some(device), Some((account_key, key_version)), Some(session), Some(link)) = (
        custody::load_device(state_root)?,
        custody::load_account_key(state_root)?,
        custody::load_session(state_root)?,
        custody::load_link(state_root)?,
    ) else {
        return Ok(None);
    };
    let boot_generation = link.boot_generation + 1;
    custody::store_link(
        state_root,
        &custody::RelayLink {
            boot_generation,
            deployment_url: link.deployment_url.clone(),
        },
    )?;
    Ok(Some(LaneKeys {
        device,
        account_key,
        key_version,
        session,
        deployment_url: link.deployment_url,
        boot_generation,
    }))
}

/// A command's decrypted payload plus the requester identity needed to
/// address the reply.
pub struct OpenedCommand {
    pub public_id: String,
    pub kind: String,
    pub requesting_device_id: String,
    pub plaintext: Vec<u8>,
}

/// What the handler decides for one command.
pub enum CommandOutcome {
    /// Effect completed; the payload seals into the result envelope.
    Applied {
        result_code: String,
        plaintext: Vec<u8>,
    },
    /// Effect failed cleanly; payload is diagnostic, still bounded.
    Failed {
        result_code: String,
        plaintext: Vec<u8>,
    },
}

pub struct RelayLane {
    client: RelayClient,
    device: DeviceIdentity,
    account_key: AccountKey,
    key_version: u64,
    authority: AuthorityTuple,
    /// deviceId → public keys, refreshed per poll so enrollments land.
    peers: BTreeMap<String, PeerDevice>,
    /// Presence connection this boot owns.
    connection_id: String,
    presence_until: u64,
}

impl RelayLane {
    /// Boot the lane: connect, authenticate, register presence.
    pub async fn boot(keys: LaneKeys) -> Result<Self> {
        let mut client = RelayClient::connect(&keys.deployment_url).await?;
        client.authenticate(&keys.session.token).await;
        let connection_id = uuid::Uuid::now_v7().to_string();
        let fingerprint = format!("boot:{}", keys.boot_generation);
        let mut lane = Self {
            client,
            device: keys.device,
            account_key: keys.account_key,
            key_version: keys.key_version,
            authority: AuthorityTuple::boot(keys.boot_generation),
            peers: BTreeMap::new(),
            connection_id,
            presence_until: 0,
        };
        let response = lane
            .client
            .mutation(
                "relayDevices:connect",
                vec![
                    ("connectionId", json!(lane.connection_id)),
                    ("deviceId", json!(lane.device.device)),
                    ("fingerprint", json!(fingerprint)),
                ],
            )
            .await?;
        lane.presence_until = response
            .get("presenceUntil")
            .and_then(Value::as_f64)
            .ok_or(protocol("connect missing presenceUntil"))? as u64;
        Ok(lane)
    }

    /// The next fence value for an authority-bearing call. A fresh fence is
    /// drawn once per command claim; every later call in that command's
    /// lifecycle reuses the bound tuple, since `sameDeviceAuthority` is
    /// field-exact.
    fn next_authority(&mut self) -> AuthorityTuple {
        self.authority.fence += 1;
        self.authority.clone()
    }

    /// Refresh peer keys and heartbeat if presence is close to expiry.
    pub async fn keepalive(&mut self, now_ms: u64) -> Result<()> {
        if self.presence_until <= now_ms + 30_000 {
            let response = self
                .client
                .mutation(
                    "relayDevices:heartbeat",
                    vec![
                        ("connectionId", json!(self.connection_id)),
                        ("deviceId", json!(self.device.device)),
                    ],
                )
                .await?;
            self.presence_until = response
                .get("presenceUntil")
                .and_then(Value::as_f64)
                .ok_or(protocol("heartbeat missing presenceUntil"))?
                as u64;
        }
        self.peers = link::peers(&mut self.client).await?;
        Ok(())
    }

    /// List nonterminal commands addressed to this device, parsed through
    /// the wire shape. `limit` rides the backend batch bound.
    pub async fn poll(&mut self, limit: Option<u64>) -> Result<Vec<CommandRow>> {
        let mut args = vec![("deviceId", json!(self.device.device))];
        if let Some(limit) = limit {
            args.push(("limit", json!(limit)));
        }
        let rows = self
            .client
            .query("relayCommands:listForTarget", args)
            .await?;
        let list = rows.as_array().ok_or(protocol("listForTarget shape"))?;
        let mut commands = Vec::with_capacity(list.len());
        for row in list {
            commands.push(serde_json::from_value::<CommandRow>(row.clone()).map_err(Error::Json)?);
        }
        Ok(commands)
    }

    /// Open a claimed command's payload: verify the requester signature and
    /// decrypt under the account key.
    pub fn open_payload(&self, command: &CommandRow) -> Result<OpenedCommand> {
        let envelope = command.payload.as_ref().ok_or(invalid("command payload"))?;
        wire::check_signed_envelope(envelope, CIPHERTEXT_CHARS)?;
        let sender = self
            .peers
            .get(&command.requesting_device_id)
            .ok_or(protocol("requester not in device list"))?;
        let envelope_value = serde_json::to_value(envelope).map_err(Error::Json)?;
        let plaintext = open_envelope(&envelope_value, &self.device, sender, &self.account_key)
            .map_err(|_| protocol("command payload failed to open"))?;
        if plaintext.len() > MAX_COMMAND_PLAINTEXT {
            return Err(invalid("command plaintext bound"));
        }
        Ok(OpenedCommand {
            public_id: command.public_id.clone(),
            kind: command.kind.clone(),
            requesting_device_id: command.requesting_device_id.clone(),
            plaintext,
        })
    }

    /// Claim a pending command, binding a fresh-fence authority. The
    /// returned tuple must be presented verbatim to `mark_effect_started`
    /// and `settle`.
    pub async fn claim(&mut self, command: &CommandRow) -> Result<AuthorityTuple> {
        let authority = self.next_authority();
        self.client
            .mutation(
                "relayCommands:claim",
                vec![
                    (
                        "authority",
                        serde_json::to_value(&authority).map_err(Error::Json)?,
                    ),
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(command.public_id)),
                ],
            )
            .await?;
        Ok(authority)
    }

    /// Mark a claimed command effect_started — the point of no return for
    /// honest recovery.
    pub async fn mark_effect_started(
        &mut self,
        command: &CommandRow,
        authority: &AuthorityTuple,
    ) -> Result<()> {
        self.client
            .mutation(
                "relayCommands:markEffectStarted",
                vec![
                    (
                        "authority",
                        serde_json::to_value(authority).map_err(Error::Json)?,
                    ),
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(command.public_id)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Settle a started command under its bound authority, sealing the
    /// result back to the requester.
    pub async fn settle(
        &mut self,
        command: &OpenedCommand,
        outcome: CommandOutcome,
        authority: &AuthorityTuple,
    ) -> Result<()> {
        let (state, result_code, plaintext) = match outcome {
            CommandOutcome::Applied {
                result_code,
                plaintext,
            } => ("applied", result_code, plaintext),
            CommandOutcome::Failed {
                result_code,
                plaintext,
            } => ("failed", result_code, plaintext),
        };
        if plaintext.len() > MAX_COMMAND_PLAINTEXT {
            return Err(invalid("result plaintext bound"));
        }
        let requester = self
            .peers
            .get(&command.requesting_device_id)
            .ok_or(protocol("requester not in device list"))?;
        let result = seal_envelope(
            &self.device,
            &self.account_key,
            &format!("command.result.{}", command.public_id),
            self.key_version,
            &plaintext,
            Some(&requester.device),
        )?;
        let result_digest = super::canonical::canonical_digest(&result)?;
        self.client
            .mutation(
                "relayCommands:settle",
                vec![
                    (
                        "authority",
                        serde_json::to_value(authority).map_err(Error::Json)?,
                    ),
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(command.public_id)),
                    ("result", result),
                    ("resultCode", json!(result_code)),
                    ("resultDigest", json!(result_digest)),
                    ("state", json!(state)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Run one pass over the command queue: claim each pending row, mark
    /// effect_started, hand the plaintext to `handler`, and settle. A
    /// handler error settles the command `failed` rather than poisoning the
    /// lane.
    pub async fn pump<H>(&mut self, handler: &mut H) -> Result<usize>
    where
        H: FnMut(&OpenedCommand) -> Result<CommandOutcome>,
    {
        self.keepalive(now_ms()).await?;
        let mut settled = 0;
        for command in self.poll(None).await? {
            // `prepared` rows either belong to a dead earlier authority (this
            // later one rebinds them) or replay under our own bound tuple —
            // both land back in `prepared` and proceed.
            if command.state != CommandState::Pending && command.state != CommandState::Prepared {
                continue;
            }
            let authority = self.claim(&command).await?;
            self.mark_effect_started(&command, &authority).await?;
            let (opened, outcome) = match self.open_payload(&command) {
                Ok(opened) => match handler(&opened) {
                    Ok(outcome) => (opened, outcome),
                    Err(_) => (
                        opened_clone(&command),
                        CommandOutcome::Failed {
                            result_code: "handler-error".to_string(),
                            plaintext: b"handler error".to_vec(),
                        },
                    ),
                },
                Err(_) => (
                    opened_clone(&command),
                    CommandOutcome::Failed {
                        result_code: "payload-rejected".to_string(),
                        plaintext: b"payload rejected".to_vec(),
                    },
                ),
            };
            self.settle(&opened, outcome, &authority).await?;
            settled += 1;
        }
        Ok(settled)
    }

    /// Publish an encrypted projection under `scope`, CAS-pinned to
    /// `expected_revision` (0 for first write).
    pub async fn publish_projection(
        &mut self,
        scope: &str,
        plaintext: &[u8],
        expected_revision: u64,
    ) -> Result<u64> {
        if plaintext.len() > MAX_PROJECTION_PLAINTEXT {
            return Err(invalid("projection plaintext bound"));
        }
        let envelope = seal_envelope(
            &self.device,
            &self.account_key,
            scope,
            self.key_version,
            plaintext,
            None,
        )?;
        let response = self
            .client
            .mutation(
                "relayProjections:publish",
                vec![
                    ("deviceId", json!(self.device.device)),
                    ("envelope", envelope),
                    ("expectedRevision", json!(expected_revision)),
                    ("scope", json!(scope)),
                ],
            )
            .await?;
        response
            .get("revision")
            .and_then(Value::as_f64)
            .map(|revision| revision as u64)
            .ok_or(protocol("publish missing revision"))
    }
}

fn opened_clone(command: &CommandRow) -> OpenedCommand {
    OpenedCommand {
        public_id: command.public_id.clone(),
        kind: command.kind.clone(),
        requesting_device_id: command.requesting_device_id.clone(),
        plaintext: Vec::new(),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
