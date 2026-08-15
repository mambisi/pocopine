//! RFC-116 — report lexer-hostile text inside `poco!` bodies before cargo runs.
//!
//! A `poco!` body is bare HTML tokens, so rustc lexes it before any proc macro
//! sees it. Ordinary prose does not always survive that: `don't` becomes
//! `error: prefix 'don' is unknown`, and `©` becomes `unknown start of token:
//! \u{a9}`. Neither message mentions pocopine, names the template, or suggests
//! a fix — and no macro of ours can improve them, because the failure happens
//! upstream of macro expansion.
//!
//! The CLI is where that gap closes: it owns the build, so it can look first
//! and say what actually went wrong.
//!
//! **This scan is textual, not token-based, and that is the whole point.** The
//! files it must diagnose are exactly the files that do not lex, so
//! [`pocopine_template_parser::inline_scan`] — which tokenizes — reports
//! nothing on them. Brace counting over raw text is approximate, but an
//! approximate locator that works on broken input beats an exact one that
//! goes blind precisely when it is needed.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// One character that will not survive the Rust lexer.
struct Offender {
    line: usize,
    column: usize,
    /// How the offender reads in the report — usually the character itself,
    /// but `//` is shown whole, since half a comment marker explains nothing.
    shown: String,
    escape: String,
    reason: &'static str,
}

/// Scan a project's `src/` tree and fail the build if any `poco!` body holds
/// text the Rust lexer will reject.
pub fn check_project(project: &Path) -> Result<()> {
    let mut findings: Vec<(PathBuf, Vec<Offender>)> = Vec::new();
    let mut files = Vec::new();
    collect_rs(&project.join("src"), &mut files);
    files.sort();

    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut offenders = Vec::new();
        for body in find_bodies(&source) {
            offenders.extend(scan_body(&source, body));
        }
        if !offenders.is_empty() {
            findings.push((path, offenders));
        }
    }

    if findings.is_empty() {
        return Ok(());
    }

    let mut report = String::from(
        "inline templates contain text the Rust lexer cannot read.\n\n\
         A `poco!` body is HTML tokens, so rustc reads it before pocopine does. \
         Put the affected text in quotes — a string literal is one token, and \
         its contents land in the template unchanged:\n\n    \
         <p>\"Don't stop — © 2026\"</p>\n\n\
         or use the entity shown for each character below.\n",
    );
    let mut count = 0;
    for (path, offenders) in &findings {
        let shown = path.strip_prefix(project).unwrap_or(path);
        for offender in offenders {
            count += 1;
            report.push_str(&format!(
                "\n  {}:{}:{}\n      {} `{}` — write `{}` or quote the run\n",
                shown.display(),
                offender.line,
                offender.column,
                offender.reason,
                offender.shown,
                offender.escape,
            ));
        }
    }
    // No `pocopine fmt --fix` hint here: that command is RFC-117 and does not
    // exist yet. Pointing at it would send authors to an unknown subcommand.
    bail!("{report}\n{count} occurrence(s).");
}

/// The lexer-hostile tokens in a bare template body, as they would be shown
/// in a diagnostic. Empty means the body can be written as `poco!` tokens.
///
/// `pocopine fmt` asks this before inlining a `.poco` file, so "would the
/// build reject this body?" has one answer in the codebase rather than two
/// that can drift apart.
pub(crate) fn lexer_offenders(body: &str) -> Vec<String> {
    scan_body(body, 0..body.len())
        .into_iter()
        .map(|offender| offender.shown)
        .collect()
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !name.starts_with('.') && name != "target" {
                collect_rs(&path, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Byte ranges of `poco!` bodies, located by brace counting over raw text.
///
/// Deliberately naive: it must work on input rustc itself rejects, so it
/// cannot lean on a tokenizer. Over-reporting is prevented by the per-body
/// scan below, which skips quoted runs — the same thing that makes a body
/// legal in the first place.
fn find_bodies(source: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        // Rust comments and literals are not macro invocations, so the
        // `poco!` inside `let note = "poco! { … }";` must not be mistaken for
        // a template — reporting it would fail a build that compiles.
        if let Some(next) = skip_rust_trivia(source, index) {
            index = next;
            continue;
        }

        if is_ident_start(bytes[index]) {
            let start = index;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
            // Reading whole identifiers is also what keeps `my_poco!` out,
            // and lets `pocopine::poco!` in.
            if &source[start..index] == "poco"
                && let Some(body) = body_after_macro_name(source, index)
            {
                index = body.end + 1;
                out.push(body);
            }
            continue;
        }

        index += 1;
    }
    out
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Step over a Rust comment or literal starting at `index`, if one is there.
fn skip_rust_trivia(source: &str, index: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let rest = &source[index..];

    if rest.starts_with("//") {
        return Some(match source[index..].find('\n') {
            Some(offset) => index + offset + 1,
            None => bytes.len(),
        });
    }
    if rest.starts_with("/*") {
        // Rust block comments nest.
        let mut depth = 0usize;
        let mut cursor = index;
        while cursor + 1 < bytes.len() {
            if bytes[cursor] == b'/' && bytes[cursor + 1] == b'*' {
                depth += 1;
                cursor += 2;
            } else if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
                depth -= 1;
                cursor += 2;
                if depth == 0 {
                    return Some(cursor);
                }
            } else {
                cursor += 1;
            }
        }
        return Some(bytes.len());
    }
    if let Some(end) = raw_string_end(source, index) {
        return Some(end);
    }
    if bytes[index] == b'"' {
        let mut cursor = index + 1;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\\' => cursor += 2,
                b'"' => return Some(cursor + 1),
                _ => cursor += 1,
            }
        }
        return Some(bytes.len());
    }
    if bytes[index] == b'\'' {
        return char_literal_len(source, index).map(|length| index + length);
    }
    None
}

