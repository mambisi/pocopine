# RFC-113: Typed node views and external blocks for `pine-richtext`

| Field | Value |
|---|---|
| **Status** | Implemented (N0–N7) |
| **Author** | pocopine team |
| **Created** | 2026-07-12 |
| **Crates** | `pine-richtext`, `pine-richtext-macros`, `pine-richtext-extensions`, `pocopine-core`, `pocopine-macros`, `pine-richtext-collab` |
| **Builds on** | RFC-061 (owned subtree mounts), RFC-079 (tables), RFC-112 (typed dynamic components) |

## 1. Summary

Add a first-class, typed **node-view** lifecycle to `pine-richtext` so an
extension can render a semantic editor node with a Pocopine component while
the editor remains responsible for the document, selection, history,
serialization, collaboration, and editable descendants.

The durable document continues to store only semantic node data:

```json
{
  "type": "diagram",
  "version": 1,
  "attrs": { "diagram_id": "dg_42", "theme": "dark" },
  "leaf": true
}
```

The editor runtime separately maps the semantic node type to a component using
a typed constructor:

```rust
NodeViewSpec::atom_component::<DiagramNode, PineDiagramView>()
```

Application code never stores a component tag or `ComponentRef` in the
document. Adding, deleting, moving, or changing an external block is an editor
transaction. A per-editor `NodeViewManager` observes reconciler changes and
owns component mount, update, and deterministic teardown. The implemented
surface covers block views, editable table shells, and inline atom views such
as tags and chips.

This RFC deliberately does **not** use `<pp-component>` inside generated
rich-text HTML and does not allow a computed function to imperatively return a
list of mounted children. `<pp-component>` remains valid inside a node-view
component's own compiled `.poco` template.

## 2. Problem statement

Before this RFC, `pine-richtext` had two partial forms of custom block
rendering.

### 2.1 CSS-only unknown-node rendering

The generic renderer emitted an unknown node as:

```html
<span data-pos="N" data-type="custom-type">...</span>
```

Keep used this fallback for its `title` node and turned the span into a block
with CSS. `KeepTitleNode` now declares a native `h1` DOM view. The old fallback
was useful for presentation, but provided no component lifecycle, typed
attributes, commands, content-ownership boundary, or extension-owned DOM
shape.

### 2.2 The task-item component node view

`TaskListExtension::with_node_view::<PineTaskItem>()` was the conceptual
foundation. It derived the custom-element name from `C::NAME`, mounted the
component, and threaded model content into
`[data-pine-richtext-content]`.

That legacy implementation was incomplete:

- `C` was immediately erased to a raw tag `String`.
- The application had to call `App::register::<C>()` separately; forgetting it
  left an inert custom element.
- Node attributes were mirrored only as `data-*` attributes. Reconciler attr
  patches did not update component state.
- The component read `data-pos` and `data-checked`, emitted an application-owned
  `CustomEvent`, and relied on a parent listener to create a transaction.
- Every structural patch scanned the whole editor with `querySelectorAll` for
  every registered node-view tag.
- Reconciler removal and `innerHTML` replacement did not first release the
  Pocopine subtree. Component scopes, effects, refs, and listeners could
  outlive a deleted editor node.
- Component chrome and editor-owned editable content had no formal event or
  mutation boundary.

Those gaps were manageable for a checkbox. They were not safe enough for
tables, embeds, diagrams, attachments, polls, callouts, or third-party blocks.

### 2.3 Tables expose both halves of the problem

RFC-079 remains the table design record. Its semantic GFM table model is now
implemented in `pine-richtext-extensions`; tables need more than a
component-shaped DOM island:

- rows and cells must remain editor nodes;
- selection and keyboard navigation cross cell boundaries;
- row/column commands must participate in history and collaboration;
- copy/paste and Markdown/HTML serialization operate on semantic cells;
- optional resize handles, menus, and selection chrome are view concerns.

An opaque component can implement a spreadsheet widget, but it cannot become a
native rich-text table merely by mounting inside the editor.

## 3. Decision

Pine adopts the node-view split used by mature editor architectures:

1. The **document node** is immutable semantic data and the source of truth.
2. The **node-view manager** owns mounted view instances and reconciles them
   against document transactions.
3. The **component** owns interactive chrome and ephemeral UI state.
4. The **editor** exclusively owns any declared editable content hole.
5. **Serialization** is defined by the extension and never serializes the
   component's live DOM.

The mapping from semantic node type to component is typed when the runtime is
built, then type-erased behind an internal vtable. Erasure happens only after
the compiler has proved `N: RichTextNodeType`,
`C: RichTextNodeView<N>`, and that `C` has the macro-emitted typed mount ABI.
The semantic type `N`, not the UI component, owns the persisted attribute
schema, version, defaults, and migrations.

## 4. Goals

- Let third-party crates add component-backed block and inline-atom node views.
- Keep component selection type-checked; no user-authored component-name
  strings for Pocopine node views.
- Mount each view once, update it without remounting, and unmount it exactly
  once.
- Make add/remove/update commands editor transactions so undo/redo,
  collaboration, selection mapping, and serialization stay correct.
- Support atomic views and component chrome around editor-owned content.
- Automatically register typed node-view components.
- Replace full-editor node-view scans with a synchronous reconciler lifecycle
  sink.
- Give component handlers a typed, live editor handle instead of `data-pos`
  parsing and ad-hoc `CustomEvent` glue.
- Provide an extension-owned, validated native DOM specification for custom
  nodes that do not need a component.
- Make missing extensions, invalid attrs, mount failures, and missing content
  holes loud and actionable.
- Give semantic tables a sound component/chrome, selection, and DOM-rendering
  foundation.

## 5. Non-goals

- Persisting component tags, `ComponentRef`s, callbacks, DOM, or mount handles
  in a document.
- Mounting arbitrary components named by untrusted document data.
- Letting computed functions mount/unmount editor children as side effects.
- Async code loading or remote plugin discovery.
- Serializing the in-editor component DOM as HTML or Markdown.
- Making an opaque data-grid component behave like a native rich-text table.
- Supporting multiple editable content holes in one node view in v1.
- Editable inline component views with editor-owned content holes. Inline atom
  views are supported; editable inline cursor boundaries, mark inheritance,
  and IME ownership remain out of scope.
- Preserving a component instance across every possible structural move. V1
  preserves it whenever reconciliation retains the same DOM host; a future
  keyed-node identity contract may extend this.

## 6. Terminology and ownership

- **Semantic node** — `pine_richtext::model::Node`, including its type, attrs,
  marks, and children.
- **Node view** — the in-editor presentation and interaction object for one
  semantic node.
- **Chrome** — component-owned UI such as buttons, handles, toolbars, menus,
  status, or previews.
- **Content hole** — the single element where Pine renders and reconciles the
  semantic node's children.
- **Atom** — a node view with no editor-owned children; selected and deleted as
  one unit.
- **Native DOM view** — a validated DOM output description supplied by an
  extension without a Pocopine component.

| Concern | Owner |
|---|---|
| Node type, attrs, marks, children | document/schema |
| Insert, replace, move, delete | editor transaction |
| Selection and position mapping | editor state/view |
| Undo/redo and collaboration | transaction/plugin layers |
| Component mount/update/unmount | `NodeViewManager` |
| Buttons, handles, menus, local hover/open state | component |
| Editable descendants | Pine inside the content hole |
| JSON/Markdown/HTML output | extension serializers |

