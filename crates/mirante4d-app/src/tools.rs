use mirante4d_renderer::{PickHit, PickHitKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerTool {
    Navigate,
    Inspect,
    Crosshair,
    RoiBox,
    MeasureDistance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewerToolState {
    pub active_tool: ViewerTool,
    pub hover: Option<PickHit>,
    pub selection: Option<ToolSelection>,
    pub crosshair: Option<PickHit>,
    pub pending_roi_anchor: Option<PickHit>,
    pub pending_measurement_anchor: Option<PickHit>,
    pub active_scene_handle_drag: Option<PickHit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSelection {
    SceneObject {
        kind: PickHitKind,
        object_id: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolEvent {
    Hover(Option<PickHit>),
    PrimaryClick(PickHit),
    PrimaryDrag(PickHit),
    PrimaryRelease(PickHit),
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewerToolCommand {
    SetHover(Option<PickHit>),
    Select(Option<ToolSelection>),
    SetCrosshair(PickHit),
    BeginRoi { anchor: PickHit },
    PreviewRoi { anchor: PickHit, current: PickHit },
    CommitRoi { anchor: PickHit, current: PickHit },
    BeginMeasurement { anchor: PickHit },
    PreviewMeasurement { anchor: PickHit, current: PickHit },
    CommitMeasurement { anchor: PickHit, current: PickHit },
    BeginSceneHandleDrag { handle: PickHit },
    DragSceneHandle { handle: PickHit, current: PickHit },
    CommitSceneHandleDrag { handle: PickHit, current: PickHit },
    CancelTransientToolState,
}

impl Default for ViewerToolState {
    fn default() -> Self {
        Self {
            active_tool: ViewerTool::Navigate,
            hover: None,
            selection: None,
            crosshair: None,
            pending_roi_anchor: None,
            pending_measurement_anchor: None,
            active_scene_handle_drag: None,
        }
    }
}

impl ViewerToolState {
    pub fn set_active_tool(&mut self, tool: ViewerTool) {
        if self.active_tool != tool {
            self.pending_roi_anchor = None;
            self.pending_measurement_anchor = None;
            self.active_scene_handle_drag = None;
        }
        self.active_tool = tool;
    }

    pub fn handle_event(&mut self, event: ViewerToolEvent) -> Vec<ViewerToolCommand> {
        match event {
            ViewerToolEvent::Hover(hit) => {
                self.hover = hit.clone();
                hit.into_iter()
                    .map(Some)
                    .chain((self.hover.is_none()).then_some(None))
                    .map(ViewerToolCommand::SetHover)
                    .collect()
            }
            ViewerToolEvent::PrimaryClick(hit) => self.handle_primary_click(hit),
            ViewerToolEvent::PrimaryDrag(hit) => self.handle_primary_drag(hit),
            ViewerToolEvent::PrimaryRelease(hit) => self.handle_primary_release(hit),
            ViewerToolEvent::Cancel => {
                self.pending_roi_anchor = None;
                self.pending_measurement_anchor = None;
                self.active_scene_handle_drag = None;
                vec![ViewerToolCommand::CancelTransientToolState]
            }
        }
    }

    fn handle_primary_click(&mut self, hit: PickHit) -> Vec<ViewerToolCommand> {
        match self.active_tool {
            ViewerTool::Navigate | ViewerTool::Inspect => {
                if hit.kind == PickHitKind::AnnotationHandle {
                    self.active_scene_handle_drag = Some(hit.clone());
                    return vec![ViewerToolCommand::BeginSceneHandleDrag { handle: hit }];
                }
                let selection = selection_from_hit(&hit);
                self.selection = selection.clone();
                vec![ViewerToolCommand::Select(selection)]
            }
            ViewerTool::Crosshair => {
                self.crosshair = Some(hit.clone());
                vec![ViewerToolCommand::SetCrosshair(hit)]
            }
            ViewerTool::RoiBox => {
                self.pending_roi_anchor = Some(hit.clone());
                vec![ViewerToolCommand::BeginRoi { anchor: hit }]
            }
            ViewerTool::MeasureDistance => {
                self.pending_measurement_anchor = Some(hit.clone());
                vec![ViewerToolCommand::BeginMeasurement { anchor: hit }]
            }
        }
    }

    fn handle_primary_drag(&mut self, hit: PickHit) -> Vec<ViewerToolCommand> {
        match self.active_tool {
            ViewerTool::Navigate | ViewerTool::Inspect => self
                .active_scene_handle_drag
                .clone()
                .map(|handle| ViewerToolCommand::DragSceneHandle {
                    handle,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::RoiBox => self
                .pending_roi_anchor
                .clone()
                .map(|anchor| ViewerToolCommand::PreviewRoi {
                    anchor,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::MeasureDistance => self
                .pending_measurement_anchor
                .clone()
                .map(|anchor| ViewerToolCommand::PreviewMeasurement {
                    anchor,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::Crosshair => Vec::new(),
        }
    }

    fn handle_primary_release(&mut self, hit: PickHit) -> Vec<ViewerToolCommand> {
        match self.active_tool {
            ViewerTool::Navigate | ViewerTool::Inspect => self
                .active_scene_handle_drag
                .take()
                .map(|handle| ViewerToolCommand::CommitSceneHandleDrag {
                    handle,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::RoiBox => self
                .pending_roi_anchor
                .take()
                .map(|anchor| ViewerToolCommand::CommitRoi {
                    anchor,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::MeasureDistance => self
                .pending_measurement_anchor
                .take()
                .map(|anchor| ViewerToolCommand::CommitMeasurement {
                    anchor,
                    current: hit,
                })
                .into_iter()
                .collect(),
            ViewerTool::Crosshair => Vec::new(),
        }
    }
}

pub fn selection_from_hit(hit: &PickHit) -> Option<ToolSelection> {
    match (&hit.kind, &hit.value, &hit.object_id) {
        (
            PickHitKind::Track
            | PickHitKind::Roi
            | PickHitKind::Annotation
            | PickHitKind::AnnotationHandle
            | PickHitKind::Measurement
            | PickHitKind::Plane
            | PickHitKind::Ui,
            _,
            Some(object_id),
        ) => Some(ToolSelection::SceneObject {
            kind: hit.kind,
            object_id: object_id.as_str().to_owned(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::TimeIndex;
    use mirante4d_renderer::{
        PickCompleteness, PickHitKind, PickPolicy, PickValue, ScreenPosition, empty_pick_hit,
    };

    use super::*;

    #[test]
    fn roi_tool_tracks_anchor_and_emits_commit_command() {
        let mut state = ViewerToolState {
            active_tool: ViewerTool::RoiBox,
            ..Default::default()
        };
        let anchor = empty_hit();
        let current = empty_hit();

        assert!(matches!(
            state
                .handle_event(ViewerToolEvent::PrimaryClick(anchor.clone()))
                .as_slice(),
            [ViewerToolCommand::BeginRoi { .. }]
        ));
        assert!(state.pending_roi_anchor.is_some());
        assert!(matches!(
            state
                .handle_event(ViewerToolEvent::PrimaryRelease(current))
                .as_slice(),
            [ViewerToolCommand::CommitRoi { .. }]
        ));
        assert!(state.pending_roi_anchor.is_none());
    }

    #[test]
    fn measurement_tool_tracks_anchor_and_emits_commit_command() {
        let mut state = ViewerToolState {
            active_tool: ViewerTool::MeasureDistance,
            ..Default::default()
        };

        state.handle_event(ViewerToolEvent::PrimaryClick(empty_hit()));
        let commands = state.handle_event(ViewerToolEvent::PrimaryRelease(empty_hit()));

        assert!(matches!(
            commands.as_slice(),
            [ViewerToolCommand::CommitMeasurement { .. }]
        ));
    }

    #[test]
    fn navigate_tool_tracks_scene_handle_drag_without_reselecting_object() {
        let mut state = ViewerToolState {
            selection: Some(ToolSelection::SceneObject {
                kind: PickHitKind::Roi,
                object_id: "roi-a".to_owned(),
            }),
            ..Default::default()
        };
        let handle = scene_handle_hit("roi-a", "roi_box_min");
        let current = empty_hit();

        assert!(matches!(
            state
                .handle_event(ViewerToolEvent::PrimaryClick(handle.clone()))
                .as_slice(),
            [ViewerToolCommand::BeginSceneHandleDrag { .. }]
        ));
        assert_eq!(state.active_scene_handle_drag, Some(handle.clone()));
        assert_eq!(
            state.selection,
            Some(ToolSelection::SceneObject {
                kind: PickHitKind::Roi,
                object_id: "roi-a".to_owned(),
            })
        );
        assert!(matches!(
            state
                .handle_event(ViewerToolEvent::PrimaryDrag(current.clone()))
                .as_slice(),
            [ViewerToolCommand::DragSceneHandle { .. }]
        ));
        assert!(matches!(
            state
                .handle_event(ViewerToolEvent::PrimaryRelease(current))
                .as_slice(),
            [ViewerToolCommand::CommitSceneHandleDrag { .. }]
        ));
        assert!(state.active_scene_handle_drag.is_none());
    }

    #[test]
    fn changing_tools_clears_pending_transient_anchors() {
        let mut state = ViewerToolState {
            active_tool: ViewerTool::RoiBox,
            pending_roi_anchor: Some(empty_hit()),
            pending_measurement_anchor: Some(empty_hit()),
            ..Default::default()
        };

        state.set_active_tool(ViewerTool::Inspect);

        assert!(state.pending_roi_anchor.is_none());
        assert!(state.pending_measurement_anchor.is_none());
    }

    fn scene_handle_hit(object_id: &str, handle: &str) -> PickHit {
        PickHit {
            kind: PickHitKind::AnnotationHandle,
            layer_id: None,
            object_id: Some(mirante4d_renderer::SceneObjectId::new(object_id).unwrap()),
            source_layer_id: None,
            timepoint: TimeIndex::new(0),
            world_position: None,
            grid_position: None,
            screen_position: Some(ScreenPosition::new(1.0, 2.0)),
            value: Some(PickValue::ObjectMetadata(handle.to_owned())),
            policy: PickPolicy::SceneObject,
            completeness: PickCompleteness::Exact,
        }
    }

    fn empty_hit() -> PickHit {
        empty_pick_hit(mirante4d_renderer::PickQuery {
            timepoint: TimeIndex::new(0),
            screen_position: ScreenPosition::new(1.0, 2.0),
        })
    }
}
