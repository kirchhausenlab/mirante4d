//! Native egui/WGPU presentation owned by the process composition root.

use std::{collections::BTreeMap, sync::Arc};

use eframe::egui;
use mirante4d_render_api::PresentationToken;

pub(crate) struct NativePresentationBridge {
    texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    device: Option<eframe::wgpu::Device>,
    textures: BTreeMap<PresentationToken, egui::TextureId>,
}

impl NativePresentationBridge {
    pub(crate) fn new(
        texture_renderer: Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
        device: eframe::wgpu::Device,
    ) -> Self {
        Self {
            texture_renderer: Some(texture_renderer),
            device: Some(device),
            textures: BTreeMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            texture_renderer: None,
            device: None,
            textures: BTreeMap::new(),
        }
    }

    pub(crate) fn texture_id(&self, token: PresentationToken) -> Option<egui::TextureId> {
        self.textures.get(&token).copied()
    }

    pub(crate) fn bind_texture(
        &mut self,
        token: PresentationToken,
        view: &eframe::wgpu::TextureView,
        extent_changed: bool,
    ) -> anyhow::Result<()> {
        let existing = self.texture_id(token);
        let Some(texture_renderer) = self.texture_renderer.as_ref() else {
            #[cfg(test)]
            if existing.is_some() {
                return Ok(());
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
        self.textures.insert(token, texture_id);
        Ok(())
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

    #[test]
    fn unavailable_bridge_has_no_native_texture_mapping() {
        let bridge = NativePresentationBridge::unavailable();
        let token = PresentationToken::new(1).unwrap();

        assert_eq!(bridge.texture_id(token), None);
    }
}
