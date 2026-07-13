//! Selection-aware floating controls for a Pine rich-text editor.
//!
//! [`PineRichTextBubbleMenu`] is the declarative Pocopine component. It finds
//! an editor by CSS selector (or the nearest editor in its ancestor subtree),
//! follows text, node, and rectangular cell selections, and keeps author-owned
//! controls mounted while moving only the menu shell.
//!
//! ```html
//! <section class="editor-with-menu">
//!   <pine-rich-text-root id="body-editor"></pine-rich-text-root>
//!   <pine-rich-text-bubble-menu
//!     editor="#body-editor"
//!     placement="top"
//!     align="center"
//!     offset="8">
//!     <button type="button">Bold</button>
//!     <button type="button">Link</button>
//!   </pine-rich-text-bubble-menu>
//! </section>
//! ```
//!
//! Applications that need a Rust `should_show` predicate or a searchable
//! command surface can attach [`BubbleMenuController`] directly to any menu
//! element. The controller and its subscriptions are RAII values: dropping it
//! removes listeners, cancels a queued animation frame, and hides the menu.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use pine_richtext::state::{Selection, SelectionBookmark};
use pine_richtext::transform::{Mapping, Step};
use pine_richtext::view::{
    Doc, DocChangeSubscription, Editor, SelectionChangeSubscription, SelectionSnapshot,
    ViewportRect,
};
use pocopine::current_scope_id;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, Event, EventTarget, HtmlElement, KeyboardEvent};

/// Stable class on the positioned menu shell.
pub const BUBBLE_MENU_CLASS: &str = "pine-richtext-bubble-menu";

static NEXT_MENU_KEY: AtomicU64 = AtomicU64::new(1);
static NEXT_SEARCH_SESSION: AtomicU64 = AtomicU64::new(1);

/// Per-menu identity, analogous to Tiptap's per-plugin `PluginKey`.
///
/// Construction always allocates a fresh identity. Cloning preserves the same
/// identity, which lets controller-owned callbacks name their menu without
/// allowing two independently-created menus to collide accidentally.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BubbleMenuKey(String);

impl BubbleMenuKey {
    /// Allocate a globally unique key for one menu instance.
    pub fn new() -> Self {
        let id = NEXT_MENU_KEY.fetch_add(1, Ordering::Relaxed);
        Self(format!("pine-richtext-bubble-menu-{id}"))
    }

    /// The DOM-safe identity string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for BubbleMenuKey {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BubbleMenuKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Preferred side of the selection rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BubbleMenuPlacement {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl BubbleMenuPlacement {
    fn opposite(self) -> Self {
        match self {
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "bottom" => Self::Bottom,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Top,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// Alignment on the placement's cross axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BubbleMenuAlign {
    Start,
    #[default]
    Center,
    End,
}

impl BubbleMenuAlign {
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "start" => Self::Start,
            "end" => Self::End,
            _ => Self::Center,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }
}

/// Selection shape used for the current virtual anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BubbleAnchorKind {
    Text,
    Node,
    Cells,
    Document,
}

impl BubbleAnchorKind {
    fn from_selection(selection: &Selection) -> Self {
        match selection {
            Selection::Text { .. } => Self::Text,
            Selection::Node { .. } => Self::Node,
            Selection::Cells { .. } => Self::Cells,
            Selection::All => Self::Document,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Node => "node",
            Self::Cells => "cells",
            Self::Document => "document",
        }
    }
}

/// Read-only context passed to an application `should_show` predicate.
#[derive(Clone, Debug)]
pub struct BubbleMenuContext {
    pub key: BubbleMenuKey,
    pub snapshot: SelectionSnapshot,
    pub anchor_kind: BubbleAnchorKind,
    pub anchor_rect: ViewportRect,
    /// True while focus is inside the menu or a pointer press began there.
    pub menu_interacting: bool,
}

/// Browser-independent dimensions used by [`compute_position`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BubbleMenuSize {
    pub width: f64,
    pub height: f64,
}

/// Browser-independent viewport bounds used by [`compute_position`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BubbleViewport {
    pub width: f64,
    pub height: f64,
}

/// Resolved fixed-position coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BubbleMenuPosition {
    pub x: f64,
    pub y: f64,
    pub placement: BubbleMenuPlacement,
    pub align: BubbleMenuAlign,
}

type ShowPredicate = Rc<dyn Fn(&BubbleMenuContext) -> bool>;

/// Runtime behavior for [`BubbleMenuController`].
#[derive(Clone)]
pub struct BubbleMenuOptions {
    pub placement: BubbleMenuPlacement,
    pub align: BubbleMenuAlign,
    pub offset: f64,
    pub viewport_padding: f64,
    pub flip: bool,
    /// Minimum quiet period before a queued position update is applied.
    /// Updates remain animation-frame coalesced even when this is zero.
    pub debounce_ms: f64,
    pub show_on_empty: bool,
    pub show_when_read_only: bool,
    pub escape_refocus: bool,
    pub searchable: bool,
    should_show: Option<ShowPredicate>,
}

impl Default for BubbleMenuOptions {
    fn default() -> Self {
        Self {
            placement: BubbleMenuPlacement::Top,
            align: BubbleMenuAlign::Center,
            offset: 8.0,
            viewport_padding: 8.0,
            flip: true,
            debounce_ms: 0.0,
            show_on_empty: false,
            show_when_read_only: false,
            escape_refocus: true,
            searchable: false,
            should_show: None,
        }
    }
}

