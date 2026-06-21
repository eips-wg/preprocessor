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
            if !find_root::is_root(path).whatever_context("invalid root directory")? {
                snafu::whatever!("invalid root directory");
            }
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;
    use tempfile::TempDir;

    use crate::{cli::Args, execution::resolve_execution, find_root};

    use super::root;

    fn explicit_root_args(path: &Path) -> Args {
        Args::try_parse_from(["build-eips", "-C", path.to_str().unwrap(), "changed"]).unwrap()
    }

    #[test]
    fn explicit_plain_directory_is_rejected_before_execution_manifest_loading() {
        let directory = TempDir::new().unwrap();
        let args = explicit_root_args(directory.path());

        let error = resolve_execution(&args).unwrap_err().to_string();

        assert_eq!(error, "invalid root directory");
        assert!(!error.contains("unable to load active repository Build.toml"));
        assert!(!error.contains("Build.toml"));
    }

    #[test]
    fn explicit_valid_root_is_accepted() {
        let directory = TempDir::new().unwrap();
        std::fs::create_dir_all(directory.path().join("content")).unwrap();
        std::fs::write(directory.path().join("Build.toml"), "").unwrap();
        let args = explicit_root_args(directory.path());

        assert_eq!(
            root(&args).unwrap(),
            directory.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn implicit_root_keeps_auto_discovery_behavior() {
        let args = Args::try_parse_from(["build-eips", "changed"]).unwrap();

        match (root(&args), find_root::find_root()) {
            (Ok(root), Ok(discovered)) => assert_eq!(root, discovered),
            (Err(error), Err(_)) => {
                assert!(error.to_string().contains("cannot find repository root"));
                assert!(!error.to_string().contains("invalid root directory"));
            }
            (root, discovered) => panic!(
                "context root result {root:?} did not match automatic discovery {discovered:?}"
            ),
        }
    }
}
