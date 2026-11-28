#![feature(portable_simd)]

//! Static statistical realization, independent of circuit traversal order,
//! parameter candidates, and numerical integration steps.
//!
//! The source expression parser identifies the dialect-specific function.
//! The occurrence binder provides stable expression and physical-scope
//! identities. This crate deliberately does not infer hierarchy or use a
//! process-global random generator.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
mod random;

pub use random::CounterStream;
use xxhash_rust::xxh3::xxh3_128_with_seed;

/// Which physical occurrences share a random variate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Correlation {
    /// Each device or lexical parameter occurrence has its own draw.
    Independent,
    /// Instances referencing the same bound model share the draw.
    Lot,
}

/// Source distribution syntax. The variants distinguish functions that
/// have different arities, even when their realized laws coincide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// Untruncated normal distribution, three-sigma tolerance.
    Gaussian,
    /// Untruncated normal distribution with an explicit sigma multiplier.
    GaussianSigma,
    /// Normal distribution conditioned on the specified tolerance interval.
    GaussianTruncated,
    /// Symmetric uniform distribution.
    Uniform,
    /// Uniform distribution with separate upper and lower tolerances.
    UniformAsymmetric,
    /// Equal-probability symmetric tolerance endpoints.
    Endpoints,
    /// Equal-probability asymmetric tolerance endpoints.
    EndpointsAsymmetric,
    /// Piecewise-linear probability density over a deviation table.
    Density,
    /// Logarithmic Gaussian law, reserved pending numeric reference.
    LogGaussian,
    /// Logarithmic uniform law, reserved pending numeric reference.
    LogUniform,
    /// Logarithmic endpoint law, reserved pending numeric reference.
    LogEndpoints,
}

/// A typed `SIMetrix` distribution function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Function {
    /// Distribution and argument syntax.
    pub kind: Kind,
    /// Occurrence correlation policy.
    pub correlation: Correlation,
}

impl Function {
    /// Identify a source function case-insensitively, including documented
    /// aliases. These are not the similarly named ngspice functions.
    #[must_use = "the result identifies the typed distribution and correlation"]
    pub fn simetrix(name: &str) -> Option<Self> {
        use Correlation::{Independent, Lot};
        for correlation in [Independent, Lot] {
            for kind in [
                Kind::Gaussian,
                Kind::GaussianSigma,
                Kind::GaussianTruncated,
                Kind::Uniform,
                Kind::UniformAsymmetric,
                Kind::Endpoints,
                Kind::EndpointsAsymmetric,
                Kind::Density,
                Kind::LogGaussian,
                Kind::LogUniform,
                Kind::LogEndpoints,
            ] {
                let function = Self { kind, correlation };
                if name.eq_ignore_ascii_case(function.name()) {
                    return Some(function);
                }
            }
        }
        let correlation = if name.eq_ignore_ascii_case("ud2") {
            Independent
        } else if name.eq_ignore_ascii_case("ud2l") {
            Lot
        } else {
            return None;
        };
        Some(Self {
            kind: Kind::Density,
            correlation,
        })
    }

