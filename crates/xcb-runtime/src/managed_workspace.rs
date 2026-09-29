//! One global thread over workspace-keyed projects: the v7 schema step, the
//! single workspace validator, the known-workspace registry, and the
//! thread's intake plumbing. A project is a canonical directory, never a
//! conversation; nothing here infers authority.
use super::*;
use crate::workspace_infer::{
    self, BindingOrigin, Cues, KnownWorkspace, Resolution, WorkspaceBinding,
};
use std::io::Read;
use xcb_core::ui::WorkspaceRow;

pub(super) const SCHEMA_VERSION: u32 = 7;
/// How long an opener waits for a peer's upgrade before reporting the
/// running-supervisor conflict. Tests shorten it.
pub(super) const MIGRATION_GUARD_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(1_500)
} else {
    Duration::from_secs(20)
};
pub(super) const MIGRATION_GUARD_POLL: Duration = Duration::from_millis(100);
/// The thread carries every project's transcript, so it keeps more.
pub(super) const RETENTION_GLOBAL_MESSAGES: i64 = 16_384;
pub(super) const THREAD_SPANS: &str = "the thread spans projects; name a directory";
const MAX_WORKSPACE_BYTES: usize = 4096;
const MAX_REGISTRY_ROWS: i64 = 4096;
const MAX_SNAP_LEVELS: usize = 64;
const MAX_GIT_FILE_BYTES: u64 = 64 * 1024;
const BACKUP_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const BACKUP_PREFIX: &str = "managed.pre-v7.";
const BACKUP_RETENTION_MS: u64 = 14 * 24 * 60 * 60 * 1000;
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const NONTERMINAL: &str = "('queued','running','needs_input','uncertain')";

// ---------------------------------------------------------------------------
// Schema v7

fn table_exists(db: &Connection, name: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )?)
}

/// `pragma_table_xinfo`, unlike `pragma_table_info`, lists generated columns.
fn column_exists(db: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE name=?2)",
        params![table, column],
        |row| row.get(0),
    )?)
}

/// The one v7 step. Shape-detected before every action so it stays
/// idempotent after an older step resets `user_version`; runs in a single
/// immediate transaction and touches no file.
pub(super) fn migrate_v7(db: &mut Connection, now: u64) -> Result<()> {
    let version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: u32 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if !column_exists(&tx, "tasks", "workspace")? {
        tx.execute_batch(
            "ALTER TABLE tasks ADD COLUMN workspace TEXT GENERATED ALWAYS AS (CASE WHEN json_valid(payload) THEN json_extract(payload,'$.workspace') END) VIRTUAL;",
        )?;
    }
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS tasks_workspace_state ON tasks(workspace,state,updated_at,id);
         CREATE TABLE IF NOT EXISTS workspaces(path TEXT PRIMARY KEY,name TEXT NOT NULL,repo TEXT,admitted_by TEXT NOT NULL,first_seen INTEGER NOT NULL,last_used INTEGER NOT NULL,task_count INTEGER NOT NULL DEFAULT 0,hidden INTEGER NOT NULL DEFAULT 0);
         CREATE INDEX IF NOT EXISTS workspaces_recent ON workspaces(hidden,last_used);
         CREATE TABLE IF NOT EXISTS project_migration_conflicts(id TEXT PRIMARY KEY,kind TEXT NOT NULL,workspace TEXT,conversation TEXT NOT NULL,disposition TEXT NOT NULL,stranded_tasks INTEGER NOT NULL DEFAULT 0,payload TEXT NOT NULL,created_at INTEGER NOT NULL,resolved_at INTEGER);",
    )?;
    let mut rekey = false;
    for (table, kind) in [("project_policies", "grant"), ("project_memory", "memory")] {
        let legacy = table_exists(&tx, table)? && column_exists(&tx, table, "conversation")?;
        if !legacy {
            continue;
        }
        let archive = format!("{table}_v6");
        if table_exists(&tx, &archive)? {
            // A v7 store whose project tables an older step dropped and
            // recreated in the legacy shape: keep every row as evidence.
            // Nothing is left to act on, so each is recorded settled.
            let (rows, unreadable) = legacy_rows(&tx, table)?;
            for row in rows {
                insert_conflict(
                    &tx,
                    &PlannedConflict {
                        kind,
                        workspace: None,
                        conversation: row.key,
                        disposition: "dropped",
                        stranded_tasks: 0,
                        payload: row.payload,
                    },
                    now,
                    true,
                )?;
            }
            for conflict in &unreadable {
                insert_conflict(&tx, conflict, now, true)?;
            }
            tx.execute_batch(&format!("DROP TABLE {table};"))?;
        } else {
            tx.execute_batch(&format!("ALTER TABLE {table} RENAME TO {archive};"))?;
            rekey = true;
        }
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS project_policies(workspace TEXT PRIMARY KEY,revision INTEGER NOT NULL,payload TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS project_memory(workspace TEXT PRIMARY KEY,revision INTEGER NOT NULL,payload TEXT NOT NULL);",
    )?;
    if rekey {
        let mut unreadable = Vec::new();
        let mut rows = |table: &str| -> Result<Vec<LegacyRow>> {
            if !table_exists(&tx, table)? {
                return Ok(vec![]);
            }
            let (rows, bad) = legacy_rows(&tx, table)?;
            unreadable.extend(bad);
            Ok(rows)
        };
        let grants = rows("project_policies_v6")?;
        let memory = rows("project_memory_v6")?;
        let input = RekeyInput {
            grants,
            memory,
            conversations: conversation_infos(&tx)?,
            generation_refs: generation_refs(&tx)?,
            now,
        };
        let mut plan = plan_project_rekey(&input);
        plan.conflicts.extend(unreadable);
        for policy in &plan.grants {
            project::write_policy(&tx, policy)?;
        }
        for binding in &plan.memory {
            tx.execute(
                "INSERT INTO project_memory(workspace,revision,payload) VALUES(?1,?2,?3)",
                params![
                    binding.workspace,
                    sql(binding.revision)?,
                    serde_json::to_string(binding)?
                ],
            )?;
        }
        // Only what still needs the owner stays open: a paused winner and
        // the grants it superseded, and unbound Wordcell configs. Moved,
        // merged and dropped rows are audit records.
        let paused: BTreeSet<&str> = plan
            .conflicts
            .iter()
            .filter(|conflict| conflict.disposition == "winner_paused")
            .filter_map(|conflict| conflict.workspace.as_deref())
            .collect();
        for conflict in &plan.conflicts {
            let settled = match conflict.disposition {
                "winner_paused" | "unbound" => false,
                "superseded" => !conflict
                    .workspace
                    .as_deref()
                    .is_some_and(|workspace| paused.contains(workspace)),
                _ => true,
            };
            insert_conflict(&tx, conflict, now, settled)?;
        }
    }
    backfill_registry(&tx)?;
    tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};"))?;
    tx.commit()?;
    Ok(())
}

pub(super) struct LegacyRow {
    pub key: String,
    pub revision: i64,
    pub payload: String,
}

/// The legacy rows, and each row whose columns do not have their declared
/// types (storable where a 0.8.x writer ran without constraints) as a
/// dropped `undecodable` conflict, so one bad row never fails the upgrade.
fn legacy_rows(db: &Connection, table: &str) -> Result<(Vec<LegacyRow>, Vec<PlannedConflict>)> {
    use rusqlite::types::Value;
    let mut query = db.prepare(&format!(
        "SELECT conversation,revision,payload FROM {table} ORDER BY conversation LIMIT 4096"
    ))?;
    let values = query
        .query_map([], |row| {
            Ok((
                row.get::<_, Value>(0)?,
                row.get::<_, Value>(1)?,
                row.get::<_, Value>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut rows = Vec::new();
    let mut unreadable = Vec::new();
    for values in values {
        match values {
            (Value::Text(key), Value::Integer(revision), Value::Text(payload)) => {
                rows.push(LegacyRow {
                    key,
                    revision,
                    payload,
                })
            }
            (key, revision, payload) => unreadable.push(PlannedConflict {
                kind: "undecodable",
                workspace: None,
                conversation: value_text(&key, 512),
                disposition: "dropped",
                stranded_tasks: 0,
                payload: match payload {
                    Value::Text(payload) => payload,
                    payload => format!(
                        "revision={} payload={}",
                        value_text(&revision, 64),
                        value_text(&payload, 4096)
                    ),
                },
            }),
        }
    }
    Ok((rows, unreadable))
}

/// A bounded rendering of one SQLite value for a conflict record.
fn value_text(value: &rusqlite::types::Value, max: usize) -> String {
    use rusqlite::types::Value;
    let text = match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Text(text) => text.clone(),
        Value::Blob(bytes) => format!(
            "x'{}'",
            bytes
                .iter()
                .take(max / 2)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ),
    };
    xcb_core::display_text(&text, max)
}

pub(super) struct ConversationInfo {
    pub workspace: Option<String>,
    pub updated_at: i64,
}

fn conversation_infos(db: &Connection) -> Result<BTreeMap<String, ConversationInfo>> {
    let mut query = db.prepare(
        "SELECT id,updated_at,CASE WHEN json_valid(payload) THEN json_extract(payload,'$.workspace') END FROM conversations",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<rusqlite::types::Value>>(2)?,
        ))
    })?;
    let mut infos = BTreeMap::new();
    for row in rows {
        let (id, updated_at, workspace) = row?;
        let workspace = match workspace {
            Some(rusqlite::types::Value::Text(text)) => Some(text),
            _ => None,
        };
        infos.insert(
            id,
            ConversationInfo {
                workspace,
                updated_at,
            },
        );
    }
    Ok(infos)
}

