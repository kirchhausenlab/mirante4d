use std::path::Path;

use mirante4d_domain::{GridToWorld, IntensityDType, LogicalLayerKey, Shape4D};
use mirante4d_identity::{
    PackageId, PreparedScientificTile, SCIENTIFIC_TILE_SHAPE_TZYX, ScientificContentId,
    ScientificDatasetHasher, ScientificHashError, ScientificLayerDescriptor, ScientificLayerHasher,
    ScientificLayerRoot, ScientificTemporalCalibration as IdentityTemporalCalibration,
    ScientificTile,
};
use thiserror::Error;

use crate::brick_address::{brick_grid, pixel_brick};
use crate::package_read::{LocalDirectBrickRead, LocalDirectBrickReadError};
use crate::range_io::{LocalPackageRootSeal, PublishedPackageRootBinding};
use crate::{
    DatasetProfileAdmission, DirectoryInventoryError, ExactPackageCapability,
    INNER_CODEC_WORKING_BYTES_MAX, LocalBrickRead, LocalPackageCatalog,
    PACKAGE_VALIDATION_WORKING_BYTES, PackagePath, PackageReadError, PackageValidationError,
    PackedIndexCoordinates, RangeReadError, ScienceTemporalKind, amplification_2d,
    amplification_3d,
};

const SCIENTIFIC_SCAN_FIXED_WORKING_BYTES: u64 = 256 * 1024;

/// Closed storage execution contract used when a self-consistent package
/// crosses the create-only publication boundary.
///
/// The identifier is evidence, not caller input: it is issued only after the
/// storage-owned inventory/snapshot/inventory implementation completes.
pub const SCIENTIFIC_PUBLICATION_CURRENTNESS_CONTRACT_ID: &str =
    "mirante4d-publication-currentness-inventory-snapshot-inventory-1";

/// Observed work performed by one successful publication-currentness check.
///
/// The expected read count comes from the already-authenticated exact-package
/// proof. The observed counts are deltas from the capability's strict reader.
/// Construction is private to the storage-owned execution path, and that path
/// fails closed unless exactly one complete snapshot sweep and no codec decode
/// occurred between the two directory inventories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "publication-currentness evidence must remain bound to its consumed capability"]
pub struct ScientificPublicationTransferEvidence {
    expected_snapshot_object_reads: u64,
    first_inventory_object_reads: u64,
    observed_snapshot_object_reads: u64,
    second_inventory_object_reads: u64,
    observed_total_object_reads: u64,
    observed_codec_decode_calls: u64,
}

impl ScientificPublicationTransferEvidence {
    pub const fn contract_id(self) -> &'static str {
        SCIENTIFIC_PUBLICATION_CURRENTNESS_CONTRACT_ID
    }

    pub const fn expected_snapshot_object_reads(self) -> u64 {
        self.expected_snapshot_object_reads
    }

    pub const fn first_inventory_object_reads(self) -> u64 {
        self.first_inventory_object_reads
    }

    pub const fn observed_snapshot_object_reads(self) -> u64 {
        self.observed_snapshot_object_reads
    }

    pub const fn second_inventory_object_reads(self) -> u64 {
        self.second_inventory_object_reads
    }

    pub const fn observed_total_object_reads(self) -> u64 {
        self.observed_total_object_reads
    }

    pub const fn observed_codec_decode_calls(self) -> u64 {
        self.observed_codec_decode_calls
    }
}

/// Deterministic work performed by one successful scientific-content scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScientificValidationReport {
    layer_count: u32,
    identity_tiles: u64,
    brick_reads: u64,
    pyramid_fact_brick_reads: u64,
    pyramid_fact_object_reads: u64,
    pyramid_fact_range_requests: u64,
    pyramid_fact_encoded_bytes_read: u64,
    pyramid_fact_decoded_bytes: u64,
    object_reads: u64,
    range_requests: u64,
    encoded_bytes_read: u64,
    decoded_bytes: u64,
    logical_voxels: u64,
    canonical_value_bytes: u64,
    validity_bytes: u64,
    peak_tile_buffer_bytes: u64,
    peak_prepared_tiles: u64,
    peak_scan_working_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScientificValidationProgressStage {
    CanonicalBaseContent,
    PyramidAccelerationFacts,
}

/// Truthful cumulative decode work emitted only after a brick has been read,
/// decoded, and incorporated into the current audit stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScientificValidationProgress {
    stage: ScientificValidationProgressStage,
    decoded_bricks: u64,
    decoded_bytes: u64,
}

impl ScientificValidationProgress {
    pub const fn stage(self) -> ScientificValidationProgressStage {
        self.stage
    }

    pub const fn decoded_bricks(self) -> u64 {
        self.decoded_bricks
    }

    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }
}

impl ScientificValidationReport {
    pub const fn layer_count(self) -> u32 {
        self.layer_count
    }

    pub const fn identity_tiles(self) -> u64 {
        self.identity_tiles
    }

    pub const fn brick_reads(self) -> u64 {
        self.brick_reads
    }

    /// Non-base bricks decoded solely to prove every packed-index fact used by
    /// the published fast path. Base-brick facts are proved while computing
    /// the canonical content address and remain included in `brick_reads`.
    pub const fn pyramid_fact_brick_reads(self) -> u64 {
        self.pyramid_fact_brick_reads
    }

    pub const fn pyramid_fact_object_reads(self) -> u64 {
        self.pyramid_fact_object_reads
    }

    pub const fn pyramid_fact_range_requests(self) -> u64 {
        self.pyramid_fact_range_requests
    }

    pub const fn pyramid_fact_encoded_bytes_read(self) -> u64 {
        self.pyramid_fact_encoded_bytes_read
    }

    pub const fn pyramid_fact_decoded_bytes(self) -> u64 {
        self.pyramid_fact_decoded_bytes
    }

    pub const fn object_reads(self) -> u64 {
        self.object_reads
    }

    pub const fn range_requests(self) -> u64 {
        self.range_requests
    }

    pub const fn encoded_bytes_read(self) -> u64 {
        self.encoded_bytes_read
    }

    pub const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    pub const fn logical_voxels(self) -> u64 {
        self.logical_voxels
    }

    pub const fn canonical_value_bytes(self) -> u64 {
        self.canonical_value_bytes
    }

    pub const fn validity_bytes(self) -> u64 {
        self.validity_bytes
    }

    pub const fn peak_tile_buffer_bytes(self) -> u64 {
        self.peak_tile_buffer_bytes
    }

    pub const fn peak_prepared_tiles(self) -> u64 {
        self.peak_prepared_tiles
    }

    pub const fn peak_scan_working_bytes(self) -> u64 {
        self.peak_scan_working_bytes
    }
}

