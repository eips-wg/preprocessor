/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Proposal metadata JSON generation for theme popovers.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use serde::Serialize;
use snafu::{OptionExt, ResultExt, Whatever};

use crate::{
    layout::CONTENT_DIR,
    proposal::OnlyRenderPlan,
    proposal_catalog::{collect_proposal_catalog, ProposalCatalogPrefix, ProposalCatalogRecord},
};

const PROPOSAL_METADATA_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Serialize)]
struct ProposalMetadataIndex {
    schema_version: u8,
    active_prefix: ProposalCatalogPrefix,
    proposals: BTreeMap<String, ProposalCatalogRecord>,
}

pub(crate) fn write_proposal_metadata_json(
    repo_path: &Path,
    repository_title: &str,
    only_plan: Option<&OnlyRenderPlan>,
) -> Result<(), Whatever> {
    let json_path = proposal_metadata_json_path(repo_path);
    ensure_proposal_metadata_output_available(&json_path)?;

    let metadata = ProposalMetadataIndex {
        schema_version: PROPOSAL_METADATA_SCHEMA_VERSION,
        active_prefix: active_proposal_metadata_prefix(repository_title)?,
        proposals: collect_proposal_catalog(&repo_path.join(CONTENT_DIR), only_plan)?
            .into_records(),
    };

    write_proposal_metadata_file(&json_path, &metadata)
}

fn proposal_metadata_json_path(repo_path: &Path) -> PathBuf {
    repo_path
        .join("static")
        .join("assets")
        .join("data")
        .join("proposals.json")
}

