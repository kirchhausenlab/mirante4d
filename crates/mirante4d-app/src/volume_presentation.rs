//! Native-resolution 3D navigation and hidden exact-work observations.

use std::collections::{BTreeMap, VecDeque};

use mirante4d_application::RenderGestureId;
use mirante4d_dataset::DatasetCatalog;
use mirante4d_domain::{
    CameraView, IsoShadingPolicy, LogicalLayerKey, Projection, RenderMode, RenderState,
    SamplingPolicy, ScaleLevel, TimeIndex,
};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{FrameIdentity, PresentationViewport, RenderExtent};
use mirante4d_render_wgpu::{GpuFrameTiming, GpuTimingTicket, VolumeColorSchedule};

// Neutral work counts sample taps rather than assigning permanent costs to
// render modes. The former 1x MIP / 2x DVR-or-ISO distinction survives only as
// a first-observation prior; asynchronous GPU evidence replaces it per family.
const INITIAL_INTERACTIVE_WORK_UNITS: u64 = 128_000_000;
// Native visible navigation uses a fixed product envelope, not a learned
// timing probe. At 1920x1080 this admits roughly 1,200 neutral sample taps per
// pixel: enough for the normal S2/S3 voxel-exact Cell views while smooth
// eight-tap bodies fall back to the terminal full-volume fast path.
const NATIVE_NAVIGATION_WORK_UNITS: u64 = 2_500_000_000;
const MIN_WORK_UNITS: u64 = 1_000_000;
const INTERACTION_GPU_BUDGET_NS: u64 = 12_000_000;
const REFINEMENT_GPU_BUDGET_NS: u64 = 3_000_000;
const DIRECT_CERTIFICATION_NS: u64 = 8_000_000;
const ADAPTIVE_SAFETY_NUMERATOR: u64 = 3;
const ADAPTIVE_SAFETY_DENOMINATOR: u64 = 4;
const MAX_FAMILY_CALIBRATIONS: usize = 32;
const MAX_PROFILE_OBSERVATIONS: usize = 32;
const MAX_PENDING_TIMINGS: usize = 64;
const MAX_STRIP_ACCUMULATORS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VolumeLayerWorkClass {
    mode: RenderMode,
    sampling: SamplingPolicy,
    iso_shading: Option<IsoShadingPolicy>,
    /// One of 32 bounded display-threshold bands. ISO first-hit work changes
    /// radically with threshold, so unrelated regimes must not share hidden
    /// strip timing authority.
    iso_display_bucket: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeWorkFamily {
    projection: Projection,
    layers: Box<[VolumeLayerWorkClass]>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct VolumeLayerWorkload {
    layer: LogicalLayerKey,
    scale: ScaleLevel,
    render_state: RenderState,
    maximum_steps: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VolumeWorkloadProfile {
    extent: RenderExtent,
    presentation_viewport: PresentationViewport,
    camera: CameraView,
    timepoint: TimeIndex,
    layers: Box<[VolumeLayerWorkload]>,
    work_per_pixel: u64,
    initial_prior_work_per_pixel: u64,
    family: VolumeWorkFamily,
}

impl VolumeWorkloadProfile {
    pub(crate) fn from_view(
        catalog: &DatasetCatalog,
        view: &ViewState,
        camera: CameraView,
        presentation_viewport: PresentationViewport,
        layer_scales: &BTreeMap<LogicalLayerKey, ScaleLevel>,
        extent: RenderExtent,
    ) -> anyhow::Result<Self> {
        let mut layers = Vec::new();
        let mut work_per_pixel = 0_u64;
        let mut initial_prior_work_per_pixel = 0_u64;
        let mut family_layers = Vec::new();
        for layer in view.layers().iter().filter(|layer| layer.visible()) {
            let key = layer.layer_key();
            let scale = layer_scales.get(&key).copied().ok_or_else(|| {
                anyhow::anyhow!(
                    "3D workload has no selected scale for visible layer {}",
                    key.ordinal()
                )
            })?;
            let dataset_scale = catalog
                .layer(key)
                .and_then(|catalog_layer| catalog_layer.scale(scale))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "3D workload references absent layer {} scale s{}",
                        key.ordinal(),
                        scale.get()
                    )
                })?;
            let shape = dataset_scale.shape();
            let maximum_steps = shape.x().max(shape.y()).max(shape.z());
            let render_state = *layer.render_state();
            let taps = match render_state.sampling_policy() {
                SamplingPolicy::VoxelExact => 1_u64,
                SamplingPolicy::SmoothLinear => 8_u64,
            };
            // This factor is deliberately not part of neutral work. It only
            // seeds an unknown family's initial safe envelope.
            let initial_mode_prior = if render_state.mode() == RenderMode::Mip {
                1_u64
            } else {
                2_u64
            };
            let gradient_taps = render_state
                .iso_parameters()
                .filter(|parameters| {
                    parameters.shading_policy()
                        == mirante4d_domain::IsoShadingPolicy::GradientLighting
                })
                .map_or(0_u64, |_| 6_u64.saturating_mul(taps));
            let neutral_layer_work = maximum_steps
                .saturating_mul(taps)
                .saturating_add(gradient_taps)
                .max(1);
            let prior_layer_work = maximum_steps
                .saturating_mul(taps)
                .saturating_mul(initial_mode_prior)
                .saturating_add(gradient_taps)
                .max(1);
            work_per_pixel = work_per_pixel.saturating_add(neutral_layer_work);
            initial_prior_work_per_pixel =
                initial_prior_work_per_pixel.saturating_add(prior_layer_work);
            family_layers.push(VolumeLayerWorkClass {
                mode: render_state.mode(),
                sampling: render_state.sampling_policy(),
                iso_shading: render_state
                    .iso_parameters()
                    .map(|parameters| parameters.shading_policy()),
                iso_display_bucket: render_state.iso_parameters().map(|parameters| {
                    let bucket = (parameters.display_level().clamp(0.0, 1.0) * 32.0).floor();
                    (bucket as u8).min(31)
                }),
            });
            layers.push(VolumeLayerWorkload {
                layer: key,
                scale,
                render_state,
                maximum_steps,
            });
        }
        if layers.is_empty() {
            anyhow::bail!("3D workload requires at least one visible layer");
        }
        Ok(Self {
            extent,
            presentation_viewport,
            camera,
            timepoint: view.timepoint(),
            layers: layers.into_boxed_slice(),
            work_per_pixel: work_per_pixel.max(1),
            initial_prior_work_per_pixel: initial_prior_work_per_pixel.max(1),
            family: VolumeWorkFamily {
                projection: camera.projection(),
                layers: family_layers.into_boxed_slice(),
            },
        })
    }

    pub(crate) const fn extent(&self) -> RenderExtent {
        self.extent
    }

    pub(crate) const fn camera(&self) -> CameraView {
        self.camera
    }

    pub(crate) fn full_work_units(&self) -> u64 {
        extent_pixels(self.extent).saturating_mul(self.work_per_pixel)
    }

    fn work_units_for_rows(&self, rows: u32) -> u64 {
        u64::from(self.extent.width_pixels())
            .saturating_mul(u64::from(rows))
            .saturating_mul(self.work_per_pixel)
    }

    fn initial_safe_work_units(&self) -> u64 {
        INITIAL_INTERACTIVE_WORK_UNITS
            .saturating_mul(self.work_per_pixel)
            .checked_div(self.initial_prior_work_per_pixel.max(1))
            .unwrap_or(0)
            .max(MIN_WORK_UNITS)
    }

    fn native_navigation_is_safe(&self) -> bool {
        self.full_work_units() <= NATIVE_NAVIGATION_WORK_UNITS
    }

    fn scale_configuration(&self) -> VolumeScaleConfiguration {
        VolumeScaleConfiguration(
            self.layers
                .iter()
                .map(|layer| (layer.layer, layer.scale))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        )
    }

    fn is_no_finer_than(&self, target: &Self) -> bool {
        self.layers.len() == target.layers.len()
            && self.layers.iter().all(|candidate| {
                target
                    .layers
                    .iter()
                    .find(|target| target.layer == candidate.layer)
                    .is_some_and(|target| candidate.scale >= target.scale)
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeScaleConfiguration(Box<[(LogicalLayerKey, ScaleLevel)]>);

#[derive(Debug, Clone)]
pub(crate) struct VolumePreviewCandidate {
    profile: VolumeWorkloadProfile,
    available: bool,
    cold_bootstrap: bool,
    full_volume: bool,
    active_scale: ScaleLevel,
    resource_count: usize,
    payload_bytes: u64,
}

impl VolumePreviewCandidate {
    pub(crate) fn navigation(
        profile: VolumeWorkloadProfile,
        available: bool,
        cold_bootstrap: bool,
        active_scale: ScaleLevel,
        resource_count: usize,
        payload_bytes: u64,
    ) -> Self {
        Self {
            profile,
            available,
            cold_bootstrap,
            full_volume: true,
            active_scale,
            resource_count,
            payload_bytes,
        }
    }

    pub(crate) fn target(
        profile: VolumeWorkloadProfile,
        available: bool,
        active_scale: ScaleLevel,
        resource_count: usize,
        payload_bytes: u64,
    ) -> Self {
        Self {
            profile,
            available,
            cold_bootstrap: false,
            full_volume: false,
            active_scale,
            resource_count,
            payload_bytes,
        }
    }

    pub(crate) fn profile(&self) -> &VolumeWorkloadProfile {
        &self.profile
    }

    pub(crate) const fn available(&self) -> bool {
        self.available
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumePreviewCandidateKind {
    Navigation,
    ExactTarget,
}

impl VolumePreviewCandidateKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "navigation",
            Self::ExactTarget => "exact-target",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VolumePreviewCandidateDisposition {
    NotResident,
    FinerThanTarget,
    InteractionUnsafe,
    Eligible,
    Selected,
    SelectedColdBootstrap,
    SelectedTerminalEmergency,
}

impl VolumePreviewCandidateDisposition {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::NotResident => "not resident",
            Self::FinerThanTarget => "finer than target",
            Self::InteractionUnsafe => "interaction-unsafe",
            Self::Eligible => "eligible",
            Self::Selected => "selected",
            Self::SelectedColdBootstrap => "selected cold bootstrap",
            Self::SelectedTerminalEmergency => "selected terminal emergency",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VolumePreviewCandidateFacts {
    pub(crate) kind: VolumePreviewCandidateKind,
    pub(crate) active_scale: ScaleLevel,
    pub(crate) resource_count: usize,
    pub(crate) payload_bytes: u64,
    pub(crate) native_work_units: u64,
    pub(crate) complete_and_resident: bool,
    pub(crate) target_quality_eligible: bool,
    pub(crate) interaction_safe: bool,
    pub(crate) full_volume: bool,
    pub(crate) disposition: VolumePreviewCandidateDisposition,
}

#[derive(Debug, Clone)]
struct ProfileObservation {
    profile: VolumeWorkloadProfile,
    certified: bool,
    last_use: u64,
}

#[derive(Debug, Clone)]
struct FamilyCalibration {
    family: VolumeWorkFamily,
    safe_work_units: u64,
    last_use: u64,
}

#[derive(Debug, Clone)]
struct ActiveRefinementSchedule {
    frame: FrameIdentity,
    profile: VolumeWorkloadProfile,
    strip_height_pixels: u32,
}

#[derive(Debug, Clone)]
struct PresentedPreview {
    frame: FrameIdentity,
}

#[derive(Debug, Clone)]
struct GesturePreviewPolicy {
    gesture: RenderGestureId,
    scales: VolumeScaleConfiguration,
    navigation_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct VolumePreviewChoice {
    candidate_index: usize,
    profile: VolumeWorkloadProfile,
}

impl VolumePreviewChoice {
    pub(crate) const fn candidate_index(&self) -> usize {
        self.candidate_index
    }

    pub(crate) fn into_profile(self) -> VolumeWorkloadProfile {
        self.profile
    }
}

#[derive(Debug, Clone)]
enum PendingTimingKind {
    Direct,
    Preview,
    AtomicStrip {
        frame: FrameIdentity,
        completed_strip: u32,
        total_strips: u32,
        strip_work_units: u64,
    },
}

#[derive(Debug, Clone)]
struct PendingTiming {
    execution_id: u64,
    profile: VolumeWorkloadProfile,
    kind: PendingTimingKind,
}

#[derive(Debug, Clone)]
struct StripAccumulator {
    frame: FrameIdentity,
    profile: VolumeWorkloadProfile,
    observed: Box<[bool]>,
    total_ns: u64,
    last_use: u64,
}

#[derive(Debug, Default)]
pub(crate) struct VolumePresentationController {
    family_calibrations: Vec<FamilyCalibration>,
    profile_observations: Vec<ProfileObservation>,
    pending_timings: VecDeque<PendingTiming>,
    strip_accumulators: Vec<StripAccumulator>,
    active_refinement: Option<ActiveRefinementSchedule>,
    presented_preview: Option<PresentedPreview>,
    gesture_preview_policy: Option<GesturePreviewPolicy>,
    latest_candidate_facts: Vec<VolumePreviewCandidateFacts>,
    clock: u64,
    #[cfg(test)]
    test_work_limits: Option<(u64, u64)>,
}

impl VolumePresentationController {
    pub(crate) fn direct_is_safe(&mut self, profile: &VolumeWorkloadProfile) -> bool {
        let now = self.tick();
        if let Some(observation) = self
            .profile_observations
            .iter_mut()
            .find(|observation| observation.profile == *profile)
        {
            observation.last_use = now;
            if observation.certified {
                return true;
            }
        }
        // Visible presentation policy is deliberately static. Runtime timing
        // may certify this exact camera/profile for a future direct frame and
        // may size hidden exact strips, but it never changes the visible
        // preview body or output resolution while the user is interacting.
        self.navigation_is_safe(profile)
    }

    pub(crate) fn select_preview_profile(
        &mut self,
        target_profile: &VolumeWorkloadProfile,
        candidates: &[VolumePreviewCandidate],
        gesture: Option<RenderGestureId>,
    ) -> Option<VolumePreviewChoice> {
        // Candidates are ordered from the guaranteed terminal navigation
        // body to the selected finer body. Choose the finest statically safe
        // native-resolution body. If none satisfies the static envelope, the
        // terminal body remains the unconditional navigation floor.
        #[cfg(test)]
        let test_navigation_limit = self.test_work_limits.map(|(interactive, _)| interactive);
        #[cfg(not(test))]
        let test_navigation_limit: Option<u64> = None;
        let profile_is_safe = |profile: &VolumeWorkloadProfile| {
            test_navigation_limit.map_or_else(
                || profile.native_navigation_is_safe(),
                |limit| profile.full_work_units() <= limit,
            )
        };
        let candidate_is_eligible = |candidate: &VolumePreviewCandidate| {
            candidate.available
                && candidate.profile.extent() == target_profile.extent()
                && candidate.profile.is_no_finer_than(target_profile)
                && profile_is_safe(&candidate.profile)
        };
        let statically_selected = || {
            candidates
                .iter()
                .enumerate()
                .rev()
                .find(|(_, candidate)| candidate_is_eligible(candidate))
                .map(|(index, _)| index)
                .or_else(|| {
                    candidates.iter().position(|candidate| {
                        candidate.full_volume && (candidate.available || candidate.cold_bootstrap)
                    })
                })
        };
        let candidate_index = (|| {
            if let Some(gesture) = gesture {
                let existing = self
                    .gesture_preview_policy
                    .as_ref()
                    .filter(|policy| policy.gesture == gesture)
                    .cloned();
                if let Some(policy) = existing {
                    if let Some(index) =
                        candidates
                            .iter()
                            .enumerate()
                            .rev()
                            .find_map(|(index, candidate)| {
                                (candidate_is_eligible(candidate)
                                    && (!policy.navigation_only || candidate.full_volume)
                                    && candidate.profile.scale_configuration() == policy.scales)
                                    .then_some(index)
                            })
                    {
                        Some(index)
                    } else {
                        // A camera-local exact body can cease to cover sustained
                        // movement. Make one monotonic cut to the finest complete
                        // safe full-volume rung and keep that configuration for
                        // the rest of the gesture.
                        let index = candidates
                            .iter()
                            .enumerate()
                            .rev()
                            .find(|(_, candidate)| {
                                candidate.full_volume && candidate_is_eligible(candidate)
                            })
                            .map(|(index, _)| index)
                            .or_else(statically_selected)?;
                        self.gesture_preview_policy = Some(GesturePreviewPolicy {
                            gesture,
                            scales: candidates[index].profile.scale_configuration(),
                            navigation_only: true,
                        });
                        Some(index)
                    }
                } else {
                    let index = statically_selected()?;
                    self.gesture_preview_policy = Some(GesturePreviewPolicy {
                        gesture,
                        scales: candidates[index].profile.scale_configuration(),
                        navigation_only: candidates[index].full_volume,
                    });
                    Some(index)
                }
            } else {
                self.gesture_preview_policy = None;
                statically_selected()
            }
        })();

        self.latest_candidate_facts = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let target_quality_eligible = candidate.profile.extent() == target_profile.extent()
                    && candidate.profile.is_no_finer_than(target_profile);
                let interaction_safe = profile_is_safe(&candidate.profile);
                let disposition = if candidate_index == Some(index) {
                    if candidate_is_eligible(candidate) {
                        VolumePreviewCandidateDisposition::Selected
                    } else if candidate.cold_bootstrap {
                        VolumePreviewCandidateDisposition::SelectedColdBootstrap
                    } else {
                        VolumePreviewCandidateDisposition::SelectedTerminalEmergency
                    }
                } else if !candidate.available {
                    VolumePreviewCandidateDisposition::NotResident
                } else if !target_quality_eligible {
                    VolumePreviewCandidateDisposition::FinerThanTarget
                } else if !interaction_safe {
                    VolumePreviewCandidateDisposition::InteractionUnsafe
                } else {
                    VolumePreviewCandidateDisposition::Eligible
                };
                VolumePreviewCandidateFacts {
                    kind: if candidate.full_volume {
                        VolumePreviewCandidateKind::Navigation
                    } else {
                        VolumePreviewCandidateKind::ExactTarget
                    },
                    active_scale: candidate.active_scale,
                    resource_count: candidate.resource_count,
                    payload_bytes: candidate.payload_bytes,
                    native_work_units: candidate.profile.full_work_units(),
                    complete_and_resident: candidate.available,
                    target_quality_eligible,
                    interaction_safe,
                    full_volume: candidate.full_volume,
                    disposition,
                }
            })
            .collect();

        let candidate_index = candidate_index?;
        Some(VolumePreviewChoice {
            candidate_index,
            profile: candidates[candidate_index].profile.clone(),
        })
    }

    pub(crate) fn latest_candidate_facts(&self) -> &[VolumePreviewCandidateFacts] {
        &self.latest_candidate_facts
    }

    pub(crate) fn preview_needs_replacement(
        &self,
        frame: FrameIdentity,
        proposed: &VolumeWorkloadProfile,
        output_extent: RenderExtent,
    ) -> bool {
        let _ = (proposed, output_extent);
        // A frame identity is the stable presentation transaction. Once its
        // complete native-resolution preview is visible, later resource
        // arrival or timing evidence cannot replace it with another preview.
        // The only next visible state for this frame is completed exact.
        self.presented_preview
            .as_ref()
            .is_none_or(|current| current.frame != frame)
    }

    pub(crate) fn note_presented_preview(
        &mut self,
        frame: FrameIdentity,
        profile: VolumeWorkloadProfile,
        output_extent: RenderExtent,
    ) {
        debug_assert_eq!(
            profile.extent(),
            output_extent,
            "a presented 3D preview must be native resolution"
        );
        self.presented_preview = Some(PresentedPreview { frame });
    }

    pub(crate) fn note_presented_exact(&mut self, frame: FrameIdentity) {
        if self
            .presented_preview
            .as_ref()
            .is_some_and(|preview| preview.frame == frame)
        {
            self.presented_preview = None;
        }
        if self
            .active_refinement
            .as_ref()
            .is_some_and(|refinement| refinement.frame == frame)
        {
            self.active_refinement = None;
        }
    }

    pub(crate) fn refinement_strip_height(
        &mut self,
        frame: FrameIdentity,
        profile: &VolumeWorkloadProfile,
    ) -> u32 {
        if let Some(active) = self
            .active_refinement
            .as_ref()
            .filter(|active| active.frame == frame && active.profile == *profile)
        {
            return active.strip_height_pixels;
        }
        let work_limit = self.refinement_work_limit(profile);
        let row_work = profile.work_units_for_rows(1).max(1);
        let rows = (work_limit / row_work).max(1);
        let strip_height_pixels = u32::try_from(rows)
            .unwrap_or(u32::MAX)
            .min(profile.extent.height_pixels())
            .max(1);
        self.active_refinement = Some(ActiveRefinementSchedule {
            frame,
            profile: profile.clone(),
            strip_height_pixels,
        });
        strip_height_pixels
    }

    #[cfg(test)]
    fn refinement_work_units(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        self.refinement_work_limit(profile)
    }

    pub(crate) fn track_timing(
        &mut self,
        ticket: GpuTimingTicket,
        profile: VolumeWorkloadProfile,
        schedule: VolumeColorSchedule,
        frame: FrameIdentity,
        refinement: Option<mirante4d_render_wgpu::VolumeRefinementProgress>,
    ) {
        let kind = match schedule {
            VolumeColorSchedule::Direct => PendingTimingKind::Direct,
            VolumeColorSchedule::InteractivePreview => PendingTimingKind::Preview,
            VolumeColorSchedule::AtomicRefinement {
                strip_height_pixels,
            } => {
                let Some(refinement) = refinement else {
                    return;
                };
                let completed_strip = refinement.completed_strips();
                if completed_strip == 0 {
                    return;
                }
                let y = completed_strip
                    .saturating_sub(1)
                    .saturating_mul(strip_height_pixels);
                let rows = profile
                    .extent
                    .height_pixels()
                    .saturating_sub(y)
                    .min(strip_height_pixels);
                PendingTimingKind::AtomicStrip {
                    frame,
                    completed_strip,
                    total_strips: refinement.total_strips(),
                    strip_work_units: profile.work_units_for_rows(rows),
                }
            }
        };
        if self.pending_timings.len() == MAX_PENDING_TIMINGS {
            self.pending_timings.pop_front();
        }
        self.pending_timings.push_back(PendingTiming {
            execution_id: ticket.execution_id(),
            profile,
            kind,
        });
    }

    pub(crate) fn observe_timing(&mut self, timing: GpuFrameTiming) -> bool {
        let execution_id = timing.ticket().execution_id();
        let Some(index) = self
            .pending_timings
            .iter()
            .position(|pending| pending.execution_id == execution_id)
        else {
            return false;
        };
        let pending = self
            .pending_timings
            .remove(index)
            .expect("a located bounded pending timing remains present");
        let Some(render_ns) = timing.render_pass_ns() else {
            return false;
        };
        match pending.kind {
            PendingTimingKind::Direct => {
                let profile_changed =
                    self.observe_complete_profile(pending.profile.clone(), render_ns);
                self.observe_family_work(
                    &pending.profile,
                    pending.profile.full_work_units(),
                    render_ns,
                ) || profile_changed
            }
            PendingTimingKind::Preview => self.observe_family_work(
                &pending.profile,
                pending.profile.full_work_units(),
                render_ns,
            ),
            PendingTimingKind::AtomicStrip {
                frame,
                completed_strip,
                total_strips,
                strip_work_units,
            } => {
                let family_changed =
                    self.observe_family_work(&pending.profile, strip_work_units, render_ns);
                let profile_changed = self.observe_atomic_strip(
                    frame,
                    pending.profile,
                    completed_strip,
                    total_strips,
                    render_ns,
                );
                family_changed || profile_changed
            }
        }
    }

    pub(crate) fn discard_timing(&mut self, ticket: GpuTimingTicket) {
        if let Some(index) = self
            .pending_timings
            .iter()
            .position(|pending| pending.execution_id == ticket.execution_id())
        {
            self.pending_timings.remove(index);
        }
    }

    pub(crate) fn retire_dataset(&mut self) {
        self.family_calibrations.clear();
        self.profile_observations.clear();
        self.pending_timings.clear();
        self.strip_accumulators.clear();
        self.active_refinement = None;
        self.presented_preview = None;
        self.gesture_preview_policy = None;
        self.latest_candidate_facts.clear();
        #[cfg(test)]
        {
            self.test_work_limits = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_work_limits_for_test(
        &mut self,
        interactive_work_units: u64,
        refinement_work_units: u64,
    ) {
        self.test_work_limits = Some((interactive_work_units.max(1), refinement_work_units.max(1)));
        self.family_calibrations.clear();
        self.profile_observations.clear();
        self.pending_timings.clear();
        self.strip_accumulators.clear();
        self.active_refinement = None;
        self.presented_preview = None;
        self.gesture_preview_policy = None;
        self.latest_candidate_facts.clear();
    }

    fn observe_complete_profile(&mut self, profile: VolumeWorkloadProfile, render_ns: u64) -> bool {
        let now = self.tick();
        if let Some(observation) = self
            .profile_observations
            .iter_mut()
            .find(|observation| observation.profile == profile)
        {
            let previous = observation.certified;
            observation.certified = render_ns <= DIRECT_CERTIFICATION_NS;
            observation.last_use = now;
            return observation.certified != previous;
        }
        if self.profile_observations.len() == MAX_PROFILE_OBSERVATIONS {
            let oldest = self
                .profile_observations
                .iter()
                .enumerate()
                .min_by_key(|(_, observation)| observation.last_use)
                .map(|(index, _)| index)
                .expect("a full observation cache has one oldest entry");
            self.profile_observations.swap_remove(oldest);
        }
        self.profile_observations.push(ProfileObservation {
            profile,
            certified: render_ns <= DIRECT_CERTIFICATION_NS,
            last_use: now,
        });
        true
    }

    fn observe_atomic_strip(
        &mut self,
        frame: FrameIdentity,
        profile: VolumeWorkloadProfile,
        completed_strip: u32,
        total_strips: u32,
        render_ns: u64,
    ) -> bool {
        if total_strips == 0 || completed_strip == 0 || completed_strip > total_strips {
            return false;
        }
        let now = self.tick();
        let accumulator_index = self
            .strip_accumulators
            .iter()
            .position(|accumulator| {
                accumulator.frame == frame
                    && accumulator.profile == profile
                    && accumulator.observed.len() == total_strips as usize
            })
            .unwrap_or_else(|| {
                if self.strip_accumulators.len() == MAX_STRIP_ACCUMULATORS {
                    let oldest = self
                        .strip_accumulators
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, accumulator)| accumulator.last_use)
                        .map(|(index, _)| index)
                        .expect("a full accumulator cache has one oldest entry");
                    self.strip_accumulators.swap_remove(oldest);
                }
                self.strip_accumulators.push(StripAccumulator {
                    frame,
                    profile: profile.clone(),
                    observed: vec![false; total_strips as usize].into_boxed_slice(),
                    total_ns: 0,
                    last_use: now,
                });
                self.strip_accumulators.len() - 1
            });
        let accumulator = &mut self.strip_accumulators[accumulator_index];
        accumulator.last_use = now;
        let observed = &mut accumulator.observed[completed_strip as usize - 1];
        if !*observed {
            *observed = true;
            accumulator.total_ns = accumulator.total_ns.saturating_add(render_ns);
        }
        if accumulator.observed.iter().all(|observed| *observed) {
            let complete = self.strip_accumulators.swap_remove(accumulator_index);
            return self.observe_complete_profile(complete.profile, complete.total_ns);
        }
        false
    }

    fn safe_work_units(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        #[cfg(test)]
        if let Some((interactive, _)) = self.test_work_limits {
            return interactive;
        }
        let index = self.family_calibration_index(profile);
        self.family_calibrations[index].safe_work_units
    }

    fn navigation_is_safe(&self, profile: &VolumeWorkloadProfile) -> bool {
        #[cfg(test)]
        if let Some((interactive, _)) = self.test_work_limits {
            return profile.full_work_units() <= interactive;
        }
        profile.native_navigation_is_safe()
    }

    fn refinement_work_limit(&mut self, profile: &VolumeWorkloadProfile) -> u64 {
        #[cfg(test)]
        if let Some((_, refinement)) = self.test_work_limits {
            return refinement;
        }
        self.safe_work_units(profile)
            .saturating_mul(REFINEMENT_GPU_BUDGET_NS)
            .checked_div(INTERACTION_GPU_BUDGET_NS)
            .unwrap_or(0)
            .max(
                MIN_WORK_UNITS
                    .saturating_mul(REFINEMENT_GPU_BUDGET_NS)
                    .checked_div(INTERACTION_GPU_BUDGET_NS)
                    .unwrap_or(1),
            )
    }

    fn family_calibration_index(&mut self, profile: &VolumeWorkloadProfile) -> usize {
        let now = self.tick();
        if let Some(index) = self
            .family_calibrations
            .iter()
            .position(|calibration| calibration.family == profile.family)
        {
            self.family_calibrations[index].last_use = now;
            return index;
        }
        if self.family_calibrations.len() == MAX_FAMILY_CALIBRATIONS {
            let oldest = self
                .family_calibrations
                .iter()
                .enumerate()
                .min_by_key(|(_, calibration)| calibration.last_use)
                .map(|(index, _)| index)
                .expect("a full family calibration cache has one oldest entry");
            self.family_calibrations.swap_remove(oldest);
        }
        self.family_calibrations.push(FamilyCalibration {
            family: profile.family.clone(),
            safe_work_units: profile.initial_safe_work_units(),
            last_use: now,
        });
        self.family_calibrations.len() - 1
    }

    fn observe_family_work(
        &mut self,
        profile: &VolumeWorkloadProfile,
        observed_work: u64,
        render_ns: u64,
    ) -> bool {
        #[cfg(test)]
        if self.test_work_limits.is_some() {
            return false;
        }
        let index = self.family_calibration_index(profile);
        let calibration = &mut self.family_calibrations[index];
        if render_ns > INTERACTION_GPU_BUDGET_NS {
            let safe = scaled_safe_work(observed_work, render_ns, INTERACTION_GPU_BUDGET_NS);
            let previous = calibration.safe_work_units;
            calibration.safe_work_units = calibration.safe_work_units.min(safe);
            return calibration.safe_work_units != previous;
        }
        // Fast observations do not grow a visible preview envelope. They are
        // retained only as evidence for conservative hidden-strip sizing.
        false
    }

    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }
}

fn extent_pixels(extent: RenderExtent) -> u64 {
    u64::from(extent.width_pixels()).saturating_mul(u64::from(extent.height_pixels()))
}

fn scaled_safe_work(observed_work: u64, render_ns: u64, budget_ns: u64) -> u64 {
    let proportional = observed_work
        .saturating_mul(budget_ns)
        .checked_div(render_ns.max(1))
        .unwrap_or(0);
    proportional
        .saturating_mul(ADAPTIVE_SAFETY_NUMERATOR)
        .checked_div(ADAPTIVE_SAFETY_DENOMINATOR)
        .unwrap_or(0)
        .max(MIN_WORK_UNITS)
}

#[cfg(test)]
mod tests {
    use mirante4d_application::{
        CurrentnessGeneration, RenderGestureKind, RenderIntentBase, RenderIntentMailbox,
        RenderIntentSample, RenderIntentTarget, SourceSessionGeneration,
    };

    use super::*;

    fn profile(extent: RenderExtent, work_per_pixel: u64) -> VolumeWorkloadProfile {
        profile_with(
            extent,
            work_per_pixel,
            work_per_pixel,
            1,
            RenderMode::Mip,
            ScaleLevel::BASE,
        )
    }

    fn profile_with(
        extent: RenderExtent,
        work_per_pixel: u64,
        maximum_steps: u64,
        initial_mode_prior: u64,
        mode: RenderMode,
        scale: ScaleLevel,
    ) -> VolumeWorkloadProfile {
        let camera = CameraView::new(
            Projection::Orthographic,
            mirante4d_domain::WorldPoint3::origin(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            1.0,
            1.0,
        )
        .unwrap();
        VolumeWorkloadProfile {
            extent,
            presentation_viewport: PresentationViewport::new(
                f64::from(extent.width_pixels()),
                f64::from(extent.height_pixels()),
            )
            .unwrap(),
            camera,
            timepoint: TimeIndex::new(0),
            layers: vec![VolumeLayerWorkload {
                layer: LogicalLayerKey::new(0),
                scale,
                render_state: RenderState::mip(SamplingPolicy::VoxelExact),
                maximum_steps,
            }]
            .into_boxed_slice(),
            work_per_pixel,
            initial_prior_work_per_pixel: work_per_pixel.saturating_mul(initial_mode_prior),
            family: VolumeWorkFamily {
                projection: camera.projection(),
                layers: vec![VolumeLayerWorkClass {
                    mode,
                    sampling: SamplingPolicy::VoxelExact,
                    iso_shading: None,
                    iso_display_bucket: None,
                }]
                .into_boxed_slice(),
            },
        }
    }

    fn navigation_candidate(
        profile: VolumeWorkloadProfile,
        available: bool,
    ) -> VolumePreviewCandidate {
        let scale = profile.layers[0].scale;
        VolumePreviewCandidate::navigation(profile, available, false, scale, 1, 1)
    }

    fn cold_navigation_candidate(profile: VolumeWorkloadProfile) -> VolumePreviewCandidate {
        let scale = profile.layers[0].scale;
        VolumePreviewCandidate::navigation(profile, false, true, scale, 1, 1)
    }

    fn target_candidate(profile: VolumeWorkloadProfile) -> VolumePreviewCandidate {
        let scale = profile.layers[0].scale;
        VolumePreviewCandidate::target(profile, true, scale, 1, 1)
    }

    #[test]
    fn cold_terminal_candidate_bootstraps_without_claiming_gpu_residency() {
        let mut controller = VolumePresentationController::default();
        let extent = RenderExtent::new(640, 480).unwrap();
        let target = profile_with(extent, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        let terminal = profile_with(extent, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(6));
        let choice = controller
            .select_preview_profile(&target, &[cold_navigation_candidate(terminal)], None)
            .expect("an uploadable cold terminal rung must start renderer residency");

        assert_eq!(choice.candidate_index, 0);
        let facts = controller.latest_candidate_facts();
        assert_eq!(facts.len(), 1);
        assert!(!facts[0].complete_and_resident);
        assert_eq!(
            facts[0].disposition,
            VolumePreviewCandidateDisposition::SelectedColdBootstrap
        );
    }

    #[test]
    fn visible_direct_policy_is_not_demoted_by_runtime_timing() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(1_000, 1_000).unwrap(), 100);
        assert!(controller.direct_is_safe(&profile));
        controller.observe_complete_profile(profile.clone(), INTERACTION_GPU_BUDGET_NS + 1);
        controller.observe_family_work(
            &profile,
            profile.full_work_units(),
            INTERACTION_GPU_BUDGET_NS * 2,
        );
        assert!(controller.direct_is_safe(&profile));
    }

    #[test]
    fn iso_threshold_bands_have_independent_hidden_strip_calibration() {
        let extent = RenderExtent::new(640, 480).unwrap();
        let mut low_threshold = profile_with(
            extent,
            64,
            64,
            2,
            RenderMode::Isosurface,
            ScaleLevel::new(3),
        );
        low_threshold.family.layers[0].iso_shading = Some(IsoShadingPolicy::Flat);
        low_threshold.family.layers[0].iso_display_bucket = Some(3);
        let mut high_threshold = low_threshold.clone();
        high_threshold.family.layers[0].iso_display_bucket = Some(27);
        let mut controller = VolumePresentationController::default();

        let low_index = controller.family_calibration_index(&low_threshold);
        let high_index = controller.family_calibration_index(&high_threshold);

        assert_ne!(low_index, high_index);
        assert_eq!(controller.family_calibrations.len(), 2);
    }

    #[test]
    fn exact_strip_height_never_exceeds_refinement_work_envelope() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(1_920, 1_080).unwrap(), 4_000);
        let limit = controller.refinement_work_units(&profile);
        let rows = controller.refinement_strip_height(FrameIdentity::new(1), &profile);
        assert!(rows >= 1);
        assert!(profile.work_units_for_rows(rows) <= limit || rows == 1);
    }

    #[test]
    fn complete_strip_observation_can_certify_the_matching_full_profile() {
        let mut controller = VolumePresentationController::default();
        let profile = profile(RenderExtent::new(100, 100).unwrap(), 300_000);
        assert!(!controller.direct_is_safe(&profile));
        controller.observe_atomic_strip(
            FrameIdentity::new(7),
            profile.clone(),
            1,
            2,
            DIRECT_CERTIFICATION_NS / 4,
        );
        assert!(!controller.direct_is_safe(&profile));
        controller.observe_atomic_strip(
            FrameIdentity::new(7),
            profile.clone(),
            2,
            2,
            DIRECT_CERTIFICATION_NS / 4,
        );
        assert!(controller.direct_is_safe(&profile));
    }

    #[test]
    fn direct_certification_does_not_cross_camera_geometry() {
        let mut controller = VolumePresentationController::default();
        let first = profile(RenderExtent::new(100, 100).unwrap(), 300_000);
        let mut moved = first.clone();
        moved.camera = CameraView::new(
            mirante4d_domain::Projection::Orthographic,
            mirante4d_domain::WorldPoint3::new(1.0, 0.0, 0.0).unwrap(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            1.0,
            1.0,
        )
        .unwrap();
        assert!(!controller.direct_is_safe(&first));
        controller.observe_complete_profile(first.clone(), DIRECT_CERTIFICATION_NS / 2);
        assert!(controller.direct_is_safe(&first));
        assert!(!controller.direct_is_safe(&moved));
    }

    #[test]
    fn mode_costs_are_initial_priors_and_slow_families_shrink_hidden_strip_work() {
        let extent = RenderExtent::new(400, 250).unwrap();
        let mip = profile_with(extent, 1_000, 1_000, 1, RenderMode::Mip, ScaleLevel::BASE);
        let dvr = profile_with(extent, 1_000, 1_000, 2, RenderMode::Dvr, ScaleLevel::BASE);
        let mut controller = VolumePresentationController::default();

        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
        assert_eq!(
            controller.safe_work_units(&dvr),
            INITIAL_INTERACTIVE_WORK_UNITS / 2
        );
        assert!(!controller.observe_family_work(&dvr, 64_000_000, 3_000_000));
        assert_eq!(
            controller.safe_work_units(&dvr),
            INITIAL_INTERACTIVE_WORK_UNITS / 2
        );
        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );

        assert!(controller.observe_family_work(&dvr, 128_000_000, 24_000_000));
        assert_eq!(controller.safe_work_units(&dvr), 48_000_000);
        assert_eq!(
            controller.safe_work_units(&mip),
            INITIAL_INTERACTIVE_WORK_UNITS
        );
    }

    #[test]
    fn preview_selection_is_native_resolution_and_uses_static_data_detail() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let floor = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 3_000, 3_000, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(80_000_000, 10_000_000);
        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor.clone(), true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .unwrap();
        assert_eq!(choice.candidate_index(), 0);
        assert_eq!(choice.into_profile().extent(), output);

        let target = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        controller.set_work_limits_for_test(400_000_000, 10_000_000);
        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor, true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .unwrap();
        assert_eq!(choice.candidate_index(), 1);
        assert_eq!(choice.into_profile().extent(), output);
    }

    #[test]
    fn preview_selects_the_finest_complete_safe_intermediate_rung() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let terminal = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(6));
        let s5 = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(5));
        let s4 = profile_with(output, 300, 300, 1, RenderMode::Mip, ScaleLevel::new(4));
        let s3 = profile_with(output, 500, 500, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 3_000, 3_000, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(350_000_000, 10_000_000);

        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(s5, true),
                    navigation_candidate(s4, true),
                    navigation_candidate(s3, true),
                    target_candidate(target.clone()),
                ],
                None,
            )
            .expect("the resident ladder has a safe candidate");

        assert_eq!(choice.candidate_index(), 2);
        assert_eq!(choice.into_profile().layers[0].scale, ScaleLevel::new(4));
        let facts = controller.latest_candidate_facts();
        assert_eq!(
            facts[2].disposition,
            VolumePreviewCandidateDisposition::Selected
        );
        assert_eq!(
            facts[3].disposition,
            VolumePreviewCandidateDisposition::InteractionUnsafe
        );
        assert_eq!(
            facts[4].disposition,
            VolumePreviewCandidateDisposition::InteractionUnsafe
        );
    }

    #[test]
    fn preview_never_selects_a_resident_rung_finer_than_the_current_target() {
        let output = RenderExtent::new(100, 100).unwrap();
        let terminal = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(6));
        let too_fine = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(2));
        let target = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(3));
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(u64::MAX, 10_000_000);

        let choice = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(too_fine, true),
                ],
                None,
            )
            .expect("the terminal body remains eligible");

        assert_eq!(choice.candidate_index(), 0);
        assert_eq!(
            controller.latest_candidate_facts()[1].disposition,
            VolumePreviewCandidateDisposition::FinerThanTarget
        );
    }

    #[test]
    fn losing_a_camera_local_target_makes_one_way_cut_to_finest_resident_rung() {
        let output = RenderExtent::new(100, 100).unwrap();
        let terminal = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(6));
        let intermediate = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(4));
        let target = profile_with(output, 10, 10, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut mailbox = RenderIntentMailbox::new();
        let base = RenderIntentBase::new(
            SourceSessionGeneration::new(1),
            CurrentnessGeneration::initial(),
        );
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                1,
                true,
            )
            .unwrap();
        let gesture = mailbox.snapshot().active_gesture;
        let mut controller = VolumePresentationController::default();
        controller.set_work_limits_for_test(u64::MAX, 10_000_000);

        let first = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal.clone(), true),
                    navigation_candidate(intermediate.clone(), true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(first.into_profile().layers[0].scale, ScaleLevel::new(2));

        let lost = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal.clone(), true),
                    navigation_candidate(intermediate.clone(), true),
                    VolumePreviewCandidate::target(target.clone(), false, ScaleLevel::new(2), 1, 1),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            lost.into_profile().layers[0].scale,
            ScaleLevel::new(4),
            "loss of the camera-local target must choose the finest safe full-volume rung"
        );

        let after_arrival = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(terminal, true),
                    navigation_candidate(intermediate, true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            after_arrival.into_profile().layers[0].scale,
            ScaleLevel::new(4),
            "one gesture must not upgrade again after its one-way full-volume cut"
        );
    }

    #[test]
    fn one_frame_never_replaces_its_presented_preview() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let current = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let proposed = profile_with(output, 400, 400, 1, RenderMode::Mip, ScaleLevel::new(2));
        let frame = FrameIdentity::new(9);
        let mut controller = VolumePresentationController::default();
        controller.note_presented_preview(frame, current, output);
        assert!(!controller.preview_needs_replacement(frame, &proposed, output));
        assert!(controller.preview_needs_replacement(FrameIdentity::new(10), &proposed, output));
    }

    #[test]
    fn one_gesture_freezes_floor_even_if_a_finer_body_arrives() {
        let output = RenderExtent::new(1_000, 1_000).unwrap();
        let floor = profile_with(output, 200, 200, 1, RenderMode::Mip, ScaleLevel::new(3));
        let target = profile_with(output, 100, 100, 1, RenderMode::Mip, ScaleLevel::new(2));
        let mut mailbox = RenderIntentMailbox::new();
        let base = RenderIntentBase::new(
            SourceSessionGeneration::new(1),
            CurrentnessGeneration::initial(),
        );
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                1,
                true,
            )
            .unwrap();
        let gesture = mailbox.snapshot().active_gesture;
        let mut controller = VolumePresentationController::default();

        let first = controller
            .select_preview_profile(
                &target,
                &[navigation_candidate(floor.clone(), true)],
                gesture,
            )
            .unwrap();
        assert_eq!(first.candidate_index(), 0);
        let after_arrival = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor.clone(), true),
                    target_candidate(target.clone()),
                ],
                gesture,
            )
            .unwrap();
        assert_eq!(
            after_arrival.into_profile().layers[0].scale,
            floor.layers[0].scale,
            "resource arrival must not upgrade the visible body during one gesture"
        );

        mailbox.finish(base, RenderIntentTarget::ThreeD).unwrap();
        mailbox
            .sample(
                base,
                RenderIntentSample::camera(RenderGestureKind::Drag, target.camera),
                2,
                true,
            )
            .unwrap();
        let next_gesture = mailbox.snapshot().active_gesture;
        let next = controller
            .select_preview_profile(
                &target,
                &[
                    navigation_candidate(floor, true),
                    target_candidate(target.clone()),
                ],
                next_gesture,
            )
            .unwrap();
        assert_eq!(next.into_profile().layers[0].scale, target.layers[0].scale);
    }

    #[test]
    fn learned_strip_size_changes_only_for_the_next_exact_candidate() {
        let profile = profile(RenderExtent::new(1_920, 1_080).unwrap(), 4_000);
        let mut controller = VolumePresentationController::default();
        let first_frame = FrameIdentity::new(3);
        let first = controller.refinement_strip_height(first_frame, &profile);
        assert!(first > 1);
        assert!(controller.observe_family_work(&profile, 32_000_000, 24_000_000));
        assert_eq!(
            controller.refinement_strip_height(first_frame, &profile),
            first
        );
        assert!(
            controller.refinement_strip_height(FrameIdentity::new(4), &profile) < first,
            "new timing must resize future work without restarting the active candidate"
        );
    }

    #[test]
    fn calibration_cache_is_bounded_and_dataset_retirement_clears_adaptation() {
        let mut controller = VolumePresentationController::default();
        for layer_count in 1..=(MAX_FAMILY_CALIBRATIONS + 5) {
            let mut profile = profile(RenderExtent::new(64, 64).unwrap(), 10);
            profile.family.layers = vec![
                VolumeLayerWorkClass {
                    mode: RenderMode::Mip,
                    sampling: SamplingPolicy::VoxelExact,
                    iso_shading: None,
                    iso_display_bucket: None,
                };
                layer_count
            ]
            .into_boxed_slice();
            let _ = controller.safe_work_units(&profile);
        }
        assert_eq!(
            controller.family_calibrations.len(),
            MAX_FAMILY_CALIBRATIONS
        );
        controller.retire_dataset();
        assert!(controller.family_calibrations.is_empty());
        assert!(controller.presented_preview.is_none());
        assert!(controller.active_refinement.is_none());
    }
}
