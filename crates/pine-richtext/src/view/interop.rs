//! Typed Rust handle for a mounted `<pine-rich-text-root>`.
//!
//! [`Editor`] is a TipTap-Vue-style imperative handle. Read,
//! write, and subscribe to the surface's doc without
//! hand-rolling DOM events. The handle is parameterized over
//! [`ContentFormat`] so any registered format (markdown, doc
//! JSON, HTML, …) plugs in uniformly.
//!
//! ```ignore
//! use pine_richtext::view::{Editor, Markdown};
//!
//! let editor = Editor::find(&form_root)?;
//! editor.set::<Markdown>("# Hello\n\nWorld")?;
//! let _sub = editor.on_update::<Markdown, _>(|md| log(&md));
//! // Later, at save time:
//! let md = editor.get::<Markdown>()?;
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{CustomEvent, CustomEventInit, Element, Event};

use super::content::{ContentError, ContentFormat, Markdown};
use super::root::{
    COMMAND_EVENT, CommandRequest, DOC_CHANGED_EVENT, EXPORT_STATE_REQUEST_EVENT,
    EXPORT_STATE_RESULT_EVENT, runtime_plugins_view, state_json_from_doc,
};
use crate::runtime::{self, EditorRuntime};
use crate::state::EditorState;

/// CSS selector the [`Editor`] handle uses to locate the
/// surface's custom-element host under any DOM ancestor.
/// The COMMAND_EVENT / EXPORT_STATE listeners are installed
/// on this host element in [`super::root`] so dispatching at
/// either the outer host or the inner `.pine-rich-text`
/// contentEditable both reach the listener (events bubble
/// up). Targeting the host means [`Editor::resolve_runtime`]
/// can read the `runtime` attribute directly without walking
/// the DOM.
const SURFACE_SELECTOR: &str = "pine-rich-text-root";

/// Selector for the inner `pp-ref="surface"` element that
/// `PineRichTextRoot.poco` declares as its contentEditable
/// root. Used by [`Editor::is_ready`] to find the ready
/// attribute when an `Editor` wraps the outer custom-element
/// host; the host itself doesn't carry the flag.
const INNER_SURFACE_SELECTOR: &str = ".pine-rich-text";

/// Attribute the surface sets at the end of its `on_ready` to
/// signal that the command + export-state listeners are
/// installed. Read by [`Editor::is_ready`] and the rAF retry
/// loop in [`Editor::deliver_request`] so callers don't have
/// to think about mount-order timing.
pub(crate) const SURFACE_READY_ATTR: &str = "data-pine-richtext-ready";

/// Maximum number of `requestAnimationFrame` retries before
/// the rAF-driven `deliver_request` loop gives up. Sized to
/// cover any plausible mount-order race (16 frames ≈ 250 ms
/// on a 60 Hz display) without spinning indefinitely if the
/// surface is permanently detached or never finished mounting.
const READY_RETRY_FRAMES: u32 = 16;

/// Helper — `attr == "true"`. Avoids `.as_deref() == Some("true")`
/// at every call site.
fn attr_is_true(el: &Element, name: &str) -> bool {
    el.get_attribute(name).as_deref() == Some("true")
}

/// All the ways the typed [`Editor`] API can fail.
#[derive(Debug, Clone)]
pub enum EditorError {
    /// The surface hasn't initialized yet (no `on_ready`
    /// fired), or the export-state event returned a `null`
    /// payload because the surface couldn't materialize a
    /// state. Treat as transient — retry after `tick::next`,
    /// or simply skip this read.
    SurfaceUnavailable,
    /// `dispatch_event` returned `false` — usually means a
    /// listener canceled the event. Rare in practice.
    DispatchFailed,
    /// Encoding the dispatch payload to a JS value failed.
    /// Bug in the typed wrapper if seen — the wire shape
    /// matches what the surface accepts.
    Encoding(String),
    /// A [`ContentFormat`] parse or serialize step failed.
    Content(ContentError),
    /// Reconstructing the runtime's [`EditorState`] from the
    /// surface's JSON snapshot failed. Indicates schema
    /// mismatch between client and surface — typically when
    /// the surface mounts against a runtime the caller
    /// doesn't have registered locally.
    State(String),
}

