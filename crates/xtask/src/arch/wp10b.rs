use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use serde_json::Value;

use super::{
    WorkspaceDependencyMetadata, collect_rust_source_files, public_root_api_names, sha256_file,
    workspace_dependency_metadata, wp08a::forbidden_import_violations,
};

const CONTRACT_PATH: &str = "architecture/wp10b-project-store-contract.json";
const CONTRACT_SCHEMA: &str = "mirante4d-wp10b-project-store-contract";
const ENTRY_PATH: &str = "architecture/wp10b-project-store-entry.json";
const ENTRY_SHA256: &str = "b5a189658d0f0d40f7ca0023bd3a688b3ca7926d28e028bb19acdc764920c793";
const DEPENDENCY_CORRECTION_PATH: &str = "architecture/wp10b-project-store-entry-correction.json";
const DEPENDENCY_CORRECTION_SHA256: &str =
    "e9fdbca7e78bdb34eca3533e28c5caa811c6cc0d8e71d8e40324a8b896eefa79";
const QUATERNION_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-quaternion-correction.json";
const QUATERNION_CORRECTION_SHA256: &str =
    "f2ec1d537e90536e8e67212d663e49823edac5eee30d0d45fccd0fb8e5d6876f";
const RECOVERY_AHEAD_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-recovery-ahead-correction.json";
const RECOVERY_AHEAD_CORRECTION_SHA256: &str =
    "dcc908615da8ba94c937fa4ae6745651734caf56f7583ef2638f9e9cc90a6aa1";
const RECOVERY_API_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-recovery-api-correction.json";
const RECOVERY_API_CORRECTION_SHA256: &str =
    "48957fd7c36a34cd916f659cd3becaf8b54c497ab55ad2ed120af8ac2e384e73";
const MAINTENANCE_TRANSITION_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-maintenance-transition-correction.json";
const MAINTENANCE_TRANSITION_CORRECTION_SHA256: &str =
    "778ac85e7c40c1327f6ce6f1854cf7f2cc3a1d631df2da007d0906d819eafac1";
const TRASH_SAFETY_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-trash-safety-correction.json";
const TRASH_SAFETY_CORRECTION_SHA256: &str =
    "1eb60a85cdaf13f458826dace05ba548559b4fe30bd70616fe97af21f2ed7ee5";
const PURGE_SAFETY_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-purge-safety-correction.json";
const PURGE_SAFETY_CORRECTION_SHA256: &str =
    "7e7a0e47ae085c2684b9ae9c465eb5568e3555882c7540ef581cce594fff0278";
const PROVISIONAL_AUTOSAVE_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-provisional-autosave-correction.json";
const PROVISIONAL_AUTOSAVE_CORRECTION_SHA256: &str =
    "2eb9df2cd56472dba37ed9d321b680b2a2a76274210dd7e57aca10be31466e9a";
const STAGING_CLEANUP_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-staging-cleanup-correction.json";
const STAGING_CLEANUP_CORRECTION_SHA256: &str =
    "890f405df832e895da229ba26515efb734fcbe0ccc098f61c7674798a4222324";
const PUBLIC_ACTOR_LIFECYCLE_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-public-actor-lifecycle-correction.json";
const PUBLIC_ACTOR_LIFECYCLE_CORRECTION_SHA256: &str =
    "ac1a7804655af4dab8110f7a584a7bf990ac25a9b03799da9d03638e3d787bbc";
const DURABILITY_QUALIFICATION_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-durability-qualification-correction.json";
const DURABILITY_QUALIFICATION_CORRECTION_SHA256: &str =
    "6b4bbc23115115e36a449209810764cbce2738ca693a985275f5e6513d4252e6";
const B3_INTEGRATION_CORRECTION_PATH: &str =
    "architecture/wp10b-project-store-b3-integration-correction.json";
const B3_INTEGRATION_CORRECTION_SHA256: &str =
    "227b3ff385fdf8d7021758b8013ae6ce98f8fb0d742aadeb2fb6e16e4d5199eb";
const B4_ACTIVATION_PATH: &str = "architecture/wp10b-project-store-b4-activation.json";
const B4_ACTIVATION_SHA256: &str =
    "08f2a5116e063a5e98978bd7a1433732882ecdc869ff3d1949b3a4fdbefddf78";
