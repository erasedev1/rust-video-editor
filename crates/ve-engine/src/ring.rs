//! A single-producer, single-consumer ring buffer for audio samples.
//!
//! The audio device's callback runs on a real-time thread. Anything that can
//! block it — a mutex, an allocation, a system call — risks a priority
//! inversion that the user hears as a click. So the callback only ever copies
//! out of this ring, and all the decoding and mixing happens on an ordinary
//! thread that copies in.
//!
//! Correctness rests on there being exactly one producer and one consumer,
//! which [`AudioRing::split`] enforces by handing out one of each.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct Shared {
    /// One slot is always left empty so that a full ring is distinguishable
    /// from an empty one without a separate length counter.
    buffer: UnsafeCell<Box<[f32]>>,
    capacity: usize,
    read: AtomicUsize,
    write: AtomicUsize,
    /// Times the consumer wanted samples that were not there, which is the
    /// number the overlay reports as audio underruns.
    underruns: AtomicUsize,
}

// SAFETY: the buffer is only ever touched through a `Producer` or a `Consumer`.
// The producer writes strictly between `write` and `read`, the consumer reads
// strictly between `read` and `write`, and each publishes its index with a
// Release store that the other reads with an Acquire load. The two therefore
// never touch the same slot, and the index ordering gives the happens-before
// edge that makes each side's writes visible to the other.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// Creates a ring and returns its two ends.
pub struct AudioRing;

impl AudioRing {
    /// Builds a ring holding `capacity` samples and splits it into its two ends.
    pub fn split(capacity: usize) -> (Producer, Consumer) {
        // One extra slot for the full/empty distinction.
        let slots = capacity.max(1) + 1;
        let shared = Arc::new(Shared {
            buffer: UnsafeCell::new(vec![0.0f32; slots].into_boxed_slice()),
            capacity: slots,
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            underruns: AtomicUsize::new(0),
        });
        (Producer { shared: Arc::clone(&shared) }, Consumer { shared })
    }
}

/// The writing end. Lives on the mixing thread.
pub struct Producer {
    shared: Arc<Shared>,
}

impl Producer {
    /// Writes as many samples as fit, returning how many were taken.
    ///
    /// Never blocks and never overwrites unread samples: a full ring means the
    /// mixer is ahead of the device, which is exactly where it should be.
    pub fn push(&self, samples: &[f32]) -> usize {
        let write = self.shared.write.load(Ordering::Relaxed);
        let read = self.shared.read.load(Ordering::Acquire);
        let free = self.shared.free_from(read, write);
        let count = samples.len().min(free);

        // SAFETY: these `count` slots sit strictly between `write` and `read`,
        // so the consumer will not touch them until the Release store below
        // publishes the new write index.
        let buffer = unsafe { &mut *self.shared.buffer.get() };
        let mut cursor = write;
        for &sample in &samples[..count] {
            buffer[cursor] = sample;
            cursor = (cursor + 1) % self.shared.capacity;
        }

        self.shared.write.store(cursor, Ordering::Release);
        count
    }

    /// Samples that can be written without blocking.
    pub fn free(&self) -> usize {
        let write = self.shared.write.load(Ordering::Relaxed);
        let read = self.shared.read.load(Ordering::Acquire);
        self.shared.free_from(read, write)
    }

    /// Samples currently queued for the device.
    pub fn queued(&self) -> usize {
        self.shared.len()
    }

    pub fn capacity(&self) -> usize {
        self.shared.capacity - 1
    }
}

/// The reading end. Lives on the audio callback thread.
pub struct Consumer {
    shared: Arc<Shared>,
}

