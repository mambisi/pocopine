# Generated locale API contract

Run `python3 tools/check-locale-codegen.py` from the repository root with
Chrome and a compatible chromedriver available (`CHROMEDRIVER` may name it). This is an
isolated Rust library fixture, so its intentional compile-fail features do not
participate in ordinary workspace builds. The verifier seeds its ignored lock
file from the workspace lock and uses the workspace target directory.

The fixture builds the actual generated API for host and wasm. It checks typed
arguments (including names that overlap Rust keywords), namespace collisions,
missing keys, host-only function pruning, explicit catalog initialization and
default browser dependencies. Its wasm test also installs a catalog through the
core locale controller and reads it through generated functions, then formats
client network-error copy using a generated function. Core is a test dependency
and does not contribute to this fixture's release binary.
Release exports keep translation calls reachable
while the verifier checks that catalog text and static keys stay out of wasm.
The reported size is for the entire fixture, not the marginal locale cost.

`queued-messages.json` represents application-owned delivery jobs with recipient
locale, semantic message kind and typed inputs. Both catalog builds consume the
same file. `catalog-update` adds an earlier sorted key and changes wording, so
build IDs and dense IDs change. The worker still selects the correct generated
function after a durable retry round-trip; an appointment also preserves its
explicit timezone. Retaining exact wording across deployments would require
persisting rendered content or its catalog version.

With `server-integration`, an in-process HTTP test drives actual `#[server]`
routes: locale precedes auth, rejected guards and invalid bodies use the
generated catalogs, application messages stay intact, and concurrent SSE
responses keep separate locales. No listener or message delivery is started.
Browser UI, outgoing RPC metadata and application build wiring have separate
acceptance checks in `docs/locale-implementation.md`.

`template-integration` compiles a real component against the generated API. Its
headless Chrome test covers `$t` paths and calls in text, attributes,
interpolation, conditionals and keyed rows; variables, plurals and safe text
insertion; recovery after catalog activation; and rich element reordering and
reparenting with focus, refs, listener identity and teardown preserved. It runs
with the default Intl and optional strict-parity formatting backends.

The template compile-fail features reject unknown/dynamic keys, incorrect
arity, rich attribute messages and the removed `pp-t` directive. A separate
release export mounts the component, keeping its installers reachable while the
verifier audits key/message elimination, including source HTML left in lifted
bodies. That fixture includes core/component code, so its whole-binary size is
reported separately from the leaf generated-API fixture.
