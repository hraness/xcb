//! Reviewed provider builds admitted independently of the baked constants.
//! Provider releases outpace xcb releases; the repository's
//! `qualified-builds.json` carries reviewed version/digest pairs so an
//! admitted update lands without waiting for a release. The catalog is a
//! data channel over the same trust root that publishes release binaries —
//! it can only ever name digests, and the local hash still binds the exact
//! executable bytes a pin custodies.

use crate::{Error, Result, digest, private};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::Path,
    time::UNIX_EPOCH,
};
use xcb_core::Provider;

/// Pinned to the repository's default branch — the same authority that
/// ships release binaries and attestations.
const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/hraness/xcb/main/qualified-builds.json";
const CATALOG_LIMIT: usize = 64 * 1024;
const ENTRY_LIMIT: usize = 64;
/// Stored catalogs refresh at most hourly; a pending digest adopts on the
/// next supervisor or launch pass after publication.
const REFRESH_MS: u64 = 60 * 60 * 1000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    version: u32,
    codex: Option<Vec<RawBuild>>,
    devin: Option<Vec<RawBuild>>,
    claude: Option<Vec<RawBuild>>,
    deny: Option<BTreeMap<String, Vec<String>>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawBuild {
    version: String,
    sha256: String,
    platform: Option<String>,
    qualified_by: Option<String>,
}

/// Validated reviewed builds: `(version, sha256)` pairs plus denied digests
/// per provider. Parsing is total — a malformed catalog admits nothing.
#[derive(Debug, Default)]
pub struct Catalog {
    admitted: BTreeMap<Provider, BTreeSet<(String, String)>>,
    digests: BTreeSet<String>,
    denied: BTreeSet<String>,
}
impl Catalog {
    fn parse(bytes: &[u8]) -> Result<Self> {
        let raw: RawCatalog =
            serde_json::from_slice(bytes).map_err(|_| Error::Protocol("catalog shape"))?;
        if raw.version != 1 {
            return Err(Error::Protocol("catalog version"));
        }
        let mut catalog = Catalog::default();
        for (provider, builds) in [
            (Provider::Claude, raw.claude),
            (Provider::Codex, raw.codex),
            (Provider::Devin, raw.devin),
        ] {
            let Some(builds) = builds else { continue };
            if builds.len() > ENTRY_LIMIT {
                return Err(Error::Protocol("catalog size"));
            }
            let entries = catalog.admitted.entry(provider).or_default();
            for build in builds {
                check_version(&build.version)?;
                check_digest(&build.sha256)?;
                if let Some(platform) = &build.platform {
                    check_label(platform)?;
                }
                if let Some(qualified_by) = &build.qualified_by {
                    check_label(qualified_by)?;
                }
                let pair = (build.version, build.sha256);
                if !entries.insert(pair.clone()) {
                    return Err(Error::Protocol("catalog duplicate"));
                }
                catalog.digests.insert(pair.1);
            }
        }
        if let Some(deny) = raw.deny {
            for (provider, digests) in deny {
                if !Provider::ALL.iter().any(|known| known.as_str() == provider) {
                    return Err(Error::Protocol("catalog deny provider"));
                }
                if digests.len() > ENTRY_LIMIT {
                    return Err(Error::Protocol("catalog deny size"));
                }
                for sha256 in digests {
                    check_digest(&sha256)?;
                    catalog.denied.insert(sha256);
                }
            }
        }
        Ok(catalog)
    }
    /// A `(version, sha256)` pair the catalog admits; a denied digest is
    /// never admitted even when listed.
    pub fn admitted(&self, provider: Provider, version: &str, sha256: &str) -> bool {
        !self.denied.contains(sha256)
            && self
                .admitted
                .get(&provider)
                .is_some_and(|entries| entries.contains(&(version.to_owned(), sha256.to_owned())))
    }
    /// Whether any provider lists this digest and it is not denied — the
    /// pending-marker check, where the version is only known after inspect.
    pub fn lists(&self, sha256: &str) -> bool {
        !self.denied.contains(sha256) && self.digests.contains(sha256)
    }
    /// A denied digest revokes admission even for baked-in reviewed builds.
    pub fn denied(&self, sha256: &str) -> bool {
        self.denied.contains(sha256)
    }
}

