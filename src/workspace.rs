/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Local workspace setup and diagnostics.

use std::{
    fmt,
    fs::OpenOptions,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

use log::info;
use snafu::{Report, ResultExt, Whatever};
use url::Url;

use crate::{
    cli::Args,
    config::{self, ActiveRepo, LoadedWorkspaceConfig, Manifest},
    context::{load_workspace_command_context, resolve_input_path, root},
    git,
};

const PROPOSAL_TEMPLATE_URL: &str = "https://github.com/eips-wg/template.git";
const PLATFORM_PREPROCESSOR_URL: &str = "https://github.com/eips-wg/preprocessor.git";
const PLATFORM_EIPW_URL: &str = "https://github.com/ethereum/eipw.git";
const WORKSPACE_DOC_FILE: &str = "WORKSPACE.md";

#[derive(Debug, Clone, Copy)]
enum DoctorStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Default)]
struct DoctorReport {
    warnings: usize,
    failures: usize,
}

struct WorkspaceInitToolingRepositories<'a> {
    template: &'a Url,
    preprocessor: &'a Url,
    eipw: &'a Url,
}

impl fmt::Display for DoctorStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        };

        f.write_str(label)
    }
}

impl DoctorReport {
    fn record(&mut self, status: DoctorStatus, message: impl AsRef<str>) {
        match status {
            DoctorStatus::Ok => (),
            DoctorStatus::Warn => self.warnings += 1,
            DoctorStatus::Fail => self.failures += 1,
        }

        println!("[{status}] {}", message.as_ref());
    }
}

#[cfg(unix)]
fn is_command_candidate(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(all(not(unix), not(windows)))]
fn is_command_candidate(path: &Path) -> bool {
    path.is_file()
}

#[cfg(windows)]
fn is_command_candidate(path: &Path) -> bool {
    path.is_file()
}

fn command_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(not(windows))]
    let candidates = [command.to_owned()];

    #[cfg(windows)]
    {
        use std::ffi::OsString;

        let mut candidates = vec![command.to_owned()];
        let command = OsString::from(command);
        let path_exts = std::env::var_os("PATHEXT")
            .unwrap_or_default()
            .to_string_lossy()
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| format!("{}{}", command.to_string_lossy(), ext))
            .collect::<Vec<_>>();
        candidates.extend(path_exts);
        std::env::split_paths(&path).find_map(|entry| {
            candidates
                .iter()
                .map(|candidate| entry.join(candidate))
                .find(|candidate| is_command_candidate(candidate))
        })
    }

    #[cfg(not(windows))]
    {
        std::env::split_paths(&path).find_map(|entry| {
            candidates
                .iter()
                .map(|candidate| entry.join(candidate))
                .find(|candidate| is_command_candidate(candidate))
        })
    }
}

fn check_workspace_repo(
    report: &mut DoctorReport,
    workspace_root: &Path,
    name: &str,
) -> Option<git2::Repository> {
    let path = workspace_root.join(name);
    match git::open_usable_repository(&path) {
        Ok(repository) => {
            report.record(
                DoctorStatus::Ok,
                format!(
                    "found usable workspace repo `{}` at `{}`",
                    name,
                    path.to_string_lossy()
                ),
            );
            Some(repository)
        }
        Err(_) if !path.exists() => {
            report.record(
                DoctorStatus::Fail,
                format!(
                    "expected workspace repo `{}` at `{}`",
                    name,
                    path.to_string_lossy()
                ),
            );
            None
        }
        Err(error) => {
            report.record(
                DoctorStatus::Fail,
                format!(
                    "expected `{}` to be a usable git repository at `{}`: {}",
                    name,
                    path.to_string_lossy(),
                    Report::from_error(error)
                ),
            );
            None
        }
    }
}

fn check_sibling_manifest_id(
    report: &mut DoctorReport,
    sibling_path: &Path,
    expected_repo_id: &str,
) {
    let manifest_path = sibling_path.join(config::MANIFEST_FILE);
    match Manifest::load(&manifest_path) {
        Ok(manifest) if manifest.name == *expected_repo_id => report.record(
            DoctorStatus::Ok,
            format!("sibling `{expected_repo_id}` Build.toml name matches workspace key"),
        ),
        Ok(manifest) => report.record(
            DoctorStatus::Fail,
            format!(
                "sibling `{expected_repo_id}` Build.toml declares name `{}`",
                manifest.name
            ),
        ),
        Err(config::Error::Io { source, .. })
            if matches!(
                source.kind(),
                ErrorKind::NotFound | ErrorKind::NotADirectory
            ) => report.record(
            DoctorStatus::Warn,
            format!(
                "sibling `{expected_repo_id}` has no Build.toml yet; this is a transitional rollout warning"
            ),
        ),
        Err(error) => report.record(
            DoctorStatus::Fail,
            format!(
                "sibling `{expected_repo_id}` Build.toml could not be loaded: {}",
                Report::from_error(error)
            ),
        ),
    }
}

fn check_theme_zola_config(report: &mut DoctorReport, theme_path: &Path) {
    let zola_config = theme_path.join("config").join("zola.toml");
    match std::fs::read_to_string(&zola_config) {
        Ok(contents) => match toml::from_str::<toml::Value>(&contents) {
            Ok(_) => report.record(
                DoctorStatus::Ok,
                format!(
                    "workspace theme Zola config parses at `{}`",
                    zola_config.to_string_lossy()
                ),
            ),
            Err(error) => report.record(
                DoctorStatus::Fail,
                format!(
                    "workspace theme Zola config is invalid at `{}`: {error}",
                    zola_config.to_string_lossy()
                ),
            ),
        },
        Err(error) => report.record(
            DoctorStatus::Fail,
            format!(
                "workspace theme Zola config could not be read at `{}`: {error}",
                zola_config.to_string_lossy()
            ),
        ),
    }
}

fn check_theme_eipw_schema(report: &mut DoctorReport, theme_path: &Path) {
    match crate::lint::eipw_schema_status(theme_path) {
        Ok(()) => report.record(
            DoctorStatus::Ok,
            "workspace theme eipw config schema is compatible",
        ),
        Err(error) => report.record(
            DoctorStatus::Fail,
            format!("workspace theme eipw config is not usable: {error}"),
        ),
    }
}

fn normalized_repository_path(path: &str) -> String {
    path.trim_matches('/')
        .strip_suffix(".git")
        .unwrap_or(path.trim_matches('/'))
        .to_owned()
}

