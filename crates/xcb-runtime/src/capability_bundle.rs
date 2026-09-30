//! Private, content-addressed copies of explicitly selected installed runtimes.
//! Workspace content is never an enrollment source. Relative internal links
//! retain their exact spelling, including signed application's framework links.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityBundle {
    pub root: PathBuf,
    pub sha256: String,
}

impl CapabilityBundle {
    /// Configuration validation deliberately performs no disk reads: a missing
    /// or damaged bundle must still be removable from the owner's config.
    pub fn validate(&self) -> Result<()> {
        if !self.root.is_absolute()
            || self.root.as_os_str().len() > 4096
            || self
                .root
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self
                .root
                .parent()
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                != Some("payload")
            || self
                .root
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|s| s.to_str())
                != Some(self.sha256.as_str())
            || self.root.file_name().and_then(|s| s.to_str()).is_none()
        {
            return Err(Error::Protocol("invalid capability bundle identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BundleLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_file_bytes: u64,
    pub max_depth: usize,
    pub max_manifest_bytes: usize,
}
impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_entries: 32_768,
            max_bytes: 1024 * 1024 * 1024,
            max_file_bytes: 256 * 1024 * 1024,
            max_depth: 128,
            max_manifest_bytes: 8 * 1024 * 1024,
        }
    }
}

