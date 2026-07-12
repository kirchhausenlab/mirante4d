use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use syn::visit::Visit;

mod wp08a;
mod wp10a;

const MAX_TRACKED_GENERATED_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;
const WP07A_MODEL_CONTRACT_PATH: &str = "architecture/model-contract.json";
const WP07A_MODEL_CONTRACT_SHA256: &str =
    "095dc1c2b96ea70a893d385c3bf08ccd4c204c897156109c9e95b09ead7c5f0b";
const FORBIDDEN_DUMPING_GROUND_MODULE_NAMES: &[&str] =
    &["common.rs", "helpers.rs", "misc.rs", "utils.rs"];
const FORBIDDEN_AXIS_ALIGNED_2D_CHUNK_PATTERNS: &[&str] = &[
    "(512,512,1)",
    "(512, 512, 1)",
    "(512,1,512)",
    "(512, 1, 512)",
    "(1,512,512)",
    "(1, 512, 512)",
    "512x512x1",
    "512x1x512",
    "1x512x512",
    "slice_chunk",
    "slice_chunks",
    "SliceChunk",
    "SliceChunks",
];
const ALLOWED_LOCAL_CARGO_OVERRIDES: &[(&str, &str)] =
    &[("wayland-scanner", "vendor/wayland-scanner")];
const WP07B_BOUNDARY_CRATES: &[&str] = &[
    "mirante4d-application",
    "mirante4d-dataset",
    "mirante4d-render-api",
    "mirante4d-settings",
];

pub(crate) fn architecture_self_check() -> anyhow::Result<()> {
    let required = [
        "crates/mirante4d-analysis",
        "crates/mirante4d-format",
        "crates/mirante4d-import",
        "crates/mirante4d-data",
        "crates/mirante4d-dataset",
        "crates/mirante4d-domain",
        "crates/mirante4d-identity",
        "crates/mirante4d-project-model",
        "crates/mirante4d-render-api",
        "crates/mirante4d-renderer",
        "crates/mirante4d-settings",
        "crates/mirante4d-application",
        "crates/mirante4d-app",
        "crates/xtask",
    ];
    for path in required {
        if !Path::new(path).is_dir() {
            bail!("required crate directory is missing: {path}");
        }
    }
    for forbidden in ["crates/mirante4d-core", "crates/mirante4d-preprocess"] {
        if Path::new(forbidden).exists() {
            bail!("first milestone must not create empty future crate: {forbidden}");
        }
    }
    wp08a::check_wp08a_subsystem_contract(Path::new("."))?;
    wp10a::check_wp10a_storage_contract(Path::new("."))?;
    check_source_architecture_policy()?;
    check_wp07a_contracts(Path::new("."))?;
    check_wp07b_boundary_contract(Path::new("."))?;
    check_current_state_field_ledger(Path::new("."))?;
    check_tracked_artifact_policy()?;
    Ok(())
}

fn check_source_architecture_policy() -> anyhow::Result<()> {
    let mut violations = Vec::new();
    for path in collect_rust_source_files(Path::new("crates"))? {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read source file {}", path.display()))?;
        violations.extend(source_architecture_violations(&path, &source));
    }
    if !violations.is_empty() {
        bail!(
            "source architecture policy failed:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

fn source_architecture_violations(path: &Path, source: &str) -> Vec<String> {
    let normalized = normalize_repo_path(path);
    let mut violations = Vec::new();
    if let Some(violation) = dumping_ground_module_name_violation(path) {
        violations.push(violation);
    }
    if normalized.starts_with("crates/xtask/") {
        return violations;
    }
    violations.extend(axis_aligned_2d_chunk_dependency_violations(path, source));
    let app_or_app_test = normalized.starts_with("crates/mirante4d-app/");
    if !app_or_app_test {
        violations.extend(source_pattern_violations(
            path,
            source,
            &[
                "eframe::",
                "egui::",
                "egui_kittest",
                "mirante4d_app",
                "rfd::",
            ],
            "non-app crate must not import UI/app layer",
        ));
    }
    if normalized.starts_with("crates/mirante4d-renderer/src/") {
        violations.extend(source_pattern_violations(
            path,
            source,
            &[
                "std::fs",
                "fs::",
                "File::open",
                "File::create",
                "OpenOptions",
                "read_to_string",
                "read_dir",
            ],
            "renderer source must not perform direct filesystem I/O",
        ));
    }
    if [
        "crates/mirante4d-domain/src/",
        "crates/mirante4d-identity/src/",
        "crates/mirante4d-project-model/src/",
    ]
    .iter()
    .any(|prefix| normalized.starts_with(prefix))
    {
        violations.extend(source_pattern_violations(
            path,
            source,
            &[
                "std::env",
                "std::fs",
                "std::io",
                "std::net",
                "std::os",
                "std::path",
                "std::process",
                "std::sync",
                "std::thread",
                "std::time",
                "async_std::",
                "tokio::",
                "eframe::",
                "egui::",
                "wgpu::",
                "winit::",
                "zarrs::",
                "serde::",
                "mirante4d_analysis",
                "mirante4d_app",
                "mirante4d_data",
                "mirante4d_format",
                "mirante4d_import",
                "mirante4d_renderer",
            ],
            "WP-07A canonical-model crate must remain pure and independent of product/runtime frameworks",
        ));
        violations.extend(forbidden_canonical_model_std_use_violations(path, source));
    }
    violations
}

fn forbidden_canonical_model_std_use_violations(path: &Path, source: &str) -> Vec<String> {
    let forbidden = BTreeSet::from([
        "env", "fs", "io", "net", "os", "path", "process", "sync", "thread", "time",
    ]);
    forbidden_std_authority_violations(path, source, &forbidden, "WP-07A canonical-model crate")
}

fn forbidden_std_authority_violations(
    path: &Path,
    source: &str,
    forbidden: &BTreeSet<&'static str>,
    policy: &'static str,
) -> Vec<String> {
    let Ok(file) = syn::parse_file(source) else {
        return vec![format!(
            "{} cannot be parsed for {policy} side-effect ownership",
            path.display()
        )];
    };
    let mut visitor = ForbiddenStdUseVisitor {
        source_path: path,
        forbidden,
        policy,
        violations: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.violations
}

struct ForbiddenStdUseVisitor<'a> {
    source_path: &'a Path,
    forbidden: &'a BTreeSet<&'static str>,
    policy: &'static str,
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for ForbiddenStdUseVisitor<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        if use_tree_aliases_std_root(&item.tree, true, false) {
            self.violations.push(format!(
                "{}: {} may not alias the std crate",
                self.source_path.display(),
                self.policy,
            ));
        }
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for segments in paths {
            let imports_forbidden = segments.first().is_some_and(|segment| segment == "std")
                && segments
                    .get(1)
                    .is_some_and(|segment| self.forbidden.contains(segment.as_str()));
            if imports_forbidden {
                self.violations.push(format!(
                    "{}: {} imports forbidden std authority {}",
                    self.source_path.display(),
                    self.policy,
                    segments.join("::")
                ));
            }
        }
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "std" {
            self.violations.push(format!(
                "{}: {} may not alias the std crate",
                self.source_path.display(),
                self.policy,
            ));
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let mut segments = path.segments.iter();
        let root = segments.next().map(|segment| segment.ident.to_string());
        let authority = segments.next().map(|segment| segment.ident.to_string());
        if root.as_deref() == Some("std")
            && authority
                .as_deref()
                .is_some_and(|segment| self.forbidden.contains(segment))
        {
            self.violations.push(format!(
                "{}: {} uses forbidden std authority std::{}",
                self.source_path.display(),
                self.policy,
                authority.expect("checked as present"),
            ));
        }
        syn::visit::visit_path(self, path);
    }
}

fn use_tree_aliases_std_root(tree: &syn::UseTree, at_root: bool, beneath_std: bool) -> bool {
    match tree {
        syn::UseTree::Path(path) => {
            let beneath_std = if at_root {
                path.ident == "std"
            } else {
                beneath_std
            };
            use_tree_aliases_std_root(&path.tree, false, beneath_std)
        }
        syn::UseTree::Rename(rename) => {
            (at_root && rename.ident == "std") || (beneath_std && rename.ident == "self")
        }
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .any(|tree| use_tree_aliases_std_root(tree, at_root, beneath_std)),
        syn::UseTree::Name(_) | syn::UseTree::Glob(_) => false,
    }
}

fn flatten_use_tree(tree: &syn::UseTree, prefix: &mut Vec<String>, paths: &mut Vec<Vec<String>>) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use_tree(&path.tree, prefix, paths);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let mut path = prefix.clone();
            path.push(name.ident.to_string());
            paths.push(path);
        }
        syn::UseTree::Rename(rename) => {
            let mut path = prefix.clone();
            path.push(rename.ident.to_string());
            paths.push(path);
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                flatten_use_tree(tree, prefix, paths);
            }
        }
        syn::UseTree::Glob(_) => paths.push(prefix.clone()),
    }
}

fn check_wp07b_boundary_source_ownership(repo_root: &Path) -> anyhow::Result<()> {
    let pure_forbidden_std = BTreeSet::from([
        "env", "fs", "io", "net", "os", "path", "process", "thread", "time",
    ]);
    let settings_forbidden_std = BTreeSet::from(["net", "process"]);
    let forbidden_frameworks = [
        "async_std::",
        "eframe::",
        "egui::",
        "rfd::",
        "tokio::",
        "wgpu::",
        "winit::",
        "zarrs::",
        "mirante4d_analysis::",
        "mirante4d_app::",
        "mirante4d_data::",
        "mirante4d_format::",
        "mirante4d_import::",
        "mirante4d_renderer::",
    ];
    let mut violations = Vec::new();
    for crate_name in WP07B_BOUNDARY_CRATES {
        let source_root = repo_root.join("crates").join(crate_name).join("src");
        for path in collect_rust_source_files(&source_root)? {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            violations.extend(source_pattern_violations(
                &path,
                &source,
                &forbidden_frameworks,
                "WP-07B boundary crate imports a forbidden runtime/UI authority",
            ));
            let (forbidden_std, policy) = if *crate_name == "mirante4d-settings" {
                (&settings_forbidden_std, "WP-07B settings crate")
            } else {
                (&pure_forbidden_std, "pure WP-07B boundary crate")
            };
            violations.extend(forbidden_std_authority_violations(
                &path,
                &source,
                forbidden_std,
                policy,
            ));
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "WP-07B boundary source ownership failed:\n{}",
            violations.join("\n")
        )
    }
}

fn check_wp07a_contracts(repo_root: &Path) -> anyhow::Result<()> {
    check_wp07a_model_contract(repo_root).map(|_| ())
}

