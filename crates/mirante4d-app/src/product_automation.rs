use std::{
    collections::BTreeSet,
    env, fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use mirante4d_application::{ApplicationCommand, CrossSectionPanelId};
use mirante4d_data::DataEngineStats;
use mirante4d_domain::{
    CrossSectionView as CanonicalCrossSectionView, DisplayWindow, DvrOpacityTransfer,
    IsoShadingPolicy, LayerTransfer, Opacity, RenderMode, RenderState, TimeIndex, UnitQuaternion,
    ViewerLayout, WorldPoint3,
};
use mirante4d_project_model::LayerViewState;
use mirante4d_render_api::PresentationViewport;
use mirante4d_renderer::gpu::{GpuDisplayFrame, GpuRenderer};
use serde::Serialize;
use serde_json::{Value, json};

use crate::cross_section_readout::{
    CrossSectionHoverGenerationStatus, CrossSectionHoverReadout, CrossSectionHoverStatus,
    CrossSectionHoverValue, CrossSectionReadoutInput, cross_section_hover_readout_for_panel_point,
};
use crate::cross_section_runtime::{CrossSectionChunkPayload, CrossSectionChunkState};
use crate::display_refresh::DisplayRefreshTiming;
use crate::tool_interactions::{apply_viewer_tool_commands, pick_hit_from_viewport_hover};
use crate::tools::ViewerToolEvent;
use crate::viewport::viewport_hover_from_normalized_point;
use crate::{
    DVR_DENSITY_SCALE_MAX, DVR_DENSITY_SCALE_MIN, DisplayedFrameFreshness, FrameCompleteness,
    MiranteWorkbenchApp, application_view,
    cross_section_scheduler::cross_section_interaction_recent,
    current_egui_shell_bridge, current_physical_layer_id,
    viewer_layout::{PanelId, render_cross_section_view_state},
};

mod capture;
mod diagnostics;
mod model;
mod picking;
mod timing;

use capture::{
    ProductAutomationArtifact, ProductAutomationImageStats, capture_color_image,
    color_image_from_rgba, current_display_image_stats, sanitize_artifact_label,
    write_color_image_ppm,
};
use diagnostics::{
    brick_queue_diagnostics_json, data_engine_diagnostics_json, gpu_adapter_diagnostics_json,
    gpu_renderer_stats_json, gpu_timestamp_timing_json,
};
use model::*;
use picking::{pick_hit_json, viewport_hover_json};
use timing::{
    ProductAutomationAppUpdatePhases, ProductAutomationAppUpdateSample,
    ProductAutomationCrossSectionLatencySample, ProductAutomationDisplayRefreshSample,
    ProductAutomationInputToPresentSample, app_update_timing_summary_json,
    cross_section_latency_summary_json, details_with_display_refresh_timing,
    display_refresh_timing_json, display_refresh_timing_summary_json,
    input_to_present_timing_summary_json, new_display_refresh_timing_from_details,
    presentation_timing_json,
};

const ENABLE_AUTOMATION_ENV: &str = "MIRANTE4D_ENABLE_AUTOMATION";
const AUTOMATION_SCRIPT_ENV: &str = "MIRANTE4D_AUTOMATION_SCRIPT";
const AUTOMATION_REPORT_ENV: &str = "MIRANTE4D_AUTOMATION_REPORT";
const AUTOMATION_SCRIPT_SCHEMA: &str = "mirante4d-product-automation-script";
const AUTOMATION_REPORT_SCHEMA: &str = "mirante4d-product-automation-report";
const AUTOMATION_SCHEMA_VERSION: u32 = 1;

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
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let layer = application_view(&snapshot)
        .layers()
        .get(layer_index)
        .ok_or_else(|| format!("layer index {layer_index} is out of range"))?;
    Ok(ApplicationCommand::SetLayerView(update(layer)?))
}

