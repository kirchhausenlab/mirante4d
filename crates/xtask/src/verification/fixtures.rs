use std::{collections::BTreeSet, fs};

use anyhow::{Context, bail};
use serde::Serialize;
use serde_json::Value;

use super::registry::{repo_path, sha256_file};

const FIXTURE_REGISTRY_PATH: &str = "verification/fixtures.json";
const ARCHIVE_LIMITS: &str = "max_files max_directories max_unpacked_bytes max_file_bytes \
    max_depth max_fan_out max_path_bytes max_compression_ratio";

#[derive(Debug)]
struct FileReference {
    fixture_id: String,
    role: String,
    path: String,
    sha256: String,
}

pub(crate) fn validate_fixture_registry() -> anyhow::Result<()> {
    let registry = read_json(FIXTURE_REGISTRY_PATH)?;
    let references = validate_registry_document(&registry)?;
    for reference in &references {
        let actual = sha256_file(&repo_path(&reference.path))?;
        if actual != reference.sha256 {
            bail!(
                "fixture {:?} {} digest mismatch",
                reference.fixture_id,
                reference.role
            );
        }
    }
    for record in array(&registry, "records")? {
        match string(record, "tier")? {
            "T1-source" => validate_source_manifest_contract(record)?,
            "T1-target" => validate_target_manifest_contract(record)?,
            "T1-project" => validate_project_manifest_contract(record)?,
            _ => {}
        }
    }
    println!(
        "fixture registry: {} records, {} promotions",
        array(&registry, "records")?.len(),
        array(&registry, "promotions")?.len()
    );
    Ok(())
}

fn validate_registry_document(registry: &Value) -> anyhow::Result<Vec<FileReference>> {
    require_keys(
        registry,
        "schema schema_version record_count rules promotions records",
        "",
        "fixture registry",
    )?;
    if string(registry, "schema")? != "mirante4d-verification-fixture-registry"
        || u64_field(registry, "schema_version")? != 1
    {
        bail!("fixture registry has unsupported schema identity");
    }
    let rules = object(registry, "rules")?;
    require_keys(
        rules,
        "maximum_records candidate_output_root candidate_cannot_promote_itself \
         one_promoted_authority_per_claim_scope target_format_t1_available \
         project_store_t1_available",
        "",
        "fixture registry rules",
    )?;
    if !bool_field(rules, "candidate_cannot_promote_itself")?
        || !bool_field(rules, "one_promoted_authority_per_claim_scope")?
    {
        bail!("fixture registry authority rules are invalid");
    }
    let candidate_root = string(rules, "candidate_output_root")?;
    validate_relative_path(candidate_root, "fixture candidate root")?;
    if !candidate_root.starts_with("target/") {
        bail!("fixture candidates must remain under ignored target storage");
    }

    let records = array(registry, "records")?;
    let maximum = u64_field(rules, "maximum_records")?;
    if records.is_empty()
        || maximum == 0
        || records.len() as u64 > maximum
        || u64_field(registry, "record_count")? != records.len() as u64
    {
        bail!("fixture registry record count is invalid");
    }
    let mut ids = BTreeSet::new();
    let mut opaque_ids = BTreeSet::new();
    let mut references = Vec::new();
    let mut source_t1_count = 0;
    let mut target_t1_count = 0;
    let mut project_t1_count = 0;
    for record in records {
        record
            .as_object()
            .context("fixture registry records must be objects")?;
        let id = string(record, "id")?;
        if !ids.insert(id) {
            bail!("duplicate fixture registry id {id:?}");
        }
        for field in [
            "claim_scope",
            "authority_state",
            "owner",
            "capability",
            "publication",
            "validation_command",
        ] {
            string(record, field).with_context(|| format!("fixture {id:?} has invalid {field}"))?;
        }
        match string(record, "tier")? {
            "T1-source" => {
                source_t1_count += 1;
                validate_source_t1(record, &mut references)?;
            }
            "T1-target" => {
                target_t1_count += 1;
                validate_target_t1(record, &mut references)?;
            }
            "T1-project" => {
                project_t1_count += 1;
                validate_project_t1(record, &mut references)?;
            }
            "T2" => validate_t2(record, &mut references)?,
            "T5" => {
                validate_t5(record)?;
                if !opaque_ids.insert(string(record, "opaque_id")?) {
                    bail!("duplicate opaque T5 fixture identity");
                }
            }
            tier => bail!("fixture {id:?} has unsupported tier {tier:?}"),
        }
    }
    if source_t1_count == 0 {
        bail!("fixture registry must retain independent source-only T1 evidence");
    }
    if target_t1_count > 1
        || bool_field(rules, "target_format_t1_available")? != (target_t1_count == 1)
    {
        bail!("target-format T1 availability must match exactly one promoted target authority");
    }
    if project_t1_count > 1
        || bool_field(rules, "project_store_t1_available")? != (project_t1_count == 1)
    {
        bail!("project-store T1 availability must match exactly one promoted authority");
    }
    validate_promotions(registry, records, candidate_root, &references)?;
    Ok(references)
}

