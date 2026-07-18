#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use mirante4d_dataset::{
    DatasetCatalog, DatasetLayer, DatasetResourceIdentity, DatasetResourceKey, DatasetSource,
    DatasetSourceFault, DatasetSourceId, ReservedDecodeSink, ResourceLease, ResourceRegion,
    ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_dataset_runtime::{
    AccountedResourceLease, CancellationGeneration, DatasetRuntime, DatasetRuntimeConfig,
    RequestPriority, ResourceRequest, RuntimeOutcome,
};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, GridToWorld, IntensityDType,
    IsoLightState, IsoShadingPolicy, LayerTransfer, LogicalLayerKey, Opacity, Projection, RgbColor,
    SamplingPolicy, ScaleLevel, Shape3D, Shape4D, TimeIndex, TransferCurve, UnitQuaternion,
    WorldPoint3,
};
use mirante4d_render_api::{
    FrameCompleteness, FrameIdentity, FrameLimitation, FrameProgress, GpuLedgerCategory,
    LayerRenderIntent, PreparedRenderRequirements, PresentationToken, PresentationViewport,
    PresentedFrame, RenderExtent, RenderIntent, RenderPassKind, RenderRequirement,
    RenderRequirementRole, RenderRequirements, RenderViewIntent, VolumePickCompleteness,
    VolumePickHitKind, VolumePickPolicy, VolumePickQuery, VolumePickResult, VolumePickTicket,
    VolumePickValue,
};
use mirante4d_render_reference::{
    NumericalColorFacts, NumericalConformanceContract, NumericalConformanceOracle,
    NumericalCrossSectionPlane, NumericalDvrParameters, NumericalIsoParameters,
    NumericalIsoShading, NumericalPickCompleteness, NumericalPickFacts, NumericalPickKind,
    NumericalSampling, NumericalTransfer, NumericalVolume, NumericalVolumeFacts,
    NumericalVolumeMode, NumericalVolumeQuery, NumericalVoxel, NumericalWorldRay, ReferenceFrame,
    ReferenceRenderer,
};

use super::{
    FrameExecutionReport, GpuFrameTiming, GpuTimingTicket, RetainedFrameRenderPolicy,
    ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntime, WgpuRenderRuntimeConfig,
    WgpuRenderRuntimeDiagnostics, WgpuRenderRuntimeError, preflight_static_presentation_layout,
    preflight_static_presentation_layout_update, prepare_static_presentation_layout,
    prepare_static_presentation_layout_update,
};

const MIB: u64 = 1024 * 1024;
const QUALIFICATION_GPU_BYTES: u64 = 4 * 1024 * MIB;
const PERFORMANCE_GPU_BYTES: u64 = 128 * MIB;
const SMALL_GPU_BYTES: u64 = 11 * MIB;
const SOURCE_ID: DatasetSourceId = DatasetSourceId::new(0x5750_3039_4100_0001);
const REQUEST_SCOPE: u64 = 0x5750_3039_4100_0002;
const SEMANTIC_FIXTURE_ID: &str = "wp09a-semantic-small";
const UPLOAD_FIXTURE_ID: &str = "wp09a-upload-boundary";
const WORK_FIXTURE_ID: &str = "wp09a-work-boundary";
const HIGH_COUNT_WORK_RESOURCES: usize = 32_768;

#[derive(Clone)]
struct PayloadBytes {
    values: Arc<[u8]>,
    validity: Option<Arc<[u8]>>,
}

struct FixtureSource {
    catalog: Arc<DatasetCatalog>,
    payloads: BTreeMap<DatasetResourceKey, PayloadBytes>,
    blocked_key: DatasetResourceKey,
    block_enabled: AtomicBool,
    block_entered: AtomicBool,
}

impl FixtureSource {
    fn release_block(&self) {
        self.block_enabled.store(false, Ordering::Release);
    }
}

impl DatasetSource for FixtureSource {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
        Ok(Arc::clone(&self.catalog))
    }

    #[allow(
        clippy::result_large_err,
        reason = "the trait fixes the per-sink scientific source-fault result type"
    )]
    fn decode_cohort_into(
        &self,
        sinks: &mut [&mut dyn ReservedDecodeSink],
    ) -> Vec<Result<(), DatasetSourceFault>> {
        sinks
            .iter_mut()
            .map(|sink| {
                let sink = &mut **sink;
                let key = sink.resource_key();
                self.catalog
                    .validate_decode_reservation(sink)
                    .map_err(|reason| DatasetSourceFault::InvalidResource {
                        key,
                        reason: Box::new(reason),
                    })?;
                let payload = self
                    .payloads
                    .get(&key)
                    .ok_or(DatasetSourceFault::ResourceUnavailable { key })?;

                if key == self.blocked_key && self.block_enabled.load(Ordering::Acquire) {
                    self.block_entered.store(true, Ordering::Release);
                    while self.block_enabled.load(Ordering::Acquire) && !sink.is_cancelled() {
                        std::thread::yield_now();
                    }
                }
                if sink.is_cancelled() {
                    return Err(DatasetSourceFault::Cancelled { key });
                }

                sink.write(&payload.values)
                    .and_then(|()| {
                        if let Some(validity) = &payload.validity {
                            sink.write(validity)
                        } else {
                            Ok(())
                        }
                    })
                    .and_then(|()| sink.finish())
                    .map_err(|reason| DatasetSourceFault::SinkRejected {
                        key,
                        reason: Box::new(reason),
                    })
            })
            .collect()
    }
}

struct QualificationFixtures {
    source: Arc<FixtureSource>,
    semantic: [Vec<DatasetResourceKey>; 3],
    missing_u8: DatasetResourceKey,
    upload: Vec<DatasetResourceKey>,
    work: Vec<DatasetResourceKey>,
    multichannel: [DatasetResourceKey; 3],
    adversarial: Vec<DatasetResourceKey>,
    empty: DatasetResourceKey,
    sparse_off_ray: DatasetResourceKey,
    affine: DatasetResourceKey,
}

fn layer(
    ordinal: u32,
    label: &str,
    shape: [u64; 3],
    dtype: IntensityDType,
    validity: ResourceValidity,
) -> DatasetLayer {
    layer_with_transform(
        ordinal,
        label,
        shape,
        dtype,
        validity,
        GridToWorld::identity(),
    )
}

fn layer_with_transform(
    ordinal: u32,
    label: &str,
    shape: [u64; 3],
    dtype: IntensityDType,
    validity: ResourceValidity,
    grid_to_world: GridToWorld,
) -> DatasetLayer {
    DatasetLayer::new(
        LogicalLayerKey::new(ordinal),
        label,
        Shape4D::new(1, shape[0], shape[1], shape[2]).expect("fixture shape is valid"),
        dtype,
        grid_to_world,
        validity,
    )
    .expect("fixture layer is valid")
}

fn resource_key(layer: u32, origin: [u64; 3], shape: [u64; 3]) -> DatasetResourceKey {
    DatasetResourceKey::new(
        DatasetResourceIdentity::Unverified(SOURCE_ID),
        LogicalLayerKey::new(layer),
        TimeIndex::new(0),
        ScaleLevel::BASE,
        ResourceRegion::new(
            origin,
            Shape3D::new(shape[0], shape[1], shape[2]).expect("fixture shape is valid"),
        )
        .expect("fixture region is valid"),
    )
}

fn set_valid(bits: &mut [u8], index: usize, valid: bool) {
    let mask = 1_u8 << (index % 8);
    if valid {
        bits[index / 8] |= mask;
    } else {
        bits[index / 8] &= !mask;
    }
}

fn build_semantic_payload(dtype: IntensityDType, origin: [u64; 3]) -> PayloadBytes {
    const EDGE: usize = 16;
    const SAMPLES: usize = EDGE * EDGE * EDGE;
    let mut values = Vec::with_capacity(SAMPLES * usize::from(dtype.bytes_per_sample()));
    let mut validity = vec![0_u8; SAMPLES.div_ceil(8)];
    for z in 0..EDGE {
        for y in 0..EDGE {
            for x in 0..EDGE {
                let global_z = origin[0] as usize + z;
                let global_y = origin[1] as usize + y;
                let global_x = origin[2] as usize + x;
                let linear = global_z * 32 * 32 + global_y * 32 + global_x;
                match dtype {
                    IntensityDType::Uint8 => {
                        values.push(((global_x * 17 + global_y * 3 + global_z * 5) % 256) as u8)
                    }
                    IntensityDType::Uint16 => {
                        values.extend_from_slice(&((linear as u16) * 2).to_le_bytes());
                    }
                    IntensityDType::Float32 => {
                        values.extend_from_slice(&(linear as f32 / 32_767.0).to_le_bytes());
                    }
                }
                let local = z * EDGE * EDGE + y * EDGE + x;
                set_valid(
                    &mut validity,
                    local,
                    !(global_z == 0 && global_y == 0 && global_x == 1),
                );
            }
        }
    }
    PayloadBytes {
        values: values.into(),
        validity: Some(validity.into()),
    }
}

fn build_fixtures() -> QualificationFixtures {
    let catalog = Arc::new(
        DatasetCatalog::new(
            "wp09a qualification fixtures",
            ScientificIdentityStatus::Unverified(SOURCE_ID),
            vec![
                layer(
                    0,
                    SEMANTIC_FIXTURE_ID,
                    [32, 32, 32],
                    IntensityDType::Uint8,
                    ResourceValidity::BitMask,
                ),
                layer(
                    1,
                    SEMANTIC_FIXTURE_ID,
                    [32, 32, 32],
                    IntensityDType::Uint16,
                    ResourceValidity::BitMask,
                ),
                layer(
                    2,
                    SEMANTIC_FIXTURE_ID,
                    [32, 32, 32],
                    IntensityDType::Float32,
                    ResourceValidity::BitMask,
                ),
                layer(
                    3,
                    UPLOAD_FIXTURE_ID,
                    [64, 192, 192],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    4,
                    WORK_FIXTURE_ID,
                    [1, 1, 1_000_000],
                    IntensityDType::Uint8,
                    ResourceValidity::AllValid,
                ),
                layer(
                    5,
                    "multichannel-red",
                    [8, 8, 8],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    6,
                    "multichannel-green",
                    [8, 8, 8],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer_with_transform(
                    7,
                    "multichannel-near-green",
                    [8, 8, 8],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                    GridToWorld::from_row_major([
                        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 0.0,
                        1.0,
                    ])
                    .expect("translated fixture transform is valid"),
                ),
                layer(
                    8,
                    "adversarial-deep-mip",
                    [512, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    9,
                    "all-invalid-sparse-page",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::BitMask,
                ),
                layer(
                    10,
                    "sparse-validity-off-ray",
                    [8, 8, 8],
                    IntensityDType::Float32,
                    ResourceValidity::BitMask,
                ),
                layer_with_transform(
                    11,
                    "vp04-full-affine",
                    [8, 8, 8],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                    GridToWorld::from_row_major([
                        0.0, -1.0, 0.25, 10.0, 1.0, 0.0, 0.0, 2.0, 0.2, 0.0, 1.0, 1.0, 0.0, 0.0,
                        0.0, 1.0,
                    ])
                    .expect("rotated, sheared, translated fixture transform is valid"),
                ),
            ],
        )
        .expect("fixture catalog is valid"),
    );
    let mut payloads = BTreeMap::new();
    let mut semantic: [Vec<DatasetResourceKey>; 3] = std::array::from_fn(|_| Vec::new());
    let dtypes = [
        IntensityDType::Uint8,
        IntensityDType::Uint16,
        IntensityDType::Float32,
    ];
    let mut semantic_bytes = 0_u64;
    for (layer_index, dtype) in dtypes.into_iter().enumerate() {
        for z in [0_u64, 16] {
            for y in [0_u64, 16] {
                for x in [0_u64, 16] {
                    let key = resource_key(layer_index as u32, [z, y, x], [16, 16, 16]);
                    let payload = build_semantic_payload(dtype, [z, y, x]);
                    semantic_bytes += payload.values.len() as u64
                        + payload
                            .validity
                            .as_ref()
                            .map_or(0, |bits| bits.len() as u64);
                    assert!(payloads.insert(key, payload).is_none());
                    semantic[layer_index].push(key);
                }
            }
        }
    }
    assert_eq!(semantic.iter().map(Vec::len).sum::<usize>(), 24);
    assert_eq!(semantic_bytes, 241_664);
    let missing_u8 = semantic[0]
        .iter()
        .copied()
        .find(|key| key.region().origin() == [0, 0, 16])
        .expect("the missing semantic brick exists");

    let mut upload = Vec::new();
    for y in [0_u64, 64, 128] {
        for x in [0_u64, 64, 128] {
            let key = resource_key(3, [0, y, x], [64, 64, 64]);
            let sample_count = 64_usize * 64 * 64;
            let mut values = Vec::with_capacity(sample_count * 4);
            for index in 0..sample_count {
                let value = (index % 1024) as f32 / 1023.0;
                values.extend_from_slice(&value.to_le_bytes());
            }
            assert_eq!(values.len(), MIB as usize);
            assert!(
                payloads
                    .insert(
                        key,
                        PayloadBytes {
                            values: values.into(),
                            validity: None,
                        },
                    )
                    .is_none()
            );
            upload.push(key);
        }
    }
    assert_eq!(upload.len(), 9);

    let mut work = Vec::new();
    for x in 0_u64..HIGH_COUNT_WORK_RESOURCES as u64 {
        let key = resource_key(4, [0, 0, x], [1, 1, 1]);
        assert!(
            payloads
                .insert(
                    key,
                    PayloadBytes {
                        values: Arc::from([x as u8]),
                        validity: None,
                    },
                )
                .is_none()
        );
        work.push(key);
    }
    assert_eq!(work.len(), HIGH_COUNT_WORK_RESOURCES);

    let multichannel = std::array::from_fn(|index| {
        let key = resource_key(5 + index as u32, [0, 0, 0], [8, 8, 8]);
        let value = if index == 0 { 0.25_f32 } else { 0.75_f32 };
        let mut values = Vec::with_capacity(8 * 8 * 8 * 4);
        for _ in 0..8 * 8 * 8 {
            values.extend_from_slice(&value.to_le_bytes());
        }
        assert!(
            payloads
                .insert(
                    key,
                    PayloadBytes {
                        values: values.into(),
                        validity: None,
                    },
                )
                .is_none()
        );
        key
    });

    let mut adversarial = Vec::new();
    for z in (0_u64..512).step_by(64) {
        let key = resource_key(8, [z, 0, 0], [64, 64, 64]);
        // The identity camera enters at high z. Maxima therefore increase at
        // every crossed brick and defeat MIP min/max rejection for all 512
        // required samples on every covered ray.
        let value = (8 - z / 64) as f32 / 8.0;
        let mut values = Vec::with_capacity(MIB as usize);
        for _ in 0..64 * 64 * 64 {
            values.extend_from_slice(&value.to_le_bytes());
        }
        assert_eq!(values.len(), MIB as usize);
        assert!(
            payloads
                .insert(
                    key,
                    PayloadBytes {
                        values: values.into(),
                        validity: None,
                    },
                )
                .is_none()
        );
        adversarial.push(key);
    }

    let empty = resource_key(9, [0, 0, 0], [64, 64, 64]);
    let sample_count = 64_usize * 64 * 64;
    let mut empty_values = Vec::with_capacity(sample_count * 4);
    for _ in 0..sample_count {
        // Deliberately nonzero: validity, not sample value, makes this page
        // empty and must prevent any payload-arena copy.
        empty_values.extend_from_slice(&0.75_f32.to_le_bytes());
    }
    assert!(
        payloads
            .insert(
                empty,
                PayloadBytes {
                    values: empty_values.into(),
                    validity: Some(vec![0_u8; sample_count.div_ceil(8)].into()),
                },
            )
            .is_none()
    );

    let affine = resource_key(11, [0, 0, 0], [8, 8, 8]);
    let mut affine_values = Vec::with_capacity(8 * 8 * 8 * 4);
    for z in 0..8 {
        for y in 0..8 {
            for x in 0..8 {
                let value = (x + 8 * y + 64 * z) as f32 / 511.0;
                affine_values.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    assert!(
        payloads
            .insert(
                affine,
                PayloadBytes {
                    values: affine_values.into(),
                    validity: None,
                },
            )
            .is_none()
    );

    let sparse_off_ray = resource_key(10, [0, 0, 0], [8, 8, 8]);
    let sparse_samples = 8_usize * 8 * 8;
    let sparse_values = vec![0_u8; sparse_samples * 4];
    let mut sparse_validity = vec![0_u8; sparse_samples.div_ceil(8)];
    set_valid(&mut sparse_validity, 0, true);
    assert!(
        payloads
            .insert(
                sparse_off_ray,
                PayloadBytes {
                    values: sparse_values.into(),
                    validity: Some(sparse_validity.into()),
                },
            )
            .is_none()
    );

    let blocked_key = work[128];
    let source = Arc::new(FixtureSource {
        catalog,
        payloads,
        blocked_key,
        block_enabled: AtomicBool::new(true),
        block_entered: AtomicBool::new(false),
    });
    QualificationFixtures {
        source,
        semantic,
        missing_u8,
        upload,
        work,
        multichannel,
        adversarial,
        empty,
        sparse_off_ray,
        affine,
    }
}

fn start_dataset_runtime(
    source: &Arc<FixtureSource>,
) -> (Arc<dyn DatasetRuntime>, Arc<DatasetCatalog>) {
    let config = DatasetRuntimeConfig::new(64 * MIB, 4, 256, 256)
        .expect("fixture runtime configuration is valid");
    let source = Arc::clone(source);
    <dyn DatasetRuntime>::start(config, move |_ledger| {
        let source: Arc<dyn DatasetSource> = source;
        Ok(source)
    })
    .expect("fixture dataset runtime starts")
}

fn load_keys(
    runtime: &Arc<dyn DatasetRuntime>,
    keys: &[DatasetResourceKey],
    generation: CancellationGeneration,
    deadline: Instant,
) -> BTreeMap<DatasetResourceKey, AccountedResourceLease> {
    for key in keys {
        runtime
            .submit(ResourceRequest::new(
                *key,
                RequestPriority::CurrentView,
                generation,
            ))
            .expect("fixture resource request is admitted");
    }
    let mut leases = BTreeMap::new();
    while leases.len() < keys.len() {
        assert!(
            Instant::now() < deadline,
            "dataset fixture decode exceeded 60 seconds"
        );
        for completion in runtime.poll(256).expect("fixture completions poll") {
            match completion.outcome() {
                RuntimeOutcome::Ready(lease) => {
                    leases.insert(completion.ticket().resource(), lease.clone());
                }
                RuntimeOutcome::Cancelled => panic!("current fixture request was cancelled"),
                RuntimeOutcome::Failed(fault) => panic!("fixture request failed: {fault}"),
            }
        }
        std::thread::yield_now();
    }
    leases
}

fn prove_cancellation(
    fixtures: &QualificationFixtures,
    runtime: &Arc<dyn DatasetRuntime>,
    deadline: Instant,
) -> CancellationGeneration {
    let old = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let current = CancellationGeneration::for_scope(REQUEST_SCOPE, 2);
    let ticket = runtime
        .submit(ResourceRequest::new(
            fixtures.source.blocked_key,
            RequestPriority::CurrentView,
            old,
        ))
        .expect("cancellation fixture request is admitted");
    while !fixtures.source.block_entered.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "cancellation fixture did not enter decode"
        );
        std::thread::yield_now();
    }
    runtime
        .cancel_before(current)
        .expect("current cancellation generation is accepted");
    loop {
        assert!(
            Instant::now() < deadline,
            "cancellation fixture did not terminate"
        );
        for completion in runtime.poll(256).expect("cancellation completion poll") {
            if completion.ticket().id() == ticket.id() {
                assert!(matches!(completion.outcome(), RuntimeOutcome::Cancelled));
                fixtures.source.release_block();
                return current;
            }
        }
        std::thread::yield_now();
    }
}

fn transfer(low: f32, high: f32) -> LayerTransfer {
    transfer_color(low, high, [1.0, 0.0, 0.0])
}

fn transfer_color(low: f32, high: f32, color: [f32; 3]) -> LayerTransfer {
    transfer_color_opacity(low, high, color, 1.0)
}

fn transfer_color_opacity(low: f32, high: f32, color: [f32; 3], opacity: f32) -> LayerTransfer {
    LayerTransfer::new(
        DisplayWindow::new(low, high).expect("fixture window is valid"),
        RgbColor::new(color).expect("fixture color is valid"),
        Opacity::new(opacity).expect("fixture opacity is valid"),
        TransferCurve::linear(),
        false,
    )
}

fn volume_view() -> RenderViewIntent {
    volume_view_for([15.5, 15.5, 15.5], 0.5, 40.0)
}

fn volume_view_for(target: [f64; 3], scale: f64, distance: f64) -> RenderViewIntent {
    volume_view_for_light(target, scale, distance, IsoLightState::attached_camera())
}

fn volume_view_for_light(
    target: [f64; 3],
    scale: f64,
    distance: f64,
    iso_light: IsoLightState,
) -> RenderViewIntent {
    volume_view_for_projection_light(Projection::Orthographic, target, scale, distance, iso_light)
}

fn volume_view_for_projection_light(
    projection: Projection,
    target: [f64; 3],
    scale: f64,
    distance: f64,
    iso_light: IsoLightState,
) -> RenderViewIntent {
    RenderViewIntent::volume(
        CameraView::new(
            projection,
            WorldPoint3::new(target[0], target[1], target[2]).expect("fixture target is finite"),
            UnitQuaternion::identity(),
            scale,
            320.0,
            distance,
        )
        .expect("fixture camera is valid"),
        iso_light,
    )
}

fn cross_section_view(center: [f64; 3], scale: f64) -> RenderViewIntent {
    RenderViewIntent::cross_section(
        CrossSectionView::new(
            WorldPoint3::new(center[0], center[1], center[2]).expect("fixture center is finite"),
            UnitQuaternion::identity(),
            scale,
            1.0,
        )
        .expect("fixture cross-section is valid"),
    )
}

fn intent_and_requirements(
    frame: u64,
    layer: u32,
    render_state: mirante4d_domain::RenderState,
    view: RenderViewIntent,
    extent: RenderExtent,
    keys: &[DatasetResourceKey],
) -> (RenderIntent, RenderRequirements) {
    let presentation = PresentationViewport::new(
        f64::from(extent.width_pixels()),
        f64::from(extent.height_pixels()),
    )
    .expect("fixture presentation is valid");
    let transfer = match layer {
        0 | 4 => transfer(0.0, 255.0),
        1 => transfer(0.0, 65_535.0),
        2 | 3 | 8 | 9 | 10 | 11 => transfer(0.0, 1.0),
        _ => panic!("unknown qualification layer"),
    };
    let intent = RenderIntent::new(
        FrameIdentity::new(frame),
        DatasetResourceIdentity::Unverified(SOURCE_ID),
        TimeIndex::new(0),
        view,
        presentation,
        extent,
        vec![LayerRenderIntent::new(
            LogicalLayerKey::new(layer),
            transfer,
            render_state,
        )],
    )
    .expect("fixture render intent is valid");
    let requirements = RenderRequirements::new(
        &intent,
        keys.iter()
            .enumerate()
            .map(|(index, key)| {
                RenderRequirement::new(
                    *key,
                    if index == 0 {
                        RenderRequirementRole::FirstUsefulFrame
                    } else {
                        RenderRequirementRole::Refinement
                    },
                )
            })
            .collect(),
    )
    .expect("fixture requirements are valid");
    (intent, requirements)
}

fn multichannel_intent_and_requirements(
    frame: u64,
    view: RenderViewIntent,
    extent: RenderExtent,
    layers: Vec<LayerRenderIntent>,
    keys: &[DatasetResourceKey],
) -> (RenderIntent, RenderRequirements) {
    let presentation = PresentationViewport::new(
        f64::from(extent.width_pixels()),
        f64::from(extent.height_pixels()),
    )
    .expect("fixture presentation is valid");
    let intent = RenderIntent::new(
        FrameIdentity::new(frame),
        DatasetResourceIdentity::Unverified(SOURCE_ID),
        TimeIndex::new(0),
        view,
        presentation,
        extent,
        layers,
    )
    .expect("multichannel fixture render intent is valid");
    let requirements = RenderRequirements::new(
        &intent,
        keys.iter()
            .enumerate()
            .map(|(index, key)| {
                RenderRequirement::new(
                    *key,
                    if index == 0 {
                        RenderRequirementRole::FirstUsefulFrame
                    } else {
                        RenderRequirementRole::Refinement
                    },
                )
            })
            .collect(),
    )
    .expect("multichannel fixture requirements are valid");
    (intent, requirements)
}

fn borrowed_leases<'a>(
    keys: &[DatasetResourceKey],
    leases: &'a BTreeMap<DatasetResourceKey, AccountedResourceLease>,
    omit: Option<DatasetResourceKey>,
) -> Vec<&'a dyn ResourceLease> {
    keys.iter()
        .filter(|key| Some(**key) != omit)
        .map(|key| leases.get(key).expect("fixture lease exists") as &dyn ResourceLease)
        .collect()
}

fn poll_capture(
    runtime: &mut WgpuRenderRuntime,
    ticket: ValidationCaptureTicket,
    deadline: Instant,
) -> ValidationCapture {
    loop {
        assert!(
            Instant::now() < deadline,
            "asynchronous GPU readback exceeded 60 seconds"
        );
        match runtime
            .poll_validation_capture(ticket)
            .expect("validation capture polling succeeds")
        {
            Some(capture) => return capture,
            None => std::thread::yield_now(),
        }
    }
}

fn poll_gpu_timing(
    runtime: &mut WgpuRenderRuntime,
    ticket: GpuTimingTicket,
    deadline: Instant,
) -> GpuFrameTiming {
    loop {
        assert!(
            Instant::now() < deadline,
            "asynchronous GPU timing exceeded its deadline"
        );
        match runtime
            .poll_gpu_timing(ticket)
            .expect("GPU timing polling succeeds")
        {
            Some(timing) => return timing,
            None => std::thread::yield_now(),
        }
    }
}

fn poll_pick(
    runtime: &mut WgpuRenderRuntime,
    ticket: VolumePickTicket,
    deadline: Instant,
) -> VolumePickResult {
    loop {
        assert!(
            Instant::now() < deadline,
            "asynchronous GPU pick exceeded its deadline"
        );
        match runtime
            .poll_pick(ticket)
            .expect("GPU pick polling succeeds")
        {
            Some(result) => return result,
            None => std::thread::yield_now(),
        }
    }
}

fn percentile(sorted: &[u64], percentile: f64) -> u64 {
    assert!(!sorted.is_empty());
    let rank = (percentile * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn encode_payload_benchmark_pass(
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
    queries: &wgpu::QuerySet,
    query_start: u32,
) {
    let attachment = Some(wgpu::RenderPassColorAttachment {
        view: target,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            store: wgpu::StoreOp::Store,
        },
    });
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mirante4d-payload-representation-benchmark"),
        color_attachments: &[attachment],
        depth_stencil_attachment: None,
        timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
            query_set: queries,
            beginning_of_pass_write_index: Some(query_start),
            end_of_pass_write_index: Some(query_start + 1),
        }),
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, bind_group, &[]);
    pass.draw(0..3, 0..1);
}

