use chrono::Datelike;
use unicode_width::UnicodeWidthStr;

/// Shorten a filesystem path to fit within `max_width` terminal cells by
/// dropping middle segments and inserting `…/`. The first segment ("/" or
/// "~") and the final 1–2 segments are kept when possible so the reader
/// still sees what filesystem root the path lives under AND which leaf
/// directory the session was in.
///
/// Falls back to head-truncation with `…` suffix if even one segment
/// doesn't fit. Width-aware via UnicodeWidthStr (handles CJK / emoji).
pub fn shorten_path_middle(path: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(path) <= max_width {
        return path.to_string();
    }

    // Split preserving leading "/" — `~/foo/bar` → ["~", "foo", "bar"];
    // `/a/b/c` → ["", "a", "b", "c"]; treat empty first segment as root.
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() <= 2 {
        // No meaningful middle to drop — fall back to tail truncation.
        return tail_truncate(path, max_width);
    }

    let first = if segs[0].is_empty() { "/" } else { segs[0] };
    let ellipsis = "…";

    // Prefer "first/…/last2/last1"; if too wide, "first/…/last1".
    for keep_tail in [2usize, 1] {
        if segs.len() < keep_tail + 2 {
            continue;
        }
        let tail = &segs[segs.len() - keep_tail..];
        let candidate = format!(
            "{}{}/{}/{}",
            first,
            if first == "/" { "" } else { "/" },
            ellipsis,
            tail.join("/"),
        );
        if UnicodeWidthStr::width(candidate.as_str()) <= max_width {
            return candidate;
        }
    }

    // Last resort — just keep the very last segment with leading "…/".
    let last = segs.last().unwrap_or(&"");
    let candidate = format!("{ellipsis}/{last}");
    if UnicodeWidthStr::width(candidate.as_str()) <= max_width {
        candidate
    } else {
        tail_truncate(last, max_width)
    }
}

/// Truncate `s` from the head until it fits in `max_width` cells, prefixing
/// `…`. Used as a fallback when no segment-aware shortening fits.
fn tail_truncate(s: &str, max_width: usize) -> String {
    if max_width <= 1 {
        return "…".chars().take(max_width).collect();
    }
    if UnicodeWidthStr::width(s) <= max_width {
        return s.to_string();
    }
    let budget = max_width.saturating_sub(1);
    let mut out = String::new();
    let mut width = 0usize;
    // Walk from the END so we keep the rightmost part (most informative for paths).
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > budget {
            break;
        }
        out.push(c);
        width += cw;
    }
    let reversed: String = out.chars().rev().collect();
    format!("…{reversed}")
}

/// Humanise an absolute timestamp for the session list. Hybrid display
/// (ported from claude-history): recent events stay relative ("3h", "5d");
/// older events show a calendar date ("Mar 14" or "Mar 14 2025" if not the
/// current year). The breakpoint is 14 days — long enough that "5d" still
/// feels recent, short enough that "Mar 14" beats "1mo" for memorability.
pub fn format_age(started_at: i64) -> String {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let diff_ms = now_ms - started_at;
    let diff_mins = diff_ms / 60_000;
    let diff_hours = diff_mins / 60;
    let diff_days = diff_hours / 24;

    if diff_mins < 60 {
        if diff_mins < 1 { "now".to_string() } else { format!("{diff_mins}m") }
    } else if diff_hours < 24 {
        format!("{diff_hours}h")
    } else if diff_days < 14 {
        format!("{diff_days}d")
    } else {
        let Some(dt) = chrono::DateTime::from_timestamp_millis(started_at) else {
            return format!("{diff_days}d");
        };
        let local = dt.with_timezone(&chrono::Local);
        let now_local = chrono::Local::now();
        if local.year() == now_local.year() {
            local.format("%b %-d").to_string()
        } else {
            local.format("%b %-d %Y").to_string()
        }
    }
}

pub fn parse_since(s: &str) -> Option<i64> {
    let s = s.trim().to_lowercase();
    let (num_str, multiplier) = if let Some(n) = s.strip_suffix('d') {
        (n, 24 * 3600 * 1000i64)
    } else if let Some(n) = s.strip_suffix('w') {
        (n, 7 * 24 * 3600 * 1000i64)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 30 * 24 * 3600 * 1000i64)
    } else {
        return None;
    };
    let n: i64 = num_str.parse().ok()?;
    let now = chrono::Utc::now().timestamp_millis();
    Some(now - n * multiplier)
}

pub fn sanitize_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else if c.is_control() {
            continue;
        } else {
            out.push(c);
        }
    }
    out
}

