use mirante4d_dataset::DatasetCatalog;
use mirante4d_domain::{LayerTransfer, RenderState, RgbColor, TransferCurve};
use mirante4d_project_model::{
    ChannelPreset, ChannelPresetEntry, ChannelPresetId, LayerViewState, ProjectModelError,
    ViewState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuiltInTransferPreset {
    Linear,
    BrightGamma,
    HighContrast,
}

#[cfg(test)]
pub(crate) fn built_in_transfer_preset_id(preset: BuiltInTransferPreset) -> &'static str {
    match preset {
        BuiltInTransferPreset::Linear => "linear",
        BuiltInTransferPreset::BrightGamma => "bright_gamma",
        BuiltInTransferPreset::HighContrast => "high_contrast",
    }
}

pub(crate) fn built_in_transfer_preset_curve(preset: BuiltInTransferPreset) -> TransferCurve {
    match preset {
        BuiltInTransferPreset::Linear => TransferCurve::linear(),
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

pub(crate) fn fluorescence_palette_color(index: usize) -> RgbColor {
    const PALETTE: [[f32; 3]; 8] = [
        [0.00, 0.85, 0.35],
        [0.95, 0.20, 0.75],
        [0.00, 0.65, 1.00],
        [1.00, 0.82, 0.10],
        [0.95, 0.25, 0.20],
        [0.45, 0.45, 1.00],
        [0.90, 0.90, 0.90],
        [0.15, 0.95, 0.95],
    ];
    RgbColor::new(PALETTE[index % PALETTE.len()]).expect("built-in palette color is valid")
}

pub(crate) fn default_channel_presets(
    catalog: &DatasetCatalog,
    view: &ViewState,
) -> Result<Vec<ChannelPreset>, ProjectModelError> {
    let all_entries = view
        .layers()
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            ChannelPresetEntry::new(
                layer.layer_key(),
                true,
                LayerTransfer::new(
                    layer.transfer().window(),
                    fluorescence_palette_color(index),
                    layer.transfer().opacity(),
                    TransferCurve::linear(),
                    false,
                ),
                *layer.render_state(),
            )
        })
        .collect();
    let mut presets = vec![ChannelPreset::new(
        ChannelPresetId::new("all_channels_fluorescence")?,
        "All channels",
        all_entries,
    )?];

    for (selected_index, selected_layer) in view.layers().iter().enumerate() {
        let entries = view
            .layers()
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                let visible = index == selected_index;
                ChannelPresetEntry::new(
                    layer.layer_key(),
                    visible,
                    LayerTransfer::new(
                        layer.transfer().window(),
                        if visible {
                            fluorescence_palette_color(selected_index)
                        } else {
                            layer.transfer().color()
                        },
                        layer.transfer().opacity(),
                        layer.transfer().curve(),
                        layer.transfer().invert(),
                    ),
                    *layer.render_state(),
                )
            })
            .collect();
        let layer_label = catalog
            .layer(selected_layer.layer_key())
            .expect("catalog and view layer closure is constructed together")
            .label();
        presets.push(ChannelPreset::new(
            ChannelPresetId::new(format!("solo_{selected_index}"))?,
            format!("Solo {layer_label}"),
            entries,
        )?);
    }
    Ok(presets)
}

pub(crate) fn channel_preset_from_current_view(
    view: &ViewState,
    preset_id: ChannelPresetId,
    label: impl AsRef<str>,
) -> Result<ChannelPreset, ProjectModelError> {
    ChannelPreset::new(
        preset_id,
        label,
        view.layers()
            .iter()
            .map(channel_preset_entry_from_layer)
            .collect(),
    )
}

pub(crate) fn next_user_channel_preset_id(presets: &[ChannelPreset]) -> ChannelPresetId {
    let mut index = 1usize;
    loop {
        let candidate = format!("user_display_{index}");
        if presets
            .iter()
            .all(|preset| preset.id().as_str() != candidate.as_str())
        {
            return ChannelPresetId::new(candidate)
                .expect("generated user channel preset ID is valid");
        }
        index = index
            .checked_add(1)
            .expect("user channel preset counter exhausted");
    }
}

fn channel_preset_entry_from_layer(layer: &LayerViewState) -> ChannelPresetEntry {
    channel_preset_entry(
        layer,
        layer.visible(),
        layer.transfer().clone(),
        *layer.render_state(),
    )
}

fn channel_preset_entry(
    layer: &LayerViewState,
    visible: bool,
    transfer: LayerTransfer,
    render_state: RenderState,
) -> ChannelPresetEntry {
    ChannelPresetEntry::new(layer.layer_key(), visible, transfer, render_state)
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
                .map(|preset| built_in_transfer_preset_id(*preset).to_owned())
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
            TransferCurve::linear()
        );
        assert_eq!(
            built_in_transfer_preset_curve(BuiltInTransferPreset::BrightGamma),
            TransferCurve::gamma(2.0).unwrap()
        );
    }
}
