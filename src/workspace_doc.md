# build-eips Workspace

This directory is a local multi-repo workspace for building and checking EIPs/ERCs with the shared theme and proposal sibling repos.

## Workspace Layout

After initialization, the minimal workspace should look like this:

```text
EIPs-project/
├── .build-eips.toml
├── WORKSPACE.md
├── .local-build/
├── EIPs/
├── ERCs/
└── theme/
```

Use `build-eips init ..` from an active proposal repo such as `EIPs/` or `ERCs/` to create missing sibling repos, clone `theme/`, create `.local-build/`, write `.build-eips.toml`, and generate this guide.

Pass `--template` when proposal template work also needs the optional `template/` repo.

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
