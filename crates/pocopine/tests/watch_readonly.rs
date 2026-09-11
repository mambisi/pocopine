//! Generated snapshot observers preserve their scope,
//! deferred seed, coalescing, callback safe point, and unmount cleanup.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};

use pocopine::current_scope_id;
use pocopine::prelude::*;
use pocopine_core::reactive::{ScopeId, flush_sync, set_auto_flush};
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static WATCH_SCOPE: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static SINGLE: RefCell<Vec<(u32, Option<u32>)>> = const { RefCell::new(Vec::new()) };
    static MULTI: RefCell<Vec<(u32, u32)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "readonly-watch-fixture",
    template = poco! {<div><button pp-ref="button" @click="on_click">Click</button></div>}
)]
struct ReadonlyWatch {
    a: u32,
    b: u32,
    clicks: u32,
}

#[handlers]
impl ReadonlyWatch {
    fn on_ready(&self) {
        WATCH_SCOPE.with(|scope| scope.set(current_scope_id()));
        // Installation must not call the user watchers during on_ready.
        SINGLE.with(|runs| assert!(runs.borrow().is_empty()));
        MULTI.with(|runs| assert!(runs.borrow().is_empty()));
    }

    #[watch(a)]
    fn on_a(a: Change<u32>) {
        let (next, prev) = (a.current, a.previous);
        assert_eq!(current_scope_id(), WATCH_SCOPE.with(Cell::get));
        assert_eq!(this::<Self>().with(|state| state.a), next);
        SINGLE.with(|runs| runs.borrow_mut().push((next, prev)));
        if next == 7 {
            // Browser APIs may synchronously dispatch back into a component.
            // The mutating event handler must wait until evaluation and
            // the callback safe point have ended.
            let button = pocopine::refs::get_on(current_scope_id().unwrap(), "button").unwrap();
            button.dyn_into::<web_sys::HtmlElement>().unwrap().click();
            assert_eq!(this::<Self>().with(|state| state.clicks), 0);
        }
    }

    #[watch(a, b)]
    fn on_inputs(a: &u32, b: u32) {
        assert_eq!(current_scope_id(), WATCH_SCOPE.with(Cell::get));
        assert_eq!(this::<Self>().with(|state| (state.a, state.b)), (*a, b));
        MULTI.with(|runs| runs.borrow_mut().push((*a, b)));
    }

    fn on_click(&mut self) {
        self.clicks += 1;
        self.a += 1;
    }
}

async fn settle() {
    for _ in 0..12 {
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
    }
}

#[wasm_bindgen_test]
async fn snapshot_observers_preserve_delivery_context_and_safe_reentry() {
    SINGLE.with(|runs| runs.borrow_mut().clear());
    MULTI.with(|runs| runs.borrow_mut().clear());
    let document = web_sys::window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&host).unwrap();
    let mounted = App::mount_subtree::<ReadonlyWatch>(&host);
    settle().await;

    SINGLE.with(|runs| assert_eq!(*runs.borrow(), vec![(0, None)]));
    MULTI.with(|runs| assert_eq!(*runs.borrow(), vec![(0, 0)]));
    let sid = WATCH_SCOPE.with(Cell::get).unwrap();
    let scope = Scope::find(sid).unwrap();
    let handle = Handle::new(scope.typed::<ReadonlyWatch>().unwrap(), sid);

    set_auto_flush(false);
    handle.update(|state| {
        state.a = 1;
        state.b = 2;
    });
    flush_sync();
    SINGLE.with(|runs| assert_eq!(*runs.borrow(), vec![(0, None), (1, Some(0))]));
    MULTI.with(|runs| assert_eq!(*runs.borrow(), vec![(0, 0), (1, 2)]));
    assert!(current_scope_id().is_none(), "watch scope must be restored");

    // An unrelated update does not produce watcher callbacks.
    handle.update(|state| state.clicks = 2);
    flush_sync();
    SINGLE.with(|runs| assert_eq!(runs.borrow().len(), 2));
    MULTI.with(|runs| assert_eq!(runs.borrow().len(), 2));

    handle.update(|state| {
        state.clicks = 0;
        state.a = 7;
    });
    flush_sync();
    assert_eq!(handle.with(|state| state.clicks), 1);
    SINGLE.with(|runs| assert_eq!(runs.borrow().last(), Some(&(7, Some(1)))));
    flush_sync();
    SINGLE.with(|runs| assert_eq!(runs.borrow().last(), Some(&(8, Some(7)))));
    MULTI.with(|runs| assert_eq!(runs.borrow().last(), Some(&(8, 2))));

    // A queued callback cannot outlive its component.
    handle.update(|state| state.a = 9);
    let runs_before_unmount = SINGLE.with(|runs| runs.borrow().len());
    let multi_before_unmount = MULTI.with(|runs| runs.borrow().len());
    mounted.unmount();
    flush_sync();
    SINGLE.with(|runs| assert_eq!(runs.borrow().len(), runs_before_unmount));
    MULTI.with(|runs| assert_eq!(runs.borrow().len(), multi_before_unmount));
    set_auto_flush(true);
    host.remove();
}
