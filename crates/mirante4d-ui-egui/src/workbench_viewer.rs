use std::time::Duration;

use eframe::egui;
use mirante4d_application::{
    ApplicationCommand, ApplicationSnapshot, CameraView, CrossSectionPanelId, FrameCompleteness,
    FrameFidelityStatus, PresentationSlot, PresentationViewport, RenderBackend, RenderExtent,
    RenderMode, ViewState, ViewerLayout,
    viewer_tools::{
        PickCompleteness, PickHit, PickHitKind, PickPolicy, PickQuery, PickValue, ScreenPosition,
        ViewerTool, ViewerToolCommand, ViewerToolEvent, empty_pick_hit,
    },
    viewport_interaction::{
        CrossSectionPanel, ViewportOrbitDrag, orbit_camera, pan_camera, zoom_camera,
    },
};

use crate as ui_kit;
use crate::{
    CrossSectionReadoutRequest, EguiUiState, RenderUiRequest, ViewportHover, ViewportIntensity,
    ViewportObservation, WorkbenchUiOutput,
};

const CROSS_SECTION_SCROLL_POINTS_PER_NOTCH: f32 = 120.0;
const CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE: f32 = 0.001;

#[derive(Debug, Clone)]
enum ViewportDisplayImage {
    UiBackground {
        size: egui::Vec2,
    },
    Presentation {
        slot: PresentationSlot,
        size: egui::Vec2,
    },
}

impl ViewportDisplayImage {
    fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::UiBackground { size } | Self::Presentation { size, .. } => *size,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ViewerInteractionConfig {
    pub cross_section_settle_duration: Duration,
    pub cross_section_fast_slice_multiplier: f64,
    pub cross_section_rotate_radians_per_point: f64,
}

#[derive(Debug, Clone)]
pub struct ViewerWorkbenchView<'a> {
    pub application: &'a ApplicationSnapshot,
    pub frame_fidelity: &'a FrameFidelityStatus,
    pub fallback_render_extent: RenderExtent,
    pub xy_placeholder: &'a str,
    pub xz_placeholder: &'a str,
    pub yz_placeholder: &'a str,
    pub test_render_viewport_max_side: Option<usize>,
    pub automation_render_target: Option<RenderExtent>,
    pub interaction: ViewerInteractionConfig,
}

impl ViewerWorkbenchView<'_> {
    fn display_for_panel(&self, panel_id: PanelId) -> Option<ViewportDisplayImage> {
        match panel_id {
            PanelId::Xy | PanelId::Xz | PanelId::Yz => {
                self.presentation_display(panel_id.presentation_slot())
            }
            PanelId::ThreeD => Some(self.three_d_display()),
        }
    }

    fn presentation_display(&self, slot: PresentationSlot) -> Option<ViewportDisplayImage> {
        let extent = self
            .application
            .presentations()
            .get(slot)?
            .frame()?
            .extent();
        Some(ViewportDisplayImage::Presentation {
            slot,
            size: extent_size(extent),
        })
    }

    fn three_d_display(&self) -> ViewportDisplayImage {
        self.presentation_display(PresentationSlot::ThreeD)
            .unwrap_or_else(|| ViewportDisplayImage::UiBackground {
                size: extent_size(self.fallback_render_extent),
            })
    }

    fn placeholder_for_panel(&self, panel_id: PanelId) -> &str {
        match panel_id {
            PanelId::Xy => self.xy_placeholder,
            PanelId::Xz => self.xz_placeholder,
            PanelId::Yz => self.yz_placeholder,
            PanelId::ThreeD => "3D",
        }
    }

    fn render_viewport_max_side(&self, context_max: usize) -> usize {
        self.test_render_viewport_max_side
            .map_or(context_max, |test_max| context_max.min(test_max))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelId {
    Xy,
    Xz,
    ThreeD,
    Yz,
}

impl PanelId {
    fn label(self) -> &'static str {
        match self {
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::ThreeD => "3D",
            Self::Yz => "YZ",
        }
    }

    fn cross_section_panel(self) -> Option<CrossSectionPanel> {
        match self {
            Self::Xy => Some(CrossSectionPanel::Xy),
            Self::Xz => Some(CrossSectionPanel::Xz),
            Self::Yz => Some(CrossSectionPanel::Yz),
            Self::ThreeD => None,
        }
    }

    const fn presentation_slot(self) -> PresentationSlot {
        match self {
            Self::ThreeD => PresentationSlot::ThreeD,
            Self::Xy => PresentationSlot::Xy,
            Self::Xz => PresentationSlot::Xz,
            Self::Yz => PresentationSlot::Yz,
        }
    }
}

