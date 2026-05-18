/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Proposal path classification and targeted render policy.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fmt,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use eipw_preamble::Preamble;
use log::warn;
use serde::{
    de::{self, Unexpected, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};
use snafu::{OptionExt, ResultExt, Whatever};
use walkdir::WalkDir;

use crate::layout::CONTENT_DIR;

/// Positive proposal number used by CLI selectors and `[render].only` config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProposalNumber(NonZeroU32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProposalNumberParseFailure {
    Empty,
    NonDigit,
    Zero,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditorialNumberSelector {
    Number(ProposalNumber),
    InvalidNumberLike(ProposalNumberParseFailure),
    PathLike,
}

impl ProposalNumber {
    fn parse_selector(selector: &str) -> Result<Self, ProposalNumberParseFailure> {
        if selector.is_empty() {
            return Err(ProposalNumberParseFailure::Empty);
        }

        if !selector.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ProposalNumberParseFailure::NonDigit);
        }

        let number = selector
            .parse::<u32>()
            .map_err(|_| ProposalNumberParseFailure::Overflow)?;

        Self::from_u32(number).map_err(|_| ProposalNumberParseFailure::Zero)
    }

    pub(crate) fn parse_cli_selector(selector: &str) -> Result<Self, String> {
        Self::parse_selector(selector).map_err(|_| {
            format!(
                "`{selector}` is not a valid --only selector; expected a positive proposal number"
            )
        })
    }

    pub(crate) fn from_u32(number: u32) -> Result<Self, ()> {
        NonZeroU32::new(number).map(Self).ok_or(())
    }

    pub(crate) fn get(self) -> u32 {
        self.0.get()
    }
}

pub(crate) fn classify_editorial_number_selector(selector: &str) -> EditorialNumberSelector {
    match ProposalNumber::parse_selector(selector) {
        Ok(number) => EditorialNumberSelector::Number(number),
        Err(failure) if is_number_like_selector(selector) => {
            EditorialNumberSelector::InvalidNumberLike(failure)
        }
        Err(_) => EditorialNumberSelector::PathLike,
    }
}

fn is_number_like_selector(selector: &str) -> bool {
    !selector.is_empty()
        && selector
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'+' | b'-' | b','))
}

impl fmt::Display for ProposalNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.get())
    }
}

impl Serialize for ProposalNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.get())
    }
}

impl<'de> Deserialize<'de> for ProposalNumber {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProposalNumberVisitor;

        impl Visitor<'_> for ProposalNumberVisitor {
            type Value = ProposalNumber;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a positive proposal number")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u32::try_from(value).map_err(|_| {
                    E::invalid_value(Unexpected::Signed(value), &"a positive u32 proposal number")
                })?;
                ProposalNumber::from_u32(value).map_err(|_| {
                    E::invalid_value(
                        Unexpected::Unsigned(u64::from(value)),
                        &"a non-zero proposal number",
                    )
                })
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let value = u32::try_from(value).map_err(|_| {
                    E::invalid_value(
                        Unexpected::Unsigned(value),
                        &"a positive u32 proposal number",
                    )
                })?;
                ProposalNumber::from_u32(value).map_err(|_| {
                    E::invalid_value(
                        Unexpected::Unsigned(u64::from(value)),
                        &"a non-zero proposal number",
                    )
                })
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Err(E::invalid_type(Unexpected::Str(value), &self))
            }
        }

        deserializer.deserialize_any(ProposalNumberVisitor)
    }
}

#[derive(Debug)]
pub(crate) enum ProposalReference<'a> {
    Internal(String),
    External(&'a str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProposalAssetKind {
    Static,
    Markdown,
}

