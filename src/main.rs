/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod changed;
mod cli;
mod config;
mod context;
mod execution;
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
    cli::{Args, Operation, RuntimeOperation},
    execution::{resolve_execution, validate_non_execution_command_flags, ResolvedExecution},
    layout::{output_path, CONTENT_DIR, REPO_DIR},
    workspace::{doctor_workspace, init_workspace},
};

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

fn make_build_dir(build_path: &Path) -> Result<PathBuf, Whatever> {
    if let Err(e) = std::fs::create_dir_all(build_path) {
        debug!(
            "got while creating build directory: {}",
            Report::from_error(e)
        );
    }
    Ok(build_path.to_path_buf())
}

#[derive(Debug)]
struct Prepared {
    repo_path: PathBuf,
    output_path: PathBuf,
    repository_use: git::RepositoryUse,
    theme_path: PathBuf,
    base_url_override: Option<url::Url>,
}

impl Prepared {
    fn prepare(eipw: lint::CmdArgs, resolved: ResolvedExecution) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;
        let theme_path = resolved.theme_path()?.to_path_buf();

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme_path: _,
            source_materialization,
            base_url_override,
        } = resolved;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = output_path(&build_path);

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
            .filter(|p| changed::is_proposal_path(p.into()))
            .map(|p| repo_path.join(p))
            .collect();

        both.merge()
            .whatever_context("unable to merge ERC/EIP repositories")?;

        lint::eipw(&theme_path, &root_path, &repo_path, changed_files, eipw)
            .whatever_context("linting failed")?;

        markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;

        Ok(Prepared {
            repo_path,
            output_path,
            repository_use,
            theme_path,
            base_url_override,
        })
    }

    fn build(self) -> Result<(), Whatever> {
        let base_url = self
            .base_url_override
            .as_ref()
            .unwrap_or(&self.repository_use.location.base_url);
        zola::build(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        Ok(())
    }

    fn serve(self) -> Result<(), Whatever> {
        zola::serve(&self.theme_path, &self.repo_path, &self.output_path)
            .whatever_context("zola serve failed")?;
        Ok(())
    }

    fn check(self) -> Result<(), Whatever> {
        zola::check(&self.theme_path, &self.repo_path).whatever_context("zola check failed")?;
        Ok(())
    }
}

fn run() -> Result<(), Whatever> {
    let args = Args::parse();
    validate_non_execution_command_flags(&args)?;

    if let Operation::Print { print } = &args.operation {
        print::print(print.clone());
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

    let runtime_operation = args
        .operation
        .runtime_operation()
        .expect("non-execution commands should have returned earlier");
    let resolved = resolve_execution(&args)?;
    let build_path = make_build_dir(&resolved.build_path)?;

    let mut lock_file = lock(&build_path)?;

    match runtime_operation {
        RuntimeOperation::Clean => {
            // TODO: There's a race condition here. Maybe we move the lockfile to the repository
            //       root?
            lock_file
                .unlock()
                .whatever_context("unable to unlock build directory")?;
            std::fs::remove_dir_all(build_path)
                .whatever_context("unable to remove build directory")?;
            return Ok(());
        }
        RuntimeOperation::Check { eipw } => {
            Prepared::prepare(eipw, resolved)?.check()?;
        }
        RuntimeOperation::Build { eipw } => {
            Prepared::prepare(eipw, resolved)?.build()?;
        }
        RuntimeOperation::Serve { eipw } => {
            Prepared::prepare(eipw, resolved)?.serve()?;
        }
        RuntimeOperation::Changed { all, format } => {
            changed::run(&resolved, &build_path, all, &format)?;
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
