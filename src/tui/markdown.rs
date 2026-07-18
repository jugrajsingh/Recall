//! CommonMark rendering for the viewing pane (F5c): pulldown-cmark walker -> pre-wrap
//! `Vec<Line<'static>>`, syntect-highlighted fenced code, DoS caps (F9 P2).
//!
//! Output lines are UNWRAPPED logical lines; wrapping to the pane width is the caller's
//! job via `text_layout::wrap_spans_to_lines` so row-math and render stay lock-step (D4).

use std::ops::Range;
use std::sync::OnceLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use unicode_width::UnicodeWidthStr;

pub(crate) const MARKDOWN_INPUT_MAX_BYTES: usize = 128 * 1024;
pub(crate) const CODE_BLOCK_HIGHLIGHT_MAX_BYTES: usize = 64 * 1024;
pub(crate) const CODE_LINE_HIGHLIGHT_MAX_BYTES: usize = 8 * 1024;
pub(crate) const TABLE_MAX_COLS: usize = 24;
pub(crate) const TABLE_MAX_ROWS: usize = 200;

const HEADING_COLOR: Color = Color::Cyan;
const CODE_INLINE_COLOR: Color = Color::Yellow;
const CODE_BLOCK_PLAIN_COLOR: Color = Color::DarkGray;
const RULE_WIDTH: usize = 40;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    &THEME_SET.get_or_init(ThemeSet::load_defaults).themes["base16-eighties.dark"]
}

fn markdown_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_GFM);
    options
}

struct ListFrame {
    ordered: bool,
    counter: u64,
}

struct CodeBlockState {
    lang: Option<String>,
    raw: String,
}

struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    col_count: usize,
}

struct Walker {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    bold: u32,
    italic: u32,
    strike: u32,
    heading_level: Option<HeadingLevel>,
    list_stack: Vec<ListFrame>,
    code_block: Option<CodeBlockState>,
    table: Option<TableState>,
}

