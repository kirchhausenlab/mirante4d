use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use serde::Deserialize;
use serde_json::Value;
use syn::visit::Visit;

use super::{collect_rust_source_files, flatten_use_tree};

const CONTRACT_PATH: &str = "architecture/wp10a-storage-contract.json";
const CONTRACT_SCHEMA: &str = "mirante4d-wp10a-storage-successor-contract";
const CONTRACT_SCHEMA_VERSION: u64 = 1;
const CONTRACT_STATUS: &str = "accepted-off-product";
const PREDECESSOR_PATH: &str = "architecture/wp08a-subsystem-contract.json";
const PREDECESSOR_SCHEMA: &str = "mirante4d-wp08a-subsystem-contract";
const PREDECESSOR_SCHEMA_VERSION: u64 = 2;
const PREDECESSOR_SHA256: &str = "0500c27c9c4e13ce2eb0d833534a865c01efb0eabfea34df30015cf9af416cd3";
const ACCEPTED_FREEZE_SHA256: &str =
    "68266ce2c1cf782881824befc2df1c76712068fd445cecccfcd62156aa420807";
const DECISION_PROPOSAL_SHA256: &str =
    "a6edba8779e704664d56c872c62f8051e3b914cec27db460fab247568e7d4f6d";
const STORAGE_CRATE: &str = "mirante4d-storage";
const STORAGE_PATH: &str = "crates/mirante4d-storage";
const DEPENDENCY_KINDS: [&str; 3] = ["normal", "dev", "build"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageContract {
    schema: String,
    schema_version: u64,
    status: String,
    predecessor: PredecessorBinding,
    authorization: AuthorizationBinding,
    activation: ActivationContract,
    identity_successor: IdentitySuccessorContract,
    dependencies: DependencyContract,
    source_policy: SourcePolicy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredecessorBinding {
    path: String,
    schema: String,
    schema_version: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthorizationBinding {
    accepted_profile_freeze_sha256: String,
    normative_decision_proposal_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivationContract {
    crate_name: String,
    crate_path: String,
    lifecycle: String,
    format_family: String,
    storage_profile: String,
    product_status: String,
    product_activation_gate: String,
    product_reachability: bool,
    library_and_tests_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentitySuccessorContract {
    crate_name: String,
    normal_dependency_additions: Vec<String>,
    required_external: Vec<ExternalDependency>,
    public_api_additions: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyContract {
    workspace_by_kind: DependencyKinds,
    workspace_reachability_allowlist: Vec<String>,
    required_external: Vec<ExternalDependency>,
    direct_external_by_kind: DependencyKinds,
    production_closure_scope: String,
    production_third_party_packages_max: u64,
    forbidden_transitive_packages: Vec<String>,
    allowed_workspace_dependents: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DependencyKinds {
    normal: Vec<String>,
    dev: Vec<String>,
    build: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExternalDependency {
    name: String,
    version: String,
    requirement: String,
    checksum: String,
    default_features: bool,
    features: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePolicy {
    roots: Vec<String>,
    optional_roots: Vec<String>,
    forbidden_import_roots: Vec<String>,
    unsafe_allowed: bool,
    forbidden_paths: Vec<String>,
}

pub(super) fn check_wp10a_storage_contract(repo_root: &Path) -> anyhow::Result<()> {
    let contract = read_contract(repo_root)?;
    validate_contract_header(&contract)?;
    validate_predecessor(repo_root, &contract.predecessor)?;
    validate_forbidden_paths(repo_root, &contract.source_policy)?;

    let metadata = cargo_metadata(repo_root)?;
    validate_storage_package(repo_root, &contract, &metadata)?;
    validate_storage_sources(repo_root, &contract.source_policy)?;
    Ok(())
}

fn read_contract(repo_root: &Path) -> anyhow::Result<StorageContract> {
    let path = repo_root.join(CONTRACT_PATH);
    serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))
}

fn validate_contract_header(contract: &StorageContract) -> anyhow::Result<()> {
    if contract.schema != CONTRACT_SCHEMA
        || contract.schema_version != CONTRACT_SCHEMA_VERSION
        || contract.status != CONTRACT_STATUS
        || contract.predecessor.path != PREDECESSOR_PATH
        || contract.predecessor.schema != PREDECESSOR_SCHEMA
        || contract.predecessor.schema_version != PREDECESSOR_SCHEMA_VERSION
        || contract.predecessor.sha256 != PREDECESSOR_SHA256
        || contract.authorization.accepted_profile_freeze_sha256 != ACCEPTED_FREEZE_SHA256
        || contract.authorization.normative_decision_proposal_sha256 != DECISION_PROPOSAL_SHA256
    {
        bail!("{CONTRACT_PATH} does not bind the accepted WP-10A freeze and WP-08A predecessor");
    }

    let activation = &contract.activation;
    if activation.crate_name != STORAGE_CRATE
        || activation.crate_path != STORAGE_PATH
        || activation.lifecycle != "EXPERIMENTAL"
        || activation.format_family != "mirante4d"
        || activation.storage_profile != "m4d-zarr3-local-1.0"
        || activation.product_status != "off-product"
        || activation.product_activation_gate != "WP-10C"
        || activation.product_reachability
        || !activation.library_and_tests_only
    {
        bail!("WP-10A storage activation must remain experimental and off-product until WP-10C");
    }

    if contract.identity_successor.crate_name != "mirante4d-identity" {
        bail!("WP-10A identity successor names the wrong crate");
    }
    require_exact_set(
        &contract.identity_successor.normal_dependency_additions,
        &["mirante4d-domain", "sha2", "unicode-normalization"],
        "identity normal dependency addition",
    )?;
    require_exact_set(
        &contract.identity_successor.public_api_additions,
        accepted_identity_public_api_additions(),
        "identity public API addition",
    )?;
    if contract.identity_successor.required_external != expected_identity_external() {
        bail!("WP-10A frozen identity dependencies drifted");
    }

    require_exact_kinds(
        &contract.dependencies.workspace_by_kind,
        &[
            "mirante4d-dataset",
            "mirante4d-domain",
            "mirante4d-identity",
        ],
        &[],
        &[],
        "workspace dependency",
    )?;
    require_exact_kinds(
        &contract.dependencies.direct_external_by_kind,
        &[
            "crc32c",
            "serde",
            "serde_json",
            "thiserror",
            "zarrs_metadata",
            "zstd",
        ],
        &[],
        &[],
        "external dependency",
    )?;
    require_exact_set(
        &contract.dependencies.workspace_reachability_allowlist,
        &[
            "mirante4d-dataset",
            "mirante4d-domain",
            "mirante4d-identity",
            "mirante4d-storage",
        ],
        "workspace reachability allowlist",
    )?;
    require_exact_set(
        &contract.dependencies.forbidden_transitive_packages,
        &["paste", "zarrs"],
        "forbidden transitive package",
    )?;
    if !contract
        .dependencies
        .allowed_workspace_dependents
        .is_empty()
        || contract.dependencies.production_closure_scope
            != "package-selected-all-features-all-target-normal-and-build"
        || contract.dependencies.production_third_party_packages_max != 55
    {
        bail!(
            "WP-10A storage must have no workspace dependents and at most 55 production third-party packages"
        );
    }

    let expected_external = [
        ExternalDependency {
            name: "crc32c".to_owned(),
            version: "0.6.8".to_owned(),
            requirement: "=0.6.8".to_owned(),
            checksum: "3a47af21622d091a8f0fb295b88bc886ac74efcc613efc19f5d0b21de5c89e47".to_owned(),
            default_features: true,
            features: Vec::new(),
        },
        ExternalDependency {
            name: "zarrs_metadata".to_owned(),
            version: "0.7.5".to_owned(),
            requirement: "=0.7.5".to_owned(),
            checksum: "d60c4c363a8a302d7babb3c29017850a7b4e0af6ca5f9ba2946263a185b62fea".to_owned(),
            default_features: true,
            features: Vec::new(),
        },
        ExternalDependency {
            name: "zstd".to_owned(),
            version: "0.13.3".to_owned(),
            requirement: "=0.13.3".to_owned(),
            checksum: "e91ee311a569c327171651566e07972200e76fcfe2242a4fa446149a3881c08a".to_owned(),
            default_features: false,
            features: Vec::new(),
        },
    ];
    if contract.dependencies.required_external != expected_external {
        bail!("WP-10A frozen codec dependencies drifted");
    }

    require_exact_set(
        &contract.source_policy.roots,
        &[
            "crates/mirante4d-storage/src",
            "crates/mirante4d-storage/tests",
        ],
        "storage source root",
    )?;
    require_exact_set(
        &contract.source_policy.optional_roots,
        &["crates/mirante4d-storage/tests"],
        "optional storage source root",
    )?;
    require_exact_set(
        &contract.source_policy.forbidden_import_roots,
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
            "mirante4d_render_api",
            "mirante4d_renderer",
            "mirante4d_settings",
            "wgpu",
            "winit",
            "xtask",
            "zarrs",
        ],
        "forbidden import root",
    )?;
    require_exact_set(
        &contract.source_policy.forbidden_paths,
        &[
            "crates/mirante4d-storage/build.rs",
            "crates/mirante4d-storage/src/bin",
            "crates/mirante4d-storage/src/main.rs",
        ],
        "forbidden storage path",
    )?;
    if contract.source_policy.unsafe_allowed {
        bail!("WP-10A storage cannot authorize unsafe Rust");
    }
    Ok(())
}

fn expected_identity_external() -> Vec<ExternalDependency> {
    vec![
        ExternalDependency {
            name: "sha2".to_owned(),
            version: "0.11.0".to_owned(),
            requirement: "=0.11.0".to_owned(),
            checksum: "446ba717509524cb3f22f17ecc096f10f4822d76ab5c0b9822c5f9c284e825f4".to_owned(),
            default_features: false,
            features: Vec::new(),
        },
        ExternalDependency {
            name: "unicode-normalization".to_owned(),
            version: "0.1.25".to_owned(),
            requirement: "=0.1.25".to_owned(),
            checksum: "5fd4f6878c9cb28d874b009da9e8d183b5abc80117c40bbd187a1fde336be6e8".to_owned(),
            default_features: true,
            features: Vec::new(),
        },
    ]
}

pub(super) fn accepted_identity_public_api_additions() -> &'static [&'static str] {
    &[
        "M4D_UNICODE_VERSION",
        "SCIENTIFIC_TILE_SHAPE_TZYX",
        "ScientificDatasetHasher",
        "ScientificHashError",
        "ScientificLayerDescriptor",
        "ScientificLayerHasher",
        "ScientificLayerRoot",
        "ScientificTemporalCalibration",
        "ScientificTile",
        "Sha256Hasher",
        "WP10A_ARTIFACT_HAND_VECTOR_BODY",
        "WP10A_ARTIFACT_HAND_VECTOR_ID",
        "is_nfc",
        "normalize_nfc",
        "verify_wp10a_artifact_hand_vector",
    ]
}

fn require_exact_kinds(
    actual: &DependencyKinds,
    normal: &[&str],
    dev: &[&str],
    build: &[&str],
    label: &str,
) -> anyhow::Result<()> {
    require_exact_set(&actual.normal, normal, &format!("{label} normal"))?;
    require_exact_set(&actual.dev, dev, &format!("{label} dev"))?;
    require_exact_set(&actual.build, build, &format!("{label} build"))
}

fn require_exact_set(actual: &[String], expected: &[&str], label: &str) -> anyhow::Result<()> {
    let actual = unique_set(actual, label)?;
    let expected = expected
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if actual != expected {
        bail!("WP-10A {label} drifted: expected={expected:?}, actual={actual:?}");
    }
    Ok(())
}

fn unique_set(values: &[String], label: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() || !result.insert(value.to_owned()) {
            bail!("WP-10A {label} contains an empty or duplicate value");
        }
    }
    Ok(result)
}

fn validate_predecessor(repo_root: &Path, binding: &PredecessorBinding) -> anyhow::Result<()> {
    let path = repo_root.join(&binding.path);
    let digest = sha256_file(&path)?;
    if digest != binding.sha256 {
        bail!(
            "immutable WP-08A predecessor digest drifted: expected {}, found {digest}",
            binding.sha256
        );
    }
    let predecessor: Value = serde_json::from_slice(&fs::read(&path)?)?;
    if predecessor.get("schema").and_then(Value::as_str) != Some(binding.schema.as_str())
        || predecessor.get("schema_version").and_then(Value::as_u64) != Some(binding.schema_version)
        || predecessor.get("status").and_then(Value::as_str) != Some("frozen")
    {
        bail!("WP-10A predecessor is not the frozen WP-08A contract");
    }
    Ok(())
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

fn validate_forbidden_paths(repo_root: &Path, policy: &SourcePolicy) -> anyhow::Result<()> {
    for relative in &policy.forbidden_paths {
        if repo_root.join(relative).exists() {
            bail!("off-product WP-10A storage contains forbidden path {relative}");
        }
    }
    Ok(())
}

fn cargo_metadata(repo_root: &Path) -> anyhow::Result<Value> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version=1",
            "--locked",
            "--offline",
            "--all-features",
        ])
        .current_dir(repo_root)
        .output()
        .context("failed to run cargo metadata for WP-10A storage contract")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed for WP-10A storage contract: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("failed to parse WP-10A cargo metadata")
}

fn validate_storage_package(
    repo_root: &Path,
    contract: &StorageContract,
    metadata: &Value,
) -> anyhow::Result<()> {
    let workspace_members = string_array(metadata, "workspace_members", "cargo metadata")?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .context("cargo metadata has no packages")?;
    let package_by_id = packages
        .iter()
        .map(|package| {
            Ok((
                string_field(package, "id", "cargo package")?.to_owned(),
                package,
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let storage = packages
        .iter()
        .filter(|package| {
            package.get("name").and_then(Value::as_str) == Some(STORAGE_CRATE)
                && package
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| workspace_members.contains(id))
        })
        .collect::<Vec<_>>();
    if storage.len() != 1 {
        bail!("WP-10A requires exactly one workspace package named {STORAGE_CRATE}");
    }
    let storage = storage[0];
    let identity = packages
        .iter()
        .filter(|package| {
            package.get("name").and_then(Value::as_str) == Some("mirante4d-identity")
                && package
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| workspace_members.contains(id))
        })
        .collect::<Vec<_>>();
    if identity.len() != 1 {
        bail!("WP-10A requires exactly one workspace package named mirante4d-identity");
    }
    validate_storage_manifest_path(repo_root, storage)?;
    validate_storage_targets(storage)?;
    validate_direct_dependencies(storage, contract)?;
    validate_identity_dependencies(identity[0], contract)?;
    validate_locked_dependencies(repo_root, contract)?;
    validate_dependency_graph(
        repo_root,
        metadata,
        &package_by_id,
        &workspace_members,
        contract,
    )
}

fn validate_storage_manifest_path(repo_root: &Path, package: &Value) -> anyhow::Result<()> {
    let actual = PathBuf::from(string_field(package, "manifest_path", "storage package")?);
    let expected = repo_root.join(STORAGE_PATH).join("Cargo.toml");
    if fs::canonicalize(&actual).ok() != fs::canonicalize(&expected).ok()
        || !repo_root.join(STORAGE_PATH).join("src/lib.rs").is_file()
    {
        bail!("{STORAGE_CRATE} must be the library crate at {STORAGE_PATH}");
    }
    Ok(())
}

fn validate_storage_targets(package: &Value) -> anyhow::Result<()> {
    let targets = package
        .get("targets")
        .and_then(Value::as_array)
        .context("storage package has no targets")?;
    let mut library_count = 0;
    for target in targets {
        for kind in string_array(target, "kind", "storage target")? {
            match kind.as_str() {
                "lib" => library_count += 1,
                "test" => {}
                other => bail!("off-product storage has forbidden Cargo target kind {other:?}"),
            }
        }
    }
    if library_count != 1 {
        bail!("off-product storage must expose exactly one library target");
    }
    Ok(())
}

fn validate_direct_dependencies(package: &Value, contract: &StorageContract) -> anyhow::Result<()> {
    let dependencies = package
        .get("dependencies")
        .and_then(Value::as_array)
        .context("storage package has no dependency inventory")?;
    let expected_workspace = kinds_as_sets(&contract.dependencies.workspace_by_kind)?;
    let expected_external = kinds_as_sets(&contract.dependencies.direct_external_by_kind)?;
    let mut actual_workspace = empty_kind_sets();
    let mut actual_external = empty_kind_sets();
    let required = contract
        .dependencies
        .required_external
        .iter()
        .map(|dependency| (dependency.name.as_str(), dependency))
        .collect::<BTreeMap<_, _>>();
    let mut observed_required = BTreeSet::new();

    for dependency in dependencies {
        let name = string_field(dependency, "name", "storage dependency")?;
        if dependency
            .get("rename")
            .is_some_and(|value| !value.is_null())
            || dependency
                .get("target")
                .is_some_and(|value| !value.is_null())
            || dependency.get("optional").and_then(Value::as_bool) != Some(false)
        {
            bail!("storage dependency {name:?} cannot be renamed, target-specific, or optional");
        }
        let kind = dependency
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("normal");
        if !DEPENDENCY_KINDS.contains(&kind) {
            bail!("storage dependency {name:?} has unsupported kind {kind:?}");
        }
        let destination = if dependency.get("path").is_some_and(|value| !value.is_null()) {
            &mut actual_workspace
        } else {
            &mut actual_external
        };
        if !destination
            .get_mut(kind)
            .expect("all dependency kinds are initialized")
            .insert(name.to_owned())
        {
            bail!("storage repeats {kind} dependency {name:?}");
        }

        if let Some(frozen) = required.get(name) {
            if kind != "normal"
                || string_field(dependency, "req", "frozen storage dependency")?
                    != frozen.requirement
                || dependency
                    .get("uses_default_features")
                    .and_then(Value::as_bool)
                    != Some(frozen.default_features)
                || string_array(dependency, "features", "frozen storage dependency")?
                    != frozen.features.iter().cloned().collect::<BTreeSet<_>>()
            {
                bail!("frozen storage dependency {name:?} declaration drifted");
            }
            observed_required.insert(name);
        }
    }
    if actual_workspace != expected_workspace || actual_external != expected_external {
        bail!(
            "WP-10A direct dependency sets drifted: workspace expected={expected_workspace:?} actual={actual_workspace:?}; external expected={expected_external:?} actual={actual_external:?}"
        );
    }
    if observed_required != required.keys().copied().collect() {
        bail!("WP-10A frozen external dependency set is incomplete");
    }
    Ok(())
}

fn validate_locked_dependencies(
    repo_root: &Path,
    contract: &StorageContract,
) -> anyhow::Result<()> {
    let lock = fs::read_to_string(repo_root.join("Cargo.lock"))?
        .parse::<toml::Table>()
        .context("failed to parse Cargo.lock for WP-10A dependency checks")?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .context("Cargo.lock has no package inventory")?;
    for expected in contract
        .dependencies
        .required_external
        .iter()
        .chain(&contract.identity_successor.required_external)
    {
        let matches = packages
            .iter()
            .filter(|package| {
                package.get("name").and_then(toml::Value::as_str) == Some(expected.name.as_str())
                    && package.get("version").and_then(toml::Value::as_str)
                        == Some(expected.version.as_str())
                    && package
                        .get("source")
                        .and_then(toml::Value::as_str)
                        .is_some_and(|source| source.starts_with("registry+"))
            })
            .collect::<Vec<_>>();
        if matches.len() != 1
            || matches[0].get("checksum").and_then(toml::Value::as_str)
                != Some(expected.checksum.as_str())
        {
            bail!(
                "Cargo resolution does not contain exact frozen {} {} checksum {}",
                expected.name,
                expected.version,
                expected.checksum
            );
        }
    }
    Ok(())
}

fn validate_identity_dependencies(
    package: &Value,
    contract: &StorageContract,
) -> anyhow::Result<()> {
    let dependencies = package
        .get("dependencies")
        .and_then(Value::as_array)
        .context("identity package has no dependency inventory")?;
    let required = contract
        .identity_successor
        .required_external
        .iter()
        .map(|dependency| (dependency.name.as_str(), dependency))
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeSet::new();
    for dependency in dependencies {
        let name = string_field(dependency, "name", "identity dependency")?;
        let Some(frozen) = required.get(name) else {
            continue;
        };
        if dependency.get("path").is_some_and(|value| !value.is_null())
            || dependency
                .get("rename")
                .is_some_and(|value| !value.is_null())
            || dependency
                .get("target")
                .is_some_and(|value| !value.is_null())
            || dependency.get("kind").is_some_and(|value| !value.is_null())
            || dependency.get("optional").and_then(Value::as_bool) != Some(false)
            || string_field(dependency, "req", "frozen identity dependency")? != frozen.requirement
            || dependency
                .get("uses_default_features")
                .and_then(Value::as_bool)
                != Some(frozen.default_features)
            || string_array(dependency, "features", "frozen identity dependency")?
                != frozen.features.iter().cloned().collect::<BTreeSet<_>>()
        {
            bail!("frozen identity dependency {name:?} declaration drifted");
        }
        observed.insert(name);
    }
    if observed != required.keys().copied().collect() {
        bail!("WP-10A frozen identity dependency set is incomplete");
    }
    Ok(())
}

fn validate_dependency_graph(
    repo_root: &Path,
    metadata: &Value,
    package_by_id: &BTreeMap<String, &Value>,
    workspace_members: &BTreeSet<String>,
    contract: &StorageContract,
) -> anyhow::Result<()> {
    let nodes = metadata
        .pointer("/resolve/nodes")
        .and_then(Value::as_array)
        .context("cargo metadata has no resolve graph")?;
    let node_by_id = nodes
        .iter()
        .map(|node| Ok((string_field(node, "id", "resolve node")?.to_owned(), node)))
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let storage_id = workspace_members
        .iter()
        .find(|id| {
            package_by_id
                .get(*id)
                .and_then(|package| package.get("name"))
                .and_then(Value::as_str)
                == Some(STORAGE_CRATE)
        })
        .context("storage package is absent from the resolve graph")?;

    // Workspace-wide Cargo metadata unifies features across every selected
    // package. Use Cargo's package-selected tree for the storage closure so
    // unrelated workspace features and target dependencies cannot inflate or
    // hide this off-product crate's actual graph.
    let all_reachable = cargo_tree_closure(repo_root, "normal,build,dev")?;
    let production_reachable = cargo_tree_closure(repo_root, "normal,build")?;
    let forbidden = unique_set(
        &contract.dependencies.forbidden_transitive_packages,
        "forbidden transitive package",
    )?;
    for name in &all_reachable.names {
        if forbidden.contains(name) {
            bail!("WP-10A storage transitively reaches forbidden package {name:?}");
        }
    }

    let allowed_workspace = unique_set(
        &contract.dependencies.workspace_reachability_allowlist,
        "workspace reachability allowlist",
    )?;
    let workspace_names = workspace_members
        .iter()
        .map(|id| {
            package_by_id
                .get(id)
                .and_then(|package| package.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .context("workspace package has no name")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let reached_workspace_names = all_reachable
        .names
        .intersection(&workspace_names)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !reached_workspace_names.is_subset(&allowed_workspace) {
        bail!(
            "WP-10A storage reaches forbidden workspace packages {:?}",
            reached_workspace_names
                .difference(&allowed_workspace)
                .collect::<Vec<_>>()
        );
    }

    let production_third_party = production_reachable
        .packages
        .iter()
        .filter(|(name, _version)| !workspace_names.contains(name))
        .cloned()
        .collect::<BTreeSet<_>>();
    if production_third_party.len() as u64
        > contract.dependencies.production_third_party_packages_max
    {
        bail!(
            "WP-10A storage production closure has {} third-party packages, limit {}: {production_third_party:?}",
            production_third_party.len(),
            contract.dependencies.production_third_party_packages_max
        );
    }

    for origin in workspace_members {
        if origin == storage_id {
            continue;
        }
        if reachable_package_ids(origin, &node_by_id, true)?.contains(storage_id) {
            let origin_name = package_by_id
                .get(origin)
                .and_then(|package| package.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            bail!("off-product storage is reachable from workspace package {origin_name:?}");
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct CargoTreeClosure {
    packages: BTreeSet<(String, String)>,
    names: BTreeSet<String>,
}

fn cargo_tree_closure(repo_root: &Path, edges: &str) -> anyhow::Result<CargoTreeClosure> {
    let output = Command::new("cargo")
        .args([
            "tree",
            "--locked",
            "--offline",
            "--package",
            STORAGE_CRATE,
            "--all-features",
            "--target",
            "all",
            "--edges",
            edges,
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .current_dir(repo_root)
        .output()
        .context("failed to run cargo tree for WP-10A storage contract")?;
    if !output.status.success() {
        bail!(
            "cargo tree failed for WP-10A storage contract: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8(output.stdout).context("cargo tree output was not UTF-8")?;
    let closure = parse_cargo_tree_closure(&stdout)?;
    if !closure.names.contains(STORAGE_CRATE) {
        bail!("cargo tree did not contain the WP-10A storage root");
    }
    Ok(closure)
}

fn parse_cargo_tree_closure(stdout: &str) -> anyhow::Result<CargoTreeClosure> {
    let mut packages = BTreeSet::new();
    let mut names = BTreeSet::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let name = fields
            .next()
            .context("cargo tree row has no package name")?;
        let version = fields
            .next()
            .context("cargo tree row has no package version")?;
        if !version.starts_with('v') || version.len() == 1 {
            bail!("cargo tree row has malformed package version: {line:?}");
        }
        packages.insert((name.to_owned(), version[1..].to_owned()));
        names.insert(name.to_owned());
    }
    if packages.is_empty() {
        bail!("cargo tree returned an empty package closure");
    }
    Ok(CargoTreeClosure { packages, names })
}

fn reachable_package_ids(
    root: &str,
    node_by_id: &BTreeMap<String, &Value>,
    include_dev: bool,
) -> anyhow::Result<BTreeSet<String>> {
    let mut reached = BTreeSet::from([root.to_owned()]);
    let mut queue = VecDeque::from([root.to_owned()]);
    while let Some(id) = queue.pop_front() {
        let node = node_by_id
            .get(&id)
            .with_context(|| format!("resolve graph has no node for {id}"))?;
        let dependencies = node
            .get("deps")
            .and_then(Value::as_array)
            .context("resolve node has no dependencies")?;
        for dependency in dependencies {
            let admitted = dependency
                .get("dep_kinds")
                .and_then(Value::as_array)
                .context("resolve dependency has no kind inventory")?
                .iter()
                .any(|kind| {
                    include_dev
                        || kind.get("kind").and_then(Value::as_str).unwrap_or("normal") != "dev"
                });
            if !admitted {
                continue;
            }
            let dependency_id = string_field(dependency, "pkg", "resolve dependency")?;
            if reached.insert(dependency_id.to_owned()) {
                queue.push_back(dependency_id.to_owned());
            }
        }
    }
    Ok(reached)
}

fn kinds_as_sets(kinds: &DependencyKinds) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    Ok(BTreeMap::from([
        (
            "normal".to_owned(),
            unique_set(&kinds.normal, "normal dependency")?,
        ),
        ("dev".to_owned(), unique_set(&kinds.dev, "dev dependency")?),
        (
            "build".to_owned(),
            unique_set(&kinds.build, "build dependency")?,
        ),
    ]))
}

fn empty_kind_sets() -> BTreeMap<String, BTreeSet<String>> {
    DEPENDENCY_KINDS
        .into_iter()
        .map(|kind| (kind.to_owned(), BTreeSet::new()))
        .collect()
}

fn validate_storage_sources(repo_root: &Path, policy: &SourcePolicy) -> anyhow::Result<()> {
    let optional = unique_set(&policy.optional_roots, "optional source root")?;
    let forbidden = unique_set(&policy.forbidden_import_roots, "forbidden import root")?;
    let mut saw_source = false;
    for relative in &policy.roots {
        let root = repo_root.join(relative);
        if !root.exists() && optional.contains(relative) {
            continue;
        }
        if !root.is_dir() {
            bail!("WP-10A storage source root is missing: {relative}");
        }
        for path in collect_rust_source_files(&root)? {
            saw_source = true;
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let file = syn::parse_file(&source)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            let mut visitor = StorageSourceVisitor {
                path: &path,
                forbidden_roots: &forbidden,
                unsafe_allowed: policy.unsafe_allowed,
                violations: Vec::new(),
            };
            visitor.visit_file(&file);
            if !visitor.violations.is_empty() {
                bail!(
                    "WP-10A storage source policy failed:\n{}",
                    visitor.violations.join("\n")
                );
            }
        }
    }
    if !saw_source {
        bail!("WP-10A storage contains no Rust source");
    }
    Ok(())
}

struct StorageSourceVisitor<'a> {
    path: &'a Path,
    forbidden_roots: &'a BTreeSet<String>,
    unsafe_allowed: bool,
    violations: Vec<String>,
}

impl StorageSourceVisitor<'_> {
    fn reject_root(&mut self, root: &str) {
        if self.forbidden_roots.contains(root) {
            self.violations.push(format!(
                "{} imports forbidden authority {root}",
                self.path.display()
            ));
        }
    }

    fn reject_unsafe(&mut self, kind: &str) {
        if !self.unsafe_allowed {
            self.violations.push(format!(
                "{} contains forbidden unsafe {kind}",
                self.path.display()
            ));
        }
    }
}

impl<'ast> Visit<'ast> for StorageSourceVisitor<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for path in paths {
            if let Some(root) = path.first() {
                self.reject_root(root);
            }
        }
        syn::visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        self.reject_root(&item.ident.to_string());
        syn::visit::visit_item_extern_crate(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(root) = path.segments.first() {
            self.reject_root(&root.ident.to_string());
        }
        syn::visit::visit_path(self, path);
    }

    fn visit_expr_unsafe(&mut self, expression: &'ast syn::ExprUnsafe) {
        self.reject_unsafe("block");
        syn::visit::visit_expr_unsafe(self, expression);
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if item.sig.unsafety.is_some() {
            self.reject_unsafe("function");
        }
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if item.sig.unsafety.is_some() {
            self.reject_unsafe("method");
        }
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if item.unsafety.is_some() {
            self.reject_unsafe("implementation");
        }
        syn::visit::visit_item_impl(self, item);
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        if item.unsafety.is_some() {
            self.reject_unsafe("trait");
        }
        syn::visit::visit_item_trait(self, item);
    }

    fn visit_item_foreign_mod(&mut self, item: &'ast syn::ItemForeignMod) {
        self.reject_unsafe("foreign module");
        syn::visit::visit_item_foreign_mod(self, item);
    }
}

fn string_field<'a>(value: &'a Value, field: &str, context: &str) -> anyhow::Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("{context} has no string field {field:?}"))
}

fn string_array(value: &Value, field: &str, context: &str) -> anyhow::Result<BTreeSet<String>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("{context} has no array field {field:?}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .with_context(|| format!("{context} field {field:?} contains a non-string"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_contract_binds_freeze_and_predecessor() {
        let contract: StorageContract = serde_json::from_str(include_str!(
            "../../../../architecture/wp10a-storage-contract.json"
        ))
        .unwrap();

        validate_contract_header(&contract).unwrap();
        validate_predecessor(Path::new("../.."), &contract.predecessor).unwrap();
    }

    #[test]
    fn source_policy_rejects_forbidden_authority_and_unsafe() {
        let forbidden = BTreeSet::from(["mirante4d_format".to_owned()]);
        let file =
            syn::parse_file("use mirante4d_format::Manifest; unsafe fn bypass() { unsafe {} }")
                .unwrap();
        let mut visitor = StorageSourceVisitor {
            path: Path::new("storage.rs"),
            forbidden_roots: &forbidden,
            unsafe_allowed: false,
            violations: Vec::new(),
        };

        visitor.visit_file(&file);

        assert!(
            visitor
                .violations
                .iter()
                .any(|violation| violation.contains("mirante4d_format"))
        );
        assert!(
            visitor
                .violations
                .iter()
                .any(|violation| violation.contains("unsafe function"))
        );
        assert!(
            visitor
                .violations
                .iter()
                .any(|violation| violation.contains("unsafe block"))
        );
    }

    #[test]
    fn reachability_distinguishes_production_from_dev_edges() {
        let root = serde_json::json!({
            "id": "storage",
            "deps": [
                {"pkg": "normal", "dep_kinds": [{"kind": null, "target": null}]},
                {"pkg": "dev", "dep_kinds": [{"kind": "dev", "target": null}]}
            ]
        });
        let normal = serde_json::json!({"id": "normal", "deps": []});
        let dev = serde_json::json!({"id": "dev", "deps": []});
        let nodes = BTreeMap::from([
            ("storage".to_owned(), &root),
            ("normal".to_owned(), &normal),
            ("dev".to_owned(), &dev),
        ]);

        assert_eq!(
            reachable_package_ids("storage", &nodes, false).unwrap(),
            BTreeSet::from(["normal".to_owned(), "storage".to_owned()])
        );
        assert_eq!(
            reachable_package_ids("storage", &nodes, true).unwrap(),
            BTreeSet::from(["dev".to_owned(), "normal".to_owned(), "storage".to_owned()])
        );
    }

    #[test]
    fn cargo_tree_parser_counts_multiple_versions_as_distinct_packages() {
        let closure = parse_cargo_tree_closure(
            "same v1.0.0\nsame v2.0.0\nsame v2.0.0 (*)\nother v3.0.0 (proc-macro)\n",
        )
        .unwrap();

        assert_eq!(closure.packages.len(), 3);
        assert_eq!(
            closure.names,
            BTreeSet::from(["other".into(), "same".into()])
        );
    }
}
