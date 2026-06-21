# Architecture Overview

`build-eips` coordinates independent proposal, theme, and tooling repositories into a disposable prepared render tree. Proposal and theme checkouts remain authoritative source state; generated materialization and rendered output stay under the resolved build root.

Workspace setup and command usage are documented in `README.md` and the generated workspace guide sourced from `src/workspace_doc.md`. Rust module ownership is documented in `src/README.md`.

## System Layers

The system has five layers:

- proposal repositories: canonical proposal content and a repo-owned `Build.toml`
- local workspace overlay: workspace paths plus `.build-eips.toml` preferences
- preprocessor orchestration: active manifest loading, source policy, materialization, validation, and runtime handoff
- disposable prepared state: generated Git worktrees, a mounted local theme, and rendered output under the resolved build root
- render/output surface: the prepared tree and mounted theme consumed by Zola, plus static output served by preview

## Repository Responsibilities

- An active proposal repository owns `Build.toml` and proposal content. `Build.toml` declares the active name, locations for the active and sibling proposal repositories, each location's published base URL, and the theme repository/pin.
- The theme repository owns render templates, runtime configuration, styles, assets, and syntax definitions.
- The preprocessor owns orchestration, source resolution, materialization, preprocessing, validation, and runtime handoff.
- eipw owns editorial validation rules consumed through Cargo dependencies such as `eipw-lint`, `eipw-preamble`, and `eipw-snippets`.
- The template repository owns proposal scaffolding.

## Source And Generated State

- `Build.toml` is source-controlled metadata for the active proposal repository.
- `.build-eips.toml` is workspace-local state for server settings, site URL overrides, and render defaults.
- `workspace/theme` is an editable local checkout. A fresh `init` clone uses the active manifest's `[theme]` repository and pin; existing local themes are not reset.
- Prepared repositories and rendered output are generated under the resolved build root. The normal workspace root is `.local-build/<active-name>`; without workspace config, the fallback is `<active-repo>/build`.
- Mounted theme copies are generated runtime state. Canonical proposal content and the editable local theme are not rewritten by runtime commands.

## Active Manifest And Topology Resolution

`config::ActiveRepo` loads `Build.toml` from the selected active working tree. A missing or invalid active `Build.toml` is an error; there is no legacy manifest or hardcoded identity fallback.

The manifest determines:

- the active repository's managed workspace name
- sibling workspace names and declared repository URLs
- the active location's fallback `base-url`
- the initial source and pin for a missing workspace theme checkout

Workspace-local paths come from the manifest location names. Only the active location's `base-url` controls the rendered site; sibling base URLs remain manifest schema data and do not select runtime URL behavior.

## Source Policy

| Command mode | Active proposal source | Sibling proposal source | Theme source |
| --- | --- | --- | --- |
| Default build, check, or serve | local checkout with tracked edits materialized | workspace-local siblings | editable local `workspace/theme` |
| `--clean` | local checkout after cleanliness validation | workspace-local siblings | editable local `workspace/theme` |
| `--remote-siblings` | local checkout using the selected dirty or clean policy | active manifest's declared repository URLs | editable local `workspace/theme` |
| `changed` and editorial `--against-upstream` | prepared Git state with upstream comparison | not render topology | local theme for editorial lint |

`--clean` rejects a dirty active checkout before materialization. `--remote-siblings` changes only sibling source selection; combine it with `--clean` when the active source also needs a clean comparison.

Base URL precedence for rendering is CLI `--base-url`, then workspace `.build-eips.toml` `[site].base_url`, then the active manifest location `base-url`.

## Overlay And Materialization

The preprocessor does not merge source repositories in place. It materializes active and sibling proposal content into a disposable prepared repository, preprocesses markdown there, and mounts the local theme at `themes/eips-theme` for Zola.

Tracked dirty active-repository changes are included only in default dirty materialization. Clean materialization rejects dirty or untracked active state. Theme materialization includes tracked local theme files plus tracked or staged theme working-tree changes independently of active-source mode. Serve watches the local active repository when dirty mode is enabled and always watches the local theme checkout when a runtime theme is present.

## Validation And Rendering

Editorial lint reads `workspace/theme/config/eipw.toml` and uses the same schema compatibility check as workspace doctor. Editorial check composes editorial lint with the site-level check path.

`build`, `check`, and `serve` prepare the runtime tree and invoke Zola. `preview` serves existing rendered output and bypasses source materialization, markdown preprocessing, and Zola.

## Data Flow

```text
active Build.toml + local workspace config + local checkouts
        |
        v
execution resolution
        |-------------------------------|
        v                               v
editorial/changed-file validation   prepared build repository
                                        |
                                        v
                             markdown/proposal transformation
                                        |
                                        v
                       local theme materialization + Zola config
                                        |
                                        v
                            Zola validation or rendering
                                        |
                                        v
                              static output / preview surface
```

## Failure Boundaries

The system stops before ambiguous prepared state is created:

- a missing or invalid active `Build.toml` halts manifest-driven commands
- missing required local theme state halts Zola-backed and editorial lint paths
- sibling content conflicts halt materialization
- invalid workspace config halts execution-resolved runtime paths
- prepared-state mutation uses `<build_path>/.lock`; preview serves existing output without that lock

## Rollout Boundary

The template repository's legacy manifest is separate repository-owned rollout work. It is not read by the preprocessor's active-manifest loader and is not migrated by preprocessor workspace commands.