impl fmt::Debug for BubbleMenuOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BubbleMenuOptions")
            .field("placement", &self.placement)
            .field("align", &self.align)
            .field("offset", &self.offset)
            .field("viewport_padding", &self.viewport_padding)
            .field("flip", &self.flip)
            .field("debounce_ms", &self.debounce_ms)
            .field("show_on_empty", &self.show_on_empty)
            .field("show_when_read_only", &self.show_when_read_only)
            .field("escape_refocus", &self.escape_refocus)
            .field("searchable", &self.searchable)
            .field("has_should_show", &self.should_show.is_some())
            .finish()
    }
}

impl BubbleMenuOptions {
    /// Add an application predicate. It may narrow the default visibility
    /// policy; detached, geometry-less, empty, or read-only selections remain
    /// hidden unless their corresponding explicit options allow them.
    pub fn with_should_show(
        mut self,
        predicate: impl Fn(&BubbleMenuContext) -> bool + 'static,
    ) -> Self {
        self.should_show = Some(Rc::new(predicate));
        self
    }
}

/// Compute a menu position without touching the DOM.
///
/// The preferred side flips when it lacks main-axis room, then the final point
/// is clamped to `viewport_padding`. This small pure function is shared by the
/// browser controller and host tests.
pub fn compute_position(
    anchor: ViewportRect,
    menu: BubbleMenuSize,
    viewport: BubbleViewport,
    options: &BubbleMenuOptions,
) -> BubbleMenuPosition {
    let padding = finite_non_negative(options.viewport_padding);
    let offset = finite(options.offset, 0.0);
    let preferred = options.placement;
    let placement = if options.flip
        && !main_axis_fits(preferred, anchor, menu, viewport, offset, padding)
        && main_axis_fits(
            preferred.opposite(),
            anchor,
            menu,
            viewport,
            offset,
            padding,
        ) {
        preferred.opposite()
    } else {
        preferred
    };

    let (mut x, mut y) = raw_position(placement, options.align, anchor, menu, offset);
    let max_x = (viewport.width - padding - menu.width).max(padding);
    let max_y = (viewport.height - padding - menu.height).max(padding);
    x = x.clamp(padding, max_x);
    y = y.clamp(padding, max_y);

    BubbleMenuPosition {
        x,
        y,
        placement,
        align: options.align,
    }
}

fn main_axis_fits(
    placement: BubbleMenuPlacement,
    anchor: ViewportRect,
    menu: BubbleMenuSize,
    viewport: BubbleViewport,
    offset: f64,
    padding: f64,
) -> bool {
    match placement {
        BubbleMenuPlacement::Top => anchor.top - offset - menu.height >= padding,
        BubbleMenuPlacement::Bottom => {
            anchor.bottom + offset + menu.height <= viewport.height - padding
        }
        BubbleMenuPlacement::Left => anchor.left - offset - menu.width >= padding,
        BubbleMenuPlacement::Right => {
            anchor.right + offset + menu.width <= viewport.width - padding
        }
    }
}

fn raw_position(
    placement: BubbleMenuPlacement,
    align: BubbleMenuAlign,
    anchor: ViewportRect,
    menu: BubbleMenuSize,
    offset: f64,
) -> (f64, f64) {
    match placement {
        BubbleMenuPlacement::Top | BubbleMenuPlacement::Bottom => {
            let x = match align {
                BubbleMenuAlign::Start => anchor.left,
                BubbleMenuAlign::Center => anchor.left + (anchor.width - menu.width) / 2.0,
                BubbleMenuAlign::End => anchor.right - menu.width,
            };
            let y = if placement == BubbleMenuPlacement::Top {
                anchor.top - menu.height - offset
            } else {
                anchor.bottom + offset
            };
            (x, y)
        }
        BubbleMenuPlacement::Left | BubbleMenuPlacement::Right => {
            let y = match align {
                BubbleMenuAlign::Start => anchor.top,
                BubbleMenuAlign::Center => anchor.top + (anchor.height - menu.height) / 2.0,
                BubbleMenuAlign::End => anchor.bottom - menu.height,
            };
            let x = if placement == BubbleMenuPlacement::Left {
                anchor.left - menu.width - offset
            } else {
                anchor.right + offset
            };
            (x, y)
        }
    }
}

fn finite(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

fn finite_non_negative(value: f64) -> f64 {
    finite(value, 0.0).max(0.0)
}

/// Observable controller state. The shell also mirrors these values to stable
/// data attributes and CSS custom properties.
#[derive(Clone, Debug, PartialEq)]
pub struct BubbleMenuState {
    pub visible: bool,
    pub x: f64,
    pub y: f64,
    pub placement: BubbleMenuPlacement,
    pub align: BubbleMenuAlign,
    pub anchor_kind: BubbleAnchorKind,
}

impl Default for BubbleMenuState {
    fn default() -> Self {
        Self {
            visible: false,
            x: 0.0,
            y: 0.0,
            placement: BubbleMenuPlacement::Top,
            align: BubbleMenuAlign::Center,
            anchor_kind: BubbleAnchorKind::Text,
        }
    }
}

/// Errors returned while attaching or using a bubble menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BubbleMenuError {
    EditorNotFound(String),
    SurfaceUnavailable,
    SearchDisabled,
}

impl fmt::Display for BubbleMenuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EditorNotFound(selector) => {
                write!(f, "no Pine rich-text editor matched `{selector}`")
            }
            Self::SurfaceUnavailable => f.write_str("rich-text editor surface is unavailable"),
            Self::SearchDisabled => f.write_str("bubble-menu search is not enabled"),
        }
    }
}

impl std::error::Error for BubbleMenuError {}

/// Token returned for one asynchronous search request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequestToken {
    session: u64,
    generation: u64,
    query: String,
}

impl SearchRequestToken {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// Accepted asynchronous result paired with the latest mapped selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedSearchResult<T> {
    pub value: T,
    pub selection: SelectionBookmark,
}

