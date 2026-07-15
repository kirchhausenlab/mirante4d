use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, CameraView, CrossSectionView,
    DEFAULT_DVR_OPACITY_GAMMA, DvrOpacityTransfer, IsoShadingPolicy, LayerViewState,
    PresentationViewport, Projection, RenderMode, RenderState, SamplingPolicy,
    SourceVerificationSnapshot, TimeIndex, TransferCurve, ViewerLayout, channel_preset_from_view,
    import_workflow::{ImportCommand, ImportWorkflowSnapshot},
    next_user_channel_preset_id, stepped_timepoint,
    viewport_interaction::{fit_active_layer_camera, reset_active_layer_view},
};

use crate::{
    EguiUiState, StatusTone, ViewportHover, WorkbenchLayoutSpec, WorkbenchUiAction,
    WorkbenchUiOutput, elided_label, layer_row, muted_label, panel_scroll, property_row, section,
    status_badge, toolbar_button,
};

const DEFAULT_ISO_DISPLAY_LEVEL: f32 = 0.5;
const DEFAULT_DVR_DENSITY_SCALE: f64 = 12.0;
#[derive(Debug, Clone, Copy)]
pub struct ProjectControlsView<'a> {
    pub status_message: Option<&'a str>,
    pub dataset_open_pending: bool,
    pub project_store_idle: bool,
    pub can_new: bool,
    pub can_open: bool,
    pub can_save: bool,
    pub can_save_as: bool,
    pub recovery_available: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TopToolbarView<'a> {
    pub application: &'a ApplicationSnapshot,
    pub project: ProjectControlsView<'a>,
    pub presentation_viewport: PresentationViewport,
}

#[derive(Debug, Clone, Copy)]
pub struct LeftWorkbenchView<'a> {
    pub application: &'a ApplicationSnapshot,
    pub source_verification_available: bool,
    pub composite_fidelity: &'a str,
    pub dataset_path: &'a str,
}

