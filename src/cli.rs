/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Clap command surface and command helper methods.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use url::Url;

use crate::print;

/// Build script for Ethereum EIPs and ERCs.
#[derive(Parser, Debug)]
#[command(version, about)]
pub(crate) struct Args {
    /// Use ROOT as the base directory (instead of finding it automatically)
    #[clap(short = 'C')]
    pub(crate) root: Option<PathBuf>,

    /// Use the configured remote sibling content repositories
    #[clap(long)]
    pub(crate) remote_siblings: bool,

    /// Write build artifacts under BUILD_ROOT instead of the default location
    #[clap(long)]
    pub(crate) build_root: Option<PathBuf>,

    #[clap(subcommand)]
    pub(crate) operation: Operation,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub(crate) struct ServerCliArgs {
    /// Host/interface for the local server to bind
    #[arg(long)]
    pub(crate) host: Option<String>,

    /// Port for the local server to bind
    #[arg(long)]
    pub(crate) port: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub(crate) struct BaseUrlCliArgs {
    /// Override the rendered-site base URL for this command
    #[arg(long, value_parser = clap::value_parser!(Url))]
    pub(crate) base_url: Option<Url>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, clap::Args)]
pub(crate) struct CleanCliArgs {
    /// Ignore tracked working-tree changes in the active repo
    #[arg(long)]
    pub(crate) clean: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum Operation {
    /// Print various useful things, like available lints
    Print {
        #[command(flatten)]
        print: print::CmdArgs,
    },

    /// Build the project and output HTML
    Build {
        #[command(flatten)]
        base_url: BaseUrlCliArgs,

        #[command(flatten)]
        clean: CleanCliArgs,
    },

    /// Build the project and launch a web server to preview it
    Serve {
        #[command(flatten)]
        server: ServerCliArgs,

        #[command(flatten)]
        base_url: BaseUrlCliArgs,

        #[command(flatten)]
        clean: CleanCliArgs,
    },

    /// Remove temporary and output files
    Clean,

    /// Analyze the repository and report errors, but don't build HTML files
    Check {
        #[command(flatten)]
        clean: CleanCliArgs,
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

    /// Check workspace layout, local repos, and required tools
    Doctor,
}

#[derive(Debug, Clone)]
pub(crate) enum RuntimeOperation {
    Build,
    Serve,
    Clean,
    Check,
    Changed { all: bool, format: ChangedFormat },
}

impl Operation {
    pub(crate) fn server_cli_args(&self) -> ServerCliArgs {
        match self {
            Self::Serve { server, .. } => server.clone(),
            _ => ServerCliArgs::default(),
        }
    }

    pub(crate) fn base_url_cli_args(&self) -> BaseUrlCliArgs {
        match self {
            Self::Build { base_url, .. } | Self::Serve { base_url, .. } => base_url.clone(),
            _ => BaseUrlCliArgs::default(),
        }
    }

    pub(crate) fn clean_cli_args(&self) -> CleanCliArgs {
        match self {
            Self::Build { clean, .. } | Self::Serve { clean, .. } | Self::Check { clean } => clean.clone(),
            _ => CleanCliArgs::default(),
        }
    }

    pub(crate) fn is_plain_site_command(&self) -> bool {
        matches!(self, Self::Build { .. } | Self::Serve { .. } | Self::Check { .. })
    }

    pub(crate) fn runtime_operation(&self) -> Option<RuntimeOperation> {
        match self {
            Self::Print { .. } | Self::Init { .. } | Self::Doctor => None,
            Self::Build { .. } => Some(RuntimeOperation::Build),
            Self::Serve { .. } => Some(RuntimeOperation::Serve),
            Self::Clean => Some(RuntimeOperation::Clean),
            Self::Check { .. } => Some(RuntimeOperation::Check),
            Self::Changed { all, format } => Some(RuntimeOperation::Changed { all: *all, format: format.clone() }),
        }
    }

    pub(crate) fn is_workspace_lifecycle_command(&self) -> bool {
        matches!(self, Self::Init { .. } | Self::Doctor)
    }

    pub(crate) fn is_print_command(&self) -> bool {
        matches!(self, Self::Print { .. })
    }
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
