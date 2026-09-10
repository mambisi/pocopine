use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

use super::{CodeEditorHandle, CodeOptions, dom_reader, runtime::Runtime};
use crate::{CodeError, DocumentRevision, Selection, ViewStatus};

wasm_bindgen_test_configure!(run_in_browser);

struct Fixture {
    runtime: Rc<Runtime>,
    handle: CodeEditorHandle,
}
impl Fixture {
    fn new(text: &str) -> Self {
        Self::options(CodeOptions {
            initial_value: text.into(),
            ..CodeOptions::default()
        })
    }
    fn options(options: CodeOptions) -> Self {
        let document = web_sys::window().unwrap().document().unwrap();
        let root = document.create_element("div").unwrap();
        root.set_inner_html("<div data-pine-code-gutter></div><div data-pine-code-content></div><div data-pine-code-status></div>");
        document.body().unwrap().append_child(&root).unwrap();
        let runtime = Runtime::mount(root, options).unwrap();
        let handle = runtime.handle();
        Self { runtime, handle }
    }
    fn surface(&self) -> web_sys::Element {
        self.runtime.view.borrow().surface.clone()
    }
    fn mutate(&self, text: &str) {
        let line = self.runtime.view.borrow().lines[0].element.clone();
        line.set_text_content(Some(text));
        let node = line.first_child().unwrap();
        web_sys::window()
            .unwrap()
            .get_selection()
            .unwrap()
            .unwrap()
            .set_base_and_extent(
                &node,
                text.encode_utf16().count() as u32,
                &node,
                text.encode_utf16().count() as u32,
            )
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.runtime.finalize(true);
        self.runtime.view.borrow().root.remove();
    }
}

async fn frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        pocopine::tick::next_frame(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
    });
    wasm_bindgen_futures::JsFuture::from(promise).await.unwrap();
}

#[wasm_bindgen_test]
fn native_input_commits_once_and_retains_unrelated_line_nodes() {
    let fixture = Fixture::new("a\nuntouched\n");
    fixture.handle.focus().unwrap();
    let untouched = fixture.runtime.view.borrow().lines[1].element.clone();
    let notifications = Rc::new(RefCell::new(Vec::new()));
    let events = notifications.clone();
    let _subscription = fixture
        .handle
        .on_change(move |change| events.borrow_mut().push(change.clone()))
        .unwrap();
    fixture.mutate("a🦀");
    let snapshot = fixture.handle.snapshot().unwrap();
    assert_eq!(snapshot.text, "a🦀\nuntouched\n");
    assert_eq!(snapshot.revision, DocumentRevision(1));
    assert_eq!(snapshot.selection, Selection::caret(5));
    fixture.runtime.flush().unwrap();
    assert_eq!(
        notifications
            .borrow()
            .iter()
            .filter(|change| change.document_changed)
            .count(),
        1
    );
    assert!(untouched.is_same_node(Some(&fixture.runtime.view.borrow().lines[1].element)));
    fixture.handle.undo().unwrap();
    assert_eq!(fixture.handle.text().unwrap(), "a\nuntouched\n");
}

#[wasm_bindgen_test]
fn stale_selection_is_checked_after_pending_input_flush() {
    let fixture = Fixture::new("abc");
    fixture.handle.focus().unwrap();
    fixture.mutate("abcd");
    assert!(matches!(
        fixture
            .handle
            .set_selection(Selection::caret(1), DocumentRevision(0)),
        Err(CodeError::StaleRevision { .. })
    ));
    assert_eq!(
        fixture.handle.snapshot().unwrap().selection,
        Selection::caret(4)
    );
}

#[wasm_bindgen_test]
fn dom_reader_preserves_blocks_blank_lines_tabs_and_utf16_points() {
    let doc = web_sys::window().unwrap().document().unwrap();
    let root = doc.create_element("div").unwrap();
    for (html, expected) in [
        (
            "<div>a</div><div><br></div><div>b</div><div><br></div>",
            "a\n\nb\n",
        ),
        ("a<div>b</div><div>c</div>", "a\nb\nc"),
        ("<p>a<br>b<br></p><p>c</p>", "a\nb\nc"),
        ("<div><span>a\t🦀</span><span>é</span></div>", "a\t🦀é"),
        ("<div><br data-pine-code-empty></div>", ""),
    ] {
        root.set_inner_html(html);
        assert_eq!(
            dom_reader::read(root.as_ref(), None).unwrap().text,
            expected,
            "{html}"
        );
    }
    root.set_inner_html("<div><span>a🦀b</span></div>");
    let node = root
        .first_child()
        .unwrap()
        .first_child()
        .unwrap()
        .first_child()
        .unwrap();
    let point = dom_reader::DomPoint { node, offset: 3 };
    let read = dom_reader::read(root.as_ref(), Some(&[point.clone(), point])).unwrap();
    assert_eq!(read.points, [Some(5), Some(5)]);
}

