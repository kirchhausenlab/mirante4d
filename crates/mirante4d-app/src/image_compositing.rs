use eframe::egui;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_renderer::{
    DisplayRgbaImage, IntensityChannelFrame, IntensityTransfer, composite_intensity_channels,
};

use crate::{application_view, current_runtime::render::CurrentRenderRuntime};

/// Builds the CPU-side placeholder texture used only while no GPU texture is
/// available. Interactive dataset payloads stay owned by the unified runtime
/// and renderer lease bridge; this path never inspects or reconstructs them.
pub(crate) fn color_image_for_snapshot(
    snapshot: &ApplicationSnapshot,
    render: &CurrentRenderRuntime,
) -> egui::ColorImage {
    let view = application_view(snapshot);
    let active_layer = view
        .layer(view.active_layer())
        .expect("application view has an active layer");
    let transfer = IntensityTransfer::new(active_layer.visible(), active_layer.transfer().clone());
    display_rgba_to_color_image(
        composite_intensity_channels(&[IntensityChannelFrame::new(&render.frame, transfer)])
            .expect("retained reference frame dimensions are internally consistent"),
    )
}

fn display_rgba_to_color_image(image: DisplayRgbaImage) -> egui::ColorImage {
    let width = image.width;
    let height = image.height;
    egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &image.into_pixels(),
    )
}
