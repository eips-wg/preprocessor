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

use crate::{
    config::ServerBinding,
    layout::{mounted_theme_path, theme_config_path},
};

const MINIMUM_VERSION: Version = Version::new(0, 22, 1);

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
    match std::fs::symlink_metadata(link) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(link)?;
        }
        Ok(_) => std::fs::remove_file(link)?,
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e),
    }

    symlink_dir(original, link)
}

fn mount_theme(theme_dir: &Path, project_path: &Path) -> Result<PathBuf, std::io::Error> {
    let mounted_theme_path = mounted_theme_path(project_path);
    if theme_dir == mounted_theme_path {
        return Ok(mounted_theme_path);
    }

    if let Some(parent) = mounted_theme_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    force_symlink_dir(theme_dir, &mounted_theme_path)?;
    Ok(mounted_theme_path)
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

pub fn check(theme_dir: &Path, project_path: &Path) -> Result<(), Error> {
    let args = ["check", "--drafts", "--skip-external-links"];
    spawn_log(theme_dir, project_path, args)?;
    Ok(())
}

pub fn build(
    theme_dir: &Path,
    project_path: &Path,
    output_path: &Path,
    base_url: &str,
) -> Result<(), Error> {
    remove_output(output_path);
    let args = ["build", "--drafts", "-u", base_url, "-o"]
        .map(OsString::from)
        .into_iter()
        .chain(std::iter::once(output_path.into()));
    spawn_log(theme_dir, project_path, args)?;
    if let Ok(url) = Url::from_file_path(output_path) {
        info!("HTML output to: {}", url);
    }
    Ok(())
}

pub fn serve(
    theme_dir: &Path,
    project_path: &Path,
    output_path: &Path,
    server_binding: &ServerBinding,
    base_url_override: Option<&Url>,
) -> Result<(), Error> {
    // TODO: Properly kill the child process when we receive ctrl-c.
    remove_output(output_path);
    let args = serve_args(server_binding, output_path, base_url_override);
    spawn_log(theme_dir, project_path, args)?;
    Ok(())
}

fn serve_args(
    server_binding: &ServerBinding,
    output_path: &Path,
    base_url_override: Option<&Url>,
) -> Vec<OsString> {
    let mut args = [
        "serve",
        "--drafts",
        "--fast",
        "--force",
        "--interface",
        server_binding.host.as_str(),
        "--port",
    ]
    .map(OsString::from)
    .to_vec();

    args.push(OsString::from(server_binding.port.to_string()));

    if let Some(base_url) = base_url_override {
        args.extend([
            OsString::from("-u"),
            OsString::from(base_url.as_str()),
            OsString::from("--no-port-append"),
        ]);
    }

    args.extend([OsString::from("-o"), output_path.as_os_str().to_os_string()]);
    args
}

fn remove_output(output_path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(output_path) {
        debug!(
            "got while removing output directory: {}",
            Report::from_error(e)
        );
    }
}

fn spawn_log<U, I>(theme_dir: &Path, project_path: &Path, args: U) -> Result<(), Error>
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

    let mounted_theme_path =
        mount_theme(theme_dir, project_path).context(FsSnafu { path: theme_dir })?;
    let config_path = theme_config_path(&mounted_theme_path);

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

