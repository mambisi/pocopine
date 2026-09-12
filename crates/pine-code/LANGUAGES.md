# Language packages and custom grammars

`pine-code` uses Tree-sitter in a dedicated module worker. The editor component
selects a registered language by ID; the worker owns the parser and cached syntax
tree. The default model-only crate has no parser dependency.

Enable the view and only the language packages your application needs:

```toml
pine-code = { workspace = true, features = ["view", "lang-python", "lang-javascript"] }
```

Available package features are `lang-rust`, `lang-json`, `lang-python`, and
`lang-javascript`. They expose `languages::rust()`, `json()`, `python()`, and
`javascript()`. `syntax` alone exposes the registry and host-testable parser
without including a language grammar. `plain` needs no registration.

Use one registry factory in both browser entrypoints:

```rust
use pine_code::{LanguageError, LanguageRegistry, languages};
use pine_code::client::{
    LanguageWorkerConfig, configure_languages, start_language_worker,
};
use wasm_bindgen::prelude::*;

fn editor_languages() -> Result<LanguageRegistry, LanguageError> {
    let mut languages = LanguageRegistry::new();
    languages.register(languages::python())?;
    languages.register(languages::javascript())?;
    Ok(languages)
}

#[wasm_bindgen]
pub fn code_languages() -> Result<(), JsValue> {
    let languages = editor_languages().map_err(|e| JsValue::from_str(&e.to_string()))?;
    start_language_worker(languages)
}

#[wasm_bindgen]
pub fn start(module_url: String) -> Result<(), JsValue> {
    let languages = editor_languages().map_err(|e| JsValue::from_str(&e.to_string()))?;
    configure_languages(
        LanguageWorkerConfig::new(module_url, "code_languages"),
        &languages,
    )?;
    // Register PineCodeEditor and your components on the Pocopine App, then run it.
    Ok(())
}
```

Keep DOM mounting in `start`, not in an unconditional `#[wasm_bindgen(start)]`:
the worker imports the same module and initializes its own WASM instance. The
main entrypoint retains language metadata; only the worker compiles queries and
parses source. Grammar pointers are never sent between WASM instances.

In the application's **index.html**, load the module and pass its URL:

```html
<script type="module">
  import init, * as app from "/pkg/my_app.js";
  await init();
  app.start(new URL("/pkg/my_app.js", location.href).href);
</script>
```

Replace `my_app` with the application's wasm-pack output name. The Pocopine CLI
rewrites both references to the same content-hashed module. It must be the
built module URL, so the page and worker use matching grammar/code versions.
The library creates and starts the worker, handles messages/errors/timeouts,
revokes its bootstrap Blob URL, and terminates it when the editor is disposed
or falls back to plain presentation. CSP must allow this application's module
and `worker-src blob:`. Worker startup failure reports `PlainFallback` and leaves
editing available; an explicit load or language change retries it.

Then select the language normally:

```html
<pine-code-editor language="python" line-numbers aria-label="Python source">
</pine-code-editor>
```

Bind `language` to application state to switch languages. Selection, document,
revision, and undo history remain owned by the editor. Python/Rust default to
four-space indentation; JSON/JavaScript use two. The `indent` prop overrides
the language's indentation unit.

## Custom registration

A developer can depend on any compatible Tree-sitter grammar crate. For example,
with `tree-sitter-json` as an application dependency:

```rust
use pine_code::TreeSitterLanguage;

languages.register(
    TreeSitterLanguage::new("config", tree_sitter_json::LANGUAGE)
        .highlights(include_str!("queries/config-highlights.scm"))
        .indent_unit("    "),
)?;
```

An example `config-highlights.scm`:

```scheme
(pair key: (string) @property)
(number) @number
(true) @constant
(false) @constant
```

Select it with `language="config"`. A new programming language supplies its
own generated grammar and queries through the same API. Registration checks
IDs, duplicates, indentation, nonempty queries, and grammar ABI compatibility.
Invalid queries report an `InvalidLanguage` presentation error from the worker
and remain plain until an explicit load or language change, avoiding repeated
worker startup for the same invalid configuration.
Unknown IDs report `UnknownLanguage`; neither condition rejects text edits.

Supported color categories are keyword, string, number, literal/constant,
comment, punctuation, lifetime/label, function, type/constructor, property,
variable, operator, attribute, tag, and escape. Dotted names fall back to their
base category; `string.special.key` maps to property. CSS variables such as
`--pine-code-function` and `--pine-code-property` customize theme colors.
Nested captures override parent captures; later patterns win for identical
ranges. Returned ranges are validated against UTF-8 boundaries before rendering.

This API consumes standalone highlight queries. Text predicates such as
`#match?` and `#eq?` are supported. Patterns with property predicates (`#is?`,
`#is-not?`, including local-scope-dependent rules) are omitted; ordinary syntax
patterns still apply. Other custom predicates are rejected. Locals queries,
embedded-language injections, semantic tokens, and LSP are not implemented.
Do not assume an arbitrary editor's complete query collection is compatible.

## Build requirements and bounds

Tree-sitter's C runtime and grammar sources require Clang with the wasm32 target
and llvm-ar. Install those development tools or set
`CC_wasm32_unknown_unknown` and `AR_wasm32_unknown_unknown` to their executables.
The Pocopine CLI discovers `tree-sitter-language` in the application's dependency
graph and supplies its upstream WASM headers automatically:

```sh
pocopine build --path examples/code-editor --release
pocopine dev --path examples/code-editor
```

For direct library Cargo/wasm-pack commands, set
`CFLAGS_wasm32_unknown_unknown=-I<tree-sitter-language>/wasm/include`; locate the
crate with `cargo metadata --format-version 1`. No local grammar fork or custom
libc implementation is required. Each custom grammar still needs a verified
WASM build, especially if it has an external scanner.

One request per editor is in flight. Edits are coalesced and the newest committed
snapshot is sent next; Tree-sitter updates its retained tree from the UTF-8 diff
between those snapshots. Responses are fenced by document revision, language,
and generation (including loads and view recovery). Composition defers DOM
patching. Rendering is sliced into 4 ms frames and capped at 8,000 spans and
8 KiB per highlighted line. Span overflow remains plain until load/language
change. Parsing is additionally capped at 1 MiB, with bounded parse/query work,
64,000 captures, and 32 MiB of capture-paint work. Worker startup has a 30-second
timeout and requests have a 10-second timeout. These are recovery ceilings,
not latency targets.

The [code-editor example](../../examples/code-editor/src/client/mod.rs) includes
all four packages, a custom configuration grammar, and a native language picker.
