/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    ffi::OsStr,
    fmt,
    path::{Path, PathBuf},
};

use crate::{
    cache::Cache,
    progress::{Git, ProgressIteratorExt},
};
use enum_map::{enum_map, Enum, EnumMap};
use git2::{
    build::{CheckoutBuilder, TreeUpdateBuilder},
    BranchType, Commit, FetchOptions, FileMode, ObjectType, Repository, RepositoryOpenFlags,
    StatusOptions, Tree, TreeEntry, TreeWalkResult,
};
use log::{debug, info};
use snafu::{ensure, Backtrace, IntoError, OptionExt, ResultExt, Snafu};
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
    #[snafu(context(false))]
    Cache {
        #[snafu(backtrace)]
        source: crate::cache::Error,
    },
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
        RepositoryUse::Eips => "https://github.com/eips-wg/EIPs.git",
        RepositoryUse::Ercs => "https://github.com/eips-wg/ERCs.git",
    };
}

impl RepositoryUse {
    const EIP_COMMIT: &str = "0f44e2b94df4e504bb7b912f56ebd712db2ad396";
    const ERC_COMMIT: &str = "8dd085d159cb123f545c272c0d871a5339550e79";

    fn identify(path: &Path) -> Result<Self, Error> {
        let repo = Repository::open_ext(path, RepositoryOpenFlags::NO_SEARCH, &[] as &[&OsStr])
            .context(GitSnafu {
                what: "identify open",
            })?;
        let eip = repo.revparse_single(Self::EIP_COMMIT).is_ok();
        let erc = repo.revparse_single(Self::ERC_COMMIT).is_ok();

        match (eip, erc) {
            (true, false) => Ok(Self::Eips),
            (false, true) => Ok(Self::Ercs),
            (_, _) => IdentifySnafu.fail(),
        }
    }

    fn url(self) -> &'static str {
        REPO_URLS[self]
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

pub fn merge_repositories(root_path: &Path, build_path: &Path) -> Result<Vec<PathBuf>, Error> {
    check_dirty(root_path)?;

    let repo_use = RepositoryUse::identify(root_path)?;

    let url = Url::from_directory_path(root_path)
        .ok()
        .context(PathUrlSnafu { path: root_path })?;

    info!("cloning local repository");
    debug!("local repository at `{url}`");
    let repo = open_or_init(build_path)?;
    let master = fetch(&repo, url.as_str(), "HEAD")?;
    repo.set_head_detached(master.id())
        .context(GitSnafu { what: "detach" })?;
    let branch = repo.branch("master", &master, true).context(GitSnafu {
        what: "branch master",
    })?;
    repo.set_head("refs/heads/master")
        .context(GitSnafu { what: "set head" })?;
    assert!(branch.is_head());
    repo.checkout_head(Some(
        CheckoutBuilder::default()
            .remove_ignored(true)
            .remove_untracked(true)
            .force(),
    ))
    .context(GitSnafu {
        what: "checkout local",
    })?;

    if !repo.submodules().unwrap().is_empty() {
        panic!("submodules not supported yet");
    }

    info!("fetching latest {repo_use} repository");
    let latest_master = fetch(&repo, repo_use.url(), "master")?;
    let merge_base = repo
        .merge_base(master.id(), latest_master.id())
        .context(GitSnafu { what: "merge base" })?;
    debug!(
        "merge base of `{}` (local) and `{}` (latest) is `{}`",
        master.id(),
        latest_master.id(),
        merge_base
    );

    let merge_base_tree = repo
        .find_commit(merge_base)
        .context(GitSnafu {
            what: "getting merge base commit",
        })?
        .tree()
        .context(GitSnafu {
            what: "getting merge base tree",
        })?;

    let master_tree = master.tree().context(GitSnafu {
        what: "getting master tree",
    })?;

    let diff = repo
        .diff_tree_to_tree(Some(&merge_base_tree), Some(&master_tree), None)
        .context(GitSnafu {
            what: "comparing merge base to master",
        })?;

    let changed_files = diff
        .deltas()
        .filter_map(|d| d.new_file().path())
        .map(Path::to_path_buf)
        .collect();

    for (other_kind, other_repo) in repo_use.other_repos().iter().progress_ext("Merge Repos") {
        info!("fetching {other_kind} repository");
        let master_other = fetch(&repo, &other_repo, "master:master-other")?;
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

    Ok(changed_files)
}

fn fetch<'a>(repo: &'a Repository, url: &'_ str, refspec: &'_ str) -> Result<Commit<'a>, Error> {
    debug!("fetching repository at `{url}`");
    let mut remote = repo.remote_anonymous(url).context(GitSnafu {
        what: "creating remote",
    })?;
    {
        let git_progress = Git::new();
        let mut fetch_options = FetchOptions::new();
        fetch_options.remote_callbacks(git_progress.remote_callbacks());
        remote
            .fetch(&[refspec], Some(&mut fetch_options), None)
            .context(GitSnafu {
                what: "fetching repo",
            })?;
    }
    let commit = repo
        .revparse_single("FETCH_HEAD")
        .context(GitSnafu {
            what: "revparse FETCH_HEAD",
        })?
        .peel_to_commit()
        .context(GitSnafu {
            what: "peel FETCH_HEAD",
        })?;
    Ok(commit)
}

fn open_or_init(dir: &Path) -> Result<Repository, Error> {
    let repo = match Repository::open_ext(dir, RepositoryOpenFlags::NO_SEARCH, &[] as &[&OsStr]) {
        Ok(r) => r,
        Err(e) if e.code() == git2::ErrorCode::NotFound => {
            Repository::init(&dir).context(GitSnafu { what: "init repo" })?
        }
        Err(e) => return Err(GitSnafu { what: "open repo" }.into_error(e)),
    };
    Ok(repo)
}

impl Cache {
    pub fn repo(&self, url: &str, commit: &str) -> Result<PathBuf, Error> {
        let key = format!("git\0{url}");
        let dir = self.dir(&key)?;

        let repo = open_or_init(&dir)?;
        let object = match repo.revparse_single(commit) {
            Ok(c) => c,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                fetch(&repo, url, "master")?;
                repo.revparse_single(commit).context(GitSnafu {
                    what: "revparse cached commit",
                })?
            }
            Err(e) => {
                return Err(GitSnafu {
                    what: "revparse cached commit",
                }
                .into_error(e))
            }
        };

        repo.checkout_tree(&object, Some(CheckoutBuilder::new().force()))
            .context(GitSnafu {
                what: "checkout cached commit",
            })?;
        repo.set_head_detached(object.id()).context(GitSnafu {
            what: "set detached head",
        })?;

        Ok(dir)
    }
}
