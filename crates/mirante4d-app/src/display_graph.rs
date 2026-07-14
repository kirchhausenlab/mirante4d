use mirante4d_application::ApplicationSnapshot;
#[cfg(test)]
use mirante4d_domain::RenderMode;
use mirante4d_domain::{LogicalLayerKey, RenderState};

use crate::application_view;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayGraph {
    pub(crate) channels: Vec<DisplayGraphChannel>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayGraphChannel {
    pub(crate) layer: LogicalLayerKey,
    pub(crate) render_state: RenderState,
}

impl DisplayGraph {
    pub(crate) fn from_snapshot(snapshot: &ApplicationSnapshot) -> Self {
        let channels = application_view(snapshot)
            .layers()
            .iter()
            .filter(|layer| layer.visible())
            .map(|layer| DisplayGraphChannel {
                layer: layer.layer_key(),
                render_state: *layer.render_state(),
            })
            .collect();
        Self { channels }
    }

    #[cfg(test)]
    fn channel_keys_for_mode(&self, mode: RenderMode) -> Vec<LogicalLayerKey> {
        self.channels
            .iter()
            .filter(|channel| channel.render_state.mode() == mode)
            .map(|channel| channel.layer)
            .collect()
    }

    pub(crate) fn is_mixed_mode(&self) -> bool {
        let mut modes = self
            .channels
            .iter()
            .map(|channel| channel.render_state.mode());
        let Some(first) = modes.next() else {
            return false;
        };
        modes.any(|mode| mode != first)
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::{DisplayWindow, DvrOpacityTransfer, SamplingPolicy, TransferCurve};

    use super::*;

    #[test]
    fn display_graph_partitions_canonical_render_modes_by_logical_key() {
        let graph = DisplayGraph {
            channels: vec![
                DisplayGraphChannel {
                    layer: LogicalLayerKey::new(0),
                    render_state: RenderState::mip(SamplingPolicy::VoxelExact),
                },
                DisplayGraphChannel {
                    layer: LogicalLayerKey::new(1),
                    render_state: RenderState::dvr(
                        SamplingPolicy::VoxelExact,
                        DvrOpacityTransfer::new(
                            DisplayWindow::new(0.0, 1.0).unwrap(),
                            TransferCurve::linear(),
                        ),
                        18.0,
                    )
                    .unwrap(),
                },
            ],
        };

        assert_eq!(
            graph.channel_keys_for_mode(RenderMode::Mip),
            vec![LogicalLayerKey::new(0)]
        );
        assert_eq!(
            graph.channel_keys_for_mode(RenderMode::Dvr),
            vec![LogicalLayerKey::new(1)]
        );
        assert!(graph.is_mixed_mode());
    }
}