/// Nonterminal tasks per grant generation they depend on.
fn generation_refs(db: &Connection) -> Result<BTreeMap<String, u64>> {
    let mut refs = BTreeMap::new();
    for path in [
        "$.project_proposal.generation",
        "$.program_generation",
        "$.program_child.generation",
        "$.daemon_child.generation",
    ] {
        let mut query = db.prepare(&format!(
            "SELECT json_extract(payload,?1),count(*) FROM tasks WHERE json_valid(payload) AND state IN {NONTERMINAL} AND json_extract(payload,?1) IS NOT NULL GROUP BY 1"
        ))?;
        let rows = query.query_map([path], |row| {
            Ok((
                row.get::<_, rusqlite::types::Value>(0)?,
                row.get::<_, i64>(1)?,
            ))
        })?;
        for row in rows {
            if let (rusqlite::types::Value::Text(generation), count) = row? {
                *refs.entry(generation).or_insert(0) += u64::try_from(count).unwrap_or(0);
            }
        }
    }
    Ok(refs)
}

/// The 0.8.x grant shape, for migration decoding only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectPolicyV6 {
    conversation: Id,
    generation: Id,
    goal: String,
    enabled: bool,
    max_tasks: u32,
    admitted_tasks: u32,
    expires_at_ms: u64,
    required_provider: Option<Provider>,
    revision: u64,
}

/// The 0.8.x Wordcell binding shape, for migration decoding only.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryBindingV6 {
    conversation: Id,
    config: crate::wordcell::WordcellConfig,
    revision: u64,
}

pub(super) struct RekeyInput {
    pub grants: Vec<LegacyRow>,
    pub memory: Vec<LegacyRow>,
    pub conversations: BTreeMap<String, ConversationInfo>,
    pub generation_refs: BTreeMap<String, u64>,
    pub now: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedConflict {
    pub kind: &'static str,
    pub workspace: Option<String>,
    pub conversation: String,
    pub disposition: &'static str,
    pub stranded_tasks: u64,
    pub payload: String,
}

#[derive(Debug, Default)]
pub(super) struct RekeyPlan {
    pub grants: Vec<ProjectPolicy>,
    pub memory: Vec<MemoryBinding>,
    pub conflicts: Vec<PlannedConflict>,
}

/// The legacy row's readable workspace, or why it cannot move.
fn rekey_workspace<'a>(
    input: &'a RekeyInput,
    row: &LegacyRow,
    payload_conversation: &Id,
    revision: u64,
) -> std::result::Result<&'a str, &'static str> {
    if payload_conversation.as_str() != row.key || sql(revision).ok() != Some(row.revision) {
        return Err("orphan");
    }
    input
        .conversations
        .get(&row.key)
        .and_then(|info| info.workspace.as_deref())
        .filter(|workspace| {
            workspace.len() <= MAX_WORKSPACE_BYTES && Path::new(workspace).is_absolute()
        })
        .ok_or("orphan")
}

/// Re-key 0.8.x per-conversation grants and Wordcell bindings onto their
/// conversations' workspaces. Pure: it never sums budgets, merges goals or
/// providers, or verifies a Wordcell config; every row it cannot carry over
/// unchanged is recorded.
pub(super) fn plan_project_rekey(input: &RekeyInput) -> RekeyPlan {
    let mut plan = RekeyPlan::default();
    // Unrecoverable rows are recorded under their reason, not their table.
    let dropped = |row: &LegacyRow, why: &'static str| PlannedConflict {
        kind: why,
        workspace: None,
        conversation: row.key.clone(),
        disposition: "dropped",
        stranded_tasks: 0,
        payload: row.payload.clone(),
    };
    let mut grants: BTreeMap<String, Vec<(&LegacyRow, ProjectPolicyV6)>> = BTreeMap::new();
    for row in &input.grants {
        let Ok(policy) = decode::<ProjectPolicyV6>(&row.payload) else {
            plan.conflicts.push(dropped(row, "undecodable"));
            continue;
        };
        match rekey_workspace(input, row, &policy.conversation, policy.revision) {
            Ok(workspace) => grants
                .entry(workspace.to_owned())
                .or_default()
                .push((row, policy)),
            Err(why) => plan.conflicts.push(dropped(row, why)),
        }
    }
    for (workspace, mut rows) in grants {
        let active = |policy: &ProjectPolicyV6| {
            policy.enabled
                && policy.expires_at_ms > input.now
                && policy.admitted_tasks < policy.max_tasks
        };
        let updated = |row: &LegacyRow| {
            input
                .conversations
                .get(&row.key)
                .map_or(i64::MIN, |info| info.updated_at)
        };
        // Most recently updated conversation first, then the latest expiry,
        // then the id.
        rows.sort_by(|(left_row, left), (right_row, right)| {
            updated(right_row)
                .cmp(&updated(left_row))
                .then(right.expires_at_ms.cmp(&left.expires_at_ms))
                .then(left_row.key.cmp(&right_row.key))
        });
        let actives = rows.iter().filter(|(_, policy)| active(policy)).count();
        let winner = if actives == 1 {
            rows.iter()
                .position(|(_, policy)| active(policy))
                .expect("one active grant")
        } else {
            0
        };
        for (index, (row, policy)) in rows.into_iter().enumerate() {
            let conflict = |disposition, stranded_tasks| PlannedConflict {
                kind: "grant",
                workspace: Some(workspace.clone()),
                conversation: row.key.clone(),
                disposition,
                stranded_tasks,
                payload: row.payload.clone(),
            };
            if index != winner {
                let stranded = input
                    .generation_refs
                    .get(policy.generation.as_str())
                    .copied()
                    .unwrap_or(0);
                plan.conflicts.push(conflict("superseded", stranded));
                continue;
            }
            let paused = actives >= 2;
            let moved = ProjectPolicy {
                workspace: workspace.clone(),
                generation: policy.generation,
                goal: policy.goal,
                enabled: policy.enabled && !paused,
                max_tasks: policy.max_tasks,
                admitted_tasks: policy.admitted_tasks,
                expires_at_ms: policy.expires_at_ms,
                required_provider: policy.required_provider,
                revision: policy.revision + u64::from(paused),
            };
            if moved.validate().is_err() {
                plan.conflicts.push(PlannedConflict {
                    kind: "undecodable",
                    workspace: Some(workspace.clone()),
                    conversation: row.key.clone(),
                    disposition: "dropped",
                    stranded_tasks: 0,
                    payload: row.payload.clone(),
                });
                continue;
            }
            plan.conflicts
                .push(conflict(if paused { "winner_paused" } else { "moved" }, 0));
            plan.grants.push(moved);
        }
    }
    let mut bindings: BTreeMap<String, Vec<(&LegacyRow, MemoryBindingV6)>> = BTreeMap::new();
    for row in &input.memory {
        let Ok(binding) = decode::<MemoryBindingV6>(&row.payload) else {
            plan.conflicts.push(dropped(row, "undecodable"));
            continue;
        };
        if binding.revision == 0 {
            plan.conflicts.push(dropped(row, "undecodable"));
            continue;
        }
        match rekey_workspace(input, row, &binding.conversation, binding.revision) {
            Ok(workspace) => bindings
                .entry(workspace.to_owned())
                .or_default()
                .push((row, binding)),
            Err(why) => plan.conflicts.push(dropped(row, why)),
        }
    }
    for (workspace, rows) in bindings {
        let configs: BTreeSet<String> = rows
            .iter()
            .map(|(_, binding)| serde_json::to_string(&binding.config).unwrap_or_default())
            .collect();
        let disposition = match (rows.len(), configs.len()) {
            (1, _) => "moved",
            (_, 1) => "merged",
            _ => "unbound",
        };
        for (row, _) in &rows {
            plan.conflicts.push(PlannedConflict {
                kind: "memory",
                workspace: Some(workspace.clone()),
                conversation: row.key.clone(),
                disposition,
                stranded_tasks: 0,
                payload: row.payload.clone(),
            });
        }
        if disposition != "unbound" {
            let revision = rows
                .iter()
                .map(|(_, binding)| binding.revision)
                .max()
                .unwrap_or(1);
            let (_, binding) = rows.into_iter().next().expect("one binding");
            plan.memory.push(MemoryBinding {
                workspace,
                config: binding.config,
                revision,
            });
        }
    }
    plan
}

