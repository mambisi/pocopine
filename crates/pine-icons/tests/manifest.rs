//! Manifest + vendored-tree integrity. Pure host-side tests —
//! don't need wasm runtime.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

fn assets_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/tabler")
}

fn svg_stems_in(subdir: &str) -> HashSet<String> {
    let dir = assets_root().join(subdir);
    let mut out = HashSet::new();
    for entry in fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("svg") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("svg stem");
        out.insert(stem.to_string());
    }
    out
}

#[test]
fn manifest_counts_match_disk() {
    let manifest_path = assets_root().join("MANIFEST.json");
    let raw =
        fs::read_to_string(&manifest_path).unwrap_or_else(|e| panic!("read MANIFEST.json: {e}"));
    let json: serde_json::Value =
        serde_json::from_str(&raw).expect("MANIFEST.json is not valid JSON");

    let claimed_outline = json["counts"]["outline"]
        .as_u64()
        .expect("counts.outline must be a number");
    let claimed_filled = json["counts"]["filled"]
        .as_u64()
        .expect("counts.filled must be a number");

    let actual_outline = svg_stems_in("outline").len() as u64;
    let actual_filled = svg_stems_in("filled").len() as u64;

    assert_eq!(
        claimed_outline, actual_outline,
        "MANIFEST.counts.outline drift — re-run sync-tabler-icons.sh"
    );
    assert_eq!(
        claimed_filled, actual_filled,
        "MANIFEST.counts.filled drift — re-run sync-tabler-icons.sh"
    );

    assert_eq!(json["license"].as_str(), Some("MIT"));
    assert!(
        json["commit"]
            .as_str()
            .map(|s| s.len() == 40)
            .unwrap_or(false),
        "MANIFEST.commit must be a 40-char sha"
    );
}

#[test]
fn every_filename_is_valid_kebab_case() {
    for subdir in ["outline", "filled"] {
        for stem in svg_stems_in(subdir) {
            for c in stem.chars() {
                assert!(
                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-',
                    "bad char {c:?} in {subdir}/{stem}.svg — vendored name violates the `[a-z0-9-]+` contract"
                );
            }
            assert!(!stem.starts_with('-'));
            assert!(!stem.ends_with('-'));
            assert!(!stem.contains("--"));
        }
    }
}

#[test]
fn registry_lookup_returns_registered_icons() {
    pine_icons::register_icons!["user", "chevron-down", filled / "star"];
    assert!(pine_icons::lookup("outline", "user").is_some());
    assert!(pine_icons::lookup("outline", "chevron-down").is_some());
    assert!(pine_icons::lookup("filled", "star").is_some());
    assert!(pine_icons::lookup("outline", "not-registered").is_none());
    // Sanity — the SVG string should actually start with `<svg`.
    assert!(pine_icons::lookup("outline", "user")
        .unwrap()
        .trim_start()
        .starts_with("<svg"));
}

#[test]
fn icon_macro_emits_svg_string() {
    let s = pine_icons::icon!("user");
    assert!(s.trim_start().starts_with("<svg"));
    let f = pine_icons::icon!(filled / "star");
    assert!(f.trim_start().starts_with("<svg"));
}
