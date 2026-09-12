use super::{dom_reader::DomPoint, runtime::Runtime, view::dom_error};
use crate::{
    Change, ChangeSet, CodeError, CodeResult, DocumentRevision, EditOrigin, Selection, TextRange,
};
use std::rc::Rc;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{DragEvent, Event};

#[derive(Clone)]
pub(super) struct DragSession {
    revision: DocumentRevision,
    range: TextRange,
    text: String,
    can_move: bool,
}

pub(super) fn install(runtime: &Rc<Runtime>) -> CodeResult<()> {
    let surface = runtime.view.borrow().surface.clone();
    runtime.listen(surface.as_ref(), "dragstart", start)?;
    runtime.listen(surface.as_ref(), "dragend", |runtime, _| {
        runtime.drag.borrow_mut().take();
    })?;
    runtime.listen(surface.as_ref(), "dragover", |runtime, event| {
        if runtime.composing() || runtime.editable().is_err() {
            return;
        }
        event.prevent_default();
        if let Some(data) = event
            .dyn_ref::<DragEvent>()
            .and_then(|drag| drag.data_transfer())
        {
            data.set_drop_effect(
                if runtime
                    .drag
                    .borrow()
                    .as_ref()
                    .is_some_and(|session| session.can_move)
                {
                    "move"
                } else {
                    "copy"
                },
            );
        }
    })?;
    runtime.listen(surface.as_ref(), "drop", |runtime, event| {
        event.prevent_default();
        if let Err(error) = drop_text(runtime, event) {
            runtime
                .view
                .borrow()
                .status
                .set_text_content(Some(&format!("Drop rejected: {error}")));
        }
    })
}

fn start(runtime: &Runtime, event: Event) {
    if runtime.composing() || runtime.options.borrow().disabled {
        event.prevent_default();
        return;
    }
    let Some(data) = event
        .dyn_ref::<DragEvent>()
        .and_then(|drag| drag.data_transfer())
    else {
        return;
    };
    let Ok(snapshot) = runtime.snapshot() else {
        event.prevent_default();
        return;
    };
    let range = snapshot.selection.range();
    if range.is_empty() {
        return;
    }
    let text = snapshot.text[range.from.0..range.to.0].to_owned();
    if data.clear_data().is_err() || data.set_data("text/plain", &text).is_err() {
        event.prevent_default();
        return;
    }
    let can_move = runtime.editable().is_ok();
    data.set_effect_allowed(if can_move { "copyMove" } else { "copy" });
    *runtime.drag.borrow_mut() = Some(DragSession {
        revision: snapshot.revision,
        range,
        text,
        can_move,
    });
}

fn point(runtime: &Runtime, event: &DragEvent) -> CodeResult<usize> {
    let view = runtime.view.borrow();
    let doc = view
        .surface
        .owner_document()
        .ok_or(CodeError::ViewUnavailable)?;
    let x = JsValue::from_f64(f64::from(event.client_x()));
    let y = JsValue::from_f64(f64::from(event.client_y()));
    // Feature detection is necessary: WebKit exposes caretRangeFromPoint.
    for method in ["caretPositionFromPoint", "caretRangeFromPoint"] {
        let function = js_sys::Reflect::get(doc.as_ref(), &JsValue::from_str(method))
            .ok()
            .and_then(|value| value.dyn_into::<js_sys::Function>().ok());
        let Some(function) = function else {
            continue;
        };
        let result = function.call2(doc.as_ref(), &x, &y).map_err(dom_error)?;
        if result.is_null() || result.is_undefined() {
            continue;
        }
        let point = if method == "caretPositionFromPoint" {
            let position: web_sys::CaretPosition = result.unchecked_into();
            DomPoint {
                node: position.offset_node().ok_or(CodeError::InvalidPosition)?,
                offset: position.offset(),
            }
        } else {
            let range: web_sys::Range = result.unchecked_into();
            DomPoint {
                node: range.start_container().map_err(dom_error)?,
                offset: range.start_offset().map_err(dom_error)?,
            }
        };
        if !view.surface.contains(Some(&point.node)) {
            return Err(CodeError::InvalidPosition);
        }
        let position = view.offset(&point)?;
        runtime
            .editor
            .borrow()
            .state()
            .document()
            .check_offset(crate::TextOffset(position))?;
        return Ok(position);
    }
    Err(CodeError::ViewUnavailable)
}

fn drop_text(runtime: &Runtime, event: Event) -> CodeResult<()> {
    let _guard = runtime.enter(true)?;
    runtime.flush_inside()?;
    runtime.editable()?;
    runtime.usable_view()?;
    let event = event
        .dyn_ref::<DragEvent>()
        .ok_or(CodeError::InvalidConfiguration)?;
    let data = event
        .data_transfer()
        .ok_or(CodeError::InvalidConfiguration)?;
    let text = crate::normalize_lf(&data.get_data("text/plain").map_err(dom_error)?);
    if text.is_empty() {
        return Ok(());
    }
    let at = point(runtime, event)?;
    let session = runtime.drag.borrow_mut().take();
    let editor = runtime.editor.borrow();
    let mut changes = Vec::new();
    let mut inserted_at = at;
    if let Some(session) = session.filter(|session| {
        session.text == text
            && session.can_move
            && !event.ctrl_key()
            && !event.alt_key()
            && data.drop_effect() != "copy"
    }) {
        editor.check_revision(session.revision)?;
        if (session.range.from.0..=session.range.to.0).contains(&at) {
            return Ok(());
        }
        changes.push(Change::replace(
            session.range.from.0,
            session.range.to.0,
            "",
        ));
        if at > session.range.to.0 {
            inserted_at -= session.range.to.0 - session.range.from.0;
        }
    }
    changes.push(Change::replace(at, at, &text));
    let mut transaction = editor.state().transaction(
        ChangeSet::new(editor.state().document(), changes)?,
        EditOrigin::Drop,
    );
    transaction.selection = Some(Selection::between(inserted_at, inserted_at + text.len()));
    drop(editor);
    let change = runtime
        .editor
        .borrow_mut()
        .dispatch(transaction, runtime.now())?;
    runtime.publish(change, false, true);
    Ok(())
}
