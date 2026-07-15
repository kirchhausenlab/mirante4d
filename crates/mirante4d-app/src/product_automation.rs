use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, CommandEffect, ProjectStoreLifecycle,
    SourceVerificationSnapshot, WorkspaceSnapshot,
    viewport_interaction::{
        fit_camera_to_shape_preserving_view, orbit_camera, pan_camera, zoom_camera,
    },
};
use mirante4d_domain::{
    DisplayWindow, DvrOpacityTransfer, IsoShadingPolicy, LayerTransfer, Opacity, RenderMode,
    RenderState, SamplingPolicy, ViewerLayout,
};
use mirante4d_project_model::{LayerViewState, ProjectRevisionId};
use mirante4d_render_api::RenderExtent;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    DVR_DENSITY_SCALE_MAX, DVR_DENSITY_SCALE_MIN, DisplayedFrameFreshness, FrameCompleteness,
    MiranteWorkbenchApp, application_view, set_render_viewport, viewer_layout::PanelId,
};

mod capture;
mod diagnostics;
mod model;

use capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, capture_color_image,
    current_display_image_stats, product_target_capture, sanitize_artifact_label,
    write_color_image_ppm,
};
use diagnostics::{dataset_runtime_diagnostics_json, gpu_adapter_diagnostics_json};
use model::*;

const ENABLE_AUTOMATION_ENV: &str = "MIRANTE4D_ENABLE_AUTOMATION";
const AUTOMATION_SCRIPT_ENV: &str = "MIRANTE4D_AUTOMATION_SCRIPT";
const AUTOMATION_REPORT_ENV: &str = "MIRANTE4D_AUTOMATION_REPORT";

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
const AUTOMATION_SCHEMA_VERSION: u32 = 2;

