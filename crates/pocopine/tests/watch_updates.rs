//! Snapshot delivery, restricted multi-field commits, and model writeback.
#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine::{current_scope_id, flush_sync, set_auto_flush};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
struct Counted {
    kind: u8,
    value: u32,
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        if self.kind == 1 {
            OWNED_CLONES.with(|n| n.set(n.get() + 1));
        }
        Self {
            kind: self.kind,
            value: self.value,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct NotClone {
    value: u32,
}

thread_local! {
    static OWNED_CLONES: Cell<usize> = const { Cell::new(0) };
    static MIXED_OWNER: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static MIXED_INPUTS: RefCell<Vec<(u32, u32, u32, Option<u32>)>> = const { RefCell::new(Vec::new()) };
    static ALL_OWNER: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static ALL_INPUTS: RefCell<Vec<(Change<u32>, Change<String>)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "watch-mixed-fixture", template = poco! { <div></div> })]
struct MixedFixture {
    owned: Counted,
    borrowed: NotClone,
    history: Counted,
    output: u32,
}

#[handlers]
impl MixedFixture {
    fn on_mount(&mut self) {
        self.owned = Counted { kind: 1, value: 2 };
        self.borrowed.value = 3;
        self.history.value = 4;
    }
    fn on_ready(&self) {
        MIXED_OWNER.with(|s| s.set(current_scope_id()));
    }

    #[watch(owned, borrowed, history, updates(output))]
    fn sum(owned: Counted, borrowed: &NotClone, history: Change<Counted>) -> Update<Self> {
        MIXED_INPUTS.with(|runs| {
            runs.borrow_mut().push((
                owned.value,
                borrowed.value,
                history.current.value,
                history.previous.map(|v| v.value),
            ))
        });
        Update::new().output(owned.value + borrowed.value + history.current.value)
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "watch-all-fixture", template = poco! { <div pp-text="doubled"></div> })]
struct AllFixture {
    count: u32,
    label: String,
    #[serde(skip)]
    cache: NotClone,
}

#[handlers]
impl AllFixture {
    fn on_ready(&self) {
        ALL_OWNER.with(|s| s.set(current_scope_id()));
    }
    #[computed]
    fn doubled(count: u32) -> u32 {
        count * 2
    }
    #[watch]
    fn observe(changes: Changes<Self>) {
        ALL_INPUTS.with(|runs| runs.borrow_mut().push((changes.count, changes.label)));
    }
}

thread_local! {
    static OWNER: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static INPUTS: RefCell<Vec<(Change<u32>, Change<u32>)>> = const { RefCell::new(Vec::new()) };
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

    #[watch(a, b)]
    fn sum(
        b: Change<u32>,
        a: Change<u32>,
    ) -> Update<Self, (Self::Total, Self::Label, Self::Issue)> {
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
    fn observe(total: Change<u32>, label: Change<String>) {
        assert!(pocopine_core::reactive::current_effect().is_none());
        OBSERVED.with(|v| v.borrow_mut().push((total.current, label.current)));
    }

    #[watch(total, updates(summary))]
    fn summarize(total: Change<u32>) -> Update<Self> {
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
async fn mixed_inputs_borrow_without_clone_and_retain_only_requested_history() {
    OWNED_CLONES.with(|n| n.set(0));
    MIXED_INPUTS.with(|runs| runs.borrow_mut().clear());
    let doc = web_sys::window().unwrap().document().unwrap();
    let host = doc.create_element("div").unwrap();
    doc.body().unwrap().append_child(&host).unwrap();
    let mounted = App::mount_subtree::<MixedFixture>(&host);
    settle().await;
    let sid = MIXED_OWNER.with(Cell::get).unwrap();
    let handle = Handle::new(
        Scope::find(sid).unwrap().typed::<MixedFixture>().unwrap(),
        sid,
    );
    assert_eq!(
        handle.with(|s| s.output),
        9,
        "patch commits after borrowed inputs are released"
    );
    MIXED_INPUTS.with(|runs| assert_eq!(*runs.borrow(), vec![(2, 3, 4, None)]));
    assert_eq!(
        OWNED_CLONES.with(Cell::get),
        1,
        "owned current-only inputs are not cloned into history"
    );

    handle.update(|s| s.borrowed.value = 7);
    flush_sync();
    settle().await;
    assert_eq!(handle.with(|s| s.output), 13);
    MIXED_INPUTS.with(|runs| assert_eq!(runs.borrow().last(), Some(&(2, 7, 4, Some(4)))));
    assert_eq!(
        OWNED_CLONES.with(Cell::get),
        2,
        "no previous snapshot is cloned for current-only inputs"
    );

    handle.update(|s| s.history.value = 8);
    flush_sync();
    settle().await;
    MIXED_INPUTS.with(|runs| assert_eq!(runs.borrow().last(), Some(&(2, 7, 8, Some(4)))));
    assert_eq!(OWNED_CLONES.with(Cell::get), 3);
    mounted.unmount();
    host.remove();
}

#[wasm_bindgen_test]
async fn bare_observer_coalesces_fields_and_releases_queued_work() {
    ALL_INPUTS.with(|runs| runs.borrow_mut().clear());
    let doc = web_sys::window().unwrap().document().unwrap();
    let host = doc.create_element("div").unwrap();
    doc.body().unwrap().append_child(&host).unwrap();
    let mounted = App::mount_subtree::<AllFixture>(&host);
    settle().await;
    let sid = ALL_OWNER.with(Cell::get).unwrap();
    let handle = Handle::new(
        Scope::find(sid).unwrap().typed::<AllFixture>().unwrap(),
        sid,
    );
    ALL_INPUTS.with(|runs| {
        assert_eq!(
            *runs.borrow(),
            vec![(
                Change {
                    current: 0,
                    previous: None
                },
                Change {
                    current: String::new(),
                    previous: None
                },
            )]
        )
    });

    set_auto_flush(false);
    handle.update(|s| s.count = 1);
    handle.update(|s| {
        s.count = 2;
        s.label = "two".into();
    });
    flush_sync();
    ALL_INPUTS.with(|runs| {
        assert_eq!(runs.borrow().len(), 2);
        assert_eq!(
            runs.borrow().last(),
            Some(&(
                Change {
                    current: 2,
                    previous: Some(0)
                },
                Change {
                    current: "two".into(),
                    previous: Some(String::new())
                },
            ))
        );
    });
    handle.update(|s| s.cache.value = 1);
    flush_sync();
    ALL_INPUTS.with(|runs| {
        assert_eq!(
            runs.borrow().len(),
            2,
            "skipped fields do not trigger bare observers"
        )
    });
    handle.update(|s| s.count = 3);
    mounted.unmount();
    flush_sync();
    ALL_INPUTS.with(|runs| assert_eq!(runs.borrow().len(), 2));
    set_auto_flush(true);
    host.remove();
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
                Change {
                    current: 0,
                    previous: None
                },
                Change {
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
                Change {
                    current: 4,
                    previous: Some(2)
                },
                Change {
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
