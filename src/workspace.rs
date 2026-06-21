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
) -> Result<(), Whatever> {
    let template_repository = Url::parse(PROPOSAL_TEMPLATE_URL)
        .whatever_context("invalid proposal template repository URL")?;
    let repositories = WorkspaceInitToolingRepositories {
        template: &template_repository,
    };

    init_workspace_with_repositories(
        args,
        workspace_root,
        include_template,
        &repositories,
    )
}

fn init_workspace_with_repositories(
    args: &Args,
    workspace_root: PathBuf,
    include_template: bool,
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

