/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Block-aware search corpus chunk splitting.

use std::{collections::BTreeMap, fmt, sync::LazyLock};

use regex::Regex;
use scraper::ElementRef;

pub(super) const SPLITTER_VERSION: u8 = 1;
pub(super) const TARGET_CHUNK_CHARS: usize = 1_600;
pub(super) const MAX_CHUNK_CHARS: usize = 2_000;

const MIN_TRAILING_CHARS: usize = TARGET_CHUNK_CHARS / 4;

static HEX_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]{16,}").expect("valid hex token regex"));
static SENTENCE_BOUNDARY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?]\s+").expect("valid sentence boundary regex"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum BlockKind {
    Prose,
    Code,
    Table,
    Hex,
    Mixed,
}

impl fmt::Display for BlockKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prose => write!(f, "prose"),
            Self::Code => write!(f, "code"),
            Self::Table => write!(f, "table"),
            Self::Hex => write!(f, "hex"),
            Self::Mixed => write!(f, "mixed"),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SourceBlock {
    kind: BlockKind,
    raw_text: String,
    normalized_text: String,
}

impl SourceBlock {
    fn new(base_kind: BlockKind, raw_text: String) -> Option<Self> {
        let normalized_text = normalize_text(&raw_text);
        if normalized_text.is_empty() {
            return None;
        }
        let kind = if is_hex_blob_text(&normalized_text) {
            BlockKind::Hex
        } else {
            base_kind
        };

        Some(Self {
            kind,
            raw_text,
            normalized_text,
        })
    }

    fn len(&self) -> usize {
        text_len(&self.normalized_text)
    }
}

#[derive(Debug, Clone)]
pub(super) struct SplitPart {
    pub(super) text: String,
    pub(super) block_kind: BlockKind,
    pub(super) split_from_oversized_block: bool,
}

#[derive(Debug, Clone)]
struct Piece {
    text: String,
    kind: BlockKind,
    split_from_oversized_block: bool,
}

impl Piece {
    fn new(text: String, kind: BlockKind, split_from_oversized_block: bool) -> Option<Self> {
        let text = normalize_text(&text);
        if text.is_empty() {
            return None;
        }
        debug_assert!(text_len(&text) <= MAX_CHUNK_CHARS);
        Some(Self {
            text,
            kind,
            split_from_oversized_block,
        })
    }

    fn len(&self) -> usize {
        text_len(&self.text)
    }
}

#[derive(Debug, Clone, Default)]
struct PartBuilder {
    pieces: Vec<Piece>,
}

impl PartBuilder {
    fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    fn len(&self) -> usize {
        joined_len(self.pieces.iter().map(Piece::len))
    }

    fn len_with(&self, piece: &Piece) -> usize {
        joined_len(self.pieces.iter().map(Piece::len).chain([piece.len()]))
    }

    fn push(&mut self, piece: Piece) {
        self.pieces.push(piece);
    }

    fn pop(&mut self) -> Option<Piece> {
        self.pieces.pop()
    }

    fn insert_front(&mut self, piece: Piece) {
        self.pieces.insert(0, piece);
    }

