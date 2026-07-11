use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use serde::Deserialize;
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

pub(crate) fn architecture_self_check() -> anyhow::Result<()> {
    let required = [
        "crates/mirante4d-analysis",
        "crates/mirante4d-core",
        "crates/mirante4d-format",
        "crates/mirante4d-import",
        "crates/mirante4d-data",
        "crates/mirante4d-domain",
        "crates/mirante4d-identity",
        "crates/mirante4d-project-model",
        "crates/mirante4d-renderer",
        "crates/mirante4d-app",
        "crates/xtask",
    ];
    for path in required {
        if !Path::new(path).is_dir() {
            bail!("required crate directory is missing: {path}");
        }
    }
    for forbidden in ["crates/mirante4d-preprocess"] {
        if Path::new(forbidden).exists() {
            bail!("first milestone must not create empty future crate: {forbidden}");
        }
    }
    check_crate_dependency_policy()?;
    check_source_architecture_policy()?;
    check_wp07a_contracts(Path::new("."))?;
    check_tracked_artifact_policy()?;
    Ok(())
}

fn check_crate_dependency_policy() -> anyhow::Result<()> {
    let policies = [
        ("mirante4d-domain", &[][..]),
        ("mirante4d-identity", &[][..]),
        (
            "mirante4d-project-model",
            &["mirante4d-domain", "mirante4d-identity"][..],
        ),
        ("mirante4d-core", &[][..]),
        ("mirante4d-format", &["mirante4d-core"][..]),
        (
            "mirante4d-data",
            &["mirante4d-core", "mirante4d-format"][..],
        ),
        (
            "mirante4d-renderer",
            &["mirante4d-core", "mirante4d-data"][..],
        ),
        (
            "mirante4d-import",
            &["mirante4d-core", "mirante4d-format"][..],
        ),
        (
            "mirante4d-analysis",
            &["mirante4d-core", "mirante4d-data"][..],
        ),
        (
            "mirante4d-app",
            &[
                "mirante4d-analysis",
                "mirante4d-core",
                "mirante4d-data",
                "mirante4d-format",
                "mirante4d-import",
                "mirante4d-renderer",
            ][..],
        ),
        (
            "xtask",
            &[
                "mirante4d-analysis",
                "mirante4d-core",
                "mirante4d-data",
                "mirante4d-format",
                "mirante4d-import",
                "mirante4d-renderer",
            ][..],
        ),
    ];

    for (crate_name, allowed) in policies {
        let manifest_path = Path::new("crates").join(crate_name).join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let dependencies = normal_workspace_dependencies(&manifest);
        for dependency in dependencies {
            if !allowed.contains(&dependency.as_str()) {
                bail!(
                    "crate {crate_name} has forbidden normal dependency {dependency}; allowed Mirante4D dependencies are: {}",
                    if allowed.is_empty() {
                        "<none>".to_owned()
                    } else {
                        allowed.join(", ")
                    }
                );
            }
        }
    }
    Ok(())
}

fn normal_workspace_dependencies(manifest: &str) -> Vec<String> {
    let mut in_dependencies = false;
    let mut dependencies = Vec::new();
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_dependencies = trimmed == "[dependencies]";
            continue;
        }
        if !in_dependencies || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, _rest)) = trimmed.split_once('=') else {
            continue;
        };
        let name = name
            .trim()
            .split_once('.')
            .map(|(crate_name, _field)| crate_name)
            .unwrap_or_else(|| name.trim());
        if name.starts_with("mirante4d-") {
            dependencies.push(name.to_owned());
        }
    }
    dependencies
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
                "mirante4d_core",
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
    let Ok(file) = syn::parse_file(source) else {
        return vec![format!(
            "{} cannot be parsed for WP-07A side-effect ownership",
            path.display()
        )];
    };
    let forbidden = BTreeSet::from([
        "env", "fs", "io", "net", "os", "path", "process", "sync", "thread", "time",
    ]);
    let mut visitor = ForbiddenStdUseVisitor {
        source_path: path,
        forbidden: &forbidden,
        violations: Vec::new(),
    };
    visitor.visit_file(&file);
    visitor.violations
}

