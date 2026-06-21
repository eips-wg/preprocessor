/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Prepared Zola runtime pipeline.

use std::path::{Path, PathBuf};

use snafu::{OptionExt, ResultExt, Whatever};
use url::Url;

use crate::{
    config::{RepositoryUse, ServerBinding},
    execution::ResolvedExecution,
    git,
    layout::{mounted_theme_path, output_path, CONTENT_DIR, REPO_DIR},
    markdown,
    proposal::OnlyRenderPlan,
    serve::{serve_sync_config, DirtyServeWatcher, LocalThemeServeSync},
    zola,
};

fn prepare_theme_for_zola(
    theme_path: PathBuf,
    repo_path: &Path,
) -> Result<(PathBuf, LocalThemeServeSync), Whatever> {
    let mounted_theme_dir = mounted_theme_path(repo_path);
    git::materialize_working_tree(&theme_path, &mounted_theme_dir)
        .whatever_context("unable to materialize workspace-local theme")?;
    let theme_index_path = git::index_path(&theme_path)
        .whatever_context("unable to resolve workspace-local theme Git index path")?;

    Ok((
        mounted_theme_dir.clone(),
        LocalThemeServeSync {
            theme_source_root: theme_path,
            mounted_theme_dir,
            theme_index_path,
        },
    ))
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
    local_theme_sync: Option<LocalThemeServeSync>,
    only_plan: Option<OnlyRenderPlan>,
    source_root: PathBuf,
    source_materialization: git::SourceMaterialization,
    server_binding: ServerBinding,
    base_url_override: Option<Url>,
}

impl Prepared {
    pub(crate) fn prepare(resolved: ResolvedExecution) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme_path,
            only,
            source_materialization,
            server_binding,
            base_url_override,
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

        let only_plan = only
            .map(|selected_numbers| OnlyRenderPlan::build(&content_path, selected_numbers))
            .transpose()
            .whatever_context("unable to build targeted render plan")?;
        markdown::preprocess(&content_path, only_plan.as_ref())
            .whatever_context("unable to preprocess markdown")?;
        if let Some(only_plan) = &only_plan {
            only_plan
                .prune_content(&content_path)
                .whatever_context("unable to prune unselected proposals")?;
        }
        let (theme_path, local_theme_sync) = prepare_theme_for_zola(theme_path, &repo_path)?;

        Ok(Prepared {
            repository_use,
            theme_path,
            local_theme_sync: Some(local_theme_sync),
            repo_path,
            output_path,
            only_plan,
            source_root: root_path,
            source_materialization,
            server_binding,
            base_url_override,
        })
    }

    pub(crate) fn build(self) -> Result<(), Whatever> {
        let base_url = self
            .base_url_override
            .as_ref()
            .unwrap_or(&self.repository_use.location.base_url);
        zola::build(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        Ok(())
    }

    pub(crate) fn serve(self) -> Result<(), Whatever> {
        let sync_config = serve_sync_config(
            self.source_materialization,
            &self.source_root,
            &self.repo_path,
            self.only_plan.clone(),
            self.local_theme_sync.clone(),
        );
        let dirty_watcher = if sync_config.has_targets() {
            Some(
                DirtyServeWatcher::start(sync_config)
                    .whatever_context("unable to start dirty serve watcher")?,
            )
        } else {
            None
        };

        let result = zola::serve(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            &self.server_binding,
            self.base_url_override.as_ref(),
        )
        .whatever_context("zola serve failed");

        if let Some(dirty_watcher) = dirty_watcher {
            dirty_watcher.stop();
        }

        result
    }

    pub(crate) fn check(self) -> Result<(), Whatever> {
        zola::check(&self.theme_path, &self.repo_path).whatever_context("zola check failed")?;
        Ok(())
    }
}

