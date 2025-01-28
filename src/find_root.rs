/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use snafu::{ResultExt, Snafu};
use std::{
    backtrace::Backtrace,
    path::{Path, PathBuf},
};
use super::CONTENT_DIR;

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

    #[snafu(display("could not find root directory (containing `.git` and `{CONTENT_DIR}`)"))]
    NoRoot { backtrace: Backtrace },
}

pub fn is_root(path: &Path) -> Result<(), Error> {
    let git = path.join(".git");
    let contents = path.join(CONTENT_DIR);
    if git.is_dir() && contents.is_dir() {
        Ok(())
    } else {
        NoRootSnafu.fail()
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
            Ok(()) => return Ok(candidate.to_path_buf()),
            Err(Error::NoRoot { .. }) => (),
            Err(e) => return Err(e),
        }
        current = candidate.parent();
    }

    NoRootSnafu.fail()
}
