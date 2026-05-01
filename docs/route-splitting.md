# Route splitting without shipping another runtime

Pocopine is experimenting with route-level code splitting. The
first implementation produced separate wasm files per route, but
the numbers showed an important problem: a route wasm file is not
automatically a small route chunk.

This article explains what failed, what we learned, and the
post-link split backend now used by `pocopine build --split`.

## The naive split

The first split build emitted one shell artifact and one wasm
artifact per route:

```text
hn_bg.wasm
hn_route_home_bg.wasm
hn_route_story_bg.wasm
hn_route_not_found_bg.wasm
```

That looked like code splitting, but each route artifact was still
a complete Rust/wasm-bindgen module. In practice, every route wasm
carried its own copy of common machinery:

- wasm-bindgen glue
- web-sys/js-sys bindings
- allocator/runtime support
- pocopine mount/runtime code
- reactivity and scope infrastructure
- registry and template machinery

So the split removed unrelated route components from each artifact,
but it did not remove the duplicated runtime.

## The measurement

Fresh HN release build, monolithic:

```text
hn_bg.wasm: 708,459 raw / 279,541 gzip
hn.js:       68,676 raw
```

Fresh split build before the descriptor experiment:

```text
shell wasm:     477,251 raw / 196,855 gzip
home wasm:      486,408 raw / 199,651 gzip
story wasm:     507,737 raw / 206,474 gzip
not_found wasm: 325,303 raw / 138,164 gzip
```

The first route load became worse:

```text
monolith wasm:      708,459 raw
shell + home wasm:  963,659 raw
shell + story wasm: 984,988 raw
```

That is not a successful split. It is two standalone wasm apps
loaded for one page.

## What did work

The component ownership split did work.

Inspecting the shell artifact showed that route component templates
were gone from the shell. The shell contained the app shell and the
route manifest, but not `story-list`, `story-detail`, `hn-comment`,
or `not-found`.

That means strict route ownership is useful:

```text
src/
  shell/
  routes/
  shared/
```

It lets the compiler and CLI reason about which components belong
to boot, which belong to a route, and which are shared.

The missing piece is not ownership. The missing piece is the
runtime boundary.

## The real boundary

Pocopine components currently register through Rust function
pointers and Rust constructors:

```rust
pub struct ComponentVTable {
    pub register: fn(),
    pub mount_template: Option<ComponentMountFn>,
}
```

That is fine inside one wasm instance. It is not a browser module
ABI. A separately instantiated route wasm cannot hand the shell a
Rust function pointer or a `Scope` constructor from its own linear
memory and expect the shell runtime to call it like local code.

So the correct split cannot be:

```text
shell runtime
route wasm with Rust vtables
```

The correct split has to be:

```text
shell runtime
route descriptor / route instructions
```

In plain language:

```text
Bad split:
  each route brings another engine

Good split:
  shell brings the engine once
  routes bring page instructions
```

## The descriptor experiment

We added a tiny proof of the better model for static routes.

Instead of building `not_found` as a standalone wasm module, the
split builder now emits a descriptor-style JS route:

```text
hn_route_not_found.js
```

That file calls host functions exported by the shell:

```js
window.__pocopine_shell.pocopine_host_register_static_component(tag, html);
window.__pocopine_shell.pocopine_host_mount_static_component(outlet, tag);
```

The shell owns the runtime. The route provides serializable data:
the component tag and template HTML.

Result:

```text
old not_found route wasm: ~325,303 raw / ~138,164 gzip
new not_found route JS:        605 raw
```

This is the important proof. When the route crosses the boundary
as data instead of a standalone wasm app, the duplicated runtime
disappears.

## The post-link backend

Dynamic routes now use a different backend from the naive split.
Instead of compiling each route as a separate wasm app, Pocopine
builds one linked wasm binary with route entry functions marked as
split points. The CLI then runs a post-link wasm splitter before
`wasm-bindgen`.

That gives the splitter one whole-program dependency graph. Runtime
code, memory, tables, wasm-bindgen support, and shared route code
can stay in the main/shared modules instead of being duplicated in
every route file.

Current HN release split output:

```text
hn_bg.wasm:                    530,971 raw / 213,651 gzip
chunk_5.wasm:                   99,779 raw /  44,572 gzip
split_pocopine_route_home.wasm: 27,514 raw /  11,552 gzip
split_pocopine_route_story.wasm:37,556 raw /  13,924 gzip
hn_route_not_found.js:             605 raw
```

The first dynamic route load is now:

```text
home:  shell + shared chunk + home chunk
story: shell + shared chunk + story chunk
```

The route chunk is no longer another complete Pocopine app.

## What this still does not solve

Post-link splitting is not independent route deployment. All chunks
come from the same build and must be deployed together. A content
change can move code between main, route, and shared chunks because
the splitter re-runs reachability analysis over the whole wasm.

The descriptor path also still only handles simple static route
templates. Dynamic routes use wasm chunks; static routes can be even
smaller because they cross the boundary as serializable HTML data.

## The next steps

The route descriptor model needs to grow in layers.

First, support more template features as serializable operations:

- static attributes
- classes and styles
- text bindings
- simple property reads
- child component mounts

Then add events and state updates:

- event listener descriptors
- handler IDs
- shell-interpreted update operations
- server-function dispatch through shell-owned transport

Inner route crates are still useful: if `routes::story` depends on
an external markdown crate and `routes::home` does not, that
dependency should live in the story route crate. But inner crates
are an authoring and dependency hygiene layer. The duplicated
runtime problem is solved by post-link splitting and the shared
runtime ABI.

## Production details

Readable route names are useful during development:

```text
hn_route_home.js
hn_route_story.js
hn_route_not_found.js
```

For production, these should gain content hashes:

```text
hn.route.home-D4E5F6.js
hn.route.story-A1B2C3.js
```

Content hashes solve caching. They do not solve duplicate runtime
size. They are a separate production hardening step.

## The rule

The design rule going forward is:

> Route artifacts must not be miniature pocopine apps.

The shell should load the pocopine runtime once. Route artifacts
should provide descriptors, compiled plans, and explicit host-ABI
calls. That is the path where route splitting can become smaller
than the monolith instead of just moving bytes into more files.
