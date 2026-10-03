# One global thread, workspace-keyed projects — 0.9.0

## Outcome

Plain `xcb` opens one machine-global managed conversation, **the thread**, from
any directory. The owner types work into it, and the harness decides which
project directory each prompt's task runs in. The ack says which directory it
chose and why. A **project** is now a canonical workspace directory, not a
conversation. Grants, the backlog, working memory, the Wordcell binding,
schedule authority and worker habitat tools are all keyed on `task.workspace`.
A grant for one directory can never authorize work in another, even though both
live in the same thread.

Recovered context: the Devin session `xcb` proposed items (1)–(4) below and Ben
approved them. On the one open question, the owner decided: "I want the harness
to infer". Directory scoping is the metaharness's job. The launch cwd is at most
a hint or fallback. The owner drives xcb mostly through the Grok controller over
the remote CLI, and through the TUI on two laptops. Base: origin/main `5756a49`,
package 0.8.13, managed `user_version` 6.

Approved scope:

1. Re-key `project_memory` (the Wordcell binding) on workspace.
2. Re-key the rest on workspace: backlog scoping, working memory, `policy_from`
   and project grants, and every other conversation-wide "project" query.
3. `xcb memory configure|status|search` take a workspace directory. A legacy
   conversation id still works as an alias.
4. Add the global thread. `ManagedConversation.workspace` becomes optional
   (`None` means the thread). Bare `xcb` opens the thread, and each task's
   workspace binds per prompt by harness inference.

This revision folds in the design-panel review (see "Review notes" at the end).

## Invariants

