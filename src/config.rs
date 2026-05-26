/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use snafu::{Backtrace, IntoError, OptionExt, ResultExt, Snafu};
use url::{Position, Url};

use crate::proposal::ProposalNumber;

pub const LOCAL_CONFIG_FILE: &str = ".build-eips.toml";
pub const REPO_MANIFEST_FILE: &str = ".build-eips.repo.toml";
pub const DEFAULT_BUILD_ROOT_BASE: &str = ".local-build";
pub const DEFAULT_THEME_DIR: &str = "theme";
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";
pub const DEFAULT_SERVER_PORT: u16 = 1111;
pub const DEFAULT_SITE_BASE_URL: &str = "http://127.0.0.1:1111";
const RESERVED_WORKSPACE_NAMES: &[&str] = &[DEFAULT_THEME_DIR, "preprocessor", "eipw"];

#[derive(Debug, Snafu)]
pub enum RepoManifestError {
    #[snafu(
        context(name(RepoManifestIoSnafu)),
        display("i/o error while accessing `{}`", path.to_string_lossy())
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(
        context(name(RepoManifestParseSnafu)),
        display(
            "unable to parse repo manifest `{}`",
            manifest_path.to_string_lossy()
        )
    )]
    Parse {
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

#[derive(Debug, Snafu)]
pub enum WorkspaceError {
    #[snafu(display("i/o error while accessing `{}`", path.to_string_lossy()))]
    Fs {
        path: PathBuf,
        source: std::io::Error,
        backtrace: Backtrace,
    },

    #[snafu(display(
        "unable to parse workspace config `{}`",
        config_path.to_string_lossy()
    ))]
    Parse {
        config_path: PathBuf,
        #[snafu(source(from(toml::de::Error, Box::new)))]
        source: Box<toml::de::Error>,
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
        let repo_id = Self::required_value(manifest_path, "repo_id", raw.repo_id)?;
        let production = Self::required_value(manifest_path, "production", raw.production)?;
        let staging = Self::required_value(manifest_path, "staging", raw.staging)?;
        let siblings = raw
            .siblings
            .into_iter()
            .map(|(repo_id, sibling)| {
                let production = Self::required_value(
                    manifest_path,
                    &format!("siblings.{repo_id}.production"),
                    sibling.production,
                )?;
                let staging = Self::required_value(
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
        Self::validate_repo_key(manifest_path, "repo_id", &self.repo_id)?;

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
            Self::validate_repo_key(manifest_path, "sibling key", sibling_id)?;
        }

        Self::validate_unique_sibling_repositories(
            manifest_path,
            "production",
            self.siblings
                .iter()
                .map(|(id, sibling)| (id.as_str(), sibling.production.repository.as_str())),
        )?;
        Self::validate_unique_sibling_repositories(
            manifest_path,
            "staging",
            self.siblings
                .iter()
                .map(|(id, sibling)| (id.as_str(), sibling.staging.repository.as_str())),
        )?;

        Ok(())
    }

    pub fn active_endpoint(&self, staging: bool) -> &RepositoryEndpoint {
        if staging {
            &self.staging
        } else {
            &self.production
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

    fn required_value<T>(
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
        } else if RESERVED_WORKSPACE_NAMES.contains(&key) {
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
            Err(error) => Err(RepoManifestIoSnafu {
                path: manifest_path,
            }
            .into_error(error)),
        }
    }

    #[cfg(test)]
    pub fn from_path(path: &Path) -> Result<Self, RepoManifestError> {
        let manifest_path = path.canonicalize().with_context(|_| RepoManifestIoSnafu {
            path: path.to_path_buf(),
        })?;
        let contents =
            std::fs::read_to_string(&manifest_path).with_context(|_| RepoManifestIoSnafu {
                path: manifest_path.clone(),
            })?;
        Self::from_contents(manifest_path, &contents)
    }

    fn from_contents(manifest_path: PathBuf, contents: &str) -> Result<Self, RepoManifestError> {
        let manifest = toml::from_str::<RawRepoManifest>(contents).with_context(|_| {
            RepoManifestParseSnafu {
                manifest_path: manifest_path.clone(),
            }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyLocation {
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

impl LegacyLocation {
    pub fn endpoint(&self) -> RepositoryEndpoint {
        RepositoryEndpoint {
            repository: self.repository.clone(),
            base_url: self.base_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LegacyLocations(pub HashMap<String, LegacyLocation>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub locations: LegacyLocations,
}

impl Config {
    pub fn production() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            LegacyLocation {
                repository: "https://github.com/ethereum/EIPs.git".try_into().unwrap(),
                base_url: "https://eips.ethereum.org/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            LegacyLocation {
                repository: "https://github.com/ethereum/ERCs.git".try_into().unwrap(),
                base_url: "https://ercs.ethereum.org/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            locations: LegacyLocations(locations),
        }
    }

    pub fn staging() -> Self {
        let mut locations = HashMap::new();

        locations.insert(
            "EIPs".into(),
            LegacyLocation {
                repository: "https://github.com/eips-wg/EIPs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/EIPs/".try_into().unwrap(),
                identifying_commit: "0f44e2b94df4e504bb7b912f56ebd712db2ad396".into(),
            },
        );

        locations.insert(
            "ERCs".into(),
            LegacyLocation {
                repository: "https://github.com/eips-wg/ERCs.git".try_into().unwrap(),
                base_url: "https://eips-wg.github.io/ERCs/".try_into().unwrap(),
                identifying_commit: "8dd085d159cb123f545c272c0d871a5339550e79".into(),
            },
        );

        Self {
            locations: LegacyLocations(locations),
        }
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

    /// Local render filtering defaults.
    #[serde(default)]
    pub render: RenderSettings,

    /// Local search indexing defaults for rendered output.
    #[serde(default)]
    pub search: SearchSettings,
}

impl WorkspaceConfig {
    fn starter() -> Self {
        Self {
            server: ServerSettings::default(),
            site: SiteSettings::starter(),
            render: RenderSettings::default(),
            search: SearchSettings::default(),
        }
    }
}

/// Local render filtering settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RenderSettings {
    /// Proposal numbers to render for applicable local build and serve commands.
    #[serde(default)]
    pub only: Vec<ProposalNumber>,
}

/// Workspace-local search indexing settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchSettings {
    /// Whether `build-eips build` writes Pagefind search assets after rendering HTML.
    pub pagefind: bool,

    /// Optional rendered-HTML corpus output for debugging and external consumers.
    #[serde(default)]
    pub corpus: SearchCorpusSettings,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            pagefind: true,
            corpus: SearchCorpusSettings::default(),
        }
    }
}

/// Workspace-local rendered search corpus settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchCorpusSettings {
    /// Whether `build-eips build` writes the rendered search corpus artifact.
    pub enabled: bool,

    /// Which corpus views to emit.
    pub format: SearchCorpusFormat,

    /// Corpus output path relative to the rendered output directory.
    pub output: PathBuf,
}

impl Default for SearchCorpusSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            format: SearchCorpusFormat::default(),
            output: PathBuf::from("search-corpus.json"),
        }
    }
}

/// Rendered search corpus output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SearchCorpusFormat {
    /// Emit only document records.
    Documents,

    /// Emit only retrieval chunk records.
    Chunks,

    /// Emit document and retrieval chunk records.
    #[default]
    DocumentsAndChunks,
}

impl fmt::Display for SearchCorpusFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Documents => write!(f, "documents"),
            Self::Chunks => write!(f, "chunks"),
            Self::DocumentsAndChunks => write!(f, "documents-and-chunks"),
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
    #[serde(
        default,
        serialize_with = "serialize_optional_base_url",
        deserialize_with = "deserialize_optional_base_url"
    )]
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