fn ensure_proposal_metadata_output_available(json_path: &Path) -> Result<(), Whatever> {
    match std::fs::metadata(json_path) {
        Ok(_) => {
            snafu::whatever!(
                "proposal metadata output `{}` already exists; refusing to overwrite it",
                json_path.to_string_lossy()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            snafu::whatever!(
                "unable to inspect proposal metadata output `{}`: {error}",
                json_path.to_string_lossy()
            );
        }
    }
}

fn active_proposal_metadata_prefix(
    repository_title: &str,
) -> Result<ProposalCatalogPrefix, Whatever> {
    match repository_title {
        "EIPs" => Ok(ProposalCatalogPrefix::Eip),
        "ERCs" => Ok(ProposalCatalogPrefix::Erc),
        _ => {
            snafu::whatever!(
                "unsupported active repository title `{repository_title}` for proposal metadata; expected `EIPs` or `ERCs`"
            );
        }
    }
}

fn write_proposal_metadata_file(
    json_path: &Path,
    metadata: &ProposalMetadataIndex,
) -> Result<(), Whatever> {
    let parent = json_path.parent().with_whatever_context(|| {
        format!(
            "proposal metadata output path `{}` has no parent directory",
            json_path.to_string_lossy()
        )
    })?;
    std::fs::create_dir_all(parent).with_whatever_context(|_| {
        format!(
            "unable to create proposal metadata directory `{}`",
            parent.to_string_lossy()
        )
    })?;

    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(json_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            snafu::whatever!(
                "proposal metadata output `{}` already exists; refusing to overwrite it",
                json_path.to_string_lossy()
            );
        }
        Err(error) => {
            snafu::whatever!(
                "unable to create proposal metadata output `{}`: {error}",
                json_path.to_string_lossy()
            );
        }
    };

    serde_json::to_writer_pretty(&mut file, metadata).with_whatever_context(|_| {
        format!(
            "unable to write proposal metadata JSON `{}`",
            json_path.to_string_lossy()
        )
    })?;
    file.write_all(b"\n").with_whatever_context(|_| {
        format!(
            "unable to finish proposal metadata JSON `{}`",
            json_path.to_string_lossy()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::{proposal_metadata_json_path, write_proposal_metadata_json};
    use crate::proposal::{OnlyRenderPlan, ProposalNumber};

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

    fn metadata_proposal_markdown(
        number: u32,
        title: &str,
        category: Option<&str>,
        description: Option<&str>,
    ) -> String {
        let description = description
            .map(|description| format!("description: {description}\n"))
            .unwrap_or_default();
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: {title}\n{description}status: Final\ntype: Standards Track\n{category}---\nBody\n"
        )
    }

    fn write_metadata_json(
        repo_path: &Path,
        repository_title: &str,
        only_plan: Option<&OnlyRenderPlan>,
    ) -> Value {
        write_proposal_metadata_json(repo_path, repository_title, only_plan).unwrap();
        serde_json::from_str(
            &std::fs::read_to_string(proposal_metadata_json_path(repo_path)).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn proposal_metadata_full_build_writes_json() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(
                20,
                "Token Standard",
                Some("ERC"),
                Some("A standard interface for tokens."),
            ),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposal = &metadata["proposals"]["erc-20"];

        assert_eq!(metadata["schema_version"], json!(1));
        assert_eq!(metadata["active_prefix"], json!("EIP"));
        assert_eq!(proposal["number"], json!(20));
        assert_eq!(proposal["prefix"], json!("ERC"));
        assert_eq!(proposal["title"], json!("Token Standard"));
        assert_eq!(
            proposal["description"],
            json!("A standard interface for tokens.")
        );
        assert_eq!(proposal["status"], json!("Final"));
        assert_eq!(proposal["type"], json!("Standards Track"));
        assert_eq!(proposal["category"], json!("ERC"));
        assert_eq!(proposal["url"], json!("/20/"));
    }

    #[test]
    fn proposal_metadata_full_build_records_use_local_urls() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", None, None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);

        assert_eq!(metadata["proposals"]["eip-20"]["url"], json!("/20/"));
    }

    #[test]
    fn proposal_metadata_targeted_build_includes_pre_prune_proposals() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", None, None),
        );
        write_file(
            temp.path(),
            "content/021.md",
            &metadata_proposal_markdown(21, "Proposal 21", None, None),
        );
        write_file(
            temp.path(),
            "content/022.md",
            &metadata_proposal_markdown(22, "Proposal 22", Some("ERC"), None),
        );
        let plan = OnlyRenderPlan::build(
            &temp.path().join("content"),
            [number(20)].into_iter().collect(),
        )
        .unwrap();

        let metadata = write_metadata_json(temp.path(), "EIPs", Some(&plan));
        let proposals = metadata["proposals"].as_object().unwrap();

        assert_eq!(proposals.len(), 3);
        assert!(proposals.contains_key("eip-20"));
        assert!(proposals.contains_key("eip-21"));
        assert!(proposals.contains_key("erc-22"));
    }

    #[test]
    fn proposal_metadata_targeted_selected_records_use_local_urls() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", None, None),
        );
        write_file(
            temp.path(),
            "content/021.md",
            &metadata_proposal_markdown(21, "Proposal 21", None, None),
        );
        let plan = OnlyRenderPlan::build(
            &temp.path().join("content"),
            [number(20)].into_iter().collect(),
        )
        .unwrap();

        let metadata = write_metadata_json(temp.path(), "EIPs", Some(&plan));

        assert_eq!(metadata["proposals"]["eip-20"]["url"], json!("/20/"));
    }

    #[test]
    fn proposal_metadata_targeted_omitted_records_use_public_urls() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", None, None),
        );
        write_file(
            temp.path(),
            "content/021.md",
            &metadata_proposal_markdown(21, "Proposal 21", None, None),
        );
        write_file(
            temp.path(),
            "content/022.md",
            &metadata_proposal_markdown(22, "Proposal 22", Some("ERC"), None),
        );
        let plan = OnlyRenderPlan::build(
            &temp.path().join("content"),
            [number(20)].into_iter().collect(),
        )
        .unwrap();

        let metadata = write_metadata_json(temp.path(), "EIPs", Some(&plan));

        assert_eq!(
            metadata["proposals"]["eip-21"]["url"],
            json!("https://eips.ethereum.org/EIPS/eip-21")
        );
        assert_eq!(
            metadata["proposals"]["erc-22"]["url"],
            json!("https://ercs.ethereum.org/ERCS/erc-22")
        );
    }

    #[test]
    fn proposal_metadata_active_prefix_comes_from_repository_title() {
        for (repository_title, expected_prefix) in [("EIPs", "EIP"), ("ERCs", "ERC")] {
            let temp = TempDir::new().unwrap();
            write_file(
                temp.path(),
                "content/001.md",
                &metadata_proposal_markdown(1, "Proposal 1", None, None),
            );

            let metadata = write_metadata_json(temp.path(), repository_title, None);

            assert_eq!(metadata["active_prefix"], json!(expected_prefix));
        }
    }

    #[test]
    fn proposal_metadata_erc_category_produces_erc_prefix_and_key() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", Some("ERC"), None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposals = metadata["proposals"].as_object().unwrap();

        assert!(proposals.contains_key("erc-20"));
        assert_eq!(metadata["proposals"]["erc-20"]["prefix"], json!("ERC"));
    }

    #[test]
    fn proposal_metadata_non_erc_category_and_default_produce_eip_prefix_and_key() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "Proposal 20", Some("Core"), None),
        );
        write_file(
            temp.path(),
            "content/021.md",
            &metadata_proposal_markdown(21, "Proposal 21", None, None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposals = metadata["proposals"].as_object().unwrap();

        assert!(proposals.contains_key("eip-20"));
        assert!(proposals.contains_key("eip-21"));
        assert_eq!(metadata["proposals"]["eip-20"]["prefix"], json!("EIP"));
        assert_eq!(metadata["proposals"]["eip-21"]["prefix"], json!("EIP"));
    }

    #[test]
    fn proposal_metadata_missing_optional_description_is_omitted() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/001.md",
            &metadata_proposal_markdown(1, "Proposal 1", None, None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposal = metadata["proposals"]["eip-1"].as_object().unwrap();

        assert!(!proposal.contains_key("description"));
    }

    #[test]
    fn proposal_metadata_malformed_preamble_fails_clearly() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/001.md", "not front matter\n");

        let error = write_proposal_metadata_json(temp.path(), "EIPs", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("couldn't split preamble"));
        assert!(error.contains("001.md"));
    }

    #[test]
    fn proposal_metadata_missing_required_field_fails_clearly() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/001.md",
            "---\neip: 1\ntitle: Proposal 1\ntype: Standards Track\n---\nBody\n",
        );

        let error = write_proposal_metadata_json(temp.path(), "EIPs", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing required proposal metadata field `status`"));
        assert!(error.contains("001.md"));
    }

    #[test]
    fn proposal_metadata_path_and_preamble_number_mismatch_fails_clearly() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/001.md",
            "---\neip: 2\ntitle: Proposal 1\nstatus: Final\ntype: Standards Track\n---\nBody\n",
        );

        let error = write_proposal_metadata_json(temp.path(), "EIPs", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("proposal metadata number mismatch"));
        assert!(error.contains("001.md"));
        assert!(error.contains("path indicates `1`"));
        assert!(error.contains("`eip` contains `2`"));
    }

    #[test]
    fn proposal_metadata_allows_eip_and_erc_keys_for_same_number() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "ERC 20", Some("ERC"), None),
        );
        write_file(
            temp.path(),
            "content/20/index.md",
            &metadata_proposal_markdown(20, "EIP 20", None, None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposals = metadata["proposals"].as_object().unwrap();

        assert_eq!(proposals.len(), 2);
        assert!(proposals.contains_key("eip-20"));
        assert!(proposals.contains_key("erc-20"));
    }

    #[test]
    fn proposal_metadata_duplicate_same_key_fails_with_both_paths() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/020.md",
            &metadata_proposal_markdown(20, "ERC 20", Some("ERC"), None),
        );
        write_file(
            temp.path(),
            "content/20/index.md",
            &metadata_proposal_markdown(20, "Duplicate ERC 20", Some("ERC"), None),
        );

        let error = write_proposal_metadata_json(temp.path(), "EIPs", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("duplicate proposal metadata key `erc-20`"));
        assert!(error.contains("020.md"));
        assert!(error.contains("20/index.md"));
    }

    #[test]
    fn proposal_metadata_existing_output_collision_fails_loudly() {
        let temp = TempDir::new().unwrap();
        write_file(
            temp.path(),
            "content/001.md",
            &metadata_proposal_markdown(1, "Proposal 1", None, None),
        );
        write_file(
            temp.path(),
            "static/assets/data/proposals.json",
            "{\"existing\":true}\n",
        );

        let error = write_proposal_metadata_json(temp.path(), "EIPs", None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("already exists"));
        assert_eq!(
            std::fs::read_to_string(proposal_metadata_json_path(temp.path())).unwrap(),
            "{\"existing\":true}\n"
        );
    }

    #[test]
    fn proposal_metadata_unsupported_markdown_paths_are_ignored() {
        let temp = TempDir::new().unwrap();
        write_file(temp.path(), "content/_index.md", "not front matter\n");
        write_file(temp.path(), "content/foo.md", "not front matter\n");
        write_file(temp.path(), "content/001/readme.md", "not front matter\n");
        write_file(
            temp.path(),
            "content/001/assets/readme.md",
            "not front matter\n",
        );
        write_file(
            temp.path(),
            "content/002.md",
            &metadata_proposal_markdown(2, "Proposal 2", None, None),
        );

        let metadata = write_metadata_json(temp.path(), "EIPs", None);
        let proposals = metadata["proposals"].as_object().unwrap();

        assert_eq!(proposals.len(), 1);
        assert!(proposals.contains_key("eip-2"));
    }
}
