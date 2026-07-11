use std::{path::Path, sync::Arc};

use zarrs::{
    array::{Array, ArrayShardedExt},
    filesystem::FilesystemStore,
    storage::{ReadableWritableListableStorage, ReadableWritableListableStorageTraits},
};

pub type ZarrArray = Array<dyn ReadableWritableListableStorageTraits>;

pub fn open_store(
    root: &Path,
) -> Result<ReadableWritableListableStorage, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Arc::new(FilesystemStore::new(root)?))
}

pub fn open_array(
    root: &Path,
    array_path: &str,
) -> Result<ZarrArray, Box<dyn std::error::Error + Send + Sync>> {
    let store = open_store(root)?;
    let zarr_path = format!("/{array_path}");
    Ok(Array::open(store, &zarr_path)?)
}

pub fn array_shape(
    root: &Path,
    array_path: &str,
) -> Result<Vec<u64>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(open_array(root, array_path)?.shape().to_vec())
}

pub fn array_chunk_shape(
    root: &Path,
    array_path: &str,
) -> Result<Vec<u64>, Box<dyn std::error::Error + Send + Sync>> {
    let array = open_array(root, array_path)?;
    Ok(array
        .chunk_shape(&vec![0; array.dimensionality()])?
        .into_iter()
        .map(|dimension| dimension.get())
        .collect())
}

pub fn array_subchunk_shape(
    root: &Path,
    array_path: &str,
) -> Result<Option<Vec<u64>>, Box<dyn std::error::Error + Send + Sync>> {
    Ok(open_array(root, array_path)?
        .subchunk_shape()
        .map(|shape| shape.iter().map(|dimension| dimension.get()).collect()))
}

pub fn array_dimension_names(
    root: &Path,
    array_path: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let array = open_array(root, array_path)?;
    let names = array.dimension_names().clone().unwrap_or_default();
    Ok(names
        .into_iter()
        .map(|name| name.unwrap_or_default())
        .collect())
}
