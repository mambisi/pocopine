#![cfg(not(target_arch = "wasm32"))]

//! Live schema-drift test for the Cloudflare Pages adapter.
//!
//! Same intent as the Render and Railway drift tests — catch an upstream
//! API change before a user's deploy does — but pinned differently, and
//! deliberately so.
//!
//! Render's spec is product-scoped, so hashing the whole document is a
//! useful signal. Cloudflare publishes **one** spec for the entire
//! platform: ~2,000 paths and ~24 MB covering DNS, Workers, Zero Trust,
//! and everything else. A whole-document hash there would trip several
//! times a week on products this adapter never calls, and a check that
//! cries wolf gets muted. So this test pins a **subtree**: the seven
//! operations [`pocopine_deploy_cloudflare_pages::client`] calls, plus
//! the transitive `$ref` closure those operations reach into. Drift in
//! Workers AI is correctly invisible here; drift in the Direct Upload
//! protocol is not.
//!
//! Three layers, cheapest and most specific first:
//!
//!   * structural — every path and method the client calls must exist.
//!   * field-level — every field the client *sends* must still be
//!     accepted, and every field it *deserializes* must still be
//!     returned. This is the layer that catches a silently renamed key,
//!     which a path-existence check sails straight past.
//!   * checksum — the canonicalised subtree (recursive key-sort, so
//!     formatting churn doesn't false-trip) must hash to
//!     [`EXPECTED_SHA256`]. Anything the first two layers didn't think
//!     to ask about lands here.
//!
//! Gating: the spec is public (no token). The test SKIPS when the
//! endpoint is unreachable so offline `cargo test` stays green; CI's
//! spec-drift job sets `POCOPINE_REQUIRE_SPEC_DRIFT=1` to make an
//! unreachable endpoint a hard failure instead.

use std::collections::BTreeMap;
use std::time::Duration;

use pocopine_crypto::sha256_hex;
use serde_json::{Map, Value, json};

/// Cloudflare's `api-schemas` repository is the source of record for the
/// v4 API; the dashboard's reference is generated from it.
const CLOUDFLARE_OPENAPI_URL: &str =
    "https://raw.githubusercontent.com/cloudflare/api-schemas/main/openapi.json";

/// SHA-256 of the canonicalised pinned subtree, last reconciled
/// 2026-08-15 against `tested_against` = `>=4.0.0, <5.0.0`.
///
/// Update this when the drift test reports a new hash for a reviewed,
/// deliberate upstream change. (2026-08-15: first pin. Every operation,
/// request field, and response field this adapter depends on was
/// verified present upstream — including `commit_dirty` and both
/// `_headers`/`_redirects` multipart parts on deployment creation, and
/// `metadata.contentType` on asset upload.)
const EXPECTED_SHA256: &str = "de5999cfce8aa218b7fe5bf2e77fb1420ee85b59b0d821aea2cf887594aa0350";

const PROJECTS: &str = "/accounts/{account_id}/pages/projects";
const PROJECT: &str = "/accounts/{account_id}/pages/projects/{project_name}";
const DEPLOYMENTS: &str = "/accounts/{account_id}/pages/projects/{project_name}/deployments";
const UPLOAD_TOKEN: &str = "/accounts/{account_id}/pages/projects/{project_name}/upload-token";
const CHECK_MISSING: &str = "/pages/assets/check-missing";
const UPLOAD: &str = "/pages/assets/upload";
const UPSERT_HASHES: &str = "/pages/assets/upsert-hashes";

/// Operations `crate::client` calls. Also the seed set for the pinned
/// subtree, so adding a call here automatically widens the checksum.
const REQUIRED_OPERATIONS: &[(&str, &[&str])] = &[
    (PROJECTS, &["post"]),
    (PROJECT, &["get"]),
    (DEPLOYMENTS, &["get", "post"]),
    (UPLOAD_TOKEN, &["get"]),
    (CHECK_MISSING, &["post"]),
    (UPLOAD, &["post"]),
    (UPSERT_HASHES, &["post"]),
];

/// Query parameters `latest_deployment` sends.
const REQUIRED_QUERY_PARAMS: &[(&str, &str, &[&str])] =
    &[(DEPLOYMENTS, "get", &["env", "page", "per_page"])];

