//! Egui presentation components for Mirante4D.

#![forbid(unsafe_code)]

mod fidelity;

pub use fidelity::{frame_fidelity_label, show_frame_fidelity_property_rows};

use std::{fmt::Display, hash::Hash, time::Duration};

use eframe::egui::{self, Color32, RichText};
use mirante4d_application::{
    ApplicationCommand, ApplicationEvent, CrossSectionPanelId, OperationOutcome,
    PresentationPaintRequest, PresentationSlot, PresentationSurface, PresentationViewport,
    ProjectGenerationId, ProjectId, RenderExtent,
    import_workflow::{
        ImportCommand, ImportProgressSnapshot, ImportReviewDraft, ImportReviewId,
        ImportReviewSnapshot, ImportSourceDtype, ImportSourceLayout, ImportWorkflowSnapshot,
    },
    viewer_tools::ViewerToolState,
    viewport_interaction::ViewportOrbitDrag,
};

/// Egui-local draft values and interaction state.
#[derive(Debug)]
pub struct EguiUiState {
    pub viewport_orbit_drag: Option<ViewportOrbitDrag>,
    pub analysis_plot_view: Option<AnalysisPlotViewRange>,
    pub analysis_filter: String,
    pub analysis_sort: Option<AnalysisTableSort>,
    pub viewer_tools: ViewerToolState,
    pub hovered_pixel: Option<ViewportHover>,
    pub hovered_source_readout: Option<String>,
    pub close_prompt_open: bool,
    pub allow_close_without_prompt: bool,
    pub settings_runtime_draft: ResourcePolicyDraft,
    pub analysis_workspace_open: bool,
    import_review: Option<ImportReviewUiState>,
    import_checkpoint_reset_confirmed: bool,
    import_checkpoint_retry_id: Option<ImportReviewId>,
}

