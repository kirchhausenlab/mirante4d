use mirante4d_dataset::{
    DatasetCatalog, DatasetLayer, ResourcePayloadView, ResourceRegion, ResourceValidity,
    ScientificIdentityStatus,
};
use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape3D, Shape4D};
use mirante4d_identity::ScientificContentId;

use crate::{
    AnalysisAccumulator, AnalysisDefinition, AnalysisError, AnalysisPlan, AnalysisPlotArtifact,
    AnalysisTableArtifact,
};

const SCIENTIFIC_ID: &str =
    "m4d-sc-v1-sha256:1111111111111111111111111111111111111111111111111111111111111111";

fn catalog(dtype: IntensityDType, shape: [u64; 4], validity: ResourceValidity) -> DatasetCatalog {
    DatasetCatalog::new(
        "checked analysis",
        ScientificIdentityStatus::Verified(ScientificContentId::parse(SCIENTIFIC_ID).unwrap()),
        vec![
            DatasetLayer::new(
                LogicalLayerKey::new(3),
                "intensity",
                Shape4D::new(shape[0], shape[1], shape[2], shape[3]).unwrap(),
                dtype,
                GridToWorld::identity(),
                validity,
            )
            .unwrap(),
        ],
    )
    .unwrap()
}

#[test]
fn planner_emits_canonical_bounded_edge_blocks_without_materializing_a_volume() {
    let catalog = catalog(
        IntensityDType::Float32,
        [2, 65, 67, 69],
        ResourceValidity::BitMask,
    );
    let definition =
        AnalysisDefinition::full_intensity_summary(&catalog, LogicalLayerKey::new(3), 0, 2)
            .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    assert_eq!(plan.blocks_per_timepoint(), 8);
    assert_eq!(plan.total_blocks(), 16);

    let expected = [
        (0, [0, 0, 0], [64, 64, 64]),
        (0, [0, 0, 64], [64, 64, 5]),
        (0, [0, 64, 0], [64, 3, 64]),
        (0, [0, 64, 64], [64, 3, 5]),
        (0, [64, 0, 0], [1, 64, 64]),
        (0, [64, 0, 64], [1, 64, 5]),
        (0, [64, 64, 0], [1, 3, 64]),
        (0, [64, 64, 64], [1, 3, 5]),
        (1, [0, 0, 0], [64, 64, 64]),
    ];
    for (ordinal, (timepoint, origin, shape)) in expected.into_iter().enumerate() {
        let block = plan.block(ordinal as u64).unwrap();
        assert_eq!(block.ordinal(), ordinal as u64);
        assert_eq!(block.resource().timepoint().get(), timepoint);
        assert_eq!(block.resource().region().origin(), origin);
        assert_eq!(block.resource().region().shape().dimensions(), shape);
        let descriptor = catalog
            .resource_payload_descriptor(block.resource())
            .unwrap();
        assert!(descriptor.byte_len() <= 1_081_344);
    }
    assert!(plan.block(16).is_none());
}

