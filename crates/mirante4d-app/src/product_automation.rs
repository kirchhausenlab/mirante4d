use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, ApplicationSnapshot, CommandEffect, CrossSectionPanelId,
    OperationToken, PresentationSlot, ProjectStoreLifecycle, SourceVerificationSnapshot,
    WorkspaceSnapshot,
    import_workflow::{
        ImportCommand, ImportProgressSnapshot, ImportReviewDraft, ImportWorkflowSnapshot,
    },
    viewer_tools::{
        PickCompleteness, PickHit, PickHitKind, PickPolicy, PickValue, ScreenPosition,
        ViewerOverlayPhase, ViewerTool, ViewerToolContext, ViewerToolOverlay,
    },
    viewport_interaction::{
        CrossSectionPanel, CrossSectionViewState, fit_camera_to_shape_preserving_view,
        orbit_camera, pan_camera, zoom_camera,
    },
};
use mirante4d_domain::{
    CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, IsoShadingPolicy,
    LayerTransfer, Opacity, Projection, RenderMode, RenderState, SamplingPolicy, TimeIndex,
    UnitQuaternion, ViewerLayout, WorldPoint3,
};
use mirante4d_import_pipeline::{ImportReceipt, ImportStatistics, TiffSource};
use mirante4d_project_model::{LayerViewState, ProjectRevisionId};
use mirante4d_render_api::{RenderExtent, VolumePickQuery};
use mirante4d_storage::ScientificPublicationTransferEvidence;
use mirante4d_ui_egui::{ViewerPickPurpose, ViewerPickRequest};
use rustix::time::{ClockId, clock_gettime};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    DVR_DENSITY_SCALE_MAX, DVR_DENSITY_SCALE_MIN, DisplayedFrameFreshness, FrameCompleteness,
    MiranteWorkbenchApp, application_view, import_worker_service::ImportWorkerTimingOrigin,
    set_render_viewport, viewer_layout::PanelId,
};

mod capture;
mod diagnostics;
mod model;
mod progress;

use capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, capture_color_image,
    current_display_image_stats, product_target_capture, sanitize_artifact_label,
    write_color_image_ppm,
};
use diagnostics::{
    dataset_runtime_diagnostics_json, gpu_adapter_diagnostics_json,
    local_dataset_source_diagnostics_json, local_package_read_diagnostics_json,
};
use model::*;
use progress::ProductAutomationProgressPublisher;

const ENABLE_AUTOMATION_ENV: &str = "MIRANTE4D_ENABLE_AUTOMATION";
const AUTOMATION_SCRIPT_ENV: &str = "MIRANTE4D_AUTOMATION_SCRIPT";
const AUTOMATION_REPORT_ENV: &str = "MIRANTE4D_AUTOMATION_REPORT";
const AUTOMATION_PICK_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_DATASET_SWITCH_TIMEOUT: Duration = Duration::from_secs(120);

fn sequence_sample_target_ns(sample: u32, samples: u32, duration_ms: u64) -> u64 {
    if samples <= 1 {
        return 0;
    }
    let numerator = u128::from(duration_ms)
        .saturating_mul(1_000_000)
        .saturating_mul(u128::from(sample));
    u64::try_from(numerator / u128::from(samples - 1)).unwrap_or(u64::MAX)
}

fn application_cross_section_panel(panel: ProductAutomationPanelId) -> CrossSectionPanelId {
    match panel {
        ProductAutomationPanelId::Xy => CrossSectionPanelId::Xy,
        ProductAutomationPanelId::Xz => CrossSectionPanelId::Xz,
        ProductAutomationPanelId::Yz => CrossSectionPanelId::Yz,
    }
}

fn interaction_cross_section_panel(panel: ProductAutomationPanelId) -> CrossSectionPanel {
    match panel {
        ProductAutomationPanelId::Xy => CrossSectionPanel::Xy,
        ProductAutomationPanelId::Xz => CrossSectionPanel::Xz,
        ProductAutomationPanelId::Yz => CrossSectionPanel::Yz,
    }
}

fn product_presentation(
    app: &MiranteWorkbenchApp,
    panel: PanelId,
) -> Option<&mirante4d_render_api::PresentedFrame> {
    app.native_presentation
        .product_gpu
        .as_ref()?
        .targets
        .get(&panel)?
        .presented
        .as_ref()
}

fn product_presentations_ready(
    app: &mut MiranteWorkbenchApp,
    panels: &[PanelId],
) -> Result<bool, String> {
    app.poll_product_validation_captures()
        .map_err(|error| format!("failed to poll GPU validation capture: {error}"))?;
    Ok(panels
        .iter()
        .all(|panel| product_target_capture(app, *panel).is_some()))
}