impl EguiUiState {
    pub fn new(cpu_dataset_budget_bytes: u64, gpu_budget_bytes: u64) -> Self {
        Self {
            viewport_orbit_drag: None,
            analysis_plot_view: None,
            analysis_filter: String::new(),
            analysis_sort: None,
            viewer_tools: ViewerToolState::default(),
            hovered_pixel: None,
            hovered_source_readout: None,
            close_prompt_open: false,
            allow_close_without_prompt: false,
            settings_runtime_draft: ResourcePolicyDraft {
                cpu_dataset_budget_bytes,
                gpu_budget_bytes,
            },
            analysis_workspace_open: false,
            import_review: None,
            import_checkpoint_reset_confirmed: false,
            import_checkpoint_retry_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ImportReviewUiState {
    review_id: ImportReviewId,
    draft: ImportReviewDraft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourcePolicyDraft {
    pub cpu_dataset_budget_bytes: u64,
    pub gpu_budget_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisTableSort {
    pub column_key: String,
    pub ascending: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalysisPlotViewRange {
    pub plot_index: usize,
    pub min_x: f64,
    pub max_x: f64,
    pub min_y: f64,
    pub max_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportHover {
    pub x: u64,
    pub y: u64,
    pub intensity: ViewportIntensity,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewportIntensity {
    U8(u8),
    U16(u16),
    F32(f32),
}

impl Display for ViewportIntensity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::U8(value) => write!(formatter, "{value}"),
            Self::U16(value) => write!(formatter, "{value}"),
            Self::F32(value) => write!(formatter, "{value:.6}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiColors {
    pub app_background: Color32,
    pub panel_background: Color32,
    pub panel_background_alt: Color32,
    pub viewport_background: Color32,
    pub border: Color32,
    pub text_primary: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub status_ready: Color32,
    pub status_warning: Color32,
    pub status_error: Color32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UiSpacing {
    pub panel_margin: f32,
    pub section_gap: f32,
    pub row_gap: f32,
    pub property_label_width: f32,
    pub control_height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UiTokens {
    pub colors: UiColors,
    pub spacing: UiSpacing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkbenchLayoutSpec {
    pub top_bar_height: f32,
    pub bottom_strip_height: f32,
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub min_left_panel_width: f32,
    pub max_left_panel_width: f32,
    pub min_right_panel_width: f32,
    pub max_right_panel_width: f32,
    pub min_viewport_width: f32,
    pub min_viewport_height: f32,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkbenchLayoutReport {
    pub window_size_points: egui::Vec2,
    pub pixels_per_point: f32,
    pub top_bar_height: f32,
    pub bottom_strip_height: f32,
    pub left_panel_width: f32,
    pub right_panel_width: f32,
    pub viewport_size_points: egui::Vec2,
    pub viewport_size_pixels: egui::Vec2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Ready,
    Warning,
    Error,
}

/// One egui rectangle reserved for a backend-neutral presentation request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EguiPresentationPaint {
    slot: PresentationSlot,
    request: PresentationPaintRequest,
    rect: egui::Rect,
}

/// Analysis operation requested by a workbench widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchAnalysisKind {
    FullTimeTrace,
    CurrentTimepointBox,
}

/// Work requested by a workbench widget.
///
/// These actions contain no filesystem paths, service handles, or backend
/// resources. The process composition root performs them after egui has
/// finished building the workbench.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchUiAction {
    OpenDatasetDialog,
    NewProject,
    OpenProjectDialog,
    SaveProject,
    SaveProjectAs,
    OpenProjectRecovery,
    ImportTiffDirectoryDialog,
    ImportTiffFileDialog,
    CopySelectedAnalysisCsv,
    CancelAnalysis,
    SetAnalysisRoi { origin: [u64; 3], shape: [u64; 3] },
    StartAnalysis(WorkbenchAnalysisKind),
    SaveSettings(ResourcePolicyDraft),
    ReplaceRejectedSettings(ResourcePolicyDraft),
    UseRecommendedSettings,
    SaveDirtyProject,
    SaveDirtyProjectAs,
    DiscardDirtyProject,
    CancelDirtyProjectClose,
    RecoverReviewedAutosave(ProjectGenerationId),
    AcceptSavedProjectAfterRecoveryReview,
    CloseProjectRecoveryPanel,
    OpenRecoveryCandidate(ProjectGenerationId),
    OpenRecoveryLocator(ProjectId),
    CopyDiagnostics,
}

/// One validated viewport measurement observed while egui lays out a panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportObservation {
    slot: PresentationSlot,
    presentation: PresentationViewport,
    render: RenderExtent,
}

impl ViewportObservation {
    pub const fn new(
        slot: PresentationSlot,
        presentation: PresentationViewport,
        render: RenderExtent,
    ) -> Self {
        Self {
            slot,
            presentation,
            render,
        }
    }

    pub const fn slot(self) -> PresentationSlot {
        self.slot
    }

    pub const fn presentation(self) -> PresentationViewport {
        self.presentation
    }

    pub const fn render(self) -> RenderExtent {
        self.render
    }
}

/// Rendering work requested after egui finishes building a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderUiRequest {
    EnsureCrossSectionCurrent { panel: CrossSectionPanelId },
}

impl RenderUiRequest {
    pub const fn cross_section_panel(self) -> CrossSectionPanelId {
        match self {
            Self::EnsureCrossSectionCurrent { panel } => panel,
        }
    }
}

/// One hovered cross-section point to resolve against retained source data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossSectionReadoutRequest {
    panel: CrossSectionPanelId,
    presentation: PresentationViewport,
    normalized_point: [f64; 2],
}

impl CrossSectionReadoutRequest {
    pub fn new(
        panel: CrossSectionPanelId,
        presentation: PresentationViewport,
        normalized_point: [f64; 2],
    ) -> Option<Self> {
        normalized_point
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
            .then_some(Self {
                panel,
                presentation,
                normalized_point,
            })
    }

    pub fn from_response(
        panel: CrossSectionPanelId,
        presentation: PresentationViewport,
        response: &egui::Response,
    ) -> Option<Self> {
        if !response.hovered() || response.rect.width() <= 0.0 || response.rect.height() <= 0.0 {
            return None;
        }
        let position = response.hover_pos()?;
        if !response.rect.contains(position) {
            return None;
        }
        let normalized_x =
            ((position.x - response.rect.min.x) / response.rect.width()).clamp(0.0, 1.0);
        let normalized_y =
            ((position.y - response.rect.min.y) / response.rect.height()).clamp(0.0, 1.0);
        Self::new(
            panel,
            presentation,
            [f64::from(normalized_x), f64::from(normalized_y)],
        )
    }

    pub const fn panel(self) -> CrossSectionPanelId {
        self.panel
    }

    pub const fn presentation(self) -> PresentationViewport {
        self.presentation
    }

    pub const fn normalized_point(self) -> [f64; 2] {
        self.normalized_point
    }
}

/// Typed results emitted while egui builds one visible workbench frame.
///
/// Commands retain their existing post-build batching behavior. Actions and
/// backend-neutral paint requests are likewise resolved only by the process
/// composition root.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WorkbenchUiOutput {
    pub application_commands: Vec<ApplicationCommand>,
    pub import_commands: Vec<ImportCommand>,
    pub actions: Vec<WorkbenchUiAction>,
    pub viewport_observations: Vec<ViewportObservation>,
    pub cross_section_readout_requests: Vec<CrossSectionReadoutRequest>,
    pub render_requests: Vec<RenderUiRequest>,
    pub presentation_paints: Vec<EguiPresentationPaint>,
    pub rerender_requested: bool,
    pub texture_refresh_requested: bool,
    pub repaint_after: Option<Duration>,
}

impl WorkbenchUiOutput {
    pub fn request_repaint_after(&mut self, delay: Duration) {
        self.repaint_after = Some(
            self.repaint_after
                .map_or(delay, |current| current.min(delay)),
        );
    }
}

impl EguiPresentationPaint {
    pub const fn new(
        slot: PresentationSlot,
        request: PresentationPaintRequest,
        rect: egui::Rect,
    ) -> Self {
        Self {
            slot,
            request,
            rect,
        }
    }

    pub const fn slot(self) -> PresentationSlot {
        self.slot
    }

    pub const fn request(self) -> PresentationPaintRequest {
        self.request
    }

    pub const fn rect(self) -> egui::Rect {
        self.rect
    }
}

impl Default for UiColors {
    fn default() -> Self {
        Self {
            app_background: Color32::from_rgb(16, 18, 21),
            panel_background: Color32::from_rgb(24, 26, 29),
            panel_background_alt: Color32::from_rgb(28, 31, 35),
            viewport_background: Color32::from_rgb(8, 10, 13),
            border: Color32::from_rgb(58, 64, 72),
            text_primary: Color32::from_rgb(232, 236, 241),
            text_muted: Color32::from_rgb(156, 164, 174),
            accent: Color32::from_rgb(92, 177, 188),
            status_ready: Color32::from_rgb(93, 179, 128),
            status_warning: Color32::from_rgb(222, 170, 76),
            status_error: Color32::from_rgb(224, 102, 102),
        }
    }
}

impl Default for UiSpacing {
    fn default() -> Self {
        Self {
            panel_margin: 8.0,
            section_gap: 10.0,
            row_gap: 4.0,
            property_label_width: 96.0,
            control_height: 24.0,
        }
    }
}

impl Default for WorkbenchLayoutSpec {
    fn default() -> Self {
        Self {
            top_bar_height: 34.0,
            bottom_strip_height: 0.0,
            left_panel_width: 260.0,
            right_panel_width: 340.0,
            min_left_panel_width: 220.0,
            max_left_panel_width: 360.0,
            min_right_panel_width: 300.0,
            max_right_panel_width: 420.0,
            min_viewport_width: 320.0,
            min_viewport_height: 240.0,
        }
    }
}

impl WorkbenchLayoutSpec {
    #[cfg(test)]
    pub fn report(
        self,
        window_size_points: egui::Vec2,
        pixels_per_point: f32,
    ) -> Option<WorkbenchLayoutReport> {
        if window_size_points.x <= 0.0
            || window_size_points.y <= 0.0
            || pixels_per_point <= 0.0
            || !window_size_points.x.is_finite()
            || !window_size_points.y.is_finite()
            || !pixels_per_point.is_finite()
        {
            return None;
        }

        let viewport_width = window_size_points.x - self.left_panel_width - self.right_panel_width;
        let viewport_height = window_size_points.y - self.top_bar_height - self.bottom_strip_height;
        if viewport_width < self.min_viewport_width || viewport_height < self.min_viewport_height {
            return None;
        }

        let viewport_size_points = egui::vec2(viewport_width, viewport_height);
        Some(WorkbenchLayoutReport {
            window_size_points,
            pixels_per_point,
            top_bar_height: self.top_bar_height,
            bottom_strip_height: self.bottom_strip_height,
            left_panel_width: self.left_panel_width,
            right_panel_width: self.right_panel_width,
            viewport_size_points,
            viewport_size_pixels: viewport_size_points * pixels_per_point,
        })
    }

    pub fn left_width_range(self) -> std::ops::RangeInclusive<f32> {
        self.min_left_panel_width..=self.max_left_panel_width
    }

    pub fn right_width_range(self) -> std::ops::RangeInclusive<f32> {
        self.min_right_panel_width..=self.max_right_panel_width
    }
}

pub fn configure_visuals(ctx: &egui::Context) {
    let tokens = UiTokens::default();
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = tokens.colors.panel_background;
    visuals.window_fill = tokens.colors.panel_background_alt;
    visuals.extreme_bg_color = tokens.colors.app_background;
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(38, 41, 46);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(48, 52, 58);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(60, 65, 73);
    visuals.widgets.active.bg_fill = Color32::from_rgb(68, 75, 84);
    visuals.selection.bg_fill = tokens.colors.accent.linear_multiply(0.55);
    visuals.selection.stroke.color = tokens.colors.accent;
    ctx.set_visuals(visuals);
}

pub fn application_problem_message(event: Option<&ApplicationEvent>) -> Option<String> {
    match event? {
        ApplicationEvent::OperationCompleted {
            token,
            outcome: OperationOutcome::Failed(code),
        } => Some(format!(
            "{:?} failed ({code:?}); correct the input, permissions, or resource limit and retry",
            token.kind()
        )),
        ApplicationEvent::ResourcePolicyRejected { reason, .. } => Some(format!(
            "settings save failed ({reason:?}); correct the settings file or permissions and retry"
        )),
        _ => None,
    }
}

/// Presents the current import workflow and returns only framework-neutral commands.
pub fn show_import_workflow_window(
    ctx: &egui::Context,
    state: &mut EguiUiState,
    snapshot: &ImportWorkflowSnapshot,
) -> Vec<ImportCommand> {
    state.synchronize_import_snapshot(snapshot);
    if matches!(snapshot, ImportWorkflowSnapshot::Idle) {
        return Vec::new();
    }

    let mut commands = Vec::new();
    egui::Window::new("TIFF Import")
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 80.0).max(240.0))
                .show(ui, |ui| match snapshot {
                    ImportWorkflowSnapshot::Idle => {}
                    ImportWorkflowSnapshot::Inspecting(inspection) => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new());
                            status_badge(
                                ui,
                                StatusTone::Warning,
                                if inspection.cancellation_requested {
                                    "stopping inspection"
                                } else {
                                    "inspecting input"
                                },
                            );
                        });
                        property_row(ui, "source", &inspection.source);
                        property_row(ui, "destination", &inspection.destination);
                        ui.add_space(8.0);
                        if toolbar_button(
                            ui,
                            "Cancel Inspection",
                            !inspection.cancellation_requested,
                        )
                        .clicked()
                        {
                            commands.push(ImportCommand::CancelInspection);
                        }
                    }
                    ImportWorkflowSnapshot::Review(review) => {
                        if let Some(review_state) = state.import_review.as_mut() {
                            show_import_review(ui, review, review_state, &mut commands);
                        }
                    }
                    ImportWorkflowSnapshot::Importing(import) => {
                        ui.horizontal(|ui| {
                            if !matches!(import.progress, ImportProgressSnapshot::Finished) {
                                ui.add(egui::Spinner::new());
                            }
                            status_badge(
                                ui,
                                StatusTone::Warning,
                                if import.cancellation_requested {
                                    "stopping import"
                                } else {
                                    "importing"
                                },
                            );
                        });
                        property_row(ui, "destination", &import.destination);
                        property_row(ui, "progress", import_progress_message(import.progress));
                        if let Some(progress) = import_progress_fraction(import.progress) {
                            ui.add(egui::ProgressBar::new(progress).show_percentage());
                        }
                        ui.add_space(8.0);
                        if toolbar_button(ui, "Cancel Import", !import.cancellation_requested)
                            .clicked()
                        {
                            commands.push(ImportCommand::CancelImport);
                        }
                    }
                    ImportWorkflowSnapshot::Failed(failure) => {
                        status_badge(ui, StatusTone::Error, "import could not continue");
                        ui.add_space(6.0);
                        ui.label(&failure.message);
                        if let Some(checkpoint) = failure.checkpoint.as_deref() {
                            property_row(ui, "checkpoint", checkpoint);
                        }
                        if let Some(retry_id) = failure.retry_id {
                            ui.checkbox(
                                &mut state.import_checkpoint_reset_confirmed,
                                "I confirm this saved import checkpoint may be deleted",
                            );
                            ui.add_space(8.0);
                            if toolbar_button(
                                ui,
                                "Reset and Restart",
                                state.import_checkpoint_reset_confirmed,
                            )
                            .clicked()
                            {
                                commands.push(ImportCommand::ResetCheckpointAndRestart {
                                    retry_id,
                                });
                                state.import_checkpoint_reset_confirmed = false;
                            }
                        } else {
                            muted_label(
                                ui,
                                "Select a supported grayscale TIFF file or an unambiguous TIFF directory.",
                            );
                        }
                        ui.add_space(8.0);
                        if toolbar_button(ui, "Dismiss", true).clicked() {
                            commands.push(ImportCommand::DismissProblem);
                            state.import_checkpoint_reset_confirmed = false;
                            state.import_checkpoint_retry_id = None;
                        }
                    }
                });
        });
    commands
}

