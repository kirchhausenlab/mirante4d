//! Bounded latest-only construction of target-specific shader work proofs.
//!
//! Proof construction walks every participating layer/scale and performs
//! interval geometry. It therefore cannot live in the UI render turn. This
//! owner keeps exactly one worker, one replaceable request and one current
//! result for each of the four fixed presentation targets. Request identity
//! uses the render frame and immutable requirement allocation, so lookup and
//! replacement never walk a resource body.

use std::{
    fmt,
    mem::size_of,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

use mirante4d_dataset::{
    CpuByteLease, CpuByteLedger, CpuLedgerCategory, CpuLedgerError, DatasetCatalog,
    DatasetResourceIdentity,
};
use mirante4d_render_api::{
    FrameIdentity, LayerRenderIntent, PresentationTarget, PresentationViewport, RenderExtent,
    RenderIntent, RenderRequirements, RenderViewIntent, ShaderAdmissionError, ShaderLayerAffine,
    ShaderWorkEnvelope, SharedShaderWorkEnvelope, TimeIndex,
};

const WORKER_NAME: &str = "mirante4d-shader-envelope";

#[derive(Clone)]
struct WorkKey {
    target: PresentationTarget,
    frame: FrameIdentity,
    resource_identity: DatasetResourceIdentity,
    timepoint: TimeIndex,
    presentation: PresentationViewport,
    extent: RenderExtent,
    view: RenderViewIntent,
    layers: Box<[LayerRenderIntent]>,
    requirements: RenderRequirements,
}

impl WorkKey {
    fn new(
        target: PresentationTarget,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> Self {
        Self {
            target,
            frame: intent.frame(),
            resource_identity: intent.resource_identity(),
            timepoint: intent.timepoint(),
            presentation: intent.presentation(),
            extent: intent.extent(),
            view: intent.view(),
            layers: intent.layers().to_vec().into_boxed_slice(),
            requirements: requirements.clone(),
        }
    }
}

impl PartialEq for WorkKey {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.frame == other.frame
            && self.resource_identity == other.resource_identity
            && self.timepoint == other.timepoint
            && self.presentation == other.presentation
            && self.extent == other.extent
            && self.view == other.view
            && self.layers == other.layers
            && self.requirements.prefetch_promoted() == other.requirements.prefetch_promoted()
            && self.requirements.shares_resources_with(&other.requirements)
    }
}

impl Eq for WorkKey {}

struct WorkRequest {
    cache_generation: u64,
    key: WorkKey,
    catalog: Arc<DatasetCatalog>,
    intent: RenderIntent,
    requirements: RenderRequirements,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ShaderWorkEnvelopeBuildError {
    Admission(ShaderAdmissionError),
    Capacity(CpuLedgerError),
    WorkerPanicked,
}

impl fmt::Display for ShaderWorkEnvelopeBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Admission(error) => error.fmt(formatter),
            Self::Capacity(error) => write!(formatter, "shader proof cache: {error}"),
            Self::WorkerPanicked => formatter.write_str("shader work-envelope worker panicked"),
        }
    }
}

impl std::error::Error for ShaderWorkEnvelopeBuildError {}

struct WorkResult {
    key: WorkKey,
    outcome: Result<Arc<SharedShaderWorkEnvelope>, ShaderWorkEnvelopeBuildError>,
}

struct SharedState {
    shutdown: bool,
    pending: [Option<WorkRequest>; 4],
    results: [Option<WorkResult>; 4],
    running: Option<(u64, WorkKey)>,
    cache_generation: u64,
    next_target: usize,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            shutdown: false,
            pending: std::array::from_fn(|_| None),
            results: std::array::from_fn(|_| None),
            running: None,
            cache_generation: 1,
            next_target: 0,
        }
    }
}