Component-local state is intentionally ephemeral. If a value must survive
undo, reload, collaboration, or a remount, the component writes it to node
attrs through a transaction.

## 7. Public API

### 7.1 The semantic node owns the wire contract

The component is a replaceable view, so it must not define persisted attrs.
A target-independent semantic descriptor owns the name, attrs, schema version,
defaults, and migrations:

```rust
pub trait RichTextNodeAttrs: serde::Serialize
    + serde::de::DeserializeOwned
    + Clone
    + PartialEq
    + Send
    + Sync
    + 'static
{
    /// Closed serde wire-key set, including serde renames.
    const KEYS: &'static [&'static str];
}

pub trait RichTextNodeType: Send + Sync + 'static {
    const NAME: &'static str;
    const VERSION: u32;
    type Attrs: RichTextNodeAttrs;

    fn spec() -> NodeSpec;
    fn migrations() -> &'static [NodeMigration] { &[] }
}

pub struct NodeMigration {
    pub from: u32,
    pub to: u32,
    pub apply: fn(WireNode) -> Result<WireNode, NodeMigrationError>,
}
```

For example:

```rust
#[derive(Clone, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct DiagramAttrs {
    pub diagram_id: String,
    #[serde(default = "default_theme")]
    pub theme: String,
}

pub struct DiagramNode;

impl RichTextNodeType for DiagramNode {
    const NAME: &'static str = "diagram";
    const VERSION: u32 = 1;
    type Attrs = DiagramAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .required_attr("diagram_id")
            .attr("theme", json!("light"))
    }
}
```

The new target-independent `pine-richtext-macros` proc-macro crate provides
`#[derive(RichTextNodeAttrs)]`, re-exported by `pine-richtext`, and generates
`KEYS` for a named-field struct using `rename`/`rename_all` rules. V1 does not
support manual implementations and rejects serde features that make the
accepted and emitted maps asymmetric or unknowable: `flatten`, `alias`, field
`skip*`, `skip_serializing_if`, custom `serialize_with`/`deserialize_with`,
container `from`/`try_from`/`into`, transparent forms, and enums. `default` and
renames remain supported; aliases belong in explicit migrations.

Before serde decoding, Pine rejects a same-version wire attr not present in
`KEYS`. After every serialization used for an update, it revalidates that the
output is an object containing only declared keys before diff/merge. Older data
must carry its earlier version and pass through an explicit migration, while
newer unsupported data is rejected intact. This prevents typed mutation from
silently discarding attrs even if serde behavior changes.

`RichTextExtension` gains a source-compatible, target-independent method:

```rust
fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
    vec![TypedNodeSpec::of::<DiagramNode>()]
}
```

The existing `nodes() -> Vec<NodeSpec>` remains for built-ins and migration.
A component-backed external node must be contributed through `typed_nodes`.
`RuntimeBuilder` validates `N::NAME`, `N::VERSION >= 1`, exact agreement
between `N::Attrs::KEYS` and `N::spec()` attrs, decodable defaults, and exactly
one `from -> from + 1` migration for every `from` in `1..N::VERSION`. This
requires exposing read-only `NodeSpec` attr metadata and fixing schema
materialization so required attrs, defaults, and typed decode errors are
enforced before a node enters editor state.

The erased typed-node entry retains `TypeId::of::<N>()`. A second Rust marker
with the same `NAME` is a duplicate-definition error even if its `NodeSpec`
happens to look identical, and a node-view registration must match the exact
semantic `TypeId` folded into the runtime. Matching strings alone never prove
the association.

### 7.2 Feature-gated typed view registration

The model, transforms, and collaboration crates remain host-capable without
Pocopine or `web-sys`. The target-independent `RichTextExtension` does not gain
a component-returning method. Under Pine's `view` feature, a separate trait is
available:

```rust
pub trait RichTextViewExtension: RichTextExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        Vec::new()
    }
}

impl RichTextViewExtension for DiagramExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        vec![
            NodeViewSpec::editable_component::<TaskItemNode, PineTaskItem>(),
            NodeViewSpec::atom_component::<DiagramNode, PineDiagramView>(),
        ]
    }
}
```

`RuntimeBuilder::with_view(E)` folds both contracts from one shared `Arc<E>`
where `E: RichTextExtension + RichTextViewExtension`; model-only code continues
to use `with(E)`. An adapter crate that supports both modes must
wire its own feature explicitly, for example
`view = ["pine-richtext/view"]`, and gate its view module/impl on **that local
feature**. It must not assume `#[cfg(feature = "view")]` observes a dependency's
feature.

These constructors require:

```rust
N: RichTextNodeType,
C: RichTextNodeView<N> + MountableComponent,
```

The editable constructor additionally requires the macro-emitted
`OwnedContentOutletComponent` metadata contract from `pocopine-core`, emitted by
`pocopine-macros` when the component template contains exactly one
`pp-owned-content` outlet. The builder stores function pointers
for fallible registration, pre-setup initialization, typed updates, and
one-shot teardown. It may retain `N::NAME` and `C::NAME` internally, but the
author provides neither as a string.

The raw `node_views() -> Vec<ExtensionNodeView>` and
`TaskListExtension::{with_node_view,with_node_view_tag}` APIs are removed in
this change. There is no compatibility window: every in-repository caller
migrates to the typed lane before it lands, leaving one ownership and failure
model to test.

`RuntimeBuilder` adds:

```rust
pub fn try_build(self) -> Result<Arc<EditorRuntime>, RuntimeBuildError>;
```

`build()` remains an `expect`-style convenience wrapper.
`try_build` validates one effective view per semantic type, atom/content
compatibility, the typed node association, and component/outlet metadata.
Typed component registration is idempotent across runtimes. The core mount ABI
must expose a fallible registration result so a global component-name
collision is reported even when a lazy default runtime is finalized after
`App::run`; it must not rely only on the earlier app boot preflight.

### 7.3 Component contract and snapshots

```rust
pub trait RichTextNodeView<N: RichTextNodeType>: MountableComponent {
    fn sync_node(
        &mut self,
        update: NodeViewUpdate<N::Attrs>,
    ) -> Result<(), NodeViewError>;
}

pub struct NodeViewUpdate<A> {
    pub attrs: A,
    pub marks: Vec<Mark>,
    pub content: Fragment,
    pub selection: NodeViewSelection,
    pub editable: bool,
    pub editor_focused: bool,
}

pub enum NodeViewSelection {
    Outside,
    Node,
    CursorInside,
    NodeContainsRange,
    RangeContainsNode,
    CrossesBoundary,
    Cells {
        anchor_cell: usize,
        head_cell: usize,
    },
}
```

`Node` means an exact node selection; `CursorInside` is a collapsed caret
inside an editable node. `NodeContainsRange` means both endpoints of a
non-collapsed text selection are inside the node;
`RangeContainsNode` means the range fully covers it; `CrossesBoundary` means
only one side intersects. `Cells` carries the endpoints of a validated,
same-table rectangular cell selection. The variants are mutually exclusive.

The view maps the typed snapshot into ordinary reactive component state; it is
not initialized by string `data-*` props:

```rust
impl RichTextNodeView<TaskItemNode> for PineTaskItem {
    fn sync_node(
        &mut self,
        update: NodeViewUpdate<TaskItemAttrs>,
    ) -> Result<(), NodeViewError> {
        self.checked = update.attrs.checked;
        self.selection = update.selection;
        Ok(())
    }
}
```