pub fn show_top_toolbar(
    ui: &mut egui::Ui,
    view: TopToolbarView<'_>,
    output: &mut WorkbenchUiOutput,
) {
    let snapshot = view.application;
    let canonical_view = snapshot.view();
    let import_snapshot = snapshot.import_workflow();
    let import_active = matches!(import_snapshot, ImportWorkflowSnapshot::Importing(_));
    let workflow_busy = workflow_busy(snapshot);

    egui::Panel::top("top-toolbar").show_inside(ui, |ui| {
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("Mirante4D");
                ui.separator();
                if toolbar_button(
                    ui,
                    "Open",
                    !workflow_busy
                        && view.project.project_store_idle
                        && !view.project.dataset_open_pending,
                )
                .clicked()
                {
                    output.actions.push(WorkbenchUiAction::OpenDatasetDialog);
                }
                if toolbar_button(ui, "New Project", !workflow_busy && view.project.can_new)
                    .on_hover_text("Start an unsaved project for the verified dataset")
                    .clicked()
                {
                    output.actions.push(WorkbenchUiAction::NewProject);
                }
                if toolbar_button(ui, "Open Project", !workflow_busy && view.project.can_open)
                    .on_hover_text("Requires a verified scientific dataset identity")
                    .clicked()
                {
                    output.actions.push(WorkbenchUiAction::OpenProjectDialog);
                }
                if toolbar_button(ui, "Save Project", !workflow_busy && view.project.can_save)
                    .on_hover_text("Requires a verified scientific dataset identity")
                    .clicked()
                {
                    output.actions.push(WorkbenchUiAction::SaveProject);
                }
                if toolbar_button(
                    ui,
                    "Save Project As",
                    !workflow_busy && view.project.can_save_as,
                )
                .on_hover_text("Save a new project identity with exact fork provenance")
                .clicked()
                {
                    output.actions.push(WorkbenchUiAction::SaveProjectAs);
                }
                if toolbar_button(
                    ui,
                    "Recovery",
                    !workflow_busy && view.project.recovery_available,
                )
                .on_hover_text("List validated autosave and manual recovery branches")
                .clicked()
                {
                    output.actions.push(WorkbenchUiAction::OpenProjectRecovery);
                }
                if toolbar_button(ui, "Import Dir", !workflow_busy)
                    .on_hover_text("Import TIFF directory")
                    .clicked()
                {
                    output
                        .actions
                        .push(WorkbenchUiAction::ImportTiffDirectoryDialog);
                }
                if toolbar_button(ui, "Import File", !workflow_busy)
                    .on_hover_text("Import TIFF file")
                    .clicked()
                {
                    output.actions.push(WorkbenchUiAction::ImportTiffFileDialog);
                }
                if import_active {
                    let cancellation_pending = matches!(
                        import_snapshot,
                        ImportWorkflowSnapshot::Importing(execution)
                            if execution.cancellation_requested
                    );
                    if toolbar_button(
                        ui,
                        if cancellation_pending {
                            "Stopping Import"
                        } else {
                            "Cancel Import"
                        },
                        !cancellation_pending,
                    )
                    .clicked()
                    {
                        output.import_commands.push(ImportCommand::CancelImport);
                    }
                }
                ui.separator();
                elided_label(ui, snapshot.catalog().label(), 42);
            });
            if let Some(message) = view.project.status_message {
                muted_label(ui, message);
            }
            ui.horizontal_wrapped(|ui| {
                layout_selector(
                    ui,
                    canonical_view.layout(),
                    *canonical_view.cross_section(),
                    &mut output.application_commands,
                );
                ui.separator();
                muted_label(ui, "Render");
                if let Some(command) = render_mode_selector(ui, canonical_view) {
                    output.application_commands.push(command);
                }
                ui.separator();
                muted_label(ui, "Camera");
                if let Some(command) = projection_selector(ui, *canonical_view.camera()) {
                    output.application_commands.push(command);
                }
                if ui.button("Fit Data").clicked() {
                    output
                        .application_commands
                        .push(ApplicationCommand::SetCamera(fit_active_layer_camera(
                            snapshot,
                            view.presentation_viewport,
                        )));
                }
                if ui.button("Reset View").clicked() {
                    output
                        .application_commands
                        .push(ApplicationCommand::ReplaceView(reset_active_layer_view(
                            snapshot,
                            view.presentation_viewport,
                        )));
                }
            });
        });
    });
}