fn validate_source_t1(record: &Value, references: &mut Vec<FileReference>) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    require_keys(
        record,
        "id tier claim_scope authority_state owner capability publication license path sha256 \
         manifest archive lineages expected_facts_sha256 independent_reader_report promotion \
         limits validation_command expiry deletion_gate target_format_conformance",
        "",
        &format!("T1-source fixture {id:?}"),
    )?;
    string(record, "license")?;
    false_field(record, "target_format_conformance", id)?;
    null_field(record, "expiry", id)?;
    null_field(record, "deletion_gate", id)?;
    reference(record, "path", "sha256", id, "archive root", references)?;

    let manifest = object(record, "manifest")?;
    require_keys(manifest, "path sha256", "", "source manifest reference")?;
    reference(manifest, "path", "sha256", id, "manifest", references)?;
    let report = object(record, "independent_reader_report")?;
    require_keys(report, "path sha256", "", "reader report reference")?;
    reference(report, "path", "sha256", id, "reader report", references)?;
    sha256(string(record, "expected_facts_sha256")?, "expected facts")?;

    let archive = object(record, "archive")?;
    require_keys(
        archive,
        "path sha256 bytes compression max_files max_directories max_unpacked_bytes \
         max_file_bytes max_depth max_fan_out max_path_bytes max_compression_ratio",
        "",
        "source archive declaration",
    )?;
    reference(archive, "path", "sha256", id, "archive", references)?;
    if u64_field(archive, "bytes")? == 0 {
        bail!("fixture {id:?} archive bytes must be positive");
    }
    string(archive, "compression")?;
    let limits = object(record, "limits")?;
    require_keys(limits, ARCHIVE_LIMITS, "", "source archive limits")?;
    for field in ARCHIVE_LIMITS.split_whitespace() {
        let declared = u64_field(archive, field)?;
        if declared == 0 || declared != u64_field(limits, field)? {
            bail!("fixture {id:?} has inconsistent archive limit {field:?}");
        }
    }

    let lineages = array(record, "lineages")?;
    let required_roles = BTreeSet::from([
        "byte-producer",
        "expected-fact-oracle",
        "independent-reader",
    ]);
    let mut roles = BTreeSet::new();
    let mut lineage_ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut digests = BTreeSet::new();
    for lineage in lineages {
        require_keys(lineage, "role id path sha256", "", "source lineage")?;
        roles.insert(string(lineage, "role")?);
        lineage_ids.insert(string(lineage, "id")?);
        paths.insert(string(lineage, "path")?);
        digests.insert(string(lineage, "sha256")?);
        reference(lineage, "path", "sha256", id, "lineage", references)?;
    }
    if roles != required_roles || lineage_ids.len() != 3 || paths.len() != 3 || digests.len() != 3 {
        bail!("fixture {id:?} source lineages are not pairwise independent");
    }
    validate_record_promotion(record, id)?;
    Ok(())
}

