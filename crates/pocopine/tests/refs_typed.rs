//! RFC 081 Phase 2 — end-to-end test for the macro-generated
//! `<ComponentName>Refs` struct.
//!
//! A `TypedRefsParent` `.poco` template carries
//! `<typed-refs-child pp-ref="body" />`, and the test asserts
//! that:
//!
//! 1. The macro generated a `TypedRefsParentRefs` struct with
//!    a `fn body(&self) -> RefAccessor` method.
//! 2. `RefAccessor::component::<TypedRefsChild>()` resolves a
//!    working `Handle<TypedRefsChild>` after mount.
//! 3. `RefAccessor::component::<TypedRefsParent>()` returns
//!    `None` (mismatched type protection).
//! 4. `RefAccessor::element()` returns the host element.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test refs_typed`

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-child",
    template_inline = r#"<div class="trc">child mounted</div>"#
)]
struct TypedRefsChild {
    seed: u32,
}

#[handlers]
impl TypedRefsChild {
    pub fn on_mount(&mut self) {
        CHILD_MOUNT_COUNT.with(|c| *c.borrow_mut() += 1);
    }
}

impl TypedRefsChild {
    pub fn seed_value(&self) -> u32 {
        self.seed
    }
}

// Test-only observation channel. `#[component]` requires every
// struct field to be `Serialize + Deserialize` (RFC 081 keeps
// this rule), so the observed values live in thread-locals the
// on_ready handler writes and the test reads.
thread_local! {
    static OBSERVED_SEED: RefCell<Option<u32>> = const { RefCell::new(None) };
    static OBSERVED_MISMATCH_IS_NONE: RefCell<Option<bool>> = const { RefCell::new(None) };
    static OBSERVED_ELEMENT_PRESENT: RefCell<Option<bool>> = const { RefCell::new(None) };
    /// Codex P1 regression — counts `on_mount` invocations on
    /// the child. The Phase 1 host stamp used to also be
    /// `SCOPE_ID_KEY`, which made `fire_mount_hook` fire the
    /// child's `on_mount` twice (once for the host, once for
    /// the inner template root). The fix moved the host stamp
    /// to a separate key the lifecycle dispatch ignores. This
    /// counter pins the fix: must equal 1 after a single mount.
    static CHILD_MOUNT_COUNT: RefCell<u32> = const { RefCell::new(0) };
}

fn reset_observations() {
    OBSERVED_SEED.with(|c| *c.borrow_mut() = None);
    OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow_mut() = None);
    OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow_mut() = None);
    CHILD_MOUNT_COUNT.with(|c| *c.borrow_mut() = 0);
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-parent",
    uses = [TypedRefsChild],
    template_inline = r#"<div class="trp">
        <typed-refs-child pp-ref="body"></typed-refs-child>
    </div>"#
)]
struct TypedRefsParent {}

#[handlers]
impl TypedRefsParent {
    pub fn on_ready(&self, refs: TypedRefsParentRefs) {
        // Typed-component resolution
        if let Some(child) = refs.body().component::<TypedRefsChild>() {
            OBSERVED_SEED.with(|c| *c.borrow_mut() = Some(child.with(|c| c.seed_value())));
        }
        // Mismatched type — must return None.
        let mismatched = refs.body().component::<TypedRefsParent>();
        OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow_mut() = Some(mismatched.is_none()));
        // Untyped element lookup
        OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow_mut() = Some(refs.body().element().is_some()));
    }
}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn mount() -> Element {
    TypedRefsChild::register();
    TypedRefsParent::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<typed-refs-parent></typed-refs-parent>");
    body.append_child(&host).unwrap();
    let el = host.query_selector("typed-refs-parent").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "typed-refs-parent");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    host
}

#[wasm_bindgen_test]
async fn generated_refs_struct_resolves_child_component_handle() {
    reset_observations();
    let host = mount();
    tick().await;

    // Typed component resolution worked and the handle read
    // through to child state.
    let observed_seed = OBSERVED_SEED.with(|c| *c.borrow());
    assert_eq!(
        observed_seed,
        Some(0),
        "typed handle reads child state via the generated `body()` accessor"
    );
    // Mismatched type guarded by the `child_scope != parent_scope` check.
    let mismatch_is_none = OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow());
    assert_eq!(
        mismatch_is_none,
        Some(true),
        "component::<WrongType>() must return None"
    );
    // Untyped element lookup composes the same name-baked path.
    let element_present = OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow());
    assert_eq!(
        element_present,
        Some(true),
        "RefAccessor::element() resolves the host element"
    );

    host.remove();
}