impl ProposalAssetKind {
    pub(crate) fn from_path(path: &Path) -> Self {
        if path.extension().and_then(OsStr::to_str) == Some("md") {
            Self::Markdown
        } else {
            Self::Static
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct ProposalAssetInventoryEntry {
    proposal_number: ProposalNumber,
    site: ProposalPublicSite,
    asset_relative_path: PathBuf,
    kind: ProposalAssetKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProposalPublicSite {
    Eips,
    Ercs,
}

impl ProposalPublicSite {
    pub(crate) fn proposal_prefix(self) -> &'static str {
        match self {
            Self::Eips => "EIP",
            Self::Ercs => "ERC",
        }
    }

    pub(crate) fn proposal_url(self, proposal_number: ProposalNumber) -> String {
        match self {
            Self::Eips => format!(
                "https://eips.ethereum.org/EIPS/eip-{}",
                proposal_number.get()
            ),
            Self::Ercs => format!(
                "https://ercs.ethereum.org/ERCS/erc-{}",
                proposal_number.get()
            ),
        }
    }

    fn asset_base_url(self) -> &'static str {
        match self {
            Self::Eips => "https://eips.ethereum.org",
            Self::Ercs => "https://ercs.ethereum.org",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OnlyRenderPlan {
    selected_numbers: BTreeSet<ProposalNumber>,
    asset_inventory: BTreeMap<PathBuf, ProposalAssetInventoryEntry>,
    canonical_proposal_numbers: BTreeMap<PathBuf, ProposalNumber>,
    markdown_paths_by_number: BTreeMap<ProposalNumber, BTreeSet<PathBuf>>,
    public_sites_by_number: BTreeMap<ProposalNumber, ProposalPublicSite>,
    public_urls_by_number: BTreeMap<ProposalNumber, String>,
}

impl OnlyRenderPlan {
    pub(crate) fn build(
        content_root: &Path,
        selected_numbers: BTreeSet<ProposalNumber>,
    ) -> Result<Self, Whatever> {
        let mut plan = Self {
            selected_numbers,
            asset_inventory: BTreeMap::new(),
            canonical_proposal_numbers: BTreeMap::new(),
            markdown_paths_by_number: BTreeMap::new(),
            public_sites_by_number: BTreeMap::new(),
            public_urls_by_number: BTreeMap::new(),
        };

        let entries = std::fs::read_dir(content_root).with_whatever_context(|_| {
            format!(
                "unable to read materialized content directory `{}`",
                content_root.to_string_lossy()
            )
        })?;

        for entry in entries {
            let entry = entry.with_whatever_context(|_| {
                format!(
                    "unable to read materialized content directory entry in `{}`",
                    content_root.to_string_lossy()
                )
            })?;
            let entry_path = entry.path();
            let file_type = entry.file_type().with_whatever_context(|_| {
                format!(
                    "unable to inspect materialized content path `{}`",
                    entry_path.to_string_lossy()
                )
            })?;

            if file_type.is_file() {
                let Some(number) = flat_proposal_number(&entry_path) else {
                    continue;
                };
                plan.record_markdown_path(content_root, number, &entry_path)?;
            } else if file_type.is_dir() {
                let Some(number) = path_component_proposal_number(entry_path.file_name()) else {
                    continue;
                };
                let index_path = entry_path.join("index.md");
                match std::fs::read_to_string(&index_path) {
                    Ok(contents) => {
                        plan.record_markdown_contents(content_root, number, &index_path, &contents)?
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                        ) => {}
                    Err(error) => {
                        snafu::whatever!(
                            "unable to read proposal markdown `{}`: {error}",
                            index_path.to_string_lossy()
                        );
                    }
                }
            }
        }

        plan.inventory_assets(content_root)?;

        for selected_number in &plan.selected_numbers {
            if !plan.markdown_paths_by_number.contains_key(selected_number) {
                snafu::whatever!("selected proposal `{selected_number}` was not found");
            }
        }

        Ok(plan)
    }

    fn record_markdown_path(
        &mut self,
        content_root: &Path,
        proposal_number: ProposalNumber,
        markdown_path: &Path,
    ) -> Result<(), Whatever> {
        let contents = std::fs::read_to_string(markdown_path).with_whatever_context(|_| {
            format!(
                "unable to read proposal markdown `{}`",
                markdown_path.to_string_lossy()
            )
        })?;
        self.record_markdown_contents(content_root, proposal_number, markdown_path, &contents)
    }

    fn record_markdown_contents(
        &mut self,
        content_root: &Path,
        proposal_number: ProposalNumber,
        markdown_path: &Path,
        contents: &str,
    ) -> Result<(), Whatever> {
        let relative_path = markdown_path
            .strip_prefix(content_root)
            .with_whatever_context(|_| {
                format!(
                    "proposal markdown `{}` is outside content root `{}`",
                    markdown_path.to_string_lossy(),
                    content_root.to_string_lossy()
                )
            })?
            .to_path_buf();
        let canonical_path = std::fs::canonicalize(markdown_path).with_whatever_context(|_| {
            format!(
                "unable to canonicalize proposal markdown `{}`",
                markdown_path.to_string_lossy()
            )
        })?;
        let site = public_site_for_markdown(markdown_path, contents)?;
        let public_url = site.proposal_url(proposal_number);

        match self.public_urls_by_number.get(&proposal_number) {
            Some(existing_url) if existing_url != &public_url => {
                snafu::whatever!(
                    "proposal `{proposal_number}` has conflicting public URLs `{existing_url}` and `{public_url}`"
                );
            }
            Some(_) => {}
            None => {
                self.public_sites_by_number.insert(proposal_number, site);
                self.public_urls_by_number
                    .insert(proposal_number, public_url);
            }
        }

        self.canonical_proposal_numbers
            .insert(canonical_path, proposal_number);
        self.markdown_paths_by_number
            .entry(proposal_number)
            .or_default()
            .insert(relative_path);

        Ok(())
    }

    fn inventory_assets(&mut self, content_root: &Path) -> Result<(), Whatever> {
        let canon_root = std::fs::canonicalize(content_root).with_whatever_context(|_| {
            format!(
                "unable to canonicalize content root `{}` for proposal asset inventory",
                content_root.to_string_lossy()
            )
        })?;

        let proposal_assets = self
            .markdown_paths_by_number
            .iter()
            .flat_map(|(proposal_number, markdown_paths)| {
                markdown_paths.iter().map(move |markdown_path| {
                    (
                        *proposal_number,
                        markdown_path.clone(),
                        asset_dir_for_markdown_path(markdown_path),
                    )
                })
            })
            .collect::<Vec<_>>();

        for (proposal_number, markdown_path, asset_dir) in proposal_assets {
            let Some(site) = self.public_sites_by_number.get(&proposal_number).copied() else {
                continue;
            };

            let absolute_asset_dir = content_root.join(&asset_dir);
            for entry in WalkDir::new(&absolute_asset_dir)
                .follow_links(true)
                .into_iter()
            {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) if missing_asset_dir(&error) => continue,
                    Err(error) => {
                        return Err(error).with_whatever_context(|_| {
                            format!(
                                "couldn't read proposal asset inventory entry in `{}`",
                                absolute_asset_dir.to_string_lossy()
                            )
                        });
                    }
                };

                if !entry.file_type().is_file() {
                    continue;
                }

                let candidate = match std::fs::canonicalize(entry.path()) {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        warn!(
                            "unable to canonicalize `{}`: {error}",
                            entry.path().to_string_lossy()
                        );
                        continue;
                    }
                };

                if !candidate.starts_with(&canon_root) {
                    warn!(
                        "asset `{}` not in root, skipping",
                        entry.path().to_string_lossy()
                    );
                    continue;
                }

                let content_relative_path = entry
                    .path()
                    .strip_prefix(content_root)
                    .with_whatever_context(|_| {
                        format!(
                            "proposal asset `{}` for `{}` is outside content root `{}`",
                            entry.path().to_string_lossy(),
                            markdown_path.to_string_lossy(),
                            content_root.to_string_lossy()
                        )
                    })?;
                let asset_relative_path = entry
                    .path()
                    .strip_prefix(&absolute_asset_dir)
                    .with_whatever_context(|_| {
                        format!(
                            "proposal asset `{}` is outside asset directory `{}`",
                            entry.path().to_string_lossy(),
                            absolute_asset_dir.to_string_lossy()
                        )
                    })?;
                let entry = ProposalAssetInventoryEntry {
                    proposal_number,
                    site,
                    asset_relative_path: asset_relative_path.to_path_buf(),
                    kind: ProposalAssetKind::from_path(asset_relative_path),
                };

                self.asset_inventory
                    .insert(content_relative_path.to_path_buf(), entry);
            }
        }

        Ok(())
    }

    pub(crate) fn has_proposal_asset(&self, content_relative_asset_path: &Path) -> bool {
        self.asset_inventory
            .contains_key(content_relative_asset_path)
    }

    pub(crate) fn public_url_for_omitted_proposal_asset(
        &self,
        content_relative_asset_path: &Path,
    ) -> Option<String> {
        let entry = self.asset_inventory.get(content_relative_asset_path)?;
        if self.selected_numbers.contains(&entry.proposal_number) {
            return None;
        }

        Some(public_asset_url(entry))
    }

    pub(crate) fn is_selected_number(&self, proposal_number: ProposalNumber) -> bool {
        self.selected_numbers.contains(&proposal_number)
    }

    pub(crate) fn external_url_for_canonical_target(
        &self,
        canonical_target: &Path,
    ) -> Option<&str> {
        let proposal_number = self.canonical_proposal_numbers.get(canonical_target)?;
        if self.selected_numbers.contains(proposal_number) {
            return None;
        }

        self.public_urls_by_number
            .get(proposal_number)
            .map(String::as_str)
    }

    pub(crate) fn external_url_for_content_target(
        &self,
        content_relative_path: &Path,
    ) -> Option<&str> {
        let proposal_number = proposal_number_from_content_markdown_path(content_relative_path)?;
        if self.selected_numbers.contains(&proposal_number) {
            return None;
        }

        self.public_urls_by_number
            .get(&proposal_number)
            .map(String::as_str)
    }

    pub(crate) fn reference_for_required_number(
        &self,
        proposal_number: ProposalNumber,
    ) -> Result<ProposalReference<'_>, Whatever> {
        if self.selected_numbers.contains(&proposal_number) {
            let markdown_path = self
                .markdown_paths_by_number
                .get(&proposal_number)
                .and_then(|paths| paths.iter().next())
                .with_whatever_context(|| {
                    format!("required selected proposal `{proposal_number}` was not found")
                })?;
            return Ok(ProposalReference::Internal(format!(
                "@/{}",
                markdown_path.to_string_lossy()
            )));
        }

        let public_url = self
            .public_urls_by_number
            .get(&proposal_number)
            .with_whatever_context(|| {
                format!("required proposal `{proposal_number}` was not found")
            })?;
        Ok(ProposalReference::External(public_url))
    }

    pub(crate) fn should_preprocess_markdown(&self, content_relative_path: &Path) -> bool {
        match proposal_number_from_content_markdown_path(content_relative_path) {
            Some(proposal_number) => {
                self.selected_numbers.contains(&proposal_number)
                    && self
                        .markdown_paths_by_number
                        .get(&proposal_number)
                        .map(|paths| paths.contains(content_relative_path))
                        .unwrap_or(false)
            }
            None => true,
        }
    }

    pub(crate) fn should_process_proposal_dir(&self, content_relative_path: &Path) -> bool {
        path_component_proposal_number(content_relative_path.file_name())
            .map(|proposal_number| self.selected_numbers.contains(&proposal_number))
            .unwrap_or(true)
    }

    pub(crate) fn should_sync_dirty_path(&self, repo_relative_path: &Path) -> bool {
        let Ok(content_relative_path) = repo_relative_path.strip_prefix(CONTENT_DIR) else {
            return true;
        };

        self.should_sync_content_dirty_path(content_relative_path)
    }

    pub(crate) fn is_selected_proposal_markdown_path(&self, repo_relative_path: &Path) -> bool {
        let Ok(content_relative_path) = repo_relative_path.strip_prefix(CONTENT_DIR) else {
            return false;
        };

        self.is_selected_content_proposal_markdown_path(content_relative_path)
    }

    fn is_selected_content_proposal_markdown_path(&self, content_relative_path: &Path) -> bool {
        let Some(proposal_number) =
            proposal_number_from_content_markdown_path(content_relative_path)
        else {
            return false;
        };

        self.selected_numbers.contains(&proposal_number)
            && self
                .markdown_paths_by_number
                .get(&proposal_number)
                .map(|paths| paths.contains(content_relative_path))
                .unwrap_or(false)
    }

    fn should_sync_content_dirty_path(&self, content_relative_path: &Path) -> bool {
        if proposal_number_from_content_markdown_path(content_relative_path).is_some() {
            return self.is_selected_content_proposal_markdown_path(content_relative_path);
        }

        let mut components = content_relative_path.components();
        let Some(first) = components.next() else {
            return true;
        };

        path_component_proposal_number(Some(first.as_os_str()))
            .map(|proposal_number| self.selected_numbers.contains(&proposal_number))
            .unwrap_or(true)
    }

    pub(crate) fn prune_content(&self, content_root: &Path) -> Result<(), Whatever> {
        let entries = std::fs::read_dir(content_root).with_whatever_context(|_| {
            format!(
                "unable to read materialized content directory `{}` for pruning",
                content_root.to_string_lossy()
            )
        })?;

        for entry in entries {
            let entry = entry.with_whatever_context(|_| {
                format!(
                    "unable to read materialized content directory entry in `{}` for pruning",
                    content_root.to_string_lossy()
                )
            })?;
            let entry_path = entry.path();
            let file_type = entry.file_type().with_whatever_context(|_| {
                format!(
                    "unable to inspect materialized content path `{}` for pruning",
                    entry_path.to_string_lossy()
                )
            })?;

            if file_type.is_file() {
                let Some(number) = flat_proposal_number(&entry_path) else {
                    continue;
                };
                if !self.selected_numbers.contains(&number) {
                    remove_file_if_present(&entry_path)?;
                }
            } else if file_type.is_dir() {
                let Some(number) = path_component_proposal_number(entry_path.file_name()) else {
                    continue;
                };
                if !self.selected_numbers.contains(&number) {
                    remove_dir_if_present(&entry_path)?;
                }
            }
        }

        Ok(())
    }
}

fn missing_asset_dir(error: &walkdir::Error) -> bool {
    error.depth() == 0
        && error.io_error().is_some_and(|io_error| {
            matches!(
                io_error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            )
        })
}

fn asset_dir_for_markdown_path(markdown_path: &Path) -> PathBuf {
    if markdown_path.file_name() == Some(OsStr::new("index.md")) {
        markdown_path
            .parent()
            .map(|proposal_dir| proposal_dir.join("assets"))
            .expect("index path has proposal parent")
    } else {
        markdown_path.with_extension("").join("assets")
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), Whatever> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => {
            snafu::whatever!(
                "unable to prune unselected proposal file `{}`: {error}",
                path.to_string_lossy()
            );
        }
    }
}

