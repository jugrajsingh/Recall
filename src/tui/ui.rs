use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::db::search::TimeRange;
use crate::tui::app::{App, AppMode, PanelFocus, ResumeOrigin, SanitizedLine, SortOrder};
use crate::types::Role;

fn highlight_spans(text: &str, hay: &str, needle_lower: &str, base: Style) -> Vec<Span<'static>> {
    if needle_lower.is_empty() {
        return vec![Span::styled(text.to_string(), base)];
    }
    if hay.len() != text.len() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    let match_style =
        Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD);
    while cursor < text.len() {
        match hay[cursor..].find(needle_lower) {
            Some(rel) => {
                let start = cursor + rel;
                let end = start + needle_lower.len();
                if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                    spans.push(Span::styled(text[cursor..].to_string(), base));
                    break;
                }
                if start > cursor {
                    spans.push(Span::styled(text[cursor..start].to_string(), base));
                }
                spans.push(Span::styled(text[start..end].to_string(), match_style));
                cursor = end;
            }
            None => {
                spans.push(Span::styled(text[cursor..].to_string(), base));
                break;
            }
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), base));
    }
    spans
}

/// Humanise duration in minutes: "42m" if <60, "Nh Mm" otherwise.
fn format_duration_minutes(mins: u32) -> String {
    if mins < 60 { format!("{mins}m") } else { format!("{}h {}m", mins / 60, mins % 60) }
}

/// Build a short kebab-case-ish slug from the first user message — claude-
/// history-style short label like `lgtm-stack-deployment-tasks`. Takes up to
/// `max_words` meaningful tokens, lowercases, drops punctuation, joins with
/// `-`. Empty input → "(untitled)" so the column is never blank.
fn make_slug(title: &str, max_words: usize) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned
        .split_whitespace()
        .filter(|w| w.len() > 1 || w.chars().any(|c| c.is_alphanumeric()))
        .take(max_words)
        .collect();
    if words.is_empty() { "(untitled)".to_string() } else { words.join("-").to_lowercase() }
}

/// Pre-wrap a `Line` into one-or-more single-row Lines that EXACTLY match
/// what the terminal will render at `width`. Each output Line is guaranteed
/// to fit on a single terminal row, so callers can do precise scroll math
/// (each Line == 1 row). Falls back to the original Line when wrapping
/// produces no segments (empty content).
///
/// We use `textwrap` because ratatui's `Paragraph::wrap` algorithm is
/// word-boundary aware — `chars/width` estimates always underestimate. With
/// pre-wrap + `wrap(None)` rendering, scroll-to-selection lands exactly.
fn prewrap_line<'a>(line: Line<'a>, width: usize) -> Vec<Line<'a>> {
    if width == 0 {
        return vec![line];
    }
    // Concatenate span text for wrap-boundary calculation. Re-applying the
    // span styles after wrap would require splitting at byte indices; for
    // our use case (mostly mono-style body text) we re-apply the FIRST
    // span's style to every wrapped row. Header lines are short and rarely
    // wrap so the loss is invisible.
    let full: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if full.is_empty() {
        return vec![line];
    }
    let style = line.spans.first().map(|s| s.style).unwrap_or_default();
    let wrapped = textwrap::wrap(&full, width);
    if wrapped.is_empty() {
        return vec![line];
    }
    wrapped.into_iter().map(|cow| Line::from(Span::styled(cow.into_owned(), style))).collect()
}

pub fn render(f: &mut Frame, app: &mut App) {
    match app.mode {
        AppMode::Search => render_search(f, app),
        AppMode::Viewing => render_viewing(f, app),
        AppMode::ExportInput => {
            render_viewing(f, app);
            render_export_input(f, app);
        }
        AppMode::Settings => {
            render_search(f, app);
            render_settings(f, app);
        }
        AppMode::ConfirmResume => {
            match app.pending_resume.as_ref().map(|p| p.origin) {
                Some(ResumeOrigin::Viewing) => render_viewing(f, app),
                _ => render_search(f, app),
            }
            render_confirm_resume(f, app);
        }
    }
}

