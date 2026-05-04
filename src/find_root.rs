/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use super::CONTENT_DIR;
use crate::config::MANIFEST_FILE;

use snafu::{ResultExt, Snafu};
use std::{
    backtrace::Backtrace,
    path::{Path, PathBuf},
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("cannot read env's {what}"))]
    Env {
        what: String,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "could not find root directory (containing `{MANIFEST_FILE}` and `{CONTENT_DIR}`)"
    ))]
    NoRoot { backtrace: Backtrace },
}

pub fn is_root(path: &Path) -> Result<bool, Error> {
    let manifest_path = path.join(MANIFEST_FILE);
    let content_path = path.join(CONTENT_DIR);
    let git_path = path.join(".git");

    if manifest_path.is_file() && content_path.is_dir() {
        Ok(true)
    } else if git_path.is_dir() {
        NoRootSnafu.fail()
    } else {
        Ok(false)
    }
}

pub fn find_root() -> Result<PathBuf, Error> {
    let cwd = std::env::current_dir().context(EnvSnafu {
        what: "current_dir",
    })?;
    let current = cwd.canonicalize().context(FsSnafu { path: cwd })?;
    let mut current = Some(current.as_path());

    while let Some(candidate) = current {
        match is_root(candidate) {
            Ok(true) => return Ok(candidate.to_path_buf()),
            Ok(false) => (),
            Err(e) => return Err(e),
        }
        current = candidate.parent();
    }

    NoRootSnafu.fail()
}
