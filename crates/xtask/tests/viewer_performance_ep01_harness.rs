#![allow(dead_code)]

use serde_json::{Map, Value, json};

#[path = "../src/viewer_performance/ep01_harness.rs"]
mod harness;

use harness::{
    AdmittedArtifactEnvelope, ArtifactManifestRow, ArtifactRole, Comparator, Ep01Authority,
    Ep01PreflightBinding, PackageRole, PrimitiveObservation, PrimitiveSourceCommitment,
    PrimitiveSourceKind, RecomputedGateArtifact, TieKey, artifact_sha256, canonical_artifact_bytes,
    select_candidate, strict_artifact_closure_for_tests, validate_artifact_manifest_envelopes,
};

const AUTHORITY: &[u8] =
    include_bytes!("../../../verification/viewer-performance-ep01-selection.json");

fn admitted_authority() -> Ep01Authority {
    let authority: Value = serde_json::from_slice(AUTHORITY).unwrap();
    let mut profile = Value::Object(Map::new());
    for row in authority["gate_observation_contract"]["registry"]
        .as_array()
        .unwrap()
    {
        let path = row.as_str().unwrap().split('|').next().unwrap();
        if !path.starts_with("gates.") {
            insert_path(&mut profile, path, json!(1_000_000_u64));
        }
    }
    Ep01Authority::preflight(
        &profile,
        &Ep01PreflightBinding {
            repository_revision: "a".repeat(40),
            clean_tree: true,
            qualification_binding_verified: true,
            selection_authority_sha256: artifact_sha256(AUTHORITY),
        },
    )
    .unwrap()
}

