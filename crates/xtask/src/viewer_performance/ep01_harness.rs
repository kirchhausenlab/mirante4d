//! Headless EP-01 authority preflight and exact gate reduction.
//!
//! This module deliberately does not authorize candidate selection yet.
//! Non-gate artifacts receive bounded canonical envelope/manifest admission
//! only; complete bound-schema validation and source-commitment resolution
//! must construct the otherwise unconstructible `StrictArtifactClosure`
//! before selection or public projection can run.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, bail};
use mirante4d_identity::Sha256Hasher;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const AUTHORITY_BYTES: &[u8] =
    include_bytes!("../../../../verification/viewer-performance-ep01-selection.json");
const AUTHORITY_SHA256: &str = "da8640314abaf95b57c3ed3da3afe627ccb6e3f250deb4ca7151f311f573ad20";
const MAX_ARTIFACT_BYTES: usize = 67_108_864;
const MAX_PRIMITIVE_OBSERVATIONS: usize = 262_144;
const GATE_COUNT: usize = 74;

const COMMON_SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../../verification/schemas/viewer-performance-ep01-common.schema.json");
const FAILURE_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-failure-evidence.schema.json"
);
const PROJECTION_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-projection-input.schema.json"
);
const SOURCE_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-source-inventory.schema.json"
);
const PACKAGE_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-package-validation.schema.json"
);
const BUILD_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-build-import.schema.json"
);
const TRACE_SCHEMA_BYTES: &[u8] =
    include_bytes!("../../../../verification/schemas/viewer-performance-ep01-trace.schema.json");
const RUNTIME_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-runtime-gpu.schema.json"
);
const OBSERVATION_SCHEMA_BYTES: &[u8] = include_bytes!(
    "../../../../verification/schemas/viewer-performance-ep01-gate-observation.schema.json"
);