    /// Canonical source spelling; aliases share one distribution identity.
    #[must_use = "the returned spelling identifies the canonical distribution"]
    pub const fn name(self) -> &'static str {
        match (self.kind, self.correlation) {
            (Kind::Gaussian, Correlation::Independent) => "gauss",
            (Kind::Gaussian, Correlation::Lot) => "gaussl",
            (Kind::GaussianSigma, Correlation::Independent) => "gausssigma",
            (Kind::GaussianSigma, Correlation::Lot) => "gausssigmal",
            (Kind::GaussianTruncated, Correlation::Independent) => "gausstrunc",
            (Kind::GaussianTruncated, Correlation::Lot) => "gausstruncl",
            (Kind::Uniform, Correlation::Independent) => "unif",
            (Kind::Uniform, Correlation::Lot) => "unifl",
            (Kind::UniformAsymmetric, Correlation::Independent) => "unif2",
            (Kind::UniformAsymmetric, Correlation::Lot) => "unifl2",
            (Kind::Endpoints, Correlation::Independent) => "wc",
            (Kind::Endpoints, Correlation::Lot) => "wcl",
            (Kind::EndpointsAsymmetric, Correlation::Independent) => "wc2",
            (Kind::EndpointsAsymmetric, Correlation::Lot) => "wcl2",
            (Kind::Density, Correlation::Independent) => "distribution2",
            (Kind::Density, Correlation::Lot) => "distribution2l",
            (Kind::LogGaussian, Correlation::Independent) => "gausse",
            (Kind::LogGaussian, Correlation::Lot) => "gaussel",
            (Kind::LogUniform, Correlation::Independent) => "unife",
            (Kind::LogUniform, Correlation::Lot) => "unifel",
            (Kind::LogEndpoints, Correlation::Independent) => "wce",
            (Kind::LogEndpoints, Correlation::Lot) => "wcel",
        }
    }

    /// Validate syntax without evaluating arguments. Nominal analyses do
    /// not evaluate tolerance expressions, even undefined ones.
    ///
    /// # Errors
    /// Incorrect number of arguments.
    pub fn validate_arity(self, count: usize) -> Result<()> {
        match self.kind {
            Kind::Density => ensure!(
                count >= 5 && count % 2 == 1,
                "{} requires a tolerance and at least two deviation/density pairs",
                self.name()
            ),
            Kind::GaussianSigma
            | Kind::GaussianTruncated
            | Kind::UniformAsymmetric
            | Kind::EndpointsAsymmetric => {
                ensure!(count == 2, "{} requires two arguments", self.name());
            }
            _ => ensure!(count == 1, "{} requires one argument", self.name()),
        }
        Ok(())
    }

    /// Whether a numerically specified realization is available.
    #[must_use = "the result determines whether realization is supported"]
    pub const fn sampled_supported(self) -> bool {
        !matches!(
            self.kind,
            Kind::LogGaussian | Kind::LogUniform | Kind::LogEndpoints
        )
    }

    fn prepare_law(self, args: &[f64]) -> Result<Law> {
        self.validate_arity(args.len())?;
        ensure!(
            args.iter().all(|v| v.is_finite()),
            "non-finite statistical argument"
        );
        let tolerance = args[0];
        ensure!(tolerance >= 0., "negative tolerance");
        let law = match self.kind {
            Kind::Gaussian => Law::Gaussian {
                tolerance,
                sigma: 3.,
                truncated: false,
            },
            Kind::GaussianSigma | Kind::GaussianTruncated => {
                let sigma = args[1];
                ensure!(sigma > 0., "sigma multiplier must be positive");
                Law::Gaussian {
                    tolerance,
                    sigma,
                    truncated: self.kind == Kind::GaussianTruncated,
                }
            }
            Kind::Uniform | Kind::Endpoints => Law::Interval {
                upper: tolerance,
                lower: tolerance,
                endpoints: self.kind == Kind::Endpoints,
            },
            Kind::UniformAsymmetric | Kind::EndpointsAsymmetric => {
                ensure!(args[1] >= 0., "negative lower tolerance");
                Law::Interval {
                    upper: tolerance,
                    lower: args[1],
                    endpoints: self.kind == Kind::EndpointsAsymmetric,
                }
            }
            Kind::Density => Law::Density {
                tolerance,
                table: Density::new(&args[1..])?,
            },
            Kind::LogGaussian | Kind::LogUniform | Kind::LogEndpoints => bail!(
                "{} logarithmic realization requires a numerical reference",
                self.name()
            ),
        };
        Ok(law)
    }

    /// Bind the source law to its physical owner and declaration.
    /// This does not depend on seed, sample index, or tolerance values.
    ///
    /// # Errors
    /// Lot-correlated distributions require a bound model owner.
    pub fn bind(self, site: Site) -> Result<BoundFunction> {
        let owner = match self.correlation {
            Correlation::Independent => site.occurrence,
            Correlation::Lot => site
                .lot
                .context("lot distribution requires a bound model identity")?,
        };
        let key = Identity::root(b"model-statistics/v2")
            .child(self.name().as_bytes())
            .combine(site.expression)
            .combine(owner);
        Ok(BoundFunction {
            function: self,
            key,
        })
    }
}

/// Explicit deterministic experiment identity. Changing the sample index
/// changes the draw, not the nominal model or its occurrence identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Realization {
    /// User-selected experiment seed, including all 64 bits.
    pub seed: u64,
    /// Zero-based sample index within the experiment.
    pub sample: u64,
}

