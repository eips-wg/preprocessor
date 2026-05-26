/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

//! Prepared Zola runtime pipeline.

use std::path::{Path, PathBuf};

use snafu::{OptionExt, ResultExt, Whatever};
use url::Url;

use crate::{
    config::ServerBinding,
    execution::ResolvedExecution,
    git,
    layout::{mounted_theme_path, output_path, CONTENT_DIR, REPO_DIR},
    markdown,
    proposal::OnlyRenderPlan,
    proposal_metadata,
    search::{
        self, SearchConfig, SearchIndexRequest, SearchIndexSummary, SearchRouteSummary,
        SearchTemplateState,
    },
    serve::{serve_sync_config, DirtyServeWatcher, LocalThemeServeSync},
    zola,
};

fn prepare_theme_for_zola(
    theme_path: PathBuf,
    repo_path: &Path,
) -> Result<(PathBuf, LocalThemeServeSync), Whatever> {
    let mounted_theme_dir = mounted_theme_path(repo_path);
    git::materialize_working_tree(&theme_path, &mounted_theme_dir)
        .whatever_context("unable to materialize workspace-local theme")?;
    let theme_index_path = git::index_path(&theme_path)
        .whatever_context("unable to resolve workspace-local theme Git index path")?;

    Ok((
        mounted_theme_dir.clone(),
        LocalThemeServeSync {
            theme_source_root: theme_path,
            mounted_theme_dir,
            theme_index_path,
        },
    ))
}

fn prepare_runtime_source(
    root_path: &Path,
    repo_path: &Path,
    repository_use: &git::RepositoryUse,
    source_materialization: git::SourceMaterialization,
) -> Result<(), Whatever> {
    let source = git::Fresh::new(
        root_path,
        repo_path,
        repository_use.clone(),
        source_materialization,
    )
    .whatever_context("initializing build repo")?
    .clone_src()
    .whatever_context("cloning source repo")?;

    source
        .merge()
        .whatever_context("unable to merge ERC/EIP repositories")?;

    Ok(())
}

