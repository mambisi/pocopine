# Component composition

One way to compose: **tag-based composition with bare kebab-case
tags**. A parent's template references a child by its tag; attributes
become props; children inside the tag become slot content. Tags read
as HTML, and `pp-data` stays a compiler-injected implementation
detail rather than something authors type.

## Tag naming

The runtime tag for a component is the kebab-case of its struct
ident — no prefix:

| Struct ident | Tag |
|---|---|
| `Counter` | `<counter>` |
| `TodoItem` | `<todo-item>` |
| `NavBar` | `<nav-bar>` |

**Rules:**

* Authors don't choose tag names. The struct ident decides.
* Override the derived name with `#[component(name = "x")]` — tag
  becomes `<x>`.
* **HTML collision check.** The `#[component]` macro rejects struct
  idents whose kebab-case matches a known HTML element. The rejected
  list is the full HTML5 catalog plus `slot`, `template`, `svg`,
  `math`. Rename the struct or pass an explicit `name = "..."` to
  opt out (but you *should* rename — HTML collisions confuse readers
  and break tooling even when the runtime copes).
* Browsers treat unknown kebab-case tags (`<counter>`, `<todo-item>`)
  as `HTMLUnknownElement`. That's fine: we don't rely on the native
  custom-elements API, we just walk the DOM. Unknown elements render
  nothing by default, which matches what we want before the walker
  clones the registered template in.

## Using a component in a parent's template

```html
<!-- TodoList.poco -->
<div pp-init="init">
  <h1 pp-text="title"></h1>
  <todo-item id="1" label="Buy milk" />
  <todo-item id="2" label="Write docs" done />
</div>
```

`TodoItem` gets instantiated once per `<todo-item>` tag. Attributes
on the tag feed the child's initial state (next section). The parent's
directives (`pp-init`, `pp-text`) still evaluate in the parent's scope.

## Props: attributes flow into child state

**Static attributes** seed the child's fields on mount. All `pub`
fields on the child struct are addressable; attribute names match
field names (`id`, `label`, `done`). Values are parsed into the field's
type:

| Field type | Attribute value format |
|---|---|
| `String` | anything (used verbatim) |
| `i8..i64`, `u8..u64` | decimal integer |
| `f32`, `f64` | decimal number |
| `bool` | presence = true, absence = false; `"false"` = false |
| `Option<T>` | absent = `None`, else parsed as `T` |
| `Vec<T>` / `HashMap<..>` / other `Deserialize` | JSON: `foo='[1,2,3]'` |

**Reactive attributes** use `pp-bind:field="expr"`. `expr` evaluates
in the parent's scope; a binding effect keeps the child's field in
sync when the parent mutates:

```html
<todo-item
  pp-bind:id="todo.id"
  pp-bind:label="todo.label"
  pp-bind:done="todo.done" />
```

Under the hood, `pp-bind:X` on a custom-element tag writes to
`child_proxy["X"]` inside an `effect()` — so it rides the existing
reactivity engine, no new machinery.

**Rules:**

* Only `pub` fields are settable from attributes. Non-pub = not a prop.
* **Static attributes are one-shot**. They run during `init` and are
  not reactive. If you want reactivity, use `pp-bind:`.
* Passing a prop the child doesn't declare is a warning (dev build)
  and a no-op (release).
* Missing required props is *not* an error — fields start at
  `Default::default()`. A child component is expected to render
  sensibly with defaults.

## Slots

Children inside the tag become slot content. One default slot for v0;
named slots land with iteration.

```html
<!-- Card.poco -->
<div class="card">
  <header pp-text="title"></header>
  <main>
    <slot></slot>
  </main>
</div>
```

```html
<!-- Parent.poco, using <card> -->
<card title="Hello">
  <p>Body content lives here.</p>
  <button pp-on:click="dismiss">OK</button>
</card>
```

The walker, when it instantiates a component, clones the registered
template into the custom-element tag and then moves the tag's
original children into wherever `<slot></slot>` was. Directives
inside the slot content still evaluate in the **parent's** scope, not
the child's — that's what makes a slot useful.

