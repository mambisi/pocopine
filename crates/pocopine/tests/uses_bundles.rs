//! RFC 060 Tier 3 — `#[component(extends = [...])]` bundle
//! markers transitively register every member when the bundle's
//! own `register()` runs.
//!
//! Locks three behaviours:
//!
//!   - **Single bundle**: `Bundle { extends = [A, B] }` →
//!     `Bundle::register()` registers both A and B.
//!   - **Nested bundle**: `Outer { extends = [Inner] }`,
//!     `Inner { extends = [Leaf] }` → `Outer::register()`
//!     registers Leaf.
//!   - **Cyclic bundle**: `A { extends = [B] }`,
//!     `B { extends = [A] }` → `A::register()` terminates and
//!     both ends register (cycle is broken by the Tier 1
//!     `mark_registered` guard, not compile-time detection).
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test uses_bundles`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine_core::templates::registered_template_names;
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

// ─── single-bundle fixtures ─────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ub-leaf-a",
    template = poco! {<span class="ub-leaf-a"></span>}
)]
struct UbLeafA {}

#[handlers]
impl UbLeafA {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ub-leaf-b",
    template = poco! {<span class="ub-leaf-b"></span>}
)]
struct UbLeafB {}

#[handlers]
impl UbLeafB {}

#[derive(Default)]
#[component(extends = [UbLeafA, UbLeafB])]
struct UbBundle;

// ─── nested-bundle fixtures ─────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ub-nested-leaf",
    template = poco! {<span class="ub-nested-leaf"></span>}
)]
struct UbNestedLeaf {}

#[handlers]
impl UbNestedLeaf {}

#[derive(Default)]
#[component(extends = [UbNestedLeaf])]
struct UbInner;

#[derive(Default)]
#[component(extends = [UbInner])]
struct UbOuter;

// ─── cycle fixtures ─────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ub-cycle-leaf-a",
    template = poco! {<span class="ub-cycle-leaf-a"></span>}
)]
struct UbCycleLeafA {}

#[handlers]
impl UbCycleLeafA {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "ub-cycle-leaf-b",
    template = poco! {<span class="ub-cycle-leaf-b"></span>}
)]
struct UbCycleLeafB {}

#[handlers]
impl UbCycleLeafB {}

// Two bundles whose `extends` lists reference each other.
// Calling either one's `register()` must terminate.
#[derive(Default)]
#[component(extends = [UbCycleLeafA, UbCycleBundleB])]
struct UbCycleBundleA;

#[derive(Default)]
#[component(extends = [UbCycleLeafB, UbCycleBundleA])]
struct UbCycleBundleB;

// ─── tests ──────────────────────────────────────────────────────

#[wasm_bindgen_test]
fn single_bundle_registers_every_member() {
    UbBundle::register();

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "ub-leaf-a"),
        "UbBundle.register() must register UbLeafA",
    );
    assert!(
        names.iter().any(|n| n == "ub-leaf-b"),
        "UbBundle.register() must register UbLeafB",
    );
}

#[wasm_bindgen_test]
fn nested_bundle_resolves_through_intermediates() {
    UbOuter::register();

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "ub-nested-leaf"),
        "UbOuter → UbInner → UbNestedLeaf chain must register the leaf",
    );
}

#[wasm_bindgen_test]
fn cyclic_bundle_terminates_via_runtime_guard() {
    UbCycleBundleA::register();

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "ub-cycle-leaf-a"),
        "Cycle's leaf A must register",
    );
    assert!(
        names.iter().any(|n| n == "ub-cycle-leaf-b"),
        "Cycle's leaf B must register (reached via the back-edge)",
    );
}