`Fragment` clones are cheap because storage is shared. Passing content and
marks lets table chrome react to row count or other semantic changes without
giving the component mutation ownership. `Cells` lets table chrome react to
rectangular selection; a single `selected: bool` is insufficient.

The first `sync_node` runs through a pre-setup initializer, so `on_setup` and
`on_mount` observe the real node state rather than `Default` placeholders.
Later attrs, marks, content, selection, editor focus, or editability changes
call it through `Handle<C>::update` without remounting. Recoverable hook errors
are reported and isolated; this RFC does not promise to contain arbitrary Rust
panics on wasm.

The component never receives or caches a numeric document position in its
snapshot.

### 7.4 Typed, live node handles

The handle is parameterized by the semantic node, not the renderer. This lets
the root view and typed child toolbars use the same capability through
Pocopine context:

```rust
pub trait FromHandlerContext: Sized {
    fn from_handler_context(
        context: &HandlerContext,
    ) -> Result<Self, HandlerExtractError>;
}
```

`#[handlers]` gains an explicit `#[context]` parameter lane. Such a parameter
does not consume a JavaScript event-argument slot; the macro resolves it from
the current component scope through `FromHandlerContext`. Pine implements that
trait for `NodeViewHandle<N>` by reading the manager-provided typed context.
Using it outside the matching node-view scope reports a structured diagnostic
naming the component, handler, requested `N`, and current scope; it never
silently skips the handler as a failed `FromHandlerArg` conversion.

```rust
#[handlers]
impl PineTaskItem {
    pub fn toggle(
        &mut self,
        #[context] node: NodeViewHandle<TaskItemNode>,
    ) {
        if let Err(error) = node.update_attrs(|attrs| {
            attrs.checked = !attrs.checked;
        }) {
            tracing::warn!(%error, "task-item update failed");
        }
    }

    pub fn remove(
        &mut self,
        #[context] node: NodeViewHandle<TaskItemNode>,
    ) {
        if let Err(error) = node.delete() {
            tracing::warn!(%error, "task-item delete failed");
        }
    }
}
```

The core surface is:

```rust
pub struct NodeViewHandle<N: RichTextNodeType> { /* weak binding */ }

impl<N: RichTextNodeType> NodeViewHandle<N> {
    pub fn position(&self) -> Result<usize, NodeViewError>;
    pub fn attrs(&self) -> Result<N::Attrs, NodeViewError>;

    pub fn update_attrs(
        &self,
        update: impl FnOnce(&mut N::Attrs),
    ) -> Result<(), NodeViewError>;

    pub fn delete(&self) -> Result<(), NodeViewError>;
    pub fn select_node(&self) -> Result<(), NodeViewError>;
    pub fn focus_editor(&self) -> Result<(), NodeViewError>;
    pub fn dispatch<K: NodeCommand<N>>(&self, command: K)
        -> Result<(), NodeViewError>;
}
```

`NodeCommand<N>` receives a freshly resolved, type-checked node target and
builds a normal editor transaction; it cannot accidentally act on whichever
selection happens to be current. Table operations such as `AddRow` implement
this trait. The basic helpers are commands over the same anchored transaction
surface.

The handle stores a weak editor binding plus the node-view host and generation.
Every operation resolves the host's **current** position and proves that the
node is still `N`. After deletion or editor teardown it returns
`NodeViewError::Stale`; it never becomes a silent no-op or target a new node at
the old position.

Attribute updates serialize before/after `N::Attrs`, diff only declared keys,
and patch those keys into the existing attr map. Because the typed node owns
the complete same-version schema and unknown attrs are rejected at load, no
wire state is lost.

There is one critical reentrancy rule: a component handler or lifecycle/watch
callback may already hold `&mut C`, so a self-update must not synchronously call
`sync_node(&mut C)`, and self-delete must not destroy `C` before that borrow
returns. The editor commits the document transaction immediately, but defers
the **entire view reconciliation/DOM patch** until Pocopine's outermost mutable
component-callback frame unwinds. During that later pass, `WillRemove` is still
delivered synchronously immediately before DOM removal. Multiple queued state
changes coalesce against the latest state. Calls made with no active component
borrow may reconcile immediately. The same guard covers initial `sync_node`,
setup, mount, update, watch, and unmount callbacks, so a command dispatched by
any of them cannot re-enter or destroy the borrowed component. This is a
framework safe point, not an application `setTimeout` convention.

`pocopine-core` implements this with one RAII `ComponentCallbackFrame`.
`Scope::invoke`, `Handle::update`, lifecycle hooks, and watch callbacks all
enter it. Each call site scopes and drops its `RefMut` before the outermost
frame guard is dropped; only that outer drop drains queued view work. The
primitive is generic Pocopine infrastructure, not a Pine-only thread-local
hidden in one event path. The drain is FIFO and continues until stable; work
queued by a callback during the drain appends to the same safe-point queue
without recursively borrowing that component.

### 7.5 Atom and editable content ownership

An atom component owns all descendant DOM. Pine stamps its host
`contenteditable="false"` and treats it as one selectable/deletable unit. This
fits block embeds, diagrams, attachments, polls, opaque data grids, and inline
atoms such as tags and chips.

An editable component owns the shell while Pine exclusively owns one content
outlet. The component template marks that element explicitly:

```html
<root class="pine-task-item">
  <button type="button" @click="toggle">Done</button>
  <div pp-owned-content></div>
</root>
```

`pp-owned-content` is a generic Pocopine compile-time marker, not a selector
evaluated after mount. The component macro emits a stable template path plus
the `OwnedContentOutletComponent` metadata contract from `pocopine-core`. It
requires exactly one unconditional native element and rejects outlets under
`pp-if`, `pp-for`, slots, teleports, dynamic components, or another structural
branch where the compiled node path is not always present. The editable
constructor requires the metadata contract; runtime
construction rejects an atom view that contains an outlet.

The contract contains only generic owned-outlet path metadata.
`pocopine-core` and `pocopine-macros` do not depend on `pine-richtext`; Pine is
one consumer of the generic mount contract.

An editable shell remains inside the editor's existing editing host; Pine does
**not** stamp the whole shell `contenteditable="false"` and then create a nested
editing host. The component macro identifies the stable ancestor path to the
outlet. Pine marks only maximal chrome branches off that path non-editable,
while the path and outlet inherit the surface's editability. This preserves
cross-boundary selection, cursor motion, and composition. Read-only mode makes
the whole surface non-editable. Normal Pocopine components, including a
compiled `<pp-component>`, remain valid in chrome outside the outlet.

Component views use a structural `NodeViewHost` policy, never a raw tag
string. `Flow` (the default) selects `div`/`span` from the semantic inline flag
and lets the component render richer nested chrome; the table view uses this so
its toolbar and nested `<table>` remain valid. `Native` reuses the registered
`NodeDomSpec` root tag and projected root attrs; context-sensitive task items
opt into it so the editor produces `ul > li`, not `ul > div`. Runtime building
rejects `Native` when the semantic node has no native DOM spec.

Initial construction order is normative:

```text
create an empty stable node-view host
-> mount the component shell
-> resolve the compiled content-outlet path
-> render semantic children directly into that outlet
-> reconcile future children only inside that outlet
```

