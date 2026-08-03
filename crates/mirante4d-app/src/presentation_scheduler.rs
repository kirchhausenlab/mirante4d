//! Application-side composition of temporal, spatial, and quality intent.
//!
//! This module owns no GPU work.  It selects the one semantic transaction
//! that the renderer's `FrameCoordinator` must execute next and provides the
//! fixed-shape logical-target assembly used before an incremental physical
//! demand delta is installed.

use std::{collections::BTreeMap, fmt, sync::Arc};

use mirante4d_application::{
    ApplicationSnapshot, PresentationSlot, RenderCoordinationState, RenderIntentBase,
    RenderIntentMailbox, RenderIntentMailboxSnapshot, RenderIntentRevision,
    SourceSessionGeneration,
};
use mirante4d_domain::{
    CameraView, CrossSectionView, LogicalLayerKey, ScaleLevel, TimeIndex, ViewerLayout,
};
use mirante4d_render_api::{FrameCompleteness, PresentationTarget};
use mirante4d_render_wgpu::CoordinatedPublicationGroup;

use crate::{
    application_view,
    playback_session::{PlaybackFrameContract, PlaybackSession, PlaybackTargetSet},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PresentationTransactionCause {
    Temporal(PlaybackFrameContract),
    RetainedQuality,
}

/// One immutable semantic cutoff handed to the renderer coordinator.
///
/// The two spatial revisions are deliberately independent. A four-panel
/// temporal transaction can therefore advance time while composing the
/// latest 3D camera and latest linked-plane geometry at the same cutoff.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PresentationTransaction {
    source_generation: SourceSessionGeneration,
    layout: ViewerLayout,
    timepoint: TimeIndex,
    three_d_revision: RenderIntentRevision,
    linked_revision: RenderIntentRevision,
    camera: CameraView,
    cross_section: CrossSectionView,
    cause: PresentationTransactionCause,
}

impl PresentationTransaction {
    pub(crate) const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub(crate) const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub(crate) const fn is_retained_quality(&self) -> bool {
        matches!(self.cause, PresentationTransactionCause::RetainedQuality)
    }

    pub(crate) const fn camera(&self) -> CameraView {
        self.camera
    }

    pub(crate) const fn cross_section(&self) -> CrossSectionView {
        self.cross_section
    }

    pub(crate) const fn publication_group(&self) -> CoordinatedPublicationGroup {
        match self.layout {
            ViewerLayout::Single3d => CoordinatedPublicationGroup::THREE_D,
            ViewerLayout::FourPanel => CoordinatedPublicationGroup::FULL_LAYOUT,
        }
    }

    pub(crate) const fn target_set(&self) -> PlaybackTargetSet {
        match self.layout {
            ViewerLayout::Single3d => PlaybackTargetSet::ThreeD,
            ViewerLayout::FourPanel => PlaybackTargetSet::FullLayout,
        }
    }

    pub(crate) const fn contains(&self, target: PresentationTarget) -> bool {
        match self.layout {
            ViewerLayout::Single3d => target.index() == PresentationTarget::ThreeD.index(),
            ViewerLayout::FourPanel => true,
        }
    }

    pub(crate) const fn expected_revision(
        &self,
        target: PresentationTarget,
    ) -> RenderIntentRevision {
        if target.index() == PresentationTarget::ThreeD.index() {
            self.three_d_revision
        } else {
            self.linked_revision
        }
    }

    pub(crate) fn spatial_followup_required(&self, mailbox: RenderIntentMailboxSnapshot) -> bool {
        mailbox.three_d_revision > self.three_d_revision
            || mailbox.linked_2d_revision > self.linked_revision
    }