fn check_version(version: &str) -> Result<()> {
    if version.len() > 64
        || !regex::Regex::new(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$")
            .expect("static version grammar")
            .is_match(version)
    {
        return Err(Error::Protocol("catalog version entry"));
    }
    Ok(())
}
fn check_digest(sha256: &str) -> Result<()> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Protocol("catalog digest"));
    }
    Ok(())
}
fn check_label(label: &str) -> Result<()> {
    if label.is_empty() || label.len() > 64 {
        return Err(Error::Protocol("catalog label"));
    }
    Ok(())
}

fn catalog_path(root: &Path) -> std::path::PathBuf {
    root.join("providers").join("catalog.json")
}

/// The stored catalog, or none when absent or malformed — a bad catalog
/// fails closed to the baked constants rather than widening admission.
pub fn stored(root: &Path) -> Option<Catalog> {
    let bytes = private::read(&catalog_path(root), CATALOG_LIMIT).ok()?;
    Catalog::parse(&bytes).ok()
}

pub fn admitted(root: &Path, provider: Provider, version: &str, sha256: &str) -> bool {
    stored(root).is_some_and(|catalog| catalog.admitted(provider, version, sha256))
}

/// Whether the stored catalog lists this digest for any provider and does
/// not deny it — consulted before re-inspecting a pending build.
pub fn listed(root: &Path, sha256: &str) -> bool {
    stored(root).is_some_and(|catalog| catalog.lists(sha256))
}

/// A denied digest revokes even the baked admission constants.
pub fn denied(root: &Path, sha256: &str) -> bool {
    stored(root).is_some_and(|catalog| catalog.denied(sha256))
}

/// Doctor-facing summary of the stored catalog: how many reviewed builds it
/// admits, how many digests it denies, and when it was last fetched.
pub struct CatalogStatus {
    pub builds: usize,
    pub denied: usize,
    pub age_secs: Option<u64>,
}
pub fn status(root: &Path) -> CatalogStatus {
    let stored = stored(root);
    let age_secs = std::fs::metadata(catalog_path(root))
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.elapsed().ok())
        .map(|age| age.as_secs());
    CatalogStatus {
        builds: stored
            .as_ref()
            .map(|catalog| catalog.digests.len())
            .unwrap_or(0),
        denied: stored
            .as_ref()
            .map(|catalog| catalog.denied.len())
            .unwrap_or(0),
        age_secs,
    }
}

/// The stored catalog's age; absent catalogs always refresh.
fn stale(root: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(catalog_path(root)) else {
        return true;
    };
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .is_none_or(|age| age.as_secs() > REFRESH_MS / 1000)
}