fn remove_dir_if_present(path: &Path) -> Result<(), Whatever> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(())
        }
        Err(error) => {
            snafu::whatever!(
                "unable to prune unselected proposal directory `{}`: {error}",
                path.to_string_lossy()
            );
        }
    }
}

pub(crate) fn parse_proposal_preamble<'a>(
    markdown_path: &Path,
    contents: &'a str,
) -> Result<Preamble<'a>, Whatever> {
    let path_lossy = markdown_path.to_string_lossy();
    let (preamble, _) = Preamble::split(contents)
        .with_whatever_context(|_| format!("couldn't split preamble for `{path_lossy}`"))?;
    Preamble::parse(None, preamble)
        .ok()
        .with_whatever_context(|| format!("couldn't parse preamble in `{path_lossy}`"))
}

pub(crate) fn public_site_for_preamble(preamble: &Preamble<'_>) -> ProposalPublicSite {
    let is_erc = preamble
        .fields()
        .any(|field| field.name() == "category" && field.value().trim() == "ERC");

    if is_erc {
        ProposalPublicSite::Ercs
    } else {
        ProposalPublicSite::Eips
    }
}

pub(crate) fn public_site_for_markdown(
    markdown_path: &Path,
    contents: &str,
) -> Result<ProposalPublicSite, Whatever> {
    let preamble = parse_proposal_preamble(markdown_path, contents)?;
    Ok(public_site_for_preamble(&preamble))
}

