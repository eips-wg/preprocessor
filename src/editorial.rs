/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Editorial target selection and runtime helpers.

use std::{
    collections::BTreeSet,
    fs::File,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use log::info;
use snafu::{OptionExt, ResultExt, Whatever};

use crate::{
    cli::EditorialSelectorArgs,
    context::resolve_input_path,
    execution::ResolvedExecution,
    git,
    layout::REPO_DIR,
    lint,
    proposal::{
        classify_editorial_number_selector, is_proposal_path,
        resolve_proposal_number_markdown_path, EditorialNumberSelector,
    },
};

fn repo_relative_canonical_path(
    root_path: &Path,
    path: &Path,
    canonical_path: &Path,
) -> Result<PathBuf, Whatever> {
    if path.is_absolute() {
        snafu::whatever!(
            "editorial selectors require repo-relative proposal paths, got `{}`",
            path.to_string_lossy()
        );
    }

    let relative = canonical_path
        .strip_prefix(root_path)
        .with_whatever_context(|_| {
            format!(
                "editorial target `{}` escapes the active repository root",
                path.to_string_lossy()
            )
        })?
        .to_path_buf();

    Ok(relative)
}

fn validate_editorial_targets(
    root_path: &Path,
    paths: Vec<PathBuf>,
    strict: bool,
) -> Result<Vec<PathBuf>, Whatever> {
    let mut unique = BTreeSet::new();
    let mut targets = Vec::new();

    for path in paths {
        if path.is_absolute() {
            snafu::whatever!(
                "editorial selectors require repo-relative proposal paths, got `{}`",
                path.to_string_lossy()
            );
        }

        let full_path = root_path.join(&path);
        let canonical_path = match full_path.canonicalize() {
            Ok(canonical_path) => canonical_path,
            Err(error)
                if !strict
                    && matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                continue;
            }
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!(
                        "unable to resolve editorial target `{}`",
                        full_path.to_string_lossy()
                    )
                });
            }
        };

        let relative = repo_relative_canonical_path(root_path, &path, &canonical_path)?;

        if !is_proposal_path(&relative) {
            if strict {
                snafu::whatever!(
                    "editorial target `{}` is not a supported proposal path",
                    relative.to_string_lossy()
                );
            }
            continue;
        }

        if unique.insert(relative.clone()) {
            targets.push(relative);
        }
    }

    if strict && targets.is_empty() {
        snafu::whatever!("editorial selector resolved no proposal files");
    }

    Ok(targets)
}

fn read_editorial_batch(path: &Path) -> Result<Vec<PathBuf>, Whatever> {
    let contents =
        std::fs::read_to_string(path).whatever_context("unable to read editorial batch file")?;
    let mut paths = Vec::new();

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        paths.push(PathBuf::from(line));
    }

    Ok(paths)
}

fn normalize_editorial_selector(root_path: &Path, path: PathBuf) -> Result<PathBuf, Whatever> {
    let Some(selector) = path.as_os_str().to_str() else {
        return Ok(path);
    };

    match classify_editorial_number_selector(selector) {
        EditorialNumberSelector::Number(proposal_number) => {
            resolve_proposal_number_markdown_path(root_path, proposal_number)
        }
        EditorialNumberSelector::InvalidNumberLike(_failure) => {
            snafu::whatever!(
                "editorial number selector `{selector}` is invalid; expected a positive proposal number that fits in u32, without signs or commas"
            );
        }
        EditorialNumberSelector::PathLike => Ok(path),
    }
}

