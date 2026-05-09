/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Active repository identity selection.

use std::path::Path;

use snafu::{ResultExt, Whatever};

use crate::{
    config::{self, Config, LoadedRepoManifest},
    git,
};

#[derive(Debug, Clone)]
pub(crate) enum ActiveRepoIdentity {
    Manifest(Box<LoadedRepoManifest>),
    Legacy { repo_id: String },
}

impl ActiveRepoIdentity {
    pub(crate) fn load(root_path: &Path) -> Result<Self, Whatever> {
        if let Some(manifest) =
            LoadedRepoManifest::load(root_path).whatever_context("unable to load repo manifest")?
        {
            return Ok(Self::Manifest(Box::new(manifest)));
        }

        match Config::production()
            .locations
            .identify_repository_title(root_path)
        {
            Ok(repo_id) => Ok(Self::Legacy { repo_id }),
            Err(git::Error::NoIdentify { .. }) => {
                snafu::whatever!(
                    "active repository `{}` does not carry `{}` and does not match the legacy EIPs/ERCs identity fallback",
                    root_path.to_string_lossy(),
                    config::REPO_MANIFEST_FILE
                )
            }
            Err(error) => Err(error).whatever_context("cannot identify legacy repository use"),
        }
    }

    pub(crate) fn repo_id(&self) -> &str {
        match self {
            Self::Manifest(manifest) => &manifest.manifest().repo_id,
            Self::Legacy { repo_id } => repo_id,
        }
    }

    pub(crate) fn source_description(&self) -> &'static str {
        match self {
            Self::Manifest(_) => "repo manifest",
            Self::Legacy { .. } => "legacy EIPs/ERCs fallback",
        }
    }

    pub(crate) fn manifest(&self) -> Option<&LoadedRepoManifest> {
        match self {
            Self::Manifest(manifest) => Some(manifest.as_ref()),
            Self::Legacy { .. } => None,
        }
    }

    pub(crate) fn repository_use(&self, staging: bool) -> Result<git::RepositoryUse, Whatever> {
        match self {
            Self::Manifest(manifest) => {
                let manifest = manifest.manifest();
                Ok(git::RepositoryUse {
                    title: manifest.repo_id.clone(),
                    location: manifest.active_endpoint(staging),
                    other_repos: manifest.sibling_repositories(staging),
                })
            }
            Self::Legacy { repo_id } => {
                let baseline = if staging {
                    Config::staging()
                } else {
                    Config::production()
                };
                let Some(repository_use) = baseline.locations.repository_use_for_title(repo_id)
                else {
                    snafu::whatever!("legacy repository metadata for `{repo_id}` is unavailable");
                };
                Ok(repository_use)
            }
        }
    }
}
