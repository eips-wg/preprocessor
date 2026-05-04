# build-eips Workspace

This directory is a local multi-repo workspace for building, serving, previewing, and validating EIPs/ERCs with the shared theme and proposal sibling repos.

The workspace keeps the proposal repos, theme repo, generated build output, and local workspace settings in one predictable layout. Run commands from an active proposal repo such as `EIPs/` or `ERCs/`, or from this workspace root with `-C EIPs` or `-C ERCs`.

## Workspace Layout

After running a setup script, the minimal operational workspace should look like this:

```text
EIPs-project/
├── .build-eips.toml
├── WORKSPACE.md
├── .local-build/
├── EIPs/
├── ERCs/
└── theme/
```

Optional setup flags can add more repos:

```text
EIPs-project/
├── template/       # --template
├── preprocessor/  # --platform-dev
└── eipw/          # --platform-dev
```

- `.build-eips.toml`: workspace settings.
- `WORKSPACE.md`: generated workspace guide.
- `.local-build/`: generated build output and materialized repositories.
- `EIPs/` and `ERCs/`: proposal source repositories.
- `theme/`: workspace-local Zola theme required by build, serve, and check commands.
- `template/`: optional proposal template repository.
- `preprocessor/`: optional local `build-eips` development checkout.
- `eipw/`: optional local `eipw` development checkout.

If the optional repos are missing, rerun build-eips init with the needed flags.

From an active proposal repo:

```sh
build-eips init .. --template
build-eips init .. --platform-dev
build-eips init .. --template --platform-dev
```

From the workspace root:

```sh
build-eips -C EIPs init . --template
build-eips -C EIPs init . --platform-dev
build-eips -C EIPs init . --template --platform-dev
```

## Requirements And Troubleshooting

Local workspace commands require these tools on `PATH`:

- Git
- `build-eips`
- Zola 0.22.1

Git must be installed separately. The setup scripts locate or install `build-eips` and Zola, add locally installed tool directories to `PATH` for the current shell session, and print guidance for making those `PATH` changes permanent.

Run `build-eips doctor` after setup and whenever a command cannot find a repo, config file, theme, or required tool:

```sh
build-eips doctor
```

From the workspace root, anchor the command through an active proposal repo:

```sh
build-eips -C EIPs doctor
build-eips -C ERCs doctor
```

`build-eips doctor` checks:

- required tools: Git, `build-eips`, and Zola 0.22.1
- the active proposal repo manifest
- `.build-eips.toml`
- workspace-local sibling proposal repos
- workspace-local `theme/`
- optional setup helper tools used by setup scripts

If a fresh shell cannot find `build-eips` or Zola, rerun the setup script or apply the permanent `PATH` guidance printed by the setup script.

If a sibling repo, `theme/`, or optional platform repo is missing, rerun `build-eips init` with the needed flags.

If `theme/` or `preprocessor/` setup cannot create the default `EIPs/` checkout, check that Git is installed and that the workspace `EIPs/` path does not already exist as a non-git directory. Set `ACTIVE_REPO_ROOT` when you want setup to use an existing ERCs or custom proposal repo checkout.

If `build-eips doctor` reports that Zola is missing or too old, rerun the setup script to install the supported Zola version.

## Local Commands

Use the active proposal repo for local build commands:

```sh
build-eips check
build-eips build
build-eips serve
```

The workspace config starts with local server and site defaults:

```toml
[server]
host = "127.0.0.1"
port = 1111

[site]
base_url = "http://127.0.0.1:1111"
```

## Render Specific Proposals Only

Full local `build` and `serve` runs can take time because they process every
proposal file. When you want to quickly test a single proposal or a specific
batch, add a list of desired proposal numbers to the workspace
`.build-eips.toml`:

```toml
[render]
only = [555, 678]
```

Whenever `[render].only` is populated, regular local dirty `build` and `serve`
commands render only those proposal pages. Links and references to excluded
proposals are rewritten to the canonical public site.

Use CLI `--only` when you want a one-run target list; it overrides any
proposals in `[render].only` for that run:

```sh
build-eips serve --only 555
build-eips build --only 555
build-eips build --only 555 678
```

Multiple proposal numbers in the CLI are space-separated; no commas.
