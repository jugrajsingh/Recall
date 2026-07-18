//! Tool-call envelope classification and summarization (F5a presentation-layer, F5d).
//!
//! `extract_content` in `adapters/claude_code.rs` flattens assistant `tool_use` blocks to
//! `[Name] {json}` on a single logical line. This module recognizes that shape, renders a
//! one-line human summary per tool, and strips the Read line-number gutter. It never touches
//! `types::Role`, the DB schema, or the export/protocol layer (D1).

const UNKNOWN_TOOL_JSON_MAX_CHARS: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ToolVisibility {
    #[default]
    Trunc,
    Full,
    Off,
}

impl ToolVisibility {
    pub(crate) fn next(self) -> Self {
        match self {
            ToolVisibility::Trunc => ToolVisibility::Full,
            ToolVisibility::Full => ToolVisibility::Off,
            ToolVisibility::Off => ToolVisibility::Trunc,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            ToolVisibility::Trunc => "tools:trunc",
            ToolVisibility::Full => "tools:full",
            ToolVisibility::Off => "tools:off",
        }
    }
}

/// A parsed `[Name] {json}` envelope. `raw` is the original single logical line.
pub(crate) struct ToolCall<'a> {
    pub(crate) name: &'a str,
    pub(crate) input: serde_json::Value,
    #[allow(dead_code)]
    pub(crate) raw: &'a str,
}

/// Parse a single logical line of the form `[Name] {json-object}`.
/// Returns None if the line is not a well-formed envelope.
pub(crate) fn parse_tool_call(line: &str) -> Option<ToolCall<'_>> {
    let rest = line.strip_prefix('[')?;
    let (name, rest) = rest.split_once("] ")?;
    if name.is_empty() {
        return None;
    }
    if !(rest.starts_with('{') && rest.ends_with('}')) {
        return None;
    }
    let input: serde_json::Value = serde_json::from_str(rest).ok()?;
    Some(ToolCall { name, input, raw: line })
}

/// One-line human summary, per-tool field extraction. Never panics.
pub(crate) fn summarize_tool_call(call: &ToolCall<'_>) -> String {
    match call.name {
        "Bash" => summarize_bash(call),
        "Read" => summarize_field(call, "file_path", "Read"),
        "Edit" => summarize_field(call, "file_path", "Edit"),
        "Write" => summarize_field(call, "file_path", "Write"),
        "MultiEdit" => summarize_field(call, "file_path", "MultiEdit"),
        "NotebookEdit" => summarize_field(call, "file_path", "NotebookEdit"),
        "Grep" => summarize_grep(call),
        "Glob" => summarize_field(call, "pattern", "Glob"),
        "TodoWrite" => summarize_todowrite(call),
        "Task" => summarize_field(call, "description", "Task"),
        "AskUserQuestion" => summarize_ask(call),
        "WebFetch" => summarize_field(call, "url", "WebFetch"),
        other => summarize_unknown(other, &call.input),
    }
}

fn str_field<'a>(input: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str())
}

fn summarize_field(call: &ToolCall<'_>, key: &str, label: &str) -> String {
    match str_field(&call.input, key) {
        Some(value) => format!("{label}: {value}"),
        None => summarize_unknown(label, &call.input),
    }
}

fn summarize_bash(call: &ToolCall<'_>) -> String {
    if let Some(command) = str_field(&call.input, "command") {
        return format!("Bash: {command}");
    }
    if let Some(description) = str_field(&call.input, "description") {
        return format!("Bash: {description}");
    }
    summarize_unknown("Bash", &call.input)
}

fn summarize_grep(call: &ToolCall<'_>) -> String {
    let Some(pattern) = str_field(&call.input, "pattern") else {
        return summarize_unknown("Grep", &call.input);
    };
    match str_field(&call.input, "path") {
        Some(path) => format!("Grep: {pattern} in {path}"),
        None => format!("Grep: {pattern}"),
    }
}

fn summarize_todowrite(call: &ToolCall<'_>) -> String {
    let count = call.input.get("todos").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
    format!("TodoWrite: {count} todos")
}

fn summarize_ask(call: &ToolCall<'_>) -> String {
    let question = call
        .input
        .get("questions")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|q| q.get("question"))
        .and_then(|v| v.as_str());
    match question {
        Some(q) => format!("Ask: {q}"),
        None => summarize_unknown("AskUserQuestion", &call.input),
    }
}

fn summarize_unknown(name: &str, input: &serde_json::Value) -> String {
    let json = serde_json::to_string(input).unwrap_or_default();
    format!("{name}: {}", truncate_chars(&json, UNKNOWN_TOOL_JSON_MAX_CHARS))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}\u{2026}")
}

/// True when EVERY non-empty logical line of `content` is a tool envelope.
pub(crate) fn content_is_all_tool_envelopes(content: &str) -> bool {
    let mut any = false;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if parse_tool_call(line).is_none() {
            return false;
        }
        any = true;
    }
    any
}