#[allow(dead_code)]
fn public_asset_url(entry: &ProposalAssetInventoryEntry) -> String {
    let mut url = format!(
        "{}/{}/assets",
        entry.site.asset_base_url(),
        entry.proposal_number.get()
    );
    for component in entry.asset_relative_path.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        url.push('/');
        url.push_str(&percent_encode_url_segment(&component.to_string_lossy()));
    }

    if entry.kind == ProposalAssetKind::Markdown {
        url.truncate(url.len() - ".md".len());
        url.push('/');
    }

    url
}

#[allow(dead_code)]
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

pub(crate) fn flat_proposal_number(path: &Path) -> Option<ProposalNumber> {
    if path.extension().and_then(OsStr::to_str) != Some("md") {
        return None;
    }

    path_component_proposal_number(path.file_stem())
}

pub(crate) fn path_component_proposal_number(component: Option<&OsStr>) -> Option<ProposalNumber> {
    let name = component?.to_str()?;
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    name.parse::<u32>()
        .ok()
        .and_then(|number| NonZeroU32::new(number).map(ProposalNumber))
}

pub(crate) fn proposal_number_from_content_markdown_path(
    content_relative_path: &Path,
) -> Option<ProposalNumber> {
    let mut components = content_relative_path.components();
    let first = components.next()?;
    let first_path = Path::new(first.as_os_str());

    match components.next() {
        None => flat_proposal_number(first_path),
        Some(component)
            if component.as_os_str() == OsStr::new("index.md") && components.next().is_none() =>
        {
            path_component_proposal_number(Some(first.as_os_str()))
        }
        Some(_) => None,
    }
}

