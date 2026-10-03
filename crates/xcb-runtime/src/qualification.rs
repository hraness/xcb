use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub mod probe;

pub const LINUX_QUALIFICATION_NAME: &str = "qualification/linux.json";
/// v2 added the AppArmor user-namespace restriction to the bound facts; a v1
/// receipt no longer describes enough of the host and must be re-taken.
pub const LINUX_QUALIFICATION_SCHEMA: &str = "xcb.qualification.linux.v2";

/// The exact-path AppArmor profile that lets `/usr/bin/bwrap` create user
/// namespaces while Ubuntu's `kernel.apparmor_restrict_unprivileged_userns`
/// stays on for everything else. The documented fix instead of lifting the
/// global sysctl, which also does not survive a reboot.
pub const APPARMOR_BWRAP_PROFILE: &str = include_str!("qualification/xcb-bwrap.apparmor");
pub const APPARMOR_BWRAP_PROFILE_PATH: &str = "/etc/apparmor.d/xcb-bwrap";

const USERNS_CLONE_SYSCTL: &str = "/proc/sys/kernel/unprivileged_userns_clone";
const MAX_USER_NAMESPACES_SYSCTL: &str = "/proc/sys/user/max_user_namespaces";
const APPARMOR_RESTRICT_SYSCTL: &str = "/proc/sys/kernel/apparmor_restrict_unprivileged_userns";

/// Receipts older than this are stale evidence: the admitted wrapper binary,
/// AppArmor restrictions and namespace sysctls the receipt binds can all drift
/// after collection, so qualification must be re-attested rather than carried
/// forward indefinitely. 30 days bounds that drift while leaving room for
/// evidence produced by a CI run and installed by hand.
pub const MAX_RECEIPT_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

/// Small future-timestamp allowance for clock skew between the runner that
/// stamps `observed_at_ms` and the host admitting the receipt.
pub const RECEIPT_FUTURE_SKEW_MS: u64 = 5 * 60 * 1_000;

