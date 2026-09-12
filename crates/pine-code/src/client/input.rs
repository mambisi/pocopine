use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{ClipboardEvent, Event, InputEvent, KeyboardEvent};

use super::runtime::Runtime;
use crate::{CodeError, CodeResult, EditOrigin};

#[derive(Clone, Debug)]
pub enum CodeCommand {
    Undo,
    Redo,
    Newline,
    Indent,
    Outdent,
    SelectAll,
    Insert(String),
    PassThrough,
}

#[derive(Clone, Debug)]
pub struct KeyBinding {
    pub key: String,
    /// Control or Command. AltGraph is always left to native text input.
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub command: CodeCommand,
}

pub(super) fn install(runtime: &Rc<Runtime>) -> CodeResult<()> {
    let surface = runtime.view.borrow().surface.clone();
    runtime.listen(surface.as_ref(), "beforeinput", before_input)?;
    runtime.listen(surface.as_ref(), "input", |runtime, event| {
        if event
            .dyn_ref::<InputEvent>()
            .is_some_and(|input| input.is_composing())
            && !runtime.composing()
        {
            runtime.begin_composition(false);
        }
        // A non-composing terminal text input also settles dead-key sessions
        // in engines that omit compositionend. This uses an observed input,
        // never a timer that invents a commit while the IME is still active.
        if runtime.composing()
            && event.dyn_ref::<InputEvent>().is_some_and(|input| {
                !input.is_composing()
                    && matches!(
                        input.input_type().as_str(),
                        "insertText" | "insertReplacementText" | "insertFromComposition"
                    )
            })
        {
            runtime.end_composition();
        }
        if !runtime.composing() {
            let _ = runtime.flush();
        }
    })?;
    runtime.listen(surface.as_ref(), "compositionstart", |runtime, _| {
        runtime.start_composition()
    })?;
    runtime.listen(surface.as_ref(), "compositionend", |runtime, _| {
        runtime.end_composition()
    })?;
    runtime.listen(surface.as_ref(), "keydown", keydown)?;
    runtime.listen(surface.as_ref(), "paste", |runtime, event| {
        clipboard(runtime, event, "paste")
    })?;
    runtime.listen(surface.as_ref(), "copy", |runtime, event| {
        clipboard(runtime, event, "copy")
    })?;
    runtime.listen(surface.as_ref(), "cut", |runtime, event| {
        clipboard(runtime, event, "cut")
    })?;
    runtime.listen(surface.as_ref(), "pointerdown", |runtime, event| {
        if runtime.options.borrow().disabled {
            event.prevent_default();
        }
    })?;
    runtime.listen(surface.as_ref(), "blur", |runtime, _| {
        let _ = runtime.flush();
        runtime.editor.borrow_mut().close_history_group();
    })?;
    if let Some(document) = surface.owner_document() {
        runtime.listen(document.as_ref(), "selectionchange", |runtime, _| {
            if !runtime.busy.get() {
                let _ = runtime.flush();
            }
        })?;
    }
    Ok(())
}

fn before_input(runtime: &Runtime, event: Event) {
    let Some(input) = event.dyn_ref::<InputEvent>() else {
        return;
    };
    if input.is_composing() && !runtime.composing() {
        runtime.start_composition();
    }
    if runtime.composing() {
        return;
    }
    let _ = runtime.flush();
    if runtime.editable().is_err() || !runtime.can_read_dom() {
        if event.cancelable() {
            event.prevent_default();
        }
        return;
    }
    let handle = runtime.handle();
    {
        let editor = runtime.editor.borrow();
        *runtime.input_intent.borrow_mut() = Some(super::native_change::InputIntent {
            revision: editor.state().revision(),
            selection: editor.state().selection(),
            input_type: input.input_type(),
            data: input.data(),
        });
    }
    match input.input_type().as_str() {
        "historyUndo" | "historyRedo" => {
            if !event.cancelable() {
                runtime
                    .pending_native_history
                    .set(Some(input.input_type() == "historyRedo"));
                return;
            }
            event.prevent_default();
            if input.input_type() == "historyUndo" {
                let _ = handle.undo();
            } else {
                let _ = handle.redo();
            }
        }
        "insertParagraph" | "insertLineBreak" if event.cancelable() => {
            event.prevent_default();
            let _ = handle.insert_newline();
        }
        kind if kind.starts_with("format") => event.prevent_default(),
        _ => (),
    }
}

