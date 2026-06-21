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
    cli::{Args, Operation, ServerCliArgs},
    config::{self, ActiveRepo, LoadedWorkspaceConfig, RepositoryUse, ServerBinding},
    context::{resolve_input_path, root},
    git,
    layout::BUILD_DIR,
};

#[derive(Debug, Clone)]
pub(crate) struct ResolvedExecution {
    pub(crate) root_path: PathBuf,
    pub(crate) build_path: PathBuf,
    pub(crate) repository_use: RepositoryUse,
    pub(crate) theme_path: Option<PathBuf>,
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
        source_materialization,
        server_binding: resolve_server_binding(
            workspace_config.as_ref(),
            &args.operation.server_cli_args(),
        ),
        base_url_override,
    })
}

