/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    collections::BTreeSet,
    collections::HashMap,
    ffi::OsStr,
    path::{absolute, Path, PathBuf},
};

use crate::{
    cache::Cache,
    config::{Location, Locations},
    progress::{Git, ProgressIteratorExt},
};
use git2::{
    build::{CheckoutBuilder, TreeUpdateBuilder},
    Commit, FetchOptions, FileMode, ObjectType, Oid, RepositoryOpenFlags, Signature, Status,
    StatusOptions, Tree, TreeEntry, TreeWalkResult,
};
use log::{debug, info};
use snafu::{ensure, Backtrace, IntoError, OptionExt, ResultExt, Snafu};
use url::Url;

const DIRTY_PATH_DISPLAY_LIMIT: usize = 10;

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("cannot convert path into URL (`{}`)", path.to_string_lossy()))]
    PathUrl { path: PathBuf, backtrace: Backtrace },
    #[snafu(display("i/o error accessing `{}`", path.to_string_lossy()))]
    Io {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("unable to {what}"))]
    Git {
        what: &'static str,
        source: git2::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("unable to determine which repository is being built (could be: {})", titles.join(", ")))]
    AmbiguousIdentify {
        titles: Vec<String>,
        backtrace: Backtrace,
    },
    #[snafu(display("unable to determine which repository is being built (none match)"))]
    NoIdentify { backtrace: Backtrace },
    #[snafu(display("{message}"))]
    Dirty {
        message: String,
        backtrace: Backtrace,
    },
    #[snafu(display(
        "dirty mode cannot materialize conflicted path `{}`; resolve the conflict and try again",
        path.to_string_lossy()
    ))]
    DirtyConflict { path: PathBuf, backtrace: Backtrace },
    #[snafu(display(
        "dirty mode cannot materialize `{}` because it is not a tracked file or symlink in the working tree",
        path.to_string_lossy()
    ))]
    DirtyUnsupportedPath { path: PathBuf, backtrace: Backtrace },
    #[snafu(display("unable to update tree ({msg})"))]
    UpdateTree { msg: String, backtrace: Backtrace },
    #[snafu(context(false))]
    Cache {
        #[snafu(backtrace)]
        source: crate::cache::Error,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMaterialization {
    Clean,
    Dirty,
}

#[derive(Debug, Clone)]
pub struct RepositoryUse {
    pub title: String,
    pub location: Location,
    pub other_repos: HashMap<String, Url>,
}

impl Locations {
    pub fn identify_repository(&self, path: &Path) -> Result<RepositoryUse, Error> {
        let repo =
            git2::Repository::open_ext(path, RepositoryOpenFlags::NO_SEARCH, &[] as &[&OsStr])
                .context(GitSnafu {
                    what: "identify open",
                })?;

        let containing_locations: Vec<_> = self
            .0
            .iter()
            .filter_map(|(k, v)| match repo.revparse_single(&v.identifying_commit) {
                Ok(_) => Some((k, v)),
                _ => None,
            })
            .collect();

        ensure!(
            containing_locations.len() < 2,
            AmbiguousIdentifySnafu {
                titles: containing_locations
                    .into_iter()
                    .map(|x| x.0)
                    .cloned()
                    .collect::<Vec<_>>(),
            }
        );
        ensure!(containing_locations.len() == 1, NoIdentifySnafu);

        let (title, location) = containing_locations[0];

        // TODO: this is a bit weird, and is a leftover from the previous architecture.
        let other_repos = self
            .0
            .iter()
            .filter_map(|(k, v)| {
                if k == title || v.repository == location.repository {
                    None
                } else {
                    Some((k.clone(), v.repository.clone()))
                }
            })
            .collect();

        Ok(RepositoryUse {
            title: title.clone(),
            location: location.clone(),
            other_repos,
        })
    }
}

impl RepositoryUse {
    pub fn only_other_repo(&self) -> Option<(&str, &Url)> {
        let mut repos = self.other_repos.iter();
        let next = repos.next()?;
        if repos.next().is_some() {
            None
        } else {
            Some((next.0.as_str(), next.1))
        }
    }
}

fn is_generated_path(path: &Path) -> bool {
    path.components()
        .next()
        .map(|component| component.as_os_str() == OsStr::new(super::BUILD_DIR))
        .unwrap_or(false)
}

fn dirty_statuses(repo: &git2::Repository) -> Result<git2::Statuses<'_>, Error> {
    let mut options = StatusOptions::default();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    repo.statuses(Some(&mut options)).context(GitSnafu {
        what: "get root repository status",
    })
}