`<slot>` here is the native HTML element. Outside Shadow DOM (which
we don't use), `<slot>` is inert — the browser renders it as a
transparent pass-through, and our walker repurposes it as the slot
marker. Same tag name, same mental model, no Shadow DOM cost.

**Rules:**

* **Default slot only in v0.** One `<slot>` per template.
* **Slot content is parent-scoped.** A handler referenced inside slot
  content (`pp-on:click="dismiss"`) resolves to the parent's handler.
* **Shadow DOM is not used.** Slots are a flat DOM move; CSS scoping
  still works because scoping is attribute-based, not shadow-based.

## Iteration (`pp-for`, deferred)

Planned syntax for the array-reactivity milestone:

```html
<todo-item
  pp-for="todo in todos"
  pp-bind:key="todo.id"
  pp-bind:id="todo.id"
  pp-bind:label="todo.label" />
```

**Rules (when shipped):**

* `pp-for` on a custom-element tag clones the child once per item.
* `pp-bind:key` is required — that's how the walker pairs list items
  to child scopes across re-renders (stable identity = no unnecessary
  re-mounts).
* Iteration variable scope: `todo` is readable by any `pp-bind:X` on
  the same element. It's not a real parent-scope field — it's a
  per-iteration lexical binding.

This is **blocked** by the array-reactivity work in
`docs/reactivity/02-roadmap.md` (#9). Syntax is committed now so
users plan around it.

## Where `pp-data` went

Authors don't write `pp-data` anymore. When the compiler emits a
component's registered template string, it injects `pp-data="<name>"`
onto the root element. Authors just write the template:

```html
<!-- Counter.poco — as authored -->
<div pp-init="init">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

The compiler emits:

```html
<div pp-data="counter" pp-init="init">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

This keeps the authored template focused on content and directives —
the "this element owns a scope" fact is tracked by the filename and
struct ident, not duplicated in the markup.

(Note: the current runtime *requires* `pp-data` on the root. This
change lands as part of the composition milestone, alongside tag
resolution. Until then, authored templates still include
`pp-data="..."` manually.)

## Full example

**`components/todo.rs`**

```rust
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Todo { pub id: u32, pub label: String, pub done: bool }

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoList {
    pub title: String,
    pub todos: Vec<Todo>,
}

#[handlers]
impl TodoList {
    pub fn init(&mut self) {
        self.title = "Things to do".into();
        self.todos = vec![
            Todo { id: 1, label: "Buy milk".into(),   done: false },
            Todo { id: 2, label: "Write docs".into(), done: true  },
        ];
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoItem {
    pub id: u32,
    pub label: String,
    pub done: bool,
}

#[handlers]
impl TodoItem {
    pub fn toggle(&mut self) { self.done = !self.done; }
}
```

**`components/TodoList.poco`** (v0 — `pp-for` not yet available)

```html
<div pp-init="init">
  <h1 pp-text="title"></h1>
  <todo-item id="1" label="Buy milk" />
  <todo-item id="2" label="Write docs" done />
</div>
```

**`components/TodoItem.poco`**

```html
<div>
  <input type="checkbox" pp-model="done" />
  <span pp-text="label" pp-bind:class="done ? 'done' : ''"></span>
  <button pp-on:click="toggle">toggle</button>
</div>
```

## Runtime resolution

For the record, here's what the walker does — nothing surprising,
just the hook points:

1. Pre-order DOM walk. For each element:
2. Look up the tag name in the component registry. If registered:
   * Capture the tag's original direct children (future slot content).
   * Build a fresh `Scope`.
   * Apply attribute-props to the scope's state (static + `pp-bind:`).
   * Clone the registered template into the element (replacing any
     current children).
   * Move the captured children into the first `<slot>` found in
     the clone.
   * Recurse into the cloned subtree.
3. Else: handle `pp-*` attributes on this element normally (existing
   behavior).

Tag-to-component resolution is O(1) with the existing registry. The
registry keyspace is "exact tag name" — since we kebab-case idents at
registration time and reject HTML-collision names at compile time,
the lookup can't accidentally fire on a real HTML element.

## Out of scope for v0

* Named slots (`<slot name="footer">` + `slot="footer"` on children).
* `pp-for` on custom elements — blocked by array reactivity.
* Scoped CSS cascading into slot content (we leave slot content in the
  parent's scope attribute, so its styles come from the parent).
* Event-based child→parent callbacks beyond `$dispatch`. Props don't
  accept callbacks today; use event dispatch instead
  (see `02-state.md`).
* Two-way prop binding (`pp-model:prop`). Use parent-side state + event
  dispatch; if the pattern gets common, we'll promote it.
