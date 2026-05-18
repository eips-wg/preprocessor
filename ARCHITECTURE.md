# Architecture Overview

This document describes the `build-eips` system model: its system layers, repository responsibilities, trust boundaries, source and generated state, data flow, and the separation between validation, transformation, materialization, and rendering.

`build-eips` coordinates independent proposal, theme, and tooling repositories into a prepared render tree. The source repositories remain authoritative for their own data, while generated state is isolated to the resolved build root and consumed by validation and rendering paths.

Workspace setup, command usage, and Rust module ownership are documented in `README.md`, `src/workspace_doc.md`, and `src/README.md`, respectively.

## System Layers

The system is organized into layers with separate responsibilities:

- source repositories: canonical proposal content, tracked topology metadata, theme source files, and tooling crates
- local workspace overlay: local operator state and local checkout paths that let independent repositories work as one system
- preprocessor orchestration: active repository identity, topology, source policy, execution resolution, and runtime handoff
- disposable prepared state: generated Git worktrees, mounted theme copies, and rendered output under the resolved build root
- render/output surface: the prepared tree and mounted theme consumed by Zola, plus static output served by preview

Validation, transformation, materialization, and rendering are pipeline responsibilities. They are not separate system layers.

## Repository Responsibilities

Each repository owns one part of the system:

- proposal repositories own canonical content and `.build-eips.repo.toml` repo manifests
- the theme owns render templates, runtime configuration, styles, assets, and syntax definitions
- the preprocessor owns orchestration, source resolution, materialization, preprocessing, and runtime handoff
- eipw owns editorial validation rules consumed through Cargo dependencies such as `eipw-lint`, `eipw-preamble`, and `eipw-snippets`
- the template repository owns proposal scaffolding

## Source And Generated State

The architecture separates authoritative state from generated state:

- proposal `content/`, `.build-eips.repo.toml` repo manifests, and theme files are source
- `.build-eips.repo.toml` provides source-controlled topology; its `repo_id` is the stable key used for workspace directories, default build roots, and sibling references
- `.build-eips.toml` is local workspace state for local server, site, and render defaults
- prepared repositories and rendered output are generated under the resolved build root
- the normal workspace build root is `.local-build/<repo_id>`
- without workspace config, the fallback build root is `<active-repo>/build`
- mounted theme copies are generated runtime state
- canonical proposal content is not rewritten by runtime paths

## Trust Boundaries

Inputs enter the system with different trust and ownership properties:

- tracked `.build-eips.repo.toml` manifests are source-controlled topology
- `.build-eips.toml` is local operator state
- CLI flags are per-run user intent
- remote Git refs are fetched external state used for source materialization and merge-base comparison
- proposal markdown is source content that must be transformed and validated before rendering
- theme files are local source input and may include tracked or staged local theme changes during materialization
- prepared repositories, mounted themes, and rendered output are generated state

## Identity Resolution

Identity resolution determines the active proposal repository before topology is derived.

The inputs are:

- command anchor: current working directory or `-C`
- active proposal repository root
- tracked `.build-eips.repo.toml` manifest, when present
- built-in EIPs/ERCs identifying-commit lookup, when no manifest is present

The resolution order is manifest-backed identity first, then built-in EIPs/ERCs compatibility identity for checkouts without tracked manifests.

## Topology Resolution

Topology follows identity:

- sibling repositories come from the manifest or built-in compatibility metadata
- remote endpoints come from the selected repository identity
- local workspace paths come from workspace discovery and local config
- build roots come from local workspace defaults or per-run overrides

## Source Policy

Source policy determines which repository state is materialized or consumed after identity and topology are resolved.

| Policy | Active repository source | Dirty active edits | Sibling source | Environment metadata |
| --- | --- | --- | --- | --- |
| Local dirty site validation/rendering | local active checkout | tracked edits included | workspace-local siblings | local site defaults with selected upstream metadata |
| Clean local site validation/rendering | local active checkout | rejected before materialization | workspace-local siblings | local site defaults with selected upstream metadata |
| Remote-sibling site validation/rendering | local active checkout | follows the selected local dirty/clean policy | selected remote sibling endpoints | selected environment metadata |
| Explicit staging, production, and parity | local active checkout | rejected before materialization | selected remote sibling endpoints | selected environment metadata |
| Changed-file comparison and upstream editorial target selection | local active checkout through prepared Git state | clean comparison | not render topology | selected upstream merge base |
| Editorial lint with explicit, batch, or working-tree selectors | selected files in the active checkout | consumed directly by selector | not render topology | lint configuration from the local theme |