fn check_wp07b_boundary_contract(repo_root: &Path) -> anyhow::Result<()> {
    let path = repo_root.join("architecture/wp07b-boundary-contract.json");
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let contract: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if contract.get("schema").and_then(serde_json::Value::as_str)
        != Some("mirante4d-wp07b-boundary-contract")
        || contract
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(2)
        || contract.get("status").and_then(serde_json::Value::as_str) != Some("live-cutover")
    {
        bail!("{} has an unsupported WP-07B live boundary", path.display());
    }
    let expected_entry_binding = [
        ("predecessor_tag", "foundation-wp-07a-exit-1"),
        (
            "predecessor_revision",
            "5383cbb93c13c59e6f035bfa551356c75fb426dc",
        ),
        (
            "entry_sha256",
            "ee14dae5433626e19fa28d0bcc13cabfdbc259604845ee967c0404d978a27d15",
        ),
        (
            "entry_correction_sha256",
            "d4309d2cdd342c84d536f34de4bfa05013f46b6e009f7e37516c93f814067ac6",
        ),
    ];
    for (field, expected) in expected_entry_binding {
        if contract.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
            bail!("WP-07B entry binding {field} drifted");
        }
    }
    let expected_scope = serde_json::json!({
        "checkpoint": "WP07B-B-live-cutover",
        "product_reachable": true,
        "canonical_application_authoritative": true,
        "canonical_project_model_authoritative": true,
        "settings_product_path_live": true,
        "project_v15_private_bridge_live": true,
        "core_deleted": true,
        "predecessors_deleted": true
    });
    if contract.get("scope") != Some(&expected_scope) {
        bail!("WP-07B live-cutover scope drifted");
    }
    let expected_product_open_validation = serde_json::json!({
        "scenario_id": "wp07b-t2-state-settings-identity-gate-v1",
        "fixture": {
            "name": "time-multichannel-u16-8cube-3t-2c",
            "dataset_id": "fixture-time-multichannel-u16-8cube-3t-2c",
            "tier": "T2"
        },
        "runtime": {
            "binary": "release",
            "display": "real-vulkan",
            "input": "os",
            "xdg_profile": "isolated",
            "automation": false,
            "smoke": false
        },
        "launch_1": {
            "physical_client_area_px": [1280, 720],
            "gpu_frame": "nonblank",
            "same_source_open": {
                "old_frame": "visible-until-completion"
            },
            "project_io_gate": {
                "open": "disabled",
                "save": "disabled",
                "text": "verification-required",
                "before": "dialog-or-io"
            },
            "next": "t2/3",
            "play": {
                "minimum_current_nonblank_timepoints": 2,
                "then": "Pause"
            },
            "layouts": ["3D", "4Panel", "3D"],
            "render_modes": ["MIP", "DVR", "MIP"],
            "settings_save_mib": {
                "cpu": 3072,
                "gpu": 1280
            },
            "physical_close_exit_code": 0
        },
        "launch_2": {
            "physical_client_area_px": [1920, 1080],
            "profile": "same-isolated-xdg",
            "settings_status": "Loaded",
            "settings_mib": {
                "cpu": 3072,
                "gpu": 1280
            },
            "gpu_frame": "nonblank",
            "project_io_gate": {
                "open": "disabled",
                "save": "disabled",
                "text": "verification-required",
                "before": "dialog-or-io"
            },
            "physical_close_exit_code": 0
        },
        "postconditions": {
            "launch_2_settings_bytes": "identical-to-post-launch-1",
            "source_tree": "unchanged"
        },
        "reject_log_signatures": [
            "panic",
            "validation error",
            "wgpu error",
            "fallback",
            "retry loop",
            "corruption"
        ],
        "exclusions": [
            "segmentation",
            "4K",
            "private-data",
            "synthetic-large-dataset",
            "unsupported-project-roundtrip"
        ],
        "qualification": {
            "preflight": "nonqualifying",
            "final_run": {
                "qualifying": true,
                "workspace": "clean",
                "revision": "protected-main",
                "order": "after-checks"
            }
        }
    });
    if contract.get("product_open_validation") != Some(&expected_product_open_validation) {
        bail!("WP-07B product-open validation scenario drifted");
    }
    let expected_activation_invariants = serde_json::json!([
        "The canonical application, dataset, render-api, and settings boundaries are live dependencies of mirante4d-app.",
        "ApplicationState and the canonical project model are the sole live semantic and durable-state authorities.",
        "AppState, WorkbenchCommand, project-v14, app-local preferences, and mirante4d-core authority are absent.",
        "Exactly seven temporary runtime owners remain with their frozen field counts and expiry gates.",
        "Exactly two crate-private bridges remain: the egui shell bridge through WP-09C and project-v15 persistence bridge through WP-10B.",
        "The application reducer owns no filesystem, thread, UI, renderer, runtime payload, or serialization authority.",
        "Scientific identity remains explicit and project open/save stays gated on a verified source."
    ]);
    if contract.get("activation_invariants") != Some(&expected_activation_invariants)
        || contract.get("unresolved_decisions") != Some(&serde_json::json!([]))
    {
        bail!("WP-07B live-cutover invariants or decision closure drifted");
    }

    let expected_mirante4d_dependencies = BTreeMap::from([
        (
            "mirante4d-application",
            BTreeSet::from(
                [
                    "mirante4d-dataset",
                    "mirante4d-domain",
                    "mirante4d-identity",
                    "mirante4d-project-model",
                    "mirante4d-settings",
                ]
                .map(str::to_owned),
            ),
        ),
        (
            "mirante4d-dataset",
            BTreeSet::from(["mirante4d-domain", "mirante4d-identity"].map(str::to_owned)),
        ),
        (
            "mirante4d-render-api",
            BTreeSet::from(["mirante4d-domain"].map(str::to_owned)),
        ),
        ("mirante4d-settings", BTreeSet::<String>::new()),
    ]);
    let expected_external_dependencies = BTreeMap::from([
        ("mirante4d-application", BTreeSet::<String>::new()),
        (
            "mirante4d-dataset",
            BTreeSet::from(["thiserror"].map(str::to_owned)),
        ),
        (
            "mirante4d-render-api",
            BTreeSet::from(["thiserror"].map(str::to_owned)),
        ),
        (
            "mirante4d-settings",
            BTreeSet::from(["serde", "serde_json", "thiserror"].map(str::to_owned)),
        ),
    ]);
    let expected_dev_dependencies = BTreeMap::from([
        ("mirante4d-application", BTreeSet::<String>::new()),
        ("mirante4d-dataset", BTreeSet::<String>::new()),
        ("mirante4d-render-api", BTreeSet::<String>::new()),
        (
            "mirante4d-settings",
            BTreeSet::from(["tempfile"].map(str::to_owned)),
        ),
    ]);
    let expected_side_effects = BTreeMap::from([
        ("mirante4d-application", BTreeSet::<String>::new()),
        ("mirante4d-dataset", BTreeSet::<String>::new()),
        ("mirante4d-render-api", BTreeSet::<String>::new()),
        (
            "mirante4d-settings",
            BTreeSet::from(
                ["background_thread", "clock", "environment", "filesystem"].map(str::to_owned),
            ),
        ),
    ]);
    let expected_authorities = BTreeMap::from([
        (
            "mirante4d-application",
            "Sole live bounded application command, reducer, event, snapshot, project revision/history, transient semantic selection, operation-currentness, and typed-fault authority",
        ),
        (
            "mirante4d-dataset",
            "Sole live bounded immutable logical-layer catalog and verified/unverified scientific-identity status authority",
        ),
        (
            "mirante4d-render-api",
            "Sole live framework-neutral presentation viewport and canonical-camera projection-math authority at the WP-07B boundary",
        ),
        (
            "mirante4d-settings",
            "Sole live validated two-ledger settings document, Linux path, bounded background persistence actor, and typed persistence-outcome authority",
        ),
    ]);
    let crate_contracts = contract
        .get("crate_contracts")
        .and_then(serde_json::Value::as_array)
        .context("WP-07B boundary crate_contracts must be an array")?;
    let workspace_metadata = workspace_dependency_metadata(repo_root)?;
    let mut observed = BTreeSet::new();
    for crate_contract in crate_contracts {
        let name = crate_contract
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("WP-07B boundary crate name must be a string")?;
        if !expected_mirante4d_dependencies.contains_key(name) || !observed.insert(name) {
            bail!("unexpected or duplicate WP-07B boundary crate {name}");
        }
        if crate_contract
            .get("authority")
            .and_then(serde_json::Value::as_str)
            != expected_authorities.get(name).copied()
        {
            bail!("WP-07B boundary crate {name} authority text drifted");
        }
        let crate_path = crate_contract
            .get("path")
            .and_then(serde_json::Value::as_str)
            .context("WP-07B boundary crate path must be a string")?;
        if crate_path != format!("crates/{name}")
            || repo_root.join(crate_path).join("build.rs").exists()
        {
            bail!("WP-07B boundary crate {name} has a forbidden path or build script");
        }
        let package_id = workspace_metadata
            .workspace_package_ids_by_name
            .get(name)
            .with_context(|| format!("cargo metadata is missing WP-07B crate {name}"))?;
        if workspace_metadata
            .custom_build_package_ids
            .contains(package_id)
        {
            bail!("WP-07B boundary crate {name} must not have a custom-build target");
        }

        let mirante4d_dependencies = json_string_set(
            crate_contract,
            "permitted_normal_mirante4d_dependencies",
            "WP-07B Mirante4D dependency allowlist",
        )?;
        let external_dependencies = json_string_set(
            crate_contract,
            "permitted_normal_external_dependencies",
            "WP-07B external dependency allowlist",
        )?;
        let dev_dependencies = json_string_set(
            crate_contract,
            "permitted_dev_dependencies",
            "WP-07B dev-dependency allowlist",
        )?;
        let side_effects = json_string_set(
            crate_contract,
            "permitted_external_side_effects",
            "WP-07B side-effect allowlist",
        )?;
        if &mirante4d_dependencies != expected_mirante4d_dependencies.get(name).unwrap()
            || &external_dependencies != expected_external_dependencies.get(name).unwrap()
            || &dev_dependencies != expected_dev_dependencies.get(name).unwrap()
            || &side_effects != expected_side_effects.get(name).unwrap()
        {
            bail!("WP-07B boundary crate {name} contract drifted from its frozen allowlists");
        }
        let actual_kinds = workspace_metadata
            .declared_dependency_kinds_by_name
            .get(name)
            .with_context(|| format!("cargo metadata is missing WP-07B crate {name}"))?;
        let expected_normal = mirante4d_dependencies
            .iter()
            .chain(&external_dependencies)
            .cloned()
            .collect::<BTreeSet<_>>();
        // WP-07B remains historical evidence. WP-08A's exact matrix, checked
        // first, owns these narrowly superseding dependency/API additions.
        let wp08a_normal_additions = match name {
            "mirante4d-application" => BTreeSet::from(["mirante4d-render-api".to_owned()]),
            "mirante4d-render-api" => BTreeSet::from(["mirante4d-dataset".to_owned()]),
            _ => BTreeSet::new(),
        };
        let wp08a_normal = expected_normal
            .union(&wp08a_normal_additions)
            .cloned()
            .collect::<BTreeSet<_>>();
        if actual_kinds.get("normal").cloned().unwrap_or_default() != wp08a_normal
            || actual_kinds.get("dev").cloned().unwrap_or_default() != dev_dependencies
            || actual_kinds
                .get("build")
                .is_some_and(|dependencies| !dependencies.is_empty())
        {
            bail!("WP-07B boundary crate {name} declared dependency kinds drifted");
        }

        let public_api =
            json_string_set(crate_contract, "public_api", "WP-07B public API allowlist")?;
        if public_api.is_empty() {
            bail!("WP-07B boundary crate {name} must have a nonempty public API");
        }
        let actual_public_api =
            public_root_api_names(&repo_root.join(crate_path).join("src/lib.rs"))?;
        let wp08a_supersedes_public_root =
            matches!(name, "mirante4d-dataset" | "mirante4d-render-api");
        if (!wp08a_supersedes_public_root && actual_public_api != public_api)
            || (wp08a_supersedes_public_root && !public_api.is_subset(&actual_public_api))
        {
            let missing = public_api
                .difference(&actual_public_api)
                .collect::<Vec<_>>();
            let unexpected = actual_public_api
                .difference(&public_api)
                .collect::<Vec<_>>();
            bail!(
                "WP-07B boundary crate {name} public API drifted: missing={missing:?}, unexpected={unexpected:?}"
            );
        }
    }
    if observed
        != WP07B_BOUNDARY_CRATES
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
    {
        bail!("WP-07B boundary contract must cover exactly the four approved crates");
    }

    check_wp07b_live_cutover(repo_root, &contract, &workspace_metadata)?;
    check_wp07b_boundary_source_ownership(repo_root)
}