fn dispatch_application_command(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    command: ApplicationCommand,
) -> Result<(), String> {
    app.apply_application_command(command, ctx)
        .map(|_| ())
        .map_err(|fault| format!("application command was rejected: {fault:?}"))
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
    let sampling = SamplingPolicy::VoxelExact;
    match mode {
        RenderMode::Mip => Ok(RenderState::mip(sampling)),
        RenderMode::Isosurface => {
            let level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(0.5);
            RenderState::iso(sampling, IsoShadingPolicy::Flat, level)
                .map_err(|error| error.to_string())
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

pub(crate) struct ProductAutomationController {
    script: ProductAutomationScript,
    script_path: PathBuf,
    report_path: PathBuf,
    command_index: usize,
    active_wait_started: Option<Instant>,
    sleep_frames_remaining: Option<u32>,
    started_at_epoch_ms: u128,
    started_at: Instant,
    events: Vec<ProductAutomationEvent>,
    diagnostics: Vec<Value>,
    artifacts: Vec<ProductAutomationArtifact>,
    limit_observations: ProductAutomationLimitObservations,
    render_target_override: Option<RenderExtent>,
    requested_mapped_client_pixels: Option<(u32, u32)>,
    report_written: bool,
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
        match automation.step(app, ctx) {
            AutomationStatus::Continue => {
                ctx.request_repaint();
            }
            AutomationStatus::Waiting { repaint_after } => {
                if let Some(delay) = repaint_after {
                    ctx.request_repaint_after(delay);
                }
            }
            AutomationStatus::Finished => {
                automation.write_report_and_close(app, ctx, "passed", None);
            }
            AutomationStatus::Failed(reason) => {
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
        Ok(Self::new(script, script_path, report_path))
    }

    fn new(script: ProductAutomationScript, script_path: PathBuf, report_path: PathBuf) -> Self {
        Self {
            script,
            script_path,
            report_path,
            command_index: 0,
            active_wait_started: None,
            sleep_frames_remaining: None,
            started_at_epoch_ms: epoch_ms(),
            started_at: Instant::now(),
            events: Vec::new(),
            diagnostics: Vec::new(),
            artifacts: Vec::new(),
            limit_observations: ProductAutomationLimitObservations::default(),
            render_target_override: None,
            requested_mapped_client_pixels: None,
            report_written: false,
        }
    }

    pub(crate) const fn render_target_override(&self) -> Option<RenderExtent> {
        self.render_target_override
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
        let command_started = Instant::now();
        let result = self.execute_command(app, ctx, &command);
        if let Err(reason) = self.observe_and_enforce_limits(app) {
            self.events.push(ProductAutomationEvent::failed(
                command_index,
                command.name(),
                command_started.elapsed(),
                reason.clone(),
            ));
            return AutomationStatus::Failed(reason);
        }
        match result {
            Ok(CommandProgress::Done(details)) => {
                self.events.push(ProductAutomationEvent::passed(
                    command_index,
                    command.name(),
                    command_started.elapsed(),
                    details,
                ));
                self.active_wait_started = None;
                self.sleep_frames_remaining = None;
                if self.command_index == command_index {
                    self.command_index += 1;
                }
                AutomationStatus::Continue
            }
            Ok(CommandProgress::Waiting) => AutomationStatus::Waiting {
                repaint_after: Some(Duration::from_millis(16)),
            },
            Ok(CommandProgress::PassiveWaiting(repaint_after)) => {
                AutomationStatus::Waiting { repaint_after }
            }
            Err(reason) => {
                self.events.push(ProductAutomationEvent::failed(
                    command_index,
                    command.name(),
                    command_started.elapsed(),
                    reason.clone(),
                ));
                AutomationStatus::Failed(reason)
            }
        }
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
            ProductAutomationCommand::ProbeHover {
                x_fraction,
                y_fraction,
            } => self.probe_hover(app, *x_fraction, *y_fraction),
            ProductAutomationCommand::CopyDiagnostics => {
                let diagnostics = self.diagnostics_json(app);
                self.diagnostics.push(diagnostics.clone());
                Ok(CommandProgress::Done(diagnostics))
            }
            ProductAutomationCommand::CaptureScreenshot { name } => {
                if !product_presentations_ready(app, &[PanelId::ThreeD])? {
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

    fn probe_hover(
        &self,
        app: &mut MiranteWorkbenchApp,
        x_fraction: f32,
        y_fraction: f32,
    ) -> Result<CommandProgress, String> {
        if !x_fraction.is_finite()
            || !y_fraction.is_finite()
            || !(0.0..=1.0).contains(&x_fraction)
            || !(0.0..=1.0).contains(&y_fraction)
        {
            return Err("probe_hover fractions must be finite and between 0.0 and 1.0".to_owned());
        }
        app.egui_ui.hovered_pixel = None;
        app.egui_ui.hovered_source_readout = None;
        Ok(CommandProgress::Done(json!({
            "x_fraction": x_fraction,
            "y_fraction": y_fraction,
            "status": "unavailable",
            "reason": "3D scientific intensity probing is unavailable on the current GPU presentation path",
            "placeholder_sampled": false,
        })))
    }

    fn wait_condition_met(
        &self,
        app: &MiranteWorkbenchApp,
        condition: ProductAutomationWaitCondition,
    ) -> bool {
        let snapshot = app.application.snapshot();
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
                !crate::workbench_playback_runtime::background_work_active(
                    &snapshot,
                    &app.import.workers,
                    &app.dataset,
                    &app.render_coordination,
                    &app.native_presentation,
                )
            }
            ProductAutomationWaitCondition::FrameFreshnessCurrent => {
                app.render_coordination.frame_fidelity.display_freshness
                    == DisplayedFrameFreshness::Current
                    || matches!(
                        app.render_coordination.frame_fidelity.completeness,
                        FrameCompleteness::Exact | FrameCompleteness::Complete
                    )
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
            "render": {
                "active_render_mode": format!("{:?}", view.layer(view.active_layer()).expect("active layer").render_state().mode()),
                "projection": format!("{:?}", view.camera().projection()),
                "backend": format!("{:?}", app.render_coordination.frame_fidelity.backend),
                "adapter": app.startup_diagnostics.gpu_adapter.clone(),
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
                })),
            },
            "dataset_demand": {
                "current_scale_level": app.dataset.current_scale().get(),
                "last_plan_error": app.dataset.last_plan_error(),
                "dispatcher_pending": app.dataset.dispatcher().has_pending_work(),
                "last_fault": app.dataset.dispatcher().last_fault().map(|fault| fault.to_string()),
            },
            "dataset_runtime": app
                .dataset
                .dispatcher()
                .diagnostics()
                .ok()
                .map(dataset_runtime_diagnostics_json),
            "retained_leases": retained_leases_diagnostics_json(app),
            "cross_section": cross_section_diagnostics_json(app),
            "gpu_adapter": app
                .native_presentation.product_gpu
                .as_ref()
                .map(|product| gpu_adapter_diagnostics_json(product.renderer.diagnostics())),
            "camera": {
                "projection": format!("{:?}", view.camera().projection()),
                "viewport": {
                    "width": app.render_coordination.render_viewport.width_pixels(),
                    "height": app.render_coordination.render_viewport.height_pixels(),
                },
            },
            "project_state": project_state_json(app),
            "project_store_evidence": project_store_evidence_json(app),
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
        let report = json!({
            "schema": AUTOMATION_REPORT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
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
            "binary": env::current_exe().ok().map(|path| path.display().to_string()),
            "script": {
                "path": self.script_path.display().to_string(),
                "schema": self.script.schema.clone(),
                "schema_version": self.script.schema_version,
                "scenario": self.script.scenario.clone(),
                "command_count": self.script.commands.len(),
            },
            "limits": self.script.limits,
            "limit_observations": self.limit_observations.json(),
            "dataset": {
                "path": app.dataset.selected_path().display().to_string(),
                "name": snapshot.catalog().label(),
            },
            "project_state": project_state_json(app),
            "project_store_evidence": project_store_evidence_json(app),
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

    fn observe_and_enforce_limits(&mut self, app: &MiranteWorkbenchApp) -> Result<(), String> {
        let limits = self.script.limits;
        let diagnostics =
            app.dataset.dispatcher().diagnostics().map_err(|err| {
                format!("failed to read unified dataset runtime diagnostics: {err}")
            })?;
        self.limit_observations.observe_dataset_runtime(diagnostics);
        limits.check_dataset_runtime(diagnostics)?;
        Ok(())
    }

    fn capture_failure_artifact(&mut self, app: &mut MiranteWorkbenchApp) -> Result<(), String> {
        let artifact = self.capture_viewport_artifact(app, Some("failure-final-frame"))?;
        self.artifacts.push(artifact);
        Ok(())
    }
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
    json!({
        "required": bridge.required_len(),
        "retained": bridge.retained_len(),
        "missing": bridge.missing_len(),
        "complete": bridge.is_complete(),
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

fn cross_section_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = app.application.snapshot();
    let view = application_view(&snapshot);
    let panels = app
        .render_coordination
        .iter()
        .map(|(slot, panel)| {
            let panel_id = PanelId::from_presentation_slot(slot);
            json!({
                "panel_id": panel_id.label(),
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
    let (current_revision, saved_revision) = match snapshot.workspace() {
        WorkspaceSnapshot::Bound {
            revision,
            saved_revision,
            ..
        } => (Some(*revision), *saved_revision),
        WorkspaceSnapshot::Unbound { .. } => (None, None),
    };
    let status = app.project_store.as_ref().map(|service| service.status());
    let facts = project_state_facts(app);
    json!({
        "bound": facts.bound,
        "dirty": facts.dirty,
        "current_revision": project_revision_json(current_revision),
        "saved_revision": project_revision_json(saved_revision),
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
