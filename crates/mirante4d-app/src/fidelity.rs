use crate::{RenderCoordinationState, application_view, display_graph::DisplayGraph, ui_kit};
use mirante4d_application::ApplicationSnapshot;

pub(crate) fn composite_fidelity_label(
    snapshot: &ApplicationSnapshot,
    render: &RenderCoordinationState,
) -> String {
    let mut label = ui_kit::frame_fidelity_label(&render.frame_fidelity);
    label.push_str(" | ");
    let display_graph = DisplayGraph::from_snapshot(snapshot);
    if display_graph.is_mixed_mode() {
        label.push_str("mixed render modes");
    } else {
        let view = application_view(snapshot);
        let sampling = view
            .layer(view.active_layer())
            .expect("application view contains its active layer")
            .render_state()
            .sampling_policy();
        label.push_str(ui_kit::render_sampling_policy_label(sampling));
    }
    label
}