fn active_layer_index(app: &MiranteWorkbenchApp) -> usize {
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
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
            let (shading, level) = current
                .iso_parameters()
                .map(|parameters| (parameters.shading_policy(), parameters.display_level()))
                .unwrap_or((IsoShadingPolicy::GradientLighting, 0.5));
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

fn application_cross_section_panel_id(panel_id: PanelId) -> Option<CrossSectionPanelId> {
    match panel_id {
        PanelId::Xy => Some(CrossSectionPanelId::Xy),
        PanelId::Xz => Some(CrossSectionPanelId::Xz),
        PanelId::Yz => Some(CrossSectionPanelId::Yz),
        PanelId::ThreeD => None,
    }
}

fn canonical_cross_section_view(
    runtime: mirante4d_renderer::CrossSectionViewState,
) -> Result<CanonicalCrossSectionView, String> {
    let [x, y, z] = runtime.center_world.to_array();
    let [qx, qy, qz, qw] = runtime.orientation.to_array();
    CanonicalCrossSectionView::new(
        WorldPoint3::new(x, y, z).map_err(|error| error.to_string())?,
        UnitQuaternion::new_xyzw(qx, qy, qz, qw).map_err(|error| error.to_string())?,
        runtime.scale_world_per_screen_point,
        runtime.depth_world,
    )
    .map_err(|error| error.to_string())
}

fn apply_cross_section_edit(
    app: &mut MiranteWorkbenchApp,
    ctx: &egui::Context,
    panel_id: PanelId,
    edit: impl FnOnce(
        &mut mirante4d_renderer::CrossSectionViewState,
        mirante4d_renderer::CrossSectionPanel,
    ),
) -> Result<(), String> {
    let application_panel = application_cross_section_panel_id(panel_id)
        .ok_or_else(|| "3D is not a cross-section panel".to_owned())?;
    dispatch_application_command(
        app,
        ctx,
        ApplicationCommand::SetActiveCrossSectionPanel(Some(application_panel)),
    )?;
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let view = application_view(&snapshot);
    let mut cross_section = render_cross_section_view_state(*view.cross_section());
    edit(
        &mut cross_section,
        panel_id
            .cross_section_panel()
            .expect("validated cross-section panel"),
    );
    let layout = view.layout();
    let cross_section = canonical_cross_section_view(cross_section)?;
    app.dataset_runtime.cross_section_last_interaction_at = Some(Instant::now());
    dispatch_application_command(
        app,
        ctx,
        ApplicationCommand::SetLayout {
            layout,
            cross_section,
        },
    )?;
    ctx.request_repaint_after(crate::CROSS_SECTION_INTERACTION_SETTLE_DURATION);
    Ok(())
}

pub(crate) struct ProductAutomationController {
    script: ProductAutomationScript,
    script_path: PathBuf,
    report_path: PathBuf,
    command_index: usize,
    active_wait_started: Option<Instant>,
    sleep_started: Option<Instant>,
    sleep_frames_remaining: Option<u32>,
    started_at_epoch_ms: u128,
    started_at: Instant,
    events: Vec<ProductAutomationEvent>,
    diagnostics: Vec<Value>,
    artifacts: Vec<ProductAutomationArtifact>,
    app_update_samples: Vec<ProductAutomationAppUpdateSample>,
    display_refresh_samples: Vec<ProductAutomationDisplayRefreshSample>,
    input_to_present_samples: Vec<ProductAutomationInputToPresentSample>,
    cross_section_latency_samples: Vec<ProductAutomationCrossSectionLatencySample>,
    pending_cross_section_latency_samples: Vec<PendingCrossSectionLatencySample>,
    limit_observations: ProductAutomationLimitObservations,
    report_written: bool,
}

#[derive(Debug)]
struct PendingCrossSectionLatencySample {
    command_index: usize,
    command: &'static str,
    operation: &'static str,
    panel_id: PanelId,
    started_at: Instant,
    target_generation: u64,
    active_timepoint: u64,
}

impl PendingCrossSectionLatencySample {
    fn json(&self) -> Value {
        json!({
            "kind": "pending_cross_section_command_to_current_partial_latency",
            "taxonomy_version": 1,
            "command_index": self.command_index,
            "command": self.command,
            "operation": self.operation,
            "panel": self.panel_id.label(),
            "target_generation": self.target_generation,
            "active_timepoint": self.active_timepoint,
            "elapsed_ms": duration_ms(self.started_at.elapsed()),
        })
    }

    fn completed_sample(
        &self,
        app: &MiranteWorkbenchApp,
    ) -> Option<ProductAutomationCrossSectionLatencySample> {
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        if application_view(&snapshot).timepoint().get() != self.active_timepoint {
            return None;
        }
        let panel = app
            .render_runtime
            .cross_section_runtime
            .panel(self.panel_id)?;
        let displayed_generation = panel.displayed_generation?;
        if displayed_generation < self.target_generation {
            return None;
        }
        let displayed_frame = app
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&self.panel_id)?;
        if displayed_frame.generation < self.target_generation {
            return None;
        }
        let schedule = panel.cross_section_schedule;
        Some(ProductAutomationCrossSectionLatencySample {
            command_index: self.command_index,
            command: self.command,
            operation: self.operation,
            panel_id: self.panel_id,
            event_epoch_ms: epoch_ms(),
            latency_ms: duration_ms(self.started_at.elapsed()),
            target_generation: self.target_generation,
            displayed_generation,
            active_timepoint: self.active_timepoint,
            target_scale_level: schedule.and_then(|schedule| schedule.target_scale_level),
            render_scale_level: schedule.and_then(|schedule| schedule.render_scale_level),
            missing_occupied_chunks: schedule
                .map_or(0, |schedule| schedule.missing_occupied_bricks),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProductAutomationAppUpdateTiming {
    pub(crate) update_started: Instant,
    pub(crate) setup_ms: f64,
    pub(crate) task_drain_ms: f64,
    pub(crate) playback_ms: f64,
    pub(crate) ui_build_ms: f64,
    pub(crate) histogram_ui_ms: f64,
    pub(crate) command_apply_ms: f64,
    pub(crate) display_refresh_trigger_ms: f64,
    pub(crate) import_action_ms: f64,
    pub(crate) brick_result_drain_ms: f64,
    pub(crate) background_repaint_request_ms: f64,
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

    pub(crate) fn drive(
        app: &mut MiranteWorkbenchApp,
        ctx: &egui::Context,
        update_timing: ProductAutomationAppUpdateTiming,
    ) {
        let Some(mut automation) = app.validation_runtime.product_automation.take() else {
            return;
        };
        let automation_started = Instant::now();
        match automation.step(app, ctx) {
            AutomationStatus::Continue => {
                automation.record_app_update_sample(
                    app,
                    update_timing,
                    duration_ms(automation_started.elapsed()),
                );
                ctx.request_repaint();
            }
            AutomationStatus::Waiting => {
                automation.record_app_update_sample(
                    app,
                    update_timing,
                    duration_ms(automation_started.elapsed()),
                );
                ctx.request_repaint_after(Duration::from_millis(16));
            }
            AutomationStatus::Finished => {
                automation.record_app_update_sample(
                    app,
                    update_timing,
                    duration_ms(automation_started.elapsed()),
                );
                automation.write_report_and_close(app, ctx, "passed", None);
            }
            AutomationStatus::Failed(reason) => {
                automation.record_app_update_sample(
                    app,
                    update_timing,
                    duration_ms(automation_started.elapsed()),
                );
                automation.write_report_and_close(app, ctx, "failed", Some(reason));
            }
        }
        app.validation_runtime.product_automation = Some(automation);
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
            sleep_started: None,
            sleep_frames_remaining: None,
            started_at_epoch_ms: epoch_ms(),
            started_at: Instant::now(),
            events: Vec::new(),
            diagnostics: Vec::new(),
            artifacts: Vec::new(),
            app_update_samples: Vec::new(),
            display_refresh_samples: Vec::new(),
            input_to_present_samples: Vec::new(),
            cross_section_latency_samples: Vec::new(),
            pending_cross_section_latency_samples: Vec::new(),
            limit_observations: ProductAutomationLimitObservations::default(),
            report_written: false,
        }
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
            return AutomationStatus::Waiting;
        }
        self.observe_cross_section_latency_samples(app);
        if self.command_index >= self.script.commands.len() {
            if self.events.iter().any(|event| event.status == "failed") {
                return AutomationStatus::Failed("automation initialization failed".to_owned());
            }
            return AutomationStatus::Finished;
        }

        let command = self.script.commands[self.command_index].clone();
        let command_index = self.command_index;
        let command_started = Instant::now();
        let previous_display_refresh_timing = app.render_runtime.last_display_refresh_timing;
        let result = self.execute_command(app, ctx, &command, previous_display_refresh_timing);
        let command_execution_elapsed = command_started.elapsed();
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
                if let Some(timing) = new_display_refresh_timing_from_details(
                    &details,
                    app,
                    previous_display_refresh_timing,
                ) {
                    let event_epoch_ms = epoch_ms();
                    self.display_refresh_samples
                        .push(ProductAutomationDisplayRefreshSample {
                            command_index,
                            command: command.name(),
                            event_epoch_ms,
                            timing,
                        });
                    self.input_to_present_samples
                        .push(ProductAutomationInputToPresentSample {
                            command_index,
                            command: command.name(),
                            event_epoch_ms,
                            latency_ms: duration_ms(command_execution_elapsed),
                            display_refresh_timing: timing,
                        });
                }
                self.queue_cross_section_latency_samples_for_command(
                    app,
                    &command,
                    command_index,
                    command_started,
                );
                self.observe_cross_section_latency_samples(app);
                self.events.push(ProductAutomationEvent::passed(
                    command_index,
                    command.name(),
                    command_started.elapsed(),
                    details,
                ));
                self.active_wait_started = None;
                self.sleep_started = None;
                self.sleep_frames_remaining = None;
                if self.command_index == command_index {
                    self.command_index += 1;
                }
                AutomationStatus::Continue
            }
            Ok(CommandProgress::Waiting) => AutomationStatus::Waiting,
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
        previous_display_refresh_timing: Option<DisplayRefreshTiming>,
    ) -> Result<CommandProgress, String> {
        match command {
            ProductAutomationCommand::OpenDataset { path } => {
                let expected = normalize_path(path);
                let actual = normalize_path(app.dataset_runtime.dataset.root());
                if actual != expected {
                    return Err(format!(
                        "automation dataset mismatch: product opened {}, script expected {}",
                        app.dataset_runtime.dataset.root().display(),
                        path.display()
                    ));
                }
                Ok(CommandProgress::Done(json!({
                    "mode": "opened_by_product_startup",
                    "path": app.dataset_runtime.dataset.root().display().to_string(),
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
                    Ok(CommandProgress::Waiting)
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
            ProductAutomationCommand::SetViewerLayout { layout } => {
                let viewer_layout: ViewerLayout = (*layout).into();
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let cross_section = *application_view(&snapshot).cross_section();
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetLayout {
                        layout: viewer_layout,
                        cross_section,
                    },
                )?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "layout": layout.name(),
                    }),
                )))
            }
            ProductAutomationCommand::SetTimepoint { timepoint } => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let view = application_view(&snapshot);
                let timepoint_count = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog")
                    .shape()
                    .t();
                if *timepoint >= timepoint_count {
                    return Err(format!(
                        "timepoint {timepoint} is out of range for {} timepoint(s)",
                        timepoint_count
                    ));
                }
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetTimepoint(TimeIndex::new(*timepoint)),
                )?;
                let active_timepoint =
                    application_view(&current_egui_shell_bridge::snapshot(&app.application))
                        .timepoint()
                        .get();
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "timepoint": timepoint,
                        "active_timepoint": active_timepoint,
                    }),
                )))
            }
            ProductAutomationCommand::StepTimepoint { delta } => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let view = application_view(&snapshot);
                let count = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog")
                    .shape()
                    .t();
                let next = crate::playback::stepped_timepoint(view.timepoint(), count, *delta);
                dispatch_application_command(app, ctx, ApplicationCommand::SetTimepoint(next))?;
                let active_timepoint =
                    application_view(&current_egui_shell_bridge::snapshot(&app.application))
                        .timepoint()
                        .get();
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "delta": delta,
                        "active_timepoint": active_timepoint,
                    }),
                )))
            }
            ProductAutomationCommand::SetPlayback { playing } => {
                dispatch_application_command(
                    app,
                    ctx,
                    ApplicationCommand::SetPlaybackActive(*playing),
                )?;
                let active_timepoint =
                    application_view(&current_egui_shell_bridge::snapshot(&app.application))
                        .timepoint()
                        .get();
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "playing": playing,
                        "active_timepoint": active_timepoint,
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "render_mode": mode.name(),
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "layer_index": layer_index,
                        "render_mode": mode.name(),
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "display_level": display_level,
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "density_scale": density_scale,
                    }),
                )))
            }
            ProductAutomationCommand::SetChannelVisibility {
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "layer_index": layer_index,
                        "visible": visible,
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "layer_index": layer_index,
                        "opacity": opacity,
                    }),
                )))
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
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "layer_index": layer_index,
                        "low": low,
                        "high": high,
                    }),
                )))
            }
            ProductAutomationCommand::CameraFitData => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let view = application_view(&snapshot);
                let layer = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog");
                let camera = crate::viewport::fit_camera_to_shape_preserving_view(
                    *view.camera(),
                    layer.shape().spatial(),
                    layer.grid_to_world(),
                    app.render_runtime.presentation_viewport,
                );
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({}),
                )))
            }
            ProductAutomationCommand::CameraReset => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let view = application_view(&snapshot);
                let layer = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .expect("application view closes over the dataset catalog");
                let camera = crate::viewport::default_camera_for_shape(
                    layer.shape().spatial(),
                    layer.grid_to_world(),
                );
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({}),
                )))
            }
            ProductAutomationCommand::CameraOrbit {
                yaw_points,
                pitch_points,
                viewport_height_points,
            } => {
                let viewport_side = viewport_height_points.unwrap_or(800.0);
                let start = egui::pos2(viewport_side * 0.5, viewport_side * 0.5);
                let current = start + egui::vec2(*yaw_points, *pitch_points);
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let start_camera = *application_view(&snapshot).camera();
                let mut camera = start_camera;
                crate::viewport::apply_camera_orbit(
                    &mut camera,
                    start_camera,
                    start,
                    current,
                    egui::vec2(viewport_side, viewport_side),
                );
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "yaw_points": yaw_points,
                        "pitch_points": pitch_points,
                    }),
                )))
            }
            ProductAutomationCommand::CameraPan {
                x_points,
                y_points,
                viewport_height_points,
            } => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let mut camera = *application_view(&snapshot).camera();
                crate::viewport::apply_camera_pan(&mut camera, egui::vec2(*x_points, *y_points));
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "x_points": x_points,
                        "y_points": y_points,
                        "viewport_height_points": viewport_height_points,
                    }),
                )))
            }
            ProductAutomationCommand::CameraZoom { scroll_y_points } => {
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let mut camera = *application_view(&snapshot).camera();
                crate::viewport::apply_camera_zoom(&mut camera, *scroll_y_points);
                dispatch_application_command(app, ctx, ApplicationCommand::SetCamera(camera))?;
                Ok(CommandProgress::Done(details_with_display_refresh_timing(
                    app,
                    previous_display_refresh_timing,
                    json!({
                        "scroll_y_points": scroll_y_points,
                    }),
                )))
            }
            ProductAutomationCommand::CrossSectionPan {
                panel,
                x_points,
                y_points,
                probe_after,
            } => {
                ensure_finite_pair("cross_section_pan motion", *x_points, *y_points)?;
                let panel_id = automation_cross_section_panel_id(*panel)?;
                apply_cross_section_edit(app, ctx, panel_id, |cross_section, panel| {
                    cross_section.pan_by_panel_points(
                        panel,
                        f64::from(*x_points),
                        f64::from(*y_points),
                    );
                })?;
                let probe_after = if let Some(probe) = probe_after {
                    match self.probe_panel_hover(
                        app,
                        *panel,
                        probe.x_fraction,
                        probe.y_fraction,
                        probe.expected_status,
                        probe.expect_value,
                        probe.expected_generation_status,
                        probe.expected_display_current,
                        probe.expected_target_generation,
                        probe.expected_displayed_generation,
                        probe.expected_schedule_generation,
                    )? {
                        CommandProgress::Done(details) => Some(details),
                        CommandProgress::Waiting => {
                            return Err(
                                "cross_section_pan probe_after unexpectedly waited".to_owned()
                            );
                        }
                    }
                } else {
                    None
                };
                Ok(CommandProgress::Done(json!({
                    "panel": panel.name(),
                    "x_points": x_points,
                    "y_points": y_points,
                    "probe_after": probe_after,
                })))
            }
            ProductAutomationCommand::CrossSectionSliceStep {
                panel,
                notches,
                fast,
            } => {
                if !notches.is_finite() {
                    return Err("cross_section_slice_step notches must be finite".to_owned());
                }
                let panel_id = automation_cross_section_panel_id(*panel)?;
                let snapshot = current_egui_shell_bridge::snapshot(&app.application);
                let view = application_view(&snapshot);
                let voxel_size = crate::lod_scheduler::representative_voxel_world_size(
                    snapshot
                        .catalog()
                        .layer(view.active_layer())
                        .expect("application view closes over the dataset catalog")
                        .grid_to_world(),
                );
                let multiplier = if *fast {
                    crate::CROSS_SECTION_FAST_SLICE_MULTIPLIER
                } else {
                    1.0
                };
                apply_cross_section_edit(app, ctx, panel_id, |cross_section, panel| {
                    cross_section
                        .slice_by_world_distance(panel, *notches * voxel_size * multiplier);
                })?;
                Ok(CommandProgress::Done(json!({
                    "panel": panel.name(),
                    "notches": notches,
                    "fast": fast,
                })))
            }
            ProductAutomationCommand::CrossSectionZoom {
                panel,
                x_fraction,
                y_fraction,
                scroll_y_points,
            } => {
                ensure_fraction("cross_section_zoom x_fraction", *x_fraction)?;
                ensure_fraction("cross_section_zoom y_fraction", *y_fraction)?;
                if !scroll_y_points.is_finite() {
                    return Err("cross_section_zoom scroll_y_points must be finite".to_owned());
                }
                let panel_id = automation_cross_section_panel_id(*panel)?;
                let presentation_viewport =
                    cross_section_panel_presentation_viewport(app, panel_id)?;
                let pointer_position_points = egui::pos2(
                    (presentation_viewport.width_points() as f32) * *x_fraction,
                    (presentation_viewport.height_points() as f32) * *y_fraction,
                );
                let factor = (-f64::from(*scroll_y_points) * 0.001).exp();
                apply_cross_section_edit(app, ctx, panel_id, |cross_section, panel| {
                    cross_section.zoom_around_panel_point(
                        panel,
                        presentation_viewport,
                        f64::from(pointer_position_points.x),
                        f64::from(pointer_position_points.y),
                        factor,
                    );
                })?;
                Ok(CommandProgress::Done(json!({
                    "panel": panel.name(),
                    "x_fraction": x_fraction,
                    "y_fraction": y_fraction,
                    "scroll_y_points": scroll_y_points,
                    "viewport_width_points": presentation_viewport.width_points(),
                    "viewport_height_points": presentation_viewport.height_points(),
                })))
            }
            ProductAutomationCommand::CrossSectionRotate {
                panel,
                x_points,
                y_points,
            } => {
                ensure_finite_pair("cross_section_rotate motion", *x_points, *y_points)?;
                let panel_id = automation_cross_section_panel_id(*panel)?;
                apply_cross_section_edit(app, ctx, panel_id, |cross_section, panel| {
                    cross_section.rotate_oblique_by_panel_drag(
                        panel,
                        f64::from(*x_points),
                        f64::from(*y_points),
                        crate::CROSS_SECTION_ROTATE_RADIANS_PER_POINT,
                    );
                })?;
                Ok(CommandProgress::Done(json!({
                    "panel": panel.name(),
                    "x_points": x_points,
                    "y_points": y_points,
                })))
            }
            ProductAutomationCommand::ProbePanelHover {
                panel,
                x_fraction,
                y_fraction,
                expected_status,
                expect_value,
                expected_generation_status,
                expected_display_current,
                expected_target_generation,
                expected_displayed_generation,
                expected_schedule_generation,
            } => self.probe_panel_hover(
                app,
                *panel,
                *x_fraction,
                *y_fraction,
                *expected_status,
                *expect_value,
                *expected_generation_status,
                *expected_display_current,
                *expected_target_generation,
                *expected_displayed_generation,
                *expected_schedule_generation,
            ),
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
                self.assert_condition(app, condition)?;
                Ok(CommandProgress::Done(json!({
                    "condition": condition.name(),
                    "cross_section_snapshot": condition
                        .is_cross_section_condition()
                        .then(|| cross_section_diagnostics_json(app)),
                })))
            }
            ProductAutomationCommand::SleepOrFrames { millis, frames } => {
                if let Some(frames) = frames {
                    let remaining = self.sleep_frames_remaining.get_or_insert(*frames);
                    if *remaining == 0 {
                        return Ok(CommandProgress::Done(json!({ "frames": frames })));
                    }
                    *remaining -= 1;
                    return Ok(CommandProgress::Waiting);
                }
                let millis = millis.unwrap_or(0);
                let started = *self.sleep_started.get_or_insert_with(Instant::now);
                if started.elapsed() >= Duration::from_millis(millis) {
                    Ok(CommandProgress::Done(json!({ "millis": millis })))
                } else {
                    Ok(CommandProgress::Waiting)
                }
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

    #[allow(clippy::too_many_arguments)]
    fn probe_panel_hover(
        &self,
        app: &mut MiranteWorkbenchApp,
        panel: ProductAutomationPanelId,
        x_fraction: f32,
        y_fraction: f32,
        expected_status: Option<ProductAutomationCrossSectionHoverStatus>,
        expect_value: Option<bool>,
        expected_generation_status: Option<ProductAutomationCrossSectionGenerationStatus>,
        expected_display_current: Option<bool>,
        expected_target_generation: Option<u64>,
        expected_displayed_generation: Option<u64>,
        expected_schedule_generation: Option<u64>,
    ) -> Result<CommandProgress, String> {
        ensure_fraction("probe_panel_hover x_fraction", x_fraction)?;
        ensure_fraction("probe_panel_hover y_fraction", y_fraction)?;
        let panel_id = automation_cross_section_panel_id(panel)?;
        let presentation_viewport = cross_section_panel_presentation_viewport(app, panel_id)?;
        let x_points = f64::from(x_fraction) * presentation_viewport.width_points();
        let y_points = f64::from(y_fraction) * presentation_viewport.height_points();
        let before = panel_hover_readout_side_effect_snapshot(app);
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        let view = application_view(&snapshot);
        let layer = snapshot
            .catalog()
            .layer(view.active_layer())
            .expect("application view closes over the dataset catalog");
        let active_layer_id = current_physical_layer_id(&app.dataset_runtime, view.active_layer())
            .map_err(|error| error.to_string())?;
        let readout = cross_section_hover_readout_for_panel_point(
            &app.dataset_runtime,
            &app.render_runtime,
            CrossSectionReadoutInput {
                view,
                active_layer_id: &active_layer_id,
                active_layer_dtype: layer.dtype(),
            },
            panel_id,
            x_points,
            y_points,
            presentation_viewport,
        )
        .ok_or_else(|| {
            format!(
                "probe_panel_hover could not map panel {} at ({x_fraction:.3}, {y_fraction:.3})",
                panel_id.label()
            )
        })?;
        let after = panel_hover_readout_side_effect_snapshot(app);
        if after != before {
            return Err(format!(
                "probe_panel_hover mutated data/stream state; before={before} after={after}"
            ));
        }
        if let Some(expected_status) = expected_status {
            let expected: CrossSectionHoverStatus = expected_status.into();
            if readout.status != expected {
                return Err(format!(
                    "probe_panel_hover status for panel {} is {}, expected {}",
                    panel_id.label(),
                    cross_section_hover_status_name(readout.status),
                    expected_status.name()
                ));
            }
        }
        if let Some(expect_value) = expect_value
            && readout.value.is_some() != expect_value
        {
            return Err(format!(
                "probe_panel_hover value presence for panel {} is {}, expected {}",
                panel_id.label(),
                readout.value.is_some(),
                expect_value
            ));
        }
        if let Some(expected_generation_status) = expected_generation_status {
            let expected: CrossSectionHoverGenerationStatus = expected_generation_status.into();
            if readout.generation_status != expected {
                return Err(format!(
                    "probe_panel_hover generation status for panel {} is {}, expected {}",
                    panel_id.label(),
                    cross_section_hover_generation_status_name(readout.generation_status),
                    expected_generation_status.name()
                ));
            }
        }
        if let Some(expected_display_current) = expected_display_current
            && readout.display_current != expected_display_current
        {
            return Err(format!(
                "probe_panel_hover display_current for panel {} is {}, expected {}",
                panel_id.label(),
                readout.display_current,
                expected_display_current
            ));
        }
        if let Some(expected_target_generation) = expected_target_generation
            && readout.target_generation != expected_target_generation
        {
            return Err(format!(
                "probe_panel_hover target generation for panel {} is {}, expected {}",
                panel_id.label(),
                readout.target_generation,
                expected_target_generation
            ));
        }
        if let Some(expected_displayed_generation) = expected_displayed_generation
            && readout.displayed_generation != Some(expected_displayed_generation)
        {
            return Err(format!(
                "probe_panel_hover displayed generation for panel {} is {:?}, expected {}",
                panel_id.label(),
                readout.displayed_generation,
                expected_displayed_generation
            ));
        }
        if let Some(expected_schedule_generation) = expected_schedule_generation
            && readout.schedule_generation != Some(expected_schedule_generation)
        {
            return Err(format!(
                "probe_panel_hover schedule generation for panel {} is {:?}, expected {}",
                panel_id.label(),
                readout.schedule_generation,
                expected_schedule_generation
            ));
        }
        app.ui_runtime.hovered_pixel = None;
        app.ui_runtime.hovered_source_readout = Some(readout.text.clone());
        Ok(CommandProgress::Done(json!({
            "panel": panel.name(),
            "x_fraction": x_fraction,
            "y_fraction": y_fraction,
            "x_points": x_points,
            "y_points": y_points,
            "viewport_width_points": presentation_viewport.width_points(),
            "viewport_height_points": presentation_viewport.height_points(),
            "expected_status": expected_status.map(ProductAutomationCrossSectionHoverStatus::name),
            "expect_value": expect_value,
            "expected_generation_status": expected_generation_status.map(ProductAutomationCrossSectionGenerationStatus::name),
            "expected_display_current": expected_display_current,
            "expected_target_generation": expected_target_generation,
            "expected_displayed_generation": expected_displayed_generation,
            "expected_schedule_generation": expected_schedule_generation,
            "readout": cross_section_hover_readout_json(&readout),
            "no_synchronous_source_read": true,
            "side_effect_snapshot_before": before,
            "side_effect_snapshot_after": after,
        })))
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

        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        let view = application_view(&snapshot);
        let active_layer_dtype = snapshot
            .catalog()
            .layer(view.active_layer())
            .expect("application view closes over the dataset catalog")
            .dtype();
        let hover = viewport_hover_from_normalized_point(
            &app.render_runtime.frame,
            app.render_runtime.frame_f32.as_ref(),
            active_layer_dtype,
            x_fraction,
            y_fraction,
        )
        .ok_or_else(|| "probe_hover could not map the requested viewport point".to_owned())?;
        let hit = pick_hit_from_viewport_hover(
            &snapshot,
            &app.dataset_runtime,
            &app.analysis_runtime,
            &app.ui_runtime,
            &app.render_runtime,
            hover,
        )
        .map_err(|err| err.to_string())?;
        let commands = app
            .ui_runtime
            .viewer_tools
            .handle_event(ViewerToolEvent::Hover(Some(hit.clone())));
        let outcome = apply_viewer_tool_commands(
            &snapshot,
            &mut app.analysis_runtime,
            &mut app.ui_runtime,
            commands,
        )
        .map_err(|err| err.to_string())?;
        app.ui_runtime.hovered_pixel = Some(hover);
        app.ui_runtime.hovered_source_readout = None;
        Ok(CommandProgress::Done(json!({
            "x_fraction": x_fraction,
            "y_fraction": y_fraction,
            "hover": viewport_hover_json(hover),
            "pick_hit": pick_hit_json(&hit),
            "texture_refresh_requested": outcome.texture_refresh_requested,
            "rerender_requested": outcome.rerender_requested,
        })))
    }

    fn wait_condition_met(
        &self,
        app: &MiranteWorkbenchApp,
        condition: ProductAutomationWaitCondition,
    ) -> bool {
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        match condition {
            ProductAutomationWaitCondition::WindowReady => true,
            ProductAutomationWaitCondition::FirstFrame => {
                app.render_runtime
                    .frame_fidelity
                    .displayed_scale_level
                    .is_some()
                    || app.render_runtime.gpu_display_frame.is_some()
                    || app.render_runtime.diagnostics.output_pixels > 0
            }
            ProductAutomationWaitCondition::RuntimeIdle => {
                !crate::workbench_playback_runtime::background_work_active(
                    &snapshot,
                    &app.import_runtime,
                    &app.analysis_runtime,
                    &app.dataset_runtime,
                    &app.render_runtime,
                )
            }
            ProductAutomationWaitCondition::FrameFreshnessCurrent => {
                app.render_runtime.frame_fidelity.display_freshness
                    == DisplayedFrameFreshness::Current
                    || matches!(
                        app.render_runtime.frame_fidelity.completeness,
                        FrameCompleteness::Exact | FrameCompleteness::Complete
                    )
            }
            ProductAutomationWaitCondition::NoRenderError => {
                app.render_runtime
                    .frame_fidelity
                    .last_failure_kind
                    .is_none()
                    && app
                        .render_runtime
                        .frame_fidelity
                        .last_capacity_error
                        .is_none()
            }
            ProductAutomationWaitCondition::GpuFramePresented => {
                app.render_runtime.gpu_display_frame.is_some()
            }
        }
    }

    fn assert_condition(
        &self,
        app: &MiranteWorkbenchApp,
        condition: &ProductAutomationAssertCondition,
    ) -> Result<(), String> {
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
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
                if let Some(kind) = app.render_runtime.frame_fidelity.last_failure_kind {
                    Err(format!("render failure is set: {kind:?}"))
                } else if let Some(error) = app
                    .render_runtime
                    .frame_fidelity
                    .last_capacity_error
                    .as_ref()
                {
                    Err(format!("render capacity error is set: {error}"))
                } else {
                    Ok(())
                }
            }
            ProductAutomationAssertCondition::FrameFreshnessCurrent => {
                if self
                    .wait_condition_met(app, ProductAutomationWaitCondition::FrameFreshnessCurrent)
                {
                    Ok(())
                } else {
                    Err(format!(
                        "frame is not current: {:?}",
                        app.render_runtime.frame_fidelity.display_freshness
                    ))
                }
            }
            ProductAutomationAssertCondition::RuntimeIdle => {
                if crate::workbench_playback_runtime::background_work_active(
                    &snapshot,
                    &app.import_runtime,
                    &app.analysis_runtime,
                    &app.dataset_runtime,
                    &app.render_runtime,
                ) {
                    Err("background work is still active".to_owned())
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
            ProductAutomationAssertCondition::ActiveTimepoint { timepoint } => {
                if view.timepoint().get() == *timepoint {
                    Ok(())
                } else {
                    Err(format!(
                        "active timepoint is {}, expected {}",
                        view.timepoint().get(),
                        timepoint
                    ))
                }
            }
            ProductAutomationAssertCondition::ObservedTimepoints { min_distinct } => {
                let mut observed = BTreeSet::new();
                observed.insert(view.timepoint().get());
                observed.extend(
                    self.app_update_samples
                        .iter()
                        .map(|sample| sample.active_timepoint),
                );
                if observed.len() >= *min_distinct {
                    Ok(())
                } else {
                    Err(format!(
                        "observed {} distinct active timepoint(s), expected at least {}; observed={:?}",
                        observed.len(),
                        min_distinct,
                        observed
                    ))
                }
            }
            ProductAutomationAssertCondition::Playback { playing } => {
                let actual = snapshot.transient().playback_active();
                if actual == *playing {
                    Ok(())
                } else {
                    Err(format!(
                        "playback playing is {}, expected {}",
                        actual, playing
                    ))
                }
            }
            ProductAutomationAssertCondition::CrossSectionActivePanel { panel } => {
                let expected = match panel {
                    Some(panel) => application_cross_section_panel_id(
                        automation_cross_section_panel_id(*panel)?,
                    ),
                    None => None,
                };
                let actual = snapshot.transient().active_cross_section_panel();
                if actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "active cross-section panel is {:?}, expected {:?}",
                        actual, expected
                    ))
                }
            }
            ProductAutomationAssertCondition::CrossSectionPanelSchedule {
                panel,
                status,
                min_generation,
                target_scale_level,
                render_scale_level,
                min_selected_bricks,
                max_missing_occupied_bricks,
                display_current,
            } => {
                let panel_id = automation_cross_section_panel_id(*panel)?;
                if view.layout() != ViewerLayout::FourPanel {
                    return Err("four-panel runtime is not active".to_owned());
                }
                let panel_state = app
                    .render_runtime
                    .cross_section_runtime
                    .panel(panel_id)
                    .ok_or_else(|| format!("panel {} is not active", panel_id.label()))?;
                let schedule = panel_state.cross_section_schedule.ok_or_else(|| {
                    format!("panel {} has no cross-section schedule", panel_id.label())
                })?;
                if let Some(expected_status) = status {
                    let expected_status = (*expected_status).into();
                    if schedule.status != expected_status {
                        return Err(format!(
                            "panel {} schedule status is {:?}, expected {:?}",
                            panel_id.label(),
                            schedule.status,
                            expected_status
                        ));
                    }
                }
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
                if let Some(target_scale_level) = target_scale_level
                    && schedule.target_scale_level != Some(*target_scale_level)
                {
                    return Err(format!(
                        "panel {} target scale is {:?}, expected s{}",
                        panel_id.label(),
                        schedule.target_scale_level,
                        target_scale_level
                    ));
                }
                if let Some(render_scale_level) = render_scale_level
                    && schedule.render_scale_level != Some(*render_scale_level)
                {
                    return Err(format!(
                        "panel {} render scale is {:?}, expected s{}",
                        panel_id.label(),
                        schedule.render_scale_level,
                        render_scale_level
                    ));
                }
                if let Some(min_selected_bricks) = min_selected_bricks
                    && schedule.selected_bricks < *min_selected_bricks
                {
                    return Err(format!(
                        "panel {} selected {} bricks, expected at least {}",
                        panel_id.label(),
                        schedule.selected_bricks,
                        min_selected_bricks
                    ));
                }
                if let Some(max_missing_occupied_bricks) = max_missing_occupied_bricks
                    && schedule.missing_occupied_bricks > *max_missing_occupied_bricks
                {
                    return Err(format!(
                        "panel {} missing {} occupied bricks, expected at most {}",
                        panel_id.label(),
                        schedule.missing_occupied_bricks,
                        max_missing_occupied_bricks
                    ));
                }
                if let Some(display_current) = display_current
                    && panel_state.display_current() != *display_current
                {
                    return Err(format!(
                        "panel {} display_current is {}, expected {}",
                        panel_id.label(),
                        panel_state.display_current(),
                        display_current
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::CrossSectionStream {
                panel,
                timepoint,
                priority,
                fairness_promoted,
                active_panel_at_submission,
                min_queued_current_frame,
                min_queued_prefetch,
                min_requested,
                min_completed,
                min_visible_chunks,
                max_stale,
                max_failed,
            } => {
                let panel_id = automation_cross_section_panel_id(*panel)?;
                let stream = app
                    .render_runtime
                    .cross_section_runtime
                    .panel_streams
                    .get(&panel_id)
                    .ok_or_else(|| {
                        format!("panel {} has no cross-section stream", panel_id.label())
                    })?;
                if let Some(timepoint) = timepoint
                    && stream.request_key.timepoint.get() != *timepoint
                {
                    return Err(format!(
                        "panel {} stream timepoint is {}, expected {}",
                        panel_id.label(),
                        stream.request_key.timepoint.get(),
                        timepoint
                    ));
                }
                if let Some(priority) = priority {
                    let expected = (*priority).into();
                    if stream.priority != expected {
                        return Err(format!(
                            "panel {} stream priority is {:?}, expected {:?}",
                            panel_id.label(),
                            stream.priority,
                            expected
                        ));
                    }
                }
                if let Some(fairness_promoted) = fairness_promoted
                    && stream.fairness_promoted != *fairness_promoted
                {
                    return Err(format!(
                        "panel {} fairness_promoted is {}, expected {}",
                        panel_id.label(),
                        stream.fairness_promoted,
                        fairness_promoted
                    ));
                }
                if let Some(active_panel_at_submission) = active_panel_at_submission {
                    let expected = Some(automation_cross_section_panel_id(
                        *active_panel_at_submission,
                    )?);
                    if stream.active_panel_at_submission != expected {
                        return Err(format!(
                            "panel {} active_panel_at_submission is {:?}, expected {:?}",
                            panel_id.label(),
                            stream.active_panel_at_submission,
                            expected
                        ));
                    }
                }
                if let Some(min_queued_current_frame) = min_queued_current_frame
                    && stream.queued_current_frame < *min_queued_current_frame
                {
                    return Err(format!(
                        "panel {} queued_current_frame is {}, expected at least {}",
                        panel_id.label(),
                        stream.queued_current_frame,
                        min_queued_current_frame
                    ));
                }
                if let Some(min_queued_prefetch) = min_queued_prefetch
                    && stream.queued_prefetch < *min_queued_prefetch
                {
                    return Err(format!(
                        "panel {} queued_prefetch is {}, expected at least {}",
                        panel_id.label(),
                        stream.queued_prefetch,
                        min_queued_prefetch
                    ));
                }
                if let Some(min_requested) = min_requested
                    && stream.requested < *min_requested
                {
                    return Err(format!(
                        "panel {} stream requested {} chunks, expected at least {}",
                        panel_id.label(),
                        stream.requested,
                        min_requested
                    ));
                }
                if let Some(min_completed) = min_completed
                    && stream.completed < *min_completed
                {
                    return Err(format!(
                        "panel {} stream completed {} chunks, expected at least {}",
                        panel_id.label(),
                        stream.completed,
                        min_completed
                    ));
                }
                if let Some(min_visible_chunks) = min_visible_chunks
                    && stream.visible_chunks < *min_visible_chunks
                {
                    return Err(format!(
                        "panel {} stream has {} visible chunks, expected at least {}",
                        panel_id.label(),
                        stream.visible_chunks,
                        min_visible_chunks
                    ));
                }
                if let Some(max_stale) = max_stale
                    && stream.stale > *max_stale
                {
                    return Err(format!(
                        "panel {} stream stale count is {}, expected at most {}",
                        panel_id.label(),
                        stream.stale,
                        max_stale
                    ));
                }
                if let Some(max_failed) = max_failed
                    && stream.failed > *max_failed
                {
                    return Err(format!(
                        "panel {} stream failed count is {}, expected at most {}",
                        panel_id.label(),
                        stream.failed,
                        max_failed
                    ));
                }
                Ok(())
            }
            ProductAutomationAssertCondition::CrossSectionStreamsMatchActiveTimepoint {
                min_completed,
                min_visible_chunks,
                max_failed,
            } => assert_cross_section_streams_match_active_timepoint(
                app,
                *min_completed,
                *min_visible_chunks,
                *max_failed,
            ),
            ProductAutomationAssertCondition::CrossSectionPanelNonblank {
                panel,
                min_nonzero_rgb_pixels,
            } => {
                let panel_id = automation_cross_section_panel_id(*panel)?;
                assert_cross_section_panel_nonblank(
                    app,
                    panel_id,
                    min_nonzero_rgb_pixels.unwrap_or(1),
                )
            }
            ProductAutomationAssertCondition::CrossSectionPanelImagesDistinct {
                min_different_pixels,
            } => assert_cross_section_panel_images_distinct(app, min_different_pixels.unwrap_or(1)),
            ProductAutomationAssertCondition::FourPanelImagesDistinct {
                min_different_pixels,
            } => assert_four_panel_images_distinct(app, min_different_pixels.unwrap_or(1)),
            ProductAutomationAssertCondition::CrossSectionRetired => {
                assert_cross_section_retired(app)
            }
        }
    }

    fn diagnostics_json(&self, app: &MiranteWorkbenchApp) -> Value {
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        let view = application_view(&snapshot);
        let active_layer = snapshot
            .catalog()
            .layer(view.active_layer())
            .expect("application view closes over the dataset catalog");
        let active_layer_id = current_physical_layer_id(&app.dataset_runtime, view.active_layer())
            .expect("application catalog closes over the current physical manifest");
        let active_scale_count = app
            .dataset_runtime
            .dataset
            .scale_count(&active_layer_id)
            .ok();
        let typed_render_error = app
            .render_runtime
            .frame_fidelity
            .last_failure_kind
            .map(|kind| format!("{kind:?}"))
            .or_else(|| {
                app.render_runtime
                    .frame_fidelity
                    .last_capacity_error
                    .clone()
            });
        json!({
            "dataset": {
                "path": app.dataset_runtime.dataset.root().display().to_string(),
                "name": snapshot.catalog().label(),
                "layer_count": snapshot.catalog().len(),
                "active_layer_id": active_layer_id.as_str(),
                "active_layer_dtype": format!("{:?}", active_layer.dtype()),
                "active_layer_shape": {
                    "x": active_layer.shape().x(),
                    "y": active_layer.shape().y(),
                    "z": active_layer.shape().z(),
                    "t": active_layer.shape().t(),
                },
                "active_scale_count": active_scale_count,
                "timepoint_count": active_layer.shape().t(),
            },
            "render": {
                "active_render_mode": format!("{:?}", view.layer(view.active_layer()).expect("active layer").render_state().mode()),
                "projection": format!("{:?}", view.camera().projection()),
                "backend": format!("{:?}", app.render_runtime.render_backend),
                "adapter": app.startup_diagnostics.gpu_adapter.clone(),
                "last_error": typed_render_error,
                "gpu_display_frame_present": app.render_runtime.gpu_display_frame.is_some(),
                "cpu_nonzero_pixels": app.render_runtime.diagnostics.nonzero_pixels,
                "cpu_output_pixels": app.render_runtime.diagnostics.output_pixels,
                "frame_fidelity": {
                    "target_scale_level": app.render_runtime.frame_fidelity.target_scale_level,
                    "displayed_scale_level": app.render_runtime.frame_fidelity.displayed_scale_level,
                    "completeness": format!("{:?}", app.render_runtime.frame_fidelity.completeness),
                    "reason": format!("{:?}", app.render_runtime.frame_fidelity.reason),
                    "display_freshness": format!("{:?}", app.render_runtime.frame_fidelity.display_freshness),
                    "frame_time_ms": app.render_runtime.frame_fidelity.frame_time_ms,
                    "visible_bricks": app.render_runtime.frame_fidelity.visible_bricks,
                    "resident_bricks": app.render_runtime.frame_fidelity.resident_bricks,
                    "missing_occupied_bricks": app.render_runtime.frame_fidelity.missing_occupied_bricks,
                    "cpu_cache_bytes": app.render_runtime.frame_fidelity.cpu_cache_bytes,
                    "gpu_resident_bytes": app.render_runtime.frame_fidelity.gpu_resident_bytes,
                    "upload_queue_depth": app.render_runtime.frame_fidelity.upload_queue_depth,
                    "last_failure_kind": app.render_runtime.frame_fidelity.last_failure_kind.map(|kind| format!("{kind:?}")),
                    "last_capacity_error": app.render_runtime.frame_fidelity.last_capacity_error.clone(),
                },
                "display_refresh_timing": app
                    .render_runtime.last_display_refresh_timing
                    .map(display_refresh_timing_json),
            },
            "streaming": {
                "visible_brick_count": app.render_runtime.visible_brick_count,
                "visible_brick_plan_stride": app.render_runtime.visible_brick_plan_stride,
                "visible_brick_plan_error": app.render_runtime.visible_brick_plan_error.clone(),
                "brick_stream_scale_level": app.dataset_runtime.brick_stream_scale_level,
                "brick_stream_requested": app.dataset_runtime.brick_stream_requested,
                "brick_stream_completed": app.dataset_runtime.brick_stream_completed,
                "brick_stream_cancelled": app.dataset_runtime.brick_stream_cancelled,
                "brick_stream_stale": app.dataset_runtime.brick_stream_stale,
                "brick_stream_failed": app.dataset_runtime.brick_stream_failed,
                "brick_stream_complete": app.dataset_runtime.brick_stream_complete,
                "brick_stream_last_error": app.dataset_runtime.brick_stream_last_error.clone(),
            },
            "cross_section": cross_section_diagnostics_json(app),
            "data_engine": app
                .dataset_runtime.dataset
                .diagnostics()
                .ok()
                .map(data_engine_diagnostics_json),
            "gpu_renderer": app
                .render_runtime.gpu_renderer
                .as_ref()
                .and_then(|renderer| renderer.stats().ok())
                .map(gpu_renderer_stats_json),
            "gpu_adapter": app
                .render_runtime.gpu_renderer
                .as_ref()
                .map(|renderer| gpu_adapter_diagnostics_json(renderer.adapter_diagnostics())),
            "gpu_timestamp_timing": app
                .render_runtime.gpu_renderer
                .as_ref()
                .map(|renderer| gpu_timestamp_timing_json(renderer.adapter_diagnostics())),
            "presentation_timing": presentation_timing_json(),
            "brick_queue": app
                .dataset_runtime.brick_read_pool
                .as_ref()
                .and_then(|pool| pool.queue_diagnostics().ok())
                .map(brick_queue_diagnostics_json),
            "brick_result_drain": {
                "limit": app.dataset_runtime.brick_result_drain_limit,
                "time_budget_ms": app.dataset_runtime.brick_result_drain_time_budget_ms,
                "last_count": app.dataset_runtime.brick_result_drain_last_count,
                "last_budget_limited": app.dataset_runtime.brick_result_drain_last_budget_limited,
                "last_repaint_reason": app.dataset_runtime.brick_result_drain_last_repaint_reason.as_deref(),
                "budget_hit_count": app.dataset_runtime.brick_result_drain_budget_hit_count,
                "total_drained": app.dataset_runtime.brick_result_drain_total_drained,
            },
            "camera": {
                "projection": format!("{:?}", view.camera().projection()),
                "viewport": {
                    "width": app.render_runtime.render_viewport.width,
                    "height": app.render_runtime.render_viewport.height,
                },
            },
            "summary_text": app.diagnostics_summary_text(),
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
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        let report = json!({
            "schema": AUTOMATION_REPORT_SCHEMA,
            "schema_version": AUTOMATION_SCHEMA_VERSION,
            "status": status,
            "failure_reason": failure_reason,
            "evidence_level": "E1",
            "claim_boundary": {
                "evidence_type": "internal_native_window_product_automation",
                "source": "instrumented_application_commands_internal_state_and_readback",
                "closure_authority": "integration_support_only_not_black_box_product_open",
                "e4_product_open_satisfied": false,
            },
            "viewport_evidence": {
                "requested_window_inner_size_points": requested_window_inner_size_points,
                "pixels_per_point": ctx.pixels_per_point(),
                "observed_client_area_pixels": Value::Null,
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
                "path": app.dataset_runtime.dataset.root().display().to_string(),
                "name": snapshot.catalog().label(),
            },
            "events": &self.events,
            "app_update_timing_samples": self
                .app_update_samples
                .iter()
                .map(ProductAutomationAppUpdateSample::json)
                .collect::<Vec<_>>(),
            "app_update_timing_summary": app_update_timing_summary_json(
                &self.app_update_samples,
            ),
            "display_refresh_timing_samples": self
                .display_refresh_samples
                .iter()
                .map(ProductAutomationDisplayRefreshSample::json)
                .collect::<Vec<_>>(),
            "display_refresh_timing_summary": display_refresh_timing_summary_json(
                &self.display_refresh_samples,
            ),
            "input_to_present_timing_samples": self
                .input_to_present_samples
                .iter()
                .map(ProductAutomationInputToPresentSample::json)
                .collect::<Vec<_>>(),
            "input_to_present_timing_summary": input_to_present_timing_summary_json(
                &self.input_to_present_samples,
            ),
            "cross_section_latency_samples": self
                .cross_section_latency_samples
                .iter()
                .map(ProductAutomationCrossSectionLatencySample::json)
                .collect::<Vec<_>>(),
            "cross_section_latency_pending_samples": self
                .pending_cross_section_latency_samples
                .iter()
                .map(PendingCrossSectionLatencySample::json)
                .collect::<Vec<_>>(),
            "cross_section_latency_summary": cross_section_latency_summary_json(
                &self.cross_section_latency_samples,
                self.pending_cross_section_latency_samples.len(),
            ),
            "presentation_timing": presentation_timing_json(),
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
        app.ui_runtime.allow_close_without_prompt = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn record_app_update_sample(
        &mut self,
        app: &MiranteWorkbenchApp,
        timing: ProductAutomationAppUpdateTiming,
        automation_step_ms: f64,
    ) {
        if self.report_written {
            return;
        }
        let total_update_ms = duration_ms(timing.update_started.elapsed());
        let snapshot = current_egui_shell_bridge::snapshot(&app.application);
        let view = application_view(&snapshot);
        let render_mode = view
            .layer(view.active_layer())
            .expect("application view has an active layer")
            .render_state()
            .mode();
        self.app_update_samples
            .push(ProductAutomationAppUpdateSample {
                sample_index: self.app_update_samples.len(),
                command_index: self.command_index,
                event_epoch_ms: epoch_ms(),
                timing: ProductAutomationAppUpdatePhases {
                    setup_ms: timing.setup_ms,
                    task_drain_ms: timing.task_drain_ms,
                    playback_ms: timing.playback_ms,
                    ui_build_ms: timing.ui_build_ms,
                    histogram_ui_ms: timing.histogram_ui_ms,
                    command_apply_ms: timing.command_apply_ms,
                    display_refresh_trigger_ms: timing.display_refresh_trigger_ms,
                    import_action_ms: timing.import_action_ms,
                    brick_result_drain_ms: timing.brick_result_drain_ms,
                    background_repaint_request_ms: timing.background_repaint_request_ms,
                    automation_step_ms,
                    total_update_ms,
                },
                background_work_active: crate::workbench_playback_runtime::background_work_active(
                    &snapshot,
                    &app.import_runtime,
                    &app.analysis_runtime,
                    &app.dataset_runtime,
                    &app.render_runtime,
                ),
                active_timepoint: view.timepoint().get(),
                render_mode,
                display_freshness: app.render_runtime.frame_fidelity.display_freshness,
                target_scale_level: app.render_runtime.frame_fidelity.target_scale_level,
                displayed_scale_level: app.render_runtime.frame_fidelity.displayed_scale_level,
                visible_bricks: app.render_runtime.frame_fidelity.visible_bricks,
                resident_bricks: app.render_runtime.frame_fidelity.resident_bricks,
            });
    }

    fn queue_cross_section_latency_samples_for_command(
        &mut self,
        app: &MiranteWorkbenchApp,
        command: &ProductAutomationCommand,
        command_index: usize,
        started_at: Instant,
    ) {
        match command {
            ProductAutomationCommand::CrossSectionPan { panel, .. } => self
                .queue_cross_section_latency_sample(
                    app,
                    command_index,
                    command.name(),
                    "pan",
                    PanelId::from(*panel),
                    started_at,
                ),
            ProductAutomationCommand::CrossSectionSliceStep { panel, .. } => self
                .queue_cross_section_latency_sample(
                    app,
                    command_index,
                    command.name(),
                    "slice_shift",
                    PanelId::from(*panel),
                    started_at,
                ),
            ProductAutomationCommand::CrossSectionZoom { panel, .. } => self
                .queue_cross_section_latency_sample(
                    app,
                    command_index,
                    command.name(),
                    "zoom",
                    PanelId::from(*panel),
                    started_at,
                ),
            ProductAutomationCommand::CrossSectionRotate { panel, .. } => self
                .queue_cross_section_latency_sample(
                    app,
                    command_index,
                    command.name(),
                    "oblique_rotation",
                    PanelId::from(*panel),
                    started_at,
                ),
            ProductAutomationCommand::SetTimepoint { .. }
            | ProductAutomationCommand::StepTimepoint { .. } => {
                for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                    self.queue_cross_section_latency_sample(
                        app,
                        command_index,
                        command.name(),
                        "timepoint_change",
                        panel_id,
                        started_at,
                    );
                }
            }
            ProductAutomationCommand::OpenDataset { .. }
            | ProductAutomationCommand::WaitFor { .. }
            | ProductAutomationCommand::SetViewportSize { .. }
            | ProductAutomationCommand::SetViewerLayout { .. }
            | ProductAutomationCommand::SetPlayback { .. }
            | ProductAutomationCommand::SetRenderMode { .. }
            | ProductAutomationCommand::SetLayerRenderMode { .. }
            | ProductAutomationCommand::SetIsoDisplayLevel { .. }
            | ProductAutomationCommand::SetDvrDensityScale { .. }
            | ProductAutomationCommand::SetChannelVisibility { .. }
            | ProductAutomationCommand::SetLayerOpacity { .. }
            | ProductAutomationCommand::SetLayerWindow { .. }
            | ProductAutomationCommand::CameraFitData
            | ProductAutomationCommand::CameraReset
            | ProductAutomationCommand::CameraOrbit { .. }
            | ProductAutomationCommand::CameraPan { .. }
            | ProductAutomationCommand::CameraZoom { .. }
            | ProductAutomationCommand::ProbePanelHover { .. }
            | ProductAutomationCommand::ProbeHover { .. }
            | ProductAutomationCommand::CopyDiagnostics
            | ProductAutomationCommand::CaptureScreenshot { .. }
            | ProductAutomationCommand::Assert { .. }
            | ProductAutomationCommand::SleepOrFrames { .. }
            | ProductAutomationCommand::Quit => {}
        }
    }

    fn queue_cross_section_latency_sample(
        &mut self,
        app: &MiranteWorkbenchApp,
        command_index: usize,
        command: &'static str,
        operation: &'static str,
        panel_id: PanelId,
        started_at: Instant,
    ) {
        let Some(panel) = app.render_runtime.cross_section_runtime.panel(panel_id) else {
            return;
        };
        if panel_id.cross_section_panel().is_none() {
            return;
        }
        self.pending_cross_section_latency_samples
            .push(PendingCrossSectionLatencySample {
                command_index,
                command,
                operation,
                panel_id,
                started_at,
                target_generation: panel.generation,
                active_timepoint: application_view(&current_egui_shell_bridge::snapshot(
                    &app.application,
                ))
                .timepoint()
                .get(),
            });
    }

    fn observe_cross_section_latency_samples(&mut self, app: &MiranteWorkbenchApp) {
        if self.pending_cross_section_latency_samples.is_empty() {
            return;
        }
        let mut still_pending = Vec::new();
        let pending = std::mem::take(&mut self.pending_cross_section_latency_samples);
        for sample in pending {
            if let Some(completed) = sample.completed_sample(app) {
                self.cross_section_latency_samples.push(completed);
            } else {
                still_pending.push(sample);
            }
        }
        self.pending_cross_section_latency_samples = still_pending;
    }

    fn observe_and_enforce_limits(&mut self, app: &MiranteWorkbenchApp) -> Result<(), String> {
        let limits = self.script.limits;
        let data_diagnostics = app
            .dataset_runtime
            .dataset
            .diagnostics()
            .map_err(|err| err.to_string());
        match data_diagnostics {
            Ok(diagnostics) => {
                self.limit_observations
                    .observe_data_engine(diagnostics.stats);
                limits.check_data_engine(diagnostics.stats)?;
            }
            Err(err) if limits.requires_data_engine() => {
                return Err(format!(
                    "failed to read data engine diagnostics for limits: {err}"
                ));
            }
            Err(_) => {}
        }

        let queue_diagnostics = app
            .dataset_runtime
            .brick_read_pool
            .as_ref()
            .and_then(|pool| pool.queue_diagnostics().ok());
        match queue_diagnostics.as_ref() {
            Some(queue) => {
                self.limit_observations.observe_brick_queue(queue);
                limits.check_brick_queue(queue)?;
            }
            None if limits.requires_brick_queue() => {
                return Err(
                    "brick queue limits were configured but no brick queue diagnostics are active"
                        .to_owned(),
                );
            }
            None => {}
        }

        let gpu_stats = app
            .render_runtime
            .gpu_renderer
            .as_ref()
            .map(|renderer| renderer.stats().map_err(|err| err.to_string()));
        match gpu_stats {
            Some(Ok(stats)) => {
                self.limit_observations.observe_gpu_renderer(stats);
                limits.check_gpu_renderer(stats)?;
            }
            Some(Err(err)) if limits.requires_gpu_renderer() => {
                return Err(format!(
                    "failed to read GPU renderer stats for limits: {err}"
                ));
            }
            None if limits.requires_gpu_renderer() => {
                return Err(
                    "GPU renderer limits were configured but no GPU renderer is active".to_owned(),
                );
            }
            Some(Err(_)) | None => {}
        }
        Ok(())
    }

    fn capture_failure_artifact(&mut self, app: &mut MiranteWorkbenchApp) -> Result<(), String> {
        let artifact = self.capture_viewport_artifact(app, Some("failure-final-frame"))?;
        self.artifacts.push(artifact);
        Ok(())
    }
}

fn automation_cross_section_panel_id(panel: ProductAutomationPanelId) -> Result<PanelId, String> {
    let panel_id: PanelId = panel.into();
    if panel_id.cross_section_panel().is_some() {
        Ok(panel_id)
    } else {
        Err(format!(
            "panel {} is not a cross-section automation target",
            panel.name()
        ))
    }
}

fn ensure_finite_pair(name: &str, x: f32, y: f32) -> Result<(), String> {
    if x.is_finite() && y.is_finite() {
        Ok(())
    } else {
        Err(format!("{name} values must be finite"))
    }
}

fn ensure_fraction(name: &str, value: f32) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be finite and between 0.0 and 1.0"))
    }
}

fn cross_section_panel_presentation_viewport(
    app: &MiranteWorkbenchApp,
    panel_id: PanelId,
) -> Result<PresentationViewport, String> {
    app.render_runtime
        .cross_section_runtime
        .panel(panel_id)
        .and_then(|panel| panel.presentation_viewport)
        .ok_or_else(|| {
            format!(
                "panel {} has no recorded presentation viewport; wait for the four-panel UI before zooming",
                panel_id.label()
            )
        })
}

impl From<ProductAutomationCrossSectionHoverStatus> for CrossSectionHoverStatus {
    fn from(value: ProductAutomationCrossSectionHoverStatus) -> Self {
        match value {
            ProductAutomationCrossSectionHoverStatus::Value => Self::Value,
            ProductAutomationCrossSectionHoverStatus::Loading => Self::Loading,
            ProductAutomationCrossSectionHoverStatus::Stale => Self::Stale,
            ProductAutomationCrossSectionHoverStatus::Incomplete => Self::Incomplete,
            ProductAutomationCrossSectionHoverStatus::Unavailable => Self::Unavailable,
            ProductAutomationCrossSectionHoverStatus::InvalidNoData => Self::InvalidNoData,
            ProductAutomationCrossSectionHoverStatus::Outside => Self::Outside,
        }
    }
}

impl From<ProductAutomationCrossSectionGenerationStatus> for CrossSectionHoverGenerationStatus {
    fn from(value: ProductAutomationCrossSectionGenerationStatus) -> Self {
        match value {
            ProductAutomationCrossSectionGenerationStatus::CurrentDisplayed => {
                Self::CurrentDisplayed
            }
            ProductAutomationCrossSectionGenerationStatus::CurrentUndisplayed => {
                Self::CurrentUndisplayed
            }
            ProductAutomationCrossSectionGenerationStatus::RetainedStale => Self::RetainedStale,
            ProductAutomationCrossSectionGenerationStatus::Unavailable => Self::Unavailable,
        }
    }
}

fn cross_section_hover_status_name(status: CrossSectionHoverStatus) -> &'static str {
    match status {
        CrossSectionHoverStatus::Value => "value",
        CrossSectionHoverStatus::Loading => "loading",
        CrossSectionHoverStatus::Stale => "stale",
        CrossSectionHoverStatus::Incomplete => "incomplete",
        CrossSectionHoverStatus::Unavailable => "unavailable",
        CrossSectionHoverStatus::InvalidNoData => "invalid_no_data",
        CrossSectionHoverStatus::Outside => "outside",
    }
}

fn cross_section_hover_generation_status_name(
    status: crate::cross_section_readout::CrossSectionHoverGenerationStatus,
) -> &'static str {
    match status {
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentDisplayed => {
            "current_displayed"
        }
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::CurrentUndisplayed => {
            "current_undisplayed"
        }
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::RetainedStale => {
            "retained_stale"
        }
        crate::cross_section_readout::CrossSectionHoverGenerationStatus::Unavailable => {
            "unavailable"
        }
    }
}

fn cross_section_hover_readout_json(readout: &CrossSectionHoverReadout) -> Value {
    json!({
        "text": readout.text.clone(),
        "panel_id": readout.panel_id.label(),
        "layer_id": readout.layer_id.clone(),
        "timepoint": readout.timepoint,
        "scale_level": readout.scale_level,
        "target_generation": readout.target_generation,
        "displayed_generation": readout.displayed_generation,
        "schedule_generation": readout.schedule_generation,
        "display_current": readout.display_current,
        "generation_status": cross_section_hover_generation_status_name(readout.generation_status),
        "world_position": readout.world_position.map(vec3_json),
        "grid_position": readout.grid_position.map(vec3_json),
        "nearest_grid_index": readout.nearest_grid_index.map(|index| {
            json!({
                "x": index.x,
                "y": index.y,
                "z": index.z,
            })
        }),
        "value": readout.value.map(cross_section_hover_value_json),
        "status": cross_section_hover_status_name(readout.status),
    })
}

fn cross_section_hover_value_json(value: CrossSectionHoverValue) -> Value {
    match value {
        CrossSectionHoverValue::U8(value) => json!({
            "dtype": "u8",
            "value": value,
        }),
        CrossSectionHoverValue::U16(value) => json!({
            "dtype": "u16",
            "value": value,
        }),
        CrossSectionHoverValue::F32(value) => json!({
            "dtype": "f32",
            "value": finite_f32_json(value),
        }),
    }
}

fn finite_f32_json(value: f32) -> Value {
    serde_json::Number::from_f64(f64::from(value))
        .map(Value::Number)
        .unwrap_or_else(|| {
            json!({
                "non_finite": value.to_string(),
            })
        })
}

fn vec3_json(value: glam::DVec3) -> Value {
    json!({
        "x": value.x,
        "y": value.y,
        "z": value.z,
    })
}

fn panel_hover_readout_side_effect_snapshot(app: &MiranteWorkbenchApp) -> Value {
    let cross_section_streams = app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .iter()
        .map(|(panel_id, stream)| {
            json!({
                "panel_id": panel_id.label(),
                "panel_generation": stream.request_key.panel_generation,
                "scale_level": stream.request_key.scale_level,
                "timepoint": stream.request_key.timepoint.get(),
                "visible_chunks": stream.request_key.visible_chunk_count,
                "visible_chunk_fingerprint": stream.request_key.visible_chunk_fingerprint,
                "priority": format!("{:?}", stream.priority),
                "active_panel_at_submission": stream
                    .active_panel_at_submission
                    .map(|panel_id| panel_id.label().to_owned()),
                "fairness_promoted": stream.fairness_promoted,
                "requested": stream.requested,
                "deferred": stream.deferred,
                "queued_current_frame": stream.queued_current_frame,
                "queued_prefetch": stream.queued_prefetch,
                "completed": stream.completed,
                "cancelled": stream.cancelled,
                "stale": stream.stale,
                "failed": stream.failed,
                "materialized_empty": stream.materialized_empty,
                "complete": stream.complete,
            })
        })
        .collect::<Vec<_>>();
    let panel_global_chunks = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .map(|panel_id| {
            let counts = cross_section_global_panel_chunk_counts(app, panel_id);
            json!({
                "panel_id": panel_id.label(),
                "visible_chunks": counts.visible,
                "geometry_chunks": counts.geometry,
                "cpu_resident_chunks": counts.cpu_resident,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "data_engine_stats": app.dataset_runtime.dataset.stats().ok().map(data_engine_stats_json),
        "streaming_3d": {
            "brick_stream_generation": app.dataset_runtime.brick_stream_generation,
            "brick_stream_requested": app.dataset_runtime.brick_stream_requested,
            "brick_stream_completed": app.dataset_runtime.brick_stream_completed,
            "brick_stream_cancelled": app.dataset_runtime.brick_stream_cancelled,
            "brick_stream_stale": app.dataset_runtime.brick_stream_stale,
            "brick_stream_failed": app.dataset_runtime.brick_stream_failed,
            "brick_stream_complete": app.dataset_runtime.brick_stream_complete,
            "current_brick_ticket_count": app.dataset_runtime.current_brick_tickets.len(),
            "prefetch_brick_ticket_count": app.dataset_runtime.prefetch_brick_tickets.len(),
            "warm_brick_ticket_count": app.dataset_runtime.warm_brick_tickets.len(),
        },
        "cross_section_brick_ticket_count": app
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        "cross_section_streams": cross_section_streams,
        "cross_section_global_panel_chunks": panel_global_chunks,
    })
}

fn data_engine_stats_json(stats: DataEngineStats) -> Value {
    json!({
        "volume_cache_hits": stats.volume_cache_hits,
        "volume_cache_misses": stats.volume_cache_misses,
        "volume_cache_evictions": stats.volume_cache_evictions,
        "volume_cache_bytes": stats.volume_cache_bytes,
        "brick_cache_hits": stats.brick_cache_hits,
        "brick_cache_misses": stats.brick_cache_misses,
        "brick_cache_evictions": stats.brick_cache_evictions,
        "brick_cache_bytes": stats.brick_cache_bytes,
        "brick_cache_u8_bytes": stats.brick_cache_u8_bytes,
        "brick_cache_u16_bytes": stats.brick_cache_u16_bytes,
        "brick_cache_f32_bytes": stats.brick_cache_f32_bytes,
        "brick_reads": stats.brick_reads,
        "decoded_brick_values": stats.decoded_brick_values,
        "brick_requests_queued": stats.brick_requests_queued,
        "brick_requests_completed": stats.brick_requests_completed,
        "brick_requests_cancelled": stats.brick_requests_cancelled,
        "brick_requests_stale": stats.brick_requests_stale,
        "brick_requests_failed": stats.brick_requests_failed,
        "brick_queue_full": stats.brick_queue_full,
        "subset_reads": stats.subset_reads,
        "decoded_values": stats.decoded_values,
        "decoded_bytes": stats.decoded_bytes,
        "decoded_brick_bytes": stats.decoded_brick_bytes,
        "encoded_payload_bytes_read": stats.encoded_payload_bytes_read,
        "encoded_shard_payloads_read": stats.encoded_shard_payloads_read,
        "shard_index_cache_hits": stats.shard_index_cache_hits,
        "shard_index_cache_misses": stats.shard_index_cache_misses,
        "shard_index_cache_entries": stats.shard_index_cache_entries,
    })
}

#[derive(Debug, Clone, Copy, Default)]
struct CrossSectionGlobalPanelChunkCounts {
    candidate: usize,
    visible: usize,
    geometry: usize,
    cpu_resident: usize,
    cpu_resident_bytes: u64,
    cpu_only_resident: usize,
    cpu_only_resident_bytes: u64,
    upload_queued: usize,
    upload_queued_bytes: u64,
    gpu_resident: usize,
    gpu_resident_bytes: u64,
}

fn cross_section_global_panel_chunk_counts(
    app: &MiranteWorkbenchApp,
    panel_id: PanelId,
) -> CrossSectionGlobalPanelChunkCounts {
    let Some(panel) = app
        .render_runtime
        .cross_section_runtime
        .panels
        .get(&panel_id)
    else {
        return CrossSectionGlobalPanelChunkCounts::default();
    };
    let mut cpu_resident = 0usize;
    let mut cpu_resident_bytes = 0u64;
    let mut cpu_only_resident = 0usize;
    let mut cpu_only_resident_bytes = 0u64;
    let mut upload_queued = 0usize;
    let mut upload_queued_bytes = 0u64;
    let mut gpu_resident = 0usize;
    let mut gpu_resident_bytes = 0u64;
    for key in &panel.visible_chunks {
        let Some(entry) = app.render_runtime.cross_section_runtime.chunks.get(key) else {
            continue;
        };
        let Some(payload) = entry.payload.as_ref() else {
            continue;
        };
        let decoded_bytes = cross_section_payload_decoded_bytes(payload);
        match entry.state {
            CrossSectionChunkState::CpuResident => {
                cpu_only_resident = cpu_only_resident.saturating_add(1);
                cpu_only_resident_bytes = cpu_only_resident_bytes.saturating_add(decoded_bytes);
            }
            CrossSectionChunkState::UploadQueued => {
                upload_queued = upload_queued.saturating_add(1);
                upload_queued_bytes = upload_queued_bytes.saturating_add(decoded_bytes);
            }
            CrossSectionChunkState::GpuResident => {
                gpu_resident = gpu_resident.saturating_add(1);
                gpu_resident_bytes = gpu_resident_bytes.saturating_add(decoded_bytes);
            }
            CrossSectionChunkState::Absent
            | CrossSectionChunkState::Queued
            | CrossSectionChunkState::Decoding
            | CrossSectionChunkState::Failed
            | CrossSectionChunkState::Evicted => continue,
        }
        cpu_resident = cpu_resident.saturating_add(1);
        cpu_resident_bytes = cpu_resident_bytes.saturating_add(decoded_bytes);
    }
    CrossSectionGlobalPanelChunkCounts {
        candidate: panel.candidate_chunks,
        visible: panel.visible_chunks.len(),
        geometry: panel.visible_chunk_geometries.len(),
        cpu_resident,
        cpu_resident_bytes,
        cpu_only_resident,
        cpu_only_resident_bytes,
        upload_queued,
        upload_queued_bytes,
        gpu_resident,
        gpu_resident_bytes,
    }
}

fn assert_cross_section_panel_images_distinct(
    app: &MiranteWorkbenchApp,
    min_different_pixels: usize,
) -> Result<(), String> {
    let renderer = app
        .render_runtime
        .gpu_renderer
        .as_deref()
        .ok_or_else(|| "GPU renderer is unavailable for panel image readback".to_owned())?;
    let mut images = Vec::new();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let displayed = app
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)
            .ok_or_else(|| {
                format!(
                    "panel {} has no displayed cross-section frame",
                    panel_id.label()
                )
            })?;
        images.push(read_gpu_display_frame_image(
            renderer,
            panel_id.label(),
            &displayed.frame,
        )?);
    }
    assert_gpu_display_images_distinct("cross-section panels", &images, min_different_pixels)
}

fn assert_cross_section_streams_match_active_timepoint(
    app: &MiranteWorkbenchApp,
    min_completed: Option<usize>,
    min_visible_chunks: Option<usize>,
    max_failed: Option<usize>,
) -> Result<(), String> {
    let active_timepoint =
        application_view(&current_egui_shell_bridge::snapshot(&app.application)).timepoint();
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let stream = app
            .render_runtime
            .cross_section_runtime
            .panel_streams
            .get(&panel_id)
            .ok_or_else(|| format!("panel {} has no cross-section stream", panel_id.label()))?;
        if stream.request_key.timepoint != active_timepoint {
            return Err(format!(
                "panel {} stream timepoint is {}, expected active timepoint {}",
                panel_id.label(),
                stream.request_key.timepoint.get(),
                active_timepoint.get()
            ));
        }
        if let Some(min_completed) = min_completed
            && stream.completed < min_completed
        {
            return Err(format!(
                "panel {} stream completed {} chunks, expected at least {}",
                panel_id.label(),
                stream.completed,
                min_completed
            ));
        }
        if let Some(min_visible_chunks) = min_visible_chunks
            && stream.visible_chunks < min_visible_chunks
        {
            return Err(format!(
                "panel {} stream has {} visible chunks, expected at least {}",
                panel_id.label(),
                stream.visible_chunks,
                min_visible_chunks
            ));
        }
        if let Some(max_failed) = max_failed
            && stream.failed > max_failed
        {
            return Err(format!(
                "panel {} stream failed count is {}, expected at most {}",
                panel_id.label(),
                stream.failed,
                max_failed
            ));
        }
    }
    Ok(())
}

