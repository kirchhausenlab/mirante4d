use std::collections::HashSet;

use mirante4d_core::{
    ChannelColor, ChannelTransferFunction, LayerDisplay, TransferCurve, TransferPresetId,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppLayerSummary, AppState, ChannelRenderState, DvrOpacityTransfer,
    commands::BuiltInTransferPreset,
    layer_state::{
        active_layer_render_state_from_runtime, set_layer_dvr_opacity_transfer_state,
        set_layer_render_state, set_layer_transfer_state,
    },
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelDisplayPreset {
    pub preset_id: String,
    pub name: String,
    pub entries: Vec<ChannelDisplayPresetEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelDisplayPresetEntry {
    pub layer_id: String,
    pub layer_name: String,
    pub transfer: ChannelTransferFunction,
    pub dvr_opacity_transfer: DvrOpacityTransfer,
    pub render_state: ChannelRenderState,
}

pub(crate) fn built_in_transfer_preset_id(preset: BuiltInTransferPreset) -> TransferPresetId {
    TransferPresetId::new(match preset {
        BuiltInTransferPreset::Linear => "linear",
        BuiltInTransferPreset::BrightGamma => "bright_gamma",
        BuiltInTransferPreset::HighContrast => "high_contrast",
    })
    .expect("built-in transfer preset id is valid")
}

pub(crate) fn built_in_transfer_preset_curve(preset: BuiltInTransferPreset) -> TransferCurve {
    match preset {
        BuiltInTransferPreset::Linear => TransferCurve::Linear,
        BuiltInTransferPreset::BrightGamma => TransferCurve::gamma(2.0).unwrap(),
        BuiltInTransferPreset::HighContrast => TransferCurve::gamma(0.75).unwrap(),
    }
}

pub(crate) fn built_in_transfer_preset_label(preset: BuiltInTransferPreset) -> &'static str {
    match preset {
        BuiltInTransferPreset::Linear => "Linear",
        BuiltInTransferPreset::BrightGamma => "Bright gamma",
        BuiltInTransferPreset::HighContrast => "High contrast",
    }
}

pub(crate) fn built_in_transfer_presets() -> [BuiltInTransferPreset; 3] {
    [
        BuiltInTransferPreset::Linear,
        BuiltInTransferPreset::BrightGamma,
        BuiltInTransferPreset::HighContrast,
    ]
}

pub(crate) fn transfer_preset_label_for_id(preset: &TransferPresetId) -> String {
    match preset.as_str() {
        "linear" => "Linear".to_owned(),
        "bright_gamma" => "Bright gamma".to_owned(),
        "high_contrast" => "High contrast".to_owned(),
        other => other.to_owned(),
    }
}

pub(crate) fn fluorescence_palette_color(index: usize) -> ChannelColor {
    const PALETTE: [[f32; 4]; 8] = [
        [0.00, 0.85, 0.35, 1.0],
        [0.95, 0.20, 0.75, 1.0],
        [0.00, 0.65, 1.00, 1.0],
        [1.00, 0.82, 0.10, 1.0],
        [0.95, 0.25, 0.20, 1.0],
        [0.45, 0.45, 1.00, 1.0],
        [0.90, 0.90, 0.90, 1.0],
        [0.15, 0.95, 0.95, 1.0],
    ];
    ChannelColor::new(PALETTE[index % PALETTE.len()]).expect("built-in palette color is valid")
}

pub(crate) fn transfer_from_layer_summary(layer: &AppLayerSummary) -> ChannelTransferFunction {
    ChannelTransferFunction::new(
        layer.display,
        layer.color,
        layer.curve,
        layer.preset.clone(),
    )
    .map(|transfer| transfer.with_invert(layer.invert))
    .expect("app layer transfer state is validated at mutation boundaries")
}

pub(crate) fn default_channel_presets_from_layers(
    layers: &[AppLayerSummary],
) -> Vec<ChannelDisplayPreset> {
    if layers.is_empty() {
        return Vec::new();
    }
    let mut presets = Vec::with_capacity(layers.len() + 1);
    presets.push(ChannelDisplayPreset {
        preset_id: "all_channels_fluorescence".to_owned(),
        name: "All channels".to_owned(),
        entries: layers
            .iter()
            .enumerate()
            .map(|(index, layer)| ChannelDisplayPresetEntry {
                layer_id: layer.id.clone(),
                layer_name: layer.name.clone(),
                dvr_opacity_transfer: layer.dvr_opacity_transfer,
                render_state: layer.render_state,
                transfer: ChannelTransferFunction::linear(
                    LayerDisplay::new(true, layer.display.window, layer.display.opacity)
                        .expect("dataset layer display is valid"),
                    fluorescence_palette_color(index),
                ),
            })
            .collect(),
    });
    for (index, selected) in layers.iter().enumerate() {
        presets.push(ChannelDisplayPreset {
            preset_id: format!("solo_{}", selected.id),
            name: format!("Solo {}", selected.name),
            entries: layers
                .iter()
                .enumerate()
                .map(|(entry_index, layer)| {
                    let visible = entry_index == index;
                    ChannelDisplayPresetEntry {
                        layer_id: layer.id.clone(),
                        layer_name: layer.name.clone(),
                        dvr_opacity_transfer: layer.dvr_opacity_transfer,
                        render_state: layer.render_state,
                        transfer: ChannelTransferFunction::new(
                            LayerDisplay::new(visible, layer.display.window, layer.display.opacity)
                                .expect("dataset layer display is valid"),
                            if visible {
                                fluorescence_palette_color(index)
                            } else {
                                layer.color
                            },
                            layer.curve,
                            layer.preset.clone(),
                        )
                        .expect("preset transfer is valid")
                        .with_invert(layer.invert),
                    }
                })
                .collect(),
        });
    }
    presets
}

pub(crate) fn channel_preset_from_current_state(
    state: &AppState,
    preset_id: String,
    name: String,
) -> ChannelDisplayPreset {
    ChannelDisplayPreset {
        preset_id,
        name,
        entries: state
            .layers
            .iter()
            .map(|layer| ChannelDisplayPresetEntry {
                layer_id: layer.id.clone(),
                layer_name: layer.name.clone(),
                dvr_opacity_transfer: if layer.id == state.active_layer_id {
                    state.active_dvr_opacity_transfer
                } else {
                    layer.dvr_opacity_transfer
                },
                render_state: if layer.id == state.active_layer_id {
                    active_layer_render_state_from_runtime(state)
                } else {
                    layer.render_state
                },
                transfer: if layer.id == state.active_layer_id {
                    state.active_layer_transfer.clone()
                } else {
                    transfer_from_layer_summary(layer)
                },
            })
            .collect(),
    }
}

pub(crate) fn next_user_channel_preset_id(state: &AppState) -> String {
    let mut index = 1usize;
    loop {
        let candidate = format!("user_display_{index}");
        if state
            .channel_presets
            .iter()
            .all(|preset| preset.preset_id != candidate)
        {
            return candidate;
        }
        index += 1;
    }
}

pub(crate) fn validate_channel_presets_for_state(
    state: &AppState,
    presets: &[ChannelDisplayPreset],
) -> Vec<String> {
    presets
        .iter()
        .flat_map(|preset| validate_channel_preset_for_state(state, preset))
        .collect()
}

fn validate_channel_preset_for_state(
    state: &AppState,
    preset: &ChannelDisplayPreset,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if preset.preset_id.is_empty() {
        warnings.push(format!("channel preset {:?} has an empty id", preset.name));
    }
    if preset.name.trim().is_empty() {
        warnings.push(format!(
            "channel preset {:?} has an empty name",
            preset.preset_id
        ));
    }
    let mut seen = HashSet::new();
    for entry in &preset.entries {
        if !seen.insert(entry.layer_id.clone()) {
            warnings.push(format!(
                "channel preset {:?} contains duplicate layer {:?}",
                preset.name, entry.layer_id
            ));
            continue;
        }
        match state.layers.iter().find(|layer| layer.id == entry.layer_id) {
            Some(layer) if layer.name != entry.layer_name => warnings.push(format!(
                "channel preset {:?} references layer {:?} with stale name {:?}; current name is {:?}",
                preset.name, entry.layer_id, entry.layer_name, layer.name
            )),
            Some(_) => {
                if let Err(err) = DvrOpacityTransfer::new(
                    entry.dvr_opacity_transfer.window,
                    entry.dvr_opacity_transfer.curve,
                ) {
                    warnings.push(format!(
                        "channel preset {:?} has invalid DVR opacity transfer for layer {:?}: {err}",
                        preset.name, entry.layer_id
                    ));
                }
                if let Err(err) = ChannelTransferFunction::new(
                    entry.transfer.display,
                    entry.transfer.color,
                    entry.transfer.curve,
                    entry.transfer.preset.clone(),
                )
                .map(|transfer| transfer.with_invert(entry.transfer.invert))
                {
                    warnings.push(format!(
                        "channel preset {:?} has invalid transfer for layer {:?}: {err}",
                        preset.name, entry.layer_id
                    ));
                }
                match entry.render_state {
                    ChannelRenderState::Isosurface(parameters)
                        if !parameters.display_level.is_finite()
                            || !(0.0..=1.0).contains(&parameters.display_level) =>
                    {
                        warnings.push(format!(
                            "channel preset {:?} has invalid ISO display level for layer {:?}",
                            preset.name, entry.layer_id
                        ));
                    }
                    ChannelRenderState::Dvr(parameters)
                        if !parameters.density_scale.is_finite()
                            || parameters.density_scale <= 0.0 =>
                    {
                        warnings.push(format!(
                            "channel preset {:?} has invalid DVR density scale for layer {:?}",
                            preset.name, entry.layer_id
                        ));
                    }
                    _ => {}
                }
            }
            None => warnings.push(format!(
                "channel preset {:?} references missing layer {:?}",
                preset.name, entry.layer_id
            )),
        }
    }
    for layer in &state.layers {
        if !seen.contains(&layer.id) {
            warnings.push(format!(
                "channel preset {:?} has no entry for layer {:?}",
                preset.name, layer.id
            ));
        }
    }
    warnings
}

pub(crate) fn apply_channel_display_preset(
    state: &mut AppState,
    preset_index: usize,
) -> anyhow::Result<bool> {
    let preset = state
        .channel_presets
        .get(preset_index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("channel preset index {preset_index} is out of range"))?;
    let warnings = validate_channel_preset_for_state(state, &preset);
    if !warnings.is_empty() {
        state.channel_preset_warnings = warnings.clone();
        anyhow::bail!(
            "channel preset {:?} is stale: {}",
            preset.name,
            warnings.join("; ")
        );
    }
    let mut changed = false;
    for entry in &preset.entries {
        let layer_index = state
            .layers
            .iter()
            .position(|layer| layer.id == entry.layer_id)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "channel preset references missing layer {:?}",
                    entry.layer_id
                )
            })?;
        changed |= set_layer_transfer_state(state, layer_index, entry.transfer.clone())?;
        changed |=
            set_layer_dvr_opacity_transfer_state(state, layer_index, entry.dvr_opacity_transfer)?;
        changed |= set_layer_render_state(state, layer_index, entry.render_state)?;
    }
    state.selected_channel_preset_index = Some(preset_index);
    state.channel_preset_warnings.clear();
    state.last_workflow_message = Some(format!("Applied channel preset {}", preset.name));
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_transfer_presets_have_stable_ids_labels_and_curves() {
        let presets = built_in_transfer_presets();

        assert_eq!(
            presets
                .iter()
                .map(|preset| built_in_transfer_preset_id(*preset).as_str().to_owned())
                .collect::<Vec<_>>(),
            vec!["linear", "bright_gamma", "high_contrast"]
        );
        assert_eq!(
            presets
                .iter()
                .map(|preset| built_in_transfer_preset_label(*preset))
                .collect::<Vec<_>>(),
            vec!["Linear", "Bright gamma", "High contrast"]
        );
        assert_eq!(
            built_in_transfer_preset_curve(BuiltInTransferPreset::Linear),
            TransferCurve::Linear
        );
        assert_eq!(
            built_in_transfer_preset_curve(BuiltInTransferPreset::BrightGamma),
            TransferCurve::gamma(2.0).unwrap()
        );
    }

    #[test]
    fn transfer_preset_label_falls_back_to_custom_id() {
        let custom = TransferPresetId::new("custom_gamma").unwrap();

        assert_eq!(transfer_preset_label_for_id(&custom), "custom_gamma");
    }
}