impl EguiUiState {
    fn synchronize_import_snapshot(&mut self, snapshot: &ImportWorkflowSnapshot) {
        match snapshot {
            ImportWorkflowSnapshot::Review(review) => {
                if self
                    .import_review
                    .is_none_or(|current| current.review_id != review.review_id)
                {
                    self.import_review = Some(ImportReviewUiState {
                        review_id: review.review_id,
                        draft: review.initial_draft,
                    });
                }
            }
            ImportWorkflowSnapshot::Idle
            | ImportWorkflowSnapshot::Inspecting(_)
            | ImportWorkflowSnapshot::Importing(_) => self.import_review = None,
            ImportWorkflowSnapshot::Failed(_) => {}
        }

        let retry_id = match snapshot {
            ImportWorkflowSnapshot::Failed(failure) => failure.retry_id,
            _ => None,
        };
        if self.import_checkpoint_retry_id != retry_id {
            self.import_checkpoint_reset_confirmed = false;
            self.import_checkpoint_retry_id = retry_id;
        }
    }
}

fn show_import_review(
    ui: &mut egui::Ui,
    review: &ImportReviewSnapshot,
    state: &mut ImportReviewUiState,
    commands: &mut Vec<ImportCommand>,
) {
    status_badge(ui, StatusTone::Warning, "review import");
    ui.add_space(6.0);
    property_row(ui, "source", &review.source);
    property_row(ui, "destination", &review.destination);
    property_row(
        ui,
        "layout",
        import_source_layout_label(review.source_layout),
    );
    property_row(
        ui,
        "shape",
        format!(
            "t{} c{} z{} y{} x{}",
            review.shape.timepoints,
            review.shape.channels,
            review.shape.depth,
            review.shape.height,
            review.shape.width
        ),
    );
    property_row(
        ui,
        "source dtype",
        import_source_dtype_label(review.source_dtype),
    );
    property_row(ui, "source size", format_byte_quantity(review.source_bytes));
    property_row(
        ui,
        "calibration metadata",
        review.ome_spacing_zyx_um.map_or_else(
            || "not present; enter calibrated values".to_owned(),
            |spacing| {
                format!(
                    "OME z {:.4}, y {:.4}, x {:.4} micrometers",
                    spacing[0], spacing[1], spacing[2]
                )
            },
        ),
    );

    ui.add_space(6.0);
    ui.label("spatial calibration (micrometers)");
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::DragValue::new(&mut state.draft.spacing_zyx_um[0])
                .speed(0.01)
                .prefix("z "),
        );
        ui.add(
            egui::DragValue::new(&mut state.draft.spacing_zyx_um[1])
                .speed(0.01)
                .prefix("y "),
        );
        ui.add(
            egui::DragValue::new(&mut state.draft.spacing_zyx_um[2])
                .speed(0.01)
                .prefix("x "),
        );
    });
    ui.checkbox(
        &mut state.draft.calibration_confirmed,
        "spatial calibration reviewed",
    );

    ui.add_space(6.0);
    let mut time_step_enabled = state.draft.time_step_seconds.is_some();
    if ui
        .checkbox(&mut time_step_enabled, "regular time step")
        .changed()
    {
        state.draft.time_step_seconds = time_step_enabled.then_some(1.0);
    }
    if let Some(time_step) = state.draft.time_step_seconds.as_mut() {
        ui.horizontal(|ui| {
            ui.label("seconds per timepoint");
            ui.add(egui::DragValue::new(time_step).speed(0.01));
        });
    }

    ui.add_space(6.0);
    let sentinel_supported = review.source_dtype == ImportSourceDtype::Uint8;
    if !sentinel_supported {
        state.draft.no_data_sentinel = None;
    }
    let mut sentinel_enabled = state.draft.no_data_sentinel.is_some();
    if ui
        .add_enabled(
            sentinel_supported,
            egui::Checkbox::new(&mut sentinel_enabled, "uint8 no-data sentinel"),
        )
        .changed()
    {
        state.draft.no_data_sentinel = sentinel_enabled.then_some(255);
    }
    if let Some(sentinel) = state.draft.no_data_sentinel.as_mut() {
        let mut value = u16::from(*sentinel);
        ui.horizontal(|ui| {
            ui.label("sentinel value");
            if ui
                .add(egui::DragValue::new(&mut value).range(0..=255))
                .changed()
            {
                *sentinel = value as u8;
            }
        });
    } else if !sentinel_supported {
        property_row(ui, "no-data sentinel", "available only for uint8 sources");
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("working memory");
        egui::ComboBox::from_id_salt("tiff-import-working-memory")
            .selected_text(format_byte_quantity(state.draft.working_memory_bytes))
            .show_ui(ui, |ui| {
                for bytes in review.working_memory_choices {
                    ui.selectable_value(
                        &mut state.draft.working_memory_bytes,
                        bytes,
                        format_byte_quantity(bytes),
                    );
                }
            });
    });
    property_row(
        ui,
        "publication",
        "create new package; never replace source or output",
    );

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if toolbar_button(ui, "Start Import", import_review_ready(review, state.draft)).clicked() {
            commands.push(ImportCommand::Start {
                review_id: review.review_id,
                draft: state.draft,
            });
        }
        if toolbar_button(ui, "Cancel", true).clicked() {
            commands.push(ImportCommand::CancelReview {
                review_id: review.review_id,
            });
        }
    });
}