struct Shared {
    state: Mutex<SharedState>,
    wake_worker: Condvar,
    wake_ui: Arc<dyn Fn() + Send + Sync>,
    cpu_ledger: Arc<dyn CpuByteLedger>,
    completed_result_pending: AtomicBool,
    affine_computations: AtomicU64,
    discarded_stale_results: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct ShaderWorkEnvelopeCacheDiagnostics {
    pub(crate) cache_generation: u64,
    pub(crate) pending_targets: usize,
    pub(crate) ready_or_failed_targets: usize,
    pub(crate) worker_running: bool,
    pub(crate) affine_computations: u64,
    pub(crate) discarded_stale_results: u64,
}

pub(crate) enum ShaderWorkEnvelopeLookup {
    Ready(Arc<SharedShaderWorkEnvelope>),
    Pending,
    Failed(ShaderWorkEnvelopeBuildError),
}

pub(crate) struct ShaderWorkEnvelopeCache {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl ShaderWorkEnvelopeCache {
    pub(crate) fn new(
        cpu_ledger: Arc<dyn CpuByteLedger>,
        wake_ui: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, std::io::Error> {
        let shared = Arc::new(Shared {
            state: Mutex::new(SharedState::default()),
            wake_worker: Condvar::new(),
            wake_ui: Arc::new(wake_ui),
            cpu_ledger,
            completed_result_pending: AtomicBool::new(false),
            affine_computations: AtomicU64::new(0),
            discarded_stale_results: AtomicU64::new(0),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name(WORKER_NAME.to_owned())
            .spawn(move || worker_main(worker_shared))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub(crate) fn resolve_or_submit(
        &self,
        target: PresentationTarget,
        catalog: Arc<DatasetCatalog>,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> ShaderWorkEnvelopeLookup {
        let key = WorkKey::new(target, intent, requirements);
        let index = target.index();
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(result) = state.results[index].as_ref()
            && result.key == key
        {
            return match &result.outcome {
                Ok(envelope) => ShaderWorkEnvelopeLookup::Ready(Arc::clone(envelope)),
                Err(error) => ShaderWorkEnvelopeLookup::Failed(*error),
            };
        }
        if state.running.as_ref().is_some_and(|(generation, running)| {
            *generation == state.cache_generation && *running == key
        }) || state.pending[index]
            .as_ref()
            .is_some_and(|pending| pending.key == key)
        {
            return ShaderWorkEnvelopeLookup::Pending;
        }

        state.results[index] = None;
        state.pending[index] = Some(WorkRequest {
            cache_generation: state.cache_generation,
            key,
            catalog,
            intent: intent.clone(),
            requirements: requirements.clone(),
        });
        self.shared.wake_worker.notify_one();
        ShaderWorkEnvelopeLookup::Pending
    }

    pub(crate) fn invalidate_all(&self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for pending in &mut state.pending {
            *pending = None;
        }
        for result in &mut state.results {
            *result = None;
        }
        state.cache_generation = state.cache_generation.saturating_add(1);
        self.shared.wake_worker.notify_one();
    }

    pub(crate) fn has_pending_work(&self) -> bool {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.running.is_some() || state.pending.iter().any(Option::is_some)
    }

    /// Consumes the coalesced fact that the worker published at least one
    /// current-generation result since the previous UI observation. The
    /// worker's callback only schedules the UI turn; this edge is what
    /// authorizes that turn to retry coordinated request construction.
    pub(crate) fn take_completed_result_wake(&self) -> bool {
        self.shared
            .completed_result_pending
            .swap(false, Ordering::AcqRel)
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> ShaderWorkEnvelopeCacheDiagnostics {
        let state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        ShaderWorkEnvelopeCacheDiagnostics {
            cache_generation: state.cache_generation,
            pending_targets: state
                .pending
                .iter()
                .filter(|request| request.is_some())
                .count(),
            ready_or_failed_targets: state
                .results
                .iter()
                .filter(|result| result.is_some())
                .count(),
            worker_running: state.running.is_some(),
            affine_computations: self.shared.affine_computations.load(Ordering::Acquire),
            discarded_stale_results: self.shared.discarded_stale_results.load(Ordering::Acquire),
        }
    }
}

struct CachedAffineSlot {
    layer: mirante4d_domain::LogicalLayerKey,
    scale: mirante4d_domain::ScaleLevel,
    outcome: Option<Result<ShaderLayerAffine, ShaderAdmissionError>>,
}

struct DatasetAffineCache {
    resource_identity: DatasetResourceIdentity,
    slots: Vec<CachedAffineSlot>,
    _charge: Arc<dyn CpuByteLease>,
}

impl DatasetAffineCache {
    fn new(
        catalog: &DatasetCatalog,
        cpu_ledger: &dyn CpuByteLedger,
    ) -> Result<Self, CpuLedgerError> {
        let slot_count = catalog
            .layers()
            .map(|layer| layer.scales().len())
            .sum::<usize>();
        let bytes = u64::try_from(slot_count)
            .unwrap_or(u64::MAX)
            .saturating_mul(size_of::<CachedAffineSlot>() as u64)
            .saturating_add(size_of::<Self>() as u64)
            .max(1);
        let charge: Arc<dyn CpuByteLease> =
            Arc::from(cpu_ledger.try_acquire(CpuLedgerCategory::MetadataAndIndexes, bytes)?);
        let mut slots = catalog
            .layers()
            .flat_map(|layer| {
                layer.scales().map(move |scale| CachedAffineSlot {
                    layer: layer.key(),
                    scale: scale.level(),
                    outcome: None,
                })
            })
            .collect::<Vec<_>>();
        slots.sort_unstable_by_key(|slot| (slot.layer, slot.scale));
        Ok(Self {
            resource_identity: catalog.resource_identity(),
            slots,
            _charge: charge,
        })
    }

    fn resolve(
        &mut self,
        catalog: &DatasetCatalog,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
        affine_computations: &AtomicU64,
    ) -> Result<Vec<ShaderLayerAffine>, ShaderWorkEnvelopeBuildError> {
        if self.resource_identity != catalog.resource_identity() {
            return Err(ShaderWorkEnvelopeBuildError::Admission(
                ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                    stage: mirante4d_render_api::ShaderEnvelopeStage::VolumeWorldToGrid,
                    axis: mirante4d_render_api::ShaderEnvelopeAxis::All,
                    reason: mirante4d_render_api::ShaderEnvelopeFailure::NonFiniteBound,
                },
            ));
        }
        let mut selected = Vec::new();
        for layer in intent.layers() {
            let chain = requirements.scale_chain(layer.layer()).ok_or({
                ShaderWorkEnvelopeBuildError::Admission(
                    ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                        stage: mirante4d_render_api::ShaderEnvelopeStage::VolumeWorldToGrid,
                        axis: mirante4d_render_api::ShaderEnvelopeAxis::All,
                        reason: mirante4d_render_api::ShaderEnvelopeFailure::NonFiniteBound,
                    },
                )
            })?;
            for scale in chain.scales().iter().copied() {
                let index = self
                    .slots
                    .binary_search_by_key(&(layer.layer(), scale), |slot| (slot.layer, slot.scale))
                    .map_err(|_| {
                        ShaderWorkEnvelopeBuildError::Admission(
                            ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                                stage: mirante4d_render_api::ShaderEnvelopeStage::VolumeWorldToGrid,
                                axis: mirante4d_render_api::ShaderEnvelopeAxis::All,
                                reason: mirante4d_render_api::ShaderEnvelopeFailure::NonFiniteBound,
                            },
                        )
                    })?;
                let slot = &mut self.slots[index];
                if slot.outcome.is_none() {
                    affine_computations.fetch_add(1, Ordering::Relaxed);
                }
                let outcome = *slot.outcome.get_or_insert_with(|| {
                    catalog
                        .layer(slot.layer)
                        .and_then(|catalog_layer| catalog_layer.scale(slot.scale))
                        .ok_or(ShaderAdmissionError::ShaderCoordinateEnvelopeExceeded {
                            stage: mirante4d_render_api::ShaderEnvelopeStage::VolumeWorldToGrid,
                            axis: mirante4d_render_api::ShaderEnvelopeAxis::All,
                            reason: mirante4d_render_api::ShaderEnvelopeFailure::NonFiniteBound,
                        })
                        .and_then(|catalog_scale| {
                            ShaderLayerAffine::new(
                                slot.layer,
                                slot.scale,
                                catalog_scale.grid_to_world(),
                                catalog_scale.shape(),
                            )
                        })
                });
                selected.push(outcome.map_err(ShaderWorkEnvelopeBuildError::Admission)?);
            }
        }
        Ok(selected)
    }
}

impl Drop for ShaderWorkEnvelopeCache {
    fn drop(&mut self) {
        {
            let mut state = self
                .shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.shutdown = true;
            for pending in &mut state.pending {
                *pending = None;
            }
        }
        self.shared.wake_worker.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_main(shared: Arc<Shared>) {
    let mut affine_cache = None::<DatasetAffineCache>;
    let mut observed_cache_generation = 0_u64;
    loop {
        let request = {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            loop {
                if state.shutdown {
                    return;
                }
                if state.cache_generation != observed_cache_generation {
                    observed_cache_generation = state.cache_generation;
                    affine_cache = None;
                }
                let request = (0..state.pending.len()).find_map(|offset| {
                    let index = (state.next_target + offset) % state.pending.len();
                    state.pending[index].take().map(|request| (index, request))
                });
                if let Some((index, request)) = request {
                    state.next_target = (index + 1) % state.pending.len();
                    state.running = Some((request.cache_generation, request.key.clone()));
                    break request;
                }
                state = shared
                    .wake_worker
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };

        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if affine_cache
                .as_ref()
                .is_none_or(|cache| cache.resource_identity != request.catalog.resource_identity())
            {
                affine_cache = Some(
                    DatasetAffineCache::new(request.catalog.as_ref(), shared.cpu_ledger.as_ref())
                        .map_err(ShaderWorkEnvelopeBuildError::Capacity)?,
                );
            }
            let affines = affine_cache
                .as_mut()
                .expect("the dataset affine cache was initialized")
                .resolve(
                    request.catalog.as_ref(),
                    &request.intent,
                    &request.requirements,
                    &shared.affine_computations,
                )?;
            ShaderWorkEnvelope::for_intent_with_validated_affines(
                request.catalog.as_ref(),
                &request.intent,
                &request.requirements,
                affines,
            )
            .map_err(ShaderWorkEnvelopeBuildError::Admission)
        }))
        .unwrap_or(Err(ShaderWorkEnvelopeBuildError::WorkerPanicked));

        let outcome = match built {
            Ok(envelope) => match shared.cpu_ledger.try_acquire(
                CpuLedgerCategory::QueuesAndResults,
                envelope.owned_allocation_bytes(),
            ) {
                Ok(charge) => Ok(Arc::new(SharedShaderWorkEnvelope::accounted(
                    envelope,
                    Arc::from(charge),
                ))),
                Err(error) => Err(ShaderWorkEnvelopeBuildError::Capacity(error)),
            },
            Err(error) => Err(error),
        };

        {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state.shutdown {
                return;
            }
            if request.cache_generation != state.cache_generation {
                shared
                    .discarded_stale_results
                    .fetch_add(1, Ordering::Relaxed);
                state.running = None;
                continue;
            }
            let index = request.key.target.index();
            state.results[index] = Some(WorkResult {
                key: request.key,
                outcome,
            });
            state.running = None;
        }
        shared
            .completed_result_pending
            .store(true, Ordering::Release);
        (shared.wake_ui)();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    use mirante4d_dataset::{
        BrickKey, ContentAddressStatus, DatasetLayer, DatasetSourceId, ResourceRegion,
        ResourceValidity,
    };
    use mirante4d_domain::{
        CameraView, DisplayWindow, GridToWorld, IntensityDType, IsoLightState, LayerTransfer,
        LogicalLayerKey, Opacity, Projection, RenderState, RgbColor, SamplingPolicy, Shape3D,
        Shape4D, TimeIndex, TransferCurve, UnitQuaternion, WorldPoint3,
    };
    use mirante4d_render_api::{RenderRequirement, RenderRequirementRole};

    use super::*;

    struct TestLease {
        category: CpuLedgerCategory,
        bytes: u64,
        active_result_bytes: Arc<AtomicU64>,
    }

    impl CpuByteLease for TestLease {
        fn category(&self) -> CpuLedgerCategory {
            self.category
        }

        fn reserved_bytes(&self) -> u64 {
            self.bytes
        }
    }

    impl Drop for TestLease {
        fn drop(&mut self) {
            if self.category == CpuLedgerCategory::QueuesAndResults {
                self.active_result_bytes
                    .fetch_sub(self.bytes, Ordering::AcqRel);
            }
        }
    }

    #[derive(Clone)]
    struct TestLedger {
        active_result_bytes: Arc<AtomicU64>,
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
            if category == CpuLedgerCategory::QueuesAndResults {
                self.active_result_bytes.fetch_add(bytes, Ordering::AcqRel);
            }
            Ok(Box::new(TestLease {
                category,
                bytes,
                active_result_bytes: Arc::clone(&self.active_result_bytes),
            }))
        }
    }

    fn fixture() -> (Arc<DatasetCatalog>, RenderIntent, RenderRequirements) {
        let layer = LogicalLayerKey::new(0);
        let shape = Shape3D::new(8, 8, 8).unwrap();
        let catalog = Arc::new(
            DatasetCatalog::new(
                "shader-envelope-cache",
                ContentAddressStatus::SessionLocal(DatasetSourceId::new(71)),
                vec![
                    DatasetLayer::new(
                        layer,
                        "signal",
                        Shape4D::new(1, shape.z(), shape.y(), shape.x()).unwrap(),
                        IntensityDType::Uint16,
                        GridToWorld::identity(),
                        ResourceValidity::AllValid,
                    )
                    .unwrap(),
                ],
            )
            .unwrap(),
        );
        let layer_intent = LayerRenderIntent::new(
            layer,
            LayerTransfer::new(
                DisplayWindow::new(0.0, 65_535.0).unwrap(),
                RgbColor::new([1.0, 1.0, 1.0]).unwrap(),
                Opacity::new(1.0).unwrap(),
                TransferCurve::linear(),
                false,
            ),
            RenderState::mip(SamplingPolicy::VoxelExact),
        );
        let camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(3.5, 3.5, 3.5).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            8.0,
            20.0,
        )
        .unwrap();
        let intent = RenderIntent::new(
            FrameIdentity::new(5),
            catalog.resource_identity(),
            TimeIndex::new(0),
            RenderViewIntent::volume(camera, IsoLightState::attached_camera()),
            PresentationViewport::new(8.0, 8.0).unwrap(),
            RenderExtent::new(8, 8).unwrap(),
            vec![layer_intent],
        )
        .unwrap();
        let key = BrickKey::new(
            catalog.resource_identity(),
            layer,
            TimeIndex::new(0),
            mirante4d_domain::ScaleLevel::BASE,
            ResourceRegion::new([0, 0, 0], shape).unwrap(),
        );
        let requirements = RenderRequirements::new(
            &intent,
            vec![RenderRequirement::new(
                key,
                RenderRequirementRole::FirstUsefulFrame,
            )],
        )
        .unwrap();
        (catalog, intent, requirements)
    }

    fn await_ready(
        cache: &ShaderWorkEnvelopeCache,
        wake: &mpsc::Receiver<()>,
        target: PresentationTarget,
        catalog: &Arc<DatasetCatalog>,
        intent: &RenderIntent,
        requirements: &RenderRequirements,
    ) -> Arc<SharedShaderWorkEnvelope> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match cache.resolve_or_submit(target, Arc::clone(catalog), intent, requirements) {
                ShaderWorkEnvelopeLookup::Ready(envelope) => return envelope,
                ShaderWorkEnvelopeLookup::Failed(error) => {
                    panic!("shader envelope construction failed: {error}")
                }
                ShaderWorkEnvelopeLookup::Pending => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "shader envelope worker timed out");
            let _ = wake.recv_timeout(remaining.min(Duration::from_millis(100)));
        }
    }

