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
    config::{self, LoadedRepoManifest, LoadedWorkspaceConfig},
    context::{load_workspace_command_context, resolve_input_path, root},
    git,
    identity::ActiveRepoIdentity,
};

const WORKSPACE_THEME_URL: &str = "https://github.com/eips-wg/theme.git";
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

struct WorkspaceInitRepositories<'a> {
    theme: &'a Url,
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
                .find(|candidate| candidate.is_file())
        })
    }

    #[cfg(not(windows))]
    {
        std::env::split_paths(&path).find_map(|entry| {
            candidates
                .iter()
                .map(|candidate| entry.join(candidate))
                .find(|candidate| candidate.is_file())
        })
    }
}

fn check_workspace_repo(
    report: &mut DoctorReport,
    workspace_root: &Path,
    name: &str,
) -> Option<PathBuf> {
    let path = workspace_root.join(name);
    match git2::Repository::open(&path) {
        Ok(_) => {
            report.record(
                DoctorStatus::Ok,
                format!(
                    "found workspace repo `{}` at `{}`",
                    name,
                    path.to_string_lossy()
                ),
            );
            Some(path)
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
        Err(_) => {
            report.record(
                DoctorStatus::Fail,
                format!(
                    "expected `{}` to be a git repository at `{}`",
                    name,
                    path.to_string_lossy()
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
    match LoadedRepoManifest::load(sibling_path) {
        Ok(Some(manifest)) if manifest.manifest().repo_id == expected_repo_id => report.record(
            DoctorStatus::Ok,
            format!("sibling `{expected_repo_id}` manifest repo_id matches workspace key"),
        ),
        Ok(Some(manifest)) => report.record(
            DoctorStatus::Fail,
            format!(
                "sibling `{expected_repo_id}` manifest declares repo_id `{}`",
                manifest.manifest().repo_id
            ),
        ),
        Ok(None) => (),
        Err(error) => report.record(
            DoctorStatus::Fail,
            format!(
                "sibling `{expected_repo_id}` repo manifest could not be loaded: {}",
                Report::from_error(error)
            ),
        ),
    }
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
    let (root_path, active_repo) = match root(args) {
        Ok(root_path) => match ActiveRepoIdentity::load(&root_path) {
            Ok(active_repo) => {
                report.record(
                    DoctorStatus::Ok,
                    format!(
                        "identified active repo `{}` from {}",
                        active_repo.repo_id(),
                        active_repo.source_description()
                    ),
                );
                if let Some(manifest) = active_repo.manifest() {
                    report.record(
                        DoctorStatus::Ok,
                        format!(
                            "repo manifest parses at `{}`",
                            manifest.manifest_path().to_string_lossy()
                        ),
                    );
                }
                (Some(root_path), Some(active_repo))
            }
            Err(error) => {
                report.record(
                    DoctorStatus::Fail,
                    format!("active repo identity could not be resolved: {error}"),
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
                let expected_root = workspace_root.join(active_repo.repo_id());
                if root_path == &expected_root {
                    report.record(
                        DoctorStatus::Ok,
                        format!(
                            "active repo `{}` is checked out at `{}`",
                            active_repo.repo_id(),
                            expected_root.to_string_lossy()
                        ),
                    );
                } else {
                    report.record(
                        DoctorStatus::Fail,
                        format!(
                            "active repo `{}` should be checked out at `{}`, found `{}`",
                            active_repo.repo_id(),
                            expected_root.to_string_lossy(),
                            root_path.to_string_lossy()
                        ),
                    );
                }

                check_workspace_repo(&mut report, workspace_root, active_repo.repo_id());
                for sibling_id in active_repo.sibling_ids() {
                    if let Some(sibling_path) =
                        check_workspace_repo(&mut report, workspace_root, &sibling_id)
                    {
                        check_sibling_manifest_id(&mut report, &sibling_path, &sibling_id);
                    }
                }
            } else {
                report.record(
                    DoctorStatus::Warn,
                    "workspace repo layout checks were skipped because active repo identity was unavailable",
                );
            }

            check_workspace_repo(&mut report, workspace_root, config::DEFAULT_THEME_DIR);
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
    let theme_repository = Url::parse(WORKSPACE_THEME_URL)
        .whatever_context("invalid workspace theme repository URL")?;
    let template_repository = Url::parse(PROPOSAL_TEMPLATE_URL)
        .whatever_context("invalid proposal template repository URL")?;
    let preprocessor_repository = Url::parse(PLATFORM_PREPROCESSOR_URL)
        .whatever_context("invalid platform preprocessor repository URL")?;
    let eipw_repository =
        Url::parse(PLATFORM_EIPW_URL).whatever_context("invalid platform eipw repository URL")?;
    let repositories = WorkspaceInitRepositories {
        theme: &theme_repository,
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

    if platform_dev {
        git::clone_missing_repo(
            repositories.preprocessor.as_str(),
            &workspace_root.join("preprocessor"),
        )
        .whatever_context("unable to clone workspace preprocessor repo")?;
        git::clone_missing_repo(repositories.eipw.as_str(), &workspace_root.join("eipw"))
            .whatever_context("unable to clone workspace eipw repo")?;
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
        collect_doctor_report, init_workspace_with_repositories, workspace_doc_text,
        WorkspaceInitRepositories, WORKSPACE_DOC_FILE, WORKSPACE_THEME_URL,
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

    fn write_repo_manifest_file(
        path: &Path,
        repo_id: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
    ) {
        write_file(
            path,
            config::REPO_MANIFEST_FILE,
            &repo_manifest_text(repo_id, upstream, siblings),
        );
    }

    fn write_manifest_repo(
        path: &Path,
        repo_id: &str,
        upstream: &Url,
        siblings: &[(&str, Url)],
    ) -> Repository {
        let repo = init_repo(path, &[("content/0001.md", "# Proposal\n")]);
        write_repo_manifest_file(path, repo_id, upstream, siblings);
        commit_all(&repo, "add repo manifest");
        repo
    }

    fn init_workspace_source_repo(remotes_root: &Path, name: &str) -> Url {
        let path = remotes_root.join(name);
        init_repo(&path, &[("README.md", "init test repo\n")]);
        file_url(&path)
    }

    fn workspace_init_test_repository_urls(remotes_root: &Path) -> (Url, Url, Url, Url) {
        (
            init_workspace_source_repo(remotes_root, "theme"),
            init_workspace_source_repo(remotes_root, "template"),
            init_workspace_source_repo(remotes_root, "preprocessor"),
            init_workspace_source_repo(remotes_root, "eipw"),
        )
    }

    fn run_workspace_init_for_docs(
        existing_doc: Option<&str>,
        existing_config: Option<&str>,
    ) -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitRepositories {
            theme: &theme_url,
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);

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
    fn workspace_theme_url_is_bootstrap_metadata() {
        assert_eq!(
            Url::parse(WORKSPACE_THEME_URL).unwrap().as_str(),
            "https://github.com/eips-wg/theme.git"
        );
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
            ".build-eips.toml",
            ".local-build",
            "build-eips init",
            "build-eips doctor",
            "build-eips build",
            "build-eips serve",
            "build-eips preview",
            "build-eips editorial check",
            "[render]",
            "only = [",
            "--only",
            "--remote-siblings",
            "--base-url",
        ] {
            assert!(
                text.contains(expected),
                "workspace document text should contain `{expected}`"
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

    fn assert_workspace_init_and_doctor_for_siblings(sibling_ids: &[&str]) {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let remotes_root = temp.path().join("remotes");
        let (theme_url, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitRepositories {
            theme: &theme_url,
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
            .map(|(repo_id, url)| (repo_id.as_str(), url.clone()))
            .collect::<Vec<_>>();
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &sibling_manifest_entries);
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
        let (theme_url, template_url, preprocessor_url, eipw_url) =
            workspace_init_test_repository_urls(&remotes_root);
        let repositories = WorkspaceInitRepositories {
            theme: &theme_url,
            template: &template_url,
            preprocessor: &preprocessor_url,
            eipw: &eipw_url,
        };
        let active_path = workspace_root.join("Core");
        let active_url = file_url(&active_path);
        write_manifest_repo(&active_path, "Core", &active_url, &[]);
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