const B4_SCOPE_CORRECTION_PATH: &str = "architecture/wp10b-project-store-b4-scope-correction.json";
const B4_SCOPE_CORRECTION_SHA256: &str =
    "50dd4a82f3a4ea88c306bd9930ce90323a0303f26b767ab50078cd51f0ae0df4";
const B4_PREDECESSOR_COMMIT: &str = "8fdd94dc9c60406e8de8a96749d7148d38b1dc7a";
const B4_PREDECESSOR_TREE: &str = "1b97468c39bb529285d2727c1021057edb38ff82";
const PROTECTED_MAIN_COMMIT: &str = "b6e0267802f8ac2d0d49a0f04302fd321ef2f617";
const PROTECTED_MAIN_TREE: &str = "b20b598603b47fdbe7c85c3b6d1cba8c78fd433e";
const PROTECTED_MAIN_RUN: &str =
    "https://github.com/kirchhausenlab/mirante4d/actions/runs/29199277402";
const PROJECT_STORE_CRATE: &str = "mirante4d-project-store";
const PROJECT_STORE_PATH: &str = "crates/mirante4d-project-store";
const FIXTURE_MANIFEST_PATH: &str = "fixtures/project/manifest.json";
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

// SHA-256 of the parsed contract serialized with sorted object keys and with only
// fixture_manifest.sha256 replaced by ZERO_SHA256. This freezes the accepted B1
// contract plus bound corrections while allowing the independent fixture
// producer to remain bound to its final manifest.
const NORMALIZED_CONTRACT_SHA256: &str =
    "d49116e00aeb3d9e158fcfe9068fe0cc7c23d10c9cbb11d56461348ce99a36ec";

