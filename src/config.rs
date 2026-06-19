/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{borrow::Borrow, collections::HashMap, path::PathBuf, str::FromStr};

use regex::Regex;
use serde::{Deserialize, Serialize};
use snafu::{Backtrace, IntoError, OptionExt, ResultExt, Snafu};
use url::Url;

pub const MANIFEST_FILE: &str = "Build.toml";

#[derive(Debug, Snafu)]
#[non_exhaustive]
pub enum Error {
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Io {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "unable to parse repo manifest `{}`",
        manifest_path.to_string_lossy()
    ))]
    Parse {
        manifest_path: PathBuf,
        #[snafu(source(from(toml::de::Error, Box::new)))]
        source: Box<toml::de::Error>,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "repo manifest `{}` is invalid: {}",
        manifest_path.to_string_lossy(),
        source,
    ))]
    Invalid {
        manifest_path: PathBuf,
        backtrace: Backtrace,
        #[snafu(source(from(NoIdentityError, Box::new)))]
        source: Box<dyn 'static + std::error::Error + Send>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Theme {
    /// Where to fetch the theme from.
    pub repository: Url,

    /// Specific revision to checkout from the theme repository.
    pub commit: String,
}

/// Location-specific repository metadata for an active proposal repo or sibling repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Location {
    /// Git repository to fetch proposal content from.
    pub repository: Url,

    /// Base URL where rendered HTML and assets for this repository are served.
    pub base_url: Url,
}

#[derive(Debug, Snafu)]
#[snafu(display(
    "invalid location name `{name}`; only letters/numbers/dashes/underscores are allowed"
))]
pub struct NameError {
    name: String,
    backtrace: Backtrace,
}

lazy_static::lazy_static! {
    static ref RE_LOC_NAME: Regex = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LocName(String);

impl Borrow<str> for LocName {
    fn borrow(&self) -> &str {
        self.0.as_str()
    }
}

impl Borrow<String> for LocName {
    fn borrow(&self) -> &String {
        &self.0
    }
}

impl PartialEq<str> for LocName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<String> for LocName {
    fn eq(&self, other: &String) -> bool {
        self.0 == other.as_str()
    }
}

impl FromStr for LocName {
    type Err = NameError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::try_from(name.to_owned())
    }
}

impl TryFrom<String> for LocName {
    type Error = NameError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if RE_LOC_NAME.is_match(&name) {
            Ok(Self(name))
        } else {
            NameSnafu { name }.fail()
        }
    }
}

impl From<LocName> for String {
    fn from(value: LocName) -> Self {
        value.0
    }
}

impl std::fmt::Display for LocName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

pub type Locations = HashMap<LocName, Location>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct InnerManifest {
    name: LocName,

    #[serde(default, skip_serializing_if = "Locations::is_empty")]
    locations: Locations,

    theme: Theme,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub manifest_path: PathBuf,
    pub name: LocName,
    pub locations: Locations,
    pub theme: Theme,
}

impl Manifest {
    pub fn load<P: Into<PathBuf>>(path: P) -> Result<Self, Error> {
        let path = path.into();
        match std::fs::read_to_string(&path) {
            Ok(contents) => Self::from_contents(path, &contents),
            Err(e) => Err(IoSnafu { path }.into_error(e)),
        }
    }

    fn from_inner(manifest_path: PathBuf, inner: InnerManifest) -> Result<Self, Error> {
        if !inner.locations.contains_key(&inner.name) {
            return NoIdentitySnafu { name: inner.name }
                .fail()
                .context(InvalidSnafu { manifest_path });
        }

        Ok(Self {
            manifest_path,
            name: inner.name,
            locations: inner.locations,
            theme: inner.theme,
        })
    }

    fn from_contents(manifest_path: PathBuf, contents: &str) -> Result<Self, Error> {
        let new = toml::from_str::<InnerManifest>(contents).context(ParseSnafu {
            manifest_path: &manifest_path,
        })?;

        Self::from_inner(manifest_path, new)
    }
}

#[derive(Debug, Snafu)]
#[snafu(display("this locations's name (`{name}`) must appear in `locations`"))]
pub struct NoIdentityError {
    name: LocName,
    backtrace: Backtrace,
}

#[derive(Debug, Clone)]
pub struct RepositoryUse {
    pub title: String,
    pub location: Location,
    pub other_repos: HashMap<String, Url>,
}

impl TryFrom<Manifest> for RepositoryUse {
    type Error = NoIdentityError;

    fn try_from(mut value: Manifest) -> Result<Self, Self::Error> {
        let location = value
            .locations
            .remove(&value.name)
            .with_context(|| NoIdentitySnafu {
                name: value.name.clone(),
            })?;

        Ok(Self {
            title: value.name.into(),
            location,
            other_repos: value
                .locations
                .into_iter()
                .map(|(k, v)| (k.into(), v.repository))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{Error, Manifest, MANIFEST_FILE};

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

    #[test]
    fn malformed_repo_manifest_reports_parse_error() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, "repo_id = [");

        let error = Manifest::load(&manifest_path).unwrap_err();

        assert!(matches!(error, Error::Parse { .. }));
    }

    #[test]
    fn parses_repo_manifest() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            MANIFEST_FILE,
            r#"
name = "Core"

[locations.Core]
repository = "https://example.test/EIPs.git"
base-url = "https://example.test/EIPs/"

[theme]
repository = "https://example.test/theme.git"
commit = "aaa"
"#,
        );

        let manifest = Manifest::load(&manifest_path).expect("loaded successfully");

        assert_eq!(&manifest.name, "Core");
        assert_eq!(manifest.locations.len(), 1);
        let core = &manifest.locations["Core"];

        assert_eq!(core.base_url.as_str(), "https://example.test/EIPs/");
    }

    #[test]
    fn repo_manifest_rejects_unsafe_names() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, r#"name = "^^^^""#);

        let Err(Error::Parse { source, .. }) = Manifest::load(&manifest_path) else {
            panic!("expected parse error");
        };

        let reason = source.to_string();

        assert!(reason.contains("invalid location name"));
    }

    #[test]
    fn repo_manifest_rejects_empty_names() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, r#"name = """#);

        let Err(Error::Parse { source, .. }) = Manifest::load(&manifest_path) else {
            panic!("expected parse error");
        };

        let reason = source.to_string();

        assert!(reason.contains("invalid location name"));
    }

    #[test]
    fn repo_manifest_requires_self() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(
            MANIFEST_FILE,
            r#"
name = "banana"

[theme]
repository = "https://example.test/theme.git"
commit = "aaa"
"#,
        );

        let Err(Error::Invalid { source, .. }) = Manifest::load(&manifest_path) else {
            panic!("expected invalid error");
        };

        let reason = source.to_string();

        assert!(reason.contains("this locations's name (`banana`) must appear in `locations`"));
    }
}
