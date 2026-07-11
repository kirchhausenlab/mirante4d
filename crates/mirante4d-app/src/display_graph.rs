#[cfg(test)]
use crate::RenderMode;
use crate::{AppState, ChannelRenderState, layer_state::active_layer_render_state_from_runtime};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayGraph {
    pub(crate) channels: Vec<DisplayGraphChannel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayGraphChannel {
    pub(crate) layer_index: usize,
    pub(crate) layer_id: String,
    pub(crate) render_state: ChannelRenderState,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DisplayChannelModeIdentity {
    pub(crate) layer_id: String,
    pub(crate) render_state: ChannelRenderState,
}

impl DisplayGraph {
    pub(crate) fn from_state(state: &AppState) -> Self {
        let active_render_state = active_layer_render_state_from_runtime(state);
        let channels = state
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.display.visible)
            .map(|(layer_index, layer)| {
                let render_state = if layer_index == state.active_layer_index {
                    active_render_state
                } else {
                    layer.render_state
                };
                DisplayGraphChannel {
                    layer_index,
                    layer_id: layer.id.clone(),
                    render_state,
                }
            })
            .collect();
        Self { channels }
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
    use mirante4d_core::TimeIndex;
    use mirante4d_format::{FixtureKind, write_fixture};

    use crate::{
        ChannelRenderState, RenderIsoShadingPolicy, RenderMode, RenderSamplingPolicy,
        layer_state::{activate_layer_timepoint, default_dvr_opacity_transfer},
    };

    use super::*;

    #[test]
    fn display_graph_partitions_visible_channels_by_render_mode() {
        let tempdir = tempfile::tempdir().unwrap();
        let root =
            write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
        let mut state = crate::open_dataset_and_render_first_frame(root).unwrap();
        state.layers[0].display.visible = true;
        state.layers[1].display.visible = true;
        state.layers[1].render_state = ChannelRenderState::for_mode(
            RenderMode::Dvr,
            RenderSamplingPolicy::VoxelExact,
            RenderIsoShadingPolicy::Flat,
            0.5,
            default_dvr_opacity_transfer(state.layers[1].display),
            18.0,
        );

        let graph = DisplayGraph::from_state(&state);

        assert_eq!(graph.channel_ids_for_mode(RenderMode::Mip), vec!["ch0"]);
        assert_eq!(graph.channel_ids_for_mode(RenderMode::Dvr), vec!["ch1"]);
        assert!(graph.is_mixed_mode());
    }

    #[test]
    fn display_graph_partitions_render_modes() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::MultiChannelU16_8Cube4C, tempdir.path()).unwrap();
        let mut state = crate::open_dataset_and_render_first_frame(root).unwrap();
        for layer in &mut state.layers {
            layer.display.visible = true;
        }
        state.layers[1].render_state = ChannelRenderState::for_mode(
            RenderMode::Dvr,
            RenderSamplingPolicy::VoxelExact,
            RenderIsoShadingPolicy::Flat,
            0.5,
            default_dvr_opacity_transfer(state.layers[1].display),
            18.0,
        );
        state.layers[2].render_state = ChannelRenderState::for_mode(
            RenderMode::Isosurface,
            RenderSamplingPolicy::VoxelExact,
            RenderIsoShadingPolicy::GradientLighting,
            0.35,
            default_dvr_opacity_transfer(state.layers[2].display),
            18.0,
        );

        let graph = DisplayGraph::from_state(&state);

        assert_eq!(
            graph.channel_ids_for_mode(RenderMode::Mip),
            vec!["ch0", "ch3"]
        );
        assert_eq!(graph.channel_ids_for_mode(RenderMode::Dvr), vec!["ch1"]);
        assert_eq!(
            graph.channel_ids_for_mode(RenderMode::Isosurface),
            vec!["ch2"]
        );
        assert!(graph.is_mixed_mode());
    }

    #[test]
    fn display_graph_uses_active_runtime_render_state_for_active_channel() {
        let tempdir = tempfile::tempdir().unwrap();
        let root =
            write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
        let mut state = crate::open_dataset_and_render_first_frame(root).unwrap();
        activate_layer_timepoint(&mut state, 1, TimeIndex(0)).unwrap();
        state.active_render_mode = RenderMode::Isosurface;
        state.render_sampling_policy = RenderSamplingPolicy::VoxelExact;
        state.render_iso_shading_policy = RenderIsoShadingPolicy::Flat;
        state.iso_display_level = 0.25;

        let graph = DisplayGraph::from_state(&state);
        let active_identity = graph
            .mode_identities()
            .into_iter()
            .find(|identity| identity.layer_id == "ch1")
            .unwrap();

        let ChannelRenderState::Isosurface(parameters) = active_identity.render_state else {
            panic!("expected active channel to use runtime ISO state");
        };
        assert_eq!(parameters.sampling_policy, RenderSamplingPolicy::VoxelExact);
        assert_eq!(parameters.shading_policy, RenderIsoShadingPolicy::Flat);
        assert_eq!(parameters.display_level, 0.25);
    }

    #[test]
    fn display_graph_omits_hidden_channels() {
        let tempdir = tempfile::tempdir().unwrap();
        let root =
            write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
        let mut state = crate::open_dataset_and_render_first_frame(root).unwrap();
        state.layers[0].display.visible = true;
        state.layers[1].display.visible = false;

        let graph = DisplayGraph::from_state(&state);

        assert_eq!(graph.mode_identities().len(), 1);
        assert_eq!(graph.mode_identities()[0].layer_id, "ch0");
    }
}