/// A request-body assertion: `(path, method, content type, cursor into
/// the body schema, fields)`. A cursor segment of [`ITEMS`] steps into an
/// array's element schema.
type RequestFieldCheck = (
    &'static str,
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// A response assertion: `(path, method, cursor into the 200 schema,
/// fields)`.
type ResponseFieldCheck = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// Request fields the client sends.
const REQUIRED_REQUEST_FIELDS: &[RequestFieldCheck] = &[
    // create_project
    (PROJECTS, "post", JSON, &[], &["name", "production_branch"]),
    // create_deployment — the Direct Upload multipart form.
    (
        DEPLOYMENTS,
        "post",
        "multipart/form-data",
        &[],
        &[
            "manifest",
            "branch",
            "commit_hash",
            "commit_dirty",
            "_headers",
            "_redirects",
        ],
    ),
    // check_missing / upsert_hashes
    (CHECK_MISSING, "post", JSON, &[], &["hashes"]),
    (UPSERT_HASHES, "post", JSON, &[], &["hashes"]),
    // upload_batch — a bare array of assets, so step into the item.
    (
        UPLOAD,
        "post",
        JSON,
        &[ITEMS],
        &["key", "value", "metadata", "base64"],
    ),
    (UPLOAD, "post", JSON, &[ITEMS, "metadata"], &["contentType"]),
];

/// Response fields the client deserializes. An empty cursor asserts on
/// the envelope itself.
const REQUIRED_RESPONSE_FIELDS: &[ResponseFieldCheck] = &[
    // Envelope
    (
        CHECK_MISSING,
        "post",
        &[],
        &["success", "errors", "messages", "result"],
    ),
    // Project
    (
        PROJECT,
        "get",
        &["result"],
        &["name", "subdomain", "production_branch"],
    ),
    // Deployment
    (
        DEPLOYMENTS,
        "get",
        &["result", ITEMS],
        &[
            "id",
            "url",
            "environment",
            "aliases",
            "latest_stage",
            "created_on",
            "modified_on",
        ],
    ),
    // DeploymentStage
    (
        DEPLOYMENTS,
        "get",
        &["result", ITEMS, "latest_stage"],
        &["name", "status", "started_on", "ended_on"],
    ),
    // upload_token
    (UPLOAD_TOKEN, "get", &["result"], &["jwt"]),
];

const JSON: &str = "application/json";

/// Cursor segment meaning "step into this array's element schema".
/// Not a legal JSON property name, so it cannot collide with a field.
const ITEMS: &str = "[]";

/// Cloudflare's spec nests `$ref`s a few levels deep; this only bounds a
/// pathological cycle, it is not a real depth limit.
const MAX_REF_HOPS: usize = 32;

/// CI's spec-drift job sets this so an unreachable endpoint fails
/// instead of skipping.
fn require_drift() -> bool {
    std::env::var("POCOPINE_REQUIRE_SPEC_DRIFT")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

/// Resolve a JSON pointer of the form `#/components/schemas/foo`.
fn pointer<'a>(spec: &'a Value, reference: &str) -> Option<&'a Value> {
    let mut node = spec;
    for segment in reference.strip_prefix("#/")?.split('/') {
        // RFC 6901 escapes: `~1` before `~0`, or `~01` decodes wrong.
        let segment = segment.replace("~1", "/").replace("~0", "~");
        node = node.get(&segment)?;
    }
    Some(node)
}

/// Follow a `$ref` chain to the schema it names.
fn resolve<'a>(spec: &'a Value, node: &'a Value) -> &'a Value {
    let mut node = node;
    for _ in 0..MAX_REF_HOPS {
        let Some(reference) = node.get("$ref").and_then(Value::as_str) else {
            return node;
        };
        match pointer(spec, reference) {
            Some(next) => node = next,
            None => return node,
        }
    }
    node
}

