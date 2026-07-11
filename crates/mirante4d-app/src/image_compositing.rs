use eframe::egui;
use mirante4d_core::{ChannelColor, LayerDisplay};
use mirante4d_renderer::scene_render::{SceneRgbaImage, render_scene_layers_rgba_cpu};
use mirante4d_renderer::{
    DisplayRgbaImage, DvrRgbaChannelFrame, IntensityChannelFrame, IntensityChannelFrameF32,
    IntensityTransfer, IsoSurfaceChannelFrame, IsoSurfaceChannelFrameF32, MipImageF32, MipImageU16,
    RenderViewport, SceneColorRgba, composite_dvr_rgba_channels, composite_f32_intensity_channels,
    composite_intensity_channels, composite_iso_surface_channels,
    composite_iso_surface_f32_channels,
};

use crate::{
    AppState, DisplayedFrameFreshness, FrameCompleteness, RenderMode, RenderedIntensityChannel,
    scene_draw_list_for_state,
};

fn empty_display_rgba_image(viewport: RenderViewport) -> DisplayRgbaImage {
    empty_display_rgba_image_for_size(viewport.width, viewport.height)
}

fn empty_display_rgba_image_for_size(width: u64, height: u64) -> DisplayRgbaImage {
    let byte_count = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .and_then(|bytes| usize::try_from(bytes).ok())
        .expect("render viewport dimensions are addressable");
    DisplayRgbaImage::new(width, height, vec![0; byte_count])
        .expect("empty RGBA frame dimensions are internally consistent")
}

fn source_over_rgba(dst: &mut [u8], src: &[u8]) {
    let src_a = f32::from(src[3]) / 255.0;
    let dst_a = f32::from(dst[3]) / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    for channel in 0..3 {
        let src_c = f32::from(src[channel]) / 255.0;
        let dst_c = f32::from(dst[channel]) / 255.0;
        let out_c = if out_a <= f32::EPSILON {
            0.0
        } else {
            (src_c * src_a + dst_c * dst_a * (1.0 - src_a)) / out_a
        };
        dst[channel] = (out_c.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    dst[3] = (out_a.clamp(0.0, 1.0) * 255.0).round() as u8;
}

fn additive_rgba(dst: &mut [u8], src: &[u8]) {
    let src_a = f32::from(src[3]) / 255.0;
    for channel in 0..3 {
        let added = f32::from(dst[channel]) + f32::from(src[channel]) * src_a;
        dst[channel] = added.min(255.0).round() as u8;
    }
    dst[3] = dst[3].max(src[3]);
}

pub(crate) fn missing_typed_payload_is_reportable_error(state: &AppState) -> bool {
    state.frame_fidelity.display_freshness == DisplayedFrameFreshness::Current
        && matches!(
            state.frame_fidelity.completeness,
            FrameCompleteness::Exact
                | FrameCompleteness::Complete
                | FrameCompleteness::BudgetLimited
        )
}

fn blend_channel_rgba(
    base: DisplayRgbaImage,
    channel: DisplayRgbaImage,
    mode: RenderMode,
) -> DisplayRgbaImage {
    let width = base.width;
    let height = base.height;
    let mut pixels = base.into_pixels();
    let channel_pixels = channel.into_pixels();
    for (dst, src) in pixels
        .chunks_exact_mut(4)
        .zip(channel_pixels.chunks_exact(4))
    {
        match mode {
            RenderMode::Dvr | RenderMode::Isosurface => source_over_rgba(dst, src),
            RenderMode::Mip => additive_rgba(dst, src),
        }
    }
    DisplayRgbaImage::new(width, height, pixels)
        .expect("blended RGBA frame dimensions must match the active viewport")
}

fn rendered_channel_to_rgba(
    state: &AppState,
    channel: &RenderedIntensityChannel,
) -> DisplayRgbaImage {
    match channel.render_state.mode() {
        RenderMode::Dvr => {
            if let Some(dvr_rgba) = channel.frame.dvr_rgba() {
                composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(dvr_rgba)])
                    .expect("DVR channel RGBA dimensions must match the active viewport")
            } else {
                if missing_typed_payload_is_reportable_error(state) {
                    tracing::error!(
                        layer_id = %channel.layer_id,
                        "DVR channel missing typed RGBA frame; showing empty DVR channel instead of scalar fallback"
                    );
                } else {
                    tracing::debug!(
                        layer_id = %channel.layer_id,
                        "DVR channel typed RGBA frame is pending; showing empty DVR channel instead of scalar fallback"
                    );
                }
                empty_display_rgba_image(state.render_viewport)
            }
        }
        RenderMode::Isosurface => {
            if let Some(surface) = channel
                .frame_f32
                .as_ref()
                .and_then(MipImageF32::iso_surface)
            {
                composite_iso_surface_f32_channels(
                    &[IsoSurfaceChannelFrameF32::new(
                        surface,
                        IntensityTransfer::from_transfer_function(channel.transfer.clone()),
                    )],
                    state.iso_light_state,
                    state.camera.axes(),
                )
                .expect("f32 ISO channel dimensions must match the active viewport")
            } else if let Some(surface) = channel.frame.iso_surface() {
                composite_iso_surface_channels(
                    &[IsoSurfaceChannelFrame::new(
                        surface,
                        IntensityTransfer::from_transfer_function(channel.transfer.clone()),
                    )],
                    state.iso_light_state,
                    state.camera.axes(),
                )
                .expect("ISO channel dimensions must match the active viewport")
            } else {
                if missing_typed_payload_is_reportable_error(state) {
                    tracing::error!(
                        layer_id = %channel.layer_id,
                        "ISO channel missing typed surface frame; showing empty ISO channel instead of scalar fallback"
                    );
                } else {
                    tracing::debug!(
                        layer_id = %channel.layer_id,
                        "ISO channel typed surface frame is pending; showing empty ISO channel instead of scalar fallback"
                    );
                }
                empty_display_rgba_image(state.render_viewport)
            }
        }
        RenderMode::Mip => {
            if let Some(frame_f32) = channel.frame_f32.as_ref() {
                composite_f32_intensity_channels(&[IntensityChannelFrameF32::new(
                    frame_f32,
                    IntensityTransfer::from_transfer_function(channel.transfer.clone()),
                )])
                .expect("f32 intensity channel dimensions must match the active viewport")
            } else {
                composite_intensity_channels(&[IntensityChannelFrame::new(
                    &channel.frame,
                    IntensityTransfer::from_transfer_function(channel.transfer.clone()),
                )])
                .expect("intensity channel dimensions must match the active viewport")
            }
        }
    }
}

