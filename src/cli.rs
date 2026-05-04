/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Clap command surface and command helper methods.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::{lint, print};

/// Build script for Ethereum EIPs and ERCs.
#[derive(Parser, Debug)]
#[command(version, about)]
pub(crate) struct Args {
    /// Use ROOT as the base directory (instead of finding it automatically)
    #[clap(short = 'C')]
    pub(crate) root: Option<PathBuf>,

    /// Use the staging repositories (for testing)
    #[clap(long = "staging")]
    pub(crate) staging: bool,

    #[clap(subcommand)]
    pub(crate) operation: Operation,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Operation {
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

    /// Create workspace config, docs, build root, and missing local repos
    Init {
        /// Workspace root directory
        path: PathBuf,

        /// Also clone template for proposal-family scaffold work
        #[arg(long)]
        template: bool,
    },
}

#[derive(Debug, clap::ValueEnum, Clone, Default)]
pub(crate) enum ChangedFormat {
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

    pub(crate) fn print(&self, files: &[PathBuf], repo_path: &Path) {
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