    #[test]
    fn affine_and_work_envelope_cache_is_semantically_keyed_and_bounded() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let active_result_bytes = Arc::new(AtomicU64::new(0));
        let cache = ShaderWorkEnvelopeCache::new(
            Arc::new(TestLedger {
                active_result_bytes: Arc::clone(&active_result_bytes),
            }),
            move || {
                let _ = wake_tx.send(());
            },
        )
        .unwrap();
        let (catalog, intent, requirements) = fixture();

        let first = await_ready(
            &cache,
            &wake_rx,
            PresentationTarget::ThreeD,
            &catalog,
            &intent,
            &requirements,
        );
        assert!(cache.take_completed_result_wake());
        assert!(!cache.take_completed_result_wake());
        let repeated = await_ready(
            &cache,
            &wake_rx,
            PresentationTarget::ThreeD,
            &catalog,
            &intent,
            &requirements,
        );
        assert!(Arc::ptr_eq(&first, &repeated));
        assert!(
            !cache.take_completed_result_wake(),
            "a cache hit cannot manufacture a worker-completion edge"
        );

        for target in [
            PresentationTarget::Xy,
            PresentationTarget::Xz,
            PresentationTarget::Yz,
        ] {
            let _ = await_ready(&cache, &wake_rx, target, &catalog, &intent, &requirements);
        }
        let diagnostics = cache.diagnostics();
        assert_eq!(diagnostics.ready_or_failed_targets, 4);
        assert_eq!(diagnostics.pending_targets, 0);
        assert_eq!(diagnostics.affine_computations, 1);
        assert!(cache.take_completed_result_wake());
        assert!(!cache.take_completed_result_wake());

