//! RFC 084 — typed slot props end-to-end compilation fixture.
//!
//! These types exercise the `#[slot(props = T)]` arg + the
//! macro's static- and iterated-mode publication validation
//! against real `#[component]` invocations. The assertion is
//! compilation success: every component below must reach the
//! `cargo check` step without the macro emitting a
//! `compile_error!` or a const-block panic.
//!
//! Macro expansion happens at host compile time, so this file
//! deliberately is **not** gated on `target_arch = "wasm32"` —
//! `cargo check --tests -p pocopine` on a host runs the macro,
//! exercising every validation path. Only the runtime sentinel
//! `#[wasm_bindgen_test]` itself needs the wasm gate.
//!
//! Run with:
//!   `cargo check --tests -p pocopine`
//!   `wasm-pack test --firefox --headless crates/pocopine --test typed_slot_props`

// Compilation IS the test — the structs below are exercised by
// the macro expansion path, not by runtime construction.
#![allow(dead_code)]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[cfg(target_arch = "wasm32")]
wasm_bindgen_test_configure!(run_in_browser);

// ── Static-mode fixture ────────────────────────────────────────

#[derive(Default, Props, Serialize, Deserialize)]
struct StaticHeaderProps {
    #[prop]
    queue_size: usize,
    #[prop]
    all_done: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-slot-static-host",
    template = poco! {
<section class="typed-slot-static-host">
  <slot name="header" :queue_size="queue_size" :all_done="all_done"></slot>
  <div class="body">content</div>
</section>
}
)]
#[slot(name = "header", props = StaticHeaderProps)]
struct TypedSlotStaticHost {
    queue_size: usize,
    all_done: bool,
}

#[handlers]
impl TypedSlotStaticHost {}

// ── Iterated-mode fixture ──────────────────────────────────────
//
// `<slot name="row">` sits inside `pp-for="file in files"` and
// has zero `:LHS=` attrs — Phase 2 dispatches to iterated mode,
// auto-publishes `:file="file"`, and emits the Rust type
// assertion that `files`'s element type matches `UploadFileLite`.

#[derive(Default, Clone, Props, Serialize, Deserialize)]
struct UploadFileLite {
    #[prop]
    name: String,
    #[prop]
    progress: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-slot-iterated-host",
    template = poco! {
<section class="typed-slot-iterated-host">
  <ul>
    <li pp-for="file in files">
      <slot name="row"></slot>
    </li>
  </ul>
</section>
}
)]
#[slot(name = "row", props = UploadFileLite)]
struct TypedSlotIteratedHost {
    files: Vec<UploadFileLite>,
}

#[handlers]
impl TypedSlotIteratedHost {}

// ── Mixed-mode fixture (§5.4) ──────────────────────────────────
//
// `<slot>` sits inside `pp-for` AND has explicit `:LHS=` attrs:
// presence of any publication forces static mode. The macro must
// validate every `#[prop]` field on `IteratedRowFlat` is covered
// by the publication. The Props struct stays flat (one `#[prop]`
// per leaf) — `#[prop(flatten)]` is a `#[component]`-side
// concept, not a `#[derive(Props)]` arg.

#[derive(Default, Clone, Props, Serialize, Deserialize)]
struct IteratedRowFlat {
    #[prop]
    name: String,
    #[prop]
    progress: f64,
    #[prop]
    is_last: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-slot-mixed-host",
    template = poco! {
<section class="typed-slot-mixed-host">
  <ul>
    <li pp-for="file in files">
      <slot name="row" :name="file.name" :progress="file.progress" :is_last="$last"></slot>
    </li>
  </ul>
</section>
}
)]
#[slot(name = "row", props = IteratedRowFlat)]
struct TypedSlotMixedHost {
    files: Vec<UploadFileLite>,
}

#[handlers]
impl TypedSlotMixedHost {}

// ── Backwards-compatibility fixture ────────────────────────────
//
// Existing `#[slot]` sites without `props = T` must keep
// compiling unchanged — no validation runs, no const blocks
// emitted.

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "untyped-slot-host",
    template = poco! {
<section class="untyped-slot-host">
  <slot></slot>
  <slot name="footer"></slot>
</section>
}
)]
#[slot(default)]
#[slot(name = "footer")]
struct UntypedSlotHost {}

#[handlers]
impl UntypedSlotHost {}

// ── Sentinel test ──────────────────────────────────────────────
//
// Reaching this point means every `#[component]` + `#[slot]`
// invocation above expanded without emitting `compile_error!`
// or a const-block panic. That IS the end-to-end assertion.

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
fn typed_slot_fixtures_compile() {
    // No-op — compilation success is the test.
}
