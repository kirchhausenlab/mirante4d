use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use glam::{DQuat, DVec2, DVec3};
use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetResourceIdentity,
    DatasetResourceKey, DatasetSourceId, ResourceLease, ResourcePayloadDescriptor,
    ResourcePayloadView, ResourceRegion, ResourceValidity,
};
use mirante4d_domain::{
    DisplayWindow, GridToWorld, IntensityDType, IsoLightState, LayerTransfer, LogicalLayerKey,
    Opacity, Projection, RgbColor, ScaleLevel, Shape3D, TimeIndex, TransferCurve,
};
use mirante4d_render_api::PresentationViewport;

use super::{
    GpuBrickAtlasPagePriority, GpuCrossSectionChunkDraw, GpuLeaseCrossSectionChannel,
    GpuLeaseDisplayChannel, GpuLeaseDisplayRequest, GpuRenderer,
};
use crate::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, CrossSectionPanel,
    CrossSectionPanelBounds, CrossSectionView, CurrentLeaseBridge, CurrentLeaseVolume,
    IntensityTransfer, RenderViewport,
};

const TEST_GPU_CACHE_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(10_000);
static SHARED_RENDERER: OnceLock<Mutex<GpuRenderer>> = OnceLock::new();

struct FixtureLease {
    key: DatasetResourceKey,
    descriptor: ResourcePayloadDescriptor,
    values: Box<[u8]>,
    validity: Option<Box<[u8]>>,
}

impl ResourceLease for FixtureLease {
    fn key(&self) -> DatasetResourceKey {
        self.key
    }

    fn payload(&self) -> ResourcePayloadView<'_> {
        self.descriptor
            .view(&self.values, self.validity.as_deref())
            .unwrap()
    }
}

struct LeaseFixture {
    bridge: CurrentLeaseBridge,
    identity: DatasetResourceIdentity,
    layer: LogicalLayerKey,
    shape: Shape3D,
    resource_shape: Shape3D,
}

impl LeaseFixture {
    fn volume(&self) -> CurrentLeaseVolume<'_> {
        CurrentLeaseVolume::new(
            self.bridge.resident_set(
                self.identity,
                self.layer,
                TimeIndex::new(0),
                ScaleLevel::BASE,
            ),
            self.shape,
            self.resource_shape,
            GridToWorld::identity(),
        )
    }
}

#[derive(Clone, Default)]
struct TestLedger {
    used: Arc<AtomicU64>,
    peak: Arc<AtomicU64>,
}

struct TestCharge {
    category: CpuLedgerCategory,
    bytes: u64,
    used: Arc<AtomicU64>,
}

impl CpuByteLedger for TestLedger {
    fn try_acquire(
        &self,
        category: CpuLedgerCategory,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
        if bytes == 0 {
            return Err(CpuLedgerError::ZeroByteReservation);
        }
        let used = self.used.fetch_add(bytes, Ordering::SeqCst) + bytes;
        self.peak.fetch_max(used, Ordering::SeqCst);
        Ok(Box::new(TestCharge {
            category,
            bytes,
            used: Arc::clone(&self.used),
        }))
    }
}

