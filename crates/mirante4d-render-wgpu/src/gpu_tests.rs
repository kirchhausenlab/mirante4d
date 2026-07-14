#![forbid(unsafe_code)]

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
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
    FrameIdentity, FrameLimitation, GpuLedgerCategory, LayerRenderIntent, PresentationToken,
    PresentationViewport, RenderExtent, RenderIntent, RenderRequirement, RenderRequirementRole,
    RenderRequirements, RenderViewIntent,
};
use mirante4d_render_reference::{ReferenceFrame, ReferenceRenderer};

use super::{
    FrameExecutionReport, ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntime,
    WgpuRenderRuntimeConfig, WgpuRenderRuntimeDiagnostics, WgpuRenderRuntimeError,
};

const MIB: u64 = 1024 * 1024;
const QUALIFICATION_GPU_BYTES: u64 = 4 * 1024 * MIB;
const SMALL_GPU_BYTES: u64 = 11 * MIB;
const SOURCE_ID: DatasetSourceId = DatasetSourceId::new(0x5750_3039_4100_0001);
const REQUEST_SCOPE: u64 = 0x5750_3039_4100_0002;
const SEMANTIC_FIXTURE_ID: &str = "wp09a-semantic-small";
const UPLOAD_FIXTURE_ID: &str = "wp09a-upload-boundary";
const WORK_FIXTURE_ID: &str = "wp09a-work-boundary";

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

    fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
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
    }
}

struct QualificationFixtures {
    source: Arc<FixtureSource>,
    semantic: [Vec<DatasetResourceKey>; 3],
    missing_u8: DatasetResourceKey,
    upload: Vec<DatasetResourceKey>,
    work: Vec<DatasetResourceKey>,
}

