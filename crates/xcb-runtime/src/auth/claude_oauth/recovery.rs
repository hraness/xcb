//! Offline reconciliation: never reads Keychain or retries an OAuth request.
use super::*;
use std::{collections::BTreeSet, path::Path};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GenerationIntent {
    version: u8,
    account: Id,
    run: Id,
    generation: String,
    username: String,
    service: String,
    profile: PathBuf,
}

pub(crate) struct Proof {
    pub generations: Vec<String>,
    pub reauthentication_required: bool,
    quarantine: Vec<(PathBuf, Vec<u8>)>,
}

fn refusal() -> Error {
    Error::Conflict("Claude sign-in recovery evidence is incomplete or changed")
}

pub(crate) fn recognized(call: &str, operation: &str) -> bool {
    call.starts_with("xcb_auth_claude_") || operation.starts_with("host_auth_claude_")
}

pub(crate) fn auth_marker(name: &str) -> bool {
    matches!(name, AUTH_CUSTODY | KEYCHAIN_CUSTODY)
}

/// The caller holds an immediate database transaction, with the exact lease
/// and stopped owner/helpers established. All paths derive from trusted IDs.
pub(crate) fn inspect(
    account_root: &Path,
    run: &RunRecord,
    run_digest: &str,
    effects: &[(String, String, String)],
) -> Result<Proof> {
    let active = read_active_at(account_root, &run.account)?;
    let revision = active.as_ref().map(|active| active.revision.as_str());
    let mut generations = BTreeSet::new();
    let mut seen_auth = false;
    let mut seen_keychain = false;
    let mut reauthentication_required = false;
    for (call, operation, expected) in effects {
        let suffix = call.strip_prefix("xcb_auth_claude_").ok_or_else(refusal)?;
        let (generation_id, call_id) = suffix.split_once('_').ok_or_else(refusal)?;
        if !valid_uuid(generation_id) || !valid_uuid(call_id) {
            return Err(refusal());
        }
        let generation = Generation::resolve_at(account_root.to_owned(), generation_id)?;
        // Do not call Generation::check: a retained file fallback is evidence
        // to quarantine, never an authorized credential source.
        private::check_directory(&generation.root)?;
        let declaration: GenerationIntent = serde_json::from_slice(&private::read(
            &generation.root.join("intent.json"),
            MAX_RECORD,
        )?)
        .map_err(|_| refusal())?;
        if declaration.version != 1
            || declaration.account != run.account
            || declaration.generation != generation_id
            || declaration.username != generation.username
            || declaration.service != generation.service
            || declaration.profile != generation.profile
        {
            return Err(refusal());
        }
        let bytes = private::read(
            &generation.root.join("effects").join(format!("{call}.json")),
            MAX_RECORD,
        )?;
        if crate::digest(&bytes) != *expected {
            return Err(refusal());
        }
        let intent: EffectIntent = serde_json::from_slice(&bytes).map_err(|_| refusal())?;
        if intent.version != 1
            || intent.run != run.id
            || intent.account != run.account
            || intent.generation != generation_id
            || intent.call != *call
            || intent
                .active_revision
                .as_deref()
                .is_some_and(|value| !xcb_core::hex64(value))
        {
            return Err(refusal());
        }
        let mutating = match (operation.as_str(), intent.operation.as_str()) {
            ("host_auth_claude_oauth", "keychain-read" | "keychain-preflight") => {
                if intent.publication_revision.is_some() {
                    return Err(refusal());
                }
                seen_keychain = true;
                false
            }
            ("host_auth_claude_oauth", "login" | "refresh") => {
                if intent.publication_revision.is_some()
                    || (intent.operation == "login" && declaration.run != run.id)
                {
                    return Err(refusal());
                }
                seen_auth = true;
                true
            }
            ("host_auth_claude_publish", "publish") => {
                if !intent
                    .publication_revision
                    .as_deref()
                    .is_some_and(xcb_core::hex64)
                {
                    return Err(refusal());
                }
                seen_auth = true;
                true
            }
            _ => return Err(refusal()),
        };
        if active
            .as_ref()
            .is_some_and(|active| active.value.generation == generation_id)
        {
            reauthentication_required |= mutating;
            if !mutating && intent.active_revision.as_deref() != revision {
                return Err(refusal());
            }
        } else if intent.active_revision.as_deref() != revision {
            // No evidence that the previous active generation was untouched.
            return Err(refusal());
        }
        generations.insert(generation_id.to_owned());
    }
    // A pending effect cannot prove an unrecorded child stopped. Likewise a
    // marker with no matching receipt must not borrow another helper's proof.
    if seen_auth != run.capability_processes.contains_key(AUTH_CUSTODY)
        || seen_keychain != run.capability_processes.contains_key(KEYCHAIN_CUSTODY)
        || generations.is_empty()
    {
        return Err(refusal());
    }
    let generations: Vec<_> = generations.into_iter().collect();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "version": 1, "run": run.id, "account": run.account,
        "runDigest": run_digest, "effects": effects,
        "activeRevision": revision, "generations": generations,
        "reauthenticationRequired": reauthentication_required,
    }))?;
    let quarantine = generations
        .iter()
        .map(|id| {
            let generation = Generation::resolve_at(account_root.to_owned(), id)?;
            Ok((
                generation.root.join(format!("quarantine-{}.json", run.id)),
                bytes.clone(),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Proof {
        generations,
        reauthentication_required,
        quarantine,
    })
}

impl Proof {
    /// Written before settlement; identical retries are safe after a DB error.
    pub(crate) fn retain(&self) -> Result<()> {
        for (path, bytes) in &self.quarantine {
            match private::read(path, MAX_RECORD) {
                Ok(previous) if previous == *bytes => (),
                Ok(_) => return Err(refusal()),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    private::create(path, bytes)?
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use std::io::Write as _;

    pub(crate) const STOPPED_PID: u32 = i32::MAX as u32;
    const TOKEN: &str = "sk-ant-oat01-synthetic_recovery_token";

    #[derive(Clone, Copy)]
    pub(crate) enum ActiveGeneration {
        None,
        Pending,
        Previous,
    }

    pub(crate) struct Evidence {
        pub generation: String,
        pub call: String,
        pub intent: PathBuf,
        pub quarantine: PathBuf,
        files: Vec<(PathBuf, Vec<u8>)>,
    }

    pub(crate) fn write_provider_fixture(path: &Path, bytes: &[u8]) {
        // This file belongs to the official provider, whose dotfile name is
        // deliberately outside xcb's atomic-publication name grammar.
        private::check_directory(path.parent().unwrap()).unwrap();
        let mut file = crate::os::no_follow(
            crate::os::owner_only(std::fs::OpenOptions::new().write(true).create_new(true)),
            true,
        )
        .open(path)
        .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
        private::check_file(&file, MAX_RECORD as u64).unwrap();
        assert_eq!(private::read(path, MAX_RECORD).unwrap(), bytes);
    }

    impl Evidence {
        pub(crate) fn assert_retained(&self) {
            for (path, bytes) in &self.files {
                assert_eq!(private::read(path, MAX_RECORD).unwrap(), *bytes);
            }
        }

        pub(crate) fn assert_quarantined_without_secrets(&self) {
            let bytes = private::read(&self.quarantine, MAX_RECORD).unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(!text.contains(TOKEN));
            assert!(!text.contains("private-refresh-sentinel"));
            assert!(!text.contains("fixture@example.test"));
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["generations"][0], self.generation);
        }
    }

    pub(crate) fn evidence(
        store: &Store,
        run: &RunRecord,
        active: ActiveGeneration,
        operation: &str,
    ) -> Evidence {
        let generation = Generation::create(store, run).unwrap();
        let account_root = store.account_root(&run.account).unwrap();
        let mut files = vec![];
        let mut retain = |path: PathBuf, bytes: Vec<u8>| {
            private::create(&path, &bytes).unwrap();
            files.push((path, bytes));
        };
        // Recovery must preserve both legacy credentials and the official
        // CLI's possible private file fallback without activating either.
        retain(
            account_root.join("subscription-token"),
            TOKEN.as_bytes().to_vec(),
        );
        let active_id = match active {
            ActiveGeneration::None => None,
            ActiveGeneration::Pending => Some(generation.id.clone()),
            ActiveGeneration::Previous => Some(Generation::create(store, run).unwrap().id),
        };
        if let Some(id) = active_id {
            retain(
                account_root.join(ACTIVE),
                serde_json::to_vec(&Active {
                    version: 1,
                    account: run.account.clone(),
                    generation: id,
                    account_uuid: "00000000-0000-4000-8000-000000000001".into(),
                    email: "fixture@example.test".into(),
                    scopes: vec!["user:profile".into(), "user:inference".into()],
                    expires_at_ms: 1,
                    access_token: TOKEN.into(),
                })
                .unwrap(),
            );
        }
        let fallback = generation.profile.join(".credentials.json");
        let fallback_bytes = br#"{"refreshToken":"private-refresh-sentinel"}"#.to_vec();
        write_provider_fixture(&fallback, &fallback_bytes);
        files.push((fallback, fallback_bytes));
        let publication = (operation == "publish").then(|| "a".repeat(64));
        let effect = effect_intent(store, run, &generation, operation, publication).unwrap();
        let intent = generation
            .root
            .join("effects")
            .join(format!("{}.json", effect.call));
        files.push((intent.clone(), private::read(&intent, MAX_RECORD).unwrap()));
        let declaration = generation.root.join("intent.json");
        files.push((
            declaration.clone(),
            private::read(&declaration, MAX_RECORD).unwrap(),
        ));
        let host_operation = if operation == "publish" {
            "host_auth_claude_publish"
        } else {
            "host_auth_claude_oauth"
        };
        store
            .begin_tool(run, &effect.call, host_operation, &effect.digest)
            .unwrap();
        let marker = if matches!(operation, "keychain-read" | "keychain-preflight") {
            KEYCHAIN_CUSTODY
        } else {
            AUTH_CUSTODY
        };
        store.mark_capability_starting(run, marker).unwrap();
        store
            .mark_capability_spawned(run, marker, STOPPED_PID)
            .unwrap();
        Evidence {
            generation: generation.id,
            call: effect.call,
            intent,
            quarantine: generation.root.join(format!("quarantine-{}.json", run.id)),
            files,
        }
    }
}