pub(crate) fn show_workbench_viewer(
    ui: &mut egui::Ui,
    viewer: &ViewerWorkbenchView<'_>,
    egui_ui: &mut EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let snapshot = viewer.application;
    let view = snapshot.view();
    egui::CentralPanel::default().show_inside(ui, |ui| match view.layout() {
        ViewerLayout::Single3d => {
            show_single_3d_viewport(ui, snapshot, view, viewer, egui_ui, output);
        }
        ViewerLayout::FourPanel => {
            show_four_panel_viewport(ui, snapshot, view, viewer, egui_ui, output);
        }
    });
}

fn observe_3d_viewport_for_display_size(
    ctx: &egui::Context,
    display_size_points: egui::Vec2,
    viewer: &ViewerWorkbenchView,
    output: &mut WorkbenchUiOutput,
) {
    let max_texture_side =
        viewer.render_viewport_max_side(ctx.input(|input| input.max_texture_side));
    let Some(presentation_viewport) = presentation_viewport_for_display_size(display_size_points)
    else {
        return;
    };
    let Some(render_viewport) = viewer.automation_render_target.or_else(|| {
        render_viewport_for_display_size(
            display_size_points,
            ctx.pixels_per_point(),
            max_texture_side,
        )
    }) else {
        return;
    };
    output.viewport_observations.push(ViewportObservation::new(
        PresentationSlot::ThreeD,
        presentation_viewport,
        render_viewport,
    ));
}

fn observe_four_panel_viewport(
    ctx: &egui::Context,
    panel_id: PanelId,
    display_size_points: egui::Vec2,
    viewer: &ViewerWorkbenchView,
    output: &mut WorkbenchUiOutput,
) -> Option<PresentationViewport> {
    let presentation_viewport = presentation_viewport_for_display_size(display_size_points)?;
    let max_texture_side =
        viewer.render_viewport_max_side(ctx.input(|input| input.max_texture_side));
    let render_viewport = render_viewport_for_display_size(
        display_size_points,
        ctx.pixels_per_point(),
        max_texture_side,
    )?;
    output.viewport_observations.push(ViewportObservation::new(
        panel_id.presentation_slot(),
        presentation_viewport,
        render_viewport,
    ));
    Some(presentation_viewport)
}

fn show_single_3d_viewport(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
    let display_image = viewer.three_d_display();
    let image_size = fit_size(display_image.size_vec2(), available);
    ui.centered_and_justified(|ui| {
        show_3d_viewport_image(
            ui,
            display_image,
            image_size,
            snapshot,
            view,
            viewer,
            egui_ui,
            output,
        );
    });
}

