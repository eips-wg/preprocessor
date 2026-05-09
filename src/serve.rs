/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Dirty active-repo and local-theme serve synchronization.

use std::{
    collections::BTreeSet,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use log::{debug, info, warn};
use notify::{Event, RecursiveMode, Watcher};
use snafu::{Report, ResultExt, Whatever};

use crate::{git, layout::CONTENT_DIR, markdown};

#[derive(Debug)]
pub(crate) struct DirtyServeWatcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct ActiveRepoServeSync {
    source_root: PathBuf,
    build_repo_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalThemeServeSync {
    pub(crate) theme_source_root: PathBuf,
    pub(crate) mounted_theme_dir: PathBuf,
    pub(crate) theme_index_path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ServeSyncConfig {
    active_repo: Option<ActiveRepoServeSync>,
    local_theme: Option<LocalThemeServeSync>,
}

impl ServeSyncConfig {
    pub(crate) fn has_targets(&self) -> bool {
        self.active_repo.is_some() || self.local_theme.is_some()
    }
}

impl DirtyServeWatcher {
    pub(crate) fn start(sync_config: ServeSyncConfig) -> Result<Self, Whatever> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread =
            thread::spawn(move || dirty_serve_sync_loop(sync_config, stop_thread, ready_tx));

        match ready_rx
            .recv()
            .whatever_context("dirty serve watcher exited before initialization")?
        {
            Ok(()) => Ok(Self { stop, thread }),
            Err(message) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                snafu::whatever!("{message}");
            }
        }
    }

    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
    }
}

fn path_is_watched_source_path(root_path: &Path, path: &Path) -> bool {
    let Ok(relative_path) = path.strip_prefix(root_path) else {
        return false;
    };

    relative_path
        .components()
        .next()
        .map(|component| component.as_os_str() != OsStr::new(".git"))
        .unwrap_or(false)
}

fn event_has_watched_source_path(root_path: &Path, event: &Event) -> bool {
    event
        .paths
        .iter()
        .any(|path| path_is_watched_source_path(root_path, path))
}

fn index_lock_path(index_path: &Path) -> Option<PathBuf> {
    let file_name = index_path.file_name()?.to_string_lossy();
    Some(index_path.with_file_name(format!("{file_name}.lock")))
}

fn event_has_theme_index_path(index_path: &Path, event: &Event) -> bool {
    let lock_path = index_lock_path(index_path);
    event.paths.iter().any(|path| {
        path == index_path
            || lock_path
                .as_ref()
                .map(|lock_path| path == lock_path)
                .unwrap_or(false)
    })
}

fn sync_dirty_serve_state(
    source_root: &Path,
    build_repo_path: &Path,
    previous_dirty_paths: &mut BTreeSet<PathBuf>,
) -> Result<(), Whatever> {
    let current_dirty_paths: BTreeSet<_> = git::working_tree_paths(source_root)
        .whatever_context("unable to list tracked dirty paths for dirty serve")?
        .into_iter()
        .collect();

    let affected_paths: BTreeSet<_> = previous_dirty_paths
        .union(&current_dirty_paths)
        .cloned()
        .collect();

    if affected_paths.is_empty() {
        *previous_dirty_paths = current_dirty_paths;
        return Ok(());
    }

    git::sync_materialized_paths(source_root, build_repo_path, &affected_paths)
        .whatever_context("unable to synchronize tracked paths into the materialized repo")?;
    markdown::preprocess(&build_repo_path.join(CONTENT_DIR), None)
        .whatever_context("unable to preprocess synchronized markdown during dirty serve")?;

    info!(
        "synchronized {} tracked path(s) into the materialized repo for dirty serve",
        affected_paths.len()
    );

    *previous_dirty_paths = current_dirty_paths;
    Ok(())
}

fn capture_active_dirty_paths(source_root: &Path) -> Result<BTreeSet<PathBuf>, Whatever> {
    Ok(git::working_tree_paths(source_root)
        .whatever_context("unable to list tracked dirty paths for dirty serve")?
        .into_iter()
        .collect())
}

fn sync_theme_serve_state(
    theme_source_root: &Path,
    mounted_theme_dir: &Path,
    previous_dirty_paths: &mut BTreeSet<PathBuf>,
) -> Result<(), Whatever> {
    let current_dirty_paths: BTreeSet<_> = git::tracked_working_tree_paths(theme_source_root)
        .whatever_context("unable to list tracked dirty paths for local theme serve")?
        .into_iter()
        .collect();

    let affected_paths: BTreeSet<_> = previous_dirty_paths
        .union(&current_dirty_paths)
        .cloned()
        .collect();

    if affected_paths.is_empty() {
        *previous_dirty_paths = current_dirty_paths;
        return Ok(());
    }

    git::sync_working_tree_paths(theme_source_root, mounted_theme_dir, &affected_paths)
        .whatever_context("unable to synchronize tracked local theme paths")?;

    info!(
        "synchronized {} tracked path(s) into the mounted local theme for serve",
        affected_paths.len()
    );

    *previous_dirty_paths = current_dirty_paths;
    Ok(())
}

