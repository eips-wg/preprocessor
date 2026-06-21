/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    borrow::Borrow,
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use snafu::{Backtrace, IntoError, OptionExt, ResultExt, Snafu};
use url::Url;

pub const MANIFEST_FILE: &str = "Build.toml";
pub const LOCAL_CONFIG_FILE: &str = ".build-eips.toml";
pub const DEFAULT_BUILD_ROOT_BASE: &str = ".local-build";
pub const DEFAULT_THEME_DIR: &str = "theme";
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";
pub const DEFAULT_SERVER_PORT: u16 = 1111;
pub const DEFAULT_SITE_BASE_URL: &str = "http://127.0.0.1:1111";

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
        "unable to parse Build.toml `{}`",
        manifest_path.to_string_lossy()
    ))]
    Parse {
        manifest_path: PathBuf,
        #[snafu(source(from(toml::de::Error, Box::new)))]
        source: Box<toml::de::Error>,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "Build.toml `{}` is invalid: {}",
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

#[derive(Debug, Snafu)]
pub enum WorkspaceError {
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(
        context(name(WorkspaceParseSnafu)),
        display(
            "unable to parse workspace config `{}`",
            config_path.to_string_lossy()
        )
    )]
    Parse {
        config_path: PathBuf,
        #[snafu(source(from(toml::de::Error, Box::new)))]
        source: Box<toml::de::Error>,
        backtrace: Backtrace,
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

/// Resolved manifest context for the active proposal repository.
#[derive(Debug, Clone)]
pub struct ActiveRepo {
    /// Managed workspace title for the active repository.
    pub title: String,

    /// Declared sibling repository titles.
    pub sibling_ids: Vec<String>,

    /// Normalized source selection for the active repository and its siblings.
    pub repository_use: RepositoryUse,

    /// Theme metadata declared by the active repository manifest.
    pub theme: Theme,
}

impl ActiveRepo {
    /// Load the active repository manifest from its working-tree checkout.
    pub fn load(repo_root: &Path) -> Result<Self, Error> {
        let manifest = Manifest::load(repo_root.join(MANIFEST_FILE))?;
        let manifest_path = manifest.manifest_path.clone();
        let theme = manifest.theme.clone();
        let repository_use =
            RepositoryUse::try_from(manifest).context(InvalidSnafu { manifest_path })?;
        let title = repository_use.title.clone();
        let mut sibling_ids = repository_use
            .other_repos
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        sibling_ids.sort();

        Ok(Self {
            title,
            sibling_ids,
            repository_use,
            theme,
        })
    }
}

/// Workspace-local configuration loaded from `.build-eips.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Local server defaults for `build-eips serve` and `build-eips preview`.
    #[serde(default)]
    pub server: ServerSettings,

    /// Local rendered-site URL defaults for build and serve commands.
    #[serde(default)]
    pub site: SiteSettings,
}

impl WorkspaceConfig {
    fn starter() -> Self {
        Self {
            server: ServerSettings::default(),
            site: SiteSettings::starter(),
        }
    }
}

/// Workspace-local bind address defaults for local server commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSettings {
    /// Host or interface address used by `serve` and `preview`.
    pub host: String,

    /// TCP port used by `serve` and `preview`.
    pub port: u16,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: DEFAULT_SERVER_HOST.to_owned(),
            port: DEFAULT_SERVER_PORT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerBinding {
    pub host: String,
    pub port: u16,
}

impl Default for ServerBinding {
    fn default() -> Self {
        ServerSettings::default().into()
    }
}

impl From<ServerSettings> for ServerBinding {
    fn from(settings: ServerSettings) -> Self {
        Self {
            host: settings.host,
            port: settings.port,
        }
    }
}

impl From<&ServerSettings> for ServerBinding {
    fn from(settings: &ServerSettings) -> Self {
        settings.clone().into()
    }
}

impl fmt::Display for ServerBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.host, self.port)
    }
}

/// Workspace-local rendered-site URL defaults for build and serve commands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SiteSettings {
    /// Base URL written into rendered HTML, feeds, canonical links, and sitemaps.
    #[serde(default)]
    pub base_url: Option<Url>,
}