fn insert_conflict(
    tx: &Transaction<'_>,
    conflict: &PlannedConflict,
    now: u64,
    settled: bool,
) -> Result<()> {
    let id = format!(
        "pmc_{}",
        digest(format!(
            "xcb-migration-conflict-v1\0{}\0{}\0{}\0{}",
            conflict.kind, conflict.disposition, conflict.conversation, conflict.payload
        ))
    );
    tx.execute(
        "INSERT OR IGNORE INTO project_migration_conflicts(id,kind,workspace,conversation,disposition,stranded_tasks,payload,created_at,resolved_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            id,
            conflict.kind,
            conflict.workspace,
            conflict.conversation,
            conflict.disposition,
            sql(conflict.stranded_tasks)?,
            conflict.payload,
            sql(now)?,
            settled.then(|| sql(now)).transpose()?,
        ],
    )?;
    Ok(())
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned()
}

/// DB-only: every absolute workspace string the store already recorded,
/// admitted as history. Validity is checked on use, never here.
fn backfill_registry(tx: &Transaction<'_>) -> Result<()> {
    // Each source yields (path, at, n): a workspace string, a timestamp and
    // whether it counts as a task.
    let mut sources = vec![
        "SELECT json_extract(payload,'$.workspace'),updated_at,0 FROM conversations WHERE json_valid(payload)".to_owned(),
        "SELECT workspace,updated_at,1 FROM tasks".to_owned(),
        "SELECT scope,0,0 FROM route_stats".to_owned(),
        "SELECT scope,created_at,0 FROM preferences WHERE scope<>'global'".to_owned(),
    ];
    if table_exists(tx, "daemon_meta")? {
        sources.push("SELECT workspace,created_at,0 FROM daemon_meta".into());
    }
    for table in ["project_policies_v6", "project_memory_v6"] {
        if table_exists(tx, table)? {
            sources.push(format!(
                "SELECT json_extract(c.payload,'$.workspace'),c.updated_at,0 FROM {table} p JOIN conversations c ON c.id=p.conversation WHERE json_valid(c.payload)"
            ));
        }
    }
    let query = format!(
        "WITH s(path,at,n) AS ({}) SELECT path,COALESCE(min(at),0),COALESCE(max(at),0),sum(n) FROM s WHERE typeof(path)='text' AND substr(path,1,1)='/' AND length(path)<={MAX_WORKSPACE_BYTES} GROUP BY path",
        sources.join(" UNION ALL ")
    );
    let rows: Vec<(String, i64, i64, i64)> = {
        let mut statement = tx.prepare(&query)?;
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?
    };
    for (path, first, last, count) in rows {
        if path.chars().any(char::is_control) {
            continue;
        }
        tx.execute(
            "INSERT INTO workspaces(path,name,repo,admitted_by,first_seen,last_used,task_count,hidden) VALUES(?1,?2,NULL,'history',?3,?4,?5,0) ON CONFLICT(path) DO NOTHING",
            params![path, basename(&path), first, last, count],
        )?;
    }
    Ok(())
}

/// Best-effort pre-upgrade copy: the only downgrade path. Skipped with a
/// notice when the file is large or the volume is short on space. Returns
/// the copy so an open whose upgrade then fails can remove it.
pub(super) fn backup_before_upgrade(db: &Connection, root: &Path, version: u32) -> Option<PathBuf> {
    let skip = |why: &str| {
        record_supervisor_fault(
            root,
            &format!("managed state upgrade ran without a pre-v7 backup: {why}"),
        );
    };
    let bytes = db_bytes(&root.join("managed.sqlite"));
    if bytes > BACKUP_MAX_BYTES {
        skip("the database is larger than 1 GiB");
        return None;
    }
    if let Err(why) = room_for_copy(root, bytes) {
        skip(why);
        return None;
    }
    let target = root.join(format!("{BACKUP_PREFIX}{}.sqlite", now_ms()));
    let result: Result<()> = (|| {
        crate::os::owner_only(OpenOptions::new().write(true).create_new(true)).open(&target)?;
        db.execute(
            "VACUUM INTO ?1",
            [target.to_str().ok_or(Error::PrivateState)?],
        )?;
        // On Windows the copy inherits the managed directory's owner-only DACL.
        #[cfg(unix)]
        fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        Ok(())
    })();
    match result {
        Ok(()) => Some(target),
        Err(error) => {
            let _ = fs::remove_file(&target);
            skip(&format!(
                "the copy failed at v{version}: {}",
                fault_text(&error)
            ));
            None
        }
    }
}

/// A `VACUUM INTO` copy of `bytes` needs twice that free on `dir`'s volume.
fn room_for_copy(dir: &Path, bytes: u64) -> std::result::Result<(), &'static str> {
    #[cfg(unix)]
    let available = {
        let stat = rustix::fs::statvfs(dir).map_err(|_| "free space could not be measured")?;
        stat.f_bavail.saturating_mul(stat.f_frsize)
    };
    #[cfg(windows)]
    let available =
        xcb_platform::available_space(dir).map_err(|_| "free space could not be measured")?;
    if available < bytes.saturating_mul(2) {
        return Err("free space is below twice the database size");
    }
    Ok(())
}

pub(super) fn prune_upgrade_backups(root: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stamp) = name
            .to_str()
            .and_then(|name| name.strip_prefix(BACKUP_PREFIX))
            .and_then(|rest| rest.strip_suffix(".sqlite"))
            .and_then(|stamp| stamp.parse::<u64>().ok())
        else {
            continue;
        };
        if now.saturating_sub(stamp) > BACKUP_RETENTION_MS {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// What `migrate_copy` found on a private copy of the store.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpgradeReport {
    pub from_version: u32,
    pub to_version: u32,
    pub conversations: u64,
    pub tasks: u64,
    pub workspaces: u64,
    pub grants: u64,
    pub memory_bindings: u64,
    pub conflicts: Vec<MigrationConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationConflict {
    pub id: String,
    pub kind: String,
    pub workspace: Option<String>,
    pub conversation: String,
    pub disposition: String,
    pub stranded_tasks: u64,
    pub payload: String,
    pub created_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Validation

/// The single chokepoint for every stored workspace string. `root` is the
/// managed store root (`<state>/managed`). Returns the canonical path.
pub fn validate_workspace_root(root: &Path, path: &Path) -> Result<String> {
    let canonical = xcb_core::canonical(path)?;
    if !canonical.is_dir() {
        return Err(Error::Unavailable("managed workspace is not a directory"));
    }
    let text = canonical.to_str().ok_or(Error::PrivateState)?;
    if text.len() > MAX_WORKSPACE_BYTES || text.chars().any(char::is_control) {
        return Err(xcb_core::Error::Invalid("workspace path").into());
    }
    let refuse = |why: &'static str| -> Result<String> { Err(Error::Conflict(why)) };
    if canonical.parent().is_none() {
        return refuse("workspace is not allowed: filesystem root");
    }
    // Without a home directory the home, hidden and Library checks cannot
    // run, so nothing validates: the chokepoint fails closed.
    let home = home_dir().ok_or(Error::Unavailable(if cfg!(windows) {
        "home directory is unknown; set USERPROFILE to an absolute path"
    } else {
        "home directory is unknown; set HOME to an absolute path"
    }))?;
    if canonical == home || home.starts_with(&canonical) {
        return refuse("workspace is not allowed: home");
    }
    if let Ok(inside) = canonical.strip_prefix(&home)
        && inside.components().next().is_some_and(|first| {
            let first = first.as_os_str().to_string_lossy();
            first.starts_with('.') || first == "Library" || (cfg!(windows) && first == "AppData")
        })
    {
        return refuse("workspace is not allowed: hidden or library directory");
    }
    let mut state_roots = vec![root.to_path_buf()];
    if let Some(state) = root.parent() {
        state_roots.push(state.to_path_buf());
        state_roots.push(state.join("attachments"));
        state_roots.push(state.join("input-recovery"));
    }
    state_roots.extend(private::default_root().ok());
    state_roots.extend(crate::coordination::default_root().ok());
    for state in state_roots {
        let state = xcb_core::canonical(&state).unwrap_or(state);
        if canonical.starts_with(&state) || state.starts_with(&canonical) {
            return refuse("workspace is not allowed: xcb state");
        }
    }
    const SYSTEM_TREES: [&str; 9] = [
        "/System",
        "/usr",
        "/bin",
        "/sbin",
        "/etc",
        "/private/etc",
        "/dev",
        "/proc",
        "/sys",
    ];
    const SYSTEM_EXACT: [&str; 7] = [
        "/Applications",
        "/Library",
        "/Volumes",
        "/tmp",
        "/private/tmp",
        "/var",
        "/private/var",
    ];
    if SYSTEM_TREES.iter().any(|tree| canonical.starts_with(tree))
        || SYSTEM_EXACT
            .iter()
            .any(|exact| canonical == Path::new(exact))
    {
        return refuse("workspace is not allowed: system directory");
    }
    #[cfg(windows)]
    for variable in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(tree) = std::env::var_os(variable).map(PathBuf::from)
            && let Ok(tree) = xcb_core::canonical(&tree)
            && canonical.starts_with(&tree)
        {
            return refuse("workspace is not allowed: system directory");
        }
    }
    Ok(text.to_owned())
}

fn home_dir() -> Option<PathBuf> {
    let home = xcb_core::home_dir()?;
    if !home.is_absolute() {
        return None;
    }
    Some(xcb_core::canonical(&home).unwrap_or(home))
}

/// A `.git` directory, or a `gitdir:` file for a linked worktree.
fn is_git_toplevel(dir: &Path) -> bool {
    let git = dir.join(".git");
    match fs::symlink_metadata(&git) {
        Ok(meta) if meta.is_dir() => true,
        Ok(meta) if meta.is_file() => {
            bounded_read(&git, 1024).is_some_and(|text| text.trim_start().starts_with("gitdir:"))
        }
        _ => false,
    }
}

fn bounded_read(path: &Path, max: u64) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > max {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// `owner/name` of the repository's `origin` remote, read from `.git/config`
/// or a linked worktree's common directory. Bounded; no subprocess. Only
/// used to group and rank candidates.
pub(super) fn repo_identity(path: &Path) -> Option<String> {
    let git = path.join(".git");
    let common = if git.is_dir() {
        git
    } else {
        let pointer = bounded_read(&git, 1024)?;
        let gitdir = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            path.join(gitdir)
        };
        match bounded_read(&gitdir.join("commondir"), 4096) {
            Some(common) => {
                let common = PathBuf::from(common.trim());
                if common.is_absolute() {
                    common
                } else {
                    gitdir.join(common)
                }
            }
            None => gitdir,
        }
    };
    let config = bounded_read(&common.join("config"), MAX_GIT_FILE_BYTES)?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
            continue;
        }
        if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return repo_from_url(value.trim());
        }
    }
    None
}

