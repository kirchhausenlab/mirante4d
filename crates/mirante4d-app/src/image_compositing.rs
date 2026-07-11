use eframe::egui;
use mirante4d_application::ApplicationSnapshot;
use mirante4d_domain::RgbColor;
use mirante4d_format::LayerDisplay;
use mirante4d_renderer::{
    DisplayRgbaImage, IntensityChannelFrame, IntensityTransfer, MipImageU16,
    composite_intensity_channels,
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

pub fn mip_to_color_image(image: &MipImageU16, display: LayerDisplay) -> egui::ColorImage {
    mip_to_color_image_with_color(image, display, default_intensity_color())
}

pub(crate) fn mip_to_color_image_with_color(
    image: &MipImageU16,
    display: LayerDisplay,
    color: RgbColor,
) -> egui::ColorImage {
    let transfer = IntensityTransfer::new(display.visible(), display.layer_transfer(color));
    let base = composite_intensity_channels(&[IntensityChannelFrame::new(image, transfer)])
        .expect("single channel frame dimensions are internally consistent");
    display_rgba_to_color_image(base)
}

fn display_rgba_to_color_image(image: DisplayRgbaImage) -> egui::ColorImage {
    let width = image.width;
    let height = image.height;
    egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &image.into_pixels(),
    )
}

fn default_intensity_color() -> RgbColor {
    RgbColor::new([1.0, 1.0, 1.0]).expect("default channel color is valid")
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::DisplayWindow;
    use mirante4d_format::LayerDisplay;

    use super::*;

    #[test]
    fn mip_texture_conversion_uses_layer_display_window_and_opacity() {
        let frame = MipImageU16::new(4, 1, vec![500, 1_000, 1_058, 1_115]);
        let display =
            LayerDisplay::new(true, DisplayWindow::new(1_000.0, 1_115.0).unwrap(), 0.5).unwrap();

        let image = mip_to_color_image(&frame, display);

        assert_eq!(image.size, [4, 1]);
        assert_eq!(
            image.pixels,
            vec![
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128),
                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 128),
                egui::Color32::from_rgba_unmultiplied(129, 129, 129, 128),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 128),
            ]
        );
    }

    #[test]
    fn mip_texture_conversion_hides_invisible_layers() {
        let frame = MipImageU16::new(1, 1, vec![1_115]);
        let display =
            LayerDisplay::new(false, DisplayWindow::new(1_000.0, 1_115.0).unwrap(), 1.0).unwrap();

        let image = mip_to_color_image(&frame, display);

        assert_eq!(
            image.pixels,
            vec![egui::Color32::from_rgba_unmultiplied(255, 255, 255, 0)]
        );
    }
}