impl Walker {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            heading_level: None,
            list_stack: Vec::new(),
            code_block: None,
            table: None,
        }
    }

    fn active_style(&self) -> Style {
        let mut style = Style::default();
        if self.heading_level.is_some() {
            style = style.fg(HEADING_COLOR).add_modifier(Modifier::BOLD);
        }
        if self.bold > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn push_text(&mut self, text: &str) {
        let sanitized = crate::utils::sanitize_line(text);
        if sanitized.is_empty() {
            return;
        }
        self.current.push(Span::styled(sanitized, self.active_style()));
    }

    fn push_code_span(&mut self, text: &str) {
        let style = self.active_style().fg(CODE_INLINE_COLOR);
        self.current.push(Span::styled(crate::utils::sanitize_line(text), style));
    }

    fn flush_line(&mut self) {
        if self.current.is_empty() {
            return;
        }
        self.lines.push(Line::from(std::mem::take(&mut self.current)));
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        self.heading_level = Some(level);
        let hashes = "#".repeat(heading_level_num(level));
        self.current.push(Span::styled(format!("{hashes} "), self.active_style()));
    }

    fn end_heading(&mut self) {
        self.flush_line();
        self.heading_level = None;
    }

    fn start_item(&mut self) {
        let indent = "  ".repeat(self.list_stack.len().saturating_sub(1));
        let Some(frame) = self.list_stack.last_mut() else {
            return;
        };
        let bullet = if frame.ordered {
            let n = frame.counter;
            frame.counter += 1;
            format!("{indent}{n}. ")
        } else {
            format!("{indent}\u{2022} ")
        };
        self.current.push(Span::raw(bullet));
    }

    fn start_code_block(&mut self, kind: CodeBlockKind<'_>) {
        self.flush_line();
        self.code_block =
            Some(CodeBlockState { lang: code_block_language(kind), raw: String::new() });
    }

    fn end_code_block(&mut self) {
        let Some(state) = self.code_block.take() else {
            return;
        };
        self.lines.extend(render_code_block(&state.raw, state.lang.as_deref()));
    }

    fn start_table(&mut self, col_count: usize) {
        self.flush_line();
        self.table = Some(TableState {
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
            col_count,
        });
    }

    fn end_table(&mut self, source: &str, range: Range<usize>) {
        let Some(state) = self.table.take() else {
            return;
        };
        if state.col_count > TABLE_MAX_COLS || state.rows.len() > TABLE_MAX_ROWS {
            self.lines.extend(plain_fallback(&source[range]));
            return;
        }
        self.lines.extend(render_table_lines(&state.rows));
    }

    fn handle_code_block_event(&mut self, event: Event<'_>) {
        let Some(state) = self.code_block.as_mut() else {
            return;
        };
        match event {
            Event::End(TagEnd::CodeBlock) => self.end_code_block(),
            Event::Text(t) | Event::Code(t) => state.raw.push_str(&t),
            Event::SoftBreak | Event::HardBreak => state.raw.push('\n'),
            _ => {}
        }
    }

    fn handle_table_event(&mut self, event: Event<'_>, source: &str, range: Range<usize>) {
        let Some(state) = self.table.as_mut() else {
            return;
        };
        match event {
            Event::End(TagEnd::Table) => self.end_table(source, range),
            Event::Start(Tag::TableRow) | Event::Start(Tag::TableHead) => {
                state.current_row.clear();
            }
            Event::End(TagEnd::TableRow) | Event::End(TagEnd::TableHead) => {
                let row = std::mem::take(&mut state.current_row);
                state.rows.push(row);
            }
            Event::Start(Tag::TableCell) => state.current_cell.clear(),
            Event::End(TagEnd::TableCell) => {
                let cell = std::mem::take(&mut state.current_cell);
                state.current_row.push(cell);
            }
            Event::Text(t) | Event::Code(t) => {
                state.current_cell.push_str(&crate::utils::sanitize_line(&t));
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, event: Event<'_>, source: &str, range: Range<usize>) {
        if self.code_block.is_some() {
            self.handle_code_block_event(event);
            return;
        }
        if self.table.is_some() {
            self.handle_table_event(event, source, range);
            return;
        }
        match event {
            Event::Start(Tag::Heading { level, .. }) => self.start_heading(level),
            Event::End(TagEnd::Heading(_)) => self.end_heading(),
            Event::Start(Tag::Strong) => self.bold += 1,
            Event::End(TagEnd::Strong) => self.bold = self.bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => self.italic += 1,
            Event::End(TagEnd::Emphasis) => self.italic = self.italic.saturating_sub(1),
            Event::Start(Tag::Strikethrough) => self.strike += 1,
            Event::End(TagEnd::Strikethrough) => self.strike = self.strike.saturating_sub(1),
            Event::Code(t) => self.push_code_span(&t),
            Event::Start(Tag::CodeBlock(kind)) => self.start_code_block(kind),
            Event::Start(Tag::List(start)) => {
                self.list_stack
                    .push(ListFrame { ordered: start.is_some(), counter: start.unwrap_or(1) });
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
            }
            Event::Start(Tag::Item) => self.start_item(),
            Event::End(TagEnd::Item) => self.flush_line(),
            Event::Start(Tag::Table(aligns)) => self.start_table(aligns.len()),
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) => self.flush_line(),
            Event::Rule => {
                self.flush_line();
                self.lines.push(Line::raw("\u{2500}".repeat(RULE_WIDTH)));
            }
            Event::SoftBreak | Event::HardBreak => self.flush_line(),
            Event::Text(t) => self.push_text(&t),
            Event::Html(t) | Event::InlineHtml(t) => self.push_text(&t),
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line();
        if let Some(state) = self.code_block.take() {
            self.lines.extend(render_code_block(&state.raw, state.lang.as_deref()));
        }
        if self.lines.is_empty() {
            self.lines.push(Line::raw(String::new()));
        }
        self.lines
    }
}

fn heading_level_num(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn code_block_language(kind: CodeBlockKind<'_>) -> Option<String> {
    match kind {
        CodeBlockKind::Indented => None,
        CodeBlockKind::Fenced(info) => {
            let language = info.split_whitespace().next().unwrap_or("").trim();
            if language.is_empty() { None } else { Some(language.to_string()) }
        }
    }
}

fn plain_code_lines(code: &str) -> Vec<Line<'static>> {
    code.lines()
        .map(|l| {
            Line::from(Span::styled(
                crate::utils::sanitize_line(l),
                Style::default().fg(CODE_BLOCK_PLAIN_COLOR),
            ))
        })
        .collect()
}

fn render_code_block(code: &str, lang: Option<&str>) -> Vec<Line<'static>> {
    if code.is_empty() {
        return Vec::new();
    }
    if code.len() > CODE_BLOCK_HIGHLIGHT_MAX_BYTES {
        return plain_code_lines(code);
    }
    if code.lines().any(|line| line.len() > CODE_LINE_HIGHLIGHT_MAX_BYTES) {
        return plain_code_lines(code);
    }
    let syntax = lang
        .and_then(|token| syntax_set().find_syntax_by_token(token))
        .unwrap_or_else(|| syntax_set().find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, theme());
    code.lines()
        .map(|line| {
            // Sanitize BEFORE syntect ever sees the line: escapes/control bytes must
            // never reach the highlighter, not just the rendered output (D-security).
            let sanitized = crate::utils::sanitize_line(line);
            match highlighter.highlight_line(&sanitized, syntax_set()) {
                Ok(ranges) => highlighted_line(&ranges),
                Err(_) => Line::styled(sanitized, Style::default().fg(CODE_BLOCK_PLAIN_COLOR)),
            }
        })
        .collect()
}

fn highlighted_line(ranges: &[(syntect::highlighting::Style, &str)]) -> Line<'static> {
    let spans = ranges
        .iter()
        .map(|(style, text)| {
            Span::styled(text.to_string(), Style::default().fg(to_ratatui_color(style.foreground)))
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn to_ratatui_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn pad_cell(cell: &str, width: usize) -> String {
    let cell_width = UnicodeWidthStr::width(cell);
    let padding = width.saturating_sub(cell_width);
    format!("{cell}{}", " ".repeat(padding))
}

fn table_row_line(row: &[String], widths: &[usize]) -> Line<'static> {
    let mut s = String::from("\u{2502}");
    for (i, width) in widths.iter().enumerate() {
        let cell = row.get(i).map(String::as_str).unwrap_or("");
        s.push(' ');
        s.push_str(&pad_cell(cell, *width));
        s.push_str(" \u{2502}");
    }
    Line::raw(s)
}

fn table_border_line(widths: &[usize], left: char, mid: char, right: char) -> Line<'static> {
    let mut s = String::new();
    s.push(left);
    for (i, width) in widths.iter().enumerate() {
        s.push_str(&"\u{2500}".repeat(width + 2));
        if i + 1 < widths.len() {
            s.push(mid);
        }
    }
    s.push(right);
    Line::raw(s)
}

fn render_table_lines(rows: &[Vec<String>]) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; col_count];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(table_border_line(&widths, '\u{250c}', '\u{252c}', '\u{2510}'));
    for (i, row) in rows.iter().enumerate() {
        lines.push(table_row_line(row, &widths));
        if i == 0 && rows.len() > 1 {
            lines.push(table_border_line(&widths, '\u{251c}', '\u{253c}', '\u{2524}'));
        }
    }
    lines.push(table_border_line(&widths, '\u{2514}', '\u{2534}', '\u{2518}'));
    lines
}

