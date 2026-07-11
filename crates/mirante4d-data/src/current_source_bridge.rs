//! Temporary current-format source bridge for the unified dataset runtime.
//!
//! This is the only product-facing translation from the current
//! `mirante4d-v1` package into the storage-independent dataset contract. It is
//! deleted with `mirante4d-data` at WP-10C.

#![allow(
    clippy::result_large_err,
    reason = "the frozen DatasetSource contract requires the context-rich typed DatasetSourceFault"
)]

use std::{error::Error as StdError, fs, path::Path, sync::Arc};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetCatalogError, DatasetLayer, DatasetResourceKey, DatasetScale, DatasetSource,
    DatasetSourceFault, DatasetSourceId, DecodeSinkError, ReservedDecodeSink,
    ResourceContractError, ResourceValidity, ScientificIdentityStatus,
};
use mirante4d_domain::{IntensityDType, LogicalLayerKey, ScaleLevel};
use thiserror::Error;
use zarrs::{
    array::{ArrayError, ArrayShardedReadableExt, ArrayShardedReadableExtCache, CodecOptions},
    storage::StorageError,
};

use crate::{DataError, DatasetHandle};

const CURRENT_METADATA_MIN_BYTES: u64 = 64 * 1024;
const CURRENT_METADATA_ENCODED_MULTIPLIER: u64 = 8;
const SOURCE_DECODE_TILE_BYTES: u64 = 64 * 1024;
const SINK_WRITE_CHUNK_BYTES: usize = 8 * 1024;