/// Stable binding identity, constructed once after lexical expansion.
///
/// Callers keep the actual circuit occurrence identity for correctness;
/// these compact keys identify pseudorandom streams, not circuit objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Site {
    /// Expression declaration and static call path (including macro calls).
    /// Do not include parameter values or a mutable traversal counter.
    pub expression: Identity,
    /// Physical device or parameter occurrence, including its enclosing
    /// instance path. All reads of one resolved parameter reuse its identity.
    pub occurrence: Identity,
    /// Bound model declaration occurrence, including its owner's instance
    /// path. Global models have a common owner across subcircuits.
    pub lot: Option<Identity>,
}

/// A noncryptographic hierarchical sampling identity. Components have
/// distinct domains and boundaries; never use this key to establish
/// electrical identity or to deduplicate distinct declarations.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Identity(u128);

impl Identity {
    /// Start an identity domain.
    #[must_use = "the returned identity establishes a sampling domain"]
    #[inline]
    pub fn root(domain: &[u8]) -> Self {
        Self(xxh3_128_with_seed(domain, 0x726f_6f74_2d76_3200))
    }

    /// Extend a path by one independent byte component.
    #[must_use = "the returned identity extends the immutable path"]
    #[inline]
    pub fn child(self, component: &[u8]) -> Self {
        let component = Self(xxh3_128_with_seed(component, 0x636f_6d70_2d76_3200));
        self.pair(component, 0x6368_696c_642d_7632)
    }

    /// Mix a previously bound identity without rehashing its path or
    /// treating its bytes as a source-language string component.
    #[must_use = "the returned identity combines two immutable identities"]
    #[inline]
    pub fn combine(self, other: Self) -> Self {
        self.pair(other, 0x7061_6972_2d76_3200)
    }

    #[inline]
    fn pair(self, other: Self, domain: u64) -> Self {
        let mut bytes = [0; 32];
        bytes[..16].copy_from_slice(&self.0.to_le_bytes());
        bytes[16..].copy_from_slice(&other.0.to_le_bytes());
        Self(xxh3_128_with_seed(&bytes, domain))
    }

    /// The canonical little-endian sampling identity bytes.
    #[must_use = "the returned bytes identify the immutable sampling path"]
    pub const fn bytes(self) -> [u8; 16] {
        self.0.to_le_bytes()
    }

    /// The low and high 64-bit words of this sampling key.
    #[must_use = "the returned words identify the immutable stream key"]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "split the 128-bit identity into exact low and high limbs"
    )]
    pub const fn words(self) -> [u64; 2] {
        [self.0 as u64, (self.0 >> 64) as u64]
    }
}

/// Reserved native function namespace for typed source statistical calls.
/// Ordinary native statistical functions have different argument semantics.
pub const PREPARED_PREFIX: &str = "simetrix_stat_";

/// Explicit sampled source call.
///
/// The first six native operands encode
/// seed, sample, and static source site as pairs of exact 32-bit words.
/// This preserves the full experiment identity in a scalar expression
/// without depending on floating-point seed parsing or ambient state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Invocation {
    /// Selected source law.
    pub function: Function,
    /// Experiment identity.
    pub realization: Realization,
    /// Static call ordinal within the containing declaration, before physical instance expansion.
    /// The consumer also binds the declaration and macro invocation identity.
    pub site: u64,
}

const fn words(value: u64) -> [u32; 2] {
    let bytes = value.to_le_bytes();
    [
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    ]
}

fn join(words: [u32; 2]) -> u64 {
    u64::from(words[0]) | (u64::from(words[1]) << 32)
}

impl Invocation {
    /// Function name of the explicit native representation.
    #[must_use = "the returned function name encodes the source distribution"]
    pub fn marker_name(self) -> String {
        format!("{PREPARED_PREFIX}{}", self.function.name())
    }

    /// Exact metadata operands, in wire order.
    #[must_use = "the returned words preserve the complete experiment identity"]
    pub const fn metadata(self) -> [u32; 6] {
        let [seed_low, seed_high] = words(self.realization.seed);
        let [sample_low, sample_high] = words(self.realization.sample);
        let [site_low, site_high] = words(self.site);
        [
            seed_low,
            seed_high,
            sample_low,
            sample_high,
            site_low,
            site_high,
        ]
    }

