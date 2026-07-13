# Typed rich-text extensions

Extensions split durable semantic data from browser presentation. Documents
store a typed node name, version, attrs, marks, and children. They never store a
Pocopine component tag, callback, DOM node, or mount handle.

## Model and native fallback

The target-independent extension owns the wire schema and a safe structural DOM
fallback:

```rust
use pine_richtext::extension::RichTextExtension;
use pine_richtext::model::NodeSpec;
use pine_richtext::render::{DomAttrBinding, NodeDomSpec};
use pine_richtext::{
    RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
struct CalloutAttrs {
    kind: String,
}

struct CalloutNode;

impl RichTextNodeType for CalloutNode {
    const NAME: &'static str = "callout";
    const VERSION: u32 = 1;
    type Attrs = CalloutAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .content("block+")
            .required_attr("kind")
    }
}

struct CalloutExtension;

impl RichTextExtension for CalloutExtension {
    fn name(&self) -> &str { "callout" }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<CalloutNode>()]
    }

    fn dom_views(&self) -> Vec<NodeDomSpec> {
        vec![NodeDomSpec::content::<CalloutNode>("aside")
            .class("pine-callout")
            .bind_attr(DomAttrBinding::token(
                "data-kind",
                "kind",
                ["note", "warning"],
            ))]
    }
}
```

`RichTextNodeAttrs` is a closed serde contract. Unknown same-version attrs,
missing required attrs, unsupported future versions, incomplete migrations,
unsafe DOM names, event attributes, and values outside a declared token/URL
policy fail during load or runtime construction. DOM shapes are data structures;
html5ever owns static serialization and browser DOM APIs own interactive
materialization.

## Typed component view

Browser presentation is a separate feature-gated contract:

```rust
use pine_richtext::view::{
    NodeViewError, NodeViewSpec, NodeViewUpdate, RichTextNodeView,
    RichTextViewExtension,
};

impl RichTextNodeView<CalloutNode> for PineCallout {
    fn sync_node(
        &mut self,
        update: NodeViewUpdate<CalloutAttrs>,
    ) -> Result<(), NodeViewError> {
        self.kind = update.attrs.kind;
        self.selection = update.selection;
        Ok(())
    }
}

impl RichTextViewExtension for CalloutExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        vec![NodeViewSpec::editable_component::<CalloutNode, PineCallout>()]
    }
}
```

An editable component template declares exactly one compiled ownership outlet:

```html
<aside class="pine-callout-shell">
  <button class="pine-callout-menu" contenteditable="false">Options</button>
  <div class="pine-callout-content" pp-owned-content></div>
</aside>
```

Atoms use `NodeViewSpec::atom_component` and have no outlet. Runtime building
proves the exact semantic `TypeId`, component mount ABI, atom/editable ownership,
and compiled outlet path before an editor mounts:

```rust
let runtime = pine_richtext::RuntimeBuilder::new()
    .name("document")
    .with_view(CalloutExtension)
    .try_build()?;
```

Typed view components register automatically. There is no raw
`node_type -> custom-element tag` API and no post-render `querySelectorAll`
mount pass.

## Editing from component handlers

Component chrome receives a weak, generation-checked handle through the
explicit handler-context lane:

```rust
#[pocopine::handlers]
impl PineCallout {
    fn make_warning(
        &mut self,
        #[context] node: pine_richtext::view::NodeViewHandle<CalloutNode>,
    ) {
        let _ = node.update_attrs(|attrs| attrs.kind = "warning".into());
    }

    fn remove(
        &mut self,
        #[context] node: pine_richtext::view::NodeViewHandle<CalloutNode>,
    ) {
        let _ = node.delete();
    }
}
```

Every mutation is a normal editor transaction, so selection mapping, history,
serialization, collaboration, and teardown remain coherent. Handles resolve
the node's current position on every call and return `NodeViewError::Stale`
after deletion, replacement, or editor teardown.

## Ownership rules

- The document owns attrs, marks, children, versioning, and migrations.
- The per-editor manager owns component mount/update/unmount receipts.
- The component owns chrome and ephemeral UI state.
- Pine exclusively owns descendants under `pp-owned-content`.
- The nearest stamped node-view host classifies events: outlet input belongs to
  Pine; component controls and atom chrome do not reach the editor keymap.
- Markdown, semantic HTML, plain text, clipboard data, and collaboration use
  semantic nodes—not live component DOM.

The built-in typed task-item and `pine-richtext-extensions` table/tag examples
are the reference implementations.
