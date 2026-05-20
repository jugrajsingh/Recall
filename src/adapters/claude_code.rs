use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::debug;
use walkdir::WalkDir;

use crate::adapters::file_scan::{self, FileScanEntry};
use crate::adapters::{
    RawMessage, RawSession, ResumeCommand, SourceAdapter, SyncScanResult, SyncScanStats,
};
use crate::db::store::Store;
use crate::types::Role;

pub struct ClaudeCodeAdapter;

impl SourceAdapter for ClaudeCodeAdapter {
    fn id(&self) -> &str {
        "claude-code"
    }
    fn label(&self) -> &str {
        "CC"
    }

    fn resume_command(&self, source_id: &str) -> Option<ResumeCommand> {
        Some(ResumeCommand {
            program: "claude".to_string(),
            args: vec!["--resume".to_string(), source_id.to_string()],
        })
    }

    fn scan(&self) -> anyhow::Result<Vec<RawSession>> {
        let Some(claude_dir) = resolve_claude_dir()? else {
            return Ok(vec![]);
        };
        let session_index = load_session_index(&claude_dir);

        let mut sessions = Vec::new();
        let mut entries = collect_project_entries(&claude_dir, &session_index);
        entries.extend(collect_transcript_entries(&claude_dir));

        for entry in entries {
            let Some(mtime_ms) = file_scan::stat_mtime_ms(&entry.stat_target) else {
                continue;
            };
            if let Some(raw) = parse_claude_session_file(entry, mtime_ms, &session_index)? {
                sessions.push(raw);
            }
        }

        Ok(sessions)
    }

    fn scan_for_sync(
        &self,
        store: &Store,
        since_ts: Option<i64>,
    ) -> anyhow::Result<Option<SyncScanResult>> {
        let Some(claude_dir) = resolve_claude_dir()? else {
            return Ok(Some(SyncScanResult { sessions: vec![], stats: SyncScanStats::default() }));
        };
        let result = scan_for_sync_impl(&claude_dir, store, since_ts)?;
        Ok(Some(result))
    }
}

struct SessionMeta {
    cwd: Option<String>,
    started_at: i64,
    entrypoint: Option<String>,
}

fn resolve_claude_dir() -> anyhow::Result<Option<PathBuf>> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let dir = home.join(".claude");
    if !dir.exists() {
        debug!("~/.claude not found, skipping Claude Code");
        return Ok(None);
    }
    Ok(Some(dir))
}

fn scan_for_sync_impl(
    claude_dir: &Path,
    store: &Store,
    since_ts: Option<i64>,
) -> anyhow::Result<SyncScanResult> {
    let session_index = load_session_index(claude_dir);
    let mut entries = collect_project_entries(claude_dir, &session_index);
    entries.extend(collect_transcript_entries(claude_dir));

    file_scan::run_file_scan(store, "claude-code", since_ts, entries, |entry, mtime_ms| {
        parse_claude_session_file(entry, mtime_ms, &session_index)
    })
}

fn load_session_index(claude_dir: &Path) -> HashMap<String, SessionMeta> {
    let sessions_dir = claude_dir.join("sessions");
    let mut index = HashMap::new();
    if !sessions_dir.exists() {
        return index;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("cannot read ~/.claude/sessions: {e}");
            return index;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let v: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(session_id) = v.get("sessionId").and_then(|s| s.as_str()) {
            let meta = SessionMeta {
                cwd: v.get("cwd").and_then(|s| s.as_str()).map(|s| s.to_string()),
                started_at: v.get("startedAt").and_then(|s| s.as_i64()).unwrap_or(0),
                entrypoint: v.get("entrypoint").and_then(|s| s.as_str()).map(|s| s.to_string()),
            };
            index.insert(session_id.to_string(), meta);
        }
    }
    index
}