fn validate_target_t1(record: &Value, references: &mut Vec<FileReference>) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    require_keys(
        record,
        "id tier claim_scope authority_state owner capability publication license path sha256 \
         validation_command promotion expiry deletion_gate target_format_conformance",
        "",
        &format!("T1-target fixture {id:?}"),
    )?;
    if string(record, "authority_state")? != "promoted-target-format"
        || id != "target-m4d-v1"
        || string(record, "claim_scope")? != "target-m4d-v1-independent-format-conformance"
        || string(record, "capability")? != "public-cpu"
        || string(record, "publication")? != "public-safe-synthetic"
        || string(record, "license")? != "MIT"
        || string(record, "path")? != "fixtures/target/manifest.json"
        || string(record, "validation_command")?
            != "python3 tools/target-fixtures/t1/validate.py --manifest fixtures/target/manifest.json --self-test"
    {
        bail!("fixture {id:?} target authority metadata is invalid");
    }
    true_field(record, "target_format_conformance", id)?;
    null_field(record, "expiry", id)?;
    null_field(record, "deletion_gate", id)?;
    reference(record, "path", "sha256", id, "target manifest", references)?;
    validate_record_promotion(record, id)?;
    Ok(())
}

fn validate_project_t1(record: &Value, references: &mut Vec<FileReference>) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    require_keys(
        record,
        "id tier claim_scope authority_state owner capability publication license path sha256 \
         validation_command promotion expiry deletion_gate target_format_conformance",
        "",
        &format!("T1-project fixture {id:?}"),
    )?;
    if string(record, "authority_state")? != "promoted-project-store-wire"
        || id != "project-store-v1"
        || string(record, "claim_scope")? != "project-store-v1-independent-wire-and-recovery"
        || string(record, "capability")? != "public-cpu"
        || string(record, "publication")? != "public-safe-synthetic"
        || string(record, "license")? != "MIT"
        || string(record, "path")? != "fixtures/project/manifest.json"
        || string(record, "validation_command")?
            != "python3 tools/project-fixtures/validate.py --manifest fixtures/project/manifest.json --self-test"
    {
        bail!("fixture {id:?} project-store authority metadata is invalid");
    }
    false_field(record, "target_format_conformance", id)?;
    null_field(record, "expiry", id)?;
    null_field(record, "deletion_gate", id)?;
    reference(record, "path", "sha256", id, "project manifest", references)?;
    validate_record_promotion(record, id)?;
    Ok(())
}

fn validate_t2(record: &Value, references: &mut Vec<FileReference>) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    require_keys(
        record,
        "id tier claim_scope authority_state owner capability publication license path sha256 \
         lineages limits validation_command promotion expiry deletion_gate target_format_conformance",
        "generator fixture_names",
        &format!("T2 fixture {id:?}"),
    )?;
    string(record, "license")?;
    false_field(record, "target_format_conformance", id)?;
    string(record, "expiry").context("T2 fixtures require a non-empty expiry")?;
    match record.get("deletion_gate") {
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        Some(Value::Null)
            if string(record, "authority_state")? != "non-authoritative-generated" => {}
        Some(Value::Null) => bail!("generated T2 fixture {id:?} requires a deletion gate"),
        _ => bail!("T2 fixture {id:?} deletion_gate must be a non-empty string or null"),
    }
    reference(record, "path", "sha256", id, "fixture", references)?;
    object(record, "limits")?;

    for lineage in array(record, "lineages")? {
        require_keys(lineage, "role id path sha256", "", "T2 lineage")?;
        string(lineage, "role")?;
        string(lineage, "id")?;
        reference(lineage, "path", "sha256", id, "lineage", references)?;
    }
    match record.get("generator") {
        None if string(record, "authority_state")? == "non-authoritative-generated" => {
            bail!("generated T2 fixture {id:?} must declare its generator")
        }
        None => {}
        Some(generator) if generator.is_object() => {
            require_keys(
                generator,
                "command source sha256 production_writer_used",
                "",
                "T2 generator",
            )?;
            string(generator, "command")?;
            bool_field(generator, "production_writer_used")?;
            let (source, digest) =
                reference(generator, "source", "sha256", id, "generator", references)?;
            if source != string(record, "path")? || digest != string(record, "sha256")? {
                bail!("T2 fixture {id:?} generator identity disagrees with its record");
            }
        }
        Some(_) => bail!("T2 fixture {id:?} generator must be an object"),
    }
    if let Some(names) = record.get("fixture_names") {
        let names = names
            .as_array()
            .context("T2 fixture_names must be an array")?;
        let mut unique = BTreeSet::new();
        if names.is_empty()
            || names
                .iter()
                .any(|name| nonempty(name, "fixture name").is_err())
            || names
                .iter()
                .any(|name| !unique.insert(name.as_str().unwrap()))
        {
            bail!("T2 fixture {id:?} has invalid fixture_names");
        }
    }
    match record.get("promotion") {
        Some(Value::Null) => {}
        Some(value) if value.is_object() => validate_record_promotion(record, id)?,
        _ => bail!("T2 fixture {id:?} promotion must be an object or null"),
    }
    Ok(())
}

