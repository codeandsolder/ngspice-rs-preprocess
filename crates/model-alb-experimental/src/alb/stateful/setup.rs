//! LEVEL SETUP timing history, specialized by the resolved timing law.
//!
//! Only active setup windows need historical data-change timestamps. For
//! whole-word rejection the latest change in any bit is sufficient; for
//! bitwise rejection, the per-bit timestamps are contiguous across words.

#[derive(Clone, Debug)]
pub(super) enum SetupHistory {
    Disabled,
    WholeWord(Vec<f64>),
    Bitwise { width: usize, timestamps: Vec<f64> },
}

impl SetupHistory {
    pub(super) fn new(word_count: usize, width: usize, bitwise: bool, setup: f64) -> Self {
        if setup == 0.0 {
            Self::Disabled
        } else if bitwise {
            Self::Bitwise {
                width,
                timestamps: vec![f64::NEG_INFINITY; word_count * width],
            }
        } else {
            Self::WholeWord(vec![f64::NEG_INFINITY; word_count])
        }
    }

    /// Record changes in scheduler dispatch order. Input samples and
    /// internal commits never move backwards in time. Whole-word rejection
    /// can therefore use the latest timestamp instead of all bit timestamps.
    pub(super) fn record(&mut self, word: usize, changed: u32, time_s: f64) {
        if changed == 0 {
            return;
        }
        match self {
            Self::Disabled => {}
            Self::WholeWord(timestamps) => timestamps[word] = time_s,
            Self::Bitwise { width, timestamps } => {
                let offset = word * *width;
                let mut remaining = changed;
                while remaining != 0 {
                    let bit = remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    timestamps[offset + bit] = time_s;
                }
            }
        }
    }

    pub(super) fn reject_bits(&self, word: usize, mask: u32, time_s: f64, setup: f64) -> u32 {
        // Preserve the vendor's strict lower and inclusive upper boundary.
        let in_window = |last: f64| last > time_s - setup && last <= time_s;
        match self {
            Self::Disabled => 0,
            Self::WholeWord(timestamps) => {
                if in_window(timestamps[word]) {
                    mask
                } else {
                    0
                }
            }
            Self::Bitwise { width, timestamps } => {
                let offset = word * *width;
                let mut remaining = mask;
                let mut rejected = 0;
                while remaining != 0 {
                    let bit = remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    if in_window(timestamps[offset + bit]) {
                        rejected |= 1_u32 << bit;
                    }
                }
                rejected
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_setup_modes_match_per_bit_reference_across_words_and_boundaries() {
        for width in [1, 8, 32] {
            let valid = u32::MAX >> (32 - width);
            let mut seed = 0x9b87_2a42_u32;
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            let mut whole = SetupHistory::new(3, width, false, 3.0);
            let mut bitwise = SetupHistory::new(3, width, true, 3.0);
            let mut reference = vec![vec![f64::NEG_INFINITY; width]; 3];
            for sample in 0..1024_u32 {
                // Exactly repeated times and integer-spaced changes exercise
                // both inclusive upper and strict lower window boundaries.
                let time_s = f64::from(sample / 3);
                let word = (next() % 3) as usize;
                let changed = next() & next() & valid;
                whole.record(word, changed, time_s);
                bitwise.record(word, changed, time_s);
                for (bit, last) in reference[word].iter_mut().enumerate() {
                    if changed & (1_u32 << bit) != 0 {
                        *last = time_s;
                    }
                }
                let pending = next() & valid;
                for (word, timestamps) in reference.iter().enumerate() {
                    for setup in [0.5, 1.0, 3.0, 5.25] {
                        let expected =
                            timestamps.iter().enumerate().fold(0, |mask, (bit, last)| {
                                if *last > time_s - setup && *last <= time_s {
                                    mask | (1_u32 << bit)
                                } else {
                                    mask
                                }
                            });
                        assert_eq!(
                            bitwise.reject_bits(word, pending, time_s, setup),
                            expected & pending
                        );
                        assert_eq!(
                            whole.reject_bits(word, pending, time_s, setup),
                            if expected == 0 { 0 } else { pending }
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn disabled_and_whole_word_history_do_not_allocate_per_bit_storage() {
        let mut disabled = SetupHistory::new(256, 32, true, 0.0);
        disabled.record(255, u32::MAX, 1.0);
        assert!(matches!(disabled, SetupHistory::Disabled));
        assert_eq!(disabled.reject_bits(255, u32::MAX, 1.0, 0.0), 0);
        let whole = SetupHistory::new(256, 32, false, 1.0);
        assert!(matches!(whole, SetupHistory::WholeWord(ref values) if values.len() == 256));
        let bitwise = SetupHistory::new(256, 32, true, 1.0);
        assert!(
            matches!(bitwise, SetupHistory::Bitwise { width: 32, ref timestamps } if timestamps.len() == 8192)
        );
    }
}
