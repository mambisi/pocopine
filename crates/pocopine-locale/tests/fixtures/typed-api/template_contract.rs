use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
struct Row {
    id: u32,
    name: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-translation-fixture", template = "TranslationHost.html")]
struct TranslationHost {
    name: String,
    count: u32,
    clicks: u32,
    visible: bool,
    rows: Vec<Row>,
}

#[handlers]
impl TranslationHost {
    fn clicked(&mut self) {
        self.clicks += 1;
    }
}

#[cfg(feature = "template-missing-key")]
#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-missing-key", template = poco! { <p pp-text="$t.common.absent"></p> })]
struct MissingKey;
#[cfg(feature = "template-missing-key")]
#[handlers]
impl MissingKey {}

#[cfg(feature = "template-bad-arity")]
#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-bad-arity", template = poco! { <p pp-text="$t.common.welcome"></p> })]
struct BadArity;
#[cfg(feature = "template-bad-arity")]
#[handlers]
impl BadArity {}

#[cfg(feature = "template-rich-attribute")]
#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-rich-attribute", template = poco! { <input :title="$t.cart.terms"> })]
struct RichAttribute;
#[cfg(feature = "template-rich-attribute")]
#[handlers]
impl RichAttribute {}

#[cfg(feature = "template-dynamic-key")]
#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-dynamic-key", template = poco! { <p pp-text="$t(key)"></p> })]
struct DynamicKey {
    key: String,
}
#[cfg(feature = "template-dynamic-key")]
#[handlers]
impl DynamicKey {}

#[cfg(feature = "template-old-directive")]
#[derive(Default, Serialize, Deserialize)]
#[component(name = "locale-old-directive", template = poco! { <p pp-t="cart.title"></p> })]
struct OldDirective;
#[cfg(feature = "template-old-directive")]
#[handlers]
impl OldDirective {}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn mount_translation_fixture(host: web_sys::Element) {
    let mounted = App::mount_subtree_with::<TranslationHost, _>(&host, |state, _| {
        state.name = "Amina".into();
        state.count = 1;
        Ok(())
    })
    .unwrap();
    // Keep the exported consumer alive so the release artifact audit exercises
    // reachable template installers and cannot pass by dead-stripping them.
    std::mem::forget(mounted);
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser {
    use super::*;
    use crate::t;
    use pocopine::locale::{Locales, client::LocaleController};
    use pocopine_core::{flush_sync, set_auto_flush};
    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    fn templates_update_variables_language_and_rich_elements_without_remounting() {
        set_auto_flush(false);
        t::initialize(
            Locales::new(
                "en".parse().unwrap(),
                ["en", "fr"].map(|l| l.parse().unwrap()),
            )
            .unwrap(),
        )
        .unwrap();
        t::install(
            "en".parse().unwrap(),
            include_bytes!(concat!(env!("OUT_DIR"), "/en.json")),
        )
        .unwrap();
        let ui = LocaleController::new(t::catalogs().unwrap(), "en".parse().unwrap()).unwrap();
        let doc = web_sys::window().unwrap().document().unwrap();
        let host = doc.create_element("div").unwrap();
        doc.body().unwrap().append_child(&host).unwrap();
        let mounted = App::mount_subtree_with::<TranslationHost, _>(&host, |state, _| {
            state.name = "Amina".into();
            state.count = 1;
            state.visible = true;
            state.rows = vec![Row {
                id: 1,
                name: "Sam".into(),
            }];
            Ok(())
        })
        .unwrap();
        flush_sync();
        let element = |selector: &str| host.query_selector(selector).unwrap().unwrap();
        let text = |selector: &str| element(selector).text_content().unwrap();
        assert_eq!(text(".welcome"), "⟦common.welcome⟧");
        ui.activate().unwrap();
        flush_sync();
        assert_eq!(text(".welcome"), "Hello Amina, welcome to Pocopine");
        assert_eq!(text(".items"), "1 item");
        assert_eq!(text(".interp"), text(".welcome"));
        assert_eq!(text(".conditional"), text(".welcome"));
        assert_eq!(
            element(".label").get_attribute("aria-label").unwrap(),
            text(".welcome")
        );
        assert_eq!(text(".row"), "Hello Sam, welcome to Pocopine");
        let n0 = element(".n0");
        let n1 = element(".n1");
        assert!(n0.contains(Some(&n1)));
        let terms = element(".terms");
        let privacy = element(".privacy");
        terms
            .dyn_ref::<web_sys::HtmlElement>()
            .unwrap()
            .focus()
            .unwrap();
        let scope_id = mounted.handle().scope_id();
        let original_ref = pocopine::refs::get_on(scope_id, "terms").unwrap();
        mounted.handle().update(|state| {
            state.name = "<b>No HTML</b>".into();
            state.count = 2;
            state.rows[0].name = "Noor".into();
        });
        flush_sync();
        assert_eq!(text(".items"), "2 items");
        assert!(
            n1.contains(Some(&n0)),
            "nested placeholders may reverse parentage"
        );
        assert!(element(".n0").is_same_node(Some(&n0)));
        assert_eq!(
            text(".welcome"),
            "Hello <b>No HTML</b>, welcome to Pocopine"
        );
        assert!(element(".welcome").query_selector("b").unwrap().is_none());
        ui.begin_switch("fr".parse().unwrap())
            .unwrap()
            .commit(Some(include_bytes!(concat!(env!("OUT_DIR"), "/fr.json"))))
            .unwrap();
        flush_sync();
        assert_eq!(
            text(".welcome"),
            "Bonjour <b>No HTML</b>, bienvenue sur Pocopine"
        );
        assert_eq!(text(".items"), "2 articles");
        assert_eq!(text(".interp"), text(".welcome"));
        assert_eq!(text(".conditional"), text(".welcome"));
        assert_eq!(text(".row"), "Bonjour Noor, bienvenue sur Pocopine");
        assert_eq!(
            element(".label").get_attribute("aria-label").unwrap(),
            text(".welcome")
        );
        assert_eq!(text(".rich"), "Je lis Confidentialité et Conditions.");
        assert!(
            element(".rich")
                .children()
                .item(0)
                .unwrap()
                .is_same_node(Some(&privacy))
        );
        assert!(element(".terms").is_same_node(Some(&terms)));
        assert!(doc.active_element().unwrap().is_same_node(Some(&terms)));
        assert!(
            pocopine::refs::get_on(scope_id, "terms")
                .unwrap()
                .is_same_node(Some(&original_ref))
        );
        terms.dyn_ref::<web_sys::HtmlElement>().unwrap().click();
        flush_sync();
        assert_eq!(text(".trailing"), "1");
        assert_eq!(terms.get_attribute("href").as_deref(), Some("#terms"));
        mounted.handle().update(|state| {
            state.visible = false;
            state.rows.clear();
        });
        flush_sync();
        assert!(host.query_selector(".conditional").unwrap().is_none());
        assert!(host.query_selector(".row").unwrap().is_none());
        let detached = element(".welcome");
        let before = detached.text_content();
        mounted.unmount();
        ui.begin_switch("en".parse().unwrap())
            .unwrap()
            .commit(None)
            .unwrap();
        flush_sync();
        assert_eq!(
            detached.text_content(),
            before,
            "unmounted effects must be released"
        );
        assert!(pocopine::refs::get_on(scope_id, "terms").is_none());
        assert!(host.children().length() == 0);
        host.remove();
        set_auto_flush(true);
    }
}
