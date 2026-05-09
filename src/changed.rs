/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Changed-file command execution.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use snafu::{ResultExt, Whatever};

use crate::{cli::ChangedFormat, execution::ResolvedExecution, git, layout::REPO_DIR};

pub(crate) fn is_proposal_path(mut p: PathBuf) -> bool {
    // Only lint `content/00001.md` and `content/00001/index.md` files.

    // content/00000.md  |  content/00000/index.md
    //         ^^^^^^^^  |                ^^^^^^^^
    match p.file_name() {
        Some(n) if n == "index.md" => {
            p.pop();
        }
        Some(_) if p.extension().map(|x| x == "md").unwrap_or(false) => {
            p.set_extension("");
        }
        None | Some(_) => return false,
    }

    // content/00000
    //         ^^^^^
    match p.file_name().and_then(OsStr::to_str) {
        None => return false,
        Some(f) if f.parse::<u64>().is_err() => return false,
        Some(_) => {
            p.pop();
        }
    }

    // content
    // ^^^^^^^
    match p.file_name() {
        Some(f) if f == "content" => {
            p.pop();
        }
        _ => return false,
    }

    p == OsStr::new("")
}

pub(crate) fn run(
    resolved: &ResolvedExecution,
    build_path: &Path,
    all: bool,
    format: &ChangedFormat,
) -> Result<(), Whatever> {
    let repo_path = build_path.join(REPO_DIR);

    let both = git::Fresh::new(
        &resolved.root_path,
        &repo_path,
        resolved.repository_use.clone(),
        resolved.source_materialization,
    )
    .whatever_context("initializing build repo")?
    .clone_src()
    .whatever_context("cloning source repo")?
    .fetch_upstream()
    .whatever_context("fetching upstream repo")?;

    let changed_files: Vec<_> = both
        .changed_files()
        .whatever_context("unable to list changed files")?
        .into_iter()
        .filter(|p| all || is_proposal_path(p.into()))
        .map(|p| repo_path.join(p))
        .collect();

    format.print(&changed_files, &repo_path);
    Ok(())
}