#[wasm_bindgen_test]
async fn host_stamp_does_not_double_fire_child_lifecycle() {
    // Codex review P1 regression. With the original Phase 1
    // implementation, the host stamp reused `SCOPE_ID_KEY`,
    // and `finalize_compiled_subtree` would call
    // `fire_mount_hook` once for the host and once for the
    // inner template root, firing the child's `on_mount`
    // twice for a single mount. The fix moves the host
    // stamp to `HOST_CHILD_SCOPE_ID_KEY` (separate from
    // `SCOPE_ID_KEY`) so lifecycle dispatch only sees the
    // inner root.
    reset_observations();
    let host = mount();
    tick().await;
    let mount_count = CHILD_MOUNT_COUNT.with(|c| *c.borrow());
    assert_eq!(
        mount_count, 1,
        "child `on_mount` must fire exactly once per mount"
    );
    host.remove();
}

// Codex P2 regression — a `pp-ref` inside a `pp-if` body
// is collected into a separate nested AnalysisCtx in the
// macro, so the original Phase 2 codegen omitted accessors
// for it. The fix aggregates nested ref names via
// `absorb_lifted_refs`; this component would not compile
// (no `lifted` method on the generated Refs struct) if the
// aggregation regressed.
thread_local! {
    static OBSERVED_LIFTED_REF_PRESENT: RefCell<Option<bool>> = const { RefCell::new(None) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-lifted-parent",
    template_inline = r#"<div class="trlp">
        <template pp-if="true">
            <span pp-ref="lifted">inside pp-if body</span>
        </template>
    </div>"#
)]
struct TypedRefsLiftedParent {}

#[handlers]
impl TypedRefsLiftedParent {
    pub fn on_ready(&self, refs: TypedRefsLiftedParentRefs) {
        // If this line wouldn't compile, the macro regressed
        // (missed the pp-ref inside the pp-if body). The
        // runtime resolution may legitimately return None
        // when the controller hasn't materialized the body
        // yet — but the method must EXIST.
        let element = refs.lifted().element();
        OBSERVED_LIFTED_REF_PRESENT.with(|c| *c.borrow_mut() = Some(element.is_some()));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-conditional",
    template_inline = r#"<div class="tr-conditional">
        <button class="tr-toggle" @click="toggle">toggle</button>
        <template pp-if="open">
            <span pp-ref="conditional">conditional</span>
        </template>
    </div>"#
)]
struct TypedRefsConditional {
    open: bool,
}

#[handlers]
impl TypedRefsConditional {
    pub fn on_setup(&mut self) {
        self.open = true;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[wasm_bindgen_test]
async fn conditional_ref_unregisters_when_its_branch_unmounts() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let handle = pocopine::App::mount_subtree::<TypedRefsConditional>(&host);
    tick().await;
    let root = host.first_element_child().expect("rendered root");
    let scope_id = pocopine_core::mount::scope_id_of_element(&root).expect("scope id");
    assert!(
        pocopine_core::refs::get_on(scope_id, "conditional").is_some(),
        "truthy branch registers its ref"
    );

    host.query_selector(".tr-toggle")
        .unwrap()
        .unwrap()
        .dyn_ref::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;

    assert!(
        pocopine_core::refs::get_on(scope_id, "conditional").is_none(),
        "branch teardown must unregister the detached ref immediately"
    );
    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn generated_refs_struct_includes_pp_refs_from_lifted_bodies() {
    OBSERVED_LIFTED_REF_PRESENT.with(|c| *c.borrow_mut() = None);
    TypedRefsLiftedParent::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<typed-refs-lifted-parent></typed-refs-lifted-parent>");
    body.append_child(&host).unwrap();
    let el = host
        .query_selector("typed-refs-lifted-parent")
        .unwrap()
        .unwrap();
    pocopine_core::mount::mount_child_component(&el, "typed-refs-lifted-parent");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    tick().await;

    // The accessor exists (compile-checked above); the
    // pp-if body should materialize before on_ready fires,
    // so the element lookup resolves.
    let lifted_present = OBSERVED_LIFTED_REF_PRESENT.with(|c| *c.borrow());
    assert_eq!(
        lifted_present,
        Some(true),
        "pp-ref inside pp-if body must be reachable through the typed `lifted()` accessor"
    );
    host.remove();
}

// Codex P2 — kebab/dot/colon in `pp-ref` names normalize to
// underscore for the generated method ident. Two refs whose
// normalized form collides emit a `compile_error!` (covered
// by build-time compilation, not this runtime test). The
// positive case here pins that distinct normalized names
// emit distinct methods.
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-kebab",
    template_inline = r#"<div class="trk">
        <span pp-ref="form-root">a</span>
        <span pp-ref="title-input">b</span>
    </div>"#
)]
struct TypedRefsKebab {}