fn compare_reference(capture: &ValidationCapture, reference: &ReferenceFrame) -> u8 {
    assert_eq!(capture.extent(), reference.extent());
    assert_exact_bytes("coverage", capture.coverage(), reference.coverage());
    assert_exact_bytes("validity", capture.validity(), reference.validity());
    assert_eq!(capture.rgba8().len(), reference.rgba8().len());
    let max_delta = capture
        .rgba8()
        .iter()
        .zip(reference.rgba8())
        .map(|(actual, expected)| actual.abs_diff(*expected))
        .max()
        .unwrap_or(0);
    assert!(max_delta <= 1, "GPU/reference RGBA8 delta was {max_delta}");
    max_delta
}

fn assert_exact_bytes(label: &str, actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len(), "{label} length differs");
    if let Some((index, (actual, expected))) = actual
        .iter()
        .zip(expected)
        .enumerate()
        .find(|(_, (actual, expected))| actual != expected)
    {
        panic!("{label} differs at byte {index}: actual={actual}, expected={expected}");
    }
}

fn pixel(capture: &ValidationCapture, x: u32, y: u32) -> ([u8; 4], u8, u8) {
    let width = capture.extent().width_pixels() as usize;
    let index = y as usize * width + x as usize;
    let start = index * 4;
    (
        capture.rgba8()[start..start + 4]
            .try_into()
            .expect("one RGBA8 pixel"),
        capture.coverage()[index],
        capture.validity()[index],
    )
}

fn assert_pixel_near(
    capture: &ValidationCapture,
    x: u32,
    y: u32,
    expected_rgba8: [u8; 4],
    expected_coverage: u8,
    expected_validity: u8,
) {
    let (actual, coverage, validity) = pixel(capture, x, y);
    assert_eq!((coverage, validity), (expected_coverage, expected_validity));
    let max_delta = actual
        .into_iter()
        .zip(expected_rgba8)
        .map(|(actual, expected)| actual.abs_diff(expected))
        .max()
        .unwrap_or(0);
    assert!(
        max_delta <= 1,
        "GPU pixel {actual:?} differs from independent fact {expected_rgba8:?} by {max_delta}"
    );
}

fn ep00_semantic_f32_volume() -> NumericalVolume {
    let mut voxels = Vec::with_capacity(32 * 32 * 32);
    for z in 0_u32..32 {
        for y in 0_u32..32 {
            for x in 0_u32..32 {
                if [x, y, z] == [1, 0, 0] {
                    voxels.push(NumericalVoxel::Invalid);
                } else {
                    let linear = z * 32 * 32 + y * 32 + x;
                    let encoded = linear as f32 / 32_767.0_f32;
                    voxels.push(NumericalVoxel::Valid(f64::from(encoded)));
                }
            }
        }
    }
    NumericalVolume::new([32, 32, 32], GridToWorld::identity(), voxels)
        .expect("the independent EP-00 semantic volume is bounded and finite")
}

fn ep00_primary_perspective_view() -> RenderViewIntent {
    RenderViewIntent::volume(
        CameraView::new(
            Projection::Perspective,
            WorldPoint3::new(-4.5, 15.25, 15.5).expect("the EP-00 perspective target is finite"),
            UnitQuaternion::identity(),
            0.5,
            32.0,
            40.0,
        )
        .expect("the EP-00 perspective camera is valid"),
        IsoLightState::attached_camera(),
    )
}

fn ep00_primary_perspective_ray() -> NumericalWorldRay {
    // For the 65x65 viewport, render pixel (48, 32) is exactly 16 screen
    // points to the right of center. With a 32-point focal length the
    // independently reconstructed direction is therefore (0.5, 0, -1)
    // because the canonical identity camera looks down world Z.
    NumericalWorldRay::new([-4.5, 15.25, 55.5], [0.5, 0.0, -1.0])
        .expect("the independent EP-00 perspective ray is finite")
}

fn ep00_primary_sample_step_world() -> f64 {
    // Identity grid, unit world ray (1/sqrt(5), 0, 2/sqrt(5)): advance one
    // voxel along its dominant grid axis while retaining physical distance.
    5.0_f64.sqrt() / 2.0
}

fn ep00_numerical_transfer(window: [f64; 2], color: [f64; 3], opacity: f64) -> NumericalTransfer {
    NumericalTransfer::new(window, 1.0, false, color, opacity)
        .expect("the EP-00 numerical transfer is valid")
}

fn assert_numerical_color(
    label: &str,
    capture: &ValidationCapture,
    pixel_xy: [u32; 2],
    expected: NumericalColorFacts,
) {
    let contract = NumericalConformanceContract::ep00();
    let (observed_rgba8, observed_coverage, observed_validity) =
        pixel(capture, pixel_xy[0], pixel_xy[1]);
    assert_eq!(
        observed_coverage,
        u8::from(expected.covered()),
        "{label} coverage differs from the independent oracle"
    );
    assert_eq!(
        observed_validity,
        u8::from(expected.valid()),
        "{label} validity differs from the independent oracle"
    );
    assert!(
        contract.rgba8_matches(expected.rgba8(), observed_rgba8),
        "{label} RGBA8 differs from the independent oracle: observed={observed_rgba8:?}, expected={:?}, tolerance={}",
        expected.rgba8(),
        contract.rgba8_channel_tolerance(),
    );
}

fn assert_numerical_pick(label: &str, observed: VolumePickResult, expected: NumericalPickFacts) {
    let contract = NumericalConformanceContract::ep00();
    let expected_kind = match expected.kind() {
        NumericalPickKind::Voxel => VolumePickHitKind::Voxel,
        NumericalPickKind::InterpolatedSample => VolumePickHitKind::InterpolatedSample,
    };
    let expected_completeness = match expected.completeness() {
        NumericalPickCompleteness::Exact => VolumePickCompleteness::Exact,
        NumericalPickCompleteness::Incomplete => VolumePickCompleteness::Incomplete,
    };
    assert_eq!(observed.kind(), expected_kind, "{label} pick kind differs");
    assert_eq!(
        observed.completeness(),
        expected_completeness,
        "{label} pick completeness differs"
    );
    let observed_world = observed
        .world_position()
        .unwrap_or_else(|| panic!("{label} expected a world position"))
        .components();
    assert!(
        contract.world_position_matches(expected.world_position(), observed_world),
        "{label} pick world position differs: observed={observed_world:?}, expected={:?}",
        expected.world_position(),
    );
    let observed_distance = observed
        .ray_distance_world()
        .unwrap_or_else(|| panic!("{label} expected a physical ray distance"));
    assert!(
        contract.ray_distance_matches(expected.ray_distance_world(), observed_distance),
        "{label} pick distance differs: observed={observed_distance}, expected={}",
        expected.ray_distance_world(),
    );
    let observed_value = match observed.value() {
        Some(VolumePickValue::IntensityF32(value)) => f64::from(value),
        other => panic!("{label} expected one finite f32 pick value, observed {other:?}"),
    };
    assert!(
        contract.scalar_matches(expected.value(), observed_value),
        "{label} pick value differs: observed={observed_value}, expected={}",
        expected.value(),
    );
}

fn ep00_volume_facts(
    volume: &NumericalVolume,
    transfer: NumericalTransfer,
    mode: NumericalVolumeMode,
) -> NumericalVolumeFacts {
    NumericalConformanceOracle::new()
        .volume(
            volume,
            NumericalVolumeQuery::new(
                ep00_primary_perspective_ray(),
                NumericalSampling::VoxelExact,
                transfer,
                mode,
                ep00_primary_sample_step_world(),
            )
            .expect("the independent EP-00 volume query is valid"),
        )
        .expect("the independent EP-00 volume facts are bounded")
}

#[derive(Default)]
struct Counters {
    frames: u64,
    resources_visited: u64,
    resources_uploaded: u64,
    payload_upload_bytes: u64,
    control_upload_bytes: u64,
    command_buffers: u64,
    queue_submissions: u64,
    max_resources_visited: u64,
    max_resources_uploaded: u64,
    max_payload_upload_bytes: u64,
    max_control_upload_bytes: u64,
    max_command_buffers: u64,
    max_queue_submissions: u64,
    captures: u64,
}

impl Counters {
    fn record(&mut self, report: &FrameExecutionReport) {
        let resources_visited = report.visited_resources() as u64;
        let resources_uploaded = report.uploaded_resources() as u64;
        let payload_upload_bytes = report.payload_upload_bytes();
        let control_upload_bytes = report.control_upload_bytes();
        let command_buffers = u64::from(report.command_buffers());
        let queue_submissions = u64::from(report.queue_submissions());
        self.frames += 1;
        self.resources_visited += resources_visited;
        self.resources_uploaded += resources_uploaded;
        self.payload_upload_bytes += payload_upload_bytes;
        self.control_upload_bytes += control_upload_bytes;
        self.command_buffers += command_buffers;
        self.queue_submissions += queue_submissions;
        self.max_resources_visited = self.max_resources_visited.max(resources_visited);
        self.max_resources_uploaded = self.max_resources_uploaded.max(resources_uploaded);
        self.max_payload_upload_bytes = self.max_payload_upload_bytes.max(payload_upload_bytes);
        self.max_control_upload_bytes = self.max_control_upload_bytes.max(control_upload_bytes);
        self.max_command_buffers = self.max_command_buffers.max(command_buffers);
        self.max_queue_submissions = self.max_queue_submissions.max(queue_submissions);
        self.captures += u64::from(report.validation_capture().is_some());
    }
}

struct ComparisonInput<'a> {
    catalog: &'a DatasetCatalog,
    intent: &'a RenderIntent,
    requirements: &'a RenderRequirements,
    leases: &'a [&'a dyn ResourceLease],
}

fn execute_and_compare(
    gpu: &mut WgpuRenderRuntime,
    presentation: PresentationToken,
    input: ComparisonInput<'_>,
    deadline: Instant,
    counters: &mut Counters,
) -> (ValidationCapture, u8) {
    let report = gpu
        .execute_frame(
            presentation,
            input.catalog,
            input.intent,
            input.requirements,
            input.leases,
        )
        .expect("semantic GPU frame executes");
    counters.record(&report);
    let ticket = report
        .validation_capture()
        .expect("qualification enables asynchronous validation capture");
    let capture = poll_capture(gpu, ticket, deadline);
    let reference = ReferenceRenderer::new()
        .render(input.catalog, input.intent, input.leases)
        .expect("independent CPU reference renders fixture leases");
    let max_delta = compare_reference(&capture, &reference);
    (capture, max_delta)
}