/// Returned when an older asynchronous result arrives after a newer query or
/// after the search session was canceled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleSearchResult {
    pub query: String,
}

impl fmt::Display for StaleSearchResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stale bubble-menu search result for `{}`", self.query)
    }
}

impl std::error::Error for StaleSearchResult {}

#[derive(Clone)]
pub struct BubbleMenuSearch {
    inner: Rc<RefCell<SearchState>>,
}

#[derive(Debug)]
struct SearchState {
    session: u64,
    next_generation: u64,
    active: Option<ActiveSearch>,
}

#[derive(Debug)]
struct ActiveSearch {
    generation: u64,
    query: String,
    bookmark: SelectionBookmark,
}

impl BubbleMenuSearch {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(SearchState {
                session: NEXT_SEARCH_SESSION.fetch_add(1, Ordering::Relaxed),
                next_generation: 1,
                active: None,
            })),
        }
    }

    /// Start a query against the current selection. Starting another query
    /// makes the previous token stale immediately.
    pub fn begin(&self, query: impl Into<String>, selection: &Selection) -> SearchRequestToken {
        let query = query.into();
        let mut state = self.inner.borrow_mut();
        let generation = state.next_generation;
        state.next_generation = state.next_generation.saturating_add(1);
        state.active = Some(ActiveSearch {
            generation,
            query: query.clone(),
            bookmark: selection.bookmark(),
        });
        SearchRequestToken {
            session: state.session,
            generation,
            query,
        }
    }

    /// Map the preserved bookmark through committed document steps while an
    /// asynchronous provider is running.
    pub fn map_steps(&self, steps: &[Step]) {
        let mut mapping = Mapping::new();
        for step in steps {
            mapping.append_map(step.map());
        }
        self.map(&mapping);
    }

    /// Map the preserved bookmark through an already-built mapping.
    pub fn map(&self, mapping: &Mapping) {
        if mapping.maps.is_empty() {
            return;
        }
        if let Some(active) = self.inner.borrow_mut().active.as_mut() {
            active.bookmark = active.bookmark.map(mapping);
        }
    }

    /// Accept a provider result only if its token is still current. The active
    /// request is consumed, so duplicate delivery is stale too.
    pub fn finish<T>(
        &self,
        token: SearchRequestToken,
        value: T,
    ) -> Result<MappedSearchResult<T>, StaleSearchResult> {
        let mut state = self.inner.borrow_mut();
        let is_current = token.session == state.session
            && state.active.as_ref().is_some_and(|active| {
                active.generation == token.generation && active.query == token.query
            });
        if !is_current {
            return Err(StaleSearchResult { query: token.query });
        }
        let active = state.active.take().expect("checked active search");
        Ok(MappedSearchResult {
            value,
            selection: active.bookmark,
        })
    }

    /// Invalidate any outstanding request.
    pub fn cancel(&self) {
        self.inner.borrow_mut().active = None;
    }
}

impl Default for BubbleMenuSearch {
    fn default() -> Self {
        Self::new()
    }
}

struct FrameScheduler {
    active: Cell<bool>,
    pending: Cell<Option<i32>>,
    changed_at: Cell<f64>,
    debounce_ms: f64,
    latest: RefCell<Option<SelectionSnapshot>>,
    apply: RefCell<Box<dyn FnMut(SelectionSnapshot)>>,
    frame: Closure<dyn FnMut(f64)>,
}

impl FrameScheduler {
    fn new(debounce_ms: f64, apply: impl FnMut(SelectionSnapshot) + 'static) -> Rc<Self> {
        Rc::new_cyclic(|weak: &Weak<Self>| {
            let weak = weak.clone();
            let frame = Closure::wrap(Box::new(move |timestamp: f64| {
                if let Some(scheduler) = weak.upgrade() {
                    scheduler.on_frame(timestamp);
                }
            }) as Box<dyn FnMut(f64)>);
            Self {
                active: Cell::new(true),
                pending: Cell::new(None),
                changed_at: Cell::new(0.0),
                debounce_ms: finite_non_negative(debounce_ms),
                latest: RefCell::new(None),
                apply: RefCell::new(Box::new(apply)),
                frame,
            }
        })
    }

    fn schedule(&self, snapshot: SelectionSnapshot) {
        if !self.active.get() {
            return;
        }
        *self.latest.borrow_mut() = Some(snapshot);
        self.changed_at.set(now_ms());
        self.request_frame();
    }

    fn request_frame(&self) {
        if !self.active.get() || self.pending.get().is_some() {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        if let Ok(id) = window.request_animation_frame(self.frame.as_ref().unchecked_ref()) {
            self.pending.set(Some(id));
        }
    }

    fn on_frame(&self, timestamp: f64) {
        self.pending.set(None);
        if !self.active.get() {
            return;
        }
        let elapsed = timestamp - self.changed_at.get();
        if self.debounce_ms > 0.0 && elapsed < self.debounce_ms {
            self.request_frame();
            return;
        }
        if let Some(snapshot) = self.latest.borrow_mut().take() {
            (self.apply.borrow_mut())(snapshot);
        }
    }

    fn cancel(&self) {
        self.active.set(false);
        self.latest.borrow_mut().take();
        if let Some(id) = self.pending.take()
            && let Some(window) = web_sys::window()
        {
            let _ = window.cancel_animation_frame(id);
        }
    }
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| performance.now())
        .unwrap_or(0.0)
}

struct DomListener {
    target: EventTarget,
    event: &'static str,
    capture: bool,
    callback: Closure<dyn FnMut(Event)>,
}