#[handlers]
impl TypedRefsKebab {
    pub fn on_ready(&self, refs: TypedRefsKebabRefs) {
        // Method idents are snake_case normalized from kebab
        // — `form-root` → `form_root`, `title-input` →
        // `title_input`. If the normalization regressed,
        // these calls would not compile.
        let _ = refs.form_root().element();
        let _ = refs.title_input().element();
    }
}

#[wasm_bindgen_test]
async fn generated_refs_struct_normalizes_kebab_to_snake_case_idents() {
    TypedRefsKebab::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<typed-refs-kebab></typed-refs-kebab>");
    body.append_child(&host).unwrap();
    let el = host.query_selector("typed-refs-kebab").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "typed-refs-kebab");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    tick().await;
    // No runtime assertion — the test passes by virtue of
    // `on_ready` having compiled with both kebab-derived
    // method names.
    host.remove();
}

#[wasm_bindgen_test]
async fn generated_refs_struct_method_name_is_compile_time_typo_safe() {
    // The point of Phase 2: the generated method `fn body(&self)`
    // means a typo at the call site fails to compile. We can't
    // assert that at runtime (the compile already happened), but
    // the test still ensures the generated method is reachable
    // by NAME, not by string — i.e. the macro emitted the right
    // method. If a future refactor regresses to a string lookup,
    // this test's call site would still compile but the runtime
    // would resolve via the wrong path; this test pins both the
    // codegen and the resolution semantics together.
    let host = mount();
    tick().await;
    // The parent's scope id lives on the inner template root,
    // not the custom-element host — `<typed-refs-parent>` is
    // the host; `<div class="trp">` (its first element child)
    // is the rendered root with `SCOPE_ID_KEY`.
    let parent_host = host.query_selector("typed-refs-parent").unwrap().unwrap();
    let parent_root = parent_host
        .first_element_child()
        .expect("rendered template root");
    let parent_scope_id =
        pocopine_core::mount::scope_id_of_element(&parent_root).expect("parent scope id");
    // Directly resolve via the free-fn to confirm the name lands
    // in the parent scope's ref table.
    let resolved = pocopine_core::refs::get_component_on::<TypedRefsChild>(parent_scope_id, "body");
    assert!(
        resolved.is_some(),
        "free-fn lookup via the same name the macro baked in resolves"
    );
    host.remove();
}

#[wasm_bindgen_test]
fn unregistering_an_old_ref_element_does_not_remove_its_replacement() {
    let parent_scope = Scope::new(Rc::new(RefCell::new(TypedRefsParent::default())));
    let old = doc().create_element("span").unwrap();
    let replacement = doc().create_element("span").unwrap();
    pocopine_core::refs::register(parent_scope.id, "body", &old);
    pocopine_core::refs::register(parent_scope.id, "body", &replacement);

    pocopine_core::mount::release_compiled_subtree(&old);

    let current = pocopine_core::refs::get_on(parent_scope.id, "body")
        .expect("replacement ref survives old branch teardown");
    assert_eq!(
        current, replacement,
        "tearing down an old branch must not delete a newer same-name ref"
    );
    Scope::remove(parent_scope.id);
}
