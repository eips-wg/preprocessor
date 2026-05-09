/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Execution source and path resolution.

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use log::{debug, info};
use snafu::{OptionExt, ResultExt, Whatever};
use url::Url;

use crate::{
    cli::{Args, Operation},
    config::{self, LoadedWorkspaceConfig},
    context::{resolve_input_path, root},
    git,
    identity::ActiveRepoIdentity,
    layout::BUILD_DIR,
};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedExecution {
    pub(crate) root_path: PathBuf,
    pub(crate) build_path: PathBuf,
    pub(crate) repository_use: git::RepositoryUse,
    pub(crate) theme_path: Option<PathBuf>,
    pub(crate) source_materialization: git::SourceMaterialization,
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
    pub(crate) staging: bool,
    pub(crate) allow_dirty: bool,
    pub(crate) sibling: SelectedSource,
}

fn has_execution_override_flags(args: &Args) -> bool {
    args.staging || args.production || args.remote_siblings || args.build_root.is_some()
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

fn resolve_bool_override(
    enabled: bool,
    disabled: bool,
    enabled_flag: &str,
    disabled_flag: &str,
) -> Result<Option<bool>, Whatever> {
    match (enabled, disabled) {
        (true, true) => {
            snafu::whatever!("cannot pass both `{enabled_flag}` and `{disabled_flag}`")
        }
        (true, false) => Ok(Some(true)),
        (false, true) => Ok(Some(false)),
        (false, false) => Ok(None),
    }
}

fn remote_source_override(force_remote: bool) -> Option<SelectedSource> {
    force_remote.then_some(SelectedSource::Remote)
}

fn format_sibling_ids(sibling_ids: &[String]) -> String {
    sibling_ids.join(", ")
}

fn resolve_environment_override(args: &Args) -> Result<Option<bool>, Whatever> {
    resolve_bool_override(args.staging, args.production, "--staging", "--production")
}

fn explicit_environment_or_parity(args: &Args) -> Result<Option<bool>, Whatever> {
    if let Some(staging) = resolve_environment_override(args)? {
        return Ok(Some(staging));
    }

    if matches!(args.operation, Operation::Parity { .. }) {
        return Ok(Some(true));
    }

    Ok(None)
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
    let explicit_environment = explicit_environment_or_parity(args)?;
    let sibling_override = remote_source_override(args.remote_siblings);
    let clean = args.operation.clean_cli_args().clean;

    let (staging, allow_dirty, default_sibling) = if let Some(staging) = explicit_environment {
        (staging, false, SelectedSource::Remote)
    } else if args.operation.is_plain_site_command() {
        (true, !clean, SelectedSource::WorkspaceLocal)
    } else {
        (false, false, SelectedSource::Remote)
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
                "the selected command requires workspace-local sibling sources, but no `{}` was found to provide them.\nResolve this by doing one of the following:\n1. run `build-eips init <workspace-root>` so the workspace config supplies the local sources\n2. pass `--remote-siblings` for remote sibling source overrides\n3. use `parity <command>`, `--staging <command>`, or `--production <command>` for remote clean environment behavior",
                config::LOCAL_CONFIG_FILE
            );
        }
        _ => {}
    }

    let sibling = sibling_override.unwrap_or(default_sibling);

    Ok(ExecutionSettings {
        build_root,
        staging,
        allow_dirty,
        sibling,
    })
}

fn local_repo_url(path: &Path) -> Result<Url, Whatever> {
    Url::from_directory_path(path)
        .ok()
        .whatever_context("unable to convert local sibling repository path into a file URL")
}

