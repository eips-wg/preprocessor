use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::Value;
use tempfile::TempDir;
use url::Url;

use crate::{
    config::SearchCorpusFormat,
    search::{SearchCorpusConfig, SearchCorpusRequest, DEFAULT_CORPUS_OUTPUT_FILE},
};

use super::{
    reconcile_corpus,
    schema::ChunkRecord,
    splitter::{self, SPLITTER_VERSION},
};

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn request(
    output_path: PathBuf,
    enabled: bool,
    format: SearchCorpusFormat,
    output: &str,
) -> SearchCorpusRequest {
    SearchCorpusRequest {
        output_path,
        base_url: Url::parse("https://eips-wg.github.io/EIPs/").unwrap(),
        config: SearchCorpusConfig {
            enabled,
            format,
            output: PathBuf::from(output),
        },
    }
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn corpus_extracts_rendered_documents_chunks_and_pagefind_hooks() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    write_file(
        &output,
        "1559/index.html",
        r#"
<!doctype html>
<html>
<head><title>Rendered fallback title</title></head>
<body>
<nav data-pagefind-ignore>Repeated navigation</nav>
<aside>
  <span data-pagefind-filter="status:Last Call"></span>
  <span data-pagefind-meta="description:Outside-body description"></span>
  <span data-pagefind-sort="created:2019-04-13"></span>
</aside>
<main data-pagefind-body>
  <h1 data-pagefind-meta="title[data-pagefind-value]" data-pagefind-value="EIP-1559: Fee Market">EIP-1559: Fee Market</h1>
  <span data-pagefind-filter="status:Final"></span>
  <span data-pagefind-filter="proposal_category:Core"></span>
  <span data-pagefind-filter="author[data-pagefind-value]" data-pagefind-value="Alice"></span>
  <span data-pagefind-meta="proposal:EIP-1559"></span>
  <span data-pagefind-meta="created[data-pagefind-value]" data-pagefind-value="2019-04-13"></span>
  <span data-pagefind-sort="number:1559"></span>
  <span data-pagefind-ignore>
    <span data-pagefind-filter="status:Draft"></span>
    <span>Ignored body copy</span>
  </span>
  <span data-pagefind-ignore="all" data-pagefind-filter="status:Hidden"></span>
  <p>Intro text before heading.</p>
  <h2 id="abstract">Abstract</h2>
  <script>Script text.</script>
  <style>Style text.</style>
  <p>Chunk text &amp; entities.</p>
  <h3>Motivation</h3>
  <p>Subsection text.</p>
  <h2 id="abstract">Abstract</h2>
  <p>Duplicate heading text.</p>
</main>
</body>
</html>
"#,
    );

    let summary = reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();

    assert_eq!(summary.documents, 1);
    assert_eq!(summary.chunks, 3);
    let output_file = output.join(DEFAULT_CORPUS_OUTPUT_FILE);
    assert_eq!(summary.output_path, output_file);
    let raw = std::fs::read_to_string(&output_file).unwrap();
    assert!(raw.ends_with('\n'));
    assert!(!raw.contains("\n  "));

    let json = read_json(&output_file);
    assert_eq!(json["version"], 1);
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    assert_eq!(json["chunks"].as_array().unwrap().len(), 3);

    let document = &json["documents"][0];
    assert_eq!(document["id"], "proposal:EIP-1559");
    assert_eq!(document["kind"], "document");
    assert_eq!(document["url"], "/EIPs/1559/");
    assert_eq!(document["title"], "EIP-1559: Fee Market");
    assert!(document["text"]
        .as_str()
        .unwrap()
        .contains("Intro text before heading."));
    assert!(document["text"]
        .as_str()
        .unwrap()
        .contains("Chunk text & entities."));
    assert!(!document["text"]
        .as_str()
        .unwrap()
        .contains("Ignored body copy"));
    assert!(!document["text"].as_str().unwrap().contains("Script text"));
    assert!(!document["text"].as_str().unwrap().contains("Style text"));
    assert_eq!(document["filters"]["author"][0], "Alice");
    assert_eq!(document["filters"]["proposal_category"][0], "Core");
    assert_eq!(document["filters"]["status"][0], "Draft");
    assert_eq!(document["filters"]["status"][1], "Final");
    assert_eq!(document["filters"]["status"][2], "Last Call");
    assert_eq!(document["meta"]["description"], "Outside-body description");
    assert_eq!(document["meta"]["proposal"], "EIP-1559");
    assert_eq!(document["meta"]["created"], "2019-04-13");
    assert_eq!(document["sort"]["created"], "2019-04-13");
    assert_eq!(document["sort"]["number"], "1559");

    let chunks = json["chunks"].as_array().unwrap();
    assert_eq!(chunks[0]["id"], "proposal:EIP-1559#abstract:0:part:0");
    assert_eq!(chunks[0]["section_id"], "proposal:EIP-1559#abstract:0");
    assert_eq!(chunks[0]["document_id"], "proposal:EIP-1559");
    assert_eq!(chunks[0]["url"], "/EIPs/1559/#abstract");
    assert_eq!(chunks[0]["heading"], "Abstract");
    assert_eq!(chunks[0]["heading_path"][0], "Abstract");
    assert_eq!(chunks[0]["ordinal"], 0);
    assert_eq!(chunks[0]["part_index"], 0);
    assert_eq!(chunks[0]["part_count"], 1);
    assert_eq!(chunks[0]["block_kind"], "prose");
    assert_eq!(chunks[0]["splitter_version"], SPLITTER_VERSION);
    assert_eq!(chunks[0]["split_from_oversized_block"], false);
    assert_eq!(chunks[0]["text"], "Chunk text & entities.");
    assert_eq!(chunks[0]["filters"], document["filters"]);
    assert_eq!(chunks[0]["meta"], document["meta"]);
    assert_eq!(chunks[0]["sort"], document["sort"]);

    assert_eq!(chunks[1]["id"], "proposal:EIP-1559#motivation:0:part:0");
    assert_eq!(chunks[1]["url"], "/EIPs/1559/");
    assert_eq!(chunks[1]["heading_path"][0], "Abstract");
    assert_eq!(chunks[1]["heading_path"][1], "Motivation");
    assert_eq!(chunks[2]["id"], "proposal:EIP-1559#abstract:1:part:0");
    assert_eq!(chunks[2]["section_id"], "proposal:EIP-1559#abstract:1");
    assert_eq!(chunks[2]["url"], "/EIPs/1559/#abstract");
    assert_eq!(chunks[2]["ordinal"], 1);
}

