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
pub const TRAIN_EVERY: i64 = reflex::TRAIN_EVERY as i64;

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
    if !json_within_limit(
        &input,
        program
            .manifest
            .budgets
            .max_context_bytes
            .min(MAX_CONTEXT_BYTES),
    ) {
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
        if !json_within_limit(
            &outputs,
            manifest.budgets.max_output_bytes.min(MAX_OUTPUT_BYTES),
        ) {
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
    /// Trials scored in this pass, decided or still collecting labels.
    pub heads: BTreeMap<String, Comparison>,
    /// Heads for which this pass fitted a new challenger.
    pub started: Vec<String>,
    /// Whether each acting head may act under `auto`. Left empty by
    /// [`ReflexStore::train`]; callers that certify synchronously (see
    /// [`ReflexStore::certify`]) fill it in.
    pub certificates: BTreeMap<String, reflex::Certificate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeadStatus {
    pub labeled: u32,
    pub positives: u32,
    /// The active head on labels that arrived after it became active, which
    /// it was never fitted on.
    pub live: Option<reflex::Metrics>,
    /// The open challenger's trial so far, if one is running.
    pub trial: Option<Comparison>,
    /// Whether the head may act under `auto`, as of the last training pass.
    pub certificate: Option<reflex::Certificate>,
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

/// A label as the ledger keeps it: the example plus its label sequence, which
/// orders labels by arrival so a trial can tell which ones a challenger was
/// never fitted on.
#[derive(Debug, Clone)]
struct Labeled {
    example: Example,
    seq: i64,
    at_ms: i64,
    source: String,
}

pub struct ReflexStore {
    root: PathBuf,
    db: Mutex<Connection>,
}

const SCHEMA_VERSION: u32 = 2;

impl ReflexStore {
    pub fn open(root: &Path) -> Result<Self> {
        let mut connection = Connection::open(root.join("reflex.sqlite"))?;
        connection.busy_timeout(std::time::Duration::from_secs(15))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::Unavailable(
                "reflex state was written by a newer xcb",
            ));
        }
        // Version 1 kept one label per observation. Version 2 keeps one per
        // head, so a single operator reply can label several heads. The v1
        // columns stay in place, unused, and their labels are copied forward.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS generations(reflex TEXT NOT NULL, version INTEGER NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(reflex,version));
             CREATE TABLE IF NOT EXISTS observations(id TEXT PRIMARY KEY, reflex TEXT NOT NULL, subject TEXT NOT NULL, head TEXT NOT NULL, value TEXT NOT NULL, gate TEXT NOT NULL, score INTEGER NOT NULL, features TEXT NOT NULL, params_version INTEGER NOT NULL, program TEXT NOT NULL, receipt TEXT NOT NULL, at_ms INTEGER NOT NULL, label INTEGER, label_weight REAL, label_source TEXT, labeled_at_ms INTEGER);
             CREATE INDEX IF NOT EXISTS observations_subject ON observations(reflex,subject,at_ms);
             CREATE TABLE IF NOT EXISTS labels(observation TEXT NOT NULL, reflex TEXT NOT NULL, head TEXT NOT NULL, label INTEGER NOT NULL, weight REAL NOT NULL, source TEXT NOT NULL, at_ms INTEGER NOT NULL, seq INTEGER NOT NULL, PRIMARY KEY(observation,head));
             CREATE INDEX IF NOT EXISTS labels_head ON labels(reflex,head,seq);
             CREATE TABLE IF NOT EXISTS trials(reflex TEXT NOT NULL, head TEXT NOT NULL, candidate TEXT NOT NULL, started_at_ms INTEGER NOT NULL, fitted_through INTEGER NOT NULL, PRIMARY KEY(reflex,head));
             CREATE TABLE IF NOT EXISTS certificates(reflex TEXT NOT NULL, head TEXT NOT NULL, payload TEXT NOT NULL, at_ms INTEGER NOT NULL, through INTEGER NOT NULL, PRIMARY KEY(reflex,head));",
        )?;
        let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current < SCHEMA_VERSION {
            tx.execute(
                "INSERT OR IGNORE INTO labels(observation,reflex,head,label,weight,source,at_ms,seq) SELECT id,reflex,head,label,label_weight,COALESCE(label_source,'v1'),COALESCE(labeled_at_ms,at_ms),rowid FROM observations WHERE label IS NOT NULL AND label_weight IS NOT NULL",
                [],
            )?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        tx.commit()?;
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
        Ok(self.active_since(reflex)?.0)
    }

    /// The active generation and when it became active (0 for the prior).
    fn active_since(&self, reflex: Reflex) -> Result<(Params, i64)> {
        let row: Option<(String, i64)> = self
            .db()?
            .query_row(
                "SELECT payload,created_at FROM generations WHERE reflex=?1 AND active=1 ORDER BY version DESC LIMIT 1",
                [reflex.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            Some((payload, created_at)) => {
                let params: Params = serde_json::from_str(&payload)?;
                params.validate()?;
                // A generation whose feature schema no longer matches this
                // build's prior is not trusted; the prior takes over.
                Ok(if schema(&params) == schema(&reflex::prior(reflex)) {
                    (params, created_at)
                } else {
                    (reflex::prior(reflex), 0)
                })
            }
            None => Ok((reflex::prior(reflex), 0)),
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
        let removed = tx.execute(
            "DELETE FROM observations WHERE id IN (SELECT id FROM observations WHERE reflex=?1 ORDER BY EXISTS(SELECT 1 FROM labels WHERE labels.observation=observations.id), at_ms LIMIT max(0,(SELECT count(*) FROM observations WHERE reflex=?1)-?2))",
            params![decision.reflex.as_str(), MAX_OBSERVATIONS],
        )?;
        if removed > 0 {
            tx.execute(
                "DELETE FROM labels WHERE reflex=?1 AND observation NOT IN (SELECT id FROM observations WHERE reflex=?1)",
                [decision.reflex.as_str()],
            )?;
        }
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

    /// Labels one head of the latest observation for `subject`; `None`
    /// labels the head that made the decision. A label replaces an existing
    /// one only with greater confidence, or equal confidence and a different
    /// value, so explicit feedback (weight 1) overrides inferred behavior and
    /// not the reverse, and a repeated label never looks fresh to a trial.
    pub fn label(
        &self,
        reflex: Reflex,
        subject: &str,
        head: Option<&str>,
        label: bool,
        weight: f64,
        source: &str,
    ) -> Result<bool> {
        if !(weight > 0.0 && weight <= 1.0) {
            return Err(xcb_core::Error::Invalid("label weight").into());
        }
        if head.is_some_and(|head| !heads_for(reflex).contains(&head)) {
            return Err(xcb_core::Error::Invalid("reflex head").into());
        }
        let Some((id, _)) = self.latest(reflex, subject)? else {
            return Ok(false);
        };
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let head = match head {
            Some(head) => head.to_owned(),
            None => tx.query_row("SELECT head FROM observations WHERE id=?1", [&id], |row| {
                row.get(0)
            })?,
        };
        let changed = insert_label(&tx, &id, reflex, &head, label, weight, source)?;
        tx.commit()?;
        Ok(changed)
    }

    /// Labels and, when learning is enabled, trains after every
    /// [`TRAIN_EVERY`] new labels and then re-certifies in the background
    /// (see [`Self::certify_in_background`]). Training failure never fails
    /// the caller's operation. Returns how many labels changed.
    pub fn label_and_learn(
        &self,
        reflex: Reflex,
        subject: &str,
        labels: &[(Option<&str>, bool, f64)],
        source: &str,
        learn: bool,
    ) -> Result<usize> {
        let before = self.label_count(reflex)?;
        let mut changed = 0;
        for (head, label, weight) in labels {
            changed += usize::from(self.label(reflex, subject, *head, *label, *weight, source)?);
        }
        let after = self.label_count(reflex)?;
        if learn && changed > 0 && after / TRAIN_EVERY > before / TRAIN_EVERY {
            let _ = self.train(reflex, FitOptions::default());
            self.certify_in_background(reflex);
        }
        Ok(changed)
    }

    /// Certification replays every retained label, which takes seconds on a
    /// full ledger, so the labeling path never waits for it: it runs on its
    /// own thread and connection. One pass per ledger and reflex runs at a
    /// time; a skipped pass is picked up after the next training step, and
    /// a pass that loses the race to newer labels is not stored.
    fn certify_in_background(&self, reflex: Reflex) {
        static RUNNING: Mutex<BTreeSet<(PathBuf, &'static str)>> = Mutex::new(BTreeSet::new());
        struct Running((PathBuf, &'static str));
        impl Drop for Running {
            fn drop(&mut self) {
                if let Ok(mut running) = RUNNING.lock() {
                    running.remove(&self.0);
                }
            }
        }
        let key = (self.root.clone(), reflex.as_str());
        match RUNNING.lock() {
            Ok(mut running) => {
                if !running.insert(key.clone()) {
                    return;
                }
            }
            Err(_) => return,
        }
        let running = Running(key);
        let root = self.root.clone();
        let _ = std::thread::Builder::new()
            .name("xcb-reflex-certify".into())
            .spawn(move || {
                let _running = running;
                if let Ok(store) = ReflexStore::open(&root) {
                    let _ = store.certify(reflex, FitOptions::default());
                }
            });
    }

    fn label_count(&self, reflex: Reflex) -> Result<i64> {
        Ok(self.db()?.query_row(
            "SELECT count(*) FROM labels WHERE reflex=?1",
            [reflex.as_str()],
            |row| row.get(0),
        )?)
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
                    "xcb-reflex-import-v1\0{}\0{}\0{}",
                    reflex.as_str(),
                    row.head,
                    row.id
                ))[..32]
            );
            let added = tx.execute(
                "INSERT OR IGNORE INTO observations(id,reflex,subject,head,value,gate,score,features,params_version,program,receipt,at_ms) VALUES(?1,?2,?3,?4,'imported','import',0,?5,0,?6,'',?7)",
                params![
                    id,
                    reflex.as_str(),
                    format!("import:{}", xcb_core::display_text(&row.id, 160)),
                    row.head,
                    serde_json::to_string(&row.features)?,
                    program.digest,
                    now_ms() as i64,
                ],
            )?;
            if added == 1 {
                insert_label(&tx, &id, reflex, &row.head, row.label, row.weight, "import")?;
                inserted += 1;
            }
        }
        // Imported history arrives after any open challenger was fitted but
        // is not forward evidence, so it must not decide a trial.
        if inserted > 0 {
            tx.execute("DELETE FROM trials WHERE reflex=?1", [reflex.as_str()])?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Labeled examples for one head in arrival order (newest
    /// [`reflex::MAX_EXAMPLES`]). Examples whose stored features do not cover
    /// the head's current feature schema are skipped: evidence recorded
    /// under an older feature set cannot train a head that reads new
    /// features.
    fn labeled(&self, reflex: Reflex, head: &str) -> Result<Vec<Labeled>> {
        let prior = reflex::prior(reflex);
        let required = prior.head(head)?.weights.keys().collect::<Vec<_>>();
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT o.id,o.features,l.label,l.weight,l.seq,l.at_ms,l.source FROM labels l JOIN observations o ON o.id=l.observation WHERE l.reflex=?1 AND l.head=?2 ORDER BY l.seq DESC LIMIT ?3",
        )?;
        let rows = query.query_map(
            params![reflex.as_str(), head, reflex::MAX_EXAMPLES as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let mut labeled = Vec::new();
        for row in rows {
            let (id, features, label, weight, seq, at_ms, source) = row?;
            let Ok(features) = serde_json::from_str::<Features>(&features) else {
                continue;
            };
            if !required.iter().all(|name| features.contains_key(*name)) {
                continue;
            }
            labeled.push(Labeled {
                example: Example {
                    id,
                    features,
                    label,
                    weight,
                },
                seq,
                at_ms,
                source,
            });
        }
        labeled.reverse();
        Ok(labeled)
    }

    #[cfg(test)]
    fn examples(&self, reflex: Reflex, head: &str) -> Result<Vec<Example>> {
        Ok(self
            .labeled(reflex, head)?
            .into_iter()
            .map(|row| row.example)
            .collect())
    }

    /// The open challenger for a head, if its schema still matches.
    fn trial(&self, reflex: Reflex, head: &str) -> Result<Option<(Head, i64)>> {
        let row: Option<(String, i64)> = self
            .db()?
            .query_row(
                "SELECT candidate,fitted_through FROM trials WHERE reflex=?1 AND head=?2",
                params![reflex.as_str(), head],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let prior = reflex::prior(reflex);
        let expected = prior.head(head)?;
        Ok(row.and_then(|(candidate, through)| {
            serde_json::from_str::<Head>(&candidate)
                .ok()
                .filter(|candidate| {
                    candidate.validate().is_ok()
                        && candidate.weights.keys().eq(expected.weights.keys())
                })
                .map(|candidate| (candidate, through))
        }))
    }

    /// One learning step for every head. An open trial is scored on the
    /// labels that arrived after its challenger was fitted; once it has
    /// [`reflex::TRIAL_LABELS`] of them it is decided (see
    /// [`reflex::compare`]). A head with no open trial gets a new challenger:
    /// the posterior of the shipped prior given the newest retained evidence,
    /// so repeated passes converge instead of drifting away from the prior.
    pub fn train(&self, reflex: Reflex, options: FitOptions) -> Result<TrainReport> {
        let current = self.active(reflex)?;
        let prior = reflex::prior(reflex);
        let mut heads = current.heads.clone();
        let mut comparisons = BTreeMap::new();
        let mut decided = Vec::new();
        let mut started = BTreeMap::new();
        let mut labeled = 0u32;
        for (name, head) in &current.heads {
            let rows = self.labeled(reflex, name)?;
            labeled = labeled.max(rows.len() as u32);
            let mut open = self.trial(reflex, name)?;
            if let Some((candidate, through)) = &open {
                let fresh: Vec<Example> = rows
                    .iter()
                    .filter(|row| row.seq > *through)
                    .map(|row| row.example.clone())
                    .collect();
                let comparison = reflex::compare(head, candidate, &fresh);
                if reflex::trial_complete(&comparison) {
                    if comparison.promoted {
                        heads.insert(name.clone(), candidate.clone());
                    }
                    decided.push(name.clone());
                    open = None;
                }
                comparisons.insert(name.clone(), comparison);
            }
            if open.is_none()
                && let Some(last) = rows.last()
            {
                let examples: Vec<Example> = rows.iter().map(|row| row.example.clone()).collect();
                let candidate = reflex::fit(prior.head(name)?, reflex::recent(&examples), options)?;
                if candidate != heads[name] {
                    started.insert(name.clone(), (candidate, last.seq));
                }
            }
        }
        let evidence: BTreeMap<String, Comparison> = comparisons
            .iter()
            .filter(|(_, comparison)| comparison.promoted)
            .map(|(name, comparison)| (name.clone(), comparison.clone()))
            .collect();
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for name in decided.iter().chain(started.keys()) {
            tx.execute(
                "DELETE FROM trials WHERE reflex=?1 AND head=?2",
                params![reflex.as_str(), name],
            )?;
        }
        for (name, (candidate, through)) in &started {
            tx.execute(
                "INSERT INTO trials(reflex,head,candidate,started_at_ms,fitted_through) VALUES(?1,?2,?3,?4,?5)",
                params![
                    reflex.as_str(),
                    name,
                    serde_json::to_string(candidate)?,
                    now_ms() as i64,
                    through
                ],
            )?;
        }
        let promoted_version = if evidence.is_empty() {
            None
        } else {
            Some(append_generation(&tx, &current, heads, labeled, evidence)?)
        };
        tx.commit()?;
        Ok(TrainReport {
            reflex,
            from_version: current.version,
            promoted_version,
            labeled,
            heads: comparisons,
            started: started.into_keys().collect(),
            certificates: BTreeMap::new(),
        })
    }

    /// Recomputes and stores whether each acting head may act under `auto`
    /// (see [`reflex::certify`]).
    pub fn certify(
        &self,
        reflex: Reflex,
        options: FitOptions,
    ) -> Result<BTreeMap<String, reflex::Certificate>> {
        let prior = reflex::prior(reflex);
        let mut certificates = BTreeMap::new();
        let mut through = BTreeMap::new();
        for name in heads_for(reflex) {
            if reflex::precision_floor(name).is_none() {
                continue;
            }
            let rows = self.labeled(reflex, name)?;
            through.insert((*name).to_owned(), rows.last().map_or(0, |row| row.seq));
            let examples: Vec<Example> = rows.iter().map(|row| row.example.clone()).collect();
            let operator: Vec<bool> = rows
                .iter()
                .map(|row| reflex::operator_label(&row.source))
                .collect();
            let previous = self.certificate(reflex, name)?;
            let certificate = reflex::certify(
                name,
                prior.head(name)?,
                &examples,
                &operator,
                previous.as_ref(),
                options,
            )?;
            certificates.insert((*name).to_owned(), certificate);
        }
        self.store_certificates(reflex, &certificates, &through)?;
        Ok(certificates)
    }

    /// Stores certificates computed through each head's newest label
    /// sequence. A concurrent pass that saw fewer labels never replaces a
    /// newer certificate.
    fn store_certificates(
        &self,
        reflex: Reflex,
        certificates: &BTreeMap<String, reflex::Certificate>,
        through: &BTreeMap<String, i64>,
    ) -> Result<()> {
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (name, certificate) in certificates {
            tx.execute(
                "INSERT INTO certificates(reflex,head,payload,at_ms,through) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(reflex,head) DO UPDATE SET payload=excluded.payload,at_ms=excluded.at_ms,through=excluded.through WHERE excluded.through>=certificates.through",
                params![
                    reflex.as_str(),
                    name,
                    serde_json::to_string(certificate)?,
                    now_ms() as i64,
                    through.get(name).copied().unwrap_or(0)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn put_certificate(
        &self,
        reflex: Reflex,
        head: &str,
        certificate: reflex::Certificate,
    ) -> Result<()> {
        self.store_certificates(
            reflex,
            &BTreeMap::from([(head.to_owned(), certificate)]),
            &BTreeMap::from([(head.to_owned(), i64::MAX)]),
        )
    }

    /// The stored certificate for a head. An unreadable one counts as none,
    /// so `auto` falls back to observing.
    pub fn certificate(&self, reflex: Reflex, head: &str) -> Result<Option<reflex::Certificate>> {
        let payload: Option<String> = self
            .db()?
            .query_row(
                "SELECT payload FROM certificates WHERE reflex=?1 AND head=?2",
                params![reflex.as_str(), head],
                |row| row.get(0),
            )
            .optional()?;
        Ok(payload.and_then(|payload| serde_json::from_str(&payload).ok()))
    }

    /// Whether `head` may act on `features` under `auto`: it holds a
    /// certificate and the certified head scores the turn at or above the
    /// certified threshold. The probability is computed here rather than
    /// read from the program's output, which a custom program controls.
    pub fn certified(&self, reflex: Reflex, head: &str, features: &Features) -> Result<bool> {
        Ok(self.certificate(reflex, head)?.is_some_and(|certificate| {
            certificate.certified && certificate.head.probability(features) >= certificate.threshold
        }))
    }

    /// Appends a generation whose heads were earned by a replay of labeled
    /// history (see [`reflex::replay`]) and makes it active. Open trials for
    /// those heads are discarded; the next pass fits fresh challengers.
    pub fn adopt(
        &self,
        reflex: Reflex,
        replayed: BTreeMap<String, (Head, Comparison)>,
        trained_on: u32,
    ) -> Result<Option<u32>> {
        if replayed.is_empty() {
            return Ok(None);
        }
        let current = self.active(reflex)?;
        let mut heads = current.heads.clone();
        let mut evidence = BTreeMap::new();
        for (name, (head, comparison)) in replayed {
            if !heads.contains_key(&name) {
                return Err(xcb_core::Error::Invalid("reflex head").into());
            }
            heads.insert(name.clone(), head);
            evidence.insert(name, comparison);
        }
        let mut db = self.db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for name in evidence.keys() {
            tx.execute(
                "DELETE FROM trials WHERE reflex=?1 AND head=?2",
                params![reflex.as_str(), name],
            )?;
        }
        let version = append_generation(&tx, &current, heads, trained_on, evidence)?;
        tx.commit()?;
        Ok(Some(version))
    }

    /// Reactivates a recorded generation; version 0 restores the prior. Open
    /// trials are discarded because they were fitted against another head.
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
        tx.execute("DELETE FROM trials WHERE reflex=?1", [reflex.as_str()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn status(&self, reflex: Reflex, mode: ReflexMode, learn: bool) -> Result<Status> {
        let (params, since) = self.active_since(reflex)?;
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
                    "SELECT count(DISTINCT observation) FROM labels WHERE reflex=?1",
                    [reflex.as_str()],
                    |row| row.get(0),
                )?,
            )
        };
        let mut heads = BTreeMap::new();
        for (name, head) in &params.heads {
            let rows = self.labeled(reflex, name)?;
            let live: Vec<Example> = rows
                .iter()
                .filter(|row| row.at_ms > since)
                .map(|row| row.example.clone())
                .collect();
            let trial = self.trial(reflex, name)?.map(|(candidate, through)| {
                let fresh: Vec<Example> = rows
                    .iter()
                    .filter(|row| row.seq > through)
                    .map(|row| row.example.clone())
                    .collect();
                reflex::compare(head, &candidate, &fresh)
            });
            heads.insert(
                name.clone(),
                HeadStatus {
                    labeled: rows.len() as u32,
                    positives: rows.iter().filter(|row| row.example.label).count() as u32,
                    live: (!live.is_empty()).then(|| reflex::evaluate(head, &live)),
                    trial,
                    certificate: self.certificate(reflex, name)?,
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

fn schema(params: &Params) -> BTreeMap<String, BTreeSet<String>> {
    params
        .heads
        .iter()
        .map(|(name, head)| (name.clone(), head.weights.keys().cloned().collect()))
        .collect()
}

fn insert_label(
    tx: &rusqlite::Transaction<'_>,
    observation: &str,
    reflex: Reflex,
    head: &str,
    label: bool,
    weight: f64,
    source: &str,
) -> Result<bool> {
    let changed = tx.execute(
        "INSERT INTO labels(observation,reflex,head,label,weight,source,at_ms,seq) VALUES(?1,?2,?3,?4,?5,?6,?7,(SELECT COALESCE(max(seq),0)+1 FROM labels))
         ON CONFLICT(observation,head) DO UPDATE SET label=excluded.label,weight=excluded.weight,source=excluded.source,at_ms=excluded.at_ms,seq=excluded.seq
         WHERE labels.weight<excluded.weight OR (labels.weight=excluded.weight AND labels.label<>excluded.label)",
        params![
            observation,
            reflex.as_str(),
            head,
            label,
            weight,
            xcb_core::display_text(source, 48),
            now_ms() as i64,
        ],
    )?;
    Ok(changed == 1)
}

fn append_generation(
    tx: &rusqlite::Transaction<'_>,
    current: &Params,
    heads: BTreeMap<String, Head>,
    trained_on: u32,
    evidence: BTreeMap<String, Comparison>,
) -> Result<u32> {
    let reflex = current.reflex;
    // Another writer (the supervisor's automatic pass and a CLI `train`, say)
    // may have promoted since `current` was read; appending on a stale parent
    // would decide the same trial twice.
    // Resolved as `active_since` does: a stored generation whose feature
    // schema no longer matches this build counts as the prior (version 0).
    let active: Option<(u32, String)> = tx
        .query_row(
            "SELECT version,payload FROM generations WHERE reflex=?1 AND active=1 ORDER BY version DESC LIMIT 1",
            [reflex.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let effective = active.map_or(0, |(version, payload)| {
        serde_json::from_str::<Params>(&payload)
            .ok()
            .filter(|params| schema(params) == schema(&reflex::prior(reflex)))
            .map_or(0, |_| version)
    });
    if effective != current.version {
        return Err(Error::Conflict("reflex generation changed; train again"));
    }
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
    let version = u32::try_from(latest + 1).map_err(|_| Error::Unavailable("reflex version"))?;
    let next = Params {
        reflex,
        version,
        parent: Some(current.version),
        heads,
        trained_on,
        evidence,
    };
    next.validate()?;
    tx.execute(
        "UPDATE generations SET active=0 WHERE reflex=?1",
        [reflex.as_str()],
    )?;
    tx.execute(
        "INSERT INTO generations(reflex,version,active,created_at,payload) VALUES(?1,?2,1,?3,?4)",
        params![
            reflex.as_str(),
            version,
            now_ms() as i64,
            serde_json::to_string(&next)?
        ],
    )?;
    Ok(version)
}

/// Replays imported history per head, in file order, from the active
/// generation (see [`reflex::replay`]). Pure: nothing is written.
pub fn replay_import(
    active: &Params,
    rows: &[Imported],
) -> Result<BTreeMap<String, reflex::Replay>> {
    let prior = reflex::prior(active.reflex);
    let mut replays = BTreeMap::new();
    for name in heads_for(active.reflex) {
        let examples: Vec<Example> = rows
            .iter()
            .filter(|row| row.head == *name)
            .map(|row| Example {
                id: row.id.clone(),
                features: row.features.clone(),
                label: row.label,
                weight: row.weight,
            })
            .collect();
        if examples.is_empty() {
            continue;
        }
        replays.insert(
            (*name).to_owned(),
            reflex::replay(
                prior.head(name)?,
                active.head(name)?,
                &examples,
                FitOptions::default(),
            )?,
        );
    }
    Ok(replays)
}

/// Whether imported history alone would certify each acting head (see
/// [`reflex::certify`]); every imported label is the operator's. Pure.
pub fn certify_import(
    reflex: Reflex,
    rows: &[Imported],
) -> Result<BTreeMap<String, reflex::Certificate>> {
    let prior = reflex::prior(reflex);
    let mut certificates = BTreeMap::new();
    for name in heads_for(reflex) {
        if reflex::precision_floor(name).is_none() {
            continue;
        }
        let examples: Vec<Example> = rows
            .iter()
            .filter(|row| row.head == *name)
            .map(|row| Example {
                id: row.id.clone(),
                features: row.features.clone(),
                label: row.label,
                weight: row.weight,
            })
            .collect();
        if examples.is_empty() {
            continue;
        }
        let operator = vec![true; examples.len()];
        certificates.insert(
            (*name).to_owned(),
            reflex::certify(
                name,
                prior.head(name)?,
                &examples,
                &operator,
                None,
                FitOptions::default(),
            )?,
        );
    }
    Ok(certificates)
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
/// (criterion-index scale) for the judged route head, or, for settle, the
/// head the label is for (`unfinished` by default) and the turn's tool-call
/// count. Only derived features are stored; the text never enters the ledger.
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
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<u32>,
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
        Reflex::Route if line.head.is_some() || line.tool_calls.is_some() => {
            return Err(xcb_core::Error::Invalid("imported example").into());
        }
        Reflex::Settle if line.judge.is_some() => {
            return Err(xcb_core::Error::Invalid("imported example").into());
        }
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
            let head = line.head.as_deref().unwrap_or(reflex::SETTLE_UNFINISHED);
            let head = heads_for(reflex)
                .iter()
                .find(|known| **known == head)
                .ok_or(xcb_core::Error::Invalid("imported example"))?;
            (
                *head,
                reflex::settle_features(&line.text, &facts, line.tool_calls.unwrap_or(0)),
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

/// Parses a user label word into `(head, label)` pairs, where `None` is the
/// head that made the decision: `frontier`/`standard` for route;
/// `unfinished`, `confirm` or `done` (neither) for settle.
pub fn parse_label(reflex: Reflex, word: &str) -> Option<&'static [(Option<&'static str>, bool)]> {
    use reflex::{SETTLE_CONFIRM, SETTLE_UNFINISHED};
    match (reflex, word) {
        (Reflex::Route, "frontier") => Some(&[(None, true)]),
        (Reflex::Route, "standard") => Some(&[(None, false)]),
        (Reflex::Settle, "unfinished" | "stopped_short") => Some(&[
            (Some(SETTLE_UNFINISHED), true),
            (Some(SETTLE_CONFIRM), false),
        ]),
        (Reflex::Settle, "confirm") => Some(&[
            (Some(SETTLE_CONFIRM), true),
            (Some(SETTLE_UNFINISHED), false),
        ]),
        (Reflex::Settle, "done") => Some(&[
            (Some(SETTLE_UNFINISHED), false),
            (Some(SETTLE_CONFIRM), false),
        ]),
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
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
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
        for (key, limit) in [
            ("maxSteps", 1),
            ("maxContextBytes", 1),
            ("maxOutputBytes", 1),
        ] {
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
        assert_eq!(decision.score_milli, 488);
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
                let features = settle_features(text, &facts, 60);
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
                "The fix is ready on the branch. Should I open the PR and merge it?",
                State::Idle,
                settled()
            )
            .await,
            "confirm"
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
        for i in 0..224 {
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
                .label_and_learn(
                    Reflex::Route,
                    &subject,
                    &[(None, imperative, 1.0)],
                    "explicit",
                    false,
                )
                .unwrap();
            if i == 159 {
                let status = store
                    .status(Reflex::Route, ReflexMode::Active, true)
                    .unwrap();
                assert_eq!((status.observations, status.labeled), (160, 160));
                // The first pass only fits a challenger; nothing it was
                // fitted on can promote it.
                let report = store.train(Reflex::Route, FitOptions::default()).unwrap();
                assert_eq!(report.promoted_version, None, "{report:?}");
                assert_eq!(report.started, vec![ROUTE_PLAIN.to_owned()]);
                let status = store
                    .status(Reflex::Route, ReflexMode::Active, true)
                    .unwrap();
                assert!(status.heads[ROUTE_PLAIN].trial.is_some());
            }
        }
        // The labels after it are the challenger's forward trial, and it wins.
        let report = store.train(Reflex::Route, FitOptions::default()).unwrap();
        assert_eq!(report.promoted_version, Some(1), "{report:?}");
        assert!(report.heads[ROUTE_PLAIN].promoted);
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
        // A pass without fresh labels cannot promote again.
        assert_eq!(
            store
                .train(Reflex::Route, FitOptions::default())
                .unwrap()
                .promoted_version,
            None
        );
        let status = store
            .status(Reflex::Route, ReflexMode::Active, true)
            .unwrap();
        assert!(status.heads[ROUTE_PLAIN].live.is_none());
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
        let features = settle_features("Next, I'll add tests:", &settled(), 3);
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
        let label = |head: Option<&str>, label: bool, weight: f64, subject: &str| {
            store
                .label(Reflex::Settle, subject, head, label, weight, "test")
                .unwrap()
        };
        let unfinished = Some(xcb_core::reflex::SETTLE_UNFINISHED);
        let confirm = Some(xcb_core::reflex::SETTLE_CONFIRM);
        assert!(label(unfinished, false, 0.5, "t_1"));
        assert!(label(unfinished, true, 1.0, "t_1"));
        assert!(!label(unfinished, false, 0.5, "t_1"));
        assert!(!label(unfinished, true, 1.0, "t_1"));
        assert!(!label(unfinished, true, 1.0, "t_9"));
        // One reply labels each head separately.
        assert!(label(confirm, false, 0.5, "t_1"));
        assert!(
            store
                .label(Reflex::Settle, "t_1", Some("handoff"), true, 1.0, "x")
                .is_err()
        );
        let examples = store.examples(Reflex::Settle, "unfinished").unwrap();
        assert_eq!(examples.len(), 1);
        assert!(examples[0].label);
        let examples = store.examples(Reflex::Settle, "confirm").unwrap();
        assert_eq!(examples.len(), 1);
        assert!(!examples[0].label);
        // Crossing a multiple of TRAIN_EVERY labels starts learning.
        for i in 0..TRAIN_EVERY {
            let subject = format!("u_{i}");
            store.observe(&format!("{subject}#1"), &decision).unwrap();
            store
                .label_and_learn(
                    Reflex::Settle,
                    &subject,
                    &[(unfinished, i % 2 == 0, 1.0)],
                    "test",
                    true,
                )
                .unwrap();
        }
        let status = store
            .status(Reflex::Settle, ReflexMode::Active, true)
            .unwrap();
        assert!(status.heads["unfinished"].trial.is_some());
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
        assert_eq!(
            parse_label(Reflex::Route, "frontier"),
            Some(&[(None, true)][..])
        );
        assert_eq!(parse_label(Reflex::Settle, "frontier"), None);
        assert_eq!(
            parse_label(Reflex::Settle, "confirm"),
            Some(&[(Some("confirm"), true), (Some("unfinished"), false)][..])
        );
        // Settle examples may name their head and carry the turn's tool calls;
        // route examples may not.
        let settle = parse_import(
            Reflex::Settle,
            "{\"id\":\"q\",\"text\":\"Want me to push it?\",\"label\":true,\"head\":\"confirm\",\"tool_calls\":40}",
        )
        .unwrap();
        assert_eq!(settle[0].head, "confirm");
        assert!(settle[0].features["tools"] > 0.7);
        assert!(
            parse_import(
                Reflex::Settle,
                "{\"id\":\"q\",\"text\":\"x\",\"label\":true,\"head\":\"handoff\"}"
            )
            .is_err()
        );
        assert!(
            parse_import(
                Reflex::Route,
                "{\"id\":\"q\",\"text\":\"x\",\"label\":true,\"head\":\"plain\"}"
            )
            .is_err()
        );
        assert!(
            parse_import(
                Reflex::Route,
                "{\"id\":\"q\",\"text\":\"x\",\"label\":true,\"tool_calls\":1}"
            )
            .is_err()
        );
    }

    #[test]
    fn a_generation_from_an_older_schema_does_not_block_promotion() {
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        // An upgrade changed the feature schema: the stored active generation
        // no longer matches, so the prior (version 0) is in effect.
        let mut stale = reflex::prior(Reflex::Route);
        stale.version = 1;
        stale.parent = Some(0);
        stale
            .heads
            .get_mut(ROUTE_PLAIN)
            .unwrap()
            .weights
            .insert("retired_feature".into(), 1.0);
        store
            .db()
            .unwrap()
            .execute(
                "INSERT INTO generations(reflex,version,active,created_at,payload) VALUES('route',1,1,0,?1)",
                [serde_json::to_string(&stale).unwrap()],
            )
            .unwrap();
        assert_eq!(store.active(Reflex::Route).unwrap().version, 0);
        let head = reflex::prior(Reflex::Route).heads[ROUTE_PLAIN].clone();
        let comparison = reflex::compare(&head, &head, &[]);
        let adopted = store
            .adopt(
                Reflex::Route,
                BTreeMap::from([(ROUTE_PLAIN.to_owned(), (head, comparison))]),
                1,
            )
            .unwrap();
        assert_eq!(adopted, Some(2));
        assert_eq!(store.active(Reflex::Route).unwrap().version, 2);
    }

    #[test]
    fn version_one_labels_carry_forward() {
        let dir = temp();
        {
            let connection = Connection::open(dir.path().join("reflex.sqlite")).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE generations(reflex TEXT NOT NULL, version INTEGER NOT NULL, active INTEGER NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(reflex,version));
                     CREATE TABLE observations(id TEXT PRIMARY KEY, reflex TEXT NOT NULL, subject TEXT NOT NULL, head TEXT NOT NULL, value TEXT NOT NULL, gate TEXT NOT NULL, score INTEGER NOT NULL, features TEXT NOT NULL, params_version INTEGER NOT NULL, program TEXT NOT NULL, receipt TEXT NOT NULL, at_ms INTEGER NOT NULL, label INTEGER, label_weight REAL, label_source TEXT, labeled_at_ms INTEGER);
                     INSERT INTO observations VALUES('a','route','t_1','plain','frontier','','1','{}',0,'p','r',5,1,1.0,'explicit',6);
                     INSERT INTO observations VALUES('b','route','t_2','plain','standard','','1','{}',0,'p','r',7,NULL,NULL,NULL,NULL);
                     PRAGMA user_version=1;",
                )
                .unwrap();
        }
        let store = ReflexStore::open(dir.path()).unwrap();
        let db = store.db().unwrap();
        let rows: Vec<(String, String, bool, String)> = db
            .prepare("SELECT observation,head,label,source FROM labels")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![("a".into(), "plain".into(), true, "explicit".into())]
        );
        let version: u32 = db
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(db);
        drop(store);
        // Reopening does not copy again.
        let store = ReflexStore::open(dir.path()).unwrap();
        let count: i64 = store
            .db()
            .unwrap()
            .query_row("SELECT count(*) FROM labels", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn replayed_imports_are_adopted_only_when_they_won() {
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        // Imperative prompts go to frontier; the prior's plain head misses it.
        let source: String = (0..400)
            .map(|i| {
                let text = if i % 2 == 0 {
                    format!("build feature {i}")
                } else {
                    format!("what is {i}")
                };
                format!(
                    "{{\"id\":\"r{i}\",\"text\":\"{text}\",\"label\":{}}}\n",
                    i % 2 == 0
                )
            })
            .collect();
        let rows = parse_import(Reflex::Route, &source).unwrap();
        let active = store.active(Reflex::Route).unwrap();
        let replays = replay_import(&active, &rows).unwrap();
        let plain = &replays[ROUTE_PLAIN];
        assert!(plain.promotions > 0, "{plain:?}");
        assert!(!replays.contains_key(ROUTE_JUDGED));
        // Deterministic.
        assert_eq!(replay_import(&active, &rows).unwrap()[ROUTE_PLAIN], *plain);
        let won = replays
            .into_iter()
            .filter(|(_, replay)| replay.promotions > 0)
            .filter_map(|(head, replay)| Some((head, (replay.head, replay.evidence?))))
            .collect();
        assert_eq!(store.adopt(Reflex::Route, won, 400).unwrap(), Some(1));
        let adopted = store.active(Reflex::Route).unwrap();
        assert!(adopted.heads[ROUTE_PLAIN].decide(&route_features("build it", false, false)));
        assert_eq!(adopted.heads[ROUTE_JUDGED], active.heads[ROUTE_JUDGED]);
        assert_eq!(
            store.adopt(Reflex::Route, BTreeMap::new(), 0).unwrap(),
            None
        );
    }

    #[test]
    fn operator_history_certifies_a_settle_head_in_the_ledger() {
        use reflex::{SETTLE_CONFIRM, SETTLE_UNFINISHED};
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        let facts = TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::None,
            pending_attention: false,
            failure: None,
        };
        let features = settle_features("Parser updated. Next, I'll wire the CLI:", &facts, 60);
        assert!(
            !store
                .certified(Reflex::Settle, SETTLE_UNFINISHED, &features)
                .unwrap()
        );
        let rows: Vec<Imported> = (0..400)
            .map(|i| Imported {
                id: format!("s{i}"),
                head: SETTLE_UNFINISHED.into(),
                features: features.clone(),
                label: i % 10 != 0,
                weight: 1.0,
            })
            .collect();
        let dry = certify_import(Reflex::Settle, &rows).unwrap();
        assert!(dry[SETTLE_UNFINISHED].certified);
        assert!(!dry.contains_key(SETTLE_CONFIRM));
        assert_eq!(store.import(Reflex::Settle, &rows).unwrap(), 400);
        let certificates = store
            .certify(Reflex::Settle, FitOptions::default())
            .unwrap();
        assert!(certificates[SETTLE_UNFINISHED].certified);
        assert!(!certificates[SETTLE_CONFIRM].certified);
        // Acting scores with the certified head, so rolling the active
        // generation back does not change what was measured.
        store.rollback(Reflex::Settle, 0).unwrap();
        assert!(
            store
                .certified(Reflex::Settle, SETTLE_UNFINISHED, &features)
                .unwrap()
        );
        let status = store
            .status(Reflex::Settle, ReflexMode::Auto, true)
            .unwrap();
        assert!(
            status.heads[SETTLE_UNFINISHED]
                .certificate
                .as_ref()
                .is_some_and(|certificate| certificate.certified)
        );
        // An unreadable certificate counts as none.
        store
            .db()
            .unwrap()
            .execute("UPDATE certificates SET payload='{'", [])
            .unwrap();
        assert!(
            !store
                .certified(Reflex::Settle, SETTLE_UNFINISHED, &features)
                .unwrap()
        );
    }

    #[tokio::test]
    async fn labeling_certifies_in_the_background_and_stale_passes_do_not_win() {
        use reflex::SETTLE_UNFINISHED;
        let dir = temp();
        let store = ReflexStore::open(dir.path()).unwrap();
        let facts = TurnFacts {
            terminal: Terminal::Completed,
            joined: true,
            effects: EffectState::None,
            pending_attention: false,
            failure: None,
        };
        let features = settle_features("Parser updated. Next, I'll wire the CLI:", &facts, 60);
        let rows: Vec<Imported> = (0..400)
            .map(|i| Imported {
                id: format!("s{i}"),
                head: SETTLE_UNFINISHED.into(),
                features: features.clone(),
                label: i % 10 != 0,
                weight: 1.0,
            })
            .collect();
        store.import(Reflex::Settle, &rows).unwrap();
        assert!(
            store
                .certificate(Reflex::Settle, SETTLE_UNFINISHED)
                .unwrap()
                .is_none()
        );
        // Crossing a training step returns without waiting for certification.
        for i in 0..TRAIN_EVERY {
            let decision = store
                .decide(
                    Reflex::Settle,
                    &features,
                    settle_evidence(State::Idle, &features),
                    false,
                )
                .await
                .unwrap();
            let subject = format!("live{i}");
            store.observe(&subject, &decision).unwrap();
            store
                .label_and_learn(
                    Reflex::Settle,
                    &subject,
                    &[(Some(SETTLE_UNFINISHED), true, 1.0)],
                    "explicit",
                    true,
                )
                .unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        let certificate = loop {
            if let Some(certificate) = store
                .certificate(Reflex::Settle, SETTLE_UNFINISHED)
                .unwrap()
            {
                break certificate;
            }
            assert!(Instant::now() < deadline, "certification never landed");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(certificate.certified);
        // A pass computed through fewer labels never replaces a newer one.
        let stale = reflex::Certificate {
            certified: false,
            reason: "stale".into(),
            ..certificate.clone()
        };
        store
            .store_certificates(
                Reflex::Settle,
                &BTreeMap::from([(SETTLE_UNFINISHED.to_owned(), stale)]),
                &BTreeMap::from([(SETTLE_UNFINISHED.to_owned(), 1)]),
            )
            .unwrap();
        assert_eq!(
            store
                .certificate(Reflex::Settle, SETTLE_UNFINISHED)
                .unwrap(),
            Some(certificate)
        );
    }
}
