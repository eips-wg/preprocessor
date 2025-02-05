/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use eipw_lint::config::DefaultOptions;
use eipw_snippets::Message;

use clap::ValueEnum;
use log::debug;

use crate::cache::Cache;
use crate::progress::ProgressIteratorExt;

use eipw_lint::reporters::{AdditionalHelp, Count, Json, Reporter, Text};
use eipw_lint::Linter;

use figment::providers::{Format as _, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

use snafu::{ensure, Backtrace, IntoError, OptionExt, ResultExt, Snafu};

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("unable to construct eipw configuration"))]
    Config {
        source: figment::Error,
        backtrace: Backtrace,
    },
    #[snafu(display("no lint `{name}` is enabled"))]
    NoLint { name: String, backtrace: Backtrace },
    #[snafu(transparent)]
    Eipw {
        #[snafu(backtrace)]
        source: eipw_lint::Error,
    },
    #[snafu(display("validation failed with {n_errors} errors :("))]
    Failed { n_errors: usize },
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        backtrace: Backtrace,
        source: std::io::Error,
    },
    #[snafu(transparent)]
    Git {
        #[snafu(backtrace)]
        source: crate::git::Error,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    command: CmdArgs,

    #[serde(flatten)]
    eipw: eipw_lint::config::DefaultOptions,
}

#[derive(Debug, clap::Args, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CmdArgs {
    /// Disable linting entirely
    #[arg(long, exclusive(true))]
    no_lint: bool,

    /// Restrict linting to specific files and/or directories (relative to project root)
    #[clap(required(false))]
    sources: Vec<PathBuf>,

    /// Lint output format
    #[clap(long, value_enum, default_value_t)]
    format: Format,

    /// Do not enable the default lints
    #[clap(long)]
    no_default_lints: bool,

    /// Lints to enable as errors
    #[clap(long, short('D'))]
    deny: Vec<String>,

    /// Lints to enable as warnings
    #[clap(long, short('W'))]
    warn: Vec<String>,

    /// Lints to disable
    #[clap(long, short('A'))]
    allow: Vec<String>,
}

#[derive(ValueEnum, Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Format {
    Text,
    Json,
}

impl Default for Format {
    fn default() -> Self {
        Self::Text
    }
}

#[derive(Debug)]
enum EitherReporter {
    Text(Text<String>),
    Json(Json),
}

impl Reporter for EitherReporter {
    fn report(&self, snippet: Message<'_>) -> Result<(), eipw_lint::reporters::Error> {
        match self {
            Self::Text(s) => s.report(snippet),
            Self::Json(j) => j.report(snippet),
        }
    }
}

fn defaults() {
    let options = DefaultOptions::<String>::default();

    let output = toml::to_string_pretty(&options).unwrap();

    println!("{output}\n");
}

fn list_lints() {
    let options = DefaultOptions::<String>::default();
    println!("Available lints:");

    for (slug, _) in options.lints {
        println!("\t{}", slug);
    }

    println!();
}

async fn collect_sources(sources: Vec<PathBuf>) -> Result<Vec<PathBuf>, Error> {
    use std::ffi::OsStr;
    use tokio::fs;

    let mut output = Vec::with_capacity(sources.len());

    for source in sources.into_iter() {
        let metadata = fs::metadata(&source)
            .await
            .context(FsSnafu { path: &source })?;
        if metadata.is_file() {
            debug!("collecting `{}` for linting", source.to_string_lossy());
            output.push(source.clone());
        }

        if !metadata.is_dir() {
            debug!(
                "not collecting `{}` for linting (not directory)",
                source.to_string_lossy()
            );
            continue;
        }

        let mut entries = fs::read_dir(&source)
            .await
            .context(FsSnafu { path: &source })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .context(FsSnafu { path: &source })?
        {
            let mut path = entry.path();
            debug!("examining `{}` for linting", path.to_string_lossy());

            let metadata = fs::metadata(&path).await.context(FsSnafu { path: &path })?;
            if metadata.is_file() && path.extension() == Some(OsStr::new("md")) {
                debug!("collecting `{}` for linting", path.to_string_lossy());
                output.push(path);
                continue;
            }

            if !metadata.is_dir() {
                debug!(
                    "not collecting `{}` for linting (not directory)",
                    path.to_string_lossy()
                );
                continue;
            }

            path.push("index.md");

            debug!("examining `{}` for linting", path.to_string_lossy());
            let metadata = match fs::metadata(&path).await {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(FsSnafu { path: &path }.into_error(e)),
            };

            if metadata.is_file() {
                debug!("collecting `{}` for linting", path.to_string_lossy());
                output.push(path);
            }
        }
    }

    Ok(output)
}

