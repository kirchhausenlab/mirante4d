use glam::{DQuat, DVec3};
use mirante4d_core::{GridToWorld, PresentationViewport, Shape3D};
use mirante4d_renderer::{CrossSectionPanel, CrossSectionViewState, RenderViewport};

const FIT_MARGIN: f64 = 1.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewerLayout {
    Single3d,
    FourPanel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum PanelId {
    Xy,
    Xz,
    ThreeD,
    Yz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKind {
    CrossSectionXy,
    CrossSectionXz,
    ThreeD,
    CrossSectionYz,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ViewerLayoutState {
    layout: ViewerLayout,
    pub(crate) cross_section: CrossSectionViewState,
    four_panel: Option<FourPanelRuntimeState>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FourPanelRuntimeState {
    panels: [PanelRuntimeState; 4],
    active_cross_section_panel: Option<PanelId>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PanelRuntimeState {
    pub(crate) panel_id: PanelId,
    pub(crate) kind: PanelKind,
    pub(crate) presentation_viewport: Option<PresentationViewport>,
    pub(crate) render_viewport: Option<RenderViewport>,
    pub(crate) generation: u64,
    pub(crate) displayed_generation: Option<u64>,
    pub(crate) cross_section_schedule: Option<CrossSectionPanelScheduleState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionPanelScheduleStatus {
    MissingViewport,
    Loading,
    Ready,
    Current,
    Coarse,
    Incomplete,
    BudgetLimited,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrossSectionPanelScheduleReason {
    MissingViewport,
    GpuUnavailable,
    ResidentFramePending,
    TargetScaleReady,
    ResidentScaleCoarserThanTarget,
    MissingSelectedBricks,
    DecodedBudgetExceeded,
    Rendered,
    StaleGeneration,
    RenderFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CrossSectionPanelScheduleState {
    pub(crate) generation: u64,
    pub(crate) target_scale_level: Option<u32>,
    pub(crate) render_scale_level: Option<u32>,
    pub(crate) fallback_scale_level: Option<u32>,
    pub(crate) selected_bricks: usize,
    pub(crate) occupied_selected_bricks: usize,
    pub(crate) missing_occupied_bricks: usize,
    pub(crate) estimated_decoded_bytes: u64,
    pub(crate) decoded_budget_bytes: u64,
    pub(crate) status: CrossSectionPanelScheduleStatus,
    pub(crate) reason: CrossSectionPanelScheduleReason,
}

impl ViewerLayoutState {
    pub(crate) fn single_3d_for_dataset(
        shape: Shape3D,
        grid_to_world: GridToWorld,
        presentation_viewport: PresentationViewport,
    ) -> Self {
        Self {
            layout: ViewerLayout::Single3d,
            cross_section: default_cross_section_state(shape, grid_to_world, presentation_viewport),
            four_panel: None,
        }
    }

    pub(crate) fn layout(&self) -> ViewerLayout {
        self.layout
    }

    #[cfg(test)]
    pub(crate) fn is_single_3d(&self) -> bool {
        self.layout == ViewerLayout::Single3d
    }

    #[cfg(test)]
    pub(crate) fn has_four_panel_runtime(&self) -> bool {
        self.four_panel.is_some()
    }

    pub(crate) fn four_panel_runtime(&self) -> Option<&FourPanelRuntimeState> {
        self.four_panel.as_ref()
    }

    pub(crate) fn switch_to_four_panel(&mut self) {
        self.layout = ViewerLayout::FourPanel;
        if self.four_panel.is_none() {
            self.four_panel = Some(FourPanelRuntimeState::new_shell());
        }
    }

    pub(crate) fn switch_to_single_3d(&mut self) {
        self.layout = ViewerLayout::Single3d;
        self.four_panel = None;
    }

    pub(crate) fn reset_cross_section_for_dataset(
        &mut self,
        shape: Shape3D,
        grid_to_world: GridToWorld,
        presentation_viewport: PresentationViewport,
    ) -> bool {
        let reset = default_cross_section_state(shape, grid_to_world, presentation_viewport);
        if self.cross_section == reset {
            return false;
        }
        self.cross_section = reset;
        self.mark_cross_section_panels_dirty();
        true
    }

    pub(crate) fn record_panel_viewports(
        &mut self,
        panel_id: PanelId,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        let Some(runtime) = self.four_panel.as_mut() else {
            return false;
        };
        runtime.record_panel_viewports(panel_id, presentation_viewport, render_viewport)
    }

    pub(crate) fn mark_panel_displayed(&mut self, panel_id: PanelId, generation: u64) -> bool {
        let Some(runtime) = self.four_panel.as_mut() else {
            return false;
        };
        runtime.mark_panel_displayed(panel_id, generation)
    }

    pub(crate) fn set_cross_section_panel_schedule(
        &mut self,
        panel_id: PanelId,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        let Some(runtime) = self.four_panel.as_mut() else {
            return false;
        };
        runtime.set_cross_section_panel_schedule(panel_id, schedule)
    }

    pub(crate) fn mark_cross_section_panels_dirty(&mut self) -> bool {
        let Some(runtime) = self.four_panel.as_mut() else {
            return false;
        };
        runtime.mark_cross_section_panels_dirty()
    }

    pub(crate) fn active_cross_section_panel(&self) -> Option<PanelId> {
        self.four_panel
            .as_ref()
            .and_then(FourPanelRuntimeState::active_cross_section_panel)
    }

    pub(crate) fn mark_active_cross_section_panel(&mut self, panel_id: PanelId) -> bool {
        let Some(runtime) = self.four_panel.as_mut() else {
            return false;
        };
        runtime.mark_active_cross_section_panel(panel_id)
    }
}

impl FourPanelRuntimeState {
    pub(crate) fn new_shell() -> Self {
        Self {
            panels: [
                PanelRuntimeState::new(PanelId::Xy),
                PanelRuntimeState::new(PanelId::Xz),
                PanelRuntimeState::new(PanelId::ThreeD),
                PanelRuntimeState::new(PanelId::Yz),
            ],
            active_cross_section_panel: None,
        }
    }

    pub(crate) fn panels(&self) -> &[PanelRuntimeState; 4] {
        &self.panels
    }

    pub(crate) fn panel(&self, panel_id: PanelId) -> Option<&PanelRuntimeState> {
        self.panels.iter().find(|panel| panel.panel_id == panel_id)
    }

    fn panel_mut(&mut self, panel_id: PanelId) -> Option<&mut PanelRuntimeState> {
        self.panels
            .iter_mut()
            .find(|panel| panel.panel_id == panel_id)
    }

    pub(crate) fn record_panel_viewports(
        &mut self,
        panel_id: PanelId,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        self.panel_mut(panel_id)
            .map(|panel| panel.record_viewports(presentation_viewport, render_viewport))
            .unwrap_or(false)
    }

    pub(crate) fn mark_panel_displayed(&mut self, panel_id: PanelId, generation: u64) -> bool {
        self.panel_mut(panel_id)
            .map(|panel| panel.mark_displayed_generation(generation))
            .unwrap_or(false)
    }

    pub(crate) fn set_cross_section_panel_schedule(
        &mut self,
        panel_id: PanelId,
        schedule: CrossSectionPanelScheduleState,
    ) -> bool {
        self.panel_mut(panel_id)
            .map(|panel| panel.set_cross_section_schedule(schedule))
            .unwrap_or(false)
    }

    fn mark_cross_section_panels_dirty(&mut self) -> bool {
        let mut changed = false;
        for panel in &mut self.panels {
            if panel.panel_id.cross_section_panel().is_some() {
                panel.mark_dirty();
                changed = true;
            }
        }
        changed
    }

    pub(crate) fn active_cross_section_panel(&self) -> Option<PanelId> {
        self.active_cross_section_panel
    }

    fn mark_active_cross_section_panel(&mut self, panel_id: PanelId) -> bool {
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        if self.active_cross_section_panel == Some(panel_id) {
            return false;
        }
        self.active_cross_section_panel = Some(panel_id);
        true
    }
}

impl PanelRuntimeState {
    fn new(panel_id: PanelId) -> Self {
        Self {
            panel_id,
            kind: panel_id.kind(),
            presentation_viewport: None,
            render_viewport: None,
            generation: 0,
            displayed_generation: None,
            cross_section_schedule: panel_id
                .cross_section_panel()
                .map(|_| CrossSectionPanelScheduleState::missing_viewport(0)),
        }
    }

    fn record_viewports(
        &mut self,
        presentation_viewport: PresentationViewport,
        render_viewport: RenderViewport,
    ) -> bool {
        if self.presentation_viewport == Some(presentation_viewport)
            && self.render_viewport == Some(render_viewport)
        {
            return false;
        }
        self.presentation_viewport = Some(presentation_viewport);
        self.render_viewport = Some(render_viewport);
        self.generation = self.generation.saturating_add(1);
        self.displayed_generation = None;
        if let Some(schedule) = self.cross_section_schedule.as_mut() {
            *schedule = CrossSectionPanelScheduleState::missing_viewport(self.generation);
        }
        true
    }

    pub(crate) fn display_current(&self) -> bool {
        self.displayed_generation == Some(self.generation)
    }

    fn mark_displayed_generation(&mut self, generation: u64) -> bool {
        if generation != self.generation {
            return false;
        }
        self.displayed_generation = Some(generation);
        true
    }

    fn mark_dirty(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(schedule) = self.cross_section_schedule.as_mut() {
            schedule.generation = self.generation;
            schedule.status = CrossSectionPanelScheduleStatus::Loading;
            schedule.reason = CrossSectionPanelScheduleReason::ResidentFramePending;
        }
    }

    fn set_cross_section_schedule(&mut self, schedule: CrossSectionPanelScheduleState) -> bool {
        if self.panel_id.cross_section_panel().is_none() || schedule.generation != self.generation {
            return false;
        }
        self.cross_section_schedule = Some(schedule);
        true
    }
}

impl CrossSectionPanelScheduleState {
    pub(crate) fn missing_viewport(generation: u64) -> Self {
        Self {
            generation,
            target_scale_level: None,
            render_scale_level: None,
            fallback_scale_level: None,
            selected_bricks: 0,
            occupied_selected_bricks: 0,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::MissingViewport,
            reason: CrossSectionPanelScheduleReason::MissingViewport,
        }
    }

    pub(crate) fn rendered(mut self) -> Self {
        self.status = if self.missing_occupied_bricks > 0 {
            CrossSectionPanelScheduleStatus::Incomplete
        } else if self.fallback_scale_level.is_some() {
            CrossSectionPanelScheduleStatus::Coarse
        } else {
            CrossSectionPanelScheduleStatus::Current
        };
        self.reason = CrossSectionPanelScheduleReason::Rendered;
        self
    }

    pub(crate) fn is_renderable(self) -> bool {
        let has_current_partial = self.occupied_selected_bricks > self.missing_occupied_bricks;
        (matches!(
            self.status,
            CrossSectionPanelScheduleStatus::Ready | CrossSectionPanelScheduleStatus::Coarse
        ) && self.missing_occupied_bricks == 0)
            || (matches!(
                self.status,
                CrossSectionPanelScheduleStatus::Loading
                    | CrossSectionPanelScheduleStatus::Incomplete
            ) && has_current_partial)
    }

    pub(crate) fn status_label(self) -> &'static str {
        match self.status {
            CrossSectionPanelScheduleStatus::MissingViewport => "waiting for panel size",
            CrossSectionPanelScheduleStatus::Loading => "loading",
            CrossSectionPanelScheduleStatus::Ready => "ready",
            CrossSectionPanelScheduleStatus::Current => "current",
            CrossSectionPanelScheduleStatus::Coarse => "coarse",
            CrossSectionPanelScheduleStatus::Incomplete => "incomplete",
            CrossSectionPanelScheduleStatus::BudgetLimited => "budget limited",
            CrossSectionPanelScheduleStatus::Unavailable => "unavailable",
        }
    }
}

impl PanelId {
    pub(crate) fn kind(self) -> PanelKind {
        match self {
            Self::Xy => PanelKind::CrossSectionXy,
            Self::Xz => PanelKind::CrossSectionXz,
            Self::ThreeD => PanelKind::ThreeD,
            Self::Yz => PanelKind::CrossSectionYz,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Xy => "XY",
            Self::Xz => "XZ",
            Self::ThreeD => "3D",
            Self::Yz => "YZ",
        }
    }

    pub(crate) fn cross_section_panel(self) -> Option<CrossSectionPanel> {
        match self {
            Self::Xy => Some(CrossSectionPanel::Xy),
            Self::Xz => Some(CrossSectionPanel::Xz),
            Self::Yz => Some(CrossSectionPanel::Yz),
            Self::ThreeD => None,
        }
    }
}

fn default_cross_section_state(
    shape: Shape3D,
    grid_to_world: GridToWorld,
    presentation_viewport: PresentationViewport,
) -> CrossSectionViewState {
    let center_world = shape_center_world(shape, grid_to_world);
    CrossSectionViewState::new(
        center_world,
        DQuat::IDENTITY,
        fit_world_per_screen_point(shape, grid_to_world, presentation_viewport),
        effective_voxel_depth_world(grid_to_world),
    )
}

fn shape_center_world(shape: Shape3D, grid_to_world: GridToWorld) -> DVec3 {
    grid_to_world.transform_point(DVec3::new(
        (shape.x.saturating_sub(1)) as f64 * 0.5,
        (shape.y.saturating_sub(1)) as f64 * 0.5,
        (shape.z.saturating_sub(1)) as f64 * 0.5,
    ))
}

fn fit_world_per_screen_point(
    shape: Shape3D,
    grid_to_world: GridToWorld,
    presentation_viewport: PresentationViewport,
) -> f64 {
    let center = shape_center_world(shape, grid_to_world);
    let corners = shape_bounds_corners_world(shape, grid_to_world);
    let mut max_abs_x = 0.0_f64;
    let mut max_abs_y = 0.0_f64;
    for corner in corners {
        let relative = corner - center;
        max_abs_x = max_abs_x.max(relative.x.abs());
        max_abs_y = max_abs_y.max(relative.y.abs());
    }
    let fit_width_points = (presentation_viewport.width_points / FIT_MARGIN).max(1.0);
    let fit_height_points = (presentation_viewport.height_points / FIT_MARGIN).max(1.0);
    (max_abs_x / (fit_width_points * 0.5))
        .max(max_abs_y / (fit_height_points * 0.5))
        .max(f64::EPSILON)
}

fn effective_voxel_depth_world(grid_to_world: GridToWorld) -> f64 {
    let x = grid_to_world.transform_vector(DVec3::X).length();
    let y = grid_to_world.transform_vector(DVec3::Y).length();
    let z = grid_to_world.transform_vector(DVec3::Z).length();
    x.min(y).min(z).max(f64::EPSILON)
}

fn shape_bounds_corners_world(shape: Shape3D, grid_to_world: GridToWorld) -> [DVec3; 8] {
    let min_x = -0.5;
    let min_y = -0.5;
    let min_z = -0.5;
    let max_x = shape.x as f64 - 0.5;
    let max_y = shape.y as f64 - 0.5;
    let max_z = shape.z as f64 - 0.5;
    [
        grid_to_world.transform_point(DVec3::new(min_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, min_z)),
        grid_to_world.transform_point(DVec3::new(min_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, min_y, max_z)),
        grid_to_world.transform_point(DVec3::new(min_x, max_y, max_z)),
        grid_to_world.transform_point(DVec3::new(max_x, max_y, max_z)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_3d_layout_does_not_allocate_four_panel_runtime() {
        let state = ViewerLayoutState::single_3d_for_dataset(
            Shape3D::new(16, 16, 16).unwrap(),
            GridToWorld::identity(),
            PresentationViewport::new(512.0, 512.0).unwrap(),
        );

        assert_eq!(state.layout(), ViewerLayout::Single3d);
        assert!(state.is_single_3d());
        assert!(!state.has_four_panel_runtime());
        assert!(state.four_panel_runtime().is_none());
        assert_eq!(state.cross_section.center_world, DVec3::splat(7.5));
    }

    #[test]
    fn four_panel_shell_uses_neuroglancer_panel_order() {
        let runtime = FourPanelRuntimeState::new_shell();
        let panels = runtime.panels();

        let panel_ids = panels
            .iter()
            .map(|panel| panel.panel_id)
            .collect::<Vec<_>>();
        let panel_labels = panels
            .iter()
            .map(|panel| panel.panel_id.label())
            .collect::<Vec<_>>();
        let panel_kinds = panels.iter().map(|panel| panel.kind).collect::<Vec<_>>();

        assert_eq!(
            panel_ids,
            vec![PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz,]
        );
        assert_eq!(panel_labels, vec!["XY", "XZ", "3D", "YZ"]);
        assert_eq!(
            panel_kinds,
            vec![
                PanelKind::CrossSectionXy,
                PanelKind::CrossSectionXz,
                PanelKind::ThreeD,
                PanelKind::CrossSectionYz,
            ]
        );
    }

    #[test]
    fn single_3d_layout_ignores_panel_viewport_recording() {
        let mut state = ViewerLayoutState::single_3d_for_dataset(
            Shape3D::new(16, 16, 16).unwrap(),
            GridToWorld::identity(),
            PresentationViewport::new(512.0, 512.0).unwrap(),
        );

        assert!(!state.record_panel_viewports(
            PanelId::Xy,
            PresentationViewport::new(240.0, 180.0).unwrap(),
            RenderViewport::new(480, 360).unwrap(),
        ));
        assert!(!state.has_four_panel_runtime());
    }

    #[test]
    fn four_panel_runtime_tracks_panel_viewport_generations() {
        let mut runtime = FourPanelRuntimeState::new_shell();
        let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
        let render = RenderViewport::new(480, 360).unwrap();

        assert!(runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        let panel = runtime.panel(PanelId::Xy).unwrap();
        assert_eq!(panel.presentation_viewport, Some(presentation));
        assert_eq!(panel.render_viewport, Some(render));
        assert_eq!(panel.generation, 1);
        assert!(!panel.display_current());

        assert!(runtime.mark_panel_displayed(PanelId::Xy, 1));
        assert!(runtime.panel(PanelId::Xy).unwrap().display_current());
        assert!(!runtime.record_panel_viewports(PanelId::Xy, presentation, render));
        assert_eq!(runtime.panel(PanelId::Xy).unwrap().generation, 1);
        assert!(runtime.panel(PanelId::Xy).unwrap().display_current());

        let resized_presentation = PresentationViewport::new(300.0, 180.0).unwrap();
        assert!(runtime.record_panel_viewports(PanelId::Xy, resized_presentation, render));
        let panel = runtime.panel(PanelId::Xy).unwrap();
        assert_eq!(panel.presentation_viewport, Some(resized_presentation));
        assert_eq!(panel.generation, 2);
        assert!(!panel.display_current());
        assert!(!runtime.mark_panel_displayed(PanelId::Xy, 1));
        assert!(!runtime.panel(PanelId::Xy).unwrap().display_current());
        assert!(runtime.mark_panel_displayed(PanelId::Xy, 2));
        assert!(runtime.panel(PanelId::Xy).unwrap().display_current());
    }

    #[test]
    fn cross_section_schedule_state_is_panel_generation_guarded() {
        let mut runtime = FourPanelRuntimeState::new_shell();
        let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
        let render = RenderViewport::new(480, 360).unwrap();
        assert!(runtime.record_panel_viewports(PanelId::Xz, presentation, render));

        let schedule = CrossSectionPanelScheduleState {
            generation: 1,
            target_scale_level: Some(0),
            render_scale_level: Some(1),
            fallback_scale_level: Some(1),
            selected_bricks: 4,
            occupied_selected_bricks: 4,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 1024,
            decoded_budget_bytes: 2048,
            status: CrossSectionPanelScheduleStatus::Coarse,
            reason: CrossSectionPanelScheduleReason::ResidentScaleCoarserThanTarget,
        };
        assert!(runtime.set_cross_section_panel_schedule(PanelId::Xz, schedule));
        assert_eq!(
            runtime.panel(PanelId::Xz).unwrap().cross_section_schedule,
            Some(schedule)
        );
        assert!(!runtime.set_cross_section_panel_schedule(PanelId::ThreeD, schedule));

        assert!(runtime.record_panel_viewports(
            PanelId::Xz,
            PresentationViewport::new(320.0, 180.0).unwrap(),
            render
        ));
        assert!(!runtime.set_cross_section_panel_schedule(PanelId::Xz, schedule));
        assert_eq!(
            runtime
                .panel(PanelId::Xz)
                .unwrap()
                .cross_section_schedule
                .unwrap()
                .generation,
            2
        );
    }

    #[test]
    fn four_panel_dirty_mark_invalidates_only_cross_section_panels() {
        let mut runtime = FourPanelRuntimeState::new_shell();
        let presentation = PresentationViewport::new(240.0, 180.0).unwrap();
        let render = RenderViewport::new(480, 360).unwrap();
        for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::ThreeD, PanelId::Yz] {
            assert!(runtime.record_panel_viewports(panel_id, presentation, render));
            assert!(runtime.mark_panel_displayed(panel_id, 1));
            assert!(runtime.panel(panel_id).unwrap().display_current());
        }

        assert!(runtime.mark_cross_section_panels_dirty());

        for panel_id in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let panel = runtime.panel(panel_id).unwrap();
            assert_eq!(panel.generation, 2);
            assert_eq!(panel.displayed_generation, Some(1));
            assert!(!panel.display_current());
            assert!(!runtime.mark_panel_displayed(panel_id, 1));
            assert!(runtime.mark_panel_displayed(panel_id, 2));
        }
        let three_d = runtime.panel(PanelId::ThreeD).unwrap();
        assert_eq!(three_d.generation, 1);
        assert!(three_d.display_current());
    }

    #[test]
    fn four_panel_tracks_active_cross_section_panel_without_marking_3d() {
        let mut runtime = FourPanelRuntimeState::new_shell();

        assert_eq!(runtime.active_cross_section_panel(), None);
        assert!(!runtime.mark_active_cross_section_panel(PanelId::ThreeD));
        assert_eq!(runtime.active_cross_section_panel(), None);

        assert!(runtime.mark_active_cross_section_panel(PanelId::Xz));
        assert_eq!(runtime.active_cross_section_panel(), Some(PanelId::Xz));
        assert!(!runtime.mark_active_cross_section_panel(PanelId::Xz));
        assert_eq!(runtime.active_cross_section_panel(), Some(PanelId::Xz));

        assert!(runtime.mark_active_cross_section_panel(PanelId::Yz));
        assert_eq!(runtime.active_cross_section_panel(), Some(PanelId::Yz));
    }

    #[test]
    fn panel_ids_map_to_cross_section_renderer_panels() {
        assert_eq!(
            PanelId::Xy.cross_section_panel(),
            Some(CrossSectionPanel::Xy)
        );
        assert_eq!(
            PanelId::Xz.cross_section_panel(),
            Some(CrossSectionPanel::Xz)
        );
        assert_eq!(
            PanelId::Yz.cross_section_panel(),
            Some(CrossSectionPanel::Yz)
        );
        assert_eq!(PanelId::ThreeD.cross_section_panel(), None);
    }
}
