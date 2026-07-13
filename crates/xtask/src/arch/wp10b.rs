use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use serde_json::Value;

use super::{
    collect_rust_source_files, public_root_api_names, sha256_file, workspace_dependency_metadata,
    wp08a::forbidden_import_violations,
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
    let actual_api = public_root_api_names(&library_root)?;
    if actual_api != expected_api {
        bail!(
            "WP-10B project-store public root drifted: missing={:?}, extra={:?}",
            expected_api.difference(&actual_api).collect::<Vec<_>>(),
            actual_api.difference(&expected_api).collect::<Vec<_>>()
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
    let allowed_b3_consumers = BTreeSet::from([
        ("mirante4d-app", "normal"),
        ("mirante4d-application", "normal"),
    ]);
    for (source, kinds) in &metadata.declared_dependency_kinds_by_name {
        if source == PROJECT_STORE_CRATE {
            continue;
        }
        for (kind, dependencies) in kinds {
            if dependencies.contains(PROJECT_STORE_CRATE) {
                if !allowed_b3_consumers.contains(&(source.as_str(), kind.as_str())) {
                    bail!(
                        "off-product WP-10B B3 project store has unauthorized consumer {source} ({kind})"
                    );
                }
            }
        }
    }

    validate_source_policy(repo_root, contract, &library_root)?;
    validate_b3_product_isolation(repo_root)
}

fn validate_b3_product_isolation(repo_root: &Path) -> anyhow::Result<()> {
    for relative in [
        "crates/mirante4d-app/src/current_project_persistence_bridge.rs",
        "crates/mirante4d-app/src/current_runtime/project.rs",
    ] {
        if !repo_root.join(relative).is_file() {
            bail!("WP-10B B3 predecessor must remain the sole product route: missing {relative}");
        }
    }

    let app_root = repo_root.join("crates/mirante4d-app/src");
    for source_path in collect_rust_source_files(&app_root)? {
        let relative = source_path
            .strip_prefix(&app_root)
            .unwrap_or(&source_path)
            .to_string_lossy();
        if relative == "tests.rs"
            || relative.starts_with("tests/")
            || relative.ends_with("/tests.rs")
        {
            continue;
        }
        let source = fs::read_to_string(&source_path)
            .with_context(|| format!("failed to read {}", source_path.display()))?;
        for forbidden in [
            "mirante4d_project_store",
            "ProjectStoreService",
            "project_store_service::",
        ] {
            if source.contains(forbidden) {
                bail!(
                    "WP-10B B3 project-store service reached product source {} through {forbidden}",
                    source_path.display()
                );
            }
        }
    }

    let application_root = repo_root.join("crates/mirante4d-application/src");
    let service = application_root.join("project_store_service.rs");
    if service.exists() {
        let root_source = fs::read_to_string(application_root.join("lib.rs"))?;
        if !root_source.contains("mod project_store_service;")
            || root_source.contains("pub mod project_store_service;")
            || root_source.contains("pub use project_store_service")
        {
            bail!("WP-10B B3 project-store service must compile as one private application module");
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