fn validate_t5(record: &Value) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    require_keys(
        record,
        "id tier claim_scope authority_state owner capability publication validation_command \
         lineages limits opaque_id public_identity public_path_or_digest expiry deletion_gate",
        "",
        &format!("T5 fixture {id:?}"),
    )?;
    if !id.starts_with("T5-")
        || string(record, "opaque_id")? != id
        || string(record, "authority_state")? != "private-resolver-only"
        || string(record, "publication")? != "internal_lab_data"
        || string(record, "public_identity")? != "opaque-id-only"
    {
        bail!("fixture {id:?} is not an opaque private-resolver identity");
    }
    null_field(record, "public_path_or_digest", id)?;
    null_field(record, "expiry", id)?;
    null_field(record, "deletion_gate", id)?;
    if !array(record, "lineages")?.is_empty() {
        bail!("T5 fixture {id:?} must expose no public lineages");
    }
    let limits = object(record, "limits")?;
    require_keys(
        limits,
        "public_paths public_digests",
        "",
        "T5 public limits",
    )?;
    if u64_field(limits, "public_paths")? != 0 || u64_field(limits, "public_digests")? != 0 {
        bail!("T5 fixture {id:?} must expose zero public paths and digests");
    }
    Ok(())
}

fn validate_promotions(
    registry: &Value,
    records: &[Value],
    candidate_root: &str,
    references: &[FileReference],
) -> anyhow::Result<()> {
    let mut promoted_ids = BTreeSet::new();
    let mut claim_scopes = BTreeSet::new();
    for promotion in array(registry, "promotions")? {
        require_keys(
            promotion,
            "fixture_id authority changed_bytes_or_facts_require_new_version",
            "approved_on",
            "fixture promotion",
        )?;
        let id = string(promotion, "fixture_id")?;
        if !promoted_ids.insert(id)
            || !bool_field(promotion, "changed_bytes_or_facts_require_new_version")?
        {
            bail!("fixture {id:?} has an invalid or duplicate promotion");
        }
        let authority = string(promotion, "authority")?;
        if authority.starts_with(candidate_root) {
            bail!("fixture {id:?} cannot promote itself from candidate output");
        }
        optional_string(promotion, "approved_on")?;
        let record = records
            .iter()
            .find(|record| record.get("id").and_then(Value::as_str) == Some(id))
            .with_context(|| format!("promotion names unknown fixture {id:?}"))?;
        let record_promotion = record
            .get("promotion")
            .filter(|value| value.is_object())
            .with_context(|| format!("promoted fixture {id:?} lacks record promotion metadata"))?;
        if string(record_promotion, "authority")? != authority
            || optional_string(record_promotion, "approved_on")?
                != optional_string(promotion, "approved_on")?
            || !claim_scopes.insert(string(record, "claim_scope")?)
            || references.iter().any(|reference| {
                reference.fixture_id == id && reference.path.starts_with(candidate_root)
            })
        {
            bail!("fixture {id:?} promotion disagrees with its record or authority rules");
        }
    }
    let record_promotions = records
        .iter()
        .filter(|record| record.get("promotion").is_some_and(Value::is_object))
        .map(|record| string(record, "id"))
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    if promoted_ids != record_promotions {
        bail!("top-level promotions and record promotion metadata disagree");
    }
    Ok(())
}

