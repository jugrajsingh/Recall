use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

use recall::adapters;
use recall::config::AppConfig;
use recall::db;
use recall::db::search::{SearchEngine, SearchFilters, TimeRange};
use recall::db::store::Store;
use recall::embedding::EmbeddingProvider;
use recall::semantic;
use recall::types::{self, Message, Role, Session};
use recall::utils;

#[derive(Parser)]
#[command(name = "recall", version, about = "Search and recall AI coding sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum WorkerAction {
    /// Print whether the worker is running and what it's doing.
    Status,
    /// Send SIGTERM to the worker holding the lock. Lock is released
    /// automatically when the process exits.
    Stop {
        /// Also drop every queued semantic embedding job. Use when the
        /// queue is stale (e.g. after switching to a mini build that
        /// cannot drain it).
        #[arg(long)]
        clear_queue: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the resolved config (everything the running binary sees).
    Show,
    /// Open the config file in $EDITOR; validates JSON + globs on save.
    Edit,
    /// Diagnose common config issues — perms, glob syntax, embedding
    /// settings consistent with this build.
    Doctor,
}

#[derive(Subcommand)]
enum Commands {
    Info,
    Sync {
        #[arg(long, help = "Reprocess every session, even if unchanged")]
        force: bool,
        #[arg(short, long, help = "Show per-source scan progress and settings")]
        verbose: bool,
    },
    /// Remove DB rows that are no longer eligible — file deleted on disk,
    /// matches an exclusion rule, or belongs to a now-disabled source.
    /// Run this after editing the config; then run `sync` to pick up new files.
    Prune {
        #[arg(long, help = "Show what would be removed without making changes")]
        dry_run: bool,
    },
    /// Drop ALL session data (sessions, messages, FTS, vector embeddings).
    /// Config and config file location are kept. Use after major changes
    /// (embedding provider swap, schema rebuild) or to start fresh.
    Reset {
        #[arg(long, help = "Skip the interactive confirmation prompt")]
        yes: bool,
    },
    /// VACUUM + ANALYZE the SQLite database. Reclaims disk space and
    /// refreshes query planner stats. Safe to run anytime.
    Vacuum,
    /// Inspect or control the background semantic worker.
    Worker {
        #[command(subcommand)]
        action: WorkerAction,
    },
    /// Inspect or edit the recall config file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Drop only the vector embeddings and re-enqueue every session for
    /// re-embedding. Use when changing embedding provider or model. FTS
    /// keeps working in the meantime. (Full build only.)
    Reembed {
        #[arg(long, help = "Skip the interactive confirmation prompt")]
        yes: bool,
    },
    #[command(hide = true, name = "__background-worker")]
    BackgroundWorker {
        #[arg(long)]
        sync_first: bool,
    },
    #[command(hide = true, name = "__bench-semantic")]
    BenchSemantic,
    #[command(hide = true, name = "__bench-search")]
    BenchSearch {
        query: String,
    },
    #[command(hide = true, name = "__bench-eval")]
    BenchEval {
        #[arg(long)]
        dataset: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    #[command(hide = true, name = "__bench-dump-sessions")]
    BenchDumpSessions,
    Search {
        query: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        time: Option<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    db::schema::register_sqlite_vec();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Info) => cmd_info()?,
        Some(Commands::Sync { force, verbose }) => cmd_sync(force, verbose)?,
        Some(Commands::Prune { dry_run }) => cmd_prune(dry_run)?,
        Some(Commands::Reset { yes }) => cmd_reset(yes)?,
        Some(Commands::Vacuum) => cmd_vacuum()?,
        Some(Commands::Worker { action }) => match action {
            WorkerAction::Status => cmd_worker_status()?,
            WorkerAction::Stop { clear_queue } => cmd_worker_stop(clear_queue)?,
        },
        Some(Commands::Config { action }) => match action {
            ConfigAction::Show => cmd_config_show()?,
            ConfigAction::Edit => cmd_config_edit()?,
            ConfigAction::Doctor => cmd_config_doctor()?,
        },
        Some(Commands::Reembed { yes }) => cmd_reembed(yes)?,
        Some(Commands::BackgroundWorker { sync_first }) => cmd_background_worker(sync_first)?,
        Some(Commands::BenchSemantic) => recall::bench::run_semantic()?,
        Some(Commands::BenchSearch { query }) => recall::bench::run_search(&query)?,
        Some(Commands::BenchEval { dataset, verbose }) => {
            recall::bench::run_eval(dataset.as_deref(), verbose)?
        }
        Some(Commands::BenchDumpSessions) => recall::bench::dump_sessions()?,
        Some(Commands::Search { query, source, time }) => {
            cmd_search(&query, source.as_deref(), time.as_deref())?
        }
        None => cmd_tui()?,
    }

    Ok(())
}