struct ForbiddenStdUseVisitor<'a> {
    source_path: &'a Path,
    forbidden: &'a BTreeSet<&'static str>,
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for ForbiddenStdUseVisitor<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for segments in paths {
            if segments.first().is_some_and(|segment| segment == "std")
                && segments
                    .get(1)
                    .is_some_and(|segment| self.forbidden.contains(segment.as_str()))
            {
                self.violations.push(format!(
                    "{}: WP-07A canonical-model crate imports forbidden std authority {}",
                    self.source_path.display(),
                    segments.join("::")
                ));
            }
        }
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

#[derive(Debug, Deserialize)]
struct StateFieldLedger {
    schema: String,
    schema_version: u64,
    source_revision: String,
    sources: Vec<StateFieldSource>,
    invariants: StateLedgerInvariants,
    dispositions: Vec<StateFieldDisposition>,
    nested_aggregate_deletions: Vec<serde_json::Value>,
    project_v14_source: StateFieldSource,
    project_v14_dto_dispositions: Vec<ProjectDtoDisposition>,
}

#[derive(Debug, Deserialize)]
struct StateLedgerInvariants {
    every_source_field_exactly_once: bool,
    one_target_owner_or_deletion: bool,
    product_cutover_gate: String,
    no_live_target_model_in_wp07a: bool,
    appstate_predecessor_field_removal_gate: String,
    finalization_gate_meaning: String,
}

#[derive(Debug, Deserialize)]
struct StateFieldSource {
    path: String,
    #[serde(rename = "struct")]
    struct_name: String,
    expected_fields: usize,
}

