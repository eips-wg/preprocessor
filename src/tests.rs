#![cfg(test)]

// Cross-domain behavior tests live here; see src/README.md for module test ownership.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use clap::Parser;
use git2::{IndexAddOption, Repository, Signature};
use snafu::Report;
use tempfile::TempDir;
use url::Url;

use crate::{
    cli::{Args, EditorialCommand, EditorialSelectorArgs, Operation, RuntimeOperation},
    config::{self, LoadedWorkspaceConfig},
    editorial::editorial_targets_from_source,
    execution::{
        resolve_execution, resolve_execution_settings, validate_non_execution_command_flags,
        ExecutionSettings, ResolvedExecution, SelectedSource,
    },
    layout::{BUILD_DIR, CONTENT_DIR, REPO_DIR},
    markdown,
    proposal::{OnlyRenderPlan, ProposalNumber},
};

fn parse_args(arguments: &[&str]) -> Args {
    Args::try_parse_from(arguments).unwrap()
}

fn settings_for(
    arguments: &[&str],
    sibling_ids: &[&str],
    workspace_config: Option<&LoadedWorkspaceConfig>,
) -> ExecutionSettings {
    let args = parse_args(arguments);
    let sibling_ids = sibling_ids
        .iter()
        .map(|sibling_id| (*sibling_id).to_owned())
        .collect::<Vec<_>>();

    resolve_execution_settings(&args, &sibling_ids, workspace_config).unwrap()
}

fn assert_settings(
    arguments: &[&str],
    sibling_ids: &[&str],
    workspace_config: Option<&LoadedWorkspaceConfig>,
    expected: ExecutionSettings,
) {
    assert_eq!(
        settings_for(arguments, sibling_ids, workspace_config),
        expected
    );
}

