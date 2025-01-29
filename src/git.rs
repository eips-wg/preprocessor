/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::progress::{Git, ProgressIteratorExt};
use enum_map::{enum_map, Enum, EnumMap};
use git2::{
    build::{CheckoutBuilder, RepoBuilder, TreeUpdateBuilder},
    BranchType, Commit, FetchOptions, FileMode, ObjectType, Repository, StatusOptions, Tree,
    TreeEntry, TreeWalkResult,
};
use log::{debug, info};
use snafu::{ensure, Backtrace, OptionExt, Report, ResultExt, Snafu};
use url::Url;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("cannot convert path into URL (`{}`)", path.to_string_lossy()))]
    PathUrl { path: PathBuf, backtrace: Backtrace },
    #[snafu(display("unable to {what}"))]
    Git {
        what: &'static str,
        source: git2::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("unable to determine if repository is EIPs or ERCs"))]
    Identify { backtrace: Backtrace },
    #[snafu(display("working tree or index has uncommitted modifications"))]
    Dirty { backtrace: Backtrace },
    #[snafu(display("unable to update tree ({msg})"))]
    UpdateTree { msg: String, backtrace: Backtrace },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Enum)]
enum RepositoryUse {
    Eips,
    Ercs,
}

impl fmt::Display for RepositoryUse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let txt = match self {
            Self::Ercs => "ERCs",
            Self::Eips => "EIPs",
        };
        write!(f, "{}", txt)
    }
}

lazy_static::lazy_static! {
    static ref REPO_URLS: EnumMap<RepositoryUse, &'static str> = enum_map! {
        RepositoryUse::Eips => "https://github.com/ethereum/EIPs.git",
        RepositoryUse::Ercs => "https://github.com/ethereum/ERCs.git",
    };
}

impl RepositoryUse {
    const EIP_COMMIT: &str = "0f44e2b94df4e504bb7b912f56ebd712db2ad396";
    const ERC_COMMIT: &str = "8dd085d159cb123f545c272c0d871a5339550e79";

    fn identify(repo: &Repository) -> Result<Self, Error> {
        let eip = repo.revparse_single(Self::EIP_COMMIT);
        let erc = repo.revparse_single(Self::ERC_COMMIT);

        match (eip, erc) {
            (Ok(_), Err(_)) => Ok(Self::Eips),
            (Err(_), Ok(_)) => Ok(Self::Ercs),
            (_, _) => IdentifySnafu.fail(),
        }
    }

    fn other_repos(self) -> Vec<(Self, &'static str)> {
        REPO_URLS.into_iter().filter(|(k, _)| *k != self).collect()
    }
}

pub fn check_dirty(root_path: &Path) -> Result<(), Error> {
    let repo = Repository::open(root_path).context(GitSnafu {
        what: "open root repository",
    })?;
    let mut options = StatusOptions::default();
    options.include_untracked(true);
    let statuses = repo.statuses(Some(&mut options)).context(GitSnafu {
        what: "get root repository status",
    })?;
    let mut statuses = statuses.iter().filter(|x| {
        x.path()
            .map(|x| !x.trim_end_matches('/').ends_with(super::BUILD_DIR))
            .unwrap_or(false)
    });
    if statuses.next().is_some() {
        DirtySnafu.fail()
    } else {
        Ok(())
    }
}

fn branch_to_commit<'a, 'b>(repo: &'a Repository, rev: &'b str) -> Result<Commit<'a>, Error> {
    repo.find_branch(rev, BranchType::Local)
        .context(GitSnafu {
            what: "revparse to annotated",
        })?
        .into_reference()
        .peel_to_commit()
        .context(GitSnafu {
            what: "peel to annotated",
        })
}

fn check_conflict(master_tree: &Tree, path: &Path, entry: &TreeEntry) -> Result<(), Error> {
    let original = match master_tree.get_path(path) {
        Err(_) => return Ok(()),
        Ok(o) => o,
    };

    ensure!(
        original.filemode() == entry.filemode(),
        UpdateTreeSnafu {
            msg: format!("conflicting path `{}` (filemode)", path.to_string_lossy()),
        }
    );

    ensure!(
        original.kind() == entry.kind(),
        UpdateTreeSnafu {
            msg: format!("conflicting path `{}` (kind)", path.to_string_lossy()),
        }
    );

    ensure!(
        original.id() == entry.id(),
        UpdateTreeSnafu {
            msg: format!("conflicting path `{}` (id)", path.to_string_lossy()),
        }
    );

    Ok(())
}