fn render_search(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    render_search_box(f, app, outer[0]);
    render_filters(f, app, outer[1]);

    let main_area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(outer[2]);

    render_result_list(f, app, main_area[0]);
    render_preview(f, app, main_area[1]);
    render_status_bar(f, app, outer[3]);
}

fn render_search_box(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" Recall ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let display_query =
        if app.query.is_empty() { "Type to search...".to_string() } else { app.query.clone() };

    let style = if app.query.is_empty() {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };

    let input = Paragraph::new(display_query).style(style).block(block);
    f.render_widget(input, area);

    if app.panel_focus == PanelFocus::SessionList {
        let cursor_x = area.x + 1 + UnicodeWidthStr::width(&app.query[..app.cursor_pos]) as u16;
        f.set_cursor_position((cursor_x, area.y + 1));
    }
}

fn render_filters(f: &mut Frame, app: &mut App, area: Rect) {
    let source_label = app.source_filter_label();
    let time_label = match app.time_filter {
        TimeRange::Today => "today",
        TimeRange::Week => "7d",
        TimeRange::Month => "30d",
        TimeRange::All => "all",
    };

    let sort_label = match app.sort_order {
        SortOrder::Relevance => "relevance",
        SortOrder::Newest => "newest",
    };

    let line = Line::from(vec![
        Span::styled("  Source: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{source_label}]"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Time: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{time_label}]"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  Sort: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("[{sort_label}]"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  (Ctrl+S to configure)", Style::default().fg(Color::DarkGray)),
    ]);

    f.render_widget(Paragraph::new(line), area);
}

