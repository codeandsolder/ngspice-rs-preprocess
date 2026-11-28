//! Private deterministic queue for pending ALB register-word transitions.
//!
//! Deadlines are ordered exactly as IEEE total order; simultaneous writes
//! retain insertion order. Bit masks encode independent bit transitions
//! without allocating a timer for every bit of a wide register.

use std::{cmp::Ordering, collections::BTreeMap};

// ngspice can reach one mathematically identical instant by different
// floating-point paths: a comparator timestamp for the clock, versus an
// earlier reset comparator timestamp plus a COMB propagation delay.
// Their f64 results may differ by a few ULPs (one ULP in a real 9.40d
// collision oracle). Account only for accumulated IEEE-754 roundoff,
// NOT for an arbitrary physical-time interval: 2ps-separated clock and
// release events must remain distinguishable at nanosecond timescales.
fn deadline_within_boundary(event_time_s: f64, boundary_time_s: f64) -> bool {
    event_time_s <= boundary_time_s
        || (event_time_s.is_finite()
            && event_time_s - boundary_time_s
                <= 4.0 * f64::EPSILON * event_time_s.abs().max(boundary_time_s.abs()))
}

/// A previously scheduled timer may reconcile independently rounded
/// timestamps at the callback boundary. A new timer with a representable
/// positive delay starting at this very boundary is not due yet.
pub(super) fn event_due_at_boundary(deadline: f64, source_time: f64, boundary: f64) -> bool {
    deadline <= boundary || (source_time < boundary && deadline_within_boundary(deadline, boundary))
}

#[derive(Clone, Copy, Debug)]
struct Deadline(f64);

impl PartialEq for Deadline {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Deadline {}

impl PartialOrd for Deadline {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Deadline {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EventKind {
    Synchronous,
    Asynchronous,
}

/// One pending write to the selected bits of a register word. The deadline
/// belongs to its queue bucket; the source time is retained for HOLD and
/// MINCLOCK decisions, independently of propagation delay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RegisterEvent {
    pub(super) source_time_s: f64,
    pub(super) register: usize,
    pub(super) word: usize,
    pub(super) bits: u32,
    pub(super) value: u32,
    pub(super) kind: EventKind,
}

/// Owns the lifecycle and deterministic insertion order of register timers.
/// Vendor-specific cancellation decisions remain with the state machine.
#[derive(Debug, Default)]
pub(super) struct PendingRegisterEvents {
    deadlines: BTreeMap<Deadline, Vec<RegisterEvent>>,
    // One vacant allocation per engine, never part of semantic state.
    // Reuse the largest available batch; do not retain a pool proportional
    // to the number of previously outstanding deadlines.
    spare: Vec<RegisterEvent>,
}

impl Clone for PendingRegisterEvents {
    fn clone(&self) -> Self {
        Self {
            deadlines: self.deadlines.clone(),
            spare: Vec::new(),
        }
    }
}

fn recycle_batch(spare: &mut Vec<RegisterEvent>, mut batch: Vec<RegisterEvent>) {
    if batch.capacity() > spare.capacity() {
        batch.clear();
        *spare = batch;
    }
}

impl PendingRegisterEvents {
    pub(super) fn next_due_time(&self) -> Option<f64> {
        self.deadlines.first_key_value().map(|(time, _)| time.0)
    }

    /// Remove only the selected bits of writes to one register. Empty
    /// records and deadlines disappear immediately, including cancellations
    /// of an entire word. Retain the order of all surviving writes.
    pub(super) fn cancel_bits(
        &mut self,
        register: usize,
        mut selected: impl FnMut(&RegisterEvent) -> u32,
    ) {
        let spare = &mut self.spare;
        self.deadlines.retain(|_, batch| {
            batch.retain_mut(|event| {
                if event.register == register {
                    event.bits &= !selected(event);
                }
                event.bits != 0
            });
            if batch.is_empty() {
                recycle_batch(spare, std::mem::take(batch));
                false
            } else {
                true
            }
        });
    }

    pub(super) fn cancel_where(
        &mut self,
        register: usize,
        mut predicate: impl FnMut(&RegisterEvent) -> bool,
    ) {
        self.cancel_bits(
            register,
            |event| {
                if predicate(event) { event.bits } else { 0 }
            },
        );
    }