pub(super) fn check_wp10b_project_store_contract(repo_root: &Path) -> anyhow::Result<()> {
    validate_b4_authorities(repo_root)?;
    let contract_path = repo_root.join(CONTRACT_PATH);
    let mut contract: Value = serde_json::from_slice(
        &fs::read(&contract_path)
            .with_context(|| format!("failed to read {}", contract_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", contract_path.display()))?;

    validate_header_and_bindings(repo_root, &contract)?;
    validate_frozen_contract(&mut contract)?;
    validate_fixture_binding(repo_root)?;
    validate_project_store_crate(repo_root, &contract)?;
    Ok(())
}

fn validate_b4_authorities(repo_root: &Path) -> anyhow::Result<()> {
    let activation = read_bound_authority(repo_root, B4_ACTIVATION_PATH, B4_ACTIVATION_SHA256)?;
    validate_b4_correction_header(&activation)?;
    expect_string(
        &activation,
        "/accepted_predecessor/commit",
        B4_PREDECESSOR_COMMIT,
    )?;
    expect_string(
        &activation,
        "/accepted_predecessor/tree",
        B4_PREDECESSOR_TREE,
    )?;
    expect_string(
        &activation,
        "/authority_flip/successor",
        "mirante4d-project-store actor through mirante4d-application::ProjectStoreApplicationService",
    )?;
    expect_string(
        &activation,
        "/authority_flip/composition_owner",
        "mirante4d-app",
    )?;
    let expected_predecessors = BTreeSet::from([
        "crates/mirante4d-app/src/current_project_persistence_bridge.rs".to_owned(),
        "crates/mirante4d-app/src/current_runtime/project.rs".to_owned(),
    ]);
    if string_set(&activation, "/authority_flip/predecessor_files")? != expected_predecessors {
        bail!("WP-10B B4 predecessor-file authority drifted");
    }

    let correction = read_bound_authority(
        repo_root,
        B4_SCOPE_CORRECTION_PATH,
        B4_SCOPE_CORRECTION_SHA256,
    )?;
    validate_b4_correction_header(&correction)?;
    expect_string(
        &correction,
        "/accepted_boundary/activation_path",
        B4_ACTIVATION_PATH,
    )?;
    expect_string(
        &correction,
        "/accepted_boundary/activation_sha256",
        B4_ACTIVATION_SHA256,
    )?;
    expect_string(
        &correction,
        "/accepted_boundary/accepted_predecessor_commit",
        B4_PREDECESSOR_COMMIT,
    )?;
    expect_string(
        &correction,
        "/accepted_boundary/accepted_predecessor_tree",
        B4_PREDECESSOR_TREE,
    )?;
    let expected_additional_paths = BTreeSet::from([
        B4_SCOPE_CORRECTION_PATH.to_owned(),
        "Cargo.lock".to_owned(),
        "crates/xtask/src/arch/wp08a.rs".to_owned(),
    ]);
    if string_set(&correction, "/correction/additional_allowed_paths")? != expected_additional_paths
    {
        bail!("WP-10B B4 scope-correction paths drifted");
    }
    Ok(())
}

fn read_bound_authority(
    repo_root: &Path,
    relative_path: &str,
    expected_sha256: &str,
) -> anyhow::Result<Value> {
    let path = repo_root.join(relative_path);
    let observed = sha256_file(&path)?;
    if observed != expected_sha256 {
        bail!(
            "WP-10B B4 authority {relative_path} drifted: expected {expected_sha256}, observed {observed}"
        );
    }
    serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn validate_b4_correction_header(document: &Value) -> anyhow::Result<()> {
    expect_string(
        document,
        "/schema",
        "mirante4d-foundation-package-entry-correction",
    )?;
    expect_u64(document, "/schema_version", 1)?;
    expect_string(document, "/package", "WP-10B")?;
    expect_string(document, "/checkpoint", "B4")?;
    expect_string(document, "/status", "accepted")
}

fn validate_header_and_bindings(repo_root: &Path, contract: &Value) -> anyhow::Result<()> {
    expect_string(contract, "/schema", CONTRACT_SCHEMA)?;
    expect_u64(contract, "/schema_version", 1)?;
    expect_string(contract, "/status", "accepted-wire-authority")?;

    for (name, path, digest) in [
        ("entry", ENTRY_PATH, ENTRY_SHA256),
        (
            "dependency_correction",
            DEPENDENCY_CORRECTION_PATH,
            DEPENDENCY_CORRECTION_SHA256,
        ),
        (
            "quaternion_correction",
            QUATERNION_CORRECTION_PATH,
            QUATERNION_CORRECTION_SHA256,
        ),
        (
            "recovery_ahead_correction",
            RECOVERY_AHEAD_CORRECTION_PATH,
            RECOVERY_AHEAD_CORRECTION_SHA256,
        ),
        (
            "recovery_api_correction",
            RECOVERY_API_CORRECTION_PATH,
            RECOVERY_API_CORRECTION_SHA256,
        ),
        (
            "maintenance_transition_correction",
            MAINTENANCE_TRANSITION_CORRECTION_PATH,
            MAINTENANCE_TRANSITION_CORRECTION_SHA256,
        ),
        (
            "trash_safety_correction",
            TRASH_SAFETY_CORRECTION_PATH,
            TRASH_SAFETY_CORRECTION_SHA256,
        ),
        (
            "purge_safety_correction",
            PURGE_SAFETY_CORRECTION_PATH,
            PURGE_SAFETY_CORRECTION_SHA256,
        ),
        (
            "provisional_autosave_correction",
            PROVISIONAL_AUTOSAVE_CORRECTION_PATH,
            PROVISIONAL_AUTOSAVE_CORRECTION_SHA256,
        ),
        (
            "staging_cleanup_correction",
            STAGING_CLEANUP_CORRECTION_PATH,
            STAGING_CLEANUP_CORRECTION_SHA256,
        ),
        (
            "public_actor_lifecycle_correction",
            PUBLIC_ACTOR_LIFECYCLE_CORRECTION_PATH,
            PUBLIC_ACTOR_LIFECYCLE_CORRECTION_SHA256,
        ),
        (
            "durability_qualification_correction",
            DURABILITY_QUALIFICATION_CORRECTION_PATH,
            DURABILITY_QUALIFICATION_CORRECTION_SHA256,
        ),
        (
            "b3_integration_correction",
            B3_INTEGRATION_CORRECTION_PATH,
            B3_INTEGRATION_CORRECTION_SHA256,
        ),
    ] {
        expect_string(contract, &format!("/bindings/{name}/path"), path)?;
        expect_string(contract, &format!("/bindings/{name}/sha256"), digest)?;
        if sha256_file(&repo_root.join(path))? != digest {
            bail!("WP-10B bound authority {path} changed");
        }
    }

    expect_string(
        contract,
        "/bindings/protected_main/commit",
        PROTECTED_MAIN_COMMIT,
    )?;
    expect_string(
        contract,
        "/bindings/protected_main/tree",
        PROTECTED_MAIN_TREE,
    )?;
    expect_string(
        contract,
        "/bindings/protected_main/hosted_run",
        PROTECTED_MAIN_RUN,
    )?;
    expect_string(
        contract,
        "/bindings/protected_main/hosted_result",
        "success",
    )?;
    Ok(())
}

fn validate_frozen_contract(contract: &mut Value) -> anyhow::Result<()> {
    let fixture_digest = contract
        .pointer_mut("/fixture_manifest/sha256")
        .context("WP-10B contract has no fixture manifest digest")?;
    if !fixture_digest.is_string() {
        bail!("WP-10B fixture manifest digest must be a string");
    }
    *fixture_digest = Value::String(ZERO_SHA256.to_owned());

    let canonical = serde_json::to_vec(&sorted_json(contract))
        .context("failed to canonicalize the parsed WP-10B contract")?;
    let observed = sha256_bytes(&canonical)?;
    if observed != NORMALIZED_CONTRACT_SHA256 {
        bail!(
            "{CONTRACT_PATH} drifted from the frozen B1 wire/API/transition contract: expected {NORMALIZED_CONTRACT_SHA256}, observed {observed}"
        );
    }
    Ok(())
}

fn sorted_json(value: &Value) -> Value {
    match value {
        Value::Array(rows) => Value::Array(rows.iter().map(sorted_json).collect()),
        Value::Object(rows) => {
            let mut entries = rows.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), sorted_json(value)))
                    .collect(),
            )
        }
        scalar => scalar.clone(),
    }
}

