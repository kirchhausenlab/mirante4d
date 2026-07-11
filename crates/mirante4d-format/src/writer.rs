use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use mirante4d_core::{
    DisplayWindow, GridToWorld, IntensityDType, LayerDisplay, Shape4D, WorldSpace,
};
use zarrs::{
    array::{ArrayBuilder, ArraySubset, CodecOptions, codec::ZstdCodec, data_type},
    filesystem::FilesystemStore,
    group::GroupBuilder,
    storage::ReadableWritableListableStorage,
};

use crate::{
    manifest::{
        BOOTSTRAP_CHECKSUM_ALGORITHM, BrickIndex, BrickRecord, BrickTable, ChannelMetadata,
        DENSE_INTENSITY_ZSTD_LEVEL, DTypeConversion, DTypeMetadata, FORMAT_ID, Histogram,
        LayerKind, LayerManifest, NativeDatasetProvenance, NativeManifest, NoDataPolicy,
        PayloadChecksum, Percentiles, SCHEMA_VERSION, SHARDED_CHECKSUM_SCOPE, ScaleManifest,
        ScaleReduction, ScaleStorage, ScaleValidityMask, ShardRecord, Statistics,
        ValidityMaskEncoding, ValidityMaskRecord, WriterMetadata,
    },
    multiscale::{
        GRID_TO_WORLD_EPSILON, expected_downsampled_grid_to_world, grid_to_world_approx_eq,
        infer_downsample_factors,
    },
    validate::{FormatError, load_and_validate_dataset, write_manifest},
    zarr_io::ZarrArray,
};

mod brick_records;
mod dense;
mod models;
mod package;
mod statistics;
mod streaming;
mod validation;
mod zarr_arrays;

use brick_records::*;
use dense::*;
pub use dense::{
    default_f32_display, default_u16_display, write_native_f32_dataset,
    write_native_f32_multiscale_dataset, write_native_u16_dataset,
    write_native_u16_multiscale_dataset,
};
pub use models::*;
use package::*;
use statistics::*;
pub use streaming::{StreamingF32LayerWriter, StreamingU8LayerWriter, StreamingU16LayerWriter};
use validation::*;
use zarr_arrays::*;

pub struct NativeMultiscaleDatasetWriter {
    package_root: PathBuf,
    store: ReadableWritableListableStorage,
    dataset_id: String,
    dataset_name: String,
    world_space: WorldSpace,
    provenance: NativeDatasetProvenance,
    manifest_layers: Vec<LayerManifest>,
}

impl NativeMultiscaleDatasetWriter {
    pub fn create(
        package_root: impl AsRef<Path>,
        dataset_id: String,
        dataset_name: String,
        world_space: WorldSpace,
        existing_policy: ExistingPackagePolicy,
    ) -> Result<Self, FormatError> {
        let package_root = package_root.as_ref();
        prepare_package_root(package_root, existing_policy)?;

        let store: ReadableWritableListableStorage =
            Arc::new(FilesystemStore::new(package_root).map_err(zarr_storage_error)?);
        GroupBuilder::new()
            .build(store.clone(), "/")
            .map_err(zarr_storage_error)?
            .store_metadata()
            .map_err(zarr_storage_error)?;

        Ok(Self {
            package_root: package_root.to_path_buf(),
            store,
            dataset_id,
            dataset_name,
            world_space,
            provenance: NativeDatasetProvenance::generated_default(),
            manifest_layers: Vec::new(),
        })
    }

    pub fn set_provenance(&mut self, provenance: NativeDatasetProvenance) {
        self.provenance = provenance;
    }

    pub fn write_layer(&mut self, layer: DenseU16MultiscaleLayer) -> Result<(), FormatError> {
        let manifest_layer = write_dense_u16_multiscale_layer(&self.store, layer)?;
        self.manifest_layers.push(manifest_layer);
        Ok(())
    }

    pub fn write_f32_layer(&mut self, layer: DenseF32MultiscaleLayer) -> Result<(), FormatError> {
        let manifest_layer = write_dense_f32_multiscale_layer(&self.store, layer)?;
        self.manifest_layers.push(manifest_layer);
        Ok(())
    }

    pub fn begin_streaming_layer(
        &self,
        spec: StreamingU16LayerSpec,
    ) -> Result<StreamingU16LayerWriter, FormatError> {
        StreamingU16LayerWriter::create(self.store.clone(), spec)
    }

    pub fn begin_streaming_u8_layer(
        &self,
        spec: StreamingU8LayerSpec,
    ) -> Result<StreamingU8LayerWriter, FormatError> {
        StreamingU8LayerWriter::create(self.store.clone(), spec)
    }

    pub fn begin_streaming_f32_layer(
        &self,
        spec: StreamingF32LayerSpec,
    ) -> Result<StreamingF32LayerWriter, FormatError> {
        StreamingF32LayerWriter::create(self.store.clone(), spec)
    }

    pub fn finish_streaming_layer(
        &mut self,
        layer: StreamingU16LayerWriter,
    ) -> Result<(), FormatError> {
        self.manifest_layers.push(layer.finish()?);
        Ok(())
    }

    pub fn finish_streaming_u8_layer(
        &mut self,
        layer: StreamingU8LayerWriter,
    ) -> Result<(), FormatError> {
        self.manifest_layers.push(layer.finish()?);
        Ok(())
    }

    pub fn finish_streaming_f32_layer(
        &mut self,
        layer: StreamingF32LayerWriter,
    ) -> Result<(), FormatError> {
        self.manifest_layers.push(layer.finish()?);
        Ok(())
    }

    pub fn finish(self) -> Result<(), FormatError> {
        let manifest = NativeManifest {
            format: FORMAT_ID.to_owned(),
            schema_version: SCHEMA_VERSION,
            writer: WriterMetadata {
                name: "mirante4d".to_owned(),
                version: "0.0.0-dev".to_owned(),
            },
            dataset: crate::manifest::DatasetMetadata {
                id: self.dataset_id,
                name: self.dataset_name,
            },
            axes: ["t", "z", "y", "x"].map(str::to_owned).to_vec(),
            world_space: self.world_space,
            provenance: self.provenance,
            layers: self.manifest_layers,
        };
        write_manifest(&self.package_root, &manifest)?;
        load_and_validate_dataset(&self.package_root)?;
        Ok(())
    }
}

fn linear_tzyx(shape: Shape4D, t: u64, z: u64, y: u64, x: u64) -> usize {
    (((t * shape.z + z) * shape.y + y) * shape.x + x) as usize
}

fn zarr_storage_error(err: impl std::fmt::Display) -> FormatError {
    FormatError::ZarrStorage {
        layer_id: "<writer>".to_owned(),
        message: err.to_string(),
    }
}

#[cfg(test)]
mod tests;