#[wasm_bindgen_test(async)]
async fn composition_is_protected_then_commits_one_history_entry() {
    let fixture = Fixture::new("a");
    fixture.handle.focus().unwrap();
    fixture.runtime.start_composition();
    fixture.mutate("aに");
    assert_eq!(fixture.handle.text().unwrap(), "a");
    assert!(fixture.handle.snapshot().unwrap().composing);
    assert_eq!(fixture.handle.focus(), Err(CodeError::CompositionActive));
    assert_eq!(
        fixture
            .handle
            .set_selection(Selection::caret(0), DocumentRevision(0)),
        Err(CodeError::CompositionActive)
    );
    fixture.runtime.end_composition();
    fixture.mutate("a日本"); // terminal input after compositionend
    await_frame_twice().await;
    assert_eq!(fixture.handle.text().unwrap(), "a日本");
    assert_eq!(
        fixture.handle.snapshot().unwrap().revision,
        DocumentRevision(1)
    );
    fixture.handle.undo().unwrap();
    assert_eq!(fixture.handle.text().unwrap(), "a");
}

async fn await_frame_twice() {
    frame().await;
    frame().await;
}

#[wasm_bindgen_test(async)]
async fn readonly_rejects_native_and_composition_candidates_without_revision() {
    let fixture = Fixture::options(CodeOptions {
        initial_value: "safe".into(),
        read_only: true,
        ..CodeOptions::default()
    });
    fixture.handle.focus().unwrap();
    assert_eq!(
        fixture
            .surface()
            .get_attribute("contenteditable")
            .as_deref(),
        Some("true")
    );
    fixture.mutate("unsafe");
    assert_eq!(fixture.handle.text().unwrap(), "safe");
    assert_eq!(fixture.surface().text_content().as_deref(), Some("safe"));
    fixture.runtime.start_composition();
    fixture.mutate("candidate");
    fixture.runtime.end_composition();
    await_frame_twice().await;
    assert_eq!(
        fixture.handle.snapshot().unwrap().revision,
        DocumentRevision(0)
    );
    assert_eq!(fixture.surface().text_content().as_deref(), Some("safe"));
    assert_eq!(fixture.handle.insert_text("x"), Err(CodeError::ReadOnly));
}

#[wasm_bindgen_test]
fn committed_edit_survives_failed_render_and_recovery_does_not_repeat_it() {
    let fixture = Fixture::new("a");
    let changes = Rc::new(RefCell::new(Vec::new()));
    let events = changes.clone();
    let _subscription = fixture
        .handle
        .on_change(move |change| events.borrow_mut().push(change.clone()))
        .unwrap();
    let surface = fixture.surface();
    surface.remove();
    let outcome = fixture.handle.insert_text("X").unwrap();
    assert!(matches!(outcome.view_status, ViewStatus::Failed(_)));
    assert_eq!(fixture.handle.text().unwrap(), "Xa");
    surface.set_text_content(Some("untrusted"));
    assert_eq!(fixture.handle.text().unwrap(), "Xa");
    fixture
        .runtime
        .view
        .borrow()
        .root
        .append_child(&surface)
        .unwrap();
    assert_eq!(fixture.handle.recover_view().unwrap(), ViewStatus::Ready);
    assert_eq!(fixture.handle.text().unwrap(), "Xa");
    assert_eq!(changes.borrow().len(), 1);
    fixture.handle.undo().unwrap();
    assert_eq!(fixture.handle.text().unwrap(), "a");
}

#[wasm_bindgen_test]
fn finalization_delivers_owned_draft_and_freezes_callback_reads() {
    let fixture = Fixture::new("committed");
    fixture.runtime.start_composition();
    fixture.mutate("provisional");
    let delivered = Rc::new(RefCell::new(None));
    let output = delivered.clone();
    let handle = fixture.handle.clone();
    let _subscription = fixture
        .handle
        .on_finalize(move |snapshot| {
            assert_eq!(handle.text().unwrap(), "committed");
            assert_eq!(handle.insert_text("bad"), Err(CodeError::Finalizing));
            *output.borrow_mut() = Some(snapshot.clone());
        })
        .unwrap();
    fixture.runtime.finalize(true);
    assert_eq!(fixture.handle.text(), Err(CodeError::Disposed));
    let snapshot = delivered.borrow().clone().unwrap();
    assert_eq!(snapshot.committed.text, "committed");
    assert!(!snapshot.committed.composing);
    assert_eq!(
        snapshot.interrupted.unwrap().text.as_deref(),
        Some("provisional")
    );
}

#[wasm_bindgen_test]
fn callbacks_can_read_but_cannot_reenter_a_commit() {
    let fixture = Fixture::new("");
    let handle = fixture.handle.clone();
    let _subscription = fixture
        .handle
        .on_change(move |_| {
            assert_eq!(handle.text().unwrap(), "X");
            assert_eq!(handle.insert_text("Y"), Err(CodeError::ReentrantDispatch));
        })
        .unwrap();
    fixture.handle.insert_text("X").unwrap();
}

