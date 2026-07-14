//! Native egui/WGPU presentation owned by the process composition root.

use std::{collections::BTreeMap, sync::Arc};

use eframe::egui;
use mirante4d_render_api::{FrameIdentity, PresentationToken, PresentedFrame, RenderExtent};
use mirante4d_render_wgpu::{ValidationCapture, ValidationCaptureTicket, WgpuRenderRuntime};
use mirante4d_ui_egui::EguiPresentationPaint;

use crate::{product_render_intent::ProductRenderRequest, viewer_layout::PanelId};

pub(crate) struct ProductPresentationTarget {
    pub(crate) token: PresentationToken,
    pub(crate) extent: RenderExtent,
    pub(crate) request: Option<ProductRenderRequest>,
    pub(crate) presented: Option<PresentedFrame>,
    pub(crate) pending_capture: Option<(PresentedFrame, ValidationCaptureTicket)>,
    pub(crate) completed_capture: Option<(PresentedFrame, ValidationCapture)>,
    pub(crate) partial_seen: bool,
}

pub(crate) struct ProductGpuRenderRuntime {
    pub(crate) renderer: WgpuRenderRuntime,
    pub(crate) targets: BTreeMap<PanelId, ProductPresentationTarget>,
    next_frame_identity: u64,
    pub(crate) current_partial_frames_presented: u64,
    pub(crate) partial_to_settled_transitions: u64,
    pub(crate) stale_frames_rejected: u64,
}

impl ProductGpuRenderRuntime {
    pub(crate) fn new(renderer: WgpuRenderRuntime) -> Self {
        Self {
            renderer,
            targets: BTreeMap::new(),
            next_frame_identity: 1,
            current_partial_frames_presented: 0,
            partial_to_settled_transitions: 0,
            stale_frames_rejected: 0,
        }
    }

    pub(crate) fn allocate_frame_identity(&mut self) -> FrameIdentity {
        let frame = FrameIdentity::new(self.next_frame_identity);
        self.next_frame_identity = self.next_frame_identity.saturating_add(1);
        frame
    }
}

pub(crate) struct NativePresentationBridge {
    texture_renderer: Option<Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>>,
    device: Option<eframe::wgpu::Device>,
    textures: BTreeMap<PresentationToken, egui::TextureId>,
    pub(crate) product_gpu: Option<ProductGpuRenderRuntime>,
}

impl NativePresentationBridge {
    pub(crate) fn new(
        texture_renderer: Arc<egui::mutex::RwLock<eframe::egui_wgpu::Renderer>>,
        device: eframe::wgpu::Device,
        product_renderer: WgpuRenderRuntime,
    ) -> Self {
        Self {
            texture_renderer: Some(texture_renderer),
            device: Some(device),
            textures: BTreeMap::new(),
            product_gpu: Some(ProductGpuRenderRuntime::new(product_renderer)),
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            texture_renderer: None,
            device: None,
            textures: BTreeMap::new(),
            product_gpu: None,
        }
    }

    pub(crate) fn texture_id(&self, token: PresentationToken) -> Option<egui::TextureId> {
        self.textures.get(&token).copied()
    }

    pub(crate) fn paint(
        &self,
        ui: &mut egui::Ui,
        paint: EguiPresentationPaint,
    ) -> anyhow::Result<()> {
        let texture_id = self
            .texture_id(paint.request().token())
            .ok_or_else(|| anyhow::anyhow!("presentation token has no native texture"))?;
        egui::Image::from_texture((texture_id, paint.rect().size()))
            .fit_to_exact_size(paint.rect().size())
            .paint_at(ui, paint.rect());
        Ok(())
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
