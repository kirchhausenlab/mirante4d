use eframe::egui;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::RenderMode;

use crate::{
    FrameCompleteness, ViewportHover, ViewportIntensity, application_view,
    current_runtime::{
        analysis::AnalysisProductRuntime, render::CurrentRenderRuntime, ui::CurrentUiRuntime,
    },
    tools::{
        PickCompleteness, PickHit, PickHitKind, PickPolicy, PickQuery, PickValue, ScreenPosition,
        ViewerToolCommand, ViewerToolEvent, empty_pick_hit,
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ToolInteractionOutcome {
    pub(crate) texture_refresh_requested: bool,
    pub(crate) rerender_requested: bool,
}

pub(crate) fn apply_viewport_tool_response(
    snapshot: &ApplicationSnapshot,
    analysis: &mut AnalysisProductRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    render: &CurrentRenderRuntime,
    response: &egui::Response,
    hover: Option<ViewportHover>,
) -> anyhow::Result<ToolInteractionOutcome> {
    let hit = hover
        .map(|hover| pick_hit_from_viewport_hover(snapshot, render, hover))
        .transpose()?;
    let mut commands = ui_runtime
        .viewer_tools
        .handle_event(ViewerToolEvent::Hover(hit.clone()));
    if response
        .ctx
        .input(|input| input.key_pressed(egui::Key::Escape))
    {
        commands.extend(
            ui_runtime
                .viewer_tools
                .handle_event(ViewerToolEvent::Cancel),
        );
    }
    if let Some(hit) = hit {
        if response.clicked_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryClick(hit.clone())),
            );
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryDrag(hit.clone())),
            );
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            commands.extend(
                ui_runtime
                    .viewer_tools
                    .handle_event(ViewerToolEvent::PrimaryRelease(hit)),
            );
        }
    }
    apply_viewer_tool_commands(snapshot, analysis, ui_runtime, commands)
}

/// Converts a value from an explicit CPU/reference frame into a hover hit.
/// The GPU product path does not call this with its presentation placeholder.
/// World/grid/source fields remain absent instead of being guessed.
pub(crate) fn pick_hit_from_viewport_hover(
    snapshot: &ApplicationSnapshot,
    render: &CurrentRenderRuntime,
    hover: ViewportHover,
) -> anyhow::Result<PickHit> {
    let view = application_view(snapshot);
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
        completeness: pick_completeness_for_frame(render.frame_fidelity.completeness),
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

pub(crate) fn apply_viewer_tool_commands(
    _snapshot: &ApplicationSnapshot,
    _analysis: &mut AnalysisProductRuntime,
    ui_runtime: &mut CurrentUiRuntime,
    commands: Vec<ViewerToolCommand>,
) -> anyhow::Result<ToolInteractionOutcome> {
    for command in commands {
        match command {
            ViewerToolCommand::SetHover(hit) => ui_runtime.viewer_tools.hover = hit,
            ViewerToolCommand::Select(selection) => ui_runtime.viewer_tools.selection = selection,
            ViewerToolCommand::SetCrosshair(hit) => {
                ui_runtime.viewer_tools.crosshair = Some(hit);
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
                anyhow::bail!(
                    "ROI drawing, measurement, and scene editing are not part of the current foundation scope."
                );
            }
            ViewerToolCommand::CancelTransientToolState => {}
        }
    }
    Ok(ToolInteractionOutcome::default())
}