fn execute_and_capture(
    gpu: &mut WgpuRenderRuntime,
    presentation: PresentationToken,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    leases: &[&dyn ResourceLease],
    deadline: Instant,
) -> ValidationCapture {
    let report = gpu
        .execute_frame(presentation, catalog, intent, requirements, leases)
        .expect("multichannel GPU frame executes");
    assert!(!report.deferred_by_backpressure());
    assert_eq!(
        report
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    poll_capture(
        gpu,
        report
            .validation_capture()
            .expect("multichannel validation capture exists"),
        deadline,
    )
}

fn execute_presented_and_capture(
    gpu: &mut WgpuRenderRuntime,
    presentation: PresentationToken,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    leases: &[&dyn ResourceLease],
    deadline: Instant,
) -> (PresentedFrame, ValidationCapture) {
    let report = gpu
        .execute_frame(presentation, catalog, intent, requirements, leases)
        .expect("numerical-conformance GPU frame executes");
    assert!(!report.deferred_by_backpressure());
    assert_eq!(
        report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let presented = report
        .presentation()
        .cloned()
        .expect("numerical-conformance frame is presented");
    let capture = poll_capture(
        gpu,
        report
            .validation_capture()
            .expect("numerical-conformance validation capture exists"),
        deadline,
    );
    (presented, capture)
}

fn sanitize_evidence_text(text: &str) -> String {
    let sanitized = text
        .chars()
        .take(256)
        .map(|character| {
            if character.is_control() || matches!(character, '/' | '\\' | '"') {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_owned()
    } else {
        sanitized
    }
}

fn ledger_json(diagnostics: &WgpuRenderRuntimeDiagnostics) -> String {
    format!(
        concat!(
            "{{\"configured_bytes\":{},\"payload_residency_capacity_bytes\":{},",
            "\"transfer_staging_capacity_bytes\":{},",
            "\"display_page_table_scratch_capacity_bytes\":{},",
            "\"peak_payload_residency_bytes\":{},\"peak_transfer_staging_bytes\":{},",
            "\"peak_display_target_bytes\":{},\"peak_page_table_bytes\":{},",
            "\"peak_scratch_bytes\":{}}}"
        ),
        diagnostics.gpu_budget_bytes(),
        diagnostics.payload_capacity_bytes(),
        diagnostics.transfer_capacity_bytes(),
        diagnostics.other_capacity_bytes(),
        diagnostics
            .peak_resident_payload_bytes()
            .max(diagnostics.payload_arena_allocated_bytes()),
        diagnostics.peak_transfer_bytes(),
        diagnostics.peak_display_target_bytes(),
        diagnostics.peak_page_table_bytes(),
        diagnostics.peak_scratch_bytes(),
    )
}

fn counters_json(counters: &Counters) -> String {
    format!(
        concat!(
            "{{\"frames\":{},\"resources_visited\":{},\"resources_uploaded\":{},",
            "\"payload_upload_bytes\":{},\"control_upload_bytes\":{},",
            "\"command_buffers\":{},\"queue_submissions\":{},",
            "\"max_resources_visited\":{},\"max_resources_uploaded\":{},",
            "\"max_payload_upload_bytes\":{},\"max_control_upload_bytes\":{},",
            "\"max_command_buffers\":{},\"max_queue_submissions\":{}}}"
        ),
        counters.frames,
        counters.resources_visited,
        counters.resources_uploaded,
        counters.payload_upload_bytes,
        counters.control_upload_bytes,
        counters.command_buffers,
        counters.queue_submissions,
        counters.max_resources_visited,
        counters.max_resources_uploaded,
        counters.max_payload_upload_bytes,
        counters.max_control_upload_bytes,
        counters.max_command_buffers,
        counters.max_queue_submissions,
    )
}

fn emit_evidence(
    diagnostics: &WgpuRenderRuntimeDiagnostics,
    counters: &Counters,
    capacity_diagnostics: &WgpuRenderRuntimeDiagnostics,
    capacity_counters: &Counters,
    max_delta: u8,
) {
    let frame_budget = super::FrameBudget::interactive();
    let maximum_extent = WgpuRenderRuntime::maximum_extent();
    let name = sanitize_evidence_text(diagnostics.adapter_name());
    let driver = sanitize_evidence_text(diagnostics.driver());
    let ledger = ledger_json(diagnostics);
    let main_counters_json = counters_json(counters);
    let capacity_ledger = ledger_json(capacity_diagnostics);
    let capacity_counters_json = counters_json(capacity_counters);
    println!(
        concat!(
            "wp09a-evidence-json:{{",
            "\"schema\":\"mirante4d-wp09a-trusted-gpu-evidence\",",
            "\"schema_version\":2,",
            "\"adapter\":{{\"name\":\"{}\",\"backend\":\"{}\",\"driver\":\"{}\",",
            "\"max_buffer_size_bytes\":{},\"max_storage_buffer_binding_size_bytes\":{},",
            "\"max_storage_buffers_per_shader_stage\":{}}},",
            "\"envelope\":{{\"resident_resources_visited\":{},",
            "\"new_resources_uploaded\":{},\"payload_upload_bytes\":{},",
            "\"control_upload_bytes\":{},\"command_buffers\":{},",
            "\"queue_submissions\":{},\"maximum_extent_pixels\":[{},{}]}},",
            "\"ledger\":{},\"counters\":{},",
            "\"capacity_ledger\":{},\"capacity_counters\":{},",
            "\"cases\":{{",
            "\"semantic_modes_and_dtypes\":[\"mip-u8\",\"dvr-u16\",\"iso-f32\",\"cross-section-u8\"],",
            "\"semantic_fixture_resources\":24,",
            "\"semantic_fixture_decoded_bytes_with_validity\":241664,",
            "\"perspective_expected_image_proved\":true,",
            "\"full_affine_expected_image_proved\":true,",
            "\"gradient_iso_attached_expected_image_proved\":true,",
            "\"gradient_iso_detached_expected_image_proved\":true,",
            "\"upload_first_resources\":8,\"upload_first_bytes\":8388608,",
            "\"upload_second_resources\":1,\"upload_second_bytes\":1048576,",
            "\"work_first_visits\":512,\"work_second_visits\":1,",
            "\"all_invalid_zero_byte_resident_proved\":true,",
            "\"cancellation_proved\":true,\"stale_capture_rejected\":true,",
            "\"stale_frame_rejected_without_submit\":true,",
            "\"eviction_reupload_proved\":true,",
            "\"capacity_rejected_without_submit\":true,",
            "\"lease_release_render_proved\":true,",
            "\"qualification_extents\":[[1280,720],[1920,1080]]}},",
            "\"readback\":{{\"captures\":{},\"rgba8_max_delta\":{},\"coverage_exact\":true,",
            "\"validity_exact\":true,\"selected_hand_facts_exact\":true}},",
            "\"validation_errors\":[],\"result\":\"passed\"}}"
        ),
        name,
        diagnostics.backend(),
        driver,
        diagnostics.max_buffer_size_bytes(),
        diagnostics.max_storage_buffer_binding_size_bytes(),
        diagnostics.max_storage_buffers_per_shader_stage(),
        frame_budget.resident_resources_visited(),
        frame_budget.new_resources_uploaded(),
        frame_budget.payload_upload_bytes(),
        frame_budget.control_upload_bytes(),
        frame_budget.command_buffers(),
        frame_budget.queue_submissions(),
        maximum_extent.width_pixels(),
        maximum_extent.height_pixels(),
        ledger,
        main_counters_json,
        capacity_ledger,
        capacity_counters_json,
        counters.captures,
        max_delta,
    );
}

#[test]
fn ep00_numerical_conformance_fixture_binds_independent_geometry_and_validity() {
    let volume = ep00_semantic_f32_volume();
    let plane =
        NumericalCrossSectionPlane::new([0.0, 31.5, 0.0], [0.5, 0.0, 0.0], [0.0, -0.5, 0.0])
            .expect("the independent plane coefficients are valid");
    let transfer = ep00_numerical_transfer([0.0, 0.05], [1.0, 0.0, 0.0], 1.0);
    let oracle = NumericalConformanceOracle::new();
    let invalid = oracle.cross_section_pixel(
        &volume,
        plane,
        [1, 63],
        NumericalSampling::SmoothLinear,
        transfer,
    );
    assert_eq!(invalid.world_position(), [0.5, 0.0, 0.0]);
    assert_eq!(
        invalid.sample().state(),
        mirante4d_render_reference::NumericalSampleState::Invalid
    );
    assert_eq!(invalid.color().rgba8(), [0, 0, 0, 0]);
    assert!(invalid.color().covered());
    assert!(!invalid.color().valid());

    let valid = oracle.cross_section_pixel(
        &volume,
        plane,
        [31, 31],
        NumericalSampling::SmoothLinear,
        transfer,
    );
    assert_eq!(valid.world_position(), [15.5, 16.0, 0.0]);
    assert_eq!(
        valid.sample().state(),
        mirante4d_render_reference::NumericalSampleState::Valid
    );
    assert_eq!(valid.color().rgba8(), [82, 0, 0, 255]);
    assert!(valid.color().covered());
    assert!(valid.color().valid());

    let ray = ep00_primary_perspective_ray();
    let expected_direction = [1.0 / 5.0_f64.sqrt(), 0.0, -2.0 / 5.0_f64.sqrt()];
    assert!(
        NumericalConformanceContract::ep00()
            .world_position_matches(expected_direction, ray.direction())
    );
    assert_eq!(ep00_primary_sample_step_world(), 5.0_f64.sqrt() / 2.0);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn ep00_plane_mip_iso_depth_and_pick_match_independent_numerical_oracle() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 700);
    let f32_lease_map = load_keys(
        &dataset_runtime,
        &fixtures.semantic[2],
        generation,
        deadline,
    );
    let multichannel_lease_map = load_keys(
        &dataset_runtime,
        &fixtures.multichannel,
        generation,
        deadline,
    );
    let f32_leases = borrowed_leases(&fixtures.semantic[2], &f32_lease_map, None);
    let volume = ep00_semantic_f32_volume();
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the numerical-conformance GPU ledger is valid")
            .with_validation_capture(true),
    ))
    .expect("the trusted workstation exposes the qualifying Vulkan adapter");
    let extent = RenderExtent::new(65, 65).expect("the numerical volume extent is valid");
    let presentation = gpu
        .register_presentation(extent)
        .expect("the numerical-conformance presentation registers")
        .token();

    let plane_extent = RenderExtent::new(64, 64).expect("the numerical plane extent is valid");
    let plane_transfer = transfer_color(0.0, 0.05, [1.0, 0.0, 0.0]);
    let (plane_intent, plane_requirements) = multichannel_intent_and_requirements(
        700,
        cross_section_view([15.75, 15.75, 0.0], 0.5),
        plane_extent,
        vec![LayerRenderIntent::new(
            LogicalLayerKey::new(2),
            plane_transfer,
            mirante4d_domain::RenderState::mip(SamplingPolicy::SmoothLinear),
        )],
        &fixtures.semantic[2],
    );
    let (_plane_presented, plane_capture) = execute_presented_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &plane_intent,
        &plane_requirements,
        &f32_leases,
        deadline,
    );
    let numerical_plane =
        NumericalCrossSectionPlane::new([0.0, 31.5, 0.0], [0.5, 0.0, 0.0], [0.0, -0.5, 0.0])
            .expect("the independent plane coefficients are valid");
    let numerical_plane_transfer = ep00_numerical_transfer([0.0, 0.05], [1.0, 0.0, 0.0], 1.0);
    for (label, pixel_xy) in [
        ("SmoothLinear valid footprint", [31, 31]),
        ("SmoothLinear invalid footprint", [1, 63]),
    ] {
        let expected = NumericalConformanceOracle::new().cross_section_pixel(
            &volume,
            numerical_plane,
            pixel_xy,
            NumericalSampling::SmoothLinear,
            numerical_plane_transfer,
        );
        assert_numerical_color(label, &plane_capture, pixel_xy, expected.color());
    }

    let transfer = ep00_numerical_transfer([0.0, 1.0], [1.0, 0.0, 0.0], 1.0);
    let (mip_intent, mip_requirements) = intent_and_requirements(
        701,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        ep00_primary_perspective_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (mip_presented, mip_capture) = execute_presented_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &mip_intent,
        &mip_requirements,
        &f32_leases,
        deadline,
    );
    let mip_expected = ep00_volume_facts(&volume, transfer, NumericalVolumeMode::Mip);
    assert_numerical_color(
        "off-axis perspective MIP",
        &mip_capture,
        [48, 32],
        mip_expected.color(),
    );
    let mip_query = VolumePickQuery::new(
        &mip_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(2),
        [48.0, 32.0],
        VolumePickPolicy::MipArgmax,
    )
    .expect("the off-axis MIP pick query is valid");
    let mip_ticket = gpu
        .request_pick(presentation, mip_query)
        .expect("the off-axis MIP pick is accepted");
    assert_numerical_pick(
        "off-axis perspective MIP",
        poll_pick(&mut gpu, mip_ticket, deadline),
        mip_expected
            .pick()
            .expect("the independent off-axis MIP has a pick"),
    );

    let iso_parameters = NumericalIsoParameters::new(0.55, NumericalIsoShading::Flat).unwrap();
    let iso_state = mirante4d_domain::RenderState::iso(
        SamplingPolicy::VoxelExact,
        IsoShadingPolicy::Flat,
        0.55,
    )
    .expect("the off-axis ISO state is valid");
    let (iso_intent, iso_requirements) = intent_and_requirements(
        702,
        2,
        iso_state,
        ep00_primary_perspective_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (iso_presented, iso_capture) = execute_presented_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &iso_intent,
        &iso_requirements,
        &f32_leases,
        deadline,
    );
    let iso_expected =
        ep00_volume_facts(&volume, transfer, NumericalVolumeMode::Iso(iso_parameters));
    assert_numerical_color(
        "off-axis perspective ISO",
        &iso_capture,
        [48, 32],
        iso_expected.color(),
    );
    let iso_query = VolumePickQuery::new(
        &iso_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(2),
        [48.0, 32.0],
        VolumePickPolicy::FirstThresholdHit,
    )
    .expect("the off-axis ISO pick query is valid");
    let iso_ticket = gpu
        .request_pick(presentation, iso_query)
        .expect("the off-axis ISO pick is accepted");
    assert_numerical_pick(
        "off-axis perspective ISO",
        poll_pick(&mut gpu, iso_ticket, deadline),
        iso_expected
            .pick()
            .expect("the independent off-axis ISO has a pick"),
    );
    assert_eq!(
        iso_expected.hit_depth_world(),
        iso_expected
            .pick()
            .map(NumericalPickFacts::ray_distance_world),
        "ISO rendered depth and first-threshold pick share one physical hit"
    );

    let ordered_extent = RenderExtent::new(33, 33).expect("the ISO-order extent is valid");
    let ordered_view = RenderViewIntent::volume(
        CameraView::new(
            Projection::Perspective,
            WorldPoint3::new(-4.5, 3.25, 4.5).unwrap(),
            UnitQuaternion::identity(),
            0.5,
            16.0,
            16.0,
        )
        .expect("the ISO-order camera is valid"),
        IsoLightState::attached_camera(),
    );
    let ordered_iso_state =
        mirante4d_domain::RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.1)
            .expect("the ISO-order state is valid");
    let far_key = fixtures.multichannel[0];
    let near_key = fixtures.multichannel[2];
    let (ordered_intent, ordered_requirements) = multichannel_intent_and_requirements(
        703,
        ordered_view,
        ordered_extent,
        vec![
            LayerRenderIntent::new(
                LogicalLayerKey::new(5),
                transfer_color_opacity(0.0, 1.0, [1.0, 0.0, 0.0], 0.4),
                ordered_iso_state,
            ),
            LayerRenderIntent::new(
                LogicalLayerKey::new(7),
                transfer_color_opacity(0.0, 1.0, [0.0, 1.0, 0.0], 0.6),
                ordered_iso_state,
            ),
        ],
        &[far_key, near_key],
    );
    let far_lease = multichannel_lease_map
        .get(&far_key)
        .expect("the translated far-layer lease exists");
    let near_lease = multichannel_lease_map
        .get(&near_key)
        .expect("the near-layer lease exists");
    let (ordered_presented, ordered_capture) = execute_presented_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &ordered_intent,
        &ordered_requirements,
        &[far_lease, near_lease],
        deadline,
    );
    let far_volume = NumericalVolume::new(
        [8, 8, 8],
        GridToWorld::identity(),
        vec![NumericalVoxel::Valid(0.25); 8 * 8 * 8],
    )
    .unwrap();
    let near_volume = NumericalVolume::new(
        [8, 8, 8],
        GridToWorld::from_row_major([
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 1.0,
        ])
        .unwrap(),
        vec![NumericalVoxel::Valid(0.75); 8 * 8 * 8],
    )
    .unwrap();
    let ordered_ray = NumericalWorldRay::new([-4.5, 3.25, 20.5], [0.5, 0.0, -1.0]).unwrap();
    let ordered_iso = NumericalIsoParameters::new(0.1, NumericalIsoShading::Flat).unwrap();
    let ordered_query = |transfer, volume: &NumericalVolume| {
        NumericalConformanceOracle::new()
            .volume(
                volume,
                NumericalVolumeQuery::new(
                    ordered_ray,
                    NumericalSampling::VoxelExact,
                    transfer,
                    NumericalVolumeMode::Iso(ordered_iso),
                    ep00_primary_sample_step_world(),
                )
                .unwrap(),
            )
            .unwrap()
    };
    let far_expected = ordered_query(
        ep00_numerical_transfer([0.0, 1.0], [1.0, 0.0, 0.0], 0.4),
        &far_volume,
    );
    let near_expected = ordered_query(
        ep00_numerical_transfer([0.0, 1.0], [0.0, 1.0, 0.0], 0.6),
        &near_volume,
    );
    let ordered_expected = NumericalConformanceOracle::new()
        .composite_iso_depth_ordered(&[far_expected.clone(), near_expected.clone()])
        .unwrap();
    assert_eq!(
        ordered_expected.source_order(),
        &[1, 0],
        "the independent oracle must reverse the deliberately far-first authored order"
    );
    assert_numerical_color(
        "off-axis ISO world-depth order",
        &ordered_capture,
        [24, 16],
        ordered_expected.color(),
    );
    for (label, layer, expected) in [
        ("off-axis far ISO layer", 5_u32, &far_expected),
        ("off-axis near ISO layer", 7_u32, &near_expected),
    ] {
        let query = VolumePickQuery::new(
            &ordered_presented,
            TimeIndex::new(0),
            LogicalLayerKey::new(layer),
            [24.0, 16.0],
            VolumePickPolicy::FirstThresholdHit,
        )
        .expect("the ordered ISO pick query is valid");
        let ticket = gpu
            .request_pick(presentation, query)
            .expect("the ordered ISO pick is accepted");
        assert_numerical_pick(
            label,
            poll_pick(&mut gpu, ticket, deadline),
            expected
                .pick()
                .expect("each independent ISO layer has a threshold pick"),
        );
    }

    let plane_valid_expected = NumericalConformanceOracle::new().cross_section_pixel(
        &volume,
        numerical_plane,
        [31, 31],
        NumericalSampling::SmoothLinear,
        numerical_plane_transfer,
    );
    let plane_invalid_expected = NumericalConformanceOracle::new().cross_section_pixel(
        &volume,
        numerical_plane,
        [1, 63],
        NumericalSampling::SmoothLinear,
        numerical_plane_transfer,
    );
    let mip_pick = mip_expected.pick().unwrap();
    let iso_pick = iso_expected.pick().unwrap();
    let far_pick = far_expected.pick().unwrap();
    let near_pick = near_expected.pick().unwrap();
    let contract = NumericalConformanceContract::ep00();
    println!(
        concat!(
            "mirante4d-ep00-numerical-gpu-conformance-json:{{",
            "\"schema\":\"mirante4d-ep00-numerical-gpu-conformance\",",
            "\"schema_version\":1,",
            "\"scalar_absolute_tolerance\":{},\"scalar_relative_tolerance\":{},",
            "\"premultiplied_rgba_absolute_tolerance\":{},",
            "\"world_position_absolute_tolerance\":{},",
            "\"ray_distance_absolute_tolerance\":{},\"rgba8_channel_tolerance\":{},",
            "\"plane_smooth_valid_pixel\":[31,31],",
            "\"plane_smooth_valid_rgba8\":{:?},",
            "\"plane_smooth_valid_premultiplied_rgba\":{:?},",
            "\"plane_smooth_valid_covered\":{},\"plane_smooth_valid_valid\":{},",
            "\"plane_smooth_invalid_pixel\":[1,63],",
            "\"plane_smooth_invalid_rgba8\":{:?},",
            "\"plane_smooth_invalid_premultiplied_rgba\":{:?},",
            "\"plane_smooth_invalid_covered\":{},\"plane_smooth_invalid_valid\":{},",
            "\"perspective_mip_pixel\":[48,32],\"perspective_mip_rgba8\":{:?},",
            "\"perspective_mip_premultiplied_rgba\":{:?},",
            "\"perspective_mip_covered\":{},\"perspective_mip_valid\":{},",
            "\"perspective_mip_pick_kind\":\"voxel\",\"perspective_mip_pick_complete\":true,",
            "\"perspective_mip_pick_value\":{},\"perspective_mip_pick_world\":{:?},",
            "\"perspective_mip_pick_distance_world\":{},",
            "\"perspective_iso_pixel\":[48,32],\"perspective_iso_rgba8\":{:?},",
            "\"perspective_iso_premultiplied_rgba\":{:?},",
            "\"perspective_iso_covered\":{},\"perspective_iso_valid\":{},",
            "\"perspective_iso_pick_kind\":\"voxel\",\"perspective_iso_pick_complete\":true,",
            "\"perspective_iso_hit_depth_world\":{},\"perspective_iso_pick_value\":{},",
            "\"perspective_iso_pick_world\":{:?},\"perspective_iso_pick_distance_world\":{},",
            "\"perspective_iso_depth_order_pixel\":[24,16],",
            "\"perspective_iso_depth_order_authored_layers\":[5,7],",
            "\"perspective_iso_depth_order_source_order\":{:?},",
            "\"perspective_iso_depth_order_rgba8\":{:?},",
            "\"perspective_iso_depth_order_premultiplied_rgba\":{:?},",
            "\"perspective_iso_depth_order_covered\":{},",
            "\"perspective_iso_depth_order_valid\":{},",
            "\"perspective_iso_depth_order_hit_depth_world\":{},",
            "\"perspective_iso_depth_order_pick_kind\":\"voxel\",",
            "\"perspective_iso_depth_order_pick_complete\":true,",
            "\"perspective_iso_near_pick_kind\":\"voxel\",",
            "\"perspective_iso_near_pick_complete\":true,",
            "\"perspective_iso_far_pick_value\":{},\"perspective_iso_far_pick_world\":{:?},",
            "\"perspective_iso_far_pick_distance_world\":{},",
            "\"perspective_iso_near_pick_value\":{},\"perspective_iso_near_pick_world\":{:?},",
            "\"perspective_iso_near_pick_distance_world\":{},",
            "\"result\":\"passed\"}}"
        ),
        contract.scalar_absolute_tolerance(),
        contract.scalar_relative_tolerance(),
        contract.premultiplied_rgba_absolute_tolerance(),
        contract.world_position_absolute_tolerance(),
        contract.ray_distance_absolute_tolerance(),
        contract.rgba8_channel_tolerance(),
        plane_valid_expected.color().rgba8(),
        plane_valid_expected.color().premultiplied_rgba(),
        plane_valid_expected.color().covered(),
        plane_valid_expected.color().valid(),
        plane_invalid_expected.color().rgba8(),
        plane_invalid_expected.color().premultiplied_rgba(),
        plane_invalid_expected.color().covered(),
        plane_invalid_expected.color().valid(),
        mip_expected.color().rgba8(),
        mip_expected.color().premultiplied_rgba(),
        mip_expected.color().covered(),
        mip_expected.color().valid(),
        mip_pick.value(),
        mip_pick.world_position(),
        mip_pick.ray_distance_world(),
        iso_expected.color().rgba8(),
        iso_expected.color().premultiplied_rgba(),
        iso_expected.color().covered(),
        iso_expected.color().valid(),
        iso_expected.hit_depth_world().unwrap(),
        iso_pick.value(),
        iso_pick.world_position(),
        iso_pick.ray_distance_world(),
        ordered_expected.source_order(),
        ordered_expected.color().rgba8(),
        ordered_expected.color().premultiplied_rgba(),
        ordered_expected.color().covered(),
        ordered_expected.color().valid(),
        near_expected.hit_depth_world().unwrap(),
        far_pick.value(),
        far_pick.world_position(),
        far_pick.ray_distance_world(),
        near_pick.value(),
        near_pick.world_position(),
        near_pick.ray_distance_world(),
    );

    assert_eq!(gpu.diagnostics().validation_error_count(), 0);
    dataset_runtime
        .request_shutdown()
        .expect("the numerical-conformance fixture begins bounded shutdown");
    drop(f32_leases);
    drop(f32_lease_map);
    drop(multichannel_lease_map);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "the independent numerical GPU conformance check exceeded its deadline"
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn ep00_off_axis_perspective_dvr_uses_physical_world_distance() {
    // This is an acceptance gate, not a snapshot of predecessor output. The
    // EP-00 baseline currently fails it because the fragment path attenuates
    // by an unnormalized perspective ray parameter. Keep the independent
    // world-distance expectation fixed until the product kernel conforms.
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 704);
    let lease_map = load_keys(
        &dataset_runtime,
        &fixtures.semantic[2],
        generation,
        deadline,
    );
    let leases = borrowed_leases(&fixtures.semantic[2], &lease_map, None);
    let extent = RenderExtent::new(65, 65).expect("the off-axis DVR extent is valid");
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the off-axis DVR GPU ledger is valid")
            .with_validation_capture(true),
    ))
    .expect("the trusted workstation exposes the qualifying Vulkan adapter");
    let presentation = gpu
        .register_presentation(extent)
        .expect("the off-axis DVR presentation registers")
        .token();
    let dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).expect("the DVR opacity window is valid"),
            TransferCurve::linear(),
        ),
        0.05,
    )
    .expect("the off-axis DVR state is valid");
    let (intent, requirements) = intent_and_requirements(
        704,
        2,
        dvr_state,
        ep00_primary_perspective_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (presented, capture) = execute_presented_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &intent,
        &requirements,
        &leases,
        deadline,
    );
    let expected = ep00_volume_facts(
        &ep00_semantic_f32_volume(),
        ep00_numerical_transfer([0.0, 1.0], [1.0, 0.0, 0.0], 1.0),
        NumericalVolumeMode::Dvr(
            NumericalDvrParameters::new([0.0, 1.0], 1.0, 0.05)
                .expect("the independent DVR parameters are valid"),
        ),
    );
    let query = VolumePickQuery::new(
        &presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(2),
        [48.0, 32.0],
        VolumePickPolicy::MaximumOpacityContribution,
    )
    .expect("the off-axis DVR pick query is valid");
    let ticket = gpu
        .request_pick(presentation, query)
        .expect("the off-axis DVR pick is accepted");
    let observed_pick = poll_pick(&mut gpu, ticket, deadline);
    assert_numerical_pick(
        "off-axis perspective DVR",
        observed_pick,
        expected
            .pick()
            .expect("the independent off-axis DVR has a pick"),
    );

    let (observed_rgba8, observed_coverage, observed_validity) = pixel(&capture, 48, 32);
    let contract = NumericalConformanceContract::ep00();
    let rgba_matches = contract.rgba8_matches(expected.color().rgba8(), observed_rgba8);
    let coverage_matches = observed_coverage == u8::from(expected.color().covered());
    let validity_matches = observed_validity == u8::from(expected.color().valid());
    let expected_pick = expected.pick().unwrap();
    let observed_pick_value = match observed_pick.value() {
        Some(VolumePickValue::IntensityF32(value)) => value,
        _ => f32::NAN,
    };
    let observed_pick_world = observed_pick
        .world_position()
        .map_or([f64::NAN; 3], WorldPoint3::components);
    let observed_pick_distance = observed_pick.ray_distance_world().unwrap_or(f64::NAN);
    println!(
        concat!(
            "mirante4d-ep00-numerical-gpu-conformance-gap-json:{{",
            "\"schema\":\"mirante4d-ep00-numerical-gpu-conformance-gap\",",
            "\"schema_version\":1,\"case\":\"perspective_dvr_world_distance\",",
            "\"pixel\":[48,32],\"sample_step_world\":{},",
            "\"expected_rgba8\":{:?},\"observed_rgba8\":{:?},",
            "\"expected_premultiplied_rgba\":{:?},",
            "\"expected_covered\":{},\"expected_valid\":{},",
            "\"coverage_matches\":{},\"validity_matches\":{},",
            "\"expected_pick_kind\":\"voxel\",\"expected_pick_complete\":true,",
            "\"expected_pick_value\":{},\"expected_pick_world\":{:?},",
            "\"expected_pick_distance_world\":{},",
            "\"observed_pick_value\":{},\"observed_pick_world\":{:?},",
            "\"observed_pick_distance_world\":{},",
            "\"rgba8_channel_tolerance\":{},\"result\":\"{}\"}}"
        ),
        ep00_primary_sample_step_world(),
        expected.color().rgba8(),
        observed_rgba8,
        expected.color().premultiplied_rgba(),
        expected.color().covered(),
        expected.color().valid(),
        coverage_matches,
        validity_matches,
        expected_pick.value(),
        expected_pick.world_position(),
        expected_pick.ray_distance_world(),
        observed_pick_value,
        observed_pick_world,
        observed_pick_distance,
        contract.rgba8_channel_tolerance(),
        if rgba_matches && coverage_matches && validity_matches {
            "passed"
        } else {
            "failed"
        },
    );

    dataset_runtime
        .request_shutdown()
        .expect("the off-axis DVR fixture begins bounded shutdown");
    drop(leases);
    drop(lease_map);
    drop(dataset_runtime);
    assert!(coverage_matches, "off-axis DVR coverage differs");
    assert!(validity_matches, "off-axis DVR validity differs");
    assert!(
        rgba_matches,
        concat!(
            "off-axis perspective DVR extinction is not expressed in physical world distance: ",
            "observed={:?}, expected={:?}, world_step={}, RGBA8 tolerance={}"
        ),
        observed_rgba8,
        expected.color().rgba8(),
        ep00_primary_sample_step_world(),
        contract.rgba8_channel_tolerance(),
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn qualification() {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    assert!(Arc::ptr_eq(&catalog, &fixtures.source.catalog));

    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let semantic_keys = fixtures
        .semantic
        .iter()
        .flatten()
        .copied()
        .filter(|key| *key != fixtures.missing_u8)
        .collect::<Vec<_>>();
    let leases = load_keys(&dataset_runtime, &semantic_keys, generation, deadline);
    let current_generation = prove_cancellation(&fixtures, &dataset_runtime, deadline);
    let empty_leases = load_keys(
        &dataset_runtime,
        &[fixtures.empty],
        current_generation,
        deadline,
    );
    let sparse_leases = load_keys(
        &dataset_runtime,
        &[fixtures.sparse_off_ray],
        current_generation,
        deadline,
    );
    let affine_leases = load_keys(
        &dataset_runtime,
        &[fixtures.affine],
        current_generation,
        deadline,
    );

    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(QUALIFICATION_GPU_BYTES)
            .expect("qualification ledger is valid")
            .with_validation_capture(true),
    ))
    .expect("trusted workstation exposes the qualifying Vulkan adapter");
    assert_eq!(gpu.diagnostics().backend(), "Vulkan");
    let mut counters = Counters::default();
    let extent = RenderExtent::new(96, 96).expect("semantic extent is valid");
    let presentation = gpu
        .register_presentation(extent)
        .expect("qualification presentation registers")
        .token();
    let mut rgba8_max_delta = 0_u8;

    let (mip, mip_requirements) = intent_and_requirements(
        1,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view(),
        extent,
        &fixtures.semantic[0],
    );
    let u8_leases = borrowed_leases(&fixtures.semantic[0], &leases, Some(fixtures.missing_u8));
    let (_, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &mip,
            requirements: &mip_requirements,
            leases: &u8_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);

    let dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 65_535.0).expect("DVR opacity window is valid"),
            TransferCurve::linear(),
        ),
        0.05,
    )
    .expect("fixture DVR state is valid");
    let (dvr, dvr_requirements) = intent_and_requirements(
        2,
        1,
        dvr_state,
        volume_view(),
        extent,
        &fixtures.semantic[1],
    );
    let u16_leases = borrowed_leases(&fixtures.semantic[1], &leases, None);
    let (_, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &dvr,
            requirements: &dvr_requirements,
            leases: &u16_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);

    let iso_state =
        mirante4d_domain::RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.5)
            .expect("fixture flat ISO state is valid");
    let (iso, iso_requirements) = intent_and_requirements(
        3,
        2,
        iso_state,
        volume_view(),
        extent,
        &fixtures.semantic[2],
    );
    let f32_leases = borrowed_leases(&fixtures.semantic[2], &leases, None);
    let (iso_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &iso,
            requirements: &iso_requirements,
            leases: &f32_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);

    let (section, section_requirements) = intent_and_requirements(
        4,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 0.0], 0.5),
        extent,
        &fixtures.semantic[0],
    );
    let (section_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &section,
            requirements: &section_requirements,
            leases: &u8_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(pixel(&section_capture, 16, 79), ([0, 0, 0, 255], 1, 1));
    assert_eq!(pixel(&section_capture, 18, 79), ([0, 0, 0, 0], 1, 0));
    assert_eq!(pixel(&section_capture, 46, 79), ([255, 0, 0, 255], 1, 1));
    assert_eq!(pixel(&section_capture, 57, 79), ([0, 0, 0, 0], 0, 0));
    assert_eq!(pixel(&section_capture, 0, 0), ([0, 0, 0, 0], 1, 0));

    let (empty_intent, empty_requirements) = intent_and_requirements(
        5,
        9,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([31.5, 31.5, 31.5], 1.0),
        extent,
        &[fixtures.empty],
    );
    let empty_lease = empty_leases
        .get(&fixtures.empty)
        .expect("all-invalid fixture lease exists");
    let resident_bytes_before_empty = gpu.diagnostics().resident_payload_bytes();
    let empty_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &empty_intent,
            &empty_requirements,
            &[empty_lease],
        )
        .expect("all-invalid page renders from metadata only");
    assert_eq!(empty_report.uploaded_resources(), 0);
    assert_eq!(empty_report.payload_upload_bytes(), 0);
    assert_eq!(empty_report.newly_resident_keys(), &[fixtures.empty]);
    assert!(empty_report.evicted_keys().is_empty());
    assert_eq!(
        empty_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    counters.record(&empty_report);
    let empty_capture = poll_capture(
        &mut gpu,
        empty_report
            .validation_capture()
            .expect("all-invalid validation capture exists"),
        deadline,
    );
    let empty_reference = ReferenceRenderer::new()
        .render(&catalog, &empty_intent, &[empty_lease])
        .expect("reference renderer accepts all-invalid fixture");
    rgba8_max_delta = rgba8_max_delta.max(compare_reference(&empty_capture, &empty_reference));
    assert_eq!(
        gpu.diagnostics().resident_payload_bytes(),
        resident_bytes_before_empty,
        "all-invalid page must not consume payload-arena bytes"
    );

    let (smooth_mip, smooth_mip_requirements) = intent_and_requirements(
        6,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::SmoothLinear),
        volume_view(),
        extent,
        &fixtures.semantic[0],
    );
    let (_, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &smooth_mip,
            requirements: &smooth_mip_requirements,
            leases: &u8_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);

    let gradient_iso_state = mirante4d_domain::RenderState::iso(
        SamplingPolicy::SmoothLinear,
        IsoShadingPolicy::GradientLighting,
        0.5,
    )
    .expect("fixture gradient ISO state is valid");
    let missing_gradient_key = fixtures.missing_u8;
    let mut gradient_keys = fixtures.semantic[0]
        .iter()
        .copied()
        .filter(|key| *key != missing_gradient_key)
        .collect::<Vec<_>>();
    gradient_keys.push(missing_gradient_key);
    assert!(!gpu.resource_is_resident(missing_gradient_key));
    let missing_gradient_iso_state = mirante4d_domain::RenderState::iso(
        SamplingPolicy::VoxelExact,
        IsoShadingPolicy::GradientLighting,
        136.0 / 255.0,
    )
    .expect("missing-halo gradient ISO state is valid");
    let inverted_transfer = LayerTransfer::new(
        DisplayWindow::new(0.0, 255.0).unwrap(),
        RgbColor::new([1.0, 0.0, 0.0]).unwrap(),
        Opacity::new(1.0).unwrap(),
        TransferCurve::linear(),
        true,
    );
    let (missing_gradient_intent, missing_gradient_requirements) =
        multichannel_intent_and_requirements(
            7,
            volume_view_for_light(
                [15.0, 15.0, 15.0],
                32.0 / 96.0,
                40.0,
                IsoLightState::detached_screen(0.25, -0.5).unwrap(),
            ),
            extent,
            vec![LayerRenderIntent::new(
                LogicalLayerKey::new(0),
                inverted_transfer,
                missing_gradient_iso_state,
            )],
            &gradient_keys,
        );
    let missing_gradient_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &missing_gradient_intent,
            &missing_gradient_requirements,
            &u8_leases,
        )
        .expect("gradient-halo fixture executes");
    assert_eq!(
        missing_gradient_report
            .progress()
            .map(FrameProgress::completeness),
        Some(FrameCompleteness::Progressive)
    );
    let missing_gradient_capture = poll_capture(
        &mut gpu,
        missing_gradient_report.validation_capture().unwrap(),
        deadline,
    );
    let (_, halo_coverage, halo_validity) = pixel(&missing_gradient_capture, 48, 48);
    assert_eq!(
        (halo_coverage, halo_validity),
        (0, 1),
        "missing gradient support is incomplete even when the ISO center sample is valid"
    );

    let (gradient_iso, gradient_iso_requirements) = intent_and_requirements(
        8,
        2,
        gradient_iso_state,
        volume_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (gradient_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &gradient_iso,
            requirements: &gradient_iso_requirements,
            leases: &f32_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(gradient_capture.coverage(), iso_capture.coverage());
    assert_eq!(gradient_capture.validity(), iso_capture.validity());

    let sparse_dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.5, 1.0).unwrap(),
            TransferCurve::linear(),
        ),
        1.0,
    )
    .unwrap();
    let (sparse_dvr, sparse_dvr_requirements) = intent_and_requirements(
        9,
        10,
        sparse_dvr_state,
        volume_view_for([3.5, 3.5, 3.5], 8.0 / 96.0, 16.0),
        extent,
        &[fixtures.sparse_off_ray],
    );
    let sparse_lease = sparse_leases.get(&fixtures.sparse_off_ray).unwrap();
    let sparse_capture = execute_and_capture(
        &mut gpu,
        presentation,
        &catalog,
        &sparse_dvr,
        &sparse_dvr_requirements,
        &[sparse_lease],
        deadline,
    );
    assert_eq!(
        pixel(&sparse_capture, 48, 48),
        ([0, 0, 0, 0], 1, 0),
        "a valid voxel elsewhere in a sparse brick cannot make an invalid ray valid"
    );

    let upload_leases = load_keys(
        &dataset_runtime,
        &fixtures.upload,
        current_generation,
        deadline,
    );
    let upload_extent = RenderExtent::new(1, 1).expect("boundary extent is valid");
    let (upload_intent, upload_requirements) = intent_and_requirements(
        10,
        3,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([96.0, 96.0, 32.0], 1.0),
        upload_extent,
        &fixtures.upload,
    );
    let prepared_upload_requirements = PreparedRenderRequirements::new(
        upload_intent.resource_identity(),
        upload_intent.timepoint(),
        vec![LogicalLayerKey::new(3)],
        upload_requirements.prepared_body().clone(),
        1,
    )
    .expect("upload requirements expose one prepared body");
    let prepared_upload_layout =
        prepare_static_presentation_layout(&catalog, &prepared_upload_requirements)
            .expect("upload requirements prepare one static layout");
    let upload_updates = fixtures
        .upload
        .iter()
        .map(|key| {
            Arc::new(upload_leases.get(key).expect("upload lease exists").clone())
                as Arc<dyn ResourceLease>
        })
        .collect::<Vec<_>>();
    let frames_before_atomic_upload = gpu.diagnostics().frames_executed();
    let rebuilds_before_atomic_upload = gpu.diagnostics().control_static_rebuilds();
    let submissions_before_atomic_upload = gpu.diagnostics().queue_submissions();
    let first_upload = gpu
        .execute_prepared_retained_frame_with_policy(
            presentation,
            &catalog,
            &upload_intent,
            &upload_requirements,
            &prepared_upload_layout,
            &upload_updates[..8],
            0,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .expect("first upload-boundary frame executes");
    assert_eq!(first_upload.uploaded_resources(), 8);
    assert_eq!(first_upload.payload_upload_bytes(), 8 * MIB);
    assert_eq!(first_upload.newly_resident_keys().len(), 8);
    assert_eq!(
        first_upload
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Progressive)
    );
    assert_eq!(
        first_upload
            .progress()
            .and_then(|progress| progress.limitation()),
        Some(FrameLimitation::BudgetLimited)
    );
    assert!(first_upload.presentation().is_none());
    assert!(first_upload.validation_capture().is_none());
    assert_eq!(first_upload.control_upload_bytes(), 0);
    assert_eq!(first_upload.queue_submissions(), 1);
    assert_eq!(
        gpu.diagnostics().frames_executed(),
        frames_before_atomic_upload,
        "an incomplete hidden cohort must not execute a volume pass"
    );
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        rebuilds_before_atomic_upload,
        "an incomplete hidden cohort must not publish render controls"
    );
    counters.record(&first_upload);
    let second_upload = gpu
        .execute_prepared_retained_frame_with_policy(
            presentation,
            &catalog,
            &upload_intent,
            &upload_requirements,
            &prepared_upload_layout,
            &upload_updates[8..],
            0,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .expect("second upload-boundary frame executes");
    assert_eq!(second_upload.uploaded_resources(), 1);
    assert_eq!(second_upload.payload_upload_bytes(), MIB);
    assert_eq!(
        second_upload
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    assert_eq!(
        second_upload
            .progress()
            .and_then(mirante4d_render_api::FrameProgress::limitation),
        None
    );
    assert!(second_upload.presentation().is_some());
    assert_eq!(second_upload.queue_submissions(), 1);
    assert_eq!(
        gpu.diagnostics().queue_submissions(),
        submissions_before_atomic_upload + 2,
        "two bounded upload cohorts require two submissions"
    );
    assert_eq!(
        gpu.diagnostics().frames_executed(),
        frames_before_atomic_upload + 1,
        "the hidden target raymarches only the exact final cohort"
    );
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        rebuilds_before_atomic_upload + 1,
        "the hidden target publishes controls only with its final volume pass"
    );
    counters.record(&second_upload);
    let _ = poll_capture(
        &mut gpu,
        second_upload
            .validation_capture()
            .expect("boundary capture exists"),
        deadline,
    );

    // A navigation guard is resident metadata, not current-view coverage.
    // Its cohort must prime only the touched stable control slot; promotion
    // then shares the body/layout and publishes one dynamic prefix + pass.
    let guard_keys = &fixtures.work[1_000..1_003];
    let guard_leases = load_keys(&dataset_runtime, guard_keys, current_generation, deadline);
    let (guard_intent, guard_all_required) = intent_and_requirements(
        11,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([64_000.0, 0.0, 0.0], 1.0),
        upload_extent,
        guard_keys,
    );
    let guard_prepared = PreparedRenderRequirements::new_with_required_prefix(
        guard_intent.resource_identity(),
        guard_intent.timepoint(),
        vec![LogicalLayerKey::new(4)],
        guard_all_required.prepared_body().clone(),
        1,
        2,
    )
    .expect("guard requirements expose one optional ranked suffix");
    let guard_requirements = guard_prepared
        .bind(&guard_intent)
        .expect("guard requirements bind to the current camera");
    let guard_layout = prepare_static_presentation_layout(&catalog, &guard_prepared)
        .expect("guard requirements prepare one shared sparse layout");
    let guard_updates = guard_keys
        .iter()
        .map(|key| {
            Arc::new(guard_leases.get(key).expect("guard lease exists").clone())
                as Arc<dyn ResourceLease>
        })
        .collect::<Vec<_>>();
    let initial_guard = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &guard_intent,
            &guard_requirements,
            &guard_layout,
            &guard_updates[..2],
        )
        .expect("the exact-visible guard prefix executes");
    assert_eq!(
        initial_guard
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let _ = poll_capture(
        &mut gpu,
        initial_guard
            .validation_capture()
            .expect("exact-visible guard-prefix capture exists"),
        deadline,
    );
    let frames_before_guard_prefetch = gpu.diagnostics().frames_executed();
    let rebuilds_before_guard_prefetch = gpu.diagnostics().control_static_rebuilds();
    let dense_before_guard_prefetch = gpu.diagnostics().control_dense_fallbacks();
    let guard_prefetch = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &guard_intent,
            &guard_requirements,
            &guard_layout,
            &guard_updates[2..],
        )
        .expect("the optional guard cohort uploads without a display pass");
    assert!(guard_prefetch.presentation().is_none());
    assert!(guard_prefetch.validation_capture().is_none());
    assert_eq!(guard_prefetch.uploaded_resources(), 1);
    assert_eq!(guard_prefetch.queue_submissions(), 1);
    assert!(guard_prefetch.control_upload_bytes() <= 64);
    assert_eq!(
        gpu.diagnostics().frames_executed(),
        frames_before_guard_prefetch,
        "guard-only residency must not execute a volume pass"
    );
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        rebuilds_before_guard_prefetch,
        "guard-only residency reuses the installed sparse layout"
    );

    let promoted_intent = guard_intent.clone().with_frame(FrameIdentity::new(12));
    let promoted_requirements = guard_prepared
        .promote_prefetch()
        .bind(&promoted_intent)
        .expect("contained navigation promotes the same requirement body");
    assert!(guard_requirements.shares_resources_with(&promoted_requirements));
    let promoted_report = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &promoted_intent,
            &promoted_requirements,
            &guard_layout,
            &[],
        )
        .expect("contained navigation renders from the primed guard slots");
    assert_eq!(
        promoted_report
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    assert!(promoted_report.control_upload_bytes() <= ((32 + 64) * 4) as u64);
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        rebuilds_before_guard_prefetch
    );
    assert_eq!(
        gpu.diagnostics().control_dense_fallbacks(),
        dense_before_guard_prefetch
    );
    counters.record(&initial_guard);
    counters.record(&guard_prefetch);
    counters.record(&promoted_report);
    let _ = poll_capture(
        &mut gpu,
        promoted_report
            .validation_capture()
            .expect("promoted guard capture exists"),
        deadline,
    );

    let loaded_work_keys = &fixtures.work[..513];
    let mut work_leases = BTreeMap::new();
    for chunk in loaded_work_keys.chunks(128) {
        work_leases.extend(load_keys(
            &dataset_runtime,
            chunk,
            current_generation,
            deadline,
        ));
    }
    let (work_intent, work_requirements) = intent_and_requirements(
        20,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([16_383.5, 0.0, 0.0], 1.0),
        upload_extent,
        &fixtures.work,
    );
    let prepared_work_requirements = PreparedRenderRequirements::new(
        work_intent.resource_identity(),
        work_intent.timepoint(),
        vec![LogicalLayerKey::new(4)],
        work_requirements.prepared_body().clone(),
        1,
    )
    .expect("high-count requirements expose one worker-preparable body");
    let static_preflight =
        preflight_static_presentation_layout(&catalog, &prepared_work_requirements)
            .expect("high-count static layout has an exact pre-allocation ledger");
    let prepared_static_layout =
        prepare_static_presentation_layout(&catalog, &prepared_work_requirements)
            .expect("high-count static layout prepares off the execution path");
    assert_eq!(
        prepared_static_layout.renderer_host_allocation_bytes(),
        static_preflight.renderer_host_allocation_bytes()
    );
    assert_eq!(
        prepared_static_layout.preparation_peak_host_allocation_bytes(),
        static_preflight.construction_peak_host_allocation_bytes()
    );
    assert_eq!(
        prepared_static_layout.logical_control_bytes(),
        static_preflight.logical_control_bytes()
    );
    assert_eq!(
        prepared_static_layout.shared_requirement_host_allocation_bytes(),
        work_requirements.prepared_body().host_allocation_bytes()
    );
    let work_updates = loaded_work_keys
        .iter()
        .map(|key| {
            Arc::new(work_leases.get(key).expect("work lease exists").clone())
                as Arc<dyn ResourceLease>
        })
        .collect::<Vec<_>>();
    let static_rebuilds_before_work = gpu.diagnostics().control_static_rebuilds();
    let page_layouts_before_work = gpu.diagnostics().page_layout_constructions();
    let cold_checks_before_work = gpu.diagnostics().cold_coverage_membership_checks();
    let first_work = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &work_intent,
            &work_requirements,
            &prepared_static_layout,
            &work_updates[..512],
        )
        .expect("first work-boundary frame executes");
    assert!(first_work.retained_updates_accepted());
    assert_eq!(first_work.visited_resources(), 512);
    assert_eq!(first_work.newly_resident_keys().len(), 512);
    let cold_checks_after_first_work = gpu.diagnostics().cold_coverage_membership_checks();
    assert!(
        cold_checks_after_first_work - cold_checks_before_work < loaded_work_keys.len() as u64,
        "cold coverage seeds from the smaller resident side instead of walking the 32,768-key body"
    );
    counters.record(&first_work);
    let _ = poll_capture(
        &mut gpu,
        first_work
            .validation_capture()
            .expect("work capture exists"),
        deadline,
    );
    let static_rebuilds_after_first_work = gpu.diagnostics().control_static_rebuilds();
    assert_eq!(
        static_rebuilds_after_first_work,
        static_rebuilds_before_work + 1
    );
    assert_eq!(
        gpu.diagnostics().page_layout_constructions(),
        page_layouts_before_work + 1,
        "one new one-layer requirement body constructs exactly one sparse page layout"
    );
    let second_work = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &work_intent,
            &work_requirements,
            &prepared_static_layout,
            &work_updates[512..],
        )
        .expect("second work-boundary frame executes");
    assert!(second_work.retained_updates_accepted());
    assert_eq!(second_work.visited_resources(), 1);
    assert_eq!(second_work.newly_resident_keys().len(), 1);
    assert!(
        second_work.control_upload_bytes() <= ((32 + 64) * 4 + 16 * 4) as u64,
        "one progressive addition patches one resource record plus the dynamic prefix"
    );
    counters.record(&second_work);
    let _ = poll_capture(
        &mut gpu,
        second_work
            .validation_capture()
            .expect("work capture exists"),
        deadline,
    );
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        static_rebuilds_after_first_work,
        "stable demand hash/control layout must not rebuild as residency grows"
    );
    assert_eq!(
        gpu.diagnostics().page_layout_constructions(),
        page_layouts_before_work + 1,
        "progressive additions patch the stable layout without reconstructing it"
    );

    let (navigation_intent, _) = intent_and_requirements(
        21,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([16_382.5, 0.0, 0.0], 1.0),
        upload_extent,
        &fixtures.work,
    );
    let navigation_requirements = work_requirements
        .rebind(&navigation_intent)
        .expect("high-count semantic body rebinds without cloning keys");
    let allocator_plans_before_navigation = gpu.diagnostics().allocator_plans();
    let staging_allocations_before_navigation = gpu.diagnostics().explicit_staging_allocations();
    let page_layouts_before_navigation = gpu.diagnostics().page_layout_constructions();
    let cold_checks_before_navigation = gpu.diagnostics().cold_coverage_membership_checks();
    let navigation = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &navigation_intent,
            &navigation_requirements,
            &prepared_static_layout,
            &[],
        )
        .expect("high-count retained camera frame executes");
    assert_eq!(navigation.visited_resources(), 0);
    assert_eq!(navigation.uploaded_resources(), 0);
    assert_eq!(navigation.payload_upload_bytes(), 0);
    assert_eq!(
        gpu.diagnostics().allocator_plans(),
        allocator_plans_before_navigation
    );
    assert_eq!(
        gpu.diagnostics().explicit_staging_allocations(),
        staging_allocations_before_navigation
    );
    assert_eq!(
        gpu.diagnostics().page_layout_constructions(),
        page_layouts_before_navigation
    );
    assert_eq!(
        gpu.diagnostics().cold_coverage_membership_checks(),
        cold_checks_before_navigation,
        "retained camera navigation must not reseed coverage from either large set"
    );
    counters.record(&navigation);
    let navigation_ticket = navigation
        .validation_capture()
        .expect("navigation capture exists");
    let (superseding_navigation_intent, _) = intent_and_requirements(
        22,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([16_381.5, 0.0, 0.0], 1.0),
        upload_extent,
        &fixtures.work,
    );
    let superseding_navigation_requirements = work_requirements
        .rebind(&superseding_navigation_intent)
        .expect("superseding semantic body rebinds without cloning keys");
    let allocator_plans_before_superseding_navigation = gpu.diagnostics().allocator_plans();
    let staging_allocations_before_superseding_navigation =
        gpu.diagnostics().explicit_staging_allocations();
    let page_layouts_before_superseding_navigation = gpu.diagnostics().page_layout_constructions();
    let superseding_navigation = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &superseding_navigation_intent,
            &superseding_navigation_requirements,
            &prepared_static_layout,
            &[],
        )
        .expect("a newer retained camera frame supersedes an unread validation capture");
    assert_eq!(superseding_navigation.visited_resources(), 0);
    assert_eq!(superseding_navigation.uploaded_resources(), 0);
    assert_eq!(superseding_navigation.payload_upload_bytes(), 0);
    assert_eq!(
        gpu.diagnostics().allocator_plans(),
        allocator_plans_before_superseding_navigation
    );
    assert_eq!(
        gpu.diagnostics().explicit_staging_allocations(),
        staging_allocations_before_superseding_navigation
    );
    assert_eq!(
        gpu.diagnostics().page_layout_constructions(),
        page_layouts_before_superseding_navigation
    );
    counters.record(&superseding_navigation);
    assert_eq!(
        gpu.poll_validation_capture(navigation_ticket),
        Err(WgpuRenderRuntimeError::StaleValidationCapture)
    );
    let _ = poll_capture(
        &mut gpu,
        superseding_navigation
            .validation_capture()
            .expect("superseding navigation capture exists"),
        deadline,
    );

    // Replace coordinate zero with a key whose first hash slot collides with
    // it. The existing 32k control body, base page allocation, and GPU buffer
    // must remain in place; only one record and the two affected hash entries
    // are eligible for publication.
    let removed_work_key = fixtures.work[0];
    let removed_slot = prepared_static_layout
        .initial_page_hash_slot(removed_work_key)
        .expect("coordinate zero has a page hash slot");
    let added_work_key = (HIGH_COUNT_WORK_RESOURCES as u64..1_000_000)
        .map(|x| resource_key(4, [0, 0, x], [1, 1, 1]))
        .find(|key| prepared_static_layout.initial_page_hash_slot(*key) == Some(removed_slot))
        .expect("the bounded layer contains a coordinate-zero hash collision");
    let mut delta_work_keys = fixtures.work[1..].to_vec();
    delta_work_keys.push(added_work_key);
    delta_work_keys.sort_unstable();
    let (delta_work_intent, delta_work_requirements) = intent_and_requirements(
        23,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([0.5, 0.0, 0.0], 1.0),
        upload_extent,
        &delta_work_keys,
    );
    let prepared_delta_requirements = PreparedRenderRequirements::new(
        delta_work_intent.resource_identity(),
        delta_work_intent.timepoint(),
        vec![LogicalLayerKey::new(4)],
        delta_work_requirements.prepared_body().clone(),
        1,
    )
    .expect("the one-key replacement has one prepared body");
    let delta_preflight = preflight_static_presentation_layout_update(
        &catalog,
        &prepared_delta_requirements,
        Some(&prepared_static_layout),
        &[added_work_key],
        &[removed_work_key],
    )
    .expect("the one-key replacement preflights against stable slots");
    let prepared_delta_layout = prepare_static_presentation_layout_update(
        &catalog,
        &prepared_delta_requirements,
        Some(&prepared_static_layout),
        &[added_work_key],
        &[removed_work_key],
    )
    .expect("the one-key replacement prepares against stable slots");
    assert!(prepared_delta_layout.is_incremental_body_update());
    assert!(prepared_delta_layout.shares_base_page_storage_with(&prepared_static_layout));
    assert_eq!(
        prepared_delta_layout.incremental_preparation_key_visits(),
        2
    );
    assert_eq!(prepared_delta_layout.incremental_resource_slot_changes(), 1);
    assert_eq!(
        delta_preflight.renderer_host_allocation_bytes(),
        prepared_delta_layout.renderer_host_allocation_bytes()
    );
    let rebuilds_before_delta = gpu.diagnostics().control_static_rebuilds();
    let page_layouts_before_delta = gpu.diagnostics().page_layout_constructions();
    let control_allocations_before_delta = gpu.diagnostics().control_buffer_allocations();
    let control_writes_before_delta = gpu.diagnostics().control_publication_writes();
    let delta_updates_before = gpu.diagnostics().control_body_delta_updates();
    let delta_keys_before = gpu.diagnostics().control_body_delta_keys();
    let pin_keys_before = gpu.diagnostics().body_delta_pin_lru_keys();
    let validation_errors_before_delta = gpu.diagnostics().validation_error_count();
    let delta_report = gpu
        .execute_prepared_retained_frame(
            presentation,
            &catalog,
            &delta_work_intent,
            &delta_work_requirements,
            &prepared_delta_layout,
            &[],
        )
        .expect("the predecessor-bound one-key delta executes");
    assert!(delta_report.presentation().is_some());
    assert!(delta_report.control_upload_bytes() < 1_024);
    assert_eq!(
        gpu.diagnostics().control_static_rebuilds(),
        rebuilds_before_delta
    );
    assert_eq!(
        gpu.diagnostics().page_layout_constructions(),
        page_layouts_before_delta
    );
    assert_eq!(
        gpu.diagnostics().control_buffer_allocations(),
        control_allocations_before_delta
    );
    assert_eq!(
        gpu.diagnostics().control_body_delta_updates(),
        delta_updates_before + 1
    );
    assert_eq!(
        gpu.diagnostics().control_body_delta_keys(),
        delta_keys_before + 2
    );
    assert_eq!(
        gpu.diagnostics().body_delta_pin_lru_keys(),
        pin_keys_before + 2
    );
    assert!(gpu.diagnostics().control_publication_writes() - control_writes_before_delta <= 4);
    counters.record(&delta_report);
    let _ = poll_capture(
        &mut gpu,
        delta_report
            .validation_capture()
            .expect("one-key delta capture exists"),
        deadline,
    );
    assert_eq!(
        gpu.diagnostics().validation_error_count(),
        validation_errors_before_delta,
        "coordinate-zero tombstones and colliding insertion must not address an invalid record"
    );

    let (stale_intent, stale_requirements) = intent_and_requirements(
        30,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 0.0], 0.5),
        extent,
        &fixtures.semantic[0],
    );
    let stale_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &stale_intent,
            &stale_requirements,
            &u8_leases,
        )
        .expect("candidate stale frame executes");
    counters.record(&stale_report);
    let stale_ticket = stale_report
        .validation_capture()
        .expect("stale ticket exists");
    let (current_intent, current_requirements) = intent_and_requirements(
        31,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 0.0], 0.5),
        extent,
        &fixtures.semantic[0],
    );
    let current_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &current_intent,
            &current_requirements,
            &u8_leases,
        )
        .expect("newer current frame executes");
    counters.record(&current_report);
    assert_eq!(
        gpu.poll_validation_capture(stale_ticket),
        Err(WgpuRenderRuntimeError::StaleValidationCapture)
    );
    let current_capture = poll_capture(
        &mut gpu,
        current_report
            .validation_capture()
            .expect("current ticket exists"),
        deadline,
    );
    assert_eq!(current_capture.frame(), FrameIdentity::new(31));
    let unchanged = gpu
        .execute_frame(
            presentation,
            &catalog,
            &current_intent,
            &current_requirements,
            &u8_leases,
        )
        .expect("settled unchanged frame is a zero-submit operation");
    assert_eq!(unchanged.queue_submissions(), 0);
    assert_eq!(unchanged.command_buffers(), 0);
    assert_eq!(unchanged.control_upload_bytes(), 0);
    assert!(unchanged.presentation().is_none());
    assert!(unchanged.validation_capture().is_none());
    assert!(!unchanged.deferred_by_backpressure());
    let submissions_before_stale = gpu.diagnostics().queue_submissions();
    assert!(matches!(
        gpu.execute_frame(
            presentation,
            &catalog,
            &stale_intent,
            &stale_requirements,
            &u8_leases,
        ),
        Err(WgpuRenderRuntimeError::StaleFrame { .. })
    ));
    assert_eq!(
        gpu.diagnostics().queue_submissions(),
        submissions_before_stale
    );

    for (frame, width, height) in [(40_u64, 1280_u32, 720_u32), (41, 1920, 1080)] {
        let qualified_extent =
            RenderExtent::new(width, height).expect("qualification extent is valid");
        let (qualified_intent, qualified_requirements) = intent_and_requirements(
            frame,
            0,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([15.5, 15.5, 0.0], 32.0 / f64::from(height)),
            qualified_extent,
            &fixtures.semantic[0],
        );
        let report = gpu
            .execute_frame(
                presentation,
                &catalog,
                &qualified_intent,
                &qualified_requirements,
                &u8_leases,
            )
            .expect("accepted positive qualification extent renders");
        counters.record(&report);
        let capture = poll_capture(
            &mut gpu,
            report
                .validation_capture()
                .expect("qualification extent capture exists"),
            deadline,
        );
        assert_eq!(capture.extent(), qualified_extent);
        assert_eq!(capture.rgba8().len(), width as usize * height as usize * 4);
        assert_eq!(capture.coverage().len(), width as usize * height as usize);
        assert_eq!(capture.validity().len(), width as usize * height as usize);
    }

    let perspective_extent = RenderExtent::new(95, 95).expect("perspective extent is valid");
    let (perspective_intent, perspective_requirements) = intent_and_requirements(
        42,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view_for_projection_light(
            Projection::Perspective,
            [15.5, 15.5, 15.5],
            0.5,
            40.0,
            IsoLightState::attached_camera(),
        ),
        perspective_extent,
        &fixtures.semantic[2],
    );
    let (perspective_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &perspective_intent,
            requirements: &perspective_requirements,
            leases: &f32_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(
        pixel(&perspective_capture, 47, 47),
        ([251, 0, 0, 255], 1, 1),
        "the analytic center perspective ray crosses the volume maximum first"
    );

    let (affine_intent, affine_requirements) = intent_and_requirements(
        43,
        11,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view_for([7.375, 5.5, 5.2], 1.0 / 32.0, 16.0),
        extent,
        &[fixtures.affine],
    );
    let affine_lease = affine_leases
        .get(&fixtures.affine)
        .expect("full-affine fixture lease exists");
    let (affine_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &affine_intent,
            requirements: &affine_requirements,
            leases: &[affine_lease],
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(
        pixel(&affine_capture, 48, 48),
        ([241, 0, 0, 255], 1, 1),
        "the rotated, sheared, translated center ray has a fixed MIP hand fact"
    );

    let (detached_gradient_intent, detached_gradient_requirements) = intent_and_requirements(
        44,
        2,
        gradient_iso_state,
        volume_view_for_light(
            [15.5, 15.5, 15.5],
            0.5,
            40.0,
            IsoLightState::detached_screen(0.25, -0.5).unwrap(),
        ),
        extent,
        &fixtures.semantic[2],
    );
    let (detached_gradient_capture, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &detached_gradient_intent,
            requirements: &detached_gradient_requirements,
            leases: &f32_leases,
        },
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(
        detached_gradient_capture.coverage(),
        gradient_capture.coverage()
    );
    assert_eq!(
        detached_gradient_capture.validity(),
        gradient_capture.validity()
    );

    let (small_gpu, capacity_counters) = {
        let mut small_gpu = pollster::block_on(WgpuRenderRuntime::new(
            WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES)
                .expect("small qualification ledger is valid"),
        ))
        .expect("small-ledger runtime uses the same qualifying Vulkan adapter");
        let small_presentation = small_gpu
            .register_presentation(upload_extent)
            .expect("small-ledger presentation registers")
            .token();
        let mut capacity_counters = Counters::default();
        for (index, key) in fixtures.upload.iter().enumerate() {
            let (intent, requirements) = intent_and_requirements(
                100 + index as u64,
                3,
                mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
                cross_section_view([96.0, 96.0, 32.0], 1.0),
                upload_extent,
                &[*key],
            );
            let lease = upload_leases.get(key).expect("upload lease exists");
            let report = loop {
                let report = small_gpu
                    .execute_frame(
                        small_presentation,
                        &catalog,
                        &intent,
                        &requirements,
                        &[lease],
                    )
                    .expect("small-ledger eviction sequence executes");
                if !report.deferred_by_backpressure() {
                    break report;
                }
                assert!(Instant::now() < deadline, "staging slot recall timed out");
                std::thread::yield_now();
            };
            assert_eq!(report.uploaded_resources(), 1);
            assert_eq!(report.newly_resident_keys(), &[*key]);
            assert_eq!(report.evicted_keys().len(), usize::from(index >= 8));
            capacity_counters.record(&report);
        }
        let first_key = fixtures.upload[0];
        let (replacement_intent, replacement_requirements) = intent_and_requirements(
            109,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([96.0, 96.0, 32.0], 1.0),
            upload_extent,
            &[first_key],
        );
        let first_lease = upload_leases
            .get(&first_key)
            .expect("first upload lease exists");
        let replacement = loop {
            let report = small_gpu
                .execute_frame(
                    small_presentation,
                    &catalog,
                    &replacement_intent,
                    &replacement_requirements,
                    &[first_lease],
                )
                .expect("evicted residency can be replaced from a genuine lease");
            if !report.deferred_by_backpressure() {
                break report;
            }
            assert!(Instant::now() < deadline, "staging slot recall timed out");
            std::thread::yield_now();
        };
        assert_eq!(replacement.uploaded_resources(), 1);
        assert_eq!(replacement.newly_resident_keys(), &[first_key]);
        assert_eq!(replacement.evicted_keys().len(), 1);
        capacity_counters.record(&replacement);
        assert!(
            small_gpu.diagnostics().resident_payload_bytes()
                <= small_gpu.diagnostics().payload_capacity_bytes()
        );
        assert!(
            small_gpu.diagnostics().peak_explicit_staging_bytes()
                <= small_gpu.diagnostics().transfer_capacity_bytes(),
            "persistent staging slots must remain inside the transfer ledger"
        );

        let huge_extent = RenderExtent::new(1920, 1080).expect("maximum extent is valid");
        let (capacity_intent, capacity_requirements) = intent_and_requirements(
            110,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([96.0, 96.0, 32.0], 1.0),
            huge_extent,
            &[first_key],
        );
        let submissions_before_capacity = small_gpu.diagnostics().queue_submissions();
        assert!(matches!(
            small_gpu.execute_frame(
                small_presentation,
                &catalog,
                &capacity_intent,
                &capacity_requirements,
                &[first_lease],
            ),
            Err(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::DisplayTarget,
                ..
            })
        ));
        assert_eq!(
            small_gpu.diagnostics().queue_submissions(),
            submissions_before_capacity
        );
        (small_gpu, capacity_counters)
    };

    drop(u8_leases);
    drop(u16_leases);
    drop(f32_leases);
    drop(upload_updates);
    drop(work_updates);
    drop(empty_leases);
    drop(sparse_leases);
    drop(affine_leases);
    dataset_runtime
        .request_shutdown()
        .expect("fixture runtime begins bounded shutdown");
    drop(work_leases);
    drop(upload_leases);
    drop(leases);
    drop(dataset_runtime);

    let (lease_release_intent, lease_release_requirements) = intent_and_requirements(
        45,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 0.0], 0.5),
        extent,
        &fixtures.semantic[0],
    );
    let lease_release_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &lease_release_intent,
            &lease_release_requirements,
            &[],
        )
        .expect("GPU residency remains renderable after all runtime leases are released");
    counters.record(&lease_release_report);
    let lease_release_capture = poll_capture(
        &mut gpu,
        lease_release_report
            .validation_capture()
            .expect("lease-release validation capture exists"),
        deadline,
    );
    assert_exact_bytes(
        "lease-release RGBA8",
        lease_release_capture.rgba8(),
        current_capture.rgba8(),
    );
    assert_exact_bytes(
        "lease-release coverage",
        lease_release_capture.coverage(),
        current_capture.coverage(),
    );
    assert_exact_bytes(
        "lease-release validity",
        lease_release_capture.validity(),
        current_capture.validity(),
    );

    assert_eq!(gpu.diagnostics().validation_error_count(), 0);
    assert_eq!(small_gpu.diagnostics().validation_error_count(), 0);
    assert!(
        Instant::now() <= deadline,
        "qualification exceeded its 60-second deadline"
    );
    emit_evidence(
        gpu.diagnostics(),
        &counters,
        small_gpu.diagnostics(),
        &capacity_counters,
        rgba8_max_delta,
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn multi_presentation_pins_are_union_scoped_and_deactivatable() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let leases = load_keys(
        &dataset_runtime,
        &fixtures.upload,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 1),
        deadline,
    );
    let extent = RenderExtent::new(1, 1).unwrap();
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES).unwrap(),
    ))
    .expect("trusted workstation exposes a qualifying Vulkan adapter");
    let presentations = (0..4)
        .map(|_| gpu.register_presentation(extent).unwrap().token())
        .collect::<Vec<_>>();

    let execute = |gpu: &mut WgpuRenderRuntime,
                   token: PresentationToken,
                   frame: u64,
                   key: DatasetResourceKey| {
        let (intent, requirements) = intent_and_requirements(
            frame,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([96.0, 96.0, 32.0], 1.0),
            extent,
            &[key],
        );
        loop {
            let report = gpu
                .execute_frame(
                    token,
                    &catalog,
                    &intent,
                    &requirements,
                    &[leases.get(&key).expect("pin fixture lease exists")],
                )
                .expect("pin fixture frame executes");
            if !report.deferred_by_backpressure() {
                break report;
            }
            assert!(Instant::now() < deadline, "pin fixture staging timed out");
            std::thread::yield_now();
        }
    };

    for (index, token) in presentations.iter().copied().enumerate() {
        let report = execute(&mut gpu, token, 700 + index as u64, fixtures.upload[index]);
        assert_eq!(report.newly_resident_keys(), &[fixtures.upload[index]]);
    }
    for index in 4..9 {
        let report = execute(
            &mut gpu,
            presentations[0],
            700 + index as u64,
            fixtures.upload[index],
        );
        if index == 8 {
            assert_eq!(report.evicted_keys().len(), 1);
            assert!(
                !fixtures.upload[1..4].contains(&report.evicted_keys()[0]),
                "other live presentations' current resources are pinned"
            );
        }
    }
    for (index, token) in presentations.iter().copied().enumerate().take(4).skip(1) {
        let warm = execute(&mut gpu, token, 720 + index as u64, fixtures.upload[index]);
        assert_eq!(warm.uploaded_resources(), 0);
        assert!(warm.evicted_keys().is_empty());
    }

    for token in presentations.iter().copied().skip(1) {
        gpu.deactivate_presentation(token).unwrap();
    }
    let replacement = execute(&mut gpu, presentations[0], 730, fixtures.upload[0]);
    assert_eq!(replacement.newly_resident_keys(), &[fixtures.upload[0]]);
    assert_eq!(
        replacement.evicted_keys(),
        &[fixtures.upload[4]],
        "warm revisits refresh nonempty payload age; the oldest untouched inactive page leaves first"
    );

    let residency_epoch_before_retirement = gpu.residency_epoch();
    let invalidation_epoch_before_retirement = gpu.residency_invalidation_epoch();
    assert!(gpu.resident_keys().len() > 0);
    assert!(gpu.resident_payload_bytes() > 0);

    gpu.retire_dataset_generation();

    assert_eq!(gpu.resident_keys().len(), 0);
    assert_eq!(gpu.resident_payload_bytes(), 0);
    assert_eq!(gpu.diagnostics().empty_resident_metadata_records(), 0);
    assert!(gpu.residency_epoch() > residency_epoch_before_retirement);
    assert!(gpu.residency_invalidation_epoch() > invalidation_epoch_before_retirement);
    let replacement_generation = execute(&mut gpu, presentations[0], 740, fixtures.upload[0]);
    assert_eq!(
        replacement_generation.newly_resident_keys(),
        &[fixtures.upload[0]]
    );
    assert!(replacement_generation.evicted_keys().is_empty());

    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
    prove_changed_requirement_body_revisits_refresh_payload_eviction_age();
}

