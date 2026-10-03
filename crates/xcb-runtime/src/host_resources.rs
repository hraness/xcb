//! Bounded host observations and conservative admission decisions.
//!
//! This module never signals processes or removes files. Keep at most one
//! `collect` future in flight: filesystem calls run off the async executor,
//! but an unavailable filesystem can leave that worker pending. Freshness
//! checks then stop new work without accumulating replacement workers.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

const GIB: u64 = 1024 * 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
pub const MAX_PATHS: usize = 32;
const MAX_PATH_BYTES: usize = 1024;
const MAX_ERRORS: usize = 32;
const MAX_ERROR_BYTES: usize = 256;
const MAX_HISTORY: usize = 60;
// The supervisor considers at most 128 active tasks, plus its own state root.
// Retain paused paths across rotating 32-path samples without an unbounded map.
const MAX_TRACKED_PATHS: usize = 129;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ResourcePolicy {
    pub enabled: bool,
    pub sample_interval_secs: u64,
    pub stale_after_secs: u64,
    pub warn_disk_bytes: u64,
    pub pause_disk_bytes: u64,
    pub resume_disk_bytes: u64,
    pub pressure_sustain_secs: u64,
    pub recovery_samples: u32,
}

impl Default for ResourcePolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_interval_secs: 30,
            stale_after_secs: 120,
            warn_disk_bytes: 60 * GIB,
            pause_disk_bytes: 24 * GIB,
            resume_disk_bytes: 32 * GIB,
            pressure_sustain_secs: 60,
            recovery_samples: 3,
        }
    }
}

impl ResourcePolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(10..=300).contains(&self.sample_interval_secs)
            || self.stale_after_secs < self.sample_interval_secs.saturating_mul(2)
            || self.stale_after_secs > 900
        {
            return Err("resource sampling must be 10–300 seconds and freshness 2 sampling intervals–900 seconds".into());
        }
        if self.pause_disk_bytes == 0
            || self.pause_disk_bytes >= self.resume_disk_bytes
            || self.resume_disk_bytes > self.warn_disk_bytes
        {
            return Err("resource disk thresholds must satisfy 0 < pause < resume <= warn".into());
        }
        if !(10..=900).contains(&self.pressure_sustain_secs)
            || !(2..=10).contains(&self.recovery_samples)
        {
            return Err(
                "resource pressure duration must be 10–900 seconds and recovery 2–10 samples"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemorySnapshot {
    pub pressure: MemoryPressure,
    pub physical_total_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DiskSnapshot {
    /// The requested path, retained even when another path shares its volume.
    pub path: PathBuf,
    pub volume_id: Option<u64>,
    pub free_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u32,
    /// Start of sampling, so a slow sampler cannot make old data look fresh.
    pub at_ms: u64,
    pub state_root: PathBuf,
    pub memory: MemorySnapshot,
    pub disks: Vec<DiskSnapshot>,
    pub errors: Vec<String>,
}

impl Snapshot {
    /// Call after a byte-bounded read, before accepting retained observations.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.at_ms == 0 {
            return Err("unsupported resource snapshot schema or timestamp".into());
        }
        if !valid_path(&self.state_root)
            || self.disks.is_empty()
            || self.disks.len() > MAX_PATHS
            || self.disks[0].path != self.state_root
            || self.errors.len() > MAX_ERRORS
            || self
                .errors
                .iter()
                .any(|error| error.len() > MAX_ERROR_BYTES)
        {
            return Err("resource snapshot exceeds bounds or lacks its state volume".into());
        }
        for (index, disk) in self.disks.iter().enumerate() {
            if !valid_path(&disk.path)
                || self.disks[..index]
                    .iter()
                    .any(|other| other.path == disk.path)
                || disk
                    .error
                    .as_ref()
                    .is_some_and(|error| error.len() > MAX_ERROR_BYTES)
            {
                return Err("resource disk entry is duplicate or exceeds bounds".into());
            }
            match (disk.free_bytes, disk.total_bytes, &disk.error) {
                (Some(free), Some(total), None) if total > 0 && free <= total => {}
                (None, None, Some(_)) => {}
                _ => return Err("resource disk entry has inconsistent measurements".into()),
            }
        }
        if serde_json::to_vec(self)
            .map_err(|_| "resource snapshot serialization failed")?
            .len()
            > MAX_SNAPSHOT_BYTES
        {
            return Err("resource snapshot exceeds byte limit".into());
        }
        Ok(())
    }
}

fn valid_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|text| {
            !text.is_empty() && text.len() <= MAX_PATH_BYTES && !text.chars().any(char::is_control)
        })
}