pub fn merge_repositories(root_path: &Path, build_path: &Path) -> Result<(), Error> {
    check_dirty(root_path)?;

    let repo_path = build_path.join(super::COMBINED_DIR);
    let url = Url::from_directory_path(root_path)
        .ok()
        .context(PathUrlSnafu { path: root_path })?;

    if let Err(e) = std::fs::remove_dir_all(&repo_path) {
        debug!(
            "got while removing combined repo: {}",
            Report::from_error(e)
        );
    }

    info!("cloning local repository");
    debug!("local repository at `{url}`");
    let repo = {
        let git_progress = Git::new();
        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(git_progress.remote_callbacks());
        let x = RepoBuilder::new()
            .fetch_options(fetch_options)
            .clone(url.as_str(), &repo_path)
            .context(GitSnafu {
                what: "clone local repo",
            })?;
        x
    };
    if !repo.submodules().unwrap().is_empty() {
        panic!("submodules not supported yet");
    }
    let repo_use = RepositoryUse::identify(&repo)?;

    for (other_kind, other_repo) in repo_use.other_repos().iter().progress_ext("Merge Repos") {
        info!("fetching {other_kind} repository");
        debug!("{other_kind} repository at `{other_repo}`");
        let mut remote = repo.remote_anonymous(&other_repo).context(GitSnafu {
            what: "creating remote",
        })?;
        {
            let git_progress = Git::new();
            let mut fetch_options = FetchOptions::new();
            fetch_options.remote_callbacks(git_progress.remote_callbacks());
            remote
                .fetch(&["master:master-other"], Some(&mut fetch_options), None)
                .context(GitSnafu {
                    what: "fetching remote repo",
                })?;
        }
        let master = branch_to_commit(&repo, "master")?;
        let master_tree = master.tree().context(GitSnafu {
            what: "getting master tree",
        })?;
        let master_other = branch_to_commit(&repo, "master-other")?;
        let other_tree = master_other.tree().context(GitSnafu {
            what: "getting other tree",
        })?;

        let mut tree_builder = TreeUpdateBuilder::new();
        let prefix = format!("{}/", super::CONTENT_DIR);
        let mut walk_error: Option<Error> = None;
        let walk_result = other_tree.walk(git2::TreeWalkMode::PreOrder, |a, b| {
            if !a.starts_with(&prefix) {
                if !a.is_empty() || b.name() != Some(super::CONTENT_DIR) {
                    return TreeWalkResult::Skip;
                }
            }

            let name = match b.name() {
                Some(n) => n,
                None => {
                    walk_error = Some(
                        UpdateTreeSnafu {
                            msg: format!("tree entry without name in `{a}`"),
                        }
                        .build(),
                    );
                    return TreeWalkResult::Abort;
                }
            };

            let path = format!("{}{}", a, name);
            match b.kind() {
                Some(ObjectType::Blob) => (),
                Some(ObjectType::Tree) => return TreeWalkResult::Ok,
                kind => {
                    walk_error = Some(
                        UpdateTreeSnafu {
                            msg: format!("unknown blob type `{kind:?}` for `{path}`"),
                        }
                        .build(),
                    );
                    return TreeWalkResult::Abort;
                }
            }

            if let Err(e) = check_conflict(&master_tree, Path::new(&path), b) {
                walk_error = Some(e);
                return TreeWalkResult::Abort;
            }

            debug!("upsert `{path}`");
            tree_builder.upsert(path, b.id(), FileMode::Blob);
            TreeWalkResult::Ok
        });

        if let Some(error) = walk_error {
            return Err(error);
        }

        walk_result.context(GitSnafu {
            what: "traverse tree",
        })?;

        let merged_tree_oid = tree_builder
            .create_updated(&repo, &master_tree)
            .context(GitSnafu { what: "build tree" })?;
        let merged_tree = repo.find_tree(merged_tree_oid).unwrap();

        let sig = repo.signature().context(GitSnafu {
            what: "commit signature",
        })?;
        let msg = format!("Merge {other_repo}");
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            &msg,
            &merged_tree,
            &[&master, &master_other],
        )
        .context(GitSnafu { what: "committing" })?;

        repo.checkout_head(Some(CheckoutBuilder::default().force()))
            .context(GitSnafu {
                what: "checkout merged",
            })?;

        repo.find_branch("master-other", BranchType::Local)
            .context(GitSnafu {
                what: "find master-other",
            })?
            .delete()
            .context(GitSnafu {
                what: "delete master-other",
            })?;
    }

    Ok(())
}