#[test]
fn long_prose_section_emits_bounded_retrieval_parts() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    let paragraphs = (0..12)
        .map(|index| {
            format!(
                "Sentence {index}. {}",
                "retrieval sized prose content ".repeat(14)
            )
        })
        .collect::<Vec<_>>();
    let paragraph_html = paragraphs
        .iter()
        .map(|paragraph| format!("<p>{paragraph}</p>"))
        .collect::<String>();
    write_file(
        &output,
        "2/index.html",
        &format!(
            r#"<main data-pagefind-body><span data-pagefind-meta="proposal:EIP-2"></span><p>Document preamble only.</p><h2 id="specification">Specification</h2>{paragraph_html}</main>"#
        ),
    );

    reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();

    let json = read_json(&output.join(DEFAULT_CORPUS_OUTPUT_FILE));
    let document = &json["documents"][0];
    assert!(document["text"]
        .as_str()
        .unwrap()
        .contains("Document preamble only."));

    let chunks = json["chunks"].as_array().unwrap();
    assert!(chunks.len() > 1);
    let section_id = "proposal:EIP-2#specification:0";
    let reconstructed = chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            assert_eq!(chunk["section_id"], section_id);
            assert_eq!(chunk["id"], format!("{section_id}:part:{index}"));
            assert_eq!(chunk["ordinal"], 0);
            assert_eq!(chunk["part_index"], index);
            assert_eq!(chunk["part_count"], chunks.len());
            assert_eq!(chunk["block_kind"], "prose");
            assert_eq!(chunk["splitter_version"], SPLITTER_VERSION);
            assert_eq!(chunk["split_from_oversized_block"], false);
            assert_eq!(chunk["url"], "/EIPs/2/#specification");
            assert_eq!(chunk["heading"], "Specification");
            let text = chunk["text"].as_str().unwrap();
            assert!(text.chars().count() <= splitter::MAX_CHUNK_CHARS);
            assert!(!text.contains("Specification"));
            assert!(!text.contains("Document preamble only."));
            text
        })
        .collect::<Vec<_>>()
        .join(" ");
    let expected = splitter::normalize_text(&paragraphs.join(" "));
    assert_eq!(splitter::normalize_text(&reconstructed), expected);
}