fn validate_fixture_binding(repo_root: &Path) -> anyhow::Result<()> {
    // validate_frozen_contract normalizes its mutable Value in place, so read the
    // checked-in document again to inspect the one deliberately late-bound fact.
    let contract: Value = serde_json::from_slice(&fs::read(repo_root.join(CONTRACT_PATH))?)?;
    expect_string(&contract, "/fixture_manifest/path", FIXTURE_MANIFEST_PATH)?;
    expect_string(
        &contract,
        "/fixture_manifest/schema",
        "mirante4d-foundation-project-fixture-manifest",
    )?;
    expect_u64(&contract, "/fixture_manifest/schema_version", 1)?;
    expect_string(&contract, "/fixture_manifest/tier", "T1-project")?;

    let expected = string(&contract, "/fixture_manifest/sha256")?;
    if expected == ZERO_SHA256 || !is_lower_sha256(expected) {
        bail!(
            "WP-10B independent fixture manifest is not bound yet; replace the reserved zero digest with its exact lowercase SHA-256"
        );
    }
    let path = repo_root.join(FIXTURE_MANIFEST_PATH);
    let observed = sha256_file(&path)?;
    if observed != expected {
        bail!(
            "WP-10B independent fixture manifest digest drifted: expected {expected}, observed {observed}"
        );
    }
    Ok(())
}

