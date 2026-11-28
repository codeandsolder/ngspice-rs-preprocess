//! Counter-based Philox4x64-10. Each key selects an independent stream;
//! seed, sample index and block index are disjoint counter coordinates.
//! No shared state, platform byte order, or hash per random variate.
use crate::Realization;
use anyhow::{Result, bail};
use std::{ops::BitXor, simd::Simd};

const MULTIPLIERS: [u64; 2] = [0xd2e7_470e_e14c_6c93, 0xca5a_8263_9512_1157];
const WEYL: [u64; 2] = [0x9e37_79b9_7f4a_7c15, 0xbb67_ae85_84ca_a73b];

// One permutation for scalar and SIMD words. Only the exact widening
// multiplication and modular key increment differ by representation.
trait Word: Copy + BitXor<Output = Self> {
    fn product(self, multiplier: u64) -> (Self, Self);
    fn add_constant(self, constant: u64) -> Self;
}

impl Word for u64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "split the full product into its exact low and high limbs"
    )]
    #[inline]
    fn product(self, multiplier: u64) -> (Self, Self) {
        let full = u128::from(self) * u128::from(multiplier);
        ((full >> 64) as Self, full as Self)
    }

    #[inline]
    fn add_constant(self, constant: u64) -> Self {
        self.wrapping_add(constant)
    }
}

impl<const N: usize> Word for Simd<u64, N> {
    #[inline]
    fn product(self, multiplier: u64) -> (Self, Self) {
        // Four exact 32x32 products per lane. The three middle limbs
        // fit in u64; the carry is incorporated before joining the low word.
        let mask = Self::splat(u64::from(u32::MAX));
        let low = self & mask;
        let high = self >> 32;
        let multiplier_low = Self::splat(multiplier & u64::from(u32::MAX));
        let multiplier_high = Self::splat(multiplier >> 32);
        let low_low = low * multiplier_low;
        let high_low = high * multiplier_low;
        let low_high = low * multiplier_high;
        let high_high = high * multiplier_high;
        let middle = (low_low >> 32) + (high_low & mask) + (low_high & mask);
        let upper = high_high + (high_low >> 32) + (low_high >> 32) + (middle >> 32);
        (upper, (low_low & mask) | (middle << 32))
    }

    #[inline]
    fn add_constant(self, constant: u64) -> Self {
        self + Self::splat(constant)
    }
}

#[inline]
fn philox<W: Word>(mut counter: [W; 4], mut key: [W; 2]) -> [W; 4] {
    for _ in 0..10 {
        let (hi0, lo0) = counter[0].product(MULTIPLIERS[0]);
        let (hi1, lo1) = counter[2].product(MULTIPLIERS[1]);
        counter = [
            hi1 ^ counter[1] ^ key[0],
            lo1,
            hi0 ^ counter[3] ^ key[1],
            lo0,
        ];
        key[0] = key[0].add_constant(WEYL[0]);
        key[1] = key[1].add_constant(WEYL[1]);
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
    domain: u64,
}

impl CounterStream {
    /// Bind an experiment to a previously selected two-word stream key.
    ///
    /// The caller selects a stable domain independently of the seed,
    /// sample, and block. Domain zero is reserved for prepared source laws.
    #[must_use = "the stream defines independently addressable draws"]
    #[inline]
    pub const fn new(key: [u64; 2], realization: Realization, domain: u64) -> Self {
        Self {
            key,
            realization,
            domain,
        }
    }

    /// Generate four integer words at the given block index.
    #[must_use = "the block contains four deterministic draws"]
    #[inline]
    pub fn block(self, index: u64) -> [u64; 4] {
        philox(
            [
                index,
                self.realization.sample,
                self.realization.seed,
                self.domain,
            ],
            self.key,
        )
    }

    /// Generate blocks for independent stream/counter lanes.
    ///
    /// The permutation is identical to scalar draws. The lane count is any supported
    /// portable SIMD width; each lane may have different seed, sample and key.
    #[must_use = "the returned blocks contain the independently indexed draws"]
    #[inline]
    pub fn blocks<const N: usize>(streams: &[Self; N], indices: [u64; N]) -> [[u64; 4]; N] {
        let counter = [
            Simd::from_array(indices),
            Simd::from_array(std::array::from_fn(|lane| streams[lane].realization.sample)),
            Simd::from_array(std::array::from_fn(|lane| streams[lane].realization.seed)),
            Simd::from_array(std::array::from_fn(|lane| streams[lane].domain)),
        ];
        let key = [
            Simd::from_array(std::array::from_fn(|lane| streams[lane].key[0])),
            Simd::from_array(std::array::from_fn(|lane| streams[lane].key[1])),
        ];
        let words = philox(counter, key).map(Simd::to_array);
        std::array::from_fn(|lane| words.map(|word| word[lane]))
    }

    /// Generate a scalar uniform draw, excluding both endpoints.
    #[must_use = "the returned value is an independently indexed uniform draw"]
    #[inline]
    pub fn uniform(self, index: u64) -> f64 {
        let [a, b, c, d] = self.block(index / 4);
        unit(match index % 4 {
            0 => a,
            1 => b,
            2 => c,
            _ => d,
        })
    }

    /// Generate two successive uniform values in a single permutation.
    /// Pair index zero selects words 0 and 1 of block zero.
    #[must_use = "the returned pair is an independently indexed uniform draw"]
    #[inline]
    pub fn uniform_pair(self, index: u64) -> [f64; 2] {
        let [a, b, c, d] = self.block(index / 2);
        if index.is_multiple_of(2) {
            [unit(a), unit(b)]
        } else {
            [unit(c), unit(d)]
        }
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
            philox([0_u64; 4], [0; 2]),
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
                    0x243f_6a88_85a3_08d3_u64,
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
