# Agenkitty Patch Tools

Patch tools apply structured workspace edits through a bounded format instead
of letting a model write arbitrary shell commands. They sit beside the `fs`
tools: `fs.write` handles simple file writes, while `patch.*` handles
multi-file edits with validation, summaries, previews, and rollback on failed
apply.

## Tool Set

| Tool | Side effects | Purpose |
|------|--------------|---------|
| `patch.preview` | read-only | Validate a structured patch and return affected files plus a bounded diff. |
| `patch.apply` | side-effecting | Validate, stage, and apply the same structured patch inside the workspace root. |

Both tools accept:

```json
{
  "patch": "*** Begin Patch\n*** Add File: notes/todo.txt\n+one\n*** End Patch\n"
}
```

## Patch Format

A patch must start with `*** Begin Patch` and end with `*** End Patch`.

Supported operations:

```text
*** Add File: path/to/file.txt
+new line
+another line

*** Update File: path/to/file.txt
@@ -10,2 +10,2 @@
 context
-old
+new

*** Update File: old/path.txt
*** Move to: new/path.txt
@@
-old
+new

*** Delete File: path/to/file.txt
```

Rules:

- `Add File` lines must all begin with `+`.
- `Update File` hunks use context (` `), addition (`+`), and removal (`-`)
  lines. Blank patch lines are treated as blank context lines.
- Hunk headers are optional, but line hints are used when present to disambiguate
  repeated context.
- Move is represented as `Update File` plus `*** Move to: ...`.
- A single patch may touch up to 50 files and 200 hunks.
- The patch input is capped at 256 KiB.

## Safety Contract

- Paths must be relative to the configured workspace root.
- Secret-like paths are rejected using the shared fs path checks.
- Symlink targets and symlink parents are rejected for patch writes.
- Existing source files are canonicalized and must remain under the workspace
  root.
- A patch cannot edit the same normalized path more than once.
- `patch.preview` never writes.
- `patch.apply` stages writes beside their destination, creates backups for
  update/delete/move operations, and rolls back already-applied changes if a
  later staged change fails.

## Output

Outputs include:

- `applied`: `false` for preview, `true` for apply.
- `files`: normalized paths, operation kind, destination for moves, hunk counts,
  matched hunk locations, additions, and deletions.
- `diff`: preview-only, bounded to 16 KiB or 400 lines with a `truncated` flag.

## Registration

`register_patch_tools(builder, root)` registers both tools for a canonicalized
workspace root. `known_patch_tool_ids()` returns `patch.preview` and
`patch.apply`; `patch.apply` is marked side-effecting and is not part of the
default read-only tool set.

## Tests

The regression suite covers path escape rejection, secret-path rejection,
symlink safety, duplicate path detection, bounded preview diffs, stale patch
rollback, CRLF handling, move/delete/add/update behavior, and tool-id
resolution.
