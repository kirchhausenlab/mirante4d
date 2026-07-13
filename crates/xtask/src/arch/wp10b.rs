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
    "8cb08e614c619097d14a95b5634d21a180d431b3fed8a4e155da115def9533a8";

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
    for (source, kinds) in &metadata.declared_dependency_kinds_by_name {
        if source == PROJECT_STORE_CRATE {
            continue;
        }
        for (kind, dependencies) in kinds {
            if dependencies.contains(PROJECT_STORE_CRATE) {
                bail!("off-product WP-10B B1 project store is reachable from {source} ({kind})");
            }
        }
    }

    validate_source_policy(repo_root, contract, &library_root)
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
