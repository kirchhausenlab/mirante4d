use super::*;

pub fn write_native_u16_dataset(
    package_root: impl AsRef<Path>,
    dataset: NativeU16Dataset,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), FormatError> {
    let dataset = NativeU16MultiscaleDataset {
        id: dataset.id,
        name: dataset.name,
        world_space: dataset.world_space,
        layers: dataset
            .layers
            .into_iter()
            .map(|layer| DenseU16MultiscaleLayer {
                id: layer.id,
                name: layer.name,
                channel: layer.channel,
                shape: layer.shape,
                grid_to_world: layer.grid_to_world,
                display: layer.display,
                scales: vec![DenseU16Scale {
                    level: 0,
                    shape: layer.shape,
                    brick_shape: layer.brick_shape,
                    grid_to_world: layer.grid_to_world,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                    values_tzyx: layer.values_tzyx,
                }],
            })
            .collect(),
    };
    write_native_u16_multiscale_dataset(package_root, dataset, existing_policy)
}

pub fn write_native_u16_multiscale_dataset(
    package_root: impl AsRef<Path>,
    dataset: NativeU16MultiscaleDataset,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), FormatError> {
    let mut writer = NativeMultiscaleDatasetWriter::create(
        package_root,
        dataset.id,
        dataset.name,
        dataset.world_space,
        existing_policy,
    )?;
    for layer in dataset.layers {
        writer.write_layer(layer)?;
    }
    writer.finish()
}

pub fn write_native_f32_dataset(
    package_root: impl AsRef<Path>,
    dataset: NativeF32Dataset,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), FormatError> {
    let dataset = NativeF32MultiscaleDataset {
        id: dataset.id,
        name: dataset.name,
        world_space: dataset.world_space,
        layers: dataset
            .layers
            .into_iter()
            .map(|layer| DenseF32MultiscaleLayer {
                id: layer.id,
                name: layer.name,
                channel: layer.channel,
                shape: layer.shape,
                grid_to_world: layer.grid_to_world,
                display: layer.display,
                scales: vec![DenseF32Scale {
                    level: 0,
                    shape: layer.shape,
                    brick_shape: layer.brick_shape,
                    grid_to_world: layer.grid_to_world,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                    values_tzyx: layer.values_tzyx,
                }],
            })
            .collect(),
    };
    write_native_f32_multiscale_dataset(package_root, dataset, existing_policy)
}

pub fn write_native_f32_multiscale_dataset(
    package_root: impl AsRef<Path>,
    dataset: NativeF32MultiscaleDataset,
    existing_policy: ExistingPackagePolicy,
) -> Result<(), FormatError> {
    let mut writer = NativeMultiscaleDatasetWriter::create(
        package_root,
        dataset.id,
        dataset.name,
        dataset.world_space,
        existing_policy,
    )?;
    for layer in dataset.layers {
        writer.write_f32_layer(layer)?;
    }
    writer.finish()
}

pub fn default_u16_display() -> LayerDisplay {
    LayerDisplay::new(true, DisplayWindow::new(0.0, 65535.0).unwrap(), 1.0).unwrap()
}

pub fn default_f32_display() -> LayerDisplay {
    LayerDisplay::new(true, DisplayWindow::new(0.0, 1.0).unwrap(), 1.0).unwrap()
}

pub(super) fn write_dense_u16_multiscale_layer(
    store: &ReadableWritableListableStorage,
    layer: DenseU16MultiscaleLayer,
) -> Result<LayerManifest, FormatError> {
    let DenseU16MultiscaleLayer {
        id,
        name,
        channel,
        shape,
        grid_to_world,
        display,
        scales: input_scales,
    } = layer;
    validate_layer_scales(&id, shape, grid_to_world, &input_scales)?;
    let mut scales = Vec::with_capacity(input_scales.len());
    for scale in input_scales {
        validate_scale_values(&id, &scale)?;
        let array_path = format!("arrays/intensity/{}/s{}", id, scale.level);
        let array = create_u16_array(store, &array_path, scale.shape, scale.brick_shape)?;
        array
            .store_array_subset_opt(
                &array.subset_all(),
                scale.values_tzyx.as_slice(),
                &store_all_chunks_options(),
            )
            .map_err(zarr_storage_error)?;

        let bricks = build_brick_table(&array, &id, &scale)?;
        let storage = sharded_storage_metadata(
            &id,
            &array,
            array_path.as_str(),
            IntensityDType::Uint16,
            scale.shape,
            scale.brick_shape,
        )?;
        let statistics = statistics_for_values(&scale.values_tzyx);
        scales.push(ScaleManifest {
            level: scale.level,
            array_path,
            shape: scale.shape,
            storage,
            grid_to_world: scale.grid_to_world,
            source_scale: scale.source_scale,
            reduction: scale.reduction,
            statistics,
            validity: None,
            bricks,
        });
    }
    Ok(LayerManifest {
        id,
        kind: LayerKind::DenseIntensity,
        name,
        channel,
        shape,
        dtype: DTypeMetadata {
            source: IntensityDType::Uint16,
            stored: IntensityDType::Uint16,
            conversion: DTypeConversion::Lossless,
        },
        no_data_policy: None,
        grid_to_world,
        display,
        scales,
    })
}

pub(super) fn write_dense_f32_multiscale_layer(
    store: &ReadableWritableListableStorage,
    layer: DenseF32MultiscaleLayer,
) -> Result<LayerManifest, FormatError> {
    let DenseF32MultiscaleLayer {
        id,
        name,
        channel,
        shape,
        grid_to_world,
        display,
        scales: input_scales,
    } = layer;
    validate_f32_layer_scales(&id, shape, grid_to_world, &input_scales)?;
    let mut scales = Vec::with_capacity(input_scales.len());
    for scale in input_scales {
        validate_f32_scale_values(&id, &scale)?;
        let array_path = format!("arrays/intensity/{}/s{}", id, scale.level);
        let array = create_f32_array(store, &array_path, scale.shape, scale.brick_shape)?;
        array
            .store_array_subset_opt(
                &array.subset_all(),
                scale.values_tzyx.as_slice(),
                &store_all_chunks_options(),
            )
            .map_err(zarr_storage_error)?;

        let bricks = build_f32_brick_table(&array, &id, &scale)?;
        let storage = sharded_storage_metadata(
            &id,
            &array,
            array_path.as_str(),
            IntensityDType::Float32,
            scale.shape,
            scale.brick_shape,
        )?;
        let statistics = statistics_for_f32_values(&scale.values_tzyx);
        scales.push(ScaleManifest {
            level: scale.level,
            array_path,
            shape: scale.shape,
            storage,
            grid_to_world: scale.grid_to_world,
            source_scale: scale.source_scale,
            reduction: scale.reduction,
            statistics,
            validity: None,
            bricks,
        });
    }
    Ok(LayerManifest {
        id,
        kind: LayerKind::DenseIntensity,
        name,
        channel,
        shape,
        dtype: DTypeMetadata {
            source: IntensityDType::Float32,
            stored: IntensityDType::Float32,
            conversion: DTypeConversion::Lossless,
        },
        no_data_policy: None,
        grid_to_world,
        display,
        scales,
    })
}
