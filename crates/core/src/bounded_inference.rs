//! Non-blocking handoff from capture consumers to inference workers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

/// Counters shared by the capture consumer and status reporting.
#[derive(Debug, Default)]
pub(crate) struct QueueCounters {
    pub(crate) pending: AtomicU64,
    pub(crate) dropped: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueResult {
    Queued,
    DroppedFull,
    Disconnected,
}

/// Enqueue without ever blocking the capture consumer.
///
/// Overflow deliberately drops the newest item: queued utterances are older
/// and therefore closer to being processed, while accepting more work would
/// only grow live-transcript latency. A disconnected worker is not counted as
/// backlog overflow because no queue exists to be full.
pub(crate) fn try_send_drop_newest<T>(
    tx: &SyncSender<T>,
    item: T,
    counters: &QueueCounters,
) -> EnqueueResult {
    // Reserve before publication: a fast consumer may finish before try_send
    // returns. Incrementing afterwards lets its decrement wrap below zero.
    counters.pending.fetch_add(1, Ordering::Relaxed);
    match tx.try_send(item) {
        Ok(()) => EnqueueResult::Queued,
        Err(TrySendError::Full(_)) => {
            counters.pending.fetch_sub(1, Ordering::Relaxed);
            counters.dropped.fetch_add(1, Ordering::Relaxed);
            EnqueueResult::DroppedFull
        }
        Err(TrySendError::Disconnected(_)) => {
            counters.pending.fetch_sub(1, Ordering::Relaxed);
            EnqueueResult::Disconnected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn fast_worker_never_decrements_an_unpublished_reservation() {
        let (tx, rx) = mpsc::sync_channel::<u8>(3);
        let counters = Arc::new(QueueCounters::default());
        let worker_counters = Arc::clone(&counters);
        let worker = std::thread::spawn(move || {
            while rx.recv().is_ok() {
                let previous = worker_counters.pending.fetch_sub(1, Ordering::Relaxed);
                assert!(
                    previous > 0 && previous < 10,
                    "pending underflowed: {previous}"
                );
            }
        });
        for _ in 0..10_000 {
            try_send_drop_newest(&tx, 1, &counters);
        }
        drop(tx);
        worker.join().unwrap();
        assert_eq!(counters.pending.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn blocked_inference_worker_does_not_starve_capture_and_queue_stays_bounded() {
        const QUEUE_CAPACITY: usize = 3;
        const CAPTURE_CHUNKS: u64 = 10;

        let (inference_tx, inference_rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let release = Arc::new(Barrier::new(2));
        let worker_blocked = Arc::new(Barrier::new(2));
        let worker = {
            let release = Arc::clone(&release);
            let worker_blocked = Arc::clone(&worker_blocked);
            std::thread::spawn(move || {
                let _first: Vec<f32> = inference_rx.recv().unwrap();
                worker_blocked.wait();
                release.wait();
            })
        };

        let counters = Arc::new(QueueCounters::default());
        assert_eq!(
            try_send_drop_newest(&inference_tx, vec![0.25; 160], &counters),
            EnqueueResult::Queued
        );
        worker_blocked.wait();

        let (capture_tx, capture_rx) = mpsc::sync_channel::<Vec<f32>>(2);
        let consumed = Arc::new(AtomicU64::new(0));
        let (done_tx, done_rx) = mpsc::channel();
        let consumer = {
            let counters = Arc::clone(&counters);
            let consumed = Arc::clone(&consumed);
            std::thread::spawn(move || {
                while let Ok(chunk) = capture_rx.recv() {
                    consumed.fetch_add(1, Ordering::Relaxed);
                    try_send_drop_newest(&inference_tx, chunk, &counters);
                }
                done_tx.send(()).unwrap();
            })
        };

        let producer = std::thread::spawn(move || {
            for _ in 0..CAPTURE_CHUNKS {
                capture_tx.send(vec![0.5; 160]).unwrap();
            }
        });

        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("capture consumer blocked behind inference worker");
        assert_eq!(consumed.load(Ordering::Relaxed), CAPTURE_CHUNKS);
        assert_eq!(
            counters.pending.load(Ordering::Relaxed),
            (QUEUE_CAPACITY + 1) as u64,
            "one in flight plus the bounded queue"
        );
        assert_eq!(
            counters.dropped.load(Ordering::Relaxed),
            CAPTURE_CHUNKS - QUEUE_CAPACITY as u64,
            "overflow must drop newest and remain observable"
        );

        release.wait();
        producer.join().unwrap();
        worker.join().unwrap();
        consumer.join().unwrap();
    }
}
