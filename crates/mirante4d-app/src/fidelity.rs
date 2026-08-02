use crate::{RenderCoordinationState, display_graph::DisplayGraph, ui_kit};
use mirante4d_application::ApplicationSnapshot;

pub(crate) fn composite_fidelity_label(
    snapshot: &ApplicationSnapshot,
    render: &RenderCoordinationState,
) -> String {
    let mut label = ui_kit::frame_fidelity_label(&render.frame_fidelity);
    label.push_str(" | ");
    let display_graph = DisplayGraph::from_snapshot(snapshot);
    if display_graph.channels.is_empty() {
        label.push_str("no visible channels");
    } else if display_graph.is_mixed_mode() {
        label.push_str("mixed render modes");
    } else if let Some(sampling) = display_graph.uniform_sampling_policy() {
        label.push_str(ui_kit::render_sampling_policy_label(sampling));
    } else {
        label.push_str("mixed sampling");
    }
    label
}
