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

use crate::{git, layout::CONTENT_DIR, markdown, proposal::OnlyRenderPlan};

#[derive(Debug)]
pub(crate) struct DirtyServeWatcher {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct ActiveRepoServeSync {
    source_root: PathBuf,
    build_repo_path: PathBuf,
    only_plan: Option<OnlyRenderPlan>,
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
    only_plan: Option<&OnlyRenderPlan>,
    previous_dirty_paths: &mut BTreeSet<PathBuf>,
) -> Result<(), Whatever> {
    let current_dirty_paths = filter_dirty_paths(
        git::working_tree_paths(source_root)
            .whatever_context("unable to list tracked dirty paths for dirty serve")?,
        only_plan,
    );

    let affected_paths = affected_dirty_paths(previous_dirty_paths, &current_dirty_paths);

    if affected_paths.is_empty() {
        *previous_dirty_paths = current_dirty_paths;
        return Ok(());
    }

    for path in selected_deleted_proposal_markdown_paths(source_root, &affected_paths, only_plan) {
        warn!(
            "selected proposal path `{}` was removed from the source repo; removing it from the targeted serve build input",
            path.to_string_lossy()
        );
    }

    git::sync_materialized_paths(source_root, build_repo_path, &affected_paths)
        .whatever_context("unable to synchronize tracked paths into the materialized repo")?;
    markdown::preprocess_paths(
        &build_repo_path.join(CONTENT_DIR),
        &affected_paths,
        only_plan,
    )
    .whatever_context("unable to preprocess synchronized markdown during dirty serve")?;

    info!(
        "synchronized {} tracked path(s) into the materialized repo for dirty serve",
        affected_paths.len()
    );

    *previous_dirty_paths = current_dirty_paths;
    Ok(())
}

fn filter_dirty_paths(
    dirty_paths: impl IntoIterator<Item = PathBuf>,
    only_plan: Option<&OnlyRenderPlan>,
) -> BTreeSet<PathBuf> {
    dirty_paths
        .into_iter()
        .filter(|path| {
            only_plan
                .map(|plan| plan.should_sync_dirty_path(path))
                .unwrap_or(true)
        })
        .collect()
}

fn affected_dirty_paths(
    previous_dirty_paths: &BTreeSet<PathBuf>,
    current_dirty_paths: &BTreeSet<PathBuf>,
) -> BTreeSet<PathBuf> {
    previous_dirty_paths
        .union(current_dirty_paths)
        .cloned()
        .collect()
}

fn selected_deleted_proposal_markdown_paths(
    source_root: &Path,
    affected_paths: &BTreeSet<PathBuf>,
    only_plan: Option<&OnlyRenderPlan>,
) -> Vec<PathBuf> {
    let Some(only_plan) = only_plan else {
        return Vec::new();
    };

    affected_paths
        .iter()
        .filter(|path| only_plan.is_selected_proposal_markdown_path(path))
        .filter(
            |path| match std::fs::symlink_metadata(source_root.join(path)) {
                Ok(_) => false,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    true
                }
                Err(_) => false,
            },
        )
        .cloned()
        .collect()
}

