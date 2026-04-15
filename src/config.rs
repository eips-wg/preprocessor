/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use snafu::{Backtrace, OptionExt, ResultExt, Snafu};
use url::Url;

pub const LOCAL_CONFIG_FILE: &str = ".build-eips.toml";
pub const DEFAULT_BUILD_ROOT_BASE: &str = ".local-build";
pub const DEFAULT_THEME_DIR: &str = "theme";
pub const DEFAULT_PROFILE: &str = "workspace";

#[derive(Debug, Snafu)]
pub enum WorkspaceError {
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display("unable to parse workspace config `{}`", path.to_string_lossy()))]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
        backtrace: Backtrace,
    },

    #[snafu(display("cannot use `--profile {profile}` without a workspace config"))]
    ProfileWithoutConfig {
        profile: String,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "workspace config `{}` does not define profile `{profile}`",
        path.to_string_lossy()
    ))]
    MissingProfile {
        path: PathBuf,
        profile: String,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "workspace config profile `{profile}` sets incompatible values for `{local}` and `{remote}`"
    ))]
    ConflictingProfileSwitch {
        path: PathBuf,
        profile: String,
        local: &'static str,
        remote: &'static str,
        backtrace: Backtrace,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Where to fetch the theme from.
    pub repository: Url,

    /// Specific revision to checkout from the theme repository.
    pub commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    /// Git repository to fetch proposals from.
    pub repository: Url,

    /// Location where the rendered HTML and assets will end up.
    pub base_url: Url,

    /// A commit hash that exists solely in this repository.
    ///
    /// Use to determine which repository is being rendered. Pick a commit after every other
    /// location/working group/etc. split off.
    pub identifying_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locations(pub HashMap<String, Location>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub theme: Theme,
    pub locations: Locations,
}

#[derive(Debug, Clone, Default)]
pub struct LocalOverrides {
    pub theme_path: Option<PathBuf>,
    pub other_repo_path: Option<PathBuf>,
    pub build_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub default_profile: Option<String>,
    pub build_root_base: PathBuf,
    pub profiles: HashMap<String, LocalProfile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalProfile {
    pub staging: bool,
    pub use_local_theme: bool,
    pub use_local_sibling: bool,
    pub allow_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct LoadedWorkspaceConfig {
    path: PathBuf,
    workspace_root: PathBuf,
    config: WorkspaceConfig,
}

#[derive(Debug, Clone)]
pub struct SelectedProfile {
    pub name: String,
    pub profile: LocalProfile,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct RawLocalProfile {
    staging: bool,
    use_local_theme: Option<bool>,
    use_local_sibling: Option<bool>,
    use_remote_theme: Option<bool>,
    use_remote_sibling: Option<bool>,
    allow_dirty: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct RawWorkspaceConfig {
    default_profile: Option<String>,
    build_root_base: PathBuf,
    profiles: HashMap<String, RawLocalProfile>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            default_profile: Some(DEFAULT_PROFILE.into()),
            build_root_base: DEFAULT_BUILD_ROOT_BASE.into(),
            profiles: HashMap::new(),
        }
    }
}

impl Default for RawWorkspaceConfig {
    fn default() -> Self {
        Self {
            default_profile: Some(DEFAULT_PROFILE.into()),
            build_root_base: DEFAULT_BUILD_ROOT_BASE.into(),
            profiles: HashMap::new(),
        }
    }
}

impl Config {
    pub fn production() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            Location {
                repository: "https://github.com/ethereum/EIPs.git".try_into().unwrap(),
                base_url: "https://eips.ethereum.org/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            Location {
                repository: "https://github.com/ethereum/ERCs.git".try_into().unwrap(),
                base_url: "https://ercs.ethereum.org/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            theme: Theme {
                repository: "https://github.com/ethereum/eips-theme.git"
                    .try_into()
                    .unwrap(),
                commit: "0ddac35da36d311a8401c6cfb79c9991f78b647d".into(),
            },
            locations: Locations(locations),
        }
    }

    pub fn staging() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            Location {
                repository: "https://github.com/eips-wg/EIPs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/EIPs/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            Location {
                repository: "https://github.com/eips-wg/ERCs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/ERCs/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            theme: Theme {
                repository: "https://github.com/eips-wg/theme.git".try_into().unwrap(),
                commit: "0ddac35da36d311a8401c6cfb79c9991f78b647d".into(),
            },
            locations: Locations(locations),
        }
    }
}

impl LoadedWorkspaceConfig {
    pub fn load(
        explicit: Option<&Path>,
        search_from: &Path,
    ) -> Result<Option<Self>, WorkspaceError> {
        match explicit {
            Some(path) => Self::from_path(path).map(Some),
            None => Self::discover(search_from),
        }
    }

    pub fn from_path(path: &Path) -> Result<Self, WorkspaceError> {
        let path = path.canonicalize().context(FsSnafu {
            path: path.to_path_buf(),
        })?;
        let contents = std::fs::read_to_string(&path).context(FsSnafu { path: &path })?;
        let raw =
            toml::from_str::<RawWorkspaceConfig>(&contents).context(ParseSnafu { path: &path })?;
        let workspace_root = path
            .parent()
            .expect("workspace config should always have a parent")
            .to_path_buf();

        let mut profiles = HashMap::with_capacity(raw.profiles.len());
        for (name, raw_profile) in raw.profiles {
            let profile = LocalProfile::from_raw(&path, &name, raw_profile)?;
            profiles.insert(name, profile);
        }

        Ok(Self {
            path,
            workspace_root,
            config: WorkspaceConfig {
                default_profile: raw.default_profile,
                build_root_base: raw.build_root_base,
                profiles,
            },
        })
    }

