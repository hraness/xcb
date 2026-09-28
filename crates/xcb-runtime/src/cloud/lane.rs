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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
/// Peer public keys are refreshed at this cadence — not every poll — and
/// immediately when a command arrives from a requester we do not know.
const PEER_REFRESH_MS: u64 = 60_000;
/// Presence is extended when fewer than this many milliseconds remain on
/// the relay's 45 s presence window.
const PRESENCE_LEAD_MS: u64 = 30_000;
/// Presence is also extended after this much monotonic time since the last
/// refresh, so a local clock that disagrees with the relay's cannot let the
/// row lapse while the machine is awake.
const PRESENCE_REFRESH: Duration = Duration::from_secs(15);

/// What keeping presence alive needs on this pump.
#[derive(Debug, PartialEq, Eq)]
enum PresenceStep {
    Fresh,
    Heartbeat,
    /// The row already lapsed by the local clock (after a sleep or a long
    /// stall): the relay only extends live rows, so register it again.
    Reconnect,
}

fn presence_step(presence_until: u64, now_ms: u64, since_refresh: Duration) -> PresenceStep {
    if presence_until <= now_ms {
        PresenceStep::Reconnect
    } else if presence_until <= now_ms.saturating_add(PRESENCE_LEAD_MS)
        || since_refresh >= PRESENCE_REFRESH
    {
        PresenceStep::Heartbeat
    } else {
        PresenceStep::Fresh
    }
}

/// `relayDevices:heartbeat` refuses a presence row that expired or was
/// swept (`invalid-argument` on `device`). That is not a lane failure: the
/// same connection registers again through `relayDevices:connect`.
fn presence_lapsed(error: &Error) -> bool {
    matches!(
        error,
        Error::Protocol("relay invalid-argument: device" | "relay invalid-argument")
    )
}

