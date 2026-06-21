/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Prepared Zola runtime pipeline.

use std::path::{Path, PathBuf};

use snafu::{OptionExt, ResultExt, Whatever};
use crate::{
    config::RepositoryUse,
    execution::ResolvedExecution,
    git,
    layout::{mounted_theme_path, output_path, CONTENT_DIR, REPO_DIR},
    markdown,
    zola,
};

fn prepare_theme_for_zola(
    theme_path: PathBuf,
    repo_path: &Path,
) -> Result<PathBuf, Whatever> {
    let mounted_theme_dir = mounted_theme_path(repo_path);
    git::materialize_working_tree(&theme_path, &mounted_theme_dir)
        .whatever_context("unable to materialize workspace-local theme")?;
    Ok(mounted_theme_dir)
}

fn prepare_runtime_source(
    root_path: &Path,
    repo_path: &Path,
    repository_use: &RepositoryUse,
    source_materialization: git::SourceMaterialization,
) -> Result<(), Whatever> {
    let source = git::Fresh::new(
        root_path,
        repo_path,
        repository_use.clone(),
        source_materialization,
    )
    .whatever_context("initializing build repo")?
    .clone_src()
    .whatever_context("cloning source repo")?;

    source
        .merge()
        .whatever_context("unable to merge ERC/EIP repositories")?;

    Ok(())
}

#[derive(Debug)]
pub(crate) struct Prepared {
    repo_path: PathBuf,
    output_path: PathBuf,
    repository_use: RepositoryUse,
    theme_path: PathBuf,
    source_materialization: git::SourceMaterialization,
}

impl Prepared {
    pub(crate) fn prepare(resolved: ResolvedExecution) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme_path,
            source_materialization,
        } = resolved;
        let theme_path =
            theme_path.whatever_context("Zola runtime requires a workspace-local theme path")?;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = output_path(&build_path);

        prepare_runtime_source(
            &root_path,
            &repo_path,
            &repository_use,
            source_materialization,
        )?;

        markdown::preprocess(&content_path, None)
            .whatever_context("unable to preprocess markdown")?;
        let theme_path = prepare_theme_for_zola(theme_path, &repo_path)?;

        Ok(Prepared {
            repository_use,
            theme_path,
            repo_path,
            output_path,
            source_materialization,
        })
    }

    pub(crate) fn build(self) -> Result<(), Whatever> {
        let base_url = &self.repository_use.location.base_url;
        zola::build(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        Ok(())
    }

    pub(crate) fn check(self) -> Result<(), Whatever> {
        zola::check(&self.theme_path, &self.repo_path).whatever_context("zola check failed")?;
        Ok(())
    }
}