fn composite_rendered_channels_mixed(state: &AppState) -> DisplayRgbaImage {
    let Some(first) = state.rendered_channels.first() else {
        return empty_display_rgba_image(state.render_viewport);
    };
    let mut base = empty_display_rgba_image_for_size(first.frame.width, first.frame.height);
    for channel in &state.rendered_channels {
        let channel_rgba = rendered_channel_to_rgba(state, channel);
        base = blend_channel_rgba(base, channel_rgba, channel.render_state.mode());
    }
    base
}

pub(crate) fn color_image_for_state(state: &AppState) -> egui::ColorImage {
    let homogeneous_dvr = state.active_render_mode == RenderMode::Dvr
        && state
            .rendered_channels
            .iter()
            .all(|channel| channel.render_state.mode() == RenderMode::Dvr);
    let base = if homogeneous_dvr {
        let base = if let Some(dvr_rgba) = state.frame.dvr_rgba() {
            composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(dvr_rgba)])
                .expect("same-ray DVR RGBA dimensions must match the active viewport")
        } else {
            if missing_typed_payload_is_reportable_error(state) {
                tracing::error!(
                    rendered_channel_count = state.rendered_channels.len(),
                    "active DVR frame missing typed same-ray RGBA frame; showing an empty DVR frame instead of scalar fallback"
                );
            } else {
                tracing::debug!(
                    rendered_channel_count = state.rendered_channels.len(),
                    "active DVR same-ray RGBA frame is pending; showing an empty DVR frame instead of scalar fallback"
                );
            }
            empty_display_rgba_image(state.render_viewport)
        };
        display_rgba_to_color_image(base)
    } else if !state.rendered_channels.is_empty() {
        display_rgba_to_color_image(composite_rendered_channels_mixed(state))
    } else {
        mip_to_color_image_with_color(
            &state.frame,
            state.active_layer_display,
            state.active_layer_color,
        )
    };
    color_image_with_scene_layers(state, base)
}

fn color_image_with_scene_layers(state: &AppState, base: egui::ColorImage) -> egui::ColorImage {
    let Ok(draw_list) = scene_draw_list_for_state(state) else {
        return base;
    };
    if draw_list.is_empty() {
        return base;
    }
    let width = base.size[0] as u64;
    let height = base.size[1] as u64;
    let pixels = base
        .pixels
        .iter()
        .map(|pixel| {
            SceneColorRgba::new(pixel.r(), pixel.g(), pixel.b(), pixel.a()).packed_rgba_u32()
        })
        .collect::<Vec<_>>();
    let Ok(base_rgba) = SceneRgbaImage::new(width, height, pixels) else {
        return base;
    };
    let Ok(output) = render_scene_layers_rgba_cpu(
        &base_rgba,
        &draw_list,
        state.camera.to_camera_state(state.presentation_viewport),
        state.render_viewport,
    ) else {
        return base;
    };
    scene_rgba_image_to_color_image(output.image)
}

fn scene_rgba_image_to_color_image(image: SceneRgbaImage) -> egui::ColorImage {
    let mut rgba = Vec::with_capacity(image.pixels().len() * 4);
    let width = image.width;
    let height = image.height;
    for packed in image.pixels() {
        let color = SceneColorRgba::from_packed_rgba_u32(*packed);
        rgba.extend([color.red, color.green, color.blue, color.alpha]);
    }
    egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba)
}

pub fn mip_to_color_image(image: &MipImageU16, display: LayerDisplay) -> egui::ColorImage {
    mip_to_color_image_with_color(image, display, default_intensity_color())
}

pub(crate) fn mip_to_color_image_with_color(
    image: &MipImageU16,
    display: LayerDisplay,
    color: ChannelColor,
) -> egui::ColorImage {
    let transfer = IntensityTransfer::new(display, color);
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

fn default_intensity_color() -> ChannelColor {
    ChannelColor::new([1.0, 1.0, 1.0, 1.0]).expect("default channel color is valid")
}

#[cfg(test)]
mod tests {
    use mirante4d_core::{DisplayWindow, LayerDisplay};

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