/// Opening failure for the temporary current-format source bridge.
#[derive(Debug, Error)]
pub enum CurrentDatasetSourceOpenError {
    #[error("current dataset manifest metadata is unavailable")]
    ManifestMetadata(#[source] std::io::Error),
    #[error("current dataset metadata accounting overflows")]
    MetadataAccountingOverflow,
    #[error("current dataset metadata admission failed: {0}")]
    MetadataAdmission(#[source] CpuLedgerError),
    #[error(transparent)]
    Dataset(#[from] DataError),
    #[error(transparent)]
    Catalog(#[from] DatasetCatalogError),
    #[error("current dataset has more logical layers than the catalog key can represent")]
    LayerOrdinalOverflow,
    #[error("the CPU byte ledger returned an invalid metadata lease")]
    InvalidMetadataLease,
}

/// One immutable bridge from the current package reader to `DatasetSource`.
///
/// The handle, manifest, catalog, and logical-to-physical mapping are shared;
/// worker threads receive an `Arc` to this source rather than cloning package
/// metadata. The injected ledger is owned by `mirante4d-dataset-runtime`.
pub struct CurrentDatasetSource {
    dataset: DatasetHandle,
    catalog: Arc<DatasetCatalog>,
    ledger: Arc<dyn CpuByteLedger>,
    _metadata_lease: Box<dyn CpuByteLease>,
}

impl CurrentDatasetSource {
    /// Opens one current package and binds all retained metadata to the
    /// runtime-owned CPU ledger.
    pub fn open(
        root: impl AsRef<Path>,
        source_id: DatasetSourceId,
        ledger: Arc<dyn CpuByteLedger>,
    ) -> Result<Arc<Self>, CurrentDatasetSourceOpenError> {
        let root = root.as_ref();
        let manifest_bytes = fs::metadata(root.join("mirante4d.json"))
            .map_err(CurrentDatasetSourceOpenError::ManifestMetadata)?
            .len();
        let metadata_bytes = manifest_bytes
            .checked_mul(CURRENT_METADATA_ENCODED_MULTIPLIER)
            .ok_or(CurrentDatasetSourceOpenError::MetadataAccountingOverflow)?
            .max(CURRENT_METADATA_MIN_BYTES);
        let metadata_lease = ledger
            .try_acquire(CpuLedgerCategory::MetadataAndIndexes, metadata_bytes)
            .map_err(CurrentDatasetSourceOpenError::MetadataAdmission)?;
        if metadata_lease.category() != CpuLedgerCategory::MetadataAndIndexes
            || metadata_lease.reserved_bytes() != metadata_bytes
        {
            return Err(CurrentDatasetSourceOpenError::InvalidMetadataLease);
        }

        let dataset = DatasetHandle::open(root)?;
        let catalog = Arc::new(build_catalog(&dataset, source_id)?);
        Ok(Arc::new(Self {
            dataset,
            catalog,
            ledger,
            _metadata_lease: metadata_lease,
        }))
    }

    fn checkpoint(
        &self,
        sink: &dyn ReservedDecodeSink,
        key: DatasetResourceKey,
    ) -> Result<(), DatasetSourceFault> {
        if sink.is_cancelled() {
            Err(DatasetSourceFault::Cancelled { key })
        } else {
            Ok(())
        }
    }

    fn acquire_decode_scratch(
        &self,
        key: DatasetResourceKey,
        bytes: u64,
    ) -> Result<Box<dyn CpuByteLease>, DatasetSourceFault> {
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::InFlightDecode, bytes)
            .map_err(|error| {
                map_ledger_error(key, CpuLedgerCategory::InFlightDecode, bytes, error)
            })?;
        if lease.category() != CpuLedgerCategory::InFlightDecode || lease.reserved_bytes() != bytes
        {
            return Err(DatasetSourceFault::DecodeFailed { key });
        }
        Ok(lease)
    }

    fn current_scale(
        &self,
        key: DatasetResourceKey,
    ) -> Result<&mirante4d_format::ScaleManifest, DatasetSourceFault> {
        let layer_index = usize::try_from(key.layer().ordinal()).map_err(|_| {
            invalid_resource(
                key,
                ResourceContractError::UnknownLayer {
                    ordinal: key.layer().ordinal(),
                },
            )
        })?;
        let layer = self
            .dataset
            .manifest()
            .layers
            .get(layer_index)
            .ok_or_else(|| {
                invalid_resource(
                    key,
                    ResourceContractError::UnknownLayer {
                        ordinal: key.layer().ordinal(),
                    },
                )
            })?;
        layer
            .scales
            .iter()
            .find(|scale| scale.level == key.scale().get())
            .ok_or_else(|| {
                invalid_resource(
                    key,
                    ResourceContractError::UnknownScale {
                        level: key.scale().get(),
                    },
                )
            })
    }

    fn decode_values(
        &self,
        sink: &mut dyn ReservedDecodeSink,
        key: DatasetResourceKey,
        scale: &mirante4d_format::ScaleManifest,
        dtype: IntensityDType,
    ) -> Result<(), DatasetSourceFault> {
        self.checkpoint(sink, key)?;
        let array = mirante4d_format::zarr_io::open_array(self.dataset.root(), &scale.array_path)
            .map_err(|error| map_open_error(key, error.as_ref()))?;

        visit_decode_tiles(key, dtype.bytes_per_sample(), |tile| {
            self.checkpoint(sink, key)?;
            let returned_bytes = tile
                .sample_count()
                .checked_mul(u64::from(dtype.bytes_per_sample()))
                .ok_or(DatasetSourceFault::DecodeFailed { key })?;
            let scratch_bytes = source_decode_scratch_bytes(
                key,
                &scale.storage,
                returned_bytes,
                dtype.bytes_per_sample(),
            )?;
            let _scratch = self.acquire_decode_scratch(key, scratch_bytes)?;
            let subset = tile.subset(key);
            let cache = ArrayShardedReadableExtCache::new(&array);
            let options = bounded_codec_options();
            match dtype {
                IntensityDType::Uint8 => {
                    let values = array
                        .retrieve_array_subset_sharded_opt::<Vec<u8>>(&cache, &subset, &options)
                        .map_err(|error| map_array_error(key, &error))?;
                    ensure_sample_count(key, tile.sample_count(), values.len())?;
                    self.checkpoint(sink, key)?;
                    write_sink_bytes(sink, key, &values)?;
                }
                IntensityDType::Uint16 => {
                    let values = array
                        .retrieve_array_subset_sharded_opt::<Vec<u16>>(&cache, &subset, &options)
                        .map_err(|error| map_array_error(key, &error))?;
                    ensure_sample_count(key, tile.sample_count(), values.len())?;
                    self.checkpoint(sink, key)?;
                    write_u16_le(sink, key, &values)?;
                }
                IntensityDType::Float32 => {
                    let values = array
                        .retrieve_array_subset_sharded_opt::<Vec<f32>>(&cache, &subset, &options)
                        .map_err(|error| map_array_error(key, &error))?;
                    ensure_sample_count(key, tile.sample_count(), values.len())?;
                    self.checkpoint(sink, key)?;
                    write_f32_le(sink, key, &values)?;
                }
            }
            self.checkpoint(sink, key)
        })
    }

    fn decode_validity(
        &self,
        sink: &mut dyn ReservedDecodeSink,
        key: DatasetResourceKey,
        scale: &mirante4d_format::ScaleManifest,
    ) -> Result<(), DatasetSourceFault> {
        let validity = scale
            .validity
            .as_ref()
            .ok_or(DatasetSourceFault::CorruptResource { key })?;
        self.checkpoint(sink, key)?;
        let array =
            mirante4d_format::zarr_io::open_array(self.dataset.root(), &validity.array_path)
                .map_err(|error| map_open_error(key, error.as_ref()))?;
        let mut packer = ValidityBitPacker::default();

        visit_decode_tiles(key, 1, |tile| {
            self.checkpoint(sink, key)?;
            let scratch_bytes =
                source_decode_scratch_bytes(key, &validity.storage, tile.sample_count(), 1)?;
            let _scratch = self.acquire_decode_scratch(key, scratch_bytes)?;
            let subset = tile.subset(key);
            let cache = ArrayShardedReadableExtCache::new(&array);
            let options = bounded_codec_options();
            let values = array
                .retrieve_array_subset_sharded_opt::<Vec<u8>>(&cache, &subset, &options)
                .map_err(|error| map_array_error(key, &error))?;
            ensure_sample_count(key, tile.sample_count(), values.len())?;
            self.checkpoint(sink, key)?;
            for value in values {
                match value {
                    0 => packer.push(false, sink, key)?,
                    1 => packer.push(true, sink, key)?,
                    _ => return Err(DatasetSourceFault::CorruptResource { key }),
                }
            }
            self.checkpoint(sink, key)
        })?;
        packer.finish(sink, key)
    }
}

impl DatasetSource for CurrentDatasetSource {
    fn catalog(&self) -> Result<Arc<DatasetCatalog>, DatasetSourceFault> {
        Ok(Arc::clone(&self.catalog))
    }

    fn decode_into(&self, sink: &mut dyn ReservedDecodeSink) -> Result<(), DatasetSourceFault> {
        let key = sink.resource_key();
        self.checkpoint(sink, key)?;
        let descriptor = self
            .catalog
            .validate_decode_reservation(sink)
            .map_err(|reason| invalid_resource(key, reason))?;
        self.checkpoint(sink, key)?;
        let scale = self.current_scale(key)?;
        self.decode_values(sink, key, scale, descriptor.dtype())?;
        if descriptor.validity() == ResourceValidity::BitMask {
            self.decode_validity(sink, key, scale)?;
        }
        self.checkpoint(sink, key)?;
        sink.finish().map_err(|reason| map_sink_error(key, reason))
    }
}

fn build_catalog(
    dataset: &DatasetHandle,
    source_id: DatasetSourceId,
) -> Result<DatasetCatalog, CurrentDatasetSourceOpenError> {
    let mut layers = Vec::with_capacity(dataset.manifest().layers.len());
    for (index, layer) in dataset.manifest().layers.iter().enumerate() {
        let ordinal = u32::try_from(index)
            .map_err(|_| CurrentDatasetSourceOpenError::LayerOrdinalOverflow)?;
        let scales = layer
            .scales
            .iter()
            .map(|scale| {
                DatasetScale::new(
                    ScaleLevel::new(scale.level),
                    scale.shape.spatial(),
                    scale.grid_to_world,
                    if scale.validity.is_some() {
                        ResourceValidity::BitMask
                    } else {
                        ResourceValidity::AllValid
                    },
                )
            })
            .collect();
        layers.push(DatasetLayer::new_multiscale(
            LogicalLayerKey::new(ordinal),
            &layer.name,
            layer.shape.t(),
            layer.dtype.stored,
            scales,
        )?);
    }
    Ok(DatasetCatalog::new(
        dataset.dataset_name(),
        ScientificIdentityStatus::Unverified(source_id),
        layers,
    )?)
}

#[derive(Debug, Clone, Copy)]
struct DecodeTile {
    origin: [u64; 3],
    shape: [u64; 3],
}

impl DecodeTile {
    fn sample_count(self) -> u64 {
        self.shape[0]
            .checked_mul(self.shape[1])
            .and_then(|count| count.checked_mul(self.shape[2]))
            .expect("decode tiles are subsets of a validated payload descriptor")
    }

    fn subset(self, key: DatasetResourceKey) -> [std::ops::Range<u64>; 4] {
        let t = key.timepoint().get();
        [
            t..t + 1,
            self.origin[0]..self.origin[0] + self.shape[0],
            self.origin[1]..self.origin[1] + self.shape[1],
            self.origin[2]..self.origin[2] + self.shape[2],
        ]
    }
}

fn visit_decode_tiles(
    key: DatasetResourceKey,
    bytes_per_sample: u8,
    mut visit: impl FnMut(DecodeTile) -> Result<(), DatasetSourceFault>,
) -> Result<(), DatasetSourceFault> {
    let origin = key.region().origin();
    let [z_len, y_len, x_len] = key.region().shape().dimensions();
    let max_samples = (SOURCE_DECODE_TILE_BYTES / u64::from(bytes_per_sample)).max(1);
    let plane_samples = y_len
        .checked_mul(x_len)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;

    if plane_samples <= max_samples {
        let z_step = (max_samples / plane_samples).max(1);
        let mut z = 0;
        while z < z_len {
            let count = (z_len - z).min(z_step);
            visit(DecodeTile {
                origin: [origin[0] + z, origin[1], origin[2]],
                shape: [count, y_len, x_len],
            })?;
            z += count;
        }
    } else if x_len <= max_samples {
        let y_step = (max_samples / x_len).max(1);
        for z in 0..z_len {
            let mut y = 0;
            while y < y_len {
                let count = (y_len - y).min(y_step);
                visit(DecodeTile {
                    origin: [origin[0] + z, origin[1] + y, origin[2]],
                    shape: [1, count, x_len],
                })?;
                y += count;
            }
        }
    } else {
        for z in 0..z_len {
            for y in 0..y_len {
                let mut x = 0;
                while x < x_len {
                    let count = (x_len - x).min(max_samples);
                    visit(DecodeTile {
                        origin: [origin[0] + z, origin[1] + y, origin[2] + x],
                        shape: [1, 1, count],
                    })?;
                    x += count;
                }
            }
        }
    }
    Ok(())
}

fn ensure_sample_count(
    key: DatasetResourceKey,
    expected: u64,
    actual: usize,
) -> Result<(), DatasetSourceFault> {
    if u64::try_from(actual).ok() == Some(expected) {
        Ok(())
    } else {
        Err(DatasetSourceFault::CorruptResource { key })
    }
}

fn bounded_codec_options() -> CodecOptions {
    // A single source worker already occupies one runtime worker slot. Keeping
    // Zarr chunk/shard decode serial makes the codec working-set bound below
    // honest instead of multiplying it by Rayon availability.
    CodecOptions::default()
        .with_concurrent_target(1)
        .with_chunk_concurrent_minimum(1)
}

fn source_decode_scratch_bytes(
    key: DatasetResourceKey,
    storage: &mirante4d_format::ScaleStorage,
    returned_bytes: u64,
    bytes_per_sample: u8,
) -> Result<u64, DatasetSourceFault> {
    // zarrs' sharded subset path owns the final subset, a per-shard overlap,
    // and a typed conversion while an inner codec may simultaneously retain
    // one encoded subchunk and one fully decoded logical chunk. Current schema
    // 1 records only whole-shard encoded bytes, so using the largest shard is
    // deliberately conservative. The cache is per tile and concurrency is 1.
    let returned_working_set = returned_bytes
        .checked_mul(3)
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    let decoded_chunk_bytes = storage
        .brick_shape
        .element_count()
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?
        .checked_mul(u64::from(bytes_per_sample))
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    let encoded_chunk_upper_bound = storage
        .shard_records
        .iter()
        .map(|record| record.payload_bytes)
        .max()
        .ok_or(DatasetSourceFault::CorruptResource { key })?;
    let shard_index_bytes = storage
        .chunks_per_shard
        .element_count()
        .map_err(|_| DatasetSourceFault::DecodeFailed { key })?
        .checked_mul(2 * u64::try_from(std::mem::size_of::<u64>()).unwrap())
        .and_then(|bytes| bytes.checked_add(2 * u64::try_from(std::mem::size_of::<u64>()).unwrap()))
        .ok_or(DatasetSourceFault::DecodeFailed { key })?;
    returned_working_set
        .checked_add(decoded_chunk_bytes)
        .and_then(|bytes| bytes.checked_add(encoded_chunk_upper_bound))
        .and_then(|bytes| bytes.checked_add(shard_index_bytes))
        .ok_or(DatasetSourceFault::DecodeFailed { key })
}

fn write_u16_le(
    sink: &mut dyn ReservedDecodeSink,
    key: DatasetResourceKey,
    values: &[u16],
) -> Result<(), DatasetSourceFault> {
    let mut output = [0_u8; SINK_WRITE_CHUNK_BYTES];
    let mut used = 0;
    for value in values {
        let bytes = value.to_le_bytes();
        output[used..used + 2].copy_from_slice(&bytes);
        used += 2;
        if used == output.len() {
            write_sink_bytes(sink, key, &output)?;
            used = 0;
        }
    }
    if used != 0 {
        write_sink_bytes(sink, key, &output[..used])?;
    }
    Ok(())
}

fn write_f32_le(
    sink: &mut dyn ReservedDecodeSink,
    key: DatasetResourceKey,
    values: &[f32],
) -> Result<(), DatasetSourceFault> {
    let mut output = [0_u8; SINK_WRITE_CHUNK_BYTES];
    let mut used = 0;
    for value in values {
        if !value.is_finite() {
            return Err(DatasetSourceFault::CorruptResource { key });
        }
        let bytes = value.to_bits().to_le_bytes();
        output[used..used + 4].copy_from_slice(&bytes);
        used += 4;
        if used == output.len() {
            write_sink_bytes(sink, key, &output)?;
            used = 0;
        }
    }
    if used != 0 {
        write_sink_bytes(sink, key, &output[..used])?;
    }
    Ok(())
}

fn write_sink_bytes(
    sink: &mut dyn ReservedDecodeSink,
    key: DatasetResourceKey,
    bytes: &[u8],
) -> Result<(), DatasetSourceFault> {
    for chunk in bytes.chunks(SINK_WRITE_CHUNK_BYTES) {
        if sink.is_cancelled() {
            return Err(DatasetSourceFault::Cancelled { key });
        }
        sink.write(chunk)
            .map_err(|reason| map_sink_error(key, reason))?;
        if sink.is_cancelled() {
            return Err(DatasetSourceFault::Cancelled { key });
        }
    }
    Ok(())
}

struct ValidityBitPacker {
    output: [u8; SINK_WRITE_CHUNK_BYTES],
    output_len: usize,
    current: u8,
    current_bits: u8,
}

impl Default for ValidityBitPacker {
    fn default() -> Self {
        Self {
            output: [0; SINK_WRITE_CHUNK_BYTES],
            output_len: 0,
            current: 0,
            current_bits: 0,
        }
    }
}

impl ValidityBitPacker {
    fn push(
        &mut self,
        valid: bool,
        sink: &mut dyn ReservedDecodeSink,
        key: DatasetResourceKey,
    ) -> Result<(), DatasetSourceFault> {
        if valid {
            self.current |= 1 << self.current_bits;
        }
        self.current_bits += 1;
        if self.current_bits == 8 {
            self.push_byte(self.current, sink, key)?;
            self.current = 0;
            self.current_bits = 0;
        }
        Ok(())
    }

    fn push_byte(
        &mut self,
        byte: u8,
        sink: &mut dyn ReservedDecodeSink,
        key: DatasetResourceKey,
    ) -> Result<(), DatasetSourceFault> {
        self.output[self.output_len] = byte;
        self.output_len += 1;
        if self.output_len == self.output.len() {
            write_sink_bytes(sink, key, &self.output)?;
            self.output_len = 0;
        }
        Ok(())
    }

    fn finish(
        mut self,
        sink: &mut dyn ReservedDecodeSink,
        key: DatasetResourceKey,
    ) -> Result<(), DatasetSourceFault> {
        if self.current_bits != 0 {
            self.push_byte(self.current, sink, key)?;
        }
        if self.output_len != 0 {
            write_sink_bytes(sink, key, &self.output[..self.output_len])?;
        }
        Ok(())
    }
}

fn invalid_resource(key: DatasetResourceKey, reason: ResourceContractError) -> DatasetSourceFault {
    DatasetSourceFault::InvalidResource {
        key,
        reason: Box::new(reason),
    }
}

fn map_ledger_error(
    key: DatasetResourceKey,
    category: CpuLedgerCategory,
    requested_bytes: u64,
    error: CpuLedgerError,
) -> DatasetSourceFault {
    match error {
        CpuLedgerError::CapacityExceeded {
            category,
            requested_bytes,
            available_bytes,
        } => DatasetSourceFault::CapacityExceeded {
            key,
            category,
            requested_bytes,
            available_bytes,
        },
        CpuLedgerError::ShuttingDown => DatasetSourceFault::ShuttingDown {
            key,
            category,
            requested_bytes,
        },
        CpuLedgerError::ZeroByteReservation => DatasetSourceFault::DecodeFailed { key },
    }
}

fn map_sink_error(key: DatasetResourceKey, reason: DecodeSinkError) -> DatasetSourceFault {
    match reason {
        DecodeSinkError::Cancelled => DatasetSourceFault::Cancelled { key },
        reason => DatasetSourceFault::SinkRejected {
            key,
            reason: Box::new(reason),
        },
    }
}

fn map_open_error(key: DatasetResourceKey, error: &(dyn StdError + 'static)) -> DatasetSourceFault {
    if error_chain_has_not_found(error) {
        DatasetSourceFault::ResourceUnavailable { key }
    } else {
        DatasetSourceFault::DecodeFailed { key }
    }
}

fn error_chain_has_not_found(mut error: &(dyn StdError + 'static)) -> bool {
    loop {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return true;
        }
        let Some(source) = error.source() else {
            return false;
        };
        error = source;
    }
}

fn map_array_error(key: DatasetResourceKey, error: &ArrayError) -> DatasetSourceFault {
    match error {
        ArrayError::StorageError(StorageError::IOError(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            DatasetSourceFault::ResourceUnavailable { key }
        }
        ArrayError::StorageError(_) => DatasetSourceFault::ResourceUnavailable { key },
        ArrayError::CodecError(_)
        | ArrayError::UnexpectedChunkDecodedSize(_, _)
        | ArrayError::InvalidBytesInputSize(_, _)
        | ArrayError::UnexpectedChunkDecodedShape(_, _)
        | ArrayError::ElementError(_)
        | ArrayError::InvalidDataShape(_, _) => DatasetSourceFault::CorruptResource { key },
        ArrayError::UnsupportedMethod(_) => DatasetSourceFault::UnsupportedResource { key },
        _ => DatasetSourceFault::DecodeFailed { key },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use mirante4d_dataset::{DatasetResourceIdentity, ResourcePayloadDescriptor, ResourceRegion};
    use mirante4d_domain::{DisplayWindow, GridToWorld, Shape3D, Shape4D, TimeIndex};
    use mirante4d_format::{
        ChannelMetadata, CurrentGridToWorldExt, ExistingPackagePolicy, FixtureKind, LayerDisplay,
        NativeMultiscaleDatasetWriter, NoDataPolicy, NoDataPolicyKind, NoDataVisibilityPolicy,
        ScaleReduction, StreamingU8LayerSpec, StreamingU8ScaleSpec, WorldSpace, WorldUnit,
        expected_f32_fixture_value, expected_fixture_value, write_fixture,
    };

    use super::*;

    const TEST_SOURCE_ID: DatasetSourceId = DatasetSourceId::new(91);

    #[derive(Debug)]
    struct TestLedgerState {
        caps: [u64; 7],
        used: Mutex<[u64; 7]>,
        maximum_used: Mutex<[u64; 7]>,
        shutting_down: AtomicBool,
    }

    #[derive(Debug, Clone)]
    struct TestLedger {
        state: Arc<TestLedgerState>,
    }

    #[derive(Debug)]
    struct TestLease {
        state: Arc<TestLedgerState>,
        category: CpuLedgerCategory,
        bytes: u64,
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            let mut used = self.state.used.lock().unwrap();
            used[test_category_index(self.category)] -= self.bytes;
        }
    }

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl CpuByteLedger for TestLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            if self.state.shutting_down.load(Ordering::SeqCst) {
                return Err(CpuLedgerError::ShuttingDown);
            }
            let index = test_category_index(category);
            let mut used = self.state.used.lock().unwrap();
            let available = self.state.caps[index].saturating_sub(used[index]);
            if bytes > available {
                return Err(CpuLedgerError::CapacityExceeded {
                    category,
                    requested_bytes: bytes,
                    available_bytes: available,
                });
            }
            used[index] += bytes;
            let current = used[index];
            drop(used);
            let mut maximum = self.state.maximum_used.lock().unwrap();
            maximum[index] = maximum[index].max(current);
            Ok(Box::new(TestLease {
                state: Arc::clone(&self.state),
                category,
                bytes,
            }))
        }
    }

    impl TestLedger {
        fn new(metadata_cap: u64, decode_cap: u64) -> Arc<Self> {
            let mut caps = [16 * 1024 * 1024; 7];
            caps[test_category_index(CpuLedgerCategory::MetadataAndIndexes)] = metadata_cap;
            caps[test_category_index(CpuLedgerCategory::InFlightDecode)] = decode_cap;
            Arc::new(Self {
                state: Arc::new(TestLedgerState {
                    caps,
                    used: Mutex::new([0; 7]),
                    maximum_used: Mutex::new([0; 7]),
                    shutting_down: AtomicBool::new(false),
                }),
            })
        }

        fn used(&self, category: CpuLedgerCategory) -> u64 {
            self.state.used.lock().unwrap()[test_category_index(category)]
        }

        fn maximum_used(&self, category: CpuLedgerCategory) -> u64 {
            self.state.maximum_used.lock().unwrap()[test_category_index(category)]
        }
    }

    const fn test_category_index(category: CpuLedgerCategory) -> usize {
        match category {
            CpuLedgerCategory::DecodedResidency => 0,
            CpuLedgerCategory::UploadStaging => 1,
            CpuLedgerCategory::InFlightDecode => 2,
            CpuLedgerCategory::MetadataAndIndexes => 3,
            CpuLedgerCategory::QueuesAndResults => 4,
            CpuLedgerCategory::Prefetch => 5,
            CpuLedgerCategory::ImportWorkingSet => 6,
        }
    }

    struct TestSink {
        key: DatasetResourceKey,
        descriptor: ResourcePayloadDescriptor,
        bytes: Vec<u8>,
        finished: bool,
        cancelled: Arc<AtomicBool>,
        cancel_after_writes: Option<usize>,
        writes: usize,
    }

    impl TestSink {
        fn new(key: DatasetResourceKey, descriptor: ResourcePayloadDescriptor) -> Self {
            Self {
                key,
                descriptor,
                bytes: Vec::new(),
                finished: false,
                cancelled: Arc::new(AtomicBool::new(false)),
                cancel_after_writes: None,
                writes: 0,
            }
        }

        fn cancelled(key: DatasetResourceKey, descriptor: ResourcePayloadDescriptor) -> Self {
            let sink = Self::new(key, descriptor);
            sink.cancelled.store(true, Ordering::SeqCst);
            sink
        }
    }

    impl ReservedDecodeSink for TestSink {
        fn resource_key(&self) -> DatasetResourceKey {
            self.key
        }

        fn payload_descriptor(&self) -> ResourcePayloadDescriptor {
            self.descriptor
        }

        fn written_bytes(&self) -> u64 {
            self.bytes.len() as u64
        }

        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }

        fn write(&mut self, bytes: &[u8]) -> Result<(), DecodeSinkError> {
            if self.is_cancelled() {
                return Err(DecodeSinkError::Cancelled);
            }
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            let attempted = self
                .written_bytes()
                .checked_add(bytes.len() as u64)
                .ok_or(DecodeSinkError::ByteCountOverflow)?;
            if attempted > self.reserved_bytes() {
                return Err(DecodeSinkError::ReservationExceeded {
                    reserved: self.reserved_bytes(),
                    attempted,
                });
            }
            self.bytes.extend_from_slice(bytes);
            self.writes += 1;
            if self
                .cancel_after_writes
                .is_some_and(|maximum| self.writes >= maximum)
            {
                self.cancelled.store(true, Ordering::SeqCst);
            }
            Ok(())
        }

        fn finish(&mut self) -> Result<(), DecodeSinkError> {
            if self.is_cancelled() {
                return Err(DecodeSinkError::Cancelled);
            }
            if self.finished {
                return Err(DecodeSinkError::AlreadyFinished);
            }
            if self.written_bytes() != self.reserved_bytes() {
                return Err(DecodeSinkError::Incomplete {
                    reserved: self.reserved_bytes(),
                    written: self.written_bytes(),
                });
            }
            self.finished = true;
            Ok(())
        }

        fn is_finished(&self) -> bool {
            self.finished
        }
    }

    fn ledger_trait(ledger: &Arc<TestLedger>) -> Arc<dyn CpuByteLedger> {
        ledger.clone()
    }

    fn open_source(root: &Path, ledger: &Arc<TestLedger>) -> Arc<CurrentDatasetSource> {
        CurrentDatasetSource::open(root, TEST_SOURCE_ID, ledger_trait(ledger)).unwrap()
    }

    fn key(
        layer: u32,
        timepoint: u64,
        scale: u32,
        origin: [u64; 3],
        shape: Shape3D,
    ) -> DatasetResourceKey {
        DatasetResourceKey::new(
            DatasetResourceIdentity::Unverified(TEST_SOURCE_ID),
            LogicalLayerKey::new(layer),
            TimeIndex::new(timepoint),
            ScaleLevel::new(scale),
            ResourceRegion::new(origin, shape).unwrap(),
        )
    }

    fn write_validity_multiscale_package(root: &Path) {
        let s0_shape = Shape4D::new(1, 1, 1, 10).unwrap();
        let s1_shape = Shape4D::new(1, 1, 1, 5).unwrap();
        let s0_grid = GridToWorld::identity();
        let s1_grid = s0_grid.downsampled_integer_centered(2, 1, 1).unwrap();
        let mut writer = NativeMultiscaleDatasetWriter::create(
            root,
            "bridge-validity".to_owned(),
            "Bridge validity".to_owned(),
            WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            ExistingPackagePolicy::Fail,
        )
        .unwrap();
        let mut layer = writer
            .begin_streaming_u8_layer(StreamingU8LayerSpec {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape: s0_shape,
                no_data_policy: Some(NoDataPolicy {
                    kind: NoDataPolicyKind::SentinelValue,
                    source_value: 255.0,
                    source_dtype: IntensityDType::Uint8,
                    visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
                }),
                grid_to_world: s0_grid,
                display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0)
                    .unwrap(),
                scales: vec![
                    StreamingU8ScaleSpec {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: s0_shape,
                        grid_to_world: s0_grid,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                    },
                    StreamingU8ScaleSpec {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: s1_shape,
                        grid_to_world: s1_grid,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                    },
                ],
            })
            .unwrap();
        layer
            .write_timepoint_with_render_valid(
                0,
                0,
                &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
                &[1, 0, 1, 0, 0, 0, 0, 1, 1, 0],
            )
            .unwrap();
        layer
            .write_timepoint_with_render_valid(1, 0, &[0, 2, 4, 6, 8], &[1, 1, 0, 1, 0])
            .unwrap();
        writer.finish_streaming_u8_layer(layer).unwrap();
        writer.finish().unwrap();
    }

    fn write_large_inner_chunk_package(root: &Path) {
        let x = SOURCE_DECODE_TILE_BYTES + 1;
        let shape = Shape4D::new(1, 1, 1, x).unwrap();
        let grid = GridToWorld::identity();
        let mut writer = NativeMultiscaleDatasetWriter::create(
            root,
            "bridge-large-chunk".to_owned(),
            "Bridge large chunk".to_owned(),
            WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            ExistingPackagePolicy::Fail,
        )
        .unwrap();
        let mut layer = writer
            .begin_streaming_u8_layer(StreamingU8LayerSpec {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                no_data_policy: None,
                grid_to_world: grid,
                display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0)
                    .unwrap(),
                scales: vec![StreamingU8ScaleSpec {
                    level: 0,
                    shape,
                    brick_shape: shape,
                    grid_to_world: grid,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                }],
            })
            .unwrap();
        let values = (0..x).map(|index| index as u8).collect::<Vec<_>>();
        layer.write_timepoint(0, 0, &values).unwrap();
        writer.finish_streaming_u8_layer(layer).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn catalog_contains_every_scale_and_is_shared_without_manifest_clones() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("validity-multiscale.m4d");
        write_validity_multiscale_package(&root);
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = open_source(&root, &ledger);

        let first = source.catalog().unwrap();
        let second = source.catalog().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            first.scientific_identity().source_id(),
            Some(TEST_SOURCE_ID)
        );
        let layer = first.layer(LogicalLayerKey::new(0)).unwrap();
        assert_eq!(layer.scales().len(), 2);
        assert_eq!(layer.scale(ScaleLevel::BASE).unwrap().shape().x(), 10);
        assert_eq!(layer.scale(ScaleLevel::new(1)).unwrap().shape().x(), 5);
        assert_eq!(
            layer.validity(ScaleLevel::BASE),
            Some(ResourceValidity::BitMask)
        );
        assert_eq!(
            layer.validity(ScaleLevel::new(1)),
            Some(ResourceValidity::BitMask)
        );
        assert!(ledger.used(CpuLedgerCategory::MetadataAndIndexes) >= CURRENT_METADATA_MIN_BYTES);