impl DomListener {
    fn new<T>(
        target: &T,
        event: &'static str,
        capture: bool,
        callback: impl FnMut(Event) + 'static,
    ) -> Self
    where
        T: AsRef<EventTarget>,
    {
        let callback = Closure::wrap(Box::new(callback) as Box<dyn FnMut(Event)>);
        let target = target.as_ref().clone();
        let _ = target.add_event_listener_with_callback_and_bool(
            event,
            callback.as_ref().unchecked_ref(),
            capture,
        );
        Self {
            target,
            event,
            capture,
            callback,
        }
    }
}

impl Drop for DomListener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback_and_bool(
            self.event,
            self.callback.as_ref().unchecked_ref(),
            self.capture,
        );
    }
}

struct MenuPresenter {
    menu: Element,
    state: Rc<RefCell<BubbleMenuState>>,
    on_state: RefCell<Box<dyn FnMut(BubbleMenuState)>>,
    notify: Cell<bool>,
}

impl MenuPresenter {
    fn publish(&self, next: BubbleMenuState) {
        if *self.state.borrow() == next {
            return;
        }
        *self.state.borrow_mut() = next.clone();
        if self.notify.get() {
            (self.on_state.borrow_mut())(next);
        }
    }

    fn hide(&self) {
        let _ = self.menu.set_attribute("hidden", "");
        let _ = self.menu.set_attribute("aria-hidden", "true");
        let _ = self.menu.set_attribute("data-state", "closed");
        let mut next = self.state.borrow().clone();
        next.visible = false;
        self.publish(next);
    }

    fn show(&self, position: BubbleMenuPosition, kind: BubbleAnchorKind) {
        if let Some(html) = self.menu.dyn_ref::<HtmlElement>() {
            let style = html.style();
            let x = format!("{}px", position.x);
            let y = format!("{}px", position.y);
            let _ = style.set_property("--pine-richtext-bubble-menu-x", &x);
            let _ = style.set_property("--pine-richtext-bubble-menu-y", &y);
            let _ = style.set_property("left", &x);
            let _ = style.set_property("top", &y);
        }
        let _ = self.menu.remove_attribute("hidden");
        let _ = self.menu.set_attribute("aria-hidden", "false");
        let _ = self.menu.set_attribute("data-state", "open");
        let _ = self
            .menu
            .set_attribute("data-placement", position.placement.as_str());
        let _ = self
            .menu
            .set_attribute("data-align", position.align.as_str());
        let _ = self.menu.set_attribute("data-anchor-kind", kind.as_str());
        self.publish(BubbleMenuState {
            visible: true,
            x: position.x,
            y: position.y,
            placement: position.placement,
            align: position.align,
            anchor_kind: kind,
        });
    }
}

/// Owns one menu's editor subscription, DOM listeners, positioning scheduler,
/// and optional searchable-command bookmark.
#[must_use = "dropping the controller detaches and hides the bubble menu"]
pub struct BubbleMenuController {
    key: BubbleMenuKey,
    editor: Editor,
    presenter: Rc<MenuPresenter>,
    scheduler: Rc<FrameScheduler>,
    last_snapshot: Rc<RefCell<Option<SelectionSnapshot>>>,
    dismissed_selection: Rc<RefCell<Option<Selection>>>,
    selection_subscription: Option<SelectionChangeSubscription>,
    document_subscription: Option<DocChangeSubscription>,
    listeners: Vec<DomListener>,
    search: Option<BubbleMenuSearch>,
}

impl BubbleMenuController {
    /// Attach with the default state callback.
    pub fn attach(
        editor: Editor,
        menu: Element,
        options: BubbleMenuOptions,
    ) -> Result<Self, BubbleMenuError> {
        Self::attach_with_state(editor, menu, options, |_| {})
    }

