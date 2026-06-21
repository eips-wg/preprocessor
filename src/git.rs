/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    io::ErrorKind,
    path::{absolute, Path, PathBuf},
};

use crate::config::RepositoryUse;
use crate::{
    layout::{BUILD_DIR, CONTENT_DIR},
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
const CONTENT_INDEX_PATH: &str = "content/_index.md";

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
    #[snafu(display(
        "workspace path `{}` already exists but is not a usable git repository",
        path.to_string_lossy()
    ))]
    ExistingWorkspacePath { path: PathBuf, backtrace: Backtrace },
    #[snafu(display(
        "workspace repository `{}` is unusable: {reason}",
        path.to_string_lossy()
    ))]
    UnusableWorkspaceRepository {
        path: PathBuf,
        reason: &'static str,
        backtrace: Backtrace,
    },
    #[snafu(display(
        "fresh workspace repository `{}` was missing before checkout",
        path.to_string_lossy()
    ))]
    MissingFreshWorkspaceRepository { path: PathBuf, backtrace: Backtrace },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMaterialization {
    Clean,
    Dirty,
}

pub fn repository_available(path: &Path) -> bool {
    git2::Repository::open(path).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneOutcome {
    Fresh,
    Existing,
}

fn ensure_usable_workspace_repository(
    repository: &git2::Repository,
    path: &Path,
) -> Result<(), Error> {
    if repository.workdir().is_none() {
        return UnusableWorkspaceRepositorySnafu {
            path: path.to_path_buf(),
            reason: "it is bare",
        }
        .fail();
    }

    if repository.head().is_err() {
        return UnusableWorkspaceRepositorySnafu {
            path: path.to_path_buf(),
            reason: "it has no HEAD commit",
        }
        .fail();
    }

    Ok(())
}

fn open_existing_workspace_repository(
    destination: &Path,
) -> Result<Option<git2::Repository>, Error> {
    match git2::Repository::open_ext(
        destination,
        RepositoryOpenFlags::NO_SEARCH,
        &[] as &[&OsStr],
    ) {
        Ok(repository) => {
            ensure_usable_workspace_repository(&repository, destination)?;
            Ok(Some(repository))
        }
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            match std::fs::symlink_metadata(destination) {
                Ok(_) => ExistingWorkspacePathSnafu {
                    path: destination.to_path_buf(),
                }
                .fail(),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
                Err(error) => Err(IoSnafu {
                    path: destination.to_path_buf(),
                }
                .into_error(error)),
            }
        }
        Err(error) => match std::fs::symlink_metadata(destination) {
            Ok(_) => ExistingWorkspacePathSnafu {
                path: destination.to_path_buf(),
            }
            .fail(),
            Err(metadata_error) if metadata_error.kind() == ErrorKind::NotFound => Err(GitSnafu {
                what: "open existing workspace repository",
            }
            .into_error(error)),
            Err(metadata_error) => Err(IoSnafu {
                path: destination.to_path_buf(),
            }
            .into_error(metadata_error)),
        },
    }
}

pub fn clone_missing_repo(url: &str, destination: &Path) -> Result<CloneOutcome, Error> {
    if open_existing_workspace_repository(destination)?.is_some() {
        info!(
            "using existing workspace repo `{}`",
            destination.to_string_lossy()
        );
        return Ok(CloneOutcome::Existing);
    }

    info!("cloning `{url}` into `{}`", destination.to_string_lossy());
    let repository = git2::Repository::clone(url, destination).context(GitSnafu {
        what: "clone workspace repository",
    })?;
    ensure_usable_workspace_repository(&repository, destination)?;
    Ok(CloneOutcome::Fresh)
}

pub fn checkout_fresh_clone_at_commit(
    destination: &Path,
    repository_url: &str,
    commit: &str,
) -> Result<(), Error> {
    let Some(repository) = open_existing_workspace_repository(destination)? else {
        return MissingFreshWorkspaceRepositorySnafu {
            path: destination.to_path_buf(),
        }
        .fail();
    };
    let commit = match repository
        .revparse_single(commit)
        .and_then(|object| object.peel_to_commit())
    {
        Ok(commit) => commit,
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::InvalidSpec
            ) =>
        {
            let refspec = format!("+{commit}:refs/build-eips/theme-pin");
            fetch(&repository, repository_url, &refspec)?
        }
        Err(error) => {
            return Err(GitSnafu {
                what: "resolve manifest theme commit",
            }
            .into_error(error));
        }
    };

    repository
        .set_head_detached(commit.id())
        .context(GitSnafu {
            what: "detach manifest theme commit",
        })?;
    repository
        .checkout_head(Some(CheckoutBuilder::default().force()))
        .context(GitSnafu {
            what: "checkout manifest theme commit",
        })?;

    Ok(())
}