fn plain_fallback(text: &str) -> Vec<Line<'static>> {
    text.lines().map(|l| Line::raw(crate::utils::sanitize_line(l))).collect()
}

/// Render CommonMark `text` to pre-wrap logical lines with styled spans.
/// Applies DoS caps (D3); any breach falls back to plain sanitized lines for
/// the affected unit. Never panics. Output lines are UNWRAPPED (wrapping is the
/// caller's job via wrap_spans_to_lines, so row-math and render stay lock-step).
pub(crate) fn render_markdown(text: &str) -> Vec<Line<'static>> {
    if text.len() > MARKDOWN_INPUT_MAX_BYTES {
        return plain_fallback(text);
    }
    // Strip ANSI/C1 escape sequences over the WHOLE raw document BEFORE parsing,
    // not only per-fragment below. pulldown-cmark can split one logical run of text
    // into multiple Event::Text fragments at markdown-syntax boundaries (e.g. at a
    // bare '[' while it speculatively looks ahead for a link), which would let a
    // CSI/OSC sequence survive a purely per-fragment sanitize by having its
    // introducer land in one fragment and its terminator in the next. Unlike
    // `sanitize_line`, `strip_ansi_sequences` never touches '\n' or other
    // whitespace, so running it over the whole document here cannot corrupt
    // markdown structure (headings/paragraphs/fences all key off '\n' and
    // indentation, both untouched). That structural risk is exactly why the
    // *other* sanitization layer below -- `sanitize_line`, which also drops
    // control chars and expands tabs -- stays scoped to individual fragments/
    // rendered lines rather than the whole document: applied whole-document it
    // would delete every '\n' and flatten the document into one line.
    let source = crate::utils::strip_ansi_sequences(text);
    let mut walker = Walker::new();
    for (event, range) in Parser::new_ext(&source, markdown_options()).into_offset_iter() {
        walker.handle_event(event, &source, range);
    }
    walker.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_bold_heading_when_atx_heading() {
        let lines = render_markdown("# Title");
        assert_eq!(lines.len(), 1);
        assert!(lines[0].spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn test_should_style_inline_code_when_backticked() {
        let lines = render_markdown("run `x` now");
        let has_code_style = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.fg == Some(CODE_INLINE_COLOR));
        assert!(has_code_style);
    }

    #[test]
    fn test_should_prefix_bullet_when_unordered_list() {
        let lines = render_markdown("- a\n- b");
        let text: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(text[0].starts_with('\u{2022}'));
        assert!(text[1].starts_with('\u{2022}'));
    }

    #[test]
    fn test_should_render_fence_as_code_lines_when_fenced_block() {
        let md = "```rust\nfn main() {}\nlet x = 1;\n```";
        let lines = render_markdown(md);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_should_draw_border_when_table_within_caps() {
        let md = "| a | b |\n| - | - |\n| 1 | 2 |\n";
        let lines = render_markdown(md);
        let text: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(text.iter().any(|l| l.contains('\u{2502}')));
        assert!(text.iter().any(|l| l.contains('\u{2500}')));
    }

    #[test]
    fn test_should_fallback_plain_when_input_exceeds_cap() {
        let input = "a".repeat(MARKDOWN_INPUT_MAX_BYTES + 1);
        let lines = render_markdown(&input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), input.as_str());
        assert_eq!(lines[0].spans[0].style, Style::default());
    }

    #[test]
    fn test_should_skip_syntect_when_code_line_exceeds_cap() {
        let long_line = "x".repeat(CODE_LINE_HIGHLIGHT_MAX_BYTES + 1);
        let md = format!("```\nshort\n{long_line}\n```");
        let lines = render_markdown(&md);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_should_fallback_plain_when_table_exceeds_col_cap() {
        let header: Vec<String> = (0..TABLE_MAX_COLS + 1).map(|i| format!("c{i}")).collect();
        let sep: Vec<String> = (0..TABLE_MAX_COLS + 1).map(|_| "-".to_string()).collect();
        let md = format!("| {} |\n| {} |\n", header.join(" | "), sep.join(" | "));
        let lines = render_markdown(&md);
        let text: Vec<String> =
            lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(!text.iter().any(|l| l.contains('\u{2502}')));
    }

    /// Walks every span of every produced Line and fails if any control character
    /// (C0, DEL, or C1 -- everything `char::is_control()` reports, which is exactly
    /// what `sanitize_line` is supposed to have dropped) survived into rendered
    /// span content.
    fn assert_no_control_chars(lines: &[Line<'static>]) {
        for line in lines {
            for span in &line.spans {
                assert!(
                    !span.content.chars().any(|c| c.is_control()),
                    "control char survived in span content: {:?}",
                    span.content
                );
            }
        }
    }

    fn all_span_text(lines: &[Line<'static>]) -> String {
        lines.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn test_should_strip_ansi_when_body_text_contains_escape() {
        let lines = render_markdown("hello \x1b[31mworld");
        assert_no_control_chars(&lines);
        assert_eq!(all_span_text(&lines), "hello world");
    }

    #[test]
    fn test_should_strip_control_chars_when_fenced_code_block_contains_escape() {
        let md = "```\n\x1b]0;evil\x07code\n```";
        let lines = render_markdown(md);
        assert_no_control_chars(&lines);
        assert_eq!(all_span_text(&lines), "code");
    }

    #[test]
    fn test_should_strip_control_chars_when_table_cell_contains_escape() {
        let md = "| a |\n| - |\n| \u{9b}31mcell |\n";
        let lines = render_markdown(md);
        assert_no_control_chars(&lines);
        let text = all_span_text(&lines);
        assert!(text.contains("cell"));
        assert!(!text.contains('\u{9b}'));
    }
}
