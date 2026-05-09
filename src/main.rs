/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod changed;
mod cli;
mod config;
mod context;
mod editorial;
mod execution;
mod find_root;
mod git;
mod github;
mod identity;
mod layout;
mod lint;
mod markdown;
mod pipeline;
mod preview;
mod print;
mod progress;
mod proposal;
mod serve;
mod workspace;
mod zola;

use std::path::{Path, PathBuf};

use clap::Parser;
use fslock::LockFile;
use log::{debug, info};
use snafu::{Report, ResultExt, Whatever};

use crate::{
    cli::{Args, EditorialCommand, Operation, RuntimeOperation},
    editorial::{editorial_runtime_execution, run_editorial_lint},
    execution::{resolve_execution, validate_non_execution_command_flags},
    layout::output_path,
    pipeline::Prepared,
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

    if matches!(runtime_operation, RuntimeOperation::Preview) {
        preview::serve(&output_path(&resolved.build_path), &resolved.server_binding)
            .whatever_context("preview server failed")?;
        return Ok(());
    }

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
        RuntimeOperation::Check => {
            Prepared::prepare(resolved)?.check()?;
        }
        RuntimeOperation::Build => {
            Prepared::prepare(resolved)?.build()?;
        }
        RuntimeOperation::Serve => {
            Prepared::prepare(resolved)?.serve()?;
        }
        RuntimeOperation::Preview => unreachable!(),
        RuntimeOperation::Changed { all, format } => {
            changed::run(&resolved, &build_path, all, &format)?;
        }
        RuntimeOperation::Editorial { command } => match command {
            EditorialCommand::Lint { selectors, eipw } => {
                run_editorial_lint(&resolved, &selectors, eipw)?;
            }
            EditorialCommand::Check { selectors, eipw } => {
                run_editorial_lint(&resolved, &selectors, eipw.clone())?;
                Prepared::prepare(editorial_runtime_execution(resolved, &selectors))?.check()?;
            }
        },
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
