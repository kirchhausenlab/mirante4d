use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, bail};
use serde::Deserialize;
use syn::visit::Visit;

use super::{
    WorkspaceDependencyMetadata, collect_rust_source_files, flatten_use_tree,
    public_root_api_names, workspace_dependency_metadata,
};

const CONTRACT_PATH: &str = "architecture/wp08a-subsystem-contract.json";
const CONTRACT_SCHEMA: &str = "mirante4d-wp08a-subsystem-contract";
const CONTRACT_SCHEMA_VERSION: u64 = 2;
const CONTRACT_STATUS: &str = "frozen";
const ENTRY_SHA256: &str = "5d4a0b73c9e0689fad6295c61f25ada7a1095ea65436e846450ed72b41b9b477";
const ENTRY_CLARIFICATION_SHA256: &str =
    "3ce060eb0f4ec259b6581419b4785df461b6d9d3c9594cb145d043d2a5c953d1";
const CORRECTIVE_ENTRY_SHA256: &str =
    "413d16ebe0094f3d9a2160e2fdcfc459c61d8d1c809e741e7d478af6fe144ddf";
const CPU_LEDGER_OWNER: &str = "mirante4d-dataset-runtime";
const CPU_LEDGER_CONTRACT_CRATE: &str = "mirante4d-dataset";
const GPU_LEDGER_OWNER: &str = "mirante4d-render-wgpu";
const GPU_LEDGER_CONTRACT_CRATE: &str = "mirante4d-render-api";
const DEPENDENCY_KINDS: [&str; 3] = ["normal", "dev", "build"];
const TARGET_CRATES: [&str; 17] = [
    "mirante4d-analysis-core",
    "mirante4d-analysis-runtime",
    "mirante4d-app",
    "mirante4d-application",
    "mirante4d-dataset",
    "mirante4d-dataset-runtime",
    "mirante4d-domain",
    "mirante4d-identity",
    "mirante4d-import-pipeline",
    "mirante4d-project-model",
    "mirante4d-project-store",
    "mirante4d-render-api",
    "mirante4d-render-reference",
    "mirante4d-render-wgpu",
    "mirante4d-settings",
    "mirante4d-storage",
    "mirante4d-ui-egui",
];
const SUCCESSOR_OWNED_WORKSPACE_CRATES: [&str; 7] = [
    "mirante4d-analysis-core",
    "mirante4d-analysis-runtime",
    "mirante4d-import-pipeline",
    "mirante4d-project-store",
    "mirante4d-render-reference",
    "mirante4d-render-wgpu",
    "mirante4d-storage",
];
const RETIRED_WORKSPACE_CRATES: [&str; 4] = [
    "mirante4d-analysis",
    "mirante4d-data",
    "mirante4d-format",
    "mirante4d-import",
];

// Keep the frozen WP-08A predecessor matrix unchanged while checking the
// exact dependency changes made by accepted successor cutovers.
pub(super) fn accepted_successor_normal_dependency_additions(
    crate_name: &str,
) -> &'static [&'static str] {
    match crate_name {
        "mirante4d-app" => &[
            "mirante4d-analysis-core",
            "mirante4d-analysis-runtime",
            "mirante4d-dataset-runtime",
            "mirante4d-import-pipeline",
            "mirante4d-project-store",
            "mirante4d-storage",
        ],
        "mirante4d-application" => &["mirante4d-project-store"],
        "mirante4d-renderer" => &["mirante4d-dataset"],
        "mirante4d-identity" => &["mirante4d-domain", "sha2", "unicode-normalization"],
        "mirante4d-project-store" => &["mirante4d-domain"],
        "xtask" => &["mirante4d-storage"],
        _ => &[],
    }
}

fn accepted_successor_normal_dependency_removals(crate_name: &str) -> &'static [&'static str] {
    match crate_name {
        "mirante4d-app" => &[
            "mirante4d-analysis",
            "mirante4d-data",
            "mirante4d-format",
            "mirante4d-identity",
            "mirante4d-import",
        ],
        "mirante4d-renderer" => &["mirante4d-data", "mirante4d-format"],
        "xtask" => &[
            "glam",
            "mirante4d-analysis",
            "mirante4d-data",
            "mirante4d-domain",
            "mirante4d-format",
            "mirante4d-import",
            "mirante4d-render-api",
            "mirante4d-renderer",
        ],
        _ => &[],
    }
}