pub fn show_left_workbench_panel(
    ui: &mut egui::Ui,
    view: LeftWorkbenchView<'_>,
    state: &EguiUiState,
    layout: WorkbenchLayoutSpec,
    output: &mut WorkbenchUiOutput,
) {
    let snapshot = view.application;
    let canonical_view = snapshot.view();
    let import_snapshot = snapshot.import_workflow();
    let workflow_busy = workflow_busy(snapshot);
    let timepoint_count = snapshot.timepoint_count();

    egui::Panel::left("layers-panel")
        .resizable(true)
        .default_size(layout.left_panel_width)
        .size_range(layout.left_width_range())
        .show_inside(ui, |ui| {
            panel_scroll(ui, "layers-panel-scroll", |ui| {
                section(ui, "Dataset", |ui| {
                    property_row(ui, "name", snapshot.catalog().label());
                    property_row(ui, "layers", snapshot.catalog().len().to_string());
                    property_row(ui, "timepoints", timepoint_count.to_string());
                    property_row(
                        ui,
                        "scientific identity",
                        source_verification_label(snapshot.source()),
                    );
                    if let SourceVerificationSnapshot::Verifying { operation_id, .. } =
                        snapshot.source()
                        && toolbar_button(ui, "Cancel Verification", true).clicked()
                    {
                        output
                            .application_commands
                            .push(ApplicationCommand::CancelOperation(*operation_id));
                    }
                    if matches!(snapshot.source(), SourceVerificationSnapshot::Required)
                        && toolbar_button(ui, "Verify Source", view.source_verification_available)
                            .clicked()
                    {
                        output
                            .application_commands
                            .push(ApplicationCommand::RequestSourceVerification);
                    }
                });
                section(ui, "Status", |ui| {
                    show_import_status(ui, import_snapshot);
                    property_row(ui, "fidelity", view.composite_fidelity);
                    if let Some(hover) = state.hovered_pixel {
                        property_row(ui, "hover", viewport_hover_status_label(hover));
                    }
                    if let Some(readout) = &state.hovered_source_readout {
                        property_row(ui, "readout", readout);
                    }
                    property_row(
                        ui,
                        "playback",
                        playback_status_label(
                            snapshot.transient().playback_active(),
                            canonical_view.timepoint(),
                            timepoint_count,
                        ),
                    );
                    property_row(ui, "path", view.dataset_path);
                    ui.horizontal_wrapped(|ui| {
                        show_playback_controls(
                            ui,
                            snapshot,
                            workflow_busy,
                            &mut output.application_commands,
                        );
                    });
                });
                section(ui, "Layers", |ui| {
                    for layer in canonical_view.layers() {
                        let catalog_layer = snapshot
                            .catalog()
                            .layer(layer.layer_key())
                            .expect("application view closes over the dataset catalog");
                        let selected = layer.layer_key() == canonical_view.active_layer();
                        let detail = format!(
                            "{} {:?} t{} z{} y{} x{}",
                            layer.layer_key().ordinal(),
                            catalog_layer.dtype(),
                            catalog_layer.shape().t(),
                            catalog_layer.shape().z(),
                            catalog_layer.shape().y(),
                            catalog_layer.shape().x()
                        );
                        ui.horizontal(|ui| {
                            let mut visible = layer.visible();
                            if ui
                                .checkbox(&mut visible, "")
                                .on_hover_text(format!("Show {}", catalog_layer.label()))
                                .changed()
                            {
                                output
                                    .application_commands
                                    .push(ApplicationCommand::SetLayerView(LayerViewState::new(
                                        layer.layer_key(),
                                        visible,
                                        layer.transfer().clone(),
                                        *layer.render_state(),
                                    )));
                            }
                            if layer_row(
                                ui,
                                selected,
                                layer.visible(),
                                catalog_layer.label(),
                                &detail,
                            )
                            .clicked()
                                && !selected
                            {
                                output
                                    .application_commands
                                    .push(ApplicationCommand::SetActiveLayer(layer.layer_key()));
                            }
                            let mut mode = layer.render_state().mode();
                            egui::ComboBox::from_id_salt(format!(
                                "layer-render-mode-{}",
                                layer.layer_key().ordinal()
                            ))
                            .selected_text(render_mode_label(mode))
                            .width(72.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut mode, RenderMode::Mip, "MIP");
                                ui.selectable_value(&mut mode, RenderMode::Isosurface, "ISO");
                                ui.selectable_value(&mut mode, RenderMode::Dvr, "DVR");
                            });
                            if mode != layer.render_state().mode() {
                                match layer_render_state_for_mode(layer, mode) {
                                    Ok(render_state) => output.application_commands.push(
                                        ApplicationCommand::SetLayerView(LayerViewState::new(
                                            layer.layer_key(),
                                            layer.visible(),
                                            layer.transfer().clone(),
                                            render_state,
                                        )),
                                    ),
                                    Err(error) => {
                                        tracing::warn!(%error, "render mode change rejected");
                                    }
                                }
                            }
                        });
                    }
                    property_row(
                        ui,
                        "active ID",
                        canonical_view.active_layer().ordinal().to_string(),
                    );
                });
                section(ui, "Channel Presets", |ui| {
                    let presets = snapshot.channel_presets();
                    if presets.is_empty() {
                        status_badge(ui, StatusTone::Warning, "no channel presets");
                        return;
                    }
                    let selected = snapshot
                        .transient()
                        .selected_channel_preset()
                        .and_then(|id| presets.iter().find(|preset| preset.id() == id))
                        .unwrap_or(&presets[0]);
                    egui::ComboBox::from_label("channel preset")
                        .selected_text(selected.label())
                        .show_ui(ui, |ui| {
                            for preset in presets {
                                if ui
                                    .selectable_label(preset.id() == selected.id(), preset.label())
                                    .clicked()
                                {
                                    output.application_commands.push(
                                        ApplicationCommand::ApplyChannelPreset(preset.id().clone()),
                                    );
                                }
                            }
                        });
                    ui.horizontal_wrapped(|ui| {
                        if toolbar_button(ui, "Apply", true).clicked() {
                            output.application_commands.push(
                                ApplicationCommand::ApplyChannelPreset(selected.id().clone()),
                            );
                        }
                        if toolbar_button(ui, "Save Current", true).clicked() {
                            let id = next_user_channel_preset_id(presets);
                            let label = format!("Display {}", presets.len() + 1);
                            match channel_preset_from_view(canonical_view, id, label) {
                                Ok(preset) => output
                                    .application_commands
                                    .push(ApplicationCommand::UpsertChannelPreset(preset)),
                                Err(error) => {
                                    tracing::warn!(%error, "channel preset creation rejected");
                                }
                            }
                        }
                        if toolbar_button(ui, "Update", true).clicked() {
                            match channel_preset_from_view(
                                canonical_view,
                                selected.id().clone(),
                                selected.label(),
                            ) {
                                Ok(preset) => output
                                    .application_commands
                                    .push(ApplicationCommand::UpsertChannelPreset(preset)),
                                Err(error) => {
                                    tracing::warn!(%error, "channel preset update rejected");
                                }
                            }
                        }
                    });
                });
            });
        });
}