    fn into_part(self) -> SplitPart {
        let text = normalize_text(
            &self
                .pieces
                .iter()
                .map(|piece| piece.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
        debug_assert!(text_len(&text) <= MAX_CHUNK_CHARS);
        let split_from_oversized_block = self
            .pieces
            .iter()
            .any(|piece| piece.split_from_oversized_block);
        let block_kind = emitted_block_kind(&self.pieces);

        SplitPart {
            text,
            block_kind,
            split_from_oversized_block,
        }
    }
}

pub(super) fn split_blocks(blocks: &[SourceBlock]) -> Vec<SplitPart> {
    let pieces = blocks.iter().flat_map(split_block).collect::<Vec<Piece>>();
    assemble_pieces(pieces)
}

pub(super) fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn section_text(blocks: &[SourceBlock]) -> String {
    normalize_text(
        &blocks
            .iter()
            .map(|block| block.normalized_text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub(super) fn source_block_for_element(element: ElementRef<'_>) -> Option<SourceBlock> {
    if has_block_owner_ancestor(element) {
        return None;
    }

    match element.value().name() {
        "p" => SourceBlock::new(
            BlockKind::Prose,
            collect_descendant_text(element, TextMode::Prose),
        ),
        "li" => SourceBlock::new(
            BlockKind::Prose,
            collect_descendant_text(element, TextMode::Prose),
        ),
        "pre" => SourceBlock::new(
            BlockKind::Code,
            collect_descendant_text(element, TextMode::Preformatted),
        ),
        "tr" => SourceBlock::new(BlockKind::Table, collect_table_row_text(element)),
        "code" if is_standalone_code_block(element) => SourceBlock::new(
            BlockKind::Code,
            collect_descendant_text(element, TextMode::Preformatted),
        ),
        "div" | "section" | "article" | "blockquote" => {
            SourceBlock::new(BlockKind::Prose, collect_direct_text(element))
        }
        _ => None,
    }
}

pub(super) fn is_hex_blob_character(character: char) -> bool {
    character.is_ascii_hexdigit()
        || matches!(
            character,
            'x' | 'X' | ',' | ':' | '_' | '-' | '[' | ']' | '(' | ')'
        )
}

pub(super) fn is_hex_blob_text(normalized_text: &str) -> bool {
    if text_len(normalized_text) < 512 {
        return false;
    }
    if HEX_TOKEN_RE.find_iter(normalized_text).take(8).count() >= 8 {
        return true;
    }

    let mut non_whitespace = 0usize;
    let mut hex_blob = 0usize;
    for character in normalized_text.chars() {
        if character.is_whitespace() {
            continue;
        }
        non_whitespace += 1;
        if is_hex_blob_character(character) {
            hex_blob += 1;
        }
    }

    non_whitespace > 0 && hex_blob * 100 >= non_whitespace * 70
}

fn split_block(block: &SourceBlock) -> Vec<Piece> {
    if block.len() <= MAX_CHUNK_CHARS {
        return Piece::new(block.normalized_text.clone(), block.kind, false)
            .into_iter()
            .collect();
    }

    let split = match block.kind {
        BlockKind::Prose => split_prose_block(block),
        BlockKind::Code => split_code_block(block),
        BlockKind::Table => split_table_block(block),
        BlockKind::Hex => split_hard(&block.normalized_text, block.kind),
        BlockKind::Mixed => unreachable!("source blocks are never mixed"),
    };

    split
        .into_iter()
        .map(|piece| Piece {
            split_from_oversized_block: true,
            ..piece
        })
        .collect()
}

fn split_prose_block(block: &SourceBlock) -> Vec<Piece> {
    split_on_sentence_boundaries(&block.raw_text, block.kind)
        .or_else(|| split_on_whitespace(&block.raw_text, block.kind))
        .unwrap_or_else(|| split_hard(&block.normalized_text, block.kind))
}

fn split_code_block(block: &SourceBlock) -> Vec<Piece> {
    split_on_lines(&block.raw_text, block.kind)
        .or_else(|| split_on_whitespace(&block.raw_text, block.kind))
        .unwrap_or_else(|| split_hard(&block.normalized_text, block.kind))
}

fn split_table_block(block: &SourceBlock) -> Vec<Piece> {
    split_on_lines(&block.raw_text, block.kind)
        .or_else(|| split_on_whitespace(&block.raw_text, block.kind))
        .unwrap_or_else(|| split_hard(&block.normalized_text, block.kind))
}

fn split_on_sentence_boundaries(raw_text: &str, kind: BlockKind) -> Option<Vec<Piece>> {
    let mut start = 0usize;
    let mut units = Vec::new();
    for boundary in SENTENCE_BOUNDARY_RE.find_iter(raw_text) {
        let end = boundary.end();
        units.push(raw_text[start..end].to_owned());
        start = end;
    }
    if start < raw_text.len() {
        units.push(raw_text[start..].to_owned());
    }
    group_boundary_units(units, " ", kind)
}

fn split_on_lines(raw_text: &str, kind: BlockKind) -> Option<Vec<Piece>> {
    let units = raw_text.lines().map(str::to_owned).collect::<Vec<_>>();
    group_boundary_units(units, "\n", kind)
}

fn split_on_whitespace(raw_text: &str, kind: BlockKind) -> Option<Vec<Piece>> {
    let units = raw_text
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    group_boundary_units(units, " ", kind)
}

fn group_boundary_units(
    units: Vec<String>,
    separator: &str,
    kind: BlockKind,
) -> Option<Vec<Piece>> {
    let units = units
        .into_iter()
        .filter(|unit| !normalize_text(unit).is_empty())
        .collect::<Vec<_>>();
    if units.len() <= 1 {
        return None;
    }
    if units
        .iter()
        .any(|unit| text_len(&normalize_text(unit)) > MAX_CHUNK_CHARS)
    {
        return None;
    }

    let mut grouped = Vec::new();
    let mut current = String::new();

    for unit in units {
        let candidate = if current.is_empty() {
            unit.clone()
        } else {
            format!("{current}{separator}{unit}")
        };
        if !current.is_empty() && text_len(&normalize_text(&candidate)) > MAX_CHUNK_CHARS {
            grouped.push(Piece::new(current, kind, false).expect("non-empty grouped text"));
            current = unit;
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        grouped.push(Piece::new(current, kind, false).expect("non-empty grouped text"));
    }

    (!grouped.is_empty()).then_some(grouped)
}

fn split_hard(normalized_text: &str, kind: BlockKind) -> Vec<Piece> {
    let characters = normalized_text.chars().collect::<Vec<_>>();
    let part_count = characters.len().div_ceil(TARGET_CHUNK_CHARS).max(1);
    let part_size = characters.len().div_ceil(part_count).min(MAX_CHUNK_CHARS);
    let mut pieces = Vec::new();
    let mut start = 0usize;

    while start < characters.len() {
        let end = (start + part_size).min(characters.len());
        let text = characters[start..end].iter().collect::<String>();
        if let Some(piece) = Piece::new(text, kind, false) {
            pieces.push(piece);
        }
        start = end;
    }

    pieces
}

fn assemble_pieces(pieces: Vec<Piece>) -> Vec<SplitPart> {
    let mut parts = Vec::<PartBuilder>::new();
    let mut current = PartBuilder::default();

    for piece in pieces {
        if !current.is_empty() && current.len_with(&piece) > MAX_CHUNK_CHARS {
            parts.push(current);
            current = PartBuilder::default();
        }
        current.push(piece);
    }

    if !current.is_empty() {
        parts.push(current);
    }

    rebalance_tiny_trailing_part(&mut parts);
    parts.into_iter().map(PartBuilder::into_part).collect()
}

fn rebalance_tiny_trailing_part(parts: &mut [PartBuilder]) {
    if parts.len() < 2 {
        return;
    }

    let last_index = parts.len() - 1;
    if parts[last_index].len() >= MIN_TRAILING_CHARS {
        return;
    }

    let previous_index = last_index - 1;
    if parts[previous_index].pieces.len() <= 1 {
        return;
    }

    let Some(piece) = parts[previous_index].pop() else {
        return;
    };
    if parts[last_index].len_with(&piece) <= MAX_CHUNK_CHARS {
        parts[last_index].insert_front(piece);
    } else {
        parts[previous_index].push(piece);
    }
}

fn emitted_block_kind(pieces: &[Piece]) -> BlockKind {
    let mut contributions = BTreeMap::<BlockKind, usize>::new();
    for piece in pieces {
        *contributions.entry(piece.kind).or_default() += piece.len();
    }
    let total = contributions.values().sum::<usize>();
    let material_kinds = contributions
        .iter()
        .filter(|(_, length)| **length * 4 >= total)
        .count();
    if material_kinds >= 2 {
        return BlockKind::Mixed;
    }

    [
        BlockKind::Prose,
        BlockKind::Code,
        BlockKind::Table,
        BlockKind::Hex,
    ]
    .into_iter()
    .max_by_key(|kind| contributions.get(kind).copied().unwrap_or_default())
    .unwrap_or(BlockKind::Prose)
}

fn joined_len(lengths: impl IntoIterator<Item = usize>) -> usize {
    let mut total = 0usize;
    let mut count = 0usize;
    for length in lengths {
        total += length;
        count += 1;
    }
    total + count.saturating_sub(1)
}

fn text_len(text: &str) -> usize {
    text.chars().count()
}

#[derive(Debug, Clone, Copy)]
enum TextMode {
    Prose,
    Preformatted,
}

fn collect_descendant_text(element: ElementRef<'_>, mode: TextMode) -> String {
    let mut segments = Vec::new();

    for node in element.descendants() {
        let Some(text) = node.value().as_text() else {
            continue;
        };
        if node
            .ancestors()
            .filter_map(ElementRef::wrap)
            .any(|ancestor| {
                matches!(ancestor.value().name(), "script" | "style")
                    || ancestor.attr("data-pagefind-ignore").is_some()
            })
        {
            continue;
        }
        match mode {
            TextMode::Prose => {
                let normalized = normalize_text(text);
                if !normalized.is_empty() {
                    segments.push(normalized);
                }
            }
            TextMode::Preformatted => segments.push(text.to_string()),
        }
    }

    match mode {
        TextMode::Prose => segments.join(" "),
        TextMode::Preformatted => segments.join(""),
    }
}

fn collect_table_row_text(element: ElementRef<'_>) -> String {
    collect_descendant_text(element, TextMode::Prose)
}

fn collect_direct_text(element: ElementRef<'_>) -> String {
    let segments = element
        .children()
        .filter_map(|node| node.value().as_text())
        .map(|text| normalize_text(text))
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    segments.join(" ")
}

fn has_block_owner_ancestor(element: ElementRef<'_>) -> bool {
    element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            matches!(ancestor.value().name(), "p" | "li" | "pre" | "tr")
                || is_standalone_code_block(ancestor)
        })
}

fn is_standalone_code_block(element: ElementRef<'_>) -> bool {
    if element.value().name() != "code" {
        return false;
    }

    !element
        .ancestors()
        .filter_map(ElementRef::wrap)
        .any(|ancestor| {
            matches!(
                ancestor.value().name(),
                "p" | "li" | "pre" | "tr" | "td" | "th"
            )
        })
}

#[cfg(test)]
mod tests {
    use scraper::Html;

    use super::*;

    fn body_blocks(html: &str) -> Vec<SourceBlock> {
        let parsed = Html::parse_fragment(html);
        parsed
            .tree
            .root()
            .descendants()
            .filter_map(ElementRef::wrap)
            .filter_map(source_block_for_element)
            .collect()
    }

    #[test]
    fn hex_blob_character_class_is_locked() {
        for character in [
            '0', '9', 'a', 'f', 'A', 'F', 'x', 'X', ',', ':', '_', '-', '[', ']', '(', ')',
        ] {
            assert!(is_hex_blob_character(character), "{character}");
        }
        for character in ['g', 'z', '/', '.', '"', '\'', '{', '}'] {
            assert!(!is_hex_blob_character(character), "{character}");
        }
    }

    #[test]
    fn hex_blob_heuristic_uses_tokens_or_character_ratio() {
        let token_text = (0..8)
            .map(|index| format!("0x{:016x}", index))
            .collect::<Vec<_>>()
            .join(" ");
        let token_text = format!("{token_text} {}", "ordinary ".repeat(80));
        assert!(is_hex_blob_text(&token_text));

        let ratio_text = "0xabcdef_0123456789, ".repeat(40);
        assert!(is_hex_blob_text(&ratio_text));

        let prose_text =
            "This is ordinary prose with enough characters to exceed the minimum length. "
                .repeat(10);
        assert!(!is_hex_blob_text(&prose_text));
    }

    #[test]
    fn block_extraction_avoids_nested_code_and_table_duplicates() {
        let blocks = body_blocks(
            r#"
<p>Paragraph with <code>inline code</code>.</p>
<pre><code>line one
line two</code></pre>
<table><tbody><tr><td>A</td><td>B</td></tr></tbody></table>
"#,
        );

        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].kind, BlockKind::Prose);
        assert_eq!(blocks[0].normalized_text, "Paragraph with inline code .");
        assert_eq!(blocks[1].kind, BlockKind::Code);
        assert_eq!(blocks[1].normalized_text, "line one line two");
        assert_eq!(blocks[2].kind, BlockKind::Table);
        assert_eq!(blocks[2].normalized_text, "A B");
    }

    #[test]
    fn hex_blob_classification_takes_precedence_over_code() {
        let hex_line = "0xabcdef0123456789 ".repeat(40);
        let blocks = body_blocks(&format!("<pre><code>{hex_line}</code></pre>"));

        assert_eq!(blocks[0].kind, BlockKind::Hex);
    }

    #[test]
    fn mixed_requires_multiple_material_primary_kinds() {
        let prose = Piece::new("a".repeat(1_000), BlockKind::Prose, false).unwrap();
        let code = Piece::new("b".repeat(500), BlockKind::Code, false).unwrap();
        assert_eq!(
            emitted_block_kind(&[prose.clone(), code.clone()]),
            BlockKind::Mixed
        );

        let small_code = Piece::new("b".repeat(200), BlockKind::Code, false).unwrap();
        assert_eq!(emitted_block_kind(&[prose, small_code]), BlockKind::Prose);
    }

    #[test]
    fn whole_blocks_may_exceed_target_but_never_max() {
        let first = SourceBlock::new(BlockKind::Prose, "alpha ".repeat(250)).unwrap();
        let second = SourceBlock::new(BlockKind::Prose, "bravo ".repeat(70)).unwrap();
        let parts = split_blocks(&[first, second]);

        assert_eq!(parts.len(), 1);
        let length = text_len(&parts[0].text);
        assert!(length > TARGET_CHUNK_CHARS);
        assert!(length <= MAX_CHUNK_CHARS);
    }

    #[test]
    fn tiny_trailing_parts_are_rebalanced_when_boundaries_permit() {
        let first = SourceBlock::new(BlockKind::Prose, "alpha ".repeat(167)).unwrap();
        let second = SourceBlock::new(BlockKind::Prose, "bravo ".repeat(134)).unwrap();
        let third = SourceBlock::new(BlockKind::Prose, "charlie ".repeat(43)).unwrap();
        let parts = split_blocks(&[first, second, third]);

        assert_eq!(parts.len(), 2);
        assert!(text_len(&parts[1].text) >= MIN_TRAILING_CHARS);
        assert!(parts
            .iter()
            .all(|part| text_len(&part.text) <= MAX_CHUNK_CHARS));
    }

    #[test]
    fn oversized_code_splits_on_lines_where_possible() {
        let raw = (0..80)
            .map(|index| format!("let value_{index} = \"{}\";", "x".repeat(30)))
            .collect::<Vec<_>>()
            .join("\n");
        let block = SourceBlock::new(BlockKind::Code, raw).unwrap();
        let parts = split_blocks(&[block]);

        assert!(parts.len() > 1);
        assert!(parts
            .iter()
            .all(|part| text_len(&part.text) <= MAX_CHUNK_CHARS));
        assert!(parts.iter().all(|part| part.split_from_oversized_block));
        assert_eq!(
            normalize_text(
                &parts
                    .iter()
                    .map(|part| part.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    #[test]
    fn hard_split_balances_giant_single_token_hex() {
        let block = SourceBlock::new(BlockKind::Hex, format!("0x{}", "a".repeat(5_000))).unwrap();
        let parts = split_blocks(&[block]);

        assert!(parts.len() > 1);
        assert!(parts
            .iter()
            .all(|part| text_len(&part.text) <= MAX_CHUNK_CHARS));
        assert!(parts.iter().all(|part| part.split_from_oversized_block));
        assert_eq!(parts[0].block_kind, BlockKind::Hex);
    }
}
