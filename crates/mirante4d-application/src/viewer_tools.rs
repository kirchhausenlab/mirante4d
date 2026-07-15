//! Framework-neutral viewer-tool interaction state and commands.

use mirante4d_domain::TimeIndex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickCompleteness {
    Exact,
    Approximate,
    Incomplete,
    Loading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickPolicy {
    FirstThresholdHit,
    MipArgmax,
    ProbeRay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PickHitKind {
    Voxel,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenPosition {
    pub x: f32,
    pub y: f32,
}

impl ScreenPosition {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PickValue {
    IntensityU8(u8),
    IntensityU16(u16),
    IntensityF32(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PickHit {
    pub kind: PickHitKind,
    pub timepoint: TimeIndex,
    pub screen_position: Option<ScreenPosition>,
    pub value: Option<PickValue>,
    pub policy: PickPolicy,
    pub completeness: PickCompleteness,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickQuery {
    pub timepoint: TimeIndex,
    pub screen_position: ScreenPosition,
}

pub fn empty_pick_hit(query: PickQuery, policy: PickPolicy) -> PickHit {
    PickHit {
        kind: PickHitKind::Empty,
        timepoint: query.timepoint,
        screen_position: Some(query.screen_position),
        value: None,
        policy,
        completeness: PickCompleteness::Exact,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerTool {
    Navigate,
    Inspect,
    Crosshair,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewerToolState {
    pub active_tool: ViewerTool,
    pub hover: Option<PickHit>,
    pub crosshair: Option<PickHit>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolEvent {
    Hover(Option<PickHit>),
    PrimaryClick(PickHit),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolCommand {
    SetHover(Option<PickHit>),
    SetCrosshair(PickHit),
}

impl Default for ViewerToolState {
    fn default() -> Self {
        Self {
            active_tool: ViewerTool::Navigate,
            hover: None,
            crosshair: None,
        }
    }
}

impl ViewerToolState {
    pub fn set_active_tool(&mut self, tool: ViewerTool) {
        self.active_tool = tool;
    }

    pub fn handle_event(&mut self, event: ViewerToolEvent) -> Vec<ViewerToolCommand> {
        match event {
            ViewerToolEvent::Hover(hit) => {
                self.hover = hit.clone();
                vec![ViewerToolCommand::SetHover(hit)]
            }
            ViewerToolEvent::PrimaryClick(hit) => self.handle_primary_click(hit),
        }
    }

    fn handle_primary_click(&mut self, hit: PickHit) -> Vec<ViewerToolCommand> {
        match self.active_tool {
            ViewerTool::Navigate | ViewerTool::Inspect => Vec::new(),
            ViewerTool::Crosshair => {
                self.crosshair = Some(hit.clone());
                vec![ViewerToolCommand::SetCrosshair(hit)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::TimeIndex;

    use super::*;

    #[test]
    fn hover_updates_are_forwarded_and_can_be_cleared() {
        let mut state = ViewerToolState::default();
        let hit = voxel_hit();

        assert_eq!(
            state.handle_event(ViewerToolEvent::Hover(Some(hit.clone()))),
            vec![ViewerToolCommand::SetHover(Some(hit.clone()))]
        );
        assert_eq!(state.hover, Some(hit));
        assert_eq!(
            state.handle_event(ViewerToolEvent::Hover(None)),
            vec![ViewerToolCommand::SetHover(None)]
        );
        assert_eq!(state.hover, None);
    }

    #[test]
    fn crosshair_click_updates_the_crosshair() {
        let mut state = ViewerToolState {
            active_tool: ViewerTool::Crosshair,
            ..Default::default()
        };
        let hit = voxel_hit();

        assert_eq!(
            state.handle_event(ViewerToolEvent::PrimaryClick(hit.clone())),
            vec![ViewerToolCommand::SetCrosshair(hit.clone())]
        );
        assert_eq!(state.crosshair, Some(hit));
    }

    #[test]
    fn navigation_and_inspection_do_not_create_editing_commands() {
        let mut state = ViewerToolState::default();
        let hit = voxel_hit();

        for tool in [ViewerTool::Navigate, ViewerTool::Inspect] {
            state.set_active_tool(tool);
            assert!(
                state
                    .handle_event(ViewerToolEvent::PrimaryClick(hit.clone()))
                    .is_empty()
            );
        }
        assert_eq!(state.crosshair, None);
    }

    fn voxel_hit() -> PickHit {
        PickHit {
            kind: PickHitKind::Voxel,
            timepoint: TimeIndex::new(0),
            screen_position: Some(ScreenPosition::new(1.0, 2.0)),
            value: Some(PickValue::IntensityU16(42)),
            policy: PickPolicy::MipArgmax,
            completeness: PickCompleteness::Exact,
        }
    }
}