pub const fn supported_platform() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

/// Observe only bounded, explicitly supplied paths. Callers must retain a
/// single pending sampler and use `Monitor::assess` to detect a stalled one.
pub async fn collect(state_root: &Path, workspaces: &[PathBuf]) -> Snapshot {
    let at_ms = timestamp_ms();
    let mut paths = Vec::with_capacity(MAX_PATHS);
    let mut errors = Vec::new();
    if workspaces.len() >= MAX_PATHS {
        push_error(
            &mut errors,
            "resource path limit reached; omitted workspaces remain blocked",
        );
    }
    for path in std::iter::once(state_root)
        .chain(workspaces.iter().take(MAX_PATHS - 1).map(PathBuf::as_path))
    {
        if !valid_path(path) {
            push_error(
                &mut errors,
                "resource path must be absolute UTF-8 and at most 1024 bytes",
            );
            continue;
        }
        if paths.iter().any(|known| known == path) {
            continue;
        }
        if paths.len() == MAX_PATHS {
            push_error(
                &mut errors,
                "resource path limit reached; omitted workspaces remain blocked",
            );
            break;
        }
        paths.push(path.to_path_buf());
    }
    let (memory, disks) = tokio::join!(
        collect_memory(),
        tokio::task::spawn_blocking(move || collect_disks(&paths))
    );
    let (memory, memory_errors) = memory;
    for error in memory_errors {
        push_error(&mut errors, &error);
    }
    let disks = match disks {
        Ok(disks) => disks,
        Err(_) => {
            push_error(&mut errors, "disk sampler did not complete");
            Vec::new()
        }
    };
    Snapshot {
        schema_version: 1,
        at_ms,
        state_root: state_root.to_path_buf(),
        memory,
        disks,
        errors,
    }
}