// Keep the frozen WP-08A public-root inventory unchanged while admitting exact
// additions from accepted successor cutovers.
pub(super) fn accepted_successor_public_root_additions(
    crate_name: &str,
) -> &'static [&'static str] {
    match crate_name {
        "mirante4d-application" => &[
            "LoadedAnalysisDescriptorBundle",
            "MonotonicClock",
            "ProjectRecoveryStoreLocator",
            "ProjectStoreApplicationService",
            "ProjectStoreLifecycle",
            "ProjectStoreServiceError",
            "ProjectStoreServiceEvent",
            "ProjectStoreServiceStatus",
            "SystemMonotonicClock",
        ],
        _ => &[],
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubsystemContract {
    schema: String,
    schema_version: u64,
    status: String,
    entry_sha256: String,
    entry_clarification_sha256: String,
    corrective_entry_sha256: String,
    workspace_dependency_matrix: Vec<DependencyContract>,
    target_dependency_matrix: Vec<TargetDependencyContract>,
    transitional_edges: Vec<TransitionalEdge>,
    side_effect_capabilities: Vec<SideEffectCapability>,
    frozen_public_roots: Vec<FrozenPublicRoot>,
    public_api_forbidden_identifiers: Vec<String>,
    frozen_source_rules: Vec<FrozenSourceRule>,
    resource_allocations: Vec<ResourceAllocation>,
    ledger_authorities: LedgerAuthorities,
    restricted_trait_implementations: Vec<RestrictedTraitImplementation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyContract {
    name: String,
    path: String,
    dependencies: DependencyKinds,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DependencyKinds {
    normal: Vec<String>,
    dev: Vec<String>,
    build: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetDependencyContract {
    name: String,
    normal: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionalEdge {
    from: String,
    to: String,
    kind: String,
    reason: String,
    expiry_gate: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SideEffectCapability {
    capability: String,
    target_owner: String,
    owners: Vec<SideEffectOwner>,
    current_exceptions: Vec<SideEffectException>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SideEffectOwner {
    #[serde(rename = "crate")]
    crate_name: String,
    evidence_dependency: Option<String>,
    evidence_source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SideEffectException {
    #[serde(rename = "crate")]
    crate_name: String,
    reason: String,
    expiry_gate: String,
    evidence_dependency: Option<String>,
    evidence_source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenPublicRoot {
    #[serde(rename = "crate")]
    crate_name: String,
    path: String,
    items: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenSourceRule {
    path: String,
    forbidden_imports: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceAllocation {
    class: String,
    owner: String,
    ledger_category: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerAuthorities {
    cpu: LedgerAuthority,
    gpu: LedgerAuthority,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerAuthority {
    owner: String,
    contract_crate: String,
    categories: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestrictedTraitImplementation {
    trait_name: String,
    owner: String,
    required: bool,
}

pub(super) fn check_wp08a_subsystem_contract(repo_root: &Path) -> anyhow::Result<()> {
    let path = repo_root.join(CONTRACT_PATH);
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let contract: SubsystemContract = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    validate_header(&contract)?;
    let target_graph = validate_target_dependency_matrix(&contract)?;
    let metadata = workspace_dependency_metadata(repo_root)?;
    let crate_paths = validate_dependency_matrix(repo_root, &contract, &metadata)?;
    validate_transitional_edges(&contract, &metadata)?;
    validate_normal_edge_closure(&contract, &metadata, &target_graph)?;
    validate_side_effect_capabilities(
        repo_root,
        &contract,
        &metadata,
        &crate_paths,
        &target_graph,
    )?;
    validate_frozen_public_api(repo_root, &contract, &crate_paths)?;
    validate_resource_allocations(&contract, &target_graph)?;
    validate_restricted_trait_implementations(repo_root, &contract, &crate_paths)?;
    validate_frozen_source_rules(repo_root, &contract)?;
    Ok(())
}

fn validate_target_dependency_matrix(
    contract: &SubsystemContract,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let mut graph = BTreeMap::new();
    for entry in &contract.target_dependency_matrix {
        require_nonempty("target dependency crate", &entry.name)?;
        let dependencies = unique_string_set(
            &entry.normal,
            &format!("WP-08A target dependency list for {}", entry.name),
        )?;
        if dependencies.contains(&entry.name) {
            bail!("WP-08A target crate {} depends on itself", entry.name);
        }
        if graph.insert(entry.name.clone(), dependencies).is_some() {
            bail!("WP-08A target dependency matrix repeats {}", entry.name);
        }
    }

    let required = TARGET_CRATES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    let observed = graph.keys().cloned().collect::<BTreeSet<_>>();
    if observed != required {
        bail!(
            "WP-08A target dependency matrix has the wrong crate set: missing={:?}, extra={:?}",
            required.difference(&observed).collect::<Vec<_>>(),
            observed.difference(&required).collect::<Vec<_>>()
        );
    }

    for (name, dependencies) in &graph {
        let unknown = dependencies.difference(&required).collect::<Vec<_>>();
        if !unknown.is_empty() {
            bail!("WP-08A target crate {name} has unknown dependencies {unknown:?}");
        }
    }

    let mut states = BTreeMap::new();
    let mut stack = Vec::new();
    for name in graph.keys() {
        visit_target_dependency(name, &graph, &mut states, &mut stack)?;
    }
    Ok(graph)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetVisitState {
    Visiting,
    Complete,
}

fn visit_target_dependency(
    name: &str,
    graph: &BTreeMap<String, BTreeSet<String>>,
    states: &mut BTreeMap<String, TargetVisitState>,
    stack: &mut Vec<String>,
) -> anyhow::Result<()> {
    match states.get(name) {
        Some(TargetVisitState::Complete) => return Ok(()),
        Some(TargetVisitState::Visiting) => {
            stack.push(name.to_owned());
            bail!(
                "WP-08A target dependency matrix contains a cycle: {}",
                stack.join(" -> ")
            );
        }
        None => {}
    }

    states.insert(name.to_owned(), TargetVisitState::Visiting);
    stack.push(name.to_owned());
    for dependency in graph
        .get(name)
        .expect("target graph was validated before traversal")
    {
        visit_target_dependency(dependency, graph, states, stack)?;
    }
    stack.pop();
    states.insert(name.to_owned(), TargetVisitState::Complete);
    Ok(())
}

fn validate_header(contract: &SubsystemContract) -> anyhow::Result<()> {
    if contract.schema != CONTRACT_SCHEMA
        || contract.schema_version != CONTRACT_SCHEMA_VERSION
        || contract.status != CONTRACT_STATUS
        || contract.entry_sha256 != ENTRY_SHA256
        || contract.entry_clarification_sha256 != ENTRY_CLARIFICATION_SHA256
        || contract.corrective_entry_sha256 != CORRECTIVE_ENTRY_SHA256
    {
        bail!(
            "{CONTRACT_PATH} must bind schema {CONTRACT_SCHEMA} v{CONTRACT_SCHEMA_VERSION}, status {CONTRACT_STATUS}, WP-08A entry {ENTRY_SHA256}, clarification {ENTRY_CLARIFICATION_SHA256}, and corrective entry {CORRECTIVE_ENTRY_SHA256}"
        );
    }
    Ok(())
}

fn validate_dependency_matrix(
    repo_root: &Path,
    contract: &SubsystemContract,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut declared = BTreeMap::new();
    for entry in &contract.workspace_dependency_matrix {
        require_nonempty("dependency-matrix crate name", &entry.name)?;
        require_nonempty("dependency-matrix crate path", &entry.path)?;
        if declared.insert(entry.name.clone(), entry).is_some() {
            bail!("WP-08A dependency matrix repeats crate {}", entry.name);
        }
    }

    // Separately owned successor crates are delegated to their package checks;
    // every other live workspace crate must match the frozen registry after
    // accepted predecessor retirement.
    let actual_crates = metadata
        .declared_dependency_kinds_by_name
        .keys()
        .filter(|name| !SUCCESSOR_OWNED_WORKSPACE_CRATES.contains(&name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_crates = declared
        .keys()
        .filter(|name| !RETIRED_WORKSPACE_CRATES.contains(&name.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    if actual_crates != expected_crates {
        bail!(
            "WP-08A dependency matrix does not cover the workspace exactly: missing={:?}, extra={:?}",
            actual_crates
                .difference(&expected_crates)
                .collect::<Vec<_>>(),
            expected_crates
                .difference(&actual_crates)
                .collect::<Vec<_>>()
        );
    }

    let mut crate_paths = BTreeMap::new();
    for (name, entry) in declared {
        if RETIRED_WORKSPACE_CRATES.contains(&name.as_str()) {
            continue;
        }
        let manifest_path = repo_root.join(&entry.path).join("Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest = manifest
            .parse::<toml::Table>()
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        if manifest
            .get("package")
            .and_then(toml::Value::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            != Some(name.as_str())
        {
            bail!(
                "WP-08A dependency-matrix path {} does not contain package {name}",
                entry.path
            );
        }

        let actual = metadata
            .declared_dependency_kinds_by_name
            .get(&name)
            .with_context(|| format!("cargo metadata is missing workspace crate {name}"))?;
        let expected_by_kind = [
            ("normal", &entry.dependencies.normal),
            ("dev", &entry.dependencies.dev),
            ("build", &entry.dependencies.build),
        ];
        for (kind, expected) in expected_by_kind {
            let mut expected =
                unique_string_set(expected, &format!("WP-08A {name} {kind} dependency list"))?;
            if kind == "normal" {
                expected.extend(
                    accepted_successor_normal_dependency_additions(&name)
                        .iter()
                        .map(|dependency| (*dependency).to_owned()),
                );
                for dependency in accepted_successor_normal_dependency_removals(&name) {
                    if !expected.remove(*dependency) {
                        bail!(
                            "accepted dependency removal {name} -> {dependency} is absent from the frozen predecessor matrix"
                        );
                    }
                }
            }
            let actual = actual.get(kind).cloned().unwrap_or_default();
            if actual != expected {
                bail!(
                    "WP-08A {name} {kind} dependencies drifted: missing={:?}, extra={:?}",
                    expected.difference(&actual).collect::<Vec<_>>(),
                    actual.difference(&expected).collect::<Vec<_>>()
                );
            }
        }
        let unsupported = actual
            .keys()
            .filter(|kind| !DEPENDENCY_KINDS.contains(&kind.as_str()))
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            bail!("WP-08A {name} has unsupported dependency kinds {unsupported:?}");
        }
        crate_paths.insert(name.clone(), entry.path.clone());
    }
    Ok(crate_paths)
}

fn validate_transitional_edges(
    contract: &SubsystemContract,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    let mut observed = BTreeSet::new();
    for edge in &contract.transitional_edges {
        require_nonempty("transitional-edge reason", &edge.reason)?;
        validate_expiry_gate(&edge.expiry_gate)?;
        if !DEPENDENCY_KINDS.contains(&edge.kind.as_str()) {
            bail!(
                "WP-08A transitional edge {} -> {} has invalid kind {}",
                edge.from,
                edge.to,
                edge.kind
            );
        }
        if !observed.insert((&edge.from, &edge.to, &edge.kind)) {
            bail!(
                "WP-08A repeats transitional edge {} -> {} ({})",
                edge.from,
                edge.to,
                edge.kind
            );
        }
        if accepted_expired_transitional_edge(edge) {
            continue;
        }
        if !metadata
            .declared_dependency_kinds_by_name
            .contains_key(&edge.to)
        {
            bail!(
                "WP-08A transitional edge target {} is not a workspace crate",
                edge.to
            );
        }
        let dependencies = metadata
            .declared_dependency_kinds_by_name
            .get(&edge.from)
            .and_then(|kinds| kinds.get(&edge.kind))
            .with_context(|| {
                format!(
                    "WP-08A transitional edge source {} has no {} dependencies",
                    edge.from, edge.kind
                )
            })?;
        if !dependencies.contains(&edge.to) {
            bail!(
                "WP-08A transitional edge {} -> {} ({}) has no direct dependency evidence",
                edge.from,
                edge.to,
                edge.kind
            );
        }
    }
    Ok(())
}

fn accepted_expired_transitional_edge(edge: &TransitionalEdge) -> bool {
    RETIRED_WORKSPACE_CRATES.contains(&edge.from.as_str())
        || RETIRED_WORKSPACE_CRATES.contains(&edge.to.as_str())
        || (edge.kind == "normal"
            && accepted_successor_normal_dependency_removals(&edge.from)
                .contains(&edge.to.as_str()))
}

fn validate_normal_edge_closure(
    contract: &SubsystemContract,
    metadata: &WorkspaceDependencyMetadata,
    target_graph: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    let transitional = contract
        .transitional_edges
        .iter()
        .filter(|edge| edge.kind == "normal" && !accepted_expired_transitional_edge(edge))
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect::<BTreeSet<_>>();
    let workspace_crates = metadata
        .declared_dependency_kinds_by_name
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    for (source, kinds) in &metadata.declared_dependency_kinds_by_name {
        for dependency in kinds.get("normal").into_iter().flatten() {
            if !workspace_crates.contains(dependency.as_str()) {
                continue;
            }
            let target_permitted = target_graph
                .get(source)
                .is_some_and(|dependencies| dependencies.contains(dependency));
            let explicitly_transitional =
                transitional.contains(&(source.as_str(), dependency.as_str()));
            let accepted_successor_edge = !target_permitted
                && accepted_successor_normal_dependency_additions(source)
                    .contains(&dependency.as_str());
            let owners = usize::from(target_permitted)
                + usize::from(explicitly_transitional)
                + usize::from(accepted_successor_edge);
            match owners {
                1 => {}
                0 => bail!(
                    "live normal edge {source} -> {dependency} is not target-permitted, transitional, or an accepted successor addition"
                ),
                _ => bail!(
                    "live normal edge {source} -> {dependency} has overlapping architecture classifications"
                ),
            }
        }
    }
    Ok(())
}

fn validate_side_effect_capabilities(
    repo_root: &Path,
    contract: &SubsystemContract,
    metadata: &WorkspaceDependencyMetadata,
    crate_paths: &BTreeMap<String, String>,
    target_graph: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    let mut capabilities = BTreeSet::new();
    for capability in &contract.side_effect_capabilities {
        require_nonempty("side-effect capability", &capability.capability)?;
        require_nonempty("side-effect target_owner", &capability.target_owner)?;
        if capability.target_owner != "xtask"
            && !target_graph.contains_key(&capability.target_owner)
        {
            bail!(
                "WP-08A side-effect capability {} names target owner {} outside the frozen target graph",
                capability.capability,
                capability.target_owner
            );
        }
        if !capabilities.insert(&capability.capability) {
            bail!(
                "WP-08A repeats side-effect capability {}",
                capability.capability
            );
        }

        let mut current_crates = BTreeSet::new();
        for owner in &capability.owners {
            if RETIRED_WORKSPACE_CRATES.contains(&owner.crate_name.as_str()) {
                continue;
            }
            if !current_crates.insert(&owner.crate_name) {
                bail!(
                    "WP-08A side-effect capability {} repeats current crate {}",
                    capability.capability,
                    owner.crate_name
                );
            }
            validate_side_effect_evidence(
                repo_root,
                metadata,
                crate_paths,
                &owner.crate_name,
                owner.evidence_dependency.as_deref(),
                owner.evidence_source.as_deref(),
            )?;
        }
        for exception in &capability.current_exceptions {
            require_nonempty("side-effect exception reason", &exception.reason)?;
            validate_expiry_gate(&exception.expiry_gate)?;
            if RETIRED_WORKSPACE_CRATES.contains(&exception.crate_name.as_str()) {
                continue;
            }
            let expired_at_wp08b = capability.capability == "dataset-demand-and-open-workers"
                && exception.crate_name == "mirante4d-data"
                && exception.expiry_gate == "WP-08B";
            let retired_at_wp12 = (capability.capability == "analysis-export-filesystem"
                && exception.crate_name == "mirante4d-analysis")
                || (capability.capability == "analysis-background-workers"
                    && exception.crate_name == "mirante4d-app");
            let expired_at_wp10b = matches!(
                capability.capability.as_str(),
                "project-package-filesystem" | "project-store-background-worker"
            ) && exception.crate_name == "mirante4d-app"
                && exception.expiry_gate == "WP-10B";
            let expired_at_wp10c = capability.capability == "source-import-background-workers"
                && exception.crate_name == "mirante4d-app"
                && exception.expiry_gate == "WP-10C";
            if expired_at_wp08b || retired_at_wp12 || expired_at_wp10b || expired_at_wp10c {
                continue;
            }
            if !current_crates.insert(&exception.crate_name) {
                bail!(
                    "WP-08A side-effect capability {} repeats current crate {}",
                    capability.capability,
                    exception.crate_name
                );
            }
            validate_side_effect_evidence(
                repo_root,
                metadata,
                crate_paths,
                &exception.crate_name,
                exception.evidence_dependency.as_deref(),
                exception.evidence_source.as_deref(),
            )?;
        }
        if capability.capability == "dataset-demand-and-open-workers" {
            current_crates.insert(&capability.target_owner);
            validate_side_effect_evidence(
                repo_root,
                metadata,
                crate_paths,
                &capability.target_owner,
                None,
                Some("thread::Builder::new"),
            )?;
        }
        if matches!(
            capability.capability.as_str(),
            "project-package-filesystem" | "project-store-background-worker"
        ) {
            current_crates.insert(&capability.target_owner);
            validate_wp10b_project_store_side_effect_owner(
                repo_root,
                &capability.capability,
                &capability.target_owner,
            )?;
        }
        if matches!(
            capability.capability.as_str(),
            "dataset-package-filesystem-and-codecs" | "source-import-and-staging-filesystem"
        ) {
            if !metadata
                .declared_dependency_kinds_by_name
                .contains_key(&capability.target_owner)
            {
                bail!(
                    "WP-08A activated side-effect target owner {} is not a workspace crate",
                    capability.target_owner
                );
            }
            current_crates.insert(&capability.target_owner);
        }
        if capability.capability == "source-import-background-workers" {
            current_crates.insert(&capability.target_owner);
            validate_wp10c_import_worker_owner(repo_root, &capability.target_owner)?;
        }
        if current_crates.is_empty()
            && !matches!(
                capability.capability.as_str(),
                "analysis-background-workers" | "analysis-export-filesystem"
            )
        {
            bail!(
                "WP-08A side-effect capability {} has no evidenced current owner or exception",
                capability.capability
            );
        }
    }
    if capabilities.is_empty() {
        bail!("WP-08A side-effect capability ledger must not be empty");
    }
    validate_thread_creation_owners(repo_root, contract, crate_paths)?;
    Ok(())
}

fn validate_wp10b_project_store_side_effect_owner(
    repo_root: &Path,
    capability: &str,
    target_owner: &str,
) -> anyhow::Result<()> {
    if target_owner != "mirante4d-project-store" {
        bail!("WP-10B B4 project side-effect target owner drifted: {target_owner}");
    }
    let (relative_path, marker) = match capability {
        "project-package-filesystem" => (
            "crates/mirante4d-project-store/src/local.rs",
            "fs::OpenOptions::new",
        ),
        "project-store-background-worker" => (
            "crates/mirante4d-project-store/src/actor.rs",
            "thread::Builder::new",
        ),
        _ => bail!("unsupported WP-10B B4 project side-effect capability {capability}"),
    };
    let path = repo_root.join(relative_path);
    let source =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    if !source.contains(marker) {
        bail!("WP-10B B4 target owner {target_owner} lacks {capability} evidence {marker:?}");
    }
    Ok(())
}

fn validate_wp10c_import_worker_owner(repo_root: &Path, target_owner: &str) -> anyhow::Result<()> {
    if target_owner != "mirante4d-import-pipeline" {
        bail!("WP-10C import worker target owner drifted: {target_owner}");
    }

    let worker_path = repo_root.join("crates/mirante4d-import-pipeline/src/worker.rs");
    let worker_source = fs::read_to_string(&worker_path)
        .with_context(|| format!("failed to read {}", worker_path.display()))?;
    if !source_creates_thread(&worker_path, &worker_source)? {
        bail!("WP-10C import pipeline worker owner creates no production thread");
    }
    for marker in ["spawn_tiff_inspection_worker", "spawn_tiff_import_worker"] {
        if !worker_source.contains(marker) {
            bail!("WP-10C import pipeline worker owner lacks {marker}");
        }
    }

    let app_path = repo_root.join("crates/mirante4d-app/src/workbench_import.rs");
    let app_source = fs::read_to_string(&app_path)
        .with_context(|| format!("failed to read {}", app_path.display()))?;
    if source_creates_thread(&app_path, &app_source)? {
        bail!("WP-10C app import module still creates a production thread");
    }
    for marker in ["spawn_tiff_inspection_worker", "spawn_tiff_import_worker"] {
        if !app_source.contains(marker) {
            bail!("WP-10C app import module does not use target worker {marker}");
        }
    }
    Ok(())
}

fn validate_thread_creation_owners(
    repo_root: &Path,
    contract: &SubsystemContract,
    crate_paths: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut declared = contract
        .side_effect_capabilities
        .iter()
        .filter(|capability| capability.capability.contains("worker"))
        .flat_map(|capability| {
            capability
                .owners
                .iter()
                .map(|owner| owner.crate_name.as_str())
                .chain(
                    capability
                        .current_exceptions
                        .iter()
                        .map(|exception| exception.crate_name.as_str()),
                )
        })
        .collect::<BTreeSet<_>>();
    declared.insert("mirante4d-dataset-runtime");

    let mut observed = BTreeSet::new();
    for (crate_name, crate_path) in crate_paths {
        for path in collect_rust_source_files(&repo_root.join(crate_path).join("src"))? {
            if path
                .components()
                .any(|component| component.as_os_str() == "tests")
                || path.file_stem().is_some_and(|stem| {
                    stem == "tests" || stem.to_string_lossy().ends_with("_tests")
                })
            {
                continue;
            }
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if source_creates_thread(&path, &source)? {
                observed.insert(crate_name.as_str());
            }
        }
    }

    let undeclared = observed.difference(&declared).copied().collect::<Vec<_>>();
    if !undeclared.is_empty() {
        bail!("WP-08A production thread creation has no worker-capability owner: {undeclared:?}");
    }
    Ok(())
}

fn source_creates_thread(path: &Path, source: &str) -> anyhow::Result<bool> {
    let file =
        syn::parse_file(source).with_context(|| format!("failed to parse {}", path.display()))?;
    let mut import_visitor = ThreadImportVisitor::default();
    import_visitor.visit_file(&file);
    let mut creation_visitor = ThreadCreationVisitor {
        found: false,
        imports: import_visitor.imports,
    };
    creation_visitor.visit_file(&file);
    Ok(creation_visitor.found)
}

#[derive(Default)]
struct ThreadImports {
    modules: BTreeSet<String>,
    spawn_functions: BTreeSet<String>,
    builder_types: BTreeSet<String>,
    scope_functions: BTreeSet<String>,
}

#[derive(Default)]
struct ThreadImportVisitor {
    imports: ThreadImports,
}

impl<'ast> Visit<'ast> for ThreadImportVisitor {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !is_test_only_module(item) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        collect_thread_imports(&item.tree, &mut Vec::new(), &mut self.imports);
    }
}

fn collect_thread_imports(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    imports: &mut ThreadImports,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_thread_imports(&path.tree, prefix, imports);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let source_name = name.ident.to_string();
            let mut source = prefix.clone();
            let local = if source_name == "self" {
                prefix.last().cloned().unwrap_or(source_name)
            } else {
                source.push(source_name.clone());
                source_name
            };
            register_thread_import(&source, local, imports);
        }
        syn::UseTree::Rename(rename) => {
            let source_name = rename.ident.to_string();
            let mut source = prefix.clone();
            if source_name != "self" {
                source.push(source_name);
            }
            register_thread_import(&source, rename.rename.to_string(), imports);
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_thread_imports(item, prefix, imports);
            }
        }
        syn::UseTree::Glob(_)
            if prefix.len() == 2 && prefix[0] == "std" && prefix[1] == "thread" =>
        {
            imports.spawn_functions.insert("spawn".to_owned());
            imports.builder_types.insert("Builder".to_owned());
            imports.scope_functions.insert("scope".to_owned());
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn register_thread_import(source: &[String], local: String, imports: &mut ThreadImports) {
    let source = source.iter().map(String::as_str).collect::<Vec<_>>();
    match source.as_slice() {
        ["std", "thread"] => {
            imports.modules.insert(local);
        }
        ["std", "thread", "spawn"] => {
            imports.spawn_functions.insert(local);
        }
        ["std", "thread", "Builder"] => {
            imports.builder_types.insert(local);
        }
        ["std", "thread", "scope"] => {
            imports.scope_functions.insert(local);
        }
        _ => {}
    }
}

struct ThreadCreationVisitor {
    found: bool,
    imports: ThreadImports,
}

impl<'ast> Visit<'ast> for ThreadCreationVisitor {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !is_test_only_module(item) {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_expr_call(&mut self, item: &'ast syn::ExprCall) {
        if let syn::Expr::Path(function) = item.func.as_ref() {
            let segments = function
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            self.found |= self.is_thread_spawn(&segments)
                || self.is_thread_builder_constructor(&segments)
                || self.is_thread_scope(&segments);
        }
        syn::visit::visit_expr_call(self, item);
    }
}

impl ThreadCreationVisitor {
    fn is_thread_spawn(&self, segments: &[String]) -> bool {
        matches_path(segments, &["std", "thread", "spawn"])
            || matches_module_item(segments, &self.imports.modules, "spawn")
            || matches_imported_item(segments, &self.imports.spawn_functions)
    }

    fn is_thread_builder_constructor(&self, segments: &[String]) -> bool {
        let direct = segments.len() == 4
            && segments[0] == "std"
            && segments[1] == "thread"
            && segments[2] == "Builder"
            && matches!(segments[3].as_str(), "new" | "default");
        let through_module = segments.len() == 3
            && self.imports.modules.contains(&segments[0])
            && segments[1] == "Builder"
            && matches!(segments[2].as_str(), "new" | "default");
        let imported = segments.len() == 2
            && self.imports.builder_types.contains(&segments[0])
            && matches!(segments[1].as_str(), "new" | "default");
        direct || through_module || imported
    }

    fn is_thread_scope(&self, segments: &[String]) -> bool {
        matches_path(segments, &["std", "thread", "scope"])
            || matches_module_item(segments, &self.imports.modules, "scope")
            || matches_imported_item(segments, &self.imports.scope_functions)
    }
}

fn matches_path(actual: &[String], expected: &[&str]) -> bool {
    actual
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
}

fn matches_module_item(segments: &[String], module_aliases: &BTreeSet<String>, item: &str) -> bool {
    segments.len() == 2 && module_aliases.contains(&segments[0]) && segments[1] == item
}

fn matches_imported_item(segments: &[String], aliases: &BTreeSet<String>) -> bool {
    segments.len() == 1 && aliases.contains(&segments[0])
}

fn is_test_only_module(item: &syn::ItemMod) -> bool {
    item.attrs.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string() == "test")
    })
}

fn validate_side_effect_evidence(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
    crate_paths: &BTreeMap<String, String>,
    crate_name: &str,
    evidence_dependency: Option<&str>,
    evidence_source: Option<&str>,
) -> anyhow::Result<()> {
    require_nonempty("side-effect current crate", crate_name)?;
    if evidence_dependency.is_some() == evidence_source.is_some() {
        bail!(
            "WP-08A side-effect crate {crate_name} must declare exactly one of evidence_dependency or evidence_source"
        );
    }
    if let Some(dependency) = evidence_dependency {
        require_nonempty("side-effect evidence_dependency", dependency)?;
        let normal = metadata
            .declared_dependency_kinds_by_name
            .get(crate_name)
            .and_then(|kinds| kinds.get("normal"))
            .with_context(|| {
                format!("WP-08A side-effect crate {crate_name} has no normal dependencies")
            })?;
        if !normal.contains(dependency) {
            bail!(
                "WP-08A side-effect crate {crate_name} lacks direct normal dependency evidence {dependency}"
            );
        }
    }
    if let Some(literal) = evidence_source {
        require_nonempty("side-effect evidence_source", literal)?;
        let crate_path = crate_paths.get(crate_name).with_context(|| {
            format!("WP-08A side-effect crate {crate_name} is not in the matrix")
        })?;
        let source_root = repo_root.join(crate_path).join("src");
        let found = collect_rust_source_files(&source_root)?
            .into_iter()
            .map(fs::read_to_string)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|source| source.contains(literal));
        if !found {
            bail!(
                "WP-08A side-effect crate {crate_name} has no production source evidence {literal:?}"
            );
        }
    }
    Ok(())
}

fn validate_frozen_public_api(
    repo_root: &Path,
    contract: &SubsystemContract,
    crate_paths: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let forbidden = unique_string_set(
        &contract.public_api_forbidden_identifiers,
        "WP-08A forbidden public identifiers",
    )?;
    if forbidden.is_empty() {
        bail!("WP-08A forbidden public identifier list must not be empty");
    }

    let mut crates = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for root in &contract.frozen_public_roots {
        if !crates.insert(&root.crate_name) {
            bail!(
                "WP-08A repeats frozen public-root crate {}",
                root.crate_name
            );
        }
        if !paths.insert(&root.path) {
            bail!("WP-08A repeats frozen public-root path {}", root.path);
        }
        let crate_path = crate_paths.get(&root.crate_name).with_context(|| {
            format!(
                "WP-08A frozen public-root crate {} is not in the dependency matrix",
                root.crate_name
            )
        })?;
        let expected_prefix = format!("{crate_path}/src/");
        if !root.path.starts_with(&expected_prefix) {
            bail!(
                "WP-08A frozen public root {} is outside crate {}",
                root.path,
                root.crate_name
            );
        }
        let mut expected = unique_string_set(
            &root.items,
            &format!("WP-08A {} public root items", root.crate_name),
        )?;
        for addition in accepted_successor_public_root_additions(&root.crate_name) {
            if !expected.insert((*addition).to_owned()) {
                bail!(
                    "WP-08A {} accepted successor public-root addition duplicates the frozen inventory: {addition}",
                    root.crate_name
                );
            }
        }
        let actual = public_root_api_names(&repo_root.join(&root.path))?;
        if actual != expected {
            bail!(
                "WP-08A {} public root drifted: missing={:?}, extra={:?}",
                root.crate_name,
                expected.difference(&actual).collect::<Vec<_>>(),
                actual.difference(&expected).collect::<Vec<_>>()
            );
        }

        let source_root = repo_root.join(crate_path).join("src");
        let mut violations = Vec::new();
        for path in collect_rust_source_files(&source_root)? {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            violations.extend(public_api_violations(&path, &source, &forbidden)?);
        }
        if !violations.is_empty() {
            bail!(
                "WP-08A {} public API leaks forbidden types:\n{}",
                root.crate_name,
                violations.join("\n")
            );
        }
    }
    if crates.is_empty() {
        bail!("WP-08A frozen public-root list must not be empty");
    }
    Ok(())
}

pub(super) fn public_api_violations(
    path: &Path,
    source: &str,
    forbidden: &BTreeSet<String>,
) -> anyhow::Result<Vec<String>> {
    let file = syn::parse_file(source)
        .with_context(|| format!("failed to parse public API source {}", path.display()))?;
    let mut violations = BTreeSet::new();
    for item in &file.items {
        scan_public_item(path, item, forbidden, &mut violations);
    }
    Ok(violations.into_iter().collect())
}

fn scan_public_item(
    path: &Path,
    item: &syn::Item,
    forbidden: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    if let syn::Item::Mod(module) = item
        && let Some((_, items)) = &module.content
    {
        for item in items {
            scan_public_item(path, item, forbidden, violations);
        }
    }
    match item {
        syn::Item::Const(item) if is_public(&item.vis) => {
            scan_named_signature(path, &item.ident, &item.ty, forbidden, violations);
        }
        syn::Item::Enum(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
            for variant in &item.variants {
                visitor.visit_ident(&variant.ident);
                for field in &variant.fields {
                    visitor.visit_type(&field.ty);
                }
            }
        }
        syn::Item::ExternCrate(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
        }
        syn::Item::Fn(item) if is_public(&item.vis) => {
            scan_signature(path, &item.sig, forbidden, violations);
        }
        syn::Item::Impl(item) => {
            let trait_implementation = item.trait_.is_some();
            if let Some((_, trait_path, _)) = &item.trait_ {
                let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
                visitor.visit_path(trait_path);
                visitor.visit_type(&item.self_ty);
            }
            for implementation_item in &item.items {
                match implementation_item {
                    syn::ImplItem::Const(item) if trait_implementation || is_public(&item.vis) => {
                        scan_named_signature(path, &item.ident, &item.ty, forbidden, violations);
                    }
                    syn::ImplItem::Fn(item) if trait_implementation || is_public(&item.vis) => {
                        scan_signature(path, &item.sig, forbidden, violations);
                    }
                    syn::ImplItem::Type(item) if trait_implementation || is_public(&item.vis) => {
                        scan_named_signature(path, &item.ident, &item.ty, forbidden, violations);
                    }
                    _ => {}
                }
            }
        }
        syn::Item::Mod(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
        }
        syn::Item::Static(item) if is_public(&item.vis) => {
            scan_named_signature(path, &item.ident, &item.ty, forbidden, violations);
        }
        syn::Item::Struct(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
            for field in &item.fields {
                if is_public(&field.vis) {
                    visitor.visit_type(&field.ty);
                }
            }
        }
        syn::Item::Trait(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
            for bound in &item.supertraits {
                visitor.visit_type_param_bound(bound);
            }
            for trait_item in &item.items {
                match trait_item {
                    syn::TraitItem::Const(item) => {
                        visitor.visit_ident(&item.ident);
                        visitor.visit_type(&item.ty);
                    }
                    syn::TraitItem::Fn(item) => visitor.visit_signature(&item.sig),
                    syn::TraitItem::Type(item) => {
                        visitor.visit_ident(&item.ident);
                        visitor.visit_generics(&item.generics);
                        for bound in &item.bounds {
                            visitor.visit_type_param_bound(bound);
                        }
                        if let Some((_, default)) = &item.default {
                            visitor.visit_type(default);
                        }
                    }
                    _ => {}
                }
            }
        }
        syn::Item::TraitAlias(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
            for bound in &item.bounds {
                visitor.visit_type_param_bound(bound);
            }
        }
        syn::Item::Type(item) if is_public(&item.vis) => {
            scan_named_signature(path, &item.ident, &item.ty, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
        }
        syn::Item::Union(item) if is_public(&item.vis) => {
            scan_ident(path, &item.ident, forbidden, violations);
            let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
            visitor.visit_generics(&item.generics);
            for field in &item.fields.named {
                if is_public(&field.vis) {
                    visitor.visit_type(&field.ty);
                }
            }
        }
        syn::Item::Use(item) if is_public(&item.vis) => {
            scan_public_use(path, &item.tree, forbidden, violations);
        }
        _ => {}
    }
}

fn scan_signature(
    path: &Path,
    signature: &syn::Signature,
    forbidden: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
    visitor.visit_signature(signature);
}

fn scan_named_signature(
    path: &Path,
    ident: &syn::Ident,
    value: &syn::Type,
    forbidden: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    scan_ident(path, ident, forbidden, violations);
    let mut visitor = PublicTypeVisitor::new(path, forbidden, violations);
    visitor.visit_type(value);
}

fn scan_ident(
    path: &Path,
    ident: &syn::Ident,
    forbidden: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    let ident = ident.to_string();
    if matches_forbidden_identifier(&ident, forbidden) {
        violations.insert(format!(
            "{} exposes forbidden identifier {ident}",
            path.display()
        ));
    }
}

fn scan_public_use(
    path: &Path,
    tree: &syn::UseTree,
    forbidden: &BTreeSet<String>,
    violations: &mut BTreeSet<String>,
) {
    match tree {
        syn::UseTree::Path(item) => {
            scan_ident(path, &item.ident, forbidden, violations);
            scan_public_use(path, &item.tree, forbidden, violations);
        }
        syn::UseTree::Name(item) => scan_ident(path, &item.ident, forbidden, violations),
        syn::UseTree::Rename(item) => {
            scan_ident(path, &item.ident, forbidden, violations);
            scan_ident(path, &item.rename, forbidden, violations);
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                scan_public_use(path, item, forbidden, violations);
            }
        }
        syn::UseTree::Glob(_) => {
            violations.insert(format!(
                "{} has an unverifiable public glob re-export",
                path.display()
            ));
        }
    }
}

struct PublicTypeVisitor<'a, 'b> {
    path: &'a Path,
    forbidden: &'a BTreeSet<String>,
    violations: &'b mut BTreeSet<String>,
}

impl<'a, 'b> PublicTypeVisitor<'a, 'b> {
    fn new(
        path: &'a Path,
        forbidden: &'a BTreeSet<String>,
        violations: &'b mut BTreeSet<String>,
    ) -> Self {
        Self {
            path,
            forbidden,
            violations,
        }
    }
}

impl<'ast> Visit<'ast> for PublicTypeVisitor<'_, '_> {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        let ident = ident.to_string();
        if matches_forbidden_identifier(&ident, self.forbidden) {
            self.violations.insert(format!(
                "{} exposes forbidden identifier {ident}",
                self.path.display()
            ));
        }
    }

    fn visit_type_path(&mut self, item: &'ast syn::TypePath) {
        if owning_pixel_vec_scalar(item).is_some() {
            self.violations.insert(format!(
                "{} exposes owning pixel {}",
                self.path.display(),
                owning_pixel_vec_scalar(item).expect("checked as present")
            ));
        }
        syn::visit::visit_type_path(self, item);
    }
}

fn owning_pixel_vec_scalar(item: &syn::TypePath) -> Option<String> {
    let segment = item.path.segments.last()?;
    if segment.ident != "Vec" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let scalar = arguments.args.first()?;
    let syn::GenericArgument::Type(syn::Type::Path(scalar)) = scalar else {
        return None;
    };
    let scalar = scalar.path.segments.last()?.ident.to_string();
    ["u8", "u16", "f32"]
        .contains(&scalar.as_str())
        .then_some(format!("Vec<{scalar}>"))
}

fn matches_forbidden_identifier(identifier: &str, forbidden: &BTreeSet<String>) -> bool {
    forbidden.iter().any(|pattern| {
        pattern
            .strip_suffix('*')
            .map_or(identifier == pattern, |prefix| {
                identifier.starts_with(prefix)
            })
    })
}

fn validate_resource_allocations(
    contract: &SubsystemContract,
    target_graph: &BTreeMap<String, BTreeSet<String>>,
) -> anyhow::Result<()> {
    if contract.ledger_authorities.cpu.owner != CPU_LEDGER_OWNER
        || contract.ledger_authorities.gpu.owner != GPU_LEDGER_OWNER
        || contract.ledger_authorities.cpu.contract_crate != CPU_LEDGER_CONTRACT_CRATE
        || contract.ledger_authorities.gpu.contract_crate != GPU_LEDGER_CONTRACT_CRATE
    {
        bail!(
            "WP-08A ledger authorities must be CPU={CPU_LEDGER_OWNER} through {CPU_LEDGER_CONTRACT_CRATE} and GPU={GPU_LEDGER_OWNER} through {GPU_LEDGER_CONTRACT_CRATE}"
        );
    }
    for (kind, authority) in [
        ("CPU", &contract.ledger_authorities.cpu),
        ("GPU", &contract.ledger_authorities.gpu),
    ] {
        if !target_graph.contains_key(&authority.owner)
            || !target_graph.contains_key(&authority.contract_crate)
        {
            bail!("WP-08A {kind} ledger names a crate outside the target graph");
        }
        if !target_dependency_reaches(target_graph, &authority.owner, &authority.contract_crate) {
            bail!(
                "WP-08A {kind} ledger owner {} cannot reach contract crate {}",
                authority.owner,
                authority.contract_crate
            );
        }
    }
    let cpu_categories = unique_string_set(
        &contract.ledger_authorities.cpu.categories,
        "WP-08A CPU ledger categories",
    )?;
    let gpu_categories = unique_string_set(
        &contract.ledger_authorities.gpu.categories,
        "WP-08A GPU ledger categories",
    )?;
    if cpu_categories.is_empty() || gpu_categories.is_empty() {
        bail!("WP-08A CPU and GPU ledger category sets must both be nonempty");
    }
    if !cpu_categories.is_disjoint(&gpu_categories) {
        bail!("WP-08A CPU and GPU ledger categories must be disjoint");
    }
    let all_categories = cpu_categories
        .union(&gpu_categories)
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut classes = BTreeSet::new();
    for allocation in &contract.resource_allocations {
        require_nonempty("resource-allocation class", &allocation.class)?;
        require_nonempty("resource-allocation owner", &allocation.owner)?;
        if !classes.insert(&allocation.class) {
            bail!(
                "WP-08A repeats resource-allocation class {}",
                allocation.class
            );
        }
        if !all_categories.contains(&allocation.ledger_category) {
            bail!(
                "WP-08A resource-allocation class {} uses unknown ledger category {}",
                allocation.class,
                allocation.ledger_category
            );
        }
        if !target_graph.contains_key(&allocation.owner) {
            bail!(
                "WP-08A resource-allocation class {} has owner {} outside the target graph",
                allocation.class,
                allocation.owner
            );
        }
        let authority = if cpu_categories.contains(&allocation.ledger_category) {
            &contract.ledger_authorities.cpu
        } else {
            &contract.ledger_authorities.gpu
        };
        if allocation.owner != authority.owner
            && !target_dependency_reaches(
                target_graph,
                &allocation.owner,
                &authority.contract_crate,
            )
        {
            bail!(
                "WP-08A resource-allocation owner {} cannot reach {} ledger contract crate {}",
                allocation.owner,
                if cpu_categories.contains(&allocation.ledger_category) {
                    "CPU"
                } else {
                    "GPU"
                },
                authority.contract_crate
            );
        }
    }
    if classes.is_empty() {
        bail!("WP-08A resource-allocation ledger must not be empty");
    }
    Ok(())
}

fn target_dependency_reaches(
    graph: &BTreeMap<String, BTreeSet<String>>,
    from: &str,
    target: &str,
) -> bool {
    let mut pending = vec![from];
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        if !visited.insert(current) {
            continue;
        }
        for dependency in graph.get(current).into_iter().flatten() {
            if dependency == target {
                return true;
            }
            pending.push(dependency);
        }
    }
    false
}

fn validate_restricted_trait_implementations(
    repo_root: &Path,
    contract: &SubsystemContract,
    crate_paths: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let mut restrictions = BTreeMap::new();
    for restriction in &contract.restricted_trait_implementations {
        require_nonempty("restricted trait name", &restriction.trait_name)?;
        require_nonempty("restricted trait owner", &restriction.owner)?;
        if !crate_paths.contains_key(&restriction.owner) {
            bail!(
                "WP-08A restricted trait {} names unknown owner {}",
                restriction.trait_name,
                restriction.owner
            );
        }
        if restrictions
            .insert(restriction.trait_name.clone(), restriction)
            .is_some()
        {
            bail!("WP-08A repeats restricted trait {}", restriction.trait_name);
        }
    }
    if restrictions.is_empty() {
        bail!("WP-08A restricted-trait implementation ledger must not be empty");
    }

    let restricted_names = restrictions.keys().cloned().collect::<BTreeSet<_>>();
    let mut observed = BTreeMap::<String, BTreeSet<String>>::new();
    for (crate_name, crate_path) in crate_paths {
        for path in collect_rust_source_files(&repo_root.join(crate_path).join("src"))? {
            if path
                .components()
                .any(|component| component.as_os_str() == "tests")
                || path.file_stem().is_some_and(|stem| {
                    stem == "tests" || stem.to_string_lossy().ends_with("_tests")
                })
            {
                continue;
            }
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let file = syn::parse_file(&source)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            let mut visitor = RestrictedTraitVisitor {
                restricted_names: &restricted_names,
                observed: BTreeSet::new(),
            };
            visitor.visit_file(&file);
            for trait_name in visitor.observed {
                observed
                    .entry(trait_name)
                    .or_default()
                    .insert(crate_name.clone());
            }
        }
    }

    for (trait_name, restriction) in restrictions {
        let implementors = observed.get(&trait_name).cloned().unwrap_or_default();
        let unauthorized = implementors
            .iter()
            .filter(|crate_name| *crate_name != &restriction.owner)
            .collect::<Vec<_>>();
        if !unauthorized.is_empty() {
            bail!(
                "WP-08A trait {trait_name} has implementations outside sole owner {}: {unauthorized:?}",
                restriction.owner
            );
        }
        if restriction.required && !implementors.contains(&restriction.owner) {
            bail!(
                "WP-08A trait {trait_name} requires an implementation in sole owner {}",
                restriction.owner
            );
        }
    }
    Ok(())
}

struct RestrictedTraitVisitor<'a> {
    restricted_names: &'a BTreeSet<String>,
    observed: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for RestrictedTraitVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let test_only = item.attrs.iter().any(|attribute| {
            attribute.path().is_ident("cfg")
                && attribute
                    .meta
                    .require_list()
                    .is_ok_and(|list| list.tokens.to_string() == "test")
        });
        if !test_only {
            syn::visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        if let Some((_, trait_path, _)) = &item.trait_
            && let Some(name) = trait_path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
            && self.restricted_names.contains(&name)
        {
            self.observed.insert(name);
        }
        syn::visit::visit_item_impl(self, item);
    }
}

fn validate_frozen_source_rules(
    repo_root: &Path,
    contract: &SubsystemContract,
) -> anyhow::Result<()> {
    let mut rule_paths = BTreeSet::new();
    let mut violations = Vec::new();
    for rule in &contract.frozen_source_rules {
        require_nonempty("frozen-source path", &rule.path)?;
        if !rule_paths.insert(&rule.path) {
            bail!("WP-08A repeats frozen-source path {}", rule.path);
        }
        let forbidden = unique_string_set(
            &rule.forbidden_imports,
            &format!("WP-08A {} forbidden imports", rule.path),
        )?;
        if forbidden.is_empty() {
            bail!("WP-08A frozen-source rule {} is empty", rule.path);
        }
        let path = repo_root.join(&rule.path);
        let files = if path.is_file() {
            vec![path]
        } else if path.is_dir() {
            collect_rust_source_files(&path)?
        } else {
            bail!("WP-08A frozen-source path {} does not exist", rule.path);
        };
        for path in files {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            violations.extend(forbidden_import_violations(&path, &source, &forbidden)?);
        }
    }
    if rule_paths.is_empty() {
        bail!("WP-08A frozen-source rule list must not be empty");
    }
    if !violations.is_empty() {
        bail!(
            "WP-08A frozen source imports drifted:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

pub(super) fn forbidden_import_violations(
    path: &Path,
    source: &str,
    forbidden: &BTreeSet<String>,
) -> anyhow::Result<Vec<String>> {
    let file = syn::parse_file(source)
        .with_context(|| format!("failed to parse frozen source {}", path.display()))?;
    let mut visitor = ForbiddenImportVisitor {
        path,
        forbidden: forbidden
            .iter()
            .map(|name| {
                name.replace('-', "_")
                    .split("::")
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect(),
        violations: BTreeSet::new(),
    };
    visitor.visit_file(&file);
    Ok(visitor.violations.into_iter().collect())
}

struct ForbiddenImportVisitor<'a> {
    path: &'a Path,
    forbidden: Vec<Vec<String>>,
    violations: BTreeSet<String>,
}

impl ForbiddenImportVisitor<'_> {
    fn inspect_segments(&mut self, segments: &[String]) {
        for forbidden in &self.forbidden {
            if segments.starts_with(forbidden) {
                self.violations.insert(format!(
                    "{} imports/references forbidden authority {}",
                    self.path.display(),
                    forbidden.join("::")
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for ForbiddenImportVisitor<'_> {
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut paths = Vec::new();
        flatten_use_tree(&item.tree, &mut Vec::new(), &mut paths);
        for path in paths {
            self.inspect_segments(&path);
        }
        syn::visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        self.inspect_segments(&[item.ident.to_string()]);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segments = path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        self.inspect_segments(&segments);
        syn::visit::visit_path(self, path);
    }
}

fn validate_expiry_gate(expiry_gate: &str) -> anyhow::Result<()> {
    let suffix = expiry_gate
        .strip_prefix("WP-")
        .context("WP-08A expiry gate must start with WP-")?;
    let bytes = suffix.as_bytes();
    let valid = matches!(bytes.len(), 2 | 3)
        && bytes[..2].iter().all(u8::is_ascii_digit)
        && (bytes.len() == 2 || bytes[2].is_ascii_uppercase());
    if !valid {
        bail!("WP-08A has invalid expiry gate {expiry_gate:?}");
    }
    Ok(())
}

fn unique_string_set(values: &[String], context: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for value in values {
        require_nonempty(context, value)?;
        if !set.insert(value.clone()) {
            bail!("{context} repeats {value:?}");
        }
    }
    Ok(set)
}

fn require_nonempty(context: &str, value: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{context} must not be empty");
    }
    Ok(())
}

fn is_public(visibility: &syn::Visibility) -> bool {
    matches!(visibility, syn::Visibility::Public(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestDependencyKinds<'a> = &'a [(&'a str, &'a [&'a str])];

    fn metadata(dependencies: &[(&str, TestDependencyKinds<'_>)]) -> WorkspaceDependencyMetadata {
        let declared_dependency_kinds_by_name = dependencies
            .iter()
            .map(|(name, kinds)| {
                (
                    (*name).to_owned(),
                    kinds
                        .iter()
                        .map(|(kind, dependencies)| {
                            (
                                (*kind).to_owned(),
                                dependencies
                                    .iter()
                                    .map(|dependency| (*dependency).to_owned())
                                    .collect(),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        WorkspaceDependencyMetadata {
            declared_dependency_kinds_by_name,
            workspace_package_ids_by_name: BTreeMap::new(),
            custom_build_package_ids: BTreeSet::new(),
        }
    }

    fn dependency_entry(
        name: &str,
        normal: &[&str],
        dev: &[&str],
        build: &[&str],
    ) -> DependencyContract {
        DependencyContract {
            name: name.to_owned(),
            path: format!("crates/{name}"),
            dependencies: DependencyKinds {
                normal: normal.iter().map(|value| (*value).to_owned()).collect(),
                dev: dev.iter().map(|value| (*value).to_owned()).collect(),
                build: build.iter().map(|value| (*value).to_owned()).collect(),
            },
        }
    }

    fn contract() -> SubsystemContract {
        SubsystemContract {
            schema: CONTRACT_SCHEMA.to_owned(),
            schema_version: CONTRACT_SCHEMA_VERSION,
            status: CONTRACT_STATUS.to_owned(),
            entry_sha256: ENTRY_SHA256.to_owned(),
            entry_clarification_sha256: ENTRY_CLARIFICATION_SHA256.to_owned(),
            corrective_entry_sha256: CORRECTIVE_ENTRY_SHA256.to_owned(),
            workspace_dependency_matrix: Vec::new(),
            target_dependency_matrix: target_entries(),
            transitional_edges: Vec::new(),
            side_effect_capabilities: Vec::new(),
            frozen_public_roots: Vec::new(),
            public_api_forbidden_identifiers: vec!["wgpu".to_owned()],
            frozen_source_rules: Vec::new(),
            resource_allocations: vec![ResourceAllocation {
                class: "decoded_payload".to_owned(),
                owner: CPU_LEDGER_OWNER.to_owned(),
                ledger_category: "decoded".to_owned(),
            }],
            ledger_authorities: LedgerAuthorities {
                cpu: LedgerAuthority {
                    owner: CPU_LEDGER_OWNER.to_owned(),
                    contract_crate: CPU_LEDGER_CONTRACT_CRATE.to_owned(),
                    categories: vec!["decoded".to_owned()],
                },
                gpu: LedgerAuthority {
                    owner: GPU_LEDGER_OWNER.to_owned(),
                    contract_crate: GPU_LEDGER_CONTRACT_CRATE.to_owned(),
                    categories: vec!["resident".to_owned()],
                },
            },
            restricted_trait_implementations: vec![RestrictedTraitImplementation {
                trait_name: "ResourceLease".to_owned(),
                owner: CPU_LEDGER_OWNER.to_owned(),
                required: true,
            }],
        }
    }

    fn target_entries() -> Vec<TargetDependencyContract> {
        TARGET_CRATES
            .iter()
            .map(|name| TargetDependencyContract {
                name: (*name).to_owned(),
                normal: match *name {
                    CPU_LEDGER_OWNER => vec![CPU_LEDGER_CONTRACT_CRATE.to_owned()],
                    GPU_LEDGER_OWNER => vec![GPU_LEDGER_CONTRACT_CRATE.to_owned()],
                    _ => Vec::new(),
                },
            })
            .collect()
    }

    #[test]
    fn corrective_entry_hash_is_required_and_exact() {
        let mut missing: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../architecture/wp08a-subsystem-contract.json"
        ))
        .unwrap();
        missing
            .as_object_mut()
            .unwrap()
            .remove("corrective_entry_sha256");
        assert!(
            serde_json::from_value::<SubsystemContract>(missing)
                .unwrap_err()
                .to_string()
                .contains("corrective_entry_sha256")
        );

        let mut wrong = contract();
        wrong.corrective_entry_sha256 = "0".repeat(64);
        assert!(
            validate_header(&wrong)
                .unwrap_err()
                .to_string()
                .contains("corrective entry")
        );
    }

    #[test]
    fn dependency_matrix_rejects_missing_extra_and_wrong_kind() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["one", "two"] {
            let crate_root = temp.path().join("crates").join(name);
            fs::create_dir_all(&crate_root).unwrap();
            fs::write(
                crate_root.join("Cargo.toml"),
                format!("[package]\nname = {name:?}\nversion = \"0.0.0\"\n"),
            )
            .unwrap();
        }
        let metadata = metadata(&[
            ("one", &[("normal", &["two"])]),
            ("two", &[("dev", &["one"])]),
        ]);

        let mut missing = contract();
        missing.workspace_dependency_matrix = vec![dependency_entry("one", &["two"], &[], &[])];
        assert!(
            validate_dependency_matrix(temp.path(), &missing, &metadata)
                .unwrap_err()
                .to_string()
                .contains("cover the workspace exactly")
        );

        let mut extra = contract();
        extra.workspace_dependency_matrix = vec![
            dependency_entry("one", &["two"], &[], &[]),
            dependency_entry("two", &[], &["one"], &[]),
            dependency_entry("three", &[], &[], &[]),
        ];
        assert!(
            validate_dependency_matrix(temp.path(), &extra, &metadata)
                .unwrap_err()
                .to_string()
                .contains("cover the workspace exactly")
        );

        let mut wrong_kind = contract();
        wrong_kind.workspace_dependency_matrix = vec![
            dependency_entry("one", &[], &["two"], &[]),
            dependency_entry("two", &[], &["one"], &[]),
        ];
        assert!(
            validate_dependency_matrix(temp.path(), &wrong_kind, &metadata)
                .unwrap_err()
                .to_string()
                .contains("normal dependencies drifted")
        );
    }

    #[test]
    fn dependency_matrix_delegates_only_accepted_successors() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["one", "two"] {
            let crate_root = temp.path().join("crates").join(name);
            fs::create_dir_all(&crate_root).unwrap();
            fs::write(
                crate_root.join("Cargo.toml"),
                format!("[package]\nname = {name:?}\nversion = \"0.0.0\"\n"),
            )
            .unwrap();
        }
        let mut predecessor = contract();
        predecessor.workspace_dependency_matrix = vec![
            dependency_entry("one", &["two"], &[], &[]),
            dependency_entry("two", &[], &[], &[]),
        ];

        let delegated = metadata(&[
            ("one", &[("normal", &["two"])]),
            ("two", &[]),
            (
                "mirante4d-storage",
                &[(
                    "normal",
                    &[
                        "mirante4d-dataset",
                        "mirante4d-domain",
                        "mirante4d-identity",
                    ],
                )],
            ),
            ("mirante4d-render-reference", &[]),
            ("mirante4d-render-wgpu", &[]),
        ]);
        validate_dependency_matrix(temp.path(), &predecessor, &delegated).unwrap();

        let unknown = metadata(&[
            ("one", &[("normal", &["two"])]),
            ("two", &[]),
            ("mirante4d-storage", &[]),
            ("mirante4d-render-reference", &[]),
            ("mirante4d-render-wgpu", &[]),
            ("unreviewed-successor", &[]),
        ]);
        assert!(
            validate_dependency_matrix(temp.path(), &predecessor, &unknown)
                .unwrap_err()
                .to_string()
                .contains("cover the workspace exactly")
        );
    }

    #[test]
    fn target_dependency_matrix_rejects_missing_unknown_and_cycles() {
        let mut missing = contract();
        missing.target_dependency_matrix.pop();
        assert!(
            validate_target_dependency_matrix(&missing)
                .unwrap_err()
                .to_string()
                .contains("wrong crate set")
        );

        let mut unknown = contract();
        unknown.target_dependency_matrix[0]
            .normal
            .push("mirante4d-unknown".to_owned());
        assert!(
            validate_target_dependency_matrix(&unknown)
                .unwrap_err()
                .to_string()
                .contains("unknown dependencies")
        );

        let mut cycle = contract();
        cycle
            .target_dependency_matrix
            .iter_mut()
            .find(|entry| entry.name == "mirante4d-domain")
            .unwrap()
            .normal
            .push("mirante4d-identity".to_owned());
        cycle
            .target_dependency_matrix
            .iter_mut()
            .find(|entry| entry.name == "mirante4d-identity")
            .unwrap()
            .normal
            .push("mirante4d-domain".to_owned());
        assert!(
            validate_target_dependency_matrix(&cycle)
                .unwrap_err()
                .to_string()
                .contains("contains a cycle")
        );
    }

    #[test]
    fn normal_workspace_edge_requires_target_or_transitional_authority() {
        let contract = contract();
        let metadata = metadata(&[("one", &[("normal", &["two"])]), ("two", &[])]);
        assert!(
            validate_normal_edge_closure(&contract, &metadata, &BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("not target-permitted, transitional, or an accepted successor addition")
        );
    }

    #[test]
    fn public_api_rejects_forbidden_types_and_owning_pixel_vectors() {
        let forbidden = BTreeSet::from(["NativeManifest".to_owned(), "VolumeBrick*".to_owned()]);
        let violations = public_api_violations(
            Path::new("src/lib.rs"),
            r#"
pub use storage::NativeManifest;
pub fn pixels() -> Vec<u16> { unreachable!() }
pub trait Source { fn read(&self, key: VolumeBrickKey) -> Vec<f32>; }
fn private(_: NativeManifest) -> Vec<u8> { unreachable!() }
"#,
            &forbidden,
        )
        .unwrap();
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("NativeManifest"))
        );
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("VolumeBrickKey"))
        );
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("Vec<u16>"))
        );
        assert!(
            violations
                .iter()
                .any(|violation| violation.contains("Vec<f32>"))
        );
        assert!(
            !violations
                .iter()
                .any(|violation| violation.contains("Vec<u8>"))
        );
    }

    #[test]
    fn frozen_public_api_rejects_root_name_drift() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("crates/example/src");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(source_root.join("lib.rs"), "pub struct Unexpected;").unwrap();

        let mut contract = contract();
        contract.frozen_public_roots = vec![FrozenPublicRoot {
            crate_name: "example".to_owned(),
            path: "crates/example/src/lib.rs".to_owned(),
            items: vec!["Expected".to_owned()],
        }];
        let crate_paths = BTreeMap::from([("example".to_owned(), "crates/example".to_owned())]);
        assert!(
            validate_frozen_public_api(temp.path(), &contract, &crate_paths)
                .unwrap_err()
                .to_string()
                .contains("public root drifted")
        );
    }

    #[test]
    fn duplicate_transitional_edge_and_resource_class_are_rejected() {
        let mut contract = contract();
        contract.transitional_edges = vec![
            TransitionalEdge {
                from: "one".to_owned(),
                to: "two".to_owned(),
                kind: "normal".to_owned(),
                reason: "bridge".to_owned(),
                expiry_gate: "WP-10C".to_owned(),
            },
            TransitionalEdge {
                from: "one".to_owned(),
                to: "two".to_owned(),
                kind: "normal".to_owned(),
                reason: "duplicate".to_owned(),
                expiry_gate: "WP-10C".to_owned(),
            },
        ];
        let metadata = metadata(&[("one", &[("normal", &["two"])]), ("two", &[])]);
        assert!(
            validate_transitional_edges(&contract, &metadata)
                .unwrap_err()
                .to_string()
                .contains("repeats transitional edge")
        );

        contract.resource_allocations.push(ResourceAllocation {
            class: "decoded_payload".to_owned(),
            owner: "other".to_owned(),
            ledger_category: "resident".to_owned(),
        });
        let target_graph = validate_target_dependency_matrix(&contract).unwrap();
        assert!(
            validate_resource_allocations(&contract, &target_graph)
                .unwrap_err()
                .to_string()
                .contains("repeats resource-allocation class")
        );
    }

    #[test]
    fn side_effect_exception_requires_a_valid_expiry() {
        let mut contract = contract();
        contract.side_effect_capabilities = vec![SideEffectCapability {
            capability: "filesystem".to_owned(),
            target_owner: "mirante4d-storage".to_owned(),
            owners: Vec::new(),
            current_exceptions: vec![SideEffectException {
                crate_name: "one".to_owned(),
                reason: "current storage".to_owned(),
                expiry_gate: "later".to_owned(),
                evidence_dependency: None,
                evidence_source: Some("std::fs".to_owned()),
            }],
        }];
        let metadata = metadata(&[("one", &[])]);
        let crate_paths = BTreeMap::from([("one".to_owned(), "crates/one".to_owned())]);
        let target_graph = validate_target_dependency_matrix(&contract).unwrap();
        assert!(
            validate_side_effect_capabilities(
                Path::new("."),
                &contract,
                &metadata,
                &crate_paths,
                &target_graph,
            )
            .unwrap_err()
            .to_string()
            .contains("expiry gate")
        );
    }

    #[test]
    fn production_thread_creation_requires_a_worker_capability_owner() {
        let temp = tempfile::tempdir().unwrap();
        let source_root = temp.path().join("crates/rogue/src");
        fs::create_dir_all(&source_root).unwrap();
        fs::write(
            source_root.join("lib.rs"),
            "pub fn start() { let _ = std::thread::spawn(|| {}); }",
        )
        .unwrap();

        let contract = contract();
        let crate_paths = BTreeMap::from([("rogue".to_owned(), "crates/rogue".to_owned())]);
        assert!(
            validate_thread_creation_owners(temp.path(), &contract, &crate_paths)
                .unwrap_err()
                .to_string()
                .contains("no worker-capability owner")
        );
    }

    #[test]
    fn wp10c_import_workers_are_created_only_by_the_pipeline() {
        let temp = tempfile::tempdir().unwrap();
        let pipeline_root = temp.path().join("crates/mirante4d-import-pipeline/src");
        let app_root = temp.path().join("crates/mirante4d-app/src");
        fs::create_dir_all(&pipeline_root).unwrap();
        fs::create_dir_all(&app_root).unwrap();
        fs::write(
            pipeline_root.join("worker.rs"),
            r#"
pub fn spawn_tiff_inspection_worker() { let _ = std::thread::spawn(|| {}); }
pub fn spawn_tiff_import_worker() { let _ = std::thread::spawn(|| {}); }
"#,
        )
        .unwrap();
        fs::write(
            app_root.join("workbench_import.rs"),
            "fn start() { spawn_tiff_inspection_worker(); spawn_tiff_import_worker(); }",
        )
        .unwrap();

        validate_wp10c_import_worker_owner(temp.path(), "mirante4d-import-pipeline").unwrap();

        fs::write(
            app_root.join("workbench_import.rs"),
            "fn start() { spawn_tiff_inspection_worker(); spawn_tiff_import_worker(); let _ = std::thread::spawn(|| {}); }",
        )
        .unwrap();
        assert!(
            validate_wp10c_import_worker_owner(temp.path(), "mirante4d-import-pipeline")
                .unwrap_err()
                .to_string()
                .contains("app import module still creates")
        );
    }

    #[test]
    fn thread_creation_detection_closes_import_alias_and_scope_bypasses() {
        let creating_threads = [
            (
                "direct spawn",
                "fn start() { let _ = std::thread::spawn(|| {}); }",
            ),
            (
                "imported spawn",
                "use std::thread::spawn; fn start() { let _ = spawn(|| {}); }",
            ),
            (
                "renamed spawn",
                "use std::thread::spawn as launch; fn start() { let _ = launch(|| {}); }",
            ),
            (
                "aliased thread module",
                "use std::thread as workers; fn start() { let _ = workers::spawn(|| {}); }",
            ),
            (
                "direct builder",
                "fn start() { let _ = std::thread::Builder::new().spawn(|| {}); }",
            ),
            (
                "imported builder",
                "use std::thread::Builder; fn start() { let _ = Builder::new().spawn(|| {}); }",
            ),
            (
                "renamed builder",
                "use std::thread::Builder as ThreadBuilder; fn start() { let _ = ThreadBuilder::default().spawn(|| {}); }",
            ),
            (
                "direct scoped worker",
                "fn start() { std::thread::scope(|scope| { scope.spawn(|| {}); }); }",
            ),
            (
                "renamed scoped worker",
                "use std::thread::scope as with_threads; fn start() { with_threads(|scope| { scope.spawn(|| {}); }); }",
            ),
        ];
        for (case, source) in creating_threads {
            assert!(
                source_creates_thread(Path::new("src/lib.rs"), source).unwrap(),
                "{case} bypassed worker ownership detection"
            );
        }

        let arbitrary_spawn = r#"
struct Executor;
impl Executor { fn spawn(&self, _: impl FnOnce()) {} }
fn run(executor: &Executor) { executor.spawn(|| {}); }
"#;
        assert!(
            !source_creates_thread(Path::new("src/lib.rs"), arbitrary_spawn).unwrap(),
            "an arbitrary .spawn method must not imply OS thread creation"
        );
    }
}
