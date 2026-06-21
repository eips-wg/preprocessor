#![cfg(test)]

// Cross-domain behavior tests live here; see src/README.md for module test ownership.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use clap::Parser;
use git2::{IndexAddOption, Repository, Signature};
use tempfile::TempDir;
use url::Url;

use crate::{
    cli::Args,
    config,
    execution::{resolve_execution, ResolvedExecution},
    layout::{BUILD_DIR, CONTENT_DIR, REPO_DIR},
    markdown,
    proposal::{OnlyRenderPlan, ProposalNumber},
};

fn parse_args(arguments: &[&str]) -> Args {
    Args::try_parse_from(arguments).unwrap()
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

fn build_manifest_text(repo_name: &str, repository: &Url, siblings: &[(&str, Url)]) -> String {
    let mut manifest = format!(
        r#"
name = "{repo_name}"

[locations.{repo_name}]
repository = "{repository}"
base-url = "https://example.test/{repo_name}/"

[theme]
repository = "https://example.test/theme.git"
commit = "test-theme-commit"
"#
    );

    for (sibling_id, sibling_repository) in siblings {
        manifest.push_str(&format!(
            r#"
[locations.{sibling_id}]
repository = "{sibling_repository}"
base-url = "https://example.test/{sibling_id}/"
"#
        ));
    }

    manifest
}

fn write_build_manifest_file(
    path: &Path,
    repo_name: &str,
    upstream: &Url,
    siblings: &[(&str, Url)],
) {
    write_file(
        path,
        config::MANIFEST_FILE,
        &build_manifest_text(repo_name, upstream, siblings),
    );
}

fn write_manifest_repo(
    path: &Path,
    repo_name: &str,
    upstream: &Url,
    siblings: &[(&str, Url)],
) -> Repository {
    let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
    write_build_manifest_file(path, repo_name, upstream, siblings);
    commit_all(&repo, "add Build.toml");
    repo
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
fn base_url_precedence_uses_cli_then_workspace_then_active_manifest() {
    let workspace = TempDir::new().unwrap();
    let workspace_root = workspace.path().join("workspace");
    let active_path = workspace_root.join("Core");
    let active_url = file_url(&active_path);
    write_manifest_repo(&active_path, "Core", &active_url, &[]);
    std::fs::create_dir(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
    write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");

    let fallback = resolve_execution(&parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "build",
    ]))
    .unwrap();
    assert!(fallback.base_url_override.is_none());
    assert_eq!(
        fallback.repository_use.location.base_url.as_str(),
        "https://example.test/Core/"
    );

    write_file(
        &workspace_root,
        config::LOCAL_CONFIG_FILE,
        r#"
[site]
base_url = "http://workspace.test"
"#,
    );
    let workspace_override = resolve_execution(&parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "build",
    ]))
    .unwrap();
    assert_eq!(
        workspace_override.base_url_override.unwrap().as_str(),
        "http://workspace.test/"
    );

    let cli_override = resolve_execution(&parse_args(&[
        "build-eips",
        "-C",
        active_path.to_str().unwrap(),
        "build",
        "--base-url",
        "http://cli.test",
    ]))
    .unwrap();
    assert_eq!(
        cli_override.base_url_override.unwrap().as_str(),
        "http://cli.test/"
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
    write_build_manifest_file(
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
    write_build_manifest_file(&active_path, "Core", &active_url, &[]);
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
    write_build_manifest_file(&active_path, "Core", &active_url, &[]);
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
    write_build_manifest_file(&active_path, "Core", &active_url, &[]);
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
    write_build_manifest_file(&active_path, "Core", &active_url, &[]);
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
