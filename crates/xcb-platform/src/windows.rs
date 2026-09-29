#![allow(unsafe_code)]
//! Every `unsafe` block here is a single Win32 call whose pointer arguments
//! are locals, owned buffers, or pointers the same call family returned.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, LocalFree, STILL_ACTIVE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce, GetLengthSid,
    GetSecurityDescriptorDacl, GetTokenInformation, INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, GetFileInformationByHandle, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `FILE_FLAG_OPEN_REPARSE_POINT`: open a symlink, junction, or mount point
/// itself instead of its target. Pair it with [`Facts::kind`] to refuse one.
pub const OPEN_REPARSE_POINT: u32 = FILE_FLAG_OPEN_REPARSE_POINT;
/// `FILE_FLAG_BACKUP_SEMANTICS`: required to open a directory handle.
pub const BACKUP_SEMANTICS: u32 = FILE_FLAG_BACKUP_SEMANTICS;

/// S-1-5-18, `NT AUTHORITY\SYSTEM`.
const SYSTEM_SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
/// S-1-5-32-544, `BUILTIN\Administrators`.
const ADMINISTRATORS_SID: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
/// S-1-3-4, `OWNER RIGHTS`: narrows what the owner may do.
const OWNER_RIGHTS_SID: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0];

const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;
const ACCESS_DENIED_OBJECT: u8 = 6;
const ACCESS_DENIED_CALLBACK: u8 = 10;
const ACCESS_DENIED_CALLBACK_OBJECT: u8 = 12;

/// What an opened object is. A reparse point is never followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    ReparsePoint,
}

/// The custody facts of one opened object, the Windows counterpart of the
/// `fstat` fields xcb's Unix code checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub kind: Kind,
    /// Volume serial number (`st_dev`).
    pub volume: u64,
    /// File index on that volume (`st_ino`).
    pub index: u64,
    /// Hard-link count (`st_nlink`).
    pub links: u64,
    pub len: u64,
    /// The owner SID is this process's user or its token's default owner.
    pub owned: bool,
    /// Only this user, the token owner, SYSTEM, or Administrators have an
    /// allow entry that applies to the object (`mode & 0o077 == 0`).
    pub private: bool,
}

impl Facts {
    /// The same filesystem object (`dev`/`ino` equality).
    pub fn same_object(&self, other: &Facts) -> bool {
        self.volume == other.volume && self.index == other.index
    }

    /// An owned, private regular file with exactly one name.
    pub fn is_private_file(&self) -> bool {
        self.kind == Kind::File && self.owned && self.private && self.links == 1
    }

    /// An owned, private real directory.
    pub fn is_private_directory(&self) -> bool {
        self.kind == Kind::Directory && self.owned && self.private
    }
}

struct Principals {
    user: Vec<u8>,
    owner: Vec<u8>,
    user_string: String,
}

fn principals() -> io::Result<&'static Principals> {
    static PRINCIPALS: OnceLock<Principals> = OnceLock::new();
    if let Some(principals) = PRINCIPALS.get() {
        return Ok(principals);
    }
    let user = token_sid(TokenUser)?;
    let owner = token_sid(TokenOwner)?;
    let user_string = sid_string(&user)?;
    Ok(PRINCIPALS.get_or_init(|| Principals {
        user,
        owner,
        user_string,
    }))
}

/// Copy a SID out of memory the OS owns.
///
/// # Safety
/// `sid` must point at a valid SID.
unsafe fn sid_bytes(sid: PSID) -> Vec<u8> {
    let len = unsafe { GetLengthSid(sid) } as usize;
    unsafe { std::slice::from_raw_parts(sid as *const u8, len) }.to_vec()
}

fn token_sid(class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u8>> {
    let mut raw: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0u32;
    unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // u64 storage keeps the pointer-bearing structure aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // TOKEN_USER starts with SID_AND_ATTRIBUTES { Sid, .. } and TOKEN_OWNER
    // with { Owner }: both begin with the SID pointer, which points into
    // `buffer`.
    let sid = unsafe { *(buffer.as_ptr() as *const PSID) };
    Ok(unsafe { sid_bytes(sid) })
}

fn sid_string(sid: &[u8]) -> io::Result<String> {
    let mut raw: *mut u16 = null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_ptr() as PSID, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0usize;
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let text = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw, len) });
    unsafe { LocalFree(raw.cast()) };
    Ok(text)
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

