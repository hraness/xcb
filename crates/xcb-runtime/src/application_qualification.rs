//! Private admission for the version-one application inference boundary.
//!
//! This is a reader, never an evidence importer or qualification producer. The
//! trusted host qualification harness must collect the observations below from
//! the actual application executor on the final executable, after the source
//! gates pass, and publish artifacts only after independently joining custody.
//! A pin, login, generated response, or caller-supplied `passed: true` is not
//! evidence. Private files protect this evidence from confined providers; they
//! are not signatures against a malicious process already acting as the owner.
//!
//! Qualification publication and explicit credential replacement must hold the
//! exclusive account lease. Replacement rotates `application-generation.json`
//! BEFORE changing credentials; routine same-account refresh preserves it.
//! Legacy accounts acquire their first random generation only in the trusted
//! qualification producer, under that lease. This reader never creates state.
//! Generate must call `load` again after reserving its account, not reuse a
//! capabilities result. The producer must not expose a generic bypass of normal
//! application admission, arbitrary JSON import, or an expiry-renewal endpoint.

use crate::{Error, Result, application::ApplicationQualification, digest, private, process::Pin};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use xcb_core::{Id, Provider};

pub(crate) const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_MODELS: usize = 64;
const MAX_RECEIPT: usize = 64 * 1024;
const MAX_BOUNDARY: usize = 64 * 1024;
const MAX_GATE_OUTPUT: usize = 4 * 1024 * 1024;
const MAX_LIVE: usize = 16 * 1024;
const UNAVAILABLE: &str = "application qualification missing, expired, or invalid";