fn insert_path(root: &mut Value, path: &str, value: Value) {
    let components = path.split('.').collect::<Vec<_>>();
    let mut current = root;
    for component in &components[..components.len() - 1] {
        let object = current.as_object_mut().unwrap();
        current = object
            .entry((*component).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
    }
    current
        .as_object_mut()
        .unwrap()
        .insert(components.last().unwrap().to_string(), value);
}

fn source_commitments(gate: u32, authority_sha256: &str) -> Vec<PrimitiveSourceCommitment> {
    let one = |kind: PrimitiveSourceKind, byte: char| PrimitiveSourceCommitment {
        source_kind_tag: match kind {
            PrimitiveSourceKind::SelectionAuthority => 0,
            PrimitiveSourceKind::PackageValidation => 1,
            PrimitiveSourceKind::BuildImportAccounting => 2,
            PrimitiveSourceKind::RuntimeGpu => 3,
        },
        source_kind: kind,
        source_artifact_sha256: if kind == PrimitiveSourceKind::SelectionAuthority {
            authority_sha256.to_owned()
        } else {
            byte.to_string().repeat(64)
        },
        source_field_path: if kind == PrimitiveSourceKind::SelectionAuthority {
            format!(
                "/gates/headroom/minimum_{}_basis_points",
                if gate == 35 { "latency" } else { "resource" }
            )
        } else {
            "/facts/value".to_owned()
        },
    };
    match gate {
        0 | 6 | 22..=24 | 43..=52 => vec![one(PrimitiveSourceKind::BuildImportAccounting, 'c')],
        1..=19 | 25 | 30..=34 | 53..=73 => {
            vec![one(PrimitiveSourceKind::RuntimeGpu, 'd')]
        }
        20..=21 | 26..=28 | 37..=42 => {
            vec![one(PrimitiveSourceKind::PackageValidation, 'b')]
        }
        29 => vec![
            one(PrimitiveSourceKind::PackageValidation, 'b'),
            one(PrimitiveSourceKind::BuildImportAccounting, 'c'),
        ],
        35..=36 => vec![one(PrimitiveSourceKind::SelectionAuthority, 'a')],
        _ => unreachable!(),
    }
}

fn passing_primitives(authority: &Ep01Authority) -> Vec<PrimitiveObservation> {
    authority
        .gates()
        .iter()
        .map(|gate| PrimitiveObservation {
            gate_ordinal: gate.ordinal,
            gate_id: gate.gate_id.clone(),
            authority_path: gate.authority_path.clone(),
            unit: gate.unit,
            tie_key: TieKey {
                package_role_tag: 255,
                trace_family_tag: 255,
                state_ordinal: u64::MAX.to_string(),
                protocol_sample_ordinal: u64::MAX.to_string(),
                within_state_observation_ordinal: "0".to_owned(),
            },
            numerator: match gate.comparator {
                Comparator::ExactEq => gate.limit.to_string(),
                Comparator::HeadroomLte | Comparator::DirectLte | Comparator::ZeroEq => {
                    "0".to_owned()
                }
            },
            denominator: "1".to_owned(),
            source_commitments: source_commitments(gate.ordinal, authority.authority_sha256()),
        })
        .collect()
}

fn gate_artifact_bytes(
    authority: &Ep01Authority,
    primitives: &[PrimitiveObservation],
    selected_rows: Option<Value>,
) -> Vec<u8> {
    let rows = authority.evaluate_primitives(primitives).unwrap();
    canonical_artifact_bytes(&json!({
        "schema": "mirante4d-viewer-performance-ep01-gate-observation-evidence-1",
        "selection_authority_sha256": authority.authority_sha256(),
        "candidate_edge": 32,
        "package_set_generation_sha256": "e".repeat(64),
        "successor_executable_sha256": "f".repeat(64),
        "primitive_observations": primitives,
        "selected_gate_rows": selected_rows.unwrap_or_else(|| serde_json::to_value(rows).unwrap()),
        "observations_complete": true,
        "candidate_pass": true
    }))
    .unwrap()
}

#[test]
fn preflight_compiles_the_exact_bound_gate_population() {
    let authority = admitted_authority();
    assert_eq!(authority.gates().len(), 74);
    for (ordinal, gate) in authority.gates().iter().enumerate() {
        assert_eq!(gate.ordinal as usize, ordinal);
        assert_eq!(gate.gate_id, format!("EP01-G{ordinal:03}"));
        assert!(!gate.scope.is_empty());
    }
}

#[test]
fn preflight_rejects_dirty_stale_or_unverified_bindings() {
    let authority: Value = serde_json::from_slice(AUTHORITY).unwrap();
    let mut profile = Value::Object(Map::new());
    for row in authority["gate_observation_contract"]["registry"]
        .as_array()
        .unwrap()
    {
        let path = row.as_str().unwrap().split('|').next().unwrap();
        if !path.starts_with("gates.") {
            insert_path(&mut profile, path, json!(1_u64));
        }
    }
    let baseline = Ep01PreflightBinding {
        repository_revision: "a".repeat(40),
        clean_tree: true,
        qualification_binding_verified: true,
        selection_authority_sha256: artifact_sha256(AUTHORITY),
    };
    let mut dirty = baseline.clone();
    dirty.clean_tree = false;
    assert!(Ep01Authority::preflight(&profile, &dirty).is_err());
    let mut unverified = baseline.clone();
    unverified.qualification_binding_verified = false;
    assert!(Ep01Authority::preflight(&profile, &unverified).is_err());
    let mut stale = baseline;
    stale.selection_authority_sha256 = "0".repeat(64);
    assert!(Ep01Authority::preflight(&profile, &stale).is_err());
}

#[test]
fn complete_gate_artifact_recomputes_all_rows_and_pass_status() {
    let authority = admitted_authority();
    let primitives = passing_primitives(&authority);
    let bytes = gate_artifact_bytes(&authority, &primitives, None);
    let candidate = authority
        .recompute_gate_observation_artifact(&bytes, 32, &"e".repeat(64), &"f".repeat(64))
        .unwrap();
    assert_eq!(candidate.gate_rows.len(), 74);
    assert!(candidate.candidate_pass);
}

#[test]
fn malformed_population_never_becomes_a_failed_selectable_gate() {
    let authority = admitted_authority();
    let mut missing = passing_primitives(&authority);
    missing.remove(10);
    assert!(authority.evaluate_primitives(&missing).is_err());

    let mut duplicate = passing_primitives(&authority);
    duplicate.insert(1, duplicate[0].clone());
    assert!(authority.evaluate_primitives(&duplicate).is_err());

    let mut reordered = passing_primitives(&authority);
    reordered.swap(0, 1);
    assert!(authority.evaluate_primitives(&reordered).is_err());

    let mut zero_denominator = passing_primitives(&authority);
    zero_denominator[0].denominator = "0".to_owned();
    assert!(authority.evaluate_primitives(&zero_denominator).is_err());
}

#[test]
fn exact_ratio_aggregation_and_p95_are_order_independent_but_input_order_is_not() {
    let authority = admitted_authority();
    let mut primitives = passing_primitives(&authority);
    let ratio_index = primitives
        .iter()
        .position(|primitive| primitive.gate_ordinal == 25)
        .unwrap();
    let mut second_max = primitives[ratio_index].clone();
    second_max.tie_key.within_state_observation_ordinal = "1".to_owned();
    second_max.numerator = "3".to_owned();
    second_max.denominator = "2".to_owned();
    primitives.insert(ratio_index + 1, second_max);

    let p95_index = primitives
        .iter()
        .position(|primitive| primitive.gate_ordinal == 8)
        .unwrap();
    for ordinal in 1..=19 {
        let mut sample = primitives[p95_index].clone();
        sample.tie_key.within_state_observation_ordinal = ordinal.to_string();
        sample.numerator = ordinal.to_string();
        primitives.insert(p95_index + ordinal as usize, sample);
    }
    let rows = authority.evaluate_primitives(&primitives).unwrap();
    assert_eq!(rows[25].numerator, "3");
    assert_eq!(rows[25].denominator, "2");
    assert_eq!(rows[8].numerator, "18");
}

#[test]
fn failed_comparison_is_typed_but_tampered_selected_row_is_invalid() {
    let authority = admitted_authority();
    let mut primitives = passing_primitives(&authority);
    primitives[0].numerator = u64::MAX.to_string();
    let rows = authority.evaluate_primitives(&primitives).unwrap();
    assert!(!rows[0].passed);
    assert_eq!(
        rows[0].reason_code.as_deref(),
        Some("EP01-G000-comparison-failed")
    );

    let mut selected = serde_json::to_value(&rows).unwrap();
    selected[0]["passed"] = json!(true);
    let bytes = gate_artifact_bytes(&authority, &primitives, Some(selected));
    assert!(
        authority
            .recompute_gate_observation_artifact(&bytes, 32, &"e".repeat(64), &"f".repeat(64))
            .is_err()
    );
}

#[test]
fn source_mapping_and_decimal_spellings_are_fail_closed() {
    let authority = admitted_authority();
    let mut wrong_source = passing_primitives(&authority);
    wrong_source[1].source_commitments = source_commitments(22, authority.authority_sha256());
    assert!(authority.evaluate_primitives(&wrong_source).is_err());

    let mut leading_zero = passing_primitives(&authority);
    leading_zero[0].numerator = "00".to_owned();
    assert!(authority.evaluate_primitives(&leading_zero).is_err());

    let mut stale_selection = passing_primitives(&authority);
    stale_selection[35].source_commitments[0].source_artifact_sha256 = "0".repeat(64);
    assert!(authority.evaluate_primitives(&stale_selection).is_err());
}

fn fake_candidate(edge: u32, passed: bool) -> RecomputedGateArtifact {
    let authority = admitted_authority();
    RecomputedGateArtifact {
        candidate_edge: edge,
        package_set_generation_sha256: "e".repeat(64),
        successor_executable_sha256: "f".repeat(64),
        gate_rows: authority
            .evaluate_primitives(&passing_primitives(&authority))
            .unwrap(),
        candidate_pass: passed,
    }
}

#[test]
fn selection_is_exactly_first_passing_edge_without_scoring() {
    let closure = strict_artifact_closure_for_tests();
    assert_eq!(
        select_candidate(
            &closure,
            &[fake_candidate(32, true), fake_candidate(64, true)]
        )
        .unwrap(),
        Some(32)
    );
    assert_eq!(
        select_candidate(
            &closure,
            &[fake_candidate(32, false), fake_candidate(64, true)]
        )
        .unwrap(),
        Some(64)
    );
    assert_eq!(
        select_candidate(
            &closure,
            &[fake_candidate(32, false), fake_candidate(64, false)]
        )
        .unwrap(),
        None
    );
    assert!(
        select_candidate(
            &closure,
            &[fake_candidate(64, true), fake_candidate(32, true)]
        )
        .is_err()
    );
}

fn manifest_artifacts() -> Vec<AdmittedArtifactEnvelope> {
    let mut artifacts = Vec::new();
    let mut ordinal = 0;
    for role in [
        ArtifactRole::PackageValidation,
        ArtifactRole::BuildImportAccounting,
        ArtifactRole::OrderedUniqueTrace,
        ArtifactRole::RuntimeGpu,
        ArtifactRole::GateObservations,
    ] {
        for edge in [32, 64] {
            let package_roles: Vec<Option<PackageRole>> = if role == ArtifactRole::PackageValidation
            {
                vec![
                    Some(PackageRole::RepresentativePackage),
                    Some(PackageRole::SupportingTemporalPackage),
                ]
            } else {
                vec![None]
            };
            for package_role in package_roles {
                artifacts.push(AdmittedArtifactEnvelope {
                    manifest: ArtifactManifestRow {
                        logical_role_tag: role as u8,
                        logical_role: role,
                        candidate_edge: edge,
                        package_role,
                        relative_path: format!("artifacts/a{ordinal}.jcs"),
                        schema: match role {
                            ArtifactRole::PackageValidation => {
                                "mirante4d-viewer-performance-ep01-package-validation-evidence-1"
                            }
                            ArtifactRole::BuildImportAccounting => {
                                "mirante4d-viewer-performance-ep01-build-import-evidence-1"
                            }
                            ArtifactRole::OrderedUniqueTrace => {
                                "mirante4d-viewer-performance-ep01-trace-evidence-1"
                            }
                            ArtifactRole::RuntimeGpu => {
                                "mirante4d-viewer-performance-ep01-runtime-gpu-evidence-1"
                            }
                            ArtifactRole::GateObservations => {
                                "mirante4d-viewer-performance-ep01-gate-observation-evidence-1"
                            }
                        }
                        .to_owned(),
                        bytes: "1".to_owned(),
                        sha256: "a".repeat(64),
                    },
                });
                ordinal += 1;
            }
        }
    }
    artifacts
}

#[test]
fn artifact_manifest_requires_exact_cardinality_order_and_unique_paths() {
    let artifacts = manifest_artifacts();
    validate_artifact_manifest_envelopes(&artifacts).unwrap();

    let mut reordered = artifacts.clone();
    reordered.swap(0, 1);
    assert!(validate_artifact_manifest_envelopes(&reordered).is_err());

    let mut duplicate_path = artifacts.clone();
    duplicate_path[1].manifest.relative_path = duplicate_path[0].manifest.relative_path.clone();
    assert!(validate_artifact_manifest_envelopes(&duplicate_path).is_err());

    assert!(validate_artifact_manifest_envelopes(&artifacts[..11]).is_err());
}

fn artifact_schema(role: ArtifactRole) -> &'static str {
    match role {
        ArtifactRole::PackageValidation => {
            "mirante4d-viewer-performance-ep01-package-validation-evidence-1"
        }
        ArtifactRole::BuildImportAccounting => {
            "mirante4d-viewer-performance-ep01-build-import-evidence-1"
        }
        ArtifactRole::OrderedUniqueTrace => "mirante4d-viewer-performance-ep01-trace-evidence-1",
        ArtifactRole::RuntimeGpu => "mirante4d-viewer-performance-ep01-runtime-gpu-evidence-1",
        ArtifactRole::GateObservations => {
            "mirante4d-viewer-performance-ep01-gate-observation-evidence-1"
        }
    }
}

