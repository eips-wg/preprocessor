# Preprocessor Module And Test Ownership

This file documents how the preprocessor crate is organized: which module owns each behavior, where tests should live, and how to keep module boundaries reviewable. It is for contributors and maintainers, not end-user documentation.


## Module Ownership

- `main.rs` owns CLI entry, top-level dispatch, build locking, and runtime orchestration.
- `changed.rs` owns changed-file command execution, coordinating upstream diffing, proposal filtering, and changed-output formatting.
- `cli.rs` owns the clap command surface and command helper methods.
- `config.rs` owns built-in repository metadata, workspace config schema, repo manifest schema, parsing, defaults, and config discovery.
- `context.rs` owns command input path resolution and workspace command context.
- `editorial.rs` owns editorial selector resolution, active-repo target validation, prepared-source editorial lint orchestration, and editorial check runtime handoff.
- `execution.rs` owns source mode, environment, base URL, server binding, build path, workspace source, and runtime execution resolution.
- `find_root.rs` owns active proposal-repo root detection.
- `git.rs` owns git repository identification, clone/fetch/merge behavior, source materialization, and tracked path synchronization.
- `github.rs` owns GitHub annotation reporting support for lint output.
- `identity.rs` owns active repository identity selection from repo manifests or legacy metadata.
- `layout.rs` owns shared build layout names and path helpers.
- `lint.rs` owns eipw config loading, reporter setup, and invocation for caller-provided source lists.
- `markdown.rs` owns proposal markdown preprocessing.
- `pipeline.rs` owns prepared Zola runtime setup and build/check/serve steps.
- `preview.rs` owns static preview serving for already-built output.
- `print.rs` owns diagnostic print subcommands.
- `progress.rs` owns progress/log rendering helpers.
- `proposal.rs` owns proposal path classification, proposal-number parsing, and targeted-rendering selection policy.
- `serve.rs` owns dirty active-repo and local-theme serve synchronization.
- `workspace.rs` owns local workspace setup and diagnostics.
- `zola.rs` owns Zola discovery, theme mounting, and Zola command invocation.

## Test Ownership

New tests should generally live in the module that owns the behavior. Use `super::` from module-local tests where natural, or sibling module paths from the owning module. Module-local tests cover behavior in modules such as `cli.rs`, `config.rs`, `execution.rs`, `git.rs`, `markdown.rs`, `pipeline.rs`, `proposal.rs`, `serve.rs`, `workspace.rs`, and `zola.rs`; `src/tests.rs` intentionally holds the remaining cross-domain behavior tests.

Use `src/tests.rs` for cross-domain behavior tests, especially tests covering:

- command dispatch across modules
- CLI plus execution plus runtime behavior
- downstream CI invariance
- multi-repo or manifest-driven flows
- workspace plus execution plus editorial behavior
- source materialization behavior spanning git, execution, pipeline, or serve
- tests that would require exposing more internals just to move them

Move tests to module-local `#[cfg(test)]` modules only when the behavior is owned by one module and the test remains clearer there. Examples include pure clap parsing in `cli.rs`, execution policy helpers in `execution.rs`, serve event filtering in `serve.rs`, workspace-local theme materialization in `pipeline.rs`, prepared-source editorial lint behavior in `editorial.rs`, workspace setup and diagnostics behavior in `workspace.rs`, active repo identity behavior in `identity.rs` when it does not require the full execution path, and proposal path or proposal-number behavior in `proposal.rs`.

## Dependency Direction

Imports should generally flow from orchestration modules toward domain and shared modules. `main.rs` should call into command/runtime modules, while lower-level modules should avoid depending on high-level orchestration.

`execution.rs` is the runtime resolution layer between command/config inputs and runtime execution: it combines CLI flags, workspace config, active repo identity, source mode, build path, server binding, and base URL decisions before `main.rs` hands work to `changed.rs`, `pipeline.rs`, `serve.rs`, `preview.rs`, or editorial helpers.

Examples of high-level orchestration modules include `main.rs`, `workspace.rs`, `pipeline.rs`, and `serve.rs`. Shared or lower-level modules include `cli.rs`, `layout.rs`, `proposal.rs`, and focused domain modules such as `git.rs`, `markdown.rs`, and `zola.rs`.

Lower-level or shared modules should not import higher-level orchestration modules just to reuse behavior. Move shared behavior into the owning domain module instead.


## Visibility

Do not make private helpers `pub` or `pub(crate)` just to move a test. If a test needs direct access to private module behavior, it probably belongs in that module's `#[cfg(test)]` block. Use `src/tests.rs` for cross-module behavior tests that exercise crate-visible paths.
