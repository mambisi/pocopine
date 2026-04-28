//! RFC 060 Tier 4 — `app!{}` macro emits a `&'static
//! phf::Map<&'static str, &'static ComponentVTable>` populated
//! from an explicit `components: [...]` list. The phf map is
//! the static surface; the existing thread-local runtime
//! registries still back lookups so today's mount path keeps
//! working unchanged.
//!
//! V1 scope: prove the plumbing — phf map literal compiles,
//! `App::run_with_registry` walks each vtable's `register` fn
//! (transitively idempotent via the Tier 1 guard), every
//! reachable component lands in the runtime template registry.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test phf_registry`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine::ComponentVTable;
use pocopine_core::templates::registered_template_names;
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(name = "phf-home", template_inline = r#"<div class="phf-home"></div>"#)]
struct PhfHome {}

#[handlers]
impl PhfHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-about",
    template_inline = r#"<div class="phf-about"></div>"#
)]
struct PhfAbout {}

#[handlers]
impl PhfAbout {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-leaf",
    template_inline = r#"<span class="phf-leaf"></span>"#
)]
struct PhfLeaf {}

#[handlers]
impl PhfLeaf {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-host",
    template_inline = r#"<div class="phf-host"><phf-leaf></phf-leaf></div>"#,
    uses = [PhfLeaf],
)]
struct PhfHost {}

#[handlers]
impl PhfHost {}

// ─── tests ──────────────────────────────────────────────────────

/// Each `#[component]` emits a `__POCO_VTABLE` const in
/// `.rodata`. Confirm the static carries the right name + a
/// callable `register` fn.
#[wasm_bindgen_test]
fn vtable_const_carries_name_and_register() {
    let v: &'static ComponentVTable = PhfHome::__POCO_VTABLE;
    assert_eq!(v.name, "phf-home");
    (v.register)();
    assert!(
        registered_template_names().iter().any(|n| n == "phf-home"),
        "vtable.register() must populate the runtime template registry",
    );
}

/// `app!{}` emits a phf map that, when walked through
/// `App::run_with_registry`, registers every entry. The map
/// itself is `&'static` — proves the const surface compiles.
#[wasm_bindgen_test]
fn phf_map_keys_match_component_names() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-home" => PhfHome::__POCO_VTABLE,
        "phf-about" => PhfAbout::__POCO_VTABLE,
    };

    assert!(REGISTRY.contains_key("phf-home"));
    assert!(REGISTRY.contains_key("phf-about"));
    assert!(!REGISTRY.contains_key("not-registered"));

    let home = REGISTRY.get("phf-home").expect("phf-home in map");
    assert_eq!(home.name, "phf-home");
}

/// Walking the map and calling each vtable's `register` fn is
/// what `App::run_with_registry` does. Confirm transitive
/// closure works through the same path: registering the host
/// also registers the leaf via the host's `uses = [PhfLeaf]`
/// (Tier 1 transitivity).
#[wasm_bindgen_test]
fn run_with_registry_walks_vtables_and_transitive_uses() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-host" => PhfHost::__POCO_VTABLE,
    };

    for vtable in REGISTRY.values() {
        (vtable.register)();
    }

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "phf-host"),
        "host registered via vtable",
    );
    assert!(
        names.iter().any(|n| n == "phf-leaf"),
        "leaf registered transitively through host's `uses` list (Tier 1 + Tier 4 compose)",
    );
}
