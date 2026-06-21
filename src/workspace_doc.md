# build-eips Workspace

This guide is generated as `WORKSPACE.md` by `build-eips init`.

A workspace keeps proposal repositories, an editable local theme, generated build output, and workspace-local settings together.

The active proposal repository must contain a valid `Build.toml`. Its locations define sibling proposal repositories, and `[theme]` supplies the repository and pin used when `init` creates a missing theme checkout.

```sh
build-eips init ..
build-eips init .. --template
```

`init` preserves existing usable repositories and writes `.build-eips.toml` only when it is missing.


## Render Specific Proposals Only

Use `--only` with local dirty builds and serves to render selected proposal numbers. A workspace `[render]` section can provide the same defaults.
