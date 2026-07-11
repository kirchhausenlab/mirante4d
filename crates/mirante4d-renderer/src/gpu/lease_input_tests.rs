use glam::DVec2;
use mirante4d_dataset::{DatasetResourceIdentity, DatasetSourceId, ResourceRegion};
use mirante4d_domain::{
    DisplayWindow, GridToWorld, LayerTransfer, LogicalLayerKey, Opacity, RgbColor, ScaleLevel,
    Shape3D, TimeIndex, TransferCurve,
};

use super::{
    GpuBrickAtlasPagePriority, GpuCrossSectionChunkDraw, GpuLeaseCrossSectionChannel,
    GpuLeaseDisplayChannel,
};
use crate::{
    CameraRenderMode, CrossSectionPanelBounds, CurrentLeaseBridge, CurrentLeaseVolume,
    IntensityTransfer,
};

#[test]
fn display_and_cross_section_inputs_borrow_one_semantic_lease_volume() {
    let bridge = CurrentLeaseBridge::new();
    let volume = CurrentLeaseVolume::new(
        bridge.resident_set(
            DatasetResourceIdentity::Unverified(DatasetSourceId::new(4)),
            LogicalLayerKey::new(1),
            TimeIndex::new(3),
            ScaleLevel::BASE,
        ),
        Shape3D::new(8, 8, 8).unwrap(),
        Shape3D::new(4, 4, 4).unwrap(),
        GridToWorld::identity(),
    );
    let transfer = IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, 10.0).unwrap(),
            RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    );
    let display = GpuLeaseDisplayChannel::U16 {
        volume,
        mode: CameraRenderMode::Mip,
        transfer,
    };
    assert!(matches!(display, GpuLeaseDisplayChannel::U16 { .. }));

    let region = ResourceRegion::new([0, 0, 4], Shape3D::new(4, 4, 4).unwrap()).unwrap();
    let draws = [GpuCrossSectionChunkDraw {
        resource_region: region,
        panel_bounds: CrossSectionPanelBounds {
            min_points: DVec2::ZERO,
            max_points: DVec2::new(8.0, 8.0),
        },
        vertex_count: 6,
        cache_priority: GpuBrickAtlasPagePriority::new(0, 1.0),
    }];
    let cross_section = GpuLeaseCrossSectionChannel::U16 {
        volume,
        transfer,
        chunks: &draws,
    };
    assert!(matches!(
        cross_section,
        GpuLeaseCrossSectionChannel::U16 { chunks, .. }
            if chunks[0].resource_region == region
    ));
}
