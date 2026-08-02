//! Pure semantic demand planning for the unified runtime.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use mirante4d_dataset::{
    BrickKey, CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    ResourceRegion,
};
use mirante4d_domain::{
    CameraView, IsoShadingPolicy, LogicalLayerKey, Projection, RenderState, SamplingPolicy,
    ScaleLevel, Shape3D, TimeIndex,
};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    CameraFrame, PreparedResourceBody, PresentationViewport, RenderApiError, RenderExtent,
};

use crate::{
    projected_lod::{select_cross_section_level, select_volume_level},
    semantic_demand::{
        CrossSectionPlane, SemanticCameraReuseEnvelope, SemanticPlanError, SemanticPlanLimits,
        SemanticPlaneLayerReuseGuard, SemanticPlaneReuseEnvelope, SemanticRegionGridSpec,
        plan_cross_section_resource_regions_cancellable,
        plan_guarded_cross_section_resource_regions_cancellable,
        plan_prioritized_visible_resource_regions_cancellable,
    },
    semantic_tiles::{SEMANTIC_TILE_SIDE, SemanticTileGrid, SemanticTileIndex},
    viewer_layout::PanelId,
};

const NAVIGATION_TAIL_PAYLOAD_DIVISOR: u64 = 4;
const NAVIGATION_TAIL_MAX_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const NAVIGATION_TAIL_RESOURCE_DIVISOR: usize = 4;
const NAVIGATION_TAIL_MAX_RESOURCES: usize = 16_384;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlan {
    /// Exact selected scale for every and only visible render layer.
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) resources: Vec<BrickKey>,
    pub(crate) payload_bytes: u64,
    pub(crate) playback_downshifted: bool,
    pub(crate) covers_full_volume: bool,
    /// Exact current-camera prefix. Remaining resources, if any, are the
    /// fixed bounded guard tier and never precede current-view I/O.
    pub(crate) primary_resource_count: usize,
    /// Camera target plus its optional guard before the independent resident
    /// navigation ladder is appended. Playback may prefetch this prefix but
    /// must not duplicate the active timepoint's full ladder.
    pub(crate) playback_resource_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressiveDatasetDemandPlan {
    pub(crate) ideal_layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) target: DatasetDemandPlan,
    pub(crate) coarse: Option<DatasetDemandPlan>,
    /// Complete full-volume candidates ordered from the mandatory terminal
    /// body toward successively finer coherent per-layer scale maps.
    pub(crate) navigation_candidates: Vec<DatasetDemandPlan>,
}

pub(crate) struct ProgressiveDatasetDemandPlanning {
    pub(crate) plan: ProgressiveDatasetDemandPlan,
    /// Number of requested future timepoints admitted at the selected stable
    /// playback rung. This can be shorter than the desired one-second tail,
    /// but never shorter than the required startup runway.
    pub(crate) playback_timepoint_count: usize,
    pub(crate) candidates_visited: usize,
    pub(crate) reuse_envelope: Option<SemanticCameraReuseEnvelope>,
    pub(crate) scratch_charges: Vec<Box<dyn CpuByteLease>>,
}

pub(crate) struct DatasetDemandPlanning {
    pub(crate) plan: DatasetDemandPlan,
    pub(crate) candidates_visited: usize,
    guard_retained: bool,
    pub(crate) plane_reuse_envelope: Option<SemanticPlaneReuseEnvelope>,
    pub(crate) scratch_charges: Vec<Box<dyn CpuByteLease>>,
}

/// One immutable, already-installed full-volume navigation rung.
#[derive(Debug, Clone)]
pub(crate) struct NavigationCandidateBaseline {
    body: PreparedResourceBody,
    layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
    planned_payload_bytes: u64,
}

impl NavigationCandidateBaseline {
    pub(crate) fn new(
        body: PreparedResourceBody,
        layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
        planned_payload_bytes: u64,
    ) -> Self {
        Self {
            body,
            layer_scales,
            planned_payload_bytes,
        }
    }
}

/// Immutable, already-installed resident ladder which can seed a replacement
/// camera plan without traversing every complete full-volume rung again.
///
/// The application only constructs this from prepared navigation render bodies
/// sharing the installed current-3D residency union. The planner still
/// revalidates source, timepoint, visible layers, scale adjacency, semantic
/// tiling, payload accounting, and the current byte/resource limits before
/// adopting any rung.
#[derive(Debug, Clone)]
pub(crate) struct NavigationLadderBaseline {
    candidates: Arc<[NavigationCandidateBaseline]>,
}

impl NavigationLadderBaseline {
    pub(crate) fn new(candidates: Vec<NavigationCandidateBaseline>) -> Self {
        Self {
            candidates: candidates.into(),
        }
    }

    fn terminal(&self) -> Option<&NavigationCandidateBaseline> {
        self.candidates.first()
    }
}

impl DatasetDemandPlanning {
    pub(crate) fn prepare_accounted(
        self,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<(
        PreparedDatasetDemandPlan,
        usize,
        Option<SemanticPlaneReuseEnvelope>,
    )> {
        let Self {
            plan,
            candidates_visited,
            guard_retained: _,
            plane_reuse_envelope,
            scratch_charges,
        } = self;
        let prepared = PreparedDatasetDemandPlan::from_plan_accounted(plan, reservations)?;
        drop(scratch_charges);
        Ok((prepared, candidates_visited, plane_reuse_envelope))
    }
}

/// Exact result reservations acquired before worker-built immutable arrays.
/// Small per-allocation leases avoid a pessimistic 65k worst-case reservation;
/// `finish` folds their lifetimes into one shared authority for every result
/// consumer without changing ledger accounting.
pub(crate) struct PreparedAllocationReservations<'a> {
    ledger: &'a dyn CpuByteLedger,
    result_leases: Vec<Box<dyn CpuByteLease>>,
    result_bytes: u64,
}

impl<'a> PreparedAllocationReservations<'a> {
    pub(crate) fn new(ledger: &'a dyn CpuByteLedger) -> Self {
        Self {
            ledger,
            result_leases: Vec::new(),
            result_bytes: 0,
        }
    }

    pub(crate) fn reserve_result(&mut self, bytes: u64) -> anyhow::Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let total = self
            .result_bytes
            .checked_add(bytes)
            .ok_or_else(|| anyhow::anyhow!("prepared result byte accounting overflow"))?;
        let lease = self
            .ledger
            .try_acquire(CpuLedgerCategory::QueuesAndResults, bytes)?;
        self.result_leases.push(lease);
        self.result_bytes = total;
        Ok(())
    }

    pub(crate) fn reserve_temporary(
        &self,
        bytes: u64,
    ) -> anyhow::Result<Option<Box<dyn CpuByteLease>>> {
        if bytes == 0 {
            return Ok(None);
        }
        self.ledger
            .try_acquire(CpuLedgerCategory::MetadataAndIndexes, bytes)
            .map(Some)
            .map_err(Into::into)
    }

    pub(crate) fn finish(self) -> Arc<dyn CpuByteLease> {
        Arc::new(PreparedResultLeaseBundle {
            leases: self.result_leases.into_boxed_slice(),
            reserved_bytes: self.result_bytes,
        })
    }
}

/// Exact allocation ownership for one independently replaceable result
/// cohort. The camera worker finishes a distinct bundle for the 3D cohort,
/// each cross-section panel, each retained-requirement union, and its transient
/// removal delta. Consequently an unchanged tiny panel cannot pin a replaced
/// large 3D allocation (or vice versa), while reclamation remains O(scopes).
struct PreparedResultLeaseBundle {
    leases: Box<[Box<dyn CpuByteLease>]>,
    reserved_bytes: u64,
}

impl CpuByteLease for PreparedResultLeaseBundle {
    fn category(&self) -> CpuLedgerCategory {
        CpuLedgerCategory::QueuesAndResults
    }

    fn reserved_bytes(&self) -> u64 {
        debug_assert!(
            self.leases
                .iter()
                .all(|lease| { lease.category() == CpuLedgerCategory::QueuesAndResults })
        );
        self.reserved_bytes
    }
}

/// Immutable requirement bodies prepared on the demand worker. Canonical
/// membership and contribution-ranked admission have one owner and transfer
/// to the UI without cloning, sorting, or retaining a per-key index map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedDemandRequirements {
    body: PreparedResourceBody,
    admitted_prefix_len: usize,
    first_useful_prefix_len: usize,
    /// Contribution-ranked resources before this boundary are required by
    /// the current semantic view. The remaining suffix is a resident camera
    /// guard and cannot delay readiness until explicitly promoted.
    required_prefix_len: usize,
}

