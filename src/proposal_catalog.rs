/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Shared proposal catalog collection from prepared content sources.

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
};

use eipw_preamble::Preamble;
use serde::Serialize;
use snafu::{ResultExt, Whatever};
use toml_datetime::Datetime;

use crate::{
    markdown::{extract_authors, Author},
    proposal::{
        flat_proposal_number, parse_proposal_preamble, path_component_proposal_number,
        public_site_for_preamble, OnlyRenderPlan, ProposalNumber, ProposalPublicSite,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ProposalCatalogPrefix {
    #[serde(rename = "EIP")]
    Eip,
    #[serde(rename = "ERC")]
    Erc,
}

impl ProposalCatalogPrefix {
    pub(crate) fn key(self, proposal_number: ProposalNumber) -> String {
        format!("{self}-{proposal_number}").to_ascii_lowercase()
    }
}

impl From<ProposalPublicSite> for ProposalCatalogPrefix {
    fn from(site: ProposalPublicSite) -> Self {
        match site {
            ProposalPublicSite::Eips => Self::Eip,
            ProposalPublicSite::Ercs => Self::Erc,
        }
    }
}

impl fmt::Display for ProposalCatalogPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Eip => formatter.write_str("EIP"),
            Self::Erc => formatter.write_str("ERC"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProposalCatalogRecord {
    pub(crate) number: ProposalNumber,
    pub(crate) prefix: ProposalCatalogPrefix,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    #[serde(rename = "type")]
    pub(crate) proposal_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
    pub(crate) url: String,
}

// Phase 3 exposes this view before Phase 4 consumes it from rendering hooks.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProposalSearchAuthor {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) github: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) email: Option<String>,
    pub(crate) display: String,
}

// Phase 3 exposes this view before Phase 4 consumes it from rendering hooks.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProposalSearchMetadataRecord {
    pub(crate) proposal_id: String,
    pub(crate) number: ProposalNumber,
    pub(crate) prefix: ProposalCatalogPrefix,
    pub(crate) title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) status: String,
    #[serde(rename = "type")]
    pub(crate) proposal_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) category: Option<String>,
    pub(crate) authors: Vec<ProposalSearchAuthor>,
    pub(crate) author_filter_values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created: Option<Datetime>,
    pub(crate) url: String,
}

// Relationship fields are collected internally for later search result/filter work.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProposalCatalogRelationships {
    requires: Vec<ProposalNumber>,
    replaces: Vec<ProposalNumber>,
    superseded_by: Vec<ProposalNumber>,
    discussions_to: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct ProposalCatalogEntry {
    number: ProposalNumber,
    prefix: ProposalCatalogPrefix,
    title: String,
    description: Option<String>,
    status: String,
    proposal_type: String,
    category: Option<String>,
    url: String,
    authors: Vec<Author>,
    created: Option<Datetime>,
    relationships: ProposalCatalogRelationships,
}

impl ProposalCatalogEntry {
    fn metadata_json_record(&self) -> ProposalCatalogRecord {
        ProposalCatalogRecord {
            number: self.number,
            prefix: self.prefix,
            title: self.title.clone(),
            description: self.description.clone(),
            status: self.status.clone(),
            proposal_type: self.proposal_type.clone(),
            category: self.category.clone(),
            url: self.url.clone(),
        }
    }

    fn search_metadata_record(&self) -> ProposalSearchMetadataRecord {
        let authors = self.authors.iter().map(search_author).collect::<Vec<_>>();
        let author_filter_values = self
            .authors
            .iter()
            .map(|author| author.name.clone())
            .collect::<Vec<_>>();

        ProposalSearchMetadataRecord {
            proposal_id: format!("{}-{}", self.prefix, self.number),
            number: self.number,
            prefix: self.prefix,
            title: self.title.clone(),
            description: self.description.clone(),
            status: self.status.clone(),
            proposal_type: self.proposal_type.clone(),
            category: self.category.clone(),
            authors,
            author_filter_values,
            created: self.created,
            url: self.url.clone(),
        }
    }
}

fn search_author(author: &Author) -> ProposalSearchAuthor {
    ProposalSearchAuthor {
        name: author.name.clone(),
        github: author.github.clone(),
        email: author.email.clone(),
        display: search_author_display(author),
    }
}

