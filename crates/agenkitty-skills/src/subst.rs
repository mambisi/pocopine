//! `$ARGUMENTS` substitution with Claude Code semantics (RFC-121 § Argument
//! substitution).
//!
//! Supported placeholders: `$ARGUMENTS` (full string as typed),
//! `$ARGUMENTS[N]` / `$N` (0-based, shell-style quoting for multi-word
//! values), and `$name` for names declared in the skill's `arguments`
//! frontmatter (mapped by position). A single backslash directly before a
//! placeholder escapes it; a doubled backslash stays and the placeholder still
//! expands. When arguments are supplied but the body never mentions
//! `$ARGUMENTS`, the full string is appended as a trailing `ARGUMENTS: <value>`
//! line.

/// Apply substitution. `declared` is the skill's ordered `arguments` list.
pub(crate) fn substitute(body: &str, raw_args: &str, declared: &[String]) -> String {
    let positional = split_shell_style(raw_args);
    let mut out = String::with_capacity(body.len() + raw_args.len());
    let mut saw_arguments_placeholder = false;

    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            // Count the backslash run so `\$1` escapes while `\\$1` expands.
            let run_start = i;
            while i < bytes.len() && bytes[i] == b'\\' {
                i += 1;
            }
            let run_len = i - run_start;
            let followed_by_placeholder =
                parse_placeholder(&body[i..], declared, &positional, raw_args).is_some();
            if followed_by_placeholder && run_len % 2 == 1 {
                // Odd run: one backslash is the escape; the placeholder stays
                // literal.
                out.push_str(&"\\".repeat(run_len - 1));
                let (token_len, _) = parse_placeholder(&body[i..], declared, &positional, raw_args)
                    .expect("checked above");
                out.push_str(&body[i..i + token_len]);
                i += token_len;
            } else {
                out.push_str(&"\\".repeat(run_len));
            }
            continue;
        }
        if bytes[i] == b'$'
            && let Some((token_len, expansion)) =
                parse_placeholder(&body[i..], declared, &positional, raw_args)
        {
            if body[i..].starts_with("$ARGUMENTS") && !body[i..].starts_with("$ARGUMENTS[") {
                saw_arguments_placeholder = true;
            }
            out.push_str(&expansion);
            i += token_len;
            continue;
        }
        let ch = body[i..].chars().next().expect("in-bounds char");
        out.push(ch);
        i += ch.len_utf8();
    }

    if !raw_args.trim().is_empty() && !saw_arguments_placeholder {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\nARGUMENTS: ");
        out.push_str(raw_args);
        out.push('\n');
    }
    out
}

/// Recognize the placeholder at the start of `rest` (which begins with `$` or
/// this returns `None`). Returns `(token_length, expansion)`.
fn parse_placeholder(
    rest: &str,
    declared: &[String],
    positional: &[String],
    raw_args: &str,
) -> Option<(usize, String)> {
    let after = rest.strip_prefix('$')?;

    // `$ARGUMENTS[N]` before `$ARGUMENTS`.
    if let Some(indexed) = after.strip_prefix("ARGUMENTS[") {
        let close = indexed.find(']')?;
        let digits = &indexed[..close];
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            let index: usize = digits.parse().ok()?;
            let token_len = 1 + "ARGUMENTS[".len() + close + 1;
            let value = positional.get(index).cloned().unwrap_or_default();
            return Some((token_len, value));
        }
        return None;
    }
    if let Some(tail) = after.strip_prefix("ARGUMENTS") {
        // Reject `$ARGUMENTSX` — the token must end at a word boundary.
        if tail.chars().next().is_none_or(|ch| !is_ident_char(ch)) {
            return Some((1 + "ARGUMENTS".len(), raw_args.to_string()));
        }
        return None;
    }

    // `$N` shorthand.
    let digit_len = after.bytes().take_while(u8::is_ascii_digit).count();
    if digit_len > 0 {
        let next = after[digit_len..].chars().next();
        if next.is_none_or(|ch| !is_ident_char(ch)) {
            let index: usize = after[..digit_len].parse().ok()?;
            let value = positional.get(index).cloned().unwrap_or_default();
            return Some((1 + digit_len, value));
        }
        return None;
    }

    // `$name` for declared argument names, longest declared match first so
    // `$file-format` wins over a declared `$file`.
    let mut best: Option<(usize, usize)> = None; // (name_len, position)
    for (position, name) in declared.iter().enumerate() {
        if after.starts_with(name.as_str()) {
            let boundary_ok = after[name.len()..]
                .chars()
                .next()
                .is_none_or(|ch| !is_word_char(ch));
            if boundary_ok && best.is_none_or(|(len, _)| name.len() > len) {
                best = Some((name.len(), position));
            }
        }
    }
    if let Some((name_len, position)) = best {
        let value = positional.get(position).cloned().unwrap_or_default();
        return Some((1 + name_len, value));
    }
    None
}