fn watch_theme_index(
    watcher: &mut notify::RecommendedWatcher,
    theme_index_path: &Path,
) -> Result<(), String> {
    let file_result = watcher.watch(theme_index_path, RecursiveMode::NonRecursive);
    let Some(parent) = theme_index_path.parent() else {
        return file_result.map_err(|file_error| {
            format!(
                "unable to watch local theme Git index `{}`: {file_error}",
                theme_index_path.to_string_lossy()
            )
        });
    };
    let parent_result = watcher.watch(parent, RecursiveMode::NonRecursive);

    match (file_result, parent_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(parent_error)) => {
            debug!(
                "unable to watch local theme Git index parent `{}`: {parent_error}",
                parent.to_string_lossy()
            );
            Ok(())
        }
        (Err(file_error), Ok(())) => {
            debug!(
                "using local theme Git index parent watch for `{}` after file watch failed: {file_error}",
                theme_index_path.to_string_lossy()
            );
            Ok(())
        }
        (Err(file_error), Err(parent_error)) => Err(format!(
            "unable to watch local theme Git index `{}`: {file_error}; fallback watch on `{}` also failed: {parent_error}",
            theme_index_path.to_string_lossy(),
            parent.to_string_lossy()
        )),
    }
}

fn dirty_serve_sync_loop(
    sync_config: ServeSyncConfig,
    stop: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<(), String>>,
) {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = match notify::recommended_watcher(move |result| {
        let _ = event_tx.send(result);
    }) {
        Ok(watcher) => watcher,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("unable to start dirty serve watcher: {error}")));
            return;
        }
    };

    if let Some(active_repo) = &sync_config.active_repo {
        if let Err(error) = watcher.watch(&active_repo.source_root, RecursiveMode::Recursive) {
            let _ = ready_tx.send(Err(format!(
                "unable to watch `{}` for dirty serve changes: {error}",
                active_repo.source_root.to_string_lossy()
            )));
            return;
        }
    }

    if let Some(local_theme) = &sync_config.local_theme {
        if let Err(error) = watcher.watch(&local_theme.theme_source_root, RecursiveMode::Recursive)
        {
            let _ = ready_tx.send(Err(format!(
                "unable to watch local theme `{}` for serve changes: {error}",
                local_theme.theme_source_root.to_string_lossy()
            )));
            return;
        }

        if let Err(message) = watch_theme_index(&mut watcher, &local_theme.theme_index_path) {
            let _ = ready_tx.send(Err(message));
            return;
        }
    }

    let mut previous_active_dirty_paths: BTreeSet<_> = match &sync_config.active_repo {
        Some(active_repo) => match capture_active_dirty_paths(&active_repo.source_root) {
            Ok(paths) => paths,
            Err(error) => {
                let _ = ready_tx.send(Err(format!(
                    "unable to capture initial dirty serve state: {}",
                    Report::from_error(error)
                )));
                return;
            }
        },
        None => BTreeSet::new(),
    };

    let mut previous_theme_dirty_paths: BTreeSet<_> = match &sync_config.local_theme {
        Some(local_theme) => {
            if let Err(error) = git::materialize_working_tree(
                &local_theme.theme_source_root,
                &local_theme.mounted_theme_dir,
            ) {
                let _ = ready_tx.send(Err(format!(
                    "unable to synchronize initial local theme state after watcher setup: {}",
                    Report::from_error(error)
                )));
                return;
            }

            match git::tracked_working_tree_paths(&local_theme.theme_source_root) {
                Ok(paths) => paths.into_iter().collect(),
                Err(error) => {
                    let _ = ready_tx.send(Err(format!(
                        "unable to capture initial local theme dirty state: {}",
                        Report::from_error(error)
                    )));
                    return;
                }
            }
        }
        None => BTreeSet::new(),
    };

    if let Some(active_repo) = &sync_config.active_repo {
        info!(
            "watching `{}` for dirty serve changes",
            active_repo.source_root.to_string_lossy()
        );
    }
    if let Some(local_theme) = &sync_config.local_theme {
        info!(
            "watching `{}` for local theme serve changes",
            local_theme.theme_source_root.to_string_lossy()
        );
    }
    let _ = ready_tx.send(Ok(()));

    while !stop.load(Ordering::Relaxed) {
        let first_event = match event_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };

        let Some(first_event) = first_event else {
            continue;
        };

        let mut saw_active_event = false;
        let mut saw_theme_event = false;

        match first_event {
            Ok(event) => {
                if let Some(active_repo) = &sync_config.active_repo {
                    saw_active_event |=
                        event_has_watched_source_path(&active_repo.source_root, &event);
                }
                if let Some(local_theme) = &sync_config.local_theme {
                    saw_theme_event |=
                        event_has_watched_source_path(&local_theme.theme_source_root, &event)
                            || event_has_theme_index_path(&local_theme.theme_index_path, &event);
                }
            }
            Err(error) => {
                warn!("filesystem watcher error: {error}");
            }
        }

        loop {
            match event_rx.recv_timeout(Duration::from_millis(75)) {
                Ok(Ok(event)) => {
                    if let Some(active_repo) = &sync_config.active_repo {
                        saw_active_event |=
                            event_has_watched_source_path(&active_repo.source_root, &event);
                    }
                    if let Some(local_theme) = &sync_config.local_theme {
                        saw_theme_event |=
                            event_has_watched_source_path(&local_theme.theme_source_root, &event)
                                || event_has_theme_index_path(
                                    &local_theme.theme_index_path,
                                    &event,
                                );
                    }
                }
                Ok(Err(error)) => warn!("filesystem watcher error: {error}"),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }

        if saw_active_event {
            if let Some(active_repo) = &sync_config.active_repo {
                if let Err(error) = sync_dirty_serve_state(
                    &active_repo.source_root,
                    &active_repo.build_repo_path,
                    &mut previous_active_dirty_paths,
                ) {
                    warn!(
                        "unable to synchronize dirty serve changes: {}",
                        Report::from_error(error)
                    );
                }
            }
        }

        if saw_theme_event {
            if let Some(local_theme) = &sync_config.local_theme {
                if let Err(error) = sync_theme_serve_state(
                    &local_theme.theme_source_root,
                    &local_theme.mounted_theme_dir,
                    &mut previous_theme_dirty_paths,
                ) {
                    warn!(
                        "unable to synchronize local theme serve changes: {}",
                        Report::from_error(error)
                    );
                }
            }
        }
    }
}

