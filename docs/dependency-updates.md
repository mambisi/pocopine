# Dependency updates

[Renovate](https://docs.renovatebot.com/) opens dependency update PRs using
[`renovate.json`](../renovate.json). Maintainers review and merge each PR after CI.

## Activation

Install the [Mend Renovate GitHub App](https://github.com/apps/renovate) and grant
it access to `mambisi/pocopine`. Merge the configuration into `main`; a
configuration file alone does not install the app. Once Renovate has run, its
Dependency Dashboard issue lists pending updates and any configuration errors.
There is no repository token or scheduled Actions workflow to maintain.

## Update policy

- Check Cargo dependencies and their lockfile, npm manifests and lockfiles,
  GitHub Actions, Dockerfile and Compose images, and pinned Rust toolchains.
  Examples are included.
- Open routine updates on Mondays between midnight and 09:00, Asia/Dubai time,
  with at most five concurrent PRs and two new PRs per hour.
- Group related packages using Renovate's recommended presets. Group non-major
  JavaScript changes together and the wasm-bindgen family with its CLI pins in
  CI and the build container.
- Require approval in the Dependency Dashboard before opening major upgrades.
  Pinned nightly updates also require approval and use a monthly window on the
  first day of the month, midnight to 09:00. The toolchain file and CI pins belong
  to the same update group.
- Keep automerge disabled. Generated bundles, build outputs, vendor directories,
  and temporary checkouts are excluded.

Whole-lockfile maintenance is disabled because it can advance wasm-bindgen
independently of the installed CLI pins. Individual Cargo updates still update
`Cargo.lock`. When reviewing a wasm-bindgen PR, check that the CLI version in CI
and the build container matches `wasm-bindgen` in the resulting lockfile, then
require the browser tests to pass.

## Configuration changes

Run Renovate's [configuration validator](https://docs.renovatebot.com/config-validation/)
from the repository root before changing the bot policy:

```sh
npx --yes --package renovate -- renovate-config-validator --strict
```

The custom regex manager tracks `tool: crate@version` and
`cargo install --locked crate@version` in workflow files and the canonical build
Dockerfile. Keep those forms when editing pinned Cargo tools so Renovate can
continue to detect them.

The tool matcher uses `semver` versioning so each installed version is an exact
pin. Cargo dependency versioning would interpret `0.2.95` as a compatible range
and skip later `0.2.x` releases of the installed binary.
