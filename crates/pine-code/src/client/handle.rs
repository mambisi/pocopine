use std::rc::{Rc, Weak};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

use super::{
    component,
    events::Subscription,
    runtime::{CodeFinalSnapshot, CodeOptions, CodeSnapshot, CompositionStatus, Runtime},
    view::dom_error,
};
use crate::{
    CodeChange, CodeError, CodeResult, CommitOutcome, DocumentRevision, EditOrigin, Selection,
    Transaction, ViewStatus,
};

/// A weak reference to one mounted editor. Reusing an element or ref cannot
/// revive this handle after that instance has finalized.
#[derive(Clone)]
pub struct CodeEditorHandle {
    pub(super) runtime: Weak<Runtime>,
}

impl CodeEditorHandle {
    pub fn from_element(element: &Element) -> CodeResult<Self> {
        component::resolve(element)
    }
    pub(super) fn get(&self) -> CodeResult<Rc<Runtime>> {
        let runtime = self.runtime.upgrade().ok_or(CodeError::Disposed)?;
        runtime.check_live()?;
        Ok(runtime)
    }
    pub fn snapshot(&self) -> CodeResult<CodeSnapshot> {
        self.get()?.snapshot()
    }
    pub fn text(&self) -> CodeResult<String> {
        Ok(self.snapshot()?.text)
    }
    pub fn selection(&self) -> CodeResult<Selection> {
        Ok(self.snapshot()?.selection)
    }
    /// Observational counters; this method does not flush or serialize the DOM.
    pub fn metrics(&self) -> CodeResult<super::CodeMetrics> {
        let runtime = self.get()?;
        let mut metrics = runtime.metrics.borrow().clone();
        let view = runtime.view.borrow();
        metrics.mounted_lines = view.lines.len();
        metrics.token_spans = view.lines.iter().map(|line| line.tokens.len()).sum();
        metrics.history_bytes = runtime.editor.borrow().history_retained_bytes();
        Ok(metrics)
    }