fn collect_project_entries(
    claude_dir: &Path,
    session_index: &HashMap<String, SessionMeta>,
) -> Vec<FileScanEntry> {
    let projects_dir = claude_dir.join("projects");
    if !projects_dir.exists() {
        return vec![];
    }

    let mut entries = Vec::new();

    let project_dirs = match fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(e) => {
            debug!("cannot read ~/.claude/projects: {e}");
            return vec![];
        }
    };

    for project_entry in project_dirs.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }

        let dir_name = project_entry.file_name().to_string_lossy().to_string();
        // We deliberately do NOT decode `dir_name` back into a path. Claude
        // encodes paths to directory names by replacing `/` with `-`, which is
        // lossy whenever a real path segment contains `-` (e.g. `.claude-mem`
        // decodes to `/.claude/mem`). The JSONL `cwd` field is authoritative;
        // when it's missing, we leave `directory` as `None` rather than
        // fabricate a wrong path. Display layer shows `<no cwd>` for None.
        let _ = dir_name; // keep for potential future use (e.g. debug logging)

        let jsonl_files = match fs::read_dir(&project_path) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for file_entry in jsonl_files.flatten() {
            let file_path = file_entry.path();
            if file_path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let session_id = match file_path.file_stem().and_then(|s| s.to_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };

            // cwd from JSONL header is authoritative; no fallback (see note above).
            let directory = session_index.get(&session_id).and_then(|m| m.cwd.clone());

            entries.push(FileScanEntry { session_id, stat_target: file_path, directory });
        }
    }

    entries
}

fn collect_transcript_entries(claude_dir: &Path) -> Vec<FileScanEntry> {
    let transcripts_dir = claude_dir.join("transcripts");
    if !transcripts_dir.exists() {
        return vec![];
    }

    let mut entries = Vec::new();

    for entry in WalkDir::new(&transcripts_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let session_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        entries.push(FileScanEntry {
            session_id,
            stat_target: path.to_path_buf(),
            directory: None,
        });
    }

    entries
}

fn parse_claude_session_file(
    entry: FileScanEntry,
    mtime_ms: i64,
    session_index: &HashMap<String, SessionMeta>,
) -> anyhow::Result<Option<RawSession>> {
    let parsed = match parse_conversation_jsonl(&entry.stat_target) {
        Ok(p) => p,
        Err(e) => {
            debug!("failed to parse {}: {e}", entry.stat_target.display());
            return Ok(None);
        }
    };

    if parsed.messages.is_empty() {
        return Ok(None);
    }

    let meta = session_index.get(&entry.session_id);
    let started_at = meta
        .map(|m| m.started_at)
        .or_else(|| parsed.messages.first().and_then(|m| m.timestamp))
        .unwrap_or(0);
    let directory = meta.and_then(|m| m.cwd.clone()).or(entry.directory);
    let entrypoint = meta.and_then(|m| m.entrypoint.clone());
    // Preserve the absolute path of the JSONL file so path-exclusion can
    // match cwd-less sessions (e.g. claude-mem observer sessions) by their
    // on-disk location under ~/.claude/projects/<encoded-dir>/.
    let source_file_path = entry.stat_target.to_str().map(|s| s.to_string());

    // Compute duration in minutes from first→last message timestamp. Falls
    // back to None when either bound is missing.
    let duration_minutes = match (parsed.first_ts, parsed.last_ts) {
        (Some(first), Some(last)) if last > first => Some(((last - first) / 60_000) as u32),
        _ => None,
    };

    Ok(Some(RawSession {
        source_id: entry.session_id,
        directory,
        started_at,
        updated_at: Some(mtime_ms),
        entrypoint,
        messages: parsed.messages,
        source_file_path,
        custom_title: parsed.custom_title,
        summary: parsed.summary,
        duration_minutes,
    }))
}

/// Everything we pull from a single Claude JSONL transcript in one pass.
/// Ported from claude-history's richer extraction model.
pub(crate) struct ParsedClaudeFile {
    pub messages: Vec<RawMessage>,
    /// `{"type":"custom-title","customTitle":"..."}` — Claude `/rename` value.
    /// When multiple custom-title events exist, the latest wins (Claude can
    /// rename mid-session).
    pub custom_title: Option<String>,
    /// `{"type":"summary","summary":"..."}` — Claude auto-generated 1-line
    /// session summary. First non-empty wins; later ones are usually
    /// regenerations and not noticeably better.
    pub summary: Option<String>,
    /// Earliest message timestamp seen (ms epoch).
    pub first_ts: Option<i64>,
    /// Latest message timestamp seen (ms epoch).
    pub last_ts: Option<i64>,
}