impl PreparedDemandRequirements {
    #[cfg(test)]
    pub(crate) fn from_prepared_body_for_test(
        body: PreparedResourceBody,
        admitted_prefix_len: usize,
        required_prefix_len: usize,
    ) -> Self {
        assert!(admitted_prefix_len <= body.ranked().len());
        assert!(required_prefix_len <= body.ranked().len());
        Self {
            body,
            admitted_prefix_len,
            first_useful_prefix_len: required_prefix_len.min(1),
            required_prefix_len,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_ranked(
        ranked: impl Into<Arc<[BrickKey]>>,
    ) -> Result<Self, PreparedDemandPlanError> {
        let ranked = ranked.into();
        let mut canonical = ranked.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.len() != ranked.len() {
            return Err(PreparedDemandPlanError::DuplicateRequirement);
        }
        let required_prefix_len = ranked.len();
        Ok(Self {
            body: PreparedResourceBody::new(canonical.into(), ranked, None)?,
            admitted_prefix_len: 0,
            first_useful_prefix_len: usize::from(required_prefix_len != 0),
            required_prefix_len,
        })
    }

    pub(crate) fn from_ranked_accounted(
        ranked: Vec<BrickKey>,
        required_prefix_len: usize,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        Self::from_ranked_with_prefixes_accounted(
            ranked,
            usize::from(required_prefix_len != 0),
            required_prefix_len,
            reservations,
        )
    }

    pub(crate) fn from_ranked_with_prefixes_accounted(
        ranked: Vec<BrickKey>,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        let empty = ranked.is_empty() && first_useful_prefix_len == 0 && required_prefix_len == 0;
        if !empty
            && (first_useful_prefix_len == 0
                || first_useful_prefix_len > required_prefix_len
                || required_prefix_len > ranked.len())
        {
            return Err(PreparedDemandPlanError::RequiredPrefixOutOfBounds.into());
        }
        let retained_bytes =
            PreparedResourceBody::preflight_host_allocation_bytes(ranked.len(), ranked.len())?;
        reservations.reserve_result(retained_bytes)?;
        let temporary_bytes = key_array_bytes(ranked.len())?;
        let temporary_charge = reservations.reserve_temporary(temporary_bytes)?;
        let mut canonical = ranked.clone();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.len() != ranked.len() {
            return Err(PreparedDemandPlanError::DuplicateRequirement.into());
        }
        let body = PreparedResourceBody::new(canonical.into(), ranked.into(), None)?;
        drop(temporary_charge);
        Ok(Self {
            body,
            admitted_prefix_len: 0,
            first_useful_prefix_len,
            required_prefix_len,
        })
    }

    /// Replaces this body's ranked/canonical allocation while transferring its
    /// existing retained reservation and acquiring only the exact growth.
    ///
    /// The old and new bodies coexist briefly while the floor is merged, so a
    /// temporary lease accounts that overlap. The finished result must not pin
    /// the discarded target-only body's full byte charge.
    pub(crate) fn into_merged_ranked_with_prefixes_accounted(
        self,
        ranked: Vec<BrickKey>,
        first_useful_prefix_len: usize,
        required_prefix_len: usize,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        if ranked.is_empty()
            || first_useful_prefix_len == 0
            || first_useful_prefix_len > required_prefix_len
            || required_prefix_len > ranked.len()
        {
            return Err(PreparedDemandPlanError::RequiredPrefixOutOfBounds.into());
        }
        let old_body_bytes = self.body.host_allocation_bytes();
        let new_body_bytes =
            PreparedResourceBody::preflight_host_allocation_bytes(ranked.len(), ranked.len())?;
        let retained_growth = new_body_bytes
            .checked_sub(old_body_bytes)
            .ok_or_else(|| anyhow::anyhow!("merged prepared body unexpectedly shrank"))?;
        reservations.reserve_result(retained_growth)?;
        let overlap_charge = reservations.reserve_temporary(old_body_bytes)?;
        let mut canonical = ranked.clone();
        canonical.sort_unstable();
        canonical.dedup();
        if canonical.len() != ranked.len() {
            return Err(PreparedDemandPlanError::DuplicateRequirement.into());
        }
        let body = PreparedResourceBody::new(canonical.into(), ranked.into(), None)?;
        drop(self);
        drop(overlap_charge);
        Ok(Self {
            body,
            admitted_prefix_len: 0,
            first_useful_prefix_len,
            required_prefix_len,
        })
    }

    /// Keeps already-submitted resources at the front while reprioritizing
    /// only the unadmitted tail. This merge runs on the planning worker, so a
    /// camera-result commit can swap the finished ranked body by `Arc`.
    #[cfg(test)]
    pub(crate) fn preserving_admitted_prefix(
        &self,
        admitted_prefix: &[BrickKey],
    ) -> Result<Self, PreparedDemandPlanError> {
        if admitted_prefix.is_empty() {
            return Ok(Self {
                body: self.body.clone(),
                admitted_prefix_len: 0,
                first_useful_prefix_len: self.first_useful_prefix_len,
                required_prefix_len: self.required_prefix_len,
            });
        }

        let mut required = self.ranked()[..self.required_prefix_len].to_vec();
        required.sort_unstable();
        let preserved = admitted_prefix
            .iter()
            .copied()
            .filter(|key| required.binary_search(key).is_ok())
            .collect::<Vec<_>>();
        let mut sorted_preserved = preserved.clone();
        sorted_preserved.sort_unstable();
        sorted_preserved.dedup();
        if sorted_preserved.len() != preserved.len() {
            return Err(PreparedDemandPlanError::DuplicateAdmittedRequirement);
        }

        let mut merged = Vec::with_capacity(self.ranked().len());
        merged.extend_from_slice(&preserved);
        merged.extend(
            self.ranked()
                .iter()
                .copied()
                .filter(|key| sorted_preserved.binary_search(key).is_err()),
        );
        debug_assert_eq!(merged.len(), self.ranked().len());
        Ok(Self {
            body: PreparedResourceBody::new(Arc::clone(self.canonical()), merged.into(), None)?,
            admitted_prefix_len: preserved.len(),
            first_useful_prefix_len: self.first_useful_prefix_len,
            required_prefix_len: self.required_prefix_len,
        })
    }

    pub(crate) fn into_preserving_admitted_prefix_accounted(
        self,
        admitted_prefix: &[BrickKey],
        reservations: &PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        if admitted_prefix.is_empty() {
            return Ok(Self {
                body: self.body,
                admitted_prefix_len: 0,
                first_useful_prefix_len: self.first_useful_prefix_len,
                required_prefix_len: self.required_prefix_len,
            });
        }
        let body_bytes = PreparedResourceBody::preflight_host_allocation_bytes(
            self.canonical().len(),
            self.ranked().len(),
        )?;
        let admitted_bytes = key_array_bytes(admitted_prefix.len())?
            .checked_mul(2)
            .ok_or_else(|| anyhow::anyhow!("admitted-prefix scratch byte overflow"))?;
        let required_bytes = key_array_bytes(self.required_prefix_len)?;
        let temporary_bytes = body_bytes
            .checked_add(admitted_bytes)
            .and_then(|bytes| bytes.checked_add(required_bytes))
            .ok_or_else(|| anyhow::anyhow!("admitted-prefix scratch byte overflow"))?;
        let temporary_charge = reservations.reserve_temporary(temporary_bytes)?;
        let mut required = self.ranked()[..self.required_prefix_len].to_vec();
        required.sort_unstable();
        let preserved = admitted_prefix
            .iter()
            .copied()
            .filter(|key| required.binary_search(key).is_ok())
            .collect::<Vec<_>>();
        let mut sorted_preserved = preserved.clone();
        sorted_preserved.sort_unstable();
        sorted_preserved.dedup();
        if sorted_preserved.len() != preserved.len() {
            return Err(PreparedDemandPlanError::DuplicateAdmittedRequirement.into());
        }
        // The retained result reservation transfers from the consumed body to
        // the identically-sized replacement. This temporary lease covers the
        // old ranked/body allocation plus the exact-capacity merge overlap.
        let canonical = Arc::clone(self.canonical());
        let required_prefix_len = self.required_prefix_len;
        let first_useful_prefix_len = self.first_useful_prefix_len;
        let mut merged = Vec::with_capacity(self.ranked().len());
        merged.extend_from_slice(&preserved);
        merged.extend(
            self.ranked()
                .iter()
                .copied()
                .filter(|key| sorted_preserved.binary_search(key).is_err()),
        );
        let body = PreparedResourceBody::new(canonical, merged.into(), None)?;
        drop(self);
        drop(temporary_charge);
        Ok(Self {
            body,
            admitted_prefix_len: preserved.len(),
            first_useful_prefix_len,
            required_prefix_len,
        })
    }

    pub(crate) fn empty() -> Self {
        Self {
            body: PreparedResourceBody::new(Arc::from([]), Arc::from([]), None)
                .expect("an empty prepared application body is valid"),
            admitted_prefix_len: 0,
            first_useful_prefix_len: 0,
            required_prefix_len: 0,
        }
    }

    pub(crate) fn empty_accounted(
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        reservations
            .reserve_result(PreparedResourceBody::preflight_host_allocation_bytes(0, 0)?)?;
        Ok(Self::empty())
    }

    pub(crate) fn into_body_and_prefixes(self) -> (PreparedResourceBody, usize, usize) {
        (
            self.body,
            self.admitted_prefix_len,
            self.required_prefix_len,
        )
    }

    pub(crate) const fn body(&self) -> &PreparedResourceBody {
        &self.body
    }

    pub(crate) fn canonical(&self) -> &Arc<[BrickKey]> {
        self.body.canonical()
    }

    pub(crate) fn ranked(&self) -> &Arc<[BrickKey]> {
        self.body.ranked()
    }

    #[cfg(test)]
    pub(crate) fn host_allocation_bytes(&self) -> u64 {
        self.body.host_allocation_bytes()
    }

    #[cfg(test)]
    pub(crate) const fn admitted_prefix_len(&self) -> usize {
        self.admitted_prefix_len
    }

    pub(crate) const fn required_prefix_len(&self) -> usize {
        self.required_prefix_len
    }

    pub(crate) const fn first_useful_prefix_len(&self) -> usize {
        self.first_useful_prefix_len
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedDatasetDemandPlan {
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) requirements: PreparedDemandRequirements,
    pub(crate) payload_bytes: u64,
    pub(crate) playback_downshifted: bool,
    pub(crate) covers_full_volume: bool,
    pub(crate) primary_resource_count: usize,
    pub(crate) playback_resource_count: usize,
}

impl PreparedDatasetDemandPlan {
    fn from_plan_accounted(
        plan: DatasetDemandPlan,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        let requirements = PreparedDemandRequirements::from_ranked_accounted(
            plan.resources,
            plan.primary_resource_count,
            reservations,
        )?;
        Ok(Self {
            layer_scales: plan.layer_scales,
            requirements,
            payload_bytes: plan.payload_bytes,
            playback_downshifted: plan.playback_downshifted,
            covers_full_volume: plan.covers_full_volume,
            primary_resource_count: plan.primary_resource_count,
            playback_resource_count: plan.playback_resource_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedProgressiveDatasetDemandPlan {
    pub(crate) ideal_layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) target: PreparedDatasetDemandPlan,
    pub(crate) coarse: Option<PreparedDatasetDemandPlan>,
    pub(crate) navigation_candidates: Vec<PreparedDatasetDemandPlan>,
    pub(crate) reuse_envelope: Option<SemanticCameraReuseEnvelope>,
    pub(crate) playback_timepoint_count: usize,
}

impl PreparedProgressiveDatasetDemandPlan {
    pub(crate) fn from_planning_accounted(
        planning: ProgressiveDatasetDemandPlanning,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<(Self, usize)> {
        let ProgressiveDatasetDemandPlanning {
            plan,
            playback_timepoint_count,
            candidates_visited,
            reuse_envelope,
            scratch_charges,
        } = planning;
        let ProgressiveDatasetDemandPlan {
            ideal_layer_scales,
            target,
            coarse,
            navigation_candidates,
        } = plan;
        let prepared = Self {
            ideal_layer_scales,
            target: PreparedDatasetDemandPlan::from_plan_accounted(target, reservations)?,
            coarse: coarse
                .map(|plan| PreparedDatasetDemandPlan::from_plan_accounted(plan, reservations))
                .transpose()?,
            navigation_candidates: navigation_candidates
                .into_iter()
                .map(|plan| PreparedDatasetDemandPlan::from_plan_accounted(plan, reservations))
                .collect::<anyhow::Result<Vec<_>>>()?,
            reuse_envelope,
            playback_timepoint_count,
        };
        drop(scratch_charges);
        Ok((prepared, candidates_visited))
    }
}

pub(crate) fn key_array_bytes(length: usize) -> anyhow::Result<u64> {
    let bytes = length
        .checked_mul(std::mem::size_of::<BrickKey>())
        .ok_or_else(|| anyhow::anyhow!("prepared key-array byte accounting overflow"))?;
    u64::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("prepared key-array byte accounting exceeds u64"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedDemandPlanError {
    DuplicateRequirement,
    DuplicateAdmittedRequirement,
    RequiredPrefixOutOfBounds,
    RenderApi(RenderApiError),
}

impl fmt::Display for PreparedDemandPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRequirement => {
                formatter.write_str("prepared demand contains a duplicate requirement")
            }
            Self::DuplicateAdmittedRequirement => formatter
                .write_str("prepared demand contains a duplicate admitted-prefix requirement"),
            Self::RequiredPrefixOutOfBounds => formatter
                .write_str("prepared demand required prefix exceeds its ranked resource body"),
            Self::RenderApi(error) => write!(formatter, "prepared render body is invalid: {error}"),
        }
    }
}

impl Error for PreparedDemandPlanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RenderApi(error) => Some(error),
            Self::DuplicateRequirement
            | Self::DuplicateAdmittedRequirement
            | Self::RequiredPrefixOutOfBounds => None,
        }
    }
}

impl From<RenderApiError> for PreparedDemandPlanError {
    fn from(error: RenderApiError) -> Self {
        Self::RenderApi(error)
    }
}

/// Exact grid-space support needed by one layer's sampling and shading.
/// Smooth interpolation contributes its upper tap and central-difference ISO
/// lighting contributes one tap on either side; together they reach two
/// voxels across a semantic-brick boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SamplingFootprintClass {
    Exact,
    OneVoxel,
    TwoVoxels,
}

impl SamplingFootprintClass {
    pub(crate) const fn halo_voxels(self) -> u64 {
        match self {
            Self::Exact => 0,
            Self::OneVoxel => 1,
            Self::TwoVoxels => 2,
        }
    }
}

pub(crate) fn sampling_footprint_class(render_state: RenderState) -> SamplingFootprintClass {
    let smooth = render_state.sampling_policy() == SamplingPolicy::SmoothLinear;
    let gradient = render_state.iso_parameters().is_some_and(|parameters| {
        parameters.shading_policy() == IsoShadingPolicy::GradientLighting
    });
    match (smooth, gradient) {
        (false, false) => SamplingFootprintClass::Exact,
        (true, true) => SamplingFootprintClass::TwoVoxels,
        (true, false) | (false, true) => SamplingFootprintClass::OneVoxel,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanLimits {
    pub(crate) max_candidates_per_layer: usize,
    pub(crate) max_resources: usize,
    /// Total payload residency allowed for the semantic cohort on the GPU.
    /// CPU decode residency is a separate bounded streaming working set and
    /// must never silently force a coarser semantic scale.
    pub(crate) max_gpu_payload_bytes: u64,
    /// Playback is the exceptional full-volume multi-timepoint cohort. Its
    /// decoded bodies remain CPU-authoritative until the window rolls, so the
    /// exact aggregate must also fit the dataset CPU ledger.
    pub(crate) max_playback_decoded_bytes: u64,
}

impl DatasetDemandPlanLimits {
    pub(crate) const fn new(
        max_candidates_per_layer: usize,
        max_resources: usize,
        max_gpu_payload_bytes: u64,
    ) -> Self {
        Self {
            max_candidates_per_layer,
            max_resources,
            max_gpu_payload_bytes,
            max_playback_decoded_bytes: u64::MAX,
        }
    }

    pub(crate) const fn with_playback_decoded_capacity(mut self, bytes: u64) -> Self {
        self.max_playback_decoded_bytes = bytes;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanCapacityError {
    limits: DatasetDemandPlanLimits,
    uniform_scale: Option<ScaleLevel>,
    target: &'static str,
    excess: Option<DatasetDemandCapacityExcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatasetDemandCapacityDimension {
    Candidates,
    Resources,
    GpuPayloadBytes,
    PlaybackDecodedBytes,
    ScratchBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DatasetDemandCapacityExcess {
    dimension: DatasetDemandCapacityDimension,
    required: u64,
    available: u64,
}

impl DatasetDemandPlanCapacityError {
    pub(crate) const fn is_gpu_payload_capacity(self) -> bool {
        matches!(
            self.excess,
            Some(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                ..
            })
        )
    }

    const fn at_selected_scale_with_excess(
        limits: DatasetDemandPlanLimits,
        target: &'static str,
        uniform_scale: Option<ScaleLevel>,
        excess: DatasetDemandCapacityExcess,
    ) -> Self {
        Self {
            limits,
            uniform_scale,
            target,
            excess: Some(excess),
        }
    }

    pub(crate) fn for_global_usage(
        limits: DatasetDemandPlanLimits,
        target: &'static str,
        uniform_scale: Option<ScaleLevel>,
        required_resources: usize,
        required_payload_bytes: u64,
    ) -> Self {
        let excess = if required_resources > limits.max_resources {
            Some(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Resources,
                required: u64::try_from(required_resources).unwrap_or(u64::MAX),
                available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
            })
        } else if required_payload_bytes > limits.max_gpu_payload_bytes {
            Some(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                required: required_payload_bytes,
                available: limits.max_gpu_payload_bytes,
            })
        } else {
            None
        };
        Self {
            limits,
            uniform_scale,
            target,
            excess,
        }
    }

    fn for_playback_decoded_usage(
        limits: DatasetDemandPlanLimits,
        target: &'static str,
        uniform_scale: Option<ScaleLevel>,
        required_bytes: u64,
    ) -> Self {
        Self {
            limits,
            uniform_scale,
            target,
            excess: Some(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::PlaybackDecodedBytes,
                required: required_bytes,
                available: limits.max_playback_decoded_bytes,
            }),
        }
    }
}

impl fmt::Display for DatasetDemandPlanCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(scale) = self.uniform_scale {
            write!(
                formatter,
                "{} minimum navigation scale s{} ",
                self.target,
                scale.get()
            )?;
        } else {
            write!(
                formatter,
                "{} minimum navigation configuration ",
                self.target
            )?;
        }
        if let Some(excess) = self.excess {
            let dimension = match excess.dimension {
                DatasetDemandCapacityDimension::Candidates => "planning candidates",
                DatasetDemandCapacityDimension::Resources => "resources",
                DatasetDemandCapacityDimension::GpuPayloadBytes => "GPU payload bytes",
                DatasetDemandCapacityDimension::PlaybackDecodedBytes => {
                    "playback decoded-residency bytes"
                }
                DatasetDemandCapacityDimension::ScratchBytes => "planning scratch bytes",
            };
            return write!(
                formatter,
                "requires {} {}, but the renderer-global capacity provides {}",
                excess.required, dimension, excess.available
            );
        }
        write!(
            formatter,
            "exceeded one of the bounded planning limits: {} resources, {} GPU payload bytes, or {} candidates per visible layer",
            self.limits.max_resources,
            self.limits.max_gpu_payload_bytes,
            self.limits.max_candidates_per_layer,
        )
    }
}

impl Error for DatasetDemandPlanCapacityError {}

enum PlanAttemptError {
    Capacity(DatasetDemandCapacityExcess),
    Cancelled,
    Other(anyhow::Error),
}

type PlanAttemptResult<T> = Result<T, PlanAttemptError>;

struct PlanAccumulator {
    resources: Vec<BrickKey>,
    layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    payload_bytes: u64,
    primary_payload_bytes: u64,
    resource_counts_by_layer: BTreeMap<LogicalLayerKey, usize>,
    primary_resource_counts_by_layer: BTreeMap<LogicalLayerKey, usize>,
    candidates_visited: usize,
    guard_retained: bool,
    plane_reuse_guards: Vec<SemanticPlaneLayerReuseGuard>,
    scratch_charges: Vec<Box<dyn CpuByteLease>>,
}

impl Default for PlanAccumulator {
    fn default() -> Self {
        Self {
            resources: Vec::new(),
            layer_scales: BTreeMap::new(),
            payload_bytes: 0,
            primary_payload_bytes: 0,
            resource_counts_by_layer: BTreeMap::new(),
            primary_resource_counts_by_layer: BTreeMap::new(),
            candidates_visited: 0,
            guard_retained: true,
            plane_reuse_guards: Vec::new(),
            scratch_charges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct SelectedCurrent3dLevels {
    camera: CameraFrame,
    layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    ideal_layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    playback_downshifted: bool,
}

/// A fixed one-sixteenth screen margin is large enough to absorb a short
/// interaction burst while keeping guard-only I/O a small second tier. It is
/// intentionally not adaptive: failed containment simply starts one new
/// exact plan.
const CAMERA_REUSE_GUARD_SCALE: f64 = 17.0 / 16.0;

fn expanded_camera_guard(camera: CameraView) -> anyhow::Result<CameraView> {
    let orthographic_scale = if camera.projection() == Projection::Orthographic {
        camera.orthographic_world_per_screen_point() * CAMERA_REUSE_GUARD_SCALE
    } else {
        camera.orthographic_world_per_screen_point()
    };
    let perspective_focal = if camera.projection() == Projection::Perspective {
        camera.perspective_focal_length_screen_points() / CAMERA_REUSE_GUARD_SCALE
    } else {
        camera.perspective_focal_length_screen_points()
    };
    Ok(CameraView::new(
        camera.projection(),
        camera.target(),
        camera.orientation(),
        orthographic_scale,
        perspective_focal,
        camera.perspective_view_distance_world(),
    )?)
}

#[cfg(test)]
pub(crate) fn plan_current_3d(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
) -> anyhow::Result<DatasetDemandPlan> {
    let selected = select_current_3d_levels(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
    )?;
    let mut never_cancelled = || false;
    let adaptive = plan_adaptive_current_3d_exact(
        catalog,
        view,
        selected,
        viewport,
        limits,
        playback_active,
        &[],
        0,
        &[],
        None,
        None,
        &mut never_cancelled,
    )?
    .expect("the synchronous planner cannot be cancelled");
    Ok(adaptive.target.plan)
}

fn select_current_3d_levels(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
) -> anyhow::Result<SelectedCurrent3dLevels> {
    let _ = limits;
    projected_current_3d_levels(catalog, view, presentation, viewport, playback_active)
}

pub(crate) fn current_3d_projected_layer_scales(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    playback_active: bool,
) -> anyhow::Result<BTreeMap<LogicalLayerKey, ScaleLevel>> {
    projected_current_3d_levels(catalog, view, presentation, viewport, playback_active)
        .map(|selected| selected.ideal_layer_scales)
}

pub(crate) fn cross_section_projected_layer_scales(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    presentation: PresentationViewport,
    viewport: RenderExtent,
) -> anyhow::Result<BTreeMap<LogicalLayerKey, ScaleLevel>> {
    let mut layer_scales = BTreeMap::new();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        layer_scales.insert(
            key,
            select_cross_section_level(
                layer,
                *view.cross_section(),
                panel,
                presentation,
                viewport,
            )?,
        );
    }
    Ok(layer_scales)
}

fn projected_current_3d_levels(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    playback_active: bool,
) -> anyhow::Result<SelectedCurrent3dLevels> {
    let camera = CameraFrame::new(*view.camera(), presentation)?;
    let four_panel_playback =
        playback_active && view.layout() == mirante4d_domain::ViewerLayout::FourPanel;
    let mut layer_scales = BTreeMap::new();
    let mut ideal_layer_scales = BTreeMap::new();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        let ideal = if four_panel_playback {
            layer
                .scales()
                .map(|scale| scale.level())
                .min()
                .ok_or_else(|| anyhow::anyhow!("a visible layer has no catalog scale"))?
        } else {
            select_volume_level(layer, camera, viewport)?
        };
        ideal_layer_scales.insert(key, ideal);
        layer_scales.insert(key, ideal);
    }
    Ok(SelectedCurrent3dLevels {
        camera,
        layer_scales,
        ideal_layer_scales,
        playback_downshifted: false,
    })
}

struct AdaptiveCurrent3dPlanning {
    target: DatasetDemandPlanning,
    floor: DatasetDemandPlanning,
    navigation_candidates: Vec<DatasetDemandPlan>,
    playback_timepoint_count: usize,
}

pub(crate) fn coarsest_visible_layer_scales(
    catalog: &DatasetCatalog,
    view: &ViewState,
) -> anyhow::Result<BTreeMap<LogicalLayerKey, ScaleLevel>> {
    view.layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|view_layer| {
            let key = view_layer.layer_key();
            let layer = catalog.layer(key).ok_or_else(|| {
                anyhow::anyhow!(
                    "visible layer {} is absent from the dataset catalog",
                    key.ordinal()
                )
            })?;
            let coarsest = layer
                .scales()
                .map(|scale| scale.level())
                .max()
                .expect("DatasetLayer always has at least one scale");
            Ok((key, coarsest))
        })
        .collect()
}

/// Returns a scalar only when the complete visible-layer configuration is
/// nonempty and uniform. Rendering authority always remains the input map.
pub(crate) fn uniform_layer_scale(
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
) -> Option<ScaleLevel> {
    let mut scales = layer_scales.values().copied();
    let first = scales.next()?;
    scales.all(|scale| scale == first).then_some(first)
}

fn next_finer_catalog_level(
    catalog: &DatasetCatalog,
    layer: LogicalLayerKey,
    current: ScaleLevel,
    ideal: ScaleLevel,
) -> anyhow::Result<Option<ScaleLevel>> {
    let layer = catalog.layer(layer).ok_or_else(|| {
        anyhow::anyhow!(
            "visible layer {} is absent from the dataset catalog",
            layer.ordinal()
        )
    })?;
    Ok(layer
        .scales()
        .map(|scale| scale.level())
        .filter(|level| *level >= ideal && *level < current)
        .max())
}

fn refinement_distance(
    catalog: &DatasetCatalog,
    layer: LogicalLayerKey,
    from: ScaleLevel,
    ideal: ScaleLevel,
) -> anyhow::Result<u64> {
    let mut current = from;
    let mut distance = 0_u64;
    while let Some(finer) = next_finer_catalog_level(catalog, layer, current, ideal)? {
        current = finer;
        distance = distance.saturating_add(1);
    }
    if current != ideal {
        anyhow::bail!(
            "layer {} cannot refine from s{} to ideal s{} through its catalog",
            layer.ordinal(),
            from.get(),
            ideal.get(),
        );
    }
    Ok(distance)
}

/// Selects one deterministic max-min refinement step. Authored visible order
/// breaks equal normalized-progress ties. A capacity-refused step remains
/// blocked for this bounded selection pass so an irregular catalog cannot
/// repeatedly trigger the same exact planning work or turn selection into a
/// combinatorial joint-map search.
fn next_fair_refinement(
    catalog: &DatasetCatalog,
    view: &ViewState,
    floor_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    current_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    ideal_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    blocked: &BTreeSet<LogicalLayerKey>,
) -> anyhow::Result<Option<(LogicalLayerKey, ScaleLevel)>> {
    let mut selected: Option<(LogicalLayerKey, ScaleLevel, u64, u64)> = None;
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let layer = view_layer.layer_key();
        if blocked.contains(&layer) {
            continue;
        }
        let floor = floor_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!("visible layer {} has no refinement floor", layer.ordinal())
        })?;
        let current = current_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} has no current refinement",
                layer.ordinal()
            )
        })?;
        let ideal = ideal_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!("visible layer {} has no ideal refinement", layer.ordinal())
        })?;
        let Some(finer) = next_finer_catalog_level(catalog, layer, current, ideal)? else {
            continue;
        };
        let total = refinement_distance(catalog, layer, floor, ideal)?.max(1);
        let remaining = refinement_distance(catalog, layer, current, ideal)?;
        let completed = total.saturating_sub(remaining);
        let replace = selected
            .as_ref()
            .is_none_or(|(_, _, selected_completed, selected_total)| {
                completed.saturating_mul(*selected_total) < selected_completed.saturating_mul(total)
            });
        if replace {
            selected = Some((layer, finer, completed, total));
        }
    }
    Ok(selected.map(|(layer, finer, _, _)| (layer, finer)))
}

/// Retains the established playback refinement order without making that
/// temporal policy authoritative for ordinary static viewing. When the
/// active analysis layer is visible it remains first, exactly as before this
/// static-viewer cut; a hidden active layer simply falls back to authored
/// visible order instead of making the render set invalid.
fn next_playback_refinement(
    catalog: &DatasetCatalog,
    view: &ViewState,
    current_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    ideal_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    blocked: &BTreeSet<LogicalLayerKey>,
) -> anyhow::Result<Option<(LogicalLayerKey, ScaleLevel)>> {
    let mut layer_priority = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.layer_key())
        .collect::<Vec<_>>();
    if let Some(active_index) = layer_priority
        .iter()
        .position(|layer| *layer == view.active_layer())
    {
        layer_priority.swap(0, active_index);
    }
    for layer in layer_priority {
        if blocked.contains(&layer) {
            continue;
        }
        let current = current_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} has no current playback refinement",
                layer.ordinal()
            )
        })?;
        let ideal = ideal_scales.get(&layer).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} has no ideal playback refinement",
                layer.ordinal()
            )
        })?;
        if let Some(finer) = next_finer_catalog_level(catalog, layer, current, ideal)? {
            return Ok(Some((layer, finer)));
        }
    }
    Ok(None)
}

fn current_plan_union_usage(
    catalog: &DatasetCatalog,
    obligated_resources: &[BrickKey],
    floor: &DatasetDemandPlan,
    target: &DatasetDemandPlan,
    playback_timepoints: &[TimeIndex],
) -> anyhow::Result<(usize, u64, u64)> {
    let mut keys = BTreeSet::new();
    keys.extend(obligated_resources.iter().copied());
    keys.extend(floor.resources.iter().copied());
    keys.extend(target.resources.iter().copied());
    for &timepoint in playback_timepoints {
        keys.extend(
            target
                .resources
                .iter()
                .take(target.playback_resource_count)
                .map(|key| {
                    BrickKey::new(
                        key.identity(),
                        key.layer(),
                        timepoint,
                        key.scale(),
                        key.region(),
                    )
                }),
        );
    }
    let mut payload_bytes = 0_u64;
    let mut decoded_bytes = 0_u64;
    for key in &keys {
        let descriptor = catalog.resource_payload_descriptor(*key)?;
        payload_bytes = payload_bytes
            .checked_add(mirante4d_render_wgpu::payload_allocation_bytes(descriptor)?)
            .ok_or_else(|| anyhow::anyhow!("global GPU payload accounting overflow"))?;
        decoded_bytes = decoded_bytes
            .checked_add(descriptor.byte_len())
            .ok_or_else(|| anyhow::anyhow!("global decoded payload accounting overflow"))?;
    }
    Ok((keys.len(), payload_bytes, decoded_bytes))
}