fn import_review_ready(review: &ImportReviewSnapshot, draft: ImportReviewDraft) -> bool {
    draft.calibration_confirmed
        && draft
            .spacing_zyx_um
            .iter()
            .all(|value| value.is_finite() && *value > 0.0)
        && draft
            .time_step_seconds
            .is_none_or(|value| value.is_finite() && value > 0.0)
        && (draft.no_data_sentinel.is_none() || review.source_dtype == ImportSourceDtype::Uint8)
        && review
            .working_memory_choices
            .contains(&draft.working_memory_bytes)
}

fn import_source_layout_label(layout: ImportSourceLayout) -> &'static str {
    match layout {
        ImportSourceLayout::Automatic => "automatic",
        ImportSourceLayout::MultipageStacks => "multipage stacks",
        ImportSourceLayout::ChannelFoldersOfPlanes => "channel folders of planes",
    }
}

fn import_source_dtype_label(dtype: ImportSourceDtype) -> &'static str {
    match dtype {
        ImportSourceDtype::Uint8 => "uint8",
        ImportSourceDtype::Uint16 => "uint16",
        ImportSourceDtype::Float32 => "float32",
    }
}

fn import_progress_message(progress: ImportProgressSnapshot) -> String {
    match progress {
        ImportProgressSnapshot::Preparing => "Preparing import".to_owned(),
        ImportProgressSnapshot::Producing {
            completed_work_units,
            total_work_units,
        } => format!("Building package {completed_work_units}/{total_work_units}"),
        ImportProgressSnapshot::HashingScience => "Checking scientific content".to_owned(),
        ImportProgressSnapshot::Publishing => "Validating and publishing package".to_owned(),
        ImportProgressSnapshot::Finished => "Import finished".to_owned(),
    }
}