#[tokio::main(flavor = "current_thread")]
pub async fn eipw(
    cache: &Cache,
    root_dir: &Path,
    repo_dir: &Path,
    changed_paths: Vec<PathBuf>,
    opts: CmdArgs,
) -> Result<(), Error> {
    if opts.no_lint {
        return Ok(());
    }

    let mut stdout = std::io::stdout();

    let mut config_path = cache.repo(
        crate::THEME_REPO,
        crate::THEME_REV,
    )?;

    config_path.push("config");
    config_path.push("eipw.toml");

    let config: Config = Figment::new()
        .merge(DefaultOptions::<String>::figment())
        .merge(Toml::file_exact(config_path))
        .merge(Serialized::global("command", opts))
        .extract()
        .context(ConfigSnafu)?;

    let opts = config.command;

    let paths = if opts.sources.is_empty() {
        changed_paths
    } else {
        let root_dir = tokio::fs::canonicalize(root_dir)
            .await
            .context(FsSnafu { path: root_dir })?;
        let repo_dir = tokio::fs::canonicalize(repo_dir)
            .await
            .context(FsSnafu { path: repo_dir })?;

        let mut repo_relative_sources = Vec::with_capacity(opts.sources.len());
        for source in &opts.sources {
            let root_relative_source = root_dir.join(source);
            let full_source = tokio::fs::canonicalize(&root_relative_source)
                .await
                .context(FsSnafu {
                    path: root_relative_source,
                })?;

            let relative_source = match full_source.strip_prefix(&root_dir) {
                Ok(r) => r,
                Err(e) => {
                    let err = std::io::Error::new(std::io::ErrorKind::NotFound, e);
                    return Err(FsSnafu { path: full_source }.into_error(err));
                }
            };

            repo_relative_sources.push(repo_dir.join(relative_source));
        }

        repo_relative_sources
    };

    let sources = collect_sources(paths).await?;

    let reporter = match opts.format {
        Format::Json => EitherReporter::Json(Json::default()),
        Format::Text => EitherReporter::Text(Text::default()),
    };

    let reporter = AdditionalHelp::new(reporter, |t: &str| {
        Ok(format!("see https://ethereum.github.io/eipw/{}/", t))
    });
    let reporter = Count::new(reporter);

    let mut linter = Linter::with_options(reporter, config.eipw);

    if opts.no_default_lints {
        linter = linter.clear_lints();
    }

    for allow in opts.allow {
        linter = linter.allow(&allow);
    }

    if !opts.warn.is_empty() {
        let defaults = DefaultOptions::<String>::default();
        let mut lints: HashMap<_, _> = defaults.lints;
        for warn in opts.warn {
            let (k, v) = lints
                .remove_entry(warn.as_str())
                .context(NoLintSnafu { name: warn })?;
            linter = linter.warn(k, v.into_lint().unwrap());
        }
    }

    if !opts.deny.is_empty() {
        let defaults = DefaultOptions::<String>::default();
        let mut lints: HashMap<_, _> = defaults.lints;
        for deny in opts.deny {
            let (k, v) = lints
                .remove_entry(deny.as_str())
                .context(NoLintSnafu { name: deny })?;
            linter = linter.deny(k, v.into_lint().unwrap());
        }
    }

    for source in sources.iter().progress_ext("Lint") {
        linter = linter.check_file(source);
    }

    let reporter = linter.run().await?;

    let n_errors = reporter.counts().error;

    match reporter.into_inner().into_inner() {
        EitherReporter::Json(j) => serde_json::to_writer_pretty(&stdout, &j).unwrap(),
        EitherReporter::Text(t) => write!(stdout, "{}", t.into_inner()).unwrap(),
    }

    ensure!(n_errors == 0, FailedSnafu { n_errors });

    Ok(())
}