fn render_result_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.panel_focus == PanelFocus::SessionList;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };
    let block = Block::default()
        .title(format!(" Sessions ({}) ", app.results.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    if app.results.is_empty() {
        let msg = "No results";
        let p = Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)).block(block);
        f.render_widget(p, area);
        return;
    }

    // claude-history-inspired 2-line rows:
    //   line 1: <project> · <slug>                ... [N msgs · ×K · age]
    //   line 2: <dimmed content snippet>
    // <project>  = basename of cwd, cyan + bold
    // <slug>     = first 6 meaningful words of the first user message (green)
    // <N msgs>   = session.message_count
    // <×K>       = cluster size badge when this row groups K duplicates
    // <age>      = humanised relative time
    // The snippet uses the full title so users see the actual opening prompt.
    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let s = &result.session;
            let selected = i == app.selected_index;

            // --- left header parts -----------------------------------------
            let project = s
                .directory
                .as_deref()
                .and_then(|d| d.trim_end_matches('/').rsplit('/').next())
                .filter(|p| !p.is_empty())
                .unwrap_or("(no cwd)")
                .to_string();
            // Label fallback chain — strongest signal wins:
            //   1. custom_title (from Claude /rename)
            //   2. summary (Claude auto-generated)
            //   3. derived slug from first user message
            let label = s
                .custom_title
                .as_deref()
                .map(|t| t.to_string())
                .or_else(|| s.summary.as_deref().map(|s| make_slug(s, 6)))
                .unwrap_or_else(|| make_slug(&s.title, 6));

            // --- right metadata parts --------------------------------------
            let age = crate::utils::format_age(s.started_at);
            let msgs_str = format!("{} msgs", s.message_count);
            let duration_str = s.duration_minutes.map(format_duration_minutes);
            let cluster_n = app.cluster_size_for(&s.id);
            let cluster_str =
                if cluster_n > 1 { format!(" · ×{cluster_n}") } else { String::new() };
            let right_text = match duration_str {
                Some(d) => format!("{msgs_str} · {d}{cluster_str} · {age}"),
                None => format!("{msgs_str}{cluster_str} · {age}"),
            };

            // pad so right_text sits flush with the right edge
            let left_text_w = project.chars().count() + 3 + label.chars().count();
            let right_w = right_text.chars().count();
            let pad_w = inner_width.saturating_sub(left_text_w + right_w).max(1);

            // ---- snippet body (line 2) ------------------------------------
            let snippet_budget = inner_width.saturating_sub(2);
            let snippet_raw = s.title.trim().replace('\n', " ");
            let snippet: String = snippet_raw.chars().take(snippet_budget).collect();
            let snippet = if snippet_raw.chars().count() > snippet_budget {
                format!("{}…", snippet.trim_end())
            } else {
                snippet
            };

            // Colours: brighter when selected so the highlight ribbon reads.
            let project_fg = if selected { Color::White } else { Color::Cyan };
            let slug_fg = if selected { Color::White } else { Color::Green };
            let meta_fg = if selected { Color::Gray } else { Color::DarkGray };

            let header_line = Line::from(vec![
                Span::styled(project, Style::default().fg(project_fg).add_modifier(Modifier::BOLD)),
                Span::styled(" · ", Style::default().fg(Color::DarkGray)),
                Span::styled(label, Style::default().fg(slug_fg)),
                Span::raw(" ".repeat(pad_w)),
                Span::styled(right_text, Style::default().fg(meta_fg)),
            ]);
            let snippet_line = Line::from(Span::styled(
                format!("  {snippet}"),
                Style::default().fg(if selected { Color::Gray } else { Color::DarkGray }),
            ));

            let lines = vec![header_line, snippet_line];
            if selected {
                ListItem::new(lines).style(Style::default().bg(Color::Rgb(30, 50, 70)))
            } else {
                ListItem::new(lines)
            }
        })
        .collect();

    // Use ratatui's stateful list so viewport offset auto-tracks selection.
    // Without this the cursor can slide off-screen on long lists (k9s feel).
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(Color::Cyan).add_modifier(Modifier::BOLD));
    app.list_state.select(Some(app.selected_index));
    app.list_rect = Some(area);
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_preview(f: &mut Frame, app: &mut App, area: Rect) {
    // Record the rect so mouse routing can tell whether the cursor is in
    // the preview pane and direct scroll/click events accordingly.
    app.preview_rect = Some(area);
    let focused = app.panel_focus == PanelFocus::Preview;
    let border_color = if focused { Color::Cyan } else { Color::DarkGray };

    let title = if let Some(result) = app.results.get(app.selected_index) {
        let dir = result.session.directory.as_deref().unwrap_or("<no cwd>");
        // Budget for the path = pane width minus the chrome (corners, " Preview ",
        // optional " [N/M] " counter, " — ", trailing space). Keep ≥10 so we
        // always show at least the leaf segment.
        let pos = app.preview_selected_msg + 1;
        let total = app.preview_messages.len();
        let counter_chrome = if focused {
            format!(" Preview [{pos}/{total}] — ").len()
        } else {
            " Preview — ".len()
        };
        let pane_width = area.width as usize;
        let budget = pane_width.saturating_sub(counter_chrome + 3).max(10);
        let short_dir = crate::utils::shorten_path_middle(dir, budget);
        if focused {
            format!(" Preview [{pos}/{total}] — {short_dir} ")
        } else {
            format!(" Preview — {short_dir} ")
        }
    } else {
        " Preview ".to_string()
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    if app.preview_messages.is_empty() {
        let p =
            Paragraph::new("No messages").style(Style::default().fg(Color::DarkGray)).block(block);
        f.render_widget(p, area);
        return;
    }

    // Pre-wrap content so each emitted Line == exactly one terminal row.
    // Eliminates the cumulative drift that broke selection-follows-viewport
    // at large message counts — every scroll target now matches the
    // rendered position 1:1.
    let inner_width = block.inner(area).width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_row: usize = 0;

    for (i, msg) in app.preview_messages.iter().enumerate() {
        let selected = focused && i == app.preview_selected_msg;
        let (prefix, color) = match msg.role {
            Role::User => ("User: ", Color::Cyan),
            Role::Assistant => ("Asst: ", Color::Green),
        };

        if selected {
            selected_row = lines.len();
        }

        let bg = if selected { Color::DarkGray } else { Color::Reset };

        let time_str = crate::utils::format_message_time(msg.timestamp);
        let mut header = vec![Span::styled(
            prefix,
            Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
        )];
        if !time_str.is_empty() {
            header.push(Span::styled(time_str, Style::default().fg(Color::DarkGray).bg(bg)));
        }
        let header_line = Line::from(header);
        lines.extend(prewrap_line(header_line, inner_width));

        // Collapse long messages by default; expand on Ctrl+E.
        const PREVIEW_COLLAPSED_LINES: usize = 6;
        let expanded = app.preview_expanded.contains(&i);
        let raw_lines: Vec<&str> = msg.content.lines().collect();
        let total = raw_lines.len();
        let take_n = if expanded { total } else { PREVIEW_COLLAPSED_LINES };
        let truncated = !expanded && total > take_n;
        for line in raw_lines.iter().take(take_n) {
            let line = crate::utils::sanitize_line(line);
            let body_line = Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(Color::White).bg(bg),
            ));
            lines.extend(prewrap_line(body_line, inner_width));
        }
        if truncated {
            let hint_style = if selected {
                Style::default().fg(Color::Yellow).bg(bg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray).bg(bg)
            };
            lines.push(Line::from(Span::styled(
                format!("  ... ({} more lines — Ctrl+E to expand)", total - take_n),
                hint_style,
            )));
        }
        // blank separator between messages
        lines.push(Line::from(""));
    }

    // Clamp the scroll so we never scroll past the last screenful — guarantees
    // the selected row is visible even when the cumulative pre-wrap was off
    // by a row due to ratatui internal padding.
    let inner_height = block.inner(area).height as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    let scroll = selected_row.saturating_sub(2).min(max_scroll) as u16;

    // wrap(None) — content is already pre-wrapped; Paragraph just consumes
    // one row per Line.
    let p = Paragraph::new(lines).block(block).scroll((scroll, 0));
    f.render_widget(p, area);
}