    /// Attach and receive each visible/position state transition.
    pub fn attach_with_state(
        editor: Editor,
        menu: Element,
        options: BubbleMenuOptions,
        on_state: impl FnMut(BubbleMenuState) + 'static,
    ) -> Result<Self, BubbleMenuError> {
        let key = BubbleMenuKey::new();
        ensure_class(&menu, BUBBLE_MENU_CLASS);
        let _ = menu.set_attribute("data-plugin-key", key.as_str());
        let _ = menu.set_attribute("hidden", "");
        let _ = menu.set_attribute("aria-hidden", "true");
        let _ = menu.set_attribute("data-state", "closed");

        let state = Rc::new(RefCell::new(BubbleMenuState::default()));
        let presenter = Rc::new(MenuPresenter {
            menu: menu.clone(),
            state,
            on_state: RefCell::new(Box::new(on_state)),
            notify: Cell::new(true),
        });
        let pointer_active = Rc::new(Cell::new(false));
        let last_snapshot = Rc::new(RefCell::new(None));
        let dismissed_selection = Rc::new(RefCell::new(None));

        let presenter_for_apply = presenter.clone();
        let menu_for_apply = menu.clone();
        let editor_for_apply = editor.clone();
        let key_for_apply = key.clone();
        let pointer_for_apply = pointer_active.clone();
        let dismissed_for_apply = dismissed_selection.clone();
        let options_for_apply = options.clone();
        let scheduler = FrameScheduler::new(options.debounce_ms, move |snapshot| {
            let kind = BubbleAnchorKind::from_selection(&snapshot.selection);
            let same_as_dismissed = dismissed_for_apply
                .borrow()
                .as_ref()
                .is_some_and(|dismissed| dismissed == &snapshot.selection);
            if !same_as_dismissed {
                dismissed_for_apply.borrow_mut().take();
            }

            let connected =
                menu_for_apply.is_connected() && editor_for_apply.element().is_connected();
            let menu_interacting = pointer_for_apply.get() || menu_contains_focus(&menu_for_apply);
            let Some(anchor_rect) = snapshot.rect else {
                presenter_for_apply.hide();
                return;
            };
            let default_visible = connected
                && !same_as_dismissed
                && (options_for_apply.show_on_empty || !snapshot.empty)
                && (options_for_apply.show_when_read_only || snapshot.editable)
                && (snapshot.focused || menu_interacting);
            if !default_visible {
                presenter_for_apply.hide();
                return;
            }
            let context = BubbleMenuContext {
                key: key_for_apply.clone(),
                snapshot,
                anchor_kind: kind,
                anchor_rect,
                menu_interacting,
            };
            if options_for_apply
                .should_show
                .as_ref()
                .is_some_and(|predicate| !predicate(&context))
            {
                presenter_for_apply.hide();
                return;
            }

            // `hidden` menus have a zero bounding rect. Reveal invisibly for
            // one synchronous measurement, then publish the final position.
            let _ = menu_for_apply.remove_attribute("hidden");
            let _ = menu_for_apply.set_attribute("data-measuring", "true");
            let rect = menu_for_apply.get_bounding_client_rect();
            let menu_size = BubbleMenuSize {
                width: rect.width(),
                height: rect.height(),
            };
            let _ = menu_for_apply.remove_attribute("data-measuring");
            let viewport = viewport_size();
            presenter_for_apply.show(
                compute_position(anchor_rect, menu_size, viewport, &options_for_apply),
                kind,
            );
        });

        let subscription_scheduler = scheduler.clone();
        let snapshot_for_subscription = last_snapshot.clone();
        let selection_subscription = editor.on_selection_change(move |snapshot| {
            *snapshot_for_subscription.borrow_mut() = Some(snapshot.clone());
            subscription_scheduler.schedule(snapshot);
        });

        let search = options.searchable.then(BubbleMenuSearch::new);
        let document_subscription = search.as_ref().map(|search| {
            let search = search.clone();
            editor.on_update_steps::<Doc, _>(move |_doc, steps| search.map_steps(&steps))
        });

        let mut listeners = Vec::new();
        {
            let pointer = pointer_active.clone();
            let scheduler = scheduler.clone();
            let last = last_snapshot.clone();
            listeners.push(DomListener::new(
                &menu,
                "pointerdown",
                true,
                move |_event| {
                    pointer.set(true);
                    schedule_last(&scheduler, &last);
                },
            ));
        }
        if let Some(document) = menu.owner_document() {
            for event_name in ["pointerup", "pointercancel"] {
                let pointer = pointer_active.clone();
                let scheduler = scheduler.clone();
                let last = last_snapshot.clone();
                listeners.push(DomListener::new(
                    &document,
                    event_name,
                    true,
                    move |_event| {
                        pointer.set(false);
                        schedule_last(&scheduler, &last);
                    },
                ));
            }
            {
                let scheduler = scheduler.clone();
                let last = last_snapshot.clone();
                // Scroll does not bubble, so capture at the document boundary.
                // One listener covers the editor and every scrollable ancestor.
                listeners.push(DomListener::new(&document, "scroll", true, move |_event| {
                    schedule_last(&scheduler, &last)
                }));
            }
            if let Some(window) = document.default_view() {
                let scheduler = scheduler.clone();
                let last = last_snapshot.clone();
                listeners.push(DomListener::new(&window, "resize", false, move |_event| {
                    schedule_last(&scheduler, &last)
                }));
            }
        }
        for event_name in ["focusin", "focusout"] {
            let scheduler = scheduler.clone();
            let last = last_snapshot.clone();
            listeners.push(DomListener::new(&menu, event_name, true, move |_event| {
                schedule_last(&scheduler, &last)
            }));
        }
        {
            let presenter = presenter.clone();
            let dismissed = dismissed_selection.clone();
            let last = last_snapshot.clone();
            let editor_for_escape = editor.clone();
            let escape_refocus = options.escape_refocus;
            listeners.push(DomListener::new(&menu, "keydown", true, move |event| {
                let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
                    return;
                };
                if key.key() != "Escape" {
                    return;
                }
                key.prevent_default();
                key.stop_propagation();
                *dismissed.borrow_mut() = last
                    .borrow()
                    .as_ref()
                    .map(|snapshot| snapshot.selection.clone());
                presenter.hide();
                if escape_refocus {
                    focus_editor(&editor_for_escape);
                }
            }));
        }

        let controller = Self {
            key,
            editor,
            presenter,
            scheduler,
            last_snapshot,
            dismissed_selection,
            selection_subscription: Some(selection_subscription),
            document_subscription,
            listeners,
            search,
        };
        controller.refresh()?;
        Ok(controller)
    }

    pub fn key(&self) -> &BubbleMenuKey {
        &self.key
    }

    pub fn state(&self) -> BubbleMenuState {
        self.presenter.state.borrow().clone()
    }

    /// Re-read the editor snapshot and queue one coalesced position update.
    pub fn refresh(&self) -> Result<(), BubbleMenuError> {
        let snapshot = self
            .editor
            .selection_snapshot()
            .map_err(|_| BubbleMenuError::SurfaceUnavailable)?;
        *self.last_snapshot.borrow_mut() = Some(snapshot.clone());
        self.scheduler.schedule(snapshot);
        Ok(())
    }

    /// Hide until the model selection changes. This mirrors Escape dismissal
    /// and avoids immediately reopening on the editor's refocus event.
    pub fn dismiss(&self, refocus_editor: bool) {
        *self.dismissed_selection.borrow_mut() = self
            .last_snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.selection.clone());
        self.presenter.hide();
        if refocus_editor {
            focus_editor(&self.editor);
        }
    }

    pub fn search(&self) -> Option<&BubbleMenuSearch> {
        self.search.as_ref()
    }

    pub fn begin_search(
        &self,
        query: impl Into<String>,
    ) -> Result<SearchRequestToken, BubbleMenuError> {
        let search = self
            .search
            .as_ref()
            .ok_or(BubbleMenuError::SearchDisabled)?;
        let selection = self
            .last_snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.selection.clone())
            .or_else(|| self.editor.selection_snapshot().ok().map(|s| s.selection))
            .ok_or(BubbleMenuError::SurfaceUnavailable)?;
        Ok(search.begin(query, &selection))
    }
}