pub fn format_message_time(ts: Option<i64>) -> String {
    let Some(ts) = ts else {
        return String::new();
    };
    chrono::DateTime::from_timestamp_millis(ts)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

pub fn f32_slice_to_bytes(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for &f in data {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

const TITLE_MAX_CHARS: usize = 80;
const TITLE_TRUNCATE_TAIL: usize = 77;

pub fn title_from_user_messages(user_contents: &[&str]) -> String {
    let chosen = user_contents
        .iter()
        .copied()
        .find(|c| !is_noise_first_message(c))
        .or_else(|| user_contents.first().copied())
        .unwrap_or("");

    let trimmed = chosen.trim();
    if trimmed.is_empty() {
        return "Untitled".to_string();
    }
    if trimmed.chars().count() > TITLE_MAX_CHARS {
        let truncated: String = trimmed.chars().take(TITLE_TRUNCATE_TAIL).collect();
        format!("{truncated}...")
    } else {
        trimmed.to_string()
    }
}

fn is_noise_first_message(content: &str) -> bool {
    // Minimal residual filter. The authoritative way to skip machinery
    // messages is via JSONL flags (isCompactSummary / isSidechain / isMeta)
    // applied at the source adapter — see src/adapters/claude_code.rs.
    // Only keep substring rules here for content that has no per-event
    // flag (some legacy / synthesized lines).
    let trimmed = content.trim_start();
    trimmed.starts_with("<command-message>")
        || trimmed.starts_with("<local-command-caveat>")
        || trimmed.starts_with("<local-command-stdout>")
        || trimmed.starts_with("# New session -")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_empty_input_returns_untitled() {
        assert_eq!(title_from_user_messages(&[]), "Untitled");
    }

    #[test]
    fn title_single_plain_message_returned_verbatim() {
        assert_eq!(title_from_user_messages(&["fix the parser bug"]), "fix the parser bug");
    }

    #[test]
    fn title_trims_whitespace() {
        assert_eq!(title_from_user_messages(&["  hello world  "]), "hello world");
    }

    #[test]
    fn title_long_message_is_truncated_with_ellipsis() {
        let long = "a".repeat(200);
        let result = title_from_user_messages(&[&long]);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 80);
    }

    #[test]
    fn shorten_path_returns_path_when_already_short() {
        let p = "/home/user/proj";
        assert_eq!(shorten_path_middle(p, 40), p);
    }

    #[test]
    fn shorten_path_keeps_root_and_last_two_segments() {
        let p = "/home/user/PycharmProjects/Github/some/deep/leaf-dir";
        let out = shorten_path_middle(p, 30);
        assert!(out.starts_with("/"), "preserve root: {out}");
        assert!(out.contains("…"), "contains ellipsis: {out}");
        assert!(out.ends_with("deep/leaf-dir"), "keeps last 2 segs: {out}");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 30);
    }

    #[test]
    fn shorten_path_falls_back_to_last_segment_when_very_narrow() {
        let p = "/home/user/PycharmProjects/Github/some/deep/leaf";
        let out = shorten_path_middle(p, 10);
        assert!(out.contains("…"), "narrow → ellipsis: {out}");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 10);
    }

    #[test]
    fn shorten_path_preserves_dotted_first_segment() {
        // Regression: `.claude-mem` previously got mangled by the decoder.
        // Now with cwd authoritative, this is the path we render, so the
        // shortener must NOT touch internal hyphens.
        let p = "/home/user/.claude-mem/observer-sessions/a/b/c";
        let out = shorten_path_middle(p, 40);
        // Hyphens stay literal — no slash substitution.
        assert!(!out.contains(".claude/mem"), "must not mangle hyphens: {out}");
        assert!(UnicodeWidthStr::width(out.as_str()) <= 40);
    }

    #[test]
    fn title_skips_command_message_noise() {
        let msgs = [
            "<command-message>ship</command-message>\n<command-name>/ship</command-name>",
            "actually implement the feature",
        ];
        assert_eq!(title_from_user_messages(&msgs), "actually implement the feature");
    }

    #[test]
    fn title_skips_local_command_caveat_noise() {
        let msgs = [
            "<local-command-caveat>Caveat: ignore this wrapper</local-command-caveat>",
            "real intent here",
        ];
        assert_eq!(title_from_user_messages(&msgs), "real intent here");
    }

    #[test]
    fn title_skips_opencode_new_session_header() {
        let msgs = [
            "# New session - 2026-04-08T03:29:50.987Z\n\n**Session ID:** ses_abc",
            "debug the sync pipeline",
        ];
        assert_eq!(title_from_user_messages(&msgs), "debug the sync pipeline");
    }

    #[test]
    fn title_skips_multiple_noise_messages_in_a_row() {
        let msgs = [
            "<command-message>ship</command-message>",
            "<command-message>review</command-message>",
            "explain the regression",
        ];
        assert_eq!(title_from_user_messages(&msgs), "explain the regression");
    }

    #[test]
    fn title_falls_back_to_first_when_all_are_noise() {
        let msgs = [
            "<command-message>ship</command-message>",
            "<command-message>review</command-message>",
        ];
        assert_eq!(title_from_user_messages(&msgs), "<command-message>ship</command-message>");
    }

    #[test]
    fn title_does_not_misclassify_plain_markdown_heading() {
        let msgs = ["# Design notes\nthinking about the search pipeline"];
        let result = title_from_user_messages(&msgs);
        assert!(result.starts_with("# Design notes"));
    }

    #[test]
    fn title_detects_noise_with_leading_whitespace() {
        let msgs = ["   <command-message>ship</command-message>", "real content"];
        assert_eq!(title_from_user_messages(&msgs), "real content");
    }
}