fn push_error(errors: &mut Vec<String>, message: &str) {
    if errors.len() < MAX_ERRORS && !errors.iter().any(|existing| existing == message) {
        let mut end = message.len().min(MAX_ERROR_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        errors.push(message[..end].to_owned());
    }
}

#[cfg(unix)]
fn collect_disks(paths: &[PathBuf]) -> Vec<DiskSnapshot> {
    use std::os::unix::fs::MetadataExt;
    let mut volumes = BTreeMap::<u64, Result<(u64, u64), String>>::new();
    paths
        .iter()
        .map(|path| {
            let mut disk = DiskSnapshot {
                path: path.clone(),
                volume_id: None,
                free_bytes: None,
                total_bytes: None,
                error: None,
            };
            match std::fs::metadata(path) {
                Ok(metadata) if metadata.is_dir() => {
                    let device = metadata.dev();
                    disk.volume_id = Some(device);
                    let measurement = volumes.entry(device).or_insert_with(|| {
                        let status = rustix::fs::statvfs(path)
                            .map_err(|_| "volume capacity query failed".to_string())?;
                        let unit = status.f_frsize;
                        let total = status
                            .f_blocks
                            .checked_mul(unit)
                            .ok_or("volume capacity overflow")?;
                        let free = status
                            .f_bavail
                            .checked_mul(unit)
                            .ok_or("volume free space overflow")?;
                        if total == 0 || free > total {
                            return Err("volume capacity query returned invalid values".into());
                        }
                        Ok((free, total))
                    });
                    match measurement {
                        Ok((free, total)) => {
                            disk.free_bytes = Some(*free);
                            disk.total_bytes = Some(*total);
                        }
                        Err(error) => disk.error = Some(error.clone()),
                    }
                }
                Ok(_) => disk.error = Some("resource path is not a directory".into()),
                Err(_) => disk.error = Some("resource directory is unavailable".into()),
            }
            disk
        })
        .collect()
}

#[cfg(not(unix))]
fn collect_disks(paths: &[PathBuf]) -> Vec<DiskSnapshot> {
    paths
        .iter()
        .map(|path| DiskSnapshot {
            path: path.clone(),
            volume_id: None,
            free_bytes: None,
            total_bytes: None,
            error: Some("disk observations are unavailable on this platform".into()),
        })
        .collect()
}

#[cfg(target_os = "macos")]
async fn collect_memory() -> (MemorySnapshot, Vec<String>) {
    let (pressure, physical, swap) = tokio::join!(
        sysctl("kern.memorystatus_vm_pressure_level"),
        sysctl("hw.memsize"),
        sysctl("vm.swapusage"),
    );
    let mut errors = Vec::new();
    let pressure = pressure
        .ok()
        .as_deref()
        .map(parse_macos_pressure)
        .unwrap_or_default();
    if pressure == MemoryPressure::Unknown {
        push_error(&mut errors, "macOS memory pressure observation unavailable");
    }
    let physical_total_bytes = physical
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0);
    if physical_total_bytes.is_none() {
        push_error(&mut errors, "physical memory observation unavailable");
    }
    let swap_used_bytes = swap.ok().as_deref().and_then(parse_macos_swap);
    if swap_used_bytes.is_none() {
        push_error(&mut errors, "swap usage observation unavailable");
    }
    (
        MemorySnapshot {
            pressure,
            physical_total_bytes,
            swap_used_bytes,
        },
        errors,
    )
}

