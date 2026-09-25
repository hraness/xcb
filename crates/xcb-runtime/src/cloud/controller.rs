//! The controller lane: `xcb fleet`, `xcb dispatch`, `xcb attention
//! --remote`, `xcb send`, `xcb remote revoke` all drive these calls. A
//! controller is an enrolled device like any other — it never executes
//! commands and never sees plaintext payloads other devices sealed for
//! executors; it seals commands *to* executors and opens their results and
//! account-scoped projections.

use std::collections::BTreeMap;

use serde_json::json;

use crate::{Error, Result};

use super::client::RelayClient;
use super::crypto::{AccountKey, DeviceIdentity, PeerDevice, open_envelope, seal_envelope};
use super::link;
use super::wire::{self, CommandRow, ProjectionRow};

fn invalid(what: &'static str) -> Error {
    Error::from(xcb_core::Error::Invalid(what))
}

fn protocol(what: &'static str) -> Error {
    Error::Protocol(what)
}

/// Wire ciphertext bound the backend enforces.
const CIPHERTEXT_CHARS: usize = 64 * 1024 * 2;
/// Command plaintext bound from the plan.
const MAX_COMMAND_PLAINTEXT: usize = 8 * 1024;

/// A controller session: custody + client + the peer table.
pub struct Controller {
    pub client: RelayClient,
    pub device: DeviceIdentity,
    pub account_key: AccountKey,
    pub key_version: u64,
    pub peers: BTreeMap<String, PeerDevice>,
}

/// The fleet as `relayDevices:list` returns it.
#[derive(Debug, Clone)]
pub struct FleetDevice {
    pub device: String,
    pub device_class: String,
    pub label: String,
    pub online: bool,
    pub status: String,
    pub key_version: u64,
}

/// A decrypted projection row plus its opaque metadata.
pub struct OpenedProjection {
    pub device: String,
    pub scope: String,
    pub revision: u64,
    pub plaintext: Vec<u8>,
    pub updated_at: f64,
}

impl Controller {
    /// Build a controller from custody: load device, session, account key;
    /// connect and authenticate; pull the peer table.
    pub async fn open(
        device: DeviceIdentity,
        account_key: AccountKey,
        key_version: u64,
        session: &super::custody::CloudSession,
        deployment_url: &str,
    ) -> Result<Self> {
        let mut client = RelayClient::connect(deployment_url).await?;
        client.authenticate(&session.token).await;
        let peers = link::peers(&mut client).await?;
        Ok(Self {
            client,
            device,
            account_key,
            key_version,
            peers,
        })
    }

    /// `xcb fleet` — the device list with presence.
    pub async fn fleet(&mut self) -> Result<Vec<FleetDevice>> {
        let rows = self.client.query("relayDevices:list", vec![]).await?;
        let list = rows.as_array().ok_or(protocol("devices list shape"))?;
        let mut out = Vec::with_capacity(list.len());
        for row in list {
            let device: wire::DeviceRow =
                serde_json::from_value(row.clone()).map_err(Error::Json)?;
            out.push(FleetDevice {
                device: device.device_id,
                device_class: device.device_class,
                label: device.label,
                online: device.online,
                status: device.status,
                key_version: device.key_version,
            });
        }
        Ok(out)
    }

    /// `xcb dispatch` / `xcb send` / `xcb attention --remote`: seal the
    /// command payload to the target executor and enqueue it.
    ///
    /// `kind` must be a member of the closed union; `plaintext` is the
    /// already-encoded command body (≤8KiB); `idempotency_key` is a UUIDv7;
    /// `request_digest` pins the payload bytes (`sha256:` of the canonical
    /// form) so an idempotency replay can't smuggle a different effect.
    pub async fn dispatch(
        &mut self,
        target_device_id: &str,
        kind: &str,
        plaintext: &[u8],
        idempotency_key: &str,
        request_digest: &str,
        deadline_ms: Option<u64>,
    ) -> Result<CommandRow> {
        if !wire::is_command_kind(kind) {
            return Err(invalid("command kind"));
        }
        if !wire::is_device_id(target_device_id) {
            return Err(invalid("target device"));
        }
        if plaintext.len() > MAX_COMMAND_PLAINTEXT {
            return Err(invalid("command plaintext bound"));
        }
        if !wire::is_digest(request_digest) {
            return Err(invalid("request digest"));
        }
        let target = self
            .peers
            .get(target_device_id)
            .ok_or(protocol("target device unknown"))?;
        // Addressed to the executor: only it can open the payload.
        let payload = seal_envelope(
            &self.device,
            &self.account_key,
            &format!("command.{kind}.{target_device_id}"),
            self.key_version,
            plaintext,
            Some(&target.device),
        )?;
        let mut args = vec![
            ("idempotencyKey", json!(idempotency_key)),
            ("kind", json!(kind)),
            ("payload", payload),
            ("requestDigest", json!(request_digest)),
            ("targetDeviceId", json!(target_device_id)),
        ];
        if let Some(deadline) = deadline_ms {
            args.push(("deadlineMs", json!(deadline)));
        }
        let response = self.client.mutation("relayCommands:enqueue", args).await?;
        let command = response
            .get("command")
            .cloned()
            .ok_or(protocol("enqueue missing command"))?;
        serde_json::from_value(command).map_err(Error::Json)
    }