/// Fetch the reviewed-builds catalog when the stored copy is missing or
/// stale. Failures keep the stored copy — offline refresh degrades to the
/// baked constants rather than failing the pass.
pub fn refresh(root: &Path) -> Result<()> {
    if !stale(root) {
        return Ok(());
    }
    let output = std::process::Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "8",
            "--connect-timeout",
            "3",
            "--proto",
            "=https",
            "--user-agent",
            concat!("xcb-catalog/", env!("CARGO_PKG_VERSION")),
            CATALOG_URL,
        ])
        .output()
        .map_err(|error| {
            Error::Io(std::io::Error::new(
                error.kind(),
                "catalog refresh needs curl on PATH",
            ))
        })?;
    if !output.status.success() {
        return Err(Error::Unavailable("catalog fetch failed"));
    }
    if output.stdout.len() > CATALOG_LIMIT {
        return Err(Error::Unavailable("catalog response exceeded the bound"));
    }
    Catalog::parse(&output.stdout)?;
    let path = catalog_path(root);
    match private::read(&path, CATALOG_LIMIT) {
        Ok(old) if digest(&old) == digest(&output.stdout) => Ok(()),
        Ok(old) => private::replace(&path, &output.stdout, &digest(old)),
        Err(Error::Io(error)) if error.kind() == ErrorKind::NotFound => {
            private::create(&path, &output.stdout)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0196e89fe5a7598f816ee54232c3d7c26d75e502ab5cfe2c9240e81d90f7255a";
    const OTHER: &str = "7ef3859e68d4eabc0115e51898fcd4eab1edde753c27a472349ef551180b38ff";

    fn build(version: &str, sha256: &str) -> serde_json::Value {
        serde_json::json!({"version": version, "sha256": sha256})
    }

    #[test]
    fn a_valid_catalog_admits_listed_pairs_only() {
        let catalog = Catalog::parse(
            serde_json::json!({"version": 1, "codex": [build("0.157.1", DIGEST)]})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        assert!(catalog.admitted(Provider::Codex, "0.157.1", DIGEST));
        assert!(catalog.lists(DIGEST));
        assert!(!catalog.admitted(Provider::Codex, "0.157.1", OTHER));
        assert!(!catalog.admitted(Provider::Codex, "0.156.1", DIGEST));
        assert!(!catalog.admitted(Provider::Devin, "0.157.1", DIGEST));
        assert!(!catalog.lists(OTHER));
    }

    #[test]
    fn a_denied_digest_is_never_admitted() {
        let catalog = Catalog::parse(
            serde_json::json!({"version": 1, "codex": [build("0.157.1", DIGEST)],
                "deny": {"codex": [DIGEST]}})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        assert!(!catalog.admitted(Provider::Codex, "0.157.1", DIGEST));
        assert!(!catalog.lists(DIGEST));
        assert!(catalog.denied(DIGEST));
    }

    #[test]
    fn malformed_catalogs_admit_nothing() {
        for bytes in [
            &b"{}"[..],
            b"not json",
            serde_json::json!({"version": 2}).to_string().as_bytes(),
            serde_json::json!({"version": 1, "codex": [build("0.157.1", "nope")]})
                .to_string()
                .as_bytes(),
            serde_json::json!({"version": 1, "codex": [build("latest", DIGEST)]})
                .to_string()
                .as_bytes(),
            serde_json::json!({"version": 1, "codex": [build("0.157.1", DIGEST), build("0.157.1", DIGEST)]})
                .to_string()
                .as_bytes(),
            serde_json::json!({"version": 1, "codex": [build("0.157.1", DIGEST)], "extra": true})
                .to_string()
                .as_bytes(),
            serde_json::json!({"version": 1, "deny": {"bogus": [DIGEST]}})
                .to_string()
                .as_bytes(),
        ] {
            assert!(Catalog::parse(bytes).is_err(), "admitted malformed catalog");
        }
    }

    #[test]
    fn oversized_and_unknown_provider_lists_are_rejected() {
        let many: Vec<_> = (0..65)
            .map(|index| build("0.157.1", &format!("{:064x}", index)))
            .collect();
        let bytes = serde_json::json!({"version": 1, "codex": many}).to_string();
        assert!(Catalog::parse(bytes.as_bytes()).is_err());
        // Unknown providers cannot smuggle digests in.
        assert!(
            Catalog::parse(
                serde_json::json!({"version": 1, "openai": [build("0.157.1", DIGEST)]})
                    .to_string()
                    .as_bytes()
            )
            .is_err()
        );
    }

    #[test]
    fn the_checked_in_catalog_is_valid() {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../qualified-builds.json"),
        )
        .unwrap();
        let catalog = Catalog::parse(&bytes).unwrap();
        for provider in Provider::ALL {
            assert!(catalog.admitted.contains_key(&provider));
        }
    }
}
