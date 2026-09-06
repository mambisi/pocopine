# Translation authoring

Use the Pocopine CLI built from this checkout. The application build discovers
active browser, server and worker references, generates the typed `t` API, and
publishes fingerprinted browser catalogs.

Configure `pocopine.toml` and create a flat JSON file per configured locale:

```toml
[locale]
default = "en"
locales = ["en", "fr"]
routing = "none"
```

```json
{
  "common.title": "Welcome",
  "common.welcome": "Hello {name}, welcome to Pocopine",
  "common.items": "{count, plural, one {# item} other {# items}}"
}
```

The example above goes in `locales/en.json`. Put French translations under the
same keys in `locales/fr.json`; an omitted key falls back through configured
CLDR parents and finally the default locale. Translations must preserve the
default message's named arguments, argument types, and rich element contract.

Include the generated API once at the application crate root:

```rust,ignore
pocopine::locale::include_translations!();
```

In templates, `$t.common.title` is shorthand for a message without arguments.
Use a static string key and positional values when arguments are present:

```html
<h1 pp-text="$t('common.welcome', name)"></h1>
<p pp-text="$t('common.items', count)"></p>
<input :placeholder="$t.common.title">
```

Arguments follow the catalog argument names in alphabetical order, matching the
generated Rust signature. For example, a message with `{first}` and `{last}`
takes `$t('common.full_name', first, last)`. Changing a value or the committed
browser language updates its bindings.

Rust passes the locale explicitly:

```rust,ignore
let greeting = t::common::welcome(locale, &name);
let summary = t::common::items(locale, 3_u64.into());
```

Browser startup initializes the generated cache, then awaits catalog boot
before mounting components:

```rust,ignore
t::initialize(t::locales())?;
let ui = pocopine::locale::client::boot(t::catalogs()?).await?;
// Mount the application now.
ui.set_locale("fr".parse()?).await?;
```

The HTML shell starts the initial catalog download alongside wasm. A later
selection commits only after its catalog validates; a failed request preserves
the current language. Repeating the selection retries it. `lang` and `dir`
follow the committed locale.

On the host, call `t::initialize()` before accepting requests or processing
jobs. The server middleware supplies `Extension<Locale>` to server functions;
workers pass the recipient's saved locale. See the complete
[browser/server example](../examples/locale/README.md) and the framework error
and worker contracts in [RFC-120](../rfcs/rfc-120-i18n.md#55-server-errors-and-messages-to-users).

## Rich messages

Catalogs can reorder existing elements with positional placeholders:

```json
{
  "common.terms": "Read <0>the terms</0> before continuing."
}
```

```html
<p pp-text="$t.common.terms"><a href="/terms"></a></p>
```

Keep placeholder elements empty in the template; their text comes from the
catalog. Attributes, listeners and element identity stay with the application.
Each branch of the message must use the same elements exactly once. Rich
messages require a direct translation in `pp-text`; attributes and ordinary
string expressions accept plain text messages.

## Commands

`i18n` is an alias for `locale`. `--path` selects the application, and
`--release` selects its release source configuration. Both options can appear
before or after the subcommand.

```sh
pocopine locale check --path examples/locale
pocopine locale check --path examples/locale --deny-warnings
pocopine i18n stats --path examples/locale --json
```

`check` validates ICU syntax, default keys, module ownership, arguments, rich
elements and fallback. Build errors fail the command. `--deny-warnings` also
fails on missing translations and orphaned keys, which is useful for a strict
translation CI gate. `stats` reports direct coverage, fallback and orphaned
entries against the active set of browser and host messages.

After adding a static key in application code:

```sh
pocopine locale extract --path my-app
```

Extraction adds missing default keys with empty skeleton values and creates
any missing configured catalog files. Fill the new messages and ICU arguments
before building. Existing message text stays intact. JSON remains a strict
string map; source-location notes are stored in `locales/<default>.sources.json`
and included as notes in XLIFF exports. Repeated extraction is deterministic.

Merge a translator's flat JSON update into a configured locale:

```sh
pocopine locale merge --path my-app --locale fr --input fr-update.json
```

The update replaces matching entries and preserves untouched ones. Unknown
non-default keys and invalid argument or element contracts fail before the
catalog file is written. Output is sorted by key.

## XLIFF exchange

```sh
pocopine locale export --path my-app --locale fr --xliff --output fr.xlf
pocopine locale import --path my-app --input fr.xlf
```

The exchange uses [XLIFF 2.0](https://docs.oasis-open.org/xliff/xliff-core/v2.0/os/xliff-core-v2.0-os.html)
with one complete MF1 message per unit and resegmentation disabled. Message
keys are unit names, and ICU syntax—including `<0>` placeholders—is escaped XML
text. Source locations are notes. Files and groups are accepted on import;
messages must remain in a single segment with plain text source/target content.
Unsupported inline XML codes, changed source copy, conflicting language
metadata, duplicate message keys, and invalid translation contracts fail
explicitly. A missing target leaves that translation untouched.

Imports compare each exported source message with the current default catalog.
If the application copy changed, export it again before importing. Input is
bounded to 16 MiB; DTDs and external entities are disabled. XML whitespace and
character references preserve message text, including carriage returns.

## Editor support

The Rust language server (`pocopine lsp --stdio`) completes `$t` paths and static
call keys from the default catalog. Selecting an argument-bearing message from
a path completion inserts a call with argument snippets in signature order.
Hover shows the default message, argument types, and any rich element count.
Open catalog text takes precedence over disk when the editor sends it to the
server; otherwise the catalog is read fresh for each request.

Locale-prefixed navigation, persistence, configured ICU data slicing, and SSR
integration are still tracked in the [implementation checklist](locale-implementation.md).