impl Drop for BubbleMenuController {
    fn drop(&mut self) {
        self.scheduler.cancel();
        self.selection_subscription.take();
        self.document_subscription.take();
        self.listeners.clear();
        if let Some(search) = &self.search {
            search.cancel();
        }
        // A component-owned callback usually writes back through `Handle`.
        // Teardown may already hold that component's mutable lifecycle borrow,
        // so the final DOM hide must not re-enter component state.
        self.presenter.notify.set(false);
        self.presenter.hide();
    }
}

fn schedule_last(scheduler: &FrameScheduler, last_snapshot: &RefCell<Option<SelectionSnapshot>>) {
    if let Some(snapshot) = last_snapshot.borrow().clone() {
        scheduler.schedule(snapshot);
    }
}

fn viewport_size() -> BubbleViewport {
    let Some(window) = web_sys::window() else {
        return BubbleViewport::default();
    };
    BubbleViewport {
        width: window
            .inner_width()
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0),
        height: window
            .inner_height()
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0),
    }
}

fn ensure_class(element: &Element, class: &str) {
    let current = element.get_attribute("class").unwrap_or_default();
    if current.split_ascii_whitespace().any(|item| item == class) {
        return;
    }
    let next = if current.trim().is_empty() {
        class.to_string()
    } else {
        format!("{} {class}", current.trim())
    };
    let _ = element.set_attribute("class", &next);
}

fn menu_contains_focus(menu: &Element) -> bool {
    menu.owner_document()
        .and_then(|document| document.active_element())
        .is_some_and(|active| {
            menu.is_same_node(Some(&active)) || menu.contains(Some(active.as_ref()))
        })
}

fn focus_editor(editor: &Editor) {
    let surface = if editor.element().matches(".pine-rich-text").unwrap_or(false) {
        Some(editor.element().clone())
    } else {
        editor
            .element()
            .query_selector(".pine-rich-text")
            .ok()
            .flatten()
    };
    if let Some(surface) = surface.and_then(|element| element.dyn_into::<HtmlElement>().ok()) {
        let _ = surface.focus();
    }
}

thread_local! {
    static COMPONENT_CONTROLLERS: RefCell<HashMap<ScopeId, BubbleMenuController>> =
        RefCell::new(HashMap::new());
}

/// Declarative Pocopine shell for [`BubbleMenuController`].
///
/// All visual values have stable hooks: `.pine-richtext-bubble-menu`,
/// `data-state`, `data-placement`, `data-align`, `data-anchor-kind`, and the
/// `--pine-richtext-bubble-menu-*` custom properties in the bundled stylesheet.
#[derive(Serialize, Deserialize)]
#[component(
    template = "PineRichTextBubbleMenu.poco",
    style = "bubble_menu.css",
    role = "panel"
)]
#[slot(default)]
pub struct PineRichTextBubbleMenu {
    /// CSS selector for the rich-text host. Empty searches the nearest ancestor
    /// subtree, which supports an editor and menu placed as siblings.
    #[prop]
    pub editor: String,
    #[prop]
    pub placement: String,
    #[prop]
    pub align: String,
    #[prop]
    pub offset: f64,
    #[prop]
    pub viewport_padding: f64,
    #[prop]
    pub flip: bool,
    #[prop]
    pub debounce_ms: f64,
    #[prop]
    pub show_on_empty: bool,
    #[prop]
    pub show_when_read_only: bool,
    #[prop]
    pub escape_refocus: bool,
    #[prop]
    pub searchable: bool,
    pub open: bool,
    pub x: f64,
    pub y: f64,
    pub resolved_placement: String,
    pub anchor_kind: String,
    pub plugin_key: String,
    pub error: String,
}

impl Default for PineRichTextBubbleMenu {
    fn default() -> Self {
        Self {
            editor: String::new(),
            placement: "top".into(),
            align: "center".into(),
            offset: 8.0,
            viewport_padding: 8.0,
            flip: true,
            debounce_ms: 0.0,
            show_on_empty: false,
            show_when_read_only: false,
            escape_refocus: true,
            searchable: false,
            open: false,
            x: 0.0,
            y: 0.0,
            resolved_placement: "top".into(),
            anchor_kind: "text".into(),
            plugin_key: String::new(),
            error: String::new(),
        }
    }
}