fn format_dirty_rejection(tracked_paths: &BTreeSet<PathBuf>, untracked_count: usize) -> String {
    let mut lines = vec![String::from(
        "working tree or index has uncommitted modifications; the clean/default path requires a clean working tree:",
    )];

    for path in tracked_paths.iter().take(DIRTY_PATH_DISPLAY_LIMIT) {
        lines.push(format!("- {}", path.to_string_lossy()));
    }

    if tracked_paths.len() > DIRTY_PATH_DISPLAY_LIMIT {
        lines.push(format!(
            "- ... and {} more tracked path(s)",
            tracked_paths.len() - DIRTY_PATH_DISPLAY_LIMIT
        ));
    }

    if untracked_count > 0 {
        lines.push(format!(
            "- ... plus {} untracked file(s) not listed",
            untracked_count
        ));
    }

    lines.push(String::new());

    if untracked_count > 0 {
        lines.push(String::from(
            "Use `--profile dirty` or `--allow-dirty` to include tracked local changes, and commit/stash/remove any untracked files first.",
        ));
    } else {
        lines.push(String::from(
            "Use `--profile dirty` or `--allow-dirty` to include tracked local changes, or commit/stash them first.",
        ));
    }

    lines.join("\n")
}

pub fn check_dirty(root_path: &Path) -> Result<(), Error> {
    let (tracked_paths, untracked_count) = collect_dirty_paths(root_path)?;

    if tracked_paths.is_empty() && untracked_count == 0 {
        Ok(())
    } else {
        DirtySnafu {
            message: format_dirty_rejection(&tracked_paths, untracked_count),
        }
        .fail()
    }
}

fn entry_path(entry: &git2::StatusEntry<'_>) -> Option<PathBuf> {
    entry
        .head_to_index()
        .and_then(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
        .or_else(|| {
            entry
                .index_to_workdir()
                .and_then(|delta| delta.new_file().path().or_else(|| delta.old_file().path()))
        })
        .or_else(|| entry.path().map(Path::new))
        .map(Path::to_path_buf)
}

fn collect_dirty_paths(root_path: &Path) -> Result<(BTreeSet<PathBuf>, usize), Error> {
    let repo = git2::Repository::open(root_path).context(GitSnafu {
        what: "open root repository",
    })?;
    let statuses = dirty_statuses(&repo)?;
    let mut paths = BTreeSet::new();
    let mut untracked_count = 0;

    for entry in statuses.iter() {
        let status = entry.status();
        let path = entry_path(&entry).unwrap_or_else(|| PathBuf::from("<unknown>"));

        if status.contains(Status::CONFLICTED) {
            return DirtyConflictSnafu { path }.fail();
        }

        if status == Status::CURRENT || status == Status::IGNORED {
            continue;
        }

        if status == Status::WT_NEW {
            if !is_generated_path(&path) {
                untracked_count += 1;
            }
            continue;
        }

        if let Some(delta) = entry.head_to_index() {
            if let Some(old_path) = delta
                .old_file()
                .path()
                .filter(|path| !is_generated_path(path))
            {
                paths.insert(old_path.to_path_buf());
            }
            if let Some(new_path) = delta
                .new_file()
                .path()
                .filter(|path| !is_generated_path(path))
            {
                paths.insert(new_path.to_path_buf());
            }
        }

        if let Some(delta) = entry.index_to_workdir() {
            if let Some(old_path) = delta
                .old_file()
                .path()
                .filter(|path| !is_generated_path(path))
            {
                paths.insert(old_path.to_path_buf());
            }
            if let Some(new_path) = delta
                .new_file()
                .path()
                .filter(|path| !is_generated_path(path))
            {
                paths.insert(new_path.to_path_buf());
            }
        }

        if !is_generated_path(&path) {
            paths.insert(path);
        }
    }

    Ok((paths, untracked_count))
}

pub fn working_tree_paths(root_path: &Path) -> Result<Vec<PathBuf>, Error> {
    let (paths, _) = collect_dirty_paths(root_path)?;
    Ok(paths.into_iter().collect())
}

fn remove_existing_path(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path)
        }
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_index_path(index: &mut git2::Index, path: &Path) -> Result<(), Error> {
    match index.remove_path(path) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == git2::ErrorCode::NotFound => match index.remove_dir(path, -1)
        {
            Ok(()) => Ok(()),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(()),
            Err(error) => Err(GitSnafu {
                what: "remove dirty path from index",
            }
            .into_error(error)),
        },
        Err(error) => Err(GitSnafu {
            what: "remove dirty path from index",
        }
        .into_error(error)),
    }
}