fn product_capture_state(app: &MiranteWorkbenchApp, panels: &[PanelId]) -> String {
    panels
        .iter()
        .map(|panel| {
            let target = app
                .native_presentation
                .product_gpu
                .as_ref()
                .and_then(|product| product.targets.get(panel));
            match target {
                None => format!("{panel:?}:missing-target"),
                Some(target) => format!(
                    "{panel:?}:presented={:?},pending={:?},completed={:?}",
                    target.presented.as_ref().map(|frame| frame.frame()),
                    target
                        .pending_capture
                        .as_ref()
                        .map(|(frame, _)| frame.frame()),
                    target
                        .completed_capture
                        .as_ref()
                        .map(|(frame, _)| frame.frame()),
                ),
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn assertion_capture_panels(condition: &ProductAutomationAssertCondition) -> Vec<PanelId> {
    match condition {
        ProductAutomationAssertCondition::NonblankFrame => vec![PanelId::ThreeD],
        ProductAutomationAssertCondition::FourPanelImagesDistinct { .. } => {
            vec![PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz]
        }
        _ => Vec::new(),
    }
}
const AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
const AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
const AUTOMATION_SCHEMA_VERSION: u32 = 6;
const AUTOMATION_REPORT_SCHEMA_VERSION: u32 = 7;

fn dispatch_application_command(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    command: ApplicationCommand,
) -> Result<CommandEffect, String> {
    app.apply_application_command(command, ctx)
        .map_err(|fault| format!("application command was rejected: {fault:?}"))
}

fn dispatch_effective_interaction_sample(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    command: ApplicationCommand,
) -> Result<(), String> {
    match dispatch_application_command(app, ctx, command)? {
        CommandEffect::Changed => Ok(()),
        CommandEffect::NoChange => Err(
            "automation interaction sample produced no semantic change; refusing coalesced evidence"
                .to_owned(),
        ),
    }
}

fn layer_command(
    app: &MiranteWorkbenchApp,
    layer_index: usize,
    update: impl FnOnce(&LayerViewState) -> Result<LayerViewState, String>,
) -> Result<ApplicationCommand, String> {
    let snapshot = app.application.snapshot();
    let layer = application_view(&snapshot)
        .layers()
        .get(layer_index)
        .ok_or_else(|| format!("layer index {layer_index} is out of range"))?;
    Ok(ApplicationCommand::SetLayerView(update(layer)?))
}

fn active_layer_index(app: &MiranteWorkbenchApp) -> usize {
    let snapshot = app.application.snapshot();
    let view = application_view(&snapshot);
    view.layers()
        .iter()
        .position(|layer| layer.layer_key() == view.active_layer())
        .expect("application view has an active layer")
}

fn render_state_for_mode(
    current: RenderState,
    transfer: &LayerTransfer,
    mode: RenderMode,
) -> Result<RenderState, String> {
    let sampling = current.sampling_policy();
    match mode {
        RenderMode::Mip => Ok(RenderState::mip(sampling)),
        RenderMode::Isosurface => {
            let level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(0.5);
            let shading = current
                .iso_parameters()
                .map_or(IsoShadingPolicy::GradientLighting, |parameters| {
                    parameters.shading_policy()
                });
            RenderState::iso(sampling, shading, level).map_err(|error| error.to_string())
        }
        RenderMode::Dvr => {
            let (opacity_transfer, density) = current
                .dvr_parameters()
                .map(|parameters| (parameters.opacity_transfer(), parameters.density_scale()))
                .unwrap_or((
                    DvrOpacityTransfer::new(transfer.window(), transfer.curve()),
                    12.0,
                ));
            RenderState::dvr(sampling, opacity_transfer, density).map_err(|error| error.to_string())
        }
    }
}

fn render_state_with_sampling(
    current: RenderState,
    sampling: SamplingPolicy,
) -> Result<RenderState, String> {
    if current.mip_parameters().is_some() {
        Ok(RenderState::mip(sampling))
    } else if let Some(parameters) = current.dvr_parameters() {
        RenderState::dvr(
            sampling,
            parameters.opacity_transfer(),
            parameters.density_scale(),
        )
        .map_err(|error| error.to_string())
    } else if let Some(parameters) = current.iso_parameters() {
        RenderState::iso(
            sampling,
            parameters.shading_policy(),
            parameters.display_level(),
        )
        .map_err(|error| error.to_string())
    } else {
        Err("the layer has no supported render mode".to_owned())
    }
}

fn camera_with_projection(
    camera: CameraView,
    projection: Projection,
) -> Result<CameraView, String> {
    CameraView::new(
        projection,
        camera.target(),
        camera.orientation(),
        camera.orthographic_world_per_screen_point(),
        camera.perspective_focal_length_screen_points(),
        camera.perspective_view_distance_world(),
    )
    .map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn exact_cross_section_view(
    center_world: [f64; 3],
    orientation_xyzw: [f64; 4],
    scale_world_per_screen_point: f64,
    depth_world: f64,
) -> Result<CrossSectionView, String> {
    let center = WorldPoint3::new(center_world[0], center_world[1], center_world[2])
        .map_err(|error| format!("cross-section center was rejected: {error}"))?;
    let orientation = UnitQuaternion::new_xyzw(
        orientation_xyzw[0],
        orientation_xyzw[1],
        orientation_xyzw[2],
        orientation_xyzw[3],
    )
    .map_err(|error| format!("cross-section orientation was rejected: {error}"))?;
    CrossSectionView::new(
        center,
        orientation,
        scale_world_per_screen_point,
        depth_world,
    )
    .map_err(|error| format!("cross-section view was rejected: {error}"))
}

fn automation_pick_request(
    app: &MiranteWorkbenchApp,
    x_fraction: f32,
    y_fraction: f32,
    purpose: ViewerPickPurpose,
) -> Result<Option<ViewerPickRequest>, String> {
    if !x_fraction.is_finite()
        || !y_fraction.is_finite()
        || !(0.0..=1.0).contains(&x_fraction)
        || !(0.0..=1.0).contains(&y_fraction)
    {
        return Err(format!(
            "{} fractions must be finite and between 0.0 and 1.0",
            automation_pick_purpose_name(purpose)
        ));
    }
    if app.native_presentation.product_gpu.is_none() {
        return Err("native GPU presentation is unavailable for product picking".to_owned());
    }

    // Product presentation frames are native-renderer state projected into the
    // UI snapshot; the reducer snapshot alone intentionally carries no live
    // presentation. Use the same current-frame authority as the pick pump so
    // automation cannot wait forever without ever enqueueing a GPU request.
    let snapshot = app.application_snapshot_for_ui();
    let view = application_view(&snapshot);
    let tool = ViewerTool::from(snapshot.transient().active_tool());
    if tool == ViewerTool::Navigate {
        return Err(format!(
            "{} requires a non-navigation viewer tool",
            automation_pick_purpose_name(purpose)
        ));
    }
    let active_layer = view
        .layer(view.active_layer())
        .expect("application view has an active layer");
    if !active_layer.visible() {
        return Err("the active layer is hidden and cannot be picked".to_owned());
    }
    let Some(presented) = snapshot
        .presentations()
        .get(PresentationSlot::ThreeD)
        .and_then(|surface| surface.current_frame())
    else {
        return Ok(None);
    };

    let extent = presented.extent();
    let width = f64::from(extent.width_pixels());
    let height = f64::from(extent.height_pixels());
    let render_pixel = [
        (f64::from(x_fraction) * width - 0.5).clamp(0.0, width - 1.0),
        (f64::from(y_fraction) * height - 0.5).clamp(0.0, height - 1.0),
    ];
    let policy = match active_layer.render_state().mode() {
        RenderMode::Mip => PickPolicy::MipArgmax,
        RenderMode::Isosurface => PickPolicy::FirstThresholdHit,
        RenderMode::Dvr => PickPolicy::MaximumOpacityContribution,
    };
    let query = VolumePickQuery::new(
        presented,
        view.timepoint(),
        view.active_layer(),
        render_pixel,
        policy,
    )
    .map_err(|error| format!("automation pick query was rejected: {error}"))?;
    let context = ViewerToolContext::new(
        snapshot.source_generation(),
        view.timepoint(),
        view.active_layer(),
    );
    let presentation = app
        .render_coordination
        .surface(PresentationSlot::ThreeD)
        .presentation_viewport()
        .unwrap_or(app.render_coordination.presentation_viewport);
    let screen_position = ScreenPosition::new(
        x_fraction * presentation.width_points() as f32,
        y_fraction * presentation.height_points() as f32,
    );
    ViewerPickRequest::new(query, context, tool, purpose, screen_position)
        .map(Some)
        .ok_or_else(|| {
            "automation pick request was rejected by the viewer-tool contract".to_owned()
        })
}

fn automation_pick_purpose_name(purpose: ViewerPickPurpose) -> &'static str {
    match purpose {
        ViewerPickPurpose::Hover => "probe_hover",
        ViewerPickPurpose::PrimaryClick => "primary_click",
    }
}

fn frame_freshness_is_current(fidelity: &crate::FrameFidelityStatus) -> bool {
    fidelity.display_freshness == DisplayedFrameFreshness::Current
}

fn coordinated_visible_layout_current_complete(app: &MiranteWorkbenchApp) -> bool {
    coordinated_visible_layout_current_complete_with_snapshot(app, &app.application.snapshot())
}

fn coordinated_visible_layout_current_complete_with_snapshot(
    app: &MiranteWorkbenchApp,
    snapshot: &ApplicationSnapshot,
) -> bool {
    let generation = app.render_coordination.display_generation();
    if generation.current_presentation_generation != Some(generation.input_generation)
        || app.dataset.staging_current_refinement()
        || app.dataset.current_scale().get()
            != app.render_coordination.frame_fidelity.target_scale_level
        || app
            .display_performance_milestones
            .target_settled_ms()
            .is_none()
    {
        return false;
    }
    match application_view(snapshot).layout() {
        ViewerLayout::Single3d => true,
        ViewerLayout::FourPanel => [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ]
        .into_iter()
        .all(|slot| {
            let surface = app.render_coordination.surface(slot);
            surface.display_current()
                && surface.cross_section_schedule().is_some_and(|schedule| {
                    schedule.status == crate::CrossSectionPanelScheduleStatus::Current
                })
                && surface.layer_presentations().iter().all(|layer| {
                    layer.current && layer.available_requirements == layer.total_requirements
                })
        }),
    }
}

const fn automation_runtime_is_idle(
    background_work_active: bool,
    camera_demand_planning_active: bool,
    prepared_demand_install_pending: bool,
) -> bool {
    !background_work_active && !camera_demand_planning_active && !prepared_demand_install_pending
}

pub(super) fn cancel_active_source_verification(
    app: &mut MiranteWorkbenchApp,
) -> Result<Value, String> {
    let automatic_request_pending = app.pending_automatic_source_verification.is_some();
    // Observe a verifier that reached its commit point before attempting
    // cancellation; committed promotion must win over cancellation.
    app.pump_application_services();
    if app.pending_automatic_source_verification.is_some() {
        app.try_start_pending_automatic_source_verification();
        if app.pending_automatic_source_verification.is_some() {
            return Err(
                "automatic source verification could not enter a cancellable state".to_owned(),
            );
        }
    }
    let snapshot = app.application.snapshot();
    let operation_id = match snapshot.source() {
        SourceVerificationSnapshot::Verifying { operation_id, .. } => Some(*operation_id),
        SourceVerificationSnapshot::Required | SourceVerificationSnapshot::Verified(_) => None,
    };
    if let Some(operation_id) = operation_id {
        app.application
            .dispatch(ApplicationCommand::CancelOperation(operation_id))
            .map_err(|fault| {
                format!("active source-verification cancellation was rejected: {fault:?}")
            })?;
        app.pump_application_services();
    }
    Ok(json!({
        "active_operation_observed": operation_id.is_some(),
        "cancellation_requested": operation_id.is_some(),
        "automatic_request_dispatched": automatic_request_pending,
    }))
}

pub(super) fn source_verification_inactive(app: &MiranteWorkbenchApp) -> bool {
    let snapshot = app.application.snapshot();
    matches!(
        snapshot.source(),
        SourceVerificationSnapshot::Required | SourceVerificationSnapshot::Verified(_)
    ) && app
        .source_verification_service
        .as_ref()
        .is_some_and(|service| service.active_token().is_none())
        && app.pending_automatic_source_verification.is_none()
        && !app.dataset.source_quarantined()
}

fn automation_pick_json(
    request: ViewerPickRequest,
    hit: &PickHit,
    x_fraction: f32,
    y_fraction: f32,
) -> Value {
    let kind = match hit.kind {
        PickHitKind::Empty => "empty",
        PickHitKind::Voxel => "voxel",
        PickHitKind::InterpolatedSample => "interpolated_sample",
    };
    let completeness = match hit.completeness {
        PickCompleteness::Exact => "exact",
        PickCompleteness::Approximate => "approximate",
        PickCompleteness::Incomplete => "incomplete",
        PickCompleteness::Loading => "loading",
    };
    let policy = match hit.policy {
        PickPolicy::FirstThresholdHit => "first_threshold_hit",
        PickPolicy::MipArgmax => "mip_argmax",
        PickPolicy::MaximumOpacityContribution => "maximum_opacity_contribution",
    };
    let value = match hit.value {
        Some(PickValue::IntensityU8(value)) => json!({ "dtype": "uint8", "value": value }),
        Some(PickValue::IntensityU16(value)) => json!({ "dtype": "uint16", "value": value }),
        Some(PickValue::IntensityF32(value)) => json!({ "dtype": "float32", "value": value }),
        None => Value::Null,
    };
    json!({
        "x_fraction": x_fraction,
        "y_fraction": y_fraction,
        "purpose": automation_pick_purpose_name(request.purpose()),
        "status": if hit.kind == PickHitKind::Empty { "empty" } else { "sampled" },
        "kind": kind,
        "completeness": completeness,
        "policy": policy,
        "value": value,
        "world_position": hit.world_position.map(|position| position.components()),
        "render_pixel": request.query().render_pixel(),
        "placeholder_sampled": false,
        "native_gpu_pick": true,
    })
}

pub(crate) struct ProductAutomationController {
    script: ProductAutomationScript,
    script_path: PathBuf,
    report_path: PathBuf,
    progress: Option<ProductAutomationProgressPublisher>,
    command_index: usize,
    active_dataset_switch: Option<ActiveDatasetSwitch>,
    active_wait_started: Option<Instant>,
    sleep_frames_remaining: Option<u32>,
    active_input_sequence: Option<ActiveInputSequence>,
    started_at_epoch_ms: u128,
    started_at: Instant,
    started_process_cpu_time_ns: Option<u64>,
    events: Vec<ProductAutomationEvent>,
    diagnostics: Vec<Value>,
    artifacts: Vec<ProductAutomationArtifact>,
    limit_observations: ProductAutomationLimitObservations,
    render_target_override: Option<RenderExtent>,
    requested_mapped_client_pixels: Option<(u32, u32)>,
    projected_import_stages: Vec<&'static str>,
    maximum_projected_import_elapsed_ms: u64,
    active_import_pre_start_origin: Option<ImportPreStartOrigin>,
    completed_import_pre_start_measurement: Option<ImportPreStartMeasurement>,
    active_import_timing_origin: Option<ImportWorkerTimingOrigin>,
    active_import_verification_diagnostics_origin:
        Option<crate::current_source_verification_service::CurrentSourceVerificationDiagnostics>,
    completed_import_primary_measurement: Option<ImportPrimaryMeasurement>,
    imported_open_ready_outcome: Option<ImportedOpenReadyOutcome>,
    report_written: bool,
}

#[derive(Clone, Debug)]
struct ActiveDatasetSwitch {
    command_index: usize,
    started_at: Instant,
    token: OperationToken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatasetSwitchProtocolDecision {
    TargetAlreadySelected,
    Start,
    Waiting,
    Installed,
    Failed,
    TimedOut,
}

fn dataset_switch_protocol_decision(
    selected_matches: bool,
    request_started: bool,
    exact_request_pending: bool,
    elapsed: Duration,
    timeout: Duration,
) -> DatasetSwitchProtocolDecision {
    if !request_started {
        return if selected_matches {
            DatasetSwitchProtocolDecision::TargetAlreadySelected
        } else {
            DatasetSwitchProtocolDecision::Start
        };
    }
    if selected_matches {
        return DatasetSwitchProtocolDecision::Installed;
    }
    if !exact_request_pending {
        return DatasetSwitchProtocolDecision::Failed;
    }
    if elapsed >= timeout {
        return DatasetSwitchProtocolDecision::TimedOut;
    }
    DatasetSwitchProtocolDecision::Waiting
}

fn exact_dataset_switch_pending(app: &MiranteWorkbenchApp, token: &OperationToken) -> bool {
    app.source_open_service
        .as_ref()
        .and_then(|service| service.active_token())
        == Some(token)
        || app
            .pending_source_install
            .as_ref()
            .is_some_and(|pending| &pending.token == token)
        || app
            .application
            .snapshot()
            .active_operations()
            .iter()
            .any(|active| active == token)
}

fn cancel_exact_dataset_switch(app: &mut MiranteWorkbenchApp, token: &OperationToken) -> String {
    let mut outcomes = Vec::new();
    let mut reducer_cancel_requested = false;
    let reducer_active = app
        .application
        .snapshot()
        .active_operations()
        .iter()
        .any(|active| active == token);
    if reducer_active {
        match app
            .application
            .dispatch(ApplicationCommand::CancelOperation(token.operation_id()))
        {
            Ok(_) => {
                reducer_cancel_requested = true;
                outcomes.push("reducer_operation_cancel_requested".to_owned());
                app.pump_application_services();
            }
            Err(fault) => {
                outcomes.push(format!("reducer_operation_cancel_rejected:{fault:?}"));
                if app.complete_source_operation(
                    token.clone(),
                    mirante4d_application::OperationCompletion::Failed(
                        mirante4d_application::OperationFailureCode::DatasetReadFailed,
                    ),
                ) {
                    outcomes.push("reducer_operation_failed_closed".to_owned());
                }
            }
        }
    }

    let service_active = app
        .source_open_service
        .as_ref()
        .and_then(|service| service.active_token())
        == Some(token);
    if service_active && !reducer_cancel_requested {
        let outcome = match app
            .source_open_service
            .as_ref()
            .expect("the exact active source-open service was just observed")
            .cancel(token)
        {
            Ok(()) => "source_open_service_cancel_requested".to_owned(),
            Err(error) => format!("source_open_service_cancel_rejected:{error}"),
        };
        outcomes.push(outcome);
    }

    if app
        .pending_source_install
        .as_ref()
        .is_some_and(|pending| &pending.token == token)
    {
        app.abort_pending_source_install(
            "The timed-out automated dataset switch was cancelled before installation.",
        );
        outcomes.push("prepared_install_suppressed".to_owned());
    }

    if outcomes.is_empty() {
        "exact_operation_already_terminal".to_owned()
    } else {
        outcomes.join("+")
    }
}

#[derive(Clone, Copy, Debug)]
struct ActiveInputSequence {
    command_index: usize,
    started_at: Instant,
    next_sample: u32,
    samples: u32,
    duration_ms: u64,
    origin_generation: u64,
    origin_durable_commits: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputSequenceStep {
    Wait(Duration),
    Dispatch(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportPrimaryMeasurement {
    started_at_epoch_ms: u128,
    open_ready_at_epoch_ms: u128,
    wall_time_ns: u64,
    process_cpu_time_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportPublicationToOpenReadyMeasurement {
    published_at_epoch_ms: u128,
    open_ready_at_epoch_ms: u128,
    wall_time_ns: u64,
    process_cpu_time_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportPublicationCurrentnessExecution {
    contract_id: &'static str,
    expected_snapshot_object_reads: u64,
    first_inventory_object_reads: u64,
    observed_snapshot_object_reads: u64,
    second_inventory_object_reads: u64,
    observed_total_object_reads: u64,
    observed_codec_decode_calls: u64,
}

impl From<ScientificPublicationTransferEvidence> for ImportPublicationCurrentnessExecution {
    fn from(evidence: ScientificPublicationTransferEvidence) -> Self {
        Self {
            contract_id: evidence.contract_id(),
            expected_snapshot_object_reads: evidence.expected_snapshot_object_reads(),
            first_inventory_object_reads: evidence.first_inventory_object_reads(),
            observed_snapshot_object_reads: evidence.observed_snapshot_object_reads(),
            second_inventory_object_reads: evidence.second_inventory_object_reads(),
            observed_total_object_reads: evidence.observed_total_object_reads(),
            observed_codec_decode_calls: evidence.observed_codec_decode_calls(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportPublicationEvidenceSnapshot {
    publication_currentness: ImportPublicationCurrentnessExecution,
    source_verification_started_runs: u64,
    source_verification_progress_updates: u64,
    source_verification_cancelled_runs: u64,
    source_verification_failed_runs: u64,
    source_verification_successes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportVerificationEvidenceSnapshot {
    source_verification_started_runs: u64,
    source_verification_progress_updates: u64,
    source_verification_cancelled_runs: u64,
    source_verification_failed_runs: u64,
    source_verification_successes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportedOpenReadyOutcome {
    measurement: ImportPublicationToOpenReadyMeasurement,
    evidence: ImportPublicationEvidenceSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportedOpenReadyReadiness {
    selected_matches: bool,
    verified: bool,
    import_idle: bool,
    problem_absent: bool,
}

impl ImportedOpenReadyReadiness {
    const fn condition_met(self) -> bool {
        self.selected_matches && self.verified && self.import_idle && self.problem_absent
    }
}

fn imported_open_ready_readiness(
    app: &MiranteWorkbenchApp,
    snapshot: &ApplicationSnapshot,
    path: &Path,
) -> ImportedOpenReadyReadiness {
    ImportedOpenReadyReadiness {
        selected_matches: normalize_path(app.dataset.selected_path()) == normalize_path(path),
        verified: matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_))
            && app
                .source_verification_service
                .as_ref()
                .is_some_and(|service| service.active_token().is_none()),
        import_idle: !app.import.workers.status().is_active(),
        problem_absent: app.import.problem.is_none(),
    }
}

struct ImportedOpenReadyCommitState<'a, Origin, VerificationOrigin, Outcome> {
    active_origin: &'a mut Option<Origin>,
    active_verification_origin: &'a mut Option<VerificationOrigin>,
    completed_primary: &'a mut Option<ImportPrimaryMeasurement>,
    open_ready_outcome: &'a mut Option<Outcome>,
}

impl<Origin, VerificationOrigin, Outcome>
    ImportedOpenReadyCommitState<'_, Origin, VerificationOrigin, Outcome>
{
    fn commit(self, primary: ImportPrimaryMeasurement, outcome: Outcome) {
        *self.open_ready_outcome = Some(outcome);
        *self.completed_primary = Some(primary);
        *self.active_verification_origin = None;
        *self.active_origin = None;
    }
}

#[derive(Clone, Debug)]
struct ImportPreStartOrigin {
    started_at_epoch_ms: u128,
    started_at: Instant,
    process_cpu_time_ns: u64,
    destination: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportPreStartMeasurement {
    started_at_epoch_ms: u128,
    start_command_at_epoch_ms: u128,
    wall_time_ns: u64,
    process_cpu_time_ns: u64,
}

impl ProductAutomationController {
    pub(crate) fn from_env() -> Option<Self> {
        env::var(ENABLE_AUTOMATION_ENV)
            .ok()
            .filter(|value| value == "1" || value.eq_ignore_ascii_case("true"))?;
        Some(match Self::load_from_env() {
            Ok(controller) => controller,
            Err(err) => Self::failed_to_initialize(err.to_string()),
        })
    }

    pub(crate) fn drive(app: &mut MiranteWorkbenchApp, ctx: &egui::Context) {
        let Some(mut automation) = app.product_automation.take() else {
            return;
        };
        let status = match automation.publish_progress_if_due() {
            Ok(()) => automation.step(app, ctx),
            Err(reason) => AutomationStatus::Failed(reason),
        };
        // Automation submits through the same one-pending/one-latest native
        // pick queue as interactive UI input. The normal pump ran before the
        // automation command in this frame, so pump once more to submit a new
        // request without waiting an extra frame or introducing another path.
        app.pump_viewer_pick(ctx);
        match status {
            AutomationStatus::Continue => {
                ctx.request_repaint();
            }
            AutomationStatus::Waiting { repaint_after } => {
                if let Some(delay) = automation.progress_repaint_after(repaint_after) {
                    ctx.request_repaint_after(delay);
                }
            }
            AutomationStatus::Finished => match automation.publish_progress_closeout() {
                Ok(()) => automation.write_report_and_close(app, ctx, "passed", None),
                Err(reason) => {
                    automation.write_report_and_close(app, ctx, "failed", Some(reason));
                }
            },
            AutomationStatus::Failed(mut reason) => {
                if let Err(progress_reason) = automation.publish_progress_closeout() {
                    reason.push_str("; ");
                    reason.push_str(&progress_reason);
                }
                automation.write_report_and_close(app, ctx, "failed", Some(reason));
            }
        }
        app.product_automation = Some(automation);
    }

    fn load_from_env() -> anyhow::Result<Self> {
        let script_path = env::var_os(AUTOMATION_SCRIPT_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("{AUTOMATION_SCRIPT_ENV} is required"))?;
        let report_path = env::var_os(AUTOMATION_REPORT_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("{AUTOMATION_REPORT_ENV} is required"))?;
        let raw = fs::read_to_string(&script_path)
            .map_err(|err| anyhow::anyhow!("failed to read {}: {err}", script_path.display()))?;
        let script: ProductAutomationScript = serde_json::from_str(&raw)
            .map_err(|err| anyhow::anyhow!("failed to parse {}: {err}", script_path.display()))?;
        script.validate()?;
        let progress = ProductAutomationProgressPublisher::from_env()?;
        Ok(Self::new(script, script_path, report_path).with_progress(progress))
    }

    fn new(script: ProductAutomationScript, script_path: PathBuf, report_path: PathBuf) -> Self {
        Self {
            script,
            script_path,
            report_path,
            progress: None,
            command_index: 0,
            active_dataset_switch: None,
            active_wait_started: None,
            sleep_frames_remaining: None,
            active_input_sequence: None,
            started_at_epoch_ms: epoch_ms(),
            started_at: Instant::now(),
            started_process_cpu_time_ns: checked_process_cpu_time_ns(),
            events: Vec::new(),
            diagnostics: Vec::new(),
            artifacts: Vec::new(),
            limit_observations: ProductAutomationLimitObservations::default(),
            render_target_override: None,
            requested_mapped_client_pixels: None,
            projected_import_stages: Vec::new(),
            maximum_projected_import_elapsed_ms: 0,
            active_import_pre_start_origin: None,
            completed_import_pre_start_measurement: None,
            active_import_timing_origin: None,
            active_import_verification_diagnostics_origin: None,
            completed_import_primary_measurement: None,
            imported_open_ready_outcome: None,
            report_written: false,
        }
    }

    fn with_progress(mut self, progress: Option<ProductAutomationProgressPublisher>) -> Self {
        self.progress = progress;
        self
    }

    fn publish_progress_if_due(&mut self) -> Result<(), String> {
        let Some(command) = self.script.commands.get(self.command_index) else {
            return Ok(());
        };
        let command_count = self.script.commands.len();
        let command_index = self.command_index;
        let command_kind = command.name();
        let now = Instant::now();
        let result = self.progress.as_mut().map_or(Ok(false), |progress| {
            progress.publish_command_if_due(command_count, command_index, command_kind, now)
        });
        result
            .map(|_| ())
            .map_err(|_| "product automation progress publication failed".to_owned())
    }

    fn publish_progress_closeout(&mut self) -> Result<(), String> {
        let command_count = self.script.commands.len();
        let now = Instant::now();
        let result = self.progress.as_mut().map_or(Ok(()), |progress| {
            progress.publish_closeout(command_count, now)
        });
        result.map_err(|_| "product automation progress closeout publication failed".to_owned())
    }

    fn progress_repaint_after(&mut self, requested: Option<Duration>) -> Option<Duration> {
        self.progress.as_ref().map_or(requested, |progress| {
            progress.clamp_repaint_after(requested, Instant::now())
        })
    }

    pub(crate) const fn render_target_override(&self) -> Option<RenderExtent> {
        self.render_target_override
    }

    pub(crate) fn requires_validation_capture(&self) -> bool {
        self.script.requires_validation_capture()
    }

    pub(crate) const fn requires_gpu_timing(&self) -> bool {
        self.script.requires_gpu_timing()
    }

    fn input_sequence_step(
        &mut self,
        app: &MiranteWorkbenchApp,
        samples: u32,
        duration_ms: u64,
    ) -> InputSequenceStep {
        let sequence = self.active_input_sequence.get_or_insert_with(|| {
            let generation = app.render_coordination.display_generation();
            ActiveInputSequence {
                command_index: self.command_index,
                started_at: Instant::now(),
                next_sample: 0,
                samples,
                duration_ms,
                origin_generation: generation.input_generation,
                origin_durable_commits: generation.durable_gesture_commits,
            }
        });
        debug_assert_eq!(sequence.command_index, self.command_index);
        debug_assert_eq!(sequence.samples, samples);
        debug_assert_eq!(sequence.duration_ms, duration_ms);
        let target_ns =
            sequence_sample_target_ns(sequence.next_sample, sequence.samples, sequence.duration_ms);
        let elapsed_ns =
            u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if elapsed_ns < target_ns {
            InputSequenceStep::Wait(Duration::from_nanos(target_ns - elapsed_ns))
        } else {
            InputSequenceStep::Dispatch(sequence.next_sample)
        }
    }

    fn complete_input_sequence_sample(
        &mut self,
        app: &MiranteWorkbenchApp,
        workload: Value,
    ) -> CommandProgress {
        let sequence = self
            .active_input_sequence
            .as_mut()
            .expect("a dispatched automation sequence sample has active state");
        sequence.next_sample = sequence.next_sample.saturating_add(1);
        if sequence.next_sample < sequence.samples {
            let target_ns = sequence_sample_target_ns(
                sequence.next_sample,
                sequence.samples,
                sequence.duration_ms,
            );
            let elapsed_ns =
                u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
            return CommandProgress::PassiveWaiting(Some(Duration::from_nanos(
                target_ns.saturating_sub(elapsed_ns),
            )));
        }
        let sequence = self
            .active_input_sequence
            .take()
            .expect("the completed automation sequence retains its state");
        let generation = app.render_coordination.display_generation();
        CommandProgress::Done(json!({
            "workload": workload,
            "samples": sequence.samples,
            "requested_duration_ms": sequence.duration_ms,
            "actual_duration_ns": u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
            "input_evidence": {
                "automation_level": "E1_semantic_application_commands",
                "os_input_injected": false,
                "os_input_claimed": false,
                "time_distributed_across_app_updates": true,
            },
            "observed_counter_delta": {
                "input_generations": generation.input_generation.saturating_sub(sequence.origin_generation),
                "durable_gesture_commits": generation.durable_gesture_commits.saturating_sub(sequence.origin_durable_commits),
            },
        }))
    }

    fn failed_to_initialize(reason: String) -> Self {
        let report_path = env::var_os(AUTOMATION_REPORT_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/mirante4d/product-automation-failed.json"));
        let mut controller = Self::new(
            ProductAutomationScript::empty_failed_script(),
            env::var_os(AUTOMATION_SCRIPT_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("<missing-script>")),
            report_path,
        );
        controller.events.push(ProductAutomationEvent::failed(
            0,
            "initialize",
            Duration::ZERO,
            reason.clone(),
        ));
        controller.command_index = controller.script.commands.len();
        controller
    }

    fn step(&mut self, app: &mut MiranteWorkbenchApp, ctx: &egui::Context) -> AutomationStatus {
        if self.report_written {
            return AutomationStatus::Waiting {
                repaint_after: None,
            };
        }
        if self.command_index >= self.script.commands.len() {
            if self.events.iter().any(|event| event.status == "failed") {
                return AutomationStatus::Failed("automation initialization failed".to_owned());
            }
            return AutomationStatus::Finished;
        }

        let command = self.script.commands[self.command_index].clone();
        let command_index = self.command_index;
        if self
            .active_input_sequence
            .is_some_and(|sequence| sequence.command_index != command_index)
        {
            self.active_input_sequence = None;
        }
        let command_started = Instant::now();
        // Validation readback is asynchronous renderer work. Drive it before
        // evaluating waits so `runtime_idle` can settle even when the next
        // script command, rather than another render, is the first consumer.
        if let Err(error) = app.poll_product_validation_captures() {
            let mut reason = format!("failed to poll GPU validation capture: {error}");
            if let Some(cancellation) = self.cancel_active_dataset_switch(app) {
                reason.push_str(&format!("; dataset_switch_cancellation={cancellation}"));
            }
            return self.record_fatal_command_failure(
                command_index,
                command.name(),
                command_started.elapsed(),
                reason,
            );
        }
        self.observe_import_projection(app);
        let result = self.execute_command(app, ctx, &command);
        if let Err(reason) = self.observe_and_enforce_hard_safety_limits(app) {
            let reason = if let Some(cancellation) = self.cancel_active_dataset_switch(app) {
                format!("{reason}; dataset_switch_cancellation={cancellation}")
            } else {
                reason
            };
            return self.record_fatal_command_failure(
                command_index,
                command.name(),
                command_started.elapsed(),
                reason,
            );
        }
        match result {
            Ok(CommandProgress::Done(details)) => {
                self.record_successful_command(
                    command_index,
                    command.name(),
                    command_started.elapsed(),
                    details,
                );
                AutomationStatus::Continue
            }
            Ok(CommandProgress::Waiting) => AutomationStatus::Waiting {
                repaint_after: Some(Duration::from_millis(16)),
            },
            Ok(CommandProgress::PassiveWaiting(repaint_after)) => {
                AutomationStatus::Waiting { repaint_after }
            }
            Err(reason) => {
                let reason = if let Some(cancellation) = self.cancel_active_dataset_switch(app) {
                    format!("{reason}; dataset_switch_cancellation={cancellation}")
                } else {
                    reason
                };
                self.record_fatal_command_failure(
                    command_index,
                    command.name(),
                    command_started.elapsed(),
                    reason,
                )
            }
        }
    }

    fn record_fatal_command_failure(
        &mut self,
        command_index: usize,
        command: &'static str,
        duration: Duration,
        reason: String,
    ) -> AutomationStatus {
        self.events.push(ProductAutomationEvent::failed(
            command_index,
            command,
            duration,
            reason.clone(),
        ));
        AutomationStatus::Failed(reason)
    }

    fn record_successful_command(
        &mut self,
        command_index: usize,
        command: &'static str,
        duration: Duration,
        details: Value,
    ) {
        let completed_at = Instant::now();
        self.events.push(ProductAutomationEvent::passed(
            command_index,
            command,
            duration,
            details,
        ));
        self.active_wait_started = None;
        self.sleep_frames_remaining = None;
        if self.command_index == command_index {
            self.command_index += 1;
        }
        if let Some(progress) = self.progress.as_mut() {
            progress.observe_command(self.command_index, completed_at);
        }
    }

    fn cancel_active_dataset_switch(&mut self, app: &mut MiranteWorkbenchApp) -> Option<String> {
        self.active_dataset_switch
            .take()
            .map(|active| cancel_exact_dataset_switch(app, &active.token))
    }

    fn execute_dataset_switch(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        ctx: &egui::Context,
        path: &Path,
    ) -> Result<CommandProgress, String> {
        let expected = normalize_path(path);
        let selected_matches = normalize_path(app.dataset.selected_path()) == expected;
        let active = self.active_dataset_switch.as_ref();
        if let Some(active) = active
            && active.command_index != self.command_index
        {
            return Err(
                "dataset-switch state belongs to a different automation command".to_owned(),
            );
        }
        let exact_request_pending =
            active.is_some_and(|active| exact_dataset_switch_pending(app, &active.token));
        let elapsed = active.map_or(Duration::ZERO, |active| active.started_at.elapsed());
        match dataset_switch_protocol_decision(
            selected_matches,
            active.is_some(),
            exact_request_pending,
            elapsed,
            AUTOMATION_DATASET_SWITCH_TIMEOUT,
        ) {
            DatasetSwitchProtocolDecision::TargetAlreadySelected => Err(format!(
                "switch_dataset requires a distinct target, but {} is already selected; use open_dataset for the startup assertion",
                path.display()
            )),
            DatasetSwitchProtocolDecision::Start => {
                let started_at = Instant::now();
                let dispatched = app
                    .open_or_queue_dataset_path(path.to_path_buf(), Some(ctx))
                    .map_err(|error| {
                        format!(
                            "normal product dataset switch to {} was rejected: {error}",
                            path.display()
                        )
                    })?;
                if !dispatched {
                    return Err(
                        "switch_dataset requires a clean current project; the product retained the request for an interactive dirty-project decision"
                            .to_owned(),
                    );
                }
                let token = app
                    .source_open_service
                    .as_ref()
                    .and_then(|service| service.active_token())
                    .cloned()
                    .ok_or_else(|| {
                        "switch_dataset dispatched no bounded current-source open operation"
                            .to_owned()
                    })?;
                if !app
                    .application
                    .snapshot()
                    .active_operations()
                    .iter()
                    .any(|active| active == &token)
                {
                    let cancellation = cancel_exact_dataset_switch(app, &token);
                    return Err(format!(
                        "switch_dataset source service has no matching reducer operation; cancellation={cancellation}"
                    ));
                }
                self.active_dataset_switch = Some(ActiveDatasetSwitch {
                    command_index: self.command_index,
                    started_at,
                    token,
                });
                Ok(CommandProgress::Waiting)
            }
            DatasetSwitchProtocolDecision::Waiting => Ok(CommandProgress::Waiting),
            DatasetSwitchProtocolDecision::Installed => {
                let active = self
                    .active_dataset_switch
                    .take()
                    .expect("an installed dataset switch retains its exact operation state");
                Ok(CommandProgress::Done(json!({
                    "mode": "normal_product_external_dataset_switch",
                    "path": app.dataset.selected_path().display().to_string(),
                    "operation_id": active.token.operation_id().get(),
                    "normal_current_source_open_service": true,
                    "external_open_requests": 1,
                    "timeout_ms": AUTOMATION_DATASET_SWITCH_TIMEOUT.as_millis(),
                    "waited_ms": duration_ms(active.started_at.elapsed()),
                })))
            }
            DatasetSwitchProtocolDecision::Failed => {
                let active = self
                    .active_dataset_switch
                    .take()
                    .expect("a failed dataset switch retains its exact operation state");
                Err(format!(
                    "normal product dataset switch operation {} finished without installing {} after {:.3} ms",
                    active.token.operation_id().get(),
                    path.display(),
                    duration_ms(active.started_at.elapsed()),
                ))
            }
            DatasetSwitchProtocolDecision::TimedOut => {
                let active = self
                    .active_dataset_switch
                    .take()
                    .expect("a timed-out dataset switch retains its exact operation state");
                let cancellation = cancel_exact_dataset_switch(app, &active.token);
                Err(format!(
                    "timed out after {} ms waiting for normal product dataset switch operation {} to install {}; cancellation={cancellation}",
                    AUTOMATION_DATASET_SWITCH_TIMEOUT.as_millis(),
                    active.token.operation_id().get(),
                    path.display(),
                ))
            }
        }
    }

    fn capture_import_verification_evidence(
        &self,
        app: &MiranteWorkbenchApp,
    ) -> Result<ImportVerificationEvidenceSnapshot, String> {
        let verification_origin = self
            .active_import_verification_diagnostics_origin
            .as_ref()
            .ok_or_else(|| {
                "import publication has no source-verification diagnostics origin".to_owned()
            })?;
        let verification_current = app
            .source_verification_service
            .as_ref()
            .ok_or_else(|| "import publication has no source-verification diagnostics".to_owned())?
            .diagnostics();
        let source_verification_started_runs = verification_current
            .started_runs
            .checked_sub(verification_origin.started_runs)
            .ok_or_else(|| "source-verification started-run counter regressed".to_owned())?;
        let source_verification_progress_updates = verification_current
            .accepted_progress_updates
            .checked_sub(verification_origin.accepted_progress_updates)
            .ok_or_else(|| "source-verification progress counter regressed".to_owned())?;
        let source_verification_cancelled_runs = verification_current
            .cancelled_runs
            .checked_sub(verification_origin.cancelled_runs)
            .ok_or_else(|| "source-verification cancellation counter regressed".to_owned())?;
        let source_verification_failed_runs = verification_current
            .failed_runs
            .checked_sub(verification_origin.failed_runs)
            .ok_or_else(|| "source-verification failed-run counter regressed".to_owned())?;
        let source_verification_successes = verification_current
            .accepted_successes
            .checked_sub(verification_origin.accepted_successes)
            .ok_or_else(|| "source-verification success counter regressed".to_owned())?;

        Ok(ImportVerificationEvidenceSnapshot {
            source_verification_started_runs,
            source_verification_progress_updates,
            source_verification_cancelled_runs,
            source_verification_failed_runs,
            source_verification_successes,
        })
    }

    fn capture_bound_import_publication_evidence(
        &self,
        app: &MiranteWorkbenchApp,
        path: &Path,
    ) -> Result<ImportPublicationEvidenceSnapshot, String> {
        let timing_origin = self.active_import_timing_origin.as_ref().ok_or_else(|| {
            "import publication evidence has no exact worker timing origin".to_owned()
        })?;
        let diagnostics = app.import.workers.diagnostics();
        let successful = diagnostics.last_successful_import.as_ref().ok_or_else(|| {
            "import publication evidence has no retained successful import".to_owned()
        })?;
        if successful.review_id != timing_origin.review_id
            || successful.token != timing_origin.token
            || successful.source_fingerprint != timing_origin.source_fingerprint
            || successful.reviewed_source_bytes != timing_origin.reviewed_source_bytes
            || normalize_path(&successful.destination) != normalize_path(&timing_origin.destination)
            || normalize_path(path) != normalize_path(&successful.destination)
        {
            return Err(
                "publication package, worker timing origin, and successful receipt do not describe the same import"
                    .to_owned(),
            );
        }
        let publication_transfer = app
            .source_open_service
            .as_ref()
            .and_then(|service| service.completed_imported_publication_transfer())
            .ok_or_else(|| {
                "import publication has no storage execution evidence for its transfer".to_owned()
            })?;
        if normalize_path(publication_transfer.destination()) != normalize_path(path) {
            return Err(
                "storage publication-transfer evidence is bound to another destination".to_owned(),
            );
        }
        let verification = self.capture_import_verification_evidence(app)?;

        Ok(ImportPublicationEvidenceSnapshot {
            publication_currentness: publication_transfer.execution().into(),
            source_verification_started_runs: verification.source_verification_started_runs,
            source_verification_progress_updates: verification.source_verification_progress_updates,
            source_verification_cancelled_runs: verification.source_verification_cancelled_runs,
            source_verification_failed_runs: verification.source_verification_failed_runs,
            source_verification_successes: verification.source_verification_successes,
        })
    }

    fn complete_imported_open_ready_measurement(
        &mut self,
        app: &MiranteWorkbenchApp,
        path: &Path,
    ) -> Result<ImportPrimaryMeasurement, String> {
        self.complete_imported_open_ready_measurement_at(app, path, Instant::now())
    }

    fn complete_imported_open_ready_measurement_at(
        &mut self,
        app: &MiranteWorkbenchApp,
        path: &Path,
        open_ready_at: Instant,
    ) -> Result<ImportPrimaryMeasurement, String> {
        let timing_origin = self
            .active_import_timing_origin
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                "import became open-ready without an exact worker timing origin".to_owned()
            })?;
        let open_ready_at_epoch_ms = epoch_ms();
        let open_ready_process_cpu_time_ns = process_cpu_time_ns();
        let wall_time_ns = u64::try_from(
            open_ready_at
                .checked_duration_since(timing_origin.started_at)
                .ok_or_else(|| "primary-clock instant moved backwards".to_owned())?
                .as_nanos(),
        )
        .map_err(|_| "primary-clock wall time overflowed u64".to_owned())?;
        let process_cpu_time_ns = open_ready_process_cpu_time_ns
            .checked_sub(timing_origin.process_cpu_time_ns)
            .ok_or_else(|| "primary-clock process CPU time moved backwards".to_owned())?;
        let publication_evidence = self.capture_bound_import_publication_evidence(app, path)?;
        let diagnostics = app.import.workers.diagnostics();
        let published = diagnostics
            .last_successful_import
            .as_ref()
            .and_then(|successful| successful.published_timing.as_ref())
            .cloned()
            .ok_or_else(|| "successful import has no exact Published-event timing".to_owned())?;
        if published.published_at < timing_origin.started_at {
            return Err(
                "successful import Published-event timing precedes its worker origin".to_owned(),
            );
        }
        let publication_to_open_ready_wall_time_ns = u64::try_from(
            open_ready_at
                .checked_duration_since(published.published_at)
                .ok_or_else(|| "publication-to-open-ready instant moved backwards".to_owned())?
                .as_nanos(),
        )
        .map_err(|_| "publication-to-open-ready wall time overflowed u64".to_owned())?;
        let publication_to_open_ready_process_cpu_time_ns = open_ready_process_cpu_time_ns
            .checked_sub(published.process_cpu_time_ns)
            .ok_or_else(|| {
                "publication-to-open-ready process CPU time moved backwards".to_owned()
            })?;
        let measurement = ImportPrimaryMeasurement {
            started_at_epoch_ms: timing_origin.started_at_epoch_ms,
            open_ready_at_epoch_ms,
            wall_time_ns,
            process_cpu_time_ns,
        };
        let publication_measurement = ImportPublicationToOpenReadyMeasurement {
            published_at_epoch_ms: published.published_at_epoch_ms,
            open_ready_at_epoch_ms,
            wall_time_ns: publication_to_open_ready_wall_time_ns,
            process_cpu_time_ns: publication_to_open_ready_process_cpu_time_ns,
        };
        ImportedOpenReadyCommitState {
            active_origin: &mut self.active_import_timing_origin,
            active_verification_origin: &mut self.active_import_verification_diagnostics_origin,
            completed_primary: &mut self.completed_import_primary_measurement,
            open_ready_outcome: &mut self.imported_open_ready_outcome,
        }
        .commit(
            measurement,
            ImportedOpenReadyOutcome {
                measurement: publication_measurement,
                evidence: publication_evidence,
            },
        );
        Ok(measurement)
    }

    fn execute_command(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        ctx: &egui::Context,
        command: &ProductAutomationCommand,
    ) -> Result<CommandProgress, String> {
        match command {
            ProductAutomationCommand::OpenDataset { path } => {
                let expected = normalize_path(path);
                let actual = normalize_path(app.dataset.selected_path());
                if actual != expected {
                    return Err(format!(
                        "automation dataset mismatch: product opened {}, script expected {}",
                        app.dataset.selected_path().display(),
                        path.display()
                    ));
                }
                Ok(CommandProgress::Done(json!({
                    "mode": "opened_by_product_startup",
                    "path": app.dataset.selected_path().display().to_string(),
                })))
            }
            ProductAutomationCommand::SwitchDataset { path } => {
                self.execute_dataset_switch(app, ctx, path)
            }
            ProductAutomationCommand::NewProject => {
                dispatch_application_command(app, ctx, ApplicationCommand::AttachVerifiedDataset)?;
                if !app.application.snapshot().is_bound() {
                    return Err("new_project did not establish a bound workspace".to_owned());
                }
                Ok(CommandProgress::Done(project_state_json(app)))
            }
            ProductAutomationCommand::InitialSaveWithEdit { path } => {
                initial_save_with_durable_edit(app, ctx, path)
            }
            ProductAutomationCommand::OpenProject { path } => {
                if app.project_store_noninteractive_paths.open.is_some() {
                    return Err("a noninteractive project-open path is already pending".to_owned());
                }
                app.project_store_noninteractive_paths.open = Some(path.clone());
                if let Err(reason) =
                    dispatch_application_command(app, ctx, ApplicationCommand::RequestProjectOpen)
                {
                    app.project_store_noninteractive_paths.open = None;
                    return Err(reason);
                }
                if app.project_store_noninteractive_paths.open.is_some() {
                    return Err(
                        "open_project path was not consumed by the project event route".to_owned(),
                    );
                }
                Ok(CommandProgress::Done(json!({
                    "path": path.display().to_string(),
                    "normal_reducer_service_path": true,
                })))
            }
            ProductAutomationCommand::RecoverAutomaticAutosave => {
                let (generation_id, token) = app
                    .project_recovery_review
                    .as_ref()
                    .map(|review| (review.automatic_newer, review.token.clone()))
                    .ok_or_else(|| {
                        "no automatic autosave recovery is awaiting review".to_owned()
                    })?;
                app.project_store
                    .as_mut()
                    .ok_or_else(|| "project-store service is unavailable".to_owned())?
                    .submit_open_recovery(token, generation_id)
                    .map_err(|error| {
                        format!("automatic autosave recovery was rejected: {error:?}")
                    })?;
                Ok(CommandProgress::Done(json!({
                    "generation_id": generation_id.to_string(),
                    "foreground_active": true,
                })))
            }
            ProductAutomationCommand::SaveProjectAs { path } => {
                if app.project_store_noninteractive_paths.save_as.is_some() {
                    return Err("a noninteractive Save As path is already pending".to_owned());
                }
                let new_project_id = mirante4d_project_model::ProjectId::from_bytes(
                    *uuid::Uuid::new_v4().as_bytes(),
                );
                app.project_store_noninteractive_paths.save_as = Some(path.clone());
                if let Err(reason) = dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::RequestProjectSaveAs { new_project_id },
                ) {
                    app.project_store_noninteractive_paths.save_as = None;
                    return Err(reason);
                }
                if app.project_store_noninteractive_paths.save_as.is_some() {
                    return Err(
                        "save_project_as path was not consumed by the project event route"
                            .to_owned(),
                    );
                }
                Ok(CommandProgress::Done(json!({
                    "path": path.display().to_string(),
                    "new_project_id": new_project_id.to_string(),
                    "normal_reducer_service_path": true,
                })))
            }
            ProductAutomationCommand::CloseProjectStore => {
                app.project_store_product_evidence.close_result = None;
                app.project_store_product_evidence.actor_join = None;
                let request_id = app
                    .project_store
                    .as_mut()
                    .ok_or_else(|| "project-store service is unavailable".to_owned())?
                    .close()
                    .map_err(|error| format!("project-store close was rejected: {error:?}"))?;
                Ok(CommandProgress::Done(json!({
                    "request_id": request_id.get(),
                    "normal_actor_close": true,
                })))
            }
            ProductAutomationCommand::WriteExternalKillCheckpoint { path, stage } => {
                let checkpoint =
                    external_kill_checkpoint_json(app, stage, self.requested_mapped_client_pixels);
                write_synced_json_no_replace(path, &checkpoint)?;
                Ok(CommandProgress::Done(json!({
                    "path": path.display().to_string(),
                    "stage": stage,
                    "synced": true,
                    "project_evidence": project_store_evidence_json(app),
                })))
            }
            ProductAutomationCommand::HoldForExternalKill => {
                Ok(CommandProgress::PassiveWaiting(None))
            }
            ProductAutomationCommand::CancelSourceVerification => {
                let service = app
                    .source_verification_service
                    .as_mut()
                    .ok_or_else(|| "source-verification service is unavailable".to_owned())?;
                service
                    .reset_diagnostics()
                    .map_err(|error| error.to_string())?;

                let snapshot = app.application.snapshot();
                match snapshot.source() {
                    SourceVerificationSnapshot::Verified(_) => {
                        app.application
                            .dispatch(ApplicationCommand::InvalidateSourceVerification {
                                source_generation: snapshot.source_generation(),
                            })
                            .map_err(|fault| {
                                format!("source-verification invalidation was rejected: {fault:?}")
                            })?;
                    }
                    SourceVerificationSnapshot::Required => {}
                    SourceVerificationSnapshot::Verifying { .. } => {
                        return Err(
                            "cancel_source_verification requires an idle source state".to_owned()
                        );
                    }
                }
                app.application
                    .dispatch(ApplicationCommand::RequestSourceVerification)
                    .map_err(|fault| {
                        format!("source-verification request was rejected: {fault:?}")
                    })?;
                let operation_id = match app.application.snapshot().source() {
                    SourceVerificationSnapshot::Verifying { operation_id, .. } => *operation_id,
                    _ => {
                        return Err(
                            "source-verification request did not create an operation".to_owned()
                        );
                    }
                };
                app.application
                    .dispatch(ApplicationCommand::CancelOperation(operation_id))
                    .map_err(|fault| {
                        format!("source-verification cancellation was rejected: {fault:?}")
                    })?;
                app.pump_application_services();
                Ok(CommandProgress::Done(json!({
                    "operation_id": operation_id.get(),
                    "cancellation_requested_before_worker_poll": true,
                })))
            }
            ProductAutomationCommand::CancelActiveSourceVerification => {
                cancel_active_source_verification(app).map(CommandProgress::Done)
            }
            ProductAutomationCommand::RequestSourceVerification => {
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::RequestSourceVerification,
                )?;
                Ok(CommandProgress::Done(json!({
                    "requested": true,
                })))
            }
            ProductAutomationCommand::BeginTiffImportSetup {
                source,
                output_parent,
            } => {
                if app.import.workers.status().is_active() {
                    return Err("an import or TIFF inspection is already active".to_owned());
                }
                if !source.exists() {
                    return Err(format!(
                        "TIFF import source does not exist: {}",
                        source.display()
                    ));
                }
                if !output_parent.is_dir() {
                    return Err(format!(
                        "TIFF import output parent is not a directory: {}",
                        output_parent.display()
                    ));
                }
                let tiff_source = TiffSource::auto(source);
                let destination =
                    crate::import_workflow::tiff_destination(&tiff_source, output_parent);
                self.active_import_pre_start_origin = Some(ImportPreStartOrigin {
                    started_at_epoch_ms: epoch_ms(),
                    process_cpu_time_ns: process_cpu_time_ns(),
                    started_at: Instant::now(),
                    destination: destination.clone(),
                });
                self.completed_import_pre_start_measurement = None;
                app.start_tiff_import_setup_task(tiff_source, output_parent.clone(), ctx);
                if let Some(problem) = app.import.problem.as_ref() {
                    return Err(format!("TIFF inspection could not start: {problem}"));
                }
                Ok(CommandProgress::Done(json!({
                    "source": source.display().to_string(),
                    "destination": destination.display().to_string(),
                    "normal_setup_and_inspection_path": true,
                })))
            }
            ProductAutomationCommand::StartReviewedImport {
                spacing_zyx_um,
                time_step_seconds,
                no_data_sentinel,
                working_memory_bytes,
            } => {
                let ImportWorkflowSnapshot::Review(review) = app.import.snapshot() else {
                    return Err("no completed TIFF review is ready to start".to_owned());
                };
                let draft = ImportReviewDraft {
                    spacing_zyx_um: *spacing_zyx_um,
                    calibration_confirmed: true,
                    time_step_seconds: *time_step_seconds,
                    no_data_sentinel: *no_data_sentinel,
                    working_memory_bytes: *working_memory_bytes,
                };
                let pre_start_origin =
                    self.active_import_pre_start_origin
                        .as_ref()
                        .ok_or_else(|| {
                            "reviewed TIFF import has no exact pre-start timing origin".to_owned()
                        })?;
                if normalize_path(&pre_start_origin.destination)
                    != normalize_path(Path::new(&review.destination))
                {
                    return Err(
                        "reviewed TIFF import differs from its pre-start timing origin".to_owned(),
                    );
                }
                let start_command_at_epoch_ms = epoch_ms();
                let pre_start_wall_time_ns =
                    u64::try_from(pre_start_origin.started_at.elapsed().as_nanos())
                        .map_err(|_| "pre-start import wall time overflowed u64".to_owned())?;
                let pre_start_process_cpu_time_ns = process_cpu_time_ns()
                    .checked_sub(pre_start_origin.process_cpu_time_ns)
                    .ok_or_else(|| {
                        "pre-start import process CPU time moved backwards".to_owned()
                    })?;
                let pre_start_measurement = ImportPreStartMeasurement {
                    started_at_epoch_ms: pre_start_origin.started_at_epoch_ms,
                    start_command_at_epoch_ms,
                    wall_time_ns: pre_start_wall_time_ns,
                    process_cpu_time_ns: pre_start_process_cpu_time_ns,
                };
                let verification_diagnostics_origin = app
                    .source_verification_service
                    .as_ref()
                    .ok_or_else(|| {
                        "reviewed TIFF import has no source-verification service".to_owned()
                    })?
                    .diagnostics();
                app.apply_import_command(
                    ImportCommand::Start {
                        review_id: review.review_id,
                        draft,
                    },
                    ctx,
                );
                if let Some(problem) = app.import.problem.as_ref() {
                    return Err(format!("reviewed TIFF import could not start: {problem}"));
                }
                if !app.import.workers.status().is_importing() {
                    return Err("reviewed TIFF import did not create an active worker".to_owned());
                }
                let timing_origin = app
                    .import
                    .workers
                    .active_import_timing_origin()
                    .ok_or_else(|| {
                        "reviewed TIFF import has no exact worker timing origin".to_owned()
                    })?;
                self.active_import_timing_origin = Some(timing_origin.clone());
                self.active_import_verification_diagnostics_origin =
                    Some(verification_diagnostics_origin);
                self.completed_import_primary_measurement = None;
                self.imported_open_ready_outcome = None;
                self.completed_import_pre_start_measurement = Some(pre_start_measurement);
                self.active_import_pre_start_origin = None;
                Ok(CommandProgress::Done(json!({
                    "review_id": review.review_id.get(),
                    "destination": review.destination,
                    "operation_token": operation_token_json(&timing_origin.token),
                    "reviewed_source_fingerprint_sha256": timing_origin.source_fingerprint.to_string(),
                    "reviewed_source_bytes": timing_origin.reviewed_source_bytes,
                    "working_memory_bytes": working_memory_bytes,
                    "primary_clock_started_at_epoch_ms": timing_origin.started_at_epoch_ms,
                    "primary_clock_start_boundary": "accepted_start_import_command_immediately_before_worker_spawn",
                    "normal_review_command_path": true,
                })))
            }
            ProductAutomationCommand::WaitForImportProgress {
                stage,
                minimum_completed_work_units,
                timeout_ms,
            } => {
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                let diagnostics = app.import.workers.diagnostics();
                let completed = diagnostics.maximum_completed_for_name(stage);
                if completed >= *minimum_completed_work_units {
                    Ok(CommandProgress::Done(json!({
                        "stage": stage,
                        "minimum_completed_work_units": minimum_completed_work_units,
                        "observed_completed_work_units": completed,
                        "waited_ms": duration_ms(started.elapsed()),
                    })))
                } else if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    Err(format!(
                        "timed out after {timeout_ms} ms waiting for import stage {stage:?} to reach {minimum_completed_work_units} work units; observed {completed}"
                    ))
                } else if !app.import.workers.status().is_importing() {
                    Err(format!(
                        "import stopped before stage {stage:?} reached {minimum_completed_work_units} work units; observed {completed}"
                    ))
                } else {
                    Ok(CommandProgress::Waiting)
                }
            }
            ProductAutomationCommand::CancelImport => {
                if !app.import.workers.status().is_importing() {
                    return Err("cancel_import requires an active import".to_owned());
                }
                app.apply_import_command(ImportCommand::CancelImport, ctx);
                Ok(CommandProgress::Done(json!({
                    "cancellation_requested": true,
                    "normal_import_command_path": true,
                })))
            }
            ProductAutomationCommand::WaitForImportedOpenReady { path, timeout_ms } => {
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                let snapshot = app.application.snapshot();
                let readiness = imported_open_ready_readiness(app, &snapshot, path);
                if readiness.condition_met() {
                    let measurement = self.complete_imported_open_ready_measurement(app, path)?;
                    Ok(CommandProgress::Done(json!({
                        "path": path.display().to_string(),
                        "verified": true,
                        "import_idle": true,
                        "normal_product_open_path": true,
                        "primary_clock": import_primary_measurement_json(Some(measurement)),
                        "waited_ms": duration_ms(started.elapsed()),
                    })))
                } else if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    Err(format!(
                        "timed out after {timeout_ms} ms waiting for imported package {} to become verified and open-ready (selected_matches={}, verified={}, import_idle={}, problem={:?})",
                        path.display(),
                        readiness.selected_matches,
                        readiness.verified,
                        readiness.import_idle,
                        app.import.problem,
                    ))
                } else {
                    Ok(CommandProgress::Waiting)
                }
            }
            ProductAutomationCommand::WaitFor {
                condition,
                timeout_ms,
            } => {
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                if self.wait_condition_met(app, *condition) {
                    Ok(CommandProgress::Done(json!({
                        "condition": condition.name(),
                        "waited_ms": duration_ms(started.elapsed()),
                    })))
                } else if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    Err(format!(
                        "timed out after {timeout_ms} ms waiting for {}",
                        condition.name()
                    ))
                } else {
                    Ok(if condition.is_passive() {
                        CommandProgress::PassiveWaiting(Some(
                            Duration::from_millis(*timeout_ms).saturating_sub(started.elapsed()),
                        ))
                    } else {
                        CommandProgress::Waiting
                    })
                }
            }
            ProductAutomationCommand::SetViewportSize { width, height } => {
                if *width == 0 || *height == 0 {
                    return Err("requested window inner size in points must be nonzero".to_owned());
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    *width as f32,
                    *height as f32,
                )));
                Ok(CommandProgress::Done(json!({
                    "requested_window_inner_size_points": {
                        "width": width,
                        "height": height,
                    },
                })))
            }
            ProductAutomationCommand::SetMappedClientPixels { width, height } => {
                if *width == 0 || *height == 0 {
                    return Err("requested mapped client pixels must be nonzero".to_owned());
                }
                let pixels_per_point = ctx
                    .input(|input| input.viewport().native_pixels_per_point)
                    .unwrap_or_else(|| ctx.pixels_per_point());
                if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
                    return Err("native pixels-per-point is unavailable".to_owned());
                }
                let fullscreen = *width == 1920 && *height == 1080;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    *width as f32 / pixels_per_point,
                    *height as f32 / pixels_per_point,
                )));
                self.requested_mapped_client_pixels = Some((*width, *height));
                Ok(CommandProgress::Done(json!({
                    "requested_mapped_client_pixels": {
                        "width": width,
                        "height": height,
                    },
                    "pixels_per_point": pixels_per_point,
                    "fullscreen_requested": fullscreen,
                    "external_geometry_observation_required": true,
                })))
            }
            ProductAutomationCommand::SetRenderTargetSize { width, height } => {
                let viewport = RenderExtent::new(*width, *height)
                    .map_err(|error| format!("invalid automation render target: {error}"))?;
                let context_max = ctx.input(|input| input.max_texture_side);
                #[cfg(test)]
                let maximum = app
                    .test_render_viewport_max_side
                    .map_or(context_max, |test_max| context_max.min(test_max));
                #[cfg(not(test))]
                let maximum = context_max;
                if usize::try_from(viewport.width_pixels())
                    .ok()
                    .is_none_or(|width| width > maximum)
                    || usize::try_from(viewport.height_pixels())
                        .ok()
                        .is_none_or(|height| height > maximum)
                {
                    return Err(format!(
                        "automation render target {}x{} exceeds maximum texture side {maximum}",
                        viewport.width_pixels(),
                        viewport.height_pixels()
                    ));
                }
                self.render_target_override = Some(viewport);
                if set_render_viewport(&mut app.render_coordination, viewport) {
                    app.render_coordination.request_refresh();
                    ctx.request_repaint();
                }
                Ok(CommandProgress::Done(json!({
                    "requested_render_target_pixels": {
                        "width": viewport.width_pixels(),
                        "height": viewport.height_pixels(),
                    },
                    "evidence_scope": "automation_only_internal_gpu_render_target",
                })))
            }
            ProductAutomationCommand::SetViewerLayout { layout } => {
                let viewer_layout: ViewerLayout = (*layout).into();
                let snapshot = app.application.snapshot();
                let cross_section = *application_view(&snapshot).cross_section();
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetLayout {
                        layout: viewer_layout,
                        cross_section,
                    },
                )?;
                Ok(CommandProgress::Done(json!({
                    "layout": layout.name(),
                })))
            }
            ProductAutomationCommand::SetTimeIndex { time_index } => {
                let snapshot = app.application.snapshot();
                let view = application_view(&snapshot);
                let timepoint_count = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog")
                    .shape()
                    .t();
                if *time_index >= timepoint_count {
                    return Err(format!(
                        "time index {time_index} is out of bounds for {timepoint_count} timepoints"
                    ));
                }
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetTimepoint(TimeIndex::new(*time_index)),
                )?;
                Ok(CommandProgress::Done(json!({
                    "time_index": time_index,
                    "timepoint_count": timepoint_count,
                    "semantic_application_command": true,
                })))
            }
            ProductAutomationCommand::SetLayerVisibility {
                layer_index,
                visible,
            } => {
                let command = layer_command(app, *layer_index, |layer| {
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        *visible,
                        layer.transfer().clone(),
                        *layer.render_state(),
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "visible": visible,
                    "semantic_application_command": true,
                })))
            }
            ProductAutomationCommand::SetLayerOrder { layer_indices } => {
                let snapshot = app.application.snapshot();
                let view = application_view(&snapshot);
                if layer_indices.len() != view.layers().len() {
                    return Err(format!(
                        "layer order contains {} indices, expected {}",
                        layer_indices.len(),
                        view.layers().len()
                    ));
                }
                let mut seen = std::collections::BTreeSet::new();
                let mut order = Vec::with_capacity(layer_indices.len());
                for index in layer_indices.iter().copied() {
                    let layer = view
                        .layers()
                        .get(index)
                        .ok_or_else(|| format!("layer order index {index} is out of bounds"))?;
                    if !seen.insert(index) {
                        return Err(format!("layer order index {index} is duplicated"));
                    }
                    order.push(layer.layer_key());
                }
                dispatch_application_command(app, ctx, ApplicationCommand::SetLayerOrder(order))?;
                Ok(CommandProgress::Done(json!({
                    "layer_indices": layer_indices,
                    "semantic_application_command": true,
                })))
            }
            ProductAutomationCommand::SetRenderMode { mode } => {
                let render_mode: RenderMode = (*mode).into();
                let layer_index = active_layer_index(app);
                let command = layer_command(app, layer_index, |layer| {
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state_for_mode(
                            *layer.render_state(),
                            layer.transfer(),
                            render_mode,
                        )?,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "render_mode": mode.name(),
                })))
            }
            ProductAutomationCommand::SetLayerRenderMode { layer_index, mode } => {
                let render_mode: RenderMode = (*mode).into();
                let command = layer_command(app, *layer_index, |layer| {
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state_for_mode(
                            *layer.render_state(),
                            layer.transfer(),
                            render_mode,
                        )?,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "render_mode": mode.name(),
                })))
            }
            ProductAutomationCommand::SetProjection { projection } => {
                let snapshot = app.application.snapshot();
                let camera = camera_with_projection(
                    *application_view(&snapshot).camera(),
                    (*projection).into(),
                )?;
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(json!({
                    "projection": projection.name(),
                })))
            }
            ProductAutomationCommand::SetLayerSampling {
                layer_index,
                sampling,
            } => {
                let command = layer_command(app, *layer_index, |layer| {
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state_with_sampling(*layer.render_state(), (*sampling).into())?,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "sampling": sampling.name(),
                })))
            }
            ProductAutomationCommand::SetLayerIsoShading {
                layer_index,
                shading,
            } => {
                let command = layer_command(app, *layer_index, |layer| {
                    let current = *layer.render_state();
                    let parameters = current
                        .iso_parameters()
                        .ok_or_else(|| "ISO shading requires ISO render mode".to_owned())?;
                    let render_state = RenderState::iso(
                        current.sampling_policy(),
                        (*shading).into(),
                        parameters.display_level(),
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "iso_shading": shading.name(),
                })))
            }
            ProductAutomationCommand::SetIsoLight { light } => {
                let light_state = light.into_domain()?;
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetIsoLight(light_state),
                )?;
                Ok(CommandProgress::Done(json!({
                    "iso_light": light.name(),
                    "detached_screen_position": light_state.detached_screen_position(),
                })))
            }
            ProductAutomationCommand::SetIsoDisplayLevel { display_level } => {
                if !display_level.is_finite() || !(0.0..=1.0).contains(display_level) {
                    return Err(
                        "ISO display level must be finite and between 0.0 and 1.0".to_owned()
                    );
                }
                let command = layer_command(app, active_layer_index(app), |layer| {
                    let current = *layer.render_state();
                    let shading = current
                        .iso_parameters()
                        .map(|parameters| parameters.shading_policy())
                        .unwrap_or(IsoShadingPolicy::GradientLighting);
                    let render_state =
                        RenderState::iso(current.sampling_policy(), shading, *display_level)
                            .map_err(|error| error.to_string())?;
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "display_level": display_level,
                })))
            }
            ProductAutomationCommand::SetDvrDensityScale { density_scale } => {
                if !density_scale.is_finite()
                    || !(DVR_DENSITY_SCALE_MIN..=DVR_DENSITY_SCALE_MAX).contains(density_scale)
                {
                    return Err(format!(
                        "DVR density scale must be finite and between {DVR_DENSITY_SCALE_MIN:.1} and {DVR_DENSITY_SCALE_MAX:.1}"
                    ));
                }
                let command = layer_command(app, active_layer_index(app), |layer| {
                    let current = *layer.render_state();
                    let opacity_transfer = current
                        .dvr_parameters()
                        .map(|parameters| parameters.opacity_transfer())
                        .unwrap_or_else(|| {
                            DvrOpacityTransfer::new(
                                layer.transfer().window(),
                                layer.transfer().curve(),
                            )
                        });
                    let render_state = RenderState::dvr(
                        current.sampling_policy(),
                        opacity_transfer,
                        *density_scale,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        layer.transfer().clone(),
                        render_state,
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "density_scale": density_scale,
                })))
            }
            ProductAutomationCommand::SetLayerOpacity {
                layer_index,
                opacity,
            } => {
                if !opacity.is_finite() || !(0.0..=1.0).contains(opacity) {
                    return Err("layer opacity must be finite and between 0.0 and 1.0".to_owned());
                }
                let command = layer_command(app, *layer_index, |layer| {
                    let current = layer.transfer();
                    let transfer = LayerTransfer::new(
                        current.window(),
                        current.color(),
                        Opacity::new(*opacity).map_err(|error| error.to_string())?,
                        current.curve(),
                        current.invert(),
                    );
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        transfer,
                        *layer.render_state(),
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "opacity": opacity,
                })))
            }
            ProductAutomationCommand::SetLayerWindow {
                layer_index,
                low,
                high,
            } => {
                if !low.is_finite() || !high.is_finite() || low >= high {
                    return Err(
                        "layer window bounds must be finite with low less than high".to_owned()
                    );
                }
                let command = layer_command(app, *layer_index, |layer| {
                    let current = layer.transfer();
                    let transfer = LayerTransfer::new(
                        DisplayWindow::new(*low, *high).map_err(|error| error.to_string())?,
                        current.color(),
                        current.opacity(),
                        current.curve(),
                        current.invert(),
                    );
                    Ok(LayerViewState::new(
                        layer.layer_key(),
                        layer.visible(),
                        transfer,
                        *layer.render_state(),
                    ))
                })?;
                dispatch_application_command(app, ctx, command)?;
                Ok(CommandProgress::Done(json!({
                    "layer_index": layer_index,
                    "low": low,
                    "high": high,
                })))
            }
            ProductAutomationCommand::CameraFitData => {
                let snapshot = app.application.snapshot();
                let view = application_view(&snapshot);
                let layer = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog");
                let camera = fit_camera_to_shape_preserving_view(
                    *view.camera(),
                    layer.shape().spatial(),
                    layer.grid_to_world(),
                    app.render_coordination.presentation_viewport,
                );
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(json!({})))
            }
            ProductAutomationCommand::CameraOrbit {
                yaw_points,
                pitch_points,
            } => {
                let viewport_side = 800.0;
                let start = [viewport_side * 0.5, viewport_side * 0.5];
                let current = [start[0] + *yaw_points, start[1] + *pitch_points];
                let snapshot = app.application.snapshot();
                let start_camera = *application_view(&snapshot).camera();
                let camera =
                    orbit_camera(start_camera, start, current, [viewport_side, viewport_side]);
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(json!({
                    "yaw_points": yaw_points,
                    "pitch_points": pitch_points,
                })))
            }
            ProductAutomationCommand::CameraPan { x_points, y_points } => {
                let snapshot = app.application.snapshot();
                let camera = pan_camera(
                    *application_view(&snapshot).camera(),
                    [*x_points, *y_points],
                );
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(json!({
                    "x_points": x_points,
                    "y_points": y_points,
                })))
            }
            ProductAutomationCommand::CameraZoom { scroll_y_points } => {
                let snapshot = app.application.snapshot();
                let camera = zoom_camera(*application_view(&snapshot).camera(), *scroll_y_points);
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(json!({
                    "scroll_y_points": scroll_y_points,
                })))
            }
            ProductAutomationCommand::CameraOrbitSequence {
                samples,
                duration_ms,
                yaw_points_per_sample,
                pitch_points_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let viewport_side = 800.0;
                    let start = [viewport_side * 0.5, viewport_side * 0.5];
                    let current = [
                        start[0] + *yaw_points_per_sample,
                        start[1] + *pitch_points_per_sample,
                    ];
                    let snapshot = app.application.snapshot();
                    let camera = orbit_camera(
                        *application_view(&snapshot).camera(),
                        start,
                        current,
                        [viewport_side, viewport_side],
                    );
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetCamera(camera),
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "camera_orbit",
                            "last_sample_index": sample_index,
                            "yaw_points_per_sample": yaw_points_per_sample,
                            "pitch_points_per_sample": pitch_points_per_sample,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CameraPanSequence {
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let camera = pan_camera(
                        *application_view(&snapshot).camera(),
                        [*x_points_per_sample, *y_points_per_sample],
                    );
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetCamera(camera),
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "camera_pan",
                            "last_sample_index": sample_index,
                            "x_points_per_sample": x_points_per_sample,
                            "y_points_per_sample": y_points_per_sample,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CameraZoomSequence {
                samples,
                duration_ms,
                scroll_y_points_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let camera = zoom_camera(
                        *application_view(&snapshot).camera(),
                        *scroll_y_points_per_sample,
                    );
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetCamera(camera),
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "camera_zoom",
                            "last_sample_index": sample_index,
                            "scroll_y_points_per_sample": scroll_y_points_per_sample,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::SetActiveCrossSectionPanel { panel } => {
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetActiveCrossSectionPanel(Some(
                        application_cross_section_panel(*panel),
                    )),
                )?;
                Ok(CommandProgress::Done(json!({
                    "panel": PanelId::from(*panel).label(),
                })))
            }
            ProductAutomationCommand::SetCrossSectionView {
                center_world,
                orientation_xyzw,
                scale_world_per_screen_point,
                depth_world,
            } => {
                let snapshot = app.application.snapshot();
                let view = application_view(&snapshot);
                if view.layout() != ViewerLayout::FourPanel {
                    return Err(
                        "setting the cross-section view requires four-panel layout".to_owned()
                    );
                }
                let cross_section = exact_cross_section_view(
                    *center_world,
                    *orientation_xyzw,
                    *scale_world_per_screen_point,
                    *depth_world,
                )?;
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetLayout {
                        layout: ViewerLayout::FourPanel,
                        cross_section,
                    },
                )?;
                Ok(CommandProgress::Done(json!({
                    "center_world": cross_section.center_world().components(),
                    "orientation_xyzw": cross_section.orientation().xyzw(),
                    "scale_world_per_screen_point": cross_section.scale_world_per_screen_point(),
                    "depth_world": cross_section.depth_world(),
                })))
            }
            ProductAutomationCommand::CrossSectionRotateSequence {
                panel,
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
                radians_per_point,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(*view.cross_section());
                    state.rotate_oblique_by_panel_drag(
                        interaction_cross_section_panel(*panel),
                        *x_points_per_sample,
                        *y_points_per_sample,
                        *radians_per_point,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section rotation was rejected: {error}"))?;
                    dispatch_application_command(
                        app,
                        ctx,
                        ApplicationCommand::SetActiveCrossSectionPanel(Some(
                            application_cross_section_panel(*panel),
                        )),
                    )?;
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetLayout {
                            layout: ViewerLayout::FourPanel,
                            cross_section,
                        },
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "cross_section_rotate",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": sample_index,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CrossSectionPanSequence {
                panel,
                samples,
                duration_ms,
                x_points_per_sample,
                y_points_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(*view.cross_section());
                    state.pan_by_panel_points(
                        interaction_cross_section_panel(*panel),
                        *x_points_per_sample,
                        *y_points_per_sample,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section pan was rejected: {error}"))?;
                    dispatch_application_command(
                        app,
                        ctx,
                        ApplicationCommand::SetActiveCrossSectionPanel(Some(
                            application_cross_section_panel(*panel),
                        )),
                    )?;
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetLayout {
                            layout: ViewerLayout::FourPanel,
                            cross_section,
                        },
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "cross_section_pan",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": sample_index,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CrossSectionZoomSequence {
                panel,
                samples,
                duration_ms,
                x_fraction,
                y_fraction,
                factor_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let panel_id = PanelId::from(*panel);
                    let viewport = app
                        .render_coordination
                        .surface(panel_id.presentation_slot())
                        .presentation_viewport()
                        .ok_or_else(|| {
                            format!("{} presentation viewport is unavailable", panel_id.label())
                        })?;
                    let mut state = CrossSectionViewState::from_canonical(*view.cross_section());
                    state.zoom_around_panel_point(
                        interaction_cross_section_panel(*panel),
                        viewport,
                        *x_fraction * viewport.width_points(),
                        *y_fraction * viewport.height_points(),
                        *factor_per_sample,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section zoom was rejected: {error}"))?;
                    dispatch_application_command(
                        app,
                        ctx,
                        ApplicationCommand::SetActiveCrossSectionPanel(Some(
                            application_cross_section_panel(*panel),
                        )),
                    )?;
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetLayout {
                            layout: ViewerLayout::FourPanel,
                            cross_section,
                        },
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "cross_section_zoom",
                            "panel": panel_id.label(),
                            "last_sample_index": sample_index,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CrossSectionSliceSequence {
                panel,
                samples,
                duration_ms,
                distance_world_per_sample,
            } => match self.input_sequence_step(app, *samples, *duration_ms) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(sample_index) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(*view.cross_section());
                    state.slice_by_world_distance(
                        interaction_cross_section_panel(*panel),
                        *distance_world_per_sample,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section slice was rejected: {error}"))?;
                    dispatch_application_command(
                        app,
                        ctx,
                        ApplicationCommand::SetActiveCrossSectionPanel(Some(
                            application_cross_section_panel(*panel),
                        )),
                    )?;
                    dispatch_effective_interaction_sample(
                        app,
                        ctx,
                        ApplicationCommand::SetLayout {
                            layout: ViewerLayout::FourPanel,
                            cross_section,
                        },
                    )?;
                    Ok(self.complete_input_sequence_sample(
                        app,
                        json!({
                            "kind": "cross_section_slice",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": sample_index,
                        }),
                    ))
                }
            },
            ProductAutomationCommand::SetActiveTool { tool } => {
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetActiveTool((*tool).into()),
                )?;
                Ok(CommandProgress::Done(json!({
                    "active_tool": tool.name(),
                })))
            }
            ProductAutomationCommand::ProbeHover {
                x_fraction,
                y_fraction,
            } => {
                self.await_automation_pick(app, *x_fraction, *y_fraction, ViewerPickPurpose::Hover)
            }
            ProductAutomationCommand::PrimaryClick {
                x_fraction,
                y_fraction,
            } => self.await_automation_pick(
                app,
                *x_fraction,
                *y_fraction,
                ViewerPickPurpose::PrimaryClick,
            ),
            ProductAutomationCommand::CopyDiagnostics => {
                let diagnostics = self.diagnostics_json(app);
                self.diagnostics.push(diagnostics.clone());
                Ok(CommandProgress::Done(diagnostics))
            }
            ProductAutomationCommand::CaptureScreenshot { name } => {
                if !product_presentations_ready(app, &[PanelId::ThreeD])? {
                    let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                    if started.elapsed() >= AUTOMATION_CAPTURE_TIMEOUT {
                        return Err(format!(
                            "timed out waiting for current GPU validation capture: {}",
                            product_capture_state(app, &[PanelId::ThreeD])
                        ));
                    }
                    return Ok(CommandProgress::Waiting);
                }
                let artifact = self.capture_viewport_artifact(app, name.as_deref())?;
                if artifact.pixel_stats.is_blank() {
                    return Err(format!(
                        "viewport capture {} is blank: nonzero_rgb_pixels={}, max_rgb={}",
                        artifact.path.display(),
                        artifact.pixel_stats.nonzero_rgb_pixels,
                        artifact.pixel_stats.max_rgb
                    ));
                }
                self.artifacts.push(artifact.clone());
                Ok(CommandProgress::Done(artifact.json()))
            }
            ProductAutomationCommand::Assert { condition } => {
                let capture_panels = assertion_capture_panels(condition);
                if !capture_panels.is_empty() && !product_presentations_ready(app, &capture_panels)?
                {
                    let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                    if started.elapsed() >= AUTOMATION_CAPTURE_TIMEOUT {
                        return Err(format!(
                            "timed out waiting for current GPU validation capture: {}",
                            product_capture_state(app, &capture_panels)
                        ));
                    }
                    return Ok(CommandProgress::Waiting);
                }
                self.assert_condition(app, condition)?;
                Ok(CommandProgress::Done(json!({
                    "condition": condition.name(),
                    "cross_section_snapshot": condition
                        .is_cross_section_condition()
                        .then(|| cross_section_diagnostics_json(app)),
                })))
            }
            ProductAutomationCommand::SleepFrames { frames } => {
                let remaining = self.sleep_frames_remaining.get_or_insert(*frames);
                if *remaining == 0 {
                    return Ok(CommandProgress::Done(json!({ "frames": frames })));
                }
                *remaining -= 1;
                Ok(CommandProgress::Waiting)
            }
            ProductAutomationCommand::Quit => {
                self.command_index = self.script.commands.len();
                Ok(CommandProgress::Done(json!({})))
            }
        }
    }

    fn capture_viewport_artifact(
        &self,
        app: &mut MiranteWorkbenchApp,
        requested_name: Option<&str>,
    ) -> Result<ProductAutomationArtifact, String> {
        let artifact_dir = self.artifact_dir();
        fs::create_dir_all(&artifact_dir).map_err(|err| {
            format!(
                "failed to create automation artifact directory {}: {err}",
                artifact_dir.display()
            )
        })?;
        let label = requested_name
            .map(sanitize_artifact_label)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("viewport-{:03}", self.command_index));
        let path = artifact_dir.join(format!("{label}.ppm"));
        let (capture_source, image) = capture_color_image(app)?;
        let pixel_stats = ProductAutomationImageStats::from_color_image(&image);
        write_color_image_ppm(&path, &image)
            .map_err(|err| format!("failed to write {}: {err}", path.display()))?;
        Ok(ProductAutomationArtifact {
            kind: "viewport_capture",
            format: "ppm",
            path,
            width: image.size[0],
            height: image.size[1],
            command_index: self.command_index,
            capture_source,
            pixel_stats,
        })
    }

    fn artifact_dir(&self) -> PathBuf {
        self.report_path
            .parent()
            .map(|parent| parent.join("artifacts"))
            .unwrap_or_else(|| PathBuf::from("target/mirante4d/product-automation-artifacts"))
    }

    fn await_automation_pick(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        x_fraction: f32,
        y_fraction: f32,
        purpose: ViewerPickPurpose,
    ) -> Result<CommandProgress, String> {
        // Consume the accepted request identity before rebuilding a request
        // from current presentation state. A primary-click effect can
        // synchronously publish a new frame; that expected mutation must not
        // make automation forget the just-completed transaction and resubmit
        // the click forever.
        if let Some((request, hit)) = app
            .viewer_pick_queue
            .take_automation_completion_for(purpose)
        {
            return Ok(CommandProgress::Done(automation_pick_json(
                request, &hit, x_fraction, y_fraction,
            )));
        }
        let started = *self.active_wait_started.get_or_insert_with(Instant::now);
        if started.elapsed() > AUTOMATION_PICK_TIMEOUT {
            return Err(format!(
                "{} did not complete through the native GPU pick queue within {} ms",
                automation_pick_purpose_name(purpose),
                AUTOMATION_PICK_TIMEOUT.as_millis()
            ));
        }
        let Some(request) = automation_pick_request(app, x_fraction, y_fraction, purpose)? else {
            return Ok(CommandProgress::Waiting);
        };
        app.viewer_pick_queue.enqueue_automation(request);
        Ok(CommandProgress::Waiting)
    }

    fn wait_condition_met(
        &self,
        app: &MiranteWorkbenchApp,
        condition: ProductAutomationWaitCondition,
    ) -> bool {
        let snapshot = app.application.snapshot();
        self.wait_condition_met_with_snapshot(app, &snapshot, condition)
    }

    fn wait_condition_met_with_snapshot(
        &self,
        app: &MiranteWorkbenchApp,
        snapshot: &ApplicationSnapshot,
        condition: ProductAutomationWaitCondition,
    ) -> bool {
        match condition {
            ProductAutomationWaitCondition::WindowReady => true,
            ProductAutomationWaitCondition::FirstFrame => {
                app.render_coordination
                    .frame_fidelity
                    .displayed_scale_level
                    .is_some()
                    || product_presentation(app, PanelId::ThreeD).is_some()
            }
            ProductAutomationWaitCondition::RuntimeIdle => {
                let progressive_render_work =
                    crate::workbench_playback_runtime::progressive_render_submission_work(
                        &app.dataset,
                        &app.native_presentation,
                    );
                automation_runtime_is_idle(
                    crate::workbench_playback_runtime::background_work_active(
                        snapshot,
                        &app.import.workers,
                        &app.dataset,
                        &app.render_coordination,
                        &app.native_presentation,
                        progressive_render_work.any_required,
                    ),
                    app.camera_demand_planner.has_outstanding_request(),
                    app.pending_visible_demand_plan.is_some(),
                )
            }
            ProductAutomationWaitCondition::FrameFreshnessCurrent => {
                frame_freshness_is_current(&app.render_coordination.frame_fidelity)
            }
            ProductAutomationWaitCondition::CoordinatedPresentationSettled => {
                coordinated_visible_layout_current_complete_with_snapshot(app, snapshot)
            }
            ProductAutomationWaitCondition::SourceVerificationInactive => {
                source_verification_inactive(app)
            }
            ProductAutomationWaitCondition::SourceVerificationRequired => {
                matches!(snapshot.source(), SourceVerificationSnapshot::Required)
                    && app
                        .source_verification_service
                        .as_ref()
                        .is_some_and(|service| service.active_token().is_none())
            }
            ProductAutomationWaitCondition::SourceVerificationVerified => {
                matches!(snapshot.source(), SourceVerificationSnapshot::Verified(_))
                    && app
                        .source_verification_service
                        .as_ref()
                        .is_some_and(|service| service.active_token().is_none())
            }
            ProductAutomationWaitCondition::ImportReviewReady => {
                matches!(app.import.snapshot(), ImportWorkflowSnapshot::Review(_))
            }
            ProductAutomationWaitCondition::ImportIdle => {
                !app.import.workers.status().is_active()
                    && !matches!(app.import.snapshot(), ImportWorkflowSnapshot::Failed(_))
            }
            ProductAutomationWaitCondition::ProjectStoreIdle => {
                app.project_store.as_ref().is_some_and(|service| {
                    let status = service.status();
                    !status.foreground_active()
                        && !status.autosave_active()
                        && !matches!(
                            status.lifecycle(),
                            ProjectStoreLifecycle::Closing | ProjectStoreLifecycle::Closed
                        )
                })
            }
            ProductAutomationWaitCondition::ProjectAutosaved => app
                .project_store_product_evidence
                .latest_autosave_captured_revision
                .is_some(),
            ProductAutomationWaitCondition::RecoveryReviewRequired => {
                app.project_recovery_review.is_some()
            }
            ProductAutomationWaitCondition::ProjectStoreClosed => {
                app.project_store.is_none()
                    && matches!(
                        app.project_store_product_evidence.close_result,
                        Some(crate::ProjectStoreRecordedResult::Succeeded)
                    )
                    && matches!(
                        app.project_store_product_evidence.actor_join,
                        Some(crate::ProjectStoreRecordedResult::Succeeded)
                    )
            }
        }
    }

    fn assert_condition(
        &self,
        app: &MiranteWorkbenchApp,
        condition: &ProductAutomationAssertCondition,
    ) -> Result<(), String> {
        let snapshot = app.application.snapshot();
        let view = application_view(&snapshot);
        match condition {
            ProductAutomationAssertCondition::NonblankFrame => {
                let (source, stats) = current_display_image_stats(app)?;
                if !stats.is_blank() {
                    Ok(())
                } else {
                    Err(format!(
                        "current product frame is blank from {source}: nonzero_rgb_pixels={}, max_rgb={}",
                        stats.nonzero_rgb_pixels, stats.max_rgb
                    ))
                }
            }
            ProductAutomationAssertCondition::NoRenderError => {
                if let Some(kind) = app.render_coordination.frame_fidelity.last_failure_kind {
                    Err(format!("render failure is set: {kind:?}"))
                } else if let Some(error) = app
                    .render_coordination
                    .frame_fidelity
                    .last_capacity_error
                    .as_ref()
                {
                    Err(format!("render capacity error is set: {error}"))
                } else {
                    Ok(())
                }
            }
            ProductAutomationAssertCondition::FrameFidelity {
                scale_level,
                complete,
            } => {
                let fidelity = &app.render_coordination.frame_fidelity;
                let scale_matches = fidelity.displayed_scale_level == Some(*scale_level)
                    && fidelity.target_scale_level == *scale_level;
                let completeness_matches = !*complete
                    || matches!(
                        fidelity.completeness,
                        FrameCompleteness::Exact | FrameCompleteness::Complete
                    );
                if scale_matches && completeness_matches {
                    Ok(())
                } else {
                    Err(format!(
                        "frame fidelity mismatch: displayed={:?}, target=s{}, completeness={:?}",
                        fidelity.displayed_scale_level,
                        fidelity.target_scale_level,
                        fidelity.completeness,
                    ))
                }
            }
            ProductAutomationAssertCondition::RenderMode { mode } => {
                let expected: RenderMode = (*mode).into();
                let actual = view
                    .layer(view.active_layer())
                    .expect("application view has an active layer")
                    .render_state()
                    .mode();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "active render mode is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::Projection { projection } => {
                let expected: Projection = (*projection).into();
                let actual = view.camera().projection();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "camera projection is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::LayerSampling {
                layer_index,
                sampling,
            } => {
                let expected: SamplingPolicy = (*sampling).into();
                let actual = view
                    .layers()
                    .get(*layer_index)
                    .ok_or_else(|| format!("layer index {layer_index} is out of range"))?
                    .render_state()
                    .sampling_policy();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "layer {layer_index} sampling is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::LayerIsoShading {
                layer_index,
                shading,
            } => {
                let expected: IsoShadingPolicy = (*shading).into();
                let actual = view
                    .layers()
                    .get(*layer_index)
                    .ok_or_else(|| format!("layer index {layer_index} is out of range"))?
                    .render_state()
                    .iso_parameters()
                    .ok_or_else(|| format!("layer {layer_index} is not in ISO mode"))?
                    .shading_policy();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "layer {layer_index} ISO shading is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::IsoLight { light } => {
                let actual = *view.iso_light();
                if light.matches(actual) {
                    Ok(())
                } else {
                    Err(format!(
                        "ISO light is {:?}, expected {}",
                        actual,
                        light.name()
                    ))
                }
            }
            ProductAutomationAssertCondition::ActiveTool { tool } => {
                let expected = (*tool).into();
                let actual = snapshot.transient().active_tool();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "active viewer tool is {:?}, expected {}",
                        actual,
                        tool.name()
                    ))
                }
            }
            ProductAutomationAssertCondition::CrosshairLinked => {
                let hit = app
                    .egui_ui
                    .viewer_tools
                    .crosshair
                    .as_ref()
                    .ok_or_else(|| "the viewer has no committed crosshair pick".to_owned())?;
                let world = hit.world_position.ok_or_else(|| {
                    "the committed crosshair has no world-space position".to_owned()
                })?;
                if hit.completeness != PickCompleteness::Exact {
                    return Err(format!(
                        "the committed crosshair is {:?}, expected exact",
                        hit.completeness
                    ));
                }
                if view.cross_section().center_world() != world {
                    return Err(format!(
                        "linked cross-section center {:?} does not match crosshair {:?}",
                        view.cross_section().center_world(),
                        world
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::RoiCommitted => {
                let Some(ViewerToolOverlay::RoiBox(overlay)) =
                    app.egui_ui.viewer_tools.overlay().copied()
                else {
                    return Err("the viewer has no ROI overlay".to_owned());
                };
                if overlay.phase() != ViewerOverlayPhase::Committed {
                    return Err("the viewer ROI overlay is still a preview".to_owned());
                }
                let roi = overlay.roi();
                if app.analysis_runtime.roi_origin() != roi.origin_zyx()
                    || app.analysis_runtime.roi_shape() != roi.shape_zyx()
                {
                    return Err(format!(
                        "analysis ROI {:?}/{:?} does not match viewer ROI {:?}/{:?}",
                        app.analysis_runtime.roi_origin(),
                        app.analysis_runtime.roi_shape(),
                        roi.origin_zyx(),
                        roi.shape_zyx()
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::DistanceCommitted => {
                let Some(ViewerToolOverlay::Distance(measurement)) =
                    app.egui_ui.viewer_tools.overlay().copied()
                else {
                    return Err("the viewer has no distance-measurement overlay".to_owned());
                };
                if measurement.phase() != ViewerOverlayPhase::Committed {
                    return Err("the distance-measurement overlay is still a preview".to_owned());
                }
                if !measurement.distance_micrometers().is_finite()
                    || measurement.distance_micrometers() <= 0.0
                {
                    return Err(format!(
                        "the committed distance is not positive and finite: {}",
                        measurement.distance_micrometers()
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::ViewerLayout { layout } => {
                let expected: ViewerLayout = (*layout).into();
                if view.layout() == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "viewer layout is {:?}, expected {:?}",
                        view.layout(),
                        expected
                    ))
                }
            }
            ProductAutomationAssertCondition::CrossSectionPanelSchedule {
                panel,
                min_generation,
                min_selected_resources,
            } => {
                let panel_id: PanelId = (*panel).into();
                if view.layout() != ViewerLayout::FourPanel {
                    return Err("four-panel runtime is not active".to_owned());
                }
                let panel_state = app
                    .render_coordination
                    .surface(panel_id.presentation_slot());
                let schedule = panel_state.cross_section_schedule().ok_or_else(|| {
                    format!("panel {} has no cross-section schedule", panel_id.label())
                })?;
                if let Some(min_generation) = min_generation
                    && schedule.generation < *min_generation
                {
                    return Err(format!(
                        "panel {} schedule generation is {}, expected at least {}",
                        panel_id.label(),
                        schedule.generation,
                        min_generation
                    ));
                }
                if let Some(min_selected_resources) = min_selected_resources
                    && schedule.selected_bricks < *min_selected_resources
                {
                    return Err(format!(
                        "panel {} selected {} resources, expected at least {}",
                        panel_id.label(),
                        schedule.selected_bricks,
                        min_selected_resources
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::FourPanelImagesDistinct {
                min_different_pixels,
            } => assert_four_panel_images_distinct(app, min_different_pixels.unwrap_or(1)),
            ProductAutomationAssertCondition::CrossSectionRetired => {
                assert_cross_section_retired(app)
            }
            ProductAutomationAssertCondition::SourceVerificationEvidence {
                min_accepted_progress_updates,
                min_cancelled_runs,
                min_accepted_successes,
            } => {
                let diagnostics = app
                    .source_verification_service
                    .as_ref()
                    .ok_or_else(|| "source-verification service is unavailable".to_owned())?
                    .diagnostics();
                if diagnostics.accepted_progress_updates < *min_accepted_progress_updates
                    || diagnostics.cancelled_runs < *min_cancelled_runs
                    || diagnostics.accepted_successes < *min_accepted_successes
                {
                    Err(format!(
                        "source-verification evidence is incomplete: progress={}, cancelled={}, successes={}",
                        diagnostics.accepted_progress_updates,
                        diagnostics.cancelled_runs,
                        diagnostics.accepted_successes,
                    ))
                } else {
                    Ok(())
                }
            }
            ProductAutomationAssertCondition::ImportWorkflowEvidence {
                required_stage_names,
                min_projected_named_stages,
                min_cancelled_runs,
                min_successful_runs,
                min_resumed_work_units,
                min_elapsed_ms,
                min_projected_elapsed_ms,
                max_peak_working_bytes,
            } => {
                let diagnostics = app.import.workers.diagnostics();
                let emitted = diagnostics.emitted_stage_names();
                let missing = required_stage_names
                    .iter()
                    .filter(|required| !emitted.contains(&required.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                if missing.is_empty()
                    && self.projected_import_stages.len() >= *min_projected_named_stages
                    && diagnostics.cancelled_runs >= *min_cancelled_runs
                    && diagnostics.successful_runs >= *min_successful_runs
                    && diagnostics.maximum_resumed_work_units >= *min_resumed_work_units
                    && diagnostics.maximum_elapsed_ms >= *min_elapsed_ms
                    && self.maximum_projected_import_elapsed_ms >= *min_projected_elapsed_ms
                    && diagnostics.maximum_peak_working_bytes <= *max_peak_working_bytes
                    && diagnostics.failed_runs == 0
                    && diagnostics.published_events >= diagnostics.successful_runs
                {
                    Ok(())
                } else {
                    Err(format!(
                        "import workflow evidence is incomplete: missing_stages={missing:?}, projected_stages={:?}, projected_elapsed_ms={}, cancelled_runs={}, successful_runs={}, failed_runs={}, published_events={}, resumed_work_units={}, maximum_elapsed_ms={}, peak_working_bytes={} (limit {})",
                        self.projected_import_stages,
                        self.maximum_projected_import_elapsed_ms,
                        diagnostics.cancelled_runs,
                        diagnostics.successful_runs,
                        diagnostics.failed_runs,
                        diagnostics.published_events,
                        diagnostics.maximum_resumed_work_units,
                        diagnostics.maximum_elapsed_ms,
                        diagnostics.maximum_peak_working_bytes,
                        max_peak_working_bytes,
                    ))
                }
            }
            ProductAutomationAssertCondition::RenderTargetPixels { width, height } => {
                let frame = product_presentation(app, PanelId::ThreeD).ok_or_else(|| {
                    "no GPU display frame exists for exact-size assertion".to_owned()
                })?;
                let extent = frame.extent();
                if u64::from(extent.width_pixels()) == *width
                    && u64::from(extent.height_pixels()) == *height
                {
                    Ok(())
                } else {
                    Err(format!(
                        "GPU render target is {}x{}, expected exact {}x{} pixels",
                        extent.width_pixels(),
                        extent.height_pixels(),
                        width,
                        height
                    ))
                }
            }
            ProductAutomationAssertCondition::ProjectState {
                bound,
                dirty,
                lifecycle,
                can_save,
                can_save_as,
                manual,
                autosave,
            } => {
                let facts = project_state_facts(app);
                let expected_lifecycle = project_store_lifecycle(*lifecycle);
                if facts.bound == *bound
                    && facts.dirty == *dirty
                    && facts.lifecycle == Some(expected_lifecycle)
                    && facts.can_save == *can_save
                    && facts.can_save_as == *can_save_as
                    && facts.manual == *manual
                    && facts.autosave == *autosave
                {
                    Ok(())
                } else {
                    Err(format!(
                        "project state does not match the assertion; expected lifecycle={}, observed={}, status_message={:?}",
                        lifecycle.name(),
                        project_state_json(app),
                        app.project_status_message,
                    ))
                }
            }
        }
    }

    fn observe_import_projection(&mut self, app: &MiranteWorkbenchApp) {
        let ImportWorkflowSnapshot::Importing(execution) = app.import.snapshot() else {
            return;
        };
        self.maximum_projected_import_elapsed_ms = self
            .maximum_projected_import_elapsed_ms
            .max(execution.elapsed_ms);
        let ImportProgressSnapshot::Stage { name, .. } = execution.progress else {
            return;
        };
        if self.projected_import_stages.len() < 64 && !self.projected_import_stages.contains(&name)
        {
            self.projected_import_stages.push(name);
        }
    }

    fn import_workflow_evidence_json(&self, app: &MiranteWorkbenchApp) -> Value {
        let diagnostics = app.import.workers.diagnostics();
        let maximum_completed_by_stage = diagnostics
            .maximum_completed_by_stage
            .iter()
            .map(|(stage, completed)| {
                json!({
                    "stage": stage.name(),
                    "completed_work_units": completed,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "worker_emitted_stage_names": diagnostics.emitted_stage_names(),
            "projected_named_stage_observations": self.projected_import_stages,
            "maximum_projected_elapsed_ms": self.maximum_projected_import_elapsed_ms,
            "maximum_completed_by_stage": maximum_completed_by_stage,
            "progress_updates": diagnostics.progress_updates,
            "published_events": diagnostics.published_events,
            "cancelled_runs": diagnostics.cancelled_runs,
            "successful_runs": diagnostics.successful_runs,
            "failed_runs": diagnostics.failed_runs,
            "maximum_resumed_work_units": diagnostics.maximum_resumed_work_units,
            "maximum_peak_working_bytes": diagnostics.maximum_peak_working_bytes,
            "maximum_elapsed_ms": diagnostics.maximum_elapsed_ms,
            "inspection_and_review_clock": import_pre_start_measurement_json(self.completed_import_pre_start_measurement),
            "primary_clock": import_primary_measurement_json(self.completed_import_primary_measurement),
            "publication_to_open_ready_clock": import_publication_to_open_ready_measurement_json(
                self.imported_open_ready_outcome,
            ),
            "last_successful_receipt": diagnostics
                .last_successful_import
                .as_ref()
                .map(successful_import_evidence_json)
                .unwrap_or(Value::Null),
            "fabricated_global_percentage_or_eta_observed": false,
        })
    }

    fn diagnostics_json(&self, app: &MiranteWorkbenchApp) -> Value {
        let snapshot = app.application.snapshot();
        let view = application_view(&snapshot);
        let active_layer = snapshot
            .catalog()
            .layer(view.active_layer())
            .expect("application view closes over the dataset catalog");
        let typed_render_error = app
            .render_coordination
            .frame_fidelity
            .last_failure_kind
            .map(|kind| format!("{kind:?}"))
            .or_else(|| {
                app.render_coordination
                    .frame_fidelity
                    .last_capacity_error
                    .clone()
            });
        json!({
            "dataset": {
                "path": app.dataset.selected_path().display().to_string(),
                "name": snapshot.catalog().label(),
                "layer_count": snapshot.catalog().len(),
                "active_logical_layer": view.active_layer().ordinal(),
                "current_time_index": view.timepoint().get(),
                "active_layer_label": active_layer.label(),
                "active_layer_dtype": format!("{:?}", active_layer.dtype()),
                "active_layer_shape": {
                    "x": active_layer.shape().x(),
                    "y": active_layer.shape().y(),
                    "z": active_layer.shape().z(),
                    "t": active_layer.shape().t(),
                },
                "active_scale_count": active_layer.scales().len(),
                "timepoint_count": active_layer.shape().t(),
            },
            "application_state": {
                "currentness_generation": snapshot.currentness().get(),
                "currentness_derivation": "ApplicationSnapshot_currentness_generation",
            },
            "render": {
                "active_render_mode": format!("{:?}", view.layer(view.active_layer()).expect("active layer").render_state().mode()),
                "projection": format!("{:?}", view.camera().projection()),
                "backend": format!("{:?}", app.render_coordination.frame_fidelity.backend),
                "adapter": app.startup_diagnostics.gpu_adapter.clone(),
                "native_surface_configuration_contract": {
                    "present_mode": "Fifo",
                    "desired_maximum_frame_latency": 1,
                    "evidence_scope": "configured_product_contract_not_queried_compositor_observation",
                },
                "last_error": typed_render_error,
                "gpu_display_frame_present": product_presentation(app, PanelId::ThreeD).is_some(),
                "frame_fidelity": {
                    "target_scale_level": app.render_coordination.frame_fidelity.target_scale_level,
                    "displayed_scale_level": app.render_coordination.frame_fidelity.displayed_scale_level,
                    "completeness": format!("{:?}", app.render_coordination.frame_fidelity.completeness),
                    "reason": format!("{:?}", app.render_coordination.frame_fidelity.reason),
                    "display_freshness": format!("{:?}", app.render_coordination.frame_fidelity.display_freshness),
                    "last_failure_kind": app.render_coordination.frame_fidelity.last_failure_kind.map(|kind| format!("{kind:?}")),
                    "last_capacity_error": app.render_coordination.frame_fidelity.last_capacity_error.clone(),
                },
                "progressive_presentation": app.native_presentation.product_gpu.as_ref().map(|product| json!({
                    "current_partial_frames_presented": product.current_partial_frames_presented,
                    "partial_to_settled_transitions": product.partial_to_settled_transitions,
                    "stale_frames_rejected": product.stale_frames_rejected,
                    "last_3d_cpu_timing": product.targets.get(&PanelId::ThreeD).and_then(|target| {
                        target.last_execution_timing.and_then(|execution| execution.cpu.map(|timing| json!({
                            "frame": execution.frame.get(),
                            "planning_ns": timing.planning_ns(),
                            "queue_submit_ns": timing.queue_submit_ns(),
                        })))
                    }),
                    "presented_frame_intervals": {
                        "enabled": product.presented_frame_interval_timing_enabled(),
                        "measurement_scope": "per_visible_panel_app_frame_publication_interval_not_os_compositor_present",
                        "capacity": 256,
                        "total_publications": product.total_presented_frame_publications(),
                        "dropped_samples": product.dropped_presented_frame_interval_samples(),
                        "samples": product.presented_frame_interval_samples().iter().map(|sample| json!({
                            "sequence": sample.sequence,
                            "panel": sample.panel.label(),
                            "frame": sample.frame.get(),
                            "interval_ns": sample.interval_ns,
                            "cpu_planning_ns": sample.cpu_planning_ns,
                            "cpu_queue_submit_ns": sample.cpu_queue_submit_ns,
                            "gpu_execution_id": sample.gpu_execution.map(|execution| execution.execution_id),
                            "gpu_target": sample.gpu_execution.map(|execution| execution.target.get()),
                            "gpu_generation": sample.gpu_execution.map(|execution| execution.display_generation),
                            "gpu_renderer_frame": sample.gpu_execution.map(|execution| execution.renderer_frame.get()),
                            "gpu_pass_kind": sample.gpu_execution.map(|execution| format!("{:?}", execution.pass_kind)),
                            "gpu_timing_complete": sample.gpu_timing.is_some(),
                            "gpu_batch_envelope_ns": sample.gpu_timing.and_then(|timing| timing.batch_gpu_envelope_ns),
                            "gpu_payload_copy_ns": sample.gpu_timing.and_then(|timing| timing.payload_copy_ns),
                            "gpu_render_pass_ns": sample.gpu_timing.and_then(|timing| timing.render_pass_ns),
                        })).collect::<Vec<_>>(),
                    },
                })),
                "performance_milestones": display_performance_milestones_json(app),
                "display_coordination": display_coordination_diagnostics_json(app),
            },
            "dataset_demand": {
                "current_scale_level": app.dataset.current_scale().get(),
                "last_plan_error": app.dataset.last_plan_error(),
                "dispatcher_pending": app.dataset.dispatcher().has_pending_work(),
                "last_fault": app.dataset.dispatcher().last_fault().map(|fault| fault.to_string()),
                "planned_scope_accounting": planned_scope_accounting_json(app),
                "refinement_handoff": refinement_handoff_diagnostics_json(app),
            },
            "dataset_runtime": app
                .dataset
                .dispatcher()
                .diagnostics()
                .ok()
                .map(dataset_runtime_diagnostics_json),
            "dataset_source_io": app
                .dataset
                .local_source_diagnostics()
                .map(local_dataset_source_diagnostics_json),
            "source_verification": source_verification_diagnostics_json(app),
            "retained_leases": retained_leases_diagnostics_json(app),
            "cross_section": cross_section_diagnostics_json(app),
            "gpu_adapter": app
                .native_presentation.product_gpu
                .as_ref()
                .map(|product| gpu_adapter_diagnostics_json(product.renderer.diagnostics())),
            "camera": {
                "projection": format!("{:?}", view.camera().projection()),
                "canonical_source": "ApplicationSnapshot_ViewState_camera",
                "target_world": view.camera().target().components(),
                "orientation_xyzw": view.camera().orientation().xyzw(),
                "orthographic_world_per_screen_point": view.camera().orthographic_world_per_screen_point(),
                "perspective_focal_length_screen_points": view.camera().perspective_focal_length_screen_points(),
                "perspective_view_distance_world": view.camera().perspective_view_distance_world(),
                "viewport": {
                    "width": app.render_coordination.render_viewport.width_pixels(),
                    "height": app.render_coordination.render_viewport.height_pixels(),
                },
            },
            "project_state": project_state_json(app),
            "project_store_evidence": project_store_evidence_json(app),
            "import_workflow_evidence": self.import_workflow_evidence_json(app),
        })
    }

    fn write_report_and_close(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        ctx: &egui::Context,
        status: &'static str,
        failure_reason: Option<String>,
    ) {
        if self.report_written {
            return;
        }
        self.observe_import_projection(app);
        self.report_written = true;
        if status != "passed"
            && let Err(err) = self.capture_failure_artifact(app)
        {
            tracing::error!(error = %err, "failed to capture product automation failure artifact");
        }
        let requested_window_inner_size_points = self
            .script
            .commands
            .iter()
            .find_map(|command| match command {
                ProductAutomationCommand::SetViewportSize { width, height } => Some(json!({
                    "width": width,
                    "height": height,
                })),
                _ => None,
            })
            .unwrap_or(Value::Null);
        let render_target_pixels = self
            .artifacts
            .iter()
            .rev()
            .find(|artifact| {
                artifact.kind == "viewport_capture" && !artifact.pixel_stats.is_blank()
            })
            .map(|artifact| {
                json!({
                    "width": artifact.width,
                    "height": artifact.height,
                })
            })
            .unwrap_or(Value::Null);
        let snapshot = app.application.snapshot();
        let requested_mapped_client_pixels = self
            .requested_mapped_client_pixels
            .map(|(width, height)| json!({ "width": width, "height": height }))
            .unwrap_or(Value::Null);
        let finished_process_cpu_time_ns = checked_process_cpu_time_ns();
        let process_cpu_time = match (
            self.started_process_cpu_time_ns,
            finished_process_cpu_time_ns,
        ) {
            (Some(started_ns), Some(finished_ns)) => json!({
                "available": true,
                "clock": "CLOCK_PROCESS_CPUTIME_ID",
                "started_ns": started_ns,
                "finished_ns": finished_ns,
                "elapsed_ns": finished_ns.saturating_sub(started_ns),
            }),
            _ => json!({
                "available": false,
                "clock": "CLOCK_PROCESS_CPUTIME_ID",
                "error": "clock_value_was_negative_or_not_representable_as_u64_nanoseconds",
            }),
        };
        let report = json!({
            "schema": AUTOMATION_REPORT_SCHEMA,
            "schema_version": AUTOMATION_REPORT_SCHEMA_VERSION,
            "status": status,
            "failure_reason": failure_reason,
            "viewport_evidence": {
                "requested_window_inner_size_points": requested_window_inner_size_points,
                "requested_mapped_client_pixels": requested_mapped_client_pixels,
                "pixels_per_point": ctx.pixels_per_point(),
                "render_target_pixels": render_target_pixels,
            },
            "started_at_epoch_ms": self.started_at_epoch_ms,
            "finished_at_epoch_ms": epoch_ms(),
            "duration_ms": duration_ms(self.started_at.elapsed()),
            "process_cpu_time": process_cpu_time,
            "binary": env::current_exe().ok().map(|path| path.display().to_string()),
            "build_provenance": t5_build_provenance_json(),
            "script": {
                "path": self.script_path.display().to_string(),
                "schema": self.script.schema.clone(),
                "schema_version": self.script.schema_version,
                "scenario": self.script.scenario.clone(),
                "command_count": self.script.commands.len(),
            },
            "hard_safety_limits": self.script.hard_safety_limits,
            "limit_observations": self.limit_observations.json(),
            "dataset": {
                "path": app.dataset.selected_path().display().to_string(),
                "name": snapshot.catalog().label(),
            },
            "project_state": project_state_json(app),
            "project_store_evidence": project_store_evidence_json(app),
            "import_workflow_evidence": self.import_workflow_evidence_json(app),
            "events": &self.events,
            "diagnostics": &self.diagnostics,
            "artifacts": self
                .artifacts
                .iter()
                .map(ProductAutomationArtifact::json)
                .collect::<Vec<_>>(),
            "final_diagnostics": self.diagnostics_json(app),
            "logs": {
                "app_log": app.startup_diagnostics.logs_path.as_ref().map(|path| path.display().to_string()),
            },
        });
        if let Some(parent) = self.report_path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            tracing::error!(
                path = %parent.display(),
                error = %err,
                "failed to create product automation report directory"
            );
        }
        match serde_json::to_vec_pretty(&report) {
            Ok(bytes) => {
                if let Err(err) = fs::write(&self.report_path, bytes) {
                    tracing::error!(
                        path = %self.report_path.display(),
                        error = %err,
                        "failed to write product automation report"
                    );
                }
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to serialize product automation report");
            }
        }
        app.egui_ui.allow_close_without_prompt = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn observe_and_enforce_hard_safety_limits(
        &mut self,
        app: &MiranteWorkbenchApp,
    ) -> Result<(), String> {
        let hard_safety_limits = self.script.hard_safety_limits;
        let diagnostics =
            app.dataset.dispatcher().diagnostics().map_err(|err| {
                format!("failed to read unified dataset runtime diagnostics: {err}")
            })?;
        self.limit_observations.observe_dataset_runtime(diagnostics);
        hard_safety_limits.check_dataset_runtime(diagnostics)?;
        Ok(())
    }

    fn capture_failure_artifact(&mut self, app: &mut MiranteWorkbenchApp) -> Result<(), String> {
        let artifact = self.capture_viewport_artifact(app, Some("failure-final-frame"))?;
        self.artifacts.push(artifact);
        Ok(())
    }
}

fn import_primary_measurement_json(measurement: Option<ImportPrimaryMeasurement>) -> Value {
    measurement.map_or(Value::Null, |measurement| {
        json!({
            "start_boundary": "accepted_start_import_command_immediately_before_worker_spawn",
            "end_boundary": "published_destination_verified_and_open_ready_for_normal_product_use",
            "clock": "std_instant_monotonic",
            "started_at_epoch_ms": measurement.started_at_epoch_ms,
            "open_ready_at_epoch_ms": measurement.open_ready_at_epoch_ms,
            "wall_time_ns": measurement.wall_time_ns,
            "process_cpu_time_ns": measurement.process_cpu_time_ns,
            "inspection_and_human_review_excluded": true,
            "published_capability_transfer_and_runtime_open_included": true,
        })
    })
}

fn t5_build_provenance_json() -> Value {
    json!({
        "repository_revision": option_env!("MIRANTE4D_T5_BUILD_REVISION"),
        "profile": option_env!("MIRANTE4D_T5_BUILD_PROFILE"),
        "compiler": option_env!("MIRANTE4D_T5_BUILD_COMPILER"),
        "target_mode": option_env!("MIRANTE4D_T5_BUILD_TARGET_MODE"),
        "opt_level": option_env!("MIRANTE4D_VIEWER_BUILD_OPT_LEVEL"),
        "debug": option_env!("MIRANTE4D_VIEWER_BUILD_DEBUG"),
        "custom_rustflags": option_env!("MIRANTE4D_VIEWER_BUILD_CUSTOM_RUSTFLAGS"),
        "rustc_wrapper": option_env!("MIRANTE4D_VIEWER_BUILD_RUSTC_WRAPPER"),
    })
}

fn import_pre_start_measurement_json(measurement: Option<ImportPreStartMeasurement>) -> Value {
    measurement.map_or(Value::Null, |measurement| {
        json!({
            "start_boundary": "normal_import_setup_command_dispatch",
            "end_boundary": "reviewed_start_import_command_dispatch",
            "wall_clock": "std_instant_monotonic",
            "cpu_clock": "process_cpu_time",
            "started_at_epoch_ms": measurement.started_at_epoch_ms,
            "start_command_at_epoch_ms": measurement.start_command_at_epoch_ms,
            "wall_time_ns": measurement.wall_time_ns,
            "process_cpu_time_ns": measurement.process_cpu_time_ns,
            "excluded_from_primary_clock": true,
            "human_review_interval_included_when_present": true,
        })
    })
}

fn import_publication_to_open_ready_measurement_json(
    outcome: Option<ImportedOpenReadyOutcome>,
) -> Value {
    let Some(outcome) = outcome else {
        return Value::Null;
    };
    let ImportedOpenReadyOutcome {
        measurement,
        evidence,
    } = outcome;
    let currentness = evidence.publication_currentness;
    json!({
        "status": "open_ready_complete",
        "publication_currentness_execution": {
            "contract_id": currentness.contract_id,
            "expected_snapshot_object_reads": currentness.expected_snapshot_object_reads,
            "first_inventory_object_reads": currentness.first_inventory_object_reads,
            "observed_snapshot_object_reads": currentness.observed_snapshot_object_reads,
            "second_inventory_object_reads": currentness.second_inventory_object_reads,
            "observed_total_object_reads": currentness.observed_total_object_reads,
            "observed_codec_decode_calls": currentness.observed_codec_decode_calls,
        },
        "source_verification_started_runs": evidence.source_verification_started_runs,
        "source_verification_progress_updates": evidence.source_verification_progress_updates,
        "source_verification_cancelled_runs": evidence.source_verification_cancelled_runs,
        "source_verification_failed_runs": evidence.source_verification_failed_runs,
        "source_verification_successes": evidence.source_verification_successes,
        "start_boundary": "import_worker_published_event",
        "end_boundary": "published_destination_verified_and_open_ready_for_normal_product_use",
        "wall_clock": "std_instant_monotonic",
        "cpu_clock": "process_cpu_time",
        "published_at_epoch_ms": measurement.published_at_epoch_ms,
        "open_ready_at_epoch_ms": measurement.open_ready_at_epoch_ms,
        "wall_time_ns": measurement.wall_time_ns,
        "process_cpu_time_ns": measurement.process_cpu_time_ns,
        "included_in_primary_clock": true,
        "transfer_mode": "staged_verified_capability",
    })
}

fn import_receipt_json(receipt: &ImportReceipt) -> Value {
    json!({
        "package_id": receipt.package_id.to_string(),
        "scientific_content_id": receipt.scientific_content_id.to_string(),
        "statistics": import_statistics_json(&receipt.statistics),
    })
}

fn successful_import_evidence_json(
    evidence: &crate::import_worker_service::SuccessfulImportEvidence,
) -> Value {
    let receipt = import_receipt_json(&evidence.receipt);
    let published_event = evidence
        .published_timing
        .as_ref()
        .map_or(Value::Null, |timing| {
            json!({
                "published_at_epoch_ms": timing.published_at_epoch_ms,
                "process_cpu_time_ns": timing.process_cpu_time_ns,
            })
        });
    json!({
        "review_id": evidence.review_id.get(),
        "operation_token": operation_token_json(&evidence.token),
        "destination": evidence.destination,
        "reviewed_source_fingerprint_sha256": evidence.source_fingerprint.to_string(),
        "reviewed_source_bytes": evidence.reviewed_source_bytes,
        "published_event": published_event,
        "package_id": receipt["package_id"],
        "scientific_content_id": receipt["scientific_content_id"],
        "statistics": receipt["statistics"],
    })
}

fn operation_token_json(token: &mirante4d_application::OperationToken) -> Value {
    json!({
        "operation_id": token.operation_id().get(),
        "task_id": token.task_id().get(),
        "kind": format!("{:?}", token.kind()),
        "source_session_generation": token.source_session_generation().get(),
        "currentness_generation": token.currentness_generation().get(),
    })
}

fn import_statistics_json(statistics: &ImportStatistics) -> Value {
    let mut object = serde_json::Map::new();
    macro_rules! insert_u64 {
        ($($field:literal => $value:expr),* $(,)?) => {
            $(object.insert($field.to_owned(), Value::from($value));)*
        };
    }
    insert_u64! {
        "source_bytes_read" => statistics.source_bytes_read,
        "source_revalidation_bytes_read" => statistics.source_revalidation_bytes_read,
        "native_decoded_bytes" => statistics.native_decoded_bytes,
        "base_native_decoded_bytes" => statistics.base_native_decoded_bytes,
        "scientific_identity_native_decoded_bytes" => statistics.scientific_identity_native_decoded_bytes,
        "tiff_open_count" => statistics.tiff_open_count,
        "native_chunk_decode_count" => statistics.native_chunk_decode_count,
        "logical_output_bytes" => statistics.logical_output_bytes,
        "checkpoint_payload_bytes" => statistics.checkpoint_payload_bytes,
        "checkpoint_journal_bytes" => statistics.checkpoint_journal_bytes,
        "checkpoint_watermark_bytes" => statistics.checkpoint_watermark_bytes,
        "checkpoint_durable_work_units" => statistics.checkpoint_durable_work_units,
        "checkpoint_pending_work_units" => statistics.checkpoint_pending_work_units,
        "checkpoint_committed_batches" => statistics.checkpoint_committed_batches,
        "codec_encode_calls" => statistics.codec_encode_calls,
        "codec_encode_time_ns" => statistics.codec_encode_time_ns,
        "codec_decode_calls" => statistics.codec_decode_calls,
        "codec_decode_time_ns" => statistics.codec_decode_time_ns,
        "sync_calls" => statistics.sync_calls,
        "sync_time_ns" => statistics.sync_time_ns,
        "scientific_brick_reads" => statistics.scientific_brick_reads,
        "staged_structure_object_reads" => statistics.staged_structure_object_reads,
        "staged_exact_object_reads" => statistics.staged_exact_object_reads,
        "scientific_object_reads" => statistics.scientific_object_reads,
        "scientific_payload_object_reads" => statistics.scientific_payload_object_reads,
        "scientific_range_requests" => statistics.scientific_range_requests,
        "scientific_encoded_bytes_read" => statistics.scientific_encoded_bytes_read,
        "scientific_decoded_bytes" => statistics.scientific_decoded_bytes,
        "object_reads" => statistics.object_reads,
        "sampled_peak_open_file_descriptors" => statistics.sampled_peak_open_file_descriptors,
        "open_file_descriptor_structural_bound" => statistics.open_file_descriptor_structural_bound,
        "peak_open_file_descriptors" => statistics.peak_open_file_descriptors,
        "preflight_temporary_bytes_bound" => statistics.preflight_temporary_bytes_bound,
        "peak_temporary_bytes" => statistics.peak_temporary_bytes,
        "peak_checkpoint_regular_files" => statistics.peak_checkpoint_regular_files,
        "peak_working_bytes" => statistics.peak_working_bytes,
        "peak_process_rss_bytes" => statistics.peak_process_rss_bytes,
        "resumed_work_units" => statistics.resumed_work_units,
        "produced_work_units" => statistics.produced_work_units,
        "primary_wall_time_ns" => statistics.primary_wall_time_ns,
        "primary_cpu_time_ns" => statistics.primary_cpu_time_ns,
    }
    object.insert(
        "stages".to_owned(),
        Value::Array(
            statistics
                .stages
                .iter()
                .map(|timing| {
                    json!({
                        "stage": import_stage_evidence_name(timing.stage),
                        "wall_time_ns": timing.wall_time_ns,
                        "cpu_time_ns": timing.cpu_time_ns,
                    })
                })
                .collect(),
        ),
    );
    Value::Object(object)
}

fn import_stage_evidence_name(stage: mirante4d_import_pipeline::ImportStage) -> String {
    match stage {
        mirante4d_import_pipeline::ImportStage::SourceRevalidation { pass } => {
            format!("{}-{pass}", stage.name())
        }
        mirante4d_import_pipeline::ImportStage::PyramidProduction { scale } => {
            format!("{}-{scale}", stage.name())
        }
        _ => stage.name().to_owned(),
    }
}

fn checked_process_cpu_time_ns() -> Option<u64> {
    let time = clock_gettime(ClockId::ProcessCPUTime);
    u64::try_from(time.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(time.tv_nsec).ok()?)
}

fn process_cpu_time_ns() -> u64 {
    checked_process_cpu_time_ns().unwrap_or(u64::MAX)
}

fn active_lease_cohort_status(
    app: &MiranteWorkbenchApp,
) -> Option<crate::retained_leases::RetainedLeaseStatus> {
    let snapshot = app.application.snapshot();
    let view = application_view(&snapshot);
    let identity = app
        .dataset
        .scope_requirements(crate::dataset_requests::SCOPE_CURRENT_3D)
        .first()?
        .identity();
    Some(app.dataset.retained_leases().cohort_status(
        identity,
        view.active_layer(),
        view.timepoint(),
        app.dataset.current_scale(),
    ))
}

fn lease_cohort_status_json(status: crate::retained_leases::RetainedLeaseStatus) -> Value {
    json!({
        "required": status.required,
        "retained": status.retained,
        "missing": status.missing,
        "complete": status.is_complete(),
    })
}

fn retained_leases_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let bridge = app.dataset.retained_leases();
    let gpu_only = app.dataset.gpu_only_display_payloads();
    let unavailable = bridge.missing_len().saturating_sub(gpu_only);
    json!({
        "required": bridge.required_len(),
        "retained": bridge.retained_len(),
        "cpu_absent": bridge.missing_len(),
        "gpu_only_display": gpu_only,
        "unavailable": unavailable,
        "complete_cpu_or_gpu": bridge.required_len() != 0 && unavailable == 0,
        "active_cohort": active_lease_cohort_status(app).map(lease_cohort_status_json),
    })
}

fn assert_four_panel_images_distinct(
    app: &MiranteWorkbenchApp,
    min_different_pixels: usize,
) -> Result<(), String> {
    let mut images = Vec::new();
    images.push(read_product_target_image(app, "3D", PanelId::ThreeD)?);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        images.push(read_product_target_image(app, panel_id.label(), panel_id)?);
    }
    assert_gpu_display_images_distinct("four-panel frames", &images, min_different_pixels)
}

fn read_product_target_image(
    app: &MiranteWorkbenchApp,
    label: &str,
    panel: PanelId,
) -> Result<(String, usize, usize, Vec<u8>), String> {
    let capture = product_target_capture(app, panel)
        .ok_or_else(|| format!("{label} has no current GPU validation capture"))?;
    let width = usize::try_from(capture.extent().width_pixels())
        .map_err(|_| format!("{label} frame width does not fit in usize"))?;
    let height = usize::try_from(capture.extent().height_pixels())
        .map_err(|_| format!("{label} frame height does not fit in usize"))?;
    Ok((label.to_owned(), width, height, capture.rgba8().to_vec()))
}

fn assert_gpu_display_images_distinct(
    image_group: &str,
    images: &[(String, usize, usize, Vec<u8>)],
    min_different_pixels: usize,
) -> Result<(), String> {
    let min_different_pixels = min_different_pixels.max(1);
    let mut compared_pairs = 0usize;
    for left_index in 0..images.len() {
        for right_index in (left_index + 1)..images.len() {
            let (left_label, left_width, left_height, left_rgba) = &images[left_index];
            let (right_label, right_width, right_height, right_rgba) = &images[right_index];
            if left_width != right_width || left_height != right_height {
                continue;
            }
            compared_pairs += 1;
            let different_pixels = left_rgba
                .chunks_exact(4)
                .zip(right_rgba.chunks_exact(4))
                .filter(|(left, right)| left != right)
                .count();
            if different_pixels < min_different_pixels {
                return Err(format!(
                    "{image_group} {left_label} and {right_label} differ in {} pixels, expected at least {}",
                    different_pixels, min_different_pixels
                ));
            }
        }
    }
    if compared_pairs == 0 {
        return Err(format!(
            "{image_group} assertion did not find any same-sized frame pairs to compare"
        ));
    }
    Ok(())
}

fn assert_cross_section_retired(app: &MiranteWorkbenchApp) -> Result<(), String> {
    let snapshot = app.application.snapshot();
    let view = application_view(&snapshot);
    if view.layout() != ViewerLayout::Single3d {
        return Err(format!(
            "viewer layout is {:?}, expected Single3d for retired cross-section state",
            view.layout()
        ));
    }
    for scope in [
        crate::dataset_requests::SCOPE_CROSS_SECTION_XY,
        crate::dataset_requests::SCOPE_CROSS_SECTION_XZ,
        crate::dataset_requests::SCOPE_CROSS_SECTION_YZ,
    ] {
        if !app.dataset.scope_requirements(scope).is_empty() {
            return Err(format!(
                "cross-section dataset demand scope {scope} is still active"
            ));
        }
    }
    let active_targets = app
        .native_presentation
        .product_gpu
        .as_ref()
        .map_or(0, |product| {
            [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .filter(|panel| {
                    product
                        .targets
                        .get(panel)
                        .and_then(|target| target.presented.as_ref())
                        .is_some()
                })
                .count()
        });
    if active_targets != 0 {
        return Err(format!(
            "cross-section display frames are still active: {}",
            active_targets
        ));
    }
    Ok(())
}

fn timing_samples_json(samples: &mirante4d_application::DisplayTimingSamples) -> Value {
    let mut retained = (0..samples.retained_count())
        .filter_map(|index| samples.sample(index))
        .collect::<Vec<_>>();
    retained.sort_unstable();
    let p95_ns = (!retained.is_empty()).then(|| {
        let rank = retained.len().saturating_mul(95).div_ceil(100);
        retained[rank.saturating_sub(1)]
    });
    json!({
        "capacity": mirante4d_application::DISPLAY_TIMING_SAMPLE_CAPACITY,
        "total_count": samples.total_count(),
        "retained_count": samples.retained_count(),
        "overwritten_count": samples.overwritten_count(),
        "maximum_ns": samples.maximum_ns(),
        "p95_ns": p95_ns,
        "retained_samples_ns_oldest_first": (0..samples.retained_count())
            .filter_map(|index| samples.sample(index))
            .collect::<Vec<_>>(),
    })
}

fn display_performance_milestones_json(app: &MiranteWorkbenchApp) -> Value {
    let milestones = &app.display_performance_milestones;
    let snapshot = app.application.snapshot();
    let visible_slots: &[PresentationSlot] = match application_view(&snapshot).layout() {
        ViewerLayout::Single3d => &[PresentationSlot::ThreeD],
        ViewerLayout::FourPanel => &PresentationSlot::ALL,
    };
    let visible_panels = visible_slots
        .iter()
        .copied()
        .map(|slot| {
            let panel = milestones.panel(slot);
            json!({
                "panel": PanelId::from_presentation_slot(slot).label(),
                "first_current_presented_ms": panel.first_current_presented_ms(),
                "first_useful_frame_ms": panel.first_useful_frame_ms(),
                "complete_coarse_ms": panel.complete_coarse_ms(),
                "complete_replacement_ms": panel.complete_replacement_ms(),
                "target_settled_ms": panel.target_settled_ms(),
                "visible_layer_overflow": panel.visible_layer_overflow(),
                "visible_layers": panel
                    .visible_layers()
                    .iter()
                    .filter_map(|layer| {
                        layer.layer_ordinal().map(|layer_ordinal| json!({
                            "layer_ordinal": layer_ordinal,
                            "first_current_presented_ms": layer.first_current_presented_ms(),
                            "first_useful_frame_ms": layer.first_useful_frame_ms(),
                            "complete_coarse_ms": layer.complete_coarse_ms(),
                            "complete_replacement_ms": layer.complete_replacement_ms(),
                            "target_settled_ms": layer.target_settled_ms(),
                        }))
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "scope": "coordinated_visible_layout",
        "input_generation": milestones.generation(),
        "first_current_presented_ms": milestones.first_current_presented_ms(),
        "first_useful_frame_ms": milestones.first_useful_frame_ms(),
        "complete_coarse_ms": milestones.complete_coarse_ms(),
        "complete_replacement_ms": milestones.complete_replacement_ms(),
        "target_settled_ms": milestones.target_settled_ms(),
        "visible_panels": visible_panels,
        "three_d_panel_only": {
            "first_useful_frame_ms": milestones.three_d_first_useful_frame_ms(),
            "complete_coarse_ms": milestones.three_d_complete_coarse_ms(),
            "complete_replacement_ms": milestones.three_d_complete_replacement_ms(),
            "target_settled_ms": milestones.three_d_target_settled_ms(),
        },
    })
}

fn planned_scope_accounting_json(app: &MiranteWorkbenchApp) -> Value {
    let planner = app.camera_demand_planner.diagnostics();
    let scopes = app
        .prepared_scope_render_plans
        .iter()
        .map(|(scope, plan)| {
            let label = match *scope {
                crate::dataset_requests::SCOPE_CURRENT_3D => "current_3d",
                crate::dataset_requests::SCOPE_CURRENT_3D_REFINEMENT => "current_3d_refinement",
                crate::dataset_requests::SCOPE_CROSS_SECTION_XY => "cross_section_xy",
                crate::dataset_requests::SCOPE_CROSS_SECTION_XZ => "cross_section_xz",
                crate::dataset_requests::SCOPE_CROSS_SECTION_YZ => "cross_section_yz",
                _ => "other",
            };
            json!({
                "scope": scope,
                "label": label,
                "planned_payload_bytes": plan.planned_payload_bytes,
                "primary_resource_count": plan.primary_resource_count,
                "total_requirements": plan.requirements.body().canonical().len(),
                "payload_fact": "exact_semantic_planned_payload_for_this_requirement_body",
            })
        })
        .collect::<Vec<_>>();
    json!({
        "last_planner_candidates_visited": app.last_visible_demand_candidates_visited,
        "full_demand_traversals": planner.completed,
        "planner_candidate_visits": planner.completed_candidates_visited,
        "ui_thread_candidate_visits": planner.ui_thread_candidates_visited,
        // Camera-demand submission and result polling are bounded slot/atomic
        // operations. The UI path has no blocking wait primitive; keep this
        // explicit zero fact beside the traversals it qualifies.
        "ui_wait_for_demand_preparation_count": 0_u64,
        "demand_work": planner.submitted,
        "scopes": scopes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefinementHandoffPhase {
    Inactive,
    AwaitingHiddenTargetRegistration,
    AwaitingHiddenTargetRequest,
    HiddenTargetStreaming,
    HiddenTargetPresented,
}

impl RefinementHandoffPhase {
    const fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::AwaitingHiddenTargetRegistration => "awaiting_hidden_target_registration",
            Self::AwaitingHiddenTargetRequest => "awaiting_hidden_target_request",
            Self::HiddenTargetStreaming => "hidden_target_streaming",
            Self::HiddenTargetPresented => "hidden_target_presented",
        }
    }
}

const fn refinement_handoff_phase(
    staged_plan_installed: bool,
    hidden_target_registered: bool,
    hidden_target_request_bound: bool,
    hidden_presentation_matches_request: bool,
) -> RefinementHandoffPhase {
    if !staged_plan_installed {
        RefinementHandoffPhase::Inactive
    } else if !hidden_target_registered {
        RefinementHandoffPhase::AwaitingHiddenTargetRegistration
    } else if !hidden_target_request_bound {
        RefinementHandoffPhase::AwaitingHiddenTargetRequest
    } else if !hidden_presentation_matches_request {
        RefinementHandoffPhase::HiddenTargetStreaming
    } else {
        RefinementHandoffPhase::HiddenTargetPresented
    }
}

fn refinement_handoff_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let staged_plan_installed = app.dataset.staging_current_refinement();
    let hidden_target = app
        .native_presentation
        .product_gpu
        .as_ref()
        .and_then(|product| product.staging_3d.as_ref());
    let hidden_request = hidden_target.and_then(|target| target.request.as_ref());
    let hidden_presentation_matches_request =
        hidden_target
            .zip(hidden_request)
            .is_some_and(|(target, request)| {
                target.presented.as_ref().is_some_and(|frame| {
                    frame.frame() == request.intent.frame()
                        && frame.extent() == request.intent.extent()
                })
            });
    let phase = refinement_handoff_phase(
        staged_plan_installed,
        hidden_target.is_some(),
        hidden_request.is_some(),
        hidden_presentation_matches_request,
    );
    let visible = app
        .native_presentation
        .product_gpu
        .as_ref()
        .and_then(|product| product.targets.get(&PanelId::ThreeD));
    json!({
        "phase": phase.name(),
        "staged_plan_installed": staged_plan_installed,
        "holding_previous_presentation": app.dataset.holding_previous_presentation(),
        "refinement_scope_requirement_count": app
            .dataset
            .scope_requirements(crate::dataset_requests::SCOPE_CURRENT_3D_REFINEMENT)
            .len(),
        "refinement_scope_required_prefix_len": app
            .dataset
            .scope_required_prefix_len(crate::dataset_requests::SCOPE_CURRENT_3D_REFINEMENT),
        "prepared_refinement_render_plan_installed": app
            .prepared_scope_render_plans
            .contains_key(&crate::dataset_requests::SCOPE_CURRENT_3D_REFINEMENT),
        "pending_visible_demand_install": app.pending_visible_demand_plan.is_some(),
        "camera_demand_planner_active": app.camera_demand_planner.has_outstanding_request(),
        "hidden_target_registered": hidden_target.is_some(),
        "hidden_target_request_bound": hidden_request.is_some(),
        "hidden_target_request_requirement_count": hidden_request
            .map(|request| request.requirements.resource_keys().len()),
        "hidden_target_satisfied_requirement_count": hidden_target
            .map(|target| target.satisfied_requirement_keys.len()),
        "hidden_target_last_renderer_available_resources": hidden_target
            .map(|target| target.last_renderer_available_resources),
        "hidden_target_presentation_matches_request": hidden_presentation_matches_request,
        "hidden_target_last_execution_present": hidden_target
            .is_some_and(|target| target.last_execution_timing.is_some()),
        "progressive_probe_can_evaluate_hidden_target": hidden_request.is_some(),
        "visible_3d_presentation_completeness": visible
            .and_then(|target| target.presented.as_ref())
            .map(|frame| format!("{:?}", frame.progress().completeness())),
    })
}

fn source_verification_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = app.application.snapshot();
    let state = match snapshot.source() {
        SourceVerificationSnapshot::Required => "Required",
        SourceVerificationSnapshot::Verifying { .. } => "Verifying",
        SourceVerificationSnapshot::Verified(_) => "Verified",
    };
    let Some(service) = app.source_verification_service.as_ref() else {
        return json!({
            "state": state,
            "active_operation": false,
            "service": Value::Null,
        });
    };
    let diagnostics = service.diagnostics();
    json!({
        "state": state,
        "active_operation": service.active_token().is_some(),
        "service": {
            "started_runs": diagnostics.started_runs,
            "accepted_progress_updates": diagnostics.accepted_progress_updates,
            "cancelled_runs": diagnostics.cancelled_runs,
            "failed_runs": diagnostics.failed_runs,
            "accepted_successes": diagnostics.accepted_successes,
            "completed_reader_runs": diagnostics.completed_reader_runs,
            "completed_reader_scope": "completed_separate_strict_verification_readers",
            "completed_reader_counters_include_only_completed_runs": true,
            "completed_reader_operations": diagnostics.reader.physical_range_read_operations,
            "completed_reader_bytes": diagnostics.reader.physical_encoded_bytes_read,
            "completed_codec_decodes": diagnostics.reader.codec_decode_operations,
            "completed_reader": local_package_read_diagnostics_json(diagnostics.reader),
        },
    })
}

fn display_coordination_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let generation = app.render_coordination.display_generation();
    json!({
        "instrumentation_epoch": "app_private_monotonic_epoch",
        "input_generation": generation.input_generation,
        "current_presentation_generation": generation.current_presentation_generation,
        "presentation_generation_gap": generation.presentation_generation_gap(),
        "main_loop_heartbeat": generation.main_loop_heartbeat,
        "heartbeat_at_input_generation": generation.heartbeat_at_input_generation,
        "heartbeat_at_current_presentation": generation.heartbeat_at_current_presentation,
        "current_presentation_gap_heartbeats": generation.current_presentation_gap_heartbeats,
        "maximum_presentation_gap_heartbeats": generation.maximum_presentation_gap_heartbeats,
        "input_generation_at_ns": generation.input_generation_at_ns,
        "current_presentation_at_ns": generation.current_presentation_at_ns,
        "current_presentation_gap_ns": generation.current_presentation_gap_ns,
        "maximum_presentation_gap_ns": generation.maximum_presentation_gap_ns,
        "active_input_main_loop_gap_ns": {
            "current": generation.current_main_loop_heartbeat_gap_ns,
            "maximum": generation.maximum_main_loop_heartbeat_gap_ns,
            "samples": timing_samples_json(
                app.render_coordination.active_main_loop_gap_samples(),
            ),
            "scope": "only_while_newest_input_not_current",
        },
        "active_input_presentation_gap_ns": {
            "current": generation.current_presentation_gap_ns,
            "maximum": generation.maximum_presentation_gap_ns,
            "samples": timing_samples_json(
                app.render_coordination.active_presentation_gap_samples(),
            ),
            "scope": "only_while_newest_input_not_current",
        },
        "raw_main_loop_gap_ns": {
            "current": generation.raw_current_main_loop_heartbeat_gap_ns,
            "maximum": generation.raw_maximum_main_loop_heartbeat_gap_ns,
            "scope": "includes_settled_event_driven_idle",
        },
        "durable_gesture_commits": generation.durable_gesture_commits,
        "admitted_generation_latency": timing_samples_json(
            app.render_coordination.presentation_latency_samples(),
        ),
        "semantic_interaction_task_duration": {
            "ownership": "SetCamera_or_SetLayout_application_dispatch_reconciliation_and_service_pump_attribution_only",
            "whole_idle_frame_included": false,
            "samples": timing_samples_json(
                app.render_coordination.interaction_task_duration_samples(),
            ),
        },
        "active_ui_update_duration": {
            "ownership": "complete_eframe_App_ui_callback_when_input_or_loading_work_is_active",
            "excludes": "compositor_present_and_vsync_outside_callback",
            "settled_event_driven_idle_updates_included": false,
            "samples": timing_samples_json(
                app.render_coordination.active_ui_update_duration_samples(),
            ),
        },
        "coordinated_visible_layout_current_complete": coordinated_visible_layout_current_complete(app),
        "os_input_injected": false,
        "os_input_claimed": false,
    })
}

fn cross_section_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = app.application.snapshot();
    let view = application_view(&snapshot);
    let canonical_cross_section = *view.cross_section();
    let cross_section_state = CrossSectionViewState::from_canonical(canonical_cross_section);
    let panels = app
        .render_coordination
        .iter()
        .map(|(slot, panel)| {
            let panel_id = PanelId::from_presentation_slot(slot);
            let canonical_plane = panel_id.cross_section_panel().map(|panel| {
                let panel_view = cross_section_state.view(panel);
                json!({
                    "source": "canonical_linked_cross_section_view",
                    "plane_origin_world": panel_view.center_world(),
                    "u_axis_world": panel_view.right_world(),
                    "v_axis_world": panel_view.down_world(),
                    "normal_away_world": panel_view.normal_away_world(),
                    "world_per_screen_point": panel_view.scale_world_per_screen_point(),
                })
            });
            json!({
                "panel_id": panel_id.label(),
                "canonical_plane_geometry": canonical_plane,
                "generation": panel.generation(),
                "displayed_generation": panel.displayed_generation(),
                "display_current": panel.display_current(),
                "presentation_viewport": panel.presentation_viewport().map(|viewport| {
                    json!({
                        "width_points": viewport.width_points(),
                        "height_points": viewport.height_points(),
                    })
                }),
                "render_viewport": panel.render_viewport().map(|viewport| {
                    json!({
                        "width": viewport.width_pixels(),
                        "height": viewport.height_pixels(),
                    })
                }),
                "schedule": panel.cross_section_schedule().map(panel_schedule_json),
                "layer_presentation_overflow": panel.layer_presentation_overflow().map(|overflow| json!({
                    "actual": overflow.actual,
                    "maximum": overflow.maximum,
                })),
                "layers": panel.layer_presentations().iter().map(|layer| json!({
                    "layer_ordinal": layer.layer_ordinal,
                    "expected_scale_level": layer.expected_scale_level,
                    "displayed_scale_level": layer.displayed_scale_level,
                    "available_requirements": layer.available_requirements,
                    "total_requirements": layer.total_requirements,
                    "current": layer.current,
                })).collect::<Vec<_>>(),
                "display_frame": app
                    .native_presentation
                    .product_gpu
                    .as_ref()
                    .and_then(|product| product.targets.get(&panel_id))
                    .and_then(|target| target.presented.as_ref())
                    .map(|displayed| {
                        let progress = displayed.progress();
                        let coverage = progress.coverage();
                        json!({
                            "frame": displayed.frame().get(),
                            "width": displayed.extent().width_pixels(),
                            "height": displayed.extent().height_pixels(),
                            "completeness": format!("{:?}", progress.completeness()),
                            "limitation": progress.limitation().map(|value| format!("{value:?}")),
                            "available_requirements": coverage.available_requirements(),
                            "total_requirements": coverage.total_requirements(),
                        })
                    }),
            })
        })
        .collect::<Vec<_>>();
    let display_frame_count = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .filter(|panel| product_presentation(app, *panel).is_some())
        .count();
    json!({
        "schema": "mirante4d-cross-section-panel-diagnostics",
        "schema_version": 1,
        "layout": format!("{:?}", view.layout()),
        "canonical_linked_view": {
            "source": "ApplicationSnapshot_ViewState_cross_section",
            "center_world": canonical_cross_section.center_world().components(),
            "orientation_xyzw": canonical_cross_section.orientation().xyzw(),
            "world_per_screen_point": canonical_cross_section.scale_world_per_screen_point(),
            "depth_world": canonical_cross_section.depth_world(),
        },
        "active_panel": snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel_id| panel_id.label().to_owned()),
        "display_frame_count": display_frame_count,
        "product_display_path": "unified_dataset_leases_to_render_wgpu",
        "demand_scopes": {
            "xy": app.dataset.scope_requirements(crate::dataset_requests::SCOPE_CROSS_SECTION_XY).len(),
            "xz": app.dataset.scope_requirements(crate::dataset_requests::SCOPE_CROSS_SECTION_XZ).len(),
            "yz": app.dataset.scope_requirements(crate::dataset_requests::SCOPE_CROSS_SECTION_YZ).len(),
        },
        "active_lease_cohort": active_lease_cohort_status(app).map(lease_cohort_status_json),
        "panels": panels,
    })
}

fn panel_schedule_json(schedule: crate::CrossSectionPanelScheduleState) -> Value {
    json!({
        "generation": schedule.generation,
        "target_scale_level": schedule.target_scale_level,
        "render_scale_level": schedule.render_scale_level,
        "fallback_scale_level": schedule.fallback_scale_level,
        "selected_resources": schedule.selected_bricks,
        "occupied_selected_resources": schedule.occupied_selected_bricks,
        "missing_occupied_resources": schedule.missing_occupied_bricks,
        "estimated_decoded_bytes": schedule.estimated_decoded_bytes,
        "decoded_budget_bytes": schedule.decoded_budget_bytes,
        "status": format!("{:?}", schedule.status),
        "reason": format!("{:?}", schedule.reason),
    })
}

#[derive(Debug, Serialize)]
struct ProductAutomationEvent {
    command_index: usize,
    command: &'static str,
    status: &'static str,
    event_epoch_ms: u128,
    duration_ms: f64,
    details: Value,
}

impl ProductAutomationEvent {
    fn passed(
        command_index: usize,
        command: &'static str,
        duration: Duration,
        details: Value,
    ) -> Self {
        Self {
            command_index,
            command,
            status: "passed",
            event_epoch_ms: epoch_ms(),
            duration_ms: duration_ms(duration),
            details,
        }
    }

    fn failed(
        command_index: usize,
        command: &'static str,
        duration: Duration,
        reason: String,
    ) -> Self {
        Self {
            command_index,
            command,
            status: "failed",
            event_epoch_ms: epoch_ms(),
            duration_ms: duration_ms(duration),
            details: json!({ "reason": reason }),
        }
    }
}

enum CommandProgress {
    Done(Value),
    Waiting,
    PassiveWaiting(Option<Duration>),
}

enum AutomationStatus {
    Continue,
    Waiting { repaint_after: Option<Duration> },
    Finished,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProductAutomationProjectStateFacts {
    bound: bool,
    dirty: bool,
    lifecycle: Option<ProjectStoreLifecycle>,
    can_save: bool,
    can_save_as: bool,
    manual: bool,
    autosave: bool,
}

fn initial_save_with_durable_edit(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    path: &Path,
) -> Result<CommandProgress, String> {
    if app
        .project_store_noninteractive_paths
        .initial_save
        .is_some()
    {
        return Err("a noninteractive initial-Save path is already pending".to_owned());
    }
    let service = app
        .project_store
        .as_ref()
        .ok_or_else(|| "project-store service is unavailable".to_owned())?;
    if !service.can_save()
        || !matches!(
            service.status().lifecycle(),
            ProjectStoreLifecycle::Unbound | ProjectStoreLifecycle::Provisional
        )
    {
        return Err("initial_save_with_edit requires a saveable unestablished store".to_owned());
    }

    app.project_store_noninteractive_paths.initial_save = Some(path.to_path_buf());
    if let Err(fault) = app
        .application
        .dispatch(ApplicationCommand::RequestProjectSave)
    {
        app.project_store_noninteractive_paths.initial_save = None;
        return Err(format!("initial project Save was rejected: {fault:?}"));
    }
    let events = app.application.drain_events(256);
    let captured_revision = events
        .iter()
        .find_map(|event| match event {
            ApplicationEvent::ProjectSaveRequested { projection, .. } => {
                Some(projection.revision())
            }
            _ => None,
        })
        .ok_or_else(|| "initial project Save emitted no capture event".to_owned())?;
    for event in &events {
        app.observe_source_application_event(event);
        app.observe_project_application_event(event);
    }
    if app
        .project_store_noninteractive_paths
        .initial_save
        .is_some()
    {
        return Err("initial Save path was not consumed by the project event route".to_owned());
    }
    if !app
        .project_store
        .as_ref()
        .is_some_and(|service| service.status().foreground_active())
    {
        return Err("initial Save was not active before the durable edit".to_owned());
    }

    let snapshot = app.application.snapshot();
    let camera = pan_camera(*application_view(&snapshot).camera(), [8.0, -4.0]);
    let durable_edit_started_at = Instant::now();
    app.project_store_product_evidence.durable_edit_started_at = Some(durable_edit_started_at);
    let effect = app
        .apply_application_command(ApplicationCommand::SetCamera(camera), ctx)
        .map_err(|fault| format!("durable edit after initial Save was rejected: {fault:?}"))?;
    if effect != CommandEffect::Changed {
        app.project_store_product_evidence.durable_edit_started_at = None;
        return Err("durable camera edit after initial Save changed no state".to_owned());
    }
    let current_revision = match app.application.snapshot().workspace() {
        WorkspaceSnapshot::Bound { revision, .. } => *revision,
        WorkspaceSnapshot::Unbound { .. } => {
            return Err("durable edit left the project workspace unbound".to_owned());
        }
    };
    if current_revision == captured_revision {
        return Err("durable edit did not advance beyond the captured revision".to_owned());
    }
    Ok(CommandProgress::Done(json!({
        "path": path.display().to_string(),
        "captured_revision": project_revision_json(Some(captured_revision)),
        "current_revision_after_edit": project_revision_json(Some(current_revision)),
        "foreground_was_active_before_edit": true,
        "normal_reducer_service_path": true,
        "completion_polling_resumed_only_after_edit": true,
    })))
}

fn project_state_facts(app: &MiranteWorkbenchApp) -> ProductAutomationProjectStateFacts {
    let snapshot = app.application.snapshot();
    let (bound, dirty) = match snapshot.workspace() {
        WorkspaceSnapshot::Bound { dirty, .. } => (true, *dirty),
        WorkspaceSnapshot::Unbound { .. } => (false, false),
    };
    let status = app.project_store.as_ref().map(|service| service.status());
    let lifecycle = status
        .as_ref()
        .map(|status| status.lifecycle())
        .or_else(|| {
            app.project_store_product_evidence
                .close_result
                .as_ref()
                .map(|_| ProjectStoreLifecycle::Closed)
        });
    ProductAutomationProjectStateFacts {
        bound,
        dirty,
        lifecycle,
        can_save: app
            .project_store
            .as_ref()
            .is_some_and(|service| service.can_save()),
        can_save_as: app
            .project_store
            .as_ref()
            .is_some_and(|service| service.can_save_as()),
        manual: status
            .as_ref()
            .is_some_and(|status| status.current_manual().is_some()),
        autosave: status
            .as_ref()
            .is_some_and(|status| status.current_autosave().is_some()),
    }
}

fn project_state_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = app.application.snapshot();
    let (current_revision, saved_revision, revision_high_water_sequence, retained_history_entries) =
        match snapshot.workspace() {
            WorkspaceSnapshot::Bound {
                revision,
                revision_high_water,
                saved_revision,
                retained_history_entries,
                ..
            } => (
                Some(*revision),
                *saved_revision,
                Some(revision_high_water.sequence()),
                Some(*retained_history_entries),
            ),
            WorkspaceSnapshot::Unbound { .. } => (None, None, None, None),
        };
    let status = app.project_store.as_ref().map(|service| service.status());
    let facts = project_state_facts(app);
    json!({
        "bound": facts.bound,
        "dirty": facts.dirty,
        "current_revision": project_revision_json(current_revision),
        "saved_revision": project_revision_json(saved_revision),
        "revision_high_water_sequence": revision_high_water_sequence,
        "retained_history_entries": retained_history_entries,
        "history_entry_high_water_sequence": revision_high_water_sequence,
        "history_entry_high_water_derivation": "one_BoundWorkspace_history_push_per_allocated_durable_revision",
        "lifecycle": facts.lifecycle.map(project_store_lifecycle_name),
        "can_save": facts.can_save,
        "can_save_as": facts.can_save_as,
        "manual": facts.manual,
        "autosave": facts.autosave,
        "current_manual": status
            .as_ref()
            .and_then(|status| status.current_manual())
            .map(|generation| generation.to_string()),
        "current_autosave": status
            .as_ref()
            .and_then(|status| status.current_autosave())
            .map(|generation| generation.to_string()),
    })
}

fn project_store_evidence_json(app: &MiranteWorkbenchApp) -> Value {
    let evidence = &app.project_store_product_evidence;
    json!({
        "initial_save_captured_revision": project_revision_json(
            evidence.initial_save_captured_revision,
        ),
        "latest_autosave_captured_revision": project_revision_json(
            evidence.latest_autosave_captured_revision,
        ),
        "autosave_elapsed_from_durable_edit_ms":
            evidence.autosave_elapsed_from_durable_edit_ms,
        "autosave_wait_mode": "scheduled_deadline_no_busy_poll",
        "close_result": recorded_result_json(evidence.close_result.as_ref(), "fault"),
        "actor_join": recorded_result_json(evidence.actor_join.as_ref(), "error"),
    })
}

fn external_kill_checkpoint_json(
    app: &MiranteWorkbenchApp,
    stage: &str,
    requested_mapped_client_pixels: Option<(u32, u32)>,
) -> Value {
    json!({
        "schema": "mirante4d-product-external-kill-checkpoint",
        "schema_version": 1,
        "stage": stage,
        "written_at_epoch_ms": epoch_ms(),
        "viewport_evidence": {
            "requested_mapped_client_pixels": requested_mapped_client_pixels
                .map(|(width, height)| json!({ "width": width, "height": height })),
        },
        "project_state": project_state_json(app),
        "project_evidence": project_store_evidence_json(app),
    })
}

fn recorded_result_json(
    result: Option<&crate::ProjectStoreRecordedResult>,
    failure_key: &'static str,
) -> Value {
    let Some(result) = result else {
        return Value::Null;
    };
    let (status, failure) = match result {
        crate::ProjectStoreRecordedResult::Succeeded => ("succeeded", Value::Null),
        crate::ProjectStoreRecordedResult::Failed(reason) => {
            ("failed", Value::String(reason.clone()))
        }
    };
    let mut object = serde_json::Map::new();
    object.insert("status".to_owned(), Value::String(status.to_owned()));
    object.insert(failure_key.to_owned(), failure);
    Value::Object(object)
}

fn project_revision_json(revision: Option<ProjectRevisionId>) -> Value {
    revision.map_or(Value::Null, |revision| {
        json!({
            "project_id": revision.project_id().to_string(),
            "sequence": revision.sequence(),
        })
    })
}

fn project_store_lifecycle(
    lifecycle: ProductAutomationProjectStoreLifecycle,
) -> ProjectStoreLifecycle {
    match lifecycle {
        ProductAutomationProjectStoreLifecycle::Established => ProjectStoreLifecycle::Established,
        ProductAutomationProjectStoreLifecycle::RecoverySelected => {
            ProjectStoreLifecycle::RecoverySelected
        }
    }
}

fn project_store_lifecycle_name(lifecycle: ProjectStoreLifecycle) -> &'static str {
    match lifecycle {
        ProjectStoreLifecycle::Unbound => "unbound",
        ProjectStoreLifecycle::Provisional => "provisional",
        ProjectStoreLifecycle::Established => "established",
        ProjectStoreLifecycle::RecoveryOnly => "recovery_only",
        ProjectStoreLifecycle::RecoverySelected => "recovery_selected",
        ProjectStoreLifecycle::Closing => "closing",
        ProjectStoreLifecycle::Closed => "closed",
    }
}

fn write_synced_json_no_replace(path: &Path, value: &Value) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "external-kill checkpoint already exists: {}",
            path.display()
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create checkpoint directory {}: {error}",
            parent.display()
        )
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "checkpoint path has no UTF-8 file name".to_owned())?;
    let stage_path = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("failed to serialize external-kill checkpoint: {error}"))?;
    bytes.push(b'\n');
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stage_path)
            .map_err(|error| {
                format!(
                    "failed to create checkpoint stage {}: {error}",
                    stage_path.display()
                )
            })?;
        file.write_all(&bytes).map_err(|error| {
            format!(
                "failed to write checkpoint stage {}: {error}",
                stage_path.display()
            )
        })?;
        file.sync_all().map_err(|error| {
            format!(
                "failed to sync checkpoint stage {}: {error}",
                stage_path.display()
            )
        })?;
        drop(file);
        fs::rename(&stage_path, path)
            .map_err(|error| format!("failed to publish checkpoint {}: {error}", path.display()))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                format!(
                    "failed to sync checkpoint directory {}: {error}",
                    parent.display()
                )
            })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&stage_path);
    }
    write_result
}

fn normalize_path(path: &std::path::Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
