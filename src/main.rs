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
use snafu::{Report, ResultExt, Whatever};

use crate::config::Config;

const CONTENT_DIR: &str = "content";
const BUILD_DIR: &str = "build";
const REPO_DIR: &str = "repo";
const OUTPUT_DIR: &str = "output";

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
}

#[derive(Debug, clap::ValueEnum, Clone, Default)]
enum ChangedFormat {
    #[default]
    Newline,
    Nul,
    Json,
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

fn root(args: &Args) -> Result<PathBuf, Whatever> {
    let dir = match &args.root {
        None => find_root::find_root().whatever_context("cannot find repository root")?,
        Some(p) => p.to_path_buf(),
    };
    find_root::is_root(&dir).whatever_context("invalid root directory")?;
    Ok(dir)
}

fn make_build_dir(root: &Path) -> Result<PathBuf, Whatever> {
    let build_path = root.join(BUILD_DIR);
    if let Err(e) = std::fs::create_dir_all(&build_path) {
        debug!(
            "got while creating build directory: {}",
            Report::from_error(e)
        );
    }
    Ok(build_path)
}

#[derive(Debug)]
struct Prepared {
    cache: cache::Cache,
    root_path: PathBuf,
    repo_path: PathBuf,
    output_path: PathBuf,
    config: Config,
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

    fn prepare(
        eipw: lint::CmdArgs,
        config: Config,
        root_path: PathBuf,
        build_path: PathBuf,
    ) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = build_path.join(OUTPUT_DIR);

        let both = git::Fresh::new(&root_path, &repo_path, &config.locations)
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

        lint::eipw(
            config.theme.repository.as_str(),
            &config.theme.commit,
            &cache,
            &root_path,
            &repo_path,
            changed_files,
            eipw,
        )
        .whatever_context("linting failed")?;

        markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;

        Ok(Prepared {
            config,
            root_path,
            cache,
            repo_path,
            output_path,
        })
    }

    fn build(self) -> Result<(), Whatever> {
        let repository_use = self
            .config
            .locations
            .identify_repository(&self.root_path)
            .whatever_context("cannot identify repository use")?;
        zola::build(
            self.config.theme.repository.as_str(),
            &self.config.theme.commit,
            &self.cache,
            &self.repo_path,
            &self.output_path,
            repository_use.location.base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        Ok(())
    }

    fn serve(self) -> Result<(), Whatever> {
        zola::serve(
            self.config.theme.repository.as_str(),
            &self.config.theme.commit,
            &self.cache,
            &self.repo_path,
            &self.output_path,
        )
        .whatever_context("zola serve failed")?;
        Ok(())
    }

    fn check(self) -> Result<(), Whatever> {
        zola::check(
            self.config.theme.repository.as_str(),
            &self.config.theme.commit,
            &self.cache,
            &self.repo_path,
        )
        .whatever_context("zola check failed")?;
        Ok(())
    }
}

fn run() -> Result<(), Whatever> {
    let args = Args::parse();
    if let Operation::Print { print } = args.operation {
        print::print(print);
        return Ok(());
    }

    let config = if args.staging {
        Config::staging()
    } else {
        Config::production()
    };

    let root_path = root(&args)?;
    let build_path = make_build_dir(&root_path)?;

    let mut lock_file = lock(&build_path)?;

    match args.operation {
        Operation::Print { .. } => unreachable!(),
        Operation::Clean => {
            // TODO: There's a race condition here. Maybe we move the lockfile to the repository
            //       root?
            lock_file
                .unlock()
                .whatever_context("unable to unlock build directory")?;
            std::fs::remove_dir_all(build_path)
                .whatever_context("unable to remove build directory")?;
            return Ok(());
        }
        Operation::Check { eipw } => {
            Prepared::prepare(eipw, config, root_path, build_path)?.check()?;
        }
        Operation::Build { eipw } => {
            Prepared::prepare(eipw, config, root_path, build_path)?.build()?;
        }
        Operation::Serve { eipw } => {
            Prepared::prepare(eipw, config, root_path, build_path)?.serve()?;
        }
        Operation::Changed { all, format } => {
            let repo_path = build_path.join(REPO_DIR);

            let both = git::Fresh::new(&root_path, &repo_path, &config.locations)
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