/// Everything a boot needs out of custody.
pub struct LaneKeys {
    pub device: DeviceIdentity,
    pub account_key: AccountKey,
    pub key_version: u64,
    pub session: CloudSession,
    pub deployment_url: String,
    pub boot_generation: u64,
    /// Where refreshed sessions and the bumped boot generation persist.
    pub state_root: PathBuf,
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
        state_root: state_root.to_path_buf(),
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

/// What `claim` reports for one command.
pub enum ClaimOutcome {
    /// The command is bound to the returned authority — proceed to
    /// `mark_effect_started` with this exact tuple.
    Claimed(AuthorityTuple),
    /// The command closed before we could bind it (expired, cancelled,
    /// or already terminal) — nothing to execute.
    Closed,
}

pub struct RelayLane {
    client: RelayClient,
    device: DeviceIdentity,
    account_key: AccountKey,
    key_version: u64,
    authority: AuthorityTuple,
    session: CloudSession,
    state_root: std::path::PathBuf,
    /// deviceId → public keys, refreshed on `PEER_REFRESH_MS` and on
    /// demand for an unknown requester so enrollments land promptly.
    peers: BTreeMap<String, PeerDevice>,
    /// Wall-clock millisecond at which `peers` next counts as stale.
    peers_fresh_until: u64,
    /// Presence connection this boot owns.
    connection_id: String,
    /// This boot's presence fingerprint, reused when presence reconnects.
    fingerprint: String,
    presence_until: u64,
    /// When presence was last extended, by the monotonic clock.
    presence_refreshed: Instant,
}

impl RelayLane {
    /// Boot the lane: connect, authenticate, register presence.
    pub async fn boot(keys: LaneKeys) -> Result<Self> {
        let mut client = RelayClient::connect(&keys.deployment_url).await?;
        client.bind_session_keys(
            &keys.state_root,
            &keys.session,
            &keys.device,
            &keys.account_key,
            keys.key_version,
        )?;
        let mut session = keys.session;
        // Refresh before attaching: an expired token stalls the socket's
        // reconnect loop and starves the presence call below.
        link::refresh_if_due(&mut client, &keys.state_root, &mut session).await?;
        client.authenticate(&session.token).await;
        // One custody owns one presence row: a stable connection id means
        // a reboot patches the same row rather than accumulating stale
        // entries faster than the retention sweep collects them.
        let connection_id = format!("xcb-{}", keys.device.device);
        let fingerprint = format!("xcb-boot-{}", keys.boot_generation);
        let mut lane = Self {
            client,
            device: keys.device,
            account_key: keys.account_key,
            key_version: keys.key_version,
            authority: AuthorityTuple::boot(keys.boot_generation),
            session,
            state_root: keys.state_root,
            peers: BTreeMap::new(),
            peers_fresh_until: 0,
            connection_id,
            fingerprint: fingerprint.clone(),
            presence_until: 0,
            presence_refreshed: Instant::now(),
        };
        // `connect` doubles as the session's liveness probe: a token that
        // fails server-side while locally fresh recovers through one
        // forced refresh rather than wedging the socket.
        let device_id = lane.device.device.clone();
        let connection_id = lane.connection_id.clone();
        let response = link::with_session_recovery(
            &mut lane.client,
            &lane.state_root,
            &mut lane.session,
            async |client: &mut RelayClient| {
                client
                    .mutation(
                        "relayDevices:connect",
                        vec![
                            ("connectionId", json!(connection_id.clone())),
                            ("deviceId", json!(device_id.clone())),
                            ("fingerprint", json!(fingerprint.clone())),
                        ],
                    )
                    .await
            },
        )
        .await?;
        lane.presence_until = presence_until(&response)?;
        lane.presence_refreshed = Instant::now();
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

    /// Refresh the auth session when inside its expiry lead, keep presence
    /// alive, and refresh peer keys.
    pub async fn keepalive(&mut self, now_ms: u64) -> Result<()> {
        link::refresh_if_due(&mut self.client, &self.state_root, &mut self.session).await?;
        let response = match presence_step(
            self.presence_until,
            now_ms,
            self.presence_refreshed.elapsed(),
        ) {
            PresenceStep::Fresh => None,
            PresenceStep::Reconnect => Some(self.connect_presence().await?),
            PresenceStep::Heartbeat => Some(match self.heartbeat().await {
                // The relay expired the row sooner than the local clock
                // predicted (clock skew, a stall): register it again.
                Err(error) if presence_lapsed(&error) => self.connect_presence().await?,
                other => other?,
            }),
        };
        if let Some(response) = response {
            self.presence_until = presence_until(&response)?;
            self.presence_refreshed = Instant::now();
        }
        if self.peers_fresh_until <= now_ms {
            self.refresh_peers(now_ms).await?;
        }
        Ok(())
    }

    async fn heartbeat(&mut self) -> Result<Value> {
        self.client
            .mutation(
                "relayDevices:heartbeat",
                vec![
                    ("connectionId", json!(self.connection_id)),
                    ("deviceId", json!(self.device.device)),
                ],
            )
            .await
    }

    /// Register this boot's presence row again under the same connection
    /// id; the relay patches the existing row or recreates a swept one.
    async fn connect_presence(&mut self) -> Result<Value> {
        self.client
            .mutation(
                "relayDevices:connect",
                vec![
                    ("connectionId", json!(self.connection_id)),
                    ("deviceId", json!(self.device.device)),
                    ("fingerprint", json!(self.fingerprint)),
                ],
            )
            .await
    }

    /// Re-fetch the device list so envelope verification sees enrollments.
    /// Called by `keepalive` on the refresh cadence and by `pump` hosts
    /// when a command arrives from a requester not yet in the map.
    pub async fn refresh_peers(&mut self, now_ms: u64) -> Result<()> {
        self.peers = link::peers(&mut self.client).await?;
        self.peers_fresh_until = now_ms + PEER_REFRESH_MS;
        Ok(())
    }

    /// Whether a requester device id already resolves in the peer map —
    /// hosts use this to trigger an out-of-cadence refresh.
    pub fn knows_requester(&self, device: &str) -> bool {
        self.peers.contains_key(device)
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

    /// Claim a pending or prepared command, binding a fresh-fence
    /// authority. A `prepared` row bound to a stale earlier authority
    /// rebinds to ours. Returns `Closed` when the command expired or
    /// otherwise terminated before the bind landed; `Claimed` carries the
    /// authority tuple the relay recorded — present it verbatim to
    /// `mark_effect_started` and `settle`.
    pub async fn claim(&mut self, command: &CommandRow) -> Result<ClaimOutcome> {
        let requested = self.next_authority();
        let response = self
            .client
            .mutation(
                "relayCommands:claim",
                vec![
                    (
                        "authority",
                        serde_json::to_value(&requested).map_err(Error::Json)?,
                    ),
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(command.public_id)),
                ],
            )
            .await?;
        match response.get("outcome").and_then(Value::as_str) {
            Some("applied" | "rebound" | "replay") => {
                // The recorded tuple is the source of truth — for a replay
                // it is the previously bound tuple, which may differ from
                // `requested` (e.g. a fence we already spent).
                let bound = response
                    .get("command")
                    .and_then(|command| command.get("boundAuthority"))
                    .cloned()
                    .ok_or(protocol("claim missing boundAuthority"))?;
                let authority: AuthorityTuple =
                    serde_json::from_value(bound).map_err(Error::Json)?;
                Ok(ClaimOutcome::Claimed(authority))
            }
            _ => Ok(ClaimOutcome::Closed),
        }
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
        // The digest commits to the plaintext, not the sealed envelope —
        // a retried settle reseals under a fresh IV and must still replay.
        let result_digest = super::canonical::bytes_digest(&plaintext);
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

    /// Recover a command whose bound authority is stale or which we
    /// cannot finish: from `prepared` the honest close is `failed` (the
    /// effect never began); from `effect_started` the only honest close
    /// is `ambiguous` — the reducer rejects `applied` here, which is
    /// correct: this authority never observed the effect.
    pub async fn recover(&mut self, command: &CommandRow) -> Result<()> {
        let (state, result_code) = match command.state {
            CommandState::Prepared => ("failed", "effect-never-started"),
            CommandState::EffectStarted => ("ambiguous", "effect-uncertain"),
            _ => return Err(invalid("recover needs prepared or effect_started")),
        };
        let authority = self.next_authority();
        self.client
            .mutation(
                "relayCommands:recover",
                vec![
                    (
                        "authority",
                        serde_json::to_value(&authority).map_err(Error::Json)?,
                    ),
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(command.public_id)),
                    ("resultCode", json!(result_code)),
                    ("state", json!(state)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Drop this boot's presence connection.
    pub async fn disconnect(&mut self) -> Result<()> {
        self.client
            .mutation(
                "relayDevices:disconnect",
                vec![
                    ("connectionId", json!(self.connection_id)),
                    ("deviceId", json!(self.device.device)),
                ],
            )
            .await?;
        self.presence_until = 0;
        Ok(())
    }

    /// Run one pass over the command queue. `pending`/`prepared` rows are
    /// claimed (stale `prepared` rebinds to this later authority), marked
    /// effect_started, handed to `handler`, and settled. `effect_started`
    /// rows are unobserved effects — the previous claimant died mid-effect
    /// — so they close `ambiguous` through recovery rather than settle.
    /// A handler error settles `failed` rather than poisoning the lane.
    pub async fn pump<H>(&mut self, handler: &mut H) -> Result<usize>
    where
        H: AsyncFnMut(&OpenedCommand) -> Result<CommandOutcome>,
    {
        self.keepalive(now_ms()).await?;
        let mut settled = 0;
        let commands = self.poll(None).await?;
        // A command from a device enrolled after our last refresh can't be
        // verified — pull the peer list out of cadence rather than close
        // its payloads as rejected.
        if commands
            .iter()
            .any(|command| !self.knows_requester(&command.requesting_device_id))
        {
            self.refresh_peers(now_ms()).await?;
        }
        for command in commands {
            if command.state == CommandState::EffectStarted {
                self.recover(&command).await?;
                continue;
            }
            if command.state != CommandState::Pending && command.state != CommandState::Prepared {
                continue;
            }
            let ClaimOutcome::Claimed(authority) = self.claim(&command).await? else {
                continue;
            };
            self.mark_effect_started(&command, &authority).await?;
            let (opened, outcome) = match self.open_payload(&command) {
                Ok(opened) => match handler(&opened).await {
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

    /// Current relay-side revision for `scope` (0 when absent) — resyncs
    /// the local CAS pin after a restart or a losing write race.
    pub async fn projection_revision(&mut self, scope: &str) -> Result<u64> {
        let row = self
            .client
            .query(
                "relayProjections:get",
                vec![
                    ("deviceId", json!(self.device.device)),
                    ("scope", json!(scope)),
                ],
            )
            .await?;
        if row.is_null() {
            return Ok(0);
        }
        row.get("revision")
            .and_then(Value::as_f64)
            .map(|revision| revision as u64)
            .ok_or(protocol("projection revision shape"))
    }
}

/// The relay's new presence expiry from a `connect` or `heartbeat` reply.
fn presence_until(response: &Value) -> Result<u64> {
    response
        .get("presenceUntil")
        .and_then(Value::as_f64)
        .map(|until| until as u64)
        .ok_or(protocol("presence reply missing presenceUntil"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// After a sleep or a stall past the relay's 45 s presence window the
    /// lane re-registers presence instead of heartbeating a lapsed row, the
    /// heartbeat that failed every pump with `relay invalid-argument`.
    #[test]
    fn a_lapsed_presence_row_reconnects_instead_of_failing_the_pump() {
        let now = 1_800_000_000_000;
        let fresh = Duration::from_secs(1);
        assert_eq!(presence_step(now + 40_000, now, fresh), PresenceStep::Fresh);
        assert_eq!(
            presence_step(now + 20_000, now, fresh),
            PresenceStep::Heartbeat
        );
        assert_eq!(presence_step(now, now, fresh), PresenceStep::Reconnect);
        assert_eq!(
            presence_step(now - 3_600_000, now, fresh),
            PresenceStep::Reconnect
        );
        // A local clock running behind the relay's still heartbeats on the
        // monotonic cadence, well inside the relay's window.
        assert_eq!(
            presence_step(now + 40_000, now, PRESENCE_REFRESH),
            PresenceStep::Heartbeat
        );
        // Only the lapsed-row refusal reconnects; every other relay error
        // still fails the pump.
        assert!(presence_lapsed(&Error::Protocol(
            "relay invalid-argument: device"
        )));
        assert!(presence_lapsed(&Error::Protocol("relay invalid-argument")));
        assert!(!presence_lapsed(&Error::Protocol(
            "relay invalid-argument: deviceId"
        )));
        assert!(!presence_lapsed(&Error::Protocol("relay unauthenticated")));
        assert!(!presence_lapsed(&Error::Unavailable(
            "relay request timed out: https://relay.example did not answer within 30s"
        )));
    }

    #[test]
    fn presence_replies_parse_their_expiry() {
        assert_eq!(
            presence_until(&json!({"presenceUntil": 1_234_567.0})).unwrap(),
            1_234_567
        );
        assert!(presence_until(&json!({})).is_err());
    }
}