fn current_plan_union_fits(
    catalog: &DatasetCatalog,
    obligated_resources: &[BrickKey],
    floor: &DatasetDemandPlan,
    target: &DatasetDemandPlan,
    playback_timepoints: &[TimeIndex],
    limits: DatasetDemandPlanLimits,
) -> anyhow::Result<bool> {
    let (resources, payload_bytes, decoded_bytes) = current_plan_union_usage(
        catalog,
        obligated_resources,
        floor,
        target,
        playback_timepoints,
    )?;
    Ok(resources <= limits.max_resources
        && payload_bytes <= limits.max_gpu_payload_bytes
        && decoded_bytes <= limits.max_playback_decoded_bytes)
}

fn retryable_refinement_capacity(error: &PlanAttemptError) -> bool {
    match error {
        PlanAttemptError::Capacity(_) => true,
        PlanAttemptError::Other(error) => {
            error.downcast_ref::<CpuLedgerError>().is_some()
                || error
                    .downcast_ref::<SemanticPlanError>()
                    .is_some_and(|error| matches!(error, SemanticPlanError::ScratchCapacity(_)))
        }
        PlanAttemptError::Cancelled => false,
    }
}

fn plan_attempt_from_scratch_error(error: anyhow::Error) -> PlanAttemptError {
    if let Some(CpuLedgerError::CapacityExceeded {
        requested_bytes,
        available_bytes,
        ..
    }) = error.downcast_ref::<CpuLedgerError>()
    {
        PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::ScratchBytes,
            required: *requested_bytes,
            available: *available_bytes,
        })
    } else {
        PlanAttemptError::Other(error)
    }
}

fn adopt_navigation_candidate_baseline(
    catalog: &DatasetCatalog,
    view: &ViewState,
    baseline: &NavigationCandidateBaseline,
    limits: DatasetDemandPlanLimits,
    playback_downshifted: bool,
    require_terminal: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
) -> PlanAttemptResult<Option<DatasetDemandPlanning>> {
    let visible_layer_count = view.layers().iter().filter(|layer| layer.visible()).count();
    if baseline.body.ranked().is_empty()
        || baseline.body.ranked().len() != baseline.body.canonical().len()
        || baseline.layer_scales.len() != visible_layer_count
    {
        return Ok(None);
    }

    let mut expected_resource_count = 0_usize;
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let layer_key = view_layer.layer_key();
        let Some(level) = baseline.layer_scales.get(&layer_key).copied() else {
            return Ok(None);
        };
        let Some(scale) = catalog
            .layer(layer_key)
            .and_then(|layer| layer.scale(level))
        else {
            return Ok(None);
        };
        let terminal = catalog
            .layer(layer_key)
            .expect("the selected scale proved that the layer exists")
            .scales()
            .map(|scale| scale.level())
            .max()
            .expect("a dataset layer has at least one scale");
        if require_terminal && level != terminal {
            // The first ladder rung is geometry-derived. A formerly installed
            // finer full-volume body may remain ordinary target residency, but
            // it cannot be re-adopted as the unconditional safety floor.
            return Ok(None);
        }
        let dimensions = SemanticTileGrid::new(scale.shape())
            .grid_shape()
            .dimensions();
        let layer_count_u64 = dimensions.into_iter().try_fold(1_u64, |total, value| {
            total.checked_mul(value).ok_or_else(|| {
                PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                    dimension: DatasetDemandCapacityDimension::Candidates,
                    required: u64::MAX,
                    available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
                })
            })
        })?;
        let layer_count = usize::try_from(layer_count_u64).map_err(|_| {
            PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Candidates,
                required: layer_count_u64,
                available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
            })
        })?;
        if layer_count > limits.max_candidates_per_layer {
            return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Candidates,
                required: layer_count_u64,
                available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
            }));
        }
        if baseline
            .body
            .ranked()
            .iter()
            .filter(|key| key.layer() == layer_key)
            .count()
            != layer_count
        {
            return Ok(None);
        }
        expected_resource_count = expected_resource_count
            .checked_add(layer_count)
            .ok_or_else(|| {
                PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                    dimension: DatasetDemandCapacityDimension::Resources,
                    required: u64::MAX,
                    available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
                })
            })?;
    }
    if expected_resource_count != baseline.body.ranked().len() {
        return Ok(None);
    }
    if expected_resource_count > limits.max_resources {
        return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::Resources,
            required: u64::try_from(expected_resource_count).unwrap_or(u64::MAX),
            available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
        }));
    }

    let mut payload_bytes = 0_u64;
    for key in baseline.body.ranked().iter().copied() {
        let Some(level) = baseline.layer_scales.get(&key.layer()).copied() else {
            return Ok(None);
        };
        if key.identity() != catalog.resource_identity()
            || key.timepoint() != view.timepoint()
            || key.scale() != level
            || catalog.validate_resource_key(key).is_err()
        {
            return Ok(None);
        }
        let scale = catalog
            .layer(key.layer())
            .and_then(|layer| layer.scale(level))
            .expect("the visible-layer validation proved this scale exists");
        let origin = key.region().origin();
        if origin
            .into_iter()
            .any(|component| !component.is_multiple_of(SEMANTIC_TILE_SIDE))
        {
            return Ok(None);
        }
        let grid = SemanticTileGrid::new(scale.shape());
        let index = SemanticTileIndex {
            z: origin[0] / SEMANTIC_TILE_SIDE,
            y: origin[1] / SEMANTIC_TILE_SIDE,
            x: origin[2] / SEMANTIC_TILE_SIDE,
        };
        if grid.region(index).ok() != Some(key.region()) {
            return Ok(None);
        }
        let descriptor = catalog
            .resource_payload_descriptor(key)
            .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
        let resource_bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)
            .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
        payload_bytes = payload_bytes.checked_add(resource_bytes).ok_or_else(|| {
            PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                required: u64::MAX,
                available: limits.max_gpu_payload_bytes,
            })
        })?;
    }
    if payload_bytes != baseline.planned_payload_bytes {
        return Err(PlanAttemptError::Other(anyhow::anyhow!(
            "installed navigation-candidate payload accounting changed from {} to {} bytes",
            baseline.planned_payload_bytes,
            payload_bytes
        )));
    }
    if payload_bytes > limits.max_gpu_payload_bytes {
        return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
            required: payload_bytes,
            available: limits.max_gpu_payload_bytes,
        }));
    }

    let scratch_charge = reserve_key_scratch(scratch_ledger, expected_resource_count)
        .map_err(plan_attempt_from_scratch_error)?;
    let layer_scales = baseline.layer_scales.as_ref().clone();
    Ok(Some(DatasetDemandPlanning {
        plan: DatasetDemandPlan {
            layer_scales,
            resources: baseline.body.ranked().to_vec(),
            payload_bytes,
            playback_downshifted,
            covers_full_volume: true,
            primary_resource_count: expected_resource_count,
            playback_resource_count: expected_resource_count,
        },
        candidates_visited: 0,
        guard_retained: false,
        plane_reuse_envelope: None,
        scratch_charges: scratch_charge.into_iter().collect(),
    }))
}

#[allow(clippy::too_many_arguments)]
fn adopt_navigation_ladder_baseline(
    catalog: &DatasetCatalog,
    view: &ViewState,
    baseline: &NavigationLadderBaseline,
    limits: DatasetDemandPlanLimits,
    playback_downshifted: bool,
    playback_active: bool,
    playback_timepoints: &[TimeIndex],
    obligated_resources: &[BrickKey],
    scratch_ledger: Option<&dyn CpuByteLedger>,
) -> PlanAttemptResult<Option<(DatasetDemandPlanning, Vec<DatasetDemandPlan>)>> {
    let Some(terminal_baseline) = baseline.terminal() else {
        return Ok(None);
    };
    let Some(mut aggregate) = adopt_navigation_candidate_baseline(
        catalog,
        view,
        terminal_baseline,
        limits,
        playback_downshifted,
        true,
        scratch_ledger,
    )?
    else {
        return Ok(None);
    };
    let terminal_plan = aggregate.plan.clone();
    let (tail_resource_limit, tail_payload_limit) = navigation_tail_limits(limits, &terminal_plan);
    let mut candidates = vec![terminal_plan];
    let mut previous_scales = aggregate.plan.layer_scales.clone();

    for candidate_baseline in baseline.candidates.iter().skip(1) {
        let Some(mut candidate) = (match adopt_navigation_candidate_baseline(
            catalog,
            view,
            candidate_baseline,
            limits,
            playback_downshifted,
            false,
            scratch_ledger,
        ) {
            Ok(candidate) => candidate,
            // A finer installed rung is optional under the current limits.
            // Capacity contraction keeps the validated prefix rather than
            // invalidating the mandatory terminal body.
            Err(PlanAttemptError::Capacity(_)) => break,
            Err(error) => return Err(error),
        }) else {
            // A malformed or stale optional suffix is never partially
            // skipped: doing so would turn a contiguous scale ladder into an
            // unreported discontinuity.
            break;
        };

        let playback_lockstep = playback_active;
        let mut advanced_layers = 0_usize;
        let mut adjacent = true;
        for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
            let layer = view_layer.layer_key();
            let Some(previous) = previous_scales.get(&layer).copied() else {
                adjacent = false;
                break;
            };
            let actual = candidate.plan.layer_scales.get(&layer).copied();
            let expected = next_finer_catalog_level(catalog, layer, previous, ScaleLevel::BASE)
                .map_err(PlanAttemptError::Other)?
                .unwrap_or(previous);
            if playback_lockstep {
                if actual != Some(expected) {
                    adjacent = false;
                    break;
                }
                advanced_layers = advanced_layers.saturating_add(usize::from(expected != previous));
            } else if actual == Some(previous) {
                continue;
            } else {
                if actual != Some(expected) {
                    adjacent = false;
                    break;
                }
                advanced_layers = advanced_layers.saturating_add(1);
            }
        }
        if !adjacent || advanced_layers == 0 || (!playback_lockstep && advanced_layers != 1) {
            // A static ladder advances one visible layer per rung, while
            // playback's retained contract advances every eligible layer
            // coherently. Keeping only an incompatible ladder's terminal rung
            // would make the new policy appear valid at the emergency floor
            // and prevent its proper ladder from being rebuilt. Reject the
            // structurally incompatible baseline as a whole in either
            // transition direction.
            return Ok(None);
        }

        let scratch_count = aggregate
            .plan
            .resources
            .len()
            .checked_add(candidate.plan.resources.len())
            .ok_or_else(|| {
                PlanAttemptError::Other(anyhow::anyhow!(
                    "installed navigation-ladder scratch count overflow"
                ))
            })?;
        let membership_charge = reserve_key_scratch(scratch_ledger, scratch_count)
            .map_err(plan_attempt_from_scratch_error)?;
        let mut membership = aggregate.plan.resources.clone();
        membership.sort_unstable();
        membership.dedup();
        let missing = candidate
            .plan
            .resources
            .iter()
            .copied()
            .filter(|key| membership.binary_search(key).is_err())
            .collect::<Vec<_>>();
        let next_resource_count = aggregate
            .plan
            .resources
            .len()
            .checked_add(missing.len())
            .ok_or_else(|| {
                PlanAttemptError::Other(anyhow::anyhow!(
                    "installed navigation-ladder resource count overflow"
                ))
            })?;
        let missing_payload_bytes = missing.iter().try_fold(0_u64, |total, key| {
            let descriptor = catalog
                .resource_payload_descriptor(*key)
                .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
            let bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)
                .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
            total.checked_add(bytes).ok_or_else(|| {
                PlanAttemptError::Other(anyhow::anyhow!(
                    "installed navigation-ladder payload accounting overflow"
                ))
            })
        })?;
        let next_payload_bytes = aggregate
            .plan
            .payload_bytes
            .checked_add(missing_payload_bytes)
            .ok_or_else(|| {
                PlanAttemptError::Other(anyhow::anyhow!(
                    "installed navigation-ladder payload accounting overflow"
                ))
            })?;
        if next_resource_count > tail_resource_limit || next_payload_bytes > tail_payload_limit {
            drop(membership_charge);
            break;
        }

        let previous_resource_count = aggregate.plan.resources.len();
        let previous_payload_bytes = aggregate.plan.payload_bytes;
        aggregate.plan.resources.extend(missing);
        aggregate.plan.payload_bytes = next_payload_bytes;
        let (global_resources, global_payload_bytes, global_decoded_bytes) =
            current_plan_union_usage(
                catalog,
                obligated_resources,
                &aggregate.plan,
                &aggregate.plan,
                playback_timepoints,
            )
            .map_err(PlanAttemptError::Other)?;
        if global_resources > limits.max_resources
            || global_payload_bytes > limits.max_gpu_payload_bytes
            || global_decoded_bytes > limits.max_playback_decoded_bytes
        {
            aggregate.plan.resources.truncate(previous_resource_count);
            aggregate.plan.payload_bytes = previous_payload_bytes;
            drop(membership_charge);
            break;
        }
        drop(membership_charge);
        previous_scales = candidate.plan.layer_scales.clone();
        candidates.push(candidate.plan.clone());
        aggregate
            .scratch_charges
            .append(&mut candidate.scratch_charges);
    }

    Ok(Some((aggregate, candidates)))
}

fn plan_full_volume_navigation_floor(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    limits: DatasetDemandPlanLimits,
    playback_downshifted: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> PlanAttemptResult<DatasetDemandPlanning> {
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        if cancelled() {
            return Err(PlanAttemptError::Cancelled);
        }
        let layer_key = view_layer.layer_key();
        let layer = catalog.layer(layer_key).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                layer_key.ordinal()
            ))
        })?;
        let level = layer_scales.get(&layer_key).copied().ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} has no navigation-floor scale",
                layer_key.ordinal()
            ))
        })?;
        let scale = layer.scale(level).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} has no navigation-floor scale {}",
                layer_key.ordinal(),
                level.get()
            ))
        })?;
        let grid = SemanticTileGrid::new(scale.shape());
        let dimensions = grid.grid_shape().dimensions();
        let candidate_count_u64 = dimensions.into_iter().try_fold(1_u64, |total, value| {
            total.checked_mul(value).ok_or_else(|| {
                PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                    dimension: DatasetDemandCapacityDimension::Candidates,
                    required: u64::MAX,
                    available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
                })
            })
        })?;
        let candidate_count = usize::try_from(candidate_count_u64).map_err(|_| {
            PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Candidates,
                required: candidate_count_u64,
                available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
            })
        })?;
        if candidate_count > limits.max_candidates_per_layer {
            return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Candidates,
                required: candidate_count_u64,
                available: u64::try_from(limits.max_candidates_per_layer).unwrap_or(u64::MAX),
            }));
        }
        let result_charge = reserve_key_scratch(scratch_ledger, candidate_count)
            .map_err(plan_attempt_from_scratch_error)?;
        let mut regions = Vec::with_capacity(candidate_count);
        for (offset, index) in grid.indices().enumerate() {
            if offset.is_multiple_of(256) && cancelled() {
                return Err(PlanAttemptError::Cancelled);
            }
            regions.push(
                grid.region(index)
                    .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?,
            );
        }
        plan.candidates_visited = plan.candidates_visited.saturating_add(candidate_count);
        append_layer_resources(
            catalog,
            view,
            layer_key,
            level,
            regions,
            candidate_count,
            &mut plan,
            limits,
            cancelled,
        )?;
        if let Some(charge) = result_charge {
            plan.scratch_charges.push(charge);
        }
    }
    if let Some(charge) = reserve_interleave_scratch(scratch_ledger, plan.resources.len())
        .map_err(plan_attempt_from_scratch_error)?
    {
        plan.scratch_charges.push(charge);
    }
    let primary_resource_count = plan.resources.len();
    plan.resources =
        interleave_visible_layers(view, plan.resources, &plan.primary_resource_counts_by_layer);
    Ok(DatasetDemandPlanning {
        candidates_visited: plan.candidates_visited,
        plan: DatasetDemandPlan {
            layer_scales: plan.layer_scales,
            resources: plan.resources,
            payload_bytes: plan.payload_bytes,
            playback_downshifted,
            covers_full_volume: true,
            primary_resource_count,
            playback_resource_count: primary_resource_count,
        },
        guard_retained: false,
        plane_reuse_envelope: None,
        scratch_charges: plan.scratch_charges,
    })
}

