//! Ensures graphql-client's compile-time input (`schema.json`) is present.
//!
//! `schema.json` is Railway's introspected GraphQL schema — ~1.5 MB, too
//! large to vendor in the repo, so it is gitignored. When it is absent
//! this script downloads it from Railway's **unauthenticated**
//! introspection endpoint. A cached copy is reused, so only the first
//! build of a fresh checkout needs the network.
//!
//! Drift from the live API is caught elsewhere — `tests/spec_drift.rs`
//! re-introspects the live schema, and graphql-client itself turns a
//! renamed/removed field a query uses into a compile error.

use std::path::Path;

const ENDPOINT: &str = "https://backboard.railway.com/graphql/v2";

/// `{"query": "<standard GraphQL introspection query>"}`.
const INTROSPECTION_BODY: &str = r#"{"query":"query IntrospectionQuery { __schema { queryType { name } mutationType { name } subscriptionType { name } types { ...FullType } directives { name description locations args { ...InputValue } } } } fragment FullType on __Type { kind name description fields(includeDeprecated: true) { name description args { ...InputValue } type { ...TypeRef } isDeprecated deprecationReason } inputFields { ...InputValue } interfaces { ...TypeRef } enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason } possibleTypes { ...TypeRef } } fragment InputValue on __InputValue { name description type { ...TypeRef } defaultValue } fragment TypeRef on __Type { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name ofType { kind name } } } } } } } }"}"#;

fn main() {
    println!("cargo:rerun-if-changed=schema.json");
    println!("cargo:rerun-if-changed=build.rs");

    // The GraphQL client (and so `schema.json`) is host-only — it is
    // `#[cfg(not(target_arch = "wasm32"))]`. Skip the fetch for wasm
    // targets, where the schema is never read.
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }

    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    let schema = Path::new(&dir).join("schema.json");

    // Reuse a cached copy — only a fresh checkout hits the network.
    if schema.exists() {
        return;
    }

    println!(
        "cargo:warning=pocopine-deploy-railway: fetching Railway GraphQL schema from {ENDPOINT}"
    );
    let body = ureq::post(ENDPOINT)
        .set("content-type", "application/json")
        .send_string(INTROSPECTION_BODY)
        .unwrap_or_else(|e| {
            panic!(
                "pocopine-deploy-railway: could not fetch the Railway schema from {ENDPOINT}: {e}\n\
                 The first build of this crate needs a network connection."
            )
        })
        .into_string()
        .expect("railway introspection response body");

    std::fs::write(&schema, body).expect("write schema.json");
}