fn file_url(path: &Path) -> Url {
    Url::from_directory_path(path).unwrap()
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

fn append_and_commit(repo: &Repository, root: &Path, files: &[(&str, &str)], message: &str) {
    for (relative, contents) in files {
        write_file(root, relative, contents);
    }
    commit_all(repo, message);
}

fn proposal_markdown(number: u32, category: Option<&str>, body: &str) -> String {
    let category = category
        .map(|category| format!("category: {category}\n"))
        .unwrap_or_default();
    format!("---\neip: {number}\ntitle: Proposal {number}\n{category}---\n{body}\n")
}

fn materialize_resolved_repo(resolved: &ResolvedExecution) -> PathBuf {
    let repo_path = resolved.build_path.join(REPO_DIR);
    crate::git::Fresh::new(
        &resolved.root_path,
        &repo_path,
        resolved.repository_use.clone(),
        resolved.source_materialization,
    )
    .unwrap()
    .clone_src()
    .unwrap()
    .fetch_upstream()
    .unwrap()
    .merge()
    .unwrap();

    repo_path
}

fn preprocess_and_prune_only(repo_path: &Path, selected: BTreeSet<ProposalNumber>) {
    let content_path = repo_path.join(CONTENT_DIR);
    let plan = OnlyRenderPlan::build(&content_path, selected).unwrap();
    markdown::preprocess(&content_path, Some(&plan)).unwrap();
    plan.prune_content(&content_path).unwrap();
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

fn write_repo_manifest_file(path: &Path, repo_id: &str, upstream: &Url, siblings: &[(&str, Url)]) {
    write_file(
        path,
        config::REPO_MANIFEST_FILE,
        &repo_manifest_text(repo_id, upstream, siblings),
    );
}

fn write_manifest_repo(
    path: &Path,
    repo_id: &str,
    upstream: &Url,
    siblings: &[(&str, Url)],
) -> Repository {
    let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
    write_repo_manifest_file(path, repo_id, upstream, siblings);
    commit_all(&repo, "add repo manifest");
    repo
}

#[test]
fn command_groups_route_separately_from_parity() {
    let init = parse_args(&["build-eips", "init", "/tmp/workspace"]);
    let doctor = parse_args(&["build-eips", "doctor"]);
    let editorial_lint = parse_args(&["build-eips", "editorial", "lint", "--working-tree"]);
    let editorial_check = parse_args(&["build-eips", "editorial", "check", "--working-tree"]);

    assert!(matches!(init.operation, Operation::Init { .. }));
    assert!(matches!(doctor.operation, Operation::Doctor));
    assert!(matches!(
        editorial_lint.operation.runtime_operation(),
        Some(RuntimeOperation::Editorial {
            command: EditorialCommand::Lint { .. }
        })
    ));
    assert!(matches!(
        editorial_check.operation.runtime_operation(),
        Some(RuntimeOperation::Editorial {
            command: EditorialCommand::Check { .. }
        })
    ));
    assert!(validate_non_execution_command_flags(&editorial_lint).is_ok());
}

#[test]
fn downstream_ci_changed_forms_do_not_need_workspace_config() {
    for (arguments, expected_staging) in [
        (&["build-eips", "--staging", "changed"][..], true),
        (&["build-eips", "--production", "changed"][..], false),
    ] {
        assert_settings(
            arguments,
            &["ERCs"],
            None,
            ExecutionSettings {
                build_root: None,
                staging: expected_staging,
                allow_dirty: false,
                sibling: SelectedSource::Remote,
            },
        );
    }
}

#[test]
fn downstream_ci_zola_forms_require_workspace_local_theme_config() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    let active_path = active_path.to_string_lossy().to_string();

    for arguments in [
        &["build-eips", "--staging", "build"][..],
        &["build-eips", "--production", "build"][..],
        &[
            "build-eips",
            "--staging",
            "editorial",
            "check",
            "--against-upstream",
        ][..],
        &["build-eips", "parity", "check"][..],
    ] {
        let mut cli_arguments = vec!["build-eips", "-C", active_path.as_str()];
        cli_arguments.extend_from_slice(&arguments[1..]);
        let args = parse_args(&cli_arguments);
        let message = resolve_execution(&args).unwrap_err().to_string();

        assert!(message.contains("requires a workspace config with a local theme"));
        assert!(message.contains("build-eips init <workspace-root>"));
    }
}

#[test]
fn manifest_identity_drives_runtime_resolution() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    std::fs::create_dir(workspace.path().join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        workspace.path(),
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    let build_root = workspace.path().join("build-root");
    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "parity",
        "build",
    ]);

    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(resolved.repository_use.title, "Core");
    assert_eq!(resolved.repository_use.location.repository, active_url);
    assert!(resolved.repository_use.other_repos.is_empty());
    assert_eq!(resolved.build_path, build_root);
}

#[test]
fn execution_commands_discover_workspace_config_from_active_repo_root() {
    let workspace = TempDir::new().unwrap();
    let workspace_root = workspace.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "build"]);

    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(
        resolved.build_path,
        workspace_root
            .join(config::DEFAULT_BUILD_ROOT_BASE)
            .join("Core")
    );
    assert_eq!(
        resolved.source_materialization,
        crate::git::SourceMaterialization::Dirty
    );
}

#[test]
fn build_root_override_wins_with_workspace_config() {
    let workspace = TempDir::new().unwrap();
    let workspace_root = workspace.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    let build_root = workspace.path().join("override-build-root");
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "build",
    ]);

    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(resolved.build_path, build_root);
}

#[test]
fn non_workspace_runtime_path_falls_back_to_active_repo_build_dir() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "changed"]);

    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(resolved.build_path, active_path.join(BUILD_DIR));
}

#[test]
fn non_theme_runtime_commands_resolve_without_workspace_config() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    let active_path = active_path.to_string_lossy().to_string();

    for command in ["changed", "clean", "preview"] {
        let args = parse_args(&["build-eips", "-C", active_path.as_str(), command]);
        let resolved = resolve_execution(&args).unwrap();

        assert!(resolved.theme_path.is_none());
    }
}

