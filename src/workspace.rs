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
    config,
    context::{resolve_input_path, root},
    git,
    identity::ActiveRepoIdentity,
};

const WORKSPACE_THEME_URL: &str = "https://github.com/eips-wg/theme.git";
const PROPOSAL_TEMPLATE_URL: &str = "https://github.com/eips-wg/template.git";
const WORKSPACE_DOC_FILE: &str = "WORKSPACE.md";

struct WorkspaceInitRepositories<'a> {
    theme: &'a Url,
    template: &'a Url,
}

pub(crate) fn init_workspace(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
) -> Result<(), Whatever> {
    let theme_repository = Url::parse(WORKSPACE_THEME_URL)
        .whatever_context("invalid workspace theme repository URL")?;
    let template_repository = Url::parse(PROPOSAL_TEMPLATE_URL)
        .whatever_context("invalid proposal template repository URL")?;
    let repositories = WorkspaceInitRepositories {
        theme: &theme_repository,
        template: &template_repository,
    };

    init_workspace_with_repositories(args, workspace_root, include_template, &repositories)
}

fn init_workspace_with_repositories(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
    repositories: &WorkspaceInitRepositories<'_>,
) -> Result<(), Whatever> {
    let root_path = root(args)?;
    let active_repo = ActiveRepoIdentity::load(&root_path)?;
    let workspace_root = resolve_input_path(&workspace_root)?;
    std::fs::create_dir_all(&workspace_root)
        .whatever_context("unable to create workspace root directory")?;
    let workspace_root = workspace_root
        .canonicalize()
        .whatever_context("unable to canonicalize workspace root directory")?;

    // Workspace init is a local-dev bootstrap path, so it intentionally uses staging URLs.
    let repository_use = active_repo.repository_use(true)?;

    let expected_root = workspace_root.join(&repository_use.title);
    if root_path != expected_root {
        snafu::whatever!(
            "init expects the active repository at `{}`, found `{}`",
            expected_root.to_string_lossy(),
            root_path.to_string_lossy(),
        );
    }

    for (sibling_id, sibling_url) in repository_use.other_repos {
        git::clone_missing_repo(sibling_url.as_str(), &workspace_root.join(&sibling_id))
            .with_whatever_context(|_| {
                format!("unable to clone workspace sibling repo `{sibling_id}`")
            })?;
    }

    git::clone_missing_repo(
        repositories.theme.as_str(),
        &workspace_root.join(config::DEFAULT_THEME_DIR),
    )
    .whatever_context("unable to clone workspace theme repo")?;

    if include_template {
        git::clone_missing_repo(
            repositories.template.as_str(),
            &workspace_root.join("template"),
        )
        .whatever_context("unable to clone workspace template repo")?;
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::Parser;
    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;
    use url::Url;

    use crate::{
        cli::{Args, Operation},
        config::{self, LoadedWorkspaceConfig},
    };

    use super::{
        init_workspace_with_repositories, workspace_doc_text, WorkspaceInitRepositories,
        WORKSPACE_DOC_FILE, WORKSPACE_THEME_URL,
    };

    fn parse_args(arguments: &[&str]) -> Args {
        Args::try_parse_from(arguments).unwrap()
    }

    fn file_url(path: &Path) -> Url {
        Url::from_directory_path(path).unwrap()
    }

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

    fn repo_manifest_text(repo_id: &str, repository: &Url, siblings: &[(&str, Url)]) -> String {
        let mut manifest = format!(
            r#"
repo_id = "{repo_id}"

[production]
repository = "{repository}"
base_url = "https://example.test/{repo_id}/"

[staging]
repository = "{repository}"
base_url = "https://staging.example.test/{repo_id}/"
"#
        );

        for (sibling_id, sibling_repository) in siblings {
            manifest.push_str(&format!(
                r#"
[siblings.{sibling_id}.production]
repository = "{sibling_repository}"
base_url = "https://example.test/{sibling_id}/"

[siblings.{sibling_id}.staging]
repository = "{sibling_repository}"
base_url = "https://staging.example.test/{sibling_id}/"
"#
            ));
        }

        manifest
    }

    fn write_manifest_repo(
        path: &Path,
        repo_id: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
    ) -> Repository {
        let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
        write_file(
            path,
            config::REPO_MANIFEST_FILE,
            &repo_manifest_text(repo_id, upstream, siblings),
        );
        commit_all(&repo, "add repo manifest");
        repo
    }

    fn init_workspace_source_repo(remotes_root: &Path, name: &str) -> Url {
        let path = remotes_root.join(name);
        init_repo(&path, &[("README.md", "init test repo\n")]);
        file_url(&path)
    }

    fn workspace_init_test_repository_urls(remotes_root: &Path) -> (Url, Url) {
        (
            init_workspace_source_repo(remotes_root, "theme"),
            init_workspace_source_repo(remotes_root, "template"),
        )
    }

    #[test]
    fn init_command_parses_with_optional_template_flag() {
        let plain = parse_args(&["build-eips", "init", "/tmp/workspace"]);
        let template = parse_args(&["build-eips", "init", "/tmp/workspace", "--template"]);

        assert!(matches!(
            plain.operation,
            Operation::Init {
                template: false,
                ..
            }
        ));
        assert!(matches!(
            template.operation,
            Operation::Init { template: true, .. }
        ));
    }

    #[test]
    fn workspace_theme_url_is_bootstrap_metadata() {
        assert_eq!(
            Url::parse(WORKSPACE_THEME_URL).unwrap().as_str(),
            "https://github.com/eips-wg/theme.git"
        );
    }

    #[test]
    fn workspace_doc_text_mentions_base_workspace_content() {
        let text = workspace_doc_text();

        for expected in [
            ".build-eips.toml",
            ".local-build",
            "build-eips init",
            "build-eips build",
            "build-eips check",
            "build-eips serve",
        ] {
            assert!(
                text.contains(expected),
                "workspace document text should contain `{expected}`"
            );
        }

        assert!(text.ends_with('\n'));
    }

    #[test]
    fn workspace_init_clones_required_repos_and_writes_config_and_doc() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, template_url) = workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitRepositories {
            theme: &theme_url,
            template: &template_url,
        };

        let sibling_path = remotes_root.join("ERCs");
        let sibling_url = file_url(&sibling_path);
        write_manifest_repo(&sibling_path, "ERCs", &sibling_url, &[]);

        let active_path = workspace_root.join("EIPs");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "EIPs", &active_url, &[("ERCs", sibling_url)]);

        let init_args = parse_args(&[
            "build-eips",
            "-C",
            active_path.to_str().unwrap(),
            "init",
            workspace_root.to_str().unwrap(),
            "--template",
        ]);

        init_workspace_with_repositories(&init_args, workspace_root.clone(), true, &repositories)
            .unwrap();

        assert!(Repository::open(workspace_root.join(config::DEFAULT_THEME_DIR)).is_ok());
        assert!(Repository::open(workspace_root.join("ERCs")).is_ok());
        assert!(Repository::open(workspace_root.join("template")).is_ok());
        assert!(workspace_root
            .join(config::DEFAULT_BUILD_ROOT_BASE)
            .is_dir());
        assert!(
            LoadedWorkspaceConfig::from_path(&workspace_root.join(config::LOCAL_CONFIG_FILE))
                .is_ok()
        );
        assert_eq!(
            std::fs::read_to_string(workspace_root.join(WORKSPACE_DOC_FILE)).unwrap(),
            workspace_doc_text()
        );
    }

    #[test]
    fn workspace_init_leaves_existing_config_unchanged() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, template_url) = workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitRepositories {
            theme: &theme_url,
            template: &template_url,
        };
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);
        let existing_config = "[server]\nhost = \"127.0.0.1\"\nport = 1111\n";
        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, existing_config);
        let init_args = parse_args(&[
            "build-eips",
            "-C",
            active_path.to_str().unwrap(),
            "init",
            workspace_root.to_str().unwrap(),
        ]);

        init_workspace_with_repositories(&init_args, workspace_root.clone(), false, &repositories)
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(workspace_root.join(config::LOCAL_CONFIG_FILE)).unwrap(),
            existing_config
        );
    }
}
