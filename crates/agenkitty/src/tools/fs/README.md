# Agenkitty `fs` Tools

The `fs` tool family gives an agent bounded access to a project workspace. It is
part of the open Agenkitty framework and is intended to be useful in local
framework mode without requiring the private orchestration, sandbox, or hosted
control-plane product.

The tools are filesystem-backed host tools. They are not a replacement for an
OS sandbox. They enforce Agenkitty's tool contract before each operation, while
the product/runtime layer is still responsible for container mounts, Landlock,
seccomp, cgroups, network policy, approvals, and tenant isolation when running
untrusted code.

## Tool IDs

Read-only tools are exposed by default:

| Tool | Purpose |
| --- | --- |
| `fs.search` | Search workspace text with ripgrep JSON output, falling back to a Rust searcher when `rg` is unavailable. |
| `fs.list` | List bounded directory entries with kind and size metadata. |
| `fs.read` | Read a bounded UTF-8 line range from one file. |
| `fs.stat` | Inspect path metadata without reading file contents. |
| `fs.exists` | Check whether one or more project-relative paths exist. |

Mutating tools are registered by the local runtime but are not part of the
default exposed tool set:

| Tool | Purpose |
| --- | --- |
| `fs.write` | Create or replace a UTF-8 text file. |
| `fs.append` | Append UTF-8 text to a file. |
| `fs.mkdir` | Create a directory path, including missing descendants. |
| `fs.copy` | Copy one regular file to a new destination. |
| `fs.move` | Rename or move an entry. A final symlink is moved as the symlink itself. |
| `fs.remove` | Remove a file, symlink, or empty directory. A final symlink is removed as the symlink itself. |

Every mutating descriptor is marked `side_effecting()`. Hosts should route these
tools through policy and approval before exposing them to a model.

## Public Contract

All inputs use project-relative paths. The tool layer rejects:

- empty paths;
- absolute paths;
- `..` traversal;
- paths that resolve outside the configured project root;
- secret-like path components matching `.env*`, including `.env`,
  `.env.local`, `.envrc`, and nested variants;
- write targets that are symlinks, including broken symlinks.

Existing-path operations validate both the literal relative path and the
canonical resolved path. This blocks a symlink such as `config/current` from
being used to read or report a hidden `.env` target.

The root is canonicalized when the tool is constructed. Tool outputs use
normalized slash-separated paths so model-facing output is stable across host
platforms.

## Registration

`register_fs_tools(builder, root)` registers all filesystem tools against a
canonical root.

`default_read_only_tool_ids()` returns:

```text
fs.search
fs.list
fs.read
fs.stat
fs.exists
```

`resolve_tool_ids(raw)` accepts a comma-separated list, deduplicates ids, rejects
unknown tools, and treats `none` as an empty tool list.

## Read Tools

### `fs.search`

Input:

```json
{
  "query": "target_symbol",
  "glob": "*.rs",
  "max_results": 20,
  "fixed_strings": true,
  "case_sensitive": true
}
```

Behavior:

- defaults to fixed-string, case-sensitive search;
- clamps `max_results` to `1..=100`, defaulting to 20;
- uses `rg --json --color never -- <query> .` from the project root;
- parses ripgrep JSON events rather than colon-delimited output, so filenames
  containing `:` are reported correctly;
- kills the ripgrep child when enough results have been collected;
- ignores ripgrep exit code 1 as "no matches";
- falls back to `grep_searcher` plus `ignore::WalkBuilder` when ripgrep is not
  installed;
- filters secret-like result paths after the search engine emits them, so an
  explicit glob such as `.env*` cannot leak secret file contents.

Output:

```json
{
  "hits": [
    {
      "path": "./src/lib.rs",
      "line": 12,
      "column": 5,
      "text": "pub fn target_symbol() {}"
    }
  ],
  "truncated": false,
  "engine": "ripgrep"
}
```

### `fs.list`

Input:

```json
{
  "path": ".",
  "max_entries": 80,
  "include_hidden": false
}
```

Behavior:

- defaults to the root directory when `path` is omitted;
- clamps `max_entries` to `1..=300`, defaulting to 80;
- skips dotfiles unless `include_hidden` is true;
- always skips secret-like entries even when hidden files are included;
- sorts directories first, then files, symlinks, and other entries.

Output entries include `name`, `path`, `kind`, and optional `size_bytes`.

### `fs.read`

Input:

```json
{
  "path": "src/lib.rs",
  "start_line": 1,
  "max_lines": 80
}
```

Behavior:

- requires the target to be a regular file;
- clamps `max_lines` to `1..=200`, defaulting to 80;
- reads incrementally through a `BufReader` instead of loading the whole file;
- caps total text read at 1 MiB;
- rejects invalid UTF-8 as a validation error instead of returning raw binary
  bytes;
