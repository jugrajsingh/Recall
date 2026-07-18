//! Shared message-body shaping (F5h): the single place that turns a message's raw
//! `content` into pre-wrap logical `Line`s, applying tool-envelope summarization (D1/D5)
//! and, for the viewing pane, markdown styling (D4). Preview and viewing both derive
//! their bodies from `build_message_body` so the tool-summary layer never drifts apart.

use ratatui::text::Line;

use crate::tui::tool_render::{self, ToolVisibility};
use crate::types::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyMode {
    /// Sanitized logical lines, tool envelopes summarized, NO markdown. Preview (D5)
    /// deliberately does not call `build_message_body` at all today, since it already
    /// truncates and highlights via its own char/line-limited path; it only reuses
    /// `tool_render::summarize_envelopes` ahead of that truncation. `Plain` is kept as
    /// part of this module's public contract (see the TDD tests below) and is the
    /// route a future preview-through-`build_message_body` integration would take.
    #[allow(dead_code)]
    Plain,
    /// Markdown-styled lines, tool envelopes summarized, DoS-capped.
    Markdown,
}

/// Shape one message's content into pre-wrap logical Lines. Shared shaping core
/// for preview (Plain) and viewing (Markdown). Never panics.
pub(crate) fn build_message_body(
    content: &str,
    mode: BodyMode,
    visibility: ToolVisibility,
) -> Vec<Line<'static>> {
    if visibility == ToolVisibility::Off && tool_render::content_is_all_tool_envelopes(content) {
        return Vec::new();
    }
    let shaped = shape_tool_lines(content, visibility);
    let shaped = tool_render::strip_read_line_numbers(&shaped);
    match mode {
        BodyMode::Plain => {
            shaped.lines().map(|l| Line::raw(crate::utils::sanitize_line(l))).collect()
        }
        BodyMode::Markdown => crate::tui::markdown::render_markdown(&shaped),
    }
}

/// Replace each tool-envelope line with its one-line summary (and, for `Full`
/// visibility, the raw pretty-printed JSON body beneath it). Non-envelope lines
/// pass through unchanged.
fn shape_tool_lines(content: &str, visibility: ToolVisibility) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        match tool_render::parse_tool_call(line) {
            Some(call) => {
                out.push(tool_render::summarize_tool_call(&call));
                if visibility == ToolVisibility::Full {
                    out.extend(tool_render::full_envelope_lines(line));
                }
            }
            None => out.push(line.to_string()),
        }
    }
    out.join("\n")
}

/// Build the viewing Markdown body cache, one Vec<Line> per message.
pub(crate) fn build_viewing_bodies(
    msgs: &[Message],
    visibility: ToolVisibility,
) -> Vec<Vec<Line<'static>>> {
    msgs.iter().map(|m| build_message_body(&m.content, BodyMode::Markdown, visibility)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn test_should_summarize_tool_turn_when_plain_mode() {
        let lines = build_message_body(
            "[Bash] {\"command\":\"ls\"}",
            BodyMode::Plain,
            ToolVisibility::Trunc,
        );
        assert_eq!(lines_text(&lines), vec!["Bash: ls".to_string()]);
    }

    #[test]
    fn test_should_emit_no_lines_when_visibility_off_and_all_tools() {
        let lines =
            build_message_body("[Bash] {\"command\":\"ls\"}", BodyMode::Plain, ToolVisibility::Off);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_should_pass_prose_through_when_plain_mode() {
        let lines = build_message_body("hello\nworld", BodyMode::Plain, ToolVisibility::Trunc);
        assert_eq!(lines_text(&lines), vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn test_should_append_json_body_when_full_visibility() {
        let lines = build_message_body(
            "[Bash] {\"command\":\"ls\"}",
            BodyMode::Plain,
            ToolVisibility::Full,
        );
        let text = lines_text(&lines);
        assert_eq!(text[0], "Bash: ls");
        assert!(text.len() > 1);
        assert!(text.iter().any(|l| l.contains("\"command\"")));
    }

    #[test]
    fn test_should_render_markdown_when_markdown_mode() {
        let lines = build_message_body("# Title", BodyMode::Markdown, ToolVisibility::Trunc);
        assert_eq!(lines.len(), 1);
    }
}
