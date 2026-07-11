use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use mirante4d_domain::{IntensityDType, TimeIndex};
use mirante4d_format::LayerId;

use crate::{
    DataEngineStats, DataError, DatasetHandle, SpatialBrickIndex, VolumeBrickF32, VolumeBrickU8,
    VolumeBrickU16, VolumeRegion,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataRequestId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataGenerationId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrickRequestPriority {
    CurrentFrame,
    Prefetch,
    Warm,
}

#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub struct BrickReadTicket {
    pub request_id: DataRequestId,
    pub generation_id: DataGenerationId,
    pub scale_level: u32,
    pub cancellation: CancellationToken,
}

#[derive(Debug)]
pub struct BrickReadOutcome {
    pub request_id: DataRequestId,
    pub generation_id: DataGenerationId,
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub brick_index: SpatialBrickIndex,
    pub sample_region: Option<VolumeRegion>,
    pub priority: BrickRequestPriority,
    pub read_metrics: BrickReadMetrics,
    pub histogram_sample: Option<BrickHistogramSample>,
    pub status: BrickReadStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrickHistogramSample {
    pub total_values: u64,
    pub finite_values: u64,
    pub min_value: f32,
    pub max_value: f32,
    pub samples: Vec<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrickReadMetrics {
    pub brick_cache_hits: u64,
    pub brick_cache_misses: u64,
    pub decoded_brick_values: u64,
    pub decoded_brick_bytes: u64,
    pub encoded_payload_bytes_read: u64,
    pub encoded_shard_payloads_read: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrickReadQueueDiagnostics {
    pub capacity: usize,
    pub queued_total: usize,
    pub queued_current_frame: usize,
    pub queued_prefetch: usize,
    pub queued_warm: usize,
    pub purged_stale_requests: u64,
    pub closed: bool,
}

#[derive(Debug, Clone)]
pub struct BrickReadSpec {
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub brick_index: SpatialBrickIndex,
    pub sample_region: Option<VolumeRegion>,
    pub coalesced_brick_indices: Vec<SpatialBrickIndex>,
    pub priority: BrickRequestPriority,
    pub queue_priority: i64,
    pub cancellation: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct CrossSectionChunkReadSpec {
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub brick_index: SpatialBrickIndex,
    pub priority: BrickRequestPriority,
    pub queue_priority: i64,
    pub cancellation: CancellationToken,
}

#[derive(Debug)]
pub enum BrickReadStatus {
    Completed(BrickReadPayload),
    Cancelled,
    Stale,
    Failed(String),
}

#[derive(Debug)]
pub enum BrickReadPayload {
    U8(Box<VolumeBrickU8>),
    U16(Box<VolumeBrickU16>),
    F32(Box<VolumeBrickF32>),
    Group(Vec<BrickReadPayload>),
}

const BRICK_HISTOGRAM_SAMPLE_LIMIT: usize = 4096;

#[derive(Debug, Clone)]
struct BrickReadRequest {
    request_id: DataRequestId,
    generation_id: DataGenerationId,
    layer_id: LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    sample_region: Option<VolumeRegion>,
    coalesced_brick_indices: Vec<SpatialBrickIndex>,
    priority: BrickRequestPriority,
    queue_priority: i64,
    cancellation: CancellationToken,
}

#[derive(Debug)]
struct WorkerQueue {
    capacity: usize,
    state: Mutex<WorkerQueueState>,
    available: Condvar,
}

#[derive(Debug)]
struct WorkerQueueState {
    closed: bool,
    current_frame: VecDeque<BrickReadRequest>,
    prefetch: VecDeque<BrickReadRequest>,
    warm: VecDeque<BrickReadRequest>,
    purged_stale_requests: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePushError {
    Full,
    Closed,
}

pub struct BrickReadPool {
    dataset: DatasetHandle,
    queue: Arc<WorkerQueue>,
    results: mpsc::Receiver<BrickReadOutcome>,
    next_request_id: AtomicU64,
    active_generation: Arc<AtomicU64>,
    worker_count: usize,
    workers: Vec<JoinHandle<()>>,
}

pub struct CrossSectionChunkReadPool {
    dataset: DatasetHandle,
    queue: Arc<WorkerQueue>,
    results: mpsc::Receiver<BrickReadOutcome>,
    next_request_id: AtomicU64,
    active_generation: Arc<AtomicU64>,
    worker_count: usize,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerQueue {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(WorkerQueueState {
                closed: false,
                current_frame: VecDeque::new(),
                prefetch: VecDeque::new(),
                warm: VecDeque::new(),
                purged_stale_requests: 0,
            }),
            available: Condvar::new(),
        }
    }

    fn try_push(&self, request: BrickReadRequest) -> Result<(), QueuePushError> {
        let mut state = self.state.lock().map_err(|_| QueuePushError::Closed)?;
        if state.closed {
            return Err(QueuePushError::Closed);
        }
        if state.len() >= self.capacity {
            return Err(QueuePushError::Full);
        }
        match request.priority {
            BrickRequestPriority::CurrentFrame => {
                push_ordered_by_queue_priority(&mut state.current_frame, request)
            }
            BrickRequestPriority::Prefetch => {
                push_ordered_by_queue_priority(&mut state.prefetch, request)
            }
            BrickRequestPriority::Warm => push_ordered_by_queue_priority(&mut state.warm, request),
        }
        self.available.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<BrickReadRequest> {
        let mut state = self.state.lock().ok()?;
        loop {
            if let Some(request) = state.pop_highest_priority() {
                return Some(request);
            }
            if state.closed {
                return None;
            }
            state = self.available.wait(state).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
        }
        self.available.notify_all();
    }

    fn purge_generations_before(&self, generation_id: DataGenerationId) -> usize {
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        state.purge_generations_before(generation_id)
    }

    fn diagnostics(&self) -> Result<BrickReadQueueDiagnostics, DataError> {
        let state = self.state.lock().map_err(|_| DataError::CachePoisoned)?;
        Ok(state.diagnostics(self.capacity))
    }
}

impl WorkerQueueState {
    fn len(&self) -> usize {
        self.current_frame.len() + self.prefetch.len() + self.warm.len()
    }

    fn diagnostics(&self, capacity: usize) -> BrickReadQueueDiagnostics {
        BrickReadQueueDiagnostics {
            capacity,
            queued_total: self.len(),
            queued_current_frame: self.current_frame.len(),
            queued_prefetch: self.prefetch.len(),
            queued_warm: self.warm.len(),
            purged_stale_requests: self.purged_stale_requests,
            closed: self.closed,
        }
    }

    fn pop_highest_priority(&mut self) -> Option<BrickReadRequest> {
        self.current_frame
            .pop_front()
            .or_else(|| self.prefetch.pop_front())
            .or_else(|| self.warm.pop_front())
    }

    fn purge_generations_before(&mut self, generation_id: DataGenerationId) -> usize {
        let purged = purge_stale_brick_requests(&mut self.current_frame, generation_id)
            + purge_stale_brick_requests(&mut self.prefetch, generation_id)
            + purge_stale_brick_requests(&mut self.warm, generation_id);
        self.purged_stale_requests = self.purged_stale_requests.saturating_add(purged as u64);
        purged
    }
}

fn purge_stale_brick_requests(
    queue: &mut VecDeque<BrickReadRequest>,
    generation_id: DataGenerationId,
) -> usize {
    let mut retained = VecDeque::with_capacity(queue.len());
    let mut purged = 0;
    for request in queue.drain(..) {
        if request.generation_id.0 < generation_id.0 {
            request.cancellation.cancel();
            purged += 1;
        } else {
            retained.push_back(request);
        }
    }
    *queue = retained;
    purged
}

fn push_ordered_by_queue_priority(
    queue: &mut VecDeque<BrickReadRequest>,
    request: BrickReadRequest,
) {
    let insert_at = queue
        .iter()
        .position(|existing| existing.queue_priority < request.queue_priority);
    match insert_at {
        Some(index) => queue.insert(index, request),
        None => queue.push_back(request),
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl BrickReadTicket {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl BrickReadMetrics {
    fn from_stats_delta(before: DataEngineStats, after: DataEngineStats) -> Self {
        Self {
            brick_cache_hits: after
                .brick_cache_hits
                .saturating_sub(before.brick_cache_hits),
            brick_cache_misses: after
                .brick_cache_misses
                .saturating_sub(before.brick_cache_misses),
            decoded_brick_values: after
                .decoded_brick_values
                .saturating_sub(before.decoded_brick_values),
            decoded_brick_bytes: after
                .decoded_brick_bytes
                .saturating_sub(before.decoded_brick_bytes),
            encoded_payload_bytes_read: after
                .encoded_payload_bytes_read
                .saturating_sub(before.encoded_payload_bytes_read),
            encoded_shard_payloads_read: after
                .encoded_shard_payloads_read
                .saturating_sub(before.encoded_shard_payloads_read),
        }
    }
}

impl BrickReadPool {
    pub fn new(
        dataset: DatasetHandle,
        worker_count: usize,
        queue_capacity: usize,
    ) -> Result<Self, DataError> {
        if worker_count == 0 || queue_capacity == 0 {
            return Err(DataError::InvalidWorkerConfig {
                workers: worker_count,
                queue_capacity,
            });
        }

        let queue = Arc::new(WorkerQueue::new(queue_capacity));
        let (result_sender, results) = mpsc::channel();
        let active_generation = Arc::new(AtomicU64::new(1));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            workers.push(spawn_worker(
                index,
                dataset.clone(),
                Arc::clone(&queue),
                result_sender.clone(),
                Arc::clone(&active_generation),
            ));
        }
        drop(result_sender);

        Ok(Self {
            dataset,
            queue,
            results,
            next_request_id: AtomicU64::new(1),
            active_generation,
            worker_count,
            workers,
        })
    }

    pub fn active_generation(&self) -> DataGenerationId {
        DataGenerationId(self.active_generation.load(Ordering::SeqCst))
    }

    pub fn queue_capacity(&self) -> usize {
        self.queue.capacity
    }

    pub fn queue_diagnostics(&self) -> Result<BrickReadQueueDiagnostics, DataError> {
        self.queue.diagnostics()
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    pub fn advance_generation(&self) -> DataGenerationId {
        let generation =
            DataGenerationId(self.active_generation.fetch_add(1, Ordering::SeqCst) + 1);
        self.queue.purge_generations_before(generation);
        generation
    }

    pub fn submit_brick(
        &self,
        layer_id: LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        priority: BrickRequestPriority,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_at_scale(layer_id, 0, timepoint, brick_index, priority)
    }

    pub fn submit_brick_at_scale(
        &self,
        layer_id: LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        priority: BrickRequestPriority,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_spec_for_generation(
            self.active_generation(),
            BrickReadSpec {
                layer_id,
                scale_level,
                timepoint,
                brick_index,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority,
                queue_priority: 0,
                cancellation: CancellationToken::new(),
            },
        )
    }

    pub fn submit_brick_with_token(
        &self,
        layer_id: LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        priority: BrickRequestPriority,
        cancellation: CancellationToken,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_at_scale_with_token(
            layer_id,
            0,
            timepoint,
            brick_index,
            priority,
            cancellation,
        )
    }

    pub fn submit_brick_at_scale_with_token(
        &self,
        layer_id: LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        priority: BrickRequestPriority,
        cancellation: CancellationToken,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_spec_for_generation(
            self.active_generation(),
            BrickReadSpec {
                layer_id,
                scale_level,
                timepoint,
                brick_index,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority,
                queue_priority: 0,
                cancellation,
            },
        )
    }

    pub fn submit_brick_for_generation(
        &self,
        generation_id: DataGenerationId,
        layer_id: LayerId,
        timepoint: TimeIndex,
        brick_index: SpatialBrickIndex,
        priority: BrickRequestPriority,
        cancellation: CancellationToken,
    ) -> Result<BrickReadTicket, DataError> {
        self.submit_brick_spec_for_generation(
            generation_id,
            BrickReadSpec {
                layer_id,
                scale_level: 0,
                timepoint,
                brick_index,
                sample_region: None,
                coalesced_brick_indices: Vec::new(),
                priority,
                queue_priority: 0,
                cancellation,
            },
        )
    }

    pub fn submit_brick_spec_for_generation(
        &self,
        generation_id: DataGenerationId,
        spec: BrickReadSpec,
    ) -> Result<BrickReadTicket, DataError> {
        let request_id = DataRequestId(self.next_request_id.fetch_add(1, Ordering::SeqCst));
        let request = BrickReadRequest {
            request_id,
            generation_id,
            layer_id: spec.layer_id,
            scale_level: spec.scale_level,
            timepoint: spec.timepoint,
            brick_index: spec.brick_index,
            sample_region: spec.sample_region,
            coalesced_brick_indices: spec.coalesced_brick_indices,
            priority: spec.priority,
            queue_priority: spec.queue_priority,
            cancellation: spec.cancellation.clone(),
        };
        match self.queue.try_push(request) {
            Ok(()) => {
                self.dataset.record_brick_request_queued()?;
                Ok(BrickReadTicket {
                    request_id,
                    generation_id,
                    scale_level: spec.scale_level,
                    cancellation: spec.cancellation,
                })
            }
            Err(QueuePushError::Full) => {
                self.dataset.record_brick_queue_full()?;
                Err(DataError::WorkerQueueFull)
            }
            Err(QueuePushError::Closed) => Err(DataError::WorkerQueueClosed),
        }
    }

    pub fn try_recv(&self) -> Option<BrickReadOutcome> {
        self.results.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<BrickReadOutcome> {
        self.results.recv_timeout(timeout).ok()
    }

    pub fn drain_ready(&self) -> Vec<BrickReadOutcome> {
        self.results.try_iter().collect()
    }

    pub fn drain_ready_limit(&self, limit: usize) -> Vec<BrickReadOutcome> {
        self.results.try_iter().take(limit).collect()
    }
}

impl CrossSectionChunkReadPool {
    pub fn new(
        dataset: DatasetHandle,
        worker_count: usize,
        queue_capacity: usize,
    ) -> Result<Self, DataError> {
        if worker_count == 0 || queue_capacity == 0 {
            return Err(DataError::InvalidWorkerConfig {
                workers: worker_count,
                queue_capacity,
            });
        }

        let queue = Arc::new(WorkerQueue::new(queue_capacity));
        let (result_sender, results) = mpsc::channel();
        let active_generation = Arc::new(AtomicU64::new(1));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            workers.push(spawn_cross_section_worker(
                index,
                dataset.clone(),
                Arc::clone(&queue),
                result_sender.clone(),
                Arc::clone(&active_generation),
            ));
        }
        drop(result_sender);

        Ok(Self {
            dataset,
            queue,
            results,
            next_request_id: AtomicU64::new(1),
            active_generation,
            worker_count,
            workers,
        })
    }

    pub fn active_generation(&self) -> DataGenerationId {
        DataGenerationId(self.active_generation.load(Ordering::SeqCst))
    }

    pub fn queue_capacity(&self) -> usize {
        self.queue.capacity
    }

    pub fn queue_diagnostics(&self) -> Result<BrickReadQueueDiagnostics, DataError> {
        self.queue.diagnostics()
    }

    pub fn worker_count(&self) -> usize {
        self.worker_count
    }

    pub fn advance_generation(&self) -> DataGenerationId {
        let generation =
            DataGenerationId(self.active_generation.fetch_add(1, Ordering::SeqCst) + 1);
        self.queue.purge_generations_before(generation);
        generation
    }

    pub fn submit_chunk_for_generation(
        &self,
        generation_id: DataGenerationId,
        spec: CrossSectionChunkReadSpec,
    ) -> Result<BrickReadTicket, DataError> {
        let request_id = DataRequestId(self.next_request_id.fetch_add(1, Ordering::SeqCst));
        let request = BrickReadRequest {
            request_id,
            generation_id,
            layer_id: spec.layer_id,
            scale_level: spec.scale_level,
            timepoint: spec.timepoint,
            brick_index: spec.brick_index,
            sample_region: None,
            coalesced_brick_indices: Vec::new(),
            priority: spec.priority,
            queue_priority: spec.queue_priority,
            cancellation: spec.cancellation.clone(),
        };
        match self.queue.try_push(request) {
            Ok(()) => {
                self.dataset.record_brick_request_queued()?;
                Ok(BrickReadTicket {
                    request_id,
                    generation_id,
                    scale_level: spec.scale_level,
                    cancellation: spec.cancellation,
                })
            }
            Err(QueuePushError::Full) => {
                self.dataset.record_brick_queue_full()?;
                Err(DataError::WorkerQueueFull)
            }
            Err(QueuePushError::Closed) => Err(DataError::WorkerQueueClosed),
        }
    }

    pub fn try_recv(&self) -> Option<BrickReadOutcome> {
        self.results.try_recv().ok()
    }

    pub fn drain_ready_limit(&self, limit: usize) -> Vec<BrickReadOutcome> {
        self.results.try_iter().take(limit).collect()
    }
}

impl Drop for CrossSectionChunkReadPool {
    fn drop(&mut self) {
        self.queue.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for BrickReadPool {
    fn drop(&mut self) {
        self.queue.close();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn spawn_worker(
    index: usize,
    dataset: DatasetHandle,
    queue: Arc<WorkerQueue>,
    result_sender: mpsc::Sender<BrickReadOutcome>,
    active_generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mirante4d-data-brick-{index}"))
        .spawn(move || {
            while let Some(request) = queue.pop() {
                let outcome = process_request(&dataset, &active_generation, request);
                if result_sender.send(outcome).is_err() {
                    break;
                }
            }
        })
        .expect("data worker thread should spawn")
}

fn spawn_cross_section_worker(
    index: usize,
    dataset: DatasetHandle,
    queue: Arc<WorkerQueue>,
    result_sender: mpsc::Sender<BrickReadOutcome>,
    active_generation: Arc<AtomicU64>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name(format!("mirante4d-data-cross-section-{index}"))
        .spawn(move || {
            while let Some(request) = queue.pop() {
                let outcome =
                    process_request_with_histogram(&dataset, &active_generation, request, false);
                if result_sender.send(outcome).is_err() {
                    break;
                }
            }
        })
        .expect("cross-section data worker thread should spawn")
}

fn process_request(
    dataset: &DatasetHandle,
    active_generation: &AtomicU64,
    request: BrickReadRequest,
) -> BrickReadOutcome {
    process_request_with_histogram(dataset, active_generation, request, true)
}

fn process_request_with_histogram(
    dataset: &DatasetHandle,
    active_generation: &AtomicU64,
    request: BrickReadRequest,
    include_histogram: bool,
) -> BrickReadOutcome {
    if request.cancellation.is_cancelled() {
        let _ = dataset.record_brick_request_cancelled();
        return outcome(request, BrickReadStatus::Cancelled);
    }
    if is_stale(active_generation, request.generation_id) {
        let _ = dataset.record_brick_request_stale();
        return outcome(request, BrickReadStatus::Stale);
    }

    let is_coalesced_request =
        request.sample_region.is_none() && !request.coalesced_brick_indices.is_empty();
    let stats_before = dataset.stats().ok();
    let read = match dataset
        .layer(&request.layer_id)
        .map(|layer| layer.dtype.stored)
        .ok_or_else(|| DataError::LayerNotFound(request.layer_id.to_string()))
    {
        Ok(IntensityDType::Float32) if is_coalesced_request => dataset
            .read_f32_brick_group_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                &request.coalesced_brick_indices,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|bricks| {
                bricks.map(|bricks| {
                    BrickReadPayload::Group(
                        bricks
                            .into_iter()
                            .map(|brick| BrickReadPayload::F32(Box::new(brick)))
                            .collect(),
                    )
                })
            }),
        Ok(IntensityDType::Float32) if let Some(region) = request.sample_region => dataset
            .read_f32_brick_region_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                region,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::F32(Box::new(brick)))),
        Ok(IntensityDType::Float32) => dataset
            .read_f32_brick_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::F32(Box::new(brick)))),
        Ok(IntensityDType::Uint8) if is_coalesced_request => dataset
            .read_u8_brick_group_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                &request.coalesced_brick_indices,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|bricks| {
                bricks.map(|bricks| {
                    BrickReadPayload::Group(
                        bricks
                            .into_iter()
                            .map(|brick| BrickReadPayload::U8(Box::new(brick)))
                            .collect(),
                    )
                })
            }),
        Ok(IntensityDType::Uint8) if let Some(region) = request.sample_region => dataset
            .read_u8_brick_region_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                region,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::U8(Box::new(brick)))),
        Ok(IntensityDType::Uint8) => dataset
            .read_u8_brick_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::U8(Box::new(brick)))),
        Ok(IntensityDType::Uint16) if is_coalesced_request => dataset
            .read_u16_brick_group_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                &request.coalesced_brick_indices,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|bricks| {
                bricks.map(|bricks| {
                    BrickReadPayload::Group(
                        bricks
                            .into_iter()
                            .map(|brick| BrickReadPayload::U16(Box::new(brick)))
                            .collect(),
                    )
                })
            }),
        Ok(IntensityDType::Uint16) if let Some(region) = request.sample_region => dataset
            .read_u16_brick_region_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                region,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::U16(Box::new(brick)))),
        Ok(IntensityDType::Uint16) => dataset
            .read_u16_brick_at_scale_cancellable(
                &request.layer_id,
                request.scale_level,
                request.timepoint,
                request.brick_index,
                || {
                    request.cancellation.is_cancelled()
                        || is_stale(active_generation, request.generation_id)
                },
            )
            .map(|brick| brick.map(|brick| BrickReadPayload::U16(Box::new(brick)))),
        Err(err) => Err(err),
    };
    let read_metrics = stats_before
        .and_then(|before| {
            dataset
                .stats()
                .ok()
                .map(|after| BrickReadMetrics::from_stats_delta(before, after))
        })
        .unwrap_or_default();

    match read {
        Ok(Some(payload)) => {
            let _ = dataset.record_brick_request_completed();
            let histogram_sample = include_histogram
                .then(|| brick_histogram_sample_for_payload(&payload))
                .flatten();
            let status = BrickReadStatus::Completed(payload);
            outcome_with_metrics_and_histogram(request, status, read_metrics, histogram_sample)
        }
        Ok(None) if request.cancellation.is_cancelled() => {
            let _ = dataset.record_brick_request_cancelled();
            outcome_with_metrics(request, BrickReadStatus::Cancelled, read_metrics)
        }
        Ok(None) => {
            let _ = dataset.record_brick_request_stale();
            outcome_with_metrics(request, BrickReadStatus::Stale, read_metrics)
        }
        Err(err) => {
            let _ = dataset.record_brick_request_failed();
            outcome_with_metrics(
                request,
                BrickReadStatus::Failed(err.to_string()),
                read_metrics,
            )
        }
    }
}