/// All values come from current host state, never the application's request.
/// Policy/config digests cover normalized effective application settings,
/// sandbox rules, environment, protocol and zero-tool/hook launch settings.
pub(crate) struct Expected<'a> {
    pub pin: &'a Pin,
    pub account: &'a Id,
    pub policy_sha256: &'a str,
    pub config_sha256: &'a str,
    pub observed_models: &'a [String],
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub runtime_version: String,
    pub runtime_sha256: String,
    pub provider: Provider,
    pub provider_version: String,
    pub provider_sha256: String,
    pub os: String,
    pub arch: String,
    pub policy_sha256: String,
    pub config_sha256: String,
    pub account: Id,
    pub credential_generation: String,
    /// Sorted, unique, full provider/model[/effort] keys. Every entry requires
    /// its own joined live challenge, not merely a provider-global catalog row.
    pub models: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialGeneration {
    pub version: u32,
    pub account: Id,
    /// A fresh random 256-bit opaque value, not a credential/token hash.
    pub generation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Receipt {
    pub version: u32,
    pub binding: Binding,
    /// Collection start and fixed expiration, both Unix milliseconds. Reading
    /// a receipt never changes either, including when no account is available.
    pub observed_at_ms: u64,
    pub expires_at_ms: u64,
    pub boundary_sha256: String,
    /// Ordered identically to binding.models; artifact paths are derived from
    /// digests, never accepted as input from the serialized receipt.
    pub live_sha256: Vec<String>,
}

/// Actual raw gate output is retained in separate protected content-addressed
/// artifacts. A trusted producer records command exit status; the reader also
/// checks the complete mandatory test names against actual Cargo output.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundaryEvidence {
    pub version: u32,
    pub binding_sha256: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub source_gate: GateArtifact,
    pub application_unit: GateArtifact,
    pub application_contract: GateArtifact,
    /// Existing independently verified native confinement artifact. The
    /// producer checks its provider/policy bindings before copying its bytes;
    /// this reader does not reinterpret incompatible provider-specific schemas.
    pub provider_boundary_sha256: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GateArtifact {
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub output_sha256: String,
}

/// These are existing executor/contract tests, not caller-provided case names.
/// Their bodies own zero-authority, non-start, cancellation, bounds, sessionless
/// operation, cleanup, and retained-account assertions. Changing those tests or
/// the application policy invalidates the host/policy binding as well.
pub(crate) const UNIT_CASES: &[&str] = &[
    "application::tests::ephemeral_completion_requires_join_and_settles_sessionless_lease",
    "application::tests::empty_authoritative_output_fails_after_proven_cleanup",
    "application::tests::preparation_consumes_deadline_and_never_starts_an_expired_request",
    "application::tests::unproven_preparation_cleanup_retains_the_request_lease",
    "application::tests::denied_tools_return_no_text_and_are_never_executed",
    "application::tests::unproven_protocol_join_retains_custody_despite_completed_root",
    "application::tests::cancellation_deadline_and_output_limits_settle_without_output",
];
pub(crate) const CONTRACT_CASES: &[&str] = &[
    "request_is_closed_bounded_and_requires_explicit_selection",
    "capabilities_do_not_claim_qualification_or_expose_state_paths",
    "unqualified_request_has_no_provider_or_session_effects",
    "invalid_and_precancelled_requests_prove_request_local_non_start",
    "failure_does_not_invent_join_or_include_output",
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LiveEvidence {
    pub version: u32,
    pub binding_sha256: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub model: String,
    /// Fresh random 256-bit synthetic challenge. No user prompts/credentials.
    /// The trusted harness asks for exactly xcb-application-v1:<nonce>.
    pub nonce: String,
    /// Exact serialized GenerateResponse from the SAME application executor.
    /// Its joined/effects outcome is emitted only after physical process,
    /// protocol, egress, authentication, and account settlement have succeeded.
    pub response_json: String,
}

/// Never serialize this authority object or expose its private bindings.
pub(crate) struct Admission {
    binding: Binding,
    receipt_sha256: String,
    expires_at_ms: u64,
}
impl Admission {
    pub(crate) fn covers(&self, full_model_key: &str) -> bool {
        self.binding.models.iter().any(|key| key == full_model_key)
    }
    pub(crate) fn public(&self) -> ApplicationQualification {
        ApplicationQualification {
            runtime_version: self.binding.runtime_version.clone(),
            runtime_digest: self.binding.runtime_sha256.clone(),
            evidence_digest: self.receipt_sha256.clone(),
            expires_at: self.expires_at_ms,
        }
    }
}

pub(crate) fn load(root: &Path, expected: &Expected<'_>, now_ms: u64) -> Result<Admission> {
    // Verify both current provider bytes and the running host's captured identity.
    expected.pin.verify()?;
    load_verified(root, expected, now_ms).map_err(|_| Error::Unavailable(UNAVAILABLE))
}

fn load_verified(root: &Path, expected: &Expected<'_>, now_ms: u64) -> Result<Admission> {
    let mut reader = Reader::default();
    reader.directory(root)?;
    let accounts = root.join("accounts");
    reader.directory(&accounts)?;
    let account = accounts.join(expected.account.as_str());
    reader.directory(&account)?;
    let generation: CredentialGeneration =
        serde_json::from_slice(&reader.read(&account.join("application-generation.json"), 1024)?)?;
    require(generation.version == 1 && generation.account == *expected.account)?;
    require(sha256(&generation.generation))?;

    let qualification = root.join("qualification");
    reader.directory(&qualification)?;
    let version = qualification.join("application-v1");
    reader.directory(&version)?;
    let directory = version.join(expected.account.as_str());
    reader.directory(&directory)?;
    let artifacts = directory.join("artifacts");
    reader.directory(&artifacts)?;
    let raw = reader.read(&directory.join("receipt.json"), MAX_RECEIPT)?;
    let receipt: Receipt = serde_json::from_slice(&raw)?;
    validate_binding(&receipt.binding, expected, &generation.generation)?;
    require(receipt.version == 1)?;
    require(receipt.observed_at_ms > 0 && receipt.observed_at_ms <= now_ms)?;
    require(receipt.expires_at_ms > now_ms)?;
    require(
        receipt.expires_at_ms > receipt.observed_at_ms
            && receipt.expires_at_ms - receipt.observed_at_ms <= MAX_AGE_MS,
    )?;
    require(receipt.live_sha256.len() == receipt.binding.models.len())?;
    let binding_sha256 = digest(serde_json::to_vec(&receipt.binding)?);
    let boundary: BoundaryEvidence = serde_json::from_slice(&reader.artifact(
        &artifacts,
        &receipt.boundary_sha256,
        MAX_BOUNDARY,
    )?)?;
    require(boundary.version == 1 && boundary.binding_sha256 == binding_sha256)?;
    interval(
        boundary.started_at_ms,
        boundary.finished_at_ms,
        &receipt,
        now_ms,
    )?;
    for (gate, cases) in [
        (&boundary.source_gate, &[][..]),
        (&boundary.application_unit, UNIT_CASES),
        (&boundary.application_contract, CONTRACT_CASES),
    ] {
        validate_gate(&mut reader, &artifacts, gate, cases)?;
    }
    reader.artifact(
        &artifacts,
        &boundary.provider_boundary_sha256,
        MAX_GATE_OUTPUT,
    )?;
    let mut nonces = std::collections::BTreeSet::new();
    let mut request_ids = std::collections::BTreeSet::new();
    for (model, hash) in receipt.binding.models.iter().zip(&receipt.live_sha256) {
        let live: LiveEvidence =
            serde_json::from_slice(&reader.artifact(&artifacts, hash, MAX_LIVE)?)?;
        let request_id = validate_live(&live, model, &receipt, &binding_sha256, now_ms)?;
        require(nonces.insert(live.nonce) && request_ids.insert(request_id))?;
    }
    // Rotation/replacement while evidence was read invalidates the admission.
    // Generate additionally holds account custody through read and execution.
    reader.finish()?;
    Ok(Admission {
        binding: receipt.binding,
        receipt_sha256: digest(raw),
        expires_at_ms: receipt.expires_at_ms,
    })
}

fn validate_binding(binding: &Binding, expected: &Expected<'_>, generation: &str) -> Result<()> {
    require(
        binding.runtime_version == env!("CARGO_PKG_VERSION")
            && binding.runtime_sha256 == expected.pin.host_sha256
            && binding.provider == expected.pin.provider
            && binding.provider_version == expected.pin.version
            && binding.provider_sha256 == expected.pin.sha256
            && binding.os == std::env::consts::OS
            && binding.arch == std::env::consts::ARCH
            && binding.policy_sha256 == expected.policy_sha256
            && binding.config_sha256 == expected.config_sha256
            && binding.account == *expected.account
            && binding.credential_generation == generation,
    )?;
    for hash in [
        &binding.runtime_sha256,
        &binding.provider_sha256,
        &binding.policy_sha256,
        &binding.config_sha256,
        &binding.credential_generation,
    ] {
        require(sha256(hash))?;
    }
    require(!binding.models.is_empty() && binding.models.len() <= MAX_MODELS)?;
    require(binding.models.windows(2).all(|pair| pair[0] < pair[1]))?;
    for key in &binding.models {
        require(valid_model_key(key, binding.provider) && expected.observed_models.contains(key))?;
    }
    Ok(())
}

fn valid_model_key(key: &str, provider: Provider) -> bool {
    if key.len() > 512 {
        return false;
    }
    let parts: Vec<_> = key.split('/').collect();
    (2..=3).contains(&parts.len())
        && parts[0] == provider.to_string()
        && parts[1..].iter().all(|part| Id::new(*part).is_ok())
}

fn validate_gate(
    reader: &mut Reader,
    artifacts: &Path,
    gate: &GateArtifact,
    cases: &[&str],
) -> Result<Vec<u8>> {
    require(gate.exit_code == 0 && !gate.argv.is_empty() && gate.argv.len() <= 64)?;
    require(
        gate.argv
            .iter()
            .all(|arg| !arg.is_empty() && arg.len() <= 4096 && !arg.chars().any(char::is_control)),
    )?;
    let output = reader.artifact(artifacts, &gate.output_sha256, MAX_GATE_OUTPUT)?;
    if !cases.is_empty() {
        let output = std::str::from_utf8(&output).map_err(|_| Error::Unavailable(UNAVAILABLE))?;
        require(
            !output
                .lines()
                .any(|line| line.starts_with("test result: FAILED.")),
        )?;
        require(
            output
                .lines()
                .any(|line| line.starts_with("test result: ok.") && line.contains("; 0 failed;")),
        )?;
        for case in cases {
            let wanted = format!("test {case} ... ok");
            require(output.lines().filter(|line| *line == wanted).count() == 1)?;
        }
    }
    Ok(output)
}

fn validate_live(
    evidence: &LiveEvidence,
    model: &str,
    receipt: &Receipt,
    binding_sha256: &str,
    now_ms: u64,
) -> Result<Id> {
    require(
        evidence.version == 1
            && evidence.binding_sha256 == binding_sha256
            && evidence.model == model
            && sha256(&evidence.nonce)
            && evidence.response_json.len() <= 4096,
    )?;
    interval(
        evidence.started_at_ms,
        evidence.finished_at_ms,
        receipt,
        now_ms,
    )?;
    let response: Success = serde_json::from_str(&evidence.response_json)?;
    require(
        response.version == 1
            && response.status == "completed"
            && response.account == receipt.binding.account
            && response.model == model
            && response.text == format!("xcb-application-v1:{}", evidence.nonce)
            && response.outcome.terminal == "completed"
            && response.outcome.joined
            && response.outcome.effects == "none",
    )?;
    Ok(response.request_id)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Success {
    version: u32,
    status: String,
    request_id: Id,
    account: Id,
    model: String,
    text: String,
    outcome: Outcome,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    terminal: String,
    joined: bool,
    effects: String,
}

fn interval(start: u64, finish: u64, receipt: &Receipt, now: u64) -> Result<()> {
    require(
        start >= receipt.observed_at_ms
            && finish >= start
            && finish <= now
            && finish < receipt.expires_at_ms,
    )
}
fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn require(condition: bool) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Unavailable(UNAVAILABLE))
    }
}

