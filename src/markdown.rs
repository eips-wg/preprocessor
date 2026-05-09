/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use chrono::DateTime;

use citationberg::Style;
use eipw_preamble::Preamble;

use hayagriva::archive::ArchivedStyle;
use hayagriva::{BibliographyDriver, BibliographyRequest, CitationItem, CitationRequest};
use lazy_static::lazy_static;

use log::{debug, info, log_enabled, warn, Level};
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

use pulldown_cmark_to_cmark::cmark;

use regex::Regex;

use serde::{Deserialize, Serialize};

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs::read_to_string;
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};

use snafu::{whatever, OptionExt, ResultExt, Whatever};

use toml::Value;

use toml_datetime::Datetime;

use walkdir::WalkDir;

use iref::IriRefBuf;

use crate::{
    progress::ProgressIteratorExt,
    proposal::{
        path_component_proposal_number, proposal_number_from_content_markdown_path, OnlyRenderPlan,
        ProposalAssetKind, ProposalNumber, ProposalReference,
    },
};

#[derive(Clone, Copy)]
enum MissingPathMode {
    Error,
    Ignore,
}

impl MissingPathMode {
    fn should_ignore_io_error(self, error: &std::io::Error) -> bool {
        matches!(self, Self::Ignore)
            && matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
    }