fn artifact_envelope(authority: &Ep01Authority, role: ArtifactRole) -> Value {
    let common = json!({
        "schema": artifact_schema(role),
        "selection_authority_sha256": authority.authority_sha256(),
        "candidate_edge": 32
    });
    let mut object = common.as_object().unwrap().clone();
    match role {
        ArtifactRole::PackageValidation => {
            object.extend(
                json!({
                    "package_role": "representative_package",
                    "package_external_relative_path": "candidate/package",
                    "independent_reader_executable_sha256": "a".repeat(64),
                    "package_id": format!("m4d-package-v1-sha256:{}", "b".repeat(64)),
                    "manifest_root_sha256": "c".repeat(64),
                    "scientific_content_id": format!("m4d-sc-v1-sha256:{}", "d".repeat(64)),
                    "representation_recipe_id": format!("m4d-recipe-v1-sha256:{}", "e".repeat(64)),
                    "candidate_geometry_sha256": "f".repeat(64),
                    "candidate_content_generation_sha256": "1".repeat(64),
                    "source_inventory_sidecar_sha256": "2".repeat(64),
                    "storage_profile": "m4d-compound-brick-local-b32-1.0",
                    "facts": {},
                    "checks": {}
                })
                .as_object()
                .unwrap()
                .clone(),
            );
        }
        ArtifactRole::BuildImportAccounting => {
            object.extend(
                json!({
                    "roles": [
                        {
                            "role_tag": 0,
                            "role": "representative_package",
                            "publication_parent_synced": true,
                            "independent_validation_complete": true
                        },
                        {
                            "role_tag": 1,
                            "role": "supporting_temporal_package",
                            "publication_parent_synced": true,
                            "independent_validation_complete": true
                        }
                    ],
                    "checkpoint_contract": {},
                    "source_inventory_sidecar_sha256": "2".repeat(64),
                    "candidate_complete": true
                })
                .as_object()
                .unwrap()
                .clone(),
            );
        }
        ArtifactRole::OrderedUniqueTrace => {
            let names = [
                "arbitrary_plane",
                "four_panel",
                "time_navigation",
                "mip",
                "dvr",
                "iso",
                "analysis",
                "verification",
            ];
            let families = names
                .into_iter()
                .enumerate()
                .map(|(tag, family)| json!({"family_tag": tag, "family": family}))
                .collect::<Vec<_>>();
            object.extend(
                json!({
                    "projection_input_sidecar_sha256": "3".repeat(64),
                    "package_set_generation_sha256": "4".repeat(64),
                    "families": families
                })
                .as_object()
                .unwrap()
                .clone(),
            );
        }
        ArtifactRole::RuntimeGpu => {
            object.extend(
                json!({
                    "successor_executable_sha256": "5".repeat(64),
                    "layout_manifest_sha256": "6".repeat(64),
                    "shader_modules": [],
                    "layout_bindings": [],
                    "adapter": {},
                    "capacity": {},
                    "pipeline_counters": {},
                    "instrumentation_overhead_pairs": [],
                    "diagnostic_variants": [],
                    "samples": [],
                    "resource_ledger_closed": true
                })
                .as_object()
                .unwrap()
                .clone(),
            );
        }
        ArtifactRole::GateObservations => {
            object.extend(
                json!({
                    "package_set_generation_sha256": "4".repeat(64),
                    "successor_executable_sha256": "5".repeat(64),
                    "primitive_observations": [],
                    "selected_gate_rows": [],
                    "observations_complete": true,
                    "candidate_pass": false
                })
                .as_object()
                .unwrap()
                .clone(),
            );
        }
    }
    Value::Object(object)
}

