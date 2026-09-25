//! Opt-in login startup for an exact native habitat state root.
//! Service removal never signals an active managed supervisor.
use crate::{Error, Result, digest, private};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const RECORD: &str = "habitat-service.json";
const LIMIT: usize = 16 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub version: u32,
    pub label: String,
    pub state: PathBuf,
    pub executable: PathBuf,
    pub home: PathBuf,
    pub manifest: PathBuf,
    #[serde(default)]
    pub coordination_root: Option<PathBuf>,
}

#[derive(Serialize)]
pub struct Status {
    pub installed: bool,
    pub registered: bool,
    pub supervisor_running: bool,
    pub service: Option<Service>,
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

impl Service {
    pub fn plan(root: &Path, executable: &Path, home: &Path) -> Result<Self> {
        let state = root.canonicalize()?;
        private::check_directory(&state)?;
        let executable = executable.canonicalize()?;
        if !executable.is_file() {
            return Err(Error::PrivateState);
        }
        let home = home.canonicalize()?;
        let label = format!(
            "dev.hraness.xcb.habitat.{}",
            &digest(state.as_os_str().as_encoded_bytes())[..24]
        );
        let manifest = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let coordination_root = std::env::var_os("XCB_COORDINATION_ROOT")
            .map(PathBuf::from)
            .map(|path| {
                if !path.is_absolute() {
                    return Err(Error::PrivateState);
                }
                private::check_directory(&path)
            })
            .transpose()?;
        Ok(Self {
            version: 1,
            label,
            state,
            executable,
            home,
            manifest,
            coordination_root,
        })
    }

    pub fn render(&self) -> Result<String> {
        let path = |p: &Path| p.to_str().map(xml).ok_or(Error::PrivateState);
        let coordination = self
            .coordination_root
            .as_ref()
            .map(|root| {
                path(root).map(|value| {
                    format!("<key>XCB_COORDINATION_ROOT</key><string>{value}</string>")
                })
            })
            .transpose()?
            .unwrap_or_default();
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--state</string><string>{}</string><string>managed-daemon</string></array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string>{coordination}</dict><key>RunAtLoad</key><true/><key>StartInterval</key><integer>60</integer><key>ProcessType</key><string>Background</string><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>\n",
            xml(&self.label),
            path(&self.executable)?,
            path(&self.state)?,
            path(&self.home)?
        ))
    }

    fn verify(&self, root: &Path, home: &Path) -> Result<()> {
        let state = root.canonicalize()?;
        let home = home.canonicalize()?;
        let label = format!(
            "dev.hraness.xcb.habitat.{}",
            &digest(state.as_os_str().as_encoded_bytes())[..24]
        );
        let manifest = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let canonical_shape = |p: &Path| {
            p.is_absolute()
                && !p.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
        };
        if self.version != 1
            || self.label != label
            || self.state != state
            || self.home != home
            || self.manifest != manifest
            || !canonical_shape(&self.executable)
            || self
                .coordination_root
                .as_ref()
                .is_some_and(|root| !canonical_shape(root))
        {
            return Err(Error::Unavailable(
                "habitat service ownership does not match this state root",
            ));
        }
        Ok(())
    }
}

fn load(root: &Path, home: &Path) -> Result<Option<Service>> {
    match private::read(&root.join(RECORD), LIMIT) {
        Ok(bytes) => {
            let record: Service = serde_json::from_slice(&bytes)?;
            record.verify(root, home)?;
            Ok(Some(record))
        }
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn lock(root: &Path, name: &str) -> Result<private::ExclusiveLock> {
    let dir = private::directory(&root.join("managed"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(dir.join(name))?;
    private::check_file(&file, 4096)?;
    file.try_lock().map_err(|_| {
        Error::Conflict("habitat service or supervisor is active; pause schedules and project grants, let work settle, then retry")
    })?;
    Ok(private::ExclusiveLock::held(file))
}

fn read_manifest(path: &Path) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    private::check_file(&file, LIMIT as u64)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(Error::PrivateState);
    }
    Ok(bytes)
}

fn domain() -> String {
    format!("gui/{}", rustix::process::getuid().as_raw())
}

fn registered(service: &Service) -> Result<bool> {
    Ok(Command::new("/bin/launchctl")
        .args(["print", &format!("{}/{}", domain(), service.label)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?
        .success())
}

fn supported() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::Unavailable(
            "login service installation currently supports macOS; on Linux run xcb --state <root> managed-daemon from your user service manager",
        ))
    }
}

pub fn status(root: &Path, home: &Path) -> Result<Status> {
    supported()?;
    let service = load(root, home)?;
    let installed = match &service {
        Some(s) => match read_manifest(&s.manifest) {
            Ok(bytes) if bytes == s.render()?.as_bytes() => true,
            Ok(_) => {
                return Err(Error::Conflict(
                    "habitat service manifest changed; foreign contents preserved",
                ));
            }
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e),
        },
        None => false,
    };
    let registered = match &service {
        Some(s) => registered(s)?,
        None => false,
    };
    let supervisor_running = match lock(root, "supervisor.lock") {
        Ok(_) => false,
        Err(Error::Conflict(_)) => true,
        Err(e) => return Err(e),
    };
    Ok(Status {
        installed,
        registered,
        supervisor_running,
        service,
    })
}

