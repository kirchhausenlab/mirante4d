//! Composition-root authority for one bounded temporal playback session.

use std::{collections::BTreeMap, sync::Arc};

use mirante4d_application::{PlaybackFps, SourceSessionGeneration};
use mirante4d_domain::{LogicalLayerKey, ScaleLevel, TimeIndex, ViewerLayout};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackTargetSet {
    ThreeD,
    FullLayout,
}

impl From<ViewerLayout> for PlaybackTargetSet {
    fn from(layout: ViewerLayout) -> Self {
        match layout {
            ViewerLayout::Single3d => Self::ThreeD,
            ViewerLayout::FourPanel => Self::FullLayout,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaybackSessionContract {
    generation: u64,
    source_generation: SourceSessionGeneration,
    fps: PlaybackFps,
    target_set: PlaybackTargetSet,
    layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
    slot_count: usize,
    startup_runway: usize,
    cpu_ceiling_bytes: u64,
    gpu_ceiling_bytes: u64,
}

/// Immutable identity shared by every prepared target in one temporal
/// presentation transaction.  Camera and linked-plane geometry are
/// intentionally absent: they are composed from the latest spatial intent
/// when this body is rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlaybackFrameContract {
    session_generation: u64,
    source_generation: SourceSessionGeneration,
    timepoint: TimeIndex,
    target_set: PlaybackTargetSet,
    layer_scales: Arc<BTreeMap<LogicalLayerKey, ScaleLevel>>,
}

impl PlaybackFrameContract {
    pub(crate) const fn session_generation(&self) -> u64 {
        self.session_generation
    }

    pub(crate) const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub(crate) const fn timepoint(&self) -> TimeIndex {
        self.timepoint
    }

    pub(crate) const fn target_set(&self) -> PlaybackTargetSet {
        self.target_set
    }

    pub(crate) fn layer_scales(&self) -> &Arc<BTreeMap<LogicalLayerKey, ScaleLevel>> {
        &self.layer_scales
    }
}

impl PlaybackSessionContract {
    #[allow(clippy::too_many_arguments)]
    fn new(
        generation: u64,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
        layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
        slot_count: usize,
        startup_runway: usize,
        cpu_ceiling_bytes: u64,
        gpu_ceiling_bytes: u64,
    ) -> Option<Self> {
        if layer_scales.is_empty()
            || slot_count == 0
            || startup_runway == 0
            || cpu_ceiling_bytes == 0
            || gpu_ceiling_bytes == 0
        {
            return None;
        }
        Some(Self {
            generation,
            source_generation,
            fps,
            target_set: layout.into(),
            layer_scales: Arc::new(layer_scales),
            slot_count,
            startup_runway: startup_runway.min(slot_count),
            cpu_ceiling_bytes,
            gpu_ceiling_bytes,
        })
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) const fn source_generation(&self) -> SourceSessionGeneration {
        self.source_generation
    }

    pub(crate) const fn fps(&self) -> PlaybackFps {
        self.fps
    }

    pub(crate) const fn target_set(&self) -> PlaybackTargetSet {
        self.target_set
    }

    pub(crate) fn layer_scales(&self) -> &Arc<BTreeMap<LogicalLayerKey, ScaleLevel>> {
        &self.layer_scales
    }

    pub(crate) const fn slot_count(&self) -> usize {
        self.slot_count
    }

    pub(crate) const fn startup_runway(&self) -> usize {
        self.startup_runway
    }

    #[cfg(test)]
    pub(crate) const fn cpu_ceiling_bytes(&self) -> u64 {
        self.cpu_ceiling_bytes
    }

    #[cfg(test)]
    pub(crate) const fn gpu_ceiling_bytes(&self) -> u64 {
        self.gpu_ceiling_bytes
    }

    pub(crate) fn frame_contract(&self, timepoint: TimeIndex) -> PlaybackFrameContract {
        PlaybackFrameContract {
            session_generation: self.generation,
            source_generation: self.source_generation,
            timepoint,
            target_set: self.target_set,
            layer_scales: Arc::clone(&self.layer_scales),
        }
    }

    pub(crate) fn admits_frame(&self, frame: &PlaybackFrameContract) -> bool {
        self.generation == frame.session_generation()
            && self.source_generation == frame.source_generation
            && self.target_set == frame.target_set
            && self.layer_scales.as_ref() == frame.layer_scales.as_ref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlaybackSlotState {
    Loading,
    Ready,
    Presented,
    Recyclable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackSlot {
    pub(crate) timepoint: TimeIndex,
    pub(crate) state: PlaybackSlotState,
}

#[derive(Debug)]
pub(crate) struct PreparedPlaybackSession {
    contract: PlaybackSessionContract,
    slots: Vec<PlaybackSlot>,
}

#[derive(Debug)]
enum PlaybackSessionState {
    Stopped,
    Warming {
        generation: u64,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
    },
    Contract {
        contract: PlaybackSessionContract,
        slots: Vec<PlaybackSlot>,
    },
}

#[derive(Debug)]
pub(crate) struct PlaybackSession {
    next_generation: u64,
    state: PlaybackSessionState,
}

impl PlaybackSession {
    pub(crate) const fn new() -> Self {
        Self {
            next_generation: 1,
            state: PlaybackSessionState::Stopped,
        }
    }

    pub(crate) fn stop(&mut self) {
        self.state = PlaybackSessionState::Stopped;
    }

    pub(crate) fn begin_warmup(
        &mut self,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
    ) {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.state = PlaybackSessionState::Warming {
            generation,
            source_generation,
            fps,
            layout,
        };
    }

    pub(crate) fn ensure_warmup(
        &mut self,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
    ) {
        let matches = match &self.state {
            PlaybackSessionState::Stopped => false,
            PlaybackSessionState::Warming {
                source_generation: current_source,
                fps: current_fps,
                layout: current_layout,
                ..
            } => {
                *current_source == source_generation
                    && *current_fps == fps
                    && *current_layout == layout
            }
            PlaybackSessionState::Contract { contract, .. } => {
                contract.source_generation() == source_generation
                    && contract.fps() == fps
                    && contract.target_set() == layout.into()
            }
        };
        if !matches {
            self.begin_warmup(source_generation, fps, layout);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_contract(
        &self,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
        layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
        slot_count: usize,
        startup_runway: usize,
        cpu_ceiling_bytes: u64,
        gpu_ceiling_bytes: u64,
        current: TimeIndex,
        future: &[TimeIndex],
    ) -> Option<PreparedPlaybackSession> {
        let PlaybackSessionState::Warming {
            generation,
            source_generation: expected_source,
            fps: expected_fps,
            layout: expected_layout,
        } = self.state
        else {
            return None;
        };
        if expected_source != source_generation || expected_fps != fps || expected_layout != layout
        {
            return None;
        }
        let contract = PlaybackSessionContract::new(
            generation,
            source_generation,
            fps,
            layout,
            layer_scales,
            slot_count,
            startup_runway,
            cpu_ceiling_bytes,
            gpu_ceiling_bytes,
        )?;
        let mut slots = Vec::with_capacity(contract.slot_count().saturating_add(1));
        slots.push(PlaybackSlot {
            timepoint: current,
            state: PlaybackSlotState::Presented,
        });
        slots.extend(
            future
                .iter()
                .take(contract.slot_count())
                .copied()
                .map(|timepoint| PlaybackSlot {
                    timepoint,
                    state: PlaybackSlotState::Loading,
                }),
        );
        Some(PreparedPlaybackSession { contract, slots })
    }

    /// Publishes a contract that was fully prepared while this exact warmup
    /// generation was current. No allocation or capacity decision occurs in
    /// this commit half.
    pub(crate) fn commit_prepared(&mut self, prepared: PreparedPlaybackSession) -> bool {
        let PlaybackSessionState::Warming { generation, .. } = self.state else {
            return false;
        };
        if generation != prepared.contract.generation() {
            return false;
        }
        self.state = PlaybackSessionState::Contract {
            contract: prepared.contract,
            slots: prepared.slots,
        };
        true
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit_contract(
        &mut self,
        source_generation: SourceSessionGeneration,
        fps: PlaybackFps,
        layout: ViewerLayout,
        layer_scales: BTreeMap<LogicalLayerKey, ScaleLevel>,
        slot_count: usize,
        startup_runway: usize,
        cpu_ceiling_bytes: u64,
        gpu_ceiling_bytes: u64,
        current: TimeIndex,
        future: &[TimeIndex],
    ) -> bool {
        let Some(prepared) = self.prepare_contract(
            source_generation,
            fps,
            layout,
            layer_scales,
            slot_count,
            startup_runway,
            cpu_ceiling_bytes,
            gpu_ceiling_bytes,
            current,
            future,
        ) else {
            return false;
        };
        self.commit_prepared(prepared)
    }

    pub(crate) fn contract(&self) -> Option<&PlaybackSessionContract> {
        match &self.state {
            PlaybackSessionState::Contract { contract, .. } => Some(contract),
            PlaybackSessionState::Stopped | PlaybackSessionState::Warming { .. } => None,
        }
    }

    /// The temporal cursor backed by the coherently published target front.
    /// This is intentionally distinct from the application's requested
    /// playback timepoint while a successor transaction is in flight.
    pub(crate) fn presented_timepoint(&self) -> Option<TimeIndex> {
        match &self.state {
            PlaybackSessionState::Contract { slots, .. } => slots
                .iter()
                .find(|slot| slot.state == PlaybackSlotState::Presented)
                .map(|slot| slot.timepoint),
            PlaybackSessionState::Stopped | PlaybackSessionState::Warming { .. } => None,
        }
    }

    /// Explicitly reserves the requested successor as temporal work. Spatial
    /// currentness and retained surface mismatch do not participate in this
    /// decision.
    pub(crate) fn pending_frame_contract(
        &self,
        requested: TimeIndex,
    ) -> Option<PlaybackFrameContract> {
        let contract = self.contract()?;
        (self.presented_timepoint() != Some(requested)).then(|| contract.frame_contract(requested))
    }

    #[cfg(test)]
    pub(crate) fn slots(&self) -> &[PlaybackSlot] {
        match &self.state {
            PlaybackSessionState::Contract { slots, .. } => slots,
            PlaybackSessionState::Stopped | PlaybackSessionState::Warming { .. } => &[],
        }
    }

    pub(crate) fn mark_ready(&mut self, timepoint: TimeIndex) {
        if let PlaybackSessionState::Contract { slots, .. } = &mut self.state
            && let Some(slot) = slots.iter_mut().find(|slot| slot.timepoint == timepoint)
            && slot.state == PlaybackSlotState::Loading
        {
            slot.state = PlaybackSlotState::Ready;
        }
    }

    pub(crate) fn mark_presented(&mut self, timepoint: TimeIndex, future: &[TimeIndex]) -> bool {
        let PlaybackSessionState::Contract { contract, slots } = &mut self.state else {
            return false;
        };
        if slots
            .iter()
            .any(|slot| slot.timepoint == timepoint && slot.state == PlaybackSlotState::Presented)
        {
            return true;
        }
        let Some(next_index) = slots
            .iter()
            .position(|slot| slot.state != PlaybackSlotState::Presented)
        else {
            return false;
        };
        if slots[next_index].timepoint != timepoint
            || slots[next_index].state != PlaybackSlotState::Ready
        {
            return false;
        }
        for slot in slots.iter_mut() {
            if slot.state == PlaybackSlotState::Presented && slot.timepoint != timepoint {
                slot.state = PlaybackSlotState::Recyclable;
            }
        }
        slots[next_index].state = PlaybackSlotState::Presented;
        slots.retain(|slot| slot.state != PlaybackSlotState::Recyclable);
        for next in future.iter().take(contract.slot_count()) {
            if !slots.iter().any(|slot| slot.timepoint == *next) {
                slots.push(PlaybackSlot {
                    timepoint: *next,
                    state: PlaybackSlotState::Loading,
                });
            }
        }
        let maximum = contract.slot_count().saturating_add(1);
        if slots.len() > maximum {
            slots.truncate(maximum);
        }
        true
    }

    pub(crate) fn observe_readiness(
        &mut self,
        current: TimeIndex,
        timepoint_count: u64,
        current_presented: bool,
        immediate_successor_ready: bool,
    ) {
        let Some(contract) = self.contract() else {
            return;
        };
        let slot_count = contract.slot_count();
        if timepoint_count <= 1 {
            return;
        }
        let successor = TimeIndex::new((current.get() + 1) % timepoint_count);
        if immediate_successor_ready {
            self.mark_ready(successor);
        }
        if current_presented {
            let future = (1..=slot_count)
                .map(|offset| {
                    TimeIndex::new(
                        (current.get() + u64::try_from(offset).unwrap_or(u64::MAX))
                            % timepoint_count,
                    )
                })
                .collect::<Vec<_>>();
            let _ = self.mark_presented(current, &future);
        }
    }
}

impl Default for PlaybackSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mirante4d_domain::LogicalLayerKey;

    #[test]
    fn admitted_contract_freezes_quality_and_bounds_slot_count() {
        let mut session = PlaybackSession::new();
        let fps = PlaybackFps::new(24).unwrap();
        session.begin_warmup(SourceSessionGeneration::new(7), fps, ViewerLayout::Single3d);
        let scales = BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::new(1))]);
        let future = (1..=24).map(TimeIndex::new).collect::<Vec<_>>();
        assert!(session.admit_contract(
            SourceSessionGeneration::new(7),
            fps,
            ViewerLayout::Single3d,
            scales.clone(),
            24,
            6,
            1024,
            2048,
            TimeIndex::new(0),
            &future,
        ));
        assert_eq!(session.contract().unwrap().layer_scales().as_ref(), &scales);
        assert_eq!(session.contract().unwrap().cpu_ceiling_bytes(), 1024);
        assert_eq!(session.contract().unwrap().gpu_ceiling_bytes(), 2048);
        assert_eq!(session.slots().len(), 25);
        assert!(!session.mark_presented(
            TimeIndex::new(2),
            &(3..=26).map(TimeIndex::new).collect::<Vec<_>>()
        ));
        session.mark_ready(TimeIndex::new(1));
        assert!(session.mark_presented(
            TimeIndex::new(1),
            &(2..=25).map(TimeIndex::new).collect::<Vec<_>>()
        ));
        assert_eq!(session.slots().len(), 25);
        assert_eq!(session.contract().unwrap().layer_scales().as_ref(), &scales);
    }

    #[test]
    fn stale_frames_and_prepared_contracts_cannot_cross_a_rewarm_generation() {
        let source = SourceSessionGeneration::new(7);
        let fps = PlaybackFps::new(24).unwrap();
        let scales = BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::new(1))]);
        let mut session = PlaybackSession::new();
        session.begin_warmup(source, fps, ViewerLayout::Single3d);
        let prepared = session
            .prepare_contract(
                source,
                fps,
                ViewerLayout::Single3d,
                scales.clone(),
                2,
                1,
                1024,
                2048,
                TimeIndex::new(0),
                &[TimeIndex::new(1), TimeIndex::new(2)],
            )
            .unwrap();

        session.begin_warmup(source, fps, ViewerLayout::FourPanel);
        assert!(!session.commit_prepared(prepared));
        assert!(session.contract().is_none());

        assert!(session.admit_contract(
            source,
            fps,
            ViewerLayout::FourPanel,
            scales,
            2,
            1,
            1024,
            2048,
            TimeIndex::new(0),
            &[TimeIndex::new(1), TimeIndex::new(2)],
        ));
        let contract = session.contract().unwrap();
        let admitted = contract.frame_contract(TimeIndex::new(1));
        assert!(contract.admits_frame(&admitted));

        let mut wrong_source = admitted.clone();
        wrong_source.source_generation = SourceSessionGeneration::new(8);
        assert!(!contract.admits_frame(&wrong_source));

        let mut wrong_scales = admitted;
        wrong_scales.layer_scales = Arc::new(BTreeMap::from([(
            LogicalLayerKey::new(0),
            ScaleLevel::new(2),
        )]));
        assert!(!contract.admits_frame(&wrong_scales));
    }

    #[test]
    fn a_contract_requires_real_resource_ceilings() {
        let mut session = PlaybackSession::new();
        let source = SourceSessionGeneration::new(1);
        let fps = PlaybackFps::new(24).unwrap();
        session.begin_warmup(source, fps, ViewerLayout::Single3d);
        assert!(!session.admit_contract(
            source,
            fps,
            ViewerLayout::Single3d,
            BTreeMap::from([(LogicalLayerKey::new(0), ScaleLevel::new(1))]),
            1,
            1,
            0,
            2048,
            TimeIndex::new(0),
            &[TimeIndex::new(1)],
        ));
    }
}