        let changed_camera = CameraView::new(
            Projection::Orthographic,
            WorldPoint3::new(3.75, 3.5, 3.5).unwrap(),
            UnitQuaternion::identity(),
            1.0,
            8.0,
            20.0,
        )
        .unwrap();
        let changed_intent = RenderIntent::new(
            intent.frame(),
            intent.resource_identity(),
            intent.timepoint(),
            RenderViewIntent::volume(changed_camera, IsoLightState::attached_camera()),
            intent.presentation(),
            intent.extent(),
            intent.layers().to_vec(),
        )
        .unwrap();
        let changed_requirements = requirements.rebind(&changed_intent).unwrap();
        assert!(matches!(
            cache.resolve_or_submit(
                PresentationTarget::ThreeD,
                Arc::clone(&catalog),
                &changed_intent,
                &changed_requirements,
            ),
            ShaderWorkEnvelopeLookup::Pending
        ));
        let changed = await_ready(
            &cache,
            &wake_rx,
            PresentationTarget::ThreeD,
            &catalog,
            &changed_intent,
            &changed_requirements,
        );
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(cache.diagnostics().affine_computations, 1);
        assert!(cache.take_completed_result_wake());

        let externally_held_bytes = first
            .envelope()
            .owned_allocation_bytes()
            .saturating_add(changed.envelope().owned_allocation_bytes());
        let first_allocation = Arc::as_ptr(&first);
        let generation = cache.diagnostics().cache_generation;
        cache.invalidate_all();
        assert!(cache.diagnostics().cache_generation > generation);
        assert_eq!(cache.diagnostics().ready_or_failed_targets, 0);
        assert_eq!(
            active_result_bytes.load(Ordering::Acquire),
            externally_held_bytes,
            "cache invalidation must not release accounting for proofs retained by render intents"
        );
        drop(repeated);
        drop(first);
        drop(changed);
        assert_eq!(active_result_bytes.load(Ordering::Acquire), 0);
        let rebuilt = await_ready(
            &cache,
            &wake_rx,
            PresentationTarget::ThreeD,
            &catalog,
            &intent,
            &requirements,
        );
        assert!(!std::ptr::eq(first_allocation, Arc::as_ptr(&rebuilt)));
        assert_eq!(cache.diagnostics().affine_computations, 2);
        assert!(cache.diagnostics().ready_or_failed_targets <= 4);
        assert!(cache.take_completed_result_wake());

        let proved_intent = intent
            .clone()
            .with_shared_shader_work_envelope(Arc::clone(&rebuilt));
        assert_ne!(intent, proved_intent);
        assert!(intent.same_semantic_request(&proved_intent));
    }
}
