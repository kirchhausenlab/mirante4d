use crate::{
    application_view, current_physical_layer_id, current_runtime::dataset::CurrentDatasetRuntime,
};
use mirante4d_application::ApplicationSnapshot;
#[cfg(test)]
use mirante4d_domain::RenderMode;
use mirante4d_domain::RenderState;
use mirante4d_format::LayerId;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayGraph {
    pub(crate) channels: Vec<DisplayGraphChannel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayGraphChannel {
    pub(crate) layer_index: usize,
    pub(crate) layer_id: LayerId,
    pub(crate) render_state: RenderState,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayChannelModeIdentity {
    pub(crate) layer_id: LayerId,
    pub(crate) render_state: RenderState,
}

impl DisplayGraph {
    pub(crate) fn from_snapshot(
        snapshot: &ApplicationSnapshot,
        dataset: &CurrentDatasetRuntime,
    ) -> anyhow::Result<Self> {
        let channels = application_view(snapshot)
            .layers()
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.visible())
            .map(|(layer_index, layer)| {
                Ok(DisplayGraphChannel {
                    layer_index,
                    layer_id: current_physical_layer_id(dataset, layer.layer_key())?.clone(),
                    render_state: *layer.render_state(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self { channels })
    }

    pub(crate) fn mode_identities(&self) -> Vec<DisplayChannelModeIdentity> {
        self.channels
            .iter()
            .map(|channel| DisplayChannelModeIdentity {
                layer_id: channel.layer_id.clone(),
                render_state: channel.render_state,
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn channel_ids_for_mode(&self, mode: RenderMode) -> Vec<&str> {
        self.channels
            .iter()
            .filter(|channel| channel.render_state.mode() == mode)
            .map(|channel| channel.layer_id.as_str())
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
    fn display_graph_partitions_canonical_render_modes() {
        let graph = DisplayGraph {
            channels: vec![
                DisplayGraphChannel {
                    layer_index: 0,
                    layer_id: LayerId::new("ch0").unwrap(),
                    render_state: RenderState::mip(SamplingPolicy::SmoothLinear),
                },
                DisplayGraphChannel {
                    layer_index: 1,
                    layer_id: LayerId::new("ch1").unwrap(),
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

        assert_eq!(graph.channel_ids_for_mode(RenderMode::Mip), vec!["ch0"]);
        assert_eq!(graph.channel_ids_for_mode(RenderMode::Dvr), vec!["ch1"]);
        assert!(graph.is_mixed_mode());
        assert_eq!(graph.mode_identities().len(), 2);
    }
}