/// A package whose exact byte closure and declared scientific content both
/// passed their distinct validation contracts.
#[derive(Debug)]
pub struct SelfConsistentPackageCapability {
    exact: ExactPackageCapability,
    scientific_content_id: ScientificContentId,
    layer_roots: Vec<ScientificLayerRoot>,
    report: ScientificValidationReport,
    all_scale_packed_record_facts: CheckedAllScalePackedRecordFactsCapability,
}

/// Private proof marker issued only after the scientific pass has compared
/// every packed statistic at every runtime-addressable scale with its decoded
/// canonical samples.
#[derive(Debug)]
struct CheckedAllScalePackedRecordFactsCapability;

impl SelfConsistentPackageCapability {
    /// Number of encoded package objects covered by the exact-byte audit.
    pub const fn objects_hashed(&self) -> u64 {
        self.exact.objects_hashed()
    }

    /// Number of encoded package bytes covered by the exact-byte audit.
    pub const fn bytes_hashed(&self) -> u64 {
        self.exact.bytes_hashed()
    }

    pub const fn package_id(&self) -> PackageId {
        self.exact.package_id()
    }

    pub const fn scientific_content_id(&self) -> ScientificContentId {
        self.scientific_content_id
    }

    pub const fn admission(&self) -> DatasetProfileAdmission {
        self.exact.admission()
    }

    pub const fn catalog(&self) -> &LocalPackageCatalog {
        self.exact.catalog()
    }

    /// Canonical local root currently owned by this self-consistent capability.
    ///
    /// A capability transferred through create-only publication is rebound to
    /// the published destination before it can be returned to a caller.
    pub fn root_path(&self) -> &Path {
        self.exact.catalog().reader().root_path()
    }

    pub fn layer_roots(&self) -> &[ScientificLayerRoot] {
        &self.layer_roots
    }

    pub const fn validation_report(&self) -> ScientificValidationReport {
        self.report
    }

    pub fn revalidate_complete(
        &self,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageValidationError> {
        self.exact.revalidate_complete(is_cancelled)
    }

    pub fn read_brick(
        &self,
        coordinates: PackedIndexCoordinates,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        self.exact.read_brick(coordinates, is_cancelled)
    }

    pub(crate) fn begin_runtime_read_cohort(
        &self,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<crate::range_io::LocalCurrentnessBatch<'_>, PackageReadError> {
        let _proof = &self.all_scale_packed_record_facts;
        self.exact.begin_runtime_read_cohort(is_cancelled)
    }

    pub(crate) fn runtime_read_object_capacity(&self) -> usize {
        self.exact.runtime_read_object_capacity()
    }

    pub(crate) fn read_brick_into_sink_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        sink: &mut dyn mirante4d_dataset::ReservedDecodeSink,
        transaction: &mut crate::range_io::LocalCurrentnessBatch<'_>,
    ) -> Result<LocalDirectBrickRead, LocalDirectBrickReadError> {
        let _proof = &self.all_scale_packed_record_facts;
        self.exact.read_brick_into_sink_in_cohort(
            coordinates,
            sink,
            transaction,
            crate::package_read::DirectPayloadFactsAuthority::PublishedPackedRecord,
        )
    }

    pub(crate) fn read_brick_in_cohort(
        &self,
        coordinates: PackedIndexCoordinates,
        transaction: &mut crate::range_io::LocalCurrentnessBatch<'_>,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<LocalBrickRead, PackageReadError> {
        let _proof = &self.all_scale_packed_record_facts;
        self.exact
            .read_brick_in_cohort(coordinates, transaction, is_cancelled)
    }

    pub(crate) fn validate_cached_brick_in_cohort(
        &self,
        brick: &LocalBrickRead,
        transaction: &mut crate::range_io::LocalCurrentnessBatch<'_>,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.exact
            .validate_cached_brick_in_cohort(brick, transaction, is_cancelled)
    }

    pub(crate) fn revalidate_cached_brick(
        &self,
        brick: &LocalBrickRead,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), PackageReadError> {
        self.exact.revalidate_cached_brick(brick, is_cancelled)
    }

    pub(crate) fn prepare_atomic_publication(
        self,
        expected_stage: &Path,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<PreparedScientificPublication, ScientificPublicationTransferError> {
        let _ = refresh_publication_currentness(&self, &mut is_cancelled)?;
        let root_seal = self
            .exact
            .catalog()
            .reader()
            .seal_root_for_publication(expected_stage)
            .map_err(map_publication_root_binding_error)?;
        Ok(PreparedScientificPublication {
            capability: self,
            root_seal,
        })
    }
}

/// Typed failure while sealing, rebinding, or consuming a scientific package
/// publication transfer.
#[derive(Debug, Error)]
pub enum ScientificPublicationTransferError {
    #[error("scientific package publication transfer was cancelled")]
    Cancelled,
    #[error("published package inventory changed: {0}")]
    Inventory(DirectoryInventoryError),
    #[error("published package object snapshot changed: {0}")]
    Snapshot(PackageValidationError),
    #[error("published package root changed: {0}")]
    Root(RangeReadError),
    #[error("the published package root is not the directory sealed before publication")]
    RootBindingMismatch,
    #[error("publication-currentness evidence counter {counter} regressed")]
    EvidenceCounterRegression { counter: &'static str },
    #[error(
        "publication-currentness work differed from its closed contract: expected {expected_snapshot_object_reads} snapshot reads, observed inventory/snapshot/inventory object reads {first_inventory_object_reads}/{observed_snapshot_object_reads}/{second_inventory_object_reads} and {observed_codec_decode_calls} codec decodes"
    )]
    EvidenceContractMismatch {
        expected_snapshot_object_reads: u64,
        first_inventory_object_reads: u64,
        observed_snapshot_object_reads: u64,
        second_inventory_object_reads: u64,
        observed_codec_decode_calls: u64,
    },
}

/// Linear, crate-private handoff from staged scientific validation into atomic
/// local publication.
pub(crate) struct PreparedScientificPublication {
    capability: SelfConsistentPackageCapability,
    root_seal: LocalPackageRootSeal,
}

impl PreparedScientificPublication {
    pub(crate) const fn package_id(&self) -> PackageId {
        self.capability.package_id()
    }

    pub(crate) const fn root_matches(&self, device: u64, inode: u64) -> bool {
        self.root_seal.matches_node(device, inode)
    }

    pub(crate) const fn root_seal(&self) -> LocalPackageRootSeal {
        self.root_seal
    }

    pub(crate) fn object_open_operations(&self) -> u64 {
        self.capability.catalog().reader().object_open_operations()
    }

    pub(crate) fn rebind_after_publication(
        mut self,
        binding: PublishedPackageRootBinding,
    ) -> Result<SelfConsistentPackageCapability, ScientificPublicationTransferError> {
        self.capability
            .exact
            .catalog_mut()
            .reader_mut()
            .rebind_published_root(binding)
            .map_err(map_publication_root_binding_error)?;
        Ok(self.capability)
    }
}