impl std::fmt::Display for EditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditorError::SurfaceUnavailable => f.write_str("editor surface unavailable"),
            EditorError::DispatchFailed => f.write_str("editor event dispatch failed"),
            EditorError::Encoding(msg) => write!(f, "editor encoding failed: {msg}"),
            EditorError::Content(err) => write!(f, "{err}"),
            EditorError::State(msg) => write!(f, "editor state reconstruction failed: {msg}"),
        }
    }
}

impl std::error::Error for EditorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EditorError::Content(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ContentError> for EditorError {
    fn from(err: ContentError) -> Self {
        EditorError::Content(err)
    }
}

/// Subscription returned by [`Editor::on_update`]. Drop the
/// guard to detach the listener — the surface is left
/// untouched and the callback won't fire again.
#[must_use = "drop the guard to detach the listener"]
pub struct DocChangeSubscription {
    target: Element,
    closure: Option<Closure<dyn FnMut(Event)>>,
}

impl DocChangeSubscription {
    /// Manually detach. Equivalent to `drop(self)` but reads
    /// more clearly at call sites that don't keep the
    /// subscription around as a struct field.
    pub fn detach(self) {
        drop(self);
    }
}

impl Drop for DocChangeSubscription {
    fn drop(&mut self) {
        if let Some(closure) = self.closure.take() {
            let _ = self.target.remove_event_listener_with_callback(
                DOC_CHANGED_EVENT,
                closure.as_ref().unchecked_ref(),
            );
        }
    }
}

/// A typed, imperative handle for a mounted
/// `<pine-rich-text-root>` surface — modeled after TipTap's
/// Vue 3 `Editor` API.
///
/// The handle is a thin wrapper around the surface
/// [`Element`]: cheap to clone, no internal subscriptions or
/// owned closures. Drop it at any time; the surface keeps
/// running.
///
/// ## Acquiring a handle
///
/// - [`Editor::from_element`] — wrap an already-resolved
///   surface element directly.
/// - [`Editor::find`] — search under any DOM ancestor for
///   the `<pine-rich-text-root>` tag. Returns `None` when
///   no surface exists (e.g. when the parent template
///   conditionally renders one).
///
/// ## Content I/O
///
/// All content operations are generic over [`ContentFormat`].
/// Markdown is the common case; convenience methods
/// [`Editor::set_markdown`] / [`Editor::get_markdown`] /
/// [`Editor::on_update_markdown`] wrap the generic form.
pub struct Editor {
    surface: Element,
}

impl Clone for Editor {
    fn clone(&self) -> Self {
        Self {
            surface: self.surface.clone(),
        }
    }
}

impl Editor {
    /// Wrap an already-resolved surface element. Caller is
    /// responsible for verifying the element actually is a
    /// mounted `<pine-rich-text-root>`; passing any other
    /// element returns a handle whose methods silently fail.
    pub fn from_element(surface: Element) -> Self {
        Self { surface }
    }

    /// Locate the first mounted `<pine-rich-text-root>` under
    /// `root` and wrap it. Returns `None` when no surface is
    /// mounted — convenient for parents whose template gates
    /// the surface behind a `pp-if`. Matches against the
    /// inner `.pine-rich-text` contentEditable (where the
    /// COMMAND_EVENT / EXPORT_STATE listeners live) and
    /// falls back to the outer custom-element host when the
    /// surface is mid-mount.
    pub fn find(root: &Element) -> Option<Self> {
        let surface = root.query_selector(SURFACE_SELECTOR).ok().flatten()?;
        Some(Self::from_element(surface))
    }

    /// The underlying surface element. Useful for forwarding
    /// focus, applying scoped CSS, or attaching ad-hoc
    /// listeners. Most callers shouldn't need it.
    pub fn element(&self) -> &Element {
        &self.surface
    }

    /// Resolve the [`EditorRuntime`] this surface was mounted
    /// against, by reading the `runtime` attribute off the
    /// element. Falls back to the default runtime when the
    /// attribute is absent.
    fn resolve_runtime(&self) -> Arc<EditorRuntime> {
        let name = self.surface.get_attribute("runtime");
        runtime::registry::resolve(name.as_deref().filter(|n| !n.is_empty()))
    }

    /// Replace the surface's doc with `value`, parsed via the
    /// given [`ContentFormat`] `F`.
    ///
    /// Counterpart to TipTap's
    /// `editor.commands.setContent(html, { emitUpdate: false })`
    /// — replacement happens through the surface's
    /// [`CommandRequest::ReplaceState`] pipeline, which writes
    /// `root.doc` directly. The transaction-only
    /// [`DOC_CHANGED_EVENT`] does NOT fire for `ReplaceState`
    /// (it's a state swap, not an edit), so parent loaders
    /// can safely call this from inside an [`Editor::on_update`]
    /// subscription without feedback-looping.
    ///
    /// Textarea-like: callers don't have to wait for the
    /// surface to be "ready". If the surface's command-event
    /// listener hasn't been installed yet (component mid-
    /// mount), the request is queued via
    /// `requestAnimationFrame` and replayed on subsequent
    /// frames until the listener responds — same as how a
    /// `textarea.value = "..."` assignment works the instant
    /// the element is in the DOM.
    pub fn set<F: ContentFormat>(&self, value: &F::Repr) -> Result<(), EditorError> {
        let runtime = self.resolve_runtime();
        let doc = F::parse(value, &runtime)?;
        let state_json = state_json_from_doc(doc, &runtime).ok_or_else(|| {
            EditorError::State(format!(
                "EditorState::create failed for runtime {:?}",
                runtime.name().unwrap_or("default")
            ))
        })?;
        self.deliver_request(CommandRequest::ReplaceState { doc: state_json })
    }

    /// Clear the surface to the runtime's empty default doc.
    /// Equivalent to `textarea.value = ""` — always succeeds,
    /// no markdown round-trip, no schema-validation worry: the
    /// default doc is the schema's own auto-fill for
    /// `doc.content`, which by definition matches.
    ///
    /// Same readiness retry as [`Editor::set`]: callable
    /// before the surface is mid-mount, queued via rAF.
    pub fn clear(&self) -> Result<(), EditorError> {
        let runtime = self.resolve_runtime();
        let doc = runtime
            .schema()
            .default_node("doc")
            .map_err(|err| EditorError::State(err.to_string()))?;
        let state_json = state_json_from_doc(doc, &runtime).ok_or_else(|| {
            EditorError::State(format!(
                "EditorState::create failed for runtime {:?}",
                runtime.name().unwrap_or("default")
            ))
        })?;
        self.deliver_request(CommandRequest::ReplaceState { doc: state_json })
    }

    /// True once the surface's `on_ready` has finished
    /// installing its command / export listeners.
    ///
    /// `PineRichTextRoot` stamps the `data-pine-richtext-ready`
    /// attribute on its own `pp-ref="surface"` element — the
    /// inner `.pine-rich-text` div. `Editor::from_element` is
    /// usually called with the outer custom-element host (the
    /// shape `Editor::find` and parent components reach for),
    /// so this method checks the host's attribute *and* falls
    /// back to its first matching `.pine-rich-text` descendant.
    /// Both sides — pine-richtext stamping it, the host's
    /// Editor reading it — speak through a stable internal
    /// selector instead of having to know each other's element
    /// layout.
    pub fn is_ready(&self) -> bool {
        if attr_is_true(&self.surface, SURFACE_READY_ATTR) {
            return true;
        }
        if let Ok(Some(inner)) = self.surface.query_selector(INNER_SURFACE_SELECTOR) {
            return attr_is_true(&inner, SURFACE_READY_ATTR);
        }
        false
    }

    /// Snapshot the current doc as `F::Owned`. Round-trips
    /// the surface's state JSON synchronously via the
    /// [`EXPORT_STATE_RESULT_EVENT`] CustomEvent — the
    /// listener fires in the same call stack as the request
    /// dispatch, so this returns before the function exits.
    ///
    /// Returns [`EditorError::SurfaceUnavailable`] when the
    /// surface hasn't initialized yet (e.g. before its
    /// `on_ready` ran) or the state JSON couldn't be
    /// reconstructed.
    pub fn get<F: ContentFormat>(&self) -> Result<F::Owned, EditorError> {
        let runtime = self.resolve_runtime();
        let state = self.snapshot_state(&runtime)?;
        let owned = F::serialize(&state, &runtime)?;
        Ok(owned)
    }

    /// Subscribe to per-transaction doc changes. The
    /// callback receives the doc serialized via `F`. Loader
    /// writes via [`Editor::set`] do NOT trigger this
    /// callback. Drop the returned subscription to stop
    /// receiving events.
    pub fn on_update<F, Cb>(&self, mut callback: Cb) -> DocChangeSubscription
    where
        F: ContentFormat,
        Cb: FnMut(F::Owned) + 'static,
    {
        let runtime = self.resolve_runtime();
        let target = self.surface.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            let Ok(custom) = event.dyn_into::<CustomEvent>() else {
                return;
            };
            let Ok(state_json) = serde_wasm_bindgen::from_value::<Value>(custom.detail()) else {
                return;
            };
            let Ok(state) = EditorState::from_json(
                runtime.schema().clone(),
                runtime_plugins_view(&runtime),
                state_json,
            ) else {
                return;
            };
            let Ok(owned) = F::serialize(&state, &runtime) else {
                return;
            };
            callback(owned);
        }) as Box<dyn FnMut(Event)>);
        let _ =
            target.add_event_listener_with_callback(DOC_CHANGED_EVENT, cb.as_ref().unchecked_ref());
        DocChangeSubscription {
            target,
            closure: Some(cb),
        }
    }

    /// Dispatch a typed [`CommandRequest`] at the surface.
    /// The surface runs it through the same state pipeline
    /// keystrokes use, so undo/redo capture commands the
    /// same way they capture typed edits.
    pub fn dispatch(&self, request: CommandRequest) -> Result<(), EditorError> {
        self.dispatch_request(request)
    }

    /// Toggle a named mark (e.g. `"strong"`, `"em"`,
    /// `"code"`) across the current selection.
    pub fn toggle_mark(&self, mark: impl Into<String>) -> Result<(), EditorError> {
        self.dispatch(CommandRequest::ToggleMark { mark: mark.into() })
    }

    /// Undo the most recent committed transaction.
    pub fn undo(&self) -> Result<(), EditorError> {
        self.dispatch(CommandRequest::Undo)
    }

    /// Redo the most recently undone transaction.
    pub fn redo(&self) -> Result<(), EditorError> {
        self.dispatch(CommandRequest::Redo)
    }

    /// Convenience: `self.set::<Markdown>(md)`.
    pub fn set_markdown(&self, markdown: &str) -> Result<(), EditorError> {
        self.set::<Markdown>(markdown)
    }

    /// Convenience: `self.get::<Markdown>()`.
    pub fn get_markdown(&self) -> Result<String, EditorError> {
        self.get::<Markdown>()
    }

    /// Convenience: `self.on_update::<Markdown, _>(cb)`.
    pub fn on_update_markdown<Cb>(&self, callback: Cb) -> DocChangeSubscription
    where
        Cb: FnMut(String) + 'static,
    {
        self.on_update::<Markdown, _>(callback)
    }

    fn dispatch_request(&self, request: CommandRequest) -> Result<(), EditorError> {
        let detail = serde_wasm_bindgen::to_value(&request)
            .map_err(|err| EditorError::Encoding(err.to_string()))?;
        let init = CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        let event = CustomEvent::new_with_event_init_dict(COMMAND_EVENT, &init)
            .map_err(|err| EditorError::Encoding(format!("{err:?}")))?;
        let delivered = self.surface.dispatch_event(&event).unwrap_or(false);
        if delivered {
            Ok(())
        } else {
            Err(EditorError::DispatchFailed)
        }
    }

    /// Dispatch through the readiness gate. If the surface has
    /// signalled ready (via `data-pine-richtext-ready="true"`)
    /// the request fires immediately. Otherwise the request is
    /// queued on `requestAnimationFrame` and replayed until the
    /// surface acks ready or `READY_RETRY_FRAMES` frames have
    /// passed. Returns `Ok` for the in-flight queued case —
    /// the dispatch is fire-and-forget by design (mirroring how
    /// `textarea.value = "..."` doesn't return a status code).
    fn deliver_request(&self, request: CommandRequest) -> Result<(), EditorError> {
        if self.is_ready() {
            return self.dispatch_request(request);
        }
        self.schedule_retry(request, READY_RETRY_FRAMES);
        Ok(())
    }

    /// Schedule a single rAF-driven retry. Recurses (via the
    /// scheduled closure) until either the surface is ready or
    /// `retries_left` hits zero. The closure is leaked via
    /// `Closure::once_into_js` — that's safe because rAF
    /// guarantees a one-shot fire, and the GC reclaims the
    /// JsValue once the surface (and therefore this closure's
    /// Editor reference) drops.
    fn schedule_retry(&self, request: CommandRequest, retries_left: u32) {
        if retries_left == 0 {
            // Surface never came ready — drop the request
            // rather than leak a never-firing rAF chain. A
            // permanently-not-ready surface is a programming
            // error; the silent drop matches what
            // `textarea.value = "..."` does against a detached
            // node.
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let surface = self.surface.clone();
        let cb = Closure::once_into_js(move || {
            let editor = Editor::from_element(surface);
            if editor.is_ready() {
                let _ = editor.dispatch_request(request);
            } else {
                editor.schedule_retry(request, retries_left - 1);
            }
        });
        let _ = window.request_animation_frame(cb.unchecked_ref());
    }

    /// Synchronously round-trip the surface's
    /// [`EXPORT_STATE_REQUEST_EVENT`] to capture a fresh
    /// state JSON, then reconstruct an [`EditorState`] from
    /// it against `runtime`'s schema + plugins.
    fn snapshot_state(&self, runtime: &EditorRuntime) -> Result<EditorState, EditorError> {
        let captured: Rc<RefCell<Option<Value>>> = Rc::new(RefCell::new(None));
        let captured_for_cb = captured.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            let Ok(custom) = event.dyn_into::<CustomEvent>() else {
                return;
            };
            if let Ok(state_json) = serde_wasm_bindgen::from_value::<Value>(custom.detail()) {
                *captured_for_cb.borrow_mut() = Some(state_json);
            }
        }) as Box<dyn FnMut(Event)>);

        self.surface
            .add_event_listener_with_callback(
                EXPORT_STATE_RESULT_EVENT,
                cb.as_ref().unchecked_ref(),
            )
            .map_err(|err| EditorError::Encoding(format!("{err:?}")))?;

        let init = CustomEventInit::new();
        init.set_bubbles(true);
        if let Ok(request) =
            CustomEvent::new_with_event_init_dict(EXPORT_STATE_REQUEST_EVENT, &init)
        {
            let _ = self.surface.dispatch_event(&request);
        }

        let _ = self.surface.remove_event_listener_with_callback(
            EXPORT_STATE_RESULT_EVENT,
            cb.as_ref().unchecked_ref(),
        );
        drop(cb);

        let state_json = captured
            .borrow_mut()
            .take()
            .ok_or(EditorError::SurfaceUnavailable)?;
        EditorState::from_json(
            runtime.schema().clone(),
            runtime_plugins_view(runtime),
            state_json,
        )
        .map_err(|err| EditorError::State(err.to_string()))
    }
}