/// A security descriptor parsed from SDDL, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn parse(sddl: &str) -> io::Result<Self> {
        let text = wide(OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = null_mut();
        if unsafe { GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) }
            == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(io::Error::other("owner-only descriptor has no DACL"));
        }
        Ok(dacl)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0) };
    }
}

/// A protected DACL with one full-control entry for this user. `inherit`
/// adds object and container inheritance, so files and directories created
/// inside a private directory are private from their first byte.
fn owner_only(inherit: bool) -> io::Result<Descriptor> {
    let user = &principals()?.user_string;
    let flags = if inherit { "OICI" } else { "" };
    Descriptor::parse(&format!("D:P(A;{flags};FA;;;{user})"))
}

fn trusted(sid: &[u8], principals: &Principals) -> bool {
    sid == principals.user.as_slice()
        || sid == principals.owner.as_slice()
        || sid == SYSTEM_SID.as_slice()
        || sid == ADMINISTRATORS_SID.as_slice()
        || sid == OWNER_RIGHTS_SID.as_slice()
}

/// # Safety
/// `dacl` must point at a valid ACL.
unsafe fn dacl_private(dacl: *const ACL, principals: &Principals) -> bool {
    let count = unsafe { (*dacl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace: *mut core::ffi::c_void = null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return false;
        }
        let header = unsafe { *(ace as *const ACE_HEADER) };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            // Applies only to children created later, not this object.
            continue;
        }
        match header.AceType {
            ACCESS_DENIED
            | ACCESS_DENIED_OBJECT
            | ACCESS_DENIED_CALLBACK
            | ACCESS_DENIED_CALLBACK_OBJECT => continue,
            ACCESS_ALLOWED => {
                let allowed = ace as *const ACCESS_ALLOWED_ACE;
                if unsafe { (*allowed).Mask } == 0 {
                    continue;
                }
                let sid = unsafe { sid_bytes(std::ptr::addr_of!((*allowed).SidStart) as PSID) };
                if !trusted(&sid, principals) {
                    return false;
                }
            }
            // Object, callback, and unknown allow entries can grant access
            // xcb cannot judge; refuse them.
            _ => return false,
        }
    }
    true
}

fn security(handle: HANDLE) -> io::Result<(bool, bool)> {
    let principals = principals()?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let descriptor = Descriptor(descriptor);
    let owned = !owner.is_null() && {
        let owner = unsafe { sid_bytes(owner) };
        owner == principals.user || owner == principals.owner
    };
    // A NULL DACL grants everyone full access.
    let private = !dacl.is_null() && unsafe { dacl_private(dacl, principals) };
    drop(descriptor);
    Ok((owned, private))
}

fn facts_of(handle: HANDLE) -> io::Result<Facts> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::ReparsePoint
    } else if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Directory
    } else {
        Kind::File
    };
    let (owned, private) = security(handle)?;
    Ok(Facts {
        kind,
        volume: u64::from(info.dwVolumeSerialNumber),
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: u64::from(info.nNumberOfLinks),
        len: (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow),
        owned,
        private,
    })
}

/// Custody facts of an open file or directory handle (`fstat`). The handle
/// needs `READ_CONTROL`, which every read or write open includes.
pub fn file_facts(file: &File) -> io::Result<Facts> {
    facts_of(file.as_raw_handle())
}

