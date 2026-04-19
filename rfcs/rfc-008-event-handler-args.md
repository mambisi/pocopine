# RFC 008 — event handler arguments

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md) §5 (handlers) |

## 1. Summary

`#[handlers]` methods may now take one or more arguments beyond
`&mut self`. Typed event handlers (`ev: MouseEvent`), primitive
payload handlers (`value: String`), and the raw `JsValue` escape
hatch all work. Conversion goes through a new
[`FromHandlerArg`](#7-implementation) trait with blanket impls for
the cases every pocopine component needs.

```rust
#[handlers]
impl SearchBar {
    pub fn on_input(&mut self, ev: InputEvent) {
        let input = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok());
        if let Some(input) = input {
            self.query = input.value();
        }
    }

    pub fn apply(&mut self, override_: String) {
        self.query = override_;
    }
}
```

Before this RFC, `#[handlers]` errored on anything other than
`(&mut self)` with the text "event args will be wired in a later
milestone." That milestone is this RFC.

## 2. Motivation

Every component library needs event data. `on_click(ev)` needs the
mouse position. `on_input(ev)` needs `ev.target.value`. `on_keydown`
needs the key code. `$dispatch("rename", "new-name")` needs to land
as a string on the other side.

Without handler args, authors worked around it with brittle patterns:
reading from a `pp-ref`'d element inside the handler, writing the
DOM value into a scope field via a detour, or using `$el` + DOM
queries. All noisy, none typed. Pine components can't be built on
that — every input, button, menu needs event data.

## 3. Non-goals

* **Handler results.** Methods return `()`. Hooking return values
  into the directive that called the handler (e.g. `pp-on:click`
  collecting booleans for imperative cancellation) is separate work.
* **Async handlers.** `async fn` in `#[handlers]` is out. Use
  `dispatch!(...)` from a sync handler, same as today.
* **Deep deserialization of user-defined Rust structs.** For v0,
  custom types need to implement `FromHandlerArg` explicitly.
  `serde_wasm_bindgen` path is scoped to primitives + `JsValue`.
* **Variadic handlers** or JavaScript-style "I'll look at
  `arguments`." One signature per method, type-checked at
  compile time.
* **Positional argument **reordering** from the template.** The
  attribute value is a handler name (`"apply"`), not a call
  expression. Dispatching from the template still picks the args
  implicitly — DOM event for `pp-on:*`, `$dispatch` payload for
  custom events.

## 4. Surface

The method signature determines the shape:

| Method shape | Dispatched from | Args |
|---|---|---|
| `fn m(&mut self)` | anywhere | none (today's behaviour) |
| `fn m(&mut self, ev: Event)` | `pp-on:<event>` | DOM event |
| `fn m(&mut self, ev: MouseEvent)` | `pp-on:click` etc. | DOM event, downcast |
| `fn m(&mut self, value: String)` | `$dispatch("m", "hi")` / `pp-on:<custom>` | `args[0].as_string()` |
| `fn m(&mut self, n: f64)` / `i32` / `u32` / `bool` | `$dispatch` / custom events | primitive coercion |
| `fn m(&mut self, v: JsValue)` | anywhere | raw escape hatch |
| `fn m(&mut self, a: A, b: B)` | multi-arg `$dispatch` | `args[0]`, `args[1]` |

Every `T` in the argument position must implement `FromHandlerArg`
(see §7). Built-in impls cover the rows above.

## 5. Semantics

### 5.1 Where the args come from

* **`pp-on:<event>`** — the directive passes the DOM `Event`
  (unchecked-cast to `JsValue`) as `args[0]`. Single arg, always.
* **`$dispatch("name", payload)`** — `payload` becomes `args[0]`.
  Single arg.
* **`$dispatch("name", payload1, payload2, ...)`** — extension to
  the existing `$dispatch` (currently takes a single payload).
  Multiple args become `args[0..N]`.
* **`Scope::invoke(key, args)` directly from Rust** — caller
  passes the `Array` verbatim; its values land at the indexed slots.

### 5.2 Conversion

For each method parameter at index `i`, the macro generates:

```rust
let arg_i = match <T as FromHandlerArg>::from_handler_arg(args.get(i as u32)) {
    Some(v) => v,
    None => return JsValue::UNDEFINED, // silently drop the call
};
```

Silent drop matches the existing "key not found → UNDEFINED"
behaviour: a bad payload shouldn't panic the whole app. A dev-mode
`console::warn` on `None` is a follow-up nicety.

### 5.3 Arity mismatch

If the template invokes the handler with fewer args than the
signature expects, `args.get(i)` returns `JsValue::UNDEFINED` for
the missing slots. Primitive impls return `None` on undefined → the
call is dropped. `JsValue` and optional types (`Option<T>`) accept
undefined.

Extra args from the template are ignored.

## 6. Examples

### 6.1 Search input

```rust
#[handlers]
impl SearchBar {
    pub fn on_input(&mut self, ev: InputEvent) {
        if let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) {
            self.query = el.value();
        }
    }
}
```

```html
<input pp-on:input="on_input" />
```

### 6.2 Keyboard navigation

```rust
#[handlers]
impl Menu {
    pub fn on_key(&mut self, ev: KeyboardEvent) {
        match ev.key().as_str() {
            "ArrowDown" => self.index += 1,
            "ArrowUp"   => self.index = self.index.saturating_sub(1),
            "Escape"    => self.open = false,
            _ => {}
        }
    }
}
```

### 6.3 Custom events from a child component

```rust
// Inside <pine-input>
#[handlers]
impl PineInput {
    pub fn on_input(&mut self, ev: InputEvent) {
        let value = /* ... */;
        self.model = value.clone();
        let detail = JsValue::from_str(&value);
        dispatch_event("pp:update:model", detail);
    }
}

// Parent listens
#[handlers]
impl Form {
    pub fn set_name(&mut self, new_name: String) {
        self.name = new_name;
    }
}
```

```html
<pine-input pp-on:pp:update:model="set_name"></pine-input>
```

## 7. Implementation

### 7.1 `FromHandlerArg` trait (new)

```rust
pub trait FromHandlerArg: Sized {
    fn from_handler_arg(v: JsValue) -> Option<Self>;
}
```

Default impls (all in `pocopine-core`):

| Type | Impl |
|---|---|
| `JsValue` | identity |
| `Option<T: FromHandlerArg>` | `Some(T::from_handler_arg(v))` even when v is undefined |
| `String` | `v.as_string()` |
| `bool` | `v.as_bool()` |
| `f64` / `f32` | `v.as_f64()` |
| `i32` / `i64` / `u32` / `u64` / `usize` / `isize` | `v.as_f64()` → cast |
| `web_sys::Event` | `v.dyn_into::<Event>().ok()` |
| `web_sys::UiEvent` / `MouseEvent` / `KeyboardEvent` / `InputEvent` / `FocusEvent` / `WheelEvent` / `CustomEvent` / `PointerEvent` / `DragEvent` / `TouchEvent` | same pattern |

Users who need `#[derive(Deserialize)] struct Foo { … }` as an arg
type can implement `FromHandlerArg` themselves in two lines:

```rust
impl FromHandlerArg for Foo {
    fn from_handler_arg(v: JsValue) -> Option<Self> {
        serde_wasm_bindgen::from_value(v).ok()
    }
}
```

### 7.2 `#[handlers]` macro changes

Remove the current arity guard. For each method, generate:

```rust
#name => {
    let arg0: #ty0 = match <#ty0 as ::pocopine::__private::FromHandlerArg>::from_handler_arg(
        args.get(0u32),
    ) {
        Some(v) => v,
        None => return ::pocopine::__private::JsValue::UNDEFINED,
    };
    let arg1: #ty1 = match <#ty1 as ::pocopine::__private::FromHandlerArg>::from_handler_arg(
        args.get(1u32),
    ) {
        Some(v) => v,
        None => return ::pocopine::__private::JsValue::UNDEFINED,
    };
    Self::#ident(self, arg0, arg1);
    ::pocopine::__private::JsValue::UNDEFINED
}
```

No per-arg arity generalisation — the macro generates one arm per
method with the exact number of `args.get(N)` calls the signature
needs.

### 7.3 `pp-on` dispatch

Replace every `invoke_handler(scope_id, &handler, &Array::new())`
with a call that includes the DOM event:

```rust
let args = Array::new();
args.push(ev.as_ref());
invoke_handler(scope_id, &handler, &args);
```

Same for the debounce callback path.

### 7.4 `$dispatch` multi-arg

Today:

```js
$dispatch(name, detail)  // single detail, becomes event.detail
```

Extend (non-breaking):

```js
$dispatch(name, detail)        // existing — single payload, arg[0]
$dispatch(name, [a, b, c])     // array-as-payload — arg[0]=[a,b,c]
```

Handler implementations listening via `pp-on:<name>` can declare
`(&mut self, detail: T)` or `(&mut self, detail: JsValue)` and
unpack themselves. If authors need positional multi-arg dispatch,
they still get it through `Scope::invoke` from Rust directly — no
template syntax for that in v0.

## 8. Edge cases

* **Method with a `self: Box<Self>` receiver.** Not supported — we
  stay on `&mut self`.
* **Generic methods on `impl`.** Not supported — the macro walks
  `ImplItem::Fn` and emits a single arm per method; generics need
  monomorphisation at expansion time.
* **Moves instead of `&mut self`.** `pub fn m(self, …)` is rejected
  with a clear error — we need `&mut self` for `invoke_handler`.
* **Handler name collisions** with `on_mount` / `on_unmount`.
  `on_mount` and `on_unmount` keep their special status (wired into
  `HandlerDispatch::mount` / `unmount`). Taking `&mut self` + args
  in those lifecycle methods isn't currently supported — lifecycle
  hooks run without event context.
* **Arg type doesn't impl `FromHandlerArg`.** Compile error at the
  call site where the macro generates
  `<T as FromHandlerArg>::from_handler_arg`. Points authors at the
  trait to implement.

## 9. Alternatives considered

* **Magic arg injection by name** (Vue / Nuxt-style). Lets you write
  `fn on_click(ev)` and we pass the event as `ev` based on the
  ident. Too implicit; breaks when arg names don't match convention.
* **Only one arg allowed.** Simpler but forces multi-arg handlers
  to unpack from a tuple JsValue — awkward ergonomics.
* **`TryFrom<JsValue>`.** Exists but not implemented by `String`,
  primitive types, or most `web_sys` Event types. Our trait is
  narrower and picks the right conversion for each.
* **Blanket `impl<T: JsCast> FromHandlerArg for T`.** Conflicts
  with `impl for JsValue` (since `JsValue: JsCast`), and we'd need
  specialisation to also cover primitives. The explicit-impl-list
  approach sidesteps coherence issues.

## 10. Out of scope (future)

* Async handlers via a new `#[async_handler]` macro shape.
* Handler return values (mutable directive-side effects).
* Keyword-arg-style dispatch (`$dispatch("m", { name: "x" })` →
  `(&mut self, name: String)`).
