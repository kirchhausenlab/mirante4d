use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use syn::visit::Visit;

const MAX_TRACKED_GENERATED_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024;
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

const REQUIRED_CRATES: &[(&str, &str)] = &[
    ("mirante4d-analysis-core", "crates/mirante4d-analysis-core"),
    (
        "mirante4d-analysis-runtime",
        "crates/mirante4d-analysis-runtime",
    ),
    ("mirante4d-app", "crates/mirante4d-app"),
    ("mirante4d-application", "crates/mirante4d-application"),
    ("mirante4d-dataset", "crates/mirante4d-dataset"),
    (
        "mirante4d-dataset-runtime",
        "crates/mirante4d-dataset-runtime",
    ),
    ("mirante4d-domain", "crates/mirante4d-domain"),
    ("mirante4d-identity", "crates/mirante4d-identity"),
    (
        "mirante4d-import-pipeline",
        "crates/mirante4d-import-pipeline",
    ),
    ("mirante4d-project-model", "crates/mirante4d-project-model"),
    ("mirante4d-project-store", "crates/mirante4d-project-store"),
    ("mirante4d-render-api", "crates/mirante4d-render-api"),
    (
        "mirante4d-render-reference",
        "crates/mirante4d-render-reference",
    ),
    ("mirante4d-render-wgpu", "crates/mirante4d-render-wgpu"),
    ("mirante4d-settings", "crates/mirante4d-settings"),
    ("mirante4d-storage", "crates/mirante4d-storage"),
    ("mirante4d-ui-egui", "crates/mirante4d-ui-egui"),
    ("xtask", "crates/xtask"),
];

const FORBIDDEN_CRATE_PATHS: &[&str] = &[
    "crates/mirante4d-analysis",
    "crates/mirante4d-core",
    "crates/mirante4d-data",
    "crates/mirante4d-format",
    "crates/mirante4d-import",
    "crates/mirante4d-preprocess",
    "crates/mirante4d-renderer",
];

const FORBIDDEN_CRATE_PACKAGES: &[&str] = &[
    "mirante4d-analysis",
    "mirante4d-core",
    "mirante4d-data",
    "mirante4d-format",
    "mirante4d-import",
    "mirante4d-preprocess",
    "mirante4d-renderer",
];

const NO_CUSTOM_BUILD_CRATES: &[&str] = &[
    "mirante4d-application",
    "mirante4d-dataset",
    "mirante4d-domain",
    "mirante4d-identity",
    "mirante4d-project-model",
    "mirante4d-render-api",
    "mirante4d-settings",
];

pub(crate) fn architecture_self_check() -> anyhow::Result<()> {
    let repo_root = Path::new(".");
    check_crate_paths(repo_root)?;
    check_source_architecture_policy(repo_root)?;
    check_boundary_source_ownership(repo_root)?;
    let metadata = workspace_dependency_metadata(repo_root)?;
    check_dependency_direction(repo_root, &metadata)?;
    check_current_state_ownership(repo_root, &metadata)?;
    check_tracked_artifact_policy()?;
    Ok(())
}

fn check_crate_paths(repo_root: &Path) -> anyhow::Result<()> {
    for (_, relative_path) in REQUIRED_CRATES {
        if !repo_root.join(relative_path).is_dir() {
            bail!("required crate directory is missing: {relative_path}");
        }
    }
    for relative_path in FORBIDDEN_CRATE_PATHS {
        if repo_root.join(relative_path).exists() {
            bail!("retired or unowned crate directory exists: {relative_path}");
        }
    }
    Ok(())
}

fn check_source_architecture_policy(repo_root: &Path) -> anyhow::Result<()> {
    let mut violations = Vec::new();
    for path in collect_rust_source_files(&repo_root.join("crates"))? {
        let relative = path.strip_prefix(repo_root).unwrap_or(&path);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read source file {}", path.display()))?;
        violations.extend(source_architecture_violations(relative, &source));
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
    let ui_layer = normalized.starts_with("crates/mirante4d-app/")
        || normalized.starts_with("crates/mirante4d-ui-egui/");
    if !ui_layer {
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
            "non-UI crate must not import the UI or native app layer",
        ));
    }
    if normalized.starts_with("crates/mirante4d-render-wgpu/src/") {
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
            "product renderer source must not perform direct filesystem I/O",
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
            "canonical model crate must remain independent of runtime and product frameworks",
        ));
        violations.extend(forbidden_canonical_model_std_use_violations(path, source));
    }
    violations
}

