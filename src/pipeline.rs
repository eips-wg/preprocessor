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
    changed,
    execution::ResolvedExecution,
    git,
    layout::{mounted_theme_path, output_path, CONTENT_DIR, REPO_DIR},
    lint, markdown, zola,
};

fn prepare_theme_for_zola(theme_path: PathBuf, repo_path: &Path) -> Result<PathBuf, Whatever> {
    let mounted_theme_dir = mounted_theme_path(repo_path);
    git::materialize_working_tree(&theme_path, &mounted_theme_dir)
        .whatever_context("unable to materialize workspace-local theme")?;

    Ok(mounted_theme_dir)
}

#[derive(Debug)]
pub(crate) struct Prepared {
    repo_path: PathBuf,
    output_path: PathBuf,
    repository_use: git::RepositoryUse,
    theme_path: PathBuf,
    base_url_override: Option<Url>,
}

impl Prepared {
    pub(crate) fn prepare(
        eipw: lint::CmdArgs,
        resolved: ResolvedExecution,
    ) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme_path,
            source_materialization,
            base_url_override,
        } = resolved;
        let theme_path =
            theme_path.whatever_context("Zola runtime requires a workspace-local theme path")?;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = output_path(&build_path);

        let both = git::Fresh::new(
            &root_path,
            &repo_path,
            repository_use.clone(),
            source_materialization,
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
            .filter(|p| changed::is_proposal_path(p.into()))
            .map(|p| repo_path.join(p))
            .collect();

        both.merge()
            .whatever_context("unable to merge ERC/EIP repositories")?;

        lint::eipw(&theme_path, &root_path, &repo_path, changed_files, eipw)
            .whatever_context("linting failed")?;

        markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;
        let theme_path = prepare_theme_for_zola(theme_path, &repo_path)?;

        Ok(Prepared {
            repository_use,
            theme_path,
            repo_path,
            output_path,
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
        zola::serve(&self.theme_path, &self.repo_path, &self.output_path)
            .whatever_context("zola serve failed")?;
        Ok(())
    }

    pub(crate) fn check(self) -> Result<(), Whatever> {
        zola::check(&self.theme_path, &self.repo_path).whatever_context("zola check failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;

    use crate::layout::{mounted_theme_path, theme_config_path};

    use super::prepare_theme_for_zola;

    fn write_file(root: &Path, relative: impl AsRef<Path>, contents: &str) {
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
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| repo.find_commit(oid).unwrap())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .unwrap();
    }

    fn init_repo(path: &Path, files: &[(&str, &str)]) -> Repository {
        std::fs::create_dir_all(path).unwrap();
        let repo = Repository::init(path).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        for (relative, contents) in files {
            write_file(path, relative, contents);
        }
        commit_all(&repo, "initial");
        repo
    }

    #[test]
    fn workspace_local_theme_is_materialized_as_mounted_theme_for_zola() {
        let temp = TempDir::new().unwrap();
        let theme_root = temp.path().join("workspace/theme");
        init_repo(
            &theme_root,
            &[
                ("config/zola.toml", "title = 'theme'\n"),
                ("templates/index.html", "local theme\n"),
            ],
        );
        let repo_path = temp.path().join("workspace/.local-build/Core/repo");

        let theme_path = prepare_theme_for_zola(theme_root, &repo_path).unwrap();

        let mounted_theme_dir = mounted_theme_path(&repo_path);
        assert_eq!(theme_path, mounted_theme_dir);
        assert_eq!(
            theme_config_path(&mounted_theme_dir),
            repo_path.join("themes/eips-theme/config/zola.toml")
        );
        assert_eq!(
            std::fs::read_to_string(mounted_theme_dir.join("templates/index.html")).unwrap(),
            "local theme\n"
        );
    }
}
