/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    ffi::OsString,
    io::{BufRead, BufReader, ErrorKind},
    path::{Path, PathBuf},
};

use log::{debug, error, info, warn};
use semver::Version;
use snafu::{ensure, Backtrace, IntoError, Report, ResultExt, Snafu};
use url::Url;

use crate::{cache::Cache, git};

const MINIMUM_VERSION: Version = Version::new(0, 19, 2);

fn symlink_dir(original: &Path, link: &Path) -> Result<(), std::io::Error> {
    #[cfg(target_family = "windows")]
    {
        std::os::windows::fs::symlink_dir(original, link)
    }
    #[cfg(target_family = "unix")]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(not(any(target_family = "unix", target_family = "windows")))]
    {
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "no symlink_dir implementation available",
        ))
    }
}

fn force_symlink_dir(original: &Path, link: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(link) {
        Ok(()) => (),
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }

    symlink_dir(original, link)
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("could not find zola binary (requires at least version {MINIMUM_VERSION})"))]
    Missing {
        backtrace: Backtrace,
        source: std::io::Error,
    },
    #[snafu(display(
        "installed zola version is too old (requires at least {MINIMUM_VERSION}, got {got})"
    ))]
    TooOld { got: Version, backtrace: Backtrace },
    #[snafu(context(false))]
    Semver { source: semver::Error },
    #[snafu(display("i/o error"))]
    Io {
        backtrace: Backtrace,
        source: std::io::Error,
    },
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        backtrace: Backtrace,
        source: std::io::Error,
    },
    #[snafu(context(false))]
    Git {
        #[snafu(backtrace)]
        source: git::Error,
    },
}

pub fn find_zola() -> Result<(), Error> {
    let text = match duct::cmd!("zola", "--version").stdin_null().read() {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(MissingSnafu.into_error(e)),
        Err(e) => return Err(IoSnafu.into_error(e)),
    };

    let version_text = text
        .strip_prefix("zola ")
        .expect("weird zola output")
        .trim();
    let version: Version = version_text.parse()?;

    ensure!(version >= MINIMUM_VERSION, TooOldSnafu { got: version });

    Ok(())
}

pub fn check(
    theme_repo: &str,
    theme_rev: &str,
    cache: &Cache,
    project_path: &Path,
) -> Result<(), Error> {
    let args = ["check", "--drafts", "--skip-external-links"];
    spawn_log(theme_repo, theme_rev, cache, project_path, args)?;
    Ok(())
}

pub fn build(
    theme_repo: &str,
    theme_rev: &str,
    cache: &Cache,
    project_path: &Path,
    output_path: &Path,
    base_url: &str,
) -> Result<(), Error> {
    remove_output(output_path);
    let args = ["build", "--drafts", "-u", base_url, "-o"]
        .map(OsString::from)
        .into_iter()
        .chain(std::iter::once(output_path.into()));
    spawn_log(theme_repo, theme_rev, cache, project_path, args)?;
    if let Ok(url) = Url::from_file_path(output_path) {
        info!("HTML output to: {}", url);
    }
    Ok(())
}

pub fn serve(
    theme_repo: &str,
    theme_rev: &str,
    cache: &Cache,
    project_path: &Path,
    output_path: &Path,
) -> Result<(), Error> {
    // TODO: Properly kill the child process when we receive ctrl-c.
    warn!("live reloading is not implemented");
    remove_output(output_path);
    let args = ["serve", "--drafts", "-o"]
        .map(OsString::from)
        .into_iter()
        .chain(std::iter::once(output_path.into()));
    spawn_log(theme_repo, theme_rev, cache, project_path, args)?;
    Ok(())
}

fn remove_output(output_path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(output_path) {
        debug!(
            "got while removing output directory: {}",
            Report::from_error(e)
        );
    }
}

fn spawn_log<U, I>(
    theme_repo: &str,
    theme_rev: &str,
    cache: &Cache,
    project_path: &Path,
    args: U,
) -> Result<(), Error>
where
    U: IntoIterator<Item = I>,
    I: Into<OsString>,
{
    info!("invoking zola");
    debug!(
        "zola project directory is `{}`",
        project_path.to_string_lossy()
    );

    find_zola()?;

    let theme_dir = cache.repo(theme_repo, theme_rev)?;

    let mut themes_dir = project_path.join("themes");
    if let Err(e) = std::fs::create_dir(&themes_dir) {
        debug!("got while creating themes dir: {}", Report::from_error(e));
    }
    themes_dir.push("eips-theme");
    force_symlink_dir(&theme_dir, &themes_dir).context(FsSnafu { path: &themes_dir })?;

    let config_path: PathBuf = [&theme_dir, Path::new("config"), Path::new("zola.toml")]
        .iter()
        .collect();

    let prefix = [OsString::from("-c"), config_path.into()].into_iter();
    let args = prefix.chain(args.into_iter().map(Into::into));
    let reader = duct::cmd("zola", args)
        .dir(project_path)
        .stdin_null()
        .stderr_to_stdout()
        .reader()
        .context(IoSnafu)?;

    let mut buf = BufReader::new(reader);
    let mut line = String::new();

    while buf.read_line(&mut line).context(IoSnafu)? > 0 {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(warning) = trimmed.strip_prefix("Warning: ") {
            warn!("{}", warning);
        } else if let Some(error) = trimmed.strip_prefix("Error: ") {
            error!("{}", error);
        } else {
            info!("{}", trimmed);
        }
        line.clear();
    }

    buf.into_inner()
        .try_wait()
        .context(IoSnafu)?
        .expect("zola should have exited");

    Ok(())
}