fn cmd_info() -> Result<()> {
    let all = adapters::all_adapters();
    let labels = adapters::source_labels();
    let mut config = AppConfig::load_or_default();
    config.normalize_sources(&labels);
    let store = Store::open()?;
    let progress = store.semantic_progress().unwrap_or_default();
    let worker = store.background_job_status("pipeline").unwrap_or_default();

    struct SourceSummary {
        label: String,
        id: String,
        sessions: usize,
        messages: usize,
        range: String,
        error: Option<String>,
    }

    let mut rows = Vec::new();

    let mut grand_sessions = 0usize;
    let mut grand_messages = 0usize;

    for adapter in &all {
        let id = adapter.id();
        let label =
            labels.iter().find(|(k, _)| k == id).map(|(_, v)| v.as_str()).unwrap_or(id).to_string();

        match adapter.scan_summary() {
            Ok(Some(summary)) => {
                grand_sessions += summary.sessions;
                grand_messages += summary.messages;

                rows.push(SourceSummary {
                    label,
                    id: id.to_string(),
                    sessions: summary.sessions,
                    messages: summary.messages,
                    range: format_date_range(summary.oldest_started_at, summary.newest_started_at),
                    error: None,
                });
            }
            Ok(None) => match adapter.scan() {
                Ok(sessions) => {
                    let session_count = sessions.len();
                    let message_count: usize = sessions.iter().map(|s| s.messages.len()).sum();
                    let oldest = sessions.iter().map(|s| s.started_at).min();
                    let newest = sessions.iter().map(|s| s.started_at).max();

                    grand_sessions += session_count;
                    grand_messages += message_count;

                    rows.push(SourceSummary {
                        label,
                        id: id.to_string(),
                        sessions: session_count,
                        messages: message_count,
                        range: format_date_range(oldest, newest),
                        error: None,
                    });
                }
                Err(e) => {
                    rows.push(SourceSummary {
                        label,
                        id: id.to_string(),
                        sessions: 0,
                        messages: 0,
                        range: "-".to_string(),
                        error: Some(e.to_string()),
                    });
                }
            },
            Err(e) => {
                rows.push(SourceSummary {
                    label,
                    id: id.to_string(),
                    sessions: 0,
                    messages: 0,
                    range: "-".to_string(),
                    error: Some(e.to_string()),
                });
            }
        }
    }

    let source_width = rows
        .iter()
        .map(|row| format!("{} ({})", row.label, row.id).len())
        .max()
        .unwrap_or(12)
        .max("Source".len());
    let sessions_width = rows
        .iter()
        .map(|row| row.sessions.to_string().len())
        .max()
        .unwrap_or(1)
        .max("Sessions".len())
        .max(grand_sessions.to_string().len());
    let messages_width = rows
        .iter()
        .map(|row| row.messages.to_string().len())
        .max()
        .unwrap_or(1)
        .max("Messages".len())
        .max(grand_messages.to_string().len());

    println!("Source Scan");
    println!(
        "  {source:<source_width$}  {sessions:>sessions_width$}  {messages:>messages_width$}  Range",
        source = "Source",
        sessions = "Sessions",
        messages = "Messages"
    );
    for row in rows {
        let source = format!("{} ({})", row.label, row.id);
        if let Some(error) = row.error {
            println!(
                "  {source:<source_width$}  {sessions:>sessions_width$}  {messages:>messages_width$}  error: {error}",
                sessions = "-",
                messages = "-"
            );
            continue;
        }
        println!(
            "  {source:<source_width$}  {sessions:>sessions_width$}  {messages:>messages_width$}  {range}",
            sessions = row.sessions,
            messages = row.messages,
            range = row.range
        );
    }
    println!(
        "  {source:<source_width$}  {sessions:>sessions_width$}  {messages:>messages_width$}",
        source = "Total scanned",
        sessions = grand_sessions,
        messages = grand_messages
    );

    println!();
    println!("Settings");
    println!(
        "  Sources     {}",
        labels
            .iter()
            .filter(|(id, _)| config.is_source_enabled(id))
            .map(|(_, label)| label.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  Time scope  {}", config.sync_window.label());
    if !config.excluded_paths.is_empty() {
        println!(
            "  Exclusion   {} rule(s) — see `recall config show`",
            config.excluded_paths.len()
        );
    }

    // Build-feature reporting — tells the user whether they're running the
    // mini (FTS-only) or full (semantic) binary.
    println!();
    println!("Build");
    #[cfg(feature = "semantic-search")]
    println!("  Variant     recall-full (semantic-search enabled)");
    #[cfg(not(feature = "semantic-search"))]
    println!("  Variant     recall-mini (FTS only)");
    println!("  Version     {}", env!("CARGO_PKG_VERSION"));

    println!();
    #[cfg(feature = "semantic-search")]
    {
        println!("Semantic Queue");
        println!("  Indexed DB  {} sessions tracked locally", progress.total_sessions);
        println!(
            "  Progress    {} done, {} pending, {} failed",
            progress.done_sessions,
            progress.pending_sessions + progress.processing_sessions,
            progress.failed_sessions
        );
        if let Some(phase) = worker.phase.as_deref() {
            println!("  Worker      {phase}");
        }
    }
    #[cfg(not(feature = "semantic-search"))]
    {
        let _ = (&progress, &worker);
        println!("Semantic    not compiled in (recall-mini, FTS only)");
    }

    println!();
    println!("Tip: open the TUI and press Ctrl+S to edit settings.");

    Ok(())
}

fn format_date_range(oldest: Option<i64>, newest: Option<i64>) -> String {
    if oldest.is_none() && newest.is_none() {
        return "-".to_string();
    }

    let oldest = oldest
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "-".to_string());
    let newest = newest
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "-".to_string());

    format!("{oldest} -> {newest}")
}

