//! RFC 060 Tier 1 — `uses = [...]` is the authoritative
//! component registry: registering a consumer transitively pulls
//! every type in its `uses` list along.
//!
//! Mounts only the root and asserts that the leaf and mid both
//! appear in the runtime template registry. Locks two
//! correctness properties:
//!
//!   - **Transitivity**: nested `uses` chains close over the
//!     full graph (Root → Mid → Leaf).
//!   - **No global magic**: components NOT reachable from Root's
//!     `uses` graph are absent from the registry.
//!   - **Cycle safety**: `A.uses=[B]`, `B.uses=[A]` terminates
//!     because the macro emits a cycle/dedupe guard at the top
//!     of every `register()` body.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test uses_registry_transitive`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine_core::templates::registered_template_names;
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

// ─── transitive-closure fixtures ────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-leaf",
    template = poco! {<span class="urt-leaf"></span>}
)]
struct UrtLeaf {}

#[handlers]
impl UrtLeaf {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-mid",
    template = poco! {<div class="urt-mid"><urt-leaf></urt-leaf></div>},
    uses = [UrtLeaf],
)]
struct UrtMid {}

#[handlers]
impl UrtMid {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-root",
    template = poco! {<div class="urt-root"><urt-mid></urt-mid></div>},
    uses = [UrtMid],
)]
struct UrtRoot {}

#[handlers]
impl UrtRoot {}

/// Sibling component, NOT reachable from `UrtRoot`'s `uses`
/// closure. Has its own `register()` body but the test never
/// calls it — used to prove the registry has no global magic.
#[allow(dead_code)]
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-sibling",
    template = poco! {<span class="urt-sibling"></span>}
)]
struct UrtSibling {}

#[handlers]
impl UrtSibling {}

// ─── cycle fixtures ─────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-cycle-a",
    template = poco! {<span class="urt-cycle-a"></span>},
    uses = [UrtCycleB],
)]
struct UrtCycleA {}

#[handlers]
impl UrtCycleA {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "urt-cycle-b",
    template = poco! {<span class="urt-cycle-b"></span>},
    uses = [UrtCycleA],
)]
struct UrtCycleB {}

#[handlers]
impl UrtCycleB {}

// ─── tests ──────────────────────────────────────────────────────

/// Mounting only `UrtRoot::register()` registers the entire
/// `uses` closure: Root → Mid → Leaf all appear in the runtime
/// template registry.
#[wasm_bindgen_test]
fn root_register_brings_full_uses_closure() {
    UrtRoot::register();

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "urt-root"),
        "UrtRoot self-registers (sanity)",
    );
    assert!(
        names.iter().any(|n| n == "urt-mid"),
        "UrtMid registered transitively via Root.uses=[Mid]",
    );
    assert!(
        names.iter().any(|n| n == "urt-leaf"),
        "UrtLeaf registered transitively via Mid.uses=[Leaf]",
    );
}

/// `UrtSibling` is not reachable from `UrtRoot`'s `uses` closure
/// and the test never calls `UrtSibling::register()`. The
/// registry must NOT contain it — proves the macro doesn't
/// silently auto-register every component the linker can see.
#[wasm_bindgen_test]
fn unreachable_sibling_is_not_registered() {
    UrtRoot::register();

    let names = registered_template_names();
    assert!(
        !names.iter().any(|n| n == "urt-sibling"),
        "UrtSibling has no path from Root.uses; must not appear in the registry",
    );
}

/// Cyclic `uses` graph (`A.uses=[B]`, `B.uses=[A]`) terminates
/// thanks to the macro-emitted `mark_registered::<Self>()` guard
/// at the top of every `register()` body. Both ends end up
/// registered.
#[wasm_bindgen_test]
fn cyclic_uses_graph_terminates_and_registers_both() {
    UrtCycleA::register();

    let names = registered_template_names();
    assert!(
        names.iter().any(|n| n == "urt-cycle-a"),
        "UrtCycleA registers itself",
    );
    assert!(
        names.iter().any(|n| n == "urt-cycle-b"),
        "UrtCycleB registers transitively even though the graph is cyclic",
    );
}