impl Consumer {
    /// Fills `out` with queued samples, padding any shortfall with silence.
    ///
    /// Returns how many real samples were available. Padding rather than
    /// returning short matters: the device always needs a full buffer, and
    /// silence is a far better underrun than stale audio repeated.
    pub fn fill(&self, out: &mut [f32]) -> usize {
        let read = self.shared.read.load(Ordering::Relaxed);
        let write = self.shared.write.load(Ordering::Acquire);
        let available = self.shared.len_from(read, write);
        let count = out.len().min(available);

        // SAFETY: these `count` slots sit strictly between `read` and `write`,
        // so the producer will not touch them until the Release store below.
        let buffer = unsafe { &*self.shared.buffer.get() };
        let mut cursor = read;
        for slot in out[..count].iter_mut() {
            *slot = buffer[cursor];
            cursor = (cursor + 1) % self.shared.capacity;
        }
        out[count..].fill(0.0);

        self.shared.read.store(cursor, Ordering::Release);
        if count < out.len() {
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        count
    }

    /// Samples ready to be read.
    pub fn available(&self) -> usize {
        self.shared.len()
    }

    pub fn underruns(&self) -> usize {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Discards everything queued, for a seek: the buffered audio belongs to a
    /// position the user has left.
    pub fn clear(&self) {
        let write = self.shared.write.load(Ordering::Acquire);
        self.shared.read.store(write, Ordering::Release);
    }
}

impl Shared {
    fn len(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        self.len_from(read, write)
    }

    fn len_from(&self, read: usize, write: usize) -> usize {
        (write + self.capacity - read) % self.capacity
    }

    fn free_from(&self, read: usize, write: usize) -> usize {
        // The reserved empty slot is what keeps full from looking like empty.
        self.capacity - 1 - self.len_from(read, write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_come_out_in_the_order_they_went_in() {
        let (producer, consumer) = AudioRing::split(16);
        assert_eq!(producer.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(consumer.available(), 3);

        let mut out = [0.0f32; 3];
        assert_eq!(consumer.fill(&mut out), 3);
        assert_eq!(out, [1.0, 2.0, 3.0]);
        assert_eq!(consumer.available(), 0);
    }

    #[test]
    fn a_full_ring_refuses_more_rather_than_overwriting() {
        let (producer, consumer) = AudioRing::split(4);
        assert_eq!(producer.capacity(), 4);
        assert_eq!(producer.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4, "only four fit");

        let mut out = [0.0f32; 4];
        consumer.fill(&mut out);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0], "the earliest samples must survive");
    }

    #[test]
    fn an_underrun_is_padded_with_silence_and_counted() {
        let (producer, consumer) = AudioRing::split(16);
        producer.push(&[1.0, 2.0]);

        let mut out = [9.0f32; 4];
        assert_eq!(consumer.fill(&mut out), 2, "only two real samples");
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0], "the shortfall must be silence");
        assert_eq!(consumer.underruns(), 1);
    }

    #[test]
    fn the_ring_wraps_correctly_over_many_cycles() {
        let (producer, consumer) = AudioRing::split(8);
        let mut expected = 0.0f32;
        let mut out = [0.0f32; 3];

        for _ in 0..100 {
            let batch = [expected, expected + 1.0, expected + 2.0];
            assert_eq!(producer.push(&batch), 3);
            assert_eq!(consumer.fill(&mut out), 3);
            assert_eq!(out, batch, "wrapped read/write diverged at {expected}");
            expected += 3.0;
        }
    }

    #[test]
    fn free_and_queued_agree_with_each_other() {
        let (producer, consumer) = AudioRing::split(8);
        assert_eq!(producer.free(), 8);
        assert_eq!(producer.queued(), 0);

        producer.push(&[0.0; 5]);
        assert_eq!(producer.queued(), 5);
        assert_eq!(producer.free(), 3);
        assert_eq!(consumer.available(), 5);

        let mut out = [0.0f32; 2];
        consumer.fill(&mut out);
        assert_eq!(producer.free(), 5);
    }

    #[test]
    fn clearing_discards_everything_queued() {
        let (producer, consumer) = AudioRing::split(8);
        producer.push(&[1.0, 2.0, 3.0]);
        consumer.clear();
        assert_eq!(consumer.available(), 0);
        assert_eq!(producer.free(), 8);

        // And the ring still works afterwards.
        producer.push(&[7.0]);
        let mut out = [0.0f32; 1];
        consumer.fill(&mut out);
        assert_eq!(out, [7.0]);
    }

    #[test]
    fn a_producer_and_consumer_on_separate_threads_transfer_every_sample() {
        const TOTAL: usize = 100_000;
        let (producer, consumer) = AudioRing::split(512);

        let writer = std::thread::spawn(move || {
            let mut sent = 0usize;
            while sent < TOTAL {
                let batch: Vec<f32> =
                    (sent..(sent + 64).min(TOTAL)).map(|i| i as f32).collect();
                let mut offset = 0;
                while offset < batch.len() {
                    let n = producer.push(&batch[offset..]);
                    if n == 0 {
                        std::thread::yield_now();
                    }
                    offset += n;
                }
                sent += batch.len();
            }
        });

        let mut received = 0usize;
        let mut buffer = [0.0f32; 37]; // a size that does not divide the batches
        while received < TOTAL {
            let n = consumer.fill(&mut buffer);
            for (i, &sample) in buffer[..n].iter().enumerate() {
                assert_eq!(
                    sample,
                    (received + i) as f32,
                    "sample {} arrived out of order",
                    received + i
                );
            }
            received += n;
            if n == 0 {
                std::thread::yield_now();
            }
        }
        writer.join().unwrap();
        assert_eq!(received, TOTAL);
    }

    #[test]
    fn a_zero_capacity_ring_degrades_to_one_slot_rather_than_panicking() {
        let (producer, consumer) = AudioRing::split(0);
        assert_eq!(producer.push(&[1.0, 2.0]), 1);
        let mut out = [0.0f32; 1];
        assert_eq!(consumer.fill(&mut out), 1);
        assert_eq!(out, [1.0]);
    }
}