impl SiteSettings {
    fn starter() -> Self {
        Self {
            base_url: Some(
                DEFAULT_SITE_BASE_URL
                    .parse()
                    .expect("default site base URL should parse"),
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedWorkspaceConfig {
    config_path: PathBuf,
    workspace_root: PathBuf,
    config: WorkspaceConfig,
}

impl LoadedWorkspaceConfig {
    pub fn from_path(path: &Path) -> Result<Self, WorkspaceError> {
        let config_path = path.canonicalize().with_context(|_| FsSnafu {
            path: path.to_path_buf(),
        })?;
        let contents = std::fs::read_to_string(&config_path).with_context(|_| FsSnafu {
            path: config_path.clone(),
        })?;
        let config =
            toml::from_str::<WorkspaceConfig>(&contents).with_context(|_| WorkspaceParseSnafu {
                config_path: config_path.clone(),
            })?;

        let workspace_root = config_path
            .parent()
            .expect("workspace config should always have a parent")
            .to_path_buf();

        Ok(Self {
            config_path,
            workspace_root,
            config,
        })
    }

    pub fn discover(start: &Path) -> Result<Option<Self>, WorkspaceError> {
        match discover_path(start) {
            Some(path) => Self::from_path(&path).map(Some),
            None => Ok(None),
        }
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn workspace_build_root(&self, repo_name: &str) -> PathBuf {
        self.workspace_root
            .join(DEFAULT_BUILD_ROOT_BASE)
            .join(repo_name)
    }

    pub fn server_settings(&self) -> &ServerSettings {
        &self.config.server
    }

    pub fn site_settings(&self) -> &SiteSettings {
        &self.config.site
    }

    pub fn local_theme_path(&self) -> PathBuf {
        self.workspace_root.join(DEFAULT_THEME_DIR)
    }

    pub fn local_repo_path(&self, repo_name: &str) -> PathBuf {
        self.workspace_root.join(repo_name)
    }
}

pub fn discover_path(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);

    while let Some(candidate) = current {
        let config_path = candidate.join(LOCAL_CONFIG_FILE);
        match std::fs::File::open(&config_path) {
            Ok(_) => return Some(config_path),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                current = candidate.parent();
            }
            Err(_) => return Some(config_path),
        }
    }

    None
}

pub fn default_workspace_config_text() -> String {
    toml::to_string_pretty(&WorkspaceConfig::starter())
        .expect("workspace starter config should serialize")
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
    fn malformed_build_manifest_reports_parse_error() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, "name = [");

        let error = Manifest::load(&manifest_path).unwrap_err();

        assert!(matches!(error, Error::Parse { .. }));
    }

    #[test]
    fn parses_build_manifest() {
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
    fn build_manifest_rejects_unsafe_names() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, r#"name = "^^^^""#);

        let Err(Error::Parse { source, .. }) = Manifest::load(&manifest_path) else {
            panic!("expected parse error");
        };

        let reason = source.to_string();

        assert!(reason.contains("invalid location name"));
    }

    #[test]
    fn build_manifest_rejects_empty_names() {
        let repo = TestRepo::new();
        let manifest_path = repo.write_file(MANIFEST_FILE, r#"name = """#);

        let Err(Error::Parse { source, .. }) = Manifest::load(&manifest_path) else {
            panic!("expected parse error");
        };

        let reason = source.to_string();

        assert!(reason.contains("invalid location name"));
    }

    #[test]
    fn build_manifest_requires_self() {
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

#[cfg(test)]
mod workspace_tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::{
        default_workspace_config_text, discover_path, LoadedWorkspaceConfig, ServerBinding,
        ServerSettings, WorkspaceError, DEFAULT_SERVER_HOST, DEFAULT_SERVER_PORT,
        LOCAL_CONFIG_FILE,
    };
    struct TestWorkspace {
        tempdir: TempDir,
    }

    impl TestWorkspace {
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

        fn create_dir(&self, relative: impl AsRef<Path>) -> PathBuf {
            let path = self.path(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    #[test]
    fn parses_default_workspace_config() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(LOCAL_CONFIG_FILE, &default_workspace_config_text());

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(config.workspace_root(), workspace.root());
        assert_eq!(config.server_settings(), &ServerSettings::default());
        assert_eq!(
            config.site_settings().base_url.as_ref().unwrap().as_str(),
            "http://127.0.0.1:1111/"
        );
    }

    #[test]
    fn starter_workspace_config_roundtrips_stably() {
        let original = default_workspace_config_text();
        let parsed = toml::from_str::<super::WorkspaceConfig>(&original).unwrap();
        let reparsed = toml::to_string_pretty(&parsed).unwrap();

        assert_eq!(reparsed, original);
        assert!(!original.contains("build_root_base"));
        assert!(original.contains("[server]"));
        assert!(original.contains("host = \"127.0.0.1\""));
        assert!(original.contains("port = 1111"));
        assert!(original.contains("[site]"));
        assert!(original.contains("base_url = \"http://127.0.0.1:1111/\""));
        assert!(!original.contains("default_profile"));
        assert!(!original.contains("[profiles"));
    }

    #[test]
    fn parses_workspace_config_server_settings() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[server]
host = "0.0.0.0"
port = 8080
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(
            config.server_settings(),
            &ServerSettings {
                host: "0.0.0.0".to_owned(),
                port: 8080,
            }
        );
    }

    #[test]
    fn missing_server_settings_use_default_binding() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[site]
base_url = "http://localhost:4000"
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();
        let binding = ServerBinding::from(config.server_settings());

        assert_eq!(binding.host, DEFAULT_SERVER_HOST);
        assert_eq!(binding.port, DEFAULT_SERVER_PORT);
        assert_eq!(binding.to_string(), "127.0.0.1:1111");
    }

    #[test]
    fn parses_workspace_config_site_settings() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[site]
base_url = "http://localhost:4000"
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(
            config.site_settings().base_url.as_ref().unwrap().as_str(),
            "http://localhost:4000/"
        );
    }

    #[test]
    fn invalid_workspace_config_site_base_url_errors() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[site]
base_url = "not a url"
"#,
        );
        let error = LoadedWorkspaceConfig::from_path(&config_path).unwrap_err();

        assert!(error
            .to_string()
            .contains("unable to parse workspace config"));
    }