/// End of a raw (or byte-raw) string literal starting at `index`.
fn raw_string_end(source: &str, index: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = index;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hashes = cursor - hash_start;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let terminator = format!("\"{}", "#".repeat(hashes));
    match source[cursor + 1..].find(&terminator) {
        Some(offset) => Some(cursor + 1 + offset + terminator.len()),
        None => Some(bytes.len()),
    }
}

/// The body of a macro invocation whose name ended at `after_name`.
///
/// Whitespace and comments may sit between the name, the `!`, and the opening
/// delimiter — `poco ! { … }` is as valid as `poco!{ … }` — and any of the
/// three delimiter pairs is allowed, so `poco!( … )` is found too.
fn body_after_macro_name(source: &str, after_name: usize) -> Option<std::ops::Range<usize>> {
    let bytes = source.as_bytes();

    let bang = skip_space_and_comments(source, after_name);
    if bytes.get(bang) != Some(&b'!') {
        return None;
    }
    let open = skip_space_and_comments(source, bang + 1);
    let (opener, closer) = match bytes.get(open)? {
        b'{' => (b'{', b'}'),
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        _ => return None,
    };

    let mut depth = 0usize;
    let mut index = open;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            match byte {
                b'\\' => index += 1, // step over the escaped byte
                b'"' => in_string = false,
                _ => {}
            }
            index += 1;
            continue;
        }
        match byte {
            // Delimiters inside a quoted run are template text, not Rust
            // nesting: `<p title="}">` is one string token, and counting its
            // `}` would end the body early and hide everything after it.
            b'"' => in_string = true,
            b'\'' => {
                if let Some(length) = char_literal_len(source, index) {
                    index += length;
                    continue;
                }
            }
            b if b == opener => depth += 1,
            b if b == closer => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1)..index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None // unbalanced — rustc will say so
}

fn skip_space_and_comments(source: &str, mut index: usize) -> usize {
    let bytes = source.as_bytes();
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let rest = &source[index.min(source.len())..];
        if rest.starts_with("//") || rest.starts_with("/*") {
            match skip_rust_trivia(source, index) {
                Some(next) if next > index => index = next,
                _ => return index,
            }
        } else {
            return index;
        }
    }
}

/// Byte length of a Rust lifetime token starting at `index`, if there is one.
///
/// A lifetime is a real token, so prose that happens to look like one lexes
/// fine: `<p>'tis the season</p>` and `<p>the 'static lifetime</p>` both
/// compile. `'hello'` is not a lifetime but an over-long character literal,
/// which does not lex — so a closing quote after the run disqualifies it.
fn lifetime_len(source: &str, index: usize) -> Option<usize> {
    let rest = source.get(index..)?;
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '\'' {
        return None;
    }
    let (_, first) = chars.next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    let mut end = 1 + first.len_utf8();
    for (offset, character) in chars {
        if character.is_alphanumeric() || character == '_' {
            end = offset + character.len_utf8();
        } else if character == '\'' {
            return None; // a character-literal attempt, not a lifetime
        } else {
            break;
        }
    }
    Some(end)
}