fn is_generated_path(path: &Path) -> bool {
    path.components()
        .next()
        .map(|component| component.as_os_str() == OsStr::new(BUILD_DIR))
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
        "working tree or index has uncommitted modifications; the selected clean source path requires a clean working tree:",
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
            "For local build/serve/check commands, run without `--clean` to include tracked local changes. For `--clean` runs, commit or stash tracked changes first. Commit/stash/remove untracked files before retrying.",
        ));
    } else {
        lines.push(String::from(
            "For local build/serve/check commands, run without `--clean` to include tracked local changes. For `--clean` runs, commit or stash tracked changes first.",
        ));
    }

    lines.join("\n")
}

pub fn check_dirty(root_path: &Path) -> Result<(), Error> {
    let (tracked_paths, untracked_count) =
        collect_dirty_paths(root_path, |path| !is_generated_path(path))?;

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

fn collect_dirty_paths(
    root_path: &Path,
    include_path: impl Fn(&Path) -> bool,
) -> Result<(BTreeSet<PathBuf>, usize), Error> {
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
            if include_path(&path) {
                untracked_count += 1;
            }
            continue;
        }

        if let Some(delta) = entry.head_to_index() {
            if let Some(old_path) = delta.old_file().path().filter(|path| include_path(path)) {
                paths.insert(old_path.to_path_buf());
            }
            if let Some(new_path) = delta.new_file().path().filter(|path| include_path(path)) {
                paths.insert(new_path.to_path_buf());
            }
        }

        if let Some(delta) = entry.index_to_workdir() {
            if let Some(old_path) = delta.old_file().path().filter(|path| include_path(path)) {
                paths.insert(old_path.to_path_buf());
            }
            if let Some(new_path) = delta.new_file().path().filter(|path| include_path(path)) {
                paths.insert(new_path.to_path_buf());
            }
        }

        if include_path(&path) {
            paths.insert(path);
        }
    }

    Ok((paths, untracked_count))
}

pub fn working_tree_paths(root_path: &Path) -> Result<Vec<PathBuf>, Error> {
    let (paths, _) = collect_dirty_paths(root_path, |path| !is_generated_path(path))?;
    Ok(paths.into_iter().collect())
}

pub fn tracked_working_tree_paths(root_path: &Path) -> Result<Vec<PathBuf>, Error> {
    let (paths, _) = collect_dirty_paths(root_path, |_| true)?;
    Ok(paths.into_iter().collect())
}

pub fn materialize_working_tree(source_root: &Path, destination_root: &Path) -> Result<(), Error> {
    remove_existing_path(destination_root).with_context(|_| IoSnafu {
        path: destination_root.to_path_buf(),
    })?;
    std::fs::create_dir_all(destination_root).with_context(|_| IoSnafu {
        path: destination_root.to_path_buf(),
    })?;

    let mut paths = tracked_paths(source_root, |_| true)?;
    paths.extend(tracked_working_tree_paths(source_root)?);
    sync_working_tree_paths(source_root, destination_root, &paths)
}

pub fn sync_working_tree_paths(
    source_root: &Path,
    destination_root: &Path,
    relative_paths: &BTreeSet<PathBuf>,
) -> Result<(), Error> {
    for path in relative_paths {
        sync_working_tree_path(source_root, destination_root, path)?;
    }

    Ok(())
}

pub fn index_path(root_path: &Path) -> Result<PathBuf, Error> {
    let repo = git2::Repository::open(root_path).context(GitSnafu {
        what: "open root repository",
    })?;
    let index = repo.index().context(GitSnafu {
        what: "open root repository index",
    })?;
    index
        .path()
        .map(Path::to_path_buf)
        .with_context(|| UpdateTreeSnafu::<String> {
            msg: "repository index is in-memory".into(),
        })
}

