#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use mirante4d_dataset::{
    BrickKey, ContentAddressStatus, DatasetCatalog, DatasetLayer, DatasetResourceIdentity,
    DatasetScale, DatasetSource, DatasetSourceFault, DatasetSourceId, ReservedDecodeSink,
    ResourceLease, ResourceRegion, ResourceValidity,
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
    CameraFrame, FrameCompleteness, FrameIdentity, FrameProgress, LayerRenderIntent,
    PresentationTarget, PresentationViewport, PresentedFrame, RenderExtent, RenderIntent,
    RenderPassKind, RenderRequirement, RenderRequirementRole, RenderRequirements,
    RenderResourceGrid, RenderResourceGridCatalog, RenderViewIntent, VolumePickCompleteness,
    VolumePickHitKind, VolumePickPolicy, VolumePickQuery, VolumePickResult, VolumePickTicket,
    VolumePickValue,
};
use mirante4d_render_reference::{
    NumericalColorFacts, NumericalConformanceContract, NumericalConformanceOracle,
    NumericalDvrParameters, NumericalIsoParameters, NumericalIsoShading, NumericalPickCompleteness,
    NumericalPickFacts, NumericalPickKind, NumericalSampling, NumericalTransfer, NumericalVolume,
    NumericalVolumeFacts, NumericalVolumeMode, NumericalVolumeQuery, NumericalVoxel,
    NumericalWorldRay, PortableDirectionComponent, ReferenceFrame, ReferenceRenderer,
    classify_portable_direction, page_exit_distance_reference, segment_end_index_reference,
};

use super::{
    CoordinatedPublicationGroup, CoordinatedTargetLayout, CoordinatedTargetRequest,
    CoordinatedValidationCaptureTicket, GpuFrameTiming, GpuTimingTicket, PipelineCapability,
    PipelineReadiness, RendererEvent, RendererEventSink, RetainedFrameRenderPolicy,
    ValidationCapture, VolumeColorSchedule, WgpuRenderRuntime, WgpuRenderRuntimeConfig,
    WgpuRenderRuntimeDiagnostics, WgpuRenderRuntimeError,
    global_residency::{compact_cell_keys, directory_hash},
    runtime::GLOBAL_DIRECTORY_SLOTS,
};

const MIB: u64 = 1024 * 1024;
const FOUR_TARGET_GPU_BYTES: u64 = 4 * 1024 * MIB;
const PERFORMANCE_GPU_BYTES: u64 = 128 * MIB;
// The configurable 11-MiB fixture budget is unchanged; the runtime now
// accounts its fixed 52-MiB global directory/page-record reservation honestly.
const SMALL_GPU_BYTES: u64 = 63 * MIB;
const SOURCE_ID: DatasetSourceId = DatasetSourceId::new(0x5750_3039_4100_0001);
const REQUEST_SCOPE: u64 = 0x5750_3039_4100_0002;
const SEMANTIC_FIXTURE_LABEL: &str = "semantic-small";
const UPLOAD_FIXTURE_LABEL: &str = "upload-boundary";
const WORK_FIXTURE_LABEL: &str = "work-boundary";
const HIGH_COUNT_WORK_RESOURCES: usize = 32_768;
const DEFAULT_TRUSTED_GPU_ADAPTER: &str = "NVIDIA GeForce RTX 3070 Ti Laptop GPU";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestPipelineAdmission {
    Initializing,
    Ready,
}

fn test_gpu_runtime(
    config: WgpuRenderRuntimeConfig,
    payload_segment_limit: Option<u64>,
    admission: TestPipelineAdmission,
) -> WgpuRenderRuntime {
    test_gpu_runtime_with_initial_commitment(config, payload_segment_limit, None, admission)
}

