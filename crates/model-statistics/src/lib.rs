//! Static statistical realization, independent of circuit traversal order,
//! parameter candidates, and numerical integration steps.
//!
//! The source expression parser identifies the dialect-specific function.
//! The occurrence binder provides stable expression and physical-scope
//! identities. This crate deliberately does not infer hierarchy or use a
//! process-global random generator.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    #[must_use]
    pub fn simetrix(name: &str) -> Option<Self> {
        use Correlation::{Independent, Lot};
        let (kind, correlation) = match name.to_ascii_lowercase().as_str() {
            "gauss" => (Kind::Gaussian, Independent),
            "gaussl" => (Kind::Gaussian, Lot),
            "gausssigma" => (Kind::GaussianSigma, Independent),
            "gausssigmal" => (Kind::GaussianSigma, Lot),
            "gausstrunc" => (Kind::GaussianTruncated, Independent),
            "gausstruncl" => (Kind::GaussianTruncated, Lot),
            "unif" => (Kind::Uniform, Independent),
            "unifl" => (Kind::Uniform, Lot),
            "unif2" => (Kind::UniformAsymmetric, Independent),
            "unifl2" => (Kind::UniformAsymmetric, Lot),
            "wc" => (Kind::Endpoints, Independent),
            "wcl" => (Kind::Endpoints, Lot),
            "wc2" => (Kind::EndpointsAsymmetric, Independent),
            "wcl2" => (Kind::EndpointsAsymmetric, Lot),
            "distribution2" | "ud2" => (Kind::Density, Independent),
            "distribution2l" | "ud2l" => (Kind::Density, Lot),
            "gausse" => (Kind::LogGaussian, Independent),
            "gaussel" => (Kind::LogGaussian, Lot),
            "unife" => (Kind::LogUniform, Independent),
            "unifel" => (Kind::LogUniform, Lot),
            "wce" => (Kind::LogEndpoints, Independent),
            "wcel" => (Kind::LogEndpoints, Lot),
            _ => return None,
        };
        Some(Self { kind, correlation })
    }

    /// Canonical source spelling; aliases share one distribution identity.
    #[must_use]
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
                ensure!(count == 2, "{} requires two arguments", self.name())
            }
            _ => ensure!(count == 1, "{} requires one argument", self.name()),
        }
        Ok(())
    }

    /// Whether a numerically specified realization is available.
    #[must_use]
    pub const fn sampled_supported(self) -> bool {
        !matches!(
            self.kind,
            Kind::LogGaussian | Kind::LogUniform | Kind::LogEndpoints
        )
    }

    /// Validate and prepare the immutable law after evaluating its arguments
    /// in the correct lexical occurrence. Reuse for repeated sample indices.
    ///
    /// # Errors
    /// Wrong arity, invalid tolerance, sigma or density, or an unverified
    /// logarithmic distribution. No nominal fallback is performed.
    pub fn prepare(self, args: &[f64]) -> Result<PreparedDistribution> {
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
        Ok(PreparedDistribution {
            function: self,
            law,
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

/// Stable binding identity, constructed after lexical expansion.
/// Byte strings are independent hash components, not concatenated names.
#[derive(Clone, Copy, Debug)]
pub struct Site<'a> {
    /// Expression declaration and static call path (including macro calls).
    /// Do not include parameter values or a mutable traversal counter.
    pub expression: &'a [u8],
    /// Physical device or parameter occurrence, including its enclosing
    /// instance path. All reads of one resolved parameter reuse its identity.
    pub occurrence: &'a [u8],
    /// Bound model declaration occurrence, including the instance path of
    /// its owner. Global models have a common owner across subcircuits.
    pub lot: Option<&'a [u8]>,
}

/// An immutable hierarchical identity. Distinct component boundaries
/// are retained; callers use lexical names and actual occurrence paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Identity([u8; 32]);

impl Identity {
    /// Start an identity domain.
    #[must_use]
    pub fn root(domain: &[u8]) -> Self {
        Self(Sha256::digest(domain).into())
    }

    /// Extend a path by one independent component.
    #[must_use]
    pub fn child(self, component: &[u8]) -> Self {
        let mut hash = Sha256::new();
        hash.update(self.0);
        hash.update(Sha256::digest(component));
        Self(hash.finalize().into())
    }

    /// Stable identity bytes used by the sampling kernel.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Reserved native function namespace for typed source statistical calls.
/// Ordinary native statistical functions have different argument semantics.
pub const PREPARED_PREFIX: &str = "simetrix_stat_";

/// Explicit sampled source call. The first six native operands encode
/// seed, sample, and static source site as pairs of exact 32-bit words.
/// This preserves the full experiment identity in a scalar expression
/// without depending on floating-point seed parsing or ambient state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Invocation {
    /// Selected source law.
    pub function: Function,
    /// Experiment identity.
    pub realization: Realization,
    /// Stable call site assigned before physical instance expansion.
    pub site: u64,
}

fn words(value: u64) -> [u32; 2] {
    let [a, b, c, d, e, f, g, h] = value.to_le_bytes();
    [
        u32::from_le_bytes([a, b, c, d]),
        u32::from_le_bytes([e, f, g, h]),
    ]
}

fn join(words: [u32; 2]) -> u64 {
    u64::from(words[0]) | (u64::from(words[1]) << 32)
}

impl Invocation {
    /// Function name of the explicit native representation.
    #[must_use]
    pub fn marker_name(self) -> String {
        format!("{PREPARED_PREFIX}{}", self.function.name())
    }

    /// Exact metadata operands, in wire order.
    #[must_use]
    pub fn metadata(self) -> [u32; 6] {
        let [a, b] = words(self.realization.seed);
        let [c, d] = words(self.realization.sample);
        let [e, f] = words(self.site);
        [a, b, c, d, e, f]
    }

    /// Recognize a canonical prepared source function.
    #[must_use]
    pub fn function_for_marker(name: &str) -> Option<Function> {
        let name = name.to_ascii_lowercase();
        let suffix = name.strip_prefix(PREPARED_PREFIX)?;
        Function::simetrix(suffix).filter(|function| function.name() == suffix)
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
                clippy::cast_sign_loss,
                reason = "range and integral value are validated above"
            )]
            {
                *output = value as u32;
            }
        }
        let [a, b, c, d, e, f] = decoded;
        function.validate_arity(law.len())?;
        Ok((
            Self {
                function,
                realization: Realization {
                    seed: join([a, b]),
                    sample: join([c, d]),
                },
                site: join([e, f]),
            },
            law,
        ))
    }

    /// Realize a call at its bound occurrence. The consumer supplies the
    /// enclosing expression/macro invocation identity as well as physical
    /// occurrence and lot identities. The source call site is mixed here.
    ///
    /// # Errors
    /// Invalid law, identity or non-finite realization.
    pub fn sample(self, args: &[f64], site: Site<'_>) -> Result<f64> {
        let expression = Identity::root(site.expression).child(&self.site.to_le_bytes());
        self.function.prepare(args)?.sample(
            self.realization,
            Site {
                expression: expression.bytes(),
                ..site
            },
        )
    }
}

