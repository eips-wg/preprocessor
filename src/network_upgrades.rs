/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Network upgrade source selection and in-memory membership model.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::Write,
    path::Path,
};

use chrono::{Datelike, NaiveDate};
use eipw_preamble::Preamble;
use lazy_static::lazy_static;
use log::warn;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Serialize;
use snafu::{OptionExt, ResultExt, Whatever};

use crate::{
    proposal::{flat_proposal_number, path_component_proposal_number, ProposalNumber},
    proposal_catalog::{
        ProposalCatalog, ProposalCatalogPrefix, ProposalCatalogRecord,
        ProposalCatalogSourceDocument,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeIndex {
    pub(crate) upgrades: Vec<NetworkUpgrade>,
    pub(crate) memberships_by_proposal: BTreeMap<ProposalNumber, Vec<NetworkUpgradeMembership>>,
    pub(crate) selected_sources: Vec<NetworkUpgradeSelectedSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgrade {
    pub(crate) display_name: String,
    pub(crate) slug: String,
    pub(crate) source_meta_eip: ProposalNumber,
    pub(crate) source_bucket: NetworkUpgradeSourceBucket,
    pub(crate) meta_url: String,
    pub(crate) meta_status: String,
    pub(crate) stages: Vec<NetworkUpgradeStage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeStage {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) rows: Vec<NetworkUpgradeMemberRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeMemberRow {
    pub(crate) number: ProposalNumber,
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) proposal_type: String,
    pub(crate) category: Option<String>,
    pub(crate) url: String,
    pub(crate) subgroup: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeMembership {
    pub(crate) display_name: String,
    pub(crate) slug: String,
    pub(crate) source_meta_eip: ProposalNumber,
    pub(crate) meta_url: String,
    pub(crate) stage_key: String,
    pub(crate) stage_label: String,
    pub(crate) subgroup: Option<String>,
}

#[derive(Debug, Serialize)]
struct GeneratedHardforksFrontMatter {
    title: &'static str,
    template: &'static str,
    extra: GeneratedHardforksExtra,
}

#[derive(Debug, Serialize)]
struct GeneratedHardforksExtra {
    network_upgrades: Vec<GeneratedHardfork>,
}

#[derive(Debug, Serialize)]
struct GeneratedHardfork {
    slug: String,
    display_name: String,
    meta_eip: ProposalNumber,
    meta_url: String,
    meta_status: String,
    stages: Vec<GeneratedHardforkStage>,
}

#[derive(Debug, Serialize)]
struct GeneratedHardforkStage {
    key: String,
    label: String,
    rows: Vec<GeneratedHardforkRow>,
}

#[derive(Debug, Serialize)]
struct GeneratedHardforkRow {
    number: ProposalNumber,
    title: String,
    status: String,
    #[serde(rename = "type")]
    proposal_type: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subgroup: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkUpgradeSourceBucket {
    Marked,
    Transitional,
    Permanent,
}

impl fmt::Display for NetworkUpgradeSourceBucket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Marked => formatter.write_str("marked modern source"),
            Self::Transitional => formatter.write_str("transitional modern registry"),
            Self::Permanent => formatter.write_str("permanent registry"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkUpgradeParserMode {
    ModernStages,
    LegacyIncludedList,
    ExplicitIncludedMembers(&'static [u32]),
    #[allow(dead_code)]
    EmptyMembers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeRegistrySource {
    pub(crate) bucket: NetworkUpgradeSourceBucket,
    pub(crate) source_meta_eip: u32,
    pub(crate) parser_mode: NetworkUpgradeParserMode,
    pub(crate) upgrades: Vec<NetworkUpgradeRegistryUpgrade>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeRegistryUpgrade {
    pub(crate) display_name: &'static str,
    pub(crate) slug: Option<&'static str>,
    pub(crate) sort_order: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkedNetworkUpgradeSource {
    pub(crate) source_meta_eip: ProposalNumber,
    pub(crate) display_name: String,
    pub(crate) slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NetworkUpgradeSelectedSource {
    pub(crate) source_meta_eip: ProposalNumber,
    pub(crate) source_bucket: NetworkUpgradeSourceBucket,
    pub(crate) display_name: String,
    pub(crate) slug: String,
    pub(crate) parser_mode: NetworkUpgradeParserMode,
    pub(crate) sort_order: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedNetworkUpgradeSource {
    details: NetworkUpgradeSelectedSource,
    render_sort_key: RenderSortKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RenderSortKey {
    value: i64,
    source_meta_eip: ProposalNumber,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum NetworkUpgradeStageKey {
    Included,
    Scheduled,
    Considered,
    Proposed,
    Declined,
}

impl NetworkUpgradeStageKey {
    fn stable_key(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::Scheduled => "scheduled",
            Self::Considered => "considered",
            Self::Proposed => "proposed",
            Self::Declined => "declined",
        }
    }

    fn render_rank(self) -> u8 {
        match self {
            Self::Included => 0,
            Self::Scheduled => 10,
            Self::Considered => 20,
            Self::Proposed => 30,
            Self::Declined => 40,
        }
    }
}

impl fmt::Display for NetworkUpgradeStageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Included => formatter.write_str("Included"),
            Self::Scheduled => formatter.write_str("Scheduled for Inclusion"),
            Self::Considered => formatter.write_str("Considered for Inclusion"),
            Self::Proposed => formatter.write_str("Proposed for Inclusion"),
            Self::Declined => formatter.write_str("Declined for Inclusion"),
        }
    }
}

#[derive(Debug, Clone)]
struct ParsedNetworkUpgradeStage {
    key: NetworkUpgradeStageKey,
    members: Vec<ParsedNetworkUpgradeMember>,
}

#[derive(Debug, Clone)]
struct ParsedNetworkUpgradeMember {
    number: ProposalNumber,
    subgroup: Option<String>,
}

#[derive(Debug, Clone)]
struct ActiveStage {
    key: NetworkUpgradeStageKey,
    level: u8,
    subgroup: Option<String>,
}

#[derive(Debug)]
struct HeadingCapture {
    level: HeadingLevel,
    text: String,
}

#[derive(Debug)]
struct LinkCapture {
    dest_url: String,
    text: String,
}

#[allow(dead_code)]
pub(crate) fn collect_network_upgrades(
    catalog: &ProposalCatalog,
) -> Result<NetworkUpgradeIndex, Whatever> {
    collect_network_upgrades_with_registries(
        catalog,
        &transitional_modern_registry(),
        &permanent_registry(),
    )
}

pub(crate) fn collect_network_upgrades_with_registries(
    catalog: &ProposalCatalog,
    transitional_sources: &[NetworkUpgradeRegistrySource],
    permanent_sources: &[NetworkUpgradeRegistrySource],
) -> Result<NetworkUpgradeIndex, Whatever> {
    let selected_sources =
        select_network_upgrade_sources(catalog, transitional_sources, permanent_sources)?;
    let mut upgrades = Vec::new();
    let mut memberships_by_proposal =
        BTreeMap::<ProposalNumber, Vec<NetworkUpgradeMembership>>::new();

    for selected_source in &selected_sources {
        let source_document = source_document_for_number(
            catalog,
            selected_source.details.source_meta_eip,
            "network upgrade source",
        )?;
        let meta_record = proposal_record_for_number(
            catalog,
            selected_source.details.source_meta_eip,
            "network upgrade source",
        )?;
        let parsed_stages = match selected_source.details.parser_mode {
            NetworkUpgradeParserMode::ModernStages => parse_modern_stage_members(source_document)?,
            NetworkUpgradeParserMode::LegacyIncludedList => {
                parse_legacy_included_members(source_document)?
            }
            NetworkUpgradeParserMode::ExplicitIncludedMembers(members) => {
                explicit_included_members(members)?
            }
            NetworkUpgradeParserMode::EmptyMembers => Vec::new(),
        };
        let upgrade = build_network_upgrade(
            catalog,
            &selected_source.details,
            meta_record,
            parsed_stages,
        )?;

        for stage in &upgrade.stages {
            for row in &stage.rows {
                memberships_by_proposal.entry(row.number).or_default().push(
                    NetworkUpgradeMembership {
                        display_name: upgrade.display_name.clone(),
                        slug: upgrade.slug.clone(),
                        source_meta_eip: upgrade.source_meta_eip,
                        meta_url: upgrade.meta_url.clone(),
                        stage_key: stage.key.clone(),
                        stage_label: stage.label.clone(),
                        subgroup: row.subgroup.clone(),
                    },
                );
            }
        }

        upgrades.push(upgrade);
    }

    Ok(NetworkUpgradeIndex {
        upgrades,
        memberships_by_proposal,
        selected_sources: selected_sources
            .into_iter()
            .map(|selected_source| selected_source.details)
            .collect(),
    })
}

pub(crate) fn collect_marked_modern_sources(
    catalog: &ProposalCatalog,
) -> Result<Vec<MarkedNetworkUpgradeSource>, Whatever> {
    catalog
        .source_documents()
        .filter_map(|source_document| {
            source_document
                .network_upgrade()
                .map(|display_name| (source_document, display_name))
        })
        .map(|(source_document, display_name)| {
            marked_source_from_document(source_document, display_name)
        })
        .collect()
}

pub(crate) fn write_hardforks_index(
    content_root: &Path,
    index: &NetworkUpgradeIndex,
) -> Result<(), Whatever> {
    let hardforks_dir = content_root.join("hardforks");
    std::fs::create_dir_all(&hardforks_dir).with_whatever_context(|_| {
        format!(
            "unable to create generated hardforks directory `{}`",
            hardforks_dir.to_string_lossy()
        )
    })?;
    let index_path = hardforks_dir.join("_index.md");
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&index_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            snafu::whatever!(
                "generated hardforks index `{}` already exists; refusing to overwrite it",
                index_path.to_string_lossy()
            );
        }
        Err(error) => {
            snafu::whatever!(
                "unable to create generated hardforks index `{}`: {error}",
                index_path.to_string_lossy()
            );
        }
    };

    let front_matter = GeneratedHardforksFrontMatter {
        title: "Hardforks",
        template: "hardforks.html",
        extra: GeneratedHardforksExtra {
            network_upgrades: index
                .upgrades
                .iter()
                .map(generated_hardfork)
                .collect::<Vec<_>>(),
        },
    };
    let front_matter = toml::to_string(&front_matter).with_whatever_context(|_| {
        format!(
            "unable to serialize generated hardforks index `{}`",
            index_path.to_string_lossy()
        )
    })?;

    file.write_all(b"+++\n").with_whatever_context(|_| {
        format!(
            "unable to write generated hardforks index `{}`",
            index_path.to_string_lossy()
        )
    })?;
    file.write_all(front_matter.as_bytes())
        .with_whatever_context(|_| {
            format!(
                "unable to write generated hardforks index `{}`",
                index_path.to_string_lossy()
            )
        })?;
    file.write_all(b"+++\n").with_whatever_context(|_| {
        format!(
            "unable to finish generated hardforks index `{}`",
            index_path.to_string_lossy()
        )
    })?;

    Ok(())
}

pub(crate) fn is_registered_network_upgrade_source_number(proposal_number: ProposalNumber) -> bool {
    transitional_modern_registry()
        .into_iter()
        .chain(permanent_registry())
        .any(|source| source.source_meta_eip == proposal_number.get())
}

pub(crate) fn markdown_has_network_upgrade_marker(contents: &str) -> bool {
    let Ok((preamble, _)) = Preamble::split(contents) else {
        return false;
    };
    let Ok(preamble) = Preamble::parse(None, preamble) else {
        return false;
    };

    preamble.by_name("network-upgrade").is_some()
}

pub(crate) fn transitional_modern_registry() -> Vec<NetworkUpgradeRegistrySource> {
    vec![
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Transitional,
            source_meta_eip: 7773,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Glamsterdam",
                slug: None,
                sort_order: None,
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Transitional,
            source_meta_eip: 8081,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Hegotá",
                slug: None,
                sort_order: None,
            }],
        },
    ]
}

pub(crate) fn permanent_registry() -> Vec<NetworkUpgradeRegistrySource> {
    vec![
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 606,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Homestead",
                slug: None,
                sort_order: Some(20160314),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 779,
            parser_mode: NetworkUpgradeParserMode::EmptyMembers,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "DAO Fork",
                slug: None,
                sort_order: Some(20160720),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 608,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Tangerine Whistle",
                slug: None,
                sort_order: Some(20161018),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 607,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Spurious Dragon",
                slug: None,
                sort_order: Some(20161122),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 609,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Byzantium",
                slug: None,
                sort_order: Some(20171016),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 1013,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Constantinople",
                slug: None,
                sort_order: Some(20190228),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 1716,
            parser_mode: NetworkUpgradeParserMode::EmptyMembers,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Petersburg",
                slug: None,
                sort_order: Some(20190228),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 1679,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Istanbul",
                slug: None,
                sort_order: Some(20191208),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 2387,
            parser_mode: NetworkUpgradeParserMode::LegacyIncludedList,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Muir Glacier",
                slug: None,
                sort_order: Some(20200102),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::EmptyMembers,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Beacon Chain Launch - Serenity Phase 0",
                slug: None,
                sort_order: Some(20201201),
            }],
        },
        // EIP-7568 points to execution-specs mainnet-upgrade files pinned at
        // 8dbde99b132ff8d8fcc9cfb015a9947ccc8b12d6. These curated
        // memberships mirror that pinned source family without runtime fetches.
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[
                2565, 2929, 2718, 2930,
            ]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Berlin",
                slug: None,
                sort_order: Some(20210415),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[
                1559, 3198, 3529, 3541, 3554,
            ]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "London",
                slug: None,
                sort_order: Some(20210805),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::EmptyMembers,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Altair",
                slug: None,
                sort_order: Some(20211027),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[4345]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Arrow Glacier",
                slug: None,
                sort_order: Some(20211209),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[5133]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Gray Glacier",
                slug: None,
                sort_order: Some(20220630),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[3675, 4399]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "The Merge",
                slug: None,
                sort_order: Some(20220915),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7568,
            parser_mode: NetworkUpgradeParserMode::ExplicitIncludedMembers(&[
                3651, 3855, 3860, 4895, 6049,
            ]),
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Shapella",
                slug: None,
                sort_order: Some(20230412),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7569,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Dencun",
                slug: None,
                sort_order: Some(20240313),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7600,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Pectra",
                slug: None,
                sort_order: Some(20250507),
            }],
        },
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 7607,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name: "Fusaka",
                slug: None,
                sort_order: Some(20251203),
            }],
        },
    ]
}

