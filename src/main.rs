/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

mod markdown;
mod find_root;

use std::path::PathBuf;

use clap::Parser;
use snafu::{Report, ResultExt, Whatever};

const CONTENT_DIR: &str = "content";

/// Build script for Ethereum EIPs and ERCs.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Use ROOT as the base directory (instead of finding it automatically)
    #[clap(short = 'C')]
    root: Option<PathBuf>,
}

fn run() -> Result<(), Whatever> {
    let args = Args::parse();

    let root = match args.root {
        None => find_root::find_root().whatever_context("cannot find repository root")?,
        Some(p) => p,
    };
    find_root::is_root(&root).whatever_context("invalid root directory")?;

    let content_path = root.join(CONTENT_DIR);
    markdown::preprocess(&content_path).whatever_context("unable to preprocess markdown")?;

    Ok(())
}

fn main() -> Result<(), Report<Whatever>> {
    run().map_err(Report::from_error)
}
