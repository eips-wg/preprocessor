/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod cache;
mod config;
mod find_root;
mod git;
mod github;
mod lint;
mod markdown;
mod print;
mod progress;
mod zola;

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use fslock::LockFile;
use log::{debug, info};
use snafu::{OptionExt, Report, ResultExt, Whatever};
use url::Url;

use crate::config::{Config, LoadedWorkspaceConfig, LocalOverrides};

const CONTENT_DIR: &str = "content";
const BUILD_DIR: &str = "build";
const REPO_DIR: &str = "repo";
const OUTPUT_DIR: &str = "output";
const JUSTFILE_NAME: &str = "justfile";
const PLATFORM_PREPROCESSOR_URL: &str = "https://github.com/eips-wg/preprocessor.git";
const PLATFORM_EIPW_URL: &str = "https://github.com/ethereum/eipw.git";

#[derive(Debug, Clone)]
pub(crate) enum ThemeSource {
    Remote { repository: String, commit: String },
    Local { path: PathBuf },
}

/// Build script for Ethereum EIPs and ERCs.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Use ROOT as the base directory (instead of finding it automatically)
    #[clap(short = 'C')]
    root: Option<PathBuf>,

    /// Use the staging repositories (for testing)
    #[clap(long = "staging")]
    staging: bool,

    /// Load workspace defaults from CONFIG instead of auto-discovering `.build-eips.toml`
    #[clap(long)]
    config: Option<PathBuf>,

    /// Use the named local profile from `.build-eips.toml`
    #[clap(long)]
    profile: Option<String>,

    /// Use a local theme checkout instead of the configured remote theme
    #[clap(long)]
    theme_path: Option<PathBuf>,

    /// Use a local checkout for the sibling content repository
    #[clap(long)]
    other_repo_path: Option<PathBuf>,

    /// Write build artifacts under BUILD_ROOT instead of the default location
    #[clap(long)]
    build_root: Option<PathBuf>,

    /// Use tracked working-tree changes from the active content repo without requiring a commit
    #[clap(long)]
    allow_dirty: bool,

    #[clap(subcommand)]
    operation: Operation,
}

#[derive(Debug, Subcommand)]
enum Operation {
    /// Print various useful things, like available lints
    Print {
        #[command(flatten)]
        print: print::CmdArgs,
    },

    /// Build the project and output HTML
    Build {
        #[command(flatten)]
        eipw: lint::CmdArgs,
    },

    /// Build the project and launch a web server to preview it
    Serve {
        #[command(flatten)]
        eipw: lint::CmdArgs,
    },

    /// Remove temporary and output files
    Clean,

    /// Analyze the repository and report errors, but don't build HTML files
    Check {
        #[command(flatten)]
        eipw: lint::CmdArgs,
    },

    /// List files changed since the last commit common to both the local and upstream repositories
    Changed {
        /// List all changed files, not just proposals
        #[arg(long, short)]
        all: bool,
        #[clap(long, value_enum, default_value_t)]
        format: ChangedFormat,
    },

    /// Manage local multi-repo workspace state
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}

#[derive(Debug, Subcommand, Clone)]
enum WorkspaceCommand {
    /// Create the local workspace config and clone any missing sibling repositories
    Init {
        /// Workspace root directory
        path: PathBuf,

        /// Also clone preprocessor and eipw for platform development
        #[arg(long)]
        platform_dev: bool,
    },

    /// Regenerate generated workspace helper files
    Refresh,

    /// Check whether the local workspace is ready for the local daily workflow
    Doctor,
}

#[derive(Debug, clap::ValueEnum, Clone, Default)]
enum ChangedFormat {
    #[default]
    Newline,
    Nul,
    Json,
}

#[derive(Debug, Clone)]
struct ResolvedExecution {
    root_path: PathBuf,
    build_path: PathBuf,
    repository_use: git::RepositoryUse,
    theme: ThemeSource,
    source_materialization: git::SourceMaterialization,
}