fn generated_hardfork(upgrade: &NetworkUpgrade) -> GeneratedHardfork {
    GeneratedHardfork {
        slug: upgrade.slug.clone(),
        display_name: upgrade.display_name.clone(),
        meta_eip: upgrade.source_meta_eip,
        meta_url: upgrade.meta_url.clone(),
        meta_status: upgrade.meta_status.clone(),
        stages: upgrade
            .stages
            .iter()
            .map(generated_hardfork_stage)
            .collect(),
    }
}

fn generated_hardfork_stage(stage: &NetworkUpgradeStage) -> GeneratedHardforkStage {
    GeneratedHardforkStage {
        key: stage.key.clone(),
        label: stage.label.clone(),
        rows: stage.rows.iter().map(generated_hardfork_row).collect(),
    }
}

fn generated_hardfork_row(row: &NetworkUpgradeMemberRow) -> GeneratedHardforkRow {
    GeneratedHardforkRow {
        number: row.number,
        title: row.title.clone(),
        status: row.status.clone(),
        proposal_type: row.proposal_type.clone(),
        url: row.url.clone(),
        category: row.category.clone(),
        subgroup: row.subgroup.clone(),
    }
}

fn marked_source_from_document(
    source_document: &ProposalCatalogSourceDocument,
    display_name: &str,
) -> Result<MarkedNetworkUpgradeSource, Whatever> {
    if display_name.trim().is_empty() {
        snafu::whatever!(
            "network-upgrade marker in `{}` must be non-empty",
            source_document.source_path().to_string_lossy()
        );
    }
    if source_document.proposal_type() != Some("Meta") {
        snafu::whatever!(
            "network-upgrade marker in `{}` is only allowed on `type: Meta` proposals",
            source_document.source_path().to_string_lossy()
        );
    }
    if source_document.prefix() != ProposalCatalogPrefix::Eip {
        snafu::whatever!(
            "network-upgrade marker in `{}` is only supported on EIP Meta proposals",
            source_document.source_path().to_string_lossy()
        );
    }

    let slug = network_upgrade_slug(display_name);
    if slug.is_empty() {
        snafu::whatever!(
            "network-upgrade marker `{display_name}` in `{}` does not derive a stable slug",
            source_document.source_path().to_string_lossy()
        );
    }

    Ok(MarkedNetworkUpgradeSource {
        source_meta_eip: source_document.number(),
        display_name: display_name.trim().to_owned(),
        slug,
    })
}

fn select_network_upgrade_sources(
    catalog: &ProposalCatalog,
    transitional_sources: &[NetworkUpgradeRegistrySource],
    permanent_sources: &[NetworkUpgradeRegistrySource],
) -> Result<Vec<SelectedNetworkUpgradeSource>, Whatever> {
    let marked_sources = collect_marked_modern_sources(catalog)?;
    let marked_source_numbers = marked_sources
        .iter()
        .map(|source| source.source_meta_eip)
        .collect::<BTreeSet<_>>();
    let permanent_source_numbers = permanent_sources
        .iter()
        .map(|source| registry_source_number(source.source_meta_eip))
        .collect::<Result<BTreeSet<_>, _>>()?;

    for marked_source in &marked_sources {
        if permanent_source_numbers.contains(&marked_source.source_meta_eip) {
            snafu::whatever!(
                "network-upgrade marker on permanent registry source `{}` is not allowed",
                marked_source.source_meta_eip
            );
        }
    }

    let mut selected_sources = Vec::new();
    for marked_source in marked_sources {
        let source_document = source_document_for_number(
            catalog,
            marked_source.source_meta_eip,
            "marked network upgrade source",
        )?;
        selected_sources.push(SelectedNetworkUpgradeSource {
            render_sort_key: created_render_sort_key(source_document)?,
            details: NetworkUpgradeSelectedSource {
                source_meta_eip: marked_source.source_meta_eip,
                source_bucket: NetworkUpgradeSourceBucket::Marked,
                display_name: marked_source.display_name,
                slug: marked_source.slug,
                parser_mode: NetworkUpgradeParserMode::ModernStages,
                sort_order: None,
            },
        });
    }

    for registry_source in transitional_sources {
        let source_meta_eip = registry_source_number(registry_source.source_meta_eip)?;
        if marked_source_numbers.contains(&source_meta_eip) {
            warn!(
                "network-upgrade marker on `{source_meta_eip}` shadows transitional registry entry"
            );
            continue;
        }
        push_registry_selected_sources(catalog, registry_source, &mut selected_sources)?;
    }

    for registry_source in permanent_sources {
        push_registry_selected_sources(catalog, registry_source, &mut selected_sources)?;
    }

    validate_sort_orders(&selected_sources)?;
    validate_slugs(&selected_sources)?;
    selected_sources.sort_by(|left, right| {
        left.render_sort_key
            .cmp(&right.render_sort_key)
            .then_with(|| left.details.slug.cmp(&right.details.slug))
    });

    Ok(selected_sources)
}

