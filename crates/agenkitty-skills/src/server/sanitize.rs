//! Untrusted-text hygiene for skill content (RFC-121 S2/S4).
//!
//! Skill frontmatter and bodies are author-supplied text that reaches prompts,
//! logs, and approval surfaces. Everything the catalog emits passes through
//! here first, mirroring the MCP family's T1 ("ANSI line-jumping" /
//! tool-poisoning) treatment: strip CSI/OSC/ESC sequences and control
//! characters, and bound every string on a UTF-8 boundary. The semantics match
//! `tools/mcp/common.rs` in the `agenkitty` crate; they are duplicated here —
//! not imported — because `agenkitty` depends on this crate, not the reverse.

/// Strip ANSI escape sequences and control characters, keeping `\n` and `\t`
/// so multi-line skill bodies stay readable.
pub fn sanitize_multiline(text: &str) -> String {
    sanitize(text, true)
}

/// Strip ANSI escape sequences and *all* control characters (newlines become
/// spaces). For single-line surfaces: index entries, listings, log fields.
pub fn sanitize_single_line(text: &str) -> String {
    sanitize(text, false)
}

fn sanitize(text: &str, keep_line_whitespace: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            // ESC introduces an escape sequence; consume it without emitting.
            match chars.peek() {
                Some('[') => {
                    // CSI: ESC [ … final byte in 0x40..=0x7E.
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] … terminated by BEL or ST (ESC \).
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c == '\u{07}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            if matches!(chars.peek(), Some('\\')) {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Other two-char escape (ESC X): drop ESC + the next char.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if ch == '\n' || ch == '\t' {
            if keep_line_whitespace {
                out.push(ch);
            } else if !out.ends_with(' ') {
                out.push(' ');
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        out.push(ch);
    }
    out
}

/// Truncate to `max_bytes` on a UTF-8 boundary, appending a visible marker
/// when bytes were dropped. Returns whether truncation happened. The output
/// never exceeds `max_bytes`: when the cap is too small to even hold the
/// marker, the text is cut bare — a bound that can overshoot is not a bound.
pub fn bound_text(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    const MARKER: &str = "…(truncated)…";
    let budget = if max_bytes > MARKER.len() {
        max_bytes - MARKER.len()
    } else {
        // No room for the marker within the cap; truncate bare.
        let mut end = max_bytes.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return (text[..end].to_string(), true);
    };
    let mut end = budget.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str(MARKER);
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_sequences() {
        let poisoned = "safe\u{1b}[2K\u{1b}[1A overwritten\u{1b}]0;title\u{07} end";
        assert_eq!(sanitize_multiline(poisoned), "safe overwritten end");
    }

    #[test]
    fn multiline_keeps_newline_and_tab_single_line_does_not() {
        let text = "a\nb\tc\u{0}d";
        assert_eq!(sanitize_multiline(text), "a\nb\tcd");
        assert_eq!(sanitize_single_line(text), "a b cd");
    }

    #[test]
    fn single_line_collapses_runs_of_line_whitespace() {
        assert_eq!(sanitize_single_line("a\n\n\tb"), "a b");
    }

    #[test]
    fn bound_text_truncates_on_char_boundary() {
        let (bounded, truncated) = bound_text(&"héllo wörld".repeat(4), 30);
        assert!(truncated);
        assert!(bounded.ends_with("…(truncated)…"));
        assert!(bounded.len() <= 30);
        let (kept, untruncated) = bound_text("short", 100);
        assert_eq!(kept, "short");
        assert!(!untruncated);
    }

    #[test]
    fn bound_text_never_exceeds_a_tiny_cap() {
        // A cap smaller than the marker truncates bare instead of overshooting.
        let (bounded, truncated) = bound_text("héllo wörld", 8);
        assert!(truncated);
        assert!(bounded.len() <= 8, "{} > 8", bounded.len());
        assert!("héllo wörld".starts_with(&bounded));
    }
}
