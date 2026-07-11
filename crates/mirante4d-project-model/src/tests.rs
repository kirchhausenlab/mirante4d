use proptest::prelude::*;
use proptest::test_runner::RngSeed;

use mirante4d_domain::{
    DisplayWindow, Opacity, Projection, RgbColor, SamplingPolicy, TransferCurve, UnitQuaternion,
    WorldPoint3,
};
use mirante4d_identity::{ExactBytesDigest, MediaType, ObjectRole};

use super::*;

#[test]
fn canonical_uuid_round_trips_for_project_and_artifact_handles() {
    let value = "00112233-4455-6677-8899-aabbccddeeff";
    let project = ProjectId::parse(value).unwrap();
    let artifact = ArtifactHandleId::parse(value).unwrap();

    assert_eq!(project.to_string(), value);
    assert_eq!(artifact.to_string(), value);
    assert_eq!(project.bytes(), artifact.bytes());
}

#[test]
fn uuid_parser_rejects_noncanonical_forms() {
    for invalid in [
        "00112233445566778899aabbccddeeff",
        "00112233-4455-6677-8899-AABBCCDDEEFF",
        "{00112233-4455-6677-8899-aabbccddeeff}",
        "00112233-4455-6677-8899-aabbccddefg0",
    ] {
        assert!(ProjectId::parse(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn revision_high_water_never_reuses_after_undo_and_overflow_is_typed() {
    let project = ProjectId::from_bytes([7; 16]);
    let initial = ProjectRevisionId::initial(project);
    assert_eq!(initial.sequence(), 0);
    let mut high_water = ProjectRevisionHighWater::initial(project);
    let first = high_water.allocate_after(initial).unwrap();
    let second = high_water.allocate_after(first).unwrap();
    let after_undo = high_water.allocate_after(initial).unwrap();

    assert_eq!(first.sequence(), 1);
    assert_eq!(second.sequence(), 2);
    assert_eq!(after_undo.sequence(), 3);
    assert_eq!(high_water.sequence(), 3);

    let foreign = ProjectRevisionId::initial(ProjectId::from_bytes([8; 16]));
    assert!(matches!(
        high_water.allocate_after(foreign),
        Err(ProjectModelError::RevisionProjectMismatch { .. })
    ));
    assert_eq!(high_water.sequence(), 3);

    let unknown_future = ProjectRevisionId::new(project, 4);
    assert_eq!(
        high_water.allocate_after(unknown_future).unwrap_err(),
        ProjectModelError::RevisionBeyondHighWater {
            revision_sequence: 4,
            high_water_sequence: 3,
        }
    );
    assert_eq!(high_water.sequence(), 3);

    let mut exhausted = ProjectRevisionHighWater::new(project, u64::MAX);
    assert_eq!(
        exhausted
            .allocate_after(ProjectRevisionId::new(project, u64::MAX))
            .unwrap_err(),
        ProjectModelError::RevisionOverflow
    );
}

#[test]
fn locator_is_bounded_and_rejects_controls() {
    assert_eq!(
        DatasetLocatorHint::new("relative/dataset.m4d")
            .unwrap()
            .as_str(),
        "relative/dataset.m4d"
    );
    assert_eq!(
        DatasetLocatorHint::new("dataset\npath").unwrap_err(),
        ProjectModelError::DatasetLocatorHintContainsControl
    );
    assert_eq!(
        DatasetLocatorHint::new(" ".repeat(MAX_DATASET_LOCATOR_HINT_BYTES + 1)).unwrap_err(),
        ProjectModelError::DatasetLocatorHintTooLong {
            maximum: MAX_DATASET_LOCATOR_HINT_BYTES,
        }
    );
}

#[test]
fn dataset_locator_is_explicitly_not_scientific_identity() {
    let first = dataset_reference("first/location.m4d");
    let second = dataset_reference("second/location.m4d");

    assert_ne!(first, second);
    assert!(first.has_same_scientific_content(&second));
}

#[test]
fn view_reorder_keeps_active_selection_by_logical_layer_key() {
    let view = view(vec![layer(0), layer(1), layer(2)], 1);
    let reordered = view
        .with_layer_order(vec![
            LogicalLayerKey::new(2),
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(1),
        ])
        .unwrap();

    assert_eq!(reordered.active_layer(), LogicalLayerKey::new(1));
    assert_eq!(
        reordered
            .layers()
            .iter()
            .map(LayerViewState::layer_key)
            .collect::<Vec<_>>(),
        vec![
            LogicalLayerKey::new(2),
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(1)
        ]
    );
    assert!(
        view.with_layer_order(vec![
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(2)
        ])
        .is_err()
    );
}

#[test]
fn project_construction_rejects_invalid_preset_closure_without_a_partial_value() {
    let preset = ChannelPreset::new(
        ChannelPresetId::new("incomplete").unwrap(),
        "Incomplete",
        vec![preset_entry(0)],
    )
    .unwrap();
    let result = ProjectState::new(
        project_id(),
        dataset_reference("dataset.m4d"),
        view(vec![layer(0), layer(1)], 0),
        vec![preset],
        Vec::new(),
    );

    assert_eq!(
        result.unwrap_err(),
        ProjectModelError::InvalidPresetLayerClosure {
            preset_id: "incomplete".to_owned()
        }
    );
}

#[test]
fn project_construction_rejects_artifact_references_outside_the_view() {
    let artifact = artifact(
        ArtifactHandleId::from_bytes([9; 16]),
        vec![LogicalLayerKey::new(7)],
    );
    let result = ProjectState::new(
        project_id(),
        dataset_reference("dataset.m4d"),
        view(vec![layer(0)], 0),
        Vec::new(),
        vec![artifact],
    );

    assert!(matches!(
        result,
        Err(ProjectModelError::ArtifactLayerMissing { ordinal: 7, .. })
    ));
}

#[test]
fn preset_entries_and_artifact_source_layers_are_keyed_and_canonical() {
    let preset = ChannelPreset::new(
        ChannelPresetId::new("ordered").unwrap(),
        "Ordered",
        vec![preset_entry(2), preset_entry(0), preset_entry(1)],
    )
    .unwrap();
    assert_eq!(
        preset
            .entries()
            .iter()
            .map(ChannelPresetEntry::layer_key)
            .collect::<Vec<_>>(),
        vec![
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(1),
            LogicalLayerKey::new(2),
        ]
    );
    assert_eq!(
        preset.entry(LogicalLayerKey::new(1)).unwrap().layer_key(),
        LogicalLayerKey::new(1)
    );
    assert!(preset.entry(LogicalLayerKey::new(7)).is_none());

    let artifact = artifact(
        ArtifactHandleId::from_bytes([12; 16]),
        vec![
            LogicalLayerKey::new(2),
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(1),
        ],
    );
    assert_eq!(
        artifact.source_layers(),
        &[
            LogicalLayerKey::new(0),
            LogicalLayerKey::new(1),
            LogicalLayerKey::new(2),
        ]
    );
}

#[test]
fn artifact_schemas_enforce_exact_versioned_media_types_and_object_roles() {
    let schemas = [
        ArtifactSchema::RoiV1,
        ArtifactSchema::TrackV1,
        ArtifactSchema::AnnotationV1,
        ArtifactSchema::MeasurementV1,
        ArtifactSchema::AnalysisTableV1,
        ArtifactSchema::AnalysisPlotV1,
    ];
    for (index, schema) in schemas.into_iter().enumerate() {
        let reference = ArtifactReference::new(
            ArtifactHandleId::from_bytes([index as u8; 16]),
            schema,
            artifact_content_id(),
            raw_artifact_object(schema),
            None,
            None,
            Vec::new(),
            schema.as_str(),
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        )
        .unwrap();
        assert_eq!(reference.schema(), schema);
        assert_eq!(
            reference.object().media_type().as_str(),
            schema.media_type()
        );
        assert_eq!(reference.object().role().as_str(), schema.object_role());
    }

    let schema = ArtifactSchema::RoiV1;
    let wrong_media = raw_object("application/json", schema.object_role());
    assert!(matches!(
        ArtifactReference::new(
            ArtifactHandleId::from_bytes([80; 16]),
            schema,
            artifact_content_id(),
            wrong_media,
            None,
            None,
            Vec::new(),
            "ROI",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        ),
        Err(ProjectModelError::ArtifactMediaTypeMismatch { .. })
    ));

    let wrong_role = raw_object(schema.media_type(), "artifact.analysis-table.v1");
    assert!(matches!(
        ArtifactReference::new(
            ArtifactHandleId::from_bytes([81; 16]),
            schema,
            artifact_content_id(),
            wrong_role,
            None,
            None,
            Vec::new(),
            "ROI",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        ),
        Err(ProjectModelError::ArtifactObjectRoleMismatch { .. })
    ));
}

#[test]
fn collection_limits_reject_oversized_inputs_before_duplicate_checks() {
    let oversized_layers = vec![layer(0); MAX_VIEW_LAYERS + 1];
    assert!(matches!(
        ViewState::new(
            oversized_layers,
            LogicalLayerKey::new(0),
            TimeIndex::new(0),
            camera(),
            ViewerLayout::Single3d,
            cross_section(),
            IsoLightState::attached_camera(),
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "view layers",
            ..
        })
    ));

    let oversized_entries = vec![preset_entry(0); MAX_CHANNEL_PRESET_ENTRIES + 1];
    assert!(matches!(
        ChannelPreset::new(
            ChannelPresetId::new("bounded").unwrap(),
            "Bounded",
            oversized_entries,
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "channel preset entries",
            ..
        })
    ));

    assert_eq!(
        ChannelPreset::new(
            ChannelPresetId::new("bounded-label").unwrap(),
            " ".repeat(MAX_PROJECT_LABEL_BYTES + 1),
            Vec::new(),
        )
        .unwrap_err(),
        ProjectModelError::LabelTooLong {
            kind: "channel preset label",
            maximum: MAX_PROJECT_LABEL_BYTES,
        }
    );

    let handle = ArtifactHandleId::from_bytes([82; 16]);
    let oversized_sources = vec![LogicalLayerKey::new(0); MAX_ARTIFACT_SOURCE_LAYERS + 1];
    assert!(matches!(
        ArtifactReference::new(
            handle,
            ArtifactSchema::AnalysisTableV1,
            artifact_content_id(),
            raw_artifact_object(ArtifactSchema::AnalysisTableV1),
            None,
            None,
            oversized_sources,
            "Table",
            true,
            ArtifactCompleteness::Complete,
            ArtifactRecoverability::NonRegenerable,
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "artifact source layers",
            ..
        })
    ));

    let base_view = view(vec![layer(0)], 0);
    let preset = ChannelPreset::new(
        ChannelPresetId::new("same").unwrap(),
        "Same",
        vec![preset_entry(0)],
    )
    .unwrap();
    assert!(matches!(
        ProjectState::new(
            project_id(),
            dataset_reference("dataset.m4d"),
            base_view.clone(),
            vec![preset; MAX_CHANNEL_PRESETS + 1],
            Vec::new(),
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "channel presets",
            ..
        })
    ));

    let repeated_artifact = artifact(
        ArtifactHandleId::from_bytes([83; 16]),
        vec![LogicalLayerKey::new(0)],
    );
    assert!(matches!(
        ProjectState::new(
            project_id(),
            dataset_reference("dataset.m4d"),
            base_view.clone(),
            Vec::new(),
            vec![repeated_artifact; MAX_ARTIFACTS + 1],
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "artifacts",
            ..
        })
    ));

    let full_preset = ChannelPreset::new(
        ChannelPresetId::new("full").unwrap(),
        "Full",
        (0..MAX_CHANNEL_PRESET_ENTRIES as u32)
            .map(preset_entry)
            .collect(),
    )
    .unwrap();
    let too_many_nested_presets =
        vec![full_preset; MAX_TOTAL_CHANNEL_PRESET_ENTRIES / MAX_CHANNEL_PRESET_ENTRIES + 1];
    assert!(matches!(
        ProjectState::new(
            project_id(),
            dataset_reference("dataset.m4d"),
            base_view.clone(),
            too_many_nested_presets,
            Vec::new(),
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "total channel preset entries",
            ..
        })
    ));

    let full_artifact = artifact(
        ArtifactHandleId::from_bytes([84; 16]),
        (0..MAX_ARTIFACT_SOURCE_LAYERS as u32)
            .map(LogicalLayerKey::new)
            .collect(),
    );
    let too_many_nested_artifacts =
        vec![
            full_artifact;
            MAX_TOTAL_ARTIFACT_SOURCE_LAYER_REFERENCES / MAX_ARTIFACT_SOURCE_LAYERS + 1
        ];
    assert!(matches!(
        ProjectState::new(
            project_id(),
            dataset_reference("dataset.m4d"),
            base_view,
            Vec::new(),
            too_many_nested_artifacts,
        ),
        Err(ProjectModelError::CollectionLimitExceeded {
            collection: "total artifact source layer references",
            ..
        })
    ));
}

