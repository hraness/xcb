//! Account-owned Devin credentials. The provider receives only the opaque token
//! through WINDSURF_API_KEY; no credential file belongs in its disposable HOME.
use crate::{Error, Result, digest, private, store::Store};
use std::{
    collections::BTreeMap,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use xcb_core::{Id, Provider, session::State};
use zeroize::Zeroizing;

const MAX_TOKEN_BYTES: usize = 8192;
const MAX_CREDENTIAL_FILE_BYTES: usize = 64 * 1024;
const API_SERVER: &str = "https://server.codeium.com";
const WEBAPP: &str = "https://app.devin.ai";
const DEVIN_API: &str = "https://api.devin.ai";

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn token_path(store: &Store, account: &Id) -> Result<PathBuf> {
    if store.account(account)?.provider != Provider::Devin {
        return Err(Error::Conflict("Devin credential provider mismatch"));
    }
    Ok(store.account_root(account)?.join("windsurf-token"))
}

/// Presence and local shape only; this neither qualifies a runtime nor proves
/// that the provider will authenticate the opaque token.
pub fn has_credentials(store: &Store, account: &Id) -> Result<bool> {
    if store.account(account)?.provider != Provider::Devin {
        return Ok(false);
    }
    match private::read(&token_path(store, account)?, MAX_TOKEN_BYTES) {
        Ok(bytes) => {
            let bytes = Zeroizing::new(bytes);
            Ok(std::str::from_utf8(&bytes).is_ok_and(valid_token))
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn token(store: &Store, account: &Id) -> Result<Zeroizing<String>> {
    let bytes = Zeroizing::new(private::read(
        &token_path(store, account)?,
        MAX_TOKEN_BYTES,
    )?);
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Unavailable("invalid stored Devin credential"))?;
    if !valid_token(token) {
        return Err(Error::Unavailable("invalid stored Devin credential"));
    }
    Ok(Zeroizing::new(token.to_owned()))
}

/// Store or rotate credentials only while owning the account's exclusive probe
/// lease. An uncertain publication/receipt failure intentionally keeps custody.
pub fn store_token(store: &Store, account: &Id, bytes: &[u8]) -> Result<()> {
    let target = token_path(store, account)?;
    if bytes.len() > MAX_TOKEN_BYTES + 2 {
        return Err(Error::Unavailable("invalid Devin token"));
    }
    let token = std::str::from_utf8(bytes)
        .map_err(|_| Error::Unavailable("invalid Devin token"))?
        .trim();
    if !valid_token(token) {
        return Err(Error::Unavailable("invalid Devin token"));
    }
    let run = store.prepare_probe(account, None, crate::now_ms())?;
    let mut publication_attempted = false;
    let result = (|| {
        let previous = match private::read(&target, MAX_TOKEN_BYTES) {
            Ok(bytes) => Some(Zeroizing::new(bytes)),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        store.begin_tool(
            &run,
            "xcb_devin_auth_store",
            "host_auth_import",
            &digest(token),
        )?;
        publication_attempted = true;
        crate::application_qualification::rotate_generation(store, &run)?;
        if let Some(previous) = previous {
            private::replace(&target, token.as_bytes(), &digest(&previous))?;
        } else {
            private::create(&target, token.as_bytes())?;
        }
        store.settle_tool(&run, "xcb_devin_auth_store")?;
        Ok(())
    })();
    if result.is_ok() || !publication_attempted {
        store.settle(&run, State::Idle, crate::now_ms())?;
    }
    result
}

/// Parse the exact native CredentialsFile's flat string fields, not general
/// user-authored TOML. Reject escapes, tables, comments, duplicate/unknown keys
/// and extra fields rather than guessing at an alternate credential format.
/// All returned slices borrow the caller's zeroized file buffer.
fn credential_token(bytes: &[u8]) -> Result<&str> {
    let invalid = || Error::Unavailable("unsupported Devin credential file");
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(invalid());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, quoted) = line.split_once('=').ok_or_else(invalid)?;
        let key = key.trim();
        if !matches!(
            key,
            "windsurf_api_key" | "api_server_url" | "devin_webapp_host" | "devin_api_url"
        ) {
            return Err(invalid());
        }
        let value = quoted
            .trim()
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(invalid)?;
        if value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'"' | b'\\'))
            || fields.insert(key, value).is_some()
        {
            return Err(invalid());
        }
    }
    let expected_endpoint = |key, expected: &str| {
        fields
            .get(key)
            .is_some_and(|actual| actual.trim_end_matches('/') == expected)
    };
    if fields.len() != 4
        || !expected_endpoint("api_server_url", API_SERVER)
        // The native CredentialsFile stores a hostname here, while older
        // exports used an HTTPS origin. Neither admits an alternate host.
        || !(fields.get("devin_webapp_host") == Some(&"app.devin.ai")
            || expected_endpoint("devin_webapp_host", WEBAPP))
        || !expected_endpoint("devin_api_url", DEVIN_API)
    {
        return Err(Error::Unavailable(
            "Devin credential endpoints are not admitted",
        ));
    }
    let token = fields["windsurf_api_key"];
    if !valid_token(token) {
        return Err(invalid());
    }
    Ok(token)
}

/// The official CLI may create credentials.toml as 0644. Accept that explicit
/// import source without modifying its mode; persistent xcb state remains 0600.
/// Reject foreign/writable-by-others/link/nonregular sources and unstable reads.
fn read_import_source(source: &Path) -> Result<Zeroizing<Vec<u8>>> {
    let before = std::fs::symlink_metadata(source)?;
    if before.mode() & 0o022 != 0 {
        return Err(Error::PrivateState);
    }
    let read = local_custody::stable_read(
        source,
        &local_custody::StableReadOptions {
            exact_mode: Some(before.mode() & 0o7777),
            owner_only: false,
            maximum_bytes: MAX_CREDENTIAL_FILE_BYTES as u64,
            minimum_bytes: Some(1),
            links: Some(1),
            nonblocking: true,
        },
    )
    .map_err(|_| Error::PrivateState)?;
    let bytes = Zeroizing::new(read.bytes);
    let after = std::fs::symlink_metadata(source)?;
    if source.canonicalize()? != source
        || !before.is_file()
        || !after.is_file()
        || before.dev() != read.identity.dev
        || before.ino() != read.identity.ino
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.size() != after.size()
        || before.size() != bytes.len() as u64
        || before.mode() != after.mode()
        || before.uid() != after.uid()
        || before.nlink() != after.nlink()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Error::Conflict(
            "Devin credential source changed during import",
        ));
    }
    Ok(bytes)
}

/// Explicit source import only. Preserve the source, copy only its opaque
/// token, and never inherit endpoint overrides, plugins or provider state.
pub fn import_account(store: &Store, source: &Path) -> Result<Id> {
    if !source.is_absolute()
        || source.file_name().and_then(|name| name.to_str()) != Some("credentials.toml")
        || source.canonicalize()? != source
    {
        return Err(Error::PrivateState);
    }
    let bytes = read_import_source(source)?;
    let token = credential_token(&bytes)?;
    let account = store.add_account(
        Provider::Devin,
        "Imported subscription",
        crate::now_ms(),
        None,
    )?;
    store_token(store, &account.id, token.as_bytes())?;
    Ok(account.id)
}
