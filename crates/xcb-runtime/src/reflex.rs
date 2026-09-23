//! Reflex runtime: runs reflex programs as ALGAL organisms, keeps the local
//! observation ledger, and fits and promotes parameter generations.
//!
//! A decision is `program(features, params, evidence)`. The program is a
//! digest-pinned, effect-free organism (only `input` and `expr` cells), so
//! every decision is a replayable ALGAL receipt. Parameters are data: learning
//! appends a generation without changing the program digest. A user may
//! replace a program by placing an admissible organism at
//! `<state>/reflexes/<name>.algal.json`; its digest is recorded with every
//! observation so evidence is never mixed across programs silently.
//!
//! The ledger stores numeric features, decisions and labels — never prompt
//! or response text.

use crate::{Error, Result, config::ReflexMode, digest, now_ms};
use algal::{
    contract::Manifest,
    effects::Host,
    graph::{self, Transports},
    runtime,
    store::Store as AlgalStore,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use xcb_core::reflex::{
    self, Comparison, Example, Features, FitOptions, Head, Params, Reflex, heads_for,
};

const ROUTE_PROGRAM: &str = include_str!("../reflexes/route.algal.json");
const SETTLE_PROGRAM: &str = include_str!("../reflexes/settle.algal.json");
const MAX_PROGRAM_BYTES: u64 = 64 * 1024;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_RUN_TIME: Duration = Duration::from_secs(5);
const MAX_OBSERVATIONS: i64 = 20_000;
const MAX_GENERATIONS: i64 = 256;
/// A training pass runs after every this-many new labels.
pub const TRAIN_EVERY: i64 = 16;

pub fn shipped_source(reflex: Reflex) -> &'static str {
    match reflex {
        Reflex::Route => ROUTE_PROGRAM,
        Reflex::Settle => SETTLE_PROGRAM,
    }
}

pub fn custom_path(root: &Path, reflex: Reflex) -> PathBuf {
    root.join("reflexes")
        .join(format!("{}.algal.json", reflex.as_str()))
}

pub struct Program {
    manifest: Manifest,
    pub digest: String,
    pub custom: bool,
}

/// Count serialized bytes without allocating an oversized intermediate string.
fn json_within_limit(value: &impl Serialize, maximum: usize) -> bool {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::other("reflex JSON budget exceeded"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(maximum), value).is_ok()
}

/// Admission for reflex programs: an ALGAL organism with only `input`,
/// `const` and `expr` cells, no agent calls, and the reflex interface
/// (`features`, `params`, `evidence` in; `decision` out). Anything with an
/// effect is rejected, so a replaced program cannot reach a provider, a tool
/// or the filesystem.
pub fn admit(source: &Value) -> Result<(Manifest, String)> {
    const REJECTED: &str = "reflex program rejected";
    if !json_within_limit(source, MAX_PROGRAM_BYTES as usize) {
        return Err(Error::Unavailable(REJECTED));
    }
    let cells = source["cells"]
        .as_array()
        .ok_or(Error::Unavailable(REJECTED))?;
    let effect_free = cells.iter().all(|cell| {
        matches!(
            cell["kind"].as_str(),
            Some("input") | Some("const") | Some("expr")
        )
    });
    let inputs: BTreeSet<_> = source["interface"]["inputs"]
        .as_object()
        .map(|inputs| inputs.keys().map(String::as_str).collect())
        .unwrap_or_default();
    if !effect_free
        || source["budgets"]["maxAgentCalls"] != 0
        || inputs != BTreeSet::from(["evidence", "features", "params"])
        || source["interface"]["outputs"].get("decision").is_none()
    {
        return Err(Error::Unavailable(REJECTED));
    }
    let manifest = Manifest::parse(source).map_err(|_| Error::Unavailable(REJECTED))?;
    // ALGAL's general-purpose ceilings are too generous for a policy decision
    // in the supervisor. Keep custom programs within the shipped fast lane.
    let budgets = &manifest.budgets;
    if manifest.cells.len() > 16
        || manifest.edges.len() > 64
        || budgets.max_steps > 64
        || budgets.max_work > 100_000
        || budgets.max_context_bytes > MAX_CONTEXT_BYTES
        || budgets.max_output_bytes > MAX_OUTPUT_BYTES
        || budgets.max_depth > 1
    {
        return Err(Error::Unavailable(REJECTED));
    }
    let compiled = graph::compile(
        manifest.clone(),
        &mut AlgalStore::default(),
        &BTreeMap::new(),
        &BTreeMap::new(),
        0,
    )
    .map_err(|_| Error::Unavailable(REJECTED))?;
    let signature =
        graph::interface_signature(&compiled).map_err(|_| Error::Unavailable(REJECTED))?;
    if signature.outputs.len() != 1
        || !signature.outputs.contains_key("decision")
        || signature
            .inputs
            .values()
            .chain(signature.outputs.values())
            .any(|port| port["type"] != "json" || port["many"] == true || port["optional"] == true)
    {
        return Err(Error::Unavailable(REJECTED));
    }
    let probe = graph::interface_args(
        &manifest,
        &json!({"features": {}, "params": {}, "evidence": {}}),
    )
    .map_err(|_| Error::Unavailable(REJECTED))?;
    for cell in &manifest.cells {
        if cell["kind"] == "input"
            && cell["outputs"].as_object().is_none_or(|ports| {
                ports.iter().any(|(name, port)| {
                    port["optional"] != true
                        && probe[cell["id"].as_str().unwrap()].get(name).is_none()
                })
            })
        {
            return Err(Error::Unavailable(REJECTED));
        }
    }
    let digest = manifest
        .digest()
        .map_err(|_| Error::Unavailable(REJECTED))?;
    Ok((manifest, digest))
}