pub(crate) fn serve_sync_config(
    source_materialization: git::SourceMaterialization,
    source_root: &Path,
    repo_path: &Path,
    local_theme_sync: Option<LocalThemeServeSync>,
) -> ServeSyncConfig {
    ServeSyncConfig {
        active_repo: (source_materialization == git::SourceMaterialization::Dirty).then(|| {
            ActiveRepoServeSync {
                source_root: source_root.to_path_buf(),
                build_repo_path: repo_path.to_path_buf(),
            }
        }),
        local_theme: local_theme_sync,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use notify::{Event, EventKind};
    use tempfile::TempDir;

    use crate::git::SourceMaterialization;

    use super::{event_has_theme_index_path, serve_sync_config, LocalThemeServeSync};

    fn fake_theme_sync(root: &Path) -> LocalThemeServeSync {
        LocalThemeServeSync {
            theme_source_root: root.join("theme"),
            mounted_theme_dir: root.join("repo/themes/eips-theme"),
            theme_index_path: root.join("theme/.git/index"),
        }
    }

    #[test]
    fn local_theme_index_events_trigger_rescan() {
        let index_path = PathBuf::from("/workspace/theme/.git/index");
        let index_event = Event::new(EventKind::Any).add_path(index_path.clone());
        let lock_event =
            Event::new(EventKind::Any).add_path(PathBuf::from("/workspace/theme/.git/index.lock"));
        let unrelated_event =
            Event::new(EventKind::Any).add_path(PathBuf::from("/workspace/theme/.git/config"));

        assert!(event_has_theme_index_path(&index_path, &index_event));
        assert!(event_has_theme_index_path(&index_path, &lock_event));
        assert!(!event_has_theme_index_path(&index_path, &unrelated_event));
    }

    #[test]
    fn local_serve_syncs_theme_and_dirty_active_repo() {
        let temp = TempDir::new().unwrap();

        let sync_config = serve_sync_config(
            SourceMaterialization::Dirty,
            &temp.path().join("Core"),
            &temp.path().join(".local-build/Core/repo"),
            Some(fake_theme_sync(temp.path())),
        );

        assert!(sync_config.active_repo.is_some());
        assert!(sync_config.local_theme.is_some());
    }

    #[test]
    fn clean_local_serve_keeps_theme_sync_but_disables_active_repo_dirty_sync() {
        let temp = TempDir::new().unwrap();

        let sync_config = serve_sync_config(
            SourceMaterialization::Clean,
            &temp.path().join("Core"),
            &temp.path().join(".local-build/Core/repo"),
            Some(fake_theme_sync(temp.path())),
        );

        assert!(sync_config.active_repo.is_none());
        assert!(sync_config.local_theme.is_some());
    }
}