- strips trailing `\n` and `\r\n` from returned line text;
- sets `truncated` when the byte cap or line cap stops output early.

### `fs.stat`

Input:

```json
{
  "path": "src/lib.rs"
}
```

Behavior:

- resolves and validates the target before metadata lookup;
- returns `kind`, optional `size_bytes`, and `readonly`;
- follows symlinks because existing-path validation canonicalizes the target.

### `fs.exists`

Input:

```json
{
  "paths": ["src/lib.rs", "missing.txt"]
}
```

Behavior:

- requires at least one path and at most 100 paths;
- validates each literal path before checking it;
- returns `false` for missing paths and paths that resolve outside the root;
- returns a policy error when a path directly or indirectly targets `.env*`.

## Mutation Tools

Mutation tools return:

```json
{
  "path": "src/generated.rs",
  "operation": "write",
  "changed": true,
  "bytes": 128
}
```

Notes:

- `fs.write` and `fs.append` are simple text primitives, not structured edit
  tools. Prefer `patch.*` for multi-file, previewable edits.
- `fs.copy` is intentionally file-only at this stage. Directory copy needs a
  separate recursive policy and symlink plan.
- `fs.mkdir` validates the nearest existing ancestor so nested paths work while
  symlink ancestors are still blocked.
- `fs.move` and `fs.remove` use `symlink_metadata` for final entries so final
  symlinks are moved or removed as symlinks instead of following their targets.
- `fs.remove` removes files, symlinks, and empty directories. Recursive removal
  is not exposed by this tool.

## Path-Policy Helpers

The shared implementation lives in `common.rs`.

| Helper | Used by | Purpose |
| --- | --- | --- |
| `canonical_root` | all tools | Canonicalize the workspace root once at construction. |
| `validate_relative_path` | all path inputs | Reject empty, absolute, and parent-traversal paths. |
| `reject_secret_path` | all tools | Deny any path component whose lowercase name starts with `.env`. |
| `canonical_existing_path` | read/list/stat/copy source | Resolve an existing target, enforce root containment, and re-check resolved secret paths. |
| `checked_target_path` | write/append/copy destination/move destination | Validate a write target and reject final symlinks, including broken symlinks. |
| `checked_descendant_target_path` | mkdir | Allow missing descendants while validating the nearest existing ancestor. |
| `checked_existing_entry_path` | move/remove | Validate an existing entry and optionally allow the final component to be a symlink. |

The implementation deliberately uses `symlink_metadata` when it needs to inspect
the path entry itself and `canonicalize` when it needs to reason about the
resolved target.

## Security Boundaries

The `fs` tools provide framework-level guardrails:

- model-facing path validation;
- secret-file denial;
- symlink escape checks;
- bounded reads and searches;
- side-effect metadata for policy routing.

They do not provide kernel-level confinement. A hostile process with concurrent
write access to the workspace can still create time-of-check/time-of-use races
between validation and later filesystem calls. Production sandboxing should
combine these tools with a restricted workspace mount, no broad host filesystem
mounts, Landlock or container filesystem rules, seccomp, resource limits, and
approval policy.

If Agenkitty later needs the filesystem tool layer itself to become a stronger
local security boundary, prefer descriptor-relative Linux APIs such as
`openat2(2)` with `RESOLVE_BENEATH`/`RESOLVE_NO_SYMLINKS`, `O_NOFOLLOW`,
`O_CLOEXEC`, and explicit `renameat2(2)`/`unlinkat(2)` flows. That should be a
separate hardening pass because it changes portability and implementation
complexity.

## Tests

The focused filesystem test gate is:

```sh
cargo test -p agenkitty tools::fs
```

Important coverage:

- absolute path and parent traversal rejection;
- `.env*` direct and resolved-target denial;
- symlink escape denial;
- broken-symlink write target denial;
- secret search filtering in both ripgrep and Rust fallback engines;
- filenames containing colons in ripgrep JSON output;
- binary/invalid UTF-8 read rejection;
- bounded read/list/search truncation;
- nested `fs.mkdir`;
- side-effecting descriptors for mutating tools;
- final-symlink behavior for move and remove.

Before pushing, also run the workspace gates required by the repository
`AGENTS.md`:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --target wasm32-unknown-unknown
cargo build --workspace --target wasm32-unknown-unknown
cargo test --workspace
```

## Prior Art

- Vercel Eve: filesystem-first agent project layout and inspectable agent
  folders.
- Astro Flue: sandbox-aware local, virtual, and remote-container filesystem
  choices.
- Earendil Pi: illustrates why Agenkitty should not rely on external convention
  alone for filesystem/process/network boundaries.
- MCP filesystem server: closest public API shape for allowed directories,
  read/write/list/move/search/metadata, and tool safety hints.
