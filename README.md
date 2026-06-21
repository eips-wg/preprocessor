build-eips
==========

`build-eips` is the local and CI build tool for Ethereum Improvement Proposal repositories. It prepares a multi-repo workspace with [EIPs], [ERCs] and the site theme, runs proposal validation, and coordinates the full site build pipeline.

## Prerequisites

`build-eips` requires a few runtime dependencies, available from wherever you get your software:

- git
- libgit2
- openssl
- [Zola](https://github.com/getzola/zola) 0.22.1[^1]

[^1]: The setup scripts can install or reuse the pinned Zola version for local workspace testing.

## Installation

### Pre-compiled Binaries

Pre-compiled binaries for Ubuntu, Windows, and macOS are available from [GitHub Releases].

[GitHub Releases]: https://github.com/ethereum/build-eips/releases

### From Source

If you're feeling particularly adventurous, you can install the latest version of `build-eips` like so:

```sh
cargo install --git https://github.com/ethereum/build-eips.git
```

[EIPs]: https://github.com/ethereum/EIPs/
[ERCs]: https://github.com/ethereum/ERCs/

### Local Testing

For direct manual testing, build and run the local binary explicitly:

```bash
cargo build
./target/debug/build-eips --help
./target/debug/build-eips -C ../EIPs check
```

```powershell
cargo build
.\target\debug\build-eips.exe --help
.\target\debug\build-eips.exe -C ..\EIPs check
```

For repeated manual testing, you can put the local debug binary first on `PATH` for the current shell:

```bash
export PATH="$PWD/target/debug:$PATH"
build-eips --help
build-eips -C ../EIPs check
```

```powershell
$env:Path = "$PWD\target\debug;$env:Path"
build-eips --help
build-eips -C ..\EIPs check
```

## Workspace Bootstrap

Use the contributor setup script when you are changing `build-eips` itself and need the full local multi-repo workspace that runs this checkout.

That workspace lets you test the proposal validation and site build pipeline against local EIPs/ERCs/theme checkouts without installing or reusing a released `build-eips`.

Linux and macOS:

```sh
./scripts/dev-setup
```

Windows PowerShell:

```powershell
.\scripts\dev-setup.ps1
```

The setup script builds the local debug binary, ensures the pinned Zola version is available, bootstraps the workspace with that binary, and runs `doctor`. It does not install or reuse a released `build-eips`.

The setup script selects its active proposal checkout through `ACTIVE_REPO_ROOT`; the selected checkout must contain a valid `Build.toml`. In the standard layout the active checkout is `EIPs/`. Set `ACTIVE_REPO_ROOT` when using another manifest-bearing proposal checkout.

After setup, the workspace has this layout:

```text
EIPs-project/
├── .build-eips.toml
├── WORKSPACE.md
├── .local-build/
├── EIPs/
├── ERCs/
├── theme/
├── preprocessor/
└── eipw/
```

## Workspace Reference

After bootstrapping, use the generated `WORKSPACE.md` in the workspace root for the full local command reference, workspace layout, configuration, source modes, build outputs, and troubleshooting notes.

The generated guide is sourced from [`src/workspace_doc.md`](src/workspace_doc.md). Update that file when changing workspace documentation.

Use `build-eips doctor` when the workspace does not behave as expected. It checks the active repo, sibling repos, theme checkout, local config, required tools, and generated workspace docs.

[`ethereum/EIPs`]: https://github.com/ethereum/EIPs/
[`ethereum/ERCs`]: https://github.com/ethereum/ERCs/