#[test]
fn unknown_repo_without_manifest_or_legacy_identity_errors() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Unknown");
    init_repo(&active_path, &[("content/0001.md", "# Proposal\n")]);
    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "parity",
        "build",
    ]);

    let error = resolve_execution(&args).unwrap_err();
    let message = error.to_string();

    assert!(message.contains(config::REPO_MANIFEST_FILE));
    assert!(message.contains("legacy EIPs/ERCs identity fallback"));
}

#[test]
fn malformed_repo_manifest_does_not_fall_back_to_legacy_identity() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Malformed");
    init_repo(&active_path, &[("content/0001.md", "# Proposal\n")]);
    write_file(&active_path, config::REPO_MANIFEST_FILE, "repo_id = [");
    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "parity",
        "build",
    ]);

    let message = Report::from_error(resolve_execution(&args).unwrap_err()).to_string();

    assert!(message.contains("unable to load repo manifest"));
    assert!(!message.contains("legacy EIPs/ERCs identity fallback"));
}

#[test]
fn workspace_local_sibling_mode_is_all_or_nothing() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let eips_path = workspace.path().join("EIPs");
    init_repo(&eips_path, &[("content/0002.md", "# EIP\n")]);
    let siblings = vec![
        ("EIPs", file_url(&eips_path)),
        ("ERCs", file_url(&workspace.path().join("remotes/ERCs"))),
    ];
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &siblings);
    std::fs::create_dir(workspace.path().join(config::DEFAULT_THEME_DIR)).unwrap();
    std::fs::write(
        workspace.path().join(config::LOCAL_CONFIG_FILE),
        config::default_workspace_config_text(),
    )
    .unwrap();
    let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "build"]);

    let error = resolve_execution(&args).unwrap_err();
    let message = error.to_string();

    assert!(message.contains("requires all declared sibling repos"));
    assert!(message.contains("ERCs"));
}

#[test]
fn workspace_local_sources_resolve_from_standard_layout() {
    let workspace = TempDir::new().unwrap();
    let active_path = workspace.path().join("Core");
    let eips_path = workspace.path().join("EIPs");
    let ercs_path = workspace.path().join("ERCs");
    init_repo(&eips_path, &[("content/0002.md", "# EIP\n")]);
    init_repo(&ercs_path, &[("content/0003.md", "# ERC\n")]);
    std::fs::create_dir(workspace.path().join(config::DEFAULT_THEME_DIR)).unwrap();
    let siblings = vec![
        ("EIPs", file_url(&eips_path)),
        ("ERCs", file_url(&ercs_path)),
    ];
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &siblings);
    std::fs::write(
        workspace.path().join(config::LOCAL_CONFIG_FILE),
        config::default_workspace_config_text(),
    )
    .unwrap();
    let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "build"]);

    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(
        resolved.theme_path.as_deref(),
        Some(workspace.path().join(config::DEFAULT_THEME_DIR).as_path())
    );
    assert_eq!(
        resolved.repository_use.other_repos["EIPs"],
        file_url(&eips_path)
    );
    assert_eq!(
        resolved.repository_use.other_repos["ERCs"],
        file_url(&ercs_path)
    );
}

