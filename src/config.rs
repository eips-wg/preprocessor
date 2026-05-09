/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use snafu::{Backtrace, IntoError, OptionExt, ResultExt, Snafu};
use url::Url;

pub const REPO_MANIFEST_FILE: &str = ".build-eips.repo.toml";
const RESERVED_REPO_IDS: &[&str] = &["theme", "preprocessor", "eipw"];

#[derive(Debug, Snafu)]
pub enum RepoManifestError {
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    RepoFs {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "unable to parse repo manifest `{}`",
        manifest_path.to_string_lossy()
    ))]
    RepoParse {
        manifest_path: PathBuf,
        #[snafu(source(from(toml::de::Error, Box::new)))]
        source: Box<toml::de::Error>,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "repo manifest `{}` is invalid: {reason}",
        manifest_path.to_string_lossy()
    ))]
    Invalid {
        manifest_path: PathBuf,
        reason: String,
        backtrace: Backtrace,
    },
}

/// Environment-specific repository metadata for an active proposal repo or sibling repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryEndpoint {
    /// Git repository to fetch proposal content from.
    pub repository: Url,

    /// Base URL where rendered HTML and assets for this repository are served.
    pub base_url: Url,
}

/// Tracked active-repo manifest loaded from `.build-eips.repo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoManifest {
    /// Stable machine key for workspace directory names, build roots, and sibling references.
    pub repo_id: String,

    /// Production repository and base URL for this active repo.
    pub production: RepositoryEndpoint,

    /// Staging repository and base URL for this active repo.
    pub staging: RepositoryEndpoint,

    /// Directional sibling content repos used by this active repo.
    #[serde(default)]
    pub siblings: BTreeMap<String, RepoManifestSibling>,
}

impl RepoManifest {
    fn from_raw(raw: RawRepoManifest, manifest_path: &Path) -> Result<Self, RepoManifestError> {
        let repo_id = required_manifest_value(manifest_path, "repo_id", raw.repo_id)?;
        let production = required_manifest_value(manifest_path, "production", raw.production)?;
        let staging = required_manifest_value(manifest_path, "staging", raw.staging)?;
        let siblings = raw
            .siblings
            .into_iter()
            .map(|(repo_id, sibling)| {
                let production = required_manifest_value(
                    manifest_path,
                    &format!("siblings.{repo_id}.production"),
                    sibling.production,
                )?;
                let staging = required_manifest_value(
                    manifest_path,
                    &format!("siblings.{repo_id}.staging"),
                    sibling.staging,
                )?;

                Ok((
                    repo_id,
                    RepoManifestSibling {
                        production,
                        staging,
                    },
                ))
            })
            .collect::<Result<_, _>>()?;

        let manifest = Self {
            repo_id,
            production,
            staging,
            siblings,
        };
        manifest.validate(manifest_path)?;
        Ok(manifest)
    }

    fn validate(&self, manifest_path: &Path) -> Result<(), RepoManifestError> {
        validate_repo_key(manifest_path, "repo_id", &self.repo_id)?;

        if self.siblings.contains_key(&self.repo_id) {
            return InvalidSnafu {
                manifest_path: manifest_path.to_path_buf(),
                reason: format!(
                    "repo_id `{}` cannot also be declared as a sibling",
                    self.repo_id
                ),
            }
            .fail();
        }

        for sibling_id in self.siblings.keys() {
            validate_repo_key(manifest_path, "sibling key", sibling_id)?;
        }

        validate_unique_sibling_repositories(
            manifest_path,
            "production",
            self.siblings
                .iter()
                .map(|(id, sibling)| (id.as_str(), sibling.production.repository.as_str())),
        )?;
        validate_unique_sibling_repositories(
            manifest_path,
            "staging",
            self.siblings
                .iter()
                .map(|(id, sibling)| (id.as_str(), sibling.staging.repository.as_str())),
        )?;

        Ok(())
    }

    pub fn active_endpoint(&self, staging: bool) -> RepositoryEndpoint {
        if staging {
            self.staging.clone()
        } else {
            self.production.clone()
        }
    }

