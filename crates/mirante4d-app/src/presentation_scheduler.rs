//! Application-side composition of temporal, spatial, and quality intent.
//!
//! This module owns no GPU work.  It selects the one semantic transaction
//! that the renderer's `FrameCoordinator` must execute next and provides the
//! dynamic logical-target assembly used before an incremental physical
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
use mirante4d_render_api::{
    FrameCompleteness, PresentationTarget, PresentationTargetSet, RenderExtent,
};

use crate::{
    application_view,
    playback_session::{PlaybackFrameContract, PlaybackSession, playback_targets_for_layout},
};

pub(crate) const fn active_targets_for_layout(layout: ViewerLayout) -> PresentationTargetSet {
    match layout {
        ViewerLayout::Single3d => PresentationTargetSet::THREE_D,
        ViewerLayout::FourPanel => PresentationTargetSet::ALL,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivePresentationTarget {
    target: PresentationTarget,
    extent: RenderExtent,
    surface_generation: u64,
}

impl ActivePresentationTarget {
    pub(crate) const fn new(
        target: PresentationTarget,
        extent: RenderExtent,
        surface_generation: u64,
    ) -> Self {
        Self {
            target,
            extent,
            surface_generation,
        }
    }

    pub(crate) const fn target(self) -> PresentationTarget {
        self.target
    }

    pub(crate) const fn extent(self) -> RenderExtent {
        self.extent
    }

    pub(crate) const fn surface_generation(self) -> u64 {
        self.surface_generation
    }
}

/// Immutable fixed-capacity rendering layout for one semantic transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivePresentationLayout {
    generation: u64,
    targets: PresentationTargetSet,
    members: [Option<ActivePresentationTarget>; 4],
}

impl ActivePresentationLayout {
    pub(crate) fn new(
        generation: u64,
        targets: PresentationTargetSet,
        members: [Option<ActivePresentationTarget>; 4],
    ) -> Result<Self, ActivePresentationLayoutError> {
        for target in PresentationTarget::ALL {
            match members[target.index()] {
                Some(member) if member.target == target && targets.contains(target) => {}
                None if !targets.contains(target) => {}
                _ => return Err(ActivePresentationLayoutError { target }),
            }
        }
        Ok(Self {
            generation,
            targets,
            members,
        })
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) const fn targets(&self) -> PresentationTargetSet {
        self.targets
    }

    pub(crate) const fn affected_targets(
        &self,
        dependencies: PresentationTargetSet,
    ) -> PresentationTargetSet {
        self.targets.intersection(dependencies)
    }

    pub(crate) const fn member(
        &self,
        target: PresentationTarget,
    ) -> Option<ActivePresentationTarget> {
        self.members[target.index()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActivePresentationLayoutError {
    target: PresentationTarget,
}

impl fmt::Display for ActivePresentationLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "active layout member {:?} is missing or misplaced",
            self.target
        )
    }
}

impl std::error::Error for ActivePresentationLayoutError {}

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

    pub(crate) const fn target_set(&self) -> PresentationTargetSet {
        active_targets_for_layout(self.layout)
    }

    pub(crate) const fn contains(&self, target: PresentationTarget) -> bool {
        self.target_set().contains(target)
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
            || contract.target_set() != playback_targets_for_layout(view.layout())
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
                        && contract.target_set() == playback_targets_for_layout(view.layout())
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
            && contract.target_set() == playback_targets_for_layout(view.layout())
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

    pub(crate) fn into_prepared_request(self) -> T {
        self.prepared_request
    }
}

#[derive(Debug)]
pub(crate) struct PresentationTransactionTargets<T> {
    targets: PresentationTargetSet,
    terminal_no_work: PresentationTargetSet,
    members: [Option<PresentationTransactionMember<T>>; 4],
}

impl<T> PresentationTransactionTargets<T> {
    pub(crate) fn from_slots(
        targets: PresentationTargetSet,
        terminal_no_work: PresentationTargetSet,
        members: [Option<PresentationTransactionMember<T>>; 4],
    ) -> Result<Self, MissingLogicalTarget> {
        if targets.is_empty() || !terminal_no_work.difference(targets).is_empty() {
            return Err(MissingLogicalTarget(PresentationTarget::ThreeD));
        }
        for target in PresentationTarget::ALL {
            match members[target.index()].as_ref() {
                Some(member)
                    if targets.contains(target)
                        && !terminal_no_work.contains(target)
                        && member.target == target => {}
                None if targets.contains(target) && terminal_no_work.contains(target) => {}
                None if !targets.contains(target) => {}
                Some(member) => return Err(MissingLogicalTarget(member.target)),
                None => return Err(MissingLogicalTarget(target)),
            }
        }
        Ok(Self {
            targets,
            terminal_no_work,
            members,
        })
    }

    pub(crate) fn for_each_mut(
        &mut self,
        mut visit: impl FnMut(&mut PresentationTransactionMember<T>),
    ) {
        for target in self.targets {
            if !self.terminal_no_work.contains(target) {
                visit(
                    self.members[target.index()]
                        .as_mut()
                        .expect("a nonterminal logical target owns one immutable request"),
                );
            }
        }
    }

    pub(crate) fn into_prepared_requests(mut self) -> Vec<T> {
        let mut requests = Vec::with_capacity(
            self.targets
                .len()
                .saturating_sub(self.terminal_no_work.len()),
        );
        for target in self.targets {
            if !self.terminal_no_work.contains(target) {
                requests.push(
                    self.members[target.index()]
                        .take()
                        .expect("a nonterminal logical target owns one immutable request")
                        .into_prepared_request(),
                );
            }
        }
        requests
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

    fn target_set_from_bits(bits: u8) -> PresentationTargetSet {
        PresentationTarget::ALL
            .into_iter()
            .filter(|target| bits & (1 << target.index()) != 0)
            .fold(PresentationTargetSet::EMPTY, PresentationTargetSet::with)
    }

    fn active_layout(generation: u64, targets: PresentationTargetSet) -> ActivePresentationLayout {
        let extent = RenderExtent::new(64, 48).unwrap();
        let members = std::array::from_fn(|index| {
            let target = PresentationTarget::ALL[index];
            targets
                .contains(target)
                .then_some(ActivePresentationTarget::new(
                    target,
                    extent,
                    generation + index as u64,
                ))
        });
        ActivePresentationLayout::new(generation, targets, members).unwrap()
    }

    #[test]
    fn active_affected_target_set_covers_every_bounded_layout_combination() {
        for active_bits in 0_u8..16 {
            let active = target_set_from_bits(active_bits);
            let layout = active_layout(7, active);
            assert_eq!(
                layout.targets().iter().collect::<Vec<_>>(),
                PresentationTarget::ALL
                    .into_iter()
                    .filter(|target| active.contains(*target))
                    .collect::<Vec<_>>(),
                "active targets retain canonical order for mask {active_bits:04b}"
            );
            for dependency_bits in 0_u8..16 {
                let dependencies = target_set_from_bits(dependency_bits);
                assert_eq!(
                    layout.affected_targets(dependencies),
                    active.intersection(dependencies),
                    "active={active_bits:04b} dependencies={dependency_bits:04b}"
                );
            }
        }
    }

    #[test]
    fn layout_generation_change_suppresses_old_group_without_shrinking_it() {
        let old_targets = PresentationTargetSet::ALL;
        let old = active_layout(11, old_targets);
        let replacement_targets = old_targets.without(PresentationTarget::Yz);
        let replacement = active_layout(12, replacement_targets);

        assert_eq!(
            old.affected_targets(PresentationTargetSet::LINKED_CROSS_SECTIONS),
            PresentationTargetSet::LINKED_CROSS_SECTIONS,
            "the in-flight old cohort keeps its original membership"
        );
        assert_eq!(
            replacement.affected_targets(PresentationTargetSet::LINKED_CROSS_SECTIONS),
            PresentationTargetSet::LINKED_CROSS_SECTIONS.without(PresentationTarget::Yz)
        );
        assert_ne!(old.generation(), replacement.generation());
        assert_ne!(old, replacement);
    }

    #[test]
    fn hidden_linked_target_tracks_semantic_state_without_render_obligation() {
        let visible_targets = PresentationTargetSet::ALL.without(PresentationTarget::Yz);
        let visible = active_layout(20, visible_targets);
        let linked_work = visible.affected_targets(PresentationTargetSet::LINKED_CROSS_SECTIONS);
        assert_eq!(
            linked_work,
            PresentationTargetSet::LINKED_CROSS_SECTIONS.without(PresentationTarget::Yz)
        );

        let mut envelope_work = [0_u64; 4];
        let mut body_work = [0_u64; 4];
        let mut residency_work = [0_u64; 4];
        let mut texture_work = [0_u64; 4];
        let mut publication_work = [0_u64; 4];
        let mut latest_linked_revision = 0_u64;
        for revision in [31_u64, 32] {
            latest_linked_revision = revision;
            for target in linked_work {
                envelope_work[target.index()] += 1;
                body_work[target.index()] += 1;
                residency_work[target.index()] += 1;
                texture_work[target.index()] += 1;
                publication_work[target.index()] += 1;
            }
        }
        assert_eq!(envelope_work[PresentationTarget::Yz.index()], 0);
        assert_eq!(body_work[PresentationTarget::Yz.index()], 0);
        assert_eq!(residency_work[PresentationTarget::Yz.index()], 0);
        assert_eq!(texture_work[PresentationTarget::Yz.index()], 0);
        assert_eq!(publication_work[PresentationTarget::Yz.index()], 0);
        assert_eq!(envelope_work[PresentationTarget::ThreeD.index()], 0);

        let opened = active_layout(21, PresentationTargetSet::ALL);
        assert_eq!(
            opened.affected_targets(PresentationTargetSet::from_target(PresentationTarget::Yz)),
            PresentationTargetSet::from_target(PresentationTarget::Yz)
        );
        let opened_member = PresentationTransactionMember::new(
            PresentationTarget::Yz,
            SourceSessionGeneration::new(7),
            TimeIndex::new(3),
            RenderIntentRevision::new(latest_linked_revision),
            opened
                .member(PresentationTarget::Yz)
                .expect("the opened layout contains YZ")
                .surface_generation(),
            PresentationQuality::exact(Arc::new(BTreeMap::new())),
            latest_linked_revision,
        );
        assert_eq!(opened_member.spatial_frame().get(), 32);
        assert_eq!(*opened_member.prepared_request(), 32);
    }

    #[test]
    fn out_of_order_linked_prerequisites_publish_the_visible_affected_group_once() {
        let active = active_layout(30, PresentationTargetSet::ALL);
        let targets = active.affected_targets(PresentationTargetSet::LINKED_CROSS_SECTIONS);
        let completion_order = [
            PresentationTarget::Yz,
            PresentationTarget::Xy,
            PresentationTarget::Xz,
        ];
        let mut ready = PresentationTargetSet::EMPTY;
        let mut publication_attempts = 0_u64;
        let mut published = Vec::new();

        for completed in completion_order {
            ready = ready.with(completed);
            let members = std::array::from_fn(|index| {
                let target = PresentationTarget::ALL[index];
                (targets.contains(target) && ready.contains(target)).then(|| {
                    PresentationTransactionMember::new(
                        target,
                        SourceSessionGeneration::new(7),
                        TimeIndex::new(3),
                        RenderIntentRevision::new(44),
                        30 + index as u64,
                        PresentationQuality::exact(Arc::new(BTreeMap::new())),
                        target,
                    )
                })
            });
            match PresentationTransactionTargets::from_slots(
                targets,
                PresentationTargetSet::EMPTY,
                members,
            ) {
                Ok(group) => {
                    publication_attempts += 1;
                    published = group.into_prepared_requests();
                }
                Err(_) => {
                    assert!(published.is_empty(), "no ready subset may publish");
                }
            }
        }

        assert_eq!(publication_attempts, 1);
        assert_eq!(
            published,
            vec![
                PresentationTarget::Xy,
                PresentationTarget::Xz,
                PresentationTarget::Yz,
            ]
        );
        assert!(!targets.contains(PresentationTarget::ThreeD));
        assert_eq!(
            active
                .member(PresentationTarget::ThreeD)
                .unwrap()
                .surface_generation(),
            30,
            "unrelated visible 3D remains outside the linked cohort"
        );
    }

    #[test]
    fn terminal_no_work_member_completes_semantics_without_entering_physical_delta() {
        let targets = PresentationTargetSet::LINKED_CROSS_SECTIONS;
        let terminal = PresentationTargetSet::from_target(PresentationTarget::Yz);
        let members = std::array::from_fn(|index| {
            let target = PresentationTarget::ALL[index];
            matches!(target, PresentationTarget::Xy | PresentationTarget::Xz).then(|| {
                PresentationTransactionMember::new(
                    target,
                    SourceSessionGeneration::new(7),
                    TimeIndex::new(3),
                    RenderIntentRevision::new(44),
                    30 + index as u64,
                    PresentationQuality::exact(Arc::new(BTreeMap::new())),
                    target,
                )
            })
        });

        let physical = PresentationTransactionTargets::from_slots(targets, terminal, members)
            .unwrap()
            .into_prepared_requests();
        assert_eq!(
            physical,
            vec![PresentationTarget::Xy, PresentationTarget::Xz]
        );
        assert!(
            !physical.contains(&PresentationTarget::Yz),
            "a terminal empty member is semantic completion, not GPU work"
        );
    }

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
            let mut assembled = PresentationTransactionTargets::from_slots(
                PresentationTargetSet::ALL,
                PresentationTargetSet::EMPTY,
                members,
            )
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
        assert_eq!(transaction.target_set(), PresentationTargetSet::ALL);
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
