# Agent notes

This is a [pocopine](https://github.com/mambisi/pocopine) app — reactive Rust +
WebAssembly; a component is a `#[component]` struct paired with a same-named
`.poco` template.

**Dev loop:** `pocopine dev` (build + live reload), `pocopine build` (release),
`pocopine doctor` (toolchain check).

**Conventions:** register components in `main()` via
`App::new().register::<T>()`; `#[prop]` fields come from host-element
attributes; `#[handlers]` methods fire from `@event` bindings; `.poco` templates
need a single root element; styling is **Pine Stylekit** utility classes backed
by `@theme` tokens in `app.css`.

**Framework skills:** the `pocopine-skills` registry has a guide per feature
(components, directives, routing, server functions, styling, auth, storage,
sync, …). Add them to `.claude/skills/` — as a git submodule, or with
`pocopine skills install <name>` — so an agent has grounded framework knowledge.
