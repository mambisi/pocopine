#![cfg(not(target_arch = "wasm32"))]

//! Live schema-drift test for the Render adapter.
//!
//! Render publishes a public OpenAPI spec. Rather than vendoring the
//! 300 KB+ file, this test pins a **checksum**: it downloads the live
//! spec, canonicalises it (recursive key-sort, so formatting/key-order
//! churn doesn't false-trip), SHA-256s it, and compares to
//! [`EXPECTED_SHA256`].
//!
//!   * structural — every path [`pocopine_deploy_render::client`] calls
//!     must still exist in the live spec; a missing one fails hard.
//!   * checksum — the live spec's hash must equal `EXPECTED_SHA256`.
//!     Render warns its spec is unversioned ("names may change"), so any
//!     upstream change trips this. On a deliberate change, re-run, copy
//!     the printed hash into `EXPECTED_SHA256`, and reconcile the client
//!     if a structural assertion also failed.
//!
//! Gating: the spec is public (no token). The test SKIPS when the
//! endpoint is unreachable so offline `cargo test` stays green; CI's
//! spec-drift job sets `POCOPINE_REQUIRE_SPEC_DRIFT=1` to make an
//! unreachable endpoint a hard failure instead.

use pocopine_crypto::sha256_hex;

const RENDER_OPENAPI_URL: &str =
    "https://api-docs.render.com/v1.0/openapi/render-public-api-1.json";

/// SHA-256 of the canonicalised live spec, last reconciled 2026-08-15.
/// Update this when the drift test reports a new hash for a reviewed,
/// deliberate upstream change. (2026-07-11: benign content churn —
/// every `REQUIRED_OPERATIONS` endpoint and method still present, so no
/// client change. 2026-08-15: benign again — structural assertions
/// passed, and every field [`client`] deserializes was verified present
/// in the live spec: `Service` {id,name,type,url,serviceDetails,
/// dashboardUrl,region}, `Deploy` {id,status,image,createdAt,finishedAt}
/// + `DeployImage` {ref,sha}, and `RenderLog` {timestamp,message} —
/// the last reached through the `/logs` → `/logs/subscribe` 101 `$ref`,
/// where both are still upstream-`required`. No client change.)
const EXPECTED_SHA256: &str = "63554873b71d37d2e0a9e4ff4582ef57300c15d2773180d76dde0212199c06c6";

/// Operations `crate::client` calls.
const REQUIRED_OPERATIONS: &[(&str, &[&str])] = &[
    ("/services", &["get", "post"]),
    ("/services/{serviceId}", &["get", "patch"]),
    ("/services/{serviceId}/deploys", &["get", "post"]),
    ("/services/{serviceId}/deploys/{deployId}", &["get"]),
    ("/services/{serviceId}/env-vars", &["put"]),
    ("/services/{serviceId}/scale", &["post"]),
    ("/registrycredentials", &["get", "post"]),
    ("/registrycredentials/{registryCredentialId}", &["patch"]),
    ("/logs", &["get"]),
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
fn render_openapi_matches_the_pinned_checksum() {
    let resp = reqwest::blocking::Client::new()
        .get(RENDER_OPENAPI_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text());

    let live = match resp {
        Ok(body) => body,
        Err(e) => {
            let msg = format!("render spec-drift: could not reach {RENDER_OPENAPI_URL}: {e}");
            assert!(
                !require_drift(),
                "{msg} (POCOPINE_REQUIRE_SPEC_DRIFT is set)"
            );
            eprintln!("skipping {msg}");
            return;
        }
    };

    let spec: serde_json::Value =
        serde_json::from_str(&live).expect("live Render spec is valid JSON");

    // Structural — the operations the client depends on must still exist.
    // This runs first so a breaking change is the failure you see.
    let paths = spec["paths"]
        .as_object()
        .expect("Render OpenAPI `paths` object");
    for (path, methods) in REQUIRED_OPERATIONS {
        let item = paths.get(*path).unwrap_or_else(|| {
            panic!(
                "Render OpenAPI no longer defines path `{path}` — reconcile crates/pocopine-deploy-render/src/client.rs",
            )
        });

        for method in *methods {
            assert!(
                item.get(*method).is_some(),
                "Render OpenAPI path `{path}` no longer defines method `{method}` — reconcile crates/pocopine-deploy-render/src/client.rs",
            );
        }
    }

    // Checksum — any content change trips this. On a deliberate change,
    // copy the printed hash into EXPECTED_SHA256.
    let actual = sha256_hex(canonical_json(&spec).as_bytes());
    assert_eq!(
        actual, EXPECTED_SHA256,
        "\nRender's OpenAPI spec has drifted from the pinned checksum.\n\
         Review the change at {RENDER_OPENAPI_URL}, reconcile src/client.rs if needed,\n\
         then set EXPECTED_SHA256 to: {actual}\n",
    );
}