fn check_wp07b_live_cutover(
    repo_root: &Path,
    contract: &serde_json::Value,
    workspace_metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let expected_product_activation = serde_json::json!({
        "package": "mirante4d-app",
        "required_normal_dependencies": [
            "mirante4d-application",
            "mirante4d-dataset",
            "mirante4d-render-api",
            "mirante4d-settings"
        ]
    });
    if contract.get("product_activation") != Some(&expected_product_activation) {
        bail!("WP-07B live product activation contract drifted");
    }
    let required_product_dependencies = WP07B_BOUNDARY_CRATES
        .iter()
        .map(|dependency| (*dependency).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_product_dependencies = workspace_metadata
        .declared_dependency_kinds_by_name
        .get("mirante4d-app")
        .and_then(|kinds| kinds.get("normal"))
        .context("cargo metadata is missing mirante4d-app normal dependencies")?;
    if !required_product_dependencies.is_subset(actual_product_dependencies) {
        bail!(
            "mirante4d-app is missing live WP-07B boundary dependencies: {:?}",
            required_product_dependencies
                .difference(actual_product_dependencies)
                .collect::<Vec<_>>()
        );
    }

    let expected_edges = BTreeSet::from([
        ("mirante4d-app", "mirante4d-application"),
        ("mirante4d-app", "mirante4d-dataset"),
        ("mirante4d-app", "mirante4d-render-api"),
        ("mirante4d-app", "mirante4d-settings"),
        ("mirante4d-application", "mirante4d-dataset"),
        ("mirante4d-application", "mirante4d-settings"),
        ("mirante4d-renderer", "mirante4d-render-api"),
        ("xtask", "mirante4d-render-api"),
    ]);
    let contract_edges = contract
        .get("live_boundary_dependency_edges")
        .and_then(serde_json::Value::as_array)
        .context("WP-07B live boundary dependency edges must be an array")?
        .iter()
        .map(|edge| {
            Ok((
                edge.get("from")
                    .and_then(serde_json::Value::as_str)
                    .context("WP-07B live dependency edge must name from")?,
                edge.get("to")
                    .and_then(serde_json::Value::as_str)
                    .context("WP-07B live dependency edge must name to")?,
            ))
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    if contract_edges != expected_edges {
        bail!("WP-07B live boundary dependency-edge contract drifted");
    }
    let boundary_crates = WP07B_BOUNDARY_CRATES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let actual_edges = workspace_metadata
        .declared_dependency_kinds_by_name
        .iter()
        .flat_map(|(package, kinds)| {
            kinds.values().flatten().filter_map(|dependency| {
                boundary_crates
                    .contains(dependency.as_str())
                    .then_some((package.as_str(), dependency.as_str()))
            })
        })
        .collect::<BTreeSet<_>>();
    let live_foundation_edges = BTreeSet::from([
        ("mirante4d-application", "mirante4d-render-api"),
        ("mirante4d-data", "mirante4d-dataset"),
        ("mirante4d-dataset-runtime", "mirante4d-dataset"),
        ("mirante4d-render-api", "mirante4d-dataset"),
        ("mirante4d-renderer", "mirante4d-dataset"),
        ("mirante4d-storage", "mirante4d-dataset"),
        ("xtask", "mirante4d-dataset"),
    ]);
    let live_expected_edges = expected_edges
        .union(&live_foundation_edges)
        .copied()
        .collect::<BTreeSet<_>>();
    if actual_edges != live_expected_edges {
        bail!(
            "live foundation boundary dependency edges drifted: expected={live_expected_edges:?}, actual={actual_edges:?}"
        );
    }

    check_wp07b_historical_runtime_owner_contract(contract)?;
    check_wp07b_private_bridges(repo_root, contract)?;
    check_wp07b_predecessor_absence(repo_root, contract, workspace_metadata)
}

fn check_wp07b_historical_runtime_owner_contract(
    contract: &serde_json::Value,
) -> anyhow::Result<()> {
    let expected_contract = serde_json::json!([
        {
            "module": "dataset",
            "path": "crates/mirante4d-app/src/current_runtime/dataset.rs",
            "struct": "CurrentDatasetRuntime",
            "composition_field": "dataset_runtime",
            "expected_fields": 53,
            "expiry_gate": "WP-08B"
        },
        {
            "module": "render",
            "path": "crates/mirante4d-app/src/current_runtime/render.rs",
            "struct": "CurrentRenderRuntime",
            "composition_field": "render_runtime",
            "expected_fields": 24,
            "expiry_gate": "WP-09B"
        },
        {
            "module": "ui",
            "path": "crates/mirante4d-app/src/current_runtime/ui.rs",
            "struct": "CurrentUiRuntime",
            "composition_field": "ui_runtime",
            "expected_fields": 14,
            "expiry_gate": "WP-09C"
        },
        {
            "module": "project",
            "path": "crates/mirante4d-app/src/current_runtime/project.rs",
            "struct": "CurrentProjectRuntime",
            "composition_field": "project_runtime",
            "expected_fields": 1,
            "expiry_gate": "WP-10B"
        },
        {
            "module": "import",
            "path": "crates/mirante4d-app/src/current_runtime/import.rs",
            "struct": "CurrentImportRuntime",
            "composition_field": "import_runtime",
            "expected_fields": 4,
            "expiry_gate": "WP-10C"
        },
        {
            "module": "analysis",
            "path": "crates/mirante4d-app/src/current_runtime/analysis.rs",
            "struct": "CurrentAnalysisRuntime",
            "composition_field": "analysis_runtime",
            "expected_fields": 10,
            "expiry_gate": "WP-12"
        },
        {
            "module": "validation",
            "path": "crates/mirante4d-app/src/current_runtime/validation.rs",
            "struct": "CurrentValidationRuntime",
            "composition_field": "validation_runtime",
            "expected_fields": 2,
            "expiry_gate": "WP-14"
        }
    ]);
    if contract.get("temporary_runtime_owners") != Some(&expected_contract) {
        bail!("WP-07B temporary-runtime-owner contract drifted");
    }
    Ok(())
}

fn check_current_state_field_ledger(repo_root: &Path) -> anyhow::Result<()> {
    let path = repo_root.join("architecture/current-state-field-ledger.json");
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let ledger: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if ledger.get("schema").and_then(serde_json::Value::as_str)
        != Some("mirante4d-current-state-field-ledger")
        || ledger
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(2)
        || ledger.get("status").and_then(serde_json::Value::as_str)
            != Some("wp08b-unified-runtime-cutover")
        || ledger
            .pointer("/predecessor/tag")
            .and_then(serde_json::Value::as_str)
            != Some("foundation-wp-08a-exit-2")
        || ledger
            .pointer("/predecessor/revision")
            .and_then(serde_json::Value::as_str)
            != Some("f2e520da891134d1b3f65d8fcac7afb4140579a2")
    {
        bail!("{} is not the accepted WP-08B live ledger", path.display());
    }

    let expected_dataset_authority = serde_json::json!({
        "runtime_owner": "mirante4d-dataset-runtime",
        "composition_state": "DatasetDemandState",
        "sole_poll_owner": "DatasetRequestDispatcher",
        "source_bridge": {
            "type": "CurrentDatasetSource",
            "path": "crates/mirante4d-data/src/current_source_bridge.rs",
            "expires": "WP-10C"
        },
        "renderer_bridge": {
            "type": "CurrentLeaseBridge",
            "path": "crates/mirante4d-renderer/src/current_lease_bridge.rs",
            "expires": "WP-09B"
        }
    });
    if ledger.get("dataset_authority") != Some(&expected_dataset_authority) {
        bail!("WP-08B dataset authority ledger drifted");
    }

    let expected_owners = BTreeMap::from([
        (
            "render",
            (
                "crates/mirante4d-app/src/current_runtime/render.rs",
                "CurrentRenderRuntime",
                "render_runtime",
                "WP-09B",
            ),
        ),
        (
            "ui",
            (
                "crates/mirante4d-app/src/current_runtime/ui.rs",
                "CurrentUiRuntime",
                "ui_runtime",
                "WP-09C",
            ),
        ),
        (
            "project",
            (
                "crates/mirante4d-app/src/current_runtime/project.rs",
                "CurrentProjectRuntime",
                "project_runtime",
                "WP-10B",
            ),
        ),
        (
            "import",
            (
                "crates/mirante4d-app/src/current_runtime/import.rs",
                "CurrentImportRuntime",
                "import_runtime",
                "WP-10C",
            ),
        ),
        (
            "analysis",
            (
                "crates/mirante4d-app/src/current_runtime/analysis.rs",
                "CurrentAnalysisRuntime",
                "analysis_runtime",
                "WP-12",
            ),
        ),
        (
            "validation",
            (
                "crates/mirante4d-app/src/current_runtime/validation.rs",
                "CurrentValidationRuntime",
                "validation_runtime",
                "WP-14",
            ),
        ),
    ]);
    let owner_entries = ledger
        .get("temporary_owners")
        .and_then(serde_json::Value::as_array)
        .context("WP-08B temporary_owners must be an array")?;
    let mut observed_owners = BTreeMap::new();
    for owner in owner_entries {
        let module = owner
            .get("module")
            .and_then(serde_json::Value::as_str)
            .context("WP-08B temporary owner must name its module")?;
        let facts = (
            owner
                .get("path")
                .and_then(serde_json::Value::as_str)
                .context("WP-08B temporary owner must name its path")?,
            owner
                .get("type")
                .and_then(serde_json::Value::as_str)
                .context("WP-08B temporary owner must name its type")?,
            owner
                .get("composition_field")
                .and_then(serde_json::Value::as_str)
                .context("WP-08B temporary owner must name its composition field")?,
            owner
                .get("expires")
                .and_then(serde_json::Value::as_str)
                .context("WP-08B temporary owner must name its expiry")?,
        );
        if observed_owners.insert(module, facts).is_some() {
            bail!("WP-08B temporary owner {module} is repeated");
        }
    }
    if observed_owners != expected_owners {
        bail!("WP-08B temporary owner ledger drifted");
    }

    let runtime_root = repo_root.join("crates/mirante4d-app/src/current_runtime");
    let actual_runtime_files = fs::read_dir(&runtime_root)?
        .map(|entry| {
            let entry = entry?;
            Ok(entry
                .file_type()?
                .is_file()
                .then(|| entry.file_name().to_string_lossy().into_owned()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    let expected_runtime_files = expected_owners
        .keys()
        .map(|module| format!("{module}.rs"))
        .chain(std::iter::once("mod.rs".to_owned()))
        .collect::<BTreeSet<_>>();
    if actual_runtime_files != expected_runtime_files {
        bail!(
            "WP-08B current-runtime modules drifted: expected={expected_runtime_files:?}, actual={actual_runtime_files:?}"
        );
    }
    let runtime_mod_source = fs::read_to_string(runtime_root.join("mod.rs"))?;
    let actual_modules = crate_private_root_module_names(&runtime_mod_source)?;
    let expected_modules = expected_owners
        .keys()
        .map(|module| (*module).to_owned())
        .collect::<BTreeSet<_>>();
    if actual_modules != expected_modules {
        bail!("current_runtime must declare exactly the six post-WP-08B temporary owners");
    }

    let app_root = repo_root.join("crates/mirante4d-app/src");
    let app_source = fs::read_to_string(app_root.join("lib.rs"))?;
    let app_fields = rust_struct_field_terminal_type_names(&app_source, "MiranteWorkbenchApp")?;
    if app_fields.get("dataset").map(String::as_str) != Some("DatasetDemandState")
        || app_fields.contains_key("dataset_runtime")
    {
        bail!("MiranteWorkbenchApp must compose DatasetDemandState without CurrentDatasetRuntime");
    }
    let owner_type_names = expected_owners
        .values()
        .map(|(_, type_name, _, _)| *type_name)
        .collect::<BTreeSet<_>>();
    let expected_composition = expected_owners
        .values()
        .map(|(_, type_name, field, _)| ((*field).to_owned(), (*type_name).to_owned()))
        .collect::<BTreeMap<_, _>>();
    let actual_composition = app_fields
        .iter()
        .filter(|(_, type_name)| owner_type_names.contains(type_name.as_str()))
        .map(|(field, type_name)| (field.clone(), type_name.clone()))
        .collect::<BTreeMap<_, _>>();
    if actual_composition != expected_composition {
        bail!(
            "post-WP-08B temporary-owner composition drifted: expected={expected_composition:?}, actual={actual_composition:?}"
        );
    }
    for (relative_path, type_name, _, expiry) in expected_owners.values() {
        let source = fs::read_to_string(repo_root.join(relative_path))?;
        rust_struct_field_names(&source, type_name)?;
        if !source.contains(expiry) {
            bail!("{relative_path} does not declare its deletion gate {expiry}");
        }
    }

    check_wp08b_dataset_dispatcher(repo_root, &app_root, &app_fields)?;
    check_wp08b_bridges(repo_root, &app_root)?;
    check_wp08b_predecessor_absence(repo_root, &ledger)?;
    check_wp08b_passive_analysis(repo_root, &ledger)
}

fn check_wp08b_dataset_dispatcher(
    repo_root: &Path,
    app_root: &Path,
    app_fields: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let dispatcher_path = app_root.join("dataset_requests.rs");
    let dispatcher_source = fs::read_to_string(&dispatcher_path)?;
    let items = rust_root_defined_item_names(&dispatcher_source)?;
    for required in ["DatasetDemandState", "DatasetRequestDispatcher"] {
        if !items.contains(required) {
            bail!("dataset request authority is missing {required}");
        }
    }
    if !dispatcher_source.contains("runtime: Arc<dyn DatasetRuntime>")
        || !dispatcher_source.contains("dispatcher: DatasetRequestDispatcher")
        || app_fields.get("dataset").map(String::as_str) != Some("DatasetDemandState")
    {
        bail!("the WP-08B runtime/dispatcher/composition ownership chain drifted");
    }
    if dispatcher_source.matches(".runtime.poll(").count() != 1 {
        bail!("DatasetRequestDispatcher must contain the sole DatasetRuntime poll call");
    }
    for source_path in collect_rust_source_files(app_root)? {
        let relative = source_path.strip_prefix(repo_root).unwrap_or(&source_path);
        let normalized = normalize_repo_path(relative);
        if normalized.contains("/tests/") || normalized.ends_with("/tests.rs") {
            continue;
        }
        let source = fs::read_to_string(&source_path)?;
        if source_path != dispatcher_path
            && source_contains_identifier(&source, "DatasetRuntime")
            && source.contains(".poll(")
        {
            bail!(
                "{} bypasses the sole DatasetRequestDispatcher poll owner",
                relative.display()
            );
        }
    }
    Ok(())
}

fn check_wp08b_bridges(repo_root: &Path, app_root: &Path) -> anyhow::Result<()> {
    let source_bridge_path = repo_root.join("crates/mirante4d-data/src/current_source_bridge.rs");
    let source_bridge = fs::read_to_string(&source_bridge_path)?;
    if !rust_root_defined_item_names(&source_bridge)?.contains("CurrentDatasetSource")
        || !public_root_api_names(&repo_root.join("crates/mirante4d-data/src/lib.rs"))?
            .contains("CurrentDatasetSource")
    {
        bail!("the sole WP-08B current-storage source bridge is missing");
    }
    let mut source_open_routes = Vec::new();
    let mut source_open_calls = 0;
    for source_path in collect_rust_source_files(app_root)? {
        let relative = source_path.strip_prefix(repo_root).unwrap_or(&source_path);
        let normalized = normalize_repo_path(relative);
        if normalized.contains("/tests/") || normalized.ends_with("/tests.rs") {
            continue;
        }
        let source = fs::read_to_string(&source_path)?;
        let calls = source.matches("CurrentDatasetSource::open(").count();
        if calls != 0 {
            source_open_routes.push(normalized);
            source_open_calls += calls;
        }
    }
    if source_open_calls != 1
        || source_open_routes != ["crates/mirante4d-app/src/unified_source_open.rs"]
    {
        bail!("current source must open through one unified route: {source_open_routes:?}");
    }

    let lease_bridge_path = repo_root.join("crates/mirante4d-renderer/src/current_lease_bridge.rs");
    let lease_bridge = fs::read_to_string(&lease_bridge_path)?;
    if !rust_root_defined_item_names(&lease_bridge)?.contains("CurrentLeaseBridge")
        || !public_root_api_names(&repo_root.join("crates/mirante4d-renderer/src/lib.rs"))?
            .contains("CurrentLeaseBridge")
    {
        bail!("the sole WP-08B current-renderer lease bridge is missing");
    }
    for forbidden in [
        "mirante4d_data",
        "mirante4d_format",
        "DatasetHandle",
        "DenseVolumeU8",
        "DenseVolumeU16",
        "DenseVolumeF32",
        "VolumeBrickU8",
        "VolumeBrickU16",
        "VolumeBrickF32",
        "SpatialBrickIndex",
    ] {
        if source_contains_identifier(&lease_bridge, forbidden) {
            bail!("current renderer lease bridge imports forbidden owning fact {forbidden}");
        }
    }
    let render_runtime = fs::read_to_string(app_root.join("current_runtime/render.rs"))?;
    let render_fields =
        rust_struct_field_terminal_type_names(&render_runtime, "CurrentRenderRuntime")?;
    if render_fields.get("lease_bridge").map(String::as_str) != Some("CurrentLeaseBridge") {
        bail!("CurrentRenderRuntime must retain the sole CurrentLeaseBridge");
    }
    Ok(())
}

fn check_wp08b_predecessor_absence(
    repo_root: &Path,
    ledger: &serde_json::Value,
) -> anyhow::Result<()> {
    let expected_paths = BTreeSet::from([
        "crates/mirante4d-app/src/analysis_jobs.rs".to_owned(),
        "crates/mirante4d-app/src/brick_streaming.rs".to_owned(),
        "crates/mirante4d-app/src/brick_streaming_plan.rs".to_owned(),
        "crates/mirante4d-app/src/cross_section_read_queue.rs".to_owned(),
        "crates/mirante4d-app/src/cross_section_streaming.rs".to_owned(),
        "crates/mirante4d-app/src/current_runtime/dataset.rs".to_owned(),
        "crates/mirante4d-app/src/dataset_opening.rs".to_owned(),
        "crates/mirante4d-data/src/worker.rs".to_owned(),
        "crates/mirante4d-data/src/worker/tests.rs".to_owned(),
    ]);
    let expected_types = BTreeSet::from([
        "BrickHistogramSample".to_owned(),
        "BrickReadMetrics".to_owned(),
        "BrickReadOutcome".to_owned(),
        "BrickReadPayload".to_owned(),
        "BrickReadPool".to_owned(),
        "BrickReadQueueDiagnostics".to_owned(),
        "BrickReadSpec".to_owned(),
        "BrickReadStatus".to_owned(),
        "BrickReadTicket".to_owned(),
        "BrickRequestPriority".to_owned(),
        "CancellationToken".to_owned(),
        "CrossSectionChunkReadPool".to_owned(),
        "CrossSectionChunkReadSpec".to_owned(),
        "CurrentDatasetRuntime".to_owned(),
        "DataGenerationId".to_owned(),
        "DataRequestId".to_owned(),
    ]);
    let expected_payload_types = BTreeSet::from([
        "DenseVolumeF32".to_owned(),
        "DenseVolumeU16".to_owned(),
        "DenseVolumeU8".to_owned(),
        "VolumeBrickF32".to_owned(),
        "VolumeBrickU16".to_owned(),
        "VolumeBrickU8".to_owned(),
    ]);
    let deleted = ledger
        .get("deleted_predecessors")
        .context("WP-08B ledger must name deleted predecessors")?;
    if json_string_set(deleted, "paths", "WP-08B deleted paths")? != expected_paths
        || json_string_set(deleted, "types", "WP-08B deleted types")? != expected_types
        || json_string_set(
            deleted,
            "app_owned_payload_types",
            "WP-08B forbidden app payload types",
        )? != expected_payload_types
    {
        bail!("WP-08B predecessor-deletion ledger drifted");
    }
    for relative_path in &expected_paths {
        if repo_root.join(relative_path).exists() {
            bail!("WP-08B predecessor path still exists: {relative_path}");
        }
    }
    for source_path in collect_rust_source_files(&repo_root.join("crates"))? {
        if source_path.starts_with(repo_root.join("crates/xtask")) {
            continue;
        }
        let source = fs::read_to_string(&source_path)?;
        for identifier in &expected_types {
            if source_contains_identifier(&source, identifier) {
                bail!(
                    "WP-08B predecessor type {identifier} remains in {}",
                    source_path.display()
                );
            }
        }
        if source_path.starts_with(repo_root.join("crates/mirante4d-app")) {
            for identifier in &expected_payload_types {
                if source_contains_identifier(&source, identifier) {
                    bail!(
                        "application still owns predecessor dataset payload type {identifier} in {}",
                        source_path.display()
                    );
                }
            }
        }
    }
    for source_path in collect_rust_source_files(&repo_root.join("crates/mirante4d-data/src"))? {
        let normalized = normalize_repo_path(&source_path);
        if normalized.contains("/tests/") || normalized.ends_with("/tests.rs") {
            continue;
        }
        let source = fs::read_to_string(&source_path)?;
        if source_contains_identifier(&source, "mpsc") {
            bail!(
                "old data-worker channel authority remains in {}",
                source_path.display()
            );
        }
    }
    Ok(())
}

fn check_wp08b_passive_analysis(
    repo_root: &Path,
    ledger: &serde_json::Value,
) -> anyhow::Result<()> {
    let analysis_owner = ledger
        .get("temporary_owners")
        .and_then(serde_json::Value::as_array)
        .and_then(|owners| {
            owners.iter().find(|owner| {
                owner.get("module").and_then(serde_json::Value::as_str) == Some("analysis")
            })
        })
        .context("WP-08B ledger must retain the passive analysis owner")?;
    if analysis_owner
        .get("mode")
        .and_then(serde_json::Value::as_str)
        != Some("passive-results-only")
        || analysis_owner
            .get("expires")
            .and_then(serde_json::Value::as_str)
            != Some("WP-12")
    {
        bail!("current analysis must remain passive until WP-12");
    }
    let path = repo_root.join("crates/mirante4d-app/src/current_runtime/analysis.rs");
    let source = fs::read_to_string(&path)?;
    if !source.contains("Analysis execution is deferred until WP-12.") {
        bail!("current analysis does not expose its WP-12 deferred state");
    }
    for forbidden in [
        "AccountedResourceLease",
        "DatasetHandle",
        "DatasetRuntime",
        "ResourceRequest",
        "mpsc",
        "thread",
        "read_u8",
        "read_u16",
        "read_f32",
    ] {
        if source_contains_identifier(&source, forbidden) {
            bail!("passive analysis owner contains execution authority {forbidden}");
        }
    }
    Ok(())
}

fn check_wp07b_private_bridges(
    repo_root: &Path,
    contract: &serde_json::Value,
) -> anyhow::Result<()> {
    let expected_contract = serde_json::json!([
        {
            "module": "current_egui_shell_bridge",
            "path": "crates/mirante4d-app/src/current_egui_shell_bridge.rs",
            "route": "current-egui-shell-to-application",
            "expiry_gate": "WP-09C",
            "required_root_items": ["dispatch", "drain_events", "snapshot"]
        },
        {
            "module": "current_project_persistence_bridge",
            "path": "crates/mirante4d-app/src/current_project_persistence_bridge.rs",
            "route": "canonical-project-model-to-private-project-v15-persistence",
            "expiry_gate": "WP-10B",
            "required_root_items": [
                "CurrentProjectPersistenceBridge",
                "PROJECT_V15_SCHEMA",
                "PROJECT_V15_SCHEMA_VERSION",
                "ProjectPersistenceError",
                "ProjectPersistenceEvent"
            ]
        }
    ]);
    if contract.get("private_bridges") != Some(&expected_contract) {
        bail!("WP-07B private-bridge contract drifted");
    }
    let app_source_root = repo_root.join("crates/mirante4d-app/src");
    let bridge_paths = collect_rust_source_files(&app_source_root)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_bridge.rs"))
        })
        .map(|path| {
            path.strip_prefix(repo_root)
                .map(normalize_repo_path)
                .map_err(Into::into)
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let expected_bridge_paths = BTreeSet::from([
        "crates/mirante4d-app/src/current_egui_shell_bridge.rs".to_owned(),
        "crates/mirante4d-app/src/current_project_persistence_bridge.rs".to_owned(),
    ]);
    if bridge_paths != expected_bridge_paths {
        bail!(
            "WP-07B must retain exactly its two private bridges: expected={expected_bridge_paths:?}, actual={bridge_paths:?}"
        );
    }

    let app_source = fs::read_to_string(app_source_root.join("lib.rs"))?;
    let private_modules = private_root_module_names(&app_source)?;
    for module in [
        "current_egui_shell_bridge",
        "current_project_persistence_bridge",
    ] {
        if !private_modules.contains(module) {
            bail!("WP-07B bridge module {module} must remain private");
        }
    }
    let app_fields = rust_struct_field_terminal_type_names(&app_source, "MiranteWorkbenchApp")?;
    if app_fields.get("application").map(String::as_str) != Some("ApplicationState")
        || app_fields.get("project_persistence").map(String::as_str)
            != Some("CurrentProjectPersistenceBridge")
    {
        bail!("MiranteWorkbenchApp canonical application or project-bridge route drifted");
    }

    let egui_path = app_source_root.join("current_egui_shell_bridge.rs");
    let egui_source = fs::read_to_string(&egui_path)?;
    if !egui_source.contains("until\n//! WP-09C.") && !egui_source.contains("WP-09C") {
        bail!("current egui shell bridge must retain its WP-09C expiry");
    }
    let egui_items = rust_root_defined_item_names(&egui_source)?;
    let expected_egui_items = BTreeSet::from([
        "dispatch".to_owned(),
        "drain_events".to_owned(),
        "snapshot".to_owned(),
    ]);
    if egui_items != expected_egui_items || !public_root_api_names(&egui_path)?.is_empty() {
        bail!("current egui shell bridge API or privacy drifted");
    }
    if !egui_source.contains("application.dispatch(command)")
        || !app_source.contains("current_egui_shell_bridge::dispatch")
        || !app_source.contains("fn apply_application_command")
    {
        bail!("current egui shell mutation route no longer closes through the application bridge");
    }

    let project_path = app_source_root.join("current_project_persistence_bridge.rs");
    let project_source = fs::read_to_string(&project_path)?;
    if !project_source.contains("WP-10B") {
        bail!("current project persistence bridge must retain its WP-10B expiry");
    }
    let project_items = rust_root_defined_item_names(&project_source)?;
    let required_project_items = BTreeSet::from([
        "CurrentProjectPersistenceBridge".to_owned(),
        "PROJECT_V15_SCHEMA".to_owned(),
        "PROJECT_V15_SCHEMA_VERSION".to_owned(),
        "ProjectPersistenceError".to_owned(),
        "ProjectPersistenceEvent".to_owned(),
    ]);
    if !required_project_items.is_subset(&project_items)
        || !public_root_api_names(&project_path)?.is_empty()
    {
        bail!("current project persistence bridge API or privacy drifted");
    }
    let project_route_source = format!(
        "{app_source}\n{}",
        fs::read_to_string(app_source_root.join("workbench_ui.rs"))?
    );
    for marker in [
        "bridge.request_open",
        "bridge.request_save",
        "bridge.cancel",
        "bridge.try_recv",
        "project_persistence.shutdown",
    ] {
        if !project_route_source.contains(marker) {
            bail!("project persistence sole route is missing {marker:?}");
        }
    }
    for path in collect_rust_source_files(&app_source_root)? {
        if path == project_path || path == app_source_root.join("lib.rs") {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for identifier in [
            "CurrentProjectPersistenceBridge",
            "PROJECT_V15_SCHEMA",
            "ProjectDocumentDto",
        ] {
            if source_contains_identifier(&source, identifier) {
                bail!(
                    "{} bypasses the sole private project persistence route with {identifier}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn check_wp07b_predecessor_absence(
    repo_root: &Path,
    contract: &serde_json::Value,
    workspace_metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let expected_contract = serde_json::json!({
        "paths": [
            "crates/mirante4d-core",
            "crates/mirante4d-app/src/commands.rs",
            "crates/mirante4d-app/src/preferences.rs",
            "crates/mirante4d-app/src/project_session.rs",
            "crates/mirante4d-app/src/project_store.rs",
            "crates/mirante4d-app/src/session_state.rs",
            "crates/mirante4d-app/src/workbench_project.rs"
        ],
        "rust_identifiers": [
            "AppPreferences",
            "AppRecoverySession",
            "AppRuntimePreferences",
            "AppSession",
            "AppSessionManifest",
            "AppState",
            "PREFERENCES_FORMAT",
            "ProjectDirtySnapshot",
            "SESSION_FORMAT",
            "WorkbenchCommand",
            "WorkbenchCommandOutcome",
            "default_app_preferences_for_system",
            "load_app_preferences",
            "mirante4d_core",
            "open_dataset_with_preferences_and_render_first_frame",
            "save_app_preferences"
        ],
        "dependency_packages": ["mirante4d-core"]
    });
    if contract.get("forbidden_predecessors") != Some(&expected_contract) {
        bail!("WP-07B predecessor-deletion contract drifted");
    }
    for relative_path in expected_contract["paths"]
        .as_array()
        .expect("frozen predecessor paths are an array")
    {
        let relative_path = relative_path
            .as_str()
            .expect("frozen predecessor path is a string");
        if repo_root.join(relative_path).exists() {
            bail!("WP-07B predecessor path still exists: {relative_path}");
        }
    }
    let identifiers = expected_contract["rust_identifiers"]
        .as_array()
        .expect("frozen predecessor identifiers are an array")
        .iter()
        .map(|value| value.as_str().expect("predecessor identifier is a string"))
        .collect::<Vec<_>>();
    for path in collect_rust_source_files(&repo_root.join("crates"))? {
        if normalize_repo_path(&path).contains("/crates/xtask/")
            || path.starts_with(repo_root.join("crates/xtask"))
        {
            continue;
        }
        let source = fs::read_to_string(&path)?;
        for identifier in &identifiers {
            if source_contains_identifier(&source, identifier) {
                bail!(
                    "WP-07B predecessor identifier {identifier} remains in {}",
                    path.display()
                );
            }
        }
    }
    for (package, kinds) in &workspace_metadata.declared_dependency_kinds_by_name {
        if kinds
            .values()
            .flatten()
            .any(|dependency| dependency == "mirante4d-core")
        {
            bail!("workspace package {package} still depends on deleted mirante4d-core");
        }
    }
    check_non_workspace_manifest_forbidden_dependencies(
        repo_root,
        &BTreeSet::from(["mirante4d-core"]),
    )
}

fn check_non_workspace_manifest_forbidden_dependencies(
    repo_root: &Path,
    forbidden_packages: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    let mut manifests = Vec::new();
    collect_non_workspace_cargo_manifests(repo_root, repo_root, &mut manifests)?;
    manifests.sort();
    for manifest_path in manifests {
        let source = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest = source
            .parse::<toml::Table>()
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        let dependencies = manifest_dependency_package_names(&manifest)?;
        let forbidden = dependencies
            .iter()
            .filter(|dependency| forbidden_packages.contains(dependency.as_str()))
            .collect::<Vec<_>>();
        if !forbidden.is_empty() {
            bail!(
                "non-workspace target {} reaches deleted WP-07B predecessor packages: {forbidden:?}",
                manifest_path.display(),
            );
        }
    }
    Ok(())
}

fn collect_non_workspace_cargo_manifests(
    repo_root: &Path,
    directory: &Path,
    manifests: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == "crates" || name == "target" || name == "vendor" {
                continue;
            }
            collect_non_workspace_cargo_manifests(repo_root, &path, manifests)?;
        } else if file_type.is_file()
            && entry.file_name() == "Cargo.toml"
            && path != repo_root.join("Cargo.toml")
        {
            manifests.push(path);
        }
    }
    Ok(())
}

fn manifest_dependency_package_names(manifest: &toml::Table) -> anyhow::Result<BTreeSet<String>> {
    let mut dependencies = BTreeSet::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = manifest.get(key) {
            add_manifest_dependency_table(table, &mut dependencies, key)?;
        }
    }
    if let Some(targets) = manifest.get("target") {
        let targets = targets
            .as_table()
            .context("Cargo target dependencies must be a table")?;
        for (target, target_specification) in targets {
            let target_specification = target_specification
                .as_table()
                .with_context(|| format!("Cargo target {target:?} must be a table"))?;
            for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(table) = target_specification.get(key) {
                    add_manifest_dependency_table(
                        table,
                        &mut dependencies,
                        &format!("target.{target}.{key}"),
                    )?;
                }
            }
        }
    }
    Ok(dependencies)
}

fn add_manifest_dependency_table(
    value: &toml::Value,
    dependencies: &mut BTreeSet<String>,
    context: &str,
) -> anyhow::Result<()> {
    let table = value
        .as_table()
        .with_context(|| format!("Cargo {context} must be a table"))?;
    for (declared_name, specification) in table {
        let package_name = specification
            .as_table()
            .and_then(|specification| specification.get("package"))
            .and_then(toml::Value::as_str)
            .unwrap_or(declared_name);
        dependencies.insert(package_name.to_owned());
    }
    Ok(())
}

fn check_wp07a_model_contract(
    repo_root: &Path,
) -> anyhow::Result<(String, BTreeMap<String, BTreeSet<String>>)> {
    let path = repo_root.join(WP07A_MODEL_CONTRACT_PATH);
    let digest = sha256_file(&path)?;
    if digest != WP07A_MODEL_CONTRACT_SHA256 {
        bail!(
            "immutable WP-07A model contract drifted: expected {WP07A_MODEL_CONTRACT_SHA256}, found {digest}"
        );
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let contract: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if contract.get("schema").and_then(serde_json::Value::as_str)
        != Some("mirante4d-canonical-model-contract")
        || contract
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        bail!("{} has an unsupported schema", path.display());
    }
    if contract
        .pointer("/scope/product_reachable_in_wp07a")
        .and_then(serde_json::Value::as_bool)
        != Some(false)
    {
        bail!("WP-07A model contract must keep the new model unreachable from the product");
    }
    if contract
        .get("unresolved_decisions")
        .and_then(serde_json::Value::as_array)
        .is_none_or(|items| !items.is_empty())
    {
        bail!("WP-07A model contract must not retain unresolved state-model decisions");
    }
    let predecessor = contract
        .get("predecessor_revision")
        .and_then(serde_json::Value::as_str)
        .context("model contract predecessor_revision must be a string")?
        .to_owned();

    let crate_contracts = contract
        .get("crate_contracts")
        .and_then(serde_json::Value::as_array)
        .context("model contract crate_contracts must be an array")?;
    let workspace_metadata = workspace_dependency_metadata(repo_root)?;
    let expected = BTreeMap::from([
        ("mirante4d-domain", BTreeSet::new()),
        ("mirante4d-identity", BTreeSet::new()),
        (
            "mirante4d-project-model",
            BTreeSet::from(["mirante4d-domain", "mirante4d-identity"]),
        ),
    ]);
    let mut observed = BTreeMap::new();
    for crate_contract in crate_contracts {
        let name = crate_contract
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("model contract crate name must be a string")?;
        let crate_path = crate_contract
            .get("path")
            .and_then(serde_json::Value::as_str)
            .context("model contract crate path must be a string")?;
        if crate_path != format!("crates/{name}") {
            bail!("WP-07A crate {name} has unexpected contract path {crate_path:?}");
        }
        if repo_root.join(crate_path).join("build.rs").exists() {
            bail!("WP-07A crate {name} must not have a build script");
        }
        let package_id = workspace_metadata
            .workspace_package_ids_by_name
            .get(name)
            .with_context(|| format!("cargo metadata is missing WP-07A crate {name}"))?;
        if workspace_metadata
            .custom_build_package_ids
            .contains(package_id)
        {
            bail!("WP-07A crate {name} must not have a custom-build target");
        }
        let dependencies = crate_contract
            .get("permitted_normal_mirante4d_dependencies")
            .and_then(serde_json::Value::as_array)
            .context("model contract dependency allowlist must be an array")?
            .iter()
            .map(|dependency| {
                dependency
                    .as_str()
                    .context("model contract dependency must be a string")
            })
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        let external_dependencies = json_string_set(
            crate_contract,
            "permitted_normal_external_dependencies",
            "model contract external dependency allowlist",
        )?;
        let dev_dependencies = json_string_set(
            crate_contract,
            "permitted_dev_dependencies",
            "model contract dev-dependency allowlist",
        )?;
        let allowed_normal_dependencies = dependencies
            .iter()
            .copied()
            .chain(external_dependencies.iter().map(String::as_str))
            .collect::<BTreeSet<_>>();
        let actual_dependency_kinds = workspace_metadata
            .declared_dependency_kinds_by_name
            .get(name)
            .with_context(|| format!("cargo metadata is missing WP-07A crate {name}"))?;
        let actual_normal_dependencies = actual_dependency_kinds
            .get("normal")
            .cloned()
            .unwrap_or_default();
        let allowed_normal_dependencies = allowed_normal_dependencies
            .into_iter()
            .map(str::to_owned)
            .chain(
                wp08a::accepted_successor_normal_dependency_additions(name)
                    .iter()
                    .map(|dependency| (*dependency).to_owned()),
            )
            .collect::<BTreeSet<_>>();
        if actual_normal_dependencies != allowed_normal_dependencies {
            bail!(
                "WP-07A crate {name} normal dependencies drifted: expected={allowed_normal_dependencies:?}, actual={actual_normal_dependencies:?}"
            );
        }
        if actual_dependency_kinds
            .get("dev")
            .cloned()
            .unwrap_or_default()
            != dev_dependencies
            || actual_dependency_kinds
                .get("build")
                .is_some_and(|dependencies| !dependencies.is_empty())
        {
            bail!("WP-07A crate {name} dependency kinds drifted from the contract");
        }
        let side_effects = crate_contract
            .get("permitted_external_side_effects")
            .and_then(serde_json::Value::as_array)
            .context("model contract side-effect allowlist must be an array")?;
        if !side_effects.is_empty() {
            bail!("WP-07A crate {name} must not own external side effects");
        }
        let public_api = crate_contract
            .get("public_api")
            .and_then(serde_json::Value::as_array)
            .context("model contract public_api must be an array")?
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .context("model contract public API item must be a string")
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        if public_api.is_empty() {
            bail!("WP-07A crate {name} must freeze a nonempty public API list");
        }
        let public_api_set = public_api.iter().cloned().collect::<BTreeSet<_>>();
        if public_api_set.len() != public_api.len() {
            bail!("WP-07A crate {name} has a duplicate public API item");
        }
        let actual_public_api =
            public_root_api_names(&repo_root.join(crate_path).join("src/lib.rs"))?;
        let accepted_deletions = (name == "mirante4d-identity")
            .then_some(wp10a::accepted_identity_public_api_deletions())
            .into_iter()
            .flatten()
            .map(|item| (*item).to_owned())
            .collect::<BTreeSet<_>>();
        if !accepted_deletions.is_subset(&public_api_set) {
            bail!("WP-10A identity API deletion is absent from the WP-07A predecessor");
        }
        let accepted_public_api = public_api_set
            .difference(&accepted_deletions)
            .cloned()
            .chain(
                (name == "mirante4d-identity")
                    .then_some(wp10a::accepted_identity_public_api_additions())
                    .into_iter()
                    .flatten()
                    .map(|item| (*item).to_owned()),
            )
            .collect::<BTreeSet<_>>();
        if actual_public_api != accepted_public_api {
            let missing = accepted_public_api
                .difference(&actual_public_api)
                .cloned()
                .collect::<Vec<_>>();
            let unexpected = actual_public_api
                .difference(&accepted_public_api)
                .cloned()
                .collect::<Vec<_>>();
            bail!(
                "WP-07A crate {name} public API drifted: missing={missing:?}, unexpected={unexpected:?}"
            );
        }
        if observed.insert(name, dependencies).is_some() {
            bail!("duplicate WP-07A crate contract for {name}");
        }
    }
    if observed != expected {
        bail!("WP-07A crate dependency contracts do not match the frozen three-crate boundary");
    }
    let canonical_classes = contract
        .get("state_classes")
        .and_then(serde_json::Value::as_array)
        .context("model contract state_classes must be an array")?
        .iter()
        .map(|class| {
            class
                .get("class")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .context("model contract state class must have a string class")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let mapping = contract
        .get("field_ledger_state_class_mapping")
        .and_then(serde_json::Value::as_object)
        .context("model contract field-ledger state-class mapping must be an object")?
        .iter()
        .map(|(subtype, classes)| {
            let classes = classes
                .as_array()
                .context("field-ledger state-class mapping value must be an array")?
                .iter()
                .map(|class| {
                    class
                        .as_str()
                        .map(str::to_owned)
                        .context("field-ledger canonical state class must be a string")
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            if classes.is_empty() || !classes.is_subset(&canonical_classes) {
                bail!("field-ledger subtype {subtype:?} maps to unknown or empty classes");
            }
            Ok((subtype.clone(), classes))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    Ok((predecessor, mapping))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("failed to hash {}", path.display()))?;
    if !output.status.success() {
        bail!("sha256sum failed for {}", path.display());
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no digest")
}

fn json_string_set(
    object: &serde_json::Value,
    field: &str,
    context: &str,
) -> anyhow::Result<BTreeSet<String>> {
    object
        .get(field)
        .and_then(serde_json::Value::as_array)
        .with_context(|| format!("{context} must be an array"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .with_context(|| format!("{context} item must be a string"))
        })
        .collect()
}

#[derive(Debug)]
struct WorkspaceDependencyMetadata {
    declared_dependency_kinds_by_name: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    workspace_package_ids_by_name: BTreeMap<String, String>,
    custom_build_package_ids: BTreeSet<String>,
}

fn workspace_dependency_metadata(repo_root: &Path) -> anyhow::Result<WorkspaceDependencyMetadata> {
    validate_local_cargo_overrides(repo_root)?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version=1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(repo_root)
        .output()
        .context("failed to run cargo metadata for WP-07A dependency contract")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for WP-07A dependency contract: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    parse_workspace_dependency_metadata(&metadata)
}

fn validate_local_cargo_overrides(repo_root: &Path) -> anyhow::Result<()> {
    validate_repository_cargo_config(repo_root)?;
    let root_manifest_path = repo_root.join("Cargo.toml");
    let root_manifest = fs::read_to_string(&root_manifest_path)
        .with_context(|| format!("failed to read {}", root_manifest_path.display()))?;
    let actual = local_cargo_override_paths(&root_manifest)?;
    let expected = ALLOWED_LOCAL_CARGO_OVERRIDES
        .iter()
        .map(|(package, path)| ((*package).to_owned(), (*path).to_owned()))
        .collect::<BTreeMap<_, _>>();
    if actual != expected {
        bail!("local Cargo patch/replace paths drifted: expected={expected:?}, actual={actual:?}");
    }

    for (package, relative_path) in actual {
        let manifest_path = repo_root.join(&relative_path).join("Cargo.toml");
        let override_manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        reject_nested_local_cargo_overrides(&override_manifest, &relative_path)?;
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--manifest-path",
                manifest_path
                    .to_str()
                    .context("local Cargo override manifest path is not UTF-8")?,
                "--format-version=1",
                "--no-deps",
                "--locked",
                "--offline",
            ])
            .current_dir(repo_root)
            .output()
            .with_context(|| format!("failed to inspect local Cargo override {relative_path}"))?;
        if !output.status.success() {
            bail!(
                "cargo metadata failed for local Cargo override {relative_path}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let parsed = parse_workspace_dependency_metadata(&metadata)?;
        if parsed.workspace_package_ids_by_name.len() != 1
            || !parsed.workspace_package_ids_by_name.contains_key(&package)
        {
            bail!(
                "local Cargo override {relative_path} must contain only the frozen package {package}"
            );
        }
    }
    Ok(())
}

fn validate_repository_cargo_config(repo_root: &Path) -> anyhow::Result<()> {
    for relative_path in [".cargo/config.toml", ".cargo/config"] {
        let path = repo_root.join(relative_path);
        if !path.exists() {
            continue;
        }
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config = source
            .parse::<toml::Table>()
            .with_context(|| format!("failed to parse {}", path.display()))?;
        validate_repository_cargo_config_table(&config, relative_path)?;
    }
    Ok(())
}

fn validate_repository_cargo_config_table(
    config: &toml::Table,
    relative_path: &str,
) -> anyhow::Result<()> {
    if config.get("paths").is_some() {
        bail!("{relative_path} must not define Cargo path overrides");
    }
    if config.get("patch").is_some() {
        bail!("{relative_path} must not define Cargo patch overrides");
    }
    if let Some(sources) = config.get("source").and_then(toml::Value::as_table) {
        for (name, specification) in sources {
            let specification = specification
                .as_table()
                .context("Cargo source configuration must be a table")?;
            let forbidden = ["replace-with", "local-registry", "directory"]
                .into_iter()
                .filter(|key| specification.contains_key(*key))
                .collect::<Vec<_>>();
            if !forbidden.is_empty() {
                bail!(
                    "{relative_path} source {name:?} defines forbidden replacement keys {forbidden:?}"
                );
            }
        }
    }
    Ok(())
}

fn reject_nested_local_cargo_overrides(manifest: &str, relative_path: &str) -> anyhow::Result<()> {
    let nested = local_cargo_override_paths(manifest)?;
    if nested.is_empty() {
        Ok(())
    } else {
        bail!("local Cargo override {relative_path} defines nested local overrides: {nested:?}")
    }
}

fn local_cargo_override_paths(manifest: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let manifest = manifest
        .parse::<toml::Table>()
        .context("failed to parse root Cargo.toml while checking local overrides")?;
    let mut paths = BTreeMap::new();

    if let Some(registries) = manifest.get("patch").and_then(toml::Value::as_table) {
        for packages in registries.values() {
            let packages = packages
                .as_table()
                .context("Cargo [patch] registry must be a table")?;
            for (package, specification) in packages {
                insert_local_cargo_override(&mut paths, package, specification)?;
            }
        }
    }
    if let Some(replacements) = manifest.get("replace").and_then(toml::Value::as_table) {
        for (package, specification) in replacements {
            insert_local_cargo_override(&mut paths, package, specification)?;
        }
    }
    Ok(paths)
}

fn insert_local_cargo_override(
    paths: &mut BTreeMap<String, String>,
    package: &str,
    specification: &toml::Value,
) -> anyhow::Result<()> {
    let Some(path) = specification.as_table().and_then(|table| table.get("path")) else {
        return Ok(());
    };
    let path = path
        .as_str()
        .context("local Cargo patch/replace path must be a string")?;
    if paths.insert(package.to_owned(), path.to_owned()).is_some() {
        bail!("duplicate local Cargo override for package {package}");
    }
    Ok(())
}

fn parse_workspace_dependency_metadata(
    metadata: &serde_json::Value,
) -> anyhow::Result<WorkspaceDependencyMetadata> {
    let workspace_member_ids = metadata
        .get("workspace_members")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata has no workspace_members array")?
        .iter()
        .map(|id| {
            id.as_str()
                .map(str::to_owned)
                .context("cargo metadata workspace member ID must be a string")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let packages = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata has no packages array")?;
    let workspace_packages_by_path = packages
        .iter()
        .filter_map(|package| {
            let id = package.get("id")?.as_str()?;
            workspace_member_ids.contains(id).then_some(package)
        })
        .map(|package| {
            let name = package
                .get("name")
                .and_then(serde_json::Value::as_str)
                .context("cargo metadata workspace package has no name")?;
            let manifest_path = package
                .get("manifest_path")
                .and_then(serde_json::Value::as_str)
                .context("cargo metadata workspace package has no manifest_path")?;
            let package_path = Path::new(manifest_path)
                .parent()
                .map(|path| path.to_string_lossy().into_owned())
                .context("cargo metadata workspace manifest has no parent directory")?;
            Ok((package_path, name.to_owned()))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let mut declared_dependency_kinds_by_name = BTreeMap::new();
    let mut workspace_package_ids_by_name = BTreeMap::new();
    let mut custom_build_package_ids = BTreeSet::new();
    let mut seen_workspace_member_ids = BTreeSet::new();
    for package in packages {
        let id = package
            .get("id")
            .and_then(serde_json::Value::as_str)
            .context("cargo metadata package has no ID")?;
        let name = package
            .get("name")
            .and_then(serde_json::Value::as_str)
            .context("cargo metadata package has no name")?;
        if !workspace_member_ids.contains(id) {
            continue;
        }
        seen_workspace_member_ids.insert(id.to_owned());
        if workspace_package_ids_by_name
            .insert(name.to_owned(), id.to_owned())
            .is_some()
        {
            bail!("cargo workspace contains duplicate package name {name:?}");
        }
        let mut kinds = BTreeMap::<String, BTreeSet<String>>::new();
        for dependency in package
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .context("cargo metadata package has no dependencies array")?
        {
            let dependency_name = dependency
                .get("name")
                .and_then(serde_json::Value::as_str)
                .context("cargo metadata dependency has no name")?;
            let kind = dependency
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("normal");
            let dependency_name = match dependency.get("path").and_then(serde_json::Value::as_str) {
                Some(path) => workspace_packages_by_path.get(path).with_context(|| {
                    format!(
                        "workspace package {name} declares non-workspace local path dependency {dependency_name} at {path}; WP-07A forbids path wrappers around preparatory crates"
                    )
                })?,
                None => dependency_name,
            };
            kinds
                .entry(kind.to_owned())
                .or_default()
                .insert(dependency_name.to_owned());
        }
        declared_dependency_kinds_by_name.insert(name.to_owned(), kinds);
        if package_has_custom_build_target(package)? {
            custom_build_package_ids.insert(id.to_owned());
        }
    }
    if seen_workspace_member_ids != workspace_member_ids {
        let missing = workspace_member_ids
            .difference(&seen_workspace_member_ids)
            .collect::<Vec<_>>();
        bail!("cargo metadata omits workspace member packages: {missing:?}");
    }

    Ok(WorkspaceDependencyMetadata {
        declared_dependency_kinds_by_name,
        workspace_package_ids_by_name,
        custom_build_package_ids,
    })
}

fn package_has_custom_build_target(package: &serde_json::Value) -> anyhow::Result<bool> {
    for target in package
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .context("cargo metadata package has no targets array")?
    {
        let kinds = target
            .get("kind")
            .and_then(serde_json::Value::as_array)
            .context("cargo metadata target has no kind array")?;
        for kind in kinds {
            let kind = kind
                .as_str()
                .context("cargo metadata target kind must be a string")?;
            if kind == "custom-build" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn public_root_api_names(path: &Path) -> anyhow::Result<BTreeSet<String>> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read public API root {}", path.display()))?;
    let file = syn::parse_file(&source)
        .with_context(|| format!("failed to parse public API root {}", path.display()))?;
    let mut names = BTreeSet::new();
    for item in file.items {
        let (visibility, name) = match item {
            syn::Item::Const(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Enum(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::ExternCrate(item) => (
                item.vis,
                Some(
                    item.rename
                        .map_or_else(|| item.ident.to_string(), |(_, rename)| rename.to_string()),
                ),
            ),
            syn::Item::Fn(item) => (item.vis, Some(item.sig.ident.to_string())),
            syn::Item::Mod(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Static(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Struct(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Trait(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::TraitAlias(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Type(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Union(item) => (item.vis, Some(item.ident.to_string())),
            syn::Item::Use(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                add_public_use_tree_names(&item.tree, &mut names)?;
                continue;
            }
            _ => continue,
        };
        if matches!(visibility, syn::Visibility::Public(_)) {
            let name = name.context("public root item has no name")?;
            if !names.insert(name.clone()) {
                bail!("duplicate public root item {name:?} in {}", path.display());
            }
        }
    }
    Ok(names)
}

fn add_public_use_tree_names(
    tree: &syn::UseTree,
    names: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    match tree {
        syn::UseTree::Name(name) => {
            if !names.insert(name.ident.to_string()) {
                bail!("duplicate public-use item {:?}", name.ident);
            }
        }
        syn::UseTree::Rename(rename) => {
            if !names.insert(rename.rename.to_string()) {
                bail!("duplicate public-use item {:?}", rename.rename);
            }
        }
        syn::UseTree::Path(path) => add_public_use_tree_names(&path.tree, names)?,
        syn::UseTree::Group(group) => {
            for item in &group.items {
                add_public_use_tree_names(item, names)?;
            }
        }
        syn::UseTree::Glob(_) => bail!("glob public exports are forbidden in WP-07A crates"),
    }
    Ok(())
}

fn rust_struct_field_names(source: &str, struct_name: &str) -> anyhow::Result<Vec<String>> {
    let file =
        syn::parse_file(source).context("failed to parse Rust source for field inventory")?;
    let matches = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Struct(item) if item.ident == struct_name => Some(item),
            _ => None,
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        bail!("expected exactly one top-level struct {struct_name}");
    }
    let syn::Fields::Named(fields) = &matches[0].fields else {
        bail!("inventory struct {struct_name} must have named fields");
    };
    fields
        .named
        .iter()
        .map(|field| {
            field
                .ident
                .as_ref()
                .map(ToString::to_string)
                .context("named field has no identifier")
        })
        .collect()
}

fn rust_struct_field_terminal_type_names(
    source: &str,
    struct_name: &str,
) -> anyhow::Result<BTreeMap<String, String>> {
    let file = syn::parse_file(source).context("failed to parse Rust source for type inventory")?;
    let item = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == struct_name => Some(item),
            _ => None,
        })
        .with_context(|| format!("expected one top-level struct {struct_name}"))?;
    let syn::Fields::Named(fields) = &item.fields else {
        bail!("inventory struct {struct_name} must have named fields");
    };
    fields
        .named
        .iter()
        .map(|field| {
            let name = field
                .ident
                .as_ref()
                .map(ToString::to_string)
                .context("named field has no identifier")?;
            let type_name = terminal_type_name(&field.ty)
                .with_context(|| format!("{struct_name}.{name} has an unsupported field type"))?;
            Ok((name, type_name))
        })
        .collect()
}

fn terminal_type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Group(group) => terminal_type_name(&group.elem),
        syn::Type::Paren(paren) => terminal_type_name(&paren.elem),
        syn::Type::Reference(reference) => terminal_type_name(&reference.elem),
        syn::Type::Path(path) => {
            let segment = path.path.segments.last()?;
            if let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments
                && let Some(inner) = arguments.args.iter().find_map(|argument| match argument {
                    syn::GenericArgument::Type(ty) => Some(ty),
                    _ => None,
                })
            {
                terminal_type_name(inner)
            } else {
                Some(segment.ident.to_string())
            }
        }
        _ => None,
    }
}

fn private_root_module_names(source: &str) -> anyhow::Result<BTreeSet<String>> {
    let file =
        syn::parse_file(source).context("failed to parse Rust source for module inventory")?;
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(item) => Some(item),
            _ => None,
        })
        .map(|module| {
            if !matches!(module.vis, syn::Visibility::Inherited) {
                bail!("module {} must remain private", module.ident);
            }
            if module.content.is_some() {
                bail!("module {} must remain file-backed", module.ident);
            }
            Ok(module.ident.to_string())
        })
        .collect()
}

fn crate_private_root_module_names(source: &str) -> anyhow::Result<BTreeSet<String>> {
    let file =
        syn::parse_file(source).context("failed to parse Rust source for module inventory")?;
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(item) => Some(item),
            _ => None,
        })
        .map(|module| {
            if matches!(module.vis, syn::Visibility::Public(_)) {
                bail!("module {} must not be public", module.ident);
            }
            if module.content.is_some() {
                bail!("module {} must remain file-backed", module.ident);
            }
            Ok(module.ident.to_string())
        })
        .collect()
}

fn rust_root_defined_item_names(source: &str) -> anyhow::Result<BTreeSet<String>> {
    let file =
        syn::parse_file(source).context("failed to parse Rust source for root API inventory")?;
    Ok(file
        .items
        .into_iter()
        .filter_map(|item| match item {
            syn::Item::Const(item) => Some(item.ident.to_string()),
            syn::Item::Enum(item) => Some(item.ident.to_string()),
            syn::Item::Fn(item) => Some(item.sig.ident.to_string()),
            syn::Item::Mod(item) => Some(item.ident.to_string()),
            syn::Item::Static(item) => Some(item.ident.to_string()),
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Trait(item) => Some(item.ident.to_string()),
            syn::Item::TraitAlias(item) => Some(item.ident.to_string()),
            syn::Item::Type(item) => Some(item.ident.to_string()),
            syn::Item::Union(item) => Some(item.ident.to_string()),
            _ => None,
        })
        .collect())
}

fn source_contains_identifier(source: &str, identifier: &str) -> bool {
    source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|token| token == identifier)
}

fn axis_aligned_2d_chunk_dependency_violations(path: &Path, source: &str) -> Vec<String> {
    source_pattern_violations(
        path,
        source,
        FORBIDDEN_AXIS_ALIGNED_2D_CHUNK_PATTERNS,
        "implementation must not depend on axis-aligned 2D slice chunk layouts",
    )
}

fn dumping_ground_module_name_violation(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    if !FORBIDDEN_DUMPING_GROUND_MODULE_NAMES.contains(&file_name) {
        return None;
    }
    Some(format!(
        "{} uses forbidden dumping-ground module name {file_name:?}; choose a domain-specific module name",
        path.display()
    ))
}

fn source_pattern_violations(
    path: &Path,
    source: &str,
    patterns: &[&str],
    message: &str,
) -> Vec<String> {
    source
        .lines()
        .enumerate()
        .flat_map(|(line_index, line)| {
            patterns.iter().filter_map(move |pattern| {
                if line.contains(pattern) {
                    Some(format!(
                        "{}:{}: {message}: found {pattern:?}",
                        path.display(),
                        line_index + 1
                    ))
                } else {
                    None
                }
            })
        })
        .collect()
}

fn collect_rust_source_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rust_source_files_inner(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rust_source_files_inner(root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries = fs::read_dir(root)
        .with_context(|| format!("failed to read source directory {}", root.display()))?;
    for entry in entries {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", root.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_source_files_inner(&path, files)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            files.push(path);
        }
    }
    Ok(())
}

fn check_tracked_artifact_policy() -> anyhow::Result<()> {
    let files = tracked_repository_files()?;
    let mut violations = Vec::new();
    for path in files {
        if !path.exists() {
            continue;
        }
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to inspect tracked file {}", path.display()))?;
        if let Some(violation) = tracked_artifact_policy_violation(&path, metadata.len()) {
            violations.push(violation);
        }
    }
    if !violations.is_empty() {
        bail!("tracked artifact policy failed:\n{}", violations.join("\n"));
    }
    Ok(())
}

fn tracked_repository_files() -> anyhow::Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-files", "-z"])
        .output()
        .context("failed to run git ls-files for artifact policy check")?;
    if !output.status.success() {
        bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(String::from_utf8_lossy(entry).into_owned()))
        .collect())
}

fn tracked_artifact_policy_violation(path: &Path, byte_count: u64) -> Option<String> {
    let normalized = normalize_repo_path(path);
    for forbidden_prefix in ["target/", ".nextest/", "sample_data/"] {
        if normalized.starts_with(forbidden_prefix) {
            return Some(format!(
                "{} is a generated/local artifact path and must not be tracked",
                path.display()
            ));
        }
    }
    if byte_count > MAX_TRACKED_GENERATED_ARTIFACT_BYTES
        && has_generated_data_extension(&normalized)
    {
        return Some(format!(
            "{} is a large generated/data artifact ({} bytes) and must not be tracked",
            path.display(),
            byte_count
        ));
    }
    None
}

fn has_generated_data_extension(normalized_path: &str) -> bool {
    [
        ".czi",
        ".h5",
        ".hdf5",
        ".lif",
        ".m4d",
        ".m4dproj",
        ".mrc",
        ".nd2",
        ".npy",
        ".npz",
        ".ome.tif",
        ".ome.tiff",
        ".tif",
        ".tiff",
        ".zarr",
    ]
    .iter()
    .any(|extension| normalized_path.ends_with(extension))
}

fn normalize_repo_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_architecture_policy_rejects_ui_imports_outside_app_crate() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-data/src/lib.rs"),
            "use egui::Context;\n",
        );

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("non-app crate must not import UI/app layer"));
    }

    #[test]
    fn source_architecture_policy_allows_ui_imports_inside_app_crate() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-app/src/lib.rs"),
            "use egui::Context;\nuse rfd::FileDialog;\n",
        );

        assert!(violations.is_empty());
    }

    #[test]
    fn source_architecture_policy_accepts_current_resident_rendering_bridge_only() {
        let path = Path::new("crates/mirante4d-app/src/resident_rendering.rs");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(repo_root.join(path)).unwrap();
        let violations = source_architecture_violations(path, &source);

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn source_architecture_policy_accepts_current_app_root() {
        let path = Path::new("crates/mirante4d-app/src/lib.rs");
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source = fs::read_to_string(repo_root.join(path)).unwrap();
        let violations = source_architecture_violations(path, &source);

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn source_architecture_policy_rejects_renderer_file_io() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-renderer/src/lib.rs"),
            "use std::fs;\nlet _ = File::open(path);\n",
        );

        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("renderer source must not perform direct filesystem I/O"));
    }

    #[test]
    fn source_architecture_policy_rejects_dumping_ground_module_names() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-data/src/utils.rs"),
            "pub fn unrelated() {}\n",
        );

        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("forbidden dumping-ground module name"));
    }

    #[test]
    fn source_architecture_policy_rejects_axis_aligned_2d_slice_chunk_dependency() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-app/src/cross_section_runtime.rs"),
            "const REQUIRED_2D_CHUNK_SHAPE: (u32, u32, u32) = (512, 512, 1);\nstruct SliceChunk;\n",
        );

        assert_eq!(violations.len(), 2);
        assert!(violations[0].contains("axis-aligned 2D slice chunk layouts"));
        assert!(violations[1].contains("axis-aligned 2D slice chunk layouts"));
    }

    #[test]
    fn source_architecture_policy_accepts_current_sources_without_axis_aligned_2d_chunks() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut violations = Vec::new();
        for path in collect_rust_source_files(&repo_root.join("crates")).unwrap() {
            let relative_path = path.strip_prefix(&repo_root).unwrap();
            if normalize_repo_path(relative_path).starts_with("crates/xtask/") {
                continue;
            }
            let source = fs::read_to_string(&path).unwrap();
            violations.extend(axis_aligned_2d_chunk_dependency_violations(
                relative_path,
                &source,
            ));
        }

        assert!(violations.is_empty(), "{violations:#?}");
    }

    #[test]
    fn rust_struct_field_extraction_handles_visibility_attributes_and_split_types() {
        let source = r#"
#[derive(Debug)]
pub struct Example {
    pub visible: u64,
    pub(crate) scoped: Vec<String>,
    pub(self) restricted: u8,
    private:
        Option<String>,
    #[cfg(test)]
    test_only: bool,
}
"#;

        assert_eq!(
            rust_struct_field_names(source, "Example").unwrap(),
            ["visible", "scoped", "restricted", "private", "test_only"]
        );
    }

    #[test]
    fn wp07a_model_contract_matches_the_repository() {
        let nested_use_violations = forbidden_canonical_model_std_use_violations(
            Path::new("crates/mirante4d-domain/src/nested.rs"),
            r#"
mod nested {
    fn read() {
        use std::{fs as disk, path::{Path, PathBuf}};
    }
}
"#,
        );
        assert_eq!(nested_use_violations.len(), 3);
        assert!(
            nested_use_violations
                .iter()
                .any(|violation| violation.ends_with("std::fs"))
        );
        assert_eq!(
            nested_use_violations
                .iter()
                .filter(|violation| violation.contains("std::path::"))
                .count(),
            2
        );

        assert_eq!(
            local_cargo_override_paths(
                r#"
[patch.crates-io]
local-wrapper = { path = "vendor/local-wrapper" }
"#,
            )
            .unwrap(),
            BTreeMap::from([(
                "local-wrapper".to_owned(),
                "vendor/local-wrapper".to_owned(),
            )])
        );
        assert!(
            reject_nested_local_cargo_overrides(
                r#"
[patch.crates-io]
mirante4d-domain = { path = "../../crates/mirante4d-domain" }
"#,
                "vendor/local-wrapper",
            )
            .unwrap_err()
            .to_string()
            .contains("nested local overrides")
        );
        for config in [
            r#"paths = ["../wrapper"]"#,
            r#"
[patch.crates-io]
mirante4d-domain = { path = "crates/mirante4d-domain" }
"#,
            r#"
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
"#,
        ] {
            let config = config.parse::<toml::Table>().unwrap();
            assert!(validate_repository_cargo_config_table(&config, ".cargo/config.toml").is_err());
        }
        let patched_wrapper_metadata = serde_json::json!({
            "workspace_members": ["path+file:///repo/vendor/local-wrapper#0.1.0"],
            "packages": [{
                "id": "path+file:///repo/vendor/local-wrapper#0.1.0",
                "name": "local-wrapper",
                "manifest_path": "/repo/vendor/local-wrapper/Cargo.toml",
                "dependencies": [{
                    "name": "mirante4d-domain",
                    "kind": null,
                    "source": null,
                    "path": "/repo/crates/mirante4d-domain",
                    "target": "cfg(target_os = \"macos\")"
                }],
                "targets": [{ "kind": ["lib"] }]
            }],
            "resolve": null
        });
        assert!(
            parse_workspace_dependency_metadata(&patched_wrapper_metadata)
                .unwrap_err()
                .to_string()
                .contains("non-workspace local path dependency")
        );
        assert!(
            package_has_custom_build_target(&serde_json::json!({
                "targets": [
                    { "kind": ["lib"] },
                    { "kind": ["custom-build"], "src_path": "custom/build-location.rs" }
                ]
            }))
            .unwrap()
        );

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");

        check_wp07a_contracts(&repo_root).unwrap();
    }

    #[test]
    fn wp07b_live_cutover_matches_the_repository() {
        let forbidden = BTreeSet::from(["fs"]);
        let violations = forbidden_std_authority_violations(
            Path::new("crates/example/src/lib.rs"),
            r#"
fn direct() { let _ = std::fs::read("x"); }
mod root_alias { use std as platform; }
mod self_alias { use std::{self as platform}; }
extern crate std as platform_std;
"#,
            &forbidden,
            "test boundary",
        );
        assert_eq!(violations.len(), 4, "{violations:#?}");

        let external_manifest = r#"
[package]
name = "external-target"
version = "0.0.0"
[dev-dependencies]
app-boundary = { package = "mirante4d-application", path = "../application" }
[target.'cfg(unix)'.build-dependencies]
mirante4d-settings = { path = "../settings" }
"#
        .parse::<toml::Table>()
        .unwrap();
        assert_eq!(
            manifest_dependency_package_names(&external_manifest).unwrap(),
            BTreeSet::from([
                "mirante4d-application".to_owned(),
                "mirante4d-settings".to_owned(),
            ])
        );

        // The repository-wide contracts run once in the policy architecture
        // self-check. This unit case covers their parsing helpers without
        // recursively repeating the expensive repository scan.
    }

    #[test]
    fn tracked_artifact_policy_rejects_generated_paths_and_large_data_files() {
        assert!(
            tracked_artifact_policy_violation(Path::new("target/mirante4d/out.bin"), 1)
                .unwrap()
                .contains("must not be tracked")
        );
        assert!(
            tracked_artifact_policy_violation(
                Path::new("fixtures/large-source.ome.tiff"),
                MAX_TRACKED_GENERATED_ARTIFACT_BYTES + 1,
            )
            .unwrap()
            .contains("large generated/data artifact")
        );
        assert!(
            tracked_artifact_policy_violation(
                Path::new("docs/ARCHITECTURE.md"),
                MAX_TRACKED_GENERATED_ARTIFACT_BYTES + 1,
            )
            .is_none()
        );
    }
}
