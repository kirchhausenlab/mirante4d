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
    OperationToken, PackageIntegrityAuditSnapshot, PresentationSlot,
    ProjectStoreApplicationService, ProjectStoreLifecycle, RenderGestureKind, RenderIntentBase,
    RenderIntentSample, RenderIntentTarget, WorkspaceSnapshot,
    import_workflow::{
        ImportChannelSourceKind, ImportCommand, ImportProgressSnapshot, ImportReviewDraft,
        ImportWorkflowSnapshot,
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
use mirante4d_import_pipeline::{ImportReceipt, ImportStatistics, TiffChannelSource, TiffSource};
use mirante4d_project_model::{LayerViewState, ProjectRevisionId};
use mirante4d_render_api::{RenderExtent, VolumePickQuery};
use mirante4d_storage::ScientificPublicationTransferEvidence;
use mirante4d_ui_egui::{RenderIntentInteraction, ViewerPickPurpose, ViewerPickRequest};
use rustix::time::{ClockId, clock_gettime};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    CoordinatedPresentationGroup, DVR_DENSITY_SCALE_MAX, DVR_DENSITY_SCALE_MIN,
    DisplayedFrameFreshness, FrameCompleteness, MiranteWorkbenchApp, application_view,
    import_worker_service::ImportWorkerTimingOrigin, viewer_layout::PanelId,
};

mod capture;
mod diagnostics;
mod model;
mod progress;

pub(crate) use capture::product_target_capture;
use capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, capture_color_image,
    current_display_image_stats, sanitize_artifact_label, write_color_image_ppm,
};
use diagnostics::{
    dataset_runtime_diagnostics_json, gpu_adapter_diagnostics_json,
    local_dataset_source_diagnostics_json,
};
use model::*;
use progress::ProductAutomationProgressPublisher;

const ENABLE_AUTOMATION_ENV: &str = "MIRANTE4D_ENABLE_AUTOMATION";
const AUTOMATION_SCRIPT_ENV: &str = "MIRANTE4D_AUTOMATION_SCRIPT";
const AUTOMATION_REPORT_ENV: &str = "MIRANTE4D_AUTOMATION_REPORT";
const AUTOMATION_PICK_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_CAPTURE_TIMEOUT: Duration = Duration::from_secs(30);
const AUTOMATION_DATASET_SWITCH_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_TEMPORAL_OBSERVATIONS: usize = 4_096;

fn sequence_sample_target_ns(sample: u32, samples: u32, duration_ms: u64) -> u64 {
    if samples == 0 {
        return 0;
    }
    let numerator = u128::from(duration_ms)
        .saturating_mul(1_000_000)
        .saturating_mul(u128::from(sample.saturating_add(1)));
    u64::try_from(numerator / u128::from(samples)).unwrap_or(u64::MAX)
}

fn egui_deadline_repaint_delay(ctx: &egui::Context, remaining: Duration) -> Duration {
    let predicted_frame_time = ctx
        .input(|input| Duration::try_from_secs_f32(input.predicted_dt).unwrap_or(Duration::ZERO));
    // egui subtracts one predicted frame from every nonzero delayed repaint
    // request before handing it to eframe. Add that frame back so `remaining`
    // is an absolute input deadline measured from this UI turn rather than an
    // instruction to keep the FIFO surface continuously full.
    remaining.saturating_add(predicted_frame_time)
}

fn observed_native_viewport_pixels(ctx: &egui::Context) -> Option<[u32; 2]> {
    let pixels_per_point = ctx.pixels_per_point();
    let size_points = ctx.input(|input| input.viewport_rect().size());
    let to_pixels = |points: f32| {
        let pixels = (points * pixels_per_point).round();
        (pixels.is_finite() && pixels > 0.0 && pixels <= u32::MAX as f32).then_some(pixels as u32)
    };
    Some([to_pixels(size_points.x)?, to_pixels(size_points.y)?])
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
    app.render_coordination
        .surface(panel.presentation_slot())
        .presented_frame()
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
            let target = panel.presentation_slot();
            let presented = app.render_coordination.surface(target).presented_frame();
            let pending = app
                .native_presentation
                .product_gpu
                .as_ref()
                .and_then(|product| {
                    product
                        .pending_validation_captures
                        .iter()
                        .find(|capture| capture.ticket.target() == target)
                });
            let completed = app
                .native_presentation
                .product_gpu
                .as_ref()
                .and_then(|product| product.completed_validation_capture(target));
            format!(
                "{panel:?}:presented={:?},pending={:?},completed={:?}",
                presented.map(|frame| frame.frame()),
                pending.map(|capture| capture.frame.frame()),
                completed.map(|capture| capture.frame.frame()),
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn assertion_capture_panels(condition: &ProductAutomationAssertCondition) -> Vec<PanelId> {
    match condition {
        ProductAutomationAssertCondition::NonblankPanel { target } => {
            vec![PanelId::from(*target)]
        }
        ProductAutomationAssertCondition::FourPanelImagesDistinct { .. } => {
            vec![PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz]
        }
        _ => Vec::new(),
    }
}
const AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
const AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
const AUTOMATION_SCHEMA_VERSION: u32 = 10;
const AUTOMATION_REPORT_SCHEMA_VERSION: u32 = 9;

fn dispatch_application_command(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    command: ApplicationCommand,
) -> Result<CommandEffect, String> {
    app.apply_application_command(command, ctx)
        .map_err(|fault| format!("application command was rejected: {fault:?}"))
}

fn dispatch_render_intent_sample(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    sample: RenderIntentSample,
) -> Result<(), String> {
    app.apply_render_intent_interaction(RenderIntentInteraction::Sample(sample), ctx)
}

fn finish_render_intent(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    target: RenderIntentTarget,
) -> Result<(), String> {
    app.apply_render_intent_interaction(RenderIntentInteraction::Finish(target), ctx)
}

fn effective_interaction_camera(app: &MiranteWorkbenchApp) -> CameraView {
    let snapshot = app.application.snapshot();
    let base = RenderIntentBase::from_snapshot(&snapshot);
    app.render_intent_mailbox
        .effective_camera(base, *application_view(&snapshot).camera())
}

fn effective_interaction_cross_section(app: &MiranteWorkbenchApp) -> CrossSectionView {
    let snapshot = app.application.snapshot();
    let base = RenderIntentBase::from_snapshot(&snapshot);
    app.render_intent_mailbox
        .effective_cross_section(base, *application_view(&snapshot).cross_section())
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
    if app.render_coordination.frame_fidelity.three_d_preview {
        // A provisional preview is intentionally not exact pick authority.
        // Automation follows the same product route as the UI and waits for
        // the atomic exact swap instead of relabelling coarse data as exact.
        return Ok(None);
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
    if !app
        .native_presentation
        .initial_render_pipeline_is_ready()
        .unwrap_or(false)
    {
        return false;
    }
    let base = RenderIntentBase::from_snapshot(snapshot);
    let demand_currentness = app.visible_demand_plan_currentness();
    let generation = app.render_coordination.display_generation();
    let three_d_target_presented =
        app.dataset
            .current_target_layer_scales()
            .is_some_and(|target| {
                if target.is_empty() {
                    app.render_coordination.frame_fidelity.backend == crate::RenderBackend::Empty
                        && app
                            .render_coordination
                            .surface(PresentationSlot::ThreeD)
                            .layer_presentations()
                            .is_empty()
                } else {
                    app.render_coordination
                        .surface(PresentationSlot::ThreeD)
                        .presented_frame()
                        .is_some_and(|frame| {
                            crate::display_refresh::presented_layer_scales_match_target(
                                frame.progress().coverage(),
                                target,
                            )
                        })
                }
            });
    if generation.current_presentation_generation != Some(generation.input_generation)
        || app.render_intent_mailbox.active_target(base).is_some()
        || app.resident_cross_section_coverage.is_some()
        || !demand_currentness.current_3d
        || app.dataset.staging_current_refinement()
        || !three_d_target_presented
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
            let panel = PanelId::from_presentation_slot(slot);
            surface.display_current()
                && demand_currentness.cross_section(panel)
                && surface.cross_section_schedule().is_some_and(|schedule| {
                    matches!(
                        schedule.status,
                        crate::CrossSectionPanelScheduleStatus::Current
                            | crate::CrossSectionPanelScheduleStatus::Empty
                    )
                })
                && surface.layer_presentations().iter().all(|layer| {
                    layer.current && layer.available_requirements == layer.total_requirements
                })
        }),
    }
}

fn assert_empty_visible_presentation(
    app: &MiranteWorkbenchApp,
    snapshot: &ApplicationSnapshot,
) -> Result<(), String> {
    let view = application_view(snapshot);
    if view.layers().iter().any(|layer| layer.visible()) {
        return Err("empty presentation assertion still has a visible layer".to_owned());
    }
    if !app
        .dataset
        .current_target_layer_scales()
        .is_some_and(|scales| scales.is_empty())
    {
        return Err("empty presentation has no current explicit-empty target scale map".to_owned());
    }
    let fidelity = &app.render_coordination.frame_fidelity;
    if fidelity.backend != crate::RenderBackend::Empty
        || fidelity.completeness != FrameCompleteness::Complete
        || fidelity.display_freshness != DisplayedFrameFreshness::Current
        || fidelity.reason != crate::LodDecisionReason::NoVisibleData
        || fidelity.displayed_scale_level.is_some()
        || fidelity.last_failure_kind.is_some()
        || fidelity.last_capacity_error.is_some()
    {
        return Err(format!(
            "3D empty fidelity mismatch: backend={:?}, completeness={:?}, freshness={:?}, reason={:?}, scale={:?}, failure={:?}, error={:?}",
            fidelity.backend,
            fidelity.completeness,
            fidelity.display_freshness,
            fidelity.reason,
            fidelity.displayed_scale_level,
            fidelity.last_failure_kind,
            fidelity.last_capacity_error,
        ));
    }
    let targets: &[PresentationSlot] = match view.layout() {
        ViewerLayout::Single3d => &PresentationSlot::ALL[..1],
        ViewerLayout::FourPanel => &PresentationSlot::ALL,
    };
    for target in targets.iter().copied() {
        let surface = app.render_coordination.surface(target);
        if surface.presented_frame().is_some()
            || !surface.layer_presentations().is_empty()
            || app
                .native_presentation
                .texture_binding_identity(target)
                .is_some()
        {
            return Err(format!(
                "{target:?} retained frame, layer, or native texture authority after empty publication"
            ));
        }
        if target.is_cross_section()
            && (!surface.display_current()
                || !surface.cross_section_schedule().is_some_and(|schedule| {
                    schedule.status == crate::CrossSectionPanelScheduleStatus::Empty
                        && schedule.reason == crate::CrossSectionPanelScheduleReason::NoSelectedData
                }))
        {
            return Err(format!(
                "{target:?} is not a current explicit-empty linked surface"
            ));
        }
    }
    if app.render_attempt.wake() != crate::display_refresh::RenderWake::None {
        return Err("empty presentation retained immediate renderer/composition work".to_owned());
    }
    Ok(())
}

fn visible_product_panels(snapshot: &ApplicationSnapshot) -> Vec<PanelId> {
    match application_view(snapshot).layout() {
        ViewerLayout::Single3d => vec![PanelId::ThreeD],
        ViewerLayout::FourPanel => {
            vec![PanelId::ThreeD, PanelId::Xy, PanelId::Xz, PanelId::Yz]
        }
    }
}

const fn automation_runtime_is_idle(
    background_work_active: bool,
    camera_demand_planning_active: bool,
    prepared_demand_install_pending: bool,
) -> bool {
    !background_work_active && !camera_demand_planning_active && !prepared_demand_install_pending
}

pub(super) fn cancel_active_package_integrity_audit(
    app: &mut MiranteWorkbenchApp,
) -> Result<Value, String> {
    app.pump_application_services();
    let snapshot = app.application.snapshot();
    let operation_id = match snapshot.source().integrity_audit() {
        PackageIntegrityAuditSnapshot::Running { operation_id, .. } => Some(*operation_id),
        PackageIntegrityAuditSnapshot::NotRun
        | PackageIntegrityAuditSnapshot::SelfConsistent(_)
        | PackageIntegrityAuditSnapshot::Failed(_)
        | PackageIntegrityAuditSnapshot::Cancelled => None,
    };
    if let Some(operation_id) = operation_id {
        app.application
            .dispatch(ApplicationCommand::CancelOperation(operation_id))
            .map_err(|fault| {
                format!("active package-integrity-audit cancellation was rejected: {fault:?}")
            })?;
        app.pump_application_services();
    }
    Ok(json!({
        "active_operation_observed": operation_id.is_some(),
        "cancellation_requested": operation_id.is_some(),
        "automatic_request_dispatched": false,
    }))
}