pub fn install(root: &Path, executable: &Path, home: &Path) -> Result<Status> {
    supported()?;
    let _guard = lock(root, "service.lock")?;
    let requested = Service::plan(root, executable, home)?;
    let service = match load(root, home)? {
        Some(existing) => {
            if existing.executable != requested.executable
                || existing.coordination_root != requested.coordination_root
            {
                return Err(Error::Conflict(
                    "service uses a different binary or coordination root; uninstall the idle service before rebinding",
                ));
            }
            existing
        }
        None => {
            // Record exact intent before publication so a partial install is retryable.
            private::create(&root.join(RECORD), &serde_json::to_vec(&requested)?)?;
            requested
        }
    };
    let parent = service.manifest.parent().ok_or(Error::PrivateState)?;
    // Check each ancestor before creating the next directory; do not create a
    // LaunchAgents directory through a symlinked Library and reject it afterward.
    for directory in [service.home.join("Library"), parent.to_path_buf()] {
        match fs::symlink_metadata(&directory) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new().mode(0o700).create(&directory)?;
            }
            Err(e) => return Err(e.into()),
            Ok(_) => (),
        }
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o022 != 0
            || directory.canonicalize()? != directory
        {
            return Err(Error::PrivateState);
        }
    }
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::getuid().as_raw()
        || meta.mode() & 0o022 != 0
        || parent.canonicalize()? != parent
    {
        return Err(Error::PrivateState);
    }
    let body = service.render()?;
    match read_manifest(&service.manifest) {
        Ok(bytes) if bytes == body.as_bytes() => (),
        Ok(_) => {
            return Err(Error::Conflict(
                "habitat service manifest changed; foreign contents preserved",
            ));
        }
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut staged = tempfile::NamedTempFile::new_in(parent)?;
            staged.write_all(body.as_bytes())?;
            staged.as_file().sync_all()?;
            staged
                .persist_noclobber(&service.manifest)
                .map_err(|e| Error::Io(e.error))?;
            File::open(parent)?.sync_all()?;
        }
        Err(e) => return Err(e),
    }
    if !registered(&service)? {
        let loaded = Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(domain())
            .arg(&service.manifest)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !loaded.success() {
            return Err(Error::Unavailable(
                "service files retained but login registration failed; retry service install from your logged-in desktop",
            ));
        }
    }
    status(root, home)
}

pub fn uninstall(root: &Path, home: &Path) -> Result<Status> {
    supported()?;
    let _guard = lock(root, "service.lock")?;
    let Some(service) = load(root, home)? else {
        return status(root, home);
    };
    // Hold the dispatch lock through unload/removal. Never bootout a running agent.
    let _idle = lock(root, "supervisor.lock")?;
    let expected = service.render()?;
    let matching_manifest = || match read_manifest(&service.manifest) {
        Ok(bytes) if bytes == expected.as_bytes() => Ok(true),
        Ok(_) => Err(Error::Conflict(
            "habitat service manifest changed; foreign contents preserved",
        )),
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    };
    matching_manifest()?;
    if registered(&service)? {
        let stopped = Command::new("/bin/launchctl")
            .arg("bootout")
            .arg(format!("{}/{}", domain(), service.label))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if !stopped.success() {
            return Err(Error::Unavailable("service unload failed; files preserved"));
        }
    }
    if matching_manifest()? {
        fs::remove_file(&service.manifest)?;
        File::open(service.manifest.parent().ok_or(Error::PrivateState)?)?.sync_all()?;
    }
    fs::remove_file(root.join(RECORD))?;
    File::open(root)?.sync_all()?;
    drop(_idle);
    status(root, home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_is_scoped_and_paths_are_xml_data() {
        let root = tempfile::tempdir().unwrap();
        let state =
            private::directory(&root.path().canonicalize().unwrap().join("state<&")).unwrap();
        let exe = std::env::current_exe().unwrap();
        let service = Service::plan(&state, &exe, root.path()).unwrap();
        let text = service.render().unwrap();
        assert!(text.contains("state&lt;&amp;"));
        assert!(text.contains("<integer>60</integer>"));
        assert!(!text.contains("KeepAlive"));
        assert!(!text.contains("/bin/sh"));
        let other = private::directory(&root.path().canonicalize().unwrap().join("other")).unwrap();
        assert_ne!(
            service.label,
            Service::plan(&other, &exe, root.path()).unwrap().label
        );
        assert!(service.verify(&other, root.path()).is_err());
    }

    #[test]
    fn active_supervisor_prevents_removal_guard() {
        let root = tempfile::tempdir().unwrap();
        let root = private::directory(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let owner = lock(&root, "supervisor.lock").unwrap();
        assert!(lock(&root, "supervisor.lock").is_err());
        drop(owner);
        assert!(lock(&root, "supervisor.lock").is_ok());
    }

    #[test]
    fn foreign_symlink_manifest_is_not_read_or_followed() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        fs::write(&original, "foreign").unwrap();
        let link = root.path().join("job.plist");
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(read_manifest(&link).is_err());
        assert_eq!(fs::read_to_string(original).unwrap(), "foreign");
    }

    #[test]
    fn ownership_survives_removed_binary_and_retains_coordination_scope() {
        let root = tempfile::tempdir().unwrap();
        let state = private::directory(&root.path().canonicalize().unwrap().join("state")).unwrap();
        let mut service =
            Service::plan(&state, &std::env::current_exe().unwrap(), root.path()).unwrap();
        service.executable = state.join("removed-xcb");
        service.coordination_root = Some(state.join("coordination<&"));
        assert!(service.verify(&state, root.path()).is_ok());
        let rendered = service.render().unwrap();
        assert!(rendered.contains("XCB_COORDINATION_ROOT"));
        assert!(rendered.contains("coordination&lt;&amp;"));
        service.executable = PathBuf::from("relative");
        assert!(service.verify(&state, root.path()).is_err());
    }
}