#[handlers]
impl PineRichTextBubbleMenu {
    fn on_ready(&self, refs: pocopine::Refs, handle: Handle<Self>, scope: ScopeId) {
        let Some(menu) = refs.get("menu") else {
            return;
        };
        let Some(editor) = resolve_editor(&menu, &self.editor) else {
            let selector = if self.editor.trim().is_empty() {
                "nearest ancestor editor".to_string()
            } else {
                self.editor.clone()
            };
            let _ = menu.set_attribute("data-error", "editor-not-found");
            handle.defer_update(move |state| {
                state.error = BubbleMenuError::EditorNotFound(selector).to_string();
            });
            return;
        };

        let options = BubbleMenuOptions {
            placement: BubbleMenuPlacement::parse(&self.placement),
            align: BubbleMenuAlign::parse(&self.align),
            offset: self.offset,
            viewport_padding: self.viewport_padding,
            flip: self.flip,
            debounce_ms: self.debounce_ms,
            show_on_empty: self.show_on_empty,
            show_when_read_only: self.show_when_read_only,
            escape_refocus: self.escape_refocus,
            searchable: self.searchable,
            should_show: None,
        };
        let state_handle = handle.clone();
        match BubbleMenuController::attach_with_state(editor, menu.clone(), options, move |next| {
            state_handle.defer_update(move |state| {
                state.open = next.visible;
                state.x = next.x;
                state.y = next.y;
                state.resolved_placement = next.placement.as_str().into();
                state.anchor_kind = next.anchor_kind.as_str().into();
            });
        }) {
            Ok(controller) => {
                let key = controller.key().to_string();
                handle.defer_update(|state| state.plugin_key = key);
                COMPONENT_CONTROLLERS.with(|controllers| {
                    controllers.borrow_mut().insert(scope, controller);
                });
            }
            Err(error) => {
                let _ = menu.set_attribute("data-error", "surface-unavailable");
                handle.defer_update(move |state| state.error = error.to_string());
            }
        }
    }

    fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            COMPONENT_CONTROLLERS.with(|controllers| {
                controllers.borrow_mut().remove(&scope);
            });
        }
    }
}

