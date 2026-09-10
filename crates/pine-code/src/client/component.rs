use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use pocopine::{component, handlers, this};
use serde::{Deserialize, Serialize};
use web_sys::Element;

use super::{
    handle::CodeEditorHandle,
    runtime::{CodeOptions, Runtime},
};
use crate::{CodeError, CodeResult, DocumentLimits, HistoryLimits};

thread_local! {
    static INSTANCES: RefCell<HashMap<String, Rc<Runtime>>> = RefCell::new(HashMap::new());
    static NEXT: Cell<u64> = const { Cell::new(0) };
}

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineCodeEditor.poco",
    style = "code.css",
    role = "scope",
    display = "block"
)]
pub struct PineCodeEditor {
    #[prop]
    pub initial_value: String,
    #[prop]
    pub language: String,
    #[prop]
    pub line_numbers: bool,
    #[prop]
    pub read_only: bool,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub tab_size: u32,
    #[prop]
    pub indent: String,
    #[prop]
    pub tab_behavior: String,
    #[prop]
    pub theme: String,
    #[prop]
    pub document_limits: DocumentLimits,
    #[prop]
    pub history_limits: HistoryLimits,
}

impl Default for PineCodeEditor {
    fn default() -> Self {
        let options = CodeOptions::default();
        Self {
            initial_value: options.initial_value,
            language: options.language,
            line_numbers: options.line_numbers,
            read_only: options.read_only,
            disabled: options.disabled,
            tab_size: options.tab_size,
            indent: options.indent,
            tab_behavior: options.tab_behavior,
            theme: options.theme,
            document_limits: options.document_limits,
            history_limits: options.history_limits,
        }
    }
}

impl PineCodeEditor {
    fn options(&self) -> CodeOptions {
        CodeOptions {
            initial_value: self.initial_value.clone(),
            language: self.language.clone(),
            line_numbers: self.line_numbers,
            read_only: self.read_only,
            disabled: self.disabled,
            tab_size: self.tab_size,
            indent: self.indent.clone(),
            tab_behavior: self.tab_behavior.clone(),
            theme: self.theme.clone(),
            document_limits: self.document_limits,
            history_limits: self.history_limits,
        }
    }
}

#[handlers]
impl PineCodeEditor {
    fn on_ready(&self) {
        let owner = this::<Self>();
        let Some(scope) = pocopine::current_scope_id() else {
            return;
        };
        let Some(root) = pocopine::refs::get_on(scope, "root") else {
            return;
        };
        let instance: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let cleanup = instance.clone();
        pocopine::on_scope_unmount(move || {
            if let Some(id) = cleanup.borrow_mut().take() {
                let runtime = INSTANCES.with(|instances| instances.borrow_mut().remove(&id));
                if let Some(runtime) = runtime {
                    runtime.finalize(false);
                }
            }
        });
        let mount_root = root.clone();
        // Parent bindings have settled before reading the seed exactly once.
        // Later initial_value updates have no watcher and cannot reset edits.
        pocopine::tick::next(move || {
            if pocopine::Scope::find(scope).is_none() {
                return;
            }
            let options = owner.with(Self::options);
            // Attribute fallthrough copies the host id to its first root.
            // Keep the application id on the host so DOM ids remain unique.
            if let Ok(Some(host)) = mount_root.closest("pine-code-editor")
                && host.get_attribute("id") == mount_root.get_attribute("id")
            {
                let _ = mount_root.remove_attribute("id");
            }
            match Runtime::mount(mount_root.clone(), options.clone()) {
                Ok(runtime) => {
                    let id = NEXT.with(|next| {
                        let id = next.get() + 1;
                        next.set(id);
                        id.to_string()
                    });
                    let _ = mount_root.set_attribute("data-pine-code-instance", &id);
                    let weak = Rc::downgrade(&runtime);
                    let attached_root = mount_root.clone();
                    pocopine::on_before_detach(&mount_root, move || {
                        if let Some(runtime) = weak.upgrade() {
                            runtime.finalize(attached_root.is_connected());
                        }
                    });
                    *instance.borrow_mut() = Some(id.clone());
                    INSTANCES.with(|instances| instances.borrow_mut().insert(id, runtime));
                    let init = web_sys::CustomEventInit::new();
                    init.set_bubbles(true);
                    if let Ok(event) =
                        web_sys::CustomEvent::new_with_event_init_dict("pine:code:ready", &init)
                    {
                        let _ = mount_root.dispatch_event(&event);
                    }
                }
                Err(error) => {
                    // Failed seeds remain available, visible, and immutable.
                    if let Ok(Some(surface)) = mount_root.query_selector("[data-pine-code-content]")
                    {
                        surface.set_text_content(Some(&options.initial_value));
                        let _ = surface.set_attribute("contenteditable", "false");
                        let _ = surface.set_attribute("aria-invalid", "true");
                    }
                    if let Ok(Some(status)) = mount_root.query_selector("[data-pine-code-status]") {
                        status.set_text_content(Some(&format!(
                            "Editor could not open this document: {error}"
                        )));
                    }
                    let _ = mount_root.set_attribute("data-pine-code-error", &error.to_string());
                }
            }
        });
        macro_rules! watch {
            ($field:literal, $ty:ty) => {{
                let root = root.clone();
                let owner = this::<Self>();
                pocopine::watch_scope_field_scoped::<$ty, _>(scope, $field, move |_, previous| {
                    if previous.is_none() {
                        return;
                    }
                    if let Ok(handle) = resolve(&root) {
                        let options = owner.with(Self::options);
                        if let Err(error) = handle.configure(options) {
                            if let Ok(Some(status)) = root.query_selector("[data-pine-code-status]")
                            {
                                status.set_text_content(Some(&format!(
                                    "Configuration rejected: {error}"
                                )));
                            }
                        }
                    }
                });
            }};
        }
        watch!("language", String);
        watch!("line_numbers", bool);
        watch!("read_only", bool);
        watch!("disabled", bool);
        watch!("tab_size", u32);
        watch!("indent", String);
        watch!("tab_behavior", String);
        watch!("theme", String);
    }
}

pub(super) fn resolve(element: &Element) -> CodeResult<CodeEditorHandle> {
    let root = if element.has_attribute("data-pine-code-root") {
        element.clone()
    } else {
        element
            .query_selector("[data-pine-code-root]")
            .ok()
            .flatten()
            .ok_or(CodeError::ViewUnavailable)?
    };
    let id = root
        .get_attribute("data-pine-code-instance")
        .ok_or(CodeError::ViewUnavailable)?;
    INSTANCES.with(|instances| {
        let instances = instances.borrow();
        let runtime = instances.get(&id).ok_or(CodeError::Disposed)?;
        if !runtime.view.borrow().root.is_same_node(Some(&root)) {
            return Err(CodeError::Disposed);
        }
        runtime.check_live()?;
        Ok(runtime.handle())
    })
}
