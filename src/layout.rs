/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::path::{Path, PathBuf};

pub(crate) const CONTENT_DIR: &str = "content";
pub(crate) const BUILD_DIR: &str = "build";
pub(crate) const REPO_DIR: &str = "repo";
const OUTPUT_DIR: &str = "output";

pub(crate) fn output_path(build_path: &Path) -> PathBuf {
    build_path.join(OUTPUT_DIR)
}

pub(crate) fn mounted_theme_path(project_path: &Path) -> PathBuf {
    project_path.join("themes").join("eips-theme")
}

pub(crate) fn theme_config_path(theme_path: &Path) -> PathBuf {
    [theme_path, Path::new("config"), Path::new("zola.toml")]
        .iter()
        .collect()
}
