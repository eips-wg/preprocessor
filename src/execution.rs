/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Execution source and path resolution.

use std::{
    collections::BTreeSet,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use log::{debug, info};
use snafu::{OptionExt, ResultExt, Whatever};
use url::Url;

use crate::{
    cli::{Args, Operation, ServerCliArgs},
    config::{self, ActiveRepo, LoadedWorkspaceConfig, RepositoryUse, ServerBinding},
    context::{resolve_input_path, root},
    git,
    layout::BUILD_DIR,
    proposal::ProposalNumber,
};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedExecution {
    pub(crate) root_path: PathBuf,
    pub(crate) build_path: PathBuf,
    pub(crate) repository_use: RepositoryUse,
    pub(crate) theme_path: Option<PathBuf>,
    pub(crate) only: Option<BTreeSet<ProposalNumber>>,
    pub(crate) source_materialization: git::SourceMaterialization,
    pub(crate) server_binding: ServerBinding,
    pub(crate) base_url_override: Option<Url>,
}

impl ResolvedExecution {
    pub(crate) fn theme_path(&self) -> Result<&Path, Whatever> {
        self.theme_path
            .as_deref()
            .whatever_context("the selected command requires a resolved workspace-local theme")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectedSource {
    WorkspaceLocal,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionSettings {
    pub(crate) build_root: Option<PathBuf>,
    pub(crate) allow_dirty: bool,
    pub(crate) sibling: SelectedSource,
}

fn has_execution_override_flags(args: &Args) -> bool {
    args.remote_siblings || args.build_root.is_some()
}

pub(crate) fn validate_non_execution_command_flags(args: &Args) -> Result<(), Whatever> {
    if args.operation.is_workspace_lifecycle_command() && has_execution_override_flags(args) {
        snafu::whatever!("execution override flags cannot be used with `init` or `doctor`");
    }

    if args.operation.is_print_command() && has_execution_override_flags(args) {
        snafu::whatever!("execution override flags cannot be used with `print`");
    }

    Ok(())
}

fn remote_source_override(force_remote: bool) -> Option<SelectedSource> {
    force_remote.then_some(SelectedSource::Remote)
}

fn format_sibling_ids(sibling_ids: &[String]) -> String {
    sibling_ids.join(", ")
}

fn cli_only_requested(args: &Args) -> bool {
    args.operation
        .only_cli_args()
        .map(|only| !only.only.is_empty())
        .unwrap_or(false)
}

fn only_cli_is_applicable(args: &Args) -> bool {
    matches!(
        args.operation,
        Operation::Build { .. } | Operation::Serve { .. }
    ) && !args.operation.clean_cli_args().clean
        && !args.remote_siblings
}

pub(crate) fn resolve_execution_settings(
    args: &Args,
    sibling_ids: &[String],
    workspace_config: Option<&LoadedWorkspaceConfig>,
) -> Result<ExecutionSettings, Whatever> {
    let build_root = args
        .build_root
        .as_deref()
        .map(resolve_input_path)
        .transpose()?;
    let sibling_override = remote_source_override(args.remote_siblings);
    let clean = args.operation.clean_cli_args().clean;

    if cli_only_requested(args) && !only_cli_is_applicable(args) {
        snafu::whatever!("--only is supported only for local dirty build and serve commands");
    }

    let (allow_dirty, default_sibling) =
        if args.operation.is_plain_site_command() || args.operation.is_editorial_command() {
            (!clean, SelectedSource::WorkspaceLocal)
        } else {
            (false, SelectedSource::Remote)
        };

    let missing_theme = operation_requires_theme(&args.operation) && workspace_config.is_none();
    let missing_sibling = sibling_override.is_none()
        && default_sibling == SelectedSource::WorkspaceLocal
        && !sibling_ids.is_empty()
        && workspace_config.is_none();

    match (missing_theme, missing_sibling) {
        (true, true) => {
            snafu::whatever!(
                "the selected command requires a workspace config with local theme and sibling sources, but no `{}` was found.\n\nRun:\n  build-eips init <workspace-root>\n\nThen retry from that workspace, or pass `--remote-siblings` if you intentionally want remote sibling proposal sources.",
                config::LOCAL_CONFIG_FILE
            );
        }
        (false, true) => {
            snafu::whatever!(
                "the selected command requires workspace-local sibling sources, but no `{}` was found to provide them.\nResolve this by doing one of the following:\n1. run `build-eips init <workspace-root>` so the workspace config supplies the local sources\n2. pass `--remote-siblings` for remote sibling source overrides",
                config::LOCAL_CONFIG_FILE
            );
        }
        _ => {}
    }

    Ok(ExecutionSettings {
        build_root,
        allow_dirty,
        sibling: sibling_override.unwrap_or(default_sibling),
    })
}

fn dedupe_only_numbers(numbers: &[ProposalNumber]) -> Option<BTreeSet<ProposalNumber>> {
    let numbers = numbers.iter().copied().collect::<BTreeSet<_>>();
    (!numbers.is_empty()).then_some(numbers)
}

fn resolve_only_selection(
    args: &Args,
    settings: &ExecutionSettings,
    workspace_config: Option<&LoadedWorkspaceConfig>,
) -> Result<Option<BTreeSet<ProposalNumber>>, Whatever> {
    let applicable = matches!(
        args.operation,
        Operation::Build { .. } | Operation::Serve { .. }
    ) && settings.allow_dirty
        && settings.sibling == SelectedSource::WorkspaceLocal;

    if let Some(only) = args.operation.only_cli_args() {
        if let Some(numbers) = dedupe_only_numbers(&only.only) {
            if !applicable {
                snafu::whatever!(
                    "--only is supported only for local dirty build and serve commands"
                );
            }
            return Ok(Some(numbers));
        }
    }

    if !applicable {
        return Ok(None);
    }

    Ok(workspace_config
        .and_then(|workspace_config| dedupe_only_numbers(&workspace_config.render_settings().only)))
}

fn local_repo_url(path: &Path) -> Result<Url, Whatever> {
    Url::from_directory_path(path)
        .ok()
        .whatever_context("unable to convert local sibling repository path into a file URL")
}

fn apply_sibling_sources(
    repository_use: &mut RepositoryUse,
    sibling_ids: &[String],
    workspace_config: Option<&LoadedWorkspaceConfig>,
    sibling: &SelectedSource,
) -> Result<(), Whatever> {
    match sibling {
        SelectedSource::Remote => Ok(()),
        SelectedSource::WorkspaceLocal => {
            if sibling_ids.is_empty() {
                return Ok(());
            }

            let workspace_config = workspace_config.whatever_context(
                "workspace-local sibling selection requires a workspace config",
            )?;
            let mut missing = Vec::new();
            let mut local_repositories = Vec::new();

            for repo_id in sibling_ids {
                let path = workspace_config.local_repo_path(repo_id);
                if git::repository_available(&path) {
                    local_repositories.push((repo_id.clone(), local_repo_url(&path)?));
                } else {
                    missing.push(repo_id.clone());
                }
            }

            if !missing.is_empty() {
                snafu::whatever!(
                    "workspace-local sibling selection requires all declared sibling repos; missing or invalid sibling repo(s): {}",
                    format_sibling_ids(&missing)
                );
            }

            for (repo_id, url) in local_repositories {
                repository_use.other_repos.insert(repo_id, url);
            }

            Ok(())
        }
    }
}

fn build_path(
    root_path: &Path,
    repository_use: &RepositoryUse,
    workspace_config: Option<&LoadedWorkspaceConfig>,
    build_root: Option<&Path>,
) -> PathBuf {
    build_root
        .map(Path::to_path_buf)
        .or_else(|| {
            workspace_config.map(|workspace_config| {
                workspace_config.workspace_build_root(&repository_use.title)
            })
        })
        .unwrap_or_else(|| root_path.join(BUILD_DIR))
}

fn operation_requires_theme(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Build { .. }
            | Operation::Serve { .. }
            | Operation::Check { .. }
            | Operation::Editorial { .. }
    )
}

fn resolve_theme_path(
    workspace_config: Option<&LoadedWorkspaceConfig>,
    operation: &Operation,
) -> Result<Option<PathBuf>, Whatever> {
    if !operation_requires_theme(operation) {
        return Ok(None);
    }

    let workspace_config = workspace_config.with_whatever_context(|| {
        format!(
            "the selected command requires a workspace config with a local theme, but no `{}` was found.\n\nRun:\n  build-eips init <workspace-root>\n\nThen retry from that workspace.",
            config::LOCAL_CONFIG_FILE
        )
    })?;
    let theme_path = workspace_config.local_theme_path();

    match std::fs::metadata(&theme_path) {
        Ok(_) => Ok(Some(theme_path)),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            snafu::whatever!(
                "workspace-local theme path `{}` does not exist.\n\nRun `build-eips init <workspace-root>` to bootstrap the workspace, or\nclone/update the theme repository at the configured path.",
                theme_path.to_string_lossy()
            );
        }
        Err(error) => {
            snafu::whatever!(
                "unable to access workspace-local theme path `{}`: {error}",
                theme_path.to_string_lossy()
            );
        }
    }
}

fn resolve_server_binding(
    workspace_config: Option<&LoadedWorkspaceConfig>,
    server_cli: &ServerCliArgs,
) -> ServerBinding {
    let mut binding = workspace_config
        .map(|workspace_config| ServerBinding::from(workspace_config.server_settings()))
        .unwrap_or_default();

    if let Some(host) = &server_cli.host {
        binding.host = host.clone();
    }

    if let Some(port) = server_cli.port {
        binding.port = port;
    }

    binding
}

fn resolve_base_url_override(
    args: &Args,
    workspace_config: Option<&LoadedWorkspaceConfig>,
) -> Result<Option<Url>, Whatever> {
    if let Some(base_url) = args.operation.base_url_cli_args().base_url {
        return Ok(Some(base_url));
    }

    Ok(workspace_config.and_then(|config| config.site_settings().base_url.clone()))
}

fn clean_active_source_requested(args: &Args) -> bool {
    args.operation.clean_cli_args().clean || matches!(args.operation, Operation::Changed { .. })
}

pub(crate) fn resolve_execution(args: &Args) -> Result<ResolvedExecution, Whatever> {
    let root_path = root(args)?;

    if clean_active_source_requested(args) {
        git::check_dirty(&root_path)
            .whatever_context("clean active-source mode requires a clean active checkout")?;
    }

    let active_repo = ActiveRepo::load(&root_path)
        .whatever_context("unable to load active repository Build.toml")?;
    let sibling_ids = active_repo.sibling_ids.clone();
    let workspace_config = LoadedWorkspaceConfig::discover(&root_path)
        .whatever_context("unable to load workspace config")?;

    if let Some(workspace_config) = workspace_config.as_ref() {
        debug!(
            "using workspace config `{}`",
            workspace_config.config_path().to_string_lossy()
        );
    }

    let settings = resolve_execution_settings(args, &sibling_ids, workspace_config.as_ref())?;
    let only = resolve_only_selection(args, &settings, workspace_config.as_ref())?;
    let theme_path = resolve_theme_path(workspace_config.as_ref(), &args.operation)?;

    let mut repository_use = active_repo.repository_use;
    apply_sibling_sources(
        &mut repository_use,
        &sibling_ids,
        workspace_config.as_ref(),
        &settings.sibling,
    )?;

    let build_path = build_path(
        &root_path,
        &repository_use,
        workspace_config.as_ref(),
        settings.build_root.as_deref(),
    );
    let source_materialization = if settings.allow_dirty {
        info!(
            "dirty mode is enabled; tracked working-tree changes from the active content repo will be materialized into the build input"
        );
        git::SourceMaterialization::Dirty
    } else {
        git::SourceMaterialization::Clean
    };
    let base_url_override = resolve_base_url_override(args, workspace_config.as_ref())?;

    Ok(ResolvedExecution {
        root_path,
        build_path,
        repository_use,
        theme_path,
        only,
        source_materialization,
        server_binding: resolve_server_binding(
            workspace_config.as_ref(),
            &args.operation.server_cli_args(),
        ),
        base_url_override,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::TempDir;

    use crate::{
        cli::{Args, ServerCliArgs},
        config::{self, LoadedWorkspaceConfig, ServerBinding},
    };

    use super::{
        resolve_execution_settings, resolve_only_selection, resolve_server_binding,
        resolve_theme_path, SelectedSource,
    };

    fn parse_args(arguments: &[&str]) -> Args {
        Args::try_parse_from(arguments).unwrap()
    }

    fn load_workspace_config(contents: &str) -> LoadedWorkspaceConfig {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join(config::LOCAL_CONFIG_FILE);
        std::fs::write(&config_path, contents).unwrap();
        LoadedWorkspaceConfig::from_path(&config_path).unwrap()
    }

    fn assert_theme_only_missing_workspace_error(arguments: &[&str]) {
        let args = parse_args(arguments);
        let error = resolve_theme_path(None, &args.operation).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(
            "the selected command requires a workspace config with a local theme, but no `.build-eips.toml` was found"
        ));
        assert!(message.contains("build-eips init <workspace-root>"));
        assert!(!message.contains("theme and sibling"));
        assert!(!message.contains(concat!("--remote", "-theme")));
    }

    fn assert_combined_missing_workspace_error(arguments: &[&str]) {
        let args = parse_args(arguments);
        let sibling_ids = vec!["ERCs".to_owned()];
        let error = resolve_execution_settings(&args, &sibling_ids, None).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(
            "the selected command requires a workspace config with local theme and sibling sources"
        ));
        assert!(message.contains("no `.build-eips.toml` was found"));
        assert!(message.contains("build-eips init <workspace-root>"));
        assert!(message.contains(
            "pass `--remote-siblings` if you intentionally want remote sibling proposal sources"
        ));
        assert!(!message.contains(concat!("--remote", "-theme")));
        assert!(!message.contains("--profile"));
        assert!(!message.contains("--allow-dirty"));
        assert!(!message.contains("--theme <path>"));
        assert!(!message.contains("--sibling-repo <path>"));
    }

    fn only_selection_for(
        arguments: &[&str],
        workspace_config: Option<&LoadedWorkspaceConfig>,
    ) -> Option<Vec<u32>> {
        let args = parse_args(arguments);
        let settings = resolve_execution_settings(&args, &[], workspace_config).unwrap();
        resolve_only_selection(&args, &settings, workspace_config)
            .unwrap()
            .map(|numbers| numbers.into_iter().map(|number| number.get()).collect())
    }

    #[test]
    fn server_binding_resolution_uses_cli_config_then_defaults() {
        assert_eq!(
            resolve_server_binding(None, &ServerCliArgs::default()),
            ServerBinding {
                host: "127.0.0.1".to_owned(),
                port: 1111,
            }
        );

        let workspace_config = load_workspace_config(
            r#"
[server]
host = "0.0.0.0"
port = 8080
"#,
        );

        assert_eq!(
            resolve_server_binding(Some(&workspace_config), &ServerCliArgs::default()),
            ServerBinding {
                host: "0.0.0.0".to_owned(),
                port: 8080,
            }
        );
        assert_eq!(
            resolve_server_binding(
                Some(&workspace_config),
                &ServerCliArgs {
                    host: Some("127.0.0.1".to_owned()),
                    port: Some(4000),
                },
            ),
            ServerBinding {
                host: "127.0.0.1".to_owned(),
                port: 4000,
            }
        );
        assert_eq!(
            resolve_server_binding(
                Some(&workspace_config),
                &ServerCliArgs {
                    host: None,
                    port: Some(4000),
                },
            ),
            ServerBinding {
                host: "0.0.0.0".to_owned(),
                port: 4000,
            }
        );
    }

    #[test]
    fn non_theme_commands_do_not_require_workspace_local_theme() {
        for arguments in [
            &["build-eips", "changed"][..],
            &["build-eips", "clean"][..],
            &["build-eips", "preview"][..],
            &["build-eips", "doctor"][..],
            &["build-eips", "print", "schema-version"][..],
        ] {
            let args = parse_args(arguments);

            assert!(resolve_theme_path(None, &args.operation).unwrap().is_none());
        }
    }

    #[test]
    fn only_selection_dedupes_and_cli_replaces_config() {
        let workspace_config = load_workspace_config(
            r#"
[render]
only = [678, 555, 678]
"#,
        );

        assert_eq!(
            only_selection_for(&["build-eips", "build"], Some(&workspace_config)).unwrap(),
            vec![555, 678]
        );
        assert_eq!(
            only_selection_for(
                &["build-eips", "build", "--only", "00555", "555", "897"],
                Some(&workspace_config)
            )
            .unwrap(),
            vec![555, 897]
        );
        assert_eq!(
            only_selection_for(&["build-eips", "serve"], Some(&workspace_config)).unwrap(),
            vec![555, 678]
        );
        assert_eq!(
            only_selection_for(
                &["build-eips", "serve", "--only", "00555", "555", "897"],
                Some(&workspace_config)
            )
            .unwrap(),
            vec![555, 897]
        );
    }

    #[test]
    fn missing_render_config_and_empty_only_disable_filtering() {
        let missing_render = load_workspace_config("");
        let missing_only = load_workspace_config("[render]\n");
        let empty_only = load_workspace_config(
            r#"
[render]
only = []
"#,
        );

        assert!(only_selection_for(&["build-eips", "build"], Some(&missing_render)).is_none());
        assert!(only_selection_for(&["build-eips", "build"], Some(&missing_only)).is_none());
        assert!(only_selection_for(&["build-eips", "build"], Some(&empty_only)).is_none());
        assert!(only_selection_for(&["build-eips", "serve"], Some(&missing_render)).is_none());
        assert!(only_selection_for(&["build-eips", "serve"], Some(&missing_only)).is_none());
        assert!(only_selection_for(&["build-eips", "serve"], Some(&empty_only)).is_none());
    }

    #[test]
    fn local_first_theme_commands_without_workspace_config_report_combined_setup_error() {
        for arguments in [
            &["build-eips", "build"][..],
            &["build-eips", "serve"][..],
            &["build-eips", "check"][..],
            &["build-eips", "editorial", "lint", "content/0001.md"][..],
            &["build-eips", "editorial", "check", "content/0001.md"][..],
        ] {
            assert_combined_missing_workspace_error(arguments);
        }
    }

    #[test]
    fn zero_sibling_local_first_without_workspace_config_only_requires_theme_resolution() {
        let args = parse_args(&["build-eips", "build"]);
        let settings = resolve_execution_settings(&args, &[], None).unwrap();

        assert_eq!(settings.sibling, SelectedSource::WorkspaceLocal);
        assert_theme_only_missing_workspace_error(&["build-eips", "build"]);
    }

    #[test]
    fn remote_sibling_override_without_workspace_config_only_requires_theme_resolution() {
        let args = parse_args(&["build-eips", "--remote-siblings", "build"]);
        let sibling_ids = vec!["ERCs".to_owned()];
        let settings = resolve_execution_settings(&args, &sibling_ids, None).unwrap();

        assert_eq!(settings.sibling, SelectedSource::Remote);
        assert_theme_only_missing_workspace_error(&["build-eips", "--remote-siblings", "build"]);
    }

    #[test]
    fn missing_workspace_theme_path_reports_clear_error() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join(config::LOCAL_CONFIG_FILE);
        std::fs::write(&config_path, "").unwrap();
        let workspace_config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();
        let args = parse_args(&["build-eips", "build"]);

        let error = resolve_theme_path(Some(&workspace_config), &args.operation).unwrap_err();
        let message = error.to_string();

        assert!(message.contains(&format!(
            "workspace-local theme path `{}` does not exist",
            workspace
                .path()
                .join(config::DEFAULT_THEME_DIR)
                .to_string_lossy()
        )));
        assert!(message.contains("build-eips init <workspace-root>"));
    }
}

#[cfg(test)]
mod active_manifest_clean_tests {
    use std::path::Path;

    use clap::Parser;
    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;

    use crate::cli::Args;

    use super::resolve_execution;

    fn write_file(root: &Path, relative: &str, contents: &str) {
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
        repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &[])
            .unwrap();
    }

    fn dirty_active_repo() -> TempDir {
        let tempdir = TempDir::new().unwrap();
        let repo = Repository::init(tempdir.path()).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        write_file(
            tempdir.path(),
            "Build.toml",
            r#"
name = "EIPs"

[locations.EIPs]
repository = "https://example.test/EIPs.git"
base-url = "https://example.test/EIPs/"

[theme]
repository = "https://example.test/theme.git"
commit = "test-theme-commit"
"#,
        );
        write_file(tempdir.path(), "content/00001.md", "# Proposal\n");
        commit_all(&repo, "initial manifest");
        write_file(
            tempdir.path(),
            "Build.toml",
            r#"
name = "EIPs"

[locations.EIPs]
repository = "https://example.test/EIPs.git"
base-url = "https://dirty.example.test/EIPs/"

[theme]
repository = "https://example.test/theme.git"
commit = "test-theme-commit"
"#,
        );
        tempdir
    }

    fn assert_dirty_manifest_is_rejected(command: &[&str]) {
        let tempdir = dirty_active_repo();
        let root = tempdir.path().to_string_lossy().to_string();
        let mut arguments = vec!["build-eips", "-C", root.as_str()];
        arguments.extend_from_slice(command);
        let args = Args::try_parse_from(arguments).unwrap();
        let error = resolve_execution(&args).unwrap_err();

        assert!(error
            .to_string()
            .contains("clean active-source mode requires a clean active checkout"));
        assert!(format!("{error:?}").contains("Build.toml"));
    }

    #[test]
    fn dirty_build_toml_is_rejected_for_clean_mode() {
        assert_dirty_manifest_is_rejected(&["build", "--clean"]);
    }

    #[test]
    fn dirty_build_toml_is_rejected_for_changed() {
        assert_dirty_manifest_is_rejected(&["changed"]);
    }

    #[test]
    fn dirty_build_toml_is_rejected_for_clean_remote_siblings() {
        assert_dirty_manifest_is_rejected(&["--remote-siblings", "build", "--clean"]);
    }
}
