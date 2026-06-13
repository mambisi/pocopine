//! Second-example coverage oracle (RFC 092 M2, issue #172): the
//! `examples/tailwind` demo must compile with no diagnostics. Unlike
//! file-browser it declares no `@theme` — its colours come from the
//! built-in Tailwind palette — so this also exercises the palette path
//! end-to-end through the project compile.

use std::path::PathBuf;

use pocopine_stylekit::{CompileOptions, SourceFile, compile_project};

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/tailwind")
}

fn read_poco(dir: &std::path::Path, out: &mut Vec<SourceFile>) {
    for entry in std::fs::read_dir(dir).expect("read example dir") {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            read_poco(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("poco") {
            let source = std::fs::read_to_string(&path).unwrap();
            out.push(SourceFile { path, source });
        }
    }
}

#[test]
fn tailwind_example_compiles_clean() {
    let dir = example_dir();
    let theme = std::fs::read_to_string(dir.join("app.css")).unwrap_or_default();
    let mut files = Vec::new();
    read_poco(&dir.join("src"), &mut files);
    assert!(!files.is_empty(), "no .poco sources found");

    let out = compile_project(&theme, &files, CompileOptions::default());
    let errors: Vec<_> = out
        .diagnostics
        .iter()
        .filter(|d| d.severity == pocopine_stylekit::Severity::Error)
        .map(|d| d.message.clone())
        .collect();
    assert!(
        errors.is_empty(),
        "examples/tailwind has unresolved classes:\n{}",
        errors.join("\n")
    );
    // Palette colour landed as a literal (no @theme token for it).
    assert!(
        out.css.contains("oklch("),
        "expected palette colours in output"
    );
}