fn prove_changed_requirement_body_revisits_refresh_payload_eviction_age() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let leases = load_keys(
        &dataset_runtime,
        &fixtures.upload,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 17),
        deadline,
    );
    let extent = RenderExtent::new(1, 1).unwrap();
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES).unwrap(),
    ))
    .expect("trusted workstation exposes a qualifying Vulkan adapter");
    let presentation = gpu.register_presentation(extent).unwrap().token();

    let execute = |gpu: &mut WgpuRenderRuntime, frame: u64, key: DatasetResourceKey| {
        let (intent, requirements) = intent_and_requirements(
            frame,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([96.0, 96.0, 32.0], 1.0),
            extent,
            &[key],
        );
        loop {
            let report = gpu
                .execute_frame(
                    presentation,
                    &catalog,
                    &intent,
                    &requirements,
                    &[leases.get(&key).expect("LRU fixture lease exists")],
                )
                .expect("LRU fixture frame executes");
            if !report.deferred_by_backpressure() {
                break report;
            }
            assert!(Instant::now() < deadline, "LRU fixture staging timed out");
            std::thread::yield_now();
        }
    };

    for (index, key) in fixtures.upload[..8].iter().copied().enumerate() {
        let report = execute(&mut gpu, 800 + index as u64, key);
        assert_eq!(report.uploaded_resources(), 1);
        assert!(report.evicted_keys().is_empty());
    }
    let revisit_a = execute(&mut gpu, 808, fixtures.upload[0]);
    assert_eq!(revisit_a.uploaded_resources(), 0);
    let admit_c = execute(&mut gpu, 809, fixtures.upload[8]);
    assert_eq!(admit_c.uploaded_resources(), 1);
    assert_eq!(admit_c.evicted_keys(), &[fixtures.upload[1]]);
    let revisit_a_again = execute(&mut gpu, 810, fixtures.upload[0]);
    assert_eq!(revisit_a_again.uploaded_resources(), 0);
    assert!(revisit_a_again.evicted_keys().is_empty());

    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
}