fn is_stale(active_generation: &AtomicU64, generation_id: DataGenerationId) -> bool {
    active_generation.load(Ordering::SeqCst) != generation_id.0
}

fn outcome(request: BrickReadRequest, status: BrickReadStatus) -> BrickReadOutcome {
    outcome_with_metrics(request, status, BrickReadMetrics::default())
}

fn outcome_with_metrics(
    request: BrickReadRequest,
    status: BrickReadStatus,
    read_metrics: BrickReadMetrics,
) -> BrickReadOutcome {
    outcome_with_metrics_and_histogram(request, status, read_metrics, None)
}

fn outcome_with_metrics_and_histogram(
    request: BrickReadRequest,
    status: BrickReadStatus,
    read_metrics: BrickReadMetrics,
    histogram_sample: Option<BrickHistogramSample>,
) -> BrickReadOutcome {
    BrickReadOutcome {
        request_id: request.request_id,
        generation_id: request.generation_id,
        layer_id: request.layer_id,
        scale_level: request.scale_level,
        timepoint: request.timepoint,
        brick_index: request.brick_index,
        sample_region: request.sample_region,
        priority: request.priority,
        read_metrics,
        histogram_sample,
        status,
    }
}

fn brick_histogram_sample_for_payload(payload: &BrickReadPayload) -> Option<BrickHistogramSample> {
    match payload {
        BrickReadPayload::U8(brick) => histogram_sample_from_u8_values(brick.values()),
        BrickReadPayload::U16(brick) => histogram_sample_from_u16_values(brick.values()),
        BrickReadPayload::F32(brick) => histogram_sample_from_f32_values(brick.values()),
        BrickReadPayload::Group(payloads) => combine_brick_histogram_samples(
            payloads
                .iter()
                .filter_map(brick_histogram_sample_for_payload),
        ),
    }
}