fn repository_identity(value: &str) -> Option<(String, String)> {
    if let Ok(url) = Url::parse(value) {
        return Some((
            url.host_str()?.to_ascii_lowercase(),
            normalized_repository_path(url.path()),
        ));
    }

    let (user_host, path) = value.split_once(':')?;
    let (_, host) = user_host.rsplit_once('@')?;
    Some((host.to_ascii_lowercase(), normalized_repository_path(path)))
}

fn theme_remote_matches_manifest(remote: &str, repository: &Url) -> bool {
    remote == repository.as_str()
        || matches!(
            (
                repository_identity(remote),
                repository_identity(repository.as_str())
            ),
            (Some(remote), Some(manifest)) if remote == manifest
        )
}

fn check_theme_remote(
    report: &mut DoctorReport,
    theme_repo: &git2::Repository,
    theme_repository: &Url,
) {
    match git::remote_urls(theme_repo) {
        Ok(remotes)
            if remotes
                .iter()
                .any(|remote| theme_remote_matches_manifest(remote, theme_repository)) =>
        {
            report.record(
                DoctorStatus::Ok,
                "workspace theme has a remote matching active Build.toml theme.repository",
            );
        }
        Ok(remotes) => report.record(
            DoctorStatus::Warn,
            format!(
                "workspace theme has no remote matching active Build.toml theme.repository `{theme_repository}` (checked {} configured remote(s))",
                remotes.len()
            ),
        ),
        Err(error) => report.record(
            DoctorStatus::Warn,
            format!("workspace theme remotes could not be inspected: {error}"),
        ),
    }
}

fn check_theme_pin(report: &mut DoctorReport, theme_repo: &git2::Repository, manifest_pin: &str) {
    let head = match git::head_commit_id(theme_repo) {
        Ok(head) => head,
        Err(error) => {
            report.record(
                DoctorStatus::Fail,
                format!("workspace theme HEAD could not be resolved: {error}"),
            );
            return;
        }
    };

    match git::resolve_commit_id(theme_repo, manifest_pin) {
        Ok(pin) if pin == head => report.record(
            DoctorStatus::Ok,
            format!("workspace theme HEAD matches active Build.toml pin `{manifest_pin}`"),
        ),
        Ok(pin) => report.record(
            DoctorStatus::Warn,
            format!(
                "workspace theme HEAD `{head}` does not match active Build.toml pin `{manifest_pin}` (resolved locally as `{pin}`)"
            ),
        ),
        Err(error) => report.record(
            DoctorStatus::Warn,
            format!(
                "active Build.toml theme pin `{manifest_pin}` could not be resolved locally; doctor did not fetch it: {error}"
            ),
        ),
    }
}

fn check_theme_readiness(
    report: &mut DoctorReport,
    workspace_root: &Path,
    theme_repository: &Url,
    theme_pin: &str,
) {
    let theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
    let Some(theme_repo) = check_workspace_repo(report, workspace_root, config::DEFAULT_THEME_DIR)
    else {
        return;
    };

    check_theme_zola_config(report, &theme_path);
    check_theme_eipw_schema(report, &theme_path);
    check_theme_remote(report, &theme_repo, theme_repository);
    check_theme_pin(report, &theme_repo, theme_pin);
}

fn doctor_root(args: &Args) -> Result<PathBuf, Whatever> {
    root(args)
}

fn check_tool(report: &mut DoctorReport, command: &str, why: &str) -> bool {
    match command_path(command) {
        Some(path) => {
            report.record(
                DoctorStatus::Ok,
                format!(
                    "found required tool `{}` at `{}`",
                    command,
                    path.to_string_lossy()
                ),
            );
            true
        }
        None => {
            report.record(
                DoctorStatus::Fail,
                format!("missing required tool `{}`: {}", command, why),
            );
            false
        }
    }
}

#[cfg(windows)]
fn check_default_windows_build_eips_path(report: &mut DoctorReport) {
    let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") else {
        return;
    };

    let install_dir = PathBuf::from(local_app_data).join("build-eips").join("bin");
    let build_eips_path = install_dir.join("build-eips.exe");

    if build_eips_path.is_file() {
        report.record(
            DoctorStatus::Warn,
            format!(
                "found build-eips at the default user-local install path `{}`, but `{}` is not on PATH",
                build_eips_path.to_string_lossy(),
                install_dir.to_string_lossy()
            ),
        );
    }
}

#[cfg(not(windows))]
fn check_default_windows_build_eips_path(_report: &mut DoctorReport) {}

#[cfg(not(windows))]
fn check_optional_download_tool(report: &mut DoctorReport) {
    let curl = command_path("curl");
    let wget = command_path("wget");

    record_optional_download_tool(report, curl.as_deref(), wget.as_deref());
}

#[cfg(not(windows))]
fn record_optional_download_tool(
    report: &mut DoctorReport,
    curl: Option<&Path>,
    wget: Option<&Path>,
) {
    match (curl, wget) {
        (Some(path), _) => report.record(
            DoctorStatus::Ok,
            format!(
                "found front-door download helper `curl` at `{}`",
                path.to_string_lossy()
            ),
        ),
        (None, Some(path)) => report.record(
            DoctorStatus::Ok,
            format!(
                "found front-door download helper `wget` at `{}`",
                path.to_string_lossy()
            ),
        ),
        (None, None) => report.record(
            DoctorStatus::Warn,
            "missing both `curl` and `wget`; `scripts/dev-setup` will not be able to download a release binary",
        ),
    }
}

#[cfg(not(windows))]
fn check_front_door_archive_tool(report: &mut DoctorReport) {
    let tar = command_path("tar");
    record_front_door_archive_tool(report, tar.as_deref());
}

#[cfg(not(windows))]
fn record_front_door_archive_tool(report: &mut DoctorReport, tar: Option<&Path>) {
    match tar {
        Some(path) => report.record(
            DoctorStatus::Ok,
            format!(
                "found front-door archive tool `tar` at `{}`",
                path.to_string_lossy()
            ),
        ),
        None => report.record(
            DoctorStatus::Warn,
            "missing `tar`; `scripts/dev-setup` will not be able to unpack the release binary",
        ),
    }
}

#[cfg(not(windows))]
fn check_front_door_setup_tools(report: &mut DoctorReport) {
    check_optional_download_tool(report);
    check_front_door_archive_tool(report);
}

#[cfg(windows)]
fn check_front_door_setup_tools(_report: &mut DoctorReport) {}