fn forbidden_canonical_model_std_use_violations(path: &Path, source: &str) -> Vec<String> {
    let forbidden = BTreeSet::from([
        "env", "fs", "io", "net", "os", "path", "process", "sync", "thread", "time",
    ]);
    forbidden_std_authority_violations(path, source, &forbidden, "canonical model crate")
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

fn check_boundary_source_ownership(repo_root: &Path) -> anyhow::Result<()> {
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
    for crate_name in [
        "mirante4d-application",
        "mirante4d-dataset",
        "mirante4d-render-api",
        "mirante4d-settings",
    ] {
        let source_root = repo_root.join("crates").join(crate_name).join("src");
        for path in collect_rust_source_files(&source_root)? {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            violations.extend(source_pattern_violations(
                &path,
                &source,
                &forbidden_frameworks,
                "foundation boundary crate imports a runtime or UI authority",
            ));
            let (forbidden_std, policy) = if crate_name == "mirante4d-settings" {
                (&settings_forbidden_std, "settings crate")
            } else {
                (&pure_forbidden_std, "pure foundation boundary crate")
            };
            let mut std_violations =
                forbidden_std_authority_violations(&path, &source, forbidden_std, policy);
            if crate_name == "mirante4d-application"
                && path == source_root.join("project_store_service.rs")
            {
                let expected = BTreeSet::from(["Duration".to_owned(), "Instant".to_owned()]);
                let prefix = format!(
                    "{}: {policy} imports forbidden std authority std::time::",
                    path.display()
                );
                let mut observed = BTreeSet::new();
                std_violations.retain(|violation| {
                    let Some(authority) = violation.strip_prefix(&prefix) else {
                        return true;
                    };
                    if expected.contains(authority) {
                        observed.insert(authority.to_owned());
                        false
                    } else {
                        true
                    }
                });
                if observed != expected {
                    std_violations.push(format!(
                        "{}: application clock authority drifted: expected={expected:?}, observed={observed:?}",
                        path.display()
                    ));
                }
            }
            violations.extend(std_violations);
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        bail!(
            "foundation boundary source ownership failed:\n{}",
            violations.join("\n")
        )
    }
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
        .context("failed to run cargo metadata for architecture checks")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for architecture checks: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    parse_workspace_dependency_metadata(&metadata)
}

fn check_dependency_direction(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let expected_packages = REQUIRED_CRATES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let actual_packages = metadata
        .declared_dependency_kinds_by_name
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual_packages != expected_packages {
        bail!(
            "workspace package set drifted: missing={:?}, unexpected={:?}",
            expected_packages
                .difference(&actual_packages)
                .collect::<Vec<_>>(),
            actual_packages
                .difference(&expected_packages)
                .collect::<Vec<_>>()
        );
    }

    for crate_name in NO_CUSTOM_BUILD_CRATES {
        let package_id = metadata
            .workspace_package_ids_by_name
            .get(*crate_name)
            .with_context(|| format!("cargo metadata is missing {crate_name}"))?;
        if metadata.custom_build_package_ids.contains(package_id) {
            bail!("foundation boundary crate {crate_name} must not have a custom build target");
        }
    }

    let forbidden_packages = FORBIDDEN_CRATE_PACKAGES
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    for (package, kinds) in &metadata.declared_dependency_kinds_by_name {
        for (kind, dependencies) in kinds {
            for dependency in dependencies {
                if forbidden_packages.contains(dependency.as_str()) {
                    bail!("{package} has a {kind} dependency on retired package {dependency}");
                }
                if dependency == "mirante4d-app" {
                    bail!("{package} must not depend on the native application crate");
                }
                if dependency == "mirante4d-ui-egui" && package != "mirante4d-app" {
                    bail!("{package} must not depend on the UI crate");
                }
            }
        }

        for dependency in kinds.get("normal").into_iter().flatten() {
            let Some(source_layer) = package_layer(package) else {
                continue;
            };
            let Some(dependency_layer) = package_layer(dependency) else {
                continue;
            };
            if dependency_layer >= source_layer {
                bail!(
                    "normal dependency points upward: {package} (layer {source_layer}) -> {dependency} (layer {dependency_layer})"
                );
            }
        }
    }

    let app_normal = normal_dependencies(metadata, "mirante4d-app")?;
    let required_app_dependencies = BTreeSet::from(
        [
            "mirante4d-application",
            "mirante4d-dataset",
            "mirante4d-project-store",
            "mirante4d-render-api",
            "mirante4d-render-wgpu",
            "mirante4d-settings",
            "mirante4d-storage",
            "mirante4d-ui-egui",
        ]
        .map(str::to_owned),
    );
    if !required_app_dependencies.is_subset(app_normal) {
        bail!(
            "native app is missing foundation dependencies: {:?}",
            required_app_dependencies
                .difference(app_normal)
                .collect::<Vec<_>>()
        );
    }

    let workspace_packages = metadata
        .declared_dependency_kinds_by_name
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let ui_internal = normal_dependencies(metadata, "mirante4d-ui-egui")?
        .iter()
        .filter(|dependency| workspace_packages.contains(dependency.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    if ui_internal != BTreeSet::from(["mirante4d-application".to_owned()]) {
        bail!("UI crate must depend on exactly mirante4d-application within the workspace");
    }

    let renderer_internal = normal_dependencies(metadata, "mirante4d-render-wgpu")?
        .iter()
        .filter(|dependency| workspace_packages.contains(dependency.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_renderer_internal = BTreeSet::from([
        "mirante4d-dataset".to_owned(),
        "mirante4d-render-api".to_owned(),
    ]);
    if renderer_internal != expected_renderer_internal {
        bail!(
            "product renderer workspace dependencies drifted: expected={expected_renderer_internal:?}, actual={renderer_internal:?}"
        );
    }

    check_non_workspace_manifest_forbidden_dependencies(repo_root, &forbidden_packages)
}

fn normal_dependencies<'a>(
    metadata: &'a WorkspaceDependencyMetadata,
    package: &str,
) -> anyhow::Result<&'a BTreeSet<String>> {
    metadata
        .declared_dependency_kinds_by_name
        .get(package)
        .and_then(|kinds| kinds.get("normal"))
        .with_context(|| format!("cargo metadata is missing {package} normal dependencies"))
}

fn package_layer(package: &str) -> Option<u8> {
    match package {
        "mirante4d-domain" | "mirante4d-settings" => Some(0),
        "mirante4d-identity" => Some(1),
        "mirante4d-dataset" | "mirante4d-project-model" => Some(2),
        "mirante4d-analysis-core"
        | "mirante4d-project-store"
        | "mirante4d-render-api"
        | "mirante4d-storage" => Some(3),
        "mirante4d-dataset-runtime"
        | "mirante4d-import-pipeline"
        | "mirante4d-render-reference" => Some(4),
        "mirante4d-analysis-runtime" | "mirante4d-application" | "mirante4d-render-wgpu" => Some(5),
        "mirante4d-ui-egui" => Some(6),
        "mirante4d-app" => Some(7),
        "xtask" => Some(8),
        _ => None,
    }
}

fn check_current_state_ownership(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let ledger_path = repo_root.join("architecture/current-state-field-ledger.json");
    let ledger: serde_json::Value = serde_json::from_slice(
        &fs::read(&ledger_path)
            .with_context(|| format!("failed to read {}", ledger_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", ledger_path.display()))?;
    if ledger.get("schema").and_then(serde_json::Value::as_str)
        != Some("mirante4d-current-state-field-ledger")
        || ledger
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(2)
    {
        bail!(
            "{} is not a supported current-state ledger",
            ledger_path.display()
        );
    }

    let expected_dataset_authority = serde_json::json!({
        "runtime_owner": "mirante4d-dataset-runtime",
        "composition_state": "DatasetDemandState",
        "sole_poll_owner": "DatasetRequestDispatcher",
        "lease_retention": {
            "type": "RetainedLeases",
            "path": "crates/mirante4d-app/src/retained_leases.rs"
        },
        "source": {
            "type": "LocalDatasetSource",
            "path": "crates/mirante4d-storage/src/dataset_source.rs"
        }
    });
    if ledger.get("dataset_authority") != Some(&expected_dataset_authority) {
        bail!("current dataset authority ledger drifted");
    }
    let expected_render_authority = serde_json::json!({
        "runtime_owner": "mirante4d-render-wgpu",
        "contract_owner": "mirante4d-render-api"
    });
    if ledger.get("render_authority") != Some(&expected_render_authority) {
        bail!("current render authority ledger drifted");
    }
    let temporary_owners = ledger
        .get("temporary_owners")
        .and_then(serde_json::Value::as_array)
        .context("current-state temporary_owners must be an array")?;
    if !temporary_owners.is_empty() {
        bail!("current-state ledger must not retain temporary runtime owners");
    }

    let app_root = repo_root.join("crates/mirante4d-app/src");
    let app_source = fs::read_to_string(app_root.join("lib.rs"))?;
    let app_fields = rust_struct_field_type_identifiers(&app_source, "MiranteWorkbenchApp")?;
    require_field_type(&app_fields, "application", "ApplicationState")?;
    require_field_type(&app_fields, "dataset", "DatasetDemandState")?;
    require_field_type(&app_fields, "egui_ui", "EguiUiState")?;
    require_field_type(&app_fields, "import", "ImportWorkflow")?;
    require_field_type(&app_fields, "analysis_runtime", "AnalysisProductRuntime")?;
    require_field_type(
        &app_fields,
        "product_automation",
        "ProductAutomationController",
    )?;
    require_field_type(
        &app_fields,
        "project_store",
        "ProjectStoreApplicationService",
    )?;
    for forbidden_field in [
        "dataset_runtime",
        "render_runtime",
        "ui_runtime",
        "import_runtime",
        "project_runtime",
        "project_persistence",
        "validation_runtime",
    ] {
        if app_fields.contains_key(forbidden_field) {
            bail!("retired composition field remains: {forbidden_field}");
        }
    }

    check_cutover_predecessor_absence(repo_root)?;
    check_dataset_ownership(repo_root, &app_root, &app_fields)?;
    check_target_dataset_source(repo_root, &app_root)?;
    check_render_ownership(repo_root, metadata)?;
    check_analysis_ownership(repo_root)?;
    check_project_store_ownership(repo_root, metadata, &app_fields)?;
    check_application_route_and_private_bridges(repo_root, &app_source)
}

fn require_field_type(
    fields: &BTreeMap<String, BTreeSet<String>>,
    field: &str,
    type_name: &str,
) -> anyhow::Result<()> {
    if fields
        .get(field)
        .is_some_and(|identifiers| identifiers.contains(type_name))
    {
        Ok(())
    } else {
        bail!("MiranteWorkbenchApp.{field} must contain {type_name}")
    }
}

fn check_cutover_predecessor_absence(repo_root: &Path) -> anyhow::Result<()> {
    for relative_path in [
        "crates/mirante4d-app/src/current_egui_shell_bridge.rs",
        "crates/mirante4d-app/src/current_project_persistence_bridge.rs",
        "crates/mirante4d-app/src/current_runtime/analysis.rs",
        "crates/mirante4d-app/src/current_runtime/mod.rs",
        "crates/mirante4d-app/src/current_runtime/dataset.rs",
        "crates/mirante4d-app/src/current_runtime/import.rs",
        "crates/mirante4d-app/src/current_runtime/project.rs",
        "crates/mirante4d-app/src/current_runtime/render.rs",
        "crates/mirante4d-app/src/current_runtime/ui.rs",
        "crates/mirante4d-app/src/current_runtime/validation.rs",
        "crates/mirante4d-app/src/display_identity.rs",
        "crates/mirante4d-app/src/resident_rendering.rs",
    ] {
        if repo_root.join(relative_path).exists() {
            bail!("retired cutover path still exists: {relative_path}");
        }
    }

    let forbidden_identifiers = [
        "CurrentAnalysisRuntime",
        "CurrentDatasetRuntime",
        "CurrentImportRuntime",
        "CurrentLeaseBridge",
        "CurrentProjectPersistenceBridge",
        "CurrentProjectRuntime",
        "CurrentRenderRuntime",
        "CurrentUiRuntime",
        "CurrentValidationRuntime",
        "GpuRenderer",
        "mirante4d_renderer",
    ];
    for source_path in collect_rust_source_files(&repo_root.join("crates"))? {
        if source_path.starts_with(repo_root.join("crates/xtask")) {
            continue;
        }
        let source = fs::read_to_string(&source_path)?;
        for identifier in forbidden_identifiers {
            if source_contains_identifier(&source, identifier) {
                bail!(
                    "retired cutover identifier {identifier} remains in {}",
                    source_path.display()
                );
            }
        }
    }
    Ok(())
}

fn check_dataset_ownership(
    repo_root: &Path,
    app_root: &Path,
    app_fields: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    require_field_type(app_fields, "dataset", "DatasetDemandState")?;
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
    {
        bail!("dataset runtime, dispatcher, and composition ownership chain drifted");
    }
    let demand_fields =
        rust_struct_field_type_identifiers(&dispatcher_source, "DatasetDemandState")?;
    if !demand_fields.contains_key("retained_leases") {
        bail!("DatasetDemandState must own retained dataset leases");
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

fn check_target_dataset_source(repo_root: &Path, app_root: &Path) -> anyhow::Result<()> {
    let source_path = repo_root.join("crates/mirante4d-storage/src/dataset_source.rs");
    let source = fs::read_to_string(&source_path)?;
    if !rust_root_defined_item_names(&source)?.contains("LocalDatasetSource")
        || !public_root_api_names(&repo_root.join("crates/mirante4d-storage/src/lib.rs"))?
            .contains("LocalDatasetSource")
    {
        bail!("the target dataset source is missing");
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
        let calls = source.matches("LocalDatasetSource::from_").count();
        if calls != 0 {
            source_open_routes.push(normalized);
            source_open_calls += calls;
        }
    }
    if source_open_calls != 2
        || source_open_routes != ["crates/mirante4d-app/src/unified_source_open.rs"]
    {
        bail!("target source must open through one unified route: {source_open_routes:?}");
    }
    Ok(())
}

fn check_render_ownership(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let retained_path = repo_root.join("crates/mirante4d-app/src/retained_leases.rs");
    let retained_source = fs::read_to_string(&retained_path)?;
    if !rust_root_defined_item_names(&retained_source)?.contains("RetainedLeases") {
        bail!("dataset-demand lease retention is missing");
    }

    let app_normal = normal_dependencies(metadata, "mirante4d-app")?;
    if !app_normal.contains("mirante4d-render-wgpu")
        || app_normal.contains("mirante4d-render-reference")
        || app_normal.contains("mirante4d-renderer")
    {
        bail!("native app must use only render-wgpu for product rendering");
    }

    let app_root = repo_root.join("crates/mirante4d-app/src");
    let successor_references = collect_rust_source_files(&app_root)?
        .into_iter()
        .map(fs::read_to_string)
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .map(|source| source.matches("WgpuRenderRuntime").count())
        .sum::<usize>();
    if successor_references == 0 {
        bail!("native app does not construct WgpuRenderRuntime");
    }
    Ok(())
}

fn check_analysis_ownership(repo_root: &Path) -> anyhow::Result<()> {
    let app_root = repo_root.join("crates/mirante4d-app/src");
    let mut owners = Vec::new();
    for source_path in collect_rust_source_files(&app_root)? {
        let source = fs::read_to_string(&source_path)?;
        if rust_root_defined_item_names(&source)?.contains("AnalysisProductRuntime") {
            owners.push(source_path);
        }
    }
    if owners.len() != 1 {
        bail!("expected exactly one AnalysisProductRuntime owner, found {owners:?}");
    }
    Ok(())
}

fn check_project_store_ownership(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
    app_fields: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    const PROJECT_STORE: &str = "mirante4d-project-store";
    let project_store_root = repo_root.join("crates/mirante4d-project-store");
    let library_root = project_store_root.join("src/lib.rs");
    let library_source = fs::read_to_string(&library_root)?;
    if !library_source.contains("#![forbid(unsafe_code)]") {
        bail!("project-store library root must forbid unsafe code");
    }
    if metadata.custom_build_package_ids.contains(
        metadata
            .workspace_package_ids_by_name
            .get(PROJECT_STORE)
            .context("project-store package has no workspace package ID")?,
    ) {
        bail!("project-store must not have a custom build target");
    }

    let allowed_consumers = BTreeSet::from([
        ("mirante4d-app", "normal"),
        ("mirante4d-application", "normal"),
    ]);
    for (source, kinds) in &metadata.declared_dependency_kinds_by_name {
        if source == PROJECT_STORE {
            continue;
        }
        for (kind, dependencies) in kinds {
            if dependencies.contains(PROJECT_STORE)
                && !allowed_consumers.contains(&(source.as_str(), kind.as_str()))
            {
                bail!("project-store has unauthorized consumer {source} ({kind})");
            }
        }
    }
    for manifest in [
        "crates/mirante4d-app/Cargo.toml",
        "crates/mirante4d-application/Cargo.toml",
    ] {
        validate_canonical_project_store_dependency(&repo_root.join(manifest))?;
    }

    let application_root = repo_root.join("crates/mirante4d-application/src");
    let service_path = application_root.join("project_store_service.rs");
    let application_source = fs::read_to_string(application_root.join("lib.rs"))?;
    if !service_path.is_file() || !application_source.contains("mod project_store_service;") {
        bail!("application project-store service declaration is missing");
    }
    let required_application_api = BTreeSet::from(
        [
            "MonotonicClock",
            "ProjectStoreApplicationService",
            "ProjectStoreLifecycle",
            "ProjectStoreServiceError",
            "ProjectStoreServiceEvent",
            "ProjectStoreServiceStatus",
            "SystemMonotonicClock",
        ]
        .map(str::to_owned),
    );
    let actual_application_api = public_root_api_names(&application_root.join("lib.rs"))?;
    if !required_application_api.is_subset(&actual_application_api) {
        bail!(
            "application project-store service API is incomplete: {:?}",
            required_application_api
                .difference(&actual_application_api)
                .collect::<Vec<_>>()
        );
    }
    validate_service_impl_targets(&fs::read_to_string(&service_path)?)?;

    require_field_type(
        app_fields,
        "project_store",
        "ProjectStoreApplicationService",
    )?;
    let app_root = repo_root.join("crates/mirante4d-app/src");
    let app_source = fs::read_to_string(app_root.join("lib.rs"))?;
    for marker in [
        "ProjectStoreApplicationService::start",
        "start_project_store_service",
        "self.poll_project_store()",
        "fn handle_project_store_event",
        "ProjectStoreServiceEvent::",
    ] {
        if !app_source.contains(marker) {
            bail!("project-store product route is missing {marker:?}");
        }
    }
    let ui_source = fs::read_to_string(app_root.join("workbench_ui.rs"))?;
    for marker in [
        "ProjectStoreApplicationService::has_pending_work",
        "project_store.close()",
        "project_store.join()",
    ] {
        if !ui_source.contains(marker) {
            bail!("project-store UI lifecycle is missing {marker:?}");
        }
    }

    for source_root in [&app_root, &application_root] {
        for source_path in collect_rust_source_files(source_root)? {
            let source = fs::read_to_string(&source_path)?;
            for forbidden in [
                "current_project_persistence_bridge",
                "CurrentProjectPersistenceBridge",
                "CurrentProjectRuntime",
                "current_project_path",
                "PROJECT_V15_SCHEMA",
                "PROJECT_V15_SCHEMA_VERSION",
                "ProjectDocumentDto",
            ] {
                if source_contains_identifier(&source, forbidden) {
                    bail!(
                        "retired project-store identifier {forbidden} remains in {}",
                        source_path.display()
                    );
                }
            }
            if source.contains("mirante4d-project-v15") {
                bail!(
                    "retired project schema remains in {}",
                    source_path.display()
                );
            }
        }
    }

    let app_normal = normal_dependencies(metadata, "mirante4d-app")?;
    if app_normal.contains("mirante4d-identity") || !app_normal.contains(PROJECT_STORE) {
        bail!("native app project-store dependency route drifted");
    }
    Ok(())
}

fn check_application_route_and_private_bridges(
    repo_root: &Path,
    app_source: &str,
) -> anyhow::Result<()> {
    let app_source_root = repo_root.join("crates/mirante4d-app/src");
    let bridge_paths = collect_rust_source_files(&app_source_root)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("_bridge.rs"))
        })
        .collect::<Vec<_>>();
    if !bridge_paths.is_empty() {
        bail!("native app must not retain private bridge files: {bridge_paths:?}");
    }

    let private_bridge_modules = private_root_module_names(app_source)?
        .into_iter()
        .filter(|name| name.ends_with("_bridge"))
        .collect::<BTreeSet<_>>();
    if !private_bridge_modules.is_empty() {
        bail!("native app must not retain private bridge modules: {private_bridge_modules:?}");
    }
    if !app_source.contains("self.application.dispatch(")
        || !app_source.contains("self.application.snapshot()")
        || !app_source.contains("self.application.drain_events(")
        || !app_source.contains("fn apply_application_command")
    {
        bail!("native UI shell must route directly through ApplicationState");
    }
    Ok(())
}

fn validate_canonical_project_store_dependency(manifest_path: &Path) -> anyhow::Result<()> {
    const PROJECT_STORE: &str = "mirante4d-project-store";
    let source = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = source
        .parse::<toml::Table>()
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .with_context(|| format!("{} has no dependency table", manifest_path.display()))?;
    if !dependencies.contains_key(PROJECT_STORE) {
        bail!(
            "product consumer {} must use canonical {PROJECT_STORE} dependency name",
            manifest_path.display()
        );
    }
    if manifest_contains_project_store_rename(&toml::Value::Table(manifest)) {
        bail!(
            "product consumer {} must not rename {PROJECT_STORE}",
            manifest_path.display()
        );
    }
    Ok(())
}

fn manifest_contains_project_store_rename(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => {
            table.get("package").and_then(toml::Value::as_str) == Some("mirante4d-project-store")
                || table.values().any(manifest_contains_project_store_rename)
        }
        toml::Value::Array(values) => values.iter().any(manifest_contains_project_store_rename),
        _ => false,
    }
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
                "non-workspace target {} depends on retired packages: {forbidden:?}",
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
            bail!("local Cargo override {relative_path} must contain only package {package}");
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
                        "workspace package {name} declares a non-workspace local path dependency {dependency_name} at {path}"
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
            if kind
                .as_str()
                .context("cargo metadata target kind must be a string")?
                == "custom-build"
            {
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
        match item {
            syn::Item::Const(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Enum(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Fn(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.sig.ident.to_string());
            }
            syn::Item::Mod(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Static(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Struct(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Trait(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::TraitAlias(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Type(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Union(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                names.insert(item.ident.to_string());
            }
            syn::Item::Use(item) if matches!(item.vis, syn::Visibility::Public(_)) => {
                add_public_use_tree_names(&item.tree, &mut names);
            }
            _ => {}
        }
    }
    Ok(names)
}

fn add_public_use_tree_names(tree: &syn::UseTree, names: &mut BTreeSet<String>) {
    match tree {
        syn::UseTree::Name(name) => {
            names.insert(name.ident.to_string());
        }
        syn::UseTree::Rename(rename) => {
            names.insert(rename.rename.to_string());
        }
        syn::UseTree::Path(path) => add_public_use_tree_names(&path.tree, names),
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                add_public_use_tree_names(tree, names);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn rust_struct_field_type_identifiers(
    source: &str,
    struct_name: &str,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let file =
        syn::parse_file(source).context("failed to parse Rust source for field inventory")?;
    let item = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == struct_name => Some(item),
            _ => None,
        })
        .with_context(|| format!("expected exactly one top-level struct {struct_name}"))?;
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
            let mut identifiers = BTreeSet::new();
            collect_type_identifiers(&field.ty, &mut identifiers);
            Ok((name, identifiers))
        })
        .collect()
}

fn collect_type_identifiers(ty: &syn::Type, identifiers: &mut BTreeSet<String>) {
    match ty {
        syn::Type::Group(group) => collect_type_identifiers(&group.elem, identifiers),
        syn::Type::Paren(paren) => collect_type_identifiers(&paren.elem, identifiers),
        syn::Type::Reference(reference) => collect_type_identifiers(&reference.elem, identifiers),
        syn::Type::Path(path) => {
            for segment in &path.path.segments {
                identifiers.insert(segment.ident.to_string());
                if let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments {
                    for argument in &arguments.args {
                        if let syn::GenericArgument::Type(ty) = argument {
                            collect_type_identifiers(ty, identifiers);
                        }
                    }
                }
            }
        }
        _ => {}
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

fn rust_root_defined_item_names(source: &str) -> anyhow::Result<BTreeSet<String>> {
    let file = syn::parse_file(source).context("failed to parse Rust source for item inventory")?;
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

fn validate_service_impl_targets(source: &str) -> anyhow::Result<()> {
    let file =
        syn::parse_file(source).context("failed to parse project-store application service")?;
    let local_types = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Enum(item) => Some(item.ident.to_string()),
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Union(item) => Some(item.ident.to_string()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for item in &file.items {
        let syn::Item::Impl(item) = item else {
            continue;
        };
        let syn::Type::Path(target) = item.self_ty.as_ref() else {
            bail!("project-store service may implement only its own local types");
        };
        let target = target
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .context("project-store service impl target has no type name")?;
        if !local_types.contains(&target) {
            bail!("project-store service must not extend product-owned type {target}");
        }
    }
    Ok(())
}

fn axis_aligned_2d_chunk_dependency_violations(path: &Path, source: &str) -> Vec<String> {
    source_pattern_violations(
        path,
        source,
        FORBIDDEN_AXIS_ALIGNED_2D_CHUNK_PATTERNS,
        "source must not depend on axis-aligned 2D slice chunk layouts",
    )
}

fn dumping_ground_module_name_violation(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    FORBIDDEN_DUMPING_GROUND_MODULE_NAMES
        .contains(&file_name)
        .then(|| {
            format!(
                "{} has a forbidden dumping-ground module name",
                path.display()
            )
        })
}

fn source_pattern_violations(
    path: &Path,
    source: &str,
    forbidden_patterns: &[&str],
    policy: &str,
) -> Vec<String> {
    forbidden_patterns
        .iter()
        .filter(|pattern| source.contains(**pattern))
        .map(|pattern| format!("{}: {policy}: {pattern:?}", path.display()))
        .collect()
}

fn collect_rust_source_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rust_source_files_inner(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_rust_source_files_inner(root: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)
        .with_context(|| format!("failed to read directory {}", root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
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
    fn source_policy_keeps_ui_imports_in_the_ui_layer() {
        let outside = source_architecture_violations(
            Path::new("crates/mirante4d-storage/src/lib.rs"),
            "use egui::Context;\n",
        );
        assert_eq!(outside.len(), 1);
        assert!(outside[0].contains("must not import the UI or native app layer"));

        let inside = source_architecture_violations(
            Path::new("crates/mirante4d-ui-egui/src/lib.rs"),
            "use egui::Context;\n",
        );
        assert!(inside.is_empty());
    }

    #[test]
    fn source_policy_rejects_renderer_file_io() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-render-wgpu/src/lib.rs"),
            "use std::fs;\nlet _ = File::open(path);\n",
        );
        assert_eq!(violations.len(), 2);
        assert!(
            violations
                .iter()
                .all(|violation| violation.contains("must not perform direct filesystem I/O"))
        );
    }

    #[test]
    fn source_policy_rejects_dumping_ground_and_slice_chunk_names() {
        let violations = source_architecture_violations(
            Path::new("crates/mirante4d-app/src/utils.rs"),
            "const SHAPE: (u32, u32, u32) = (512, 512, 1);\nstruct SliceChunk;\n",
        );
        assert_eq!(violations.len(), 3);
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("dumping-ground module name"))
        );
        assert_eq!(
            violations
                .iter()
                .filter(|violation| violation.contains("axis-aligned 2D slice chunk layouts"))
                .count(),
            2
        );
    }

    #[test]
    fn canonical_model_std_scan_rejects_direct_and_aliased_authority() {
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
    }

    #[test]
    fn field_inventory_collects_nested_type_identifiers() {
        let source = r#"
struct Example {
    project_store: Option<ProjectStoreApplicationService<SystemMonotonicClock>>,
    plain: u64,
}
"#;
        let fields = rust_struct_field_type_identifiers(source, "Example").unwrap();
        assert_eq!(
            fields["project_store"],
            BTreeSet::from(
                [
                    "Option",
                    "ProjectStoreApplicationService",
                    "SystemMonotonicClock",
                ]
                .map(str::to_owned)
            )
        );
        assert_eq!(fields["plain"], BTreeSet::from(["u64".to_owned()]));
    }

    #[test]
    fn dependency_layers_point_toward_lower_foundations() {
        assert!(package_layer("mirante4d-domain") < package_layer("mirante4d-dataset"));
        assert!(package_layer("mirante4d-storage") < package_layer("mirante4d-app"));
        assert!(package_layer("mirante4d-ui-egui") < package_layer("mirante4d-app"));
    }

    #[test]
    fn cargo_override_checks_reject_hidden_replacements() {
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
            .is_err()
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
    }

    #[test]
    fn manifest_dependency_inventory_resolves_package_aliases() {
        let manifest = r#"
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
            manifest_dependency_package_names(&manifest).unwrap(),
            BTreeSet::from([
                "mirante4d-application".to_owned(),
                "mirante4d-settings".to_owned(),
            ])
        );
    }

    #[test]
    fn tracked_artifact_policy_rejects_generated_paths_and_large_data() {
        assert!(
            tracked_artifact_policy_violation(Path::new("target/mirante4d/out.bin"), 1).is_some()
        );
        assert!(
            tracked_artifact_policy_violation(
                Path::new("fixtures/large-source.ome.tiff"),
                MAX_TRACKED_GENERATED_ARTIFACT_BYTES + 1,
            )
            .is_some()
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
