use super::*;

pub(super) fn create_u16_array(
    store: &ReadableWritableListableStorage,
    array_path: &str,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> Result<ZarrArray, FormatError> {
    let shard_shape = default_shard_shape(shape, brick_shape)?;
    let zarr_array_path = format!("/{array_path}");
    let array = ArrayBuilder::new(
        shape.as_zarr_shape(),
        shard_shape.as_zarr_shape(),
        data_type::uint16(),
        0u16,
    )
    .subchunk_shape(brick_shape.as_zarr_shape())
    .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(
        DENSE_INTENSITY_ZSTD_LEVEL,
        false,
    ))])
    .dimension_names(["t", "z", "y", "x"].into())
    .build(store.clone(), &zarr_array_path)
    .map_err(zarr_storage_error)?;
    array.store_metadata().map_err(zarr_storage_error)?;
    Ok(array)
}

pub(super) fn create_u8_array(
    store: &ReadableWritableListableStorage,
    array_path: &str,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> Result<ZarrArray, FormatError> {
    let shard_shape = default_shard_shape(shape, brick_shape)?;
    let zarr_array_path = format!("/{array_path}");
    let array = ArrayBuilder::new(
        shape.as_zarr_shape(),
        shard_shape.as_zarr_shape(),
        data_type::uint8(),
        0u8,
    )
    .subchunk_shape(brick_shape.as_zarr_shape())
    .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(
        DENSE_INTENSITY_ZSTD_LEVEL,
        false,
    ))])
    .dimension_names(["t", "z", "y", "x"].into())
    .build(store.clone(), &zarr_array_path)
    .map_err(zarr_storage_error)?;
    array.store_metadata().map_err(zarr_storage_error)?;
    Ok(array)
}

pub(super) fn create_f32_array(
    store: &ReadableWritableListableStorage,
    array_path: &str,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> Result<ZarrArray, FormatError> {
    let shard_shape = default_shard_shape(shape, brick_shape)?;
    let zarr_array_path = format!("/{array_path}");
    let array = ArrayBuilder::new(
        shape.as_zarr_shape(),
        shard_shape.as_zarr_shape(),
        data_type::float32(),
        0.0f32,
    )
    .subchunk_shape(brick_shape.as_zarr_shape())
    .bytes_to_bytes_codecs(vec![Arc::new(ZstdCodec::new(
        DENSE_INTENSITY_ZSTD_LEVEL,
        false,
    ))])
    .dimension_names(["t", "z", "y", "x"].into())
    .build(store.clone(), &zarr_array_path)
    .map_err(zarr_storage_error)?;
    array.store_metadata().map_err(zarr_storage_error)?;
    Ok(array)
}

pub(super) fn store_all_chunks_options() -> CodecOptions {
    CodecOptions::default().with_store_empty_chunks(true)
}

pub(super) fn default_shard_shape(
    shape: Shape4D,
    brick_shape: Shape4D,
) -> Result<Shape4D, FormatError> {
    brick_shape.validate()?;
    if brick_shape.t != 1 {
        return Err(FormatError::ZarrStorage {
            layer_id: "storage".to_owned(),
            message: "dense shard policy requires brick_shape.t = 1".to_owned(),
        });
    }
    let grouping = if shape.z == 1 && brick_shape.z == 1 {
        Shape4D::new(1, 1, 8, 8)?
    } else {
        Shape4D::new(1, 4, 4, 4)?
    };
    let checked_axis = |axis: u64, factor: u64, name: &str| {
        axis.checked_mul(factor)
            .ok_or_else(|| FormatError::ZarrStorage {
                layer_id: "storage".to_owned(),
                message: format!("shard {name} dimension overflow"),
            })
    };
    Shape4D::new(
        brick_shape.t,
        checked_axis(brick_shape.z, grouping.z, "z")?,
        checked_axis(brick_shape.y, grouping.y, "y")?,
        checked_axis(brick_shape.x, grouping.x, "x")?,
    )
    .map_err(FormatError::InvalidShape)
}

pub(super) fn sharded_storage_metadata(
    layer_id: &str,
    array: &ZarrArray,
    array_path: &str,
    dtype: IntensityDType,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> Result<ScaleStorage, FormatError> {
    let shard_shape = default_shard_shape(shape, brick_shape)?;
    let shard_grid = shape.chunk_grid(shard_shape)?;
    let mut shard_records = Vec::with_capacity(shard_grid.element_count()? as usize);
    for t in 0..shard_grid.t {
        for z in 0..shard_grid.z {
            for y in 0..shard_grid.y {
                for x in 0..shard_grid.x {
                    let index = BrickIndex { t, z, y, x };
                    let shard_indices = [t, z, y, x];
                    let bytes = array
                        .retrieve_encoded_chunk(&shard_indices)
                        .map_err(zarr_storage_error)?
                        .ok_or_else(|| FormatError::ZarrStorage {
                            layer_id: layer_id.to_owned(),
                            message: format!("missing encoded shard {shard_indices:?}"),
                        })?;
                    shard_records.push(ShardRecord {
                        index,
                        payload_bytes: bytes.len() as u64,
                        payload_checksum: PayloadChecksum {
                            algorithm: BOOTSTRAP_CHECKSUM_ALGORITHM.to_owned(),
                            scope: SHARDED_CHECKSUM_SCOPE.to_owned(),
                            hex: blake3::hash(&bytes).to_hex().to_string(),
                        },
                    });
                }
            }
        }
    }
    ScaleStorage::sharded(
        array_path.to_owned(),
        dtype,
        shape,
        brick_shape,
        shard_shape,
        shard_records,
    )
    .map_err(FormatError::InvalidShape)
}