#[test]
fn generation_projection_round_trips_the_exact_revision_and_state() {
    let state = valid_project();
    let revision = ProjectRevisionId::new(state.project_id(), 42);
    let high_water = ProjectRevisionHighWater::new(state.project_id(), 57);
    let projection =
        ProjectGenerationProjection::new(revision, high_water.clone(), state.clone()).unwrap();
    let (decoded_revision, decoded_high_water, decoded_state) = projection.into_parts();

    assert_eq!(decoded_revision, revision);
    assert_eq!(decoded_high_water, high_water);
    assert_eq!(decoded_state, state);
}

#[test]
fn generation_projection_rejects_a_revision_from_another_project() {
    let state = valid_project();
    let foreign = ProjectRevisionId::initial(ProjectId::from_bytes([99; 16]));
    let high_water = ProjectRevisionHighWater::initial(state.project_id());

    assert!(matches!(
        ProjectGenerationProjection::new(foreign, high_water, state),
        Err(ProjectModelError::RevisionProjectMismatch { .. })
    ));
}

#[test]
fn generation_projection_rejects_foreign_or_insufficient_high_water() {
    let state = valid_project();
    let revision = ProjectRevisionId::new(state.project_id(), 8);
    let foreign = ProjectRevisionHighWater::new(ProjectId::from_bytes([99; 16]), 8);
    assert!(matches!(
        ProjectGenerationProjection::new(revision, foreign, state.clone()),
        Err(ProjectModelError::RevisionHighWaterProjectMismatch { .. })
    ));

    let behind = ProjectRevisionHighWater::new(state.project_id(), 7);
    assert_eq!(
        ProjectGenerationProjection::new(revision, behind, state).unwrap_err(),
        ProjectModelError::RevisionBeyondHighWater {
            revision_sequence: 8,
            high_water_sequence: 7,
        }
    );
}