#[derive(Clone, Debug)]
pub(crate) struct Ep01PreflightBinding {
    pub(crate) repository_revision: String,
    pub(crate) clean_tree: bool,
    pub(crate) qualification_binding_verified: bool,
    pub(crate) selection_authority_sha256: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Ep01Authority {
    authority_sha256: String,
    gates: Vec<GateDefinition>,
}

#[derive(Clone, Debug)]
pub(crate) struct GateDefinition {
    pub(crate) ordinal: u32,
    pub(crate) gate_id: String,
    pub(crate) authority_path: String,
    pub(crate) unit: Unit,
    pub(crate) scope: String,
    aggregation: Aggregation,
    pub(crate) comparator: Comparator,
    pub(crate) limit: u128,
    raw_ratio: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Aggregation {
    MaximumRatio,
    NearestRankP95,
    Exact,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Comparator {
    HeadroomLte,
    DirectLte,
    ExactEq,
    ZeroEq,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Unit {
    Bytes,
    Count,
    Nanoseconds,
    Seconds,
    BasisPoints,
    BytesPerByte,
    RequestsPerBrick,
    AttemptsPerBrick,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TieKey {
    pub(crate) package_role_tag: u8,
    pub(crate) trace_family_tag: u8,
    pub(crate) state_ordinal: String,
    pub(crate) protocol_sample_ordinal: String,
    pub(crate) within_state_observation_ordinal: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrimitiveSourceCommitment {
    pub(crate) source_kind_tag: u8,
    pub(crate) source_kind: PrimitiveSourceKind,
    pub(crate) source_artifact_sha256: String,
    pub(crate) source_field_path: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PrimitiveSourceKind {
    SelectionAuthority,
    PackageValidation,
    BuildImportAccounting,
    RuntimeGpu,
}

impl PrimitiveSourceKind {
    fn tag(self) -> u8 {
        match self {
            Self::SelectionAuthority => 0,
            Self::PackageValidation => 1,
            Self::BuildImportAccounting => 2,
            Self::RuntimeGpu => 3,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrimitiveObservation {
    pub(crate) gate_ordinal: u32,
    pub(crate) gate_id: String,
    pub(crate) authority_path: String,
    pub(crate) unit: Unit,
    pub(crate) tie_key: TieKey,
    pub(crate) numerator: String,
    pub(crate) denominator: String,
    pub(crate) source_commitments: Vec<PrimitiveSourceCommitment>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GateRow {
    pub(crate) gate_ordinal: u32,
    pub(crate) gate_id: String,
    pub(crate) authority_path: String,
    pub(crate) comparator: Comparator,
    pub(crate) unit: Unit,
    pub(crate) numerator: String,
    pub(crate) denominator: String,
    pub(crate) limit: String,
    pub(crate) passed: bool,
    pub(crate) reason_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GateObservationArtifact {
    schema: String,
    selection_authority_sha256: String,
    candidate_edge: u32,
    package_set_generation_sha256: String,
    successor_executable_sha256: String,
    primitive_observations: Vec<PrimitiveObservation>,
    selected_gate_rows: Vec<GateRow>,
    observations_complete: bool,
    candidate_pass: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecomputedGateArtifact {
    pub(crate) candidate_edge: u32,
    pub(crate) package_set_generation_sha256: String,
    pub(crate) successor_executable_sha256: String,
    pub(crate) gate_rows: Vec<GateRow>,
    pub(crate) candidate_pass: bool,
}

#[derive(Clone, Copy)]
struct BoundSchema {
    id: &'static str,
    bytes: &'static [u8],
    declaration_pointers: &'static [(&'static str, &'static str)],
}

const COMMON_DECLARATIONS: &[(&str, &str)] = &[
    (
        "/trace_derivation/projection_input_sidecar_common_schema_path",
        "/trace_derivation/projection_input_sidecar_common_schema_sha256",
    ),
    (
        "/evidence_contract/artifact_common_schema_binding/schema_path",
        "/evidence_contract/artifact_common_schema_binding/schema_sha256",
    ),
    (
        "/evidence_contract/failure_evidence_schema_binding/common_schema_path",
        "/evidence_contract/failure_evidence_schema_binding/common_schema_sha256",
    ),
    (
        "/evidence_contract/source_inventory_schema_binding/common_schema_path",
        "/evidence_contract/source_inventory_schema_binding/common_schema_sha256",
    ),
];

const SCHEMAS: &[BoundSchema] = &[
    BoundSchema {
        id: "viewer-performance-ep01-common.schema.json",
        bytes: COMMON_SCHEMA_BYTES,
        declaration_pointers: COMMON_DECLARATIONS,
    },
    BoundSchema {
        id: "viewer-performance-ep01-failure-evidence.schema.json",
        bytes: FAILURE_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/failure_evidence_schema_binding/schema_path",
            "/evidence_contract/failure_evidence_schema_binding/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-projection-input.schema.json",
        bytes: PROJECTION_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/trace_derivation/projection_input_sidecar_schema_path",
            "/trace_derivation/projection_input_sidecar_schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-source-inventory.schema.json",
        bytes: SOURCE_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/source_inventory_schema_binding/schema_path",
            "/evidence_contract/source_inventory_schema_binding/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-package-validation.schema.json",
        bytes: PACKAGE_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/artifact_schema_bindings/0/schema_path",
            "/evidence_contract/artifact_schema_bindings/0/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-build-import.schema.json",
        bytes: BUILD_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/artifact_schema_bindings/1/schema_path",
            "/evidence_contract/artifact_schema_bindings/1/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-trace.schema.json",
        bytes: TRACE_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/artifact_schema_bindings/2/schema_path",
            "/evidence_contract/artifact_schema_bindings/2/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-runtime-gpu.schema.json",
        bytes: RUNTIME_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/artifact_schema_bindings/3/schema_path",
            "/evidence_contract/artifact_schema_bindings/3/schema_sha256",
        )],
    },
    BoundSchema {
        id: "viewer-performance-ep01-gate-observation.schema.json",
        bytes: OBSERVATION_SCHEMA_BYTES,
        declaration_pointers: &[(
            "/evidence_contract/artifact_schema_bindings/4/schema_path",
            "/evidence_contract/artifact_schema_bindings/4/schema_sha256",
        )],
    },
];

impl Ep01Authority {
    pub(crate) fn preflight(
        profile: &Value,
        binding: &Ep01PreflightBinding,
    ) -> anyhow::Result<Self> {
        validate_preflight_binding(binding)?;
        let authority_sha256 = Sha256Hasher::digest(AUTHORITY_BYTES).to_string();
        if authority_sha256 != AUTHORITY_SHA256
            || binding.selection_authority_sha256 != authority_sha256
        {
            bail!("EP-01 selection authority binding is invalid");
        }

        let authority: Value = serde_json::from_slice(AUTHORITY_BYTES)
            .context("parse embedded EP-01 selection authority")?;
        if authority.pointer("/schema").and_then(Value::as_str)
            != Some("mirante4d-viewer-performance-ep01-selection-authority")
            || authority.pointer("/schema_version").and_then(Value::as_u64) != Some(5)
        {
            bail!("EP-01 selection authority schema is invalid");
        }
        validate_bound_schemas(&authority)?;
        let gates = compile_gate_registry(&authority, profile)?;
        Ok(Self {
            authority_sha256,
            gates,
        })
    }

    pub(crate) fn authority_sha256(&self) -> &str {
        &self.authority_sha256
    }

    pub(crate) fn gates(&self) -> &[GateDefinition] {
        &self.gates
    }

    pub(crate) fn evaluate_primitives(
        &self,
        primitives: &[PrimitiveObservation],
    ) -> anyhow::Result<Vec<GateRow>> {
        if !(GATE_COUNT..=MAX_PRIMITIVE_OBSERVATIONS).contains(&primitives.len()) {
            bail!("EP-01 primitive observation population is invalid");
        }

        let mut previous: Option<(u32, ParsedTieKey)> = None;
        let mut grouped: Vec<Vec<ParsedPrimitive<'_>>> =
            (0..GATE_COUNT).map(|_| Vec::new()).collect();
        for primitive in primitives {
            let ordinal =
                usize::try_from(primitive.gate_ordinal).context("convert EP-01 gate ordinal")?;
            let gate = self
                .gates
                .get(ordinal)
                .context("EP-01 primitive gate ordinal is out of range")?;
            validate_primitive_identity(primitive, gate)?;
            let tie_key = ParsedTieKey::parse(&primitive.tie_key)?;
            let sort_key = (primitive.gate_ordinal, tie_key);
            if previous.as_ref().is_some_and(|prior| prior >= &sort_key) {
                bail!("EP-01 primitive observations are not in strict canonical order");
            }
            previous = Some(sort_key.clone());
            let numerator = canonical_u128(&primitive.numerator, "primitive numerator")?;
            let denominator = canonical_u128(&primitive.denominator, "primitive denominator")?;
            if denominator == 0 || (!gate.raw_ratio && denominator != 1) {
                bail!("EP-01 primitive denominator is invalid");
            }
            validate_source_commitments(
                primitive.gate_ordinal,
                &primitive.source_commitments,
                &self.authority_sha256,
            )?;
            grouped[ordinal].push(ParsedPrimitive {
                wire: primitive,
                tie_key: sort_key.1,
                numerator,
                denominator,
            });
        }

        let mut rows = Vec::with_capacity(GATE_COUNT);
        for (gate, population) in self.gates.iter().zip(grouped.iter()) {
            if population.is_empty() {
                bail!("EP-01 gate {} has no primitive observation", gate.gate_id);
            }
            let operand = aggregate_population(gate, population)?;
            let passed = compare_gate(gate, operand.numerator, operand.denominator)?;
            rows.push(GateRow {
                gate_ordinal: gate.ordinal,
                gate_id: gate.gate_id.clone(),
                authority_path: gate.authority_path.clone(),
                comparator: gate.comparator,
                unit: gate.unit,
                numerator: operand.numerator.to_string(),
                denominator: operand.denominator.to_string(),
                limit: gate.limit.to_string(),
                passed,
                reason_code: (!passed).then(|| format!("{}-comparison-failed", gate.gate_id)),
            });
        }
        Ok(rows)
    }

    pub(crate) fn recompute_gate_observation_artifact(
        &self,
        bytes: &[u8],
        expected_edge: u32,
        expected_package_set_generation_sha256: &str,
        expected_successor_executable_sha256: &str,
    ) -> anyhow::Result<RecomputedGateArtifact> {
        let value = parse_restricted_jcs(bytes, MAX_ARTIFACT_BYTES)?;
        let artifact: GateObservationArtifact =
            serde_json::from_value(value).context("decode EP-01 gate observation artifact")?;
        if artifact.schema != "mirante4d-viewer-performance-ep01-gate-observation-evidence-1"
            || artifact.selection_authority_sha256 != self.authority_sha256
            || artifact.candidate_edge != expected_edge
            || !matches!(expected_edge, 32 | 64)
            || artifact.package_set_generation_sha256 != expected_package_set_generation_sha256
            || artifact.successor_executable_sha256 != expected_successor_executable_sha256
            || !artifact.observations_complete
        {
            bail!("EP-01 gate observation artifact binding is invalid");
        }
        validate_sha256(
            &artifact.package_set_generation_sha256,
            "package-set generation",
        )?;
        validate_sha256(
            &artifact.successor_executable_sha256,
            "successor executable",
        )?;

        let recomputed = self.evaluate_primitives(&artifact.primitive_observations)?;
        if artifact.selected_gate_rows != recomputed {
            bail!("EP-01 selected gate rows do not match exact recomputation");
        }
        let candidate_pass = recomputed.iter().all(|row| row.passed);
        if artifact.candidate_pass != candidate_pass {
            bail!("EP-01 candidate pass does not match all 74 gate rows");
        }
        Ok(RecomputedGateArtifact {
            candidate_edge: expected_edge,
            package_set_generation_sha256: artifact.package_set_generation_sha256,
            successor_executable_sha256: artifact.successor_executable_sha256,
            gate_rows: recomputed,
            candidate_pass,
        })
    }
}

pub(crate) struct StrictArtifactClosure {
    _private: (),
}

#[cfg(test)]
pub(crate) fn strict_artifact_closure_for_tests() -> StrictArtifactClosure {
    StrictArtifactClosure { _private: () }
}

pub(crate) fn select_candidate(
    _strict_closure: &StrictArtifactClosure,
    candidates: &[RecomputedGateArtifact],
) -> anyhow::Result<Option<u32>> {
    if candidates.len() != 2
        || candidates[0].candidate_edge != 32
        || candidates[1].candidate_edge != 64
    {
        bail!("EP-01 candidates must be complete and ordered 32 then 64");
    }
    if candidates
        .iter()
        .any(|candidate| candidate.gate_rows.len() != GATE_COUNT)
    {
        bail!("EP-01 candidate gate population is incomplete");
    }
    Ok(if candidates[0].candidate_pass {
        Some(32)
    } else if candidates[1].candidate_pass {
        Some(64)
    } else {
        None
    })
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ParsedTieKey {
    package_role_tag: u8,
    trace_family_tag: u8,
    state_ordinal: u64,
    protocol_sample_ordinal: u64,
    within_state_observation_ordinal: u64,
}

impl ParsedTieKey {
    fn parse(value: &TieKey) -> anyhow::Result<Self> {
        Ok(Self {
            package_role_tag: value.package_role_tag,
            trace_family_tag: value.trace_family_tag,
            state_ordinal: canonical_u64(&value.state_ordinal, "tie state ordinal")?,
            protocol_sample_ordinal: canonical_u64(
                &value.protocol_sample_ordinal,
                "tie protocol sample ordinal",
            )?,
            within_state_observation_ordinal: canonical_u64(
                &value.within_state_observation_ordinal,
                "tie within-state ordinal",
            )?,
        })
    }
}

struct ParsedPrimitive<'a> {
    #[allow(dead_code)]
    wire: &'a PrimitiveObservation,
    tie_key: ParsedTieKey,
    numerator: u128,
    denominator: u128,
}

fn validate_preflight_binding(binding: &Ep01PreflightBinding) -> anyhow::Result<()> {
    if !binding.clean_tree || !binding.qualification_binding_verified {
        bail!("EP-01 requires a clean, qualification-admitted revision");
    }
    let revision = binding.repository_revision.as_bytes();
    if !matches!(revision.len(), 40 | 64)
        || !revision
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        bail!("EP-01 repository revision binding is invalid");
    }
    validate_sha256(&binding.selection_authority_sha256, "selection authority")
}

fn validate_bound_schemas(authority: &Value) -> anyhow::Result<()> {
    for schema in SCHEMAS {
        if schema.bytes.is_empty()
            || schema.bytes.len() > 1_048_576
            || schema.bytes.starts_with(&[0xef, 0xbb, 0xbf])
        {
            bail!("EP-01 bound schema document is not admitted");
        }
        let value: Value = serde_json::from_slice(schema.bytes)
            .with_context(|| format!("parse EP-01 schema {}", schema.id))?;
        if value.pointer("/$schema").and_then(Value::as_str)
            != Some("https://json-schema.org/draft/2020-12/schema")
            || value.pointer("/$id").and_then(Value::as_str) != Some(schema.id)
        {
            bail!("EP-01 bound schema identity is invalid");
        }
        let digest = Sha256Hasher::digest(schema.bytes).to_string();
        for (path_pointer, hash_pointer) in schema.declaration_pointers {
            let expected_path = format!("verification/schemas/{}", schema.id);
            if authority.pointer(path_pointer).and_then(Value::as_str)
                != Some(expected_path.as_str())
                || authority.pointer(hash_pointer).and_then(Value::as_str) != Some(digest.as_str())
            {
                bail!("EP-01 bound schema commitment is invalid");
            }
        }
    }
    Ok(())
}

fn compile_gate_registry(
    authority: &Value,
    profile: &Value,
) -> anyhow::Result<Vec<GateDefinition>> {
    let registry = string_array(authority, "/gate_observation_contract/registry")?;
    if registry.len() != GATE_COUNT {
        bail!("EP-01 gate registry must contain exactly 74 rows");
    }
    let headroom = string_set(authority, "/comparator_partition/headroom_paths")?;
    let engineering = string_set(
        authority,
        "/comparator_partition/engineering_ratio_direct_paths",
    )?;
    let exact = string_set(authority, "/comparator_partition/structural_exact_eq_paths")?;
    let direct = string_set(authority, "/comparator_partition/structural_lte_paths")?;
    let zero = string_set(authority, "/comparator_partition/structural_zero_eq_paths")?;
    let raw_ratios = string_set(authority, "/gate_observation_contract/raw_ratio_paths")?;
    let mut partition_union = BTreeSet::new();
    for partition in [&headroom, &engineering, &exact, &direct, &zero] {
        for path in partition {
            if !partition_union.insert(path.clone()) {
                bail!("EP-01 comparator partition overlaps");
            }
        }
    }
    if partition_union.len() != GATE_COUNT {
        bail!("EP-01 comparator partition is not exhaustive");
    }

    let mut gates = Vec::with_capacity(GATE_COUNT);
    for (ordinal, encoded) in registry.iter().enumerate() {
        let parts = encoded.split('|').collect::<Vec<_>>();
        if parts.len() != 5 || parts.iter().any(|part| part.is_empty()) {
            bail!("EP-01 gate registry row is malformed");
        }
        let authority_path = parts[0].to_owned();
        if !partition_union.contains(&authority_path) {
            bail!("EP-01 gate registry path is not partitioned");
        }
        let unit = parse_unit(parts[1])?;
        let aggregation = match parts[3] {
            "max" | "configured_max" => Aggregation::MaximumRatio,
            "nearest_rank_p95" => Aggregation::NearestRankP95,
            "exact" => Aggregation::Exact,
            _ => bail!("EP-01 gate aggregation is unsupported"),
        };
        let comparator = if headroom.contains(&authority_path) {
            Comparator::HeadroomLte
        } else if engineering.contains(&authority_path) || direct.contains(&authority_path) {
            Comparator::DirectLte
        } else if exact.contains(&authority_path) {
            Comparator::ExactEq
        } else if zero.contains(&authority_path) {
            Comparator::ZeroEq
        } else {
            bail!("EP-01 comparator partition is incomplete");
        };
        let limit = resolve_limit(authority, profile, &authority_path)?;
        let ordinal = u32::try_from(ordinal).context("convert EP-01 gate ordinal")?;
        gates.push(GateDefinition {
            ordinal,
            gate_id: format!("EP01-G{ordinal:03}"),
            authority_path,
            unit,
            scope: parts[2].to_owned(),
            aggregation,
            comparator,
            limit,
            raw_ratio: raw_ratios.contains(parts[0]),
        });
    }
    Ok(gates)
}

fn resolve_limit(authority: &Value, profile: &Value, path: &str) -> anyhow::Result<u128> {
    let root = if path.starts_with("gates.") {
        authority
    } else {
        profile
    };
    let mut current = root;
    for component in path.split('.') {
        current = current
            .as_object()
            .and_then(|object| object.get(component))
            .with_context(|| format!("EP-01 limit path {path} is missing"))?;
    }
    if let Some(value) = current.as_u64() {
        return Ok(u128::from(value));
    }
    if let Some(value) = current.as_str() {
        return canonical_u128(value, "gate limit");
    }
    bail!("EP-01 gate limit is not an unsigned integer")
}

fn validate_primitive_identity(
    primitive: &PrimitiveObservation,
    gate: &GateDefinition,
) -> anyhow::Result<()> {
    if primitive.gate_id != gate.gate_id
        || primitive.authority_path != gate.authority_path
        || primitive.unit != gate.unit
    {
        bail!("EP-01 primitive observation registry identity is invalid");
    }
    Ok(())
}

fn validate_source_commitments(
    gate_ordinal: u32,
    commitments: &[PrimitiveSourceCommitment],
    authority_sha256: &str,
) -> anyhow::Result<()> {
    let allowed = allowed_sources(gate_ordinal)?;
    let actual = commitments
        .iter()
        .map(|commitment| commitment.source_kind)
        .collect::<Vec<_>>();
    let source_match = match allowed {
        AllowedSources::Exactly(expected) => actual == expected,
        AllowedSources::OneOf(first, second) => {
            actual.as_slice() == [first] || actual.as_slice() == [second]
        }
    };
    if !source_match {
        bail!("EP-01 primitive source mapping is invalid");
    }

    let mut previous: Option<(u8, Vec<u8>, Vec<u8>)> = None;
    for commitment in commitments {
        if commitment.source_kind_tag != commitment.source_kind.tag() {
            bail!("EP-01 primitive source kind tag is invalid");
        }
        validate_sha256(&commitment.source_artifact_sha256, "source artifact")?;
        validate_json_pointer(&commitment.source_field_path)?;
        if commitment.source_kind == PrimitiveSourceKind::SelectionAuthority
            && commitment.source_artifact_sha256 != authority_sha256
        {
            bail!("EP-01 selection-authority source commitment is stale");
        }
        let key = (
            commitment.source_kind_tag,
            commitment.source_artifact_sha256.as_bytes().to_vec(),
            commitment.source_field_path.as_bytes().to_vec(),
        );
        if previous.as_ref().is_some_and(|prior| prior >= &key) {
            bail!("EP-01 primitive source commitments are not strictly ordered");
        }
        previous = Some(key);
    }
    Ok(())
}

enum AllowedSources {
    Exactly(Vec<PrimitiveSourceKind>),
    OneOf(PrimitiveSourceKind, PrimitiveSourceKind),
}

fn allowed_sources(gate: u32) -> anyhow::Result<AllowedSources> {
    use PrimitiveSourceKind::{
        BuildImportAccounting as Build, PackageValidation as Package, RuntimeGpu as Runtime,
        SelectionAuthority as Selection,
    };
    Ok(match gate {
        0 | 6 => AllowedSources::OneOf(Build, Runtime),
        1..=5 | 7..=19 | 25 | 30..=34 | 53..=73 => AllowedSources::Exactly(vec![Runtime]),
        20..=21 | 26..=28 | 37..=42 => AllowedSources::Exactly(vec![Package]),
        22..=24 | 43..=52 => AllowedSources::Exactly(vec![Build]),
        29 => AllowedSources::Exactly(vec![Package, Build]),
        35..=36 => AllowedSources::Exactly(vec![Selection]),
        _ => bail!("EP-01 gate source mapping ordinal is invalid"),
    })
}

fn aggregate_population<'a>(
    gate: &GateDefinition,
    population: &'a [ParsedPrimitive<'a>],
) -> anyhow::Result<&'a ParsedPrimitive<'a>> {
    match gate.aggregation {
        Aggregation::Exact => {
            if population.len() != 1 {
                bail!("EP-01 exact gate must have exactly one observation");
            }
            Ok(&population[0])
        }
        Aggregation::MaximumRatio => {
            let mut selected = &population[0];
            for candidate in &population[1..] {
                match compare_ratios(candidate, selected)? {
                    std::cmp::Ordering::Greater => selected = candidate,
                    std::cmp::Ordering::Equal if candidate.tie_key < selected.tie_key => {
                        selected = candidate;
                    }
                    _ => {}
                }
            }
            Ok(selected)
        }
        Aggregation::NearestRankP95 => {
            if population.iter().any(|sample| sample.denominator != 1) {
                bail!("EP-01 p95 samples must have denominator one");
            }
            let mut ordered = population.iter().collect::<Vec<_>>();
            ordered.sort_by(|left, right| {
                left.numerator
                    .cmp(&right.numerator)
                    .then_with(|| left.tie_key.cmp(&right.tie_key))
            });
            let rank = population
                .len()
                .checked_mul(95)
                .and_then(|value| value.checked_add(99))
                .context("EP-01 p95 rank overflow")?
                / 100;
            Ok(ordered[rank - 1])
        }
    }
}

fn compare_ratios(
    left: &ParsedPrimitive<'_>,
    right: &ParsedPrimitive<'_>,
) -> anyhow::Result<std::cmp::Ordering> {
    let left_cross = left
        .numerator
        .checked_mul(right.denominator)
        .context("EP-01 ratio cross-product overflow")?;
    let right_cross = right
        .numerator
        .checked_mul(left.denominator)
        .context("EP-01 ratio cross-product overflow")?;
    Ok(left_cross.cmp(&right_cross))
}

fn compare_gate(gate: &GateDefinition, numerator: u128, denominator: u128) -> anyhow::Result<bool> {
    match gate.comparator {
        Comparator::HeadroomLte => Ok(numerator
            .checked_mul(10_000)
            .context("EP-01 headroom numerator overflow")?
            <= gate
                .limit
                .checked_mul(denominator)
                .and_then(|value| value.checked_mul(8_000))
                .context("EP-01 headroom limit overflow")?),
        Comparator::DirectLte if gate.unit == Unit::BasisPoints => Ok(numerator
            .checked_mul(10_000)
            .context("EP-01 basis-point numerator overflow")?
            <= gate
                .limit
                .checked_mul(denominator)
                .context("EP-01 basis-point limit overflow")?),
        Comparator::DirectLte => Ok(numerator
            <= gate
                .limit
                .checked_mul(denominator)
                .context("EP-01 direct limit overflow")?),
        Comparator::ExactEq => Ok(numerator
            == gate
                .limit
                .checked_mul(denominator)
                .context("EP-01 exact limit overflow")?),
        Comparator::ZeroEq => Ok(gate.limit == 0 && numerator == 0 && denominator == 1),
    }
}

fn parse_restricted_jcs(bytes: &[u8], maximum: usize) -> anyhow::Result<Value> {
    if bytes.is_empty()
        || bytes.len() > maximum
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.last() == Some(&b'\n')
    {
        bail!("EP-01 artifact encoding is not admitted");
    }
    let value: Value = serde_json::from_slice(bytes).context("parse EP-01 canonical JSON")?;
    validate_restricted_json_value(&value, 0)?;
    let canonical = serde_json::to_vec(&value).context("encode EP-01 canonical JSON")?;
    if canonical != bytes {
        bail!("EP-01 artifact is not restricted canonical JSON");
    }
    Ok(value)
}

fn validate_restricted_json_value(value: &Value, depth: usize) -> anyhow::Result<()> {
    if depth > 64 {
        bail!("EP-01 JSON nesting exceeds the admitted bound");
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.as_u64().is_some() => Ok(()),
        Value::Number(_) => bail!("EP-01 restricted JSON admits only unsigned JSON integers"),
        Value::Array(values) => {
            if values.len() > MAX_PRIMITIVE_OBSERVATIONS {
                bail!("EP-01 JSON array exceeds the admitted population bound");
            }
            for value in values {
                validate_restricted_json_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(object) => {
            for (key, value) in object {
                if !key.is_ascii() {
                    bail!("EP-01 restricted JSON object names must be ASCII");
                }
                validate_restricted_json_value(value, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn string_array<'a>(value: &'a Value, pointer: &str) -> anyhow::Result<Vec<&'a str>> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .with_context(|| format!("EP-01 authority array {pointer} is missing"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .with_context(|| format!("EP-01 authority array {pointer} contains a non-string"))
        })
        .collect()
}

fn string_set(value: &Value, pointer: &str) -> anyhow::Result<BTreeSet<String>> {
    let values = string_array(value, pointer)?;
    let set = values
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<BTreeSet<_>>();
    if set.len() != values.len() {
        bail!("EP-01 authority set {pointer} contains a duplicate");
    }
    Ok(set)
}

fn parse_unit(value: &str) -> anyhow::Result<Unit> {
    Ok(match value {
        "bytes" => Unit::Bytes,
        "count" => Unit::Count,
        "nanoseconds" => Unit::Nanoseconds,
        "seconds" => Unit::Seconds,
        "basis_points" => Unit::BasisPoints,
        "bytes_per_byte" => Unit::BytesPerByte,
        "requests_per_brick" => Unit::RequestsPerBrick,
        "attempts_per_brick" => Unit::AttemptsPerBrick,
        _ => bail!("EP-01 gate unit is invalid"),
    })
}

fn canonical_u64(value: &str, name: &str) -> anyhow::Result<u64> {
    let parsed = value
        .parse::<u64>()
        .with_context(|| format!("parse EP-01 {name}"))?;
    if parsed.to_string() != value {
        bail!("EP-01 {name} is not canonical unsigned decimal");
    }
    Ok(parsed)
}

fn canonical_u128(value: &str, name: &str) -> anyhow::Result<u128> {
    let parsed = value
        .parse::<u128>()
        .with_context(|| format!("parse EP-01 {name}"))?;
    if parsed.to_string() != value {
        bail!("EP-01 {name} is not canonical unsigned decimal");
    }
    Ok(parsed)
}

fn validate_sha256(value: &str, name: &str) -> anyhow::Result<()> {
    if value.len() != 64
        || !value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        bail!("EP-01 {name} SHA-256 is invalid");
    }
    Ok(())
}

fn validate_json_pointer(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 512 || !value.starts_with('/') || !value.is_ascii() {
        bail!("EP-01 primitive source field pointer is invalid");
    }
    for token in value.split('/').skip(1) {
        let bytes = token.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'~' {
                if index + 1 >= bytes.len() || !matches!(bytes[index + 1], b'0' | b'1') {
                    bail!("EP-01 primitive source field pointer escape is invalid");
                }
                index += 2;
            } else {
                if bytes[index] < 0x20 || bytes[index] == 0x7f {
                    bail!("EP-01 primitive source field pointer contains a control byte");
                }
                index += 1;
            }
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactManifestRow {
    pub(crate) logical_role_tag: u8,
    pub(crate) logical_role: ArtifactRole,
    pub(crate) candidate_edge: u32,
    pub(crate) package_role: Option<PackageRole>,
    pub(crate) relative_path: String,
    pub(crate) schema: String,
    pub(crate) bytes: String,
    pub(crate) sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactRole {
    PackageValidation,
    BuildImportAccounting,
    OrderedUniqueTrace,
    RuntimeGpu,
    GateObservations,
}

impl ArtifactRole {
    fn tag(self) -> u8 {
        match self {
            Self::PackageValidation => 0,
            Self::BuildImportAccounting => 1,
            Self::OrderedUniqueTrace => 2,
            Self::RuntimeGpu => 3,
            Self::GateObservations => 4,
        }
    }

    fn schema(self) -> &'static str {
        match self {
            Self::PackageValidation => {
                "mirante4d-viewer-performance-ep01-package-validation-evidence-1"
            }
            Self::BuildImportAccounting => {
                "mirante4d-viewer-performance-ep01-build-import-evidence-1"
            }
            Self::OrderedUniqueTrace => "mirante4d-viewer-performance-ep01-trace-evidence-1",
            Self::RuntimeGpu => "mirante4d-viewer-performance-ep01-runtime-gpu-evidence-1",
            Self::GateObservations => {
                "mirante4d-viewer-performance-ep01-gate-observation-evidence-1"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackageRole {
    RepresentativePackage,
    SupportingTemporalPackage,
}

impl PackageRole {
    fn tag(self) -> u8 {
        match self {
            Self::RepresentativePackage => 0,
            Self::SupportingTemporalPackage => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmittedArtifactEnvelope {
    pub(crate) manifest: ArtifactManifestRow,
}

impl Ep01Authority {
    pub(crate) fn admit_artifact_envelope(
        &self,
        manifest: ArtifactManifestRow,
        bytes: &[u8],
    ) -> anyhow::Result<AdmittedArtifactEnvelope> {
        validate_artifact_manifest_row(&manifest)?;
        if canonical_u64(&manifest.bytes, "artifact byte count")?
            != u64::try_from(bytes.len()).context("convert EP-01 artifact length")?
            || Sha256Hasher::digest(bytes).to_string() != manifest.sha256
        {
            bail!("EP-01 artifact bytes do not match their manifest row");
        }
        let value = parse_restricted_jcs(bytes, MAX_ARTIFACT_BYTES)?;
        validate_artifact_envelope_fields(manifest.logical_role, &value)?;
        if value.pointer("/schema").and_then(Value::as_str) != Some(manifest.schema.as_str())
            || value
                .pointer("/selection_authority_sha256")
                .and_then(Value::as_str)
                != Some(self.authority_sha256.as_str())
            || value.pointer("/candidate_edge").and_then(Value::as_u64)
                != Some(u64::from(manifest.candidate_edge))
        {
            bail!("EP-01 artifact payload binding is invalid");
        }
        match manifest.logical_role {
            ArtifactRole::PackageValidation => {
                let role = value
                    .pointer("/package_role")
                    .and_then(Value::as_str)
                    .context("EP-01 package-validation role is missing")?;
                if Some(role) != manifest.package_role.map(package_role_name) {
                    bail!("EP-01 package-validation role binding is invalid");
                }
            }
            ArtifactRole::BuildImportAccounting => {
                require_true(&value, "/candidate_complete")?;
                validate_tagged_roles(&value, "/roles")?;
            }
            ArtifactRole::OrderedUniqueTrace => validate_trace_families(&value)?,
            ArtifactRole::RuntimeGpu => require_true(&value, "/resource_ledger_closed")?,
            ArtifactRole::GateObservations => require_true(&value, "/observations_complete")?,
        }
        Ok(AdmittedArtifactEnvelope { manifest })
    }
}

pub(crate) fn validate_artifact_manifest_envelopes(
    artifacts: &[AdmittedArtifactEnvelope],
) -> anyhow::Result<()> {
    if artifacts.len() != 12 {
        bail!("EP-01 artifact manifest must contain exactly 12 artifacts");
    }
    let expected = expected_artifact_order();
    let mut paths = BTreeSet::new();
    for (artifact, expected) in artifacts.iter().zip(expected.iter()) {
        let row = &artifact.manifest;
        if (row.logical_role, row.candidate_edge, row.package_role) != *expected {
            bail!("EP-01 artifact manifest order or cardinality is invalid");
        }
        if !paths.insert(row.relative_path.clone()) {
            bail!("EP-01 artifact manifest contains a duplicate path");
        }
    }
    Ok(())
}

fn validate_artifact_envelope_fields(role: ArtifactRole, value: &Value) -> anyhow::Result<()> {
    const PACKAGE: &[&str] = &[
        "schema",
        "selection_authority_sha256",
        "candidate_edge",
        "package_role",
        "package_external_relative_path",
        "independent_reader_executable_sha256",
        "package_id",
        "manifest_root_sha256",
        "scientific_content_id",
        "representation_recipe_id",
        "candidate_geometry_sha256",
        "candidate_content_generation_sha256",
        "source_inventory_sidecar_sha256",
        "storage_profile",
        "facts",
        "checks",
    ];
    const BUILD: &[&str] = &[
        "schema",
        "selection_authority_sha256",
        "candidate_edge",
        "roles",
        "checkpoint_contract",
        "source_inventory_sidecar_sha256",
        "candidate_complete",
    ];
    const TRACE: &[&str] = &[
        "schema",
        "selection_authority_sha256",
        "projection_input_sidecar_sha256",
        "candidate_edge",
        "package_set_generation_sha256",
        "families",
    ];
    const RUNTIME: &[&str] = &[
        "schema",
        "selection_authority_sha256",
        "candidate_edge",
        "successor_executable_sha256",
        "layout_manifest_sha256",
        "shader_modules",
        "layout_bindings",
        "adapter",
        "capacity",
        "pipeline_counters",
        "instrumentation_overhead_pairs",
        "diagnostic_variants",
        "samples",
        "resource_ledger_closed",
    ];
    const OBSERVATION: &[&str] = &[
        "schema",
        "selection_authority_sha256",
        "candidate_edge",
        "package_set_generation_sha256",
        "successor_executable_sha256",
        "primitive_observations",
        "selected_gate_rows",
        "observations_complete",
        "candidate_pass",
    ];
    let fields = match role {
        ArtifactRole::PackageValidation => PACKAGE,
        ArtifactRole::BuildImportAccounting => BUILD,
        ArtifactRole::OrderedUniqueTrace => TRACE,
        ArtifactRole::RuntimeGpu => RUNTIME,
        ArtifactRole::GateObservations => OBSERVATION,
    };
    let object = value
        .as_object()
        .context("EP-01 artifact envelope is not an object")?;
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        bail!("EP-01 artifact envelope has an unknown or missing field");
    }
    Ok(())
}

fn expected_artifact_order() -> Vec<(ArtifactRole, u32, Option<PackageRole>)> {
    let mut expected = Vec::with_capacity(12);
    for role in [
        ArtifactRole::PackageValidation,
        ArtifactRole::BuildImportAccounting,
        ArtifactRole::OrderedUniqueTrace,
        ArtifactRole::RuntimeGpu,
        ArtifactRole::GateObservations,
    ] {
        for edge in [32, 64] {
            if role == ArtifactRole::PackageValidation {
                for package in [
                    PackageRole::RepresentativePackage,
                    PackageRole::SupportingTemporalPackage,
                ] {
                    expected.push((role, edge, Some(package)));
                }
            } else {
                expected.push((role, edge, None));
            }
        }
    }
    expected
}

fn validate_artifact_manifest_row(row: &ArtifactManifestRow) -> anyhow::Result<()> {
    if row.logical_role_tag != row.logical_role.tag()
        || !matches!(row.candidate_edge, 32 | 64)
        || row.schema != row.logical_role.schema()
        || (row.logical_role == ArtifactRole::PackageValidation) != row.package_role.is_some()
    {
        bail!("EP-01 artifact manifest row identity is invalid");
    }
    validate_relative_path(&row.relative_path)?;
    validate_sha256(&row.sha256, "artifact")?;
    let _ = canonical_u64(&row.bytes, "artifact byte count")?;
    Ok(())
}

fn validate_relative_path(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 240
        || value.starts_with('/')
        || value.contains('\\')
        || value.split('/').count() > 9
        || value.split('/').any(|component| {
            component.is_empty()
                || matches!(component, "." | "..")
                || !component.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
    {
        bail!("EP-01 artifact relative path is invalid");
    }
    Ok(())
}

fn require_true(value: &Value, pointer: &str) -> anyhow::Result<()> {
    if value.pointer(pointer).and_then(Value::as_bool) != Some(true) {
        bail!("EP-01 artifact closure fact is invalid");
    }
    Ok(())
}

fn validate_tagged_roles(value: &Value, pointer: &str) -> anyhow::Result<()> {
    let roles = value
        .pointer(pointer)
        .and_then(Value::as_array)
        .context("EP-01 artifact roles are missing")?;
    if roles.len() != 2 {
        bail!("EP-01 artifact role population is invalid");
    }
    for (index, role) in roles.iter().enumerate() {
        let expected_name = if index == 0 {
            "representative_package"
        } else {
            "supporting_temporal_package"
        };
        if role.pointer("/role_tag").and_then(Value::as_u64) != Some(index as u64)
            || role.pointer("/role").and_then(Value::as_str) != Some(expected_name)
            || role
                .pointer("/publication_parent_synced")
                .and_then(Value::as_bool)
                != Some(true)
            || role
                .pointer("/independent_validation_complete")
                .and_then(Value::as_bool)
                != Some(true)
        {
            bail!("EP-01 build/import role evidence is invalid");
        }
    }
    Ok(())
}

fn validate_trace_families(value: &Value) -> anyhow::Result<()> {
    const FAMILIES: [&str; 8] = [
        "arbitrary_plane",
        "four_panel",
        "time_navigation",
        "mip",
        "dvr",
        "iso",
        "analysis",
        "verification",
    ];
    let families = value
        .pointer("/families")
        .and_then(Value::as_array)
        .context("EP-01 trace families are missing")?;
    if families.len() != FAMILIES.len() {
        bail!("EP-01 trace family population is invalid");
    }
    for (index, (family, expected)) in families.iter().zip(FAMILIES).enumerate() {
        if family.pointer("/family_tag").and_then(Value::as_u64) != Some(index as u64)
            || family.pointer("/family").and_then(Value::as_str) != Some(expected)
        {
            bail!("EP-01 trace family order is invalid");
        }
    }
    Ok(())
}

fn package_role_name(role: PackageRole) -> &'static str {
    match role {
        PackageRole::RepresentativePackage => "representative_package",
        PackageRole::SupportingTemporalPackage => "supporting_temporal_package",
    }
}

pub(crate) fn canonical_artifact_bytes<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    let value = serde_json::to_value(value).context("materialize EP-01 canonical artifact")?;
    validate_restricted_json_value(&value, 0)?;
    serde_json::to_vec(&value).context("encode EP-01 canonical artifact")
}

pub(crate) fn artifact_sha256(bytes: &[u8]) -> String {
    Sha256Hasher::digest(bytes).to_string()
}

pub(crate) fn rows_by_gate(rows: &[GateRow]) -> BTreeMap<u32, &GateRow> {
    rows.iter().map(|row| (row.gate_ordinal, row)).collect()
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceFamily {
    ArbitraryPlane,
    FourPanel,
    TimeNavigation,
    Mip,
    Dvr,
    Iso,
    Analysis,
    Verification,
}

impl TraceFamily {
    fn tag(self) -> u8 {
        match self {
            Self::ArbitraryPlane => 0,
            Self::FourPanel => 1,
            Self::TimeNavigation => 2,
            Self::Mip => 3,
            Self::Dvr => 4,
            Self::Iso => 5,
            Self::Analysis => 6,
            Self::Verification => 7,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPackage {
    pub(crate) role_tag: u8,
    pub(crate) role: PackageRole,
    pub(crate) external_relative_path: String,
    pub(crate) package_id: String,
    pub(crate) manifest_root_sha256: String,
    pub(crate) candidate_content_generation_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawTrace {
    pub(crate) family_tag: u8,
    pub(crate) family: TraceFamily,
    pub(crate) ordered_sha256: String,
    pub(crate) ordered_state_count: String,
    pub(crate) ordered_key_count: String,
    pub(crate) unique_sha256: String,
    pub(crate) unique_key_count: String,
    pub(crate) unique_payload_bytes: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawCandidate {
    pub(crate) candidate_edge: u32,
    pub(crate) package_set_generation_sha256: String,
    pub(crate) packages: Vec<RawPackage>,
    pub(crate) traces: Vec<RawTrace>,
    pub(crate) gate_rows: Vec<GateRow>,
    pub(crate) candidate_pass: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicTrace {
    pub(crate) family_tag: u8,
    pub(crate) family: TraceFamily,
    pub(crate) ordered_sha256: String,
    pub(crate) unique_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicGate {
    pub(crate) gate_ordinal: u32,
    pub(crate) gate_id: String,
    pub(crate) authority_path: String,
    pub(crate) comparator: Comparator,
    pub(crate) unit: Unit,
    pub(crate) limit: String,
    pub(crate) passed: bool,
    pub(crate) reason_code: Option<String>,
    pub(crate) external_raw_gate_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicCandidate {
    pub(crate) candidate_edge: u32,
    pub(crate) traces: Vec<PublicTrace>,
    pub(crate) gates: Vec<PublicGate>,
    pub(crate) external_raw_candidate_sha256: String,
    pub(crate) candidate_pass: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicRevision {
    pub(crate) repository_revision: String,
    pub(crate) clean_tree: bool,
    pub(crate) selection_authority_sha256: String,
    pub(crate) qualification_binding_verified: bool,
    pub(crate) source_preservation_commitment_sha256: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PublicProjection {
    pub(crate) schema: String,
    pub(crate) revision: PublicRevision,
    pub(crate) candidates: Vec<PublicCandidate>,
    pub(crate) selection: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SanitizedReceipt {
    pub(crate) schema: String,
    pub(crate) projection: PublicProjection,
    pub(crate) external_raw_receipt_sha256: String,
}

impl Ep01Authority {
    pub(crate) fn project_public_receipt(
        &self,
        _strict_closure: &StrictArtifactClosure,
        private_commitment_nonce_256: &str,
        binding: &Ep01PreflightBinding,
        source_preservation_commitment_sha256: &str,
        candidates: &[RawCandidate],
    ) -> anyhow::Result<PublicProjection> {
        validate_preflight_binding(binding)?;
        if binding.selection_authority_sha256 != self.authority_sha256 {
            bail!("EP-01 public projection authority binding is invalid");
        }
        validate_sha256(
            source_preservation_commitment_sha256,
            "source-preservation commitment",
        )?;
        let nonce = decode_sha256(private_commitment_nonce_256, "private commitment nonce")?;
        if nonce.iter().all(|byte| *byte == 0) {
            bail!("EP-01 private commitment nonce must not be all zero");
        }
        if candidates.len() != 2
            || candidates[0].candidate_edge != 32
            || candidates[1].candidate_edge != 64
        {
            bail!("EP-01 raw candidates must be complete and ordered 32 then 64");
        }

        let mut public_candidates = Vec::with_capacity(2);
        for candidate in candidates {
            validate_raw_candidate(candidate)?;
            let mut gates = Vec::with_capacity(GATE_COUNT);
            for row in &candidate.gate_rows {
                gates.push(PublicGate {
                    gate_ordinal: row.gate_ordinal,
                    gate_id: row.gate_id.clone(),
                    authority_path: row.authority_path.clone(),
                    comparator: row.comparator,
                    unit: row.unit,
                    limit: row.limit.clone(),
                    passed: row.passed,
                    reason_code: row.reason_code.clone(),
                    external_raw_gate_sha256: external_raw_gate_sha256(
                        &nonce,
                        &self.authority_sha256,
                        candidate.candidate_edge,
                        row,
                    )?,
                });
            }
            public_candidates.push(PublicCandidate {
                candidate_edge: candidate.candidate_edge,
                traces: candidate
                    .traces
                    .iter()
                    .map(|trace| PublicTrace {
                        family_tag: trace.family_tag,
                        family: trace.family,
                        ordered_sha256: trace.ordered_sha256.clone(),
                        unique_sha256: trace.unique_sha256.clone(),
                    })
                    .collect(),
                gates,
                external_raw_candidate_sha256: external_raw_candidate_sha256(
                    &nonce,
                    &self.authority_sha256,
                    candidate,
                )?,
                candidate_pass: candidate.candidate_pass,
            });
        }
        let selection = if candidates[0].candidate_pass {
            Some(32)
        } else if candidates[1].candidate_pass {
            Some(64)
        } else {
            None
        };
        Ok(PublicProjection {
            schema: "mirante4d-viewer-performance-ep01-public-projection-1".to_owned(),
            revision: PublicRevision {
                repository_revision: binding.repository_revision.clone(),
                clean_tree: true,
                selection_authority_sha256: self.authority_sha256.clone(),
                qualification_binding_verified: true,
                source_preservation_commitment_sha256: source_preservation_commitment_sha256
                    .to_owned(),
            },
            candidates: public_candidates,
            selection,
        })
    }
}

pub(crate) fn close_sanitized_receipt(
    projection: PublicProjection,
    exact_raw_receipt_bytes: &[u8],
) -> anyhow::Result<SanitizedReceipt> {
    if exact_raw_receipt_bytes.is_empty() || exact_raw_receipt_bytes.len() > MAX_ARTIFACT_BYTES {
        bail!("EP-01 raw receipt byte population is invalid");
    }
    Ok(SanitizedReceipt {
        schema: "mirante4d-viewer-performance-ep01-sanitized-receipt-1".to_owned(),
        projection,
        external_raw_receipt_sha256: Sha256Hasher::digest(exact_raw_receipt_bytes).to_string(),
    })
}

fn validate_raw_candidate(candidate: &RawCandidate) -> anyhow::Result<()> {
    if !matches!(candidate.candidate_edge, 32 | 64) {
        bail!("EP-01 raw candidate edge is invalid");
    }
    validate_sha256(
        &candidate.package_set_generation_sha256,
        "candidate package-set generation",
    )?;
    if candidate.packages.len() != 2 {
        bail!("EP-01 raw candidate package population is invalid");
    }
    for (index, package) in candidate.packages.iter().enumerate() {
        let expected_role = if index == 0 {
            PackageRole::RepresentativePackage
        } else {
            PackageRole::SupportingTemporalPackage
        };
        if package.role_tag != expected_role.tag() || package.role != expected_role {
            bail!("EP-01 raw candidate package order is invalid");
        }
        validate_relative_path(&package.external_relative_path)?;
        validate_package_id(&package.package_id)?;
        validate_sha256(&package.manifest_root_sha256, "package manifest root")?;
        validate_sha256(
            &package.candidate_content_generation_sha256,
            "candidate content generation",
        )?;
    }
    if candidate.traces.len() != 8 {
        bail!("EP-01 raw candidate trace population is invalid");
    }
    for (index, trace) in candidate.traces.iter().enumerate() {
        if trace.family_tag != trace.family.tag() || usize::from(trace.family_tag) != index {
            bail!("EP-01 raw candidate trace order is invalid");
        }
        validate_sha256(&trace.ordered_sha256, "ordered trace")?;
        validate_sha256(&trace.unique_sha256, "unique trace")?;
        let _ = canonical_u64(&trace.ordered_state_count, "ordered trace state count")?;
        let _ = canonical_u64(&trace.ordered_key_count, "ordered trace key count")?;
        let _ = canonical_u64(&trace.unique_key_count, "unique trace key count")?;
        let _ = canonical_u64(&trace.unique_payload_bytes, "unique trace payload bytes")?;
    }
    if candidate.gate_rows.len() != GATE_COUNT {
        bail!("EP-01 raw candidate gate population is invalid");
    }
    for (index, row) in candidate.gate_rows.iter().enumerate() {
        if usize::try_from(row.gate_ordinal).ok() != Some(index)
            || row.gate_id != format!("EP01-G{index:03}")
            || canonical_u128(&row.numerator, "raw gate numerator").is_err()
            || canonical_u128(&row.denominator, "raw gate denominator").is_err()
            || canonical_u128(&row.limit, "raw gate limit").is_err()
        {
            bail!("EP-01 raw candidate gate row is invalid");
        }
        let expected_reason = (!row.passed).then(|| format!("{}-comparison-failed", row.gate_id));
        if row.reason_code != expected_reason {
            bail!("EP-01 raw candidate gate reason is invalid");
        }
    }
    if candidate.candidate_pass != candidate.gate_rows.iter().all(|row| row.passed) {
        bail!("EP-01 raw candidate pass is not the conjunction of all gates");
    }
    Ok(())
}

fn external_raw_gate_sha256(
    nonce: &[u8; 32],
    authority_sha256: &str,
    edge: u32,
    row: &GateRow,
) -> anyhow::Result<String> {
    let authority = decode_sha256(authority_sha256, "selection authority")?;
    let encoded = canonical_artifact_bytes(row)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-ep01-raw-gate-v2\0");
    hasher.update(nonce);
    hasher.update(authority);
    hasher.update(edge.to_le_bytes());
    hasher.update(row.gate_ordinal.to_le_bytes());
    hasher.update(encoded);
    Ok(hasher.finalize().to_string())
}

fn external_raw_candidate_sha256(
    nonce: &[u8; 32],
    authority_sha256: &str,
    candidate: &RawCandidate,
) -> anyhow::Result<String> {
    let authority = decode_sha256(authority_sha256, "selection authority")?;
    let encoded = canonical_artifact_bytes(candidate)?;
    let mut hasher = Sha256Hasher::new();
    hasher.update(b"mirante4d-ep01-raw-candidate-v2\0");
    hasher.update(nonce);
    hasher.update(authority);
    hasher.update(candidate.candidate_edge.to_le_bytes());
    hasher.update(encoded);
    Ok(hasher.finalize().to_string())
}

fn decode_sha256(value: &str, name: &str) -> anyhow::Result<[u8; 32]> {
    validate_sha256(value, name)?;
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        let high = decode_hex(value.as_bytes()[index * 2])?;
        let low = decode_hex(value.as_bytes()[index * 2 + 1])?;
        *output = high << 4 | low;
    }
    Ok(bytes)
}

fn decode_hex(value: u8) -> anyhow::Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => bail!("EP-01 lowercase hexadecimal value is invalid"),
    }
}

fn validate_package_id(value: &str) -> anyhow::Result<()> {
    let digest = value
        .strip_prefix("m4d-package-v1-sha256:")
        .context("EP-01 candidate PackageId prefix is invalid")?;
    validate_sha256(digest, "candidate PackageId")
}
