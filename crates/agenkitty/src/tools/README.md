# Agenkitty Core Tools

This directory tracks public framework documentation for Agenkitty's built-in
tool families. These are framework contracts and local host behaviors. Hosted
orchestration, cluster control planes, tenant policy, billing, and production
infrastructure stay outside this public repo.

Core tool families:

- `fs`: inspect, read, search, and mutate bounded workspace files.
- `patch`: apply structured edits with preview, validation, and rollback hooks.
- `process`: run local commands through explicit sandbox and policy controls.
- `network`: fetch bounded remote resources with allowlists and budgets.
- `memory`: persist project/session notes without leaking private host storage.
- `session`: expose thread/run context and continuation state.
- `secrets`: request scoped secret handles without exposing raw values by default.
- `mcp`: connect to external MCP servers and surface their tools through Agenkitty policy.
- `skills`: progressive disclosure of Agent Skills (agentskills.io) folders via the `agenkitty-skills` loader.

Tool folders keep the public README for the shipped tool family. Completed
plans, build notes, and speculative tool-family docs do not live here.

Not planned as built-in tool families:

- A dedicated typed repository tool: agents can use the process and patch tools
  for repository workflows; research did not show a first-party typed git tool
  as a harness requirement.
- A bespoke authoring/registration family for agent-made helpers: one-shot
  helpers are ordinary workspace scripts run through `process.*`; durable
  schema-typed tools are MCP servers connected through `mcp.*`. There is no
  custom registry or lifecycle layer between those paths.
