//! Counter-based Philox4x64-10. Each key selects an independent stream;
//! seed, sample index and block index are disjoint counter coordinates.
//! No shared state, platform byte order, or hash per random variate.
use crate::{Identity, Realization};
use anyhow::{Result, bail};

const MULTIPLIERS: [u64; 2] = [0xd2e7_470e_e14c_6c93, 0xca5a_8263_9512_1157];
const WEYL: [u64; 2] = [0x9e37_79b9_7f4a_7c15, 0xbb67_ae85_84ca_a73b];

#[expect(
    clippy::cast_possible_truncation,
    reason = "split the full product into its exact low and high limbs"
)]
const fn product(a: u64, b: u64) -> (u64, u64) {
    let full = (a as u128) * (b as u128);
    ((full >> 64) as u64, full as u64)
}

const fn philox(mut counter: [u64; 4], mut key: [u64; 2]) -> [u64; 4] {
    let mut round = 0;
    while round < 10 {
        let (hi0, lo0) = product(MULTIPLIERS[0], counter[0]);
        let (hi1, lo1) = product(MULTIPLIERS[1], counter[2]);
        counter = [
            hi1 ^ counter[1] ^ key[0],
            lo1,
            hi0 ^ counter[3] ^ key[1],
            lo0,
        ];
        key[0] = key[0].wrapping_add(WEYL[0]);
        key[1] = key[1].wrapping_add(WEYL[1]);
        round += 1;
    }
    counter
}

fn unit(bits: u64) -> f64 {
    // Midpoints of 2^52 equal intervals: strictly inside (0, 1),
    // including the largest representable result.
    let mantissa = bits >> 12;
    0.5_f64.mul_add(
        f64::EPSILON,
        f64::from_bits(0x3ff0_0000_0000_0000 | mantissa) - 1.,
    )
}

/// Immutable, independently addressable Philox4x64-10 stream.
/// A block contains four uniform integer variates, suitable for batches.
/// All 64 bits of the seed, sample and block coordinates are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CounterStream {
    key: [u64; 2],
    realization: Realization,
}

impl CounterStream {
    /// Bind an experiment to a previously selected stream key.
    #[must_use = "the stream defines independently addressable draws"]
    #[inline]
    pub const fn new(key: Identity, realization: Realization) -> Self {
        Self {
            key: key.key(),
            realization,
        }
    }

    /// Generate four integer words at the given block index.
    #[must_use = "the block contains four deterministic draws"]
    #[inline]
    pub const fn block(self, index: u64) -> [u64; 4] {
        philox(
            [index, self.realization.sample, self.realization.seed, 0],
            self.key,
        )
    }

    /// Generate a scalar uniform draw, excluding both endpoints.
    #[must_use = "the returned value is an independently indexed uniform draw"]
    #[inline]
    pub fn uniform(self, index: u64) -> f64 {
        unit(self.block(index / 4)[(index % 4) as usize])
    }

    /// Generate two successive uniform values in a single permutation.
    /// Pair index zero selects words 0 and 1 of block zero.
    #[must_use = "the returned pair is an independently indexed uniform draw"]
    #[inline]
    pub fn uniform_pair(self, index: u64) -> [f64; 2] {
        let words = self.block(index / 2);
        let offset = ((index % 2) * 2) as usize;
        [unit(words[offset]), unit(words[offset + 1])]
    }

    /// Standard normal variate from the indexed uniform pair.
    #[must_use = "the returned value is an independently indexed normal draw"]
    #[inline]
    pub fn normal(self, index: u64) -> f64 {
        let [radius, angle] = self.uniform_pair(index);
        (-2. * radius.ln()).sqrt() * (std::f64::consts::TAU * angle).cos()
    }

    pub(crate) fn truncated(self, sigma: f64) -> Result<f64> {
        // Draw a normalized deviation in [-1, 1]. Uniform proposal is
        // efficient for small sigma; normal proposal for the other range.
        for attempt in 0..1024 {
            if sigma < 1. {
                let [proposal, acceptance] = self.uniform_pair(attempt);
                let fraction = 2_f64.mul_add(proposal, -1.);
                let z = sigma * fraction;
                if acceptance <= (-0.5 * z * z).exp() {
                    return Ok(fraction);
                }
            } else {
                let z = self.normal(attempt);
                if z.abs() <= sigma {
                    return Ok(z / sigma);
                }
            }
        }
        bail!("truncated Gaussian rejection budget exhausted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random123_known_answer() {
        // Random123 upstream tests/kat_vectors: Philox4x64, ten rounds,
        // zero counter and key.
        assert_eq!(
            philox([0; 4], [0; 2]),
            [
                0x1655_4d9e_ca36_314c,
                0xdb20_fe9d_672d_0fdc,
                0xd7e7_72ce_e186_176b,
                0x7e68_b68a_ec7b_a23b
            ],
        );
    }

    #[test]
    fn random123_nonzero_known_answers() {
        assert_eq!(
            philox([u64::MAX; 4], [u64::MAX; 2]),
            [
                0x87b0_92c3_013f_e90b,
                0x438c_3c67_be8d_0224,
                0x9cc7_d7c6_9cd7_77b6,
                0xa09c_aebf_594f_0ba0
            ],
        );
        assert_eq!(
            philox(
                [
                    0x243f_6a88_85a3_08d3,
                    0x1319_8a2e_0370_7344,
                    0xa409_3822_299f_31d0,
                    0x082e_fa98_ec4e_6c89
                ],
                [0x4528_21e6_38d0_1377, 0xbe54_66cf_34e9_0c6c],
            ),
            [
                0xa528_f454_03e6_1d95,
                0x38c7_2dbd_566e_9788,
                0xa5a1_610e_72fd_18b5,
                0x57bd_43b5_e52b_7fe6
            ],
        );
    }

    #[test]
    fn endpoints_are_excluded() {
        assert!(unit(0) > 0.);
        assert!(unit(u64::MAX) < 1.);
    }
}