fn push_registry_selected_sources(
    catalog: &ProposalCatalog,
    registry_source: &NetworkUpgradeRegistrySource,
    selected_sources: &mut Vec<SelectedNetworkUpgradeSource>,
) -> Result<(), Whatever> {
    let source_meta_eip = registry_source_number(registry_source.source_meta_eip)?;
    let source_document = source_document_for_number(
        catalog,
        source_meta_eip,
        "registered network upgrade source",
    )?;
    let source_created_sort_key = created_render_sort_key(source_document)?;

    for registry_upgrade in &registry_source.upgrades {
        let slug = registry_upgrade
            .slug
            .map(str::to_owned)
            .unwrap_or_else(|| network_upgrade_slug(registry_upgrade.display_name));
        if slug.is_empty() {
            snafu::whatever!(
                "registered network upgrade `{}` does not derive a stable slug",
                registry_upgrade.display_name
            );
        }

        selected_sources.push(SelectedNetworkUpgradeSource {
            render_sort_key: registry_upgrade
                .sort_order
                .map(|sort_order| RenderSortKey {
                    value: i64::from(sort_order),
                    source_meta_eip,
                })
                .unwrap_or(source_created_sort_key),
            details: NetworkUpgradeSelectedSource {
                source_meta_eip,
                source_bucket: registry_source.bucket,
                display_name: registry_upgrade.display_name.to_owned(),
                slug,
                parser_mode: registry_source.parser_mode,
                sort_order: registry_upgrade.sort_order,
            },
        });
    }

    Ok(())
}

fn build_network_upgrade(
    catalog: &ProposalCatalog,
    selected_source: &NetworkUpgradeSelectedSource,
    meta_record: &ProposalCatalogRecord,
    parsed_stages: Vec<ParsedNetworkUpgradeStage>,
) -> Result<NetworkUpgrade, Whatever> {
    let mut seen_members = BTreeMap::<ProposalNumber, NetworkUpgradeStageKey>::new();
    let stages = parsed_stages
        .into_iter()
        .map(|parsed_stage| {
            let rows = parsed_stage
                .members
                .into_iter()
                .map(|member| {
                    if let Some(existing_stage) =
                        seen_members.insert(member.number, parsed_stage.key)
                    {
                        snafu::whatever!(
                            "network upgrade `{}` lists proposal `{}` more than once, in `{}` and `{}`",
                            selected_source.display_name,
                            member.number,
                            existing_stage,
                            parsed_stage.key
                        );
                    }

                    member_row(catalog, &selected_source.display_name, member)
                })
                .collect::<Result<Vec<_>, Whatever>>()?;

            Ok(NetworkUpgradeStage {
                key: parsed_stage.key.stable_key().to_owned(),
                label: parsed_stage.key.to_string(),
                rows,
            })
        })
        .collect::<Result<Vec<_>, Whatever>>()?;

    Ok(NetworkUpgrade {
        display_name: selected_source.display_name.clone(),
        slug: selected_source.slug.clone(),
        source_meta_eip: selected_source.source_meta_eip,
        source_bucket: selected_source.source_bucket,
        meta_url: meta_record.url.clone(),
        meta_status: meta_record.status.clone(),
        stages,
    })
}

fn member_row(
    catalog: &ProposalCatalog,
    display_name: &str,
    member: ParsedNetworkUpgradeMember,
) -> Result<NetworkUpgradeMemberRow, Whatever> {
    let record = proposal_record_for_number(catalog, member.number, display_name)?;
    Ok(NetworkUpgradeMemberRow {
        number: record.number,
        title: record.title.clone(),
        status: record.status.clone(),
        proposal_type: record.proposal_type.clone(),
        category: if record.proposal_type == "Standards Track" {
            record.category.clone()
        } else {
            None
        },
        url: record.url.clone(),
        subgroup: member.subgroup,
    })
}

fn parse_modern_stage_members(
    source_document: &ProposalCatalogSourceDocument,
) -> Result<Vec<ParsedNetworkUpgradeStage>, Whatever> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut stages = BTreeMap::<NetworkUpgradeStageKey, ParsedNetworkUpgradeStage>::new();
    let mut active_stage = None::<ActiveStage>;
    let mut heading_capture = None::<HeadingCapture>;
    let mut link_capture = None::<LinkCapture>;

    for event in Parser::new_ext(source_document.body(), options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading_capture = Some(HeadingCapture {
                    level,
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                let heading = heading_capture
                    .take()
                    .whatever_context("heading end without heading start")?;
                handle_heading(source_document, heading, &mut active_stage, &mut stages)?;
            }
            Event::Text(text) | Event::Code(text) => {
                push_cow_text(&mut heading_capture, &mut link_capture, text);
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = &mut heading_capture {
                    heading.text.push(' ');
                }
                if let Some(link) = &mut link_capture {
                    link.text.push(' ');
                }
            }
            Event::Start(Tag::Link { dest_url, .. })
                if active_stage.is_some() && heading_capture.is_none() =>
            {
                link_capture = Some(LinkCapture {
                    dest_url: dest_url.into_string(),
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Link) => {
                let Some(link) = link_capture.take() else {
                    continue;
                };
                let Some(active_stage) = &active_stage else {
                    continue;
                };
                let Some(proposal_number) = proposal_number_from_link(&link.dest_url, &link.text)
                else {
                    continue;
                };
                let stage = stages
                    .get_mut(&active_stage.key)
                    .with_whatever_context(|| {
                        format!(
                            "recognized network upgrade stage `{}` was not initialized in `{}`",
                            active_stage.key,
                            source_document.source_path().to_string_lossy()
                        )
                    })?;
                stage.members.push(ParsedNetworkUpgradeMember {
                    number: proposal_number,
                    subgroup: active_stage.subgroup.clone(),
                });
            }
            _ => {}
        }
    }

    let mut stages = stages.into_values().collect::<Vec<_>>();
    stages.sort_by_key(|stage| stage.key.render_rank());
    Ok(stages)
}

fn parse_legacy_included_members(
    source_document: &ProposalCatalogSourceDocument,
) -> Result<Vec<ParsedNetworkUpgradeStage>, Whatever> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let mut members = Vec::new();
    let mut saw_included_section = false;
    let mut active_heading_level = None::<u8>;
    let mut heading_capture = None::<HeadingCapture>;
    let mut link_capture = None::<LinkCapture>;
    let mut list_depth = 0usize;
    let mut included_list_depth = None::<usize>;
    let mut item_text_stack = Vec::<String>::new();

    for event in Parser::new_ext(source_document.body(), options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading_capture = Some(HeadingCapture {
                    level,
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                let heading = heading_capture
                    .take()
                    .whatever_context("heading end without heading start")?;
                let heading_level = heading_level_number(heading.level);
                if normalize_stage_heading(heading.text.trim())
                    == Some(NetworkUpgradeStageKey::Included)
                {
                    saw_included_section = true;
                    active_heading_level = Some(heading_level);
                } else if active_heading_level.is_some_and(|level| heading_level <= level) {
                    active_heading_level = None;
                }
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
                if included_list_depth.is_none()
                    && item_text_stack
                        .last()
                        .is_some_and(|text| legacy_included_list_label(text))
                {
                    saw_included_section = true;
                    included_list_depth = Some(list_depth);
                }
            }
            Event::End(TagEnd::List(_)) => {
                if included_list_depth == Some(list_depth) {
                    included_list_depth = None;
                }
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                item_text_stack.push(String::new());
            }
            Event::End(TagEnd::Item) => {
                item_text_stack.pop();
            }
            Event::Text(text) | Event::Code(text) => {
                push_cow_text(&mut heading_capture, &mut link_capture, text.clone());
                if let Some(item_text) = item_text_stack.last_mut() {
                    item_text.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = &mut heading_capture {
                    heading.text.push(' ');
                }
                if let Some(link) = &mut link_capture {
                    link.text.push(' ');
                }
                if let Some(item_text) = item_text_stack.last_mut() {
                    item_text.push(' ');
                }
            }
            Event::Start(Tag::Link { dest_url, .. })
                if heading_capture.is_none()
                    && (active_heading_level.is_some() || included_list_depth.is_some()) =>
            {
                link_capture = Some(LinkCapture {
                    dest_url: dest_url.into_string(),
                    text: String::new(),
                });
            }
            Event::End(TagEnd::Link) => {
                let Some(link) = link_capture.take() else {
                    continue;
                };
                let Some(proposal_number) = proposal_number_from_link(&link.dest_url, &link.text)
                else {
                    continue;
                };
                members.push(ParsedNetworkUpgradeMember {
                    number: proposal_number,
                    subgroup: None,
                });
            }
            _ => {}
        }
    }

    // LegacyIncludedList is stricter than ModernStages because historical
    // registered sources are expected to contain parseable member lists. True
    // empty hardforks must use EmptyMembers instead.
    if !saw_included_section {
        snafu::whatever!(
            "legacy included-list parsing failed for `{}` (EIP-{}): no Included EIPs section found",
            source_document.source_path().to_string_lossy(),
            source_document.number()
        );
    }
    if members.is_empty() {
        snafu::whatever!(
            "legacy included-list parsing failed for `{}` (EIP-{}): Included EIPs section did not contain proposal links",
            source_document.source_path().to_string_lossy(),
            source_document.number()
        );
    }

    Ok(vec![ParsedNetworkUpgradeStage {
        key: NetworkUpgradeStageKey::Included,
        members,
    }])
}

fn explicit_included_members(members: &[u32]) -> Result<Vec<ParsedNetworkUpgradeStage>, Whatever> {
    let members = members
        .iter()
        .map(|number| {
            let number = match ProposalNumber::from_u32(*number) {
                Ok(number) => number,
                Err(()) => {
                    snafu::whatever!("network upgrade explicit member `{number}` must be positive");
                }
            };
            Ok(ParsedNetworkUpgradeMember {
                number,
                subgroup: None,
            })
        })
        .collect::<Result<Vec<_>, Whatever>>()?;

    Ok(vec![ParsedNetworkUpgradeStage {
        key: NetworkUpgradeStageKey::Included,
        members,
    }])
}

fn legacy_included_list_label(text: &str) -> bool {
    normalize_stage_heading(text.trim().trim_end_matches(':'))
        == Some(NetworkUpgradeStageKey::Included)
}

fn handle_heading(
    source_document: &ProposalCatalogSourceDocument,
    heading: HeadingCapture,
    active_stage: &mut Option<ActiveStage>,
    stages: &mut BTreeMap<NetworkUpgradeStageKey, ParsedNetworkUpgradeStage>,
) -> Result<(), Whatever> {
    let heading_text = heading.text.trim();
    let heading_level = heading_level_number(heading.level);
    if let Some(stage_key) = normalize_stage_heading(heading_text) {
        if stages.contains_key(&stage_key) {
            snafu::whatever!(
                "duplicate network upgrade stage heading `{}` in `{}`",
                stage_key,
                source_document.source_path().to_string_lossy()
            );
        }
        stages.insert(
            stage_key,
            ParsedNetworkUpgradeStage {
                key: stage_key,
                members: Vec::new(),
            },
        );
        *active_stage = Some(ActiveStage {
            key: stage_key,
            level: heading_level,
            subgroup: None,
        });
    } else if let Some(stage) = active_stage {
        if heading_level <= stage.level {
            *active_stage = None;
        } else if !heading_text.is_empty() {
            stage.subgroup = Some(heading_text.to_owned());
        }
    }

    Ok(())
}

fn push_cow_text(
    heading_capture: &mut Option<HeadingCapture>,
    link_capture: &mut Option<LinkCapture>,
    text: CowStr<'_>,
) {
    if let Some(heading) = heading_capture {
        heading.text.push_str(&text);
    }
    if let Some(link) = link_capture {
        link.text.push_str(&text);
    }
}

fn normalize_stage_heading(heading: &str) -> Option<NetworkUpgradeStageKey> {
    let mut normalized = heading.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_prefix("eips ") {
        normalized = stripped.to_owned();
    }
    let lower = normalized.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix(" eips") {
        normalized = stripped.to_owned();
    }
    let lower = normalized.to_ascii_lowercase();
    let normalized = lower
        .strip_suffix(" for inclusion")
        .unwrap_or(&lower)
        .trim();

    match normalized {
        "included" => Some(NetworkUpgradeStageKey::Included),
        "scheduled" => Some(NetworkUpgradeStageKey::Scheduled),
        "considered" => Some(NetworkUpgradeStageKey::Considered),
        "proposed" => Some(NetworkUpgradeStageKey::Proposed),
        "declined" => Some(NetworkUpgradeStageKey::Declined),
        _ => None,
    }
}

fn proposal_number_from_link(dest_url: &str, link_text: &str) -> Option<ProposalNumber> {
    proposal_number_from_markdown_path(dest_url).or_else(|| proposal_number_from_text(link_text))
}

fn proposal_number_from_markdown_path(dest_url: &str) -> Option<ProposalNumber> {
    let path = dest_url.split(['?', '#']).next()?.trim();
    if path.contains("://") || path.starts_with("mailto:") {
        return None;
    }

    let path = Path::new(path.trim_start_matches('/'));
    if path.file_name()? == "index.md" {
        return path_component_proposal_number(path.parent()?.file_name());
    }

    flat_proposal_number(path)
}

fn proposal_number_from_text(link_text: &str) -> Option<ProposalNumber> {
    lazy_static! {
        static ref PROPOSAL_TEXT_RE: Regex = Regex::new(r"(?i)\b(?:eip|erc)-?([0-9]+)\b").unwrap();
    }

    let captures = PROPOSAL_TEXT_RE.captures(link_text)?;
    captures
        .get(1)?
        .as_str()
        .parse::<u32>()
        .ok()
        .and_then(|number| ProposalNumber::from_u32(number).ok())
}

fn proposal_record_for_number<'a>(
    catalog: &'a ProposalCatalog,
    proposal_number: ProposalNumber,
    context: &str,
) -> Result<&'a ProposalCatalogRecord, Whatever> {
    catalog
        .records()
        .get(&ProposalCatalogPrefix::Eip.key(proposal_number))
        .with_whatever_context(|| {
            format!("network upgrade `{context}` references missing proposal `{proposal_number}`")
        })
}