fn assert_cross_section_panel_nonblank(
    app: &MiranteWorkbenchApp,
    panel_id: PanelId,
    min_nonzero_rgb_pixels: usize,
) -> Result<(), String> {
    let renderer = app
        .render_runtime
        .gpu_renderer
        .as_deref()
        .ok_or_else(|| "GPU renderer is unavailable for panel image readback".to_owned())?;
    let displayed = app
        .render_runtime
        .cross_section_gpu_display_frames
        .get(&panel_id)
        .ok_or_else(|| {
            format!(
                "panel {} has no displayed cross-section frame",
                panel_id.label()
            )
        })?;
    let (label, width, height, rgba) =
        read_gpu_display_frame_image(renderer, panel_id.label(), &displayed.frame)?;
    let image = color_image_from_rgba(width, height, &rgba)?;
    let stats = ProductAutomationImageStats::from_color_image(&image);
    if stats.nonzero_rgb_pixels < min_nonzero_rgb_pixels || stats.max_rgb == 0 {
        return Err(format!(
            "{label} cross-section panel is blank: nonzero_rgb_pixels={}, max_rgb={}, expected at least {} nonzero pixels",
            stats.nonzero_rgb_pixels, stats.max_rgb, min_nonzero_rgb_pixels
        ));
    }
    Ok(())
}