fn serialize_optional_base_url<S>(base_url: &Option<Url>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match base_url {
        Some(base_url) => serializer.serialize_some(&format_base_url(base_url)),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_base_url<'de, D>(deserializer: D) -> Result<Option<Url>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Url>::deserialize(deserializer)
}

fn format_base_url(base_url: &Url) -> String {
    if base_url.path() == "/" && base_url.query().is_none() && base_url.fragment().is_none() {
        base_url[..Position::BeforePath].to_owned()
    } else {
        base_url.as_str().to_owned()
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
        let config = toml::from_str::<WorkspaceConfig>(&contents).with_context(|_| ParseSnafu {
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

    pub fn render_settings(&self) -> &RenderSettings {
        &self.config.render
    }

    pub fn search_settings(&self) -> &SearchSettings {
        &self.config.search
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

    use super::{
        default_workspace_config_text, discover_path, LoadedRepoManifest, LoadedWorkspaceConfig,
        RepoManifestError, SearchCorpusFormat, ServerBinding, ServerSettings, WorkspaceError,
        DEFAULT_SERVER_HOST, DEFAULT_SERVER_PORT, DEFAULT_SITE_BASE_URL, LOCAL_CONFIG_FILE,
        REPO_MANIFEST_FILE,
    };
    use crate::proposal::ProposalNumber;

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
        let workspace = TestWorkspace::new();

        assert!(LoadedRepoManifest::load(workspace.root())
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_repo_manifest_reports_parse_error() {
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(REPO_MANIFEST_FILE, "repo_id = [");

        let error = LoadedRepoManifest::from_path(&manifest_path).unwrap_err();

        assert!(matches!(error, RepoManifestError::Parse { .. }));
    }

    #[test]
    fn parses_repo_manifest_with_directional_siblings() {
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(
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
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(
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

        let manifest_path = workspace.write_file(
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

        let manifest_path = workspace.write_file(
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
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(REPO_MANIFEST_FILE, &manifest_text("theme", ""));

        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("repo_id `theme`"));
        assert!(reason.contains("reserved"));

        let manifest_path =
            workspace.write_file(REPO_MANIFEST_FILE, &manifest_text("Core/Meta", ""));
        let reason =
            manifest_invalid_reason(LoadedRepoManifest::from_path(&manifest_path).unwrap_err());

        assert!(reason.contains("repo_id `Core/Meta`"));
        assert!(reason.contains("single safe path component"));
    }

    #[test]
    fn repo_manifest_rejects_self_sibling() {
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(
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
    fn repo_manifest_rejects_duplicate_sibling_repositories_per_environment() {
        let workspace = TestWorkspace::new();
        let manifest_path = workspace.write_file(
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

        assert!(reason.contains("duplicate production sibling repository declaration"));
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
        assert!(config.search_settings().pagefind);
        assert!(!config.search_settings().corpus.enabled);
        assert_eq!(
            config.search_settings().corpus.format,
            SearchCorpusFormat::DocumentsAndChunks
        );
        assert_eq!(
            config.search_settings().corpus.output,
            PathBuf::from("search-corpus.json")
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
        assert!(original.contains(&format!("base_url = \"{DEFAULT_SITE_BASE_URL}\"")));
        assert!(original.contains("[render]"));
        assert!(original.contains("only = []"));
        assert!(original.contains("[search]"));
        assert!(original.contains("pagefind = true"));
        assert!(original.contains("[search.corpus]"));
        assert!(original.contains("enabled = false"));
        assert!(original.contains("format = \"documents-and-chunks\""));
        assert!(original.contains("output = \"search-corpus.json\""));
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
        assert!(config.render_settings().only.is_empty());
        assert!(config.search_settings().pagefind);
        assert!(!config.search_settings().corpus.enabled);
    }

    #[test]
    fn search_config_defaults_to_pagefind_enabled_when_missing() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(LOCAL_CONFIG_FILE, "");

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert!(config.search_settings().pagefind);
        assert!(!config.search_settings().corpus.enabled);
    }

    #[test]
    fn parses_workspace_config_search_settings() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[search]
pagefind = false

[search.corpus]
enabled = true
format = "chunks"
output = "debug/search-corpus.json"
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert!(!config.search_settings().pagefind);
        assert!(config.search_settings().corpus.enabled);
        assert_eq!(
            config.search_settings().corpus.format,
            SearchCorpusFormat::Chunks
        );
        assert_eq!(
            config.search_settings().corpus.output,
            PathBuf::from("debug/search-corpus.json")
        );
    }

    #[test]
    fn parses_workspace_config_render_only_settings() {
        let workspace = TestWorkspace::new();
        let config_path = workspace.write_file(
            LOCAL_CONFIG_FILE,
            r#"
[render]
only = [555, 678, 555]
"#,
        );

        let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

        assert_eq!(
            config.render_settings().only,
            vec![
                ProposalNumber::from_u32(555).unwrap(),
                ProposalNumber::from_u32(678).unwrap(),
                ProposalNumber::from_u32(555).unwrap(),
            ]
        );
    }

    #[test]
    fn missing_render_missing_only_and_empty_only_disable_filtering() {
        let cases = [
            ("missing render", ""),
            ("missing only", "[render]\n"),
            ("empty only", "[render]\nonly = []\n"),
        ];

        for (name, contents) in cases {
            let workspace = TestWorkspace::new();
            let config_path = workspace.write_file(LOCAL_CONFIG_FILE, contents);
            let config = LoadedWorkspaceConfig::from_path(&config_path).unwrap();

            assert!(
                config.render_settings().only.is_empty(),
                "expected `{name}` to disable render filtering"
            );
        }
    }

    #[test]
    fn workspace_config_render_only_rejects_non_positive_and_non_integer_values() {
        let cases = [
            ("zero", "only = [0]"),
            ("negative", "only = [-555]"),
            ("quoted", "only = [\"555\"]"),
            ("overflow", "only = [4294967296]"),
        ];

        for (name, contents) in cases {
            let workspace = TestWorkspace::new();
            let config_path =
                workspace.write_file(LOCAL_CONFIG_FILE, &format!("[render]\n{contents}\n"));
            let error = LoadedWorkspaceConfig::from_path(&config_path).unwrap_err();

            assert!(
                matches!(error, WorkspaceError::Parse { .. }),
                "expected `{name}` render only config to fail, got {error:?}"
            );
        }
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
