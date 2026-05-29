//! Real-world oracle for the `@theme` token reader (RFC 092 D4): parse
//! the example app's actual CSS and confirm its design tokens land in
//! the model. `examples/file-browser` is the first port target (D8), so
//! its `@theme` block is the canonical token surface.

use std::path::PathBuf;

use pocopine_stylekit::ThemeTokens;

fn file_browser_css() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/file-browser/app.css");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

#[test]
fn parses_file_browser_theme() {
    let tokens = ThemeTokens::from_css(&file_browser_css());

    // Tokens the example is known to declare.
    for (family, name) in [
        ("color", "surface"),
        ("color", "ink-100"),
        ("color", "accent"),
        ("color", "line"),
    ] {
        assert!(
            tokens.var_for(family, name).is_some(),
            "expected `--{family}-{name}` from file-browser @theme"
        );
    }

    // A non-trivial number of color tokens, and a comment-only line must
    // not have leaked a bogus token.
    let color_count = tokens.names_in_family("color").count();
    assert!(
        color_count >= 10,
        "expected the full ink/accent/status palette, got {color_count}"
    );
    assert!(tokens.var_for("color", "").is_none());
}
