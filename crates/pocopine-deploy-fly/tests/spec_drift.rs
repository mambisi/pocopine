//! Live schema-drift test for the Fly adapter.
//!
//! Fly publishes a public OpenAPI spec for the Machines API. Rather than
//! vendoring the 200 KB+ file, this test pins a **checksum**: it
//! downloads the live spec, canonicalises it (recursive key-sort, so
//! formatting/key-order churn doesn't false-trip), SHA-256s it, and
//! compares to [`EXPECTED_SHA256`].
//!
//!   * structural — every path [`pocopine_deploy_fly::machines`] calls
//!     must still exist in the live spec; a missing one fails hard.
//!   * checksum — the live spec's hash must equal `EXPECTED_SHA256`.
//!     On a deliberate upstream change, re-run, copy the printed hash
//!     into `EXPECTED_SHA256`, and reconcile the client if a structural
//!     assertion also failed.
//!
//! Gating: the spec is public (no token). The test SKIPS when the
//! endpoint is unreachable so offline `cargo test` stays green; CI's
//! spec-drift job sets `POCOPINE_REQUIRE_SPEC_DRIFT=1` to make an
//! unreachable endpoint a hard failure instead.

use sha2::{Digest, Sha256};

const FLY_OPENAPI_URL: &str = "https://docs.machines.dev/spec/openapi3.json";

/// SHA-256 of the canonicalised live spec, last reconciled 2026-05-22.
/// Update this when the drift test reports a new hash for a reviewed,
/// deliberate upstream change.
const EXPECTED_SHA256: &str = "916718be0812f8edb1fd5f1fd6572a14dce49bf08de07fa780159d1fbda2dcc9";

/// Paths `crate::machines` calls.
const REQUIRED_PATHS: &[&str] = &[
    "/apps",
    "/apps/{app_name}",
    "/apps/{app_name}/machines",
    "/apps/{app_name}/machines/{machine_id}",
    "/apps/{app_name}/machines/{machine_id}/wait",
    "/apps/{app_name}/secrets",
    "/apps/{app_name}/volumes",
    "/apps/{app_name}/volumes/{volume_id}/extend",
];

/// CI's spec-drift job sets this so an unreachable endpoint fails
/// instead of skipping.
fn require_drift() -> bool {
    std::env::var("POCOPINE_REQUIRE_SPEC_DRIFT")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Deterministic JSON serialisation: object keys recursively sorted, no
/// whitespace. Stable regardless of serde_json's `preserve_order`
/// feature, so the checksum only moves on genuine content changes.
fn canonical_json(v: &serde_json::Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, v);
    out
}

fn write_canonical(out: &mut String, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            out.push('{');
            for (i, (k, val)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string key serialises"));
                out.push(':');
                write_canonical(out, val);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(out, item);
            }
            out.push(']');
        }
        scalar => out.push_str(&serde_json::to_string(scalar).expect("scalar serialises")),
    }
}

#[test]
fn fly_openapi_matches_the_pinned_checksum() {
    let resp = reqwest::blocking::Client::new()
        .get(FLY_OPENAPI_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text());

    let live = match resp {
        Ok(body) => body,
        Err(e) => {
            let msg = format!("fly spec-drift: could not reach {FLY_OPENAPI_URL}: {e}");
            assert!(
                !require_drift(),
                "{msg} (POCOPINE_REQUIRE_SPEC_DRIFT is set)"
            );
            eprintln!("skipping {msg}");
            return;
        }
    };

    let spec: serde_json::Value = serde_json::from_str(&live).expect("live Fly spec is valid JSON");

    // Structural — the endpoints the client depends on must still exist.
    let paths = spec["paths"]
        .as_object()
        .expect("Fly OpenAPI `paths` object");
    for p in REQUIRED_PATHS {
        assert!(
            paths.contains_key(*p),
            "Fly OpenAPI no longer defines path `{p}` — reconcile crates/pocopine-deploy-fly/src/machines.rs",
        );
    }

    // Checksum — any content change trips this.
    let actual = format!("{:x}", Sha256::digest(canonical_json(&spec).as_bytes()));
    assert_eq!(
        actual, EXPECTED_SHA256,
        "\nFly's OpenAPI spec has drifted from the pinned checksum.\n\
         Review the change at {FLY_OPENAPI_URL}, reconcile src/machines.rs if needed,\n\
         then set EXPECTED_SHA256 to: {actual}\n",
    );
}
