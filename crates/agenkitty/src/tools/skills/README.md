# Skill tools (`skill.*`)

Progressive disclosure of [Agent Skills](https://agentskills.io) — folders of
`SKILL.md` instructions plus bundled files — per RFC-121. The
`agenkitty-skills` crate owns discovery, validation, sanitization, and path
confinement; the `agenkitty skills` subcommand (`validate` / `list` /
`inspect` / `index`) exposes the same loader from the CLI; this family adds
the host seams.

## Tools

- `skill.use` — load a skill's full instruction body by name (level 2) and
  unlock `skill.read` for its bundled files. Refuses
  `disable-model-invocation` skills: this tool *is* the model-invocation
  path; hosts invoke through the library.
- `skill.read` — a byte-windowed read of an activated skill's bundled file
  (level 3), confined to the skill directory with the fs family's
  double-canonicalization + secret-path discipline.

There is deliberately no `skill.list`: the index in the system prompt (level
1, rendered by `SkillRuntime::system_prompt_part`) is the list.

## Discovery and configuration

Roots come from the project's `[skills]` config section (default:
`.agents/skills` then `.claude/skills`, project-relative; earlier roots win
name collisions). `enabled = false` empties the family without unregistering
it. Budgets (`index_byte_budget`, `entry_byte_cap`, `body_byte_limit`) bound
every surface.

## Security contract (RFC-121 S1–S8)

- Skills are instructions from disk; the trust boundary is the configured
  roots (project roots sit inside the same workspace trust as `AGENTS.md`).
- Everything that reaches a prompt or log is ANSI/control-stripped and
  byte-bounded in the library; index descriptions run through the shared
  secret classifier and redact on a match.
- Reads are root-confined; symlink escapes and secret-file paths refuse.
- `allowed-tools` frontmatter never widens anything — it is surfaced to the
  host as an attenuation-only hint.
- Visibility is attenuation-only: a subagent context or a
  `SkillRuntime::fork` may only narrow the view, and out-of-view names are
  indistinguishable from nonexistent ones.
- Nothing executes. Scripts a skill mentions run through `process.*` and its
  sandbox; `hooks`/`shell` frontmatter is parsed but loudly ignored.
- Logging (RFC-069): name, digest prefix, sizes, outcome — never bodies,
  descriptions, or arguments.