    pub fn sibling_repositories(&self, staging: bool) -> BTreeMap<String, Url> {
        self.siblings
            .iter()
            .map(|(repo_id, sibling)| {
                let endpoint = if staging {
                    &sibling.staging
                } else {
                    &sibling.production
                };
                (repo_id.clone(), endpoint.repository.clone())
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepoManifest {
    repo_id: Option<String>,
    production: Option<RepositoryEndpoint>,
    staging: Option<RepositoryEndpoint>,
    #[serde(default)]
    siblings: BTreeMap<String, RawRepoManifestSibling>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRepoManifestSibling {
    production: Option<RepositoryEndpoint>,
    staging: Option<RepositoryEndpoint>,
}

/// Environment-specific metadata for one declared sibling content repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoManifestSibling {
    /// Production repository and base URL for this sibling repo.
    pub production: RepositoryEndpoint,

    /// Staging repository and base URL for this sibling repo.
    pub staging: RepositoryEndpoint,
}

#[derive(Debug, Clone)]
pub struct LoadedRepoManifest {
    manifest_path: PathBuf,
    manifest: RepoManifest,
}

impl LoadedRepoManifest {
    pub fn load(repo_root: &Path) -> Result<Option<Self>, RepoManifestError> {
        let manifest_path = repo_root.join(REPO_MANIFEST_FILE);
        match std::fs::read_to_string(&manifest_path) {
            Ok(contents) => Self::from_contents(manifest_path, &contents).map(Some),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(RepoFsSnafu {
                path: manifest_path,
            }
            .into_error(error)),
        }
    }

    #[cfg(test)]
    pub fn from_path(path: &Path) -> Result<Self, RepoManifestError> {
        let manifest_path = path.canonicalize().with_context(|_| RepoFsSnafu {
            path: path.to_path_buf(),
        })?;
        let contents = std::fs::read_to_string(&manifest_path).with_context(|_| RepoFsSnafu {
            path: manifest_path.clone(),
        })?;
        Self::from_contents(manifest_path, &contents)
    }

    fn from_contents(manifest_path: PathBuf, contents: &str) -> Result<Self, RepoManifestError> {
        let manifest =
            toml::from_str::<RawRepoManifest>(contents).with_context(|_| RepoParseSnafu {
                manifest_path: manifest_path.clone(),
            })?;
        let manifest = RepoManifest::from_raw(manifest, &manifest_path)?;

        Ok(Self {
            manifest_path,
            manifest,
        })
    }

    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    pub fn manifest(&self) -> &RepoManifest {
        &self.manifest
    }
}

fn required_manifest_value<T>(
    manifest_path: &Path,
    field: &str,
    value: Option<T>,
) -> Result<T, RepoManifestError> {
    value.with_context(|| InvalidSnafu {
        manifest_path: manifest_path.to_path_buf(),
        reason: format!("missing required `{field}` entry"),
    })
}

fn validate_repo_key(
    manifest_path: &Path,
    label: &str,
    key: &str,
) -> Result<(), RepoManifestError> {
    let invalid_reason = if key.is_empty() {
        Some("must not be empty")
    } else if matches!(key, "." | "..") {
        Some("must not be `.` or `..`")
    } else if key.contains('/') || key.contains('\\') {
        Some("must be a single safe path component")
    } else if RESERVED_REPO_IDS.contains(&key) {
        Some("collides with a reserved workspace/platform directory name")
    } else {
        None
    };

    if let Some(reason) = invalid_reason {
        return InvalidSnafu {
            manifest_path: manifest_path.to_path_buf(),
            reason: format!("{label} `{key}` {reason}"),
        }
        .fail();
    }

    Ok(())
}

fn validate_unique_sibling_repositories<'a>(
    manifest_path: &Path,
    environment: &str,
    siblings: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<(), RepoManifestError> {
    let mut seen = HashSet::new();
    for (repo_id, repository) in siblings {
        if !seen.insert(repository) {
            return InvalidSnafu {
                manifest_path: manifest_path.to_path_buf(),
                reason: format!(
                    "duplicate {environment} sibling repository declaration `{repository}` under sibling key `{repo_id}`"
                ),
            }
            .fail();
        }
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{LoadedRepoManifest, RepoManifestError, REPO_MANIFEST_FILE};

    struct TestRepo {
        tempdir: TempDir,
    }

    impl TestRepo {
        fn new() -> Self {
            Self {
                tempdir: TempDir::new().unwrap(),
            }
        }

        fn root(&self) -> &Path {
            self.tempdir.path()
        }

        fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
            self.root().join(relative)
        }

        fn write_file(&self, relative: impl AsRef<Path>, contents: &str) -> PathBuf {
            let path = self.path(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    fn manifest_text(repo_id: &str, siblings: &str) -> String {
        format!(
            r#"
repo_id = "{repo_id}"

[production]
repository = "https://example.test/{repo_id}.git"
base_url = "https://example.test/{repo_id}/"

[staging]
repository = "https://staging.example.test/{repo_id}.git"
base_url = "https://staging.example.test/{repo_id}/"

{siblings}
"#
        )
    }

    fn manifest_invalid_reason(error: RepoManifestError) -> String {
        match error {
            RepoManifestError::Invalid { reason, .. } => reason,
            other => panic!("expected invalid repo manifest, got {other:?}"),
        }
    }

    #[test]
    fn missing_repo_manifest_loads_as_none() {
        let repo = TestRepo::new();

        assert!(LoadedRepoManifest::load(repo.root()).unwrap().is_none());
    }

    #[test]
    fn malformed_repo_manifest_reports_parse_error() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(REPO_MANIFEST_FILE, "repo_id = [");

        let error = LoadedRepoManifest::from_path(&manifest_path).unwrap_err();

        assert!(matches!(error, RepoManifestError::RepoParse { .. }));
    }

    #[test]
    fn parses_repo_manifest_with_directional_siblings() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            &manifest_text(
                "Core",
                r#"
[siblings.EIPs.production]
repository = "https://example.test/EIPs.git"
base_url = "https://example.test/EIPs/"

[siblings.EIPs.staging]
repository = "https://staging.example.test/EIPs.git"
base_url = "https://staging.example.test/EIPs/"
"#,
            ),
        );

        let manifest = LoadedRepoManifest::from_path(&manifest_path).unwrap();

        assert_eq!(manifest.manifest().repo_id, "Core");
        assert_eq!(manifest.manifest().siblings.len(), 1);
        assert!(manifest.manifest().siblings.contains_key("EIPs"));
    }

    #[test]
    fn repo_manifest_requires_identity_and_environments() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            r#"
[production]
repository = "https://example.test/Core.git"
base_url = "https://example.test/Core/"
"#,
        );

        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("missing required `repo_id` entry"));

        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            r#"
repo_id = "Core"

[production]
repository = "https://example.test/Core.git"
base_url = "https://example.test/Core/"
"#,
        );
        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("missing required `staging` entry"));

        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            r#"
repo_id = "Core"

[staging]
repository = "https://staging.example.test/Core.git"
base_url = "https://staging.example.test/Core/"
"#,
        );
        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("missing required `production` entry"));
    }

    #[test]
    fn repo_manifest_rejects_unsafe_and_reserved_keys() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(REPO_MANIFEST_FILE, &manifest_text("theme", ""));

        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("repo_id `theme`"));
        assert!(reason.contains("reserved"));

        let manifest_path = repo.write_file(REPO_MANIFEST_FILE, &manifest_text("Core/Meta", ""));
        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("repo_id `Core/Meta`"));
        assert!(reason.contains("single safe path component"));
    }

    #[test]
    fn repo_manifest_rejects_self_sibling() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            &manifest_text(
                "Core",
                r#"
[siblings.Core.production]
repository = "https://example.test/Core.git"
base_url = "https://example.test/Core/"

[siblings.Core.staging]
repository = "https://staging.example.test/Core.git"
base_url = "https://staging.example.test/Core/"
"#,
            ),
        );

        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("cannot also be declared as a sibling"));
    }

    #[test]
    fn repo_manifest_rejects_duplicate_sibling_repositories() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            REPO_MANIFEST_FILE,
            &manifest_text(
                "Core",
                r#"
[siblings.EIPs.production]
repository = "https://example.test/shared.git"
base_url = "https://example.test/EIPs/"

[siblings.EIPs.staging]
repository = "https://staging.example.test/EIPs.git"
base_url = "https://staging.example.test/EIPs/"

[siblings.ERCs.production]
repository = "https://example.test/shared.git"
base_url = "https://example.test/ERCs/"

[siblings.ERCs.staging]
repository = "https://staging.example.test/ERCs.git"
base_url = "https://staging.example.test/ERCs/"
"#,
            ),
        );

        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("duplicate production sibling repository"));
        assert!(reason.contains("https://example.test/shared.git"));
    }
}