    pub fn dispatch(&self, transaction: Transaction) -> CodeResult<CommitOutcome> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        if transaction.changes.is_empty() {
            runtime.selectable()?;
        } else {
            runtime.editable()?;
        }
        let change = runtime
            .editor
            .borrow_mut()
            .dispatch(transaction, runtime.now())?;
        Ok(runtime.publish(change, false, true))
    }

    pub fn load_document(
        &self,
        text: &str,
        expected: DocumentRevision,
    ) -> CodeResult<CommitOutcome> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        let change = runtime.editor.borrow_mut().load_document(text, expected)?;
        let outcome = runtime.publish(change, false, true);
        if matches!(outcome.view_status, ViewStatus::Ready) {
            let view = runtime.view.borrow();
            view.root.set_scroll_top(0);
            view.root.set_scroll_left(0);
        }
        Ok(outcome)
    }

    pub fn set_selection(
        &self,
        selection: Selection,
        expected: DocumentRevision,
    ) -> CodeResult<CommitOutcome> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        runtime.selectable()?;
        let change = runtime
            .editor
            .borrow_mut()
            .set_selection(selection, expected)?;
        Ok(runtime.publish(change, false, true))
    }

    pub fn undo(&self) -> CodeResult<CommitOutcome> {
        self.travel(false)
    }
    pub fn redo(&self) -> CodeResult<CommitOutcome> {
        self.travel(true)
    }
    fn travel(&self, redo: bool) -> CodeResult<CommitOutcome> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        runtime.editable()?;
        let change = if redo {
            runtime.editor.borrow_mut().redo()?
        } else {
            runtime.editor.borrow_mut().undo()?
        };
        Ok(change.map_or_else(
            || runtime.outcome(),
            |change| runtime.publish(change, false, true),
        ))
    }

    pub fn insert_text(&self, text: &str) -> CodeResult<CommitOutcome> {
        self.command(
            |state, _| crate::commands::replace_selection(state, text, EditOrigin::Api),
            true,
        )
    }
    pub fn insert_newline(&self) -> CodeResult<CommitOutcome> {
        self.command(|state, _| crate::commands::insert_newline(state), true)
    }
    pub fn indent(&self) -> CodeResult<CommitOutcome> {
        self.indentation(false)
    }
    pub fn outdent(&self) -> CodeResult<CommitOutcome> {
        self.indentation(true)
    }
    fn indentation(&self, outdent: bool) -> CodeResult<CommitOutcome> {
        self.command(
            |state, options| {
                crate::commands::indent_lines(
                    state,
                    &options.indentation(),
                    outdent,
                    options.tab_size as usize,
                )
            },
            true,
        )
    }
    pub fn select_all(&self) -> CodeResult<CommitOutcome> {
        self.command(|state, _| Ok(crate::commands::select_all(state)), false)
    }

    pub fn search(&self, query: &str) -> CodeResult<crate::search::SearchResults> {
        let runtime = self.get()?;
        runtime.snapshot()?;
        Ok(crate::search::search(
            runtime.editor.borrow().state(),
            query,
        ))
    }

    pub fn find_next(&self, query: &str, backwards: bool) -> CodeResult<Option<CommitOutcome>> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        runtime.usable_view()?;
        let found = crate::search::next_match(runtime.editor.borrow().state(), query, backwards);
        let Some(found) = found else {
            return Ok(None);
        };
        runtime
            .view
            .borrow()
            .surface
            .unchecked_ref::<HtmlElement>()
            .focus()
            .map_err(dom_error)?;
        let revision = runtime.editor.borrow().state().revision();
        let change = runtime
            .editor
            .borrow_mut()
            .set_selection(Selection::between(found.from.0, found.to.0), revision)?;
        let outcome = runtime.publish(change, false, true);
        if matches!(outcome.view_status, ViewStatus::Ready) {
            let line = runtime
                .editor
                .borrow()
                .state()
                .document()
                .line_at(found.from)?;
            if let Some(line) = runtime.view.borrow().lines.get(line) {
                let options = web_sys::ScrollIntoViewOptions::new();
                options.set_block(web_sys::ScrollLogicalPosition::Nearest);
                options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
                line.element
                    .scroll_into_view_with_scroll_into_view_options(&options);
            }
        }
        Ok(Some(outcome))
    }

    pub fn replace_current(&self, query: &str, replacement: &str) -> CodeResult<CommitOutcome> {
        self.command(
            |state, _| {
                Ok(
                    crate::search::replace_current(state, query, replacement)?.unwrap_or_else(
                        || state.transaction(crate::ChangeSet::default(), EditOrigin::Command),
                    ),
                )
            },
            true,
        )
    }

    pub fn replace_all(&self, query: &str, replacement: &str) -> CodeResult<CommitOutcome> {
        self.command(
            |state, options| {
                crate::search::replace_all(state, query, replacement, options.document_limits)
            },
            true,
        )
    }

    pub(super) fn command(
        &self,
        build: impl FnOnce(&crate::EditorState, &CodeOptions) -> CodeResult<Transaction>,
        edit: bool,
    ) -> CodeResult<CommitOutcome> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        if edit {
            runtime.editable()?;
        } else {
            runtime.selectable()?;
        }
        let transaction = build(runtime.editor.borrow().state(), &runtime.options.borrow())?;
        let change = runtime
            .editor
            .borrow_mut()
            .dispatch(transaction, runtime.now())?;
        Ok(runtime.publish(change, false, true))
    }

    pub fn focus(&self) -> CodeResult<()> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        runtime.usable_view()?;
        let view = runtime.view.borrow();
        view.surface
            .unchecked_ref::<HtmlElement>()
            .focus()
            .map_err(dom_error)?;
        let editor = runtime.editor.borrow();
        view.restore_selection(editor.state().document(), editor.state().selection())
    }

    pub fn reveal_selection(&self) -> CodeResult<()> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        runtime.usable_view()?;
        let editor = runtime.editor.borrow();
        let index = editor
            .state()
            .document()
            .line_at(editor.state().selection().head)?;
        let view = runtime.view.borrow();
        let line = view.lines.get(index).ok_or(CodeError::ViewUnavailable)?;
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
        line.element
            .scroll_into_view_with_scroll_into_view_options(&options);
        Ok(())
    }

    pub fn recover_view(&self) -> CodeResult<ViewStatus> {
        self.get()?.recover()
    }
    pub fn configure(&self, options: CodeOptions) -> CodeResult<()> {
        self.get()?.configure(options)
    }
    pub fn options(&self) -> CodeResult<CodeOptions> {
        Ok(self.get()?.options.borrow().clone())
    }
    pub fn set_key_bindings(&self, bindings: Vec<super::KeyBinding>) -> CodeResult<()> {
        let runtime = self.get()?;
        let _guard = runtime.enter(true)?;
        runtime.flush_inside()?;
        *runtime.key_bindings.borrow_mut() = bindings;
        Ok(())
    }
    pub fn on_change(&self, callback: impl Fn(&CodeChange) + 'static) -> CodeResult<Subscription> {
        Ok(self.get()?.changes.subscribe(callback))
    }
    pub fn on_view_status(
        &self,
        callback: impl Fn(&ViewStatus) + 'static,
    ) -> CodeResult<Subscription> {
        Ok(self.get()?.view_events.subscribe(callback))
    }
    pub fn on_composition_change(
        &self,
        callback: impl Fn(&CompositionStatus) + 'static,
    ) -> CodeResult<Subscription> {
        Ok(self.get()?.composition_events.subscribe(callback))
    }
    pub fn on_finalize(
        &self,
        callback: impl Fn(&CodeFinalSnapshot) + 'static,
    ) -> CodeResult<Subscription> {
        Ok(self.get()?.final_events.subscribe(callback))
    }

    pub fn on_presentation_status(
        &self,
        callback: impl Fn(&crate::language::PresentationStatus) + 'static,
    ) -> CodeResult<Subscription> {
        Ok(self.get()?.presentation_events.subscribe(callback))
    }
}

impl Runtime {
    pub(super) fn handle(&self) -> CodeEditorHandle {
        CodeEditorHandle {
            runtime: self.weak.clone(),
        }
    }
}