fn keydown(runtime: &Runtime, event: Event) {
    let Some(key) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    if event.default_prevented() {
        return;
    }
    if !key.is_composing() && key.key_code() != 229 {
        runtime.settle_ended_composition();
    }
    if runtime.composing() || key.is_composing() || key.key_code() == 229 {
        return;
    }
    if runtime.options.borrow().disabled {
        event.prevent_default();
        return;
    }
    let escaped = runtime.escape_tab.replace(false);
    if key.key() == "Escape" {
        runtime.escape_tab.set(true);
        return;
    }
    if escaped && key.key() == "Tab" {
        return;
    }
    if key.key() == "Tab" && runtime.options.borrow().read_only {
        return;
    }
    if key.get_modifier_state("AltGraph") {
        return;
    }
    let handle = runtime.handle();
    let command = key.ctrl_key() || key.meta_key();
    let binding = runtime
        .key_bindings
        .borrow()
        .iter()
        .find(|binding| {
            binding.key.eq_ignore_ascii_case(&key.key())
                && binding.primary == command
                && binding.shift == key.shift_key()
                && binding.alt == key.alt_key()
        })
        .cloned();
    if let Some(binding) = binding {
        if matches!(binding.command, CodeCommand::PassThrough) {
            return;
        }
        event.prevent_default();
        let result = match binding.command {
            CodeCommand::Undo => handle.undo(),
            CodeCommand::Redo => handle.redo(),
            CodeCommand::Newline => handle.insert_newline(),
            CodeCommand::Indent => handle.indent(),
            CodeCommand::Outdent => handle.outdent(),
            CodeCommand::SelectAll => handle.select_all(),
            CodeCommand::Insert(text) => handle.command(
                |state, _| crate::commands::replace_selection(state, &text, EditOrigin::Command),
                true,
            ),
            CodeCommand::PassThrough => unreachable!(),
        };
        if let Err(error) = result {
            runtime
                .view
                .borrow()
                .status
                .set_text_content(Some(&error.to_string()));
        }
        return;
    }
    if key.alt_key() {
        return;
    }
    let result = if command && key.key().eq_ignore_ascii_case("z") {
        event.prevent_default();
        if key.shift_key() {
            handle.redo()
        } else {
            handle.undo()
        }
    } else if command && key.key().eq_ignore_ascii_case("y") {
        event.prevent_default();
        handle.redo()
    } else if command && key.key().eq_ignore_ascii_case("a") {
        event.prevent_default();
        handle.select_all()
    } else if !command && key.key() == "Enter" {
        event.prevent_default();
        handle.insert_newline()
    } else if !command
        && key.key() == "Tab"
        && !runtime.options.borrow().read_only
        && runtime.options.borrow().tab_behavior == "indent"
    {
        event.prevent_default();
        if key.shift_key() {
            handle.outdent()
        } else {
            handle.indent()
        }
    } else {
        return;
    };
    if let Err(error) = result {
        runtime
            .view
            .borrow()
            .status
            .set_text_content(Some(&error.to_string()));
    }
}

fn clipboard(runtime: &Runtime, event: Event, action: &str) {
    let Some(clipboard) = event
        .dyn_ref::<ClipboardEvent>()
        .and_then(|event| event.clipboard_data())
    else {
        return;
    };
    if runtime.composing() {
        if action != "copy" {
            event.prevent_default();
        }
        return;
    }
    if runtime.options.borrow().disabled {
        event.prevent_default();
        return;
    }
    let handle = runtime.handle();
    if action == "paste" {
        event.prevent_default();
        if let Ok(text) = clipboard.get_data("text/plain") {
            let result = handle.command(
                |state, _| crate::commands::replace_selection(state, &text, EditOrigin::Paste),
                true,
            );
            if let Err(error) = result {
                runtime
                    .view
                    .borrow()
                    .status
                    .set_text_content(Some(&error.to_string()));
            }
        }
    } else {
        let Ok(snapshot) = handle.snapshot() else {
            return;
        };
        let range = snapshot.selection.range();
        if range.is_empty() {
            return;
        }
        if clipboard
            .set_data("text/plain", &snapshot.text[range.from.0..range.to.0])
            .is_err()
        {
            return;
        }
        event.prevent_default();
        if action == "cut" {
            let result = handle.command(
                |state, _| crate::commands::replace_selection(state, "", EditOrigin::Cut),
                true,
            );
            if let Err(error) = result
                && error != CodeError::ReadOnly
            {
                runtime
                    .view
                    .borrow()
                    .status
                    .set_text_content(Some(&error.to_string()));
            }
        }
    }
}
