/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Build-side search indexing and generated search route state.

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use serde::Serialize;
use snafu::{OptionExt, ResultExt, Whatever};
use url::Url;

use crate::{config::SearchCorpusFormat, layout::CONTENT_DIR};

mod corpus;
mod pagefind;

const SEARCH_ROUTE_FILE: &str = "search.md";
const SEARCH_ROUTE_DIR: &str = "search";
pub(crate) const SEARCH_DATA_FILE: &str = "build_eips_search.toml";
pub(crate) const DEFAULT_CORPUS_OUTPUT_FILE: &str = "search-corpus.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchConfig {
    pub(crate) pagefind: bool,
    pub(crate) corpus: SearchCorpusConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            pagefind: true,
            corpus: SearchCorpusConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchCorpusConfig {
    pub(crate) enabled: bool,
    pub(crate) format: SearchCorpusFormat,
    pub(crate) output: PathBuf,
}

impl Default for SearchCorpusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            format: SearchCorpusFormat::default(),
            output: PathBuf::from(DEFAULT_CORPUS_OUTPUT_FILE),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchIndexRequest {
    pub(crate) output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchIndexSummary {
    pub(crate) pages_indexed: usize,
    pub(crate) output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchCorpusRequest {
    pub(crate) output_path: PathBuf,
    pub(crate) base_url: Url,
    pub(crate) config: SearchCorpusConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchCorpusSummary {
    pub(crate) documents: usize,
    pub(crate) chunks: usize,
    pub(crate) output_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SearchTemplateState {
    pub(crate) enabled: bool,
    pub(crate) base_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bundle_path: Option<String>,
}

impl SearchTemplateState {
    pub(crate) fn from_base_url(enabled: bool, base_url: &Url) -> Self {
        let base_path = normalized_base_path(base_url);
        let bundle_path = enabled.then(|| format!("{base_path}pagefind/"));

        Self {
            enabled,
            base_path,
            bundle_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchRouteSummary {
    pub(crate) route_path: PathBuf,
    pub(crate) data_path: PathBuf,
    pub(crate) state: SearchTemplateState,
}

#[derive(Debug, Serialize)]
struct SearchPageFrontMatter<'a> {
    title: &'static str,
    template: &'static str,
    extra: SearchPageExtra<'a>,
}

#[derive(Debug, Serialize)]
struct SearchPageExtra<'a> {
    search: &'a SearchTemplateState,
}

pub(crate) fn index_site(request: SearchIndexRequest) -> Result<SearchIndexSummary, Whatever> {
    pagefind::index_site(request)
}

pub(crate) fn reconcile_corpus(
    request: SearchCorpusRequest,
) -> Result<Option<SearchCorpusSummary>, Whatever> {
    corpus::reconcile_corpus(request)
}

pub(crate) fn ensure_search_route_available(content_path: &Path) -> Result<(), Whatever> {
    for relative_path in [
        Path::new(SEARCH_ROUTE_FILE).to_path_buf(),
        Path::new(SEARCH_ROUTE_DIR).join("index.md"),
        Path::new(SEARCH_ROUTE_DIR).join("_index.md"),
        Path::new(SEARCH_ROUTE_DIR).to_path_buf(),
    ] {
        let candidate = content_path.join(&relative_path);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => {
                snafu::whatever!(
                    "materialized content path `{}` collides with the generated `/search/` route; refusing to overwrite user-authored content",
                    candidate.to_string_lossy()
                );
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => {
                snafu::whatever!(
                    "unable to inspect potential search route collision `{}`: {error}",
                    candidate.to_string_lossy()
                );
            }
        }
    }

    Ok(())
}

pub(crate) fn write_search_route(
    repo_path: &Path,
    state: SearchTemplateState,
) -> Result<SearchRouteSummary, Whatever> {
    let route_path = repo_path.join(CONTENT_DIR).join(SEARCH_ROUTE_FILE);
    let data_path = repo_path.join("data").join(SEARCH_DATA_FILE);

    write_search_data_file(&data_path, &state)?;
    write_search_page_file(&route_path, &state)?;

    Ok(SearchRouteSummary {
        route_path,
        data_path,
        state,
    })
}

pub(crate) fn normalized_base_path(base_url: &Url) -> String {
    let path = base_url.path().trim_matches('/');

    if path.is_empty() {
        "/".to_owned()
    } else {
        format!("/{path}/")
    }
}

fn write_new_file(path: &Path, contents: &str, label: &str) -> Result<(), Whatever> {
    let parent = path.parent().with_whatever_context(|| {
        format!(
            "{label} output path `{}` has no parent directory",
            path.to_string_lossy()
        )
    })?;
    std::fs::create_dir_all(parent).with_whatever_context(|_| {
        format!(
            "unable to create {label} output directory `{}`",
            parent.to_string_lossy()
        )
    })?;

    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            snafu::whatever!(
                "{label} output `{}` already exists; refusing to overwrite it",
                path.to_string_lossy()
            );
        }
        Err(error) => {
            snafu::whatever!(
                "unable to create {label} output `{}`: {error}",
                path.to_string_lossy()
            );
        }
    };

    file.write_all(contents.as_bytes())
        .with_whatever_context(|_| {
            format!(
                "unable to write {label} output `{}`",
                path.to_string_lossy()
            )
        })?;

    Ok(())
}

fn write_search_data_file(data_path: &Path, state: &SearchTemplateState) -> Result<(), Whatever> {
    let mut contents =
        toml::to_string_pretty(state).whatever_context("unable to encode search state TOML")?;
    if !contents.ends_with('\n') {
        contents.push('\n');
    }
    write_new_file(data_path, &contents, "search state")
}

fn write_search_page_file(route_path: &Path, state: &SearchTemplateState) -> Result<(), Whatever> {
    let front_matter = SearchPageFrontMatter {
        title: "Search",
        template: "search.html",
        extra: SearchPageExtra { search: state },
    };
    let mut front_matter = toml::to_string_pretty(&front_matter)
        .whatever_context("unable to encode search route front matter")?;
    if !front_matter.ends_with('\n') {
        front_matter.push('\n');
    }
    let contents = format!("+++\n{front_matter}+++\n");
    write_new_file(route_path, &contents, "search route")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;
    use toml::Value as TomlValue;
    use url::Url;

    use super::{
        ensure_search_route_available, write_search_route, SearchTemplateState, SEARCH_DATA_FILE,
    };

    fn contains_forbidden_pagefind_reference(path: &Path, src_path: &Path) -> bool {
        let relative = path.strip_prefix(src_path).unwrap();
        let allowed_local_call = ["pagefind", "::index_site"].concat();
        let direct_path = ["pagefind", "::"].concat();
        let absolute_direct_path = ["::", "pagefind", "::"].concat();
        let use_direct = ["use", "pagefind"].concat();
        let use_absolute_direct = ["use", "::", "pagefind"].concat();
        let extern_crate = ["extern", "crate", "pagefind"].concat();

        std::fs::read_to_string(path)
            .map(|contents| {
                contents.lines().any(|line| {
                    let code = line.split("//").next().unwrap_or_default();
                    let compact = code
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect::<String>();

                    if relative == Path::new("search/mod.rs")
                        && compact.contains(&allowed_local_call)
                    {
                        return false;
                    }

                    compact.contains(&absolute_direct_path)
                        || compact.contains(&direct_path)
                        || compact.starts_with(&use_direct)
                        || compact.starts_with(&use_absolute_direct)
                        || compact.starts_with(&extern_crate)
                })
            })
            .unwrap_or(false)
    }

    fn contains_forbidden_scraper_reference(path: &Path) -> bool {
        let absolute_direct_path = ["::", "scraper", "::"].concat();
        let direct_path = ["scraper", "::"].concat();
        let use_direct = ["use", "scraper"].concat();
        let use_absolute_direct = ["use", "::", "scraper"].concat();
        let extern_crate = ["extern", "crate", "scraper"].concat();

        std::fs::read_to_string(path)
            .map(|contents| {
                contents.lines().any(|line| {
                    let code = line.split("//").next().unwrap_or_default();
                    let compact = code
                        .chars()
                        .filter(|character| !character.is_whitespace())
                        .collect::<String>();

                    compact.contains(&absolute_direct_path)
                        || compact.contains(&direct_path)
                        || compact.starts_with(&use_direct)
                        || compact.starts_with(&use_absolute_direct)
                        || compact.starts_with(&extern_crate)
                })
            })
            .unwrap_or(false)
    }

    #[test]
    fn pagefind_crate_imports_stay_inside_pagefind_module() {
        let src_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed_path = src_path.join("search/pagefind.rs");
        let violations = walkdir::WalkDir::new(&src_path)
            .into_iter()
            .filter_map(Result::ok)
            .map(|entry| entry.into_path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .filter(|path| path != &allowed_path)
            .filter(|path| contains_forbidden_pagefind_reference(path, &src_path))
            .map(|path| path.strip_prefix(&src_path).unwrap().to_path_buf())
            .collect::<Vec<PathBuf>>();

        assert!(
            violations.is_empty(),
            "Pagefind crate imports must stay in src/search/pagefind.rs, found {violations:?}"
        );
    }

    #[test]
    fn scraper_crate_imports_stay_inside_corpus_module() {
        let src_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let allowed_dir = src_path.join("search/corpus");
        let violations = walkdir::WalkDir::new(&src_path)
            .into_iter()
            .filter_map(Result::ok)
            .map(|entry| entry.into_path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .filter(|path| !path.starts_with(&allowed_dir))
            .filter(|path| contains_forbidden_scraper_reference(path))
            .map(|path| path.strip_prefix(&src_path).unwrap().to_path_buf())
            .collect::<Vec<PathBuf>>();

        assert!(
            violations.is_empty(),
            "Scraper crate imports must stay in src/search/corpus/, found {violations:?}"
        );
    }

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn front_matter(path: &Path) -> TomlValue {
        let contents = std::fs::read_to_string(path).unwrap();
        let front_matter = contents
            .strip_prefix("+++\n")
            .unwrap()
            .split_once("\n+++\n")
            .unwrap()
            .0;
        toml::from_str(front_matter).unwrap()
    }

    #[test]
    fn search_template_state_normalizes_base_path_and_bundle_path() {
        let root = Url::parse("https://wg-eips.ritovision.com/").unwrap();
        let subpath = Url::parse("https://eips-wg.github.io/EIPs/").unwrap();

        assert_eq!(
            SearchTemplateState::from_base_url(true, &root),
            SearchTemplateState {
                enabled: true,
                base_path: "/".to_owned(),
                bundle_path: Some("/pagefind/".to_owned()),
            }
        );
        assert_eq!(
            SearchTemplateState::from_base_url(true, &subpath),
            SearchTemplateState {
                enabled: true,
                base_path: "/EIPs/".to_owned(),
                bundle_path: Some("/EIPs/pagefind/".to_owned()),
            }
        );
        assert_eq!(
            SearchTemplateState::from_base_url(false, &subpath),
            SearchTemplateState {
                enabled: false,
                base_path: "/EIPs/".to_owned(),
                bundle_path: None,
            }
        );
    }

    #[test]
    fn search_route_collision_checks_all_reserved_route_paths() {
        for relative in [
            "content/search.md",
            "content/search/placeholder.txt",
            "content/search/index.md",
            "content/search/_index.md",
        ] {
            let temp = TempDir::new().unwrap();
            let content_path = temp.path().join("content");
            write_file(temp.path(), relative, "user content\n");

            let error = ensure_search_route_available(&content_path)
                .unwrap_err()
                .to_string();

            assert!(error.contains("collides with the generated `/search/` route"));
            assert!(error.contains("content/search"));
        }
    }

    #[test]
    fn search_route_writes_page_and_shared_template_state() {
        let temp = TempDir::new().unwrap();
        let state = SearchTemplateState::from_base_url(
            true,
            &Url::parse("https://eips-wg.github.io/EIPs/").unwrap(),
        );

        let summary = write_search_route(temp.path(), state.clone()).unwrap();

        assert_eq!(summary.route_path, temp.path().join("content/search.md"));
        assert_eq!(
            summary.data_path,
            temp.path().join("data").join(SEARCH_DATA_FILE)
        );
        assert_eq!(summary.state, state);

        let route_front_matter = front_matter(&summary.route_path);
        assert_eq!(route_front_matter["title"].as_str(), Some("Search"));
        assert_eq!(route_front_matter["template"].as_str(), Some("search.html"));
        assert_eq!(
            route_front_matter["extra"]["search"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            route_front_matter["extra"]["search"]["base_path"].as_str(),
            Some("/EIPs/")
        );
        assert_eq!(
            route_front_matter["extra"]["search"]["bundle_path"].as_str(),
            Some("/EIPs/pagefind/")
        );

        let data = std::fs::read_to_string(&summary.data_path).unwrap();
        let data: TomlValue = toml::from_str(&data).unwrap();
        assert_eq!(data["enabled"].as_bool(), Some(true));
        assert_eq!(data["base_path"].as_str(), Some("/EIPs/"));
        assert_eq!(data["bundle_path"].as_str(), Some("/EIPs/pagefind/"));
    }

    #[test]
    fn disabled_search_state_omits_bundle_path() {
        let temp = TempDir::new().unwrap();
        let state = SearchTemplateState::from_base_url(
            false,
            &Url::parse("https://example.test/").unwrap(),
        );

        let summary = write_search_route(temp.path(), state).unwrap();

        let route_front_matter = front_matter(&summary.route_path);
        assert_eq!(
            route_front_matter["extra"]["search"]["enabled"].as_bool(),
            Some(false)
        );
        assert!(route_front_matter["extra"]["search"]
            .as_table()
            .unwrap()
            .get("bundle_path")
            .is_none());

        let data = std::fs::read_to_string(&summary.data_path).unwrap();
        let data: TomlValue = toml::from_str(&data).unwrap();
        assert_eq!(data["enabled"].as_bool(), Some(false));
        assert!(data.as_table().unwrap().get("bundle_path").is_none());
    }
}
