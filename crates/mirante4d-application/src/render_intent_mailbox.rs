//! Bounded transient render intent for interactive viewer gestures.
//!
//! Raw pointer samples belong here, not in durable application state. The
//! mailbox retains only the latest sample for one active gesture so a burst of
//! input can drive a hot render path and then commit once when it settles.

use crate::{
    ApplicationSnapshot, CameraView, CrossSectionPanelId, CrossSectionView, CurrentnessGeneration,
    SourceSessionGeneration,
};
pub use mirante4d_render_api::FrameIdentity as RenderIntentRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RenderGestureId(u64);

impl RenderGestureId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderIntentBase {
    source_generation: SourceSessionGeneration,
    currentness: CurrentnessGeneration,
}

impl RenderIntentBase {
    pub const fn new(
        source_generation: SourceSessionGeneration,
        currentness: CurrentnessGeneration,
    ) -> Self {
        Self {
            source_generation,
            currentness,
        }
    }

    pub fn from_snapshot(snapshot: &ApplicationSnapshot) -> Self {
        Self::new(snapshot.source_generation(), snapshot.currentness())
    }

    pub const fn source_generation(self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub const fn currentness(self) -> CurrentnessGeneration {
        self.currentness
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderIntentTarget {
    ThreeD,
    CrossSection(CrossSectionPanelId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderIntentFamily {
    ThreeD,
    Linked2d,
    Both,
}

impl RenderIntentFamily {
    const fn includes_three_d(self) -> bool {
        matches!(self, Self::ThreeD | Self::Both)
    }

    const fn includes_linked_2d(self) -> bool {
        matches!(self, Self::Linked2d | Self::Both)
    }
}

impl RenderIntentTarget {
    pub const fn family(self) -> RenderIntentFamily {
        match self {
            Self::ThreeD => RenderIntentFamily::ThreeD,
            Self::CrossSection(_) => RenderIntentFamily::Linked2d,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderGestureKind {
    Drag,
    Scroll,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderIntentPayload {
    Camera(CameraView),
    CrossSection(CrossSectionView),
}

impl RenderIntentPayload {
    const fn matches_target(self, target: RenderIntentTarget) -> bool {
        matches!(
            (self, target),
            (Self::Camera(_), RenderIntentTarget::ThreeD)
                | (Self::CrossSection(_), RenderIntentTarget::CrossSection(_))
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderIntentSample {
    target: RenderIntentTarget,
    kind: RenderGestureKind,
    payload: RenderIntentPayload,
}

impl RenderIntentSample {
    pub const fn camera(kind: RenderGestureKind, camera: CameraView) -> Self {
        Self {
            target: RenderIntentTarget::ThreeD,
            kind,
            payload: RenderIntentPayload::Camera(camera),
        }
    }

    pub const fn cross_section(
        panel: CrossSectionPanelId,
        kind: RenderGestureKind,
        cross_section: CrossSectionView,
    ) -> Self {
        Self {
            target: RenderIntentTarget::CrossSection(panel),
            kind,
            payload: RenderIntentPayload::CrossSection(cross_section),
        }
    }

    pub const fn target(self) -> RenderIntentTarget {
        self.target
    }

    pub const fn kind(self) -> RenderGestureKind {
        self.kind
    }

    pub const fn payload(self) -> RenderIntentPayload {
        self.payload
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompletedRenderIntent {
    gesture_id: RenderGestureId,
    target: RenderIntentTarget,
    kind: RenderGestureKind,
    payload: RenderIntentPayload,
    revision: RenderIntentRevision,
}

impl CompletedRenderIntent {
    pub const fn gesture_id(self) -> RenderGestureId {
        self.gesture_id
    }

    pub const fn target(self) -> RenderIntentTarget {
        self.target
    }

    pub const fn kind(self) -> RenderGestureKind {
        self.kind
    }

    pub const fn payload(self) -> RenderIntentPayload {
        self.payload
    }

    pub const fn revision(self) -> RenderIntentRevision {
        self.revision
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderIntentMailboxError {
    TargetPayloadMismatch,
    CounterExhausted,
}

impl std::fmt::Display for RenderIntentMailboxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TargetPayloadMismatch => {
                formatter.write_str("render intent target does not match its payload")
            }
            Self::CounterExhausted => formatter.write_str("render intent counter exhausted"),
        }
    }
}

impl std::error::Error for RenderIntentMailboxError {}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ActiveRenderGesture {
    gesture_id: RenderGestureId,
    base: RenderIntentBase,
    target: RenderIntentTarget,
    kind: RenderGestureKind,
    payload: RenderIntentPayload,
    revision: RenderIntentRevision,
    last_input_at_ns: u64,
    renderable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderIntentMailboxSnapshot {
    /// Latest value allocated from the one uniqueness sequence. Product
    /// presentation must use the target-family fields below instead.
    pub latest_revision: RenderIntentRevision,
    pub three_d_revision: RenderIntentRevision,
    pub linked_2d_revision: RenderIntentRevision,
    pub active_gesture: Option<RenderGestureId>,
    pub active_target: Option<RenderIntentTarget>,
    pub raw_samples: u64,
    pub coalesced_samples: u64,
    pub finished_gestures: u64,
    pub cancelled_gestures: u64,
}

#[derive(Debug)]
pub struct RenderIntentMailbox {
    next_revision: u64,
    three_d_revision: RenderIntentRevision,
    linked_2d_revision: RenderIntentRevision,
    next_gesture_id: u64,
    active: Option<ActiveRenderGesture>,
    raw_samples: u64,
    coalesced_samples: u64,
    finished_gestures: u64,
    cancelled_gestures: u64,
}

impl Default for RenderIntentMailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderIntentMailbox {
    pub const fn new() -> Self {
        Self {
            next_revision: 1,
            three_d_revision: RenderIntentRevision::initial(),
            linked_2d_revision: RenderIntentRevision::initial(),
            next_gesture_id: 1,
            active: None,
            raw_samples: 0,
            coalesced_samples: 0,
            finished_gestures: 0,
            cancelled_gestures: 0,
        }
    }

    pub fn sample(
        &mut self,
        base: RenderIntentBase,
        sample: RenderIntentSample,
        now_ns: u64,
        renderable: bool,
    ) -> Result<RenderIntentRevision, RenderIntentMailboxError> {
        if !sample.payload.matches_target(sample.target) {
            return Err(RenderIntentMailboxError::TargetPayloadMismatch);
        }
        let revision = self.allocate_revision()?;
        self.record_family_revision(sample.target.family(), revision);
        self.raw_samples = self.raw_samples.saturating_add(1);
        if let Some(active) = self.active.as_mut()
            && active.base == base
            && active.target == sample.target
            && active.kind == sample.kind
        {
            active.payload = sample.payload;
            active.revision = revision;
            active.last_input_at_ns = now_ns;
            active.renderable = renderable;
            self.coalesced_samples = self.coalesced_samples.saturating_add(1);
            return Ok(revision);
        }

        self.cancel();
        let gesture_id = self.allocate_gesture_id()?;
        self.active = Some(ActiveRenderGesture {
            gesture_id,
            base,
            target: sample.target,
            kind: sample.kind,
            payload: sample.payload,
            revision,
            last_input_at_ns: now_ns,
            renderable,
        });
        Ok(revision)
    }

    pub fn finish(
        &mut self,
        base: RenderIntentBase,
        target: RenderIntentTarget,
    ) -> Option<CompletedRenderIntent> {
        if self.active.is_some_and(|active| active.base != base) {
            self.cancel();
            return None;
        }
        if !self.active.is_some_and(|active| active.target == target) {
            return None;
        }
        self.take_finished()
    }

    pub fn finish_due(
        &mut self,
        base: RenderIntentBase,
        now_ns: u64,
        settle_ns: u64,
    ) -> Option<CompletedRenderIntent> {
        if self.active.is_some_and(|active| active.base != base) {
            self.cancel();
            return None;
        }
        let due = self.active.is_some_and(|active| {
            active.kind == RenderGestureKind::Scroll
                && now_ns.saturating_sub(active.last_input_at_ns) >= settle_ns
        });
        due.then(|| self.take_finished()).flatten()
    }

    pub fn scroll_settle_remaining_ns(
        &self,
        base: RenderIntentBase,
        now_ns: u64,
        settle_ns: u64,
    ) -> Option<u64> {
        self.active.and_then(|active| {
            (active.base == base && active.kind == RenderGestureKind::Scroll)
                .then(|| settle_ns.saturating_sub(now_ns.saturating_sub(active.last_input_at_ns)))
        })
    }

    pub fn synchronize_base(&mut self, base: RenderIntentBase) -> bool {
        if self.active.is_some_and(|active| active.base != base) {
            self.cancel();
            true
        } else {
            false
        }
    }

    pub fn observe_durable_intent(
        &mut self,
        family: RenderIntentFamily,
    ) -> Result<RenderIntentRevision, RenderIntentMailboxError> {
        self.cancel();
        let revision = self.allocate_revision()?;
        self.record_family_revision(family, revision);
        Ok(revision)
    }

    pub fn cancel(&mut self) {
        if self.active.take().is_some() {
            self.cancelled_gestures = self.cancelled_gestures.saturating_add(1);
        }
    }

    pub fn effective_camera(&self, base: RenderIntentBase, durable: CameraView) -> CameraView {
        match self.active {
            Some(ActiveRenderGesture {
                base: active_base,
                payload: RenderIntentPayload::Camera(camera),
                ..
            }) if active_base == base => camera,
            _ => durable,
        }
    }

    pub fn renderable_camera(&self, base: RenderIntentBase) -> Option<CameraView> {
        match self.active {
            Some(ActiveRenderGesture {
                base: active_base,
                payload: RenderIntentPayload::Camera(camera),
                renderable: true,
                ..
            }) if active_base == base => Some(camera),
            _ => None,
        }
    }

    pub fn mark_renderable(
        &mut self,
        base: RenderIntentBase,
        revision: RenderIntentRevision,
    ) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        if active.base != base || active.revision != revision || active.renderable {
            return false;
        }
        active.renderable = true;
        true
    }

    pub fn active_target(&self, base: RenderIntentBase) -> Option<RenderIntentTarget> {
        self.active
            .filter(|active| active.base == base)
            .map(|active| active.target)
    }

    pub fn active_revision(&self, base: RenderIntentBase) -> Option<RenderIntentRevision> {
        self.active
            .filter(|active| active.base == base)
            .map(|active| active.revision)
    }

    pub fn effective_cross_section(
        &self,
        base: RenderIntentBase,
        durable: CrossSectionView,
    ) -> CrossSectionView {
        match self.active {
            Some(ActiveRenderGesture {
                base: active_base,
                payload: RenderIntentPayload::CrossSection(cross_section),
                ..
            }) if active_base == base => cross_section,
            _ => durable,
        }
    }

    pub fn snapshot(&self) -> RenderIntentMailboxSnapshot {
        RenderIntentMailboxSnapshot {
            latest_revision: RenderIntentRevision::new(self.next_revision.saturating_sub(1)),
            three_d_revision: self.three_d_revision,
            linked_2d_revision: self.linked_2d_revision,
            active_gesture: self.active.map(|active| active.gesture_id),
            active_target: self.active.map(|active| active.target),
            raw_samples: self.raw_samples,
            coalesced_samples: self.coalesced_samples,
            finished_gestures: self.finished_gestures,
            cancelled_gestures: self.cancelled_gestures,
        }
    }

    fn take_finished(&mut self) -> Option<CompletedRenderIntent> {
        let active = self.active.take()?;
        self.finished_gestures = self.finished_gestures.saturating_add(1);
        Some(CompletedRenderIntent {
            gesture_id: active.gesture_id,
            target: active.target,
            kind: active.kind,
            payload: active.payload,
            revision: active.revision,
        })
    }

    fn allocate_revision(&mut self) -> Result<RenderIntentRevision, RenderIntentMailboxError> {
        let revision = RenderIntentRevision::new(self.next_revision);
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(RenderIntentMailboxError::CounterExhausted)?;
        Ok(revision)
    }

    fn record_family_revision(
        &mut self,
        family: RenderIntentFamily,
        revision: RenderIntentRevision,
    ) {
        if family.includes_three_d() {
            self.three_d_revision = revision;
        }
        if family.includes_linked_2d() {
            self.linked_2d_revision = revision;
        }
    }

    fn allocate_gesture_id(&mut self) -> Result<RenderGestureId, RenderIntentMailboxError> {
        let gesture_id = RenderGestureId(self.next_gesture_id);
        self.next_gesture_id = self
            .next_gesture_id
            .checked_add(1)
            .ok_or(RenderIntentMailboxError::CounterExhausted)?;
        Ok(gesture_id)
    }
}

#[cfg(test)]
mod tests {
    use mirante4d_domain::{Projection, UnitQuaternion, WorldPoint3};

    use super::*;

    fn base(currentness: u64) -> RenderIntentBase {
        RenderIntentBase::new(
            SourceSessionGeneration::new(7),
            CurrentnessGeneration(currentness),
        )
    }

    fn camera(scale: f64) -> CameraView {
        CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(1.0, 2.0, 3.0).unwrap(),
            UnitQuaternion::identity(),
            scale,
            500.0,
            100.0,
        )
        .unwrap()
    }

    #[test]
    fn many_samples_overwrite_one_gesture_and_finish_latest_once() {
        let mut mailbox = RenderIntentMailbox::new();
        let intent_base = base(4);

        for sample in 0_u32..300 {
            mailbox
                .sample(
                    intent_base,
                    RenderIntentSample::camera(
                        RenderGestureKind::Drag,
                        camera(1.0 + f64::from(sample)),
                    ),
                    u64::from(sample),
                    false,
                )
                .unwrap();
        }

        let before_finish = mailbox.snapshot();
        assert_eq!(before_finish.raw_samples, 300);
        assert_eq!(before_finish.coalesced_samples, 299);
        assert!(before_finish.active_gesture.is_some());
        let completed = mailbox
            .finish(intent_base, RenderIntentTarget::ThreeD)
            .unwrap();
        assert_eq!(
            completed.payload(),
            RenderIntentPayload::Camera(camera(300.0))
        );
        assert_eq!(mailbox.snapshot().finished_gestures, 1);
        assert!(mailbox.snapshot().active_gesture.is_none());
    }

    #[test]
    fn stale_base_cancels_instead_of_committing_old_input() {
        let mut mailbox = RenderIntentMailbox::new();
        mailbox
            .sample(
                base(4),
                RenderIntentSample::camera(RenderGestureKind::Drag, camera(2.0)),
                10,
                true,
            )
            .unwrap();

        assert!(
            mailbox
                .finish(base(5), RenderIntentTarget::ThreeD)
                .is_none()
        );
        assert_eq!(mailbox.snapshot().cancelled_gestures, 1);
        assert!(mailbox.renderable_camera(base(4)).is_none());
    }

    #[test]
    fn scroll_finishes_only_after_its_last_sample_settles() {
        let mut mailbox = RenderIntentMailbox::new();
        mailbox
            .sample(
                base(4),
                RenderIntentSample::camera(RenderGestureKind::Scroll, camera(2.0)),
                100,
                true,
            )
            .unwrap();

        assert!(mailbox.finish_due(base(4), 219, 120).is_none());
        assert!(mailbox.finish_due(base(4), 220, 120).is_some());
        assert_eq!(mailbox.snapshot().finished_gestures, 1);
    }

    #[test]
    fn target_families_advance_independently_on_one_unique_sequence() {
        let mut mailbox = RenderIntentMailbox::new();
        let initial = mailbox.snapshot();

        let linked = mailbox
            .observe_durable_intent(RenderIntentFamily::Linked2d)
            .unwrap();
        let after_linked = mailbox.snapshot();
        assert_eq!(after_linked.latest_revision, linked);
        assert_eq!(after_linked.linked_2d_revision, linked);
        assert_eq!(after_linked.three_d_revision, initial.three_d_revision);

        let three_d = mailbox
            .sample(
                base(4),
                RenderIntentSample::camera(RenderGestureKind::Drag, camera(2.0)),
                100,
                true,
            )
            .unwrap();
        let after_three_d = mailbox.snapshot();
        assert_eq!(after_three_d.latest_revision, three_d);
        assert_eq!(after_three_d.three_d_revision, three_d);
        assert_eq!(after_three_d.linked_2d_revision, linked);

        let both = mailbox
            .observe_durable_intent(RenderIntentFamily::Both)
            .unwrap();
        let after_both = mailbox.snapshot();
        assert_eq!(after_both.latest_revision, both);
        assert_eq!(after_both.three_d_revision, both);
        assert_eq!(after_both.linked_2d_revision, both);
    }
}