#[test]
fn uint16_validity_and_population_variance_match_hand_computed_facts() {
    let catalog = catalog(
        IntensityDType::Uint16,
        [1, 1, 2, 3],
        ResourceValidity::BitMask,
    );
    let definition =
        AnalysisDefinition::full_intensity_summary(&catalog, LogicalLayerKey::new(3), 0, 1)
            .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let resource = plan.block(0).unwrap().resource();
    let values = [1_u16, 2, 3, 4, 5, 6]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let payload = ResourcePayloadView::new(
        IntensityDType::Uint16,
        Shape3D::new(1, 2, 3).unwrap(),
        ResourceValidity::BitMask,
        &values,
        Some(&[0b0011_0111]),
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    accumulator.include(resource, payload).unwrap();
    let artifacts = accumulator.finish().unwrap();
    let row = &artifacts.table().value().rows()[0];
    assert_eq!(row.geometric_sample_count(), 6);
    assert_eq!(row.valid_sample_count(), 5);
    assert_eq!(row.nonzero_sample_count(), 5);
    assert_eq!(row.minimum(), Some(1.0));
    assert_eq!(row.maximum(), Some(6.0));
    assert_eq!(row.sum(), Some(17.0));
    assert_eq!(row.mean(), Some(3.4));
    assert!((row.population_variance().unwrap() - 3.44).abs() < 1.0e-12);
    assert_eq!(
        artifacts.plot().unwrap().value().points()[0].mean(),
        Some(3.4)
    );
}

#[test]
fn uint8_box_roi_is_exact_and_empty_validity_is_absent_not_zero() {
    let catalog = catalog(
        IntensityDType::Uint8,
        [1, 2, 3, 4],
        ResourceValidity::BitMask,
    );
    let roi = ResourceRegion::new([1, 1, 1], Shape3D::new(1, 2, 3).unwrap()).unwrap();
    let definition = AnalysisDefinition::box_roi_intensity_statistics(
        &catalog,
        LogicalLayerKey::new(3),
        0,
        1,
        roi,
    )
    .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let resource = plan.block(0).unwrap().resource();
    assert_eq!(resource.region(), roi);
    let values = [8_u8, 9, 10, 11, 12, 13];
    let payload = ResourcePayloadView::new(
        IntensityDType::Uint8,
        Shape3D::new(1, 2, 3).unwrap(),
        ResourceValidity::BitMask,
        &values,
        Some(&[0]),
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    accumulator.include(resource, payload).unwrap();
    let artifacts = accumulator.finish().unwrap();
    assert!(artifacts.plot().is_none());
    let row = &artifacts.table().value().rows()[0];
    assert_eq!(row.geometric_sample_count(), 6);
    assert_eq!(row.valid_sample_count(), 0);
    assert_eq!(row.minimum(), None);
    assert_eq!(row.maximum(), None);
    assert_eq!(row.sum(), None);
    assert_eq!(row.mean(), None);
    assert_eq!(row.population_variance(), None);
}

#[test]
fn finite_float32_uses_fixed_order_welford_and_invalid_nan_is_ignored() {
    let catalog = catalog(
        IntensityDType::Float32,
        [1, 1, 1, 5],
        ResourceValidity::BitMask,
    );
    let definition =
        AnalysisDefinition::full_intensity_summary(&catalog, LogicalLayerKey::new(3), 0, 1)
            .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let resource = plan.block(0).unwrap().resource();
    let values = [-1.0_f32, 0.0, 1.0, 2.0, f32::NAN]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    let payload = ResourcePayloadView::new(
        IntensityDType::Float32,
        Shape3D::new(1, 1, 5).unwrap(),
        ResourceValidity::BitMask,
        &values,
        Some(&[0b0000_1111]),
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    accumulator.include(resource, payload).unwrap();
    let artifacts = accumulator.finish().unwrap();
    let row = &artifacts.table().value().rows()[0];
    assert_eq!(row.valid_sample_count(), 4);
    assert_eq!(row.nonzero_sample_count(), 3);
    assert_eq!(row.minimum(), Some(-1.0));
    assert_eq!(row.maximum(), Some(2.0));
    assert_eq!(row.sum(), Some(2.0));
    assert_eq!(row.mean(), Some(0.5));
    assert_eq!(row.population_variance(), Some(1.25));
}

#[test]
fn valid_nonfinite_float_is_rejected_without_a_partial_artifact() {
    let catalog = catalog(
        IntensityDType::Float32,
        [1, 1, 1, 1],
        ResourceValidity::AllValid,
    );
    let definition =
        AnalysisDefinition::full_intensity_summary(&catalog, LogicalLayerKey::new(3), 0, 1)
            .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let resource = plan.block(0).unwrap().resource();
    let bytes = f32::INFINITY.to_le_bytes();
    let payload = ResourcePayloadView::new(
        IntensityDType::Float32,
        Shape3D::new(1, 1, 1).unwrap(),
        ResourceValidity::AllValid,
        &bytes,
        None,
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    assert_eq!(
        accumulator.include(resource, payload),
        Err(AnalysisError::NonFiniteFloat)
    );
}

#[test]
fn canonical_table_and_plot_payloads_round_trip_with_stable_identities() {
    let catalog = catalog(
        IntensityDType::Uint8,
        [1, 1, 1, 2],
        ResourceValidity::AllValid,
    );
    let definition =
        AnalysisDefinition::full_intensity_summary(&catalog, LogicalLayerKey::new(3), 0, 1)
            .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let resource = plan.block(0).unwrap().resource();
    let values = [2_u8, 4];
    let payload = ResourcePayloadView::new(
        IntensityDType::Uint8,
        Shape3D::new(1, 1, 2).unwrap(),
        ResourceValidity::AllValid,
        &values,
        None,
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    accumulator.include(resource, payload).unwrap();
    let artifacts = accumulator.finish().unwrap();

    let table = AnalysisTableArtifact::decode(artifacts.table().bytes()).unwrap();
    let plot = AnalysisPlotArtifact::decode(artifacts.plot().unwrap().bytes()).unwrap();
    assert_eq!(table.value(), artifacts.table().value());
    assert_eq!(plot.value(), artifacts.plot().unwrap().value());
    assert_eq!(table.content_id(), artifacts.table().content_id());
    assert_eq!(plot.content_id(), artifacts.plot().unwrap().content_id());
    assert_eq!(table.descriptor(), artifacts.table().descriptor());
    assert_eq!(plot.descriptor(), artifacts.plot().unwrap().descriptor());
    assert_eq!(
        table.content_id().to_string(),
        "m4d-artifact-v1-sha256:95dd5fc18c81abf847d95d32b28cdcdfb0aace79b1a73bb9da54f96bf46732d5"
    );
    assert_eq!(
        plot.content_id().to_string(),
        "m4d-artifact-v1-sha256:38ec355b797e5e71dcdf654eff29393a0d841b65190d4455c3cf7da9d7321cfb"
    );
    assert!(table.value().to_csv().starts_with("timepoint,"));

    let mut noncanonical = artifacts.table().bytes().to_vec();
    noncanonical.push(b'\n');
    assert!(AnalysisTableArtifact::decode(&noncanonical).is_err());
}

#[test]
fn reduction_requires_the_next_exact_planned_resource() {
    let catalog = catalog(
        IntensityDType::Uint8,
        [1, 1, 1, 2],
        ResourceValidity::AllValid,
    );
    let definition = AnalysisDefinition::new(
        &catalog,
        LogicalLayerKey::new(3),
        0,
        1,
        ResourceRegion::new([0; 3], Shape3D::new(1, 1, 2).unwrap()).unwrap(),
        crate::AnalysisOperation::FullIntensitySummary,
        Shape3D::new(1, 1, 1).unwrap(),
    )
    .unwrap();
    let plan = AnalysisPlan::new(definition).unwrap();
    let wrong = plan.block(1).unwrap().resource();
    let values = [4_u8];
    let payload = ResourcePayloadView::new(
        IntensityDType::Uint8,
        Shape3D::new(1, 1, 1).unwrap(),
        ResourceValidity::AllValid,
        &values,
        None,
    )
    .unwrap();
    let mut accumulator = AnalysisAccumulator::new(plan);
    assert_eq!(
        accumulator.include(wrong, payload),
        Err(AnalysisError::UnexpectedBlock)
    );
    assert!(matches!(
        accumulator.finish(),
        Err(AnalysisError::Incomplete)
    ));
}