fn source_document_for_number<'a>(
    catalog: &'a ProposalCatalog,
    proposal_number: ProposalNumber,
    context: &str,
) -> Result<&'a ProposalCatalogSourceDocument, Whatever> {
    catalog
        .source_document(ProposalCatalogPrefix::Eip, proposal_number)
        .with_whatever_context(|| {
            format!("network upgrade `{context}` source proposal `{proposal_number}` was not found")
        })
}

fn registry_source_number(number: u32) -> Result<ProposalNumber, Whatever> {
    match ProposalNumber::from_u32(number) {
        Ok(number) => Ok(number),
        Err(()) => {
            snafu::whatever!("network upgrade registry source number `{number}` must be positive");
        }
    }
}

fn created_render_sort_key(
    source_document: &ProposalCatalogSourceDocument,
) -> Result<RenderSortKey, Whatever> {
    let created = source_document.created().with_whatever_context(|| {
        format!(
            "network upgrade source `{}` is missing `created`",
            source_document.number()
        )
    })?;
    let date = NaiveDate::parse_from_str(created, "%Y-%m-%d").with_whatever_context(|_| {
        format!(
            "network upgrade source `{}` has invalid `created` date `{created}`",
            source_document.number()
        )
    })?;

    Ok(RenderSortKey {
        value: i64::from(date.year()) * 10_000
            + i64::from(date.month()) * 100
            + i64::from(date.day()),
        source_meta_eip: source_document.number(),
    })
}

fn validate_sort_orders(selected_sources: &[SelectedNetworkUpgradeSource]) -> Result<(), Whatever> {
    let mut sort_keys = BTreeMap::<RenderSortKey, &str>::new();
    for selected_source in selected_sources {
        if selected_source.details.sort_order.is_none() {
            continue;
        }
        if let Some(existing_name) = sort_keys.insert(
            selected_source.render_sort_key,
            &selected_source.details.display_name,
        ) {
            snafu::whatever!(
                "network upgrade render sort key `{}:{}` is used by both `{existing_name}` and `{}`",
                selected_source.render_sort_key.value,
                selected_source.render_sort_key.source_meta_eip,
                selected_source.details.display_name
            );
        }
    }

    Ok(())
}

fn validate_slugs(selected_sources: &[SelectedNetworkUpgradeSource]) -> Result<(), Whatever> {
    let mut slugs = BTreeMap::<&str, &str>::new();
    for selected_source in selected_sources {
        if let Some(existing_name) = slugs.insert(
            selected_source.details.slug.as_str(),
            selected_source.details.display_name.as_str(),
        ) {
            snafu::whatever!(
                "network upgrade slug collision `{}` between `{existing_name}` and `{}`",
                selected_source.details.slug,
                selected_source.details.display_name
            );
        }
    }

    Ok(())
}

fn heading_level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn network_upgrade_slug(display_name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for character in display_name.chars().flat_map(char::to_lowercase) {
        if let Some(ascii) = slug_ascii_alphanumeric(character) {
            slug.push(ascii);
            last_was_separator = false;
        } else if !slug.is_empty() && !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }

    if last_was_separator {
        slug.pop();
    }

    slug
}

