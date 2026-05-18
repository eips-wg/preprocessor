/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use log::info;
use pagefind::api::PagefindIndex;
use snafu::{OptionExt, ResultExt, Whatever};
use tokio::runtime::Builder;

use super::{SearchIndexRequest, SearchIndexSummary};

const PAGEFIND_DIR: &str = "pagefind";

pub(super) fn index_site(request: SearchIndexRequest) -> Result<SearchIndexSummary, Whatever> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .whatever_context("unable to create Pagefind runtime")?;

    runtime.block_on(index_site_async(request))
}

async fn index_site_async(request: SearchIndexRequest) -> Result<SearchIndexSummary, Whatever> {
    let (output_path, pagefind_path) = prepare_pagefind_output_path(&request.output_path)?;
    let site_arg = pagefind_path_arg(&output_path, "rendered output directory")?;
    let output_arg = pagefind_path_arg(&pagefind_path, "Pagefind output directory")?;

    info!(
        "indexing rendered output for search from `{}`",
        output_path.to_string_lossy()
    );

    let mut index = PagefindIndex::new(None).whatever_context("unable to create Pagefind index")?;
    let pages_indexed = index
        .add_directory(site_arg, None)
        .await
        .whatever_context("unable to index rendered output with Pagefind")?;
    let written_path = index
        .write_files(Some(output_arg))
        .await
        .whatever_context("unable to write Pagefind assets")?;

    let output_path = PathBuf::from(written_path);
    info!(
        "Pagefind indexed {pages_indexed} page(s) into `{}`",
        output_path.to_string_lossy()
    );

    Ok(SearchIndexSummary {
        pages_indexed,
        output_path,
    })
}

fn prepare_pagefind_output_path(output_path: &Path) -> Result<(PathBuf, PathBuf), Whatever> {
    let output_path = output_path.canonicalize().with_whatever_context(|_| {
        format!(
            "unable to resolve rendered output directory `{}`",
            output_path.to_string_lossy()
        )
    })?;
    let pagefind_path = output_path.join(PAGEFIND_DIR);

    cleanup_stale_pagefind_path(&output_path, &pagefind_path)?;

    Ok((output_path, pagefind_path))
}

fn cleanup_stale_pagefind_path(output_path: &Path, pagefind_path: &Path) -> Result<(), Whatever> {
    let metadata = match fs::symlink_metadata(pagefind_path) {
        Ok(metadata) => metadata,
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_whatever_context(|_| {
                format!(
                    "unable to inspect stale Pagefind output path `{}`",
                    pagefind_path.to_string_lossy()
                )
            });
        }
    };

    let cleanup_target = pagefind_path.canonicalize().with_whatever_context(|_| {
        format!(
            "unable to resolve stale Pagefind output path `{}`",
            pagefind_path.to_string_lossy()
        )
    })?;

    if !is_strict_descendant(&cleanup_target, output_path) {
        snafu::whatever!(
            "refusing to clean Pagefind output path `{}` because it resolves outside rendered output directory `{}`",
            cleanup_target.to_string_lossy(),
            output_path.to_string_lossy()
        );
    }

    let file_type = metadata.file_type();
    if file_type.is_symlink() || metadata.is_file() {
        fs::remove_file(pagefind_path).with_whatever_context(|_| {
            format!(
                "unable to remove stale Pagefind output file `{}`",
                pagefind_path.to_string_lossy()
            )
        })?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(pagefind_path).with_whatever_context(|_| {
            format!(
                "unable to remove stale Pagefind output directory `{}`",
                pagefind_path.to_string_lossy()
            )
        })?;
    } else {
        snafu::whatever!(
            "refusing to clean unsupported Pagefind output path type `{}`",
            pagefind_path.to_string_lossy()
        );
    }

    Ok(())
}

fn is_strict_descendant(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent) && path != parent
}

fn pagefind_path_arg(path: &Path, role: &str) -> Result<String, Whatever> {
    path.to_str().map(str::to_owned).with_whatever_context(|| {
        format!(
            "unable to pass non-UTF-8 {role} `{}` to Pagefind",
            path.to_string_lossy()
        )
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::search::SearchIndexRequest;

    use super::{index_site, prepare_pagefind_output_path, PAGEFIND_DIR};

    fn write_file(root: &Path, relative: impl AsRef<Path>, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn rendered_site(root: &Path) -> PathBuf {
        let output_path = root.join("output");
        write_file(
            &output_path,
            "index.html",
            "<!doctype html><html><head><title>Search Fixture</title></head><body><main><h1>Search Fixture</h1><p>Rendered proposal body.</p></main></body></html>",
        );
        output_path
    }

    fn pagefind_filenames(pagefind_path: &Path) -> Vec<String> {
        walkdir::WalkDir::new(pagefind_path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(pagefind_path)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    fn assert_pagefind_artifacts(pagefind_path: &Path) {
        let filenames = pagefind_filenames(pagefind_path);

        assert!(pagefind_path.join("pagefind.js").is_file());
        assert!(pagefind_path.join("pagefind-entry.json").is_file());
        assert!(
            filenames
                .iter()
                .any(|filename| filename.ends_with(".pf_index")),
            "expected at least one Pagefind index artifact, found {filenames:?}"
        );
    }

    #[test]
    fn pagefind_writes_under_rendered_output_tree() {
        let temp = TempDir::new().unwrap();
        let output_path = rendered_site(temp.path());
        let prepared_source_path = temp.path().join("repo");
        std::fs::create_dir_all(&prepared_source_path).unwrap();

        let summary = index_site(SearchIndexRequest {
            output_path: output_path.clone(),
        })
        .unwrap();

        assert!(summary.pages_indexed >= 1);
        assert_eq!(summary.output_path, output_path.join(PAGEFIND_DIR));
        assert_pagefind_artifacts(&output_path.join(PAGEFIND_DIR));
        assert!(!prepared_source_path.join(PAGEFIND_DIR).exists());
        assert!(!temp.path().join(PAGEFIND_DIR).exists());
    }

    #[test]
    fn stale_pagefind_output_is_removed_before_indexing() {
        let temp = TempDir::new().unwrap();
        let output_path = rendered_site(temp.path());
        let stale_path = output_path.join(PAGEFIND_DIR).join("stale.txt");
        write_file(
            &output_path,
            PathBuf::from(format!("{PAGEFIND_DIR}/stale.txt")),
            "stale",
        );

        index_site(SearchIndexRequest {
            output_path: output_path.clone(),
        })
        .unwrap();

        assert!(!stale_path.exists());
        assert_pagefind_artifacts(&output_path.join(PAGEFIND_DIR));
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn stale_pagefind_cleanup_rejects_targets_outside_output_tree() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let output_path = rendered_site(temp.path());
        let outside_path = temp.path().join("outside-pagefind");
        std::fs::create_dir_all(&outside_path).unwrap();
        write_file(&outside_path, "external.txt", "outside");
        symlink(&outside_path, output_path.join(PAGEFIND_DIR)).unwrap();

        let error = prepare_pagefind_output_path(&output_path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("refusing to clean Pagefind output path"));
        assert!(outside_path.join("external.txt").is_file());
    }
}