#[cfg(target_family = "unix")]
fn copy_symlink(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let target = std::fs::read_link(source)?;
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(target_family = "windows")]
fn copy_symlink(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let target = std::fs::read_link(source)?;
    let resolved_target = source
        .parent()
        .map(|parent| parent.join(&target))
        .unwrap_or_else(|| target.clone());

    if std::fs::metadata(&resolved_target)
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}

#[cfg(not(any(target_family = "unix", target_family = "windows")))]
fn copy_symlink(_source: &Path, _destination: &Path) -> Result<(), std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no symlink implementation available",
    ))
}

fn sync_dirty_path(
    source_root: &Path,
    working_root: &Path,
    index: &mut git2::Index,
    relative_path: &Path,
) -> Result<(), Error> {
    let source_path = source_root.join(relative_path);
    let working_path = working_root.join(relative_path);

    match std::fs::symlink_metadata(&source_path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            remove_existing_path(&working_path).context(IoSnafu {
                path: working_path.clone(),
            })?;
            remove_index_path(index, relative_path)?;
            Ok(())
        }
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            if let Some(parent) = working_path.parent() {
                std::fs::create_dir_all(parent).context(IoSnafu {
                    path: parent.to_path_buf(),
                })?;
            }

            remove_existing_path(&working_path).context(IoSnafu {
                path: working_path.clone(),
            })?;

            if metadata.file_type().is_symlink() {
                copy_symlink(&source_path, &working_path).context(IoSnafu {
                    path: working_path.clone(),
                })?;
            } else {
                std::fs::copy(&source_path, &working_path).context(IoSnafu {
                    path: source_path.clone(),
                })?;
            }

            index.add_path(relative_path).context(GitSnafu {
                what: "add dirty path to index",
            })?;
            Ok(())
        }
        Ok(_) => DirtyUnsupportedPathSnafu {
            path: relative_path.to_path_buf(),
        }
        .fail(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_existing_path(&working_path).context(IoSnafu {
                path: working_path.clone(),
            })?;
            remove_index_path(index, relative_path)?;
            Ok(())
        }
        Err(error) => Err(IoSnafu { path: source_path }.into_error(error)),
    }
}

fn materialize_dirty_tree(
    source_root: &Path,
    working_repo: &git2::Repository,
    local_head: Oid,
) -> Result<Oid, Error> {
    let (dirty_paths, untracked_count) = collect_dirty_paths(source_root)?;
    if untracked_count > 0 {
        info!("dirty mode ignores untracked files in the active content repo");
    }

    if dirty_paths.is_empty() {
        return Ok(local_head);
    }

    let working_root = working_repo.workdir().context(UpdateTreeSnafu::<String> {
        msg: "build repository workdir is unavailable".into(),
    })?;
    let mut index = working_repo.index().context(GitSnafu {
        what: "open build repository index",
    })?;

    for path in dirty_paths {
        sync_dirty_path(source_root, working_root, &mut index, &path)?;
    }

    index.write().context(GitSnafu {
        what: "write build repository index",
    })?;
    let tree_id = index.write_tree().context(GitSnafu {
        what: "write dirty materialization tree",
    })?;
    let tree = working_repo.find_tree(tree_id).context(GitSnafu {
        what: "find dirty materialization tree",
    })?;

    let sig = Signature::now("eips-build", "eips-build@eips-build.invalid").context(GitSnafu {
        what: "dirty commit signature",
    })?;
    let parent = working_repo.find_commit(local_head).context(GitSnafu {
        what: "find clean local head commit",
    })?;
    let dirty_head = working_repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "Dirty working tree materialization",
            &tree,
            &[&parent],
        )
        .context(GitSnafu {
            what: "commit dirty working tree materialization",
        })?;

    info!(
        "materialized tracked dirty changes from the active content repo into `{}`",
        working_root.to_string_lossy()
    );

    Ok(dirty_head)
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

pub struct Fresh {
    src_repo_use: RepositoryUse,
    src_repo_path: PathBuf,
    src_repo_url: Url,
    source_materialization: SourceMaterialization,

    working_repo: git2::Repository,
}

impl Fresh {
    pub fn new(
        root_path: &Path,
        build_path: &Path,
        src_repo_use: RepositoryUse,
        source_materialization: SourceMaterialization,
    ) -> Result<Self, Error> {
        let root_path = absolute(root_path).context(IoSnafu { path: root_path })?;
        if source_materialization == SourceMaterialization::Clean {
            check_dirty(&root_path)?;
        }
        let src_repo_url = Url::from_directory_path(&root_path)
            .ok()
            .context(PathUrlSnafu {
                path: root_path.clone(),
            })?;

        debug!("source repository at `{src_repo_url}`");

        let working_repo = open_or_init(build_path)?;

        Ok(Self {
            working_repo,
            src_repo_path: root_path,
            src_repo_url,
            src_repo_use,
            source_materialization,
        })
    }