    fn should_ignore_walkdir_error(self, error: &walkdir::Error) -> bool {
        error
            .io_error()
            .map(|error| self.should_ignore_io_error(error))
            .unwrap_or(false)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Author {
    name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    github: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

impl From<Author> for Value {
    fn from(value: Author) -> Self {
        // TODO: Hacky way to implement this conversion...
        toml::from_str(&toml::to_string(&value).unwrap()).unwrap()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FrontMatter {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    title: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    date: Option<Datetime>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated: Option<Datetime>,

    #[serde(default, skip_serializing_if = "is_zero")]
    weight: usize,

    #[serde(default, skip_serializing_if = "is_false")]
    draft: bool,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    slug: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    path: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    aliases: Vec<PathBuf>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    authors: Vec<String>,

    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    in_search_index: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    template: Option<PathBuf>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    taxonomies: HashMap<String, Vec<String>>,

    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    extra: HashMap<String, Value>,
}

fn default_true() -> bool {
    true
}
fn is_true(x: &bool) -> bool {
    *x
}
fn is_false(x: &bool) -> bool {
    !*x
}
fn is_zero(x: &usize) -> bool {
    *x == 0
}

impl Default for FrontMatter {
    fn default() -> Self {
        Self {
            title: Default::default(),
            description: Default::default(),
            date: Default::default(),
            updated: Default::default(),
            weight: Default::default(),
            draft: Default::default(),
            slug: Default::default(),
            path: Default::default(),
            aliases: Default::default(),
            authors: Default::default(),
            in_search_index: true,
            template: Default::default(),
            taxonomies: Default::default(),
            extra: Default::default(),
        }
    }
}

fn filesystem_modified(p: &Path) -> Result<Datetime, Whatever> {
    let metadata = std::fs::metadata(p)
        .with_whatever_context(|e| format!("unable to read metadata for `{}`: {e}", p.display()))?;
    let modified = metadata.modified().with_whatever_context(|e| {
        format!(
            "unable to read filesystem modified time for `{}`: {e}",
            p.display()
        )
    })?;
    let date_time: DateTime<chrono::Utc> = modified.into();
    Ok(date_time.to_rfc3339().parse().unwrap())
}

fn last_modified(p: &Path) -> Result<Datetime, Whatever> {
    // TODO: Replace this with `git2`
    let mut command = std::process::Command::new("git");
    command
        .current_dir(p.parent().unwrap())
        .arg("log")
        .arg("-1")
        .arg("--pretty=format:%ct")
        .arg("--")
        .arg(p.file_name().unwrap());

    let output = command
        .output()
        .with_whatever_context(|e| format!("failed to execute {:?}: {e}", command))?;

    if !output.status.success() {
        let err_str = std::str::from_utf8(&output.stderr).unwrap_or("<non-utf-8>");
        whatever!("command {:?} failed: {err_str}", command);
    }

    let date_str = std::str::from_utf8(&output.stdout)
        .with_whatever_context(|e| format!("command {:?} output not UTF-8: {e}", command))?
        .trim();

    if date_str.is_empty() {
        debug!(
            "falling back to filesystem modified time for `{}` because git has no timestamp for the current path",
            p.to_string_lossy()
        );
        return filesystem_modified(p);
    }

    let unix: i64 = date_str.parse().with_whatever_context(|e| {
        let err_str = std::str::from_utf8(&output.stderr).unwrap_or("<non-utf-8>");
        format!(
            "unable to parse timestamp `{date_str}` from {:?}: {e}\n{err_str}",
            command
        )
    })?;

    let date_time = DateTime::from_timestamp(unix, 0).unwrap();

    Ok(date_time.to_rfc3339().parse().unwrap())
}

fn write_file(path: &Path, front_matter: FrontMatter, body: &str) -> std::io::Result<()> {
    let mut output = std::fs::OpenOptions::new()
        .truncate(true)
        .write(true)
        .open(path)?;
    writeln!(output, "+++")?;
    writeln!(output, "{}", toml::to_string(&front_matter).unwrap())?;
    writeln!(output, "+++")?;
    writeln!(output, "{}", body)?;
    Ok(())
}

lazy_static! {
    // Matches GitHub usernames.
    static ref RE_GITHUB: Regex = Regex::new(r"^([^()<>,@]+) \(@([a-zA-Z\d-]+)\)$").unwrap();
    // Matches email addresses.
    static ref RE_EMAIL: Regex = Regex::new(r"^([^()<>,@]+) <([^@][^>]*@[^>]+\.[^>]+)>$").unwrap();
    // Matches a GitHub username plus email address.
    static ref RE_BOTH: Regex =
        Regex::new(r"^([^()<>,@]+) \(@([a-zA-Z\d-]+)\) <([^@][^>]*@[^>]+\.[^>]+)>$").unwrap();
    // Matches just a name.
    static ref RE_NAME: Regex = Regex::new(r"^([^()<>,@]+)$").unwrap();
}

fn extract_authors(value: &str) -> Result<Vec<Author>, Whatever> {
    let mut authors = Vec::new();
    let items = value.split(',').map(|x| x.trim());
    for item in items {
        if let Some(both) = RE_BOTH.captures(item) {
            authors.push(Author {
                name: both.get(1).unwrap().as_str().into(),
                github: Some(both.get(2).unwrap().as_str().into()),
                email: Some(both.get(3).unwrap().as_str().into()),
            });
        } else if let Some(email) = RE_EMAIL.captures(item) {
            authors.push(Author {
                name: email.get(1).unwrap().as_str().into(),
                github: None,
                email: Some(email.get(2).unwrap().as_str().into()),
            });
        } else if let Some(github) = RE_GITHUB.captures(item) {
            authors.push(Author {
                name: github.get(1).unwrap().as_str().into(),
                email: None,
                github: Some(github.get(2).unwrap().as_str().into()),
            });
        } else if let Some(name) = RE_NAME.captures(item) {
            authors.push(Author {
                name: name.get(1).unwrap().as_str().into(),
                email: None,
                github: None,
            });
        } else {
            whatever!("invalid author");
        }
    }
    Ok(authors)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProposalAssetPathResolution {
    NotAProposalAsset,
    ProposalAsset(ProposalAssetPath),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProposalAssetPath {
    pub(crate) target_proposal_number: ProposalNumber,
    pub(crate) content_relative_asset_path: PathBuf,
    pub(crate) asset_relative_path: PathBuf,
    pub(crate) kind: ProposalAssetKind,
    pub(crate) rendered_target_path: String,
}

#[derive(Debug)]
struct DecodedPathSegment {
    value: String,
}

pub(crate) fn resolve_proposal_asset_path(
    content_root: &Path,
    source_md_path: &Path,
    iri_path: &str,
) -> Result<ProposalAssetPathResolution, Whatever> {
    let source_parent = source_md_path.parent().with_whatever_context(|| {
        format!(
            "source markdown path `{}` has no parent",
            source_md_path.to_string_lossy()
        )
    })?;
    let normalized_root = normalize_path_lexically(content_root);
    if !raw_iri_resolves_to_proposal_asset_candidate(
        &normalized_root,
        content_root,
        source_parent,
        iri_path,
    ) {
        return Ok(ProposalAssetPathResolution::NotAProposalAsset);
    }

    let decoded_segments = decode_iri_path_segments(iri_path)?;
    reject_unsafe_asset_segments(&decoded_segments)?;

    let target_path =
        resolve_url_path_lexically(content_root, source_parent, iri_path, &decoded_segments);
    let normalized_target = normalize_path_lexically(&target_path);
    let Ok(content_relative_path) = normalized_target.strip_prefix(&normalized_root) else {
        return Ok(ProposalAssetPathResolution::NotAProposalAsset);
    };

    let Some((target_proposal_number, asset_relative_path)) =
        proposal_asset_parts(content_relative_path)
    else {
        return Ok(ProposalAssetPathResolution::NotAProposalAsset);
    };

    let kind = if iri_path.ends_with(".md") {
        ProposalAssetKind::Markdown
    } else {
        ProposalAssetKind::Static
    };
    let rendered_target_path =
        rendered_asset_path(target_proposal_number, &asset_relative_path, kind)?;

    Ok(ProposalAssetPathResolution::ProposalAsset(
        ProposalAssetPath {
            target_proposal_number,
            content_relative_asset_path: content_relative_path.to_path_buf(),
            asset_relative_path,
            kind,
            rendered_target_path,
        },
    ))
}

pub(crate) fn absolute_rendered_path_for_content_path(
    content_relative_path: &Path,
) -> Result<String, Whatever> {
    if content_relative_path == Path::new("_index.md") {
        return Ok("/".to_owned());
    }

    if let Some(proposal_number) = proposal_number_from_content_markdown_path(content_relative_path)
    {
        return Ok(format!("/{proposal_number}/"));
    }

    if let Some((proposal_number, asset_relative_path)) =
        proposal_asset_parts(content_relative_path)
    {
        return rendered_asset_path(
            proposal_number,
            &asset_relative_path,
            ProposalAssetKind::from_path(&asset_relative_path),
        );
    }

    snafu::whatever!(
        "content path `{}` is not a proposal page or proposal asset",
        content_relative_path.to_string_lossy()
    );
}

pub(crate) fn relative_url_from_rendered_paths(
    source_rendered_path: &str,
    target_rendered_path: &str,
) -> Result<String, Whatever> {
    let source_segments = rendered_directory_segments(source_rendered_path)?;
    let (target_segments, target_is_directory) = rendered_path_segments(target_rendered_path)?;
    let common_len = source_segments
        .iter()
        .zip(target_segments.iter())
        .take_while(|(source, target)| source == target)
        .count();

    let mut relative_segments = Vec::new();
    relative_segments.extend(std::iter::repeat_n(
        "..",
        source_segments.len() - common_len,
    ));
    relative_segments.extend(target_segments[common_len..].iter().copied());

    let mut relative_url = if relative_segments.is_empty() {
        ".".to_owned()
    } else {
        relative_segments.join("/")
    };
    if target_is_directory && !relative_url.ends_with('/') {
        relative_url.push('/');
    }

    Ok(relative_url)
}

pub(crate) fn proposal_asset_exists_in_content_tree(
    content_root: &Path,
    content_relative_asset_path: &Path,
) -> bool {
    if !content_relative_asset_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return false;
    }

    let Ok(canonical_content_root) = std::fs::canonicalize(content_root) else {
        return false;
    };
    let Ok(canonical_target) =
        std::fs::canonicalize(content_root.join(content_relative_asset_path))
    else {
        return false;
    };

    if !canonical_target.starts_with(canonical_content_root) {
        return false;
    }

    std::fs::metadata(canonical_target)
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
}

fn decode_iri_path_segments(iri_path: &str) -> Result<Vec<DecodedPathSegment>, Whatever> {
    iri_path
        .split('/')
        .map(|segment| {
            Ok(DecodedPathSegment {
                value: percent_decode_url_segment(segment)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
}

fn percent_decode_url_segment(segment: &str) -> Result<String, Whatever> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        if index + 2 >= bytes.len() {
            snafu::whatever!("invalid percent encoding in URL path segment `{segment}`");
        }

        let high = hex_value(bytes[index + 1]).with_whatever_context(|| {
            format!("invalid percent encoding in URL path segment `{segment}`")
        })?;
        let low = hex_value(bytes[index + 2]).with_whatever_context(|| {
            format!("invalid percent encoding in URL path segment `{segment}`")
        })?;
        let value = (high << 4) | low;
        if matches!(value, b'/' | b'\\' | b'\0') {
            snafu::whatever!("unsafe percent encoding in URL path segment `{segment}`");
        }
        decoded.push(value);
        index += 3;
    }

    String::from_utf8(decoded)
        .with_whatever_context(|_| format!("URL path segment `{segment}` is not UTF-8"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn reject_unsafe_asset_segments(segments: &[DecodedPathSegment]) -> Result<(), Whatever> {
    let Some(assets_index) = segments.windows(2).position(|window| {
        path_component_proposal_number(Some(OsStr::new(window[0].value.as_str()))).is_some()
            && window[1].value == "assets"
    }) else {
        return Ok(());
    };

    for segment in &segments[assets_index + 2..] {
        if segment.value.is_empty() {
            continue;
        }
        if segment.value == "." || segment.value == ".." {
            snafu::whatever!("unsafe proposal asset path segment `{}`", segment.value);
        }
        if segment.value.contains(['/', '\\', '\0']) {
            snafu::whatever!("unsafe proposal asset path segment `{}`", segment.value);
        }
    }

    Ok(())
}

fn resolve_url_path_lexically(
    content_root: &Path,
    source_parent: &Path,
    iri_path: &str,
    decoded_segments: &[DecodedPathSegment],
) -> PathBuf {
    let mut path = if iri_path.starts_with('/') {
        content_root.to_path_buf()
    } else {
        source_parent.to_path_buf()
    };

    for segment in decoded_segments {
        match segment.value.as_str() {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            _ => path.push(&segment.value),
        }
    }

    path
}

fn raw_iri_resolves_to_proposal_asset_candidate(
    normalized_root: &Path,
    content_root: &Path,
    source_parent: &Path,
    iri_path: &str,
) -> bool {
    let mut path = if iri_path.starts_with('/') {
        content_root.to_path_buf()
    } else {
        source_parent.to_path_buf()
    };
    let raw_segments = iri_path.split('/').collect::<Vec<_>>();

    for (index, segment) in raw_segments.iter().enumerate() {
        match *segment {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            _ => path.push(segment),
        }

        let normalized_path = normalize_path_lexically(&path);
        let Ok(content_relative_path) = normalized_path.strip_prefix(normalized_root) else {
            continue;
        };

        if proposal_asset_parts(content_relative_path).is_some() {
            return true;
        }

        if proposal_asset_dir_prefix(content_relative_path).is_some()
            && raw_segments[index + 1..]
                .iter()
                .any(|remaining_segment| !remaining_segment.is_empty())
        {
            return true;
        }
    }

    false
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn proposal_asset_dir_prefix(content_relative_path: &Path) -> Option<ProposalNumber> {
    let mut components = content_relative_path.components();
    let proposal_component = components.next()?;
    let assets_component = components.next()?;
    if components.next().is_some() || assets_component.as_os_str() != OsStr::new("assets") {
        return None;
    }

    path_component_proposal_number(Some(proposal_component.as_os_str()))
}

fn proposal_asset_parts(content_relative_path: &Path) -> Option<(ProposalNumber, PathBuf)> {
    let mut components = content_relative_path.components();
    let proposal_component = components.next()?;
    let assets_component = components.next()?;
    if assets_component.as_os_str() != OsStr::new("assets") {
        return None;
    }

    let proposal_number = path_component_proposal_number(Some(proposal_component.as_os_str()))?;
    let asset_relative_path = components.as_path();
    if asset_relative_path.as_os_str().is_empty() {
        return None;
    }
    if !asset_relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return None;
    }

    Some((proposal_number, asset_relative_path.to_path_buf()))
}

fn rendered_asset_path(
    proposal_number: ProposalNumber,
    asset_relative_path: &Path,
    kind: ProposalAssetKind,
) -> Result<String, Whatever> {
    let mut segments = vec![proposal_number.to_string(), "assets".to_owned()];
    let asset_segments = asset_relative_path
        .components()
        .map(|component| match component {
            Component::Normal(part) => {
                part.to_str().map(str::to_owned).with_whatever_context(|| {
                    format!(
                        "non-UTF-8 proposal asset path `{}`",
                        asset_relative_path.to_string_lossy()
                    )
                })
            }
            _ => snafu::whatever!(
                "unsupported proposal asset path component in `{}`",
                asset_relative_path.to_string_lossy()
            ),
        })
        .collect::<Result<Vec<_>, _>>()?;

    let last_index = asset_segments.len().saturating_sub(1);
    for (index, mut segment) in asset_segments.into_iter().enumerate() {
        if kind == ProposalAssetKind::Markdown && index == last_index {
            segment = segment
                .strip_suffix(".md")
                .with_whatever_context(|| {
                    format!(
                        "proposal asset markdown path `{}` does not end in `.md`",
                        asset_relative_path.to_string_lossy()
                    )
                })?
                .to_owned();
        }
        segments.push(percent_encode_url_segment(&segment));
    }

    let mut rendered_path = format!("/{}", segments.join("/"));
    if kind == ProposalAssetKind::Markdown {
        rendered_path.push('/');
    }

    Ok(rendered_path)
}

fn percent_encode_url_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn rendered_directory_segments(rendered_path: &str) -> Result<Vec<&str>, Whatever> {
    let (mut segments, is_directory) = rendered_path_segments(rendered_path)?;
    if !is_directory {
        segments.pop();
    }
    Ok(segments)
}

fn rendered_path_segments(rendered_path: &str) -> Result<(Vec<&str>, bool), Whatever> {
    if !rendered_path.starts_with('/') {
        snafu::whatever!("rendered path `{rendered_path}` is not absolute");
    }

    let is_directory = rendered_path.ends_with('/');
    let segments = rendered_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    Ok((segments, is_directory))
}

fn is_generated_zola_markdown(contents: &str) -> bool {
    contents.starts_with("+++\n")
}

pub fn preprocess(root_path: &Path, only_plan: Option<&OnlyRenderPlan>) -> Result<(), Whatever> {
    let dir = std::fs::read_dir(root_path).with_whatever_context(|_| {
        format!("could not read directory `{}`", root_path.to_string_lossy())
    })?;
    let dirs: Vec<_> = dir.collect();

    info!("preprocessing markdown");

    for entry in dirs.into_iter().progress_ext("Markdown") {
        let entry = entry.with_whatever_context(|_| {
            format!(
                "could not read directory entry in `{}`",
                root_path.to_string_lossy()
            )
        })?;
        let entry_path = entry.path();

        let file_type = entry.file_type().with_whatever_context(|_| {
            format!(
                "could not get file type for `{}`",
                entry.path().to_string_lossy()
            )
        })?;

        if log_enabled!(Level::Debug) {
            let relative = match entry_path.strip_prefix(root_path) {
                Ok(r) => r,
                Err(_) => &entry_path,
            };
            match relative.with_extension("").to_string_lossy().parse::<u64>() {
                Ok(n) => debug!("preprocessing {}", n),
                Err(_) => debug!("preprocessing `{}`", relative.to_string_lossy()),
            }
        }

        if file_type.is_dir() {
            let relative_path = entry_path
                .strip_prefix(root_path)
                .with_whatever_context(|_| {
                    format!(
                        "content directory entry `{}` is outside `{}`",
                        entry_path.to_string_lossy(),
                        root_path.to_string_lossy()
                    )
                })?;
            if only_plan
                .map(|plan| plan.should_process_proposal_dir(relative_path))
                .unwrap_or(true)
            {
                let index_path = entry_path.join("index.md");
                if let Some(plan) = only_plan {
                    let index_relative_path = relative_path.join("index.md");
                    if plan.should_preprocess_markdown(&index_relative_path) {
                        process_eip(root_path, &index_path, only_plan, MissingPathMode::Error)?;
                    }
                } else {
                    process_eip(root_path, &index_path, only_plan, MissingPathMode::Error)?;
                }
                process_assets(root_path, &entry_path, only_plan, MissingPathMode::Error)?;
            }
        } else if entry_path.extension().and_then(OsStr::to_str) == Some("md") {
            let relative_path = entry_path
                .strip_prefix(root_path)
                .with_whatever_context(|_| {
                    format!(
                        "content file `{}` is outside `{}`",
                        entry_path.to_string_lossy(),
                        root_path.to_string_lossy()
                    )
                })?;
            if only_plan
                .map(|plan| plan.should_preprocess_markdown(relative_path))
                .unwrap_or(true)
            {
                process_eip(root_path, &entry_path, only_plan, MissingPathMode::Error)?;
            }
        }
    }

    Ok(())
}

pub fn preprocess_paths(
    root_path: &Path,
    relative_paths: &BTreeSet<PathBuf>,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<(), Whatever> {
    let mut eips = BTreeSet::new();
    let mut asset_dirs = BTreeSet::new();

    for relative_path in relative_paths {
        let Ok(content_relative_path) = relative_path.strip_prefix("content") else {
            continue;
        };

        if content_relative_path.as_os_str().is_empty() {
            continue;
        }

        if content_relative_path.extension().and_then(OsStr::to_str) != Some("md") {
            continue;
        }

        if only_plan
            .map(|plan| !plan.should_sync_dirty_path(relative_path))
            .unwrap_or(false)
        {
            continue;
        }

        let mut components = content_relative_path.components();
        let Some(first_component) = components.next() else {
            continue;
        };

        if matches!(
            components.next(),
            Some(component) if component.as_os_str() == OsStr::new("assets")
        ) {
            let proposal_dir = root_path.join(first_component.as_os_str());
            asset_dirs.insert(proposal_dir);
            continue;
        }

        let path = root_path.join(content_relative_path);
        eips.insert(path);
    }

    for path in eips {
        process_eip(root_path, &path, only_plan, MissingPathMode::Ignore)?;
    }

    for path in asset_dirs {
        process_assets(root_path, &path, only_plan, MissingPathMode::Ignore)?;
    }

    Ok(())
}

fn path_to_at(root: &Path, parent: &Path, input: &str) -> Result<String, Whatever> {
    let croot = std::fs::canonicalize(root).with_whatever_context(|_| {
        format!("could not canonicalize `{}`", root.to_string_lossy())
    })?;

    let child = if input.starts_with("/") {
        let mut path = Path::new(input);
        path = path.strip_prefix("/").unwrap();
        root.join(path)
    } else {
        parent.join(Path::new(input))
    };

    let cchild = canonicalize_md(&child)?;
    let relative = cchild.strip_prefix(&croot).expect("child not in root");
    Ok(format!("@/{}", relative.to_str().unwrap()))
}

fn canonicalize_md(path: &Path) -> Result<PathBuf, Whatever> {
    let first_error = match std::fs::canonicalize(path) {
        Ok(canon) => return Ok(canon),
        Err(e) => e,
    };

    if path.extension() != Some(OsStr::new("md")) {
        panic!("canonicalizing non-md file: {}", path.to_string_lossy());
    }

    let alt_path = match path.file_name().and_then(OsStr::to_str) {
        Some("index.md") => {
            let mut new_path = path.to_owned();
            new_path.pop();
            new_path.set_extension("md");
            new_path
        }
        _ => {
            let mut new_path = path.with_extension("");
            new_path.push("index.md");
            new_path
        }
    };

    if let Ok(canon) = std::fs::canonicalize(&alt_path) {
        return Ok(canon);
    };

    Err(first_error).with_whatever_context(|_| {
        format!(
            "could not canonicalize `{}` or `{}`",
            path.to_string_lossy(),
            alt_path.to_string_lossy()
        )
    })
}

enum AssetLinkRewrite {
    Rewrite(String),
    FallThrough,
}

fn resolve_asset_link_rewrite(
    root: &Path,
    source_md_path: &Path,
    only_plan: Option<&OnlyRenderPlan>,
    iri_path: &str,
) -> Result<Option<AssetLinkRewrite>, Whatever> {
    if iri_path.is_empty() {
        return Ok(None);
    }

    let ProposalAssetPathResolution::ProposalAsset(asset_path) =
        resolve_proposal_asset_path(root, source_md_path, iri_path)?
    else {
        return Ok(None);
    };

    if let Some(plan) = only_plan {
        if let Some(public_url) =
            plan.public_url_for_omitted_proposal_asset(&asset_path.content_relative_asset_path)
        {
            return Ok(Some(AssetLinkRewrite::Rewrite(public_url)));
        }

        if plan.has_proposal_asset(&asset_path.content_relative_asset_path) {
            validate_local_proposal_asset(root, source_md_path, iri_path, &asset_path)?;
            return local_asset_link_rewrite(root, source_md_path, asset_path).map(Some);
        }

        snafu::whatever!(
            "proposal asset link `{iri_path}` in `{}` resolved to `{}` but was not found in targeted render inventory",
            source_md_path.to_string_lossy(),
            asset_path.content_relative_asset_path.to_string_lossy()
        );
    }

    validate_local_proposal_asset(root, source_md_path, iri_path, &asset_path)?;
    local_asset_link_rewrite(root, source_md_path, asset_path).map(Some)
}

fn validate_local_proposal_asset(
    root: &Path,
    source_md_path: &Path,
    iri_path: &str,
    asset_path: &ProposalAssetPath,
) -> Result<(), Whatever> {
    if proposal_asset_exists_in_content_tree(root, &asset_path.content_relative_asset_path) {
        return Ok(());
    }

    snafu::whatever!(
        "proposal asset link `{iri_path}` in `{}` resolved to missing asset `{}`",
        source_md_path.to_string_lossy(),
        asset_path.content_relative_asset_path.to_string_lossy()
    );
}

fn local_asset_link_rewrite(
    root: &Path,
    source_md_path: &Path,
    asset_path: ProposalAssetPath,
) -> Result<AssetLinkRewrite, Whatever> {
    if asset_path.kind == ProposalAssetKind::Markdown {
        return Ok(AssetLinkRewrite::FallThrough);
    }

    let source_relative_path = source_md_path
        .strip_prefix(root)
        .with_whatever_context(|_| {
            format!(
                "source markdown `{}` is outside content root `{}`",
                source_md_path.to_string_lossy(),
                root.to_string_lossy()
            )
        })?;
    let source_rendered_path = absolute_rendered_path_for_content_path(source_relative_path)?;
    let relative_url =
        relative_url_from_rendered_paths(&source_rendered_path, &asset_path.rendered_target_path)?;

    Ok(AssetLinkRewrite::Rewrite(relative_url))
}

fn append_query_and_fragment(mut url: String, iri_ref: &IriRefBuf) -> String {
    if let Some(query) = iri_ref.query() {
        url.push('?');
        url.push_str(query.as_str());
    }
    if let Some(fragment) = iri_ref.fragment() {
        url.push('#');
        url.push_str(fragment.as_str());
    }
    url
}

fn fix_links<'a, 'b>(
    root: &'a Path,
    source_md_path: &'a Path,
    only_plan: Option<&'a OnlyRenderPlan>,
    mut e: Event<'b>,
) -> Result<Event<'b>, Whatever> {
    match &mut e {
        Event::Start(Tag::Image { dest_url, .. }) | Event::Start(Tag::Link { dest_url, .. }) => {
            let mut iri_ref = IriRefBuf::new(dest_url.clone().into_string())
                .map_err(|e| e.to_string())
                .whatever_context("invalid URL in image/link")?;

            if iri_ref.scheme().is_some() || iri_ref.authority().is_some() {
                // Is a protocol-relative or absolute URL.
                return Ok(e);
            }

            let iri_path: &str = iri_ref.path().as_ref();
            match resolve_asset_link_rewrite(root, source_md_path, only_plan, iri_path)? {
                Some(AssetLinkRewrite::Rewrite(url)) => {
                    *dest_url = CowStr::from(append_query_and_fragment(url, &iri_ref));
                    return Ok(e);
                }
                Some(AssetLinkRewrite::FallThrough) | None => {}
            }

            if !iri_ref.path().ends_with(".md") {
                // Only markdown files need the `@` syntax.
                return Ok(e);
            }

            let parent = source_md_path.parent().with_whatever_context(|| {
                format!(
                    "source markdown path `{}` has no parent",
                    source_md_path.to_string_lossy()
                )
            })?;
            let child = if iri_path.starts_with("/") {
                let mut path = Path::new(iri_path);
                path = path.strip_prefix("/").unwrap();
                root.join(path)
            } else {
                parent.join(Path::new(iri_path))
            };
            let normalized_root = normalize_path_lexically(root);
            let normalized_child = normalize_path_lexically(&child);
            if let Some(public_url) = only_plan.and_then(|plan| {
                normalized_child
                    .strip_prefix(&normalized_root)
                    .ok()
                    .and_then(|relative_path| plan.external_url_for_content_target(relative_path))
            }) {
                let mut external_url = public_url.to_owned();
                if let Some(query) = iri_ref.query() {
                    external_url.push('?');
                    external_url.push_str(query.as_str());
                }
                if let Some(fragment) = iri_ref.fragment() {
                    external_url.push('#');
                    external_url.push_str(fragment.as_str());
                }
                *dest_url = CowStr::from(external_url);
                return Ok(e);
            }
            let canonicalized = canonicalize_md(&child)?;
            if let Some(public_url) =
                only_plan.and_then(|plan| plan.external_url_for_canonical_target(&canonicalized))
            {
                *dest_url =
                    CowStr::from(append_query_and_fragment(public_url.to_owned(), &iri_ref));
                return Ok(e);
            }

            let canonicalized = path_to_at(root, parent, iri_ref.path())?;
            let path = iref::iri::Path::new(&canonicalized).expect("path is valid IRI");
            iri_ref.set_path(path);

            *dest_url = CowStr::from(iri_ref.into_string());
            Ok(e)
        }
        _ => Ok(e),
    }
}

struct RenderCsl {
    contents: Option<String>,
}

impl RenderCsl {
    fn render_csl<'a>(&mut self, event: Event<'a>) -> Result<Option<Event<'a>>, Whatever> {
        let text = match (&mut self.contents, event) {
            (contents @ None, Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref lang))))
                if lang.as_ref() == "csl-json" =>
            {
                *contents = Some(String::new());
                return Ok(None);
            }
            (Some(_), Event::End(TagEnd::CodeBlock)) => self.contents.take().unwrap(),
            (Some(contents), Event::Text(text)) => {
                contents.push_str(&text);
                return Ok(None);
            }
            (Some(_), event) => {
                panic!("unknown event inside csl-json block: {event:#?}");
            }
            (None, e) => return Ok(Some(e)),
        };

        let mut value: serde_json::Value =
            serde_json::from_str(&text).whatever_context("invalid JSON in citation")?;

        // TODO: Once typst/citationberg#17 is merged, we can remove this line.
        value
            .as_object_mut()
            .whatever_context("citation is not a JSON object")?
            .remove("custom");

        let item: citationberg::json::Item =
            serde_json::from_value(value).whatever_context("citation not valid")?;

        let locales = hayagriva::archive::locales();
        let style = match ArchivedStyle::AmericanPsychologicalAssociation.get() {
            Style::Independent(i) => i,
            _ => unreachable!(),
        };
        let mut driver = BibliographyDriver::new();

        let items = vec![CitationItem::with_entry(&item)];
        driver.citation(CitationRequest::from_items(items, &style, &locales));

        let result = driver.finish(BibliographyRequest {
            style: &style,
            locale: None,
            locale_files: &locales,
        });

        let bib = result.bibliography.unwrap();
        let mut text = String::new();
        for item in bib.items {
            item.content
                .write_buf(&mut text, hayagriva::BufWriteFormat::Html)
                .unwrap();
        }

        Ok(Some(Event::InlineHtml(text.into())))
    }
}

fn transform_markdown(
    root: &Path,
    path: &Path,
    body: &str,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<String, Whatever> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let mut csl = RenderCsl { contents: None };

    let events = Parser::new_ext(body, opts)
        .map(|e| fix_links(root, path, only_plan, e))
        .filter_map(|r| match r {
            Ok(e) => csl.render_csl(e).transpose(),
            err => Some(err),
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter();

    let mut output = String::with_capacity(body.len() + (body.len() / 100));

    cmark(events, &mut output).whatever_context("cannot write markdown")?;

    Ok(output)
}

fn process_assets(
    root: &Path,
    path: &Path,
    only_plan: Option<&OnlyRenderPlan>,
    missing_path_mode: MissingPathMode,
) -> Result<(), Whatever> {
    let canon_root = std::fs::canonicalize(root).whatever_context("could not canonicalize root")?;
    let assets_dir = path.join("assets");

    let mut entries = Vec::new();
    let mut ignored_missing_path = false;

    for entry in WalkDir::new(&assets_dir).follow_links(true).into_iter() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if missing_path_mode.should_ignore_walkdir_error(&error) => {
                ignored_missing_path = true;
                continue;
            }
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!("couldn't read entry in `{}`", assets_dir.to_string_lossy())
                });
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        if entry.path().extension().and_then(OsStr::to_str) != Some("md") {
            continue;
        }

        let candidate = match std::fs::canonicalize(entry.path()) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "unable to canonicalize `{}`: {e}",
                    entry.path().to_string_lossy()
                );
                continue;
            }
        };

        let in_root = candidate.starts_with(&canon_root);
        if !in_root {
            warn!(
                "asset `{}` not in root, skipping",
                entry.path().to_string_lossy()
            );
            continue;
        }

        entries.push(entry);
    }

    if entries.is_empty() && ignored_missing_path {
        return Ok(());
    }

    let number_txt = path
        .file_name()
        .with_whatever_context(|| format!("no file name for `{}`", path.to_string_lossy()))?
        .to_str()
        .with_whatever_context(|| format!("non-UTF-8 in `{}`", path.to_string_lossy()))?;

    let number: u32 = number_txt.parse().with_whatever_context(|_| {
        format!("can't parse number for `{}`", path.to_string_lossy())
    })?;

    for entry in entries.into_iter().progress_ext("Assets") {
        let path = entry.path();
        let contents = match read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if missing_path_mode.should_ignore_io_error(&error) => continue,
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!("could not read file `{}`", path.to_string_lossy())
                });
            }
        };
        if is_generated_zola_markdown(&contents) {
            continue;
        }

        let contents =
            transform_markdown(root, path, &contents, only_plan).with_whatever_context(|_| {
                format!(
                    "unable to transform markdown for `{}`",
                    path.to_string_lossy()
                )
            })?;

        let relative_path = path.strip_prefix(&assets_dir).unwrap();
        let relative_path = relative_path.with_file_name(relative_path.file_stem().unwrap());

        let alias_bases = [
            PathBuf::from(format!("/assets/eip-{number}/")),
            PathBuf::from(format!("/assets/erc-{number}/")),
        ];

        let mut aliases = Vec::with_capacity(alias_bases.len());

        for alias_base in &alias_bases {
            aliases.push(alias_base.join(&relative_path));
        }

        if relative_path.ends_with("README") || relative_path.ends_with("index") {
            let index_path = relative_path.parent().unwrap();
            for alias_base in &alias_bases {
                aliases.push(alias_base.join(index_path));
            }
        }

        let front_matter = FrontMatter {
            path: format!("{number}/assets/{}", relative_path.to_str().unwrap()),
            aliases,
            ..Default::default()
        };

        match write_file(path, front_matter, &contents) {
            Ok(()) => {}
            Err(error) if missing_path_mode.should_ignore_io_error(&error) => continue,
            Err(error) => return Err(error).whatever_context("couldn't write file"),
        }
    }

    Ok(())
}