Staging, production, and parity paths do not replace the active render source with a remote active checkout. They require the local active checkout to be clean, use the selected environment metadata for upstream comparison and rendered base URLs, and resolve sibling proposal repositories from remote endpoints.

Editorial check composes editorial lint with the site-level check path. `--build-root` changes the resolved generated-state location, and `--only` narrows proposal rendering only for local dirty rendering paths; default render selection can also come from `[render].only` in `.build-eips.toml`.

## Overlay Model

The overlay model describes the render-time proposal surface:

- independent proposal repositories are not merged as source repositories
- sibling proposal content is overlaid into one prepared render tree
- the theme is materialized into the prepared tree at `themes/eips-theme`
- the overlay exists only in generated state
- the prepared render tree is the only proposal surface seen by Zola

Git mechanics belong to the materialization boundary. The overlay model is the architectural result of that materialization.

## Data Flow

The system flow has an execution-resolution step, a validation branch, and a prepared-state branch:

```text
active repo + sibling repos + theme + workspace config
        |
        v
execution resolution
        |-------------------------------|
        v                               v
editorial/changed-file validation   prepared build repo
                                        |
                                        v
                             markdown/proposal transformation
                                        |
                                        v
                              theme mount + Zola config
                                        |
                                        v
                            Zola validation or rendering
                                        |
                                        v
                              static output / preview surface
```

Some validation work happens before or beside materialization, while Zola validation and rendering happen against prepared state. `changed` and `editorial --against-upstream` still use prepared Git state to compute merge-base differences. They bypass markdown transformation and Zola; they do not bypass Git source preparation entirely.

## Materialization Boundary

Materialization is the boundary between source repositories and runtime state:

- runtime paths do not render directly from source checkouts
- the preprocessor creates a disposable prepared repository
- active and sibling proposal content are merged into that prepared tree
- tracked dirty active-repo state is included only when the selected source mode allows it
- dirty active-repo materialization ignores untracked active-repo files
- clean source materialization rejects dirty or untracked active-repo state before proceeding
- theme materialization includes tracked theme files plus tracked or staged theme working-tree changes, independent of active-repo dirty mode
- canonical source content is not rewritten

## Pipeline Responsibilities

The pipeline separates kinds of work:

- identity and topology resolution: decide what the runtime path operates on
- materialization: Git clone/fetch/dirty/sibling/theme staging
- transformation: markdown preprocessing, front matter, links, `requires`, and citations
- validation: editorial lint, changed-file selection, and Zola check behavior
- rendering: Zola build and serve

Preview is the exception: it serves prior rendered output and bypasses materialization, transformation, and Zola.

Editorial validation is a peer of rendering. It consumes the same identity and source model, and the editorial check path composes with the runtime check path when site-level validation is required.

## Invariants

These rules keep the system coherent:

- canonical proposal content is not rewritten by runtime paths
- generated output lives under the resolved build root
- Zola sees one prepared repo and one mounted theme
- sibling content enters only through materialization
- workspace-local state is not canonical proposal content

## Failure Boundaries

The system halts at boundaries that would otherwise make prepared state ambiguous or invalid:

- prepared-state mutation paths use `<build_path>/.lock`; preview bypasses the lock because it serves existing output, and clean unlocks before removing the build root
- missing required local theme halts Zola-backed and editorial lint paths
- unresolved active repository identity halts the run
- sibling content path conflicts halt materialization
- invalid discovered workspace config halts execution-resolved runtime paths

## Deferred Or Compatibility Paths

Tracked `.build-eips.repo.toml` manifests are the normal topology description. Built-in EIPs/ERCs identity handles checkouts without tracked manifests. The template manifest exists, but supported raw-template bootstrap/developer workflow is not first-class yet.