fn import_progress_fraction(progress: ImportProgressSnapshot) -> Option<f32> {
    match progress {
        ImportProgressSnapshot::Preparing => None,
        ImportProgressSnapshot::Producing {
            completed_work_units,
            total_work_units,
        } if total_work_units > 0 => Some(
            (0.05 + 0.70 * (completed_work_units as f32 / total_work_units as f32))
                .clamp(0.05, 0.75),
        ),
        ImportProgressSnapshot::Producing { .. } => None,
        ImportProgressSnapshot::HashingScience => Some(0.80),
        ImportProgressSnapshot::Publishing => Some(0.90),
        ImportProgressSnapshot::Finished => Some(1.0),
    }
}

fn format_byte_quantity(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * KIB;
    const GIB: f64 = 1024.0 * MIB;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

pub fn reserve_presentation(
    ui: &mut egui::Ui,
    slot: PresentationSlot,
    surface: &PresentationSurface,
    size: egui::Vec2,
    sense: egui::Sense,
) -> (egui::Response, Option<EguiPresentationPaint>) {
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let paint = surface
        .paint_request()
        .map(|request| EguiPresentationPaint::new(slot, request, rect));
    (response, paint)
}

pub fn section<R>(
    ui: &mut egui::Ui,
    title: impl Into<String>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let tokens = UiTokens::default();
    ui.add_space(tokens.spacing.row_gap);
    ui.label(
        RichText::new(title.into())
            .strong()
            .color(tokens.colors.text_primary),
    );
    ui.separator();
    let output = add_contents(ui);
    ui.add_space(tokens.spacing.section_gap);
    output
}

pub fn panel_scroll<R>(
    ui: &mut egui::Ui,
    id_salt: impl Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .auto_shrink([false, false])
        .show(ui, add_contents)
        .inner
}

pub fn property_row(ui: &mut egui::Ui, label: impl Display, value: impl Display) {
    let tokens = UiTokens::default();
    ui.horizontal_top(|ui| {
        ui.set_min_height(tokens.spacing.control_height);
        ui.add_sized(
            [
                tokens.spacing.property_label_width,
                tokens.spacing.control_height,
            ],
            egui::Label::new(RichText::new(label.to_string()).color(tokens.colors.text_muted)),
        );
        let value_text = value.to_string();
        let value_width = ui.available_width().max(48.0);
        ui.add_sized(
            [value_width, tokens.spacing.control_height],
            egui::Label::new(RichText::new(value_text).color(tokens.colors.text_primary)).wrap(),
        );
    });
}

pub fn muted_label(ui: &mut egui::Ui, text: impl Display) {
    let tokens = UiTokens::default();
    ui.label(RichText::new(text.to_string()).color(tokens.colors.text_muted));
}

pub fn elided_label(ui: &mut egui::Ui, text: impl Display, max_chars: usize) {
    let full = text.to_string();
    let elided = elide_middle(&full, max_chars);
    ui.label(elided).on_hover_text(full);
}

pub fn elide_middle(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars || max_chars < 8 {
        return text.to_owned();
    }
    let keep = max_chars - 3;
    let head = keep / 2;
    let tail = keep - head;
    let prefix = text.chars().take(head).collect::<String>();
    let suffix = text
        .chars()
        .rev()
        .take(tail)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}...{suffix}")
}