/// Properties of a schema, merging `allOf`/`oneOf`/`anyOf` branches —
/// Cloudflare composes most response bodies out of a shared envelope
/// plus a `result` branch, so a plain `properties` lookup sees nothing.
fn properties<'a>(spec: &'a Value, node: &'a Value) -> BTreeMap<&'a str, &'a Value> {
    let node = resolve(spec, node);
    let mut out = BTreeMap::new();
    for combinator in ["allOf", "oneOf", "anyOf"] {
        if let Some(branches) = node.get(combinator).and_then(Value::as_array) {
            for branch in branches {
                out.extend(properties(spec, branch));
            }
        }
    }
    if let Some(map) = node.get("properties").and_then(Value::as_object) {
        for (name, schema) in map {
            out.insert(name.as_str(), schema);
        }
    }
    out
}

/// Walk a cursor into a schema, stepping through array items on [`ITEMS`].
fn descend<'a>(spec: &'a Value, node: &'a Value, cursor: &[&str]) -> Option<&'a Value> {
    let mut node = node;
    for segment in cursor {
        node = if *segment == ITEMS {
            resolve(spec, node).get("items")?
        } else {
            *properties(spec, node).get(segment)?
        };
    }
    Some(node)
}

/// The request body schema for a `(path, method, content type)`.
fn request_schema<'a>(
    spec: &'a Value,
    path: &str,
    method: &str,
    content_type: &str,
) -> Option<&'a Value> {
    let body = resolve(
        spec,
        spec.get("paths")?
            .get(path)?
            .get(method)?
            .get("requestBody")?,
    );
    body.get("content")?.get(content_type)?.get("schema")
}

/// The 200-response schema for a `(path, method)`.
fn response_schema<'a>(spec: &'a Value, path: &str, method: &str) -> Option<&'a Value> {
    let response = resolve(
        spec,
        spec.get("paths")?
            .get(path)?
            .get(method)?
            .get("responses")?
            .get("200")?,
    );
    response.get("content")?.get(JSON)?.get("schema")
}

/// Every `$ref` string anywhere inside a subtree.
fn collect_refs(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value.as_str()) {
                    ("$ref", Some(reference)) => out.push(reference.to_owned()),
                    _ => collect_refs(value, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        _ => {}
    }
}

/// Deterministic JSON serialisation: object keys recursively sorted, no
/// whitespace. Stable regardless of serde_json's `preserve_order`
/// feature, so the checksum only moves on genuine content changes.
fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(&mut out, v);
    out
}