Children are never serialized into an unmounted custom tag and later captured
as slots. This avoids browser parser reparenting and is required for table
rows. Removal reverses ownership: release the component receipt before
removing its host.

Replacing, moving, or clearing the outlet from component code is an invariant
error. A recoverable mount/update error rolls back the partial component and
renders a semantic fallback in both development and production. An editable
fallback preserves and exposes the editor-owned children rather than blanking
the node.

When an extension does not declare a `NodeDomSpec`, the generic fallback is a
safe native element naming `N::NAME`; for editable nodes it also renders all
semantic children. It never reflects arbitrary attrs or raw HTML. A declared
native view plus plain-text and accessible policies supplies the richer
fallback implemented in N4.

### 7.6 No untyped component escape hatch in v1

V1 deliberately has no raw custom-element tag adapter. A browser Web Component
can be wrapped by a typed Pocopine node-view component, preserving the semantic
`N` association, mount failure reporting, event boundary, and transaction
handle. A future atom-only adapter requires its own explicit security and
lifecycle contract; it will not reuse the removed legacy tag map.

## 8. Required Pocopine mount primitive

The manager needs a typed, owned mount result rather than the current
fire-and-forget `mount_child_component` call.

```rust
pub fn mount_subtree_with<C, F>(
    host: &web_sys::Element,
    initialize: F,
) -> Result<MountedSubtree<C>, MountError>
where
    C: MountableComponent,
    F: FnOnce(&mut C, &mut MountSetup) -> Result<(), MountInitError>;

pub struct MountedSubtree<C: MountableComponent> {
    // Owns deterministic release and exposes a typed Handle<C>.
}

impl<C: MountableComponent> MountedSubtree<C> {
    pub fn handle(&self) -> Handle<C>;
    pub fn is_active(&self) -> bool;
    pub fn unmount(self);
}
```

`Component` alone is not a sufficient bound: a manual impl can name/register a
constructor whose scope state is not actually `C`. `MountableComponent` is a
`#[doc(hidden)]`, macro-emitted metadata contract combining `Component`,
`ComponentState`, the typed constructor, and a fallible idempotent registration
entry point. A downstream proc-macro expansion cannot provide a truly sealed
trait, so manual implementations are unsupported rather than assumed
impossible. Every mount normatively verifies the created scope `TypeId` and
downcast to `C`; mismatch is a `MountError`, never unchecked casting.

The initializer runs after instantiation and static-prop application but
before `on_setup`, with the new scope current for the entire callback.
`MountSetup::provide` installs typed context before lifecycle/template
mounting. The manager first provides the weak `NodeViewHandle<N>`, then invokes
the initial `sync_node`; nested chrome components subsequently inherit the same
anchored capability during template mounting. If registration, construction,
downcast, initialization, template mount, or lifecycle setup fails, the
partial scope is rolled back and the error names the component and stage. The
existing untyped `App::mount_subtree<C>() -> SubtreeHandle` stays
source-compatible; this is the stronger internal/tooling path.

`MountedSubtree<C>` owns a shared one-shot cleanup receipt installed on the
host/scope. Calling `unmount`, dropping the receipt, or recursively releasing
an ancestor may win cleanup, but exactly one path runs unmount hooks and frees
effects, listeners, refs, contexts, and DOM ownership. If ancestor teardown
wins, it disarms outstanding receipts so later manager cleanup is a no-op.
During normal editor operation the `NodeViewManager` is the receipt owner and
drains it before the host is removed.

Current `release_subtree_inner` walks children before the parent's unmount
hook, which is too late for an editor-root `on_unmount` to drain child views.
The receipt work therefore adds a generic one-shot **pre-descendant release
hook** on an owning element. `release_subtree_inner` claims/runs these hooks
before recursing into children. `PineRichTextRoot` registers one that drains its
manager. A mounted-subtree hook reached by generic ancestor traversal only
claims/disarms its receipt and lets that already-active traversal perform the
actual recursive cleanup; it must not recursively call
`release_compiled_subtree` on the same host. Explicit receipt unmount performs
the cleanup itself. Ordinary component unmount hooks keep their existing order.

This contract deliberately does **not** require calling
`release_compiled_subtree` on every `innerHTML` target. Releasing a content
root can accidentally tear down the editor or the component that owns the
outlet. Reconciliation releases only the manager receipts whose hosts are
inside the outgoing range; whole-editor teardown drains the manager first and
then performs ordinary ancestor cleanup.

This is a useful low-level tooling API, but it is **not** the rich-text
document-mutation API. A computed function must not call it: computed values
may rerun, have no transaction semantics, and cannot preserve editor history
or collaboration. `NodeViewManager` is the owner that uses this primitive.

## 9. `NodeViewManager` lifecycle

Each mounted `PineRichTextRoot` owns one manager. It is not process-global and
is not shared merely because two editors use the same `Arc<EditorRuntime>`.

The manager keeps an internal table such as:

```rust
HashMap<NodeViewInstanceId, MountedNodeView>
```

`MountedNodeView` contains the DOM host identity, semantic node type, erased
typed-update vtable, an owned type-erased subtree receipt whose function
pointers retain the concrete `N` and `C`, last semantic/view snapshot, and a
weak binding to the editor surface. The public mount result remains
`MountedSubtree<C>`; only the manager's private storage erases it.

### 9.1 Reconciler lifecycle sink

The reconciler receives a per-editor lifecycle sink and emits typed mutations
at the point their ordering is safe:

```rust
enum NodeViewLifecycle {
    Inserted { host: Element, node: Node, pos: usize },
    Retained { host: Element, old: Node, new: Node, pos: usize },
    WillRemove { host: Element },
}
```

The exact Rust representation may avoid cloning full nodes, but the semantic
events are required.

- `WillRemove` is delivered synchronously **before** `remove_child`,
  `replace_child`, or `innerHTML` destroys live DOM. It cannot be deferred in a
  journal returned after reconciliation. When a component callback initiated
  the transaction, reconciliation itself waits for the safe point from section
  7.4; `WillRemove` does not run until that pass begins.
- `Inserted` mounts exactly that host. There is no whole-editor
  `querySelectorAll` pass.
- `Retained` updates the current position and calls the typed sync function
  when attrs, marks, content, selection relation, focus, or editability changed.
- Child reconciliation for an editable view resolves through the manager's
  stored outlet and never walks into component chrome.
- A full render first drains every mounted view, then executes a recursive
  `HostPlan`. For each component node it inserts the outer empty host, mounts
  the shell, resolves the outlet, renders that node's children into the outlet,
  and recursively executes any child host plans produced there before moving
  to the next sibling. A flat pending-host list is insufficient because a
  nested host does not exist until its outer view is mounted.
- Explicit editor teardown drains the manager before removing the surface;
  generic ancestor teardown reaches the editor's pre-descendant release hook
  and establishes the same ordering before recursive child release.
- Reconciliation that retains the same compatible host retains its component
  instance even when preceding edits change its position.

The HTML-string renderer remains available for static/debug/test markup, but
is not a semantic static exporter (`data-pos` and node-view tags are
interactive details). The interactive renderer uses the structured recursive
plan above and never puts semantic children under an unmounted custom-element
string, where the HTML parser could reparent invalid intermediate table
content.

### 9.2 Selection-only updates

A selection/focus transaction may not change document nodes. The view layer
must still identify the previously affected and newly affected node views and
sync their `NodeViewSelection`/focus state. It must not rescan or update every
mounted component.

### 9.3 Failure behavior