fn prove_segmented_payload_upload_and_sampling_cross_the_first_binding() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let semantic_key = fixtures.semantic[0][0];
    let mut keys = fixtures.upload[..4].to_vec();
    keys.push(semantic_key);
    let leases = load_keys(
        &dataset_runtime,
        &keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 18),
        deadline,
    );
    let extent = RenderExtent::new(1, 1).unwrap();
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new_with_payload_segment_limit(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES)
            .unwrap()
            .with_validation_capture(true),
        4 * MIB,
    ))
    .expect("trusted workstation exposes a qualifying Vulkan adapter");
    assert_eq!(gpu.diagnostics().payload_segment_count(), 3);
    assert_eq!(
        gpu.diagnostics().payload_segment_capacity_bytes(),
        &[4 * MIB, 4 * MIB, MIB / 4, 0]
    );
    assert_eq!(
        gpu.diagnostics()
            .payload_segment_capacity_bytes()
            .iter()
            .sum::<u64>(),
        gpu.diagnostics().payload_capacity_bytes()
    );
    let presentation = gpu.register_presentation(extent).unwrap().token();

    for (index, key) in fixtures.upload[..4].iter().copied().enumerate() {
        let origin = key.region().origin();
        let center = [
            origin[2] as f64 + 31.5,
            origin[1] as f64 + 31.5,
            origin[0] as f64 + 31.5,
        ];
        let (intent, requirements) = intent_and_requirements(
            900 + index as u64,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view(center, 1.0),
            extent,
            &[key],
        );
        let report = loop {
            let report = gpu
                .execute_frame(
                    presentation,
                    &catalog,
                    &intent,
                    &requirements,
                    &[leases.get(&key).unwrap()],
                )
                .unwrap();
            if !report.deferred_by_backpressure() {
                break report;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(report.uploaded_resources(), 1);
        assert!(report.evicted_keys().is_empty());
        let _ = poll_capture(&mut gpu, report.validation_capture().unwrap(), deadline);
    }

    let semantic_lease = leases.get(&semantic_key).unwrap();
    let (intent, requirements) = intent_and_requirements(
        904,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([7.5, 7.5, 7.5], 1.0),
        extent,
        &[semantic_key],
    );
    let mut counters = Counters::default();
    let (capture, _) = execute_and_compare(
        &mut gpu,
        presentation,
        ComparisonInput {
            catalog: &catalog,
            intent: &intent,
            requirements: &requirements,
            leases: &[semantic_lease],
        },
        deadline,
        &mut counters,
    );
    assert_eq!(capture.coverage(), &[1]);
    assert_eq!(capture.validity(), &[1]);
    assert_eq!(counters.resources_uploaded, 1);
    assert!(gpu.diagnostics().resident_payload_bytes() > 4 * MIB);

    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn rays_beyond_the_removed_16384_sample_cap_reach_the_far_voxel() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let key = fixtures.work[0];
    let leases = load_keys(
        &dataset_runtime,
        &[key],
        CancellationGeneration::for_scope(REQUEST_SCOPE, 1),
        deadline,
    );
    let extent = RenderExtent::new(1, 1).unwrap();
    let half_angle = std::f64::consts::FRAC_PI_4;
    let orientation =
        UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos()).unwrap();
    let view = RenderViewIntent::volume(
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(16_383.5, 0.0, 0.0).unwrap(),
            orientation,
            1.0,
            40_000.0,
            40_000.0,
        )
        .unwrap(),
        IsoLightState::attached_camera(),
    );
    let (intent, requirements) = intent_and_requirements(
        740,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        view,
        extent,
        &fixtures.work,
    );
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .unwrap()
            .with_validation_capture(true),
    ))
    .unwrap();
    let presentation = gpu.register_presentation(extent).unwrap().token();
    let report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &intent,
            &requirements,
            &[leases.get(&key).unwrap()],
        )
        .unwrap();
    let capture = poll_capture(&mut gpu, report.validation_capture().unwrap(), deadline);
    assert_eq!(capture.coverage(), &[0]);
    assert_eq!(
        capture.validity(),
        &[1],
        "the far x=0 resident voxel lies beyond the former silent 16,384-step cutoff"
    );
    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn multichannel_semantics_are_order_independent() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let leases = load_keys(
        &dataset_runtime,
        &fixtures.multichannel,
        generation,
        deadline,
    );
    let dvr_pick_leases = load_keys(
        &dataset_runtime,
        &fixtures.semantic[2],
        generation,
        deadline,
    );
    let extent = RenderExtent::new(64, 64).expect("multichannel extent is valid");
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("multichannel GPU ledger is valid")
            .with_validation_capture(true),
    ))
    .expect("trusted workstation exposes a qualifying Vulkan adapter");
    let presentation = gpu
        .register_presentation(extent)
        .expect("multichannel presentation registers")
        .token();
    let key_for = |layer: u32| fixtures.multichannel[(layer - 5) as usize];
    let leases_for = |layers: &[u32]| {
        layers
            .iter()
            .map(|layer| {
                leases
                    .get(&key_for(*layer))
                    .expect("multichannel lease exists") as &dyn ResourceLease
            })
            .collect::<Vec<_>>()
    };
    let layers_for = |order: [u32; 2], state: mirante4d_domain::RenderState| {
        order
            .into_iter()
            .map(|layer| {
                LayerRenderIntent::new(
                    LogicalLayerKey::new(layer),
                    transfer_color_opacity(
                        0.0,
                        1.0,
                        if layer == 5 {
                            [1.0, 0.0, 0.0]
                        } else {
                            [0.0, 1.0, 0.0]
                        },
                        if layer == 5 { 0.4 } else { 0.6 },
                    ),
                    state,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut reference_counters = Counters::default();

    let mip_state = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
    let mut mip_captures = Vec::new();
    for (frame, order) in [(600_u64, [5_u32, 6_u32]), (601, [6, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            layers_for(order, mip_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(delta <= 1, "multichannel MIP reference delta was {delta}");
        mip_captures.push(capture);
    }
    assert_exact_bytes(
        "order-independent MIP RGBA8",
        mip_captures[0].rgba8(),
        mip_captures[1].rgba8(),
    );
    assert_pixel_near(&mip_captures[0], 32, 32, [26, 115, 0, 194], 1, 1);

    let mut section_captures = Vec::new();
    for (frame, order) in [(602_u64, [5_u32, 6_u32]), (603, [6, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            cross_section_view([3.5, 3.5, 3.5], 0.125),
            extent,
            layers_for(order, mip_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(
            delta <= 1,
            "multichannel cross-section reference delta was {delta}"
        );
        section_captures.push(capture);
    }
    assert_exact_bytes(
        "order-independent cross-section RGBA8",
        section_captures[0].rgba8(),
        section_captures[1].rgba8(),
    );
    assert_pixel_near(&section_captures[0], 32, 32, [26, 115, 0, 194], 1, 1);

    let dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).expect("multichannel DVR window is valid"),
            TransferCurve::linear(),
        ),
        0.05,
    )
    .expect("multichannel DVR state is valid");
    let mut dvr_captures = Vec::new();
    for (frame, order) in [(604_u64, [5_u32, 6_u32]), (605, [6, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            layers_for(order, dvr_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(delta <= 1, "joint DVR reference delta was {delta}");
        dvr_captures.push(capture);
    }
    let dvr_delta = dvr_captures[0]
        .rgba8()
        .iter()
        .zip(dvr_captures[1].rgba8())
        .map(|(first, second)| first.abs_diff(*second))
        .max()
        .unwrap_or(0);
    assert!(dvr_delta <= 1, "fused DVR order delta was {dvr_delta}");
    let (dvr_pixel, dvr_coverage, dvr_validity) = pixel(&dvr_captures[0], 32, 32);
    assert_eq!((dvr_coverage, dvr_validity), (1, 1));
    assert!(dvr_pixel[0] > 0 && dvr_pixel[1] > dvr_pixel[0]);
    assert!(dvr_pixel[3] > 0 && dvr_pixel[3] < 255);

    let opacity_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            TransferCurve::linear(),
        ),
        1.0,
    )
    .unwrap();
    let mut opacity_captures = Vec::new();
    for (frame, opacity) in [(616_u64, 0.0_f32), (617, 0.5), (618, 1.0)] {
        let key = key_for(5);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            vec![LayerRenderIntent::new(
                LogicalLayerKey::new(5),
                transfer_color_opacity(0.0, 1.0, [1.0, 0.0, 0.0], opacity),
                opacity_state,
            )],
            &[key],
        );
        let mut counters = Counters::default();
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &leases_for(&[5]),
            },
            deadline,
            &mut counters,
        );
        assert!(
            delta <= 1,
            "single-channel opacity contract delta was {delta}"
        );
        opacity_captures.push(capture);
    }
    let zero_opacity = pixel(&opacity_captures[0], 32, 32);
    let half_opacity = pixel(&opacity_captures[1], 32, 32);
    let full_opacity = pixel(&opacity_captures[2], 32, 32);
    assert_eq!(zero_opacity, ([0, 0, 0, 0], 1, 1));
    assert!(half_opacity.0[3] > 0 && half_opacity.0[3] < full_opacity.0[3]);

    for (frame, second_layer) in [(619_u64, 6_u32), (620, 7)] {
        let keys = [key_for(5), key_for(second_layer)];
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 4.5], 0.125, 32.0),
            extent,
            vec![
                LayerRenderIntent::new(
                    LogicalLayerKey::new(5),
                    transfer_color_opacity(0.0, 1.0, [1.0, 0.0, 0.0], 0.5),
                    opacity_state,
                ),
                LayerRenderIntent::new(
                    LogicalLayerKey::new(second_layer),
                    transfer_color_opacity(0.0, 1.0, [0.0, 1.0, 0.0], 0.0),
                    opacity_state,
                ),
            ],
            &keys,
        );
        let capture = execute_and_capture(
            &mut gpu,
            presentation,
            &catalog,
            &intent,
            &requirements,
            &leases_for(&[5, second_layer]),
            deadline,
        );
        let alpha_delta = pixel(&capture, 32, 32).0[3].abs_diff(half_opacity.0[3]);
        assert!(
            alpha_delta <= 1,
            "fused/general zero-opacity channel changed canonical alpha by {alpha_delta}"
        );
    }

    let smooth_dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::SmoothLinear,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).unwrap(),
            TransferCurve::linear(),
        ),
        0.05,
    )
    .unwrap();
    let mut smooth_dvr_captures = Vec::new();
    for (frame, order) in [(621_u64, [5_u32, 6_u32]), (622, [6, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            layers_for(order, smooth_dvr_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(delta <= 1, "SmoothLinear DVR reference delta was {delta}");
        smooth_dvr_captures.push(capture);
    }
    let smooth_dvr_delta = smooth_dvr_captures[0]
        .rgba8()
        .iter()
        .zip(smooth_dvr_captures[1].rgba8())
        .map(|(first, second)| first.abs_diff(*second))
        .max()
        .unwrap_or(0);
    assert!(smooth_dvr_delta <= 1);

    let mut mixed_affine_dvr_captures = Vec::new();
    for (frame, order) in [(623_u64, [5_u32, 7_u32]), (624, [7, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 4.5], 0.125, 32.0),
            extent,
            layers_for(order, dvr_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(delta <= 1, "mixed-affine DVR reference delta was {delta}");
        mixed_affine_dvr_captures.push(capture);
    }
    let mixed_affine_delta = mixed_affine_dvr_captures[0]
        .rgba8()
        .iter()
        .zip(mixed_affine_dvr_captures[1].rgba8())
        .map(|(first, second)| first.abs_diff(*second))
        .max()
        .unwrap_or(0);
    assert!(mixed_affine_delta <= 1);

    let iso_state =
        mirante4d_domain::RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.1)
            .expect("multichannel ISO state is valid");
    let mut iso_captures = Vec::new();
    for (frame, order) in [(625_u64, [5_u32, 7_u32]), (626, [7, 5])] {
        let keys = order.map(key_for);
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 4.5], 0.125, 32.0),
            extent,
            layers_for(order, iso_state),
            &keys,
        );
        let case_leases = leases_for(&order);
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut reference_counters,
        );
        assert!(delta <= 1, "depth-sorted ISO reference delta was {delta}");
        iso_captures.push(capture);
    }
    assert_exact_bytes(
        "depth-sorted ISO RGBA8",
        iso_captures[0].rgba8(),
        iso_captures[1].rgba8(),
    );
    assert_pixel_near(&iso_captures[0], 32, 32, [10, 115, 0, 194], 1, 1);

    let mixed_cases = vec![
        (627_u64, vec![(5_u32, mip_state), (6, dvr_state)]),
        (628, vec![(6_u32, dvr_state), (5, mip_state)]),
        (629, vec![(5_u32, dvr_state), (7, iso_state)]),
        (630, vec![(7_u32, iso_state), (5, dvr_state)]),
        (
            631,
            vec![(5_u32, mip_state), (6, dvr_state), (7, iso_state)],
        ),
        (
            632,
            vec![(7_u32, iso_state), (6, dvr_state), (5, mip_state)],
        ),
    ];
    let mut mixed_captures = Vec::new();
    for (frame, spec) in mixed_cases {
        let layers = spec
            .iter()
            .map(|(layer, state)| {
                LayerRenderIntent::new(
                    LogicalLayerKey::new(*layer),
                    transfer_color_opacity(
                        0.0,
                        1.0,
                        match *layer {
                            5 => [1.0, 0.0, 0.0],
                            6 => [0.0, 1.0, 0.0],
                            _ => [0.0, 0.0, 1.0],
                        },
                        0.6,
                    ),
                    *state,
                )
            })
            .collect::<Vec<_>>();
        let layer_ordinals = spec.iter().map(|(layer, _)| *layer).collect::<Vec<_>>();
        let keys = layer_ordinals
            .iter()
            .map(|layer| key_for(*layer))
            .collect::<Vec<_>>();
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 4.5], 0.125, 32.0),
            extent,
            layers,
            &keys,
        );
        let case_leases = leases_for(&layer_ordinals);
        let mut counters = Counters::default();
        let (capture, delta) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
            &mut counters,
        );
        assert!(delta <= 1, "mixed-mode view-order delta was {delta}");
        mixed_captures.push(capture);
    }
    for pair in mixed_captures.chunks_exact(2) {
        assert_ne!(
            pixel(&pair[0], 32, 32).0,
            pixel(&pair[1], 32, 32).0,
            "reversing authored mixed-mode order must change composition"
        );
    }

    // A constant all-valid volume makes the accepted mode-specific pick
    // selection independently obvious: nearest threshold hit, nearest raw
    // argmax tie, and nearest maximum-opacity contribution all select the
    // same 0.25-f32 voxel through the shared page/payload path.
    let pick_key = fixtures.multichannel[0];
    let pick_lease = leases.get(&pick_key).expect("pick fixture lease exists");
    let pick_cases = [
        (633_u64, mip_state, VolumePickPolicy::MipArgmax),
        (
            634,
            mirante4d_domain::RenderState::iso(
                SamplingPolicy::VoxelExact,
                IsoShadingPolicy::Flat,
                0.1,
            )
            .expect("pick ISO state is valid"),
            VolumePickPolicy::FirstThresholdHit,
        ),
        (635, dvr_state, VolumePickPolicy::MaximumOpacityContribution),
    ];
    let mut picked_positions = Vec::new();
    for (frame, state, policy) in pick_cases {
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            vec![LayerRenderIntent::new(
                LogicalLayerKey::new(5),
                transfer_color(0.0, 1.0, [1.0, 0.0, 0.0]),
                state,
            )],
            &[pick_key],
        );
        let report = gpu
            .execute_frame(
                presentation,
                &catalog,
                &intent,
                &requirements,
                &[pick_lease],
            )
            .expect("pick qualification frame executes");
        let presented = report
            .presentation()
            .cloned()
            .expect("pick qualification frame is presented");
        let _ = poll_capture(
            &mut gpu,
            report
                .validation_capture()
                .expect("pick qualification capture exists"),
            deadline,
        );
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(0),
            LogicalLayerKey::new(5),
            [31.5, 31.5],
            policy,
        )
        .expect("pick qualification query is valid");
        let ticket = gpu
            .request_pick(presentation, query)
            .expect("bounded compute pick is submitted");
        let result = poll_pick(&mut gpu, ticket, deadline);
        assert_eq!(result.query(), query);
        assert_eq!(result.kind(), VolumePickHitKind::Voxel);
        assert_eq!(result.value(), Some(VolumePickValue::IntensityF32(0.25)));
        assert_eq!(result.completeness(), VolumePickCompleteness::Exact);
        assert!(result.ray_distance_world().is_some_and(|value| value > 0.0));
        picked_positions.push(
            result
                .world_position()
                .expect("voxel pick has a world position"),
        );
        assert_eq!(
            gpu.poll_pick(ticket),
            Err(WgpuRenderRuntimeError::UnknownVolumePick),
            "completed pick tickets are one-shot"
        );
    }
    assert!(picked_positions.windows(2).all(|pair| pair[0] == pair[1]));

    // This increasing ray and partial layer opacity distinguish the canonical
    // renderer/reference law from incorrectly folding layer opacity into
    // optical depth. For alpha = 0.5 * (1 - exp(-16 * value)), z=1 has the
    // greatest front-to-back opacity contribution. The folded form instead
    // selects z=2, so this is a behavioral parity fact rather than a constant-
    // volume tie.
    let dvr_pick_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 1.0).expect("DVR pick window is valid"),
            TransferCurve::linear(),
        ),
        16.0,
    )
    .expect("partial-opacity DVR pick state is valid");
    let increasing_ray_view = RenderViewIntent::volume(
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(15.5, 15.5, 15.5).expect("DVR pick target is finite"),
            UnitQuaternion::new_xyzw(0.0, 1.0, 0.0, 0.0).expect("DVR pick orientation is valid"),
            0.5,
            320.0,
            40.0,
        )
        .expect("DVR pick camera is valid"),
        IsoLightState::attached_camera(),
    );
    let (dvr_pick_intent, dvr_pick_requirements) = multichannel_intent_and_requirements(
        636,
        increasing_ray_view,
        extent,
        vec![LayerRenderIntent::new(
            LogicalLayerKey::new(2),
            transfer_color_opacity(0.0, 1.0, [1.0, 0.0, 0.0], 0.5),
            dvr_pick_state,
        )],
        &fixtures.semantic[2],
    );
    let dvr_pick_borrowed = borrowed_leases(&fixtures.semantic[2], &dvr_pick_leases, None);
    let dvr_pick_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &dvr_pick_intent,
            &dvr_pick_requirements,
            &dvr_pick_borrowed,
        )
        .expect("partial-opacity DVR pick frame executes");
    let dvr_pick_presented = dvr_pick_report
        .presentation()
        .cloned()
        .expect("partial-opacity DVR pick frame is presented");
    let _ = poll_capture(
        &mut gpu,
        dvr_pick_report
            .validation_capture()
            .expect("partial-opacity DVR pick capture exists"),
        deadline,
    );
    let dvr_pick_query = VolumePickQuery::new(
        &dvr_pick_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(2),
        [31.5, 31.5],
        VolumePickPolicy::MaximumOpacityContribution,
    )
    .expect("partial-opacity DVR pick query is valid");
    let dvr_pick_ticket = gpu
        .request_pick(presentation, dvr_pick_query)
        .expect("partial-opacity DVR pick is accepted");
    let dvr_pick_result = poll_pick(&mut gpu, dvr_pick_ticket, deadline);
    assert_eq!(dvr_pick_result.query(), dvr_pick_query);
    assert_eq!(dvr_pick_result.kind(), VolumePickHitKind::Voxel);
    assert_eq!(
        dvr_pick_result.value(),
        Some(VolumePickValue::IntensityF32(1552.0_f32 / 32_767.0))
    );
    assert_eq!(
        dvr_pick_result
            .world_position()
            .expect("partial-opacity DVR pick has a position")
            .components(),
        [16.0, 16.0, 1.0]
    );
    assert_eq!(
        dvr_pick_result.completeness(),
        VolumePickCompleteness::Exact
    );

    let smooth_pick_state = mirante4d_domain::RenderState::mip(SamplingPolicy::SmoothLinear);
    let (smooth_pick_intent, smooth_pick_requirements) = multichannel_intent_and_requirements(
        637,
        volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
        extent,
        vec![LayerRenderIntent::new(
            LogicalLayerKey::new(5),
            transfer_color(0.0, 1.0, [1.0, 0.0, 0.0]),
            smooth_pick_state,
        )],
        &[pick_key],
    );
    let smooth_report = gpu
        .execute_frame(
            presentation,
            &catalog,
            &smooth_pick_intent,
            &smooth_pick_requirements,
            &[pick_lease],
        )
        .expect("SmoothLinear pick frame executes");
    let smooth_presented = smooth_report
        .presentation()
        .cloned()
        .expect("SmoothLinear pick frame is presented");
    let _ = poll_capture(
        &mut gpu,
        smooth_report
            .validation_capture()
            .expect("SmoothLinear pick capture exists"),
        deadline,
    );
    let smooth_query = VolumePickQuery::new(
        &smooth_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(5),
        [31.5, 31.5],
        VolumePickPolicy::MipArgmax,
    )
    .expect("SmoothLinear pick query is valid");
    let smooth_ticket = gpu
        .request_pick(presentation, smooth_query)
        .expect("SmoothLinear async pick is accepted");
    let smooth_result = poll_pick(&mut gpu, smooth_ticket, deadline);
    assert_eq!(smooth_result.kind(), VolumePickHitKind::InterpolatedSample);
    assert_eq!(
        smooth_result.value(),
        Some(VolumePickValue::IntensityF32(0.25))
    );
    assert_eq!(smooth_result.completeness(), VolumePickCompleteness::Exact);

    assert_eq!(gpu.diagnostics().pick_submissions(), 5);
    assert_eq!(gpu.diagnostics().completed_picks(), 5);

    assert_eq!(gpu.diagnostics().validation_error_count(), 0);
    dataset_runtime
        .request_shutdown()
        .expect("multichannel fixture runtime begins bounded shutdown");
    drop(leases);
    drop(dvr_pick_leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "multichannel qualification exceeded its deadline"
    );
    prove_segmented_payload_upload_and_sampling_cross_the_first_binding();
}

