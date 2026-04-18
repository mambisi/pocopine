# Compiling `.poco` + `.rs` + `.css` → registered component

The user writes three files. The `#[component]` macro ties them
together at compile time; no separate build step is needed.

## Surface

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "counter",
    template = "Counter.poco",
    style = "Counter.css",     // optional
)]
pub struct Counter { pub count: i32 }
```

`template` and `style` are **paths relative to the `.rs` source file**,
matching `include_str!`'s resolution. Either may be omitted.

## What the macro does

Current `#[component]` emits:

* `impl ComponentState for Counter` (proxy get/set/keys/invoke).
* `impl Counter { pub fn register() { ... } }` — calls
  `register_component("counter", ctor)`.

The new `template` + `style` arguments extend `register()`:

```rust
impl Counter {
    pub fn register() {
        register_component("counter", || Rc::new(RefCell::new(Counter::default())));
        register_template("counter", include_str!("Counter.poco"));
        inject_style("counter", include_str!("Counter.css"));
    }
}
```

Key points:

1. **`include_str!` is used deliberately.** cargo tracks it, so edits
   to the `.poco` or `.css` invalidate the build cache. No extra
   `rerun-if-changed` plumbing.
2. **Validation at expansion time.** The macro parses the `.poco`,
   walks the root element, and rejects if:
   * there's more than one root element,
   * the root element's `pp-data` attribute doesn't match `name`.
   Errors point to the `.poco` with a file+line span.
3. **CSS is transformed before emission** (see `03-scoped-styles.md`).
   The `include_str!` still gives cargo the cache key; the transform
   runs in the macro body and the result is emitted as a string literal.
4. **Both arguments are optional.** A component can register with a
   template and no styles, or neither (the current behavior).

## Runtime additions needed

Two small additions to `pocopine-core`:

```rust
// templates.rs
pub fn register_template(name: &'static str, html: &'static str);
pub fn template_for(name: &str) -> Option<&'static str>;

// styles.rs
pub fn inject_style(component: &'static str, css: &'static str);
```

`inject_style` is idempotent per component — the first call appends a
`<style data-pp-component="counter">` to `<head>`, subsequent calls
are no-ops.

## Walker integration

Today the walker instantiates a scope when it sees `pp-data="counter"`
on an existing DOM element. With templates available, two modes:

* **SSR (already-rendered DOM)** — the server wrote the template into
  the response. The walker sees a real element with children, binds
  directives, doesn't clone the template. **Default behavior;
  unchanged.**
* **Client-side mount** — a caller wants to render a component on
  demand: `pocopine::mount("counter", target_element)`. This clones
  the registered template into `target_element`, then walks it.

Adding `mount()` is a ~15-line helper; we can add it with the template
machinery or defer.

## What the macro does **not** do

* **It doesn't parse the `.poco` as HTML deeply.** Minimum validation
  only (root element, root `pp-data`). Leaving the body opaque means
  the compiler doesn't need to keep up with every directive's options.
  The walker already does real directive parsing at runtime.
* **It doesn't compile Rust inside attribute values.** Values are still
  identifiers (field/handler names). When expression support lands, the
  macro grows a pass that parses the attribute values with `syn::parse_str`.
* **It doesn't bundle or minify.** `lightningcss` can minify CSS; we
  leave that off by default and behind a cargo feature.

## Alternative paths we explicitly chose against

* **Separate `build.rs` codegen.** Considered; adds a per-project
  build script with no upside when `include_str!` already handles
  caching. Skipped.
* **`.poco` as proc-macro input via `poco!("Counter.poco")`.** Clean in
  isolation, but then `Counter.rs` no longer looks like a normal
  struct definition — it becomes a macro call that hides the type.
  Rejected for DX (no rust-analyzer completion on the struct, no
  ordinary `impl` blocks).
* **Inline everything into `#[component(template = "...", style = "...")]`
  as literal strings.** Works for tiny components, terrible for
  anything over ~20 lines of markup. Paths stay.

## Implementation order

1. Add `template` + `style` keyword args to the `#[component]` macro
   grammar. Both optional. String literal only (no expressions).
2. Extend the macro to emit `include_str!` calls and the two runtime
   registration calls.
3. `pocopine-core::templates` + `register_template` + `template_for`.
4. `pocopine-core::styles` + `inject_style` (plain append, idempotency
   via a `thread_local` set of seen component names).
5. Walker: no change required for SSR mode; optional `pocopine::mount()`
   helper for client-side template cloning.
6. `<style scoped>`-equivalent transform (see `03-scoped-styles.md`),
   behind a `scoped = true` default in `#[component]` when `style` is
   set.

Steps 1–5 are the MVP. Step 6 is the first piece that needs a real CSS
parser (`lightningcss`).