fn envelope_manifest(role: ArtifactRole, bytes: &[u8]) -> ArtifactManifestRow {
    ArtifactManifestRow {
        logical_role_tag: role as u8,
        logical_role: role,
        candidate_edge: 32,
        package_role: (role == ArtifactRole::PackageValidation)
            .then_some(PackageRole::RepresentativePackage),
        relative_path: format!("artifacts/role-{}.jcs", role as u8),
        schema: artifact_schema(role).to_owned(),
        bytes: bytes.len().to_string(),
        sha256: artifact_sha256(bytes),
    }
}

#[test]
fn every_artifact_envelope_rejects_unknown_or_missing_top_level_fields() {
    let authority = admitted_authority();
    for role in [
        ArtifactRole::PackageValidation,
        ArtifactRole::BuildImportAccounting,
        ArtifactRole::OrderedUniqueTrace,
        ArtifactRole::RuntimeGpu,
        ArtifactRole::GateObservations,
    ] {
        let baseline = artifact_envelope(&authority, role);
        let baseline_bytes = canonical_artifact_bytes(&baseline).unwrap();
        authority
            .admit_artifact_envelope(envelope_manifest(role, &baseline_bytes), &baseline_bytes)
            .unwrap();

        let mut unknown = baseline.clone();
        unknown["unexpected"] = json!(true);
        let unknown_bytes = canonical_artifact_bytes(&unknown).unwrap();
        assert!(
            authority
                .admit_artifact_envelope(envelope_manifest(role, &unknown_bytes), &unknown_bytes)
                .is_err(),
            "role {role:?} admitted an unknown top-level field"
        );

        let mut missing = baseline;
        missing
            .as_object_mut()
            .unwrap()
            .remove("selection_authority_sha256");
        let missing_bytes = canonical_artifact_bytes(&missing).unwrap();
        assert!(
            authority
                .admit_artifact_envelope(envelope_manifest(role, &missing_bytes), &missing_bytes)
                .is_err(),
            "role {role:?} admitted a missing top-level field"
        );
    }
}