#[derive(PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    bytes: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl From<&Metadata> for FileIdentity {
    fn from(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            bytes: metadata.len(),
            mtime: (metadata.mtime(), metadata.mtime_nsec()),
            ctime: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
struct OpenEvidence {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
    maximum: usize,
}
impl OpenEvidence {
    fn verify(&self) -> Result<()> {
        private::check_file(&self.file, self.maximum as u64)?;
        require(FileIdentity::from(&self.file.metadata()?) == self.identity)?;
        require(FileIdentity::from(&fs::symlink_metadata(&self.path)?) == self.identity)
    }
}
#[derive(Default)]
struct Reader {
    files: Vec<OpenEvidence>,
    directories: Vec<(PathBuf, u64, u64)>,
}
impl Reader {
    fn directory(&mut self, path: &Path) -> Result<()> {
        let meta = read_directory(path)?;
        self.directories
            .push((path.to_owned(), meta.dev(), meta.ino()));
        Ok(())
    }
    fn read(&mut self, path: &Path, maximum: usize) -> Result<Vec<u8>> {
        let file = private::open_file(path, maximum as u64)?;
        let identity = FileIdentity::from(&file.metadata()?);
        let mut bytes = Vec::new();
        (&file).take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        require(!bytes.is_empty() && bytes.len() <= maximum)?;
        let evidence = OpenEvidence {
            path: path.to_owned(),
            file,
            identity,
            maximum,
        };
        evidence.verify()?;
        self.files.push(evidence);
        Ok(bytes)
    }
    fn artifact(&mut self, directory: &Path, hash: &str, maximum: usize) -> Result<Vec<u8>> {
        require(sha256(hash))?;
        let bytes = self.read(&directory.join(format!("{hash}.json")), maximum)?;
        require(digest(&bytes) == hash)?;
        Ok(bytes)
    }
    fn finish(self) -> Result<()> {
        for evidence in self.files {
            evidence.verify()?;
        }
        for (path, dev, ino) in self.directories {
            let meta = read_directory(&path)?;
            require(meta.dev() == dev && meta.ino() == ino)?;
        }
        Ok(())
    }
}
fn read_directory(path: &Path) -> Result<Metadata> {
    let meta = fs::symlink_metadata(path)?;
    require(
        path.is_absolute()
            && path.canonicalize()? == path
            && meta.is_dir()
            && meta.uid() == rustix::process::getuid().as_raw()
            && meta.mode() & 0o077 == 0,
    )?;
    Ok(meta)
}

/// Closed input for the trusted, fixed-challenge qualification command. The
/// descriptor references actual private artifact bytes; it cannot activate
/// application traffic on its own. The command must authenticate its provenance
/// as task-owned gate output, not offer an import of arbitrary caller assertions.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrerequisiteDescriptor {
    pub version: u32,
    pub runtime_version: String,
    pub runtime_sha256: String,
    pub provider: Provider,
    pub provider_version: String,
    pub provider_sha256: String,
    pub os: String,
    pub arch: String,
    pub policy_sha256: String,
    pub config_sha256: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub source_gate: GateArtifact,
    pub application_unit: GateArtifact,
    pub application_contract: GateArtifact,
    pub provider_boundary_sha256: String,
}

