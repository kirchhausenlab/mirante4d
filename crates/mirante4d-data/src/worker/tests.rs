use mirante4d_core::{LayerId, TimeIndex};

use super::*;

#[test]
fn worker_queue_pops_highest_priority_before_fifo_within_priority() {
    let queue = WorkerQueue::new(8);

    queue
        .try_push(request(1, BrickRequestPriority::Warm))
        .unwrap();
    queue
        .try_push(request(2, BrickRequestPriority::Prefetch))
        .unwrap();
    queue
        .try_push(request(3, BrickRequestPriority::CurrentFrame))
        .unwrap();
    queue
        .try_push(request(4, BrickRequestPriority::CurrentFrame))
        .unwrap();

    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(3));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(4));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(2));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(1));
}

#[test]
fn worker_queue_orders_within_priority_by_queue_priority_then_fifo() {
    let queue = WorkerQueue::new(8);

    queue
        .try_push(request_with_queue_priority(
            1,
            BrickRequestPriority::CurrentFrame,
            10,
        ))
        .unwrap();
    queue
        .try_push(request_with_queue_priority(
            2,
            BrickRequestPriority::CurrentFrame,
            30,
        ))
        .unwrap();
    queue
        .try_push(request_with_queue_priority(
            3,
            BrickRequestPriority::CurrentFrame,
            30,
        ))
        .unwrap();
    queue
        .try_push(request_with_queue_priority(
            4,
            BrickRequestPriority::CurrentFrame,
            20,
        ))
        .unwrap();

    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(2));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(3));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(4));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(1));
}

#[test]
fn worker_queue_coarse_priority_beats_queue_priority() {
    let queue = WorkerQueue::new(8);

    queue
        .try_push(request_with_queue_priority(
            1,
            BrickRequestPriority::Prefetch,
            100,
        ))
        .unwrap();
    queue
        .try_push(request_with_queue_priority(
            2,
            BrickRequestPriority::CurrentFrame,
            1,
        ))
        .unwrap();

    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(2));
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(1));
}

#[test]
fn worker_queue_rejects_push_when_full() {
    let queue = WorkerQueue::new(1);

    queue
        .try_push(request(1, BrickRequestPriority::Warm))
        .unwrap();
    let err = queue
        .try_push(request(2, BrickRequestPriority::CurrentFrame))
        .unwrap_err();

    assert_eq!(err, QueuePushError::Full);
}

#[test]
fn worker_queue_diagnostics_report_priority_depths() {
    let queue = WorkerQueue::new(8);

    queue
        .try_push(request(1, BrickRequestPriority::Warm))
        .unwrap();
    queue
        .try_push(request(2, BrickRequestPriority::Prefetch))
        .unwrap();
    queue
        .try_push(request(3, BrickRequestPriority::CurrentFrame))
        .unwrap();

    assert_eq!(
        queue.diagnostics().unwrap(),
        BrickReadQueueDiagnostics {
            capacity: 8,
            queued_total: 3,
            queued_current_frame: 1,
            queued_prefetch: 1,
            queued_warm: 1,
            purged_stale_requests: 0,
            closed: false,
        }
    );
}

#[test]
fn brick_read_metrics_reports_data_engine_stats_delta() {
    let before = DataEngineStats {
        brick_cache_hits: 2,
        brick_cache_misses: 3,
        decoded_brick_values: 100,
        decoded_brick_bytes: 200,
        encoded_payload_bytes_read: 300,
        encoded_shard_payloads_read: 4,
        ..DataEngineStats::default()
    };
    let after = DataEngineStats {
        brick_cache_hits: 5,
        brick_cache_misses: 9,
        decoded_brick_values: 150,
        decoded_brick_bytes: 280,
        encoded_payload_bytes_read: 700,
        encoded_shard_payloads_read: 10,
        ..before
    };

    assert_eq!(
        BrickReadMetrics::from_stats_delta(before, after),
        BrickReadMetrics {
            brick_cache_hits: 3,
            brick_cache_misses: 6,
            decoded_brick_values: 50,
            decoded_brick_bytes: 80,
            encoded_payload_bytes_read: 400,
            encoded_shard_payloads_read: 6,
        }
    );
}

#[test]
fn worker_queue_purges_stale_generations_before_workers_pop_them() {
    let queue = WorkerQueue::new(8);
    let mut stale_current = request(1, BrickRequestPriority::CurrentFrame);
    stale_current.generation_id = DataGenerationId(1);
    let mut stale_prefetch = request(2, BrickRequestPriority::Prefetch);
    stale_prefetch.generation_id = DataGenerationId(2);
    let mut latest = request(3, BrickRequestPriority::Warm);
    latest.generation_id = DataGenerationId(3);
    let stale_current_cancel = stale_current.cancellation.clone();
    let stale_prefetch_cancel = stale_prefetch.cancellation.clone();

    queue.try_push(stale_current).unwrap();
    queue.try_push(stale_prefetch).unwrap();
    queue.try_push(latest).unwrap();

    assert_eq!(queue.purge_generations_before(DataGenerationId(3)), 2);
    assert!(stale_current_cancel.is_cancelled());
    assert!(stale_prefetch_cancel.is_cancelled());
    assert_eq!(queue.pop().unwrap().request_id, DataRequestId(3));
    assert_eq!(queue.diagnostics().unwrap().purged_stale_requests, 2);
}

#[test]
fn worker_queue_closes_pending_workers() {
    let queue = WorkerQueue::new(1);

    queue.close();

    assert!(queue.pop().is_none());
    assert_eq!(
        queue
            .try_push(request(1, BrickRequestPriority::CurrentFrame))
            .unwrap_err(),
        QueuePushError::Closed
    );
}

fn request(request_id: u64, priority: BrickRequestPriority) -> BrickReadRequest {
    request_with_queue_priority(request_id, priority, 0)
}

fn request_with_queue_priority(
    request_id: u64,
    priority: BrickRequestPriority,
    queue_priority: i64,
) -> BrickReadRequest {
    BrickReadRequest {
        request_id: DataRequestId(request_id),
        generation_id: DataGenerationId(0),
        layer_id: LayerId::new("ch0").unwrap(),
        scale_level: 0,
        timepoint: TimeIndex(0),
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        sample_region: None,
        coalesced_brick_indices: Vec::new(),
        priority,
        queue_priority,
        cancellation: CancellationToken::new(),
    }
}