fn project_id() -> ProjectId {
    ProjectId::from_bytes([4; 16])
}

fn zero_hex() -> &'static str {
    "0000000000000000000000000000000000000000000000000000000000000000"
}

fn dataset_reference(locator: &str) -> DatasetReference {
    DatasetReference::new(
        ScientificContentId::parse(&format!("{}{}", ScientificContentId::PREFIX, zero_hex()))
            .unwrap(),
        None,
        None,
        Some(DatasetLocatorHint::new(locator).unwrap()),
    )
}

fn transfer() -> LayerTransfer {
    LayerTransfer::new(
        DisplayWindow::new(0.0, 1.0).unwrap(),
        RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
        Opacity::new(1.0).unwrap(),
        TransferCurve::linear(),
        false,
    )
}

fn layer(ordinal: u32) -> LayerViewState {
    LayerViewState::new(
        LogicalLayerKey::new(ordinal),
        true,
        transfer(),
        RenderState::mip(SamplingPolicy::SmoothLinear),
    )
}

fn preset_entry(ordinal: u32) -> ChannelPresetEntry {
    ChannelPresetEntry::new(
        LogicalLayerKey::new(ordinal),
        true,
        transfer(),
        RenderState::mip(SamplingPolicy::SmoothLinear),
    )
}