#[test]
fn code_table_and_hex_sections_emit_expected_block_kinds() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    let code = (0..90)
        .map(|index| format!("call_method_{index}(\"{}\");", "qz".repeat(20)))
        .collect::<Vec<_>>()
        .join("\n");
    let table_rows = (0..10)
        .map(|index| {
            format!(
                "<tr><td>case {index}</td><td>{}</td></tr>",
                "table vector ".repeat(30)
            )
        })
        .collect::<String>();
    let hex = format!("0x{}", "abcdef0123456789".repeat(260));
    write_file(
        &output,
        "3/index.html",
        &format!(
            r#"
<main data-pagefind-body>
  <span data-pagefind-meta="proposal:EIP-3"></span>
  <h2 id="reference-implementation">Reference Implementation</h2>
  <pre><code>{code}</code></pre>
  <h2 id="test-vectors">Test Vectors</h2>
  <table><tbody>{table_rows}</tbody></table>
  <h2 id="bytecode">Bytecode</h2>
  <pre><code>{hex}</code></pre>
</main>
"#
        ),
    );

    reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::Chunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();

    let json = read_json(&output.join(DEFAULT_CORPUS_OUTPUT_FILE));
    assert!(json["documents"].as_array().unwrap().is_empty());
    let chunks = json["chunks"].as_array().unwrap();
    assert!(chunks
        .iter()
        .all(|chunk| chunk["text"].as_str().unwrap().chars().count() <= splitter::MAX_CHUNK_CHARS));

    let code_chunks = chunks
        .iter()
        .filter(|chunk| chunk["section_id"] == "proposal:EIP-3#reference-implementation:0")
        .collect::<Vec<_>>();
    assert!(code_chunks.len() > 1);
    assert!(code_chunks
        .iter()
        .all(|chunk| chunk["block_kind"] == "code"));
    assert!(code_chunks
        .iter()
        .all(|chunk| chunk["split_from_oversized_block"] == true));
    assert_eq!(
        splitter::normalize_text(
            &code_chunks
                .iter()
                .map(|chunk| chunk["text"].as_str().unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        ),
        splitter::normalize_text(&code)
    );

    let table_chunks = chunks
        .iter()
        .filter(|chunk| chunk["section_id"] == "proposal:EIP-3#test-vectors:0")
        .collect::<Vec<_>>();
    assert!(table_chunks.len() > 1);
    assert!(table_chunks
        .iter()
        .all(|chunk| chunk["block_kind"] == "table"));
    assert!(table_chunks
        .iter()
        .all(|chunk| chunk["split_from_oversized_block"] == false));
    assert!(table_chunks
        .iter()
        .all(|chunk| chunk["url"] == "/EIPs/3/#test-vectors"));

    let hex_chunks = chunks
        .iter()
        .filter(|chunk| chunk["section_id"] == "proposal:EIP-3#bytecode:0")
        .collect::<Vec<_>>();
    assert!(hex_chunks.len() > 1);
    assert!(hex_chunks.iter().all(|chunk| chunk["block_kind"] == "hex"));
    assert!(hex_chunks
        .iter()
        .all(|chunk| chunk["split_from_oversized_block"] == true));
}

#[test]
fn corpus_output_is_deterministic_across_repeated_extractions() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    write_file(
        &output,
        "4/index.html",
        r#"<main data-pagefind-body><span data-pagefind-meta="proposal:EIP-4"></span><h2 id="a">A</h2><p>Stable text.</p></main>"#,
    );

    reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();
    let first = std::fs::read_to_string(output.join(DEFAULT_CORPUS_OUTPUT_FILE)).unwrap();

    reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();
    let second = std::fs::read_to_string(output.join(DEFAULT_CORPUS_OUTPUT_FILE)).unwrap();

    assert_eq!(first, second);
}

#[test]
fn chunk_record_field_order_is_stable() {
    let chunk = ChunkRecord {
        id: "proposal:EIP-1#a:0:part:0".to_owned(),
        section_id: "proposal:EIP-1#a:0".to_owned(),
        document_id: "proposal:EIP-1".to_owned(),
        kind: "chunk",
        url: "/EIPs/1/#a".to_owned(),
        title: "EIP-1".to_owned(),
        heading: "A".to_owned(),
        heading_path: vec!["A".to_owned()],
        ordinal: 0,
        part_index: 0,
        part_count: 1,
        block_kind: "prose".to_owned(),
        splitter_version: SPLITTER_VERSION,
        split_from_oversized_block: false,
        text: "Text.".to_owned(),
        filters: BTreeMap::new(),
        meta: BTreeMap::new(),
        sort: BTreeMap::new(),
    };

    assert_eq!(
        serde_json::to_string(&chunk).unwrap(),
        r#"{"id":"proposal:EIP-1#a:0:part:0","section_id":"proposal:EIP-1#a:0","document_id":"proposal:EIP-1","kind":"chunk","url":"/EIPs/1/#a","title":"EIP-1","heading":"A","heading_path":["A"],"ordinal":0,"part_index":0,"part_count":1,"block_kind":"prose","splitter_version":1,"split_from_oversized_block":false,"text":"Text.","filters":{},"meta":{},"sort":{}}"#
    );
}