pub fn status_badge(ui: &mut egui::Ui, tone: StatusTone, label: impl Display) {
    let tokens = UiTokens::default();
    let color = match tone {
        StatusTone::Ready => tokens.colors.status_ready,
        StatusTone::Warning => tokens.colors.status_warning,
        StatusTone::Error => tokens.colors.status_error,
    };
    let text = RichText::new(label.to_string())
        .strong()
        .color(Color32::BLACK);
    egui::Frame::new()
        .fill(color)
        .corner_radius(egui::CornerRadius::same(3))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(text);
        });
}

pub fn toolbar_button(
    ui: &mut egui::Ui,
    label: impl Into<String>,
    enabled: bool,
) -> egui::Response {
    let tokens = UiTokens::default();
    ui.add_enabled(
        enabled,
        egui::Button::new(label.into()).min_size(egui::vec2(0.0, tokens.spacing.control_height)),
    )
}

pub fn layer_row(
    ui: &mut egui::Ui,
    selected: bool,
    visible: bool,
    name: &str,
    detail: &str,
) -> egui::Response {
    let tokens = UiTokens::default();
    ui.horizontal(|ui| {
        let visibility_color = if visible {
            tokens.colors.status_ready
        } else {
            tokens.colors.text_muted
        };
        egui::Frame::new()
            .fill(visibility_color)
            .corner_radius(egui::CornerRadius::same(2))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |_| {});
        ui.selectable_label(selected, name)
            .on_hover_text(detail.to_owned())
    })
    .inner
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use egui_kittest::{Harness, kittest::Queryable};
    use mirante4d_application::import_workflow::{ImportFailureSnapshot, ImportShapeSnapshot};

    use super::*;

    #[test]
    fn workbench_runtime_requests_keep_validated_boundary_types() {
        let presentation = PresentationViewport::new(640.0, 360.0).unwrap();
        let render = RenderExtent::new(1280, 720).unwrap();
        let observation = ViewportObservation::new(PresentationSlot::Xz, presentation, render);

        assert_eq!(observation.slot(), PresentationSlot::Xz);
        assert_eq!(observation.presentation(), presentation);
        assert_eq!(observation.render(), render);
        let request = RenderUiRequest::EnsureCrossSectionCurrent {
            panel: CrossSectionPanelId::Yz,
        };
        assert_eq!(request.cross_section_panel(), CrossSectionPanelId::Yz);
        let readout =
            CrossSectionReadoutRequest::new(CrossSectionPanelId::Xy, presentation, [0.25, 0.75])
                .unwrap();
        assert_eq!(readout.panel(), CrossSectionPanelId::Xy);
        assert_eq!(readout.presentation(), presentation);
        assert_eq!(readout.normalized_point(), [0.25, 0.75]);
        assert!(CrossSectionReadoutRequest::new(
            CrossSectionPanelId::Xy,
            presentation,
            [f64::NAN, 0.5],
        )
        .is_none());
        let mut output = WorkbenchUiOutput::default();
        output.request_repaint_after(Duration::from_millis(120));
        output.request_repaint_after(Duration::from_millis(50));
        assert_eq!(output.repaint_after, Some(Duration::from_millis(50)));
        assert!(
            CrossSectionReadoutRequest::new(CrossSectionPanelId::Xy, presentation, [1.01, 0.5],)
                .is_none()
        );
    }

    fn import_draft(spacing: f64) -> ImportReviewDraft {
        ImportReviewDraft {
            spacing_zyx_um: [spacing; 3],
            calibration_confirmed: true,
            time_step_seconds: Some(1.5),
            no_data_sentinel: None,
            working_memory_bytes: 256 * 1024 * 1024,
        }
    }

    fn import_review(review_id: u64, initial_draft: ImportReviewDraft) -> ImportWorkflowSnapshot {
        ImportWorkflowSnapshot::Review(ImportReviewSnapshot {
            review_id: ImportReviewId::new(review_id),
            source: "/source/cells.ome.tiff".to_owned(),
            destination: "/output/cells.m4d".to_owned(),
            source_layout: ImportSourceLayout::MultipageStacks,
            shape: ImportShapeSnapshot {
                timepoints: 3,
                channels: 2,
                depth: 5,
                height: 32,
                width: 48,
            },
            source_dtype: ImportSourceDtype::Uint8,
            source_bytes: 4096,
            ome_spacing_zyx_um: Some([0.5, 0.2, 0.2]),
            initial_draft,
            working_memory_choices: [
                128 * 1024 * 1024,
                256 * 1024 * 1024,
                512 * 1024 * 1024,
                1024 * 1024 * 1024,
            ],
        })
    }

    struct ImportWindowHarnessState {
        ui: EguiUiState,
        snapshot: ImportWorkflowSnapshot,
        commands: Vec<ImportCommand>,
    }

    #[test]
    fn egui_state_starts_with_only_the_supplied_resource_draft() {
        let state = EguiUiState::new(256, 128);

        assert_eq!(
            state.settings_runtime_draft,
            ResourcePolicyDraft {
                cpu_dataset_budget_bytes: 256,
                gpu_budget_bytes: 128,
            }
        );
        assert!(state.viewport_orbit_drag.is_none());
        assert!(state.analysis_filter.is_empty());
        assert!(!state.close_prompt_open);
        assert!(!state.analysis_workspace_open);
        assert!(state.import_review.is_none());
        assert!(!state.import_checkpoint_reset_confirmed);
    }

    #[test]
    fn workbench_output_contains_only_typed_ui_results() {
        let output = WorkbenchUiOutput {
            application_commands: vec![ApplicationCommand::SetPlaybackActive(true)],
            import_commands: vec![ImportCommand::CancelImport],
            actions: vec![
                WorkbenchUiAction::OpenDatasetDialog,
                WorkbenchUiAction::CopySelectedAnalysisCsv,
                WorkbenchUiAction::CancelAnalysis,
                WorkbenchUiAction::SetAnalysisRoi {
                    origin: [1, 2, 3],
                    shape: [4, 5, 6],
                },
                WorkbenchUiAction::StartAnalysis(WorkbenchAnalysisKind::CurrentTimepointBox),
                WorkbenchUiAction::SaveSettings(ResourcePolicyDraft {
                    cpu_dataset_budget_bytes: 256,
                    gpu_budget_bytes: 128,
                }),
                WorkbenchUiAction::ReplaceRejectedSettings(ResourcePolicyDraft {
                    cpu_dataset_budget_bytes: 512,
                    gpu_budget_bytes: 256,
                }),
                WorkbenchUiAction::UseRecommendedSettings,
            ],
            viewport_observations: Vec::new(),
            cross_section_readout_requests: Vec::new(),
            render_requests: Vec::new(),
            presentation_paints: Vec::new(),
            rerender_requested: true,
            texture_refresh_requested: false,
            repaint_after: None,
        };

        assert_eq!(
            output.application_commands,
            vec![ApplicationCommand::SetPlaybackActive(true)]
        );
        assert_eq!(output.import_commands, vec![ImportCommand::CancelImport]);
        assert_eq!(
            output.actions,
            vec![
                WorkbenchUiAction::OpenDatasetDialog,
                WorkbenchUiAction::CopySelectedAnalysisCsv,
                WorkbenchUiAction::CancelAnalysis,
                WorkbenchUiAction::SetAnalysisRoi {
                    origin: [1, 2, 3],
                    shape: [4, 5, 6],
                },
                WorkbenchUiAction::StartAnalysis(WorkbenchAnalysisKind::CurrentTimepointBox),
                WorkbenchUiAction::SaveSettings(ResourcePolicyDraft {
                    cpu_dataset_budget_bytes: 256,
                    gpu_budget_bytes: 128,
                }),
                WorkbenchUiAction::ReplaceRejectedSettings(ResourcePolicyDraft {
                    cpu_dataset_budget_bytes: 512,
                    gpu_budget_bytes: 256,
                }),
                WorkbenchUiAction::UseRecommendedSettings,
            ]
        );
        assert!(output.presentation_paints.is_empty());
        assert!(output.rerender_requested);
        assert!(!output.texture_refresh_requested);
    }
    use mirante4d_application::{PresentationSurface, PresentationViewport};

    #[test]
    fn import_review_preserves_edits_until_a_new_review_arrives() {
        let mut state = EguiUiState::new(256, 128);
        let first = import_review(7, import_draft(1.0));
        state.synchronize_import_snapshot(&first);
        state.import_review.as_mut().unwrap().draft.spacing_zyx_um = [2.0; 3];

        state.synchronize_import_snapshot(&import_review(7, import_draft(3.0)));
        assert_eq!(state.import_review.unwrap().draft.spacing_zyx_um, [2.0; 3]);

        state.synchronize_import_snapshot(&import_review(8, import_draft(4.0)));
        assert_eq!(state.import_review.unwrap().draft.spacing_zyx_um, [4.0; 3]);
    }

    #[test]
    fn ready_import_review_emits_draft_and_review_identity() {
        let draft = import_draft(0.5);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 700.0))
            .build_ui_state(
                |ui, state: &mut ImportWindowHarnessState| {
                    state.commands =
                        show_import_workflow_window(ui.ctx(), &mut state.ui, &state.snapshot);
                },
                ImportWindowHarnessState {
                    ui: EguiUiState::new(256, 128),
                    snapshot: import_review(11, draft),
                    commands: Vec::new(),
                },
            );

        harness.get_by_label("Start Import").click_accesskit();
        harness.step();

        assert_eq!(
            harness.state().commands,
            vec![ImportCommand::Start {
                review_id: ImportReviewId::new(11),
                draft,
            }]
        );
    }

    #[test]
    fn checkpoint_restart_requires_confirmation_and_emits_retry_identity() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 500.0))
            .build_ui_state(
                |ui, state: &mut ImportWindowHarnessState| {
                    state.commands =
                        show_import_workflow_window(ui.ctx(), &mut state.ui, &state.snapshot);
                },
                ImportWindowHarnessState {
                    ui: EguiUiState::new(256, 128),
                    snapshot: ImportWorkflowSnapshot::Failed(ImportFailureSnapshot {
                        message: "the saved checkpoint is invalid".to_owned(),
                        checkpoint: Some("/output/.cells.m4d.import-checkpoint".to_owned()),
                        retry_id: Some(ImportReviewId::new(19)),
                    }),
                    commands: Vec::new(),
                },
            );

        harness
            .get_by_label("I confirm this saved import checkpoint may be deleted")
            .click();
        harness.step();
        assert!(harness.state().commands.is_empty());

        harness.get_by_label("Reset and Restart").click();
        harness.step();
        assert_eq!(
            harness.state().commands,
            vec![ImportCommand::ResetCheckpointAndRestart {
                retry_id: ImportReviewId::new(19),
            }]
        );
    }

    #[test]
    fn import_progress_is_coarse_and_monotonic() {
        assert_eq!(
            import_progress_fraction(ImportProgressSnapshot::Producing {
                completed_work_units: 5,
                total_work_units: 10,
            }),
            Some(0.4)
        );
        assert!(
            import_progress_fraction(ImportProgressSnapshot::HashingScience)
                < import_progress_fraction(ImportProgressSnapshot::Publishing)
        );
        assert_eq!(
            import_progress_fraction(ImportProgressSnapshot::Finished),
            Some(1.0)
        );
    }

    #[test]
    fn presentation_without_frame_reserves_ui_space_without_requesting_paint() {
        use egui_kittest::Harness;

        let emitted = Rc::new(Cell::new(true));
        let observed = Rc::clone(&emitted);
        let surface =
            PresentationSurface::new(PresentationViewport::new(320.0, 240.0).unwrap(), None);
        let _harness = Harness::builder()
            .with_size(egui::vec2(400.0, 300.0))
            .build_ui(move |ui| {
                let (_, paint) = reserve_presentation(
                    ui,
                    PresentationSlot::ThreeD,
                    &surface,
                    egui::vec2(320.0, 240.0),
                    egui::Sense::hover(),
                );
                observed.set(paint.is_some());
            });

        assert!(!emitted.get());
    }

    #[test]
    fn workbench_layout_reserves_viewport_at_common_sizes() {
        let spec = WorkbenchLayoutSpec::default();

        let laptop = spec.report(egui::vec2(1440.0, 900.0), 1.5).unwrap();
        assert!(laptop.viewport_size_points.x >= spec.min_viewport_width);
        assert!(laptop.viewport_size_points.y >= spec.min_viewport_height);
        assert_eq!(laptop.viewport_size_pixels, egui::vec2(1260.0, 1299.0));

        let desktop = spec.report(egui::vec2(1920.0, 1080.0), 2.0).unwrap();
        assert_eq!(desktop.viewport_size_points, egui::vec2(1320.0, 1046.0));
        assert_eq!(desktop.viewport_size_pixels, egui::vec2(2640.0, 2092.0));
    }

    #[test]
    fn workbench_layout_rejects_windows_that_cannot_hold_viewport() {
        let spec = WorkbenchLayoutSpec::default();

        assert!(spec.report(egui::vec2(900.0, 640.0), 1.0).is_none());
        assert!(spec.report(egui::vec2(1440.0, 900.0), 0.0).is_none());
    }

    #[test]
    fn design_tokens_have_stable_snapshot() {
        insta::assert_snapshot!(
            "mirante4d_ui_tokens",
            format!(
                "panel={:?}\nviewport={:?}\naccent={:?}\nleft={} right={} top={} bottom={}",
                UiTokens::default().colors.panel_background,
                UiTokens::default().colors.viewport_background,
                UiTokens::default().colors.accent,
                WorkbenchLayoutSpec::default().left_panel_width,
                WorkbenchLayoutSpec::default().right_panel_width,
                WorkbenchLayoutSpec::default().top_bar_height,
                WorkbenchLayoutSpec::default().bottom_strip_height,
            )
        );
    }

    #[test]
    fn shared_components_are_accessible_in_narrow_layout() {
        use egui_kittest::{Harness, kittest::Queryable};

        let harness = Harness::builder()
            .with_size(egui::vec2(320.0, 220.0))
            .with_pixels_per_point(2.0)
            .build_ui(|ui| {
                configure_visuals(ui.ctx());
                section(ui, "Dataset", |ui| {
                    property_row(ui, "name", "very-long-scientific-dataset-name");
                    property_row(ui, "shape", "t12 z64 y512 x512");
                    status_badge(ui, StatusTone::Ready, "ready");
                });
            });

        harness.get_by_label("Dataset");
        harness.get_by_label("name");
        harness.get_by_label("shape");
        harness.get_by_label("ready");
    }
}