fn camera() -> CameraView {
    CameraView::new(
        Projection::Orthographic,
        WorldPoint3::origin(),
        UnitQuaternion::identity(),
        1.0,
        320.0,
        10.0,
    )
    .unwrap()
}

fn cross_section() -> CrossSectionView {
    CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0).unwrap()
}

fn view(layers: Vec<LayerViewState>, active_ordinal: u32) -> ViewState {
    ViewState::new(
        layers,
        LogicalLayerKey::new(active_ordinal),
        TimeIndex::new(0),
        camera(),
        ViewerLayout::Single3d,
        cross_section(),
        IsoLightState::attached_camera(),
    )
    .unwrap()
}

fn artifact_content_id() -> ArtifactContentId {
    ArtifactContentId::parse(&format!("{}{}", ArtifactContentId::PREFIX, zero_hex())).unwrap()
}

fn raw_object(media_type: &str, object_role: &str) -> RawObjectDescriptor {
    RawObjectDescriptor::new(
        ExactBytesDigest::parse(&format!("{}{}", ExactBytesDigest::PREFIX, zero_hex())).unwrap(),
        12,
        MediaType::parse(media_type).unwrap(),
        ObjectRole::parse(object_role).unwrap(),
    )
}

fn raw_artifact_object(schema: ArtifactSchema) -> RawObjectDescriptor {
    raw_object(schema.media_type(), schema.object_role())
}