A recoverable view update failure must not abandon unrelated reconciler work.
The manager reports the failing runtime/node/component, rolls back a partial
mount, and leaves a visible semantic fallback at that host in development and
production. Editable fallback keeps model children usable. Independent later
mounts and updates continue. Arbitrary panics are outside this isolation
guarantee.

If a later `sync_node` returns `Err`, the instance may already be partially
mutated. The manager therefore consumes its receipt, unmounts it once, and
replaces it with the semantic fallback; it does not retain or retry that
instance in place. A later document change may attempt a fresh mount according
to an explicit retry policy, never an implicit tight loop.

Every removal path must be idempotent: repeated cleanup may report an internal
invariant failure in debug builds, but must never double-fire component
unmount hooks or release another scope.

## 10. Event and mutation boundaries

Editor root listeners currently see events from the entire contenteditable
surface. Component chrome may contain `<button>`, `<input>`, menus, drag
handles, or another editable control. Without a boundary, pressing Enter in a
chrome input can execute the editor keymap.

The root walks `event.composedPath()` to the **nearest** stamped node-view host,
then classifies the target against that host's own outlet. This nearest-boundary
rule matters for a nested atom inside an outer editable view: the inner atom's
chrome is not ordinary editor input merely because it is under the outer
content outlet.

The v1 event matrix is:

| Source | Editor behavior |
|---|---|
| Inside the nearest editable outlet | Normal `beforeinput`, composition, key, paste, drop, and selection handling |
| Button/input/link or other chrome control | Browser/component owns it; no editor keymap or text transaction |
| Unconsumed pointer on atom/background shell | Select the semantic node |
| Explicit component drag handle | Component starts drag, but move/drop commits through a node transaction |
| Escape/arrows in chrome | Component/browser owns them unless it explicitly focuses or commands the editor |
| Event with `defaultPrevented` | Pine does not add a second default action |

Focus restoration after a chrome command is explicit through
`NodeViewHandle::focus_editor`; Pine does not steal focus from a form control.
Clipboard/drop inside chrome is component-owned, while the editable outlet
uses the editor's sanitized import pipeline.

The reconciler similarly treats component chrome as opaque. It reconciles only
the compiled content outlet. A component mutation outside that outlet does not
cause Pine to reparse the document; a mutation inside it is Pine-owned and is
reported as an invariant violation when it replaces/moves owned children or
the outlet itself.

V1 uses this deterministic boundary instead of arbitrary `stop_event` or
`ignore_mutation` callbacks. Callback hooks can be added later if real views
cannot fit the content-hole rule.

## 11. Extension-owned native DOM views

Component views do not solve custom semantic rendering by themselves. Today
the renderer has a hard-coded match for built-in node names and emits every
unknown node as `<span data-type="...">`.

Add a validated DOM output spec:

```rust
fn dom_views(&self) -> Vec<NodeDomSpec> {
    vec![
        NodeDomSpec::content::<CalloutNode>("aside")
            .class("pine-callout")
            .bind_attr(DomAttrBinding::string("data-kind", "kind"))
            .content_hole(),
        NodeDomSpec::nested::<TableNode>(
            DomOutputSpec::element("table")
                .child(DomOutputSpec::element("tbody").content_hole()),
        ),
    ]
}
```

The concrete builder may differ, but it must enforce:

- static validated tag and attribute names;
- escaped attribute values;
- declarative node-attr bindings with closed conversions (`String`, `Bool`,
  integer, and validated enum token), checked against the node spec;
- declarative text/accessible-label projections for atomic fallbacks;
- explicit `UrlPolicy` for URL-valued attrs (scheme allowlist, relative-URL
  policy, and no `javascript:`/event-attribute lane);
- zero content holes for atoms and exactly one for content nodes;
- no raw HTML strings;
- no arbitrary render callbacks in v1;
- a stable way for the reconciler to find the outer host and content hole.

Dynamic DOM values come only from declared node attrs. An `AttrBinding` names
the source typed-schema attr, destination HTML/data/ARIA attr, and closed
conversion. A `TextBinding` supplies escaped fallback text without raw HTML.
Neither can inspect arbitrary ancestors or execute application code. Shapes
that derive child presentation from a parent value must normalize that value
onto the child attrs in a transaction, or defer the presentation to a
component view. This keeps rendering deterministic and independently
serializable.

The native DOM spec is the default in-editor/fallback view when no component
view overrides it, and the recovery view when a component cannot mount. A
component view may wrap a different interactive shell. Neither is
automatically the Markdown or HTML serializer: extensions declare those
outputs independently through the serialization surface in section 12.

Keep's typed `KeepTitleNode` declares a native `h1` DOM view, so its block DOM
shape is part of the extension contract rather than an accidental unknown-node
span.

## 12. Persistence and serialization

### 12.1 Document format

The semantic `Node` JSON shape remains canonical. Typed external nodes add one
optional wire field, `version`:

```json
{
  "type": "diagram",
  "version": 1,
  "attrs": { "diagram_id": "dg_42", "theme": "dark" },
  "leaf": true
}
```

Built-in unversioned nodes may omit it. A typed node created by the schema
stores and serializes `N::VERSION`; `TypedNodeSpec` folds that metadata into
the compiled `NodeType`, so ordinary `Schema::node` construction stamps it.
An old typed node with no field is treated as version 1. Transforms and steps
preserve the version. Component names,
component refs, callbacks, DOM, and live view state never appear.

Loading, JSON paste, and remote snapshot import first parse a `WireNode` tree,
then run each typed node's migrations one version at a time, and only then
materialize/validate the schema `Node`. A newer unsupported version, missing
migration, unknown same-version attr, invalid default, or typed decode failure
returns a path-rich error while leaving the caller's original input and current
editor state untouched. `PineRichTextRoot` must surface this result through a
structured load-error event/prop plus a visible fallback; the current
`materialize_doc(...)->None` early return is removed.

Each migration must preserve the semantic type, advances exactly to its
declared `to` version, and is followed by full schema validation. It may rewrite
attrs or child wire nodes, but it cannot mount a view or perform side effects.

The Yrs representation reserves the non-user key `$pine_version` in each block
map and inline-atom embed object, alongside whatever type metadata (such as
`$type`) that shape already uses. The `$pine_` prefix is reserved from
extension attrs. Collab decode removes these metadata keys from
the user attr map, constructs a `WireNode`, runs version migration, and only
then calls schema materialization; it must not continue calling `Schema::node`
directly on an unversioned attr map. `TypedNodeSpec` folds version, migration
function pointers, and typed decode metadata into `NodeType`/`Schema`, because
the collab layer receives a `Schema`, not the original extension objects.

A string node-type discriminator is correct at the JSON/network boundary. It
is resolved immediately against the typed `EditorRuntime` schema. The resolved
semantic descriptor selects a pre-checked view vtable; application logic does
not turn the string into an arbitrary component mount.

### 12.2 View DOM is not output DOM

An interactive table component may render toolbars, resize handles, selection
overlays, and an editor content hole. None of that DOM is serialized.

Each typed semantic node must provide, or explicitly declare unsupported:

- Markdown parse and emit rules;
- semantic/static HTML output built from the safe `DomOutputSpec` primitives;
- plain-text/accessible fallback;
- clipboard import/export rules.

The contribution is associated with the semantic type, not a view component:

```rust
fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
    vec![
        NodeSerializationSpec::for_node::<DiagramNode>()
            .markdown(MarkdownPolicy::Unsupported)
            .html(SemanticDomSpec::atom("figure")
                .bind_text("diagram_id"))
            .plain_text(TextProjection::attr("diagram_id"))
            .clipboard(ClipboardPolicy::Semantic),
    ]
}
```

The concrete builders may reuse existing Markdown rule factories, but the
semantic HTML side uses escaped DOM specs rather than component DOM or raw HTML
callbacks.

Attrs validation and migrations are mandatory through `RichTextNodeType`.
`RuntimeBuilder` rejects a typed node that has neither a serializer nor an
explicit `Unsupported` policy for each output. An atomic node must never
disappear from export merely because it has no children. This RFC adds the
typed serialization-policy contribution; it does not assume Pine already has
a semantic HTML serializer or normalizer API.

Clipboard export writes validated Pine slice JSON under a private MIME type,
plus semantic sanitized HTML and plain text. Import prefers Pine JSON only
after version migration and schema validation; otherwise it parses sanitized
HTML/plain text through registered rules. HTML import drops event attributes,
sanitizes URL-bearing attrs through `UrlPolicy`, and never inserts imported raw
HTML directly into the editor surface. Component view DOM is never a clipboard
or document serialization source.

### 12.3 Missing extensions and views

- If a document references a node type absent from the runtime schema, loading
  fails with the runtime name, JSON document path, and missing extension/node
  type. The existing document is not replaced and the original input remains
  available to the caller. V1 is deliberately strict: there is no editable
  opaque-unknown-node mode, and no node is silently dropped.
- If the schema knows the node but its component view is unavailable, Pine
  uses the registered native DOM view when possible.
- If neither component nor native view is available, both development and
  production render a safe semantic/plain-text placeholder; development adds
  diagnostic detail. The result is never blank output.
- Attr decoding errors name the node type, attr path, expected Rust attrs type,
  and received JSON value.

### 12.4 Runtime/schema fingerprint

`EditorRuntime` exposes a stable **wire-compatibility** fingerprint derived
only from semantic node/mark schema shapes, typed node versions, content
expressions, attr requirements/defaults, and transaction/step encoding
versions. Component types, component tags, DOM specs, CSS, and crate/extension
release versions are excluded when they do not alter document or step wire
semantics; harmless view changes must not split collaboration rooms.

The descriptor is serialized canonically and hashed through the workspace
`pocopine-crypto` API (for example `sha256_hex`); this RFC does not introduce a
raw digest dependency or a hand-rolled hash loop in `pine-richtext`.

The fingerprint is a compatibility signal, not a component registry key.
Persisted documents may record it as advisory metadata, but saved versioned
nodes still migrate individually.

`pine-richtext-collab` cannot inspect an opaque yrs update and infer safety.
Its protocol phase adds a versioned hello containing the protocol version and
fingerprint, rejects the session before exchanging updates when they differ,
and namespaces room/topic plus persisted update/snapshot keys by the compatible
protocol/schema identity. This negotiation and storage migration is a distinct
implementation phase. It is not described as a local check immediately before
`apply_update`.

## 13. Relationship to `<pp-component>`

`<pp-component>` solves a different problem: a compiled `.poco` site owned by
a concrete host selects one allowed child using `ComponentRef<Host>`. The
component macro can prove the host's `uses = [...]` contract and compile the
`:is` expression.

Generated editor HTML has no authored `.poco` expression or concrete extension
host whose closed `uses` list third-party crates can amend. This is therefore
invalid as an external-block mechanism:

```html
<!-- rejected design -->
<pp-component :is="node.attrs.component"></pp-component>
```

It also couples durable content to one application's component names.

The typed `NodeViewSpec::{atom,editable}_component::<N, C>()` constructors are
the correct boundary for rich-text extensions. They record a semantic-type and
checked-mount vtable during runtime construction. Once mounted, `C` may use
`<pp-component>` normally in its own template for component-local UI.

## 14. Tables

`TablesExtension` implements RFC-079's semantic table model on this ownership
contract. The optional `table-view` feature adds the component shell without
changing the document shape.

### 14.1 Native rich-text table

A Markdown-compatible table remains semantic editor content:

```text
table
└── table_row+
    └── table_header_cell | table_cell
        └── inline content (v1)
```

The optional `PineRichTextTable` node view owns toolbar, selection, and resize
chrome. Pine owns rows, cells, text, selection, commands, history, copy/paste,
and document transactions.

The in-editor DOM does not have to equal every serialized format. The native
DOM spec and optional component view place rows under one `<tbody>` content
hole; header cells render as `<th>`. The Markdown serializer represents the
header row through GFM table events.

This removes RFC-079's proposed invisible `<thead>/<tbody>` grouping from the
generic reconciler. If exact `<thead>` editor DOM is required, those wrappers
must become explicit model nodes or a future multi-hole/grouping contract; it
must not be a renderer-only special case hidden from reconciliation.

`Selection::Cells` and `CellSelectionRect` implement same-table rectangular
selection. Mapping, bookmarks, rectangular slices, copy/paste, and row/column
commands operate on that semantic selection rather than styling a native DOM
selection.

The optional component host is the outer table block shell; row and cell views
use native DOM specs. Pocopine custom-element hosts are forbidden directly under
`table`, `tbody`, or `tr`, where the HTML parser can foster-parent them. The
empty-host-then-outlet construction order in section 7.5 prevents transient
invalid table children.

### 14.2 Opaque data grid

An external spreadsheet/data-grid widget is valid as an `Atom` node:

```json
{
  "type": "data_grid",
  "version": 1,
  "attrs": { "document_id": "grid_7" },
  "leaf": true
}
```

Its private cells are component UI, not Pine nodes. Pine can select, move,
delete, serialize, and collaborate on the outer block, but cannot offer native
cross-cell text selection, fine-grained cell history, Markdown table output,
or per-cell collaboration. The two products must not share an API that hides
this distinction.

## 15. Diagnostics and fail-closed rules

The following conditions are errors, never silent fallthroughs:

- node view registered for a node absent from the runtime schema;
- duplicate semantic name with a different typed-node `TypeId`;
- duplicate view registrations for one semantic node type;
- typed component registration collision;
- `RichTextNodeType` name/version/spec/attr-key disagreement;
- unknown same-version attrs or a missing/failed node migration;
- invalid atom/content-node pairing;
- missing or duplicate `pp-owned-content` marker;
- forged/stale owned-outlet metadata that does not resolve to one contained
  native element;
- typed attrs decode failure;
- typed mount state downcast or initializer failure;
- component mount without a scope/mount receipt;
- stale `NodeViewHandle` command;
- `#[context] NodeViewHandle<N>` requested outside a matching view scope;
- extension attr using the reserved `$pine_` metadata prefix;
- view removal that cannot find its owned mount;
- protocol/runtime fingerprint mismatch during collaboration handshake;
- novel atomic node with no serialization rule or explicit fallback.

Runtime errors include the editor runtime name, extension name, semantic node
type, component type/name where relevant, current document position if still
resolvable, and the failed operation.

One recoverably broken view must not stop later independent node views from
mounting or updating. Panic containment is not promised.

## 16. Implemented migrations

### 16.1 `PineTaskItem`

The demo task item becomes the first reference implementation:

1. Add `TaskItemNode: RichTextNodeType` with typed
   `TaskItemAttrs { checked: bool }`.