#[derive(Debug)]
pub(crate) struct Prepared {
    repo_path: PathBuf,
    output_path: PathBuf,
    repository_use: git::RepositoryUse,
    theme_path: PathBuf,
    local_theme_sync: Option<LocalThemeServeSync>,
    only_plan: Option<OnlyRenderPlan>,
    source_root: PathBuf,
    source_materialization: git::SourceMaterialization,
    server_binding: ServerBinding,
    base_url_override: Option<Url>,
    search: SearchConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchIndexMode {
    Build,
    #[cfg(test)]
    Check,
    #[cfg(test)]
    Serve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchRouteMode {
    Build,
    Check,
    Serve,
}

fn should_index_search(mode: SearchIndexMode, search: SearchConfig) -> bool {
    matches!(mode, SearchIndexMode::Build) && search.pagefind
}

fn search_route_enabled(mode: SearchRouteMode, search: SearchConfig) -> bool {
    matches!(mode, SearchRouteMode::Build) && search.pagefind
}

fn run_search_index(
    mode: SearchIndexMode,
    output_path: &Path,
    search_config: SearchConfig,
) -> Result<Option<SearchIndexSummary>, Whatever> {
    if !should_index_search(mode, search_config) {
        return Ok(None);
    }

    search::index_site(SearchIndexRequest {
        output_path: output_path.to_path_buf(),
    })
    .map(Some)
    .whatever_context("search indexing failed")
}

impl Prepared {
    pub(crate) fn prepare(resolved: ResolvedExecution) -> Result<Self, Whatever> {
        zola::find_zola().whatever_context("unable to find suitable zola binary")?;

        let ResolvedExecution {
            root_path,
            build_path,
            repository_use,
            theme_path,
            only,
            source_materialization,
            server_binding,
            base_url_override,
            search,
        } = resolved;
        let theme_path =
            theme_path.whatever_context("Zola runtime requires a workspace-local theme path")?;

        let repo_path = build_path.join(REPO_DIR);
        let content_path = repo_path.join(CONTENT_DIR);
        let output_path = output_path(&build_path);

        prepare_runtime_source(
            &root_path,
            &repo_path,
            &repository_use,
            source_materialization,
        )?;
        search::ensure_search_route_available(&content_path)
            .whatever_context("unable to reserve generated search route")?;

        let only_plan = only
            .map(|selected_numbers| OnlyRenderPlan::build(&content_path, selected_numbers))
            .transpose()
            .whatever_context("unable to build targeted render plan")?;
        proposal_metadata::write_proposal_metadata_json(
            &repo_path,
            &repository_use.title,
            only_plan.as_ref(),
        )
        .whatever_context("unable to write proposal metadata JSON")?;
        markdown::preprocess(&content_path, only_plan.as_ref())
            .whatever_context("unable to preprocess markdown")?;
        if let Some(only_plan) = &only_plan {
            only_plan
                .prune_content(&content_path)
                .whatever_context("unable to prune unselected proposals")?;
        }
        let (theme_path, local_theme_sync) = prepare_theme_for_zola(theme_path, &repo_path)?;

        Ok(Prepared {
            repository_use,
            theme_path,
            local_theme_sync: Some(local_theme_sync),
            repo_path,
            output_path,
            only_plan,
            source_root: root_path,
            source_materialization,
            server_binding,
            base_url_override,
            search,
        })
    }

    pub(crate) fn build(self) -> Result<(), Whatever> {
        let base_url = self.resolved_base_url().clone();
        let _ = self.write_search_route_for_zola(SearchRouteMode::Build)?;
        zola::build(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            base_url.as_str(),
        )
        .whatever_context("zola build failed")?;
        let _ = run_search_index(SearchIndexMode::Build, &self.output_path, self.search)?;
        Ok(())
    }

    pub(crate) fn serve(self) -> Result<(), Whatever> {
        let _ = self.write_search_route_for_zola(SearchRouteMode::Serve)?;
        let sync_config = serve_sync_config(
            self.source_materialization,
            &self.source_root,
            &self.repo_path,
            self.only_plan.clone(),
            self.local_theme_sync.clone(),
        );
        let dirty_watcher = if sync_config.has_targets() {
            Some(
                DirtyServeWatcher::start(sync_config)
                    .whatever_context("unable to start dirty serve watcher")?,
            )
        } else {
            None
        };

        let result = zola::serve(
            &self.theme_path,
            &self.repo_path,
            &self.output_path,
            &self.server_binding,
            self.base_url_override.as_ref(),
        )
        .whatever_context("zola serve failed");

        if let Some(dirty_watcher) = dirty_watcher {
            dirty_watcher.stop();
        }

        result
    }

    pub(crate) fn check(self) -> Result<(), Whatever> {
        let _ = self.write_search_route_for_zola(SearchRouteMode::Check)?;
        zola::check(&self.theme_path, &self.repo_path).whatever_context("zola check failed")?;
        Ok(())
    }

    fn resolved_base_url(&self) -> &Url {
        self.base_url_override
            .as_ref()
            .unwrap_or(&self.repository_use.location.base_url)
    }

    fn search_template_state(&self, mode: SearchRouteMode) -> SearchTemplateState {
        SearchTemplateState::from_base_url(
            search_route_enabled(mode, self.search),
            self.resolved_base_url(),
        )
    }

    fn write_search_route_for_zola(
        &self,
        mode: SearchRouteMode,
    ) -> Result<SearchRouteSummary, Whatever> {
        search::write_search_route(&self.repo_path, self.search_template_state(mode))
            .whatever_context("unable to write generated search route")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use git2::{IndexAddOption, Repository, Signature};
    use snafu::Report;
    use tempfile::TempDir;
    use url::Url;

    use crate::{
        changed,
        cli::{Args, ChangedFormat, EditorialCommand, RuntimeOperation},
        config,
        editorial::editorial_runtime_execution,
        execution::{resolve_execution, ResolvedExecution},
        git::SourceMaterialization,
        layout::{mounted_theme_path, theme_config_path, CONTENT_DIR, REPO_DIR},
        proposal_catalog::collect_proposal_catalog,
        search::{SearchConfig, SEARCH_DATA_FILE},
    };

    use super::{
        prepare_runtime_source, prepare_theme_for_zola, run_search_index, search_route_enabled,
        should_index_search, Prepared, SearchIndexMode, SearchRouteMode,
    };

    struct RuntimeWorkspace {
        _temp: TempDir,
        active_path: PathBuf,
    }

    fn write_file(root: &Path, relative: impl AsRef<Path>, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn commit_all(repo: &Repository, message: &str) {
        let mut index = repo.index().unwrap();
        index
            .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
            .unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let signature = Signature::now("build-eips test", "build-eips@example.test").unwrap();
        let parents = repo
            .head()
            .ok()
            .and_then(|head| head.target())
            .map(|oid| repo.find_commit(oid).unwrap())
            .into_iter()
            .collect::<Vec<_>>();
        let parent_refs = parents.iter().collect::<Vec<_>>();

        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parent_refs,
        )
        .unwrap();
    }

    fn init_repo(path: &Path, files: &[(&str, &str)]) -> Repository {
        std::fs::create_dir_all(path).unwrap();
        let repo = Repository::init(path).unwrap();
        repo.set_head("refs/heads/master").unwrap();
        for (relative, contents) in files {
            write_file(path, relative, contents);
        }
        commit_all(&repo, "initial");
        repo
    }

    fn file_url(path: &Path) -> Url {
        Url::from_directory_path(path).unwrap()
    }

    fn repo_manifest_text(repo_id: &str, repository: &Url, siblings: &[(&str, Url)]) -> String {
        let mut manifest = format!(
            r#"
repo_id = "{repo_id}"

[production]
repository = "{repository}"
base_url = "https://example.test/{repo_id}/"

[staging]
repository = "{repository}"
base_url = "https://staging.example.test/{repo_id}/"
"#
        );

        for (sibling_id, sibling_repository) in siblings {
            manifest.push_str(&format!(
                r#"
[siblings.{sibling_id}.production]
repository = "{sibling_repository}"
base_url = "https://example.test/{sibling_id}/"

[siblings.{sibling_id}.staging]
repository = "{sibling_repository}"
base_url = "https://staging.example.test/{sibling_id}/"
"#
            ));
        }

        manifest
    }

    fn pipeline_proposal_markdown(number: u32, category: Option<&str>, body: &str) -> String {
        let category = category
            .map(|category| format!("category: {category}\n"))
            .unwrap_or_default();
        format!(
            "---\neip: {number}\ntitle: Proposal {number}\nstatus: Draft\ntype: Standards Track\n{category}---\n\n{body}\n"
        )
    }

    fn runtime_workspace(with_sibling: bool) -> RuntimeWorkspace {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("EIPs");
        let sibling_path = workspace_root.join("ERCs");
        let missing_upstream = file_url(&temp.path().join("missing-upstream"));
        let siblings = with_sibling.then(|| ("ERCs", file_url(&sibling_path)));
        let siblings = siblings.into_iter().collect::<Vec<_>>();
        let manifest = repo_manifest_text("EIPs", &missing_upstream, &siblings);

        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        std::fs::create_dir_all(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        let _active_repo = init_repo(
            &active_path,
            &[
                (config::REPO_MANIFEST_FILE, manifest.as_str()),
                ("content/00001.md", "active proposal\n"),
            ],
        );

        if with_sibling {
            let _sibling_repo = init_repo(&sibling_path, &[("content/00002.md", "sibling\n")]);
        }

        RuntimeWorkspace {
            _temp: temp,
            active_path,
        }
    }

    fn runtime_workspace_with_theme(active_files: &[(&str, &str)]) -> RuntimeWorkspace {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("EIPs");
        let theme_path = workspace_root.join("theme");
        let missing_upstream = file_url(&temp.path().join("missing-upstream"));
        let manifest = repo_manifest_text("EIPs", &missing_upstream, &[]);

        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        init_repo(
            &theme_path,
            &[
                (
                    "config/zola.toml",
                    "title = 'theme'\nbase_url = 'https://example.test/'\ntheme = 'eips-theme'\n",
                ),
                ("theme.toml", "name = 'eips-theme'\n"),
                ("templates/page.html", "{{ page.content | safe }}\n"),
            ],
        );

        std::fs::create_dir_all(&active_path).unwrap();
        let active_repo = Repository::init(&active_path).unwrap();
        active_repo.set_head("refs/heads/master").unwrap();
        write_file(&active_path, config::REPO_MANIFEST_FILE, manifest.as_str());
        for (relative, contents) in active_files {
            write_file(&active_path, relative, contents);
        }
        commit_all(&active_repo, "initial");

        RuntimeWorkspace {
            _temp: temp,
            active_path,
        }
    }

    fn resolved_runtime(workspace: &RuntimeWorkspace, command: &[&str]) -> ResolvedExecution {
        let active_path = workspace.active_path.to_str().unwrap();
        let mut arguments = vec!["build-eips", "-C", active_path];
        arguments.extend_from_slice(command);
        let args = Args::try_parse_from(arguments).unwrap();

        resolve_execution(&args).unwrap()
    }

    fn prepare_resolved_source(resolved: &ResolvedExecution) -> Result<(), snafu::Whatever> {
        std::fs::create_dir_all(&resolved.build_path).unwrap();
        let repo_path = resolved.build_path.join(REPO_DIR);
        prepare_runtime_source(
            &resolved.root_path,
            &repo_path,
            &resolved.repository_use,
            resolved.source_materialization,
        )
    }

    fn prepared_path(resolved: &ResolvedExecution, relative: impl AsRef<Path>) -> PathBuf {
        resolved.build_path.join(REPO_DIR).join(relative)
    }

    fn rendered_front_matter(path: &Path) -> toml::Value {
        let contents = std::fs::read_to_string(path).unwrap();
        let front_matter = contents
            .strip_prefix("+++\n")
            .unwrap()
            .split_once("\n+++\n")
            .unwrap()
            .0;
        toml::from_str(front_matter).unwrap()
    }

    #[test]
    fn workspace_local_theme_is_materialized_as_mounted_theme_for_zola() {
        let temp = TempDir::new().unwrap();
        let theme_root = temp.path().join("workspace/theme");
        init_repo(
            &theme_root,
            &[
                ("config/zola.toml", "title = 'theme'\n"),
                ("templates/index.html", "local theme\n"),
            ],
        );
        let repo_path = temp.path().join("workspace/.local-build/Core/repo");

        let (theme_path, sync) = prepare_theme_for_zola(theme_root.clone(), &repo_path).unwrap();

        let mounted_theme_dir = mounted_theme_path(&repo_path);
        assert_eq!(theme_path, mounted_theme_dir);
        assert_eq!(
            theme_config_path(&mounted_theme_dir),
            repo_path.join("themes/eips-theme/config/zola.toml")
        );
        assert_eq!(
            std::fs::read_to_string(mounted_theme_dir.join("templates/index.html")).unwrap(),
            "local theme\n"
        );
        assert_eq!(sync.theme_source_root, theme_root);
        assert_eq!(sync.mounted_theme_dir, mounted_theme_dir);
        assert!(sync.theme_index_path.ends_with(".git/index"));
    }

    #[test]
    fn prepared_runtime_source_succeeds_with_unreachable_active_upstream() {
        for command in [&["build"][..], &["check"][..], &["serve"][..]] {
            let workspace = runtime_workspace(false);
            let resolved = resolved_runtime(&workspace, command);

            prepare_resolved_source(&resolved).unwrap();

            assert_eq!(
                std::fs::read_to_string(prepared_path(&resolved, "content/00001.md")).unwrap(),
                "active proposal\n"
            );
        }
    }

    #[test]
    fn prepared_runtime_source_uses_remote_siblings_without_active_upstream_fetch() {
        let workspace = runtime_workspace(true);
        let resolved = resolved_runtime(&workspace, &["--remote-siblings", "build"]);

        prepare_resolved_source(&resolved).unwrap();

        assert_eq!(
            std::fs::read_to_string(prepared_path(&resolved, "content/00001.md")).unwrap(),
            "active proposal\n"
        );
        assert_eq!(
            std::fs::read_to_string(prepared_path(&resolved, "content/00002.md")).unwrap(),
            "sibling\n"
        );
    }

    #[test]
    fn remote_environment_runtime_source_prep_keeps_local_active_checkout() {
        for command in [
            &["--staging", "build"][..],
            &["--production", "check"][..],
            &["parity", "serve"][..],
        ] {
            let workspace = runtime_workspace(false);
            let resolved = resolved_runtime(&workspace, command);

            assert_eq!(
                resolved.source_materialization,
                SourceMaterialization::Clean
            );
            prepare_resolved_source(&resolved).unwrap();

            assert_eq!(
                std::fs::read_to_string(prepared_path(&resolved, "content/00001.md")).unwrap(),
                "active proposal\n"
            );
        }
    }

    #[test]
    fn changed_still_requires_active_upstream() {
        let workspace = runtime_workspace(false);
        let resolved = resolved_runtime(&workspace, &["changed"]);
        std::fs::create_dir_all(&resolved.build_path).unwrap();

        let error = changed::run(
            &resolved,
            &resolved.build_path,
            false,
            &ChangedFormat::Newline,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("fetching upstream repo"));
    }

    #[test]
    fn editorial_check_site_phase_source_prep_does_not_fetch_active_upstream() {
        let workspace = runtime_workspace(false);
        let active_path = workspace.active_path.to_str().unwrap();
        let args = Args::try_parse_from([
            "build-eips",
            "-C",
            active_path,
            "editorial",
            "check",
            "--against-upstream",
        ])
        .unwrap();
        let resolved = resolve_execution(&args).unwrap();
        let RuntimeOperation::Editorial {
            command: EditorialCommand::Check { selectors, .. },
        } = args.operation.runtime_operation().unwrap()
        else {
            panic!("expected editorial check runtime operation");
        };
        let resolved = editorial_runtime_execution(resolved, &selectors);

        prepare_resolved_source(&resolved).unwrap();

        assert_eq!(
            std::fs::read_to_string(prepared_path(&resolved, "content/00001.md")).unwrap(),
            "active proposal\n"
        );
    }

    #[test]
    fn proposal_catalog_collection_uses_prepared_merged_sources() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path().join("workspace");
        let active_path = workspace_root.join("EIPs");
        let sibling_path = workspace_root.join("ERCs");
        let active_url = file_url(&active_path);
        let sibling_url = file_url(&sibling_path);
        let manifest = repo_manifest_text("EIPs", &active_url, &[("ERCs", sibling_url)]);
        let active_markdown = pipeline_proposal_markdown(1, None, "Active proposal.");
        let sibling_markdown = pipeline_proposal_markdown(2, Some("ERC"), "Sibling proposal.");

        write_file(&workspace_root, config::LOCAL_CONFIG_FILE, "");
        std::fs::create_dir_all(workspace_root.join(config::DEFAULT_THEME_DIR)).unwrap();
        let _active_repo = init_repo(
            &active_path,
            &[
                (config::REPO_MANIFEST_FILE, manifest.as_str()),
                ("content/00001.md", active_markdown.as_str()),
            ],
        );
        let _sibling_repo = init_repo(
            &sibling_path,
            &[("content/00002.md", sibling_markdown.as_str())],
        );
        let workspace = RuntimeWorkspace {
            _temp: temp,
            active_path,
        };
        let resolved = resolved_runtime(&workspace, &["build"]);

        prepare_resolved_source(&resolved).unwrap();
        let catalog =
            collect_proposal_catalog(&prepared_path(&resolved, CONTENT_DIR), None).unwrap();
        let records = catalog.into_records();

        assert!(!resolved.root_path.join("content/00002.md").exists());
        assert_eq!(records["erc-2"].title, "Proposal 2");
    }

    #[test]
    fn search_index_policy_runs_only_for_enabled_builds() {
        let enabled = SearchConfig { pagefind: true };
        let disabled = SearchConfig { pagefind: false };

        assert!(should_index_search(SearchIndexMode::Build, enabled));
        assert!(!should_index_search(SearchIndexMode::Build, disabled));
        assert!(!should_index_search(SearchIndexMode::Check, enabled));
        assert!(!should_index_search(SearchIndexMode::Serve, enabled));
    }

    #[test]
    fn search_route_state_policy_matches_runtime_command() {
        let enabled = SearchConfig { pagefind: true };
        let disabled = SearchConfig { pagefind: false };

        assert!(search_route_enabled(SearchRouteMode::Build, enabled));
        assert!(!search_route_enabled(SearchRouteMode::Build, disabled));
        assert!(!search_route_enabled(SearchRouteMode::Check, enabled));
        assert!(!search_route_enabled(SearchRouteMode::Serve, enabled));
    }

    #[test]
    fn search_route_collisions_are_detected_in_materialized_content() {
        for (relative, expected) in [
            ("content/search.md", "content/search.md"),
            ("content/search/placeholder.txt", "content/search"),
            ("content/search/index.md", "content/search/index.md"),
            ("content/search/_index.md", "content/search/_index.md"),
        ] {
            let workspace = runtime_workspace_with_theme(&[(relative, "not a proposal\n")]);
            let resolved = resolved_runtime(&workspace, &["build"]);

            let error = Report::from_error(Prepared::prepare(resolved).unwrap_err()).to_string();

            assert!(error.contains("unable to reserve generated search route"));
            assert!(error.contains("collides with the generated `/search/` route"));
            assert!(error.contains(expected));
        }
    }

    #[test]
    fn generated_search_route_is_written_to_prepared_repo_not_source_worktree() {
        let proposal = pipeline_proposal_markdown(1, None, "Active proposal.");
        let workspace = runtime_workspace_with_theme(&[("content/00001.md", proposal.as_str())]);
        let resolved = resolved_runtime(&workspace, &["build"]);
        let source_search_route = workspace.active_path.join("content/search.md");

        let prepared = Prepared::prepare(resolved).unwrap();
        let summary = prepared
            .write_search_route_for_zola(SearchRouteMode::Build)
            .unwrap();

        assert_eq!(
            summary.route_path,
            prepared.repo_path.join("content/search.md")
        );
        assert_eq!(
            summary.data_path,
            prepared.repo_path.join("data").join(SEARCH_DATA_FILE)
        );
        assert!(!source_search_route.exists());
        assert!(summary.route_path.is_file());
        assert!(summary.data_path.is_file());

        let front_matter = rendered_front_matter(&summary.route_path);
        assert_eq!(front_matter["template"].as_str(), Some("search.html"));
        assert_eq!(
            front_matter["extra"]["search"]["enabled"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn targeted_build_writes_search_route_after_preprocess_and_prune() {
        let selected = pipeline_proposal_markdown(555, None, "Selected proposal.");
        let unselected = pipeline_proposal_markdown(678, Some("ERC"), "Unselected proposal.");
        let workspace = runtime_workspace_with_theme(&[
            ("content/00555.md", selected.as_str()),
            ("content/00678.md", unselected.as_str()),
        ]);
        let resolved = resolved_runtime(&workspace, &["build", "--only", "555"]);

        let prepared = Prepared::prepare(resolved).unwrap();
        let selected_path = prepared.repo_path.join("content/00555.md");
        let unselected_path = prepared.repo_path.join("content/00678.md");
        let search_route_path = prepared.repo_path.join("content/search.md");
        let search_data_path = prepared.repo_path.join("data").join(SEARCH_DATA_FILE);

        assert!(selected_path.is_file());
        assert!(!unselected_path.exists());
        assert_eq!(
            rendered_front_matter(&selected_path)["extra"]["number"].as_integer(),
            Some(555)
        );
        assert!(!search_route_path.exists());
        assert!(!search_data_path.exists());

        let summary = prepared
            .write_search_route_for_zola(SearchRouteMode::Build)
            .unwrap();

        assert_eq!(summary.route_path, search_route_path);
        assert_eq!(summary.data_path, search_data_path);
        assert!(summary.route_path.is_file());
        assert!(summary.data_path.is_file());
    }

    #[test]
    fn search_template_state_uses_resolved_runtime_base_url() {
        let proposal = pipeline_proposal_markdown(1, None, "Active proposal.");
        let repository_workspace =
            runtime_workspace_with_theme(&[("content/00001.md", proposal.as_str())]);

        let prepared =
            Prepared::prepare(resolved_runtime(&repository_workspace, &["build"])).unwrap();
        let repository_state = prepared.search_template_state(SearchRouteMode::Build);
        assert!(repository_state.enabled);
        assert_eq!(repository_state.base_path, "/EIPs/");
        assert_eq!(
            repository_state.bundle_path.as_deref(),
            Some("/EIPs/pagefind/")
        );

        let override_workspace =
            runtime_workspace_with_theme(&[("content/00001.md", proposal.as_str())]);
        let prepared = Prepared::prepare(resolved_runtime(
            &override_workspace,
            &["build", "--base-url", "https://wg-eips.ritovision.com/"],
        ))
        .unwrap();
        let override_state = prepared.search_template_state(SearchRouteMode::Build);
        assert!(override_state.enabled);
        assert_eq!(override_state.base_path, "/");
        assert_eq!(override_state.bundle_path.as_deref(), Some("/pagefind/"));
    }

    #[test]
    fn generated_search_route_uses_disabled_state_for_disabled_builds_and_serve() {
        let proposal = pipeline_proposal_markdown(1, None, "Active proposal.");
        let workspace = runtime_workspace_with_theme(&[("content/00001.md", proposal.as_str())]);
        let mut resolved = resolved_runtime(&workspace, &["build"]);
        resolved.search = SearchConfig { pagefind: false };
        let prepared = Prepared::prepare(resolved).unwrap();

        let build_state = prepared.search_template_state(SearchRouteMode::Build);
        let serve_state = prepared.search_template_state(SearchRouteMode::Serve);
        let check_state = prepared.search_template_state(SearchRouteMode::Check);

        assert!(!build_state.enabled);
        assert!(build_state.bundle_path.is_none());
        assert!(!serve_state.enabled);
        assert!(serve_state.bundle_path.is_none());
        assert!(!check_state.enabled);
        assert!(check_state.bundle_path.is_none());
    }

    #[test]
    fn disabled_build_search_hook_leaves_stale_pagefind_assets_untouched() {
        let temp = TempDir::new().unwrap();
        let output_path = temp.path().join("output");
        write_file(&output_path, "pagefind/stale.txt", "stale");

        let summary = run_search_index(
            SearchIndexMode::Build,
            &output_path,
            SearchConfig { pagefind: false },
        )
        .unwrap();

        assert!(summary.is_none());
        assert_eq!(
            std::fs::read_to_string(output_path.join("pagefind/stale.txt")).unwrap(),
            "stale"
        );
    }
}