fn assert_four_panel_images_distinct(
    app: &MiranteWorkbenchApp,
    min_different_pixels: usize,
) -> Result<(), String> {
    let renderer = app
        .render_runtime
        .gpu_renderer
        .as_deref()
        .ok_or_else(|| "GPU renderer is unavailable for panel image readback".to_owned())?;
    let mut images = Vec::new();
    let frame = app
        .render_runtime
        .gpu_display_frame
        .as_ref()
        .ok_or_else(|| "3D panel has no displayed GPU frame".to_owned())?;
    images.push(read_gpu_display_frame_image(renderer, "3D", frame)?);
    for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
        let displayed = app
            .render_runtime
            .cross_section_gpu_display_frames
            .get(&panel_id)
            .ok_or_else(|| {
                format!(
                    "panel {} has no displayed cross-section frame",
                    panel_id.label()
                )
            })?;
        images.push(read_gpu_display_frame_image(
            renderer,
            panel_id.label(),
            &displayed.frame,
        )?);
    }
    assert_gpu_display_images_distinct("four-panel frames", &images, min_different_pixels)
}

fn read_gpu_display_frame_image(
    renderer: &GpuRenderer,
    label: &str,
    frame: &GpuDisplayFrame,
) -> Result<(String, usize, usize, Vec<u8>), String> {
    let width = usize::try_from(frame.viewport.width).map_err(|_| {
        format!(
            "{label} frame width {} does not fit in usize",
            frame.viewport.width
        )
    })?;
    let height = usize::try_from(frame.viewport.height).map_err(|_| {
        format!(
            "{label} frame height {} does not fit in usize",
            frame.viewport.height
        )
    })?;
    let rgba = renderer
        .read_display_frame_rgba_for_diagnostics(frame)
        .map_err(|err| format!("failed to read {label} GPU frame: {err}"))?;
    Ok((label.to_owned(), width, height, rgba))
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
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let view = application_view(&snapshot);
    if view.layout() != ViewerLayout::Single3d {
        return Err(format!(
            "viewer layout is {:?}, expected Single3d for retired cross-section state",
            view.layout()
        ));
    }
    if !app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .is_empty()
    {
        return Err(format!(
            "cross-section streams are still active: {}",
            app.render_runtime.cross_section_runtime.panel_streams.len()
        ));
    }
    if app
        .render_runtime
        .cross_section_runtime
        .pending_read_ticket_count()
        != 0
    {
        return Err(format!(
            "cross-section brick tickets are still active: {}",
            app.render_runtime
                .cross_section_runtime
                .pending_read_ticket_count()
        ));
    }
    if app.render_runtime.cross_section_runtime.has_visible_work() {
        return Err("cross-section global runtime still has visible work".to_owned());
    }
    if !app
        .render_runtime
        .cross_section_gpu_display_frames
        .is_empty()
    {
        return Err(format!(
            "cross-section display frames are still active: {}",
            app.render_runtime.cross_section_gpu_display_frames.len()
        ));
    }
    Ok(())
}