fn normalize_editorial_selectors(
    root_path: &Path,
    paths: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, Whatever> {
    paths
        .into_iter()
        .map(|path| normalize_editorial_selector(root_path, path))
        .collect::<Result<_, _>>()
}

fn prepare_editorial_lint_source(
    resolved: &ResolvedExecution,
) -> Result<(PathBuf, git::SourceOnly), Whatever> {
    let repo_path = resolved.build_path.join(REPO_DIR);
    let source = git::Fresh::new(
        &resolved.root_path,
        &repo_path,
        resolved.repository_use.clone(),
        resolved.source_materialization,
    )
    .whatever_context("initializing build repo for editorial source preparation")?
    .clone_src()
    .whatever_context("cloning source repo for editorial source preparation")?;

    Ok((repo_path, source))
}

fn prepare_editorial_lint_source_with_upstream(
    resolved: &ResolvedExecution,
) -> Result<(PathBuf, git::SourceWithUpstream), Whatever> {
    let (repo_path, source) = prepare_editorial_lint_source(resolved)?;
    let source = source
        .fetch_upstream()
        .whatever_context("fetching upstream repo for editorial source preparation")?;

    Ok((repo_path, source))
}

fn raw_editorial_targets(
    selectors: &EditorialSelectorArgs,
    resolved: &ResolvedExecution,
    upstream_source: Option<&git::SourceWithUpstream>,
) -> Result<Vec<PathBuf>, Whatever> {
    if selectors.selector_count() != 1 {
        snafu::whatever!(
            "choose exactly one editorial selector: explicit proposal targets, `--batch`, `--working-tree`, or `--against-upstream`"
        );
    }

    let raw_targets = if !selectors.paths.is_empty() {
        selectors.paths.clone()
    } else if let Some(batch) = selectors.batch.as_deref() {
        let batch = resolve_input_path(batch)?;
        read_editorial_batch(&batch)?
    } else if selectors.working_tree {
        git::working_tree_paths(&resolved.root_path)
            .whatever_context("unable to resolve working-tree editorial targets")?
    } else {
        upstream_source
            .whatever_context(
                "against-upstream editorial target selection requires upstream source",
            )?
            .changed_files()
            .whatever_context("unable to list editorial targets against upstream")?
    };

    Ok(raw_targets)
}

fn validate_raw_editorial_targets(
    selectors: &EditorialSelectorArgs,
    resolved: &ResolvedExecution,
    raw_targets: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, Whatever> {
    let strict = !selectors.paths.is_empty() || selectors.batch.is_some();
    let targets = if strict {
        normalize_editorial_selectors(&resolved.root_path, raw_targets)?
    } else {
        raw_targets
    };
    validate_editorial_targets(&resolved.root_path, targets, strict)
}

fn editorial_targets_from_source(
    selectors: &EditorialSelectorArgs,
    resolved: &ResolvedExecution,
    upstream_source: Option<&git::SourceWithUpstream>,
) -> Result<Vec<PathBuf>, Whatever> {
    let raw_targets = raw_editorial_targets(selectors, resolved, upstream_source)?;

    validate_raw_editorial_targets(selectors, resolved, raw_targets)
}

pub(crate) fn editorial_targets(
    selectors: &EditorialSelectorArgs,
    resolved: &ResolvedExecution,
) -> Result<Vec<PathBuf>, Whatever> {
    editorial_targets_from_source(selectors, resolved, None)
}

fn validate_prepared_editorial_targets(
    prepared_repo_path: &Path,
    targets: &[PathBuf],
) -> Result<(), Whatever> {
    for target in targets {
        if target.is_absolute() {
            snafu::whatever!(
                "editorial selectors require repo-relative proposal paths, got `{}`",
                target.to_string_lossy()
            );
        }

        let prepared_target = prepared_repo_path.join(target);
        let file = match File::open(&prepared_target) {
            Ok(file) => file,
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                snafu::whatever!(
                    "editorial target `{}` exists in the active repo but was not materialized into the prepared source tree; untracked files are not supported",
                    target.to_string_lossy()
                );
            }
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!(
                        "unable to open prepared editorial target `{}`",
                        prepared_target.to_string_lossy()
                    )
                });
            }
        };
        let metadata = file.metadata().with_whatever_context(|_| {
            format!(
                "unable to inspect prepared editorial target `{}`",
                prepared_target.to_string_lossy()
            )
        })?;

        if !metadata.is_file() {
            snafu::whatever!(
                "prepared editorial target `{}` is not a file",
                target.to_string_lossy()
            );
        }
    }

    Ok(())
}

