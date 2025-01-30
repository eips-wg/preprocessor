/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod cache;
mod find_root;
mod git;
mod markdown;
mod progress;
mod zola;

use std::path::{Path, PathBuf};

use clap::Parser;
use fslock::LockFile;
use log::{debug, info};
use snafu::{Report, ResultExt, Whatever};

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

fn run() -> Result<(), Whatever> {
    let args = Args::parse();

    zola::check().whatever_context("unable to find suitable zola binary")?;

    let root_path = root(&args)?;
    let build_path = make_build_dir(&root_path)?;
    let repo_path = build_path.join(REPO_DIR);
    let content_path = repo_path.join(CONTENT_DIR);
    let output_path = build_path.join(OUTPUT_DIR);

    let mut lock_file = lock(&build_path)?;

    git::merge_repositories(&root_path, &repo_path)
        .whatever_context("unable to merge EIP/ERC repositories")?;

    markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;

    let cache = cache::Cache::open().whatever_context("unable to open cache")?;

    zola::build(&cache, &repo_path, &output_path).whatever_context("zola build failed")?;

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