fn cross_section_diagnostics_json(app: &MiranteWorkbenchApp) -> Value {
    let snapshot = current_egui_shell_bridge::snapshot(&app.application);
    let view = application_view(&snapshot);
    let panels = app
        .render_runtime
        .cross_section_runtime
        .panels()
        .map(|panel| {
            json!({
                "panel_id": panel.panel_id.label(),
                "generation": panel.generation,
                "displayed_generation": panel.displayed_generation,
                "display_current": panel.display_current(),
                "presentation_viewport": panel.presentation_viewport.map(|viewport| {
                    json!({
                        "width_points": viewport.width_points(),
                        "height_points": viewport.height_points(),
                    })
                }),
                "render_viewport": panel.render_viewport.map(|viewport| {
                    json!({
                        "width": viewport.width,
                        "height": viewport.height,
                    })
                }),
                "schedule": panel.cross_section_schedule.map(|schedule| {
                    json!({
                        "generation": schedule.generation,
                        "target_scale_level": schedule.target_scale_level,
                        "render_scale_level": schedule.render_scale_level,
                        "fallback_scale_level": schedule.fallback_scale_level,
                        "selected_bricks": schedule.selected_bricks,
                        "occupied_selected_bricks": schedule.occupied_selected_bricks,
                        "missing_occupied_bricks": schedule.missing_occupied_bricks,
                        "estimated_decoded_bytes": schedule.estimated_decoded_bytes,
                        "decoded_budget_bytes": schedule.decoded_budget_bytes,
                        "status": format!("{:?}", schedule.status),
                        "reason": format!("{:?}", schedule.reason),
                    })
                }),
            })
        })
        .collect::<Vec<_>>();
    let streams = app
        .render_runtime
        .cross_section_runtime
        .panel_streams
        .iter()
        .map(|(panel_id, stream)| {
            json!({
                "panel_id": panel_id.label(),
                "priority": format!("{:?}", stream.priority),
                "active_panel_at_submission": stream
                    .active_panel_at_submission
                    .map(|panel_id| panel_id.label().to_owned()),
                "fairness_promoted": stream.fairness_promoted,
                "requested": stream.requested,
                "deferred": stream.deferred,
                "queued_current_frame": stream.queued_current_frame,
                "queued_prefetch": stream.queued_prefetch,
                "completed": stream.completed,
                "cancelled": stream.cancelled,
                "stale": stream.stale,
                "failed": stream.failed,
                "materialized_empty": stream.materialized_empty,
                "visible_chunks": stream.visible_chunks,
                "occupied_visible_chunks": stream.occupied_visible_chunks,
                "decoded_bytes": stream.decoded_bytes,
                "encoded_payload_bytes_read": stream.encoded_payload_bytes_read,
                "last_error": stream.last_error.clone(),
                "complete": stream.complete,
            })
        })
        .collect::<Vec<_>>();
    let panel_resources = [PanelId::Xy, PanelId::Xz, PanelId::Yz]
        .into_iter()
        .map(|panel_id| {
            let chunk_counts = cross_section_global_panel_chunk_counts(app, panel_id);
            let display_frame = app
                .render_runtime
                .cross_section_gpu_display_frames
                .get(&panel_id)
                .map(|displayed| {
                    let diagnostics = displayed.frame.diagnostics;
                    json!({
                        "channels": diagnostics.channels,
                        "output_bytes": diagnostics.output_bytes,
                        "accumulator_bytes": diagnostics.accumulator_bytes,
                        "texture_bytes": diagnostics.texture_bytes,
                        "draw_calls": diagnostics.draw_calls,
                        "vertex_count": diagnostics.vertex_count,
                    })
                });
            json!({
                "panel_id": panel_id.label(),
                "candidate_chunks": chunk_counts.candidate,
                "visible_chunks": chunk_counts.visible,
                "geometry_chunks": chunk_counts.geometry,
                "cpu_resident_chunks": chunk_counts.cpu_resident,
                "cpu_resident_bytes": chunk_counts.cpu_resident_bytes,
                "cpu_only_resident_chunks": chunk_counts.cpu_only_resident,
                "cpu_only_resident_bytes": chunk_counts.cpu_only_resident_bytes,
                "upload_queued_chunks": chunk_counts.upload_queued,
                "upload_queued_bytes": chunk_counts.upload_queued_bytes,
                "gpu_resident_chunks": chunk_counts.gpu_resident,
                "gpu_resident_bytes": chunk_counts.gpu_resident_bytes,
                "missing_visible_chunks": chunk_counts.visible.saturating_sub(chunk_counts.cpu_resident),
                "display_frame_present": app.render_runtime.cross_section_gpu_display_frames.contains_key(&panel_id),
                "display_frame": display_frame,
            })
        })
        .collect::<Vec<_>>();
    let runtime_counts = cross_section_runtime_counts_json(app);
    let renderer_gpu = app
        .render_runtime
        .gpu_renderer
        .as_ref()
        .and_then(|renderer| renderer.stats().ok())
        .map(gpu_renderer_stats_json);
    json!({
        "schema": "mirante4d-cross-section-runtime-diagnostics",
        "schema_version": 1,
        "layout": format!("{:?}", view.layout()),
        "active_panel": snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(|panel_id| panel_id.label().to_owned()),
        "interaction_recent": cross_section_interaction_recent(&app.dataset_runtime),
        "last_interaction_age_ms": app
            .dataset_runtime
            .cross_section_last_interaction_at
            .map(|instant| instant.elapsed().as_millis()),
        "pending_ticket_count": app
            .render_runtime
            .cross_section_runtime
            .pending_read_ticket_count(),
        "display_frame_count": app.render_runtime.cross_section_gpu_display_frames.len(),
        "read_submission_budget_per_refresh": crate::cross_section_streaming::CROSS_SECTION_CHUNK_READ_SUBMISSIONS_PER_REFRESH,
        "product_display_path": "global_runtime_chunked_renderer",
        "old_path_fallback_used": false,
        "panel_local_resident_cache_authority": false,
        "renderer_gpu": renderer_gpu,
        "runtime": runtime_counts,
        "panel_resources": panel_resources,
        "panels": panels,
        "streams": streams,
    })
}