fn parse_conversation_jsonl(path: &Path) -> anyhow::Result<ParsedClaudeFile> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut messages = Vec::new();
    let mut custom_title: Option<String> = None;
    let mut summary: Option<String> = None;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // claude-history-style sidechannel extraction: `/rename` writes a
        // `custom-title` (and a sibling `agent-name`) record. Take the most
        // recent customTitle as authoritative.
        if msg_type == "custom-title"
            && let Some(title) = v.get("customTitle").and_then(|t| t.as_str())
        {
            let trimmed = title.trim();
            custom_title = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
            continue;
        }
        // Claude's auto-summary event. First non-empty wins.
        if msg_type == "summary"
            && summary.is_none()
            && let Some(s) = v.get("summary").and_then(|t| t.as_str())
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                summary = Some(trimmed.to_string());
            }
            continue;
        }

        match msg_type {
            "user" | "assistant" => {}
            _ => continue,
        }

        // Skip machinery messages that claude -r itself doesn't surface as
        // conversation content. Flag-based, not substring-based.
        let is_machinery = v.get("isCompactSummary").and_then(|b| b.as_bool()).unwrap_or(false)
            || v.get("isSidechain").and_then(|b| b.as_bool()).unwrap_or(false)
            || v.get("isMeta").and_then(|b| b.as_bool()).unwrap_or(false);
        if is_machinery {
            continue;
        }

        let role = if msg_type == "user" { Role::User } else { Role::Assistant };

        let message = match v.get("message") {
            Some(m) => m,
            None => continue,
        };

        let text = extract_content(message.get("content"));
        if text.is_empty() {
            continue;
        }

        let timestamp = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.timestamp_millis());

        // Track first/last ts for duration computation (port #3 from claude-history).
        if let Some(ts) = timestamp {
            if first_ts.map(|f| ts < f).unwrap_or(true) {
                first_ts = Some(ts);
            }
            if last_ts.map(|l| ts > l).unwrap_or(true) {
                last_ts = Some(ts);
            }
        }

        messages.push(RawMessage { role, content: text, timestamp });
    }

    Ok(ParsedClaudeFile { messages, custom_title, summary, first_ts, last_ts })
}