fn validate_project_store_crate(repo_root: &Path, contract: &Value) -> anyhow::Result<()> {
    expect_string(contract, "/activation/crate_name", PROJECT_STORE_CRATE)?;
    expect_string(contract, "/activation/crate_path", PROJECT_STORE_PATH)?;
    expect_string(contract, "/activation/lifecycle", "EXPERIMENTAL")?;
    expect_string(
        contract,
        "/activation/product_status",
        "off-product-through-B3",
    )?;
    expect_string(contract, "/activation/product_activation_checkpoint", "B4")?;
    expect_bool(contract, "/activation/product_reachability", false)?;

    let crate_root = repo_root.join(PROJECT_STORE_PATH);
    let library_root = crate_root.join("src/lib.rs");
    if !crate_root.exists() {
        return Ok(());
    }
    if !library_root.is_file() || !crate_root.join("Cargo.toml").is_file() {
        bail!(
            "an introduced WP-10B project-store crate must contain both Cargo.toml and src/lib.rs"
        );
    }

    let expected_api = string_set(contract, "/public_api")?;
    if expected_api.len() != 16 {
        bail!("WP-10B freezes exactly sixteen public root names");
    }
    let accepted_wp12_api = BTreeSet::from([
        "LoadedProjectArtifact".to_owned(),
        "ProjectObjectBytes".to_owned(),
    ]);
    let expected_live_api = expected_api
        .union(&accepted_wp12_api)
        .cloned()
        .collect::<BTreeSet<_>>();
    let actual_api = public_root_api_names(&library_root)?;
    if actual_api != expected_live_api {
        bail!(
            "WP-10B project-store public root drifted: missing={:?}, extra={:?}",
            expected_live_api
                .difference(&actual_api)
                .collect::<Vec<_>>(),
            actual_api
                .difference(&expected_live_api)
                .collect::<Vec<_>>()
        );
    }

    let metadata = workspace_dependency_metadata(repo_root)?;
    let actual_kinds = metadata
        .declared_dependency_kinds_by_name
        .get(PROJECT_STORE_CRATE)
        .context("WP-10B project-store crate is not a workspace member")?;
    let expected_normal = string_set(contract, "/dependencies/normal_workspace")?
        .into_iter()
        .chain(string_set(contract, "/dependencies/normal_external")?)
        .collect::<BTreeSet<_>>();
    let expected_dev = string_set(contract, "/dependencies/dev_workspace")?
        .into_iter()
        .chain(string_set(contract, "/dependencies/dev_external")?)
        .collect::<BTreeSet<_>>();
    let actual_normal = actual_kinds.get("normal").cloned().unwrap_or_default();
    let actual_dev = actual_kinds.get("dev").cloned().unwrap_or_default();
    let actual_build = actual_kinds.get("build").cloned().unwrap_or_default();
    if actual_normal != expected_normal || actual_dev != expected_dev || !actual_build.is_empty() {
        bail!(
            "WP-10B project-store dependencies drifted: normal={actual_normal:?}, dev={actual_dev:?}, build={actual_build:?}"
        );
    }
    if metadata.custom_build_package_ids.contains(
        metadata
            .workspace_package_ids_by_name
            .get(PROJECT_STORE_CRATE)
            .context("WP-10B project-store package has no workspace package ID")?,
    ) {
        bail!("WP-10B project-store must not have a build script");
    }
    let allowed_product_consumers = BTreeSet::from([
        ("mirante4d-app", "normal"),
        ("mirante4d-application", "normal"),
    ]);
    for (source, kinds) in &metadata.declared_dependency_kinds_by_name {
        if source == PROJECT_STORE_CRATE {
            continue;
        }
        for (kind, dependencies) in kinds {
            if dependencies.contains(PROJECT_STORE_CRATE)
                && !allowed_product_consumers.contains(&(source.as_str(), kind.as_str()))
            {
                bail!("WP-10B project store has unauthorized consumer {source} ({kind})");
            }
        }
    }
    for manifest in [
        "crates/mirante4d-app/Cargo.toml",
        "crates/mirante4d-application/Cargo.toml",
    ] {
        validate_canonical_project_store_dependency(&repo_root.join(manifest))?;
    }

    validate_source_policy(repo_root, contract, &library_root)?;
    validate_b4_product_activation(repo_root, &metadata)
}

fn validate_canonical_project_store_dependency(manifest_path: &Path) -> anyhow::Result<()> {
    let source = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = source
        .parse::<toml::Table>()
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let dependencies = manifest
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .with_context(|| format!("{} has no dependency table", manifest_path.display()))?;
    if !dependencies.contains_key(PROJECT_STORE_CRATE) {
        bail!(
            "WP-10B product consumer {} must use the canonical {PROJECT_STORE_CRATE} dependency name",
            manifest_path.display()
        );
    }
    if manifest_contains_project_store_rename(&toml::Value::Table(manifest)) {
        bail!(
            "WP-10B product consumer {} must not rename {PROJECT_STORE_CRATE}",
            manifest_path.display()
        );
    }
    Ok(())
}