fn histogram_sample_from_u8_values(values: &[u8]) -> Option<BrickHistogramSample> {
    histogram_sample_from_values(
        values.len() as u64,
        values.iter().map(|value| f32::from(*value)),
    )
}

fn histogram_sample_from_u16_values(values: &[u16]) -> Option<BrickHistogramSample> {
    histogram_sample_from_values(
        values.len() as u64,
        values.iter().map(|value| f32::from(*value)),
    )
}

fn histogram_sample_from_f32_values(values: &[f32]) -> Option<BrickHistogramSample> {
    histogram_sample_from_values(values.len() as u64, values.iter().copied())
}

fn histogram_sample_from_values(
    total_values: u64,
    values: impl IntoIterator<Item = f32>,
) -> Option<BrickHistogramSample> {
    if total_values == 0 {
        return None;
    }
    let mut finite_values = 0_u64;
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut all_finite = Vec::new();
    for value in values {
        if value.is_finite() {
            finite_values += 1;
            min_value = min_value.min(value);
            max_value = max_value.max(value);
            all_finite.push(value);
        }
    }
    if finite_values == 0 {
        return None;
    }
    let samples = downsample_finite_values(all_finite, BRICK_HISTOGRAM_SAMPLE_LIMIT);
    Some(BrickHistogramSample {
        total_values,
        finite_values,
        min_value,
        max_value,
        samples,
    })
}

fn combine_brick_histogram_samples(
    samples: impl IntoIterator<Item = BrickHistogramSample>,
) -> Option<BrickHistogramSample> {
    let mut total_values = 0_u64;
    let mut finite_values = 0_u64;
    let mut min_value = f32::INFINITY;
    let mut max_value = f32::NEG_INFINITY;
    let mut combined = Vec::new();
    for sample in samples {
        total_values = total_values.saturating_add(sample.total_values);
        finite_values = finite_values.saturating_add(sample.finite_values);
        min_value = min_value.min(sample.min_value);
        max_value = max_value.max(sample.max_value);
        combined.extend(sample.samples);
    }
    if finite_values == 0 {
        return None;
    }
    Some(BrickHistogramSample {
        total_values,
        finite_values,
        min_value,
        max_value,
        samples: downsample_finite_values(combined, BRICK_HISTOGRAM_SAMPLE_LIMIT),
    })
}

fn downsample_finite_values(values: Vec<f32>, limit: usize) -> Vec<f32> {
    if values.len() <= limit {
        return values;
    }
    let stride = values.len().div_ceil(limit).max(1);
    values.into_iter().step_by(stride).take(limit).collect()
}

#[cfg(test)]
mod tests;