fn navigation_tail_limits(
    limits: DatasetDemandPlanLimits,
    terminal: &DatasetDemandPlan,
) -> (usize, u64) {
    let optional_resources = limits
        .max_resources
        .checked_div(NAVIGATION_TAIL_RESOURCE_DIVISOR)
        .unwrap_or(0)
        .min(NAVIGATION_TAIL_MAX_RESOURCES);
    let optional_payload_bytes = limits
        .max_gpu_payload_bytes
        .checked_div(NAVIGATION_TAIL_PAYLOAD_DIVISOR)
        .unwrap_or(0)
        .min(NAVIGATION_TAIL_MAX_PAYLOAD_BYTES);
    // The terminal body is the mandatory geometry-safe floor and is outside
    // the optional retention allowance. These are aggregate ceilings, so add
    // the bounded tail to the terminal rather than making the terminal consume
    // its own allowance.
    (
        terminal
            .resources
            .len()
            .saturating_add(optional_resources)
            .min(limits.max_resources),
        terminal
            .payload_bytes
            .saturating_add(optional_payload_bytes)
            .min(limits.max_gpu_payload_bytes),
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_full_volume_navigation_ladder(
    catalog: &DatasetCatalog,
    view: &ViewState,
    terminal: DatasetDemandPlanning,
    finest_layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    playback_timepoints: &[TimeIndex],
    obligated_resources: &[BrickKey],
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<(DatasetDemandPlanning, Vec<DatasetDemandPlan>)>> {
    let mut aggregate = terminal;
    let terminal_plan = aggregate.plan.clone();
    let (tail_resource_limit, tail_payload_limit) = navigation_tail_limits(limits, &terminal_plan);
    let mut candidates = vec![terminal_plan];
    let mut current_scales = aggregate.plan.layer_scales.clone();
    let floor_scales = current_scales.clone();
    let mut blocked = BTreeSet::new();

    loop {
        if cancelled() {
            return Ok(None);
        }
        let mut next_scales = current_scales.clone();
        let static_blocked_layer = if !playback_active {
            let Some((layer, finer)) = next_fair_refinement(
                catalog,
                view,
                &floor_scales,
                &current_scales,
                finest_layer_scales,
                &blocked,
            )?
            else {
                break;
            };
            next_scales.insert(layer, finer);
            Some(layer)
        } else {
            // A playback ladder remains the established coherent lockstep
            // sequence. Static max-min rungs must not change temporal quality
            // or memory policy.
            let mut advanced = false;
            for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
                let layer = view_layer.layer_key();
                let current = current_scales.get(&layer).copied().ok_or_else(|| {
                    anyhow::anyhow!(
                        "visible layer {} has no playback navigation scale",
                        layer.ordinal()
                    )
                })?;
                let finest = finest_layer_scales.get(&layer).copied().ok_or_else(|| {
                    anyhow::anyhow!(
                        "visible layer {} has no playback navigation quality ceiling",
                        layer.ordinal()
                    )
                })?;
                if let Some(finer) = next_finer_catalog_level(catalog, layer, current, finest)? {
                    next_scales.insert(layer, finer);
                    advanced = true;
                }
            }
            if !advanced {
                break;
            }
            None
        };

        let candidate = match plan_full_volume_navigation_floor(
            catalog,
            view,
            &next_scales,
            limits,
            aggregate.plan.playback_downshifted,
            scratch_ledger,
            cancelled,
        ) {
            Ok(candidate) => candidate,
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(PlanAttemptError::Capacity(_)) => {
                if let Some(layer) = static_blocked_layer {
                    blocked.insert(layer);
                    continue;
                }
                break;
            }
            Err(PlanAttemptError::Other(error)) => return Err(error),
        };
        aggregate.candidates_visited = aggregate
            .candidates_visited
            .saturating_add(candidate.candidates_visited);

        let scratch_count = aggregate
            .plan
            .resources
            .len()
            .checked_add(candidate.plan.resources.len())
            .ok_or_else(|| anyhow::anyhow!("navigation-ladder scratch count overflow"))?;
        let membership_charge = reserve_key_scratch(scratch_ledger, scratch_count)?;
        let mut membership = aggregate.plan.resources.clone();
        membership.sort_unstable();
        membership.dedup();
        let missing = candidate
            .plan
            .resources
            .iter()
            .copied()
            .filter(|key| membership.binary_search(key).is_err())
            .collect::<Vec<_>>();
        let next_resource_count = aggregate
            .plan
            .resources
            .len()
            .checked_add(missing.len())
            .ok_or_else(|| anyhow::anyhow!("navigation-ladder resource count overflow"))?;
        let missing_payload_bytes = missing.iter().try_fold(0_u64, |total, key| {
            let descriptor = catalog.resource_payload_descriptor(*key)?;
            total
                .checked_add(mirante4d_render_wgpu::payload_allocation_bytes(descriptor)?)
                .ok_or_else(|| anyhow::anyhow!("navigation-ladder payload accounting overflow"))
        })?;
        let next_payload_bytes = aggregate
            .plan
            .payload_bytes
            .checked_add(missing_payload_bytes)
            .ok_or_else(|| anyhow::anyhow!("navigation-ladder payload accounting overflow"))?;
        if next_resource_count > tail_resource_limit || next_payload_bytes > tail_payload_limit {
            drop(membership_charge);
            if let Some(layer) = static_blocked_layer {
                blocked.insert(layer);
                continue;
            }
            break;
        }

        let previous_resource_count = aggregate.plan.resources.len();
        let previous_payload_bytes = aggregate.plan.payload_bytes;
        aggregate.plan.resources.extend(missing);
        aggregate.plan.payload_bytes = next_payload_bytes;
        let (global_resources, global_payload_bytes, global_decoded_bytes) =
            current_plan_union_usage(
                catalog,
                obligated_resources,
                &aggregate.plan,
                &aggregate.plan,
                playback_timepoints,
            )?;
        if global_resources > limits.max_resources
            || global_payload_bytes > limits.max_gpu_payload_bytes
            || global_decoded_bytes > limits.max_playback_decoded_bytes
        {
            aggregate.plan.resources.truncate(previous_resource_count);
            aggregate.plan.payload_bytes = previous_payload_bytes;
            drop(membership_charge);
            if let Some(layer) = static_blocked_layer {
                blocked.insert(layer);
                continue;
            }
            break;
        }
        drop(membership_charge);
        candidates.push(candidate.plan.clone());
        aggregate.scratch_charges.extend(candidate.scratch_charges);
        current_scales = next_scales;
    }

    Ok(Some((aggregate, candidates)))
}

fn retain_navigation_ladder_in_target(
    catalog: &DatasetCatalog,
    floor: &DatasetDemandPlan,
    target: &mut DatasetDemandPlanning,
    limits: DatasetDemandPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
) -> PlanAttemptResult<()> {
    if target.plan.resources == floor.resources {
        return Ok(());
    }
    let membership_charge = reserve_key_scratch(scratch_ledger, target.plan.resources.len())
        .map_err(plan_attempt_from_scratch_error)?;
    let mut membership = target.plan.resources.clone();
    membership.sort_unstable();
    membership.dedup();
    let missing_floor_charge = reserve_key_scratch(scratch_ledger, floor.resources.len())
        .map_err(plan_attempt_from_scratch_error)?;
    let missing_floor = floor
        .resources
        .iter()
        .copied()
        .filter(|resource| membership.binary_search(resource).is_err())
        .collect::<Vec<_>>();
    drop(membership_charge);
    let combined_count = target
        .plan
        .resources
        .len()
        .checked_add(missing_floor.len())
        .ok_or_else(|| {
            PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Resources,
                required: u64::MAX,
                available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
            })
        })?;
    if combined_count > limits.max_resources {
        return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::Resources,
            required: u64::try_from(combined_count).unwrap_or(u64::MAX),
            available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
        }));
    }
    let charge = reserve_key_scratch(scratch_ledger, combined_count)
        .map_err(plan_attempt_from_scratch_error)?;
    target.plan.resources.extend(missing_floor);
    drop(missing_floor_charge);
    let payload_bytes = target.plan.resources.iter().try_fold(0_u64, |total, key| {
        let descriptor = catalog
            .resource_payload_descriptor(*key)
            .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
        let bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)
            .map_err(|error| PlanAttemptError::Other(anyhow::Error::new(error)))?;
        total.checked_add(bytes).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "navigation-ladder target payload accounting overflow"
            ))
        })
    })?;
    if payload_bytes > limits.max_gpu_payload_bytes {
        return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
            required: payload_bytes,
            available: limits.max_gpu_payload_bytes,
        }));
    }
    target.plan.payload_bytes = payload_bytes;
    if let Some(charge) = charge {
        target.scratch_charges.push(charge);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn plan_current_3d_configuration(
    catalog: &DatasetCatalog,
    view: &ViewState,
    camera: CameraFrame,
    priority_camera: Option<CameraFrame>,
    viewport: RenderExtent,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    limits: DatasetDemandPlanLimits,
    playback_downshifted: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> PlanAttemptResult<DatasetDemandPlanning> {
    plan_level(
        catalog,
        view,
        camera,
        priority_camera,
        viewport,
        layer_scales,
        limits,
        playback_downshifted,
        scratch_ledger,
        cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_adaptive_current_3d_exact(
    catalog: &DatasetCatalog,
    view: &ViewState,
    ideal: SelectedCurrent3dLevels,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    playback_timepoints: &[TimeIndex],
    playback_required_timepoint_count: usize,
    obligated_resources: &[BrickKey],
    navigation_ladder_baseline: Option<&NavigationLadderBaseline>,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<AdaptiveCurrent3dPlanning>> {
    if cancelled() {
        return Ok(None);
    }
    let playback_required_timepoint_count = if playback_timepoints.is_empty() {
        0
    } else {
        playback_required_timepoint_count
            .max(1)
            .min(playback_timepoints.len())
    };
    // Spatial quality is selected against the desired one-second runway when
    // the coarsest body can support it. The short startup runway is the
    // bounded fallback for a machine that cannot retain the full desired
    // window even at the terminal scale.
    let startup_timepoints = &playback_timepoints[..playback_required_timepoint_count];
    let baseline_ladder = navigation_ladder_baseline
        .map(|baseline| {
            adopt_navigation_ladder_baseline(
                catalog,
                view,
                baseline,
                limits,
                ideal.playback_downshifted,
                playback_active,
                startup_timepoints,
                obligated_resources,
                scratch_ledger,
            )
        })
        .transpose()
        .map_err(|error| match error {
            PlanAttemptError::Capacity(excess) => {
                DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                    limits,
                    "3D",
                    navigation_ladder_baseline
                        .and_then(NavigationLadderBaseline::terminal)
                        .and_then(|candidate| uniform_layer_scale(&candidate.layer_scales))
                        .or_else(|| uniform_layer_scale(&ideal.layer_scales)),
                    excess,
                )
                .into()
            }
            PlanAttemptError::Cancelled => anyhow::anyhow!("adaptive planning was cancelled"),
            PlanAttemptError::Other(error) => error,
        })?
        .flatten();
    let (floor, navigation_candidates) = if let Some(ladder) = baseline_ladder {
        ladder
    } else {
        let selected_scales = coarsest_visible_layer_scales(catalog, view)?;
        let terminal = match plan_full_volume_navigation_floor(
            catalog,
            view,
            &selected_scales,
            limits,
            ideal.playback_downshifted,
            scratch_ledger,
            cancelled,
        ) {
            Ok(planning) => planning,
            Err(PlanAttemptError::Capacity(excess)) => {
                return Err(
                    DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                        limits,
                        "3D",
                        uniform_layer_scale(&selected_scales),
                        excess,
                    )
                    .into(),
                );
            }
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(PlanAttemptError::Other(error)) => return Err(error),
        };
        let Some(ladder) = plan_full_volume_navigation_ladder(
            catalog,
            view,
            terminal,
            &ideal.layer_scales,
            limits,
            playback_active,
            startup_timepoints,
            obligated_resources,
            scratch_ledger,
            cancelled,
        )?
        else {
            return Ok(None);
        };
        ladder
    };
    let mut selected_scales = floor.plan.layer_scales.clone();
    let (floor_union_resources, floor_union_payload_bytes, floor_union_decoded_bytes) =
        current_plan_union_usage(
            catalog,
            obligated_resources,
            &floor.plan,
            &floor.plan,
            startup_timepoints,
        )?;
    if floor_union_resources > limits.max_resources
        || floor_union_payload_bytes > limits.max_gpu_payload_bytes
        || floor_union_decoded_bytes > limits.max_playback_decoded_bytes
    {
        let error = if floor_union_decoded_bytes > limits.max_playback_decoded_bytes {
            DatasetDemandPlanCapacityError::for_playback_decoded_usage(
                limits,
                "3D",
                uniform_layer_scale(&floor.plan.layer_scales),
                floor_union_decoded_bytes,
            )
        } else {
            DatasetDemandPlanCapacityError::for_global_usage(
                limits,
                "3D",
                uniform_layer_scale(&floor.plan.layer_scales),
                floor_union_resources,
                floor_union_payload_bytes,
            )
        };
        return Err(error.into());
    }

    // The installed navigation body is a terminal-first, bounded full-volume
    // ladder. Memory admission decides which rungs may be retained; the
    // presentation work controller independently decides which complete rung
    // may be shown during native-resolution input.

    let mut target = None;
    let mut selected_is_floor = true;
    let floor_scales = selected_scales.clone();
    let mut blocked = BTreeSet::new();
    loop {
        let refinement = if !playback_active {
            next_fair_refinement(
                catalog,
                view,
                &floor_scales,
                &selected_scales,
                &ideal.layer_scales,
                &blocked,
            )?
        } else {
            // Playback quality ordering is deliberately outside this static
            // refactor. Preserve its established active-first greedy order.
            next_playback_refinement(
                catalog,
                view,
                &selected_scales,
                &ideal.layer_scales,
                &blocked,
            )?
        };
        let Some((layer, finer)) = refinement else {
            break;
        };
        if cancelled() {
            return Ok(None);
        }
        let mut trial_scales = selected_scales.clone();
        trial_scales.insert(layer, finer);
        let mut trial = match plan_current_3d_configuration(
            catalog,
            view,
            ideal.camera,
            None,
            viewport,
            &trial_scales,
            limits,
            ideal.playback_downshifted,
            scratch_ledger,
            cancelled,
        ) {
            Ok(planning) => planning,
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(error) if retryable_refinement_capacity(&error) => {
                blocked.insert(layer);
                continue;
            }
            Err(PlanAttemptError::Other(error)) => return Err(error),
            Err(PlanAttemptError::Capacity(_)) => unreachable!("capacity was handled above"),
        };
        if trial.plan.primary_resource_count == 0 {
            blocked.insert(layer);
            continue;
        }
        if let Err(error) = retain_navigation_ladder_in_target(
            catalog,
            &floor.plan,
            &mut trial,
            limits,
            scratch_ledger,
        ) {
            if retryable_refinement_capacity(&error) {
                blocked.insert(layer);
                continue;
            }
            match error {
                PlanAttemptError::Cancelled => return Ok(None),
                PlanAttemptError::Other(error) => return Err(error),
                PlanAttemptError::Capacity(_) => {
                    unreachable!("retryable refinement capacity was handled above")
                }
            }
        }
        if !current_plan_union_fits(
            catalog,
            obligated_resources,
            &floor.plan,
            &trial.plan,
            &[],
            limits,
        )? {
            blocked.insert(layer);
            continue;
        }
        selected_scales = trial_scales;
        target = Some(trial);
        selected_is_floor = false;
    }
    let mut target = if selected_is_floor {
        DatasetDemandPlanning {
            plan: floor.plan.clone(),
            candidates_visited: 0,
            guard_retained: false,
            plane_reuse_envelope: None,
            scratch_charges: Vec::new(),
        }
    } else {
        target.expect("a non-floor adaptive selection owns its accepted plan")
    };
    let mut playback_timepoint_count = 0;
    if !startup_timepoints.is_empty() {
        let mut playback_target = None;
        let mut selected_window_count = 0;
        // Smooth cadence has priority over a finer rung. First select the
        // finest complete body that can retain the desired one-second window;
        // only machines that cannot hold even the coarsest desired window
        // fall back to the smaller mandatory startup runway.
        for selection_timepoints in [playback_timepoints, startup_timepoints] {
            for candidate in navigation_candidates.iter().rev() {
                let mut trial = DatasetDemandPlanning {
                    plan: candidate.clone(),
                    candidates_visited: 0,
                    guard_retained: false,
                    plane_reuse_envelope: None,
                    scratch_charges: Vec::new(),
                };
                if let Err(error) = retain_navigation_ladder_in_target(
                    catalog,
                    &floor.plan,
                    &mut trial,
                    limits,
                    scratch_ledger,
                ) {
                    if retryable_refinement_capacity(&error) {
                        continue;
                    }
                    match error {
                        PlanAttemptError::Cancelled => return Ok(None),
                        PlanAttemptError::Other(error) => return Err(error),
                        PlanAttemptError::Capacity(_) => {
                            unreachable!("retryable playback capacity was handled above")
                        }
                    }
                }
                if current_plan_union_fits(
                    catalog,
                    obligated_resources,
                    &floor.plan,
                    &trial.plan,
                    selection_timepoints,
                    limits,
                )? {
                    playback_target = Some(trial);
                    selected_window_count = selection_timepoints.len();
                    break;
                }
            }
            if playback_target.is_some() {
                break;
            }
        }
        target = playback_target.ok_or_else(|| {
            anyhow::anyhow!(
                "the coarsest complete playback body cannot fit the required startup runway"
            )
        })?;
        playback_timepoint_count = selected_window_count;
        for candidate_count in selected_window_count.saturating_add(1)..=playback_timepoints.len() {
            if !current_plan_union_fits(
                catalog,
                obligated_resources,
                &floor.plan,
                &target.plan,
                &playback_timepoints[..candidate_count],
                limits,
            )? {
                break;
            }
            playback_timepoint_count = candidate_count;
        }
    }
    target.plan.playback_downshifted =
        !startup_timepoints.is_empty() && target.plan.layer_scales != ideal.ideal_layer_scales;
    Ok(Some(AdaptiveCurrent3dPlanning {
        target,
        floor,
        navigation_candidates,
        playback_timepoint_count,
    }))
}

/// Plans the finest globally feasible target plus a complete catalog-provided
/// navigation floor. The screen-derived level is an ideal, not an admission
/// requirement; only failure of the coarsest configuration is terminal.
#[cfg(test)]
pub(crate) fn plan_progressive_current_3d(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
) -> anyhow::Result<ProgressiveDatasetDemandPlan> {
    let planning = plan_progressive_current_3d_cancellable(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
        None,
        || false,
    )?;
    Ok(planning
        .expect("the synchronous planner cannot be cancelled")
        .plan)
}

/// Runs the exact target/coarse planner with cooperative cancellation for the
/// application-owned latest-camera worker. `None` is a superseded generation;
/// capacity and contract failures remain typed errors and are never converted
/// into a coarser fidelity choice.
// These are the immutable planner inputs plus its ledger and cancellation
// authorities; wrapping them would only add a second call-site representation.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn plan_progressive_current_3d_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Option<ProgressiveDatasetDemandPlanning>> {
    plan_progressive_current_3d_with_visibility_cancellable(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
        &[],
        0,
        None,
        &[],
        None,
        scratch_ledger,
        None,
        None,
        &mut cancelled,
    )
}

/// Builds a small fixed visibility guard without changing sampling/LOD. A
/// guard that cannot fit the ordinary hard limits is discarded and retried as
/// the exact current view; fidelity and capacity are never traded for reuse.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn plan_guarded_progressive_current_3d_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Option<ProgressiveDatasetDemandPlanning>> {
    plan_guarded_progressive_current_3d_with_obligations_cancellable(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
        &[],
        0,
        None,
        &[],
        None,
        scratch_ledger,
        &mut cancelled,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_guarded_progressive_current_3d_with_obligations_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    playback_timepoints: &[TimeIndex],
    playback_required_timepoint_count: usize,
    fixed_playback_layer_scales: Option<&BTreeMap<LogicalLayerKey, ScaleLevel>>,
    obligated_resources: &[BrickKey],
    navigation_ladder_baseline: Option<&NavigationLadderBaseline>,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Option<ProgressiveDatasetDemandPlanning>> {
    let primary_camera = CameraFrame::new(*view.camera(), presentation)?;
    let guard_view = expanded_camera_guard(*view.camera())?;
    let guard_camera = CameraFrame::new(guard_view, presentation)?;
    match plan_progressive_current_3d_with_visibility_cancellable(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
        playback_timepoints,
        playback_required_timepoint_count,
        fixed_playback_layer_scales,
        obligated_resources,
        navigation_ladder_baseline,
        scratch_ledger,
        Some(guard_camera),
        Some(primary_camera),
        &mut cancelled,
    ) {
        Ok(result) => Ok(result),
        Err(error)
            if error
                .downcast_ref::<DatasetDemandPlanCapacityError>()
                .is_some() =>
        {
            plan_progressive_current_3d_with_visibility_cancellable(
                catalog,
                view,
                presentation,
                viewport,
                limits,
                playback_active,
                playback_timepoints,
                playback_required_timepoint_count,
                fixed_playback_layer_scales,
                obligated_resources,
                navigation_ladder_baseline,
                scratch_ledger,
                None,
                None,
                &mut cancelled,
            )
        }
        Err(error) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_progressive_current_3d_with_visibility_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
    playback_timepoints: &[TimeIndex],
    playback_required_timepoint_count: usize,
    fixed_playback_layer_scales: Option<&BTreeMap<LogicalLayerKey, ScaleLevel>>,
    obligated_resources: &[BrickKey],
    navigation_ladder_baseline: Option<&NavigationLadderBaseline>,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    visibility_camera: Option<CameraFrame>,
    priority_camera: Option<CameraFrame>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<ProgressiveDatasetDemandPlanning>> {
    if cancelled() {
        return Ok(None);
    }
    let ideal = select_current_3d_levels(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
    )?;
    let mut selected = ideal.clone();
    if let Some(fixed) = fixed_playback_layer_scales {
        if !playback_active {
            anyhow::bail!("a fixed playback scale map requires active playback");
        }
        validate_fixed_playback_layer_scales(catalog, view, fixed)?;
        selected.layer_scales = fixed.clone();
        selected.playback_downshifted = selected.layer_scales != selected.ideal_layer_scales;
    }
    let Some(mut adaptive) = plan_adaptive_current_3d_exact(
        catalog,
        view,
        selected,
        viewport,
        limits,
        playback_active,
        playback_timepoints,
        playback_required_timepoint_count,
        obligated_resources,
        navigation_ladder_baseline,
        scratch_ledger,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    if let Some(fixed) = fixed_playback_layer_scales
        && &adaptive.target.plan.layer_scales != fixed
    {
        anyhow::bail!(
            "the admitted playback session scale map no longer fits its fixed resource ceilings"
        );
    }
    if cancelled() {
        return Ok(None);
    }

    let playback_timepoints = &playback_timepoints[..adaptive
        .playback_timepoint_count
        .min(playback_timepoints.len())];
    let mut guard_adopted = false;
    if !playback_active
        && let Some(guard_camera) = visibility_camera
        && adaptive.floor.plan.resources != adaptive.target.plan.resources
    {
        let guarded_target = plan_current_3d_configuration(
            catalog,
            view,
            guard_camera,
            priority_camera,
            viewport,
            &adaptive.target.plan.layer_scales,
            limits,
            adaptive.target.plan.playback_downshifted,
            scratch_ledger,
            cancelled,
        )
        .and_then(|mut target| {
            retain_navigation_ladder_in_target(
                catalog,
                &adaptive.floor.plan,
                &mut target,
                limits,
                scratch_ledger,
            )?;
            Ok(target)
        });
        match guarded_target {
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Ok(target)
                if target.plan.primary_resource_count != 0
                    && target.guard_retained
                    && current_plan_union_fits(
                        catalog,
                        obligated_resources,
                        &adaptive.floor.plan,
                        &target.plan,
                        playback_timepoints,
                        limits,
                    )? =>
            {
                adaptive.target = target;
                guard_adopted = true;
            }
            _ => {
                // A visibility guard is a reuse optimization. The exact
                // adaptive target and full-volume ladder remain authoritative.
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }

    let navigation_candidates = adaptive.navigation_candidates;
    let target = adaptive.target;
    let target_is_navigation_ladder = target.plan.layer_scales == adaptive.floor.plan.layer_scales
        && target.plan.resources == adaptive.floor.plan.resources;
    let mut navigation = adaptive.floor;
    let candidates_visited = target
        .candidates_visited
        .saturating_add(navigation.candidates_visited);
    let reuse_envelope = if guard_adopted
        && target.guard_retained
        && navigation.guard_retained
        && let Some(visibility_camera) = visibility_camera
    {
        let mut specs = plan_visibility_specs(catalog, &target.plan)?;
        specs.extend(plan_visibility_specs(catalog, &navigation.plan)?);
        Some(SemanticCameraReuseEnvelope::new(
            visibility_camera,
            viewport,
            specs,
            candidates_visited,
        )?)
    } else {
        None
    };
    let mut scratch_charges = target.scratch_charges;
    scratch_charges.append(&mut navigation.scratch_charges);
    let coarse_plan = (!target_is_navigation_ladder).then_some(navigation.plan);
    Ok(Some(ProgressiveDatasetDemandPlanning {
        plan: ProgressiveDatasetDemandPlan {
            ideal_layer_scales: ideal.ideal_layer_scales,
            target: target.plan,
            coarse: coarse_plan,
            navigation_candidates,
        },
        playback_timepoint_count: playback_timepoints.len(),
        candidates_visited,
        reuse_envelope,
        scratch_charges,
    }))
}

fn validate_fixed_playback_layer_scales(
    catalog: &DatasetCatalog,
    view: &ViewState,
    fixed: &BTreeMap<LogicalLayerKey, ScaleLevel>,
) -> anyhow::Result<()> {
    let visible = view.layers().iter().filter(|layer| layer.visible()).count();
    if fixed.len() != visible {
        anyhow::bail!("fixed playback scale map does not match the visible layer set");
    }
    for layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = layer.layer_key();
        let level = fixed.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "fixed playback scale map omits visible layer {}",
                key.ordinal()
            )
        })?;
        if catalog
            .layer(key)
            .and_then(|layer| layer.scale(*level))
            .is_none()
        {
            anyhow::bail!(
                "fixed playback scale {} is absent from visible layer {}",
                level.get(),
                key.ordinal(),
            );
        }
    }
    Ok(())
}

fn plan_visibility_specs(
    catalog: &DatasetCatalog,
    plan: &DatasetDemandPlan,
) -> anyhow::Result<Vec<SemanticRegionGridSpec>> {
    plan.layer_scales
        .iter()
        .map(|(layer, level)| {
            let scale = catalog
                .layer(*layer)
                .and_then(|layer| layer.scale(*level))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "planned layer {} scale {} disappeared from the catalog",
                        layer.ordinal(),
                        level.get()
                    )
                })?;
            Ok(SemanticRegionGridSpec {
                volume_shape: scale.shape(),
                resource_shape: semantic_resource_shape(scale.shape()),
                grid_to_world: scale.grid_to_world(),
            })
        })
        .collect()
}

/// Builds a bounded plane guard first. If its candidate, resource, byte, or
/// ledger cost does not fit, the same exact current plane is planned without
/// a guard. Capacity fallback changes no LOD or scientific coverage.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn plan_cross_section_panel_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    mut cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Option<DatasetDemandPlanning>> {
    match plan_cross_section_panel_attempt(
        catalog,
        view,
        panel,
        presentation,
        viewport,
        limits,
        scratch_ledger,
        None,
        true,
        &mut cancelled,
    ) {
        Ok(result) => Ok(result),
        Err(error) if plane_guard_attempt_should_retry_exact(&error) => {
            plan_cross_section_panel_attempt(
                catalog,
                view,
                panel,
                presentation,
                viewport,
                limits,
                scratch_ledger,
                None,
                false,
                &mut cancelled,
            )
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn plane_guard_attempt_should_retry_exact(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DatasetDemandPlanCapacityError>()
        .is_some()
        || error.downcast_ref::<CpuLedgerError>().is_some()
        || error
            .downcast_ref::<SemanticPlanError>()
            .is_some_and(|error| matches!(error, SemanticPlanError::ScratchCapacity(_)))
}

fn adaptive_attempt_error_is_capacity(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<DatasetDemandPlanCapacityError>()
        .is_some()
        || error.downcast_ref::<CpuLedgerError>().is_some()
        || error
            .downcast_ref::<SemanticPlanError>()
            .is_some_and(|error| matches!(error, SemanticPlanError::ScratchCapacity(_)))
}

fn cross_plan_union_fits(
    catalog: &DatasetCatalog,
    obligated_resources: &[BrickKey],
    plan: &DatasetDemandPlan,
    limits: DatasetDemandPlanLimits,
) -> anyhow::Result<bool> {
    let (resources, payload_bytes) = cross_plan_union_usage(catalog, obligated_resources, plan)?;
    Ok(resources <= limits.max_resources && payload_bytes <= limits.max_gpu_payload_bytes)
}

fn cross_plan_union_usage(
    catalog: &DatasetCatalog,
    obligated_resources: &[BrickKey],
    plan: &DatasetDemandPlan,
) -> anyhow::Result<(usize, u64)> {
    let mut keys = BTreeSet::new();
    keys.extend(obligated_resources.iter().copied());
    keys.extend(plan.resources.iter().copied());
    let resource_count = keys.len();
    let mut payload_bytes = 0_u64;
    for key in keys {
        let descriptor = catalog.resource_payload_descriptor(key)?;
        payload_bytes = payload_bytes
            .checked_add(mirante4d_render_wgpu::payload_allocation_bytes(descriptor)?)
            .ok_or_else(|| anyhow::anyhow!("global GPU payload accounting overflow"))?;
    }
    Ok((resource_count, payload_bytes))
}

const fn panel_capacity_target(panel: PanelId) -> &'static str {
    match panel {
        PanelId::ThreeD => "3D",
        PanelId::Xy => "XY",
        PanelId::Xz => "XZ",
        PanelId::Yz => "YZ",
    }
}

/// Builds the geometry-independent coarsest full-volume floor retained by
/// linked Plane targets. The floor uses the same semantic tiles and global
/// capacity contract as 3D navigation; it creates no second physical cache.
pub(crate) fn plan_cross_section_navigation_floor_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    limits: DatasetDemandPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<DatasetDemandPlanning>> {
    let layer_scales = coarsest_visible_layer_scales(catalog, view)?;
    match plan_full_volume_navigation_floor(
        catalog,
        view,
        &layer_scales,
        limits,
        false,
        scratch_ledger,
        cancelled,
    ) {
        Ok(planning) => Ok(Some(planning)),
        Err(PlanAttemptError::Cancelled) => Ok(None),
        Err(PlanAttemptError::Capacity(excess)) => Err(
            DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                limits,
                "linked 2D navigation",
                uniform_layer_scale(&layer_scales),
                excess,
            )
            .into(),
        ),
        Err(PlanAttemptError::Other(error)) => Err(error),
    }
}

/// Builds the fixed-scale, geometry-independent linked body owned by a
/// four-panel playback contract. The 3D target already obligates this exact
/// full-volume resource set, so adding a linked wrapper changes neither the
/// renderer-global resource union nor temporal residency.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_cross_section_playback_body_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    limits: DatasetDemandPlanLimits,
    obligated_resources: &[BrickKey],
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<DatasetDemandPlanning>> {
    let planning = match plan_full_volume_navigation_floor(
        catalog,
        view,
        layer_scales,
        limits,
        false,
        scratch_ledger,
        cancelled,
    ) {
        Ok(planning) => planning,
        Err(PlanAttemptError::Cancelled) => return Ok(None),
        Err(PlanAttemptError::Capacity(excess)) => {
            return Err(
                DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                    limits,
                    panel_capacity_target(panel),
                    uniform_layer_scale(layer_scales),
                    excess,
                )
                .into(),
            );
        }
        Err(PlanAttemptError::Other(error)) => return Err(error),
    };
    let (resources, payload_bytes) =
        cross_plan_union_usage(catalog, obligated_resources, &planning.plan)?;
    if resources > limits.max_resources || payload_bytes > limits.max_gpu_payload_bytes {
        return Err(DatasetDemandPlanCapacityError::for_global_usage(
            limits,
            panel_capacity_target(panel),
            uniform_layer_scale(&planning.plan.layer_scales),
            resources,
            payload_bytes,
        )
        .into());
    }
    Ok(Some(planning))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_adaptive_cross_section_panel_with_obligations_cancellable(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    ideal_layer_scales: Option<&BTreeMap<LogicalLayerKey, ScaleLevel>>,
    obligated_resources: &[BrickKey],
    guarded: bool,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<DatasetDemandPlanning>> {
    if cancelled() {
        return Ok(None);
    }
    let ideal_layer_scales = ideal_layer_scales.cloned().map_or_else(
        || cross_section_projected_layer_scales(catalog, view, panel, presentation, viewport),
        Ok,
    )?;
    let mut selected_scales = coarsest_visible_layer_scales(catalog, view)?;
    let Some(mut selected) = plan_cross_section_panel_attempt(
        catalog,
        view,
        panel,
        presentation,
        viewport,
        limits,
        scratch_ledger,
        Some(&selected_scales),
        false,
        cancelled,
    )?
    else {
        return Ok(None);
    };
    let (minimum_resources, minimum_payload_bytes) =
        cross_plan_union_usage(catalog, obligated_resources, &selected.plan)?;
    if minimum_resources > limits.max_resources
        || minimum_payload_bytes > limits.max_gpu_payload_bytes
    {
        return Err(DatasetDemandPlanCapacityError::for_global_usage(
            limits,
            panel_capacity_target(panel),
            uniform_layer_scale(&selected.plan.layer_scales),
            minimum_resources,
            minimum_payload_bytes,
        )
        .into());
    }

    let floor_scales = selected_scales.clone();
    let mut blocked = BTreeSet::new();
    while let Some((layer, finer)) = next_fair_refinement(
        catalog,
        view,
        &floor_scales,
        &selected_scales,
        &ideal_layer_scales,
        &blocked,
    )? {
        if cancelled() {
            return Ok(None);
        }
        let mut trial_scales = selected_scales.clone();
        trial_scales.insert(layer, finer);
        let trial = match plan_cross_section_panel_attempt(
            catalog,
            view,
            panel,
            presentation,
            viewport,
            limits,
            scratch_ledger,
            Some(&trial_scales),
            false,
            cancelled,
        ) {
            Ok(Some(planning)) => planning,
            Ok(None) => return Ok(None),
            Err(error) if adaptive_attempt_error_is_capacity(&error) => {
                blocked.insert(layer);
                continue;
            }
            Err(error) => return Err(error),
        };
        if !cross_plan_union_fits(catalog, obligated_resources, &trial.plan, limits)? {
            blocked.insert(layer);
            continue;
        }
        selected_scales = trial_scales;
        selected = trial;
    }

    if guarded {
        match plan_cross_section_panel_attempt(
            catalog,
            view,
            panel,
            presentation,
            viewport,
            limits,
            scratch_ledger,
            Some(&selected_scales),
            true,
            cancelled,
        ) {
            Ok(Some(guarded))
                if cross_plan_union_fits(catalog, obligated_resources, &guarded.plan, limits)? =>
            {
                selected = guarded;
            }
            Ok(None) => return Ok(None),
            Ok(Some(_)) => {}
            Err(error) if plane_guard_attempt_should_retry_exact(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(selected))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn plan_cross_section_panel_attempt(
    catalog: &DatasetCatalog,
    view: &ViewState,
    panel: PanelId,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    selected_layer_scales: Option<&BTreeMap<LogicalLayerKey, ScaleLevel>>,
    guarded: bool,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<DatasetDemandPlanning>> {
    if cancelled() {
        return Ok(None);
    }
    let semantic_panel = match panel {
        PanelId::Xy => CrossSectionPlane::Xy,
        PanelId::Xz => CrossSectionPlane::Xz,
        PanelId::Yz => CrossSectionPlane::Yz,
        PanelId::ThreeD => anyhow::bail!("the 3D panel is not a cross-section demand target"),
    };
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        let level = selected_layer_scales
            .and_then(|scales| scales.get(&key).copied())
            .map_or_else(
                || {
                    select_cross_section_level(
                        layer,
                        *view.cross_section(),
                        panel,
                        presentation,
                        viewport,
                    )
                },
                |level| {
                    layer
                        .scale(level)
                        .is_some()
                        .then_some(level)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "transient cross-section LOD s{} is absent from layer {}",
                                level.get(),
                                key.ordinal()
                            )
                        })
                },
            )?;
        let scale = layer
            .scale(level)
            .expect("the projected LOD selector returns a catalog scale");
        if cancelled() {
            return Ok(None);
        }
        let spec = SemanticRegionGridSpec {
            volume_shape: scale.shape(),
            resource_shape: semantic_resource_shape(scale.shape()),
            grid_to_world: scale.grid_to_world(),
        };
        let halo = sampling_footprint_class(*view_layer.render_state()).halo_voxels();
        let semantic_limits = semantic_limits(limits, plan.resources.len());
        let region_plan = if guarded {
            plan_guarded_cross_section_resource_regions_cancellable(
                *view.cross_section(),
                semantic_panel,
                presentation,
                viewport,
                spec,
                halo,
                semantic_limits,
                scratch_ledger,
                &mut *cancelled,
            )
        } else {
            plan_cross_section_resource_regions_cancellable(
                *view.cross_section(),
                semantic_panel,
                presentation,
                viewport,
                spec,
                halo,
                semantic_limits,
                scratch_ledger,
                &mut *cancelled,
            )
        }
        .map_err(plan_attempt_from_semantic_error);
        let region_plan = match region_plan {
            Ok(region_plan) => region_plan,
            Err(PlanAttemptError::Capacity(excess)) => {
                return Err(
                    DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                        limits,
                        panel_capacity_target(panel),
                        selected_layer_scales.and_then(uniform_layer_scale),
                        excess,
                    )
                    .into(),
                );
            }
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(PlanAttemptError::Other(error)) => return Err(error),
        };
        plan.candidates_visited = plan
            .candidates_visited
            .saturating_add(region_plan.work.candidates_visited);
        let candidate_charge = region_plan.scratch_charge;
        let primary_regions = region_plan.primary_regions;
        if guarded {
            if let Some(guard) = region_plan.plane_reuse_guard {
                plan.plane_reuse_guards.push(guard);
            } else {
                plan.guard_retained = false;
            }
        }
        let regions = region_plan.regions;
        let result_charge = reserve_key_scratch(scratch_ledger, regions.len())?;
        match append_layer_resources(
            catalog,
            view,
            key,
            scale.level(),
            regions,
            primary_regions,
            &mut plan,
            limits,
            &mut *cancelled,
        ) {
            Ok(()) => {}
            Err(PlanAttemptError::Capacity(excess)) => {
                return Err(
                    DatasetDemandPlanCapacityError::at_selected_scale_with_excess(
                        limits,
                        panel_capacity_target(panel),
                        selected_layer_scales.and_then(uniform_layer_scale),
                        excess,
                    )
                    .into(),
                );
            }
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(PlanAttemptError::Other(error)) => return Err(error),
        }
        drop(candidate_charge);
        if let Some(charge) = result_charge {
            plan.scratch_charges.push(charge);
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let mut covers_full_volume = accumulator_covers_full_volume(catalog, view, &plan);
    if let Some(charge) = reserve_interleave_scratch(scratch_ledger, plan.resources.len())? {
        plan.scratch_charges.push(charge);
    }
    let primary_resource_count = plan
        .primary_resource_counts_by_layer
        .values()
        .copied()
        .sum();
    plan.resources =
        interleave_visible_layers(view, plan.resources, &plan.primary_resource_counts_by_layer);
    if !guarded || !plan.guard_retained {
        plan.resources.truncate(primary_resource_count);
        plan.payload_bytes = plan.primary_payload_bytes;
        covers_full_volume = accumulator_primary_covers_full_volume(catalog, view, &plan);
        plan.plane_reuse_guards.clear();
    }
    if cancelled() {
        return Ok(None);
    }
    let plane_reuse_guards = std::mem::take(&mut plan.plane_reuse_guards);
    let plane_reuse_envelope = SemanticPlaneReuseEnvelope::new(
        plane_reuse_guards,
        covers_full_volume,
        plan.candidates_visited,
    );
    let playback_resource_count = plan.resources.len();
    Ok(Some(DatasetDemandPlanning {
        candidates_visited: plan.candidates_visited,
        plan: DatasetDemandPlan {
            layer_scales: plan.layer_scales,
            resources: plan.resources,
            payload_bytes: plan.payload_bytes,
            playback_downshifted: false,
            covers_full_volume,
            primary_resource_count,
            playback_resource_count,
        },
        guard_retained: guarded && plan.guard_retained,
        plane_reuse_envelope,
        scratch_charges: plan.scratch_charges,
    }))
}

// This hot worker receives borrowed/scalar planning facts; retaining an
// explicit signature avoids building and unpacking a one-use context object.
#[allow(clippy::too_many_arguments)]
fn plan_level(
    catalog: &DatasetCatalog,
    view: &ViewState,
    camera: CameraFrame,
    priority_camera: Option<CameraFrame>,
    viewport: RenderExtent,
    layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    limits: DatasetDemandPlanLimits,
    playback_downshifted: bool,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    cancelled: &mut impl FnMut() -> bool,
) -> PlanAttemptResult<DatasetDemandPlanning> {
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        if cancelled() {
            return Err(PlanAttemptError::Cancelled);
        }
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            ))
        })?;
        let level = layer_scales.get(&key).copied().ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} has no projected LOD selection",
                key.ordinal()
            ))
        })?;
        let scale = layer.scale(level).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} has no selected scale {}",
                key.ordinal(),
                level.get()
            ))
        })?;
        let resource_shape = semantic_resource_shape(scale.shape());
        let region_plan = plan_prioritized_visible_resource_regions_cancellable(
            camera,
            priority_camera,
            viewport,
            SemanticRegionGridSpec {
                volume_shape: scale.shape(),
                resource_shape,
                grid_to_world: scale.grid_to_world(),
            },
            sampling_footprint_class(*view_layer.render_state()).halo_voxels(),
            semantic_limits(limits, plan.resources.len()),
            scratch_ledger,
            &mut *cancelled,
        )
        .map_err(plan_attempt_from_semantic_error)?;
        plan.candidates_visited = plan
            .candidates_visited
            .saturating_add(region_plan.work.candidates_visited);
        let candidate_charge = region_plan.scratch_charge;
        let primary_regions = region_plan.primary_regions;
        let regions = region_plan.regions;
        let result_charge =
            reserve_key_scratch(scratch_ledger, regions.len()).map_err(PlanAttemptError::Other)?;
        append_layer_resources(
            catalog,
            view,
            key,
            scale.level(),
            regions,
            primary_regions,
            &mut plan,
            limits,
            cancelled,
        )?;
        drop(candidate_charge);
        if let Some(charge) = result_charge {
            plan.scratch_charges.push(charge);
        }
    }
    if cancelled() {
        return Err(PlanAttemptError::Cancelled);
    }
    let playback_resource_count = plan.resources.len();
    let mut covers_full_volume = accumulator_covers_full_volume(catalog, view, &plan);
    if let Some(charge) = reserve_interleave_scratch(scratch_ledger, plan.resources.len())
        .map_err(PlanAttemptError::Other)?
    {
        plan.scratch_charges.push(charge);
    }
    let primary_resource_count = plan
        .primary_resource_counts_by_layer
        .values()
        .copied()
        .sum::<usize>();
    plan.resources =
        interleave_visible_layers(view, plan.resources, &plan.primary_resource_counts_by_layer);
    if !plan.guard_retained {
        plan.resources.truncate(primary_resource_count);
        plan.payload_bytes = plan.primary_payload_bytes;
        covers_full_volume = accumulator_primary_covers_full_volume(catalog, view, &plan);
    }
    if cancelled() {
        return Err(PlanAttemptError::Cancelled);
    }
    Ok(DatasetDemandPlanning {
        candidates_visited: plan.candidates_visited,
        plan: DatasetDemandPlan {
            layer_scales: plan.layer_scales,
            resources: plan.resources,
            payload_bytes: plan.payload_bytes,
            playback_downshifted,
            covers_full_volume,
            primary_resource_count,
            playback_resource_count,
        },
        guard_retained: plan.guard_retained,
        plane_reuse_envelope: None,
        scratch_charges: plan.scratch_charges,
    })
}

