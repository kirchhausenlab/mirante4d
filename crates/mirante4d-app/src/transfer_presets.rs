use mirante4d_dataset::DatasetCatalog;
use mirante4d_domain::{LayerTransfer, RgbColor, TransferCurve};
use mirante4d_project_model::{
    ChannelPreset, ChannelPresetEntry, ChannelPresetId, ProjectModelError, ViewState,
};

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