fn process_eip(
    root: &Path,
    path: &Path,
    only_plan: Option<&OnlyRenderPlan>,
    missing_path_mode: MissingPathMode,
) -> Result<(), Whatever> {
    let path_lossy = path.to_string_lossy();
    let contents = match read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if missing_path_mode.should_ignore_io_error(&error) => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_whatever_context(|_| format!("could not read file `{}`", path_lossy));
        }
    };
    if is_generated_zola_markdown(&contents) {
        return Ok(());
    }

    let (preamble, body) = Preamble::split(&contents)
        .with_whatever_context(|_| format!("couldn't split preamble for `{}`", path_lossy))?;

    let body = transform_markdown(root, path, body, only_plan)
        .with_whatever_context(|_| format!("unable to transform markdown for `{path_lossy}`"))?;

    let preamble = Preamble::parse(Some(&path_lossy), preamble)
        .ok()
        .with_whatever_context(|| format!("couldn't parse preamble in `{}`", path_lossy))?;

    let updated = match path.file_name() {
        Some(x) if x == "_index.md" => None,
        _ => Some(last_modified(path)?),
    };

    let mut front_matter = FrontMatter {
        updated,
        ..Default::default()
    };

    for field in preamble.fields() {
        let value = field.value().trim();
        match field.name() {
            "title" => front_matter.title = value.to_owned(),
            "description" => front_matter.description = value.to_owned(),
            "created" => {
                let parsed = value.parse().with_whatever_context(|_| {
                    format!("couldn't parse created in `{}`", path_lossy)
                })?;
                front_matter.date = Some(parsed);
            }
            "status" => {
                if value != "Final" && value != "Living" {
                    front_matter.draft = true;
                }
                front_matter.extra.insert("status".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("status".into(), vec![value.into()]);
            }
            "type" => {
                front_matter.extra.insert("type".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("type".into(), vec![value.into()]);
            }
            "category" => {
                front_matter.extra.insert("category".into(), value.into());
                front_matter
                    .taxonomies
                    .insert("category".into(), vec![value.into()]);
            }
            "eip" | "number" => {
                let number = value
                    .parse::<u32>()
                    .whatever_context("couldn't parse eip/number")?;

                front_matter.template = Some("eip.html".into());
                front_matter.slug = number.to_string();
                front_matter.extra.insert("number".into(), number.into());

                let alias_path = PathBuf::from(&path);
                if let Some(file_stem) = alias_path.file_stem() {
                    let root = match file_stem.to_str() {
                        Some("index") => alias_path.parent().unwrap().file_name().unwrap(),
                        _ => file_stem,
                    };
                    front_matter.aliases.push(root.into());
                }

                front_matter
                    .aliases
                    .push(format!("ERCS/erc-{number}").into());
                front_matter
                    .aliases
                    .push(format!("EIPS/eip-{number}").into());
            }
            "author" => {
                let authors = extract_authors(value)?;
                front_matter.authors = authors.iter().map(|a| a.name.clone()).collect();
                front_matter
                    .extra
                    .insert("author_details".into(), Value::from(authors));
            }
            "requires" => {
                let items: Vec<String> = value
                    .split(',')
                    .map(str::trim)
                    .map(str::parse)
                    .collect::<Result<Vec<u32>, _>>()
                    .whatever_context("could not parse requires")?
                    .into_iter()
                    .map(|eip| {
                        let proposal_number = match ProposalNumber::from_u32(eip) {
                            Ok(proposal_number) => proposal_number,
                            Err(()) => snafu::whatever!("could not parse requires"),
                        };
                        match only_plan {
                            Some(plan) => {
                                match plan.reference_for_required_number(proposal_number)? {
                                    ProposalReference::Internal(path) => Ok(path),
                                    ProposalReference::External(public_url) => {
                                        Ok(public_url.to_owned())
                                    }
                                }
                            }
                            None => {
                                let path = format!("/{eip:0>5}.md");
                                path_to_at(root, root, &path)
                            }
                        }
                    })
                    .collect::<Result<_, _>>()?;
                front_matter
                    .extra
                    .insert("requires".into(), Value::from(items));
            }
            other => {
                let name = other.replace('-', "_");
                front_matter.extra.insert(name, value.into());
            }
        }
    }

    match write_file(path, front_matter, &body) {
        Ok(()) => {}
        Err(error) if missing_path_mode.should_ignore_io_error(&error) => return Ok(()),
        Err(error) => return Err(error).whatever_context("couldn't write file"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use git2::{IndexAddOption, Repository, Signature};
    use snafu::Report;
    use tempfile::TempDir;
    use toml::Value as TomlValue;

    use super::{
        absolute_rendered_path_for_content_path, preprocess, preprocess_paths,
        proposal_asset_exists_in_content_tree, relative_url_from_rendered_paths,
        resolve_proposal_asset_path, ProposalAssetPath, ProposalAssetPathResolution,
    };
    use crate::proposal::ProposalAssetKind;
    use crate::proposal::{OnlyRenderPlan, ProposalNumber};

    fn number(value: u32) -> ProposalNumber {
        ProposalNumber::from_u32(value).unwrap()
    }

    fn write_file(root: &Path, relative: &str, contents: impl AsRef<str>) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents.as_ref()).unwrap();
    }

    fn commit_all(repo: &Repository) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["content"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let signature = Signature::now("build-eips test", "build-eips@example.test").unwrap();

        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
    }

    fn content_repo(files: &[(&str, String)]) -> (TempDir, PathBuf) {
        let temp = TempDir::new().unwrap();
        let repo_root = temp.path().join("repo");
        let content_root = repo_root.join("content");
        std::fs::create_dir_all(&content_root).unwrap();
        let repo = Repository::init(&repo_root).unwrap();
        repo.set_head("refs/heads/master").unwrap();

        for (relative, contents) in files {
            write_file(&content_root, relative, contents);
        }

        commit_all(&repo);
        (temp, content_root)
    }

    fn proposal_markdown(
        proposal_number: u32,
        category: Option<&str>,
        extra_preamble: &str,
        body: &str,
    ) -> String {
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {proposal_number}\ntitle: Proposal {proposal_number}\n{category}{extra_preamble}---\n{body}\n"
        )
    }

    fn only_plan(content_root: &Path, selected: &[u32]) -> OnlyRenderPlan {
        let selected = selected
            .iter()
            .copied()
            .map(number)
            .collect::<BTreeSet<_>>();
        OnlyRenderPlan::build(content_root, selected).unwrap()
    }

    fn repo_paths(paths: &[&str]) -> BTreeSet<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn rendered_body(path: &Path) -> String {
        let contents = std::fs::read_to_string(path).unwrap();
        contents.split_once("\n+++\n").unwrap().1.to_owned()
    }

    fn rendered_front_matter(path: &Path) -> TomlValue {
        let contents = std::fs::read_to_string(path).unwrap();
        let front_matter = contents
            .strip_prefix("+++\n")
            .unwrap()
            .split_once("\n+++\n")
            .unwrap()
            .0;
        toml::from_str(front_matter).unwrap()
    }

    fn resolved_asset(
        content_root: &Path,
        source_md_path: &Path,
        iri_path: &str,
    ) -> ProposalAssetPath {
        match resolve_proposal_asset_path(content_root, source_md_path, iri_path).unwrap() {
            ProposalAssetPathResolution::ProposalAsset(asset_path) => asset_path,
            ProposalAssetPathResolution::NotAProposalAsset => {
                panic!("expected `{iri_path}` to resolve as proposal asset")
            }
        }
    }

    #[test]
    fn resolver_detects_flat_source_cross_proposal_static_asset() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let asset_path = resolved_asset(&content, &source, "./00678/assets/foo.pdf");

        assert_eq!(asset_path.target_proposal_number, number(678));
        assert_eq!(
            asset_path.content_relative_asset_path,
            Path::new("00678/assets/foo.pdf")
        );
        assert_eq!(asset_path.asset_relative_path, Path::new("foo.pdf"));
        assert_eq!(asset_path.kind, ProposalAssetKind::Static);
        assert_eq!(asset_path.rendered_target_path, "/678/assets/foo.pdf");
    }

    #[test]
    fn resolver_detects_directory_source_cross_proposal_static_asset() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555/index.md");

        let asset_path = resolved_asset(&content, &source, "../00678/assets/foo.pdf");

        assert_eq!(
            asset_path.content_relative_asset_path,
            Path::new("00678/assets/foo.pdf")
        );
        assert_eq!(asset_path.rendered_target_path, "/678/assets/foo.pdf");
    }

    #[test]
    fn resolver_detects_asset_markdown_lexically_without_filesystem() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555/assets/guide.md");

        let asset_path = resolved_asset(&content, &source, "../../00678/assets/guide.md");

        assert_eq!(asset_path.kind, ProposalAssetKind::Markdown);
        assert_eq!(
            asset_path.content_relative_asset_path,
            Path::new("00678/assets/guide.md")
        );
        assert_eq!(asset_path.rendered_target_path, "/678/assets/guide/");
    }

    #[test]
    fn resolver_decodes_safe_percent_paths_and_renders_encoded_urls() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let asset_path = resolved_asset(
            &content,
            &source,
            "./00678/assets/Contract%20Interactions%20diagram.svg",
        );

        assert_eq!(
            asset_path.content_relative_asset_path,
            Path::new("00678/assets/Contract Interactions diagram.svg")
        );
        assert_eq!(
            asset_path.rendered_target_path,
            "/678/assets/Contract%20Interactions%20diagram.svg"
        );
    }

    #[test]
    fn resolver_rejects_unsafe_percent_and_asset_segments() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        for iri_path in [
            "./00678/assets/foo%2Fbar.pdf",
            "./00678/assets/foo%5Cbar.pdf",
            "./00678/assets/foo%00bar.pdf",
            "./00678/assets/.",
            "./00678/assets/..",
            "./00678/assets/%2E",
            "./00678/assets/%2E%2E",
        ] {
            let error = resolve_proposal_asset_path(&content, &source, iri_path)
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("unsafe"),
                "expected unsafe path error for `{iri_path}`, got `{error}`"
            );
        }
    }

    #[test]
    fn resolver_allows_unsafe_percent_encodings_for_non_proposal_paths() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let resolution =
            resolve_proposal_asset_path(&content, &source, "./images/foo%2Fbar.pdf").unwrap();

        assert_eq!(resolution, ProposalAssetPathResolution::NotAProposalAsset);
    }

    #[test]
    fn resolver_still_rejects_unsafe_percent_encodings_for_proposal_assets() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let error = resolve_proposal_asset_path(&content, &source, "./00678/assets/foo%2Fbar.pdf")
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsafe"));
    }

    #[test]
    fn resolver_returns_passthrough_for_outside_root_without_error() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let resolution =
            resolve_proposal_asset_path(&content, &source, "../elsewhere/foo.pdf").unwrap();

        assert_eq!(resolution, ProposalAssetPathResolution::NotAProposalAsset);
    }

    #[test]
    fn resolver_returns_passthrough_for_non_proposal_asset_paths() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        let source = content.join("00555.md");

        let resolution =
            resolve_proposal_asset_path(&content, &source, "./images/foo.pdf").unwrap();

        assert_eq!(resolution, ProposalAssetPathResolution::NotAProposalAsset);
    }

    #[test]
    fn rendered_path_helper_maps_proposal_and_asset_content_paths() {
        for (content_relative_path, expected_rendered_path) in [
            ("_index.md", "/"),
            ("00555.md", "/555/"),
            ("00555/index.md", "/555/"),
            ("00555/assets/guide.md", "/555/assets/guide/"),
            ("00555/assets/README.md", "/555/assets/README/"),
            ("00555/assets/index.md", "/555/assets/index/"),
            ("00678/assets/foo.pdf", "/678/assets/foo.pdf"),
            (
                "00678/assets/Contract Interactions diagram.svg",
                "/678/assets/Contract%20Interactions%20diagram.svg",
            ),
        ] {
            assert_eq!(
                absolute_rendered_path_for_content_path(Path::new(content_relative_path)).unwrap(),
                expected_rendered_path
            );
        }
    }

    #[test]
    fn relative_url_helper_uses_rendered_paths() {
        assert_eq!(
            relative_url_from_rendered_paths("/555/", "/678/assets/foo.pdf").unwrap(),
            "../678/assets/foo.pdf"
        );
        assert_eq!(
            relative_url_from_rendered_paths("/555/assets/guide/", "/678/assets/foo.pdf").unwrap(),
            "../../../678/assets/foo.pdf"
        );
        assert_eq!(
            relative_url_from_rendered_paths("/555/", "/678/assets/guide/").unwrap(),
            "../678/assets/guide/"
        );
    }

    #[test]
    fn filesystem_validator_checks_content_relative_assets() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        write_file(&content, "00678/assets/foo.pdf", "");

        assert!(proposal_asset_exists_in_content_tree(
            &content,
            Path::new("00678/assets/foo.pdf")
        ));
        assert!(!proposal_asset_exists_in_content_tree(
            &content,
            Path::new("00678/assets/missing.pdf")
        ));
        assert!(!proposal_asset_exists_in_content_tree(
            &content,
            Path::new("../00678/assets/foo.pdf")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_validator_rejects_symlink_targets_outside_content_root() {
        let temp = TempDir::new().unwrap();
        let content = temp.path().join("content");
        write_file(&content, "00678/assets/placeholder", "");
        let outside = temp.path().join("outside.pdf");
        std::fs::write(&outside, "").unwrap();
        std::os::unix::fs::symlink(&outside, content.join("00678/assets/outside.pdf")).unwrap();

        assert!(!proposal_asset_exists_in_content_tree(
            &content,
            Path::new("00678/assets/outside.pdf")
        ));
    }

    #[test]
    fn preprocess_rewrites_flat_source_cross_proposal_static_asset() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](./00678/assets/foo.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(../678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_rewrites_directory_source_cross_proposal_static_asset() {
        let (_temp, content) = content_repo(&[
            (
                "00555/index.md",
                proposal_markdown(555, None, "", "See [asset](../00678/assets/foo.pdf)."),
            ),
            ("00555/assets/.keep", "".to_owned()),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555/index.md"));
        assert!(body.contains("(../678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_rewrites_root_index_source_cross_proposal_static_asset() {
        let (_temp, content) = content_repo(&[
            (
                "_index.md",
                "---\ntitle: Home\n---\nSee [asset](/00678/assets/foo.pdf).\n".to_owned(),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("_index.md"));
        assert!(body.contains("(678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_rewrites_asset_markdown_source_using_rendered_source_path() {
        let (_temp, content) = content_repo(&[
            (
                "00555/index.md",
                proposal_markdown(555, None, "", "Source."),
            ),
            (
                "00555/assets/guide.md",
                "See [asset](../../00678/assets/foo.pdf).".to_owned(),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555/assets/guide.md"));
        assert!(body.contains("(../../../678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_rewrites_source_root_absolute_asset_path() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](/00678/assets/foo.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(../678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_rewrites_cross_proposal_image_links() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "![diagram](./00678/assets/diagram.png)"),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/diagram.png", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("![diagram](../678/assets/diagram.png)"));
    }

    #[test]
    fn preprocess_preserves_query_and_fragment_on_asset_links() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "",
                    "See [asset](./00678/assets/foo.pdf?download=1#page=2).",
                ),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(../678/assets/foo.pdf?download=1#page=2)"));
    }

    #[test]
    fn preprocess_decodes_asset_paths_and_keeps_generated_urls_encoded() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "",
                    "See [asset](./00678/assets/Contract%20Interactions%20diagram.svg).",
                ),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            (
                "00678/assets/Contract Interactions diagram.svg",
                "".to_owned(),
            ),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("../678/assets/Contract%20Interactions%20diagram.svg"));
    }

    #[test]
    fn preprocess_keeps_selected_or_full_asset_markdown_links_on_existing_md_path() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [guide](./00678/assets/guide.md)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/guide.md", "Guide.".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(@/00678/assets/guide.md)"));
    }

    #[test]
    fn preprocess_keeps_asset_markdown_fragment_links_unchanged() {
        let (_temp, content) = content_repo(&[
            (
                "00555/index.md",
                proposal_markdown(555, None, "", "Source."),
            ),
            (
                "00555/assets/guide.md",
                "See [heading](#heading).\n\n## Heading\n".to_owned(),
            ),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555/assets/guide.md"));
        assert!(body.contains("[heading](#heading)"));
    }

    #[test]
    fn preprocess_keeps_ordinary_proposal_markdown_links_on_existing_path() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [proposal](./00678.md)."),
            ),
            ("00678.md", proposal_markdown(678, None, "", "Target.")),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(@/00678.md)"));
    }

    #[test]
    fn targeted_preprocess_rewrites_omitted_static_asset_to_public_url() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](./00678/assets/foo.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(https://eips.ethereum.org/678/assets/foo.pdf)"));
    }

    #[test]
    fn targeted_preprocess_rewrites_omitted_asset_markdown_to_public_url() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [guide](./00678/assets/guide.md)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/guide.md", "Guide.".to_owned()),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(https://eips.ethereum.org/678/assets/guide/)"));
    }

    #[test]
    fn targeted_preprocess_rewrites_omitted_readme_and_index_asset_markdown_public_urls() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "",
                    "See [readme](./00678/assets/README.md) and [index](./00678/assets/index.md).",
                ),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/README.md", "Readme.".to_owned()),
            ("00678/assets/index.md", "Index.".to_owned()),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(https://eips.ethereum.org/678/assets/README/)"));
        assert!(body.contains("(https://eips.ethereum.org/678/assets/index/)"));
    }

    #[test]
    fn targeted_preprocess_keeps_selected_static_asset_local() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](./00678/assets/foo.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);
        let plan = only_plan(&content, &[555, 678]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(../678/assets/foo.pdf)"));
        assert!(!body.contains("https://eips.ethereum.org/678/assets/foo.pdf"));
    }

    #[test]
    fn targeted_dirty_preprocess_uses_inventory_after_omitted_target_is_pruned() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](./00678/assets/foo.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);
        let plan = only_plan(&content, &[555]);
        plan.prune_content(&content).unwrap();

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(https://eips.ethereum.org/678/assets/foo.pdf)"));
    }

    #[test]
    fn preprocess_errors_clearly_for_missing_selected_or_full_asset_target() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [asset](./00678/assets/missing.pdf)."),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/.keep", "".to_owned()),
        ]);

        let error = Report::from_error(preprocess(&content, None).unwrap_err()).to_string();

        assert!(error.contains("proposal asset link"));
        assert!(error.contains("00555.md"));
        assert!(error.contains("./00678/assets/missing.pdf"));
        assert!(error.contains("00678/assets/missing.pdf"));
    }

    #[test]
    fn preprocess_skips_generated_zola_markdown_files() {
        let original =
            "+++\ntitle = \"Generated\"\n+++\nSee [asset](./00678/assets/missing.pdf).\n";
        let (_temp, content) = content_repo(&[("00555.md", original.to_owned())]);

        preprocess(&content, None).unwrap();

        assert_eq!(
            std::fs::read_to_string(content.join("00555.md")).unwrap(),
            original
        );
    }

    #[test]
    fn process_assets_skips_only_generated_asset_markdown_file() {
        let generated =
            "+++\ntitle = \"Generated\"\n+++\nSee [missing](../../00678/assets/missing.pdf).\n";
        let (_temp, content) = content_repo(&[
            (
                "00555/index.md",
                proposal_markdown(555, None, "", "Source."),
            ),
            ("00555/assets/generated.md", generated.to_owned()),
            ("00555/assets/fresh.md", "Fresh asset markdown.".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        assert_eq!(
            std::fs::read_to_string(content.join("00555/assets/generated.md")).unwrap(),
            generated
        );
        assert!(
            std::fs::read_to_string(content.join("00555/assets/fresh.md"))
                .unwrap()
                .starts_with("+++\n")
        );
    }

    #[test]
    fn preprocess_leaves_non_proposal_relative_asset_links_unchanged() {
        let (_temp, content) = content_repo(&[(
            "00555.md",
            proposal_markdown(555, None, "", "See [local](./images/foo.pdf)."),
        )]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("(./images/foo.pdf)"));
    }

    #[test]
    fn preprocess_leaves_raw_html_asset_references_unchanged() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "",
                    r#"<img src="./00678/assets/foo.pdf" alt="asset">"#,
                ),
            ),
            (
                "00678/index.md",
                proposal_markdown(678, None, "", "Target."),
            ),
            ("00678/assets/foo.pdf", "".to_owned()),
        ]);

        preprocess(&content, None).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains(r#"<img src="./00678/assets/foo.pdf" alt="asset">"#));
    }

    #[test]
    fn targeted_preprocess_rewrites_selected_body_links_to_unselected_public_urls() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "", "See [ERC-678](/00678.md)."),
            ),
            (
                "00678.md",
                proposal_markdown(678, Some("ERC"), "", "Target."),
            ),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("https://ercs.ethereum.org/ERCS/erc-678"));
        assert!(!body.contains("@/00678.md"));
    }

    #[test]
    fn targeted_preprocess_paths_rewrites_selected_dirty_markdown_with_plan() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "requires: 155\n",
                    "See [EIP-155](./00155.md#list-of-chain-id-s).",
                ),
            ),
            ("00155.md", proposal_markdown(155, None, "", "Unselected.")),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess_paths(&content, &repo_paths(&["content/00555.md"]), Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        let front_matter = rendered_front_matter(&content.join("00555.md"));
        let requires = front_matter["extra"]["requires"].as_array().unwrap();
        assert!(body.contains("https://eips.ethereum.org/EIPS/eip-155#list-of-chain-id-s"));
        assert_eq!(
            requires[0].as_str().unwrap(),
            "https://eips.ethereum.org/EIPS/eip-155"
        );
    }

    #[test]
    fn targeted_preprocess_paths_ignores_deleted_dirty_markdown() {
        let (_temp, content) =
            content_repo(&[("00555.md", proposal_markdown(555, None, "", "Selected."))]);
        let plan = only_plan(&content, &[555]);
        std::fs::remove_file(content.join("00555.md")).unwrap();

        preprocess_paths(&content, &repo_paths(&["content/00555.md"]), Some(&plan)).unwrap();

        assert!(!content.join("00555.md").exists());
    }

    #[test]
    fn targeted_preprocess_rewrites_retained_non_proposal_links_to_public_urls() {
        let (_temp, content) = content_repo(&[
            (
                "_index.md",
                "---\ntitle: Home\n---\nSee [EIP-678](/00678.md).\n".to_owned(),
            ),
            ("00555.md", proposal_markdown(555, None, "", "Selected.")),
            ("00678.md", proposal_markdown(678, None, "", "Unselected.")),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("_index.md"));
        assert!(body.contains("https://eips.ethereum.org/EIPS/eip-678"));
        assert!(!body.contains("@/00678.md"));
    }

    #[test]
    fn targeted_preprocess_paths_rewrites_retained_non_proposal_markdown_with_plan() {
        let (_temp, content) = content_repo(&[
            (
                "_index.md",
                "---\ntitle: Home\n---\nSee [EIP-678](/00678.md).\n".to_owned(),
            ),
            ("00555.md", proposal_markdown(555, None, "", "Selected.")),
            ("00678.md", proposal_markdown(678, None, "", "Unselected.")),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess_paths(&content, &repo_paths(&["content/_index.md"]), Some(&plan)).unwrap();

        let body = rendered_body(&content.join("_index.md"));
        assert!(body.contains("https://eips.ethereum.org/EIPS/eip-678"));
        assert!(!body.contains("@/00678.md"));
    }

    #[test]
    fn targeted_preprocess_paths_rewrites_selected_asset_markdown_with_plan() {
        let (_temp, content) = content_repo(&[
            ("00555.md", proposal_markdown(555, None, "", "Selected.")),
            (
                "00555/assets/guide.md",
                "See [EIP-678](/00678.md).\n".to_owned(),
            ),
            ("00555/assets/diagram.png", "image\n".to_owned()),
            (
                "00678.md",
                proposal_markdown(678, Some("ERC"), "", "Unselected."),
            ),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess_paths(
            &content,
            &repo_paths(&["content/00555/assets/guide.md"]),
            Some(&plan),
        )
        .unwrap();

        let body = rendered_body(&content.join("00555/assets/guide.md"));
        assert!(body.contains("https://ercs.ethereum.org/ERCS/erc-678"));
        assert_eq!(
            std::fs::read_to_string(content.join("00555/assets/diagram.png")).unwrap(),
            "image\n"
        );
    }

    #[test]
    fn targeted_preprocess_paths_ignores_deleted_dirty_asset_dir() {
        let (_temp, content) = content_repo(&[
            ("00555.md", proposal_markdown(555, None, "", "Selected.")),
            (
                "00555/assets/guide.md",
                "See [EIP-678](/00678.md).\n".to_owned(),
            ),
            ("00678.md", proposal_markdown(678, None, "", "Unselected.")),
        ]);
        let plan = only_plan(&content, &[555]);
        std::fs::remove_dir_all(content.join("00555/assets")).unwrap();

        preprocess_paths(
            &content,
            &repo_paths(&["content/00555/assets/guide.md"]),
            Some(&plan),
        )
        .unwrap();

        assert!(!content.join("00555/assets").exists());
    }

    #[test]
    fn targeted_preprocess_preserves_query_and_fragment_on_external_links() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(
                    555,
                    None,
                    "",
                    "See [Fragment](./00155.md#list-of-chain-id-s).\nSee [Query](./00155.md?foo=bar#list-of-chain-id-s).",
                ),
            ),
            (
                "00155.md",
                proposal_markdown(155, None, "", "Unselected."),
            ),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        assert!(body.contains("https://eips.ethereum.org/EIPS/eip-155#list-of-chain-id-s"));
        assert!(body.contains("https://eips.ethereum.org/EIPS/eip-155?foo=bar#list-of-chain-id-s"));
    }

    #[test]
    fn targeted_preprocess_rewrites_requires_to_unselected_public_urls() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "requires: 678\n", "Selected."),
            ),
            (
                "00678.md",
                proposal_markdown(678, Some("ERC"), "", "Target."),
            ),
        ]);
        let plan = only_plan(&content, &[555]);

        preprocess(&content, Some(&plan)).unwrap();

        let front_matter = rendered_front_matter(&content.join("00555.md"));
        let requires = front_matter["extra"]["requires"].as_array().unwrap();
        assert_eq!(
            requires[0].as_str().unwrap(),
            "https://ercs.ethereum.org/ERCS/erc-678"
        );
    }

    #[test]
    fn targeted_preprocess_keeps_internal_references_between_selected_proposals() {
        let (_temp, content) = content_repo(&[
            (
                "00555.md",
                proposal_markdown(555, None, "requires: 678\n", "See [EIP-678](/00678.md)."),
            ),
            (
                "00678.md",
                proposal_markdown(678, Some("ERC"), "", "Target."),
            ),
        ]);
        let plan = only_plan(&content, &[555, 678]);

        preprocess(&content, Some(&plan)).unwrap();

        let body = rendered_body(&content.join("00555.md"));
        let front_matter = rendered_front_matter(&content.join("00555.md"));
        let requires = front_matter["extra"]["requires"].as_array().unwrap();
        assert!(body.contains("@/00678.md"));
        assert_eq!(requires[0].as_str().unwrap(), "@/00678.md");
    }

    #[test]
    fn targeted_preprocess_does_not_mask_missing_body_link_targets() {
        let (_temp, content) = content_repo(&[(
            "00555.md",
            proposal_markdown(555, None, "", "See [Missing](/00678.md)."),
        )]);
        let plan = only_plan(&content, &[555]);

        let error = Report::from_error(preprocess(&content, Some(&plan)).unwrap_err()).to_string();

        assert!(error.contains("could not canonicalize"));
        assert!(error.contains("00678.md"));
    }
}