fn manifest_contains_project_store_rename(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => {
            table.get("package").and_then(toml::Value::as_str) == Some(PROJECT_STORE_CRATE)
                || table.values().any(manifest_contains_project_store_rename)
        }
        toml::Value::Array(values) => values.iter().any(manifest_contains_project_store_rename),
        _ => false,
    }
}

fn validate_b4_product_activation(
    repo_root: &Path,
    metadata: &WorkspaceDependencyMetadata,
) -> anyhow::Result<()> {
    for relative in [
        "crates/mirante4d-app/src/current_project_persistence_bridge.rs",
        "crates/mirante4d-app/src/current_runtime/project.rs",
    ] {
        if repo_root.join(relative).exists() {
            bail!("WP-10B B4 predecessor path still exists: {relative}");
        }
    }

    let app_root = repo_root.join("crates/mirante4d-app/src");
    let application_root = repo_root.join("crates/mirante4d-application/src");
    let service = application_root.join("project_store_service.rs");
    let root_path = application_root.join("lib.rs");
    let root_source = fs::read_to_string(&root_path)?;
    if !service.is_file()
        || rust_identifier_occurrences(&root_source, "project_store_service") != 2
        || !root_source.contains("mod project_store_service;")
    {
        bail!("WP-10B B4 application service declaration or public re-export drifted");
    }
    let service_source = fs::read_to_string(&service)
        .with_context(|| format!("failed to read {}", service.display()))?;
    validate_service_impl_targets(&service_source)?;

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
    let actual_application_api = public_root_api_names(&root_path)?;
    if !required_application_api.is_subset(&actual_application_api) {
        bail!(
            "WP-10B B4 application service API is incomplete: missing={:?}",
            required_application_api
                .difference(&actual_application_api)
                .collect::<Vec<_>>()
        );
    }

    let app_source_path = app_root.join("lib.rs");
    let app_source = fs::read_to_string(&app_source_path)?;
    let app_fields = top_level_struct_field_type_identifiers(&app_source, "MiranteWorkbenchApp")?;
    let project_store_field = app_fields
        .get("project_store")
        .context("WP-10B B4 app has no project_store composition field")?;
    let expected_project_store_field_types = BTreeSet::from(
        [
            "Option",
            "ProjectStoreApplicationService",
            "SystemMonotonicClock",
        ]
        .map(str::to_owned),
    );
    if !expected_project_store_field_types.is_subset(project_store_field) {
        bail!(
            "WP-10B B4 project_store composition type drifted: expected={expected_project_store_field_types:?}, actual={project_store_field:?}"
        );
    }
    for forbidden_field in ["project_runtime", "project_persistence"] {
        if app_fields.contains_key(forbidden_field) {
            bail!("WP-10B B4 predecessor composition field remains: {forbidden_field}");
        }
    }
    for marker in [
        "ProjectStoreApplicationService::start",
        "start_project_store_service",
        "self.poll_project_store()",
        "fn handle_project_store_event",
        "ProjectStoreServiceEvent::",
    ] {
        if !app_source.contains(marker) {
            bail!("WP-10B B4 successor product route is missing {marker:?}");
        }
    }
    let ui_source = fs::read_to_string(app_root.join("workbench_ui.rs"))?;
    for marker in [
        "ProjectStoreApplicationService::has_pending_work",
        "project_store.close()",
        "project_store.join()",
    ] {
        if !ui_source.contains(marker) {
            bail!("WP-10B B4 successor UI lifecycle is missing {marker:?}");
        }
    }

    for source_root in [&app_root, &application_root] {
        for source_path in collect_rust_source_files(source_root)? {
            let source = fs::read_to_string(&source_path)
                .with_context(|| format!("failed to read {}", source_path.display()))?;
            for forbidden in [
                "current_project_persistence_bridge",
                "CurrentProjectPersistenceBridge",
                "CurrentProjectRuntime",
                "current_project_path",
                "PROJECT_V15_SCHEMA",
                "PROJECT_V15_SCHEMA_VERSION",
                "ProjectDocumentDto",
            ] {
                if contains_rust_identifier(&source, forbidden) {
                    bail!(
                        "WP-10B B4 predecessor identifier {forbidden} remains in {}",
                        source_path.display()
                    );
                }
            }
            if source.contains("mirante4d-project-v15") {
                bail!(
                    "WP-10B B4 predecessor schema string remains in {}",
                    source_path.display()
                );
            }
        }
    }

    let app_normal_dependencies = metadata
        .declared_dependency_kinds_by_name
        .get("mirante4d-app")
        .and_then(|kinds| kinds.get("normal"))
        .context("cargo metadata has no mirante4d-app normal dependencies")?;
    if app_normal_dependencies.contains("mirante4d-identity") {
        bail!("WP-10B B4 app still declares the predecessor-only mirante4d-identity edge");
    }
    if !app_normal_dependencies.contains(PROJECT_STORE_CRATE) {
        bail!("WP-10B B4 app lacks its canonical project-store dependency");
    }
    Ok(())
}

