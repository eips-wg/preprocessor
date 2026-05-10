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
use snafu::{OptionExt, ResultExt, Whatever};

use crate::proposal::{
    flat_proposal_number, path_component_proposal_number, public_site_for_markdown, OnlyRenderPlan,
    ProposalNumber, ProposalPublicSite,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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

#[derive(Debug, Clone)]
pub(crate) struct ProposalCatalog {
    records: BTreeMap<String, ProposalCatalogRecord>,
    source_documents: BTreeMap<String, ProposalCatalogSourceDocument>,
}

impl ProposalCatalog {
    pub(crate) fn records(&self) -> &BTreeMap<String, ProposalCatalogRecord> {
        &self.records
    }

    pub(crate) fn source_documents(&self) -> impl Iterator<Item = &ProposalCatalogSourceDocument> {
        self.source_documents.values()
    }

    pub(crate) fn source_document(
        &self,
        prefix: ProposalCatalogPrefix,
        proposal_number: ProposalNumber,
    ) -> Option<&ProposalCatalogSourceDocument> {
        self.source_documents.get(&prefix.key(proposal_number))
    }

    #[allow(dead_code)]
    pub(crate) fn into_records(self) -> BTreeMap<String, ProposalCatalogRecord> {
        self.records
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProposalCatalogSourceDocument {
    number: ProposalNumber,
    prefix: ProposalCatalogPrefix,
    source_path: PathBuf,
    preamble_fields: Vec<(String, String)>,
    body: String,
}

impl ProposalCatalogSourceDocument {
    pub(crate) fn number(&self) -> ProposalNumber {
        self.number
    }

    pub(crate) fn prefix(&self) -> ProposalCatalogPrefix {
        self.prefix
    }

    pub(crate) fn source_path(&self) -> &Path {
        &self.source_path
    }

    pub(crate) fn field(&self, name: &str) -> Option<&str> {
        self.preamble_fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn created(&self) -> Option<&str> {
        self.field("created")
    }

    pub(crate) fn network_upgrade(&self) -> Option<&str> {
        self.field("network-upgrade")
    }

    pub(crate) fn proposal_type(&self) -> Option<&str> {
        self.field("type")
    }

    pub(crate) fn body(&self) -> &str {
        &self.body
    }
}

#[derive(Debug)]
struct CollectedProposalCatalogRecord {
    key: String,
    source_path: PathBuf,
    record: ProposalCatalogRecord,
    source_document: ProposalCatalogSourceDocument,
}

#[derive(Debug)]
struct ProposalCatalogFields {
    title: String,
    description: Option<String>,
    status: String,
    proposal_type: String,
    category: Option<String>,
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

    let mut source_documents = BTreeMap::new();
    let records = records
        .into_iter()
        .map(|(key, metadata)| {
            source_documents.insert(key.clone(), metadata.source_document);
            (key, metadata.record)
        })
        .collect();

    Ok(ProposalCatalog {
        records,
        source_documents,
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

    let site = public_site_for_markdown(markdown_path, contents)?;
    let (preamble, body) = split_proposal_source(markdown_path, contents)?;
    let prefix = ProposalCatalogPrefix::from(site);
    let fields = ProposalCatalogFields::parse(markdown_path, proposal_number, &preamble)?;
    let key = prefix.key(proposal_number);
    let source_document = ProposalCatalogSourceDocument {
        number: proposal_number,
        prefix,
        source_path: markdown_path.to_path_buf(),
        preamble_fields: preamble
            .fields()
            .map(|field| (field.name().to_owned(), field.value().trim().to_owned()))
            .collect(),
        body: body.to_owned(),
    };

    Ok(CollectedProposalCatalogRecord {
        key,
        source_path: markdown_path.to_path_buf(),
        record: ProposalCatalogRecord {
            number: proposal_number,
            prefix,
            title: fields.title,
            description: fields.description,
            status: fields.status,
            proposal_type: fields.proposal_type,
            category: fields.category,
            url: catalog_record_url(proposal_number, site, only_plan),
        },
        source_document,
    })
}

fn split_proposal_source<'a>(
    markdown_path: &Path,
    contents: &'a str,
) -> Result<(Preamble<'a>, &'a str), Whatever> {
    let path_lossy = markdown_path.to_string_lossy();
    let (preamble, body) = Preamble::split(contents)
        .with_whatever_context(|_| format!("couldn't split preamble for `{path_lossy}`"))?;
    let preamble = Preamble::parse(None, preamble)
        .ok()
        .with_whatever_context(|| format!("couldn't parse preamble in `{path_lossy}`"))?;

    Ok((preamble, body))
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

    use super::{collect_proposal_catalog, ProposalCatalogPrefix};
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
    fn proposal_catalog_exposes_source_documents_separately_from_records() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "7773.md",
            "---\neip: 7773\ntitle: Hardfork Meta\nstatus: Draft\ntype: Meta\ncreated: 2024-09-26\nnetwork-upgrade: Glamsterdam\n---\nBody\n",
        );

        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let records = catalog.records();
        let document = catalog.source_documents().next().unwrap();

        assert_eq!(records["eip-7773"].title, "Hardfork Meta");
        assert_eq!(document.number(), number(7773));
        assert_eq!(document.prefix(), ProposalCatalogPrefix::Eip);
        assert_eq!(document.created(), Some("2024-09-26"));
        assert_eq!(document.network_upgrade(), Some("Glamsterdam"));
        assert_eq!(document.body(), "Body\n");
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
