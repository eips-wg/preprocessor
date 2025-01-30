/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{io::ErrorKind, path::PathBuf, sync::Arc};

use directories::ProjectDirs;
use fslock::LockFile;
use log::{debug, info};
use sha3::{Digest, Sha3_256};
use snafu::{Backtrace, IntoError, OptionExt, Report, ResultExt, Snafu};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("unable to discover application directories (unset $HOME?)"))]
    Directories { backtrace: Backtrace },
    #[snafu(display("unable to access the path `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        backtrace: Backtrace,
        source: std::io::Error,
    },
}

#[derive(Debug)]
struct Inner {
    _lock: LockFile,
    dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Cache(Arc<Inner>);

impl Cache {
    pub fn open() -> Result<Self, Error> {
        debug!("opening local file cache");

        let dirs =
            ProjectDirs::from("org.ethereum", "eips", "eips-build").context(DirectoriesSnafu)?;
        let cache_path = dirs.cache_dir();
        if let Err(e) = std::fs::create_dir_all(cache_path) {
            debug!(
                "got while creating cache directory: {}",
                Report::from_error(e)
            );
        }

        let lock_path = cache_path.join(".lock");
        let mut lock = LockFile::open(&lock_path).context(FsSnafu { path: &lock_path })?;

        let locked = lock
            .try_lock_with_pid()
            .context(FsSnafu { path: &lock_path })?;

        if !locked {
            info!("waiting on cache directory...");
            lock.lock_with_pid().context(FsSnafu { path: &lock_path })?;
        }

        Ok(Self(Arc::new(Inner {
            _lock: lock,
            dir: cache_path.into(),
        })))
    }

    pub fn dir(&self, key: &str) -> Result<PathBuf, Error> {
        let mut hasher = Sha3_256::new();
        hasher.update(key.as_bytes());
        let hash = hasher.finalize();
        let hash_text = format!("{:x}", hash);
        let path = self.0.dir.join(hash_text);

        debug!("creating cache directory `{}`", path.to_string_lossy());
        match std::fs::create_dir(&path) {
            Ok(()) => (),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => (),
            Err(e) => return Err(FsSnafu { path }.into_error(e)),
        }

        Ok(path)
    }
}
