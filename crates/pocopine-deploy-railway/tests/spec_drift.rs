//! Live schema-drift test for the Railway adapter.
//!
//! Railway publishes no OpenAPI file, so the "spec" is the GraphQL
//! schema reached by an introspection query. This test downloads the
//! live schema from `backboard.railway.com/graphql/v2` and asserts every
//! mutation / query / input type [`pocopine_deploy_railway::client`]
//! depends on still exists. If Railway renames or removes one, this test
//! fails — telling you to reconcile `src/client.rs`.
//!
//! Nothing is committed to the repo: the live schema is the source of
//! truth, checked fresh each run.
//!
//! Gating — introspection requires auth, so:
//!   * no token  → the test SKIPS (keeps offline `cargo test` green),
//!   * token set → it runs and FAILS on drift.
//!
//! Set `POCOPINE_REQUIRE_SPEC_DRIFT=1` (CI's spec-drift job) to turn a
//! missing token or an unreachable endpoint into a hard failure, so
//! drift can't slip through a silent skip.

use std::collections::HashSet;

const RAILWAY_GRAPHQL: &str = "https://backboard.railway.com/graphql/v2";

/// Mutations `crate::client` sends. Keep in sync with `src/client.rs`.
const REQUIRED_MUTATIONS: &[&str] = &[
    "projectCreate",
    "serviceCreate",
    "serviceInstanceUpdate",
    "serviceInstanceDeployV2",
    "variableCollectionUpsert",
    "serviceDomainCreate",
];

/// Queries `crate::client` sends.
const REQUIRED_QUERIES: &[&str] = &["projects"];

/// GraphQL input types the client builds payloads for.
const REQUIRED_INPUT_TYPES: &[&str] = &[
    "ProjectCreateInput",
    "ServiceCreateInput",
    "ServiceInstanceUpdateInput",
    "VariableCollectionUpsertInput",
    "ServiceDomainCreateInput",
];

fn railway_token() -> Option<String> {
    for var in [
        "RAILWAY_API_TOKEN",
        "POCOPINE_RAILWAY_TOKEN",
        "RAILWAY_TOKEN",
    ] {
        if let Ok(t) = std::env::var(var) {
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// CI's spec-drift job sets this so a missing token / unreachable
/// endpoint fails instead of skipping.
fn require_drift() -> bool {
    std::env::var("POCOPINE_REQUIRE_SPEC_DRIFT")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

#[test]
fn railway_schema_still_defines_the_operations_we_use() {
    let Some(token) = railway_token() else {
        let msg =
            "railway spec-drift: no RAILWAY_API_TOKEN / POCOPINE_RAILWAY_TOKEN / RAILWAY_TOKEN set";
        assert!(
            !require_drift(),
            "{msg} (POCOPINE_REQUIRE_SPEC_DRIFT is set)"
        );
        eprintln!("skipping {msg}");
        return;
    };

    let introspection = r#"{"query":"query { __schema { queryType { fields { name } } mutationType { fields { name } } types { name } } }"}"#;

    let resp = reqwest::blocking::Client::new()
        .post(RAILWAY_GRAPHQL)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(introspection)
        .send();

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("railway spec-drift: could not reach {RAILWAY_GRAPHQL}: {e}");
            assert!(
                !require_drift(),
                "{msg} (POCOPINE_REQUIRE_SPEC_DRIFT is set)"
            );
            eprintln!("skipping {msg}");
            return;
        }
    };

    assert!(
        resp.status().is_success(),
        "railway introspection returned HTTP {}",
        resp.status(),
    );
    let body: serde_json::Value = resp.json().expect("introspection response is JSON");
    let schema = &body["data"]["__schema"];

    let names = |v: &serde_json::Value| -> HashSet<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    let mutations = names(&schema["mutationType"]["fields"]);
    let queries = names(&schema["queryType"]["fields"]);
    let types = names(&schema["types"]);

    assert!(
        !mutations.is_empty() && !types.is_empty(),
        "railway introspection returned an empty schema — check the token and endpoint",
    );

    for m in REQUIRED_MUTATIONS {
        assert!(
            mutations.contains(*m),
            "Railway schema no longer defines mutation `{m}` — reconcile crates/pocopine-deploy-railway/src/client.rs",
        );
    }
    for q in REQUIRED_QUERIES {
        assert!(
            queries.contains(*q),
            "Railway schema no longer defines query `{q}` — reconcile crates/pocopine-deploy-railway/src/client.rs",
        );
    }
    for t in REQUIRED_INPUT_TYPES {
        assert!(
            types.contains(*t),
            "Railway schema no longer defines input type `{t}` — reconcile crates/pocopine-deploy-railway/src/client.rs",
        );
    }
}
