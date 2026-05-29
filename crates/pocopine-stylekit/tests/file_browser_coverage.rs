//! Milestone-1 coverage oracle (RFC 092 D8/D9): every utility class the
//! `examples/file-browser` template uses must resolve against its own
//! `@theme`, with no diagnostics. This is the contract that lets the
//! port in Phase 6 drop Tailwind.
//!
//! The class list is a committed fixture (regenerated from the example's
//! `.poco` files); the tokens come from the real `app.css`.

use std::path::PathBuf;

use pocopine_stylekit::extract::UsedClass;
use pocopine_stylekit::{CompileOptions, Compiler, Severity, ThemeTokens};

const CLASSES: &str = include_str!("fixtures/file-browser-classes.txt");

fn example_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/file-browser")
}

#[test]
fn every_file_browser_class_resolves() {
    let css = std::fs::read_to_string(example_root().join("app.css")).expect("read app.css");
    let tokens = ThemeTokens::from_css(&css);
    let compiler = Compiler::new(tokens, CompileOptions::default());

    let classes: Vec<&str> = CLASSES
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .collect();
    assert!(
        classes.len() > 200,
        "fixture looks truncated: {} classes",
        classes.len()
    );

    let mut failures = Vec::new();
    for class in &classes {
        let out = compiler.compile(&[UsedClass::new(*class, pocopine_stylekit::Span::UNKNOWN)]);
        for d in &out.diagnostics {
            if d.severity == Severity::Error {
                failures.push(format!("{class}: {}", d.message));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} file-browser classes failed to resolve:\n{}",
        failures.len(),
        classes.len(),
        failures.join("\n")
    );
}