fn reserve_interleave_scratch(
    ledger: Option<&dyn CpuByteLedger>,
    resource_count: usize,
) -> anyhow::Result<Option<Box<dyn CpuByteLease>>> {
    let Some(ledger) = ledger.filter(|_| resource_count != 0) else {
        return Ok(None);
    };
    ledger
        .try_acquire(
            CpuLedgerCategory::MetadataAndIndexes,
            key_array_bytes(resource_count)?,
        )
        .map(Some)
        .map_err(Into::into)
}

fn reserve_key_scratch(
    ledger: Option<&dyn CpuByteLedger>,
    resource_count: usize,
) -> anyhow::Result<Option<Box<dyn CpuByteLease>>> {
    reserve_interleave_scratch(ledger, resource_count)
}

// The parameters are the exact catalog/view/plan authorities for this bounded
// append; a separate parameter aggregate would not simplify either caller.
#[allow(clippy::too_many_arguments)]
fn append_layer_resources(
    catalog: &DatasetCatalog,
    view: &ViewState,
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    regions: Vec<ResourceRegion>,
    primary_regions: usize,
    plan: &mut PlanAccumulator,
    limits: DatasetDemandPlanLimits,
    cancelled: &mut impl FnMut() -> bool,
) -> PlanAttemptResult<()> {
    for (offset, region) in regions.into_iter().enumerate() {
        if offset.is_multiple_of(256) && cancelled() {
            return Err(PlanAttemptError::Cancelled);
        }
        if plan.resources.len() == limits.max_resources {
            return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::Resources,
                required: u64::try_from(plan.resources.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
                available: u64::try_from(limits.max_resources).unwrap_or(u64::MAX),
            }));
        }
        let key = BrickKey::new(
            catalog.resource_identity(),
            layer,
            view.timepoint(),
            scale,
            region,
        );
        let descriptor = catalog
            .resource_payload_descriptor(key)
            .map_err(|error| PlanAttemptError::Other(error.into()))?;
        let allocation_bytes = mirante4d_render_wgpu::payload_allocation_bytes(descriptor)
            .map_err(|error| PlanAttemptError::Other(error.into()))?;
        let next_payload_bytes =
            plan.payload_bytes
                .checked_add(allocation_bytes)
                .ok_or(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                    dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                    required: u64::MAX,
                    available: limits.max_gpu_payload_bytes,
                }))?;
        if next_payload_bytes > limits.max_gpu_payload_bytes {
            return Err(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                required: next_payload_bytes,
                available: limits.max_gpu_payload_bytes,
            }));
        }
        plan.payload_bytes = next_payload_bytes;
        plan.resources.push(key);
        *plan.resource_counts_by_layer.entry(layer).or_default() += 1;
        if offset < primary_regions {
            plan.primary_payload_bytes =
                plan.primary_payload_bytes
                    .checked_add(allocation_bytes)
                    .ok_or(PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                        dimension: DatasetDemandCapacityDimension::GpuPayloadBytes,
                        required: u64::MAX,
                        available: limits.max_gpu_payload_bytes,
                    }))?;
            *plan
                .primary_resource_counts_by_layer
                .entry(layer)
                .or_default() += 1;
        }
    }
    plan.layer_scales.insert(layer, scale);
    Ok(())
}