pub(crate) struct VerifiedPrerequisites {
    descriptor: PrerequisiteDescriptor,
    artifacts: std::collections::BTreeMap<String, Vec<u8>>,
}
impl VerifiedPrerequisites {
    pub(crate) fn observed_at_ms(&self) -> u64 {
        self.descriptor.started_at_ms
    }
    pub(crate) fn artifacts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.artifacts
            .iter()
            .map(|(hash, bytes)| (hash.as_str(), bytes.as_slice()))
    }
    /// Produces only the already-observed boundary record. Live evidence and
    /// publication still require the common executor and account custody.
    pub(crate) fn boundary(&self, binding: &Binding) -> Result<BoundaryEvidence> {
        let d = &self.descriptor;
        require(
            binding.runtime_version == d.runtime_version
                && binding.runtime_sha256 == d.runtime_sha256
                && binding.provider == d.provider
                && binding.provider_version == d.provider_version
                && binding.provider_sha256 == d.provider_sha256
                && binding.os == d.os
                && binding.arch == d.arch
                && binding.policy_sha256 == d.policy_sha256
                && binding.config_sha256 == d.config_sha256,
        )?;
        Ok(BoundaryEvidence {
            version: 1,
            binding_sha256: digest(serde_json::to_vec(binding)?),
            started_at_ms: d.started_at_ms,
            finished_at_ms: d.finished_at_ms,
            source_gate: d.source_gate.clone(),
            application_unit: d.application_unit.clone(),
            application_contract: d.application_contract.clone(),
            provider_boundary_sha256: d.provider_boundary_sha256.clone(),
        })
    }
}

pub(crate) fn verify_prerequisites(
    directory: &Path,
    expected: &Expected<'_>,
    now_ms: u64,
) -> Result<VerifiedPrerequisites> {
    expected.pin.verify()?;
    verify_prerequisites_verified(directory, expected, now_ms)
        .map_err(|_| Error::Unavailable(UNAVAILABLE))
}
fn verify_prerequisites_verified(
    directory: &Path,
    expected: &Expected<'_>,
    now_ms: u64,
) -> Result<VerifiedPrerequisites> {
    let mut reader = Reader::default();
    reader.directory(directory)?;
    let d: PrerequisiteDescriptor =
        serde_json::from_slice(&reader.read(&directory.join("prerequisites.json"), MAX_BOUNDARY)?)?;
    require(
        d.version == 1
            && d.runtime_version == env!("CARGO_PKG_VERSION")
            && d.runtime_sha256 == expected.pin.host_sha256
            && d.provider == expected.pin.provider
            && d.provider_version == expected.pin.version
            && d.provider_sha256 == expected.pin.sha256
            && d.os == std::env::consts::OS
            && d.arch == std::env::consts::ARCH
            && d.policy_sha256 == expected.policy_sha256
            && d.config_sha256 == expected.config_sha256
            && d.started_at_ms > 0
            && d.started_at_ms <= d.finished_at_ms
            && d.finished_at_ms <= now_ms
            && now_ms - d.started_at_ms < MAX_AGE_MS,
    )?;
    for hash in [
        &d.runtime_sha256,
        &d.provider_sha256,
        &d.policy_sha256,
        &d.config_sha256,
    ] {
        require(sha256(hash))?;
    }
    let artifacts_dir = directory.join("artifacts");
    reader.directory(&artifacts_dir)?;
    let mut artifacts = std::collections::BTreeMap::new();
    for (gate, cases) in [
        (&d.source_gate, &[][..]),
        (&d.application_unit, UNIT_CASES),
        (&d.application_contract, CONTRACT_CASES),
    ] {
        let bytes = validate_gate(&mut reader, &artifacts_dir, gate, cases)?;
        artifacts.insert(gate.output_sha256.clone(), bytes);
    }
    let bytes = reader.artifact(&artifacts_dir, &d.provider_boundary_sha256, MAX_GATE_OUTPUT)?;
    artifacts.insert(d.provider_boundary_sha256.clone(), bytes);
    reader.finish()?;
    Ok(VerifiedPrerequisites {
        descriptor: d,
        artifacts,
    })
}