#[derive(Debug, Deserialize)]
struct StateFieldDisposition {
    id: String,
    source_struct: String,
    fields: Vec<String>,
    action: String,
    target_owner: String,
    state_class: String,
    finalization_gate: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct ProjectDtoDisposition {
    field: String,
    action: String,
    target_owner: String,
    reason: String,
}

fn check_wp07a_contracts(repo_root: &Path) -> anyhow::Result<()> {
    let (predecessor, state_class_mapping) = check_wp07a_model_contract(repo_root)?;
    check_current_state_field_ledger(repo_root, &predecessor, &state_class_mapping)
}

fn check_wp07a_model_contract(
    repo_root: &Path,
) -> anyhow::Result<(String, BTreeMap<String, BTreeSet<String>>)> {
    let path = repo_root.join("architecture/model-contract.json");
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
        if actual_public_api != public_api_set {
            let missing = public_api_set
                .difference(&actual_public_api)
                .cloned()
                .collect::<Vec<_>>();
            let unexpected = actual_public_api
                .difference(&public_api_set)
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
    let preparatory_crates = expected.keys().copied().collect::<BTreeSet<_>>();
    for (package, kinds) in &workspace_metadata.declared_dependency_kinds_by_name {
        if preparatory_crates.contains(package.as_str()) {
            continue;
        }
        let forbidden = kinds
            .values()
            .flatten()
            .filter(|dependency| preparatory_crates.contains(dependency.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !forbidden.is_empty() {
            bail!(
                "existing workspace package {package} reaches WP-07A preparatory crates through some declared dependency kind or target: {forbidden:?}"
            );
        }
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

fn check_current_state_field_ledger(
    repo_root: &Path,
    expected_revision: &str,
    state_class_mapping: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    let path = repo_root.join("architecture/current-state-field-ledger.json");
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let ledger: StateFieldLedger = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if ledger.schema != "mirante4d-current-state-field-ledger" || ledger.schema_version != 1 {
        bail!("{} has an unsupported schema", path.display());
    }
    if ledger.source_revision != expected_revision {
        bail!("state-field ledger and model contract must name the same predecessor revision");
    }
    if !ledger.invariants.every_source_field_exactly_once
        || !ledger.invariants.one_target_owner_or_deletion
        || ledger.invariants.product_cutover_gate != "WP-07B"
        || !ledger.invariants.no_live_target_model_in_wp07a
        || ledger.invariants.appstate_predecessor_field_removal_gate != "WP-07B"
        || ledger
            .invariants
            .finalization_gate_meaning
            .trim()
            .is_empty()
    {
        bail!("state-field ledger invariants drifted");
    }

    let expected_sources = BTreeMap::from([
        ("AppState", ("crates/mirante4d-app/src/lib.rs", 120_usize)),
        (
            "MiranteWorkbenchApp",
            ("crates/mirante4d-app/src/lib.rs", 32_usize),
        ),
    ]);
    if ledger.sources.len() != expected_sources.len() {
        bail!("state-field ledger must name exactly the two frozen application structs");
    }
    let mut source_fields = BTreeMap::<String, BTreeSet<String>>::new();
    for source_contract in &ledger.sources {
        let expected = expected_sources
            .get(source_contract.struct_name.as_str())
            .with_context(|| {
                format!(
                    "unknown state-field source struct {}",
                    source_contract.struct_name
                )
            })?;
        if source_contract.path != expected.0 || source_contract.expected_fields != expected.1 {
            bail!(
                "state-field source contract drifted for {}",
                source_contract.struct_name
            );
        }
        let source_path = repo_root.join(&source_contract.path);
        let source = fs::read_to_string(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        let fields = rust_struct_field_names(&source, &source_contract.struct_name)?;
        if fields.len() != source_contract.expected_fields {
            bail!(
                "{} has {} fields, but the ledger expects {}",
                source_contract.struct_name,
                fields.len(),
                source_contract.expected_fields
            );
        }
        let field_set = fields.into_iter().collect::<BTreeSet<_>>();
        if source_fields
            .insert(source_contract.struct_name.clone(), field_set)
            .is_some()
        {
            bail!(
                "duplicate state-field source {}",
                source_contract.struct_name
            );
        }
    }
    if source_fields.values().map(BTreeSet::len).sum::<usize>() != 152 {
        bail!("state-field ledger source inventory must contain exactly 152 fields");
    }

    let mut dispositions = BTreeMap::<String, BTreeSet<String>>::new();
    let mut disposition_ids = BTreeSet::new();
    let allowed_gates = BTreeSet::from([
        "WP-07B", "WP-08B", "WP-09B", "WP-09C", "WP-10B", "WP-10C", "WP-12", "WP-14",
    ]);
    let allowed_owners = BTreeSet::from([
        "mirante4d-analysis-core",
        "mirante4d-analysis-runtime",
        "mirante4d-app",
        "mirante4d-application",
        "mirante4d-dataset",
        "mirante4d-dataset-runtime",
        "mirante4d-import-pipeline",
        "mirante4d-project-model",
        "mirante4d-project-store",
        "mirante4d-render-api",
        "mirante4d-render-wgpu",
        "mirante4d-settings",
        "mirante4d-ui-egui",
        "test-harness",
        "validation-harness",
    ]);
    for disposition in &ledger.dispositions {
        if !disposition_ids.insert(&disposition.id) {
            bail!("duplicate state-field disposition ID {}", disposition.id);
        }
        if !matches!(disposition.action.as_str(), "move" | "delete" | "split")
            || disposition.target_owner.trim().is_empty()
            || !disposition
                .target_owner
                .split('+')
                .all(|owner| allowed_owners.contains(owner))
            || !allowed_gates.contains(disposition.finalization_gate.as_str())
            || !state_class_mapping.contains_key(&disposition.state_class)
            || disposition.reason.trim().is_empty()
            || disposition.fields.is_empty()
        {
            bail!("incomplete state-field disposition {}", disposition.id);
        }
        let target = dispositions
            .entry(disposition.source_struct.clone())
            .or_default();
        for field in &disposition.fields {
            if !target.insert(field.clone()) {
                bail!(
                    "{}.{} has more than one disposition",
                    disposition.source_struct,
                    field
                );
            }
        }
    }
    if dispositions != source_fields {
        bail!("state-field ledger does not cover every current source field exactly once");
    }
    validate_nested_aggregate_dispositions(&ledger.nested_aggregate_deletions)?;
    validate_project_v14_dispositions(repo_root, &ledger)?;
    Ok(())
}

fn rust_struct_field_names(source: &str, struct_name: &str) -> anyhow::Result<Vec<String>> {
    let file = syn::parse_file(source).context("failed to parse Rust source for field ledger")?;
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
        bail!("state-ledger struct {struct_name} must have named fields");
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

fn validate_nested_aggregate_dispositions(items: &[serde_json::Value]) -> anyhow::Result<()> {
    let expected = BTreeSet::from([
        "AppLayerSummary",
        "SceneArtifactStore",
        "ViewerLayoutState",
        "ViewerToolState",
    ]);
    let mut observed = BTreeSet::new();
    for item in items {
        let aggregate = item
            .get("type")
            .and_then(serde_json::Value::as_str)
            .context("nested aggregate disposition must name its type")?;
        if !observed.insert(aggregate) {
            bail!("duplicate nested aggregate disposition {aggregate}");
        }
        let split = item
            .get("split")
            .and_then(serde_json::Value::as_object)
            .context("nested aggregate disposition must have a split object")?;
        let mut concepts = BTreeSet::new();
        for fields in split.values() {
            let fields = fields
                .as_array()
                .context("nested aggregate split fields must be an array")?;
            if fields.is_empty() {
                bail!("nested aggregate {aggregate} has an empty split");
            }
            for field in fields {
                let field = field
                    .as_str()
                    .context("nested aggregate split field must be a string")?;
                if !concepts.insert(field) {
                    bail!("nested aggregate {aggregate} assigns {field:?} more than once");
                }
            }
        }
        if let Some(deletions) = item.get("delete_duplicates") {
            for field in deletions
                .as_array()
                .context("nested aggregate delete_duplicates must be an array")?
            {
                let field = field
                    .as_str()
                    .context("nested aggregate deletion must be a string")?;
                if !concepts.insert(field) {
                    bail!("nested aggregate {aggregate} both moves and deletes {field:?}");
                }
            }
        }
    }
    if observed != expected {
        bail!("nested aggregate disposition set drifted");
    }
    Ok(())
}

fn validate_project_v14_dispositions(
    repo_root: &Path,
    ledger: &StateFieldLedger,
) -> anyhow::Result<()> {
    let source = &ledger.project_v14_source;
    if source.path != "crates/mirante4d-app/src/project_session.rs"
        || source.struct_name != "AppSession"
        || source.expected_fields != 13
    {
        bail!("project-v14 source contract drifted");
    }
    let source_text = fs::read_to_string(repo_root.join(&source.path))?;
    let actual = rust_struct_field_names(&source_text, &source.struct_name)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if actual.len() != source.expected_fields {
        bail!("project-v14 source field count drifted");
    }
    let mut disposed = BTreeSet::new();
    for disposition in &ledger.project_v14_dto_dispositions {
        if !disposed.insert(disposition.field.clone())
            || !matches!(disposition.action.as_str(), "move" | "delete" | "replace")
            || disposition.target_owner.trim().is_empty()
            || disposition.reason.trim().is_empty()
        {
            bail!("project-v14 DTO disposition is incomplete or duplicate");
        }
    }
    if actual != disposed {
        bail!("project-v14 DTO dispositions do not cover every AppSession field exactly once");
    }
    Ok(())
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
    fn normal_workspace_dependencies_reads_only_normal_dependency_section() {
        let manifest = r#"
[package]
name = "example"

[dependencies]
mirante4d-core.workspace = true
serde.workspace = true
mirante4d-data = { workspace = true }

[dev-dependencies]
mirante4d-format.workspace = true
"#;

        assert_eq!(
            normal_workspace_dependencies(manifest),
            vec!["mirante4d-core".to_owned(), "mirante4d-data".to_owned()]
        );
    }

    #[test]
    fn normal_workspace_dependencies_ignores_comments_and_other_sections() {
        let manifest = r#"
[dependencies]
# mirante4d-app.workspace = true

[build-dependencies]
mirante4d-renderer.workspace = true
"#;

        assert!(normal_workspace_dependencies(manifest).is_empty());
    }

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
    fn wp07a_model_and_current_state_contracts_match_the_repository() {
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