    pub(super) fn schedule_word(&mut self, time_s: f64, event: RegisterEvent) {
        if event.bits != 0 {
            self.deadlines
                .entry(Deadline(time_s))
                .or_insert_with(|| std::mem::take(&mut self.spare))
                .push(event);
        }
    }

    /// Remove the earliest eligible exact deadline batch, returning its
    /// effective dispatch time. A near-future timer admitted solely to
    /// reconcile numerical equality executes at the actual boundary.
    fn take_next_due(&mut self, until: f64) -> Option<(f64, Vec<RegisterEvent>)> {
        let (&first, _) = self.deadlines.first_key_value()?;
        if first.0 <= until {
            return self
                .deadlines
                .pop_first()
                .map(|(deadline, batch)| (deadline.0, batch));
        }

        // This slow path only examines the handful of representable keys
        // within the roundoff window. A newly scheduled positive delay may
        // share a bucket with an older timer, or precede another eligible
        // bucket. Leave those new writes pending rather than pulling their
        // causal descendants repeatedly back to the current callback.
        let deadline = self
            .deadlines
            .iter()
            .take_while(|(deadline, _)| deadline_within_boundary(deadline.0, until))
            .find(|(_, batch)| batch.iter().any(|event| event.source_time_s < until))
            .map(|(deadline, _)| *deadline)?;
        let batch = self.deadlines.get_mut(&deadline)?;
        // Move the existing allocation without partitioning when the
        // entire bucket is eligible. A mixed bucket must retain its newer
        // writes in their original order.
        if batch.iter().all(|event| event.source_time_s < until) {
            return self.deadlines.remove(&deadline).map(|batch| (until, batch));
        }
        let mut due = std::mem::take(&mut self.spare);
        due.extend(batch.extract_if(.., |event| event.source_time_s < until));
        Some((until, due))
    }

    /// Dispatch exactly one eligible batch and reclaim its allocation
    /// before callers evaluate dependent expressions or schedule new timers.
    pub(super) fn dispatch_next_due(
        &mut self,
        until: f64,
        mut apply: impl FnMut(RegisterEvent),
    ) -> Option<f64> {
        let (time, batch) = self.take_next_due(until)?;
        for &event in &batch {
            apply(event);
        }
        recycle_batch(&mut self.spare, batch);
        Some(time)
    }

    pub(super) fn has_due_at(&self, time_s: f64) -> bool {
        self.deadlines.contains_key(&Deadline(time_s))
    }