pub(crate) fn refresh_publication_currentness(
    capability: &SelfConsistentPackageCapability,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<ScientificPublicationTransferEvidence, ScientificPublicationTransferError> {
    let reader = capability.exact.catalog().reader();
    let object_reads_before = reader.object_open_operations();
    let codec_decodes_before = reader.codec_decode_operations();
    inspect_publication_inventory(capability, is_cancelled)?;
    let object_reads_after_first_inventory = reader.object_open_operations();
    capability
        .revalidate_complete(&mut *is_cancelled)
        .map_err(map_publication_snapshot_error)?;
    let object_reads_after_snapshot = reader.object_open_operations();
    inspect_publication_inventory(capability, is_cancelled)?;
    let object_reads_after_second_inventory = reader.object_open_operations();

    let first_inventory_object_reads = object_reads_after_first_inventory
        .checked_sub(object_reads_before)
        .ok_or(
            ScientificPublicationTransferError::EvidenceCounterRegression {
                counter: "first-inventory object opens",
            },
        )?;
    let observed_snapshot_object_reads = object_reads_after_snapshot
        .checked_sub(object_reads_after_first_inventory)
        .ok_or(
            ScientificPublicationTransferError::EvidenceCounterRegression {
                counter: "snapshot object opens",
            },
        )?;
    let second_inventory_object_reads = object_reads_after_second_inventory
        .checked_sub(object_reads_after_snapshot)
        .ok_or(
            ScientificPublicationTransferError::EvidenceCounterRegression {
                counter: "second-inventory object opens",
            },
        )?;
    let observed_total_object_reads = object_reads_after_second_inventory
        .checked_sub(object_reads_before)
        .ok_or(
            ScientificPublicationTransferError::EvidenceCounterRegression {
                counter: "total currentness object opens",
            },
        )?;
    let observed_codec_decode_calls = reader
        .codec_decode_operations()
        .checked_sub(codec_decodes_before)
        .ok_or(
            ScientificPublicationTransferError::EvidenceCounterRegression {
                counter: "codec decodes",
            },
        )?;
    let evidence = ScientificPublicationTransferEvidence {
        expected_snapshot_object_reads: capability.exact.objects_hashed(),
        first_inventory_object_reads,
        observed_snapshot_object_reads,
        second_inventory_object_reads,
        observed_total_object_reads,
        observed_codec_decode_calls,
    };
    validate_publication_currentness_evidence(evidence)?;
    Ok(evidence)
}

fn validate_publication_currentness_evidence(
    evidence: ScientificPublicationTransferEvidence,
) -> Result<(), ScientificPublicationTransferError> {
    let reconciled_total = evidence
        .first_inventory_object_reads
        .checked_add(evidence.observed_snapshot_object_reads)
        .and_then(|value| value.checked_add(evidence.second_inventory_object_reads));
    if evidence.expected_snapshot_object_reads == 0
        || evidence.first_inventory_object_reads == 0
        || evidence.first_inventory_object_reads != evidence.second_inventory_object_reads
        || evidence.observed_snapshot_object_reads != evidence.expected_snapshot_object_reads
        || reconciled_total != Some(evidence.observed_total_object_reads)
        || evidence.observed_codec_decode_calls != 0
    {
        return Err(
            ScientificPublicationTransferError::EvidenceContractMismatch {
                expected_snapshot_object_reads: evidence.expected_snapshot_object_reads,
                first_inventory_object_reads: evidence.first_inventory_object_reads,
                observed_snapshot_object_reads: evidence.observed_snapshot_object_reads,
                second_inventory_object_reads: evidence.second_inventory_object_reads,
                observed_codec_decode_calls: evidence.observed_codec_decode_calls,
            },
        );
    }
    Ok(())
}

fn inspect_publication_inventory(
    capability: &SelfConsistentPackageCapability,
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPublicationTransferError> {
    capability
        .catalog()
        .inspect_directory_closure(&mut *is_cancelled)
        .map(|_| ())
        .map_err(map_publication_inventory_error)
}

fn map_publication_inventory_error(
    error: DirectoryInventoryError,
) -> ScientificPublicationTransferError {
    if matches!(error, DirectoryInventoryError::Cancelled) {
        ScientificPublicationTransferError::Cancelled
    } else {
        ScientificPublicationTransferError::Inventory(error)
    }
}

fn map_publication_snapshot_error(
    error: PackageValidationError,
) -> ScientificPublicationTransferError {
    if matches!(error, PackageValidationError::Cancelled) {
        ScientificPublicationTransferError::Cancelled
    } else {
        ScientificPublicationTransferError::Snapshot(error)
    }
}

fn map_publication_root_binding_error(error: RangeReadError) -> ScientificPublicationTransferError {
    if matches!(error, RangeReadError::RootChanged) {
        ScientificPublicationTransferError::RootBindingMismatch
    } else {
        ScientificPublicationTransferError::Root(error)
    }
}

/// Typed failure before a self-consistent package capability can issue.
#[derive(Debug, Error)]
pub enum ScientificPackageValidationError {
    #[error("scientific-content validation was cancelled")]
    Cancelled,
    #[error(transparent)]
    Read(PackageReadError),
    #[error(transparent)]
    Exact(PackageValidationError),
    #[error(transparent)]
    Identity(ScientificHashError),
    #[error("scientific metadata is internally inconsistent: {reason}")]
    MetadataInvariant { reason: &'static str },
    #[error("scientific validation {metric} arithmetic overflowed")]
    ArithmeticOverflow { metric: &'static str },
    #[error("scientific validation {metric} cannot be represented on this platform")]
    PlatformLength { metric: &'static str },
    #[error("logical layer {layer} has no unique physical image/channel mapping")]
    LogicalLayerMapping { layer: u32 },
    #[error("decoded {component} brick has {actual} bytes; expected exactly {expected}")]
    BrickPayloadLength {
        component: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error(
        "scientific scan requires {required} working bytes; validation reservation is {maximum}"
    )]
    WorkingSetExceeded { required: u64, maximum: u64 },
    #[error("computed scientific content {computed} differs from declared {declared}")]
    ScientificContentMismatch {
        declared: ScientificContentId,
        computed: ScientificContentId,
    },
}

impl ExactPackageCapability {
    /// Consumes an exact-package capability and recomputes the storage-independent
    /// content address from base-scale pixels and effective validity.
    ///
    /// The operation retains at most four fixed D-009 identity tiles, one
    /// decoded storage brick, and one profile-bounded slab of opaque tile
    /// digests. It checks manifest currentness around the whole scan, checks
    /// every consumed shard against the exact-package proof, and
    /// performs a final complete snapshot sweep before issuing the stronger
    /// capability.
    pub fn validate_scientific_content(
        self,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> Result<SelfConsistentPackageCapability, ScientificPackageValidationError> {
        self.validate_scientific_content_with_progress(&mut is_cancelled, |_| {})
    }

    pub fn validate_scientific_content_with_progress(
        self,
        mut is_cancelled: impl FnMut() -> bool,
        mut report_progress: impl FnMut(ScientificValidationProgress),
    ) -> Result<SelfConsistentPackageCapability, ScientificPackageValidationError> {
        check_cancelled(&mut is_cancelled)?;
        self.begin_scientific_scan(&mut is_cancelled)
            .map_err(map_exact_error)?;
        report_progress(ScientificValidationProgress {
            stage: ScientificValidationProgressStage::CanonicalBaseContent,
            decoded_bricks: 0,
            decoded_bytes: 0,
        });
        let (computed, layer_roots, mut report) =
            compute_scientific_content(&self, &mut is_cancelled, &mut report_progress)?;
        let declared = self.catalog().science().scientific_content_id();
        if computed != declared || self.catalog().profile().scientific_content_id() != declared {
            return Err(
                ScientificPackageValidationError::ScientificContentMismatch { declared, computed },
            );
        }
        report_progress(ScientificValidationProgress {
            stage: ScientificValidationProgressStage::PyramidAccelerationFacts,
            decoded_bricks: report.brick_reads,
            decoded_bytes: report.decoded_bytes,
        });
        validate_pyramid_packed_record_facts(
            &self,
            &mut report,
            &mut is_cancelled,
            &mut report_progress,
        )?;
        self.finish_scientific_scan(&mut is_cancelled)
            .map_err(map_exact_error)?;
        Ok(SelfConsistentPackageCapability {
            exact: self,
            scientific_content_id: computed,
            layer_roots,
            report,
            all_scale_packed_record_facts: CheckedAllScalePackedRecordFactsCapability,
        })
    }
}

/// Proves the packed statistics for every non-base brick before the runtime
/// can receive a self-consistent capability. Base bricks were already decoded
/// and checked by `compute_scientific_content`; this pass deliberately visits
/// only the remaining levels so the audit adds the pyramid volume once.
fn validate_pyramid_packed_record_facts(
    exact: &ExactPackageCapability,
    report: &mut ScientificValidationReport,
    is_cancelled: &mut impl FnMut() -> bool,
    report_progress: &mut impl FnMut(ScientificValidationProgress),
) -> Result<(), ScientificPackageValidationError> {
    for image in exact.catalog().profile().images() {
        for level in image.levels().iter().skip(1) {
            check_cancelled(is_cancelled)?;
            let metadata_path = PackagePath::parse(&format!("{}/zarr.json", level.pixel_path()))
                .map_err(|_| ScientificPackageValidationError::MetadataInvariant {
                    reason: "a validated pyramid pixel path stopped being canonical",
                })?;
            let metadata = exact.catalog().zarr_array(&metadata_path).ok_or(
                ScientificPackageValidationError::MetadataInvariant {
                    reason: "a validated pyramid level lost its pixel metadata",
                },
            )?;
            let shape: [u64; 5] = metadata.shape().try_into().map_err(|_| {
                ScientificPackageValidationError::MetadataInvariant {
                    reason: "a validated pyramid pixel array stopped being five-dimensional",
                }
            })?;
            let (brick_shape, _) = pixel_brick(metadata.kind()).ok_or(
                ScientificPackageValidationError::MetadataInvariant {
                    reason: "a validated pyramid pixel array stopped using a pixel profile",
                },
            )?;
            let grid = brick_grid(shape, brick_shape).map_err(|error| {
                ScientificPackageValidationError::Read(PackageReadError::Address(error))
            })?;
            for t in 0..grid[0] {
                for channel in 0..grid[1] {
                    for z in 0..grid[2] {
                        for y in 0..grid[3] {
                            for x in 0..grid[4] {
                                check_cancelled(is_cancelled)?;
                                let coordinates = PackedIndexCoordinates::new(
                                    image.image_ordinal(),
                                    level.scale_ordinal(),
                                    to_u32(t, "pyramid time coordinate")?,
                                    to_u32(channel, "pyramid channel coordinate")?,
                                    to_u32(z, "pyramid z coordinate")?,
                                    to_u32(y, "pyramid y coordinate")?,
                                    to_u32(x, "pyramid x coordinate")?,
                                );
                                let object_reads_started =
                                    exact.catalog().reader().object_open_operations();
                                let brick = exact
                                    .read_brick_for_scientific_scan(coordinates)
                                    .map_err(map_read_error)?;
                                if brick.payload_facts().is_none() {
                                    return Err(
                                        ScientificPackageValidationError::MetadataInvariant {
                                            reason: "pyramid fact validation returned no facts",
                                        },
                                    );
                                }
                                let object_reads = exact
                                    .catalog()
                                    .reader()
                                    .object_open_operations()
                                    .checked_sub(object_reads_started)
                                    .ok_or(
                                        ScientificPackageValidationError::ArithmeticOverflow {
                                            metric: "pyramid fact object-read counter delta",
                                        },
                                    )?;
                                record_pyramid_fact_read(&brick, object_reads, report)?;
                                report_progress(ScientificValidationProgress {
                                    stage:
                                        ScientificValidationProgressStage::PyramidAccelerationFacts,
                                    decoded_bricks: report
                                        .brick_reads
                                        .saturating_add(report.pyramid_fact_brick_reads),
                                    decoded_bytes: report
                                        .decoded_bytes
                                        .saturating_add(report.pyramid_fact_decoded_bytes),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn compute_scientific_content(
    exact: &ExactPackageCapability,
    is_cancelled: &mut impl FnMut() -> bool,
    report_progress: &mut impl FnMut(ScientificValidationProgress),
) -> Result<
    (
        ScientificContentId,
        Vec<ScientificLayerRoot>,
        ScientificValidationReport,
    ),
    ScientificPackageValidationError,
> {
    let science = exact.catalog().science();
    let layer_count = u32::try_from(science.layers().len()).map_err(|_| {
        ScientificPackageValidationError::PlatformLength {
            metric: "layer count",
        }
    })?;
    let mut dataset = ScientificDatasetHasher::new(layer_count)
        .map_err(ScientificPackageValidationError::Identity)?;
    let mut roots = Vec::with_capacity(science.layers().len());
    let mut report = ScientificValidationReport {
        layer_count,
        ..ScientificValidationReport::default()
    };

    for layer in science.layers() {
        check_cancelled(is_cancelled)?;
        let (image, physical_channel) = logical_mapping(exact.catalog(), layer.logical_layer())?;
        let temporal = match layer.temporal_calibration().kind() {
            ScienceTemporalKind::Unknown => IdentityTemporalCalibration::Unknown,
            ScienceTemporalKind::Regular => IdentityTemporalCalibration::Regular {
                step_seconds: layer
                    .temporal_calibration()
                    .regular_step_seconds()
                    .ok_or(ScientificPackageValidationError::MetadataInvariant {
                        reason: "regular time has no step",
                    })?
                    .value(),
            },
            ScienceTemporalKind::Explicit => IdentityTemporalCalibration::Explicit {
                positions_seconds: layer
                    .temporal_calibration()
                    .explicit_positions_seconds()
                    .ok_or(ScientificPackageValidationError::MetadataInvariant {
                        reason: "explicit time has no positions",
                    })?
                    .iter()
                    .map(|value| value.value())
                    .collect(),
            },
        };
        let grid_to_world = GridToWorld::from_row_major(
            layer
                .grid_to_world_micrometer_f64_bits()
                .map(|value| value.value()),
        )
        .map_err(|_| ScientificPackageValidationError::MetadataInvariant {
            reason: "scientific transform stopped being finite affine metadata",
        })?;
        let descriptor = ScientificLayerDescriptor::new(
            layer.logical_layer(),
            layer.dtype(),
            layer.base_shape(),
            temporal,
            grid_to_world,
        )
        .map_err(ScientificPackageValidationError::Identity)?;
        let mut hasher = ScientificLayerHasher::new(descriptor)
            .map_err(ScientificPackageValidationError::Identity)?;
        push_layer_tiles(
            exact,
            image,
            physical_channel,
            layer.base_shape(),
            layer.dtype(),
            &mut hasher,
            &mut report,
            is_cancelled,
            report_progress,
        )?;
        let root = hasher
            .finalize()
            .map_err(ScientificPackageValidationError::Identity)?;
        dataset
            .push_layer(root)
            .map_err(ScientificPackageValidationError::Identity)?;
        roots.push(root);
    }
    let scientific_content_id = dataset
        .finalize()
        .map_err(ScientificPackageValidationError::Identity)?;
    Ok((scientific_content_id, roots, report))
}

fn logical_mapping(
    catalog: &LocalPackageCatalog,
    logical_layer: LogicalLayerKey,
) -> Result<(u32, u32), ScientificPackageValidationError> {
    let mut result = None;
    for image in catalog.profile().images() {
        for mapping in image.logical_layers() {
            if mapping.logical_layer() == logical_layer
                && result
                    .replace((image.image_ordinal(), mapping.physical_channel()))
                    .is_some()
            {
                return Err(ScientificPackageValidationError::LogicalLayerMapping {
                    layer: logical_layer.ordinal(),
                });
            }
        }
    }
    result.ok_or(ScientificPackageValidationError::LogicalLayerMapping {
        layer: logical_layer.ordinal(),
    })
}

#[allow(clippy::too_many_arguments)]
fn push_layer_tiles(
    exact: &ExactPackageCapability,
    image: u32,
    physical_channel: u32,
    shape: Shape4D,
    dtype: IntensityDType,
    hasher: &mut ScientificLayerHasher,
    report: &mut ScientificValidationReport,
    is_cancelled: &mut impl FnMut() -> bool,
    report_progress: &mut impl FnMut(ScientificValidationProgress),
) -> Result<(), ScientificPackageValidationError> {
    let tile_counts = [
        shape.t(),
        ceil_div(shape.z(), SCIENTIFIC_TILE_SHAPE_TZYX[1])?,
        ceil_div(shape.y(), SCIENTIFIC_TILE_SHAPE_TZYX[2])?,
        ceil_div(shape.x(), SCIENTIFIC_TILE_SHAPE_TZYX[3])?,
    ];
    let brick_shape = if shape.z() == 1 {
        [1, 256, 256]
    } else {
        [64, 64, 64]
    };
    let z_bricks = ceil_div(shape.z(), brick_shape[0])?;

    for t in 0..tile_counts[0] {
        let time = to_u32(t, "time coordinate")?;
        for bz in 0..z_bricks {
            check_cancelled(is_cancelled)?;
            let slab_z_start = checked_mul(bz, brick_shape[0], "brick z origin")?;
            let slab_z_end =
                checked_add(slab_z_start, brick_shape[0], "brick z end")?.min(shape.z());
            let first_z_tile = slab_z_start / SCIENTIFIC_TILE_SHAPE_TZYX[1];
            let last_z_tile = (slab_z_end - 1) / SCIENTIFIC_TILE_SHAPE_TZYX[1];
            let slab_z_tiles = checked_add(last_z_tile - first_z_tile, 1, "slab tile count")?;
            let prepared_count = checked_product(
                [slab_z_tiles, tile_counts[2], tile_counts[3]],
                "prepared tile count",
            )?;
            report.peak_prepared_tiles = report.peak_prepared_tiles.max(prepared_count);
            let prepared_bytes = checked_mul(
                prepared_count,
                to_u64(
                    std::mem::size_of::<Option<PreparedScientificTile>>(),
                    "prepared tile slot bytes",
                )?,
                "prepared tile storage bytes",
            )?;
            let brick_working_bytes = brick_read_working_bytes(dtype, shape.z() == 1)?;
            record_scan_working_set(prepared_bytes, 0, brick_working_bytes, report)?;
            let mut prepared = vec![None; to_usize(prepared_count, "prepared tile count")?];

            for y_tile in 0..tile_counts[2] {
                let y_start = checked_mul(y_tile, SCIENTIFIC_TILE_SHAPE_TZYX[2], "tile y origin")?;
                let y_extent = SCIENTIFIC_TILE_SHAPE_TZYX[2].min(shape.y() - y_start);
                let y_end = checked_add(y_start, y_extent, "tile y end")?;
                let first_by = y_start / brick_shape[1];
                let last_by = (y_end - 1) / brick_shape[1];

                for x_tile in 0..tile_counts[3] {
                    check_cancelled(is_cancelled)?;
                    let x_start =
                        checked_mul(x_tile, SCIENTIFIC_TILE_SHAPE_TZYX[3], "tile x origin")?;
                    let x_extent = SCIENTIFIC_TILE_SHAPE_TZYX[3].min(shape.x() - x_start);
                    let x_end = checked_add(x_start, x_extent, "tile x end")?;
                    let first_bx = x_start / brick_shape[2];
                    let last_bx = (x_end - 1) / brick_shape[2];

                    let mut buffer_specs =
                        Vec::with_capacity(to_usize(slab_z_tiles, "slab tile buffer count")?);
                    for z_tile in first_z_tile..=last_z_tile {
                        let z_start =
                            checked_mul(z_tile, SCIENTIFIC_TILE_SHAPE_TZYX[1], "tile z origin")?;
                        buffer_specs.push((
                            [t, z_start, y_start, x_start],
                            [
                                1,
                                SCIENTIFIC_TILE_SHAPE_TZYX[1].min(shape.z() - z_start),
                                y_extent,
                                x_extent,
                            ],
                        ));
                    }
                    let buffered_bytes =
                        buffer_specs
                            .iter()
                            .try_fold(0_u64, |total, (_origin, extent)| {
                                checked_add(
                                    total,
                                    ScientificTileBuffer::byte_len_for(dtype, *extent)?,
                                    "tile buffer bytes",
                                )
                            })?;
                    report.peak_tile_buffer_bytes =
                        report.peak_tile_buffer_bytes.max(buffered_bytes);
                    record_scan_working_set(
                        prepared_bytes,
                        buffered_bytes,
                        brick_working_bytes,
                        report,
                    )?;
                    let mut buffers = Vec::with_capacity(buffer_specs.len());
                    for (origin, extent) in buffer_specs {
                        buffers.push(ScientificTileBuffer::new(dtype, origin, extent)?);
                    }

                    for by in first_by..=last_by {
                        for bx in first_bx..=last_bx {
                            check_cancelled(is_cancelled)?;
                            let coordinates = PackedIndexCoordinates::new(
                                image,
                                0,
                                time,
                                physical_channel,
                                to_u32(bz, "z brick coordinate")?,
                                to_u32(by, "y brick coordinate")?,
                                to_u32(bx, "x brick coordinate")?,
                            );
                            let object_reads_started =
                                exact.catalog().reader().object_open_operations();
                            let brick = exact
                                .read_brick_for_scientific_scan(coordinates)
                                .map_err(map_read_error)?;
                            let object_reads_finished =
                                exact.catalog().reader().object_open_operations();
                            let object_reads = object_reads_finished
                                .checked_sub(object_reads_started)
                                .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
                                    metric: "brick object-read counter delta",
                                })?;
                            record_brick_read(&brick, object_reads, report)?;
                            report_progress(ScientificValidationProgress {
                                stage: ScientificValidationProgressStage::CanonicalBaseContent,
                                decoded_bricks: report.brick_reads,
                                decoded_bytes: report.decoded_bytes,
                            });
                            for buffer in &mut buffers {
                                let tile_start = buffer.spatial_origin();
                                let tile_extent = buffer.spatial_extent();
                                let tile_end = checked_end(tile_start, tile_extent)?;
                                copy_intersection(
                                    &brick,
                                    dtype,
                                    brick_shape,
                                    [bz, by, bx],
                                    tile_start,
                                    tile_end,
                                    tile_extent,
                                    &mut buffer.validity,
                                    &mut buffer.values,
                                    is_cancelled,
                                )?;
                            }
                        }
                    }

                    for (local_z, buffer) in buffers.into_iter().enumerate() {
                        let z_tile = checked_add(
                            first_z_tile,
                            to_u64(local_z, "local z tile")?,
                            "z tile index",
                        )?;
                        let linear_index =
                            linear_tile_index([t, z_tile, y_tile, x_tile], tile_counts)?;
                        let prepared_tile = hasher
                            .prepare_tile(linear_index, buffer.tile())
                            .map_err(ScientificPackageValidationError::Identity)?;
                        let slot = linear_3d(
                            [z_tile - first_z_tile, y_tile, x_tile],
                            [slab_z_tiles, tile_counts[2], tile_counts[3]],
                        )?;
                        if prepared[slot].replace(prepared_tile).is_some() {
                            return Err(ScientificPackageValidationError::MetadataInvariant {
                                reason: "scientific tile was prepared more than once",
                            });
                        }
                        record_identity_tile(&buffer, report)?;
                    }
                }
            }

            for prepared_tile in prepared {
                check_cancelled(is_cancelled)?;
                hasher
                    .push_prepared_tile(prepared_tile.ok_or(
                        ScientificPackageValidationError::MetadataInvariant {
                            reason: "scientific tile was not prepared",
                        },
                    )?)
                    .map_err(ScientificPackageValidationError::Identity)?;
            }
        }
    }
    Ok(())
}

struct ScientificTileBuffer {
    origin: [u64; 4],
    extent: [u64; 4],
    validity: Vec<u8>,
    values: Vec<u8>,
}

impl ScientificTileBuffer {
    fn new(
        dtype: IntensityDType,
        origin: [u64; 4],
        extent: [u64; 4],
    ) -> Result<Self, ScientificPackageValidationError> {
        let voxel_count = checked_product(extent, "tile voxel count")?;
        let value_bytes = checked_mul(
            voxel_count,
            u64::from(dtype.bytes_per_sample()),
            "tile value bytes",
        )?;
        let validity_bytes = voxel_count.checked_add(7).ok_or(
            ScientificPackageValidationError::ArithmeticOverflow {
                metric: "tile validity bytes",
            },
        )? / 8;
        Ok(Self {
            origin,
            extent,
            validity: vec![0; to_usize(validity_bytes, "tile validity bytes")?],
            values: vec![0; to_usize(value_bytes, "tile value bytes")?],
        })
    }

    fn byte_len_for(
        dtype: IntensityDType,
        extent: [u64; 4],
    ) -> Result<u64, ScientificPackageValidationError> {
        let voxel_count = checked_product(extent, "tile voxel count")?;
        let values = checked_mul(
            voxel_count,
            u64::from(dtype.bytes_per_sample()),
            "tile value bytes",
        )?;
        let validity = checked_add(voxel_count, 7, "tile validity bytes")? / 8;
        checked_add(validity, values, "tile buffer bytes")
    }

    fn tile(&self) -> ScientificTile<'_> {
        ScientificTile::new(self.origin, self.extent, &self.validity, &self.values)
    }

    const fn spatial_origin(&self) -> [u64; 3] {
        [self.origin[1], self.origin[2], self.origin[3]]
    }

    const fn spatial_extent(&self) -> [u64; 3] {
        [self.extent[1], self.extent[2], self.extent[3]]
    }
}

fn brick_read_working_bytes(
    dtype: IntensityDType,
    two_dimensional: bool,
) -> Result<u64, ScientificPackageValidationError> {
    let amplification = if two_dimensional {
        amplification_2d(dtype)
    } else {
        amplification_3d(dtype)
    };
    checked_add(
        amplification.read_bytes_max,
        amplification.decoded_bytes_max,
        "brick read working bytes",
    )
}

fn record_scan_working_set(
    prepared_bytes: u64,
    tile_buffer_bytes: u64,
    brick_read_bytes: u64,
    report: &mut ScientificValidationReport,
) -> Result<(), ScientificPackageValidationError> {
    let required = [
        SCIENTIFIC_SCAN_FIXED_WORKING_BYTES,
        prepared_bytes,
        tile_buffer_bytes,
        brick_read_bytes,
        INNER_CODEC_WORKING_BYTES_MAX,
    ]
    .into_iter()
    .try_fold(0_u64, |total, bytes| {
        checked_add(total, bytes, "scientific scan working bytes")
    })?;
    if required > PACKAGE_VALIDATION_WORKING_BYTES {
        return Err(ScientificPackageValidationError::WorkingSetExceeded {
            required,
            maximum: PACKAGE_VALIDATION_WORKING_BYTES,
        });
    }
    report.peak_scan_working_bytes = report.peak_scan_working_bytes.max(required);
    Ok(())
}

fn record_brick_read(
    brick: &LocalBrickRead,
    object_reads: u64,
    report: &mut ScientificValidationReport,
) -> Result<(), ScientificPackageValidationError> {
    if brick.payload_facts().is_none() {
        return Err(ScientificPackageValidationError::MetadataInvariant {
            reason: "base-brick fact validation returned no facts",
        });
    }
    report.brick_reads = checked_add(report.brick_reads, 1, "brick read count")?;
    report.object_reads = checked_add(report.object_reads, object_reads, "object read count")?;
    report.range_requests = checked_add(
        report.range_requests,
        u64::from(brick.range_requests()),
        "range request count",
    )?;
    report.encoded_bytes_read = checked_add(
        report.encoded_bytes_read,
        brick.encoded_bytes_read(),
        "encoded bytes read",
    )?;
    report.decoded_bytes =
        checked_add(report.decoded_bytes, brick.decoded_bytes(), "decoded bytes")?;
    Ok(())
}

fn record_pyramid_fact_read(
    brick: &LocalBrickRead,
    object_reads: u64,
    report: &mut ScientificValidationReport,
) -> Result<(), ScientificPackageValidationError> {
    report.pyramid_fact_brick_reads = checked_add(
        report.pyramid_fact_brick_reads,
        1,
        "pyramid fact brick-read count",
    )?;
    report.pyramid_fact_object_reads = checked_add(
        report.pyramid_fact_object_reads,
        object_reads,
        "pyramid fact object-read count",
    )?;
    report.pyramid_fact_range_requests = checked_add(
        report.pyramid_fact_range_requests,
        u64::from(brick.range_requests()),
        "pyramid fact range-request count",
    )?;
    report.pyramid_fact_encoded_bytes_read = checked_add(
        report.pyramid_fact_encoded_bytes_read,
        brick.encoded_bytes_read(),
        "pyramid fact encoded-byte count",
    )?;
    report.pyramid_fact_decoded_bytes = checked_add(
        report.pyramid_fact_decoded_bytes,
        brick.decoded_bytes(),
        "pyramid fact decoded-byte count",
    )?;
    Ok(())
}

fn record_identity_tile(
    buffer: &ScientificTileBuffer,
    report: &mut ScientificValidationReport,
) -> Result<(), ScientificPackageValidationError> {
    report.identity_tiles = checked_add(report.identity_tiles, 1, "identity tile count")?;
    let voxels = checked_product(buffer.extent, "tile voxel count")?;
    report.logical_voxels = checked_add(report.logical_voxels, voxels, "logical voxel count")?;
    report.canonical_value_bytes = checked_add(
        report.canonical_value_bytes,
        to_u64(buffer.values.len(), "canonical value bytes")?,
        "canonical value bytes",
    )?;
    report.validity_bytes = checked_add(
        report.validity_bytes,
        to_u64(buffer.validity.len(), "validity bytes")?,
        "validity bytes",
    )?;
    Ok(())
}

fn linear_tile_index(
    coordinate: [u64; 4],
    counts: [u64; 4],
) -> Result<u64, ScientificPackageValidationError> {
    coordinate
        .into_iter()
        .zip(counts)
        .try_fold(0_u64, |ordinal, (coordinate, count)| {
            ordinal
                .checked_mul(count)
                .and_then(|value| value.checked_add(coordinate))
                .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
                    metric: "scientific tile ordinal",
                })
        })
}

#[allow(clippy::too_many_arguments)]
fn copy_intersection(
    brick: &LocalBrickRead,
    dtype: IntensityDType,
    brick_shape: [u64; 3],
    brick_coordinates: [u64; 3],
    tile_start: [u64; 3],
    tile_end: [u64; 3],
    tile_extent: [u64; 3],
    validity: &mut [u8],
    values: &mut [u8],
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPackageValidationError> {
    let sample_bytes = usize::from(dtype.bytes_per_sample());
    let brick_capacity = checked_product(brick_shape, "brick capacity")?;
    if let Some(pixel) = brick.pixel_payload() {
        let expected = to_usize(
            brick_capacity.checked_mul(sample_bytes as u64).ok_or(
                ScientificPackageValidationError::ArithmeticOverflow {
                    metric: "brick pixel bytes",
                },
            )?,
            "brick pixel bytes",
        )?;
        if pixel.len() != expected {
            return Err(ScientificPackageValidationError::BrickPayloadLength {
                component: "pixel",
                expected,
                actual: pixel.len(),
            });
        }
    }
    if let Some(bits) = brick.validity_payload() {
        let expected = to_usize(brick_capacity.div_ceil(8), "brick validity bytes")?;
        if bits.len() != expected {
            return Err(ScientificPackageValidationError::BrickPayloadLength {
                component: "validity",
                expected,
                actual: bits.len(),
            });
        }
    }
    let brick_start = [
        checked_mul(brick_coordinates[0], brick_shape[0], "brick z origin")?,
        checked_mul(brick_coordinates[1], brick_shape[1], "brick y origin")?,
        checked_mul(brick_coordinates[2], brick_shape[2], "brick x origin")?,
    ];
    let brick_end = checked_end(brick_start, brick.logical_extent_zyx())?;
    let start = [
        tile_start[0].max(brick_start[0]),
        tile_start[1].max(brick_start[1]),
        tile_start[2].max(brick_start[2]),
    ];
    let end = [
        tile_end[0].min(brick_end[0]),
        tile_end[1].min(brick_end[1]),
        tile_end[2].min(brick_end[2]),
    ];
    for z in start[0]..end[0] {
        for y in start[1]..end[1] {
            check_cancelled(is_cancelled)?;
            for x in start[2]..end[2] {
                let source = linear_3d(
                    [z - brick_start[0], y - brick_start[1], x - brick_start[2]],
                    brick_shape,
                )?;
                let target = linear_3d(
                    [z - tile_start[0], y - tile_start[1], x - tile_start[2]],
                    tile_extent,
                )?;
                let valid = if !brick.record().explicit_validity() {
                    true
                } else if brick.record().statistics().valid_voxel_count() == 0 {
                    false
                } else {
                    let bits = brick.validity_payload().ok_or(
                        ScientificPackageValidationError::MetadataInvariant {
                            reason: "a partly valid explicit brick has no validity payload",
                        },
                    )?;
                    bits[source / 8] & (1 << (source % 8)) != 0
                };
                if valid {
                    validity[target / 8] |= 1 << (target % 8);
                    if let Some(pixel) = brick.pixel_payload() {
                        let source = source * sample_bytes;
                        let target = target * sample_bytes;
                        values[target..target + sample_bytes]
                            .copy_from_slice(&pixel[source..source + sample_bytes]);
                    }
                }
            }
        }
    }
    Ok(())
}

fn linear_3d(
    coordinate: [u64; 3],
    shape: [u64; 3],
) -> Result<usize, ScientificPackageValidationError> {
    let ordinal = coordinate[0]
        .checked_mul(shape[1])
        .and_then(|value| value.checked_add(coordinate[1]))
        .and_then(|value| value.checked_mul(shape[2]))
        .and_then(|value| value.checked_add(coordinate[2]))
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
            metric: "brick sample ordinal",
        })?;
    to_usize(ordinal, "brick sample ordinal")
}

fn checked_end(
    start: [u64; 3],
    extent: [u64; 3],
) -> Result<[u64; 3], ScientificPackageValidationError> {
    Ok([
        checked_add(start[0], extent[0], "z extent")?,
        checked_add(start[1], extent[1], "y extent")?,
        checked_add(start[2], extent[2], "x extent")?,
    ])
}

fn checked_product<const N: usize>(
    values: [u64; N],
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    values.into_iter().try_fold(1_u64, |result, value| {
        result
            .checked_mul(value)
            .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
    })
}

fn ceil_div(value: u64, divisor: u64) -> Result<u64, ScientificPackageValidationError> {
    value
        .checked_add(divisor - 1)
        .map(|value| value / divisor)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow {
            metric: "identity tile count",
        })
}

fn checked_add(
    left: u64,
    right: u64,
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    left.checked_add(right)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
}

fn checked_mul(
    left: u64,
    right: u64,
    metric: &'static str,
) -> Result<u64, ScientificPackageValidationError> {
    left.checked_mul(right)
        .ok_or(ScientificPackageValidationError::ArithmeticOverflow { metric })
}

fn to_u64(value: usize, metric: &'static str) -> Result<u64, ScientificPackageValidationError> {
    u64::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn to_usize(value: u64, metric: &'static str) -> Result<usize, ScientificPackageValidationError> {
    usize::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn to_u32(value: u64, metric: &'static str) -> Result<u32, ScientificPackageValidationError> {
    u32::try_from(value).map_err(|_| ScientificPackageValidationError::PlatformLength { metric })
}

fn check_cancelled(
    is_cancelled: &mut impl FnMut() -> bool,
) -> Result<(), ScientificPackageValidationError> {
    if is_cancelled() {
        Err(ScientificPackageValidationError::Cancelled)
    } else {
        Ok(())
    }
}

fn map_read_error(error: PackageReadError) -> ScientificPackageValidationError {
    match error {
        PackageReadError::Cancelled => ScientificPackageValidationError::Cancelled,
        PackageReadError::NonFinitePixelPayload {
            sample_index,
            sample_valid: true,
        } => {
            // Canonical content validation owns the finite-float
            // contract. The packed-fact pass may detect the same non-finite
            // sample before the tile hasher, but it must preserve the
            // established scientific-readback rejection class.
            ScientificPackageValidationError::Identity(ScientificHashError::NonFiniteFloatSample {
                index: sample_index,
            })
        }
        error => ScientificPackageValidationError::Read(error),
    }
}

fn map_exact_error(error: PackageValidationError) -> ScientificPackageValidationError {
    if matches!(error, PackageValidationError::Cancelled) {
        ScientificPackageValidationError::Cancelled
    } else {
        ScientificPackageValidationError::Exact(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_and_checked_work_accounting_fail_closed() {
        assert!(matches!(
            check_cancelled(&mut || true),
            Err(ScientificPackageValidationError::Cancelled)
        ));
        assert!(matches!(
            checked_add(u64::MAX, 1, "test"),
            Err(ScientificPackageValidationError::ArithmeticOverflow { metric: "test" })
        ));
        let mut report = ScientificValidationReport::default();
        assert!(matches!(
            record_scan_working_set(PACKAGE_VALIDATION_WORKING_BYTES, 0, 0, &mut report),
            Err(ScientificPackageValidationError::WorkingSetExceeded {
                maximum: PACKAGE_VALIDATION_WORKING_BYTES,
                ..
            })
        ));
    }

    #[test]
    fn publication_evidence_rejects_one_extra_complete_snapshot_pass() {
        let valid = ScientificPublicationTransferEvidence {
            expected_snapshot_object_reads: 3,
            first_inventory_object_reads: 5,
            observed_snapshot_object_reads: 3,
            second_inventory_object_reads: 5,
            observed_total_object_reads: 13,
            observed_codec_decode_calls: 0,
        };
        validate_publication_currentness_evidence(valid).unwrap();

        let one_extra_pass = ScientificPublicationTransferEvidence {
            observed_snapshot_object_reads: 6,
            observed_total_object_reads: 16,
            ..valid
        };
        assert!(matches!(
            validate_publication_currentness_evidence(one_extra_pass),
            Err(ScientificPublicationTransferError::EvidenceContractMismatch { .. })
        ));
    }

    #[test]
    fn identity_hasher_rejects_valid_nonfinite_and_accepts_invalid_zero() {
        let descriptor = ScientificLayerDescriptor::new(
            LogicalLayerKey::new(0),
            IntensityDType::Float32,
            Shape4D::new(1, 1, 1, 1).unwrap(),
            IdentityTemporalCalibration::Unknown,
            GridToWorld::identity(),
        )
        .unwrap();
        let mut nonfinite = ScientificLayerHasher::new(descriptor.clone()).unwrap();
        assert!(matches!(
            nonfinite.push_tile(ScientificTile::new(
                [0; 4],
                [1; 4],
                &[1],
                &f32::NAN.to_bits().to_le_bytes(),
            )),
            Err(ScientificHashError::NonFiniteFloatSample { .. })
        ));

        let mut invalid = ScientificLayerHasher::new(descriptor).unwrap();
        invalid
            .push_tile(ScientificTile::new(
                [0; 4],
                [1; 4],
                &[0],
                &0_u32.to_le_bytes(),
            ))
            .unwrap();
        assert!(invalid.finalize().is_ok());
    }

    #[test]
    fn read_error_mapping_promotes_only_valid_nonfinite_samples() {
        assert!(matches!(
            map_read_error(PackageReadError::NonFinitePixelPayload {
                sample_index: 7,
                sample_valid: true,
            }),
            ScientificPackageValidationError::Identity(ScientificHashError::NonFiniteFloatSample {
                index: 7
            })
        ));
        assert!(matches!(
            map_read_error(PackageReadError::NonFinitePixelPayload {
                sample_index: 7,
                sample_valid: false,
            }),
            ScientificPackageValidationError::Read(PackageReadError::NonFinitePixelPayload {
                sample_index: 7,
                sample_valid: false,
            })
        ));
    }
}