fn slug_ascii_alphanumeric(character: char) -> Option<char> {
    if character.is_ascii_alphanumeric() {
        return Some(character);
    }

    // This intentionally small fold covers current hardfork names without
    // adding a new direct dependency. Broader Unicode slugging should replace
    // it if source names expand beyond simple Latin diacritics.
    match character {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => Some('a'),
        'ç' | 'ć' | 'č' => Some('c'),
        'ď' => Some('d'),
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' => Some('e'),
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => Some('i'),
        'ñ' | 'ń' => Some('n'),
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => Some('o'),
        'ŕ' | 'ř' => Some('r'),
        'ś' | 'š' => Some('s'),
        'ť' => Some('t'),
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => Some('u'),
        'ý' | 'ÿ' => Some('y'),
        'ź' | 'ż' | 'ž' => Some('z'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use tempfile::TempDir;
    use toml::Value as TomlValue;

    use super::{
        collect_marked_modern_sources, collect_network_upgrades_with_registries,
        network_upgrade_slug, normalize_stage_heading, permanent_registry,
        transitional_modern_registry, write_hardforks_index, NetworkUpgradeIndex,
        NetworkUpgradeParserMode, NetworkUpgradeRegistrySource, NetworkUpgradeRegistryUpgrade,
        NetworkUpgradeSourceBucket, NetworkUpgradeStageKey,
    };
    use crate::{
        proposal::{OnlyRenderPlan, ProposalNumber},
        proposal_catalog::{collect_proposal_catalog, ProposalCatalog},
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

    fn proposal_markdown(
        number: u32,
        title: &str,
        status: &str,
        proposal_type: &str,
        category: Option<&str>,
        extra_preamble: &str,
        body: &str,
    ) -> String {
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: {title}\nstatus: {status}\ntype: {proposal_type}\n{category}created: 2024-01-01\n{extra_preamble}---\n\n{body}\n"
        )
    }

    fn meta_markdown(number: u32, title: &str, created: &str, extra: &str, body: &str) -> String {
        format!(
            "---\neip: {number}\ntitle: {title}\nstatus: Draft\ntype: Meta\ncreated: {created}\n{extra}---\n\n{body}\n"
        )
    }

    fn catalog(files: &[(&str, String)]) -> ProposalCatalog {
        let temp = TempDir::new().unwrap();
        for (relative, contents) in files {
            write_file(temp.path(), relative, contents);
        }
        collect_proposal_catalog(temp.path(), None).unwrap()
    }

    fn front_matter_from_generated_hardforks_index(content_root: &Path) -> TomlValue {
        let contents = std::fs::read_to_string(content_root.join("hardforks/_index.md")).unwrap();
        let front_matter = contents
            .strip_prefix("+++\n")
            .unwrap()
            .split_once("\n+++\n")
            .unwrap()
            .0;
        toml::from_str(front_matter).unwrap()
    }

    fn transitional_source(
        number: u32,
        display_name: &'static str,
    ) -> NetworkUpgradeRegistrySource {
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Transitional,
            source_meta_eip: number,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name,
                slug: None,
                sort_order: None,
            }],
        }
    }

    fn permanent_source(
        number: u32,
        display_name: &'static str,
        sort_order: Option<i32>,
    ) -> NetworkUpgradeRegistrySource {
        permanent_source_with_mode(
            number,
            display_name,
            sort_order,
            NetworkUpgradeParserMode::ModernStages,
        )
    }

    fn permanent_source_with_mode(
        number: u32,
        display_name: &'static str,
        sort_order: Option<i32>,
        parser_mode: NetworkUpgradeParserMode,
    ) -> NetworkUpgradeRegistrySource {
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: number,
            parser_mode,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name,
                slug: None,
                sort_order,
            }],
        }
    }

    fn legacy_source(
        number: u32,
        display_name: &'static str,
        sort_order: Option<i32>,
    ) -> NetworkUpgradeRegistrySource {
        permanent_source_with_mode(
            number,
            display_name,
            sort_order,
            NetworkUpgradeParserMode::LegacyIncludedList,
        )
    }

    fn empty_source(
        number: u32,
        display_name: &'static str,
        sort_order: Option<i32>,
    ) -> NetworkUpgradeRegistrySource {
        permanent_source_with_mode(
            number,
            display_name,
            sort_order,
            NetworkUpgradeParserMode::EmptyMembers,
        )
    }

    fn explicit_source(
        number: u32,
        display_name: &'static str,
        sort_order: Option<i32>,
        members: &'static [u32],
    ) -> NetworkUpgradeRegistrySource {
        permanent_source_with_mode(
            number,
            display_name,
            sort_order,
            NetworkUpgradeParserMode::ExplicitIncludedMembers(members),
        )
    }

    #[test]
    fn slug_generation_is_deterministic_and_ascii_folds_known_names() {
        assert_eq!(network_upgrade_slug("Hegotá"), "hegota");
        assert_eq!(
            network_upgrade_slug("Hegotá"),
            network_upgrade_slug("hegota")
        );
        assert_eq!(
            network_upgrade_slug("  Prague / Electra  "),
            "prague-electra"
        );
    }

    #[test]
    fn heading_normalization_supports_known_variants() {
        assert_eq!(
            normalize_stage_heading("EIPs Scheduled for Inclusion"),
            Some(NetworkUpgradeStageKey::Scheduled)
        );
        assert_eq!(
            normalize_stage_heading("Included EIPs"),
            Some(NetworkUpgradeStageKey::Included)
        );
        assert_eq!(
            normalize_stage_heading("EIPs Included"),
            Some(NetworkUpgradeStageKey::Included)
        );
        assert_eq!(
            normalize_stage_heading("Proposed"),
            Some(NetworkUpgradeStageKey::Proposed)
        );
        assert_eq!(
            normalize_stage_heading("Declined for Inclusion"),
            Some(NetworkUpgradeStageKey::Declined)
        );
    }

    #[test]
    fn active_stage_parsing_handles_glamsterdam_and_hegota_shapes() {
        let glamsterdam = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### EIPs Scheduled for Inclusion\n\n* [EIP-7732](./07732.md)\n\n### Considered for Inclusion\n\n* [EIP-2780](./02780.md)\n\n### Declined for Inclusion\n\n* [EIP-2926](./02926.md)\n\n### Proposed for Inclusion\n\n* [EIP-7610](./07610.md)\n",
        );
        let hegota = meta_markdown(
            8081,
            "Hardfork Meta - Hegotá",
            "2025-11-11",
            "",
            "### EIPs Scheduled for Inclusion\n\n### Considered for Inclusion\n\n* [EIP-7805](./07805.md)\n\n### Declined for Inclusion\n\n### Proposed for Inclusion\n",
        );
        let catalog = catalog(&[
            ("07773.md", glamsterdam),
            ("08081.md", hegota),
            (
                "07732.md",
                proposal_markdown(7732, "EIP 7732", "Draft", "Standards Track", None, "", ""),
            ),
            (
                "02780.md",
                proposal_markdown(2780, "EIP 2780", "Draft", "Standards Track", None, "", ""),
            ),
            (
                "02926.md",
                proposal_markdown(2926, "EIP 2926", "Draft", "Standards Track", None, "", ""),
            ),
            (
                "07610.md",
                proposal_markdown(7610, "EIP 7610", "Draft", "Standards Track", None, "", ""),
            ),
            (
                "07805.md",
                proposal_markdown(7805, "EIP 7805", "Draft", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[
                transitional_source(7773, "Glamsterdam"),
                transitional_source(8081, "Hegotá"),
            ],
            &[],
        )
        .unwrap();

        let glamsterdam = index
            .upgrades
            .iter()
            .find(|upgrade| upgrade.slug == "glamsterdam")
            .unwrap();
        assert_eq!(
            glamsterdam
                .stages
                .iter()
                .map(|stage| stage.key.as_str())
                .collect::<Vec<_>>(),
            ["scheduled", "considered", "proposed", "declined"]
        );
        assert_eq!(glamsterdam.stages[0].rows[0].number, number(7732));
        assert_eq!(glamsterdam.stages[3].rows[0].number, number(2926));

        let hegota = index
            .upgrades
            .iter()
            .find(|upgrade| upgrade.slug == "hegota")
            .unwrap();
        assert!(hegota.stages[0].rows.is_empty());
        assert_eq!(hegota.stages[1].rows[0].number, number(7805));
    }

    #[test]
    fn final_included_parsing_covers_recent_permanent_hardfork_shapes() {
        let dencun = meta_markdown(
            7569,
            "Hardfork Meta - Dencun",
            "2023-12-01",
            "",
            "### Included EIPs\n\n* [EIP-1153](./01153.md)\n",
        );
        let pectra = meta_markdown(
            7600,
            "Hardfork Meta - Pectra",
            "2024-01-18",
            "",
            "### Included EIPs\n\n#### Core EIPs\n\n* [EIP-2537](./02537.md)\n\n#### Other EIPs\n\n* [EIP-7840](./07840.md)\n",
        );
        let fusaka = meta_markdown(
            7607,
            "Hardfork Meta - Fusaka",
            "2024-02-01",
            "",
            "### Included EIPs\n\n#### Core EIPs\n\n* [EIP-7594](./07594.md)\n\n#### Other EIPs\n\n* [EIP-7892](./07892.md)\n",
        );
        let catalog = catalog(&[
            ("07569.md", dencun),
            ("07600.md", pectra),
            ("07607.md", fusaka),
            (
                "01153.md",
                proposal_markdown(
                    1153,
                    "Transient storage",
                    "Final",
                    "Standards Track",
                    Some("Core"),
                    "",
                    "",
                ),
            ),
            (
                "02537.md",
                proposal_markdown(
                    2537,
                    "BLS precompile",
                    "Final",
                    "Standards Track",
                    Some("Core"),
                    "",
                    "",
                ),
            ),
            (
                "07594.md",
                proposal_markdown(
                    7594,
                    "PeerDAS",
                    "Final",
                    "Standards Track",
                    Some("Core"),
                    "",
                    "",
                ),
            ),
            (
                "07840.md",
                proposal_markdown(
                    7840,
                    "Blob schedule",
                    "Review",
                    "Informational",
                    None,
                    "",
                    "",
                ),
            ),
            (
                "07892.md",
                proposal_markdown(7892, "Blob hardforks", "Final", "Meta", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                permanent_source(7569, "Dencun", Some(20240313)),
                permanent_source(7600, "Pectra", Some(20250507)),
                permanent_source(7607, "Fusaka", Some(20251203)),
            ],
        )
        .unwrap();
        let dencun = index
            .upgrades
            .iter()
            .find(|upgrade| upgrade.slug == "dencun")
            .unwrap();
        let pectra = index
            .upgrades
            .iter()
            .find(|upgrade| upgrade.slug == "pectra")
            .unwrap();
        let fusaka = index
            .upgrades
            .iter()
            .find(|upgrade| upgrade.slug == "fusaka")
            .unwrap();
        let pectra_rows = &pectra.stages[0].rows;
        let fusaka_rows = &fusaka.stages[0].rows;

        assert_eq!(dencun.stages[0].key, "included");
        assert_eq!(dencun.stages[0].rows[0].subgroup, None);
        assert_eq!(pectra.stages[0].key, "included");
        assert_eq!(pectra_rows[0].subgroup.as_deref(), Some("Core EIPs"));
        assert_eq!(pectra_rows[0].category.as_deref(), Some("Core"));
        assert_eq!(pectra_rows[1].subgroup.as_deref(), Some("Other EIPs"));
        assert_eq!(pectra_rows[1].category, None);
        assert_eq!(fusaka_rows[0].subgroup.as_deref(), Some("Core EIPs"));
        assert_eq!(fusaka_rows[1].subgroup.as_deref(), Some("Other EIPs"));
    }

    #[test]
    fn legacy_included_list_parser_supports_body_list_shape() {
        let homestead = meta_markdown(
            606,
            "Hardfork Meta: Homestead",
            "2017-04-23",
            "requires: 2, 7, 8\n",
            "## Specification\n\n- Codename: Homestead\n- Included EIPs:\n  - [EIP-2](./00002.md)\n  - [EIP-7](./00007.md)\n  - [EIP-8](./00008.md)\n\n## References\n\nSee [EIP-999](./00999.md).\n",
        );
        let catalog = catalog(&[
            ("00606.md", homestead),
            (
                "00002.md",
                proposal_markdown(2, "EIP 2", "Final", "Standards Track", None, "", ""),
            ),
            (
                "00007.md",
                proposal_markdown(7, "EIP 7", "Final", "Standards Track", None, "", ""),
            ),
            (
                "00008.md",
                proposal_markdown(8, "EIP 8", "Final", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[legacy_source(606, "Homestead", Some(20160314))],
        )
        .unwrap();

        assert_eq!(
            index.upgrades[0].stages[0]
                .rows
                .iter()
                .map(|row| row.number)
                .collect::<Vec<_>>(),
            [number(2), number(7), number(8)]
        );
    }

    #[test]
    fn legacy_included_list_parser_supports_heading_shape_and_stops_later() {
        let istanbul = meta_markdown(
            1679,
            "Hardfork Meta: Istanbul",
            "2019-01-04",
            "",
            "## Specification\n\n### Included EIPs\n\n- [EIP-152](./00152.md)\n- [EIP-1108](./01108.md)\n\n### References\n\nSee [EIP-1716](./01716.md).\n",
        );
        let catalog = catalog(&[
            ("01679.md", istanbul),
            (
                "00152.md",
                proposal_markdown(152, "EIP 152", "Final", "Standards Track", None, "", ""),
            ),
            (
                "01108.md",
                proposal_markdown(1108, "EIP 1108", "Final", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[legacy_source(1679, "Istanbul", Some(20191208))],
        )
        .unwrap();

        assert_eq!(
            index.upgrades[0].stages[0]
                .rows
                .iter()
                .map(|row| row.number)
                .collect::<Vec<_>>(),
            [number(152), number(1108)]
        );
    }

    #[test]
    fn legacy_included_list_parser_errors_without_included_section() {
        let source = meta_markdown(
            606,
            "Hardfork Meta: Homestead",
            "2017-04-23",
            "",
            "## Specification\n\nSee [EIP-2](./00002.md).\n",
        );
        let catalog = catalog(&[("00606.md", source)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[legacy_source(606, "Homestead", Some(20160314))],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("legacy included-list parsing failed"));
        assert!(error.contains("00606.md"));
        assert!(error.contains("no Included EIPs section found"));
    }

    #[test]
    fn legacy_included_list_parser_errors_when_included_section_has_no_links() {
        let source = meta_markdown(
            1679,
            "Hardfork Meta: Istanbul",
            "2019-01-04",
            "",
            "## Specification\n\n### Included EIPs\n\nNo proposal links here.\n",
        );
        let catalog = catalog(&[("01679.md", source)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[legacy_source(1679, "Istanbul", Some(20191208))],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("legacy included-list parsing failed"));
        assert!(error.contains("01679.md"));
        assert!(error.contains("did not contain proposal links"));
    }

    #[test]
    fn legacy_included_list_parser_ignores_requires_membership() {
        let tangerine = meta_markdown(
            608,
            "Hardfork Meta: Tangerine Whistle",
            "2017-04-23",
            "requires: 150, 779\n",
            "- Included EIPs:\n  - [EIP-150](./00150.md)\n",
        );
        let byzantium = meta_markdown(
            609,
            "Hardfork Meta: Byzantium",
            "2017-04-23",
            "requires: 100, 607\n",
            "- Included EIPs:\n  - [EIP-100](./00100.md)\n",
        );
        let istanbul = meta_markdown(
            1679,
            "Hardfork Meta: Istanbul",
            "2019-01-04",
            "requires: 152, 1716\n",
            "### Included EIPs\n\n- [EIP-152](./00152.md)\n",
        );
        let catalog = catalog(&[
            ("00608.md", tangerine),
            ("00609.md", byzantium),
            ("01679.md", istanbul),
            (
                "00150.md",
                proposal_markdown(150, "EIP 150", "Final", "Standards Track", None, "", ""),
            ),
            (
                "00100.md",
                proposal_markdown(100, "EIP 100", "Final", "Standards Track", None, "", ""),
            ),
            (
                "00152.md",
                proposal_markdown(152, "EIP 152", "Final", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                legacy_source(608, "Tangerine Whistle", Some(20161018)),
                legacy_source(609, "Byzantium", Some(20171016)),
                legacy_source(1679, "Istanbul", Some(20191208)),
            ],
        )
        .unwrap();

        let rows = index
            .upgrades
            .iter()
            .map(|upgrade| {
                (
                    upgrade.display_name.as_str(),
                    upgrade.stages[0].rows[0].number,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                ("Tangerine Whistle", number(150)),
                ("Byzantium", number(100)),
                ("Istanbul", number(152))
            ]
        );
    }

    #[test]
    fn empty_member_hardforks_emit_zero_stages() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "00779.md",
            &meta_markdown(779, "Hardfork Meta: DAO Fork", "2017-11-26", "", ""),
        );
        write_file(
            temp.path(),
            "01716.md",
            &meta_markdown(1716, "Hardfork Meta: Petersburg", "2019-01-21", "", ""),
        );
        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                empty_source(779, "DAO Fork", Some(20160720)),
                empty_source(1716, "Petersburg", Some(20190228)),
            ],
        )
        .unwrap();

        write_hardforks_index(temp.path(), &index).unwrap();

        let front_matter = front_matter_from_generated_hardforks_index(temp.path());
        let upgrades = front_matter["extra"]["network_upgrades"]
            .as_array()
            .unwrap();
        assert_eq!(upgrades[0]["display_name"].as_str().unwrap(), "DAO Fork");
        assert_eq!(upgrades[1]["display_name"].as_str().unwrap(), "Petersburg");
        assert!(upgrades[0]["stages"].as_array().unwrap().is_empty());
        assert!(upgrades[1]["stages"].as_array().unwrap().is_empty());
    }

    #[test]
    fn eip_7568_backfill_entries_are_distinct_and_curated() {
        let explicit_sources = permanent_registry()
            .into_iter()
            .filter(|source| source.source_meta_eip == 7568)
            .collect::<Vec<_>>();
        let mut files = vec![(
            "07568.md",
            meta_markdown(
                7568,
                "Hardfork Meta Backfill - Berlin to Shapella",
                "2023-12-01",
                "",
                "",
            ),
        )];
        for member in [
            2565, 2929, 2718, 2930, 1559, 3198, 3529, 3541, 3554, 4345, 5133, 3675, 4399, 3651,
            3855, 3860, 4895,
        ] {
            files.push((
                Box::leak(format!("{member:05}.md").into_boxed_str()),
                proposal_markdown(
                    member,
                    &format!("EIP {member}"),
                    "Final",
                    "Standards Track",
                    Some("Core"),
                    "",
                    "",
                ),
            ));
        }
        files.push((
            "06049.md",
            proposal_markdown(6049, "EIP 6049", "Final", "Meta", None, "", ""),
        ));
        let catalog = catalog(&files);

        let index =
            collect_network_upgrades_with_registries(&catalog, &[], &explicit_sources).unwrap();

        assert_eq!(
            index
                .upgrades
                .iter()
                .map(|upgrade| upgrade.display_name.as_str())
                .collect::<Vec<_>>(),
            [
                "Beacon Chain Launch - Serenity Phase 0",
                "Berlin",
                "London",
                "Altair",
                "Arrow Glacier",
                "Gray Glacier",
                "The Merge",
                "Shapella"
            ]
        );
        assert_eq!(
            index.upgrades[0].slug,
            "beacon-chain-launch-serenity-phase-0"
        );
        assert_eq!(index.upgrades[6].slug, "the-merge");
        assert_eq!(index.upgrades[7].slug, "shapella");
        assert!(index.upgrades[0].stages.is_empty());
        assert!(index.upgrades[3].stages.is_empty());

        let berlin_members = &index.upgrades[1].stages[0].rows;
        assert_eq!(
            berlin_members
                .iter()
                .map(|row| row.number)
                .collect::<Vec<_>>(),
            [number(2565), number(2929), number(2718), number(2930)]
        );
        let merge_members = &index.upgrades[6].stages[0].rows;
        assert_eq!(
            merge_members
                .iter()
                .map(|row| row.number)
                .collect::<Vec<_>>(),
            [number(3675), number(4399)]
        );
        assert!(!merge_members.iter().any(|row| row.number == number(2124)));
        let shapella_members = &index.upgrades[7].stages[0].rows;
        assert!(shapella_members
            .iter()
            .any(|row| row.number == number(6049)));
        assert!(index.upgrades.iter().all(|upgrade| {
            upgrade.display_name != "Bellatrix" && upgrade.display_name != "Capella"
        }));
        assert!(
            index
                .upgrades
                .iter()
                .filter(|upgrade| {
                    upgrade
                        .stages
                        .first()
                        .is_some_and(|stage| stage.key == "included" && stage.label == "Included")
                })
                .count()
                >= 6
        );
    }

    #[test]
    fn missing_curated_member_references_error_with_upgrade_name() {
        let source = meta_markdown(7568, "Backfill", "2023-12-01", "", "");
        let catalog = catalog(&[("07568.md", source)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[explicit_source(7568, "Berlin", Some(20210415), &[2565])],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("network upgrade `Berlin`"));
        assert!(error.contains("references missing proposal `2565`"));
    }

    #[test]
    fn hardfork_index_writer_emits_section_front_matter_contract() {
        let temp = TempDir::new().unwrap();
        let source = meta_markdown(
            7600,
            "Hardfork Meta - Pectra",
            "2024-01-18",
            "",
            "### Included EIPs\n\n#### Core EIPs\n\n* [EIP-2](./00002.md)\n\n#### Other EIPs\n\n* [EIP-3](./00003.md)\n",
        );
        write_file(temp.path(), "07600.md", &source);
        write_file(
            temp.path(),
            "00002.md",
            &proposal_markdown(
                2,
                "Core proposal",
                "Final",
                "Standards Track",
                Some("Core"),
                "",
                "",
            ),
        );
        write_file(
            temp.path(),
            "00003.md",
            &proposal_markdown(3, "Meta proposal", "Review", "Meta", None, "", ""),
        );
        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[permanent_source(7600, "Pectra", Some(20250507))],
        )
        .unwrap();

        write_hardforks_index(temp.path(), &index).unwrap();

        let contents = std::fs::read_to_string(temp.path().join("hardforks/_index.md")).unwrap();
        let front_matter = front_matter_from_generated_hardforks_index(temp.path());
        let upgrades = front_matter["extra"]["network_upgrades"]
            .as_array()
            .unwrap();
        let upgrade = &upgrades[0];
        let stage = &upgrade["stages"].as_array().unwrap()[0];
        let rows = stage["rows"].as_array().unwrap();

        assert_eq!(front_matter["title"].as_str().unwrap(), "Hardforks");
        assert_eq!(front_matter["template"].as_str().unwrap(), "hardforks.html");
        assert_eq!(upgrade["slug"].as_str().unwrap(), "pectra");
        assert_eq!(upgrade["display_name"].as_str().unwrap(), "Pectra");
        assert_eq!(upgrade["meta_eip"].as_integer().unwrap(), 7600);
        assert_eq!(upgrade["meta_url"].as_str().unwrap(), "/7600/");
        assert_eq!(upgrade["meta_status"].as_str().unwrap(), "Draft");
        assert_eq!(stage["key"].as_str().unwrap(), "included");
        assert_eq!(stage["label"].as_str().unwrap(), "Included");
        assert_eq!(rows[0]["number"].as_integer().unwrap(), 2);
        assert_eq!(rows[0]["title"].as_str().unwrap(), "Core proposal");
        assert_eq!(rows[0]["status"].as_str().unwrap(), "Final");
        assert_eq!(rows[0]["type"].as_str().unwrap(), "Standards Track");
        assert_eq!(rows[0]["url"].as_str().unwrap(), "/2/");
        assert_eq!(rows[0]["category"].as_str().unwrap(), "Core");
        assert_eq!(rows[0]["subgroup"].as_str().unwrap(), "Core EIPs");
        assert_eq!(rows[1]["type"].as_str().unwrap(), "Meta");
        assert!(rows[1].get("category").is_none());
        assert_eq!(rows[1]["subgroup"].as_str().unwrap(), "Other EIPs");
        assert!(!contents.contains("sort_order"));
        assert!(!contents.contains("source_bucket"));
        assert!(!contents.contains("parser_mode"));
        assert!(!contents.contains("ExplicitIncludedMembers"));
        assert!(!contents.contains("LegacyIncludedList"));
        assert!(!contents.contains("source_meta_eip"));
    }

    #[test]
    fn hardfork_index_writer_preserves_empty_stage_order_and_labels() {
        let temp = TempDir::new().unwrap();
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### EIPs Scheduled for Inclusion\n\n### Considered for Inclusion\n\n### Proposed for Inclusion\n\n### Declined for Inclusion\n",
        );
        write_file(temp.path(), "07773.md", &source);
        let catalog = collect_proposal_catalog(temp.path(), None).unwrap();
        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap();

        write_hardforks_index(temp.path(), &index).unwrap();

        let front_matter = front_matter_from_generated_hardforks_index(temp.path());
        let stages = front_matter["extra"]["network_upgrades"][0]["stages"]
            .as_array()
            .unwrap();
        assert_eq!(
            stages
                .iter()
                .map(|stage| stage["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["scheduled", "considered", "proposed", "declined"]
        );
        assert_eq!(
            stages[0]["label"].as_str().unwrap(),
            "Scheduled for Inclusion"
        );
        assert_eq!(
            stages[1]["label"].as_str().unwrap(),
            "Considered for Inclusion"
        );
        assert_eq!(
            stages[2]["label"].as_str().unwrap(),
            "Proposed for Inclusion"
        );
        assert_eq!(
            stages[3]["label"].as_str().unwrap(),
            "Declined for Inclusion"
        );
    }

    #[test]
    fn hardfork_index_writer_preserves_targeted_public_urls() {
        let temp = TempDir::new().unwrap();
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### Included EIPs\n\n* [EIP-2](./00002.md)\n",
        );
        write_file(
            temp.path(),
            "00001.md",
            &proposal_markdown(1, "Selected", "Draft", "Meta", None, "", ""),
        );
        write_file(temp.path(), "07773.md", &source);
        write_file(
            temp.path(),
            "00002.md",
            &proposal_markdown(2, "Omitted", "Draft", "Standards Track", None, "", ""),
        );
        let plan = OnlyRenderPlan::build(temp.path(), [number(1)].into_iter().collect()).unwrap();
        let catalog = collect_proposal_catalog(temp.path(), Some(&plan)).unwrap();
        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap();

        write_hardforks_index(temp.path(), &index).unwrap();

        let front_matter = front_matter_from_generated_hardforks_index(temp.path());
        let upgrade = &front_matter["extra"]["network_upgrades"][0];
        let row = &upgrade["stages"].as_array().unwrap()[0]["rows"]
            .as_array()
            .unwrap()[0];
        assert_eq!(
            upgrade["meta_url"].as_str().unwrap(),
            "https://eips.ethereum.org/EIPS/eip-7773"
        );
        assert_eq!(
            row["url"].as_str().unwrap(),
            "https://eips.ethereum.org/EIPS/eip-2"
        );
    }

    #[test]
    fn hardfork_index_writer_refuses_existing_index() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "hardforks/_index.md", "existing\n");
        let index = NetworkUpgradeIndex {
            upgrades: Vec::new(),
            memberships_by_proposal: BTreeMap::new(),
            selected_sources: Vec::new(),
        };

        let error = write_hardforks_index(temp.path(), &index)
            .unwrap_err()
            .to_string();

        assert!(error.contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join("hardforks/_index.md")).unwrap(),
            "existing\n"
        );
    }

    #[test]
    fn duplicate_stage_headings_error() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### Included EIPs\n\n### EIPs Included\n",
        );
        let catalog = catalog(&[("07773.md", source)]);
        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("duplicate network upgrade stage heading"));
        assert!(error.contains("Included"));
    }

    #[test]
    fn links_outside_recognized_stage_sections_are_ignored() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "See [EIP-1](./00001.md).\n\n### Included EIPs\n\n* [EIP-2](./00002.md)\n\n### Activation\n\nSee [EIP-3](./00003.md).\n",
        );
        let catalog = catalog(&[
            ("07773.md", source),
            (
                "00002.md",
                proposal_markdown(2, "EIP 2", "Draft", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap();

        assert_eq!(index.upgrades[0].stages[0].rows.len(), 1);
        assert_eq!(index.upgrades[0].stages[0].rows[0].number, number(2));
    }

    #[test]
    fn no_markers_still_selects_transitional_registry_sources() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### Included EIPs\n",
        );
        let catalog = catalog(&[("07773.md", source)]);

        assert!(collect_marked_modern_sources(&catalog).unwrap().is_empty());
        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap();

        assert_eq!(
            index.selected_sources[0].source_bucket,
            NetworkUpgradeSourceBucket::Transitional
        );
    }

    #[test]
    fn marked_sources_shadow_transitional_entries_by_source_number() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "network-upgrade: Glamsterdam\n",
            "### Included EIPs\n",
        );
        let catalog = catalog(&[("07773.md", source)]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Different Name")],
            &[],
        )
        .unwrap();

        assert_eq!(index.upgrades.len(), 1);
        assert_eq!(
            index.selected_sources[0].source_bucket,
            NetworkUpgradeSourceBucket::Marked
        );
        assert_eq!(index.upgrades[0].display_name, "Glamsterdam");
    }

    #[test]
    fn invalid_markers_fail_preprocessor_validation() {
        let empty_marker = catalog(&[(
            "00001.md",
            meta_markdown(1, "Meta", "2024-01-01", "network-upgrade: \n", ""),
        )]);
        let error = collect_marked_modern_sources(&empty_marker)
            .unwrap_err()
            .to_string();
        assert!(error.contains("must be non-empty"));

        let non_meta = catalog(&[(
            "00002.md",
            proposal_markdown(
                2,
                "Not Meta",
                "Draft",
                "Standards Track",
                None,
                "network-upgrade: Bad\n",
                "",
            ),
        )]);
        let error = collect_marked_modern_sources(&non_meta)
            .unwrap_err()
            .to_string();
        assert!(error.contains("only allowed on `type: Meta`"));

        let erc_meta = catalog(&[(
            "00004.md",
            proposal_markdown(
                4,
                "ERC Meta",
                "Draft",
                "Meta",
                Some("ERC"),
                "network-upgrade: Foo\n",
                "",
            ),
        )]);
        let error = collect_marked_modern_sources(&erc_meta)
            .unwrap_err()
            .to_string();
        assert!(error.contains("only supported on EIP Meta proposals"));

        let empty_slug = catalog(&[(
            "00003.md",
            meta_markdown(3, "Meta", "2024-01-01", "network-upgrade: !!!\n", ""),
        )]);
        let error = collect_marked_modern_sources(&empty_slug)
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not derive a stable slug"));
    }

    #[test]
    fn marker_on_permanent_registry_source_errors() {
        let source = meta_markdown(
            7569,
            "Hardfork Meta - Dencun",
            "2023-12-01",
            "network-upgrade: Dencun\n",
            "### Included EIPs\n",
        );
        let catalog = catalog(&[("07569.md", source)]);
        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[permanent_source(7569, "Dencun", Some(20240313))],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("permanent registry source"));
    }

    #[test]
    fn stage_like_unregistered_meta_eips_are_not_collected() {
        let source = meta_markdown(
            8007,
            "Glamsterdam Gas Repricings",
            "2025-08-21",
            "",
            "### Considered for Inclusion\n\n* [EIP-2780](./02780.md)\n",
        );
        let catalog = catalog(&[
            ("08007.md", source),
            (
                "02780.md",
                proposal_markdown(2780, "EIP 2780", "Draft", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(&catalog, &[], &[]).unwrap();

        assert!(index.upgrades.is_empty());
    }

    #[test]
    fn duplicate_membership_within_same_hardfork_errors() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### Scheduled for Inclusion\n\n* [EIP-1](./00001.md)\n\n### Declined for Inclusion\n\n* [EIP-1](./00001.md)\n",
        );
        let catalog = catalog(&[
            ("07773.md", source),
            (
                "00001.md",
                proposal_markdown(1, "EIP 1", "Draft", "Standards Track", None, "", ""),
            ),
        ]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("lists proposal `1` more than once"));
    }

    #[test]
    fn cross_hardfork_membership_is_allowed() {
        let first = meta_markdown(
            100,
            "First",
            "2024-01-01",
            "",
            "### Included EIPs\n\n* [EIP-1](./00001.md)\n",
        );
        let second = meta_markdown(
            200,
            "Second",
            "2024-02-01",
            "",
            "### Included EIPs\n\n* [EIP-1](./00001.md)\n",
        );
        let catalog = catalog(&[
            ("00100.md", first),
            ("00200.md", second),
            (
                "00001.md",
                proposal_markdown(1, "EIP 1", "Draft", "Standards Track", None, "", ""),
            ),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[
                transitional_source(100, "First"),
                transitional_source(200, "Second"),
            ],
            &[],
        )
        .unwrap();

        assert_eq!(index.memberships_by_proposal[&number(1)].len(), 2);
    }

    #[test]
    fn missing_member_references_error() {
        let source = meta_markdown(
            7773,
            "Hardfork Meta - Glamsterdam",
            "2024-09-26",
            "",
            "### Included EIPs\n\n* [EIP-1](./00001.md)\n",
        );
        let catalog = catalog(&[("07773.md", source)]);
        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[transitional_source(7773, "Glamsterdam")],
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("references missing proposal `1`"));
    }

    #[test]
    fn transitional_and_permanent_registries_are_distinct_buckets() {
        assert!(transitional_modern_registry().iter().all(|source| {
            source.bucket == NetworkUpgradeSourceBucket::Transitional
                && source.parser_mode == NetworkUpgradeParserMode::ModernStages
        }));
        assert!(permanent_registry()
            .iter()
            .all(|source| source.bucket == NetworkUpgradeSourceBucket::Permanent));
        assert_eq!(
            transitional_modern_registry()
                .iter()
                .map(|source| source.source_meta_eip)
                .collect::<Vec<_>>(),
            [7773, 8081]
        );
        assert_eq!(
            permanent_registry()
                .iter()
                .map(|source| source.source_meta_eip)
                .collect::<Vec<_>>(),
            [
                606, 779, 608, 607, 609, 1013, 1716, 1679, 2387, 7568, 7568, 7568, 7568, 7568,
                7568, 7568, 7568, 7569, 7600, 7607
            ]
        );
    }

    #[test]
    fn bpo_withdrawn_stagnant_and_process_sources_are_not_registered_by_default() {
        let permanent_source_numbers = permanent_registry()
            .iter()
            .map(|source| source.source_meta_eip)
            .collect::<Vec<_>>();

        for excluded in [8134, 7892, 233, 1588, 7675, 7692, 2070] {
            assert!(!permanent_source_numbers.contains(&excluded));
        }
    }

    #[test]
    fn slug_collisions_are_detected() {
        let first = meta_markdown(100, "First", "2024-01-01", "", "### Included EIPs\n");
        let second = meta_markdown(200, "Second", "2024-02-01", "", "### Included EIPs\n");
        let catalog = catalog(&[("00100.md", first), ("00200.md", second)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[
                transitional_source(100, "Same Name"),
                transitional_source(200, "Same-Name"),
            ],
            &[],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("slug collision `same-name`"));
    }

    #[test]
    fn transitional_entries_sort_by_created_date_then_meta_number() {
        let early = meta_markdown(200, "Early", "2024-01-01", "", "### Included EIPs\n");
        let tie_low = meta_markdown(100, "Tie Low", "2024-02-01", "", "### Included EIPs\n");
        let tie_high = meta_markdown(300, "Tie High", "2024-02-01", "", "### Included EIPs\n");
        let catalog = catalog(&[
            ("00200.md", early),
            ("00100.md", tie_low),
            ("00300.md", tie_high),
        ]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[
                transitional_source(300, "Tie High"),
                transitional_source(200, "Early"),
                transitional_source(100, "Tie Low"),
            ],
            &[],
        )
        .unwrap();

        assert_eq!(
            index
                .upgrades
                .iter()
                .map(|upgrade| upgrade.source_meta_eip)
                .collect::<Vec<_>>(),
            [number(200), number(100), number(300)]
        );
    }

    #[test]
    fn permanent_sort_order_overrides_render_order() {
        let first = meta_markdown(100, "First", "2024-01-01", "", "### Included EIPs\n");
        let second = meta_markdown(200, "Second", "2024-02-01", "", "### Included EIPs\n");
        let catalog = catalog(&[("00100.md", first), ("00200.md", second)]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                permanent_source(100, "First", Some(20)),
                permanent_source(200, "Second", Some(10)),
            ],
        )
        .unwrap();

        assert_eq!(
            index
                .upgrades
                .iter()
                .map(|upgrade| upgrade.display_name.as_str())
                .collect::<Vec<_>>(),
            ["Second", "First"]
        );
    }

    #[test]
    fn duplicate_render_sort_keys_error_clearly() {
        let first = meta_markdown(100, "First", "2024-01-01", "", "### Included EIPs\n");
        let catalog = catalog(&[("00100.md", first)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                permanent_source(100, "First", Some(10)),
                permanent_source(100, "Second", Some(10)),
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("render sort key `10:100`"));
        assert!(error.contains("First"));
        assert!(error.contains("Second"));
    }

    #[test]
    fn shared_sort_dates_use_source_number_as_tie_breaker() {
        let constantinople = meta_markdown(
            1013,
            "Constantinople",
            "2018-04-20",
            "",
            "### Included EIPs\n",
        );
        let petersburg = meta_markdown(1716, "Petersburg", "2019-01-21", "", "");
        let catalog = catalog(&[("01013.md", constantinople), ("01716.md", petersburg)]);

        let index = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                empty_source(1716, "Petersburg", Some(20190228)),
                permanent_source(1013, "Constantinople", Some(20190228)),
            ],
        )
        .unwrap();

        assert_eq!(
            index
                .upgrades
                .iter()
                .map(|upgrade| upgrade.display_name.as_str())
                .collect::<Vec<_>>(),
            ["Constantinople", "Petersburg"]
        );
    }

    #[test]
    fn permanent_registry_chronology_is_deterministic() {
        let mut entries = permanent_registry()
            .into_iter()
            .flat_map(|source| {
                source.upgrades.into_iter().map(move |upgrade| {
                    (
                        upgrade.display_name,
                        upgrade.sort_order.unwrap(),
                        source.source_meta_eip,
                    )
                })
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, sort_order, source_meta_eip)| (*sort_order, *source_meta_eip));
        let names = entries
            .iter()
            .map(|(display_name, _, _)| *display_name)
            .collect::<Vec<_>>();
        let position = |name: &str| names.iter().position(|entry| *entry == name).unwrap();

        assert!(position("Homestead") < position("DAO Fork"));
        assert!(position("DAO Fork") < position("Tangerine Whistle"));
        assert!(position("Constantinople") < position("Petersburg"));
        assert!(position("Muir Glacier") < position("Beacon Chain Launch - Serenity Phase 0"));
        assert!(position("Shapella") < position("Dencun"));
        assert!(position("Dencun") < position("Pectra"));
        assert!(position("Pectra") < position("Fusaka"));
    }

    #[test]
    fn permanent_registry_shape_supports_one_source_to_many_and_empty_members() {
        let source = meta_markdown(100, "Backfill", "2024-01-01", "", "");
        let catalog = catalog(&[("00100.md", source)]);
        let registry_source = NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: 100,
            parser_mode: NetworkUpgradeParserMode::EmptyMembers,
            upgrades: vec![
                NetworkUpgradeRegistryUpgrade {
                    display_name: "First Empty",
                    slug: Some("first-empty"),
                    sort_order: Some(1),
                },
                NetworkUpgradeRegistryUpgrade {
                    display_name: "Second Empty",
                    slug: Some("second-empty"),
                    sort_order: Some(2),
                },
            ],
        };

        let index =
            collect_network_upgrades_with_registries(&catalog, &[], &[registry_source]).unwrap();

        assert_eq!(index.upgrades.len(), 2);
        assert!(index
            .upgrades
            .iter()
            .all(|upgrade| upgrade.stages.is_empty()));
    }
}