fn cmd_sync(force: bool, verbose: bool) -> Result<()> {
    run_sync_job(force, verbose)?;
    // Only the full build has a semantic worker to drain the queue.
    // In mini the worker would just spin up, find nothing to do, exit —
    // wasteful. Skip the spawn entirely.
    #[cfg(feature = "semantic-search")]
    semantic::ensure_background_worker(false)?;
    Ok(())
}

/// Remove DB rows that are no longer eligible.
///
/// A row is an "orphan" if any of the following holds against the current
/// config + filesystem:
///   1. Its source is disabled in `disabled_sources`.
///   2. Its `(source, source_id)` is not returned by the source adapter
///      anymore (the underlying file has been deleted or moved).
///   3. Its `directory` (cwd) matches an `excluded_paths` glob.
///   4. Its on-disk `source_file_path` matches an `excluded_paths` glob.
///
/// All four collapse to a single check: "is this row in the set of
/// currently-eligible sessions?" — anything not in that set is removed.
///
/// `--dry-run` prints the classification without deleting.
fn cmd_prune(dry_run: bool) -> Result<()> {
    use std::collections::{HashMap, HashSet};

    let store = Store::open()?;
    let all = adapters::all_adapters();
    let labels = adapters::source_labels();
    let mut config = AppConfig::load_or_default();
    config.normalize_sources(&labels);
    let matcher = config.build_path_excluder()?;

    if let Some(_) = matcher.as_ref() {
        println!("Exclusion rules active: {}", config.excluded_paths.len());
    } else {
        println!("Exclusion rules: none configured");
    }

    // Build the eligible set by re-scanning each enabled source. For each
    // session that survives the exclusion check, record (source, source_id).
    // We also keep a lookup of (source, source_id) → directory/file_path so
    // we can classify the reason a DB row is being removed.
    let mut eligible: HashSet<(String, String)> = HashSet::new();
    for adapter in &all {
        let source_id = adapter.id();
        if !config.is_source_enabled(source_id) {
            continue;
        }
        let scan = match adapter.scan() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warn: scanning {source_id} failed: {e} — skipping for prune");
                continue;
            }
        };
        for raw in scan {
            let excluded = matcher
                .as_ref()
                .map(|m| {
                    let dir_hit = raw.directory.as_deref().map(|d| m.is_match(d)).unwrap_or(false);
                    let path_hit =
                        raw.source_file_path.as_deref().map(|p| m.is_match(p)).unwrap_or(false);
                    dir_hit || path_hit
                })
                .unwrap_or(false);
            if !excluded {
                eligible.insert((source_id.to_string(), raw.source_id));
            }
        }
    }

    // Classify every existing row: keep if in eligible set, else remove with
    // a reason chosen by precedence: disabled source > excluded by rule >
    // orphan (file missing or moved).
    let all_rows = store.all_session_paths()?;
    let total = all_rows.len();
    let mut to_remove: Vec<(String, String, &'static str)> = Vec::new();
    for (source, source_id, directory) in all_rows {
        if eligible.contains(&(source.clone(), source_id.clone())) {
            continue;
        }
        let reason = if !config.is_source_enabled(&source) {
            "disabled source"
        } else if matcher
            .as_ref()
            .map(|m| directory.as_deref().map(|d| m.is_match(d)).unwrap_or(false))
            .unwrap_or(false)
        {
            "excluded by rule"
        } else {
            "orphan (file missing)"
        };
        to_remove.push((source, source_id, reason));
    }

    let kept = total - to_remove.len();
    println!("DB rows: {total} · keep: {kept} · remove: {}", to_remove.len());

    if !to_remove.is_empty() {
        let mut by_reason: HashMap<&'static str, usize> = HashMap::new();
        for (_, _, reason) in &to_remove {
            *by_reason.entry(*reason).or_insert(0) += 1;
        }
        let mut sorted: Vec<_> = by_reason.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));
        for (reason, count) in sorted {
            println!("  {count:>5}  {reason}");
        }
    }

    if dry_run {
        println!();
        println!("dry-run: no changes made. Re-run without --dry-run to apply.");
        return Ok(());
    }

    if to_remove.is_empty() {
        println!("nothing to remove.");
        return Ok(());
    }

    let mut failed = 0u32;
    for (source, source_id, _) in &to_remove {
        if let Err(e) = store.delete_session_data(source, source_id) {
            eprintln!("warn: failed to delete {source}/{source_id}: {e}");
            failed += 1;
        }
    }
    let removed = to_remove.len() as u32 - failed;
    println!("removed {removed} rows.");
    if failed > 0 {
        println!("({failed} failed — see warnings above)");
    }
    println!("tip: run `recall sync` to pick up any newly-eligible files.");
    Ok(())
}

