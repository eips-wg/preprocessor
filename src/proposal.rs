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