fn resolve_editor(menu: &Element, selector: &str) -> Option<Editor> {
    let selector = selector.trim();
    if !selector.is_empty() {
        let candidate = menu.owner_document()?.query_selector(selector).ok()??;
        if candidate.matches("pine-rich-text-root").unwrap_or(false) {
            return Some(Editor::from_element(candidate));
        }
        return Editor::find(&candidate);
    }

    let mut ancestor = menu.parent_element();
    while let Some(element) = ancestor {
        if element.matches("pine-rich-text-root").unwrap_or(false) {
            return Some(Editor::from_element(element));
        }
        if let Some(editor) = Editor::find(&element) {
            return Some(editor);
        }
        ancestor = element.parent_element();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::transform::StepMap;

    fn rect(left: f64, top: f64, width: f64, height: f64) -> ViewportRect {
        ViewportRect {
            x: left,
            y: top,
            width,
            height,
            top,
            right: left + width,
            bottom: top + height,
            left,
        }
    }

    #[test]
    fn menu_keys_never_collide() {
        let first = BubbleMenuKey::new();
        let second = BubbleMenuKey::new();
        assert_ne!(first, second);
        assert_eq!(first, first.clone());
    }

    #[test]
    fn preferred_top_flips_and_clamps_to_viewport() {
        let options = BubbleMenuOptions::default();
        let position = compute_position(
            rect(2.0, 3.0, 40.0, 20.0),
            BubbleMenuSize {
                width: 80.0,
                height: 30.0,
            },
            BubbleViewport {
                width: 200.0,
                height: 120.0,
            },
            &options,
        );
        assert_eq!(position.placement, BubbleMenuPlacement::Bottom);
        assert_eq!(position.x, 8.0);
        assert_eq!(position.y, 31.0);
    }

    #[test]
    fn end_alignment_uses_anchor_edge() {
        let options = BubbleMenuOptions {
            placement: BubbleMenuPlacement::Bottom,
            align: BubbleMenuAlign::End,
            flip: false,
            ..BubbleMenuOptions::default()
        };
        let position = compute_position(
            rect(100.0, 80.0, 50.0, 20.0),
            BubbleMenuSize {
                width: 30.0,
                height: 10.0,
            },
            BubbleViewport {
                width: 300.0,
                height: 200.0,
            },
            &options,
        );
        assert_eq!(position.x, 120.0);
        assert_eq!(position.y, 108.0);
    }

    #[test]
    fn search_rejects_old_results_and_maps_current_bookmark() {
        let search = BubbleMenuSearch::new();
        let old = search.begin("o", &Selection::text(5));
        let current = search.begin("open", &Selection::text_between(5, 8));
        assert!(search.finish(old, "old-result").is_err());

        let mut mapping = Mapping::new();
        mapping.append_map(StepMap::single(0, 0, 2));
        search.map(&mapping);
        let result = search.finish(current, "open-result").unwrap();
        assert_eq!(result.value, "open-result");
        assert_eq!(
            result.selection,
            SelectionBookmark::Text {
                anchor: 7,
                head: 10,
            }
        );
    }

    #[test]
    fn accepted_search_token_cannot_be_delivered_twice() {
        let search = BubbleMenuSearch::new();
        let token = search.begin("tag", &Selection::text(3));
        assert!(search.finish(token.clone(), 1).is_ok());
        assert!(search.finish(token, 2).is_err());
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use js_sys::Promise;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{CustomEvent, CustomEventInit, Document, KeyboardEventInit};

    wasm_bindgen_test_configure!(run_in_browser);

    struct FakeEditor {
        host: Element,
        snapshot: Rc<RefCell<SelectionSnapshot>>,
        _request: Closure<dyn FnMut(Event)>,
    }

    impl FakeEditor {
        fn mount(document: &Document, snapshot: SelectionSnapshot) -> Self {
            let host = document.create_element("pine-rich-text-root").unwrap();
            let surface = document.create_element("div").unwrap();
            surface.set_class_name("pine-rich-text");
            surface.set_attribute("contenteditable", "true").unwrap();
            surface.set_text_content(Some("editor"));
            host.append_child(&surface).unwrap();
            document.body().unwrap().append_child(&host).unwrap();

            let snapshot = Rc::new(RefCell::new(snapshot));
            let snapshot_for_request = snapshot.clone();
            let host_for_request = host.clone();
            let request = Closure::wrap(Box::new(move |_event: Event| {
                let detail = serde_wasm_bindgen::to_value(&*snapshot_for_request.borrow()).unwrap();
                let init = CustomEventInit::new();
                init.set_detail(&detail);
                let response = CustomEvent::new_with_event_init_dict(
                    "pine:richtext:selection-snapshot-result",
                    &init,
                )
                .unwrap();
                host_for_request.dispatch_event(&response).unwrap();
            }) as Box<dyn FnMut(Event)>);
            host.add_event_listener_with_callback(
                "pine:richtext:selection-snapshot",
                request.as_ref().unchecked_ref(),
            )
            .unwrap();

            Self {
                host,
                snapshot,
                _request: request,
            }
        }

        fn editor(&self) -> Editor {
            Editor::from_element(self.host.clone())
        }
    }

    impl Drop for FakeEditor {
        fn drop(&mut self) {
            self.host.remove();
        }
    }

    fn snapshot(selection: Selection, empty: bool) -> SelectionSnapshot {
        SelectionSnapshot {
            selection,
            from: 2,
            to: if empty { 2 } else { 5 },
            empty,
            active_mark_names: vec!["strong".into()],
            enclosing_block_types: vec!["paragraph".into()],
            rect: Some(ViewportRect {
                x: 80.0,
                y: 80.0,
                width: 60.0,
                height: 20.0,
                top: 80.0,
                right: 140.0,
                bottom: 100.0,
                left: 80.0,
            }),
            focused: true,
            editable: true,
        }
    }

    fn menu(document: &Document) -> Element {
        let menu = document.create_element("div").unwrap();
        menu.set_class_name(BUBBLE_MENU_CLASS);
        let button = document.create_element("button").unwrap();
        button.set_attribute("type", "button").unwrap();
        button.set_text_content(Some("Bold"));
        menu.append_child(button.as_ref()).unwrap();
        menu.set_attribute("style", "width: 80px; height: 30px;")
            .unwrap();
        document.body().unwrap().append_child(&menu).unwrap();
        menu
    }

    #[wasm_bindgen_test(async)]
    async fn controller_tracks_cell_geometry_and_cleans_up() {
        let document = web_sys::window().unwrap().document().unwrap();
        let fake = FakeEditor::mount(&document, snapshot(Selection::cells(3, 11), false));
        let menu = menu(&document);
        let states = Rc::new(Cell::new(0));
        let states_for_callback = states.clone();
        let controller = BubbleMenuController::attach_with_state(
            fake.editor(),
            menu.clone(),
            BubbleMenuOptions::default(),
            move |_| states_for_callback.set(states_for_callback.get() + 1),
        )
        .unwrap();

        // Several reads before the frame still publish only the latest state.
        controller.refresh().unwrap();
        controller.refresh().unwrap();
        next_frame().await;
        assert!(controller.state().visible);
        assert_eq!(
            menu.get_attribute("data-anchor-kind").as_deref(),
            Some("cells")
        );
        assert_eq!(states.get(), 1);

        drop(controller);
        assert!(menu.has_attribute("hidden"));
        assert_eq!(menu.get_attribute("data-state").as_deref(), Some("closed"));
        menu.remove();
    }

    #[wasm_bindgen_test(async)]
    async fn menu_focus_survives_editor_blur_then_escape_stays_dismissed() {
        let document = web_sys::window().unwrap().document().unwrap();
        let fake = FakeEditor::mount(&document, snapshot(Selection::text_between(2, 5), false));
        let menu = menu(&document);
        let controller =
            BubbleMenuController::attach(fake.editor(), menu.clone(), BubbleMenuOptions::default())
                .unwrap();
        next_frame().await;
        assert!(controller.state().visible);

        fake.snapshot.borrow_mut().focused = false;
        let button = menu
            .query_selector("button")
            .unwrap()
            .unwrap()
            .dyn_into::<HtmlElement>()
            .unwrap();
        button.focus().unwrap();
        controller.refresh().unwrap();
        next_frame().await;
        assert!(
            controller.state().visible,
            "focus inside menu keeps it open"
        );

        let init = KeyboardEventInit::new();
        init.set_key("Escape");
        let escape = KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init).unwrap();
        menu.dispatch_event(&escape).unwrap();
        assert!(!controller.state().visible);
        controller.refresh().unwrap();
        next_frame().await;
        assert!(
            !controller.state().visible,
            "refocus events must not reopen a dismissed unchanged selection"
        );

        drop(controller);
        menu.remove();
    }

    #[wasm_bindgen_test(async)]
    async fn pointer_capture_keeps_controls_alive_until_activation() {
        let document = web_sys::window().unwrap().document().unwrap();
        let fake = FakeEditor::mount(&document, snapshot(Selection::text_between(2, 5), false));
        let menu = menu(&document);
        let controller =
            BubbleMenuController::attach(fake.editor(), menu.clone(), BubbleMenuOptions::default())
                .unwrap();
        next_frame().await;

        fake.snapshot.borrow_mut().focused = false;
        menu.dispatch_event(&Event::new("pointerdown").unwrap())
            .unwrap();
        controller.refresh().unwrap();
        next_frame().await;
        assert!(controller.state().visible);

        document
            .dispatch_event(&Event::new("pointerup").unwrap())
            .unwrap();
        controller.refresh().unwrap();
        next_frame().await;
        assert!(!controller.state().visible);

        drop(controller);
        menu.remove();
    }

    async fn next_frame() {
        let promise = Promise::new(&mut |resolve, _reject| {
            let callback = Closure::once_into_js(move |_timestamp: f64| {
                let _ = resolve.call0(&JsValue::NULL);
            });
            web_sys::window()
                .unwrap()
                .request_animation_frame(callback.unchecked_ref())
                .unwrap();
        });
        JsFuture::from(promise).await.unwrap();
    }
}