fn accumulator_covers_full_volume(
    catalog: &DatasetCatalog,
    view: &ViewState,
    plan: &PlanAccumulator,
) -> bool {
    view.layers()
        .iter()
        .filter(|layer| layer.visible())
        .all(|layer| {
            let Some(scale) = plan
                .layer_scales
                .get(&layer.layer_key())
                .and_then(|level| catalog.layer(layer.layer_key())?.scale(*level))
            else {
                return false;
            };
            let shape = scale.shape();
            let expected = [shape.z(), shape.y(), shape.x()]
                .into_iter()
                .map(|length| length.div_ceil(SEMANTIC_TILE_SIDE))
                .try_fold(1_u64, |total, length| total.checked_mul(length))
                .and_then(|count| usize::try_from(count).ok());
            expected
                == plan
                    .resource_counts_by_layer
                    .get(&layer.layer_key())
                    .copied()
        })
}

fn accumulator_primary_covers_full_volume(
    catalog: &DatasetCatalog,
    view: &ViewState,
    plan: &PlanAccumulator,
) -> bool {
    view.layers()
        .iter()
        .filter(|layer| layer.visible())
        .all(|layer| {
            let Some(scale) = plan
                .layer_scales
                .get(&layer.layer_key())
                .and_then(|level| catalog.layer(layer.layer_key())?.scale(*level))
            else {
                return false;
            };
            let shape = scale.shape();
            let expected = [shape.z(), shape.y(), shape.x()]
                .into_iter()
                .map(|length| length.div_ceil(SEMANTIC_TILE_SIDE))
                .try_fold(1_u64, |total, length| total.checked_mul(length))
                .and_then(|count| usize::try_from(count).ok());
            expected
                == plan
                    .primary_resource_counts_by_layer
                    .get(&layer.layer_key())
                    .copied()
        })
}

/// Interleaves each visible layer's screen-contribution-ranked resources so
/// one channel cannot consume the entire queue before another channel obtains
/// any coverage. Per-layer order comes directly from semantic planning.
fn interleave_visible_layers(
    view: &ViewState,
    resources: Vec<BrickKey>,
    primary_counts: &BTreeMap<LogicalLayerKey, usize>,
) -> Vec<BrickKey> {
    let mut primary_by_layer = BTreeMap::<LogicalLayerKey, VecDeque<BrickKey>>::new();
    let mut guard_by_layer = BTreeMap::<LogicalLayerKey, VecDeque<BrickKey>>::new();
    let mut seen_by_layer = BTreeMap::<LogicalLayerKey, usize>::new();
    for resource in resources {
        let layer = resource.layer();
        let seen = seen_by_layer.entry(layer).or_default();
        let destination = if *seen < primary_counts.get(&layer).copied().unwrap_or(0) {
            &mut primary_by_layer
        } else {
            &mut guard_by_layer
        };
        destination.entry(layer).or_default().push_back(resource);
        *seen += 1;
    }
    let layer_order = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.layer_key())
        .collect::<Vec<_>>();
    let total = primary_by_layer.values().map(VecDeque::len).sum::<usize>()
        + guard_by_layer.values().map(VecDeque::len).sum::<usize>();
    let mut interleaved = Vec::with_capacity(total);
    for by_layer in [&mut primary_by_layer, &mut guard_by_layer] {
        while by_layer.values().any(|resources| !resources.is_empty()) {
            let before = interleaved.len();
            for layer in &layer_order {
                if let Some(resource) = by_layer.get_mut(layer).and_then(VecDeque::pop_front) {
                    interleaved.push(resource);
                }
            }
            assert!(
                interleaved.len() > before,
                "planned resources belong to visible layers"
            );
        }
    }
    interleaved
}

fn semantic_limits(
    limits: DatasetDemandPlanLimits,
    resources_already_planned: usize,
) -> SemanticPlanLimits {
    SemanticPlanLimits::new(
        limits.max_candidates_per_layer,
        limits
            .max_resources
            .saturating_sub(resources_already_planned),
    )
}

fn plan_attempt_from_semantic_error(error: SemanticPlanError) -> PlanAttemptError {
    match error {
        SemanticPlanError::Capacity { kind, maximum } => {
            let dimension = if kind.contains("candidate") {
                DatasetDemandCapacityDimension::Candidates
            } else if kind.contains("scratch") {
                DatasetDemandCapacityDimension::ScratchBytes
            } else {
                DatasetDemandCapacityDimension::Resources
            };
            PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
                dimension,
                required: u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1),
                available: u64::try_from(maximum).unwrap_or(u64::MAX),
            })
        }
        SemanticPlanError::ScratchCapacity(CpuLedgerError::CapacityExceeded {
            requested_bytes,
            available_bytes,
            ..
        }) => PlanAttemptError::Capacity(DatasetDemandCapacityExcess {
            dimension: DatasetDemandCapacityDimension::ScratchBytes,
            required: requested_bytes,
            available: available_bytes,
        }),
        SemanticPlanError::Cancelled => PlanAttemptError::Cancelled,
        error => PlanAttemptError::Other(error.into()),
    }
}

