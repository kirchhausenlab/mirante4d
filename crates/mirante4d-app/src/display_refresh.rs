use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use super::*;
use crate::{
    cross_section_scheduler::{CrossSectionScheduleInput, schedule_cross_section_panel},
    dataset_demand_plan::cross_section_projected_layer_scales,
    dataset_requests::{
        SCOPE_CROSS_SECTION_XY, SCOPE_CROSS_SECTION_XZ, SCOPE_CROSS_SECTION_YZ, SCOPE_CURRENT_3D,
        SCOPE_CURRENT_3D_REFINEMENT, ScopeReconciliationTargets,
    },
    native_presentation::{
        CompletedCoordinatedValidationCapture, ProductLayerRequirementFacts,
        install_if_current_texture_revision,
    },
    presentation_scheduler::{
        FixedPresentationTargetRequests, PresentationQuality, PresentationTransaction,
        PresentationTransactionMember, PresentationTransactionTargets,
    },
    product_render_intent::{ProductRenderRequest, cross_section_intent, volume_intent},
    shader_work_envelope_cache::{ShaderWorkEnvelopeBuildError, ShaderWorkEnvelopeLookup},
    viewer_layout::PanelId,
    volume_presentation::{
        VolumePreviewCandidate, VolumePreviewCandidateDisposition, VolumeWorkloadProfile,
    },
    workbench_brick_runtime::{
        first_useful_resources_complete_with_renderer,
        first_useful_resources_uploadable_with_renderer, scope_resources_complete_with_renderer,
    },
};
use mirante4d_domain::{CrossSectionView, RenderMode};
use mirante4d_project_model::ViewState;
use mirante4d_render_api::{
    FrameCompleteness as RenderFrameCompleteness, FrameIdentity, FrameLimitation,
    MAX_RENDER_LAYERS, PreparedRenderRequirements, PresentationTarget, PresentedFrame,
    RenderExtent, RenderRequirements, RenderViewIntent,
};
use mirante4d_render_wgpu::{
    CoordinatedFrameExecutionReport, CoordinatedLogicalTargetSet, CoordinatedMemberDisposition,
    CoordinatedPublicationGroup, CoordinatedTargetExecutionReport, CoordinatedTargetLayout,
    CoordinatedTargetRequest, CpuFrameTiming, HiddenRefinementCapabilityFailure,
    HiddenRefinementFailure, HiddenRefinementState, PipelineCapability,
    PipelineCompilationFailureCause, RetainedFrameRenderPolicy, VolumeColorSchedule,
    WgpuRenderRuntimeError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeRenderPlanSource {
    Scope(u64),
    Navigation(usize),
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PerformanceMilestoneObservation {
    pub(crate) first_useful: bool,
    pub(crate) complete_replacement: bool,
    pub(crate) complete_coarse: bool,
    pub(crate) target_current: bool,
    pub(crate) foreground_idle: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LayerPerformanceMilestones {
    layer_ordinal: Option<u32>,
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
}

impl LayerPerformanceMilestones {
    pub(crate) const fn layer_ordinal(self) -> Option<u32> {
        self.layer_ordinal
    }

    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PanelPerformanceMilestones {
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
    visible_layers: [LayerPerformanceMilestones; MAX_RENDER_LAYERS],
    visible_layer_overflow: bool,
}

impl Default for PanelPerformanceMilestones {
    fn default() -> Self {
        Self {
            first_current_presented_ms: None,
            first_useful_frame_ms: None,
            complete_coarse_ms: None,
            complete_replacement_ms: None,
            target_settled_ms: None,
            visible_layers: [LayerPerformanceMilestones::default(); MAX_RENDER_LAYERS],
            visible_layer_overflow: false,
        }
    }
}

impl PanelPerformanceMilestones {
    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }

    pub(crate) const fn visible_layers(&self) -> &[LayerPerformanceMilestones; MAX_RENDER_LAYERS] {
        &self.visible_layers
    }

    pub(crate) const fn visible_layer_overflow(self) -> bool {
        self.visible_layer_overflow
    }
}

/// Exact immutable inputs for one product color attempt.
///
/// The requirement body deliberately uses allocation identity: replacing a
/// prepared body is a real planning event even when it happens to contain the
/// same keys. CPU lease arrival is deliberately absent: relevant residency
/// owns an exact keyed wake instead of becoming a global retry clock.
#[derive(Debug, Clone)]
pub(crate) struct RenderAttemptFingerprint {
    source_generation: SourceSessionGeneration,
    layout: CanonicalViewerLayout,
    frames: [Option<FrameIdentity>; 4],
    timepoints: [Option<TimeIndex>; 4],
    requirements: [Option<RenderRequirements>; 4],
    surface_generations: [Option<u64>; 4],
    extents: [Option<RenderExtent>; 4],
    layer_scales: [Option<Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>>; 4],
    schedules: [Option<VolumeColorSchedule>; 4],
    dataset_runtime_epoch: u64,
    cpu_capacity_epoch: u64,
    renderer_device_generation: u64,
}

impl RenderAttemptFingerprint {
    fn new(
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
        dataset_runtime_epoch: u64,
        cpu_capacity_epoch: u64,
        renderer_device_generation: u64,
    ) -> Self {
        let mut frames = [None; 4];
        let mut timepoints = [None; 4];
        let mut requirements = std::array::from_fn(|_| None);
        let mut surface_generations = [None; 4];
        let mut extents = [None; 4];
        let mut layer_scales = std::array::from_fn(|_| None);
        let mut schedules = [None; 4];
        for request in requests {
            let index = request.target.index();
            frames[index] = Some(request.request.intent.frame());
            timepoints[index] = Some(request.request.intent.timepoint());
            requirements[index] = Some(request.request.requirements.clone());
            surface_generations[index] = Some(request.surface_generation);
            extents[index] = Some(request.output_extent);
            layer_scales[index] = Some(Arc::clone(&request.layer_scales));
            schedules[index] = Some(request.volume_schedule);
        }
        Self {
            source_generation: snapshot.source_generation(),
            layout: application_view(snapshot).layout(),
            frames,
            timepoints,
            requirements,
            surface_generations,
            extents,
            layer_scales,
            schedules,
            dataset_runtime_epoch,
            cpu_capacity_epoch,
            renderer_device_generation,
        }
    }

    fn matches_current(&self, current: &Self) -> bool {
        self.source_generation == current.source_generation
            && self.layout == current.layout
            && self.frames == current.frames
            && self.timepoints == current.timepoints
            && self
                .requirements
                .iter()
                .zip(&current.requirements)
                .all(|(left, right)| match (left, right) {
                    (Some(left), Some(right)) => {
                        left.shares_resources_with(right)
                            && left.prefetch_promoted() == right.prefetch_promoted()
                    }
                    (None, None) => true,
                    _ => false,
                })
            && self.surface_generations == current.surface_generations
            && self.extents == current.extents
            && self.schedules == current.schedules
            && self
                .layer_scales
                .iter()
                .zip(&current.layer_scales)
                .all(|(left, right)| match (left, right) {
                    (Some(left), Some(right)) => left.as_ref() == right.as_ref(),
                    (None, None) => true,
                    _ => false,
                })
            && self.dataset_runtime_epoch == current.dataset_runtime_epoch
            && self.cpu_capacity_epoch == current.cpu_capacity_epoch
            && self.renderer_device_generation == current.renderer_device_generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderWaitReason {
    LogicalMember,
    MailboxAdvance,
    CandidatePlan,
    InitialPipeline,
    SubmissionCleanup,
    EvictionAcknowledgement,
    RelevantResidency,
    HiddenWorker,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderWaitKey {
    LogicalMember {
        target: PresentationTarget,
        family_revision: u64,
    },
    MailboxAdvance {
        target: PresentationTarget,
        minimum_frame: u64,
    },
    CandidatePlan {
        demand_revision: u64,
    },
    InitialPipeline {
        device_generation: u64,
    },
    SubmissionCleanup {
        submission: u64,
    },
    EvictionAcknowledgement {
        ledger_revision: u64,
    },
    RelevantResidency {
        target: PresentationTarget,
        requirements: RenderRequirements,
    },
    HiddenWorker {
        target: PresentationTarget,
        job: u64,
    },
}

impl RenderWaitKey {
    const fn reason(&self) -> RenderWaitReason {
        match self {
            Self::LogicalMember { .. } => RenderWaitReason::LogicalMember,
            Self::MailboxAdvance { .. } => RenderWaitReason::MailboxAdvance,
            Self::CandidatePlan { .. } => RenderWaitReason::CandidatePlan,
            Self::InitialPipeline { .. } => RenderWaitReason::InitialPipeline,
            Self::SubmissionCleanup { .. } => RenderWaitReason::SubmissionCleanup,
            Self::EvictionAcknowledgement { .. } => RenderWaitReason::EvictionAcknowledgement,
            Self::RelevantResidency { .. } => RenderWaitReason::RelevantResidency,
            Self::HiddenWorker { .. } => RenderWaitReason::HiddenWorker,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WaitingWake {
    Event(RenderWaitKey),
    #[allow(
        dead_code,
        reason = "bounded auxiliary map polling is installed by the Pick/capture cutover"
    )]
    After {
        reason: RenderWaitReason,
        interval: Duration,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum RenderAttemptState {
    Idle,
    Ready {
        fingerprint: RenderAttemptFingerprint,
    },
    Waiting {
        fingerprint: RenderAttemptFingerprint,
        reason: RenderWaitReason,
        wake: WaitingWake,
    },
    Failed {
        fingerprint: RenderAttemptFingerprint,
        failure: ResidentRenderFailureStatus,
    },
    ColorUnavailable {
        failure: ResidentRenderFailureStatus,
    },
    RendererTerminal {
        failure: ResidentRenderFailureStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenderWake {
    Immediate,
    Waiting(WaitingWake),
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct RenderAttemptCoordinator {
    state: RenderAttemptState,
    ready_pending_admission: bool,
    pending_composition: [Option<(u64, u64)>; 4],
    acknowledged_composition: [Option<(u64, u64)>; 4],
    execution_decisions: u64,
    wait_decisions: u64,
    failure_decisions: u64,
}

impl Default for RenderAttemptCoordinator {
    fn default() -> Self {
        Self {
            state: RenderAttemptState::Idle,
            ready_pending_admission: false,
            pending_composition: [None; 4],
            acknowledged_composition: [None; 4],
            execution_decisions: 0,
            wait_decisions: 0,
            failure_decisions: 0,
        }
    }
}

impl RenderAttemptCoordinator {
    pub(crate) const fn state(&self) -> &RenderAttemptState {
        &self.state
    }

    pub(crate) const fn renderer_is_terminal(&self) -> bool {
        matches!(&self.state, RenderAttemptState::RendererTerminal { .. })
    }

    pub(crate) fn begin(&mut self, fingerprint: RenderAttemptFingerprint) -> bool {
        match &self.state {
            RenderAttemptState::Ready {
                fingerprint: previous,
            } if previous.matches_current(&fingerprint) => {
                if !self.ready_pending_admission {
                    return false;
                }
            }
            RenderAttemptState::Failed {
                fingerprint: previous,
                ..
            }
            | RenderAttemptState::Waiting {
                fingerprint: previous,
                ..
            } if previous.matches_current(&fingerprint) => return false,
            RenderAttemptState::Waiting {
                wake:
                    WaitingWake::Event(RenderWaitKey::MailboxAdvance {
                        target,
                        minimum_frame,
                    }),
                ..
            } if fingerprint.frames[target.index()]
                .is_none_or(|frame| frame.get() < *minimum_frame) =>
            {
                return false;
            }
            RenderAttemptState::ColorUnavailable { .. }
            | RenderAttemptState::RendererTerminal { .. } => return false,
            RenderAttemptState::Idle
            | RenderAttemptState::Ready { .. }
            | RenderAttemptState::Waiting { .. }
            | RenderAttemptState::Failed { .. } => {}
        }
        self.execution_decisions = self.execution_decisions.saturating_add(1);
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::Ready { fingerprint };
        true
    }

    /// Authorizes exactly one successor execution for the fingerprint whose
    /// completed report proved that immediately actionable renderer work
    /// remains. This is distinct from merely being in `Ready`: `begin` puts
    /// the admitted attempt in that state while it executes, and an unchanged
    /// UI turn must not manufacture another admission without this report or
    /// a matching causal wake.
    pub(crate) fn continue_ready(&mut self, fingerprint: &RenderAttemptFingerprint) {
        let RenderAttemptState::Ready {
            fingerprint: admitted,
        } = &self.state
        else {
            debug_assert!(false, "only an admitted ready attempt can continue");
            return;
        };
        if !admitted.matches_current(fingerprint) {
            debug_assert!(false, "a render report cannot continue a different attempt");
            return;
        }
        self.ready_pending_admission = true;
    }

    pub(crate) fn wait(&mut self, fingerprint: RenderAttemptFingerprint, key: RenderWaitKey) {
        let reason = key.reason();
        self.wait_decisions = self.wait_decisions.saturating_add(1);
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::Waiting {
            fingerprint,
            reason,
            wake: WaitingWake::Event(key),
        };
    }

    pub(crate) fn fail(
        &mut self,
        fingerprint: RenderAttemptFingerprint,
        failure: ResidentRenderFailureStatus,
    ) {
        self.failure_decisions = self.failure_decisions.saturating_add(1);
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::Failed {
            fingerprint,
            failure,
        };
    }

    pub(crate) fn settle(&mut self) {
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::Idle;
    }

    pub(crate) fn color_unavailable(&mut self, failure: ResidentRenderFailureStatus) {
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::ColorUnavailable { failure };
    }

    pub(crate) fn renderer_terminal(&mut self, failure: ResidentRenderFailureStatus) {
        self.ready_pending_admission = false;
        self.state = RenderAttemptState::RendererTerminal { failure };
        self.pending_composition = [None; 4];
    }

    pub(crate) fn note_published_texture(
        &mut self,
        target: PresentationTarget,
        device_generation: u64,
        texture_revision: u64,
    ) {
        let identity = (device_generation, texture_revision);
        let index = target.index();
        if self.acknowledged_composition[index].is_some_and(|acknowledged| acknowledged >= identity)
            || self.pending_composition[index].is_some_and(|pending| pending >= identity)
        {
            return;
        }
        self.pending_composition[index] = Some(identity);
    }

    pub(crate) fn acknowledge_composition(
        &mut self,
        target: PresentationTarget,
        identity: Option<(u64, u64)>,
    ) {
        let index = target.index();
        if self.pending_composition[index] == identity {
            self.acknowledged_composition[index] = identity;
            self.pending_composition[index] = None;
        }
    }

    /// Retires a texture identity whose target intentionally became a
    /// background/empty surface. There will be no later image paint capable
    /// of acknowledging that identity, so retaining it would manufacture an
    /// immediate repaint loop after the semantic surface was already current.
    pub(crate) fn retire_composition(&mut self, target: PresentationTarget) {
        self.pending_composition[target.index()] = None;
    }

    pub(crate) fn wake(&self) -> RenderWake {
        match &self.state {
            RenderAttemptState::Ready { .. } if self.ready_pending_admission => {
                RenderWake::Immediate
            }
            RenderAttemptState::Waiting { wake, .. } => RenderWake::Waiting(wake.clone()),
            RenderAttemptState::Idle
            | RenderAttemptState::Ready { .. }
            | RenderAttemptState::Failed { .. }
            | RenderAttemptState::ColorUnavailable { .. }
            | RenderAttemptState::RendererTerminal { .. } => {
                if self.pending_composition.iter().any(Option::is_some) {
                    RenderWake::Immediate
                } else {
                    RenderWake::None
                }
            }
        }
    }

    pub(crate) fn target_ready(&self, target: PresentationTarget) -> bool {
        self.ready_pending_admission
            && matches!(
            &self.state,
            RenderAttemptState::Ready { fingerprint }
                if fingerprint.frames[target.index()].is_some()
            )
    }

    pub(crate) fn observe_renderer_events(&mut self, events: RendererEventBatch) {
        let RenderAttemptState::Waiting {
            fingerprint,
            wake: WaitingWake::Event(key),
            ..
        } = &self.state
        else {
            return;
        };
        let matches = match key {
            RenderWaitKey::InitialPipeline { device_generation } => {
                events.initial_pipeline_changed()
                    && events.renderer_device_generation == *device_generation
            }
            RenderWaitKey::SubmissionCleanup { submission } => events
                .submission_completed()
                .is_some_and(|completed| completed >= *submission),
            RenderWaitKey::HiddenWorker { job, .. } => events
                .hidden_worker_result()
                .is_some_and(|completed| completed == *job),
            RenderWaitKey::LogicalMember { .. }
            | RenderWaitKey::MailboxAdvance { .. }
            | RenderWaitKey::CandidatePlan { .. }
            | RenderWaitKey::EvictionAcknowledgement { .. }
            | RenderWaitKey::RelevantResidency { .. } => false,
        };
        if !matches {
            return;
        }
        self.ready_pending_admission = true;
        self.state = RenderAttemptState::Ready {
            fingerprint: fingerprint.clone(),
        };
    }

    pub(crate) fn observe_eviction_acknowledgement(&mut self, ledger_revision: u64) {
        let RenderAttemptState::Waiting {
            fingerprint,
            wake:
                WaitingWake::Event(RenderWaitKey::EvictionAcknowledgement {
                    ledger_revision: awaited,
                }),
            ..
        } = &self.state
        else {
            return;
        };
        if ledger_revision < *awaited {
            return;
        }
        self.ready_pending_admission = true;
        self.state = RenderAttemptState::Ready {
            fingerprint: fingerprint.clone(),
        };
    }

    pub(crate) fn observe_relevant_residency_offer(
        &mut self,
        resource: mirante4d_dataset::BrickKey,
    ) {
        let RenderAttemptState::Waiting {
            fingerprint,
            wake: WaitingWake::Event(RenderWaitKey::RelevantResidency { requirements, .. }),
            ..
        } = &self.state
        else {
            return;
        };
        if !requirements.is_required_resource(resource) {
            return;
        }
        self.ready_pending_admission = true;
        self.state = RenderAttemptState::Ready {
            fingerprint: fingerprint.clone(),
        };
    }

    pub(crate) fn failure(&self) -> Option<&ResidentRenderFailureStatus> {
        match &self.state {
            RenderAttemptState::Failed { failure, .. }
            | RenderAttemptState::ColorUnavailable { failure }
            | RenderAttemptState::RendererTerminal { failure } => Some(failure),
            _ => None,
        }
    }

    pub(crate) const fn wait_reason(&self) -> Option<RenderWaitReason> {
        match &self.state {
            RenderAttemptState::Waiting { reason, .. } => Some(*reason),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorRuntimeDisposition {
    ColorUnavailable,
    RendererTerminal,
    Wait(RenderWaitReason),
    Stale,
    PlacementRecovery,
    DeterministicFailure,
    AuxiliaryOnly,
}

/// Total color-boundary classification. This intentionally has no wildcard:
/// a new renderer error cannot silently inherit retry or failure semantics.
const fn classify_color_runtime_error(error: WgpuRenderRuntimeError) -> ColorRuntimeDisposition {
    use ColorRuntimeDisposition as Disposition;
    use WgpuRenderRuntimeError as Error;
    match error {
        Error::InvalidConfiguration
        | Error::SoftwareAdapter
        | Error::UnsupportedBackend
        | Error::AdapterLimitsInsufficient
        | Error::DeviceLimitsInsufficient
        | Error::PipelineCompilerSpawnFailed
        | Error::RendererDeviceGenerationExhausted => Disposition::ColorUnavailable,
        Error::DeviceLost
        | Error::DeviceOutOfMemory
        | Error::BackendInternal
        | Error::BackendValidation => Disposition::RendererTerminal,
        Error::PipelineCompilationFailed { capability, cause } => match cause {
            PipelineCompilationFailureCause::DeviceOutOfMemory
            | PipelineCompilationFailureCause::BackendInternal => Disposition::RendererTerminal,
            PipelineCompilationFailureCause::Validation
            | PipelineCompilationFailureCause::WorkerPanicked
            | PipelineCompilationFailureCause::WorkerStopped => match capability {
                PipelineCapability::InitialRender => Disposition::ColorUnavailable,
                PipelineCapability::Pick => Disposition::AuxiliaryOnly,
            },
        },
        Error::PipelineNotReady { capability } => match capability {
            PipelineCapability::InitialRender => {
                Disposition::Wait(RenderWaitReason::InitialPipeline)
            }
            PipelineCapability::Pick => Disposition::AuxiliaryOnly,
        },
        Error::StaleFrame { .. } => Disposition::Stale,
        Error::ResidencyEvictionEventCapacityExceeded { .. } => {
            Disposition::Wait(RenderWaitReason::EvictionAcknowledgement)
        }
        Error::PayloadRecoveryDeferred => Disposition::Wait(RenderWaitReason::SubmissionCleanup),
        Error::PayloadPlacementUnavailable { .. } => Disposition::PlacementRecovery,
        Error::UnknownValidationCapture
        | Error::StaleValidationCapture
        | Error::ValidationCaptureFailed
        | Error::UnknownGpuTiming
        | Error::GpuTimingFailed
        | Error::PickQueryMismatch
        | Error::PickFrameUnavailable
        | Error::PickCapacityExceeded
        | Error::PickTicketExhausted
        | Error::PickBackpressure
        | Error::UnknownVolumePick
        | Error::VolumePickFailed => Disposition::AuxiliaryOnly,
        Error::FrameContractMismatch
        | Error::ExtentExceeded
        | Error::RequirementSetChanged
        | Error::InvalidResourceGridCatalog
        | Error::RequirementCapacityExceeded { .. }
        | Error::PresentationCapacityExceeded { .. }
        | Error::PresentationNotRegistered
        | Error::PrivatePresentationIdExhausted
        | Error::TextureRevisionExhausted
        | Error::DuplicateCoordinatedTarget { .. }
        | Error::CoordinatedTargetNotConfigured { .. }
        | Error::CoordinatedTargetViewMismatch { .. }
        | Error::InvalidVolumeColorSchedule { .. }
        | Error::InvalidCoordinatedPublicationGroup
        | Error::CoordinatedTargetExtentMismatch { .. }
        | Error::LeaseCapacityExceeded { .. }
        | Error::DuplicateLease
        | Error::UnexpectedLease
        | Error::PayloadContractMismatch
        | Error::UnsupportedView
        | Error::ShaderAdmission(_)
        | Error::ShaderWorkEnvelopeMismatch
        | Error::CoordinateLimitExceeded
        | Error::ControlCapacityExceeded
        | Error::CapacityExceeded { .. }
        | Error::ResidentMetadataCapacityExceeded { .. }
        | Error::FrameProgressContract => Disposition::DeterministicFailure,
    }
}

fn hidden_refinement_failure_status(
    state: HiddenRefinementState,
) -> Option<ResidentRenderFailureStatus> {
    match state {
        HiddenRefinementState::Failed(HiddenRefinementFailure::SubmissionTimedOutTwice) => {
            Some(ResidentRenderFailureStatus::new(
                FrameFailureKind::AllocationFailed,
                "hidden exact refinement timed out twice for this render request",
            ))
        }
        HiddenRefinementState::CapabilityFailed(cause) => {
            let message = match cause {
                HiddenRefinementCapabilityFailure::WorkerSpawnFailed => {
                    "hidden exact refinement worker could not be started"
                }
                HiddenRefinementCapabilityFailure::WorkerPanicked => {
                    "hidden exact refinement worker stopped after a panic"
                }
                HiddenRefinementCapabilityFailure::JobIdentityExhausted => {
                    "hidden exact refinement job identities are exhausted"
                }
            };
            Some(ResidentRenderFailureStatus::new(
                FrameFailureKind::BackendLimit,
                message,
            ))
        }
        HiddenRefinementState::Running(_)
        | HiddenRefinementState::WaitingForSubmission { .. }
        | HiddenRefinementState::RetryReady
        | HiddenRefinementState::Complete(_) => None,
    }
}

#[derive(Debug, Clone)]
enum ColorAttemptObservation {
    Current,
    Ready,
    Waiting(RenderWaitKey),
    Failed(ResidentRenderFailureStatus),
}

#[derive(Debug, Clone, Copy)]
struct ColorAttemptTargetFacts {
    target: PresentationTarget,
    current: bool,
    presented: bool,
    deferred_by_backpressure: bool,
    actionable_work_remaining: bool,
    hidden_waiting: bool,
    hidden_refinement: Option<HiddenRefinementState>,
}

#[derive(Debug, Clone)]
enum ColorAttemptClass {
    Current,
    Ready,
    SubmissionWait,
    HiddenWait(PresentationTarget),
    RelevantResidency(PresentationTarget),
    Failed(ResidentRenderFailureStatus),
}

fn classify_color_attempt_facts(facts: &[ColorAttemptTargetFacts]) -> ColorAttemptClass {
    if let Some(failure) = facts
        .iter()
        .filter_map(|target| target.hidden_refinement)
        .find_map(hidden_refinement_failure_status)
    {
        return ColorAttemptClass::Failed(failure);
    }
    if facts.iter().all(|target| target.current) {
        return ColorAttemptClass::Current;
    }
    if facts.iter().any(|target| target.deferred_by_backpressure) {
        return ColorAttemptClass::SubmissionWait;
    }
    if let Some(target) = facts.iter().find(|target| target.hidden_waiting) {
        return ColorAttemptClass::HiddenWait(target.target);
    }
    if facts.iter().any(|target| target.actionable_work_remaining) {
        return ColorAttemptClass::Ready;
    }
    ColorAttemptClass::RelevantResidency(
        facts
            .iter()
            .find(|target| !target.current)
            .map_or(PresentationTarget::ThreeD, |target| target.target),
    )
}

const fn report_member_should_apply(
    facts: ColorAttemptTargetFacts,
    disposition: CoordinatedMemberDisposition,
) -> bool {
    facts.presented
        || (facts.current && matches!(disposition, CoordinatedMemberDisposition::Reused))
}

/// Normalizes the complete renderer report before any presentation or UI
/// state is changed. In particular, neutral backpressure and incomplete
/// residency never travel through the generic failure/fidelity path.
fn normalize_color_attempt_observation(
    report: &CoordinatedFrameExecutionReport,
    requests: &[CoordinatedOwnedRequest],
    active_target: PresentationTarget,
    next_submission: u64,
) -> ColorAttemptObservation {
    let facts = report
        .targets()
        .iter()
        .map(|target| {
            let hidden_refinement = target.hidden_refinement();
            ColorAttemptTargetFacts {
                target: target.target(),
                current: target.current(),
                presented: target.presented(),
                deferred_by_backpressure: target.deferred_by_backpressure(),
                actionable_work_remaining: target.actionable_work_remaining(),
                hidden_waiting: matches!(
                    hidden_refinement,
                    Some(
                        HiddenRefinementState::Running(_)
                            | HiddenRefinementState::WaitingForSubmission { .. }
                    )
                ),
                hidden_refinement,
            }
        })
        .collect::<Vec<_>>();
    match classify_color_attempt_facts(&facts) {
        ColorAttemptClass::Current => ColorAttemptObservation::Current,
        ColorAttemptClass::Ready => ColorAttemptObservation::Ready,
        ColorAttemptClass::SubmissionWait => {
            ColorAttemptObservation::Waiting(RenderWaitKey::SubmissionCleanup {
                submission: next_submission,
            })
        }
        ColorAttemptClass::HiddenWait(target) => {
            ColorAttemptObservation::Waiting(RenderWaitKey::HiddenWorker {
                target,
                job: report
                    .target(target)
                    .and_then(CoordinatedTargetExecutionReport::hidden_refinement_job)
                    .expect("a renderer-reported hidden wait contains its exact job identity"),
            })
        }
        ColorAttemptClass::RelevantResidency(target) => {
            let residency_request = requests
                .iter()
                .find(|request| request.target == target)
                .or_else(|| {
                    requests
                        .iter()
                        .find(|request| request.target == active_target)
                })
                .expect("a coordinated report belongs to one prepared request");
            ColorAttemptObservation::Waiting(RenderWaitKey::RelevantResidency {
                target: residency_request.target,
                requirements: residency_request.request.requirements.clone(),
            })
        }
        ColorAttemptClass::Failed(failure) => ColorAttemptObservation::Failed(failure),
    }
}

pub(crate) const fn display_refresh_path_label(path: DisplayRefreshPath) -> &'static str {
    match path {
        DisplayRefreshPath::GpuResidentDisplay => "gpu display",
        DisplayRefreshPath::UiBackground => "ui background",
    }
}

fn retained_frame_render_policy(
    publish_to_display: bool,
    existing_presentation: Option<RenderFrameCompleteness>,
) -> RetainedFrameRenderPolicy {
    let replacing_settled_pixels = existing_presentation
        .is_some_and(|completeness| completeness != RenderFrameCompleteness::Progressive);
    if publish_to_display && !replacing_settled_pixels {
        RetainedFrameRenderPolicy::EveryUsefulFrame
    } else {
        RetainedFrameRenderPolicy::ExactFrameOnly
    }
}

fn cross_section_schedule_for_presented_coverage(
    mut schedule: CrossSectionPanelScheduleState,
    completeness: RenderFrameCompleteness,
    available_requirements: u64,
    total_requirements: u64,
) -> CrossSectionPanelScheduleState {
    debug_assert!(available_requirements <= total_requirements);
    schedule.selected_bricks = usize::try_from(total_requirements).unwrap_or(usize::MAX);
    schedule.occupied_selected_bricks =
        usize::try_from(available_requirements).unwrap_or(usize::MAX);
    schedule.missing_occupied_bricks = schedule
        .selected_bricks
        .saturating_sub(schedule.occupied_selected_bricks);
    if completeness == RenderFrameCompleteness::Progressive {
        schedule.status = CrossSectionPanelScheduleStatus::Incomplete;
    }
    schedule
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayRenderTiming {
    pub(crate) path: DisplayRefreshPath,
    pub(crate) render_ms: f64,
    pub(crate) gpu_upload_ms: Option<f64>,
    pub(crate) gpu_compute_ms: Option<f64>,
    pub(crate) egui_texture_ms: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DisplayRefreshWorkTiming {
    render: DisplayRenderTiming,
    visible_brick_request_ms: f64,
}

/// What changed in the fixed viewer composition during one coordinated
/// renderer observation.
///
/// A texture publication and an intentional background/empty publication are
/// equally visible changes, but only the former necessarily has a GPU color
/// submission. Keeping the distinction typed prevents callers from once
/// again treating a renderer counter as composition authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisplayCompositionChange {
    Unchanged,
    TexturePublished,
    SurfaceCleared,
    TexturePublishedAndSurfaceCleared,
}

impl DisplayCompositionChange {
    pub(crate) const fn requires_composition_turn(self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    const fn from_observations(texture_published: bool, surface_cleared: bool) -> Self {
        match (texture_published, surface_cleared) {
            (false, false) => Self::Unchanged,
            (true, false) => Self::TexturePublished,
            (false, true) => Self::SurfaceCleared,
            (true, true) => Self::TexturePublishedAndSurfaceCleared,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayCompositionBackground {
    ThreeD(RenderBackend),
    CrossSection {
        active: bool,
        schedule: Option<CrossSectionPanelScheduleState>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayTargetCompositionSnapshot {
    active: bool,
    frame: Option<PresentedFrame>,
    texture_binding: Option<(u64, u64)>,
    background: DisplayCompositionBackground,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisplayCompositionSnapshot {
    targets: [DisplayTargetCompositionSnapshot; 4],
}

impl DisplayCompositionSnapshot {
    fn capture(app: &MiranteWorkbenchApp) -> Self {
        let snapshot = app.application.snapshot();
        let four_panel = application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel;
        Self {
            targets: PresentationTarget::ALL.map(|target| {
                let active = target == PresentationTarget::ThreeD || four_panel;
                let surface = app.render_coordination.surface(target);
                let background = if target == PresentationTarget::ThreeD {
                    DisplayCompositionBackground::ThreeD(
                        app.render_coordination.frame_fidelity.backend,
                    )
                } else {
                    DisplayCompositionBackground::CrossSection {
                        active,
                        schedule: active.then(|| surface.cross_section_schedule()).flatten(),
                    }
                };
                DisplayTargetCompositionSnapshot {
                    active,
                    frame: surface.presented_frame().cloned(),
                    texture_binding: app.native_presentation.texture_binding_identity(target),
                    background,
                }
            }),
        }
    }

    fn change_from(&self, before: &Self) -> DisplayCompositionChange {
        let mut texture_published = false;
        let mut surface_cleared = false;
        for (before, after) in before.targets.iter().zip(&self.targets) {
            if before == after {
                continue;
            }
            if before.active != after.active {
                if after.active && after.frame.is_some() {
                    texture_published = true;
                } else {
                    surface_cleared = true;
                }
            }
            if before.frame != after.frame {
                if after.active && after.frame.is_some() {
                    texture_published = true;
                } else {
                    surface_cleared = true;
                }
            }
            if before.texture_binding != after.texture_binding {
                if after.active && after.frame.is_some() && after.texture_binding.is_some() {
                    texture_published = true;
                } else if before.texture_binding.is_some() && after.texture_binding.is_none() {
                    surface_cleared = true;
                }
            }
            if after.active && after.frame.is_none() && before.background != after.background {
                surface_cleared = true;
            }
        }
        DisplayCompositionChange::from_observations(texture_published, surface_cleared)
    }
}

struct CoordinatedOwnedRequest {
    target: PresentationTarget,
    panel: PanelId,
    layer_scales: Arc<BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>>,
    surface_generation: u64,
    request: ProductRenderRequest,
    output_extent: RenderExtent,
    volume_schedule: VolumeColorSchedule,
    volume_profile: Option<VolumeWorkloadProfile>,
    render_policy: RetainedFrameRenderPolicy,
    atomic_publication_group: Option<CoordinatedPublicationGroup>,
    cross_section: Option<(u64, CrossSectionPanelScheduleState)>,
    staged_3d_refinement: bool,
}

// The logical variant deliberately keeps its fixed one/four-target storage
// inline. This value lives for one refresh turn; boxing it would add an
// allocator dependency to every coordinated logical frame solely to reduce a
// bounded 2.2 KiB stack value.
#[allow(clippy::large_enum_variant)]
enum CoordinatedRequestCohort {
    Physical(Vec<CoordinatedOwnedRequest>),
    Logical(FixedPresentationTargetRequests<CoordinatedOwnedRequest>),
}

impl CoordinatedRequestCohort {
    fn as_slice(&self) -> &[CoordinatedOwnedRequest] {
        match self {
            Self::Physical(requests) => requests,
            Self::Logical(requests) => requests.as_slice(),
        }
    }

    const fn is_logical(&self) -> bool {
        matches!(self, Self::Logical(_))
    }
}

enum BorrowedCoordinatedRequestCohort<'a> {
    Physical(Vec<CoordinatedTargetRequest<'a>>),
    Logical(FixedPresentationTargetRequests<CoordinatedTargetRequest<'a>>),
}

fn borrow_coordinated_request(
    request: &CoordinatedOwnedRequest,
    display_generation: u64,
    staged_promotion_ready: bool,
) -> CoordinatedTargetRequest<'_> {
    let borrowed = CoordinatedTargetRequest::new(
        request.target,
        &request.request.intent,
        &request.request.requirements,
        display_generation,
        request.render_policy,
    )
    .with_volume_schedule(
        request.output_extent,
        request.volume_schedule,
        request.panel == PanelId::ThreeD,
    )
    .with_hidden_promotion_authorized(!request.staged_3d_refinement || staged_promotion_ready);
    request.atomic_publication_group.map_or(borrowed, |group| {
        borrowed.with_atomic_publication_group(group)
    })
}

struct PreparedCoordinatedStagedPromotion {
    renderer_update: crate::camera_demand_cache::PreparedRendererRequirementUpdate,
}

impl DisplayRefreshWorkTiming {
    const fn new(render: DisplayRenderTiming, visible_brick_request_ms: f64) -> Self {
        Self {
            render,
            visible_brick_request_ms,
        }
    }
}

/// Allocation-free, generation-bound clocks for the milestones that matter
/// to interactive latency. Values are monotonic elapsed milliseconds from the
/// input generation that invalidated the prior display.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DisplayPerformanceMilestones {
    generation: u64,
    started_at: Instant,
    first_current_presented_ms: Option<f64>,
    first_useful_frame_ms: Option<f64>,
    complete_coarse_ms: Option<f64>,
    complete_replacement_ms: Option<f64>,
    target_settled_ms: Option<f64>,
    three_d_first_useful_frame_ms: Option<f64>,
    three_d_complete_coarse_ms: Option<f64>,
    three_d_complete_replacement_ms: Option<f64>,
    three_d_target_settled_ms: Option<f64>,
    panels: [PanelPerformanceMilestones; 4],
}

impl Default for DisplayPerformanceMilestones {
    fn default() -> Self {
        Self {
            generation: 0,
            started_at: Instant::now(),
            first_current_presented_ms: None,
            first_useful_frame_ms: None,
            complete_coarse_ms: None,
            complete_replacement_ms: None,
            target_settled_ms: None,
            three_d_first_useful_frame_ms: None,
            three_d_complete_coarse_ms: None,
            three_d_complete_replacement_ms: None,
            three_d_target_settled_ms: None,
            panels: [PanelPerformanceMilestones::default(); 4],
        }
    }
}

impl DisplayPerformanceMilestones {
    pub(crate) fn begin_generation(&mut self, generation: u64) {
        self.generation = generation;
        self.started_at = Instant::now();
        self.first_current_presented_ms = None;
        self.first_useful_frame_ms = None;
        self.complete_coarse_ms = None;
        self.complete_replacement_ms = None;
        self.target_settled_ms = None;
        self.three_d_first_useful_frame_ms = None;
        self.three_d_complete_coarse_ms = None;
        self.three_d_complete_replacement_ms = None;
        self.three_d_target_settled_ms = None;
        self.panels = [PanelPerformanceMilestones::default(); 4];
    }

    const fn panel_index(slot: PresentationSlot) -> usize {
        match slot {
            PresentationSlot::ThreeD => 0,
            PresentationSlot::Xy => 1,
            PresentationSlot::Xz => 2,
            PresentationSlot::Yz => 3,
        }
    }

    pub(crate) fn observe_panel(
        &mut self,
        slot: PresentationSlot,
        observation: PerformanceMilestoneObservation,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        let panel = &mut self.panels[Self::panel_index(slot)];
        if observation.first_useful {
            panel.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_replacement {
            panel.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_coarse {
            panel.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if observation.target_current {
            panel.first_current_presented_ms.get_or_insert(elapsed_ms);
            if observation.foreground_idle {
                panel.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) fn observe_visible_layer(
        &mut self,
        slot: PresentationSlot,
        layer_ordinal: u32,
        observation: PerformanceMilestoneObservation,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        let panel = &mut self.panels[Self::panel_index(slot)];
        let layer_index = panel
            .visible_layers
            .iter()
            .position(|milestones| milestones.layer_ordinal == Some(layer_ordinal))
            .or_else(|| {
                panel
                    .visible_layers
                    .iter()
                    .position(|milestones| milestones.layer_ordinal.is_none())
            });
        let Some(layer_index) = layer_index else {
            panel.visible_layer_overflow = true;
            return;
        };
        let layer = &mut panel.visible_layers[layer_index];
        layer.layer_ordinal = Some(layer_ordinal);
        if observation.first_useful {
            layer.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_replacement {
            layer.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if observation.complete_coarse {
            layer.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if observation.target_current {
            layer.first_current_presented_ms.get_or_insert(elapsed_ms);
            if observation.foreground_idle {
                layer.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    fn observe_three_d(
        &mut self,
        frame: &mirante4d_render_api::PresentedFrame,
        displayed_coarser_than_target: bool,
        target_settled: bool,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        if frame.progress().coverage().available_requirements() > 0 {
            self.three_d_first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        let coverage = frame.progress().coverage();
        let complete_coverage = frame.progress().completeness()
            != RenderFrameCompleteness::Progressive
            && coverage.available_requirements() == coverage.total_requirements();
        if complete_coverage {
            self.three_d_complete_replacement_ms
                .get_or_insert(elapsed_ms);
            if displayed_coarser_than_target {
                self.three_d_complete_coarse_ms.get_or_insert(elapsed_ms);
            }
            if target_settled {
                self.three_d_target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) fn observe_coordinated_layout(
        &mut self,
        first_useful: bool,
        complete_replacement: bool,
        complete_coarse: bool,
        target_current: bool,
        foreground_idle: bool,
    ) {
        let elapsed_ms = duration_ms(self.started_at.elapsed());
        if first_useful {
            self.first_useful_frame_ms.get_or_insert(elapsed_ms);
        }
        if complete_replacement {
            self.complete_replacement_ms.get_or_insert(elapsed_ms);
        }
        if complete_coarse {
            self.complete_coarse_ms.get_or_insert(elapsed_ms);
        }
        if target_current {
            self.first_current_presented_ms.get_or_insert(elapsed_ms);
            if foreground_idle {
                self.target_settled_ms.get_or_insert(elapsed_ms);
            }
        }
    }

    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    pub(crate) const fn first_current_presented_ms(self) -> Option<f64> {
        self.first_current_presented_ms
    }

    pub(crate) const fn first_useful_frame_ms(self) -> Option<f64> {
        self.first_useful_frame_ms
    }

    pub(crate) const fn complete_replacement_ms(self) -> Option<f64> {
        self.complete_replacement_ms
    }

    pub(crate) const fn complete_coarse_ms(self) -> Option<f64> {
        self.complete_coarse_ms
    }

    pub(crate) const fn target_settled_ms(self) -> Option<f64> {
        self.target_settled_ms
    }

    pub(crate) const fn three_d_first_useful_frame_ms(self) -> Option<f64> {
        self.three_d_first_useful_frame_ms
    }

    pub(crate) const fn three_d_complete_coarse_ms(self) -> Option<f64> {
        self.three_d_complete_coarse_ms
    }

    pub(crate) const fn three_d_complete_replacement_ms(self) -> Option<f64> {
        self.three_d_complete_replacement_ms
    }

    pub(crate) const fn three_d_target_settled_ms(self) -> Option<f64> {
        self.three_d_target_settled_ms
    }

    pub(crate) const fn panel(&self, slot: PresentationSlot) -> &PanelPerformanceMilestones {
        &self.panels[Self::panel_index(slot)]
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

impl MiranteWorkbenchApp {
    pub(crate) fn enter_product_renderer_terminal(&mut self, error: WgpuRenderRuntimeError) {
        let failure = render_state::render_failure_status(&anyhow::Error::new(error));
        self.render_attempt.renderer_terminal(failure);
        self.renderer_ui_wake.enter_renderer_terminal();
        self.shader_work_envelopes.invalidate_all();
        self.viewer_pick_queue.terminate_all(
            viewer_pick_runtime::PickTerminalResult::RendererTerminal(error),
        );
        let pending_timings = self
            .native_presentation
            .product_gpu
            .as_mut()
            .map(|product| {
                product.renderer.retire_terminal_work();
                product.pending_validation_captures.clear();
                product.completed_validation_captures = std::array::from_fn(|_| None);
                product.pending_gpu_timings.drain(..).collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for ticket in pending_timings {
            self.volume_presentation.discard_timing(ticket);
        }
    }

    pub(crate) fn application_snapshot_for_ui(&self) -> ApplicationSnapshot {
        let snapshot = self.application.snapshot();
        let demand_currentness = self.visible_demand_plan_currentness();
        let three_d = Some(self.presentation_surface(
            PanelId::ThreeD,
            self.render_coordination.presentation_viewport,
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current
                && demand_currentness.current_3d,
        ));
        let (xy, xz, yz) =
            if application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel {
                (
                    self.cross_section_presentation_surface(
                        PanelId::Xy,
                        demand_currentness.cross_section(PanelId::Xy),
                    ),
                    self.cross_section_presentation_surface(
                        PanelId::Xz,
                        demand_currentness.cross_section(PanelId::Xz),
                    ),
                    self.cross_section_presentation_surface(
                        PanelId::Yz,
                        demand_currentness.cross_section(PanelId::Yz),
                    ),
                )
            } else {
                (None, None, None)
            };
        snapshot
            .with_presentations(PresentationSnapshot::new(three_d, xy, xz, yz))
            .with_import_workflow(self.import.snapshot())
    }

    fn cross_section_presentation_surface(
        &self,
        panel_id: PanelId,
        demand_current: bool,
    ) -> Option<PresentationSurface> {
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        Some(self.presentation_surface(
            panel_id,
            panel.presentation_viewport()?,
            panel.display_current() && demand_current,
        ))
    }

    fn presentation_surface(
        &self,
        panel_id: PanelId,
        viewport: PresentationViewport,
        frame_is_current: bool,
    ) -> PresentationSurface {
        let frame = self
            .render_coordination
            .surface(panel_id.presentation_slot())
            .presented_frame()
            .cloned();
        PresentationSurface::with_frame_currentness(viewport, frame, frame_is_current)
    }

    pub(crate) fn clear_3d_product_presentation(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.clear_validation_capture(PresentationTarget::ThreeD);
        }
        self.render_coordination
            .clear_presented_frame(PresentationSlot::ThreeD);
        self.render_attempt.settle();
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
        self.render_coordination.frame_fidelity.three_d_preview = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = false;
        self.render_coordination
            .frame_fidelity
            .three_d_render_viewport = self.render_coordination.render_viewport;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
        self.render_attempt
            .retire_composition(PresentationTarget::ThreeD);

        // A cleared 3D surface owns no pickable frame. Retain submitted GPU
        // work only long enough to drain it, while immediately removing old
        // frame authority from queued/completed requests and visible hover.
        let pick_currentness = viewer_pick_runtime::ViewerPickCurrentness::from_snapshot(
            &self.application_snapshot_for_ui(),
        );
        self.viewer_pick_queue.retain_current(pick_currentness);
        self.egui_ui.hovered_pixel = None;
        self.egui_ui.hovered_source_readout = None;
        self.egui_ui.viewer_tools.clear_hover();
    }

    fn record_current_empty_3d_presentation(&mut self) {
        self.clear_3d_product_presentation();
        let generation = self
            .render_coordination
            .surface(PresentationSlot::ThreeD)
            .generation();
        assert!(
            self.render_coordination
                .record_empty_3d_presentation(generation),
            "a synchronous empty publication must match the current 3D surface generation"
        );
        self.render_coordination.frame_fidelity.backend = RenderBackend::Empty;
        self.render_coordination.frame_fidelity.completeness = FrameCompleteness::Complete;
        self.render_coordination.frame_fidelity.reason = LodDecisionReason::NoVisibleData;
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Current;
        self.render_coordination.frame_fidelity.ideal_scale_level = None;
        self.render_coordination.frame_fidelity.target_scale_level = None;
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = None;
        self.render_coordination
            .frame_fidelity
            .adaptive_capacity_limited = false;
        self.render_coordination.frame_fidelity.refinement_pending = false;
        self.render_coordination.frame_fidelity.frame_time_ms = None;
        self.render_coordination.frame_fidelity.visible_bricks = 0;
        self.render_coordination.frame_fidelity.resident_bricks = 0;
        self.render_coordination
            .frame_fidelity
            .missing_occupied_bricks = 0;
        self.render_coordination.frame_fidelity.three_d_preview = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = false;
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = false;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
        self.render_coordination.frame_fidelity.last_failure_kind = None;
        self.render_coordination.frame_fidelity.last_capacity_error = None;
    }

    pub(crate) fn clear_cross_section_product_presentations(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            for target in [
                PresentationTarget::Xy,
                PresentationTarget::Xz,
                PresentationTarget::Yz,
            ] {
                product.clear_validation_capture(target);
            }
        }
        for slot in [
            PresentationSlot::Xy,
            PresentationSlot::Xz,
            PresentationSlot::Yz,
        ] {
            self.render_coordination.clear_presented_frame(slot);
        }
        self.render_attempt.settle();
    }

    fn clear_cross_section_product_presentation(&mut self, panel_id: PanelId) {
        let target = panel_id.presentation_slot();
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.clear_validation_capture(target);
        }
        self.render_coordination.clear_presented_frame(target);
        self.render_attempt.settle();
    }

    fn record_current_empty_cross_section_presentation(&mut self, panel_id: PanelId) {
        debug_assert!(panel_id.cross_section_panel().is_some());
        let target = panel_id.presentation_slot();
        self.clear_cross_section_product_presentation(panel_id);
        self.render_attempt.retire_composition(target);
        let generation = self.render_coordination.surface(target).generation();
        let schedule = CrossSectionPanelScheduleState {
            generation,
            target_scale_level: None,
            render_scale_level: None,
            fallback_scale_level: None,
            selected_bricks: 0,
            occupied_selected_bricks: 0,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::Empty,
            reason: CrossSectionPanelScheduleReason::NoSelectedData,
        };
        assert!(
            self.render_coordination
                .record_empty_cross_section_presentation(target, generation, schedule),
            "a synchronous empty linked publication must match its current surface generation"
        );
    }

    pub(crate) fn invalidate_cross_section_panel_display_frames(&mut self) {
        self.render_coordination.invalidate_cross_sections();
    }

    /// Hard source-generation boundary. Unlike ordinary presentation
    /// deactivation, this also retires shared GPU residency and asynchronous
    /// source-scoped tickets before the replacement runtime is installed.
    pub(crate) fn retire_product_dataset_generation(&mut self) {
        if let Some(product) = self.native_presentation.product_gpu.as_mut() {
            product.retire_dataset_generation();
        }
        self.volume_presentation.retire_dataset();
        self.viewer_pick_queue.retire_source_generation();
        for slot in PresentationSlot::ALL {
            self.render_coordination.clear_presented_frame(slot);
        }
        self.render_attempt.settle();
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Unknown;
    }

    fn cross_section_panel_needs_display_render(
        &self,
        panel_id: PanelId,
        requirements: &PreparedRenderRequirements,
    ) -> bool {
        if panel_id.cross_section_panel().is_none() {
            return false;
        }
        let panel = self
            .render_coordination
            .surface(panel_id.presentation_slot());
        let renderer_execution_required =
            self.native_presentation
                .product_gpu
                .as_ref()
                .map(|product| {
                    let Some(extent) = panel.render_viewport() else {
                        return true;
                    };
                    let frame = self.render_intent_mailbox.snapshot().linked_2d_revision;
                    match product
                        .renderer
                        .coordinated_target_requires_prepared_presentation(
                            panel_id.presentation_slot(),
                            frame,
                            extent,
                            requirements,
                        ) {
                        Ok(required) => required,
                        Err(WgpuRenderRuntimeError::CoordinatedTargetNotConfigured { .. })
                            if panel.display_current()
                                && panel.cross_section_schedule().is_some_and(|schedule| {
                                    schedule.status == CrossSectionPanelScheduleStatus::Empty
                                }) =>
                        {
                            false
                        }
                        Err(_) => true,
                    }
                });
        panel.render_failure().is_none()
            && (!panel.display_current() || renderer_execution_required == Some(true))
    }

    fn volume_profile_for_render_plan(
        &self,
        snapshot: &ApplicationSnapshot,
        source: VolumeRenderPlanSource,
        camera: mirante4d_domain::CameraView,
        extent: RenderExtent,
    ) -> anyhow::Result<VolumeWorkloadProfile> {
        let prepared = match source {
            VolumeRenderPlanSource::Scope(scope) => self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| {
                    anyhow::anyhow!("scope {scope} has no prepared 3D render requirements")
                })?,
            VolumeRenderPlanSource::Navigation(index) => {
                self.navigation_render_plans.get(index).ok_or_else(|| {
                    anyhow::anyhow!("3D preview has no prepared navigation candidate {index}")
                })?
            }
        };
        VolumeWorkloadProfile::from_product_view(
            snapshot.catalog(),
            application_view(snapshot),
            camera,
            self.render_coordination.presentation_viewport,
            prepared.layer_scales.as_ref(),
            extent,
            snapshot.transient().playback_active(),
        )
    }

    fn select_volume_preview(
        &mut self,
        target_scope: u64,
        target_profile: &VolumeWorkloadProfile,
        target_available: bool,
        snapshot: &ApplicationSnapshot,
        gesture: Option<mirante4d_application::RenderGestureId>,
    ) -> anyhow::Result<Option<(VolumeRenderPlanSource, VolumeWorkloadProfile)>> {
        let mut candidates =
            Vec::with_capacity(self.navigation_render_plans.len().saturating_add(1));
        for (index, plan) in self.navigation_render_plans.iter().enumerate() {
            if plan.requirements.timepoint() != application_view(snapshot).timepoint() {
                continue;
            }
            let profile = VolumeWorkloadProfile::from_product_view(
                snapshot.catalog(),
                application_view(snapshot),
                target_profile.camera(),
                self.render_coordination.presentation_viewport,
                plan.layer_scales.as_ref(),
                target_profile.extent(),
                snapshot.transient().playback_active(),
            )?;
            let available = first_useful_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                plan,
            );
            // Only the guaranteed terminal rung may break the cold-start
            // residency cycle. It still needs every first-useful payload
            // retained on the CPU (or already resident), and the renderer
            // cannot present it until those resources finish uploading.
            let cold_bootstrap = index == 0
                && first_useful_resources_uploadable_with_renderer(
                    &self.dataset,
                    &self.native_presentation,
                    plan,
                );
            candidates.push((
                VolumeRenderPlanSource::Navigation(index),
                VolumePreviewCandidate::navigation(
                    profile,
                    available,
                    cold_bootstrap,
                    plan.primary_resource_count,
                    plan.render_payload_bytes,
                ),
            ));
        }
        let target_duplicates_navigation = candidates.iter().any(|(_, candidate)| {
            candidate.profile() == target_profile && (!target_available || candidate.available())
        });
        if !target_duplicates_navigation {
            let target_plan = self
                .prepared_scope_render_plans
                .get(&target_scope)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "scope {target_scope} has no prepared exact 3D render requirements"
                    )
                })?;
            candidates.push((
                VolumeRenderPlanSource::Scope(target_scope),
                VolumePreviewCandidate::target(
                    target_profile.clone(),
                    target_available,
                    target_plan.requirements.body().canonical().len(),
                    target_plan.render_payload_bytes,
                ),
            ));
        }
        let preview_candidates = candidates
            .iter()
            .map(|(_, candidate)| candidate.clone())
            .collect::<Vec<_>>();
        let choice = self.volume_presentation.select_preview_profile(
            target_profile,
            &preview_candidates,
            gesture,
        );
        let facts = self.volume_presentation.latest_candidate_facts();
        self.render_coordination
            .frame_fidelity
            .three_d_preview_candidate_count = u32::try_from(facts.len()).unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_resident_candidate_count = u32::try_from(
            facts
                .iter()
                .filter(|candidate| candidate.complete_and_resident)
                .count(),
        )
        .unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_safe_candidate_count = u32::try_from(
            facts
                .iter()
                .filter(|candidate| {
                    candidate.complete_and_resident
                        && candidate.target_quality_eligible
                        && candidate.interaction_safe
                })
                .count(),
        )
        .unwrap_or(u32::MAX);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_is_finest_safe = facts
            .iter()
            .any(|candidate| candidate.disposition == VolumePreviewCandidateDisposition::Selected);
        self.render_coordination
            .frame_fidelity
            .three_d_preview_uses_emergency_floor = facts.iter().any(|candidate| {
            candidate.disposition == VolumePreviewCandidateDisposition::SelectedTerminalEmergency
        });
        let Some(choice) = choice else {
            // A temporal cutover can briefly have neither a current-timepoint
            // navigation body nor an uploadable target. The already-published
            // predecessor is the correct presentation in that state. Absence
            // of a successor preview is ordinary readiness backpressure, not
            // a renderer failure and not permission to expose an empty frame.
            return Ok(None);
        };
        let (source, _) = candidates
            .get(choice.candidate_index())
            .expect("a selected preview index belongs to its bounded candidate set");
        Ok(Some((*source, choice.into_profile())))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the request boundary keeps independent target, geometry, and refinement facts explicit"
    )]
    fn build_coordinated_request(
        &self,
        panel: PanelId,
        scope: u64,
        snapshot: &ApplicationSnapshot,
        presentation: PresentationViewport,
        extent: RenderExtent,
        output_extent: RenderExtent,
        volume_schedule: VolumeColorSchedule,
        volume_profile: Option<VolumeWorkloadProfile>,
        cross_section: Option<(u64, CrossSectionPanelScheduleState)>,
        staged_3d_refinement: bool,
        navigation_candidate: Option<usize>,
        transaction: Option<&PresentationTransaction>,
    ) -> anyhow::Result<Option<CoordinatedOwnedRequest>> {
        if staged_3d_refinement && navigation_candidate.is_some() {
            anyhow::bail!("a staged fine frame cannot also bind the navigation ladder");
        }
        if panel == PanelId::ThreeD {
            if volume_profile.is_none() {
                anyhow::bail!("a 3D request requires a bounded workload profile");
            }
            if volume_schedule == VolumeColorSchedule::InteractivePreview && extent != output_extent
            {
                anyhow::bail!("a 3D navigation preview must render at the physical output extent");
            }
        } else if output_extent != extent
            || volume_schedule != VolumeColorSchedule::Direct
            || volume_profile.is_some()
        {
            anyhow::bail!("a linked-plane request cannot use a 3D presentation schedule");
        }
        let base = RenderIntentBase::from_snapshot(snapshot);
        let active_intent_target = self.render_intent_mailbox.active_target(base);
        let durable_camera = *application_view(snapshot).camera();
        let transaction_camera = transaction
            .filter(|transaction| transaction.contains(PresentationTarget::ThreeD))
            .map(PresentationTransaction::camera);
        let resident_camera = (panel == PanelId::ThreeD)
            .then(|| self.render_intent_mailbox.renderable_camera(base))
            .flatten();
        let camera_override = transaction_camera.or(resident_camera).or_else(|| {
            (panel == PanelId::ThreeD
                && staged_3d_refinement
                && active_intent_target == Some(RenderIntentTarget::ThreeD))
            .then(|| {
                self.render_intent_mailbox
                    .effective_camera(base, durable_camera)
            })
        });
        let prepared = if let Some(index) = navigation_candidate {
            self.navigation_render_plans.get(index).ok_or_else(|| {
                anyhow::anyhow!("resident camera has no prepared navigation candidate {index}")
            })?
        } else {
            let prepared = self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| {
                    anyhow::anyhow!("scope {scope} has no prepared render requirements")
                })?;
            let resources = self.dataset.scope_requirement_handle(scope);
            if !Arc::ptr_eq(&prepared.scope_requirements, &resources) {
                anyhow::bail!(
                    "scope {scope} render requirements do not match its current residency union"
                );
            }
            prepared
        };
        if let Some(contract) = self.playback_session.contract()
            && prepared.layer_scales.as_ref() != contract.layer_scales().as_ref()
        {
            anyhow::bail!("a playing target attempted to escape the immutable playback scale map");
        }
        if transaction.is_some_and(PresentationTransaction::is_retained_quality)
            && application_view(snapshot)
                .layers()
                .iter()
                .filter(|layer| layer.visible())
                .any(|layer| !prepared.layer_scales.contains_key(&layer.layer_key()))
        {
            // Playback-scope retirement can briefly leave an installed scope
            // handle with no stationary scale map. That is not an empty
            // scientific target: it is an unmaterialized quality successor.
            // Refuse to construct the member so fixed-target assembly retains
            // the playback front until the ordinary stationary plan arrives.
            return Ok(None);
        }
        // A temporal transition deliberately retains the predecessor's
        // pixels while the successor body is staged. That retained body is
        // paintable evidence only: it must never be rebound to the new
        // timepoint's render intent through the camera-navigation fast path.
        // Waiting here is a normal transitional state, not a renderer fault.
        if prepared.requirements.timepoint() != application_view(snapshot).timepoint() {
            return Ok(None);
        }
        let durable_cross_section = *application_view(snapshot).cross_section();
        let linked_interaction_active = active_intent_target
            .is_some_and(|target| matches!(target, RenderIntentTarget::CrossSection(_)));
        let cross_section_view = panel.cross_section_panel().map(|_| {
            if let Some(transaction) = transaction {
                transaction.cross_section()
            } else if linked_interaction_active {
                self.render_intent_mailbox
                    .effective_cross_section(base, durable_cross_section)
            } else {
                durable_cross_section
            }
        });
        let frame = transaction.map_or_else(
            || {
                let mailbox = self.render_intent_mailbox.snapshot();
                if panel == PanelId::ThreeD {
                    mailbox.three_d_revision
                } else {
                    mailbox.linked_2d_revision
                }
            },
            |transaction| transaction.expected_revision(panel.presentation_slot()),
        );
        let Some(intent) = build_product_intent(
            snapshot,
            frame,
            panel.cross_section_panel().map(|_| panel),
            cross_section_view,
            presentation,
            extent,
            camera_override,
        )?
        else {
            return Ok(None);
        };
        if let Some(transaction) = transaction
            && (transaction.source_generation() != snapshot.source_generation()
                || transaction.timepoint() != application_view(snapshot).timepoint()
                || !transaction.contains(panel.presentation_slot())
                || intent.frame() != transaction.expected_revision(panel.presentation_slot()))
        {
            anyhow::bail!(
                "a composed presentation request escaped its semantic transaction cutoff"
            );
        }
        let requirements = prepared.requirements.bind(&intent)?;
        let shader_work_envelope = match self.shader_work_envelopes.resolve_or_submit(
            panel.presentation_slot(),
            Arc::clone(snapshot.catalog()),
            &intent,
            &requirements,
        ) {
            ShaderWorkEnvelopeLookup::Ready(envelope) => envelope,
            ShaderWorkEnvelopeLookup::Pending => return Ok(None),
            ShaderWorkEnvelopeLookup::Failed(ShaderWorkEnvelopeBuildError::Admission(error)) => {
                return Err(
                    crate::semantic_demand::SemanticPlanError::ShaderAdmission(error).into(),
                );
            }
            ShaderWorkEnvelopeLookup::Failed(ShaderWorkEnvelopeBuildError::Capacity(error)) => {
                return Err(
                    crate::semantic_demand::SemanticPlanError::ScratchCapacity(error).into(),
                );
            }
            ShaderWorkEnvelopeLookup::Failed(ShaderWorkEnvelopeBuildError::WorkerPanicked) => {
                anyhow::bail!("shader work-envelope worker panicked");
            }
        };
        let intent = intent.with_shared_shader_work_envelope(shader_work_envelope);
        let existing_presentation = self
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame()
            .map(|frame| frame.progress().completeness());
        let render_policy = if panel.cross_section_panel().is_some() {
            // Every Plane body owns a complete navigation floor as its
            // first-useful prefix. A useful frame is therefore current
            // geometry with valid fallback fidelity, never a hole-filled
            // partial frame.
            RetainedFrameRenderPolicy::EveryUsefulFrame
        } else if staged_3d_refinement
            || camera_override.is_some()
            || volume_schedule != VolumeColorSchedule::Direct
        {
            RetainedFrameRenderPolicy::ExactFrameOnly
        } else {
            retained_frame_render_policy(true, existing_presentation)
        };
        // The exact atomic group is attached only after fixed-shape logical
        // assembly. Reuse remains exclusively renderer-owned; every member
        // crosses the boundary with its complete immutable request.
        let atomic_publication_group = None;
        let render_policy = if transaction.is_some() {
            RetainedFrameRenderPolicy::ExactFrameOnly
        } else {
            render_policy
        };
        Ok(Some(CoordinatedOwnedRequest {
            target: panel.presentation_slot(),
            panel,
            layer_scales: Arc::clone(&prepared.layer_scales),
            surface_generation: self
                .render_coordination
                .surface(panel.presentation_slot())
                .generation(),
            request: ProductRenderRequest {
                intent,
                requirements,
            },
            output_extent,
            volume_schedule,
            volume_profile,
            render_policy,
            atomic_publication_group,
            cross_section,
            staged_3d_refinement,
        }))
    }

    fn coordinated_active_target(
        &self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
    ) -> Option<PresentationTarget> {
        let base = RenderIntentBase::from_snapshot(snapshot);
        let transient = self
            .render_intent_mailbox
            .active_target(base)
            .map(|target| match target {
                RenderIntentTarget::ThreeD => PresentationTarget::ThreeD,
                RenderIntentTarget::CrossSection(panel) => {
                    PanelId::from_application_panel(panel).presentation_slot()
                }
            });
        let durable = snapshot
            .transient()
            .active_cross_section_panel()
            .map(PanelId::from_application_panel)
            .map(PanelId::presentation_slot)
            .filter(|_| application_view(snapshot).layout() == CanonicalViewerLayout::FourPanel);
        transient
            .or(durable)
            .filter(|target| requests.iter().any(|request| request.target == *target))
            .or_else(|| {
                requests
                    .iter()
                    .find(|request| request.target == PresentationTarget::ThreeD)
                    .map(|request| request.target)
            })
            .or_else(|| requests.first().map(|request| request.target))
    }

    fn apply_coordinated_layout(
        &mut self,
        desired: &[CoordinatedTargetLayout],
    ) -> anyhow::Result<f64> {
        let started = Instant::now();
        let bindings = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let report = product.renderer.request_coordinated_layout(desired)?;
            report
                .targets()
                .iter()
                .map(|state| {
                    Ok::<_, WgpuRenderRuntimeError>((
                        state.target(),
                        state.device_generation(),
                        state.texture_revision(),
                        product
                            .renderer
                            .coordinated_target_texture_view(state.target())?
                            .clone(),
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let retained = bindings
            .iter()
            .map(|(target, _, _, _)| *target)
            .collect::<Vec<_>>();
        self.native_presentation.retain_texture_bindings(&retained);
        for (target, device_generation, texture_revision, view) in bindings {
            self.native_presentation.bind_texture(
                target,
                device_generation,
                texture_revision,
                &view,
            );
            self.render_attempt.note_published_texture(
                target,
                device_generation.get(),
                texture_revision.get(),
            );
        }
        Ok(duration_ms(started.elapsed()))
    }

    /// Retires every renderer/native presentation target for a semantically
    /// empty visible layout. Empty publication is independent of color
    /// pipeline readiness; a terminal device cause remains recorded on the
    /// renderer authority without relabelling the empty surface as failed.
    fn retire_empty_coordinated_layout(&mut self) -> anyhow::Result<f64> {
        if self.native_presentation.product_gpu.is_none()
            || self.render_attempt.renderer_is_terminal()
        {
            self.native_presentation.retain_texture_bindings(&[]);
            return Ok(0.0);
        }
        match self.apply_coordinated_layout(&[]) {
            Ok(elapsed_ms) => Ok(elapsed_ms),
            Err(error) => {
                let terminal = error
                    .downcast_ref::<WgpuRenderRuntimeError>()
                    .copied()
                    .filter(|error| {
                        classify_color_runtime_error(*error)
                            == ColorRuntimeDisposition::RendererTerminal
                    });
                let Some(terminal) = terminal else {
                    return Err(error);
                };
                self.enter_product_renderer_terminal(terminal);
                self.native_presentation.retain_texture_bindings(&[]);
                Ok(0.0)
            }
        }
    }

    fn record_presented_volume_schedule(&mut self, request: &CoordinatedOwnedRequest) {
        if request.panel != PanelId::ThreeD {
            return;
        }
        let preview = request.volume_schedule == VolumeColorSchedule::InteractivePreview;
        if let Some(profile) = request.volume_profile.clone() {
            if preview {
                self.volume_presentation.note_presented_preview(
                    request.request.intent.frame(),
                    profile,
                    request.output_extent,
                );
            } else {
                self.volume_presentation
                    .note_presented_exact(request.request.intent.frame());
            }
        }
        self.render_coordination
            .frame_fidelity
            .three_d_render_viewport = request.request.intent.extent();
        self.render_coordination.frame_fidelity.three_d_preview = preview;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_completed = 0;
        self.render_coordination
            .frame_fidelity
            .three_d_refinement_strips_total = 0;
        if preview {
            self.render_coordination.frame_fidelity.reason = LodDecisionReason::FrameBudgetLimited;
            self.render_coordination.frame_fidelity.refinement_pending = true;
            self.render_coordination.request_refresh();
        }
    }

    /// True once every required staged-refinement payload is either retained
    /// by the app or already resident in the renderer. Before this point the
    /// refinement scope can still own live requests needed by the private
    /// candidate, so promotion reconciliation must not cancel them.
    fn staged_3d_promotion_payloads_ready(&self) -> bool {
        let Some(product) = self.native_presentation.product_gpu.as_ref() else {
            return false;
        };
        self.dataset
            .scope_resources_complete_with_gpu_residency(SCOPE_CURRENT_3D_REFINEMENT, |key| {
                product.renderer.resource_is_resident(key)
            })
    }

    /// Atomically rebinds the already-rendered exact transient planes into
    /// the new durable linked generation without manufacturing replacement
    /// pixels. All three panels displayed the same transient geometry and
    /// therefore cross the durable boundary as one retained presentation.
    pub(crate) fn adopt_completed_exact_cross_sections(
        &mut self,
        completed_frame: FrameIdentity,
        completed_view: CrossSectionView,
    ) -> anyhow::Result<bool> {
        let snapshot = self.application.snapshot();
        if application_view(&snapshot).layout() != CanonicalViewerLayout::FourPanel
            || *application_view(&snapshot).cross_section() != completed_view
            || ![PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .all(|panel| self.visible_demand_plan_currentness().cross_section(panel))
        {
            return Ok(false);
        }

        let view = application_view(&snapshot);
        let mut adoptions = Vec::with_capacity(3);
        for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
            let target = panel.presentation_slot();
            let scope = cross_section_scope(panel)?;
            let surface = self.render_coordination.surface(target);
            let Some(presentation) = surface.presentation_viewport() else {
                return Ok(false);
            };
            let Some(extent) = surface.render_viewport() else {
                return Ok(false);
            };
            let projected_layer_scales = cross_section_projected_layer_scales(
                snapshot.catalog(),
                view,
                panel,
                presentation,
                extent,
            )?;
            if self.dataset.scope_layer_scales(scope) != Some(&projected_layer_scales) {
                // A transient interaction LOD is truthful while the gesture
                // is active, but it is not an exact durable result after an
                // orientation-driven LOD threshold. The ordinary settled
                // plan must promote the screen-selected target instead.
                return Ok(false);
            }
            let Some(prepared) = self.prepared_scope_render_plans.get(&scope) else {
                return Ok(false);
            };
            let canonical_resources = self.dataset.scope_requirement_handle(scope);
            if !Arc::ptr_eq(
                prepared.requirements.body().canonical(),
                &canonical_resources,
            ) || self.render_attempt.target_ready(target)
                || self.native_presentation.texture_id(target).is_none()
            {
                return Ok(false);
            }
            let Some(frame) = self
                .render_coordination
                .surface(target)
                .presented_frame()
                .filter(|frame| {
                    frame.target() == target
                        && frame.frame() == completed_frame
                        && frame.progress().completeness() == RenderFrameCompleteness::Exact
                        && self.render_coordination.surface(target).render_viewport()
                            == Some(frame.extent())
                })
                .cloned()
            else {
                return Ok(false);
            };

            let uniform_target = self
                .dataset
                .scope_layer_scales(scope)
                .and_then(crate::dataset_demand_plan::uniform_layer_scale);
            let priority = self.dataset.scope_gpu_priority_handle(scope);
            let required = self.dataset.scope_required_prefix_len(scope);
            let requirements = &priority[..required];
            let all_requirements_available = scope_resources_complete_with_renderer(
                &self.dataset,
                &self.native_presentation,
                scope,
            );
            let prepared = self
                .prepared_scope_render_plans
                .get(&scope)
                .ok_or_else(|| anyhow::anyhow!("linked scope has no prepared render plan"))?;
            let schedule = schedule_cross_section_panel(
                &mut self.render_coordination,
                CrossSectionScheduleInput {
                    view,
                    uniform_target,
                    requirements,
                    first_useful_requirements: prepared.requirements.first_useful_prefix_len(),
                    first_useful_available: first_useful_resources_complete_with_renderer(
                        &self.dataset,
                        &self.native_presentation,
                        prepared,
                    ),
                    retained_leases: self.dataset.retained_leases(),
                    all_requirements_available,
                    dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
                    #[cfg(test)]
                    requirement_visit_counter: None,
                },
                panel,
                true,
            )?
            .schedule;
            if !schedule.is_renderable() {
                return Ok(false);
            }
            let coverage = frame.progress().coverage();
            let schedule = cross_section_schedule_for_presented_coverage(
                schedule,
                frame.progress().completeness(),
                coverage.available_requirements(),
                coverage.total_requirements(),
            );
            adoptions.push((
                panel,
                target,
                scope,
                self.render_coordination.surface(target).generation(),
                frame,
                schedule,
            ));
        }
        if !adoptions.iter().all(|(_, target, _, generation, _, _)| {
            self.render_coordination.surface(*target).generation() == *generation
        }) {
            return Ok(false);
        }
        for (panel, target, scope, generation, frame, schedule) in adoptions {
            assert!(
                self.render_coordination
                    .record_cross_section_presentation(target, generation, schedule),
                "a preflighted linked adoption must retain its surface generation"
            );
            self.record_coordinated_product_frame(panel, generation, &frame, None, true);
            self.record_coordinated_layer_presentations(panel, scope);
        }
        Ok(true)
    }

    fn desired_coordinated_layout(
        &self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
    ) -> Vec<CoordinatedTargetLayout> {
        PresentationTarget::ALL
            .into_iter()
            .filter(|target| {
                *target == PresentationTarget::ThreeD
                    || application_view(snapshot).layout() == CanonicalViewerLayout::FourPanel
            })
            .filter_map(|target| {
                let surface = self.render_coordination.surface(target);
                let requested = requests.iter().find(|request| request.target == target);
                (requested.is_some() || surface.presented_frame().is_some())
                    .then(|| {
                        requested.map_or_else(
                            || {
                                (target == PresentationTarget::ThreeD)
                                    .then_some(
                                        self.render_coordination
                                            .frame_fidelity
                                            .three_d_render_viewport,
                                    )
                                    .or_else(|| surface.render_viewport())
                            },
                            |request| Some(request.request.intent.extent()),
                        )
                    })
                    .flatten()
                    .map(|extent| CoordinatedTargetLayout::new(target, extent))
            })
            .collect()
    }

    pub(crate) fn rerender_coordinated_display_state(
        &mut self,
    ) -> anyhow::Result<DisplayRefreshWorkTiming> {
        let snapshot = self.application.snapshot();
        let four_panel = application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel;
        let canonical_view_is_empty = application_view(&snapshot)
            .layers()
            .iter()
            .all(|layer| !layer.visible());
        if canonical_view_is_empty {
            // The canonical view is the display cutoff authority. Do not wait
            // for a worker-prepared empty demand body before removing pixels:
            // the previous nonempty renderer front is precisely what makes
            // an empty aggregate-union preflight invalid. Publish the empty
            // surface first, retire that front and its frame lease, and only
            // then consume or submit the latest-only empty demand plan.
            self.record_current_empty_3d_presentation();
            if four_panel {
                for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                    self.record_current_empty_cross_section_presentation(panel);
                }
            }
            let egui_texture_ms = self.retire_empty_coordinated_layout()?;

            let demand_started = Instant::now();
            self.request_visible_bricks();
            let visible_brick_request_ms = duration_ms(demand_started.elapsed());
            // Planning is allowed to report its own pending state, but it
            // cannot relabel the already-authoritative canonical empty
            // surface as Loading while the zero-body transaction catches up.
            self.record_current_empty_3d_presentation();
            if four_panel {
                for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                    self.record_current_empty_cross_section_presentation(panel);
                }
            }
            let demand_currentness = self.visible_demand_plan_currentness();
            let semantic_empty_is_current = demand_currentness.current_3d
                && self.dataset.scope_is_empty(SCOPE_CURRENT_3D)
                && (!four_panel
                    || [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                        .into_iter()
                        .all(|panel| {
                            let scope = cross_section_scope(panel)
                                .expect("a fixed linked panel owns one demand scope");
                            demand_currentness.cross_section(panel)
                                && self.dataset.scope_is_empty(scope)
                        }));
            if semantic_empty_is_current {
                // A temporal or retained-quality cutoff whose complete target
                // set is empty is Exact by semantics. Complete it without
                // manufacturing renderer members.
                if let Some(transaction) = self.presentation_scheduler.transaction(
                    &snapshot,
                    &self.playback_session,
                    &self.render_intent_mailbox,
                ) {
                    self.complete_presentation_transaction(&snapshot, &transaction);
                }
            }
            self.observe_coordinated_display_milestones(false);
            self.record_current_layout_presentation_if_complete();
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        }

        let demand_started = Instant::now();
        self.request_visible_bricks();
        let visible_brick_request_ms = duration_ms(demand_started.elapsed());
        let demand_currentness = self.visible_demand_plan_currentness();
        let demand_renderability = self.visible_demand_renderability();
        let current_3d_is_empty =
            demand_currentness.current_3d && self.dataset.scope_is_empty(SCOPE_CURRENT_3D);
        if current_3d_is_empty {
            // Empty is a terminal semantic result, not GPU work. Publish it
            // even while the renderer is unavailable or still compiling.
            self.record_current_empty_3d_presentation();
        }
        let current_linked_layout_is_empty = !four_panel
            || [PanelId::Xy, PanelId::Xz, PanelId::Yz]
                .into_iter()
                .all(|panel| {
                    let scope = cross_section_scope(panel)
                        .expect("a fixed linked panel owns one demand scope");
                    demand_currentness.cross_section(panel) && self.dataset.scope_is_empty(scope)
                });
        if current_3d_is_empty && current_linked_layout_is_empty {
            if four_panel {
                for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                    self.record_current_empty_cross_section_presentation(panel);
                }
            }

            // A temporal or retained-quality cutoff whose complete target set
            // is empty is already Exact by semantics. Complete that logical
            // transaction without manufacturing empty renderer members.
            if let Some(transaction) = self.presentation_scheduler.transaction(
                &snapshot,
                &self.playback_session,
                &self.render_intent_mailbox,
            ) {
                self.complete_presentation_transaction(&snapshot, &transaction);
            }
            let egui_texture_ms = self.retire_empty_coordinated_layout()?;
            self.observe_coordinated_display_milestones(false);
            self.record_current_layout_presentation_if_complete();
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        }
        if current_3d_is_empty {
            self.record_current_layout_presentation_if_complete();
        }
        if !self
            .native_presentation
            .initial_render_pipeline_is_ready()?
        {
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms: 0.0,
                },
                visible_brick_request_ms,
            ));
        }

        let presentation_transaction = self.presentation_scheduler.transaction(
            &snapshot,
            &self.playback_session,
            &self.render_intent_mailbox,
        );
        let playback_contract_active = self.playback_session.contract().is_some();
        let retained_quality_handoff = presentation_transaction
            .as_ref()
            .is_some_and(PresentationTransaction::is_retained_quality);
        if retained_quality_handoff {
            let full_layout =
                application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel;
            let stationary_plan_current = demand_currentness.current_3d
                && (!full_layout || demand_currentness.cross_sections);
            let stationary_plan_renderable = demand_renderability.current_3d
                && (!full_layout || demand_renderability.cross_sections);
            if !stationary_plan_current || !stationary_plan_renderable {
                // Stop retires the playback planning signature before the
                // ordinary stationary signature is installed. During that
                // bounded handoff, prepared scope maps can still describe the
                // old fixed playback scale. They are neither changed work nor
                // reusable proof for the desired stationary transaction.
                // Keep the coherent playback front authoritative until the
                // complete current stationary plan exists for the layout.
                self.observe_coordinated_display_milestones(false);
                self.record_current_layout_presentation_if_complete();
                return Ok(DisplayRefreshWorkTiming::new(
                    DisplayRenderTiming {
                        path: DisplayRefreshPath::UiBackground,
                        render_ms: 0.0,
                        gpu_upload_ms: None,
                        gpu_compute_ms: None,
                        egui_texture_ms: 0.0,
                    },
                    visible_brick_request_ms,
                ));
            }
        }
        let mut requests = Vec::with_capacity(4);
        let base = RenderIntentBase::from_snapshot(&snapshot);
        let resident_camera = self.render_intent_mailbox.renderable_camera(base);
        let effective_timepoint = application_view(&snapshot).timepoint();
        let current_scope_matches_timepoint = self
            .prepared_scope_render_plans
            .get(&SCOPE_CURRENT_3D)
            .is_some_and(|plan| plan.requirements.timepoint() == effective_timepoint);
        let navigation_ladder_matches_timepoint = self
            .navigation_render_plans
            .first()
            .is_some_and(|plan| plan.requirements.timepoint() == effective_timepoint);
        let resident_target_body = resident_camera
            .is_some_and(|camera| self.resident_camera_target_body_is_complete(camera));
        let resident_navigation_intent = resident_camera.is_some()
            && (resident_target_body
                || (!playback_contract_active
                    && current_scope_matches_timepoint
                    && navigation_ladder_matches_timepoint
                    && self.dataset.scope_is_installed(SCOPE_CURRENT_3D)
                    && self.navigation_render_plans.first().is_some_and(|plan| {
                        first_useful_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            plan,
                        )
                    })));
        let transaction_requires_three_d = presentation_transaction
            .as_ref()
            .is_some_and(|transaction| transaction.contains(PresentationTarget::ThreeD));
        if (transaction_requires_three_d
            || demand_currentness.current_3d
            || resident_navigation_intent)
            && !current_3d_is_empty
        {
            let retained_complete = self
                .render_coordination
                .surface(PresentationTarget::ThreeD)
                .presented_frame()
                .is_some_and(|frame| {
                    frame.progress().completeness() != RenderFrameCompleteness::Progressive
                });
            let transient_camera_staging = self.dataset.staging_current_refinement()
                && self.render_intent_mailbox.active_target(base)
                    == Some(RenderIntentTarget::ThreeD);
            // A progressive transaction has two complete-frame candidates:
            // the navigation scope for the latest intent, then the finer
            // hidden successor. Whenever input has made the display stale,
            // submit the navigation scope first. Once that exact frame is
            // current, the next refresh may work on the refinement without
            // making interaction wait behind it.
            let navigation_frame_pending = !playback_contract_active
                && !resident_target_body
                && self.dataset.staging_current_refinement()
                && current_scope_matches_timepoint
                && navigation_ladder_matches_timepoint
                && self.dataset.scope_is_installed(SCOPE_CURRENT_3D)
                && self.render_coordination.frame_fidelity.display_freshness
                    != DisplayedFrameFreshness::Current;
            let staged_3d_refinement = !navigation_frame_pending
                && self.dataset.staging_current_refinement()
                && (transient_camera_staging
                    || self.dataset.holding_previous_presentation()
                    || retained_complete);
            let scope = if staged_3d_refinement {
                SCOPE_CURRENT_3D_REFINEMENT
            } else {
                SCOPE_CURRENT_3D
            };
            let output_extent = self.render_coordination.render_viewport;
            let mailbox = self.render_intent_mailbox.snapshot();
            let frame = presentation_transaction
                .as_ref()
                .map_or(mailbox.three_d_revision, |transaction| {
                    transaction.expected_revision(PresentationTarget::ThreeD)
                });
            let preview_gesture = matches!(mailbox.active_target, Some(RenderIntentTarget::ThreeD))
                .then_some(mailbox.active_gesture)
                .flatten();
            let durable_camera = *application_view(&snapshot).camera();
            let workload_camera = presentation_transaction
                .as_ref()
                .filter(|transaction| transaction.contains(PresentationTarget::ThreeD))
                .map(PresentationTransaction::camera)
                .or(resident_camera)
                .unwrap_or_else(|| {
                    if self.render_intent_mailbox.active_target(base)
                        == Some(RenderIntentTarget::ThreeD)
                    {
                        self.render_intent_mailbox
                            .effective_camera(base, durable_camera)
                    } else {
                        durable_camera
                    }
                });
            let seek_handoff = self
                .render_coordination
                .surface(PresentationTarget::ThreeD)
                .presented_frame()
                .is_some_and(|frame| frame.timepoint() != effective_timepoint);
            let target_uses_navigation_ladder = !playback_contract_active
                && !retained_quality_handoff
                && !resident_target_body
                && (navigation_frame_pending
                    || (resident_camera.is_some() && !staged_3d_refinement));

            let (
                request_source,
                request_extent,
                request_schedule,
                request_profile,
                request_staged_refinement,
                preview_replacement_needed,
            ) = if target_uses_navigation_ladder {
                let target_profile_source = if self
                    .prepared_scope_render_plans
                    .contains_key(&SCOPE_CURRENT_3D_REFINEMENT)
                {
                    VolumeRenderPlanSource::Scope(SCOPE_CURRENT_3D_REFINEMENT)
                } else {
                    VolumeRenderPlanSource::Scope(SCOPE_CURRENT_3D)
                };
                let target_profile = self.volume_profile_for_render_plan(
                    &snapshot,
                    target_profile_source,
                    workload_camera,
                    output_extent,
                )?;
                let Some((preview_source, preview_profile)) = self.select_volume_preview(
                    match target_profile_source {
                        VolumeRenderPlanSource::Scope(scope) => scope,
                        VolumeRenderPlanSource::Navigation(_) => SCOPE_CURRENT_3D,
                    },
                    &target_profile,
                    false,
                    &snapshot,
                    preview_gesture,
                )?
                else {
                    return Ok(DisplayRefreshWorkTiming::new(
                        DisplayRenderTiming {
                            path: DisplayRefreshPath::UiBackground,
                            render_ms: 0.0,
                            gpu_upload_ms: None,
                            gpu_compute_ms: None,
                            egui_texture_ms: 0.0,
                        },
                        visible_brick_request_ms,
                    ));
                };
                let replace = self.volume_presentation.preview_needs_replacement(
                    frame,
                    &preview_profile,
                    output_extent,
                );
                (
                    preview_source,
                    output_extent,
                    VolumeColorSchedule::InteractivePreview,
                    preview_profile,
                    false,
                    replace,
                )
            } else {
                let target_profile = self.volume_profile_for_render_plan(
                    &snapshot,
                    VolumeRenderPlanSource::Scope(scope),
                    workload_camera,
                    output_extent,
                )?;
                if playback_contract_active {
                    // The session contract selected this complete full-volume
                    // body before Playing began. Camera work composes against
                    // that same body directly; the ordinary navigation
                    // preview selector is forbidden because it can choose a
                    // different ladder rung and create a coarse flash.
                    (
                        VolumeRenderPlanSource::Scope(scope),
                        output_extent,
                        VolumeColorSchedule::Direct,
                        target_profile,
                        staged_3d_refinement,
                        false,
                    )
                } else if retained_quality_handoff {
                    // Stopping playback changes only the desired quality.
                    // Keep the already-visible playback front authoritative
                    // while the stationary exact candidate is recorded into
                    // private refinement storage; never route this transition
                    // through a coarser navigation rung.
                    let strip_height = self
                        .volume_presentation
                        .refinement_strip_height(frame, &target_profile);
                    (
                        VolumeRenderPlanSource::Scope(scope),
                        output_extent,
                        VolumeColorSchedule::AtomicRefinement {
                            strip_height_pixels: strip_height,
                        },
                        target_profile,
                        staged_3d_refinement,
                        false,
                    )
                } else if seek_handoff && preview_gesture.is_none() {
                    // A recorded-timepoint change keeps the predecessor
                    // visible while the complete successor is rendered into
                    // hidden atomic-refinement storage. Showing a spatial
                    // preview of the successor first would add an avoidable
                    // visual transition while the camera is settled.
                    //
                    // During an active camera gesture the opposite rule is
                    // required: every new camera sample invalidates a private
                    // exact image, so insisting on atomic refinement would
                    // starve temporal publication until input stopped. The
                    // ordinary complete navigation preview below is instead
                    // published with the linked targets for that timepoint;
                    // spatial refinement remains independent and resumes
                    // after the gesture settles.
                    let strip_height = self
                        .volume_presentation
                        .refinement_strip_height(frame, &target_profile);
                    (
                        VolumeRenderPlanSource::Scope(scope),
                        output_extent,
                        VolumeColorSchedule::AtomicRefinement {
                            strip_height_pixels: strip_height,
                        },
                        target_profile,
                        staged_3d_refinement,
                        false,
                    )
                } else if preview_gesture.is_none()
                    && self.volume_presentation.direct_is_safe(&target_profile)
                {
                    (
                        VolumeRenderPlanSource::Scope(scope),
                        output_extent,
                        VolumeColorSchedule::Direct,
                        target_profile,
                        staged_3d_refinement,
                        false,
                    )
                } else {
                    let Some((preview_source, preview_profile)) = self.select_volume_preview(
                        scope,
                        &target_profile,
                        true,
                        &snapshot,
                        preview_gesture,
                    )?
                    else {
                        return Ok(DisplayRefreshWorkTiming::new(
                            DisplayRenderTiming {
                                path: DisplayRefreshPath::UiBackground,
                                render_ms: 0.0,
                                gpu_upload_ms: None,
                                gpu_compute_ms: None,
                                egui_texture_ms: 0.0,
                            },
                            visible_brick_request_ms,
                        ));
                    };
                    let replace = self.volume_presentation.preview_needs_replacement(
                        frame,
                        &preview_profile,
                        output_extent,
                    );
                    if replace {
                        (
                            preview_source,
                            output_extent,
                            VolumeColorSchedule::InteractivePreview,
                            preview_profile,
                            false,
                            replace,
                        )
                    } else {
                        let strip_height = self
                            .volume_presentation
                            .refinement_strip_height(frame, &target_profile);
                        (
                            VolumeRenderPlanSource::Scope(scope),
                            output_extent,
                            VolumeColorSchedule::AtomicRefinement {
                                strip_height_pixels: strip_height,
                            },
                            target_profile,
                            staged_3d_refinement,
                            false,
                        )
                    }
                }
            };

            let presented_target_map_matches = self
                .dataset
                .current_target_layer_scales()
                .is_some_and(|target| {
                    self.render_coordination
                        .surface(PresentationTarget::ThreeD)
                        .presented_frame()
                        .is_some_and(|presented| {
                            presented.frame() == frame
                                && presented.extent() == output_extent
                                && presented.progress().completeness()
                                    == RenderFrameCompleteness::Exact
                                && presented_layer_scales_match_target(
                                    presented.progress().coverage(),
                                    target,
                                )
                        })
                });
            let exact_frame_already_presented = !self.dataset.staging_current_refinement()
                && self.render_coordination.frame_fidelity.display_freshness
                    == DisplayedFrameFreshness::Current
                && !self.render_attempt.target_ready(PresentationTarget::ThreeD)
                && presented_target_map_matches
                && !self.render_coordination.frame_fidelity.three_d_preview;
            let needs_execution = !exact_frame_already_presented
                && (request_staged_refinement
                    || matches!(
                        request_schedule,
                        VolumeColorSchedule::AtomicRefinement { .. }
                    )
                    || (request_schedule == VolumeColorSchedule::InteractivePreview
                        && preview_replacement_needed)
                    || (request_schedule == VolumeColorSchedule::Direct
                        && self.render_coordination.frame_fidelity.three_d_preview)
                    || self.render_coordination.frame_fidelity.display_freshness
                        != DisplayedFrameFreshness::Current
                    || self.render_attempt.target_ready(PresentationTarget::ThreeD));
            let needs_execution = needs_execution
                || presentation_transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.contains(PresentationTarget::ThreeD));
            let (request_scope, navigation_candidate) = match request_source {
                VolumeRenderPlanSource::Scope(scope) => (scope, None),
                VolumeRenderPlanSource::Navigation(index) => (SCOPE_CURRENT_3D, Some(index)),
            };
            if needs_execution
                && let Some(request) = self.build_coordinated_request(
                    PanelId::ThreeD,
                    request_scope,
                    &snapshot,
                    self.render_coordination.presentation_viewport,
                    request_extent,
                    output_extent,
                    request_schedule,
                    Some(request_profile),
                    None,
                    request_staged_refinement,
                    navigation_candidate,
                    presentation_transaction.as_ref(),
                )?
            {
                requests.push(request);
            }
        }

        if application_view(&snapshot).layout() == CanonicalViewerLayout::FourPanel {
            for panel in [PanelId::Xy, PanelId::Xz, PanelId::Yz] {
                let transaction_requires_panel = presentation_transaction
                    .as_ref()
                    .is_some_and(|transaction| transaction.contains(panel.presentation_slot()));
                // A playback-stop handoff is a quality-only transaction. Its
                // retained front remains the visible authority until the
                // ordinary stationary planner has produced a current body
                // for every linked target. A transiently empty or stale scope
                // during playback-scope teardown is not a new empty image.
                if retained_quality_handoff
                    && (!demand_currentness.cross_section(panel)
                        || !demand_renderability.cross_section(panel))
                {
                    continue;
                }
                if !transaction_requires_panel && !demand_renderability.cross_section(panel) {
                    continue;
                }
                let scope = cross_section_scope(panel)?;
                let prepared = self
                    .prepared_scope_render_plans
                    .get(&scope)
                    .ok_or_else(|| anyhow::anyhow!("linked scope has no prepared render plan"))?;
                if !transaction_requires_panel
                    && !self.cross_section_panel_needs_display_render(panel, &prepared.requirements)
                {
                    continue;
                }
                let priority = self.dataset.scope_gpu_priority_handle(scope);
                let required = self.dataset.scope_required_prefix_len(scope);
                let requirements = &priority[..required];
                let view = application_view(&snapshot);
                let uniform_target = self
                    .dataset
                    .scope_layer_scales(scope)
                    .and_then(crate::dataset_demand_plan::uniform_layer_scale);
                let all_requirements_available = scope_resources_complete_with_renderer(
                    &self.dataset,
                    &self.native_presentation,
                    scope,
                );
                let mut schedule = schedule_cross_section_panel(
                    &mut self.render_coordination,
                    CrossSectionScheduleInput {
                        view,
                        uniform_target,
                        requirements,
                        first_useful_requirements: prepared.requirements.first_useful_prefix_len(),
                        first_useful_available: first_useful_resources_complete_with_renderer(
                            &self.dataset,
                            &self.native_presentation,
                            prepared,
                        ),
                        retained_leases: self.dataset.retained_leases(),
                        all_requirements_available,
                        dataset_failed: self.dataset.dispatcher().scope_failure(scope).is_some(),
                        #[cfg(test)]
                        requirement_visit_counter: None,
                    },
                    panel,
                    true,
                )?
                .schedule;
                if !transaction_requires_panel && !demand_currentness.cross_section(panel) {
                    schedule = schedule.provisional();
                }
                if schedule.status == CrossSectionPanelScheduleStatus::Empty
                    && !transaction_requires_panel
                {
                    self.clear_cross_section_product_presentation(panel);
                    if !self
                        .render_coordination
                        .record_empty_cross_section_presentation(
                            panel.presentation_slot(),
                            schedule.generation,
                            schedule,
                        )
                    {
                        anyhow::bail!("stale empty cross-section presentation was suppressed");
                    }
                    continue;
                }
                if !schedule.is_renderable()
                    && (!transaction_requires_panel
                        || schedule.status != CrossSectionPanelScheduleStatus::Empty)
                {
                    continue;
                }
                let surface = self.render_coordination.surface(panel.presentation_slot());
                let presentation = surface.presentation_viewport().ok_or_else(|| {
                    anyhow::anyhow!("cross-section presentation viewport is unavailable")
                })?;
                let extent = surface.render_viewport().ok_or_else(|| {
                    anyhow::anyhow!("cross-section render viewport is unavailable")
                })?;
                if let Some(request) = self.build_coordinated_request(
                    panel,
                    scope,
                    &snapshot,
                    presentation,
                    extent,
                    extent,
                    VolumeColorSchedule::Direct,
                    None,
                    Some((surface.generation(), schedule)),
                    false,
                    None,
                    presentation_transaction.as_ref(),
                )? {
                    requests.push(request);
                }
            }
        }

        let request_cohort = if let Some(transaction) = presentation_transaction.as_ref() {
            let incomplete_fingerprint = RenderAttemptFingerprint::new(
                &snapshot,
                &requests,
                self.dataset_runtime_epoch,
                self.dataset.cpu_capacity_epoch(),
                self.native_presentation
                    .product_gpu
                    .as_ref()
                    .map_or(0, |product| product.renderer.device_generation().get()),
            );
            let mut members = std::array::from_fn(|_| None);
            for request in std::mem::take(&mut requests) {
                let target = request.target;
                let member = PresentationTransactionMember::new(
                    request.target,
                    transaction.source_generation(),
                    transaction.timepoint(),
                    transaction.expected_revision(request.target),
                    request.surface_generation,
                    PresentationQuality::exact(Arc::clone(&request.layer_scales)),
                    request,
                );
                if members[target.index()].replace(member).is_some() {
                    anyhow::bail!(
                        "a composed presentation assembled target {target:?} more than once"
                    );
                }
            }
            let mut logical =
                match PresentationTransactionTargets::from_slots(transaction.target_set(), members)
                {
                    Ok(logical) => logical,
                    Err(missing) => {
                        // A logical transaction is fixed-shape. Until every immutable
                        // body exists, retain the predecessor and do not submit a
                        // shorter physical vector as if it were the semantic frame.
                        self.render_attempt.wait(
                            incomplete_fingerprint,
                            RenderWaitKey::LogicalMember {
                                target: missing.0,
                                family_revision: transaction.expected_revision(missing.0).get(),
                            },
                        );
                        self.observe_coordinated_display_milestones(false);
                        self.record_current_layout_presentation_if_complete();
                        return Ok(DisplayRefreshWorkTiming::new(
                            DisplayRenderTiming {
                                path: DisplayRefreshPath::UiBackground,
                                render_ms: 0.0,
                                gpu_upload_ms: None,
                                gpu_compute_ms: None,
                                egui_texture_ms: 0.0,
                            },
                            visible_brick_request_ms,
                        ));
                    }
                };
            let publication_group = transaction.publication_group();
            logical.for_each_mut(|member| {
                debug_assert_eq!(member.source_generation(), transaction.source_generation());
                debug_assert_eq!(member.timepoint(), transaction.timepoint());
                debug_assert_eq!(
                    member.spatial_frame(),
                    transaction.expected_revision(member.target())
                );
                debug_assert_eq!(
                    member.surface_generation(),
                    member.prepared_request().surface_generation
                );
                debug_assert!(Arc::ptr_eq(
                    member.quality().layer_scales(),
                    &member.prepared_request().layer_scales
                ));
                let request = member.prepared_request_mut();
                request.atomic_publication_group = Some(publication_group);
                request.render_policy = RetainedFrameRenderPolicy::ExactFrameOnly;
            });
            CoordinatedRequestCohort::Logical(logical.into_prepared_requests())
        } else {
            CoordinatedRequestCohort::Physical(requests)
        };
        let logical_transaction = request_cohort.is_logical();
        let requests = request_cohort.as_slice();

        let desired = self.desired_coordinated_layout(&snapshot, requests);
        let egui_texture_ms = self.apply_coordinated_layout(&desired)?;
        let Some(active_target) = self.coordinated_active_target(&snapshot, requests) else {
            self.observe_coordinated_display_milestones(false);
            self.record_current_layout_presentation_if_complete();
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        };

        let renderer_device_generation = self
            .native_presentation
            .product_gpu
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?
            .renderer
            .device_generation()
            .get();
        let attempt_fingerprint = RenderAttemptFingerprint::new(
            &snapshot,
            requests,
            self.dataset_runtime_epoch,
            self.dataset.cpu_capacity_epoch(),
            renderer_device_generation,
        );
        if !self.render_attempt.begin(attempt_fingerprint.clone()) {
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: 0.0,
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        }
        let display_generation = self
            .render_coordination
            .display_generation()
            .input_generation;
        let staged_candidate_ready = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let mut ready = false;
            for request in requests
                .iter()
                .filter(|request| request.staged_3d_refinement)
            {
                ready |= match request.volume_schedule {
                    VolumeColorSchedule::Direct => true,
                    VolumeColorSchedule::InteractivePreview => false,
                    VolumeColorSchedule::AtomicRefinement { .. } => {
                        let borrowed = CoordinatedTargetRequest::new(
                            request.target,
                            &request.request.intent,
                            &request.request.requirements,
                            display_generation,
                            request.render_policy,
                        )
                        .with_volume_schedule(request.output_extent, request.volume_schedule, false)
                        .with_hidden_promotion_authorized(false);
                        product
                            .renderer
                            .poll_coordinated_hidden_refinement_ready(borrowed)?
                    }
                };
            }
            ready
        };
        let prepared_staged_promotion = (staged_candidate_ready
            && self.staged_3d_promotion_payloads_ready())
        .then(|| self.prepare_coordinated_staged_current_promotion())
        .transpose()?;
        #[cfg(test)]
        {
            self.product_render_attempts = self.product_render_attempts.saturating_add(1);
        }
        let staged_promotion_ready = prepared_staged_promotion.is_some();
        let borrowed = match &request_cohort {
            CoordinatedRequestCohort::Physical(requests) => {
                let mut borrowed = Vec::with_capacity(requests.len());
                for request in requests {
                    borrowed.push(borrow_coordinated_request(
                        request,
                        display_generation,
                        staged_promotion_ready,
                    ));
                }
                BorrowedCoordinatedRequestCohort::Physical(borrowed)
            }
            CoordinatedRequestCohort::Logical(FixedPresentationTargetRequests::ThreeD(
                [three_d],
            )) => BorrowedCoordinatedRequestCohort::Logical(
                FixedPresentationTargetRequests::ThreeD([borrow_coordinated_request(
                    three_d,
                    display_generation,
                    staged_promotion_ready,
                )]),
            ),
            CoordinatedRequestCohort::Logical(FixedPresentationTargetRequests::FourPanel(
                [three_d, xy, xz, yz],
            )) => BorrowedCoordinatedRequestCohort::Logical(
                FixedPresentationTargetRequests::FourPanel([
                    borrow_coordinated_request(three_d, display_generation, staged_promotion_ready),
                    borrow_coordinated_request(xy, display_generation, staged_promotion_ready),
                    borrow_coordinated_request(xz, display_generation, staged_promotion_ready),
                    borrow_coordinated_request(yz, display_generation, staged_promotion_ready),
                ]),
            ),
        };
        let placement_target_group = match (
            requests
                .iter()
                .any(|request| request.panel == PanelId::ThreeD),
            requests
                .iter()
                .any(|request| request.panel != PanelId::ThreeD),
        ) {
            (false, true) => "linked 2D",
            (true, false) => "3D",
            (true, true) | (false, false) => "visible aggregate",
        };
        let render_started = Instant::now();
        let report = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("progressive GPU renderer is unavailable"))?;
            let execution = match &borrowed {
                BorrowedCoordinatedRequestCohort::Logical(
                    FixedPresentationTargetRequests::ThreeD([three_d]),
                ) => product.renderer.execute_coordinated_logical_frame(
                    snapshot.catalog(),
                    active_target,
                    CoordinatedLogicalTargetSet::three_d(*three_d)?,
                ),
                BorrowedCoordinatedRequestCohort::Logical(
                    FixedPresentationTargetRequests::FourPanel([three_d, xy, xz, yz]),
                ) => product.renderer.execute_coordinated_logical_frame(
                    snapshot.catalog(),
                    active_target,
                    CoordinatedLogicalTargetSet::four_panel(*three_d, *xy, *xz, *yz)?,
                ),
                BorrowedCoordinatedRequestCohort::Physical(requests) => product
                    .renderer
                    .execute_coordinated_frame(snapshot.catalog(), active_target, requests),
            };
            match execution {
                Ok(report) => report,
                Err(error @ WgpuRenderRuntimeError::StaleFrame { .. }) => {
                    product.stale_frames_rejected = product.stale_frames_rejected.saturating_add(1);
                    tracing::debug!(%error, "stale coordinated frame was rejected");
                    let (stale_target, revision) = match error {
                        WgpuRenderRuntimeError::StaleFrame {
                            target, current, ..
                        } => (target.unwrap_or(active_target), current.get()),
                        _ => unreachable!("the enclosing pattern is a stale frame"),
                    };
                    self.render_attempt.wait(
                        attempt_fingerprint.clone(),
                        RenderWaitKey::MailboxAdvance {
                            target: stale_target,
                            minimum_frame: revision,
                        },
                    );
                    return Ok(DisplayRefreshWorkTiming::new(
                        DisplayRenderTiming {
                            path: DisplayRefreshPath::UiBackground,
                            render_ms: duration_ms(render_started.elapsed()),
                            gpu_upload_ms: None,
                            gpu_compute_ms: None,
                            egui_texture_ms,
                        },
                        visible_brick_request_ms,
                    ));
                }
                Err(error @ WgpuRenderRuntimeError::PayloadPlacementUnavailable { .. }) => {
                    match product.renderer.recover_payload_fragmentation() {
                        Ok(true) => {
                            tracing::info!(
                                %error,
                                "compacted fragmented GPU payload residency; coordinated frame will retry"
                            );
                            self.render_attempt.wait(
                                attempt_fingerprint.clone(),
                                RenderWaitKey::SubmissionCleanup {
                                    submission: product.renderer.next_submission_completion_event(),
                                },
                            );
                            return Ok(DisplayRefreshWorkTiming::new(
                                DisplayRenderTiming {
                                    path: DisplayRefreshPath::UiBackground,
                                    render_ms: duration_ms(render_started.elapsed()),
                                    gpu_upload_ms: None,
                                    gpu_compute_ms: None,
                                    egui_texture_ms,
                                },
                                visible_brick_request_ms,
                            ));
                        }
                        Err(WgpuRenderRuntimeError::PayloadRecoveryDeferred) => {
                            self.render_attempt.wait(
                                attempt_fingerprint.clone(),
                                RenderWaitKey::SubmissionCleanup {
                                    submission: product.renderer.next_submission_completion_event(),
                                },
                            );
                            return Ok(DisplayRefreshWorkTiming::new(
                                DisplayRenderTiming {
                                    path: DisplayRefreshPath::UiBackground,
                                    render_ms: duration_ms(render_started.elapsed()),
                                    gpu_upload_ms: None,
                                    gpu_compute_ms: None,
                                    egui_texture_ms,
                                },
                                visible_brick_request_ms,
                            ));
                        }
                        Err(recovery_error) => return Err(recovery_error.into()),
                        Ok(false) => {}
                    }
                    if self
                        .recover_from_coordinated_payload_placement(placement_target_group, error)
                    {
                        tracing::info!(
                            %error,
                            target_group = placement_target_group,
                            "residual GPU placement refusal will retry through adaptive visible-demand selection"
                        );
                        self.render_attempt.wait(
                            attempt_fingerprint.clone(),
                            RenderWaitKey::CandidatePlan {
                                demand_revision: self.dataset_runtime_epoch,
                            },
                        );
                        return Ok(DisplayRefreshWorkTiming::new(
                            DisplayRenderTiming {
                                path: DisplayRefreshPath::UiBackground,
                                render_ms: duration_ms(render_started.elapsed()),
                                gpu_upload_ms: None,
                                gpu_compute_ms: None,
                                egui_texture_ms,
                            },
                            visible_brick_request_ms,
                        ));
                    }
                    let failure = render_state::render_failure_status(&anyhow::Error::new(error));
                    self.render_attempt
                        .fail(attempt_fingerprint.clone(), failure);
                    tracing::error!(%error, "deterministic coordinated render request failed");
                    return Ok(DisplayRefreshWorkTiming::new(
                        DisplayRenderTiming {
                            path: DisplayRefreshPath::UiBackground,
                            render_ms: duration_ms(render_started.elapsed()),
                            gpu_upload_ms: None,
                            gpu_compute_ms: None,
                            egui_texture_ms,
                        },
                        visible_brick_request_ms,
                    ));
                }
                Err(error) => {
                    let disposition = classify_color_runtime_error(error);
                    let failure = render_state::render_failure_status(&anyhow::Error::new(error));
                    match disposition {
                        ColorRuntimeDisposition::ColorUnavailable => {
                            self.render_attempt.color_unavailable(failure);
                        }
                        ColorRuntimeDisposition::RendererTerminal => {
                            self.enter_product_renderer_terminal(error);
                        }
                        ColorRuntimeDisposition::Wait(reason) => {
                            let key = match reason {
                                RenderWaitReason::InitialPipeline => {
                                    RenderWaitKey::InitialPipeline {
                                        device_generation: renderer_device_generation,
                                    }
                                }
                                RenderWaitReason::SubmissionCleanup => {
                                    RenderWaitKey::SubmissionCleanup {
                                        submission: product
                                            .renderer
                                            .next_submission_completion_event(),
                                    }
                                }
                                RenderWaitReason::EvictionAcknowledgement => {
                                    RenderWaitKey::EvictionAcknowledgement {
                                        ledger_revision: product
                                            .renderer
                                            .pending_residency_evictions(1)
                                            .first()
                                            .map_or(0, |event| event.sequence()),
                                    }
                                }
                                RenderWaitReason::HiddenWorker => unreachable!(
                                    "hidden-worker waits originate only in a complete execution report"
                                ),
                                RenderWaitReason::LogicalMember
                                | RenderWaitReason::MailboxAdvance
                                | RenderWaitReason::CandidatePlan
                                | RenderWaitReason::RelevantResidency => {
                                    unreachable!(
                                        "application-owned waits do not originate as renderer errors"
                                    )
                                }
                            };
                            self.render_attempt.wait(attempt_fingerprint.clone(), key);
                        }
                        ColorRuntimeDisposition::DeterministicFailure => {
                            self.render_attempt
                                .fail(attempt_fingerprint.clone(), failure);
                            tracing::error!(%error, "deterministic coordinated render request failed");
                        }
                        ColorRuntimeDisposition::AuxiliaryOnly => {
                            tracing::warn!(%error, "auxiliary renderer outcome reached the color boundary");
                        }
                        ColorRuntimeDisposition::Stale
                        | ColorRuntimeDisposition::PlacementRecovery => {
                            unreachable!("stale and placement outcomes have dedicated handling")
                        }
                    }
                    return Ok(DisplayRefreshWorkTiming::new(
                        DisplayRenderTiming {
                            path: DisplayRefreshPath::UiBackground,
                            render_ms: duration_ms(render_started.elapsed()),
                            gpu_upload_ms: None,
                            gpu_compute_ms: None,
                            egui_texture_ms,
                        },
                        visible_brick_request_ms,
                    ));
                }
            }
        };
        let next_submission = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map_or(0, |product| {
                product.renderer.next_submission_completion_event()
            });
        let observation =
            normalize_color_attempt_observation(&report, requests, active_target, next_submission);
        if let ColorAttemptObservation::Failed(failure) = &observation {
            // A local hidden-refinement failure leaves the predecessor and all
            // public transaction state untouched. Residency consequences are
            // still retired below because the renderer has already committed
            // those authoritative directory changes.
            self.render_attempt
                .fail(attempt_fingerprint.clone(), failure.clone());
            let refill_admission = self.retire_coordinated_gpu_resident_payloads(&report) > 0;
            if refill_admission {
                self.dataset.defer_interactive_admission_refill(true);
            }
            tracing::error!("hidden exact refinement failed for the current render request");
            return Ok(DisplayRefreshWorkTiming::new(
                DisplayRenderTiming {
                    path: DisplayRefreshPath::UiBackground,
                    render_ms: duration_ms(render_started.elapsed()),
                    gpu_upload_ms: None,
                    gpu_compute_ms: None,
                    egui_texture_ms,
                },
                visible_brick_request_ms,
            ));
        }
        self.apply_coordinated_execution_report(
            &snapshot,
            requests,
            &report,
            prepared_staged_promotion,
            presentation_transaction.as_ref(),
            logical_transaction,
        );
        let refill_admission = self.retire_coordinated_gpu_resident_payloads(&report) > 0;
        if refill_admission {
            self.dataset.defer_interactive_admission_refill(true);
        }
        match observation {
            ColorAttemptObservation::Current => self.render_attempt.settle(),
            ColorAttemptObservation::Ready => {
                // `begin` left this exact fingerprint admitted in `Ready`.
                // The completed report is the sole authority that exactly one
                // same-fingerprint successor can make immediate progress.
                self.render_attempt.continue_ready(&attempt_fingerprint);
            }
            ColorAttemptObservation::Waiting(key) => {
                self.render_attempt.wait(attempt_fingerprint.clone(), key);
            }
            ColorAttemptObservation::Failed(_) => {
                unreachable!("a normalized local failure returned before report application")
            }
        }
        self.observe_coordinated_display_milestones(false);
        self.record_current_layout_presentation_if_complete();
        Ok(DisplayRefreshWorkTiming::new(
            DisplayRenderTiming {
                path: if report.color_queue_submissions() > 0
                    || PresentationTarget::ALL.into_iter().any(|target| {
                        self.render_coordination
                            .surface(target)
                            .presented_frame()
                            .is_some()
                    }) {
                    DisplayRefreshPath::GpuResidentDisplay
                } else {
                    DisplayRefreshPath::UiBackground
                },
                render_ms: duration_ms(render_started.elapsed()),
                gpu_upload_ms: None,
                gpu_compute_ms: None,
                egui_texture_ms,
            },
            visible_brick_request_ms,
        ))
    }

    fn apply_coordinated_execution_report(
        &mut self,
        snapshot: &ApplicationSnapshot,
        requests: &[CoordinatedOwnedRequest],
        report: &CoordinatedFrameExecutionReport,
        prepared_staged_promotion: Option<PreparedCoordinatedStagedPromotion>,
        transaction: Option<&PresentationTransaction>,
        logical_transaction_assembled: bool,
    ) {
        let any_target_applied = report
            .targets()
            .iter()
            .any(|target| target.presented() || target.current());
        if let Some(timing) = report.cpu_timing() {
            real_interaction_trace::record_renderer_cpu_timing(timing);
        }
        if let Some(ticket) = report.gpu_timing()
            && let Some(request) = requests
                .iter()
                .find(|request| request.target == ticket.target())
            && let Some(profile) = request.volume_profile.clone()
        {
            self.volume_presentation.track_timing(
                ticket,
                profile,
                request.volume_schedule,
                request.request.intent.frame(),
                report
                    .target(request.target)
                    .and_then(|target| target.volume_refinement()),
            );
        }
        let staged_satisfied = requests.iter().any(|request| {
            request.staged_3d_refinement
                && report
                    .target(request.target)
                    .is_some_and(|target| target.presented() || target.current())
        });
        if staged_satisfied {
            assert!(
                requests
                    .iter()
                    .filter(|request| request.staged_3d_refinement)
                    .all(|request| {
                        report.target(request.target).is_some_and(|target| {
                            target.progress().is_some_and(|progress| {
                                progress.completeness() == RenderFrameCompleteness::Exact
                            })
                        })
                    }),
                "the renderer may publish a staged 3D candidate only at exact coverage"
            );
            self.commit_coordinated_staged_current_promotion(
                snapshot,
                prepared_staged_promotion
                    .expect(
                        "an exact staged candidate passed payload readiness and promotion preflight before submission",
                    ),
            );
        }
        let bindings = {
            let product = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("the renderer that produced the coordinated report remains installed");
            product.last_coordinated_cpu_timing = report.cpu_timing();
            product.last_coordinated_recorded_targets =
                report.recorded_targets().to_vec().into_boxed_slice();
            product.last_coordinated_color_submissions = report.color_queue_submissions();
            product.total_coordinated_color_submissions = product
                .total_coordinated_color_submissions
                .saturating_add(u64::from(report.color_queue_submissions()));
            product.last_coordinated_residency_submissions = report.residency_queue_submissions();
            if let Some(ticket) = report.gpu_timing() {
                product.pending_gpu_timings.push_back(ticket);
            }
            report
                .targets()
                .iter()
                .filter(|target_report| target_report.presented())
                .map(|target_report| {
                    (
                        target_report.target(),
                        target_report.device_generation(),
                        target_report.texture_revision(),
                        product
                            .renderer
                            .coordinated_target_texture_view(target_report.target())
                            .expect(
                                "a presented coordinated target retains its current texture view",
                            )
                            .clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        for (target, device_generation, texture_revision, view) in bindings {
            self.native_presentation.bind_texture(
                target,
                device_generation,
                texture_revision,
                &view,
            );
        }

        for request in requests {
            let target_report = report
                .target(request.target)
                .expect("a coordinated report contains every requested target");
            real_interaction_trace::record_coordinated_execution(
                request.target,
                request.request.intent.frame(),
                match request.request.intent.view() {
                    RenderViewIntent::CrossSection(view) => {
                        Some(view.scale_world_per_screen_point())
                    }
                    RenderViewIntent::Volume { .. } => None,
                },
                target_report.presented(),
                report.color_queue_submissions(),
            );
            if request.panel == PanelId::ThreeD
                && let Some(refinement) = target_report.volume_refinement()
            {
                self.render_coordination
                    .frame_fidelity
                    .three_d_refinement_strips_completed = refinement.completed_strips();
                self.render_coordination
                    .frame_fidelity
                    .three_d_refinement_strips_total = refinement.total_strips();
                self.render_coordination.frame_fidelity.refinement_pending =
                    !refinement.is_complete();
            }
            let facts = ColorAttemptTargetFacts {
                target: target_report.target(),
                current: target_report.current(),
                presented: target_report.presented(),
                deferred_by_backpressure: target_report.deferred_by_backpressure(),
                actionable_work_remaining: target_report.actionable_work_remaining(),
                hidden_waiting: matches!(
                    target_report.hidden_refinement(),
                    Some(
                        HiddenRefinementState::Running(_)
                            | HiddenRefinementState::WaitingForSubmission { .. }
                    )
                ),
                hidden_refinement: target_report.hidden_refinement(),
            };
            if !report_member_should_apply(facts, target_report.disposition()) {
                continue;
            }
            let progress = target_report
                .progress()
                .cloned()
                .expect("a presented coordinated target contains progress");
            let frame = PresentedFrame::new(request.target, request.output_extent, progress);
            if request.staged_3d_refinement {
                debug_assert_eq!(
                    frame.progress().completeness(),
                    RenderFrameCompleteness::Exact,
                    "the renderer publishes an ExactFrameOnly candidate only after exact coverage"
                );
            }

            let previous_partial = self
                .render_coordination
                .surface(request.target)
                .presented_frame()
                .is_some_and(|previous| {
                    previous.progress().completeness() == RenderFrameCompleteness::Progressive
                });
            let current_partial =
                frame.progress().completeness() == RenderFrameCompleteness::Progressive;
            if let Some(product) = self.native_presentation.product_gpu.as_mut() {
                if current_partial && !previous_partial {
                    product.current_partial_frames_presented =
                        product.current_partial_frames_presented.saturating_add(1);
                }
                if !current_partial && previous_partial {
                    product.partial_to_settled_transitions =
                        product.partial_to_settled_transitions.saturating_add(1);
                }
                if let Some(ticket) = target_report.validation_capture() {
                    product.queue_validation_capture(frame.clone(), ticket);
                }
            }

            if let Some((generation, schedule)) = request.cross_section {
                let coverage = frame.progress().coverage();
                let schedule = cross_section_schedule_for_presented_coverage(
                    schedule,
                    frame.progress().completeness(),
                    coverage.available_requirements(),
                    coverage.total_requirements(),
                );
                assert!(
                    self.render_coordination.record_cross_section_presentation(
                        request.target,
                        generation,
                        schedule,
                    ),
                    "the synchronously submitted cross-section generation remains current"
                );
            }
            // Cutoff CPU timing is recorded once on ProductGpuRenderRuntime;
            // a four-target cutoff is not four independent CPU executions.
            self.record_coordinated_product_frame(
                request.panel,
                request.surface_generation,
                &frame,
                None,
                target_report.presented(),
            );
            self.record_presented_volume_schedule(request);
            self.record_coordinated_layer_presentations_with_scales(
                request.panel,
                Some(request.layer_scales.as_ref()),
            );
        }
        if any_target_applied && self.dataset.last_plan_error().is_none() {
            self.render_coordination.frame_fidelity.last_failure_kind = None;
            self.render_coordination.frame_fidelity.last_capacity_error = None;
        }
        let every_logical_member_current = logical_transaction_assembled
            && requests.iter().all(|request| {
                report
                    .target(request.target)
                    .is_some_and(|target| target.current())
            });
        if let Some(transaction) = transaction
            && every_logical_member_current
        {
            // The full renderer report is the proof: every member is either a
            // newly executed Exact front or a front revalidated under the
            // same coordinator observation. Advance only the temporal cursor
            // here; spatial mailboxes remain independently latest-only.
            self.complete_presentation_transaction(snapshot, transaction);
        }
    }

    fn complete_presentation_transaction(
        &mut self,
        snapshot: &ApplicationSnapshot,
        transaction: &PresentationTransaction,
    ) {
        if transaction.temporal_contract().is_some() {
            self.playback_session.mark_ready(transaction.timepoint());
            self.playback_session.observe_readiness(
                transaction.timepoint(),
                snapshot.timepoint_count(),
                true,
                false,
            );
        }
        let spatial_followup_required =
            transaction.spatial_followup_required(self.render_intent_mailbox.snapshot());
        self.presentation_scheduler.complete(transaction);
        if spatial_followup_required {
            self.render_coordination.request_refresh();
        }
    }

    fn retire_coordinated_gpu_resident_payloads(
        &mut self,
        report: &CoordinatedFrameExecutionReport,
    ) -> usize {
        let newly_resident = report
            .targets()
            .iter()
            .flat_map(|target| target.newly_resident_keys().iter().copied())
            .collect::<BTreeSet<_>>();
        let (dataset, native_presentation) = (&mut self.dataset, &self.native_presentation);
        let Some(product) = native_presentation.product_gpu.as_ref() else {
            return 0;
        };
        dataset.retire_gpu_resident_current_payloads(newly_resident, |key| {
            product.renderer.resource_is_resident(key)
        })
    }

    fn prepare_coordinated_staged_current_promotion(
        &mut self,
    ) -> anyhow::Result<PreparedCoordinatedStagedPromotion> {
        if !self.staged_3d_promotion_payloads_ready() {
            anyhow::bail!(
                "staged promotion reconciliation requires every primary payload to be retained or GPU-resident"
            );
        }
        let mut scope_targets = ScopeReconciliationTargets::default();
        if !self
            .dataset
            .prepare_staged_current_promotion_scope_targets(&mut scope_targets)
        {
            anyhow::bail!("the complete coordinated candidate has no staged dataset plan");
        }
        let post_promotion_update = self
            .staged_post_promotion_renderer_update
            .clone()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the complete coordinated candidate has no prepared promotion union"
                )
            })?;
        self.dataset
            .preflight_prepared_renderer_requirement_update(
                &post_promotion_update.previous.requirements,
                &post_promotion_update.next.requirements,
            )?;
        let prepared_scope_reconciliation =
            self.dataset.prepare_scope_reconciliation(scope_targets)?;
        // This is the final fallible pre-submit boundary. The readiness gate
        // proves no missing primary payload depends on a cancelled waiter;
        // arrived payloads remain protected by retained/GPU leases. This
        // scheduling cleanup does not publish the staged dataset plan.
        self.dataset
            .commit_prepared_scope_reconciliation(prepared_scope_reconciliation)?;
        Ok(PreparedCoordinatedStagedPromotion {
            renderer_update: post_promotion_update,
        })
    }

    fn commit_coordinated_staged_current_promotion(
        &mut self,
        snapshot: &ApplicationSnapshot,
        prepared: PreparedCoordinatedStagedPromotion,
    ) {
        self.dataset
            .commit_reconciled_gpu_prefilled_staged_current_plan();
        let refinement_render_plan = self
            .prepared_scope_render_plans
            .remove(&SCOPE_CURRENT_3D_REFINEMENT)
            .expect("a preflighted coordinated candidate retains refinement requirements");
        for navigation_candidate in &mut self.navigation_render_plans {
            navigation_candidate.scope_requirements =
                Arc::clone(&refinement_render_plan.scope_requirements);
        }
        self.prepared_scope_render_plans
            .insert(SCOPE_CURRENT_3D, refinement_render_plan);
        let crate::camera_demand_cache::PreparedRendererRequirementUpdate {
            previous,
            next,
            removals,
            removal_charge: _,
        } = prepared.renderer_update;
        self.dataset.commit_preflighted_renderer_requirement_update(
            previous.requirements,
            next.requirements,
            &removals,
            next.charge,
        );
        self.native_presentation
            .product_gpu
            .as_mut()
            .expect("the submitted coordinated renderer remains installed")
            .renderer
            .retire_residency_offers(&removals);
        self.staged_post_promotion_renderer_update = None;
        let base = RenderIntentBase::from_snapshot(snapshot);
        if self.render_intent_mailbox.active_target(base) == Some(RenderIntentTarget::ThreeD)
            && self.visible_demand_plan_currentness().current_3d
            && let Some(revision) = self.render_intent_mailbox.active_revision(base)
        {
            self.render_intent_mailbox.mark_renderable(base, revision);
        }
    }

    fn record_coordinated_product_frame(
        &mut self,
        panel: PanelId,
        surface_generation: u64,
        frame: &PresentedFrame,
        cpu_timing: Option<CpuFrameTiming>,
        record_presentation_interval: bool,
    ) {
        if record_presentation_interval
            && let Some(product) = self
                .native_presentation
                .product_gpu
                .as_mut()
                .filter(|product| product.presented_frame_interval_timing_enabled())
        {
            product.record_presented_frame_interval(panel, frame.frame(), cpu_timing);
        }
        assert!(
            self.render_coordination.record_presented_frame(
                panel.presentation_slot(),
                surface_generation,
                frame.clone(),
            ),
            "a coordinated report must match its semantic application surface generation and extent"
        );
        if panel != PanelId::ThreeD {
            return;
        }
        if self.dataset.staging_current_refinement()
            && frame.progress().completeness() != RenderFrameCompleteness::Progressive
        {
            // A settled coarse front can be published after the last dataset
            // completion wake was consumed by this cutoff. Queue the private
            // exact successor explicitly so an otherwise idle UI cannot
            // strand the staged candidate.
            self.render_coordination.request_refresh();
        }
        let snapshot = self.application.snapshot();
        let view = application_view(&snapshot);
        let displayed_scale_level = uniform_presented_scale(frame.progress().coverage());
        let target_scales = self.dataset.current_target_layer_scales();
        let target_settled = !self.dataset.staging_current_refinement()
            && target_scales.is_some_and(|target| {
                presented_layer_scales_match_target(frame.progress().coverage(), target)
            });
        let displayed_coarser_than_target = target_scales.is_some_and(|target| {
            presented_layer_scales_are_coarser_than_target(frame.progress().coverage(), target)
        });
        self.display_performance_milestones.observe_three_d(
            frame,
            displayed_coarser_than_target,
            target_settled,
        );
        let progress = frame.progress();
        let coverage = progress.coverage();
        self.render_coordination.frame_fidelity.resident_bricks =
            usize::try_from(coverage.available_requirements()).unwrap_or(usize::MAX);
        self.render_coordination
            .frame_fidelity
            .missing_occupied_bricks = usize::try_from(
            coverage
                .total_requirements()
                .saturating_sub(coverage.available_requirements()),
        )
        .unwrap_or(usize::MAX);
        self.render_coordination.frame_fidelity.completeness = match progress.completeness() {
            RenderFrameCompleteness::Progressive => FrameCompleteness::Incomplete,
            RenderFrameCompleteness::Complete => FrameCompleteness::Complete,
            RenderFrameCompleteness::Exact => {
                if displayed_scale_level == Some(0) {
                    FrameCompleteness::Exact
                } else {
                    FrameCompleteness::Complete
                }
            }
        };
        self.render_coordination.frame_fidelity.reason = match progress.limitation() {
            Some(FrameLimitation::BudgetLimited | FrameLimitation::CapacityLimited) => {
                LodDecisionReason::GpuBudgetLimited
            }
            Some(FrameLimitation::CoarserScale) => LodDecisionReason::ScreenEquivalentCoarserScale,
            Some(FrameLimitation::MissingResources) => LodDecisionReason::IncompleteResidency,
            None if self.dataset.current_capacity_constrained() => {
                LodDecisionReason::AdaptiveCapacity
            }
            None if self.dataset.current_playback_downshifted() => {
                LodDecisionReason::PlaybackDownshift
            }
            None if displayed_scale_level == Some(0) => LodDecisionReason::ExactS0,
            None => LodDecisionReason::ScreenEquivalentCoarserScale,
        };
        self.render_coordination.frame_fidelity.backend = render_backend_for_view(view);
        self.render_coordination.frame_fidelity.display_freshness =
            DisplayedFrameFreshness::Current;
        self.render_coordination.frame_fidelity.refinement_pending =
            self.dataset.staging_current_refinement();
        self.render_coordination
            .frame_fidelity
            .displayed_scale_level = displayed_scale_level;
    }

    fn record_coordinated_layer_presentations(&mut self, panel: PanelId, scope: u64) {
        let expected = self.dataset.scope_layer_scales(scope).cloned();
        self.record_coordinated_layer_presentations_with_scales(panel, expected.as_ref());
    }

    fn record_coordinated_layer_presentations_with_scales(
        &mut self,
        panel: PanelId,
        expected: Option<
            &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
        >,
    ) {
        let presented = self
            .render_coordination
            .surface(panel.presentation_slot())
            .presented_frame();
        let facts = presented
            .into_iter()
            .flat_map(|frame| frame.progress().coverage().layer_coverages())
            .map(|layer| {
                (
                    layer.layer(),
                    ProductLayerRequirementFacts {
                        displayed_scale_level: layer.scale().map(|scale| scale.get()),
                        target_scale_level: Some(layer.target_scale().get()),
                        finest_fallback_scale_level: layer
                            .finest_fallback_scale()
                            .map(|scale| scale.get()),
                        fallback_scale_level: layer.fallback_scale().map(|scale| scale.get()),
                        target_available_requirements: layer.available_target_requirements(),
                        target_total_requirements: layer.total_target_requirements(),
                        available_requirements: layer.available_requirements(),
                        total_requirements: layer.total_requirements(),
                        mixed: layer.is_mixed(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let current = if panel == PanelId::ThreeD {
            self.render_coordination.frame_fidelity.display_freshness
                == DisplayedFrameFreshness::Current
        } else {
            self.render_coordination
                .surface(panel.presentation_slot())
                .display_current()
                && self.visible_demand_plan_currentness().cross_section(panel)
        };
        let layers = expected
            .into_iter()
            .flat_map(|scales| scales.keys())
            .chain(facts.keys())
            .copied()
            .collect::<BTreeSet<_>>();
        let presentations = layers
            .into_iter()
            .map(|layer| {
                product_layer_presentation_status(
                    layer.ordinal(),
                    expected
                        .and_then(|scales| scales.get(&layer))
                        .map(|scale| scale.get()),
                    facts.get(&layer).copied(),
                    current,
                )
            })
            .collect::<Vec<_>>();
        if let Err(overflow) = self
            .render_coordination
            .set_layer_presentations(panel.presentation_slot(), presentations)
        {
            tracing::error!(
                panel = panel.label(),
                actual = overflow.actual,
                maximum = overflow.maximum,
                "layer presentation instrumentation exceeded the renderer layer bound"
            );
        }
    }

    pub(crate) fn poll_product_validation_captures(&mut self) -> anyhow::Result<()> {
        let pending_count = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map_or(0, |product| product.pending_validation_captures.len());
        let mut first_error = None;
        for _ in 0..pending_count {
            let pending = self
                .native_presentation
                .product_gpu
                .as_mut()
                .and_then(|product| product.pending_validation_captures.pop_front())
                .expect("the bounded pending count came from the same queue");
            let result = self
                .native_presentation
                .product_gpu
                .as_mut()
                .expect("a pending capture belongs to the product renderer")
                .renderer
                .poll_coordinated_validation_capture(pending.ticket);
            match result {
                Ok(None) => self
                    .native_presentation
                    .product_gpu
                    .as_mut()
                    .expect("a pending capture belongs to the product renderer")
                    .pending_validation_captures
                    .push_back(pending),
                Ok(Some(capture)) => {
                    let target = pending.ticket.target();
                    let frame_is_current =
                        self.render_coordination.surface(target).presented_frame()
                            == Some(&pending.frame);
                    let binding = self.native_presentation.texture_binding_identity(target);
                    if frame_is_current {
                        let destination = &mut self
                            .native_presentation
                            .product_gpu
                            .as_mut()
                            .expect("a completed capture belongs to the product renderer")
                            .completed_validation_captures[target.index()];
                        install_if_current_texture_revision(
                            binding,
                            pending.ticket.device_generation().get(),
                            pending.ticket.texture_revision().get(),
                            destination,
                            CompletedCoordinatedValidationCapture {
                                frame: pending.frame,
                                ticket: pending.ticket,
                                capture,
                            },
                        );
                    }
                }
                Err(WgpuRenderRuntimeError::StaleValidationCapture) => {
                    // A newer frame may replace the captured presentation
                    // before its asynchronous readback is mapped. The old
                    // observation is then intentionally unusable, but this is
                    // not a renderer failure: discard it and let the current
                    // frame's capture (or the next render) establish evidence.
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => {
                if classify_color_runtime_error(error) == ColorRuntimeDisposition::RendererTerminal
                {
                    self.enter_product_renderer_terminal(error);
                }
                Err(error.into())
            }
            None => Ok(()),
        }
    }

    pub(crate) fn record_display_refresh_timing(
        &mut self,
        render: DisplayRenderTiming,
        visible_brick_request_ms: f64,
        total_ms: f64,
    ) {
        self.render_coordination.last_display_refresh_timing = Some(DisplayRefreshTiming {
            path: render.path,
            render_ms: render.render_ms,
            gpu_upload_ms: render.gpu_upload_ms,
            gpu_compute_ms: render.gpu_compute_ms,
            egui_texture_ms: render.egui_texture_ms,
            visible_brick_request_ms,
            total_ms,
        });
    }

    /// Executes one coordinated renderer observation and reports its complete
    /// fixed-target composition mutation.
    ///
    /// The egui paint list for the current UI turn is built before this
    /// method is called. Every non-unchanged result therefore requires one
    /// subsequent composition wake; otherwise either new texture pixels or a
    /// newly cleared surface can remain absent from the mapped window.
    pub(crate) fn refresh_frame(&mut self) -> DisplayCompositionChange {
        #[cfg(test)]
        {
            self.refresh_frame_calls = self.refresh_frame_calls.saturating_add(1);
        }
        let composition_before = DisplayCompositionSnapshot::capture(self);
        self.poll_product_gpu_timings();
        let total_start = Instant::now();
        let completed_work = match self.rerender_coordinated_display_state() {
            Ok(work) => Some(work),
            Err(error) => {
                tracing::error!(%error, "GPU display refresh failed");
                self.record_product_render_failure(&error);
                None
            }
        };
        if let Some(work) = completed_work {
            self.record_display_refresh_timing(
                work.render,
                work.visible_brick_request_ms,
                duration_ms(total_start.elapsed()),
            );
        }
        DisplayCompositionSnapshot::capture(self).change_from(&composition_before)
    }

    pub(crate) fn poll_product_gpu_timings(&mut self) {
        let Some(pending_count) = self
            .native_presentation
            .product_gpu
            .as_ref()
            .map(|product| product.pending_gpu_timings.len())
        else {
            return;
        };
        for _ in 0..pending_count {
            let outcome = {
                let product = self
                    .native_presentation
                    .product_gpu
                    .as_mut()
                    .expect("the timing runtime remains installed during one bounded poll");
                let Some(ticket) = product.pending_gpu_timings.front().copied() else {
                    break;
                };
                (ticket, product.renderer.poll_gpu_timing(ticket))
            };
            match outcome {
                (ticket, Ok(Some(timing))) => {
                    self.native_presentation
                        .product_gpu
                        .as_mut()
                        .expect("the timing runtime remains installed")
                        .pending_gpu_timings
                        .pop_front();
                    debug_assert_eq!(timing.ticket(), ticket);
                    if self.volume_presentation.observe_timing(timing) {
                        self.render_coordination.request_refresh();
                    }
                    real_interaction_trace::record_gpu_timing(timing);
                }
                (_, Ok(None)) => break,
                (_, Err(error)) => {
                    let ticket = self
                        .native_presentation
                        .product_gpu
                        .as_mut()
                        .expect("the timing runtime remains installed")
                        .pending_gpu_timings
                        .pop_front()
                        .expect("the failed front timing remains pending");
                    self.volume_presentation.discard_timing(ticket);
                    if classify_color_runtime_error(error)
                        == ColorRuntimeDisposition::RendererTerminal
                    {
                        self.enter_product_renderer_terminal(error);
                        tracing::error!(%error, "GPU timing observed the terminal renderer cause");
                        break;
                    }
                    tracing::warn!(%error, "adaptive GPU timing could not be collected");
                }
            }
        }
    }

    pub(crate) fn refresh_texture_only(&mut self) -> DisplayCompositionChange {
        self.invalidate_cross_section_panel_display_frames();
        self.refresh_frame()
    }

    fn record_product_render_failure(&mut self, error: &anyhow::Error) {
        let failure = render_state::render_failure_status(error);
        self.render_coordination.frame_fidelity.last_failure_kind = Some(failure.kind());
        self.render_coordination.frame_fidelity.last_capacity_error =
            Some(failure.message().to_owned());
    }
}

fn build_product_intent(
    snapshot: &ApplicationSnapshot,
    frame: FrameIdentity,
    cross_section: Option<PanelId>,
    cross_section_view: Option<CrossSectionView>,
    presentation: PresentationViewport,
    extent: RenderExtent,
    camera_override: Option<CameraView>,
) -> anyhow::Result<Option<mirante4d_render_api::RenderIntent>> {
    match cross_section {
        Some(panel) => cross_section_intent(
            snapshot,
            frame,
            panel,
            cross_section_view.ok_or_else(|| {
                anyhow::anyhow!("a cross-section target requires explicit plane geometry")
            })?,
            presentation,
            extent,
        ),
        None => volume_intent(snapshot, frame, presentation, extent, camera_override),
    }
}

fn product_layer_presentation_status(
    layer_ordinal: u32,
    expected_scale_level: Option<u32>,
    presented_facts: Option<ProductLayerRequirementFacts>,
    panel_current: bool,
) -> mirante4d_application::LayerPresentationStatus {
    let displayed_scale_level = presented_facts.and_then(|facts| facts.displayed_scale_level);
    let target_scale_level = presented_facts.and_then(|facts| facts.target_scale_level);
    let mixed = presented_facts.is_some_and(|facts| facts.mixed);
    mirante4d_application::LayerPresentationStatus {
        layer_ordinal,
        expected_scale_level,
        displayed_scale_level,
        target_scale_level,
        finest_fallback_scale_level: presented_facts
            .and_then(|facts| facts.finest_fallback_scale_level),
        fallback_scale_level: presented_facts.and_then(|facts| facts.fallback_scale_level),
        target_available_requirements: presented_facts
            .map_or(0, |facts| facts.target_available_requirements),
        target_total_requirements: presented_facts
            .map_or(0, |facts| facts.target_total_requirements),
        available_requirements: presented_facts.map_or(0, |facts| facts.available_requirements),
        total_requirements: presented_facts.map_or(0, |facts| facts.total_requirements),
        mixed,
        current: panel_current
            && !mixed
            && expected_scale_level == displayed_scale_level
            && expected_scale_level == target_scale_level,
    }
}

fn cross_section_scope(panel_id: PanelId) -> anyhow::Result<u64> {
    match panel_id {
        PanelId::Xy => Ok(SCOPE_CROSS_SECTION_XY),
        PanelId::Xz => Ok(SCOPE_CROSS_SECTION_XZ),
        PanelId::Yz => Ok(SCOPE_CROSS_SECTION_YZ),
        PanelId::ThreeD => anyhow::bail!("the 3D panel has no cross-section demand scope"),
    }
}

fn uniform_presented_scale(coverage: &mirante4d_render_api::FrameCoverage) -> Option<u32> {
    let mut scales = coverage
        .layer_coverages()
        .map(|layer| layer.scale().map(mirante4d_domain::ScaleLevel::get));
    let first = scales.next()??;
    scales.all(|scale| scale == Some(first)).then_some(first)
}

pub(crate) fn presented_layer_scales_match_target(
    coverage: &mirante4d_render_api::FrameCoverage,
    target: &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
) -> bool {
    layer_scale_pairs_match_target(
        coverage
            .layer_coverages()
            .map(|layer| (layer.layer(), layer.scale())),
        target,
    )
}

fn layer_scale_pairs_match_target(
    mut layers: impl ExactSizeIterator<
        Item = (
            mirante4d_domain::LogicalLayerKey,
            Option<mirante4d_domain::ScaleLevel>,
        ),
    >,
    target: &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
) -> bool {
    layers.len() == target.len()
        && layers.all(|(layer, scale)| target.get(&layer).copied() == scale)
}

fn presented_layer_scales_are_coarser_than_target(
    coverage: &mirante4d_render_api::FrameCoverage,
    target: &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
) -> bool {
    layer_scale_pairs_are_coarser_than_target(
        coverage
            .layer_coverages()
            .map(|layer| (layer.layer(), layer.scale())),
        target,
    )
}

fn layer_scale_pairs_are_coarser_than_target(
    layers: impl ExactSizeIterator<
        Item = (
            mirante4d_domain::LogicalLayerKey,
            Option<mirante4d_domain::ScaleLevel>,
        ),
    >,
    target: &BTreeMap<mirante4d_domain::LogicalLayerKey, mirante4d_domain::ScaleLevel>,
) -> bool {
    if layers.len() != target.len() {
        return false;
    }
    let mut coarser = false;
    for (layer, displayed) in layers {
        let Some(target) = target.get(&layer).copied() else {
            return false;
        };
        let Some(displayed) = displayed else {
            return false;
        };
        if displayed < target {
            return false;
        }
        coarser |= displayed > target;
    }
    coarser
}

fn render_backend_for_mode(mode: RenderMode) -> RenderBackend {
    match mode {
        RenderMode::Mip => RenderBackend::GpuCameraMip,
        RenderMode::Isosurface => RenderBackend::GpuCameraIso,
        RenderMode::Dvr => RenderBackend::GpuCameraDvr,
    }
}

/// Reports the shader family selected by the ordered visible layer set.
/// Durable analysis focus is deliberately irrelevant and may be hidden.
pub(crate) fn render_backend_for_view(view: &ViewState) -> RenderBackend {
    let mut visible_modes = view
        .layers()
        .iter()
        .filter(|layer| layer.visible())
        .map(|layer| layer.render_state().mode());
    let Some(first) = visible_modes.next() else {
        return RenderBackend::Empty;
    };
    if visible_modes.all(|mode| mode == first) {
        render_backend_for_mode(first)
    } else {
        RenderBackend::GpuCameraMixed
    }
}

#[cfg(test)]
mod requirement_lease_update_tests {
    use super::*;

    fn blank_composition_snapshot() -> DisplayCompositionSnapshot {
        DisplayCompositionSnapshot {
            targets: PresentationTarget::ALL.map(|target| DisplayTargetCompositionSnapshot {
                active: true,
                frame: None,
                texture_binding: None,
                background: if target == PresentationTarget::ThreeD {
                    DisplayCompositionBackground::ThreeD(RenderBackend::Loading)
                } else {
                    DisplayCompositionBackground::CrossSection {
                        active: true,
                        schedule: Some(CrossSectionPanelScheduleState::missing_viewport(0)),
                    }
                },
            }),
        }
    }

    #[test]
    fn composition_change_distinguishes_texture_surface_mixed_and_quiescent_states() {
        let extent = RenderExtent::new(64, 64).unwrap();
        let before = blank_composition_snapshot();

        let mut texture = before.clone();
        texture.targets[PresentationTarget::ThreeD.index()].frame = Some(
            crate::tests::synthetic_presented_frame(PresentationTarget::ThreeD, extent),
        );
        texture.targets[PresentationTarget::ThreeD.index()].texture_binding = Some((1, 1));
        assert_eq!(
            texture.change_from(&before),
            DisplayCompositionChange::TexturePublished
        );

        let mut cleared = texture.clone();
        cleared.targets[PresentationTarget::ThreeD.index()].frame = None;
        cleared.targets[PresentationTarget::ThreeD.index()].texture_binding = None;
        cleared.targets[PresentationTarget::ThreeD.index()].background =
            DisplayCompositionBackground::ThreeD(RenderBackend::Empty);
        assert_eq!(
            cleared.change_from(&texture),
            DisplayCompositionChange::SurfaceCleared
        );
        assert_eq!(
            cleared.change_from(&cleared),
            DisplayCompositionChange::Unchanged
        );

        let mut mixed_before = blank_composition_snapshot();
        mixed_before.targets[PresentationTarget::Xy.index()].frame = Some(
            crate::tests::synthetic_presented_frame(PresentationTarget::Xy, extent),
        );
        mixed_before.targets[PresentationTarget::Xy.index()].texture_binding = Some((1, 1));
        let mut mixed_after = mixed_before.clone();
        mixed_after.targets[PresentationTarget::ThreeD.index()].frame = Some(
            crate::tests::synthetic_presented_frame(PresentationTarget::ThreeD, extent),
        );
        mixed_after.targets[PresentationTarget::ThreeD.index()].texture_binding = Some((1, 2));
        mixed_after.targets[PresentationTarget::Xy.index()].frame = None;
        mixed_after.targets[PresentationTarget::Xy.index()].texture_binding = None;
        assert_eq!(
            mixed_after.change_from(&mixed_before),
            DisplayCompositionChange::TexturePublishedAndSurfaceCleared
        );

        assert!(DisplayCompositionChange::TexturePublished.requires_composition_turn());
        assert!(DisplayCompositionChange::SurfaceCleared.requires_composition_turn());
        assert!(!DisplayCompositionChange::Unchanged.requires_composition_turn());
    }

    fn backend_test_view(first_visible: bool, second_visible: bool) -> ViewState {
        let transfer = || {
            mirante4d_domain::LayerTransfer::new(
                mirante4d_domain::DisplayWindow::new(0.0, 1.0).unwrap(),
                mirante4d_domain::RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                mirante4d_domain::Opacity::new(1.0).unwrap(),
                mirante4d_domain::TransferCurve::linear(),
                false,
            )
        };
        let dvr = mirante4d_domain::RenderState::dvr(
            mirante4d_domain::SamplingPolicy::VoxelExact,
            mirante4d_domain::DvrOpacityTransfer::new(
                mirante4d_domain::DisplayWindow::new(0.0, 1.0).unwrap(),
                mirante4d_domain::TransferCurve::linear(),
            ),
            1.0,
        )
        .unwrap();
        ViewState::new(
            vec![
                mirante4d_project_model::LayerViewState::new(
                    mirante4d_domain::LogicalLayerKey::new(0),
                    first_visible,
                    transfer(),
                    mirante4d_domain::RenderState::mip(
                        mirante4d_domain::SamplingPolicy::VoxelExact,
                    ),
                ),
                mirante4d_project_model::LayerViewState::new(
                    mirante4d_domain::LogicalLayerKey::new(1),
                    second_visible,
                    transfer(),
                    dvr,
                ),
            ],
            mirante4d_domain::LogicalLayerKey::new(0),
            mirante4d_domain::TimeIndex::new(0),
            mirante4d_domain::CameraView::new(
                mirante4d_domain::Projection::Orthographic,
                mirante4d_domain::WorldPoint3::origin(),
                mirante4d_domain::UnitQuaternion::identity(),
                1.0,
                100.0,
                100.0,
            )
            .unwrap(),
            mirante4d_domain::ViewerLayout::Single3d,
            mirante4d_domain::CrossSectionView::new(
                mirante4d_domain::WorldPoint3::origin(),
                mirante4d_domain::UnitQuaternion::identity(),
                1.0,
                1.0,
            )
            .unwrap(),
            mirante4d_domain::IsoLightState::attached_camera(),
        )
        .unwrap()
    }

    #[test]
    fn backend_classification_uses_visible_modes_not_hidden_analysis_focus() {
        assert_eq!(
            render_backend_for_view(&backend_test_view(false, true)),
            RenderBackend::GpuCameraDvr
        );
        assert_eq!(
            render_backend_for_view(&backend_test_view(true, true)),
            RenderBackend::GpuCameraMixed
        );
        assert_eq!(
            render_backend_for_view(&backend_test_view(false, false)),
            RenderBackend::Empty
        );
    }

    #[test]
    fn heterogeneous_layer_scale_maps_compare_without_a_scalar_alias() {
        let target = BTreeMap::from([
            (
                mirante4d_domain::LogicalLayerKey::new(3),
                mirante4d_domain::ScaleLevel::new(0),
            ),
            (
                mirante4d_domain::LogicalLayerKey::new(7),
                mirante4d_domain::ScaleLevel::new(2),
            ),
        ]);
        let exact = [
            (
                mirante4d_domain::LogicalLayerKey::new(3),
                Some(mirante4d_domain::ScaleLevel::new(0)),
            ),
            (
                mirante4d_domain::LogicalLayerKey::new(7),
                Some(mirante4d_domain::ScaleLevel::new(2)),
            ),
        ];
        assert!(layer_scale_pairs_match_target(exact.into_iter(), &target));
        assert!(!layer_scale_pairs_are_coarser_than_target(
            exact.into_iter(),
            &target
        ));

        let coarse = [
            (
                mirante4d_domain::LogicalLayerKey::new(3),
                Some(mirante4d_domain::ScaleLevel::new(1)),
            ),
            (
                mirante4d_domain::LogicalLayerKey::new(7),
                Some(mirante4d_domain::ScaleLevel::new(2)),
            ),
        ];
        assert!(!layer_scale_pairs_match_target(coarse.into_iter(), &target));
        assert!(layer_scale_pairs_are_coarser_than_target(
            coarse.into_iter(),
            &target
        ));
        assert!(!layer_scale_pairs_match_target(
            exact[..1].iter().copied(),
            &target
        ));
        let extra = [
            exact[0],
            exact[1],
            (
                mirante4d_domain::LogicalLayerKey::new(9),
                Some(mirante4d_domain::ScaleLevel::new(0)),
            ),
        ];
        assert!(!layer_scale_pairs_match_target(extra.into_iter(), &target));
        assert!(!layer_scale_pairs_match_target(
            extra.into_iter(),
            &BTreeMap::new()
        ));
    }

    #[test]
    fn layer_evidence_uses_the_presented_resource_scale() {
        let status = product_layer_presentation_status(
            7,
            Some(1),
            Some(ProductLayerRequirementFacts {
                displayed_scale_level: Some(0),
                target_scale_level: Some(0),
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 3,
                target_total_requirements: 4,
                available_requirements: 3,
                total_requirements: 4,
                mixed: false,
            }),
            true,
        );
        assert_eq!(status.layer_ordinal, 7);
        assert_eq!(status.expected_scale_level, Some(1));
        assert_eq!(status.displayed_scale_level, Some(0));
        assert_eq!(status.target_scale_level, Some(0));
        assert_eq!(status.finest_fallback_scale_level, Some(1));
        assert_eq!(status.fallback_scale_level, Some(3));
        assert_eq!(status.target_available_requirements, 3);
        assert_eq!(status.target_total_requirements, 4);
        assert_eq!(status.available_requirements, 3);
        assert_eq!(status.total_requirements, 4);
        assert!(!status.current);
    }

    #[test]
    fn mixed_layer_evidence_never_claims_one_shown_scale_or_exact_currentness() {
        let status = product_layer_presentation_status(
            7,
            Some(0),
            Some(ProductLayerRequirementFacts {
                displayed_scale_level: None,
                target_scale_level: Some(0),
                finest_fallback_scale_level: Some(1),
                fallback_scale_level: Some(3),
                target_available_requirements: 7,
                target_total_requirements: 20,
                available_requirements: 8,
                total_requirements: 21,
                mixed: true,
            }),
            true,
        );
        assert_eq!(status.displayed_scale_level, None);
        assert_eq!(status.target_scale_level, Some(0));
        assert_eq!(status.finest_fallback_scale_level, Some(1));
        assert_eq!(status.fallback_scale_level, Some(3));
        assert!(status.mixed);
        assert!(!status.current);
    }

    #[test]
    fn coordinated_milestones_require_the_visible_layout_and_foreground_idle() {
        let mut milestones = DisplayPerformanceMilestones::default();
        milestones.begin_generation(1);

        milestones.observe_coordinated_layout(true, true, true, false, false);
        assert!(milestones.first_useful_frame_ms().is_some());
        assert!(milestones.complete_coarse_ms().is_some());
        assert!(milestones.complete_replacement_ms().is_some());
        assert!(milestones.first_current_presented_ms().is_none());
        assert!(milestones.target_settled_ms().is_none());

        milestones.observe_coordinated_layout(true, true, false, true, false);
        assert!(milestones.first_current_presented_ms().is_some());
        assert!(milestones.target_settled_ms().is_none());

        milestones.observe_coordinated_layout(true, true, false, true, true);
        assert!(milestones.target_settled_ms().is_some());

        let exact_idle = PerformanceMilestoneObservation {
            first_useful: true,
            complete_replacement: true,
            complete_coarse: true,
            target_current: true,
            foreground_idle: true,
        };
        milestones.observe_panel(PresentationSlot::Xy, exact_idle);
        milestones.observe_visible_layer(PresentationSlot::Xy, 17, exact_idle);
        let xy = milestones.panel(PresentationSlot::Xy);
        assert!(xy.first_useful_frame_ms().is_some());
        assert!(xy.target_settled_ms().is_some());
        let layer = xy
            .visible_layers()
            .iter()
            .find(|layer| layer.layer_ordinal() == Some(17))
            .expect("the visible layer has generation-bound milestones");
        assert!(layer.complete_coarse_ms().is_some());
        assert!(layer.target_settled_ms().is_some());
    }

    fn render_failure_signature(
        frame: u64,
        dataset_runtime_epoch: u64,
    ) -> RenderAttemptFingerprint {
        RenderAttemptFingerprint {
            source_generation: SourceSessionGeneration::new(1),
            layout: CanonicalViewerLayout::Single3d,
            frames: [Some(FrameIdentity::new(frame)), None, None, None],
            timepoints: [Some(TimeIndex::new(0)), None, None, None],
            requirements: std::array::from_fn(|_| None),
            surface_generations: [Some(1), None, None, None],
            extents: [None; 4],
            layer_scales: std::array::from_fn(|_| None),
            schedules: [Some(VolumeColorSchedule::Direct), None, None, None],
            dataset_runtime_epoch,
            cpu_capacity_epoch: 1,
            renderer_device_generation: 1,
        }
    }

    fn fingerprint_requirement_fixtures()
    -> (RenderRequirements, RenderRequirements, RenderRequirements) {
        use mirante4d_dataset::{
            BrickKey, DatasetResourceIdentity, DatasetSourceId, ResourceRegion,
        };
        use mirante4d_domain::{
            DisplayWindow, LayerTransfer, Opacity, RgbColor, SamplingPolicy, Shape3D,
            TransferCurve, UnitQuaternion, WorldPoint3,
        };
        use mirante4d_render_api::{
            LayerRenderIntent, PreparedResourceBody, PresentationViewport, RenderIntent,
        };

        let identity = DatasetResourceIdentity::SessionLocal(DatasetSourceId::new(97));
        let layer = LogicalLayerKey::new(3);
        let timepoint = TimeIndex::new(0);
        let intent = RenderIntent::new(
            FrameIdentity::new(7),
            identity,
            timepoint,
            RenderViewIntent::cross_section(
                CrossSectionView::new(WorldPoint3::origin(), UnitQuaternion::identity(), 1.0, 1.0)
                    .unwrap(),
            ),
            PresentationViewport::new(64.0, 64.0).unwrap(),
            RenderExtent::new(64, 64).unwrap(),
            vec![LayerRenderIntent::new(
                layer,
                LayerTransfer::new(
                    DisplayWindow::new(0.0, 1.0).unwrap(),
                    RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                    Opacity::new(1.0).unwrap(),
                    TransferCurve::linear(),
                    false,
                ),
                mirante4d_domain::RenderState::mip(SamplingPolicy::VoxelExact),
            )],
        )
        .unwrap();
        let key = |origin_x| {
            BrickKey::new(
                identity,
                layer,
                timepoint,
                ScaleLevel::BASE,
                ResourceRegion::new([0, 0, origin_x], Shape3D::new(1, 1, 1).unwrap()).unwrap(),
            )
        };
        let prepared = |suffix_origin| {
            let required = key(0);
            let suffix = key(suffix_origin);
            let mut canonical = vec![required, suffix];
            canonical.sort_unstable();
            let body =
                PreparedResourceBody::new(canonical.into(), vec![required, suffix].into(), None)
                    .unwrap();
            PreparedRenderRequirements::new_with_required_prefix(
                identity,
                timepoint,
                vec![layer],
                body,
                1,
                1,
            )
            .unwrap()
        };
        let first = prepared(1);
        let changed = prepared(2);
        (
            first.bind(&intent).unwrap(),
            changed.bind(&intent).unwrap(),
            first.promote_prefetch().bind(&intent).unwrap(),
        )
    }

    fn complete_render_signature(requirements: RenderRequirements) -> RenderAttemptFingerprint {
        let mut signature = render_failure_signature(7, 17);
        signature.requirements[PresentationTarget::ThreeD.index()] = Some(requirements);
        signature.extents[PresentationTarget::ThreeD.index()] =
            Some(RenderExtent::new(64, 64).unwrap());
        signature.layer_scales[PresentationTarget::ThreeD.index()] = Some(Arc::new(
            BTreeMap::from([(LogicalLayerKey::new(3), ScaleLevel::BASE)]),
        ));
        signature
    }

    #[test]
    fn composed_same_frame_successor_body_cannot_reuse_predecessor() {
        let (predecessor_body, successor_body, _) = fingerprint_requirement_fixtures();
        assert!(!predecessor_body.shares_resources_with(&successor_body));
        let predecessor = complete_render_signature(predecessor_body);
        let successor = complete_render_signature(successor_body);
        assert!(!predecessor.matches_current(&successor));

        let predecessor_fidelity = RenderFrameCompleteness::Exact;
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.fail(
            predecessor,
            ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "old body"),
        );
        assert!(coordinator.begin(successor));
        assert_eq!(predecessor_fidelity, RenderFrameCompleteness::Exact);
        assert!(matches!(
            coordinator.state(),
            RenderAttemptState::Ready { .. }
        ));
    }

    #[test]
    fn composed_prefetch_role_change_cannot_reuse_predecessor() {
        let (unpromoted, _, promoted) = fingerprint_requirement_fixtures();
        assert!(unpromoted.shares_resources_with(&promoted));
        assert_ne!(unpromoted.prefetch_promoted(), promoted.prefetch_promoted());
        let predecessor = complete_render_signature(unpromoted);
        let successor = complete_render_signature(promoted);
        assert!(!predecessor.matches_current(&successor));

        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.fail(
            predecessor,
            ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "old role"),
        );
        assert!(coordinator.begin(successor));
    }

    #[test]
    fn composed_source_time_scale_spatial_extent_or_surface_mismatch_never_reuses() {
        let (requirements, changed_body, _) = fingerprint_requirement_fixtures();
        let baseline = complete_render_signature(requirements);
        let mut variants = Vec::new();

        let mut source = baseline.clone();
        source.source_generation = SourceSessionGeneration::new(2);
        variants.push(source);
        let mut time = baseline.clone();
        time.timepoints[0] = Some(TimeIndex::new(1));
        variants.push(time);
        let mut spatial = baseline.clone();
        spatial.frames[0] = Some(FrameIdentity::new(8));
        variants.push(spatial);
        let mut surface = baseline.clone();
        surface.surface_generations[0] = Some(2);
        variants.push(surface);
        let mut extent = baseline.clone();
        extent.extents[0] = Some(RenderExtent::new(65, 64).unwrap());
        variants.push(extent);
        let mut scale = baseline.clone();
        scale.layer_scales[0] = Some(Arc::new(BTreeMap::from([(
            LogicalLayerKey::new(3),
            ScaleLevel::new(1),
        )])));
        variants.push(scale);
        let mut body = baseline.clone();
        body.requirements[0] = Some(changed_body);
        variants.push(body);

        for variant in variants {
            assert!(!baseline.matches_current(&variant));
        }
    }

    #[test]
    fn latched_failure_is_quiescent_for_128_unchanged_ui_turns() {
        let current = render_failure_signature(7, 17);
        let mut coordinator = RenderAttemptCoordinator::default();
        assert!(coordinator.begin(current.clone()));
        coordinator.fail(
            current.clone(),
            ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "capacity"),
        );
        for _ in 0..128 {
            assert!(!coordinator.begin(current.clone()));
            assert_eq!(coordinator.wake(), RenderWake::None);
        }

        let changed = [
            render_failure_signature(8, 17),
            render_failure_signature(7, 18),
        ];
        for signature in changed {
            assert!(coordinator.begin(signature));
            coordinator.fail(
                current.clone(),
                ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "capacity"),
            );
        }
    }

    #[test]
    fn published_texture_revision_requests_exactly_one_composition_turn() {
        let mut coordinator = RenderAttemptCoordinator::default();
        let target = PresentationTarget::ThreeD;

        coordinator.note_published_texture(target, 3, 7);
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        coordinator.acknowledge_composition(target, Some((3, 7)));
        assert_eq!(coordinator.wake(), RenderWake::None);

        coordinator.note_published_texture(target, 3, 7);
        coordinator.note_published_texture(target, 3, 6);
        assert_eq!(
            coordinator.wake(),
            RenderWake::None,
            "an acknowledged or stale texture cannot manufacture another composition turn"
        );

        coordinator.note_published_texture(target, 3, 8);
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        coordinator.acknowledge_composition(target, Some((3, 7)));
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        coordinator.acknowledge_composition(target, Some((3, 8)));
        assert_eq!(coordinator.wake(), RenderWake::None);
    }

    #[test]
    fn render_fingerprint_change_reopens_exactly_one_attempt() {
        let (requirements, changed_body, promoted) = fingerprint_requirement_fixtures();
        let current = complete_render_signature(requirements);
        let mut changed = Vec::new();
        let mut edit = current.clone();
        edit.source_generation = SourceSessionGeneration::new(2);
        changed.push(edit);
        let mut edit = current.clone();
        edit.layout = CanonicalViewerLayout::FourPanel;
        changed.push(edit);
        let mut edit = current.clone();
        edit.frames[0] = Some(FrameIdentity::new(8));
        changed.push(edit);
        let mut edit = current.clone();
        edit.timepoints[0] = Some(TimeIndex::new(1));
        changed.push(edit);
        for replacement in [changed_body, promoted] {
            let mut edit = current.clone();
            edit.requirements[0] = Some(replacement);
            changed.push(edit);
        }
        let mut edit = current.clone();
        edit.surface_generations[0] = Some(2);
        changed.push(edit);
        let mut edit = current.clone();
        edit.extents[0] = Some(RenderExtent::new(65, 64).unwrap());
        changed.push(edit);
        let mut edit = current.clone();
        edit.layer_scales[0] = Some(Arc::new(BTreeMap::from([(
            LogicalLayerKey::new(3),
            ScaleLevel::new(1),
        )])));
        changed.push(edit);
        let mut edit = current.clone();
        edit.schedules[0] = Some(VolumeColorSchedule::InteractivePreview);
        changed.push(edit);
        for (runtime, cpu, renderer) in [(18, 1, 1), (17, 2, 1), (17, 1, 2)] {
            let mut edit = current.clone();
            edit.dataset_runtime_epoch = runtime;
            edit.cpu_capacity_epoch = cpu;
            edit.renderer_device_generation = renderer;
            changed.push(edit);
        }

        for changed in changed {
            let mut coordinator = RenderAttemptCoordinator::default();
            coordinator.fail(
                current.clone(),
                ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "capacity"),
            );
            assert!(coordinator.begin(changed.clone()));
            coordinator.fail(
                changed.clone(),
                ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "capacity"),
            );
            assert!(!coordinator.begin(changed));
            assert_eq!(coordinator.execution_decisions, 1);
        }
    }

    #[test]
    fn actionable_report_authorizes_exactly_one_same_fingerprint_retry() {
        let current = render_failure_signature(7, 17);
        let target = PresentationTarget::ThreeD;
        let mut coordinator = RenderAttemptCoordinator::default();

        assert!(coordinator.begin(current.clone()));
        assert!(!coordinator.target_ready(target));
        assert!(!coordinator.begin(current.clone()));

        coordinator.continue_ready(&current);
        assert!(coordinator.target_ready(target));
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        assert!(coordinator.begin(current.clone()));

        assert!(!coordinator.target_ready(target));
        assert_eq!(coordinator.wake(), RenderWake::None);
        assert!(!coordinator.begin(current));
        assert_eq!(coordinator.execution_decisions, 2);
    }

    fn renderer_event_batch(
        bits: u8,
        renderer_device_generation: u64,
        completed_submission: u64,
        hidden_worker_job: u64,
    ) -> RendererEventBatch {
        RendererEventBatch {
            renderer_device_generation,
            bits,
            completed_submission,
            hidden_worker_job,
        }
    }

    #[test]
    fn renderer_event_sink_coalesces_keys_and_ignores_late_callbacks_after_drop() {
        let wake = Arc::new(RendererUiWake::new(egui::Context::default(), 1));
        let weak = Arc::downgrade(&wake);
        let sink = mirante4d_render_wgpu::RendererEventSink::new(move |event| {
            if let Some(wake) = weak.upgrade() {
                wake.wake_renderer_event(event);
            }
        });

        sink.wake(RendererEvent::SubmissionCompleted { submission: 7 });
        sink.wake(RendererEvent::SubmissionCompleted { submission: 5 });
        sink.wake(RendererEvent::HiddenWorkerResult { job: 11 });
        sink.wake(RendererEvent::HiddenWorkerResult { job: 9 });
        assert!(wake.ui_turn_pending.load(Ordering::Acquire));
        let batch = wake.begin_ui_turn();
        assert_eq!(batch.submission_completed(), Some(7));
        assert_eq!(batch.hidden_worker_result(), Some(11));
        assert!(!wake.ui_turn_pending.load(Ordering::Acquire));

        sink.wake(RendererEvent::PipelineCapabilityChanged(
            PipelineCapability::InitialRender,
        ));
        sink.wake(RendererEvent::PipelineCapabilityChanged(
            PipelineCapability::Pick,
        ));
        let initial = wake.begin_ui_turn();
        assert!(initial.initial_pipeline_changed());
        assert!(!initial.pick_pipeline_changed());
        assert!(wake.ui_turn_pending.load(Ordering::Acquire));
        let pick = wake.begin_ui_turn();
        assert!(!pick.initial_pipeline_changed());
        assert!(pick.pick_pipeline_changed());
        assert!(!wake.ui_turn_pending.load(Ordering::Acquire));

        let fingerprint = render_failure_signature(7, 17);
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(
            fingerprint.clone(),
            RenderWaitKey::SubmissionCleanup { submission: 8 },
        );
        coordinator.observe_renderer_events(batch);
        assert!(!coordinator.begin(fingerprint.clone()));

        // This commit occurs after the handler cleared the coalescing flag.
        // It must therefore own exactly one successor UI turn.
        sink.wake(RendererEvent::SubmissionCompleted { submission: 8 });
        assert!(wake.ui_turn_pending.load(Ordering::Acquire));
        let successor = wake.begin_ui_turn();
        coordinator.observe_renderer_events(successor);
        assert!(coordinator.begin(fingerprint));
        assert!(!wake.ui_turn_pending.load(Ordering::Acquire));

        wake.close();
        let weak = Arc::downgrade(&wake);
        drop(wake);
        assert!(weak.upgrade().is_none());
        sink.wake(RendererEvent::HiddenWorkerResult { job: 12 });
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn payload_recovery_completion_causes_one_retry_without_hot_polling() {
        let current = render_failure_signature(7, 17);
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(
            current.clone(),
            RenderWaitKey::SubmissionCleanup { submission: 9 },
        );
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));

        coordinator.observe_renderer_events(renderer_event_batch(
            SUBMISSION_COMPLETED_EVENT,
            1,
            8,
            0,
        ));
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));
        assert!(!coordinator.begin(current.clone()));

        coordinator.observe_renderer_events(renderer_event_batch(
            SUBMISSION_COMPLETED_EVENT,
            1,
            9,
            0,
        ));
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        assert!(coordinator.begin(current));
        assert_eq!(coordinator.execution_decisions, 1);
    }

    #[test]
    fn eviction_acknowledgement_causes_one_retry_without_signature_change() {
        let current = render_failure_signature(7, 17);
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(
            current.clone(),
            RenderWaitKey::EvictionAcknowledgement {
                ledger_revision: 41,
            },
        );
        coordinator.observe_eviction_acknowledgement(40);
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));
        assert!(!coordinator.begin(current.clone()));

        coordinator.observe_eviction_acknowledgement(41);
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        assert!(coordinator.begin(current));
        assert_eq!(coordinator.execution_decisions, 1);
    }

    #[test]
    fn render_wait_priority_registers_one_keyed_causal_event() {
        let current = render_failure_signature(7, 17);
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(
            current.clone(),
            RenderWaitKey::InitialPipeline {
                device_generation: 3,
            },
        );
        coordinator.observe_renderer_events(renderer_event_batch(PICK_PIPELINE_EVENT, 3, 0, 0));
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));
        coordinator.observe_renderer_events(renderer_event_batch(INITIAL_PIPELINE_EVENT, 2, 0, 0));
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));
        coordinator.observe_renderer_events(renderer_event_batch(INITIAL_PIPELINE_EVENT, 3, 0, 0));
        assert_eq!(coordinator.wake(), RenderWake::Immediate);

        coordinator.wait(
            current,
            RenderWaitKey::HiddenWorker {
                target: PresentationTarget::ThreeD,
                job: 17,
            },
        );
        coordinator.observe_renderer_events(renderer_event_batch(HIDDEN_WORKER_EVENT, 3, 0, 16));
        assert!(matches!(coordinator.wake(), RenderWake::Waiting(_)));
        coordinator.observe_renderer_events(renderer_event_batch(HIDDEN_WORKER_EVENT, 3, 0, 17));
        assert_eq!(coordinator.wake(), RenderWake::Immediate);
        assert_eq!(coordinator.wait_decisions, 2);
    }

    #[test]
    fn renderer_outcomes_have_exhaustive_attempt_dispositions() {
        use mirante4d_render_api::GpuLedgerCategory;

        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::BackendValidation),
            ColorRuntimeDisposition::RendererTerminal
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::StaleFrame {
                target: Some(PresentationTarget::ThreeD),
                actual: FrameIdentity::new(1),
                current: FrameIdentity::new(2),
            }),
            ColorRuntimeDisposition::Stale
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::CapacityExceeded {
                category: GpuLedgerCategory::PayloadResidency,
                requested_bytes: 2,
                available_bytes: 1,
            }),
            ColorRuntimeDisposition::DeterministicFailure
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::RequirementCapacityExceeded {
                actual: 2,
                maximum: 1,
            }),
            ColorRuntimeDisposition::DeterministicFailure
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::PickBackpressure),
            ColorRuntimeDisposition::AuxiliaryOnly
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::PayloadPlacementUnavailable {
                requested_bytes: 256,
                total_free_bytes: 512,
                largest_contiguous_bytes: 128,
            }),
            ColorRuntimeDisposition::PlacementRecovery
        );
        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::PayloadRecoveryDeferred),
            ColorRuntimeDisposition::Wait(RenderWaitReason::SubmissionCleanup)
        );
    }

    #[test]
    fn retryable_renderer_outcomes_preserve_presented_fidelity_and_use_neutral_status() {
        let exact_pixels = RenderFrameCompleteness::Exact;
        let current = render_failure_signature(7, 17);
        let retryable = [
            (
                WgpuRenderRuntimeError::PipelineNotReady {
                    capability: PipelineCapability::InitialRender,
                },
                RenderWaitKey::InitialPipeline {
                    device_generation: 1,
                },
            ),
            (
                WgpuRenderRuntimeError::PayloadRecoveryDeferred,
                RenderWaitKey::SubmissionCleanup { submission: 5 },
            ),
            (
                WgpuRenderRuntimeError::ResidencyEvictionEventCapacityExceeded {
                    actual: 2,
                    maximum: 1,
                },
                RenderWaitKey::EvictionAcknowledgement { ledger_revision: 9 },
            ),
        ];
        for (error, key) in retryable {
            assert!(matches!(
                classify_color_runtime_error(error),
                ColorRuntimeDisposition::Wait(_)
            ));
            let mut coordinator = RenderAttemptCoordinator::default();
            coordinator.wait(current.clone(), key);
            assert!(matches!(
                coordinator.state(),
                RenderAttemptState::Waiting { .. }
            ));
            assert!(coordinator.failure().is_none());
            assert_eq!(exact_pixels, RenderFrameCompleteness::Exact);
        }
    }

    #[test]
    fn non_error_backpressure_and_progress_are_normalized_before_report_application() {
        let facts = |presented, deferred, actionable, hidden_waiting| ColorAttemptTargetFacts {
            target: PresentationTarget::ThreeD,
            current: false,
            presented,
            deferred_by_backpressure: deferred,
            actionable_work_remaining: actionable,
            hidden_waiting,
            hidden_refinement: None,
        };

        let deferred = facts(false, true, false, false);
        assert!(matches!(
            classify_color_attempt_facts(&[deferred]),
            ColorAttemptClass::SubmissionWait
        ));
        assert!(!report_member_should_apply(
            deferred,
            CoordinatedMemberDisposition::Executed
        ));

        let missing_exact_residency = facts(false, false, false, false);
        assert!(matches!(
            classify_color_attempt_facts(&[missing_exact_residency]),
            ColorAttemptClass::RelevantResidency(PresentationTarget::ThreeD)
        ));
        assert!(!report_member_should_apply(
            missing_exact_residency,
            CoordinatedMemberDisposition::Executed
        ));

        let running_hidden = facts(false, false, false, true);
        assert!(matches!(
            classify_color_attempt_facts(&[running_hidden]),
            ColorAttemptClass::HiddenWait(PresentationTarget::ThreeD)
        ));

        let published_preview = facts(true, false, false, false);
        assert!(matches!(
            classify_color_attempt_facts(&[published_preview]),
            ColorAttemptClass::RelevantResidency(PresentationTarget::ThreeD)
        ));
        assert!(report_member_should_apply(
            published_preview,
            CoordinatedMemberDisposition::Executed
        ));

        let retry_ready = facts(false, false, true, false);
        assert!(matches!(
            classify_color_attempt_facts(&[retry_ready]),
            ColorAttemptClass::Ready
        ));
    }

    #[test]
    fn stale_frame_waits_for_reported_mailbox_advance_without_resubmission() {
        let target = PresentationTarget::Xy;
        let mut stale = render_failure_signature(7, 17);
        stale.frames[PresentationTarget::ThreeD.index()] = None;
        stale.frames[target.index()] = Some(FrameIdentity::new(7));
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(
            stale.clone(),
            RenderWaitKey::MailboxAdvance {
                target,
                minimum_frame: 9,
            },
        );

        let mut unrelated = stale.clone();
        unrelated.dataset_runtime_epoch = 18;
        assert!(!coordinator.begin(unrelated));
        let mut still_stale = stale.clone();
        still_stale.frames[target.index()] = Some(FrameIdentity::new(8));
        assert!(!coordinator.begin(still_stale));
        assert_eq!(coordinator.execution_decisions, 0);

        let mut current = stale;
        current.frames[target.index()] = Some(FrameIdentity::new(9));
        assert!(coordinator.begin(current.clone()));
        assert!(!coordinator.begin(current));
        assert_eq!(coordinator.execution_decisions, 1);
    }

    #[test]
    fn initial_render_failure_is_color_unavailable_across_fingerprint_changes() {
        let initial_failure = ResidentRenderFailureStatus::new(
            FrameFailureKind::BackendLimit,
            "initial color pipeline validation failed",
        );
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.color_unavailable(initial_failure);
        for changed in [
            render_failure_signature(7, 17),
            render_failure_signature(8, 18),
        ] {
            assert!(!coordinator.begin(changed));
            assert_eq!(coordinator.wake(), RenderWake::None);
        }
        assert!(matches!(
            coordinator.state(),
            RenderAttemptState::ColorUnavailable { .. }
        ));
        assert!(!coordinator.renderer_is_terminal());
        assert_eq!(coordinator.execution_decisions, 0);
    }

    #[test]
    fn viewer_render_failure_state_machine() {
        let fingerprint = render_failure_signature(7, 17);

        let mut retryable = RenderAttemptCoordinator::default();
        retryable.wait(
            fingerprint.clone(),
            RenderWaitKey::SubmissionCleanup { submission: 5 },
        );
        assert!(matches!(retryable.wake(), RenderWake::Waiting(_)));
        assert!(retryable.failure().is_none());

        let mut deterministic = RenderAttemptCoordinator::default();
        deterministic.fail(
            fingerprint.clone(),
            ResidentRenderFailureStatus::new(FrameFailureKind::BudgetExceeded, "capacity"),
        );
        assert!(!deterministic.begin(fingerprint.clone()));
        assert_eq!(deterministic.wake(), RenderWake::None);

        assert_eq!(
            classify_color_runtime_error(WgpuRenderRuntimeError::VolumePickFailed),
            ColorRuntimeDisposition::AuxiliaryOnly,
            "a Pick-only failure cannot enter color attempt state"
        );

        let hidden_timeout = ColorAttemptTargetFacts {
            target: PresentationTarget::ThreeD,
            current: false,
            presented: false,
            deferred_by_backpressure: false,
            actionable_work_remaining: false,
            hidden_waiting: false,
            hidden_refinement: Some(HiddenRefinementState::Failed(
                HiddenRefinementFailure::SubmissionTimedOutTwice,
            )),
        };
        assert!(matches!(
            classify_color_attempt_facts(&[hidden_timeout]),
            ColorAttemptClass::Failed(_)
        ));

        let mut terminal = RenderAttemptCoordinator::default();
        terminal.note_published_texture(PresentationTarget::ThreeD, 1, 9);
        terminal.renderer_terminal(ResidentRenderFailureStatus::new(
            FrameFailureKind::BackendLimit,
            "first device cause",
        ));
        assert_eq!(terminal.wake(), RenderWake::None);
        assert!(!terminal.begin(fingerprint));
        assert!(terminal.renderer_is_terminal());
    }

    #[test]
    fn eviction_event_backpressure_registers_one_causal_wait() {
        let current = render_failure_signature(7, 17);
        let key = RenderWaitKey::EvictionAcknowledgement {
            ledger_revision: 41,
        };
        let mut coordinator = RenderAttemptCoordinator::default();
        coordinator.wait(current, key.clone());
        assert_eq!(
            coordinator.wake(),
            RenderWake::Waiting(WaitingWake::Event(key))
        );
    }

    #[test]
    fn visible_target_retains_settled_pixels_until_exact_replacement() {
        for settled in [
            RenderFrameCompleteness::Complete,
            RenderFrameCompleteness::Exact,
        ] {
            assert_eq!(
                retained_frame_render_policy(true, Some(settled)),
                RetainedFrameRenderPolicy::ExactFrameOnly,
                "linked panels and visible 3D retain settled pixels on every refresh until replacement"
            );
        }
        assert_eq!(
            retained_frame_render_policy(false, None),
            RetainedFrameRenderPolicy::ExactFrameOnly,
            "hidden and transient targets remain exact-only"
        );
    }

    #[test]
    fn initial_and_already_progressive_targets_can_publish_useful_progress() {
        assert_eq!(
            retained_frame_render_policy(true, None),
            RetainedFrameRenderPolicy::EveryUsefulFrame
        );
        assert_eq!(
            retained_frame_render_policy(true, Some(RenderFrameCompleteness::Progressive),),
            RetainedFrameRenderPolicy::EveryUsefulFrame
        );
    }

    #[test]
    fn cross_section_currentness_uses_presented_gpu_coverage() {
        let cpu_ready = CrossSectionPanelScheduleState {
            generation: 7,
            target_scale_level: Some(0),
            render_scale_level: Some(0),
            fallback_scale_level: None,
            selected_bricks: 4,
            occupied_selected_bricks: 4,
            missing_occupied_bricks: 0,
            estimated_decoded_bytes: 0,
            decoded_budget_bytes: 0,
            status: CrossSectionPanelScheduleStatus::Ready,
            reason: CrossSectionPanelScheduleReason::TargetScaleReady,
        };

        let progressive = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Progressive,
            2,
            4,
        )
        .rendered();
        assert_eq!(
            progressive.status,
            CrossSectionPanelScheduleStatus::Incomplete
        );
        assert_eq!(progressive.occupied_selected_bricks, 2);
        assert_eq!(progressive.missing_occupied_bricks, 2);

        let defensive_progressive = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Progressive,
            4,
            4,
        )
        .rendered();
        assert_eq!(
            defensive_progressive.status,
            CrossSectionPanelScheduleStatus::Incomplete,
            "progressive metadata is never promoted to Current from counters alone"
        );

        let exact = cross_section_schedule_for_presented_coverage(
            cpu_ready,
            RenderFrameCompleteness::Exact,
            4,
            4,
        )
        .rendered();
        assert_eq!(exact.status, CrossSectionPanelScheduleStatus::Current);
        assert_eq!(exact.occupied_selected_bricks, 4);
        assert_eq!(exact.missing_occupied_bricks, 0);
    }
}