/// Probes the receipt must attest; a missing or renamed probe is a failed
/// admission, not a skipped one.
pub const EXPECTED_PROBES: &[&str] = &["linux-sandbox", "linux-egress", "linux-loopback"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wrapper {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Namespaces {
    pub unprivileged_userns_clone: String,
    pub max_user_namespaces: String,
    /// Ubuntu's `kernel.apparmor_restrict_unprivileged_userns`. A receipt
    /// taken with the restriction lifted (0) stops matching once a reboot
    /// turns it back on (1), which is exactly when bwrap starts failing.
    pub apparmor_restrict_unprivileged_userns: String,
}

impl Namespaces {
    /// The live host facts, normalized the way receipts record them: an
    /// unreadable knob is "absent" (or "0" for the namespace limit), never
    /// a missing field.
    pub fn live() -> Self {
        let read = |path: &str, missing: &str| {
            std::fs::read_to_string(path)
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| missing.to_owned())
        };
        Self {
            unprivileged_userns_clone: read(USERNS_CLONE_SYSCTL, "absent"),
            max_user_namespaces: read(MAX_USER_NAMESPACES_SYSCTL, "0"),
            apparmor_restrict_unprivileged_userns: read(APPARMOR_RESTRICT_SYSCTL, "absent"),
        }
    }

    /// True when Ubuntu's AppArmor restriction is on for this host.
    pub fn apparmor_restricted(&self) -> bool {
        self.apparmor_restrict_unprivileged_userns.trim() == "1"
    }
}

/// Why a receipt does or does not admit this host, in the order checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Qualified,
    Schema,
    WrapperChanged,
    /// The AppArmor restriction differs from when the receipt was taken.
    AppArmorChanged {
        recorded: String,
        live: String,
    },
    NamespacesChanged,
    Stale,
    NamespacesUnusable,
    ProbesFailed(Vec<String>),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub exit_code: i32,
    pub passed: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxQualification {
    pub schema: String,
    pub wrapper: Wrapper,
    pub namespaces: Namespaces,
    pub probes: std::collections::BTreeMap<String, Probe>,
    pub observed_at_ms: u64,
}

impl LinuxQualification {
    pub fn load(root: &Path) -> Result<Self> {
        let bytes = std::fs::read(root.join(LINUX_QUALIFICATION_NAME))?;
        if bytes.len() > 64 * 1024 {
            return Err(Error::Unavailable("qualification receipt too large"));
        }
        let receipt: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Protocol("qualification receipt invalid"))?;
        if receipt.schema != LINUX_QUALIFICATION_SCHEMA {
            return Err(Error::Protocol("qualification receipt schema"));
        }
        Ok(receipt)
    }

    /// Admission is bound to the host that produced the evidence: `namespaces`
    /// carries the *current* reads of the same sysctl facts the receipt
    /// recorded ([`Namespaces::live`]), and `now_ms` bounds freshness. Any
    /// drift — a different wrapper, changed sysctls (including the AppArmor
    /// user-namespace restriction), a stale or future timestamp, a missing or
    /// failed probe — means the evidence no longer describes this host.
    pub fn qualified(
        &self,
        bwrap: &Path,
        expected_sha256: &str,
        namespaces: &Namespaces,
        now_ms: u64,
    ) -> bool {
        self.verdict(bwrap, expected_sha256, namespaces, now_ms) == Verdict::Qualified
    }

    pub fn verdict(
        &self,
        bwrap: &Path,
        expected_sha256: &str,
        namespaces: &Namespaces,
        now_ms: u64,
    ) -> Verdict {
        if self.schema != LINUX_QUALIFICATION_SCHEMA {
            return Verdict::Schema;
        }
        if self.wrapper.path != bwrap || self.wrapper.sha256 != expected_sha256 {
            return Verdict::WrapperChanged;
        }
        if self.namespaces.apparmor_restrict_unprivileged_userns
            != namespaces.apparmor_restrict_unprivileged_userns
        {
            return Verdict::AppArmorChanged {
                recorded: self
                    .namespaces
                    .apparmor_restrict_unprivileged_userns
                    .clone(),
                live: namespaces.apparmor_restrict_unprivileged_userns.clone(),
            };
        }
        if self.namespaces.unprivileged_userns_clone != namespaces.unprivileged_userns_clone
            || self.namespaces.max_user_namespaces != namespaces.max_user_namespaces
        {
            return Verdict::NamespacesChanged;
        }
        if self.observed_at_ms > now_ms.saturating_add(RECEIPT_FUTURE_SKEW_MS)
            || now_ms.saturating_sub(self.observed_at_ms) > MAX_RECEIPT_AGE_MS
        {
            return Verdict::Stale;
        }
        let userns_ok = self
            .namespaces
            .unprivileged_userns_clone
            .trim()
            .parse::<i64>()
            .is_ok_and(|v| v != 0);
        let max_ok = self
            .namespaces
            .max_user_namespaces
            .trim()
            .parse::<i64>()
            .is_ok_and(|v| v > 0);
        if !(userns_ok && max_ok) {
            return Verdict::NamespacesUnusable;
        }
        let failed: Vec<String> = EXPECTED_PROBES
            .iter()
            .filter(|name| {
                !self
                    .probes
                    .get(**name)
                    .is_some_and(|p| p.passed == Some(true) && p.exit_code == 0)
            })
            .map(|name| (*name).to_owned())
            .collect();
        if failed.is_empty() {
            Verdict::Qualified
        } else {
            Verdict::ProbesFailed(failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The on-disk receipt format `xcb doctor --qualify-sandbox` writes,
    /// pinned so a field rename can't silently strand existing receipts.
    const FIXTURE: &str = include_str!("../tests/fixtures/linux-qualification.json");

    const NOW: u64 = 1_800_000_000_000;
    const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn receipt() -> LinuxQualification {
        LinuxQualification {
            schema: LINUX_QUALIFICATION_SCHEMA.into(),
            wrapper: Wrapper {
                path: "/usr/bin/bwrap".into(),
                sha256: SHA.into(),
            },
            namespaces: host(),
            probes: [
                (
                    "linux-sandbox",
                    Probe {
                        exit_code: 0,
                        passed: Some(true),
                    },
                ),
                (
                    "linux-egress",
                    Probe {
                        exit_code: 0,
                        passed: Some(true),
                    },
                ),
                (
                    "linux-loopback",
                    Probe {
                        exit_code: 0,
                        passed: Some(true),
                    },
                ),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
            observed_at_ms: NOW,
        }
    }

    fn host() -> Namespaces {
        Namespaces {
            unprivileged_userns_clone: "1".into(),
            max_user_namespaces: "10000".into(),
            apparmor_restrict_unprivileged_userns: "1".into(),
        }
    }

    fn qualified(receipt: &LinuxQualification) -> bool {
        receipt.qualified(Path::new("/usr/bin/bwrap"), SHA, &host(), NOW)
    }

    #[test]
    fn emitted_fixture_deserializes_verbatim() {
        // The receipt must carry exactly the fields this struct
        // deserializes — probe names, exit_code and passed — or admission
        // can never see a real pass.
        let fixture: LinuxQualification = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(fixture.schema, LINUX_QUALIFICATION_SCHEMA);
        assert_eq!(fixture.wrapper.path, PathBuf::from("/usr/bin/bwrap"));
        assert_eq!(fixture.wrapper.sha256.len(), 64);
        assert_eq!(fixture.probes.len(), EXPECTED_PROBES.len());
        for name in EXPECTED_PROBES {
            let probe = fixture.probes.get(*name).expect("expected probe");
            assert_eq!(probe.exit_code, 0, "{name} exit_code");
            assert_eq!(probe.passed, Some(true), "{name} passed");
        }
        assert!(fixture.observed_at_ms > 0);
        assert_eq!(
            fixture.namespaces.apparmor_restrict_unprivileged_userns,
            "1"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(FIXTURE).unwrap(),
            serde_json::to_value(&fixture).unwrap(),
            "the fixture round-trips byte-for-byte in meaning"
        );
    }

    /// The L1 regression: a receipt taken after `sysctl -w
    /// kernel.apparmor_restrict_unprivileged_userns=0` stops admitting the
    /// host once a reboot turns the restriction back on.
    #[test]
    fn a_receipt_taken_without_the_apparmor_restriction_fails_once_it_returns() {
        let mut r = receipt();
        r.namespaces.apparmor_restrict_unprivileged_userns = "0".into();
        let mut lifted = host();
        lifted.apparmor_restrict_unprivileged_userns = "0".into();
        assert!(r.qualified(Path::new("/usr/bin/bwrap"), SHA, &lifted, NOW));
        assert!(!qualified(&r), "live restriction is back on");
        assert_eq!(
            r.verdict(Path::new("/usr/bin/bwrap"), SHA, &host(), NOW),
            Verdict::AppArmorChanged {
                recorded: "0".into(),
                live: "1".into()
            }
        );
        // With the exact-path profile the restriction stays on and the
        // receipt keeps matching.
        assert!(qualified(&receipt()));
        // A kernel without the knob is not a host that had it.
        let mut gone = host();
        gone.apparmor_restrict_unprivileged_userns = "absent".into();
        assert!(!receipt().qualified(Path::new("/usr/bin/bwrap"), SHA, &gone, NOW));
    }

    #[test]
    fn a_v1_receipt_without_the_apparmor_fact_is_refused() {
        let mut v1: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        v1["schema"] = "xcb.qualification.linux.v1".into();
        v1["namespaces"]
            .as_object_mut()
            .unwrap()
            .remove("apparmor_restrict_unprivileged_userns");
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("qualification")).unwrap();
        fs::write(
            dir.path().join(LINUX_QUALIFICATION_NAME),
            serde_json::to_vec(&v1).unwrap(),
        )
        .unwrap();
        assert!(LinuxQualification::load(dir.path()).is_err());
    }

    #[test]
    fn verdict_names_failed_probes() {
        let mut r = receipt();
        r.probes.get_mut("linux-egress").unwrap().passed = Some(false);
        assert_eq!(
            r.verdict(Path::new("/usr/bin/bwrap"), SHA, &host(), NOW),
            Verdict::ProbesFailed(vec!["linux-egress".into()])
        );
    }

    #[test]
    fn the_shipped_apparmor_profile_is_exact_path_and_userns_only() {
        assert!(APPARMOR_BWRAP_PROFILE.contains("/usr/bin/bwrap flags=(unconfined)"));
        assert!(APPARMOR_BWRAP_PROFILE.contains("userns,"));
        assert_eq!(
            crate::sandbox::BWRAP_CANDIDATES[0],
            "/usr/bin/bwrap",
            "the profile names the first bwrap candidate"
        );
    }

    #[test]
    fn qualified_accepts_matching_fresh_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let r = receipt();
        fs::create_dir(dir.path().join("qualification")).unwrap();
        fs::write(
            dir.path().join(LINUX_QUALIFICATION_NAME),
            serde_json::to_vec(&r).unwrap(),
        )
        .unwrap();
        let loaded = LinuxQualification::load(dir.path()).unwrap();
        assert!(loaded.qualified(Path::new("/usr/bin/bwrap"), SHA, &host(), NOW));
    }

    #[test]
    fn rejects_schema_mismatch() {
        let mut r = receipt();
        r.schema = "xcb.qualification.linux.v0".into();
        assert!(!qualified(&r));
    }

    #[test]
    fn rejects_wrapper_mismatch() {
        let r = receipt();
        assert!(!r.qualified(Path::new("/bin/bwrap"), SHA, &host(), NOW));
        assert!(!r.qualified(Path::new("/usr/bin/bwrap"), &"1".repeat(64), &host(), NOW));
    }

    #[test]
    fn rejects_stale_and_future_receipts() {
        let mut r = receipt();
        r.observed_at_ms = NOW - MAX_RECEIPT_AGE_MS - 1;
        assert!(!qualified(&r));
        let mut r = receipt();
        r.observed_at_ms = NOW - MAX_RECEIPT_AGE_MS;
        assert!(qualified(&r));
        let mut r = receipt();
        r.observed_at_ms = NOW + RECEIPT_FUTURE_SKEW_MS + 1;
        assert!(!qualified(&r));
        let mut r = receipt();
        r.observed_at_ms = NOW + RECEIPT_FUTURE_SKEW_MS;
        assert!(qualified(&r));
    }

    #[test]
    fn rejects_namespace_drift() {
        let r = receipt();
        let mut drifted = host();
        drifted.unprivileged_userns_clone = "0".into();
        assert!(!r.qualified(Path::new("/usr/bin/bwrap"), SHA, &drifted, NOW));
        let mut drifted = host();
        drifted.max_user_namespaces = "20000".into();
        assert!(!r.qualified(Path::new("/usr/bin/bwrap"), SHA, &drifted, NOW));
        // A host whose sysctl knob vanished entirely is not the attested host.
        let mut drifted = host();
        drifted.unprivileged_userns_clone = "absent".into();
        assert!(!r.qualified(Path::new("/usr/bin/bwrap"), SHA, &drifted, NOW));
    }

    #[test]
    fn rejects_missing_and_failed_probes() {
        let mut r = receipt();
        r.probes.remove("linux-loopback");
        assert!(!qualified(&r));
        let mut r = receipt();
        r.probes.get_mut("linux-egress").unwrap().passed = Some(false);
        assert!(!qualified(&r));
        let mut r = receipt();
        r.probes.get_mut("linux-sandbox").unwrap().exit_code = 1;
        assert!(!qualified(&r));
        let mut r = receipt();
        r.probes.get_mut("linux-sandbox").unwrap().passed = None;
        assert!(!qualified(&r));
    }

    #[test]
    fn rejects_unusable_namespace_facts() {
        let mut r = receipt();
        r.namespaces.unprivileged_userns_clone = "0".into();
        assert!(!qualified(&r));
        let mut r = receipt();
        r.namespaces.max_user_namespaces = "0".into();
        assert!(!qualified(&r));
    }

    #[test]
    fn missing_receipt_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LinuxQualification::load(dir.path()).is_err());
    }
}
