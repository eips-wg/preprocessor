/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Command context and path resolution helpers.

use std::path::{Path, PathBuf};

use snafu::{ResultExt, Whatever};

use crate::{cli::Args, config, find_root};

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceCommandContext {
    pub(crate) search_from: PathBuf,
    pub(crate) config_path: Option<PathBuf>,
}

pub(crate) fn resolve_input_path(path: &Path) -> Result<PathBuf, Whatever> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().whatever_context("unable to get current directory")?;
        Ok(cwd.join(path))
    }
}

pub(crate) fn root(args: &Args) -> Result<PathBuf, Whatever> {
    let dir = match &args.root {
        None => find_root::find_root().whatever_context("cannot find repository root")?,
        Some(path) => {
            find_root::is_root(path).whatever_context("invalid root directory")?;
            path.canonicalize()
                .whatever_context("unable to canonicalize root directory")?
        }
    };
    find_root::is_root(&dir).whatever_context("invalid root directory")?;
    Ok(dir)
}

fn workspace_search_start(args: &Args) -> Result<PathBuf, Whatever> {
    match &args.root {
        Some(path) => {
            let path = resolve_input_path(path)?;
            path.canonicalize()
                .whatever_context("unable to canonicalize workspace search path")
        }
        None => std::env::current_dir().whatever_context("unable to get current directory"),
    }
}

pub(crate) fn load_workspace_command_context(
    args: &Args,
) -> Result<WorkspaceCommandContext, Whatever> {
    let search_from = workspace_search_start(args)?;
    let config_path = config::discover_path(&search_from);

    Ok(WorkspaceCommandContext {
        search_from,
        config_path,
    })
}
