//! RFC-113 N1 — typed owned subtree mounting.

use pocopine::prelude::*;
use pocopine::{MountInitError, MountableComponent};
use serde::{Deserialize, Serialize};

pocopine::create_context!(MOUNTED_SUBTREE_CONTEXT: String);

thread_local! {
    static SETUP_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static MOUNT_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static UNMOUNT_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-owned-mount-fixture",
    template = poco! {<div class="typed-owned-mount"><span pp-text="value"></span></div>}
)]
struct TypedOwnedMountFixture {
    #[prop]
    value: String,
    setup_seen: String,
    mount_seen: String,
}

#[handlers]
impl TypedOwnedMountFixture {
    fn on_setup(&mut self) {
        SETUP_COUNT.with(|count| count.set(count.get() + 1));
        let context = MOUNTED_SUBTREE_CONTEXT.inject().unwrap_or_default();
        self.setup_seen = format!("{}:{context}", self.value);
    }

    fn on_mount(&mut self) {
        MOUNT_COUNT.with(|count| count.set(count.get() + 1));
        self.mount_seen = self.value.clone();
    }

    fn on_unmount(&mut self) {
        UNMOUNT_COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[test]
fn component_macro_emits_mountable_component_contract() {
    fn assert_mountable<C: MountableComponent>() {}
    assert_mountable::<TypedOwnedMountFixture>();
}

#[test]
fn mount_init_error_is_structured() {
    let error = MountInitError::new("invalid semantic node");
    assert_eq!(error.message(), "invalid semantic node");
    assert_eq!(error.to_string(), "invalid semantic node");
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use pocopine::{App, Component, ComponentState, MountError};
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    wasm_bindgen_test_configure!(run_in_browser);

    thread_local! {
        static PRE_RELEASE_ORDER: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    #[derive(Default, Serialize, Deserialize)]
    #[component(
        name = "typed-pre-release-child",
        template = poco! {<span class="typed-pre-release-child">child</span>}
    )]
    struct TypedPreReleaseChild;

    #[handlers]
    impl TypedPreReleaseChild {
        fn on_unmount(&mut self) {
            PRE_RELEASE_ORDER.with(|order| order.borrow_mut().push("child"));
        }
    }

    #[derive(Default, Serialize, Deserialize)]
    #[component(
        name = "typed-pre-release-parent",
        template = poco! {<div class="typed-pre-release-parent"><typed-pre-release-child></typed-pre-release-child></div>},
        uses = [TypedPreReleaseChild]
    )]
    struct TypedPreReleaseParent;

    #[handlers]
    impl TypedPreReleaseParent {
        fn on_unmount(&mut self) {
            PRE_RELEASE_ORDER.with(|order| order.borrow_mut().push("parent"));
        }
    }

    fn document() -> web_sys::Document {
        web_sys::window().unwrap().document().unwrap()
    }

    fn reset_counts() {
        SETUP_COUNT.with(|count| count.set(0));
        MOUNT_COUNT.with(|count| count.set(0));
        UNMOUNT_COUNT.with(|count| count.set(0));
    }

    fn counter(counter: &'static std::thread::LocalKey<std::cell::Cell<u32>>) -> u32 {
        counter.with(std::cell::Cell::get)
    }

    #[wasm_bindgen_test]
    fn initializer_precedes_setup_and_mount_and_returns_a_live_handle() {
        reset_counts();
        let host = document()
            .create_element("typed-owned-mount-fixture")
            .unwrap();
        host.set_attribute("value", "from-static-prop").unwrap();
        document().body().unwrap().append_child(&host).unwrap();

        let mounted =
            App::mount_subtree_with::<TypedOwnedMountFixture, _>(&host, |state, setup| {
                assert_eq!(state.value, "from-static-prop");
                state.value = "from-initializer".into();
                setup.provide(&MOUNTED_SUBTREE_CONTEXT, "provided".into());
                Ok(())
            })
            .expect("typed mount succeeds");

        assert!(mounted.is_active());
        assert_eq!(counter(&SETUP_COUNT), 1);
        assert_eq!(counter(&MOUNT_COUNT), 1);
        let handle = mounted.handle();
        handle.with(|state| {
            assert_eq!(state.value, "from-initializer");
            assert_eq!(state.setup_seen, "from-initializer:provided");
            assert_eq!(state.mount_seen, "from-initializer");
        });
        assert_eq!(
            host.query_selector(".typed-owned-mount span")
                .unwrap()
                .unwrap()
                .text_content()
                .as_deref(),
            Some("from-initializer")
        );

        handle.update(|state| state.value = "through-handle".into());
        pocopine::flush_sync();
        assert_eq!(
            host.query_selector(".typed-owned-mount span")
                .unwrap()
                .unwrap()
                .text_content()
                .as_deref(),
            Some("through-handle")
        );

        mounted.unmount();
        assert_eq!(counter(&UNMOUNT_COUNT), 1);
        assert_eq!(host.inner_html(), "");
        host.remove();
    }

    #[wasm_bindgen_test]
    fn initializer_error_rolls_back_without_lifecycle_or_scope_leak() {
        reset_counts();
        let host = document()
            .create_element("typed-owned-mount-fixture")
            .unwrap();
        host.set_inner_html("<span class=\"fallback\">fallback</span>");
        document().body().unwrap().append_child(&host).unwrap();
        let baseline = pocopine::Scope::count();

        let result =
            App::mount_subtree_with::<TypedOwnedMountFixture, _>(&host, |_state, _setup| {
                Err(MountInitError::new("rejected"))
            });
        assert!(matches!(
            result,
            Err(MountError::Initialization { ref source, .. })
                if source.message() == "rejected"
        ));
        assert_eq!(pocopine::Scope::count(), baseline);
        assert_eq!(counter(&SETUP_COUNT), 0);
        assert_eq!(counter(&MOUNT_COUNT), 0);
        assert_eq!(counter(&UNMOUNT_COUNT), 0);
        assert!(host.query_selector(".fallback").unwrap().is_some());
        host.remove();
    }

    struct ForgedTypedMount;
    struct WrongConstructedState;

    fn empty_get(_key: &str) -> JsValue {
        JsValue::UNDEFINED
    }

    impl ComponentState for ForgedTypedMount {
        fn get(&self, key: &str) -> JsValue {
            empty_get(key)
        }
        fn set(&mut self, _key: &str, _value: JsValue) {}
        fn keys(&self) -> &'static [&'static str] {
            &[]
        }
        fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> JsValue {
            JsValue::UNDEFINED
        }
        fn type_name(&self) -> &'static str {
            "forged-typed-mount"
        }
    }

    impl ComponentState for WrongConstructedState {
        fn get(&self, key: &str) -> JsValue {
            empty_get(key)
        }
        fn set(&mut self, _key: &str, _value: JsValue) {}
        fn keys(&self) -> &'static [&'static str] {
            &[]
        }
        fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> JsValue {
            JsValue::UNDEFINED
        }
        fn type_name(&self) -> &'static str {
            "wrong-constructed-state"
        }
    }

    impl Component for ForgedTypedMount {
        const NAME: &'static str = "forged-typed-mount";

        fn register() {
            if !pocopine::__private::mark_registered::<Self>() {
                return;
            }
            pocopine::__private::register_component_with_mount(
                Self::NAME,
                <Self as MountableComponent>::OWNER,
                || {
                    let state = Rc::new(RefCell::new(WrongConstructedState));
                    pocopine::Scope::new(state)
                },
                Some(<Self as Component>::mount_template),
            );
        }
    }

    impl MountableComponent for ForgedTypedMount {
        const OWNER: &'static str = "mounted_subtree_test::ForgedTypedMount";
    }

    #[wasm_bindgen_test]
    fn forged_manual_constructor_is_rejected_by_runtime_downcast() {
        let host = document().create_element("forged-typed-mount").unwrap();
        document().body().unwrap().append_child(&host).unwrap();
        let baseline = pocopine::Scope::count();
        let initializer_ran = std::cell::Cell::new(false);

        let result = App::mount_subtree_with::<ForgedTypedMount, _>(&host, |_state, _setup| {
            initializer_ran.set(true);
            Ok(())
        });
        assert!(matches!(
            result,
            Err(MountError::StateTypeMismatch { actual, .. })
                if actual == "wrong-constructed-state"
        ));
        assert!(!initializer_ran.get());
        assert_eq!(pocopine::Scope::count(), baseline);
        host.remove();
    }

    #[wasm_bindgen_test]
    fn ancestor_teardown_disarms_receipt_and_cleanup_runs_once() {
        reset_counts();
        let ancestor = document().create_element("section").unwrap();
        let host = document()
            .create_element("typed-owned-mount-fixture")
            .unwrap();
        ancestor.append_child(&host).unwrap();
        document().body().unwrap().append_child(&ancestor).unwrap();

        let mounted =
            App::mount_subtree_with::<TypedOwnedMountFixture, _>(&host, |_state, _setup| Ok(()))
                .unwrap();
        assert!(mounted.is_active());

        pocopine::__private::release_compiled_subtree(&ancestor);
        assert_eq!(counter(&UNMOUNT_COUNT), 1);
        assert!(!mounted.is_active());

        drop(mounted);
        assert_eq!(counter(&UNMOUNT_COUNT), 1);
        ancestor.remove();
    }

    #[wasm_bindgen_test]
    fn pre_release_hook_runs_once_before_descendant_component_unmount() {
        PRE_RELEASE_ORDER.with(|order| order.borrow_mut().clear());
        let host = document()
            .create_element("typed-pre-release-parent")
            .unwrap();
        document().body().unwrap().append_child(&host).unwrap();

        let mounted =
            App::mount_subtree_with::<TypedPreReleaseParent, _>(&host, |_state, _setup| Ok(()))
                .unwrap();
        let owner = host
            .query_selector(".typed-pre-release-parent")
            .unwrap()
            .unwrap();
        pocopine::on_before_subtree_release(&owner, || {
            PRE_RELEASE_ORDER.with(|order| order.borrow_mut().push("before"));
        });

        mounted.unmount();
        PRE_RELEASE_ORDER.with(|order| {
            assert_eq!(&*order.borrow(), &["before", "child", "parent"]);
        });
        host.remove();
    }
}