pub fn sync_materialized_paths(
    source_root: &Path,
    build_repo_path: &Path,
    relative_paths: &BTreeSet<PathBuf>,
) -> Result<(), Error> {
    if relative_paths.is_empty() {
        return Ok(());
    }

    let working_repo = git2::Repository::open(build_repo_path).context(GitSnafu {
        what: "open build repository",
    })?;
    let working_root = working_repo
        .workdir()
        .with_context(|| UpdateTreeSnafu::<String> {
            msg: "build repository workdir is unavailable".into(),
        })?;
    let mut index = working_repo.index().context(GitSnafu {
        what: "open build repository index",
    })?;

    for path in relative_paths {
        sync_dirty_path(source_root, working_root, &mut index, path)?;
    }

    index.write().context(GitSnafu {
        what: "write build repository index",
    })?;

    Ok(())
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

fn tracked_paths(
    root_path: &Path,
    include_path: impl Fn(&Path) -> bool,
) -> Result<BTreeSet<PathBuf>, Error> {
    let repo = git2::Repository::open(root_path).context(GitSnafu {
        what: "open root repository",
    })?;
    let head = repo.head().context(GitSnafu { what: "head" })?;
    let commit = head.peel_to_commit().context(GitSnafu {
        what: "peel head to commit",
    })?;
    let tree = commit.tree().context(GitSnafu { what: "head tree" })?;
    let mut paths = BTreeSet::new();
    let mut walk_error = None;

    let walk_result = tree.walk(git2::TreeWalkMode::PreOrder, |prefix, entry| {
        let Some(name) = entry.name() else {
            walk_error = Some(
                UpdateTreeSnafu {
                    msg: format!("tree entry without name in `{prefix}`"),
                }
                .build(),
            );
            return TreeWalkResult::Abort;
        };

        match entry.kind() {
            Some(ObjectType::Blob) => (),
            Some(ObjectType::Tree) => return TreeWalkResult::Ok,
            kind => {
                walk_error = Some(
                    UpdateTreeSnafu {
                        msg: format!("unknown blob type `{kind:?}` for `{}{name}`", prefix),
                    }
                    .build(),
                );
                return TreeWalkResult::Abort;
            }
        }

        let path = PathBuf::from(format!("{prefix}{name}"));
        if include_path(&path) {
            paths.insert(path);
        }

        TreeWalkResult::Ok
    });

    if let Some(error) = walk_error {
        return Err(error);
    }

    walk_result.context(GitSnafu {
        what: "traverse tree",
    })?;

    Ok(paths)
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
            remove_existing_path(&working_path).with_context(|_| IoSnafu {
                path: working_path.clone(),
            })?;
            remove_index_path(index, relative_path)?;
            Ok(())
        }
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            if let Some(parent) = working_path.parent() {
                std::fs::create_dir_all(parent).with_context(|_| IoSnafu {
                    path: parent.to_path_buf(),
                })?;
            }

            remove_existing_path(&working_path).with_context(|_| IoSnafu {
                path: working_path.clone(),
            })?;

            if metadata.file_type().is_symlink() {
                copy_symlink(&source_path, &working_path).with_context(|_| IoSnafu {
                    path: working_path.clone(),
                })?;
            } else {
                std::fs::copy(&source_path, &working_path).with_context(|_| IoSnafu {
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
            remove_existing_path(&working_path).with_context(|_| IoSnafu {
                path: working_path.clone(),
            })?;
            remove_index_path(index, relative_path)?;
            Ok(())
        }
        Err(error) => Err(IoSnafu { path: source_path }.into_error(error)),
    }
}

fn sync_working_tree_path(
    source_root: &Path,
    destination_root: &Path,
    relative_path: &Path,
) -> Result<(), Error> {
    let source_path = source_root.join(relative_path);
    let destination_path = destination_root.join(relative_path);

    match std::fs::symlink_metadata(&source_path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            remove_existing_path(&destination_path).with_context(|_| IoSnafu {
                path: destination_path.clone(),
            })
        }
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            if let Some(parent) = destination_path.parent() {
                std::fs::create_dir_all(parent).with_context(|_| IoSnafu {
                    path: parent.to_path_buf(),
                })?;
            }

            remove_existing_path(&destination_path).with_context(|_| IoSnafu {
                path: destination_path.clone(),
            })?;

            if metadata.file_type().is_symlink() {
                copy_symlink(&source_path, &destination_path).with_context(|_| IoSnafu {
                    path: destination_path.clone(),
                })?;
            } else {
                std::fs::copy(&source_path, &destination_path).with_context(|_| IoSnafu {
                    path: source_path.clone(),
                })?;
            }

            Ok(())
        }
        Ok(_) => DirtyUnsupportedPathSnafu {
            path: relative_path.to_path_buf(),
        }
        .fail(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_existing_path(&destination_path).with_context(|_| IoSnafu {
                path: destination_path,
            })
        }
        Err(error) => Err(IoSnafu { path: source_path }.into_error(error)),
    }
}