    pub(super) fn append_signature_at(&self, time_s: f64, signature: &mut Vec<u64>) {
        if let Some(batch) = self.deadlines.get(&Deadline(time_s)) {
            for event in batch {
                // Encode the same ordered bit transitions regardless of
                // how writes were grouped into word masks.
                let mut bits = event.bits;
                while bits != 0 {
                    let bit = bits.trailing_zeros();
                    bits &= bits - 1;
                    signature.extend([
                        event.register as u64,
                        event.word as u64,
                        u64::from(bit),
                        u64::from(event.value & (1_u32 << bit) != 0),
                        u64::from(event.kind == EventKind::Asynchronous),
                    ]);
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.deadlines
            .values()
            .flatten()
            .map(|event| event.bits.count_ones() as usize)
            .sum()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.deadlines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};

    fn write(register: usize, word: usize, bits: u32, value: u32) -> RegisterEvent {
        RegisterEvent {
            source_time_s: 0.0,
            register,
            word,
            bits,
            value,
            kind: EventKind::Synchronous,
        }
    }

    #[test]
    fn next_due_batch_accepts_only_roundoff_not_distinct_physical_times() -> Result<()> {
        // A clock comparator and a separately accumulated register timer can
        // disagree by one ULP even when they represent the same physical time.
        // A 2 ps earlier clock must still precede the timer.
        let clock = f64::from_bits(0x3e47_815a_2dc5_64cc);
        let timer = f64::from_bits(clock.to_bits() + 1);
        let mut events = PendingRegisterEvents::default();
        events.schedule_word(timer, write(0, 0, 1, 1));
        assert!(events.take_next_due(clock - 2e-12).is_none());
        assert_eq!(events.next_due_time(), Some(timer));
        assert!(
            events
                .take_next_due(f64::from_bits(clock.to_bits() - 16))
                .is_none()
        );
        let (due, batch) = events
            .take_next_due(clock)
            .context("one-ULP timer was not due")?;
        assert_eq!(due.to_bits(), clock.to_bits());
        assert_eq!(batch, [write(0, 0, 1, 1)]);
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn rounded_boundary_skips_new_positive_delays_without_losing_older_writes() -> Result<()> {
        let boundary = 10e-9_f64;
        let first = f64::from_bits(boundary.to_bits() + 1);
        let later = f64::from_bits(boundary.to_bits() + 2);
        let new_write = RegisterEvent {
            source_time_s: boundary,
            ..write(0, 0, 1, 1)
        };
        let old_write = write(1, 0, 1, 1);
        let another_old = write(2, 0, 1, 0);
        let mut queue = PendingRegisterEvents::default();
        queue.schedule_word(first, new_write);
        queue.schedule_word(later, old_write);
        queue.schedule_word(later, new_write);
        queue.schedule_word(later, another_old);
        let (dispatch, batch) = queue
            .take_next_due(boundary)
            .context("older timer was blocked")?;
        assert_eq!(dispatch.to_bits(), boundary.to_bits());
        assert_eq!(batch, [old_write, another_old]);
        assert!(queue.take_next_due(boundary).is_none());
        assert_eq!(queue.next_due_time(), Some(first));
        let (dispatch, batch) = queue
            .take_next_due(first)
            .context("missing positive-delay timer")?;
        assert_eq!(dispatch.to_bits(), first.to_bits());
        assert_eq!(batch, [new_write]);
        let (dispatch, batch) = queue
            .take_next_due(first)
            .context("later timer can now reconcile")?;
        assert_eq!(dispatch.to_bits(), first.to_bits());
        assert_eq!(batch, [new_write]);
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn same_timestamp_events_keep_fifo_order_and_canceled_deadlines_disappear() -> Result<()> {
        let mut events = PendingRegisterEvents::default();
        events.schedule_word(2.0, write(0, 0, 0b11, 0b01));
        events.schedule_word(1.0, write(0, 1, 1, 1));
        events.schedule_word(2.0, write(1, 0, 1, 1));
        events.schedule_word(2.0, write(0, 0, 1, 0));
        events.schedule_word(3.0, write(2, 0, 1, 0));
        events.cancel_where(0, |event| event.word == 1);
        assert_eq!(events.next_due_time(), Some(2.0));
        assert!(events.take_next_due(1.5).is_none());
        let (time_s, due) = events
            .take_next_due(2.0)
            .context("missing live transitions")?;
        assert_eq!(time_s.to_bits(), 2.0_f64.to_bits());
        assert_eq!(
            due,
            [
                write(0, 0, 0b11, 0b01),
                write(1, 0, 1, 1),
                write(0, 0, 1, 0)
            ]
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events.next_due_time(), Some(3.0));
        let (_, tail) = events
            .take_next_due(3.0)
            .context("missing future transition")?;
        assert_eq!(tail, [write(2, 0, 1, 0)]);
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn partial_bit_cancellation_preserves_other_registers_and_exact_signature() -> Result<()> {
        let mut events = PendingRegisterEvents::default();
        events.schedule_word(0.5, write(0, 0, 0, 0));
        events.schedule_word(1.0, write(0, 1, 0b1010, 0b1000));
        events.schedule_word(1.0, write(1, 1, 0b1010, 0b0010));
        events.schedule_word(2.0, write(0, 1, 0b0010, 0));
        events.cancel_bits(0, |_| 0b0010);
        assert_eq!(events.len(), 3);
        assert_eq!(events.next_due_time(), Some(1.0));
        assert!(!events.has_due_at(0.5));
        assert!(!events.has_due_at(2.0));
        let mut signature = Vec::new();
        events.append_signature_at(1.0, &mut signature);
        assert_eq!(signature, [0, 1, 3, 1, 0, 1, 1, 1, 1, 0, 1, 1, 3, 0, 0]);
        let (_, batch) = events.take_next_due(1.0).context("missing partial batch")?;
        assert_eq!(
            batch,
            [write(0, 1, 0b1000, 0b1000), write(1, 1, 0b1010, 0b0010)]
        );
        assert!(events.is_empty());
        Ok(())
    }

    #[test]
    fn deadline_keys_preserve_total_order_and_exact_zero_identity() -> Result<()> {
        let mut queue = PendingRegisterEvents::default();
        let earlier = -0.0_f64;
        let later = f64::from_bits(1);
        for (time, value) in [(later, 1), (0.0, 0), (earlier, 1)] {
            queue.schedule_word(time, write(0, 0, 1, value));
        }
        assert!(queue.has_due_at(earlier));
        assert!(queue.has_due_at(0.0));
        for expected in [earlier, 0.0, later] {
            let (due, batch) = queue
                .take_next_due(later)
                .context("missing exact deadline")?;
            assert_eq!(due.to_bits(), expected.to_bits());
            assert_eq!(batch.len(), 1);
        }
        assert!(queue.is_empty());
        Ok(())
    }

    #[test]
    fn batch_storage_is_recycled_after_dispatch_and_full_cancellation() {
        let mut queue = PendingRegisterEvents::default();
        for word in 0..256 {
            queue.schedule_word(1.0, write(0, word, 1, 1));
        }
        let capacity = queue.deadlines[&Deadline(1.0)].capacity();
        let mut next_word = 0;
        let time = queue.dispatch_next_due(1.0, |event| {
            assert_eq!(event, write(0, next_word, 1, 1));
            next_word += 1;
        });
        assert_eq!(time.map(f64::to_bits), Some(1.0_f64.to_bits()));
        assert_eq!(next_word, 256);
        assert!(queue.is_empty());
        assert_eq!(queue.spare.capacity(), capacity);
        assert_eq!(queue.spare, []);
        assert_eq!(queue.clone().spare.capacity(), 0);

        queue.schedule_word(2.0, write(0, 0, 1, 1));
        assert_eq!(queue.spare.capacity(), 0);
        assert_eq!(queue.deadlines[&Deadline(2.0)].capacity(), capacity);
        // Cloning a live deadline must keep its write, not spare capacity.
        let mut checkpoint = queue.clone();
        assert_eq!(checkpoint.len(), 1);
        assert_eq!(checkpoint.spare.capacity(), 0);
        queue.cancel_where(0, |_| true);
        assert!(queue.is_empty());
        assert_eq!(queue.spare.capacity(), capacity);
        assert_eq!(queue.spare, []);
        queue.schedule_word(3.0, write(1, 0, 0, 0));
        assert!(queue.is_empty());
        assert_eq!(queue.spare.capacity(), capacity);
        assert_eq!(
            checkpoint
                .dispatch_next_due(2.0, |event| {
                    assert_eq!(event, write(0, 0, 1, 1));
                })
                .map(f64::to_bits),
            Some(2.0_f64.to_bits())
        );
    }

    #[test]
    fn recycled_dispatch_preserves_mixed_boundary_fifo_and_pending_positive_delay() -> Result<()> {
        let boundary = 10e-9_f64;
        let deadline = f64::from_bits(boundary.to_bits() + 1);
        let old = write(0, 0, 3, 1);
        let new = RegisterEvent {
            source_time_s: boundary,
            ..write(0, 0, 3, 2)
        };
        let mut queue = PendingRegisterEvents::default();
        for word in 0..64 {
            queue.schedule_word(1e-9, write(1, word, 1, 1));
        }
        queue
            .dispatch_next_due(1e-9, |_| {})
            .context("missing warmup")?;
        queue.schedule_word(deadline, old);
        queue.schedule_word(deadline, new);
        queue.schedule_word(deadline, write(0, 0, 3, 0));
        let capacity = queue.deadlines[&Deadline(deadline)].capacity();

        let mut seen = Vec::new();
        assert_eq!(
            queue
                .dispatch_next_due(boundary, |event| seen.push(event))
                .map(f64::to_bits),
            Some(boundary.to_bits())
        );
        assert_eq!(seen, [old, write(0, 0, 3, 0)]);
        assert_eq!(
            queue.next_due_time().map(f64::to_bits),
            Some(deadline.to_bits())
        );
        assert!(queue.dispatch_next_due(boundary, |_| {}).is_none());
        assert_eq!(queue.deadlines[&Deadline(deadline)].capacity(), capacity);
        seen.clear();
        assert_eq!(
            queue
                .dispatch_next_due(deadline, |event| seen.push(event))
                .map(f64::to_bits),
            Some(deadline.to_bits())
        );
        assert_eq!(seen, [new]);
        assert!(queue.is_empty());
        assert_eq!(queue.spare.capacity(), capacity);
        Ok(())
    }

    fn bit_writes(event: RegisterEvent) -> impl Iterator<Item = RegisterEvent> {
        (0..32).filter_map(move |bit| {
            let bits = 1_u32 << bit;
            (event.bits & bits != 0).then_some(RegisterEvent { bits, ..event })
        })
    }

    #[test]
    fn compressed_deadline_queue_matches_flat_bit_reference() -> Result<()> {
        let mut queue = PendingRegisterEvents::default();
        let mut reference = Vec::<(f64, RegisterEvent)>::new();
        let mut seed = 0x8d_7462_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..2048 {
            let register = (next() % 5) as usize;
            let word = (next() % 4) as usize;
            let mask = next();
            let value = next();
            let deadline = f64::from(next() % 64);
            if next() & 3 != 0 {
                let event = RegisterEvent {
                    source_time_s: f64::from(next() % 16),
                    kind: if next() & 1 == 0 {
                        EventKind::Synchronous
                    } else {
                        EventKind::Asynchronous
                    },
                    ..write(register, word, mask, value)
                };
                queue.schedule_word(deadline, event);
                reference.extend(bit_writes(event).map(|event| (deadline, event)));
            } else {
                queue.cancel_bits(register, |event| if event.word == word { mask } else { 0 });
                reference.retain(|(_, event)| {
                    event.register != register || event.word != word || event.bits & mask == 0
                });
            }
            if next().is_multiple_of(8) {
                compare_next_batch(&mut queue, &mut reference)?;
            }
            assert_eq!(queue.len(), reference.len());
        }
        while !reference.is_empty() {
            compare_next_batch(&mut queue, &mut reference)?;
        }
        assert!(queue.is_empty());
        Ok(())
    }

    fn compare_next_batch(
        queue: &mut PendingRegisterEvents,
        reference: &mut Vec<(f64, RegisterEvent)>,
    ) -> Result<()> {
        let Some(expected_time) = reference
            .iter()
            .map(|(time, _)| *time)
            .min_by(f64::total_cmp)
        else {
            assert!(queue.is_empty());
            return Ok(());
        };
        let expected = reference
            .extract_if(.., |(time, _)| time.to_bits() == expected_time.to_bits())
            .map(|(_, event)| event)
            .collect::<Vec<_>>();
        let mut actual = Vec::new();
        let time = queue
            .dispatch_next_due(f64::MAX, |event| actual.extend(bit_writes(event)))
            .context("missing reference batch")?;
        assert_eq!(time.to_bits(), expected_time.to_bits());
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    #[ignore = "manual queue throughput profile"]
    fn pending_register_deadline_profile() {
        use std::time::Instant;

        // Compare a backlog of distinct deadlines and wide register captures
        // with a steady stream of small, promptly consumed batches.
        for (deadlines, rounds) in [(1_u32, 100_000), (16, 10_000), (4096, 2)] {
            let mut schedule_ns = 0;
            let mut drain_ns = 0;
            let mut total_bits = 0;
            for _ in 0..rounds {
                let mut events = PendingRegisterEvents::default();
                let start = Instant::now();
                for index in 0..deadlines {
                    let deadline = f64::from((index * 17) % deadlines + 1);
                    events.schedule_word(
                        deadline,
                        write(index as usize % 8, index as usize, u32::MAX, index),
                    );
                }
                events.cancel_bits(3, |_| 0x5555_5555);
                schedule_ns += start.elapsed().as_nanos();
                let start = Instant::now();
                while let Some(deadline) = events.dispatch_next_due(f64::MAX, |event| {
                    total_bits += std::hint::black_box(event).bits.count_ones() as usize;
                }) {
                    std::hint::black_box(deadline);
                }
                drain_ns += start.elapsed().as_nanos();
            }
            assert!(total_bits > 0);
            println!(
                "queue_profile deadlines={deadlines} rounds={rounds} bits={total_bits} schedule_ns={schedule_ns} drain_ns={drain_ns}"
            );
        }
    }
}
