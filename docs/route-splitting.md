# Route splitting without shipping another runtime

Pocopine is experimenting with route-level code splitting. The
first implementation produced separate wasm files per route, but
the numbers showed an important problem: a route wasm file is not
automatically a small route chunk.

This article explains what failed, what we learned, and the
direction we are moving toward.

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

## What this does not solve yet

This does not yet make every route small.

The current descriptor path only handles simple static route
templates. Dynamic routes like HN's `home` and `story` still build
as route wasm artifacts because they have state, bindings, events,
handlers, and server-function calls.

Current status:

```text
shell wasm:       ~479 KiB raw
home route wasm:  ~486 KiB raw
story route wasm: ~508 KiB raw
not_found route:  605 B JS
```

So this is not the final split system. It is the first proof that
the final system must be descriptor/ABI-based.

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

Only after that should inner route crates become the main focus.
Inner crates are still useful: if `routes::story` depends on an
external markdown crate and `routes::home` does not, that
dependency should live in the story route crate. But inner crates
do not solve duplicated runtime by themselves. The shared runtime
ABI does.

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
