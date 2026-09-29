//! Read provider transcripts as context. An import never owns a provider
//! session, account, process, task, or workspace execution grant.

use super::*;
use std::{
    collections::VecDeque,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    time::UNIX_EPOCH,
};
#[cfg(unix)]
use std::{os::unix::fs::MetadataExt, path::Component};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const MAX_ENTRIES: usize = 32_768;
const MAX_FILES: usize = 256;
const MAX_SCAN_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
const HEAD_BYTES: usize = 512 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_IMPORT_MESSAGES: usize = 256;
const MAX_IMPORT_BYTES: usize = 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_BYTES: usize = 64 * 1024;
const FUTURE_TOLERANCE_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Serialize)]
pub struct DiscoveredSession {
    /// Stable across imports and provider file moves; also the project view id.
    pub id: Id,
    pub provider: Provider,
    pub source: PathBuf,
    pub workspace: String,
    pub title: String,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
    pub message_count: usize,
    /// Older or overlong text was omitted at an import limit.
    pub truncated: bool,
    #[serde(skip)]
    messages: Vec<Message>,
}

#[derive(Debug, Serialize)]
pub struct Discovery {
    pub version: u32,
    pub since_ms: u64,
    pub sessions: Vec<DiscoveredSession>,
    pub skipped_files: usize,
    pub read_bytes: usize,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub conversation: Id,
    pub source_workspace: String,
    pub workspace: String,
    pub created: bool,
    pub added_messages: usize,
    pub unchanged_messages: usize,
    pub ignored_older_messages: usize,
}

/// Sources are explicit here so tests and embedding hosts never need to read
/// another user's home. The CLI only uses the provider's normal state roots.
pub struct Sources {
    pub codex: PathBuf,
    pub claude: PathBuf,
}
impl Sources {
    pub fn from_environment() -> Result<Self> {
        let home = std::env::var_os("HOME").ok_or(Error::PrivateState)?;
        let home = PathBuf::from(home);
        Ok(Self {
            codex: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("sessions"),
            claude: std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude"))
                .join("projects"),
        })
    }
}

struct SourceFile {
    provider: Provider,
    path: PathBuf,
    modified: u64,
}

/// Discover recent main-session text without changing either provider's state.
/// A recent timestamp is a selection heuristic, never evidence of liveness.
pub fn discover(
    sources: &Sources,
    provider: Option<Provider>,
    hours: u16,
    now: u64,
) -> Result<Discovery> {
    if !(1..=8760).contains(&hours) || provider == Some(Provider::Devin) {
        return Err(xcb_core::Error::Invalid("session discovery options").into());
    }
    if cfg!(windows) {
        return Err(Error::providers_unsupported());
    }
    let since_ms = now.saturating_sub(u64::from(hours) * 3_600_000);
    let mut report = Discovery {
        version: 1,
        since_ms,
        sessions: Vec::new(),
        skipped_files: 0,
        read_bytes: 0,
        truncated: false,
    };
    let mut files = Vec::new();
    let mut entries = 0;
    for (kind, root, depth) in [
        (Provider::Codex, &sources.codex, 3),
        (Provider::Claude, &sources.claude, 1),
    ] {
        if provider.is_none_or(|provider| provider == kind) {
            collect_files(kind, root, depth, &mut entries, &mut files, &mut report);
        }
    }
    files.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
    // Provider writes advance mtime, so old files need no transcript read.
    // After reading, event timestamps decide recency; touching an old log
    // cannot make old conversation text recent.
    let files: Vec<_> = files
        .into_iter()
        .filter(|file| file.modified >= since_ms)
        .collect();
    let mut ids = BTreeSet::new();
    let mut main_files = 0;
    for source in files {
        if report.read_bytes >= MAX_SCAN_BYTES || main_files >= MAX_FILES {
            report.truncated = true;
            break;
        }
        let budget = (MAX_SCAN_BYTES - report.read_bytes).min(MAX_FILE_BYTES);
        match read_session(&source, budget, now) {
            Ok((Some(session), bytes, _)) => {
                main_files += 1;
                report.read_bytes += bytes;
                if session.last_active_at_ms >= since_ms && ids.insert(session.id.clone()) {
                    report.sessions.push(session);
                }
            }
            Ok((None, bytes, excluded_subagent)) => {
                main_files += usize::from(!excluded_subagent);
                report.read_bytes += bytes;
                report.skipped_files += 1;
            }
            Err(_) => {
                // A file can disappear or be replaced while its provider is
                // running. One bad file must not hide the remaining sessions.
                report.read_bytes += budget;
                report.skipped_files += 1;
                main_files += 1;
            }
        }
    }
    report.sessions.sort_by(|a, b| {
        b.last_active_at_ms
            .cmp(&a.last_active_at_ms)
            .then(a.id.cmp(&b.id))
    });
    Ok(report)
}

