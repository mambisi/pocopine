//! Compile-time contract for macro argument parsing — duplicate keys,
//! ignored arguments, and dead flag combinations must be compile
//! errors, never silent last-one-wins or discarded tokens.
//!
//! Reject fixtures pin our own `compile_error!` text; regenerate with
//! `TRYBUILD=overwrite` if rustc framing drifts.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn macro_args_contract() {
    let cases = trybuild::TestCases::new();
    // #[derive(Props)] fields take bare `#[prop]` — arguments used to
    // be recognized, never parsed, and silently discarded.
    cases.compile_fail("tests/ui/props_args_fail.rs");
    // Duplicate #[component(...)] keys used to last-one-win silently.
    cases.compile_fail("tests/ui/component_dup_key_fail.rs");
    // `unchecked_paths` only accepts "true"/"false" — any other value
    // used to silently disable the opt-out.
    cases.compile_fail("tests/ui/component_unchecked_paths_fail.rs");
    // Duplicate #[job] keys used to last-one-win silently.
    cases.compile_fail("tests/ui/job_dup_queue_fail.rs");
    // `retries` and `max_retries` are aliases — passing both silently
    // kept the later one.
    cases.compile_fail("tests/ui/job_retries_alias_fail.rs");
    // `idempotent` is parsed, stored, and never consulted for
    // streaming functions — the config was dead.
    cases.compile_fail("tests/ui/server_idempotent_streaming_fail.rs");
}