fn layer(
    ordinal: u32,
    label: &str,
    shape: [u64; 3],
    dtype: IntensityDType,
    validity: ResourceValidity,
) -> DatasetLayer {
    DatasetLayer::new(
        LogicalLayerKey::new(ordinal),
        label,
        Shape4D::new(1, shape[0], shape[1], shape[2]).expect("fixture shape is valid"),
        dtype,
        GridToWorld::identity(),
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
                    [1, 1, 129],
                    IntensityDType::Uint8,
                    ResourceValidity::AllValid,
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
    for x in 0_u64..129 {
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
    assert_eq!(work.len(), 129);

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
    LayerTransfer::new(
        DisplayWindow::new(low, high).expect("fixture window is valid"),
        RgbColor::new([1.0, 0.0, 0.0]).expect("fixture color is valid"),
        Opacity::new(1.0).expect("fixture opacity is valid"),
        TransferCurve::linear(),
        false,
    )
}

fn volume_view() -> RenderViewIntent {
    RenderViewIntent::volume(
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(15.5, 15.5, 15.5).expect("fixture target is finite"),
            UnitQuaternion::identity(),
            0.5,
            320.0,
            40.0,
        )
        .expect("fixture camera is valid"),
        IsoLightState::attached_camera(),
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
        2 | 3 => transfer(0.0, 1.0),
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

fn execute_and_compare(
    gpu: &mut WgpuRenderRuntime,
    presentation: PresentationToken,
    catalog: &DatasetCatalog,
    intent: &RenderIntent,
    requirements: &RenderRequirements,
    leases: &[&dyn ResourceLease],
    deadline: Instant,
    counters: &mut Counters,
) -> (ValidationCapture, u8) {
    let report = gpu
        .execute_frame(presentation, catalog, intent, requirements, leases)
        .expect("semantic GPU frame executes");
    counters.record(&report);
    let ticket = report
        .validation_capture()
        .expect("qualification enables asynchronous validation capture");
    let capture = poll_capture(gpu, ticket, deadline);
    let reference = ReferenceRenderer::new()
        .render(catalog, intent, leases)
        .expect("independent CPU reference renders fixture leases");
    let max_delta = compare_reference(&capture, &reference);
    (capture, max_delta)
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
            "\"schema_version\":1,",
            "\"adapter\":{{\"name\":\"{}\",\"backend\":\"{}\",\"driver\":\"{}\",",
            "\"max_buffer_size_bytes\":{},\"max_storage_buffer_binding_size_bytes\":{},",
            "\"max_storage_buffers_per_shader_stage\":{}}},",
            "\"ledger\":{},\"counters\":{},",
            "\"capacity_ledger\":{},\"capacity_counters\":{},",
            "\"cases\":{{",
            "\"semantic_modes_and_dtypes\":[\"mip-u8\",\"dvr-u16\",\"iso-f32\",\"cross-section-u8\"],",
            "\"semantic_fixture_resources\":24,",
            "\"semantic_fixture_decoded_bytes_with_validity\":241664,",
            "\"upload_first_resources\":8,\"upload_first_bytes\":8388608,",
            "\"upload_second_resources\":1,\"upload_second_bytes\":1048576,",
            "\"work_first_visits\":128,\"work_second_visits\":1,",
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
        ledger,
        main_counters_json,
        capacity_ledger,
        capacity_counters_json,
        counters.captures,
        max_delta,
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
        &catalog,
        &mip,
        &mip_requirements,
        &u8_leases,
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
        &catalog,
        &dvr,
        &dvr_requirements,
        &u16_leases,
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
    let (_, delta) = execute_and_compare(
        &mut gpu,
        presentation,
        &catalog,
        &iso,
        &iso_requirements,
        &f32_leases,
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
        &catalog,
        &section,
        &section_requirements,
        &u8_leases,
        deadline,
        &mut counters,
    );
    rgba8_max_delta = rgba8_max_delta.max(delta);
    assert_eq!(pixel(&section_capture, 16, 79), ([0, 0, 0, 255], 1, 1));
    assert_eq!(pixel(&section_capture, 18, 79), ([0, 0, 0, 0], 1, 0));
    assert_eq!(pixel(&section_capture, 46, 79), ([255, 0, 0, 255], 1, 1));
    assert_eq!(pixel(&section_capture, 57, 79), ([0, 0, 0, 0], 0, 0));
    assert_eq!(pixel(&section_capture, 0, 0), ([0, 0, 0, 0], 1, 0));

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
    let upload_borrowed = borrowed_leases(&fixtures.upload, &upload_leases, None);
    let first_upload = gpu
        .execute_frame(
            presentation,
            &catalog,
            &upload_intent,
            &upload_requirements,
            &upload_borrowed,
        )
        .expect("first upload-boundary frame executes");
    assert_eq!(first_upload.uploaded_resources(), 8);
    assert_eq!(first_upload.payload_upload_bytes(), 8 * MIB);
    assert_eq!(
        first_upload
            .progress()
            .and_then(|progress| progress.limitation()),
        Some(FrameLimitation::BudgetLimited)
    );
    counters.record(&first_upload);
    let _ = poll_capture(
        &mut gpu,
        first_upload
            .validation_capture()
            .expect("boundary capture exists"),
        deadline,
    );
    let second_upload = gpu
        .execute_frame(
            presentation,
            &catalog,
            &upload_intent,
            &upload_requirements,
            &upload_borrowed,
        )
        .expect("second upload-boundary frame executes");
    assert_eq!(second_upload.uploaded_resources(), 1);
    assert_eq!(second_upload.payload_upload_bytes(), MIB);
    counters.record(&second_upload);
    let _ = poll_capture(
        &mut gpu,
        second_upload
            .validation_capture()
            .expect("boundary capture exists"),
        deadline,
    );

    let work_leases = load_keys(
        &dataset_runtime,
        &fixtures.work,
        current_generation,
        deadline,
    );
    let (work_intent, work_requirements) = intent_and_requirements(
        20,
        4,
        mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
        cross_section_view([64.0, 0.0, 0.0], 1.0),
        upload_extent,
        &fixtures.work,
    );
    let work_borrowed = borrowed_leases(&fixtures.work, &work_leases, None);
    let first_work = gpu
        .execute_frame(
            presentation,
            &catalog,
            &work_intent,
            &work_requirements,
            &work_borrowed[..128],
        )
        .expect("first work-boundary frame executes");
    assert_eq!(first_work.visited_resources(), 128);
    counters.record(&first_work);
    let _ = poll_capture(
        &mut gpu,
        first_work
            .validation_capture()
            .expect("work capture exists"),
        deadline,
    );
    let second_work = gpu
        .execute_frame(
            presentation,
            &catalog,
            &work_intent,
            &work_requirements,
            &work_borrowed[128..],
        )
        .expect("second work-boundary frame executes");
    assert_eq!(second_work.visited_resources(), 1);
    counters.record(&second_work);
    let _ = poll_capture(
        &mut gpu,
        second_work
            .validation_capture()
            .expect("work capture exists"),
        deadline,
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
            let report = small_gpu
                .execute_frame(
                    small_presentation,
                    &catalog,
                    &intent,
                    &requirements,
                    &[lease],
                )
                .expect("small-ledger eviction sequence executes");
            assert_eq!(report.uploaded_resources(), 1);
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
        let replacement = small_gpu
            .execute_frame(
                small_presentation,
                &catalog,
                &replacement_intent,
                &replacement_requirements,
                &[first_lease],
            )
            .expect("evicted residency can be replaced from a genuine lease");
        assert_eq!(replacement.uploaded_resources(), 1);
        capacity_counters.record(&replacement);
        assert!(
            small_gpu.diagnostics().resident_payload_bytes()
                <= small_gpu.diagnostics().payload_capacity_bytes()
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
    drop(upload_borrowed);
    drop(work_borrowed);
    dataset_runtime
        .request_shutdown()
        .expect("fixture runtime begins bounded shutdown");
    drop(work_leases);
    drop(upload_leases);
    drop(leases);
    drop(dataset_runtime);

    let (lease_release_intent, lease_release_requirements) = intent_and_requirements(
        42,
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