#[test]
fn corpus_formats_and_cleanup_reconcile_known_artifacts() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    write_file(
        &output,
        "1/index.html",
        r#"<main data-pagefind-body><span data-pagefind-meta="proposal:EIP-1"></span><h2 id="a">A</h2><p>Text.</p></main>"#,
    );
    write_file(&output, DEFAULT_CORPUS_OUTPUT_FILE, "stale default");

    let summary = reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::Chunks,
        "debug/corpus.json",
    ))
    .unwrap()
    .unwrap();

    assert_eq!(summary.documents, 0);
    assert_eq!(summary.chunks, 1);
    assert!(!output.join(DEFAULT_CORPUS_OUTPUT_FILE).exists());
    let json = read_json(&output.join("debug/corpus.json"));
    assert!(json["documents"].as_array().unwrap().is_empty());
    assert_eq!(json["chunks"].as_array().unwrap().len(), 1);

    let summary = reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::Documents,
        "debug/corpus.json",
    ))
    .unwrap()
    .unwrap();
    assert_eq!(summary.documents, 1);
    assert_eq!(summary.chunks, 0);
    let json = read_json(&output.join("debug/corpus.json"));
    assert_eq!(json["documents"].as_array().unwrap().len(), 1);
    assert!(json["chunks"].as_array().unwrap().is_empty());

    let summary = reconcile_corpus(request(
        output.clone(),
        false,
        SearchCorpusFormat::DocumentsAndChunks,
        "debug/corpus.json",
    ))
    .unwrap();
    assert!(summary.is_none());
    assert!(!output.join(DEFAULT_CORPUS_OUTPUT_FILE).exists());
    assert!(!output.join("debug/corpus.json").exists());
}

#[test]
fn corpus_skips_pages_without_body_and_pagefind_output() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    write_file(&output, "index.html", "<p>Home</p>");
    write_file(
        &output,
        "pagefind/index.html",
        r#"<main data-pagefind-body><h2 id="hidden">Hidden</h2><p>Search runtime.</p></main>"#,
    );

    let summary = reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();

    assert_eq!(summary.documents, 0);
    assert_eq!(summary.chunks, 0);
    let json = read_json(&output.join(DEFAULT_CORPUS_OUTPUT_FILE));
    assert!(json["documents"].as_array().unwrap().is_empty());
    assert!(json["chunks"].as_array().unwrap().is_empty());
}

#[test]
fn corpus_uses_url_and_heading_fallbacks_when_metadata_or_ids_are_absent() {
    let temp = TempDir::new().unwrap();
    let output = temp.path().join("output");
    write_file(
        &output,
        "reference/page.html",
        r#"
<html>
<head><title>Reference Page</title></head>
<body>
<main data-pagefind-body>
  <h2>Heading Without Id!</h2>
  <p>Rendered section.</p>
</main>
</body>
</html>
"#,
    );

    reconcile_corpus(request(
        output.clone(),
        true,
        SearchCorpusFormat::DocumentsAndChunks,
        DEFAULT_CORPUS_OUTPUT_FILE,
    ))
    .unwrap()
    .unwrap();

    let json = read_json(&output.join(DEFAULT_CORPUS_OUTPUT_FILE));
    let document = &json["documents"][0];
    assert_eq!(document["id"], "url:/EIPs/reference/page.html");
    assert_eq!(document["url"], "/EIPs/reference/page.html");
    assert_eq!(document["title"], "Reference Page");

    let chunk = &json["chunks"][0];
    assert_eq!(
        chunk["id"],
        "url:/EIPs/reference/page.html#heading-without-id:0:part:0"
    );
    assert_eq!(
        chunk["section_id"],
        "url:/EIPs/reference/page.html#heading-without-id:0"
    );
    assert_eq!(chunk["url"], "/EIPs/reference/page.html");
    assert_eq!(chunk["heading"], "Heading Without Id!");
}