#[cfg(unix)]
mod implementation {
    use super::*;
    use crate::private;
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeSet,
        fs::{self, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    };

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Manifest {
        version: u32,
        root_name: String,
        entries: Vec<Entry>,
    }
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        path: String,
        kind: Kind,
    }
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    enum Kind {
        Directory,
        File {
            sha256: String,
            bytes: u64,
            executable: bool,
        },
        Symlink {
            target: String,
        },
    }

    struct Stage(PathBuf);
    impl Drop for Stage {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits_valid(limits: BundleLimits) -> Result<()> {
        if limits.max_entries == 0
            || limits.max_entries > 65_536
            || limits.max_bytes == 0
            || limits.max_bytes > 4 * 1024 * 1024 * 1024
            || limits.max_file_bytes == 0
            || limits.max_file_bytes > limits.max_bytes
            || limits.max_depth == 0
            || limits.max_depth > 256
            || limits.max_manifest_bytes == 0
            || limits.max_manifest_bytes > 16 * 1024 * 1024
        {
            return Err(Error::Protocol("invalid capability bundle limits"));
        }
        Ok(())
    }

    fn disjoint(a: &Path, b: &Path) -> bool {
        !a.starts_with(b) && !b.starts_with(a)
    }
    fn path_text(path: &Path) -> Result<String> {
        let value = path
            .to_str()
            .ok_or(Error::Protocol("non-UTF8 capability bundle path"))?;
        if value.is_empty()
            || value.len() > 4096
            || path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Error::Protocol("invalid capability bundle relative path"));
        }
        Ok(value.to_owned())
    }

    fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.len() == b.len()
            && a.mode() == b.mode()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }

    fn hash_file(
        path: &Path,
        private_input: bool,
        limits: BundleLimits,
        total: &mut u64,
        destination: Option<&Path>,
    ) -> Result<Kind> {
        let before = fs::symlink_metadata(path)?;
        if !before.is_file() || before.len() > limits.max_file_bytes {
            return Err(Error::Protocol(
                "capability bundle file kind or size refused",
            ));
        }
        let mut input = if private_input {
            private::open_file(path, limits.max_file_bytes)?
        } else {
            OpenOptions::new()
                .read(true)
                .custom_flags(
                    (rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::CLOEXEC)
                        .bits() as i32,
                )
                .open(path)?
        };
        if !same_metadata(&before, &input.metadata()?) {
            return Err(Error::Conflict("capability source changed"));
        }
        let executable = before.mode() & 0o111 != 0;
        if private_input && before.mode() & 0o7777 != if executable { 0o700 } else { 0o600 } {
            return Err(Error::PrivateState);
        }
        let mut output = destination
            .map(|path| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
                    .open(path)
            })
            .transpose()?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .ok_or(Error::Protocol("capability bundle size overflow"))?;
            *total = total
                .checked_add(count as u64)
                .ok_or(Error::Protocol("capability bundle size overflow"))?;
            if bytes > limits.max_file_bytes || *total > limits.max_bytes {
                return Err(Error::Protocol("capability bundle byte limit exceeded"));
            }
            hash.update(&buffer[..count]);
            if let Some(output) = &mut output {
                output.write_all(&buffer[..count])?;
            }
        }
        private::same_file(path, &input)?;
        if !same_metadata(&before, &input.metadata()?)
            || !same_metadata(&before, &fs::symlink_metadata(path)?)
        {
            return Err(Error::Conflict("capability source changed"));
        }
        if let Some(output) = output {
            output.set_permissions(fs::Permissions::from_mode(if executable {
                0o700
            } else {
                0o600
            }))?;
            output.sync_all()?;
        }
        Ok(Kind::File {
            sha256: hex::encode(hash.finalize()),
            bytes,
            executable,
        })
    }

    fn link_target(root: &Path, path: &Path) -> Result<String> {
        let target = fs::read_link(path)?;
        if target.is_absolute() {
            return Err(Error::Protocol(
                "absolute capability bundle symlink refused",
            ));
        }
        let resolved = fs::canonicalize(path)?;
        let parent = fs::canonicalize(path.parent().ok_or(Error::PrivateState)?)?;
        if !resolved.starts_with(root) || parent.starts_with(&resolved) {
            return Err(Error::Protocol(
                "external or cyclic capability bundle symlink refused",
            ));
        }
        let target = target
            .to_str()
            .ok_or(Error::Protocol("non-UTF8 capability symlink"))?;
        if target.is_empty() || target.len() > 4096 {
            return Err(Error::Protocol("invalid capability symlink"));
        }
        Ok(target.to_owned())
    }

    fn walk(
        root: &Path,
        relative: &Path,
        destination: Option<&Path>,
        private_input: bool,
        limits: BundleLimits,
        total: &mut u64,
        entries: &mut Vec<Entry>,
    ) -> Result<()> {
        if relative.components().count() > limits.max_depth || entries.len() >= limits.max_entries {
            return Err(Error::Protocol(
                "capability bundle entry or depth limit exceeded",
            ));
        }
        let path = root.join(relative);
        let before = fs::symlink_metadata(&path)?;
        let kind = if before.file_type().is_symlink() {
            if private_input && before.uid() != rustix::process::geteuid().as_raw() {
                return Err(Error::PrivateState);
            }
            let target = link_target(root, &path)?;
            if let Some(output) = destination {
                symlink(&target, output.join(relative))?;
            }
            Kind::Symlink { target }
        } else if before.is_dir() {
            if private_input {
                private::check_directory(&path)?;
                if before.mode() & 0o7777 != 0o700 {
                    return Err(Error::PrivateState);
                }
            }
            if let Some(output) = destination {
                private::directory(&output.join(relative))?;
            }
            entries.push(Entry {
                path: path_text(relative)?,
                kind: Kind::Directory,
            });
            for name in children(&path, limits.max_entries.saturating_sub(entries.len()))? {
                walk(
                    root,
                    &relative.join(name),
                    destination,
                    private_input,
                    limits,
                    total,
                    entries,
                )?;
            }
            if !same_metadata(&before, &fs::symlink_metadata(&path)?) {
                return Err(Error::Conflict("capability source changed"));
            }
            if let Some(output) = destination {
                private::sync_directory(&output.join(relative))?;
            }
            return Ok(());
        } else if before.is_file() {
            hash_file(
                &path,
                private_input,
                limits,
                total,
                destination.map(|output| output.join(relative)).as_deref(),
            )?
        } else {
            return Err(Error::Protocol("special capability bundle file refused"));
        };
        entries.push(Entry {
            path: path_text(relative)?,
            kind,
        });
        Ok(())
    }

    fn manifest(
        root: &Path,
        root_name: &str,
        destination: Option<&Path>,
        private_input: bool,
        single_file: Option<&str>,
        limits: BundleLimits,
    ) -> Result<Manifest> {
        if private_input {
            private::check_directory(root)?;
        }
        let before = fs::symlink_metadata(root)?;
        if !before.is_dir() {
            return Err(Error::PrivateState);
        }
        let mut entries = Vec::new();
        let mut total = 0;
        if let Some(name) = single_file {
            walk(
                root,
                Path::new(name),
                destination,
                private_input,
                limits,
                &mut total,
                &mut entries,
            )?;
        } else {
            for name in children(root, limits.max_entries)? {
                walk(
                    root,
                    Path::new(&name),
                    destination,
                    private_input,
                    limits,
                    &mut total,
                    &mut entries,
                )?;
            }
        }
        if !same_metadata(&before, &fs::symlink_metadata(root)?) {
            return Err(Error::Conflict("capability source changed"));
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Manifest {
            version: 1,
            root_name: root_name.to_owned(),
            entries,
        })
    }

    fn children(path: &Path, maximum: usize) -> Result<Vec<std::ffi::OsString>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(path)? {
            if names.len() >= maximum {
                return Err(Error::Protocol("capability bundle entry limit exceeded"));
            }
            names.push(entry?.file_name());
        }
        names.sort();
        Ok(names)
    }

    fn exact_children(path: &Path, names: &[&str]) -> Result<()> {
        private::check_directory(path)?;
        let found = children(path, names.len())?
            .into_iter()
            .map(|name| {
                name.into_string()
                    .map_err(|_| Error::Protocol("non-UTF8 bundle entry"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if found != names.iter().map(|name| (*name).to_owned()).collect() {
            return Err(Error::Conflict("capability bundle tree changed"));
        }
        Ok(())
    }

    pub fn verify(root: &Path, sha256: &str, limits: BundleLimits) -> Result<()> {
        limits_valid(limits)?;
        CapabilityBundle {
            root: root.to_owned(),
            sha256: sha256.to_owned(),
        }
        .validate()?;
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::PrivateState)?;
        let payload = root.parent().ok_or(Error::PrivateState)?;
        let container = payload.parent().ok_or(Error::PrivateState)?;
        exact_children(container, &["manifest.json", "payload"])?;
        exact_children(payload, &[name])?;
        private::check_directory(root)?;
        let bytes = private::read(&container.join("manifest.json"), limits.max_manifest_bytes)?;
        if crate::digest(&bytes) != sha256 {
            return Err(Error::Conflict("capability bundle manifest changed"));
        }
        let expected: Manifest = serde_json::from_slice(&bytes)?;
        if expected.version != 1
            || expected.root_name != name
            || expected.entries.len() > limits.max_entries
            || serde_json::to_vec(&expected)? != bytes
        {
            return Err(Error::Protocol("invalid capability bundle manifest"));
        }
        let actual = manifest(root, name, None, true, None, limits)?;
        if actual != expected {
            return Err(Error::Conflict("capability bundle tree changed"));
        }
        Ok(())
    }

    fn capture(
        source: &Path,
        single: Option<&str>,
        root_name: &str,
        store: &Path,
        workspace: &Path,
        limits: BundleLimits,
    ) -> Result<CapabilityBundle> {
        limits_valid(limits)?;
        path_text(Path::new(root_name))?;
        let source = fs::canonicalize(source)?;
        let workspace = fs::canonicalize(workspace)?;
        let source_scope = single.map_or_else(|| source.clone(), |name| source.join(name));
        // Check before creating anything. Neither source nor output may be an
        // ancestor of the consumer, and output must not sit inside the source.
        if !store.is_absolute()
            || !disjoint(&source_scope, &workspace)
            || !disjoint(store, &workspace)
            || !disjoint(store, &source_scope)
        {
            return Err(Error::Protocol(
                "capability bundle must be outside the consumer workspace and source",
            ));
        }
        let store = private::directory(store)?;
        if !disjoint(&store, &workspace) || !disjoint(&store, &source_scope) {
            return Err(Error::PrivateState);
        }
        let stage = Stage(private::directory(
            &store.join(crate::new_id("bundle").as_str()),
        )?);
        let payload = private::directory(&stage.0.join("payload"))?;
        let root = private::directory(&payload.join(root_name))?;
        let captured = manifest(&source, root_name, Some(&root), false, single, limits)?;
        // A provider update must not leave a mixed runtime: rescan the entire
        // selected source after capture, not only each opened file's metadata.
        if captured != manifest(&source, root_name, None, false, single, limits)? {
            return Err(Error::Conflict("capability source changed during capture"));
        }
        let bytes = serde_json::to_vec(&captured)?;
        if bytes.len() > limits.max_manifest_bytes {
            return Err(Error::Protocol("capability bundle manifest limit exceeded"));
        }
        let sha256 = crate::digest(&bytes);
        private::create(&stage.0.join("manifest.json"), &bytes)?;
        private::sync_directory(&root)?;
        private::sync_directory(&payload)?;
        private::sync_directory(&stage.0)?;
        let lock_path = store.join("bundle.lock");
        match private::create(&lock_path, b"") {
            Ok(()) => (),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error),
        }
        let lock = private::open_file(&lock_path, 0)?;
        private::lock(&lock)?;
        let _lock = private::ExclusiveLock::held(lock);
        let container = store.join(&sha256);
        let bundle = CapabilityBundle {
            root: container.join("payload").join(root_name),
            sha256,
        };
        match fs::symlink_metadata(&container) {
            Ok(_) => verify(&bundle.root, &bundle.sha256, limits)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::rename(&stage.0, &container)?;
                private::sync_directory(&store)?;
                verify(&bundle.root, &bundle.sha256, limits)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(bundle)
    }

    pub fn snapshot(
        trusted_root: &Path,
        bundle_store: &Path,
        consumer_workspace: &Path,
        limits: BundleLimits,
    ) -> Result<CapabilityBundle> {
        let source = fs::canonicalize(trusted_root)?;
        let name = trusted_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::Protocol("invalid trusted runtime root"))?;
        capture(
            &source,
            None,
            name,
            bundle_store,
            consumer_workspace,
            limits,
        )
    }

    pub fn snapshot_file(
        trusted_file: &Path,
        bundle_store: &Path,
        consumer_workspace: &Path,
        limits: BundleLimits,
    ) -> Result<CapabilityBundle> {
        let file = fs::canonicalize(trusted_file)?;
        if !fs::symlink_metadata(&file)?.is_file() {
            return Err(Error::Protocol("trusted runtime must be a file"));
        }
        let parent = file.parent().ok_or(Error::PrivateState)?;
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::Protocol("invalid trusted runtime file"))?;
        capture(
            parent,
            Some(name),
            "files",
            bundle_store,
            consumer_workspace,
            limits,
        )
    }
}