fn test_gpu_runtime_with_initial_commitment(
    config: WgpuRenderRuntimeConfig,
    payload_segment_limit: Option<u64>,
    initial_commitment: Option<u64>,
    admission: TestPipelineAdmission,
) -> WgpuRenderRuntime {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("the trusted workstation exposes a Vulkan adapter");
    let adapter_info = adapter.get_info();
    let expected_adapter = std::env::var("MIRANTE4D_TRUSTED_GPU_ADAPTER_NAME")
        .unwrap_or_else(|_| DEFAULT_TRUSTED_GPU_ADAPTER.to_owned());
    assert_eq!(adapter_info.backend, wgpu::Backend::Vulkan);
    assert_eq!(
        adapter_info.name, expected_adapter,
        "trusted GPU tests selected an unqualified adapter"
    );
    let descriptor = super::renderer_device_descriptor(&adapter, "mirante4d-render-test-device")
        .expect("the trusted adapter satisfies renderer limits");
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
        .expect("trusted renderer-test device creation succeeds");
    let mut gpu = match (payload_segment_limit, initial_commitment) {
        (Some(segment_limit), Some(initial_commitment)) => {
            WgpuRenderRuntime::from_existing_device_with_payload_test_limits(
                &adapter,
                device,
                queue,
                config,
                segment_limit,
                initial_commitment,
            )
        }
        (None, None) => WgpuRenderRuntime::from_existing_device(&adapter, device, queue, config),
        (Some(segment_limit), None) => {
            WgpuRenderRuntime::from_existing_device_with_payload_segment_limit(
                &adapter,
                device,
                queue,
                config,
                segment_limit,
            )
        }
        (None, Some(_)) => panic!("an initial commitment override requires a segment limit"),
    }
    .expect("fixed resources and the bounded compiler worker initialize");
    if admission == TestPipelineAdmission::Ready {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match gpu
                .poll_pipeline_readiness()
                .expect("renderer-test pipeline compilation succeeds")
            {
                PipelineReadiness::Ready => break,
                PipelineReadiness::CompilingInitial | PipelineReadiness::InitialRenderReady => {
                    assert!(
                        Instant::now() < deadline,
                        "renderer-test pipeline compilation timed out"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    gpu
}

#[derive(Clone)]
struct PayloadBytes {
    values: Arc<[u8]>,
    validity: Option<Arc<[u8]>>,
}

struct FixtureSource {
    catalog: Arc<DatasetCatalog>,
    payloads: BTreeMap<BrickKey, PayloadBytes>,
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

struct GpuFixtures {
    source: Arc<FixtureSource>,
    resource_grids: RenderResourceGridCatalog,
    semantic: [Vec<BrickKey>; 3],
    missing_u8: BrickKey,
    upload: Vec<BrickKey>,
    work: Vec<BrickKey>,
    multichannel: [BrickKey; 3],
    /// Eight co-registered 64-cubed channels used only by the ignored fixed-
    /// LOD multichannel timing matrix.
    performance_channels: [BrickKey; 8],
    affine: BrickKey,
    /// Fine-left, fine-right, and complete coarse floor.
    multiscale_plane: [BrickKey; 3],
    /// One complete 64-cubed terminal volume covered by one canonical page.
    terminal: BrickKey,
    /// The same terminal samples covered by eight ordinary sparse pages.
    terminal_sparse: Vec<BrickKey>,
    /// The same samples on a translated affine, used to force general DVR.
    terminal_shifted: BrickKey,
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

fn resource_key(layer: u32, origin: [u64; 3], shape: [u64; 3]) -> BrickKey {
    resource_key_at_scale(layer, ScaleLevel::BASE, origin, shape)
}

fn resource_key_at_scale(
    layer: u32,
    scale: ScaleLevel,
    origin: [u64; 3],
    shape: [u64; 3],
) -> BrickKey {
    BrickKey::new(
        DatasetResourceIdentity::SessionLocal(SOURCE_ID),
        LogicalLayerKey::new(layer),
        TimeIndex::new(0),
        scale,
        ResourceRegion::new(
            origin,
            Shape3D::new(shape[0], shape[1], shape[2]).expect("fixture shape is valid"),
        )
        .expect("fixture region is valid"),
    )
}

fn fixture_resource_grids(catalog: &DatasetCatalog) -> RenderResourceGridCatalog {
    let grids = catalog
        .layers()
        .flat_map(|layer| {
            layer.scales().map(move |scale| {
                let cell_side = match layer.key().ordinal() {
                    0..=2 => 16,
                    3 => 64,
                    4 => 1,
                    5..=7 => 8,
                    11 => 8,
                    12 => 2,
                    13 | 15..=23 => 64,
                    14 => 32,
                    ordinal => panic!("fixture layer {ordinal} has no declared resource grid"),
                };
                let volume_shape = scale.shape();
                let cell_shape = Shape3D::new(
                    volume_shape.z().min(cell_side),
                    volume_shape.y().min(cell_side),
                    volume_shape.x().min(cell_side),
                )
                .expect("a fixture cell clipped to its catalog volume is nonempty");
                RenderResourceGrid::new(layer.key(), scale.level(), volume_shape, cell_shape)
            })
        })
        .collect();
    RenderResourceGridCatalog::new(catalog, grids)
        .expect("every fixture layer and scale has one stable logical resource grid")
}

fn activate_fixture_dataset(
    gpu: &mut WgpuRenderRuntime,
    fixtures: &GpuFixtures,
    catalog: &DatasetCatalog,
) {
    gpu.activate_dataset_generation_with_resource_grids(catalog, &fixtures.resource_grids)
        .expect("the fixture resource-grid catalog activates before GPU work");
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

fn build_fixtures() -> GpuFixtures {
    let catalog = Arc::new(
        DatasetCatalog::new(
            "renderer GPU fixtures",
            ContentAddressStatus::SessionLocal(SOURCE_ID),
            vec![
                layer(
                    0,
                    SEMANTIC_FIXTURE_LABEL,
                    [32, 32, 32],
                    IntensityDType::Uint8,
                    ResourceValidity::BitMask,
                ),
                layer(
                    1,
                    SEMANTIC_FIXTURE_LABEL,
                    [32, 32, 32],
                    IntensityDType::Uint16,
                    ResourceValidity::BitMask,
                ),
                layer(
                    2,
                    SEMANTIC_FIXTURE_LABEL,
                    [32, 32, 32],
                    IntensityDType::Float32,
                    ResourceValidity::BitMask,
                ),
                layer(
                    3,
                    UPLOAD_FIXTURE_LABEL,
                    [64, 192, 192],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    4,
                    WORK_FIXTURE_LABEL,
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
                DatasetLayer::new_multiscale(
                    LogicalLayerKey::new(12),
                    "progressive-plane",
                    1,
                    IntensityDType::Uint8,
                    vec![
                        DatasetScale::new(
                            ScaleLevel::BASE,
                            Shape3D::new(2, 2, 4).unwrap(),
                            GridToWorld::identity(),
                            ResourceValidity::BitMask,
                        ),
                        DatasetScale::new(
                            ScaleLevel::new(1),
                            Shape3D::new(1, 1, 2).unwrap(),
                            GridToWorld::scale(2.0, 2.0, 2.0).unwrap(),
                            ResourceValidity::AllValid,
                        ),
                    ],
                )
                .expect("the progressive plane fixture pyramid is valid"),
                layer(
                    13,
                    "terminal-full-volume",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    14,
                    "terminal-sparse-volume",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    15,
                    "performance-channel-1",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    16,
                    "performance-channel-2",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    17,
                    "performance-channel-3",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    18,
                    "performance-channel-4",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    19,
                    "performance-channel-5",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    20,
                    "performance-channel-6",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    21,
                    "performance-channel-7",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer(
                    22,
                    "performance-channel-8",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                ),
                layer_with_transform(
                    23,
                    "terminal-shifted-volume",
                    [64, 64, 64],
                    IntensityDType::Float32,
                    ResourceValidity::AllValid,
                    GridToWorld::from_row_major([
                        1.0, 0.0, 0.0, 0.25, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
                        1.0,
                    ])
                    .expect("the translated terminal transform is valid"),
                ),
            ],
        )
        .expect("fixture catalog is valid"),
    );
    let resource_grids = fixture_resource_grids(&catalog);
    let mut payloads = BTreeMap::new();
    let mut semantic: [Vec<BrickKey>; 3] = std::array::from_fn(|_| Vec::new());
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

    let fine_left = resource_key_at_scale(12, ScaleLevel::BASE, [0, 0, 0], [2, 2, 2]);
    let fine_right = resource_key_at_scale(12, ScaleLevel::BASE, [0, 0, 2], [2, 2, 2]);
    let coarse = resource_key_at_scale(12, ScaleLevel::new(1), [0, 0, 0], [1, 1, 2]);
    let mut fine_validity = [0xff_u8];
    set_valid(&mut fine_validity, 0, false);
    assert!(
        payloads
            .insert(
                fine_left,
                PayloadBytes {
                    values: Arc::from([200_u8; 8]),
                    validity: Some(Arc::from(fine_validity)),
                },
            )
            .is_none()
    );
    assert!(
        payloads
            .insert(
                fine_right,
                PayloadBytes {
                    values: Arc::from([200_u8; 8]),
                    validity: Some(Arc::from([0xff_u8])),
                },
            )
            .is_none()
    );
    assert!(
        payloads
            .insert(
                coarse,
                PayloadBytes {
                    values: Arc::from([40_u8; 2]),
                    validity: None,
                },
            )
            .is_none()
    );

    let terminal_value = |z: u64, y: u64, x: u64| (x + y + z) as f32 / 189.0;
    let terminal = resource_key(13, [0, 0, 0], [64, 64, 64]);
    let mut terminal_values = Vec::with_capacity(64 * 64 * 64 * 4);
    for z in 0..64 {
        for y in 0..64 {
            for x in 0..64 {
                terminal_values.extend_from_slice(&terminal_value(z, y, x).to_le_bytes());
            }
        }
    }
    assert_eq!(terminal_values.len(), MIB as usize);
    let terminal_payload: Arc<[u8]> = terminal_values.into();
    assert!(
        payloads
            .insert(
                terminal,
                PayloadBytes {
                    values: Arc::clone(&terminal_payload),
                    validity: None,
                },
            )
            .is_none()
    );
    let terminal_shifted = resource_key(23, [0, 0, 0], [64, 64, 64]);
    assert!(
        payloads
            .insert(
                terminal_shifted,
                PayloadBytes {
                    values: terminal_payload,
                    validity: None,
                },
            )
            .is_none()
    );

    let mut terminal_sparse = Vec::new();
    for origin_z in [0_u64, 32] {
        for origin_y in [0_u64, 32] {
            for origin_x in [0_u64, 32] {
                let key = resource_key(14, [origin_z, origin_y, origin_x], [32, 32, 32]);
                let mut values = Vec::with_capacity(32 * 32 * 32 * 4);
                for local_z in 0..32 {
                    for local_y in 0..32 {
                        for local_x in 0..32 {
                            values.extend_from_slice(
                                &terminal_value(
                                    origin_z + local_z,
                                    origin_y + local_y,
                                    origin_x + local_x,
                                )
                                .to_le_bytes(),
                            );
                        }
                    }
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
                terminal_sparse.push(key);
            }
        }
    }
    assert_eq!(terminal_sparse.len(), 8);

    let performance_channels = std::array::from_fn(|index| {
        let key = resource_key(15 + index as u32, [0, 0, 0], [64, 64, 64]);
        let mut values = Vec::with_capacity(MIB as usize);
        for z in 0_u64..64 {
            for y in 0_u64..64 {
                for x in 0_u64..64 {
                    let spatial = (x + y + z) as f32 / 189.0;
                    let value = (spatial * (0.65 + index as f32 * 0.025)).min(1.0);
                    values.extend_from_slice(&value.to_le_bytes());
                }
            }
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
        key
    });

    let source = Arc::new(FixtureSource { catalog, payloads });
    GpuFixtures {
        source,
        resource_grids,
        semantic,
        missing_u8,
        upload,
        work,
        multichannel,
        performance_channels,
        affine,
        multiscale_plane: [fine_left, fine_right, coarse],
        terminal,
        terminal_sparse,
        terminal_shifted,
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
    keys: &[BrickKey],
    generation: CancellationGeneration,
    deadline: Instant,
) -> BTreeMap<BrickKey, AccountedResourceLease> {
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
    keys: &[BrickKey],
) -> (RenderIntent, RenderRequirements) {
    let presentation = PresentationViewport::new(
        f64::from(extent.width_pixels()),
        f64::from(extent.height_pixels()),
    )
    .expect("fixture presentation is valid");
    let transfer = match layer {
        0 | 4 => transfer(0.0, 255.0),
        1 => transfer(0.0, 65_535.0),
        2 | 3 | 8 | 9 | 10 | 11 | 13 | 14 => transfer(0.0, 1.0),
        12 => transfer(0.0, 255.0),
        _ => panic!("unknown renderer fixture layer"),
    };
    let intent = RenderIntent::new(
        FrameIdentity::new(frame),
        DatasetResourceIdentity::SessionLocal(SOURCE_ID),
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
    keys: &[BrickKey],
) -> (RenderIntent, RenderRequirements) {
    let presentation = PresentationViewport::new(
        f64::from(extent.width_pixels()),
        f64::from(extent.height_pixels()),
    )
    .expect("fixture presentation is valid");
    let intent = RenderIntent::new(
        FrameIdentity::new(frame),
        DatasetResourceIdentity::SessionLocal(SOURCE_ID),
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

fn owned_leases(
    keys: &[BrickKey],
    leases: &BTreeMap<BrickKey, AccountedResourceLease>,
    omit: Option<BrickKey>,
) -> Vec<Arc<dyn ResourceLease>> {
    keys.iter()
        .filter(|key| Some(**key) != omit)
        .map(|key| {
            Arc::new(leases.get(key).expect("fixture lease exists").clone())
                as Arc<dyn ResourceLease>
        })
        .collect()
}

fn poll_coordinated_capture(
    runtime: &mut WgpuRenderRuntime,
    ticket: CoordinatedValidationCaptureTicket,
    deadline: Instant,
) -> ValidationCapture {
    loop {
        assert!(
            Instant::now() < deadline,
            "asynchronous coordinated GPU readback exceeded its deadline"
        );
        match runtime
            .poll_coordinated_validation_capture(ticket)
            .expect("coordinated validation capture polling succeeds")
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
    if max_delta > 1
        && let Some((byte, _)) = capture
            .rgba8()
            .iter()
            .zip(reference.rgba8())
            .enumerate()
            .find(|(_, (actual, expected))| actual.abs_diff(**expected) > 1)
    {
        let pixel = byte / 4;
        let start = pixel * 4;
        eprintln!(
            "GPU/reference first color mismatch pixel={} actual={:?} expected={:?} coverage={}/{} validity={}/{}",
            pixel,
            &capture.rgba8()[start..start + 4],
            &reference.rgba8()[start..start + 4],
            capture.coverage()[pixel],
            reference.coverage()[pixel],
            capture.validity()[pixel],
            reference.validity()[pixel],
        );
    }
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

fn assert_pixel_matches_reference(
    label: &str,
    capture: &ValidationCapture,
    reference: &ReferenceFrame,
    x: u32,
    y: u32,
) -> u8 {
    let (actual_rgba8, actual_coverage, actual_validity) = pixel(capture, x, y);
    let expected_rgba8 = reference
        .rgba8_pixel(x, y)
        .expect("the reference pixel lies inside the shared extent");
    assert_eq!(
        actual_coverage,
        u8::from(
            reference
                .pixel_is_covered(x, y)
                .expect("the reference coverage pixel lies inside the shared extent")
        ),
        "{label} coverage differs from the independent CPU reference"
    );
    assert_eq!(
        actual_validity,
        u8::from(
            reference
                .pixel_is_valid(x, y)
                .expect("the reference validity pixel lies inside the shared extent")
        ),
        "{label} validity differs from the independent CPU reference"
    );
    let max_delta = actual_rgba8
        .into_iter()
        .zip(expected_rgba8)
        .map(|(actual, expected)| actual.abs_diff(expected))
        .max()
        .unwrap_or(0);
    assert!(
        max_delta <= 1,
        "{label} RGBA8 differs from the independent CPU reference: actual={actual_rgba8:?}, expected={expected_rgba8:?}, delta={max_delta}"
    );
    max_delta
}

fn numerical_semantic_f32_volume() -> NumericalVolume {
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

fn numerical_primary_perspective_view() -> RenderViewIntent {
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

fn numerical_primary_perspective_ray() -> NumericalWorldRay {
    // For the 65x65 viewport, render pixel (48, 32) is exactly 16 screen
    // points to the right of center. With a 32-point focal length the
    // independently reconstructed direction is therefore (0.5, 0, -1)
    // because the canonical identity camera looks down world Z.
    NumericalWorldRay::new([-4.5, 15.25, 55.5], [0.5, 0.0, -1.0])
        .expect("the independent EP-00 perspective ray is finite")
}

fn numerical_primary_sample_step_world() -> f64 {
    // Identity grid, unit world ray (1/sqrt(5), 0, 2/sqrt(5)): advance one
    // voxel along its dominant grid axis while retaining physical distance.
    5.0_f64.sqrt() / 2.0
}

fn numerical_transfer(window: [f64; 2], color: [f64; 3], opacity: f64) -> NumericalTransfer {
    NumericalTransfer::new(window, 1.0, false, color, opacity)
        .expect("the EP-00 numerical transfer is valid")
}

fn assert_numerical_color(
    label: &str,
    capture: &ValidationCapture,
    pixel_xy: [u32; 2],
    expected: NumericalColorFacts,
) {
    let contract = NumericalConformanceContract::independent();
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
    let contract = NumericalConformanceContract::independent();
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

fn numerical_volume_facts(
    volume: &NumericalVolume,
    transfer: NumericalTransfer,
    mode: NumericalVolumeMode,
) -> NumericalVolumeFacts {
    NumericalConformanceOracle::new()
        .volume(
            volume,
            NumericalVolumeQuery::new(
                numerical_primary_perspective_ray(),
                NumericalSampling::VoxelExact,
                transfer,
                mode,
                numerical_primary_sample_step_world(),
            )
            .expect("the independent EP-00 volume query is valid"),
        )
        .expect("the independent EP-00 volume facts are bounded")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraversalKernel {
    Mip,
    Dvr,
    GeneralDvr,
    Iso,
    Mixed,
}

impl TraversalKernel {
    const fn label(self) -> &'static str {
        match self {
            Self::Mip => "MIP",
            Self::Dvr => "DVR",
            Self::GeneralDvr => "general DVR",
            Self::Iso => "ISO",
            Self::Mixed => "Mixed",
        }
    }
}

fn traversal_orientation(direction_x: f64) -> UnitQuaternion {
    assert!(direction_x.is_finite() && direction_x.abs() < 1.0);
    // Rotating canonical -Z about +Y by `angle` produces forward X
    // `-sin(angle)`. Build from the desired component so the center ray is an
    // independently inspectable exact fixture rather than a screen-offset
    // approximation.
    let angle = -direction_x.asin();
    UnitQuaternion::new_xyzw(0.0, (0.5 * angle).sin(), 0.0, (0.5 * angle).cos())
        .expect("the traversal camera orientation is finite and nonzero")
}

fn traversal_volume_view(direction_x: f64, target_x: f64) -> RenderViewIntent {
    RenderViewIntent::volume(
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(target_x, 31.5, 31.5).expect("the traversal target is finite"),
            traversal_orientation(direction_x),
            1.0,
            1.0,
            96.0,
        )
        .expect("the traversal camera is valid"),
        IsoLightState::attached_camera(),
    )
}

fn traversal_layer_transfer_with_opacity(
    kernel: TraversalKernel,
    color: [f32; 3],
    opacity: f32,
) -> LayerTransfer {
    LayerTransfer::new(
        DisplayWindow::new(0.0, 1.0).expect("the traversal window is valid"),
        RgbColor::new(color).expect("the traversal color is valid"),
        Opacity::new(opacity).expect("the traversal opacity is valid"),
        TransferCurve::linear(),
        kernel == TraversalKernel::Iso,
    )
}

fn traversal_layer_transfer(kernel: TraversalKernel, color: [f32; 3]) -> LayerTransfer {
    traversal_layer_transfer_with_opacity(kernel, color, 1.0)
}

fn traversal_state(
    kernel: TraversalKernel,
    sampling: SamplingPolicy,
) -> mirante4d_domain::RenderState {
    match kernel {
        TraversalKernel::Mip | TraversalKernel::Mixed => {
            mirante4d_domain::RenderState::mip(sampling)
        }
        TraversalKernel::Dvr | TraversalKernel::GeneralDvr => mirante4d_domain::RenderState::dvr(
            sampling,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 1.0).expect("the traversal DVR window is valid"),
                TransferCurve::linear(),
            ),
            0.001,
        )
        .expect("the traversal DVR state is valid"),
        TraversalKernel::Iso => {
            mirante4d_domain::RenderState::iso(sampling, IsoShadingPolicy::Flat, 0.55)
                .expect("the traversal ISO state is valid")
        }
    }
}

fn traversal_numerical_contract(
    kernel: TraversalKernel,
) -> (NumericalTransfer, NumericalVolumeMode, VolumePickPolicy) {
    let inverted = kernel == TraversalKernel::Iso;
    let transfer = NumericalTransfer::new([0.0, 1.0], 1.0, inverted, [1.0, 0.0, 0.0], 1.0)
        .expect("the independent traversal transfer is valid");
    let (mode, policy) = match kernel {
        TraversalKernel::Mip | TraversalKernel::Mixed => {
            (NumericalVolumeMode::Mip, VolumePickPolicy::MipArgmax)
        }
        TraversalKernel::Dvr | TraversalKernel::GeneralDvr => (
            NumericalVolumeMode::Dvr(
                NumericalDvrParameters::new([0.0, 1.0], 1.0, 0.001)
                    .expect("the independent traversal DVR parameters are valid"),
            ),
            VolumePickPolicy::MaximumOpacityContribution,
        ),
        TraversalKernel::Iso => (
            NumericalVolumeMode::Iso(
                NumericalIsoParameters::new(0.55, NumericalIsoShading::Flat)
                    .expect("the independent traversal ISO parameters are valid"),
            ),
            VolumePickPolicy::FirstThresholdHit,
        ),
    };
    (transfer, mode, policy)
}

fn traversal_intent_and_requirements(
    frame: u64,
    kernel: TraversalKernel,
    sampling: SamplingPolicy,
    view: RenderViewIntent,
    extent: RenderExtent,
    fixtures: &GpuFixtures,
) -> (RenderIntent, RenderRequirements) {
    let mut keys = fixtures.terminal_sparse.clone();
    let mut layers = vec![LayerRenderIntent::new(
        LogicalLayerKey::new(14),
        traversal_layer_transfer(kernel, [1.0, 0.0, 0.0]),
        traversal_state(kernel, sampling),
    )];
    if kernel == TraversalKernel::Mixed {
        keys.push(fixtures.terminal);
        layers.push(LayerRenderIntent::new(
            LogicalLayerKey::new(13),
            traversal_layer_transfer(TraversalKernel::Iso, [0.0, 1.0, 0.0]),
            traversal_state(TraversalKernel::Iso, sampling),
        ));
    } else if kernel == TraversalKernel::GeneralDvr {
        keys.push(fixtures.terminal_shifted);
        layers.push(LayerRenderIntent::new(
            LogicalLayerKey::new(23),
            traversal_layer_transfer_with_opacity(kernel, [0.0, 1.0, 0.0], 0.0),
            traversal_state(kernel, sampling),
        ));
    }
    multichannel_intent_and_requirements(frame, view, extent, layers, &keys)
}

fn traversal_numerical_volume() -> NumericalVolume {
    let mut voxels = Vec::with_capacity(64 * 64 * 64);
    for z in 0_u32..64 {
        for y in 0_u32..64 {
            for x in 0_u32..64 {
                let encoded = (x + y + z) as f32 / 189.0_f32;
                voxels.push(NumericalVoxel::Valid(f64::from(encoded)));
            }
        }
    }
    NumericalVolume::new([64, 64, 64], GridToWorld::identity(), voxels)
        .expect("the independent traversal volume is valid")
}

fn traversal_numerical_facts(
    volume: &NumericalVolume,
    intent: &RenderIntent,
    kernel: TraversalKernel,
    sampling: SamplingPolicy,
) -> NumericalVolumeFacts {
    let RenderViewIntent::Volume { camera, .. } = intent.view() else {
        panic!("a traversal fixture always uses a volume camera")
    };
    let ray = CameraFrame::new(camera, intent.presentation())
        .and_then(|frame| {
            frame.ray_for_render_pixel(
                0.0,
                0.0,
                intent.extent().width_pixels(),
                intent.extent().height_pixels(),
            )
        })
        .expect("the traversal center ray is finite");
    let ray = NumericalWorldRay::new(ray.origin().components(), ray.direction())
        .expect("the independent traversal ray is valid");
    let portable_direction = ray.direction().map(|component| {
        classify_portable_direction(component)
            .expect("the traversal direction is finite")
            .value()
    });
    let grid_speed = portable_direction
        .into_iter()
        .map(f32::abs)
        .fold(0.0_f32, f32::max);
    let (transfer, mode, _) = traversal_numerical_contract(kernel);
    NumericalConformanceOracle::new()
        .volume(
            volume,
            NumericalVolumeQuery::new(
                ray,
                match sampling {
                    SamplingPolicy::VoxelExact => NumericalSampling::VoxelExact,
                    SamplingPolicy::SmoothLinear => NumericalSampling::SmoothLinear,
                },
                transfer,
                mode,
                f64::from(grid_speed).recip(),
            )
            .expect("the independent traversal query is valid"),
        )
        .expect("the independent traversal facts are bounded")
}

fn execute_traversal_case(
    gpu: &mut WgpuRenderRuntime,
    catalog: &DatasetCatalog,
    leases: &[Arc<dyn ResourceLease>],
    numerical_volume: &NumericalVolume,
    frame: u64,
    kernel: TraversalKernel,
    sampling: SamplingPolicy,
    direction_x: f64,
    target_x: f64,
    fixtures: &GpuFixtures,
    deadline: Instant,
) {
    let target = PresentationTarget::ThreeD;
    let extent = RenderExtent::new(1, 1).expect("the traversal extent is valid");
    let (intent, requirements) = traversal_intent_and_requirements(
        frame,
        kernel,
        sampling,
        traversal_volume_view(direction_x, target_x),
        extent,
        fixtures,
    );
    gpu.offer_residency_leases(leases)
        .expect("the traversal leases are offered");
    let report = execute_coordinated_target(
        gpu,
        target,
        catalog,
        &intent,
        &requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let target_report = report
        .target(target)
        .expect("the traversal target has one report");
    assert_eq!(
        target_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let presented = presented_frame(target, &intent, target_report);
    let capture = poll_coordinated_capture(
        gpu,
        target_report
            .validation_capture()
            .expect("the traversal case owns one validation capture"),
        deadline,
    );
    let lease_refs = leases.iter().map(Arc::as_ref).collect::<Vec<_>>();
    let reference = ReferenceRenderer::new()
        .render(catalog, &intent, &lease_refs)
        .expect("the independent reference renders the traversal case");
    assert!(
        compare_reference(&capture, &reference) <= 1,
        "{} {sampling:?} traversal color differed from the independent reference",
        kernel.label(),
    );

    let expected = traversal_numerical_facts(numerical_volume, &intent, kernel, sampling);
    if kernel != TraversalKernel::Mixed {
        assert_numerical_color(
            &format!("{} {sampling:?} traversal", kernel.label()),
            &capture,
            [0, 0],
            expected.color(),
        );
    }
    let (_, _, policy) = traversal_numerical_contract(kernel);
    let query = VolumePickQuery::new(
        &presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(14),
        [0.0, 0.0],
        policy,
    )
    .expect("the traversal pick query is valid");
    let ticket = gpu
        .request_coordinated_pick(target, query)
        .expect("the traversal pick is accepted");
    assert_numerical_pick(
        &format!("{} {sampling:?} traversal", kernel.label()),
        poll_pick(gpu, ticket, deadline),
        expected
            .pick()
            .expect("the traversal fixture has one independent pick"),
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn volume_page_traversal_crosses_sub_epsilon_direction_for_all_kernels_and_pick() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let mut all_keys = fixtures.terminal_sparse.clone();
    all_keys.push(fixtures.terminal);
    all_keys.push(fixtures.terminal_shifted);
    let lease_map = load_keys(
        &dataset_runtime,
        &all_keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 900),
        deadline,
    );
    let sparse_leases = owned_leases(&fixtures.terminal_sparse, &lease_map, None);
    let mut mixed_keys = fixtures.terminal_sparse.clone();
    mixed_keys.push(fixtures.terminal);
    let mixed_leases = owned_leases(&mixed_keys, &lease_map, None);
    let mut general_keys = fixtures.terminal_sparse.clone();
    general_keys.push(fixtures.terminal_shifted);
    let general_leases = owned_leases(&general_keys, &lease_map, None);
    let numerical_volume = traversal_numerical_volume();
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the traversal GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    request_target_layout(
        &mut gpu,
        PresentationTarget::ThreeD,
        RenderExtent::new(1, 1).unwrap(),
    );

    let mut frame = 900_u64;
    for direction_x in [5.0e-7_f64, -5.0e-7_f64] {
        assert!(matches!(
            classify_portable_direction(direction_x).unwrap(),
            PortableDirectionComponent::Moving(_)
        ));
        let (lower_x, upper_x, point_x) = if direction_x.is_sign_positive() {
            (-0.5, 31.5, 31.499_984)
        } else {
            (31.5, 63.5, 31.500_016)
        };
        let boundary = page_exit_distance_reference(
            [lower_x, -0.5, -f64::from(f32::MAX)],
            [upper_x, 63.5, f64::from(f32::MAX)],
            [point_x, 31.5, 0.0],
            [direction_x, 0.0, -(1.0 - direction_x * direction_x).sqrt()],
            0.0,
            64.0,
        )
        .expect("the independent traversal crossing is finite");
        assert!(
            boundary > 0.0 && boundary < 64.0,
            "the independently resolved X page boundary must precede ray exit: {boundary}"
        );
        let target_x = 31.5 + direction_x.signum() * 8.0e-6;

        for sampling in [SamplingPolicy::VoxelExact, SamplingPolicy::SmoothLinear] {
            for kernel in [
                TraversalKernel::Mip,
                TraversalKernel::Dvr,
                TraversalKernel::GeneralDvr,
                TraversalKernel::Iso,
                TraversalKernel::Mixed,
            ] {
                execute_traversal_case(
                    &mut gpu,
                    &catalog,
                    match kernel {
                        TraversalKernel::Mixed => &mixed_leases,
                        TraversalKernel::GeneralDvr => &general_leases,
                        _ => &sparse_leases,
                    },
                    &numerical_volume,
                    frame,
                    kernel,
                    sampling,
                    direction_x,
                    target_x,
                    &fixtures,
                    deadline,
                );
                frame += 1;
            }
        }
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the traversal fixture begins bounded shutdown");
    drop(sparse_leases);
    drop(mixed_leases);
    drop(general_leases);
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn volume_page_traversal_zero_direction_stays_in_one_page() {
    assert_eq!(
        classify_portable_direction(0.0).unwrap(),
        PortableDirectionComponent::Stationary
    );
    let exit =
        page_exit_distance_reference([-0.5; 3], [31.5; 3], [15.5; 3], [0.0, -0.0, 0.0], 0.0, 64.0)
            .expect("zero direction has one finite page segment");
    assert_eq!(exit, 64.0);
    assert_eq!(
        segment_end_index_reference(0.0, 1.0, 0, exit, 64)
            .expect("the stationary page consumes the remaining samples"),
        64
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let lease_map = load_keys(
        &dataset_runtime,
        &fixtures.terminal_sparse,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 920),
        deadline,
    );
    let leases = owned_leases(&fixtures.terminal_sparse, &lease_map, None);
    let numerical_volume = traversal_numerical_volume();
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the stationary traversal GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    request_target_layout(
        &mut gpu,
        PresentationTarget::ThreeD,
        RenderExtent::new(1, 1).unwrap(),
    );
    for (offset, sampling) in [
        (0_u64, SamplingPolicy::VoxelExact),
        (1, SamplingPolicy::SmoothLinear),
    ] {
        execute_traversal_case(
            &mut gpu,
            &catalog,
            &leases,
            &numerical_volume,
            920 + offset,
            TraversalKernel::Mip,
            sampling,
            0.0,
            15.5,
            &fixtures,
            deadline,
        );
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the stationary traversal fixture begins bounded shutdown");
    drop(leases);
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn volume_page_traversal_subnormal_direction_is_portably_canonicalized_or_rejected() {
    let subnormal = f64::from(f32::from_bits(0x0040_0000));
    assert_eq!(
        classify_portable_direction(subnormal).unwrap(),
        PortableDirectionComponent::Stationary
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let mut all_keys = fixtures.terminal_sparse.clone();
    all_keys.push(fixtures.terminal);
    all_keys.push(fixtures.terminal_shifted);
    let lease_map = load_keys(
        &dataset_runtime,
        &all_keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 930),
        deadline,
    );
    let sparse_leases = owned_leases(&fixtures.terminal_sparse, &lease_map, None);
    let mut mixed_keys = fixtures.terminal_sparse.clone();
    mixed_keys.push(fixtures.terminal);
    let mixed_leases = owned_leases(&mixed_keys, &lease_map, None);
    let mut general_keys = fixtures.terminal_sparse.clone();
    general_keys.push(fixtures.terminal_shifted);
    let general_leases = owned_leases(&general_keys, &lease_map, None);
    let numerical_volume = traversal_numerical_volume();
    let extent = RenderExtent::new(1, 1).unwrap();
    let (admission_intent, admission_requirements) = traversal_intent_and_requirements(
        930,
        TraversalKernel::Mip,
        SamplingPolicy::VoxelExact,
        traversal_volume_view(subnormal, 15.5),
        extent,
        &fixtures,
    );
    let admission = mirante4d_render_api::ShaderWorkEnvelope::for_intent(
        &catalog,
        &admission_intent,
        &admission_requirements,
    )
    .expect("a safely discardable subnormal component is admitted");
    let mirante4d_render_api::ShaderWorkEnvelope::Volume(admission) = admission else {
        panic!("the traversal admission fixture is volumetric")
    };
    assert_eq!(
        classify_portable_direction(f64::from(admission.controls().direction_base()[0])).unwrap(),
        PortableDirectionComponent::Stationary,
        "the exact binary32 shader control is canonical stationary"
    );
    assert!(
        admission.facts().iter().all(|facts| facts
            .grid_error_upper()
            .into_iter()
            .all(|upper| upper < 0.5)),
        "the complete admitted work envelope retains the half-voxel bound"
    );

    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the subnormal traversal GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    request_target_layout(&mut gpu, PresentationTarget::ThreeD, extent);
    let mut frame = 930_u64;
    for sampling in [SamplingPolicy::VoxelExact, SamplingPolicy::SmoothLinear] {
        for kernel in [
            TraversalKernel::Mip,
            TraversalKernel::Dvr,
            TraversalKernel::GeneralDvr,
            TraversalKernel::Iso,
            TraversalKernel::Mixed,
        ] {
            execute_traversal_case(
                &mut gpu,
                &catalog,
                match kernel {
                    TraversalKernel::Mixed => &mixed_leases,
                    TraversalKernel::GeneralDvr => &general_leases,
                    _ => &sparse_leases,
                },
                &numerical_volume,
                frame,
                kernel,
                sampling,
                subnormal,
                15.5,
                &fixtures,
                deadline,
            );
            frame += 1;
        }
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the subnormal traversal fixture begins bounded shutdown");
    drop(sparse_leases);
    drop(mixed_leases);
    drop(general_leases);
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn volume_page_traversal_far_boundary_overflow_clamps_only_to_ray_exit() {
    let direction_x = 2.0 * f64::from(f32::MIN_POSITIVE);
    assert!(matches!(
        classify_portable_direction(direction_x).unwrap(),
        PortableDirectionComponent::Moving(_)
    ));
    let exit = page_exit_distance_reference(
        [-0.5; 3],
        [f64::from(f32::MAX), 31.5, 31.5],
        [15.5; 3],
        [direction_x, 0.0, 0.0],
        0.0,
        64.0,
    )
    .expect("the irrelevant far boundary does not require its overflowing quotient");
    assert_eq!(exit, 64.0);

    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let lease_map = load_keys(
        &dataset_runtime,
        &fixtures.terminal_sparse,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 940),
        deadline,
    );
    let leases = owned_leases(&fixtures.terminal_sparse, &lease_map, None);
    let numerical_volume = traversal_numerical_volume();
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the far-boundary traversal GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    request_target_layout(
        &mut gpu,
        PresentationTarget::ThreeD,
        RenderExtent::new(1, 1).unwrap(),
    );
    execute_traversal_case(
        &mut gpu,
        &catalog,
        &leases,
        &numerical_volume,
        940,
        TraversalKernel::Mip,
        SamplingPolicy::VoxelExact,
        direction_x,
        15.5,
        &fixtures,
        deadline,
    );
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the far-boundary traversal fixture begins bounded shutdown");
    drop(leases);
    drop(lease_map);
    drop(dataset_runtime);
}

struct ComparisonInput<'a> {
    catalog: &'a DatasetCatalog,
    intent: &'a RenderIntent,
    requirements: &'a RenderRequirements,
    leases: &'a [Arc<dyn ResourceLease>],
}

fn request_target_layout(
    gpu: &mut WgpuRenderRuntime,
    target: PresentationTarget,
    extent: RenderExtent,
) {
    let report = gpu
        .request_coordinated_layout(&[CoordinatedTargetLayout::new(target, extent)])
        .expect("the fixed target layout is accepted");
    assert_eq!(
        report.target(target).map(|state| state.extent()),
        Some(extent)
    );
}

fn validated_fixture_intent(
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
) -> RenderIntent {
    intent.clone().with_shader_work_envelope(
        mirante4d_render_api::ShaderWorkEnvelope::for_intent(catalog, intent, requirements)
            .expect("the GPU fixture has a valid shader-work envelope"),
    )
}

fn execute_coordinated_target(
    gpu: &mut WgpuRenderRuntime,
    target: PresentationTarget,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    render_policy: RetainedFrameRenderPolicy,
) -> super::CoordinatedFrameExecutionReport {
    let validated_intent = if intent.shader_work_envelope().is_some() {
        intent.clone()
    } else {
        validated_fixture_intent(catalog, intent, requirements)
    };
    let request = CoordinatedTargetRequest::new(
        target,
        &validated_intent,
        requirements,
        validated_intent.frame().get(),
        render_policy,
    );
    gpu.execute_coordinated_frame(catalog, target, &[request])
        .expect("the fixed-target coordinated frame executes")
}

fn wait_for_submission_completion(
    gpu: &mut WgpuRenderRuntime,
    events: &Arc<Mutex<VecDeque<RendererEvent>>>,
    expected_submission: u64,
    deadline: Instant,
) {
    loop {
        let completed = {
            let mut events = events
                .lock()
                .expect("the renderer test event queue is never poisoned");
            let mut completed = false;
            while let Some(event) = events.pop_front() {
                if matches!(
                    event,
                    RendererEvent::SubmissionCompleted { submission }
                        if submission >= expected_submission
                ) {
                    completed = true;
                }
            }
            completed
        };
        if completed {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "renderer submission {expected_submission} did not complete before the test deadline"
        );
        let completed_submission = gpu
            .poll_submission_completions()
            .expect("the renderer completion boundary remains healthy");
        if completed_submission >= expected_submission {
            return;
        }
        std::thread::yield_now();
    }
}

fn presented_frame(
    target: PresentationTarget,
    intent: &RenderIntent,
    report: &super::CoordinatedTargetExecutionReport,
) -> PresentedFrame {
    assert!(
        report.presented(),
        "the fixed target did not publish pixels"
    );
    PresentedFrame::new(
        target,
        intent.extent(),
        report
            .progress()
            .cloned()
            .expect("a presented coordinated target has progress"),
    )
}

fn execute_and_compare(
    gpu: &mut WgpuRenderRuntime,
    target: PresentationTarget,
    input: ComparisonInput<'_>,
    deadline: Instant,
) -> (ValidationCapture, u8, usize) {
    gpu.offer_residency_leases(input.leases)
        .expect("semantic GPU leases are offered");
    let report = execute_coordinated_target(
        gpu,
        target,
        input.catalog,
        input.intent,
        input.requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let target_report = report
        .target(target)
        .expect("the coordinated target has one execution report");
    assert!(!target_report.deferred_by_backpressure());
    assert_eq!(
        target_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let uploaded_resources = target_report.uploaded_resources();
    let ticket = target_report
        .validation_capture()
        .expect("the validation-enabled fixture produces an asynchronous capture");
    let capture = poll_coordinated_capture(gpu, ticket, deadline);
    let lease_refs = input.leases.iter().map(Arc::as_ref).collect::<Vec<_>>();
    let reference = ReferenceRenderer::new()
        .render(input.catalog, input.intent, &lease_refs)
        .expect("independent CPU reference renders fixture leases");
    let max_delta = compare_reference(&capture, &reference);
    (capture, max_delta, uploaded_resources)
}

fn execute_and_capture(
    gpu: &mut WgpuRenderRuntime,
    target: PresentationTarget,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    leases: &[Arc<dyn ResourceLease>],
    deadline: Instant,
) -> ValidationCapture {
    gpu.offer_residency_leases(leases)
        .expect("capture GPU leases are offered");
    let report = execute_coordinated_target(
        gpu,
        target,
        catalog,
        intent,
        requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let target_report = report
        .target(target)
        .expect("the coordinated target has one execution report");
    assert!(!target_report.deferred_by_backpressure());
    assert_eq!(
        target_report
            .progress()
            .map(mirante4d_render_api::FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    poll_coordinated_capture(
        gpu,
        target_report
            .validation_capture()
            .expect("multichannel validation capture exists"),
        deadline,
    )
}

fn execute_presented_and_capture(
    gpu: &mut WgpuRenderRuntime,
    target: PresentationTarget,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    leases: &[Arc<dyn ResourceLease>],
    deadline: Instant,
) -> (PresentedFrame, ValidationCapture) {
    gpu.offer_residency_leases(leases)
        .expect("numerical-conformance GPU leases are offered");
    let report = execute_coordinated_target(
        gpu,
        target,
        catalog,
        intent,
        requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let target_report = report
        .target(target)
        .expect("the coordinated target has one execution report");
    assert!(!target_report.deferred_by_backpressure());
    assert_eq!(
        target_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let presented = presented_frame(target, intent, target_report);
    let capture = poll_coordinated_capture(
        gpu,
        target_report
            .validation_capture()
            .expect("numerical-conformance validation capture exists"),
        deadline,
    );
    (presented, capture)
}

fn sanitize_diagnostic_text(text: &str) -> String {
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

fn assert_qualifying_adapter(diagnostics: &WgpuRenderRuntimeDiagnostics) {
    assert_eq!(diagnostics.backend(), "Vulkan");
    assert!(diagnostics.max_buffer_size_bytes() >= 256 * MIB);
    assert!(diagnostics.max_storage_buffer_binding_size_bytes() >= 256 * MIB);
    assert!(diagnostics.max_storage_buffers_per_shader_stage() >= 8);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn existing_device_constructor_publishes_color_before_pick_readiness() {
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB).expect("the cold-start GPU ledger is valid"),
        None,
        TestPipelineAdmission::Initializing,
    );

    assert_eq!(
        gpu.pipeline_readiness(),
        Ok(PipelineReadiness::CompilingInitial)
    );
    assert_eq!(gpu.diagnostics().usable_pipeline_handles(), 0);
    assert_eq!(
        gpu.pipeline_capability_is_ready(PipelineCapability::InitialRender),
        Ok(false)
    );
    assert_eq!(
        gpu.pipeline_capability_is_ready(PipelineCapability::Pick),
        Ok(false)
    );

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match gpu
            .poll_pipeline_readiness()
            .expect("initial pipeline compilation succeeds")
        {
            PipelineReadiness::CompilingInitial => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            PipelineReadiness::InitialRenderReady => break,
            PipelineReadiness::Ready => {
                panic!("one readiness poll must not consume color and pick events together")
            }
        }
    }
    assert_eq!(gpu.diagnostics().usable_pipeline_handles(), 5);
    assert_eq!(
        gpu.pipeline_capability_is_ready(PipelineCapability::InitialRender),
        Ok(true)
    );
    assert_eq!(
        gpu.pipeline_capability_is_ready(PipelineCapability::Pick),
        Ok(false)
    );

    loop {
        match gpu
            .poll_pipeline_readiness()
            .expect("pick pipeline compilation succeeds")
        {
            PipelineReadiness::InitialRenderReady => {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            PipelineReadiness::Ready => break,
            PipelineReadiness::CompilingInitial => {
                panic!("pipeline readiness cannot regress")
            }
        }
    }
    assert_eq!(gpu.diagnostics().usable_pipeline_handles(), 6);
    assert_eq!(
        gpu.pipeline_capability_is_ready(PipelineCapability::Pick),
        Ok(true)
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn progressive_plane_falls_back_by_whole_footprint_and_invalid_never_falls_through() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let lease_map = load_keys(
        &dataset_runtime,
        &fixtures.multiscale_plane,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 810),
        deadline,
    );
    let fine_left = owned_leases(&fixtures.multiscale_plane[..1], &lease_map, None);
    let fine_right = owned_leases(&fixtures.multiscale_plane[1..2], &lease_map, None);
    let coarse = owned_leases(&fixtures.multiscale_plane[2..], &lease_map, None);

    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the progressive plane GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let extent = RenderExtent::new(1, 1).unwrap();
    let target = PresentationTarget::Xy;
    request_target_layout(&mut gpu, target, extent);
    let (smooth_intent, _) = intent_and_requirements(
        810,
        12,
        mirante4d_domain::RenderState::mip(SamplingPolicy::SmoothLinear),
        cross_section_view([1.5, 0.5, 0.5], 1.0),
        extent,
        &fixtures.multiscale_plane,
    );
    let smooth_requirements = RenderRequirements::new(
        &smooth_intent,
        vec![
            RenderRequirement::new(
                fixtures.multiscale_plane[2],
                RenderRequirementRole::FirstUsefulFrame,
            ),
            RenderRequirement::new(
                fixtures.multiscale_plane[0],
                RenderRequirementRole::Refinement,
            ),
            RenderRequirement::new(
                fixtures.multiscale_plane[1],
                RenderRequirementRole::Refinement,
            ),
        ],
    )
    .unwrap();
    let execute_capture = |gpu: &mut WgpuRenderRuntime,
                           intent: &RenderIntent,
                           requirements: &RenderRequirements| {
        let execution = execute_coordinated_target(
            gpu,
            target,
            &catalog,
            intent,
            requirements,
            RetainedFrameRenderPolicy::EveryUsefulFrame,
        );
        let report = execution.target(target).unwrap();
        assert!(report.presented());
        let progress = report.progress().cloned().unwrap();
        let capture = poll_coordinated_capture(gpu, report.validation_capture().unwrap(), deadline);
        (progress, capture)
    };

    gpu.offer_residency_leases(&coarse).unwrap();
    let (fallback_progress, fallback_capture) =
        execute_capture(&mut gpu, &smooth_intent, &smooth_requirements);
    assert_eq!(
        fallback_progress.completeness(),
        FrameCompleteness::Progressive
    );
    let fallback_layer = fallback_progress
        .coverage()
        .layer_coverages()
        .next()
        .unwrap();
    assert_eq!(fallback_layer.scale(), Some(ScaleLevel::new(1)));
    assert_eq!(pixel(&fallback_capture, 0, 0), ([40, 0, 0, 255], 1, 1));

    gpu.offer_residency_leases(&fine_left).unwrap();
    let (mixed_progress, mixed_capture) =
        execute_capture(&mut gpu, &smooth_intent, &smooth_requirements);
    let mixed_layer = mixed_progress.coverage().layer_coverages().next().unwrap();
    assert!(mixed_layer.is_mixed());
    assert_eq!(
        pixel(&mixed_capture, 0, 0),
        pixel(&fallback_capture, 0, 0),
        "one missing fine tap must retry the complete footprint at the coarse scale"
    );

    gpu.offer_residency_leases(&fine_right).unwrap();
    let (exact_progress, exact_capture) =
        execute_capture(&mut gpu, &smooth_intent, &smooth_requirements);
    assert_eq!(exact_progress.completeness(), FrameCompleteness::Exact);
    assert_eq!(pixel(&exact_capture, 0, 0), ([200, 0, 0, 255], 1, 1));

    let (invalid_intent, _) = intent_and_requirements(
        811,
        12,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([0.0, 0.0, 0.0], 1.0),
        extent,
        &fixtures.multiscale_plane,
    );
    let invalid_requirements = smooth_requirements.rebind(&invalid_intent).unwrap();
    let (invalid_progress, invalid_capture) =
        execute_capture(&mut gpu, &invalid_intent, &invalid_requirements);
    assert_eq!(invalid_progress.completeness(), FrameCompleteness::Exact);
    assert_eq!(
        pixel(&invalid_capture, 0, 0),
        ([0, 0, 0, 0], 1, 0),
        "resident invalid fine data is authoritative and must not expose the valid coarse value"
    );

    dataset_runtime.request_shutdown().unwrap();
    drop(fine_left);
    drop(fine_right);
    drop(coarse);
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn dedicated_mip_off_axis_color_matches_independent_numerical_oracle() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 699);
    let f32_lease_map = load_keys(
        &dataset_runtime,
        &fixtures.semantic[2],
        generation,
        deadline,
    );
    let f32_leases = owned_leases(&fixtures.semantic[2], &f32_lease_map, None);
    let volume = numerical_semantic_f32_volume();
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the dedicated MIP GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let extent = RenderExtent::new(65, 65).expect("the numerical volume extent is valid");
    let target = PresentationTarget::ThreeD;
    request_target_layout(&mut gpu, target, extent);
    let (intent, requirements) = intent_and_requirements(
        699,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        numerical_primary_perspective_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (_presented, capture) = execute_presented_and_capture(
        &mut gpu,
        target,
        &catalog,
        &intent,
        &requirements,
        &f32_leases,
        deadline,
    );
    let transfer = numerical_transfer([0.0, 1.0], [1.0, 0.0, 0.0], 1.0);
    let expected = numerical_volume_facts(&volume, transfer, NumericalVolumeMode::Mip);
    assert_numerical_color(
        "dedicated off-axis perspective MIP",
        &capture,
        [48, 32],
        expected.color(),
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn dedicated_iso_affine_lighting_and_six_tap_halo_match_independent_reference() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 704);
    let affine_lease_map = load_keys(&dataset_runtime, &[fixtures.affine], generation, deadline);
    let halo_lease_map = load_keys(
        &dataset_runtime,
        &fixtures.semantic[0],
        generation,
        deadline,
    );
    let affine_leases = owned_leases(&[fixtures.affine], &affine_lease_map, None);
    let missing_halo_key = fixtures.missing_u8;
    let mut halo_keys = fixtures.semantic[0]
        .iter()
        .copied()
        .filter(|key| *key != missing_halo_key)
        .collect::<Vec<_>>();
    halo_keys.push(missing_halo_key);
    let incomplete_halo_leases = owned_leases(&halo_keys, &halo_lease_map, Some(missing_halo_key));
    let complete_halo_leases = owned_leases(&halo_keys, &halo_lease_map, None);

    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the dedicated ISO GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let extent = RenderExtent::new(96, 96).expect("the dedicated ISO extent is valid");
    let target = PresentationTarget::ThreeD;
    request_target_layout(&mut gpu, target, extent);

    // The affine scalar field has non-axis-aligned grid gradients. Inverted
    // transfer delays the first threshold hit beyond the entry face so all six
    // central-difference taps are interior. Attached and detached world lights
    // must therefore produce distinct colors, each matching the independent
    // CPU implementation of inverse-transpose normal transformation.
    let affine_state = mirante4d_domain::RenderState::iso(
        SamplingPolicy::VoxelExact,
        IsoShadingPolicy::GradientLighting,
        0.2,
    )
    .expect("the affine gradient ISO state is valid");
    let affine_transfer = LayerTransfer::new(
        DisplayWindow::new(0.0, 1.0).expect("the affine display window is valid"),
        RgbColor::new([1.0, 0.0, 0.0]).expect("the affine color is valid"),
        Opacity::new(1.0).expect("the affine opacity is valid"),
        TransferCurve::linear(),
        true,
    );
    let affine_view = |light| volume_view_for_light([7.375, 5.5, 5.2], 1.0 / 32.0, 16.0, light);
    let mut affine_pixels = Vec::new();
    for (frame, light, label) in [
        (
            704_u64,
            IsoLightState::attached_camera(),
            "attached affine ISO light",
        ),
        (
            705,
            IsoLightState::detached_screen(0.25, -0.5)
                .expect("the detached affine ISO light is valid"),
            "detached affine ISO light",
        ),
    ] {
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            affine_view(light),
            extent,
            vec![LayerRenderIntent::new(
                LogicalLayerKey::new(11),
                affine_transfer.clone(),
                affine_state,
            )],
            &[fixtures.affine],
        );
        let capture = execute_and_capture(
            &mut gpu,
            target,
            &catalog,
            &intent,
            &requirements,
            &affine_leases,
            deadline,
        );
        let affine_lease_refs = affine_leases.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let reference = ReferenceRenderer::new()
            .render(&catalog, &intent, &affine_lease_refs)
            .expect("the CPU reference renders the affine gradient ISO frame");
        let max_delta = assert_pixel_matches_reference(label, &capture, &reference, 48, 48);
        assert!(
            max_delta <= 1,
            "{label} differed from the independent CPU reference by {max_delta}"
        );
        let observed = pixel(&capture, 48, 48);
        assert_eq!(
            (observed.1, observed.2),
            (1, 1),
            "{label} must have complete valid gradient support"
        );
        assert_eq!(observed.0[3], 255, "{label} must hit the opaque ISO");
        affine_pixels.push(observed.0);
    }
    assert!(
        affine_pixels[0][0].abs_diff(affine_pixels[1][0]) > 2,
        "attached and detached world lights must produce observably distinct affine ISO shading: {:?}",
        affine_pixels
    );
    gpu.request_coordinated_layout(&[])
        .expect("the affine target is retired before the independent halo case");
    request_target_layout(&mut gpu, target, extent);

    let halo_state = mirante4d_domain::RenderState::iso(
        SamplingPolicy::VoxelExact,
        IsoShadingPolicy::GradientLighting,
        136.0 / 255.0,
    )
    .expect("the six-tap halo ISO state is valid");
    let halo_transfer = LayerTransfer::new(
        DisplayWindow::new(0.0, 255.0).expect("the halo display window is valid"),
        RgbColor::new([1.0, 0.0, 0.0]).expect("the halo color is valid"),
        Opacity::new(1.0).expect("the halo opacity is valid"),
        TransferCurve::linear(),
        true,
    );
    let halo_view = volume_view_for_light(
        [15.0, 15.0, 15.0],
        32.0 / 96.0,
        40.0,
        IsoLightState::detached_screen(0.25, -0.5).expect("the detached halo ISO light is valid"),
    );
    let halo_intent = |frame| {
        multichannel_intent_and_requirements(
            frame,
            halo_view,
            extent,
            vec![LayerRenderIntent::new(
                LogicalLayerKey::new(0),
                halo_transfer.clone(),
                halo_state,
            )],
            &halo_keys,
        )
    };

    // The threshold center sample is resident. Only its +X gradient-support
    // page is withheld, so incompleteness comes from the six-tap lighting halo
    // rather than from the visible ray sample itself.
    let (incomplete_intent, incomplete_requirements) = halo_intent(706);
    gpu.offer_residency_leases(&incomplete_halo_leases)
        .expect("the incomplete gradient-halo leases are offered");
    let incomplete_cutoff = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &incomplete_intent,
        &incomplete_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let incomplete_report = incomplete_cutoff
        .target(target)
        .expect("the incomplete gradient-halo target has one report");
    assert_eq!(
        incomplete_report
            .progress()
            .map(FrameProgress::completeness),
        Some(FrameCompleteness::Progressive)
    );
    let incomplete_presented = presented_frame(target, &incomplete_intent, incomplete_report);
    let incomplete_capture = poll_coordinated_capture(
        &mut gpu,
        incomplete_report
            .validation_capture()
            .expect("the incomplete gradient-halo capture exists"),
        deadline,
    );
    let (_, incomplete_coverage, incomplete_validity) = pixel(&incomplete_capture, 48, 48);
    assert_eq!(
        (incomplete_coverage, incomplete_validity),
        (0, 1),
        "missing gradient support is incomplete even when the ISO center sample is valid"
    );
    let incomplete_query = VolumePickQuery::new(
        &incomplete_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(0),
        [47.5, 47.5],
        VolumePickPolicy::FirstThresholdHit,
    )
    .expect("the incomplete gradient-halo pick query is valid");
    let incomplete_ticket = gpu
        .request_coordinated_pick(target, incomplete_query)
        .expect("the incomplete gradient-halo pick is accepted");
    let incomplete_pick = poll_pick(&mut gpu, incomplete_ticket, deadline);
    assert_eq!(incomplete_pick.query(), incomplete_query);
    assert_eq!(incomplete_pick.kind(), VolumePickHitKind::Voxel);
    assert_eq!(
        incomplete_pick.completeness(),
        VolumePickCompleteness::Incomplete
    );

    let (complete_intent, complete_requirements) = halo_intent(707);
    gpu.offer_residency_leases(&complete_halo_leases)
        .expect("the complete gradient-halo leases are offered");
    let complete_cutoff = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &complete_intent,
        &complete_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let complete_report = complete_cutoff
        .target(target)
        .expect("the complete gradient-halo target has one report");
    assert_eq!(
        complete_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    let complete_presented = presented_frame(target, &complete_intent, complete_report);
    let complete_capture = poll_coordinated_capture(
        &mut gpu,
        complete_report
            .validation_capture()
            .expect("the complete gradient-halo capture exists"),
        deadline,
    );
    let (_, complete_coverage, complete_validity) = pixel(&complete_capture, 48, 48);
    assert_eq!(
        (complete_coverage, complete_validity),
        (1, 1),
        "resident six-tap support restores exact covered ISO output"
    );
    let complete_query = VolumePickQuery::new(
        &complete_presented,
        TimeIndex::new(0),
        LogicalLayerKey::new(0),
        [47.5, 47.5],
        VolumePickPolicy::FirstThresholdHit,
    )
    .expect("the complete gradient-halo pick query is valid");
    let complete_ticket = gpu
        .request_coordinated_pick(target, complete_query)
        .expect("the complete gradient-halo pick is accepted");
    let complete_pick = poll_pick(&mut gpu, complete_ticket, deadline);
    assert_eq!(complete_pick.query(), complete_query);
    assert_eq!(complete_pick.kind(), VolumePickHitKind::Voxel);
    assert_eq!(complete_pick.completeness(), VolumePickCompleteness::Exact);

    assert_eq!(gpu.diagnostics().validation_error_count(), 0);
    dataset_runtime
        .request_shutdown()
        .expect("the dedicated ISO fixture begins bounded shutdown");
    drop(affine_leases);
    drop(incomplete_halo_leases);
    drop(complete_halo_leases);
    drop(affine_lease_map);
    drop(halo_lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn dedicated_mixed_mip_iso_authored_over_matches_independent_hand_facts() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let keys = [fixtures.multichannel[0], fixtures.multichannel[1]];
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 708);
    let lease_map = load_keys(&dataset_runtime, &keys, generation, deadline);
    let extent = RenderExtent::new(64, 64).expect("the dedicated Mixed extent is valid");
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the dedicated Mixed GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let target = PresentationTarget::ThreeD;
    request_target_layout(&mut gpu, target, extent);
    let mip_state = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
    let iso_state =
        mirante4d_domain::RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.1)
            .expect("the dedicated Mixed ISO state is valid");
    let layer = |ordinal, state| {
        LayerRenderIntent::new(
            LogicalLayerKey::new(ordinal),
            transfer_color_opacity(
                0.0,
                1.0,
                if ordinal == 5 {
                    [1.0, 0.0, 0.0]
                } else {
                    [0.0, 1.0, 0.0]
                },
                if ordinal == 5 { 0.4 } else { 0.6 },
            ),
            state,
        )
    };

    let mut observed = Vec::new();
    for (frame, order, expected) in [
        (708_u64, [5_u32, 6_u32], [26, 69, 0, 194]),
        (709, [6_u32, 5_u32], [10, 115, 0, 194]),
    ] {
        let states = if order[0] == 5 {
            [mip_state, iso_state]
        } else {
            [iso_state, mip_state]
        };
        let case_keys = order.map(|ordinal| keys[(ordinal - 5) as usize]);
        let case_leases = case_keys
            .iter()
            .map(|key| {
                Arc::new(
                    lease_map
                        .get(key)
                        .expect("the dedicated Mixed lease exists")
                        .clone(),
                ) as Arc<dyn ResourceLease>
            })
            .collect::<Vec<_>>();
        let (intent, requirements) = multichannel_intent_and_requirements(
            frame,
            volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
            extent,
            vec![layer(order[0], states[0]), layer(order[1], states[1])],
            &case_keys,
        );
        let (capture, max_delta, _) = execute_and_compare(
            &mut gpu,
            target,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
        );
        assert!(
            max_delta <= 1,
            "authored Mixed order {order:?} differed from the independent CPU reference by {max_delta}"
        );
        assert_pixel_near(&capture, 32, 32, expected, 1, 1);
        observed.push(pixel(&capture, 32, 32).0);
    }
    assert_ne!(
        observed[0], observed[1],
        "reversing authored MIP/ISO order must reverse the noncommutative over-composition"
    );
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the dedicated Mixed fixture begins bounded shutdown");
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn dedicated_dvr_off_axis_perspective_uses_physical_world_distance() {
    // Keep the independent physical-world-distance expectation fixed so a
    // perspective ray's native, non-unit parameter cannot silently become an
    // optical-distance unit.
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
    let leases = owned_leases(&fixtures.semantic[2], &lease_map, None);
    let extent = RenderExtent::new(65, 65).expect("the off-axis DVR extent is valid");
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the off-axis DVR GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let target = PresentationTarget::ThreeD;
    request_target_layout(&mut gpu, target, extent);
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
        numerical_primary_perspective_view(),
        extent,
        &fixtures.semantic[2],
    );
    let (presented, capture) = execute_presented_and_capture(
        &mut gpu,
        target,
        &catalog,
        &intent,
        &requirements,
        &leases,
        deadline,
    );
    let expected = numerical_volume_facts(
        &numerical_semantic_f32_volume(),
        numerical_transfer([0.0, 1.0], [1.0, 0.0, 0.0], 1.0),
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
        .request_coordinated_pick(target, query)
        .expect("the off-axis DVR pick is accepted");
    let observed_pick = poll_pick(&mut gpu, ticket, deadline);
    assert_numerical_pick(
        "off-axis perspective DVR",
        observed_pick,
        expected
            .pick()
            .expect("the independent off-axis DVR has a pick"),
    );
    assert_numerical_color(
        "off-axis perspective DVR",
        &capture,
        [48, 32],
        expected.color(),
    );

    dataset_runtime
        .request_shutdown()
        .expect("the off-axis DVR fixture begins bounded shutdown");
    drop(leases);
    drop(lease_map);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn resident_body_changes_reuse_global_gpu_residency_across_targets() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let keys = [fixtures.work[192], fixtures.work[935]];
    let collision_leases = load_keys(
        &dataset_runtime,
        &keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 705),
        deadline,
    );
    let upload_leases = load_keys(
        &dataset_runtime,
        &fixtures.upload,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 705),
        deadline,
    );
    let resource_center = |key: BrickKey| {
        let origin = key.region().origin();
        let shape = key.region().shape().dimensions();
        [
            origin[2] as f64 + (shape[2] - 1) as f64 * 0.5,
            origin[1] as f64 + (shape[1] - 1) as f64 * 0.5,
            origin[0] as f64 + (shape[0] - 1) as f64 * 0.5,
        ]
    };
    let compact = keys.map(|key| {
        let grid = fixtures
            .resource_grids
            .validate_key(key)
            .expect("the collision key belongs to the fixture grid");
        let cells =
            compact_cell_keys(key, grid).expect("the collision key has a compact directory key");
        assert_eq!(
            cells.len(),
            1,
            "each collision fixture must occupy exactly one directory cell"
        );
        cells[0]
    });
    let directory_mask =
        u32::try_from(GLOBAL_DIRECTORY_SLOTS - 1).expect("the production directory mask fits u32");
    let hashes = compact.map(directory_hash);
    assert_ne!(
        hashes[0], hashes[1],
        "the fixture must exercise probing, not duplicate compact keys"
    );
    assert_eq!(
        hashes[0] & directory_mask,
        hashes[1] & directory_mask,
        "the fixture keys must collide in the actual production directory"
    );

    let extent = RenderExtent::new(1, 1).expect("the one-pixel resident fixture is valid");
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("the resident-body GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let residency_offers = owned_leases(&keys, &collision_leases, None);
    let upload_offers = owned_leases(&fixtures.upload, &upload_leases, None);
    gpu.offer_residency_leases(&residency_offers)
        .expect("future-target residency leases are offered");
    assert!(
        !gpu.has_pending_residency_work(),
        "offers without an active target must not force repaint polling"
    );
    let first_target = PresentationTarget::Xy;
    let second_target = PresentationTarget::Xz;
    let churn_target = PresentationTarget::Yz;
    let layout = [first_target, second_target, churn_target]
        .map(|target| CoordinatedTargetLayout::new(target, extent));
    let layout_report = gpu
        .request_coordinated_layout(&layout)
        .expect("the three fixed resident targets allocate transactionally");
    assert_eq!(layout_report.targets().len(), layout.len());

    let (first_intent, first_requirements) = intent_and_requirements(
        1,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view(resource_center(keys[0]), 1.0),
        extent,
        &keys[..1],
    );
    let (first_capture, _, first_uploads) = execute_and_compare(
        &mut gpu,
        first_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &first_intent,
            requirements: &first_requirements,
            leases: &residency_offers[..1],
        },
        deadline,
    );
    assert_eq!(first_uploads, 1);

    let (second_intent, second_requirements) = intent_and_requirements(
        1,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view(resource_center(keys[1]), 1.0),
        extent,
        &keys[1..],
    );
    let (second_capture, _, second_uploads) = execute_and_compare(
        &mut gpu,
        second_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &second_intent,
            requirements: &second_requirements,
            leases: &residency_offers[1..],
        },
        deadline,
    );
    assert_eq!(second_uploads, 1);

    let first_pixel = pixel(&first_capture, 0, 0);
    let second_pixel = pixel(&second_capture, 0, 0);
    assert_eq!((first_pixel.1, first_pixel.2), (1, 1));
    assert_eq!((second_pixel.1, second_pixel.2), (1, 1));
    assert_ne!(
        first_pixel.0, second_pixel.0,
        "the two warm resident bodies must produce observably different pixels"
    );
    assert_ne!(second_pixel.0, [0, 0, 0, 0]);
    assert_eq!(
        gpu.diagnostics().peak_in_flight_color_cutoffs(),
        1,
        "the sequential coordinated fixture must observe exactly one live color cutoff"
    );

    let control_buffers = gpu.diagnostics().control_buffer_allocations();
    let bind_groups = gpu.diagnostics().bind_group_creations();
    let pipelines = gpu.diagnostics().usable_pipeline_handles();
    let uploads = gpu.diagnostics().uploaded_resources();
    let evictions = gpu.diagnostics().residency_evictions();
    let resident_bytes = gpu.diagnostics().resident_payload_bytes();
    let allocator_plans = gpu.diagnostics().allocator_plans();
    let coverage_membership_checks = gpu.diagnostics().cold_coverage_membership_checks();
    let coverage_resident_matches = gpu.diagnostics().cold_coverage_resident_matches();
    let directory_mutations = gpu.diagnostics().directory_mutations();

    let (replacement_intent, replacement_requirements) = intent_and_requirements(
        2,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view(resource_center(keys[1]), 1.0),
        extent,
        &keys[1..],
    );
    assert!(
        !replacement_requirements.shares_resources_with(&second_requirements),
        "the regression needs a fresh requirement body so no retained presentation can source the proof"
    );
    let (replacement_capture, _, replacement_uploads) = execute_and_compare(
        &mut gpu,
        first_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &replacement_intent,
            requirements: &replacement_requirements,
            leases: &residency_offers[1..],
        },
        deadline,
    );
    assert_eq!(replacement_uploads, 0);
    assert_eq!(
        (
            replacement_capture.rgba8(),
            replacement_capture.coverage(),
            replacement_capture.validity(),
        ),
        (
            second_capture.rgba8(),
            second_capture.coverage(),
            second_capture.validity(),
        ),
        "cross-target residency reuse must render the exact expected body"
    );

    assert_eq!(gpu.diagnostics().uploaded_resources(), uploads);
    assert_eq!(gpu.diagnostics().residency_evictions(), evictions);
    assert_eq!(gpu.diagnostics().resident_payload_bytes(), resident_bytes);
    assert_eq!(
        gpu.diagnostics().allocator_plans(),
        allocator_plans,
        "a changed requirement body already present in global residency must bypass allocator planning"
    );
    assert_eq!(
        gpu.diagnostics().cold_coverage_membership_checks(),
        coverage_membership_checks + 1,
        "the fresh body must execute exactly one global-residency membership proof"
    );
    assert_eq!(
        gpu.diagnostics().cold_coverage_resident_matches(),
        coverage_resident_matches + 1,
        "the cold coverage proof must directly count its reused resident requirement"
    );
    assert_eq!(
        gpu.diagnostics().directory_mutations(),
        directory_mutations,
        "a changed fully resident body must not republish residency metadata"
    );
    assert_eq!(
        gpu.diagnostics().control_buffer_allocations(),
        control_buffers,
        "a registered target keeps its fixed target-control buffer"
    );
    assert_eq!(
        gpu.diagnostics().bind_group_creations(),
        bind_groups,
        "resident view intent must not recreate bindings"
    );
    assert_eq!(gpu.diagnostics().usable_pipeline_handles(), pipelines);
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    gpu.request_coordinated_layout(&[
        CoordinatedTargetLayout::new(first_target, extent),
        CoordinatedTargetLayout::new(churn_target, extent),
    ])
    .expect("the source target retires before capacity churn");

    for (index, (key, lease)) in fixtures
        .upload
        .iter()
        .copied()
        .zip(upload_offers.iter())
        .enumerate()
    {
        let (intent, requirements) = intent_and_requirements(
            100 + index as u64,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view(resource_center(key), 1.0),
            extent,
            &[key],
        );
        let (_, _, uploaded_resources) = execute_and_compare(
            &mut gpu,
            churn_target,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: std::slice::from_ref(lease),
            },
            deadline,
        );
        assert_eq!(uploaded_resources, 1);
        if index < fixtures.upload.len() - 1 {
            assert!(
                gpu.resource_is_resident(keys[0]),
                "the collision fixture must remain resident until the final capacity miss"
            );
        }
    }
    assert!(
        !gpu.resource_is_resident(keys[0]),
        "the oldest unpinned collision entry must be evicted"
    );
    assert!(
        gpu.resource_is_resident(keys[1]),
        "the changed-body target must retain its globally proven resource after the source target retires"
    );
    assert!(
        gpu.pending_residency_evictions(16)
            .iter()
            .any(|event| event.key() == keys[0]),
        "the exact collision-key eviction must cross the renderer event boundary"
    );

    let tombstone_intent = second_intent.clone().with_frame(FrameIdentity::new(3));
    let tombstone_requirements = second_requirements
        .rebind(&tombstone_intent)
        .expect("the resident colliding body rebinds through the tombstone");
    let (tombstone_capture, _, tombstone_uploads) = execute_and_compare(
        &mut gpu,
        first_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &tombstone_intent,
            requirements: &tombstone_requirements,
            leases: &residency_offers[1..],
        },
        deadline,
    );
    assert_eq!(
        tombstone_uploads, 0,
        "shader lookup must reach the resident collision without a reupload"
    );
    assert_eq!(
        (
            tombstone_capture.rgba8(),
            tombstone_capture.coverage(),
            tombstone_capture.validity(),
        ),
        (
            second_capture.rgba8(),
            second_capture.coverage(),
            second_capture.validity(),
        ),
        "a tombstone at the home slot must not hide the next colliding page"
    );

    let reupload_intent = first_intent.clone().with_frame(FrameIdentity::new(200));
    let reupload_requirements = first_requirements
        .rebind(&reupload_intent)
        .expect("the evicted collision body rebinds for exact replacement");
    let (reupload_capture, _, reuploaded_resources) = execute_and_compare(
        &mut gpu,
        churn_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &reupload_intent,
            requirements: &reupload_requirements,
            leases: &residency_offers[..1],
        },
        deadline,
    );
    assert_eq!(reuploaded_resources, 1);
    assert_eq!(
        (
            reupload_capture.rgba8(),
            reupload_capture.coverage(),
            reupload_capture.validity(),
        ),
        (
            first_capture.rgba8(),
            first_capture.coverage(),
            first_capture.validity(),
        ),
        "reusing the tombstoned home slot must restore the exact evicted page"
    );

    let final_neighbor_intent = second_intent.clone().with_frame(FrameIdentity::new(4));
    let final_neighbor_requirements = second_requirements
        .rebind(&final_neighbor_intent)
        .expect("the neighboring collision body rebinds after tombstone reuse");
    let (final_neighbor_capture, _, final_neighbor_uploads) = execute_and_compare(
        &mut gpu,
        first_target,
        ComparisonInput {
            catalog: &catalog,
            intent: &final_neighbor_intent,
            requirements: &final_neighbor_requirements,
            leases: &residency_offers[1..],
        },
        deadline,
    );
    assert_eq!(final_neighbor_uploads, 0);
    assert_eq!(
        (
            final_neighbor_capture.rgba8(),
            final_neighbor_capture.coverage(),
            final_neighbor_capture.validity(),
        ),
        (
            second_capture.rgba8(),
            second_capture.coverage(),
            second_capture.validity(),
        ),
        "tombstone reuse must not corrupt the adjacent colliding page"
    );
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the resident-body fixture begins bounded shutdown");
    drop(upload_offers);
    drop(residency_offers);
    drop(upload_leases);
    drop(collision_leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "the resident-body production regression exceeded its deadline"
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn coordinated_atomic_volume_strips_stay_hidden_and_match_the_direct_frame() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let leases = load_keys(
        &dataset_runtime,
        &fixtures.semantic[1],
        generation,
        deadline,
    );
    let offers = owned_leases(&fixtures.semantic[1], &leases, None);
    let mixed_keys = [fixtures.multichannel[0], fixtures.multichannel[1]];
    let mixed_leases = load_keys(&dataset_runtime, &mixed_keys, generation, deadline);
    let mixed_offers = owned_leases(&mixed_keys, &mixed_leases, None);

    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(FOUR_TARGET_GPU_BYTES)
            .expect("atomic volume fixture ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    let extent = RenderExtent::new(64, 65).expect("atomic volume extent is valid");
    let preview_extent = extent;
    request_target_layout(&mut gpu, PresentationTarget::ThreeD, preview_extent);
    gpu.offer_residency_leases(&offers)
        .expect("the atomic volume body is offered globally");
    gpu.offer_residency_leases(&mixed_offers)
        .expect("the atomic Mixed body is offered globally");

    let mode = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
    let (preview_intent, preview_requirements) = intent_and_requirements(
        200,
        1,
        mode,
        volume_view(),
        preview_extent,
        &fixtures.semantic[1],
    );
    let preview_intent = validated_fixture_intent(&catalog, &preview_intent, &preview_requirements);
    let preview_request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &preview_intent,
        &preview_requirements,
        200,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(extent, VolumeColorSchedule::InteractivePreview, false);
    let preview = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[preview_request])
        .expect("the complete native-resolution preview executes");
    let preview_report = preview
        .target(PresentationTarget::ThreeD)
        .expect("the complete preview has a target report");
    assert!(preview_report.presented());
    assert!(
        preview_report.validation_capture().is_none(),
        "a provisional preview cannot satisfy exact validation capture"
    );
    let visible_revision = preview_report.texture_revision();
    let full_candidate_layout = gpu
        .request_coordinated_layout(&[CoordinatedTargetLayout::new(
            PresentationTarget::ThreeD,
            extent,
        )])
        .expect("the full exact candidate is allocated behind the preview");
    assert_eq!(
        full_candidate_layout
            .target(PresentationTarget::ThreeD)
            .map(|state| state.extent()),
        Some(preview_extent),
        "resizing the private candidate must retain the visible preview allocation"
    );
    assert_eq!(
        gpu.inner.coordinated_3d_private_extents_for_test(),
        (preview_extent, extent, extent)
    );

    let (striped_intent, striped_requirements) =
        intent_and_requirements(201, 1, mode, volume_view(), extent, &fixtures.semantic[1]);
    let striped_intent = validated_fixture_intent(&catalog, &striped_intent, &striped_requirements);
    let striped_request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &striped_intent,
        &striped_requirements,
        201,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(
        extent,
        VolumeColorSchedule::AtomicRefinement {
            strip_height_pixels: 16,
        },
        false,
    );
    let stale_execution = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[striped_request])
        .expect("the soon-obsolete hidden exact job starts");
    let stale_report = stale_execution
        .target(PresentationTarget::ThreeD)
        .expect("the soon-obsolete hidden job has a target report");
    assert_eq!(stale_execution.color_queue_submissions(), 0);
    assert_eq!(
        stale_report
            .volume_refinement()
            .expect("the hidden job reports row progress")
            .completed_strips(),
        0
    );
    assert_eq!(
        stale_report
            .volume_refinement()
            .expect("the hidden job reports row progress")
            .total_strips(),
        extent.height_pixels()
    );
    assert!(!stale_report.presented());
    assert_eq!(stale_report.texture_revision(), visible_revision);
    assert_eq!(
        gpu.inner.coordinated_3d_private_extents_for_test(),
        (preview_extent, extent, extent),
        "hidden work must preserve both the visible preview and full private candidate"
    );
    assert!(
        !gpu.coordinated_target_requires_render_presentation(
            PresentationTarget::ThreeD,
            preview_extent,
            &preview_requirements,
            VolumeColorSchedule::InteractivePreview,
        )
        .expect("the matching preview-front query succeeds"),
        "a hidden exact candidate must not invalidate its matching visible preview"
    );
    assert!(
        gpu.coordinated_target_requires_render_presentation(
            PresentationTarget::ThreeD,
            extent,
            &striped_requirements,
            VolumeColorSchedule::AtomicRefinement {
                strip_height_pixels: 16,
            },
        )
        .expect("the exact-candidate query succeeds"),
        "the same target must still require its hidden exact result"
    );

    let (replacement_intent, replacement_requirements) =
        intent_and_requirements(202, 1, mode, volume_view(), extent, &fixtures.semantic[1]);
    let replacement_intent =
        validated_fixture_intent(&catalog, &replacement_intent, &replacement_requirements);
    let replacement_request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &replacement_intent,
        &replacement_requirements,
        202,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(
        extent,
        VolumeColorSchedule::AtomicRefinement {
            strip_height_pixels: 16,
        },
        false,
    );
    let replacement_held_request = replacement_request.with_hidden_promotion_authorized(false);
    let replacement_started = gpu
        .execute_coordinated_frame(
            &catalog,
            PresentationTarget::ThreeD,
            &[replacement_held_request],
        )
        .expect("the replacement hidden exact job starts once");
    assert_eq!(replacement_started.color_queue_submissions(), 0);
    assert!(
        !replacement_started
            .target(PresentationTarget::ThreeD)
            .expect("the replacement start has a target report")
            .presented()
    );
    while !gpu
        .poll_coordinated_hidden_refinement_ready(replacement_held_request)
        .expect("the autonomous hidden completion query succeeds")
    {
        assert!(
            Instant::now() < deadline,
            "the replacement hidden exact job exceeded its bounded deadline"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let withheld = gpu
        .execute_coordinated_frame(
            &catalog,
            PresentationTarget::ThreeD,
            &[replacement_held_request],
        )
        .expect("a complete but unauthorized candidate remains private");
    let withheld_report = withheld
        .target(PresentationTarget::ThreeD)
        .expect("the withheld exact candidate has a target report");
    let withheld_progress = withheld_report
        .volume_refinement()
        .expect("the withheld exact candidate reports complete row progress");
    assert_eq!(withheld_progress.completed_strips(), extent.height_pixels());
    assert_eq!(withheld_progress.total_strips(), extent.height_pixels());
    assert!(!withheld_report.presented());
    assert_eq!(withheld.color_queue_submissions(), 0);
    assert_eq!(withheld_report.texture_revision(), visible_revision);
    assert!(withheld_report.validation_capture().is_none());

    let published = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[replacement_request])
        .expect("the authorized complete exact candidate publishes once");
    let published_report = published
        .target(PresentationTarget::ThreeD)
        .expect("the authorized exact candidate has a target report");
    assert!(published_report.presented());
    assert_eq!(published.color_queue_submissions(), 1);
    let striped_capture = poll_coordinated_capture(
        &mut gpu,
        published_report
            .validation_capture()
            .expect("only the authorized exact candidate exposes validation pixels"),
        deadline,
    );
    let settled_layout = gpu
        .request_coordinated_layout(&[CoordinatedTargetLayout::new(
            PresentationTarget::ThreeD,
            extent,
        )])
        .expect("the old preview allocation is resized after the exact swap");
    assert_eq!(
        settled_layout
            .target(PresentationTarget::ThreeD)
            .map(|state| state.extent()),
        Some(extent)
    );

    let (direct_intent, direct_requirements) =
        intent_and_requirements(203, 1, mode, volume_view(), extent, &fixtures.semantic[1]);
    let direct = execute_coordinated_target(
        &mut gpu,
        PresentationTarget::ThreeD,
        &catalog,
        &direct_intent,
        &direct_requirements,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    );
    let direct_capture = poll_coordinated_capture(
        &mut gpu,
        direct
            .target(PresentationTarget::ThreeD)
            .and_then(|report| report.validation_capture())
            .expect("the ordinary exact pass exposes validation pixels"),
        deadline,
    );
    assert_eq!(striped_capture.extent(), direct_capture.extent());
    assert_eq!(striped_capture.rgba8(), direct_capture.rgba8());
    assert_eq!(striped_capture.coverage(), direct_capture.coverage());
    assert_eq!(striped_capture.validity(), direct_capture.validity());

    let dvr_state = mirante4d_domain::RenderState::dvr(
        SamplingPolicy::VoxelExact,
        DvrOpacityTransfer::new(
            DisplayWindow::new(0.0, 65_535.0).expect("the atomic DVR window is valid"),
            TransferCurve::linear(),
        ),
        0.05,
    )
    .expect("the atomic DVR state is valid");
    let (dvr_intent, dvr_requirements) = intent_and_requirements(
        204,
        1,
        dvr_state,
        volume_view(),
        extent,
        &fixtures.semantic[1],
    );
    assert_atomic_striped_capture_matches_direct(
        &mut gpu,
        &catalog,
        &dvr_intent,
        &dvr_requirements,
        FrameIdentity::new(205),
        extent,
        deadline,
        "DVR",
    );

    let iso_state =
        mirante4d_domain::RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.1)
            .expect("the atomic ISO state is valid");
    let (iso_intent, iso_requirements) = intent_and_requirements(
        206,
        1,
        iso_state,
        volume_view(),
        extent,
        &fixtures.semantic[1],
    );
    assert_atomic_striped_capture_matches_direct(
        &mut gpu,
        &catalog,
        &iso_intent,
        &iso_requirements,
        FrameIdentity::new(207),
        extent,
        deadline,
        "ISO",
    );

    let mixed_layers = || {
        vec![
            LayerRenderIntent::new(
                LogicalLayerKey::new(5),
                transfer_color_opacity(0.0, 1.0, [1.0, 0.0, 0.0], 0.4),
                mode,
            ),
            LayerRenderIntent::new(
                LogicalLayerKey::new(6),
                transfer_color_opacity(0.0, 1.0, [0.0, 1.0, 0.0], 0.6),
                iso_state,
            ),
        ]
    };
    let (mixed_intent, mixed_requirements) = multichannel_intent_and_requirements(
        208,
        volume_view_for([3.5, 3.5, 3.5], 0.125, 32.0),
        extent,
        mixed_layers(),
        &mixed_keys,
    );
    assert_atomic_striped_capture_matches_direct(
        &mut gpu,
        &catalog,
        &mixed_intent,
        &mixed_requirements,
        FrameIdentity::new(209),
        extent,
        deadline,
        "Mixed",
    );

    let (full_preview_intent, full_preview_requirements) =
        intent_and_requirements(210, 1, mode, volume_view(), extent, &fixtures.semantic[1]);
    let full_preview_intent =
        validated_fixture_intent(&catalog, &full_preview_intent, &full_preview_requirements);
    let full_preview_request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &full_preview_intent,
        &full_preview_requirements,
        210,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(extent, VolumeColorSchedule::InteractivePreview, false);
    let full_preview = gpu
        .execute_coordinated_frame(
            &catalog,
            PresentationTarget::ThreeD,
            &[full_preview_request],
        )
        .expect("the full-size provisional preview executes");
    let full_preview_report = full_preview
        .target(PresentationTarget::ThreeD)
        .expect("the full-size preview has a target report");
    assert!(full_preview_report.presented());
    assert!(
        full_preview_report.validation_capture().is_none(),
        "even a full-size preview remains provisional"
    );
    assert!(
        gpu.coordinated_target_requires_render_presentation(
            PresentationTarget::ThreeD,
            extent,
            &full_preview_requirements,
            VolumeColorSchedule::Direct,
        )
        .expect("the exact-promotion query succeeds"),
        "a matching full-size preview must not satisfy exact presentation"
    );

    let full_exact_request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &full_preview_intent,
        &full_preview_requirements,
        210,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(extent, VolumeColorSchedule::Direct, false);
    let full_exact = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[full_exact_request])
        .expect("the exact full-size promotion executes");
    let full_exact_report = full_exact
        .target(PresentationTarget::ThreeD)
        .expect("the exact full-size promotion has a target report");
    assert!(full_exact_report.presented());
    let full_exact_capture = full_exact_report
        .validation_capture()
        .expect("the exact full-size promotion owns a validation capture");
    assert_eq!(
        poll_coordinated_capture(&mut gpu, full_exact_capture, deadline).extent(),
        extent
    );
    assert!(
        !gpu.coordinated_target_requires_render_presentation(
            PresentationTarget::ThreeD,
            extent,
            &full_preview_requirements,
            VolumeColorSchedule::Direct,
        )
        .expect("the settled exact query succeeds"),
        "the real exact pass must settle the presentation"
    );

    assert!(
        !gpu.coordinated_target_requires_execution(PresentationTarget::ThreeD)
            .expect("the settled exact target remains configured"),
        "settled idle must submit no additional exact strips"
    );
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    dataset_runtime
        .request_shutdown()
        .expect("the atomic volume fixture begins bounded shutdown");
    drop(mixed_offers);
    drop(mixed_leases);
    drop(offers);
    drop(leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "the atomic volume production regression exceeded its deadline"
    );
}

#[allow(clippy::too_many_arguments)]
fn assert_atomic_striped_capture_matches_direct(
    gpu: &mut WgpuRenderRuntime,
    catalog: &DatasetCatalog,
    striped_intent: &RenderIntent,
    striped_requirements: &RenderRequirements,
    direct_frame: FrameIdentity,
    extent: RenderExtent,
    deadline: Instant,
    mode_label: &str,
) {
    let strip_height_pixels = 16;
    let striped_intent = validated_fixture_intent(catalog, striped_intent, striped_requirements);
    let request = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &striped_intent,
        striped_requirements,
        striped_intent.frame().get(),
        RetainedFrameRenderPolicy::ExactFrameOnly,
    )
    .with_volume_schedule(
        extent,
        VolumeColorSchedule::AtomicRefinement {
            strip_height_pixels,
        },
        false,
    );
    let striped_capture = loop {
        let execution = loop {
            assert!(
                Instant::now() < deadline,
                "the asynchronous atomic {mode_label} job exceeded its bounded deadline"
            );
            let execution = gpu
                .execute_coordinated_frame(catalog, PresentationTarget::ThreeD, &[request])
                .unwrap_or_else(|error| panic!("the atomic {mode_label} job executes: {error}"));
            let report = execution
                .target(PresentationTarget::ThreeD)
                .expect("the atomic mode job has a target report");
            if report.presented() || report.volume_refinement().is_some() {
                break execution;
            }
            assert_eq!(execution.color_queue_submissions(), 0);
            std::thread::sleep(Duration::from_millis(1));
        };
        let report = execution
            .target(PresentationTarget::ThreeD)
            .expect("the atomic mode job has a target report");
        let progress = report
            .volume_refinement()
            .expect("the atomic mode job reports hidden row progress");
        assert_eq!(progress.total_strips(), extent.height_pixels());
        if !report.presented() {
            assert_eq!(execution.color_queue_submissions(), 0);
            assert!(!report.presented());
            assert!(report.validation_capture().is_none());
            std::thread::sleep(Duration::from_millis(1));
        } else {
            assert_eq!(execution.color_queue_submissions(), 1);
            assert_eq!(progress.completed_strips(), extent.height_pixels());
            break poll_coordinated_capture(
                gpu,
                report
                    .validation_capture()
                    .expect("the completed atomic mode exposes validation pixels"),
                deadline,
            );
        }
    };

    let direct_intent = striped_intent.clone().with_frame(direct_frame);
    let direct_requirements = striped_requirements
        .rebind(&direct_intent)
        .expect("the atomic mode body rebinds to the direct comparison frame");
    let direct = execute_coordinated_target(
        gpu,
        PresentationTarget::ThreeD,
        catalog,
        &direct_intent,
        &direct_requirements,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    );
    let direct_capture = poll_coordinated_capture(
        gpu,
        direct
            .target(PresentationTarget::ThreeD)
            .and_then(|report| report.validation_capture())
            .expect("the direct mode comparison exposes validation pixels"),
        deadline,
    );
    assert_eq!(
        (
            striped_capture.extent(),
            striped_capture.rgba8(),
            striped_capture.coverage(),
            striped_capture.validity(),
        ),
        (
            direct_capture.extent(),
            direct_capture.rgba8(),
            direct_capture.coverage(),
            direct_capture.validity(),
        ),
        "atomic {mode_label} pixels must exactly match an ordinary direct frame"
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn coordinated_active_target_set_has_real_pixels_one_submit_and_idle_zero() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let u16_leases = load_keys(
        &dataset_runtime,
        &fixtures.semantic[1],
        generation,
        deadline,
    );
    let f32_leases = load_keys(
        &dataset_runtime,
        &fixtures.semantic[2],
        generation,
        deadline,
    );
    let u16_offers = owned_leases(&fixtures.semantic[1], &u16_leases, None);
    let f32_offers = owned_leases(&fixtures.semantic[2], &f32_leases, None);

    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(FOUR_TARGET_GPU_BYTES)
            .expect("coordinated fixture ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    let extent = RenderExtent::new(64, 64).expect("coordinated fixture extent is valid");
    let layout = PresentationTarget::ALL.map(|target| CoordinatedTargetLayout::new(target, extent));
    let layout_report = gpu
        .request_coordinated_layout(&layout)
        .expect("four fixed fronts and the private 3D candidate allocate transactionally");
    assert_eq!(layout_report.targets().len(), 4);

    gpu.offer_residency_leases(&u16_offers)
        .expect("the first distinct body is offered globally");
    let (prewarm_3d_intent, prewarm_3d_requirements) = intent_and_requirements(
        100,
        1,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view(),
        extent,
        &fixtures.semantic[1],
    );
    let prewarm_3d_intent =
        validated_fixture_intent(&catalog, &prewarm_3d_intent, &prewarm_3d_requirements);
    let prewarm_3d = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &prewarm_3d_intent,
        &prewarm_3d_requirements,
        100,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let first = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[prewarm_3d])
        .expect("the first body prewarms through the coordinator");
    assert_eq!(first.residency_queue_submissions(), 1);
    assert_eq!(first.color_queue_submissions(), 1);
    let _ = poll_coordinated_capture(
        &mut gpu,
        first
            .target(PresentationTarget::ThreeD)
            .and_then(|report| report.validation_capture())
            .expect("the first prewarm produces real pixels"),
        deadline,
    );

    gpu.offer_residency_leases(&f32_offers[..1])
        .expect("one first-useful resource is offered for the hidden candidate");
    let (incomplete_3d_intent, incomplete_3d_requirements) = intent_and_requirements(
        150,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view(),
        extent,
        &fixtures.semantic[2],
    );
    let incomplete_3d_intent =
        validated_fixture_intent(&catalog, &incomplete_3d_intent, &incomplete_3d_requirements);
    let incomplete_3d = CoordinatedTargetRequest::new(
        PresentationTarget::ThreeD,
        &incomplete_3d_intent,
        &incomplete_3d_requirements,
        150,
        RetainedFrameRenderPolicy::ExactFrameOnly,
    );
    let incomplete = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::ThreeD, &[incomplete_3d])
        .expect("the hidden 3D candidate admits its first useful resource");
    let incomplete_report = incomplete
        .target(PresentationTarget::ThreeD)
        .expect("the incomplete candidate has one report");
    assert_eq!(
        incomplete_report
            .progress()
            .map(FrameProgress::completeness),
        Some(FrameCompleteness::Progressive)
    );
    assert!(!incomplete_report.presented());
    assert_eq!(incomplete.color_queue_submissions(), 0);
    assert!(
        !gpu.coordinated_target_requires_execution(PresentationTarget::ThreeD)
            .expect("the desired 3D target remains configured"),
        "an incomplete hidden candidate with no relevant offer must sleep"
    );
    assert!(
        !gpu.has_pending_residency_work(),
        "required-removal/dirtiness is not pending residency work"
    );
    gpu.offer_residency_leases(&f32_offers[1..])
        .expect("the hidden candidate's remaining relevant resources are offered");
    assert!(
        gpu.coordinated_target_requires_execution(PresentationTarget::ThreeD)
            .expect("the desired 3D target remains configured"),
        "a relevant offer wakes the incomplete hidden candidate"
    );

    let half_angle = std::f64::consts::FRAC_PI_4;
    let xz_orientation = UnitQuaternion::new_xyzw(half_angle.sin(), 0.0, 0.0, half_angle.cos())
        .expect("the XZ fixture orientation is valid");
    let yz_orientation = UnitQuaternion::new_xyzw(0.0, half_angle.sin(), 0.0, half_angle.cos())
        .expect("the YZ fixture orientation is valid");
    let xz_view = RenderViewIntent::cross_section(
        CrossSectionView::new(
            WorldPoint3::new(15.5, 9.5, 15.5).expect("the XZ fixture center is finite"),
            xz_orientation,
            0.5,
            1.0,
        )
        .expect("the XZ fixture plane is valid"),
    );
    let (prewarm_xz_intent, prewarm_xz_requirements) = intent_and_requirements(
        101,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        xz_view,
        extent,
        &fixtures.semantic[2],
    );
    let prewarm_xz_intent =
        validated_fixture_intent(&catalog, &prewarm_xz_intent, &prewarm_xz_requirements);
    let prewarm_xz = CoordinatedTargetRequest::new(
        PresentationTarget::Xz,
        &prewarm_xz_intent,
        &prewarm_xz_requirements,
        101,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let second = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::Xz, &[prewarm_xz])
        .expect("the second body prewarms through the coordinator");
    assert_eq!(second.residency_queue_submissions(), 1);
    assert_eq!(second.color_queue_submissions(), 1);
    let _ = poll_coordinated_capture(
        &mut gpu,
        second
            .target(PresentationTarget::Xz)
            .and_then(|report| report.validation_capture())
            .expect("the second prewarm produces real pixels"),
        deadline,
    );

    let (three_d_intent, _) = intent_and_requirements(
        200,
        1,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        volume_view_for([14.5, 15.5, 15.5], 0.5, 40.0),
        extent,
        &fixtures.semantic[1],
    );
    let three_d_requirements = prewarm_3d_requirements
        .rebind(&three_d_intent)
        .expect("the resident 3D body rebinds to the new camera");
    let (xy_intent, _) = intent_and_requirements(
        200,
        1,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 7.5], 0.5),
        extent,
        &fixtures.semantic[1],
    );
    let xy_requirements = prewarm_3d_requirements
        .rebind(&xy_intent)
        .expect("the first resident body rebinds to XY");
    let (xz_intent, _) = intent_and_requirements(
        200,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        xz_view,
        extent,
        &fixtures.semantic[2],
    );
    let xz_requirements = prewarm_xz_requirements
        .rebind(&xz_intent)
        .expect("the second resident body rebinds to XZ");
    let yz_view = RenderViewIntent::cross_section(
        CrossSectionView::new(
            WorldPoint3::new(10.5, 15.5, 15.5).expect("the YZ fixture center is finite"),
            yz_orientation,
            0.5,
            1.0,
        )
        .expect("the YZ fixture plane is valid"),
    );
    let (yz_intent, _) = intent_and_requirements(
        200,
        2,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        yz_view,
        extent,
        &fixtures.semantic[2],
    );
    let yz_requirements = prewarm_xz_requirements
        .rebind(&yz_intent)
        .expect("the second resident body rebinds to YZ");
    let three_d_intent = validated_fixture_intent(&catalog, &three_d_intent, &three_d_requirements);
    let xy_intent = validated_fixture_intent(&catalog, &xy_intent, &xy_requirements);
    let xz_intent = validated_fixture_intent(&catalog, &xz_intent, &xz_requirements);
    let yz_intent = validated_fixture_intent(&catalog, &yz_intent, &yz_requirements);
    let active_group = CoordinatedPublicationGroup::exact_target_set(
        mirante4d_render_api::PresentationTargetSet::ALL,
    )
    .expect("the current active four-target set forms one exact group");
    let requests = [
        CoordinatedTargetRequest::new(
            PresentationTarget::ThreeD,
            &three_d_intent,
            &three_d_requirements,
            200,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .with_atomic_publication_group(active_group),
        CoordinatedTargetRequest::new(
            PresentationTarget::Xy,
            &xy_intent,
            &xy_requirements,
            200,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .with_atomic_publication_group(active_group),
        CoordinatedTargetRequest::new(
            PresentationTarget::Xz,
            &xz_intent,
            &xz_requirements,
            200,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .with_atomic_publication_group(active_group),
        CoordinatedTargetRequest::new(
            PresentationTarget::Yz,
            &yz_intent,
            &yz_requirements,
            200,
            RetainedFrameRenderPolicy::ExactFrameOnly,
        )
        .with_atomic_publication_group(active_group),
    ];

    let malformed_before = (
        gpu.diagnostics().queue_submissions(),
        gpu.diagnostics().uploaded_resources(),
        gpu.diagnostics().uploaded_payload_bytes(),
        gpu.diagnostics().residency_evictions(),
        gpu.diagnostics().directory_mutations(),
        gpu.diagnostics().payload_arena_allocated_bytes(),
        gpu.diagnostics().explicit_staging_allocations(),
        gpu.diagnostics().control_buffer_allocations(),
        gpu.diagnostics().bind_group_creations(),
        gpu.diagnostics().usable_pipeline_handles(),
        gpu.diagnostics().cold_coverage_membership_checks(),
        gpu.diagnostics().allocator_plans(),
    );
    let malformed = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::Xy, &requests[..3])
        .expect_err("an incomplete declared physical group is malformed");
    assert_eq!(
        malformed,
        WgpuRenderRuntimeError::InvalidCoordinatedPublicationGroup
    );

    let diagnostics_before = (
        gpu.diagnostics().queue_submissions(),
        gpu.diagnostics().uploaded_resources(),
        gpu.diagnostics().uploaded_payload_bytes(),
        gpu.diagnostics().residency_evictions(),
        gpu.diagnostics().directory_mutations(),
        gpu.diagnostics().payload_arena_allocated_bytes(),
        gpu.diagnostics().explicit_staging_allocations(),
        gpu.diagnostics().control_buffer_allocations(),
        gpu.diagnostics().bind_group_creations(),
        gpu.diagnostics().usable_pipeline_handles(),
        gpu.diagnostics().cold_coverage_membership_checks(),
        gpu.diagnostics().allocator_plans(),
    );
    assert_eq!(
        diagnostics_before, malformed_before,
        "group validation happens before target mutation, upload, allocation, or submission"
    );
    let coordinated = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::Xy, &requests)
        .expect("the resident four-target cutoff executes");
    assert_eq!(
        coordinated.recorded_targets(),
        &[
            PresentationTarget::Xy,
            PresentationTarget::Xz,
            PresentationTarget::Yz,
            PresentationTarget::ThreeD,
        ],
        "the active target records first, related 2D targets next, and passive 3D last"
    );
    assert_eq!(coordinated.residency_queue_submissions(), 0);
    assert_eq!(coordinated.color_queue_submissions(), 1);
    for target in PresentationTarget::ALL {
        let report = coordinated
            .target(target)
            .expect("every requested fixed target has one report");
        assert!(
            report.presented(),
            "{target:?} did not produce current pixels"
        );
        assert_eq!(report.visited_resources(), 0);
        assert_eq!(report.uploaded_resources(), 0);
        assert_eq!(report.payload_upload_bytes(), 0);
        assert_eq!(report.residency_queue_submissions(), 0);
        assert_eq!(
            report.progress().map(FrameProgress::completeness),
            Some(FrameCompleteness::Exact)
        );
    }
    let diagnostics_after = gpu.diagnostics();
    assert_eq!(
        diagnostics_after.queue_submissions(),
        diagnostics_before.0 + 1
    );
    assert_eq!(diagnostics_after.uploaded_resources(), diagnostics_before.1);
    assert_eq!(
        diagnostics_after.uploaded_payload_bytes(),
        diagnostics_before.2
    );
    assert_eq!(
        diagnostics_after.residency_evictions(),
        diagnostics_before.3
    );
    assert_eq!(
        diagnostics_after.directory_mutations(),
        diagnostics_before.4
    );
    assert_eq!(
        diagnostics_after.payload_arena_allocated_bytes(),
        diagnostics_before.5
    );
    assert_eq!(
        diagnostics_after.explicit_staging_allocations(),
        diagnostics_before.6
    );
    assert_eq!(
        diagnostics_after.control_buffer_allocations(),
        diagnostics_before.7
    );
    assert_eq!(
        diagnostics_after.bind_group_creations(),
        diagnostics_before.8
    );
    assert_eq!(
        diagnostics_after.usable_pipeline_handles(),
        diagnostics_before.9
    );
    assert_eq!(
        diagnostics_after.cold_coverage_membership_checks(),
        diagnostics_before.10,
        "same-body resident targets, including the private 3D candidate, must not reseed coverage"
    );
    assert_eq!(
        diagnostics_after.allocator_plans(),
        diagnostics_before.11,
        "same-body resident targets, including the private 3D candidate, must bypass allocator planning"
    );

    let captures = PresentationTarget::ALL.map(|target| {
        poll_coordinated_capture(
            &mut gpu,
            coordinated
                .target(target)
                .and_then(|report| report.validation_capture())
                .unwrap_or_else(|| panic!("{target:?} has one real validation capture")),
            deadline,
        )
    });
    let intents = [&three_d_intent, &xy_intent, &xz_intent, &yz_intent];
    let reference_leases = [&u16_offers, &u16_offers, &f32_offers, &f32_offers];
    for ((target, capture), (intent, leases)) in PresentationTarget::ALL
        .into_iter()
        .zip(captures.iter())
        .zip(intents.into_iter().zip(reference_leases))
    {
        assert!(
            capture
                .rgba8()
                .chunks_exact(4)
                .any(|pixel| pixel[3] != 0 && pixel[..3].iter().any(|channel| *channel != 0)),
            "{target:?} capture is blank"
        );
        let lease_refs = leases.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let reference = ReferenceRenderer::new()
            .render(&catalog, intent, &lease_refs)
            .expect("the independent reference renders each coordinated target");
        let _ = compare_reference(capture, &reference);
    }
    for left in 0..captures.len() {
        for right in left + 1..captures.len() {
            assert_ne!(
                captures[left].rgba8(),
                captures[right].rgba8(),
                "coordinated targets {left} and {right} unexpectedly alias the same pixels"
            );
        }
    }

    let submissions_before_idle = gpu.diagnostics().queue_submissions();
    let allocator_plans_before_idle = gpu.diagnostics().allocator_plans();
    let idle = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::Xy, &requests)
        .expect("an identical settled cutoff is accepted");
    assert_eq!(idle.residency_queue_submissions(), 0);
    assert_eq!(idle.color_queue_submissions(), 0);
    assert!(idle.recorded_targets().is_empty());
    assert!(idle.targets().iter().all(|report| {
        !report.presented()
            && report.uploaded_resources() == 0
            && report.residency_queue_submissions() == 0
            && report.validation_capture().is_none()
    }));
    assert_eq!(
        gpu.diagnostics().queue_submissions(),
        submissions_before_idle,
        "settled idle must submit no renderer work"
    );
    assert_eq!(
        gpu.diagnostics().allocator_plans(),
        allocator_plans_before_idle,
        "settled idle must not enter allocator planning"
    );

    dataset_runtime
        .request_shutdown()
        .expect("the coordinated fixture begins bounded shutdown");
    drop(u16_offers);
    drop(f32_offers);
    drop(u16_leases);
    drop(f32_leases);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn cross_target_upload_after_progress_snapshot_remains_dirty_until_consumed() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let keys = &fixtures.semantic[1][..2];
    let leases = load_keys(
        &dataset_runtime,
        keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 1),
        deadline,
    );
    let offers = owned_leases(keys, &leases, None);
    let extent = RenderExtent::new(64, 64).expect("cross-target fixture extent is valid");
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES).unwrap(),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    gpu.request_coordinated_layout(&[
        CoordinatedTargetLayout::new(PresentationTarget::Xy, extent),
        CoordinatedTargetLayout::new(PresentationTarget::Xz, extent),
    ])
    .expect("the two linked targets allocate transactionally");

    let (xz_intent, xz_requirements) = intent_and_requirements(
        600,
        1,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 15.5], 0.5),
        extent,
        keys,
    );
    gpu.offer_residency_leases(&offers[..1])
        .expect("the XZ first-useful resource is offered");
    let first = execute_coordinated_target(
        &mut gpu,
        PresentationTarget::Xz,
        &catalog,
        &xz_intent,
        &xz_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let first_xz = first
        .target(PresentationTarget::Xz)
        .expect("the first XZ report exists");
    assert!(first_xz.presented());
    assert_eq!(
        first_xz.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Progressive)
    );

    let (xy_intent, xy_requirements) = intent_and_requirements(
        601,
        1,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([15.5, 15.5, 15.5], 0.5),
        extent,
        &keys[1..],
    );
    gpu.offer_residency_leases(&offers[1..])
        .expect("the shared successor resource is offered");
    let xy_intent = validated_fixture_intent(&catalog, &xy_intent, &xy_requirements);
    let xz_intent = validated_fixture_intent(&catalog, &xz_intent, &xz_requirements);
    let xy_request = CoordinatedTargetRequest::new(
        PresentationTarget::Xy,
        &xy_intent,
        &xy_requirements,
        601,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let xz_request = CoordinatedTargetRequest::new(
        PresentationTarget::Xz,
        &xz_intent,
        &xz_requirements,
        600,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let shared_cutoff = gpu
        .execute_coordinated_frame(&catalog, PresentationTarget::Xy, &[xy_request, xz_request])
        .expect("the active XY target uploads the XZ successor resource");
    assert_eq!(shared_cutoff.residency_queue_submissions(), 1);
    assert_eq!(
        shared_cutoff
            .target(PresentationTarget::Xz)
            .and_then(|report| report.progress())
            .map(FrameProgress::completeness),
        Some(FrameCompleteness::Progressive),
        "XZ's prepared progress snapshot predates the cross-target upload"
    );
    assert!(
        gpu.coordinated_target_requires_execution(PresentationTarget::Xz)
            .expect("XZ remains configured"),
        "publishing the older useful XZ pixels must not erase the newer residency addition"
    );

    let settled = execute_coordinated_target(
        &mut gpu,
        PresentationTarget::Xz,
        &catalog,
        &xz_intent,
        &xz_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let settled_xz = settled
        .target(PresentationTarget::Xz)
        .expect("the settled XZ report exists");
    assert!(settled_xz.presented());
    assert_eq!(
        settled_xz.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    assert!(
        !gpu.coordinated_target_requires_execution(PresentationTarget::Xz)
            .expect("XZ remains configured"),
        "the exact XZ successor consumes the cross-target residency addition"
    );

    dataset_runtime.request_shutdown().unwrap();
    drop(offers);
    drop(leases);
    drop(dataset_runtime);
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn coordinated_target_pins_are_union_scoped_and_layout_retirement_releases_them() {
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
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES).unwrap(),
        None,
        TestPipelineAdmission::Ready,
    );
    let renderer_events = Arc::new(Mutex::new(VecDeque::new()));
    let event_queue = Arc::clone(&renderer_events);
    gpu.set_renderer_event_sink(RendererEventSink::new(move |event| {
        event_queue
            .lock()
            .expect("the renderer test event queue is never poisoned")
            .push_back(event);
    }));
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let presentations = PresentationTarget::ALL;
    let layout = presentations.map(|target| CoordinatedTargetLayout::new(target, extent));
    gpu.request_coordinated_layout(&layout)
        .expect("the four fixed pin targets allocate transactionally");

    let execute =
        |gpu: &mut WgpuRenderRuntime, target: PresentationTarget, frame: u64, key: BrickKey| {
            let view = if target == PresentationTarget::ThreeD {
                volume_view_for([96.0, 96.0, 32.0], 1.0, 256.0)
            } else {
                cross_section_view([96.0, 96.0, 32.0], 1.0)
            };
            let (intent, requirements) = intent_and_requirements(
                frame,
                3,
                mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
                view,
                extent,
                &[key],
            );
            let lease_offer = Arc::new(leases.get(&key).expect("pin fixture lease exists").clone())
                as Arc<dyn ResourceLease>;
            loop {
                gpu.offer_residency_leases(&[Arc::clone(&lease_offer)])
                    .expect("pin fixture lease is offered");
                let cutoff = execute_coordinated_target(
                    gpu,
                    target,
                    &catalog,
                    &intent,
                    &requirements,
                    RetainedFrameRenderPolicy::EveryUsefulFrame,
                );
                let report = cutoff
                    .target(target)
                    .expect("the pin target has one report")
                    .clone();
                if !report.deferred_by_backpressure() {
                    let submitted_through = cutoff
                        .submitted_through_event()
                        .expect("the accepted pin fixture report names its final submission");
                    break (report, submitted_through);
                }
                assert!(Instant::now() < deadline, "pin fixture staging timed out");
                std::thread::yield_now();
            }
        };

    for (index, target) in presentations.into_iter().enumerate() {
        let (report, _) = execute(&mut gpu, target, 700 + index as u64, fixtures.upload[index]);
        assert_eq!(report.newly_resident_keys(), &[fixtures.upload[index]]);
    }
    for index in 4..9 {
        let (report, submitted_through) = execute(
            &mut gpu,
            presentations[0],
            700 + index as u64,
            fixtures.upload[index],
        );
        if index == 7 {
            // Establish a real queue-completion boundary before asking which
            // inactive page is oldest. Temporary submission leases are an
            // implementation detail and must not make this semantic pin test
            // depend on how quickly the workstation drains its queue.
            wait_for_submission_completion(&mut gpu, &renderer_events, submitted_through, deadline);
        }
        if index == 8 {
            assert_eq!(
                report.evicted_keys(),
                &[fixtures.upload[0]],
                "the oldest inactive page leaves while every live presentation keeps its current resource pinned"
            );
        }
    }
    let mut warm_submitted_through = None;
    for (index, target) in presentations.into_iter().enumerate().take(4).skip(1) {
        let (warm, submitted_through) =
            execute(&mut gpu, target, 720 + index as u64, fixtures.upload[index]);
        assert_eq!(warm.uploaded_resources(), 0);
        assert!(warm.evicted_keys().is_empty());
        warm_submitted_through = Some(submitted_through);
    }
    wait_for_submission_completion(
        &mut gpu,
        &renderer_events,
        warm_submitted_through.expect("the final warm revisit names its submission"),
        deadline,
    );

    gpu.request_coordinated_layout(&[CoordinatedTargetLayout::new(presentations[0], extent)])
        .expect("omitted fixed targets retire their frame leases and pins");
    let (replacement, _) = execute(&mut gpu, presentations[0], 730, fixtures.upload[0]);
    assert_eq!(replacement.newly_resident_keys(), &[fixtures.upload[0]]);
    assert_eq!(
        replacement.evicted_keys(),
        &[fixtures.upload[4]],
        "warm revisits refresh nonempty payload age; the oldest untouched inactive page leaves first"
    );

    assert!(gpu.resident_payload_bytes() > 0);

    gpu.retire_dataset_generation();

    assert_eq!(gpu.resident_payload_bytes(), 0);
    assert_eq!(gpu.diagnostics().empty_resident_metadata_records(), 0);
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let (replacement_generation, _) = execute(&mut gpu, presentations[0], 740, fixtures.upload[0]);
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
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES).unwrap(),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let target = PresentationTarget::Xy;
    request_target_layout(&mut gpu, target, extent);

    let execute = |gpu: &mut WgpuRenderRuntime, frame: u64, key: BrickKey| {
        let (intent, requirements) = intent_and_requirements(
            frame,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view([96.0, 96.0, 32.0], 1.0),
            extent,
            &[key],
        );
        let lease_offer = Arc::new(leases.get(&key).expect("LRU fixture lease exists").clone())
            as Arc<dyn ResourceLease>;
        loop {
            gpu.offer_residency_leases(&[Arc::clone(&lease_offer)])
                .expect("LRU fixture lease is offered");
            let cutoff = execute_coordinated_target(
                gpu,
                target,
                &catalog,
                &intent,
                &requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let report = cutoff
                .target(target)
                .expect("the LRU target has one report")
                .clone();
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

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn segmented_payload_upload_and_sampling_cross_the_first_binding() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let semantic_key = fixtures.semantic[0][0];
    let mut keys = fixtures.upload[..6].to_vec();
    keys.push(semantic_key);
    let leases = load_keys(
        &dataset_runtime,
        &keys,
        CancellationGeneration::for_scope(REQUEST_SCOPE, 18),
        deadline,
    );
    let extent = RenderExtent::new(1, 1).unwrap();
    let mut gpu = test_gpu_runtime_with_initial_commitment(
        WgpuRenderRuntimeConfig::new(SMALL_GPU_BYTES)
            .unwrap()
            .with_validation_capture(true),
        Some(4 * MIB),
        Some(MIB),
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
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
    assert_eq!(gpu.diagnostics().payload_committed_capacity_bytes(), MIB);
    assert!(
        gpu.diagnostics().payload_arena_allocated_bytes()
            < gpu.diagnostics().payload_capacity_bytes()
    );
    let target = PresentationTarget::Xy;
    request_target_layout(&mut gpu, target, extent);

    for (index, key) in fixtures.upload[..4].iter().copied().enumerate() {
        let origin = key.region().origin();
        let center = [
            origin[2] as f64 + 31.5,
            origin[1] as f64 + 31.5,
            origin[0] as f64 + 31.5,
        ];
        let (intent, requirements) = intent_and_requirements(
            900 + index as u64 * 10,
            3,
            mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            cross_section_view(center, 1.0),
            extent,
            &[key],
        );
        let lease_offer = Arc::new(leases.get(&key).unwrap().clone()) as Arc<dyn ResourceLease>;
        let report = loop {
            gpu.offer_residency_leases(&[Arc::clone(&lease_offer)])
                .expect("segmented payload lease is offered");
            let cutoff = execute_coordinated_target(
                &mut gpu,
                target,
                &catalog,
                &intent,
                &requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let report = cutoff
                .target(target)
                .expect("the segmented target has one report")
                .clone();
            if !report.deferred_by_backpressure() {
                break report;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        assert_eq!(report.uploaded_resources(), 1);
        assert!(report.evicted_keys().is_empty());
        let _ = poll_coordinated_capture(&mut gpu, report.validation_capture().unwrap(), deadline);

        if index == 0 {
            let mut growth_union = fixtures.upload[..4].to_vec();
            growth_union.sort();
            gpu.ensure_global_requirement_union(&catalog, &growth_union)
                .expect("an empty segment opens before copying the populated first segment");
            assert_eq!(gpu.diagnostics().payload_growths(), 1);
            assert_eq!(
                gpu.diagnostics().payload_committed_capacity_bytes(),
                4 * MIB,
                "the empty segment commits only its three-MiB placement high-watermark"
            );
            assert_eq!(
                gpu.diagnostics().payload_growth_copy_bytes(),
                0,
                "opening an empty logical segment requires no preservation copy"
            );

            let (revisit_intent, revisit_requirements) = intent_and_requirements(
                901,
                3,
                mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
                cross_section_view(center, 1.0),
                extent,
                &[key],
            );
            let revisit = execute_coordinated_target(
                &mut gpu,
                target,
                &catalog,
                &revisit_intent,
                &revisit_requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let revisit_report = revisit
                .target(target)
                .expect("the preserved payload revisit has one report");
            assert_eq!(revisit_report.uploaded_resources(), 0);
            let preserved = poll_coordinated_capture(
                &mut gpu,
                revisit_report
                    .validation_capture()
                    .expect("the preserved payload revisit exposes pixels"),
                deadline,
            );
            assert_eq!(preserved.coverage(), &[1]);
            assert_eq!(preserved.validity(), &[1]);
        }
    }

    let mut full_union = fixtures.upload[..4].to_vec();
    full_union.push(semantic_key);
    full_union.sort();
    gpu.ensure_global_requirement_union(&catalog, &full_union)
        .expect("the next logical segment opens without reallocating full capacity");
    assert!(gpu.diagnostics().payload_growths() >= 2);
    assert!(
        gpu.diagnostics().payload_committed_capacity_bytes()
            < gpu.diagnostics().payload_capacity_bytes()
    );

    let semantic_lease =
        Arc::new(leases.get(&semantic_key).unwrap().clone()) as Arc<dyn ResourceLease>;
    let (intent, requirements) = intent_and_requirements(
        1_000,
        0,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([7.5, 7.5, 7.5], 1.0),
        extent,
        &[semantic_key],
    );
    let (capture, _, uploaded_resources) = execute_and_compare(
        &mut gpu,
        target,
        ComparisonInput {
            catalog: &catalog,
            intent: &intent,
            requirements: &requirements,
            leases: &[semantic_lease],
        },
        deadline,
    );
    assert_eq!(capture.coverage(), &[1]);
    assert_eq!(capture.validity(), &[1]);
    assert_eq!(uploaded_resources, 1);
    assert!(gpu.diagnostics().resident_payload_bytes() > 4 * MIB);

    let mut populated_growth_union = fixtures.upload[..6].to_vec();
    populated_growth_union.push(semantic_key);
    populated_growth_union.sort();
    gpu.ensure_global_requirement_union(&catalog, &populated_growth_union)
        .expect("demand beyond opened segments grows one populated prefix");
    assert!(gpu.diagnostics().payload_growths() >= 3);
    let committed = gpu.diagnostics().payload_committed_capacity_bytes();
    assert!(
        committed >= 6 * MIB && committed < gpu.diagnostics().payload_capacity_bytes(),
        "the exact union high-watermarks must fit without committing the logical maximum; \
         committed={committed}"
    );
    assert!(
        gpu.diagnostics().payload_growth_copy_bytes() >= MIB,
        "populated growth must preserve the resident first payload"
    );

    let first_key = fixtures.upload[0];
    let first_origin = first_key.region().origin();
    let first_center = [
        first_origin[2] as f64 + 31.5,
        first_origin[1] as f64 + 31.5,
        first_origin[0] as f64 + 31.5,
    ];
    let (revisit_intent, revisit_requirements) = intent_and_requirements(
        1_001,
        3,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view(first_center, 1.0),
        extent,
        &[first_key],
    );
    let revisit = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &revisit_intent,
        &revisit_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let revisit_report = revisit
        .target(target)
        .expect("the copied payload revisit has one report");
    assert_eq!(revisit_report.uploaded_resources(), 0);
    let preserved = poll_coordinated_capture(
        &mut gpu,
        revisit_report
            .validation_capture()
            .expect("the copied payload revisit exposes pixels"),
        deadline,
    );
    assert_eq!(preserved.coverage(), &[1]);
    assert_eq!(preserved.validity(), &[1]);

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
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .unwrap()
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let target = PresentationTarget::ThreeD;
    request_target_layout(&mut gpu, target, extent);
    let lease_offer = Arc::new(leases.get(&key).unwrap().clone()) as Arc<dyn ResourceLease>;
    gpu.offer_residency_leases(&[lease_offer])
        .expect("far-voxel lease is offered");
    let cutoff = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &intent,
        &requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let report = cutoff
        .target(target)
        .expect("the far-ray target has one report");
    let capture =
        poll_coordinated_capture(&mut gpu, report.validation_capture().unwrap(), deadline);
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
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(64 * MIB)
            .expect("multichannel GPU ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    let presentation = PresentationTarget::ThreeD;
    let section_presentation = PresentationTarget::Xy;
    gpu.request_coordinated_layout(&[
        CoordinatedTargetLayout::new(presentation, extent),
        CoordinatedTargetLayout::new(section_presentation, extent),
    ])
    .expect("the volume and cross-section multichannel targets allocate");
    let key_for = |layer: u32| fixtures.multichannel[(layer - 5) as usize];
    let leases_for = |layers: &[u32]| {
        layers
            .iter()
            .map(|layer| {
                Arc::new(
                    leases
                        .get(&key_for(*layer))
                        .expect("multichannel lease exists")
                        .clone(),
                ) as Arc<dyn ResourceLease>
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            section_presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &leases_for(&[5]),
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
        let (capture, delta, _) = execute_and_compare(
            &mut gpu,
            presentation,
            ComparisonInput {
                catalog: &catalog,
                intent: &intent,
                requirements: &requirements,
                leases: &case_leases,
            },
            deadline,
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
    let pick_lease = Arc::new(
        leases
            .get(&pick_key)
            .expect("pick fixture lease exists")
            .clone(),
    ) as Arc<dyn ResourceLease>;
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
        gpu.offer_residency_leases(&[Arc::clone(&pick_lease)])
            .expect("the multichannel pick lease is offered");
        let cutoff = execute_coordinated_target(
            &mut gpu,
            presentation,
            &catalog,
            &intent,
            &requirements,
            RetainedFrameRenderPolicy::EveryUsefulFrame,
        );
        let report = cutoff
            .target(presentation)
            .expect("the multichannel pick target has one report");
        let presented = presented_frame(presentation, &intent, report);
        let _ = poll_coordinated_capture(
            &mut gpu,
            report
                .validation_capture()
                .expect("the multichannel pick capture exists"),
            deadline,
        );
        let query = VolumePickQuery::new(
            &presented,
            TimeIndex::new(0),
            LogicalLayerKey::new(5),
            [31.5, 31.5],
            policy,
        )
        .expect("the multichannel pick query is valid");
        let ticket = gpu
            .request_coordinated_pick(presentation, query)
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
    let dvr_pick_borrowed = owned_leases(&fixtures.semantic[2], &dvr_pick_leases, None);
    gpu.offer_residency_leases(&dvr_pick_borrowed)
        .expect("partial-opacity DVR pick leases are offered");
    let dvr_pick_cutoff = execute_coordinated_target(
        &mut gpu,
        presentation,
        &catalog,
        &dvr_pick_intent,
        &dvr_pick_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let dvr_pick_report = dvr_pick_cutoff
        .target(presentation)
        .expect("the partial-opacity DVR target has one report");
    let dvr_pick_presented = presented_frame(presentation, &dvr_pick_intent, dvr_pick_report);
    let _ = poll_coordinated_capture(
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
        .request_coordinated_pick(presentation, dvr_pick_query)
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
    gpu.offer_residency_leases(&[Arc::clone(&pick_lease)])
        .expect("SmoothLinear pick lease is offered idempotently");
    let smooth_cutoff = execute_coordinated_target(
        &mut gpu,
        presentation,
        &catalog,
        &smooth_pick_intent,
        &smooth_pick_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let smooth_report = smooth_cutoff
        .target(presentation)
        .expect("the SmoothLinear target has one report");
    let smooth_presented = presented_frame(presentation, &smooth_pick_intent, smooth_report);
    let _ = poll_coordinated_capture(
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
        .request_coordinated_pick(presentation, smooth_query)
        .expect("SmoothLinear async pick is accepted");
    let smooth_result = poll_pick(&mut gpu, smooth_ticket, deadline);
    assert_eq!(smooth_result.kind(), VolumePickHitKind::InterpolatedSample);
    assert_eq!(
        smooth_result.value(),
        Some(VolumePickValue::IntensityF32(0.25))
    );
    assert_eq!(smooth_result.completeness(), VolumePickCompleteness::Exact);

    assert_eq!(gpu.diagnostics().validation_error_count(), 0);
    dataset_runtime
        .request_shutdown()
        .expect("multichannel fixture runtime begins bounded shutdown");
    drop(leases);
    drop(dvr_pick_leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "the multichannel GPU check exceeded its deadline"
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn terminal_full_volume_fast_path_matches_reference_for_all_volume_modes_and_sampling() {
    let deadline = Instant::now() + Duration::from_secs(60);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 31);
    let mut keys = fixtures.terminal_sparse.clone();
    keys.push(fixtures.terminal);
    let leases = load_keys(&dataset_runtime, &keys, generation, deadline);
    let fast_offers = owned_leases(&[fixtures.terminal], &leases, None);
    let sparse_offers = owned_leases(&fixtures.terminal_sparse, &leases, None);
    let extent = RenderExtent::new(48, 49).expect("terminal conformance extent is valid");
    let target = PresentationTarget::ThreeD;
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(PERFORMANCE_GPU_BYTES)
            .expect("terminal conformance ledger is valid")
            .with_validation_capture(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    request_target_layout(&mut gpu, target, extent);

    let mut cases = Vec::new();
    for sampling in [SamplingPolicy::VoxelExact, SamplingPolicy::SmoothLinear] {
        cases.push((
            format!("MIP-{sampling:?}"),
            mirante4d_domain::RenderState::mip(sampling),
        ));
        cases.push((
            format!("DVR-{sampling:?}"),
            mirante4d_domain::RenderState::dvr(
                sampling,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 1.0).unwrap(),
                    TransferCurve::linear(),
                ),
                0.05,
            )
            .unwrap(),
        ));
        cases.push((
            format!("ISO-{sampling:?}"),
            mirante4d_domain::RenderState::iso(sampling, IsoShadingPolicy::Flat, 0.5).unwrap(),
        ));
    }

    for (index, (label, state)) in cases.into_iter().enumerate() {
        eprintln!("terminal fast-path conformance case={label}");
        let sparse_frame = 1_100 + (index as u64) * 2;
        let (sparse_intent, sparse_requirements) = intent_and_requirements(
            sparse_frame,
            14,
            state,
            volume_view_for([31.5, 31.5, 31.5], 1.0, 128.0),
            extent,
            &fixtures.terminal_sparse,
        );
        let sparse_capture = execute_and_capture(
            &mut gpu,
            target,
            &catalog,
            &sparse_intent,
            &sparse_requirements,
            &sparse_offers,
            deadline,
        );
        let sparse_lease_refs = sparse_offers.iter().map(Arc::as_ref).collect::<Vec<_>>();
        let reference = ReferenceRenderer::new()
            .render(&catalog, &sparse_intent, &sparse_lease_refs)
            .expect("the independent reference renders the sparse terminal body");
        assert_exact_bytes(
            &format!("{label} sparse/reference coverage"),
            sparse_capture.coverage(),
            reference.coverage(),
        );
        assert_exact_bytes(
            &format!("{label} sparse/reference validity"),
            sparse_capture.validity(),
            reference.validity(),
        );
        let max_delta = sparse_capture
            .rgba8()
            .iter()
            .zip(reference.rgba8())
            .map(|(actual, expected)| actual.abs_diff(*expected))
            .max()
            .unwrap_or(0);
        // ISO first-hit color is discontinuous at the synthetic threshold;
        // the dedicated affine ISO oracle below owns that numerical contract.
        // This test still requires exact fact agreement with the reference
        // and exact fast-path agreement with the ordinary sparse ISO kernel.
        if state.mode() != mirante4d_domain::RenderMode::Isosurface {
            assert!(
                max_delta <= 2,
                "{label} ordinary sparse/reference color delta was {max_delta}"
            );
        }
        let (fast_intent, fast_requirements) = intent_and_requirements(
            sparse_frame + 1,
            13,
            state,
            volume_view_for([31.5, 31.5, 31.5], 1.0, 128.0),
            extent,
            &[fixtures.terminal],
        );
        let fast_capture = execute_and_capture(
            &mut gpu,
            target,
            &catalog,
            &fast_intent,
            &fast_requirements,
            &fast_offers,
            deadline,
        );
        assert_exact_bytes(
            &format!("{label} fast/sparse coverage"),
            fast_capture.coverage(),
            sparse_capture.coverage(),
        );
        assert_exact_bytes(
            &format!("{label} fast/sparse validity"),
            fast_capture.validity(),
            sparse_capture.validity(),
        );
        let fast_sparse_delta = fast_capture
            .rgba8()
            .iter()
            .zip(sparse_capture.rgba8())
            .map(|(fast, sparse)| fast.abs_diff(*sparse))
            .max()
            .unwrap_or(0);
        assert!(
            fast_sparse_delta <= 1,
            "{label} terminal fast/sparse color delta was {fast_sparse_delta}"
        );
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    drop(fast_offers);
    drop(sparse_offers);
    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "terminal full-volume conformance exceeded its deadline"
    );
}

#[test]
#[ignore = "requires a trusted Vulkan GPU with timestamp-query support for measurements"]
fn native_1080p_terminal_navigation_gpu_timing() {
    const GLOBAL_PIPELINE_WARMUP_FRAMES: usize = 30;
    const CASE_WARMUP_FRAMES: usize = 30;
    const MEASURED_TRIALS: usize = 120;
    const PREFERRED_60_HZ_COMPONENT_NS: u64 = 16_667_000;
    const ABSOLUTE_30_HZ_COMPONENT_NS: u64 = 33_300_000;

    let deadline = Instant::now() + Duration::from_secs(900);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 32);
    let leases = load_keys(&dataset_runtime, &[fixtures.terminal], generation, deadline);
    let offers = owned_leases(&[fixtures.terminal], &leases, None);
    let extent = RenderExtent::new(1920, 1080).expect("native timing extent is valid");
    let target = PresentationTarget::ThreeD;
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(512 * MIB)
            .expect("native timing ledger is valid")
            .with_gpu_timing(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    assert!(
        gpu.diagnostics().gpu_timestamps_supported(),
        "native timing requires Vulkan timestamp queries"
    );
    request_target_layout(&mut gpu, target, extent);

    let mut cases = Vec::new();
    for sampling in [SamplingPolicy::VoxelExact, SamplingPolicy::SmoothLinear] {
        cases.push((
            format!("MIP-{sampling:?}"),
            mirante4d_domain::RenderState::mip(sampling),
        ));
        cases.push((
            format!("DVR-{sampling:?}"),
            mirante4d_domain::RenderState::dvr(
                sampling,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 1.0).unwrap(),
                    TransferCurve::linear(),
                ),
                0.05,
            )
            .unwrap(),
        ));
        cases.push((
            format!("ISO-{sampling:?}"),
            mirante4d_domain::RenderState::iso(sampling, IsoShadingPolicy::Flat, 0.5).unwrap(),
        ));
    }

    let mut next_frame = 1_200_u64;
    let (warmup_intent, warmup_requirements) = intent_and_requirements(
        next_frame,
        13,
        mirante4d_domain::RenderState::mip(SamplingPolicy::SmoothLinear),
        volume_view_for([31.5, 31.5, 31.5], 64.0 / 1080.0, 128.0),
        extent,
        &[fixtures.terminal],
    );
    next_frame += 1;
    gpu.offer_residency_leases(&offers)
        .expect("terminal warmup lease is offered");
    let warmup_seed = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &warmup_intent,
        &warmup_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let warmup_seed_report = warmup_seed
        .target(target)
        .expect("terminal warmup seed report exists");
    assert!(warmup_seed_report.presented());
    assert!(!warmup_seed_report.deferred_by_backpressure());
    let _ = poll_gpu_timing(
        &mut gpu,
        warmup_seed
            .gpu_timing()
            .expect("terminal warmup seed has a GPU timing ticket"),
        deadline,
    );
    for _ in 0..GLOBAL_PIPELINE_WARMUP_FRAMES {
        let intent = warmup_intent
            .clone()
            .with_frame(FrameIdentity::new(next_frame));
        next_frame += 1;
        let requirements = warmup_requirements
            .rebind(&intent)
            .expect("terminal warmup body rebinds without changing resources");
        gpu.offer_residency_leases(&offers)
            .expect("resident terminal lease remains idempotent");
        let cutoff = execute_coordinated_target(
            &mut gpu,
            target,
            &catalog,
            &intent,
            &requirements,
            RetainedFrameRenderPolicy::EveryUsefulFrame,
        );
        let report = cutoff
            .target(target)
            .expect("terminal warmup report exists");
        assert!(report.presented());
        assert!(!report.deferred_by_backpressure());
        assert_eq!(report.visited_resources(), 0);
        assert_eq!(report.uploaded_resources(), 0);
        assert_eq!(cutoff.residency_queue_submissions(), 0);
        let _ = poll_gpu_timing(
            &mut gpu,
            cutoff
                .gpu_timing()
                .expect("terminal warmup frame has a GPU timing ticket"),
            deadline,
        );
    }

    let mut absolute_failures = Vec::new();
    for (label, state) in cases {
        let (warm_intent, warm_requirements) = intent_and_requirements(
            next_frame,
            13,
            state,
            volume_view_for([31.5, 31.5, 31.5], 64.0 / 1080.0, 128.0),
            extent,
            &[fixtures.terminal],
        );
        next_frame += 1;
        for _ in 0..CASE_WARMUP_FRAMES {
            let intent = warm_intent
                .clone()
                .with_frame(FrameIdentity::new(next_frame));
            next_frame += 1;
            let requirements = warm_requirements
                .rebind(&intent)
                .expect("terminal case warmup rebinds the same resident body");
            gpu.offer_residency_leases(&offers)
                .expect("terminal timing lease is offered");
            let warm = execute_coordinated_target(
                &mut gpu,
                target,
                &catalog,
                &intent,
                &requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let warm_report = warm.target(target).expect("warm timing report exists");
            assert!(warm_report.presented());
            assert!(!warm_report.deferred_by_backpressure());
            assert_eq!(warm_report.visited_resources(), 0);
            assert_eq!(warm_report.uploaded_resources(), 0);
            assert_eq!(warm.residency_queue_submissions(), 0);
            let _ = poll_gpu_timing(
                &mut gpu,
                warm.gpu_timing()
                    .expect("warm terminal frame has a GPU timing ticket"),
                deadline,
            );
        }

        let mut raw_render_pass_ns = Vec::with_capacity(MEASURED_TRIALS);
        for _ in 0..MEASURED_TRIALS {
            let intent = warm_intent
                .clone()
                .with_frame(FrameIdentity::new(next_frame));
            next_frame += 1;
            let requirements = warm_requirements
                .rebind(&intent)
                .expect("terminal timing body rebinds without changing resources");
            gpu.offer_residency_leases(&offers)
                .expect("resident terminal lease remains idempotent");
            let cutoff = execute_coordinated_target(
                &mut gpu,
                target,
                &catalog,
                &intent,
                &requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let report = cutoff.target(target).expect("timed terminal report exists");
            assert!(report.presented());
            assert!(!report.deferred_by_backpressure());
            assert_eq!(report.visited_resources(), 0);
            assert_eq!(report.uploaded_resources(), 0);
            assert_eq!(cutoff.residency_queue_submissions(), 0);
            let timing = poll_gpu_timing(
                &mut gpu,
                cutoff
                    .gpu_timing()
                    .expect("resident terminal frame has a GPU timing ticket"),
                deadline,
            );
            raw_render_pass_ns.push(
                timing
                    .render_pass_ns()
                    .expect("native terminal frame has a volume-pass interval"),
            );
        }
        let mut render_pass_ns = raw_render_pass_ns.clone();
        render_pass_ns.sort_unstable();
        let p95_ns = percentile(&render_pass_ns, 0.95);
        let preferred_met = p95_ns <= PREFERRED_60_HZ_COMPONENT_NS;
        println!(
            "M4D_GPU_PERFORMANCE_V1 {}",
            serde_json::json!({
                "family": "native_terminal",
                "measurement_id": format!("native_terminal::{label}"),
                "adapter": sanitize_diagnostic_text(gpu.diagnostics().adapter_name()),
                "backend": "Vulkan",
                "driver": sanitize_diagnostic_text(gpu.diagnostics().driver()),
                "shape": "64x64x64",
                "extent": "1920x1080",
                "global_warmups": GLOBAL_PIPELINE_WARMUP_FRAMES,
                "case_warmups": CASE_WARMUP_FRAMES,
                "mode": label,
                "samples": MEASURED_TRIALS,
                "raw_render_pass_ns": raw_render_pass_ns,
                "median_ns": percentile(&render_pass_ns, 0.50),
                "p95_ns": p95_ns,
                "absolute_limit_ns": ABSOLUTE_30_HZ_COMPONENT_NS,
                "absolute_met": p95_ns <= ABSOLUTE_30_HZ_COMPONENT_NS,
                "preferred_ns": PREFERRED_60_HZ_COMPONENT_NS,
                "preferred_met": preferred_met,
                "uploads_during_samples": 0,
                "validation_errors": 0,
            })
        );
        if p95_ns > ABSOLUTE_30_HZ_COMPONENT_NS {
            absolute_failures.push(format!(
                "{label} terminal component p95 {p95_ns} ns exceeded the 30-Hz feasibility limit"
            ));
        }
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    drop(offers);
    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "native terminal timing exceeded its 900-second deadline"
    );
    assert!(
        absolute_failures.is_empty(),
        "native terminal absolute feasibility failures: {}",
        absolute_failures.join("; ")
    );
}

#[test]
#[ignore = "requires a trusted Vulkan GPU with timestamp-query support for measurements"]
fn fixed_lod_multichannel_gpu_timing_matrix() {
    const WARMUP_FRAMES: usize = 30;
    const MEASURED_TRIALS: usize = 120;
    const ABSOLUTE_30_HZ_COMPONENT_NS: u64 = 33_300_000;
    const HOMOGENEOUS_LINEAR_RATIO_LIMIT: f64 = 1.2;

    let deadline = Instant::now() + Duration::from_secs(1_200);
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 41);
    let leases = load_keys(
        &dataset_runtime,
        &fixtures.performance_channels,
        generation,
        deadline,
    );
    let offers = owned_leases(&fixtures.performance_channels, &leases, None);
    let extent = RenderExtent::new(1920, 1080).expect("multichannel timing extent is valid");
    let target = PresentationTarget::ThreeD;
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(512 * MIB)
            .expect("multichannel timing ledger is valid")
            .with_gpu_timing(true),
        None,
        TestPipelineAdmission::Ready,
    );
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    assert!(
        gpu.diagnostics().gpu_timestamps_supported(),
        "the multichannel timing matrix requires Vulkan timestamp queries"
    );
    request_target_layout(&mut gpu, target, extent);

    let view = volume_view_for([31.5, 31.5, 31.5], 64.0 / 1080.0, 128.0);
    let colors = [
        [1.0, 0.2, 0.2],
        [0.2, 1.0, 0.2],
        [0.2, 0.4, 1.0],
        [1.0, 0.8, 0.2],
        [1.0, 0.2, 0.8],
        [0.2, 1.0, 1.0],
        [0.8, 0.6, 0.3],
        [0.7, 0.7, 0.7],
    ];
    let dvr_state = |sampling| {
        mirante4d_domain::RenderState::dvr(
            sampling,
            DvrOpacityTransfer::new(
                DisplayWindow::new(0.0, 1.0).unwrap(),
                TransferCurve::linear(),
            ),
            0.02,
        )
        .unwrap()
    };
    let iso_state = |sampling| {
        mirante4d_domain::RenderState::iso(sampling, IsoShadingPolicy::Flat, 0.5).unwrap()
    };
    let layers_for = |count: usize, kernel: &str, sampling: SamplingPolicy| {
        (0..count)
            .map(|index| {
                let state = match kernel {
                    "MIP" => mirante4d_domain::RenderState::mip(sampling),
                    "DVR" => dvr_state(sampling),
                    "ISO" => iso_state(sampling),
                    "Mixed" => match index % 3 {
                        0 => mirante4d_domain::RenderState::mip(sampling),
                        1 => dvr_state(sampling),
                        _ => iso_state(sampling),
                    },
                    _ => unreachable!("the timing kernel matrix is fixed"),
                };
                LayerRenderIntent::new(
                    fixtures.performance_channels[index].layer(),
                    transfer_color_opacity(0.0, 1.0, colors[index], 0.25),
                    state,
                )
            })
            .collect::<Vec<_>>()
    };

    let mut next_frame = 20_000_u64;
    let seed_layers = layers_for(8, "MIP", SamplingPolicy::VoxelExact);
    let (seed_intent, seed_requirements) = multichannel_intent_and_requirements(
        next_frame,
        view,
        extent,
        seed_layers,
        &fixtures.performance_channels,
    );
    next_frame += 1;
    gpu.offer_residency_leases(&offers)
        .expect("all multichannel timing leases are offered");
    let seed = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &seed_intent,
        &seed_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let seed_report = seed
        .target(target)
        .expect("the upload seed has a target report");
    assert!(seed_report.presented());
    let _ = poll_gpu_timing(
        &mut gpu,
        seed.gpu_timing()
            .expect("the upload seed has a timing ticket"),
        deadline,
    );

    let mut reference_p95 = BTreeMap::<String, u64>::new();
    let mut maximum_homogeneous_linear_ratio = 0.0_f64;
    let mut maximum_homogeneous_linear_case = String::new();
    let mut contract_failures = Vec::new();
    for sampling in [SamplingPolicy::VoxelExact, SamplingPolicy::SmoothLinear] {
        for kernel in ["MIP", "DVR", "ISO", "Mixed"] {
            let case_key = format!("{kernel}-{sampling:?}");
            for channel_count in [1_usize, 2, 4, 8] {
                let keys = &fixtures.performance_channels[..channel_count];
                let (base_intent, base_requirements) = multichannel_intent_and_requirements(
                    next_frame,
                    view,
                    extent,
                    layers_for(channel_count, kernel, sampling),
                    keys,
                );
                next_frame += 1;
                for _ in 0..WARMUP_FRAMES {
                    let intent = base_intent
                        .clone()
                        .with_frame(FrameIdentity::new(next_frame));
                    next_frame += 1;
                    let requirements = base_requirements
                        .rebind(&intent)
                        .expect("a fixed-LOD warmup rebinds the same resident resources");
                    gpu.offer_residency_leases(&offers[..channel_count])
                        .expect("warm resident channel offers remain idempotent");
                    let cutoff = execute_coordinated_target(
                        &mut gpu,
                        target,
                        &catalog,
                        &intent,
                        &requirements,
                        RetainedFrameRenderPolicy::EveryUsefulFrame,
                    );
                    let report = cutoff.target(target).expect("warmup report exists");
                    assert!(report.presented());
                    assert_eq!(report.visited_resources(), 0);
                    assert_eq!(report.uploaded_resources(), 0);
                    assert_eq!(cutoff.residency_queue_submissions(), 0);
                    let _ = poll_gpu_timing(
                        &mut gpu,
                        cutoff.gpu_timing().expect("warmup has a timing ticket"),
                        deadline,
                    );
                }

                let mut render_ns = Vec::with_capacity(MEASURED_TRIALS);
                let mut cpu_planning_ns = Vec::with_capacity(MEASURED_TRIALS);
                let mut cpu_submit_ns = Vec::with_capacity(MEASURED_TRIALS);
                for _ in 0..MEASURED_TRIALS {
                    let intent = base_intent
                        .clone()
                        .with_frame(FrameIdentity::new(next_frame));
                    next_frame += 1;
                    let requirements = base_requirements
                        .rebind(&intent)
                        .expect("a fixed-LOD sample rebinds the same resident resources");
                    let cutoff = execute_coordinated_target(
                        &mut gpu,
                        target,
                        &catalog,
                        &intent,
                        &requirements,
                        RetainedFrameRenderPolicy::EveryUsefulFrame,
                    );
                    let report = cutoff.target(target).expect("timed report exists");
                    assert!(report.presented());
                    assert_eq!(report.visited_resources(), 0);
                    assert_eq!(report.uploaded_resources(), 0);
                    assert_eq!(cutoff.residency_queue_submissions(), 0);
                    let cpu = cutoff
                        .cpu_timing()
                        .expect("CPU timing is enabled with GPU timing");
                    cpu_planning_ns.push(cpu.planning_ns());
                    cpu_submit_ns.push(cpu.queue_submit_ns());
                    let timing = poll_gpu_timing(
                        &mut gpu,
                        cutoff.gpu_timing().expect("sample has a timing ticket"),
                        deadline,
                    );
                    render_ns.push(
                        timing
                            .render_pass_ns()
                            .expect("the fixed-LOD sample has a color-pass interval"),
                    );
                }
                let raw_render_ns = render_ns.clone();
                let raw_cpu_planning_ns = cpu_planning_ns.clone();
                let raw_cpu_submit_ns = cpu_submit_ns.clone();
                render_ns.sort_unstable();
                cpu_planning_ns.sort_unstable();
                cpu_submit_ns.sort_unstable();
                let p95_ns = percentile(&render_ns, 0.95);
                let reference_channels = if kernel == "Mixed" { 2 } else { 1 };
                if channel_count == reference_channels {
                    reference_p95.insert(case_key.clone(), p95_ns);
                }
                let ratios = reference_p95.get(&case_key).map(|baseline| {
                    let reference_ratio = p95_ns as f64 / (*baseline).max(1) as f64;
                    let ideal_channel_ratio = channel_count as f64 / reference_channels as f64;
                    (reference_ratio, reference_ratio / ideal_channel_ratio)
                });
                if kernel != "Mixed"
                    && matches!(channel_count, 4 | 8)
                    && let Some((_, linear_ratio)) = ratios
                    && linear_ratio > maximum_homogeneous_linear_ratio
                {
                    maximum_homogeneous_linear_ratio = linear_ratio;
                    maximum_homogeneous_linear_case =
                        format!("{kernel}-{sampling:?}-{channel_count}ch");
                }
                let reference_ratio = ratios
                    .map(|(ratio, _)| format!("{ratio:.4}"))
                    .unwrap_or_else(|| "n/a".to_owned());
                let linear_ratio = ratios
                    .map(|(_, ratio)| format!("{ratio:.4}"))
                    .unwrap_or_else(|| "n/a".to_owned());
                let reported_kernel = if kernel == "Mixed" && channel_count == 1 {
                    "MIP-control"
                } else {
                    kernel
                };
                let compatibility = if kernel == "Mixed" && channel_count == 1 {
                    "homogeneous-control"
                } else if kernel == "Mixed" {
                    "authored-mixed"
                } else {
                    "co-registered-homogeneous"
                };
                println!(
                    "M4D_GPU_PERFORMANCE_V1 {}",
                    serde_json::json!({
                        "family": "fixed_lod_multichannel",
                        "measurement_id": format!(
                            "fixed_lod_multichannel::{reported_kernel}::{sampling:?}::{compatibility}::{channel_count}ch"
                        ),
                        "adapter": sanitize_diagnostic_text(gpu.diagnostics().adapter_name()),
                        "backend": "Vulkan",
                        "driver": sanitize_diagnostic_text(gpu.diagnostics().driver()),
                        "shape": "64x64x64",
                        "extent": "1920x1080",
                        "warmups": WARMUP_FRAMES,
                        "samples": MEASURED_TRIALS,
                        "kernel": reported_kernel,
                        "sampling": format!("{sampling:?}"),
                        "compatibility": compatibility,
                        "channels": channel_count,
                        "resources": channel_count,
                        "payload_bytes": channel_count as u64 * MIB,
                        "raw_render_pass_ns": raw_render_ns,
                        "raw_cpu_planning_ns": raw_cpu_planning_ns,
                        "raw_cpu_submit_ns": raw_cpu_submit_ns,
                        "median_ns": percentile(&render_ns, 0.50),
                        "p95_ns": p95_ns,
                        "maximum_ns": *render_ns.last().unwrap(),
                        "absolute_limit_ns": ABSOLUTE_30_HZ_COMPONENT_NS,
                        "absolute_met": p95_ns <= ABSOLUTE_30_HZ_COMPONENT_NS,
                        "reference_channels": reference_channels,
                        "reference_ratio": reference_ratio,
                        "ideal_linear_normalized_ratio": linear_ratio,
                        "cpu_planning_p95_ns": percentile(&cpu_planning_ns, 0.95),
                        "cpu_submit_p95_ns": percentile(&cpu_submit_ns, 0.95),
                        "uploads_during_samples": 0,
                        "validation_errors": 0,
                    })
                );
                if p95_ns > ABSOLUTE_30_HZ_COMPONENT_NS {
                    contract_failures.push(format!(
                        "{reported_kernel}-{sampling:?}-{channel_count}ch component p95 {p95_ns} ns exceeded the 30-Hz feasibility limit"
                    ));
                }
            }
        }
    }
    let gate_met = maximum_homogeneous_linear_ratio <= HOMOGENEOUS_LINEAR_RATIO_LIMIT;
    println!(
        "M4D_GPU_PERFORMANCE_GATE_V1 {}",
        serde_json::json!({
            "gate": "fixed_lod_homogeneous_linear_ratio",
            "threshold": HOMOGENEOUS_LINEAR_RATIO_LIMIT,
            "observed": maximum_homogeneous_linear_ratio,
            "case": maximum_homogeneous_linear_case,
            "met": gate_met,
        })
    );
    if !gate_met {
        contract_failures.push(format!(
            "fixed-LOD homogeneous linear ratio {maximum_homogeneous_linear_ratio:.4} for {maximum_homogeneous_linear_case} exceeded {HOMOGENEOUS_LINEAR_RATIO_LIMIT:.4}"
        ));
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    drop(offers);
    dataset_runtime.request_shutdown().unwrap();
    drop(leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "fixed-LOD multichannel timing exceeded its 1200-second deadline"
    );
    assert!(
        contract_failures.is_empty(),
        "fixed-LOD multichannel performance contract failures: {}",
        contract_failures.join("; ")
    );
}

#[test]
#[ignore = "requires the trusted HW2 Vulkan workstation"]
fn resident_coordinated_volume_is_exact_zero_work_and_idle_when_resident() {
    run_resident_coordinated_volume_case(false);
}

#[test]
#[ignore = "requires a trusted Vulkan GPU with timestamp-query support for measurements"]
fn resident_coordinated_volume_gpu_timing() {
    run_resident_coordinated_volume_case(true);
}

fn run_resident_coordinated_volume_case(measure_performance: bool) {
    const WARMUP_FRAMES: usize = 30;
    const MEASURED_TRIALS: usize = 120;
    const CORRECTNESS_CHANGED_FRAMES: usize = 2;
    const ABSOLUTE_30_HZ_COMPONENT_NS: u64 = 33_300_000;

    let deadline =
        Instant::now() + Duration::from_secs(if measure_performance { 600 } else { 120 });
    let fixtures = build_fixtures();
    let (dataset_runtime, catalog) = start_dataset_runtime(&fixtures.source);
    let generation = CancellationGeneration::for_scope(REQUEST_SCOPE, 1);
    let upload_leases = load_keys(&dataset_runtime, &fixtures.upload, generation, deadline);
    let upload_offers = owned_leases(&fixtures.upload, &upload_leases, None);
    let extent = RenderExtent::new(1280, 720).expect("timing extent is valid");
    let target = PresentationTarget::ThreeD;
    let mut gpu = test_gpu_runtime(
        WgpuRenderRuntimeConfig::new(PERFORMANCE_GPU_BYTES)
            .expect("timing GPU ledger is valid")
            .with_gpu_timing(measure_performance),
        None,
        TestPipelineAdmission::Ready,
    );
    let renderer_events = Arc::new(Mutex::new(VecDeque::new()));
    let event_queue = Arc::clone(&renderer_events);
    gpu.set_renderer_event_sink(RendererEventSink::new(move |event| {
        event_queue
            .lock()
            .expect("the renderer test event queue is never poisoned")
            .push_back(event);
    }));
    activate_fixture_dataset(&mut gpu, &fixtures, &catalog);
    assert_qualifying_adapter(gpu.diagnostics());
    if measure_performance {
        assert!(
            gpu.diagnostics().gpu_timestamps_supported(),
            "this diagnostic requires Vulkan timestamp-query support"
        );
    }
    request_target_layout(&mut gpu, target, extent);

    let mip_state = mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact);
    let (resident_intent, resident_requirements) = intent_and_requirements(
        500,
        3,
        mip_state,
        volume_view_for([96.0, 96.0, 32.0], 192.0 / 720.0, 256.0),
        extent,
        &fixtures.upload,
    );

    // Establish the exact nine-brick working set through the product
    // coordinator. Seven pages fit the configured transfer-staging category,
    // so the nine-page fixture requires two cutoffs;
    // neither is part of the resident timing sample.
    gpu.offer_residency_leases(&upload_offers[..7])
        .expect("resident-volume leases are offered");
    let first_upload = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &resident_intent,
        &resident_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let first_report = first_upload
        .target(target)
        .expect("the first upload cutoff has one target report");
    assert_eq!(first_report.uploaded_resources(), 7);
    assert_eq!(first_report.payload_upload_bytes(), 7 * MIB);
    assert_eq!(first_upload.residency_queue_submissions(), 1);
    assert_eq!(first_upload.color_queue_submissions(), 1);
    if measure_performance {
        let _ = poll_gpu_timing(
            &mut gpu,
            first_upload
                .gpu_timing()
                .expect("the first coordinated upload has a timing ticket"),
            deadline,
        );
    } else {
        assert!(first_upload.gpu_timing().is_none());
    }
    wait_for_submission_completion(
        &mut gpu,
        &renderer_events,
        first_upload
            .submitted_through_event()
            .expect("the first setup report names its final submission"),
        deadline,
    );

    gpu.offer_residency_leases(&upload_offers[7..])
        .expect("the remaining resident-volume leases are offered");
    let second_upload = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &resident_intent,
        &resident_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    let second_report = second_upload
        .target(target)
        .expect("the second upload cutoff has one target report");
    assert_eq!(second_report.uploaded_resources(), 2);
    assert_eq!(second_report.payload_upload_bytes(), 2 * MIB);
    assert_eq!(
        second_report.progress().map(FrameProgress::completeness),
        Some(FrameCompleteness::Exact)
    );
    assert_eq!(second_upload.residency_queue_submissions(), 1);
    assert_eq!(second_upload.color_queue_submissions(), 1);
    if measure_performance {
        let _ = poll_gpu_timing(
            &mut gpu,
            second_upload
                .gpu_timing()
                .expect("the second coordinated upload has a timing ticket"),
            deadline,
        );
    } else {
        assert!(second_upload.gpu_timing().is_none());
    }
    wait_for_submission_completion(
        &mut gpu,
        &renderer_events,
        second_upload
            .submitted_through_event()
            .expect("the second setup report names its final submission"),
        deadline,
    );

    let structural_before = (
        gpu.diagnostics().uploaded_resources(),
        gpu.diagnostics().uploaded_payload_bytes(),
        gpu.diagnostics().residency_evictions(),
        gpu.diagnostics().directory_mutations(),
        gpu.diagnostics().allocator_plans(),
        gpu.diagnostics().control_buffer_allocations(),
        gpu.diagnostics().bind_group_creations(),
        gpu.diagnostics().usable_pipeline_handles(),
    );
    let mut next_frame = 501_u64;
    if measure_performance {
        for _ in 0..WARMUP_FRAMES {
            let intent = resident_intent
                .clone()
                .with_frame(FrameIdentity::new(next_frame));
            next_frame += 1;
            let requirements = resident_requirements
                .rebind(&intent)
                .expect("the resident warmup rebinds the same requirements");
            gpu.offer_residency_leases(&upload_offers)
                .expect("resident-volume leases remain idempotent during warmup");
            let cutoff = execute_coordinated_target(
                &mut gpu,
                target,
                &catalog,
                &intent,
                &requirements,
                RetainedFrameRenderPolicy::EveryUsefulFrame,
            );
            let report = cutoff
                .target(target)
                .expect("the resident warmup has one target report");
            assert!(report.presented());
            assert!(!report.deferred_by_backpressure());
            assert_eq!(report.visited_resources(), 0);
            assert_eq!(report.uploaded_resources(), 0);
            assert_eq!(report.payload_upload_bytes(), 0);
            assert_eq!(cutoff.residency_queue_submissions(), 0);
            assert_eq!(cutoff.color_queue_submissions(), 1);
            let _ = poll_gpu_timing(
                &mut gpu,
                cutoff
                    .gpu_timing()
                    .expect("the resident warmup has a timing ticket"),
                deadline,
            );
        }
    }
    let measured_trials = if measure_performance {
        MEASURED_TRIALS
    } else {
        CORRECTNESS_CHANGED_FRAMES
    };
    let mut raw_render_pass_ns = Vec::with_capacity(MEASURED_TRIALS);
    let mut last_frame = None;
    for _ in 0..measured_trials {
        let intent = resident_intent
            .clone()
            .with_frame(FrameIdentity::new(next_frame));
        next_frame += 1;
        let requirements = resident_requirements
            .rebind(&intent)
            .expect("the resident timing body rebinds without changing requirements");
        gpu.offer_residency_leases(&upload_offers)
            .expect("resident-volume leases remain idempotent");
        let cutoff = execute_coordinated_target(
            &mut gpu,
            target,
            &catalog,
            &intent,
            &requirements,
            RetainedFrameRenderPolicy::EveryUsefulFrame,
        );
        let report = cutoff
            .target(target)
            .expect("the resident timing cutoff has one target report");
        assert!(report.presented());
        assert!(!report.deferred_by_backpressure());
        assert_eq!(
            report.progress().map(FrameProgress::completeness),
            Some(FrameCompleteness::Exact)
        );
        assert_eq!(report.visited_resources(), 0);
        assert_eq!(report.uploaded_resources(), 0);
        assert_eq!(report.payload_upload_bytes(), 0);
        assert_eq!(cutoff.residency_queue_submissions(), 0);
        assert_eq!(cutoff.color_queue_submissions(), 1);
        assert_eq!(cutoff.recorded_targets(), &[target]);
        if measure_performance {
            let ticket = cutoff
                .gpu_timing()
                .expect("a resident coordinated color cutoff has a timing ticket");
            assert_eq!(ticket.target(), target);
            assert_eq!(ticket.generation(), intent.frame());
            assert_eq!(ticket.pass_kind(), RenderPassKind::Volume);
            let timing = poll_gpu_timing(&mut gpu, ticket, deadline);
            assert_eq!(timing.ticket(), ticket);
            raw_render_pass_ns.push(
                timing
                    .render_pass_ns()
                    .expect("the resident cutoff has a GPU volume-pass interval"),
            );
        } else {
            assert!(cutoff.gpu_timing().is_none());
            wait_for_submission_completion(
                &mut gpu,
                &renderer_events,
                cutoff
                    .submitted_through_event()
                    .expect("the measured changed frame names its color submission"),
                deadline,
            );
        }
        last_frame = Some((intent, requirements));
    }

    assert_eq!(
        (
            gpu.diagnostics().uploaded_resources(),
            gpu.diagnostics().uploaded_payload_bytes(),
            gpu.diagnostics().residency_evictions(),
            gpu.diagnostics().directory_mutations(),
            gpu.diagnostics().allocator_plans(),
            gpu.diagnostics().control_buffer_allocations(),
            gpu.diagnostics().bind_group_creations(),
            gpu.diagnostics().usable_pipeline_handles(),
        ),
        structural_before,
        "resident coordinated movement must not rebuild residency or GPU structure"
    );

    let (last_intent, last_requirements) =
        last_frame.expect("the resident timing loop produced one final frame");
    let submissions_before_idle = gpu.diagnostics().queue_submissions();
    let idle = execute_coordinated_target(
        &mut gpu,
        target,
        &catalog,
        &last_intent,
        &last_requirements,
        RetainedFrameRenderPolicy::EveryUsefulFrame,
    );
    assert_eq!(idle.residency_queue_submissions(), 0);
    assert_eq!(idle.color_queue_submissions(), 0);
    assert!(idle.recorded_targets().is_empty());
    assert!(idle.gpu_timing().is_none());
    assert_eq!(
        gpu.diagnostics().queue_submissions(),
        submissions_before_idle,
        "an identical settled cutoff must submit no renderer work"
    );

    if measure_performance {
        let mut render_pass_ns = raw_render_pass_ns.clone();
        render_pass_ns.sort_unstable();
        let p95_ns = percentile(&render_pass_ns, 0.95);
        println!(
            "M4D_GPU_PERFORMANCE_V1 {}",
            serde_json::json!({
                "family": "resident_coordinated",
                "measurement_id": "resident_coordinated::1280x720_mip_9x64cubed_resident",
                "adapter": sanitize_diagnostic_text(gpu.diagnostics().adapter_name()),
                "backend": "Vulkan",
                "driver": sanitize_diagnostic_text(gpu.diagnostics().driver()),
                "workload": "1280x720_mip_9x64cubed_resident",
                "extent": "1280x720",
                "warmups": WARMUP_FRAMES,
                "samples": MEASURED_TRIALS,
                "raw_render_pass_ns": raw_render_pass_ns,
                "median_ns": percentile(&render_pass_ns, 0.50),
                "p95_ns": p95_ns,
                "absolute_limit_ns": ABSOLUTE_30_HZ_COMPONENT_NS,
                "absolute_met": p95_ns <= ABSOLUTE_30_HZ_COMPONENT_NS,
                "uploads_during_samples": 0,
                "validation_errors": 0,
            })
        );
        assert!(
            p95_ns <= ABSOLUTE_30_HZ_COMPONENT_NS,
            "resident coordinated component p95 {p95_ns} ns exceeded the 30-Hz feasibility limit"
        );
    }
    assert_eq!(gpu.diagnostics().validation_error_count(), 0);

    drop(upload_offers);
    dataset_runtime
        .request_shutdown()
        .expect("timing fixture runtime begins bounded shutdown");
    drop(upload_leases);
    drop(dataset_runtime);
    assert!(
        Instant::now() <= deadline,
        "resident coordinated case exceeded its finite deadline"
    );
}