/// Provider transcripts are read only where providers run: Windows builds
/// refuse discovery and import before any source is opened.
#[cfg(windows)]
fn owned_readable(_metadata: &fs::Metadata, _file: bool) -> bool {
    false
}

#[cfg(unix)]
fn owned_readable(metadata: &fs::Metadata, file: bool) -> bool {
    metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o022 == 0
        && if file {
            metadata.is_file() && metadata.nlink() == 1
        } else {
            metadata.is_dir()
        }
}

fn collect_files(
    provider: Provider,
    root: &Path,
    depth: usize,
    entries: &mut usize,
    files: &mut Vec<SourceFile>,
    report: &mut Discovery,
) {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    if !owned_readable(&metadata, false) || xcb_core::canonical(root).ok().as_deref() != Some(root)
    {
        report.skipped_files += 1;
        return;
    }
    let Ok(directory) = fs::read_dir(root) else {
        report.skipped_files += 1;
        return;
    };
    for entry in directory {
        if *entries >= MAX_ENTRIES {
            report.truncated = true;
            return;
        }
        *entries += 1;
        let Ok(entry) = entry else {
            report.skipped_files += 1;
            continue;
        };
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            report.skipped_files += 1;
            continue;
        };
        if metadata.file_type().is_symlink() {
            report.skipped_files += 1;
            continue;
        }
        if metadata.is_dir() && depth > 0 {
            // Codex's date hierarchy; Claude's one project level. Nested
            // Claude subagents and arbitrary provider-state folders stay out.
            if provider == Provider::Claude
                || entry.file_name().to_str().is_some_and(|name| {
                    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                collect_files(provider, &path, depth - 1, entries, files, report);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            if !owned_readable(&metadata, true) {
                report.skipped_files += 1;
                continue;
            }
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|time| u64::try_from(time.as_millis()).ok())
                .unwrap_or(0);
            files.push(SourceFile {
                provider,
                path,
                modified,
            });
        }
    }
}

/// Open every path component relative to the previous directory descriptor.
/// NOFOLLOW on just the leaf would still follow a replaced parent directory.
#[cfg(windows)]
fn open_source(_path: &Path) -> Result<File> {
    Err(Error::providers_unsupported())
}

#[cfg(unix)]
fn open_source(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    if !path.is_absolute() {
        return Err(Error::PrivateState);
    }
    let dir_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut fd = open("/", dir_flags, Mode::empty()).map_err(std::io::Error::from)?;
    let parts: Vec<_> = path.components().skip(1).collect();
    if parts.is_empty() {
        return Err(Error::PrivateState);
    }
    for (index, component) in parts.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(Error::PrivateState);
        };
        let flags = if index + 1 == parts.len() {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
        } else {
            dir_flags
        };
        fd = openat(&fd, *name, flags, Mode::empty()).map_err(std::io::Error::from)?;
    }
    let file = File::from(fd);
    if !owned_readable(&file.metadata()?, true) {
        return Err(Error::PrivateState);
    }
    Ok(file)
}

fn timestamp(value: &Value, now: u64) -> Option<u64> {
    let at = if let Some(value) = value.as_u64() {
        value
    } else {
        let value = OffsetDateTime::parse(value.as_str()?, &Rfc3339).ok()?;
        u64::try_from(value.unix_timestamp_nanos() / 1_000_000).ok()?
    };
    (at > 0 && at <= now.saturating_add(FUTURE_TOLERANCE_MS)).then_some(at.min(now))
}

fn text_content(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return xcb_core::display_text(text, MAX_MESSAGE_BYTES);
    }
    let mut text = String::new();
    if let Some(parts) = value.as_array() {
        for part in parts {
            if matches!(
                part.get("type").and_then(Value::as_str),
                Some("text" | "input_text" | "output_text")
            ) && let Some(value) = part.get("text").and_then(Value::as_str)
            {
                if !text.is_empty() && text.len() < MAX_MESSAGE_BYTES {
                    text.push('\n');
                }
                text.push_str(&xcb_core::display_text(
                    value,
                    MAX_MESSAGE_BYTES.saturating_sub(text.len()),
                ));
            }
        }
    }
    text
}

fn codex_subagent(payload: &Value) -> bool {
    ["source", "thread_source"].iter().any(|key| {
        payload
            .get(key)
            .is_some_and(|source| source.to_string().to_ascii_lowercase().contains("subagent"))
    })
}