fn artifact(handle_id: ArtifactHandleId, source_layers: Vec<LogicalLayerKey>) -> ArtifactReference {
    let schema = ArtifactSchema::AnalysisTableV1;
    ArtifactReference::new(
        handle_id,
        schema,
        artifact_content_id(),
        raw_artifact_object(schema),
        None,
        None,
        source_layers,
        "Table",
        true,
        ArtifactCompleteness::Complete,
        ArtifactRecoverability::NonRegenerable,
    )
    .unwrap()
}

fn valid_project() -> ProjectState {
    let view = view(vec![layer(0), layer(1)], 1);
    let preset = ChannelPreset::new(
        ChannelPresetId::new("all").unwrap(),
        "All layers",
        vec![preset_entry(0), preset_entry(1)],
    )
    .unwrap();
    ProjectState::new(
        project_id(),
        dataset_reference("dataset.m4d"),
        view,
        vec![preset],
        vec![artifact(
            ArtifactHandleId::from_bytes([8; 16]),
            vec![LogicalLayerKey::new(0)],
        )],
    )
    .unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 128,
        max_shrink_iters: 1_024,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(0x4d34_5052_4f4a_4d4f),
        ..ProptestConfig::default()
    })]

    #[test]
    fn uuid_format_parse_is_a_bijection(bytes in any::<[u8; 16]>()) {
        let id = ProjectId::from_bytes(bytes);
        let encoded = id.to_string();
        prop_assert_eq!(ProjectId::parse(&encoded).unwrap(), id);
    }

    #[test]
    fn high_water_allocation_is_exact_and_project_bound(
        project_bytes in any::<[u8; 16]>(),
        sequence in 0_u64..u64::MAX,
    ) {
        let project_id = ProjectId::from_bytes(project_bytes);
        let mut high_water = ProjectRevisionHighWater::new(project_id, sequence);
        let next = high_water.allocate_after(ProjectRevisionId::initial(project_id)).unwrap();
        prop_assert_eq!(next.project_id(), project_id);
        prop_assert_eq!(next.sequence(), sequence + 1);
        prop_assert_eq!(high_water.sequence(), sequence + 1);
    }

    #[test]
    fn arbitrary_reorder_keeps_active_selection_by_identity(
        layer_count in 1_u32..16,
        selector in any::<u32>(),
    ) {
        let active = selector % layer_count;
        let original = view((0..layer_count).map(layer).collect(), active);
        let reversed = (0..layer_count)
            .rev()
            .map(LogicalLayerKey::new)
            .collect();
        let reordered = original.with_layer_order(reversed).unwrap();

        prop_assert_eq!(reordered.active_layer(), LogicalLayerKey::new(active));
        prop_assert!(reordered.layer(LogicalLayerKey::new(active)).is_some());
    }
}
