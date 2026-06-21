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

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::{Command, ExitStatus},
    };

    use crate::{
        config::ServerBinding,
        layout::{mounted_theme_path, theme_config_path},
    };
    use tempfile::TempDir;

    use super::{find_zola, mount_theme, serve_args};

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn zola_build_status(project_root: &Path) -> ExitStatus {
        Command::new("zola")
            .arg("build")
            .arg("--drafts")
            .current_dir(project_root)
            .status()
            .unwrap()
    }

    #[test]
    fn zola_rejects_missing_internal_links_but_accepts_external_links() {
        if find_zola().is_err() {
            eprintln!("skipping zola link behavior fixture because zola is not installed");
            return;
        }

        let temp = TempDir::new().unwrap();
        let internal = temp.path().join("internal");
        write_file(
            &internal,
            "config.toml",
            "base_url = \"https://example.test\"\n",
        );
        write_file(
            &internal,
            "content/_index.md",
            "+++\ntitle = \"Internal\"\n+++\n[Missing](@/missing.md)\n",
        );
        let external = temp.path().join("external");
        write_file(
            &external,
            "config.toml",
            "base_url = \"https://example.test\"\n",
        );
        write_file(
            &external,
            "content/_index.md",
            "+++\ntitle = \"External\"\n+++\n[External](https://eips.ethereum.org/EIPS/eip-1)\n",
        );

        assert!(!zola_build_status(&internal).success());
        assert!(zola_build_status(&external).success());
    }

    #[test]
    fn serve_args_include_configured_interface_and_port() {
        let server_binding = ServerBinding {
            host: "0.0.0.0".to_owned(),
            port: 8080,
        };

        assert_eq!(
            serve_args(&server_binding, Path::new("/tmp/build-output"), None),
            vec![
                OsString::from("serve"),
                OsString::from("--drafts"),
                OsString::from("--fast"),
                OsString::from("--force"),
                OsString::from("--interface"),
                OsString::from("0.0.0.0"),
                OsString::from("--port"),
                OsString::from("8080"),
                OsString::from("-o"),
                OsString::from("/tmp/build-output"),
            ]
        );
    }

    #[test]
    fn serve_args_include_base_url_override_when_present() {
        let server_binding = ServerBinding {
            host: "127.0.0.1".to_owned(),
            port: 1111,
        };
        let base_url = "http://127.0.0.1:1111".parse().unwrap();

        assert_eq!(
            serve_args(
                &server_binding,
                Path::new("/tmp/build-output"),
                Some(&base_url)
            ),
            vec![
                OsString::from("serve"),
                OsString::from("--drafts"),
                OsString::from("--fast"),
                OsString::from("--force"),
                OsString::from("--interface"),
                OsString::from("127.0.0.1"),
                OsString::from("--port"),
                OsString::from("1111"),
                OsString::from("-u"),
                OsString::from("http://127.0.0.1:1111/"),
                OsString::from("--no-port-append"),
                OsString::from("-o"),
                OsString::from("/tmp/build-output"),
            ]
        );
    }

    #[test]
    fn mounted_theme_paths_are_under_project_themes_directory() {
        let project_path = PathBuf::from("/tmp/project");
        let mounted_theme = mounted_theme_path(&project_path);

        assert_eq!(
            mounted_theme,
            PathBuf::from("/tmp/project/themes/eips-theme")
        );
        assert_eq!(
            theme_config_path(&mounted_theme),
            PathBuf::from("/tmp/project/themes/eips-theme/config/zola.toml")
        );
    }

    #[test]
    fn mount_theme_does_not_symlink_mounted_local_theme_onto_itself() {
        let temp = TempDir::new().unwrap();
        let project_path = temp.path().join("repo");
        let mounted_theme = mounted_theme_path(&project_path);
        fs::create_dir_all(mounted_theme.join("config")).unwrap();
        fs::write(mounted_theme.join("config/zola.toml"), "title = 'local'\n").unwrap();

        let result = mount_theme(&mounted_theme, &project_path).unwrap();

        assert_eq!(result, mounted_theme);
        assert!(mounted_theme.join("config/zola.toml").is_file());
        assert!(!fs::symlink_metadata(&mounted_theme)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn theme_mount_replaces_prior_real_mounted_theme_directory() {
        let temp = TempDir::new().unwrap();
        let project_path = temp.path().join("repo");
        let source_theme = temp.path().join("source-theme");
        fs::create_dir_all(source_theme.join("config")).unwrap();
        fs::write(source_theme.join("config/zola.toml"), "title = 'source'\n").unwrap();

        let mounted_theme = mounted_theme_path(&project_path);
        fs::create_dir_all(&mounted_theme).unwrap();
        fs::write(mounted_theme.join("stale.txt"), "stale").unwrap();

        let result = mount_theme(&source_theme, &project_path).unwrap();

        assert_eq!(result, mounted_theme);
        assert!(fs::symlink_metadata(&mounted_theme)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::read_to_string(theme_config_path(&mounted_theme)).unwrap(),
            "title = 'source'\n"
        );
    }
}
