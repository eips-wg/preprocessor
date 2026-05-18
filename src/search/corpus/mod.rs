/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Rendered-HTML search corpus extraction.

mod chunks;
mod extract;
mod output;
mod schema;
mod splitter;

#[cfg(test)]
mod tests;

use std::path::Path;

use snafu::{ResultExt, Whatever};

use crate::{config::SearchCorpusFormat, search::normalized_base_path};

use super::{SearchCorpusRequest, SearchCorpusSummary, DEFAULT_CORPUS_OUTPUT_FILE};
use schema::{CorpusOutput, CORPUS_VERSION};

pub(super) fn reconcile_corpus(
    request: SearchCorpusRequest,
) -> Result<Option<SearchCorpusSummary>, Whatever> {
    let output_path = request
        .output_path
        .canonicalize()
        .with_whatever_context(|_| {
            format!(
                "unable to resolve rendered output directory `{}`",
                request.output_path.to_string_lossy()
            )
        })?;
    let default_output = Path::new(DEFAULT_CORPUS_OUTPUT_FILE);
    let configured_output = request.config.output.as_path();

    output::cleanup_known_corpus_artifact(&output_path, default_output)?;
    if configured_output != default_output {
        output::cleanup_known_corpus_artifact(&output_path, configured_output)?;
    }

    if !request.config.enabled {
        return Ok(None);
    }

    let corpus = extract::extract_corpus(&output_path, &normalized_base_path(&request.base_url))?;
    let documents = match request.config.format {
        SearchCorpusFormat::Documents | SearchCorpusFormat::DocumentsAndChunks => corpus.documents,
        SearchCorpusFormat::Chunks => Vec::new(),
    };
    let chunks = match request.config.format {
        SearchCorpusFormat::Chunks | SearchCorpusFormat::DocumentsAndChunks => corpus.chunks,
        SearchCorpusFormat::Documents => Vec::new(),
    };
    let output = CorpusOutput {
        version: CORPUS_VERSION,
        documents,
        chunks,
    };
    let documents = output.documents.len();
    let chunks = output.chunks.len();
    let output_file = output::write_corpus_file(&output_path, configured_output, &output)?;

    Ok(Some(SearchCorpusSummary {
        documents,
        chunks,
        output_path: output_file,
    }))
}
