//! Terminal rendering for Stylekit diagnostics (RFC 092 D5/D6).
//!
//! Dependency-free, rustc-flavoured output so Stylekit diagnostics read
//! the same as the rest of `pocopine`'s build output:
//!
//! ```text
//! error: unknown utility `flexx`
//!   --> src/file.poco:3:18
//!    |
//!  3 |   <div class="flexx items-center">
//!    |               ^^^^^ did you mean `flex`?
//! ```

use crate::diagnostics::{Diagnostic, Severity};
use crate::project::SourceFile;

/// Render one diagnostic against the project's file table. `files` is
/// indexed by `Span::file`; an unknown/out-of-range file renders the
/// message without a source frame.
pub fn render(diag: &Diagnostic, files: &[SourceFile]) -> String {
    let label = match diag.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let mut out = format!("{label}: {}\n", diag.message);

    let idx = diag.span.file as usize;
    let Some(file) = files.get(idx) else {
        if let Some(help) = &diag.help {
            out.push_str(&format!("  = help: {help}\n"));
        }
        return out;
    };

    let (line_no, col, line_text) = locate(&file.source, diag.span.start as usize);
    let width = (diag.span.end.saturating_sub(diag.span.start)).max(1) as usize;
    let gutter = line_no.to_string();
    let pad = " ".repeat(gutter.len());

    out.push_str(&format!(
        "{pad}--> {}:{line_no}:{col}\n",
        file.path.display()
    ));
    out.push_str(&format!("{pad} |\n"));
    out.push_str(&format!("{gutter} | {line_text}\n"));
    let caret_pad = " ".repeat(col.saturating_sub(1));
    let carets = "^".repeat(width);
    let hint = diag
        .help
        .as_deref()
        .map(|h| format!(" {h}"))
        .unwrap_or_default();
    out.push_str(&format!("{pad} | {caret_pad}{carets}{hint}\n"));
    out
}

/// Map a byte offset to (1-based line, 1-based column, line text).
fn locate(source: &str, offset: usize) -> (usize, usize, &str) {
    let offset = offset.min(source.len());
    let mut line_start = 0;
    let mut line_no = 1;
    for (i, b) in source.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            line_no += 1;
            line_start = i + 1;
        }
    }
    let line_end = source[line_start..]
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(source.len());
    let col = offset - line_start + 1;
    (line_no, col, &source[line_start..line_end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Span;

    #[test]
    fn renders_frame_with_caret_and_help() {
        let files = vec![SourceFile {
            path: "src/x.poco".into(),
            source: "line one\n<div class=\"flexx\">\n".into(),
        }];
        // `flexx` sits at byte 19..24 on line 2.
        let start = files[0].source.find("flexx").unwrap() as u32;
        let diag = Diagnostic::error("unknown utility `flexx`")
            .with_help("did you mean `flex`?")
            .at(Span {
                file: 0,
                start,
                end: start + 5,
            });
        let text = render(&diag, &files);
        assert!(text.contains("error: unknown utility `flexx`"));
        assert!(text.contains("--> src/x.poco:2:13"));
        assert!(text.contains("^^^^^ did you mean `flex`?"));
    }

    #[test]
    fn unknown_file_renders_message_only() {
        let diag = Diagnostic::error("boom");
        let text = render(&diag, &[]);
        assert_eq!(text, "error: boom\n");
    }
}