#[test]
#[ignore = "requires a trusted Vulkan GPU with timestamp-query support for measurements"]
fn resident_volume_gpu_timing() {
    const MEASURED_TRIALS: usize = 5;
    const CPU_OVERHEAD_TRIALS: usize = MEASURED_TRIALS;

    let deadline = Instant::now() + Duration::from_secs(120);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let upload_leases = load_keys(&dataset_runtime, &fixtures.upload, generation, deadline);
    let upload_borrowed = borrowed_leases(&fixtures.upload, &upload_leases, None);
    let adversarial_leases = load_keys(
        &dataset_runtime,
        &fixtures.adversarial,
        generation,
        deadline,
    );
    let adversarial_borrowed = borrowed_leases(&fixtures.adversarial, &adversarial_leases, None);
    let base_extent = RenderExtent::new(1280, 720).expect("timing extent is valid");
    let mut gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(PERFORMANCE_GPU_BYTES)
            .expect("timing GPU ledger is valid")
            .with_gpu_timing(true),
    ))
    .expect("trusted workstation exposes a qualifying Vulkan adapter");
    assert_eq!(gpu.diagnostics().backend(), "Vulkan");
    let presentation = gpu
        .register_presentation(base_extent)
        .expect("timing presentation registers")
        .token();
    let mut timing_off_gpu = pollster::block_on(WgpuRenderRuntime::new(
        WgpuRenderRuntimeConfig::new(PERFORMANCE_GPU_BYTES)
            .expect("timing-off GPU ledger is valid"),
    ))
    .expect("trusted workstation exposes a timing-off Vulkan runtime");
    assert_eq!(timing_off_gpu.diagnostics().backend(), "Vulkan");
    let timing_off_presentation = timing_off_gpu
        .register_presentation(base_extent)
        .expect("timing-off presentation registers")
        .token();
    let mip_state = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
    let (resident_intent, resident_requirements) = intent_and_requirements(
        500,
        3,
        mip_state,
        volume_view_for([96.0, 96.0, 32.0], 192.0 / 720.0, 256.0),
        base_extent,
        &fixtures.upload,
    );

    // Establish an exact, genuinely resident nine-brick s0 working set. The
    // upload ceiling intentionally requires two submissions; neither upload is
    // included in the warm volume-pass distribution below.
    let first_upload = gpu
        .execute_frame(
            presentation,
            &catalog,
            &resident_intent,
            &resident_requirements,
            &upload_borrowed,
        )
        .expect("first resident-volume upload executes");
    assert_eq!(first_upload.uploaded_resources(), 8);
    assert_eq!(first_upload.payload_upload_bytes(), 8 * MIB);
    let mut cold_payload_copy_ns = Vec::new();
    if let Some(ticket) = first_upload.gpu_timing() {
        assert_eq!(ticket.target(), presentation);
        assert_eq!(ticket.generation(), resident_intent.frame());
        assert_eq!(ticket.pass_kind(), RenderPassKind::Volume);
        let timing = poll_gpu_timing(&mut gpu, ticket, deadline);
        assert_eq!(timing.ticket(), ticket);
        assert_eq!(timing.target(), presentation);
        assert_eq!(timing.generation(), resident_intent.frame());
        assert_eq!(timing.pass_kind(), RenderPassKind::Volume);
        let render_pass_ns = timing
            .render_pass_ns()
            .expect("known-copy frame rendered a volume pass");
        assert_eq!(
            timing.payload_copy_ns().is_some(),
            gpu.diagnostics().gpu_payload_copy_timestamps_supported(),
            "known encoded copy work is available exactly when encoder timestamps are supported"
        );
        assert_eq!(
            timing.batch_gpu_envelope_ns().is_some(),
            gpu.diagnostics().gpu_payload_copy_timestamps_supported()
        );
        if let Some(envelope) = timing.batch_gpu_envelope_ns() {
            assert!(envelope >= render_pass_ns);
            assert!(envelope >= timing.payload_copy_ns().unwrap_or(0));
        }
        if let Some(elapsed) = timing.payload_copy_ns() {
            cold_payload_copy_ns.push(elapsed);
        }
    }
    let second_upload = gpu
        .execute_frame(
            presentation,
            &catalog,
            &resident_intent,
            &resident_requirements,
            &upload_borrowed,
        )
        .expect("second resident-volume upload executes");
    assert_eq!(second_upload.uploaded_resources(), 1);
    assert_eq!(second_upload.payload_upload_bytes(), MIB);
    assert_eq!(
        second_upload
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    if let Some(ticket) = second_upload.gpu_timing() {
        let timing = poll_gpu_timing(&mut gpu, ticket, deadline);
        assert_eq!(timing.ticket(), ticket);
        assert!(timing.render_pass_ns().is_some());
        assert_eq!(
            timing.payload_copy_ns().is_some(),
            gpu.diagnostics().gpu_payload_copy_timestamps_supported()
        );
        assert_eq!(
            timing.batch_gpu_envelope_ns().is_some(),
            gpu.diagnostics().gpu_payload_copy_timestamps_supported()
        );
        if let Some(elapsed) = timing.payload_copy_ns() {
            cold_payload_copy_ns.push(elapsed);
        }
    }
    let timing_off_first_upload = timing_off_gpu
        .execute_frame(
            timing_off_presentation,
            &catalog,
            &resident_intent,
            &resident_requirements,
            &upload_borrowed,
        )
        .expect("first timing-off resident-volume upload executes");
    assert_eq!(timing_off_first_upload.uploaded_resources(), 8);
    assert_eq!(timing_off_first_upload.payload_upload_bytes(), 8 * MIB);
    assert!(timing_off_first_upload.cpu_timing().is_none());
    assert!(timing_off_first_upload.gpu_timing().is_none());
    let timing_off_second_upload = timing_off_gpu
        .execute_frame(
            timing_off_presentation,
            &catalog,
            &resident_intent,
            &resident_requirements,
            &upload_borrowed,
        )
        .expect("second timing-off resident-volume upload executes");
    assert_eq!(timing_off_second_upload.uploaded_resources(), 1);
    assert_eq!(timing_off_second_upload.payload_upload_bytes(), MIB);
    assert_eq!(
        timing_off_second_upload
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    assert!(timing_off_second_upload.cpu_timing().is_none());
    assert!(timing_off_second_upload.gpu_timing().is_none());

    let adapter = sanitize_evidence_text(gpu.diagnostics().adapter_name());
    let driver = sanitize_evidence_text(gpu.diagnostics().driver());
    if gpu.diagnostics().gpu_timestamps_supported() {
        let mut frame = 501_u64;
        let mut distributions = Vec::new();
        for (width, height) in [(1280_u32, 720_u32), (1920, 1080)] {
            let extent = RenderExtent::new(width, height).expect("timing extent is valid");
            let mut batch_gpu_envelope_ns = Vec::with_capacity(MEASURED_TRIALS);
            let mut render_pass_ns = Vec::with_capacity(MEASURED_TRIALS);
            let mut control_publication_cpu_ns = Vec::with_capacity(MEASURED_TRIALS);
            let mut empty_pass_control = None;
            // The first pass at each extent is an excluded pipeline/target
            // warm-up; the following trials retain identical s0 payloads and
            // camera geometry while advancing only the frame identity.
            for trial in 0..=MEASURED_TRIALS {
                let (intent, requirements) = intent_and_requirements(
                    frame,
                    3,
                    mip_state,
                    volume_view_for([96.0, 96.0, 32.0], 192.0 / f64::from(height), 256.0),
                    extent,
                    &fixtures.upload,
                );
                frame += 1;
                let report = gpu
                    .execute_frame(
                        presentation,
                        &catalog,
                        &intent,
                        &requirements,
                        &upload_borrowed,
                    )
                    .expect("warm resident volume frame executes");
                empty_pass_control = Some((intent.clone(), requirements.clone()));
                assert!(!report.deferred_by_backpressure());
                assert_eq!(report.visited_resources(), fixtures.upload.len());
                assert_eq!(report.uploaded_resources(), 0);
                assert_eq!(report.payload_upload_bytes(), 0);
                assert!(report.control_upload_bytes() > 0);
                let expected_submissions =
                    1 + u32::from(gpu.diagnostics().gpu_payload_copy_timestamps_supported());
                assert_eq!(report.command_buffers(), expected_submissions);
                assert_eq!(report.queue_submissions(), expected_submissions);
                assert_eq!(
                    report
                        .progress()
                        .map(mirante4d_render_api::FrameProgress::completeness),
                    Some(FrameCompleteness::Exact)
                );
                let timing = poll_gpu_timing(
                    &mut gpu,
                    report
                        .gpu_timing()
                        .expect("supported timestamp queries produce a timing ticket"),
                    deadline,
                );
                assert_eq!(timing.target(), presentation);
                assert_eq!(timing.generation(), intent.frame());
                assert_eq!(timing.pass_kind(), RenderPassKind::Volume);
                assert_eq!(timing.payload_copy_ns(), None);
                assert_eq!(
                    timing.batch_gpu_envelope_ns().is_some(),
                    gpu.diagnostics().gpu_payload_copy_timestamps_supported()
                );
                let cpu_timing = report
                    .cpu_timing()
                    .expect("timing-enabled control publication has CPU phase clocks");
                assert!(cpu_timing.control_publication_ns().is_some());
                assert_eq!(cpu_timing.payload_staging_ns(), None);
                if trial != 0 {
                    if let Some(envelope) = timing.batch_gpu_envelope_ns() {
                        batch_gpu_envelope_ns.push(envelope);
                    }
                    render_pass_ns.push(
                        timing
                            .render_pass_ns()
                            .expect("a rendered frame has a render-pass interval"),
                    );
                    control_publication_cpu_ns.push(
                        cpu_timing
                            .control_publication_ns()
                            .expect("this frame published control queue writes"),
                    );
                }
            }
            let (same_intent, same_requirements) =
                empty_pass_control.expect("the warm loop produced an empty-pass control");
            let no_pass = gpu
                .execute_frame(
                    presentation,
                    &catalog,
                    &same_intent,
                    &same_requirements,
                    &upload_borrowed,
                )
                .expect("unchanged resident frame is suppressed");
            assert_eq!(no_pass.command_buffers(), 0);
            assert_eq!(no_pass.queue_submissions(), 0);
            assert_eq!(no_pass.payload_upload_bytes(), 0);
            assert_eq!(no_pass.control_upload_bytes(), 0);
            assert!(no_pass.cpu_timing().is_none());
            assert!(no_pass.gpu_timing().is_none());

            batch_gpu_envelope_ns.sort_unstable();
            render_pass_ns.sort_unstable();
            control_publication_cpu_ns.sort_unstable();
            distributions.push(format!(
                concat!(
                    "{{\"extent\":[{},{}],\"trials\":{},",
                    "\"batch_gpu_envelope_ns\":{:?},",
                    "\"render_pass_ns\":{:?},\"render_p50_ns\":{},\"render_p95_ns\":{},",
                    "\"control_publication_cpu_ns\":{:?}}}"
                ),
                width,
                height,
                MEASURED_TRIALS,
                batch_gpu_envelope_ns,
                render_pass_ns,
                percentile(&render_pass_ns, 0.50),
                percentile(&render_pass_ns, 0.95),
                control_publication_cpu_ns,
            ));
        }

        let adversarial_extent =
            RenderExtent::new(1920, 1080).expect("adversarial timing extent is valid");
        let (adversarial_intent, adversarial_requirements) = intent_and_requirements(
            frame,
            8,
            mip_state,
            volume_view_for([32.0, 32.0, 256.0], 64.0 / 1080.0, 768.0),
            adversarial_extent,
            &fixtures.adversarial,
        );
        frame += 1;
        let adversarial_upload = gpu
            .execute_frame(
                presentation,
                &catalog,
                &adversarial_intent,
                &adversarial_requirements,
                &adversarial_borrowed,
            )
            .expect("adversarial deep volume upload executes");
        assert_eq!(adversarial_upload.uploaded_resources(), 8);
        assert_eq!(adversarial_upload.payload_upload_bytes(), 8 * MIB);
        let _ = poll_gpu_timing(
            &mut gpu,
            adversarial_upload
                .gpu_timing()
                .expect("adversarial upload receives a timing ticket"),
            deadline,
        );
        let mut adversarial_render_pass_ns = Vec::new();
        for trial in 0..=3 {
            let (intent, requirements) = intent_and_requirements(
                frame,
                8,
                mip_state,
                volume_view_for([32.0, 32.0, 256.0], 64.0 / 1080.0, 768.0),
                adversarial_extent,
                &fixtures.adversarial,
            );
            frame += 1;
            let report = gpu
                .execute_frame(
                    presentation,
                    &catalog,
                    &intent,
                    &requirements,
                    &adversarial_borrowed,
                )
                .expect("warm adversarial resident volume frame executes");
            assert_eq!(report.uploaded_resources(), 0);
            assert_eq!(report.payload_upload_bytes(), 0);
            let timing = poll_gpu_timing(
                &mut gpu,
                report
                    .gpu_timing()
                    .expect("adversarial frame receives a timing ticket"),
                deadline,
            );
            assert_eq!(timing.payload_copy_ns(), None);
            if trial != 0 {
                adversarial_render_pass_ns.push(
                    timing
                        .render_pass_ns()
                        .expect("a rendered frame has a render-pass interval"),
                );
            }
        }
        adversarial_render_pass_ns.sort_unstable();
        let adversarial_summary = format!(
            concat!(
                "{{\"extent\":[1920,1080],\"shape_zyx\":[512,64,64],",
                "\"resident_bricks\":8,\"required_samples_per_covered_ray\":512,",
                "\"trials\":3,\"render_pass_ns\":{:?},\"render_p50_ns\":{},",
                "\"render_p95_ns\":{}}}"
            ),
            adversarial_render_pass_ns,
            percentile(&adversarial_render_pass_ns, 0.50),
            percentile(&adversarial_render_pass_ns, 0.95),
        );
        let mut timing_off_execute_ns = Vec::with_capacity(CPU_OVERHEAD_TRIALS);
        let mut timing_on_execute_ns = Vec::with_capacity(CPU_OVERHEAD_TRIALS);
        let mut timing_on_planning_ns = Vec::with_capacity(CPU_OVERHEAD_TRIALS);
        let mut timing_on_queue_submit_ns = Vec::with_capacity(CPU_OVERHEAD_TRIALS);
        for trial in 0..=CPU_OVERHEAD_TRIALS {
            let (timing_off_intent, timing_off_requirements) = intent_and_requirements(
                frame,
                3,
                mip_state,
                volume_view_for([96.0, 96.0, 32.0], 192.0 / 720.0, 256.0),
                base_extent,
                &fixtures.upload,
            );
            let (timing_on_intent, timing_on_requirements) = intent_and_requirements(
                frame,
                3,
                mip_state,
                volume_view_for([96.0, 96.0, 32.0], 192.0 / 720.0, 256.0),
                base_extent,
                &fixtures.upload,
            );
            frame += 1;
            let execute = |runtime: &mut WgpuRenderRuntime,
                           presentation,
                           intent: &RenderIntent,
                           requirements: &RenderRequirements| {
                loop {
                    let started = Instant::now();
                    let report = runtime
                        .execute_frame(
                            presentation,
                            &catalog,
                            intent,
                            requirements,
                            &upload_borrowed,
                        )
                        .expect("warm resident CPU-overhead frame executes");
                    let elapsed_ns =
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    if !report.deferred_by_backpressure() {
                        break (report, elapsed_ns);
                    }
                    assert!(
                        Instant::now() <= deadline,
                        "CPU-overhead backpressure retry exceeded its deadline"
                    );
                    std::thread::yield_now();
                }
            };
            let ((timing_off_report, timing_off_ns), (timing_on_report, timing_on_ns)) =
                if trial % 2 == 0 {
                    (
                        execute(
                            &mut timing_off_gpu,
                            timing_off_presentation,
                            &timing_off_intent,
                            &timing_off_requirements,
                        ),
                        execute(
                            &mut gpu,
                            presentation,
                            &timing_on_intent,
                            &timing_on_requirements,
                        ),
                    )
                } else {
                    let timing_on = execute(
                        &mut gpu,
                        presentation,
                        &timing_on_intent,
                        &timing_on_requirements,
                    );
                    let timing_off = execute(
                        &mut timing_off_gpu,
                        timing_off_presentation,
                        &timing_off_intent,
                        &timing_off_requirements,
                    );
                    (timing_off, timing_on)
                };
            for report in [&timing_off_report, &timing_on_report] {
                assert!(!report.deferred_by_backpressure());
                assert_eq!(report.visited_resources(), fixtures.upload.len());
                assert_eq!(report.uploaded_resources(), 0);
                assert_eq!(report.payload_upload_bytes(), 0);
                assert_eq!(
                    report.progress().map(FrameProgress::completeness),
                    Some(FrameCompleteness::Exact)
                );
            }
            assert_eq!(timing_off_report.command_buffers(), 1);
            assert_eq!(timing_off_report.queue_submissions(), 1);
            let expected_timing_on_submissions =
                1 + u32::from(gpu.diagnostics().gpu_payload_copy_timestamps_supported());
            assert_eq!(
                timing_on_report.command_buffers(),
                expected_timing_on_submissions
            );
            assert_eq!(
                timing_on_report.queue_submissions(),
                expected_timing_on_submissions
            );
            assert_eq!(timing_off_report.frame(), timing_off_intent.frame());
            assert_eq!(timing_on_report.frame(), timing_on_intent.frame());
            assert!(timing_off_report.cpu_timing().is_none());
            assert!(timing_off_report.gpu_timing().is_none());
            let timing_on_cpu = timing_on_report
                .cpu_timing()
                .expect("timing-on report binds CPU timing to the exact frame");
            assert!(timing_on_cpu.control_publication_ns().is_some());
            assert_eq!(timing_on_cpu.payload_staging_ns(), None);
            let timing_on_gpu = timing_on_report
                .gpu_timing()
                .expect("timing-on report binds a GPU timing ticket to the exact frame");
            assert!(
                timing_on_cpu
                    .planning_ns()
                    .saturating_add(timing_on_cpu.queue_submit_ns())
                    <= timing_on_ns,
                "internal CPU phase timings must fit inside the external execute call"
            );
            let completed_gpu = poll_gpu_timing(&mut gpu, timing_on_gpu, deadline);
            assert_eq!(completed_gpu.payload_copy_ns(), None);
            assert!(completed_gpu.render_pass_ns().is_some());
            assert_eq!(
                completed_gpu.batch_gpu_envelope_ns().is_some(),
                gpu.diagnostics().gpu_payload_copy_timestamps_supported()
            );
            if trial != 0 {
                timing_off_execute_ns.push(timing_off_ns);
                timing_on_execute_ns.push(timing_on_ns);
                timing_on_planning_ns.push(timing_on_cpu.planning_ns());
                timing_on_queue_submit_ns.push(timing_on_cpu.queue_submit_ns());
            }
        }
        let paired_delta_ns = timing_on_execute_ns
            .iter()
            .zip(&timing_off_execute_ns)
            .map(|(timing_on, timing_off)| i128::from(*timing_on) - i128::from(*timing_off))
            .collect::<Vec<_>>();
        timing_off_execute_ns.sort_unstable();
        timing_on_execute_ns.sort_unstable();
        timing_on_planning_ns.sort_unstable();
        timing_on_queue_submit_ns.sort_unstable();
        let timing_off_p50_ns = percentile(&timing_off_execute_ns, 0.50);
        let timing_on_p50_ns = percentile(&timing_on_execute_ns, 0.50);
        assert!(timing_off_p50_ns > 0);
        let timing_p50_delta_ns = i128::from(timing_on_p50_ns) - i128::from(timing_off_p50_ns);
        let timing_p50_ratio = timing_on_p50_ns as f64 / timing_off_p50_ns as f64;
        let cpu_overhead_summary = format!(
            concat!(
                "{{\"trials\":{},\"warmups\":1,\"order\":\"alternating\",",
                "\"timing_off_execute_ns\":{:?},\"timing_on_execute_ns\":{:?},",
                "\"paired_on_minus_off_ns\":{:?},",
                "\"timing_off_p50_ns\":{},\"timing_off_p95_ns\":{},",
                "\"timing_on_p50_ns\":{},\"timing_on_p95_ns\":{},",
                "\"p50_delta_ns\":{},\"p50_ratio\":{:.3},",
                "\"timing_on_planning_ns\":{:?},\"timing_on_planning_p50_ns\":{},",
                "\"timing_on_queue_submit_ns\":{:?},",
                "\"timing_on_queue_submit_p50_ns\":{},\"acceptance_threshold\":null}}"
            ),
            CPU_OVERHEAD_TRIALS,
            timing_off_execute_ns,
            timing_on_execute_ns,
            paired_delta_ns,
            timing_off_p50_ns,
            percentile(&timing_off_execute_ns, 0.95),
            timing_on_p50_ns,
            percentile(&timing_on_execute_ns, 0.95),
            timing_p50_delta_ns,
            timing_p50_ratio,
            timing_on_planning_ns,
            percentile(&timing_on_planning_ns, 0.50),
            timing_on_queue_submit_ns,
            percentile(&timing_on_queue_submit_ns, 0.50),
        );
        println!(
            concat!(
                "mirante4d-ep00-resident-volume-gpu-timing-json:",
                "{{\"schema\":\"mirante4d-resident-volume-gpu-timing-v3\",",
                "\"adapter\":\"{}\",\"driver\":\"{}\",\"backend\":\"Vulkan\",",
                "\"scale\":0,\"shape_zyx\":[64,192,192],\"brick_shape\":[64,64,64],",
                "\"resident_bricks\":9,\"sampling\":\"voxel-exact\",\"mode\":\"mip\",",
                "\"gpu_timestamps\":true,\"payload_copy_timestamps\":{},",
                "\"known_copy_timestamp_placement_control\":\"passed\",",
                "\"empty_pass_unavailable_interval_control\":\"passed\",",
                "\"cold_payload_copy_ns\":{:?},\"measurements\":[{}],",
                "\"adversarial_dense\":{},\"cpu_instrumentation_overhead\":{},",
                "\"result\":\"measured\"}}"
            ),
            adapter,
            driver,
            gpu.diagnostics().gpu_payload_copy_timestamps_supported(),
            cold_payload_copy_ns,
            distributions.join(","),
            adversarial_summary,
            cpu_overhead_summary,
        );
        assert_eq!(gpu.diagnostics().validation_error_count(), 0);
        assert_eq!(timing_off_gpu.diagnostics().validation_error_count(), 0);
        assert_eq!(timing_off_gpu.diagnostics().completed_cpu_timings(), 0);
        assert_eq!(gpu.diagnostics().resident_payload_bytes(), 17 * MIB);
        assert_eq!(
            timing_off_gpu.diagnostics().resident_payload_bytes(),
            9 * MIB
        );
        assert_eq!(gpu.diagnostics().render_thread_payload_fact_scan_bytes(), 0);
        assert_eq!(
            gpu.diagnostics().upload_staging_padding_zero_bytes(),
            0,
            "aligned all-valid f32 pages must not be pre-cleared before upload"
        );
    } else {
        println!(
            concat!(
                "mirante4d-ep00-resident-volume-gpu-timing-json:",
                "{{\"schema\":\"mirante4d-resident-volume-gpu-timing-v3\",",
                "\"adapter\":\"{}\",\"driver\":\"{}\",\"backend\":\"Vulkan\",",
                "\"scale\":0,\"resident_bricks\":9,\"gpu_timestamps\":false,",
                "\"result\":\"timestamps-unavailable\"}}"
            ),
            adapter, driver,
        );
    }

    drop(upload_borrowed);
    drop(adversarial_borrowed);
    dataset_runtime
        .request_shutdown()
        .expect("timing fixture runtime begins bounded shutdown");
    drop(upload_leases);
    drop(adversarial_leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "resident volume timing exceeded its 120-second deadline"
    );
}

#[test]
#[ignore = "requires a trusted Vulkan GPU with timestamp writes inside encoders"]
fn queue_write_batch_envelope_gpu_timing_control() {
    const CONTROL_BYTES: u64 = 32 * MIB;
    const WARMUPS: usize = 1;
    const TRIALS: usize = 5;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("trusted workstation exposes a Vulkan adapter");
    let info = adapter.get_info();
    let descriptor = super::renderer_device_descriptor(
        &adapter,
        "mirante4d-queue-write-envelope-control-device",
    )
    .expect("control adapter satisfies product limits");
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .expect("control device creation succeeds");
    if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
        || !device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
    {
        println!(
            "mirante4d-ep00-queue-write-envelope-control-json:{{\"schema\":\"mirante4d-queue-write-envelope-control-v1\",\"adapter\":\"{}\",\"result\":\"timestamps-unavailable\"}}",
            sanitize_evidence_text(&info.name),
        );
        return;
    }

    let control_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-queue-write-envelope-control-target"),
        size: CONTROL_BYTES,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let control_bytes = vec![0x5a_u8; CONTROL_BYTES as usize];
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("mirante4d-queue-write-envelope-control-queries"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-queue-write-envelope-control-resolve"),
        size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-queue-write-envelope-control-readback"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let measure = |publish_control: bool| {
        let mut prelude = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mirante4d-queue-write-envelope-control-prelude"),
        });
        prelude.write_timestamp(&queries, 0);
        queue.submit([prelude.finish()]);
        if publish_control {
            queue.write_buffer(&control_buffer, 0, &control_bytes);
        }
        let mut completion = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mirante4d-queue-write-envelope-control-completion"),
        });
        completion.write_timestamp(&queries, 1);
        completion.resolve_query_set(&queries, 0..2, &resolve, 0);
        completion.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 16);
        let submission = queue.submit([completion.finish()]);
        let mapped = Arc::new(Mutex::new(None));
        let callback = Arc::clone(&mapped);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if let Ok(mut status) = callback.lock() {
                    *status = Some(result.map_err(|_| ()));
                }
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(30)),
            })
            .expect("queue-write envelope control completes");
        assert_eq!(*mapped.lock().expect("mapping state lock"), Some(Ok(())));
        let bytes = readback.slice(..).get_mapped_range();
        let beginning = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let end = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        assert!(end >= beginning);
        let elapsed =
            (((end - beginning) as f64) * f64::from(queue.get_timestamp_period())).round() as u64;
        drop(bytes);
        readback.unmap();
        elapsed
    };

    let mut no_control_ns = Vec::with_capacity(TRIALS);
    let mut queue_write_ns = Vec::with_capacity(TRIALS);
    for trial in 0..WARMUPS + TRIALS {
        let (without, with) = if trial.is_multiple_of(2) {
            (measure(false), measure(true))
        } else {
            let with = measure(true);
            (measure(false), with)
        };
        if trial >= WARMUPS {
            no_control_ns.push(without);
            queue_write_ns.push(with);
        }
    }
    no_control_ns.sort_unstable();
    queue_write_ns.sort_unstable();
    let no_control_p50 = percentile(&no_control_ns, 0.50);
    let queue_write_p50 = percentile(&queue_write_ns, 0.50);
    assert!(
        queue_write_p50 > no_control_p50,
        "a 32-MiB queue write must be visible in the enclosing GPU interval"
    );
    println!(
        concat!(
            "mirante4d-ep00-queue-write-envelope-control-json:",
            "{{\"schema\":\"mirante4d-queue-write-envelope-control-v1\",",
            "\"adapter\":\"{}\",\"control_bytes\":{},\"trials\":{},",
            "\"no_control_ns\":{:?},\"queue_write_ns\":{:?},",
            "\"no_control_p50_ns\":{},\"queue_write_p50_ns\":{},",
            "\"enclosing_interval_detected_queue_write\":true,",
            "\"result\":\"measured\"}}"
        ),
        sanitize_evidence_text(&info.name),
        CONTROL_BYTES,
        TRIALS,
        no_control_ns,
        queue_write_ns,
        no_control_p50,
        queue_write_p50,
    );
}

