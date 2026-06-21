# build-eips Workspace

This guide is generated as `WORKSPACE.md` by `build-eips init`. Its source lives in the preprocessor repository at `src/workspace_doc.md`.

A workspace brings proposal repositories, an editable local theme, generated build output, and workspace-local settings into one predictable layout. Run commands from an active proposal repository such as `EIPs/` or `ERCs/`, or anchor them with `-C` from the workspace root.

## Workspace Layout

After setup, a minimal workspace looks like this:

```text
EIPs-project/
├── .build-eips.toml
├── WORKSPACE.md
├── .local-build/
├── EIPs/
├── ERCs/
└── theme/
```

Optional setup flags can add more repositories:

```text
EIPs-project/
├── template/       # --template
├── preprocessor/   # --platform-dev
└── eipw/           # --platform-dev
```

- `.build-eips.toml`: workspace-local server, site, and render settings.
- `WORKSPACE.md`: this generated guide.
- `.local-build/`: generated build output and prepared repositories.
- `EIPs/` and `ERCs/`: proposal source repositories in this example workspace.
- `theme/`: editable workspace-local Zola theme used by runtime and editorial commands.
- `template/`: optional proposal template repository.
- `preprocessor/`: optional local `build-eips` development checkout.
- `eipw/`: optional local `eipw` development checkout.

The active proposal repository must contain a valid `Build.toml`. Its `name` and `locations` define the managed workspace names and sibling repositories. Its `[theme]` metadata supplies the repository and pin used only when `init` creates a missing `workspace/theme` checkout.

Once `workspace/theme` exists, it remains an editable local checkout. `build`, `check`, `serve`, and editorial lint use that local theme directly.

If optional repositories are missing, rerun `build-eips init` with the needed flags.

From an active proposal repository:

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
- Zola 0.22.1 or newer

Git must be installed separately. The setup scripts locate or install `build-eips` and Zola, add locally installed tool directories to `PATH` for the current shell session, and print guidance for making those `PATH` changes permanent.

Run `build-eips doctor` after setup and whenever a command cannot find a repository, config file, theme, or required tool:

```sh
build-eips doctor
```

From the workspace root, anchor the command through an active proposal repository:

```sh
build-eips -C EIPs doctor
build-eips -C ERCs doctor
```

`build-eips doctor` checks:

- required tools: Git, `build-eips`, and Zola
- the active repository `Build.toml`
- `.build-eips.toml`
- workspace-local sibling proposal repositories
- workspace-local `theme/` readiness
- optional setup helper tools used by setup scripts

If a fresh shell cannot find `build-eips` or Zola, rerun the setup script or apply the permanent `PATH` guidance printed by the setup script.

If a sibling repository, `theme/`, or optional platform repository is missing, rerun `build-eips init` with the needed flags. If doctor reports a theme pin mismatch, the checkout is still usable for local theme development; the warning means it differs from the active manifest pin.

If `theme/` or `preprocessor/` setup cannot create the active proposal checkout, check that Git is installed and that the workspace path does not already exist as a non-git directory. Set `ACTIVE_REPO_ROOT` when setup should use an existing manifest-bearing proposal checkout.

If `build-eips doctor` reports that Zola is missing or too old, rerun the setup script to install a supported Zola version.

## Build And Serve Locally

Build the full static site, then preview that built output:

```bash
build-eips build
build-eips preview
```

`preview` serves the last output written by `build`. Run `build` again before `preview` when you want to inspect fresh output.

Use `serve` when you want a live development server that reloads changes instead of reusable build output:

```bash
build-eips serve
```

`serve` runs a fresh temporary site build, starts a local development server, and watches tracked local proposal and theme edits. Its output cannot be reused by `preview`.

Use `check` to validate that the site can build cleanly without producing full static output:

```bash
build-eips check
```

By default, `check`, `build`, and `serve` materialize tracked working-tree edits from the active proposal repository and use the editable local `workspace/theme`.

Use `--clean` when you need a clean active proposal source. It requires the active checkout to be clean before materialization; it does not replace the local theme checkout.

```bash
build-eips check --clean
build-eips build --clean
build-eips serve --clean
```

## Local Settings And URL Overrides

Local settings live in the workspace `.build-eips.toml` generated by setup:

```toml
[server]
host = "127.0.0.1"
port = 1111

[site]
base_url = "http://127.0.0.1:1111"
```

`serve` and `preview` use `[server]` for the local bind address. `build` and `serve` use `[site].base_url` when generating links.

For `build` and `serve`, URL precedence is:

1. CLI `--base-url`
2. workspace `.build-eips.toml` `[site].base_url`
3. the active `Build.toml` location `base-url`

CLI flags override workspace settings for one run:

```bash
build-eips serve --host 0.0.0.0 --port 3000 --base-url http://127.0.0.1:3000
build-eips build --base-url https://preview.example.test/
```

## Source Selection And Build Roots

Default workspace mode uses the local active proposal checkout, local sibling repositories named by the active `Build.toml` locations, and the editable local `workspace/theme`.

Use `--remote-siblings` to resolve sibling proposal content from the active manifest's declared repository URLs for one command. The active proposal checkout remains local. Combine it with `--clean` when the active source must also be clean:

```bash
build-eips --remote-siblings check
build-eips --clean --remote-siblings build
build-eips --clean --remote-siblings serve
```

Use global `--build-root <path>` for a separate prepared repository and output directory, such as side-by-side local and clean builds. The path replaces the default `.local-build/<active-name>` location for commands that receive it, so reuse the same value when previewing that output.

```bash
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-local build --base-url http://127.0.0.1:1111
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-clean --clean --remote-siblings build --base-url http://127.0.0.1:1112

build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-local preview --port 1111
build-eips -C /work/EIPs-project/EIPs --build-root /tmp/eips-clean preview --port 1112
```

## Render Specific Proposals Only

Full local `build` and `serve` runs can take time because they process every proposal file. To render a specific set, add proposal numbers to the workspace `.build-eips.toml`:

```toml
[render]
only = [555, 678]
```

Whenever `[render].only` is populated, regular `build` and `serve` commands render only those proposal pages. Links and references to excluded proposals are rewritten to the canonical public site.

Use CLI `--only` for a one-run target list; it overrides `[render].only` for that run:

```bash
build-eips serve --only 555
build-eips build --only 555
build-eips build --only 555 678
```

Multiple CLI proposal numbers are space-separated; do not use commas.

## Editorial Validation

Use editorial commands to validate proposal files before opening or updating a pull request.

- `editorial lint` runs targeted `eipw` proposal-rule checks using `workspace/theme/config/eipw.toml`.
- `editorial check` runs `editorial lint`, then checks that selected proposal changes will not prevent the full site from building cleanly.

Check one or more specific proposals by number:

```bash
build-eips editorial check 1
build-eips editorial check 1 123
```

For the closest match to a change-based CI check, compare proposal files against upstream:

```bash
build-eips editorial check --against-upstream --format github
```

Both commands accept the same selector modes:

- proposal numbers or repository-relative proposal paths for explicit targets
- `--working-tree` for tracked dirty proposal files
- `--against-upstream` for proposal files changed versus the upstream merge-base
- `--batch <path>` for a repeatable target list

They also accept `eipw` options such as `--format github`.

Use a batch file when you want to lint or check the same proposal set repeatedly. A batch file is plain text with one proposal number per line:

```txt
1
7949
```

```bash
build-eips editorial lint --batch editor-batch.txt
build-eips editorial check --batch editor-batch.txt
```