fn workflow_busy(snapshot: &ApplicationSnapshot) -> bool {
    !matches!(snapshot.import_workflow(), ImportWorkflowSnapshot::Idle)
        || snapshot
            .active_operations()
            .iter()
            .any(|token| token.kind() == mirante4d_application::OperationKind::DatasetOpen)
}

fn source_verification_label(source: &SourceVerificationSnapshot) -> String {
    match source {
        SourceVerificationSnapshot::Required => {
            "verification required; project open/save unavailable".to_owned()
        }
        SourceVerificationSnapshot::Verifying {
            completed_work,
            total_work,
            ..
        } => completed_work
            .saturating_mul(100)
            .checked_div(*total_work)
            .map_or_else(
                || "verifying".to_owned(),
                |percent| format!("verifying ({percent}%)"),
            ),
        SourceVerificationSnapshot::Verified(_) => "verified".to_owned(),
    }
}

fn show_import_status(ui: &mut egui::Ui, snapshot: &ImportWorkflowSnapshot) {
    match snapshot {
        ImportWorkflowSnapshot::Importing(execution) => status_badge(
            ui,
            StatusTone::Warning,
            if execution.cancellation_requested {
                "stopping TIFF import"
            } else {
                "importing TIFF"
            },
        ),
        ImportWorkflowSnapshot::Inspecting(_) => {
            status_badge(ui, StatusTone::Warning, "inspecting TIFF input");
        }
        ImportWorkflowSnapshot::Review(_) => {
            status_badge(ui, StatusTone::Warning, "review TIFF import settings");
        }
        ImportWorkflowSnapshot::Failed(_) => {
            status_badge(ui, StatusTone::Error, "TIFF import needs attention");
        }
        ImportWorkflowSnapshot::Idle => status_badge(ui, StatusTone::Ready, "ready"),
    }
}

fn layout_selector(
    ui: &mut egui::Ui,
    current: ViewerLayout,
    cross_section: CrossSectionView,
    commands: &mut Vec<ApplicationCommand>,
) {
    muted_label(ui, "Layout");
    if ui
        .selectable_label(current == ViewerLayout::Single3d, "3D")
        .clicked()
    {
        commands.push(ApplicationCommand::SetLayout {
            layout: ViewerLayout::Single3d,
            cross_section,
        });
    }
    if ui
        .selectable_label(current == ViewerLayout::FourPanel, "4 Panel")
        .clicked()
    {
        commands.push(ApplicationCommand::SetLayout {
            layout: ViewerLayout::FourPanel,
            cross_section,
        });
    }
}