pub(crate) fn is_proposal_path(path: &Path) -> bool {
    let Ok(content_relative_path) = path.strip_prefix(CONTENT_DIR) else {
        return false;
    };

    proposal_number_from_content_markdown_path(content_relative_path).is_some()
}

pub(crate) fn resolve_proposal_number_markdown_path(
    active_repo_root: &Path,
    proposal_number: ProposalNumber,
) -> Result<PathBuf, Whatever> {
    let content_root = active_repo_root.join(CONTENT_DIR);
    let mut matches = BTreeSet::new();
    let entries = std::fs::read_dir(&content_root).with_whatever_context(|_| {
        format!(
            "unable to read active repository content directory `{}`",
            content_root.to_string_lossy()
        )
    })?;

    for entry in entries {
        let entry = entry.with_whatever_context(|_| {
            format!(
                "unable to read active repository content directory entry in `{}`",
                content_root.to_string_lossy()
            )
        })?;
        let entry_path = entry.path();
        let file_type = entry.file_type().with_whatever_context(|_| {
            format!(
                "unable to inspect active repository content path `{}`",
                entry_path.to_string_lossy()
            )
        })?;

        if file_type.is_file() {
            if flat_proposal_number(&entry_path) == Some(proposal_number) {
                matches.insert(PathBuf::from(CONTENT_DIR).join(entry.file_name()));
            }
        } else if file_type.is_dir()
            && path_component_proposal_number(Some(entry.file_name().as_os_str()))
                == Some(proposal_number)
        {
            let index_path = entry_path.join("index.md");
            match std::fs::metadata(&index_path) {
                Ok(metadata) if metadata.is_file() => {
                    matches.insert(
                        PathBuf::from(CONTENT_DIR)
                            .join(entry.file_name())
                            .join("index.md"),
                    );
                }
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) => {}
                Err(error) => {
                    snafu::whatever!(
                        "unable to inspect proposal markdown `{}`: {error}",
                        index_path.to_string_lossy()
                    );
                }
            }
        }
    }

    match matches.len() {
        0 => snafu::whatever!(
            "proposal `{proposal_number}` was not found in active repository content"
        ),
        1 => Ok(matches.into_iter().next().expect("one proposal path")),
        _ => {
            let paths = matches
                .iter()
                .map(|path| format!("`{}`", path.to_string_lossy()))
                .collect::<Vec<_>>()
                .join(", ");
            snafu::whatever!(
                "proposal `{proposal_number}` has more than one markdown path in active repository content: {paths}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;

    use super::{
        classify_editorial_number_selector, is_proposal_path,
        proposal_number_from_content_markdown_path, resolve_proposal_number_markdown_path,
        EditorialNumberSelector, OnlyRenderPlan, ProposalNumber, ProposalNumberParseFailure,
    };

    fn number(value: u32) -> ProposalNumber {
        ProposalNumber::from_u32(value).unwrap()
    }

    fn write_file(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn proposal_markdown(number: u32, category: Option<&str>) -> String {
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!("---\neip: {number}\ntitle: Test\n{category}---\nBody\n")
    }

    #[test]
    fn proposal_numbers_parse_cli_selectors_strictly() {
        assert_eq!(
            ProposalNumber::parse_cli_selector("555").unwrap(),
            number(555)
        );
        assert_eq!(
            ProposalNumber::parse_cli_selector("00555").unwrap(),
            number(555)
        );

        for selector in ["+555", "0", "-555", "abc", "555,678", "content/00555.md"] {
            let error = ProposalNumber::parse_cli_selector(selector).unwrap_err();
            assert_eq!(
                error,
                format!(
                    "`{selector}` is not a valid --only selector; expected a positive proposal number"
                )
            );
        }

        let error = ProposalNumber::parse_cli_selector("4294967296").unwrap_err();
        assert_eq!(
            error,
            "`4294967296` is not a valid --only selector; expected a positive proposal number"
        );
    }

    #[test]
    fn editorial_number_selector_classifier_splits_numbers_invalid_numbers_and_paths() {
        assert_eq!(
            classify_editorial_number_selector("000555"),
            EditorialNumberSelector::Number(number(555))
        );

        for (selector, expected_failure) in [
            ("0", ProposalNumberParseFailure::Zero),
            ("+555", ProposalNumberParseFailure::NonDigit),
            ("-555", ProposalNumberParseFailure::NonDigit),
            ("555,678", ProposalNumberParseFailure::NonDigit),
            ("4294967296", ProposalNumberParseFailure::Overflow),
        ] {
            assert_eq!(
                classify_editorial_number_selector(selector),
                EditorialNumberSelector::InvalidNumberLike(expected_failure)
            );
        }

        for selector in ["foo", "draft.md", "4a", "draft-4.md", "content/00555.md"] {
            assert_eq!(
                classify_editorial_number_selector(selector),
                EditorialNumberSelector::PathLike
            );
        }
    }

    #[test]
    fn proposal_path_matching_normalizes_numeric_paths() {
        assert_eq!(
            proposal_number_from_content_markdown_path(Path::new("555.md")),
            Some(number(555))
        );
        assert_eq!(
            proposal_number_from_content_markdown_path(Path::new("00555.md")),
            Some(number(555))
        );
        assert_eq!(
            proposal_number_from_content_markdown_path(Path::new("000555/index.md")),
            Some(number(555))
        );
        assert!(is_proposal_path(Path::new("content/000555/index.md")));
        assert!(!is_proposal_path(Path::new(
            "content/000555/assets/readme.md"
        )));
    }

    #[test]
    fn proposal_number_resolver_returns_exact_flat_markdown_path() {
        for (selector, existing_path) in [
            ("4", "content/4.md"),
            ("004", "content/0004.md"),
            ("0004", "content/004.md"),
        ] {
            let temp = TempDir::new().unwrap();
            write_file(temp.path(), existing_path, "");

            assert_eq!(
                resolve_proposal_number_markdown_path(
                    temp.path(),
                    ProposalNumber::parse_cli_selector(selector).unwrap(),
                )
                .unwrap(),
                Path::new(existing_path)
            );
        }
    }

    #[test]
    fn proposal_number_resolver_returns_exact_directory_index_path() {
        for (selector, existing_path) in [
            ("4", "content/4/index.md"),
            ("004", "content/0004/index.md"),
            ("0004", "content/004/index.md"),
        ] {
            let temp = TempDir::new().unwrap();
            write_file(temp.path(), existing_path, "");

            assert_eq!(
                resolve_proposal_number_markdown_path(
                    temp.path(),
                    ProposalNumber::parse_cli_selector(selector).unwrap(),
                )
                .unwrap(),
                Path::new(existing_path)
            );
        }
    }

    #[test]
    fn proposal_number_resolver_reports_missing_and_ignores_assets_only_dirs() {
        let missing = TempDir::new().unwrap();
        write_file(missing.path(), "content/0005.md", "");
        let error = resolve_proposal_number_markdown_path(missing.path(), number(4))
            .unwrap_err()
            .to_string();
        assert!(error.contains("proposal `4` was not found in active repository content"));

        let assets_only = TempDir::new().unwrap();
        write_file(assets_only.path(), "content/0004/assets/foo.png", "");
        let error = resolve_proposal_number_markdown_path(assets_only.path(), number(4))
            .unwrap_err()
            .to_string();
        assert!(error.contains("proposal `4` was not found in active repository content"));
    }

    #[test]
    fn proposal_number_resolver_reports_ambiguous_markdown_paths() {
        for paths in [
            &["content/4.md", "content/0004/index.md"][..],
            &["content/4.md", "content/0004.md"][..],
            &["content/4/index.md", "content/0004/index.md"][..],
        ] {
            let temp = TempDir::new().unwrap();
            for path in paths {
                write_file(temp.path(), path, "");
            }

            let error = resolve_proposal_number_markdown_path(temp.path(), number(4))
                .unwrap_err()
                .to_string();

            assert!(error.contains(
                "proposal `4` has more than one markdown path in active repository content"
            ));
        }
    }

    #[test]
    fn proposal_number_resolver_searches_only_active_repo_content() {
        let temp = TempDir::new().unwrap();
        let active_repo = temp.path().join("active");
        let sibling_repo = temp.path().join("sibling");
        write_file(&active_repo, "content/0005.md", "");
        write_file(&sibling_repo, "content/0004.md", "");

        let error = resolve_proposal_number_markdown_path(&active_repo, number(4))
            .unwrap_err()
            .to_string();

        assert!(error.contains("proposal `4` was not found in active repository content"));
    }

    #[test]
    fn only_render_plan_requires_selected_markdown_not_assets_only() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555/assets/foo.png", "");

        let error = OnlyRenderPlan::build(content, [number(555)].into_iter().collect())
            .unwrap_err()
            .to_string();

        assert!(error.contains("selected proposal `555` was not found"));
    }

    #[test]
    fn only_render_plan_reports_missing_selected_proposal() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));

        let error = OnlyRenderPlan::build(content, [number(678)].into_iter().collect())
            .unwrap_err()
            .to_string();

        assert!(error.contains("selected proposal `678` was not found"));
    }

    #[test]
    fn only_render_plan_records_exact_public_urls_by_category() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", &proposal_markdown(678, Some("ERC")));
        write_file(
            content,
            "00777.md",
            &proposal_markdown(777, Some("Standards Track")),
        );

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert_eq!(
            plan.public_urls_by_number.get(&number(555)).unwrap(),
            "https://eips.ethereum.org/EIPS/eip-555"
        );
        assert_eq!(
            plan.public_urls_by_number.get(&number(678)).unwrap(),
            "https://ercs.ethereum.org/ERCS/erc-678"
        );
        assert_eq!(
            plan.public_urls_by_number.get(&number(777)).unwrap(),
            "https://eips.ethereum.org/EIPS/eip-777"
        );
    }

    #[test]
    fn only_render_plan_inventories_directory_proposal_assets() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(
            content,
            "00678/index.md",
            &proposal_markdown(678, Some("ERC")),
        );
        write_file(content, "00678/assets/foo.pdf", "");
        write_file(content, "00678/assets/guide.md", "");

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert!(plan.has_proposal_asset(Path::new("00678/assets/foo.pdf")));
        assert!(plan.has_proposal_asset(Path::new("00678/assets/guide.md")));
        assert_eq!(
            plan.public_url_for_omitted_proposal_asset(Path::new("00678/assets/foo.pdf"))
                .unwrap(),
            "https://ercs.ethereum.org/678/assets/foo.pdf"
        );
        assert_eq!(
            plan.public_url_for_omitted_proposal_asset(Path::new("00678/assets/guide.md"))
                .unwrap(),
            "https://ercs.ethereum.org/678/assets/guide/"
        );
    }

    #[test]
    fn only_render_plan_inventories_flat_proposal_assets_when_directory_exists() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", &proposal_markdown(678, None));
        write_file(content, "00678/assets/README.md", "");
        write_file(content, "00678/assets/index.md", "");
        write_file(
            content,
            "00678/assets/Contract Interactions diagram.svg",
            "",
        );

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert_eq!(
            plan.public_url_for_omitted_proposal_asset(Path::new("00678/assets/README.md"))
                .unwrap(),
            "https://eips.ethereum.org/678/assets/README/"
        );
        assert_eq!(
            plan.public_url_for_omitted_proposal_asset(Path::new("00678/assets/index.md"))
                .unwrap(),
            "https://eips.ethereum.org/678/assets/index/"
        );
        assert_eq!(
            plan.public_url_for_omitted_proposal_asset(Path::new(
                "00678/assets/Contract Interactions diagram.svg"
            ))
            .unwrap(),
            "https://eips.ethereum.org/678/assets/Contract%20Interactions%20diagram.svg"
        );
    }

    #[test]
    fn only_render_plan_records_flat_proposals_without_assets_as_empty() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", &proposal_markdown(678, None));

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert!(!plan.has_proposal_asset(Path::new("00678/assets/foo.pdf")));
        assert!(plan
            .public_url_for_omitted_proposal_asset(Path::new("00678/assets/foo.pdf"))
            .is_none());
    }

    #[test]
    fn only_render_plan_does_not_inventory_assets_only_numeric_dirs() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678/assets/foo.pdf", "");

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert!(!plan.has_proposal_asset(Path::new("00678/assets/foo.pdf")));
        assert!(plan
            .public_url_for_omitted_proposal_asset(Path::new("00678/assets/foo.pdf"))
            .is_none());
    }

    #[test]
    fn only_render_plan_does_not_construct_public_asset_urls_for_selected_targets() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", &proposal_markdown(678, None));
        write_file(content, "00678/assets/foo.pdf", "");

        let plan = OnlyRenderPlan::build(content, [number(555), number(678)].into_iter().collect())
            .unwrap();

        assert!(plan.has_proposal_asset(Path::new("00678/assets/foo.pdf")));
        assert!(plan
            .public_url_for_omitted_proposal_asset(Path::new("00678/assets/foo.pdf"))
            .is_none());
    }

    #[test]
    fn only_render_plan_errors_on_malformed_proposal_before_asset_inventory_policy() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", "not front matter");
        write_file(content, "00678/assets/foo.pdf", "");

        let error = OnlyRenderPlan::build(content, [number(555)].into_iter().collect())
            .unwrap_err()
            .to_string();

        assert!(error.contains("couldn't split preamble"));
    }

    #[test]
    fn only_render_plan_does_not_mask_missing_required_targets() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        let error = plan
            .reference_for_required_number(number(678))
            .unwrap_err()
            .to_string();

        assert!(error.contains("required proposal `678` was not found"));
    }

    #[test]
    fn only_render_plan_does_not_mask_malformed_target_front_matter() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", "not front matter");

        let error = OnlyRenderPlan::build(content, [number(555)].into_iter().collect())
            .unwrap_err()
            .to_string();

        assert!(error.contains("couldn't split preamble"));
    }

    #[test]
    fn only_render_plan_detects_conflicting_public_urls() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(
            content,
            "00555/index.md",
            &proposal_markdown(555, Some("ERC")),
        );

        let error = OnlyRenderPlan::build(content, [number(555)].into_iter().collect())
            .unwrap_err()
            .to_string();

        assert!(error.contains("conflicting public URLs"));
    }

    #[test]
    fn only_render_plan_prunes_unselected_proposal_content_only() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00555/assets/foo.png", "");
        write_file(content, "00678.md", &proposal_markdown(678, None));
        write_file(content, "00777/index.md", &proposal_markdown(777, None));
        write_file(content, "00777/assets/foo.png", "");
        write_file(content, "_index.md", "+++\ntitle = \"Home\"\n+++\n");

        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();
        plan.prune_content(content).unwrap();

        assert!(content.join("00555.md").is_file());
        assert!(content.join("00555/assets/foo.png").is_file());
        assert!(!content.join("00678.md").exists());
        assert!(!content.join("00777").exists());
        assert!(content.join("_index.md").is_file());
    }

    #[test]
    fn only_render_plan_filters_dirty_paths_without_filesystem_state() {
        let temp = TempDir::new().unwrap();
        let content = temp.path();
        write_file(content, "00555.md", &proposal_markdown(555, None));
        write_file(content, "00678.md", &proposal_markdown(678, None));
        let plan = OnlyRenderPlan::build(content, [number(555)].into_iter().collect()).unwrap();

        assert!(plan.should_sync_dirty_path(Path::new("content/00555.md")));
        assert!(plan.should_sync_dirty_path(Path::new("content/00555/assets/diagram.png")));
        assert!(plan.should_sync_dirty_path(Path::new("content/_index.md")));
        assert!(plan.should_sync_dirty_path(Path::new(".build-eips.repo.toml")));
        assert!(!plan.should_sync_dirty_path(Path::new("content/00678.md")));
        assert!(!plan.should_sync_dirty_path(Path::new("content/00678/assets/diagram.png")));
        assert!(!plan.should_sync_dirty_path(Path::new("content/00999.md")));

        assert!(plan.is_selected_proposal_markdown_path(Path::new("content/00555.md")));
        assert!(
            !plan.is_selected_proposal_markdown_path(Path::new("content/00555/assets/diagram.png"))
        );
        assert!(!plan.is_selected_proposal_markdown_path(Path::new("content/_index.md")));
        assert!(!plan.is_selected_proposal_markdown_path(Path::new("content/00678.md")));
    }
}
