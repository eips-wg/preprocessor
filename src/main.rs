/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod cache;
mod changed;
mod cli;
mod config;
mod context;
mod find_root;
mod git;
mod github;
mod identity;
mod layout;
mod lint;
mod markdown;
mod print;
mod progress;
mod workspace;
mod zola;

use std::path::{Path, PathBuf};

use clap::Parser;
use fslock::LockFile;
use log::{debug, info};
use snafu::{Report, ResultExt, Whatever};

use crate::{
    cli::{Args, Operation},
    config::Config,
    layout::{BUILD_DIR, CONTENT_DIR, OUTPUT_DIR, REPO_DIR},
    workspace::{doctor_workspace, init_workspace},
};

fn repository_use(config: &Config, root_path: &Path) -> Result<git::RepositoryUse, Whatever> {
    let repo_id = config
        .locations
        .identify_repository_title(root_path)
        .whatever_context("cannot identify repository use")?;
    let Some(repository_use) = config.locations.repository_use_for_title(&repo_id) else {
        snafu::whatever!("repository metadata for `{repo_id}` is unavailable");
    };
    Ok(repository_use)
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

        let repository_use = repository_use(&config, &root_path)?;
        let both = git::Fresh::new(
            &root_path,
            &repo_path,
            repository_use,
            git::SourceMaterialization::Clean,
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
            .filter(|p| changed::is_proposal_path(p.into()))
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
        let repository_use = repository_use(&self.config, &self.root_path)?;
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

    if let Operation::Init { path, template } = &args.operation {
        init_workspace(&args, path.clone(), *template)?;
        return Ok(());
    }

    if let Operation::Doctor = &args.operation {
        doctor_workspace(&args)?;
        return Ok(());
    }

    let config = if args.staging {
        Config::staging()
    } else {
        Config::production()
    };

    let root_path = context::root(&args)?;
    let build_path = make_build_dir(&root_path)?;

    let mut lock_file = lock(&build_path)?;

    match args.operation {
        Operation::Print { .. } => unreachable!(),
        Operation::Init { .. } => unreachable!(),
        Operation::Doctor => unreachable!(),
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
            changed::run(&root_path, &build_path, &config, all, &format)?;
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
