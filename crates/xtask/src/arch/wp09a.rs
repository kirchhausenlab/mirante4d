use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, bail};
use serde::Deserialize;

use super::{
    public_root_api_names, workspace_dependency_metadata,
    wp08a::{forbidden_import_violations, public_api_violations},
};

const CONTRACT_PATH: &str = "architecture/wp09a-render-contract.json";
const ENTRY_PATH: &str = "architecture/wp09a-progressive-render-entry.json";
const ENTRY_SHA256: &str = "db991cbb9c96bb4e5e8a791aafff0495ba762a43862c78db136865fa495c53af";
const PREDECESSOR_PATH: &str = "architecture/wp08a-subsystem-contract.json";
const PREDECESSOR_SHA256: &str = "0500c27c9c4e13ce2eb0d833534a865c01efb0eabfea34df30015cf9af416cd3";
const WGPU_CRATE: &str = "mirante4d-render-wgpu";
const REFERENCE_CRATE: &str = "mirante4d-render-reference";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenderContract {
    schema: String,
    schema_version: u64,
    status: String,
    entry: FileBinding,
    predecessor: FileBinding,
    activation: Activation,
    crates: Vec<CrateContract>,
    public_api_forbidden_identifiers: Vec<String>,
    source_policy: SourcePolicy,
    frame_budget: FrameBudget,
    gpu_budget: GpuBudget,
    verification: Verification,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBinding {
    path: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    product_reachability: bool,
    product_authority_flip: bool,
    product_activation_gate: String,
    reachable_product_renderer: String,
    successor_gpu_owner: String,
    cpu_oracle: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrateContract {
    name: String,
    path: String,
    publish: bool,
    dependencies: Dependencies,
    public_api: Vec<String>,
    forbidden_import_roots: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependencies {
    normal_workspace: Vec<String>,
    dev_workspace: Vec<String>,
    normal_external: Vec<String>,
    dev_external: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePolicy {
    unsafe_allowed: bool,
    product_dependency_allowlist: Vec<String>,
    reference_dependent_allowlist: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameBudget {
    presentation_targets_max: u64,
    extent_pixels_max: [u64; 2],
    requirement_metadata_records_max: u64,
    resident_resource_records_max: u64,
    supplied_resource_leases_max: u64,
    resident_resources_visited_max: u64,
    shader_resources_max: u64,
    semantic_scales_per_layer_max: u64,
    new_resources_uploaded_max: u64,
    payload_upload_bytes_max: u64,
    control_upload_bytes_max: u64,
    command_buffers_max: u64,
    queue_submissions_max: u64,
    synchronous_waits_or_readbacks: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GpuBudget {
    unknown_capacity_bytes: u64,
    payload_percent_max: u64,
    transfer_percent_max: u64,
    display_page_table_scratch_percent_min: u64,
    accounting_tolerance_percent: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    trusted_lane: String,
    predecessor_selector_adapter: String,
    predecessor_ignored_cases: u64,
    successor_selector_adapter: String,
    successor_ignored_cases: u64,
    successor_test_package: String,
    successor_test_prefix: String,
    evidence_line_prefix: String,
    evidence_schema: String,
    evidence_schema_version: u64,
}

pub(super) fn check_wp09a_render_contract(repo_root: &Path) -> anyhow::Result<()> {
    let contract_path = repo_root.join(CONTRACT_PATH);
    let contract: RenderContract = serde_json::from_slice(
        &fs::read(&contract_path)
            .with_context(|| format!("failed to read {}", contract_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", contract_path.display()))?;

    validate_header(repo_root, &contract)?;
    validate_fixed_policy(&contract)?;
    validate_verification_registry(repo_root, &contract.verification)?;
    validate_crates(repo_root, &contract)?;
    validate_reachability(repo_root, &contract)
}

fn validate_verification_registry(
    repo_root: &Path,
    verification: &Verification,
) -> anyhow::Result<()> {
    let registry: serde_json::Value =
        serde_json::from_slice(&fs::read(repo_root.join("verification/registry.json"))?)?;
    let adapters = registry
        .get("selector_adapters")
        .and_then(serde_json::Value::as_array)
        .context("verification registry has no selector adapters")?;
    for (id, package, prefix, expected) in [
        (
            verification.predecessor_selector_adapter.as_str(),
            "mirante4d-renderer",
            "gpu::",
            verification.predecessor_ignored_cases,
        ),
        (
            verification.successor_selector_adapter.as_str(),
            verification.successor_test_package.as_str(),
            verification.successor_test_prefix.as_str(),
            verification.successor_ignored_cases,
        ),
    ] {
        let matches = adapters
            .iter()
            .filter(|adapter| adapter.get("id").and_then(serde_json::Value::as_str) == Some(id))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!("WP-09A requires exactly one selector adapter {id}");
        }
        let adapter = matches[0];
        let selectors = adapter
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .context("WP-09A selector adapter has no matches")?;
        let selector_is_exact = selectors.len() == 1
            && selectors[0]
                .get("package")
                .and_then(serde_json::Value::as_str)
                == Some(package)
            && selectors[0]
                .get("test_prefix")
                .and_then(serde_json::Value::as_str)
                == Some(prefix);
        if adapter.get("lane").and_then(serde_json::Value::as_str) != Some("trusted-gpu")
            || adapter
                .get("expected_ignored_cases")
                .and_then(serde_json::Value::as_u64)
                != Some(expected)
            || !selector_is_exact
        {
            bail!("WP-09A selector adapter {id} drifted");
        }
    }
    Ok(())
}

fn validate_header(repo_root: &Path, contract: &RenderContract) -> anyhow::Result<()> {
    if contract.schema != "mirante4d-wp09a-render-successor-contract"
        || contract.schema_version != 1
        || contract.status != "accepted-off-product"
        || contract.entry.path != ENTRY_PATH
        || contract.entry.sha256 != ENTRY_SHA256
        || contract.predecessor.path != PREDECESSOR_PATH
        || contract.predecessor.sha256 != PREDECESSOR_SHA256
    {
        bail!("{CONTRACT_PATH} does not bind the accepted WP-09A entry and WP-08A predecessor");
    }
    for binding in [&contract.entry, &contract.predecessor] {
        let digest = super::sha256_file(&repo_root.join(&binding.path))?;
        if digest != binding.sha256 {
            bail!("WP-09A bound file {} changed", binding.path);
        }
    }
    Ok(())
}

fn validate_fixed_policy(contract: &RenderContract) -> anyhow::Result<()> {
    let activation = &contract.activation;
    if activation.product_reachability
        || activation.product_authority_flip
        || activation.product_activation_gate != "WP-09B"
        || activation.reachable_product_renderer != "mirante4d-renderer"
        || activation.successor_gpu_owner != WGPU_CRATE
        || activation.cpu_oracle != REFERENCE_CRATE
    {
        bail!("WP-09A successors must remain off-product until WP-09B");
    }
    let frame = &contract.frame_budget;
    if frame.presentation_targets_max != 1
        || frame.extent_pixels_max != [1920, 1080]
        || frame.requirement_metadata_records_max != 256
        || frame.resident_resource_records_max != 256
        || frame.supplied_resource_leases_max != 128
        || frame.resident_resources_visited_max != 128
        || frame.shader_resources_max != 128
        || frame.semantic_scales_per_layer_max != 1
        || frame.new_resources_uploaded_max != 8
        || frame.payload_upload_bytes_max != 8 * 1024 * 1024
        || frame.control_upload_bytes_max != 64 * 1024
        || frame.command_buffers_max != 1
        || frame.queue_submissions_max != 1
        || frame.synchronous_waits_or_readbacks != 0
    {
        bail!("WP-09A frame budgets drifted from the accepted entry");
    }
    let gpu = &contract.gpu_budget;
    if gpu.unknown_capacity_bytes != 1024 * 1024 * 1024
        || gpu.payload_percent_max != 75
        || gpu.transfer_percent_max != 10
        || gpu.display_page_table_scratch_percent_min != 15
        || gpu.accounting_tolerance_percent != 10
    {
        bail!("WP-09A GPU budgets drifted from the accepted entry");
    }
    let source = &contract.source_policy;
    if source.unsafe_allowed
        || !source.product_dependency_allowlist.is_empty()
        || exact_set(&source.reference_dependent_allowlist)
            != BTreeSet::from([format!("{WGPU_CRATE}:dev")])
    {
        bail!("WP-09A source/reachability policy drifted");
    }
    let verification = &contract.verification;
    if verification.trusted_lane != "trusted-gpu"
        || verification.predecessor_selector_adapter != "WP06-ADAPTER-TRUSTED-SPECIAL-SELECTOR"
        || verification.predecessor_ignored_cases != 8
        || verification.successor_selector_adapter != "WP09A-ADAPTER-TRUSTED-GPU"
        || verification.successor_ignored_cases != 1
        || verification.successor_test_package != WGPU_CRATE
        || verification.successor_test_prefix != "gpu_tests::"
        || verification.evidence_line_prefix != "wp09a-evidence-json:"
        || verification.evidence_schema != "mirante4d-wp09a-trusted-gpu-evidence"
        || verification.evidence_schema_version != 1
    {
        bail!("WP-09A verification ownership drifted");
    }
    Ok(())
}

fn validate_crates(repo_root: &Path, contract: &RenderContract) -> anyhow::Result<()> {
    let contracts = contract
        .crates
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect::<BTreeMap<_, _>>();
    if contracts.len() != 2
        || !contracts.contains_key(WGPU_CRATE)
        || !contracts.contains_key(REFERENCE_CRATE)
    {
        bail!("WP-09A contract must own exactly the WGPU and reference crates");
    }
    let expected_public = BTreeMap::from([
        (
            WGPU_CRATE,
            BTreeSet::from([
                "FrameBudget",
                "FrameExecutionReport",
                "ValidationCapture",
                "ValidationCaptureTicket",
                "WgpuRenderRuntime",
                "WgpuRenderRuntimeConfig",
                "WgpuRenderRuntimeDiagnostics",
                "WgpuRenderRuntimeError",
            ]),
        ),
        (
            REFERENCE_CRATE,
            BTreeSet::from([
                "ReferenceFrame",
                "ReferenceRenderError",
                "ReferenceRenderer",
            ]),
        ),
    ]);
    let forbidden_public = exact_set(&contract.public_api_forbidden_identifiers);
    let expected_forbidden_public = string_set(&[
        "Adapter",
        "BindGroup",
        "BindGroupLayout",
        "Buffer",
        "CommandBuffer",
        "CommandEncoder",
        "ComputePipeline",
        "Device",
        "Queue",
        "RenderPipeline",
        "ShaderModule",
        "Surface",
        "Texture",
        "TextureView",
        "wgpu",
    ]);
    if forbidden_public != expected_forbidden_public
        || forbidden_public.len() != contract.public_api_forbidden_identifiers.len()
    {
        bail!("WP-09A public forbidden identifier set drifted");
    }

    for (name, item) in contracts {
        let expected_path = format!("crates/{name}");
        if item.path != expected_path || item.publish != (name == WGPU_CRATE) {
            bail!("WP-09A crate activation metadata drifted for {name}");
        }
        let (normal_workspace, dev_workspace, normal_external, dev_external, forbidden_imports) =
            if name == WGPU_CRATE {
                (
                    &["mirante4d-dataset", "mirante4d-render-api"][..],
                    &[
                        "mirante4d-dataset-runtime",
                        "mirante4d-domain",
                        "mirante4d-render-reference",
                    ][..],
                    &["bytemuck", "thiserror", "wgpu"][..],
                    &["pollster"][..],
                    &[
                        "eframe",
                        "egui",
                        "mirante4d_analysis",
                        "mirante4d_app",
                        "mirante4d_application",
                        "mirante4d_data",
                        "mirante4d_format",
                        "mirante4d_import",
                        "mirante4d_project_model",
                        "mirante4d_renderer",
                        "mirante4d_settings",
                        "mirante4d_storage",
                        "rfd",
                        "std::fs",
                        "std::path",
                        "winit",
                    ][..],
                )
            } else {
                (
                    &[
                        "mirante4d-dataset",
                        "mirante4d-domain",
                        "mirante4d-render-api",
                    ][..],
                    &[][..],
                    &["thiserror"][..],
                    &[][..],
                    &[
                        "eframe",
                        "egui",
                        "mirante4d_analysis",
                        "mirante4d_app",
                        "mirante4d_application",
                        "mirante4d_data",
                        "mirante4d_dataset_runtime",
                        "mirante4d_format",
                        "mirante4d_import",
                        "mirante4d_project_model",
                        "mirante4d_renderer",
                        "mirante4d_settings",
                        "mirante4d_storage",
                        "rfd",
                        "std::fs",
                        "std::path",
                        "wgpu",
                        "winit",
                    ][..],
                )
            };
        if exact_set(&item.dependencies.normal_workspace) != string_set(normal_workspace)
            || exact_set(&item.dependencies.dev_workspace) != string_set(dev_workspace)
            || exact_set(&item.dependencies.normal_external) != string_set(normal_external)
            || exact_set(&item.dependencies.dev_external) != string_set(dev_external)
            || exact_set(&item.forbidden_import_roots) != string_set(forbidden_imports)
        {
            bail!("WP-09A dependency or source policy drifted for {name}");
        }
        validate_manifest(repo_root, item)?;
        let lib_path = repo_root.join(&item.path).join("src/lib.rs");
        let lib_source = fs::read_to_string(&lib_path)?;
        if !lib_source.contains("#![forbid(unsafe_code)]") {
            bail!("WP-09A crate {name} does not forbid unsafe code at its root");
        }
        let actual_public = public_root_api_names(&lib_path)?;
        let expected = expected_public[name]
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<BTreeSet<_>>();
        if actual_public != expected || exact_set(&item.public_api) != expected {
            bail!("WP-09A public root drifted for {name}");
        }
        let mut violations = Vec::new();
        for path in super::collect_rust_source_files(&repo_root.join(&item.path).join("src"))? {
            let source = fs::read_to_string(&path)?;
            violations.extend(forbidden_import_violations(
                &path,
                &source,
                &exact_set(&item.forbidden_import_roots),
            )?);
            violations.extend(public_api_violations(&path, &source, &forbidden_public)?);
        }
        if !violations.is_empty() {
            bail!(
                "WP-09A {name} source policy failed:\n{}",
                violations.join("\n")
            );
        }
    }
    Ok(())
}

fn validate_manifest(repo_root: &Path, item: &CrateContract) -> anyhow::Result<()> {
    let path = repo_root.join(&item.path).join("Cargo.toml");
    let manifest = fs::read_to_string(&path)?.parse::<toml::Table>()?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .context("WP-09A crate manifest has no package table")?;
    if package.get("name").and_then(toml::Value::as_str) != Some(item.name.as_str())
        || package
            .get("publish")
            .and_then(toml::Value::as_bool)
            .unwrap_or(true)
            != item.publish
    {
        bail!("WP-09A package metadata drifted for {}", item.name);
    }
    let expected_normal = exact_set(&item.dependencies.normal_workspace)
        .union(&exact_set(&item.dependencies.normal_external))
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_dev = exact_set(&item.dependencies.dev_workspace)
        .union(&exact_set(&item.dependencies.dev_external))
        .cloned()
        .collect::<BTreeSet<_>>();
    validate_dependency_table(&manifest, "dependencies", &expected_normal)?;
    validate_dependency_table(&manifest, "dev-dependencies", &expected_dev)?;
    validate_dependency_table(&manifest, "build-dependencies", &BTreeSet::new())?;
    if manifest.contains_key("target") || manifest.contains_key("features") {
        bail!(
            "WP-09A crate {} must not add target-specific dependencies or features",
            item.name
        );
    }
    Ok(())
}

fn validate_dependency_table(
    manifest: &toml::Table,
    table_name: &str,
    expected: &BTreeSet<String>,
) -> anyhow::Result<()> {
    let empty = toml::Table::new();
    let table = manifest
        .get(table_name)
        .map(|value| value.as_table().context("dependency table must be a table"))
        .transpose()?
        .unwrap_or(&empty);
    let actual = table.keys().cloned().collect::<BTreeSet<_>>();
    if &actual != expected {
        bail!("WP-09A {table_name} drifted: expected={expected:?}, actual={actual:?}");
    }
    for (name, specification) in table {
        if specification
            .as_table()
            .and_then(|table| table.get("workspace"))
            .and_then(toml::Value::as_bool)
            != Some(true)
        {
            bail!("WP-09A dependency {name} must inherit its workspace pin");
        }
    }
    Ok(())
}

fn validate_reachability(repo_root: &Path, contract: &RenderContract) -> anyhow::Result<()> {
    let metadata = workspace_dependency_metadata(repo_root)?;
    for (source, kinds) in metadata.declared_dependency_kinds_by_name {
        for (kind, dependencies) in kinds {
            if dependencies.contains(WGPU_CRATE) {
                bail!("off-product WP-09A WGPU successor is reachable from {source} ({kind})");
            }
            if dependencies.contains(REFERENCE_CRATE) && !(source == WGPU_CRATE && kind == "dev") {
                bail!("WP-09A CPU oracle is reachable from {source} ({kind})");
            }
        }
    }
    let declared = contract
        .crates
        .iter()
        .map(|item| item.name.as_str())
        .collect::<BTreeSet<_>>();
    if declared != BTreeSet::from([REFERENCE_CRATE, WGPU_CRATE]) {
        bail!("WP-09A successor ownership drifted");
    }
    Ok(())
}

fn exact_set(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
}

fn string_set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_contract_binds_off_product_successors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        check_wp09a_render_contract(&root).unwrap();
    }

    #[test]
    fn public_guard_rejects_wgpu_in_any_signature_path_segment() {
        let forbidden = BTreeSet::from(["wgpu".to_owned()]);
        let source = r#"
            pub fn leak(
                value: Option<outer::wgpu::Input>,
            ) -> Result<wgpu::Output, crate::errors::wgpu::Failure> {
                todo!()
            }
        "#;
        let violations = public_api_violations(Path::new("synthetic.rs"), source, &forbidden)
            .expect("synthetic public signature parses");
        assert!(!violations.is_empty());

        let private_source = "fn internal(value: wgpu::Device) { let _ = value; }";
        assert!(
            public_api_violations(Path::new("synthetic.rs"), private_source, &forbidden)
                .expect("synthetic private signature parses")
                .is_empty()
        );
    }
}
