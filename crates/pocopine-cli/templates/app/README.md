# pocopine-app

A [pocopine](https://github.com/mambisi/pocopine) app, scaffolded with
`pocopine create`.

## Develop

```bash
pocopine dev      # build + serve with live reload
pocopine build    # release build (wasm + Stylekit CSS)
pocopine doctor   # check your toolchain
```

Open the URL `pocopine dev` prints, then edit `src/WelcomeApp.poco`.

## Layout

```
Cargo.toml          package + pocopine dep + Stylekit config
rust-toolchain.toml pinned nightly + the wasm32 target
app.css             Pine Stylekit @theme tokens (compiled to pkg/stylekit.css)
index.html          host page; links the CSS, mounts <welcome-app> under [pp-app]
src/lib.rs          #[component] structs + #[handlers] + the wasm entrypoint
src/*.poco          component templates (paired to structs by filename)
```

A component is a Rust `#[component]` struct + a sibling `Name.poco`. `#[prop]`
fields come from host-element attributes; `#[handlers]` methods fire from
`@event` bindings; `pp-text` / `{{ … }}` read state; `<slot>` projects children.
Styling uses Pine Stylekit utility classes (edit the `@theme` tokens in
`app.css` to rebrand). See `AGENTS.md` for agent skills.
