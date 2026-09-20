//! Bounded, host-selected public trust roots for confined Codex HTTPS.
#[cfg(target_os = "macos")]
use crate::private;
use crate::{Error, Result};
use std::{
    fs::{self, File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
const LIMIT: u64 = 2 * 1024 * 1024;

fn stamp(m: &Metadata) -> (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.uid(),
        m.mode(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

fn read_trusted(source: &Path, owner: u32) -> Result<Vec<u8>> {
    let fd = rustix::fs::open(
        source,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(std::io::Error::from)?;
    let mut file = File::from(fd);
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != owner
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
        || before.len() == 0
        || before.len() > LIMIT
    {
        return Err(Error::Unavailable(
            "system public CA bundle has unsafe ownership, type, permissions, or size",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let named = fs::symlink_metadata(source)?;
    if !named.is_file()
        || stamp(&before) != stamp(&file.metadata()?)
        || stamp(&before) != stamp(&named)
        || bytes.len() as u64 != before.len()
    {
        return Err(Error::Conflict(
            "system public CA bundle changed during snapshot",
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Unavailable("system public CA bundle is not PEM text"))?;
    if !text.contains("-----BEGIN CERTIFICATE-----") || !text.contains("-----END CERTIFICATE-----")
    {
        return Err(Error::Unavailable(
            "system public CA bundle contains no PEM certificate",
        ));
    }
    Ok(bytes)
}

#[cfg(target_os = "macos")]
pub(crate) fn snapshot(directory: &Path) -> Result<PathBuf> {
    // Never inherit a caller-selected trust store or consult a user keychain.
    let bytes = read_trusted(Path::new("/private/etc/ssl/cert.pem"), 0)?;
    let target = directory.join("public-ca.pem");
    private::create(&target, &bytes)?;
    if private::read(&target, LIMIT as usize)? != bytes {
        return Err(Error::Conflict("public CA snapshot changed before launch"));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    const PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\nSYNTHETIC\n-----END CERTIFICATE-----\n";
    fn fixture() -> (tempfile::TempDir, PathBuf, u32) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ca.pem");
        fs::write(&path, PEM).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let uid = path.metadata().unwrap().uid();
        (directory, path, uid)
    }
    #[test]
    fn trusted_public_ca_is_bounded_and_owner_selected() {
        let (_directory, path, uid) = fixture();
        assert_eq!(read_trusted(&path, uid).unwrap(), PEM);
        assert!(read_trusted(&path, uid.wrapping_add(1)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(read_trusted(&path, uid).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&path, b"not a certificate").unwrap();
        assert!(read_trusted(&path, uid).is_err());
        File::create(&path).unwrap().set_len(LIMIT + 1).unwrap();
        assert!(read_trusted(&path, uid).is_err());
    }
    #[test]
    fn public_ca_rejects_aliases_and_nonfiles() {
        let (directory, path, uid) = fixture();
        let alias = directory.path().join("alias");
        symlink(&path, &alias).unwrap();
        assert!(read_trusted(&alias, uid).is_err());
        fs::remove_file(&alias).unwrap();
        fs::hard_link(&path, &alias).unwrap();
        assert!(read_trusted(&path, uid).is_err());
        assert!(read_trusted(directory.path(), uid).is_err());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn public_ca_snapshot_is_private_single_link_and_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let root =
            private::directory(&directory.path().canonicalize().unwrap().join("launch")).unwrap();
        let target = snapshot(&root).unwrap();
        let first = private::read(&target, LIMIT as usize).unwrap();
        assert_eq!(target.metadata().unwrap().nlink(), 1);
        assert_eq!(target.metadata().unwrap().mode() & 0o777, 0o600);
        assert_eq!(
            first,
            read_trusted(Path::new("/private/etc/ssl/cert.pem"), 0).unwrap()
        );
        assert!(snapshot(&root).is_err());
        assert_eq!(first, private::read(&target, LIMIT as usize).unwrap());
    }
}