/// Boundary for `$N` / `$ARGUMENTS`: `$0-foo` expands `$0`, `$ARGUMENTSX`
/// stays literal.
fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Boundary for declared `$name` matching: hyphens extend the word so a
/// declared `file` never swallows the prefix of `$file-format`.
fn is_word_char(ch: char) -> bool {
    is_ident_char(ch) || ch == '-'
}

/// Split an argument string shell-style: whitespace-separated, single and
/// double quotes group words, a backslash escapes the next character outside
/// single quotes.
fn split_shell_style(raw: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    current.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(&next) = chars.peek() {
                                if next == '"' || next == '\\' {
                                    chars.next();
                                    current.push(next);
                                } else {
                                    current.push('\\');
                                }
                            }
                        }
                        _ => current.push(c),
                    }
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    in_word = true;
                    current.push(next);
                }
            }
            _ if ch.is_whitespace() => {
                if in_word || !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                current.push(ch);
            }
        }
    }
    if in_word || !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_and_indexed_placeholders_expand() {
        let body = "All: $ARGUMENTS\nFirst: $0\nSecond: $ARGUMENTS[1]\n";
        let out = substitute(body, "\"hello world\" second", &[]);
        assert_eq!(
            out,
            "All: \"hello world\" second\nFirst: hello world\nSecond: second\n"
        );
    }

    #[test]
    fn named_arguments_map_by_position() {
        let declared = vec!["issue".to_string(), "format".to_string()];
        let out = substitute("Fix $issue as $format.", "42 markdown", &declared);
        // The trailing block still appends: the rule keys on `$ARGUMENTS`.
        assert_eq!(out, "Fix 42 as markdown.\n\nARGUMENTS: 42 markdown\n");
    }

    #[test]
    fn missing_positions_expand_empty() {
        let out = substitute("[$1]", "only-one", &[]);
        assert!(out.starts_with("[]"));
    }

    #[test]
    fn single_backslash_escapes_doubled_does_not() {
        let out = substitute("Costs \\$1.00 but $0 and \\\\$0.", "five", &[]);
        assert!(out.starts_with("Costs $1.00 but five and \\\\five."));
    }

    #[test]
    fn backslash_before_other_text_is_untouched() {
        let out = substitute("a \\n b", "", &[]);
        assert_eq!(out, "a \\n b");
    }

    #[test]
    fn args_without_placeholder_append_trailing_block() {
        let out = substitute("Deploy the app.", "prod --fast", &[]);
        assert_eq!(out, "Deploy the app.\n\nARGUMENTS: prod --fast\n");
    }

    #[test]
    fn indexed_only_body_still_gets_trailing_block() {
        // Claude Code keys the append rule on `$ARGUMENTS` alone.
        let out = substitute("First: $0", "one two", &[]);
        assert!(out.starts_with("First: one\n"));
        assert!(out.contains("ARGUMENTS: one two"));
    }

    #[test]
    fn no_args_appends_nothing() {
        assert_eq!(substitute("Body.", "", &[]), "Body.");
        assert_eq!(substitute("Body.", "   ", &[]), "Body.");
    }

    #[test]
    fn dollar_arguments_prefix_of_longer_word_is_literal() {
        let out = substitute("$ARGUMENTSX stays", "a", &[]);
        assert!(out.starts_with("$ARGUMENTSX stays"));
    }
}