    #[test]
    fn missing_site_settings_preserve_no_base_url_override() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[server]
host = "127.0.0.1"
port = 1111
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert!(config.site_settings().base_url.is_none());
    }

    #[test]
    fn minimal_workspace_config_parses() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[server]
host = "127.0.0.1"
port = 1111

[site]
base_url = "http://127.0.0.1:1111"
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(config.server_settings(), &ServerSettings::default());
        assert_eq!(
            config.site_settings().base_url.as_ref().unwrap().as_str(),
            "http://127.0.0.1:1111/"
        );
    }

    #[test]
    fn empty_workspace_config_uses_defaults() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(LOCAL_CONFIG_FILE, " \n");

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(config.server_settings(), &ServerSettings::default());
        assert!(config.site_settings().base_url.is_none());
    }

    #[test]
    fn removed_workspace_config_fields_use_strict_parse_errors() {
        let removed_theme_ref_field = concat!("co", "mmit");
        let cases = vec![
            (
                "build_root_base".to_owned(),
                r#"build_root_base = ".local-build""#.to_owned(),
            ),
            (
                "default_profile".to_owned(),
                r#"default_profile = "local""#.to_owned(),
            ),
            (
                "profiles".to_owned(),
                r#"
[profiles.local]
staging = true
"#
                .to_owned(),
            ),
            (
                "theme".to_owned(),
                r#"
[theme]
repository = "https://github.com/eips-wg/theme.git"
"#
                .to_owned(),
            ),
            (
                format!("theme.{removed_theme_ref_field}"),
                format!(
                    r#"
[theme]
{removed_theme_ref_field} = "3a597d4cd68ec82d36f01c01335492cfa59501ae"
"#
                ),
            ),
        ];

        for (field, contents) in cases {
            let workspace = TestWorkspace::new();
            let config_path = workspace.write_file(LOCAL_CONFIG_FILE, &contents);
            let error = LoadedWorkspaceConfig::from_path(&config_path).unwrap_err();

            assert!(
                matches!(error, WorkspaceError::Parse { .. }),
                "expected strict parse error for removed field `{field}`, got {error:?}"
            );
        }
    }

    #[test]
    fn discover_path_walks_upward() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(LOCAL_CONFIG_FILE, &default_workspace_config_text());
        let nested = workspace.create_dir("EIPs/content");

        assert_eq!(discover_path(&nested).unwrap(), config_path);
        assert_eq!(
            LoadedWorkspaceConfig::discover(&nested)
                .unwrap()
                .unwrap()
                .config_path(),
            config_path
        );
    }

    #[test]
    fn missing_workspace_config_is_not_discovered() {
        let workspace = TestWorkspace::new();
        let nested = workspace.create_dir("EIPs/content");

        assert!(discover_path(&nested).is_none());
        assert!(LoadedWorkspaceConfig::discover(&nested).unwrap().is_none());
    }
}

#[cfg(test)]
mod active_repo_tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::{ActiveRepo, MANIFEST_FILE};

    #[test]
    fn legacy_repo_manifest_does_not_satisfy_active_repo_loading() {
        let tempdir = TempDir::new().unwrap();
        std::fs::write(
            tempdir.path().join(".build-eips.repo.toml"),
            "repo_id = \"EIPs\"\n",
        )
        .unwrap();

        let error = ActiveRepo::load(tempdir.path()).unwrap_err().to_string();

        assert!(error.contains("Build.toml"), "{error}");
    }

    #[test]
    fn active_repo_loads_manifest_and_normalizes_repository_use() {
        let tempdir = TempDir::new().unwrap();
        let manifest_path = tempdir.path().join(MANIFEST_FILE);
        std::fs::write(
            &manifest_path,
            r#"
name = "EIPs"

[locations.EIPs]
repository = "https://example.test/EIPs.git"
base-url = "https://example.test/EIPs/"

[locations.ERCs]
repository = "https://example.test/ERCs.git"
base-url = "https://example.test/ERCs/"

[theme]
repository = "https://example.test/theme.git"
commit = "abc123"
"#,
        )
        .unwrap();

        let active_repo = ActiveRepo::load(Path::new(tempdir.path())).unwrap();

        assert_eq!(active_repo.title, "EIPs");
        assert_eq!(active_repo.sibling_ids, ["ERCs"]);
        assert_eq!(active_repo.repository_use.title, "EIPs");
        assert_eq!(
            active_repo.repository_use.location.repository.as_str(),
            "https://example.test/EIPs.git"
        );
        assert_eq!(
            active_repo.repository_use.other_repos["ERCs"].as_str(),
            "https://example.test/ERCs.git"
        );
        assert_eq!(active_repo.theme.commit, "abc123");
    }
}
