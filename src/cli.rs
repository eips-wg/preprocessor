/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Clap command surface and command helper methods.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use url::Url;

use crate::{lint, print};

/// Build script for Ethereum EIPs and ERCs.
#[derive(Parser, Debug)]
#[command(version, about)]
pub(crate) struct Args {
    /// Use ROOT as the base directory (instead of finding it automatically)
    #[clap(short = 'C')]
    pub(crate) root: Option<PathBuf>,

    /// Force the staging repositories and base URLs
    #[clap(long)]
    pub(crate) staging: bool,

    /// Force the production repositories and base URLs
    #[clap(long)]
    pub(crate) production: bool,

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
    /// Print linter schema metadata and lint configuration
    Print {
        #[command(flatten)]
        print: print::CmdArgs,
    },

    /// Build the project and output HTML
    Build {
        #[command(flatten)]
        eipw: lint::CmdArgs,

        #[command(flatten)]
        base_url: BaseUrlCliArgs,

        #[command(flatten)]
        clean: CleanCliArgs,
    },

    /// Build the project and launch a web server to preview it
    Serve {
        #[command(flatten)]
        eipw: lint::CmdArgs,

        #[command(flatten)]
        base_url: BaseUrlCliArgs,

        #[command(flatten)]
        clean: CleanCliArgs,
    },

    /// Remove the selected build directory and generated output
    Clean,