fn capture_active_dirty_paths(
    source_root: &Path,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<BTreeSet<PathBuf>, Whatever> {
    Ok(filter_dirty_paths(
        git::working_tree_paths(source_root)
            .whatever_context("unable to list tracked dirty paths for dirty serve")?,
        only_plan,
    ))
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
        Some(active_repo) => match capture_active_dirty_paths(
            &active_repo.source_root,
            active_repo.only_plan.as_ref(),
        ) {
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
                    active_repo.only_plan.as_ref(),
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
    only_plan: Option<OnlyRenderPlan>,
    local_theme_sync: Option<LocalThemeServeSync>,
) -> ServeSyncConfig {
    ServeSyncConfig {
        active_repo: (source_materialization == git::SourceMaterialization::Dirty).then(|| {
            ActiveRepoServeSync {
                source_root: source_root.to_path_buf(),
                build_repo_path: repo_path.to_path_buf(),
                only_plan,
            }
        }),
        local_theme: local_theme_sync,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        path::{Path, PathBuf},
    };

    use git2::{IndexAddOption, Repository, Signature};
    use notify::{Event, EventKind};
    use tempfile::TempDir;

    use crate::{
        git::{self, SourceMaterialization},
        proposal::{OnlyRenderPlan, ProposalNumber},
    };

    use super::{
        affected_dirty_paths, event_has_theme_index_path, filter_dirty_paths,
        selected_deleted_proposal_markdown_paths, serve_sync_config, sync_dirty_serve_state,
        sync_theme_serve_state, LocalThemeServeSync,
    };

    fn fake_theme_sync(root: &Path) -> LocalThemeServeSync {
        LocalThemeServeSync {
            theme_source_root: root.join("theme"),
            mounted_theme_dir: root.join("repo/themes/eips-theme"),
            theme_index_path: root.join("theme/.git/index"),
        }
    }

    fn number(value: u32) -> ProposalNumber {
        ProposalNumber::from_u32(value).unwrap()
    }

    fn write_file(root: &Path, relative: &str, contents: &str) {
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

    fn init_repo(root: &Path, files: &[(&str, &str)]) -> Repository {
        std::fs::create_dir_all(root).unwrap();
        let repo = Repository::init(root).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        for (relative, contents) in files {
            write_file(root, relative, contents);
        }
        commit_all(&repo, "initial");
        repo
    }

    fn proposal_markdown(value: u32, extra_preamble: &str, body: &str) -> String {
        format!("---\neip: {value}\ntitle: Proposal {value}\n{extra_preamble}---\n{body}\n")
    }

    fn only_plan(root: &Path) -> OnlyRenderPlan {
        let content = root.join("content");
        write_file(&content, "00555.md", &proposal_markdown(555, "", "Body"));
        write_file(&content, "00678.md", &proposal_markdown(678, "", "Body"));
        OnlyRenderPlan::build(&content, [number(555)].into_iter().collect()).unwrap()
    }

    fn paths(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn rendered_body(path: &Path) -> String {
        let contents = std::fs::read_to_string(path).unwrap();
        contents.split_once("\n+++\n").unwrap().1.to_owned()
    }

    fn dirty_sync_fixture() -> (TempDir, PathBuf, PathBuf, OnlyRenderPlan) {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let build = temp.path().join("build/repo");
        let selected = proposal_markdown(555, "requires: 678\n", "See [EIP-678](/00678.md).");
        let unselected = proposal_markdown(678, "", "Unselected.");
        init_repo(
            &source,
            &[
                ("content/00555.md", selected.as_str()),
                ("content/00555/assets/diagram.png", "selected image\n"),
                ("content/00678.md", unselected.as_str()),
                ("content/00678/assets/diagram.png", "unselected image\n"),
                (
                    "content/_index.md",
                    "---\ntitle: Home\n---\nSee [EIP-678](/00678.md).\n",
                ),
            ],
        );
        let plan =
            OnlyRenderPlan::build(&source.join("content"), [number(555)].into_iter().collect())
                .unwrap();
        init_repo(
            &build,
            &[
                ("content/00555.md", selected.as_str()),
                ("content/00555/assets/diagram.png", "selected image\n"),
                (
                    "content/_index.md",
                    "---\ntitle: Home\n---\nSee [EIP-678](/00678.md).\n",
                ),
            ],
        );
        (temp, source, build, plan)
    }

    #[test]
    fn local_theme_serve_syncs_tracked_edits_into_mounted_theme() {
        let temp = TempDir::new().unwrap();
        let theme_root = temp.path().join("workspace/theme");
        let mounted_theme_dir = temp.path().join("build/repo/themes/eips-theme");
        init_repo(
            &theme_root,
            &[
                ("config/zola.toml", "title = 'theme'\n"),
                ("templates/index.html", "committed local theme\n"),
            ],
        );
        git::materialize_working_tree(&theme_root, &mounted_theme_dir).unwrap();
        write_file(&theme_root, "templates/index.html", "dirty local theme\n");
        let mut previous_dirty_paths = BTreeSet::new();

        sync_theme_serve_state(&theme_root, &mounted_theme_dir, &mut previous_dirty_paths).unwrap();

        assert_eq!(
            std::fs::read_to_string(mounted_theme_dir.join("templates/index.html")).unwrap(),
            "dirty local theme\n"
        );
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
        let plan = only_plan(temp.path());

        let sync_config = serve_sync_config(
            SourceMaterialization::Dirty,
            &temp.path().join("Core"),
            &temp.path().join(".local-build/Core/repo"),
            Some(plan),
            Some(fake_theme_sync(temp.path())),
        );

        assert!(sync_config.active_repo.is_some());
        assert!(sync_config
            .active_repo
            .as_ref()
            .unwrap()
            .only_plan
            .is_some());
        assert!(sync_config.local_theme.is_some());
    }

    #[test]
    fn clean_local_serve_keeps_theme_sync_but_disables_active_repo_dirty_sync() {
        let temp = TempDir::new().unwrap();

        let sync_config = serve_sync_config(
            SourceMaterialization::Clean,
            &temp.path().join("Core"),
            &temp.path().join(".local-build/Core/repo"),
            None,
            Some(fake_theme_sync(temp.path())),
        );

        assert!(sync_config.active_repo.is_none());
        assert!(sync_config.local_theme.is_some());
    }

    #[test]
    fn only_dirty_path_filter_runs_before_union_and_keeps_selected_deletions() {
        let temp = TempDir::new().unwrap();
        let plan = only_plan(temp.path());
        let previous_raw = paths(&[
            "content/00555.md",
            "content/00678.md",
            "content/00678/assets/diagram.png",
        ]);
        let current_raw = paths(&["content/00555/assets/diagram.png", "content/00999.md"]);

        let previous_filtered = filter_dirty_paths(previous_raw, Some(&plan));
        let current_filtered = filter_dirty_paths(current_raw, Some(&plan));
        let affected = affected_dirty_paths(&previous_filtered, &current_filtered);

        assert_eq!(
            affected,
            paths(&["content/00555.md", "content/00555/assets/diagram.png"])
        );
    }

    #[test]
    fn selected_deleted_proposal_markdown_paths_reports_only_selected_markdown_deletions() {
        let (_temp, source, _build, plan) = dirty_sync_fixture();
        std::fs::remove_file(source.join("content/00555.md")).unwrap();
        std::fs::remove_file(source.join("content/00555/assets/diagram.png")).unwrap();
        std::fs::remove_file(source.join("content/_index.md")).unwrap();

        let affected_paths = paths(&[
            "content/00555.md",
            "content/00555/assets/diagram.png",
            "content/_index.md",
            "content/00678.md",
        ]);

        assert_eq!(
            selected_deleted_proposal_markdown_paths(&source, &affected_paths, Some(&plan)),
            vec![PathBuf::from("content/00555.md")]
        );
        assert!(
            selected_deleted_proposal_markdown_paths(&source, &affected_paths, None).is_empty()
        );
    }

    #[test]
    fn only_dirty_sync_does_not_reintroduce_unselected_markdown_or_assets() {
        let (_temp, source, build, plan) = dirty_sync_fixture();
        write_file(
            &source,
            "content/00678.md",
            &proposal_markdown(678, "", "Dirty unselected."),
        );
        write_file(
            &source,
            "content/00678/assets/diagram.png",
            "dirty unselected image\n",
        );
        let mut previous_dirty_paths = BTreeSet::new();

        sync_dirty_serve_state(&source, &build, Some(&plan), &mut previous_dirty_paths).unwrap();

        assert!(!build.join("content/00678.md").exists());
        assert!(!build.join("content/00678/assets/diagram.png").exists());
        assert!(previous_dirty_paths.is_empty());
    }

    #[test]
    fn only_dirty_sync_copies_selected_assets_without_markdown_preprocessing() {
        let (_temp, source, build, plan) = dirty_sync_fixture();
        write_file(
            &source,
            "content/00555/assets/diagram.png",
            "dirty selected image\n",
        );
        let mut previous_dirty_paths = BTreeSet::new();

        sync_dirty_serve_state(&source, &build, Some(&plan), &mut previous_dirty_paths).unwrap();

        assert_eq!(
            std::fs::read_to_string(build.join("content/00555/assets/diagram.png")).unwrap(),
            "dirty selected image\n"
        );
        assert!(previous_dirty_paths.contains(Path::new("content/00555/assets/diagram.png")));
    }

    #[test]
    fn only_dirty_sync_preprocesses_selected_and_retained_markdown_with_plan() {
        let (_temp, source, build, plan) = dirty_sync_fixture();
        write_file(
            &source,
            "content/00555.md",
            &proposal_markdown(555, "requires: 678\n", "Dirty [EIP-678](/00678.md)."),
        );
        write_file(
            &source,
            "content/_index.md",
            "---\ntitle: Home\n---\nDirty [EIP-678](/00678.md).\n",
        );
        let mut previous_dirty_paths = BTreeSet::new();

        sync_dirty_serve_state(&source, &build, Some(&plan), &mut previous_dirty_paths).unwrap();

        let selected = std::fs::read_to_string(build.join("content/00555.md")).unwrap();
        let index_body = rendered_body(&build.join("content/_index.md"));
        assert!(selected.contains("https://eips.ethereum.org/EIPS/eip-678"));
        assert!(index_body.contains("https://eips.ethereum.org/EIPS/eip-678"));
    }

    #[test]
    fn only_dirty_sync_propagates_selected_proposal_deletion() {
        let (_temp, source, build, plan) = dirty_sync_fixture();
        std::fs::remove_file(source.join("content/00555.md")).unwrap();
        let mut previous_dirty_paths = BTreeSet::new();

        sync_dirty_serve_state(&source, &build, Some(&plan), &mut previous_dirty_paths).unwrap();

        assert!(!build.join("content/00555.md").exists());
        assert!(previous_dirty_paths.contains(Path::new("content/00555.md")));
    }
}
