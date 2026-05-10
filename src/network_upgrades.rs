/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Network upgrade source selection and in-memory membership model.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
};

use chrono::{Datelike, NaiveDate};
use lazy_static::lazy_static;
use log::warn;
use pulldown_cmark::{CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;
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
    let mut sort_orders = BTreeMap::<i32, &str>::new();
    for selected_source in selected_sources {
        let Some(sort_order) = selected_source.details.sort_order else {
            continue;
        };
        if let Some(existing_name) =
            sort_orders.insert(sort_order, &selected_source.details.display_name)
        {
            snafu::whatever!(
                "network upgrade sort_order `{sort_order}` is used by both `{existing_name}` and `{}`",
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
    use std::path::Path;

    use tempfile::TempDir;

    use super::{
        collect_marked_modern_sources, collect_network_upgrades_with_registries,
        network_upgrade_slug, normalize_stage_heading, permanent_registry,
        transitional_modern_registry, NetworkUpgradeParserMode, NetworkUpgradeRegistrySource,
        NetworkUpgradeRegistryUpgrade, NetworkUpgradeSourceBucket, NetworkUpgradeStageKey,
    };
    use crate::{
        proposal::ProposalNumber,
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
        NetworkUpgradeRegistrySource {
            bucket: NetworkUpgradeSourceBucket::Permanent,
            source_meta_eip: number,
            parser_mode: NetworkUpgradeParserMode::ModernStages,
            upgrades: vec![NetworkUpgradeRegistryUpgrade {
                display_name,
                slug: None,
                sort_order,
            }],
        }
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
            [7569, 7600, 7607]
        );
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
    fn sort_order_collisions_error_clearly() {
        let first = meta_markdown(100, "First", "2024-01-01", "", "### Included EIPs\n");
        let second = meta_markdown(200, "Second", "2024-02-01", "", "### Included EIPs\n");
        let catalog = catalog(&[("00100.md", first), ("00200.md", second)]);

        let error = collect_network_upgrades_with_registries(
            &catalog,
            &[],
            &[
                permanent_source(100, "First", Some(10)),
                permanent_source(200, "Second", Some(10)),
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("sort_order `10`"));
        assert!(error.contains("First"));
        assert!(error.contains("Second"));
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