fn run_sync_job(force: bool, verbose: bool) -> Result<()> {
    let store = Store::open()?;
    let all = adapters::all_adapters();
    let labels = adapters::source_labels();
    let mut config = AppConfig::load_or_default();
    config.normalize_sources(&labels);
    let since_ts = config.sync_window.to_since_cutoff();
    let path_excluder = config.build_path_excluder()?;
    if verbose && let Some(_) = path_excluder.as_ref() {
        println!("Path exclusion: {} rule(s) active", config.excluded_paths.len());
    }

    let mut new_sessions = 0u32;
    let mut updated_sessions = 0u32;
    let mut reprocessed_sessions = 0u32;
    let mut total_messages = 0u32;
    let mut skipped = 0u32;
    let mut filtered_out = 0u32;
    let mut excluded_out = 0u32;

    for adapter in &all {
        let source_id = adapter.id();
        let label = adapter.label();

        if !config.is_source_enabled(source_id) {
            if verbose {
                println!("Skipping {label} (filtered)");
            }
            continue;
        }

        if verbose {
            println!("Scanning {label}...");
        }
        let optimized = if force {
            None
        } else {
            match adapter.scan_for_sync(&store, since_ts) {
                Ok(scan) => scan,
                Err(e) => {
                    eprintln!("Error scanning {label}: {e}");
                    continue;
                }
            }
        };
        let (raw_sessions, pre_skipped, pre_filtered) = match optimized {
            Some(scan) => {
                (scan.sessions, scan.stats.skipped_sessions, scan.stats.filtered_sessions)
            }
            None => {
                let raw_sessions = match adapter.scan() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("Error scanning {label}: {e}");
                        continue;
                    }
                };
                (raw_sessions, 0, 0)
            }
        };
        skipped += pre_skipped;
        filtered_out += pre_filtered;

        // Apply path-exclusion at ingestion time so newly-discovered excluded
        // sessions never enter the DB. Deletion of EXISTING rows is the
        // responsibility of `recall prune` — sync does not delete by rule.
        let raw_sessions: Vec<_> = if let Some(matcher) = path_excluder.as_ref() {
            raw_sessions
                .into_iter()
                .filter(|raw| {
                    let dir_hit =
                        raw.directory.as_deref().map(|d| matcher.is_match(d)).unwrap_or(false);
                    let path_hit = raw
                        .source_file_path
                        .as_deref()
                        .map(|p| matcher.is_match(p))
                        .unwrap_or(false);
                    if dir_hit || path_hit {
                        excluded_out += 1;
                        false
                    } else {
                        true
                    }
                })
                .collect()
        } else {
            raw_sessions
        };

        if verbose {
            println!("  Found {} sessions", raw_sessions.len());
        }

        let mut existing_meta = store.session_meta_map(source_id)?;

        for raw in raw_sessions {
            if let Some(cutoff) = since_ts {
                let ts = raw.updated_at.unwrap_or(raw.started_at);
                if ts < cutoff {
                    filtered_out += 1;
                    continue;
                }
            }

            let msg_count = raw.messages.len() as u32;

            match existing_meta.get(&raw.source_id) {
                Some(&(old_updated_at, old_msg_count)) => {
                    let changed = old_msg_count != msg_count
                        || (raw.updated_at.is_some() && raw.updated_at != old_updated_at);
                    if !changed && !force {
                        skipped += 1;
                        continue;
                    }
                    store.delete_session_data(source_id, &raw.source_id)?;
                    if changed {
                        updated_sessions += 1;
                    } else {
                        reprocessed_sessions += 1;
                    }
                }
                None => {
                    new_sessions += 1;
                }
            }

            let session_uuid = uuid::Uuid::new_v4().to_string();
            let title = generate_title(&raw.messages);

            let session = Session {
                id: session_uuid.clone(),
                source: source_id.to_string(),
                source_id: raw.source_id,
                title,
                directory: raw.directory,
                started_at: raw.started_at,
                updated_at: raw.updated_at,
                message_count: msg_count,
                entrypoint: raw.entrypoint,
                custom_title: raw.custom_title,
                summary: raw.summary,
                duration_minutes: raw.duration_minutes,
            };

            let messages: Vec<Message> = raw
                .messages
                .into_iter()
                .enumerate()
                .map(|(i, m)| Message {
                    session_id: session_uuid.clone(),
                    role: m.role,
                    content: m.content,
                    timestamp: m.timestamp,
                    seq: i as u32,
                })
                .collect();

            store.persist_session(&session, &messages)?;
            existing_meta
                .insert(session.source_id.clone(), (session.updated_at, session.message_count));
            total_messages += msg_count;
        }

        info!("{label} done");
    }

    let touched = new_sessions + updated_sessions + reprocessed_sessions;

    if verbose {
        println!();
        if force {
            print!(
                "Force sync: {new_sessions} new, {updated_sessions} updated, {reprocessed_sessions} reprocessed, {total_messages} messages"
            );
        } else {
            print!(
                "Sync: {new_sessions} new, {updated_sessions} updated, {skipped} unchanged, {total_messages} messages"
            );
        }
        if filtered_out > 0 {
            print!(", {filtered_out} outside configured time scope");
        }
        if excluded_out > 0 {
            print!(
                ", {excluded_out} excluded by path rules (run `recall prune` to clean existing)"
            );
        }
        println!();
        println!(
            "Settings: sources [{}], time scope [{}]",
            labels
                .iter()
                .filter(|(id, _)| config.is_source_enabled(id))
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            config.sync_window.label()
        );
        let progress = store.semantic_progress()?;
        if progress.total_sessions > 0 {
            println!(
                "Semantic queue: {}/{} done, {} pending, {} failed",
                progress.done_sessions,
                progress.total_sessions,
                progress.pending_sessions + progress.processing_sessions,
                progress.failed_sessions
            );
        }
    } else if force {
        println!("Reprocessed {touched} sessions, {total_messages} messages");
    } else if touched == 0 {
        println!("Up to date.");
    } else {
        println!("{new_sessions} new, {updated_sessions} updated, {total_messages} messages");
    }

    Ok(())
}