fn render_viewing(f: &mut Frame, app: &mut App) {
    let area = f.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let session_info = app
        .results
        .get(app.selected_index)
        .map(|r| {
            let s = &r.session;
            let dir = s.directory.as_deref().unwrap_or("<no cwd>");
            let count = app.viewing_messages.len();
            let pos = app.viewing_selected_msg + 1;
            // Chrome: title + " — " + " [pos/count] " + corners/padding.
            let chrome = s.title.len() + 4 + format!(" [{pos}/{count}] ").len() + 3;
            let budget = (area.width as usize).saturating_sub(chrome).max(12);
            let short_dir = crate::utils::shorten_path_middle(dir, budget);
            format!(" {} — {short_dir} [{pos}/{count}] ", s.title)
        })
        .unwrap_or_else(|| " Conversation ".to_string());

    let block = Block::default()
        .title(session_info)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    // Pre-wrap everything so 1 Line == 1 terminal row. Scroll math becomes
    // exact even at 1000+ messages.
    let inner_width = block.inner(outer[0]).width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_row: usize = 0;
    let needle_lower = app.viewing_search_query.to_lowercase();

    for (i, msg) in app.viewing_messages.iter().enumerate() {
        let selected = i == app.viewing_selected_msg;
        let (prefix, color) = match msg.role {
            Role::User => ("User", Color::Cyan),
            Role::Assistant => ("Assistant", Color::Green),
        };

        if selected {
            selected_row = lines.len();
        }

        let bg = if selected { Color::DarkGray } else { Color::Reset };

        let time_str = crate::utils::format_message_time(msg.timestamp);
        let mut header = vec![Span::styled(
            format!("── {prefix} ──"),
            Style::default().fg(color).bg(bg).add_modifier(Modifier::BOLD),
        )];
        if !time_str.is_empty() {
            header.push(Span::styled(
                format!("  {time_str}"),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }
        let header_line = Line::from(header);
        lines.extend(prewrap_line(header_line, inner_width));

        let body_style = Style::default().fg(Color::White).bg(bg);
        let empty: Vec<SanitizedLine> = Vec::new();
        let cached_lines = app.viewing_sanitized_lines.get(i).unwrap_or(&empty);
        for sl in cached_lines {
            let spans = highlight_spans(&sl.text, &sl.lower, &needle_lower, body_style);
            let body_line = Line::from(spans);
            lines.extend(prewrap_line(body_line, inner_width));
        }
        lines.push(Line::from(""));
    }

    let inner_height = block.inner(outer[0]).height as usize;
    let max_scroll = lines.len().saturating_sub(inner_height);
    let scroll = selected_row.saturating_sub(2).min(max_scroll) as u16;

    let p = Paragraph::new(lines).block(block).scroll((scroll, 0));
    f.render_widget(p, outer[0]);

    let help_spans = vec![
        Span::styled(" ↑/↓", Style::default().fg(Color::Yellow)),
        Span::styled(" messages  ", Style::default().fg(Color::DarkGray)),
        Span::styled("/", Style::default().fg(Color::Yellow)),
        Span::styled(" find  ", Style::default().fg(Color::DarkGray)),
        Span::styled("n/N", Style::default().fg(Color::Yellow)),
        Span::styled(" next/prev  ", Style::default().fg(Color::DarkGray)),
        Span::styled("c", Style::default().fg(Color::Yellow)),
        Span::styled(" copy  ", Style::default().fg(Color::DarkGray)),
        Span::styled("e", Style::default().fg(Color::Yellow)),
        Span::styled(" export  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
        Span::styled(" resume  ", Style::default().fg(Color::DarkGray)),
        Span::styled("Esc", Style::default().fg(Color::Yellow)),
        Span::styled(" back  ", Style::default().fg(Color::DarkGray)),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::styled(" quit", Style::default().fg(Color::DarkGray)),
    ];

    let status_line = if let Some(ref input) = app.viewing_search_input {
        Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(input.clone(), Style::default().fg(Color::White)),
        ])
    } else if let Some(ref msg) = app.status_message {
        Line::from(vec![Span::styled(format!(" {msg}"), Style::default().fg(Color::Green))])
    } else if let Some(ref note) = app.viewing_search_status {
        Line::from(vec![Span::styled(
            format!(" {note}: \"{}\"", app.viewing_search_query),
            Style::default().fg(Color::Red),
        )])
    } else if !app.viewing_search_query.is_empty() {
        let matches = app.viewing_match_indices();
        let total = matches.len();
        let current_pos =
            matches.iter().position(|&i| i == app.viewing_selected_msg).map(|n| n + 1).unwrap_or(0);
        let mut spans = help_spans.clone();
        spans.push(Span::styled(
            format!("  [{current_pos}/{total} \"{}\"]", app.viewing_search_query),
            Style::default().fg(Color::Yellow),
        ));
        Line::from(spans)
    } else {
        Line::from(help_spans)
    };

    if let Some(ref input) = app.viewing_search_input {
        let cursor_byte = app.viewing_search_input_cursor.min(input.len());
        let cursor_x = outer[1].x + 2 + UnicodeWidthStr::width(&input[..cursor_byte]) as u16;
        f.set_cursor_position((cursor_x, outer[1].y));
    }

    f.render_widget(Paragraph::new(status_line), outer[1]);
}

fn render_export_input(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let popup_height = 3u16;
    let y = area.height.saturating_sub(popup_height + 1);
    let popup_area = Rect::new(area.x, y, area.width, popup_height);

    let block = Block::default()
        .title(" Export to (Enter confirm, Esc cancel) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let input = Paragraph::new(app.export_path.as_str())
        .style(Style::default().fg(Color::White).bg(Color::Black))
        .block(block);

    f.render_widget(Clear, popup_area);
    f.render_widget(input, popup_area);

    let cursor_x =
        popup_area.x + 1 + UnicodeWidthStr::width(&app.export_path[..app.export_cursor]) as u16;
    f.set_cursor_position((cursor_x.min(popup_area.right() - 2), y + 1));
}

fn render_settings(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let width = area.width.min(70);
    let height = (app.all_sources.len() as u16 + 7).min(area.height.saturating_sub(2).max(6));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);

    let block = Block::default()
        .title(" Settings (Enter/Space toggle, Esc close) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let mut lines = Vec::new();
    let selected_style = Style::default().bg(Color::Yellow).fg(Color::Black);
    let normal_style = Style::default().fg(Color::White);

    // Visibility palette:
    //   row label (focused) → bright yellow + bold
    //   row label (idle)    → soft gray  (was DarkGray — unreadable on dark term)
    //   chip active         → yellow bg + black fg + bold + bracket frame [ x ]
    //   chip inactive       → light gray fg, no bg
    //   chip inactive (focused row) → bright white fg so the row stands out
    let label_focused = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let label_idle = Style::default().fg(Color::Gray);
    let chip_active =
        Style::default().bg(Color::Yellow).fg(Color::Black).add_modifier(Modifier::BOLD);
    let chip_idle = Style::default().fg(Color::Gray);
    let chip_idle_focus = Style::default().fg(Color::White);
    let bracket_active = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let separator = Style::default().fg(Color::DarkGray);

    let render_chip_row =
        |label: &'static str, options: &[(usize, &'static str, bool)], row_focused: bool| {
            let mut spans: Vec<Span> = vec![Span::styled(
                format!(" {label:<13}"),
                if row_focused { label_focused } else { label_idle },
            )];
            for (i, (_, text, active)) in options.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled("  ·  ", separator));
                }
                if *active {
                    spans.push(Span::styled("[ ", bracket_active));
                    spans.push(Span::styled((*text).to_string(), chip_active));
                    spans.push(Span::styled(" ]", bracket_active));
                } else {
                    spans.push(Span::styled(
                        format!("  {text}  "),
                        if row_focused { chip_idle_focus } else { chip_idle },
                    ));
                }
            }
            Line::from(spans)
        };

    // ---- Time Scope row ----
    let time_options: [(crate::config::SyncWindow, &str); 4] = [
        (crate::config::SyncWindow::Today, "today"),
        (crate::config::SyncWindow::Week, "7d"),
        (crate::config::SyncWindow::Month, "30d"),
        (crate::config::SyncWindow::All, "all"),
    ];
    let time_row: Vec<(usize, &'static str, bool)> = time_options
        .iter()
        .enumerate()
        .map(|(i, (v, l))| (i, *l, app.config.sync_window == *v))
        .collect();
    lines.push(render_chip_row("Time Scope:", &time_row, app.settings_selected == 0));

    // ---- Sort Order row ----
    let sort_options: [(crate::tui::app::SortOrder, &str); 2] = [
        (crate::tui::app::SortOrder::Relevance, "relevance"),
        (crate::tui::app::SortOrder::Newest, "newest"),
    ];
    let sort_row: Vec<(usize, &'static str, bool)> =
        sort_options.iter().enumerate().map(|(i, (v, l))| (i, *l, app.sort_order == *v)).collect();
    lines.push(render_chip_row("Sort Order:", &sort_row, app.settings_selected == 1));
    lines.push(Line::from(""));

    for (index, (source_id, label)) in app.all_sources.iter().enumerate() {
        let enabled = app.config.is_source_enabled(source_id);
        let prefix = if enabled { "[x]" } else { "[ ]" };
        let style = if app.settings_selected == index + 2 { selected_style } else { normal_style };
        lines.push(Line::from(Span::styled(format!(" {prefix} {label} ({source_id})"), style)));
    }

    let widget = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(Clear, popup);
    f.render_widget(widget, popup);
}

fn render_confirm_resume(f: &mut Frame, app: &mut App) {
    let Some(pending) = app.pending_resume.as_ref() else {
        return;
    };

    let area = f.area();
    let width = area.width.clamp(40, 76);
    let height: u16 = 9;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let popup = Rect::new(x, y, width, height);

    let block = Block::default()
        .title(" Resume session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .style(Style::default().bg(Color::Black));

    let title: String = pending.session_title.chars().take(width as usize - 10).collect();
    let command_text: String =
        pending.command.display().chars().take(width as usize - 14).collect();
    let cwd_text: String = pending
        .cwd
        .as_deref()
        .unwrap_or("-")
        .chars()
        .rev()
        .take(width as usize - 10)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(" Source:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                pending.source_label.clone(),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(title, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled(" Cwd:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(cwd_text, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled(" Command: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                command_text,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  [Y] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("confirm & exec     ", Style::default().fg(Color::White)),
            Span::styled("[N] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled("cancel", Style::default().fg(Color::White)),
        ]),
    ];

    let widget = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(Clear, popup);
    f.render_widget(widget, popup);
}

fn render_status_bar(f: &mut Frame, app: &mut App, area: Rect) {
    // Semantic indicator only matters in full builds. Mini has nothing
    // to embed and nothing to drain — hide the indicator entirely to
    // avoid showing a misleading "0/N" or "N/N" that the user can't act on.
    #[cfg(feature = "semantic-search")]
    let semantic_span = if app.semantic_progress.total_sessions > 0 {
        let mut text = format!(
            " [semantic {}/{}]",
            app.semantic_progress.done_sessions, app.semantic_progress.total_sessions
        );
        if app.semantic_progress.failed_sessions > 0 {
            text = format!(
                " [semantic {}/{}, {} failed]",
                app.semantic_progress.done_sessions,
                app.semantic_progress.total_sessions,
                app.semantic_progress.failed_sessions
            );
        }
        Some(Span::styled(text, Style::default().fg(Color::Blue)))
    } else {
        None
    };
    #[cfg(not(feature = "semantic-search"))]
    let semantic_span: Option<Span> = None;
    let _ = &app.semantic_progress; // suppress unused warning in mini

    let stats_span = Span::styled(
        format!(" [{} sessions, {} messages]", app.total_sessions, app.total_messages),
        Style::default().fg(Color::DarkGray),
    );

    let line = if let Some(ref msg) = app.status_message {
        let mut spans = vec![Span::styled(format!(" {msg}"), Style::default().fg(Color::Green))];
        if let Some(span) = semantic_span.clone() {
            spans.push(span);
        }
        spans.push(stats_span);
        Line::from(spans)
    } else {
        match app.panel_focus {
            PanelFocus::SessionList => {
                let mut spans = vec![
                    Span::styled(" ↑/↓", Style::default().fg(Color::Yellow)),
                    Span::styled(" sessions  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("→", Style::default().fg(Color::Yellow)),
                    Span::styled(" preview  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Enter", Style::default().fg(Color::Yellow)),
                    Span::styled(" detail  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Tab", Style::default().fg(Color::Yellow)),
                    Span::styled(" focus  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Ctrl+R", Style::default().fg(Color::Yellow)),
                    Span::styled(" resume  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Ctrl+S", Style::default().fg(Color::Yellow)),
                    Span::styled(" settings  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Yellow)),
                    Span::styled(" clear  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Yellow)),
                    Span::styled(" quit", Style::default().fg(Color::DarkGray)),
                ];
                if let Some(span) = semantic_span.clone() {
                    spans.push(span);
                }
                spans.push(stats_span);
                Line::from(spans)
            }
            PanelFocus::Preview => {
                let mut spans = vec![
                    Span::styled(" ↑/↓", Style::default().fg(Color::Yellow)),
                    Span::styled(" messages  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("←", Style::default().fg(Color::Yellow)),
                    Span::styled(" sessions  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Enter", Style::default().fg(Color::Yellow)),
                    Span::styled(" detail  ", Style::default().fg(Color::DarkGray)),
                    Span::styled("Esc", Style::default().fg(Color::Yellow)),
                    Span::styled(" back", Style::default().fg(Color::DarkGray)),
                ];
                if let Some(span) = semantic_span {
                    spans.push(span);
                }
                spans.push(stats_span);
                Line::from(spans)
            }
        }
    };
    f.render_widget(Paragraph::new(line), area);
}
