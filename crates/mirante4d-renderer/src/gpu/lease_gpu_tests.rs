use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};

use glam::{DQuat, DVec2, DVec3};
use mirante4d_data::{DenseVolumeF32, DenseVolumeU8, DenseVolumeU16};
use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetResourceIdentity,
    DatasetResourceKey, DatasetSourceId, ResourceLease, ResourcePayloadDescriptor,
    ResourcePayloadView, ResourceRegion, ResourceValidity,
};
use mirante4d_domain::{
    DisplayWindow, GridToWorld, IntensityDType, IsoLightState, LayerTransfer, LogicalLayerKey,
    Opacity, Projection, RgbColor, ScaleLevel, Shape3D, TimeIndex, TransferCurve,
};
use mirante4d_format::{DatasetId, LayerId};
use mirante4d_render_api::PresentationViewport;

use super::{
    GpuBrickAtlasPagePriority, GpuCrossSectionChunkDraw, GpuLeaseCrossSectionChannel,
    GpuLeaseDisplayChannel, GpuLeaseDisplayRequest, GpuRenderError, GpuRenderer,
    REQUIRED_MAX_BUFFER_SIZE, REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE,
    REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE, adapter_preference_score,
    renderer_device_descriptor, renderer_required_limits_for_adapter,
};
use crate::{
    CameraRenderMode, CameraRenderModeF32, CameraRenderQuality, CrossSectionPanel,
    CrossSectionPanelBounds, CrossSectionView, CurrentLeaseBridge, CurrentLeaseVolume,
    DvrRenderParameters, DvrRgbaChannelFrame, FrameDiagnostics, FrameDiagnosticsF32,
    IntensityChannelFrame, IntensityTransfer, IsoSurfaceChannelFrameF32, IsoSurfaceParameters,
    MipImageF32, MipImageU16, RenderViewport, ScalarDisplayTransfer, composite_dvr_rgba_channels,
    composite_intensity_channels, composite_iso_surface_f32_channels, render_camera,
    render_camera_f32, render_camera_f32_with_quality, render_camera_u8,
    render_camera_with_quality, render_dvr_channels_with_quality,
};

const TEST_GPU_CACHE_BYTES: u64 = 64 * 1024 * 1024;

static NEXT_SOURCE_ID: AtomicU64 = AtomicU64::new(10_000);
static SHARED_RENDERER: OnceLock<Mutex<GpuRenderer>> = OnceLock::new();

#[derive(Debug)]
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
            .expect("fixture payload remains valid")
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

#[derive(Clone)]
struct TestLedger {
    limit: u64,
    used: Arc<AtomicU64>,
    peak: Arc<AtomicU64>,
}

struct TestCpuCharge {
    category: CpuLedgerCategory,
    bytes: u64,
    used: Arc<AtomicU64>,
}

impl TestLedger {
    fn unlimited() -> Self {
        Self {
            limit: u64::MAX,
            used: Arc::new(AtomicU64::new(0)),
            peak: Arc::new(AtomicU64::new(0)),
        }
    }

    fn capped(limit: u64) -> Self {
        Self {
            limit,
            used: Arc::new(AtomicU64::new(0)),
            peak: Arc::new(AtomicU64::new(0)),
        }
    }
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
        loop {
            let used = self.used.load(Ordering::SeqCst);
            let available = self.limit.saturating_sub(used);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            let next = used + bytes;
            if self
                .used
                .compare_exchange(used, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.peak.fetch_max(next, Ordering::SeqCst);
                return Ok(Box::new(TestCpuCharge {
                    category,
                    bytes,
                    used: Arc::clone(&self.used),
                }));
            }
        }
    }
}

