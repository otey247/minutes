//! Sample-clocked, nonblocking audio handoff to the optional recording sidecar.

#[cfg(any(test, all(feature = "whisper", feature = "streaming")))]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
#[cfg(any(test, all(feature = "whisper", feature = "streaming")))]
use std::time::Duration;
use std::time::Instant;

pub const FRAME_SAMPLES: usize = 1_600;

#[derive(Debug)]
pub struct SidecarAudio {
    pub samples: Vec<f32>,
    pub start_sample: u64,
    /// Time of the newest captured sample, before queueing or inference.
    pub captured_at: Instant,
    pub discontinuity: bool,
    reservation: Option<SampleReservation>,
}

#[derive(Debug)]
struct SampleBudget {
    pending: AtomicUsize,
    limit: usize,
}

#[derive(Debug)]
struct SampleReservation {
    budget: Arc<SampleBudget>,
    count: usize,
}

impl Drop for SampleReservation {
    fn drop(&mut self) {
        self.budget.pending.fetch_sub(self.count, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct SidecarSender {
    tx: mpsc::Sender<SidecarAudio>,
    next_sample: Arc<AtomicU64>,
    budget: Arc<SampleBudget>,
}

/// Capacity in 100 ms frames, independent of native callback size. Reserving
/// samples before publication bounds even the transport's variable-size packet
/// queue. Empty packets are never enqueued. A 200-frame budget holds 20 seconds
/// whether callbacks contain 10 ms, 100 ms, or irregular amounts of audio.
pub fn channel(capacity: usize) -> (SidecarSender, mpsc::Receiver<SidecarAudio>) {
    assert!(capacity > 0);
    let (tx, rx) = mpsc::channel();
    (
        SidecarSender {
            tx,
            next_sample: Arc::new(AtomicU64::new(0)),
            budget: Arc::new(SampleBudget {
                pending: AtomicUsize::new(0),
                limit: capacity.saturating_mul(FRAME_SAMPLES),
            }),
        },
        rx,
    )
}

impl SidecarSender {
    fn packet(&self, samples: Vec<f32>) -> SidecarAudio {
        let start_sample = self
            .next_sample
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
        SidecarAudio {
            samples,
            start_sample,
            captured_at: Instant::now(),
            discontinuity: false,
            reservation: None,
        }
    }

    fn reserve(&self, packet: &mut SidecarAudio) -> bool {
        let count = packet.samples.len();
        if self
            .budget
            .pending
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |pending| {
                pending
                    .checked_add(count)
                    .filter(|total| *total <= self.budget.limit)
            })
            .is_err()
        {
            return false;
        }
        packet.reservation = Some(SampleReservation {
            budget: Arc::clone(&self.budget),
            count,
        });
        true
    }

    pub fn try_send(&self, samples: Vec<f32>) -> Result<(), mpsc::TrySendError<SidecarAudio>> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut packet = self.packet(samples);
        if !self.reserve(&mut packet) {
            return Err(mpsc::TrySendError::Full(packet));
        }
        self.tx.send(packet).map_err(|mut error| {
            error.0.reservation.take();
            mpsc::TrySendError::Disconnected(error.0)
        })
    }

    /// File replay/tailing only; real-time capture always uses try_send.
    pub fn send(&self, samples: Vec<f32>) -> Result<(), mpsc::SendError<SidecarAudio>> {
        if samples.is_empty() {
            return Ok(());
        }
        let mut packet = self.packet(samples);
        if packet.samples.len() > self.budget.limit {
            return Err(mpsc::SendError(packet));
        }
        while !self.reserve(&mut packet) {
            // Only file replay/tailing may wait; capture uses try_send above.
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        self.tx.send(packet).map_err(|mut error| {
            error.0.reservation.take();
            error
        })
    }
}

/// Reframe on the consumer, never adding VAD or buffering work to capture.
#[derive(Default)]
#[cfg(any(test, all(feature = "whisper", feature = "streaming")))]
pub(crate) struct SidecarFrames {
    samples: VecDeque<f32>,
    start_sample: u64,
    expected_sample: u64,
    newest_at: Option<Instant>,
    discontinuity: bool,
    pub dropped_samples: u64,
}

#[cfg(any(test, all(feature = "whisper", feature = "streaming")))]
impl SidecarFrames {
    pub fn push(&mut self, packet: SidecarAudio) {
        if packet.start_sample != self.expected_sample {
            self.dropped_samples = self
                .dropped_samples
                .saturating_add(packet.start_sample.saturating_sub(self.expected_sample));
            self.samples.clear();
            self.discontinuity = true;
        }
        if self.samples.is_empty() {
            self.start_sample = packet.start_sample;
        }
        self.expected_sample = packet
            .start_sample
            .saturating_add(packet.samples.len() as u64);
        self.newest_at = Some(packet.captured_at);
        self.samples.extend(packet.samples);
    }

    pub fn take(&mut self, flush: bool) -> Option<SidecarAudio> {
        if self.samples.is_empty() || (!flush && self.samples.len() < FRAME_SAMPLES) {
            return None;
        }
        let count = self.samples.len().min(FRAME_SAMPLES);
        let samples = self.samples.drain(..count).collect();
        let captured_at = self
            .newest_at?
            .checked_sub(Duration::from_secs_f64(
                self.samples.len() as f64 / 16_000.0,
            ))
            .unwrap_or(self.newest_at?);
        let frame = SidecarAudio {
            samples,
            start_sample: self.start_sample,
            captured_at,
            discontinuity: std::mem::take(&mut self.discontinuity),
            reservation: None,
        };
        self.start_sample = self.start_sample.saturating_add(count as u64);
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_sizes_preserve_every_sample_and_the_source_clock() {
        for sizes in [&[160][..], &[1600], &[4096], &[71, 800, 3199, 160]] {
            let input: Vec<f32> = (0..32_123).map(|n| n as f32).collect();
            let mut frames = SidecarFrames::default();
            let mut output = Vec::new();
            let mut offset = 0;
            let mut index = 0;
            while offset < input.len() {
                let end = (offset + sizes[index % sizes.len()]).min(input.len());
                frames.push(SidecarAudio {
                    samples: input[offset..end].to_vec(),
                    start_sample: offset as u64,
                    captured_at: Instant::now(),
                    discontinuity: false,
                    reservation: None,
                });
                while let Some(frame) = frames.take(false) {
                    assert_eq!(frame.samples.len(), FRAME_SAMPLES);
                    assert_eq!(frame.start_sample, output.len() as u64);
                    output.extend(frame.samples);
                }
                offset = end;
                index += 1;
            }
            output.extend(frames.take(true).unwrap().samples);
            assert_eq!(input, output);
            assert_eq!(frames.dropped_samples, 0);
        }
    }

    #[test]
    fn capacity_is_audio_time_and_is_released_on_receive_or_disconnect() {
        for size in [160, 1600, 3200] {
            let (tx, rx) = channel(20);
            for _ in 0..32_000 / size {
                tx.try_send(vec![0.0; size]).unwrap();
            }
            assert!(matches!(
                tx.try_send(vec![0.0; size]),
                Err(mpsc::TrySendError::Full(_))
            ));
            assert_eq!(tx.budget.pending.load(Ordering::Relaxed), 32_000);
            drop(rx.recv().unwrap());
            tx.try_send(vec![0.0; size]).unwrap();
            drop(rx);
            assert_eq!(tx.budget.pending.load(Ordering::Relaxed), 0);
            let failed = tx.try_send(vec![0.0; size]);
            assert!(matches!(failed, Err(mpsc::TrySendError::Disconnected(_))));
            assert_eq!(tx.budget.pending.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn overflow_is_a_gap_and_never_splices_audio_across_it() {
        let (tx, rx) = channel(1);
        tx.try_send(vec![1.0; 800]).unwrap();
        assert!(tx.try_send(vec![2.0; 1600]).is_err());
        let mut frames = SidecarFrames::default();
        frames.push(rx.recv().unwrap());
        tx.try_send(vec![3.0; 1600]).unwrap();
        frames.push(rx.recv().unwrap());
        let frame = frames.take(false).unwrap();
        assert!(frame.discontinuity);
        assert_eq!(frame.start_sample, 2400);
        assert_eq!(frame.samples, vec![3.0; 1600]);
        assert_eq!(frames.dropped_samples, 1600);
    }
}