fn write_canonical(out: &mut String, v: &Value) {
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
fn cloudflare_pages_openapi_matches_the_pinned_checksum() {
    // ~24 MB over a public CDN; the default no-timeout client would hang
    // a CI runner on a stalled connection rather than skip.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .expect("building a blocking reqwest client");

    let resp = client
        .get(CLOUDFLARE_OPENAPI_URL)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text());

    let live = match resp {
        Ok(body) => body,
        Err(e) => {
            let msg = format!("cf-pages spec-drift: could not reach {CLOUDFLARE_OPENAPI_URL}: {e}");
            assert!(
                !require_drift(),
                "{msg} (POCOPINE_REQUIRE_SPEC_DRIFT is set)"
            );
            eprintln!("skipping {msg}");
            return;
        }
    };

    let spec: Value = serde_json::from_str(&live).expect("live Cloudflare spec is valid JSON");
    let paths = spec["paths"]
        .as_object()
        .expect("Cloudflare OpenAPI `paths` object");

    // 1. Structural — the operations the client depends on must exist.
    // First, so a removed endpoint is the failure you actually read.
    for (path, methods) in REQUIRED_OPERATIONS {
        let item = paths.get(*path).unwrap_or_else(|| {
            panic!(
                "Cloudflare OpenAPI no longer defines path `{path}` — reconcile \
                 crates/pocopine-deploy-cloudflare-pages/src/client.rs",
            )
        });
        for method in *methods {
            assert!(
                item.get(*method).is_some(),
                "Cloudflare OpenAPI path `{path}` no longer defines method `{method}` — \
                 reconcile crates/pocopine-deploy-cloudflare-pages/src/client.rs",
            );
        }
    }

    // 2a. Query parameters `latest_deployment` sends.
    for (path, method, expected) in REQUIRED_QUERY_PARAMS {
        let declared: Vec<&str> = spec["paths"][path][method]["parameters"]
            .as_array()
            .map(|params| {
                params
                    .iter()
                    .map(|p| resolve(&spec, p))
                    .filter(|p| p.get("in").and_then(Value::as_str) == Some("query"))
                    .filter_map(|p| p.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        for name in *expected {
            assert!(
                declared.contains(name),
                "Cloudflare OpenAPI `{method} {path}` no longer accepts query parameter \
                 `{name}` (declares {declared:?}) — reconcile client.rs::latest_deployment",
            );
        }
    }

    // 2b. Request fields the client sends must still be accepted.
    for (path, method, content_type, cursor, fields) in REQUIRED_REQUEST_FIELDS {
        let schema = request_schema(&spec, path, method, content_type).unwrap_or_else(|| {
            panic!("Cloudflare OpenAPI `{method} {path}` no longer declares a `{content_type}` request body")
        });
        let schema = descend(&spec, schema, cursor).unwrap_or_else(|| {
            panic!("Cloudflare OpenAPI `{method} {path}` request body no longer has the shape {cursor:?}")
        });
        let declared = properties(&spec, schema);
        for field in *fields {
            assert!(
                declared.contains_key(field),
                "Cloudflare OpenAPI `{method} {path}` request body no longer accepts field \
                 `{field}` (declares {:?}) — reconcile \
                 crates/pocopine-deploy-cloudflare-pages/src/client.rs",
                declared.keys().collect::<Vec<_>>(),
            );
        }
    }

    // 2c. Response fields the client deserializes must still be returned.
    for (path, method, cursor, fields) in REQUIRED_RESPONSE_FIELDS {
        let schema = response_schema(&spec, path, method).unwrap_or_else(|| {
            panic!("Cloudflare OpenAPI `{method} {path}` no longer declares a JSON 200 response")
        });
        let schema = descend(&spec, schema, cursor).unwrap_or_else(|| {
            panic!("Cloudflare OpenAPI `{method} {path}` 200 response no longer has the shape {cursor:?}")
        });
        let declared = properties(&spec, schema);
        for field in *fields {
            assert!(
                declared.contains_key(field),
                "Cloudflare OpenAPI `{method} {path}` 200 response no longer returns field \
                 `{field}` (declares {:?}) — reconcile \
                 crates/pocopine-deploy-cloudflare-pages/src/client.rs",
                declared.keys().collect::<Vec<_>>(),
            );
        }
    }

    // 3. Checksum over the pinned subtree: the operations above plus the
    // transitive `$ref` closure they reach. Catches everything layers 1
    // and 2 didn't think to ask about, without hashing 2,000 unrelated
    // paths.
    let mut pinned_paths = Map::new();
    for (path, methods) in REQUIRED_OPERATIONS {
        let mut item = Map::new();
        for method in *methods {
            item.insert((*method).to_owned(), spec["paths"][path][method].clone());
        }
        pinned_paths.insert((*path).to_owned(), Value::Object(item));
    }

    let mut components: BTreeMap<String, Value> = BTreeMap::new();
    let mut frontier = Vec::new();
    collect_refs(&Value::Object(pinned_paths.clone()), &mut frontier);
    while let Some(reference) = frontier.pop() {
        if components.contains_key(&reference) {
            continue;
        }
        // An unresolvable `$ref` is drift too — a schema the operations
        // still name but the spec no longer defines.
        let target = pointer(&spec, &reference).unwrap_or_else(|| {
            panic!("Cloudflare OpenAPI references `{reference}`, which it no longer defines")
        });
        collect_refs(target, &mut frontier);
        components.insert(reference, target.clone());
    }

    let pinned = json!({ "paths": pinned_paths, "components": components });
    let actual = sha256_hex(canonical_json(&pinned).as_bytes());
    assert_eq!(
        actual, EXPECTED_SHA256,
        "\nCloudflare's Pages API surface has drifted from the pinned checksum.\n\
         The structural and field-level assertions above still passed, so this is a\n\
         change to a part of the subtree they don't cover. Review it at\n\
         {CLOUDFLARE_OPENAPI_URL}, reconcile src/client.rs if needed, then set\n\
         EXPECTED_SHA256 to: {actual}\n",
    );
}
