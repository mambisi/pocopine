//! Generated watchers borrow state immutably while preserving their scope,
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
    fn on_a(&self, next: u32, prev: Option<u32>) {
        assert_eq!(current_scope_id(), WATCH_SCOPE.with(Cell::get));
        assert_eq!(this::<Self>().with(|state| state.a), self.a);
        SINGLE.with(|runs| runs.borrow_mut().push((next, prev)));
        if next == 7 {
            // Browser APIs may synchronously dispatch back into a component.
            // The mutating event handler must wait until this shared borrow
            // has ended, just as it did with the previous mutable dispatch.
            let button = pocopine::refs::get_on(current_scope_id().unwrap(), "button").unwrap();
            button.dyn_into::<web_sys::HtmlElement>().unwrap().click();
            assert_eq!(self.clicks, 0);
        }
    }

    #[watch(a, b)]
    fn on_inputs(&self) {
        assert_eq!(current_scope_id(), WATCH_SCOPE.with(Cell::get));
        assert_eq!(
            this::<Self>().with(|state| (state.a, state.b)),
            (self.a, self.b)
        );
        MULTI.with(|runs| runs.borrow_mut().push((self.a, self.b)));
    }

    fn on_click(&mut self) {
        self.clicks += 1;
    }
}

async fn settle() {
    for _ in 0..12 {
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
    }
}

#[wasm_bindgen_test]
async fn shared_watchers_preserve_delivery_context_and_safe_reentry() {
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
    MULTI.with(|runs| assert_eq!(runs.borrow().last(), Some(&(7, 2))));
    flush_sync();

    // Watch dispatch does not fingerprint state or acquire a mutable borrow.
    #[cfg(any(debug_assertions, feature = "devtools"))]
    {
        let fingerprints = pocopine_core::scope::fingerprint_count();
        let shared = handle.borrow();
        pocopine::__private::invoke_watch_handler::<ReadonlyWatch>(sid, |state| {
            assert_eq!(state.a, shared.a);
            assert_eq!(current_scope_id(), Some(sid));
        });
        assert_eq!(pocopine_core::scope::fingerprint_count(), fingerprints);
    }

    // A queued callback cannot outlive its component.
    handle.update(|state| state.a = 9);
    let runs_before_unmount = SINGLE.with(|runs| runs.borrow().len());
    let multi_before_unmount = MULTI.with(|runs| runs.borrow().len());
    mounted.unmount();
    flush_sync();
    pocopine::__private::invoke_watch_handler::<ReadonlyWatch>(sid, |_| {
        panic!("removed scopes must not run watcher callbacks");
    });
    SINGLE.with(|runs| assert_eq!(runs.borrow().len(), runs_before_unmount));
    MULTI.with(|runs| assert_eq!(runs.borrow().len(), multi_before_unmount));
    set_auto_flush(true);
    host.remove();
}