pub(crate) fn run_editorial_lint(
    resolved: &ResolvedExecution,
    selectors: &EditorialSelectorArgs,
    eipw: lint::CmdArgs,
) -> Result<bool, Whatever> {
    if selectors.against_upstream {
        let (repo_path, source) = prepare_editorial_lint_source_with_upstream(resolved)?;
        let targets = editorial_targets_from_source(selectors, resolved, Some(&source))?;
        if targets.is_empty() {
            info!("editorial selector resolved no proposal files; skipping editorial lint");
            return Ok(false);
        }
        source
            .merge()
            .whatever_context("unable to merge ERC/EIP repositories for editorial lint")?;
        validate_prepared_editorial_targets(&repo_path, &targets)?;

        lint::eipw(resolved.theme_path()?, &repo_path, targets, eipw)
            .whatever_context("editorial lint failed")?;

        return Ok(true);
    }

    let targets = editorial_targets(selectors, resolved)?;
    if targets.is_empty() {
        info!("editorial selector resolved no proposal files; skipping editorial lint");
        return Ok(false);
    }

    let (repo_path, source) = prepare_editorial_lint_source(resolved)?;
    source
        .merge()
        .whatever_context("unable to merge ERC/EIP repositories for editorial lint")?;
    validate_prepared_editorial_targets(&repo_path, &targets)?;

    lint::eipw(resolved.theme_path()?, &repo_path, targets, eipw)
        .whatever_context("editorial lint failed")?;

    Ok(true)
}