#[test]
#[ignore = "explicit trusted-GPU diagnostic; compares warm storage-buffer and 3D-texture fetch"]
fn payload_buffer_vs_texture_gpu_timing() {
    const WIDTH: u32 = 1_920;
    const HEIGHT: u32 = 1_080;
    const VOLUME_WIDTH: u32 = 192;
    const VOLUME_HEIGHT: u32 = 192;
    const VOLUME_DEPTH: u32 = 64;
    const WARMUPS: usize = 2;
    const TRIALS: usize = 7;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("trusted workstation exposes a Vulkan adapter");
    let info = adapter.get_info();
    let mut descriptor = super::renderer_device_descriptor(
        &adapter,
        "mirante4d-payload-representation-benchmark-device",
    )
    .expect("benchmark adapter satisfies product limits");
    let filterable_r32 = adapter
        .features()
        .contains(wgpu::Features::FLOAT32_FILTERABLE);
    if filterable_r32 {
        descriptor.required_features |= wgpu::Features::FLOAT32_FILTERABLE;
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .expect("benchmark device creation succeeds");
    if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
        println!(
            "{{\"schema\":\"mirante4d-payload-representation-v1\",\"adapter\":\"{}\",\"gpu_timestamps\":false,\"result\":\"timestamps-unavailable\"}}",
            sanitize_evidence_text(&info.name),
        );
        return;
    }
    if !filterable_r32 {
        println!(
            "{{\"schema\":\"mirante4d-payload-representation-v1\",\"adapter\":\"{}\",\"gpu_timestamps\":true,\"filterable_r32float\":false,\"result\":\"filterable-texture-unavailable\"}}",
            sanitize_evidence_text(&info.name),
        );
        return;
    }

    let semantic_value = |x: u32, y: u32, z: u32| ((x * 7 + y * 3 + z * 13) % 1024) as f32 / 1023.0;
    let sample_count = VOLUME_WIDTH as usize * VOLUME_HEIGHT as usize * VOLUME_DEPTH as usize;
    let mut texture_values = Vec::with_capacity(sample_count);
    for z in 0..VOLUME_DEPTH {
        for y in 0..VOLUME_HEIGHT {
            for x in 0..VOLUME_WIDTH {
                texture_values.push(semantic_value(x, y, z));
            }
        }
    }
    let mut buffer_values = Vec::with_capacity(sample_count);
    for brick_y in 0..3 {
        for brick_x in 0..3 {
            for z in 0..64 {
                for y in 0..64 {
                    for x in 0..64 {
                        buffer_values.push(semantic_value(brick_x * 64 + x, brick_y * 64 + y, z));
                    }
                }
            }
        }
    }
    assert_eq!(buffer_values.len(), texture_values.len());
    let payload_bytes = (sample_count * std::mem::size_of::<f32>()) as u64;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-benchmark-storage-buffer"),
        size: payload_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&buffer_values));
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mirante4d-benchmark-r32float-texture"),
        size: wgpu::Extent3d {
            width: VOLUME_WIDTH,
            height: VOLUME_HEIGHT,
            depth_or_array_layers: VOLUME_DEPTH,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&texture_values),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(VOLUME_WIDTH * 4),
            rows_per_image: Some(VOLUME_HEIGHT),
        },
        wgpu::Extent3d {
            width: VOLUME_WIDTH,
            height: VOLUME_HEIGHT,
            depth_or_array_layers: VOLUME_DEPTH,
        },
    );
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("payload uploads complete");

    let buffer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirante4d-benchmark-buffer-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(payload_bytes),
            },
            count: None,
        }],
    });
    let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirante4d-benchmark-texture-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D3,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let buffer_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mirante4d-benchmark-buffer-bind-group"),
        layout: &buffer_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("mirante4d-benchmark-filtering-sampler"),
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
    });
    let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mirante4d-benchmark-texture-bind-group"),
        layout: &texture_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&texture_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&texture_sampler),
            },
        ],
    });
    let common_shader = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let position = vec2<f32>(
        f32((vertex_index << 1u) & 2u),
        f32(vertex_index & 2u),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(position * 2.0 - 1.0, 0.0, 1.0);
    return output;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let span_x = 192.0 * 1920.0 / 1080.0;
    let x = i32(floor((position.x / 1920.0 - 0.5) * span_x + 96.0));
    let y = i32(floor((0.5 - position.y / 1080.0) * 192.0 + 96.0));
    if x < 0 || x >= 192 || y < 0 || y >= 192 {
        return vec4<f32>(0.0);
    }
    var maximum = 0.0;
    for (var z = 0u; z < 64u; z += 1u) {
        maximum = max(maximum, sample_payload(u32(x), u32(y), z));
    }
    return vec4<f32>(maximum, maximum, maximum, 1.0);
}
"#;
    let buffer_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mirante4d-benchmark-buffer-shader"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}{}",
                r#"