fn show_four_panel_viewport(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let gap = 6.0;
    let cell_size = egui::vec2(
        ((available.x - gap) * 0.5).max(1.0),
        ((available.y - gap) * 0.5).max(1.0),
    );
    let panels = [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz];

    egui_ui.hovered_pixel = None;
    egui_ui.hovered_source_readout = None;

    ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
    ui.vertical(|ui| {
        for row in panels.chunks_exact(2) {
            ui.horizontal(|ui| {
                for panel_id in row {
                    show_four_panel_cell(
                        ui, *panel_id, cell_size, snapshot, view, viewer, egui_ui, output,
                    );
                }
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_four_panel_cell(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    cell_size: egui::Vec2,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let tokens = ui_kit::UiTokens::default();
    ui.allocate_ui_with_layout(cell_size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_size(cell_size);
        egui::Frame::new()
            .fill(tokens.colors.viewport_background)
            .stroke(egui::Stroke::new(1.0, tokens.colors.border))
            .corner_radius(egui::CornerRadius::same(3))
            .inner_margin(egui::Margin::same(6))
            .show(ui, |ui| {
                ui.set_min_size((cell_size - egui::vec2(12.0, 12.0)).max(egui::Vec2::ZERO));
                ui.label(egui::RichText::new(panel_id.label()).strong());
                ui.add_space(4.0);
                match panel_id {
                    PanelId::ThreeD => {
                        show_embedded_3d_panel(ui, snapshot, view, viewer, egui_ui, output)
                    }
                    PanelId::Xy | PanelId::Xz | PanelId::Yz => {
                        show_cross_section_panel(
                            ui, panel_id, snapshot, view, viewer, egui_ui, output,
                        );
                    }
                }
            });
    });
}

fn show_embedded_3d_panel(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    observe_3d_viewport_for_display_size(&ctx, available, viewer, output);
    let display_image = viewer.three_d_display();
    let image_size = fit_size(display_image.size_vec2(), available);
    ui.centered_and_justified(|ui| {
        show_3d_viewport_image(
            ui,
            display_image,
            image_size,
            snapshot,
            view,
            viewer,
            egui_ui,
            output,
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn show_3d_viewport_image(
    ui: &mut egui::Ui,
    display_image: ViewportDisplayImage,
    image_size: egui::Vec2,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    if image_size == egui::Vec2::ZERO {
        return;
    }
    let response = match display_image {
        ViewportDisplayImage::UiBackground { .. } => {
            let (rect, response) =
                ui.allocate_exact_size(image_size, egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let label = if viewer.frame_fidelity.backend == RenderBackend::Empty {
                "No visible data"
            } else {
                "Loading…"
            };
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(18.0),
                ui.visuals().weak_text_color(),
            );
            response
        }
        ViewportDisplayImage::Presentation { slot, .. } => show_presentation(
            ui,
            snapshot,
            slot,
            image_size,
            egui::Sense::click_and_drag(),
            output,
        ),
    };
    let hover = viewport_hover_from_response(snapshot, view, &response);
    if response.hovered() || view.layout() == ViewerLayout::Single3d {
        egui_ui.hovered_pixel = hover;
        egui_ui.hovered_source_readout = None;
    }
    match apply_viewport_tool_response(
        snapshot,
        egui_ui,
        viewer.frame_fidelity.completeness,
        &response,
        hover,
    ) {
        Ok(outcome) => {
            output.texture_refresh_requested |= outcome.texture_refresh_requested;
            output.rerender_requested |= outcome.rerender_requested;
        }
        Err(err) => {
            tracing::warn!(%err, "viewer tool interaction rejected");
        }
    }
    if matches!(
        egui_ui.viewer_tools.active_tool,
        ViewerTool::Navigate | ViewerTool::Inspect
    ) {
        output
            .application_commands
            .extend(viewport_interaction_commands(
                egui_ui, view, &response, image_size,
            ));
    }
}

fn show_cross_section_panel(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    viewer: &ViewerWorkbenchView,
    egui_ui: &mut ui_kit::EguiUiState,
    output: &mut WorkbenchUiOutput,
) {
    let available = ui.available_size();
    let ctx = ui.ctx().clone();
    let presentation_viewport =
        observe_four_panel_viewport(&ctx, panel_id, available, viewer, output);
    let application_panel = application_cross_section_panel_id(panel_id)
        .expect("a cross-section widget has a cross-section panel ID");
    output
        .render_requests
        .push(RenderUiRequest::EnsureCrossSectionCurrent {
            panel: application_panel,
        });
    let response = if let Some(display_image) = viewer.display_for_panel(panel_id) {
        let image_size = fit_size(display_image.size_vec2(), available);
        if image_size != egui::Vec2::ZERO {
            Some(
                ui.centered_and_justified(|ui| {
                    show_cross_section_panel_image(
                        ui,
                        display_image,
                        image_size,
                        panel_id,
                        snapshot,
                        output,
                    )
                })
                .inner,
            )
        } else {
            None
        }
    } else {
        None
    }
    .unwrap_or_else(|| show_cross_section_panel_placeholder(ui, panel_id, available, viewer));

    if let Some(presentation_viewport) = presentation_viewport
        && let Some(request) = CrossSectionReadoutRequest::from_response(
            application_panel,
            presentation_viewport,
            &response,
        )
    {
        output.cross_section_readout_requests.push(request);
    }

    if matches!(
        egui_ui.viewer_tools.active_tool,
        ViewerTool::Navigate | ViewerTool::Inspect
    ) && let Some(presentation_viewport) = presentation_viewport
    {
        match cross_section_interaction_commands(
            snapshot,
            view,
            panel_id,
            presentation_viewport,
            &response,
            viewer.interaction,
        ) {
            Ok(commands) if !commands.is_empty() => {
                output.application_commands.extend(commands);
                output.request_repaint_after(viewer.interaction.cross_section_settle_duration);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cross-section interaction rejected"),
        }
    }
}

fn show_cross_section_panel_image(
    ui: &mut egui::Ui,
    display_image: ViewportDisplayImage,
    image_size: egui::Vec2,
    panel_id: PanelId,
    snapshot: &ApplicationSnapshot,
    output: &mut WorkbenchUiOutput,
) -> egui::Response {
    let response = match display_image {
        ViewportDisplayImage::UiBackground { .. } => {
            let (rect, response) =
                ui.allocate_exact_size(image_size, egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            response
        }
        ViewportDisplayImage::Presentation { slot, .. } => show_presentation(
            ui,
            snapshot,
            slot,
            image_size,
            egui::Sense::click_and_drag(),
            output,
        ),
    };
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            ui.is_enabled(),
            format!("{} cross-section panel", panel_id.label()),
        )
    });
    response
}

fn show_presentation(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    slot: PresentationSlot,
    image_size: egui::Vec2,
    sense: egui::Sense,
    output: &mut WorkbenchUiOutput,
) -> egui::Response {
    let surface = snapshot
        .presentations()
        .get(slot)
        .expect("a displayed presentation belongs to a projected surface");
    let (response, paint) = ui_kit::reserve_presentation(ui, slot, surface, image_size, sense);
    ui.painter()
        .rect_filled(response.rect, 0.0, ui.visuals().extreme_bg_color);
    if let Some(paint) = paint {
        output.presentation_paints.push(paint);
    }
    response
}

fn show_cross_section_panel_placeholder(
    ui: &mut egui::Ui,
    panel_id: PanelId,
    available: egui::Vec2,
    viewer: &ViewerWorkbenchView,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Other,
            ui.is_enabled(),
            format!("{} cross-section panel", panel_id.label()),
        )
    });
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        viewer.placeholder_for_panel(panel_id),
        egui::FontId::proportional(18.0),
        ui.visuals().weak_text_color(),
    );
    response
}

fn cross_section_interaction_commands(
    snapshot: &ApplicationSnapshot,
    view: &ViewState,
    panel_id: PanelId,
    presentation_viewport: PresentationViewport,
    response: &egui::Response,
    interaction: ViewerInteractionConfig,
) -> Result<Vec<ApplicationCommand>, String> {
    let panel = panel_id
        .cross_section_panel()
        .ok_or_else(|| "3D is not a cross-section interaction target".to_owned())?;
    let application_panel = application_cross_section_panel_id(panel_id)
        .ok_or_else(|| "3D is not a cross-section interaction target".to_owned())?;
    let mut cross_section =
        mirante4d_application::viewport_interaction::CrossSectionViewState::from_canonical(
            *view.cross_section(),
        );
    let mut edited = false;
    let modifiers = response.ctx.input(|input| input.modifiers);
    if response.dragged() {
        let primary_down = response.ctx.input(|input| input.pointer.primary_down());
        let motion_points = response.drag_motion();
        if primary_down
            && motion_points.x.is_finite()
            && motion_points.y.is_finite()
            && motion_points != egui::Vec2::ZERO
        {
            if modifiers.shift {
                cross_section.rotate_oblique_by_panel_drag(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                    interaction.cross_section_rotate_radians_per_point,
                );
            } else {
                cross_section.pan_by_panel_points(
                    panel,
                    f64::from(motion_points.x),
                    f64::from(motion_points.y),
                );
            }
            edited = true;
        }
    }
    if response.hovered() {
        if modifiers.ctrl || modifiers.command {
            let zoom_delta = response.ctx.input(|input| input.zoom_delta());
            if let Some(scroll_y) = scroll_y_points_from_zoom_delta(zoom_delta)
                && let Some(pointer) = response.hover_pos()
            {
                let local = pointer - response.rect.min.to_vec2();
                let factor =
                    (-f64::from(scroll_y) * CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE as f64).exp();
                cross_section.zoom_around_panel_point(
                    panel,
                    presentation_viewport,
                    f64::from(local.x),
                    f64::from(local.y),
                    factor,
                );
                edited = true;
            }
        } else {
            let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
            if scroll_y.is_finite() && scroll_y != 0.0 {
                let layer = snapshot
                    .catalog()
                    .layer(view.active_layer())
                    .ok_or_else(|| "active layer is absent from the dataset catalog".to_owned())?;
                let voxel_size =
                    mirante4d_application::viewport_interaction::representative_voxel_world_size(
                        layer.grid_to_world(),
                    );
                let multiplier = if modifiers.shift {
                    interaction.cross_section_fast_slice_multiplier
                } else {
                    1.0
                };
                let notches = f64::from(scroll_y / CROSS_SECTION_SCROLL_POINTS_PER_NOTCH);
                cross_section.slice_by_world_distance(panel, notches * voxel_size * multiplier);
                edited = true;
            }
        }
    }
    if !edited {
        return Ok(Vec::new());
    }
    let cross_section = cross_section
        .into_canonical()
        .map_err(|error| error.to_string())?;
    Ok(vec![
        ApplicationCommand::SetActiveCrossSectionPanel(Some(application_panel)),
        ApplicationCommand::SetLayout {
            layout: view.layout(),
            cross_section,
        },
    ])
}

fn application_cross_section_panel_id(panel_id: PanelId) -> Option<CrossSectionPanelId> {
    match panel_id {
        PanelId::Xy => Some(CrossSectionPanelId::Xy),
        PanelId::Xz => Some(CrossSectionPanelId::Xz),
        PanelId::Yz => Some(CrossSectionPanelId::Yz),
        PanelId::ThreeD => None,
    }
}

fn scroll_y_points_from_zoom_delta(zoom_delta: f32) -> Option<f32> {
    if !zoom_delta.is_finite() || zoom_delta <= 0.0 || zoom_delta == 1.0 {
        return None;
    }
    let scroll_y = -zoom_delta.ln() / CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE;
    scroll_y.is_finite().then_some(scroll_y)
}

fn presentation_viewport_for_display_size(
    display_size_points: egui::Vec2,
) -> Option<PresentationViewport> {
    PresentationViewport::new(
        f64::from(display_size_points.x),
        f64::from(display_size_points.y),
    )
    .ok()
}

pub fn render_viewport_for_display_size(
    display_size_points: egui::Vec2,
    pixels_per_point: f32,
    max_texture_side: usize,
) -> Option<RenderExtent> {
    if display_size_points.x <= 0.0
        || display_size_points.y <= 0.0
        || !display_size_points.x.is_finite()
        || !display_size_points.y.is_finite()
        || pixels_per_point <= 0.0
        || !pixels_per_point.is_finite()
        || max_texture_side == 0
    {
        return None;
    }
    let desired_width = f64::from(display_size_points.x * pixels_per_point).max(1.0);
    let desired_height = f64::from(display_size_points.y * pixels_per_point).max(1.0);
    let max_side = max_texture_side.min(u32::MAX as usize) as f64;
    let scale = (max_side / desired_width.max(desired_height)).min(1.0);
    let width = (desired_width * scale).round().max(1.0) as u32;
    let height = (desired_height * scale).round().max(1.0) as u32;
    RenderExtent::new(width, height).ok()
}

fn fit_size(image_size: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (available.x / image_size.x).min(available.y / image_size.y);
    image_size * scale
}

fn extent_size(extent: RenderExtent) -> egui::Vec2 {
    egui::vec2(extent.width_pixels() as f32, extent.height_pixels() as f32)
}

fn viewport_hover_from_response(
    _snapshot: &ApplicationSnapshot,
    _view: &ViewState,
    _response: &egui::Response,
) -> Option<ViewportHover> {
    None
}

fn viewport_interaction_commands(
    egui_ui: &mut EguiUiState,
    view: &ViewState,
    response: &egui::Response,
    viewport_size: egui::Vec2,
) -> Vec<ApplicationCommand> {
    let mut commands = Vec::new();
    if response.drag_stopped() {
        egui_ui.viewport_orbit_drag = None;
    }
    if response.dragged() {
        let camera_pan_requested = response.ctx.input(|input| {
            input.pointer.middle_down() || input.pointer.secondary_down() || input.modifiers.shift
        });
        if camera_pan_requested {
            egui_ui.viewport_orbit_drag = None;
        }
        if let Some(command) = viewport_drag_command(
            egui_ui,
            *view.camera(),
            response,
            viewport_size,
            camera_pan_requested,
        ) {
            commands.push(command);
        }
    }

    if response.hovered() {
        let scroll_y = response.ctx.input(|input| input.smooth_scroll_delta().y);
        if scroll_y != 0.0
            && let Some(command) = viewport_scroll_command(*view.camera(), scroll_y)
        {
            commands.push(command);
        }
    }
    commands
}

fn viewport_drag_command(
    egui_ui: &mut EguiUiState,
    camera: CameraView,
    response: &egui::Response,
    viewport_size_points: egui::Vec2,
    camera_pan_requested: bool,
) -> Option<ApplicationCommand> {
    if viewport_size_points.x <= 0.0
        || viewport_size_points.y <= 0.0
        || !viewport_size_points.x.is_finite()
        || !viewport_size_points.y.is_finite()
    {
        return None;
    }
    if camera_pan_requested {
        let motion_points = response.drag_motion();
        if !motion_points.x.is_finite() || !motion_points.y.is_finite() {
            return None;
        }
        let camera = pan_camera(camera, [motion_points.x, motion_points.y]);
        return Some(ApplicationCommand::SetCamera(camera));
    }

    let current_pointer = response.interact_pointer_pos()?;
    let total_drag_delta = response.total_drag_delta()?;
    if !current_pointer.x.is_finite()
        || !current_pointer.y.is_finite()
        || !total_drag_delta.x.is_finite()
        || !total_drag_delta.y.is_finite()
    {
        return None;
    }
    let drag_state = egui_ui
        .viewport_orbit_drag
        .get_or_insert(ViewportOrbitDrag::new(camera));
    let current_position_points = current_pointer - response.rect.min.to_vec2();
    let start_position_points = current_position_points - total_drag_delta;
    let camera = orbit_camera(
        drag_state.start_camera(),
        [start_position_points.x, start_position_points.y],
        [current_position_points.x, current_position_points.y],
        [viewport_size_points.x, viewport_size_points.y],
    );
    Some(ApplicationCommand::SetCamera(camera))
}

fn viewport_scroll_command(camera: CameraView, scroll_y_points: f32) -> Option<ApplicationCommand> {
    if scroll_y_points == 0.0 || !scroll_y_points.is_finite() {
        return None;
    }
    let camera = zoom_camera(camera, scroll_y_points);
    Some(ApplicationCommand::SetCamera(camera))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ToolInteractionOutcome {
    texture_refresh_requested: bool,
    rerender_requested: bool,
}

fn apply_viewport_tool_response(
    snapshot: &ApplicationSnapshot,
    egui_ui: &mut EguiUiState,
    frame_completeness: FrameCompleteness,
    response: &egui::Response,
    hover: Option<ViewportHover>,
) -> Result<ToolInteractionOutcome, String> {
    let hit = hover
        .map(|hover| pick_hit_from_viewport_hover(snapshot, frame_completeness, hover))
        .transpose()?;
    let mut commands = egui_ui
        .viewer_tools
        .handle_event(ViewerToolEvent::Hover(hit.clone()));
    if response
        .ctx
        .input(|input| input.key_pressed(egui::Key::Escape))
    {
        commands.extend(egui_ui.viewer_tools.handle_event(ViewerToolEvent::Cancel));
    }
    if let Some(hit) = hit {
        if response.clicked_by(egui::PointerButton::Primary) {
            commands.extend(
                egui_ui
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryClick(hit.clone())),
            );
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            commands.extend(
                egui_ui
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryDrag(hit.clone())),
            );
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            commands.extend(
                egui_ui
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryRelease(hit)),
            );
        }
    }
    apply_viewer_tool_commands(egui_ui, commands)
}

fn pick_hit_from_viewport_hover(
    snapshot: &ApplicationSnapshot,
    frame_completeness: FrameCompleteness,
    hover: ViewportHover,
) -> Result<PickHit, String> {
    let view = snapshot.view();
    let screen_position = ScreenPosition::new(hover.x as f32, hover.y as f32);
    let active_layer = view
        .layer(view.active_layer())
        .expect("application view has an active layer");
    if !active_layer.visible() {
        return Ok(empty_pick_hit(PickQuery {
            timepoint: view.timepoint(),
            screen_position,
        }));
    }

    Ok(PickHit {
        kind: PickHitKind::Voxel,
        object_id: None,
        timepoint: view.timepoint(),
        screen_position: Some(screen_position),
        value: Some(match hover.intensity {
            ViewportIntensity::U8(value) => PickValue::IntensityU8(value),
            ViewportIntensity::U16(value) => PickValue::IntensityU16(value),
            ViewportIntensity::F32(value) => PickValue::IntensityF32(value),
        }),
        policy: pick_policy_for_render_mode(active_layer.render_state().mode()),
        completeness: pick_completeness_for_frame(frame_completeness),
    })
}

fn pick_policy_for_render_mode(mode: RenderMode) -> PickPolicy {
    match mode {
        RenderMode::Mip => PickPolicy::MipArgmax,
        RenderMode::Isosurface => PickPolicy::FirstThresholdHit,
        RenderMode::Dvr => PickPolicy::ProbeRay,
    }
}

fn pick_completeness_for_frame(completeness: FrameCompleteness) -> PickCompleteness {
    match completeness {
        FrameCompleteness::Exact => PickCompleteness::Exact,
        FrameCompleteness::Complete | FrameCompleteness::BudgetLimited => {
            PickCompleteness::Approximate
        }
        FrameCompleteness::Loading => PickCompleteness::Loading,
        FrameCompleteness::Incomplete => PickCompleteness::Incomplete,
    }
}

fn apply_viewer_tool_commands(
    egui_ui: &mut EguiUiState,
    commands: Vec<ViewerToolCommand>,
) -> Result<ToolInteractionOutcome, String> {
    for command in commands {
        match command {
            ViewerToolCommand::SetHover(hit) => egui_ui.viewer_tools.hover = hit,
            ViewerToolCommand::Select(selection) => egui_ui.viewer_tools.selection = selection,
            ViewerToolCommand::SetCrosshair(hit) => {
                egui_ui.viewer_tools.crosshair = Some(hit);
            }
            ViewerToolCommand::BeginRoi { .. }
            | ViewerToolCommand::PreviewRoi { .. }
            | ViewerToolCommand::CommitRoi { .. }
            | ViewerToolCommand::BeginMeasurement { .. }
            | ViewerToolCommand::PreviewMeasurement { .. }
            | ViewerToolCommand::CommitMeasurement { .. }
            | ViewerToolCommand::BeginSceneHandleDrag { .. }
            | ViewerToolCommand::DragSceneHandle { .. }
            | ViewerToolCommand::CommitSceneHandleDrag { .. } => {
                return Err(
                    "ROI drawing, measurement, and scene editing are not part of the current foundation scope."
                        .to_owned(),
                );
            }
            ViewerToolCommand::CancelTransientToolState => {}
        }
    }
    Ok(ToolInteractionOutcome::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_size_maps_to_physical_render_viewport_and_clamps_aspect() {
        assert_eq!(
            render_viewport_for_display_size(egui::vec2(640.2, 360.2), 2.0, 2048).unwrap(),
            RenderExtent::new(1280, 720).unwrap()
        );
        assert_eq!(
            render_viewport_for_display_size(egui::vec2(1000.0, 2000.0), 2.0, 2048).unwrap(),
            RenderExtent::new(1024, 2048).unwrap()
        );
        assert!(render_viewport_for_display_size(egui::Vec2::ZERO, 2.0, 2048).is_none());
        assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 0.0, 2048).is_none());
        assert!(render_viewport_for_display_size(egui::vec2(640.0, 360.0), 2.0, 0).is_none());
    }

    #[test]
    fn cross_section_zoom_delta_converts_to_existing_scroll_units() {
        assert_eq!(scroll_y_points_from_zoom_delta(1.0), None);
        assert_eq!(scroll_y_points_from_zoom_delta(0.0), None);
        let zoom_in_scroll = scroll_y_points_from_zoom_delta(1.25).unwrap();
        let zoom_out_scroll = scroll_y_points_from_zoom_delta(0.8).unwrap();
        assert!(zoom_in_scroll < 0.0);
        assert!(zoom_out_scroll > 0.0);
        let reconstructed = (-zoom_in_scroll * CROSS_SECTION_SCROLL_ZOOM_FACTOR_SCALE).exp();
        assert!((reconstructed - 1.25).abs() < 1e-6);
    }
}