fn top_level_struct_field_type_identifiers(
    source: &str,
    struct_name: &str,
) -> anyhow::Result<BTreeMap<String, BTreeSet<String>>> {
    let file = syn::parse_file(source).context("failed to parse B4 product composition source")?;
    let item = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Struct(item) if item.ident == struct_name => Some(item),
            _ => None,
        })
        .with_context(|| format!("expected one top-level struct {struct_name}"))?;
    let syn::Fields::Named(fields) = &item.fields else {
        bail!("B4 product composition struct {struct_name} must have named fields");
    };
    fields
        .named
        .iter()
        .map(|field| {
            let name = field
                .ident
                .as_ref()
                .map(ToString::to_string)
                .context("named B4 product field has no identifier")?;
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

fn contains_rust_identifier(source: &str, identifier: &str) -> bool {
    rust_identifier_occurrences(source, identifier) != 0
}

fn rust_identifier_occurrences(source: &str, identifier: &str) -> usize {
    source
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|token| *token == identifier)
        .count()
}

fn validate_service_impl_targets(source: &str) -> anyhow::Result<()> {
    let file = syn::parse_file(source)
        .context("failed to parse the WP-10B project-store application service")?;
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
            bail!("WP-10B project-store service may implement only its own local types");
        };
        let target = target
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .context("WP-10B service impl target has no type name")?;
        if !local_types.contains(&target) {
            bail!("WP-10B project-store service must not extend product-owned type {target}");
        }
    }
    Ok(())
}

fn validate_source_policy(
    repo_root: &Path,
    contract: &Value,
    library_root: &Path,
) -> anyhow::Result<()> {
    expect_bool(contract, "/source_policy/unsafe_allowed", false)?;
    let root_source = fs::read_to_string(library_root)?;
    if !root_source.contains("#![forbid(unsafe_code)]") {
        bail!("WP-10B project-store library root must forbid unsafe code");
    }
    for path in string_set(contract, "/source_policy/forbidden_paths")? {
        if repo_root.join(&path).exists() {
            bail!("WP-10B forbidden project-store path exists: {path}");
        }
    }

    let forbidden = string_set(contract, "/source_policy/forbidden_import_roots")?;
    let mut violations = Vec::new();
    for root in string_set(contract, "/source_policy/roots")? {
        let path = repo_root.join(&root);
        if !path.exists() {
            continue;
        }
        for source_path in collect_rust_source_files(&path)? {
            let source = fs::read_to_string(&source_path)
                .with_context(|| format!("failed to read {}", source_path.display()))?;
            violations.extend(forbidden_import_violations(
                &source_path,
                &source,
                &forbidden,
            )?);
        }
    }
    if !violations.is_empty() {
        bail!(
            "WP-10B project-store source boundary drifted:\n{}",
            violations.join("\n")
        );
    }
    Ok(())
}

fn sha256_bytes(bytes: &[u8]) -> anyhow::Result<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start sha256sum for WP-10B contract")?;
    child
        .stdin
        .take()
        .context("sha256sum stdin was unavailable")?
        .write_all(bytes)
        .context("failed to write WP-10B contract to sha256sum")?;
    let output = child
        .wait_with_output()
        .context("failed to wait for WP-10B contract sha256sum")?;
    if !output.status.success() {
        bail!("sha256sum failed for normalized WP-10B contract");
    }
    String::from_utf8(output.stdout)?
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .context("sha256sum returned no WP-10B contract digest")
}