#[cfg(target_os = "macos")]
async fn sysctl(key: &str) -> Result<String, ()> {
    use std::{process::Stdio, time::Duration};
    use tokio::{io::AsyncReadExt, process::Command};
    const MAX_OUTPUT: usize = 8192;
    let mut child = Command::new("/usr/sbin/sysctl")
        .args(["-n", key])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let stdout = child.stdout.take().ok_or(())?;
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        stdout
            .take((MAX_OUTPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ())?;
        if bytes.len() > MAX_OUTPUT || !child.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        String::from_utf8(bytes).map_err(|_| ())
    })
    .await;
    if let Ok(Ok(value)) = result {
        return Ok(value);
    }
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    Err(())
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_pressure(value: &str) -> MemoryPressure {
    match value.trim() {
        "1" => MemoryPressure::Normal,
        "2" => MemoryPressure::Warning,
        "4" => MemoryPressure::Critical,
        _ => MemoryPressure::Unknown,
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_macos_swap(value: &str) -> Option<u64> {
    let fields: Vec<_> = value.split_whitespace().take(32).collect();
    let index = fields.iter().position(|field| *field == "used")?;
    if fields.get(index + 1)? != &"=" {
        return None;
    }
    let amount = fields.get(index + 2)?;
    let split = amount.len().checked_sub(1)?;
    let multiplier = match amount.get(split..)? {
        "K" => 1024.0,
        "M" => 1024.0 * 1024.0,
        "G" => GIB as f64,
        _ => return None,
    };
    let quantity: f64 = amount.get(..split)?.parse().ok()?;
    let bytes = quantity * multiplier;
    (quantity.is_finite() && bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64)
        .then_some(bytes as u64)
}

#[cfg(target_os = "linux")]
async fn collect_memory() -> (MemorySnapshot, Vec<String>) {
    tokio::task::spawn_blocking(|| {
        use std::io::Read;
        fn read_bounded(path: &str) -> Option<String> {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(65537)
                .read_to_end(&mut bytes)
                .ok()?;
            if bytes.len() > 65536 {
                return None;
            }
            String::from_utf8(bytes).ok()
        }
        let meminfo = read_bounded("/proc/meminfo");
        let psi = read_bounded("/proc/pressure/memory");
        let memory = parse_linux_memory(meminfo.as_deref(), psi.as_deref());
        let errors = if memory.pressure == MemoryPressure::Unknown {
            vec!["Linux memory pressure observation unavailable".into()]
        } else {
            Vec::new()
        };
        (memory, errors)
    })
    .await
    .unwrap_or_else(|_| {
        (
            MemorySnapshot::default(),
            vec!["memory sampler did not complete".into()],
        )
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_memory(meminfo: Option<&str>, psi: Option<&str>) -> MemorySnapshot {
    let field = |key: &str| -> Option<u64> {
        let line = meminfo?.lines().find(|line| line.starts_with(key))?;
        let mut fields = line.split_whitespace();
        if fields.next()? != key {
            return None;
        }
        let value: u64 = fields.next()?.parse().ok()?;
        if fields.next()? != "kB" {
            return None;
        }
        value.checked_mul(1024)
    };
    let physical_total_bytes = field("MemTotal:").filter(|value| *value > 0);
    let available = field("MemAvailable:");
    let swap_used_bytes = field("SwapTotal:")
        .zip(field("SwapFree:"))
        .and_then(|(total, free)| total.checked_sub(free));
    let full = psi
        .and_then(|text| text.lines().find(|line| line.starts_with("full ")))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("avg10="))
        })
        .and_then(|number| number.parse::<f64>().ok())
        .filter(|number| number.is_finite() && (0.0..=100.0).contains(number));
    let capacity_pressure = physical_total_bytes
        .zip(available)
        .filter(|(total, free)| free <= total)
        .map(|(total, free)| {
            let ratio = (free as f64) / (total as f64);
            if ratio <= 0.05 {
                MemoryPressure::Critical
            } else if ratio <= 0.10 {
                MemoryPressure::Warning
            } else {
                MemoryPressure::Normal
            }
        });
    let pressure = match full {
        Some(value) if value >= 10.0 => MemoryPressure::Critical,
        Some(value) if value >= 1.0 => {
            if capacity_pressure == Some(MemoryPressure::Critical) {
                MemoryPressure::Critical
            } else {
                MemoryPressure::Warning
            }
        }
        // Even zero observed stalls should not admit new work at nearly zero
        // available memory; use the more conservative of the two readings.
        Some(_) => capacity_pressure.unwrap_or(MemoryPressure::Normal),
        None => capacity_pressure.unwrap_or_default(),
    };
    MemorySnapshot {
        pressure,
        physical_total_bytes,
        swap_used_bytes,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn collect_memory() -> (MemorySnapshot, Vec<String>) {
    (
        MemorySnapshot::default(),
        vec!["memory observations are unavailable on this platform".into()],
    )
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Assessment {
    pub blocked: bool,
    pub reasons: Vec<String>,
    pub advisories: Vec<String>,
}

#[derive(Default)]
struct DiskState {
    paused: bool,
    recovery_samples: u32,
    last_seen_at_ms: u64,
}

#[derive(Default)]
pub struct Monitor {
    snapshot: Option<Snapshot>,
    invalid_snapshot: bool,
    evaluated_at_ms: Option<u64>,
    pressure_since_ms: Option<u64>,
    memory_paused: bool,
    memory_recovery_samples: u32,
    disks: BTreeMap<PathBuf, DiskState>,
    swap_history: VecDeque<(u64, u64)>,
}

impl Monitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn observe(&mut self, snapshot: Snapshot) {
        if snapshot.validate().is_err() {
            self.sampling_failed();
            return;
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|previous| snapshot.at_ms <= previous.at_ms)
        {
            return;
        }
        self.invalid_snapshot = false;
        self.snapshot = Some(snapshot);
    }

    /// Discard stale good news after a sampler failure. Existing pause state
    /// survives so failures can never count toward recovery.
    pub fn sampling_failed(&mut self) {
        self.snapshot = None;
        self.invalid_snapshot = true;
        self.reset_continuity();
    }

    fn reset_continuity(&mut self) {
        self.pressure_since_ms = None;
        self.memory_recovery_samples = 0;
        for disk in self.disks.values_mut() {
            disk.recovery_samples = 0;
        }
    }

    pub fn assess(&mut self, policy: &ResourcePolicy, workspace: &Path, now_ms: u64) -> Assessment {
        let mut assessment = Assessment::default();
        if !policy.enabled {
            return assessment;
        }
        if let Err(error) = policy.validate() {
            assessment.reasons.push(error);
        } else if self.invalid_snapshot {
            assessment
                .reasons
                .push("resource observation failed collection or validation".into());
        } else if let Some(snapshot) = &self.snapshot {
            if snapshot.at_ms > now_ms
                || now_ms.saturating_sub(snapshot.at_ms) > policy.stale_after_secs * 1000
            {
                assessment
                    .reasons
                    .push("resource observations are stale; waiting for the sampler".into());
                self.reset_continuity();
            } else {
                self.advance(policy);
                let snapshot = self.snapshot.as_ref().expect("snapshot remains present");
                if snapshot.memory.pressure == MemoryPressure::Unknown {
                    assessment
                        .reasons
                        .push("memory pressure observation is unavailable".into());
                } else if self.memory_paused {
                    assessment
                        .reasons
                        .push("sustained memory pressure; waiting for recovery samples".into());
                }
                if matches!(
                    snapshot.memory.pressure,
                    MemoryPressure::Warning | MemoryPressure::Critical
                ) {
                    assessment.advisories.push(
                        format!("memory pressure is {:?}", snapshot.memory.pressure).to_lowercase(),
                    );
                }
                let mut required = vec![snapshot.state_root.as_path()];
                if workspace != snapshot.state_root {
                    required.push(workspace);
                }
                for path in required {
                    match snapshot.disks.iter().find(|disk| disk.path == path) {
                        Some(disk) if disk.error.is_none() && disk.free_bytes.is_some() => {
                            match self.disks.get(path) {
                                Some(state) if state.paused => {
                                    assessment.reasons.push(format!(
                                        "disk reserve is low or recovering: {}",
                                        path.display()
                                    ));
                                }
                                None => assessment.reasons.push(format!(
                                    "disk recovery history is full; waiting for paused paths to recover: {}",
                                    path.display()
                                )),
                                _ => {}
                            }
                            if disk
                                .free_bytes
                                .is_some_and(|free| free <= policy.warn_disk_bytes)
                            {
                                assessment.advisories.push(format!(
                                    "disk free space is below the warning reserve: {}",
                                    path.display()
                                ));
                            }
                        }
                        _ => assessment
                            .reasons
                            .push(format!("disk observation unavailable: {}", path.display())),
                    }
                }
                if self
                    .swap_history
                    .front()
                    .zip(self.swap_history.back())
                    .is_some_and(|((_, first), (_, last))| last.saturating_sub(*first) >= GIB)
                {
                    assessment.advisories.push(
                        "swap use increased by at least 1 GiB within the last 10 minutes".into(),
                    );
                }
                assessment
                    .advisories
                    .extend(snapshot.errors.iter().cloned());
            }
        } else {
            assessment
                .reasons
                .push("waiting for the first resource observation".into());
        }
        assessment.blocked = !assessment.reasons.is_empty();
        assessment
    }

    fn advance(&mut self, policy: &ResourcePolicy) {
        let snapshot = self.snapshot.as_ref().expect("checked by assess");
        let at_ms = snapshot.at_ms;
        if self
            .evaluated_at_ms
            .is_some_and(|previous| at_ms <= previous)
        {
            return;
        }
        if self
            .evaluated_at_ms
            .is_some_and(|previous| at_ms.saturating_sub(previous) > policy.stale_after_secs * 1000)
        {
            self.reset_continuity();
        }
        let snapshot = self.snapshot.as_ref().expect("snapshot remains present");
        self.evaluated_at_ms = Some(at_ms);
        match snapshot.memory.pressure {
            MemoryPressure::Warning | MemoryPressure::Critical => {
                self.memory_recovery_samples = 0;
                let since = self.pressure_since_ms.get_or_insert(at_ms);
                if at_ms.saturating_sub(*since) >= policy.pressure_sustain_secs * 1000 {
                    self.memory_paused = true;
                }
            }
            MemoryPressure::Normal => {
                self.pressure_since_ms = None;
                self.memory_recovery_samples = self.memory_recovery_samples.saturating_add(1);
                if self.memory_recovery_samples >= policy.recovery_samples {
                    self.memory_paused = false;
                }
            }
            MemoryPressure::Unknown => {
                self.pressure_since_ms = None;
                self.memory_recovery_samples = 0;
            }
        }
        for disk in &snapshot.disks {
            if !self.disks.contains_key(&disk.path) && self.disks.len() >= MAX_TRACKED_PATHS {
                let evict = self
                    .disks
                    .iter()
                    .filter(|(path, state)| {
                        !state.paused
                            && !snapshot
                                .disks
                                .iter()
                                .any(|disk| disk.path.as_path() == path.as_path())
                    })
                    .min_by_key(|(_, state)| state.last_seen_at_ms)
                    .map(|(path, _)| path.clone());
                if let Some(path) = evict {
                    self.disks.remove(&path);
                } else {
                    // Never let rotation erase a pause. Until an old paused
                    // path recovers, new paths cannot be tracked or admitted.
                    continue;
                }
            }
            let state = self
                .disks
                .entry(disk.path.clone())
                .or_insert_with(|| DiskState {
                    // An unfamiliar path starts above the recovery reserve;
                    // a known healthy path still pauses at the lower threshold.
                    paused: disk
                        .free_bytes
                        .is_none_or(|free| free <= policy.resume_disk_bytes),
                    recovery_samples: 0,
                    last_seen_at_ms: at_ms,
                });
            // Allow a complete bounded workspace rotation, but never combine
            // recovery evidence across a long absence of that workspace.
            let rotation_secs =
                policy.sample_interval_secs * MAX_TRACKED_PATHS.div_ceil(MAX_PATHS - 1) as u64;
            if at_ms.saturating_sub(state.last_seen_at_ms)
                > policy.stale_after_secs.max(rotation_secs) * 1000
            {
                state.recovery_samples = 0;
            }
            state.last_seen_at_ms = at_ms;
            match disk.free_bytes.filter(|_| disk.error.is_none()) {
                Some(free) if free <= policy.pause_disk_bytes => {
                    state.paused = true;
                    state.recovery_samples = 0;
                }
                Some(free) if free > policy.resume_disk_bytes => {
                    state.recovery_samples = state.recovery_samples.saturating_add(1);
                    if state.recovery_samples >= policy.recovery_samples {
                        state.paused = false;
                    }
                }
                _ => state.recovery_samples = 0,
            }
        }
        if let Some(swap) = snapshot.memory.swap_used_bytes {
            self.swap_history.push_back((at_ms, swap));
        }
        while self.swap_history.len() > MAX_HISTORY
            || self
                .swap_history
                .front()
                .is_some_and(|(at, _)| at_ms.saturating_sub(*at) > 600_000)
        {
            self.swap_history.pop_front();
        }
    }
}

#[cfg(test)]
#[path = "host_resources_tests.rs"]
mod tests;