    pub fn discover(start: &Path) -> Result<Option<Self>, WorkspaceError> {
        match discover_path(start) {
            Some(path) => Self::from_path(&path).map(Some),
            None => Ok(None),
        }
    }

    pub fn selected_profile(
        &self,
        requested: Option<&str>,
    ) -> Result<Option<SelectedProfile>, WorkspaceError> {
        let name = requested
            .map(str::to_owned)
            .or_else(|| self.config.default_profile.clone());

        let Some(name) = name else {
            return Ok(None);
        };

        let profile = self
            .config
            .profiles
            .get(&name)
            .cloned()
            .context(MissingProfileSnafu {
                path: self.path.clone(),
                profile: name.clone(),
            })?;

        Ok(Some(SelectedProfile { name, profile }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn build_root_for(&self, repo_name: &str) -> PathBuf {
        self.resolve_path(&self.config.build_root_base)
            .join(repo_name)
    }

    pub fn local_theme_path(&self) -> PathBuf {
        self.workspace_root.join(DEFAULT_THEME_DIR)
    }

    pub fn local_repo_path(&self, repo_name: &str) -> PathBuf {
        self.workspace_root.join(repo_name)
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace_root.join(path)
        }
    }
}

pub fn discover_path(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);

    while let Some(candidate) = current {
        let path = candidate.join(LOCAL_CONFIG_FILE);
        if path.is_file() {
            return Some(path);
        }

        current = candidate.parent();
    }

    None
}

impl LocalProfile {
    fn from_raw(path: &Path, profile: &str, raw: RawLocalProfile) -> Result<Self, WorkspaceError> {
        Ok(Self {
            staging: raw.staging,
            use_local_theme: resolve_profile_switch(
                path,
                profile,
                raw.use_local_theme,
                raw.use_remote_theme,
                "use_local_theme",
                "use_remote_theme",
            )?,
            use_local_sibling: resolve_profile_switch(
                path,
                profile,
                raw.use_local_sibling,
                raw.use_remote_sibling,
                "use_local_sibling",
                "use_remote_sibling",
            )?,
            allow_dirty: raw.allow_dirty,
        })
    }
}

fn resolve_profile_switch(
    path: &Path,
    profile: &str,
    local: Option<bool>,
    remote: Option<bool>,
    local_name: &'static str,
    remote_name: &'static str,
) -> Result<bool, WorkspaceError> {
    match (local, remote) {
        (Some(local), Some(remote)) if local == !remote => Ok(local),
        (Some(_), Some(_)) => ConflictingProfileSwitchSnafu {
            path: path.to_path_buf(),
            profile: profile.to_owned(),
            local: local_name,
            remote: remote_name,
        }
        .fail(),
        (Some(local), None) => Ok(local),
        (None, Some(remote)) => Ok(!remote),
        (None, None) => Ok(false),
    }
}

pub fn selected_profile(
    config: Option<&LoadedWorkspaceConfig>,
    requested: Option<&str>,
) -> Result<Option<SelectedProfile>, WorkspaceError> {
    match config {
        Some(config) => config.selected_profile(requested),
        None => match requested {
            Some(profile) => ProfileWithoutConfigSnafu {
                profile: profile.to_owned(),
            }
            .fail(),
            None => Ok(None),
        },
    }
}

pub fn default_workspace_config_text() -> &'static str {
    r#"default_profile = "workspace"
build_root_base = ".local-build"

[profiles.workspace]
staging = true
use_local_theme = true
use_local_sibling = true

[profiles.parity]
staging = true
use_remote_theme = true
use_remote_sibling = true

[profiles.dirty]
staging = true
use_local_theme = true
use_local_sibling = true
allow_dirty = true
"#
}

#[cfg(test)]
mod tests {
    use super::{default_workspace_config_text, LoadedWorkspaceConfig, LOCAL_CONFIG_FILE};

    #[test]
    fn parses_default_workspace_config() {
        let dir = std::env::temp_dir().join(format!(
            "build-eips-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOCAL_CONFIG_FILE);
        std::fs::write(&path, default_workspace_config_text()).unwrap();

        let config = LoadedWorkspaceConfig::from_path(&path).unwrap();
        let workspace = config
            .selected_profile(Some("workspace"))
            .unwrap()
            .unwrap()
            .profile;
        let parity = config
            .selected_profile(Some("parity"))
            .unwrap()
            .unwrap()
            .profile;
        let dirty = config
            .selected_profile(Some("dirty"))
            .unwrap()
            .unwrap()
            .profile;

        assert!(workspace.staging);
        assert!(workspace.use_local_theme);
        assert!(workspace.use_local_sibling);

        assert!(parity.staging);
        assert!(!parity.use_local_theme);
        assert!(!parity.use_local_sibling);

        assert!(dirty.staging);
        assert!(dirty.use_local_theme);
        assert!(dirty.use_local_sibling);
        assert!(dirty.allow_dirty);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