fn collect_doctor_report(args: &Args, check_tools: bool) -> Result<DoctorReport, Whatever> {
    let context = load_workspace_command_context(args)?;
    let mut report = DoctorReport::default();
    let (root_path, active_repo) = match doctor_root(args) {
        Ok(root_path) => match ActiveRepo::load(&root_path) {
            Ok(active_repo) => {
                report.record(
                    DoctorStatus::Ok,
                    format!("loaded active repo Build.toml for `{}`", active_repo.title),
                );
                report.record(
                    DoctorStatus::Ok,
                    format!(
                        "Build.toml parses at `{}`",
                        root_path.join(config::MANIFEST_FILE).to_string_lossy()
                    ),
                );
                (Some(root_path), Some(active_repo))
            }
            Err(error) => {
                report.record(
                    DoctorStatus::Fail,
                    format!("active repo Build.toml could not be loaded: {error}"),
                );
                (Some(root_path), None)
            }
        },
        Err(error) => {
            report.record(
                DoctorStatus::Fail,
                format!(
                    "active repo root could not be resolved: {}",
                    Report::from_error(error)
                ),
            );
            (None, None)
        }
    };

    match context.config_path.as_ref() {
        Some(path) => report.record(
            DoctorStatus::Ok,
            format!(
                "found workspace config candidate `{}`",
                path.to_string_lossy()
            ),
        ),
        None => report.record(
            DoctorStatus::Fail,
            format!(
                "could not find `{}` while searching upward from `{}`",
                config::LOCAL_CONFIG_FILE,
                context.search_from.to_string_lossy()
            ),
        ),
    }

    let parsed_config = context
        .config_path
        .as_deref()
        .map(LoadedWorkspaceConfig::from_path)
        .transpose();

    match parsed_config {
        Ok(Some(config)) => {
            report.record(
                DoctorStatus::Ok,
                format!(
                    "workspace config parses at `{}`",
                    config.config_path().to_string_lossy()
                ),
            );

            let workspace_root = config.workspace_root();
            if workspace_root.is_dir() {
                report.record(
                    DoctorStatus::Ok,
                    format!(
                        "workspace root exists at `{}`",
                        workspace_root.to_string_lossy()
                    ),
                );
            } else {
                report.record(
                    DoctorStatus::Fail,
                    format!(
                        "workspace root is missing at `{}`",
                        workspace_root.to_string_lossy()
                    ),
                );
            }

            if let (Some(root_path), Some(active_repo)) = (root_path.as_ref(), active_repo.as_ref())
            {
                let expected_root = workspace_root.join(&active_repo.title);
                if root_path == &expected_root {
                    report.record(
                        DoctorStatus::Ok,
                        format!(
                            "active repo `{}` is checked out at `{}`",
                            &active_repo.title,
                            expected_root.to_string_lossy()
                        ),
                    );
                } else {
                    report.record(
                        DoctorStatus::Fail,
                        format!(
                            "active repo `{}` should be checked out at `{}`, found `{}`",
                            &active_repo.title,
                            expected_root.to_string_lossy(),
                            root_path.to_string_lossy()
                        ),
                    );
                }

                check_workspace_repo(&mut report, workspace_root, &active_repo.title);
                for sibling_id in &active_repo.sibling_ids {
                    let sibling_path = workspace_root.join(sibling_id);
                    if check_workspace_repo(&mut report, workspace_root, sibling_id).is_some() {
                        check_sibling_manifest_id(&mut report, &sibling_path, sibling_id);
                    }
                }

                check_theme_readiness(
                    &mut report,
                    workspace_root,
                    &active_repo.theme.repository,
                    &active_repo.theme.commit,
                );
            } else {
                report.record(
                    DoctorStatus::Warn,
                    "workspace repo layout checks were skipped because active Build.toml was unavailable",
                );
            }
        }
        Err(error) => {
            report.record(
                DoctorStatus::Fail,
                format!(
                    "workspace config could not be parsed: {}",
                    Report::from_error(error)
                ),
            );
        }
        Ok(None) => (),
    }

    if check_tools {
        if !check_tool(
            &mut report,
            "build-eips",
            "workspace bootstrap and build-eips commands expect `build-eips` on PATH",
        ) {
            check_default_windows_build_eips_path(&mut report);
        }
        check_tool(
            &mut report,
            "git",
            "workspace bootstrap and build-eips commands expect git to be available",
        );
        check_tool(
            &mut report,
            "zola",
            "build, check, and serve commands need a working zola binary",
        );
        check_front_door_setup_tools(&mut report);
    }

    Ok(report)
}

pub(crate) fn doctor_workspace(args: &Args) -> Result<(), Whatever> {
    let report = collect_doctor_report(args, true)?;

    if report.failures > 0 {
        snafu::whatever!("doctor found {} failing check(s)", report.failures);
    }

    Ok(())
}

pub(crate) fn init_workspace(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
    platform_dev: bool,
) -> Result<(), Whatever> {
    let template_repository = Url::parse(PROPOSAL_TEMPLATE_URL)
        .whatever_context("invalid proposal template repository URL")?;
    let preprocessor_repository = Url::parse(PLATFORM_PREPROCESSOR_URL)
        .whatever_context("invalid platform preprocessor repository URL")?;
    let eipw_repository =
        Url::parse(PLATFORM_EIPW_URL).whatever_context("invalid platform eipw repository URL")?;
    let repositories = WorkspaceInitToolingRepositories {
        template: &template_repository,
        preprocessor: &preprocessor_repository,
        eipw: &eipw_repository,
    };

    init_workspace_with_repositories(
        args,
        workspace_root,
        include_template,
        platform_dev,
        &repositories,
    )
}

