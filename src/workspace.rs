/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Local workspace setup.

use std::{
    fs::OpenOptions,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use log::info;
use snafu::{ResultExt, Whatever};
use url::Url;

use crate::{
    cli::Args,
    config::{self, ActiveRepo},
    context::{resolve_input_path, root},
    git,
};

const PROPOSAL_TEMPLATE_URL: &str = "https://github.com/eips-wg/template.git";
const WORKSPACE_DOC_FILE: &str = "WORKSPACE.md";

struct WorkspaceInitToolingRepositories<'a> {
    template: &'a Url,
}

pub(crate) fn init_workspace(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
) -> Result<(), Whatever> {
    let template_repository = Url::parse(PROPOSAL_TEMPLATE_URL)
        .whatever_context("invalid proposal template repository URL")?;
    let repositories = WorkspaceInitToolingRepositories {
        template: &template_repository,
    };

    init_workspace_with_repositories(
        args,
        workspace_root,
        include_template,
        &repositories,
    )
}

fn init_workspace_with_repositories(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
    repositories: &WorkspaceInitToolingRepositories<'_>,
) -> Result<(), Whatever> {
    let root_path = root(args)
        .whatever_context("workspace init requires an active Build.toml repository root")?;
    let active_repo = ActiveRepo::load(&root_path)
        .whatever_context("unable to load active repository Build.toml")?;
    let workspace_root = resolve_input_path(&workspace_root)?;
    std::fs::create_dir_all(&workspace_root)
        .whatever_context("unable to create workspace root directory")?;
    let workspace_root = workspace_root
        .canonicalize()
        .whatever_context("unable to canonicalize workspace root directory")?;

    let repository_use = active_repo.repository_use;
    let theme = active_repo.theme;

    for (sibling_id, sibling_url) in repository_use.other_repos {
        let sibling_path = workspace_root.join(&sibling_id);
        git::clone_missing_repo(sibling_url.as_str(), &sibling_path).with_whatever_context(|_| {
            format!(
                "unable to prepare workspace sibling repo `{sibling_id}` at `{}`; destination must be missing or a usable git repository",
                sibling_path.to_string_lossy()
            )
        })?;
    }

    let theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
    if git::clone_missing_repo(theme.repository.as_str(), &theme_path)
        .with_whatever_context(|_| {
            format!(
                "unable to prepare workspace theme repo at `{}`; destination must be missing or a usable git repository",
                theme_path.to_string_lossy()
            )
        })?
        == git::CloneOutcome::Fresh
    {
        git::checkout_fresh_clone_at_commit(&theme_path, theme.repository.as_str(), &theme.commit)
            .with_whatever_context(|_| {
                format!(
                    "unable to fetch or check out active Build.toml theme commit `{}`",
                    theme.commit
                )
            })?;
    }

    if include_template {
        let template_path = workspace_root.join("template");
        git::clone_missing_repo(repositories.template.as_str(), &template_path)
            .with_whatever_context(|_| {
                format!(
                    "unable to prepare workspace template repo at `{}`; destination must be missing or a usable git repository",
                    template_path.to_string_lossy()
                )
            })?;
    }

    std::fs::create_dir_all(workspace_root.join(config::DEFAULT_BUILD_ROOT_BASE))
        .whatever_context("unable to create local build root")?;

    let config_path = workspace_root.join(config::LOCAL_CONFIG_FILE);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&config_path)
    {
        Ok(mut config_file) => {
            config_file
                .write_all(config::default_workspace_config_text().as_bytes())
                .whatever_context("unable to write workspace config")?;
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            info!(
                "leaving existing workspace config `{}` in place",
                config_path.to_string_lossy()
            );
        }
        Err(error) => {
            return Err(error).whatever_context("unable to write workspace config");
        }
    }

    write_workspace_doc(&workspace_root)?;

    Ok(())
}

fn workspace_doc_text() -> &'static str {
    include_str!("workspace_doc.md")
}

fn write_workspace_doc(workspace_root: &Path) -> Result<(), Whatever> {
    let doc_path = workspace_root.join(WORKSPACE_DOC_FILE);
    std::fs::write(&doc_path, workspace_doc_text()).with_whatever_context(|_| {
        format!(
            "unable to write workspace document `{}`",
            doc_path.to_string_lossy()
        )
    })?;

    Ok(())
}