    /// Recognize a canonical prepared source function.
    #[must_use = "the result identifies the encoded source distribution"]
    pub fn function_for_marker(name: &str) -> Option<Function> {
        let (prefix, suffix) = name.split_at_checked(PREPARED_PREFIX.len())?;
        if !prefix.eq_ignore_ascii_case(PREPARED_PREFIX) {
            return None;
        }
        Function::simetrix(suffix).filter(|function| function.name().eq_ignore_ascii_case(suffix))
    }

    /// Decode the exact numeric metadata and return the tolerance operands.
    ///
    /// # Errors
    /// Unknown marker, incomplete or invalid metadata, or wrong arity.
    pub fn decode<'a>(name: &str, args: &'a [f64]) -> Result<(Self, &'a [f64])> {
        let function =
            Self::function_for_marker(name).context("unknown prepared statistical function")?;
        let (metadata, law) = args
            .split_at_checked(6)
            .context("missing statistical call metadata")?;
        let mut decoded = [0_u32; 6];
        for (output, &value) in decoded.iter_mut().zip(metadata) {
            ensure!(
                (0.0..=f64::from(u32::MAX)).contains(&value) && value.fract() == 0.,
                "invalid statistical metadata word"
            );
            #[expect(
                clippy::cast_possible_truncation,
                reason = "range and integral value are validated above"
            )]
            #[expect(
                clippy::cast_sign_loss,
                reason = "nonnegative range is validated above"
            )]
            {
                *output = value as u32;
            }
        }
        let [
            seed_low,
            seed_high,
            sample_low,
            sample_high,
            site_low,
            site_high,
        ] = decoded;
        function.validate_arity(law.len())?;
        Ok((
            Self {
                function,
                realization: Realization {
                    seed: join([seed_low, seed_high]),
                    sample: join([sample_low, sample_high]),
                },
                site: join([site_low, site_high]),
            },
            law,
        ))
    }

    /// Bind a call at its lexical and physical occurrence. The declaration-local
    /// source ordinal is mixed here, once, independently of evaluated operands.
    ///
    /// # Errors
    /// Missing lot owner.
    pub fn bind(self, site: Site) -> Result<BoundFunction> {
        self.function.bind(Site {
            expression: site.expression.child(&self.site.to_le_bytes()),
            ..site
        })
    }
}

#[derive(Clone, Debug)]
enum Law {
    Gaussian {
        tolerance: f64,
        sigma: f64,
        truncated: bool,
    },
    Interval {
        upper: f64,
        lower: f64,
        endpoints: bool,
    },
    Density {
        tolerance: f64,
        table: Density,
    },
}

/// A source function with its immutable stream identity.
///
/// Candidate-dependent tolerance expressions can change the law without rebuilding that identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundFunction {
    function: Function,
    key: Identity,
}

impl BoundFunction {
    /// Validate the evaluated operands and prepare the immutable distribution.
    /// Reuse the result across samples until those operands change.
    ///
    /// # Errors
    /// Wrong arity, invalid tolerance, sigma or density, or an unverified
    /// logarithmic distribution.
    pub fn prepare(self, args: &[f64]) -> Result<BoundDistribution> {
        Ok(BoundDistribution {
            key: self.key,
            law: self.function.prepare_law(args)?,
        })
    }
}

/// A validated distribution and immutable stream identity. Reuse it for
/// arbitrary sample indices and seeds without revalidating its law or
/// hashing its lexical paths.
#[derive(Clone, Debug)]
pub struct BoundDistribution {
    key: Identity,
    law: Law,
}

impl BoundDistribution {
    /// Realize one scalar multiplier, independently of other samples.
    ///
    /// # Errors
    /// Exhausted rejection budget or a non-finite result.
    #[inline]
    pub fn sample(&self, realization: Realization) -> Result<f64> {
        let random = CounterStream::new(self.key.words(), realization, 0);
        let value = match &self.law {
            Law::Gaussian {
                tolerance,
                sigma,
                truncated,
            } => {
                if *truncated {
                    tolerance.mul_add(random.truncated(*sigma)?, 1.)
                } else {
                    affine_ratio(*tolerance, random.normal(0), *sigma)
                }
            }
            Law::Interval {
                upper,
                lower,
                endpoints,
            } => {
                let u = random.uniform(0);
                if *endpoints {
                    if u < 0.5 { 1. - lower } else { 1. + upper }
                } else {
                    // Avoid overflow in upper+lower for wide intervals.
                    upper.mul_add(u, (-lower).mul_add(1. - u, 1.))
                }
            }
            Law::Density { tolerance, table } => {
                tolerance.mul_add(table.draw(random.uniform(0))?, 1.)
            }
        };
        ensure!(value.is_finite(), "non-finite statistical realization");
        Ok(value)
    }
}