2. Implement `RichTextNodeView<TaskItemNode>` for `PineTaskItem` and replace
   `data-checked` reads with `sync_node` state.
3. Replace the slot/content selector with one `pp-owned-content` outlet.
4. Replace `data-pos` parsing and `pine:task-toggle` with
   `NodeViewHandle<TaskItemNode>::update_attrs`.
5. Delete the application-owned deferred task-toggle event listener; the
   framework handler safe point now owns deferral.
6. Remove the separate `App::register::<PineTaskItem>()`; typed view
   registration handles it.
7. Prove mount/update/unmount counts in browser tests.

### 16.2 Existing raw node-view tags

- Delete `ExtensionNodeView`, `RichTextExtension::node_views()`,
  `TaskListExtension::{with_node_view,with_node_view_tag}`, and the raw tag
  registry.
- Delete `registered_tags()` and the whole-editor `querySelectorAll` mount
  scan in the same migration.
- Replace their tests with exact semantic/component `TypeId`, outlet, retained
  update, and deterministic teardown coverage.

### 16.3 Keep title

Keep's title node moves from the generic `<span data-type="title">` fallback to
an extension-owned native DOM spec. Its visual CSS may remain unchanged.

## 17. Testing and acceptance criteria

### 17.1 Compile-time and runtime construction

- `atom_component::<DiagramNode, NonComponent>` fails to compile.
- A component paired with the wrong semantic node type fails to compile.
- An editable component without exactly one compiled outlet proof fails to
  compile/build with the component and template named.
- Model-only/default-feature builds contain no Pocopine or DOM dependency.
- A view adapter with an explicitly wired local feature registers both model
  and view contributions; disabling it cannot silently leave a compiled-out
  override on the model trait.
- Typed attr mismatch fails at runtime construction/load with an exact path.
- Attr derive rejects aliases, skips, flattening, custom serde hooks, and
  non-struct shapes that violate the closed-map contract.
- Unknown attrs, newer versions, and missing migrations fail without changing
  the current editor state.
- Duplicate semantic view mappings fail with both extension names.
- Unknown semantic node type in a view spec fails before editor mount.
- Typed component auto-registration succeeds without a separate app call.
- Component registration collision and typed-state downcast mismatch are
  fallible errors, not silent mount returns.
- Explicit custom-element registration remains possible and visibly untyped.
- `#[context] NodeViewHandle<N>` outside a matching view reports the component,
  handler, scope, and requested node type instead of silently skipping.

### 17.2 Lifecycle

- Initial attrs are visible during `on_setup` and `on_mount`.
- Attr, mark, content, selection, focus, and editability updates call
  `sync_node` without remounting.
- A preceding insertion updates `position()` while preserving a retained view
  instance.
- Delete fires component unmount exactly once and removes all scopes, effects,
  listeners, refs, contexts, and manager entries.
- Range replacement and full render release outgoing views before DOM removal.
- Nested component views mount depth-first after each parent outlet exists.
- Editor teardown releases every mounted view.
- Parent-scope teardown disarms manager receipts; manager-first and
  ancestor-first cleanup each fire unmount once.
- Repeated insert/delete cycles return all runtime counts to baseline.
- A failed partial mount rolls back all state and does not prevent a later
  valid view from mounting.
- A later `sync_node` error consumes/unmounts that instance once and installs
  the semantic fallback.

### 17.3 Commands and history

- `update_attrs`, delete, and extension commands create transactions.
- Self-update and self-delete inside a component handler commit state but do
  not run reconciliation, DOM removal, sync, or teardown until the component
  callback borrow unwinds.
- The same callback-frame test covers `Scope::invoke`, `Handle::update`, setup,
  mount, sync, watch, and unmount dispatch paths.
- A typed node command always receives the resolved node target, not the
  editor's incidental current selection.
- Undo/redo updates or recreates the view correctly without leaks.
- A stale handle returns `NodeViewError::Stale` and cannot affect a new node at
  the former position.
- Remote deletion while component chrome is focused tears down safely.
- Collaboration applies node attrs and structure through the same sync path.

### 17.4 DOM, events, and selection

- Atom views are node-selectable and delete as one unit.
- Editable views preserve cursor movement, marks, IME/composition, and nested
  inline content inside the content hole.
- Editable shells remain in the outer editing host; only chrome branches are
  non-editable, and cross-boundary selection/composition works in supported
  browsers.
- Initial component hosts are empty; children render only after the compiled
  outlet exists, including table row children.
- Buttons and inputs in chrome do not trigger editor keymaps or `beforeinput`.
- Nested views use the nearest stamped host when classifying events.
- Paste/drop inside the content hole remains editor-managed.
- Component DOM mutations outside the content hole do not rewrite the model.
- Component attempts to replace the content hole fail loudly.
- Mount failure renders a semantic production fallback and preserves editable
  children.
- Every `NodeViewSelection` relation has mutually exclusive boundary tests.

### 17.5 Serialization

- JSON round-trips without component names or ephemeral UI state.
- Typed node versions round-trip and older versions migrate exactly once per
  registered step.
- Block and inline-atom Yrs round trips preserve `$pine_version`, never expose
  it as a user attr, and decode through the same migration path as JSON.
- Markdown/HTML output is identical whether or not the component view is
  installed.
- Clipboard JSON is validated/migrated and imported HTML rejects unsafe attrs
  and URL schemes.
- A missing view falls back visibly without dropping semantic content.
- Missing schema extension leaves the existing document untouched and reports
  the exact missing node path.
- Collaborative peers with different wire fingerprints reject during
  handshake before updates are exchanged.

### 17.6 Tables

- Table cells remain editable Pine nodes under a component/native shell.
- `Selection::Cells` validates a same-table rectangle and maps through document
  changes with a bookmark.
- Row/column commands and rectangular copy/paste slices use semantic
  transactions.
- The optional table view adds selectors and commits column resizing without
  changing the table's semantic ownership.
- Native DOM and Markdown/HTML/plain-text/clipboard policies are supplied by
  the table extension, independently of the optional component view.

## 18. Implementation phases

Phases N0–N7 are implemented. This list records the landed boundaries.

### Phase N0 — failure and model prerequisites

- Add `RichTextNodeType`, `RichTextNodeAttrs`, `TypedNodeSpec`, and the
  re-exported `pine-richtext-macros` derive.
- Fold per-node wire version, ordered migrations, typed decode metadata, and
  minimal serialization/fallback policy declarations into `NodeType`/`Schema`.
- Make initial document materialization return/surface structured errors rather
  than `None`.
- Enforce required attrs/defaults and exact typed attrs during schema
  materialization.
- Add current task-item scope/effect/listener leak counters and failing
  remove/replace/full-render baselines.

### Phase N1 — typed owned mounting

- Add macro-emitted `MountableComponent` metadata, mandatory runtime
  type/downcast verification, and fallible idempotent registration.
- Add `mount_subtree_with<C>() -> Result<MountedSubtree<C>, MountError>`.
- Run the initializer before setup and return `Handle<C>` plus RAII teardown.
- Implement the shared one-shot receipt/disarm contract for explicit and
  ancestor teardown.
- Make constructor/template/root/downcast/initializer failures explicit and
  transactional.

### Phase N2 — lifecycle sink and manager

- Add `NodeViewManager` per editor.
- Emit inserted/retained/will-remove view mutations directly from
  reconciliation with teardown-before-DOM-removal ordering.