/// Byte length of the Rust character literal starting at `index`, if any.
///
/// `'x'` and `'\n'` are single tokens the lexer accepts, so neither their
/// quotes nor a brace inside them is template text. A bare apostrophe — the
/// `'` in `don't` — yields `None`, which is what keeps it reportable.
fn char_literal_len(source: &str, index: usize) -> Option<usize> {
    let rest = source.get(index..)?;
    let mut chars = rest.char_indices();
    if chars.next()?.1 != '\'' {
        return None;
    }
    let (_, second) = chars.next()?;
    if second == '\'' {
        return None; // `''` is not a literal
    }
    if second == '\\' {
        // An escape (`'\n'`, `'\u{1F600}'`): the closing quote is near.
        for (offset, character) in chars {
            if character == '\'' {
                return Some(offset + 1);
            }
            if offset > 12 {
                return None;
            }
        }
        return None;
    }
    let (offset, third) = chars.next()?;
    (third == '\'').then_some(offset + 1)
}

/// Characters inside one body that the lexer will reject.
///
/// Quoted runs are skipped: quoting is the sanctioned escape hatch, and
/// attribute values are quoted too, so this also leaves markup alone.
fn scan_body(source: &str, body: std::ops::Range<usize>) -> Vec<Offender> {
    let text = &source[body.clone()];
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;

    while index < chars.len() {
        let (offset, character) = chars[index];

        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if character == '"' {
            in_string = true;
            index += 1;
            continue;
        }

        // An apostrophe is only a problem when it is not a character literal.
        // `{{ 'x' }}` lexes fine and is a valid expression, so reporting it
        // would fail a build that compiles. But `don't` is a reserved prefix
        // rather than a literal, so a `'` straight after an identifier
        // character stays reportable.
        if character == '\'' {
            let after_identifier = index > 0 && {
                let previous = chars[index - 1].1;
                previous.is_alphanumeric() || previous == '_'
            };
            // A character literal or a lifetime is one token that lexes
            // cleanly, so neither is reportable — `{{ 'x' }}` and prose like
            // `'tis the season` both compile.
            let token = (!after_identifier)
                .then(|| char_literal_len(text, offset).or_else(|| lifetime_len(text, offset)))
                .flatten();
            if let Some(length) = token {
                let end = offset + length;
                while index < chars.len() && chars[index].0 < end {
                    index += 1;
                }
                continue;
            }
        }

        let next = chars.get(index + 1).map(|(_, c)| *c);
        let (reason, shown) = match character {
            '\'' => (Some("apostrophe"), "'".to_string()),
            '`' => (Some("backtick"), "`".to_string()),
            '\\' => (Some("backslash"), "\\".to_string()),
            // `//` opens a Rust comment and swallows the rest of the line.
            '/' if next == Some('/') => (Some("comment marker"), "//".to_string()),
            // Letters of any script are valid identifier characters and lex
            // fine (`café`, `日本語`); non-ASCII *symbols* do not.
            c if !c.is_ascii() && !c.is_alphanumeric() => (Some("symbol"), c.to_string()),
            _ => (None, String::new()),
        };

        if let Some(reason) = reason {
            let (line, column) = line_col(source, body.start + offset);
            out.push(Offender {
                line,
                column,
                shown,
                escape: entity_for(character),
                reason,
            });
        }
        index += 1;
    }
    out
}

/// A named HTML entity where one is well known, else numeric.
fn entity_for(character: char) -> String {
    match character {
        '\'' => "&#39;".into(),
        '`' => "&#96;".into(),
        '\\' => "&#92;".into(),
        '/' => "&#47;".into(),
        '—' => "&mdash;".into(),
        '–' => "&ndash;".into(),
        '…' => "&hellip;".into(),
        '©' => "&copy;".into(),
        '®' => "&reg;".into(),
        '™' => "&trade;".into(),
        '·' => "&middot;".into(),
        '×' => "&times;".into(),
        '€' => "&euro;".into(),
        '£' => "&pound;".into(),
        '←' => "&larr;".into(),
        '→' => "&rarr;".into(),
        '‹' => "&lsaquo;".into(),
        '›' => "&rsaquo;".into(),
        '“' => "&ldquo;".into(),
        '”' => "&rdquo;".into(),
        '‘' => "&lsquo;".into(),
        '’' => "&rsquo;".into(),
        other => format!("&#{};", other as u32),
    }
}

fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rfind('\n')
        .map(|nl| before[nl + 1..].chars().count() + 1)
        .unwrap_or(before.chars().count() + 1);
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offenders(source: &str) -> Vec<String> {
        find_bodies(source)
            .into_iter()
            .flat_map(|body| scan_body(source, body))
            .map(|o| o.shown)
            .collect()
    }

    #[test]
    fn finds_bodies_in_source_that_does_not_tokenize() {
        // Why this scan is textual: a symbol like `©` is not a token at all,
        // so the token-based scanner goes blind on exactly the file that
        // needs diagnosing. Only a text scan can still locate the body.
        let source = "const T: X = poco! { <p>© 2026</p> };";
        assert!(
            pocopine_template_parser::inline_scan::scan_inline_templates(source).is_empty(),
            "precondition: this source does not tokenize"
        );
        assert_eq!(offenders(source), vec!["©"]);
    }

    #[test]
    fn catches_apostrophes_that_tokenize_but_rustc_rejects() {
        // `don't` *does* tokenize — as `don` plus the lifetime `'t` — so the
        // token scanner sees this file fine. rustc still rejects it as a
        // reserved prefix, which is why the lint checks characters rather
        // than trusting tokenization to have caught everything.
        let source = "const T: X = poco! { <p>don't stop</p> };";
        assert!(
            !pocopine_template_parser::inline_scan::scan_inline_templates(source).is_empty(),
            "precondition: this source does tokenize"
        );
        assert_eq!(offenders(source), vec!["'"]);
    }

    #[test]
    fn reports_typographic_symbols() {
        let source = "const T: X = poco! { <p>© 2026 · ⌘K</p> };";
        assert_eq!(offenders(source), vec!["©", "·", "⌘"]);
    }

    #[test]
    fn letters_of_any_script_are_fine() {
        let source = "const T: X = poco! { <p>café 日本語 naïve</p> };";
        assert!(offenders(source).is_empty());
    }

    #[test]
    fn quoted_runs_are_the_escape_hatch_and_are_skipped() {
        let source = r#"const T: X = poco! { <p>"Don't stop — © 2026"</p> };"#;
        assert!(offenders(source).is_empty());
    }

    #[test]
    fn attribute_values_are_left_alone() {
        let source = r#"const T: X = poco! { <a href="//cdn.x/a" title="a—b">ok</a> };"#;
        assert!(offenders(source).is_empty());
    }

    #[test]
    fn catches_a_rust_comment_marker_in_text() {
        let source = "const T: X = poco! { <p>see //cdn.example.com</p> };";
        assert_eq!(offenders(source), vec!["//"]);
    }

    #[test]
    fn a_comment_marker_inside_interpolation_is_still_reported() {
        // Reviewers have suggested suppressing `//` inside `{{ }}` on the
        // grounds that an expression may carry a comment. It cannot, in
        // either direction, so the report stays:
        //
        //   * on one line, `//` swallows the closing `}}` and the template
        //     fails to lex at all ("unclosed delimiter");
        //   * across lines it does lex, but `pocopine-expr` rejects `/`
        //     ("arithmetic operator `/` is not supported in pine-expr").
        //
        // Suppressing here would hide a template that cannot work either way.
        // A `//` inside a *string* is already skipped, which covers URLs.
        let source = "const T: X = poco! { <p>{{ label // note }}</p> };";
        assert_eq!(offenders(source), vec!["//"]);

        let with_url = r#"const T: X = poco! { <p>{{ "https://x" }}</p> };"#;
        assert!(offenders(with_url).is_empty(), "a URL in a string is fine");
    }

    #[test]
    fn ignores_a_macro_whose_name_merely_ends_in_poco() {
        let source = "const T: X = my_poco! { <p>don't</p> };";
        assert!(find_bodies(source).is_empty());
    }

    #[test]
    fn ignores_an_invocation_inside_a_rust_string_or_comment() {
        // Each of these compiles. Treating the text as a template would fail
        // a valid build, which is the worst outcome for a pre-build check.
        for source in [
            r#"const NOTE: &str = "poco! { <p>don't</p> }";"#,
            "// poco! { <p>don't</p> }",
            "/* poco! { <p>don't</p> } */",
            r##"const NOTE: &str = r#"poco! { <p>don't</p> }"#;"##,
            "/* outer /* nested */ poco! { <p>don't</p> } */",
        ] {
            assert!(
                find_bodies(source).is_empty(),
                "false positive on: {source}"
            );
        }
    }

    #[test]
    fn finds_invocations_however_they_are_spaced_or_delimited() {
        // `poco ! { … }` and `poco!( … )` are valid Rust; missing them would
        // leave those templates with only rustc's raw lexer error.
        for source in [
            "const T: X = poco ! { <p>©</p> };",
            "const T: X = poco!( <p>©</p> );",
            "const T: X = poco![ <p>©</p> ];",
            "const T: X = pocopine::poco! { <p>©</p> };",
        ] {
            assert_eq!(offenders(source), vec!["©"], "missed: {source}");
        }
    }

    #[test]
    fn a_string_earlier_in_the_file_does_not_hide_a_later_template() {
        // The literal must be stepped over exactly, or the scanner desyncs
        // and the real template that follows is never examined.
        let source = r#"
const NOTE: &str = "a } brace and a poco! decoy";
const T: X = poco! { <p>©</p> };
"#;
        assert_eq!(offenders(source), vec!["©"]);
    }

    #[test]
    fn handles_nested_braces_in_interpolation() {
        let source = "const T: X = poco! { <p>{{ count }}</p> }; const U: u8 = 1;";
        let bodies = find_bodies(source);
        assert_eq!(bodies.len(), 1);
        assert_eq!(source[bodies[0].clone()].trim(), "<p>{{ count }}</p>");
    }

    #[test]
    fn a_char_literal_in_interpolation_is_not_an_apostrophe() {
        // `{{ 'x' }}` compiles: `'x'` is a character literal, and the
        // expression parser accepts it. Reporting it would fail a build that
        // rustc is perfectly happy with — the worst outcome for a pre-lint.
        let source = "const T: X = poco! { <span>{{ 'x' }}</span> };";
        assert!(offenders(source).is_empty());
    }

    #[test]
    fn prose_shaped_like_a_lifetime_is_not_reported() {
        // `'tis` and `'static` lex as lifetime tokens, so these templates
        // compile and must not be rejected before cargo ever sees them.
        for source in [
            "const T: X = poco! { <p>'tis the season</p> };",
            "const T: X = poco! { <p>the 'static lifetime</p> };",
        ] {
            assert!(offenders(source).is_empty(), "false positive on: {source}");
        }
    }

    #[test]
    fn a_multi_character_single_quoted_run_is_still_reported() {
        // `'hello'` is not a character literal and does not lex, so it stays
        // reportable even though it looks like the case above. Both quotes
        // are flagged — each is a token-level problem on its own.
        let source = "const T: X = poco! { <span>{{ 'hello' }}</span> };";
        assert_eq!(offenders(source), vec!["'", "'"]);
    }

    #[test]
    fn a_brace_inside_a_quoted_value_does_not_end_the_body() {
        // `<p title="}">` is one string token to Rust, so the `}` must not be
        // counted as the closing delimiter — otherwise the body is truncated
        // and everything after it silently escapes linting.
        let source = r#"const T: X = poco! { <p title="}">a — b</p> };"#;
        let bodies = find_bodies(source);
        assert_eq!(bodies.len(), 1);
        assert!(source[bodies[0].clone()].ends_with("</p> "));
        // The offender past the brace is still found.
        assert_eq!(offenders(source), vec!["—"]);
    }

    #[test]
    fn a_brace_inside_a_char_literal_does_not_end_the_body() {
        let source = "const T: X = poco! { <p>{{ '{' }}</p> };";
        let bodies = find_bodies(source);
        assert_eq!(bodies.len(), 1);
        assert!(source[bodies[0].clone()].ends_with("</p> "));
    }

    #[test]
    fn suggests_a_named_entity_where_one_exists() {
        let source = "const T: X = poco! { <p>a — b</p> };";
        let body = find_bodies(source).remove(0);
        let found = scan_body(source, body);
        assert_eq!(found[0].escape, "&mdash;");
    }

    #[test]
    fn reports_position_in_the_rust_file() {
        let source = "// line one\nconst T: X = poco! { <p>©</p> };";
        let body = find_bodies(source).remove(0);
        let found = scan_body(source, body);
        assert_eq!(found[0].line, 2);
    }

    #[test]
    fn every_offender_on_one_line_reports_that_line() {
        // A diagnostic that names the wrong line is worse than none, so pin
        // both the line and the column against the actual source text.
        let source = "// one\n// two\nconst T: X = poco! { <p>don't © stop</p> };";
        let body = find_bodies(source).remove(0);
        let found = scan_body(source, body);

        assert_eq!(found.len(), 2, "apostrophe and symbol");
        for offender in &found {
            assert_eq!(offender.line, 3, "`{}` on the wrong line", offender.shown);
        }

        // Columns are 1-based character offsets into that line.
        let line = source.lines().nth(2).unwrap();
        let column_of = |needle: char| line.chars().position(|c| c == needle).unwrap() + 1;
        assert_eq!(found[0].column, column_of('\''));
        assert_eq!(found[1].column, column_of('©'));
    }
}
