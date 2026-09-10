use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{
    Element, Event, EventTarget, HtmlElement, MutationObserver, MutationObserverInit,
    MutationRecord,
};

use super::{
    dom_reader,
    events::Events,
    view::{View, dom_error},
};
use crate::language::{HighlightCache, HighlightLimits, PresentationStatus};
use crate::{
    CodeChange, CodeError, CodeResult, CommitOutcome, DocumentLimits, DocumentRevision, EditOrigin,
    Editor, HistoryLimits, Selection, ViewStatus,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CodeOptions {
    pub initial_value: String,
    pub language: String,
    pub line_numbers: bool,
    pub read_only: bool,
    pub disabled: bool,
    pub tab_size: u32,
    /// Empty inherits the language default. Otherwise spaces or a single tab.
    pub indent: String,
    /// `focus` (default) or `indent`.
    pub tab_behavior: String,
    pub theme: String,
    pub document_limits: DocumentLimits,
    pub history_limits: HistoryLimits,
}

impl Default for CodeOptions {
    fn default() -> Self {
        Self {
            initial_value: String::new(),
            language: "plain".into(),
            line_numbers: false,
            read_only: false,
            disabled: false,
            tab_size: 4,
            indent: String::new(),
            tab_behavior: "focus".into(),
            theme: "auto".into(),
            document_limits: DocumentLimits::default(),
            history_limits: HistoryLimits::default(),
        }
    }
}

impl CodeOptions {
    pub fn validate(&self) -> CodeResult<()> {
        self.document_limits.validate()?;
        self.history_limits.validate()?;
        if self.tab_size == 0
            || self.tab_size > 32
            || !matches!(self.tab_behavior.as_str(), "focus" | "indent")
            || !matches!(self.theme.as_str(), "auto" | "light" | "dark")
            || (!self.indent.is_empty()
                && self.indent != "\t"
                && (self.indent.len() > 32 || !self.indent.bytes().all(|ch| ch == b' ')))
        {
            return Err(CodeError::InvalidConfiguration);
        }
        Ok(())
    }

    pub fn indentation(&self) -> &str {
        if !self.indent.is_empty() {
            &self.indent
        } else if self.language == "rust" {
            "    "
        } else {
            "  "
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CodeSnapshot {
    pub text: String,
    pub revision: DocumentRevision,
    pub selection: Selection,
    pub composing: bool,
    pub view_status: ViewStatus,
    pub can_undo: bool,
    pub can_redo: bool,
    pub presentation_status: PresentationStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum CompositionStatus {
    Started,
    Committed,
    Canceled,
    Rejected(CodeError),
    Interrupted,
}

#[derive(Clone, Debug, Serialize)]
pub struct InterruptedComposition {
    pub text: Option<String>,
    pub selection: Option<Selection>,
    pub base_revision: DocumentRevision,
    pub error: Option<CodeError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CodeFinalSnapshot {
    pub committed: CodeSnapshot,
    pub interrupted: Option<InterruptedComposition>,
    pub error: Option<CodeError>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CodeMetrics {
    pub native_reads: u64,
    pub full_dom_reads: u64,
    pub last_read_lines: usize,
    pub last_read_bytes: usize,
    pub mounted_lines: usize,
    pub token_spans: usize,
    pub history_bytes: usize,
}

struct Composition {
    generation: u64,
    ended: bool,
    rejected: bool,
}

struct Listener {
    target: EventTarget,
    name: &'static str,
    callback: Closure<dyn FnMut(Event)>,
}
type ObserverCallback = Closure<dyn FnMut(js_sys::Array, MutationObserver)>;
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.name, self.callback.as_ref().unchecked_ref());
    }
}

pub(super) struct Runtime {
    pub editor: RefCell<Editor>,
    pub view: RefCell<View>,
    pub options: RefCell<CodeOptions>,
    pending_options: RefCell<Option<CodeOptions>>,
    pub status: RefCell<ViewStatus>,
    composition: RefCell<Option<Composition>>,
    generation: Cell<u64>,
    pub busy: Cell<bool>,
    pub finalizing: Cell<bool>,
    pub disposed: Cell<bool>,
    pub escape_tab: Cell<bool>,
    pub drag: RefCell<Option<super::drag::DragSession>>,
    pub key_bindings: RefCell<Vec<super::input::KeyBinding>>,
    pub input_intent: RefCell<Option<super::native_change::InputIntent>>,
    pub pending_native_history: Cell<Option<bool>>,
    pub metrics: RefCell<CodeMetrics>,
    observer: RefCell<Option<MutationObserver>>,
    observer_callback: RefCell<Option<ObserverCallback>>,
    listeners: RefCell<Vec<Listener>>,
    pub changes: Events<CodeChange>,
    pub view_events: Events<ViewStatus>,
    pub composition_events: Events<CompositionStatus>,
    pub final_events: Events<CodeFinalSnapshot>,
    pub presentation_events: Events<PresentationStatus>,
    pub presentation: RefCell<PresentationStatus>,
    highlight: RefCell<HighlightCache>,
    highlight_scheduled: Cell<bool>,
    clear_line: Cell<usize>,
    pub weak: Weak<Runtime>,
}

pub(super) struct Busy<'a>(&'a Cell<bool>);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl Runtime {
    pub fn mount(root: Element, options: CodeOptions) -> CodeResult<Rc<Self>> {
        options.validate()?;
        let editor = Editor::new(
            &options.initial_value,
            options.document_limits,
            options.history_limits,
        )?;
        let mut view = View::new(root)?;
        view.reconcile(editor.state().document(), true)?;
        let runtime = Rc::new_cyclic(|weak| Self {
            editor: RefCell::new(editor),
            view: RefCell::new(view),
            options: RefCell::new(options),
            pending_options: RefCell::new(None),
            status: RefCell::new(ViewStatus::Ready),
            composition: RefCell::new(None),
            generation: Cell::new(0),
            busy: Cell::new(false),
            finalizing: Cell::new(false),
            disposed: Cell::new(false),
            escape_tab: Cell::new(false),
            drag: RefCell::new(None),
            key_bindings: RefCell::new(Vec::new()),
            input_intent: RefCell::new(None),
            pending_native_history: Cell::new(None),
            metrics: RefCell::new(CodeMetrics::default()),
            observer: RefCell::new(None),
            observer_callback: RefCell::new(None),
            listeners: RefCell::new(Vec::new()),
            changes: Events::default(),
            view_events: Events::default(),
            composition_events: Events::default(),
            final_events: Events::default(),
            presentation_events: Events::default(),
            presentation: RefCell::new(PresentationStatus::Plain),
            highlight: RefCell::new(HighlightCache::new(HighlightLimits::default())),
            highlight_scheduled: Cell::new(false),
            clear_line: Cell::new(0),
            weak: weak.clone(),
        });
        runtime.apply_options()?;
        runtime.forward_accessibility()?;
        let weak = Rc::downgrade(&runtime);
        let callback = Closure::wrap(
            Box::new(move |records: js_sys::Array, _: MutationObserver| {
                if let Some(runtime) = weak.upgrade() {
                    let records = runtime.prepare_records(records);
                    if runtime.can_read_dom()
                        && !runtime.busy.get()
                        && let Ok(_guard) = runtime.enter(false)
                    {
                        let _ = runtime.import(records, false, EditOrigin::Input);
                    }
                }
            }) as Box<dyn FnMut(js_sys::Array, MutationObserver)>,
        );
        let observer =
            MutationObserver::new(callback.as_ref().unchecked_ref()).map_err(dom_error)?;
        let init = MutationObserverInit::new();
        init.set_subtree(true);
        init.set_child_list(true);
        init.set_character_data(true);
        observer
            .observe_with_options(&runtime.view.borrow().surface, &init)
            .map_err(dom_error)?;
        let attributes = MutationObserverInit::new();
        attributes.set_attributes(true);
        attributes.set_attribute_filter(
            &["aria-label", "aria-labelledby", "aria-describedby"]
                .into_iter()
                .map(wasm_bindgen::JsValue::from_str)
                .collect::<js_sys::Array>(),
        );
        observer
            .observe_with_options(&runtime.view.borrow().aria_owner, &attributes)
            .map_err(dom_error)?;
        *runtime.observer.borrow_mut() = Some(observer);
        *runtime.observer_callback.borrow_mut() = Some(callback);
        super::input::install(&runtime)?;
        super::drag::install(&runtime)?;
        runtime.refresh_highlights(false);
        Ok(runtime)
    }

    pub fn composing(&self) -> bool {
        self.composition.borrow().is_some()
    }
    pub fn can_read_dom(&self) -> bool {
        !self.disposed.get()
            && !self.composing()
            && matches!(*self.status.borrow(), ViewStatus::Ready)
            && {
                let view = self.view.borrow();
                view.root.is_connected() && view.root.contains(Some(&view.surface))
            }
    }
    pub fn check_live(&self) -> CodeResult<()> {
        if self.disposed.get() {
            Err(CodeError::Disposed)
        } else {
            Ok(())
        }
    }

    pub fn enter(&self, block_composition: bool) -> CodeResult<Busy<'_>> {
        self.check_live()?;
        if self.finalizing.get() {
            return Err(CodeError::Finalizing);
        }
        if self.busy.get() {
            return Err(CodeError::ReentrantDispatch);
        }
        if block_composition && self.composing() {
            return Err(CodeError::CompositionActive);
        }
        self.busy.set(true);
        Ok(Busy(&self.busy))
    }

    pub fn editable(&self) -> CodeResult<()> {
        let options = self.options.borrow();
        if options.disabled {
            Err(CodeError::Disabled)
        } else if options.read_only {
            Err(CodeError::ReadOnly)
        } else {
            Ok(())
        }
    }

    pub fn selectable(&self) -> CodeResult<()> {
        if self.options.borrow().disabled {
            Err(CodeError::Disabled)
        } else {
            Ok(())
        }
    }

    pub fn usable_view(&self) -> CodeResult<()> {
        self.selectable()?;
        if matches!(*self.status.borrow(), ViewStatus::Ready) {
            Ok(())
        } else {
            Err(CodeError::ViewUnavailable)
        }
    }

    pub fn now(&self) -> u64 {
        js_sys::Date::now() as u64
    }

    fn take_records(&self) -> js_sys::Array {
        let records = self
            .observer
            .borrow()
            .as_ref()
            .map_or_else(js_sys::Array::new, |observer| observer.take_records());
        self.prepare_records(records)
    }

    fn prepare_records(&self, records: js_sys::Array) -> js_sys::Array {
        let filtered = js_sys::Array::new();
        let mut attributes = false;
        for value in records.iter() {
            let record: MutationRecord = value.clone().unchecked_into();
            if record.type_() == "attributes" {
                attributes = true;
            } else {
                filtered.push(&value);
            }
        }
        if attributes && !self.disposed.get() {
            let _ = self.forward_accessibility();
        }
        filtered
    }

    fn forward_accessibility(&self) -> CodeResult<()> {
        let view = self.view.borrow();
        for attribute in ["aria-label", "aria-labelledby", "aria-describedby"] {
            if let Some(value) = view.aria_owner.get_attribute(attribute) {
                view.surface
                    .set_attribute(attribute, &value)
                    .map_err(dom_error)?;
            } else {
                view.surface
                    .remove_attribute(attribute)
                    .map_err(dom_error)?;
            }
        }
        Ok(())
    }

    pub fn listen(
        &self,
        target: &EventTarget,
        name: &'static str,
        callback: impl Fn(&Runtime, Event) + 'static,
    ) -> CodeResult<()> {
        let weak = self.weak.clone();
        let listener = Closure::wrap(Box::new(move |event: Event| {
            if let Some(runtime) = weak.upgrade().filter(|runtime| !runtime.disposed.get()) {
                callback(&runtime, event);
            }
        }) as Box<dyn FnMut(Event)>);
        target
            .add_event_listener_with_callback(name, listener.as_ref().unchecked_ref())
            .map_err(dom_error)?;
        self.listeners.borrow_mut().push(Listener {
            target: target.clone(),
            name,
            callback: listener,
        });
        Ok(())
    }

    /// Reads in callbacks/finalization are frozen and never import the draft.
    pub fn snapshot(&self) -> CodeResult<CodeSnapshot> {
        self.check_live()?;
        if !self.busy.get() && !self.finalizing.get() {
            self.flush()?;
        }
        Ok(self.committed_snapshot())
    }

    fn committed_snapshot(&self) -> CodeSnapshot {
        let editor = self.editor.borrow();
        let state = editor.state();
        CodeSnapshot {
            text: state.document().text().into(),
            revision: state.revision(),
            selection: state.selection(),
            composing: self.composing(),
            view_status: self.status.borrow().clone(),
            can_undo: editor.can_undo(),
            can_redo: editor.can_redo(),
            presentation_status: self.presentation.borrow().clone(),
        }
    }

    pub fn flush(&self) -> CodeResult<()> {
        if !self.can_read_dom() {
            return Ok(());
        }
        let _guard = self.enter(false)?;
        self.import(self.take_records(), false, EditOrigin::Input)?;
        Ok(())
    }

    pub fn flush_inside(&self) -> CodeResult<()> {
        if self.can_read_dom() {
            self.import(self.take_records(), false, EditOrigin::Input)?;
        }
        Ok(())
    }

    /// Publish the committed edit even if its DOM patch fails. Once this is
    /// called there is no transaction rejection or rollback path.
    pub fn publish(&self, change: CodeChange, repair: bool, selection: bool) -> CommitOutcome {
        self.publish_inner(change, repair, selection, false)
    }

    fn publish_inner(
        &self,
        mut change: CodeChange,
        repair: bool,
        selection: bool,
        native: bool,
    ) -> CommitOutcome {
        let old_status = self.status.borrow().clone();
        if matches!(old_status, ViewStatus::Ready)
            && let Err(error) = self.paint_mode(repair, selection, native)
        {
            *self.status.borrow_mut() = ViewStatus::Failed(error);
        }
        change.view_status = self.status.borrow().clone();
        if change.document_changed {
            self.refresh_highlights(change.origin == EditOrigin::Load);
        } else if repair {
            self.refresh_highlights(true);
        }
        let outcome = CommitOutcome {
            revision: change.revision,
            view_status: change.view_status.clone(),
        };
        if change.document_changed
            || change.selection_changed
            || change.revision != change.before_revision
        {
            self.changes.emit(&change);
        }
        if old_status != *self.status.borrow() {
            self.view_events.emit(&self.status.borrow().clone());
        }
        outcome
    }

    pub fn outcome(&self) -> CommitOutcome {
        CommitOutcome {
            revision: self.editor.borrow().state().revision(),
            view_status: self.status.borrow().clone(),
        }
    }

    fn paint(&self, repair: bool, selection: bool) -> CodeResult<()> {
        self.paint_mode(repair, selection, false)
    }

    fn paint_mode(&self, repair: bool, selection: bool, native: bool) -> CodeResult<()> {
        let result = (|| {
            let editor = self.editor.borrow();
            let mut view = self.view.borrow_mut();
            view.reconcile(editor.state().document(), repair)?;
            if selection && view.focused() && (!native || view.changed_dom) {
                view.restore_selection(editor.state().document(), editor.state().selection())?;
            }
            Ok(())
        })();
        self.take_records(); // Only our own patch records remain at this point.
        result
    }

    pub fn reject(&self, error: &CodeError) {
        self.view
            .borrow()
            .status
            .set_text_content(Some(&format!("Edit rejected: {error}")));
        let old = self.status.borrow().clone();
        if matches!(old, ViewStatus::Ready)
            && let Err(error) = self.paint(true, true)
        {
            *self.status.borrow_mut() = ViewStatus::Failed(error);
        }
        let status = self.status.borrow().clone();
        self.refresh_highlights(true);
        if old != status {
            self.view_events.emit(&status);
        }
    }

    /// Local text mutations read only their retained logical line region.
    /// Structural browser changes and terminal composition use the full reader.
    pub fn import(
        &self,
        records: js_sys::Array,
        full: bool,
        origin: EditOrigin,
    ) -> CodeResult<bool> {
        if !self.can_read_dom() {
            return Ok(false);
        }
        // Some native history events cannot be canceled. Their DOM result is
        // discarded; the corresponding Rust history operation owns the edit.
        if let Some(redo) = self.pending_native_history.take() {
            self.input_intent.borrow_mut().take();
            if let Err(error) = self.editable() {
                self.reject(&error);
                return Ok(false);
            }
            let change = if redo {
                self.editor.borrow_mut().redo()?
            } else {
                self.editor.borrow_mut().undo()?
            };
            if let Some(change) = change {
                let changed = change.document_changed;
                self.publish(change, true, true);
                return Ok(changed);
            }
            let mut editor = self.editor.borrow_mut();
            let selection = editor.state().selection();
            let revision = editor.state().revision();
            let unchanged = editor.set_selection(selection, revision)?;
            drop(editor);
            self.publish(unchanged, true, true);
            return Ok(false);
        }
        if records.length() == 0 && !full {
            self.read_selection()?;
            return Ok(false);
        }
        if let Err(error) = self.editable() {
            self.reject(&error);
            return Ok(false);
        }
        let intent = self
            .input_intent
            .borrow_mut()
            .take()
            .filter(|intent| intent.revision == self.editor.borrow().state().revision());
        let candidate = (|| {
            let view = self.view.borrow();
            let editor = self.editor.borrow();
            let state = editor.state();
            let document = state.document();
            let points = dom_reader::selection_points(&view.surface);
            let mut first = usize::MAX;
            let mut last = 0;
            let mut structural = full;
            for value in records.iter() {
                let record: MutationRecord = value.unchecked_into();
                match record.target().and_then(|target| view.point_line(&target)) {
                    Some(line) => {
                        first = first.min(line);
                        last = last.max(line);
                    }
                    None => structural = true,
                }
            }
            structural |= first == usize::MAX;
            let (changes, selection) = if structural {
                let read = dom_reader::read(view.surface.as_ref(), points.as_ref())?;
                {
                    let mut metrics = self.metrics.borrow_mut();
                    metrics.native_reads += 1;
                    metrics.full_dom_reads += 1;
                    metrics.last_read_lines = document.line_count();
                    metrics.last_read_bytes = read.text.len();
                }
                let selection = read.points[0]
                    .zip(read.points[1])
                    .map(|(a, h)| Selection::between(a, h));
                (
                    super::native_change::changes_from_region(
                        document,
                        crate::TextRange::new(0, document.len()),
                        &read.text,
                        editor.limits(),
                        intent.as_ref(),
                    )?,
                    selection,
                )
            } else {
                let start = view.lines[first].start;
                let end = view.lines[last].start + view.lines[last].text.len();
                let mut text = String::new();
                let mut positions = [None, None];
                for line in &view.lines[first..=last] {
                    if !text.is_empty()
                        || !line.element.is_same_node(Some(&view.lines[first].element))
                    {
                        text.push('\n');
                    }
                    let read = dom_reader::read(line.element.as_ref(), points.as_ref())?;
                    for (slot, point) in read.points.iter().enumerate() {
                        if let Some(offset) = point {
                            positions[slot] = Some(start + text.len() + offset);
                        }
                    }
                    text.push_str(&read.text);
                }
                // Derive the smallest scalar-aligned edit in this region so
                // history and selection mapping retain ordinary typing shape.
                {
                    let mut metrics = self.metrics.borrow_mut();
                    metrics.native_reads += 1;
                    metrics.last_read_lines = last - first + 1;
                    metrics.last_read_bytes = text.len();
                }
                let changes = super::native_change::changes_from_region(
                    document,
                    crate::TextRange::new(start, end),
                    &text,
                    editor.limits(),
                    intent.as_ref(),
                )?;
                if let Some(points) = &points {
                    for (slot, point) in points.iter().enumerate() {
                        if positions[slot].is_none() {
                            positions[slot] = Some(
                                changes
                                    .map_offset(
                                        document,
                                        crate::TextOffset(view.offset(point)?),
                                        crate::Bias::After,
                                    )?
                                    .0,
                            );
                        }
                    }
                }
                (
                    changes,
                    positions[0]
                        .zip(positions[1])
                        .map(|(a, h)| Selection::between(a, h)),
                )
            };
            let mut transaction = state.transaction(changes, origin);
            transaction.selection = selection;
            Ok((transaction, structural))
        })();
        let (transaction, structural) = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.reject(&error);
                return Err(error);
            }
        };
        let result = self.editor.borrow_mut().dispatch(transaction, self.now());
        let change = match result {
            Ok(change) => change,
            Err(error) => {
                self.reject(&error);
                return Err(error);
            }
        };
        let changed = change.document_changed;
        self.publish_inner(change, structural || !changed, true, true);
        Ok(changed)
    }

    pub fn read_selection(&self) -> CodeResult<()> {
        if !self.can_read_dom() || self.options.borrow().disabled {
            return Ok(());
        }
        let selection = {
            let editor = self.editor.borrow();
            self.view.borrow().selection(editor.state().document())?
        };
        if let Some(selection) = selection {
            let revision = self.editor.borrow().state().revision();
            let change = self
                .editor
                .borrow_mut()
                .set_selection(selection, revision)?;
            if change.selection_changed {
                self.publish(change, false, false);
            }
        }
        Ok(())
    }

    pub fn start_composition(&self) {
        self.begin_composition(true);
    }

    pub fn begin_composition(&self, flush_pending: bool) {
        if self.busy.get() || self.finalizing.get() {
            return;
        }
        if self.composition.borrow().as_ref().is_some_and(|c| c.ended) {
            self.finish_composition();
        }
        if self.composing() {
            return;
        }
        let Ok(_guard) = self.enter(false) else {
            return;
        };
        if flush_pending {
            let _ = self.flush_inside();
        }
        self.editor.borrow_mut().close_history_group();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        *self.composition.borrow_mut() = Some(Composition {
            generation,
            ended: false,
            rejected: self.editable().is_err(),
        });
        self.composition_events.emit(&CompositionStatus::Started);
    }

    pub fn end_composition(&self) {
        let generation = {
            let mut composition = self.composition.borrow_mut();
            let Some(composition) = composition.as_mut() else {
                return;
            };
            composition.ended = true;
            composition.generation
        };
        // Some engines deliver the final input after compositionend. Keep all
        // nodes protected until that task has finished; a new session cancels
        // this generation's scheduled work.
        let weak = self.weak.clone();
        pocopine::tick::next_frame(move || {
            if let Some(runtime) = weak.upgrade() {
                let ended = runtime
                    .composition
                    .borrow()
                    .as_ref()
                    .is_some_and(|c| c.generation == generation && c.ended);
                if ended && !runtime.disposed.get() {
                    runtime.finish_composition();
                }
            }
        });
    }

    pub fn settle_ended_composition(&self) {
        if self
            .composition
            .borrow()
            .as_ref()
            .is_some_and(|composition| composition.ended)
        {
            self.finish_composition();
        }
    }

    fn finish_composition(&self) {
        let Ok(_guard) = self.enter(false) else {
            return;
        };
        self.settle_composition();
    }

    fn settle_composition(&self) {
        let Some(composition) = self.composition.borrow_mut().take() else {
            return;
        };
        let result = if composition.rejected {
            let error = self.editable().err().unwrap_or(CodeError::ReadOnly);
            self.reject(&error);
            CompositionStatus::Rejected(error)
        } else {
            match self.import(self.take_records(), true, EditOrigin::Composition) {
                Ok(true) => CompositionStatus::Committed,
                Ok(false) => CompositionStatus::Canceled,
                Err(error) => CompositionStatus::Rejected(error),
            }
        };
        self.editor.borrow_mut().close_history_group();
        if let Some(options) = self.pending_options.borrow_mut().take() {
            *self.options.borrow_mut() = options;
            self.publish_options();
            self.refresh_highlights(false);
        }
        self.composition_events.emit(&result);
        self.queue_highlights();
    }

    pub fn configure(&self, options: CodeOptions) -> CodeResult<()> {
        let _guard = self.enter(false)?;
        options.validate()?;
        if options.document_limits != self.options.borrow().document_limits
            || options.history_limits != self.options.borrow().history_limits
        {
            return Err(CodeError::InvalidConfiguration);
        }
        if self.composing() {
            *self.pending_options.borrow_mut() = Some(options);
            return Ok(());
        }
        self.flush_inside()?;
        *self.options.borrow_mut() = options;
        self.publish_options();
        self.refresh_highlights(false);
        Ok(())
    }

    fn publish_options(&self) {
        if let Err(error) = self.apply_options() {
            let status = ViewStatus::Failed(error);
            *self.status.borrow_mut() = status.clone();
            self.view_events.emit(&status);
        }
    }

    fn apply_options(&self) -> CodeResult<()> {
        let view = self.view.borrow();
        let options = self.options.borrow();
        view.root
            .set_attribute("data-theme", &options.theme)
            .map_err(dom_error)?;
        view.gutter.set_attribute("hidden", "").map_err(dom_error)?;
        if options.line_numbers {
            view.gutter.remove_attribute("hidden").map_err(dom_error)?;
        }
        view.surface
            .set_attribute("style", &format!("tab-size:{}", options.tab_size))
            .map_err(dom_error)?;
        view.surface
            .set_attribute(
                "contenteditable",
                if options.disabled { "false" } else { "true" },
            )
            .map_err(dom_error)?;
        view.surface
            .set_attribute("tabindex", if options.disabled { "-1" } else { "0" })
            .map_err(dom_error)?;
        view.surface
            .set_attribute(
                "aria-disabled",
                if options.disabled { "true" } else { "false" },
            )
            .map_err(dom_error)?;
        view.surface
            .set_attribute(
                "aria-readonly",
                if options.read_only { "true" } else { "false" },
            )
            .map_err(dom_error)?;
        if options.disabled && view.focused() {
            view.surface
                .unchecked_ref::<HtmlElement>()
                .blur()
                .map_err(dom_error)?;
        }
        Ok(())
    }

    pub fn recover(&self) -> CodeResult<ViewStatus> {
        let _guard = self.enter(true)?;
        self.flush_inside()?;
        self.take_records();
        let old = self.status.borrow().clone();
        let result = (|| {
            self.view.borrow().recover_structure()?;
            self.apply_options()?;
            self.forward_accessibility()?;
            self.paint(true, true)
        })();
        *self.status.borrow_mut() = match result {
            Ok(()) => ViewStatus::Ready,
            Err(error) => ViewStatus::Failed(error),
        };
        let status = self.status.borrow().clone();
        self.refresh_highlights(true);
        if status != old {
            self.view_events.emit(&status);
        }
        Ok(status)
    }

    pub fn finalize(&self, attached: bool) {
        if self.disposed.get() || self.finalizing.replace(true) {
            return;
        }
        self.busy.set(true);
        let mut error = (!attached).then_some(CodeError::UnexpectedDetach);
        if attached && self.composition.borrow().as_ref().is_some_and(|c| c.ended) {
            self.settle_composition();
        }
        let composition = self.composition.borrow_mut().take();
        let interrupted = if let Some(composition) = composition {
            if composition.rejected {
                self.composition_events.emit(&CompositionStatus::Rejected(
                    self.editable().err().unwrap_or(CodeError::ReadOnly),
                ));
                None
            } else {
                let view = self.view.borrow();
                let points = dom_reader::selection_points(&view.surface);
                let read = dom_reader::read(view.surface.as_ref(), points.as_ref());
                let editor = self.editor.borrow();
                let (text, selection, draft_error) = match read {
                    Ok(read) => {
                        let validation = crate::TextDocument::new(&read.text, editor.limits());
                        let selection = read.points[0]
                            .zip(read.points[1])
                            .map(|(a, h)| Selection::between(a, h));
                        let validation_error = validation
                            .and_then(|doc| selection.map_or(Ok(()), |s| doc.check_selection(s)))
                            .err();
                        (Some(read.text), selection, validation_error)
                    }
                    Err(error) => (None, None, Some(error)),
                };
                Some(InterruptedComposition {
                    text,
                    selection,
                    base_revision: editor.state().revision(),
                    error: draft_error,
                })
            }
        } else {
            if let Err(drain_error) = self.flush_inside() {
                error.get_or_insert(drain_error);
            }
            None
        };
        let snapshot = CodeFinalSnapshot {
            committed: self.committed_snapshot(),
            interrupted,
            error,
        };
        if snapshot.interrupted.is_some() {
            self.composition_events
                .emit(&CompositionStatus::Interrupted);
        }
        self.final_events.emit(&snapshot);
        self.disposed.set(true);
        if let Some(observer) = self.observer.borrow_mut().take() {
            observer.disconnect();
        }
        self.listeners.borrow_mut().clear();
        self.observer_callback.borrow_mut().take();
        self.changes.clear();
        self.view_events.clear();
        self.composition_events.clear();
        self.final_events.clear();
        self.presentation_events.clear();
        self.composition.borrow_mut().take();
        self.drag.borrow_mut().take();
    }

    fn refresh_highlights(&self, reset: bool) {
        {
            let editor = self.editor.borrow();
            let options = self.options.borrow();
            let mut cache = self.highlight.borrow_mut();
            if reset {
                cache.invalidate();
            }
            cache.update(editor.state().document(), &options.language);
        }
        if reset
            || self.view.borrow().structure_changed
            || !matches!(
                *self.presentation.borrow(),
                PresentationStatus::Plain | PresentationStatus::PlainFallback(_)
            )
        {
            self.clear_line.set(0);
        }
        self.queue_highlights();
    }

    fn queue_highlights(&self) {
        if self.disposed.get()
            || self.finalizing.get()
            || self.composing()
            || self.highlight_scheduled.replace(true)
        {
            return;
        }
        let weak = self.weak.clone();
        pocopine::tick::next_frame(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.highlight_scheduled.set(false);
                runtime.highlight_frame();
            }
        });
    }

    fn highlight_frame(&self) {
        if self.disposed.get()
            || self.finalizing.get()
            || self.composing()
            || !matches!(*self.status.borrow(), ViewStatus::Ready)
        {
            return;
        }
        let Ok(_guard) = self.enter(true) else {
            self.queue_highlights();
            return;
        };
        if self.flush_inside().is_err() {
            return;
        }
        let started = web_sys::window()
            .and_then(|window| window.performance())
            .map_or(0.0, |performance| performance.now());
        let mut patched = false;
        let result = (|| {
            loop {
                let status = self.highlight.borrow().status.clone();
                match status {
                    PresentationStatus::Pending => {
                        let step = self.highlight.borrow_mut().next_line();
                        match step {
                            Ok(Some(step)) => {
                                patched |= self
                                    .view
                                    .borrow_mut()
                                    .patch_tokens(step.line, &step.tokens)?;
                            }
                            Ok(None) => break,
                            Err(_) => {
                                self.clear_line.set(0);
                            }
                        }
                    }
                    PresentationStatus::Plain | PresentationStatus::PlainFallback(_) => {
                        let index = self.clear_line.get();
                        if index >= self.view.borrow().lines.len() {
                            break;
                        }
                        patched |= self.view.borrow_mut().patch_tokens(index, &[])?;
                        self.clear_line.set(index + 1);
                    }
                    PresentationStatus::Highlighted => break,
                }
                let now = web_sys::window()
                    .and_then(|window| window.performance())
                    .map_or(started, |performance| performance.now());
                if now - started >= f64::from(HighlightLimits::default().slice_ms) {
                    break;
                }
            }
            if patched {
                let editor = self.editor.borrow();
                let view = self.view.borrow();
                if view.focused() {
                    view.restore_selection(editor.state().document(), editor.state().selection())?;
                }
            }
            Ok::<_, CodeError>(())
        })();
        self.take_records();
        if let Err(error) = result {
            let status = ViewStatus::Failed(error);
            *self.status.borrow_mut() = status.clone();
            self.view_events.emit(&status);
            return;
        }
        let status = self.highlight.borrow().status.clone();
        if *self.presentation.borrow() != status {
            *self.presentation.borrow_mut() = status.clone();
            let value = match &status {
                PresentationStatus::Plain => "plain",
                PresentationStatus::Pending => "pending",
                PresentationStatus::Highlighted => "highlighted",
                PresentationStatus::PlainFallback(_) => "plain-fallback",
            };
            let _ = self
                .view
                .borrow()
                .root
                .set_attribute("data-presentation", value);
            self.presentation_events.emit(&status);
        }
        if status == PresentationStatus::Pending
            || (matches!(
                status,
                PresentationStatus::Plain | PresentationStatus::PlainFallback(_)
            ) && self.clear_line.get() < self.view.borrow().lines.len())
        {
            self.queue_highlights();
        }
    }
}