fn repo_from_url(url: &str) -> Option<String> {
    let url = url.trim_end_matches('/').trim_end_matches(".git");
    let tail = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.rsplit_once(':').map_or(url, |(_, path)| path),
    };
    let mut parts = tail.rsplit('/');
    let name = parts.next().filter(|part| !part.is_empty())?;
    let owner = parts.next().filter(|part| !part.is_empty())?;
    let repo = format!("{owner}/{name}");
    (repo.len() <= 256 && !repo.chars().any(char::is_control)).then_some(repo)
}

/// A prompt path token as a path: `~` and `~/…` expand to `$HOME`, which
/// the validator then refuses or confines like any other path. Never
/// relative to the process's working directory.
fn home_path(token: &str) -> Result<PathBuf> {
    let home = || home_dir().ok_or(Error::Unavailable("home directory is unknown"));
    let path = if token == "~" {
        home()?
    } else if let Some(rest) = token.strip_prefix("~/") {
        home()?.join(rest)
    } else {
        PathBuf::from(token)
    };
    if !path.is_absolute() {
        return Err(xcb_core::Error::Invalid("workspace path").into());
    }
    Ok(path)
}

/// A `path:line[:col]` token names a file position; drop the position.
fn strip_position(path: &str) -> &str {
    let mut path = path;
    for _ in 0..2 {
        match path.rsplit_once(':') {
            Some((head, tail))
                if !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                path = head;
            }
            _ => break,
        }
    }
    path
}

// ---------------------------------------------------------------------------
// Registry

#[derive(Debug, Clone)]
struct RegistryRow {
    path: String,
    name: String,
    repo: Option<String>,
    admitted_by: String,
    first_seen: u64,
    last_used: u64,
    task_count: u64,
    hidden: bool,
}

/// One registry entry with its read-time status, for `xcb workspaces list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub path: String,
    pub name: String,
    pub repo: Option<String>,
    pub admitted_by: String,
    pub first_seen_ms: u64,
    pub last_used_ms: u64,
    pub task_count: u64,
    /// `ok`, `invalid`, `container` or `hidden`.
    pub status: &'static str,
}