pub(crate) fn semantic_resource_shape(volume: Shape3D) -> Shape3D {
    mirante4d_render_api::default_logical_brick_shape(volume)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use mirante4d_dataset::{
        ContentAddressStatus, DatasetLayer, DatasetScale, DatasetSourceId, ResourceValidity,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, GridToWorld,
        IntensityDType, IsoLightState, LayerTransfer, Opacity, Projection, RenderState, RgbColor,
        SamplingPolicy, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::LayerViewState;

    use super::*;

    struct BoundedScratchLedger {
        live: Arc<AtomicU64>,
        peak: Arc<AtomicU64>,
        capacity: u64,
    }

    struct BoundedScratchLease {
        category: CpuLedgerCategory,
        bytes: u64,
        live: Arc<AtomicU64>,
    }

    impl CpuByteLease for BoundedScratchLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for BoundedScratchLease {
        fn drop(&mut self) {
            self.live.fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }

    impl BoundedScratchLedger {
        fn new(capacity: u64) -> Self {
            Self {
                live: Arc::new(AtomicU64::new(0)),
                peak: Arc::new(AtomicU64::new(0)),
                capacity,
            }
        }

        fn peak(&self) -> u64 {
            self.peak.load(Ordering::Acquire)
        }
    }

    impl CpuByteLedger for BoundedScratchLedger {
        fn try_acquire(
            &self,
            category: CpuLedgerCategory,
            bytes: u64,
        ) -> Result<Box<dyn CpuByteLease>, CpuLedgerError> {
            if bytes == 0 {
                return Err(CpuLedgerError::ZeroByteReservation);
            }
            let mut live = self.live.load(Ordering::Acquire);
            loop {
                let Some(next) = live.checked_add(bytes) else {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.capacity.saturating_sub(live),
                    });
                };
                if next > self.capacity {
                    return Err(CpuLedgerError::CapacityExceeded {
                        category,
                        requested_bytes: bytes,
                        available_bytes: self.capacity.saturating_sub(live),
                    });
                }
                match self.live.compare_exchange_weak(
                    live,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        self.peak.fetch_max(next, Ordering::AcqRel);
                        break;
                    }
                    Err(current) => live = current,
                }
            }
            Ok(Box::new(BoundedScratchLease {
                category,
                bytes,
                live: Arc::clone(&self.live),
            }))
        }
    }

    #[test]
    fn semantic_resource_shape_clips_small_axes() {
        assert_eq!(
            semantic_resource_shape(Shape3D::new(3, 65, 128).unwrap()).dimensions(),
            [3, 64, 64]
        );
    }

    #[test]
    fn sampling_footprint_composes_interpolation_and_gradient_support_exactly() {
        assert_eq!(
            sampling_footprint_class(RenderState::mip(SamplingPolicy::VoxelExact)),
            SamplingFootprintClass::Exact
        );
        assert_eq!(
            sampling_footprint_class(RenderState::mip(SamplingPolicy::SmoothLinear)),
            SamplingFootprintClass::OneVoxel
        );
        assert_eq!(
            sampling_footprint_class(
                RenderState::iso(
                    SamplingPolicy::VoxelExact,
                    IsoShadingPolicy::GradientLighting,
                    0.5,
                )
                .unwrap()
            ),
            SamplingFootprintClass::OneVoxel
        );
        assert_eq!(
            sampling_footprint_class(
                RenderState::iso(
                    SamplingPolicy::SmoothLinear,
                    IsoShadingPolicy::GradientLighting,
                    0.5,
                )
                .unwrap()
            ),
            SamplingFootprintClass::TwoVoxels
        );
        assert_eq!(
            sampling_footprint_class(
                RenderState::iso(SamplingPolicy::VoxelExact, IsoShadingPolicy::Flat, 0.5,).unwrap()
            ),
            SamplingFootprintClass::Exact
        );
    }

    #[test]
    fn irregular_bitmask_plan_uses_exact_gpu_allocation_bytes_at_capacity_boundary() {
        let active = LogicalLayerKey::new(0);
        let shape = Shape3D::new(3, 3, 3).unwrap();
        let catalog = DatasetCatalog::new(
            "irregular-bitmask",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(7)),
            vec![
                DatasetLayer::new(
                    active,
                    "mask",
                    mirante4d_domain::Shape4D::new(1, 3, 3, 3).unwrap(),
                    IntensityDType::Uint16,
                    GridToWorld::identity(),
                    ResourceValidity::BitMask,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(active)],
            active,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(1.0, 1.0, 1.0).unwrap(),
                UnitQuaternion::identity(),
                4.0,
                1.0,
                10.0,
            )
            .unwrap(),
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(1.0, 1.0, 1.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(1.0, 1.0).unwrap();
        let extent = RenderExtent::new(1, 1).unwrap();

        let error = plan_current_3d(
            &catalog,
            &view,
            presentation,
            extent,
            DatasetDemandPlanLimits::new(8, 1, 59),
            false,
        )
        .unwrap_err();
        assert!(error.is::<DatasetDemandPlanCapacityError>());
        let plan = plan_current_3d(
            &catalog,
            &view,
            presentation,
            extent,
            DatasetDemandPlanLimits::new(8, 1, 60),
            false,
        )
        .unwrap();
        assert_eq!(plan.resources.len(), 1);
        assert_eq!(plan.payload_bytes, 60);
        assert_eq!(plan.resources[0].region().shape(), shape);
    }

    #[test]
    fn minimum_navigation_plan_returns_truthful_global_capacity_error() {
        let (catalog, view) = two_layer_catalog_and_view();
        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 0),
            false,
        )
        .unwrap_err();

        assert!(error.is::<DatasetDemandPlanCapacityError>());
        assert_eq!(
            error.to_string(),
            "3D minimum navigation configuration requires 65536 GPU payload bytes, but the renderer-global capacity provides 0"
        );
    }

    #[test]
    fn screen_ideal_uses_the_finest_catalog_configuration_that_fits() {
        let (catalog, view) = two_layer_catalog_and_view();
        let camera = CameraView::new(
            Projection::Orthographic,
            view.camera().target(),
            view.camera().orientation(),
            0.5,
            view.camera().perspective_focal_length_screen_points(),
            view.camera().perspective_view_distance_world(),
        )
        .unwrap();
        let view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();

        let plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 2, 64 * 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(
            plan.layer_scales,
            BTreeMap::from([
                (LogicalLayerKey::new(0), ScaleLevel::new(2)),
                (LogicalLayerKey::new(1), ScaleLevel::new(7)),
            ])
        );
        assert_eq!(plan.resources.len(), 2);
        assert!(
            plan.resources
                .iter()
                .all(|key| key.scale() != ScaleLevel::BASE),
            "capacity selection must use only declared coarser catalog levels"
        );
    }

    #[test]
    fn hidden_analysis_active_does_not_own_visible_volume_or_plane_demand() {
        let (catalog, view) = two_layer_catalog_and_view();
        let visible = LogicalLayerKey::new(1);
        let layers = view
            .layers()
            .iter()
            .map(|layer| {
                LayerViewState::new(
                    layer.layer_key(),
                    layer.layer_key() == visible,
                    layer.transfer().clone(),
                    *layer.render_state(),
                )
            })
            .collect();
        let hidden_active = ViewState::new(
            layers,
            view.active_layer(),
            view.timepoint(),
            *view.camera(),
            ViewerLayout::FourPanel,
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 256, 64 * 1_048_576);
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();

        let volume = plan_progressive_current_3d(
            &catalog,
            &hidden_active,
            presentation,
            extent,
            limits,
            false,
        )
        .unwrap();
        assert_eq!(
            volume
                .target
                .layer_scales
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![visible]
        );
        assert!(
            volume
                .target
                .resources
                .iter()
                .all(|key| key.layer() == visible)
        );

        let visible_focus = ViewState::new(
            hidden_active.layers().to_vec(),
            visible,
            hidden_active.timepoint(),
            *hidden_active.camera(),
            hidden_active.layout(),
            *hidden_active.cross_section(),
            *hidden_active.iso_light(),
        )
        .unwrap();
        let focused_volume = plan_progressive_current_3d(
            &catalog,
            &visible_focus,
            presentation,
            extent,
            limits,
            false,
        )
        .unwrap();
        assert_eq!(
            volume, focused_volume,
            "identical visible render layers must plan identically regardless of analysis focus"
        );

        let plane = plan_cross_section_panel_cancellable(
            &catalog,
            &hidden_active,
            PanelId::Xy,
            presentation,
            extent,
            limits,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            plane.plan.layer_scales.keys().copied().collect::<Vec<_>>(),
            vec![visible]
        );
        assert!(
            plane
                .plan
                .resources
                .iter()
                .all(|key| key.layer() == visible)
        );
        let focused_plane = plan_cross_section_panel_cancellable(
            &catalog,
            &visible_focus,
            PanelId::Xy,
            presentation,
            extent,
            limits,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(plane.plan, focused_plane.plan);
    }

    #[test]
    fn all_hidden_layers_produce_explicit_empty_demand() {
        let (catalog, view) = two_layer_catalog_and_view();
        let layers = view
            .layers()
            .iter()
            .map(|layer| {
                LayerViewState::new(
                    layer.layer_key(),
                    false,
                    layer.transfer().clone(),
                    *layer.render_state(),
                )
            })
            .collect();
        let all_hidden = ViewState::new(
            layers,
            view.active_layer(),
            view.timepoint(),
            *view.camera(),
            ViewerLayout::FourPanel,
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 256, 64 * 1_048_576);
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();

        let volume =
            plan_progressive_current_3d(&catalog, &all_hidden, presentation, extent, limits, false)
                .unwrap();
        assert!(volume.ideal_layer_scales.is_empty());
        assert!(volume.target.layer_scales.is_empty());
        assert!(volume.target.resources.is_empty());

        let plane = plan_cross_section_panel_cancellable(
            &catalog,
            &all_hidden,
            PanelId::Xy,
            presentation,
            extent,
            limits,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(plane.plan.layer_scales.is_empty());
        assert!(plane.plan.resources.is_empty());
    }

    #[test]
    fn finer_candidate_over_each_scalar_bound_selects_the_truthful_catalog_floor() {
        let (catalog, view) = two_layer_catalog_and_view();
        let camera = CameraView::new(
            Projection::Orthographic,
            view.camera().target(),
            view.camera().orientation(),
            0.5,
            view.camera().perspective_focal_length_screen_points(),
            view.camera().perspective_view_distance_world(),
        )
        .unwrap();
        let view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let floor = plan_progressive_current_3d(
            &catalog,
            &view,
            presentation,
            extent,
            DatasetDemandPlanLimits::new(4_096, 2, u64::MAX),
            false,
        )
        .unwrap()
        .target;
        let expected_scales = BTreeMap::from([
            (LogicalLayerKey::new(0), ScaleLevel::new(2)),
            (LogicalLayerKey::new(1), ScaleLevel::new(7)),
        ]);
        assert_eq!(floor.layer_scales, expected_scales);

        for (name, limits) in [
            (
                "candidate traversal",
                DatasetDemandPlanLimits::new(1, 64, u64::MAX),
            ),
            (
                "resource directory",
                DatasetDemandPlanLimits::new(4_096, 2, u64::MAX),
            ),
            (
                "GPU payload",
                DatasetDemandPlanLimits::new(4_096, 64, floor.payload_bytes),
            ),
        ] {
            let selected =
                plan_progressive_current_3d(&catalog, &view, presentation, extent, limits, false)
                    .unwrap();
            assert_eq!(
                uniform_layer_scale(&selected.ideal_layer_scales),
                Some(ScaleLevel::BASE),
                "{name} fixture must request the fine screen ideal"
            );
            assert_eq!(
                selected.target.layer_scales, expected_scales,
                "{name} must fall back to the declared coarsest levels"
            );
            assert!(
                selected
                    .target
                    .resources
                    .iter()
                    .all(|resource| resource.scale() != ScaleLevel::BASE),
                "{name} must not label a coarse body as the rejected fine ideal"
            );
        }
    }

    #[test]
    fn finer_candidate_over_scratch_capacity_selects_the_truthful_catalog_floor() {
        let (catalog, view) = two_layer_catalog_and_view();
        let camera = CameraView::new(
            Projection::Orthographic,
            view.camera().target(),
            view.camera().orientation(),
            0.5,
            view.camera().perspective_focal_length_screen_points(),
            view.camera().perspective_view_distance_world(),
        )
        .unwrap();
        let view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 64, u64::MAX);
        let ideal =
            select_current_3d_levels(&catalog, &view, presentation, extent, limits, false).unwrap();
        let floor_scales = coarsest_visible_layer_scales(&catalog, &view).unwrap();

        let floor_measure = BoundedScratchLedger::new(u64::MAX);
        let floor = plan_current_3d_configuration(
            &catalog,
            &view,
            ideal.camera,
            None,
            extent,
            &floor_scales,
            limits,
            false,
            Some(&floor_measure),
            &mut || false,
        )
        .unwrap_or_else(|_| panic!("the unbounded floor scratch measurement must plan"));
        let floor_peak = floor_measure.peak();
        drop(floor);
        assert!(floor_peak > 0);

        let fine_measure = BoundedScratchLedger::new(u64::MAX);
        let fine = plan_current_3d_configuration(
            &catalog,
            &view,
            ideal.camera,
            None,
            extent,
            &ideal.layer_scales,
            limits,
            false,
            Some(&fine_measure),
            &mut || false,
        )
        .unwrap_or_else(|_| panic!("the unbounded fine scratch measurement must plan"));
        let fine_peak = fine_measure.peak();
        drop(fine);
        assert!(
            fine_peak > floor_peak,
            "the fine fixture must require more live planner scratch"
        );

        let scratch_capacity = floor_peak.saturating_mul(2);
        assert!(floor_peak.saturating_add(fine_peak) > scratch_capacity);
        let bounded = BoundedScratchLedger::new(scratch_capacity);
        let selected = plan_progressive_current_3d_cancellable(
            &catalog,
            &view,
            presentation,
            extent,
            limits,
            false,
            Some(&bounded),
            || false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(
            uniform_layer_scale(&selected.plan.ideal_layer_scales),
            Some(ScaleLevel::BASE)
        );
        assert_eq!(selected.plan.target.layer_scales, floor_scales);
        assert!(
            selected
                .plan
                .target
                .resources
                .iter()
                .all(|resource| resource.scale() != ScaleLevel::BASE)
        );
    }

    #[test]
    fn heterogeneous_visible_layers_use_physical_resolution_and_actual_scale_keys() {
        let (catalog, view) = two_layer_catalog_and_view();
        let plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(
            plan.layer_scales,
            BTreeMap::from([
                (LogicalLayerKey::new(0), ScaleLevel::new(2)),
                (LogicalLayerKey::new(1), ScaleLevel::new(7)),
            ])
        );
        assert!(plan.resources.iter().any(|key| {
            key.layer() == LogicalLayerKey::new(0) && key.scale() == ScaleLevel::new(2)
        }));
        assert!(plan.resources.iter().any(|key| {
            key.layer() == LogicalLayerKey::new(1) && key.scale() == ScaleLevel::new(7)
        }));
        assert_eq!(
            plan.resources
                .iter()
                .map(|key| (key.layer(), key.scale()))
                .collect::<std::collections::BTreeSet<_>>(),
            plan.layer_scales.into_iter().collect()
        );
    }

    #[test]
    fn visible_layers_select_projected_affine_lod_independently() {
        let active = LogicalLayerKey::new(0);
        let sheared = LogicalLayerKey::new(1);
        let base = |level, transform| {
            DatasetScale::new(
                level,
                if level == ScaleLevel::BASE {
                    Shape3D::new(64, 64, 64).unwrap()
                } else {
                    Shape3D::new(32, 32, 32).unwrap()
                },
                transform,
                ResourceValidity::AllValid,
            )
        };
        let active_layer = DatasetLayer::new_multiscale(
            active,
            "axis aligned",
            1,
            IntensityDType::Uint16,
            vec![
                base(
                    ScaleLevel::BASE,
                    GridToWorld::scale(0.25, 0.25, 0.25).unwrap(),
                ),
                base(
                    ScaleLevel::new(1),
                    GridToWorld::scale(1.0, 1.0, 1.0).unwrap(),
                ),
            ],
        )
        .unwrap();
        let sheared_layer = DatasetLayer::new_multiscale(
            sheared,
            "sheared",
            1,
            IntensityDType::Uint16,
            vec![
                base(
                    ScaleLevel::BASE,
                    GridToWorld::scale(0.25, 0.25, 0.25).unwrap(),
                ),
                base(
                    ScaleLevel::new(1),
                    GridToWorld::from_row_major([
                        0.7, 0.7, 0.0, 0.0, 0.0, 0.7, 0.0, 0.0, 0.0, 0.0, 0.7, 0.0, 0.0, 0.0, 0.0,
                        1.0,
                    ])
                    .unwrap(),
                ),
            ],
        )
        .unwrap();
        let catalog = DatasetCatalog::new(
            "independent projected LOD",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
            vec![active_layer, sheared_layer],
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(active), view_layer(sheared)],
            active,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(32.0, 32.0, 32.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                320.0,
                200.0,
            )
            .unwrap(),
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(32.0, 32.0, 32.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();

        let plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 64 * 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(
            plan.layer_scales,
            BTreeMap::from([(active, ScaleLevel::new(1)), (sheared, ScaleLevel::BASE),])
        );

        let progressive = plan_progressive_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 64 * 1_048_576),
            false,
        )
        .unwrap();
        assert_eq!(progressive.target.layer_scales, plan.layer_scales);
        assert!(
            progressive.target.covers_full_volume,
            "the independently selected per-layer target may cover the full volume"
        );
        let floor = progressive
            .coarse
            .expect("a finer target retains a distinct terminal navigation floor");
        assert_eq!(
            floor.layer_scales,
            coarsest_visible_layer_scales(&catalog, &view).unwrap()
        );
    }

    #[test]
    fn hidpi_render_pixels_select_finer_data_than_logical_points() {
        let (catalog, view) = two_layer_catalog_and_view();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 128, 64 * 1_048_576);

        let one_x = plan_current_3d(
            &catalog,
            &view,
            presentation,
            RenderExtent::new(64, 64).unwrap(),
            limits,
            false,
        )
        .unwrap();
        let two_x = plan_current_3d(
            &catalog,
            &view,
            presentation,
            RenderExtent::new(128, 128).unwrap(),
            limits,
            false,
        )
        .unwrap();
        let playback = plan_current_3d(
            &catalog,
            &view,
            presentation,
            RenderExtent::new(128, 128).unwrap(),
            limits,
            true,
        )
        .unwrap();

        assert_eq!(uniform_layer_scale(&one_x.layer_scales), None);
        assert_eq!(
            one_x.layer_scales,
            BTreeMap::from([
                (LogicalLayerKey::new(0), ScaleLevel::new(2)),
                (LogicalLayerKey::new(1), ScaleLevel::new(7)),
            ])
        );
        assert_eq!(
            uniform_layer_scale(&two_x.layer_scales),
            Some(ScaleLevel::BASE)
        );
        assert_eq!(
            uniform_layer_scale(&playback.layer_scales),
            Some(ScaleLevel::BASE)
        );
        assert!(!playback.playback_downshifted);
    }

    #[test]
    fn exact_playback_window_cost_allows_lower_fps_to_select_a_finer_stable_body() {
        let active = LogicalLayerKey::new(0);
        let catalog = DatasetCatalog::new(
            "playback-window-cost",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(9)),
            vec![
                DatasetLayer::new_multiscale(
                    active,
                    "movie",
                    365,
                    IntensityDType::Uint16,
                    vec![
                        DatasetScale::new(
                            ScaleLevel::BASE,
                            Shape3D::new(128, 128, 128).unwrap(),
                            GridToWorld::identity(),
                            ResourceValidity::AllValid,
                        ),
                        DatasetScale::new(
                            ScaleLevel::new(2),
                            Shape3D::new(32, 32, 32).unwrap(),
                            GridToWorld::scale(4.0, 4.0, 4.0).unwrap(),
                            ResourceValidity::AllValid,
                        ),
                    ],
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(active)],
            active,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
                UnitQuaternion::identity(),
                0.5,
                320.0,
                200.0,
            )
            .unwrap(),
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(512.0, 512.0).unwrap();
        let extent = RenderExtent::new(512, 512).unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 4_096, 64 * 1_048_576);
        let plan_for = |view: &ViewState, count: u64| {
            let timepoints = (1..=count).map(TimeIndex::new).collect::<Vec<_>>();
            plan_guarded_progressive_current_3d_with_obligations_cancellable(
                &catalog,
                view,
                presentation,
                extent,
                limits,
                true,
                &timepoints,
                timepoints.len(),
                None,
                &[],
                None,
                None,
                || false,
            )
            .unwrap()
            .expect("the synchronous playback plan cannot be cancelled")
            .plan
        };

        let twelve_fps = plan_for(&view, 12);
        let twenty_four_fps = plan_for(&view, 24);
        assert_eq!(
            uniform_layer_scale(&twelve_fps.target.layer_scales),
            Some(ScaleLevel::BASE)
        );
        assert!(!twelve_fps.target.playback_downshifted);
        assert_eq!(
            uniform_layer_scale(&twenty_four_fps.target.layer_scales),
            Some(ScaleLevel::new(2))
        );
        assert!(twenty_four_fps.target.playback_downshifted);
        assert!(twenty_four_fps.target.covers_full_volume);

        let desired_timepoints = (1..=24).map(TimeIndex::new).collect::<Vec<_>>();
        let bounded_tail = plan_guarded_progressive_current_3d_with_obligations_cancellable(
            &catalog,
            &view,
            presentation,
            extent,
            limits,
            true,
            &desired_timepoints,
            6,
            None,
            &[],
            None,
            None,
            || false,
        )
        .unwrap()
        .expect("the synchronous playback plan cannot be cancelled");
        assert_eq!(
            uniform_layer_scale(&bounded_tail.plan.target.layer_scales),
            Some(ScaleLevel::new(2))
        );
        assert_eq!(
            bounded_tail.playback_timepoint_count,
            desired_timepoints.len(),
            "the desired rolling runway takes priority over a finer but unsustainable body"
        );

        let moved_camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(12.0, 109.0, 37.0).unwrap(),
            UnitQuaternion::new_xyzw(0.31, -0.23, 0.17, 0.91).unwrap(),
            0.08,
            320.0,
            200.0,
        )
        .unwrap();
        let moved_view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            moved_camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let moved_twelve_fps = plan_for(&moved_view, 12);
        assert!(moved_twelve_fps.target.covers_full_volume);
        assert_eq!(
            moved_twelve_fps.target.layer_scales, twelve_fps.target.layer_scales,
            "camera motion cannot change the playback body's selected LOD"
        );
        assert_eq!(
            moved_twelve_fps.target.playback_resource_count,
            twelve_fps.target.playback_resource_count
        );
        assert_eq!(
            &moved_twelve_fps.target.resources[..moved_twelve_fps.target.playback_resource_count],
            &twelve_fps.target.resources[..twelve_fps.target.playback_resource_count],
            "playback prefetch is a full-volume temporal body, not a camera-visible body"
        );
    }

    #[test]
    fn progressive_plan_keeps_terminal_floor_when_finer_full_volume_fits() {
        let (catalog, view) = two_layer_catalog_and_view();
        let camera = CameraView::new(
            Projection::Orthographic,
            view.camera().target(),
            view.camera().orientation(),
            0.5,
            view.camera().perspective_focal_length_screen_points(),
            view.camera().perspective_view_distance_world(),
        )
        .unwrap();
        let view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();

        let limits = DatasetDemandPlanLimits::new(4_096, 128, 64 * 1_048_576);
        let plan = plan_progressive_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            limits,
            false,
        )
        .unwrap();

        assert_eq!(
            uniform_layer_scale(&plan.target.layer_scales),
            Some(ScaleLevel::BASE)
        );
        assert!(plan.target.covers_full_volume);
        assert_eq!(
            plan.navigation_candidates.len(),
            3,
            "fair refinement retains the terminal, one mixed intermediate, and the fine configuration"
        );
        let terminal = &plan.navigation_candidates[0];
        assert_eq!(
            terminal.layer_scales,
            coarsest_visible_layer_scales(&catalog, &view).unwrap()
        );
        assert_eq!(
            terminal.resources.len(),
            terminal.primary_resource_count,
            "each candidate body is exact rather than an aggregate tail"
        );
        let installed_baseline = NavigationLadderBaseline::new(
            plan.navigation_candidates
                .iter()
                .map(|candidate| {
                    let mut canonical = candidate.resources.clone();
                    canonical.sort_unstable();
                    NavigationCandidateBaseline::new(
                        PreparedResourceBody::new(
                            canonical.into(),
                            candidate.resources.clone().into(),
                            None,
                        )
                        .unwrap(),
                        Arc::new(candidate.layer_scales.clone()),
                        candidate.payload_bytes,
                    )
                })
                .collect(),
        );
        let (_, readopted) = adopt_navigation_ladder_baseline(
            &catalog,
            &view,
            &installed_baseline,
            limits,
            false,
            false,
            &[],
            &[],
            None,
        )
        .unwrap_or_else(|_| panic!("the generated fair ladder remains within fixture limits"))
        .expect("the newly generated fair ladder remains reusable");
        assert_eq!(
            readopted
                .iter()
                .map(|candidate| &candidate.layer_scales)
                .collect::<Vec<_>>(),
            plan.navigation_candidates
                .iter()
                .map(|candidate| &candidate.layer_scales)
                .collect::<Vec<_>>(),
            "baseline adoption must retain every one-layer fair refinement rung"
        );
        let playback_plan = plan_progressive_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            limits,
            true,
        )
        .unwrap();
        assert_eq!(
            playback_plan
                .navigation_candidates
                .iter()
                .map(|candidate| candidate.layer_scales.clone())
                .collect::<Vec<_>>(),
            vec![
                coarsest_visible_layer_scales(&catalog, &view).unwrap(),
                BTreeMap::from([
                    (LogicalLayerKey::new(0), ScaleLevel::BASE),
                    (LogicalLayerKey::new(1), ScaleLevel::BASE),
                ]),
            ],
            "the static fair ladder must not change playback's established lockstep policy"
        );
        let transitioned_playback =
            plan_guarded_progressive_current_3d_with_obligations_cancellable(
                &catalog,
                &view,
                PresentationViewport::new(64.0, 64.0).unwrap(),
                RenderExtent::new(64, 64).unwrap(),
                limits,
                true,
                &[],
                0,
                None,
                &[],
                Some(&installed_baseline),
                None,
                || false,
            )
            .unwrap()
            .expect("the static-to-playback transition plan cannot be cancelled")
            .plan;
        assert_eq!(
            transitioned_playback
                .navigation_candidates
                .iter()
                .map(|candidate| candidate.layer_scales.clone())
                .collect::<Vec<_>>(),
            playback_plan
                .navigation_candidates
                .iter()
                .map(|candidate| candidate.layer_scales.clone())
                .collect::<Vec<_>>(),
            "playback must reject a one-layer static baseline and rebuild the established lockstep ladder"
        );
        assert_eq!(
            transitioned_playback.target.layer_scales, playback_plan.target.layer_scales,
            "starting playback from a static ladder cannot strand the target at the terminal rung"
        );
        let playback_baseline = NavigationLadderBaseline::new(
            playback_plan
                .navigation_candidates
                .iter()
                .map(|candidate| {
                    let mut canonical = candidate.resources.clone();
                    canonical.sort_unstable();
                    NavigationCandidateBaseline::new(
                        PreparedResourceBody::new(
                            canonical.into(),
                            candidate.resources.clone().into(),
                            None,
                        )
                        .unwrap(),
                        Arc::new(candidate.layer_scales.clone()),
                        candidate.payload_bytes,
                    )
                })
                .collect(),
        );
        let transitioned_static = plan_guarded_progressive_current_3d_with_obligations_cancellable(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            limits,
            false,
            &[],
            0,
            None,
            &[],
            Some(&playback_baseline),
            None,
            || false,
        )
        .unwrap()
        .expect("the playback-to-static transition plan cannot be cancelled")
        .plan;
        assert_eq!(
            transitioned_static
                .navigation_candidates
                .iter()
                .map(|candidate| candidate.layer_scales.clone())
                .collect::<Vec<_>>(),
            plan.navigation_candidates
                .iter()
                .map(|candidate| candidate.layer_scales.clone())
                .collect::<Vec<_>>(),
            "stopping playback must reject the lockstep baseline and rebuild the static fair ladder"
        );
        let floor = plan
            .coarse
            .expect("a byte-placeable fine volume must not replace the terminal safety floor");
        assert_eq!(
            floor.layer_scales,
            coarsest_visible_layer_scales(&catalog, &view).unwrap()
        );
        assert_eq!(floor.primary_resource_count, terminal.resources.len());
        assert!(
            floor.resources.len() > floor.primary_resource_count,
            "the installed scope keeps the terminal first-useful prefix followed by the finer rung"
        );
        let (resource_ceiling, payload_ceiling) = navigation_tail_limits(limits, terminal);
        assert_eq!(
            resource_ceiling,
            terminal
                .resources
                .len()
                .saturating_add(limits.max_resources / NAVIGATION_TAIL_RESOURCE_DIVISOR)
        );
        assert_eq!(
            payload_ceiling,
            terminal.payload_bytes.saturating_add(
                (limits.max_gpu_payload_bytes / NAVIGATION_TAIL_PAYLOAD_DIVISOR)
                    .min(NAVIGATION_TAIL_MAX_PAYLOAD_BYTES)
            )
        );
        assert!(plan.target.resources.len() <= 128);
        assert!(plan.target.payload_bytes <= 64 * 1_048_576);
    }

    #[test]
    fn installed_ladder_requires_terminal_first_and_reuses_adjacent_rungs() {
        let layer_key = LogicalLayerKey::new(0);
        let catalog = DatasetCatalog::new(
            "installed-navigation-floor",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(73)),
            vec![
                DatasetLayer::new_multiscale(
                    layer_key,
                    "volume",
                    1,
                    IntensityDType::Uint16,
                    vec![
                        DatasetScale::new(
                            ScaleLevel::BASE,
                            Shape3D::new(256, 256, 256).unwrap(),
                            GridToWorld::identity(),
                            ResourceValidity::AllValid,
                        ),
                        DatasetScale::new(
                            ScaleLevel::new(3),
                            Shape3D::new(128, 128, 128).unwrap(),
                            GridToWorld::scale(2.0, 2.0, 2.0).unwrap(),
                            ResourceValidity::AllValid,
                        ),
                        DatasetScale::new(
                            ScaleLevel::new(6),
                            Shape3D::new(64, 64, 64).unwrap(),
                            GridToWorld::scale(4.0, 4.0, 4.0).unwrap(),
                            ResourceValidity::AllValid,
                        ),
                    ],
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(layer_key)],
            layer_key,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(128.0, 128.0, 128.0).unwrap(),
                UnitQuaternion::identity(),
                0.25,
                320.0,
                200.0,
            )
            .unwrap(),
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(128.0, 128.0, 128.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let limits = DatasetDemandPlanLimits::new(4_096, 64, u64::MAX);
        let nonterminal_scales = BTreeMap::from([(layer_key, ScaleLevel::new(3))]);
        let nonterminal = plan_full_volume_navigation_floor(
            &catalog,
            &view,
            &nonterminal_scales,
            limits,
            false,
            None,
            &mut || false,
        )
        .unwrap_or_else(|_| panic!("the S3 fixture is a complete full-volume body"));
        let mut canonical = nonterminal.plan.resources.clone();
        canonical.sort_unstable();
        let nonterminal_baseline = NavigationCandidateBaseline::new(
            PreparedResourceBody::new(
                canonical.into(),
                nonterminal.plan.resources.clone().into(),
                None,
            )
            .unwrap(),
            Arc::new(nonterminal_scales),
            nonterminal.plan.payload_bytes,
        );
        assert!(
            adopt_navigation_candidate_baseline(
                &catalog,
                &view,
                &nonterminal_baseline,
                limits,
                false,
                true,
                None,
            )
            .unwrap_or_else(|_| panic!("nonterminal baseline validation must be bounded"))
            .is_none(),
            "a formerly accepted finer full-volume body is target residency, not the terminal floor"
        );

        let terminal_scales = BTreeMap::from([(layer_key, ScaleLevel::new(6))]);
        let terminal = plan_full_volume_navigation_floor(
            &catalog,
            &view,
            &terminal_scales,
            limits,
            false,
            None,
            &mut || false,
        )
        .unwrap_or_else(|_| panic!("the S6 fixture is the terminal full-volume body"));
        let mut canonical = terminal.plan.resources.clone();
        canonical.sort_unstable();
        let terminal_baseline = NavigationCandidateBaseline::new(
            PreparedResourceBody::new(
                canonical.into(),
                terminal.plan.resources.clone().into(),
                None,
            )
            .unwrap(),
            Arc::new(terminal_scales),
            terminal.plan.payload_bytes,
        );
        assert!(
            adopt_navigation_candidate_baseline(
                &catalog,
                &view,
                &terminal_baseline,
                limits,
                false,
                true,
                None,
            )
            .unwrap_or_else(|_| panic!("terminal baseline validation must be bounded"))
            .is_some(),
            "the exact terminal body is reusable as the stable navigation floor"
        );

        let baseline = NavigationLadderBaseline::new(vec![terminal_baseline, nonterminal_baseline]);
        let (aggregate, candidates) = adopt_navigation_ladder_baseline(
            &catalog,
            &view,
            &baseline,
            limits,
            false,
            false,
            &[],
            &[],
            None,
        )
        .unwrap_or_else(|_| panic!("the installed adjacent ladder must revalidate"))
        .expect("the installed adjacent ladder remains current");
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            uniform_layer_scale(&candidates[0].layer_scales),
            Some(ScaleLevel::new(6))
        );
        assert_eq!(
            uniform_layer_scale(&candidates[1].layer_scales),
            Some(ScaleLevel::new(3))
        );
        assert_eq!(aggregate.plan.primary_resource_count, 1);
        assert_eq!(aggregate.plan.resources.len(), 9);
        assert_eq!(
            aggregate.candidates_visited, 0,
            "baseline adoption must not traverse semantic candidates again"
        );
    }

    #[test]
    fn visible_channels_are_round_robin_interleaved_without_losing_rank_order() {
        let (catalog, view) = two_layer_catalog_and_view();
        let identity = catalog.resource_identity();
        let region = |x| ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap();
        let layer0 = LogicalLayerKey::new(0);
        let layer1 = LogicalLayerKey::new(1);
        let key = |layer, x| {
            BrickKey::new(
                identity,
                layer,
                TimeIndex::new(0),
                ScaleLevel::BASE,
                region(x),
            )
        };
        let interleaved = interleave_visible_layers(
            &view,
            vec![
                key(layer0, 0),
                key(layer0, 1),
                key(layer1, 0),
                key(layer1, 1),
            ],
            &BTreeMap::from([(layer0, 1), (layer1, 1)]),
        );

        assert_eq!(
            interleaved
                .iter()
                .map(|resource| (resource.layer(), resource.region().origin()[2]))
                .collect::<Vec<_>>(),
            vec![(layer0, 0), (layer1, 0), (layer0, 1), (layer1, 1)]
        );
    }

    #[test]
    fn fair_refinement_uses_normalized_progress_authored_ties_and_blocked_fallback() {
        let layer0 = LogicalLayerKey::new(0);
        let layer1 = LogicalLayerKey::new(1);
        let layer_with_levels = |key, levels: &[u32]| {
            DatasetLayer::new_multiscale(
                key,
                format!("layer-{}", key.ordinal()),
                1,
                IntensityDType::Uint16,
                levels
                    .iter()
                    .copied()
                    .map(|level| {
                        DatasetScale::new(
                            ScaleLevel::new(level),
                            Shape3D::new(8, 8, 8).unwrap(),
                            GridToWorld::identity(),
                            ResourceValidity::AllValid,
                        )
                    })
                    .collect(),
            )
            .unwrap()
        };
        let catalog = DatasetCatalog::new(
            "fair-refinement",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
            vec![
                layer_with_levels(layer0, &[0, 2, 4, 6]),
                layer_with_levels(layer1, &[0, 7]),
            ],
        )
        .unwrap();
        let base_view = two_layer_catalog_and_view().1;
        let make_view = |order: [LogicalLayerKey; 2]| {
            ViewState::new(
                order.into_iter().map(view_layer).collect(),
                layer0,
                base_view.timepoint(),
                *base_view.camera(),
                base_view.layout(),
                *base_view.cross_section(),
                *base_view.iso_light(),
            )
            .unwrap()
        };
        let floor = BTreeMap::from([(layer0, ScaleLevel::new(6)), (layer1, ScaleLevel::new(7))]);
        let ideal = BTreeMap::from([(layer0, ScaleLevel::BASE), (layer1, ScaleLevel::BASE)]);
        let mut current = floor.clone();
        let authored = make_view([layer0, layer1]);

        assert_eq!(
            next_fair_refinement(
                &catalog,
                &authored,
                &floor,
                &current,
                &ideal,
                &BTreeSet::new(),
            )
            .unwrap(),
            Some((layer0, ScaleLevel::new(4))),
            "authored order breaks the initial zero-progress tie"
        );
        current.insert(layer0, ScaleLevel::new(4));
        assert_eq!(
            next_fair_refinement(
                &catalog,
                &authored,
                &floor,
                &current,
                &ideal,
                &BTreeSet::new(),
            )
            .unwrap(),
            Some((layer1, ScaleLevel::BASE)),
            "the still-unrefined shallow catalog owns the next normalized-progress step"
        );

        let reordered = make_view([layer1, layer0]);
        assert_eq!(
            next_fair_refinement(
                &catalog,
                &reordered,
                &floor,
                &floor,
                &ideal,
                &BTreeSet::new(),
            )
            .unwrap(),
            Some((layer1, ScaleLevel::BASE))
        );
        assert_eq!(
            next_fair_refinement(
                &catalog,
                &authored,
                &floor,
                &floor,
                &ideal,
                &BTreeSet::from([layer0]),
            )
            .unwrap(),
            Some((layer1, ScaleLevel::BASE)),
            "a capacity-blocked tie leader cannot starve another visible layer"
        );
    }

    #[test]
    fn prepared_body_preserves_only_the_admitted_intersection_without_a_key_index_map() {
        let (catalog, _) = two_layer_catalog_and_view();
        let identity = catalog.resource_identity();
        let layer = LogicalLayerKey::new(0);
        let key = |x| {
            BrickKey::new(
                identity,
                layer,
                TimeIndex::new(0),
                ScaleLevel::BASE,
                ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let first = key(0);
        let admitted = key(1);
        let highest_new_priority = key(2);
        let retired = key(3);
        let base =
            PreparedDemandRequirements::from_ranked(vec![highest_new_priority, first, admitted])
                .unwrap();
        let prepared = base
            .preserving_admitted_prefix(&[admitted, retired])
            .unwrap();

        assert!(Arc::ptr_eq(base.canonical(), prepared.canonical()));
        assert_eq!(
            prepared.ranked().as_ref(),
            &[admitted, highest_new_priority, first]
        );
        assert_eq!(prepared.admitted_prefix_len(), 1);
        assert_eq!(prepared.canonical().len(), 3);
        assert!(prepared.canonical().is_sorted());
        assert!(
            prepared.host_allocation_bytes()
                >= (2 * prepared.canonical().len() * std::mem::size_of::<BrickKey>()) as u64
        );

        let mut guarded =
            PreparedDemandRequirements::from_ranked(vec![highest_new_priority, first, admitted])
                .unwrap();
        guarded.required_prefix_len = 2;
        let guarded = guarded
            .preserving_admitted_prefix(&[admitted, first])
            .unwrap();
        assert_eq!(
            guarded.ranked().as_ref(),
            &[first, highest_new_priority, admitted],
            "an already-admitted guard must not move ahead of current-view demand"
        );
        assert_eq!(guarded.admitted_prefix_len(), 1);
        assert_eq!(guarded.required_prefix_len(), 2);
    }

    #[test]
    fn visible_layer_union_obeys_one_resource_bound() {
        let (catalog, view) = two_layer_catalog_and_view();
        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 1, 1_048_576),
            false,
        )
        .unwrap_err();

        assert!(error.is::<DatasetDemandPlanCapacityError>());

        let playback_plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 4, 262_144),
            true,
        )
        .unwrap();
        assert_eq!(playback_plan.resources.len(), 2);
        assert!(playback_plan.resources.len() * 2 <= 4);
        assert!(playback_plan.payload_bytes * 2 <= 262_144);
    }

    #[test]
    fn mixed_grid_multi_channel_dvr_plans_each_layer_in_its_own_world_grid() {
        let active = LogicalLayerKey::new(0);
        let other = LogicalLayerKey::new(1);
        let catalog = DatasetCatalog::new(
            "incompatible-dvr-grids",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
            vec![
                multiscale_layer(active, ScaleLevel::new(2)),
                multiscale_layer_with_coarse_shape(
                    other,
                    ScaleLevel::new(7),
                    Shape3D::new(16, 32, 32).unwrap(),
                ),
            ],
        )
        .unwrap();
        let (_, mip_view) = two_layer_catalog_and_view();
        let view = ViewState::new(
            vec![dvr_view_layer(active), dvr_view_layer(other)],
            active,
            mip_view.timepoint(),
            *mip_view.camera(),
            mip_view.layout(),
            *mip_view.cross_section(),
            *mip_view.iso_light(),
        )
        .unwrap();

        let plan = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 64, 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(plan.layer_scales.len(), 2);
        assert!(plan.resources.iter().any(|key| key.layer() == active));
        assert!(plan.resources.iter().any(|key| key.layer() == other));
    }

    #[test]
    fn large_full_volume_navigation_floor_reuses_pan_and_orbit_without_a_guard() {
        let layer_key = LogicalLayerKey::new(0);
        let shape = Shape3D::new(64, 16_384, 16_384).unwrap();
        let catalog = DatasetCatalog::new(
            "large-camera-guard-grid",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(44)),
            vec![
                DatasetLayer::new_multiscale(
                    layer_key,
                    "large",
                    1,
                    IntensityDType::Uint16,
                    vec![DatasetScale::new(
                        ScaleLevel::BASE,
                        shape,
                        GridToWorld::identity(),
                        ResourceValidity::AllValid,
                    )],
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(8_192.0, 8_192.0, 32.0).unwrap(),
            UnitQuaternion::identity(),
            14.0,
            1_024.0,
            256.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(layer_key)],
            layer_key,
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(camera.target(), UnitQuaternion::identity(), 1.0, 1.0).unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(1_024.0, 1_024.0).unwrap();
        let extent = RenderExtent::new(1_024, 1_024).unwrap();
        let limits = DatasetDemandPlanLimits::new(131_072, 65_536, u64::MAX);
        let exact = plan_progressive_current_3d_cancellable(
            &catalog,
            &view,
            presentation,
            extent,
            limits,
            false,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        let guarded = plan_guarded_progressive_current_3d_cancellable(
            &catalog,
            &view,
            presentation,
            extent,
            limits,
            false,
            None,
            || false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(guarded.plan.target, exact.plan.target);
        assert!(guarded.plan.target.covers_full_volume);
        assert_eq!(
            guarded.plan.target.primary_resource_count,
            guarded.plan.target.resources.len(),
            "a single-scale navigation floor is complete primary coverage, not a speculative guard"
        );
        assert!(
            guarded.plan.target.resources.len() <= limits.max_resources,
            "the full-volume navigation floor must remain inside the explicit resource ceiling"
        );

        let angle = 0.001_f64;
        let moved_camera = CameraView::new(
            camera.projection(),
            WorldPoint3::new(8_256.0, 8_192.0, 32.0).unwrap(),
            UnitQuaternion::new_xyzw(0.0, 0.0, (angle * 0.5).sin(), (angle * 0.5).cos()).unwrap(),
            camera.orthographic_world_per_screen_point(),
            camera.perspective_focal_length_screen_points(),
            camera.perspective_view_distance_world(),
        )
        .unwrap();
        assert!(
            guarded.reuse_envelope.is_none(),
            "full-volume coverage needs no camera-specific containment envelope"
        );

        let moved_view = ViewState::new(
            view.layers().to_vec(),
            view.active_layer(),
            view.timepoint(),
            moved_camera,
            view.layout(),
            *view.cross_section(),
            *view.iso_light(),
        )
        .unwrap();
        let moved_exact = plan_progressive_current_3d_cancellable(
            &catalog,
            &moved_view,
            presentation,
            extent,
            limits,
            false,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(moved_exact.plan.target, guarded.plan.target);

        let exact_only_limit = DatasetDemandPlanLimits::new(
            131_072,
            exact.plan.target.resources.len(),
            exact.plan.target.payload_bytes,
        );
        let budget_fallback = plan_guarded_progressive_current_3d_cancellable(
            &catalog,
            &view,
            presentation,
            extent,
            exact_only_limit,
            false,
            None,
            || false,
        )
        .unwrap()
        .unwrap();
        assert!(budget_fallback.reuse_envelope.is_none());
        assert_eq!(budget_fallback.plan.target, exact.plan.target);
    }

    #[test]
    fn plane_guard_capacity_falls_back_to_the_exact_current_plane() {
        let layer_key = LogicalLayerKey::new(0);
        let catalog = DatasetCatalog::new(
            "plane-guard-capacity-fallback",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(45)),
            vec![
                DatasetLayer::new_multiscale(
                    layer_key,
                    "large",
                    1,
                    IntensityDType::Uint16,
                    vec![DatasetScale::new(
                        ScaleLevel::BASE,
                        Shape3D::new(512, 512, 512).unwrap(),
                        GridToWorld::identity(),
                        ResourceValidity::AllValid,
                    )],
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let rotation = glam::DQuat::from_rotation_x(0.31) * glam::DQuat::from_rotation_y(-0.27);
        let [x, y, z, w] = rotation.to_array();
        let cross_section = CrossSectionView::new(
            WorldPoint3::new(250.0, 250.0, 250.0).unwrap(),
            UnitQuaternion::new_xyzw(x, y, z, w).unwrap(),
            1.25,
            1.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(layer_key)],
            layer_key,
            TimeIndex::new(0),
            CameraView::new(
                Projection::Orthographic,
                WorldPoint3::new(256.0, 256.0, 256.0).unwrap(),
                UnitQuaternion::identity(),
                1.25,
                320.0,
                200.0,
            )
            .unwrap(),
            ViewerLayout::FourPanel,
            cross_section,
            IsoLightState::attached_camera(),
        )
        .unwrap();
        let presentation = PresentationViewport::new(64.0, 64.0).unwrap();
        let extent = RenderExtent::new(64, 64).unwrap();
        let broad_limits = DatasetDemandPlanLimits::new(4_096, 4_096, u64::MAX);
        let exact = plan_cross_section_panel_attempt(
            &catalog,
            &view,
            PanelId::Xy,
            presentation,
            extent,
            broad_limits,
            None,
            None,
            false,
            &mut || false,
        )
        .unwrap()
        .unwrap();
        assert!(!exact.plan.resources.is_empty());
        let guarded = plan_cross_section_panel_attempt(
            &catalog,
            &view,
            PanelId::Xy,
            presentation,
            extent,
            broad_limits,
            None,
            None,
            true,
            &mut || false,
        )
        .unwrap()
        .unwrap();
        assert!(
            guarded.plan.resources.len() > exact.plan.resources.len(),
            "the fixture must exercise a real dormant guard suffix: exact={}, guarded={}, envelope={:?}",
            exact.plan.resources.len(),
            guarded.plan.resources.len(),
            guarded.plane_reuse_envelope
        );
        let exact_only_limits = DatasetDemandPlanLimits::new(
            4_096,
            exact.plan.resources.len(),
            exact.plan.payload_bytes,
        );
        let fallback = plan_cross_section_panel_cancellable(
            &catalog,
            &view,
            PanelId::Xy,
            presentation,
            extent,
            exact_only_limits,
            None,
            || false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(fallback.plan, exact.plan);
        assert!(
            fallback.plane_reuse_envelope.is_none(),
            "capacity fallback must not retain a containment claim for a discarded guard"
        );
    }

    fn two_layer_catalog_and_view() -> (DatasetCatalog, ViewState) {
        let active = LogicalLayerKey::new(0);
        let other = LogicalLayerKey::new(1);
        let catalog = DatasetCatalog::new(
            "heterogeneous-scales",
            ContentAddressStatus::SessionLocal(DatasetSourceId::new(1)),
            vec![
                multiscale_layer(active, ScaleLevel::new(2)),
                multiscale_layer(other, ScaleLevel::new(7)),
            ],
        )
        .unwrap();
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
            UnitQuaternion::identity(),
            4.0,
            320.0,
            200.0,
        )
        .unwrap();
        let view = ViewState::new(
            vec![view_layer(active), view_layer(other)],
            active,
            TimeIndex::new(0),
            camera,
            ViewerLayout::Single3d,
            CrossSectionView::new(
                WorldPoint3::new(64.0, 64.0, 64.0).unwrap(),
                UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            IsoLightState::attached_camera(),
        )
        .unwrap();
        (catalog, view)
    }

    fn multiscale_layer(key: LogicalLayerKey, coarse_level: ScaleLevel) -> DatasetLayer {
        multiscale_layer_with_coarse_shape(key, coarse_level, Shape3D::new(32, 32, 32).unwrap())
    }

    fn multiscale_layer_with_coarse_shape(
        key: LogicalLayerKey,
        coarse_level: ScaleLevel,
        coarse_shape: Shape3D,
    ) -> DatasetLayer {
        DatasetLayer::new_multiscale(
            key,
            format!("layer-{}", key.ordinal()),
            1,
            IntensityDType::Uint16,
            vec![
                DatasetScale::new(
                    ScaleLevel::BASE,
                    Shape3D::new(128, 128, 128).unwrap(),
                    GridToWorld::scale(1.0, 1.0, 1.0).unwrap(),
                    ResourceValidity::AllValid,
                ),
                DatasetScale::new(
                    coarse_level,
                    coarse_shape,
                    GridToWorld::scale(4.0, 4.0, 4.0).unwrap(),
                    ResourceValidity::AllValid,
                ),
            ],
        )
        .unwrap()
    }

    fn view_layer(key: LogicalLayerKey) -> LayerViewState {
        LayerViewState::new(
            key,
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 65_535.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::SmoothLinear),
        )
    }

    fn dvr_view_layer(key: LogicalLayerKey) -> LayerViewState {
        LayerViewState::new(
            key,
            true,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 65_535.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::dvr(
                SamplingPolicy::SmoothLinear,
                DvrOpacityTransfer::new(
                    DisplayWindow::new(0.0, 65_535.0).unwrap(),
                    TransferCurve::linear(),
                ),
                12.0,
            )
            .unwrap(),
        )
    }
}