    pub fn clone_src(self) -> Result<SourceOnly, Error> {
        info!("cloning local repository");
        let master = fetch(
            &self.working_repo,
            self.src_repo_url.as_str(),
            "HEAD:refs/build-eips/source-head",
        )?;
        self.working_repo
            .set_head_detached(master.id())
            .context(GitSnafu { what: "detach" })?;
        let branch = self
            .working_repo
            .branch("master", &master, true)
            .context(GitSnafu {
                what: "branch master",
            })?;
        self.working_repo
            .set_head("refs/heads/master")
            .context(GitSnafu { what: "set head" })?;
        assert!(branch.is_head());
        self.working_repo
            .checkout_head(Some(
                CheckoutBuilder::default()
                    .remove_ignored(true)
                    .remove_untracked(true)
                    .force(),
            ))
            .context(GitSnafu {
                what: "checkout local",
            })?;

        if !self.working_repo.submodules().unwrap().is_empty() {
            panic!("submodules not supported yet");
        }

        let mut local_head = master.id();
        drop(master);
        drop(branch);

        if self.source_materialization == SourceMaterialization::Dirty {
            local_head =
                materialize_dirty_tree(&self.src_repo_path, &self.working_repo, local_head)?;
        }

        Ok(SourceOnly {
            local_head,
            src_repo_use: self.src_repo_use,
            working_repo: self.working_repo,
        })
    }
}

pub struct SourceOnly {
    src_repo_use: RepositoryUse,

    working_repo: git2::Repository,
    local_head: Oid,
}

impl SourceOnly {
    pub fn fetch_upstream(self) -> Result<SourceWithUpstream, Error> {
        info!("fetching latest {} repository", self.src_repo_use.title);
        let latest_master = fetch(
            &self.working_repo,
            self.src_repo_use.location.repository.as_str(),
            "master:refs/build-eips/upstream-head",
        )?;
        let upstream_head = latest_master.id();
        drop(latest_master);
        Ok(SourceWithUpstream {
            upstream_head,
            local_head: self.local_head,
            src_repo_use: self.src_repo_use,
            working_repo: self.working_repo,
        })
    }
}

pub struct SourceWithUpstream {
    src_repo_use: RepositoryUse,

    working_repo: git2::Repository,
    local_head: Oid,
    upstream_head: Oid,
}

impl SourceWithUpstream {
    fn local_head_tree(&self) -> Result<Tree<'_>, Error> {
        let commit = self
            .working_repo
            .find_commit(self.local_head)
            .context(GitSnafu {
                what: "local commit from id",
            })?;
        let master_tree = commit.tree().context(GitSnafu {
            what: "getting master tree",
        })?;