/// Read the opaque auth generation without creating it or taking a Store lock.
/// `account` comes from a validated account row; the filesystem-only signature is
/// safe inside the Store's account-lease transaction. Missing legacy state is
/// unknown, while malformed or unsafe state remains an error.
pub(crate) fn read_generation(root: &Path, account: &Id) -> Result<Option<String>> {
    let mut reader = Reader::default();
    let accounts = root.join("accounts");
    let directory = accounts.join(account.as_str());
    for path in [root, accounts.as_path(), directory.as_path()] {
        reader.directory(path)?;
    }
    let bytes = match reader.read(&directory.join("application-generation.json"), 1024) {
        Ok(bytes) => bytes,
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            reader.finish()?;
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let record: CredentialGeneration = serde_json::from_slice(&bytes)?;
    require(record.version == 1 && &record.account == account && sha256(&record.generation))?;
    reader.finish()?;
    Ok(Some(record.generation))
}

/// Called only by trusted qualification after reserving this account. Existing
/// generation bytes are returned unchanged; legacy creation is a durable effect.
pub(crate) fn ensure_generation(
    store: &crate::store::Store,
    run: &crate::store::RunRecord,
) -> Result<CredentialGeneration> {
    generation(store, run, false)
}

/// Call BEFORE explicit auth replacement, within its exclusive account lease.
/// Same-account routine refresh must not call this. Any mutation/settlement
/// error retains the caller's run and durable generation receipt for inspection.
pub(crate) fn rotate_generation(
    store: &crate::store::Store,
    run: &crate::store::RunRecord,
) -> Result<CredentialGeneration> {
    generation(store, run, true)
}

fn generation(
    store: &crate::store::Store,
    run: &crate::store::RunRecord,
    rotate: bool,
) -> Result<CredentialGeneration> {
    store.verify_owned_run(run)?;
    let path = store
        .account_root(&run.account)?
        .join("application-generation.json");
    let previous = match private::read(&path, 1024) {
        Ok(bytes) => {
            let record: CredentialGeneration = serde_json::from_slice(&bytes)?;
            require(
                record.version == 1 && record.account == run.account && sha256(&record.generation),
            )?;
            if !rotate {
                store.verify_owned_run(run)?;
                return Ok(record);
            }
            Some(bytes)
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let mut random = [0u8; 32];
    // Read the OS CSPRNG without UUID format bits or a dependency/entropy fallback.
    File::open("/dev/urandom")?.read_exact(&mut random)?;
    let record = CredentialGeneration {
        version: 1,
        account: run.account.clone(),
        generation: hex::encode(random),
    };
    let bytes = serde_json::to_vec(&record)?;
    store.begin_tool(
        run,
        "xcb_application_generation",
        "host_auth_generation",
        &digest(&bytes),
    )?;
    store.verify_owned_run(run)?;
    match previous {
        Some(previous) => private::replace(&path, &bytes, &digest(previous))?,
        None => private::create(&path, &bytes)?,
    }
    store.settle_tool(run, "xcb_application_generation")?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn quota_availability_generation_reader_validates_parents_without_creation() {
        let temp = tempfile::tempdir().unwrap();
        let root = private::directory(&temp.path().canonicalize().unwrap().join("state")).unwrap();
        let accounts = private::directory(&root.join("accounts")).unwrap();
        let account = Id::new("synthetic").unwrap();
        let directory = private::directory(&accounts.join(account.as_str())).unwrap();
        assert_eq!(read_generation(&root, &account).unwrap(), None);
        assert!(!directory.join("application-generation.json").exists());
        let record = CredentialGeneration {
            version: 1,
            account: account.clone(),
            generation: "a".repeat(64),
        };
        private::create(
            &directory.join("application-generation.json"),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert_eq!(
            read_generation(&root, &account).unwrap(),
            Some(record.generation)
        );
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_generation(&root, &account).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let moved = accounts.join("moved");
        fs::rename(&directory, &moved).unwrap();
        symlink(&moved, &directory).unwrap();
        assert!(read_generation(&root, &account).is_err());
        fs::remove_file(&directory).unwrap();
        assert!(read_generation(&root, &account).is_err());
        assert!(!directory.exists());
    }

    const NOW: u64 = 1_900_000_000_000;

    // Synthetic fixtures exercise the reader only. They are deliberately not
    // signed/current Pins and cannot pass production load's Pin::verify check.
    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        directory: PathBuf,
        binding: Binding,
        pin: Pin,
        receipt: Receipt,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root =
                private::directory(&temp.path().canonicalize().unwrap().join("state")).unwrap();
            let account = Id::new("a_synthetic").unwrap();
            let account_dir =
                private::directory(&root.join("accounts").join(account.as_str())).unwrap();
            let binding = Binding {
                runtime_version: env!("CARGO_PKG_VERSION").into(),
                runtime_sha256: "1".repeat(64),
                provider: Provider::Claude,
                provider_version: "synthetic-provider-1".into(),
                provider_sha256: "2".repeat(64),
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                policy_sha256: "3".repeat(64),
                config_sha256: "4".repeat(64),
                account: account.clone(),
                credential_generation: "5".repeat(64),
                models: vec!["claude/synthetic-model".into()],
            };
            write_private(
                &account_dir.join("application-generation.json"),
                &serde_json::to_vec(&CredentialGeneration {
                    version: 1,
                    account,
                    generation: binding.credential_generation.clone(),
                })
                .unwrap(),
            );
            let directory = private::directory(
                &root
                    .join("qualification/application-v1")
                    .join(binding.account.as_str()),
            )
            .unwrap();
            private::directory(&directory.join("artifacts")).unwrap();
            let source = artifact(&directory, b"synthetic final source gate output\n");
            let unit = artifact(&directory, &test_output(UNIT_CASES));
            let contract = artifact(&directory, &test_output(CONTRACT_CASES));
            let native = artifact(
                &directory,
                br#"{"synthetic":"provider confinement artifact"}"#,
            );
            let binding_hash = digest(serde_json::to_vec(&binding).unwrap());
            let boundary = BoundaryEvidence {
                version: 1,
                binding_sha256: binding_hash.clone(),
                started_at_ms: NOW - 100,
                finished_at_ms: NOW - 50,
                source_gate: gate(source),
                application_unit: gate(unit),
                application_contract: gate(contract),
                provider_boundary_sha256: native,
            };
            let boundary_sha256 = artifact(&directory, &serde_json::to_vec(&boundary).unwrap());
            let nonce = "a".repeat(64);
            let live = LiveEvidence {
                version: 1,
                binding_sha256: binding_hash,
                started_at_ms: NOW - 40,
                finished_at_ms: NOW - 10,
                model: binding.models[0].clone(),
                nonce: nonce.clone(),
                response_json: json!({
                    "version":1,"status":"completed","requestId":"application_synthetic",
                    "account":binding.account,"model":binding.models[0],
                    "text":format!("xcb-application-v1:{nonce}"),
                    "outcome":{"terminal":"completed","joined":true,"effects":"none"}
                })
                .to_string(),
            };
            let live_hash = artifact(&directory, &serde_json::to_vec(&live).unwrap());
            let pin = Pin {
                provider: binding.provider,
                executable: root.join("missing-synthetic-provider"),
                sha256: binding.provider_sha256.clone(),
                version: binding.provider_version.clone(),
                host_sha256: binding.runtime_sha256.clone(),
                observed_at_ms: NOW - 100,
            };
            let receipt = Receipt {
                version: 1,
                binding: binding.clone(),
                observed_at_ms: NOW - 100,
                expires_at_ms: NOW + 1000,
                boundary_sha256,
                live_sha256: vec![live_hash],
            };
            let fixture = Self {
                _temp: temp,
                root,
                directory,
                binding,
                pin,
                receipt,
            };
            fixture.publish();
            fixture
        }
        fn expected(&self) -> Expected<'_> {
            Expected {
                pin: &self.pin,
                account: &self.binding.account,
                policy_sha256: &self.binding.policy_sha256,
                config_sha256: &self.binding.config_sha256,
                observed_models: &self.binding.models,
            }
        }
        fn read(&self) -> Result<Admission> {
            load_verified(&self.root, &self.expected(), NOW)
        }
        fn path(&self, hash: &str) -> PathBuf {
            self.directory
                .join("artifacts")
                .join(format!("{hash}.json"))
        }
        fn publish(&self) {
            write_private(
                &self.directory.join("receipt.json"),
                &serde_json::to_vec(&self.receipt).unwrap(),
            );
        }
        fn boundary(&mut self, change: impl FnOnce(&mut BoundaryEvidence)) {
            let mut evidence = serde_json::from_slice(
                &fs::read(self.path(&self.receipt.boundary_sha256)).unwrap(),
            )
            .unwrap();
            change(&mut evidence);
            self.receipt.boundary_sha256 =
                artifact(&self.directory, &serde_json::to_vec(&evidence).unwrap());
            self.publish();
        }
        fn live(&mut self, change: impl FnOnce(&mut LiveEvidence)) {
            let mut evidence =
                serde_json::from_slice(&fs::read(self.path(&self.receipt.live_sha256[0])).unwrap())
                    .unwrap();
            change(&mut evidence);
            self.receipt.live_sha256[0] =
                artifact(&self.directory, &serde_json::to_vec(&evidence).unwrap());
            self.publish();
        }
    }
    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn artifact(directory: &Path, bytes: &[u8]) -> String {
        let hash = digest(bytes);
        write_private(
            &directory.join("artifacts").join(format!("{hash}.json")),
            bytes,
        );
        hash
    }
    fn gate(output_sha256: String) -> GateArtifact {
        GateArtifact {
            argv: vec!["cargo".into(), "test".into(), "--locked".into()],
            exit_code: 0,
            output_sha256,
        }
    }
    fn test_output(cases: &[&str]) -> Vec<u8> {
        let mut output = cases
            .iter()
            .map(|name| format!("test {name} ... ok\n"))
            .collect::<String>();
        output.push_str(&format!(
            "test result: ok. {} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
            cases.len()
        ));
        output.into_bytes()
    }

    #[test]
    fn synthetic_receipt_is_read_only_and_exposes_only_four_fixed_public_fields() {
        let fixture = Fixture::new();
        let path = fixture.directory.join("receipt.json");
        let before = FileIdentity::from(&fs::metadata(&path).unwrap());
        let bytes = fs::read(&path).unwrap();
        let admitted = fixture.read().unwrap();
        assert!(admitted.covers("claude/synthetic-model"));
        assert!(!admitted.covers("claude/synthetic-model/high"));
        let public = serde_json::to_value(admitted.public()).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 4);
        assert_eq!(public["runtimeDigest"], fixture.binding.runtime_sha256);
        assert_eq!(public["evidenceDigest"], digest(&bytes));
        assert_eq!(public["runtimeVersion"], env!("CARGO_PKG_VERSION"));
        assert_eq!(public["expiresAt"], NOW + 1000);
        assert_eq!(
            serde_json::to_value(fixture.read().unwrap().public()).unwrap(),
            public
        );
        assert!(FileIdentity::from(&fs::metadata(path).unwrap()) == before);
        // Production admission still requires actual verified host/provider bytes.
        assert!(load(&fixture.root, &fixture.expected(), NOW).is_err());
    }

    #[test]
    fn every_private_binding_and_full_model_coverage_is_required() {
        let fixture = Fixture::new();
        let original = serde_json::to_value(&fixture.receipt.binding).unwrap();
        for (field, value) in [
            ("runtime_version", json!("different")),
            ("runtime_sha256", json!("b".repeat(64))),
            ("provider", json!("codex")),
            ("provider_version", json!("different")),
            ("provider_sha256", json!("b".repeat(64))),
            ("os", json!("different")),
            ("arch", json!("different")),
            ("policy_sha256", json!("b".repeat(64))),
            ("config_sha256", json!("b".repeat(64))),
            ("account", json!("a_other")),
            ("credential_generation", json!("b".repeat(64))),
            ("models", json!([])),
            ("models", json!(["claude/synthetic-model/high"])),
            (
                "models",
                json!(["claude/synthetic-model", "claude/synthetic-model"]),
            ),
        ] {
            let mut value_binding = original.clone();
            value_binding[field] = value;
            let binding: Binding = serde_json::from_value(value_binding).unwrap();
            assert!(
                validate_binding(
                    &binding,
                    &fixture.expected(),
                    &fixture.binding.credential_generation
                )
                .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn expiry_is_fixed_and_future_or_overlong_evidence_is_rejected() {
        for (start, end) in [
            (NOW + 1, NOW + 1000),
            (NOW - 100, NOW),
            (0, NOW + 1000),
            (NOW - 100, NOW - 100 + MAX_AGE_MS + 1),
        ] {
            let mut fixture = Fixture::new();
            fixture.receipt.observed_at_ms = start;
            fixture.receipt.expires_at_ms = end;
            fixture.publish();
            assert!(fixture.read().is_err());
        }
        let mut fixture = Fixture::new();
        fixture.live(|live| live.finished_at_ms = NOW + 1);
        assert!(fixture.read().is_err());
    }

    #[test]
    fn raw_focused_outputs_require_all_cases_with_no_failed_or_ignored_substitutes() {
        for replacement in [
            "",
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n",
        ] {
            let mut fixture = Fixture::new();
            let hash = artifact(&fixture.directory, replacement.as_bytes());
            fixture.boundary(|boundary| boundary.application_unit.output_sha256 = hash);
            assert!(fixture.read().is_err());
        }
        for replacement in ["ignored", "FAILED"] {
            let mut fixture = Fixture::new();
            let output = String::from_utf8(test_output(UNIT_CASES))
                .unwrap()
                .replacen("... ok", &format!("... {replacement}"), 1);
            let hash = artifact(&fixture.directory, output.as_bytes());
            fixture.boundary(|boundary| boundary.application_unit.output_sha256 = hash);
            assert!(fixture.read().is_err());
        }
        let mut fixture = Fixture::new();
        fixture.boundary(|boundary| boundary.source_gate.exit_code = 1);
        assert!(fixture.read().is_err());
    }

    #[test]
    fn joined_response_must_match_exact_challenge_account_and_full_model() {
        for (field, value) in [
            ("text", json!("different answer")),
            ("account", json!("a_other")),
            ("model", json!("claude/synthetic-model/high")),
            ("status", json!("failed")),
            ("requestId", json!("../bad")),
            ("extra", json!(true)),
            (
                "outcome",
                json!({"terminal":"completed","joined":false,"effects":"none"}),
            ),
            (
                "outcome",
                json!({"terminal":"completed","joined":true,"effects":"settled"}),
            ),
        ] {
            let mut fixture = Fixture::new();
            fixture.live(|live| {
                let mut response: Value = serde_json::from_str(&live.response_json).unwrap();
                response[field] = value;
                live.response_json = response.to_string();
            });
            assert!(fixture.read().is_err(), "{field}");
        }
        let mut fixture = Fixture::new();
        fixture.receipt.live_sha256.clear();
        fixture.publish();
        assert!(fixture.read().is_err());
    }

    #[test]
    fn missing_evidence_changed_bytes_unknown_fields_and_path_injection_fail_closed() {
        let mut fixture = Fixture::new();
        fixture.receipt.boundary_sha256 = "../receipt".into();
        fixture.publish();
        assert!(fixture.read().is_err());
        let fixture = Fixture::new();
        fs::write(fixture.path(&fixture.receipt.live_sha256[0]), b"tampered").unwrap();
        assert!(fixture.read().is_err());
        let fixture = Fixture::new();
        fs::remove_file(fixture.path(&fixture.receipt.boundary_sha256)).unwrap();
        assert!(fixture.read().is_err());
        let fixture = Fixture::new();
        let mut receipt = serde_json::to_value(&fixture.receipt).unwrap();
        receipt["available"] = json!(true);
        write_private(
            &fixture.directory.join("receipt.json"),
            &serde_json::to_vec(&receipt).unwrap(),
        );
        assert!(fixture.read().is_err());
        write_private(
            &fixture.directory.join("receipt.json"),
            &vec![b' '; MAX_RECEIPT + 1],
        );
        assert!(fixture.read().is_err());
    }

    #[test]
    fn receipt_symlinks_hardlinks_and_nonprivate_modes_are_rejected() {
        for kind in ["symlink", "hardlink", "mode"] {
            let fixture = Fixture::new();
            let path = fixture.directory.join("receipt.json");
            let other = fixture.directory.join("other.json");
            match kind {
                "symlink" => {
                    fs::rename(&path, &other).unwrap();
                    symlink(&other, &path).unwrap();
                }
                "hardlink" => {
                    fs::hard_link(&path, &other).unwrap();
                }
                _ => fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap(),
            }
            assert!(fixture.read().is_err(), "{kind}");
        }
    }

    #[test]
    fn missing_or_rotated_generation_revokes_without_creating_state() {
        let fixture = Fixture::new();
        let path = fixture
            .root
            .join("accounts")
            .join(fixture.binding.account.as_str())
            .join("application-generation.json");
        let generation = CredentialGeneration {
            version: 1,
            account: fixture.binding.account.clone(),
            generation: "c".repeat(64),
        };
        write_private(&path, &serde_json::to_vec(&generation).unwrap());
        assert!(fixture.read().is_err());
        fs::remove_file(&path).unwrap();
        assert!(fixture.read().is_err());
        assert!(!path.exists());
        let missing = fixture.root.join("absent");
        assert!(load_verified(&missing, &fixture.expected(), NOW).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn final_read_check_rejects_atomic_replacement_even_with_identical_bytes() {
        let fixture = Fixture::new();
        let path = fixture.directory.join("receipt.json");
        let mut reader = Reader::default();
        let bytes = reader.read(&path, MAX_RECEIPT).unwrap();
        let replacement = fixture.directory.join("replacement.json");
        write_private(&replacement, &bytes);
        fs::rename(replacement, path).unwrap();
        assert!(reader.finish().is_err());
        let mut reader = Reader::default();
        reader.directory(&fixture.directory).unwrap();
        let moved = fixture.directory.with_extension("moved");
        fs::rename(&fixture.directory, moved).unwrap();
        private::directory(&fixture.directory).unwrap();
        assert!(reader.finish().is_err());
    }
    #[test]
    fn prerequisite_verifier_checks_actual_outputs_before_any_live_challenge() {
        let fixture = Fixture::new();
        let boundary: BoundaryEvidence = serde_json::from_slice(
            &fs::read(fixture.path(&fixture.receipt.boundary_sha256)).unwrap(),
        )
        .unwrap();
        let d = PrerequisiteDescriptor {
            version: 1,
            runtime_version: fixture.binding.runtime_version.clone(),
            runtime_sha256: fixture.binding.runtime_sha256.clone(),
            provider: fixture.binding.provider,
            provider_version: fixture.binding.provider_version.clone(),
            provider_sha256: fixture.binding.provider_sha256.clone(),
            os: fixture.binding.os.clone(),
            arch: fixture.binding.arch.clone(),
            policy_sha256: fixture.binding.policy_sha256.clone(),
            config_sha256: fixture.binding.config_sha256.clone(),
            started_at_ms: boundary.started_at_ms,
            finished_at_ms: boundary.finished_at_ms,
            source_gate: boundary.source_gate,
            application_unit: boundary.application_unit,
            application_contract: boundary.application_contract,
            provider_boundary_sha256: boundary.provider_boundary_sha256,
        };
        let path = fixture.directory.join("prerequisites.json");
        write_private(&path, &serde_json::to_vec(&d).unwrap());
        let proof =
            verify_prerequisites_verified(&fixture.directory, &fixture.expected(), NOW).unwrap();
        assert_eq!(proof.observed_at_ms(), NOW - 100);
        assert_eq!(proof.artifacts().count(), 4);
        assert!(proof.artifacts().all(|(hash, bytes)| digest(bytes) == hash));
        assert!(proof.boundary(&fixture.binding).is_ok());
        let mut changed = fixture.binding.clone();
        changed.policy_sha256 = "f".repeat(64);
        assert!(proof.boundary(&changed).is_err());
        assert!(verify_prerequisites(&fixture.directory, &fixture.expected(), NOW).is_err());
        write_private(
            &fixture.path(&d.application_unit.output_sha256),
            b"test result: ok. 0 passed; 0 failed;\n",
        );
        assert!(
            verify_prerequisites_verified(&fixture.directory, &fixture.expected(), NOW).is_err()
        );
    }
    #[test]
    fn generation_creation_is_explicit_owned_and_rotation_revokes_old_identity() {
        let temp = tempfile::tempdir().unwrap();
        let store =
            crate::store::Store::open(&temp.path().canonicalize().unwrap().join("state")).unwrap();
        let account = store
            .add_account(Provider::Claude, "Synthetic", "Synthetic", NOW)
            .unwrap();
        let path = store
            .account_root(&account.id)
            .unwrap()
            .join("application-generation.json");
        assert!(!path.exists());
        let run = store.prepare_probe(&account.id, None, NOW).unwrap();
        let record = ensure_generation(&store, &run).unwrap();
        assert!(sha256(&record.generation));
        assert_eq!(record.account, account.id);
        let bytes = fs::read(&path).unwrap();
        let identity = FileIdentity::from(&fs::metadata(&path).unwrap());
        assert_eq!(
            ensure_generation(&store, &run).unwrap().generation,
            record.generation
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(FileIdentity::from(&fs::metadata(&path).unwrap()) == identity);
        let other = crate::store::Store::open(store.root()).unwrap();
        assert!(ensure_generation(&other, &run).is_err());
        assert!(rotate_generation(&other, &run).is_err());
        let mut wrong = run.clone();
        wrong.account = Id::new("a_other").unwrap();
        assert!(rotate_generation(&store, &wrong).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        store
            .settle(&run, xcb_core::session::State::Idle, NOW)
            .unwrap();
        assert!(ensure_generation(&store, &run).is_err());
        assert!(rotate_generation(&store, &run).is_err());
        let new_run = store.prepare_probe(&account.id, None, NOW).unwrap();
        let next = rotate_generation(&store, &new_run).unwrap();
        assert_ne!(next.generation, record.generation);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        store
            .settle(&new_run, xcb_core::session::State::Idle, NOW)
            .unwrap();
    }
}
