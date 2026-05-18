/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Heading-section search corpus chunk extraction.

use std::collections::HashMap;

use scraper::ElementRef;

use super::{
    schema::{ChunkRecord, DocumentRecord},
    splitter::{self, SourceBlock, SPLITTER_VERSION},
};

#[derive(Debug)]
struct ChunkBuilder {
    heading_slug: String,
    fragment: Option<String>,
    heading: String,
    heading_path: Vec<String>,
    ordinal: usize,
    blocks: Vec<SourceBlock>,
}

pub(super) fn extract_chunks(body: ElementRef<'_>, document: &DocumentRecord) -> Vec<ChunkRecord> {
    let mut chunks = Vec::new();
    let mut current: Option<ChunkBuilder> = None;
    let mut heading_path: Vec<(u8, String)> = Vec::new();
    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    for node in body.descendants() {
        if let Some(element) = ElementRef::wrap(node) {
            if element_has_pagefind_ignore(element, false) || element_is_script_or_style(element) {
                continue;
            }
            if is_section_heading(element) {
                chunks.extend(finish_chunk(current.take(), document));
                let level = heading_level(element).expect("section heading has a level");
                while heading_path
                    .last()
                    .is_some_and(|(path_level, _)| *path_level >= level)
                {
                    heading_path.pop();
                }
                let heading = collect_text(element, false);
                heading_path.push((level, heading.clone()));
                let fragment = element.attr("id").map(str::to_owned);
                let heading_slug = fragment
                    .clone()
                    .unwrap_or_else(|| slugify_heading(&heading));
                let ordinal = slug_counts.entry(heading_slug.clone()).or_insert(0);
                let chunk_ordinal = *ordinal;
                *ordinal += 1;
                current = Some(ChunkBuilder {
                    heading_slug,
                    fragment,
                    heading,
                    heading_path: heading_path
                        .iter()
                        .map(|(_, heading)| heading.clone())
                        .collect(),
                    ordinal: chunk_ordinal,
                    blocks: Vec::new(),
                });
            } else if let Some(chunk) = current.as_mut() {
                if let Some(block) = splitter::source_block_for_element(element) {
                    chunk.blocks.push(block);
                }
            }
        }
    }

    chunks.extend(finish_chunk(current, document));

    chunks
}

fn finish_chunk(chunk: Option<ChunkBuilder>, document: &DocumentRecord) -> Vec<ChunkRecord> {
    let Some(chunk) = chunk else {
        return Vec::new();
    };
    if splitter::section_text(&chunk.blocks).is_empty() {
        return Vec::new();
    }

    let url = chunk
        .fragment
        .as_ref()
        .map(|fragment| format!("{}#{fragment}", document.url))
        .unwrap_or_else(|| document.url.clone());
    let section_id = format!("{}#{}:{}", document.id, chunk.heading_slug, chunk.ordinal);
    let parts = splitter::split_blocks(&chunk.blocks);
    let part_count = parts.len();

    parts
        .into_iter()
        .enumerate()
        .map(|(part_index, part)| ChunkRecord {
            id: format!("{section_id}:part:{part_index}"),
            section_id: section_id.clone(),
            document_id: document.id.clone(),
            kind: "chunk",
            url: url.clone(),
            title: document.title.clone(),
            heading: chunk.heading.clone(),
            heading_path: chunk.heading_path.clone(),
            ordinal: chunk.ordinal,
            part_index,
            part_count,
            block_kind: part.block_kind.to_string(),
            splitter_version: SPLITTER_VERSION,
            split_from_oversized_block: part.split_from_oversized_block,
            text: part.text,
            filters: document.filters.clone(),
            meta: document.meta.clone(),
            sort: document.sort.clone(),
        })
        .collect()
}

pub(super) fn collect_text(element: ElementRef<'_>, all_ignore_only: bool) -> String {
    let mut segments = Vec::new();

    for node in element.descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        let ignored = node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| {
                element_is_script_or_style(ancestor)
                    || element_has_pagefind_ignore(ancestor, all_ignore_only)
            });
        if ignored {
            continue;
        }
        if let Some(segment) = normalize_segment(text) {
            segments.push(segment);
        }
    }

    normalize_joined_segments(&segments)
}

pub(super) fn element_has_pagefind_ignore(element: ElementRef<'_>, all_ignore_only: bool) -> bool {
    element.attr("data-pagefind-ignore").is_some_and(|value| {
        if all_ignore_only {
            value == "all"
        } else {
            true
        }
    }) || element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            ancestor.attr("data-pagefind-ignore").is_some_and(|value| {
                if all_ignore_only {
                    value == "all"
                } else {
                    true
                }
            })
        })
}

fn element_is_script_or_style(element: ElementRef<'_>) -> bool {
    matches!(element.value().name(), "script" | "style")
}

fn is_section_heading(element: ElementRef<'_>) -> bool {
    matches!(element.value().name(), "h2" | "h3" | "h4")
}

fn heading_level(element: ElementRef<'_>) -> Option<u8> {
    match element.value().name() {
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        _ => None,
    }
}

fn normalize_segment(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_joined_segments(segments: &[String]) -> String {
    segments
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn slugify_heading(heading: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;

    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            previous_dash = false;
        } else if !previous_dash && !slug.is_empty() {
            slug.push('-');
            previous_dash = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "section".to_owned()
    } else {
        slug
    }
}
