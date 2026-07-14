use std::{
    fs,
    path::{Path, PathBuf},
};

use eframe::egui;
use serde_json::{Value, json};

use crate::{MiranteWorkbenchApp, viewer_layout::PanelId};

pub(crate) fn product_target_capture(
    app: &MiranteWorkbenchApp,
    panel: PanelId,
) -> Option<&mirante4d_render_wgpu::ValidationCapture> {
    let target = app
        .render_runtime
        .product_gpu
        .as_ref()?
        .targets
        .get(&panel)?;
    let (presentation, capture) = target.completed_capture.as_ref()?;
    (target.presented.as_ref() == Some(presentation)).then_some(capture)
}

#[derive(Debug, Clone)]
pub(crate) struct ProductAutomationArtifact {
    pub(crate) kind: &'static str,
    pub(crate) format: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) command_index: usize,
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
) -> Result<(&'static str, egui::ColorImage), String> {
    if let Some(capture) = product_target_capture(app, PanelId::ThreeD) {
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
        .render_runtime
        .product_gpu
        .as_ref()
        .and_then(|product| product.targets.get(&PanelId::ThreeD))
        .and_then(|target| target.presented.as_ref())
        .is_some()
    {
        return Err("current GPU validation capture is still pending".to_owned());
    }
    Err("no current GPU display frame is available".to_owned())
}

pub(crate) fn current_display_image_stats(
    app: &MiranteWorkbenchApp,
) -> Result<(&'static str, ProductAutomationImageStats), String> {
    if let Some(capture) = product_target_capture(app, PanelId::ThreeD) {
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
        .render_runtime
        .product_gpu
        .as_ref()
        .and_then(|product| product.targets.get(&PanelId::ThreeD))
        .and_then(|target| target.presented.as_ref())
        .is_some()
    {
        return Err("current GPU validation capture is still pending".to_owned());
    }
    Err("no current GPU display frame is available".to_owned())
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
