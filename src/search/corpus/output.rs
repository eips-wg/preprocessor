/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Search corpus output path safety and writing.

use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

use snafu::{ResultExt, Whatever};

use super::schema::CorpusOutput;

pub(super) fn cleanup_known_corpus_artifact(
    output_path: &Path,
    relative_path: &Path,
) -> Result<(), Whatever> {
    let Some((target, metadata)) = existing_safe_artifact(output_path, relative_path)? else {
        return Ok(());
    };

    if metadata.is_file() && !metadata.file_type().is_symlink() {
        fs::remove_file(&target).with_whatever_context(|_| {
            format!(
                "unable to remove stale search corpus output `{}`",
                target.to_string_lossy()
            )
        })?;
        return Ok(());
    }

    snafu::whatever!(
        "refusing to clean unsupported search corpus output path type `{}`",
        target.to_string_lossy()
    );
}

fn existing_safe_artifact(
    output_path: &Path,
    relative_path: &Path,
) -> Result<Option<(PathBuf, fs::Metadata)>, Whatever> {
    let mut current = output_path.to_path_buf();
    let components = relative_path.components().collect::<Vec<_>>();

    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            snafu::whatever!(
                "search corpus output path `{}` must contain only safe relative path components",
                relative_path.to_string_lossy()
            );
        };
        current.push(name);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!(
                        "unable to inspect search corpus output path `{}`",
                        current.to_string_lossy()
                    )
                });
            }
        };

        let is_target = index + 1 == components.len();
        if is_target {
            return Ok(Some((current, metadata)));
        }
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            snafu::whatever!(
                "refusing to inspect search corpus output through unsupported directory `{}`",
                current.to_string_lossy()
            );
        }
    }

    Ok(None)
}

pub(super) fn write_corpus_file(
    output_path: &Path,
    relative_path: &Path,
    corpus: &CorpusOutput,
) -> Result<PathBuf, Whatever> {
    let output_file = output_path.join(relative_path);
    ensure_safe_parent(output_path, relative_path)?;
    match fs::symlink_metadata(&output_file) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            snafu::whatever!(
                "refusing to write search corpus output over unsupported path type `{}`",
                output_file.to_string_lossy()
            );
        }
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {}
        Err(error) => {
            return Err(error).with_whatever_context(|_| {
                format!(
                    "unable to inspect search corpus output `{}`",
                    output_file.to_string_lossy()
                )
            });
        }
    }

    let temp_file = output_file.with_extension(format!("json.tmp.{}", std::process::id()));
    if let Some(parent) = temp_file.parent() {
        fs::create_dir_all(parent).with_whatever_context(|_| {
            format!(
                "unable to create search corpus output directory `{}`",
                parent.to_string_lossy()
            )
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_file)
        .with_whatever_context(|_| {
            format!(
                "unable to create temporary search corpus output `{}`",
                temp_file.to_string_lossy()
            )
        })?;
    serde_json::to_writer(&mut file, corpus).with_whatever_context(|_| {
        format!(
            "unable to encode search corpus JSON `{}`",
            temp_file.to_string_lossy()
        )
    })?;
    file.write_all(b"\n").with_whatever_context(|_| {
        format!(
            "unable to finish search corpus JSON `{}`",
            temp_file.to_string_lossy()
        )
    })?;
    file.sync_all().with_whatever_context(|_| {
        format!(
            "unable to flush search corpus JSON `{}`",
            temp_file.to_string_lossy()
        )
    })?;
    drop(file);

    fs::rename(&temp_file, &output_file).with_whatever_context(|_| {
        format!(
            "unable to replace search corpus output `{}`",
            output_file.to_string_lossy()
        )
    })?;

    Ok(output_file)
}

fn ensure_safe_parent(output_path: &Path, relative_path: &Path) -> Result<(), Whatever> {
    let parent = relative_path.parent().unwrap_or_else(|| Path::new(""));
    let mut current = output_path.to_path_buf();

    for component in parent.components() {
        let Component::Normal(name) = component else {
            snafu::whatever!(
                "search corpus output path `{}` must contain only safe relative path components",
                relative_path.to_string_lossy()
            );
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                snafu::whatever!(
                    "refusing to use unsupported search corpus output directory `{}`",
                    current.to_string_lossy()
                );
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                fs::create_dir(&current).with_whatever_context(|_| {
                    format!(
                        "unable to create search corpus output directory `{}`",
                        current.to_string_lossy()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_whatever_context(|_| {
                    format!(
                        "unable to inspect search corpus output directory `{}`",
                        current.to_string_lossy()
                    )
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;
    use url::Url;

    use crate::{
        config::SearchCorpusFormat,
        search::{SearchCorpusConfig, SearchCorpusRequest, DEFAULT_CORPUS_OUTPUT_FILE},
    };

    use super::super::reconcile_corpus;

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

    #[test]
    fn corpus_refuses_to_clean_or_replace_unsupported_targets() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");
        write_file(
            &output,
            "1/index.html",
            r#"<main data-pagefind-body><h2 id="a">A</h2><p>Text.</p></main>"#,
        );
        std::fs::create_dir_all(output.join(DEFAULT_CORPUS_OUTPUT_FILE)).unwrap();

        let error = reconcile_corpus(request(
            output.clone(),
            true,
            SearchCorpusFormat::DocumentsAndChunks,
            DEFAULT_CORPUS_OUTPUT_FILE,
        ))
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported search corpus output path type"));
    }

    #[cfg(unix)]
    #[test]
    fn corpus_refuses_symlink_output_targets() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");
        write_file(
            &output,
            "1/index.html",
            r#"<main data-pagefind-body><h2 id="a">A</h2><p>Text.</p></main>"#,
        );
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, "outside").unwrap();
        std::os::unix::fs::symlink(&outside, output.join(DEFAULT_CORPUS_OUTPUT_FILE)).unwrap();

        let error = reconcile_corpus(request(
            output,
            true,
            SearchCorpusFormat::DocumentsAndChunks,
            DEFAULT_CORPUS_OUTPUT_FILE,
        ))
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported search corpus output path type"));
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
    }

    #[cfg(unix)]
    #[test]
    fn corpus_cleanup_refuses_symlink_parent_directories() {
        let temp = TempDir::new().unwrap();
        let output = temp.path().join("output");
        std::fs::create_dir_all(&output).unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("corpus.json"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, output.join("debug")).unwrap();

        let error = reconcile_corpus(request(
            output,
            false,
            SearchCorpusFormat::DocumentsAndChunks,
            "debug/corpus.json",
        ))
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported directory"));
        assert_eq!(
            std::fs::read_to_string(outside.join("corpus.json")).unwrap(),
            "outside"
        );
    }
}