#[test]
fn manifest_driven_multi_repo_build_and_editorial_flows_resolve_siblings() {
    let temp = TempDir::new().unwrap();
    let upstream_path = temp.path().join("upstream/Core");
    init_repo(
        &upstream_path,
        &[("content/0001.md", "# Original proposal\n")],
    );
    let upstream_url = file_url(&upstream_path);

    let active_path = temp.path().join("workspace/Core");
    std::fs::create_dir_all(active_path.parent().unwrap()).unwrap();
    std::fs::create_dir(temp.path().join("workspace/theme")).unwrap();
    write_file(
        active_path.parent().unwrap(),
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    git2::build::RepoBuilder::new()
        .clone(upstream_url.as_str(), &active_path)
        .unwrap();
    let active_repo = Repository::open(&active_path).unwrap();

    let eips_path = temp.path().join("remotes/EIPs");
    init_repo(&eips_path, &[("content/0002.md", "# EIP sibling\n")]);
    let ercs_path = temp.path().join("remotes/ERCs");
    init_repo(&ercs_path, &[("content/0003.md", "# ERC sibling\n")]);
    let siblings = vec![
        ("EIPs", file_url(&eips_path)),
        ("ERCs", file_url(&ercs_path)),
    ];
    write_repo_manifest_file(&active_path, "Core", &upstream_url, &siblings);
    append_and_commit(
        &active_repo,
        &active_path,
        &[("content/0001.md", "# Updated proposal\n")],
        "local proposal update",
    );
    let build_root = temp.path().join("build-root");
    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "parity",
        "build",
    ]);
    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(resolved.repository_use.other_repos.len(), 2);

    let repo_path = resolved.build_path.join(REPO_DIR);
    let source = crate::git::Fresh::new(
        &resolved.root_path,
        &repo_path,
        resolved.repository_use.clone(),
        resolved.source_materialization,
    )
    .unwrap()
    .clone_src()
    .unwrap()
    .fetch_upstream()
    .unwrap();

    let selectors = EditorialSelectorArgs {
        paths: Vec::<PathBuf>::new(),
        batch: None,
        working_tree: false,
        against_upstream: true,
    };
    let targets = editorial_targets_from_source(&selectors, &resolved, Some(&source)).unwrap();

    source.merge().unwrap();

    assert!(repo_path.join("content/0002.md").is_file());
    assert!(repo_path.join("content/0003.md").is_file());

    assert_eq!(targets, vec![PathBuf::from("content/0001.md")]);
}

#[test]
fn only_build_selection_can_come_from_workspace_local_sibling_after_merge() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let sibling_path = workspace_root.join("ERCs");
    let active_url = file_url(&active_path);

    let active_555 = proposal_markdown(555, None, "Active proposal.");
    let active_repo = init_repo(&active_path, &[("content/00555.md", active_555.as_str())]);
    let sibling_678 = proposal_markdown(678, Some("ERC"), "Sibling proposal.");
    init_repo(&sibling_path, &[("content/00678.md", sibling_678.as_str())]);
    write_repo_manifest_file(
        &active_path,
        "Core",
        &active_url,
        &[("ERCs", file_url(&sibling_path))],
    );
    commit_all(&active_repo, "add manifest");
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );

    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "build",
        "--only",
        "678",
    ]);
    let resolved = resolve_execution(&args).unwrap();
    let repo_path = materialize_resolved_repo(&resolved);
    let selected = resolved.only.clone().unwrap();

    preprocess_and_prune_only(&repo_path, selected);

    assert!(repo_path.join("content/00678.md").is_file());
    assert!(!repo_path.join("content/00555.md").exists());
}

#[test]
fn normal_build_after_only_restores_full_materialized_content_tree() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    let selected_555 = proposal_markdown(555, None, "Selected proposal.");
    let unselected_678 = proposal_markdown(678, Some("ERC"), "Unselected proposal.");
    let active_repo = init_repo(
        &active_path,
        &[
            ("content/00555.md", selected_555.as_str()),
            ("content/00678.md", unselected_678.as_str()),
        ],
    );
    write_repo_manifest_file(&active_path, "Core", &active_url, &[]);
    commit_all(&active_repo, "add manifest");
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    let build_root = temp.path().join("build-root");

    let only_args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "build",
        "--only",
        "555",
    ]);
    let only_resolved = resolve_execution(&only_args).unwrap();
    let repo_path = materialize_resolved_repo(&only_resolved);
    preprocess_and_prune_only(&repo_path, only_resolved.only.clone().unwrap());

    assert!(repo_path.join("content/00555.md").is_file());
    assert!(!repo_path.join("content/00678.md").exists());

    let normal_args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "build",
    ]);
    let normal_resolved = resolve_execution(&normal_args).unwrap();
    assert!(normal_resolved.only.is_none());
    let restored_repo_path = materialize_resolved_repo(&normal_resolved);
    markdown::preprocess(&restored_repo_path.join(CONTENT_DIR), None).unwrap();

    assert!(restored_repo_path.join("content/00555.md").is_file());
    assert!(restored_repo_path.join("content/00678.md").is_file());
}