/// Open `path` without following a final reparse point, for metadata only.
pub fn open_metadata(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// Custody facts of `path` itself, never a reparse point's target (`lstat`).
pub fn path_facts(path: &Path) -> io::Result<Facts> {
    file_facts(&open_metadata(path)?)
}

/// Make `options` refuse to traverse a final reparse point, like
/// `O_NOFOLLOW`. The open then succeeds on the reparse point itself, so the
/// caller must check [`Facts::kind`] of the result.
pub fn no_follow(options: &mut OpenOptions) -> &mut OpenOptions {
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
}

/// Create one directory whose protected DACL grants only this user, like
/// `mkdir(path, 0o700)`. Fails with `AlreadyExists` when the name exists.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let descriptor = owner_only(true)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(path.as_os_str());
    if unsafe { CreateDirectoryW(name.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create `path` and any missing parents as private directories, like
/// `DirBuilder::new().recursive(true).mode(0o700)`.
pub fn create_private_directory_all(path: &Path) -> io::Result<()> {
    match create_private_directory(path) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return if path.is_dir() { Ok(()) } else { Err(error) };
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no parent directory"))?;
    create_private_directory_all(parent)?;
    match create_private_directory(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

/// Replace the DACL of `path` (never a reparse point's target) with one that
/// grants only this user, like `chmod 0600` / `chmod 0700`.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let handle = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let facts = file_facts(&handle)?;
    if facts.kind == Kind::ReparsePoint {
        return Err(io::Error::other("refusing to restrict a reparse point"));
    }
    let descriptor = owner_only(facts.kind == Kind::Directory)?;
    let status = unsafe {
        SetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            descriptor.dacl()?,
            null_mut(),
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    Ok(())
}

/// Whether process `pid` still exists: `Some(true)` while it runs (or is
/// visible but not openable), `Some(false)` when no such process exists or
/// it has exited, `None` when Windows gave no answer. A process number that
/// exists is never proof that it is the same process.
pub fn process_exists(pid: u32) -> Option<bool> {
    if pid == 0 {
        return Some(false);
    }
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_INVALID_PARAMETER) => Some(false),
            Some(ERROR_ACCESS_DENIED) => Some(true),
            _ => None,
        };
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return None;
    }
    Some(code == STILL_ACTIVE as u32)
}

/// `%LOCALAPPDATA%`, the per-user, non-roaming data directory.
pub fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_created_private_directory_and_its_files_are_private() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("state");
        create_private_directory(&directory).unwrap();
        let facts = path_facts(&directory).unwrap();
        assert!(facts.is_private_directory(), "{facts:?}");
        let file = directory.join("record.json");
        std::fs::write(&file, b"{}").unwrap();
        let facts = path_facts(&file).unwrap();
        assert!(facts.is_private_file(), "{facts:?}");
        assert_eq!(facts.len, 2);
        assert!(facts.same_object(&file_facts(&File::open(&file).unwrap()).unwrap()));
    }

    #[test]
    fn nested_private_directories_are_created() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("a").join("b");
        create_private_directory_all(&directory).unwrap();
        create_private_directory_all(&directory).unwrap();
        assert!(path_facts(&directory).unwrap().is_private_directory());
    }

    #[test]
    fn a_second_hard_link_is_visible() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("state");
        create_private_directory(&directory).unwrap();
        let file = directory.join("one");
        std::fs::write(&file, b"x").unwrap();
        std::fs::hard_link(&file, directory.join("two")).unwrap();
        assert_eq!(path_facts(&file).unwrap().links, 2);
        assert!(!path_facts(&file).unwrap().is_private_file());
    }

    #[test]
    fn a_junction_is_a_reparse_point_and_is_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        create_private_directory(&target).unwrap();
        let junction = temp.path().join("junction");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
        let facts = path_facts(&junction).unwrap();
        assert_eq!(facts.kind, Kind::ReparsePoint);
        assert!(!facts.same_object(&path_facts(&target).unwrap()));
        assert!(restrict_to_owner(&junction).is_err());
    }

    #[test]
    fn an_everyone_entry_is_not_private() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("state");
        create_private_directory(&directory).unwrap();
        let file = directory.join("shared");
        std::fs::write(&file, b"x").unwrap();
        let status = std::process::Command::new("icacls")
            .arg(&file)
            .args(["/grant", "*S-1-1-0:(R)"])
            .status()
            .unwrap();
        assert!(status.success());
        let facts = path_facts(&file).unwrap();
        assert!(facts.owned);
        assert!(!facts.private, "{facts:?}");
        restrict_to_owner(&file).unwrap();
        assert!(path_facts(&file).unwrap().is_private_file());
    }

    #[test]
    fn this_process_exists_and_a_finished_one_does_not() {
        assert_eq!(process_exists(std::process::id()), Some(true));
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "exit"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        // The handle `child` still holds keeps the exited process queryable.
        assert_eq!(process_exists(pid), Some(false));
    }
}