pub(crate) fn editorial_runtime_execution(
    mut resolved: ResolvedExecution,
    selectors: &EditorialSelectorArgs,
) -> ResolvedExecution {
    if selectors.working_tree {
        resolved.source_materialization = git::SourceMaterialization::Dirty;
    }
    resolved
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use eipw_lint::config::DefaultOptions;
    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;
    use url::Url;

    use crate::{
        cli::{Args, EditorialCommand, EditorialSelectorArgs, RuntimeOperation},
        config::{self, ServerBinding},
        execution::{resolve_execution, ResolvedExecution},
    };

    use super::{
        editorial_runtime_execution, editorial_targets, run_editorial_lint,
        validate_editorial_targets,
    };

    struct EditorialWorkspace {
        _temp: TempDir,
        active_path: PathBuf,
    }

    fn write_file(root: &Path, relative: impl AsRef<Path>, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn commit_all(repo: &Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let signature = Signature::now("build-eips test", "build-eips@example.test").unwrap();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| repo.find_commit(oid).unwrap())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .unwrap();
    }

    fn init_repo(path: &Path, files: &[(&str, &str)]) -> Repository {
        std::fs::create_dir_all(path).unwrap();
        let repo = Repository::init(path).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        for (relative, contents) in files {
            write_file(path, relative, contents);
        }
        commit_all(&repo, "initial");
        repo
    }

    fn file_url(path: &Path) -> Url {
        Url::from_directory_path(path).unwrap()
    }

    fn repo_manifest_text(repo_id: &str, repository: &Url, siblings: &[(&str, Url)]) -> String {
        let mut manifest = format!(
            r#"
repo_id = "{repo_id}"

[production]
repository = "{repository}"
base_url = "https://example.test/{repo_id}/"

[staging]
repository = "{repository}"
base_url = "https://staging.example.test/{repo_id}/"
"#
        );

        for (sibling_id, sibling_repository) in siblings {
            manifest.push_str(&format!(
                r#"
[siblings.{sibling_id}.production]
repository = "{sibling_repository}"
base_url = "https://example.test/{sibling_id}/"

[siblings.{sibling_id}.staging]
repository = "{sibling_repository}"
base_url = "https://staging.example.test/{sibling_id}/"
"#
            ));
        }

        manifest
    }

    fn proposal_markdown(number: u32, category: Option<&str>, body: &str) -> String {
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: Proposal {number}\ndescription: Proposal {number}\nauthor: Test Author <test@example.com>\ndiscussions-to: https://ethereum-magicians.org/t/test/{number}\nstatus: Draft\ntype: Standards Track\n{category}created: 2025-01-01\n---\n\n{body}\n"
        )
    }

    fn write_eipw_config(workspace_root: &Path) {
        let schema_version = DefaultOptions::<String>::schema_version();
        write_file(
            workspace_root,
            "theme/config/eipw.toml",
            &format!(
                "schema-version = \"{schema_version}\"\n\n[fetch]\nproposal-format = \"{{:05}}\"\n"
            ),
        );
    }

    fn missing_file_url() -> Url {
        let temp = TempDir::new().unwrap();
        file_url(&temp.path().join("missing-upstream"))
    }

    fn editorial_workspace_with_upstream(
        active_body: &str,
        sibling_body: &str,
        upstream_url: Option<Url>,
    ) -> EditorialWorkspace {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("EIPs");
        let sibling_path = workspace_root.join("ERCs");
        let active_url = upstream_url.unwrap_or_else(|| file_url(&active_path));
        let sibling_url = file_url(&sibling_path);
        let manifest = repo_manifest_text("EIPs", &active_url, &[("ERCs", sibling_url)]);
        let active_markdown = proposal_markdown(1, None, active_body);
        let sibling_markdown = proposal_markdown(2, Some("ERC"), sibling_body);

        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        write_eipw_config(&workspace_root);
        let _active_repo = init_repo(
            &active_path,
            &[
                (config::REPO_MANIFEST_FILE, manifest.as_str()),
                ("content/00001.md", active_markdown.as_str()),
            ],
        );
        let _sibling_repo = init_repo(
            &sibling_path,
            &[("content/00002.md", sibling_markdown.as_str())],
        );

        EditorialWorkspace {
            _temp: temp,
            active_path,
        }
    }

    fn editorial_workspace(active_body: &str, sibling_body: &str) -> EditorialWorkspace {
        editorial_workspace_with_upstream(active_body, sibling_body, None)
    }

    fn editorial_workspace_with_missing_upstream(
        active_body: &str,
        sibling_body: &str,
    ) -> EditorialWorkspace {
        editorial_workspace_with_upstream(active_body, sibling_body, Some(missing_file_url()))
    }

    fn parsed_editorial_lint(
        active_path: &Path,
        lint_args: &[&str],
    ) -> (
        ResolvedExecution,
        EditorialSelectorArgs,
        crate::lint::CmdArgs,
    ) {
        let active_path = active_path.to_str().unwrap();
        let mut arguments = vec!["build-eips", "-C", active_path, "editorial", "lint"];
        arguments.extend_from_slice(lint_args);
        let args = Args::try_parse_from(arguments).unwrap();
        let resolved = resolve_execution(&args).unwrap();

        match args.operation.runtime_operation().unwrap() {
            RuntimeOperation::Editorial {
                command: EditorialCommand::Lint { selectors, eipw },
            } => (resolved, selectors, eipw),
            _ => panic!("expected editorial lint command"),
        }
    }

    fn run_lint(
        workspace: &EditorialWorkspace,
        lint_args: &[&str],
    ) -> Result<bool, snafu::Whatever> {
        let (resolved, selectors, eipw) = parsed_editorial_lint(&workspace.active_path, lint_args);

        run_editorial_lint(&resolved, &selectors, eipw)
    }

    fn resolved_execution(root_path: PathBuf) -> ResolvedExecution {
        ResolvedExecution {
            root_path,
            build_path: PathBuf::from("/workspace/build/Core"),
            repository_use: crate::git::RepositoryUse {
                title: "Core".to_owned(),
                location: config::RepositoryEndpoint {
                    repository: "https://example.test/Core.git".parse().unwrap(),
                    base_url: "https://example.test/Core/".parse().unwrap(),
                },
                other_repos: Default::default(),
            },
            theme_path: Some(PathBuf::from("/workspace/theme")),
            only: None,
            source_materialization: crate::git::SourceMaterialization::Clean,
            server_binding: ServerBinding::default(),
            base_url_override: None,
        }
    }

    fn explicit_selectors(paths: &[&str]) -> EditorialSelectorArgs {
        EditorialSelectorArgs {
            paths: paths.iter().map(|path| PathBuf::from(*path)).collect(),
            batch: None,
            working_tree: false,
            against_upstream: false,
        }
    }

    #[test]
    fn editorial_lint_resolves_sibling_proposals_from_prepared_sources() {
        let workspace = editorial_workspace_with_missing_upstream(
            "Reference [ERC-2](./00002.md).",
            "Sibling proposal.",
        );

        assert!(run_lint(
            &workspace,
            &[
                "content/00001.md",
                "--no-default-lints",
                "-D",
                "markdown-refs"
            ]
        )
        .unwrap());
    }

    #[test]
    fn editorial_batch_lint_resolves_siblings_without_fetching_active_upstream() {
        let workspace = editorial_workspace_with_missing_upstream(
            "Reference [ERC-2](./00002.md).",
            "Sibling proposal.",
        );
        let batch_path = workspace.active_path.join("targets.txt");
        write_file(&workspace.active_path, "targets.txt", "content/00001.md\n");
        let batch_path = batch_path.to_str().unwrap();

        assert!(run_lint(
            &workspace,
            &[
                "--batch",
                batch_path,
                "--no-default-lints",
                "-D",
                "markdown-refs"
            ]
        )
        .unwrap());
    }

    #[test]
    fn editorial_working_tree_lint_uses_dirty_content_without_fetching_active_upstream() {
        let workspace = editorial_workspace_with_missing_upstream(
            "Reference [ERC-9999](./09999.md).",
            "Sibling proposal.",
        );
        write_file(
            &workspace.active_path,
            "content/00001.md",
            &proposal_markdown(1, None, "Reference [ERC-2](./00002.md)."),
        );

        assert!(run_lint(
            &workspace,
            &[
                "--working-tree",
                "--no-default-lints",
                "-D",
                "markdown-refs"
            ]
        )
        .unwrap());
    }

    #[test]
    fn editorial_against_upstream_lint_still_requires_active_upstream() {
        let workspace =
            editorial_workspace_with_missing_upstream("Active proposal.", "Sibling proposal.");

        let error = run_lint(
            &workspace,
            &[
                "--against-upstream",
                "--no-default-lints",
                "-D",
                "markdown-refs",
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("fetching upstream repo for editorial source preparation"));
    }

    #[test]
    fn editorial_lint_rejects_sibling_only_target_as_non_active_target() {
        let workspace = editorial_workspace("Active proposal.", "Sibling proposal.");

        let error = run_lint(
            &workspace,
            &[
                "content/00002.md",
                "--no-default-lints",
                "-D",
                "markdown-refs",
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("unable to resolve editorial target"));
    }

    #[test]
    fn editorial_lint_reports_untracked_targets_missing_from_prepared_sources() {
        let workspace = editorial_workspace("Active proposal.", "Sibling proposal.");
        write_file(
            &workspace.active_path,
            "content/00003.md",
            &proposal_markdown(3, None, "Untracked proposal."),
        );

        let error = run_lint(
            &workspace,
            &[
                "content/00003.md",
                "--no-default-lints",
                "-D",
                "markdown-refs",
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains(
            "editorial target `content/00003.md` exists in the active repo but was not materialized into the prepared source tree"
        ));
        assert!(error.contains("untracked files are not supported"));
    }

    #[test]
    fn editorial_lint_materializes_tracked_dirty_working_tree_targets() {
        let workspace =
            editorial_workspace("Reference [ERC-9999](./09999.md).", "Sibling proposal.");
        write_file(
            &workspace.active_path,
            "content/00001.md",
            &proposal_markdown(1, None, "Reference [ERC-2](./00002.md)."),
        );

        assert!(run_lint(
            &workspace,
            &[
                "--working-tree",
                "--no-default-lints",
                "-D",
                "markdown-refs"
            ]
        )
        .unwrap());
    }

    #[test]
    fn editorial_working_tree_check_still_forces_dirty_runtime_materialization() {
        let resolved = ResolvedExecution {
            root_path: PathBuf::from("/workspace/Core"),
            build_path: PathBuf::from("/workspace/build/Core"),
            repository_use: crate::git::RepositoryUse {
                title: "Core".to_owned(),
                location: config::RepositoryEndpoint {
                    repository: "https://example.test/Core.git".parse().unwrap(),
                    base_url: "https://example.test/Core/".parse().unwrap(),
                },
                other_repos: Default::default(),
            },
            theme_path: Some(PathBuf::from("/workspace/theme")),
            only: None,
            source_materialization: crate::git::SourceMaterialization::Clean,
            server_binding: ServerBinding::default(),
            base_url_override: None,
        };
        let selectors = EditorialSelectorArgs {
            paths: Vec::new(),
            batch: None,
            working_tree: true,
            against_upstream: false,
        };

        assert_eq!(
            editorial_runtime_execution(resolved, &selectors).source_materialization,
            crate::git::SourceMaterialization::Dirty
        );
    }

    #[test]
    fn editorial_explicit_numeric_selectors_resolve_to_markdown_paths() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/0004.md", "");
        let resolved = resolved_execution(temp.path().to_path_buf());

        for selector in ["4", "004", "0004"] {
            assert_eq!(
                editorial_targets(&explicit_selectors(&[selector]), &resolved).unwrap(),
                vec![PathBuf::from("content/0004.md")]
            );
        }
    }

    #[test]
    fn editorial_explicit_numeric_selectors_support_multiple_and_dedupe() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/0004.md", "");
        write_file(temp.path(), "content/0005/index.md", "");
        let resolved = resolved_execution(temp.path().to_path_buf());

        assert_eq!(
            editorial_targets(&explicit_selectors(&["4", "0004", "005"]), &resolved).unwrap(),
            vec![
                PathBuf::from("content/0004.md"),
                PathBuf::from("content/0005/index.md"),
            ]
        );
    }

    #[test]
    fn editorial_batch_accepts_numbers_paths_comments_and_empty_lines() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/0004.md", "");
        write_file(temp.path(), "content/0005/index.md", "");
        let batch_path = temp.path().join("targets.txt");
        write_file(
            temp.path(),
            "targets.txt",
            "\n# comment\n  \n4\ncontent/0005/index.md\n",
        );
        let resolved = resolved_execution(temp.path().to_path_buf());
        let selectors = EditorialSelectorArgs {
            paths: Vec::new(),
            batch: Some(batch_path),
            working_tree: false,
            against_upstream: false,
        };

        assert_eq!(
            editorial_targets(&selectors, &resolved).unwrap(),
            vec![
                PathBuf::from("content/0004.md"),
                PathBuf::from("content/0005/index.md"),
            ]
        );
    }

    #[test]
    fn editorial_explicit_repo_relative_path_selectors_still_work() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/0004.md", "");
        let resolved = resolved_execution(temp.path().to_path_buf());

        assert_eq!(
            editorial_targets(&explicit_selectors(&["content/0004.md"]), &resolved).unwrap(),
            vec![PathBuf::from("content/0004.md")]
        );
    }

    #[test]
    fn editorial_invalid_number_like_selectors_fail_with_editorial_error() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/0004.md", "");
        let resolved = resolved_execution(temp.path().to_path_buf());

        for selector in [
            "0",
            "+4",
            "-4",
            "4,5",
            "4,,5",
            ",4",
            "4,",
            "+",
            "-",
            "4294967296",
        ] {
            let error = editorial_targets(&explicit_selectors(&[selector]), &resolved)
                .unwrap_err()
                .to_string();
            assert!(error.contains(&format!(
                "editorial number selector `{selector}` is invalid"
            )));
            assert!(error.contains(
                "expected a positive proposal number that fits in u32, without signs or commas"
            ));
        }
    }

    #[test]
    fn editorial_path_like_selectors_continue_through_path_validation() {
        let temp = TempDir::new().unwrap();
        let resolved = resolved_execution(temp.path().to_path_buf());

        for selector in ["foo", "draft.md", "4a", "draft-4.md"] {
            write_file(temp.path(), selector, "");

            let error = editorial_targets(&explicit_selectors(&[selector]), &resolved)
                .unwrap_err()
                .to_string();

            assert!(error.contains("is not a supported proposal path"));
            assert!(!error.contains("editorial number selector"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn editorial_non_utf8_selector_continues_through_path_validation() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let temp = TempDir::new().unwrap();
        let resolved = resolved_execution(temp.path().to_path_buf());
        let selectors = EditorialSelectorArgs {
            paths: vec![PathBuf::from(OsStr::from_bytes(b"\xff"))],
            batch: None,
            working_tree: false,
            against_upstream: false,
        };

        let error = editorial_targets(&selectors, &resolved)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unable to resolve editorial target"));
        assert!(!error.contains("editorial number selector"));
    }

    #[test]
    fn non_strict_editorial_target_validation_does_not_normalize_numeric_paths() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "4", "");
        write_file(temp.path(), "content/0004.md", "");

        assert_eq!(
            validate_editorial_targets(temp.path(), vec![PathBuf::from("4")], false).unwrap(),
            Vec::<PathBuf>::new()
        );
    }
}