fn extract_content(content: Option<&Value>) -> String {
    match content {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut parts = Vec::new();
            for item in arr {
                match item.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            parts.push(text.to_string());
                        }
                    }
                    Some("tool_use") => {
                        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                        if let Some(input) = item.get("input") {
                            parts.push(format!("[{name}] {input}"));
                        }
                    }
                    Some("tool_result") => {
                        if let Some(content) = item.get("content") {
                            match content {
                                Value::String(s) => parts.push(s.clone()),
                                Value::Array(inner) => {
                                    for block in inner {
                                        if block.get("type").and_then(|t| t.as_str())
                                            == Some("text")
                                            && let Some(text) =
                                                block.get("text").and_then(|t| t.as_str())
                                        {
                                            parts.push(text.to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::db::{schema, store::Store};
    use crate::types::Session;

    fn setup_store() -> Store {
        schema::register_sqlite_vec();
        Store::open_in_memory().unwrap()
    }

    fn temp_claude_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("recall-cc-test-{}-{}", label, uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_user_jsonl(project_dir: &Path, session_id: &str, text: &str) -> PathBuf {
        fs::create_dir_all(project_dir).unwrap();
        let path = project_dir.join(format!("{session_id}.jsonl"));
        let line = serde_json::json!({
            "type": "user",
            "message": {"content": text},
            "timestamp": "2026-04-13T10:00:00Z"
        });
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "{line}").unwrap();
        path
    }

    fn make_existing_session(source_id: &str, updated_at: i64, message_count: u32) -> Session {
        Session {
            id: format!("internal-{source_id}"),
            source: "claude-code".to_string(),
            source_id: source_id.to_string(),
            title: "existing".to_string(),
            directory: None,
            started_at: 0,
            updated_at: Some(updated_at),
            message_count,
            entrypoint: None,
        }
    }

    #[test]
    fn parse_claude_session_file_sets_updated_at_to_mtime() {
        let root = temp_claude_root("parse");
        let project = root.join("projects").join("-tmp-foo");
        let path = write_user_jsonl(&project, "abc-123", "hello");
        let mtime = file_scan::stat_mtime_ms(&path).unwrap();

        let entry = FileScanEntry {
            session_id: "abc-123".to_string(),
            stat_target: path.clone(),
            directory: Some("/tmp/foo".to_string()),
        };
        let session_index = HashMap::new();
        let raw = parse_claude_session_file(entry, mtime, &session_index).unwrap().unwrap();

        assert_eq!(raw.source_id, "abc-123");
        assert_eq!(raw.updated_at, Some(mtime));
        assert_eq!(raw.directory.as_deref(), Some("/tmp/foo"));
        assert_eq!(raw.messages.len(), 1);
        assert_eq!(raw.messages[0].content, "hello");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn collect_project_entries_walks_nested_projects() {
        let root = temp_claude_root("collect");
        let p1 = root.join("projects").join("-tmp-foo");
        let p2 = root.join("projects").join("-tmp-bar");
        write_user_jsonl(&p1, "sess-1", "a");
        write_user_jsonl(&p2, "sess-2", "b");

        let session_index = HashMap::new();
        let entries = collect_project_entries(&root, &session_index);
        assert_eq!(entries.len(), 2);
        let ids: Vec<_> = entries.iter().map(|e| e.session_id.clone()).collect();
        assert!(ids.contains(&"sess-1".to_string()));
        assert!(ids.contains(&"sess-2".to_string()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_for_sync_skips_unchanged_session() {
        let root = temp_claude_root("skip");
        let project = root.join("projects").join("-tmp-proj");
        let path = write_user_jsonl(&project, "sess-skip", "hello");
        let mtime = file_scan::stat_mtime_ms(&path).unwrap();

        let store = setup_store();
        store.insert_session(&make_existing_session("sess-skip", mtime, 1)).unwrap();

        let result = scan_for_sync_impl(&root, &store, None).unwrap();
        assert_eq!(result.sessions.len(), 0);
        assert_eq!(result.stats.skipped_sessions, 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_for_sync_reparses_when_mtime_diverges() {
        let root = temp_claude_root("mismatch");
        let project = root.join("projects").join("-tmp-proj");
        let path = write_user_jsonl(&project, "sess-stale", "hi");
        let actual_mtime = file_scan::stat_mtime_ms(&path).unwrap();

        let store = setup_store();
        store
            .insert_session(&make_existing_session("sess-stale", actual_mtime - 1_000, 1))
            .unwrap();

        let result = scan_for_sync_impl(&root, &store, None).unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].source_id, "sess-stale");
        assert_eq!(result.sessions[0].updated_at, Some(actual_mtime));
        assert_eq!(result.stats.skipped_sessions, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_for_sync_picks_up_new_session() {
        let root = temp_claude_root("new");
        let project = root.join("projects").join("-tmp-proj");
        write_user_jsonl(&project, "sess-fresh", "fresh");

        let store = setup_store();

        let result = scan_for_sync_impl(&root, &store, None).unwrap();
        assert_eq!(result.sessions.len(), 1);
        assert_eq!(result.sessions[0].source_id, "sess-fresh");
        assert_eq!(result.stats.skipped_sessions, 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_leaves_directory_none_when_jsonl_has_no_cwd() {
        // The encoded dir name `--claude-mem-observer-sessions` would be
        // ambiguously decoded as `/.claude/mem/observer/sessions`; we must
        // not display that. Without a JSONL cwd, directory stays None.
        let root = temp_claude_root("nocwd");
        let project = root.join("projects").join("--claude-mem-observer-sessions");
        fs::create_dir_all(&project).unwrap();
        let path = project.join("sess-x.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        // user message line with no cwd field anywhere
        writeln!(
            f,
            r#"{{"type":"user","message":{{"role":"user","content":"hi"}},"timestamp":"2026-05-20T10:00:00Z"}}"#
        )
        .unwrap();

        let store = setup_store();
        let result = scan_for_sync_impl(&root, &store, None).unwrap();
        assert_eq!(result.sessions.len(), 1, "session should still be ingested");
        assert!(
            result.sessions[0].directory.is_none(),
            "directory must be None when cwd is absent — no mangled fallback. got: {:?}",
            result.sessions[0].directory
        );
        let _ = fs::remove_dir_all(&root);
    }
}
