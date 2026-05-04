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

### Build And Serve Locally

Build the full static site, then preview that built output:

```bash
build-eips build
build-eips preview
```

`preview` serves the last output written by `build`. Run `build` again before `preview` when you want to inspect fresh output.

Use `serve` when you want a live development server that livereloads changes instead of a reusable build output:

```bash
build-eips serve
```

`serve` runs a fresh temporary site build each time it is invoked (without using `build`), starts a local development server, and watches tracked local edits. Its output cannot be reused by `preview`.

Use `check` to quickly validate whether the site will build cleanly without producing the full built site:

```bash
build-eips check
```

By default, `check`, `build`, and `serve` use the local workspace in dirty mode, which includes tracked working-tree edits from this repo. `preview` serves the last output written by `build`. Use `--clean` when you want to ignore tracked local proposal edits for one command:

```bash
build-eips check --clean
build-eips build --clean
build-eips serve --clean
```

For staging, production, parity, and remote-sibling modes, see `../WORKSPACE.md`.

### Local Settings

Local build settings live in `../.build-eips.toml`, which the setup script generates. Use that workspace file to change the local server address or local site URL:

```toml
[server]
host = "127.0.0.1"
port = 1111

[site]
base_url = "http://127.0.0.1:1111"
```

`serve` and `preview` use `[server]` for the local bind address. `build` and `serve` use `[site].base_url` when generating links.

CLI flags such as `--host`, `--port`, and `--base-url` override the workspace config for one run:

```bash
build-eips serve --host 0.0.0.0 --port 3000 --base-url http://127.0.0.1:3000
```

### Render Specific Proposals Only

Full local `build` and `serve` runs can take time because they process every proposal file. When you want to quickly test a single proposal or a specific batch, add a list of desired proposal numbers to the workspace `.build-eips.toml`:

```toml
[render]
only = [555, 678]
```

Add one or more proposal numbers in `[render].only`, separated by commas. It's empty by default, but whenever it is populated, the regular `build` and `serve` commands render only those proposal pages. Links and references to excluded proposals are rewritten to the canonical public site.

Use CLI `--only` when you want a one-run target list; it also overrides any proposals in `[render].only` for that run:

```bash
build-eips serve --only 555
build-eips build --only 555
build-eips build --only 555 678
```

Multiple proposal numbers in the CLI are space-separated; no commas.

### Editorial Validation

Use editorial commands to validate proposal files before opening or updating a pull request.

- `editorial lint` runs targeted `eipw` proposal-rule checks.
- `editorial check` runs `editorial lint`, then checks that the selected proposal changes will not prevent the full site from building cleanly.

Check one or more specific proposals by number:

```bash
build-eips editorial check 1
build-eips editorial check 1 123
```

For the closest match to PR CI, use `editorial check` against the proposal files changed versus upstream:

```bash
build-eips --staging editorial check --against-upstream --format github
```

Both commands accept the same selector modes:

* proposal numbers or repo-relative proposal paths for explicit targets
* `--working-tree` for tracked dirty proposal files
* `--against-upstream` for proposal files changed versus the upstream merge-base
* `--batch <path>` for a repeatable target list

They also accept `eipw` options such as `--format github`.

Use a batch file when you want to lint or check the same proposal set repeatedly. A batch file is a plain text file with one proposal number per line:

```txt
1
7949
```

```bash
build-eips editorial lint --batch ../editor-batch.txt
build-eips editorial check --batch ../editor-batch.txt
```

### Source And Output Overrides

Workspace-local sources come from the standard workspace layout. The local theme is `workspace/theme`, and local sibling repos are `workspace/<sibling_repo_id>` from the active repo manifest.

Use `--remote-siblings` when you need to force remote sibling proposal sources for a single command.

Use global `--build-root <path>` when you want a separate prepared repo and output directory, for example to compare two builds side by side. The path replaces the default `.local-build/<repo_id>` location for each command where you pass it, so use the same `--build-root` value when serving or previewing builds.

Example:

```bash
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-local build --base-url http://127.0.0.1:1111
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-staging --staging build --base-url http://127.0.0.1:1112

build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-local preview --port 1111
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-staging preview --port 1112

# Or using serve
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-local serve --port 1111
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-staging --staging serve --port 1112
```

### Remote And Parity Modes

Use remote modes when you want a clean render of the local active checkout with staging or production environment metadata and remote sibling proposal sources.

`--staging` and `--production` use the local active checkout, reject dirty active-repo edits, and select remote sibling sources plus staging or production environment metadata:

```sh
build-eips --staging check
build-eips --staging build
build-eips --staging serve

build-eips --production check
build-eips --production build
build-eips --production serve
```

`parity` is the built-in clean staging path for checking whether the local active checkout behaves like the staging environment:

```sh
build-eips parity check
build-eips parity build
build-eips parity serve
```

Use `--remote-siblings` when you want to keep the active proposal repo local, but resolve sibling proposal repos from the configured remote environment:

```sh
build-eips --remote-siblings check
build-eips --remote-siblings build
build-eips --remote-siblings serve
```

Remote environment commands and `parity` use the local active checkout, but do not use local dirty proposal edits. They still use the workspace-local `theme/`, so check out the theme commit or branch you want before running them.