fn affine_ratio(tolerance: f64, draw: f64, sigma: f64) -> f64 {
    if tolerance == 0. || draw == 0. {
        return 1.;
    }
    let scale = tolerance / sigma;
    if scale.is_normal() {
        return scale.mul_add(draw, 1.);
    }
    // Keep a representable final deviation when the intermediate ratio
    // would overflow or underflow. This branch only affects extreme scales.
    let product = tolerance * draw;
    if product.is_normal() {
        return 1. + product / sigma;
    }
    let quotient = draw / sigma;
    if quotient.is_normal() {
        return tolerance.mul_add(quotient, 1.);
    }
    1. + (tolerance.ln() + draw.abs().ln() - sigma.ln())
        .exp()
        .copysign(draw)
}

#[derive(Clone, Debug)]
struct Segment {
    left: f64,
    right: f64,
    density_left: f64,
    density_right: f64,
    cumulative: f64,
}

#[derive(Clone, Debug)]
struct Density {
    segments: Vec<Segment>,
    total: f64,
}

impl Density {
    fn new(pairs: &[f64]) -> Result<Self> {
        let (points, remainder) = pairs.as_chunks::<2>();
        ensure!(remainder.is_empty(), "density requires pairs");
        let mut weighted = Vec::new();
        let mut maximum = f64::NEG_INFINITY;
        for interval in points.windows(2) {
            let [[left, a], [right, b]] = interval else {
                continue;
            };
            ensure!(
                right >= left && *a >= 0. && *b >= 0.,
                "invalid density table"
            );
            let peak = a.max(*b);
            if right <= left || peak == 0. {
                continue;
            }
            let width = right - left;
            let log_width = if width.is_finite() {
                width.ln()
            } else {
                let scale = left.abs().max(right.abs());
                scale.ln() + (right / scale - left / scale).ln()
            };
            let weight = log_width + peak.ln() + f64::midpoint(a / peak, b / peak).ln();
            maximum = maximum.max(weight);
            weighted.push((
                Segment {
                    left: *left,
                    right: *right,
                    density_left: a / peak,
                    density_right: b / peak,
                    cumulative: 0.,
                },
                weight,
            ));
        }
        ensure!(
            !weighted.is_empty(),
            "density must have positive integrated mass"
        );
        // Normalize in log space so products of extreme finite widths and
        // density values cannot overflow or all underflow together.
        let mut total = 0.;
        let segments = weighted
            .into_iter()
            .filter_map(|(mut segment, weight)| {
                let next = total + (weight - maximum).exp();
                if next <= total {
                    return None;
                }
                total = next;
                segment.cumulative = total;
                Some(segment)
            })
            .collect();
        Ok(Self { segments, total })
    }

    fn draw(&self, uniform: f64) -> Result<f64> {
        let target = uniform * self.total;
        let index = self
            .segments
            .partition_point(|segment| segment.cumulative <= target)
            .min(self.segments.len().saturating_sub(1));
        let segment = self.segments.get(index).context("empty density")?;
        let previous = index
            .checked_sub(1)
            .and_then(|i| self.segments.get(i))
            .map_or(0., |s| s.cumulative);
        let fraction = ((target - previous) / (segment.cumulative - previous)).clamp(0., 1.);
        let a = segment.density_left;
        let b = segment.density_right;
        // Invert the integrated linear density. The rationalized root
        // avoids subtracting nearly equal square roots.
        let position = if fraction == 0. {
            0.
        } else {
            let mass = f64::midpoint(a, b) * fraction;
            let root = (2. * (b - a)).mul_add(mass, a * a).max(0.).sqrt();
            2. * mass / (a + root)
        };
        let width = segment.right - segment.left;
        Ok(if width.is_finite() {
            width.mul_add(position, segment.left)
        } else {
            segment
                .right
                .mul_add(position, segment.left * (1. - position))
        })
    }
}