fn render_mode_selector(
    ui: &mut egui::Ui,
    view: &mirante4d_application::ViewState,
) -> Option<ApplicationCommand> {
    let current_layer = view
        .layer(view.active_layer())
        .expect("ViewState contains its active layer by construction");
    let current_mode = current_layer.render_state().mode();
    let mut mode = current_mode;
    egui::ComboBox::from_id_salt("render-mode-selector")
        .selected_text(render_mode_label(mode))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut mode, RenderMode::Mip, "MIP");
            ui.selectable_value(&mut mode, RenderMode::Isosurface, "ISO");
            ui.selectable_value(&mut mode, RenderMode::Dvr, "DVR");
        });
    (mode != current_mode).then(|| {
        ApplicationCommand::SetLayerView(LayerViewState::new(
            current_layer.layer_key(),
            current_layer.visible(),
            current_layer.transfer().clone(),
            render_state_for_mode(current_layer, mode),
        ))
    })
}

fn render_mode_label(mode: RenderMode) -> &'static str {
    match mode {
        RenderMode::Mip => "MIP",
        RenderMode::Isosurface => "ISO",
        RenderMode::Dvr => "DVR",
    }
}

fn projection_selector(ui: &mut egui::Ui, camera: CameraView) -> Option<ApplicationCommand> {
    let current_projection = camera.projection();
    let mut projection = current_projection;
    egui::ComboBox::from_id_salt("projection-selector")
        .selected_text(format!("{projection:?}"))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut projection, Projection::Orthographic, "Orthographic");
            ui.selectable_value(&mut projection, Projection::Perspective, "Perspective");
        });
    (projection != current_projection).then(|| {
        ApplicationCommand::SetCamera(
            CameraView::new(
                projection,
                camera.target(),
                camera.orientation(),
                camera.orthographic_world_per_screen_point(),
                camera.perspective_focal_length_screen_points(),
                camera.perspective_view_distance_world(),
            )
            .expect("changing projection preserves validated camera invariants"),
        )
    })
}

fn render_state_for_mode(layer: &LayerViewState, mode: RenderMode) -> RenderState {
    let current = *layer.render_state();
    let sampling = SamplingPolicy::VoxelExact;
    match mode {
        RenderMode::Mip => RenderState::mip(sampling),
        RenderMode::Isosurface => {
            let display_level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(DEFAULT_ISO_DISPLAY_LEVEL);
            RenderState::iso(sampling, IsoShadingPolicy::Flat, display_level)
                .expect("the retained or default ISO parameters are valid")
        }
        RenderMode::Dvr => {
            let (opacity_transfer, density_scale) = current
                .dvr_parameters()
                .map(|parameters| (parameters.opacity_transfer(), parameters.density_scale()))
                .unwrap_or_else(|| {
                    (
                        DvrOpacityTransfer::new(
                            layer.transfer().window(),
                            TransferCurve::gamma(DEFAULT_DVR_OPACITY_GAMMA)
                                .expect("the default DVR opacity gamma is valid"),
                        ),
                        DEFAULT_DVR_DENSITY_SCALE,
                    )
                });
            RenderState::dvr(sampling, opacity_transfer, density_scale)
                .expect("the retained or default DVR parameters are valid")
        }
    }
}

fn layer_render_state_for_mode(
    layer: &LayerViewState,
    mode: RenderMode,
) -> Result<RenderState, String> {
    let current = *layer.render_state();
    let sampling = SamplingPolicy::VoxelExact;
    match mode {
        RenderMode::Mip => Ok(RenderState::mip(sampling)),
        RenderMode::Isosurface => {
            let display_level = current
                .iso_parameters()
                .map(|parameters| parameters.display_level())
                .unwrap_or(DEFAULT_ISO_DISPLAY_LEVEL);
            RenderState::iso(sampling, IsoShadingPolicy::Flat, display_level)
                .map_err(|error| error.to_string())
        }
        RenderMode::Dvr => {
            let (opacity_transfer, density_scale) = current
                .dvr_parameters()
                .map(|parameters| (parameters.opacity_transfer(), parameters.density_scale()))
                .unwrap_or((
                    DvrOpacityTransfer::new(layer.transfer().window(), layer.transfer().curve()),
                    DEFAULT_DVR_DENSITY_SCALE,
                ));
            RenderState::dvr(sampling, opacity_transfer, density_scale)
                .map_err(|error| error.to_string())
        }
    }
}