fn read_session(
    source: &SourceFile,
    budget: usize,
    now: u64,
) -> Result<(Option<DiscoveredSession>, usize, bool)> {
    let mut file = open_source(&source.path)?;
    let len = file.metadata()?.len();
    // The newest files are often hundreds of Codex worker/reviewer logs.
    // Read only their first metadata record before spending a main-session
    // slot or copying a body. Partial/unknown metadata takes the normal path.
    let mut preflight_bytes = 0;
    if source.provider == Provider::Codex {
        let limit = budget.min(HEAD_BYTES);
        let mut limited = (&mut file).take(limit as u64);
        let mut line = Vec::new();
        {
            let mut reader = BufReader::new(&mut limited);
            reader.read_until(b'\n', &mut line)?;
        }
        preflight_bytes = limit - limited.limit() as usize;
        if line.ends_with(b"\n")
            && let Ok(value) = serde_json::from_slice::<Value>(&line)
            && value.get("type").and_then(Value::as_str) == Some("session_meta")
            && value.get("payload").is_some_and(codex_subagent)
        {
            return Ok((None, preflight_bytes, true));
        }
        file.seek(SeekFrom::Start(0))?;
    }
    let budget = budget.saturating_sub(preflight_bytes);
    if budget == 0 {
        return Ok((None, preflight_bytes, false));
    }
    let mut chunks = Vec::new();
    let truncated_file = len > budget as u64;
    let head = if truncated_file {
        budget.min(HEAD_BYTES)
    } else {
        usize::try_from(len).unwrap_or(budget).min(budget)
    };
    let mut bytes = Vec::new();
    (&mut file).take(head as u64).read_to_end(&mut bytes)?;
    chunks.push((0u64, bytes));
    if truncated_file && budget > head {
        let offset = len.saturating_sub((budget - head) as u64);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        (&mut file)
            .take((budget - head) as u64)
            .read_to_end(&mut bytes)?;
        chunks.push((offset, bytes));
    }
    let read_bytes = preflight_bytes + chunks.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
    let mut external_id = None;
    let mut workspace = None;
    let mut last = None;
    let mut first = None;
    let mut subagent = false;
    let mut truncated = truncated_file;
    let mut messages = VecDeque::new();
    let mut message_bytes = 0usize;
    for (base, bytes) in chunks {
        let mut offset = 0usize;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let line_offset = base + offset as u64;
            let first_tail_line = base > 0 && offset == 0;
            offset += line.len();
            // A provider may currently be appending the final JSON line.
            if first_tail_line || !line.ends_with(b"\n") {
                continue;
            }
            if line.len() > MAX_LINE_BYTES {
                truncated = true;
                continue;
            }
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                truncated = true;
                continue;
            };
            let event = value.get("type").and_then(Value::as_str).unwrap_or("");
            let payload = value.get("payload").unwrap_or(&Value::Null);
            let at = value
                .get("timestamp")
                .and_then(|value| timestamp(value, now));
            if value.get("timestamp").is_some() && at.is_none() {
                continue;
            }
            if let Some(at) = at {
                first = Some(first.map_or(at, |first: u64| first.min(at)));
                last = Some(last.map_or(at, |last: u64| last.max(at)));
            }
            let message = match source.provider {
                Provider::Codex => {
                    if event == "session_meta" {
                        external_id = payload
                            .get("id")
                            .or_else(|| payload.get("session_id"))
                            .and_then(Value::as_str)
                            .and_then(|id| Id::new(id).ok());
                        workspace = payload
                            .get("cwd")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        subagent |= codex_subagent(payload);
                    }
                    if event != "response_item"
                        || payload.get("type").and_then(Value::as_str) != Some("message")
                    {
                        continue;
                    }
                    payload
                }
                Provider::Claude => {
                    subagent |= value.get("isSidechain").and_then(Value::as_bool) == Some(true);
                    if external_id.is_none() {
                        external_id = value
                            .get("sessionId")
                            .and_then(Value::as_str)
                            .and_then(|id| Id::new(id).ok());
                    }
                    if workspace.is_none() {
                        workspace = value.get("cwd").and_then(Value::as_str).map(str::to_owned);
                    }
                    if !matches!(event, "user" | "assistant") {
                        continue;
                    }
                    value.get("message").unwrap_or(&Value::Null)
                }
                Provider::Devin => unreachable!("discovery only selects transcript providers"),
            };
            let role = match message.get("role").and_then(Value::as_str) {
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                _ => continue,
            };
            let text = text_content(message.get("content").unwrap_or(&Value::Null));
            if text.trim().is_empty() {
                continue;
            }
            truncated |= text.len() >= MAX_MESSAGE_BYTES;
            let at = at.or_else(|| timestamp(&json!(source.modified), now));
            let Some(at) = at else { continue };
            message_bytes += text.len();
            messages.push_back((line_offset, role, text, at));
            while messages.len() > MAX_IMPORT_MESSAGES || message_bytes > MAX_IMPORT_BYTES {
                if let Some((_, _, text, _)) = messages.pop_front() {
                    message_bytes -= text.len();
                    truncated = true;
                }
            }
        }
    }
    let (Some(external_id), Some(workspace)) = (external_id, workspace) else {
        return Ok((None, read_bytes, subagent));
    };
    if subagent || messages.is_empty() || !Path::new(&workspace).is_absolute() {
        return Ok((None, read_bytes, subagent));
    }
    let Ok(canonical) = xcb_core::canonical(&workspace) else {
        return Ok((None, read_bytes, false));
    };
    let Some(workspace) = canonical.to_str().filter(|_| canonical.is_dir()) else {
        return Ok((None, read_bytes, false));
    };
    if !xcb_core::bounded_path(workspace) {
        return Ok((None, read_bytes, false));
    }
    let Some(last) = last.or_else(|| timestamp(&json!(source.modified), now)) else {
        return Ok((None, read_bytes, false));
    };
    let key = format!("xcb-import-v1\0{}\0{external_id}", source.provider);
    let id = Id::new(format!("c_import_{}", digest(&key)))?;
    let messages: Vec<_> = messages
        .into_iter()
        .map(|(offset, role, text, at_ms)| {
            Ok(Message {
                id: Id::new(format!(
                    "m_import_{offset:020}_{}",
                    digest(format!("{key}\0{offset}\0{role:?}\0{text}"))
                ))?,
                role,
                text,
                at_ms,
                attachments: vec![],
                provenance: None,
            })
        })
        .collect::<Result<_>>()?;
    let project = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let title = xcb_core::display_text(
        &format!(
            "{} · {project} · {}",
            source.provider,
            &external_id.as_str()[..external_id.as_str().len().min(8)]
        ),
        160,
    );
    Ok((
        Some(DiscoveredSession {
            id,
            provider: source.provider,
            source: source.path.clone(),
            workspace: workspace.to_owned(),
            title,
            created_at_ms: first.unwrap_or(last).min(last),
            last_active_at_ms: last,
            message_count: messages.len(),
            truncated,
            messages,
        }),
        read_bytes,
        false,
    ))
}