#[cfg(unix)]
pub use implementation::{snapshot, snapshot_file, verify};

#[cfg(not(unix))]
pub fn verify(_root: &Path, _sha256: &str, _limits: BundleLimits) -> Result<()> {
    Err(Error::Unavailable(
        "private capability bundles require Unix custody",
    ))
}

#[cfg(not(unix))]
pub fn snapshot(
    _trusted_root: &Path,
    _store: &Path,
    _workspace: &Path,
    _limits: BundleLimits,
) -> Result<CapabilityBundle> {
    Err(Error::Unavailable(
        "private capability bundles require Unix custody",
    ))
}

#[cfg(not(unix))]
pub fn snapshot_file(
    _trusted_file: &Path,
    _store: &Path,
    _workspace: &Path,
    _limits: BundleLimits,
) -> Result<CapabilityBundle> {
    Err(Error::Unavailable(
        "private capability bundles require Unix custody",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    struct Fixture {
        _temporary: tempfile::TempDir,
        source: PathBuf,
        store: PathBuf,
        workspace: PathBuf,
    }
    fn fixture() -> Fixture {
        let temporary = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let source = base.join("Test Runtime.app");
        let workspace = base.join("consumer");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&workspace).unwrap();
        Fixture {
            _temporary: temporary,
            source,
            store: base.join("bundles"),
            workspace,
        }
    }
    fn capture(fixture: &Fixture) -> CapabilityBundle {
        snapshot(
            &fixture.source,
            &fixture.store,
            &fixture.workspace,
            BundleLimits::default(),
        )
        .unwrap()
    }
    fn checked(bundle: &CapabilityBundle) -> bool {
        verify(&bundle.root, &bundle.sha256, BundleLimits::default()).is_ok()
    }

    #[test]
    fn capability_bundle_preserves_package_name_links_and_executable_identity_and_reuses_exact_content()
     {
        let fixture = fixture();
        fs::create_dir(fixture.source.join("Contents")).unwrap();
        let program = fixture.source.join("Contents/main");
        fs::write(&program, b"original-runtime").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        symlink("Contents/main", fixture.source.join("main")).unwrap();
        let first = capture(&fixture);
        assert_eq!(first.root.file_name().unwrap(), "Test Runtime.app");
        assert_eq!(
            fs::read_link(first.root.join("main")).unwrap(),
            Path::new("Contents/main")
        );
        assert_eq!(
            fs::metadata(first.root.join("Contents/main"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert!(checked(&first));
        let second = capture(&fixture);
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.root, second.root);
        assert_eq!(fs::read_dir(&fixture.store).unwrap().count(), 2);
        fs::write(&program, b"updated-runtime").unwrap();
        let updated = capture(&fixture);
        assert_ne!(updated.sha256, first.sha256);
        assert!(checked(&first));
        assert!(checked(&updated));
        fs::write(first.root.join("Contents/main"), b"changed-runtime").unwrap();
        assert!(!checked(&first));
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn capability_bundle_rejects_file_mode_link_manifest_and_unexpected_tree_changes() {
        let fixture = fixture();
        fs::write(fixture.source.join("main"), b"one").unwrap();
        let first = capture(&fixture);
        fs::set_permissions(first.root.join("main"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!checked(&first));
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
        fs::set_permissions(first.root.join("main"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(checked(&first));
        fs::write(first.root.join("unexpected"), b"not admitted").unwrap();
        assert!(!checked(&first));
        fs::remove_file(first.root.join("unexpected")).unwrap();
        fs::hard_link(first.root.join("main"), fixture.store.join("extra-name")).unwrap();
        assert!(!checked(&first));
        fs::remove_file(fixture.store.join("extra-name")).unwrap();
        assert!(checked(&first));
        let manifest = first
            .root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("manifest.json");
        fs::write(&manifest, b"{}").unwrap();
        assert!(!checked(&first));
        // Configuration remains removable even after loss of the bundle.
        fs::remove_dir_all(first.root.parent().unwrap().parent().unwrap()).unwrap();
        assert!(first.validate().is_ok());
        assert!(!checked(&first));
    }

    #[test]
    fn capability_bundle_refuses_workspace_sources_outputs_and_external_absolute_or_cyclic_links() {
        let fixture = fixture();
        fs::write(fixture.source.join("main"), b"runtime").unwrap();
        assert!(
            snapshot(
                &fixture.workspace,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
        assert!(!fixture.store.exists());
        assert!(
            snapshot(
                &fixture.source,
                &fixture.workspace.join("bundles"),
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
        assert!(!fixture.workspace.join("bundles").exists());
        symlink(fixture.source.join("main"), fixture.source.join("absolute")).unwrap();
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
        fs::remove_file(fixture.source.join("absolute")).unwrap();
        fs::write(fixture.source.parent().unwrap().join("outside"), b"private").unwrap();
        symlink("../outside", fixture.source.join("external")).unwrap();
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
        fs::remove_file(fixture.source.join("external")).unwrap();
        symlink("b", fixture.source.join("a")).unwrap();
        symlink("a", fixture.source.join("b")).unwrap();
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits::default()
            )
            .is_err()
        );
    }

    #[test]
    fn capability_bundle_enforces_byte_entry_and_depth_bounds_and_cleans_failed_staging() {
        let fixture = fixture();
        fs::write(fixture.source.join("main"), b"runtime").unwrap();
        let small = BundleLimits {
            max_bytes: 4,
            max_file_bytes: 4,
            ..BundleLimits::default()
        };
        assert!(snapshot(&fixture.source, &fixture.store, &fixture.workspace, small).is_err());
        assert_eq!(fs::read_dir(&fixture.store).unwrap().count(), 0);
        fs::write(fixture.source.join("second"), b"runtime").unwrap();
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits {
                    max_entries: 1,
                    ..BundleLimits::default()
                }
            )
            .is_err()
        );
        fs::create_dir(fixture.source.join("nested")).unwrap();
        fs::write(fixture.source.join("nested/file"), b"runtime").unwrap();
        assert!(
            snapshot(
                &fixture.source,
                &fixture.store,
                &fixture.workspace,
                BundleLimits {
                    max_depth: 1,
                    ..BundleLimits::default()
                }
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(&fixture.store).unwrap().count(), 0);
    }

    #[test]
    fn capability_bundle_single_file_captures_only_the_explicit_binary() {
        let fixture = fixture();
        fs::write(fixture.source.join("codex"), b"binary").unwrap();
        fs::write(fixture.source.join("private-sibling"), b"do not include").unwrap();
        let bundle = snapshot_file(
            &fixture.source.join("codex"),
            &fixture.store,
            &fixture.workspace,
            BundleLimits::default(),
        )
        .unwrap();
        assert_eq!(fs::read(bundle.root.join("codex")).unwrap(), b"binary");
        assert!(!bundle.root.join("private-sibling").exists());
        assert!(checked(&bundle));
    }
}