fn read_program(path: &Path) -> Result<Value> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits() as i32,
        )
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_PROGRAM_BYTES {
        return Err(Error::Unavailable("reflex program rejected"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROGRAM_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PROGRAM_BYTES as usize {
        return Err(Error::Unavailable("reflex program rejected"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// The program a reflex runs: an admissible custom program when one is
/// installed, otherwise the shipped one. A rejected custom program is
/// reported, never silently executed.
pub fn program(root: &Path, reflex: Reflex) -> Result<(Program, Option<&'static str>)> {
    let path = custom_path(root, reflex);
    let mut fault = None;
    match read_program(&path).and_then(|source| admit(&source)) {
        Ok((manifest, digest)) => {
            return Ok((
                Program {
                    manifest,
                    digest,
                    custom: true,
                },
                None,
            ));
        }
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => fault = Some("custom reflex program rejected; using the shipped program"),
    }
    let (manifest, digest) = admit(&serde_json::from_str(shipped_source(reflex))?)?;
    Ok((
        Program {
            manifest,
            digest,
            custom: false,
        },
        fault,
    ))
}

/// One reflex decision with everything needed to audit and learn from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub reflex: Reflex,
    pub head: String,
    /// `frontier`/`standard` for route; a settle category name for settle.
    pub value: String,
    pub gate: String,
    pub score_milli: i64,
    pub features: Features,
    pub params_version: u32,
    pub program: String,
    pub receipt: String,
}

fn head_input(head: &Head) -> Value {
    json!({
        "bias": head.bias,
        "cut": head.threshold_logit(),
        "terms": head
            .weights
            .iter()
            .map(|(name, weight)| json!({"f": name, "w": weight}))
            .collect::<Vec<_>>(),
    })
}

/// Runs a reflex program. Pure: the same program, parameters, features and
/// evidence always produce the same decision and receipt digest.
pub async fn run(
    program: &Program,
    params: &Params,
    features: &Features,
    evidence: Value,
) -> Result<Decision> {
    const FAILED: &str = "reflex program failed";
    params.validate()?;
    if features.len() > reflex::MAX_FEATURES || features.values().any(|value| !value.is_finite()) {
        return Err(Error::Unavailable(FAILED));
    }
    let heads: serde_json::Map<String, Value> = params
        .heads
        .iter()
        .map(|(name, head)| (name.clone(), head_input(head)))
        .collect();
    let input = json!({
        "features": features,
        "params": heads,
        "evidence": evidence,
    });
    if !json_within_limit(&input, MAX_CONTEXT_BYTES) {
        return Err(Error::Unavailable(FAILED));
    }
    let args =
        graph::interface_args(&program.manifest, &input).map_err(|_| Error::Unavailable(FAILED))?;
    let manifest = program.manifest.clone();
    let started = Instant::now();
    // Join bounded pure work instead of running CPU evaluation on the async
    // supervisor or dropping it behind a timeout. No host or transport exists.
    let (receipt, outputs) = tokio::task::spawn_blocking(move || -> Result<_> {
        let executor = tokio::runtime::Builder::new_current_thread().build()?;
        let receipt = executor
            .block_on(runtime::run(
                manifest.clone(),
                args,
                &mut AlgalStore::default(),
                &mut Host::default(),
                &Transports::new(),
                None,
            ))
            .map_err(|_| Error::Unavailable(FAILED))?;
        if receipt["outcome"] != "complete" {
            return Err(Error::Unavailable(FAILED));
        }
        let outputs =
            runtime::outputs(&manifest, &receipt).map_err(|_| Error::Unavailable(FAILED))?;
        if !json_within_limit(&outputs, MAX_OUTPUT_BYTES) {
            return Err(Error::Unavailable(FAILED));
        }
        Ok((receipt, outputs))
    })
    .await
    .map_err(|_| Error::Unavailable(FAILED))??;
    if started.elapsed() > MAX_RUN_TIME {
        return Err(Error::Unavailable(FAILED));
    }
    let decision = &outputs["decision"];
    let value = match params.reflex {
        Reflex::Route => decision["route"]
            .as_str()
            .filter(|route| ["frontier", "standard"].contains(route)),
        Reflex::Settle => decision["category"]
            .as_str()
            .filter(|category| reflex::Category::parse(category).is_some()),
    }
    .ok_or(Error::Unavailable(FAILED))?;
    let head = decision["head"]
        .as_str()
        .filter(|head| heads_for(params.reflex).contains(head))
        .ok_or(Error::Unavailable(FAILED))?;
    Ok(Decision {
        reflex: params.reflex,
        head: head.to_owned(),
        value: value.to_owned(),
        gate: xcb_core::display_text(decision["gate"].as_str().unwrap_or(""), 32),
        score_milli: decision["score"].as_f64().unwrap_or(0.0).round() as i64,
        features: features.clone(),
        params_version: params.version,
        program: program.digest.clone(),
        receipt: receipt["digest"]
            .as_str()
            .ok_or(Error::Unavailable(FAILED))?
            .to_owned(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainReport {
    pub reflex: Reflex,
    pub from_version: u32,
    pub promoted_version: Option<u32>,
    pub labeled: u32,
    pub heads: BTreeMap<String, Comparison>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeadStatus {
    pub labeled: u32,
    pub positives: u32,
    /// Active head measured on the stable holdout split only, which training
    /// never sees.
    pub holdout: Option<reflex::Metrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub reflex: Reflex,
    pub mode: ReflexMode,
    pub learn: bool,
    pub version: u32,
    pub generations: u32,
    pub trained_on: u32,
    pub observations: u32,
    pub labeled: u32,
    pub program: String,
    pub custom_program: bool,
    pub program_fault: Option<&'static str>,
    pub heads: BTreeMap<String, HeadStatus>,
    pub params: Params,
}

/// A labeled example supplied from outside the ledger (bootstrap import).
#[derive(Debug, Clone)]
pub struct Imported {
    pub id: String,
    pub head: String,
    pub features: Features,
    pub label: bool,
    pub weight: f64,
}

pub struct ReflexStore {
    root: PathBuf,
    db: Mutex<Connection>,
}

impl ReflexStore {
    pub fn open(root: &Path) -> Result<Self> {
        let connection = Connection::open(root.join("reflex.sqlite"))?;
        connection.busy_timeout(std::time::Duration::from_secs(15))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            return Err(Error::Unavailable(
                "reflex state was written by a newer xcb",
            ));
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS generations(reflex TEXT NOT NULL, version INTEGER NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(reflex,version));
             CREATE TABLE IF NOT EXISTS observations(id TEXT PRIMARY KEY, reflex TEXT NOT NULL, subject TEXT NOT NULL, head TEXT NOT NULL, value TEXT NOT NULL, gate TEXT NOT NULL, score INTEGER NOT NULL, features TEXT NOT NULL, params_version INTEGER NOT NULL, program TEXT NOT NULL, receipt TEXT NOT NULL, at_ms INTEGER NOT NULL, label INTEGER, label_weight REAL, label_source TEXT, labeled_at_ms INTEGER);
             CREATE INDEX IF NOT EXISTS observations_subject ON observations(reflex,subject,at_ms);
             PRAGMA user_version=1;",
        )?;
        Ok(Self {
            root: root.to_owned(),
            db: Mutex::new(connection),
        })
    }

    fn db(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| Error::Unavailable("reflex store lock poisoned"))
    }

    /// The active generation, or the shipped prior when none was promoted.
    pub fn active(&self, reflex: Reflex) -> Result<Params> {
        let payload: Option<String> = self
            .db()?
            .query_row(
                "SELECT payload FROM generations WHERE reflex=?1 AND active=1 ORDER BY version DESC LIMIT 1",
                [reflex.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match payload {
            Some(payload) => {
                let params: Params = serde_json::from_str(&payload)?;
                params.validate()?;
                // A generation whose feature schema no longer matches this
                // build's prior is not trusted; the prior takes over.
                let prior = reflex::prior(reflex);
                let schema = |params: &Params| {
                    params
                        .heads
                        .iter()
                        .map(|(name, head)| (name.clone(), head.weights.keys().cloned().collect()))
                        .collect::<BTreeMap<_, BTreeSet<_>>>()
                };
                Ok(if schema(&params) == schema(&prior) {
                    params
                } else {
                    prior
                })
            }
            None => Ok(reflex::prior(reflex)),
        }
    }

    /// Decides with the active generation (or the prior when `prior_only`).
    pub async fn decide(
        &self,
        reflex: Reflex,
        features: &Features,
        evidence: Value,
        prior_only: bool,
    ) -> Result<Decision> {
        let params = if prior_only {
            reflex::prior(reflex)
        } else {
            self.active(reflex)?
        };
        let (program, _) = program(&self.root, reflex)?;
        run(&program, &params, features, evidence).await
    }

    /// Records one decision for `subject`. Idempotent per subject.
    pub fn observe(&self, subject: &str, decision: &Decision) -> Result<()> {
        let id = format!(
            "obs_{}",
            &digest(format!(
                "xcb-reflex-obs-v1\0{}\0{subject}",
                decision.reflex.as_str()
            ))[..32]
        );
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT OR IGNORE INTO observations(id,reflex,subject,head,value,gate,score,features,params_version,program,receipt,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                id,
                decision.reflex.as_str(),
                xcb_core::display_text(subject, 200),
                decision.head,
                decision.value,
                decision.gate,
                decision.score_milli,
                serde_json::to_string(&decision.features)?,
                decision.params_version,
                decision.program,
                decision.receipt,
                now_ms() as i64,
            ],
        )?;
        // Retention keeps labeled evidence longest: unlabeled rows go first.
        tx.execute(
            "DELETE FROM observations WHERE id IN (SELECT id FROM observations WHERE reflex=?1 ORDER BY label IS NOT NULL, at_ms LIMIT max(0,(SELECT count(*) FROM observations WHERE reflex=?1)-?2))",
            params![decision.reflex.as_str(), MAX_OBSERVATIONS],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The most recent observation for `subject`, or for any subject that
    /// starts with `subject#` (per-turn subjects of one task).
    pub fn latest(&self, reflex: Reflex, subject: &str) -> Result<Option<(String, String)>> {
        let prefix = format!("{subject}#");
        Ok(self
            .db()?
            .query_row(
                "SELECT id,value FROM observations WHERE reflex=?1 AND (subject=?2 OR substr(subject,1,?3)=?4) ORDER BY at_ms DESC, id DESC LIMIT 1",
                params![reflex.as_str(), subject, prefix.len() as i64, prefix],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Labels the latest observation for `subject`. A label replaces an
    /// existing one only with at least equal confidence, so explicit feedback
    /// (weight 1) overrides inferred behavior and not the reverse.
    pub fn label(
        &self,
        reflex: Reflex,
        subject: &str,
        label: bool,
        weight: f64,
        source: &str,
    ) -> Result<bool> {
        if !(weight > 0.0 && weight <= 1.0) {
            return Err(xcb_core::Error::Invalid("label weight").into());
        }
        let Some((id, _)) = self.latest(reflex, subject)? else {
            return Ok(false);
        };
        let changed = self.db()?.execute(
            "UPDATE observations SET label=?1,label_weight=?2,label_source=?3,labeled_at_ms=?4 WHERE id=?5 AND (label IS NULL OR label_weight<=?2)",
            params![
                label,
                weight,
                xcb_core::display_text(source, 48),
                now_ms() as i64,
                id
            ],
        )?;
        Ok(changed == 1)
    }

    /// Labels and, when learning is enabled, trains every [`TRAIN_EVERY`]
    /// labels. Training failure never fails the caller's operation.
    pub fn label_and_learn(
        &self,
        reflex: Reflex,
        subject: &str,
        label: bool,
        weight: f64,
        source: &str,
        learn: bool,
    ) -> Result<Option<TrainReport>> {
        if !self.label(reflex, subject, label, weight, source)? || !learn {
            return Ok(None);
        }
        let labeled: i64 = self.db()?.query_row(
            "SELECT count(*) FROM observations WHERE reflex=?1 AND label IS NOT NULL",
            [reflex.as_str()],
            |row| row.get(0),
        )?;
        if labeled % TRAIN_EVERY != 0 {
            return Ok(None);
        }
        self.train(reflex, FitOptions::default()).map(Some)
    }

    pub fn import(&self, reflex: Reflex, rows: &[Imported]) -> Result<u32> {
        let (program, _) = program(&self.root, reflex)?;
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut inserted = 0;
        for row in rows.iter().take(reflex::MAX_EXAMPLES) {
            if !heads_for(reflex).contains(&row.head.as_str())
                || !(row.weight > 0.0 && row.weight <= 1.0)
            {
                return Err(xcb_core::Error::Invalid("imported example").into());
            }
            let id = format!(
                "obs_{}",
                &digest(format!(
                    "xcb-reflex-import-v1\0{}\0{}",
                    reflex.as_str(),
                    row.id
                ))[..32]
            );
            inserted += tx.execute(
                "INSERT OR IGNORE INTO observations(id,reflex,subject,head,value,gate,score,features,params_version,program,receipt,at_ms,label,label_weight,label_source,labeled_at_ms) VALUES(?1,?2,?3,?4,'imported','import',0,?5,0,?6,'',?7,?8,?9,'import',?7)",
                params![
                    id,
                    reflex.as_str(),
                    format!("import:{}", xcb_core::display_text(&row.id, 160)),
                    row.head,
                    serde_json::to_string(&row.features)?,
                    program.digest,
                    now_ms() as i64,
                    row.label,
                    row.weight,
                ],
            )? as u32;
        }
        tx.commit()?;
        Ok(inserted)
    }

    fn examples(&self, reflex: Reflex, head: &str) -> Result<Vec<Example>> {
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT id,features,label,label_weight FROM observations WHERE reflex=?1 AND head=?2 AND label IS NOT NULL ORDER BY id LIMIT ?3",
        )?;
        let rows = query.query_map(
            params![reflex.as_str(), head, reflex::MAX_EXAMPLES as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, f64>(3)?,
                ))
            },
        )?;
        let mut examples = Vec::new();
        for row in rows {
            let (id, features, label, weight) = row?;
            let Ok(features) = serde_json::from_str::<Features>(&features) else {
                continue;
            };
            examples.push(Example {
                id,
                features,
                label,
                weight,
            });
        }
        Ok(examples)
    }

    /// Fits a candidate for every head from the training split and promotes
    /// a new generation when any head beats the active one on the stable
    /// holdout split (see [`reflex::compare`]).
    pub fn train(&self, reflex: Reflex, options: FitOptions) -> Result<TrainReport> {
        let current = self.active(reflex)?;
        // Every candidate is the posterior of the shipped prior given all
        // retained evidence, so repeated passes over the same evidence
        // converge instead of drifting away from the prior.
        let prior = reflex::prior(reflex);
        let mut heads = current.heads.clone();
        let mut comparisons = BTreeMap::new();
        let mut promoted = false;
        let mut labeled = 0u32;
        for (name, head) in &current.heads {
            let examples = self.examples(reflex, name)?;
            labeled += examples.len() as u32;
            let (holdout, training): (Vec<_>, Vec<_>) = examples
                .into_iter()
                .partition(|example| reflex::is_holdout(&example.id));
            if training.is_empty() {
                continue;
            }
            let candidate = reflex::fit(prior.head(name)?, &training, options)?;
            let comparison = reflex::compare(head, &candidate, &holdout);
            if comparison.promoted {
                heads.insert(name.clone(), candidate);
                promoted = true;
            }
            comparisons.insert(name.clone(), comparison);
        }
        let mut promoted_version = None;
        if promoted {
            let mut db = self.db()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let latest: i64 = tx.query_row(
                "SELECT COALESCE(max(version),0) FROM generations WHERE reflex=?1",
                [reflex.as_str()],
                |row| row.get(0),
            )?;
            if latest >= MAX_GENERATIONS {
                tx.execute(
                    "DELETE FROM generations WHERE reflex=?1 AND active=0 AND version=(SELECT min(version) FROM generations WHERE reflex=?1 AND active=0)",
                    [reflex.as_str()],
                )?;
            }
            let version =
                u32::try_from(latest + 1).map_err(|_| Error::Unavailable("reflex version"))?;
            let next = Params {
                reflex,
                version,
                parent: Some(current.version),
                heads,
                trained_on: labeled,
                evidence: comparisons
                    .iter()
                    .filter(|(_, comparison)| comparison.promoted)
                    .map(|(name, comparison)| (name.clone(), comparison.clone()))
                    .collect(),
            };
            next.validate()?;
            tx.execute(
                "UPDATE generations SET active=0 WHERE reflex=?1",
                [reflex.as_str()],
            )?;
            tx.execute(
                "INSERT INTO generations(reflex,version,active,created_at,payload) VALUES(?1,?2,1,?3,?4)",
                params![reflex.as_str(), version, now_ms() as i64, serde_json::to_string(&next)?],
            )?;
            tx.commit()?;
            promoted_version = Some(version);
        }
        Ok(TrainReport {
            reflex,
            from_version: current.version,
            promoted_version,
            labeled,
            heads: comparisons,
        })
    }

    /// Reactivates a recorded generation; version 0 restores the prior.
    pub fn rollback(&self, reflex: Reflex, version: u32) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version != 0 {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM generations WHERE reflex=?1 AND version=?2)",
                params![reflex.as_str(), version],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(Error::Unavailable("reflex generation not found"));
            }
        }
        tx.execute(
            "UPDATE generations SET active=(version=?2) WHERE reflex=?1",
            params![reflex.as_str(), version],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn status(&self, reflex: Reflex, mode: ReflexMode, learn: bool) -> Result<Status> {
        let params = self.active(reflex)?;
        let (program, program_fault) = program(&self.root, reflex)?;
        let (generations, observations, labeled): (i64, i64, i64) = {
            let db = self.db()?;
            (
                db.query_row(
                    "SELECT count(*) FROM generations WHERE reflex=?1",
                    [reflex.as_str()],
                    |row| row.get(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM observations WHERE reflex=?1",
                    [reflex.as_str()],
                    |row| row.get(0),
                )?,
                db.query_row(
                    "SELECT count(*) FROM observations WHERE reflex=?1 AND label IS NOT NULL",
                    [reflex.as_str()],
                    |row| row.get(0),
                )?,
            )
        };
        let mut heads = BTreeMap::new();
        for (name, head) in &params.heads {
            let examples = self.examples(reflex, name)?;
            let holdout: Vec<_> = examples
                .iter()
                .filter(|example| reflex::is_holdout(&example.id))
                .cloned()
                .collect();
            heads.insert(
                name.clone(),
                HeadStatus {
                    labeled: examples.len() as u32,
                    positives: examples.iter().filter(|example| example.label).count() as u32,
                    holdout: (!holdout.is_empty()).then(|| reflex::evaluate(head, &holdout)),
                },
            );
        }
        Ok(Status {
            reflex,
            mode,
            learn,
            version: params.version,
            generations: generations as u32,
            trained_on: params.trained_on,
            observations: observations as u32,
            labeled: labeled as u32,
            program: program.digest,
            custom_program: program.custom,
            program_fault,
            heads,
            params,
        })
    }
}

/// Route tier of a model identity for labels: the frontier tier is the top
/// recognized quality band (Astra, Opus, Fable), independent of effort.
pub fn frontier_model(model: &xcb_core::models::ModelChoice) -> bool {
    crate::routing::family_quality(model).is_some_and(|quality| quality >= 97)
}

/// Evidence for the settle program from a settled turn.
pub fn settle_evidence(state: xcb_core::session::State, features: &Features) -> Value {
    json!({
        "state": reflex::state_name(state),
        "limit": features.get("limit").copied().unwrap_or(0.0),
        "blocked": features.get("blocked").copied().unwrap_or(0.0),
        "done_claim": features.get("done_claim").copied().unwrap_or(0.0),
    })
}

/// One bootstrap example: text plus a label, optionally with judge scores
/// (criterion-index scale) for the judged route head. Only derived features
/// are stored; the text never enters the ledger.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportLine {
    pub id: String,
    pub text: String,
    pub label: bool,
    #[serde(default)]
    pub weight: Option<f64>,
    #[serde(default)]
    pub judge: Option<JudgeScores>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeScores {
    pub difficulty: f64,
    pub scope: f64,
    pub ambiguity: f64,
    pub stakes: f64,
    pub frontier: f64,
}

/// Derives the features a live decision would have seen for `line`.
pub fn import_example(reflex: Reflex, line: &ImportLine) -> Result<Imported> {
    let (head, features) = match reflex {
        Reflex::Route => {
            let class = crate::routing::classify_task(&line.text);
            let features = reflex::route_features(
                &line.text,
                class == crate::routing::TaskClass::Complex,
                class == crate::routing::TaskClass::Routine,
            );
            match line.judge {
                Some(judge) => {
                    let scores = [judge.difficulty, judge.scope, judge.ambiguity, judge.stakes];
                    if scores.iter().any(|score| !(0.0..=4.0).contains(score))
                        || !(0.0..=1.0).contains(&judge.frontier)
                    {
                        return Err(xcb_core::Error::Invalid("imported judge scores").into());
                    }
                    (
                        reflex::ROUTE_JUDGED,
                        reflex::with_judge_evidence(
                            features,
                            judge.difficulty,
                            judge.scope,
                            judge.ambiguity,
                            judge.stakes,
                            judge.frontier,
                        ),
                    )
                }
                None => (reflex::ROUTE_PLAIN, features),
            }
        }
        Reflex::Settle => {
            let facts = xcb_core::policy::TurnFacts {
                terminal: xcb_core::policy::Terminal::Completed,
                joined: true,
                effects: xcb_core::policy::EffectState::Settled,
                pending_attention: false,
                failure: None,
            };
            (
                reflex::SETTLE_UNFINISHED,
                reflex::settle_features(&line.text, &facts),
            )
        }
    };
    Ok(Imported {
        id: line.id.clone(),
        head: head.into(),
        features,
        label: line.label,
        weight: line.weight.unwrap_or(1.0),
    })
}

/// Parses a JSONL bootstrap corpus (one [`ImportLine`] per line).
pub fn parse_import(reflex: Reflex, source: &str) -> Result<Vec<Imported>> {
    source
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(reflex::MAX_EXAMPLES)
        .map(|line| import_example(reflex, &serde_json::from_str::<ImportLine>(line)?))
        .collect()
}

/// Parses a user label word for a reflex: `frontier`/`standard` for route,
/// `unfinished`/`done` for settle.
pub fn parse_label(reflex: Reflex, word: &str) -> Option<bool> {
    match (reflex, word) {
        (Reflex::Route, "frontier") | (Reflex::Settle, "unfinished" | "stopped_short") => {
            Some(true)
        }
        (Reflex::Route, "standard") | (Reflex::Settle, "done") => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xcb_core::{
        policy::{EffectState, Terminal, TurnFacts},
        reflex::{ROUTE_JUDGED, ROUTE_PLAIN, route_features, settle_features, with_judge_evidence},
        session::State,
    };

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn settled() -> TurnFacts {
        TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::Settled,
            pending_attention: false,
            failure: None,
        }
    }

    #[test]
    fn shipped_programs_are_admissible_and_effect_free() {
        for reflex in Reflex::ALL {
            let source: Value = serde_json::from_str(shipped_source(reflex)).unwrap();
            admit(&source).unwrap();
            let mut effectful = source.clone();
            effectful["cells"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id": "tool", "kind": "fn", "fn": "echo.v1"}));
            assert!(admit(&effectful).is_err());
            let mut agent = source;
            agent["budgets"]["maxAgentCalls"] = json!(1);
            assert!(admit(&agent).is_err());
        }
    }

    #[test]
    fn custom_admission_bounds_work_and_compiles_the_interface() {
        let source: Value = serde_json::from_str(ROUTE_PROGRAM).unwrap();
        for (key, excessive) in [
            ("maxSteps", 65),
            ("maxWork", 100_001),
            ("maxContextBytes", 65_537),
            ("maxOutputBytes", 16_385),
            ("maxDepth", 2),
        ] {
            let mut invalid = source.clone();
            invalid["budgets"][key] = json!(excessive);
            assert!(admit(&invalid).is_err(), "{key}");
        }
        let mut invalid = source.clone();
        invalid["interface"]["outputs"]["decision"]["cell"] = json!("missing");
        assert!(admit(&invalid).is_err());
        let mut invalid = source.clone();
        invalid["interface"]["outputs"]["extra"] =
            invalid["interface"]["outputs"]["decision"].clone();
        assert!(admit(&invalid).is_err());
        let mut invalid = source.clone();
        invalid["cells"][0]["outputs"]["features"] = json!("text");
        assert!(admit(&invalid).is_err());
        let mut invalid = source.clone();
        invalid["cells"][0]["outputs"]["hidden"] = json!("json");
        assert!(admit(&invalid).is_err());
        let mut invalid = source.clone();
        for index in 0..16 {
            invalid["cells"].as_array_mut().unwrap().push(json!({
                "id": format!("extra{index}"), "kind": "const", "value": {}
            }));
        }
        assert!(admit(&invalid).is_err());
        let mut oversized = source;
        oversized["padding"] = json!("x".repeat(MAX_PROGRAM_BYTES as usize));
        assert!(admit(&oversized).is_err());
    }

    #[test]
    fn custom_file_rejects_symlink_fifo_and_oversized_input() {
        let dir = temp();
        std::fs::create_dir(dir.path().join("reflexes")).unwrap();
        let target = dir.path().join("source.json");
        std::fs::write(&target, ROUTE_PROGRAM).unwrap();
        let path = custom_path(dir.path(), Reflex::Route);
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let (used, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(!used.custom && fault.is_some());
        std::fs::remove_file(&path).unwrap();
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, rustix::fs::Mode::RUSR).unwrap();
        let (used, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(!used.custom && fault.is_some());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, vec![b' '; MAX_PROGRAM_BYTES as usize + 1]).unwrap();
        let (used, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(!used.custom && fault.is_some());
    }

    #[tokio::test]
    async fn bounded_runner_uses_the_declared_interface_and_rejects_exhaustion() {
        let source: Value =
            serde_json::from_str(&ROUTE_PROGRAM.replace("\"src\"", "\"origin\"")).unwrap();
        let (manifest, digest) = admit(&source).unwrap();
        let mut program = Program {
            manifest,
            digest,
            custom: true,
        };
        let params = reflex::prior(Reflex::Route);
        let features = route_features("refactor the parser", true, false);
        let evidence = json!({"substantial": false, "judged": false, "kind": null});
        assert_eq!(
            run(&program, &params, &features, evidence.clone())
                .await
                .unwrap()
                .value,
            "frontier"
        );
        assert!(
            run(
                &program,
                &params,
                &features,
                json!({"large": "x".repeat(MAX_CONTEXT_BYTES)})
            )
            .await
            .is_err()
        );
        for (key, limit) in [("maxSteps", 1), ("maxOutputBytes", 1)] {
            let mut limited = source.clone();
            limited["budgets"][key] = json!(limit);
            (program.manifest, program.digest) = admit(&limited).unwrap();
            assert!(
                run(&program, &params, &features, evidence.clone())
                    .await
                    .is_err(),
                "{key}"
            );
        }
    }

    #[tokio::test]
    async fn route_program_matches_the_fitted_head_and_gates() {
        let dir = temp();
        let (program, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(fault.is_none());
        let params = reflex::prior(Reflex::Route);
        let task = "migrate the session store to the new envelope format and update every caller";
        let features =
            with_judge_evidence(route_features(task, true, false), 3.9, 3.8, 2.4, 2.9, 0.62);
        let judged = |kind: &str| json!({"substantial": false, "judged": true, "kind": kind});
        let decision = run(&program, &params, &features, judged("refactor"))
            .await
            .unwrap();
        assert_eq!(decision.value, "frontier");
        assert_eq!(decision.head, ROUTE_JUDGED);
        // Same score as the native port of ALGAL's model router.
        assert_eq!(decision.score_milli, 533);
        assert_eq!(
            params.heads[ROUTE_JUDGED].decide(&features),
            decision.value == "frontier"
        );
        for kind in ["question", "probe"] {
            let gated = run(&program, &params, &features, judged(kind))
                .await
                .unwrap();
            assert_eq!(
                (gated.value.as_str(), gated.gate.as_str()),
                ("standard", "kind")
            );
        }
        let plain = json!({"substantial": false, "judged": false, "kind": null});
        for complex in [false, true] {
            let features = route_features("fix the thing", complex, false);
            let decision = run(&program, &params, &features, plain.clone())
                .await
                .unwrap();
            assert_eq!(decision.head, ROUTE_PLAIN);
            assert_eq!(decision.value == "frontier", complex);
        }
        let large = json!({"substantial": true, "judged": false, "kind": null});
        let decision = run(&program, &params, &route_features("x", false, true), large)
            .await
            .unwrap();
        assert_eq!(
            (decision.value.as_str(), decision.gate.as_str()),
            ("frontier", "substantial")
        );
        // Deterministic receipt.
        let again = run(&program, &params, &features, judged("refactor"))
            .await
            .unwrap();
        assert_eq!(
            again.receipt,
            run(&program, &params, &features, judged("refactor"))
                .await
                .unwrap()
                .receipt
        );
    }

    #[tokio::test]
    async fn settle_program_categorizes_turns() {
        let dir = temp();
        let (program, _) = program(dir.path(), Reflex::Settle).unwrap();
        let params = reflex::prior(Reflex::Settle);
        let decide = |text: &'static str, state: State, facts: TurnFacts| {
            let program = &program;
            let params = &params;
            async move {
                let features = settle_features(text, &facts);
                let evidence = settle_evidence(state, &features);
                run(program, params, &features, evidence)
                    .await
                    .unwrap()
                    .value
            }
        };
        assert_eq!(
            decide(
                "Parser updated. Next, I'll wire the CLI:",
                State::Idle,
                settled()
            )
            .await,
            "stopped_short"
        );
        assert_eq!(
            decide("All tests pass; the PR is merged.", State::Idle, settled()).await,
            "done"
        );
        assert_eq!(
            decide(
                "I can't reach the registry from this sandbox.",
                State::Idle,
                settled()
            )
            .await,
            "blocked"
        );
        assert_eq!(
            decide("Which schema should I use?", State::NeedsAnswer, settled()).await,
            "question"
        );
        let mut limited = settled();
        limited.terminal = Terminal::TurnLimit;
        assert_eq!(
            decide("working on it", State::Idle, limited).await,
            "interrupted"
        );
        assert_eq!(
            decide("please approve", State::NeedsApproval, settled()).await,
            "needs_approval"
        );
    }

    #[tokio::test]
    async fn custom_programs_replace_the_shipped_one_only_when_admissible() {
        let dir = temp();
        std::fs::create_dir_all(dir.path().join("reflexes")).unwrap();
        let path = custom_path(dir.path(), Reflex::Route);
        std::fs::write(&path, "{\"cells\":[{\"kind\":\"fn\"}]}").unwrap();
        let (program_used, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(!program_used.custom);
        assert!(fault.is_some());
        let mut source: Value = serde_json::from_str(ROUTE_PROGRAM).unwrap();
        source["name"] = json!("my route reflex");
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let (program_used, fault) = program(dir.path(), Reflex::Route).unwrap();
        assert!(program_used.custom && fault.is_none());
        let shipped = admit(&serde_json::from_str(ROUTE_PROGRAM).unwrap())
            .unwrap()
            .1;
        assert_ne!(program_used.digest, shipped);
    }

    #[tokio::test]
    async fn ledger_learns_promotes_and_rolls_back() {
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        assert_eq!(store.active(Reflex::Route).unwrap().version, 0);
        // Local evidence: imperative prompts are sent to frontier even without
        // a complex cue, which the prior's keyword head misses.
        let plain = json!({"substantial": false, "judged": false, "kind": null});
        for i in 0..160 {
            let imperative = i % 2 == 0;
            let task = if imperative {
                format!("build feature {i}")
            } else {
                format!("what is {i}")
            };
            let features = route_features(&task, false, false);
            let decision = store
                .decide(Reflex::Route, &features, plain.clone(), false)
                .await
                .unwrap();
            let subject = format!("t_{i}");
            store.observe(&subject, &decision).unwrap();
            store.observe(&subject, &decision).unwrap();
            store
                .label_and_learn(Reflex::Route, &subject, imperative, 1.0, "explicit", false)
                .unwrap();
        }
        let status = store
            .status(Reflex::Route, ReflexMode::Active, true)
            .unwrap();
        assert_eq!((status.observations, status.labeled), (160, 160));
        let report = store.train(Reflex::Route, FitOptions::default()).unwrap();
        assert_eq!(report.promoted_version, Some(1), "{report:?}");
        let learned = store.active(Reflex::Route).unwrap();
        assert_eq!(learned.parent, Some(0));
        let features = route_features("build the thing", false, false);
        assert!(learned.heads[ROUTE_PLAIN].decide(&features));
        assert!(!reflex::prior(Reflex::Route).heads[ROUTE_PLAIN].decide(&features));
        let decision = store
            .decide(Reflex::Route, &features, plain.clone(), false)
            .await
            .unwrap();
        assert_eq!(
            (decision.value.as_str(), decision.params_version),
            ("frontier", 1)
        );
        // The prior-only path is unaffected by learning.
        let prior = store
            .decide(Reflex::Route, &features, plain, true)
            .await
            .unwrap();
        assert_eq!(prior.value, "standard");
        // A second pass on the same evidence does not promote again.
        assert_eq!(
            store
                .train(Reflex::Route, FitOptions::default())
                .unwrap()
                .promoted_version,
            None
        );
        store.rollback(Reflex::Route, 0).unwrap();
        assert_eq!(store.active(Reflex::Route).unwrap().version, 0);
        store.rollback(Reflex::Route, 1).unwrap();
        assert_eq!(store.active(Reflex::Route).unwrap().version, 1);
        assert!(store.rollback(Reflex::Route, 9).is_err());
    }

    #[tokio::test]
    async fn explicit_labels_override_inferred_ones_but_not_the_reverse() {
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        let features = settle_features("Next, I'll add tests:", &settled());
        let decision = store
            .decide(
                Reflex::Settle,
                &features,
                settle_evidence(State::Idle, &features),
                false,
            )
            .await
            .unwrap();
        store.observe("t_1#2", &decision).unwrap();
        assert!(
            store
                .label(Reflex::Settle, "t_1", false, 0.5, "moved_on")
                .unwrap()
        );
        assert!(
            store
                .label(Reflex::Settle, "t_1", true, 1.0, "explicit")
                .unwrap()
        );
        assert!(
            !store
                .label(Reflex::Settle, "t_1", false, 0.5, "moved_on")
                .unwrap()
        );
        assert!(
            !store
                .label(Reflex::Settle, "t_9", true, 1.0, "explicit")
                .unwrap()
        );
        let examples = store.examples(Reflex::Settle, "unfinished").unwrap();
        assert_eq!(examples.len(), 1);
        assert!(examples[0].label);
    }

    #[test]
    fn imports_derive_features_without_keeping_text() {
        let source = concat!(
            "{\"id\":\"a\",\"text\":\"refactor the auth layer\",\"label\":true}\n",
            "{\"id\":\"b\",\"text\":\"what is this\",\"label\":false,\"judge\":{\"difficulty\":1,\"scope\":0,\"ambiguity\":1,\"stakes\":0,\"frontier\":0.1}}\n",
        );
        let rows = parse_import(Reflex::Route, source).unwrap();
        assert_eq!(rows[0].head, ROUTE_PLAIN);
        assert_eq!(rows[0].features["complex_cue"], 1.0);
        assert_eq!(rows[1].head, ROUTE_JUDGED);
        assert!(parse_import(Reflex::Route, "{\"id\":\"c\",\"text\":\"x\",\"label\":true,\"judge\":{\"difficulty\":9,\"scope\":0,\"ambiguity\":0,\"stakes\":0,\"frontier\":0}}").is_err());
        let settle = parse_import(
            Reflex::Settle,
            "{\"id\":\"s\",\"text\":\"Next, I'll add tests:\",\"label\":true}",
        )
        .unwrap();
        assert_eq!(settle[0].features["ends_colon"], 1.0);
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        assert_eq!(store.import(Reflex::Route, &rows).unwrap(), 2);
        assert_eq!(store.import(Reflex::Route, &rows).unwrap(), 0);
        assert_eq!(parse_label(Reflex::Route, "frontier"), Some(true));
        assert_eq!(parse_label(Reflex::Settle, "frontier"), None);
    }
}
