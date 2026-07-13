//! RFC-113 — compiled `pp-owned-content` metadata and path resolution.

use pocopine::prelude::*;
use pocopine::{MountableComponent, OwnedContentOutletComponent};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-outlet-fixture",
    template_inline = r#"
        <section class="shell">
            <header>chrome</header>
            <main class="body"><div class="outlet" pp-owned-content></div></main>
        </section>
    "#
)]
struct OwnedContentOutletFixture;

#[handlers]
impl OwnedContentOutletFixture {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-root-outlet-fixture",
    template_inline = r#"<main class="root-outlet" pp-owned-content></main>"#
)]
struct OwnedContentRootOutletFixture;

#[handlers]
impl OwnedContentRootOutletFixture {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "owned-content-atom-fixture",
    template_inline = r#"<figure class="atom-shell"></figure>"#
)]
struct OwnedContentAtomFixture;

#[handlers]
impl OwnedContentAtomFixture {}

#[test]
fn component_macro_emits_the_owned_content_metadata_contract() {
    fn assert_outlet<C: OwnedContentOutletComponent>() {}

    assert_outlet::<OwnedContentOutletFixture>();
    assert_outlet::<OwnedContentRootOutletFixture>();
    assert_eq!(
        <OwnedContentOutletFixture as MountableComponent>::OWNED_CONTENT_OUTLET_PATH,
        Some(&[1, 0][..])
    );
    assert_eq!(
        <OwnedContentRootOutletFixture as MountableComponent>::OWNED_CONTENT_OUTLET_PATH,
        Some(&[][..])
    );
    assert_eq!(
        <OwnedContentAtomFixture as MountableComponent>::OWNED_CONTENT_OUTLET_PATH,
        None,
    );
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use super::*;

    use pocopine::{
        App, OwnedContentOutletError, resolve_owned_content_outlet,
        resolve_owned_content_outlet_from_root,
    };
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    wasm_bindgen_test_configure!(run_in_browser);

    fn document() -> web_sys::Document {
        web_sys::window().unwrap().document().unwrap()
    }

    #[wasm_bindgen_test]
    fn resolves_the_compiled_path_without_a_selector_scan() {
        let host = document().create_element("div").unwrap();
        document().body().unwrap().append_child(&host).unwrap();
        let mounted = App::mount_subtree_with::<OwnedContentOutletFixture, _>(&host, |_, _| Ok(()))
            .expect("fixture mounts");

        let outlet = resolve_owned_content_outlet::<OwnedContentOutletFixture>(&host)
            .expect("compiled path resolves");
        assert_eq!(outlet.class_name(), "outlet");
        assert!(!outlet.has_attribute("pp-owned-content"));

        let root = host.children().item(0).unwrap();
        let from_root = resolve_owned_content_outlet_from_root::<OwnedContentOutletFixture>(&root)
            .expect("root-relative resolver agrees");
        assert!(outlet.is_same_node(Some(from_root.as_ref())));

        mounted.unmount();
        host.remove();
    }

    #[wasm_bindgen_test]
    fn empty_path_resolves_the_rendered_template_root() {
        let host = document().create_element("div").unwrap();
        let mounted =
            App::mount_subtree_with::<OwnedContentRootOutletFixture, _>(&host, |_, _| Ok(()))
                .expect("fixture mounts");
        let root = host.children().item(0).unwrap();
        let outlet = resolve_owned_content_outlet::<OwnedContentRootOutletFixture>(&host)
            .expect("root outlet resolves");
        assert!(root.is_same_node(Some(outlet.as_ref())));
        mounted.unmount();
    }

    #[wasm_bindgen_test]
    fn stale_paths_and_non_native_targets_fail_closed() {
        let host = document().create_element("div").unwrap();
        let missing = resolve_owned_content_outlet::<OwnedContentOutletFixture>(&host)
            .expect_err("an unmounted host has no template root");
        assert!(matches!(
            missing,
            OwnedContentOutletError::MissingTemplateRoot { .. }
        ));

        let root = document().create_element("section").unwrap();
        root.set_inner_html("<header></header><main></main>");
        let missing = resolve_owned_content_outlet_from_root::<OwnedContentOutletFixture>(&root)
            .expect_err("the second path hop is absent");
        assert!(matches!(
            missing,
            OwnedContentOutletError::MissingPathSegment {
                depth: 1,
                index: 0,
                ..
            }
        ));

        root.set_inner_html("<header></header><main><external-widget></external-widget></main>");
        let non_native = resolve_owned_content_outlet_from_root::<OwnedContentOutletFixture>(&root)
            .expect_err("a stale path into a custom element must fail closed");
        assert!(matches!(
            non_native,
            OwnedContentOutletError::NonNativeTarget { ref tag, .. }
                if tag == "external-widget"
        ));
    }
}