#[derive(Debug, Clone, Copy)]
enum GeneratedFileState {
    Created,
    Updated,
    Current,
}

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

#[derive(Debug, Clone)]
struct WorkspaceCommandContext {
    search_from: PathBuf,
    config_path: Option<PathBuf>,
}

impl ChangedFormat {
    fn print_sep(files: &[&Path], sep: &str) {
        let files: Vec<_> = files
            .iter()
            .map(|f| f.to_str().expect("path not UTF-8"))
            .collect();
        if files.iter().any(|f| f.contains(sep)) {
            panic!("changed file path contains separator");
        }
        println!("{}", files.join(sep));
    }

    fn print_json(files: &[&Path]) {
        let stdout = std::io::stdout();
        serde_json::to_writer_pretty(stdout, files).unwrap();
    }

    fn print(&self, files: &[PathBuf], repo_path: &Path) {
        let files: Vec<_> = files
            .iter()
            .map(|f| match f.strip_prefix(repo_path) {
                Ok(p) => p,
                _ => f,
            })
            .collect();

        match self {
            Self::Newline => Self::print_sep(&files, "\n"),
            Self::Nul => Self::print_sep(&files, "\0"),
            Self::Json => Self::print_json(&files),
        }
    }
}

impl GeneratedFileState {
    fn verb(self) -> &'static str {
        match self {
            Self::Created => "generated",
            Self::Updated => "refreshed",
            Self::Current => "already current",
        }
    }
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

impl DoctorReport {
    fn record(&mut self, status: DoctorStatus, message: impl AsRef<str>) {
        match status {
            DoctorStatus::Ok => (),
            DoctorStatus::Warn => self.warnings += 1,
            DoctorStatus::Fail => self.failures += 1,
        }

        println!("[{}] {}", status.label(), message.as_ref());
    }
}

fn lock(build_path: &Path) -> Result<LockFile, Whatever> {
    let lock_path = build_path.join(".lock");
    let mut lock_file =
        fslock::LockFile::open(&lock_path).whatever_context("unable to open lock file")?;
    let locked = lock_file
        .try_lock_with_pid()
        .whatever_context("unable to lock build directory")?;
    if !locked {
        info!("waiting on build directory...");
        lock_file
            .lock_with_pid()
            .whatever_context("unable to lock build directory")?;
    }
    Ok(lock_file)
}

fn resolve_input_path(path: &Path) -> Result<PathBuf, Whatever> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = std::env::current_dir().whatever_context("unable to get current directory")?;
        Ok(cwd.join(path))
    }
}

fn root(args: &Args) -> Result<PathBuf, Whatever> {
    let dir = match &args.root {
        None => find_root::find_root().whatever_context("cannot find repository root")?,
        Some(path) => {
            find_root::is_root(path).whatever_context("invalid root directory")?;
            path.canonicalize()
                .whatever_context("unable to canonicalize root directory")?
        }
    };
    find_root::is_root(&dir).whatever_context("invalid root directory")?;
    Ok(dir)
}

fn workspace_search_start(args: &Args) -> Result<PathBuf, Whatever> {
    match &args.root {
        Some(path) => {
            let path = resolve_input_path(path)?;
            path.canonicalize()
                .whatever_context("unable to canonicalize workspace search path")
        }
        None => std::env::current_dir().whatever_context("unable to get current directory"),
    }
}

fn load_workspace_command_context(args: &Args) -> Result<WorkspaceCommandContext, Whatever> {
    let search_from = workspace_search_start(args)?;
    let config_path = match args.config.as_deref() {
        Some(path) => Some(resolve_input_path(path)?),
        None => config::discover_path(&search_from),
    };

    Ok(WorkspaceCommandContext {
        search_from,
        config_path,
    })
}

fn generated_justfile_text() -> &'static str {
    r#"# Generated by `build-eips workspace refresh`.
default:
    @just --list

check:
    build-eips -C "{{ invocation_directory() }}" check

build:
    build-eips -C "{{ invocation_directory() }}" build

serve:
    build-eips -C "{{ invocation_directory() }}" serve

