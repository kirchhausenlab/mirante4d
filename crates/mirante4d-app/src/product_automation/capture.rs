use std::{
    fs,
    path::{Path, PathBuf},
};

use eframe::egui;
use serde_json::{Value, json};

use crate::{
    MiranteWorkbenchApp, native_presentation::texture_revision_is_current, viewer_layout::PanelId,
};

pub(crate) fn product_target_capture(
    app: &MiranteWorkbenchApp,
    panel: PanelId,
) -> Option<&mirante4d_render_wgpu::ValidationCapture> {
    let target = panel.presentation_slot();
    let completed = app
        .native_presentation
        .product_gpu
        .as_ref()?
        .completed_validation_capture(target)?;
    (completed.ticket.target() == target
        && texture_revision_is_current(
            app.native_presentation.texture_binding_identity(target),
            completed.ticket.device_generation().get(),
            completed.ticket.texture_revision().get(),
        )
        && app.render_coordination.surface(target).presented_frame() == Some(&completed.frame))
    .then_some(&completed.capture)
}

#[derive(Debug, Clone)]
pub(crate) struct ProductAutomationArtifact {
    pub(crate) kind: &'static str,
    pub(crate) format: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) command_index: usize,
    pub(crate) target: &'static str,
    pub(crate) frame_identity: u64,
    pub(crate) surface_generation: u64,
    pub(crate) capture_source: &'static str,
    pub(crate) pixel_stats: ProductAutomationImageStats,
}

impl ProductAutomationArtifact {
    pub(crate) fn json(&self) -> Value {
        json!({
            "kind": self.kind,
            "format": self.format,
            "path": self.path.display().to_string(),
            "width": self.width,
            "height": self.height,
            "command_index": self.command_index,
            "target": self.target,
            "frame_identity": self.frame_identity,
            "surface_generation": self.surface_generation,
            "capture_source": self.capture_source,
            "pixel_stats": self.pixel_stats.json(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProductAutomationImageStats {
    pub(crate) pixel_count: usize,
    pub(crate) nonzero_rgb_pixels: usize,
    pub(crate) min_rgb: u8,
    pub(crate) max_rgb: u8,
    pub(crate) mean_rgb: f64,
}

impl ProductAutomationImageStats {
    pub(crate) fn from_color_image(image: &egui::ColorImage) -> Self {
        let mut min_rgb = u8::MAX;
        let mut max_rgb = u8::MIN;
        let mut nonzero_rgb_pixels = 0usize;
        let mut rgb_sum = 0u64;
        for pixel in &image.pixels {
            let channels = [pixel.r(), pixel.g(), pixel.b()];
            if channels.iter().any(|value| *value > 0) {
                nonzero_rgb_pixels += 1;
            }
            for value in channels {
                min_rgb = min_rgb.min(value);
                max_rgb = max_rgb.max(value);
                rgb_sum += u64::from(value);
            }
        }
        if image.pixels.is_empty() {
            min_rgb = 0;
        }
        let rgb_sample_count = image.pixels.len() * 3;
        let mean_rgb = if rgb_sample_count == 0 {
            0.0
        } else {
            rgb_sum as f64 / rgb_sample_count as f64
        };
        Self {
            pixel_count: image.pixels.len(),
            nonzero_rgb_pixels,
            min_rgb,
            max_rgb,
            mean_rgb,
        }
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.nonzero_rgb_pixels == 0 || self.max_rgb == 0
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "pixel_count": self.pixel_count,
            "nonzero_rgb_pixels": self.nonzero_rgb_pixels,
            "min_rgb": self.min_rgb,
            "max_rgb": self.max_rgb,
            "mean_rgb": self.mean_rgb,
        })
    }
}

pub(crate) fn sanitize_artifact_label(raw: &str) -> String {
    raw.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

pub(crate) fn capture_color_image(
    app: &mut MiranteWorkbenchApp,
    panel: PanelId,
) -> Result<(&'static str, egui::ColorImage), String> {
    if let Some(capture) = product_target_capture(app, panel) {
        let width = usize::try_from(capture.extent().width_pixels())
            .map_err(|_| "GPU display frame width does not fit in usize".to_owned())?;
        let height = usize::try_from(capture.extent().height_pixels())
            .map_err(|_| "GPU display frame height does not fit in usize".to_owned())?;
        return Ok((
            "gpu_display_frame_readback",
            color_image_from_rgba(width, height, capture.rgba8())?,
        ));
    }
    if app
        .render_coordination
        .surface(panel.presentation_slot())
        .presented_frame()
        .is_some()
    {
        return Err(format!(
            "current {} GPU validation capture is still pending",
            panel.label()
        ));
    }
    Err(format!(
        "no current {} GPU display frame is available",
        panel.label()
    ))
}

pub(crate) fn current_display_image_stats(
    app: &MiranteWorkbenchApp,
    panel: PanelId,
) -> Result<(&'static str, ProductAutomationImageStats), String> {
    if let Some(capture) = product_target_capture(app, panel) {
        let width = usize::try_from(capture.extent().width_pixels())
            .map_err(|_| "GPU display frame width does not fit in usize".to_owned())?;
        let height = usize::try_from(capture.extent().height_pixels())
            .map_err(|_| "GPU display frame height does not fit in usize".to_owned())?;
        let image = color_image_from_rgba(width, height, capture.rgba8())?;
        return Ok((
            "gpu_display_frame_readback",
            ProductAutomationImageStats::from_color_image(&image),
        ));
    }
    if app
        .render_coordination
        .surface(panel.presentation_slot())
        .presented_frame()
        .is_some()
    {
        return Err(format!(
            "current {} GPU validation capture is still pending",
            panel.label()
        ));
    }
    Err(format!(
        "no current {} GPU display frame is available",
        panel.label()
    ))
}

pub(crate) fn color_image_from_rgba(
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<egui::ColorImage, String> {
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| "viewport capture dimensions overflowed".to_owned())?;
    let expected_bytes = pixel_count
        .checked_mul(4)
        .ok_or_else(|| "viewport capture RGBA byte count overflowed".to_owned())?;
    if rgba.len() != expected_bytes {
        return Err(format!(
            "GPU display frame readback returned {} bytes for {width}x{height}, expected {expected_bytes}",
            rgba.len()
        ));
    }
    let pixels = rgba
        .chunks_exact(4)
        .map(|pixel| egui::Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]))
        .collect();
    Ok(egui::ColorImage {
        size: [width, height],
        pixels,
        source_size: egui::Vec2::new(width as f32, height as f32),
    })
}

pub(crate) fn write_color_image_ppm(path: &Path, image: &egui::ColorImage) -> std::io::Result<()> {
    let [width, height] = image.size;
    let mut bytes = format!("P6\n{width} {height}\n255\n").into_bytes();
    bytes.reserve(image.pixels.len() * 3);
    for pixel in &image.pixels {
        bytes.push(pixel.r());
        bytes.push(pixel.g());
        bytes.push(pixel.b());
    }
    fs::write(path, bytes)
}
