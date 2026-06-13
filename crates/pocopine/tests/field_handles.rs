//! RFC-097 — end-to-end tests for field handles + the `&self`-skips-
//! the-sweep optimisation, exercising the macro-generated code.
//!
//! 1. `#[component]` emits a `<Name>Fields` extension trait on
//!    `Handle<T>`; `handle.progress()` yields a working
//!    `FieldHandle<f64>` whose `set` updates state + DOM reactively.
//! 2. A `&self` handler dispatches with NO dirty sweep (zero
//!    fingerprints), while a `&mut self` handler still sweeps.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test field_handles`

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use js_sys::Array;
use pocopine::flush_sync;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static HANDLE: RefCell<Option<Handle<FhUploader>>> = const { RefCell::new(None) };
    static PEEKED: RefCell<f64> = const { RefCell::new(-1.0) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "fh-uploader",
    template_inline = r#"<div class="prog" pp-text="progress"></div>"#
)]
struct FhUploader {
    progress: f64,
    status: String,
}

#[handlers]
impl FhUploader {
    // Fires synchronously during the mount walk; stash a handle the
    // tests drive (mirrors how an async task would capture one).
    pub fn on_mount(&mut self) {
        HANDLE.with(|h| *h.borrow_mut() = Some(this::<FhUploader>()));
    }

    // `&mut self` — swept as today.
    pub fn bump(&mut self) {
        self.progress += 1.0;
    }

    // `&self` — read-only; RFC-097 §3.3 says dispatch must skip the
    // sweep entirely. Records what it read so the test sees it ran.
    pub fn peek(&self) {
        PEEKED.with(|p| *p.borrow_mut() = self.progress);
    }
}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount() -> Element {
    FhUploader::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<fh-uploader></fh-uploader>");
    body.append_child(&host).unwrap();
    let el = host.query_selector("fh-uploader").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "fh-uploader");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    host
}

fn read(host: &Element, sel: &str) -> String {
    host.query_selector(sel)
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

// Phase 2 — the generated `FhUploaderFields::progress()` accessor
// yields a `FieldHandle<f64>` whose set writes state + DOM.
#[wasm_bindgen_test]
fn generated_field_handle_accessor_writes_reactively() {
    let host = mount();
    let handle = HANDLE
        .with(|h| h.borrow().clone())
        .expect("on_mount stored a handle");

    let progress: FieldHandle<f64> = handle.progress();
    progress.set(0.5);
    flush_sync();

    assert_eq!(progress.get(), 0.5, "FieldHandle::get reads the write");
    assert_eq!(handle.with(|u| u.progress), 0.5, "Rust state updated");
    assert_eq!(read(&host, ".prog"), "0.5", "pp-text binding re-rendered");

    progress.update(|p| *p += 0.25);
    flush_sync();
    assert_eq!(read(&host, ".prog"), "0.75", "update is a RMW of one field");
}

// Phase 3 — a `&self` handler dispatches with no sweep; `&mut self`
// still sweeps.
#[wasm_bindgen_test]
fn readonly_handler_skips_the_sweep() {
    let _host = mount();
    let handle = HANDLE.with(|h| h.borrow().clone()).expect("handle");
    let scope = Scope::find(handle.scope_id()).expect("scope live");

    handle.progress().set(2.0);
    flush_sync();

    // `peek(&self)` → ZERO fingerprints, but it still runs.
    let fp0 = pocopine_core::scope::fingerprint_count();
    scope.invoke("peek", &Array::new());
    let fp1 = pocopine_core::scope::fingerprint_count();
    assert_eq!(fp1, fp0, "a &self handler must run no fingerprints");
    assert_eq!(PEEKED.with(|p| *p.borrow()), 2.0, "the &self handler ran");

    // `bump(&mut self)` → swept (fingerprints move), state updates.
    scope.invoke("bump", &Array::new());
    let fp2 = pocopine_core::scope::fingerprint_count();
    assert!(fp2 > fp1, "a &mut self handler must run fingerprints");
    assert_eq!(handle.progress().get(), 3.0);
}
