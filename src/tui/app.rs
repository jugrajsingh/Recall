use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::adapters::{ResumeCommand, resume_command_for};
use crate::config::AppConfig;
use crate::db::search::{SearchEngine, SearchFilters, TimeRange};
use crate::db::store::Store;
use crate::embedding::EmbeddingProvider;
use crate::types::{
    BackgroundJobStatus, MatchSource, Message, Role, SearchResult, SemanticProgress,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Search,
    Viewing,
    ExportInput,
    Settings,
    ConfirmResume,
}

#[derive(Clone, Copy)]
pub enum ResumeOrigin {
    Search,
    Viewing,
}

pub struct PendingResume {
    pub command: ResumeCommand,
    pub source_label: String,
    pub session_title: String,
    pub cwd: Option<String>,
    pub origin: ResumeOrigin,
}

pub struct SanitizedLine {
    pub text: String,
    pub lower: String,
}

fn build_viewing_caches(msgs: &[Message]) -> Vec<Vec<SanitizedLine>> {
    msgs.iter()
        .map(|m| {
            m.content
                .lines()
                .map(|line| {
                    let text = crate::utils::sanitize_line(line);
                    let lower = text.to_lowercase();
                    SanitizedLine { text, lower }
                })
                .collect()
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelFocus {
    SessionList,
    Preview,
}

pub enum FilterFocus {
    Source,
    Time,
    Sort,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortOrder {
    Relevance,
    Newest,
}

impl SortOrder {
    pub fn next(self) -> Self {
        match self {
            Self::Relevance => Self::Newest,
            Self::Newest => Self::Relevance,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Newest => "newest",
        }
    }
}

pub struct App {
    pub mode: AppMode,
    pub panel_focus: PanelFocus,
    pub query: String,
    pub cursor_pos: usize,
    pub results: Vec<SearchResult>,
    pub selected_index: usize,
    pub preview_messages: Vec<Message>,
    pub preview_selected_msg: usize,
    pub viewing_messages: Vec<Message>,
    pub viewing_selected_msg: usize,
    pub all_sources: Vec<(String, String)>,
    pub config: AppConfig,
    pub source_filter_index: usize,
    pub time_filter: TimeRange,
    pub filter_focus: FilterFocus,
    pub should_quit: bool,
    pub last_keystroke: Instant,
    pub search_pending: bool,
    pub embedding_init_pending: bool,
    pub embedding_unavailable: bool,
    pub status_message: Option<String>,
    pub sort_order: SortOrder,
    pub export_path: String,
    pub export_cursor: usize,
    pub total_sessions: u64,
    pub total_messages: u64,
    pub semantic_progress: SemanticProgress,
    pub background_status: BackgroundJobStatus,
    pub semantic_last_refresh: Instant,
    pub settings_selected: usize,
    pub pending_resume: Option<PendingResume>,
    pub exec_on_exit: Option<(ResumeCommand, Option<String>)>,
    pub viewing_search_query: String,
    pub viewing_search_input: Option<String>,
    pub viewing_search_input_cursor: usize,
    pub viewing_search_status: Option<String>,
    pub viewing_sanitized_lines: Vec<Vec<SanitizedLine>>,
    pub viewing_match_cache: Vec<usize>,
    /// Ratatui list state for the session list. Synced from `selected_index`
    /// at render time so the viewport auto-follows selection (k9s/htop feel).
    pub list_state: ratatui::widgets::ListState,
    /// Last rendered rects for each panel — used by mouse routing so a
    /// scroll wheel event over the preview pane goes to the preview, not
    /// the active panel. Updated every frame by the renderer.
    pub list_rect: Option<ratatui::layout::Rect>,
    pub preview_rect: Option<ratatui::layout::Rect>,
    /// User-runtime override for mouse capture. None = follow startup
    /// default (captured); Some(true) = force-on; Some(false) = off so
    /// terminal-native text selection works.
    pub mouse_capture_enabled: bool,
    /// Indices into `preview_messages` that the user has expanded — long
    /// messages are otherwise truncated to PREVIEW_COLLAPSED_LINES with a
    /// "(K more lines)" hint. Cleared whenever a new session is loaded.
    pub preview_expanded: std::collections::HashSet<usize>,
    /// Map from `session.id` (the visible row's id) → cluster size. Built
    /// once per `results` refresh; rows that share a `(cwd, first-line-of-
    /// title)` key collapse, the most recent survives, and the surviving
    /// row's id maps to the cluster's total count. Sessions not in a
    /// cluster have entries with value 1 (or are simply absent).
    pub cluster_counts: std::collections::HashMap<String, usize>,
}

impl App {
    pub fn new(store: &Store, all_sources: Vec<(String, String)>, mut config: AppConfig) -> Self {
        config.normalize_sources(&all_sources);

        let (total_sessions, total_messages) = store.stats().unwrap_or((0, 0));
        let semantic_progress = store.semantic_progress().unwrap_or_default();
        let background_status = store.background_job_status("pipeline").unwrap_or_default();

        let mut app = Self {
            mode: AppMode::Search,
            panel_focus: PanelFocus::SessionList,
            query: String::new(),
            cursor_pos: 0,
            results: Vec::new(),
            selected_index: 0,
            preview_messages: Vec::new(),
            preview_selected_msg: 0,
            viewing_messages: Vec::new(),
            viewing_selected_msg: 0,
            all_sources,
            config,
            source_filter_index: 0,
            time_filter: TimeRange::All,
            filter_focus: FilterFocus::Source,
            should_quit: false,
            last_keystroke: Instant::now(),
            search_pending: false,
            embedding_init_pending: false,
            embedding_unavailable: false,
            status_message: None,
            sort_order: SortOrder::Relevance,
            export_path: String::new(),
            export_cursor: 0,
            total_sessions,
            total_messages,
            semantic_progress,
            background_status,
            semantic_last_refresh: Instant::now(),
            settings_selected: 0,
            list_state: ratatui::widgets::ListState::default(),
            list_rect: None,
            preview_rect: None,
            // Default OFF so users can drag-select text in the panels
            // immediately, like claude-history does. Ctrl+M re-enables
            // mouse nav (click-to-select row, panel-aware scroll) when
            // needed.
            mouse_capture_enabled: false,
            preview_expanded: std::collections::HashSet::new(),
            cluster_counts: std::collections::HashMap::new(),
            pending_resume: None,
            exec_on_exit: None,
            viewing_search_query: String::new(),
            viewing_search_input: None,
            viewing_search_input_cursor: 0,
            viewing_search_status: None,
            viewing_sanitized_lines: Vec::new(),
            viewing_match_cache: Vec::new(),
        };
        app.reset_search_defaults();
        app.update_scope_metrics(store);
        app.load_recent(store);
        app
    }

    pub fn source_filter_ids(&self) -> Option<Vec<String>> {
        let enabled = self.enabled_sources();
        if enabled.is_empty() {
            return None;
        }
        if self.source_filter_index == 0 {
            if enabled.len() == self.all_sources.len() {
                None
            } else {
                Some(enabled.into_iter().map(|(id, _)| id.clone()).collect())
            }
        } else {
            enabled.get(self.source_filter_index - 1).map(|(id, _)| vec![id.clone()])
        }
    }

    pub fn source_filter_label(&self) -> &str {
        if self.source_filter_index == 0 {
            if self.enabled_sources().len() == self.all_sources.len() { "ALL" } else { "DEFAULT" }
        } else {
            self.enabled_sources()
                .get(self.source_filter_index - 1)
                .map(|(_, label)| label.as_str())
                .unwrap_or("ALL")
        }
    }

    /// Cluster size for the visible row representing this session id.
    /// Returns 1 when the session is unique. Computed by `rebuild_clusters`.
    pub fn cluster_size_for(&self, session_id: &str) -> usize {
        self.cluster_counts.get(session_id).copied().unwrap_or(1)
    }

    /// Collapse `results` so rows sharing `(cwd, first-line-of-title)`
    /// appear once. The most recently-updated session is kept as the
    /// representative. `cluster_counts[representative.id] = cluster_size`
    /// drives the ×N badge in the UI.
    fn rebuild_clusters(&mut self) {
        use std::collections::HashMap;

        // Single pass: per bucket, remember (survivor_orig_index, total_count).
        let mut bucket_order: Vec<String> = Vec::new();
        let mut survivor_of: HashMap<String, usize> = HashMap::new(); // key → orig idx
        let mut count_of: HashMap<String, usize> = HashMap::new(); // key → cluster size

        for (i, r) in self.results.iter().enumerate() {
            let key = Self::cluster_key(&r.session);
            *count_of.entry(key.clone()).or_insert(0) += 1;
            let cur_ts = r.session.updated_at.unwrap_or(r.session.started_at);
            match survivor_of.get(&key).copied() {
                None => {
                    bucket_order.push(key.clone());
                    survivor_of.insert(key, i);
                }
                Some(prev) => {
                    let prev_ts = self.results[prev]
                        .session
                        .updated_at
                        .unwrap_or(self.results[prev].session.started_at);
                    if cur_ts > prev_ts {
                        survivor_of.insert(key, i);
                    }
                }
            }
        }

        // Pluck survivors out in bucket-first-seen order, then sort by
        // updated_at desc so recents float to the top regardless of how
        // the search engine ordered them.
        let mut taken: Vec<Option<crate::types::SearchResult>> =
            self.results.drain(..).map(Some).collect();
        let mut survivors: Vec<crate::types::SearchResult> = Vec::with_capacity(bucket_order.len());
        let mut counts: HashMap<String, usize> = HashMap::new();
        for key in &bucket_order {
            let idx = survivor_of[key];
            if let Some(r) = taken[idx].take() {
                counts.insert(r.session.id.clone(), count_of[key]);
                survivors.push(r);
            }
        }
        survivors.sort_by(|a, b| {
            let ta = a.session.updated_at.unwrap_or(a.session.started_at);
            let tb = b.session.updated_at.unwrap_or(b.session.started_at);
            tb.cmp(&ta)
        });
        self.results = survivors;
        self.cluster_counts = counts;
    }

    /// Bucket key: (cwd or "<no cwd>") + "::" + first 80 chars of the title
    /// (which is already the first real user message thanks to JSONL flag
    /// skipping in the claude_code adapter). Two sessions that share this
    /// key are visually indistinguishable to the user.
    fn cluster_key(session: &crate::types::Session) -> String {
        let cwd = session.directory.as_deref().unwrap_or("<no cwd>");
        let title_head: String =
            session.title.chars().filter(|c| !c.is_whitespace() || *c == ' ').take(80).collect();
        let title_head = title_head.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{cwd}::{title_head}")
    }

    pub fn source_label_for<'a>(&'a self, source_id: &'a str) -> &'a str {
        self.all_sources
            .iter()
            .find(|(id, _)| id == source_id)
            .map(|(_, label)| label.as_str())
            .unwrap_or(source_id)
    }

    pub fn load_recent(&mut self, store: &Store) {
        let source_ids = self.source_filter_ids();
        let recent = store.list_recent_sessions(200).unwrap_or_default();
        self.results = recent
            .into_iter()
            .filter(|session| self.session_matches_filters(session, source_ids.as_deref()))
            .map(|session| SearchResult { session, match_source: MatchSource::Fts, snippet: None })
            .collect();
        self.rebuild_clusters();
        self.selected_index = 0;
        self.panel_focus = PanelFocus::SessionList;
        self.load_preview(store);
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        self.status_message = None;

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        match self.mode {
            AppMode::Search => self.handle_search_key(key, store, engine, provider),
            AppMode::Viewing => self.handle_viewing_key(key),
            AppMode::ExportInput => self.handle_export_key(key),
            AppMode::Settings => self.handle_settings_key(key, store, engine, provider),
            AppMode::ConfirmResume => self.handle_confirm_resume_key(key),
        }
    }

    pub fn handle_scroll_up(&mut self, store: &Store) {
        match self.mode {
            AppMode::Search => match self.panel_focus {
                PanelFocus::SessionList => {
                    if !self.results.is_empty() && self.selected_index > 0 {
                        self.selected_index -= 1;
                        self.load_preview(store);
                    }
                }
                PanelFocus::Preview => {
                    if self.preview_selected_msg > 0 {
                        self.preview_selected_msg -= 1;
                    }
                }
            },
            AppMode::Viewing if self.viewing_selected_msg > 0 => {
                self.viewing_selected_msg -= 1;
            }
            AppMode::Settings if self.settings_selected > 0 => {
                self.settings_selected -= 1;
            }
            _ => {}
        }
    }

    pub fn handle_scroll_down(&mut self, store: &Store) {
        match self.mode {
            AppMode::Search => match self.panel_focus {
                PanelFocus::SessionList => {
                    if self.selected_index + 1 < self.results.len() {
                        self.selected_index += 1;
                        self.load_preview(store);
                    }
                }
                PanelFocus::Preview => {
                    if self.preview_selected_msg + 1 < self.preview_messages.len() {
                        self.preview_selected_msg += 1;
                    }
                }
            },
            AppMode::Viewing if self.viewing_selected_msg + 1 < self.viewing_messages.len() => {
                self.viewing_selected_msg += 1;
            }
            AppMode::Settings if self.settings_selected + 1 < self.settings_row_count() => {
                self.settings_selected += 1;
            }
            _ => {}
        }
    }

    /// Panel-aware mouse scroll. Picks the target panel based on cursor
    /// position rather than the focused panel — so hovering preview and
    /// scrolling moves the preview, not the active list.
    pub fn handle_mouse_scroll(
        &mut self,
        dir: crate::tui::event::ScrollDirection,
        col: u16,
        row: u16,
        store: &Store,
    ) {
        let in_rect = |r: Option<ratatui::layout::Rect>| -> bool {
            r.map(|r| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
                .unwrap_or(false)
        };
        let prev_focus = self.panel_focus;
        // Temporarily switch focus to the panel under the cursor, so the
        // existing scroll handlers do the right thing. Restore after.
        let target_focus = if in_rect(self.preview_rect) {
            PanelFocus::Preview
        } else if in_rect(self.list_rect) {
            PanelFocus::SessionList
        } else {
            prev_focus
        };
        self.panel_focus = target_focus;
        match dir {
            crate::tui::event::ScrollDirection::Up => self.handle_scroll_up(store),
            crate::tui::event::ScrollDirection::Down => self.handle_scroll_down(store),
        }
        self.panel_focus = prev_focus;
    }

    /// Mouse left-click. In the list pane → select the clicked row. In the
    /// preview pane → focus the preview AND set the message cursor to the
    /// clicked row when within range.
    pub fn handle_mouse_click(
        &mut self,
        col: u16,
        row: u16,
        store: &Store,
        engine: &crate::db::search::SearchEngine,
        provider: &mut Option<crate::embedding::EmbeddingProvider>,
    ) {
        let _ = (engine, provider);
        let in_rect = |r: Option<ratatui::layout::Rect>| -> bool {
            r.map(|r| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
                .unwrap_or(false)
        };
        if in_rect(self.list_rect) && self.mode == AppMode::Search {
            // Map screen row → list index. Each ListItem spans LIST_LINES_PER_ITEM
            // terminal rows (header + dimmed snippet), so a click on visual
            // row R corresponds to item `offset + (R - inner_top) / lines_per_item`.
            // Without the divide, clicks land roughly twice as far down the list.
            const LIST_LINES_PER_ITEM: u16 = 2;
            if let Some(rect) = self.list_rect {
                let inner_top = rect.y + 1; // border
                if row >= inner_top {
                    let row_in_list = (row - inner_top) / LIST_LINES_PER_ITEM;
                    let offset = self.list_state.offset();
                    let target = offset + row_in_list as usize;
                    if target < self.results.len() {
                        self.panel_focus = PanelFocus::SessionList;
                        self.selected_index = target;
                        self.load_preview(store);
                    }
                }
            }
        } else if in_rect(self.preview_rect) && self.mode == AppMode::Search {
            self.panel_focus = PanelFocus::Preview;
        }
    }

    fn handle_search_key(
        &mut self,
        key: KeyEvent,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.mode = AppMode::Settings;
            self.settings_selected = 0;
            return;
        }

        // Ctrl+E toggles expansion of the focused preview message — long
        // assistant replies otherwise collapse to PREVIEW_COLLAPSED_LINES
        // with a "(K more lines)" hint. Only meaningful when preview pane
        // is focused.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('e')
            && self.panel_focus == PanelFocus::Preview
        {
            let idx = self.preview_selected_msg;
            if self.preview_expanded.contains(&idx) {
                self.preview_expanded.remove(&idx);
            } else {
                self.preview_expanded.insert(idx);
            }
            return;
        }

        // Ctrl+M toggles mouse capture so the user can drag-select text in
        // the terminal natively. The main loop reads this flag each frame
        // and (re)issues EnableMouseCapture / DisableMouseCapture.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('m') {
            self.mouse_capture_enabled = !self.mouse_capture_enabled;
            self.status_message = Some(if self.mouse_capture_enabled {
                "mouse nav ON — click/scroll panes (drag-select disabled). Ctrl+M to toggle."
                    .to_string()
            } else {
                "mouse nav OFF — drag to select text. Ctrl+M to re-enable click/scroll.".to_string()
            });
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.start_resume_confirmation(ResumeOrigin::Search);
            return;
        }

        match key.code {
            // NOTE: `q` is intentionally NOT a quit binding in search mode.
            // Otherwise typing a query that starts with "q" (e.g. "query",
            // "queue", "qa") would exit the app on the first keystroke.
            // Esc handles the empty-query → quit path below.
            KeyCode::Esc => {
                if self.panel_focus == PanelFocus::Preview {
                    self.panel_focus = PanelFocus::SessionList;
                } else if !self.query.is_empty() {
                    self.query.clear();
                    self.cursor_pos = 0;
                    self.load_recent(store);
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char(c) if self.panel_focus == PanelFocus::SessionList => {
                self.query.insert(self.cursor_pos, c);
                self.cursor_pos += c.len_utf8();
                self.last_keystroke = Instant::now();
                self.search_pending = true;
            }
            KeyCode::Backspace
                if self.panel_focus == PanelFocus::SessionList && self.cursor_pos > 0 =>
            {
                let prev = self.query[..self.cursor_pos]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.query.replace_range(prev..self.cursor_pos, "");
                self.cursor_pos = prev;
                self.last_keystroke = Instant::now();
                self.search_pending = true;
            }
            KeyCode::Left => {
                if self.panel_focus == PanelFocus::Preview {
                    self.panel_focus = PanelFocus::SessionList;
                } else if self.cursor_pos > 0 {
                    self.cursor_pos = self.query[..self.cursor_pos]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
            }
            KeyCode::Right if self.panel_focus == PanelFocus::SessionList => {
                if self.cursor_pos < self.query.len() {
                    self.cursor_pos = self.query[self.cursor_pos..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| self.cursor_pos + i)
                        .unwrap_or(self.query.len());
                } else if !self.preview_messages.is_empty() {
                    self.panel_focus = PanelFocus::Preview;
                    self.preview_selected_msg = 0;
                }
            }
            KeyCode::Up => {
                self.handle_scroll_up(store);
            }
            KeyCode::Down => {
                self.handle_scroll_down(store);
            }
            KeyCode::Enter if !self.results.is_empty() => {
                self.enter_viewing(store);
            }
            KeyCode::Tab => {
                // Tab toggles which panel has focus (list ↔ preview).
                // Filter changes live in the Ctrl+S settings popup now —
                // Tab cycling those was too easy to overshoot.
                self.panel_focus = match self.panel_focus {
                    PanelFocus::SessionList => PanelFocus::Preview,
                    PanelFocus::Preview => PanelFocus::SessionList,
                };
                let _ = (engine, provider);
                let _ = store;
            }
            _ => {}
        }
    }

    fn handle_viewing_key(&mut self, key: KeyEvent) {
        if self.viewing_search_input.is_some() {
            self.handle_viewing_search_input(key);
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.start_resume_confirmation(ResumeOrigin::Viewing);
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Search;
                self.viewing_messages.clear();
                self.viewing_selected_msg = 0;
                self.viewing_search_query.clear();
                self.viewing_search_status = None;
                self.viewing_sanitized_lines.clear();
                self.viewing_match_cache.clear();
            }
            KeyCode::Up | KeyCode::Char('k') if self.viewing_selected_msg > 0 => {
                self.viewing_selected_msg -= 1;
            }
            KeyCode::Down | KeyCode::Char('j')
                if self.viewing_selected_msg + 1 < self.viewing_messages.len() =>
            {
                self.viewing_selected_msg += 1;
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.viewing_selected_msg = 0;
            }
            KeyCode::End | KeyCode::Char('G') if !self.viewing_messages.is_empty() => {
                self.viewing_selected_msg = self.viewing_messages.len() - 1;
            }
            KeyCode::Char('c') => {
                self.copy_current_message();
            }
            KeyCode::Char('e') => {
                self.start_export();
            }
            KeyCode::Char('/') => {
                self.viewing_search_input = Some(String::new());
                self.viewing_search_input_cursor = 0;
                self.viewing_search_status = None;
            }
            KeyCode::Char('n') => {
                self.jump_viewing_match(true);
            }
            KeyCode::Char('N') => {
                self.jump_viewing_match(false);
            }
            _ => {}
        }
    }

    fn handle_viewing_search_input(&mut self, key: KeyEvent) {
        let Some(input) = self.viewing_search_input.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.viewing_search_input = None;
                self.viewing_search_input_cursor = 0;
            }
            KeyCode::Enter => {
                let query = input.clone();
                self.viewing_search_input = None;
                self.viewing_search_input_cursor = 0;
                if query.is_empty() {
                    self.viewing_search_query.clear();
                    self.viewing_search_status = None;
                    self.viewing_match_cache.clear();
                    return;
                }
                self.viewing_search_query = query;
                self.recompute_viewing_matches();
                if self.viewing_match_cache.is_empty() {
                    self.viewing_search_status = Some("No match".to_string());
                    return;
                }
                self.viewing_search_status = None;
                self.jump_viewing_match(true);
            }
            KeyCode::Backspace if self.viewing_search_input_cursor > 0 => {
                let prev = input[..self.viewing_search_input_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                input.replace_range(prev..self.viewing_search_input_cursor, "");
                self.viewing_search_input_cursor = prev;
            }
            KeyCode::Left if self.viewing_search_input_cursor > 0 => {
                let prev = input[..self.viewing_search_input_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.viewing_search_input_cursor = prev;
            }
            KeyCode::Right if self.viewing_search_input_cursor < input.len() => {
                let next = input[self.viewing_search_input_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| self.viewing_search_input_cursor + i)
                    .unwrap_or(input.len());
                self.viewing_search_input_cursor = next;
            }
            KeyCode::Home => {
                self.viewing_search_input_cursor = 0;
            }
            KeyCode::End => {
                self.viewing_search_input_cursor = input.len();
            }
            KeyCode::Char(c) => {
                input.insert(self.viewing_search_input_cursor, c);
                self.viewing_search_input_cursor += c.len_utf8();
            }
            _ => {}
        }
    }

    fn recompute_viewing_matches(&mut self) {
        self.viewing_match_cache.clear();
        if self.viewing_search_query.is_empty() {
            return;
        }
        let needle = self.viewing_search_query.to_lowercase();
        for (i, msg_lines) in self.viewing_sanitized_lines.iter().enumerate() {
            if msg_lines.iter().any(|l| l.lower.contains(&needle)) {
                self.viewing_match_cache.push(i);
            }
        }
    }

    pub fn viewing_match_indices(&self) -> &[usize] {
        &self.viewing_match_cache
    }

    fn jump_viewing_match(&mut self, forward: bool) {
        if self.viewing_search_query.is_empty() || self.viewing_match_cache.is_empty() {
            if !self.viewing_search_query.is_empty() {
                self.viewing_search_status = Some("No match".to_string());
            }
            return;
        }
        let current = self.viewing_selected_msg;
        let next = if forward {
            self.viewing_match_cache
                .iter()
                .find(|&&i| i > current)
                .copied()
                .or_else(|| self.viewing_match_cache.first().copied())
        } else {
            self.viewing_match_cache
                .iter()
                .rev()
                .find(|&&i| i < current)
                .copied()
                .or_else(|| self.viewing_match_cache.last().copied())
        };
        if let Some(idx) = next {
            self.viewing_selected_msg = idx;
            self.viewing_search_status = None;
        }
    }

    fn start_resume_confirmation(&mut self, origin: ResumeOrigin) {
        let Some(result) = self.results.get(self.selected_index) else {
            return;
        };
        let session = &result.session;
        let Some(command) = resume_command_for(&session.source, &session.source_id) else {
            self.status_message = Some(format!("Resume not supported for {}", session.source));
            return;
        };
        self.pending_resume = Some(PendingResume {
            command,
            source_label: self.source_label_for(&session.source).to_string(),
            session_title: session.title.clone(),
            cwd: session.directory.clone(),
            origin,
        });
        self.mode = AppMode::ConfirmResume;
    }

    fn handle_confirm_resume_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if let Some(pending) = self.pending_resume.take() {
                    self.exec_on_exit = Some((pending.command, pending.cwd));
                    self.should_quit = true;
                } else {
                    self.mode = AppMode::Search;
                }
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let origin =
                    self.pending_resume.take().map(|p| p.origin).unwrap_or(ResumeOrigin::Search);
                self.mode = match origin {
                    ResumeOrigin::Search => AppMode::Search,
                    ResumeOrigin::Viewing => AppMode::Viewing,
                };
            }
            _ => {}
        }
    }

    fn handle_settings_key(
        &mut self,
        key: KeyEvent,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Search;
            }
            KeyCode::Up => self.handle_scroll_up(store),
            KeyCode::Down => self.handle_scroll_down(store),
            KeyCode::Left | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                self.update_setting(store, engine, provider);
            }
            _ => {}
        }
    }

    pub fn try_search(
        &mut self,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        self.refresh_semantic_progress(store);
        if self.embedding_init_pending {
            self.do_search(store, engine, provider);
            return;
        }
        if !self.search_pending {
            return;
        }
        if self.last_keystroke.elapsed().as_millis() < 150 {
            return;
        }
        self.search_pending = false;
        self.do_search(store, engine, provider);
    }

    fn do_search(
        &mut self,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        let query = self.query.trim();
        if query.is_empty() {
            self.load_recent(store);
            return;
        }

        // Mini build: no embedder available, never attempt semantic path.
        // FTS handles the query fine.
        #[cfg(not(feature = "semantic-search"))]
        {
            let _ = (provider, &self.embedding_init_pending, &self.embedding_unavailable);
            self.run_search(store, engine, None);
            return;
        }

        #[cfg(feature = "semantic-search")]
        {
            if !self.semantic_ready() {
                self.run_search(store, engine, None);
                return;
            }

            if provider.is_none() && !self.embedding_init_pending && !self.embedding_unavailable {
                self.status_message = Some("Loading embedding model...".to_string());
                self.embedding_init_pending = true;
                return;
            }
            if self.embedding_init_pending {
                self.embedding_init_pending = false;
                match EmbeddingProvider::new(false) {
                    Ok(p) => {
                        *provider = Some(p);
                        self.embedding_unavailable = false;
                        self.status_message = None;
                    }
                    Err(_) => {
                        self.embedding_unavailable = true;
                        self.status_message =
                            Some("Semantic unavailable — using text search only".to_string());
                    }
                }
            }
            let embedding = provider
                .as_ref()
                .and_then(|p| p.embed_query(&[query]).ok())
                .and_then(|mut e| if e.is_empty() { None } else { Some(e.swap_remove(0)) });

            self.run_search(store, engine, embedding.as_deref());
        }
    }

    fn run_search(&mut self, store: &Store, engine: &SearchEngine, embedding: Option<&[f32]>) {
        let query = self.query.trim();

        let filters = SearchFilters {
            sources: self.source_filter_ids(),
            time_range: self.time_filter,
            directory: None,
        };

        match engine.hybrid_search(query, embedding, &filters, 200, 3) {
            Ok(mut results) => {
                self.apply_sort(&mut results);
                self.results = results;
                self.rebuild_clusters();
                self.selected_index = 0;
                self.status_message = None;
            }
            Err(e) => {
                self.status_message = Some(format!("Search error: {e}"));
                self.results.clear();
            }
        }

        self.panel_focus = PanelFocus::SessionList;
        self.load_preview(store);
    }

    #[cfg(feature = "semantic-search")]
    fn semantic_ready(&self) -> bool {
        !self.embedding_unavailable
            && (self.semantic_progress.done_sessions > 0
                || self.semantic_progress.processing_sessions > 0)
    }

    fn refresh_semantic_progress(&mut self, store: &Store) {
        if self.semantic_last_refresh.elapsed().as_millis() < 750 {
            return;
        }
        self.update_scope_metrics(store);
        self.semantic_last_refresh = Instant::now();
    }

    fn update_scope_metrics(&mut self, store: &Store) {
        if let Ok((sessions, messages)) =
            store.stats_for_scope(self.source_filter_ids().as_deref(), self.time_filter)
        {
            self.total_sessions = sessions;
            self.total_messages = messages;
        }
        if let Ok(progress) =
            store.semantic_progress_for_scope(self.source_filter_ids().as_deref(), self.time_filter)
        {
            self.semantic_progress = progress;
        }
        if let Ok(status) = store.background_job_status("pipeline") {
            self.background_status = status;
        }
    }

    fn enabled_sources(&self) -> Vec<&(String, String)> {
        self.all_sources.iter().filter(|(id, _)| self.config.is_source_enabled(id)).collect()
    }

    fn reset_search_defaults(&mut self) {
        self.source_filter_index = 0;
        self.time_filter = self.config.sync_window.to_time_range();
    }

    fn settings_row_count(&self) -> usize {
        2 + self.all_sources.len()
    }

    fn update_setting(
        &mut self,
        store: &Store,
        engine: &SearchEngine,
        provider: &mut Option<EmbeddingProvider>,
    ) {
        if self.settings_selected == 0 {
            self.config.sync_window = self.config.sync_window.next();
        } else if self.settings_selected == 1 {
            self.sort_order = self.sort_order.next();
        } else if let Some((source_id, _)) = self.all_sources.get(self.settings_selected - 2) {
            if self.config.is_source_enabled(source_id) {
                let enabled_count =
                    self.all_sources.len().saturating_sub(self.config.disabled_sources.len());
                if enabled_count <= 1 {
                    self.status_message = Some("At least one source must stay enabled".to_string());
                    return;
                }
                self.config.disabled_sources.push(source_id.clone());
                self.config.disabled_sources.sort();
                self.config.disabled_sources.dedup();
            } else {
                self.config.disabled_sources.retain(|id| id != source_id);
            }
        }

        if let Err(e) = self.config.save() {
            self.status_message = Some(format!("Failed to save settings: {e}"));
            return;
        }

        self.reset_search_defaults();
        self.update_scope_metrics(store);
        self.status_message = Some("Settings saved".to_string());
        if self.query.is_empty() {
            self.load_recent(store);
        } else {
            self.do_search(store, engine, provider);
        }
    }

    fn session_matches_filters(
        &self,
        session: &crate::types::Session,
        sources: Option<&[String]>,
    ) -> bool {
        if let Some(sources) = sources
            && !sources.iter().any(|source| source == &session.source)
        {
            return false;
        }

        match self.time_filter.millis_ago() {
            Some(min_ts) => session.started_at >= min_ts,
            None => true,
        }
    }

    fn load_preview(&mut self, store: &Store) {
        self.preview_selected_msg = 0;
        self.preview_expanded.clear();
        if let Some(result) = self.results.get(self.selected_index) {
            match store.get_messages(&result.session.id) {
                Ok(msgs) => {
                    self.preview_messages = msgs.into_iter().take(30).collect();
                }
                Err(_) => {
                    self.preview_messages.clear();
                }
            }
        } else {
            self.preview_messages.clear();
        }
    }

    fn enter_viewing(&mut self, store: &Store) {
        if let Some(result) = self.results.get(self.selected_index)
            && let Ok(msgs) = store.get_messages(&result.session.id)
        {
            self.viewing_sanitized_lines = build_viewing_caches(&msgs);
            self.viewing_messages = msgs;
            self.viewing_selected_msg = 0;
            self.viewing_search_query.clear();
            self.viewing_search_input = None;
            self.viewing_search_input_cursor = 0;
            self.viewing_search_status = None;
            self.viewing_match_cache.clear();
            self.mode = AppMode::Viewing;
        }
    }

    fn copy_current_message(&mut self) {
        let text = self.viewing_messages.get(self.viewing_selected_msg).map(|m| m.content.clone());
        if let Some(text) = text {
            self.copy_to_clipboard(&text);
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
            ("pbcopy", &[])
        } else if cfg!(target_os = "windows") {
            ("clip.exe", &[])
        } else {
            ("xclip", &["-selection", "clipboard"])
        };

        match Command::new(cmd).args(args).stdin(Stdio::piped()).spawn() {
            Ok(mut child) => {
                if let Some(ref mut stdin) = child.stdin {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
                self.status_message = Some("Copied to clipboard".to_string());
            }
            Err(_) => {
                self.status_message = Some(format!("Failed to copy ({cmd} not found)"));
            }
        }
    }

    fn start_export(&mut self) {
        let session = match self.results.get(self.selected_index) {
            Some(r) => &r.session,
            None => return,
        };

        let safe_title: String = session
            .title
            .chars()
            .take(40)
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let source = self.source_label_for(&session.source);

        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string())
            .or_else(|| dirs::home_dir().map(|h| h.display().to_string()))
            .unwrap_or_default();
        self.export_path = format!("{cwd}/recall-{source}-{safe_title}.txt");
        self.export_cursor = self.export_path.len();
        self.mode = AppMode::ExportInput;
    }

    fn handle_export_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = AppMode::Viewing;
                self.export_path.clear();
            }
            KeyCode::Enter => {
                let path = self.export_path.clone();
                self.mode = AppMode::Viewing;
                self.do_export(&path);
                self.export_path.clear();
            }
            KeyCode::Char(c) => {
                self.export_path.insert(self.export_cursor, c);
                self.export_cursor += c.len_utf8();
            }
            KeyCode::Backspace if self.export_cursor > 0 => {
                let prev = self.export_path[..self.export_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                self.export_path.replace_range(prev..self.export_cursor, "");
                self.export_cursor = prev;
            }
            KeyCode::Left if self.export_cursor > 0 => {
                self.export_cursor = self.export_path[..self.export_cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
            KeyCode::Right if self.export_cursor < self.export_path.len() => {
                self.export_cursor = self.export_path[self.export_cursor..]
                    .char_indices()
                    .nth(1)
                    .map(|(i, _)| self.export_cursor + i)
                    .unwrap_or(self.export_path.len());
            }
            _ => {}
        }
    }

    fn do_export(&mut self, path: &str) {
        let session = match self.results.get(self.selected_index) {
            Some(r) => &r.session,
            None => return,
        };

        if let Some(parent) = std::path::Path::new(path).parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut content = String::new();
        content.push_str(&format!("Session: {}\n", session.title));
        content.push_str(&format!("Source: {}\n", session.source));
        if let Some(ref dir) = session.directory {
            content.push_str(&format!("Directory: {dir}\n"));
        }
        content.push_str(&format!(
            "Date: {}\n",
            chrono::DateTime::from_timestamp_millis(session.started_at)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        ));
        content.push_str(&format!("Messages: {}\n", self.viewing_messages.len()));
        content.push_str("\n---\n\n");

        for msg in &self.viewing_messages {
            let role = match msg.role {
                Role::User => "User",
                Role::Assistant => "Assistant",
            };
            content.push_str(&format!("## {role}\n\n{}\n\n", msg.content));
        }

        match std::fs::write(path, &content) {
            Ok(_) => {
                self.status_message = Some(format!("Exported to {path}"));
            }
            Err(e) => {
                self.status_message = Some(format!("Export failed: {e}"));
            }
        }
    }

    fn apply_sort(&self, results: &mut [SearchResult]) {
        if self.sort_order == SortOrder::Newest {
            results.sort_by_key(|b| std::cmp::Reverse(b.session.started_at));
        }
    }
}