impl CpuByteLease for TestCharge {
    fn category(&self) -> CpuLedgerCategory {
        self.category
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for TestCharge {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

fn with_renderer<T>(run: impl FnOnce(&GpuRenderer) -> T) -> T {
    let renderer = SHARED_RENDERER.get_or_init(|| {
        Mutex::new(
            GpuRenderer::new_with_cache_budgets_blocking(
                TEST_GPU_CACHE_BYTES,
                TEST_GPU_CACHE_BYTES,
            )
            .expect("trusted GPU lane requires a usable renderer adapter"),
        )
    });
    let guard = renderer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    run(&guard)
}

fn fixture(
    dtype: IntensityDType,
    shape: Shape3D,
    resource_shape: Shape3D,
    values: &[u8],
    validity: Option<&[u8]>,
) -> LeaseFixture {
    let bytes_per_sample = usize::from(dtype.bytes_per_sample());
    let sample_count = usize::try_from(shape.element_count().unwrap()).unwrap();
    assert_eq!(values.len(), sample_count * bytes_per_sample);
    assert!(validity.is_none_or(|mask| mask.len() == sample_count));

    let identity = DatasetResourceIdentity::Unverified(DatasetSourceId::new(
        NEXT_SOURCE_ID.fetch_add(1, Ordering::SeqCst),
    ));
    let layer = LogicalLayerKey::new(0);
    let mut requirements = Vec::new();
    let mut leases = Vec::<Arc<dyn ResourceLease>>::new();
    for z in (0..shape.z()).step_by(resource_shape.z() as usize) {
        for y in (0..shape.y()).step_by(resource_shape.y() as usize) {
            for x in (0..shape.x()).step_by(resource_shape.x() as usize) {
                let page_shape = Shape3D::new(
                    resource_shape.z().min(shape.z() - z),
                    resource_shape.y().min(shape.y() - y),
                    resource_shape.x().min(shape.x() - x),
                )
                .unwrap();
                let region = ResourceRegion::new([z, y, x], page_shape).unwrap();
                let key = DatasetResourceKey::new(
                    identity,
                    layer,
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    region,
                );
                requirements.push(key);

                let mut page_values = Vec::new();
                let mut page_validity = validity.map(|_| Vec::new());
                for local_z in 0..page_shape.z() {
                    for local_y in 0..page_shape.y() {
                        for local_x in 0..page_shape.x() {
                            let index = (((z + local_z) * shape.y() + y + local_y) * shape.x()
                                + x
                                + local_x) as usize;
                            let start = index * bytes_per_sample;
                            page_values.extend_from_slice(&values[start..start + bytes_per_sample]);
                            if let (Some(source), Some(page)) = (validity, page_validity.as_mut()) {
                                page.push(source[index]);
                            }
                        }
                    }
                }
                let validity_bits = page_validity.as_deref().map(pack_validity);
                let descriptor = ResourcePayloadDescriptor::new(
                    dtype,
                    page_shape,
                    if validity_bits.is_some() {
                        ResourceValidity::BitMask
                    } else {
                        ResourceValidity::AllValid
                    },
                )
                .unwrap();
                leases.push(Arc::new(FixtureLease {
                    key,
                    descriptor,
                    values: page_values.into_boxed_slice(),
                    validity: validity_bits.map(Vec::into_boxed_slice),
                }));
            }
        }
    }

    let mut bridge = CurrentLeaseBridge::new();
    bridge.replace_current_requirements(requirements).unwrap();
    for lease in leases {
        bridge.install(lease).unwrap();
    }
    LeaseFixture {
        bridge,
        identity,
        layer,
        shape,
        resource_shape,
    }
}

fn pack_validity(validity: &[u8]) -> Vec<u8> {
    let mut bits = vec![0_u8; validity.len().div_ceil(8)];
    for (index, valid) in validity.iter().copied().enumerate() {
        if valid != 0 {
            bits[index / 8] |= 1 << (index % 8);
        }
    }
    bits
}

fn u16_bytes(values: &[u16]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn f32_bytes(values: &[f32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn front_camera(shape: Shape3D) -> mirante4d_render_api::CameraFrame {
    let center = DVec3::new(
        shape.x().saturating_sub(1) as f64 * 0.5,
        shape.y().saturating_sub(1) as f64 * 0.5,
        shape.z().saturating_sub(1) as f64 * 0.5,
    );
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        center - DVec3::Z * shape.z() as f64 * 1.5,
        center,
        -DVec3::Y,
        1.0,
        shape.y() as f64,
        crate::current_camera::presentation(shape.x() as f64, shape.y() as f64),
    )
}

fn display_transfer() -> IntensityTransfer {
    IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, f32::from(u16::MAX)).unwrap(),
            RgbColor::new([1.0, 0.4, 0.1]).unwrap(),
            Opacity::new(1.0).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn semantic_lease_display_crosses_integer_pages() {
    let shape = Shape3D::new(2, 2, 4).unwrap();
    let values = (1..=16).map(|value| value * 100).collect::<Vec<u16>>();
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &u16_bytes(&values),
        None,
    );
    let ledger = TestLedger::default();
    let viewport = RenderViewport::new(4, 2).unwrap();
    let camera = front_camera(shape);
    let rgba = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_channels_to_display_texture(
                &ledger,
                &[GpuLeaseDisplayChannel::U16 {
                    volume: fixture.volume(),
                    mode: CameraRenderMode::Mip,
                    transfer: display_transfer(),
                }],
                GpuLeaseDisplayRequest {
                    camera,
                    viewport,
                    quality: CameraRenderQuality::voxel_exact(),
                    iso_light_state: IsoLightState::default(),
                    camera_axes: camera.axes(),
                },
            )
            .unwrap();
        assert_eq!(frame.diagnostics.channels, 1);
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert!(rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn f32_validity_excludes_an_invalid_high_sample() {
    let shape = Shape3D::new(2, 1, 1).unwrap();
    let fixture = fixture(
        IntensityDType::Float32,
        shape,
        shape,
        &f32_bytes(&[7.0, 999.0]),
        Some(&[1, 0]),
    );
    let output = with_renderer(|renderer| {
        renderer.render_camera_f32_from_leases(
            fixture.volume(),
            &TestLedger::default(),
            front_camera(shape),
            RenderViewport::new(1, 1).unwrap(),
            CameraRenderModeF32::Mip,
        )
    })
    .unwrap();
    assert_eq!(output.image.pixels(), &[7.0]);
    assert!(output.image.is_covered_index(0));
    assert!(output.brick_frame.unwrap().complete);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn semantic_cross_section_renders_a_dataset_page() {
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let values = (0..64).map(|value| value * 500).collect::<Vec<u16>>();
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&values),
        None,
    );
    let region = fixture.bridge.required_keys().next().unwrap().region();
    let draws = [GpuCrossSectionChunkDraw {
        resource_region: region,
        panel_bounds: CrossSectionPanelBounds {
            min_points: DVec2::ZERO,
            max_points: DVec2::splat(4.0),
        },
        vertex_count: 4,
        cache_priority: GpuBrickAtlasPagePriority::new(0, 1.0),
    }];
    let view = CrossSectionView::new(
        DVec3::splat(1.5),
        CrossSectionPanel::Xy,
        DQuat::IDENTITY,
        1.0,
        1.0,
    );
    let rgba = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_cross_section_channels_to_display_texture(
                &TestLedger::default(),
                &[GpuLeaseCrossSectionChannel::U16 {
                    volume: fixture.volume(),
                    transfer: display_transfer(),
                    chunks: &draws,
                }],
                view,
                PresentationViewport::new(4.0, 4.0).unwrap(),
                RenderViewport::new(4, 4).unwrap(),
            )
            .unwrap();
        assert_eq!(frame.diagnostics.draw_calls, 1);
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert!(rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn atlas_reuse_releases_upload_staging_bytes() {
    let shape = Shape3D::new(2, 2, 4).unwrap();
    let values = (1..=16).map(|value| value * 100).collect::<Vec<u16>>();
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &u16_bytes(&values),
        None,
    );
    let ledger = TestLedger::default();
    with_renderer(|renderer| {
        let before = renderer.stats().unwrap();
        renderer
            .render_camera_u16_from_leases(
                fixture.volume(),
                &ledger,
                front_camera(shape),
                RenderViewport::new(4, 2).unwrap(),
                CameraRenderMode::Mip,
            )
            .unwrap();
        let first = renderer.stats().unwrap();
        renderer
            .render_camera_u16_from_leases(
                fixture.volume(),
                &ledger,
                front_camera(shape),
                RenderViewport::new(4, 2).unwrap(),
                CameraRenderMode::Mip,
            )
            .unwrap();
        let second = renderer.stats().unwrap();
        assert_eq!(first.brick_atlas_uploads - before.brick_atlas_uploads, 2);
        assert_eq!(second.brick_atlas_uploads, first.brick_atlas_uploads);
        assert!(second.brick_atlas_cache_hits > first.brick_atlas_cache_hits);
    });
    assert!(ledger.peak.load(Ordering::SeqCst) > 0);
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);
}
