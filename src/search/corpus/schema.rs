/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Search corpus record schema.

use std::collections::BTreeMap;

use serde::Serialize;

pub(super) const CORPUS_VERSION: u8 = 1;

#[derive(Debug, Serialize)]
pub(super) struct CorpusOutput {
    pub(super) version: u8,
    pub(super) documents: Vec<DocumentRecord>,
    pub(super) chunks: Vec<ChunkRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct DocumentRecord {
    pub(super) id: String,
    pub(super) kind: &'static str,
    pub(super) url: String,
    pub(super) title: String,
    pub(super) text: String,
    pub(super) filters: BTreeMap<String, Vec<String>>,
    pub(super) meta: BTreeMap<String, String>,
    pub(super) sort: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ChunkRecord {
    pub(super) id: String,
    pub(super) section_id: String,
    pub(super) document_id: String,
    pub(super) kind: &'static str,
    pub(super) url: String,
    pub(super) title: String,
    pub(super) heading: String,
    pub(super) heading_path: Vec<String>,
    pub(super) ordinal: usize,
    pub(super) part_index: usize,
    pub(super) part_count: usize,
    pub(super) block_kind: String,
    pub(super) splitter_version: u8,
    pub(super) split_from_oversized_block: bool,
    pub(super) text: String,
    pub(super) filters: BTreeMap<String, Vec<String>>,
    pub(super) meta: BTreeMap<String, String>,
    pub(super) sort: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct PagefindData {
    pub(super) filters: BTreeMap<String, Vec<String>>,
    pub(super) meta: BTreeMap<String, String>,
    pub(super) sort: BTreeMap<String, String>,
}