@group(0) @binding(0) var<storage, read> payload: array<f32>;
fn sample_payload(x: u32, y: u32, z: u32) -> f32 {
    let brick_x = x / 64u;
    let brick_y = y / 64u;
    let local_x = x % 64u;
    let local_y = y % 64u;
    let slot = brick_y * 3u + brick_x;
    let index = slot * 262144u + (z * 64u + local_y) * 64u + local_x;
    return payload[index];
}
"#,
                common_shader
            )
            .into(),
        ),
    });
    let texture_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mirante4d-benchmark-texture-shader"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "{}{}",
                r#"
@group(0) @binding(0) var payload: texture_3d<f32>;
@group(0) @binding(1) var payload_sampler: sampler;
fn sample_payload(x: u32, y: u32, z: u32) -> f32 {
    let brick_x = x / 64u;
    let brick_y = y / 64u;
    let local_x = x % 64u;
    let local_y = y % 64u;
    let atlas_x = brick_x * 64u + local_x;
    let atlas_y = brick_y * 64u + local_y;
    let coordinate = (vec3<f32>(f32(atlas_x), f32(atlas_y), f32(z)) + vec3<f32>(0.5))
        / vec3<f32>(192.0, 192.0, 64.0);
    return textureSampleLevel(payload, payload_sampler, coordinate, 0.0).x;
}
"#,
                common_shader
            )
            .into(),
        ),
    });
    let create_pipeline = |label, layout: &wgpu::BindGroupLayout, shader: &wgpu::ShaderModule| {
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    };
    let buffer_pipeline = create_pipeline(
        "mirante4d-benchmark-buffer-pipeline",
        &buffer_layout,
        &buffer_shader,
    );
    let texture_pipeline = create_pipeline(
        "mirante4d-benchmark-texture-pipeline",
        &texture_layout,
        &texture_shader,
    );
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mirante4d-benchmark-output"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("mirante4d-benchmark-timestamps"),
        ty: wgpu::QueryType::Timestamp,
        count: 4,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-benchmark-timestamp-resolve"),
        size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-benchmark-timestamp-readback"),
        size: 32,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut buffer_ns = Vec::with_capacity(TRIALS);
    let mut texture_ns = Vec::with_capacity(TRIALS);
    for trial in 0..WARMUPS + TRIALS {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mirante4d-payload-representation-benchmark"),
        });
        if trial.is_multiple_of(2) {
            encode_payload_benchmark_pass(
                &mut encoder,
                &buffer_pipeline,
                &buffer_bind_group,
                &target_view,
                &queries,
                0,
            );
            encode_payload_benchmark_pass(
                &mut encoder,
                &texture_pipeline,
                &texture_bind_group,
                &target_view,
                &queries,
                2,
            );
        } else {
            encode_payload_benchmark_pass(
                &mut encoder,
                &texture_pipeline,
                &texture_bind_group,
                &target_view,
                &queries,
                2,
            );
            encode_payload_benchmark_pass(
                &mut encoder,
                &buffer_pipeline,
                &buffer_bind_group,
                &target_view,
                &queries,
                0,
            );
        }
        encoder.resolve_query_set(&queries, 0..4, &resolve, 0);
        encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 32);
        let submission = queue.submit([encoder.finish()]);
        let mapped = Arc::new(Mutex::new(None));
        let callback = Arc::clone(&mapped);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                if let Ok(mut status) = callback.lock() {
                    *status = Some(result.map_err(|_| ()));
                }
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(30)),
            })
            .expect("representation benchmark completes");
        assert_eq!(*mapped.lock().expect("mapping state lock"), Some(Ok(())));
        let bytes = readback.slice(..).get_mapped_range();
        let timestamp = |word: usize| {
            u64::from_le_bytes(
                bytes[word * 8..word * 8 + 8]
                    .try_into()
                    .expect("timestamp word is eight bytes"),
            )
        };
        let period = f64::from(queue.get_timestamp_period());
        let elapsed = |start: usize| {
            (((timestamp(start + 1) - timestamp(start)) as f64) * period).round() as u64
        };
        if trial >= WARMUPS {
            buffer_ns.push(elapsed(0));
            texture_ns.push(elapsed(2));
        }
        drop(bytes);
        readback.unmap();
    }
    buffer_ns.sort_unstable();
    texture_ns.sort_unstable();
    let buffer_p50 = percentile(&buffer_ns, 0.50);
    let texture_p50 = percentile(&texture_ns, 0.50);
    println!(
        concat!(
            "{{\"schema\":\"mirante4d-payload-representation-v1\",",
            "\"adapter\":\"{}\",\"driver\":\"{}\",\"backend\":\"Vulkan\",",
            "\"extent\":[1920,1080],\"volume_shape_zyx\":[64,192,192],",
            "\"samples_per_covered_ray\":64,\"trials\":7,\"warmups\":2,",
            "\"storage_buffer_ns\":{:?},\"storage_buffer_p50_ns\":{},",
            "\"filterable_r32float_texture_ns\":{:?},",
            "\"filterable_r32float_texture_p50_ns\":{},",
            "\"buffer_over_texture_p50_ratio\":{:.3},",
            "\"scope\":\"f32-all-valid-voxel-exact-fetch-only\",\"result\":\"measured\"}}"
        ),
        sanitize_evidence_text(&info.name),
        sanitize_evidence_text(&info.driver_info),
        buffer_ns,
        buffer_p50,
        texture_ns,
        texture_p50,
        buffer_p50 as f64 / texture_p50 as f64,
    );
}