parity-check:
    build-eips -C "{{ invocation_directory() }}" --profile parity check

parity-build:
    build-eips -C "{{ invocation_directory() }}" --profile parity build

parity-serve:
    build-eips -C "{{ invocation_directory() }}" --profile parity serve

dirty-check:
    build-eips -C "{{ invocation_directory() }}" --profile dirty check

dirty-build:
    build-eips -C "{{ invocation_directory() }}" --profile dirty build

dirty-serve:
    build-eips -C "{{ invocation_directory() }}" --profile dirty serve
"#
}

fn sync_generated_file(path: &Path, contents: &str) -> Result<GeneratedFileState, Whatever> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == contents => Ok(GeneratedFileState::Current),
        Ok(_) => {
            std::fs::write(path, contents)
                .whatever_context("unable to update generated workspace helper")?;
            Ok(GeneratedFileState::Updated)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::write(path, contents)
                .whatever_context("unable to write generated workspace helper")?;
            Ok(GeneratedFileState::Created)
        }
        Err(error) => snafu::whatever!(
            "unable to read generated workspace helper `{}`: {}",
            path.to_string_lossy(),
            Report::from_error(error)
        ),
    }
}

fn refresh_workspace(args: &Args) -> Result<(), Whatever> {
    let context = load_workspace_command_context(args)?;
    let loaded_config = context
        .config_path
        .as_deref()
        .map(LoadedWorkspaceConfig::from_path)
        .transpose()
        .whatever_context("unable to load workspace config")?;
    let config = loaded_config
        .as_ref()
        .whatever_context("unable to find workspace config `.build-eips.toml`")?;
    let justfile_path = config.workspace_root().join(JUSTFILE_NAME);
    let state = sync_generated_file(&justfile_path, generated_justfile_text())?;
    info!("{} `{}`", state.verb(), justfile_path.to_string_lossy());
    Ok(())
}