fn validate_record_promotion(record: &Value, id: &str) -> anyhow::Result<()> {
    let promotion = object(record, "promotion")?;
    require_keys(
        promotion,
        "authority changed_authority_requires_new_version",
        "approved_on",
        "record promotion",
    )?;
    string(promotion, "authority")?;
    optional_string(promotion, "approved_on")?;
    if !bool_field(promotion, "changed_authority_requires_new_version")? {
        bail!("fixture {id:?} promotion changes must require a new version");
    }
    Ok(())
}

fn validate_source_manifest_contract(record: &Value) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    let manifest_ref = object(record, "manifest")?;
    let manifest = read_json(string(manifest_ref, "path")?)?;
    if string(&manifest, "schema")? != "mirante4d-foundation-source-fixture-manifest"
        || u64_field(&manifest, "schema_version")? != 1
        || string(&manifest, "status")? != "independently_validated"
    {
        bail!("fixture {id:?} source manifest identity or status is invalid");
    }
    let archive = object(record, "archive")?;
    for (record_field, pointer) in [
        ("path", "/archive/path"),
        ("sha256", "/archive/sha256"),
        ("compression", "/archive/compression"),
    ] {
        if string(archive, record_field)? != pointer_string(&manifest, pointer)? {
            bail!("fixture {id:?} archive declaration disagrees with its manifest");
        }
    }
    if string(record, "path")? != string(archive, "path")?
        || string(record, "sha256")? != string(archive, "sha256")?
        || u64_field(archive, "bytes")? != pointer_u64(&manifest, "/archive/archive_bytes")?
        || fs::metadata(repo_path(string(archive, "path")?))?.len() != u64_field(archive, "bytes")?
        || string(record, "expected_facts_sha256")?
            != pointer_string(&manifest, "/expected_facts/sha256")?
        || string(object(record, "independent_reader_report")?, "sha256")?
            != pointer_string(
                &manifest,
                "/expected_facts/independent_reader_report_sha256",
            )?
    {
        bail!("fixture {id:?} source identities disagree with its manifest or file");
    }

    for lineage in array(record, "lineages")? {
        let base = match string(lineage, "role")? {
            "byte-producer" => "/producer",
            "expected-fact-oracle" => "/expected_fact_authority",
            "independent-reader" => "/independent_reader",
            _ => unreachable!("roles checked structurally"),
        };
        for (field, suffix) in [
            ("id", "lineage_id"),
            ("path", "implementation_source"),
            ("sha256", "implementation_sha256"),
        ] {
            if string(lineage, field)? != pointer_string(&manifest, &format!("{base}/{suffix}"))? {
                bail!("fixture {id:?} {base} lineage disagrees with its manifest");
            }
        }
    }

    let limits = object(record, "limits")?;
    let file_count = manifest
        .get("files")
        .and_then(Value::as_object)
        .context("source manifest files must be an object")?
        .len() as u64;
    let directory_count = manifest
        .pointer("/archive/directories")
        .and_then(Value::as_array)
        .context("source manifest directories must be an array")?
        .len() as u64;
    for (observed, limit) in [
        (file_count, "max_files"),
        (directory_count, "max_directories"),
        (
            pointer_u64(&manifest, "/archive/regular_file_bytes")?,
            "max_unpacked_bytes",
        ),
        (
            pointer_u64(&manifest, "/archive/max_file_bytes")?,
            "max_file_bytes",
        ),
        (pointer_u64(&manifest, "/archive/max_depth")?, "max_depth"),
        (
            pointer_u64(&manifest, "/archive/max_fan_out")?,
            "max_fan_out",
        ),
        (
            pointer_u64(&manifest, "/archive/max_path_bytes")?,
            "max_path_bytes",
        ),
        (
            pointer_u64(&manifest, "/archive/compression_ratio")?,
            "max_compression_ratio",
        ),
    ] {
        if observed > u64_field(limits, limit)? {
            bail!("fixture {id:?} manifest exceeds declared {limit}");
        }
    }
    Ok(())
}

