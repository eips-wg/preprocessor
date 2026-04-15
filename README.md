build-eips
==========

Build system for linting and rendering Ethereum Improvement Proposals ([EIPs] /
[ERCs]).

## Prerequisites

`build-eips` requires a few runtime dependencies, available from wherever you
get your software:

- git
- libgit2
- openssl
- just[^2]
- [zola](https://github.com/getzola/zola/tree/next)[^1]

[^1]: Requires at least commit [`ead17d0a3`] for full functionality.
[^2]: Required for the generated local task surface.

[`ead17d0a3`]: https://github.com/getzola/zola/commit/ead17d0a3a20bfb67043a076c061b35ae6b6ddea

## Installation

### Pre-compiled Binaries

Pre-compiled binaries for Ubuntu, Windows, and macOS are available from
[GitHub Releases].

[GitHub Releases]: https://github.com/ethereum/build-eips/releases

### From Source

If you're feeling particularly adventurous, you can install the latest version
of `build-eips` like so:

```bash
cargo install --git https://github.com/ethereum/build-eips.git
```

[EIPs]: https://github.com/ethereum/EIPs/
[ERCs]: https://github.com/ethereum/ERCs/


## Usage

1. Clone either [`ethereum/EIPs`] or [`ethereum/ERCs`], and change directory
   into it.
1. Modify whatever proposal you'd like.
1. Build the project. You can use:
    - `build-eips check` to run the runtime site verification path.
    - `build-eips build` to create an on-disk bundle of HTML, ready to be
      deployed.
    - `build-eips serve` to launch the runtime dev server locally.
    - `build-eips preview` to serve the last built output without rebuilding.
1. Run explicit editorial validation when you need proposal-targeted `eipw`
   checks.

## Local Overrides

Local overrides add a local path layer and workspace config support without
changing the default CI-oriented path unless you opt into those settings.

### Explicit local overrides

Use these flags to point the build at local sibling repositories and an
out-of-tree build root:

```bash
build-eips --staging \
  -C /work/EIPs-project/EIPs \
  --theme-path /work/EIPs-project/theme \
  --other-repo-path /work/EIPs-project/ERCs \
  --build-root /work/EIPs-project/.local-build/EIPs \
  check
```

Available overrides:

- `--theme-path <path>`
- `--other-repo-path <path>`
- `--build-root <path>`
- `--config <path>`
- `--profile <name>`

The local theme override also reuses that checkout's `config/eipw.toml`.

### Workspace init

If `build-eips` is already installed, you can bootstrap a local workspace from
inside `EIPs/` or `ERCs/`:

```bash
build-eips workspace init /work/EIPs-project
```

By default this:

- clones the missing sibling content repo
- clones `theme`
- creates `.local-build/`
- writes `.build-eips.toml`

For platform development, you can additionally clone `preprocessor` and `eipw`:

```bash
build-eips workspace init /work/EIPs-project --platform-dev
```

After init, daily commands can run from inside `EIPs/` or `ERCs/` without
repeating the local path flags:

```bash
cd /work/EIPs-project/EIPs
build-eips check
build-eips build
build-eips serve
```

`--profile parity` is available for one-off profile selection from the local
workspace config.

### Current Constraints

- WG CI parity still means `--staging`
- clean worktrees are still required
- dirty working tree support requires the explicit dirty mode described below

## Local Workspace Workflow

The local workspace workflow adds the generated task surface and the
front-door bootstrap path while keeping `.build-eips.toml` user-owned.

### Refresh generated helpers

Once `workspace init` has created `.build-eips.toml`, refresh the generated
workspace helper files from the workspace root:

```bash
cd /work/EIPs-project
build-eips workspace refresh
```

This generates or refreshes `/work/EIPs-project/justfile` without rewriting
`.build-eips.toml`.

### Doctor checks

Validate the local setup at any point with:

```bash
build-eips workspace doctor
```

Doctor checks the workspace config, expected local repos, required tools, and
whether the generated helper files are current.

### Daily `just` commands

After `workspace refresh`, you can use the generated `justfile` from inside
`EIPs/` or `ERCs/`:

```bash
cd /work/EIPs-project/EIPs
just check
just build
just serve
just parity-build
```

`just` will find the workspace-root `justfile` by walking up from the current
content repo. The generated recipes pass the invoking content repo back to
`build-eips` with `-C`, so run them from inside `EIPs/` or `ERCs/` rather than
from the workspace root.

### Front-door `scripts/dev-setup`

If you cloned `EIPs/` or `ERCs/` first, use that repo's checked-in
`./scripts/dev-setup` helper. It locates or installs `build-eips` and `just`,
runs `workspace init`, refreshes the generated helpers, and prints the next
useful commands.

## Dirty Workflow

Dirty workflow adds an explicit local-only mode for the active content repo.

### Dirty profile

Fresh workspaces now include a `dirty` profile in `.build-eips.toml`, so the
documented daily dirty loop works immediately:

```bash
cd /work/EIPs-project/EIPs
build-eips --profile dirty check
build-eips --profile dirty build
build-eips --profile dirty serve
```

The generated `justfile` also exposes:

```bash
just dirty-build
just dirty-serve
```

`build-eips --profile dirty serve` now performs the expensive runtime
preparation once at startup, then watches the real active content repo and
mirrors tracked changes into the materialized repo that Zola is serving from.
Zola runs in fast serve mode for in-session rebuilds, so tracked edits become a
real live local dev loop without restarting the command.

### Ad hoc override

You can also enable the same dirty materialization path without relying on a
profile:

```bash
build-eips --allow-dirty check
build-eips --allow-dirty build
build-eips --allow-dirty serve
```

### Dirty-mode limits

- dirty mode is opt-in and non-parity
- the clean/default path is unchanged
- only the active content repo is materialized dirty
- sibling repo and theme still follow the clean workspace/profile rules
- untracked files in the active content repo are currently ignored
- clean `build-eips serve` remains a clean runtime serve path and does not sync
  working-tree edits during the session
- tracked deletions are mirrored into the materialized repo, but served route
  invalidation under Zola fast serve remains best-effort

## Site Commands and Editorial Commands

The command surface separates site work from targeted editorial validation.

### Site Commands

These commands no longer invoke `eipw`:

```bash
build-eips check
build-eips build
build-eips serve
```

The generated `justfile` keeps the runtime, parity, and dirty runtime recipes:

```bash
just check
just build
just serve
just parity-build
just dirty-build
just dirty-serve
```

### Parity Profile

Use the parity profile when you want to test the active content repo against
remote sibling and theme inputs instead of local checkouts:

```bash
build-eips --profile parity check
build-eips --profile parity build
build-eips --profile parity preview
```

### Editorial Commands

Use the explicit editorial surface when you want targeted `eipw` validation:

```bash
build-eips editorial lint content/07949.md
build-eips editorial lint --working-tree
build-eips editorial lint --against-upstream --format github
build-eips editorial build --batch /work/EIPs-project/editor-batch.txt
```

Selector modes are mutually exclusive:

- explicit repo-relative proposal paths
- `--batch <path>` with one repo-relative proposal path per line
- `--working-tree` for tracked dirty proposal files
- `--against-upstream` for PR-style merge-base selection

`editorial build` runs targeted editorial validation first, then reuses the
runtime `check` path.

## Serve and Preview

Local serving keeps two distinct modes:

- `build-eips serve` for the runtime dev loop
- `build-eips preview` for serving already-built static output

### Dirty Serve

`build-eips --profile dirty serve` is the live local editing loop. It performs
the expensive runtime preparation once at startup, then watches the real active
content repo and mirrors tracked edits into the materialized repo that Zola is
serving from.

### Static Preview

`build-eips preview` serves the resolved output directory for the active
profile without invoking Zola, preprocessing markdown, or rebuilding anything.
If the output directory does not exist yet, it fails and tells you to run
`build-eips build` first.

The generated `justfile` also exposes:

```bash
just preview
just parity-preview
just dirty-preview
```

[`ethereum/EIPs`]: https://github.com/ethereum/EIPs/
[`ethereum/ERCs`]: https://github.com/ethereum/ERCs/