#[wasm_bindgen_test(async)]
async fn highlights_preserve_native_composition_nodes_and_defer_editability_changes() {
    let fixture = Fixture::options(CodeOptions {
        initial_value: "let value = 1;\nlet untouched = 2;".into(),
        language: "rust".into(),
        ..CodeOptions::default()
    });
    fixture.handle.focus().unwrap();
    await_frame_twice().await;
    assert!(
        fixture
            .surface()
            .query_selector("[data-token=keyword]")
            .unwrap()
            .is_some()
    );
    let untouched = fixture.runtime.view.borrow().lines[1].element.clone();
    fixture.runtime.start_composition();
    fixture.mutate("let value = に;");
    let candidate = fixture.runtime.view.borrow().lines[0]
        .element
        .first_child()
        .unwrap();
    let mut options = fixture.handle.options().unwrap();
    options.language = "json".into();
    options.read_only = true;
    fixture.handle.configure(options).unwrap();
    await_frame_twice().await;
    assert!(
        candidate.is_same_node(
            fixture.runtime.view.borrow().lines[0]
                .element
                .first_child()
                .as_ref()
        )
    );
    assert!(!fixture.handle.options().unwrap().read_only);
    fixture.runtime.end_composition();
    await_frame_twice().await;
    assert_eq!(
        fixture.handle.text().unwrap(),
        "let value = に;\nlet untouched = 2;"
    );
    assert!(fixture.handle.options().unwrap().read_only);
    assert!(untouched.is_same_node(Some(&fixture.runtime.view.borrow().lines[1].element)));
}

#[wasm_bindgen_test]
fn selection_render_failure_still_notifies_the_committed_revision_once() {
    let fixture = Fixture::new("a");
    fixture.handle.focus().unwrap();
    let native = web_sys::window().unwrap().get_selection().unwrap().unwrap();
    let key = JsValue::from_str("setBaseAndExtent");
    let original = js_sys::Reflect::get(native.as_ref(), &key).unwrap();
    let failing = js_sys::Function::new_no_args("throw new Error('injected selection failure')");
    js_sys::Reflect::set(native.as_ref(), &key, failing.as_ref()).unwrap();
    let changes = Rc::new(RefCell::new(Vec::new()));
    let events = changes.clone();
    let reader = fixture.handle.clone();
    let _subscription = fixture
        .handle
        .on_change(move |change| {
            assert_eq!(reader.text().unwrap(), "Xa");
            events.borrow_mut().push(change.clone());
        })
        .unwrap();
    let outcome = fixture.handle.insert_text("X").unwrap();
    js_sys::Reflect::set(native.as_ref(), &key, &original).unwrap();
    assert!(matches!(outcome.view_status, ViewStatus::Failed(_)));
    assert_eq!(changes.borrow().len(), 1);
    assert_eq!(changes.borrow()[0].view_status, outcome.view_status);
    assert_eq!(fixture.handle.recover_view().unwrap(), ViewStatus::Ready);
    assert_eq!(changes.borrow().len(), 1);
}

#[wasm_bindgen_test(async)]
async fn terminal_input_without_compositionend_settles_a_dead_key_session() {
    let fixture = Fixture::new("a");
    fixture.handle.focus().unwrap();
    fixture.runtime.start_composition();
    fixture.mutate("aé");
    let init = web_sys::InputEventInit::new();
    init.set_bubbles(true);
    init.set_input_type("insertText");
    init.set_data(Some("é"));
    init.set_is_composing(false);
    fixture
        .surface()
        .dispatch_event(&web_sys::InputEvent::new_with_event_init_dict("input", &init).unwrap())
        .unwrap();
    await_frame_twice().await;
    assert!(!fixture.handle.snapshot().unwrap().composing);
    assert_eq!(fixture.handle.text().unwrap(), "aé");
    fixture.handle.undo().unwrap();
    assert_eq!(fixture.handle.text().unwrap(), "a");
}

#[wasm_bindgen_test]
fn repeated_text_uses_pre_input_caret_for_history_grouping() {
    let fixture = Fixture::new("aaaa");
    fixture.handle.focus().unwrap();
    for count in 0..3 {
        let editor = fixture.runtime.editor.borrow();
        *fixture.runtime.input_intent.borrow_mut() = Some(super::native_change::InputIntent {
            revision: editor.state().revision(),
            selection: Selection::caret(count),
            input_type: "insertText".into(),
            data: Some("a".into()),
        });
        drop(editor);
        fixture.mutate(&"a".repeat(5 + count));
        let node = fixture.runtime.view.borrow().lines[0]
            .element
            .first_child()
            .unwrap();
        web_sys::window()
            .unwrap()
            .get_selection()
            .unwrap()
            .unwrap()
            .set_base_and_extent(&node, (count + 1) as u32, &node, (count + 1) as u32)
            .unwrap();
        fixture.runtime.flush().unwrap();
    }
    fixture.handle.undo().unwrap();
    assert_eq!(fixture.handle.text().unwrap(), "aaaa");
    assert!(!fixture.handle.snapshot().unwrap().can_undo);
}