impl CpuByteLease for TestCpuCharge {
    fn category(&self) -> CpuLedgerCategory {
        self.category
    }

    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for TestCpuCharge {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

fn with_renderer<T>(f: impl FnOnce(&GpuRenderer) -> T) -> T {
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
    f(&guard)
}

fn fixture(
    dtype: IntensityDType,
    shape: Shape3D,
    resource_shape: Shape3D,
    values: &[u8],
    validity: Option<&[u8]>,
    installed_origins: Option<&[[u64; 3]]>,
) -> LeaseFixture {
    let bytes_per_sample = usize::from(dtype.bytes_per_sample());
    let expected_samples = usize::try_from(shape.element_count().unwrap()).unwrap();
    assert_eq!(values.len(), expected_samples * bytes_per_sample);
    if let Some(validity) = validity {
        assert_eq!(validity.len(), expected_samples);
        assert!(validity.iter().all(|value| matches!(value, 0 | 1)));
    }

    let identity = DatasetResourceIdentity::Unverified(DatasetSourceId::new(
        NEXT_SOURCE_ID.fetch_add(1, Ordering::SeqCst),
    ));
    let layer = LogicalLayerKey::new(0);
    let mut requirements = Vec::new();
    let mut leases = Vec::<Arc<dyn ResourceLease>>::new();

    for z in (0..shape.z()).step_by(usize::try_from(resource_shape.z()).unwrap()) {
        for y in (0..shape.y()).step_by(usize::try_from(resource_shape.y()).unwrap()) {
            for x in (0..shape.x()).step_by(usize::try_from(resource_shape.x()).unwrap()) {
                let page_shape = Shape3D::new(
                    resource_shape.z().min(shape.z() - z),
                    resource_shape.y().min(shape.y() - y),
                    resource_shape.x().min(shape.x() - x),
                )
                .unwrap();
                let origin = [z, y, x];
                let region = ResourceRegion::new(origin, page_shape).unwrap();
                let key = DatasetResourceKey::new(
                    identity,
                    layer,
                    TimeIndex::new(0),
                    ScaleLevel::BASE,
                    region,
                );
                requirements.push(key);

                let mut page_values = Vec::with_capacity(
                    usize::try_from(page_shape.element_count().unwrap()).unwrap()
                        * bytes_per_sample,
                );
                let mut page_validity = validity.map(|_| Vec::new());
                for local_z in 0..page_shape.z() {
                    for local_y in 0..page_shape.y() {
                        for local_x in 0..page_shape.x() {
                            let index = (((z + local_z) * shape.y() + y + local_y) * shape.x()
                                + x
                                + local_x) as usize;
                            let byte_start = index * bytes_per_sample;
                            page_values.extend_from_slice(
                                &values[byte_start..byte_start + bytes_per_sample],
                            );
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
                let lease: Arc<dyn ResourceLease> = Arc::new(FixtureLease {
                    key,
                    descriptor,
                    values: page_values.into_boxed_slice(),
                    validity: validity_bits.map(Vec::into_boxed_slice),
                });
                if installed_origins.is_none_or(|origins| origins.contains(&origin)) {
                    leases.push(lease);
                }
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

fn u8_bytes(values: &[u8]) -> Vec<u8> {
    values.to_vec()
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

fn pattern_u8(shape: Shape3D) -> Vec<u8> {
    (0..shape.z())
        .flat_map(|z| {
            (0..shape.y())
                .flat_map(move |y| (0..shape.x()).map(move |x| (1 + z * 41 + y * 7 + x * 3) as u8))
        })
        .collect()
}

fn pattern_u16(shape: Shape3D) -> Vec<u16> {
    (0..shape.z())
        .flat_map(|z| {
            (0..shape.y()).flat_map(move |y| {
                (0..shape.x()).map(move |x| (1 + z * 10_000 + y * 1_000 + x * 100) as u16)
            })
        })
        .collect()
}

fn pattern_f32(shape: Shape3D) -> Vec<f32> {
    (0..shape.z())
        .flat_map(|z| {
            (0..shape.y()).flat_map(move |y| {
                (0..shape.x()).map(move |x| z as f32 * 1.75 + y as f32 * 0.3 + x as f32 * 0.1 - 1.0)
            })
        })
        .collect()
}

fn dense_u8(shape: Shape3D, values: Vec<u8>) -> DenseVolumeU8 {
    DenseVolumeU8::new(
        DatasetId::new("lease-gpu-fixture").unwrap(),
        LayerId::new("u8").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap()
}

fn dense_u16(shape: Shape3D, values: Vec<u16>) -> DenseVolumeU16 {
    DenseVolumeU16::new(
        DatasetId::new("lease-gpu-fixture").unwrap(),
        LayerId::new("u16").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap()
}

fn dense_f32(shape: Shape3D, values: Vec<f32>) -> DenseVolumeF32 {
    DenseVolumeF32::new(
        DatasetId::new("lease-gpu-fixture").unwrap(),
        LayerId::new("f32").unwrap(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap()
}

fn front_camera(shape: Shape3D) -> mirante4d_render_api::CameraFrame {
    let center = DVec3::new(
        shape.x().saturating_sub(1) as f64 * 0.5,
        shape.y().saturating_sub(1) as f64 * 0.5,
        shape.z().saturating_sub(1) as f64 * 0.5,
    );
    let height = shape.y() as f64;
    crate::current_camera::frame_from_look_at(
        Projection::Orthographic,
        center - DVec3::Z * shape.z() as f64 * 1.5,
        center,
        -DVec3::Y,
        1.0,
        height / (2.0 * (std::f64::consts::FRAC_PI_3 * 0.5).tan()),
        crate::current_camera::presentation(shape.x() as f64, height),
    )
}

fn iso_parameters(low: f32, high: f32, threshold: f32) -> IsoSurfaceParameters {
    IsoSurfaceParameters::new(
        ((threshold - low) / (high - low)).clamp(0.0, 1.0),
        ScalarDisplayTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn dvr_parameters(low: f32, high: f32, density_scale: f64) -> DvrRenderParameters {
    let transfer = ScalarDisplayTransfer::new(
        DisplayWindow::new(low, high).unwrap(),
        TransferCurve::linear(),
        false,
    );
    DvrRenderParameters::new(transfer, transfer, [1.0; 4], 1.0, density_scale)
}

fn display_transfer(color: [f32; 3], opacity: f32, low: f32, high: f32) -> IntensityTransfer {
    IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            RgbColor::new(color).unwrap(),
            Opacity::new(opacity).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn display_request(
    camera: mirante4d_render_api::CameraFrame,
    viewport: RenderViewport,
    quality: CameraRenderQuality,
) -> GpuLeaseDisplayRequest {
    GpuLeaseDisplayRequest {
        camera,
        viewport,
        quality,
        iso_light_state: IsoLightState::default(),
        camera_axes: camera.axes(),
    }
}

fn assert_u16_reference(
    actual: &super::GpuMipOutput,
    expected: &MipImageU16,
    expected_frame: FrameDiagnostics,
    label: &str,
) {
    assert_eq!(
        actual.image.coverage(),
        expected.coverage(),
        "{label} coverage"
    );
    for (index, (&actual, &expected)) in actual
        .image
        .pixels()
        .iter()
        .zip(expected.pixels())
        .enumerate()
    {
        assert!(
            actual.abs_diff(expected) <= 1,
            "{label} pixel {index}: actual={actual}, expected={expected}"
        );
    }
    assert_eq!(actual.frame, expected_frame, "{label} diagnostics");
    let brick = actual.brick_frame.as_ref().unwrap();
    assert!(brick.complete, "{label} should be complete");
    assert_eq!(brick.missing_voxel_samples, 0, "{label} missing samples");
    if let (Some(actual), Some(expected)) = (actual.image.iso_surface(), expected.iso_surface()) {
        assert_eq!(
            actual.source_values(),
            expected.source_values(),
            "{label} ISO values"
        );
        assert_eq!(
            actual.coverage(),
            expected.coverage(),
            "{label} ISO coverage"
        );
        for (actual, expected) in actual.hit_depth().iter().zip(expected.hit_depth()) {
            assert!((actual - expected).abs() <= 1.0e-4, "{label} ISO depth");
        }
    }
    if let (Some(actual), Some(expected)) = (actual.image.dvr_rgba(), expected.dvr_rgba()) {
        assert_rgba_f32_close(
            actual.premultiplied_rgba(),
            expected.premultiplied_rgba(),
            1.0e-5,
            label,
        );
    }
}

fn assert_f32_reference(
    actual: &super::GpuMipOutputF32,
    expected: &MipImageF32,
    expected_frame: FrameDiagnosticsF32,
    label: &str,
) {
    assert_eq!(
        actual.image.coverage(),
        expected.coverage(),
        "{label} coverage"
    );
    for (index, (&actual, &expected)) in actual
        .image
        .pixels()
        .iter()
        .zip(expected.pixels())
        .enumerate()
    {
        assert!(
            (actual - expected).abs() <= 1.0e-5,
            "{label} pixel {index}: actual={actual}, expected={expected}"
        );
    }
    assert_eq!(actual.frame.input_voxels, expected_frame.input_voxels);
    assert_eq!(actual.frame.output_pixels, expected_frame.output_pixels);
    assert_eq!(actual.frame.nonzero_pixels, expected_frame.nonzero_pixels);
    assert!((actual.frame.max_value - expected_frame.max_value).abs() <= 1.0e-5);
    let brick = actual.brick_frame.as_ref().unwrap();
    assert!(brick.complete, "{label} should be complete");
    assert_eq!(brick.missing_voxel_samples, 0, "{label} missing samples");
    if let (Some(actual), Some(expected)) = (actual.image.iso_surface(), expected.iso_surface()) {
        for (&actual, &expected) in actual.source_values().iter().zip(expected.source_values()) {
            assert!((actual - expected).abs() <= 1.0e-5, "{label} ISO values");
        }
        assert_eq!(
            actual.coverage(),
            expected.coverage(),
            "{label} ISO coverage"
        );
    }
    if let (Some(actual), Some(expected)) = (actual.image.dvr_rgba(), expected.dvr_rgba()) {
        assert_rgba_f32_close(
            actual.premultiplied_rgba(),
            expected.premultiplied_rgba(),
            1.0e-5,
            label,
        );
    }
}

fn assert_rgba_f32_close(actual: &[[f32; 4]], expected: &[[f32; 4]], tolerance: f32, label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label} RGBA length");
    for (pixel, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        for component in 0..4 {
            assert!(
                (actual[component] - expected[component]).abs() <= tolerance,
                "{label} RGBA pixel {pixel} component {component}: actual={}, expected={}",
                actual[component],
                expected[component]
            );
        }
    }
}

fn assert_rgba_u8_close(actual: &[u8], expected: &[u8], tolerance: u8, label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label} RGBA length");
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            actual.abs_diff(expected) <= tolerance,
            "{label} RGBA byte {index}: actual={actual}, expected={expected}"
        );
    }
}

fn usable_adapter(require_default_limits: bool) -> wgpu::Adapter {
    let instance = wgpu::Instance::default();
    let default_limits = wgpu::Limits::default();
    pollster::block_on(async {
        instance
            .enumerate_adapters(wgpu::Backends::PRIMARY | wgpu::Backends::GL)
            .await
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                info.device_type != wgpu::DeviceType::Cpu
                    && info.backend != wgpu::Backend::Noop
                    && renderer_required_limits_for_adapter(adapter).is_ok()
                    && (!require_default_limits
                        || adapter.limits().max_storage_buffers_per_shader_stage
                            >= default_limits.max_storage_buffers_per_shader_stage)
            })
            .max_by_key(|adapter| adapter_preference_score(&adapter.get_info()))
    })
    .expect("trusted GPU lane requires a usable non-CPU adapter")
}

fn standard_fixture() -> (Shape3D, RenderViewport, mirante4d_render_api::CameraFrame) {
    let shape = Shape3D::new(4, 4, 4).unwrap();
    (
        shape,
        RenderViewport::new(4, 4).unwrap(),
        front_camera(shape),
    )
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn existing_default_limit_device_is_rejected_before_lease_rendering() {
    let adapter = usable_adapter(true);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("mirante4d-lease-default-limit-device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .unwrap();
    let result = GpuRenderer::from_existing_device_with_cache_budgets(
        &adapter,
        device,
        queue,
        TEST_GPU_CACHE_BYTES,
        TEST_GPU_CACHE_BYTES,
    );
    assert!(matches!(
        result,
        Err(GpuRenderError::DeviceLimitTooLow { .. })
    ));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn existing_renderer_limit_device_renders_semantic_lease() {
    let adapter = usable_adapter(false);
    let descriptor =
        renderer_device_descriptor(&adapter, "mirante4d-lease-existing-device").unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).unwrap();
    let renderer = GpuRenderer::from_existing_device_with_cache_budgets(
        &adapter,
        device,
        queue,
        TEST_GPU_CACHE_BYTES,
        TEST_GPU_CACHE_BYTES,
    )
    .unwrap();
    let limits = &renderer.adapter_diagnostics().requested_limits;
    assert!(limits.max_buffer_size >= REQUIRED_MAX_BUFFER_SIZE);
    assert!(limits.max_storage_buffer_binding_size >= REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE);
    assert!(
        limits.max_storage_buffers_per_shader_stage
            >= REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE
    );
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&values),
        None,
        None,
    );
    let output = renderer
        .render_camera_u16_from_leases(
            fixture.volume(),
            &TestLedger::unlimited(),
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
        .unwrap();
    assert!(output.image.pixels().iter().any(|value| *value != 0));
}

macro_rules! lease_u8_reference_test {
    ($name:ident, $mode:expr) => {
        #[test]
        #[ignore = "requires a usable non-CPU GPU adapter"]
        fn $name() {
            let (shape, viewport, camera) = standard_fixture();
            let values = pattern_u8(shape);
            let fixture = fixture(
                IntensityDType::Uint8,
                shape,
                Shape3D::new(2, 2, 2).unwrap(),
                &u8_bytes(&values),
                None,
                None,
            );
            let dense = dense_u8(shape, values);
            let mode = $mode;
            let (expected, expected_frame) =
                render_camera_u8(&dense, camera, viewport, mode).unwrap();
            let actual = with_renderer(|renderer| {
                renderer.render_camera_u8_from_leases(
                    fixture.volume(),
                    &TestLedger::unlimited(),
                    camera,
                    viewport,
                    mode,
                )
            })
            .unwrap();
            assert_u16_reference(&actual, &expected, expected_frame, stringify!($name));
        }
    };
}

lease_u8_reference_test!(u8_mip_matches_cpu_reference, CameraRenderMode::Mip);
lease_u8_reference_test!(
    u8_dvr_matches_cpu_reference,
    CameraRenderMode::Dvr {
        parameters: dvr_parameters(0.0, 255.0, 5.0),
    }
);
lease_u8_reference_test!(
    u8_iso_matches_cpu_reference,
    CameraRenderMode::Isosurface {
        parameters: iso_parameters(0.0, 255.0, 75.0),
    }
);

macro_rules! lease_u16_reference_test {
    ($name:ident, $mode:expr) => {
        #[test]
        #[ignore = "requires a usable non-CPU GPU adapter"]
        fn $name() {
            let (shape, viewport, camera) = standard_fixture();
            let values = pattern_u16(shape);
            let fixture = fixture(
                IntensityDType::Uint16,
                shape,
                Shape3D::new(2, 2, 2).unwrap(),
                &u16_bytes(&values),
                None,
                None,
            );
            let dense = dense_u16(shape, values);
            let mode = $mode;
            let (expected, expected_frame) = render_camera(&dense, camera, viewport, mode).unwrap();
            let actual = with_renderer(|renderer| {
                renderer.render_camera_u16_from_leases(
                    fixture.volume(),
                    &TestLedger::unlimited(),
                    camera,
                    viewport,
                    mode,
                )
            })
            .unwrap();
            assert_u16_reference(&actual, &expected, expected_frame, stringify!($name));
        }
    };
}

lease_u16_reference_test!(u16_mip_matches_cpu_reference, CameraRenderMode::Mip);
lease_u16_reference_test!(
    u16_dvr_matches_cpu_reference,
    CameraRenderMode::Dvr {
        parameters: dvr_parameters(0.0, f32::from(u16::MAX), 5.0),
    }
);
lease_u16_reference_test!(
    u16_iso_matches_cpu_reference,
    CameraRenderMode::Isosurface {
        parameters: iso_parameters(0.0, f32::from(u16::MAX), 15_000.0),
    }
);

macro_rules! lease_f32_reference_test {
    ($name:ident, $mode:expr) => {
        #[test]
        #[ignore = "requires a usable non-CPU GPU adapter"]
        fn $name() {
            let (shape, viewport, camera) = standard_fixture();
            let values = pattern_f32(shape);
            let fixture = fixture(
                IntensityDType::Float32,
                shape,
                Shape3D::new(2, 2, 2).unwrap(),
                &f32_bytes(&values),
                None,
                None,
            );
            let dense = dense_f32(shape, values);
            let mode = $mode;
            let (expected, expected_frame) =
                render_camera_f32(&dense, camera, viewport, mode).unwrap();
            let actual = with_renderer(|renderer| {
                renderer.render_camera_f32_from_leases(
                    fixture.volume(),
                    &TestLedger::unlimited(),
                    camera,
                    viewport,
                    mode,
                )
            })
            .unwrap();
            assert_f32_reference(&actual, &expected, expected_frame, stringify!($name));
        }
    };
}

lease_f32_reference_test!(f32_mip_matches_cpu_reference, CameraRenderModeF32::Mip);
lease_f32_reference_test!(
    f32_dvr_matches_cpu_reference,
    CameraRenderModeF32::Dvr {
        parameters: dvr_parameters(-1.0, 6.0, 5.0),
    }
);
lease_f32_reference_test!(
    f32_iso_matches_cpu_reference,
    CameraRenderModeF32::Isosurface {
        parameters: iso_parameters(-1.0, 6.0, 2.0),
    }
);

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn u16_voxel_and_smooth_quality_match_their_cpu_references() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &u16_bytes(&values),
        None,
        None,
    );
    let dense = dense_u16(shape, values);
    let mode = CameraRenderMode::Dvr {
        parameters: dvr_parameters(0.0, f32::from(u16::MAX), 5.0),
    };
    for (label, quality) in [
        ("u16 voxel", CameraRenderQuality::voxel_exact()),
        ("u16 smooth", CameraRenderQuality::smooth_linear()),
    ] {
        let (expected, expected_frame) =
            render_camera_with_quality(&dense, camera, viewport, mode, quality).unwrap();
        let actual = with_renderer(|renderer| {
            renderer.render_camera_u16_from_leases_with_quality(
                fixture.volume(),
                &TestLedger::unlimited(),
                camera,
                viewport,
                mode,
                quality,
            )
        })
        .unwrap();
        assert_u16_reference(&actual, &expected, expected_frame, label);
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn f32_voxel_and_smooth_quality_match_their_cpu_references() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_f32(shape);
    let fixture = fixture(
        IntensityDType::Float32,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &f32_bytes(&values),
        None,
        None,
    );
    let dense = dense_f32(shape, values);
    let mode = CameraRenderModeF32::Dvr {
        parameters: dvr_parameters(-1.0, 6.0, 5.0),
    };
    for (label, quality) in [
        ("f32 voxel", CameraRenderQuality::voxel_exact()),
        ("f32 smooth", CameraRenderQuality::smooth_linear()),
    ] {
        let (expected, expected_frame) =
            render_camera_f32_with_quality(&dense, camera, viewport, mode, quality).unwrap();
        let actual = with_renderer(|renderer| {
            renderer.render_camera_f32_from_leases_with_quality(
                fixture.volume(),
                &TestLedger::unlimited(),
                camera,
                viewport,
                mode,
                quality,
            )
        })
        .unwrap();
        assert_f32_reference(&actual, &expected, expected_frame, label);
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn integer_incomplete_residency_reports_missing_samples_and_coverage() {
    let shape = Shape3D::new(2, 2, 6).unwrap();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &u16_bytes(&values),
        None,
        Some(&[[0, 0, 0]]),
    );
    let output = with_renderer(|renderer| {
        renderer.render_camera_u16_from_leases(
            fixture.volume(),
            &TestLedger::unlimited(),
            front_camera(shape),
            RenderViewport::new(6, 2).unwrap(),
            CameraRenderMode::Mip,
        )
    })
    .unwrap();
    let brick = output.brick_frame.unwrap();
    assert!(!brick.complete);
    assert!(brick.missing_voxel_samples > 0);
    assert!((0..output.image.pixels().len()).any(|index| !output.image.is_covered_index(index)));
    assert!(output.image.pixels().iter().any(|value| *value != 0));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn f32_incomplete_residency_reports_missing_samples_and_coverage() {
    let shape = Shape3D::new(2, 2, 6).unwrap();
    let values = pattern_f32(shape);
    let fixture = fixture(
        IntensityDType::Float32,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &f32_bytes(&values),
        None,
        Some(&[[0, 0, 0]]),
    );
    let output = with_renderer(|renderer| {
        renderer.render_camera_f32_from_leases(
            fixture.volume(),
            &TestLedger::unlimited(),
            front_camera(shape),
            RenderViewport::new(6, 2).unwrap(),
            CameraRenderModeF32::Mip,
        )
    })
    .unwrap();
    let brick = output.brick_frame.unwrap();
    assert!(!brick.complete);
    assert!(brick.missing_voxel_samples > 0);
    assert!((0..output.image.pixels().len()).any(|index| !output.image.is_covered_index(index)));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn valid_zero_is_covered_and_not_treated_as_missing() {
    let shape = Shape3D::new(1, 1, 1).unwrap();
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&[0]),
        Some(&[1]),
        None,
    );
    let output = with_renderer(|renderer| {
        renderer.render_camera_u16_from_leases(
            fixture.volume(),
            &TestLedger::unlimited(),
            front_camera(shape),
            RenderViewport::new(1, 1).unwrap(),
            CameraRenderMode::Mip,
        )
    })
    .unwrap();
    assert_eq!(output.image.pixels(), &[0]);
    assert!(output.image.is_covered_index(0));
    assert!(output.brick_frame.unwrap().complete);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn validity_mask_suppresses_invalid_high_value_without_hiding_valid_sample() {
    let shape = Shape3D::new(2, 1, 1).unwrap();
    let values = vec![7_u16, u16::MAX];
    let validity = vec![1_u8, 0];
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&values),
        Some(&validity),
        None,
    );
    let dense = dense_u16(shape, values)
        .with_render_valid(validity)
        .unwrap();
    let camera = front_camera(shape);
    let viewport = RenderViewport::new(1, 1).unwrap();
    let (expected, _) = render_camera(&dense, camera, viewport, CameraRenderMode::Mip).unwrap();
    let output = with_renderer(|renderer| {
        renderer.render_camera_u16_from_leases(
            fixture.volume(),
            &TestLedger::unlimited(),
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
    })
    .unwrap();
    assert_eq!(output.image.pixels(), expected.pixels());
    assert_eq!(output.image.pixels(), &[7]);
    assert!(output.image.is_covered_index(0));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn integer_atlas_reuses_overlap_and_uploads_only_new_semantic_page() {
    let shape = Shape3D::new(2, 2, 6).unwrap();
    let resource_shape = Shape3D::new(2, 2, 2).unwrap();
    let values = pattern_u16(shape);
    let first = fixture(
        IntensityDType::Uint16,
        shape,
        resource_shape,
        &u16_bytes(&values),
        None,
        Some(&[[0, 0, 0], [0, 0, 2]]),
    );
    let second = fixture_with_identity(
        &first,
        IntensityDType::Uint16,
        &u16_bytes(&values),
        None,
        &[[0, 0, 2], [0, 0, 4]],
    );
    with_renderer(|renderer| {
        let before = renderer.stats().unwrap();
        renderer
            .render_camera_u16_from_leases(
                first.volume(),
                &TestLedger::unlimited(),
                front_camera(shape),
                RenderViewport::new(6, 2).unwrap(),
                CameraRenderMode::Mip,
            )
            .unwrap();
        let middle = renderer.stats().unwrap();
        renderer
            .render_camera_u16_from_leases(
                second.volume(),
                &TestLedger::unlimited(),
                front_camera(shape),
                RenderViewport::new(6, 2).unwrap(),
                CameraRenderMode::Mip,
            )
            .unwrap();
        let after = renderer.stats().unwrap();
        assert_eq!(middle.brick_atlas_uploads - before.brick_atlas_uploads, 2);
        assert_eq!(after.brick_atlas_uploads - middle.brick_atlas_uploads, 1);
        assert_eq!(
            middle.brick_atlas_cache_misses - before.brick_atlas_cache_misses,
            1
        );
        assert_eq!(
            after.brick_atlas_cache_hits - middle.brick_atlas_cache_hits,
            1
        );
    });
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn f32_atlas_reuses_overlap_and_uploads_only_new_semantic_page() {
    let shape = Shape3D::new(2, 2, 6).unwrap();
    let resource_shape = Shape3D::new(2, 2, 2).unwrap();
    let values = pattern_f32(shape);
    let first = fixture(
        IntensityDType::Float32,
        shape,
        resource_shape,
        &f32_bytes(&values),
        None,
        Some(&[[0, 0, 0], [0, 0, 2]]),
    );
    let second = fixture_with_identity(
        &first,
        IntensityDType::Float32,
        &f32_bytes(&values),
        None,
        &[[0, 0, 2], [0, 0, 4]],
    );
    with_renderer(|renderer| {
        let before = renderer.stats().unwrap();
        renderer
            .render_camera_f32_from_leases(
                first.volume(),
                &TestLedger::unlimited(),
                front_camera(shape),
                RenderViewport::new(6, 2).unwrap(),
                CameraRenderModeF32::Mip,
            )
            .unwrap();
        let middle = renderer.stats().unwrap();
        renderer
            .render_camera_f32_from_leases(
                second.volume(),
                &TestLedger::unlimited(),
                front_camera(shape),
                RenderViewport::new(6, 2).unwrap(),
                CameraRenderModeF32::Mip,
            )
            .unwrap();
        let after = renderer.stats().unwrap();
        assert_eq!(middle.brick_atlas_uploads - before.brick_atlas_uploads, 2);
        assert_eq!(after.brick_atlas_uploads - middle.brick_atlas_uploads, 1);
        assert_eq!(
            middle.brick_atlas_cache_misses - before.brick_atlas_cache_misses,
            1
        );
        assert_eq!(
            after.brick_atlas_cache_hits - middle.brick_atlas_cache_hits,
            1
        );
    });
}

fn fixture_with_identity(
    template: &LeaseFixture,
    dtype: IntensityDType,
    values: &[u8],
    validity: Option<&[u8]>,
    installed_origins: &[[u64; 3]],
) -> LeaseFixture {
    let mut result = fixture(
        dtype,
        template.shape,
        template.resource_shape,
        values,
        validity,
        Some(installed_origins),
    );
    let replacement = result.bridge.retained_payloads().collect::<Vec<_>>();
    let mut bridge = CurrentLeaseBridge::new();
    let requirements = result
        .bridge
        .required_keys()
        .map(|key| {
            DatasetResourceKey::new(
                template.identity,
                template.layer,
                key.timepoint(),
                key.scale(),
                key.region(),
            )
        })
        .collect::<Vec<_>>();
    bridge.replace_current_requirements(requirements).unwrap();
    for (old_key, payload) in replacement {
        let key = DatasetResourceKey::new(
            template.identity,
            template.layer,
            old_key.timepoint(),
            old_key.scale(),
            old_key.region(),
        );
        let lease: Arc<dyn ResourceLease> = Arc::new(FixtureLease {
            key,
            descriptor: payload.descriptor(),
            values: payload.value_bytes().to_vec().into_boxed_slice(),
            validity: payload
                .validity_bits()
                .map(|bits| bits.to_vec().into_boxed_slice()),
        });
        bridge.install(lease).unwrap();
    }
    result.bridge = bridge;
    result.identity = template.identity;
    result.layer = template.layer;
    result
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_atlas_capacity_error_is_typed_and_does_not_fallback() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&values),
        None,
        None,
    );
    let renderer = GpuRenderer::new_with_cache_budgets_blocking(TEST_GPU_CACHE_BYTES, 1).unwrap();
    let result = renderer.render_camera_u16_from_leases(
        fixture.volume(),
        &TestLedger::unlimited(),
        camera,
        viewport,
        CameraRenderMode::Mip,
    );
    assert!(matches!(result, Err(GpuRenderError::BudgetExceeded { .. })));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn cpu_upload_ledger_capacity_error_is_typed_and_does_not_fallback() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&values),
        None,
        None,
    );
    let ledger = TestLedger::capped(1);
    let result = with_renderer(|renderer| {
        renderer.render_camera_u16_from_leases(
            fixture.volume(),
            &ledger,
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
    });
    assert!(matches!(
        result,
        Err(GpuRenderError::CpuLedger(
            CpuLedgerError::CapacityExceeded {
                category: CpuLedgerCategory::UploadStaging,
                ..
            }
        ))
    ));
    assert_eq!(ledger.used.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn lease_display_single_channel_matches_cpu_compositor() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_u16(shape);
    let fixture = fixture(
        IntensityDType::Uint16,
        shape,
        Shape3D::new(2, 2, 2).unwrap(),
        &u16_bytes(&values),
        None,
        None,
    );
    let transfer = display_transfer([1.0, 0.0, 0.0], 0.7, 0.0, f32::from(u16::MAX));
    let (cpu, _) = render_camera(
        &dense_u16(shape, values),
        camera,
        viewport,
        CameraRenderMode::Mip,
    )
    .unwrap();
    let expected =
        composite_intensity_channels(&[IntensityChannelFrame::new(&cpu, transfer)]).unwrap();
    let actual = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[GpuLeaseDisplayChannel::U16 {
                    volume: fixture.volume(),
                    mode: CameraRenderMode::Mip,
                    transfer,
                }],
                display_request(camera, viewport, CameraRenderQuality::voxel_exact()),
            )
            .unwrap();
        assert_eq!(frame.diagnostics.channels, 1);
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert_rgba_u8_close(&actual, expected.pixels(), 1, "single display");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn lease_display_multichannel_matches_cpu_compositor() {
    let (shape, viewport, camera) = standard_fixture();
    let first_values = pattern_u16(shape);
    let second_values = first_values
        .iter()
        .map(|value| value.saturating_div(2))
        .collect::<Vec<_>>();
    let first = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&first_values),
        None,
        None,
    );
    let second = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&second_values),
        None,
        None,
    );
    let red = display_transfer([1.0, 0.0, 0.0], 0.7, 0.0, f32::from(u16::MAX));
    let green = display_transfer([0.0, 1.0, 0.0], 0.5, 0.0, f32::from(u16::MAX));
    let (first_cpu, _) = render_camera(
        &dense_u16(shape, first_values),
        camera,
        viewport,
        CameraRenderMode::Mip,
    )
    .unwrap();
    let (second_cpu, _) = render_camera(
        &dense_u16(shape, second_values),
        camera,
        viewport,
        CameraRenderMode::Mip,
    )
    .unwrap();
    let expected = composite_intensity_channels(&[
        IntensityChannelFrame::new(&first_cpu, red),
        IntensityChannelFrame::new(&second_cpu, green),
    ])
    .unwrap();
    let actual = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[
                    GpuLeaseDisplayChannel::U16 {
                        volume: first.volume(),
                        mode: CameraRenderMode::Mip,
                        transfer: red,
                    },
                    GpuLeaseDisplayChannel::U16 {
                        volume: second.volume(),
                        mode: CameraRenderMode::Mip,
                        transfer: green,
                    },
                ],
                display_request(camera, viewport, CameraRenderQuality::voxel_exact()),
            )
            .unwrap();
        assert_eq!(frame.diagnostics.channels, 2);
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert_rgba_u8_close(&actual, expected.pixels(), 1, "multichannel display");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn lease_display_multichannel_dvr_matches_same_ray_cpu_reference() {
    let (shape, viewport, camera) = standard_fixture();
    let first_values = pattern_u16(shape);
    let second_values = first_values
        .iter()
        .map(|value| value.saturating_div(2))
        .collect::<Vec<_>>();
    let first = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&first_values),
        None,
        None,
    );
    let second = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&second_values),
        None,
        None,
    );
    let red = display_transfer([1.0, 0.0, 0.0], 0.7, 0.0, f32::from(u16::MAX));
    let green = display_transfer([0.0, 1.0, 0.0], 0.5, 0.0, f32::from(u16::MAX));
    let red_parameters = DvrRenderParameters::new(
        red.scalar_transfer(),
        red.scalar_transfer(),
        red.color_rgba(),
        red.opacity().get(),
        1.5,
    );
    let green_parameters = DvrRenderParameters::new(
        green.scalar_transfer(),
        green.scalar_transfer(),
        green.color_rgba(),
        green.opacity().get(),
        1.5,
    );
    let first_dense = dense_u16(shape, first_values);
    let second_dense = dense_u16(shape, second_values);
    let quality = CameraRenderQuality::smooth_linear();
    let (cpu, _) = render_dvr_channels_with_quality(
        &[
            crate::DvrVolumeChannel::u16(&first_dense, red_parameters),
            crate::DvrVolumeChannel::u16(&second_dense, green_parameters),
        ],
        camera,
        viewport,
        quality,
    )
    .unwrap();
    let expected =
        composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(cpu.dvr_rgba().unwrap())]).unwrap();
    let actual = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[
                    GpuLeaseDisplayChannel::U16 {
                        volume: first.volume(),
                        mode: CameraRenderMode::Dvr {
                            parameters: red_parameters,
                        },
                        transfer: red,
                    },
                    GpuLeaseDisplayChannel::U16 {
                        volume: second.volume(),
                        mode: CameraRenderMode::Dvr {
                            parameters: green_parameters,
                        },
                        transfer: green,
                    },
                ],
                display_request(camera, viewport, quality),
            )
            .unwrap();
        assert_eq!(frame.diagnostics.channels, 2);
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert_rgba_u8_close(&actual, expected.pixels(), 2, "multichannel DVR");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn lease_display_f32_iso_matches_cpu_surface_compositor() {
    let (shape, viewport, camera) = standard_fixture();
    let values = pattern_f32(shape);
    let fixture = fixture(
        IntensityDType::Float32,
        shape,
        shape,
        &f32_bytes(&values),
        None,
        None,
    );
    let transfer = display_transfer([0.2, 0.5, 1.0], 0.8, -1.0, 6.0);
    let mode = CameraRenderModeF32::Isosurface {
        parameters: iso_parameters(-1.0, 6.0, 2.0),
    };
    let (cpu, _) = render_camera_f32(&dense_f32(shape, values), camera, viewport, mode).unwrap();
    let expected = composite_iso_surface_f32_channels(
        &[IsoSurfaceChannelFrameF32::new(
            cpu.iso_surface().unwrap(),
            transfer,
        )],
        IsoLightState::default(),
        camera.axes(),
    )
    .unwrap();
    let actual = with_renderer(|renderer| {
        let frame = renderer
            .render_lease_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[GpuLeaseDisplayChannel::F32 {
                    volume: fixture.volume(),
                    mode,
                    transfer,
                }],
                display_request(camera, viewport, CameraRenderQuality::voxel_exact()),
            )
            .unwrap();
        renderer
            .read_display_frame_rgba_for_diagnostics(&frame)
            .unwrap()
    });
    assert_rgba_u8_close(&actual, expected.pixels(), 2, "f32 ISO display");
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn lease_cross_section_integer_f32_and_panel_paths_render_semantic_pages() {
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let viewport = RenderViewport::new(4, 4).unwrap();
    let presentation = PresentationViewport::new(4.0, 4.0).unwrap();
    let u16_values = pattern_u16(shape);
    let f32_values = pattern_f32(shape);
    let integer = fixture(
        IntensityDType::Uint16,
        shape,
        shape,
        &u16_bytes(&u16_values),
        None,
        None,
    );
    let float = fixture(
        IntensityDType::Float32,
        shape,
        shape,
        &f32_bytes(&f32_values),
        None,
        None,
    );
    let region = integer.bridge.required_keys().next().unwrap().region();
    let draws = [GpuCrossSectionChunkDraw {
        resource_region: region,
        panel_bounds: CrossSectionPanelBounds {
            min_points: DVec2::ZERO,
            max_points: DVec2::splat(4.0),
        },
        vertex_count: 4,
        cache_priority: GpuBrickAtlasPagePriority::new(0, 1.0),
    }];
    let integer_transfer = display_transfer([1.0, 0.0, 0.0], 1.0, 0.0, 40_000.0);
    let f32_transfer = display_transfer([0.0, 1.0, 0.0], 1.0, -1.0, 6.0);
    let center = DVec3::splat(1.5);
    let xy = CrossSectionView::new(center, CrossSectionPanel::Xy, DQuat::IDENTITY, 1.0, 1.0);
    let yz = CrossSectionView::new(center, CrossSectionPanel::Yz, DQuat::IDENTITY, 1.0, 1.0);
    let (xy_rgba, yz_rgba, f32_rgba) = with_renderer(|renderer| {
        let xy_frame = renderer
            .render_lease_cross_section_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[GpuLeaseCrossSectionChannel::U16 {
                    volume: integer.volume(),
                    transfer: integer_transfer,
                    chunks: &draws,
                }],
                xy,
                presentation,
                viewport,
            )
            .unwrap();
        assert_eq!(xy_frame.diagnostics.draw_calls, 1);
        assert_eq!(xy_frame.diagnostics.vertex_count, 4);
        let xy_rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&xy_frame)
            .unwrap();
        let yz_frame = renderer
            .render_lease_cross_section_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[GpuLeaseCrossSectionChannel::U16 {
                    volume: integer.volume(),
                    transfer: integer_transfer,
                    chunks: &draws,
                }],
                yz,
                presentation,
                viewport,
            )
            .unwrap();
        let yz_rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&yz_frame)
            .unwrap();
        let f32_frame = renderer
            .render_lease_cross_section_channels_to_display_texture(
                &TestLedger::unlimited(),
                &[GpuLeaseCrossSectionChannel::F32 {
                    volume: float.volume(),
                    transfer: f32_transfer,
                    chunks: &draws,
                }],
                xy,
                presentation,
                viewport,
            )
            .unwrap();
        let f32_rgba = renderer
            .read_display_frame_rgba_for_diagnostics(&f32_frame)
            .unwrap();
        (xy_rgba, yz_rgba, f32_rgba)
    });
    assert!(xy_rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    assert!(yz_rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    assert!(f32_rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    assert_ne!(
        xy_rgba, yz_rgba,
        "XY and YZ panels must sample different planes"
    );
}