fn apply_sibling_sources(
    repository_use: &mut git::RepositoryUse,
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
    repository_use: &git::RepositoryUse,
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
            | Operation::Parity { .. }
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

fn resolve_base_url_override(
    args: &Args,
    workspace_config: Option<&LoadedWorkspaceConfig>,
) -> Result<Option<Url>, Whatever> {
    if let Some(base_url) = args.operation.base_url_cli_args().base_url {
        return Ok(Some(base_url));
    }

    if explicit_environment_or_parity(args)?.is_some() {
        return Ok(None);
    }

    Ok(workspace_config.and_then(|config| config.site_settings().base_url.clone()))
}

pub(crate) fn resolve_execution(args: &Args) -> Result<ResolvedExecution, Whatever> {
    let root_path = root(args)?;
    let active_repo = ActiveRepoIdentity::load(&root_path)?;
    let sibling_ids = active_repo.sibling_ids();
    let workspace_config = LoadedWorkspaceConfig::discover(&root_path)
        .whatever_context("unable to load workspace config")?;

    if let Some(workspace_config) = workspace_config.as_ref() {
        debug!(
            "using workspace config `{}`",
            workspace_config.config_path().to_string_lossy()
        );
    }

    let settings = resolve_execution_settings(args, &sibling_ids, workspace_config.as_ref())?;
    let theme_path = resolve_theme_path(workspace_config.as_ref(), &args.operation)?;
    let mut repository_use = active_repo.repository_use(settings.staging)?;
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
        source_materialization,
        base_url_override,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use tempfile::TempDir;

    use crate::{
        cli::Args,
        config::{self, LoadedWorkspaceConfig},
    };

    use super::{
        explicit_environment_or_parity, resolve_base_url_override, resolve_execution_settings,
        validate_non_execution_command_flags, ExecutionSettings, SelectedSource,
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

    fn assert_theme_only_missing_workspace_error(arguments: &[&str]) {
        let args = parse_args(arguments);
        let error = super::resolve_theme_path(None, &args.operation).unwrap_err();
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

    #[test]
    fn explicit_env_or_parity_provenance_is_classified_separately_from_local_defaults() {
        let cases: &[(&[&str], Option<bool>)] = &[
            (&["build-eips", "--staging", "build"], Some(true)),
            (&["build-eips", "--production", "build"], Some(false)),
            (&["build-eips", "parity", "build"], Some(true)),
            (&["build-eips", "build"], None),
            (&["build-eips", "serve"], None),
            (&["build-eips", "check"], None),
        ];

        for (arguments, expected) in cases {
            let args = parse_args(arguments);
            assert_eq!(explicit_environment_or_parity(&args).unwrap(), *expected);
        }
    }

    #[test]
    fn base_url_override_resolution_uses_cli_config_then_provenance() {
        let workspace_config = load_workspace_config(
            r#"
[site]
base_url = "http://localhost:4000"
"#,
        );
        let none = parse_args(&["build-eips", "build"]);
        assert!(resolve_base_url_override(&none, None).unwrap().is_none());

        for arguments in [&["build-eips", "build"][..], &["build-eips", "serve"][..]] {
            let args = parse_args(arguments);
            assert_eq!(
                resolve_base_url_override(&args, Some(&workspace_config))
                    .unwrap()
                    .unwrap()
                    .as_str(),
                "http://localhost:4000/"
            );
        }

        let cli = parse_args(&["build-eips", "build", "--base-url", "http://localhost:5000"]);
        assert_eq!(
            resolve_base_url_override(&cli, Some(&workspace_config))
                .unwrap()
                .unwrap()
                .as_str(),
            "http://localhost:5000/"
        );

        for arguments in [
            &["build-eips", "--staging", "build"][..],
            &["build-eips", "--production", "build"][..],
            &["build-eips", "parity", "build"][..],
            &["build-eips", "parity", "serve"][..],
        ] {
            let args = parse_args(arguments);
            assert!(resolve_base_url_override(&args, Some(&workspace_config))
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn local_site_base_url_override_does_not_change_execution_settings() {
        let workspace_config = load_workspace_config(
            r#"
[site]
base_url = "http://localhost:4000"
"#,
        );
        let args = parse_args(&["build-eips", "build"]);
        let settings = resolve_execution_settings(&args, &[], Some(&workspace_config)).unwrap();

        assert_eq!(
            resolve_base_url_override(&args, Some(&workspace_config))
                .unwrap()
                .unwrap()
                .as_str(),
            "http://localhost:4000/"
        );
        assert_eq!(
            settings,
            ExecutionSettings {
                build_root: None,
                staging: true,
                allow_dirty: true,
                sibling: SelectedSource::WorkspaceLocal,
            }
        );
    }

    #[test]
    fn plain_site_commands_are_local_first_dirty_staging() {
        let workspace_config = load_workspace_config("");
        let expected = ExecutionSettings {
            build_root: None,
            staging: true,
            allow_dirty: true,
            sibling: SelectedSource::WorkspaceLocal,
        };

        for arguments in [
            &["build-eips", "build"][..],
            &["build-eips", "serve"][..],
            &["build-eips", "check"][..],
        ] {
            assert_settings(
                arguments,
                &["ERCs"],
                Some(&workspace_config),
                expected.clone(),
            );
        }
    }

    #[test]
    fn clean_plain_site_commands_keep_local_sources_but_disable_dirty_materialization() {
        let workspace_config = load_workspace_config("");
        let expected = ExecutionSettings {
            build_root: None,
            staging: true,
            allow_dirty: false,
            sibling: SelectedSource::WorkspaceLocal,
        };

        for arguments in [
            &["build-eips", "build", "--clean"][..],
            &["build-eips", "serve", "--clean"][..],
            &["build-eips", "check", "--clean"][..],
        ] {
            assert_settings(
                arguments,
                &["ERCs"],
                Some(&workspace_config),
                expected.clone(),
            );
        }
    }

    #[test]
    fn explicit_environment_site_commands_are_remote_clean_for_proposals() {
        for (arguments, expected_staging) in [
            (&["build-eips", "--staging", "build"][..], true),
            (&["build-eips", "--staging", "serve"][..], true),
            (&["build-eips", "--staging", "check"][..], true),
            (&["build-eips", "--production", "build"][..], false),
            (&["build-eips", "--production", "serve"][..], false),
            (&["build-eips", "--production", "check"][..], false),
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
    fn clean_environment_commands_are_accepted_as_redundant_remote_clean() {
        for (arguments, expected_staging) in [
            (&["build-eips", "--staging", "build", "--clean"][..], true),
            (&["build-eips", "--staging", "serve", "--clean"][..], true),
            (&["build-eips", "--staging", "check", "--clean"][..], true),
            (
                &["build-eips", "--production", "build", "--clean"][..],
                false,
            ),
            (
                &["build-eips", "--production", "serve", "--clean"][..],
                false,
            ),
            (
                &["build-eips", "--production", "check", "--clean"][..],
                false,
            ),
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
    fn remote_source_overrides_compose_with_local_dirty_and_clean_modes() {
        let workspace_config = load_workspace_config("");
        let cases = [
            (
                &["build-eips", "--remote-siblings", "build"][..],
                true,
                SelectedSource::Remote,
            ),
            (
                &["build-eips", "--remote-siblings", "build", "--clean"][..],
                false,
                SelectedSource::Remote,
            ),
        ];

        for (arguments, allow_dirty, sibling) in cases {
            assert_settings(
                arguments,
                &["ERCs"],
                Some(&workspace_config),
                ExecutionSettings {
                    build_root: None,
                    staging: true,
                    allow_dirty,
                    sibling,
                },
            );
        }
    }

    #[test]
    fn non_site_commands_do_not_require_workspace_local_sources() {
        for arguments in [&["build-eips", "changed"][..], &["build-eips", "clean"][..]] {
            assert_settings(
                arguments,
                &["ERCs"],
                None,
                ExecutionSettings {
                    build_root: None,
                    staging: false,
                    allow_dirty: false,
                    sibling: SelectedSource::Remote,
                },
            );
        }
    }

    #[test]
    fn changed_environment_flags_use_remote_clean_metadata_without_workspace_config() {
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
    fn parity_site_commands_remain_remote_clean_staging() {
        for arguments in [
            &["build-eips", "parity", "build"][..],
            &["build-eips", "parity", "serve"][..],
            &["build-eips", "parity", "check"][..],
        ] {
            assert_settings(
                arguments,
                &["ERCs"],
                None,
                ExecutionSettings {
                    build_root: None,
                    staging: true,
                    allow_dirty: false,
                    sibling: SelectedSource::Remote,
                },
            );
        }
    }

    #[test]
    fn boolean_override_conflicts_are_hard_errors() {
        let args = parse_args(&["build-eips", "--staging", "--production", "build"]);
        let error = resolve_execution_settings(&args, &[], None).unwrap_err();

        assert!(error
            .to_string()
            .contains("cannot pass both `--staging` and `--production`"));
    }

    #[test]
    fn non_execution_commands_reject_execution_override_flags() {
        for arguments in [
            &["build-eips", "--staging", "init", "/tmp/workspace"][..],
            &["build-eips", "--production", "init", "/tmp/workspace"][..],
            &["build-eips", "--remote-siblings", "init", "/tmp/workspace"][..],
            &[
                "build-eips",
                "--build-root",
                "/tmp/build",
                "init",
                "/tmp/workspace",
            ][..],
            &["build-eips", "--staging", "doctor"][..],
            &["build-eips", "--production", "doctor"][..],
            &["build-eips", "--remote-siblings", "doctor"][..],
            &["build-eips", "--build-root", "/tmp/build", "doctor"][..],
            &["build-eips", "--remote-siblings", "print", "schema-version"][..],
        ] {
            let args = parse_args(arguments);
            let error = validate_non_execution_command_flags(&args).unwrap_err();
            let message = error.to_string();

            assert!(
                message.contains("execution override flags cannot be used"),
                "unexpected error for {arguments:?}: {message}"
            );
        }
    }

    #[test]
    fn zola_runtime_commands_require_workspace_local_theme() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join(config::LOCAL_CONFIG_FILE);
        std::fs::write(&config_path, "").unwrap();
        std::fs::create_dir(workspace.path().join(config::DEFAULT_THEME_DIR)).unwrap();
        let workspace_config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        for arguments in [
            &["build-eips", "build"][..],
            &["build-eips", "check"][..],
            &["build-eips", "serve"][..],
            &["build-eips", "--staging", "build"][..],
            &["build-eips", "--production", "check"][..],
            &["build-eips", "parity", "build"][..],
        ] {
            let args = parse_args(arguments);
            let theme_path = super::resolve_theme_path(Some(&workspace_config), &args.operation)
                .unwrap()
                .unwrap();

            assert_eq!(theme_path, workspace.path().join(config::DEFAULT_THEME_DIR));
        }
    }

    #[test]
    fn non_theme_commands_do_not_require_workspace_local_theme() {
        for arguments in [
            &["build-eips", "changed"][..],
            &["build-eips", "clean"][..],
            &["build-eips", "doctor"][..],
            &["build-eips", "print", "schema-version"][..],
        ] {
            let args = parse_args(arguments);

            assert!(super::resolve_theme_path(None, &args.operation)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn local_first_theme_commands_without_workspace_config_report_combined_setup_error() {
        for arguments in [
            &["build-eips", "build"][..],
            &["build-eips", "serve"][..],
            &["build-eips", "check"][..],
        ] {
            assert_combined_missing_workspace_error(arguments);
        }
    }

    #[test]
    fn local_first_with_remote_sibling_override_is_not_parity() {
        let local_args = parse_args(&["build-eips", "--remote-siblings", "build"]);
        let sibling_ids = vec!["ERCs".to_owned()];
        let local_settings = resolve_execution_settings(&local_args, &sibling_ids, None).unwrap();

        let parity_args = parse_args(&["build-eips", "parity", "build"]);
        let parity_settings = resolve_execution_settings(&parity_args, &sibling_ids, None).unwrap();

        assert_eq!(
            local_settings,
            ExecutionSettings {
                build_root: None,
                staging: true,
                allow_dirty: true,
                sibling: SelectedSource::Remote,
            }
        );
        assert_eq!(
            parity_settings,
            ExecutionSettings {
                build_root: None,
                staging: true,
                allow_dirty: false,
                sibling: SelectedSource::Remote,
            }
        );
    }

    #[test]
    fn zero_sibling_remote_override_is_noop() {
        let remote_args = parse_args(&["build-eips", "--remote-siblings", "parity", "build"]);
        let remote_settings = resolve_execution_settings(&remote_args, &[], None).unwrap();

        assert_eq!(remote_settings.sibling, SelectedSource::Remote);
    }

    #[test]
    fn zero_sibling_local_first_without_workspace_config_can_resolve_sibling_policy() {
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
    fn environment_and_parity_zola_commands_without_workspace_config_only_require_theme() {
        for arguments in [
            &["build-eips", "--staging", "build"][..],
            &["build-eips", "--production", "serve"][..],
            &["build-eips", "parity", "check"][..],
        ] {
            let args = parse_args(arguments);
            let sibling_ids = vec!["ERCs".to_owned()];
            let settings = resolve_execution_settings(&args, &sibling_ids, None).unwrap();

            assert_eq!(settings.sibling, SelectedSource::Remote);
            assert_theme_only_missing_workspace_error(arguments);
        }
    }

    #[test]
    fn missing_workspace_theme_path_reports_clear_error() {
        let workspace = TempDir::new().unwrap();
        let config_path = workspace.path().join(config::LOCAL_CONFIG_FILE);
        std::fs::write(&config_path, "").unwrap();
        let workspace_config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();
        let args = parse_args(&["build-eips", "build"]);

        let error =
            super::resolve_theme_path(Some(&workspace_config), &args.operation).unwrap_err();
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