    pub(crate) fn temporal_contract(&self) -> Option<&PlaybackFrameContract> {
        match &self.cause {
            PresentationTransactionCause::Temporal(contract) => Some(contract),
            PresentationTransactionCause::RetainedQuality => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct RetainedQualityHandoff {
    source_generation: SourceSessionGeneration,
    layout: ViewerLayout,
    timepoint: TimeIndex,
    three_d_revision: RenderIntentRevision,
    linked_revision: RenderIntentRevision,
    camera: CameraView,
    cross_section: CrossSectionView,
}

impl RetainedQualityHandoff {
    fn transaction(&self) -> PresentationTransaction {
        PresentationTransaction {
            source_generation: self.source_generation,
            layout: self.layout,
            timepoint: self.timepoint,
            three_d_revision: self.three_d_revision,
            linked_revision: self.linked_revision,
            camera: self.camera,
            cross_section: self.cross_section,
            cause: PresentationTransactionCause::RetainedQuality,
        }
    }

    fn matches_live(
        &self,
        snapshot: &ApplicationSnapshot,
        mailbox: RenderIntentMailboxSnapshot,
    ) -> bool {
        let view = application_view(snapshot);
        self.source_generation == snapshot.source_generation()
            && self.layout == view.layout()
            && self.timepoint == view.timepoint()
            && self.three_d_revision == mailbox.three_d_revision
            && self.linked_revision == mailbox.linked_2d_revision
    }
}

/// Bounded application-side semantic scheduler.
///
/// Temporal reservation remains owned by `PlaybackSession`; latest spatial
/// values remain owned by `RenderIntentMailbox`; GPU recording and swaps
/// remain owned by the renderer. This value owns only arbitration and the one
/// retained-front quality handoff that can outlive a playback session.
#[derive(Debug, Default)]
pub(crate) struct ComposedPresentationScheduler {
    /// One temporal cutoff remains immutable from reservation through atomic
    /// renderer publication. New spatial samples stay latest-only in the
    /// mailbox and are consumed by the next cutoff; they cannot continually
    /// replace this value and starve the due timepoint.
    reserved_temporal: Option<PresentationTransaction>,
    retained_quality: Option<RetainedQualityHandoff>,
}

impl ComposedPresentationScheduler {
    pub(crate) const fn new() -> Self {
        Self {
            reserved_temporal: None,
            retained_quality: None,
        }
    }

    /// Captures a quality-only transition before the playback contract is
    /// retired. The handoff is admitted only when every visible target owns
    /// one coherent, non-progressive front at the playback cursor.
    pub(crate) fn begin_playback_stop_handoff(
        &mut self,
        snapshot: &ApplicationSnapshot,
        session: &PlaybackSession,
        mailbox: &RenderIntentMailbox,
        render: &RenderCoordinationState,
    ) -> bool {
        let Some(contract) = session.contract() else {
            return false;
        };
        let view = application_view(snapshot);
        if contract.source_generation() != snapshot.source_generation()
            || contract.target_set() != PlaybackTargetSet::from(view.layout())
        {
            return false;
        }
        let required = match view.layout() {
            ViewerLayout::Single3d => &PresentationSlot::ALL[..1],
            ViewerLayout::FourPanel => &PresentationSlot::ALL,
        };
        if !required.iter().copied().all(|slot| {
            render.surface(slot).presented_frame().is_some_and(|frame| {
                frame.timepoint() == view.timepoint()
                    && frame.progress().completeness() != FrameCompleteness::Progressive
            })
        }) {
            return false;
        }
        let base = RenderIntentBase::from_snapshot(snapshot);
        let mailbox_snapshot = mailbox.snapshot();
        self.reserved_temporal = None;
        self.retained_quality = Some(RetainedQualityHandoff {
            source_generation: snapshot.source_generation(),
            layout: view.layout(),
            timepoint: view.timepoint(),
            three_d_revision: mailbox_snapshot.three_d_revision,
            linked_revision: mailbox_snapshot.linked_2d_revision,
            camera: mailbox.effective_camera(base, *view.camera()),
            cross_section: mailbox.effective_cross_section(base, *view.cross_section()),
        });
        true
    }

    pub(crate) fn cancel_retained_quality(&mut self) {
        self.retained_quality = None;
    }

    pub(crate) fn retained_quality_active(
        &mut self,
        snapshot: &ApplicationSnapshot,
        mailbox: RenderIntentMailboxSnapshot,
    ) -> bool {
        if self
            .retained_quality
            .as_ref()
            .is_some_and(|handoff| !handoff.matches_live(snapshot, mailbox))
        {
            self.retained_quality = None;
        }
        self.retained_quality.is_some()
    }

    /// Temporal work has priority. The first observation of a pending frame
    /// reserves its spatial cutoff; later samples remain in the mailbox for
    /// an immediate follow-up and cannot remove or continually replace the
    /// pending temporal transaction.
    pub(crate) fn transaction(
        &mut self,
        snapshot: &ApplicationSnapshot,
        session: &PlaybackSession,
        mailbox: &RenderIntentMailbox,
    ) -> Option<PresentationTransaction> {
        let view = application_view(snapshot);
        let pending_contract =
            session
                .pending_frame_contract(view.timepoint())
                .filter(|contract| {
                    contract.source_generation() == snapshot.source_generation()
                        && contract.target_set() == PlaybackTargetSet::from(view.layout())
                });
        if let Some(reserved) = self.reserved_temporal.as_ref() {
            if pending_contract.as_ref().is_some_and(|contract| {
                reserved.source_generation == snapshot.source_generation()
                    && reserved.layout == view.layout()
                    && reserved.timepoint == view.timepoint()
                    && reserved.temporal_contract() == Some(contract)
            }) {
                return Some(reserved.clone());
            }
            self.reserved_temporal = None;
        }
        if let Some(contract) = pending_contract
            && contract.source_generation() == snapshot.source_generation()
            && contract.target_set() == PlaybackTargetSet::from(view.layout())
        {
            let base = RenderIntentBase::from_snapshot(snapshot);
            let mailbox_snapshot = mailbox.snapshot();
            let transaction = PresentationTransaction {
                source_generation: snapshot.source_generation(),
                layout: view.layout(),
                timepoint: view.timepoint(),
                three_d_revision: mailbox_snapshot.three_d_revision,
                linked_revision: mailbox_snapshot.linked_2d_revision,
                camera: mailbox.effective_camera(base, *view.camera()),
                cross_section: mailbox.effective_cross_section(base, *view.cross_section()),
                cause: PresentationTransactionCause::Temporal(contract),
            };
            return Some(self.reserve_temporal_candidate(transaction));
        }
        self.retained_quality_active(snapshot, mailbox.snapshot())
            .then(|| self.retained_quality.as_ref().unwrap().transaction())
    }

    fn reserve_temporal_candidate(
        &mut self,
        candidate: PresentationTransaction,
    ) -> PresentationTransaction {
        if let Some(reserved) = self.reserved_temporal.as_ref()
            && reserved.source_generation == candidate.source_generation
            && reserved.layout == candidate.layout
            && reserved.timepoint == candidate.timepoint
            && reserved.temporal_contract() == candidate.temporal_contract()
        {
            return reserved.clone();
        }
        self.reserved_temporal = Some(candidate.clone());
        candidate
    }

    pub(crate) fn complete(&mut self, transaction: &PresentationTransaction) {
        if transaction.temporal_contract().is_some()
            && self
                .reserved_temporal
                .as_ref()
                .is_some_and(|reserved| reserved == transaction)
        {
            self.reserved_temporal = None;
        }
        if transaction.is_retained_quality()
            && self
                .retained_quality
                .as_ref()
                .is_some_and(|handoff| handoff.transaction() == *transaction)
        {
            self.retained_quality = None;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresentationQuality {
    layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
}

impl PresentationQuality {
    pub(crate) fn exact(layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>) -> Self {
        Self { layer_scales }
    }

    pub(crate) fn layer_scales(&self) -> &Arc<BTreeMap<LogicalLayerKey, ScaleLevel>> {
        &self.layer_scales
    }
}

/// Complete semantic member assembled before renderer-owned reuse
/// classification. `prepared_request` is the immutable target request; no
/// application-side prepared/reused bit exists after this boundary.
#[derive(Debug)]
pub(crate) struct PresentationTransactionMember<T> {
    target: PresentationTarget,
    source_generation: SourceSessionGeneration,
    timepoint: TimeIndex,
    spatial_frame: RenderIntentRevision,
    surface_generation: u64,
    quality: PresentationQuality,
    prepared_request: T,
}

impl<T> PresentationTransactionMember<T> {
    pub(crate) fn new(
        target: PresentationTarget,
        source_generation: SourceSessionGeneration,
        timepoint: TimeIndex,
        spatial_frame: RenderIntentRevision,
        surface_generation: u64,
        quality: PresentationQuality,
        prepared_request: T,
    ) -> Self {
        Self {
            target,
            source_generation,
            timepoint,
            spatial_frame,
            surface_generation,
            quality,
            prepared_request,
        }
    }

    pub(crate) const fn target(&self) -> PresentationTarget {
        self.target
    }

    pub(crate) const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub(crate) const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub(crate) const fn spatial_frame(&self) -> RenderIntentRevision {
        self.spatial_frame
    }

    pub(crate) const fn surface_generation(&self) -> u64 {
        self.surface_generation
    }

    pub(crate) const fn quality(&self) -> &PresentationQuality {
        &self.quality
    }

    pub(crate) const fn prepared_request(&self) -> &T {
        &self.prepared_request
    }

    pub(crate) const fn prepared_request_mut(&mut self) -> &mut T {
        &mut self.prepared_request
    }

    pub(crate) fn into_prepared_request(self) -> T {
        self.prepared_request
    }
}

#[derive(Debug)]
pub(crate) enum PresentationTransactionTargets<T> {
    ThreeD {
        three_d: PresentationTransactionMember<T>,
    },
    FourPanel {
        three_d: PresentationTransactionMember<T>,
        xy: PresentationTransactionMember<T>,
        xz: PresentationTransactionMember<T>,
        yz: PresentationTransactionMember<T>,
    },
}

impl<T> PresentationTransactionTargets<T> {
    pub(crate) fn from_slots(
        target_set: PlaybackTargetSet,
        mut members: [Option<PresentationTransactionMember<T>>; 4],
    ) -> Result<Self, MissingLogicalTarget> {
        let mut take = |target: PresentationTarget| {
            let Some(member) = members[target.index()].take() else {
                return Err(MissingLogicalTarget(target));
            };
            if member.target != target {
                return Err(MissingLogicalTarget(target));
            }
            Ok(member)
        };
        let targets = match target_set {
            PlaybackTargetSet::ThreeD => Self::ThreeD {
                three_d: take(PresentationTarget::ThreeD)?,
            },
            PlaybackTargetSet::FullLayout => Self::FourPanel {
                three_d: take(PresentationTarget::ThreeD)?,
                xy: take(PresentationTarget::Xy)?,
                xz: take(PresentationTarget::Xz)?,
                yz: take(PresentationTarget::Yz)?,
            },
        };
        if let Some(member) = members.into_iter().flatten().next() {
            return Err(MissingLogicalTarget(member.target));
        }
        Ok(targets)
    }

    pub(crate) fn for_each_mut(
        &mut self,
        mut visit: impl FnMut(&mut PresentationTransactionMember<T>),
    ) {
        match self {
            Self::ThreeD { three_d } => visit(three_d),
            Self::FourPanel {
                three_d,
                xy,
                xz,
                yz,
            } => {
                visit(three_d);
                visit(xy);
                visit(xz);
                visit(yz);
            }
        }
    }

    pub(crate) fn into_prepared_requests(self) -> FixedPresentationTargetRequests<T> {
        match self {
            Self::ThreeD { three_d } => {
                FixedPresentationTargetRequests::ThreeD([three_d.into_prepared_request()])
            }
            Self::FourPanel {
                three_d,
                xy,
                xz,
                yz,
            } => FixedPresentationTargetRequests::FourPanel([
                three_d.into_prepared_request(),
                xy.into_prepared_request(),
                xz.into_prepared_request(),
                yz.into_prepared_request(),
            ]),
        }
    }
}

/// Fixed storage for the prepared requests of one complete logical
/// transaction. The canonical array order is 3D, XY, XZ, YZ.
#[derive(Debug)]
pub(crate) enum FixedPresentationTargetRequests<T> {
    ThreeD([T; 1]),
    FourPanel([T; 4]),
}

impl<T> FixedPresentationTargetRequests<T> {
    pub(crate) const fn as_slice(&self) -> &[T] {
        match self {
            Self::ThreeD(requests) => requests,
            Self::FourPanel(requests) => requests,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MissingLogicalTarget(pub(crate) PresentationTarget);

impl fmt::Display for MissingLogicalTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "temporal logical target {:?} has no prepared immutable request",
            self.0
        )
    }
}

impl std::error::Error for MissingLogicalTarget {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use mirante4d_application::{PlaybackFps, SourceSessionGeneration};
    use mirante4d_domain::{LogicalLayerKey, ScaleLevel};

    use super::*;

    #[test]
    fn composed_physical_delta_matrix_keeps_complete_logical_members() {
        let member = |target, renderer_will_reuse| {
            PresentationTransactionMember::new(
                target,
                SourceSessionGeneration::new(7),
                TimeIndex::new(3),
                RenderIntentRevision::new(if target == PresentationTarget::ThreeD {
                    11
                } else {
                    13
                }),
                17 + target.index() as u64,
                PresentationQuality::exact(Arc::new(BTreeMap::from([(
                    LogicalLayerKey::new(0),
                    ScaleLevel::new(1),
                )]))),
                renderer_will_reuse,
            )
        };
        for reused_bits in 0_u8..16 {
            let members = std::array::from_fn(|index| {
                let target = PresentationTarget::ALL[index];
                Some(member(target, reused_bits & (1 << target.index()) != 0))
            });
            let mut assembled =
                PresentationTransactionTargets::from_slots(PlaybackTargetSet::FullLayout, members)
                    .unwrap();
            let mut observed = Vec::new();
            assembled.for_each_mut(|member| {
                observed.push((member.target(), *member.prepared_request()));
            });
            observed.sort_unstable_by_key(|(target, _)| target.index());
            assert_eq!(
                observed,
                PresentationTarget::ALL
                    .into_iter()
                    .map(|target| (target, reused_bits & (1 << target.index()) != 0))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn temporal_arbitration_uses_latest_independent_spatial_revisions() {
        let mut session = PlaybackSession::new();
        let source = SourceSessionGeneration::new(7);
        let fps = PlaybackFps::new(24).unwrap();
        session.begin_warmup(source, fps, ViewerLayout::FourPanel);
        assert!(session.admit_contract(
            source,
            fps,
            ViewerLayout::FourPanel,
            BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::new(1))]),
            2,
            1,
            1024,
            2048,
            TimeIndex::new(0),
            &[TimeIndex::new(1), TimeIndex::new(2)],
        ));
        let contract = session.pending_frame_contract(TimeIndex::new(1)).unwrap();
        let transaction = PresentationTransaction {
            source_generation: source,
            layout: ViewerLayout::FourPanel,
            timepoint: TimeIndex::new(1),
            three_d_revision: RenderIntentRevision::new(12),
            linked_revision: RenderIntentRevision::new(19),
            camera: camera(),
            cross_section: cross_section(),
            cause: PresentationTransactionCause::Temporal(contract),
        };

        assert_eq!(
            transaction.expected_revision(PresentationTarget::ThreeD),
            RenderIntentRevision::new(12)
        );
        for target in [
            PresentationTarget::Xy,
            PresentationTarget::Xz,
            PresentationTarget::Yz,
        ] {
            assert_eq!(
                transaction.expected_revision(target),
                RenderIntentRevision::new(19)
            );
        }
        assert_eq!(
            transaction.publication_group(),
            CoordinatedPublicationGroup::FULL_LAYOUT
        );
    }

    #[test]
    fn reserved_temporal_cutoff_is_not_displaced_by_newer_linked_input() {
        let source = SourceSessionGeneration::new(7);
        let fps = PlaybackFps::new(24).unwrap();
        let mut session = PlaybackSession::new();
        session.begin_warmup(source, fps, ViewerLayout::FourPanel);
        assert!(session.admit_contract(
            source,
            fps,
            ViewerLayout::FourPanel,
            BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::new(1))]),
            2,
            1,
            1024,
            2048,
            TimeIndex::new(8),
            &[TimeIndex::new(9), TimeIndex::new(10)],
        ));
        let contract = session.pending_frame_contract(TimeIndex::new(9)).unwrap();
        let first = PresentationTransaction {
            source_generation: source,
            layout: ViewerLayout::FourPanel,
            timepoint: TimeIndex::new(9),
            three_d_revision: RenderIntentRevision::new(12),
            linked_revision: RenderIntentRevision::new(19),
            camera: camera(),
            cross_section: cross_section(),
            cause: PresentationTransactionCause::Temporal(contract.clone()),
        };
        let mut newer = first.clone();
        newer.linked_revision = RenderIntentRevision::new(25);
        newer.cross_section = CrossSectionView::new(
            mirante4d_domain::WorldPoint3::new(2.0, 0.0, 0.0).unwrap(),
            mirante4d_domain::UnitQuaternion::identity(),
            0.5,
            1.0,
        )
        .unwrap();

        let mut scheduler = ComposedPresentationScheduler::new();
        assert_eq!(scheduler.reserve_temporal_candidate(first.clone()), first);
        assert_eq!(
            scheduler.reserve_temporal_candidate(newer.clone()),
            first,
            "new spatial input remains latest-only until the reserved timepoint publishes"
        );
        scheduler.complete(&first);
        assert_eq!(scheduler.reserve_temporal_candidate(newer.clone()), newer);
    }

    fn camera() -> CameraView {
        CameraView::new(
            mirante4d_domain::Projection::Orthographic,
            mirante4d_domain::WorldPoint3::origin(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            320.0,
            40.0,
        )
        .unwrap()
    }

    fn cross_section() -> CrossSectionView {
        CrossSectionView::new(
            mirante4d_domain::WorldPoint3::origin(),
            mirante4d_domain::UnitQuaternion::identity(),
            1.0,
            1.0,
        )
        .unwrap()
    }
}
