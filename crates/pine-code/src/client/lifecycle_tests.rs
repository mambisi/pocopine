use super::{CodeEditorHandle, CodeFinalSnapshot, PineCodeEditor, Subscription};
use crate::CodeError;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Default, Serialize, Deserialize)]
#[component(name = "code-test-wrapper", uses = [PineCodeEditor], template = poco! {<article><pine-code-editor initial-value="dynamic"></pine-code-editor></article>})]
struct Wrapper {}
#[handlers]
impl Wrapper {}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "code-lifecycle-fixture", uses = [PineCodeEditor, Wrapper], template = poco! {
    <div>
      <template pp-if="show"><section class="condition"><pine-code-editor initial-value="condition"></pine-code-editor></section></template>
      <div class="rows"><template pp-for="row in rows" pp-key="row"><article><pine-code-editor :initial-value="row"></pine-code-editor></article></template></div>
      <div class="dynamic"><pp-component :is="selected"></pp-component></div>
      <template pp-if="leaving"><section class="leaving" pp-transition:leave="code-test-leave" pp-transition:leave-start="code-test-leave-start" pp-transition:leave-end="code-test-leave-end"><pine-code-editor initial-value="transition"></pine-code-editor></section></template>
      <button class="remove" @click="remove">remove</button><button class="clear" @click="clear">clear</button>
      <button class="conditional" @click="conditional">condition</button><button class="swap" @click="swap">swap</button>
      <button class="toggle-leave" @click="toggle_leave">transition</button>
    </div>
})]
struct LifecycleFixture {
    show: bool,
    leaving: bool,
    rows: Vec<String>,
    selected: Option<ComponentRef<LifecycleFixture>>,
}
#[handlers]
impl LifecycleFixture {
    fn on_setup(&mut self) {
        self.show = true;
        self.leaving = true;
        self.rows = vec!["one".into(), "two".into(), "three".into()];
        self.selected = Some(ComponentRef::of::<Wrapper>());
    }
    fn remove(&mut self) {
        self.rows.remove(1);
    }
    fn clear(&mut self) {
        self.rows.clear();
    }
    fn conditional(&mut self) {
        self.show = !self.show;
    }
    fn swap(&mut self) {
        self.selected = Some(ComponentRef::of::<PineCodeEditor>());
    }
    fn toggle_leave(&mut self) {
        self.leaving = !self.leaving;
    }
}

fn document() -> web_sys::Document {
    web_sys::window().unwrap().document().unwrap()
}
fn mount<C: Component>() -> (web_sys::Element, pocopine::SubtreeHandle) {
    let host = document().create_element("div").unwrap();
    document().body().unwrap().append_child(&host).unwrap();
    let owned = App::mount_subtree::<C>(&host);
    (host, owned)
}
fn click(root: &web_sys::Element, selector: &str) {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .unchecked_ref::<web_sys::HtmlElement>()
        .click();
    pocopine::flush_sync();
    pocopine::flush_sync();
}
async fn delay(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
}

fn observe(
    host: web_sys::Element,
    output: Rc<RefCell<Vec<CodeFinalSnapshot>>>,
    attached: bool,
) -> (CodeEditorHandle, Subscription) {
    let editor = CodeEditorHandle::from_element(&host).unwrap();
    let scope = pocopine_core::mount::host_child_scope_id_of(&host)
        .expect("component scope bound to its host");
    let reader = editor.clone();
    let subscription = editor
        .on_finalize(move |snapshot| {
            assert_eq!(host.is_connected(), attached);
            assert!(
                pocopine::Scope::find(scope).is_some(),
                "scope must live through finalization"
            );
            assert_eq!(reader.text().unwrap(), snapshot.committed.text);
            assert_eq!(reader.insert_text("reentrant"), Err(CodeError::Finalizing));
            output.borrow_mut().push(snapshot.clone());
        })
        .unwrap();
    (editor, subscription)
}

