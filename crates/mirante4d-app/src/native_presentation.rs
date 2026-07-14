//! Native egui/WGPU presentation owned by the process composition root.

use std::sync::Arc;

use eframe::egui;

pub(crate) struct NativePresentationBridge {
    texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    device: Option<eframe::wgpu::Device>,
}

impl NativePresentationBridge {
    pub(crate) fn new(
        texture_renderer: Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
        device: eframe::wgpu::Device,
    ) -> Self {
        Self {
            texture_renderer: Some(texture_renderer),
            device: Some(device),
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            texture_renderer: None,
            device: None,
        }
    }

    pub(crate) fn bind_texture(
        &self,
        view: &eframe::wgpu::TextureView,
        existing: Option<egui::TextureId>,
        extent_changed: bool,
    ) -> anyhow::Result<egui::TextureId> {
        let Some(texture_renderer) = self.texture_renderer.as_ref() else {
            #[cfg(test)]
            if let Some(texture_id) = existing {
                return Ok(texture_id);
            }
            anyhow::bail!("wgpu texture renderer is unavailable");
        };
        let device = self
            .device
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("wgpu device is unavailable"))?;
        let mut texture_renderer = texture_renderer.write();
        let texture_id = if let Some(texture_id) = existing {
            if extent_changed {
                texture_renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    view,
                    display_texture_filter(),
                    texture_id,
                );
            }
            texture_id
        } else {
            texture_renderer.register_native_texture(device, view, display_texture_filter())
        };
        Ok(texture_id)
    }
}

fn display_texture_filter() -> eframe::wgpu::FilterMode {
    eframe::wgpu::FilterMode::Linear
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_texture_handoff_uses_linear_filtering() {
        assert_eq!(display_texture_filter(), eframe::wgpu::FilterMode::Linear);
    }
}