fn cmd_background_worker(sync_first: bool) -> Result<()> {
    semantic::run_background_worker(sync_first, || run_sync_job(false, false))
}

// =====================================================================
// Maintenance commands: reset / vacuum / worker / config
// Added in our fork on top of samzong/Recall. See FORK_PATCHES.md.
// =====================================================================

fn cmd_reset(yes: bool) -> Result<()> {
    use std::io::{self, BufRead, Write};

    let db_path = Store::db_path()?;
    println!("This will DROP all session data in: {}", db_path.display());
    println!("  - sessions, messages, FTS, vector embeddings, background job state");
    println!("Your config file is NOT touched.");

    if !yes {
        print!("Type 'reset' to confirm: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if line.trim() != "reset" {
            println!("aborted.");
            return Ok(());
        }
    }

    // Best-effort: ask the worker to stop before we wipe — otherwise it can
    // re-insert rows mid-reset and corrupt the queue. We don't error if it
    // wasn't running.
    if let Ok(true) = semantic::worker_lock_is_held() {
        if let Ok(Some(pid)) = semantic::worker_lock_pid() {
            eprintln!("Stopping background worker (pid {pid})...");
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            // brief pause for the worker to release the lock
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    // Reset = full wipe. Remove the DB file (and SQLite's sidecar files)
    // rather than DELETE-row-by-row. Cleaner, faster, and survives
    // corrupted virtual-table state (FTS / sqlite-vec) that would otherwise
    // break per-table DELETE.
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }
    for suffix in &["-wal", "-shm", "-journal"] {
        let sidecar = db_path.with_file_name(format!(
            "{}{suffix}",
            db_path.file_name().and_then(|n| n.to_str()).unwrap_or("recall.db")
        ));
        if sidecar.exists() {
            let _ = std::fs::remove_file(sidecar);
        }
    }

    // Re-create an empty DB so subsequent commands don't need to worry
    // about whether the file exists.
    let _ = Store::open()?;
    println!("ok. Database wiped. Run `recall sync` to rebuild the index.");
    Ok(())
}

fn cmd_vacuum() -> Result<()> {
    let store = Store::open()?;
    println!("Running VACUUM + ANALYZE...");
    let (before, after) = store.vacuum()?;
    let saved = before.saturating_sub(after);
    let fmt = |b: u64| -> String {
        if b > 1 << 30 {
            format!("{:.2} GiB", b as f64 / (1u64 << 30) as f64)
        } else if b > 1 << 20 {
            format!("{:.2} MiB", b as f64 / (1u64 << 20) as f64)
        } else if b > 1 << 10 {
            format!("{:.1} KiB", b as f64 / (1u64 << 10) as f64)
        } else {
            format!("{b} B")
        }
    };
    println!("ok. before: {} → after: {} (reclaimed {})", fmt(before), fmt(after), fmt(saved));
    Ok(())
}

fn cmd_worker_status() -> Result<()> {
    let held = semantic::worker_lock_is_held()?;
    let pid = semantic::worker_lock_pid()?;
    let lock_path = semantic::worker_lock_path()?;

    if held {
        if let Some(pid) = pid {
            println!("Worker: RUNNING (pid {pid})");
        } else {
            println!("Worker: RUNNING (pid file empty)");
        }
    } else {
        println!("Worker: not running");
        if pid.is_some() {
            println!("  (stale pid file at {})", lock_path.display());
        }
    }

    #[cfg(not(feature = "semantic-search"))]
    {
        // In mini there's no semantic worker concept — keep the output
        // minimal and don't surface stale "phase" rows.
        let _ = held;
        return Ok(());
    }

    #[cfg(feature = "semantic-search")]
    {
        // DB-side job status. When the worker isn't running this is stored
        // history, not live state — label it accordingly so it doesn't look
        // like the worker is doing something it isn't.
        let store = Store::open()?;
        let status = store.background_job_status("pipeline").unwrap_or_default();
        if let Some(phase) = status.phase {
            let detail = status.detail.unwrap_or_default();
            let label = if held { "Current phase" } else { "Last known phase (stale)" };
            println!(
                "{label}: {phase}{}",
                if detail.is_empty() { String::new() } else { format!(" — {detail}") }
            );
        }
        let progress = store.semantic_progress().unwrap_or_default();
        if progress.total_sessions > 0 {
            let pending = progress.pending_sessions + progress.processing_sessions;
            println!(
                "Semantic queue: {} done, {} pending, {} failed",
                progress.done_sessions, pending, progress.failed_sessions
            );
        }
        Ok(())
    }
}

fn cmd_worker_stop(clear_queue: bool) -> Result<()> {
    let held = semantic::worker_lock_is_held()?;
    if held {
        let Some(pid) = semantic::worker_lock_pid()? else {
            eprintln!("Worker lock held but pid file is empty — cannot stop cleanly.");
            return Ok(());
        };
        #[cfg(unix)]
        unsafe {
            if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
                return Err(anyhow::anyhow!(
                    "failed to SIGTERM pid {pid}: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }
        #[cfg(not(unix))]
        {
            return Err(anyhow::anyhow!("worker stop is only implemented on unix"));
        }
        println!("sent SIGTERM to worker pid {pid}.");
        // brief pause so the worker releases the lock before we proceed
        std::thread::sleep(std::time::Duration::from_millis(300));
    } else {
        println!("Worker is not running.");
    }

    // Always tidy up the stale pid file when the worker isn't actually
    // holding the lock anymore — saves a confusing "(stale pid file...)" on
    // the next status call.
    let lock_path = semantic::worker_lock_path()?;
    if !semantic::worker_lock_is_held()? && lock_path.exists() {
        let _ = std::fs::remove_file(&lock_path);
    }

    if clear_queue {
        let store = Store::open()?;
        // Drop both the per-session embedding state rows AND any partial
        // vectors. FTS is untouched. After this, every session will look
        // "not embedded" again — full builds can re-enqueue via `reembed`.
        let cleared = store.clear_semantic_queue()?;
        println!("cleared {cleared} queued/processed embedding rows.");
    }

    Ok(())
}

#[cfg(not(feature = "semantic-search"))]
fn cmd_reembed(_yes: bool) -> Result<()> {
    eprintln!("recall-mini does not include semantic search.");
    eprintln!("Install the full build to use `reembed`:");
    eprintln!("    cd claude-code/recall && cargo build --release --features semantic-search");
    std::process::exit(2);
}

#[cfg(feature = "semantic-search")]
fn cmd_reembed(yes: bool) -> Result<()> {
    use std::io::{self, BufRead, Write};

    println!("This will drop all vector embeddings and re-enqueue every session.");
    println!("FTS search remains available during the rebuild.");

    if !yes {
        print!("Type 'reembed' to confirm: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        if line.trim() != "reembed" {
            println!("aborted.");
            return Ok(());
        }
    }

    // Stop worker first so it doesn't insert new vectors mid-wipe.
    if let Ok(true) = semantic::worker_lock_is_held() {
        if let Ok(Some(pid)) = semantic::worker_lock_pid() {
            eprintln!("Stopping background worker (pid {pid})...");
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    let store = Store::open()?;
    store.conn.execute_batch(
        "BEGIN;
         DELETE FROM message_vec;
         DELETE FROM session_embedding_state;
         COMMIT;",
    )?;
    println!("ok. Embeddings cleared. The worker will rebuild on next launch:");
    println!("    recall            # launching TUI auto-spawns worker");
    println!("    # or:");
    println!("    recall sync       # triggers worker afterwards");
    Ok(())
}

fn cmd_config_show() -> Result<()> {
    let path = recall::config::config_path()?;
    println!("# resolved config (from {})", path.display());
    let config = AppConfig::load_or_default();
    let json = serde_json::to_string_pretty(&config)?;
    println!("{json}");
    Ok(())
}

fn cmd_config_edit() -> Result<()> {
    let path = recall::config::config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        let default = AppConfig::default();
        std::fs::write(&path, serde_json::to_string_pretty(&default)?)?;
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor).arg(&path).status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("editor exited with status {status}"));
    }
    // Re-validate after the edit so syntax errors surface immediately.
    match AppConfig::load() {
        Ok(cfg) => {
            // Also compile globs to catch syntax errors.
            cfg.build_path_excluder()?;
            println!("config ok.");
        }
        Err(e) => {
            return Err(anyhow::anyhow!("config has errors after edit: {e}"));
        }
    }
    Ok(())
}

fn cmd_config_doctor() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let path = recall::config::config_path()?;
    let mut issues = 0u32;
    let mut warnings = 0u32;

    println!("Config file: {}", path.display());
    if !path.exists() {
        println!("  [warn] file does not exist — using defaults");
        warnings += 1;
    } else {
        let meta = std::fs::metadata(&path)?;
        #[cfg(unix)]
        {
            let mode = meta.permissions().mode() & 0o777;
            if mode != 0o600 {
                println!(
                    "  [warn] perms are {:o} — recommend 0600 (chmod 600 {})",
                    mode,
                    path.display()
                );
                warnings += 1;
            } else {
                println!("  [ok] perms 0600");
            }
        }
        println!("  [ok] size {} bytes", meta.len());
    }

    println!();
    println!("Parsing config...");
    let config = match AppConfig::load() {
        Ok(c) => {
            println!("  [ok] valid JSON");
            c
        }
        Err(e) => {
            println!("  [err] failed to parse: {e}");
            issues += 1;
            return summarize_doctor(issues, warnings);
        }
    };

    println!();
    println!("Exclusion globs: {} rule(s)", config.excluded_paths.len());
    match config.build_path_excluder() {
        Ok(Some(_)) => println!("  [ok] all globs compile"),
        Ok(None) => println!("  [ok] no rules to compile"),
        Err(e) => {
            println!("  [err] {e}");
            issues += 1;
        }
    }

    println!();
    println!("Sources:");
    let labels = adapters::source_labels();
    let total = labels.len();
    let enabled: Vec<_> = labels.iter().filter(|(id, _)| config.is_source_enabled(id)).collect();
    println!("  {} of {} enabled", enabled.len(), total);
    if enabled.is_empty() {
        println!("  [err] no sources enabled — sync will find nothing");
        issues += 1;
    }

    println!();
    println!("Build features:");
    #[cfg(feature = "semantic-search")]
    println!("  [ok] semantic-search compiled in (full build)");
    #[cfg(not(feature = "semantic-search"))]
    println!("  [info] semantic-search NOT compiled (mini build) — FTS only");

    summarize_doctor(issues, warnings)
}

fn summarize_doctor(issues: u32, warnings: u32) -> Result<()> {
    println!();
    if issues == 0 && warnings == 0 {
        println!("All checks passed.");
    } else {
        println!("{issues} issue(s), {warnings} warning(s).");
        if issues > 0 {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn cmd_search(query: &str, source_filter: Option<&str>, time_filter: Option<&str>) -> Result<()> {
    let store = Store::open()?;
    let engine = SearchEngine::new(&store.conn);
    let sources = adapters::source_labels();
    let progress = store.semantic_progress().unwrap_or_default();

    // Mini builds skip embedding model load entirely (no candle compiled in
    // -- well, technically still compiled today, but logic-gated). FTS-only.
    #[cfg(not(feature = "semantic-search"))]
    let query_embedding: Option<Vec<f32>> = {
        let _ = &progress;
        None
    };
    #[cfg(feature = "semantic-search")]
    let query_embedding = if progress.done_sessions > 0 || progress.processing_sessions > 0 {
        println!("Loading embedding model...");
        match EmbeddingProvider::new(true) {
            Ok(provider) => provider
                .embed_query(&[query])?
                .into_iter()
                .next()
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("failed to generate query embedding"))?,
            Err(e) => {
                println!("Semantic unavailable: {e}");
                None
            }
        }
    } else {
        None
    };

    let resolved_source = source_filter.and_then(|s| {
        let lower = s.to_lowercase();
        sources
            .iter()
            .find(|(id, label)| id == &lower || label.to_lowercase() == lower)
            .map(|(id, _)| vec![id.clone()])
    });

    let time_range = match time_filter.map(|t| t.to_lowercase()) {
        Some(ref t) if t == "today" => TimeRange::Today,
        Some(ref t) if t == "7d" || t == "week" => TimeRange::Week,
        Some(ref t) if t == "30d" || t == "month" => TimeRange::Month,
        _ => TimeRange::All,
    };

    let filters = SearchFilters { sources: resolved_source, time_range, directory: None };

    let results = engine.hybrid_search(query, query_embedding.as_deref(), &filters, 20, 3)?;

    if results.is_empty() {
        println!("No results found.");
        return Ok(());
    }

    for (i, result) in results.iter().enumerate() {
        let s = &result.session;
        let age = utils::format_age(s.started_at);
        let dir = s.directory.as_deref().unwrap_or("-");
        let source_label = sources
            .iter()
            .find(|(id, _)| id == &s.source)
            .map(|(_, l)| l.as_str())
            .unwrap_or(&s.source);
        let match_label = match result.match_source {
            types::MatchSource::Fts => "FTS",
            types::MatchSource::Vector => "VEC",
            types::MatchSource::Hybrid => "HYB",
        };
        println!("{:>2}. [{source_label}] [{match_label}] {age:>5}  {}", i + 1, s.title);
        if let Some(snippet) = &result.snippet {
            let short: String = snippet.chars().take(120).collect();
            println!("    {short}");
        }
        println!("    dir: {dir}");
        println!();
    }

    Ok(())
}
fn cmd_tui() -> Result<()> {
    use std::io;
    use std::time::Duration;

    use crossterm::execute;
    use crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };
    use ratatui::Terminal;
    use ratatui::backend::CrosstermBackend;

    use recall::tui::app::App;
    use recall::tui::event::{AppEvent, ScrollDirection, poll_event};
    let _ = ScrollDirection::Up; // suppress unused-import warning if not referenced directly
    use recall::tui::ui;

    let store = Store::open()?;
    // Mini build: do a quick inline sync on TUI launch (no worker needed —
    // there's nothing for it to do once sync finishes). Full build: spawn
    // the worker which runs sync + embedding loop in the background.
    #[cfg(feature = "semantic-search")]
    semantic::ensure_background_worker(true)?;
    #[cfg(not(feature = "semantic-search"))]
    {
        // Run sync inline so the TUI opens with fresh data, then drop straight in.
        if let Err(e) = run_sync_job(false, false) {
            eprintln!("warn: startup sync failed: {e}");
        }
    }
    let sources = adapters::source_labels();

    struct TerminalGuard;
    impl Drop for TerminalGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
            let _ =
                execute!(io::stdout(), crossterm::event::DisableMouseCapture, LeaveAlternateScreen);
        }
    }

    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    // Enter the alt screen WITHOUT capturing mouse — capture is opt-in via
    // Ctrl+M. Without this flip, every drag in the preview pane triggers
    // app-level mouse events instead of the terminal's native text-selection
    // gesture (the thing users actually want 95% of the time).
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let engine = SearchEngine::new(&store.conn);
    let mut provider: Option<EmbeddingProvider> = None;
    let mut config = AppConfig::load_or_default();
    config.normalize_sources(&sources);

    let mut app = App::new(&store, sources, config);
    let tick_rate = Duration::from_millis(50);
    let mut last_mouse_capture = app.mouse_capture_enabled;

    loop {
        terminal.draw(|f| ui::render(f, &mut app))?;

        // Sync mouse capture state to crossterm whenever the user toggled
        // it via Ctrl+M. Re-issuing the command at every change is cheap.
        if app.mouse_capture_enabled != last_mouse_capture {
            if app.mouse_capture_enabled {
                let _ = execute!(io::stdout(), crossterm::event::EnableMouseCapture);
            } else {
                let _ = execute!(io::stdout(), crossterm::event::DisableMouseCapture);
            }
            last_mouse_capture = app.mouse_capture_enabled;
        }

        match poll_event(tick_rate)? {
            AppEvent::Key(key) => {
                app.handle_key(key, &store, &engine, &mut provider);
            }
            AppEvent::Scroll(dir, col, row) => app.handle_mouse_scroll(dir, col, row, &store),
            AppEvent::Click(col, row) => {
                app.handle_mouse_click(col, row, &store, &engine, &mut provider)
            }
            AppEvent::Tick => {}
        }

        app.try_search(&store, &engine, &mut provider);

        if app.should_quit {
            break;
        }
    }

    drop(_guard);
    terminal.show_cursor()?;

    if let Some((command, cwd)) = app.exec_on_exit.take() {
        exec_resume(command, cwd)?;
    }

    Ok(())
}

#[cfg(unix)]
fn exec_resume(command: adapters::ResumeCommand, cwd: Option<String>) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let mut cmd = std::process::Command::new(&command.program);
    cmd.args(&command.args);
    if let Some(ref dir) = cwd
        && std::path::Path::new(dir).is_dir()
    {
        cmd.current_dir(dir);
    }
    let err = cmd.exec();
    Err(anyhow::anyhow!("failed to exec {}: {err}", command.program))
}

#[cfg(not(unix))]
fn exec_resume(command: adapters::ResumeCommand, cwd: Option<String>) -> Result<()> {
    let mut cmd = std::process::Command::new(&command.program);
    cmd.args(&command.args);
    if let Some(ref dir) = cwd
        && std::path::Path::new(dir).is_dir()
    {
        cmd.current_dir(dir);
    }
    let status =
        cmd.status().map_err(|e| anyhow::anyhow!("failed to run {}: {e}", command.program))?;
    std::process::exit(status.code().unwrap_or(0));
}

fn generate_title(messages: &[adapters::RawMessage]) -> String {
    let user_contents: Vec<&str> =
        messages.iter().filter(|m| m.role == Role::User).map(|m| m.content.as_str()).collect();
    utils::title_from_user_messages(&user_contents)
}