/// Plain-text pass used by BOTH preview and viewing (D5): replace each tool
/// envelope line with its one-line summary; pass other lines through unchanged.
pub(crate) fn summarize_envelopes(content: &str) -> String {
    content
        .lines()
        .map(|line| match parse_tool_call(line) {
            Some(call) => summarize_tool_call(&call),
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// For `ToolVisibility::Full`, the raw pretty-printed JSON body lines to append
/// beneath the one-line summary of an envelope. Empty when the line isn't an envelope.
pub(crate) fn full_envelope_lines(line: &str) -> Vec<String> {
    let Some(call) = parse_tool_call(line) else {
        return Vec::new();
    };
    serde_json::to_string_pretty(&call.input)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_gutter_end(line: &str) -> Option<usize> {
    let mut idx = 0usize;
    let mut chars = line.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        if c == ' ' {
            idx = i + c.len_utf8();
            chars.next();
        } else {
            break;
        }
    }
    let mut digit_end = idx;
    let mut has_digit = false;
    for (i, c) in line[idx..].char_indices() {
        if c.is_ascii_digit() {
            has_digit = true;
            digit_end = idx + i + c.len_utf8();
        } else {
            break;
        }
    }
    if !has_digit {
        return None;
    }
    match line[digit_end..].chars().next() {
        Some('\u{2192}') => Some(digit_end + '\u{2192}'.len_utf8()),
        Some('\t') => Some(digit_end + '\t'.len_utf8()),
        _ => None,
    }
}

/// If every non-empty line matches the Read line-number gutter shape
/// (`^\s*\d+(→|\t)`), strip the gutter from every line; else return input
/// unchanged (unanimity gate prevents false positives on prose).
pub(crate) fn strip_read_line_numbers(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut any_non_empty = false;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        any_non_empty = true;
        if read_gutter_end(line).is_none() {
            return content.to_string();
        }
    }
    if !any_non_empty {
        return content.to_string();
    }
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                return (*line).to_string();
            }
            let end = read_gutter_end(line).unwrap_or(0);
            line[end..].to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_extract_command_when_bash_envelope() {
        let call = parse_tool_call("[Bash] {\"command\":\"ls -la\"}").unwrap();
        assert_eq!(summarize_tool_call(&call), "Bash: ls -la");
    }

    #[test]
    fn test_should_extract_file_path_when_read_envelope() {
        let call = parse_tool_call("[Read] {\"file_path\":\"/a/b.rs\"}").unwrap();
        assert_eq!(summarize_tool_call(&call), "Read: /a/b.rs");
    }

    #[test]
    fn test_should_extract_pattern_and_path_when_grep_envelope() {
        let call = parse_tool_call("[Grep] {\"pattern\":\"foo\",\"path\":\"src/\"}").unwrap();
        assert_eq!(summarize_tool_call(&call), "Grep: foo in src/");
    }

    #[test]
    fn test_should_count_todos_when_todowrite_envelope() {
        let call = parse_tool_call("[TodoWrite] {\"todos\":[{},{},{}]}").unwrap();
        assert_eq!(summarize_tool_call(&call), "TodoWrite: 3 todos");
    }

    #[test]
    fn test_should_truncate_json_when_unknown_tool() {
        let long_value = "x".repeat(200);
        let line = format!("[Frobnicate] {{\"value\":\"{long_value}\"}}");
        let call = parse_tool_call(&line).unwrap();
        let summary = summarize_tool_call(&call);
        assert!(summary.starts_with("Frobnicate: "));
        assert!(summary.chars().count() <= "Frobnicate: ".len() + UNKNOWN_TOOL_JSON_MAX_CHARS + 1);
        assert!(summary.ends_with('\u{2026}'));
    }

    #[test]
    fn test_should_return_none_when_line_is_not_envelope() {
        assert!(parse_tool_call("hello world").is_none());
        assert!(parse_tool_call("[Bash] not-json").is_none());
    }

    #[test]
    fn test_should_strip_gutter_when_all_lines_are_read_numbered() {
        let input = "   1\u{2192}fn main() {\n   2\u{2192}}";
        assert_eq!(strip_read_line_numbers(input), "fn main() {\n}");
    }

    #[test]
    fn test_should_leave_content_when_gutter_shape_not_unanimous() {
        let input = "1\u{2192}code\nplain prose";
        assert_eq!(strip_read_line_numbers(input), input);
    }

    #[test]
    fn test_should_detect_all_envelopes_when_content_is_pure_tool_use() {
        assert!(content_is_all_tool_envelopes(
            "[Read] {\"file_path\":\"a\"}\n[Bash] {\"command\":\"x\"}"
        ));
        assert!(!content_is_all_tool_envelopes("[Read] {\"file_path\":\"a\"}\nprose line"));
    }

    #[test]
    fn test_should_advance_visibility_when_next() {
        assert_eq!(ToolVisibility::Trunc.next(), ToolVisibility::Full);
        assert_eq!(ToolVisibility::Full.next(), ToolVisibility::Off);
        assert_eq!(ToolVisibility::Off.next(), ToolVisibility::Trunc);
    }

    #[test]
    fn test_should_summarize_envelope_lines_when_mixed_with_prose() {
        let content = "[Bash] {\"command\":\"ls\"}\nplain text";
        assert_eq!(summarize_envelopes(content), "Bash: ls\nplain text");
    }

    #[test]
    fn test_should_extract_url_when_webfetch_envelope() {
        let call = parse_tool_call("[WebFetch] {\"url\":\"https://example.com\"}").unwrap();
        assert_eq!(summarize_tool_call(&call), "WebFetch: https://example.com");
    }

    #[test]
    fn test_should_extract_first_question_when_ask_user_question_envelope() {
        let call =
            parse_tool_call("[AskUserQuestion] {\"questions\":[{\"question\":\"Proceed?\"}]}")
                .unwrap();
        assert_eq!(summarize_tool_call(&call), "Ask: Proceed?");
    }
}