fn materialize_dirty_tree(
    source_root: &Path,
    working_repo: &git2::Repository,
    local_head: Oid,
) -> Result<Oid, Error> {
    let (dirty_paths, untracked_count) =
        collect_dirty_paths(source_root, |path| !is_generated_path(path))?;
    if untracked_count > 0 {
        info!("dirty mode ignores untracked files in the active content repo");
    }

    if dirty_paths.is_empty() {
        return Ok(local_head);
    }

    let working_root = working_repo
        .workdir()
        .with_context(|| UpdateTreeSnafu::<String> {
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
        repo_path: &Path,
        src_repo_use: RepositoryUse,
        source_materialization: SourceMaterialization,
    ) -> Result<Self, Error> {
        let root_path = absolute(root_path).with_context(|_| IoSnafu { path: root_path })?;
        if source_materialization == SourceMaterialization::Clean {
            check_dirty(&root_path)?;
        }
        let src_repo_url =
            Url::from_directory_path(&root_path)
                .ok()
                .with_context(|| PathUrlSnafu {
                    path: root_path.clone(),
                })?;

        debug!("source repository at `{src_repo_url}`");

        let working_repo = open_or_init(repo_path)?;

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
            "+HEAD:refs/build-eips/source-head",
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
    pub fn merge(&self) -> Result<(), Error> {
        merge_sibling_repositories(&self.working_repo, &self.src_repo_use, self.local_head)
    }

    pub fn fetch_upstream(self) -> Result<SourceWithUpstream, Error> {
        info!("fetching latest {} repository", self.src_repo_use.title);
        let latest_master = fetch(
            &self.working_repo,
            self.src_repo_use.location.repository.as_str(),
            "+master:refs/build-eips/upstream-head",
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

    pub fn merge(&self) -> Result<(), Error> {
        merge_sibling_repositories(&self.working_repo, &self.src_repo_use, self.local_head)
    }
}

fn check_ignored(working_repo: &git2::Repository, tree: &Tree) -> Result<(), Error> {
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

        match working_repo.is_path_ignored(&path) {
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

fn merge_sibling_repositories(
    working_repo: &git2::Repository,
    repo_use: &RepositoryUse,
    mut local_head: Oid,
) -> Result<(), Error> {
    for (index, (other_kind, other_repo)) in repo_use
        .other_repos
        .iter()
        .progress_ext("Merge Repos")
        .enumerate()
    {
        let local_commit = working_repo.find_commit(local_head).context(GitSnafu {
            what: "find local head commit",
        })?;
        let local_tree = local_commit.tree().context(GitSnafu {
            what: "getting local head tree",
        })?;
        info!("fetching {other_kind} repository");
        // Local sibling overrides should follow the checked-out repo HEAD instead of assuming `master`.
        let other_ref = format!("refs/build-eips/other-head-{index}");
        let other_refspec = if other_repo.scheme() == "file" {
            format!("+HEAD:{other_ref}")
        } else {
            format!("+master:{other_ref}")
        };
        let master_other = fetch(working_repo, other_repo.as_str(), &other_refspec)?;
        let other_tree = master_other.tree().context(GitSnafu {
            what: "getting other tree",
        })?;

        let mut tree_builder = TreeUpdateBuilder::new();
        let prefix = format!("{}/", CONTENT_DIR);
        let mut walk_error: Option<Error> = None;
        let walk_result = other_tree.walk(git2::TreeWalkMode::PreOrder, |a, b| {
            if !a.starts_with(&prefix) && (!a.is_empty() || b.name() != Some(CONTENT_DIR)) {
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

            if path == CONTENT_INDEX_PATH {
                debug!("skip sibling homepage `{path}`");
                return TreeWalkResult::Ok;
            }

            if let Err(e) = check_conflict(&local_tree, Path::new(&path), b) {
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
            .create_updated(working_repo, &local_tree)
            .context(GitSnafu { what: "build tree" })?;
        let merged_tree = working_repo.find_tree(merged_tree_oid).unwrap();

        check_ignored(working_repo, &merged_tree)?;

        let sig =
            Signature::now("eips-build", "eips-build@eips-build.invalid").context(GitSnafu {
                what: "commit signature",
            })?;
        let msg = format!("Merge {other_repo}");
        local_head = working_repo
            .commit(
                Some("HEAD"),
                &sig,
                &sig,
                &msg,
                &merged_tree,
                &[&local_commit, &master_other],
            )
            .context(GitSnafu { what: "committing" })?;

        working_repo
            .checkout_head(Some(CheckoutBuilder::default().force()))
            .context(GitSnafu {
                what: "checkout merged",
            })?;

        drop(merged_tree);
        drop(other_tree);
        drop(master_other);
        drop(local_tree);
        drop(local_commit);
        match working_repo.find_reference(&other_ref) {
            Ok(mut reference) => {
                if let Err(error) = reference.delete() {
                    debug!("unable to delete temporary sibling ref `{other_ref}`: {error}");
                }
            }
            Err(error) => {
                debug!("temporary sibling ref `{other_ref}` was not deleted: {error}");
            }
        }
    }

    Ok(())
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