fn validate_target_manifest_contract(record: &Value) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    let manifest = read_json(string(record, "path")?)?;
    if string(&manifest, "schema")? != "mirante4d-foundation-target-fixture-manifest"
        || u64_field(&manifest, "schema_version")? != 1
        || string(&manifest, "status")? != "independently_validated"
        || string(&manifest, "fixture_id")? != id
        || pointer_string(&manifest, "/approvals/repository_owner/state")? != "approved"
        || pointer_string(&manifest, "/approvals/scientific_content/state")? != "approved"
    {
        bail!("fixture {id:?} target manifest identity or approval is invalid");
    }
    Ok(())
}

fn validate_project_manifest_contract(record: &Value) -> anyhow::Result<()> {
    let id = string(record, "id")?;
    let manifest = read_json(string(record, "path")?)?;
    if string(&manifest, "schema")? != "mirante4d-foundation-project-fixture-manifest"
        || u64_field(&manifest, "schema_version")? != 1
        || string(&manifest, "status")? != "independently_validated"
        || string(&manifest, "fixture_id")? != id
        || pointer_string(&manifest, "/approvals/repository_owner/state")? != "approved"
        || pointer_string(&manifest, "/contract/path")?
            != "architecture/wp10b-project-store-contract.json"
        || pointer_string(&manifest, "/archive/path")? != "fixtures/project/project-store-v1.tar.gz"
    {
        bail!("fixture {id:?} project manifest identity or approval is invalid");
    }
    let archive = repo_path(pointer_string(&manifest, "/archive/path")?);
    if sha256_file(&archive)? != pointer_string(&manifest, "/archive/sha256")?
        || fs::metadata(&archive)?.len() != pointer_u64(&manifest, "/archive/archive_bytes")?
    {
        bail!("fixture {id:?} project archive disagrees with its manifest");
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub(crate) struct FixtureIdentity {
    id: String,
    tier: String,
    sha256: Option<String>,
    opaque_id: Option<String>,
}

pub(crate) fn fixture_identities() -> anyhow::Result<Vec<FixtureIdentity>> {
    // Reports may only emit identities after the same file, digest, manifest,
    // lineage, and limit checks used by the policy leaf have passed.
    validate_fixture_registry()?;
    let registry = read_json(FIXTURE_REGISTRY_PATH)?;
    array(&registry, "records")?
        .iter()
        .map(|record| {
            Ok(FixtureIdentity {
                id: string(record, "id")?.to_owned(),
                tier: string(record, "tier")?.to_owned(),
                sha256: record
                    .get("sha256")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                opaque_id: record
                    .get("opaque_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

fn read_json(relative: &str) -> anyhow::Result<Value> {
    let path = repo_path(relative);
    serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))
}

fn reference<'a>(
    value: &'a Value,
    path_field: &str,
    digest_field: &str,
    id: &str,
    role: &str,
    references: &mut Vec<FileReference>,
) -> anyhow::Result<(&'a str, &'a str)> {
    let path = string(value, path_field)?;
    let digest = string(value, digest_field)?;
    validate_relative_path(path, role)?;
    sha256(digest, role)?;
    references.push(FileReference {
        fixture_id: id.to_owned(),
        role: role.to_owned(),
        path: path.to_owned(),
        sha256: digest.to_owned(),
    });
    Ok((path, digest))
}

fn validate_relative_path(path: &str, label: &str) -> anyhow::Result<()> {
    if path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        bail!("{label} has unsafe repository path {path:?}");
    }
    Ok(())
}

fn sha256(digest: &str, label: &str) -> anyhow::Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} has invalid SHA-256 digest");
    }
    Ok(())
}

fn require_keys(value: &Value, required: &str, optional: &str, label: &str) -> anyhow::Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{label} must be an object"))?;
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let required = required.split_whitespace().collect::<BTreeSet<_>>();
    let allowed = required
        .union(&optional.split_whitespace().collect())
        .copied()
        .collect::<BTreeSet<_>>();
    if !required.is_subset(&actual) || !actual.is_subset(&allowed) {
        bail!("{label} fields differ: required {required:?}, allowed {allowed:?}, got {actual:?}");
    }
    Ok(())
}