fn string<'a>(document: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .with_context(|| format!("WP-10B contract {pointer} must be a string"))
}

fn expect_string(document: &Value, pointer: &str, expected: &str) -> anyhow::Result<()> {
    let actual = string(document, pointer)?;
    if actual != expected {
        bail!("WP-10B contract {pointer} drifted: expected {expected:?}, got {actual:?}");
    }
    Ok(())
}

fn expect_u64(document: &Value, pointer: &str, expected: u64) -> anyhow::Result<()> {
    let actual = document
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("WP-10B contract {pointer} must be an unsigned integer"))?;
    if actual != expected {
        bail!("WP-10B contract {pointer} drifted: expected {expected}, got {actual}");
    }
    Ok(())
}

fn expect_bool(document: &Value, pointer: &str, expected: bool) -> anyhow::Result<()> {
    let actual = document
        .pointer(pointer)
        .and_then(Value::as_bool)
        .with_context(|| format!("WP-10B contract {pointer} must be a boolean"))?;
    if actual != expected {
        bail!("WP-10B contract {pointer} drifted: expected {expected}, got {actual}");
    }
    Ok(())
}

fn string_set(document: &Value, pointer: &str) -> anyhow::Result<BTreeSet<String>> {
    let values = document
        .pointer(pointer)
        .and_then(Value::as_array)
        .with_context(|| format!("WP-10B contract {pointer} must be an array"))?;
    let mut result = BTreeSet::new();
    for value in values {
        let value = value
            .as_str()
            .with_context(|| format!("WP-10B contract {pointer} entries must be strings"))?;
        if !result.insert(value.to_owned()) {
            bail!("WP-10B contract {pointer} repeats {value:?}");
        }
    }
    Ok(result)
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b4_application_root_requires_the_service_module_and_public_route() {
        let root = "mod project_store_service;\npub use project_store_service::ProjectStoreApplicationService;\n";

        assert_eq!(
            rust_identifier_occurrences(root, "project_store_service"),
            2
        );
        assert!(!contains_rust_identifier(root, "mirante4d_project_store"));
    }

    #[test]
    fn b4_predecessor_identifier_detection_is_token_exact() {
        for source in [
            "mod current_project_persistence_bridge;",
            "struct Shell { project: CurrentProjectRuntime }",
            "let path = current_project_path;",
        ] {
            assert!(
                [
                    "current_project_persistence_bridge",
                    "CurrentProjectRuntime",
                    "current_project_path",
                ]
                .iter()
                .any(|identifier| contains_rust_identifier(source, identifier)),
                "missed predecessor identifier: {source}"
            );
        }
    }

    #[test]
    fn b4_service_rejects_application_state_extension() {
        let error = validate_service_impl_targets(
            "use crate::ApplicationState; impl ApplicationState { pub fn persist(&self) {} }",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("product-owned type ApplicationState")
        );
        validate_service_impl_targets(
            "struct ProjectStoreService; impl ProjectStoreService { fn poll(&mut self) {} }",
        )
        .unwrap();

        let alias_error = validate_service_impl_targets(
            "type ServiceHost = crate::ApplicationState; impl ServiceHost { pub(crate) fn poll_store(&mut self) {} }",
        )
        .unwrap_err();
        assert!(
            alias_error
                .to_string()
                .contains("product-owned type ServiceHost")
        );
    }

    #[test]
    fn b4_activation_rejects_a_renamed_project_store_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let manifest = temp.path().join("Cargo.toml");
        fs::write(
            &manifest,
            "[dependencies]\nmirante4d-project-store = { path = \"../store\" }\n[target.'cfg(unix)'.dependencies]\nstore = { package = \"mirante4d-project-store\", path = \"../store\" }\n",
        )
        .unwrap();

        assert!(
            validate_canonical_project_store_dependency(&manifest)
                .unwrap_err()
                .to_string()
                .contains("must not rename mirante4d-project-store")
        );
    }
}