    /// Read one command's current state (dispatch status polling).
    pub async fn command(&mut self, public_id: &str) -> Result<CommandRow> {
        let row = self
            .client
            .query("relayCommands:get", vec![("publicId", json!(public_id))])
            .await?;
        serde_json::from_value(row).map_err(Error::Json)
    }

    /// Open a settled command's result envelope (it is addressed to this
    /// device by the executor that settled it).
    pub fn open_result(&self, command: &CommandRow) -> Result<Vec<u8>> {
        let result = command.result.as_ref().ok_or(invalid("no result yet"))?;
        wire::check_signed_envelope(result, CIPHERTEXT_CHARS)?;
        let sender = self
            .peers
            .get(&command.target_device_id)
            .ok_or(protocol("executor not in device list"))?;
        let envelope = serde_json::to_value(result).map_err(Error::Json)?;
        open_envelope(&envelope, &self.device, sender, &self.account_key)
            .map_err(|_| protocol("result envelope failed to open"))
    }

    /// List the commands this device issued (`listForRequester`).
    pub async fn dispatched(&mut self, limit: Option<u64>) -> Result<Vec<CommandRow>> {
        let mut args = vec![("deviceId", json!(self.device.device))];
        if let Some(limit) = limit {
            args.push(("limit", json!(limit)));
        }
        let rows = self
            .client
            .query("relayCommands:listForRequester", args)
            .await?;
        let list = rows.as_array().ok_or(protocol("listForRequester shape"))?;
        let mut commands = Vec::with_capacity(list.len());
        for row in list {
            commands.push(serde_json::from_value::<CommandRow>(row.clone()).map_err(Error::Json)?);
        }
        Ok(commands)
    }

    /// Cancel a pending command this device issued.
    pub async fn cancel(&mut self, public_id: &str) -> Result<()> {
        self.client
            .mutation(
                "relayCommands:cancel",
                vec![
                    ("deviceId", json!(self.device.device)),
                    ("publicId", json!(public_id)),
                ],
            )
            .await?;
        Ok(())
    }

    /// Read every published projection row, decrypted. Devices whose rows
    /// fail signature or decryption are skipped rather than aborting the
    /// fleet read.
    pub async fn projections(&mut self) -> Result<Vec<OpenedProjection>> {
        let rows = self.client.query("relayProjections:list", vec![]).await?;
        let list = rows.as_array().ok_or(protocol("projections list shape"))?;
        let mut out = Vec::with_capacity(list.len());
        for row in list {
            let projection: ProjectionRow = match serde_json::from_value(row.clone()) {
                Ok(row) => row,
                Err(_) => continue,
            };
            let Some(sender) = self.peers.get(&projection.device_id) else {
                continue;
            };
            wire::check_signed_envelope(&projection.envelope, CIPHERTEXT_CHARS)?;
            let envelope = serde_json::to_value(&projection.envelope).map_err(Error::Json)?;
            let Ok(plaintext) = open_envelope(&envelope, &self.device, sender, &self.account_key)
            else {
                continue;
            };
            out.push(OpenedProjection {
                device: projection.device_id,
                scope: projection.scope,
                revision: projection.revision,
                plaintext,
                updated_at: projection.updated_at,
            });
        }
        Ok(out)
    }

    /// `xcb remote revoke` — retire a device id. In-flight commands settle
    /// or expire against the new auth epoch; the id never rebinds.
    pub async fn revoke(&mut self, device_id: &str) -> Result<()> {
        if !wire::is_device_id(device_id) {
            return Err(invalid("device"));
        }
        self.client
            .mutation("relayDevices:revoke", vec![("deviceId", json!(device_id))])
            .await?;
        self.peers.remove(device_id);
        Ok(())
    }

    /// Post a key wrap admitting `recipient_device` into the account — the
    /// `xcb link` peer on the enrolling side collects it.
    pub async fn admit(&mut self, recipient_device: &str) -> Result<()> {
        let recipient = self
            .peers
            .get(recipient_device)
            .ok_or(protocol("recipient device unknown"))?
            .clone();
        link::admit_device(
            &mut self.client,
            &self.device,
            &recipient,
            &self.account_key,
            self.key_version,
        )
        .await
    }
}