fn init_workspace_with_repositories(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
    platform_dev: bool,
    repositories: &WorkspaceInitToolingRepositories<'_>,
) -> Result<(), Whatever> {
    let root_path = root(args)
        .whatever_context("workspace init requires an active Build.toml repository root")?;
    let active_repo = ActiveRepo::load(&root_path)
        .whatever_context("unable to load active repository Build.toml")?;
    let workspace_root = resolve_input_path(&workspace_root)?;
    std::fs::create_dir_all(&workspace_root)
        .whatever_context("unable to create workspace root directory")?;
    let workspace_root = workspace_root
        .canonicalize()
        .whatever_context("unable to canonicalize workspace root directory")?;

    let repository_use = active_repo.repository_use;
    let theme = active_repo.theme;

    for (sibling_id, sibling_url) in repository_use.other_repos {
        let sibling_path = workspace_root.join(&sibling_id);
        git::clone_missing_repo(sibling_url.as_str(), &sibling_path).with_whatever_context(|_| {
            format!(
                "unable to prepare workspace sibling repo `{sibling_id}` at `{}`; destination must be missing or a usable git repository",
                sibling_path.to_string_lossy()
            )
        })?;
    }

    let theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
    if git::clone_missing_repo(theme.repository.as_str(), &theme_path)
        .with_whatever_context(|_| {
            format!(
                "unable to prepare workspace theme repo at `{}`; destination must be missing or a usable git repository",
                theme_path.to_string_lossy()
            )
        })?
        == git::CloneOutcome::Fresh
    {
        git::checkout_fresh_clone_at_commit(&theme_path, theme.repository.as_str(), &theme.commit)
            .with_whatever_context(|_| {
                format!(
                    "unable to fetch or check out active Build.toml theme commit `{}`",
                    theme.commit
                )
            })?;
    }

    if include_template {
        let template_path = workspace_root.join("template");
        git::clone_missing_repo(repositories.template.as_str(), &template_path)
            .with_whatever_context(|_| {
                format!(
                    "unable to prepare workspace template repo at `{}`; destination must be missing or a usable git repository",
                    template_path.to_string_lossy()
                )
            })?;
    }

    if platform_dev {
        let preprocessor_path = workspace_root.join("preprocessor");
        git::clone_missing_repo(repositories.preprocessor.as_str(), &preprocessor_path)
            .with_whatever_context(|_| {
                format!(
                    "unable to prepare workspace preprocessor repo at `{}`; destination must be missing or a usable git repository",
                    preprocessor_path.to_string_lossy()
                )
            })?;
        let eipw_path = workspace_root.join("eipw");
        git::clone_missing_repo(repositories.eipw.as_str(), &eipw_path).with_whatever_context(|_| {
            format!(
                "unable to prepare workspace eipw repo at `{}`; destination must be missing or a usable git repository",
                eipw_path.to_string_lossy()
            )
        })?;
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
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use eipw_lint::config::DefaultOptions;
    use git2::{IndexAddOption, Repository, Signature};
    use tempfile::TempDir;
    use url::Url;

    use crate::{
        cli::{Args, Operation},
        config::{self, LoadedWorkspaceConfig},
    };

    use super::{
        collect_doctor_report, doctor_root, init_workspace_with_repositories, workspace_doc_text,
        WorkspaceInitToolingRepositories, PLATFORM_EIPW_URL, PLATFORM_PREPROCESSOR_URL,
        PROPOSAL_TEMPLATE_URL, WORKSPACE_DOC_FILE,
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

    fn build_manifest_text_with_theme(
        repo_name: &str,
        repository: &Url,
        siblings: &[(&str, Url)],
        theme_repository: &Url,
        theme_commit: &str,
    ) -> String {
        let mut manifest = format!(
            r#"
name = "{repo_name}"

[locations.{repo_name}]
repository = "{repository}"
base-url = "https://example.test/{repo_name}/"

[theme]
repository = "{theme_repository}"
commit = "{theme_commit}"
"#
        );

        for (sibling_id, sibling_repository) in siblings {
            manifest.push_str(&format!(
                r#"
[locations.{sibling_id}]
repository = "{sibling_repository}"
base-url = "https://example.test/{sibling_id}/"
"#
            ));
        }

        manifest
    }

    fn build_manifest_text(repo_name: &str, repository: &Url, siblings: &[(&str, Url)]) -> String {
        let theme_repository = Url::parse("https://example.test/theme.git").unwrap();
        build_manifest_text_with_theme(
            repo_name,
            repository,
            siblings,
            &theme_repository,
            "test-theme-commit",
        )
    }

    fn write_build_manifest_file_with_theme(
        path: &Path,
        repo_name: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
        theme_repository: &Url,
        theme_commit: &str,
    ) {
        write_file(
            path,
            config::MANIFEST_FILE,
            &build_manifest_text_with_theme(
                repo_name,
                upstream,
                siblings,
                theme_repository,
                theme_commit,
            ),
        );
    }

    fn write_build_manifest_file(
        path: &Path,
        repo_name: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
    ) {
        write_file(
            path,
            config::MANIFEST_FILE,
            &build_manifest_text(repo_name, upstream, siblings),
        );
    }

    fn write_manifest_repo_with_theme(
        path: &Path,
        repo_name: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
        theme_repository: &Url,
        theme_commit: &str,
    ) -> Repository {
        let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
        write_build_manifest_file_with_theme(
            path,
            repo_name,
            upstream,
            siblings,
            theme_repository,
            theme_commit,
        );
        commit_all(&repo, "add manifest");
        repo
    }

    fn write_manifest_repo(
        path: &Path,
        repo_name: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
    ) -> Repository {
        let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
        write_build_manifest_file(path, repo_name, upstream, siblings);
        commit_all(&repo, "add manifest");
        repo
    }

    fn repository_head(repo: &Repository) -> String {
        repo.head().unwrap().target().unwrap().to_string()
    }

    fn init_workspace_source_repo(remotes_root: &Path, name: &str) -> Url {
        let path = remotes_root.join(name);
        init_repo(&path, &[("README.md", "init test repo\n")]);
        file_url(&path)
    }

    fn compatible_eipw_config() -> String {
        format!(
            "schema-version = \"{}\"\n\n[fetch]\nproposal-format = \"{{:05}}\"\n",
            DefaultOptions::<String>::schema_version()
        )
    }

    fn init_workspace_theme_repo(remotes_root: &Path) -> (Url, String) {
        let path = remotes_root.join("theme");
        let eipw_config = compatible_eipw_config();
        let repo = init_repo(
            &path,
            &[
                ("README.md", "theme\n"),
                ("config/zola.toml", "title = \"theme\"\n"),
                ("config/eipw.toml", eipw_config.as_str()),
            ],
        );
        (file_url(&path), repository_head(&repo))
    }

    fn workspace_init_test_repository_urls(remotes_root: &Path) -> (Url, String, Url, Url, Url) {
        let (theme_url, theme_commit) = init_workspace_theme_repo(remotes_root);
        (
            theme_url,
            theme_commit,
            init_workspace_source_repo(remotes_root, "template"),
            init_workspace_source_repo(remotes_root, "preprocessor"),
            init_workspace_source_repo(remotes_root, "eipw"),
        )
    }

    fn tooling_repositories<'a>(
        template: &'a Url,
        preprocessor: &'a Url,
        eipw: &'a Url,
    ) -> WorkspaceInitToolingRepositories<'a> {
        WorkspaceInitToolingRepositories {
            template,
            preprocessor,
            eipw,
        }
    }

    fn run_workspace_init(
        active_path: &Path,
        workspace_root: &Path,
        repositories: &WorkspaceInitToolingRepositories<'_>,
    ) -> Result<(), snafu::Whatever> {
        let args = parse_args(&[
            "build-eips",
            "-C",
            active_path.to_str().unwrap(),
            "init",
            workspace_root.to_str().unwrap(),
        ]);

        init_workspace_with_repositories(
            &args,
            workspace_root.to_path_buf(),
            false,
            false,
            repositories,
        )
    }

    fn create_hidden_theme_commit(repo: &Repository, root: &Path) -> String {
        let default_head = repo.head().unwrap().target().unwrap();
        write_file(root, "PINNED.md", "pinned theme commit\n");
        commit_all(repo, "pinned theme commit");
        let pinned_head = repo.head().unwrap().target().unwrap();
        repo.reference(
            "refs/build-eips/theme-pin-source",
            pinned_head,
            true,
            "test-only pinned theme source",
        )
        .unwrap();
        repo.find_reference("refs/heads/master")
            .unwrap()
            .set_target(default_head, "restore default theme branch")
            .unwrap();
        pinned_head.to_string()
    }

    fn run_workspace_init_for_docs(
        existing_doc: Option<&str>,
        existing_config: Option<&str>,
    ) -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitToolingRepositories {
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        if let Some(contents) = existing_doc {
            write_file(&workspace_root, WORKSPACE_DOC_FILE, contents);
        }
        if let Some(contents) = existing_config {
            write_file(&workspace_root, config::LOCAL_CONFIG_FILE, contents);
        }

        let init_args = parse_args(&[
            "build-eips",
            "-C",
            active_path.to_str().unwrap(),
            "init",
            workspace_root.to_str().unwrap(),
        ]);

        init_workspace_with_repositories(
            &init_args,
            workspace_root.clone(),
            false,
            false,
            &repositories,
        )
        .unwrap();

        (temp, workspace_root)
    }

    #[test]
    fn tooling_repository_urls_remain_bootstrap_metadata() {
        for url in [
            PROPOSAL_TEMPLATE_URL,
            PLATFORM_PREPROCESSOR_URL,
            PLATFORM_EIPW_URL,
        ] {
            assert!(
                Url::parse(url).is_ok(),
                "expected valid tooling URL `{url}`"
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn front_door_setup_tool_records_posix_helper_warnings() {
        let mut report = super::DoctorReport::default();

        super::record_optional_download_tool(&mut report, None, None);
        super::record_front_door_archive_tool(&mut report, None);

        assert_eq!(report.warnings, 2);
        assert_eq!(report.failures, 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn front_door_setup_tool_accepts_posix_helpers() {
        let mut report = super::DoctorReport::default();
        let tool_path = Path::new("/usr/bin/tool");

        super::record_optional_download_tool(&mut report, Some(tool_path), None);
        super::record_front_door_archive_tool(&mut report, Some(tool_path));

        assert_eq!(report.warnings, 0);
        assert_eq!(report.failures, 0);
    }

    #[cfg(windows)]
    #[test]
    fn front_door_setup_tools_skip_posix_helpers_on_windows() {
        let mut report = super::DoctorReport::default();

        super::check_front_door_setup_tools(&mut report);

        assert_eq!(report.warnings, 0);
        assert_eq!(report.failures, 0);
    }

    #[test]
    fn workspace_doc_text_mentions_required_workspace_reference_content() {
        let text = workspace_doc_text();

        for expected in [
            "Build.toml",
            ".build-eips.toml",
            ".local-build",
            "workspace/theme",
            "build-eips init",
            "build-eips doctor",
            "build-eips build",
            "build-eips serve",
            "build-eips preview",
            "build-eips editorial check",
            "[render]",
            "only = [",
            "--only",
            "--clean",
            "--remote-siblings",
            "--base-url",
        ] {
            assert!(
                text.contains(expected),
                "workspace document text should contain `{expected}`"
            );
        }

        for removed in [
            ".build-eips.repo.toml",
            "--staging",
            "--production",
            "parity",
            "../WORKSPACE.md",
            "repo_id",
        ] {
            assert!(
                !text.contains(removed),
                "workspace document text should not contain stale `{removed}`"
            );
        }

        assert!(text.ends_with('\n'));
    }

    #[test]
    fn workspace_init_writes_workspace_doc() {
        let (_temp, workspace_root) = run_workspace_init_for_docs(None, None);

        let doc = std::fs::read_to_string(workspace_root.join(WORKSPACE_DOC_FILE)).unwrap();
        assert_eq!(doc, workspace_doc_text());
    }

    #[test]
    fn workspace_init_overwrites_existing_workspace_doc() {
        let existing_doc = "Old workspace docs\n";
        let (_temp, workspace_root) = run_workspace_init_for_docs(Some(existing_doc), None);

        let doc = std::fs::read_to_string(workspace_root.join(WORKSPACE_DOC_FILE)).unwrap();
        assert_ne!(doc, existing_doc);
        assert_eq!(doc, workspace_doc_text());
    }

    #[test]
    fn workspace_init_leaves_existing_config_without_render_unchanged() {
        let existing_config = "[server]\nhost = \"127.0.0.1\"\nport = 1111\n";
        let (_temp, workspace_root) = run_workspace_init_for_docs(None, Some(existing_config));

        assert_eq!(
            std::fs::read_to_string(workspace_root.join(config::LOCAL_CONFIG_FILE)).unwrap(),
            existing_config
        );
    }

    #[test]
    fn fresh_workspace_init_clones_manifest_theme_at_pinned_commit() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        let theme = Repository::open(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        assert_eq!(repository_head(&theme), theme_commit);
        assert_eq!(
            theme.find_remote("origin").unwrap().url(),
            Some(theme_url.as_str())
        );
    }

    #[test]
    fn fresh_theme_clone_fetches_missing_manifest_pin_before_checkout() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let theme_source_path = remotes_root.join("theme");
        let theme_source = init_repo(&theme_source_path, &[("README.md", "theme\n")]);
        let pinned_commit = create_hidden_theme_commit(&theme_source, &theme_source_path);
        let theme_url = file_url(&theme_source_path);
        let template_url = init_workspace_source_repo(&remotes_root, "template");
        let preprocessor_url = init_workspace_source_repo(&remotes_root, "preprocessor");
        let eipw_url = init_workspace_source_repo(&remotes_root, "eipw");
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &pinned_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        let theme = Repository::open(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        assert_eq!(repository_head(&theme), pinned_commit);
        assert_eq!(
            std::fs::read_to_string(
                workspace_root
                    .join(config::DEFAULT_THEME_DIR)
                    .join("PINNED.md")
            )
            .unwrap(),
            "pinned theme commit\n"
        );
    }

    #[test]
    fn fresh_theme_clone_reports_manifest_pin_fetch_or_checkout_failure() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, _theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        let missing_commit = "0000000000000000000000000000000000000000";
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            missing_commit,
        );

        let error = run_workspace_init(&active_path, &workspace_root, &repositories)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unable to fetch or check out active Build.toml theme commit"));
        assert!(error.contains(missing_commit));
    }

    #[test]
    fn existing_workspace_theme_repo_is_left_untouched_when_manifest_pin_differs() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let existing_theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
        let existing_theme = init_repo(&existing_theme_path, &[("README.md", "local theme\n")]);
        let existing_head = repository_head(&existing_theme);
        assert_ne!(existing_head, theme_commit);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        let theme = Repository::open(&existing_theme_path).unwrap();
        assert_eq!(repository_head(&theme), existing_head);
        assert_eq!(
            std::fs::read_to_string(existing_theme_path.join("README.md")).unwrap(),
            "local theme\n"
        );
    }

    #[test]
    fn existing_non_git_workspace_theme_path_fails_without_overwriting_files() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
        write_file(&theme_path, "keep.txt", "do not overwrite\n");
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        let error = run_workspace_init(&active_path, &workspace_root, &repositories).unwrap_err();

        assert!(error
            .to_string()
            .contains("unable to prepare workspace theme repo"));
        assert!(error
            .to_string()
            .contains("destination must be missing or a usable git repository"));
        assert_eq!(
            std::fs::read_to_string(theme_path.join("keep.txt")).unwrap(),
            "do not overwrite\n"
        );
    }

    #[test]
    fn existing_partial_workspace_theme_repo_fails_without_reporting_success() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let theme_path = workspace_root.join(config::DEFAULT_THEME_DIR);
        std::fs::create_dir_all(&theme_path).unwrap();
        Repository::init(&theme_path).unwrap();
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        let error = run_workspace_init(&active_path, &workspace_root, &repositories).unwrap_err();

        assert!(error
            .to_string()
            .contains("unable to prepare workspace theme repo"));
        assert!(error
            .to_string()
            .contains("destination must be missing or a usable git repository"));
        assert!(theme_path.join(".git").exists());
    }

    #[test]
    fn workspace_init_clones_missing_siblings_from_manifest_locations() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let sibling_source_path = remotes_root.join("ERCs");
        init_repo(
            &sibling_source_path,
            &[("content/00002.md", "remote sibling\n")],
        );
        let sibling_url = file_url(&sibling_source_path);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[("ERCs", sibling_url)],
            &theme_url,
            &theme_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        assert_eq!(
            std::fs::read_to_string(workspace_root.join("ERCs/content/00002.md")).unwrap(),
            "remote sibling\n"
        );
    }

    #[test]
    fn existing_workspace_sibling_repo_is_left_untouched() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let sibling_source_path = remotes_root.join("ERCs");
        init_repo(
            &sibling_source_path,
            &[("content/00002.md", "remote sibling\n")],
        );
        let sibling_url = file_url(&sibling_source_path);
        let sibling_path = workspace_root.join("ERCs");
        let sibling = init_repo(&sibling_path, &[("content/00002.md", "local sibling\n")]);
        let sibling_head = repository_head(&sibling);
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[("ERCs", sibling_url)],
            &theme_url,
            &theme_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        let sibling = Repository::open(&sibling_path).unwrap();
        assert_eq!(repository_head(&sibling), sibling_head);
        assert_eq!(
            std::fs::read_to_string(sibling_path.join("content/00002.md")).unwrap(),
            "local sibling\n"
        );
    }

    #[test]
    fn existing_non_git_workspace_sibling_path_fails_without_overwriting_files() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let sibling_source_path = remotes_root.join("ERCs");
        init_repo(
            &sibling_source_path,
            &[("content/00002.md", "remote sibling\n")],
        );
        let sibling_url = file_url(&sibling_source_path);
        let sibling_path = workspace_root.join("ERCs");
        write_file(&sibling_path, "keep.txt", "do not overwrite\n");
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[("ERCs", sibling_url)],
            &theme_url,
            &theme_commit,
        );

        let error = run_workspace_init(&active_path, &workspace_root, &repositories).unwrap_err();

        assert!(error
            .to_string()
            .contains("unable to prepare workspace sibling repo `ERCs`"));
        assert!(error
            .to_string()
            .contains("destination must be missing or a usable git repository"));
        assert_eq!(
            std::fs::read_to_string(sibling_path.join("keep.txt")).unwrap(),
            "do not overwrite\n"
        );
    }

    #[test]
    fn workspace_init_accepts_active_checkout_basename_different_from_manifest_title() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let active_path = temp.path().join("source/checkout-copy");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Managed",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );

        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        let workspace_config =
            LoadedWorkspaceConfig::from_path(&workspace_root.join(config::LOCAL_CONFIG_FILE))
                .unwrap();
        assert_eq!(
            workspace_config.workspace_build_root("Managed"),
            workspace_root
                .join(config::DEFAULT_BUILD_ROOT_BASE)
                .join("Managed")
        );
        assert!(Repository::open(workspace_root.join(config::DEFAULT_THEME_DIR)).is_ok());
    }

    #[test]
    fn workspace_init_requires_active_build_toml() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (_theme_url, _theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = tooling_repositories(&template_url, &preprocessor_url, &eipw_url);
        let active_path = temp.path().join("source/no-manifest");
        init_repo(&active_path, &[("content/00001.md", "active\n")]);

        let error = run_workspace_init(&active_path, &workspace_root, &repositories)
            .unwrap_err()
            .to_string();

        assert!(error.contains("workspace init requires an active Build.toml repository root"));
    }

    fn assert_workspace_init_optional_repos(
        workspace_root: &Path,
        expect_template: bool,
        expect_platform_dev: bool,
    ) {
        assert!(Repository::open(workspace_root.join(config::DEFAULT_THEME_DIR)).is_ok());
        assert_eq!(
            Repository::open(workspace_root.join("template")).is_ok(),
            expect_template
        );
        assert_eq!(
            Repository::open(workspace_root.join("preprocessor")).is_ok(),
            expect_platform_dev
        );
        assert_eq!(
            Repository::open(workspace_root.join("eipw")).is_ok(),
            expect_platform_dev
        );
    }

    struct DoctorWorkspace {
        _temp: TempDir,
        workspace_root: PathBuf,
        active_path: PathBuf,
        theme_source_path: PathBuf,
        theme_url: Url,
        theme_commit: String,
    }

    fn initialized_doctor_workspace(sibling_ids: &[&str]) -> DoctorWorkspace {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitToolingRepositories {
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };
        let sibling_repositories = sibling_ids
            .iter()
            .map(|sibling_id| {
                let sibling_path = remotes_root.join(sibling_id);
                let sibling_url = file_url(&sibling_path);
                write_manifest_repo(&sibling_path, sibling_id, &sibling_url, &[]);
                ((*sibling_id).to_owned(), sibling_url)
            })
            .collect::<Vec<_>>();
        let sibling_manifest_entries = sibling_repositories
            .iter()
            .map(|(sibling_id, sibling_url)| (sibling_id.as_str(), sibling_url.clone()))
            .collect::<Vec<_>>();
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &sibling_manifest_entries,
            &theme_url,
            &theme_commit,
        );
        run_workspace_init(&active_path, &workspace_root, &repositories).unwrap();

        DoctorWorkspace {
            _temp: temp,
            workspace_root,
            active_path,
            theme_source_path: theme_url.to_file_path().unwrap(),
            theme_url,
            theme_commit,
        }
    }

    fn doctor_report(workspace: &DoctorWorkspace) -> super::DoctorReport {
        let args = parse_args(&[
            "build-eips",
            "-C",
            workspace.active_path.to_str().unwrap(),
            "doctor",
        ]);
        collect_doctor_report(&args, false).unwrap()
    }

    fn rewrite_active_theme_pin(workspace: &DoctorWorkspace, theme_pin: &str) {
        let active_url = file_url(&workspace.active_path);
        write_build_manifest_file_with_theme(
            &workspace.active_path,
            "Core",
            &active_url,
            &[],
            &workspace.theme_url,
            theme_pin,
        );
    }

    fn assert_workspace_init_and_doctor_for_siblings(sibling_ids: &[&str]) {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitToolingRepositories {
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };

        let sibling_repositories = sibling_ids
            .iter()
            .map(|sibling_id| {
                let sibling_id = *sibling_id;
                let sibling_path = remotes_root.join(sibling_id);
                let sibling_url = file_url(&sibling_path);
                write_manifest_repo(&sibling_path, sibling_id, &sibling_url, &[]);
                (sibling_id.to_owned(), sibling_url)
            })
            .collect::<Vec<_>>();
        let sibling_manifest_entries = sibling_repositories
            .iter()
            .map(|(repo_name, url)| (repo_name.as_str(), url.clone()))
            .collect::<Vec<_>>();
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &sibling_manifest_entries,
            &theme_url,
            &theme_commit,
        );
        let init_args = parse_args(&[
            "build-eips",
            "-C",
            active_path.to_str().unwrap(),
            "init",
            workspace_root.to_str().unwrap(),
        ]);

        init_workspace_with_repositories(
            &init_args,
            workspace_root.clone(),
            false,
            false,
            &repositories,
        )
        .unwrap();

        assert!(workspace_root.join(config::LOCAL_CONFIG_FILE).is_file());
        assert_workspace_init_optional_repos(&workspace_root, false, false);
        for sibling_id in sibling_ids {
            assert!(Repository::open(workspace_root.join(sibling_id)).is_ok());
        }

        let doctor_args =
            parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);
        let report = collect_doctor_report(&doctor_args, false).unwrap();

        assert_eq!(report.failures, 0);
    }

    fn assert_workspace_init_optional_clone_behavior(
        flags: &[&str],
        expect_template: bool,
        expect_platform_dev: bool,
    ) {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, theme_commit, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitToolingRepositories {
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo_with_theme(
            &active_path,
            "Core",
            &active_url,
            &[],
            &theme_url,
            &theme_commit,
        );
        let active_path = active_path.to_string_lossy();
        let workspace_root_arg = workspace_root.to_string_lossy();
        let mut arguments = vec![
            "build-eips",
            "-C",
            active_path.as_ref(),
            "init",
            workspace_root_arg.as_ref(),
        ];
        arguments.extend_from_slice(flags);
        let init_args = parse_args(&arguments);
        let Operation::Init {
            path: workspace_root_path,
            template,
            platform_dev,
        } = init_args.operation.clone()
        else {
            panic!("expected init command");
        };

        assert_eq!(template, expect_template);
        assert_eq!(platform_dev, expect_platform_dev);

        init_workspace_with_repositories(
            &init_args,
            workspace_root_path,
            template,
            platform_dev,
            &repositories,
        )
        .unwrap();

        assert_workspace_init_optional_repos(&workspace_root, expect_template, expect_platform_dev);
    }

    #[test]
    fn workspace_init_and_doctor_cover_zero_one_and_many_siblings() {
        assert_workspace_init_and_doctor_for_siblings(&[]);
        assert_workspace_init_and_doctor_for_siblings(&["ERCs"]);
        assert_workspace_init_and_doctor_for_siblings(&["EIPs", "ERCs"]);
    }

    #[test]
    fn workspace_doctor_missing_active_build_toml_reports_failure() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("Core");
        std::fs::create_dir_all(&active_path).unwrap();
        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);

        let report = collect_doctor_report(&args, false).unwrap();

        assert_eq!(report.failures, 1);
    }

    #[test]
    fn workspace_doctor_invalid_active_build_toml_reports_failure() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("Core");
        init_repo(&active_path, &[("content/0001.md", "# Proposal\n")]);
        write_file(&active_path, config::MANIFEST_FILE, "[");
        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);

        let report = collect_doctor_report(&args, false).unwrap();

        assert_eq!(report.failures, 1);
    }

    #[test]
    fn workspace_doctor_accepts_matching_sibling_manifest() {
        let workspace = initialized_doctor_workspace(&["ERCs"]);

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_warns_for_missing_sibling_build_toml() {
        let workspace = initialized_doctor_workspace(&["ERCs"]);
        std::fs::remove_file(workspace.workspace_root.join("ERCs/Build.toml")).unwrap();

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 1);
    }

    #[test]
    fn workspace_doctor_fails_for_malformed_sibling_build_toml() {
        let workspace = initialized_doctor_workspace(&["ERCs"]);
        write_file(
            &workspace.workspace_root.join("ERCs"),
            config::MANIFEST_FILE,
            "[",
        );

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_sibling_manifest_name_mismatch() {
        let workspace = initialized_doctor_workspace(&["ERCs"]);
        let sibling_path = workspace.workspace_root.join("ERCs");
        let sibling_url = file_url(&sibling_path);
        write_build_manifest_file(&sibling_path, "Wrong", &sibling_url, &[]);

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_missing_theme() {
        let workspace = initialized_doctor_workspace(&[]);
        std::fs::remove_dir_all(workspace.workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_non_git_theme() {
        let workspace = initialized_doctor_workspace(&[]);
        let theme_path = workspace.workspace_root.join(config::DEFAULT_THEME_DIR);
        std::fs::remove_dir_all(&theme_path).unwrap();
        write_file(&theme_path, "config/zola.toml", "title = \"theme\"\n");

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_missing_theme_zola_config() {
        let workspace = initialized_doctor_workspace(&[]);
        std::fs::remove_file(workspace.workspace_root.join("theme/config/zola.toml")).unwrap();

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_invalid_theme_zola_config() {
        let workspace = initialized_doctor_workspace(&[]);
        write_file(
            &workspace.workspace_root.join(config::DEFAULT_THEME_DIR),
            "config/zola.toml",
            "[",
        );

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_missing_theme_eipw_config() {
        let workspace = initialized_doctor_workspace(&[]);
        std::fs::remove_file(workspace.workspace_root.join("theme/config/eipw.toml")).unwrap();

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_fails_for_incompatible_theme_eipw_schema() {
        let workspace = initialized_doctor_workspace(&[]);
        write_file(
            &workspace.workspace_root.join(config::DEFAULT_THEME_DIR),
            "config/eipw.toml",
            "schema-version = \"999.0.0\"\n",
        );

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_does_not_warn_when_theme_pin_matches() {
        let workspace = initialized_doctor_workspace(&[]);

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_accepts_short_sha_theme_pin() {
        let workspace = initialized_doctor_workspace(&[]);
        let short_pin = &workspace.theme_commit[..8];
        rewrite_active_theme_pin(&workspace, short_pin);

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_accepts_tag_theme_pin() {
        let workspace = initialized_doctor_workspace(&[]);
        let theme =
            Repository::open(workspace.workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        let head = theme.revparse_single("HEAD").unwrap();
        theme.tag_lightweight("manifest-pin", &head, false).unwrap();
        rewrite_active_theme_pin(&workspace, "manifest-pin");

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_warns_for_theme_pin_drift_without_failing_readiness() {
        let workspace = initialized_doctor_workspace(&[]);
        let theme_path = workspace.workspace_root.join(config::DEFAULT_THEME_DIR);
        let theme = Repository::open(&theme_path).unwrap();
        write_file(&theme_path, "LOCAL.md", "local theme edit\n");
        commit_all(&theme, "local theme edit");

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 1);
    }

    #[test]
    fn workspace_doctor_warns_for_missing_local_theme_pin_without_fetching() {
        let workspace = initialized_doctor_workspace(&[]);
        let source_theme = Repository::open(&workspace.theme_source_path).unwrap();
        write_file(
            &workspace.theme_source_path,
            "REMOTE-ONLY.md",
            "remote-only theme commit\n",
        );
        commit_all(&source_theme, "remote-only theme commit");
        let remote_only_pin = repository_head(&source_theme);
        rewrite_active_theme_pin(&workspace, &remote_only_pin);

        let theme_path = workspace.workspace_root.join(config::DEFAULT_THEME_DIR);
        let local_theme = Repository::open(&theme_path).unwrap();
        assert!(local_theme.revparse_single(&remote_only_pin).is_err());
        drop(local_theme);

        let report = doctor_report(&workspace);

        let local_theme = Repository::open(&theme_path).unwrap();
        assert!(local_theme.revparse_single(&remote_only_pin).is_err());
        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 1);
    }

    #[test]
    fn workspace_doctor_accepts_theme_remote_with_non_origin_name() {
        let workspace = initialized_doctor_workspace(&[]);
        let theme =
            Repository::open(workspace.workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        theme.remote_rename("origin", "theme-source").unwrap();

        let report = doctor_report(&workspace);

        assert_eq!(report.failures, 0);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn default_workspace_init_clones_required_repos_only() {
        assert_workspace_init_optional_clone_behavior(&[], false, false);
    }

    #[test]
    fn workspace_init_template_clones_template_only_as_optional_repo() {
        assert_workspace_init_optional_clone_behavior(&["--template"], true, false);
    }

    #[test]
    fn workspace_init_platform_dev_clones_platform_repos_only_as_optional_repos() {
        assert_workspace_init_optional_clone_behavior(&["--platform-dev"], false, true);
    }

    #[test]
    fn workspace_init_template_and_platform_dev_clone_all_optional_repos() {
        assert_workspace_init_optional_clone_behavior(
            &["--template", "--platform-dev"],
            true,
            true,
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_tool_candidates_must_be_executable_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let candidate = temp.path().join("tool");
        std::fs::write(&candidate, "#!/bin/sh\n").unwrap();

        let mut permissions = std::fs::metadata(&candidate).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&candidate, permissions).unwrap();
        assert!(!super::is_command_candidate(&candidate));

        let mut permissions = std::fs::metadata(&candidate).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&candidate, permissions).unwrap();
        assert!(super::is_command_candidate(&candidate));
    }

    #[test]
    fn workspace_doctor_explicit_plain_directory_reports_invalid_root() {
        let workspace = TempDir::new().unwrap();
        let args = parse_args(&[
            "build-eips",
            "-C",
            workspace.path().to_str().unwrap(),
            "doctor",
        ]);

        let error = doctor_root(&args).unwrap_err().to_string();

        assert_eq!(error, "invalid root directory");
    }

    #[test]
    fn workspace_doctor_missing_config_reports_one_failure_without_skip_warning() {
        let workspace = TempDir::new().unwrap();
        let active_path = workspace.path().join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);
        let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);

        let report = collect_doctor_report(&args, false).unwrap();

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_parse_failed_config_reports_one_failure_without_skip_warning() {
        let workspace = TempDir::new().unwrap();
        let active_path = workspace.path().join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);
        std::fs::write(workspace.path().join(config::LOCAL_CONFIG_FILE), "[").unwrap();
        let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);

        let report = collect_doctor_report(&args, false).unwrap();

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }

    #[test]
    fn workspace_doctor_removed_config_fields_report_parse_failure_check() {
        let workspace = TempDir::new().unwrap();
        let active_path = workspace.path().join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);
        let config_path = workspace.path().join(config::LOCAL_CONFIG_FILE);
        std::fs::write(
            &config_path,
            r#"
build_root_base = ".local-build"
default_profile = "local"

[profiles.local]
staging = true
"#,
        )
        .unwrap();
        let error = LoadedWorkspaceConfig::from_path(&config_path).unwrap_err();
        assert!(matches!(error, config::WorkspaceError::Parse { .. }));
        let args = parse_args(&["build-eips", "-C", active_path.to_str().unwrap(), "doctor"]);

        let report = collect_doctor_report(&args, false).unwrap();

        assert_eq!(report.failures, 1);
        assert_eq!(report.warnings, 0);
    }
}
