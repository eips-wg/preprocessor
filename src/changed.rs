/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Changed-file command helpers.

use std::path::Path;

use snafu::{ResultExt, Whatever};

use crate::{
    cli::ChangedFormat, execution::ResolvedExecution, git, layout::REPO_DIR,
    proposal::is_proposal_path,
};

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
        .filter(|p| all || is_proposal_path(p))
        .map(|p| repo_path.join(p))
        .collect();

    format.print(&changed_files, &repo_path);

    Ok(())
}