fn search_author_display(author: &Author) -> String {
    match (&author.github, &author.email) {
        (Some(github), Some(email)) => format!("{} (@{}) <{}>", author.name, github, email),
        (Some(github), None) => format!("{} (@{})", author.name, github),
        (None, Some(email)) => format!("{} <{}>", author.name, email),
        (None, None) => author.name.clone(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProposalCatalog {
    records: BTreeMap<String, ProposalCatalogEntry>,
}

impl ProposalCatalog {
    pub(crate) fn into_records(self) -> BTreeMap<String, ProposalCatalogRecord> {
        self.records
            .into_iter()
            .map(|(key, entry)| (key, entry.metadata_json_record()))
            .collect()
    }

    pub(crate) fn into_metadata_json_records(self) -> BTreeMap<String, ProposalCatalogRecord> {
        self.into_records()
    }

    #[allow(dead_code)]
    pub(crate) fn search_metadata_records(&self) -> BTreeMap<String, ProposalSearchMetadataRecord> {
        self.records
            .iter()
            .map(|(key, entry)| (key.clone(), entry.search_metadata_record()))
            .collect()
    }
}

#[derive(Debug)]
struct CollectedProposalCatalogRecord {
    key: String,
    source_path: PathBuf,
    record: ProposalCatalogEntry,
}

#[derive(Debug)]
struct ProposalCatalogFields {
    title: String,
    description: Option<String>,
    status: String,
    proposal_type: String,
    category: Option<String>,
    authors: Vec<Author>,
    created: Option<Datetime>,
    relationships: ProposalCatalogRelationships,
}

impl ProposalCatalogFields {
    fn parse(
        markdown_path: &Path,
        proposal_number: ProposalNumber,
        preamble: &Preamble<'_>,
    ) -> Result<Self, Whatever> {
        validate_preamble_proposal_number(markdown_path, proposal_number, preamble)?;

        Ok(Self {
            title: required_catalog_field(markdown_path, preamble, "title")?,
            description: optional_catalog_field(preamble, "description"),
            status: required_catalog_field(markdown_path, preamble, "status")?,
            proposal_type: required_catalog_field(markdown_path, preamble, "type")?,
            category: optional_catalog_field(preamble, "category"),
            authors: catalog_authors(markdown_path, preamble)?,
            created: optional_catalog_datetime_field(markdown_path, preamble, "created")?,
            relationships: ProposalCatalogRelationships {
                requires: optional_catalog_number_list(markdown_path, preamble, "requires")?,
                replaces: optional_catalog_number_list(markdown_path, preamble, "replaces")?,
                superseded_by: optional_catalog_number_list(
                    markdown_path,
                    preamble,
                    "superseded-by",
                )?,
                discussions_to: optional_catalog_field(preamble, "discussions-to"),
            },
        })
    }
}

struct ContentRootEntry {
    entry_path: PathBuf,
    file_type: std::fs::FileType,
}

pub(crate) fn collect_proposal_catalog(
    content_root: &Path,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<ProposalCatalog, Whatever> {
    let mut records = BTreeMap::<String, CollectedProposalCatalogRecord>::new();

    for entry in sorted_content_root_entries(content_root)? {
        if entry.file_type.is_file() {
            let Some(proposal_number) = flat_proposal_number(&entry.entry_path) else {
                continue;
            };
            insert_collected_proposal_catalog_record(
                &mut records,
                collect_proposal_catalog_from_path(
                    content_root,
                    proposal_number,
                    &entry.entry_path,
                    only_plan,
                )?,
            )?;
        } else if entry.file_type.is_dir() {
            let Some(proposal_number) =
                path_component_proposal_number(entry.entry_path.file_name())
            else {
                continue;
            };
            let index_path = entry.entry_path.join("index.md");
            match std::fs::read_to_string(&index_path) {
                Ok(contents) => {
                    insert_collected_proposal_catalog_record(
                        &mut records,
                        collect_proposal_catalog_from_contents(
                            content_root,
                            proposal_number,
                            &index_path,
                            &contents,
                            only_plan,
                        )?,
                    )?;
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

    Ok(ProposalCatalog {
        records: records
            .into_iter()
            .map(|(key, metadata)| (key, metadata.record))
            .collect(),
    })
}

fn sorted_content_root_entries(content_root: &Path) -> Result<Vec<ContentRootEntry>, Whatever> {
    let entries = std::fs::read_dir(content_root).with_whatever_context(|_| {
        format!(
            "unable to read materialized content directory `{}` for proposal metadata",
            content_root.to_string_lossy()
        )
    })?;

    let mut entries = entries
        .map(|entry| -> Result<ContentRootEntry, Whatever> {
            let entry = entry.with_whatever_context(|_| {
                format!(
                    "unable to read materialized content directory entry in `{}` for proposal metadata",
                    content_root.to_string_lossy()
                )
            })?;
            let entry_path = entry.path();
            let file_type = entry.file_type().with_whatever_context(|_| {
                format!(
                    "unable to inspect materialized content path `{}` for proposal metadata",
                    entry_path.to_string_lossy()
                )
            })?;

            Ok(ContentRootEntry {
                entry_path,
                file_type,
            })
        })
        .collect::<Result<Vec<_>, Whatever>>()?;
    entries.sort_by(|left, right| left.entry_path.cmp(&right.entry_path));
    Ok(entries)
}

fn collect_proposal_catalog_from_path(
    content_root: &Path,
    proposal_number: ProposalNumber,
    markdown_path: &Path,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<CollectedProposalCatalogRecord, Whatever> {
    let contents = std::fs::read_to_string(markdown_path).with_whatever_context(|_| {
        format!(
            "unable to read proposal markdown `{}`",
            markdown_path.to_string_lossy()
        )
    })?;
    collect_proposal_catalog_from_contents(
        content_root,
        proposal_number,
        markdown_path,
        &contents,
        only_plan,
    )
}

fn collect_proposal_catalog_from_contents(
    content_root: &Path,
    proposal_number: ProposalNumber,
    markdown_path: &Path,
    contents: &str,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<CollectedProposalCatalogRecord, Whatever> {
    markdown_path
        .strip_prefix(content_root)
        .with_whatever_context(|_| {
            format!(
                "proposal markdown `{}` is outside content root `{}`",
                markdown_path.to_string_lossy(),
                content_root.to_string_lossy()
            )
        })?;

    let preamble = parse_proposal_preamble(markdown_path, contents)?;
    let site = public_site_for_preamble(&preamble);
    let prefix = ProposalCatalogPrefix::from(site);
    let fields = ProposalCatalogFields::parse(markdown_path, proposal_number, &preamble)?;

    Ok(CollectedProposalCatalogRecord {
        key: prefix.key(proposal_number),
        source_path: markdown_path.to_path_buf(),
        record: ProposalCatalogEntry {
            number: proposal_number,
            prefix,
            title: fields.title,
            description: fields.description,
            status: fields.status,
            proposal_type: fields.proposal_type,
            category: fields.category,
            url: catalog_record_url(proposal_number, site, only_plan),
            authors: fields.authors,
            created: fields.created,
            relationships: fields.relationships,
        },
    })
}

fn insert_collected_proposal_catalog_record(
    records: &mut BTreeMap<String, CollectedProposalCatalogRecord>,
    metadata: CollectedProposalCatalogRecord,
) -> Result<(), Whatever> {
    if let Some(existing) = records.get(&metadata.key) {
        snafu::whatever!(
            "duplicate proposal metadata key `{}` from `{}` and `{}`",
            metadata.key,
            existing.source_path.to_string_lossy(),
            metadata.source_path.to_string_lossy()
        );
    }

    records.insert(metadata.key.clone(), metadata);
    Ok(())
}

fn catalog_record_url(
    proposal_number: ProposalNumber,
    site: ProposalPublicSite,
    only_plan: Option<&OnlyRenderPlan>,
) -> String {
    if only_plan.is_some_and(|plan| !plan.is_selected_number(proposal_number)) {
        site.proposal_url(proposal_number)
    } else {
        format!("/{}/", proposal_number.get())
    }
}

fn required_catalog_field(
    markdown_path: &Path,
    preamble: &Preamble<'_>,
    field_name: &str,
) -> Result<String, Whatever> {
    let Some(field) = preamble.by_name(field_name) else {
        snafu::whatever!(
            "missing required proposal metadata field `{field_name}` in `{}`",
            markdown_path.to_string_lossy()
        );
    };
    let value = field.value().trim();
    if value.is_empty() {
        snafu::whatever!(
            "missing required proposal metadata field `{field_name}` in `{}`",
            markdown_path.to_string_lossy()
        );
    }

    Ok(value.to_owned())
}

fn optional_catalog_field(preamble: &Preamble<'_>, field_name: &str) -> Option<String> {
    preamble
        .by_name(field_name)
        .map(|field| field.value().trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn optional_catalog_datetime_field(
    markdown_path: &Path,
    preamble: &Preamble<'_>,
    field_name: &str,
) -> Result<Option<Datetime>, Whatever> {
    optional_catalog_field(preamble, field_name)
        .map(|value| {
            value.parse::<Datetime>().with_whatever_context(|_| {
                format!(
                    "couldn't parse {field_name} in `{}`",
                    markdown_path.to_string_lossy()
                )
            })
        })
        .transpose()
}

fn catalog_authors(markdown_path: &Path, preamble: &Preamble<'_>) -> Result<Vec<Author>, Whatever> {
    optional_catalog_field(preamble, "author")
        .map(|value| {
            extract_authors(&value).with_whatever_context(|_| {
                format!(
                    "couldn't parse author in `{}`",
                    markdown_path.to_string_lossy()
                )
            })
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn optional_catalog_number_list(
    markdown_path: &Path,
    preamble: &Preamble<'_>,
    field_name: &str,
) -> Result<Vec<ProposalNumber>, Whatever> {
    optional_catalog_field(preamble, field_name)
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .map(|item| {
                    let number = item.parse::<u32>().with_whatever_context(|_| {
                        format!(
                            "couldn't parse {field_name} in `{}`",
                            markdown_path.to_string_lossy()
                        )
                    })?;
                    let Ok(proposal_number) = ProposalNumber::from_u32(number) else {
                        snafu::whatever!(
                            "proposal number in `{field_name}` in `{}` must be positive",
                            markdown_path.to_string_lossy()
                        )
                    };
                    Ok(proposal_number)
                })
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn validate_preamble_proposal_number(
    markdown_path: &Path,
    path_proposal_number: ProposalNumber,
    preamble: &Preamble<'_>,
) -> Result<(), Whatever> {
    for field in preamble
        .fields()
        .filter(|field| matches!(field.name(), "eip" | "number"))
    {
        let field_name = field.name();
        let field_value = field.value().trim();
        let parsed = field_value.parse::<u32>().with_whatever_context(|_| {
            format!(
                "couldn't parse proposal number field `{field_name}` in `{}`",
                markdown_path.to_string_lossy()
            )
        })?;
        let Ok(preamble_proposal_number) = ProposalNumber::from_u32(parsed) else {
            snafu::whatever!(
                "proposal number field `{field_name}` in `{}` must be positive",
                markdown_path.to_string_lossy()
            );
        };

        if preamble_proposal_number != path_proposal_number {
            snafu::whatever!(
                "proposal metadata number mismatch in `{}`: path indicates `{path_proposal_number}`, but `{field_name}` contains `{preamble_proposal_number}`",
                markdown_path.to_string_lossy()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::TempDir;
    use toml_datetime::Datetime;

    use super::{
        collect_proposal_catalog, ProposalCatalogPrefix, ProposalCatalogRelationships,
        ProposalSearchAuthor,
    };
    use crate::proposal::{OnlyRenderPlan, ProposalNumber};

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

    fn catalog_proposal_markdown(
        number: u32,
        title: &str,
        category: Option<&str>,
        description: Option<&str>,
    ) -> String {
        let description = description
            .map(|description| format!("description: {description}\n"))
            .unwrap_or_default();
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: {title}\n{description}status: Final\ntype: Standards Track\n{category}---\nBody\n"
        )
    }

    fn catalog_proposal_markdown_with_extra(
        number: u32,
        title: &str,
        category: Option<&str>,
        description: Option<&str>,
        extra: &str,
    ) -> String {
        let description = description
            .map(|description| format!("description: {description}\n"))
            .unwrap_or_default();
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: {title}\n{description}status: Final\ntype: Standards Track\n{category}{extra}---\nBody\n"
        )
    }

    #[test]
    fn proposal_catalog_collects_flat_and_directory_proposals() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "020.md",
            &catalog_proposal_markdown(20, "Flat Proposal", None, None),
        );
        write_file(
            temp.path(),
            "21/index.md",
            &catalog_proposal_markdown(21, "Directory Proposal", Some("ERC"), Some("Desc")),
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let records = catalog.into_records();

        assert_eq!(records.len(), 2);
        assert_eq!(records["eip-20"].number, number(20));
        assert_eq!(records["eip-20"].prefix, ProposalCatalogPrefix::Eip);
        assert_eq!(records["eip-20"].title, "Flat Proposal");
        assert_eq!(records["eip-20"].url, "/20/");
        assert_eq!(records["erc-21"].number, number(21));
        assert_eq!(records["erc-21"].prefix, ProposalCatalogPrefix::Erc);
        assert_eq!(records["erc-21"].description.as_deref(), Some("Desc"));
    }

    #[test]
    fn proposal_catalog_reports_malformed_preambles() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "001.md", "not front matter\n");

        let error = collect_proposal_catalog(temp.path(), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("couldn't split preamble"));
        assert!(error.contains("001.md"));
    }

    #[test]
    fn proposal_catalog_reports_path_preamble_number_mismatch() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "001.md",
            "---\neip: 2\ntitle: Proposal 1\nstatus: Final\ntype: Standards Track\n---\nBody\n",
        );

        let error = collect_proposal_catalog(temp.path(), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("proposal metadata number mismatch"));
        assert!(error.contains("001.md"));
        assert!(error.contains("path indicates `1`"));
        assert!(error.contains("`eip` contains `2`"));
    }

    #[test]
    fn proposal_catalog_collects_independently_of_json_writing() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "001.md",
            &catalog_proposal_markdown(1, "Proposal 1", None, None),
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let records = catalog.into_records();

        assert_eq!(records["eip-1"].status, "Final");
        assert!(!temp
            .path()
            .join("static/assets/data/proposals.json")
            .exists());
    }

    #[test]
    fn proposal_catalog_search_metadata_view_exposes_phase_four_fields() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "1559/index.md",
            &catalog_proposal_markdown_with_extra(
                1559,
                "Fee Market Change",
                Some("Core"),
                Some("A transaction pricing mechanism."),
                "author: Alice <alice@example.com>, Bob (@bob), Carol (@carol) <carol@example.com>, Dan\ncreated: 2021-04-13\nrequires: 1, 2\nreplaces: 3\nsuperseded-by: 4\ndiscussions-to: https://ethereum-magicians.org/t/eip-1559\n",
            ),
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let search_records = catalog.search_metadata_records();
        let record = &search_records["eip-1559"];
        let relationships = &catalog.records["eip-1559"].relationships;

        assert_eq!(record.proposal_id, "EIP-1559");
        assert_eq!(record.number, number(1559));
        assert_eq!(record.prefix, ProposalCatalogPrefix::Eip);
        assert_eq!(record.title, "Fee Market Change");
        assert_eq!(
            record.description.as_deref(),
            Some("A transaction pricing mechanism.")
        );
        assert_eq!(record.status, "Final");
        assert_eq!(record.proposal_type, "Standards Track");
        assert_eq!(record.category.as_deref(), Some("Core"));
        assert_eq!(
            record.authors,
            vec![
                ProposalSearchAuthor {
                    name: "Alice".to_owned(),
                    github: None,
                    email: Some("alice@example.com".to_owned()),
                    display: "Alice <alice@example.com>".to_owned(),
                },
                ProposalSearchAuthor {
                    name: "Bob".to_owned(),
                    github: Some("bob".to_owned()),
                    email: None,
                    display: "Bob (@bob)".to_owned(),
                },
                ProposalSearchAuthor {
                    name: "Carol".to_owned(),
                    github: Some("carol".to_owned()),
                    email: Some("carol@example.com".to_owned()),
                    display: "Carol (@carol) <carol@example.com>".to_owned(),
                },
                ProposalSearchAuthor {
                    name: "Dan".to_owned(),
                    github: None,
                    email: None,
                    display: "Dan".to_owned(),
                },
            ]
        );
        assert_eq!(record.authors[3].display, "Dan");
        assert_eq!(
            record.author_filter_values,
            vec![
                "Alice".to_owned(),
                "Bob".to_owned(),
                "Carol".to_owned(),
                "Dan".to_owned()
            ]
        );
        assert_eq!(
            record.created,
            Some("2021-04-13".parse::<Datetime>().unwrap())
        );
        assert_eq!(record.url, "/1559/");
        assert_eq!(relationships.requires, vec![number(1), number(2)]);
        assert_eq!(relationships.replaces, vec![number(3)]);
        assert_eq!(relationships.superseded_by, vec![number(4)]);
        assert_eq!(
            relationships.discussions_to.as_deref(),
            Some("https://ethereum-magicians.org/t/eip-1559")
        );
    }

    #[test]
    fn proposal_catalog_search_metadata_handles_absent_optional_fields() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "001.md",
            &catalog_proposal_markdown(1, "Minimal Proposal", None, None),
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let search_records = catalog.search_metadata_records();
        let record = &search_records["eip-1"];
        let relationships = &catalog.records["eip-1"].relationships;

        assert_eq!(record.proposal_id, "EIP-1");
        assert_eq!(record.prefix, ProposalCatalogPrefix::Eip);
        assert_eq!(record.description, None);
        assert_eq!(record.category, None);
        assert!(record.authors.is_empty());
        assert!(record.author_filter_values.is_empty());
        assert_eq!(record.created, None);
        assert_eq!(record.url, "/1/");
        assert_eq!(relationships, &ProposalCatalogRelationships::default());
    }

    #[test]
    fn proposal_catalog_search_metadata_uses_existing_url_policy() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "020.md",
            &catalog_proposal_markdown(20, "Proposal 20", None, None),
        );
        write_file(
            temp.path(),
            "021.md",
            &catalog_proposal_markdown(21, "Proposal 21", None, None),
        );
        write_file(
            temp.path(),
            "022.md",
            &catalog_proposal_markdown(22, "Proposal 22", Some("ERC"), None),
        );
        let plan = OnlyRenderPlan::build(temp.path(), [number(20)].into_iter().collect()).unwrap();

        let catalog = collect_proposal_catalog(temp.path(), Some(&plan)).unwrap();
        let records = catalog.search_metadata_records();

        assert_eq!(records["eip-20"].url, "/20/");
        assert_eq!(
            records["eip-21"].url,
            "https://eips.ethereum.org/EIPS/eip-21"
        );
        assert_eq!(
            records["erc-22"].url,
            "https://ercs.ethereum.org/ERCS/erc-22"
        );
    }

    #[test]
    fn proposal_catalog_search_metadata_excludes_relationship_fields_by_default() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "001.md",
            &catalog_proposal_markdown_with_extra(
                1,
                "Proposal 1",
                None,
                None,
                "requires: 2\nreplaces: 3\nsuperseded-by: 4\ndiscussions-to: https://example.test/discuss\n",
            ),
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let search_records = catalog.search_metadata_records();
        let record = &search_records["eip-1"];
        let value = serde_json::to_value(record).unwrap();
        let object = value.as_object().unwrap();

        assert!(!object.contains_key("requires"));
        assert!(!object.contains_key("replaces"));
        assert!(!object.contains_key("superseded_by"));
        assert!(!object.contains_key("superseded-by"));
        assert!(!object.contains_key("discussions_to"));
        assert!(!object.contains_key("discussions-to"));
    }

    #[test]
    fn proposal_catalog_applies_targeted_url_policy() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "020.md",
            &catalog_proposal_markdown(20, "Proposal 20", None, None),
        );
        write_file(
            temp.path(),
            "021.md",
            &catalog_proposal_markdown(21, "Proposal 21", None, None),
        );
        write_file(
            temp.path(),
            "022.md",
            &catalog_proposal_markdown(22, "Proposal 22", Some("ERC"), None),
        );
        let plan = OnlyRenderPlan::build(temp.path(), [number(20)].into_iter().collect()).unwrap();

        let catalog = collect_proposal_catalog(temp.path(), Some(&plan)).unwrap();
        let records = catalog.into_records();

        assert_eq!(records["eip-20"].url, "/20/");
        assert_eq!(
            records["eip-21"].url,
            "https://eips.ethereum.org/EIPS/eip-21"
        );
        assert_eq!(
            records["erc-22"].url,
            "https://ercs.ethereum.org/ERCS/erc-22"
        );
    }
}
