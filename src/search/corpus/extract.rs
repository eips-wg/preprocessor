/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Rendered HTML search corpus extraction.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use scraper::{ElementRef, Html, Selector};
use snafu::{ResultExt, Whatever};

use super::{
    chunks::{collect_text, element_has_pagefind_ignore, extract_chunks},
    schema::{CorpusOutput, DocumentRecord, PagefindData, CORPUS_VERSION},
};

const PAGEFIND_DIR: &str = "pagefind";
const DOCUMENT_SELECTOR: &str = "[data-pagefind-body]";
const HOOK_SELECTOR: &str = "[data-pagefind-filter], [data-pagefind-meta], [data-pagefind-sort]";

#[derive(Debug)]
struct Selectors {
    document: Selector,
    hooks: Selector,
    title: Selector,
}

#[derive(Debug)]
struct ExtractedDocument {
    document: DocumentRecord,
    chunks: Vec<super::schema::ChunkRecord>,
}

fn selector(selector: &str) -> Result<Selector, Whatever> {
    match Selector::parse(selector) {
        Ok(selector) => Ok(selector),
        Err(error) => {
            snafu::whatever!("unable to parse corpus selector `{selector}`: {error}");
        }
    }
}

fn selectors() -> Result<Selectors, Whatever> {
    Ok(Selectors {
        document: selector(DOCUMENT_SELECTOR)?,
        hooks: selector(HOOK_SELECTOR)?,
        title: selector("title")?,
    })
}

pub(super) fn extract_corpus(
    output_path: &Path,
    base_path: &str,
) -> Result<CorpusOutput, Whatever> {
    let selectors = selectors()?;
    let mut documents = Vec::new();
    let mut chunks = Vec::new();

    for html_path in rendered_html_files(output_path)? {
        let Some(extracted) = extract_document(output_path, &html_path, base_path, &selectors)?
        else {
            continue;
        };
        chunks.extend(extracted.chunks);
        documents.push(extracted.document);
    }

    Ok(CorpusOutput {
        version: CORPUS_VERSION,
        documents,
        chunks,
    })
}

fn rendered_html_files(output_path: &Path) -> Result<Vec<PathBuf>, Whatever> {
    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(output_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || entry
                    .path()
                    .strip_prefix(output_path)
                    .ok()
                    .and_then(|path| path.components().next())
                    .is_none_or(|component| {
                        !matches!(component, Component::Normal(name) if name == PAGEFIND_DIR)
                    })
        });

    for entry in walker {
        let entry = entry.with_whatever_context(|_| {
            format!(
                "unable to walk rendered output directory `{}`",
                output_path.to_string_lossy()
            )
        })?;
        let file_type = entry.file_type();
        if file_type.is_symlink() || !file_type.is_file() {
            continue;
        }
        let path = entry.into_path();
        if path
            .extension()
            .is_some_and(|extension| extension == "html")
        {
            files.push(path);
        }
    }

    files.sort();
    Ok(files)
}

fn extract_document(
    output_path: &Path,
    html_path: &Path,
    base_path: &str,
    selectors: &Selectors,
) -> Result<Option<ExtractedDocument>, Whatever> {
    let html = fs::read_to_string(html_path).with_whatever_context(|_| {
        format!(
            "unable to read rendered HTML `{}`",
            html_path.to_string_lossy()
        )
    })?;
    let parsed = Html::parse_document(&html);
    let Some(body) = parsed.select(&selectors.document).next() else {
        return Ok(None);
    };

    let pagefind = collect_pagefind_data(&parsed, selectors);
    let url = rendered_url(output_path, html_path, base_path)?;
    let title = document_title(&parsed, body, selectors, &pagefind, &url);
    let id = pagefind
        .meta
        .get("proposal")
        .map(|proposal| format!("proposal:{proposal}"))
        .unwrap_or_else(|| format!("url:{url}"));
    let text = collect_text(body, false);
    let document = DocumentRecord {
        id,
        kind: "document",
        url,
        title,
        text,
        filters: pagefind.filters,
        meta: pagefind.meta,
        sort: pagefind.sort,
    };
    let chunks = extract_chunks(body, &document);

    Ok(Some(ExtractedDocument { document, chunks }))
}

fn collect_pagefind_data(parsed: &Html, selectors: &Selectors) -> PagefindData {
    let mut data = PagefindData::default();

    for element in parsed.select(&selectors.hooks) {
        if element_has_pagefind_ignore(element, true) {
            continue;
        }
        collect_hook_value(element, "data-pagefind-filter", |key, value| {
            let values = data.filters.entry(key).or_default();
            if !values.contains(&value) {
                values.push(value);
            }
        });
        collect_hook_value(element, "data-pagefind-meta", |key, value| {
            data.meta.entry(key).or_insert(value);
        });
        collect_hook_value(element, "data-pagefind-sort", |key, value| {
            data.sort.entry(key).or_insert(value);
        });
    }

    for values in data.filters.values_mut() {
        values.sort();
    }

    data
}

fn collect_hook_value<F>(element: ElementRef<'_>, attr: &str, mut collect: F)
where
    F: FnMut(String, String),
{
    let Some(raw) = element.attr(attr) else {
        return;
    };
    let Some((key, value)) = pagefind_attribute_value(element, raw) else {
        return;
    };
    if key.is_empty() || value.is_empty() {
        return;
    }
    collect(key, value);
}

fn pagefind_attribute_value(element: ElementRef<'_>, raw: &str) -> Option<(String, String)> {
    if let Some((key, capture)) = raw.split_once('[') {
        let capture = capture.strip_suffix(']')?;
        let value = element.attr(capture)?.trim();
        return Some((key.trim().to_owned(), value.to_owned()));
    }

    let (key, value) = raw.split_once(':')?;
    Some((key.trim().to_owned(), value.trim().to_owned()))
}

fn document_title(
    parsed: &Html,
    body: ElementRef<'_>,
    selectors: &Selectors,
    pagefind: &PagefindData,
    url: &str,
) -> String {
    pagefind
        .meta
        .get("title")
        .filter(|title| !title.is_empty())
        .cloned()
        .or_else(|| {
            body.select(&selector("h1").expect("valid h1 selector"))
                .next()
                .map(|heading| collect_text(heading, false))
                .filter(|title| !title.is_empty())
        })
        .or_else(|| {
            parsed
                .select(&selectors.title)
                .next()
                .map(|title| collect_text(title, false))
                .filter(|title| !title.is_empty())
        })
        .unwrap_or_else(|| url.to_owned())
}

fn rendered_url(output_path: &Path, html_path: &Path, base_path: &str) -> Result<String, Whatever> {
    let relative = html_path
        .strip_prefix(output_path)
        .with_whatever_context(|_| {
            format!(
                "rendered HTML path `{}` is not under output directory `{}`",
                html_path.to_string_lossy(),
                output_path.to_string_lossy()
            )
        })?;
    let mut components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();

    if components.last().is_some_and(|name| name == "index.html") {
        components.pop();
        let suffix = components.join("/");
        return Ok(if suffix.is_empty() {
            base_path.to_owned()
        } else {
            format!("{base_path}{suffix}/")
        });
    }

    Ok(format!("{base_path}{}", components.join("/")))
}