fn cross_section_runtime_counts_json(app: &MiranteWorkbenchApp) -> Value {
    let runtime = &app.render_runtime.cross_section_runtime;
    let mut absent = 0usize;
    let mut queued = 0usize;
    let mut decoding = 0usize;
    let mut cpu_resident = 0usize;
    let mut upload_queued = 0usize;
    let mut gpu_resident = 0usize;
    let mut failed = 0usize;
    let mut evicted = 0usize;
    let mut cpu_resident_bytes = 0u64;
    let mut cpu_only_resident_bytes = 0u64;
    let mut upload_queued_bytes = 0u64;
    let mut gpu_resident_bytes = 0u64;
    let mut decoded_bytes = 0u64;
    let mut encoded_payload_bytes_read = 0u64;
    for entry in runtime.chunks.values() {
        match entry.state {
            CrossSectionChunkState::Absent => absent = absent.saturating_add(1),
            CrossSectionChunkState::Queued => queued = queued.saturating_add(1),
            CrossSectionChunkState::Decoding => decoding = decoding.saturating_add(1),
            CrossSectionChunkState::CpuResident => cpu_resident = cpu_resident.saturating_add(1),
            CrossSectionChunkState::UploadQueued => upload_queued = upload_queued.saturating_add(1),
            CrossSectionChunkState::GpuResident => gpu_resident = gpu_resident.saturating_add(1),
            CrossSectionChunkState::Failed => failed = failed.saturating_add(1),
            CrossSectionChunkState::Evicted => evicted = evicted.saturating_add(1),
        }
        if matches!(
            entry.state,
            CrossSectionChunkState::CpuResident
                | CrossSectionChunkState::UploadQueued
                | CrossSectionChunkState::GpuResident
        ) && let Some(payload) = entry.payload.as_ref()
        {
            let payload_bytes = cross_section_payload_decoded_bytes(payload);
            cpu_resident_bytes = cpu_resident_bytes.saturating_add(payload_bytes);
            match entry.state {
                CrossSectionChunkState::CpuResident => {
                    cpu_only_resident_bytes = cpu_only_resident_bytes.saturating_add(payload_bytes);
                }
                CrossSectionChunkState::UploadQueued => {
                    upload_queued_bytes = upload_queued_bytes.saturating_add(payload_bytes);
                }
                CrossSectionChunkState::GpuResident => {
                    gpu_resident_bytes = gpu_resident_bytes.saturating_add(payload_bytes);
                }
                CrossSectionChunkState::Absent
                | CrossSectionChunkState::Queued
                | CrossSectionChunkState::Decoding
                | CrossSectionChunkState::Failed
                | CrossSectionChunkState::Evicted => {}
            }
        }
        decoded_bytes = decoded_bytes.saturating_add(entry.decoded_bytes);
        encoded_payload_bytes_read =
            encoded_payload_bytes_read.saturating_add(entry.encoded_payload_bytes_read);
    }
    json!({
        "global_chunks": runtime.chunks.len(),
        "panels": runtime.panels.len(),
        "visible_work": runtime.has_visible_work(),
        "cpu_payload_budget_bytes": runtime.cpu_payload_budget_bytes,
        "cpu_payload_eviction_passes": runtime.cpu_payload_eviction_passes,
        "cpu_payload_evicted_chunks": runtime.cpu_payload_evicted_chunks,
        "cpu_payload_evicted_bytes": runtime.cpu_payload_evicted_bytes,
        "cpu_payload_last_eviction": {
            "budget_bytes": runtime.cpu_payload_last_eviction.budget_bytes,
            "payload_bytes_before": runtime.cpu_payload_last_eviction.payload_bytes_before,
            "payload_bytes_after": runtime.cpu_payload_last_eviction.payload_bytes_after,
            "evicted_chunks": runtime.cpu_payload_last_eviction.evicted_chunks,
            "evicted_bytes": runtime.cpu_payload_last_eviction.evicted_bytes,
            "protected_visible_chunks": runtime.cpu_payload_last_eviction.protected_visible_chunks,
            "protected_visible_bytes": runtime.cpu_payload_last_eviction.protected_visible_bytes,
            "over_budget_after": runtime.cpu_payload_last_eviction.over_budget_after,
        },
        "queues": {
            "revision": runtime.queues.revision,
            "download_promotions": runtime.queues.download_promotions.entries().len(),
            "gpu_promotions": runtime.queues.gpu_promotions.entries().len(),
            "cpu_evictions": runtime.queues.cpu_evictions.entries().len(),
            "gpu_evictions": runtime.queues.gpu_evictions.entries().len(),
        },
        "state_counts": {
            "absent": absent,
            "queued": queued,
            "decoding": decoding,
            "cpu_resident": cpu_resident,
            "upload_queued": upload_queued,
            "gpu_resident": gpu_resident,
            "failed": failed,
            "evicted": evicted,
        },
        "cpu_resident_bytes": cpu_resident_bytes,
        "cpu_only_resident_bytes": cpu_only_resident_bytes,
        "upload_queued_bytes": upload_queued_bytes,
        "gpu_resident_bytes": gpu_resident_bytes,
        "decoded_bytes": decoded_bytes,
        "encoded_payload_bytes_read": encoded_payload_bytes_read,
    })
}

fn cross_section_payload_decoded_bytes(payload: &CrossSectionChunkPayload) -> u64 {
    match payload {
        CrossSectionChunkPayload::U8(brick) => cross_section_region_decoded_bytes(&brick.region, 1),
        CrossSectionChunkPayload::U16(brick) => {
            cross_section_region_decoded_bytes(&brick.region, 2)
        }
        CrossSectionChunkPayload::F32(brick) => {
            cross_section_region_decoded_bytes(&brick.region, 4)
        }
    }
}

fn cross_section_region_decoded_bytes(
    region: &mirante4d_data::VolumeRegion,
    bytes_per_voxel: u64,
) -> u64 {
    region
        .shape()
        .and_then(|shape| shape.element_count())
        .map(|values| values.saturating_mul(bytes_per_voxel))
        .unwrap_or(0)
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
}

enum AutomationStatus {
    Continue,
    Waiting,
    Finished,
    Failed(String),
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