        Ok(master_tree)
    }

    pub fn changed_files(&self) -> Result<Vec<PathBuf>, Error> {
        let merge_base = self
            .working_repo
            .merge_base(self.local_head, self.upstream_head)
            .context(GitSnafu { what: "merge base" })?;
        debug!(
            "merge base of `{}` (local) and `{}` (latest) is `{}`",
            self.local_head, self.upstream_head, merge_base
        );

        let merge_base_tree = self
            .working_repo
            .find_commit(merge_base)
            .context(GitSnafu {
                what: "getting merge base commit",
            })?
            .tree()
            .context(GitSnafu {
                what: "getting merge base tree",
            })?;

        let master_tree = self.local_head_tree()?;
        let diff = self
            .working_repo
            .diff_tree_to_tree(Some(&merge_base_tree), Some(&master_tree), None)
            .context(GitSnafu {
                what: "comparing merge base to master",
            })?;

        let changed_files = diff
            .deltas()
            .filter_map(|d| d.new_file().path())
            .map(Path::to_path_buf)
            .collect();

        Ok(changed_files)
    }

    fn check_ignored(&self, tree: &Tree) -> Result<(), Error> {
        let mut walk_error = None;
        let walk_result = tree.walk(git2::TreeWalkMode::PreOrder, |a, b| {
            if b.kind() != Some(ObjectType::Blob) {
                return TreeWalkResult::Ok;
            }

            let path = match b.name() {
                None => a.to_owned(),
                Some(p) => format!("{a}{p}"),
            };

            debug!("checking if `{path}` is ignored");

            match self.working_repo.is_path_ignored(&path) {
                Ok(false) => TreeWalkResult::Ok,
                Ok(true) => {
                    walk_error = Some(
                        UpdateTreeSnafu {
                            msg: format!("contains ignored path `{path}`"),
                        }
                        .build(),
                    );
                    TreeWalkResult::Abort
                }
                Err(e) => {
                    walk_error = Some(
                        GitSnafu {
                            what: "check ignored",
                        }
                        .into_error(e),
                    );
                    TreeWalkResult::Abort
                }
            }
        });

        if let Some(error) = walk_error {
            return Err(error);
        }

        walk_result.context(GitSnafu {
            what: "traverse tree",
        })?;

        Ok(())
    }

    pub fn merge(&self) -> Result<(), Error> {
        let repo_use = &self.src_repo_use;
        let master_tree = self.local_head_tree()?;
        let mut local_head = self.local_head;
        for (other_kind, other_repo) in repo_use.other_repos.iter().progress_ext("Merge Repos") {
            info!("fetching {other_kind} repository");
            // Local sibling overrides should follow the checked-out repo HEAD instead of assuming `master`.
            let other_refspec = if other_repo.scheme() == "file" {
                "HEAD:refs/build-eips/other-head"
            } else {
                "master:refs/build-eips/other-head"
            };
            let master_other = fetch(&self.working_repo, other_repo.as_str(), other_refspec)?;
            let other_tree = master_other.tree().context(GitSnafu {
                what: "getting other tree",
            })?;

            let mut tree_builder = TreeUpdateBuilder::new();
            let prefix = format!("{}/", super::CONTENT_DIR);
            let mut walk_error: Option<Error> = None;
            let walk_result = other_tree.walk(git2::TreeWalkMode::PreOrder, |a, b| {
                if !a.starts_with(&prefix)
                    && (!a.is_empty() || b.name() != Some(super::CONTENT_DIR))
                {
                    return TreeWalkResult::Skip;
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
                .create_updated(&self.working_repo, &master_tree)
                .context(GitSnafu { what: "build tree" })?;
            let merged_tree = self.working_repo.find_tree(merged_tree_oid).unwrap();

            self.check_ignored(&merged_tree)?;

            let sig = Signature::now("eips-build", "eips-build@eips-build.invalid").context(
                GitSnafu {
                    what: "commit signature",
                },
            )?;
            let msg = format!("Merge {other_repo}");
            let master = self
                .working_repo
                .find_commit(local_head)
                .context(GitSnafu {
                    what: "find local head commit",
                })?;
            local_head = self
                .working_repo
                .commit(
                    Some("HEAD"),
                    &sig,
                    &sig,
                    &msg,
                    &merged_tree,
                    &[&master, &master_other],
                )
                .context(GitSnafu { what: "committing" })?;

            self.working_repo
                .checkout_head(Some(CheckoutBuilder::default().force()))
                .context(GitSnafu {
                    what: "checkout merged",
                })?;
        }

        Ok(())
    }
}

fn fetch<'a>(
    repo: &'a git2::Repository,
    url: &'_ str,
    refspec: &'_ str,
) -> Result<Commit<'a>, Error> {
    debug!("fetching repository at `{url}`");
    let remote_name = "__build_eips_fetch";
    match repo.remote_delete(remote_name) {
        Ok(()) => (),
        Err(error) if error.code() == git2::ErrorCode::NotFound => (),
        Err(error) => {
            return Err(GitSnafu {
                what: "deleting temporary remote",
            }
            .into_error(error))
        }
    }

    let mut remote = repo.remote(remote_name, url).context(GitSnafu {
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
    drop(remote);
    repo.remote_delete(remote_name).context(GitSnafu {
        what: "deleting temporary remote",
    })?;

    let fetched_ref = refspec
        .split_once(':')
        .map(|(_, destination)| destination)
        .filter(|destination| !destination.is_empty())
        .unwrap_or("FETCH_HEAD");
    let commit = repo
        .revparse_single(fetched_ref)
        .context(GitSnafu {
            what: "revparse fetched ref",
        })?
        .peel_to_commit()
        .context(GitSnafu {
            what: "peel fetched ref",
        })?;
    Ok(commit)
}

fn open_or_init(dir: &Path) -> Result<git2::Repository, Error> {
    let repo =
        match git2::Repository::open_ext(dir, RepositoryOpenFlags::NO_SEARCH, &[] as &[&OsStr]) {
            Ok(r) => r,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                git2::Repository::init(dir).context(GitSnafu { what: "init repo" })?
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