impl ManagedStore {
    /// Append a read-only provider snapshot to its own project view. Stable
    /// ids and one transaction make repeat and concurrent imports converge.
    pub async fn import_session(&self, source: &DiscoveredSession) -> Result<ImportResult> {
        self.import_session_into(source, None).await
    }

    /// An explicitly selected project can receive history from a session
    /// started at home. It must pass the same checks as every project view.
    /// Once imported, the conversation's project binding cannot be changed.
    pub async fn import_session_into(
        &self,
        source: &DiscoveredSession,
        workspace: Option<&Path>,
    ) -> Result<ImportResult> {
        if !source
            .id
            .as_str()
            .strip_prefix("c_import_")
            .is_some_and(xcb_core::hex64)
        {
            return Err(xcb_core::Error::Invalid("imported session identity").into());
        }
        let existing_workspace = if workspace.is_none() {
            self.conversation(&source.id)?
                .and_then(|conversation| conversation.workspace)
        } else {
            None
        };
        let workspace = self.validate_workspace(workspace.unwrap_or_else(|| {
            Path::new(existing_workspace.as_deref().unwrap_or(&source.workspace))
        }))?;
        let conversation = ManagedConversation {
            version: 1,
            id: source.id.clone(),
            title: source.title.clone(),
            workspace: Some(workspace.clone()),
            created_at_ms: source.created_at_ms,
            updated_at_ms: source.last_active_at_ms,
        };
        conversation.validate()?;
        let (_, receipt, receipt_json) = Self::algal_receipt(&conversation).await?;
        let mut db = self.write_db()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT payload FROM conversations WHERE id=?1",
                [source.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let created = existing.is_none();
        if let Some(existing) = existing {
            let existing: ManagedConversation = decode(&existing)?;
            existing.validate()?;
            if existing.workspace != conversation.workspace {
                return Err(Error::Conflict("imported session workspace changed"));
            }
        } else {
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM conversations WHERE id<>?1",
                [GLOBAL_THREAD_ID],
                |row| row.get(0),
            )?;
            if count >= MAX_CONVERSATIONS {
                return Err(xcb_core::Error::Limit("managed conversations").into());
            }
            tx.execute(
                "INSERT INTO conversations(id,updated_at,payload) VALUES(?1,?2,?3)",
                params![
                    source.id.as_str(),
                    sql(source.last_active_at_ms)?,
                    serde_json::to_string(&conversation)?
                ],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO receipts(digest,task,revision,payload) VALUES(?1,NULL,?2,?3)",
                params![receipt, sql(source.last_active_at_ms)?, receipt_json],
            )?;
            let marker = Message {
                id: Id::new(format!("m_import_notice_{}", digest(source.id.as_str())))?,
                role: Role::System,
                text: format!(
                    "Imported {} conversation text.\nSource: {}\nOriginal workspace: {}\nOriginal files are unchanged. This is saved context; it does not take over the provider session or establish whether its process is running. Send a new message to start work through xcb.",
                    source.provider,
                    source.source.display(),
                    source.workspace
                ),
                at_ms: source.created_at_ms,
                attachments: vec![],
                provenance: None,
            };
            Self::append_message_tx(&tx, &marker, &source.id, None)?;
        }
        let mut added_messages = 0;
        let mut unchanged_messages = 0;
        let mut ignored_older_messages = 0;
        let highest_source_id: Option<String> = tx.query_row(
            "SELECT id FROM messages WHERE conversation=?1 AND id GLOB 'm_import_[0-9]*' AND task IS NULL ORDER BY id DESC LIMIT 1",
            [source.id.as_str()],
            |row| row.get(0),
        ).optional()?;
        for message in &source.messages {
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT conversation,payload FROM messages WHERE id=?1",
                    [message.id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((owner, payload)) = prior {
                let prior: Message = decode(&payload)?;
                if owner != source.id.as_str()
                    || prior.role != message.role
                    || prior.text != message.text
                {
                    return Err(Error::Conflict("imported message identity changed"));
                }
                unchanged_messages += 1;
            } else if highest_source_id
                .as_ref()
                .is_some_and(|highest| message.id.as_str().get(..29) <= highest.get(..29))
            {
                // A concurrent older snapshot can arrive after a newer tail.
                // Do not append its dropped old text as the latest history.
                ignored_older_messages += 1;
            } else {
                Self::append_message_tx(&tx, message, &source.id, None)?;
                added_messages += 1;
            }
        }
        tx.commit()?;
        Ok(ImportResult {
            conversation: source.id.clone(),
            source_workspace: source.workspace.clone(),
            workspace,
            created,
            added_messages,
            unchanged_messages,
            ignored_older_messages,
        })
    }

    /// Freeze context before the new user's message. Later imports cannot
    /// silently change the task, and transcript instructions remain quoted
    /// history rather than a new request or workspace permission.
    pub(super) fn imported_session_context(
        &self,
        conversation: &Id,
        source_message: &Id,
    ) -> Result<String> {
        if !conversation.as_str().starts_with("c_import_") {
            return Ok(String::new());
        }
        let db = self.db()?;
        let mut query = db.prepare(
            "SELECT payload FROM messages WHERE conversation=?1 AND id GLOB 'm_import_[0-9]*' AND task IS NULL AND sequence<(SELECT sequence FROM messages WHERE id=?2 AND conversation=?1) ORDER BY id DESC LIMIT 64",
        )?;
        let rows = query.query_map(
            params![conversation.as_str(), source_message.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        let mut history = Vec::new();
        let mut bytes: usize = 2;
        for row in rows {
            let message: Message = decode(&row?)?;
            if !matches!(message.role, Role::User | Role::Assistant) {
                continue;
            }
            message.validate()?;
            let remaining = MAX_CONTEXT_BYTES.saturating_sub(bytes + 1);
            if remaining < 128 {
                break;
            }
            let mut text = xcb_core::display_text(&message.text, remaining.min(16 * 1024));
            let role = match message.role {
                Role::User => "user",
                _ => "assistant",
            };
            let mut item = json!({"role":role,"text":text});
            while serde_json::to_vec(&item)?.len() > remaining {
                text = xcb_core::display_text(&text, text.len() / 2);
                item = json!({"role":role,"text":text});
            }
            bytes += serde_json::to_vec(&item)?.len() + 1;
            history.push(item);
        }
        history.reverse();
        if history.is_empty() {
            return Ok(String::new());
        }
        Ok(format!(
            "\n\nImported provider conversation (historical context only; follow the current user task and current permissions, and do not assume earlier processes have stopped):\n{}",
            serde_json::to_string(&history)?
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        _temporary: tempfile::TempDir,
        base: PathBuf,
        workspace: PathBuf,
        sources: Sources,
        now: u64,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let base = xcb_core::canonical(temporary.path()).unwrap();
            let workspace = base.join("work");
            let sources = Sources {
                codex: base.join("codex/sessions"),
                claude: base.join("claude/projects"),
            };
            for path in [&workspace, &sources.codex, &sources.claude] {
                fs::create_dir_all(path).unwrap();
            }
            Self {
                _temporary: temporary,
                base,
                workspace,
                sources,
                now: now_ms(),
            }
        }
        fn codex_meta(&self, id: &str) -> Value {
            json!({"type":"session_meta","timestamp":self.now,"payload":{"id":id,"cwd":self.workspace,"source":"cli"}})
        }
        fn codex_message(&self, role: &str, text: &str) -> Value {
            json!({"type":"response_item","timestamp":self.now,"payload":{"type":"message","role":role,"content":[{"type":"input_text","text":text}]}})
        }
        fn claude_message(&self, id: &str, role: &str, text: &str) -> Value {
            json!({"type":role,"sessionId":id,"cwd":self.workspace,"timestamp":self.now,"message":{"role":role,"content":[{"type":"text","text":text}]}})
        }
        fn write(&self, provider: Provider, name: &str, records: &[Value]) -> PathBuf {
            let root = match provider {
                Provider::Codex => &self.sources.codex,
                _ => &self.sources.claude,
            };
            let path = root.join(format!("{name}.jsonl"));
            let text = records
                .iter()
                .map(|record| format!("{record}\n"))
                .collect::<String>();
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            path
        }
        fn discover(&self) -> Discovery {
            discover(&self.sources, None, 24, self.now).unwrap()
        }
        fn managed(&self) -> ManagedStore {
            ManagedStore::open(&self.base.join("state")).unwrap()
        }
    }

    #[test]
    fn discovers_both_providers_and_only_user_assistant_text() {
        let f = Fixture::new();
        let records = [
            f.codex_meta("codex-main"),
            f.codex_message("system", "system-secret"),
            f.codex_message("developer", "developer-secret"),
            f.codex_message("user", "please inspect this"),
            json!({"type":"response_item","timestamp":f.now,"payload":{"type":"function_call_output","output":"tool-secret"}}),
            f.codex_message("assistant", "the code is ready"),
        ];
        f.write(Provider::Codex, "main", &records);
        f.write(Provider::Claude, "main", &[
            f.claude_message("claude-main", "user", "continue the change"),
            json!({"type":"user","sessionId":"claude-main","cwd":f.workspace,"timestamp":f.now,"message":{"role":"user","content":[{"type":"tool_result","content":"tool-secret"},{"type":"image","source":{"data":"image-secret"}}]}}),
            f.claude_message("claude-main", "assistant", "next step"),
        ]);
        let report = f.discover();
        assert_eq!(report.sessions.len(), 2);
        for session in &report.sessions {
            assert_eq!(session.message_count, 2);
            assert!(
                session
                    .messages
                    .iter()
                    .all(|message| !message.text.contains("secret"))
            );
        }
        let public = serde_json::to_string(&report).unwrap();
        assert!(!public.contains("please inspect"));
        assert!(!public.contains("next step"));
        assert_eq!(
            discover(&f.sources, Some(Provider::Claude), 24, f.now)
                .unwrap()
                .sessions
                .len(),
            1
        );
        assert!(discover(&f.sources, Some(Provider::Devin), 24, f.now).is_err());
        assert!(discover(&f.sources, None, 0, f.now).is_err());
    }

    #[test]
    fn cutoff_uses_events_instead_of_touched_files_and_rejects_future_activity() {
        let f = Fixture::new();
        for (id, at) in [
            ("old", f.now - 25 * 3_600_000),
            ("boundary", f.now - 24 * 3_600_000),
            ("future", f.now + FUTURE_TOLERANCE_MS + 1),
        ] {
            let mut meta = f.codex_meta(id);
            meta["timestamp"] = json!(at);
            let mut message = f.codex_message("user", "test");
            message["timestamp"] = json!(at);
            f.write(Provider::Codex, id, &[meta, message]);
        }
        let report = f.discover();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].last_active_at_ms, report.since_ms);
        assert_eq!(
            discover(&f.sources, None, 26, f.now)
                .unwrap()
                .sessions
                .len(),
            2
        );
        assert!(timestamp(&json!("2026-09-28T12:00:00Z"), u64::MAX).is_some());
        assert!(timestamp(&json!("not a date"), f.now).is_none());
    }

    #[test]
    fn excludes_subagents_symlinks_hardlinks_and_writable_files() {
        let f = Fixture::new();
        let mut child = f.codex_meta("child");
        child["payload"]["source"] = json!({"subagent":{"parent_thread_id":"parent"}});
        f.write(
            Provider::Codex,
            "child",
            &[child, f.codex_message("user", "child text")],
        );
        let mut sidechain = f.claude_message("child", "user", "sidechain text");
        sidechain["isSidechain"] = json!(true);
        f.write(Provider::Claude, "child", &[sidechain]);
        let good = f.write(
            Provider::Codex,
            "good",
            &[f.codex_meta("main"), f.codex_message("user", "main text")],
        );
        symlink(&good, f.sources.codex.join("linked.jsonl")).unwrap();
        let writable = f.write(
            Provider::Codex,
            "writable",
            &[f.codex_meta("writable"), f.codex_message("user", "bad")],
        );
        fs::set_permissions(writable, fs::Permissions::from_mode(0o666)).unwrap();
        let hardlink = f.write(
            Provider::Codex,
            "hard",
            &[f.codex_meta("hard"), f.codex_message("user", "bad")],
        );
        fs::hard_link(&hardlink, f.sources.codex.join("hard-copy.jsonl")).unwrap();
        let report = f.discover();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.skipped_files >= 5);
        assert!(open_source(&f.sources.codex.join("linked.jsonl")).is_err());
        let linked_root = f.base.join("linked-root");
        symlink(&f.sources.codex, &linked_root).unwrap();
        assert!(open_source(&linked_root.join("good.jsonl")).is_err());
    }

    #[tokio::test]
    async fn repeat_import_is_idempotent_preserves_sources_and_starts_nothing() {
        let f = Fixture::new();
        let path = f.write(
            Provider::Codex,
            "main",
            &[
                f.codex_meta("main"),
                f.codex_message("user", "request"),
                f.codex_message("assistant", "answer"),
            ],
        );
        let original = fs::read(&path).unwrap();
        let report = f.discover();
        let managed = f.managed();
        let first = managed.import_session(&report.sessions[0]).await.unwrap();
        assert!(first.created);
        assert_eq!(first.added_messages, 2);
        let repeat = managed.import_session(&report.sessions[0]).await.unwrap();
        assert!(!repeat.created);
        assert_eq!(repeat.added_messages, 0);
        assert_eq!(repeat.unchanged_messages, 2);
        assert_eq!(managed.conversations(10).unwrap().len(), 1);
        assert_eq!(managed.messages(&first.conversation, 10).unwrap().len(), 3);
        assert!(managed.active_tasks(128).unwrap().is_empty());
        assert_eq!(
            managed
                .db()
                .unwrap()
                .query_row::<i64, _, _>("SELECT count(*) FROM tasks", [], |row| row.get(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        let store = Store::open(&f.base.join("state")).unwrap();
        assert!(store.accounts().unwrap().is_empty());
        assert!(store.sessions(10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn explicit_project_keeps_source_metadata_and_cannot_rebind_an_import() {
        let f = Fixture::new();
        let mut meta = f.codex_meta("home-started");
        meta["payload"]["cwd"] = json!(xcb_core::home_dir().unwrap());
        let source_path = f.write(
            Provider::Codex,
            "home",
            &[meta, f.codex_message("user", "request")],
        );
        let before = fs::read(&source_path).unwrap();
        let report = f.discover();
        let source = &report.sessions[0];
        let managed = f.managed();
        assert!(managed.import_session(source).await.is_err());
        assert!(managed.conversations(10).unwrap().is_empty());
        let imported = managed
            .import_session_into(source, Some(&f.workspace))
            .await
            .unwrap();
        assert_eq!(
            Path::new(&imported.source_workspace),
            xcb_core::home_dir().unwrap()
        );
        assert_eq!(imported.workspace, f.workspace.to_str().unwrap());
        let history = managed.messages(&imported.conversation, 10).unwrap();
        assert!(history[0].text.contains(&format!(
            "Original workspace: {}",
            imported.source_workspace
        )));
        assert!(history[0].text.contains(source_path.to_str().unwrap()));
        assert_eq!(
            managed
                .import_session_into(source, Some(&f.workspace))
                .await
                .unwrap()
                .added_messages,
            0
        );
        assert_eq!(
            managed.import_session(source).await.unwrap().workspace,
            imported.workspace
        );
        let other = f.base.join("other-project");
        fs::create_dir(&other).unwrap();
        assert!(
            managed
                .import_session_into(source, Some(&other))
                .await
                .is_err()
        );
        assert!(
            managed
                .import_session_into(source, Some(&f.base.join("state")))
                .await
                .is_err()
        );
        assert!(managed.active_tasks(128).unwrap().is_empty());
        assert_eq!(fs::read(&source_path).unwrap(), before);
    }

    #[tokio::test]
    async fn active_partial_line_is_ignored_until_complete_and_append_keeps_ids() {
        let f = Fixture::new();
        let path = f.write(
            Provider::Claude,
            "main",
            &[f.claude_message("main", "user", "first")],
        );
        let next = format!("{}\n", f.claude_message("main", "assistant", "later"));
        let split = next.len() / 2;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&next.as_bytes()[..split]).unwrap();
        let report = f.discover();
        assert_eq!(report.sessions[0].message_count, 1);
        let managed = f.managed();
        let first = managed.import_session(&report.sessions[0]).await.unwrap();
        file.write_all(&next.as_bytes()[split..]).unwrap();
        let report = f.discover();
        let second = managed.import_session(&report.sessions[0]).await.unwrap();
        assert_eq!(first.conversation, second.conversation);
        assert_eq!(second.added_messages, 1);
        assert_eq!(second.unchanged_messages, 1);
        assert_eq!(managed.messages(&first.conversation, 10).unwrap().len(), 3);
    }

    #[tokio::test]
    async fn concurrent_older_snapshot_does_not_reorder_or_duplicate_the_newer_tail() {
        let f = Fixture::new();
        f.write(
            Provider::Codex,
            "main",
            &[
                f.codex_meta("main"),
                f.codex_message("user", "oldest"),
                f.codex_message("assistant", "middle"),
                f.codex_message("user", "newest"),
            ],
        );
        let mut old = f.discover();
        old.sessions[0].messages.pop();
        let mut new = f.discover();
        new.sessions[0].messages.remove(0);
        let managed = f.managed();
        let first = managed.import_session(&new.sessions[0]).await.unwrap();
        let stale = managed.import_session(&old.sessions[0]).await.unwrap();
        assert_eq!(stale.added_messages, 0);
        assert_eq!(stale.ignored_older_messages, 1);
        let history = managed.messages(&first.conversation, 10).unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|message| message.role != Role::System)
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            vec!["middle", "newest"]
        );
    }

    #[tokio::test]
    async fn imported_context_is_frozen_before_the_new_user_message_and_stays_bounded() {
        let f = Fixture::new();
        let path = f.write(
            Provider::Codex,
            "main",
            &[
                f.codex_meta("main"),
                f.codex_message("user", "old request"),
                f.codex_message("assistant", "old response"),
            ],
        );
        let report = f.discover();
        let managed = f.managed();
        let imported = managed.import_session(&report.sessions[0]).await.unwrap();
        let input = Message {
            id: new_id("m"),
            role: Role::User,
            text: "new task".into(),
            at_ms: f.now + 1,
            attachments: vec![],
            provenance: None,
        };
        {
            let mut db = managed.write_db().unwrap();
            let tx = db.transaction().unwrap();
            ManagedStore::append_message_tx(&tx, &input, &imported.conversation, None).unwrap();
            tx.commit().unwrap();
        }
        let before = managed
            .imported_session_context(&imported.conversation, &input.id)
            .unwrap();
        assert!(before.contains("old request") && before.contains("old response"));
        assert!(!before.contains("new task"));
        assert!(before.contains("historical context only"));
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            "{}",
            f.codex_message("assistant", "late provider output")
        )
        .unwrap();
        let later = f.discover();
        managed.import_session(&later.sessions[0]).await.unwrap();
        assert_eq!(
            managed
                .imported_session_context(&imported.conversation, &input.id)
                .unwrap(),
            before
        );
        assert!(
            managed
                .imported_session_context(&Id::new("c_normal").unwrap(), &input.id)
                .unwrap()
                .is_empty()
        );
        assert!(before.len() < MAX_CONTEXT_BYTES + 512);
    }

    #[test]
    fn many_new_subagents_leave_file_slots_and_read_budget_for_main_history() {
        let f = Fixture::new();
        let main = f.write(
            Provider::Codex,
            "main",
            &[
                f.codex_meta("main"),
                f.codex_message("user", "main history"),
            ],
        );
        File::open(&main)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_millis(f.now - 1000)),
            )
            .unwrap();
        for index in 0..MAX_FILES + 4 {
            let mut meta = f.codex_meta(&format!("child-{index}"));
            meta["payload"]["source"] = json!({"subagent":{"parent_thread_id":"main"}});
            let path = f.write(Provider::Codex, &format!("child-{index}"), &[meta]);
            OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_len((MAX_FILE_BYTES * 4) as u64)
                .unwrap();
        }
        let report = f.discover();
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].messages[0].text, "main history");
        assert_eq!(report.skipped_files, MAX_FILES + 4);
        assert!(report.read_bytes < MAX_FILE_BYTES);
        assert!(!report.truncated);
    }

    #[test]
    fn large_log_reads_a_bounded_head_and_recent_tail() {
        let f = Fixture::new();
        let path = f.write(
            Provider::Codex,
            "large",
            &[f.codex_meta("large"), f.codex_message("user", "original")],
        );
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        let padding = format!("{}\n", json!({"type":"ignored","padding":"x".repeat(4096)}));
        for _ in 0..(MAX_FILE_BYTES / padding.len() + 2) {
            file.write_all(padding.as_bytes()).unwrap();
        }
        writeln!(file, "{}", f.codex_message("assistant", "latest result")).unwrap();
        let report = f.discover();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].truncated);
        assert!(report.read_bytes <= MAX_FILE_BYTES);
        assert_eq!(
            report.sessions[0].messages.last().unwrap().text,
            "latest result"
        );
        assert!(
            report.sessions[0]
                .messages
                .iter()
                .any(|message| message.text == "original")
        );
    }
}