fn registry_rows(db: &Connection) -> Result<Vec<RegistryRow>> {
    let mut query = db.prepare(
        "SELECT path,name,repo,admitted_by,first_seen,last_used,task_count,hidden FROM workspaces ORDER BY last_used DESC,path ASC LIMIT ?1",
    )?;
    let rows = query
        .query_map([MAX_REGISTRY_ROWS], |row| {
            Ok(RegistryRow {
                path: row.get(0)?,
                name: row.get(1)?,
                repo: row.get(2)?,
                admitted_by: row.get(3)?,
                first_seen: u64::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                last_used: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                task_count: u64::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                hidden: row.get::<_, i64>(7)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A container holds another visible valid entry and is not a repository.
fn container_among(path: &str, valid: &[String]) -> bool {
    valid
        .iter()
        .any(|other| other != path && Path::new(other).starts_with(path))
        && !is_git_toplevel(Path::new(path))
}

/// How many entries of a launch directory are checked for repositories.
const LAUNCH_SCAN_ENTRIES: usize = 512;

/// A launch directory that looks like a directory of projects even before
/// the registry holds any of them: not a repository, and either a container
/// among the registered entries, a direct child of `$HOME` (`~/Documents`,
/// `~/src`), or the parent of a git toplevel among its first 512 entries.
/// Launching from one admits nothing; a human can still add it.
fn launch_container(path: &str, valid: &[String]) -> bool {
    let dir = Path::new(path);
    if is_git_toplevel(dir) {
        return false;
    }
    if container_among(path, valid) {
        return true;
    }
    if home_dir().is_none_or(|home| dir.parent() == Some(home.as_path())) {
        return true;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return true;
    };
    entries
        .take(LAUNCH_SCAN_ENTRIES)
        .flatten()
        .any(|entry| is_git_toplevel(&entry.path()))
}

pub(super) fn touch_workspace(tx: &Transaction<'_>, path: &str, now: u64) -> Result<()> {
    tx.execute(
        "INSERT INTO workspaces(path,name,repo,admitted_by,first_seen,last_used,task_count,hidden) VALUES(?1,?2,NULL,'history',?3,?3,1,0) ON CONFLICT(path) DO UPDATE SET last_used=max(last_used,excluded.last_used),task_count=task_count+1",
        params![path, basename(path), sql(now)?],
    )?;
    Ok(())
}

fn admitted_by_valid(admitted_by: &str) -> bool {
    matches!(
        admitted_by,
        "launch" | "dispatch" | "command" | "grant" | "memory"
    )
}

fn conversation_workspace_from(db: &Connection, id: &Id) -> Result<String> {
    if id.as_str() == GLOBAL_THREAD_ID {
        return Err(Error::Conflict(THREAD_SPANS));
    }
    let payload: String = db
        .query_row(
            "SELECT payload FROM conversations WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(Error::Unavailable("conversation not found"))?;
    let conversation: ManagedConversation = decode(&payload)?;
    conversation.validate()?;
    conversation.workspace.ok_or(Error::Conflict(THREAD_SPANS))
}

/// The grant for a conversation's workspace. Used by callers that still
/// hold a conversation id; the thread has no single workspace.
pub(super) fn policy_for_conversation(db: &Connection, id: &Id) -> Result<Option<ProjectPolicy>> {
    project::policy_from(db, &conversation_workspace_from(db, id)?)
}

impl ManagedStore {
    /// See [`validate_workspace_root`].
    pub fn validate_workspace(&self, path: &Path) -> Result<String> {
        validate_workspace_root(&self.root, path)
    }

    /// A view's workspace; the thread spans projects and has none.
    pub fn conversation_workspace(&self, id: &Id) -> Result<String> {
        conversation_workspace_from(&*self.db()?, id)
    }

    /// Registry name for a workspace, or its basename.
    pub fn workspace_name(&self, path: &str) -> Result<String> {
        let name: Option<String> = self
            .db()?
            .query_row("SELECT name FROM workspaces WHERE path=?1", [path], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(name.unwrap_or_else(|| basename(path)))
    }

    /// Snap a launch directory, prompt path or `/workspace <path>` to its
    /// project root: the nearest ancestor (at most 64 levels, never at or
    /// above `$HOME`) that is an admitted non-container entry or a git
    /// toplevel, else the validated directory itself. A file token snaps
    /// from its parent directory. Explicit workspaces never snap.
    pub fn snap_root(&self, path: &Path) -> Result<String> {
        let mut directory = path.to_path_buf();
        if !directory.is_dir() {
            let text = path.to_str().ok_or(Error::PrivateState)?;
            directory = PathBuf::from(strip_position(text));
            if !directory.is_dir() {
                directory = directory
                    .parent()
                    .ok_or(Error::Unavailable("managed workspace is not a directory"))?
                    .to_path_buf();
            }
        }
        let validated = self.validate_workspace(&directory)?;
        let known = self.known_workspaces(256)?;
        let home = home_dir();
        let mut current = Some(PathBuf::from(&validated));
        for _ in 0..MAX_SNAP_LEVELS {
            let Some(candidate) = current else { break };
            if home
                .as_ref()
                .is_some_and(|home| home.starts_with(&candidate))
            {
                break;
            }
            let text = candidate.to_str().ok_or(Error::PrivateState)?;
            let admitted = known
                .iter()
                .any(|entry| entry.path == text && !entry.container);
            if admitted || is_git_toplevel(&candidate) {
                return Ok(match self.validate_workspace(&candidate) {
                    Ok(root) => root,
                    Err(_) => validated,
                });
            }
            current = candidate.parent().map(Path::to_path_buf);
        }
        Ok(validated)
    }

    /// Admit a directory by explicit act. A non-explicit (`launch`)
    /// admission never admits a container or a directory that looks like
    /// one before the registry knows its projects.
    pub fn admit_workspace(
        &self,
        path: &Path,
        admitted_by: &str,
        name: Option<&str>,
    ) -> Result<String> {
        let canonical = self.validate_workspace(path)?;
        let valid: Vec<String> = self
            .known_workspaces(MAX_REGISTRY_ROWS as usize)?
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        admit_tx(&tx, &canonical, admitted_by, name, &valid, now_ms())?;
        tx.commit()?;
        Ok(canonical)
    }

    /// Whether a `launch` admission of this canonical root would succeed,
    /// without writing it: the read-only launch hint's check.
    pub fn launch_admissible(&self, canonical: &str) -> Result<bool> {
        let valid: Vec<String> = self
            .known_workspaces(MAX_REGISTRY_ROWS as usize)?
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        Ok(!launch_container(canonical, &valid))
    }

    /// Visible registry entries that validate now, `last_used DESC, path
    /// ASC`. Read-only: an entry that fails validation is omitted, never
    /// hidden, so an unmounted volume comes back by itself.
    pub fn known_workspaces(&self, limit: usize) -> Result<Vec<KnownWorkspace>> {
        let rows = registry_rows(&*self.db()?)?;
        let valid: Vec<RegistryRow> = rows
            .into_iter()
            .filter(|row| !row.hidden)
            .filter(|row| {
                self.validate_workspace(Path::new(&row.path))
                    .is_ok_and(|canonical| canonical == row.path)
            })
            .collect();
        let paths: Vec<String> = valid.iter().map(|row| row.path.clone()).collect();
        Ok(valid
            .into_iter()
            .take(limit)
            .map(|row| KnownWorkspace {
                container: container_among(&row.path, &paths),
                explicit_add: row.admitted_by == "command",
                path: row.path,
                name: row.name,
                repo: row.repo,
                last_used_ms: row.last_used,
            })
            .collect())
    }

    /// Every registry entry with its status, for `xcb workspaces list`.
    pub fn all_workspaces(&self) -> Result<Vec<WorkspaceStatus>> {
        let rows = registry_rows(&*self.db()?)?;
        let valid: Vec<String> = rows
            .iter()
            .filter(|row| !row.hidden)
            .filter(|row| {
                self.validate_workspace(Path::new(&row.path))
                    .is_ok_and(|canonical| canonical == row.path)
            })
            .map(|row| row.path.clone())
            .collect();
        Ok(rows
            .into_iter()
            .map(|row| WorkspaceStatus {
                status: if row.hidden {
                    "hidden"
                } else if !valid.contains(&row.path) {
                    "invalid"
                } else if container_among(&row.path, &valid) {
                    "container"
                } else {
                    "ok"
                },
                path: row.path,
                name: row.name,
                repo: row.repo,
                admitted_by: row.admitted_by,
                first_seen_ms: row.first_seen,
                last_used_ms: row.last_used,
                task_count: row.task_count,
            })
            .collect())
    }

    /// Entries whose name or repository tail equals `name` exactly,
    /// excluding hidden entries and containers.
    pub fn lookup_name(&self, name: &str) -> Result<Vec<String>> {
        let mut hits: Vec<String> = self
            .known_workspaces(MAX_REGISTRY_ROWS as usize)?
            .into_iter()
            .filter(|entry| !entry.container)
            .filter(|entry| {
                entry.name == name
                    || entry
                        .repo
                        .as_deref()
                        .and_then(|repo| repo.rsplit('/').next())
                        == Some(name)
            })
            .map(|entry| entry.path)
            .collect();
        hits.sort();
        hits.dedup();
        Ok(hits)
    }

    /// The CLI `<scope>` order: a path, a legacy conversation id, a unique
    /// registry name, a directory relative to `cwd`, else an error naming
    /// the candidates.
    pub fn resolve_scope(&self, value: &str, cwd: &Path) -> Result<String> {
        let value = value.trim();
        if value.is_empty() {
            return Err(xcb_core::Error::Invalid("project scope").into());
        }
        if value.contains('/') || value == "." || value == ".." {
            return self.validate_workspace(&cwd.join(value));
        }
        if let Ok(id) = Id::new(value) {
            if id.as_str() == GLOBAL_THREAD_ID {
                return Err(Error::Conflict(THREAD_SPANS));
            }
            if value.starts_with("c_") {
                match self.resolve_conversation(&id) {
                    Ok(id) => return self.conversation_workspace(&id),
                    Err(Error::Unavailable(_)) => (),
                    Err(error) => return Err(error),
                }
            }
        }
        match self.lookup_name(value)?.as_slice() {
            [only] => return Ok(only.clone()),
            [] => (),
            several => {
                return Err(Error::Guided {
                    message: format!("`{value}` names several projects: {}", several.join(", ")),
                    next: Some("name the directory instead".into()),
                });
            }
        }
        let relative = cwd.join(value);
        if relative.is_dir() {
            return self.validate_workspace(&relative);
        }
        let mut candidates: Vec<String> = self
            .known_workspaces(8)?
            .into_iter()
            .filter(|entry| !entry.container)
            .map(|entry| entry.name)
            .collect();
        candidates.sort();
        Err(Error::Guided {
            message: if candidates.is_empty() {
                format!("`{value}` is not a known project or directory")
            } else {
                format!(
                    "`{value}` is not a known project or directory; known projects: {}",
                    candidates.join(", ")
                )
            },
            next: Some("xcb workspaces add <dir>".into()),
        })
    }

    pub fn hide_workspace(&self, path: &str) -> Result<()> {
        self.set_workspace_hidden(path, true)
    }

    pub fn show_workspace(&self, path: &str) -> Result<()> {
        self.set_workspace_hidden(path, false)
    }

    fn set_workspace_hidden(&self, path: &str, hidden: bool) -> Result<()> {
        let db = self.write_db()?;
        if db.execute(
            "UPDATE workspaces SET hidden=?1 WHERE path=?2",
            params![i64::from(hidden), path],
        )? != 1
        {
            return Err(Error::Unavailable("workspace is not in the registry"));
        }
        Ok(())
    }

    pub fn migration_conflicts(&self, open_only: bool) -> Result<Vec<MigrationConflict>> {
        conflicts_from(&*self.db()?, open_only, None)
    }

    /// Mark a workspace's open upgrade conflicts of one kind resolved.
    pub fn resolve_conflicts_for(&self, workspace: &str, kind: &str) -> Result<usize> {
        let db = self.write_db()?;
        resolve_conflicts_tx(&db, workspace, kind, now_ms())
    }

    /// Supervisor upkeep for the registry: fill repository identities at
    /// most once per path per day, notice each invalid entry at most once
    /// per day, and keep the upgrade-conflict notice up while any is open.
    pub(super) fn tick_workspace_identity(&self, now: u64) -> Result<()> {
        let day = now / DAY_MS;
        let due_at = |key: String, stamp: u64| -> bool {
            let Ok(mut checks) = self.workspace_checks.lock() else {
                return false;
            };
            if checks.get(&key) == Some(&stamp) {
                return false;
            }
            if checks.len() >= 8192 {
                checks.clear();
            }
            checks.insert(key, stamp);
            true
        };
        // Validating every entry touches the filesystem: once a minute.
        if !due_at("tick".into(), now / 60_000) {
            return Ok(());
        }
        let due = |key: String| due_at(key, day);
        let rows = registry_rows(&*self.db()?)?;
        for row in rows.iter().filter(|row| !row.hidden) {
            let valid = self
                .validate_workspace(Path::new(&row.path))
                .is_ok_and(|canonical| canonical == row.path);
            if !valid {
                if due(format!("invalid\0{}", row.path)) {
                    record_supervisor_fault(
                        self.root(),
                        &format!(
                            "project directory `{0}` is missing or not allowed; `xcb workspaces hide {0}` removes it from the picker",
                            xcb_core::display_text(&row.path, 512)
                        ),
                    );
                }
                continue;
            }
            if row.repo.is_none()
                && due(format!("repo\0{}", row.path))
                && let Some(repo) = repo_identity(Path::new(&row.path))
            {
                self.write_db()?.execute(
                    "UPDATE workspaces SET repo=?1 WHERE path=?2 AND repo IS NULL",
                    params![repo, row.path],
                )?;
            }
        }
        let open = self.migration_conflicts(true)?;
        let grants = open
            .iter()
            .filter(|c| c.kind == "grant" && c.disposition == "winner_paused")
            .count();
        if !open.is_empty()
            && due(format!(
                "conflicts\0{}\0{}",
                open.len(),
                now / (60 * 60 * 1000)
            ))
        {
            record_supervisor_fault(
                self.root(),
                &format!(
                    "{grants} project grant{} paused by upgrade and {} other upgrade conflict{} open; see `xcb projects` and `xcb workspaces conflicts`",
                    if grants == 1 { "" } else { "s" },
                    open.len() - grants,
                    if open.len() - grants == 1 { "" } else { "s" },
                ),
            );
        }
        Ok(())
    }

    /// The machine-global thread, created on first use. A deterministic id
    /// makes concurrent first uses converge on one row and one receipt.
    pub async fn global_thread(&self) -> Result<ManagedConversation> {
        let id = Id::new(GLOBAL_THREAD_ID)?;
        if let Some(existing) = self.conversation(&id)? {
            return Ok(existing);
        }
        let now = now_ms();
        let conversation = ManagedConversation {
            version: 1,
            id: id.clone(),
            title: "Thread".into(),
            workspace: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        conversation.validate()?;
        let (_, receipt, receipt_json) = Self::algal_receipt(&conversation).await?;
        {
            let mut db = self.write_db()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO conversations(id,updated_at,payload) VALUES(?1,?2,?3)",
                params![
                    id.as_str(),
                    sql(now)?,
                    serde_json::to_string(&conversation)?
                ],
            )?;
            if inserted == 1 {
                tx.execute(
                    "INSERT OR IGNORE INTO receipts(digest,task,revision,payload) VALUES(?1,NULL,?2,?3)",
                    params![receipt, sql(now)?, receipt_json],
                )?;
            }
            tx.commit()?;
        }
        self.conversation(&id)?
            .ok_or(Error::Unavailable("managed conversation not found"))
    }

    /// Preview the v7 upgrade on a private copy: the source store is only
    /// read, and `scratch_root` receives the migrated copy.
    pub fn migrate_copy(src_root: &Path, scratch_root: &Path) -> Result<UpgradeReport> {
        let source = src_root.join("managed").join("managed.sqlite");
        private::open_file(&source, MAX_DB_OPEN_BYTES)?;
        let scratch = private::directory(&scratch_root.join("managed"))?;
        let target = scratch.join("managed.sqlite");
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::Conflict("upgrade preview copy already exists"));
        }
        // The preview must never fill the volume ahead of the real upgrade.
        room_for_copy(&scratch, db_bytes(&source)).map_err(|why| {
            Error::guided(
                format!("the upgrade preview needs room for a copy: {why}"),
                "free disk space, then run xcb doctor --upgrade-plan again",
            )
        })?;
        let from_version = {
            let connection = Connection::open_with_flags(
                &source,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            connection.busy_timeout(Duration::from_secs(15))?;
            let version: u32 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version > SCHEMA_VERSION {
                return Err(Error::Unavailable(
                    "managed state was written by a newer xcb",
                ));
            }
            crate::os::owner_only(OpenOptions::new().write(true).create_new(true)).open(&target)?;
            connection.execute(
                "VACUUM INTO ?1",
                [target.to_str().ok_or(Error::PrivateState)?],
            )?;
            version
        };
        let copy = Self::open_with(scratch_root, false)?;
        let db = copy.db()?;
        let count = |sql: &str| -> Result<u64> {
            let value: i64 = db.query_row(sql, [], |row| row.get(0))?;
            Ok(u64::try_from(value).unwrap_or(0))
        };
        let to_version: u32 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        Ok(UpgradeReport {
            from_version,
            to_version,
            conversations: count("SELECT count(*) FROM conversations")?,
            tasks: count("SELECT count(*) FROM tasks")?,
            workspaces: count("SELECT count(*) FROM workspaces")?,
            grants: count("SELECT count(*) FROM project_policies")?,
            memory_bindings: count("SELECT count(*) FROM project_memory")?,
            conflicts: conflicts_from(&db, false, None)?,
        })
    }
}

pub(super) fn admit_tx(
    tx: &Transaction<'_>,
    canonical: &str,
    admitted_by: &str,
    name: Option<&str>,
    valid: &[String],
    now: u64,
) -> Result<()> {
    if !admitted_by_valid(admitted_by) {
        return Err(xcb_core::Error::Invalid("workspace admission").into());
    }
    if let Some(name) = name {
        label(name, 160)?;
        if name.trim().is_empty() || name.contains('/') {
            return Err(xcb_core::Error::Invalid("workspace name").into());
        }
    }
    if admitted_by == "launch" && launch_container(canonical, valid) {
        return Err(Error::Conflict(
            "workspace is not allowed: it holds other projects",
        ));
    }
    tx.execute(
        "INSERT INTO workspaces(path,name,repo,admitted_by,first_seen,last_used,task_count,hidden) VALUES(?1,?2,NULL,?3,?4,?4,0,0)
         ON CONFLICT(path) DO UPDATE SET last_used=max(last_used,excluded.last_used),
           name=CASE WHEN ?5 IS NULL THEN name ELSE excluded.name END,
           admitted_by=CASE WHEN excluded.admitted_by='command' THEN 'command' ELSE admitted_by END,
           hidden=CASE WHEN excluded.admitted_by='command' THEN 0 ELSE hidden END",
        params![
            canonical,
            name.map_or_else(|| basename(canonical), str::to_owned),
            admitted_by,
            sql(now)?,
            name,
        ],
    )?;
    Ok(())
}

pub(super) fn resolve_conflicts_tx(
    db: &Connection,
    workspace: &str,
    kind: &str,
    now: u64,
) -> Result<usize> {
    Ok(db.execute(
        "UPDATE project_migration_conflicts SET resolved_at=?1 WHERE workspace=?2 AND kind=?3 AND resolved_at IS NULL",
        params![sql(now)?, workspace, kind],
    )?)
}

pub(super) fn open_conflict(db: &Connection, workspace: &str, kind: &str) -> Result<bool> {
    Ok(!conflicts_from(db, true, Some((workspace, kind)))?.is_empty())
}

/// The directory's grant was paused by the upgrade and nobody has settled
/// it yet; a grant the owner paused later is merely paused.
pub(super) fn paused_by_upgrade(db: &Connection, workspace: &str) -> Result<bool> {
    Ok(conflicts_from(db, true, Some((workspace, "grant")))?
        .iter()
        .any(|conflict| conflict.disposition == "winner_paused"))
}

fn conflicts_from(
    db: &Connection,
    open_only: bool,
    filter: Option<(&str, &str)>,
) -> Result<Vec<MigrationConflict>> {
    let mut query = db.prepare(
        "SELECT id,kind,workspace,conversation,disposition,stranded_tasks,payload,created_at,resolved_at FROM project_migration_conflicts WHERE (?1=0 OR resolved_at IS NULL) AND (?2 IS NULL OR workspace=?2) AND (?3 IS NULL OR kind=?3) ORDER BY created_at,id LIMIT 4096",
    )?;
    let rows = query
        .query_map(
            params![
                i64::from(open_only),
                filter.map(|(workspace, _)| workspace),
                filter.map(|(_, kind)| kind)
            ],
            |row| {
                Ok(MigrationConflict {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    workspace: row.get(2)?,
                    conversation: row.get(3)?,
                    disposition: row.get(4)?,
                    stranded_tasks: u64::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                    payload: row.get(6)?,
                    created_at_ms: u64::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                    resolved_at_ms: row
                        .get::<_, Option<i64>>(8)?
                        .and_then(|at| u64::try_from(at).ok()),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Intake

/// Which surface submitted a thread prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Tui,
    Relay,
    Cli,
}
impl Origin {
    fn binding(self) -> BindingOrigin {
        match self {
            Self::Tui => BindingOrigin::Tui,
            Self::Relay => BindingOrigin::Relay,
            Self::Cli => BindingOrigin::Cli,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IntakeCues {
    pub origin: Origin,
    /// Relay absolute path or CLI `--workspace`: validated, never snapped.
    pub explicit: Option<PathBuf>,
    /// The task the prompt addresses; its workspace is validated, never snapped.
    pub target: Option<Id>,
    pub focus: Option<String>,
    pub launch_hint: Option<String>,
    /// Relay `@infer`.
    pub infer_only: bool,
}

/// The frozen intake contract carries the committed task by value.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Intake {
    Accepted {
        task: ManagedTask,
        workspace: String,
        binding: WorkspaceBinding,
        hold_until_ms: Option<u64>,
    },
    /// Nothing was written.
    Ask {
        candidates: Vec<WorkspaceRow>,
        reason: String,
    },
}

impl ManagedStore {
    /// The committed task for this message id, verified like
    /// `existing_submission`: same conversation (the thread or `conversation`),
    /// user role, text and (when given) attachments.
    fn replay_intake(
        &self,
        conversation: &Id,
        message: &Id,
        text: &str,
        attachments: Option<&[Attachment]>,
    ) -> Result<Option<ManagedTask>> {
        let db = self.db()?;
        let saved: Option<(String, String)> = db
            .query_row(
                "SELECT conversation,payload FROM messages WHERE id=?1",
                [message.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let by_source: Option<String> = db
            .query_row(
                "SELECT id FROM tasks WHERE source_message=?1",
                [message.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let by_action: Option<String> = db
            .query_row(
                "SELECT source_task FROM habitat_calls WHERE id=?1",
                [format!("ui_{}", digest(message.as_str()))],
                |row| row.get(0),
            )
            .optional()?;
        let Some(task) = by_source.or(by_action) else {
            if saved.is_some() {
                return Err(Error::Conflict(
                    "message id was reused with different input",
                ));
            }
            return Ok(None);
        };
        let (saved_conversation, payload) = saved.ok_or(Error::Conflict(
            "message id was reused with different input",
        ))?;
        let saved: Message = decode(&payload)?;
        if (saved_conversation != GLOBAL_THREAD_ID && saved_conversation != conversation.as_str())
            || saved.role != Role::User
            || saved.text != text
            || attachments.is_some_and(|attachments| {
                serde_json::to_value(&saved.attachments).ok()
                    != serde_json::to_value(attachments).ok()
            })
        {
            return Err(Error::Conflict(
                "message id was reused with different input",
            ));
        }
        let task = task_from(&db, &Id::new(task)?)?.ok_or(Error::Conflict(
            "message id was reused with different input",
        ))?;
        if task.source_message != *message || task.conversation.as_str() != saved_conversation {
            return Err(Error::Conflict(
                "message id was reused with different input",
            ));
        }
        Ok(Some(task))
    }

    /// Resolve which workspace a prompt's task runs in. Replays a committed
    /// message first; otherwise builds the snapshot (explicit and target
    /// validated only, the launch hint and prompt tokens snapped) and asks
    /// the resolver. Writes nothing.
    pub fn resolve_intake(
        &self,
        conversation: &Id,
        message: &Id,
        text: &str,
        cues: &IntakeCues,
    ) -> Result<Resolution> {
        if let Some(task) = self.replay_intake(conversation, message, text, None)? {
            let binding = task.binding.clone().ok_or(Error::Conflict(
                "message id was reused with different input",
            ))?;
            return Ok(Resolution::Bound {
                workspace: task.workspace,
                binding,
                hold: task.hold_until_ms.is_some(),
            });
        }
        let known = self.known_workspaces(256)?;
        let explicit = cues
            .explicit
            .as_deref()
            .map(|path| self.validate_workspace(path))
            .transpose()?;
        let target = match &cues.target {
            Some(id) => {
                let task = self
                    .task(id)?
                    .ok_or(Error::Unavailable("managed task not found"))?;
                Some(self.validate_workspace(Path::new(&task.workspace))?)
            }
            None => None,
        };
        let usable_root = |root: Result<String>| -> std::result::Result<String, String> {
            let root = root.map_err(|error| match error {
                Error::Conflict(why) => why.to_owned(),
                error => error.to_string(),
            })?;
            if known
                .iter()
                .any(|entry| entry.path == root && entry.container)
            {
                return Err("container".into());
            }
            Ok(root)
        };
        let launch_hint = cues
            .launch_hint
            .as_deref()
            .and_then(|hint| usable_root(self.snap_root(Path::new(hint))).ok());
        let prompt_roots: Vec<_> = workspace_infer::path_tokens(text)
            .iter()
            .map(|token| usable_root(home_path(token).and_then(|path| self.snap_root(&path))))
            .collect();
        let last_thread_task = self.last_thread_task(cues.infer_only)?;
        let resolver_cues = Cues {
            explicit: explicit.as_deref(),
            target: target.as_deref(),
            focus: cues.focus.as_deref(),
            launch_hint: launch_hint.as_deref(),
            last_thread_task: last_thread_task
                .as_ref()
                .map(|(workspace, at)| (workspace.as_str(), *at)),
            prompt_roots,
            allow_admit: cues.origin != Origin::Relay,
            allow_guess: !cues.infer_only,
            infer_only: cues.infer_only,
        };
        Ok(
            match workspace_infer::resolve(text, &resolver_cues, &known, now_ms()) {
                Resolution::Bound {
                    workspace,
                    mut binding,
                    hold,
                } => {
                    binding.origin = cues.origin.binding();
                    Resolution::Bound {
                        workspace,
                        binding,
                        hold: hold && cues.origin == Origin::Tui,
                    }
                }
                ask => ask,
            },
        )
    }

    /// The most recent thread task (for `infer_only`, the most recent one a
    /// relay created): its workspace and last update.
    fn last_thread_task(&self, relay_only: bool) -> Result<Option<(String, u64)>> {
        let row: Option<(String, i64)> = self
            .db()?
            .query_row(
                "SELECT workspace,updated_at FROM tasks WHERE conversation=?1 AND workspace IS NOT NULL AND (?2=0 OR json_extract(payload,'$.binding.origin')='relay') ORDER BY updated_at DESC,id LIMIT 1",
                params![GLOBAL_THREAD_ID, i64::from(relay_only)],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(workspace, at)| (workspace, u64::try_from(at).unwrap_or(0))))
    }

    fn workspace_row(&self, path: &str, new: bool) -> Result<WorkspaceRow> {
        let db = self.db()?;
        let entry: Option<(String, Option<String>, i64)> = db
            .query_row(
                "SELECT name,repo,last_used FROM workspaces WHERE path=?1",
                [path],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let active: i64 = db.query_row(
            &format!("SELECT count(*) FROM tasks WHERE workspace=?1 AND state IN {NONTERMINAL}"),
            [path],
            |row| row.get(0),
        )?;
        let (name, repo, last_used) = entry.unwrap_or_else(|| (basename(path), None, 0));
        Ok(WorkspaceRow {
            path: path.to_owned(),
            name,
            repo,
            last_used_ms: u64::try_from(last_used).unwrap_or(0),
            active: usize::try_from(active).unwrap_or(0),
            container: false,
            new,
        })
    }

    /// Submit a prompt to the thread. An `Ask` writes nothing; a bound
    /// prompt commits its message, task, receipt and registry touch in one
    /// transaction. A retry of the same message id replays the committed task.
    /// The committed thread task for a retried operation, before any
    /// workspace resolution or admission runs (I3): a registry change since
    /// the first attempt never turns a committed dispatch into a failure.
    pub(crate) fn replay_thread_submission(
        &self,
        message: &Id,
        text: &str,
    ) -> Result<Option<(ManagedTask, WorkspaceBinding)>> {
        let thread = Id::new(GLOBAL_THREAD_ID)?;
        let Some(task) = self.replay_intake(&thread, message, text, Some(&[]))? else {
            return Ok(None);
        };
        let binding = task.binding.clone().ok_or(Error::Conflict(
            "message id was reused with different input",
        ))?;
        Ok(Some((task, binding)))
    }

    pub async fn submit_to_thread(
        &self,
        message: Id,
        text: String,
        attachments: Vec<Attachment>,
        cues: IntakeCues,
    ) -> Result<Intake> {
        bounded_text(&text, xcb_core::MAX_TEXT_BYTES)?;
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(xcb_core::Error::Invalid("empty task").into());
        }
        if attachments.len() > 8 {
            return Err(xcb_core::Error::Limit("managed attachments").into());
        }
        for attachment in &attachments {
            attachment.validate()?;
        }
        let thread = Id::new(GLOBAL_THREAD_ID)?;
        let accepted = |task: ManagedTask| -> Result<Intake> {
            let binding = task.binding.clone().ok_or(Error::Conflict(
                "message id was reused with different input",
            ))?;
            Ok(Intake::Accepted {
                workspace: task.workspace.clone(),
                hold_until_ms: task.hold_until_ms,
                binding,
                task,
            })
        };
        if let Some(task) = self.replay_intake(&thread, &message, &text, Some(&attachments))? {
            return accepted(task);
        }
        let (workspace, binding, hold) =
            match self.resolve_intake(&thread, &message, &text, &cues)? {
                Resolution::Bound {
                    workspace,
                    binding,
                    hold,
                } => (workspace, binding, hold),
                Resolution::Ask {
                    candidates,
                    new_roots,
                    reason,
                } => {
                    let mut rows = Vec::new();
                    let known = self.known_workspaces(256)?;
                    for path in candidates.iter().take(8) {
                        let mut row = self.workspace_row(path, false)?;
                        row.container = known
                            .iter()
                            .any(|entry| entry.path == *path && entry.container);
                        rows.push(row);
                    }
                    for path in new_roots.iter().take(8) {
                        rows.push(self.workspace_row(path, true)?);
                    }
                    return Ok(Intake::Ask {
                        candidates: rows,
                        reason,
                    });
                }
            };
        self.global_thread().await?;
        let task_id = Id::new(format!(
            "t_{}",
            digest(format!("xcb-task-v1\0{thread}\0{message}\0{workspace}"))
        ))?;
        let action = habitat::UiMutation::new(
            &message,
            &task_id,
            json!({"action":"submit_new","conversation":thread,"text":text,"attachments":attachments}),
        )?;
        let options = habitat::CreateOptions {
            ui: Some(&action),
            binding: Some(binding),
            hold_until_ms: hold
                .then(|| now_ms().saturating_add(workspace_infer::WORKSPACE_HOLD_MS)),
            ..Default::default()
        };
        let created = self
            .create_habitat_task(
                &thread,
                message.clone(),
                text.clone(),
                attachments.clone(),
                Path::new(&workspace),
                options,
            )
            .await;
        match created {
            Ok(task) => accepted(task),
            // A concurrent retry committed first: replay what it committed.
            Err(error @ (Error::Conflict(_) | Error::Database(_))) => {
                match self.replay_intake(&thread, &message, &text, Some(&attachments))? {
                    Some(task) => accepted(task),
                    None => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Recreate an unstarted thread task in another directory: in one
    /// transaction the task is cancelled (nothing ran, so it has no effects)
    /// and recreated in `target` with a deterministic message id, so a retry
    /// replays. Only prompts a person or controller typed can move; work that
    /// carries a project's authority provenance is cancelled instead.
    pub async fn move_task(
        &self,
        task: &Id,
        expected_revision: u64,
        target: &str,
    ) -> Result<ManagedTask> {
        let current = self
            .task(task)?
            .ok_or(Error::Unavailable("managed task not found"))?;
        let workspace = self.validate_workspace(Path::new(target))?;
        let thread = Id::new(GLOBAL_THREAD_ID)?;
        let message = Id::new(format!(
            "m_mv_{}",
            digest(format!("{}\0{workspace}", current.id))
        ))?;
        let moved_id = Id::new(format!(
            "t_{}",
            digest(format!("xcb-task-v1\0{thread}\0{message}\0{workspace}"))
        ))?;
        if let Some(moved) = self.task(&moved_id)? {
            return if moved.moved_from.as_ref() == Some(&current.id) {
                Ok(moved)
            } else {
                Err(Error::Conflict(
                    "message id was reused with different input",
                ))
            };
        }
        let binding = movable(&current)?;
        if current.workspace == workspace {
            return Ok(current);
        }
        let name = self.workspace_name(&current.workspace)?;
        if current.state != TaskState::Queued
            || current.attempts != 0
            || current.session.is_some()
            || !current.worker_sessions.is_empty()
            || current.cancel_requested
            || current.revision != expected_revision
        {
            return Err(Error::guided(
                format!(
                    "task already started in {name}; its effects stay there — cancel it and send the prompt again"
                ),
                "/tasks",
            ));
        }
        if self
            .known_workspaces(MAX_REGISTRY_ROWS as usize)?
            .iter()
            .any(|entry| entry.path == workspace && entry.container && !entry.explicit_add)
        {
            return Err(Error::Conflict(
                "that directory holds other projects; /workspace add it to use it as one",
            ));
        }
        let target_name = self.workspace_name(&workspace)?;
        let mut cancelled = current.clone();
        cancelled.state = TaskState::Cancelled;
        cancelled.deferred = false;
        cancelled.attention = None;
        cancelled.hold_until_ms = None;
        cancelled.detail = format!("moved to {target_name}");
        cancelled.next_prompt.clear();
        cancelled.attachments.clear();
        cancelled.revision += 1;
        cancelled.updated_at_ms = now_ms().max(current.updated_at_ms);
        cancelled.validate()?;
        if !cancelled.same_identity(&current) {
            return Err(Error::Conflict("managed task transition changed identity"));
        }
        let (policy, receipt, receipt_json) = Self::algal_receipt(&cancelled).await?;
        if policy != current.policy_digest {
            return Err(Error::Conflict("managed task policy changed"));
        }
        cancelled.last_receipt = receipt.clone();
        let notice = Self::assistant(
            format!("**{}** · moved to `{target_name}`", cancelled.title),
            Some(&cancelled.id),
            cancelled.revision,
        );
        let options = habitat::CreateOptions {
            deferred: current.deferred,
            priority: current.priority,
            binding: Some(WorkspaceBinding {
                source: workspace_infer::BindingSource::Moved,
                confidence: workspace_infer::BindingConfidence::High,
                origin: binding.origin,
                reason: format!("moved from {}", current.id),
                alternatives: vec![],
            }),
            moved_from: Some(current.id.clone()),
            ..Default::default()
        };
        let prepared = self
            .prepare_habitat_task(
                &thread,
                message,
                current.goal.clone(),
                current.attachments.clone(),
                Path::new(&workspace),
                &options,
            )
            .await?;
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let saved =
            task_from(&tx, &current.id)?.ok_or(Error::Unavailable("managed task not found"))?;
        if serde_json::to_string(&saved)? != serde_json::to_string(&current)? {
            return Err(Error::guided(
                format!(
                    "task already started in {name}; its effects stay there — cancel it and send the prompt again"
                ),
                "/tasks",
            ));
        }
        if tx.execute(
            "UPDATE tasks SET state=?1,revision=?2,updated_at=?3,payload=?4 WHERE id=?5 AND revision=?6",
            params![
                cancelled.state.as_str(),
                sql(cancelled.revision)?,
                sql(cancelled.updated_at_ms)?,
                serde_json::to_string(&cancelled)?,
                cancelled.id.as_str(),
                sql(current.revision)?
            ],
        )? != 1
        {
            return Err(Error::Conflict("managed task revision changed"));
        }
        tx.execute(
            "INSERT INTO receipts(digest,task,revision,payload) VALUES(?1,?2,?3,?4)",
            params![
                receipt,
                cancelled.id.as_str(),
                sql(cancelled.revision)?,
                receipt_json
            ],
        )?;
        Self::append_message_tx(&tx, &notice, &cancelled.conversation, Some(&cancelled.id))?;
        inbox::transition(&tx, &current, &cancelled, None)?;
        program_state::transition(&tx, &current, &cancelled, None)?;
        let moved = Self::create_habitat_task_tx(&tx, &prepared, &options)?;
        tx.commit()?;
        Ok(moved)
    }

    /// Dispatch a held thread task now.
    pub async fn release_hold(&self, task: &Id, expected_revision: u64) -> Result<ManagedTask> {
        let current = self
            .task(task)?
            .ok_or(Error::Unavailable("managed task not found"))?;
        if current.revision != expected_revision {
            return Err(Error::Conflict("managed task revision changed"));
        }
        if current.hold_until_ms.is_none() {
            return Ok(current);
        }
        self.clear_hold(&current).await
    }

    /// The supervisor's first tick after a hold expires clears it.
    pub(super) async fn expire_hold(&self, task: &ManagedTask) -> Result<ManagedTask> {
        match task.hold_until_ms {
            Some(until) if until > now_ms() => Err(Error::Conflict("task is still held")),
            Some(_) => self.clear_hold(task).await,
            None => Ok(task.clone()),
        }
    }

    async fn clear_hold(&self, task: &ManagedTask) -> Result<ManagedTask> {
        let mut next = task.clone();
        next.hold_until_ms = None;
        next.revision += 1;
        next.updated_at_ms = now_ms().max(task.updated_at_ms);
        self.transition(task, next, None).await
    }
}

/// The binding of a thread task a person or controller typed, or why it
/// cannot move.
fn movable(task: &ManagedTask) -> Result<&WorkspaceBinding> {
    let binding = task
        .binding
        .as_ref()
        .filter(|_| task.conversation.as_str() == GLOBAL_THREAD_ID)
        .ok_or(Error::Conflict(
            "this task belongs to a project view; cancel it instead",
        ))?;
    let creator = if task.daemon_child.is_some() {
        BindingOrigin::Daemon
    } else if task.program.is_some() || task.program_child.is_some() {
        BindingOrigin::Program
    } else if task.schedule.is_some() {
        BindingOrigin::Schedule
    } else if task.project_proposal.is_some() {
        BindingOrigin::Worker
    } else {
        binding.origin
    };
    match creator {
        BindingOrigin::Tui | BindingOrigin::Cli | BindingOrigin::Relay => Ok(binding),
        BindingOrigin::Worker => Err(Error::Conflict(
            "this task was created by worker; cancel it instead",
        )),
        BindingOrigin::Program => Err(Error::Conflict(
            "this task was created by program; cancel it instead",
        )),
        BindingOrigin::Daemon => Err(Error::Conflict(
            "this task was created by daemon; cancel it instead",
        )),
        BindingOrigin::Schedule => Err(Error::Conflict(
            "this task was created by schedule; cancel it instead",
        )),
    }
}
