//! Pure semantic demand planning for the unified runtime.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    fmt,
    sync::Arc,
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, DatasetCatalog, DatasetResourceKey,
    ResourceRegion,
};
use mirante4d_domain::{
    CameraView, IsoShadingPolicy, LogicalLayerKey, Projection, RenderState, SamplingPolicy,
    ScaleLevel, Shape3D,
};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    CameraFrame, PreparedResourceBody, PresentationViewport, RenderApiError, RenderExtent,
};

use crate::{
    projected_lod::{select_cross_section_level, select_volume_level},
    semantic_demand::{
        CrossSectionPlane, SemanticCameraReuseEnvelope, SemanticPlanError, SemanticPlanLimits,
        SemanticRegionGridSpec, plan_cross_section_resource_regions_cancellable,
        plan_prioritized_visible_resource_regions_cancellable,
    },
    semantic_tiles::SEMANTIC_TILE_SIDE,
    viewer_layout::PanelId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlan {
    /// The active layer's selected scale. Every layer's actual scale is in
    /// `layer_scales` and in its semantic resource keys.
    pub(crate) scale: ScaleLevel,
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) resources: Vec<DatasetResourceKey>,
    pub(crate) payload_bytes: u64,
    pub(crate) playback_downshifted: bool,
    pub(crate) covers_full_volume: bool,
    /// Exact current-camera prefix. Remaining resources, if any, are the
    /// fixed bounded guard tier and never precede current-view I/O.
    pub(crate) primary_resource_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProgressiveDatasetDemandPlan {
    pub(crate) target: DatasetDemandPlan,
    pub(crate) coarse: Option<DatasetDemandPlan>,
}

pub(crate) struct ProgressiveDatasetDemandPlanning {
    pub(crate) plan: ProgressiveDatasetDemandPlan,
    pub(crate) candidates_visited: usize,
    pub(crate) reuse_envelope: Option<SemanticCameraReuseEnvelope>,
    pub(crate) scratch_charges: Vec<Box<dyn CpuByteLease>>,
}

pub(crate) struct DatasetDemandPlanning {
    pub(crate) plan: DatasetDemandPlan,
    pub(crate) candidates_visited: usize,
    guard_retained: bool,
    pub(crate) scratch_charges: Vec<Box<dyn CpuByteLease>>,
}

impl DatasetDemandPlanning {
    pub(crate) fn prepare_accounted(
        self,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<(PreparedDatasetDemandPlan, usize)> {
        let Self {
            plan,
            candidates_visited,
            guard_retained: _,
            scratch_charges,
        } = self;
        let prepared = PreparedDatasetDemandPlan::from_plan_accounted(plan, reservations)?;
        drop(scratch_charges);
        Ok((prepared, candidates_visited))
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
    /// Contribution-ranked resources before this boundary are required by
    /// the current semantic view. The remaining suffix is a resident camera
    /// guard and cannot delay readiness until explicitly promoted.
    required_prefix_len: usize,
}

impl PreparedDemandRequirements {
    #[cfg(test)]
    pub(crate) fn from_ranked(
        ranked: impl Into<Arc<[DatasetResourceKey]>>,
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
            required_prefix_len,
        })
    }

    pub(crate) fn from_ranked_accounted(
        ranked: Vec<DatasetResourceKey>,
        required_prefix_len: usize,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        if required_prefix_len > ranked.len() {
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
            required_prefix_len,
        })
    }

    /// Keeps already-submitted resources at the front while reprioritizing
    /// only the unadmitted tail. This merge runs on the planning worker, so a
    /// camera-result commit can swap the finished ranked body by `Arc`.
    #[cfg(test)]
    pub(crate) fn preserving_admitted_prefix(
        &self,
        admitted_prefix: &[DatasetResourceKey],
    ) -> Result<Self, PreparedDemandPlanError> {
        if admitted_prefix.is_empty() {
            return Ok(Self {
                body: self.body.clone(),
                admitted_prefix_len: 0,
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
            required_prefix_len: self.required_prefix_len,
        })
    }

    pub(crate) fn into_preserving_admitted_prefix_accounted(
        self,
        admitted_prefix: &[DatasetResourceKey],
        reservations: &PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<Self> {
        if admitted_prefix.is_empty() {
            return Ok(Self {
                body: self.body,
                admitted_prefix_len: 0,
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
            required_prefix_len,
        })
    }

    pub(crate) fn empty() -> Self {
        Self {
            body: PreparedResourceBody::new(Arc::from([]), Arc::from([]), None)
                .expect("an empty prepared application body is valid"),
            admitted_prefix_len: 0,
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

    pub(crate) fn canonical(&self) -> &Arc<[DatasetResourceKey]> {
        self.body.canonical()
    }

    pub(crate) fn ranked(&self) -> &Arc<[DatasetResourceKey]> {
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

    pub(crate) fn bounded_delta_from(
        &self,
        previous: &[DatasetResourceKey],
        maximum_changes: usize,
    ) -> Option<PreparedRequirementDelta> {
        let mut additions = Vec::new();
        let mut removals = Vec::new();
        let mut previous_index = 0;
        let mut next_index = 0;
        while previous_index < previous.len() && next_index < self.canonical().len() {
            match previous[previous_index].cmp(&self.canonical()[next_index]) {
                std::cmp::Ordering::Less => {
                    if additions.len().saturating_add(removals.len()) == maximum_changes {
                        return None;
                    }
                    removals.push(previous[previous_index]);
                    previous_index += 1;
                }
                std::cmp::Ordering::Greater => {
                    if additions.len().saturating_add(removals.len()) == maximum_changes {
                        return None;
                    }
                    additions.push(self.canonical()[next_index]);
                    next_index += 1;
                }
                std::cmp::Ordering::Equal => {
                    previous_index += 1;
                    next_index += 1;
                }
            }
        }
        let trailing_changes = previous
            .len()
            .saturating_sub(previous_index)
            .saturating_add(self.canonical().len().saturating_sub(next_index));
        if additions
            .len()
            .saturating_add(removals.len())
            .saturating_add(trailing_changes)
            > maximum_changes
        {
            return None;
        }
        removals.extend_from_slice(&previous[previous_index..]);
        additions.extend_from_slice(&self.canonical()[next_index..]);
        Some(PreparedRequirementDelta {
            additions: additions.into(),
            removals: removals.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedRequirementDelta {
    additions: Arc<[DatasetResourceKey]>,
    removals: Arc<[DatasetResourceKey]>,
}

impl PreparedRequirementDelta {
    pub(crate) fn additions(&self) -> &Arc<[DatasetResourceKey]> {
        &self.additions
    }

    pub(crate) fn removals(&self) -> &Arc<[DatasetResourceKey]> {
        &self.removals
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedDatasetDemandPlan {
    pub(crate) scale: ScaleLevel,
    pub(crate) layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    pub(crate) requirements: PreparedDemandRequirements,
    pub(crate) payload_bytes: u64,
    pub(crate) playback_downshifted: bool,
    pub(crate) covers_full_volume: bool,
    pub(crate) primary_resource_count: usize,
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
            scale: plan.scale,
            layer_scales: plan.layer_scales,
            requirements,
            payload_bytes: plan.payload_bytes,
            playback_downshifted: plan.playback_downshifted,
            covers_full_volume: plan.covers_full_volume,
            primary_resource_count: plan.primary_resource_count,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedProgressiveDatasetDemandPlan {
    pub(crate) target: PreparedDatasetDemandPlan,
    pub(crate) coarse: Option<PreparedDatasetDemandPlan>,
    pub(crate) reuse_envelope: Option<SemanticCameraReuseEnvelope>,
}

impl PreparedProgressiveDatasetDemandPlan {
    pub(crate) fn from_planning_accounted(
        planning: ProgressiveDatasetDemandPlanning,
        reservations: &mut PreparedAllocationReservations<'_>,
    ) -> anyhow::Result<(Self, usize)> {
        let ProgressiveDatasetDemandPlanning {
            plan,
            candidates_visited,
            reuse_envelope,
            scratch_charges,
        } = planning;
        let ProgressiveDatasetDemandPlan { target, coarse } = plan;
        let prepared = Self {
            target: PreparedDatasetDemandPlan::from_plan_accounted(target, reservations)?,
            coarse: coarse
                .map(|plan| PreparedDatasetDemandPlan::from_plan_accounted(plan, reservations))
                .transpose()?,
            reuse_envelope,
        };
        drop(scratch_charges);
        Ok((prepared, candidates_visited))
    }
}

pub(crate) fn key_array_bytes(length: usize) -> anyhow::Result<u64> {
    let bytes = length
        .checked_mul(std::mem::size_of::<DatasetResourceKey>())
        .ok_or_else(|| anyhow::anyhow!("prepared key-array byte accounting overflow"))?;
    u64::try_from(bytes)
        .map_err(|_| anyhow::anyhow!("prepared key-array byte accounting exceeds u64"))
}

/// Conservative fixed scratch for a bounded requirement delta. Two growing
/// vectors can retain rounded-up capacities while one exact `Arc` payload is
/// materialized; the extra eight keys cover both small-vector capacity floors.
pub(crate) fn bounded_requirement_delta_scratch_bytes(
    maximum_changes: usize,
) -> anyhow::Result<u64> {
    if maximum_changes == 0 {
        return Ok(0);
    }
    let capacity_keys = maximum_changes
        .checked_mul(3)
        .and_then(|count| count.checked_add(8))
        .ok_or_else(|| anyhow::anyhow!("requirement-delta scratch byte overflow"))?;
    key_array_bytes(capacity_keys)
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
        }
    }

    const fn reserve_playback_half(self, playback_active: bool) -> Self {
        if playback_active {
            Self {
                max_candidates_per_layer: self.max_candidates_per_layer,
                max_resources: self.max_resources / 2,
                max_gpu_payload_bytes: self.max_gpu_payload_bytes / 2,
            }
        } else {
            self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DatasetDemandPlanCapacityError {
    limits: DatasetDemandPlanLimits,
    selected_scale: Option<ScaleLevel>,
}

impl DatasetDemandPlanCapacityError {
    const fn new(limits: DatasetDemandPlanLimits) -> Self {
        Self {
            limits,
            selected_scale: None,
        }
    }

    const fn at_selected_scale(
        limits: DatasetDemandPlanLimits,
        selected_scale: ScaleLevel,
    ) -> Self {
        Self {
            limits,
            selected_scale: Some(selected_scale),
        }
    }

    #[cfg(test)]
    pub(crate) const fn selected_scale(self) -> Option<ScaleLevel> {
        self.selected_scale
    }
}

impl fmt::Display for DatasetDemandPlanCapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(scale) = self.selected_scale {
            write!(formatter, "screen-selected scale s{} ", scale.get())?;
        }
        write!(
            formatter,
            "dataset demand cannot fit within {} resources, {} decoded bytes, and {} candidates per visible layer; fidelity was not silently reduced",
            self.limits.max_resources,
            self.limits.max_gpu_payload_bytes,
            self.limits.max_candidates_per_layer,
        )
    }
}

impl Error for DatasetDemandPlanCapacityError {}

enum PlanAttemptError {
    Capacity,
    Cancelled,
    Other(anyhow::Error),
}

type PlanAttemptResult<T> = Result<T, PlanAttemptError>;

struct PlanAccumulator {
    resources: Vec<DatasetResourceKey>,
    layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    payload_bytes: u64,
    primary_payload_bytes: u64,
    resource_counts_by_layer: BTreeMap<LogicalLayerKey, usize>,
    primary_resource_counts_by_layer: BTreeMap<LogicalLayerKey, usize>,
    candidates_visited: usize,
    guard_retained: bool,
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
            scratch_charges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
struct SelectedCurrent3dLevels {
    camera: CameraFrame,
    active_level: ScaleLevel,
    layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
    effective_limits: DatasetDemandPlanLimits,
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
    match plan_level(
        catalog,
        view,
        selected.camera,
        None,
        viewport,
        selected.active_level,
        &selected.layer_scales,
        selected.effective_limits,
        selected.playback_downshifted,
        None,
        &mut never_cancelled,
    ) {
        Ok(planning) => Ok(planning.plan),
        Err(PlanAttemptError::Capacity) => Err(DatasetDemandPlanCapacityError::at_selected_scale(
            selected.effective_limits,
            selected.active_level,
        )
        .into()),
        Err(PlanAttemptError::Cancelled) => {
            unreachable!("the synchronous planner cannot be cancelled")
        }
        Err(PlanAttemptError::Other(error)) => Err(error),
    }
}

fn select_current_3d_levels(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    playback_active: bool,
) -> anyhow::Result<SelectedCurrent3dLevels> {
    let (camera, active_level, layer_scales, playback_downshifted) =
        projected_current_3d_levels(catalog, view, presentation, viewport, playback_active)?;
    let effective_limits = limits.reserve_playback_half(playback_active);
    Ok(SelectedCurrent3dLevels {
        camera,
        active_level,
        layer_scales,
        effective_limits,
        playback_downshifted,
    })
}

pub(crate) fn current_3d_projected_layer_scales(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    playback_active: bool,
) -> anyhow::Result<BTreeMap<LogicalLayerKey, ScaleLevel>> {
    projected_current_3d_levels(catalog, view, presentation, viewport, playback_active)
        .map(|(_, _, layer_scales, _)| layer_scales)
}

fn projected_current_3d_levels(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    playback_active: bool,
) -> anyhow::Result<(
    CameraFrame,
    ScaleLevel,
    BTreeMap<LogicalLayerKey, ScaleLevel>,
    bool,
)> {
    let active = catalog
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active layer is absent from the dataset catalog"))?;
    let camera = CameraFrame::new(*view.camera(), presentation)?;
    let ideal_active = select_volume_level(active, camera, viewport)?;
    let active_level = playback_level(active, ideal_active, playback_active);
    let mut playback_downshifted = active_level != ideal_active;
    let mut layer_scales = BTreeMap::new();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        let ideal = select_volume_level(layer, camera, viewport)?;
        let selected = playback_level(layer, ideal, playback_active);
        playback_downshifted |= selected != ideal;
        layer_scales.insert(key, selected);
    }
    Ok((camera, active_level, layer_scales, playback_downshifted))
}

fn playback_level(
    layer: &mirante4d_dataset::DatasetLayer,
    ideal: ScaleLevel,
    playback_active: bool,
) -> ScaleLevel {
    if !playback_active {
        return ideal;
    }
    layer
        .scales()
        .map(|scale| scale.level())
        .filter(|level| *level > ideal)
        .min()
        .unwrap_or(ideal)
}

/// Plans the target view plus an optional complete coarse cohort that fits in
/// the target plan's remaining count/byte budget. The target is never silently
/// coarsened merely to make room for progressive presentation; when no coarse
/// cohort fits, initial target loading remains honestly partial.
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
    scratch_ledger: Option<&dyn CpuByteLedger>,
    visibility_camera: Option<CameraFrame>,
    priority_camera: Option<CameraFrame>,
    cancelled: &mut impl FnMut() -> bool,
) -> anyhow::Result<Option<ProgressiveDatasetDemandPlanning>> {
    if cancelled() {
        return Ok(None);
    }
    let selected = select_current_3d_levels(
        catalog,
        view,
        presentation,
        viewport,
        limits,
        playback_active,
    )?;
    let target = match plan_level(
        catalog,
        view,
        visibility_camera.unwrap_or(selected.camera),
        priority_camera,
        viewport,
        selected.active_level,
        &selected.layer_scales,
        selected.effective_limits,
        selected.playback_downshifted,
        scratch_ledger,
        cancelled,
    ) {
        Ok(planning) => planning,
        Err(PlanAttemptError::Capacity) => {
            return Err(DatasetDemandPlanCapacityError::at_selected_scale(
                selected.effective_limits,
                selected.active_level,
            )
            .into());
        }
        Err(PlanAttemptError::Cancelled) => return Ok(None),
        Err(PlanAttemptError::Other(error)) => return Err(error),
    };
    if cancelled() {
        return Ok(None);
    }
    let playback_resource_reserve = if playback_active {
        target.plan.resources.len()
    } else {
        0
    };
    let playback_byte_reserve = if playback_active {
        target.plan.payload_bytes
    } else {
        0
    };
    let remaining = DatasetDemandPlanLimits::new(
        limits.max_candidates_per_layer,
        limits
            .max_resources
            .saturating_sub(target.plan.resources.len())
            .saturating_sub(playback_resource_reserve),
        limits
            .max_gpu_payload_bytes
            .saturating_sub(target.plan.payload_bytes)
            .saturating_sub(playback_byte_reserve),
    );
    let (coarse_visibility_camera, coarse_priority_camera) = if target.guard_retained {
        (visibility_camera, priority_camera)
    } else {
        (None, None)
    };
    let coarse = match plan_complete_coarse_3d(
        catalog,
        view,
        presentation,
        viewport,
        remaining,
        target.plan.scale,
        &target.plan.layer_scales,
        scratch_ledger,
        coarse_visibility_camera,
        coarse_priority_camera,
        cancelled,
    ) {
        Ok(coarse) => coarse,
        Err(PlanAttemptError::Cancelled) => return Ok(None),
        Err(PlanAttemptError::Capacity) => unreachable!("coarse capacity is optional"),
        Err(PlanAttemptError::Other(error)) => return Err(error),
    };
    if cancelled() {
        return Ok(None);
    }
    let coarse = coarse.filter(|coarse| coarse.plan.resources != target.plan.resources);
    let candidates_visited = target.candidates_visited.saturating_add(
        coarse
            .as_ref()
            .map_or(0, |planning| planning.candidates_visited),
    );
    let reuse_envelope = if target.guard_retained
        && coarse
            .as_ref()
            .is_none_or(|planning| planning.guard_retained)
        && let Some(visibility_camera) = visibility_camera
    {
        let mut specs = plan_visibility_specs(catalog, &target.plan)?;
        if let Some(coarse) = coarse.as_ref() {
            specs.extend(plan_visibility_specs(catalog, &coarse.plan)?);
        }
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
    if let Some(coarse) = coarse.as_ref() {
        // Move below with the optional plan after borrowing its work count.
        scratch_charges.reserve(coarse.scratch_charges.len());
    }
    let coarse_plan = coarse.map(|mut planning| {
        scratch_charges.append(&mut planning.scratch_charges);
        planning.plan
    });
    Ok(Some(ProgressiveDatasetDemandPlanning {
        plan: ProgressiveDatasetDemandPlan {
            target: target.plan,
            coarse: coarse_plan,
        },
        candidates_visited,
        reuse_envelope,
        scratch_charges,
    }))
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

// Keep the coarse attempt's complete planning contract explicit at this sole
// call boundary instead of introducing a one-use parameter object.
#[allow(clippy::too_many_arguments)]
fn plan_complete_coarse_3d(
    catalog: &DatasetCatalog,
    view: &ViewState,
    presentation: PresentationViewport,
    viewport: RenderExtent,
    limits: DatasetDemandPlanLimits,
    target_active_level: ScaleLevel,
    target_layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
    scratch_ledger: Option<&dyn CpuByteLedger>,
    visibility_camera: Option<CameraFrame>,
    priority_camera: Option<CameraFrame>,
    cancelled: &mut impl FnMut() -> bool,
) -> PlanAttemptResult<Option<DatasetDemandPlanning>> {
    if limits.max_resources == 0 || limits.max_gpu_payload_bytes == 0 {
        return Ok(None);
    }
    if cancelled() {
        return Err(PlanAttemptError::Cancelled);
    }
    let active = catalog.layer(view.active_layer()).ok_or_else(|| {
        PlanAttemptError::Other(anyhow::anyhow!(
            "active layer is absent from the dataset catalog"
        ))
    })?;
    let active_level = active
        .scales()
        .map(|scale| scale.level())
        .max()
        .expect("DatasetLayer always has at least one scale");
    let mut layer_scales = BTreeMap::new();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            PlanAttemptError::Other(anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            ))
        })?;
        let level = layer
            .scales()
            .map(|scale| scale.level())
            .max()
            .expect("DatasetLayer always has at least one scale");
        layer_scales.insert(key, level);
    }
    if active_level == target_active_level && &layer_scales == target_layer_scales {
        return Ok(None);
    }
    let camera = visibility_camera
        .map_or_else(|| CameraFrame::new(*view.camera(), presentation), Ok)
        .map_err(|error| PlanAttemptError::Other(error.into()))?;
    match plan_level(
        catalog,
        view,
        camera,
        priority_camera,
        viewport,
        active_level,
        &layer_scales,
        limits,
        false,
        scratch_ledger,
        cancelled,
    ) {
        Ok(plan) => Ok(Some(plan)),
        Err(PlanAttemptError::Capacity) => Ok(None),
        Err(error) => Err(error),
    }
}

// These are the immutable panel-planner inputs plus its ledger and
// cancellation authorities; a wrapper would not reduce owned work.
#[allow(clippy::too_many_arguments)]
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
    if cancelled() {
        return Ok(None);
    }
    let active = catalog
        .layer(view.active_layer())
        .ok_or_else(|| anyhow::anyhow!("active layer is absent from the dataset catalog"))?;
    let semantic_panel = match panel {
        PanelId::Xy => CrossSectionPlane::Xy,
        PanelId::Xz => CrossSectionPlane::Xz,
        PanelId::Yz => CrossSectionPlane::Yz,
        PanelId::ThreeD => anyhow::bail!("the 3D panel is not a cross-section demand target"),
    };
    let active_level =
        select_cross_section_level(active, *view.cross_section(), panel, presentation, viewport)?;
    let mut plan = PlanAccumulator::default();
    for view_layer in view.layers().iter().filter(|layer| layer.visible()) {
        let key = view_layer.layer_key();
        let layer = catalog.layer(key).ok_or_else(|| {
            anyhow::anyhow!(
                "visible layer {} is absent from the dataset catalog",
                key.ordinal()
            )
        })?;
        let level = select_cross_section_level(
            layer,
            *view.cross_section(),
            panel,
            presentation,
            viewport,
        )?;
        let scale = layer
            .scale(level)
            .expect("the projected LOD selector returns a catalog scale");
        if cancelled() {
            return Ok(None);
        }
        let region_plan = plan_cross_section_resource_regions_cancellable(
            *view.cross_section(),
            semantic_panel,
            presentation,
            SemanticRegionGridSpec {
                volume_shape: scale.shape(),
                resource_shape: semantic_resource_shape(scale.shape()),
                grid_to_world: scale.grid_to_world(),
            },
            sampling_footprint_class(*view_layer.render_state()).halo_voxels(),
            semantic_limits(limits, plan.resources.len()),
            scratch_ledger,
            &mut cancelled,
        )
        .map_err(plan_attempt_from_semantic_error);
        let region_plan = match region_plan {
            Ok(region_plan) => region_plan,
            Err(PlanAttemptError::Capacity) => {
                return Err(DatasetDemandPlanCapacityError::new(limits).into());
            }
            Err(PlanAttemptError::Cancelled) => return Ok(None),
            Err(PlanAttemptError::Other(error)) => return Err(error),
        };
        plan.candidates_visited = plan
            .candidates_visited
            .saturating_add(region_plan.work.candidates_visited);
        let candidate_charge = region_plan.scratch_charge;
        let primary_regions = region_plan.primary_regions;
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
            &mut cancelled,
        ) {
            Ok(()) => {}
            Err(PlanAttemptError::Capacity) => {
                return Err(DatasetDemandPlanCapacityError::new(limits).into());
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
    let covers_full_volume = accumulator_covers_full_volume(catalog, view, &plan);
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
    if cancelled() {
        return Ok(None);
    }
    Ok(Some(DatasetDemandPlanning {
        candidates_visited: plan.candidates_visited,
        plan: DatasetDemandPlan {
            scale: active_level,
            layer_scales: plan.layer_scales,
            resources: plan.resources,
            payload_bytes: plan.payload_bytes,
            playback_downshifted: false,
            covers_full_volume,
            primary_resource_count,
        },
        guard_retained: false,
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
    active_level: ScaleLevel,
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
        if priority_camera.is_some() {
            let guard_regions = regions.len().saturating_sub(primary_regions);
            let maximum_guard_regions = primary_regions / 4 + usize::from(primary_regions != 0) * 2;
            if guard_regions > maximum_guard_regions {
                plan.guard_retained = false;
            }
        }
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
            scale: active_level,
            layer_scales: plan.layer_scales,
            resources: plan.resources,
            payload_bytes: plan.payload_bytes,
            playback_downshifted,
            covers_full_volume,
            primary_resource_count,
        },
        guard_retained: plan.guard_retained,
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
            return Err(PlanAttemptError::Capacity);
        }
        let key = DatasetResourceKey::new(
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
        let next_payload_bytes = plan
            .payload_bytes
            .checked_add(allocation_bytes)
            .ok_or(PlanAttemptError::Capacity)?;
        if next_payload_bytes > limits.max_gpu_payload_bytes {
            return Err(PlanAttemptError::Capacity);
        }
        plan.payload_bytes = next_payload_bytes;
        plan.resources.push(key);
        *plan.resource_counts_by_layer.entry(layer).or_default() += 1;
        if offset < primary_regions {
            plan.primary_payload_bytes = plan
                .primary_payload_bytes
                .checked_add(allocation_bytes)
                .ok_or(PlanAttemptError::Capacity)?;
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
    resources: Vec<DatasetResourceKey>,
    primary_counts: &BTreeMap<LogicalLayerKey, usize>,
) -> Vec<DatasetResourceKey> {
    let mut primary_by_layer = BTreeMap::<LogicalLayerKey, VecDeque<DatasetResourceKey>>::new();
    let mut guard_by_layer = BTreeMap::<LogicalLayerKey, VecDeque<DatasetResourceKey>>::new();
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
    if error.is_capacity() {
        PlanAttemptError::Capacity
    } else if matches!(error, SemanticPlanError::Cancelled) {
        PlanAttemptError::Cancelled
    } else {
        PlanAttemptError::Other(error.into())
    }
}

pub(crate) fn semantic_resource_shape(volume: Shape3D) -> Shape3D {
    Shape3D::new(
        volume.z().min(SEMANTIC_TILE_SIDE),
        volume.y().min(SEMANTIC_TILE_SIDE),
        volume.x().min(SEMANTIC_TILE_SIDE),
    )
    .expect("a semantic resource clipped to a non-empty volume is non-empty")
}

#[cfg(test)]
mod tests {
    use mirante4d_dataset::{
        DatasetLayer, DatasetScale, DatasetSourceId, ResourceValidity, ScientificIdentityStatus,
    };
    use mirante4d_domain::{
        CameraView, CrossSectionView, DisplayWindow, DvrOpacityTransfer, GridToWorld,
        IntensityDType, IsoLightState, LayerTransfer, Opacity, Projection, RenderState, RgbColor,
        SamplingPolicy, TimeIndex, TransferCurve, UnitQuaternion, ViewerLayout, WorldPoint3,
    };
    use mirante4d_project_model::LayerViewState;

    use super::*;

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
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(7)),
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
    fn no_scale_plan_returns_stable_capacity_error_instead_of_over_budget_plan() {
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
            "screen-selected scale s2 dataset demand cannot fit within 64 resources, 0 decoded bytes, and 4096 candidates per visible layer; fidelity was not silently reduced"
        );
    }

    #[test]
    fn screen_selected_scale_reports_capacity_instead_of_silently_coarsening() {
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

        let error = plan_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 2, 64 * 1_048_576),
            false,
        )
        .unwrap_err();
        let capacity = error
            .downcast_ref::<DatasetDemandPlanCapacityError>()
            .expect("the screen-selected scale must fail with typed capacity");

        assert_eq!(capacity.selected_scale(), Some(ScaleLevel::BASE));
        assert!(
            error
                .to_string()
                .contains("fidelity was not silently reduced")
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

        assert_eq!(plan.scale, ScaleLevel::new(2));
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
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
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

        assert_eq!(plan.scale, ScaleLevel::new(1));
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
        assert_eq!(
            progressive
                .coarse
                .expect("the independently coarser sheared layer fits")
                .layer_scales,
            BTreeMap::from([(active, ScaleLevel::new(1)), (sheared, ScaleLevel::new(1)),])
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

        assert_eq!(one_x.scale, ScaleLevel::new(2));
        assert_eq!(two_x.scale, ScaleLevel::BASE);
        assert_eq!(playback.scale, ScaleLevel::new(2));
        assert!(playback.playback_downshifted);
    }

    #[test]
    fn progressive_plan_keeps_target_fidelity_and_uses_only_leftover_for_coarse_coverage() {
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

        let plan = plan_progressive_current_3d(
            &catalog,
            &view,
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            DatasetDemandPlanLimits::new(4_096, 128, 64 * 1_048_576),
            false,
        )
        .unwrap();

        assert_eq!(plan.target.scale, ScaleLevel::BASE);
        let coarse = plan.coarse.expect("the bounded coarse cohort fits");
        assert_eq!(coarse.scale, ScaleLevel::new(2));
        assert!(plan.target.resources.len() + coarse.resources.len() <= 128);
        assert!(plan.target.payload_bytes + coarse.payload_bytes <= 64 * 1_048_576);
    }

    #[test]
    fn visible_channels_are_round_robin_interleaved_without_losing_rank_order() {
        let (catalog, view) = two_layer_catalog_and_view();
        let identity = catalog.resource_identity();
        let region = |x| ResourceRegion::new([0, 0, x], Shape3D::new(1, 1, 1).unwrap()).unwrap();
        let layer0 = LogicalLayerKey::new(0);
        let layer1 = LogicalLayerKey::new(1);
        let key = |layer, x| {
            DatasetResourceKey::new(
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
    fn prepared_body_preserves_only_the_admitted_intersection_without_a_key_index_map() {
        let (catalog, _) = two_layer_catalog_and_view();
        let identity = catalog.resource_identity();
        let layer = LogicalLayerKey::new(0);
        let key = |x| {
            DatasetResourceKey::new(
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
                >= (2 * prepared.canonical().len() * std::mem::size_of::<DatasetResourceKey>())
                    as u64
        );

        let mut previous = vec![first, retired];
        previous.sort_unstable();
        let delta = prepared.bounded_delta_from(&previous, 3).unwrap();
        assert_eq!(
            delta.additions().as_ref(),
            &[admitted, highest_new_priority]
        );
        assert_eq!(delta.removals().as_ref(), &[retired]);
        assert!(prepared.bounded_delta_from(&previous, 2).is_none());
        assert_eq!(
            bounded_requirement_delta_scratch_bytes(32).unwrap(),
            key_array_bytes(104).unwrap()
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
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
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
    fn large_grid_contained_pan_and_orbit_reuse_guard_without_lod_or_budget_tradeoff() {
        let layer_key = LogicalLayerKey::new(0);
        let shape = Shape3D::new(64, 16_384, 16_384).unwrap();
        let catalog = DatasetCatalog::new(
            "large-camera-guard-grid",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(44)),
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

        assert_eq!(guarded.plan.target.scale, exact.plan.target.scale);
        assert_eq!(
            guarded.plan.target.primary_resource_count,
            exact.plan.target.resources.len()
        );
        assert_eq!(
            &guarded.plan.target.resources[..guarded.plan.target.primary_resource_count],
            exact.plan.target.resources.as_slice(),
            "the exact view must be the complete first I/O tier"
        );
        let guard_count = guarded
            .plan
            .target
            .resources
            .len()
            .saturating_sub(guarded.plan.target.primary_resource_count);
        assert!(guard_count > 0);
        assert!(
            guard_count <= guarded.plan.target.primary_resource_count / 4 + 2,
            "the fixed guard must remain a bounded thin frontier"
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
        let envelope = guarded
            .reuse_envelope
            .as_ref()
            .expect("the bounded guard should be retained");
        assert!(envelope.reusable_candidates() > 40_000);
        assert!(
            envelope
                .contains(
                    CameraFrame::new(moved_camera, presentation).unwrap(),
                    extent
                )
                .unwrap(),
            "a thin pan/orbit frontier should reuse the immutable large plan"
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
        let mut guard_canonical = guarded.plan.target.resources.clone();
        guard_canonical.sort_unstable();
        assert!(
            moved_exact
                .plan
                .target
                .resources
                .iter()
                .all(|resource| { guard_canonical.binary_search(resource).is_ok() })
        );

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

    fn two_layer_catalog_and_view() -> (DatasetCatalog, ViewState) {
        let active = LogicalLayerKey::new(0);
        let other = LogicalLayerKey::new(1);
        let catalog = DatasetCatalog::new(
            "heterogeneous-scales",
            ScientificIdentityStatus::Unverified(DatasetSourceId::new(1)),
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