    /// Validate that the site builds cleanly without writing HTML output
    Check {
        #[command(flatten)]
        eipw: lint::CmdArgs,

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

    /// Run build, serve, or check with staging remote proposal sources
    Parity {
        #[command(subcommand)]
        command: ProfiledOperation,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub(crate) enum ProfiledOperation {
    /// Build the project and output HTML
    Build {
        #[command(flatten)]
        eipw: lint::CmdArgs,

        #[command(flatten)]
        base_url: BaseUrlCliArgs,
    },

    /// Build the project and launch a web server to preview it
    Serve {
        #[command(flatten)]
        eipw: lint::CmdArgs,

        #[command(flatten)]
        base_url: BaseUrlCliArgs,
    },

    /// Validate that the site builds cleanly without writing HTML output
    Check {
        #[command(flatten)]
        eipw: lint::CmdArgs,
    },
}

#[derive(Debug, clap::ValueEnum, Clone, Default)]
pub(crate) enum ChangedFormat {
    #[default]
    Newline,
    Nul,
    Json,
}

#[derive(Debug, Clone)]
pub(crate) enum RuntimeOperation {
    Build { eipw: lint::CmdArgs },
    Serve { eipw: lint::CmdArgs },
    Clean,
    Check { eipw: lint::CmdArgs },
    Changed { all: bool, format: ChangedFormat },
}

impl Operation {
    pub(crate) fn base_url_cli_args(&self) -> BaseUrlCliArgs {
        match self {
            Self::Build { base_url, .. } | Self::Serve { base_url, .. } => base_url.clone(),
            Self::Parity { command } => command.base_url_cli_args(),
            Self::Print { .. }
            | Self::Clean
            | Self::Check { .. }
            | Self::Changed { .. }
            | Self::Init { .. }
            | Self::Doctor => BaseUrlCliArgs::default(),
        }
    }

    pub(crate) fn clean_cli_args(&self) -> CleanCliArgs {
        match self {
            Self::Build { clean, .. } | Self::Serve { clean, .. } | Self::Check { clean, .. } => {
                clean.clone()
            }
            Self::Print { .. }
            | Self::Clean
            | Self::Changed { .. }
            | Self::Init { .. }
            | Self::Doctor
            | Self::Parity { .. } => CleanCliArgs::default(),
        }
    }

    pub(crate) fn is_plain_site_command(&self) -> bool {
        matches!(
            self,
            Self::Build { .. } | Self::Serve { .. } | Self::Check { .. }
        )
    }

    pub(crate) fn runtime_operation(&self) -> Option<RuntimeOperation> {
        match self {
            Self::Print { .. } | Self::Init { .. } | Self::Doctor => None,
            Self::Build { eipw, .. } => Some(RuntimeOperation::Build { eipw: eipw.clone() }),
            Self::Serve { eipw, .. } => Some(RuntimeOperation::Serve { eipw: eipw.clone() }),
            Self::Clean => Some(RuntimeOperation::Clean),
            Self::Check { eipw, .. } => Some(RuntimeOperation::Check { eipw: eipw.clone() }),
            Self::Changed { all, format } => Some(RuntimeOperation::Changed {
                all: *all,
                format: format.clone(),
            }),
            Self::Parity { command } => Some(command.runtime_operation()),
        }
    }

    pub(crate) fn is_workspace_lifecycle_command(&self) -> bool {
        matches!(self, Self::Init { .. } | Self::Doctor)
    }

    pub(crate) fn is_print_command(&self) -> bool {
        matches!(self, Self::Print { .. })
    }
}

impl ProfiledOperation {
    fn base_url_cli_args(&self) -> BaseUrlCliArgs {
        match self {
            Self::Build { base_url, .. } | Self::Serve { base_url, .. } => base_url.clone(),
            Self::Check { .. } => BaseUrlCliArgs::default(),
        }
    }

    fn runtime_operation(&self) -> RuntimeOperation {
        match self {
            Self::Build { eipw, .. } => RuntimeOperation::Build { eipw: eipw.clone() },
            Self::Serve { eipw, .. } => RuntimeOperation::Serve { eipw: eipw.clone() },
            Self::Check { eipw } => RuntimeOperation::Check { eipw: eipw.clone() },
        }
    }
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Args, Operation, ProfiledOperation, RuntimeOperation};

    fn parse_args(arguments: &[&str]) -> Args {
        Args::try_parse_from(arguments).unwrap()
    }

    #[test]
    fn parity_command_parses_as_command_prefix() {
        let args = parse_args(&["build-eips", "parity", "build"]);

        assert!(matches!(
            args.operation,
            Operation::Parity {
                command: ProfiledOperation::Build { .. }
            }
        ));
    }

    #[test]
    fn profile_flag_is_rejected() {
        let error =
            Args::try_parse_from(["build-eips", "--profile", "local", "build"]).unwrap_err();

        assert!(error
            .to_string()
            .contains("unexpected argument '--profile'"));
    }

    #[test]
    fn removed_theme_flag_is_rejected() {
        let removed_flag = concat!("--remote", "-theme");
        let error = Args::try_parse_from(["build-eips", removed_flag, "build"]).unwrap_err();

        assert!(error
            .to_string()
            .contains(&format!("unexpected argument '{removed_flag}'")));
    }

    #[test]
    fn base_url_flags_parse_on_build_and_serve_forms() {
        let cases: &[(&[&str], &str)] = &[
            (
                &["build-eips", "build", "--base-url", "http://localhost:4000"],
                "build",
            ),
            (
                &["build-eips", "serve", "--base-url", "http://localhost:4000"],
                "serve",
            ),
            (
                &[
                    "build-eips",
                    "parity",
                    "build",
                    "--base-url",
                    "http://localhost:4000",
                ],
                "build",
            ),
            (
                &[
                    "build-eips",
                    "parity",
                    "serve",
                    "--base-url",
                    "http://localhost:4000",
                ],
                "serve",
            ),
        ];

        for (arguments, expected_runtime_operation) in cases {
            let args = parse_args(arguments);

            assert!(matches!(
                (
                    args.operation.runtime_operation().unwrap(),
                    *expected_runtime_operation
                ),
                (RuntimeOperation::Build { .. }, "build")
                    | (RuntimeOperation::Serve { .. }, "serve")
            ));
            assert_eq!(
                args.operation
                    .base_url_cli_args()
                    .base_url
                    .as_ref()
                    .unwrap()
                    .as_str(),
                "http://localhost:4000/"
            );
        }
    }

    #[test]
    fn clean_flags_parse_only_on_plain_site_commands() {
        for arguments in [
            &["build-eips", "build", "--clean"][..],
            &["build-eips", "serve", "--clean"][..],
            &["build-eips", "check", "--clean"][..],
        ] {
            let args = parse_args(arguments);
            assert!(args.operation.clean_cli_args().clean);
        }

        for arguments in [
            &["build-eips", "parity", "build", "--clean"][..],
            &["build-eips", "parity", "serve", "--clean"][..],
            &["build-eips", "parity", "check", "--clean"][..],
            &["build-eips", "changed", "--clean"][..],
            &["build-eips", "clean", "--clean"][..],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn removed_command_surface_is_rejected() {
        for arguments in [
            &["build-eips", "dirty", "build"][..],
            &["build-eips", "--allow-dirty", "build"][..],
            &["build-eips", "--no-allow-dirty", "build"][..],
            &["build-eips", "--no-staging", "build"][..],
            &["build-eips", "--remote-sibling-repo", "build"][..],
            &["build-eips", "workspace", "init", "/tmp/workspace"][..],
            &["build-eips", "workspace", "doctor"][..],
            &["build-eips", "parity", "clean"][..],
            &["build-eips", "parity", "changed"][..],
        ] {
            assert!(Args::try_parse_from(arguments).is_err());
        }
    }

    #[test]
    fn base_url_flag_is_rejected_on_non_rendering_forms() {
        let cases: &[&[&str]] = &[
            &["build-eips", "check", "--base-url", "http://localhost:4000"],
            &[
                "build-eips",
                "changed",
                "--base-url",
                "http://localhost:4000",
            ],
            &[
                "build-eips",
                "doctor",
                "--base-url",
                "http://localhost:4000",
            ],
            &[
                "build-eips",
                "init",
                "/tmp/workspace",
                "--base-url",
                "http://localhost:4000",
            ],
            &["build-eips", "print", "--base-url", "http://localhost:4000"],
        ];

        for arguments in cases {
            assert!(Args::try_parse_from(*arguments).is_err());
        }
    }

    #[test]
    fn workspace_lifecycle_commands_parse() {
        let plain = parse_args(&["build-eips", "init", "/tmp/workspace"]);
        let template = parse_args(&["build-eips", "init", "/tmp/workspace", "--template"]);
        let doctor = parse_args(&["build-eips", "doctor"]);

        assert!(matches!(
            plain.operation,
            Operation::Init {
                template: false,
                ..
            }
        ));
        assert!(matches!(
            template.operation,
            Operation::Init { template: true, .. }
        ));
        assert!(matches!(doctor.operation, Operation::Doctor));
    }

    #[test]
    fn remote_siblings_flag_parses() {
        let args = parse_args(&["build-eips", "--remote-siblings", "build"]);

        assert!(args.remote_siblings);
    }

    #[test]
    fn explicit_workspace_config_path_is_not_accepted() {
        let error = Args::try_parse_from(["build-eips", "--config", "/tmp/config.toml", "build"])
            .unwrap_err();

        assert!(error.to_string().contains("unexpected argument '--config'"));
    }
}