fn command_path(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    #[cfg(not(windows))]
    let candidates = vec![command.to_owned()];

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

fn check_workspace_repo(report: &mut DoctorReport, workspace_root: &Path, name: &str) {
    let path = workspace_root.join(name);
    if !path.exists() {
        report.record(
            DoctorStatus::Fail,
            format!(
                "expected workspace repo `{}` at `{}`",
                name,
                path.to_string_lossy()
            ),
        );
        return;
    }

    match git2::Repository::open(&path) {
        Ok(_) => report.record(
            DoctorStatus::Ok,
            format!(
                "found workspace repo `{}` at `{}`",
                name,
                path.to_string_lossy()
            ),
        ),
        Err(_) => report.record(
            DoctorStatus::Fail,
            format!(
                "expected `{}` to be a git repository at `{}`",
                name,
                path.to_string_lossy()
            ),
        ),
    }
}

fn check_tool(report: &mut DoctorReport, command: &str, why: &str) {
    match command_path(command) {
        Some(path) => report.record(
            DoctorStatus::Ok,
            format!(
                "found required tool `{}` at `{}`",
                command,
                path.to_string_lossy()
            ),
        ),
        None => report.record(
            DoctorStatus::Fail,
            format!("missing required tool `{}`: {}", command, why),
        ),
    }
}

fn check_optional_download_tool(report: &mut DoctorReport) {
    let curl = command_path("curl");
    let wget = command_path("wget");

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

fn doctor_workspace(args: &Args) -> Result<(), Whatever> {
    let context = load_workspace_command_context(args)?;
    let mut report = DoctorReport::default();

    match context.config_path.as_ref() {
        Some(path) if path.is_file() => report.record(
            DoctorStatus::Ok,
            format!(
                "found workspace config candidate `{}`",
                path.to_string_lossy()
            ),
        ),
        Some(path) => report.record(
            DoctorStatus::Fail,
            format!("expected workspace config at `{}`", path.to_string_lossy()),
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

    let parsed_config = match context.config_path.as_deref() {
        Some(path) if path.is_file() => Some(LoadedWorkspaceConfig::from_path(path)).transpose(),
        Some(_) | None => Ok(None),
    };

    if let Ok(Some(config)) = parsed_config.as_ref() {
        report.record(
            DoctorStatus::Ok,
            format!(
                "workspace config parses at `{}`",
                config.path().to_string_lossy()
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

        for repo_name in ["EIPs", "ERCs", config::DEFAULT_THEME_DIR] {
            check_workspace_repo(&mut report, workspace_root, repo_name);
        }

        let justfile_path = workspace_root.join(JUSTFILE_NAME);
        match std::fs::read_to_string(&justfile_path) {
            Ok(existing) if existing == generated_justfile_text() => report.record(
                DoctorStatus::Ok,
                format!(
                    "generated helper `{}` is current",
                    justfile_path.to_string_lossy()
                ),
            ),
            Ok(_) => report.record(
                DoctorStatus::Fail,
                format!(
                    "generated helper `{}` is stale; run `build-eips workspace refresh`",
                    justfile_path.to_string_lossy()
                ),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => report.record(
                DoctorStatus::Fail,
                format!(
                    "generated helper `{}` is missing; run `build-eips workspace refresh`",
                    justfile_path.to_string_lossy()
                ),
            ),
            Err(error) => report.record(
                DoctorStatus::Fail,
                format!(
                    "unable to read generated helper `{}`: {}",
                    justfile_path.to_string_lossy(),
                    Report::from_error(error)
                ),
            ),
        }
    } else if let Err(error) = parsed_config {
        report.record(
            DoctorStatus::Fail,
            format!(
                "workspace config could not be parsed: {}",
                Report::from_error(error)
            ),
        );
        report.record(
            DoctorStatus::Warn,
            "workspace layout checks were skipped because the workspace config could not be parsed",
        );
    } else if context.config_path.is_some() {
        report.record(
            DoctorStatus::Fail,
            "workspace config could not be parsed, so workspace layout checks were skipped",
        );
    } else {
        report.record(
            DoctorStatus::Warn,
            "workspace layout checks were skipped because no workspace config was found",
        );
    }

    check_tool(
        &mut report,
        "build-eips",
        "`just` recipes call `build-eips` directly, so install the release binary or put your dev build on PATH",
    );
    check_tool(
        &mut report,
        "git",
        "workspace init, refresh, and daily builds expect git to be available",
    );
    check_tool(
        &mut report,
        "zola",
        "daily build, check, and serve commands need a working zola binary",
    );
    check_tool(
        &mut report,
        "just",
        "local daily commands use the generated workspace justfile",
    );
    check_optional_download_tool(&mut report);

    match command_path("tar") {
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

    if report.failures > 0 {
        snafu::whatever!(
            "workspace doctor found {} failing check(s)",
            report.failures
        );
    }

    Ok(())
}

fn make_build_dir(build_path: &Path) -> Result<PathBuf, Whatever> {
    if let Err(e) = std::fs::create_dir_all(build_path) {
        debug!(
            "got while creating build directory: {}",
            Report::from_error(e)
        );
    }
    Ok(build_path.to_path_buf())
}

fn apply_local_other_repo(
    repository_use: &mut git::RepositoryUse,
    path: &Path,
) -> Result<(), Whatever> {
    repository_use
        .only_other_repo()
        .whatever_context("local sibling overrides require exactly one sibling repository")?;

    let url = Url::from_directory_path(path)
        .ok()
        .whatever_context("unable to convert local sibling repository path into a file URL")?;

    for repository in repository_use.other_repos.values_mut() {
        *repository = url.clone();
    }

    Ok(())
}

fn build_path(
    root_path: &Path,
    repository_use: &git::RepositoryUse,
    workspace_config: Option<&LoadedWorkspaceConfig>,
    overrides: &LocalOverrides,
) -> PathBuf {
    overrides
        .build_root
        .clone()
        .or_else(|| {
            workspace_config
                .map(|workspace_config| workspace_config.build_root_for(&repository_use.title))
        })
        .unwrap_or_else(|| root_path.join(BUILD_DIR))
}

fn theme_source(
    baseline: &Config,
    workspace_config: Option<&LoadedWorkspaceConfig>,
    selected_profile: Option<&config::SelectedProfile>,
    overrides: &LocalOverrides,
) -> ThemeSource {
    let theme_path =
        overrides
            .theme_path
            .clone()
            .or_else(|| match (workspace_config, selected_profile) {
                (Some(workspace_config), Some(profile)) if profile.profile.use_local_theme => {
                    Some(workspace_config.local_theme_path())
                }
                _ => None,
            });

    match theme_path {
        Some(path) => ThemeSource::Local { path },
        None => ThemeSource::Remote {
            repository: baseline.theme.repository.to_string(),
            commit: baseline.theme.commit.clone(),
        },
    }
}

fn resolve_execution(args: &Args) -> Result<ResolvedExecution, Whatever> {
    let root_path = root(args)?;
    let workspace_config = LoadedWorkspaceConfig::load(args.config.as_deref(), &root_path)
        .whatever_context("unable to load workspace config")?;
    let selected_profile =
        config::selected_profile(workspace_config.as_ref(), args.profile.as_deref())
            .whatever_context("unable to select workspace profile")?;

    if let Some(workspace_config) = workspace_config.as_ref() {
        debug!(
            "using workspace config `{}`",
            workspace_config.path().to_string_lossy()
        );
    }

    if let Some(profile) = selected_profile.as_ref() {
        info!("using workspace profile `{}`", profile.name);
    }

    let overrides = LocalOverrides {
        theme_path: args
            .theme_path
            .as_deref()
            .map(resolve_input_path)
            .transpose()?,
        other_repo_path: args
            .other_repo_path
            .as_deref()
            .map(resolve_input_path)
            .transpose()?,
        build_root: args
            .build_root
            .as_deref()
            .map(resolve_input_path)
            .transpose()?,
    };

    let use_staging = args.staging
        || selected_profile
            .as_ref()
            .map(|profile| profile.profile.staging)
            .unwrap_or(false);
    let allow_dirty = args.allow_dirty
        || selected_profile
            .as_ref()
            .map(|profile| profile.profile.allow_dirty)
            .unwrap_or(false);
    let baseline = if use_staging {
        Config::staging()
    } else {
        Config::production()
    };

    let mut repository_use = baseline
        .locations
        .identify_repository(&root_path)
        .whatever_context("cannot identify repository use")?;

    let other_repo_path = overrides.other_repo_path.clone().or_else(|| {
        match (workspace_config.as_ref(), selected_profile.as_ref()) {
            (Some(workspace_config), Some(profile)) if profile.profile.use_local_sibling => {
                let (other_name, _) = repository_use.only_other_repo()?;
                Some(workspace_config.local_repo_path(other_name))
            }
            _ => None,
        }
    });

    if let Some(path) = other_repo_path {
        apply_local_other_repo(&mut repository_use, &path)?;
    }

    let build_path = build_path(
        &root_path,
        &repository_use,
        workspace_config.as_ref(),
        &overrides,
    );
    let theme = theme_source(
        &baseline,
        workspace_config.as_ref(),
        selected_profile.as_ref(),
        &overrides,
    );
    let source_materialization = if allow_dirty {
        info!(
            "dirty mode is enabled; tracked working-tree changes from the active content repo will be materialized into the build input"
        );
        git::SourceMaterialization::Dirty
    } else {
        git::SourceMaterialization::Clean
    };

    Ok(ResolvedExecution {
        root_path,
        build_path,
        repository_use,
        theme,
        source_materialization,
    })
}

#[derive(Debug)]
struct Prepared {
    cache: cache::Cache,
    repo_path: PathBuf,
    output_path: PathBuf,
    repository_use: git::RepositoryUse,
    theme: ThemeSource,
}

impl Prepared {
    fn is_proposal_path(p: PathBuf) -> bool {
        // Only lint `content/00001.md` and `content/00001/index.md` files.
        let mut p = p.to_path_buf();

        // content/00000.md  |  content/00000/index.md
        //         ^^^^^^^^  |                ^^^^^^^^
        match p.file_name() {
            Some(n) if n == "index.md" => {
                p.pop();
            }
            Some(_) if p.extension().map(|x| x == "md").unwrap_or(false) => {
                p.set_extension("");
            }
            None | Some(_) => return false,
        }

        // content/00000
        //         ^^^^^
        match p.file_name().and_then(OsStr::to_str) {
            None => return false,
            Some(f) if f.parse::<u64>().is_err() => return false,
            Some(_) => {
                p.pop();
            }
        }

        // content
        // ^^^^^^^
        match p.file_name() {
            Some(f) if f == "content" => {
                p.pop();
            }
            _ => return false,
        }

        p == OsStr::new("")
    }

    fn prepare(eipw: lint::CmdArgs, resolved: ResolvedExecution) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme,
            source_materialization,
        } = resolved;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = build_path.join(OUTPUT_DIR);

        let both = git::Fresh::new(
            &root_path,
            &repo_path,
            repository_use.clone(),
            source_materialization,
        )
        .whatever_context("initializing build repo")?
        .clone_src()
        .whatever_context("cloning source repo")?
        .fetch_upstream()
        .whatever_context("fetching upstream repo")?;

        let changed_files: Vec<_> = both
            .changed_files()
            .whatever_context("unable to list changed files")?
            .into_iter()
            .filter(|p| Self::is_proposal_path(p.into()))
            .map(|p| repo_path.join(p))
            .collect();

        both.merge()
            .whatever_context("unable to merge ERC/EIP repositories")?;

        let cache = cache::Cache::open().whatever_context("unable to open cache")?;

        lint::eipw(&theme, &cache, &root_path, &repo_path, changed_files, eipw)
            .whatever_context("linting failed")?;

        markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;

        Ok(Prepared {
            repository_use,
            theme,
            cache,
            repo_path,
            output_path,
        })
    }

    fn build(self) -> Result<(), Whatever> {
        zola::build(
            &self.theme,
            &self.cache,
            &self.repo_path,
            &self.output_path,
            self.repository_use.location.base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        Ok(())
    }

    fn serve(self) -> Result<(), Whatever> {
        zola::serve(&self.theme, &self.cache, &self.repo_path, &self.output_path)
            .whatever_context("zola serve failed")?;
        Ok(())
    }

    fn check(self) -> Result<(), Whatever> {
        zola::check(&self.theme, &self.cache, &self.repo_path)
            .whatever_context("zola check failed")?;
        Ok(())
    }
}

fn clone_missing_repo(url: &str, destination: &Path) -> Result<(), Whatever> {
    if destination.exists() {
        git2::Repository::open(destination)
            .whatever_context("expected existing workspace repo path to be a git repository")?;
        info!(
            "using existing workspace repo `{}`",
            destination.to_string_lossy()
        );
        return Ok(());
    }

    info!("cloning `{url}` into `{}`", destination.to_string_lossy());
    git2::Repository::clone(url, destination).whatever_context("unable to clone workspace repo")?;
    Ok(())
}

fn init_workspace(args: &Args, path: PathBuf, platform_dev: bool) -> Result<(), Whatever> {
    let root_path = root(args)?;
    let workspace_root = resolve_input_path(&path)?;
    std::fs::create_dir_all(&workspace_root)
        .whatever_context("unable to create workspace root directory")?;
    let workspace_root = workspace_root
        .canonicalize()
        .whatever_context("unable to canonicalize workspace root directory")?;

    // Workspace init is a local-dev bootstrap path, so it intentionally uses staging URLs.
    let workspace_config = Config::staging();
    let repository_use = workspace_config
        .locations
        .identify_repository(&root_path)
        .whatever_context("cannot identify repository use")?;

    let expected_root = workspace_root.join(&repository_use.title);
    if root_path != expected_root {
        snafu::whatever!(
            "workspace init expects the active repository at `{}`, found `{}`",
            expected_root.to_string_lossy(),
            root_path.to_string_lossy(),
        );
    }

    let (other_name, other_url) = repository_use
        .only_other_repo()
        .whatever_context("workspace init requires exactly one sibling repository")?;
    clone_missing_repo(other_url.as_str(), &workspace_root.join(other_name))?;
    clone_missing_repo(
        workspace_config.theme.repository.as_str(),
        &workspace_root.join(config::DEFAULT_THEME_DIR),
    )?;

    if platform_dev {
        clone_missing_repo(
            PLATFORM_PREPROCESSOR_URL,
            &workspace_root.join("preprocessor"),
        )?;
        clone_missing_repo(PLATFORM_EIPW_URL, &workspace_root.join("eipw"))?;
    }

    std::fs::create_dir_all(workspace_root.join(config::DEFAULT_BUILD_ROOT_BASE))
        .whatever_context("unable to create local build root")?;

    let config_path = workspace_root.join(config::LOCAL_CONFIG_FILE);
    if config_path.exists() {
        info!(
            "leaving existing workspace config `{}` in place",
            config_path.to_string_lossy()
        );
    } else {
        std::fs::write(&config_path, config::default_workspace_config_text())
            .whatever_context("unable to write workspace config")?;
    }

    Ok(())
}

fn run() -> Result<(), Whatever> {
    let args = Args::parse();

    if let Operation::Print { print } = &args.operation {
        print::print(print.clone());
        return Ok(());
    }

    if let Operation::Workspace { command } = &args.operation {
        match command.clone() {
            WorkspaceCommand::Init { path, platform_dev } => {
                init_workspace(&args, path, platform_dev)?
            }
            WorkspaceCommand::Refresh => refresh_workspace(&args)?,
            WorkspaceCommand::Doctor => doctor_workspace(&args)?,
        }
        return Ok(());
    }

    let resolved = resolve_execution(&args)?;
    let build_path = make_build_dir(&resolved.build_path)?;
    let mut lock_file = lock(&build_path)?;

    match args.operation {
        Operation::Print { .. } | Operation::Workspace { .. } => unreachable!(),
        Operation::Clean => {
            // TODO: There's a race condition here. Maybe we move the lockfile to the repository
            //       root?
            lock_file
                .unlock()
                .whatever_context("unable to unlock build directory")?;
            std::fs::remove_dir_all(&build_path)
                .whatever_context("unable to remove build directory")?;
            return Ok(());
        }
        Operation::Check { eipw } => {
            Prepared::prepare(eipw, resolved)?.check()?;
        }
        Operation::Build { eipw } => {
            Prepared::prepare(eipw, resolved)?.build()?;
        }
        Operation::Serve { eipw } => {
            Prepared::prepare(eipw, resolved)?.serve()?;
        }
        Operation::Changed { all, format } => {
            let repo_path = build_path.join(REPO_DIR);

            let both = git::Fresh::new(
                &resolved.root_path,
                &repo_path,
                resolved.repository_use.clone(),
                resolved.source_materialization,
            )
            .whatever_context("initializing build repo")?
            .clone_src()
            .whatever_context("cloning source repo")?
            .fetch_upstream()
            .whatever_context("fetching upstream repo")?;

            let changed_files: Vec<_> = both
                .changed_files()
                .whatever_context("unable to list changed files")?
                .into_iter()
                .filter(|p| all || Prepared::is_proposal_path(p.into()))
                .map(|p| repo_path.join(p))
                .collect();

            format.print(&changed_files, &repo_path);
        }
    }

    lock_file
        .unlock()
        .whatever_context("unable to unlock build directory")?;

    info!("build finished :3");
    Ok(())
}

fn main() -> Result<(), Report<Whatever>> {
    let logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    let level = logger.filter();
    progress::init(logger);
    log::set_max_level(level);

    let result = run().map_err(Report::from_error);

    progress::clear();

    result
}