pub(super) fn package_integrity_audit_inactive(app: &MiranteWorkbenchApp) -> bool {
    let snapshot = app.application.snapshot();
    !matches!(
        snapshot.source().integrity_audit(),
        PackageIntegrityAuditSnapshot::Running { .. }
    ) && app
        .package_integrity_audit_service
        .as_ref()
        .is_some_and(|service| service.active_token().is_none())
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

fn assert_native_pick_evidence(
    evidence: &Value,
    expected_policy: ProductAutomationPickPolicy,
) -> Result<(), String> {
    if evidence.get("native_gpu_pick").and_then(Value::as_bool) != Some(true) {
        return Err("pick evidence did not come from the native GPU pick path".to_owned());
    }
    if evidence.get("placeholder_sampled").and_then(Value::as_bool) != Some(false) {
        return Err("pick evidence sampled a placeholder".to_owned());
    }
    if evidence.get("status").and_then(Value::as_str) != Some("sampled")
        || !matches!(
            evidence.get("kind").and_then(Value::as_str),
            Some("voxel" | "interpolated_sample")
        )
    {
        return Err("pick evidence did not contain a nonempty sampled hit".to_owned());
    }
    if evidence.get("completeness").and_then(Value::as_str) != Some("exact") {
        return Err("pick evidence was not exact".to_owned());
    }
    let observed_policy = evidence.get("policy").and_then(Value::as_str);
    if observed_policy != Some(expected_policy.name()) {
        return Err(format!(
            "pick policy was {:?}, expected {}",
            observed_policy,
            expected_policy.name()
        ));
    }
    let value = evidence
        .get("value")
        .and_then(Value::as_object)
        .ok_or_else(|| "pick evidence has no sampled intensity value".to_owned())?;
    if !matches!(
        value.get("dtype").and_then(Value::as_str),
        Some("uint8" | "uint16" | "float32")
    ) || !value
        .get("value")
        .and_then(Value::as_f64)
        .is_some_and(f64::is_finite)
    {
        return Err("pick evidence has no finite typed intensity value".to_owned());
    }
    for (field, expected_len) in [("world_position", 3), ("render_pixel", 2)] {
        let coordinates = evidence
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("pick evidence has no {field} coordinates"))?;
        if coordinates.len() != expected_len
            || !coordinates
                .iter()
                .all(|coordinate| coordinate.as_f64().is_some_and(f64::is_finite))
        {
            return Err(format!(
                "pick evidence {field} coordinates are incomplete or non-finite"
            ));
        }
    }
    Ok(())
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
    active_playback_cadence: Option<ActivePlaybackCadenceObservation>,
    automation_frame_nr: u64,
    started_at_epoch_ms: u128,
    started_at: Instant,
    started_process_cpu_time_ns: Option<u64>,
    events: Vec<ProductAutomationEvent>,
    diagnostics: Vec<Value>,
    artifacts: Vec<ProductAutomationArtifact>,
    temporal_observations: Vec<Value>,
    temporal_transition_count: u64,
    temporal_contract_transition_count: u64,
    last_coherent_temporal_timepoint: Option<TimeIndex>,
    active_temporal_contract: Option<(u64, Vec<(u32, u32)>)>,
    temporal_violation: Option<String>,
    active_temporal_wait: Option<(usize, u64)>,
    last_input_sequence_temporal_transitions: Option<u64>,
    last_input_sequence_cancelled_gestures: Option<u64>,
    last_input_sequence_temporal_distribution: Option<CompletedInputTemporalDistribution>,
    last_stationary_playback_cadence: Option<CompletedInputTemporalDistribution>,
    last_input_sequence_cadence_comparison: Option<PlaybackCadenceComparison>,
    active_playback_stop_trace: Option<ActivePlaybackStopTrace>,
    temporal_capture_baselines: Vec<TemporalCaptureBaseline>,
    limit_observations: ProductAutomationLimitObservations,
    render_target_override: Option<RenderExtent>,
    requested_mapped_client_pixels: Option<(u32, u32)>,
    projected_import_stages: Vec<&'static str>,
    maximum_projected_import_elapsed_ms: u64,
    active_import_pre_start_origin: Option<ImportPreStartOrigin>,
    completed_import_pre_start_measurement: Option<ImportPreStartMeasurement>,
    active_import_timing_origin: Option<ImportWorkerTimingOrigin>,
    active_import_integrity_audit_diagnostics_origin:
        Option<crate::package_integrity_audit_service::PackageIntegrityAuditDiagnostics>,
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

#[derive(Clone, Debug)]
struct ActiveInputSequence {
    command_index: usize,
    started_at: Instant,
    next_sample: u32,
    samples: u32,
    duration_ms: u64,
    required_group: CoordinatedPresentationGroup,
    origin_generation: u64,
    origin_durable_commits: u64,
    origin_render_intent_revision: u64,
    origin_render_intent_samples: u64,
    origin_render_intent_coalesced_samples: u64,
    origin_render_intent_finished_gestures: u64,
    origin_render_intent_cancelled_gestures: u64,
    origin_temporal_transitions: u64,
    temporal_transitions_at_last_dispatch: Option<u64>,
    temporal_transition_elapsed_ns: Vec<u64>,
    dispatched_samples: u32,
    distinct_dispatch_updates: u32,
    first_dispatch_elapsed_ns: Option<u64>,
    last_dispatch_elapsed_ns: Option<u64>,
    last_evaluated_frame_nr: Option<u64>,
    last_dispatch_frame_nr: Option<u64>,
    maximum_dispatch_interval_ns: u64,
    maximum_dispatch_lateness_ns: u64,
    nonmonotonic_dispatches: u32,
    same_update_dispatches: u32,
    stationary_baseline: Option<CompletedInputTemporalDistribution>,
    requested_frame_period_ns: u64,
}

#[derive(Clone, Debug)]
struct CompletedInputTemporalDistribution {
    transition_elapsed_ns: Vec<u64>,
    first_half_transitions: u64,
    second_half_transitions: u64,
    maximum_gap_ns: u64,
    observation_window_ns: u64,
}

#[derive(Clone, Debug)]
struct ActivePlaybackCadenceObservation {
    command_index: usize,
    started_at: Instant,
    duration_ms: u64,
    origin_temporal_transitions: u64,
    transition_elapsed_ns: Vec<u64>,
}

#[derive(Clone, Debug)]
struct PlaybackCadenceComparison {
    baseline_transitions: u64,
    input_transitions: u64,
    minimum_input_transitions: u64,
    baseline_maximum_gap_ns: u64,
    input_maximum_gap_ns: u64,
    maximum_allowed_input_gap_ns: u64,
    requested_frame_period_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaybackStopLayerState {
    layer_ordinal: u32,
    displayed_scale_level: Option<u32>,
    mixed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaybackStopPanelState {
    panel: &'static str,
    timepoint: Option<TimeIndex>,
    frame_identity: Option<u64>,
    frame_completeness: Option<String>,
    available_requirements: Option<u64>,
    total_requirements: Option<u64>,
    cross_section_schedule: Option<String>,
    texture_binding: Option<(u64, u64)>,
    layers: Vec<PlaybackStopLayerState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaybackStopVisibleState {
    panels: Vec<PlaybackStopPanelState>,
}

#[derive(Clone, Debug)]
struct ActivePlaybackStopTrace {
    started_at: Instant,
    expected_timepoint: TimeIndex,
    playback_layer_scales: Vec<(u32, u32)>,
    states: Vec<(u64, PlaybackStopVisibleState)>,
    violation: Option<String>,
}

impl PlaybackCadenceComparison {
    const fn passes(&self) -> bool {
        self.input_transitions >= self.minimum_input_transitions
            && self.input_maximum_gap_ns <= self.maximum_allowed_input_gap_ns
    }
}

/// Same-duration interaction cadence floor for the mapped product oracle.
///
/// Count alone is deliberately not the pause detector: the oracle also
/// requires transitions in both halves of the gesture and rejects a maximum
/// gap beyond its frame-period/stationary-baseline bound. An 80% count floor
/// keeps the comparison sensitive to material throughput loss without making
/// a one-person product gate fail on the observed 20-versus-24/25 scheduling
/// variance that remained visually continuous on the designated workstation.
const fn minimum_interaction_cadence(baseline_transitions: u64) -> u64 {
    baseline_transitions.saturating_mul(4).saturating_add(4) / 5
}

fn completed_temporal_distribution(
    transition_elapsed_ns: &[u64],
    observation_window_ns: u64,
) -> CompletedInputTemporalDistribution {
    let transition_elapsed_ns = transition_elapsed_ns
        .iter()
        .copied()
        .filter(|elapsed_ns| *elapsed_ns <= observation_window_ns)
        .collect::<Vec<_>>();
    let half = observation_window_ns / 2;
    let first_half_transitions = u64::try_from(
        transition_elapsed_ns
            .iter()
            .filter(|elapsed_ns| **elapsed_ns <= half)
            .count(),
    )
    .unwrap_or(u64::MAX);
    let second_half_transitions = u64::try_from(
        transition_elapsed_ns
            .iter()
            .filter(|elapsed_ns| **elapsed_ns > half)
            .count(),
    )
    .unwrap_or(u64::MAX);
    let maximum_gap_ns = std::iter::once(0)
        .chain(transition_elapsed_ns.iter().copied())
        .chain(std::iter::once(observation_window_ns))
        .collect::<Vec<_>>()
        .windows(2)
        .map(|pair| pair[1].saturating_sub(pair[0]))
        .max()
        .unwrap_or(observation_window_ns);
    CompletedInputTemporalDistribution {
        transition_elapsed_ns,
        first_half_transitions,
        second_half_transitions,
        maximum_gap_ns,
        observation_window_ns,
    }
}

fn playback_stop_visible_state(
    app: &MiranteWorkbenchApp,
    playback_layer_scales: &[(u32, u32)],
) -> PlaybackStopVisibleState {
    let snapshot = app.application.snapshot();
    let panels = visible_product_panels(&snapshot)
        .into_iter()
        .map(|panel| {
            let slot = panel.presentation_slot();
            let surface = app.render_coordination.surface(slot);
            let presented = surface.presented_frame();
            PlaybackStopPanelState {
                panel: panel.label(),
                timepoint: presented.map(|frame| frame.timepoint()),
                frame_identity: presented.map(|frame| frame.frame().get()),
                frame_completeness: presented
                    .map(|frame| format!("{:?}", frame.progress().completeness())),
                available_requirements: presented
                    .map(|frame| frame.progress().coverage().available_requirements()),
                total_requirements: presented
                    .map(|frame| frame.progress().coverage().total_requirements()),
                cross_section_schedule: surface
                    .cross_section_schedule()
                    .map(|schedule| format!("{:?}/{:?}", schedule.status, schedule.reason)),
                texture_binding: app.native_presentation.texture_binding_identity(slot),
                layers: playback_layer_scales
                    .iter()
                    .map(|(layer_ordinal, _)| {
                        // Quality belongs to the immutable visible frame, not
                        // to the mutable demand/schedule diagnostics carried
                        // beside it. During a retained-front handoff those
                        // diagnostics may already describe the hidden
                        // successor while the predecessor texture is still
                        // authoritative.
                        let observed = presented.and_then(|frame| {
                            frame
                                .progress()
                                .coverage()
                                .layer_coverages()
                                .find(|coverage| coverage.layer().ordinal() == *layer_ordinal)
                        });
                        PlaybackStopLayerState {
                            layer_ordinal: *layer_ordinal,
                            displayed_scale_level: observed
                                .and_then(|coverage| coverage.scale())
                                .map(|scale| scale.get()),
                            mixed: observed.is_some_and(|coverage| coverage.is_mixed()),
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    PlaybackStopVisibleState { panels }
}

impl ActivePlaybackStopTrace {
    fn observe(&mut self, app: &MiranteWorkbenchApp) {
        let state = playback_stop_visible_state(app, &self.playback_layer_scales);
        if self.violation.is_none() {
            self.violation = self.validate_state(&state);
        }
        if self
            .states
            .last()
            .is_none_or(|(_, previous)| previous != &state)
        {
            self.states.push((
                u64::try_from(self.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                state,
            ));
        }
    }

    fn validate_state(&self, state: &PlaybackStopVisibleState) -> Option<String> {
        if state.panels.is_empty() {
            return Some("playback Stop exposed no visible target front".to_owned());
        }
        let previous = self.states.last().map(|(_, state)| state);
        for panel in &state.panels {
            if panel.timepoint != Some(self.expected_timepoint) {
                return Some(format!(
                    "playback Stop exposed {} at timepoint {:?}, expected t{}",
                    panel.panel,
                    panel.timepoint.map(TimeIndex::get),
                    self.expected_timepoint.get()
                ));
            }
            for layer in &panel.layers {
                let expected = self
                    .playback_layer_scales
                    .iter()
                    .find_map(|(ordinal, scale)| {
                        (*ordinal == layer.layer_ordinal).then_some(*scale)
                    })
                    .expect("stop trace layers come from the fixed playback scale map");
                let Some(displayed) = layer.displayed_scale_level else {
                    return Some(format!(
                        "playback Stop exposed {} layer {} without a displayed scale",
                        panel.panel, layer.layer_ordinal
                    ));
                };
                if layer.mixed {
                    return Some(format!(
                        "playback Stop exposed a mixed-scale {} layer {}",
                        panel.panel, layer.layer_ordinal
                    ));
                }
                if displayed > expected {
                    return Some(format!(
                        "playback Stop downgraded {} layer {} from s{} to s{}",
                        panel.panel, layer.layer_ordinal, expected, displayed
                    ));
                }
                if let Some(previous_displayed) = previous
                    .and_then(|previous| {
                        previous
                            .panels
                            .iter()
                            .find(|candidate| candidate.panel == panel.panel)
                    })
                    .and_then(|previous| {
                        previous
                            .layers
                            .iter()
                            .find(|candidate| candidate.layer_ordinal == layer.layer_ordinal)
                    })
                    .and_then(|previous| previous.displayed_scale_level)
                    && displayed > previous_displayed
                {
                    return Some(format!(
                        "playback Stop regressed {} layer {} from s{} back to s{}",
                        panel.panel, layer.layer_ordinal, previous_displayed, displayed
                    ));
                }
            }
        }
        None
    }

    fn json(&self) -> Value {
        json!({
            "clock": "std_instant_monotonic",
            "expected_time_index": self.expected_timepoint.get(),
            "playback_layer_scales": self.playback_layer_scales.iter().map(|(layer_ordinal, scale_level)| json!({
                "layer_ordinal": layer_ordinal,
                "scale_level": scale_level,
            })).collect::<Vec<_>>(),
            "violation": self.violation,
            "states": self.states.iter().map(|(elapsed_ns, state)| json!({
                "elapsed_ns": elapsed_ns,
                "panels": state.panels.iter().map(|panel| json!({
                    "panel": panel.panel,
                    "time_index": panel.timepoint.map(TimeIndex::get),
                    "frame_identity": panel.frame_identity,
                    "frame_completeness": panel.frame_completeness.as_deref(),
                    "available_requirements": panel.available_requirements,
                    "total_requirements": panel.total_requirements,
                    "cross_section_schedule": panel.cross_section_schedule.as_deref(),
                    "texture_binding": panel.texture_binding.map(|(device_generation, texture_revision)| json!({
                        "device_generation": device_generation,
                        "texture_revision": texture_revision,
                    })),
                    "layers": panel.layers.iter().map(|layer| json!({
                        "layer_ordinal": layer.layer_ordinal,
                        "displayed_scale_level": layer.displayed_scale_level,
                        "mixed": layer.mixed,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

#[derive(Clone, Debug)]
struct TemporalCaptureBaseline {
    target: ProductAutomationPresentationTarget,
    timepoint: TimeIndex,
    rgba8: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputSequenceStep {
    Wait(Duration),
    Dispatch(u32),
    Finish,
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
    package_integrity_audit_started_runs: u64,
    package_integrity_audit_progress_updates: u64,
    package_integrity_audit_cancelled_runs: u64,
    package_integrity_audit_failed_runs: u64,
    package_integrity_audit_completed_runs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportIntegrityAuditEvidenceSnapshot {
    package_integrity_audit_started_runs: u64,
    package_integrity_audit_progress_updates: u64,
    package_integrity_audit_cancelled_runs: u64,
    package_integrity_audit_failed_runs: u64,
    package_integrity_audit_completed_runs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportedOpenReadyOutcome {
    measurement: ImportPublicationToOpenReadyMeasurement,
    evidence: ImportPublicationEvidenceSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportedOpenReadyReadiness {
    selected_matches: bool,
    content_id_computed_during_import: bool,
    import_idle: bool,
    problem_absent: bool,
}

impl ImportedOpenReadyReadiness {
    const fn condition_met(self) -> bool {
        self.selected_matches
            && self.content_id_computed_during_import
            && self.import_idle
            && self.problem_absent
    }
}

fn imported_open_ready_readiness(
    app: &MiranteWorkbenchApp,
    snapshot: &ApplicationSnapshot,
    path: &Path,
) -> ImportedOpenReadyReadiness {
    ImportedOpenReadyReadiness {
        selected_matches: normalize_path(app.dataset.selected_path()) == normalize_path(path),
        content_id_computed_during_import: snapshot.source().content_address_origin()
            == mirante4d_application::ContentAddressOrigin::ComputedDuringImport,
        import_idle: !app.import.workers.status().is_active(),
        problem_absent: app.import.problem.is_none(),
    }
}

struct ImportedOpenReadyCommitState<'a, Origin, AuditOrigin, Outcome> {
    active_origin: &'a mut Option<Origin>,
    active_audit_origin: &'a mut Option<AuditOrigin>,
    completed_primary: &'a mut Option<ImportPrimaryMeasurement>,
    open_ready_outcome: &'a mut Option<Outcome>,
}

impl<Origin, AuditOrigin, Outcome> ImportedOpenReadyCommitState<'_, Origin, AuditOrigin, Outcome> {
    fn commit(self, primary: ImportPrimaryMeasurement, outcome: Outcome) {
        *self.open_ready_outcome = Some(outcome);
        *self.completed_primary = Some(primary);
        *self.active_audit_origin = None;
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
        // This remains stable across egui multipass re-entry and advances
        // exactly once per Context::run.
        automation.automation_frame_nr = ctx.cumulative_frame_nr();
        let status = match automation.publish_progress_if_due() {
            Ok(()) => automation.step(app, ctx),
            Err(reason) => AutomationStatus::Failed(reason),
        };
        // Automation submits through the same one-pending/one-latest native
        // pick queue as interactive UI input. Pump a newly queued request now
        // so the same UI turn can observe it without introducing another path;
        // the normal post-UI pump is then an idle second observation.
        app.pump_viewer_pick(ctx);
        match status {
            AutomationStatus::Continue => {
                ctx.request_repaint();
            }
            AutomationStatus::Waiting { repaint_after } => {
                if automation.active_input_sequence.is_some() {
                    // Real pointer input wakes the event loop independently.
                    // Model that fixed 60 Hz source with an absolute deadline
                    // instead of saturating eframe's FIFO presentation loop.
                    if let Some(remaining) = repaint_after {
                        ctx.request_repaint_after(egui_deadline_repaint_delay(ctx, remaining));
                    }
                } else if let Some(delay) = automation.progress_repaint_after(repaint_after) {
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
                automation.close_active_input_sequence_window(app);
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
            active_playback_cadence: None,
            automation_frame_nr: 0,
            started_at_epoch_ms: epoch_ms(),
            started_at: Instant::now(),
            started_process_cpu_time_ns: checked_process_cpu_time_ns(),
            events: Vec::new(),
            diagnostics: Vec::new(),
            artifacts: Vec::new(),
            temporal_observations: Vec::new(),
            temporal_transition_count: 0,
            temporal_contract_transition_count: 0,
            last_coherent_temporal_timepoint: None,
            active_temporal_contract: None,
            temporal_violation: None,
            active_temporal_wait: None,
            last_input_sequence_temporal_transitions: None,
            last_input_sequence_cancelled_gestures: None,
            last_input_sequence_temporal_distribution: None,
            last_stationary_playback_cadence: None,
            last_input_sequence_cadence_comparison: None,
            active_playback_stop_trace: None,
            temporal_capture_baselines: Vec::new(),
            limit_observations: ProductAutomationLimitObservations::default(),
            render_target_override: None,
            requested_mapped_client_pixels: None,
            projected_import_stages: Vec::new(),
            maximum_projected_import_elapsed_ms: 0,
            active_import_pre_start_origin: None,
            completed_import_pre_start_measurement: None,
            active_import_timing_origin: None,
            active_import_integrity_audit_diagnostics_origin: None,
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

    fn begin_playback_stop_trace(&mut self, app: &MiranteWorkbenchApp) -> Result<(), String> {
        let contract = app.playback_session.contract().ok_or_else(|| {
            "playback Stop evidence requires an admitted session contract".to_owned()
        })?;
        let expected_timepoint = app.playback_session.presented_timepoint().ok_or_else(|| {
            "playback Stop evidence requires one coherently presented session cursor".to_owned()
        })?;
        let playback_layer_scales = contract
            .layer_scales()
            .iter()
            .map(|(layer, scale)| (layer.ordinal(), scale.get()))
            .collect::<Vec<_>>();
        let mut trace = ActivePlaybackStopTrace {
            started_at: Instant::now(),
            expected_timepoint,
            playback_layer_scales,
            states: Vec::new(),
            violation: None,
        };
        trace.observe(app);
        self.active_playback_stop_trace = Some(trace);
        Ok(())
    }

    fn observe_playback_stop_trace(&mut self, app: &MiranteWorkbenchApp) {
        if let Some(trace) = self.active_playback_stop_trace.as_mut() {
            trace.observe(app);
        }
    }

    fn close_active_input_sequence_window(&mut self, app: &mut MiranteWorkbenchApp) {
        if self.active_input_sequence.take().is_none() {
            return;
        }
        let now_ns = app.display_instrumentation_now_ns();
        app.render_coordination
            .end_active_publication_window(now_ns);
    }

    fn input_sequence_step(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        samples: u32,
        duration_ms: u64,
        required_group: CoordinatedPresentationGroup,
    ) -> InputSequenceStep {
        if self.active_input_sequence.is_none() {
            self.last_input_sequence_temporal_transitions = None;
            self.last_input_sequence_cancelled_gestures = None;
            self.last_input_sequence_temporal_distribution = None;
            self.last_input_sequence_cadence_comparison = None;
            let generation = app.render_coordination.display_generation();
            let mailbox = app.render_intent_mailbox.snapshot();
            let requested_fps = app.application.snapshot().transient().playback_fps().get();
            let requested_frame_period_ns = 1_000_000_000_u64 / u64::from(requested_fps.max(1));
            let stationary_baseline = self.last_stationary_playback_cadence.take();
            let now_ns = app.display_instrumentation_now_ns();
            app.render_coordination
                .begin_active_publication_window(now_ns, required_group);
            self.active_input_sequence = Some(ActiveInputSequence {
                command_index: self.command_index,
                started_at: Instant::now(),
                next_sample: 0,
                samples,
                duration_ms,
                required_group,
                origin_generation: generation.input_generation,
                origin_durable_commits: generation.durable_gesture_commits,
                origin_render_intent_revision: mailbox.latest_revision.get(),
                origin_render_intent_samples: mailbox.raw_samples,
                origin_render_intent_coalesced_samples: mailbox.coalesced_samples,
                origin_render_intent_finished_gestures: mailbox.finished_gestures,
                origin_render_intent_cancelled_gestures: mailbox.cancelled_gestures,
                origin_temporal_transitions: self.temporal_transition_count,
                temporal_transitions_at_last_dispatch: None,
                temporal_transition_elapsed_ns: Vec::new(),
                dispatched_samples: 0,
                distinct_dispatch_updates: 0,
                first_dispatch_elapsed_ns: None,
                last_dispatch_elapsed_ns: None,
                last_evaluated_frame_nr: None,
                last_dispatch_frame_nr: None,
                maximum_dispatch_interval_ns: 0,
                maximum_dispatch_lateness_ns: 0,
                nonmonotonic_dispatches: 0,
                same_update_dispatches: 0,
                stationary_baseline,
                requested_frame_period_ns,
            });
        }
        let frame_nr = self.automation_frame_nr;
        let sequence = self
            .active_input_sequence
            .as_mut()
            .expect("an initialized input sequence retains active state");
        debug_assert_eq!(sequence.command_index, self.command_index);
        debug_assert_eq!(sequence.samples, samples);
        debug_assert_eq!(sequence.duration_ms, duration_ms);
        debug_assert_eq!(sequence.required_group, required_group);
        if sequence.last_evaluated_frame_nr == Some(frame_nr) {
            return InputSequenceStep::Wait(Duration::ZERO);
        }
        sequence.last_evaluated_frame_nr = Some(frame_nr);
        if sequence.next_sample == sequence.samples {
            if sequence.last_dispatch_frame_nr == Some(frame_nr) {
                return InputSequenceStep::Wait(Duration::ZERO);
            }
            // A real pointer release is delivered independently of renderer
            // settlement. Holding the synthetic gesture open until its final
            // pixels became current made playback advancement and input
            // completion circular, and did not model product input. Scripts
            // that need settled pixels own an explicit subsequent wait.
            return InputSequenceStep::Finish;
        }
        let target_ns =
            sequence_sample_target_ns(sequence.next_sample, sequence.samples, sequence.duration_ms);
        let elapsed_ns =
            u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if elapsed_ns < target_ns {
            InputSequenceStep::Wait(Duration::from_nanos(target_ns - elapsed_ns))
        } else {
            let sample = sequence.next_sample;
            sequence.dispatched_samples = sequence.dispatched_samples.saturating_add(1);
            sequence.first_dispatch_elapsed_ns.get_or_insert(elapsed_ns);
            if let Some(previous_elapsed_ns) = sequence.last_dispatch_elapsed_ns {
                if elapsed_ns < previous_elapsed_ns {
                    sequence.nonmonotonic_dispatches =
                        sequence.nonmonotonic_dispatches.saturating_add(1);
                }
                sequence.maximum_dispatch_interval_ns = sequence
                    .maximum_dispatch_interval_ns
                    .max(elapsed_ns.saturating_sub(previous_elapsed_ns));
            }
            if sequence.last_dispatch_frame_nr != Some(frame_nr) {
                sequence.distinct_dispatch_updates =
                    sequence.distinct_dispatch_updates.saturating_add(1);
            } else {
                sequence.same_update_dispatches = sequence.same_update_dispatches.saturating_add(1);
            }
            sequence.last_dispatch_elapsed_ns = Some(elapsed_ns);
            sequence.last_dispatch_frame_nr = Some(frame_nr);
            sequence.maximum_dispatch_lateness_ns = sequence
                .maximum_dispatch_lateness_ns
                .max(elapsed_ns.saturating_sub(target_ns));
            InputSequenceStep::Dispatch(sample)
        }
    }

    fn complete_input_sequence_sample(&mut self) -> CommandProgress {
        let temporal_transition_count = self.temporal_transition_count;
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
        sequence.temporal_transitions_at_last_dispatch =
            Some(temporal_transition_count.saturating_sub(sequence.origin_temporal_transitions));
        CommandProgress::PassiveWaiting(Some(Duration::ZERO))
    }

    fn complete_input_sequence_finish(
        &mut self,
        app: &mut MiranteWorkbenchApp,
        workload: Value,
    ) -> CommandProgress {
        let sequence = self
            .active_input_sequence
            .take()
            .expect("the completed automation sequence retains its state");
        let now_ns = app.display_instrumentation_now_ns();
        app.render_coordination
            .end_active_publication_window(now_ns);
        let generation = app.render_coordination.display_generation();
        let mailbox = app.render_intent_mailbox.snapshot();
        let actual_duration_ns =
            u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let scheduled_span_ns = sequence_sample_target_ns(
            sequence.samples.saturating_sub(1),
            sequence.samples,
            sequence.duration_ms,
        );
        let dispatch_span_ns = sequence
            .last_dispatch_elapsed_ns
            .zip(sequence.first_dispatch_elapsed_ns)
            .map(|(last, first)| last.saturating_sub(first));
        let temporal_transitions = sequence
            .temporal_transitions_at_last_dispatch
            .expect("a completed sequence recorded its transition count at the last input sample");
        let observation_window_ns = sequence
            .last_dispatch_elapsed_ns
            .unwrap_or(actual_duration_ns);
        let temporal_distribution = completed_temporal_distribution(
            &sequence.temporal_transition_elapsed_ns,
            observation_window_ns,
        );
        let cadence_comparison = sequence.stationary_baseline.as_ref().map(|baseline| {
            let baseline_transitions =
                u64::try_from(baseline.transition_elapsed_ns.len()).unwrap_or(u64::MAX);
            let input_transitions =
                u64::try_from(temporal_distribution.transition_elapsed_ns.len())
                    .unwrap_or(u64::MAX);
            PlaybackCadenceComparison {
                baseline_transitions,
                input_transitions,
                minimum_input_transitions: minimum_interaction_cadence(baseline_transitions),
                baseline_maximum_gap_ns: baseline.maximum_gap_ns,
                input_maximum_gap_ns: temporal_distribution.maximum_gap_ns,
                maximum_allowed_input_gap_ns: sequence
                    .requested_frame_period_ns
                    .saturating_mul(3)
                    .max(
                        baseline
                            .maximum_gap_ns
                            .saturating_add(sequence.requested_frame_period_ns),
                    ),
                requested_frame_period_ns: sequence.requested_frame_period_ns,
            }
        });
        let cancelled_gestures = mailbox
            .cancelled_gestures
            .saturating_sub(sequence.origin_render_intent_cancelled_gestures);
        self.last_input_sequence_temporal_transitions = Some(temporal_transitions);
        self.last_input_sequence_cancelled_gestures = Some(cancelled_gestures);
        self.last_input_sequence_temporal_distribution = Some(temporal_distribution.clone());
        self.last_input_sequence_cadence_comparison = cadence_comparison.clone();
        CommandProgress::Done(json!({
            "workload": workload,
            "samples": sequence.samples,
            "requested_duration_ms": sequence.duration_ms,
            "actual_duration_ns": actual_duration_ns,
            "input_evidence": {
                "automation_level": "transient_render_intent_mailbox",
                "os_input_injected": false,
                "os_input_claimed": false,
                "dispatch_timing": {
                    "clock": "std_instant_monotonic",
                    "scheduled_span_ns": scheduled_span_ns,
                    "dispatched_samples": sequence.dispatched_samples,
                    "distinct_app_updates": sequence.distinct_dispatch_updates,
                    "first_dispatch_elapsed_ns": sequence.first_dispatch_elapsed_ns,
                    "last_dispatch_elapsed_ns": sequence.last_dispatch_elapsed_ns,
                    "dispatch_span_ns": dispatch_span_ns,
                    "maximum_dispatch_interval_ns": sequence.maximum_dispatch_interval_ns,
                    "maximum_dispatch_lateness_ns": sequence.maximum_dispatch_lateness_ns,
                    "nonmonotonic_dispatches": sequence.nonmonotonic_dispatches,
                    "same_update_dispatches": sequence.same_update_dispatches,
                },
            },
            "observed_counter_delta": {
                "input_generations": generation.input_generation.saturating_sub(sequence.origin_generation),
                "durable_gesture_commits": generation.durable_gesture_commits.saturating_sub(sequence.origin_durable_commits),
                "render_intent_revisions": mailbox.latest_revision.get().saturating_sub(sequence.origin_render_intent_revision),
                "render_intent_samples": mailbox.raw_samples.saturating_sub(sequence.origin_render_intent_samples),
                "render_intent_coalesced_samples": mailbox.coalesced_samples.saturating_sub(sequence.origin_render_intent_coalesced_samples),
                "render_intent_finished_gestures": mailbox.finished_gestures.saturating_sub(sequence.origin_render_intent_finished_gestures),
                "render_intent_cancelled_gestures": cancelled_gestures,
                "presented_temporal_transitions": temporal_transitions,
            },
            "temporal_presentation_timing": {
                "clock": "std_instant_monotonic",
                "observation_window_ns": temporal_distribution.observation_window_ns,
                "transition_elapsed_ns": temporal_distribution.transition_elapsed_ns,
                "first_half_transitions": temporal_distribution.first_half_transitions,
                "second_half_transitions": temporal_distribution.second_half_transitions,
                "maximum_gap_ns": temporal_distribution.maximum_gap_ns,
            },
            "stationary_cadence_comparison": cadence_comparison.as_ref().map(|comparison| json!({
                "baseline_transitions": comparison.baseline_transitions,
                "input_transitions": comparison.input_transitions,
                "minimum_input_transitions": comparison.minimum_input_transitions,
                "baseline_maximum_gap_ns": comparison.baseline_maximum_gap_ns,
                "input_maximum_gap_ns": comparison.input_maximum_gap_ns,
                "maximum_allowed_input_gap_ns": comparison.maximum_allowed_input_gap_ns,
                "requested_frame_period_ns": comparison.requested_frame_period_ns,
                "passed": comparison.passes(),
            })),
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

    fn observe_temporal_presentation(&mut self, app: &MiranteWorkbenchApp) {
        let snapshot = app.application.snapshot();
        let panels = visible_product_panels(&snapshot);
        let panel_timepoints = panels
            .iter()
            .map(|panel| {
                let frame = product_presentation(app, *panel);
                (
                    panel.label(),
                    frame.map(|frame| frame.timepoint()),
                    frame.map(|frame| frame.progress().completeness()),
                )
            })
            .collect::<Vec<_>>();
        let all_present = panel_timepoints
            .iter()
            .all(|(_, timepoint, _)| timepoint.is_some());
        let coherent = panel_timepoints
            .first()
            .and_then(|(_, timepoint, _)| *timepoint)
            .filter(|timepoint| {
                panel_timepoints
                    .iter()
                    .all(|(_, candidate, _)| *candidate == Some(*timepoint))
            });
        let playback_active = snapshot.transient().playback_active();
        let spatial_interaction_active = app
            .render_intent_mailbox
            .active_target(RenderIntentBase::from_snapshot(&snapshot))
            .is_some();
        let temporal_contract = app.playback_session.contract();
        let temporal_contract_identity = temporal_contract.map(|contract| {
            (
                contract.generation(),
                contract
                    .layer_scales()
                    .iter()
                    .map(|(layer, scale)| (layer.ordinal(), scale.get()))
                    .collect::<Vec<_>>(),
            )
        });
        if !playback_active {
            self.active_temporal_contract = None;
        } else if let Some(identity) = temporal_contract_identity.as_ref() {
            match self.active_temporal_contract.as_ref() {
                None => self.active_temporal_contract = Some(identity.clone()),
                Some(active) if active != identity && self.temporal_violation.is_none() => {
                    self.temporal_violation = Some(format!(
                        "playback replaced immutable session contract {:?} with {:?} while active",
                        active, identity
                    ));
                }
                Some(_) => {}
            }
        } else if self.active_temporal_contract.is_some() && self.temporal_violation.is_none() {
            self.temporal_violation =
                Some("playback lost its immutable session contract while active".to_owned());
        }
        if playback_active && self.temporal_violation.is_none() {
            if !all_present {
                self.temporal_violation = Some(format!(
                    "playback exposed a missing visible presentation: {}",
                    panel_timepoints
                        .iter()
                        .map(|(panel, timepoint, _)| format!(
                            "{panel}={}",
                            timepoint
                                .map_or_else(|| "none".to_owned(), |value| value.get().to_string())
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else if coherent.is_none() {
                self.temporal_violation = Some(format!(
                    "playback exposed incoherent visible timepoints: {}",
                    panel_timepoints
                        .iter()
                        .map(|(panel, timepoint, _)| format!(
                            "{panel}={}",
                            timepoint
                                .map_or_else(|| "none".to_owned(), |value| value.get().to_string())
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else if !spatial_interaction_active
                && panel_timepoints.iter().any(|(_, _, completeness)| {
                    *completeness == Some(mirante4d_render_api::FrameCompleteness::Progressive)
                })
            {
                self.temporal_violation =
                    Some("playback exposed a partially complete temporal presentation".to_owned());
            }
        }

        let Some(timepoint) = coherent else {
            return;
        };
        let changed = self.last_coherent_temporal_timepoint != Some(timepoint);
        if changed {
            let previous = self.last_coherent_temporal_timepoint;
            if playback_active && let Some(previous) = previous {
                let count = snapshot.timepoint_count();
                let expected = TimeIndex::new((previous.get() + 1) % count);
                if timepoint != expected && self.temporal_violation.is_none() {
                    self.temporal_violation = Some(format!(
                        "playback presented timepoint {} after {}, expected {}",
                        timepoint.get(),
                        previous.get(),
                        expected.get()
                    ));
                }
                let Some(contract) = temporal_contract else {
                    if self.temporal_violation.is_none() {
                        self.temporal_violation = Some(format!(
                            "playback presented timepoint {} without an admitted session contract",
                            timepoint.get()
                        ));
                    }
                    return;
                };
                if contract.target_set()
                    != crate::playback_session::PlaybackTargetSet::from(
                        application_view(&snapshot).layout(),
                    )
                    && self.temporal_violation.is_none()
                {
                    self.temporal_violation = Some(format!(
                        "playback session target set {:?} disagrees with visible layout {:?}",
                        contract.target_set(),
                        application_view(&snapshot).layout()
                    ));
                }
                for panel in &panels {
                    let surface = app.render_coordination.surface(panel.presentation_slot());
                    for (layer, selected_scale) in contract.layer_scales().iter() {
                        let observed = surface
                            .layer_presentations()
                            .iter()
                            .find(|status| status.layer_ordinal == layer.ordinal());
                        let stable = observed.is_some_and(|status| {
                            !status.mixed
                                && status.displayed_scale_level == Some(selected_scale.get())
                        });
                        if !stable && self.temporal_violation.is_none() {
                            self.temporal_violation = Some(format!(
                                "playback presented {} layer {} outside fixed session scale s{}: {:?}",
                                panel.label(),
                                layer.ordinal(),
                                selected_scale.get(),
                                observed
                            ));
                        }
                    }
                }
                if let Some(sequence) = self.active_input_sequence.as_mut() {
                    sequence.temporal_transition_elapsed_ns.push(
                        u64::try_from(sequence.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    );
                }
                if let Some(observation) = self.active_playback_cadence.as_mut() {
                    observation.transition_elapsed_ns.push(
                        u64::try_from(observation.started_at.elapsed().as_nanos())
                            .unwrap_or(u64::MAX),
                    );
                }
                self.temporal_transition_count = self.temporal_transition_count.saturating_add(1);
                self.temporal_contract_transition_count =
                    self.temporal_contract_transition_count.saturating_add(1);
            }
            self.last_coherent_temporal_timepoint = Some(timepoint);
            if self.temporal_observations.len() < MAX_TEMPORAL_OBSERVATIONS {
                self.temporal_observations.push(json!({
                    "elapsed_ns": u64::try_from(self.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    "time_index": timepoint.get(),
                    "playback_active": playback_active,
                    "playback_phase": format!("{:?}", snapshot.transient().playback_phase()),
                    "session_generation": temporal_contract.map(|contract| contract.generation()),
                    "fixed_layer_scales": temporal_contract.map(|contract| contract.layer_scales().iter().map(|(layer, scale)| json!({
                        "layer_ordinal": layer.ordinal(),
                        "scale_level": scale.get(),
                    })).collect::<Vec<_>>()),
                    "panels": panel_timepoints.iter().map(|(panel, candidate, completeness)| json!({
                        "panel": panel,
                        "time_index": candidate.map(TimeIndex::get),
                        "completeness": completeness.map(|value| format!("{value:?}")),
                    })).collect::<Vec<_>>(),
                }));
            }
        }
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
            .as_ref()
            .is_some_and(|sequence| sequence.command_index != command_index)
        {
            self.close_active_input_sequence_window(app);
        }
        let command_started = Instant::now();
        if let Some(Err(error)) = app.native_presentation.product_pipeline_readiness() {
            let mut reason = format!("GPU renderer initialization failed: {error}");
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
        self.observe_temporal_presentation(app);
        self.observe_playback_stop_trace(app);
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
        self.active_temporal_wait = None;
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

    fn capture_import_integrity_audit_evidence(
        &self,
        app: &MiranteWorkbenchApp,
    ) -> Result<ImportIntegrityAuditEvidenceSnapshot, String> {
        let audit_origin = self
            .active_import_integrity_audit_diagnostics_origin
            .as_ref()
            .ok_or_else(|| {
                "import publication has no package-integrity-audit diagnostics origin".to_owned()
            })?;
        let audit_current = app
            .package_integrity_audit_service
            .as_ref()
            .ok_or_else(|| {
                "import publication has no package-integrity-audit diagnostics".to_owned()
            })?
            .diagnostics();
        let package_integrity_audit_started_runs = audit_current
            .started_runs
            .checked_sub(audit_origin.started_runs)
            .ok_or_else(|| "package-integrity-audit started-run counter regressed".to_owned())?;
        let package_integrity_audit_progress_updates = audit_current
            .progress_updates
            .checked_sub(audit_origin.progress_updates)
            .ok_or_else(|| "package-integrity-audit progress counter regressed".to_owned())?;
        let package_integrity_audit_cancelled_runs = audit_current
            .cancelled_runs
            .checked_sub(audit_origin.cancelled_runs)
            .ok_or_else(|| "package-integrity-audit cancellation counter regressed".to_owned())?;
        let package_integrity_audit_failed_runs = audit_current
            .failed_runs
            .checked_sub(audit_origin.failed_runs)
            .ok_or_else(|| "package-integrity-audit failed-run counter regressed".to_owned())?;
        let package_integrity_audit_completed_runs = audit_current
            .completed_runs
            .checked_sub(audit_origin.completed_runs)
            .ok_or_else(|| "package-integrity-audit success counter regressed".to_owned())?;

        Ok(ImportIntegrityAuditEvidenceSnapshot {
            package_integrity_audit_started_runs,
            package_integrity_audit_progress_updates,
            package_integrity_audit_cancelled_runs,
            package_integrity_audit_failed_runs,
            package_integrity_audit_completed_runs,
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
        let audit = self.capture_import_integrity_audit_evidence(app)?;

        Ok(ImportPublicationEvidenceSnapshot {
            publication_currentness: publication_transfer.execution().into(),
            package_integrity_audit_started_runs: audit.package_integrity_audit_started_runs,
            package_integrity_audit_progress_updates: audit
                .package_integrity_audit_progress_updates,
            package_integrity_audit_cancelled_runs: audit.package_integrity_audit_cancelled_runs,
            package_integrity_audit_failed_runs: audit.package_integrity_audit_failed_runs,
            package_integrity_audit_completed_runs: audit.package_integrity_audit_completed_runs,
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
            active_audit_origin: &mut self.active_import_integrity_audit_diagnostics_origin,
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
                dispatch_application_command(app, ctx, ApplicationCommand::AttachDataset)?;
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
            ProductAutomationCommand::RecoverExposedUnsavedAutosave => {
                if !app.project_recovery_panel_open {
                    return Err(
                        "unsaved-autosave recovery was not exposed in the startup recovery panel"
                            .to_owned(),
                    );
                }
                let project_ids = app
                    .project_store
                    .as_ref()
                    .ok_or_else(|| "project-store service is unavailable".to_owned())?
                    .recovery_store_project_ids()
                    .collect::<Vec<_>>();
                let [project_id] = project_ids.as_slice() else {
                    return Err(format!(
                        "packaged recovery check requires exactly one exposed earlier-launch store, found {}",
                        project_ids.len()
                    ));
                };
                if !app
                    .project_store
                    .as_ref()
                    .is_some_and(ProjectStoreApplicationService::can_open)
                {
                    return Err("exposed unsaved-autosave recovery is not ready to open".to_owned());
                }
                let project_id = *project_id;
                app.project_recovery_panel_open = false;
                app.open_recovery_locator(project_id);
                let open_started_or_completed = app
                    .project_store
                    .as_ref()
                    .is_some_and(|service| service.status().foreground_active())
                    || app.application.snapshot().is_bound();
                if !open_started_or_completed {
                    return Err(
                        "exposed unsaved-autosave recovery did not enter the normal project-open route"
                            .to_owned(),
                    );
                }
                Ok(CommandProgress::Done(json!({
                    "project_id": project_id.to_string(),
                    "startup_panel_was_open": true,
                    "normal_reducer_service_path": true,
                    "foreground_started_or_completed": true,
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
            ProductAutomationCommand::CancelPackageIntegrityAudit => {
                let service = app
                    .package_integrity_audit_service
                    .as_mut()
                    .ok_or_else(|| "package-integrity-audit service is unavailable".to_owned())?;
                service
                    .reset_diagnostics()
                    .map_err(|error| error.to_string())?;

                let snapshot = app.application.snapshot();
                match snapshot.source().integrity_audit() {
                    PackageIntegrityAuditSnapshot::Running { .. } => {
                        return Err(
                            "cancel_package_integrity_audit requires an idle source state"
                                .to_owned(),
                        );
                    }
                    PackageIntegrityAuditSnapshot::NotRun
                    | PackageIntegrityAuditSnapshot::SelfConsistent(_)
                    | PackageIntegrityAuditSnapshot::Failed(_)
                    | PackageIntegrityAuditSnapshot::Cancelled => {}
                }
                app.application
                    .dispatch(ApplicationCommand::RequestPackageIntegrityAudit)
                    .map_err(|fault| {
                        format!("package-integrity-audit request was rejected: {fault:?}")
                    })?;
                let operation_id = match app.application.snapshot().source().integrity_audit() {
                    PackageIntegrityAuditSnapshot::Running { operation_id, .. } => *operation_id,
                    _ => {
                        return Err(
                            "package-integrity-audit request did not create an operation"
                                .to_owned(),
                        );
                    }
                };
                app.application
                    .dispatch(ApplicationCommand::CancelOperation(operation_id))
                    .map_err(|fault| {
                        format!("package-integrity-audit cancellation was rejected: {fault:?}")
                    })?;
                app.pump_application_services();
                Ok(CommandProgress::Done(json!({
                    "operation_id": operation_id.get(),
                    "cancellation_requested_before_worker_poll": true,
                })))
            }
            ProductAutomationCommand::CancelActivePackageIntegrityAudit => {
                cancel_active_package_integrity_audit(app).map(CommandProgress::Done)
            }
            ProductAutomationCommand::RequestPackageIntegrityAudit => {
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::RequestPackageIntegrityAudit,
                )?;
                Ok(CommandProgress::Done(json!({
                    "requested": true,
                })))
            }
            ProductAutomationCommand::BeginTiffImportSetup {
                source,
                output_parent,
                source_kind,
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
                let (setup_kind, channel_source) = match source_kind {
                    ProductAutomationTiffSourceKind::Single3dTiff => (
                        ImportChannelSourceKind::Single3dTiff,
                        TiffChannelSource::single_3d("channel 1", source).map_err(str::to_owned)?,
                    ),
                    ProductAutomationTiffSourceKind::FolderOf3dTiffs => (
                        ImportChannelSourceKind::FolderOf3dTiffs,
                        TiffChannelSource::folder_of_3d("channel 1", source)
                            .map_err(str::to_owned)?,
                    ),
                    ProductAutomationTiffSourceKind::FolderOf2dTiffs => (
                        ImportChannelSourceKind::FolderOf2dTiffs,
                        TiffChannelSource::folder_of_2d("channel 1", source)
                            .map_err(str::to_owned)?,
                    ),
                };
                let tiff_source = TiffSource::new(vec![channel_source]).map_err(str::to_owned)?;
                let destination =
                    crate::import_workflow::tiff_destination(&tiff_source, output_parent);
                self.active_import_pre_start_origin = Some(ImportPreStartOrigin {
                    started_at_epoch_ms: epoch_ms(),
                    process_cpu_time_ns: process_cpu_time_ns(),
                    started_at: Instant::now(),
                    destination: destination.clone(),
                });
                self.completed_import_pre_start_measurement = None;
                app.import.begin_setup();
                app.import.set_channel_kind(0, setup_kind);
                app.import.install_channel_selection(0, source.clone());
                app.bind_import_worker_completion_repaint(ctx);
                app.import
                    .workers
                    .start_inspection(tiff_source, PathBuf::new())
                    .map_err(|error| error.to_string())?;
                app.import.mark_channel_inspection_active(0);
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
                no_data_value_rule,
                hide_constant_z_planes,
            } => {
                let ImportWorkflowSnapshot::Review(review) = app.import.snapshot() else {
                    return Err("no completed TIFF review is ready to start".to_owned());
                };
                let draft = ImportReviewDraft {
                    spacing_zyx_um: *spacing_zyx_um,
                    calibration_confirmed: true,
                    time_step_seconds: *time_step_seconds,
                    no_data_value_rule: no_data_value_rule.map(|rule| match rule {
                        ProductAutomationNoDataValueRule::Automatic => {
                            mirante4d_application::import_workflow::ImportNoDataValueRule::Automatic
                        }
                        ProductAutomationNoDataValueRule::ManualUint8 {
                            value,
                        } => mirante4d_application::import_workflow::ImportNoDataValueRule::ManualUint8(value),
                    }),
                    hide_constant_z_planes: *hide_constant_z_planes,
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
                let integrity_audit_diagnostics_origin = app
                    .package_integrity_audit_service
                    .as_ref()
                    .ok_or_else(|| {
                        "reviewed TIFF import has no package-integrity-audit service".to_owned()
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
                self.active_import_integrity_audit_diagnostics_origin =
                    Some(integrity_audit_diagnostics_origin);
                self.completed_import_primary_measurement = None;
                self.imported_open_ready_outcome = None;
                self.completed_import_pre_start_measurement = Some(pre_start_measurement);
                self.active_import_pre_start_origin = None;
                Ok(CommandProgress::Done(json!({
                    "review_id": review.review_id.get(),
                    "destination": review.destination,
                    "operation_token": timing_origin.token.as_ref().map(operation_token_json),
                    "reviewed_source_fingerprint_sha256": timing_origin.source_fingerprint.to_string(),
                    "reviewed_source_bytes": timing_origin.reviewed_source_bytes,
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
                        "content_id_computed_during_import": true,
                        "import_idle": true,
                        "normal_product_open_path": true,
                        "primary_clock": import_primary_measurement_json(Some(measurement)),
                        "waited_ms": duration_ms(started.elapsed()),
                    })))
                } else if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    Err(format!(
                        "timed out after {timeout_ms} ms waiting for imported package {} to become admitted and open-ready (selected_matches={}, content_id_computed_during_import={}, import_idle={}, problem={:?})",
                        path.display(),
                        readiness.selected_matches,
                        readiness.content_id_computed_during_import,
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
                if matches!(condition, ProductAutomationWaitCondition::ImportReviewReady)
                    && matches!(app.import.snapshot(), ImportWorkflowSnapshot::Configure(_))
                    && !app.import.workers.status().is_active()
                    && app.import.setup.as_ref().is_some_and(|setup| {
                        setup
                            .channels
                            .iter()
                            .all(|channel| channel.inspection.is_some())
                    })
                {
                    let inspection = app
                        .import
                        .validated_setup_inspection()
                        .map_err(|error| error.to_string())?;
                    let source = inspection.source().clone();
                    let destination = self
                        .active_import_pre_start_origin
                        .as_ref()
                        .ok_or_else(|| {
                            "inspected TIFF setup has no automation destination".to_owned()
                        })?
                        .destination
                        .clone();
                    app.import.setup = None;
                    app.import
                        .install_review(source, inspection, destination)
                        .map_err(|error| error.to_string())?;
                }
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
                // The viewer widgets publish this override through the same
                // coalesced viewport-observation boundary as native geometry.
                // That boundary owns both the coordination mutation and its
                // single new render frame identity.
                ctx.request_repaint();
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
            ProductAutomationCommand::SetPlaybackFps { fps } => {
                let playback_fps = mirante4d_application::PlaybackFps::new(*fps)
                    .ok_or_else(|| format!("invalid playback FPS {fps}"))?;
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetPlaybackFps(playback_fps),
                )?;
                Ok(CommandProgress::Done(json!({
                    "fps": fps,
                    "semantic_application_command": true,
                })))
            }
            ProductAutomationCommand::SetPlaybackActive { active } => {
                if *active {
                    self.active_playback_stop_trace = None;
                } else {
                    self.begin_playback_stop_trace(app)?;
                }
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetPlaybackActive(*active),
                )?;
                let snapshot = app.application.snapshot();
                if snapshot.transient().playback_active() != *active {
                    return Err(format!(
                        "playback active state remained {} after requesting {active}",
                        snapshot.transient().playback_active()
                    ));
                }
                Ok(CommandProgress::Done(json!({
                    "active": active,
                    "phase": format!("{:?}", snapshot.transient().playback_phase()),
                    "effective_time_index": snapshot.view().timepoint().get(),
                    "semantic_application_command": true,
                })))
            }
            ProductAutomationCommand::WaitForPresentedTimeIndex {
                time_index,
                timeout_ms,
            } => {
                let snapshot = app.application.snapshot();
                if *time_index >= snapshot.timepoint_count() {
                    return Err(format!(
                        "presented time index {time_index} is out of bounds for {} timepoints",
                        snapshot.timepoint_count()
                    ));
                }
                let expected = TimeIndex::new(*time_index);
                let panels = visible_product_panels(&snapshot);
                let ready = snapshot.view().timepoint() == expected
                    && coordinated_visible_layout_current_complete_with_snapshot(app, &snapshot)
                    && panels.iter().all(|panel| {
                        let surface = app.render_coordination.surface(panel.presentation_slot());
                        surface.display_current()
                            && surface.presented_frame().is_some_and(|frame| {
                                frame.timepoint() == expected
                                    && frame.progress().completeness()
                                        != mirante4d_render_api::FrameCompleteness::Progressive
                            })
                    });
                if ready {
                    return Ok(CommandProgress::Done(json!({
                        "time_index": time_index,
                        "visible_panels": panels.iter().map(|panel| panel.label()).collect::<Vec<_>>(),
                        "actual_presented_timepoint_observed": true,
                        "coordinated_current_complete": true,
                    })));
                }
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    return Err(format!(
                        "timed out waiting for complete current presentation of timepoint {time_index}; actual={}",
                        panels
                            .iter()
                            .map(|panel| format!(
                                "{}={}",
                                panel.label(),
                                product_presentation(app, *panel).map_or_else(
                                    || "none".to_owned(),
                                    |frame| frame.timepoint().get().to_string(),
                                )
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                Ok(CommandProgress::Waiting)
            }
            ProductAutomationCommand::WaitForTemporalTransitions {
                minimum_transitions,
                timeout_ms,
            } => {
                if let Some(violation) = self.temporal_violation.as_ref() {
                    return Err(format!("temporal continuity violation: {violation}"));
                }
                let origin = match self.active_temporal_wait {
                    Some((command_index, origin)) if command_index == self.command_index => origin,
                    _ => {
                        let origin = self.temporal_transition_count;
                        self.active_temporal_wait = Some((self.command_index, origin));
                        origin
                    }
                };
                let observed = self.temporal_transition_count.saturating_sub(origin);
                if observed >= u64::from(*minimum_transitions) {
                    return Ok(CommandProgress::Done(json!({
                        "minimum_transitions": minimum_transitions,
                        "observed_transitions": observed,
                        "ordered_coherent_presentations": true,
                    })));
                }
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                if started.elapsed() >= Duration::from_millis(*timeout_ms) {
                    return Err(format!(
                        "timed out after {timeout_ms} ms waiting for {minimum_transitions} presented temporal transitions; observed {observed}"
                    ));
                }
                Ok(CommandProgress::Waiting)
            }
            ProductAutomationCommand::ObservePlaybackCadence { duration_ms } => {
                if !app.application.snapshot().transient().playback_active() {
                    return Err(
                        "playback cadence observation requires an active prepared playback session"
                            .to_owned(),
                    );
                }
                if self.active_playback_cadence.is_none() {
                    self.last_stationary_playback_cadence = None;
                    self.active_playback_cadence = Some(ActivePlaybackCadenceObservation {
                        command_index: self.command_index,
                        started_at: Instant::now(),
                        duration_ms: *duration_ms,
                        origin_temporal_transitions: self.temporal_transition_count,
                        transition_elapsed_ns: Vec::new(),
                    });
                }
                let active = self
                    .active_playback_cadence
                    .as_ref()
                    .expect("an initialized cadence observation retains active state");
                if active.command_index != self.command_index || active.duration_ms != *duration_ms
                {
                    return Err(
                        "playback cadence observation state belongs to a different command"
                            .to_owned(),
                    );
                }
                let elapsed_ns =
                    u64::try_from(active.started_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
                let requested_ns = duration_ms.saturating_mul(1_000_000);
                if elapsed_ns < requested_ns {
                    return Ok(CommandProgress::PassiveWaiting(Some(Duration::from_nanos(
                        requested_ns.saturating_sub(elapsed_ns),
                    ))));
                }
                let active = self
                    .active_playback_cadence
                    .take()
                    .expect("the completed cadence observation retains active state");
                let observed = self
                    .temporal_transition_count
                    .saturating_sub(active.origin_temporal_transitions);
                let distribution =
                    completed_temporal_distribution(&active.transition_elapsed_ns, elapsed_ns);
                if u64::try_from(distribution.transition_elapsed_ns.len()).unwrap_or(u64::MAX)
                    != observed
                {
                    return Err(format!(
                        "stationary cadence transition counter ({observed}) disagrees with its monotonic timestamps ({})",
                        distribution.transition_elapsed_ns.len()
                    ));
                }
                self.last_stationary_playback_cadence = Some(distribution.clone());
                Ok(CommandProgress::Done(json!({
                    "requested_duration_ms": duration_ms,
                    "actual_duration_ns": elapsed_ns,
                    "presented_temporal_transitions": observed,
                    "temporal_presentation_timing": {
                        "clock": "std_instant_monotonic",
                        "observation_window_ns": distribution.observation_window_ns,
                        "transition_elapsed_ns": distribution.transition_elapsed_ns,
                        "first_half_transitions": distribution.first_half_transitions,
                        "second_half_transitions": distribution.second_half_transitions,
                        "maximum_gap_ns": distribution.maximum_gap_ns,
                    },
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::ThreeD,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let viewport_side = 800.0;
                    let start = [viewport_side * 0.5, viewport_side * 0.5];
                    let current = [
                        start[0] + *yaw_points_per_sample,
                        start[1] + *pitch_points_per_sample,
                    ];
                    let camera = orbit_camera(
                        effective_interaction_camera(app),
                        start,
                        current,
                        [viewport_side, viewport_side],
                    );
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::camera(RenderGestureKind::Drag, camera),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(app, ctx, RenderIntentTarget::ThreeD)?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "camera_orbit",
                            "last_sample_index": samples.saturating_sub(1),
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::ThreeD,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let camera = pan_camera(
                        effective_interaction_camera(app),
                        [*x_points_per_sample, *y_points_per_sample],
                    );
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::camera(RenderGestureKind::Drag, camera),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(app, ctx, RenderIntentTarget::ThreeD)?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "camera_pan",
                            "last_sample_index": samples.saturating_sub(1),
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::ThreeD,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let camera = zoom_camera(
                        effective_interaction_camera(app),
                        *scroll_y_points_per_sample,
                    );
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::camera(RenderGestureKind::Scroll, camera),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(app, ctx, RenderIntentTarget::ThreeD)?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "camera_zoom",
                            "last_sample_index": samples.saturating_sub(1),
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::Linked2d,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(
                        effective_interaction_cross_section(app),
                    );
                    state.rotate_oblique_by_panel_drag(
                        interaction_cross_section_panel(*panel),
                        *x_points_per_sample,
                        *y_points_per_sample,
                        *radians_per_point,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section rotation was rejected: {error}"))?;
                    let application_panel = application_cross_section_panel(*panel);
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::cross_section(
                            application_panel,
                            RenderGestureKind::Drag,
                            cross_section,
                        ),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(
                        app,
                        ctx,
                        RenderIntentTarget::CrossSection(application_cross_section_panel(*panel)),
                    )?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "cross_section_rotate",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": samples.saturating_sub(1),
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::Linked2d,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(
                        effective_interaction_cross_section(app),
                    );
                    state.pan_by_panel_points(
                        interaction_cross_section_panel(*panel),
                        *x_points_per_sample,
                        *y_points_per_sample,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section pan was rejected: {error}"))?;
                    let application_panel = application_cross_section_panel(*panel);
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::cross_section(
                            application_panel,
                            RenderGestureKind::Drag,
                            cross_section,
                        ),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(
                        app,
                        ctx,
                        RenderIntentTarget::CrossSection(application_cross_section_panel(*panel)),
                    )?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "cross_section_pan",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": samples.saturating_sub(1),
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
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::Linked2d,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
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
                    let mut state = CrossSectionViewState::from_canonical(
                        effective_interaction_cross_section(app),
                    );
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
                    let application_panel = application_cross_section_panel(*panel);
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::cross_section(
                            application_panel,
                            RenderGestureKind::Scroll,
                            cross_section,
                        ),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(
                        app,
                        ctx,
                        RenderIntentTarget::CrossSection(application_cross_section_panel(*panel)),
                    )?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "cross_section_zoom",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": samples.saturating_sub(1),
                        }),
                    ))
                }
            },
            ProductAutomationCommand::CrossSectionSliceSequence {
                panel,
                samples,
                duration_ms,
                distance_world_per_sample,
            } => match self.input_sequence_step(
                app,
                *samples,
                *duration_ms,
                CoordinatedPresentationGroup::Linked2d,
            ) {
                InputSequenceStep::Wait(delay) => Ok(CommandProgress::PassiveWaiting(Some(delay))),
                InputSequenceStep::Dispatch(_) => {
                    let snapshot = app.application.snapshot();
                    let view = application_view(&snapshot);
                    if view.layout() != ViewerLayout::FourPanel {
                        return Err("cross-section sequence requires four-panel layout".to_owned());
                    }
                    let mut state = CrossSectionViewState::from_canonical(
                        effective_interaction_cross_section(app),
                    );
                    state.slice_by_world_distance(
                        interaction_cross_section_panel(*panel),
                        *distance_world_per_sample,
                    );
                    let cross_section = state
                        .into_canonical()
                        .map_err(|error| format!("cross-section slice was rejected: {error}"))?;
                    let application_panel = application_cross_section_panel(*panel);
                    dispatch_render_intent_sample(
                        app,
                        ctx,
                        RenderIntentSample::cross_section(
                            application_panel,
                            RenderGestureKind::Scroll,
                            cross_section,
                        ),
                    )?;
                    Ok(self.complete_input_sequence_sample())
                }
                InputSequenceStep::Finish => {
                    finish_render_intent(
                        app,
                        ctx,
                        RenderIntentTarget::CrossSection(application_cross_section_panel(*panel)),
                    )?;
                    Ok(self.complete_input_sequence_finish(
                        app,
                        json!({
                            "kind": "cross_section_slice",
                            "panel": PanelId::from(*panel).label(),
                            "last_sample_index": samples.saturating_sub(1),
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
            ProductAutomationCommand::CaptureScreenshot { target, name } => {
                let panel = PanelId::from(*target);
                if !product_presentations_ready(app, &[panel])? {
                    let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                    if started.elapsed() >= AUTOMATION_CAPTURE_TIMEOUT {
                        return Err(format!(
                            "timed out waiting for current GPU validation capture: {}",
                            product_capture_state(app, &[panel])
                        ));
                    }
                    return Ok(CommandProgress::Waiting);
                }
                let artifact = self.capture_viewport_artifact(app, *target, name.as_deref())?;
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
            ProductAutomationCommand::CaptureTemporalFrame {
                target,
                name,
                min_different_pixels_from_previous,
            } => {
                let panel = PanelId::from(*target);
                if !product_presentations_ready(app, &[panel])? {
                    let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                    if started.elapsed() >= AUTOMATION_CAPTURE_TIMEOUT {
                        return Err(format!(
                            "timed out waiting for temporal GPU validation capture: {}",
                            product_capture_state(app, &[panel])
                        ));
                    }
                    return Ok(CommandProgress::Waiting);
                }
                let surface = app.render_coordination.surface(panel.presentation_slot());
                let frame = surface
                    .presented_frame()
                    .ok_or_else(|| format!("{} has no presented temporal frame", panel.label()))?;
                if !surface.display_current()
                    || frame.progress().completeness()
                        == mirante4d_render_api::FrameCompleteness::Progressive
                {
                    return Err(format!(
                        "{} temporal capture is not a complete current presentation",
                        panel.label()
                    ));
                }
                let timepoint = frame.timepoint();
                let rgba8 = product_target_capture(app, panel)
                    .expect("capture readiness was established above")
                    .rgba8()
                    .to_vec();
                let intermediate_rgb_pixels = rgba8
                    .chunks_exact(4)
                    .filter(|pixel| pixel[..3].iter().any(|value| (1..=254).contains(value)))
                    .count();
                if intermediate_rgb_pixels == 0 {
                    return Err(format!(
                        "{} temporal capture at timepoint {} is clipped or blank; no intermediate RGB pixels were observed",
                        panel.label(),
                        timepoint.get()
                    ));
                }
                let previous = self
                    .temporal_capture_baselines
                    .iter()
                    .find(|baseline| baseline.target == *target);
                let different_pixels = previous.map(|baseline| {
                    baseline
                        .rgba8
                        .chunks_exact(4)
                        .zip(rgba8.chunks_exact(4))
                        .filter(|(before, after)| before[..3] != after[..3])
                        .count()
                        .saturating_add(baseline.rgba8.len().abs_diff(rgba8.len()).div_ceil(4))
                });
                if let Some(minimum) = min_different_pixels_from_previous {
                    let observed = different_pixels.ok_or_else(|| {
                        format!(
                            "{} temporal capture requested a pixel-difference threshold without a prior baseline",
                            panel.label()
                        )
                    })?;
                    if observed < *minimum {
                        return Err(format!(
                            "{} temporal capture changed {observed} pixels, fewer than required {minimum}",
                            panel.label()
                        ));
                    }
                    if previous.is_some_and(|baseline| baseline.timepoint == timepoint) {
                        return Err(format!(
                            "{} temporal capture pixels changed without advancing from timepoint {}",
                            panel.label(),
                            timepoint.get()
                        ));
                    }
                }
                let artifact = self.capture_viewport_artifact(app, *target, name.as_deref())?;
                if artifact.pixel_stats.is_blank() {
                    return Err(format!(
                        "temporal viewport capture {} is blank",
                        artifact.path.display()
                    ));
                }
                self.temporal_capture_baselines
                    .retain(|baseline| baseline.target != *target);
                self.temporal_capture_baselines
                    .push(TemporalCaptureBaseline {
                        target: *target,
                        timepoint,
                        rgba8,
                    });
                self.artifacts.push(artifact.clone());
                Ok(CommandProgress::Done(json!({
                    "artifact": artifact.json(),
                    "presented_time_index": timepoint.get(),
                    "different_pixels_from_previous": different_pixels,
                    "intermediate_rgb_pixels": intermediate_rgb_pixels,
                    "semantic_and_visual_evidence": true,
                })))
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
                let playback_stop_handoff_trace = matches!(
                    condition,
                    ProductAutomationAssertCondition::PlaybackStoppedAndReleased
                )
                .then(|| {
                    self.active_playback_stop_trace
                        .as_ref()
                        .map(ActivePlaybackStopTrace::json)
                })
                .flatten();
                Ok(CommandProgress::Done(json!({
                    "condition": condition.name(),
                    "cross_section_snapshot": condition
                        .is_cross_section_condition()
                        .then(|| cross_section_diagnostics_json(app)),
                    "playback_stop_handoff_trace": playback_stop_handoff_trace,
                })))
            }
            ProductAutomationCommand::SleepFrames { frames } => {
                let started = *self.active_wait_started.get_or_insert_with(Instant::now);
                let remaining = self.sleep_frames_remaining.get_or_insert(*frames);
                if *remaining == 0 {
                    return Ok(CommandProgress::Done(json!({
                        "frames": frames,
                        "monotonic_elapsed_ns": u64::try_from(started.elapsed().as_nanos())
                            .unwrap_or(u64::MAX),
                    })));
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
        target: ProductAutomationPresentationTarget,
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
        let panel = PanelId::from(target);
        let surface = app.render_coordination.surface(panel.presentation_slot());
        let frame_identity = surface
            .presented_frame()
            .ok_or_else(|| format!("{} has no presented frame", panel.label()))?
            .frame()
            .get();
        let surface_generation = surface.generation();
        let (capture_source, image) = capture_color_image(app, panel)?;
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
            target: target.name(),
            frame_identity,
            surface_generation,
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
        if let Some((_request, terminal)) =
            app.viewer_pick_queue.take_automation_terminal_for(purpose)
        {
            return Err(format!(
                "{} ended without a scientific result: {terminal:?}",
                automation_pick_purpose_name(purpose)
            ));
        }
        match app.native_presentation.pick_pipeline_is_ready() {
            Ok(true) => {}
            Ok(false) => return Ok(CommandProgress::Waiting),
            Err(error) => {
                return Err(format!(
                    "{} cannot start because GPU renderer initialization failed: {error}",
                    automation_pick_purpose_name(purpose)
                ));
            }
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
                        &app.render_attempt,
                    );
                let demand_currentness = app.visible_demand_plan_currentness();
                let exact_visible_demand = demand_currentness.current_3d
                    && match application_view(snapshot).layout() {
                        ViewerLayout::Single3d => true,
                        ViewerLayout::FourPanel => demand_currentness.cross_sections,
                    };
                let base = RenderIntentBase::from_snapshot(snapshot);
                automation_runtime_is_idle(
                    crate::workbench_playback_runtime::background_work_active(
                        snapshot,
                        &app.import.workers,
                        &app.dataset,
                        &app.render_coordination,
                        &app.native_presentation,
                        &app.render_attempt,
                        progressive_render_work.any_required,
                    ),
                    app.dataset.visible_demand_plan_outstanding(),
                    app.pending_visible_demand_plan.is_some(),
                ) && app.render_intent_mailbox.active_target(base).is_none()
                    && !app.shader_work_envelopes.has_pending_work()
                    && app.resident_cross_section_coverage.is_none()
                    && exact_visible_demand
            }
            ProductAutomationWaitCondition::FrameFreshnessCurrent => {
                frame_freshness_is_current(&app.render_coordination.frame_fidelity)
            }
            ProductAutomationWaitCondition::CoordinatedPresentationSettled => {
                coordinated_visible_layout_current_complete_with_snapshot(app, snapshot)
            }
            ProductAutomationWaitCondition::PackageIntegrityAuditInactive => {
                package_integrity_audit_inactive(app)
            }
            ProductAutomationWaitCondition::PackageIntegrityAuditNotRun => {
                matches!(
                    snapshot.source().integrity_audit(),
                    PackageIntegrityAuditSnapshot::NotRun
                ) && app
                    .package_integrity_audit_service
                    .as_ref()
                    .is_some_and(|service| service.active_token().is_none())
            }
            ProductAutomationWaitCondition::PackageIntegrityAuditSelfConsistent => {
                matches!(
                    snapshot.source().integrity_audit(),
                    PackageIntegrityAuditSnapshot::SelfConsistent(_)
                ) && app
                    .package_integrity_audit_service
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
            ProductAutomationWaitCondition::UnsavedAutosaveRecoveryExposed => {
                app.project_recovery_panel_open
                    && app.project_store.as_ref().is_some_and(|service| {
                        service.can_open() && service.recovery_store_project_ids().len() == 1
                    })
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
            ProductAutomationWaitCondition::InitialAutoDenseApplied => {
                app.initial_auto_dense_is_applied(snapshot)
            }
            ProductAutomationWaitCondition::PlaybackResidencyReleased => {
                let current = snapshot.view().timepoint();
                !snapshot.transient().playback_active()
                    && app
                        .dataset
                        .scope_requirements(crate::dataset_requests::SCOPE_PLAYBACK)
                        .is_empty()
                    && app
                        .dataset
                        .renderer_requirement_handle()
                        .requirements
                        .iter()
                        .all(|key| key.timepoint() == current)
                    && visible_product_panels(snapshot).iter().all(|panel| {
                        product_presentation(app, *panel)
                            .is_some_and(|frame| frame.timepoint() == current)
                    })
            }
        }
    }

    fn assert_latest_probe_pick_evidence(
        &self,
        expected_policy: ProductAutomationPickPolicy,
    ) -> Result<(), String> {
        let event = self
            .events
            .last()
            .ok_or_else(|| "pick evidence assertion has no preceding command event".to_owned())?;
        if event.command != "probe_hover" || event.status != "passed" {
            return Err(format!(
                "pick evidence assertion must immediately follow a passed probe_hover command, found {} ({})",
                event.command, event.status
            ));
        }
        assert_native_pick_evidence(&event.details, expected_policy)
    }

    fn assert_condition(
        &self,
        app: &MiranteWorkbenchApp,
        condition: &ProductAutomationAssertCondition,
    ) -> Result<(), String> {
        let snapshot = app.application.snapshot();
        let view = application_view(&snapshot);
        match condition {
            ProductAutomationAssertCondition::NonblankPanel { target } => {
                let panel = PanelId::from(*target);
                let (source, stats) = current_display_image_stats(app, panel)?;
                if !stats.is_blank() {
                    Ok(())
                } else {
                    Err(format!(
                        "current {} product frame is blank from {source}: nonzero_rgb_pixels={}, max_rgb={}",
                        panel.label(),
                        stats.nonzero_rgb_pixels,
                        stats.max_rgb
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
            ProductAutomationAssertCondition::EmptyPresentation => {
                assert_empty_visible_presentation(app, &snapshot)
            }
            ProductAutomationAssertCondition::FrameFidelity {
                scale_level,
                complete,
                exact,
            } => {
                let fidelity = &app.render_coordination.frame_fidelity;
                let scale_matches = fidelity.displayed_scale_level == Some(*scale_level)
                    && fidelity.target_scale_level == Some(*scale_level);
                let completeness_matches = if *exact {
                    fidelity.completeness == FrameCompleteness::Exact
                } else {
                    !*complete
                        || matches!(
                            fidelity.completeness,
                            FrameCompleteness::Exact | FrameCompleteness::Complete
                        )
                };
                if scale_matches && completeness_matches {
                    Ok(())
                } else {
                    Err(format!(
                        "frame fidelity mismatch: displayed={:?}, target={:?}, completeness={:?}, exact_required={exact}",
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
            ProductAutomationAssertCondition::LayerRenderMode { layer_index, mode } => {
                let expected: RenderMode = (*mode).into();
                let actual = view
                    .layers()
                    .get(*layer_index)
                    .ok_or_else(|| format!("layer index {layer_index} is out of range"))?
                    .render_state()
                    .mode();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "layer {layer_index} render mode is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::PickEvidence { policy } => {
                self.assert_latest_probe_pick_evidence(*policy)
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
            ProductAutomationAssertCondition::PackageIntegrityAuditEvidence {
                min_progress_updates,
                min_cancelled_runs,
                min_completed_runs,
            } => {
                let diagnostics = app
                    .package_integrity_audit_service
                    .as_ref()
                    .ok_or_else(|| "package-integrity-audit service is unavailable".to_owned())?
                    .diagnostics();
                if diagnostics.progress_updates < *min_progress_updates
                    || diagnostics.cancelled_runs < *min_cancelled_runs
                    || diagnostics.completed_runs < *min_completed_runs
                {
                    Err(format!(
                        "package-integrity-audit evidence is incomplete: progress={}, cancelled={}, successes={}",
                        diagnostics.progress_updates,
                        diagnostics.cancelled_runs,
                        diagnostics.completed_runs,
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
            ProductAutomationAssertCondition::TemporalContinuity => {
                if let Some(violation) = self.temporal_violation.as_ref() {
                    Err(format!("temporal continuity violation: {violation}"))
                } else if self.temporal_transition_count == 0 {
                    Err("temporal continuity has no observed presented transition".to_owned())
                } else if self.temporal_contract_transition_count != self.temporal_transition_count
                {
                    Err(format!(
                        "only {} of {} temporal transitions carried an immutable playback contract",
                        self.temporal_contract_transition_count, self.temporal_transition_count
                    ))
                } else {
                    Ok(())
                }
            }
            ProductAutomationAssertCondition::PlaybackAdvancedDuringPreviousInput {
                minimum_transitions,
            } => {
                let observed = self
                    .last_input_sequence_temporal_transitions
                    .ok_or_else(|| {
                        "playback/input assertion has no completed preceding input sequence"
                            .to_owned()
                    })?;
                let cancelled = self.last_input_sequence_cancelled_gestures.ok_or_else(|| {
                    "playback/input assertion has no gesture-lifecycle evidence".to_owned()
                })?;
                let distribution = self
                    .last_input_sequence_temporal_distribution
                    .as_ref()
                    .ok_or_else(|| {
                        "playback/input assertion has no temporal distribution evidence".to_owned()
                    })?;
                let cadence = self
                    .last_input_sequence_cadence_comparison
                    .as_ref()
                    .ok_or_else(|| {
                        "playback/input assertion has no same-duration stationary cadence baseline"
                            .to_owned()
                    })?;
                if cancelled != 0 {
                    Err(format!(
                        "playback cancelled the preceding input gesture {cancelled} times"
                    ))
                } else if u64::try_from(distribution.transition_elapsed_ns.len())
                    .unwrap_or(u64::MAX)
                    != observed
                {
                    Err(format!(
                        "playback transition counter ({observed}) disagrees with its monotonic presentation timestamps ({})",
                        distribution.transition_elapsed_ns.len()
                    ))
                } else if *minimum_transitions >= 2
                    && (distribution.first_half_transitions == 0
                        || distribution.second_half_transitions == 0)
                {
                    Err(format!(
                        "playback transitions were not distributed through the held input: first_half={}, second_half={}, maximum_gap_ns={}, window_ns={}",
                        distribution.first_half_transitions,
                        distribution.second_half_transitions,
                        distribution.maximum_gap_ns,
                        distribution.observation_window_ns,
                    ))
                } else if cadence.baseline_transitions < 3 {
                    Err(format!(
                        "stationary cadence baseline contained only {} temporal transitions",
                        cadence.baseline_transitions
                    ))
                } else if cadence.input_transitions != observed {
                    Err(format!(
                        "cadence comparison counted {} input transitions but the presentation oracle counted {observed}",
                        cadence.input_transitions
                    ))
                } else if !cadence.passes() {
                    Err(format!(
                        "input cadence regressed against the same-duration stationary baseline: input={}/{}, required>={}, input_max_gap_ns={}, allowed_max_gap_ns={}, baseline_max_gap_ns={}, frame_period_ns={}",
                        cadence.input_transitions,
                        cadence.baseline_transitions,
                        cadence.minimum_input_transitions,
                        cadence.input_maximum_gap_ns,
                        cadence.maximum_allowed_input_gap_ns,
                        cadence.baseline_maximum_gap_ns,
                        cadence.requested_frame_period_ns,
                    ))
                } else if observed >= u64::from(*minimum_transitions) {
                    Ok(())
                } else {
                    Err(format!(
                        "playback presented {observed} transitions during the preceding input sequence, fewer than required {minimum_transitions}"
                    ))
                }
            }
            ProductAutomationAssertCondition::PlaybackStoppedAndReleased => {
                let current = snapshot.view().timepoint();
                let visible_panels = visible_product_panels(&snapshot);
                let trace = self.active_playback_stop_trace.as_ref().ok_or_else(|| {
                    "playback stop assertion has no retained-front handoff trace".to_owned()
                })?;
                if let Some(violation) = trace.violation.as_ref() {
                    return Err(format!(
                        "playback stop handoff violation: {violation}; bounded_trace={}",
                        trace.json()
                    ));
                }
                let first = trace
                    .states
                    .first()
                    .map(|(_, state)| state)
                    .ok_or_else(|| {
                        "playback stop handoff trace contains no visible state".to_owned()
                    })?;
                for panel in &first.panels {
                    for layer in &panel.layers {
                        let expected = trace
                            .playback_layer_scales
                            .iter()
                            .find_map(|(ordinal, scale)| {
                                (*ordinal == layer.layer_ordinal).then_some(*scale)
                            })
                            .expect("stop trace layers come from the fixed playback scale map");
                        if layer.displayed_scale_level != Some(expected) || layer.mixed {
                            return Err(format!(
                                "playback stop trace did not begin at the fixed session scale for {} layer {}: expected s{}, observed {:?}",
                                panel.panel,
                                layer.layer_ordinal,
                                expected,
                                layer.displayed_scale_level
                            ));
                        }
                    }
                }
                if snapshot.transient().playback_active() {
                    return Err("playback remains active after stop".to_owned());
                }
                if !app
                    .dataset
                    .scope_requirements(crate::dataset_requests::SCOPE_PLAYBACK)
                    .is_empty()
                {
                    return Err("playback scope still owns resources after stop".to_owned());
                }
                if app
                    .dataset
                    .renderer_requirement_handle()
                    .requirements
                    .iter()
                    .any(|key| key.timepoint() != current)
                {
                    return Err(
                        "renderer authority retains a non-current timepoint after stop".to_owned(),
                    );
                }
                if visible_panels.iter().any(|panel| {
                    product_presentation(app, *panel)
                        .is_none_or(|frame| frame.timepoint() != current)
                }) {
                    return Err(format!(
                        "stopped playback did not retain a coherent current presentation at timepoint {}",
                        current.get()
                    ));
                }
                if !coordinated_visible_layout_current_complete_with_snapshot(app, &snapshot) {
                    return Err(
                        "playback Stop did not reach the direct stationary replacement".to_owned(),
                    );
                }
                Ok(())
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
        let layer_states = view
            .layers()
            .iter()
            .enumerate()
            .map(|(order_index, layer)| {
                let transfer = layer.transfer();
                json!({
                    "order_index": order_index,
                    "logical_layer": layer.layer_key().ordinal(),
                    "active": layer.layer_key() == view.active_layer(),
                    "visible": layer.visible(),
                    "render_mode": format!("{:?}", layer.render_state().mode()),
                    "sampling_policy": format!("{:?}", layer.render_state().sampling_policy()),
                    "transfer": {
                        "window": {
                            "low": transfer.window().low(),
                            "high": transfer.window().high(),
                        },
                        "color_rgb": transfer.color().rgb(),
                        "opacity": transfer.opacity().get(),
                        "gamma": transfer.curve().gamma_value(),
                        "invert": transfer.invert(),
                    },
                })
            })
            .collect::<Vec<_>>();
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
                "active_sampling_policy": format!("{:?}", view.layer(view.active_layer()).expect("active layer").render_state().sampling_policy()),
                "layers": layer_states,
                "projection": format!("{:?}", view.camera().projection()),
                "backend": format!("{:?}", app.render_coordination.frame_fidelity.backend),
                "pipeline_readiness": app
                    .native_presentation
                    .product_pipeline_readiness()
                    .map(|readiness| match readiness {
                        Ok(readiness) => format!("{readiness:?}"),
                        Err(error) => format!("Failed: {error}"),
                    }),
                "adapter": app.startup_diagnostics.gpu_adapter.clone(),
                "native_surface_configuration_contract": {
                    "present_mode": "AutoVsync",
                    "desired_maximum_frame_latency": 1,
                    "evidence_scope": "configured_product_contract_not_queried_compositor_observation",
                },
                "last_error": typed_render_error,
                "gpu_display_frame_present": product_presentation(app, PanelId::ThreeD).is_some(),
                "frame_fidelity": {
                    "target_scale_level": app.render_coordination.frame_fidelity.target_scale_level,
                    "displayed_scale_level": app.render_coordination.frame_fidelity.displayed_scale_level,
                    "logical_output_extent": {
                        "width": app.render_coordination.frame_fidelity.viewport.width_pixels(),
                        "height": app.render_coordination.frame_fidelity.viewport.height_pixels(),
                    },
                    "three_d_render_extent": {
                        "width": app.render_coordination.frame_fidelity.three_d_render_viewport.width_pixels(),
                        "height": app.render_coordination.frame_fidelity.three_d_render_viewport.height_pixels(),
                    },
                    "three_d_preview": app.render_coordination.frame_fidelity.three_d_preview,
                    "three_d_refinement_strips_completed": app.render_coordination.frame_fidelity.three_d_refinement_strips_completed,
                    "three_d_refinement_strips_total": app.render_coordination.frame_fidelity.three_d_refinement_strips_total,
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
                    "last_coordinated_cpu_timing": product.last_coordinated_cpu_timing.map(|timing| json!({
                        "planning_ns": timing.planning_ns(),
                        "queue_submit_ns": timing.queue_submit_ns(),
                    })),
                    "last_coordinated_recorded_targets": product
                        .last_coordinated_recorded_targets
                        .iter()
                        .map(|target| format!("{target:?}"))
                        .collect::<Vec<_>>(),
                    "last_coordinated_color_submissions": product.last_coordinated_color_submissions,
                    "total_coordinated_color_submissions": product.total_coordinated_color_submissions,
                    "last_coordinated_residency_submissions": product.last_coordinated_residency_submissions,
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
                        })).collect::<Vec<_>>(),
                    },
                })),
                "performance_milestones": display_performance_milestones_json(app),
                "display_coordination": display_coordination_diagnostics_json(app),
            },
            "dataset_demand": {
                "current_scale_level": app
                    .dataset
                    .current_uniform_scale()
                    .map(mirante4d_domain::ScaleLevel::get),
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
            "package_integrity_audit": package_integrity_audit_diagnostics_json(app),
            "retained_leases": retained_leases_diagnostics_json(app),
            "cross_section": cross_section_diagnostics_json(app),
            "gpu_adapter": app
                .native_presentation.product_gpu
                .as_ref()
                .map(|product| gpu_adapter_diagnostics_json(
                    product.renderer.diagnostics(),
                    &app.selected_adapter_memory,
                )),
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
        let observed_native_viewport_pixels = observed_native_viewport_pixels(ctx)
            .map(|[width, height]| json!({ "width": width, "height": height }))
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
                "observed_native_viewport_pixels": observed_native_viewport_pixels,
                "observed_native_viewport_scope": "egui_root_viewport_rect_at_report_close",
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
            "temporal_evidence": {
                "presented_transition_count": self.temporal_transition_count,
                "fixed_contract_transition_count": self.temporal_contract_transition_count,
                "last_coherent_time_index": self.last_coherent_temporal_timepoint.map(TimeIndex::get),
                "continuity_violation": &self.temporal_violation,
                "observations": &self.temporal_observations,
            },
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
        let artifact = self.capture_viewport_artifact(
            app,
            ProductAutomationPresentationTarget::ThreeD,
            Some("failure-final-frame"),
        )?;
        self.artifacts.push(artifact);
        Ok(())
    }
}

fn import_primary_measurement_json(measurement: Option<ImportPrimaryMeasurement>) -> Value {
    measurement.map_or(Value::Null, |measurement| {
        json!({
            "start_boundary": "accepted_start_import_command_immediately_before_worker_spawn",
            "end_boundary": "published_destination_admitted_and_open_ready_for_normal_product_use",
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
        "package_integrity_audit_started_runs": evidence.package_integrity_audit_started_runs,
        "package_integrity_audit_progress_updates": evidence.package_integrity_audit_progress_updates,
        "package_integrity_audit_cancelled_runs": evidence.package_integrity_audit_cancelled_runs,
        "package_integrity_audit_failed_runs": evidence.package_integrity_audit_failed_runs,
        "package_integrity_audit_completed_runs": evidence.package_integrity_audit_completed_runs,
        "start_boundary": "import_worker_published_event",
        "end_boundary": "published_destination_admitted_and_open_ready_for_normal_product_use",
        "wall_clock": "std_instant_monotonic",
        "cpu_clock": "process_cpu_time",
        "published_at_epoch_ms": measurement.published_at_epoch_ms,
        "open_ready_at_epoch_ms": measurement.open_ready_at_epoch_ms,
        "wall_time_ns": measurement.wall_time_ns,
        "process_cpu_time_ns": measurement.process_cpu_time_ns,
        "included_in_primary_clock": true,
        "transfer_mode": "staged_self_consistent_capability",
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
        "operation_token": evidence.token.as_ref().map(operation_token_json),
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
        "no_data_detection_native_decoded_bytes" => statistics.no_data_detection_native_decoded_bytes,
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
        "preflight_required_headroom_bytes" => statistics.preflight_required_headroom_bytes,
        "peak_temporary_bytes" => statistics.peak_temporary_bytes,
        "peak_checkpoint_regular_files" => statistics.peak_checkpoint_regular_files,
        "peak_working_bytes" => statistics.peak_working_bytes,
        "peak_process_rss_bytes" => statistics.peak_process_rss_bytes,
        "resumed_work_units" => statistics.resumed_work_units,
        "produced_work_units" => statistics.produced_work_units,
        "maximum_temporal_pipeline_width" => statistics.maximum_temporal_pipeline_width,
        "prefetch_units_admitted" => statistics.prefetch_units_admitted,
        "prefetch_units_consumed" => statistics.prefetch_units_consumed,
        "prefetch_cache_hits" => statistics.prefetch_cache_hits,
        "temporal_ingest_busy_time_ns" => statistics.temporal_ingest_busy_time_ns,
        "temporal_canonical_processing_time_ns" => statistics.temporal_canonical_processing_time_ns,
        "prefetch_ingest_busy_time_ns" => statistics.prefetch_ingest_busy_time_ns,
        "prefetch_overlap_time_ns" => statistics.prefetch_overlap_time_ns,
        "prefetch_cpu_capacity_deferrals" => statistics.prefetch_cpu_capacity_deferrals,
        "prefetch_disk_headroom_deferrals" => statistics.prefetch_disk_headroom_deferrals,
        "prefetch_queue_capacity_deferrals" => statistics.prefetch_queue_capacity_deferrals,
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
    let scale = app
        .dataset
        .scope_layer_scales(crate::dataset_requests::SCOPE_CURRENT_3D)?
        .get(&view.active_layer())
        .copied()?;
    Some(app.dataset.retained_leases().cohort_status(
        identity,
        view.active_layer(),
        view.timepoint(),
        scale,
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
    json!({
        "required": bridge.required_len(),
        "retained": bridge.retained_len(),
        "cpu_absent": bridge.missing_len(),
        "renderer_pending_residency_work": app
            .native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| product.renderer.has_pending_residency_work()),
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
    for (label, width, height, rgba) in images {
        let expected_bytes = width
            .checked_mul(*height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| format!("{image_group} {label} dimensions overflowed"))?;
        if *width == 0 || *height == 0 || rgba.len() != expected_bytes {
            return Err(format!(
                "{image_group} {label} has invalid RGBA dimensions: {width}x{height}, {} bytes",
                rgba.len()
            ));
        }
        if !rgba
            .chunks_exact(4)
            .any(|pixel| pixel[..3].iter().any(|channel| *channel != 0))
        {
            return Err(format!("{image_group} {label} is RGB-blank"));
        }
    }

    let mut compared_pairs = 0usize;
    for left_index in 0..images.len() {
        for right_index in (left_index + 1)..images.len() {
            let (left_label, left_width, left_height, left_rgba) = &images[left_index];
            let (right_label, right_width, right_height, right_rgba) = &images[right_index];
            compared_pairs += 1;
            let (different_pixels, comparison) =
                if left_width == right_width && left_height == right_height {
                    (
                        left_rgba
                            .chunks_exact(4)
                            .zip(right_rgba.chunks_exact(4))
                            .filter(|(left, right)| left[..3] != right[..3])
                            .count(),
                        "direct",
                    )
                } else {
                    (
                        count_normalized_different_pixels(
                            (*left_width, *left_height, left_rgba),
                            (*right_width, *right_height, right_rgba),
                        ),
                        "normalized-coordinate",
                    )
                };
            if different_pixels < min_different_pixels {
                return Err(format!(
                    "{image_group} {left_label} and {right_label} differ in {different_pixels} {comparison} pixels, expected at least {min_different_pixels}"
                ));
            }
        }
    }
    let expected_pairs = images.len().saturating_mul(images.len().saturating_sub(1)) / 2;
    if compared_pairs != expected_pairs || expected_pairs == 0 {
        return Err(format!(
            "{image_group} assertion compared {compared_pairs} pairs, expected {expected_pairs}"
        ));
    }
    Ok(())
}

fn count_normalized_different_pixels(
    left: (usize, usize, &[u8]),
    right: (usize, usize, &[u8]),
) -> usize {
    let sample_width = left.0.min(right.0);
    let sample_height = left.1.min(right.1);
    let mut different = 0usize;
    for sample_y in 0..sample_height {
        let left_y = normalized_sample_index(sample_y, sample_height, left.1);
        let right_y = normalized_sample_index(sample_y, sample_height, right.1);
        for sample_x in 0..sample_width {
            let left_x = normalized_sample_index(sample_x, sample_width, left.0);
            let right_x = normalized_sample_index(sample_x, sample_width, right.0);
            let left_offset = (left_y * left.0 + left_x) * 4;
            let right_offset = (right_y * right.0 + right_x) * 4;
            if left.2[left_offset..left_offset + 3] != right.2[right_offset..right_offset + 3] {
                different += 1;
            }
        }
    }
    different
}

fn normalized_sample_index(
    sample_index: usize,
    sample_extent: usize,
    image_extent: usize,
) -> usize {
    debug_assert!(sample_extent > 0);
    debug_assert!(image_extent > 0);
    let numerator = (sample_index as u128 * 2 + 1) * image_extent as u128;
    let denominator = sample_extent as u128 * 2;
    usize::try_from(numerator / denominator)
        .unwrap_or(usize::MAX)
        .min(image_extent - 1)
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
    let active_presentations = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .filter(|panel| product_presentation(app, *panel).is_some())
        .count();
    if active_presentations != 0 {
        return Err(format!(
            "cross-section display frames are still active: {}",
            active_presentations
        ));
    }
    let retained_native_bindings = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .filter(|panel| {
            app.native_presentation
                .texture_id(panel.presentation_slot())
                .is_some()
        })
        .count();
    if retained_native_bindings != 0 {
        return Err(format!(
            "retired cross-section native texture bindings are still alive: {retained_native_bindings}"
        ));
    }
    Ok(())
}

fn timing_samples_json(samples: &mirante4d_application::DisplayTimingSamples) -> Value {
    let mut retained = (0..samples.retained_count())
        .filter_map(|index| samples.sample(index))
        .collect::<Vec<_>>();
    retained.sort_unstable();
    let median_ns = (!retained.is_empty()).then(|| {
        let upper = retained.len() / 2;
        if retained.len() % 2 == 0 {
            retained[upper - 1].saturating_add(retained[upper]) / 2
        } else {
            retained[upper]
        }
    });
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
        "median_ns": median_ns,
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
    let planner = app.dataset.visible_demand_diagnostics();
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
                "render_payload_bytes": plan.render_payload_bytes,
                "planned_payload_bytes": plan.planned_payload_bytes,
                "primary_resource_count": plan.primary_resource_count,
                "total_requirements": plan.requirements.body().canonical().len(),
                "resident_guard_resource_count": plan
                    .requirements
                    .body()
                    .canonical()
                    .len()
                    .saturating_sub(plan.primary_resource_count),
                "plane_reuse_guard_present": plan.plane_reuse_envelope.is_some(),
                "payload_fact": "exact_semantic_planned_payload_for_this_requirement_body",
            })
        })
        .collect::<Vec<_>>();
    let navigation_candidates = app
        .volume_presentation
        .latest_candidate_facts()
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            json!({
                "index": index,
                "kind": candidate.kind.label(),
                "kernel_class": candidate.kernel.label(),
                "layer_scales": candidate.layer_scales.iter().map(|(layer, scale)| json!({
                    "layer_ordinal": layer.ordinal(),
                    "scale_level": scale.get(),
                })).collect::<Vec<_>>(),
                "shared_work_units": candidate.shared_work_units,
                "layer_work": candidate.layer_work.iter().map(|layer| json!({
                    "layer_ordinal": layer.layer.ordinal(),
                    "scale_level": layer.scale.get(),
                    "mode": format!("{:?}", layer.mode),
                    "sampling": format!("{:?}", layer.sampling),
                    "projected_pixels": layer.projected_pixels,
                    "traversal_step_bound": layer.traversal_step_bound,
                    "scheduled_pixels": layer.scheduled_pixels,
                    "scheduled_step_bound": layer.scheduled_step_bound,
                    "sample_taps_per_step": layer.sample_taps_per_step,
                    "gradient_taps_per_ray": layer.gradient_taps_per_ray,
                    "ray_setup_work_units": layer.ray_setup_work_units,
                    "scheduled_work_units": layer.scheduled_work_units,
                    "terminal_work_units": layer.terminal_work_units,
                    "total_work_units": layer.total_work_units(),
                })).collect::<Vec<_>>(),
                "resource_count": candidate.resource_count,
                "payload_bytes": candidate.payload_bytes,
                "schedule_work_units": candidate.schedule_work_units,
                "complete_and_resident": candidate.complete_and_resident,
                "target_quality_eligible": candidate.target_quality_eligible,
                "interaction_safe": candidate.interaction_safe,
                "full_volume": candidate.full_volume,
                "disposition": candidate.disposition.label(),
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
        "completed_guarded_rebuilds": planner.completed_guarded_rebuilds,
        "completed_exact_rebuilds": planner.completed_exact_rebuilds,
        "resident_plane_guard_reuses": app.resident_plane_guard_reuses,
        "resident_plane_async_plan_submissions": app.resident_plane_async_plan_submissions,
        "resident_plane_guard_body_installs": app.resident_plane_guard_body_installs,
        "resident_plane_exact_body_installs": app.resident_plane_exact_body_installs,
        "three_d_preview_candidates": navigation_candidates,
        "scopes": scopes,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefinementHandoffPhase {
    Inactive,
    AwaitingCoordinatedExactPresentation,
}

impl RefinementHandoffPhase {
    const fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::AwaitingCoordinatedExactPresentation => "awaiting_coordinated_exact_presentation",
        }
    }
}

const fn refinement_handoff_phase(staged_plan_installed: bool) -> RefinementHandoffPhase {
    if staged_plan_installed {
        RefinementHandoffPhase::AwaitingCoordinatedExactPresentation
    } else {
        RefinementHandoffPhase::Inactive
    }
}

fn refinement_handoff_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let staged_plan_installed = app.dataset.staging_current_refinement();
    let phase = refinement_handoff_phase(staged_plan_installed);
    let coordinated_requires_execution =
        app.native_presentation
            .product_gpu
            .as_ref()
            .and_then(|product| {
                product
                    .renderer
                    .coordinated_target_requires_execution(PresentationSlot::ThreeD)
                    .ok()
            });
    let visible = product_presentation(app, PanelId::ThreeD);
    json!({
        "phase": phase.name(),
        "staged_plan_installed": staged_plan_installed,
        "candidate_owner": "render_wgpu_fixed_target_exact_frame_only",
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
        "visible_demand_planner_active": app.dataset.visible_demand_plan_outstanding(),
        "coordinated_target_requires_execution": coordinated_requires_execution,
        "renderer_pending_residency_work": app
            .native_presentation
            .product_gpu
            .as_ref()
            .is_some_and(|product| product.renderer.has_pending_residency_work()),
        "visible_3d_presentation_completeness": visible
            .map(|frame| format!("{:?}", frame.progress().completeness())),
    })
}

fn package_integrity_audit_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = app.application.snapshot();
    let state = match snapshot.source().integrity_audit() {
        PackageIntegrityAuditSnapshot::NotRun => "NotRun",
        PackageIntegrityAuditSnapshot::Running { .. } => "Running",
        PackageIntegrityAuditSnapshot::SelfConsistent(_) => "SelfConsistent",
        PackageIntegrityAuditSnapshot::Failed(_) => "Failed",
        PackageIntegrityAuditSnapshot::Cancelled => "Cancelled",
    };
    let Some(service) = app.package_integrity_audit_service.as_ref() else {
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
            "progress_updates": diagnostics.progress_updates,
            "cancelled_runs": diagnostics.cancelled_runs,
            "failed_runs": diagnostics.failed_runs,
            "completed_runs": diagnostics.completed_runs,
        },
    })
}

fn display_coordination_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let generation = app.render_coordination.display_generation();
    let coordinated = app
        .render_coordination
        .coordinated_publication_diagnostics();
    let measurement_valid = coordinated.samples_complete()
        && coordinated.window_well_formed()
        && coordinated.window_started_at_ns().is_some()
        && coordinated.window_ended_at_ns().is_some()
        && coordinated.active_publication_count() > 0;
    json!({
        "instrumentation_epoch": "app_private_monotonic_epoch",
        "input_generation": generation.input_generation,
        "full_layout_current_presentation_generation": generation.current_presentation_generation,
        "full_layout_presentation_generation_gap": generation.presentation_generation_gap(),
        "main_loop_heartbeat": generation.main_loop_heartbeat,
        "input_generation_at_ns": generation.input_generation_at_ns,
        "full_layout_current_presentation_at_ns": generation.current_presentation_at_ns,
        "coordinated_publication": {
            "required_group": coordinated.required_group().name(),
            "current_presentation_generation": coordinated.current_presentation_generation(),
            "current_presentation_at_ns": coordinated.current_presentation_at_ns(),
            "presentation_generation_gap": generation.input_generation.saturating_sub(
                coordinated.current_presentation_generation().unwrap_or_default()
            ),
            "total_current_publications": coordinated.total_current_publications(),
            "total_superseded_before_current_publication":
                coordinated.total_superseded_before_current_publication(),
            "window_group": coordinated.window_group().map(CoordinatedPresentationGroup::name),
            "window_started_at_ns": coordinated.window_started_at_ns(),
            "window_ended_at_ns": coordinated.window_ended_at_ns(),
            "window_active": coordinated.window_active(),
            "final_input_generation": coordinated.final_input_generation(),
            "final_input_is_current": coordinated.final_input_generation().is_some()
                && coordinated.final_input_generation()
                    == coordinated.current_presentation_generation(),
            "active_publication_count": coordinated.active_publication_count(),
            "maximum_active_publication_gap_ns":
                coordinated.maximum_active_publication_gap_ns(),
            "window_group_mismatches": coordinated.window_group_mismatches(),
            "window_transition_errors": coordinated.window_transition_errors(),
            "samples_complete": coordinated.samples_complete(),
            "window_well_formed": coordinated.window_well_formed(),
            "measurement_valid": measurement_valid,
            "zero_active_publications_is_failure": true,
            "admission_to_current_latency": timing_samples_json(
                coordinated.admission_latency_samples(),
            ),
            "active_group_publication_interval": timing_samples_json(
                coordinated.publication_interval_samples(),
            ),
            "active_gap_scope":
                "gesture_start_or_previous_current_group_publication_to_next_publication_or_gesture_end",
        },
        "raw_main_loop_gap_ns": {
            "current": generation.raw_current_main_loop_heartbeat_gap_ns,
            "maximum": generation.raw_maximum_main_loop_heartbeat_gap_ns,
            "scope": "diagnostic_callback_spacing_includes_settled_event_driven_idle",
        },
        "durable_gesture_commits": generation.durable_gesture_commits,
        "full_layout_settlement_latency": timing_samples_json(
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
    let demand_currentness = app.visible_demand_plan_currentness();
    let demand_renderability = app.visible_demand_renderability();
    let canonical_cross_section = *view.cross_section();
    let cross_section_state = CrossSectionViewState::from_canonical(canonical_cross_section);
    let panels = app
        .render_coordination
        .iter()
        .map(|(slot, panel)| {
            let panel_id = PanelId::from_presentation_slot(slot);
            let display_current = if slot == PresentationSlot::ThreeD {
                panel.display_current()
                    && app.render_coordination.frame_fidelity.display_freshness
                        == DisplayedFrameFreshness::Current
                    && demand_currentness.current_3d
            } else {
                panel.display_current() && demand_currentness.cross_section(panel_id)
            };
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
            let demand_scope = match panel_id {
                PanelId::Xy => Some(crate::dataset_requests::SCOPE_CROSS_SECTION_XY),
                PanelId::Xz => Some(crate::dataset_requests::SCOPE_CROSS_SECTION_XZ),
                PanelId::Yz => Some(crate::dataset_requests::SCOPE_CROSS_SECTION_YZ),
                PanelId::ThreeD => None,
            };
            let installed_layer_scales = demand_scope
                .and_then(|scope| app.dataset.scope_layer_scales(scope))
                .map(|scales| {
                    scales
                        .iter()
                        .map(|(layer, scale)| {
                            json!({
                                "layer_ordinal": layer.ordinal(),
                                "scale_level": scale.get(),
                            })
                        })
                        .collect::<Vec<_>>()
                });
            let ideal_layer_scales = panel_id.cross_section_panel().and_then(|_| {
                Some(crate::dataset_demand_plan::cross_section_projected_layer_scales(
                    snapshot.catalog(),
                    view,
                    panel_id,
                    panel.presentation_viewport()?,
                    panel.render_viewport()?,
                ))
            })
            .and_then(Result::ok)
            .map(|scales| {
                scales
                    .iter()
                    .map(|(layer, scale)| {
                        json!({
                            "layer_ordinal": layer.ordinal(),
                            "scale_level": scale.get(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            json!({
                "panel_id": panel_id.label(),
                "canonical_plane_geometry": canonical_plane,
                "generation": panel.generation(),
                "displayed_generation": panel.displayed_generation(),
                "display_current": display_current,
                "exact_demand_current": if panel_id == PanelId::ThreeD {
                    demand_currentness.current_3d
                } else {
                    demand_currentness.cross_section(panel_id)
                },
                "resident_interaction_renderable": panel_id != PanelId::ThreeD
                    && demand_renderability.cross_section(panel_id)
                    && !demand_currentness.cross_section(panel_id),
                "ideal_layer_scales": ideal_layer_scales,
                "installed_selected_layer_scales": installed_layer_scales,
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
                    "target_scale_level": layer.target_scale_level,
                    "finest_fallback_scale_level": layer.finest_fallback_scale_level,
                    "fallback_scale_level": layer.fallback_scale_level,
                    "target_available_requirements": layer.target_available_requirements,
                    "target_total_requirements": layer.target_total_requirements,
                    "available_requirements": layer.available_requirements,
                    "total_requirements": layer.total_requirements,
                    "mixed": layer.mixed,
                    "current": layer.current,
                })).collect::<Vec<_>>(),
                "display_frame": product_presentation(app, panel_id)
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
        "schema_version": 2,
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
        ProductAutomationProjectStoreLifecycle::Provisional => ProjectStoreLifecycle::Provisional,
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