- **I1 Confinement is unchanged.** It still follows `task.workspace`. That
  covers the `active_workspaces` tick gate, `workspace_busy`,
  `kernel::new_session` and `workspace_lease`. Every new `task.workspace` is
  the exact string returned by `validate_workspace_root` (see Validation).
  Explicit and target workspaces (relay absolute paths, CLI/schedule/daemon
  `--workspace`, the addressed task's workspace) are validated and **never
  snapped**. Only the launch hint, prompt path tokens and `/workspace <path>`
  snap. Inference never produces `/`, `$HOME` or any of its ancestors, a hidden
  top-level directory under `$HOME` (`~/.ssh`, `~/.aws`, `~/.config`, …) or
  anything inside one, `~/Library` or anything inside it, an xcb state or
  coordination root, a system directory, or a *container* (a non-repository
  ancestor of another known workspace).
- **I2 Workspace is immutable task identity.** It stays in `same_identity`. A
  wrong binding is corrected only by cancel-and-recreate before dispatch, never
  by an in-place rebind.
- **I3 Resolve once per message id.** Any retry replays the committed task
  before inference runs. A replay whose conversation, text or attachments
  differ from the committed message is a `Conflict`, never a silent `Ok`. The
  task and operation digest formats
  (`xcb-task-v1\0{conversation}\0{message}\0{workspace}`) are unchanged.
- **I4 Every inferred binding is explained.** It carries a `WorkspaceBinding`
  (source, confidence, origin, reason, alternatives) that is stored in the task
  payload and its ALGAL receipt, and it is shown in the ack, the task row and
  the inspect modal. Every thread task carries a binding, including tasks
  created by workers, programs, daemons, schedules and relay dispatch.
- **I5 A doubtful guess never executes silently.** A low-confidence binding,
  and a prompt path or name mention that overrides the TUI focus, is held for
  8 s before first dispatch, and the hold is visible and correctable. When no
  safe default exists the harness asks and writes nothing.
- **I6 Only a human or controller act admits a directory.** A never-seen
  directory enters the known-workspace registry only through such an act
  (`xcb workspaces add`, `/workspace add`, picking a "new" candidate in the
  picker, relay absolute dispatch, CLI `--workspace`, a grant or memory
  configure). Model output, worker output, relayed prompt text and prompt prose
  never admit one: a prompt naming an unregistered directory **asks**. A judge,
  when one is added later, only picks among opaque keys.
- **I7 Authority is never inferred.** `/project`, `/memory`, `xcb projects` and
  `xcb memory` take an explicit directory, the TUI focus, the selected task's
  workspace or a project view's workspace. So do thread **schedules and backlog
  enqueues** (`Schedule`, `Enqueue`, `EnqueueIn`, deferred or not), because a
  schedule is standing authority and a deferred backlog item can be released
  unattended by a grant. The resolved directory is fixed at keystroke time and
  echoed back. Nothing below the focus rung is ever used for these.
- **I8 The migration never sums or merges authority.** Budgets are never
  summed. Goals and providers are never merged. Conflicting Wordcell bindings
  fail closed. Old binaries refuse v7. One deliberate scope change, approved by
  the owner and stated in the upgrade notes: a grant that was the only grant
  for its directory moves from its conversation to that directory, so it now
  covers every conversation over that directory, including relay dispatches
  and thread tasks bound there.
- **I9 The remote contract only grows.** The `task_dispatch` wire keys stay
  exactly `[kind, prompt, workspace]`. Result JSON and the fleet projection gain
  keys and lose none.

## Data model

### Conversations

`ManagedConversation.workspace: Option<String>` gets
`#[serde(default, skip_serializing_if = "Option::is_none")]`. v6 payloads
decode as `Some`.

- `xcb_core::ui::GLOBAL_THREAD_ID = "c_global"`.
- `validate()` requires `(id == c_global) == workspace.is_none()`. When the
  workspace is `Some`, it must be absolute and at most 4096 bytes.
- `ManagedStore::global_thread() -> Result<ManagedConversation>` (async)
  returns the row, or runs `INSERT OR IGNORE`. The row has title `Thread` and
  the same algal conversation receipt as `create_conversation`, written only
  when `changes()==1`.
- **`MAX_CONVERSATIONS`.** The thread is exempt: `create_conversation` counts
  `WHERE id <> 'c_global'`. `managed_view` pins the thread at the top of the
  conversation list whenever it exists, independent of the `conversations(64)`
  recency window, so it can never fall off the list.
- The thread is created lazily, never by migration. A deterministic id makes
  the race between the TUI and the relay executor harmless. The two laptops
  have separate stores, and controller output is per device. Read-only
  commands (`xcb conversations`, `xcb models route`) never create it.
- `create_conversation(&Path)` still creates per-directory **project views**,
  now validated.
- `latest_conversation_for_workspace` adds `AND id <> 'c_global'`.
- Thread retention keeps `RETENTION_GLOBAL_MESSAGES = 16_384`. Views keep
  4096.
- No conversation is re-parented. Legacy conversations remain resumable
  project views.

### Tasks

Add three fields to `ManagedTask`. Each gets
`#[serde(default, skip_serializing_if = "Option::is_none")]`, so pre-v7 receipts
replay through `verify_task` unchanged.

```rust
pub binding: Option<WorkspaceBinding>,   // in same_identity
pub hold_until_ms: Option<u64>,          // NOT identity; cleared on expiry/release
pub moved_from: Option<Id>,              // in same_identity
```

These binding types live in the new pure module
`crates/xcb-runtime/src/workspace_infer.rs`:

```rust
#[serde(rename_all = "snake_case")]
pub enum BindingSource { Explicit, Target, Mention, Focus, Continuation, Launch, Recent, Moved, Inherited }
#[serde(rename_all = "snake_case")]
pub enum BindingConfidence { High, Medium, Low }
#[serde(rename_all = "snake_case")]
pub enum BindingOrigin { Tui, Cli, Relay, Worker, Program, Daemon, Schedule }
#[serde(deny_unknown_fields)]
pub struct WorkspaceBinding {
    pub source: BindingSource,
    pub confidence: BindingConfidence,
    pub origin: BindingOrigin,                            // who created the task
    pub reason: String,                                   // label, <=160 bytes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,                        // <=4 canonical paths
}
```

`WorkspaceBinding` is new in this release, so `origin` is a required field.

Which binding each creator writes:

| Creator | Conversation | Binding |
|---|---|---|
| Legacy task, any task in a project view | view | `None` |
| TUI Enter in the thread | thread | resolver result, origin `tui` |
| CLI with `--workspace` into the thread | thread | `explicit`/high, origin `cli` |
| Relay absolute path or unique name | thread | `explicit`/high, origin `relay` |
| Relay `@infer` | thread | resolver result (restricted ladder), origin `relay` |
| TUI `Schedule`/`Enqueue`/`EnqueueIn` in the thread | thread | `explicit` or `focus` or `target`/high per the authority ladder, origin `tui` |
| Worker `xcb_backlog_add` from a thread task | thread | `inherited`/high, origin `worker`, reason `from t_…` |
| Program child, `finish_program` follow-up | thread | `inherited`/high, origin `program`, reason `from t_…` |
| Daemon child | thread | `inherited`/high, origin `daemon`, reason `from daemon <process name>` |
| Schedule occurrence | thread | `inherited`/high, origin `schedule`, reason `schedule s_…` |
| `move_task` recreation | thread | `moved`/high, origin copied from the old binding |

- `None` means a legacy or view-bound task. Every thread task carries `Some`.
- The inherited reasons are deterministic (they name the parent id, or the
  daemon's process name), so child replay through `same_identity` stays exact.
- Update the struct literals at `managed.rs` `create_habitat_task`,
  `managed_program_state.rs` (the child), `managed_daemon.rs` (the child) and
  the test helper `bare_task`.

### Schedules, projects and memory

- `HabitatSchedule.workspace: Option<String>` (serde default, skip when None).
  It is required when the owning conversation is the thread. Otherwise it
  defaults to `conversation.workspace`, and if both are set they must be equal.
- A schedule's workspace is resolved at creation by the authority ladder (I7)
  and never at fire time.
- `ProjectPolicy` replaces `conversation: Id` with `workspace: String`.
  `MemoryBinding` does the same.
- The private `ProjectPolicyV6` and `MemoryBindingV6` exist for migration
  decoding only.
- `wordcell::Promotion.conversation_id` stays `task.conversation`. It is
  provenance hashed into `request_digest`, and changing it would break the
  idempotent retry of prepared promotions.

### UI types (`crates/xcb-core/src/ui.rs`)

Additive fields are preferred so the ~20 existing constructors change minimally.

| Type | Change |
|---|---|
| `ConversationRow.workspace` | stays `String`; `""` = the thread. Add `fn is_thread(&self)`. |
| `TaskRow` | `+ binding: Option<String>` (short label, e.g. `continuing`), `+ hold_until_ms: Option<u64>` |
| `BacklogRow`, `ScheduleRow` | `+ workspace: String` (`""` when unresolved) |
| `ProjectRow` | `conversation` → `workspace: String`, `+ name: String`, `+ status: String` (`active`/`paused`/`paused by upgrade`/`expired`/`spent`) |
| `View` | `+ focus: Option<String>`, `+ launch_hint: Option<String>`, `+ workspaces: Vec<WorkspaceRow>` |
| `WorkspaceRow` (new) | `{ path, name, repo: Option<String>, last_used_ms, active: usize, container: bool, new: bool }` — `new` marks an unregistered prompt root offered by the picker for explicit add |
| `TranscriptPage` | `+ workspaces: BTreeMap<u64, String>` (sequence → task workspace for task-attributed messages) |
| `HabitatCommand::ConfigureProject` | `+ workspace: String` |
| `HabitatCommand::ProjectEnabled` | `conversation` → `workspace: String` |
| `HabitatCommand::MemorySearch` | `+ workspace: String` |
| `HabitatCommand::{Enqueue, EnqueueIn, Schedule}` | `+ workspace: Option<String>` (required for the thread; filled by `serve_ui` from the authority ladder) |
| `Intent` | `+ Focus(Option<String>)`, `+ MoveTask { task, revision, target: String }`, `+ ReleaseHold { task, revision }`, `+ AddWorkspace { path: String }` |
| `Update` | `+ WorkspaceBound { id, task, workspace, label }`, `+ ProjectPicker { id, candidates: Vec<WorkspaceRow>, reason: String }` |

The thread's "which project?" flow reuses `Update::SubmitRejected` to keep the
draft, followed by `ProjectPicker`.

## Schema v7

The single step `managed_workspace::migrate_v7` lives in the new
`crates/xcb-runtime/src/managed_workspace.rs`, included via `#[path]` like its
siblings. It runs after `daemon::migrate` (managed.rs:708).

- `open` refuses `version > 7` ("written by a newer xcb").
- `open` takes `managed_migration_guard` when `version < 7`, so a live old
  supervisor blocks the upgrade without mutating anything. The guard now
  **waits a bounded time** (poll `try_lock` every 100 ms for up to 20 s; the
  bound is a const a test can shorten) before returning the existing Conflict.
  This lets the TUI open and the `ensure_daemon`-spawned supervisor race through
  the pre-v7 backup: the loser waits, re-reads `user_version`, sees 7 and
  proceeds without the guard. A live old supervisor still ends in the Conflict.
- The step runs in one IMMEDIATE transaction. It re-reads `user_version`
  inside the transaction and returns if the value is at least 7.
- It detects the existing shape before every action, using
  `pragma_table_xinfo` (not `pragma_table_info`, which hides generated
  columns) and `sqlite_master`. This keeps it idempotent after
  `habitat::migrate` resets the version to 2.
- It does no filesystem I/O inside the transaction.

```sql
-- if absent (check pragma_table_xinfo('tasks')):
ALTER TABLE tasks ADD COLUMN workspace TEXT GENERATED ALWAYS AS
  (CASE WHEN json_valid(payload) THEN json_extract(payload,'$.workspace') END) VIRTUAL;
CREATE INDEX IF NOT EXISTS tasks_workspace_state ON tasks(workspace,state,updated_at,id);

CREATE TABLE IF NOT EXISTS workspaces(
  path TEXT PRIMARY KEY,        -- canonical, byte-equal to task.workspace
  name TEXT NOT NULL,           -- basename, or `xcb workspaces add --name`
  repo TEXT,                    -- owner/name from origin URL; filled lazily; ranking only
  admitted_by TEXT NOT NULL,    -- history|launch|dispatch|command|grant|memory
  first_seen INTEGER NOT NULL, last_used INTEGER NOT NULL,
  task_count INTEGER NOT NULL DEFAULT 0, hidden INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS workspaces_recent ON workspaces(hidden,last_used);

-- only when the legacy tables still have a `conversation` column and *_v6 is absent:
ALTER TABLE project_policies RENAME TO project_policies_v6;
ALTER TABLE project_memory   RENAME TO project_memory_v6;
-- when a legacy-shaped table exists AND *_v6 already exists (a v7 store whose
-- project tables were dropped and recreated by project::migrate after a version
-- reset): record each of its rows as a conflict (kind grant|memory, disposition
-- dropped, payload verbatim), DROP it, then create the v7 table below.
CREATE TABLE IF NOT EXISTS project_policies(workspace TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS project_memory  (workspace TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS project_migration_conflicts(
  id TEXT PRIMARY KEY, kind TEXT NOT NULL,          -- grant|memory|undecodable|orphan
  workspace TEXT, conversation TEXT NOT NULL,
  disposition TEXT NOT NULL,                        -- moved|merged|winner_paused|superseded|unbound|dropped
  stranded_tasks INTEGER NOT NULL DEFAULT 0, payload TEXT NOT NULL,
  created_at INTEGER NOT NULL, resolved_at INTEGER);
PRAGMA user_version=7;
```

- **`tasks.workspace` is a VIRTUAL generated column.** SQLite allows adding a
  VIRTUAL (not STORED) generated column with `ALTER TABLE`, and it can be
  indexed. It always equals the payload's workspace, so no `INSERT INTO tasks`
  site changes and no column/payload mismatch can exist. This also covers
  0.8.x processes that were already running when the store upgraded (a TUI
  `serve_ui`, an in-flight CLI call): they do not hold `supervisor.lock`, never
  re-read `user_version`, and keep inserting with the old column list. With a
  plain column those rows would have had a NULL workspace, and every
  workspace-keyed authority query would have skipped them. The `json_valid`
  guard is required because tests write `'{corrupt'` payloads (managed.rs
  ~8044); such rows get a NULL workspace and already land in `task_rows`'
  unreadable set because their payload does not decode. The bundled SQLite
  (rusqlite 0.40, `bundled`) supports generated columns.
- The new project tables have no foreign key, because a workspace is not a
  row. Never use `RENAME COLUMN`: it would keep
  `REFERENCES conversations(id)` on a column that holds paths.
- The legacy `*_v6` tables are kept read-only as audit and recovery data.
- **Registry backfill is DB-only.** It takes distinct absolute strings from
  conversations `$.workspace`, `tasks.workspace`, `route_stats.scope`,
  non-`global` `preferences.scope`, `daemon_meta.workspace` and the legacy
  project rows, with `admitted_by='history'`. `last_used` is the max related
  timestamp and `task_count` is the count.
- Validity is checked when a registry entry is used, never during migration.
  A moved directory or an old `~` conversation cannot break the upgrade, and
  it is never offered as a candidate.
- **Backup.** Before the transaction, when `1 <= version <= 6`, the file is at
  most 1 GiB and free space is at least twice the file size, run
  `VACUUM INTO <root>/managed/managed.pre-v7.<ms>.sqlite` with mode 0600.
  Otherwise skip the backup and record a supervisor notice. `retain()` prunes
  these files after 14 days. This backup is the only downgrade path.
- `ManagedStore::migrate_copy(src) -> UpgradeReport` runs the same chain on a
  private `VACUUM INTO` scratch copy without the guard, and returns counts plus
  conflict rows. `xcb doctor --upgrade-plan` uses it, and migration tests go
  through it too.

### Conflict rules (pure `plan_project_rekey`, tested directly)

- **Unrecoverable rows.** Undecodable rows, rows whose payload conversation
  differs from the key, and rows whose conversation has no readable workspace
  are recorded as `undecodable` or `orphan` with disposition `dropped`. This
  matches today's fault-and-skip behavior.
- **Wordcell bindings.** One row is moved with its revision. Byte-identical
  configs are merged with `revision = max`. Distinct configs **bind nothing**:
  every candidate is recorded as `memory`/`unbound`. `search_memory_in` then
  returns `Unavailable("conflicting Wordcell bindings from upgrade; run xcb
  memory configure <dir>")`, and the migration never calls `config.verify()`.
- **Grants, one row.** The row moves verbatim, keeping generation, goal, budget,
  `admitted_tasks`, expiry, provider and revision. In-flight proposals, program
  children and daemon children keep matching. This is the I8 scope change: the
  grant now covers every conversation over its directory.
- **Grants, several rows.** A grant is *active* when
  `enabled && expires > now && admitted < max`.
  - Exactly one active: it wins.
  - Otherwise the winner is the grant of the most recently updated
    conversation (`updated_at DESC, id`), then the latest expiry, then the id.
  - Two or more active: the winner is written with `enabled=false`,
    `revision+1` and disposition `winner_paused`.
  - The other rows are `superseded`, with `stranded_tasks` counting nonterminal
    tasks that reference their generation. Those tasks surface through the
    existing "project authority is paused, expired, or replaced" attention.
  - Budgets, goals and providers are never merged.
- **Resolution.**
  - `configure_project_policy_in` and `set_project_policy_enabled_in(.., true)`
    set `resolved_at` on the workspace's open grant conflicts. `bind_memory_in`
    does the same for memory conflicts.
  - A supervisor notice ("N project grants paused by upgrade; see
    `xcb projects`") stays up while open conflicts exist.

## Validation and the registry (`managed_workspace.rs`)

`validate_workspace_root(root: &Path, path: &Path) -> Result<String>` is the
single chokepoint. It runs these checks in order:

1. `canonicalize`, `is_dir`, UTF-8, at most 4096 bytes, no control characters.
2. Reject `/`, canonical `$HOME` and every ancestor of `$HOME`. Reject every
   hidden top-level directory under `$HOME` (`$HOME/.<name>`) and everything
   inside one, and `$HOME/Library` and everything inside it.
3. Reject any path that equals, contains or lies inside one of these roots: the
   live managed store root and its parent state root, `private::default_root()`,
   the coordination root (`coordination.rs` default or `XCB_COORDINATION_ROOT`),
   the attachments root and the input-recovery root.
4. Reject paths under `/System /usr /bin /sbin /etc /private/etc /dev /proc /sys`.
   Reject exact matches of `/Applications /Library /Volumes /tmp /private/tmp /var /private/var`.
   Children of `/tmp` and `/private/var/folders` stay valid, because test
   tempdirs live there.

Errors use `Conflict("workspace is not allowed: <home|filesystem root|hidden or library directory|xcb state|system directory>")`.
The function returns the canonical string, and that string is what gets stored.

Where it is called:
- `create_habitat_task`, for every conversation. For the thread the input must
  already equal the canonical string, otherwise `Conflict("workspace is not
  canonical")`. For a view the existing equality check also stays.
- `create_conversation`, relay dispatch, `admit_workspace`,
  `configure_project_policy_in`, `bind_memory_in`, and the CLI and TUI
  directory arguments.
- Again in `Supervisor::launch` before `new_session`. There, a result that
  differs from `task.workspace` fails the task with "workspace moved or was
  replaced since it was bound". This includes a legacy pre-v7 task whose stored
  workspace was never canonical (for example a symlinked or `~`-relative
  path).

This tightens today's behavior. A legacy view rooted at `$HOME` (or at a hidden
directory under it) can no longer start new tasks. Test fixtures that open the
store at the workspace directory, or put the workspace inside the state root,
move to sibling `base/state` and `base/work` directories; the validator is never
weakened for tests.

Registry and snapping:

- `snap_root(root, path) -> Result<String>`:
  - If `path` is not a directory (an absolute FILE token such as
    `/repo/src/x.rs:12`, with any `:line[:col]` suffix stripped first), use
    its parent directory.
  - Validate, then walk up **once**, at most 64 levels, never at or above
    `$HOME`, with no subprocess. Stop at the **first** directory that is either
    an admitted non-container registry entry or a git toplevel (a `.git`
    directory or a `gitdir:` file). A path inside a nested worktree
    (`/repo/.claude/worktrees/x/src`) or a nested repo therefore snaps to that
    worktree or repo, never to the outer checkout.
  - If neither is found it returns the validated directory itself.
  - The launch hint, prompt paths and `/workspace <path>` use it. Explicit and
    target workspaces never do; relay absolute paths and CLI/schedule/daemon
    `--workspace` bind exactly.
- `admit_workspace(path, admitted_by, name: Option<&str>)` runs the validator
  and then an upsert. **Container rule:** a *container* is a registry entry
  that is a strict ancestor of another visible valid entry **and is not a git
  toplevel**. A git toplevel is never a container, so admitting
  `/repo/site` or `/repo/.claude/worktrees/x` never disables `/repo`. A
  non-explicit admission (`launch`) whose root would be a container is refused,
  so launching from `~/Documents` admits nothing. Explicit acts (`command`,
  `dispatch`, `grant`, `memory`) may admit a container. Containers are never
  inferred. A container admitted by `command` (`xcb workspaces add`,
  `/workspace add`) may be used as a focus or target, because that is a human
  act; any other container is skipped on those rungs too.
- `known_workspaces(limit)` is **read-only**. It returns visible entries,
  ordered `last_used DESC, path ASC`, with `container` and `explicit_add`
  flags, and silently omits entries that fail validation at read time. It never
  writes `hidden`: a transiently unmounted volume or network path comes back by
  itself. `hidden=1` is written only by `xcb workspaces hide`. The supervisor
  tick (not the intake path) emits one notice per invalid entry per day.
- `all_workspaces() -> Vec<WorkspaceStatus>` lists every entry with status
  `ok`, `invalid`, `container` or `hidden`, for `xcb workspaces list`.
- `lookup_name(name) -> Result<Vec<String>>` returns entries whose `name` or
  repo tail equals `name` exactly, excluding hidden entries and containers. It
  serves relay names, the CLI scope parser and TUI `/workspace <name>`.
- `resolve_scope(value, cwd)` implements the CLI scope order (see Entry points).
- `migration_conflicts(open_only: bool)` and `resolve_conflicts_for(workspace,
  kind)` list and resolve `project_migration_conflicts` rows, for
  `xcb workspaces conflicts`, `xcb memory status` and `xcb projects`.
- `touch_workspace(tx, path)` runs inside the task-insert transaction and bumps
  `last_used` and `task_count`.
- `repo_identity(path)` reads `origin` from `.git/config` or from the linked
  worktree's `commondir` config, bounded to 64 KiB. It is filled by a supervisor
  tick at most once per path per day, never at intake. It only groups and ranks
  candidates.
- `view_stamp` includes `count(*)`, `max(last_used)` and `sum(hidden)` from
  `workspaces`, so `xcb workspaces add|hide|show` refreshes `View.workspaces`
  and the picker.

## Workspace inference

`workspace_infer.rs` is pure: no I/O and no clock. The store supplies a
snapshot. `path_tokens(text)` pulls out absolute and `~/` tokens that are
leading, follow `in|at|cd|under`, or are written `@/path`; it strips trailing
punctuation and caps at 8 tokens. The store snaps each token (files snap to
their parent directory) and passes the results in as `prompt_roots`.

```rust
pub struct KnownWorkspace { pub path: String, pub name: String, pub repo: Option<String>,
                            pub last_used_ms: u64, pub container: bool, pub explicit_add: bool }
pub struct Cues<'a> {
    pub explicit: Option<&'a str>,        // relay/CLI/schedule/daemon/program/view workspace — validated, never snapped
    pub target: Option<&'a str>,          // workspace of the task the prompt addresses — validated, never snapped
    pub focus: Option<&'a str>,           // TUI session focus (/workspace, grid card, Alt-←/→)
    pub launch_hint: Option<&'a str>,     // snap_root(cwd) if admitted and not a container
    pub last_thread_task: Option<(&'a str, u64)>,  // workspace, updated_at; for infer_only: the last RELAY-origin thread task
    pub prompt_roots: Vec<Result<String, String>>, // snapped roots or rejection reasons
    pub allow_admit: bool,                // TUI/CLI true (offer unregistered roots in Ask); relay false
    pub allow_guess: bool,                // false for relay @infer
    pub infer_only: bool,                 // relay @infer
}
pub enum Resolution {
    Bound { workspace: String, binding: WorkspaceBinding, hold: bool },
    Ask { candidates: Vec<String>, new_roots: Vec<String>, reason: String },
}
pub fn resolve(text: &str, cues: &Cues, known: &[KnownWorkspace], now_ms: u64) -> Resolution
```

`Resolution::Bound` has no `admit` flag: inference never admits (I6). The store
fills `binding.origin` from `IntakeCues.origin`.

The first rung that binds wins:

| # | Rule | Source / confidence | Hold (TUI only) |
|---|---|---|---|
| 0 | Replay: a task already exists for this message id (store side, before `resolve`) | saved | — |
| 1 | `explicit` | explicit / high | no |
| 2 | `target`, skipped when it is a container not admitted by `command` | target / high | no |
| 3 | Prompt path: exactly one distinct valid non-container root **that is in the registry**. A valid root that is not in the registry → **Ask**, offering it in `new_roots` when `allow_admit` (the picker marks it "new"; picking it is the explicit add). Two or more roots → **Ask**. | mention / high; medium when it differs from a set focus | when it differs from a set focus |
| 4 | Name mention: a whole-word, case-insensitive match (at least 3 characters) on `name`, the repo tail or `@name`, outside the stoplist `site docs app web api cli core test main src lib`. Containers never match. It must resolve to exactly one repo group, else **Ask**. One path in the group → high. Several worktrees → prefer focus, then the launch hint, if in the group (medium), else the most recent (low, alternatives recorded). | mention; a high match that differs from a set focus drops to medium | low, or when it differs from a set focus |
| 5 | `focus`, skipped when it is a container not admitted by `command` | focus / high | no |
| 6 | Continuation (see below) | continuation / medium | no |
| 7 | `launch_hint` (skipped unless `allow_guess`) | launch / medium | no |
| 8 | Most recent thread task within 6 h, else the most recent non-container registry entry within 30 days (skipped unless `allow_guess`) | recent / low | yes |
| 9 | **Ask** with up to 8 candidates ranked by focus, hint and recency | — | — |

- **Continuation (rung 6), settled wording.** It binds to `last_thread_task`'s
  workspace when that task was updated within `CONTINUE_WINDOW_MS` (6 h) AND
  the prompt has a continuation cue. A continuation cue is
  `continue_like(text)` OR `xcb_core::reflex::route_features(text, false,
  false).resume` OR (for TUI and CLI only) "at most 12 words and names no
  candidate". `continue_like` moves from managed.rs into `workspace_infer.rs`
  as a pure helper that managed.rs calls; `RESUME_CUES` stays private and is
  reached through `route_features`.
- **Relay `infer_only` continuation.** The short-prompt clause does not apply:
  the prompt needs `continue_like` or a `route_features(..).resume` cue, AND
  the store supplies as `last_thread_task` only the most recent thread task
  whose `binding.origin` is `relay`. A short remote prompt such as "run the
  tests" never binds to whatever the laptop user last touched in the TUI; it
  asks.
- A prompt path or name mention beats the focus, because naming a directory in
  the prompt is the more specific act. The ack says `(overrides focus <name>)`,
  and because repo names can be common words the binding drops to medium and
  is held.
- The ranking is deterministic (candidates sorted by path; ties go to
  `last_used DESC`, then path ASC). A test proves that registry row order never
  changes the result.
- A hold means `hold_until_ms = now + WORKSPACE_HOLD_MS` (a const of 8_000). It
  applies only to TUI origin. Relay and CLI never hold.
- Phase 2 (not this release): a `JudgeQuestion::Choice` over opaque keys
  `w0..w7`, with name-only descriptions, a 3 s timeout and p ≥ 0.75. It would
  sit between rungs 6 and 7. An observe-only `Reflex::Workspace` head would
  learn from `/workspace` corrections. Both fall through on any failure.

### Intake APIs (`ManagedStore`)

```rust
pub enum Origin { Tui, Relay, Cli }
pub struct IntakeCues { pub origin: Origin, pub explicit: Option<PathBuf>,
    pub target: Option<Id>, pub focus: Option<String>, pub launch_hint: Option<String>,
    pub infer_only: bool /* relay @infer */ }
pub enum Intake { Accepted { task: ManagedTask, workspace: String, binding: WorkspaceBinding, hold_until_ms: Option<u64> },
                  Ask { candidates: Vec<WorkspaceRow>, reason: String } }
pub fn resolve_intake(&self, conversation: &Id, message: &Id, text: &str, cues: &IntakeCues) -> Result<Resolution>;
pub async fn submit_to_thread(&self, message: Id, text: String, attachments: Vec<Attachment>, cues: IntakeCues) -> Result<Intake>;
pub async fn move_task(&self, task: &Id, expected_revision: u64, target: &str) -> Result<ManagedTask>;
pub async fn release_hold(&self, task: &Id, expected_revision: u64) -> Result<ManagedTask>;
```

- **`resolve_intake`** first replays. It looks up the task whose
  `source_message` is `message`, or the saved `habitat_calls` row for
  `ui_<digest(message)>`. If one exists, it checks, exactly like
  `existing_submission` (managed.rs ~1859), that the saved message's
  conversation is `c_global` (or the conversation passed in), its role is
  user, and its text and attachments equal the new input; any difference is
  `Conflict("message id was reused with different input")`. On a match it
  returns that task's workspace with its saved binding. Only then does it build
  the snapshot (`known_workspaces(256)`, the last thread task per the rung 6
  rules, `explicit` and `target` **validated only**, the launch hint and the
  prompt tokens **snapped**) and call `resolve`. It performs no writes.
  `submit_to_thread` and the CLI preview (`xcb models route`) use it. The TUI
  does not use it for `Schedule`/`Enqueue`/`EnqueueIn` (see I7).
- **`submit_to_thread`**:
  - It calls `resolve_intake`. An `Ask` writes nothing and returns, with
    `new_roots` mapped to `WorkspaceRow { new: true, .. }`.
  - A `Bound` result runs the create path on `c_global` with `CreateOptions {
    binding, hold_until_ms }`. The message, task, UiMutation and touch all
    commit in one transaction.
  - If a concurrent retry collides on a UNIQUE or UiMutation conflict, it
    re-runs the replay and returns the committed task as `Ok`.
- **No nested transactions.** `create_habitat_task` opens its own `write_db`
  and IMMEDIATE transaction and computes its async algal receipt before that
  (managed.rs ~2331-2341). Foundation splits it into an async preparation step
  (receipt and validation, outside any transaction) and
  `create_habitat_task_tx(tx: &Transaction, prepared, options)`, which only
  writes. `create_habitat_task` becomes prepare + open tx + `_tx` + commit.
  `move_task` prepares first and then calls `_tx` inside its own single
  transaction.
- **`submit_new`** stays the explicit path. Project views use it with
  `binding: None`.
- **Ack text.** When a binding is present the ack reads:
  ``Started **{title}** in `{name}` · {reason}``, then
  ``· starts in 8s · /workspace to move`` when held, otherwise
  ``· /workspace to move``. The ack for view-bound tasks is unchanged.
- **Hold.** The `Supervisor::tick` queued loop skips any task where
  `hold_until_ms > now`. Held tasks occupy no workspace slot. After the hold
  expires, the first tick clears it by `transition_ui`, and so does
  `release_hold`.
- **`move_task`**:
  - It is allowed only for a thread task whose `binding.origin` is `tui`,
    `cli` or `relay`, that has no `project_proposal`, no `schedule`, no
    `program`, no `program_child` and no `daemon_child`, and only while the
    task is `Queued`, with zero attempts, no session, no worker sessions, no
    pending cancel and a matching revision. Moving a proposal, a schedule
    occurrence or a worker/program/daemon-created task would carry one
    project's authority provenance into another, so these are refused with
    `Conflict("this task was created by {origin}; cancel it instead")`.
  - The target must validate; a container not admitted by `command` is
    refused.
  - In one transaction it cancels the task (effects none, detail `moved to
    {name}`) and recreates it via `create_habitat_task_tx` in the same
    conversation. The new task has message id `m_mv_<digest(old, target)>`,
    which is deterministic so a retry replays. It keeps the text, attachments
    and link, and sets `moved_from = old` with binding `moved`/high (origin
    copied) and no hold.
  - A started task returns `Conflict("task already started in {name}; its
    effects stay there — cancel it and send the prompt again")`.
  - A move to the task's current workspace is a no-op that returns the task
    unchanged.

### Serialization with many workspaces

- Same-workspace serialization already keys on `task.workspace`, so one thread
  over N workspaces runs up to `MAX_ACTIVE` concurrently.
- This release closes the nested-path gap. `workspaces_overlap(a, b)` returns
  true when either path `starts_with` the other. It replaces equality in the
  tick `active_workspaces` check, in `workspace_busy` and in the
  `kernel::workspace_lease` unsettled-writer scan. The flock stays per exact
  path.
- Conversation-wide queries become workspace queries on `tasks.workspace`:
  `no_outstanding`, `Occurrence::check`, `no_other_work`, `working_memory`,
  `project_context` and the `tick_projects` candidate query. Without this, the
  thread would make them machine-wide: one project would stall another
  project's schedule, and one project's summaries would leak into another's
  prompts.

## Project re-key

The key is the canonical `task.workspace` string, and a grant matches by exact
path. Snapping makes subdirectory prompts use the repo root, so the root's grant
applies to them. An explicit `/repo/sub` binding is not snapped and is not
covered by a grant on `/repo`.

- **API naming.**
  - Workspace-keyed primitives carry an `_in` suffix: `project_policy_in(&str)`,
    `configure_project_policy_in(&Path, …)`, `set_project_policy_enabled_in`,
    `project_policies()`, `memory_binding_in`, `bind_memory_in`,
    `search_memory_in`, `backlog_in(&str, limit)`,
    `working_memory_in(&str, limit)`, `working_memory_context_in` and
    `outstanding_in`.
  - The old conversation-taking names remain as shims. Each resolves
    `conversation.workspace` and errors with "the thread spans projects; name a
    directory" for `c_global`. This keeps about 100 test call sites compiling.
  - Production callers use the `_in` forms.
- **Callers that switch to `task.workspace`, `source.workspace` or
  `meta.workspace`:**
  - `check_dispatch`, and `ProjectAdmission`, where the parent must share both
    workspace and conversation and `no_outstanding_in` is used.
  - `create_habitat_task` (`schedule_requirement`, `program_generation`),
    `finish_program`, and every `require_grant` site in program_state, habitat
    and daemon.
  - `Supervisor` inbox wake, `launch`, `project_context_in` and
    `working_memory_context_in`.
  - `WorkerMutation::check`/`replay`, where the replay compares workspace and
    conversation.
  - Worker tools, all using `source.workspace`: `xcb_backlog_get`, `list`,
    `update` and `complete`, plus `xcb_memory_search` and `xcb_memory_recent`.
    `compact_task` and list/recent JSON gain `workspace` additively.
  - `tick_projects`, which uses a dedicated query:
    `tasks WHERE workspace=? AND state='queued'` plus deferred plus a matching
    proposal generation.
- **Inbox watches** drop the conversation equality and keep the workspace
  equality. The CLI `Watch` docstring (main.rs ~253) becomes "Task to observe
  in the same workspace."
- **Entry-created tasks.** `enqueue_backlog`, `enqueue_program`,
  `enqueue_daemon` and `create_schedule(_inner)` gain `workspace: Option<&Path>`.
  A view accepts `None` or an equal path. The thread requires `Some`; the caller
  has already resolved it (authority ladder in the TUI, `--workspace` or scope
  in the CLI). Thread tasks created this way, and worker/program/daemon/schedule
  children in the thread, get the bindings in the Tasks table.
- **Memory promotion.** `promote_memory(task, note)` takes the binding from
  `task.workspace`.
- **Why cross-project authorization is closed.** Every authority check uses the
  task's own workspace. Children inherit their parent's workspace. Worker tools
  compare `source.workspace`. Grants require a validated directory, so a grant
  on `$HOME`, a hidden home directory or a system directory is impossible.

## Entry points

### CLI

- **Bare `xcb` and `xcb chat`** open `global_thread()`. They pass
  `launch_hint = snap_root(cwd)` only when it validates, is admitted or
  admittable as `launch`, and is not a container. Launching from `~` or
  `~/Documents` therefore gives no hint.
- `--resume <id>` is unchanged and works for views and the thread.
- `--new` keeps today's meaning: it **always starts a new per-directory
  project view** for the validated cwd (`create_conversation`), even when one
  already exists. It never opens the thread and never reuses the latest view.
  This keeps `qualification/inbox-acceptance.py` and existing habits working.
- **Help text.** `after_help` becomes: "Plain `xcb` opens your thread from any
  directory; xcb picks each task's project directory and says which." The
  `--cwd` help becomes: "Project hint for the thread; the exact directory for
  run, chat --new and models route". `--new` help: "Start a new project view
  for this directory".
- `xcb conversations` lists the thread first as `thread (all projects)` when
  its row exists (listing never creates it), then `project view · <dir>` rows.
  `--json` emits explicit row objects: every row gains `"isThread": bool`, and
  the thread row carries `"workspace": null`. View rows keep every existing key.
  The CHANGELOG and `docs/remote-operations.md` document the null.
- **New `xcb workspaces`:**
  - `list [--json]` shows name, path, repo, admitted_by, last used, tasks and
    status (`ok`, `invalid`, `container` or `hidden`) from `all_workspaces()`,
    plus open migration conflicts.
  - `add <dir> [--name N]` (admitted_by `command`), `hide <scope>`,
    `show <scope>`.
  - `why <task>` prints the binding (source, confidence, origin, reason,
    alternatives) or "bound by its project view" for `None`.
  - `conflicts [--json]`.
- `xcb doctor --upgrade-plan` previews v7 on a copy.
- `xcb models route` also prints `Workspace: <path> (<source>)` for the prompt.
  It is read-only: it uses `resolve_intake` with a fresh message id and writes
  nothing.
- **Scope parser for `<scope>` arguments.** These are `projects
  configure|pause|resume`, `memory configure|status|search`, `backlog memory`
  and `workspaces hide|show`. The value is resolved in this order:
  1. A value containing `/`, or equal to `.` or `..`, is a directory.
  2. An existing conversation id is a legacy alias for its workspace.
     `c_global` is an error.
  3. A unique `lookup_name` hit.
  4. An existing directory relative to cwd.
  5. Otherwise, an error naming the candidates.

  The order is implemented once, as `ManagedStore::resolve_scope(value: &str,
  cwd: &Path) -> Result<String>` in `managed_workspace.rs` (foundation), and
  every CLI command above, `xcb workspaces hide|show` and the TUI `[scope]`
  arguments call it.
- `backlog add|program`, `schedules add|program` and `daemons run` accept a
  conversation id or a `<scope>`. A scope means the thread plus that workspace.
  They also accept `--workspace <dir>`, which is required when the conversation
  is `c_global`; it is validated and never snapped. `xcb backlog --workspace
  <dir>` filters and conflicts with `--conversation`.
- `xcb projects --json` rows gain `workspace`, `name` and `status`, and keep
  `conversation` as a nullable field set to the latest project view for that
  workspace.

### TUI

- **`serve_ui(store, conversation, launch_hint: Option<String>, …)`** holds
  the conversation, `bound: Option<String>` (the view's workspace), the hint and
  the session-local `focus`.
- **On Enter:**
  - In a view: `submit_new`, as today.
  - In the thread: `submit_to_thread` with the focus and hint and no target.
    Enter with a composer target guides that task and creates none, so the
    TUI never reaches rung 2; the `target` cue stays for API callers.
  - On `Ask`: send `SubmitRejected` (the draft is kept) and then
    `ProjectPicker`. Picking sends `Focus(Some(ws))` (preceded by
    `AddWorkspace` for a `new` candidate) and resubmits the draft. Esc closes
    the picker and keeps the draft.
  - On bind: send `WorkspaceBound`.
- **`Schedule`, `Enqueue` and `EnqueueIn`** in the thread (deferred or not) get
  their `workspace` from the **authority ladder**, exactly like `/project`: an
  explicit argument, then the view workspace, then the focus, then the selected
  task's workspace. Otherwise the picker opens and nothing is written. They
  never call the inference ladder; the launch hint and recent rungs never apply.
  Schedules never infer at fire time.
- **`/workspace`:**
  - With no argument it opens the picker.
  - `/workspace <name|path>` does two things. If this session's last submitted
    task can still be moved, it runs `MoveTask`. It always sets `Focus`. The
    notice echoes the result.
  - A container is refused as a focus or move target ("`<dir>` holds other
    projects; /workspace add `<dir>` to use it as one") unless it was admitted
    by `command`.
  - Subcommands: `move <task> <name|path>`, `go` (`ReleaseHold`), `clear`/`all`
    (clear focus), `add <dir>` (`AddWorkspace`, admitted_by `command`).
- **Authority commands.** `/project [grant|pause|resume] [scope]` and
  `/memory search` resolve their scope at keystroke time: an explicit argument,
  then the view workspace, then the focus, then the selected task's workspace.
  Otherwise the error is "name the project: /project grant <name|dir> …". They
  carry `workspace` in `HabitatCommand`. `HabitatAt` wrapping remains only for
  `Schedule`/`Enqueue*`.
  - **Grant grammar precedence:** `/project grant [scope] <tasks> <hours>
    <goal>`. After `grant`, a first token that parses as an unsigned integer is
    `<tasks>` and there is no explicit scope; any other first token is the
    scope. A directory whose name is all digits is written as a path
    (`./2026`). The existing pinned test (lib.rs ~3365) keeps passing and a new
    test pins the precedence.
- **Header.** The thread shows `xcb · all projects · N active` or
  `xcb · → name`. Views keep their basename. Keep the name "thread" in copy;
  the existing "global conversation" render tests keep their names.
- **Footer.** `N projects` replaces `{n} chats`.
- **Transcript.** A dim workspace chip appears on task-attributed messages,
  driven by `TranscriptPage.workspaces`. `transcript::page` selects
  `messages.task` and LEFT JOINs `tasks.workspace`.
- **Grid** (`agent_overview`):
  - Today the query is driven `FROM conversations c LEFT JOIN ranked r ON
    r.conversation=c.id AND r.position=1` (managed_overview.rs ~42-78), so
    changing only the partition key would still yield one row for `c_global`.
    It becomes a **UNION**: per-conversation rows for every view (as today,
    excluding `c_global`) UNION per-workspace rows for thread tasks
    (`FROM tasks WHERE conversation='c_global'`, ranked by
    `PARTITION BY workspace`).
  - The task/conversation workspace identity check (~125-130) is skipped for
    thread tasks and uses the task's workspace.
  - `AgentRow.workspace` comes from the task.
  - Grid identity is `(context, workspace)`. `matches_filter` includes the
    workspace name and path.
  - Opening a thread card sets focus.
  - `Alt-←/→` cycles focus in the thread and cycles conversations elsewhere.
- **Inspect modal** gains a line such as `Project: xcb · continuing (medium)`,
  plus `Moved from t_…`.
- **Held tasks** show a chip `→ xcb · starts in 6s · /workspace go`.
- **`/new`** in the thread clears the focus and the composer target and creates
  no conversation. In a view it keeps today's behavior.
- **`/sessions`** lists the thread first and then `project view · <name>` rows.
  The picker entry becomes `＋ new project view` and is shown only when the
  focus or hint names a directory.
- **Cancel** in the thread: a bare cancel with several candidates opens the
  picker, whose labels include the workspace name.
- **Recovery.** An unbound recovered draft matches the thread unconditionally.

### Remote (frozen contract, additive only)

`managed_relay::dispatch` resolves the wire `workspace` like this:

1. **Absolute path.** `validate_workspace_root` gives the exact binding, with
   no snapping: `/repo/sub` stays `/repo/sub`. It is admitted as `dispatch`
   and submitted with `IntakeCues { origin: Relay, explicit }`.
2. **`@infer`.** This is a reserved sentinel. It runs `submit_to_thread` with
   `origin: Relay` and `infer_only`. Only rungs 0, 3 (registered roots only,
   no `new_roots`), 4 (only a high single-path match) and the relay form of
   rung 6 (explicit continuation cue AND a relay-origin last task) are
   allowed. Rungs 7 and 8 never apply, because a remote guess cannot be
   corrected in time. `Ask` fails as `workspace ambiguous: <names>`. Old
   daemons fail `@infer` closed at `canonicalize`.
3. **Other non-absolute values.** These are a unique `lookup_name` hit. They
   are never resolved against the supervisor's inherited cwd. Zero or several
   hits fail with the candidate names.

After resolving, dispatch submits into `global_thread()`. The result is
`{"conversation":"c_global","dispatched":true,"task":"t_…","workspace":"/abs","workspaceSource":"explicit|mention|continuation"}`.
Fleet rows gain `workspaceSource` (`explicit` when the binding is `None`). The
fleet projection body additively gains `"xcb": "<package version>"` and
`"capabilities": ["thread", "workspace-names", "infer"]`, so a controller can
gate name lookups and `@infer` per device on a mixed-version fleet (a 0.8.x
daemon resolves a relative name against its inherited cwd). The existing
`"version"` key keeps its meaning. `task_dispatch` keys and the relay kind list
are untouched. A `task_submit` wire kind is deferred.

Tightenings the controller must be told about, documented in
`docs/remote-operations.md` under "0.9.0 notes":
- `$HOME`, `/`, hidden directories under `$HOME`, `~/Library`, the state root
  and system directories are refused.
- A relative workspace value is a name lookup; gate it on
  `capabilities` containing `workspace-names`.
- `@infer` is opt-in; gate it on `capabilities` containing `infer`.
- The result's `conversation` is the device's thread.

## Delivery graph and ownership

**All lanes land on one integration branch, `claude/global-thread-20260926`,
and ship as ONE pull request.** No lane pushes, opens a PR or merges to main.

- **Integration worktree:** `~/Documents/xcb-global-thread-20260926`
  on `claude/global-thread-20260926` (based on origin/main `5756a49`).
- **Wave 1 — `foundation`** commits directly on `claude/global-thread-20260926`
  in the integration worktree. Its first commit adds this spec.
- **Wave 2 — `rekey`, `intake`, `surfaces`, `entry`**, in parallel, after
  foundation's last commit. Each lane creates its own worktree from the
  integration branch:
  `git -C ~/Documents/xcb-global-thread-20260926 worktree add -b claude/gt-<lane>-20260926 ~/Documents/xcb-gt-<lane>-20260926 claude/global-thread-20260926`,
  commits there and **does not push**.
- **Integrator** merges the four wave-2 branches into
  `claude/global-thread-20260926` (in the integration worktree), resolves
  conflicts using the ownership rules below, and makes the whole workspace
  green, including tests that a lane wrote against another lane's behavior.
- **Wave 3 — `docs`** runs after integration, on `claude/global-thread-20260926`
  in the integration worktree: docs, README/site, CHANGELOG, cross-cutting e2e
  tests, the final acceptance run and every aggregate gate.
- The integrator then pushes `claude/global-thread-20260926` and opens the one
  PR to main, enables auto-merge, and waits for `Required`.

Because there is one PR, the per-lane CI deadlock on
`qualification/inbox-acceptance.py` (it runs in the `Native build` job that
`Required` needs, ci.yml:193) disappears. Its edits still have owners, each in
a separate hunk:

| Hunk | Owner | Change |
|---|---|---|
| ~305-307 second conversation | foundation | create the second conversation with a second temp `--cwd` (e.g. `paths["workspace2"]`, passed through an optional `cwd` argument of `terminal()`/`command()`/`value()`). Watches across different workspaces are refused before and after the change, so "watch cannot cross project" stays meaningful and green. |
| ~395 `projects configure`, ~411-412 `projects --json` row lookup | rekey | configure with the workspace dir; match rows on `workspace`. |
| ~267-269 conversation count, `conversations[0]` | entry | skip `isThread` rows when counting and picking the first conversation. |
| final run and evidence | docs | run it against the release build of the integrated tree and cite the evidence path. |

Cross-lane test dependencies are expected and written anyway: entry's `@infer`
name-mention and continuation tests pass only after intake's ladder is merged,
and surfaces' move/hold tests only after intake's `move_task`/`release_hold`.
The lane lists those tests in its final report; the integrator makes them green.

Tooling for every lane: run heavy commands through
`~/.bun/bin/host-run --mode=heavy --lane=compute --label=<lane> -- <cmd>`
(`hra-host-run` does not exist on this Mac), and use
`CARGO_TARGET_DIR=$HOME/Documents/.xcb-gt-target` for everyone. Never create a
second target dir. PATH `python3` is Homebrew 3.14.

1. **Wave 1 — `foundation`.** Types (including `BindingOrigin`), schema v7 with
   the generated column, the validator and registry (nearest-stop `snap_root`,
   git-toplevel-never-container, read-only `known_workspaces`,
   `all_workspaces`, `lookup_name`, `resolve_scope`, conflict APIs,
   `view_stamp` registry fields), the global thread, the `create_habitat_task` prepare/`_tx` split and
   relaxation, storage re-key with shims, `_in` read primitives, all `ui.rs`
   types, the bounded upgrade-guard wait, replay checks, and stubs for the
   frozen intake APIs. Also the acceptance-script second `--cwd` hunk and the
   test fixtures the validator forces (including managed_relay.rs ~384-392 and
   the ~14 managed.rs tests that open the store at the workspace directory).
2. **Wave 2, in parallel:**
   - `rekey`: project, habitat, program, daemon and inbox callers; worker tools;
     inherited bindings for worker/program/daemon/schedule children;
     `xcb-cli/src/habitat.rs`; the acceptance `projects` hunks.
   - `intake`: the resolver, intake, move/hold, launch revalidation, overlap
     serialization.
   - `surfaces`: TUI, `serve_ui`, `managed_view`, overview, transcript.
   - `entry`: `main.rs`, `xcb workspaces`, relay, fleet capabilities; the
     acceptance count hunk.
3. **Wave 3 — `docs`.** README, docs, site, CHANGELOG, cross-cutting end-to-end
   tests, the final acceptance run, then the aggregate gates.

Shared file `managed.rs`: after wave 1, `surfaces` owns `serve_ui` and
`managed_view` only. `intake` owns every other region it touches: the intake
functions, `create_habitat_task` ack text, `Supervisor::tick` and `launch`,
`workspace_busy` and `continue_like`. `rekey` and `entry` do not edit
`managed.rs`. `docs` adds only its `#[path]` e2e test module line. `foundation`
already switches every project lookup inside `managed.rs` to the `_in` forms:
`create_habitat_task` `schedule_requirement` and `program_generation`, the inbox
wake, and the `launch` `project_context` and `working_memory_context` calls.
Shared file `main.rs`: entry owns it; rekey may edit only the `Backlog` listing
args and dispatch arm; surfaces may add only the `launch_hint` argument at the
one `serve_ui` call.

## Validation and evidence

Gates (run once by the docs lane on the integrated tree, then by CI on the one
PR):
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- root `bun run check`
- `cd site && bun install --frozen-lockfile && bun run check`
- `python3 -I qualification/inbox-acceptance.py target/release/xcb --evidence-dir <dir>`

Branch policy requires `Required`.

Acceptance checklist:
- A v6 store with two conversations over one workspace migrates as follows:
  identical bindings merge, distinct bindings unbind, two active grants pause
  the winner, and a single grant keeps its generation, so an in-flight proposal
  still dispatches.
- The pins move from 6 to 7. A live old supervisor blocks the upgrade without
  mutation (after the bounded wait).
- The v7 step is idempotent after a habitat reset, and after the project
  tables are dropped and recreated in the legacy shape.
- A task inserted after v7 with the old column list (an old binary) has the
  right `tasks.workspace`.
- A retry of the same message id after the registry changes yields the same
  task; a retry with different text is a Conflict.
- The thread binds tasks in two workspaces that run concurrently. A nested
  `/r` and `/r/sub` serialize.
- A path inside `/repo/.claude/worktrees/x` snaps to the worktree, and
  admitting a subdirectory never makes a git toplevel a container.
- Prompt tokens under `~/.ssh` or `~/Library` are refused; an unregistered
  prompt root asks.
- A grant for A never admits backlog or children in B from the thread.
- Two conversations over one workspace share the grant, backlog, working
  memory, binding and watches.
- Relay dispatch lands in `c_global`. An explicit `/repo/sub` is not snapped.
  `@infer` binds a named project, fails with candidates when ambiguous, and
  asks for a short prompt without a continuation cue. Home is refused.
- A thread `Schedule` never uses the launch or recent rungs.
- `move_task` refuses proposal, schedule and worker/program/daemon tasks.
- The TUI covers `/workspace` move and focus, the picker Ask flow, the held
  chip, one card per project, and `/new` creating nothing.

## Release and rollout

- **Version.** This ships as **0.9.0**: v7 cannot be downgraded and bare `xcb`
  changes behavior.
- **CHANGELOG.** `## Unreleased` gains the thread, inference and `/workspace`,
  projects = directories (including the single-grant scope change from I8),
  `xcb memory configure <dir>`, the upgrade conflict behavior, overlap
  serialization, the remote tightenings and additions (`capabilities`, the
  `conversations --json` `isThread`/null workspace), and "schema v7 cannot be
  downgraded".
- **Version PR.** After the one feature PR merges, a follow-up version PR
  mirrors 49a5af9 (8 files). Its summary covers both the #223 accounts work and
  this release. Push an **annotated** `v0.9.0` tag, then the publication PR.
- **Each laptop, less-used one first:**
  1. `xcb doctor --upgrade-plan` with the new binary.
  2. Let work settle.
  3. Quit every xcb TUI session on the laptop, then
     `XCB_VERSION=0.9.0 sh scripts/install-native.sh`. The old supervisor
     drains and the first new open migrates under the guard, after the backup.
  4. `xcb workspaces conflicts`, then `xcb projects`, resuming any grant that is
     "paused by upgrade" deliberately.
  5. `xcb memory status <dir>`.
  6. `xcb workspaces hide` stale worktrees.
- **Rollback.** Stop the supervisor, restore `managed.pre-v7.*.sqlite` and
  reinstall 0.8.13. Activity recorded after the upgrade is lost.
- **Grok.** No controller change is required. It must read the new result keys
  only when they are present. `@infer` and name lookups are opt-in and gated on
  the fleet `capabilities`.

Deferred: the judge and Reflex workspace tiers, the `task_submit` wire kind,
per-task worktree creation, focus shared across laptops, transcript filtering
by focus, and pruning the `*_v6` tables.

## Review notes

Decisions taken while folding in the design-panel review, where this revision
goes beyond or differs from the critic's wording:

- **Generated column detection.** The repository has no `column_exists` helper
  today, and `PRAGMA table_info` omits generated columns, so shape detection
  must use `pragma_table_xinfo`; otherwise a re-run after a version reset would
  try to add the column twice.
- **Thread enqueues use the authority ladder whether deferred or not.** The
  critic scoped this to `Schedule` and non-deferred `Enqueue`. A deferred
  backlog item is released unattended by `tick_projects` when its workspace
  holds a grant, so a guessed workspace there is also standing authority.
- **Override hold covers prompt paths too.** The critic's "mention" fix is
  applied to rung 3 (source `mention`) as well as rung 4: a pasted stack trace
  naming a registered repo other than the focused one is held, not run.
- **Containers admitted by `command`.** "Refuse containers in `/workspace` and
  focus unless explicit add" is realized with a read-time `explicit_add` flag
  (`admitted_by='command'`); such an entry is usable as focus or target but is
  still never inferred.
- **Relay-origin continuation needs a persisted origin.** Tasks carried no
  origin marker (relay dispatch uses the operation id as the message id), so
  `WorkspaceBinding` gains a required `origin`. It also makes `xcb workspaces
  why` and `move_task`'s user-origin restriction checkable.
- **Hidden home directories.** Rejecting every `$HOME/.<name>` tree also
  refuses agent-tool worktrees kept there (for example `~/.codex/worktrees`),
  even for explicit relay or CLI dispatch. That is accepted: such checkouts
  should be used from a regular directory.
- **`--new` is unchanged.** The review found that the earlier "open latest view"
  meaning would break `inbox-acceptance.py` (~305-306) and user habits; `--new`
  keeps always creating a new project view.
- **`xcb conversations` never creates the thread**, so the acceptance count at
  ~267-269 changes only if something else created it; entry still owns that
  hunk and filters on `isThread`.

## Delivered differences

The integrated branch matches this spec except for these points, which the
body above now states:

- **Daemon child reason.** A daemon child's inherited binding reads
  `from daemon <process name>`, not `from t_…`; a daemon child has no parent
  task id to name.
- **Continuation reason.** A rung 6 binding's reason reads
  ``continuing in `<name>` ``. The other reasons are `named directory`
  (explicit), `addressed task` (target), ``path in `<name>` `` (prompt path),
  ``named `<word>` `` (name mention, plus `, most recent checkout` for a low
  worktree pick), `focus`, `launch dir` and `most recent project`, with the
  suffix `(overrides focus <name>)` when a path or name beats the focus.
- **No TUI target cue.** `serve_ui` passes `IntakeCues.target = None` for
  thread submissions; guidance to a selected task is a steer, not a new task.
- **`move_task` to the same workspace** is a no-op, not a Conflict.
- **Picker Esc keeps the draft**, like every other picker; nothing is written.
- **Launch revalidation of legacy tasks.** A pre-v7 task whose stored
  workspace is not the validator's canonical string fails at launch with
  "workspace moved or was replaced since it was bound" instead of running.
- **Launch admission of containers.** A `launch` admission whose root would be
  a container fails with "workspace is not allowed: it holds other projects",
  and bare `xcb` then opens the thread with no hint.
- **Relay name misses** list up to eight known project names.
- **Test gap.** No `kernel::workspace_lease` unit test uses nested paths; the
  overlap rule is covered at the supervisor tick and `workspace_busy` level and
  by the cross-cutting e2e test in `managed_global_thread_e2e_tests.rs`.