#[wasm_bindgen_test(async)]
async fn conditional_keyed_row_bulk_clear_dynamic_swap_and_owned_unmount_finalize_attached() {
    pocopine::animate::disable_transitions();
    let (host, owned) = mount::<LifecycleFixture>();
    delay(40).await;
    let output = Rc::new(RefCell::new(Vec::new()));
    let editors = host.query_selector_all("pine-code-editor").unwrap();
    assert_eq!(editors.length(), 6);
    let observed = (0..editors.length())
        .map(|index| {
            observe(
                editors.item(index).unwrap().unchecked_into(),
                output.clone(),
                true,
            )
        })
        .collect::<Vec<_>>();
    click(&host, ".remove");
    assert_eq!(output.borrow().len(), 1);
    assert_eq!(output.borrow()[0].committed.text, "two");
    click(&host, ".clear");
    assert_eq!(output.borrow().len(), 3);
    click(&host, ".conditional");
    assert_eq!(output.borrow().len(), 4);
    click(&host, ".swap");
    assert_eq!(output.borrow().len(), 5);
    delay(20).await;
    owned.unmount();
    assert_eq!(output.borrow().len(), 6);
    assert!(
        observed
            .iter()
            .all(|(editor, _)| matches!(editor.text(), Err(CodeError::Disposed)))
    );
    assert!(
        output
            .borrow()
            .iter()
            .all(|snapshot| snapshot.error.is_none())
    );
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test(async)]
async fn externally_detached_owned_mount_reports_the_weaker_outcome() {
    let (host, owned) = mount::<PineCodeEditor>();
    delay(20).await;
    let output = Rc::new(RefCell::new(Vec::new()));
    let (editor, _subscription) = observe(host.clone(), output.clone(), false);
    host.remove();
    host.query_selector("[data-pine-code-content]")
        .unwrap()
        .unwrap()
        .set_text_content(Some("detached untrusted edit"));
    assert_eq!(editor.text().unwrap(), "");
    owned.unmount();
    assert_eq!(output.borrow()[0].error, Some(CodeError::UnexpectedDetach));
    assert_eq!(output.borrow()[0].committed.text, "");
    assert_eq!(editor.text(), Err(CodeError::Disposed));
}

#[wasm_bindgen_test(async)]
async fn seed_is_read_once_and_oversized_initial_content_remains_visible() {
    let host = document().create_element("div").unwrap();
    document().body().unwrap().append_child(&host).unwrap();
    let owned = App::mount_subtree_with::<PineCodeEditor, _>(&host, |component, _| {
        component.initial_value = "seed".into();
        Ok(())
    })
    .unwrap();
    delay(30).await;
    let editor = CodeEditorHandle::from_element(&host).unwrap();
    assert_eq!(editor.text().unwrap(), "seed");
    editor.insert_text("typed ").unwrap();
    owned
        .handle()
        .update(|component| component.initial_value = "later parent value".into());
    pocopine::flush_sync();
    delay(20).await;
    assert_eq!(editor.text().unwrap(), "typed seed");
    owned.unmount();
    host.remove();
    let host = document().create_element("div").unwrap();
    document().body().unwrap().append_child(&host).unwrap();
    let owned = App::mount_subtree_with::<PineCodeEditor, _>(&host, |component, _| {
        component.initial_value = "oversized".into();
        component.document_limits.max_bytes = 3;
        Ok(())
    })
    .unwrap();
    delay(30).await;
    assert!(CodeEditorHandle::from_element(&host).is_err());
    let surface = host
        .query_selector("[data-pine-code-content]")
        .unwrap()
        .unwrap();
    assert_eq!(surface.text_content().as_deref(), Some("oversized"));
    assert_eq!(
        surface.get_attribute("contenteditable").as_deref(),
        Some("false")
    );
    assert!(
        host.query_selector("[data-pine-code-status]")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap()
            .contains("SizeLimit")
    );
    owned.unmount();
    host.remove();
}

#[wasm_bindgen_test(async)]
async fn leave_cancellation_preserves_editor_until_actual_transition_completion() {
    pocopine::animate::enable_transitions();
    let style = document().create_element("style").unwrap();
    style.set_text_content(Some(".code-test-leave { transition: opacity 80ms linear; } .code-test-leave-start { opacity: 1; } .code-test-leave-end { opacity: 0; }"));
    document().body().unwrap().append_child(&style).unwrap();
    let (host, owned) = mount::<LifecycleFixture>();
    delay(40).await;
    let output = Rc::new(RefCell::new(Vec::new()));
    let editor_host = host
        .query_selector(".leaving pine-code-editor")
        .unwrap()
        .unwrap();
    let (editor, _subscription) = observe(editor_host, output.clone(), true);
    click(&host, ".toggle-leave");
    assert_eq!(editor.text().unwrap(), "transition");
    assert!(output.borrow().is_empty());
    click(&host, ".toggle-leave");
    delay(180).await;
    assert_eq!(editor.text().unwrap(), "transition");
    assert!(output.borrow().is_empty());
    click(&host, ".toggle-leave");
    delay(180).await;
    assert_eq!(output.borrow().len(), 1);
    assert_eq!(editor.text(), Err(CodeError::Disposed));
    owned.unmount();
    host.remove();
    style.remove();
}