fn show_playback_controls(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    workflow_busy: bool,
    commands: &mut Vec<ApplicationCommand>,
) {
    let view = snapshot.view();
    let active_timepoint = view.timepoint();
    let timepoint_count = snapshot.timepoint_count();
    if timepoint_count <= 1 {
        muted_label(ui, "time t 1/1");
        return;
    }
    if toolbar_button(ui, "First", !workflow_busy).clicked() {
        commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(0)));
    }
    if toolbar_button(ui, "Prev", !workflow_busy).clicked() {
        commands.push(ApplicationCommand::SetTimepoint(stepped_timepoint(
            active_timepoint,
            timepoint_count,
            -1,
        )));
    }
    let playback_active = snapshot.transient().playback_active();
    if toolbar_button(
        ui,
        if playback_active { "Pause" } else { "Play" },
        !workflow_busy,
    )
    .clicked()
    {
        commands.push(ApplicationCommand::SetPlaybackActive(!playback_active));
    }
    if toolbar_button(ui, "Next", !workflow_busy).clicked() {
        commands.push(ApplicationCommand::SetTimepoint(stepped_timepoint(
            active_timepoint,
            timepoint_count,
            1,
        )));
    }
    if toolbar_button(ui, "Last", !workflow_busy).clicked() {
        commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(
            timepoint_count - 1,
        )));
    }

    let mut timepoint = active_timepoint.get().min(timepoint_count - 1);
    let slider_width = ui.available_width().clamp(120.0, 360.0);
    let response = ui.add_sized(
        [
            slider_width,
            crate::UiTokens::default().spacing.control_height,
        ],
        egui::Slider::new(&mut timepoint, 0..=timepoint_count - 1).text("t"),
    );
    if response.changed() {
        commands.push(ApplicationCommand::SetTimepoint(TimeIndex::new(timepoint)));
    }
    muted_label(
        ui,
        format!("t {}/{}", active_timepoint.get() + 1, timepoint_count),
    );
}

pub fn playback_status_label(playing: bool, active: TimeIndex, count: u64) -> String {
    if count <= 1 {
        return "playback stopped | t 1/1".to_owned();
    }
    let state = if playing { "playing" } else { "stopped" };
    format!("playback {state} | t {}/{}", active.get() + 1, count)
}

pub fn viewport_hover_status_label(hover: ViewportHover) -> String {
    format!(
        "hover x{} y{} intensity {}",
        hover.x, hover.y, hover.intensity
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepped_timepoint_wraps_in_both_directions() {
        assert_eq!(
            stepped_timepoint(TimeIndex::new(0), 0, 1),
            TimeIndex::new(0)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(0), 3, -1),
            TimeIndex::new(2)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(2), 3, 1),
            TimeIndex::new(0)
        );
        assert_eq!(
            stepped_timepoint(TimeIndex::new(1), 3, 5),
            TimeIndex::new(0)
        );
    }

    #[test]
    fn playback_status_reports_state_and_timepoint() {
        assert_eq!(
            playback_status_label(false, TimeIndex::new(0), 3),
            "playback stopped | t 1/3"
        );
        assert_eq!(
            playback_status_label(true, TimeIndex::new(1), 3),
            "playback playing | t 2/3"
        );
        assert_eq!(
            playback_status_label(true, TimeIndex::new(0), 1),
            "playback stopped | t 1/1"
        );
    }

    #[test]
    fn viewport_hover_label_exposes_pixel_intensity() {
        assert_eq!(
            viewport_hover_status_label(ViewportHover {
                x: 12,
                y: 34,
                intensity: crate::ViewportIntensity::U16(4095),
            }),
            "hover x12 y34 intensity 4095"
        );
    }
}