struct Random {
    key: [u8; 32],
}

impl Random {
    fn new(realization: Realization, function: Function, site: Site<'_>) -> Result<Self> {
        let owner = match function.correlation {
            Correlation::Independent => site.occurrence,
            Correlation::Lot => site
                .lot
                .context("lot distribution requires a bound model identity")?,
        };
        // Hash each variable-length component separately to keep their
        // boundaries unambiguous, including embedded zero bytes.
        let mut hash = Sha256::new();
        hash.update(b"model-statistics/v1");
        hash.update(realization.seed.to_le_bytes());
        hash.update(realization.sample.to_le_bytes());
        hash.update(Sha256::digest(function.name().as_bytes()));
        hash.update(Sha256::digest(site.expression));
        hash.update(Sha256::digest(owner));
        Ok(Self {
            key: hash.finalize().into(),
        })
    }

    fn uniform(&self, counter: u64) -> f64 {
        let mut hash = Sha256::new();
        hash.update(self.key);
        hash.update(counter.to_le_bytes());
        let digest = hash.finalize();
        let mut bits = [0; 8];
        bits.copy_from_slice(&digest[..8]);
        // Midpoints of 2^52 equal intervals: both endpoints are excluded,
        // including the largest representable variate.
        let mantissa = u64::from_le_bytes(bits) >> 12;
        (f64::from_bits(0x3ff0_0000_0000_0000 | mantissa) - 1.) + 0.5 * f64::EPSILON
    }

    fn normal(&self, counter: u64) -> f64 {
        (-2. * self.uniform(counter).ln()).sqrt()
            * (std::f64::consts::TAU * self.uniform(counter + 1)).cos()
    }

    fn truncated(&self, sigma: f64) -> Result<f64> {
        // Draw a normalized deviation in [-1, 1]. Uniform proposal is
        // efficient even when sigma is tiny; normal proposal is efficient
        // for the remaining range.
        for attempt in 0..1024_u64 {
            let counter = attempt * 2;
            if sigma < 1. {
                let fraction = 2_f64.mul_add(self.uniform(counter), -1.);
                let z = sigma * fraction;
                if self.uniform(counter + 1) <= (-0.5 * z * z).exp() {
                    return Ok(fraction);
                }
            } else {
                let z = self.normal(counter);
                if z.abs() <= sigma {
                    return Ok(z / sigma);
                }
            }
        }
        bail!("truncated Gaussian rejection budget exhausted")
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

/// A validated distribution, without mutable RNG state.
#[derive(Clone, Debug)]
pub struct PreparedDistribution {
    function: Function,
    law: Law,
}

impl PreparedDistribution {
    /// Realize one scalar multiplier. This is a preparation operation,
    /// never a Newton residual or transient-time operation.
    ///
    /// # Errors
    /// Missing lot owner, exhausted rejection budget, or a non-finite result.
    pub fn sample(&self, realization: Realization, site: Site<'_>) -> Result<f64> {
        let random = Random::new(realization, self.function, site)?;
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
            let weight = log_width + peak.ln() + (0.5 * (a / peak + b / peak)).ln();
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
            let mass = 0.5 * (a + b) * fraction;
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
