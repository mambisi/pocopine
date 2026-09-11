//! Snapshot delivery, restricted multi-field commits, and model writeback.
#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine::{current_scope_id, flush_sync, set_auto_flush};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static OWNER: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static INPUTS: RefCell<Vec<(FieldUpdate<u32>, FieldUpdate<u32>)>> = const { RefCell::new(Vec::new()) };
    static OBSERVED: RefCell<Vec<(u32, String)>> = const { RefCell::new(Vec::new()) };
    static MODELS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "watch-update-fixture", template = poco! { <div><span pp-text="summary"></span></div> })]
struct Fixture {
    a: u32,
    b: u32,
    #[model]
    total: u32,
    label: String,
    issue: Option<String>,
    summary: String,
}

#[handlers]
impl Fixture {
    fn on_mount(&mut self) {
        self.issue = Some("initial".into());
    }
    fn on_ready(&self) {
        OWNER.with(|s| s.set(current_scope_id()));
    }

    #[watch(a, b, writes(total, label, issue))]
    fn sum(b: FieldUpdate<u32>, a: FieldUpdate<u32>) -> Update<Self> {
        assert!(pocopine_core::reactive::current_effect().is_none());
        INPUTS.with(|v| v.borrow_mut().push((a.clone(), b.clone())));
        if a.current == 99 {
            return Update::new();
        }
        let total = a.current + b.current;
        let update = Update::new().total(total).label(format!("total {total}"));
        if total > 0 {
            update.issue(None)
        } else {
            update
        }
    }

    #[watch(total, label)]
    fn observe(total: FieldUpdate<u32>, label: FieldUpdate<String>) {
        assert!(pocopine_core::reactive::current_effect().is_none());
        OBSERVED.with(|v| v.borrow_mut().push((total.current, label.current)));
    }

    #[watch(total, writes(summary))]
    fn summarize(total: FieldUpdate<u32>) -> Update<Self> {
        Update::new().summary(format!("Sum = {}", total.current))
    }
}

async fn settle() {
    for _ in 0..16 {
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
    }
}

#[wasm_bindgen_test]
async fn snapshots_and_patches_preserve_coalescing_and_commit_together() {
    INPUTS.with(|v| v.borrow_mut().clear());
    OBSERVED.with(|v| v.borrow_mut().clear());
    MODELS.with(|v| v.borrow_mut().clear());
    let doc = web_sys::window().unwrap().document().unwrap();
    let host = doc.create_element("div").unwrap();
    doc.body().unwrap().append_child(&host).unwrap();
    let listener = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::CustomEvent)>::new(
        |event: web_sys::CustomEvent| {
            MODELS.with(|v| v.borrow_mut().push(event.detail().as_f64().unwrap() as u32));
        },
    );
    host.add_event_listener_with_callback("pp:update:total", listener.as_ref().unchecked_ref())
        .unwrap();
    let mounted = App::mount_subtree::<Fixture>(&host);
    settle().await;
    let sid = OWNER.with(Cell::get).unwrap();
    let handle = Handle::new(Scope::find(sid).unwrap().typed::<Fixture>().unwrap(), sid);
    INPUTS.with(|v| {
        assert_eq!(
            *v.borrow(),
            vec![(
                FieldUpdate {
                    current: 0,
                    previous: None
                },
                FieldUpdate {
                    current: 0,
                    previous: None
                }
            )]
        )
    });
    assert_eq!(
        handle.with(|s| s.issue.clone()),
        Some("initial".into()),
        "omission leaves the field alone"
    );

    handle.update(|s| {
        s.a = 2;
        s.b = 3;
    });
    flush_sync();
    settle().await;
    assert_eq!(
        handle.with(|s| (s.total, s.label.clone(), s.issue.clone(), s.summary.clone())),
        (5, "total 5".into(), None, "Sum = 5".into())
    );
    INPUTS.with(|v| assert_eq!(v.borrow().len(), 2));
    OBSERVED.with(|v| {
        assert!(
            v.borrow()
                .iter()
                .all(|(total, label)| *label == format!("total {total}")),
            "no intermediate patch state is observable"
        )
    });
    MODELS.with(|v| assert_eq!(*v.borrow(), vec![5]));

    handle.update(|s| s.a = 4);
    flush_sync();
    INPUTS.with(|v| {
        assert_eq!(
            v.borrow().last(),
            Some(&(
                FieldUpdate {
                    current: 4,
                    previous: Some(2)
                },
                FieldUpdate {
                    current: 3,
                    previous: Some(3)
                }
            ))
        )
    });
    assert_eq!(handle.with(|s| s.total), 7);
    handle.update(|s| s.a = 99);
    flush_sync();
    assert_eq!(
        handle.with(|s| (s.total, s.label.clone())),
        (7, "total 7".into())
    );

    set_auto_flush(false);
    handle.update(|s| s.b = 100);
    let count = INPUTS.with(|v| v.borrow().len());
    mounted.unmount();
    flush_sync();
    INPUTS.with(|v| assert_eq!(v.borrow().len(), count));
    set_auto_flush(true);
    host.remove_event_listener_with_callback("pp:update:total", listener.as_ref().unchecked_ref())
        .unwrap();
    host.remove();
}