- Add structured interactive rendering that returns exact empty component
  hosts and executes recursive/depth-first host plans; mount shells before
  rendering children into outlets.
- Add the pre-descendant release-hook/receipt claim path.
- Delete `querySelectorAll` remount scans.
- Handle selection/focus-only sync, safe full-render teardown, and outlet-only
  child reconciliation.

### Phase N3 — typed node-view API

- Add view-gated `RichTextViewExtension` and `RichTextNodeView<N>` without
  changing the target-independent extension trait.
- Add `NodeViewSpec::{atom,editable}_component::<N, C>()`,
  `NodeViewHandle<N>`, and typed anchored commands.
- Add the compiled `pp-owned-content` marker, nearest-boundary event
  isolation, `#[context]` handler extraction, and the generic callback-frame
  view-flush safe point.
- Use the N0 generic safe fallback; do not stabilize/release external typed-node
  persistence until N4's required output policies are enforced.
- Add `try_build()` and delete the raw tag extension methods.
- Migrate `PineTaskItem` and remove its app-owned event bridge.

### Phase N4 — native DOM and serialization

- Add validated typed `NodeDomSpec`, safe attr/text/URL projections, and a
  single native content-hole output.
- Migrate Keep's title node.
- Add explicit Markdown/semantic HTML/plain-text/clipboard policies and
  sanitizer coverage.

### Phase N5 — compatibility and collaboration protocol

- Add canonical wire-schema descriptors and fingerprints via
  `pocopine-crypto`.
- Add reserved Yrs node-version metadata and route collab decoding through
  `WireNode` migration/materialization.
- Add collab hello negotiation plus fingerprinted room/topic/persistence-key
  migration.

### Phase N6 — semantic tables and cell selection

- Add `TablesExtension` with typed table, row, header-cell, and cell nodes.
- Add table mapping plus `Selection::Cells`, rectangular slices, bookmarks,
  and copy/paste behavior.
- Add table commands, key bindings, parsing, serialization, and focused model
  and browser coverage.

### Phase N7 — optional extension UI and inline atoms

- Add the feature-gated tables, tags, and bubble-menu surface in
  `pine-richtext-extensions`.
- Add `TagNode` plus `PineRichTextTag` as a typed inline atom with commands,
  serialization, clipboard behavior, and atom navigation/deletion.
- Add `PineRichTextBubbleMenu` and `BubbleMenuController`, following text,
  node, and rectangular cell selections.
- Add the optional `PineRichTextTable` view with selection controls and resize
  commits while retaining editor-owned rows and cells.

Every phase boundary keeps the task-list demo working and adds no silent
fallback lane.

## 19. Alternatives rejected

### 19.1 Generated `<pp-component :is="...">`

Rejected because generated editor HTML has no compiled owning expression or
closed host `uses` contract. It also persists or derives UI identity from
document data. Typed node-view registration is the correct extension boundary.

### 19.2 A computed function returning child components

Rejected because computed evaluation is not an editor transaction and may run
multiple times. It bypasses selection mapping, history, serialization,
collaboration, copy/paste, and deterministic cleanup.

### 19.3 Public raw `node_type -> tag` maps

Rejected for Pocopine components because they throw away the component type,
require separate registration, cannot provide typed attrs, and fail silently
on typos. Retained only as an explicit Web Component escape hatch.

### 19.4 Store `ComponentRef` in the document

Rejected because `ComponentRef<Host>` is process/UI-specific, not serializable
semantic content. A document must survive a different application renderer or
no interactive renderer at all.

### 19.5 Serialize component DOM

Rejected because interactive chrome is not semantic output and may contain
private state, temporary controls, selection overlays, or nested components.

### 19.6 Make every rich block an atom

Rejected for tables, callouts, and other blocks whose descendants must remain
normal rich-text content. Atom views remain correct for embeds and opaque
widgets.

### 19.7 Arbitrary HTML/render callbacks

Rejected for v1 because they make escaping, content-hole ownership, path
mapping, and reconciliation unverifiable. A small declarative DOM spec covers
the required extension shapes and can be expanded deliberately.

### 19.8 Let the view component define persisted attrs

Rejected because the document must validate, migrate, serialize, and
collaborate without that renderer installed. `RichTextNodeType` owns attrs;
`RichTextNodeView<N>` proves the renderer is paired with that semantic type.

### 19.9 Find editable content with an arbitrary CSS selector

Rejected for the typed path because a selector is only checked after mount and
can drift during template refactors. The compiler-recognized
`pp-owned-content` marker emits a stable outlet proof. Legacy selector-based
views remain only during migration.

## 20. Prior art

- [ProseMirror node views guide](https://prosemirror.net/docs/guide/#view.node_views)
  and [NodeView reference](https://prosemirror.net/docs/ref/#view.NodeView):
  current-position getter, `contentDOM`, update/reuse, event and mutation
  boundaries, and deterministic destroy.
- [Tiptap node views](https://tiptap.dev/docs/editor/extensions/custom-extensions/node-views):
  editable, non-editable, and mixed component content; explicit separation
  between the in-editor view and serialized output.
- [Lexical nodes](https://lexical.dev/docs/concepts/nodes) and
  [NodeState](https://lexical.dev/docs/concepts/node-state): component-like
  `DecoratorNode`s with serializable state participating in reconciliation,
  history, and JSON.
- [Slate rendering](https://docs.slatejs.org/concepts/09-rendering): renderer
  components preserve editor-owned attributes/children; void elements remain
  explicit black boxes with selection anchors.
- [prosemirror-tables](https://github.com/ProseMirror/prosemirror-tables):
  semantic rows/cells, a dedicated cell selection, rectangular slices, and
  transaction-backed table commands.

## 21. Resolved design defaults

1. **Typed Pocopine component or general `pp-component`?** Typed node-view
   component; `pp-component` is allowed only inside its compiled template.
2. **Who adds/removes views?** Document transactions add/remove nodes;
   `NodeViewManager` follows and owns view lifecycle.
3. **Can computed functions mount views?** No. They remain pure derivations.
4. **Who owns typed attrs?** `RichTextNodeType`; the component receives typed
   full-node snapshots through `sync_node`, initialized before setup and later
   updated through `Handle<C>`.
5. **How does a component edit its node?** `NodeViewHandle<N>` creates anchored
   editor transactions and resolves the live position at call time.
6. **How do external Web Components fit?** Wrap them in a typed node-view
   component in v1; no raw tag adapter ships.
7. **Are tables component atoms?** Native rich-text tables are semantic node
   trees; opaque grids are separate atom blocks.
8. **Does editor DOM equal export HTML?** No. Semantic serialization is
   independent from interactive node-view DOM.
9. **How many editable holes in v1?** Exactly one, declared with the compiled
   `pp-owned-content` marker.
10. **What happens when a view cannot mount?** Recoverable failure rolls back,
    reports context, and renders a semantic fallback in every build;
    unrelated views continue.
11. **Can component APIs leak into model-only builds?** No. Typed semantic
    nodes are target-independent; component views/managers are `view`-gated.
12. **Are inline component node views included?** Inline atom views are. The
    implemented `TagsExtension` pairs `TagNode` with `PineRichTextTag` through
    `NodeViewSpec::atom_component`; editable inline content-hole views remain
    out of scope.
13. **When do self-updates flush the view?** After the outermost Pocopine
    mutable component-callback borrow unwinds.