fn object<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a Value> {
    value
        .get(field)
        .filter(|value| value.is_object())
        .with_context(|| format!("field {field:?} must be an object"))
}

fn array<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a [Value]> {
    value
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .with_context(|| format!("field {field:?} must be an array"))
}

fn string<'a>(value: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    nonempty(
        value
            .get(field)
            .with_context(|| format!("missing field {field:?}"))?,
        field,
    )
}

fn nonempty<'a>(value: &'a Value, label: &str) -> anyhow::Result<&'a str> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{label:?} must be a non-empty string"))
}

fn optional_string<'a>(value: &'a Value, field: &str) -> anyhow::Result<Option<&'a str>> {
    value
        .get(field)
        .map(|value| nonempty(value, field))
        .transpose()
}

fn u64_field(value: &Value, field: &str) -> anyhow::Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .with_context(|| format!("field {field:?} must be an unsigned integer"))
}

fn bool_field(value: &Value, field: &str) -> anyhow::Result<bool> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .with_context(|| format!("field {field:?} must be a boolean"))
}

fn false_field(value: &Value, field: &str, id: &str) -> anyhow::Result<()> {
    if bool_field(value, field)? {
        bail!("fixture {id:?} cannot set {field} to true");
    }
    Ok(())
}

fn true_field(value: &Value, field: &str, id: &str) -> anyhow::Result<()> {
    if !bool_field(value, field)? {
        bail!("fixture {id:?} must set {field} to true");
    }
    Ok(())
}

fn null_field(value: &Value, field: &str, id: &str) -> anyhow::Result<()> {
    match value.get(field) {
        Some(Value::Null) => Ok(()),
        Some(_) => bail!("fixture {id:?} field {field:?} must be null"),
        None => bail!("fixture {id:?} is missing field {field:?}"),
    }
}

fn pointer_string<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<&'a str> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{pointer} must be a non-empty string"))
}

fn pointer_u64(value: &Value, pointer: &str) -> anyhow::Result<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .with_context(|| format!("{pointer} must be an unsigned integer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Value {
        serde_json::from_str(include_str!("../../../../verification/fixtures.json")).unwrap()
    }

    fn record_mut<'a>(registry: &'a mut Value, id: &str) -> &'a mut Value {
        registry["records"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|record| record["id"] == id)
            .unwrap()
    }

    #[test]
    fn checked_registry_is_structurally_valid() {
        assert!(!validate_registry_document(&registry()).unwrap().is_empty());
    }

    #[test]
    fn rejects_wrong_rule_archive_and_lifecycle_types() {
        let mut value = registry();
        value["rules"]["candidate_cannot_promote_itself"] = Value::String("true".into());
        assert!(validate_registry_document(&value).is_err());

        let mut value = registry();
        record_mut(&mut value, "source-tiff-v1")["archive"]["bytes"] =
            Value::String("51200".into());
        assert!(validate_registry_document(&value).is_err());

        let mut value = registry();
        record_mut(&mut value, "bootstrap-m4d-schema1")["deletion_gate"] = Value::Bool(false);
        assert!(validate_registry_document(&value).is_err());
    }

    #[test]
    fn rejects_nonopaque_t5_and_malformed_promotions() {
        let mut value = registry();
        record_mut(&mut value, "T5-QUAL-001")["path"] = Value::String("private/data".into());
        assert!(validate_registry_document(&value).is_err());

        let mut value = registry();
        record_mut(&mut value, "T5-QUAL-001")["lineages"] = serde_json::json!([{}]);
        assert!(validate_registry_document(&value).is_err());

        let mut value = registry();
        value["promotions"][0]["changed_bytes_or_facts_require_new_version"] =
            Value::String("true".into());
        assert!(validate_registry_document(&value).is_err());
    }
}