        let cloned_handle = source.dataset.clone();
        assert!(Arc::ptr_eq(
            &source.dataset.manifest,
            &cloned_handle.manifest
        ));
    }

    #[test]
    fn decode_writes_values_then_canonical_lsb_first_validity_without_mutating_source() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("validity-decode.m4d");
        write_validity_multiscale_package(&root);
        let before = fs::read(root.join("mirante4d.json")).unwrap();
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = open_source(&root, &ledger);
        let key = key(0, 0, 0, [0, 0, 0], Shape3D::new(1, 1, 10).unwrap());
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = TestSink::new(key, descriptor);

        source.decode_into(&mut sink).unwrap();

        assert!(sink.finished);
        assert_eq!(&sink.bytes[..10], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(&sink.bytes[10..], &[0b1000_0101, 0b0000_0001]);
        let view = descriptor
            .view(&sink.bytes[..10], Some(&sink.bytes[10..]))
            .unwrap();
        assert_eq!(view.sample_is_valid(0), Ok(true));
        assert_eq!(view.value_bytes()[0], 0, "valid zero remains data");
        assert_eq!(view.sample_is_valid(1), Ok(false));
        assert_eq!(view.sample_is_valid(7), Ok(true));
        assert_eq!(view.sample_is_valid(8), Ok(true));
        assert_eq!(view.sample_is_valid(9), Ok(false));
        assert_eq!(fs::read(root.join("mirante4d.json")).unwrap(), before);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
        assert!(ledger.maximum_used(CpuLedgerCategory::InFlightDecode) > 0);
    }

    #[test]
    fn decode_emits_canonical_little_endian_u16_and_finite_f32() {
        let tempdir = tempfile::tempdir().unwrap();
        let u16_root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let f32_root = write_fixture(FixtureKind::BasicF32_8Cube, tempdir.path()).unwrap();
        let ledger = TestLedger::new(16 * 1024 * 1024, 8 * 1024 * 1024);

        let u16_source = open_source(&u16_root, &ledger);
        let u16_key = key(0, 0, 0, [1, 2, 3], Shape3D::new(1, 1, 2).unwrap());
        let u16_descriptor = u16_source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(u16_key)
            .unwrap();
        let mut u16_sink = TestSink::new(u16_key, u16_descriptor);
        u16_source.decode_into(&mut u16_sink).unwrap();
        assert_eq!(
            u16::from_le_bytes(u16_sink.bytes[0..2].try_into().unwrap()),
            expected_fixture_value(0, 1, 2, 3)
        );
        assert_eq!(
            u16::from_le_bytes(u16_sink.bytes[2..4].try_into().unwrap()),
            expected_fixture_value(0, 1, 2, 4)
        );

        let f32_source = open_source(&f32_root, &ledger);
        let f32_key = key(0, 0, 0, [1, 2, 3], Shape3D::new(1, 1, 2).unwrap());
        let f32_descriptor = f32_source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(f32_key)
            .unwrap();
        let mut f32_sink = TestSink::new(f32_key, f32_descriptor);
        f32_source.decode_into(&mut f32_sink).unwrap();
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(f32_sink.bytes[0..4].try_into().unwrap())),
            expected_f32_fixture_value(0, 1, 2, 3)
        );
        assert_eq!(
            f32::from_bits(u32::from_le_bytes(f32_sink.bytes[4..8].try_into().unwrap())),
            expected_f32_fixture_value(0, 1, 2, 4)
        );
    }

    #[test]
    fn cancellation_and_decode_capacity_failures_are_typed() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let ledger = TestLedger::new(8 * 1024 * 1024, 1);
        let source = open_source(&root, &ledger);
        let key = key(0, 0, 0, [0, 0, 0], Shape3D::new(1, 1, 2).unwrap());
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();

        let mut cancelled = TestSink::cancelled(key, descriptor);
        assert_eq!(
            source.decode_into(&mut cancelled),
            Err(DatasetSourceFault::Cancelled { key })
        );
        assert!(cancelled.bytes.is_empty());

        let mut capacity = TestSink::new(key, descriptor);
        let error = source.decode_into(&mut capacity).unwrap_err();
        match error {
            DatasetSourceFault::CapacityExceeded {
                key: actual_key,
                category,
                requested_bytes,
                available_bytes,
            } => {
                assert_eq!(actual_key, key);
                assert_eq!(category, CpuLedgerCategory::InFlightDecode);
                assert!(requested_bytes > descriptor.value_byte_len());
                assert_eq!(available_bytes, 1);
            }
            other => panic!("expected typed capacity failure, got {other:?}"),
        }
        assert!(capacity.bytes.is_empty());
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);

        ledger.state.shutting_down.store(true, Ordering::SeqCst);
        let mut shutting_down = TestSink::new(key, descriptor);
        let error = source.decode_into(&mut shutting_down).unwrap_err();
        assert!(matches!(
            error,
            DatasetSourceFault::ShuttingDown {
                key: actual_key,
                category: CpuLedgerCategory::InFlightDecode,
                ..
            } if actual_key == key
        ));
        assert!(shutting_down.bytes.is_empty());
    }

    #[test]
    fn cancellation_is_observed_after_a_partial_sink_write() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let ledger = TestLedger::new(8 * 1024 * 1024, 8 * 1024 * 1024);
        let source = open_source(&root, &ledger);
        let key = key(0, 0, 0, [0, 0, 0], Shape3D::new(1, 1, 2).unwrap());
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = TestSink::new(key, descriptor);
        sink.cancel_after_writes = Some(1);

        assert_eq!(
            source.decode_into(&mut sink),
            Err(DatasetSourceFault::Cancelled { key })
        );
        assert!(!sink.bytes.is_empty());
        assert!(!sink.finished);
        assert_eq!(ledger.used(CpuLedgerCategory::InFlightDecode), 0);
    }

    #[test]
    fn scratch_admission_covers_a_physical_chunk_larger_than_the_returned_tile() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = tempdir.path().join("large-inner-chunk.m4d");
        write_large_inner_chunk_package(&root);
        // Tile-only accounting would admit exactly 64 KiB. The current codec
        // must additionally cover the 64 KiB + 1 inner chunk and encoded/index
        // working memory, so admission must reject before any source write.
        let ledger = TestLedger::new(8 * 1024 * 1024, SOURCE_DECODE_TILE_BYTES);
        let source = open_source(&root, &ledger);
        let key = key(
            0,
            0,
            0,
            [0, 0, 0],
            Shape3D::new(1, 1, SOURCE_DECODE_TILE_BYTES).unwrap(),
        );
        let descriptor = source
            .catalog()
            .unwrap()
            .resource_payload_descriptor(key)
            .unwrap();
        let mut sink = TestSink::new(key, descriptor);

        let error = source.decode_into(&mut sink).unwrap_err();
        match error {
            DatasetSourceFault::CapacityExceeded {
                category,
                requested_bytes,
                available_bytes,
                ..
            } => {
                assert_eq!(category, CpuLedgerCategory::InFlightDecode);
                assert!(requested_bytes > SOURCE_DECODE_TILE_BYTES);
                assert_eq!(available_bytes, SOURCE_DECODE_TILE_BYTES);
            }
            other => panic!("expected physical-chunk capacity failure, got {other:?}"),
        }
        assert!(sink.bytes.is_empty());
    }
}