#[test]
fn serve_applies_render_only_config_in_phase_two() {
    let workspace = TempDir::new().unwrap();
    let workspace_root = workspace.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    let active_555 = proposal_markdown(555, None, "Active proposal.");
    let active_repo = init_repo(&active_path, &[("content/00555.md", active_555.as_str())]);
    write_repo_manifest_file(&active_path, "Core", &active_url, &[]);
    commit_all(&active_repo, "add manifest");
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        r#"
[render]
only = [555]
"#,
    );

    let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "serve"]);
    let resolved = resolve_execution(&args).unwrap();

    assert_eq!(
        resolved
            .only
            .unwrap()
            .into_iter()
            .map(|number| number.get())
            .collect::<Vec<_>>(),
        vec![555]
    );
}

#[test]
fn only_serve_startup_uses_build_filtering() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    let selected_555 = proposal_markdown(555, None, "Selected proposal.");
    let unselected_678 = proposal_markdown(678, Some("ERC"), "Unselected proposal.");
    let active_repo = init_repo(
        &active_path,
        &[
            ("content/00555.md", selected_555.as_str()),
            ("content/00678.md", unselected_678.as_str()),
        ],
    );
    write_repo_manifest_file(&active_path, "Core", &active_url, &[]);
    commit_all(&active_repo, "add manifest");
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );

    let args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "serve",
        "--only",
        "555",
    ]);
    let resolved = resolve_execution(&args).unwrap();
    let repo_path = materialize_resolved_repo(&resolved);
    preprocess_and_prune_only(&repo_path, resolved.only.clone().unwrap());

    assert!(repo_path.join("content/00555.md").is_file());
    assert!(!repo_path.join("content/00678.md").exists());
}

#[test]
fn normal_serve_after_only_restores_full_materialized_content_tree() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    let selected_555 = proposal_markdown(555, None, "Selected proposal.");
    let unselected_678 = proposal_markdown(678, Some("ERC"), "Unselected proposal.");
    let active_repo = init_repo(
        &active_path,
        &[
            ("content/00555.md", selected_555.as_str()),
            ("content/00678.md", unselected_678.as_str()),
        ],
    );
    write_repo_manifest_file(&active_path, "Core", &active_url, &[]);
    commit_all(&active_repo, "add manifest");
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        &config::default_workspace_config_text(),
    );
    let build_root = temp.path().join("build-root");

    let only_args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "serve",
        "--only",
        "555",
    ]);
    let only_resolved = resolve_execution(&only_args).unwrap();
    let repo_path = materialize_resolved_repo(&only_resolved);
    preprocess_and_prune_only(&repo_path, only_resolved.only.clone().unwrap());

    assert!(repo_path.join("content/00555.md").is_file());
    assert!(!repo_path.join("content/00678.md").exists());

    let normal_args = parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "--build-root",
        build_root.to_str().unwrap(),
        "serve",
    ]);
    let normal_resolved = resolve_execution(&normal_args).unwrap();
    assert!(normal_resolved.only.is_none());
    let restored_repo_path = materialize_resolved_repo(&normal_resolved);
    markdown::preprocess(&restored_repo_path.join(CONTENT_DIR), None).unwrap();

    assert!(restored_repo_path.join("content/00555.md").is_file());
    assert!(restored_repo_path.join("content/00678.md").is_file());
}
