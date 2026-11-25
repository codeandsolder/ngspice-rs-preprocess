//! HICUM/L2 2.4.0 physical junction charges from QJMODF, QJMOD and HICJQ.
//!
//! Unlike VBIC normalized depletion integrals, these outputs are stored
//! **coulombs** and incremental capacitances in **farads**. HICUM GICCR
//! couples internal B-E/B-C charges into the hole-charge equation. The solver
//! owns all ddt integration, accepted-step state, and branch stamping.

mod punch;

use crate::{
    EvaluationError, ModelError, Temperature, validate_batch_lengths, validate_nonnegative,
    validate_positive,
};

const SMOOTHING: f64 = 1.921_812;
const PUNCHTHROUGH_SELECTOR: f64 = 100.0;
const SOURCE_COLLECTOR_CAP_RATIO: f64 = 2.4;

/// Explicit forward-bias capacitance smoothing, QJMODF.
#[derive(Clone, Copy, Debug)]
pub struct Forward {
    pub zero_bias_capacitance: f64,
    pub potential: f64,
    /// Reference z (0 < z < 1).
    pub grading: f64,
    /// Effective `a_j` (>0), peak C/C0. Nominal model card `a_j`
    /// must be >=1; TMPHICJ can lower the effective value below 1.
    pub max_cap_ratio: f64,
}

/// B-C or S-C junction HICJQ. Punch-through is selected only when Vpt<100 V.
#[derive(Clone, Copy, Debug)]
pub struct Collector {
    pub zero_bias_capacitance: f64,
    pub potential: f64,
    pub grading: f64,
    pub punch_through: f64,
}

/// Partitioned external collector junctions share Vdcx, zcx and Vptcx.
#[derive(Clone, Copy, Debug)]
pub struct ExternalCollector {
    pub external_zero_bias_capacitance: f64,
    pub peripheral_zero_bias_capacitance: f64,
    pub potential: f64,
    pub grading: f64,
    pub punch_through: f64,
}

/// The perimeter S-C junction is linear if Vdsp=0 in native 2.4.0.
#[derive(Clone, Copy, Debug)]
pub struct PeripheralSubstrate {
    pub zero_bias_capacitance: f64,
    pub potential: f64,
    pub grading: f64,
    pub punch_through: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub intrinsic_emitter: Forward,
    pub peripheral_emitter: Forward,
    pub intrinsic_collector: Collector,
    pub external_collector: ExternalCollector,
    pub substrate: Collector,
    pub peripheral_substrate: PeripheralSubstrate,
}

/// Exact temperature derivative of one prepared physical junction's
/// capacitance, potential and forward cap ratio (or punch-through).
#[derive(Clone, Copy, Debug, Default)]
pub struct CoefficientSlopes {
    pub zero_bias_capacitance: f64,
    pub potential: f64,
    pub maximum_or_punchthrough: f64,
}

/// All prepared source TMPHICJ coefficient derivatives, including
/// the two partitions of the external collector source junction.
#[derive(Clone, Copy, Debug, Default)]
pub struct ParameterSlopes {
    pub intrinsic_emitter: CoefficientSlopes,
    pub peripheral_emitter: CoefficientSlopes,
    pub intrinsic_collector: CoefficientSlopes,
    pub external_collector: CoefficientSlopes,
    pub peripheral_collector: CoefficientSlopes,
    pub substrate: CoefficientSlopes,
    pub peripheral_substrate: CoefficientSlopes,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub vbiei: f64,
    pub vbpei: f64,
    pub vbici: f64,
    /// External base-to-internal collector V(b,ci), distinct from V(bi,ci).
    pub vbci: f64,
    pub vbpci: f64,
    pub vsici: f64,
    pub vsc: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Junction {
    pub coulombs: f64,
    /// Exact derivative of stored physical charge: dQ/dV for solver stamping.
    pub farads: f64,
    /// HICUM 2.4.0's explicitly published C result. Its punch-through
    /// formula is not always the derivative of the published Q formula;
    /// model-specific internal calculations use this explicit source value.
    pub source_capacitance: f64,
    /// Derivative of the model's explicitly published capacitance (F/V).
    /// This differs from d²Q/dV² in the punch-through branch.
    pub d_source_capacitance: f64,
}

/// Exact fixed-source-voltage forward junction thermal charge and
/// capacitance sensitivities. The source QJMODF C and dQ/dV agree.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ForwardSensitivity {
    pub charge: f64,
    pub capacitance: f64,
}

/// Independent intrinsic/peripheral QJMODF emitter contributions;
/// collector/substrate punch-through charge derivatives have their
/// own different source laws and are not silently included here.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmitterSensitivities {
    pub intrinsic: ForwardSensitivity,
    pub peripheral: ForwardSensitivity,
}

/// Collector reach-through source temperature sensitivities (fixed
/// source voltage). Published source C differs from dQ/dV; both
/// capacitance derivatives stay distinct from dQ/dT.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CollectorSensitivity {
    pub charge: f64,
    pub capacitance: f64,
    pub source_capacitance: f64,
}

/// All source collector/substrate QJMOD thermal sensitivities.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CollectorSensitivities {
    pub intrinsic: CollectorSensitivity,
    pub external: CollectorSensitivity,
    pub peripheral: CollectorSensitivity,
    pub substrate: CollectorSensitivity,
    pub peripheral_substrate: CollectorSensitivity,
}

/// All seven source depletion-family temperature derivatives.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TemperatureSensitivities {
    pub emitters: EmitterSensitivities,
    pub collectors: CollectorSensitivities,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Qjei / Cjei, intrinsic B-E depletion storage.
    pub intrinsic_emitter: Junction,
    /// Qjep / Cjep, peripheral B-E storage.
    pub peripheral_emitter: Junction,
    /// Qjci / Cjci, intrinsic B-C depletion storage.
    pub intrinsic_collector: Junction,
    /// `qjcx0_t_i` / `CjCx_i`, external base-to-internal-collector branch.
    pub external_collector: Junction,
    /// `qjcx0_t_ii` / `CjCx_ii`, peripheral base-to-collector branch.
    pub peripheral_collector: Junction,
    /// Qjs / Cjs, internal S-C storage.
    pub substrate: Junction,
    /// Qscp / Cscp, peripheral S-C storage.
    pub peripheral_substrate: Junction,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntrinsicForTransport {
    pub emitter: Junction,
    pub collector: Junction,
    /// Source HICCR Cjcit calculated via QJMODF, even if Qjci uses
    /// punch-through HICJQ. Not equal to punched-through Cjci.
    pub transit_collector_capacitance: f64,
    pub d_transit_collector_capacitance_vbici: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    parameters: Parameters,
    vt: f64,
    effective_kelvin: f64,
    temperature_slopes: ParameterSlopes,
}

impl Prepared {
    /// # Errors
    /// Reports the first unphysical HICUM junction parameter.
    pub fn new(parameters: Parameters, temperature: Temperature) -> Result<Self, ModelError> {
        Self::with_temperature_slopes(parameters, temperature, ParameterSlopes::default())
    }

    /// Exact temperature derivatives of QJMODF prepared source junction
    /// parameters. The nominal source temperature layer provides the
    /// coefficients while the junction kernel owns all physical laws.
    ///
    /// # Errors
    /// Rejects unphysical source junction parameters or nonfinite slopes.
    pub fn with_temperature_slopes(
        parameters: Parameters,
        temperature: Temperature,
        temperature_slopes: ParameterSlopes,
    ) -> Result<Self, ModelError> {
        for family in [
            temperature_slopes.intrinsic_emitter,
            temperature_slopes.peripheral_emitter,
            temperature_slopes.intrinsic_collector,
            temperature_slopes.external_collector,
            temperature_slopes.peripheral_collector,
            temperature_slopes.substrate,
            temperature_slopes.peripheral_substrate,
        ] {
            if ![
                family.zero_bias_capacitance,
                family.potential,
                family.maximum_or_punchthrough,
            ]
            .iter()
            .all(|x| x.is_finite())
            {
                return Err(ModelError::InvalidParameter("junction_temperature_slope"));
            }
        }
        let vt = (1.380_650_3e-23 / 1.602_176_462e-19) * temperature.kelvin();
        if !vt.is_finite() {
            return Err(ModelError::InvalidTemperature);
        }
        for (name, branch) in [
            ("intrinsic_emitter", parameters.intrinsic_emitter),
            ("peripheral_emitter", parameters.peripheral_emitter),
        ] {
            validate_forward(branch, name)?;
        }
        for (name, branch) in [
            ("intrinsic_collector", parameters.intrinsic_collector),
            (
                "external_collector",
                Collector {
                    zero_bias_capacitance: parameters
                        .external_collector
                        .external_zero_bias_capacitance,
                    potential: parameters.external_collector.potential,
                    grading: parameters.external_collector.grading,
                    punch_through: parameters.external_collector.punch_through,
                },
            ),
            (
                "peripheral_collector",
                Collector {
                    zero_bias_capacitance: parameters
                        .external_collector
                        .peripheral_zero_bias_capacitance,
                    potential: parameters.external_collector.potential,
                    grading: parameters.external_collector.grading,
                    punch_through: parameters.external_collector.punch_through,
                },
            ),
            ("substrate", parameters.substrate),
        ] {
            validate_collector(branch, name, vt)?;
        }
        let ps = parameters.peripheral_substrate;
        validate_nonnegative("peripheral_substrate_capacitance", ps.zero_bias_capacitance)?;
        if ps.potential > 0.0 {
            validate_collector(
                Collector {
                    zero_bias_capacitance: ps.zero_bias_capacitance,
                    potential: ps.potential,
                    grading: ps.grading,
                    punch_through: ps.punch_through,
                },
                "peripheral_substrate",
                vt,
            )?;
        } else if !ps.potential.is_finite() || ps.potential < 0.0 {
            return Err(ModelError::InvalidParameter(
                "peripheral_substrate_potential",
            ));
        }
        Ok(Self {
            parameters,
            vt,
            effective_kelvin: temperature.kelvin(),
            temperature_slopes,
        })
    }

    /// Evaluate all seven HICUM depletion charge/capacitance branches.
    ///
    /// # Errors
    /// Reports a nonfinite voltage, numerical overflow or nonphysical domain.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if ![
            bias.vbiei, bias.vbpei, bias.vbici, bias.vbci, bias.vbpci, bias.vsici, bias.vsc,
        ]
        .iter()
        .all(|x| x.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.parameters;
        let external = p.external_collector;
        let perimeter = p.peripheral_substrate;
        let result = Output {
            intrinsic_emitter: forward(p.intrinsic_emitter, bias.vbiei, self.vt)?,
            peripheral_emitter: forward(p.peripheral_emitter, bias.vbpei, self.vt)?,
            intrinsic_collector: collector(p.intrinsic_collector, bias.vbici, self.vt)?,
            external_collector: collector(
                Collector {
                    zero_bias_capacitance: external.external_zero_bias_capacitance,
                    potential: external.potential,
                    grading: external.grading,
                    punch_through: external.punch_through,
                },
                bias.vbci,
                self.vt,
            )?,
            peripheral_collector: collector(
                Collector {
                    zero_bias_capacitance: external.peripheral_zero_bias_capacitance,
                    potential: external.potential,
                    grading: external.grading,
                    punch_through: external.punch_through,
                },
                bias.vbpci,
                self.vt,
            )?,
            substrate: collector(p.substrate, bias.vsici, self.vt)?,
            peripheral_substrate: if perimeter.potential > 0.0 {
                collector(
                    Collector {
                        zero_bias_capacitance: perimeter.zero_bias_capacitance,
                        potential: perimeter.potential,
                        grading: perimeter.grading,
                        punch_through: perimeter.punch_through,
                    },
                    bias.vsc,
                    self.vt,
                )?
            } else {
                Junction {
                    coulombs: perimeter.zero_bias_capacitance * bias.vsc,
                    farads: perimeter.zero_bias_capacitance,
                    source_capacitance: perimeter.zero_bias_capacitance,
                    d_source_capacitance: 0.0,
                }
            },
        };
        let branches = [
            result.intrinsic_emitter,
            result.peripheral_emitter,
            result.intrinsic_collector,
            result.external_collector,
            result.peripheral_collector,
            result.substrate,
            result.peripheral_substrate,
        ];
        if !branches.iter().all(|branch| {
            branch.coulombs.is_finite()
                && branch.farads.is_finite()
                && branch.source_capacitance.is_finite()
                && branch.d_source_capacitance.is_finite()
        }) {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(result)
    }

    /// Source TMPHICJ coefficient slopes for coupled physical laws.
    #[must_use = "use the derivatives in the electrothermal Jacobian"]
    pub const fn temperature_slopes(&self) -> ParameterSlopes {
        self.temperature_slopes
    }

    /// Full seven-branch QJMODF/QJMOD temperature derivative
    /// assembly. Each collector branch has distinct charge, source
    /// capacitance, and stamped farads derivatives.
    ///
    /// # Errors
    /// Invalid physical junction bias or source numerical overflow.
    pub fn temperature_sensitivity(
        &self,
        bias: Bias,
    ) -> Result<TemperatureSensitivities, EvaluationError> {
        Ok(TemperatureSensitivities {
            emitters: self.emitter_temperature_sensitivity(bias)?,
            collectors: self.collector_temperature_sensitivity(bias)?,
        })
    }

    /// Exact source emitter QJMODF thermal derivatives at fixed
    /// polarity-normalized voltage. No numerical differences or
    /// duplicate forward capacitance/charge source equations.
    ///
    /// # Errors
    /// Rejects nonfinite source biases and finite precision overflow.
    pub fn emitter_temperature_sensitivity(
        &self,
        bias: Bias,
    ) -> Result<EmitterSensitivities, EvaluationError> {
        if !bias.vbiei.is_finite() || !bias.vbpei.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        Ok(EmitterSensitivities {
            intrinsic: forward_temperature_sensitivity(
                self.parameters.intrinsic_emitter,
                self.temperature_slopes.intrinsic_emitter,
                bias.vbiei,
                self.vt,
                self.effective_kelvin,
            )?,
            peripheral: forward_temperature_sensitivity(
                self.parameters.peripheral_emitter,
                self.temperature_slopes.peripheral_emitter,
                bias.vbpei,
                self.vt,
                self.effective_kelvin,
            )?,
        })
    }

    /// Exact source thermal charge and both capacitance sensitivities
    /// from all five collector/substrate physical QJMOD branches.
    ///
    /// # Errors
    /// Rejects nonfinite voltages and source physical overflow.
    pub fn collector_temperature_sensitivity(
        &self,
        bias: Bias,
    ) -> Result<CollectorSensitivities, EvaluationError> {
        if ![bias.vbici, bias.vbci, bias.vbpci, bias.vsici, bias.vsc]
            .iter()
            .all(|v| v.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.parameters;
        let s = self.temperature_slopes;
        let external = p.external_collector;
        let perimeter = p.peripheral_substrate;
        let collector = |param: Collector, slopes: CoefficientSlopes, voltage: f64| {
            collector_temperature_sensitivity(
                param,
                slopes,
                voltage,
                self.vt,
                self.effective_kelvin,
            )
        };
        Ok(CollectorSensitivities {
            intrinsic: collector(p.intrinsic_collector, s.intrinsic_collector, bias.vbici)?,
            external: collector(
                Collector {
                    zero_bias_capacitance: external.external_zero_bias_capacitance,
                    potential: external.potential,
                    grading: external.grading,
                    punch_through: external.punch_through,
                },
                s.external_collector,
                bias.vbci,
            )?,
            peripheral: collector(
                Collector {
                    zero_bias_capacitance: external.peripheral_zero_bias_capacitance,
                    potential: external.potential,
                    grading: external.grading,
                    punch_through: external.punch_through,
                },
                s.peripheral_collector,
                bias.vbpci,
            )?,
            substrate: collector(p.substrate, s.substrate, bias.vsici)?,
            peripheral_substrate: if perimeter.potential > 0.0 {
                collector(
                    Collector {
                        zero_bias_capacitance: perimeter.zero_bias_capacitance,
                        potential: perimeter.potential,
                        grading: perimeter.grading,
                        punch_through: perimeter.punch_through,
                    },
                    s.peripheral_substrate,
                    bias.vsc,
                )?
            } else {
                CollectorSensitivity {
                    charge: bias.vsc * s.peripheral_substrate.zero_bias_capacitance,
                    capacitance: s.peripheral_substrate.zero_bias_capacitance,
                    source_capacitance: s.peripheral_substrate.zero_bias_capacitance,
                }
            },
        })
    }

    /// The two exact intrinsic source hole-charge temperature
    /// sensitivities needed by the HICCR equilibrium Qp0 residual.
    /// Only these two physical junctions are evaluated; the other
    /// five source junction branches are not part of HICCR.
    ///
    /// # Errors
    /// Rejects invalid source voltage or finite precision overflow.
    pub fn intrinsic_hiccr_temperature_sensitivity(
        &self,
        emitter_voltage: f64,
        collector_voltage: f64,
    ) -> Result<(ForwardSensitivity, CollectorSensitivity), EvaluationError> {
        if !emitter_voltage.is_finite() || !collector_voltage.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let emitter = forward_temperature_sensitivity(
            self.parameters.intrinsic_emitter,
            self.temperature_slopes.intrinsic_emitter,
            emitter_voltage,
            self.vt,
            self.effective_kelvin,
        )?;
        let collector = collector_temperature_sensitivity(
            self.parameters.intrinsic_collector,
            self.temperature_slopes.intrinsic_collector,
            collector_voltage,
            self.vt,
            self.effective_kelvin,
        )?;
        Ok((emitter, collector))
    }

    /// Exact source HICCR collector transit capacitance dCjcit/dTdev.
    /// HICCR uses *unpunched* QJMODF Cjcit even when QJMOD punch-through
    /// charge/Cjci takes a different source branch.
    ///
    /// # Errors
    /// Rejects nonfinite source voltage and derivative overflow.
    pub fn transit_collector_temperature_sensitivity(
        &self,
        collector_voltage: f64,
    ) -> Result<f64, EvaluationError> {
        if !collector_voltage.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let p = self.parameters.intrinsic_collector;
        if p.zero_bias_capacitance == 0.0 {
            return Ok(0.0);
        }
        Ok(forward_temperature_sensitivity(
            Forward {
                zero_bias_capacitance: p.zero_bias_capacitance,
                potential: p.potential,
                grading: p.grading,
                max_cap_ratio: SOURCE_COLLECTOR_CAP_RATIO,
            },
            CoefficientSlopes {
                maximum_or_punchthrough: 0.0,
                ..self.temperature_slopes.intrinsic_collector
            },
            collector_voltage,
            self.vt,
            self.effective_kelvin,
        )?
        .capacitance)
    }

    /// Provide HICCR's three separate depletion inputs and their local
    /// derivatives without evaluating five unused junction branches.
    ///
    /// # Errors
    /// Rejects invalid intrinsic voltages or numerical overflow.
    pub fn intrinsic_for_transport(
        &self,
        emitter_voltage: f64,
        collector_voltage: f64,
    ) -> Result<IntrinsicForTransport, EvaluationError> {
        if !emitter_voltage.is_finite() || !collector_voltage.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        let emitter = forward(self.parameters.intrinsic_emitter, emitter_voltage, self.vt)?;
        let collector = collector(
            self.parameters.intrinsic_collector,
            collector_voltage,
            self.vt,
        )?;
        let p = self.parameters.intrinsic_collector;
        if p.zero_bias_capacitance == 0.0 {
            return Ok(IntrinsicForTransport {
                emitter,
                collector,
                ..IntrinsicForTransport::default()
            });
        }
        // Native Cjcit does *not* use the punch-through condition of Qjci:
        // HICCR explicitly evaluates the forward-smoothed 2.4 cap here.
        let smoothing = forward_smoothing(
            Forward {
                zero_bias_capacitance: p.zero_bias_capacitance,
                potential: p.potential,
                grading: p.grading,
                max_cap_ratio: SOURCE_COLLECTOR_CAP_RATIO,
            },
            collector_voltage,
            self.vt,
        )?;
        Ok(IntrinsicForTransport {
            emitter,
            collector,
            transit_collector_capacitance: smoothing.capacitance,
            d_transit_collector_capacitance_vbici: smoothing.derivative,
        })
    }

    /// # Errors
    /// Rejects mismatched buffers or any nonphysical junction evaluation.
    pub fn evaluate_batch(
        &self,
        biases: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(biases.len(), outputs.len())?;
        for (bias, output) in biases.iter().zip(outputs) {
            *output = self.evaluate(*bias)?;
        }
        Ok(())
    }
}

fn validate_forward(p: Forward, name: &'static str) -> Result<(), ModelError> {
    validate_nonnegative(name, p.zero_bias_capacitance)?;
    validate_positive(name, p.potential)?;
    if !p.grading.is_finite() || !(0.0..1.0).contains(&p.grading) || p.grading == 0.0 {
        return Err(ModelError::InvalidParameter(name));
    }
    if !p.max_cap_ratio.is_finite() || p.max_cap_ratio <= 0.0 {
        return Err(ModelError::InvalidParameter(name));
    }
    Ok(())
}

fn validate_collector(p: Collector, name: &'static str, vt: f64) -> Result<(), ModelError> {
    validate_nonnegative(name, p.zero_bias_capacitance)?;
    validate_positive(name, p.potential)?;
    if !p.grading.is_finite() || p.grading <= 0.0 || p.grading >= 1.0 {
        return Err(ModelError::InvalidParameter(name));
    }
    validate_positive(name, p.punch_through)?;
    if p.punch_through < PUNCHTHROUGH_SELECTOR
        && 0.1f64.mul_add(p.punch_through - p.potential, 4.0 * vt) <= 0.0
    {
        return Err(ModelError::InvalidParameter(name));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ForwardSmoothing {
    softened: f64,
    log: f64,
    capacitance: f64,
    derivative: f64,
}

/// Common source QJMODF capacitance and its analytic voltage derivative.
/// This is also the *unpunched* Cjcit needed by the HICCR transit law.
fn forward_smoothing(
    p: Forward,
    voltage: f64,
    vt: f64,
) -> Result<ForwardSmoothing, EvaluationError> {
    let threshold = p.potential * (1.0 - (-p.max_cap_ratio.ln() / p.grading).exp());
    let normalized = (threshold - voltage) / vt;
    let root = normalized.hypot(SMOOTHING.sqrt());
    let fraction = normalized.midpoint(root) / root;
    let softened = vt.mul_add(-normalized.midpoint(root), threshold);
    let log = (-softened / p.potential).ln_1p();
    let multiplier = p.zero_bias_capacitance * (-p.grading * log).exp();
    let peak = p.max_cap_ratio * p.zero_bias_capacitance;
    let capacitance = multiplier.mul_add(fraction, peak * (1.0 - fraction));
    let derivative_fraction = -0.5 * SMOOTHING / (vt * root * root * root);
    let graded_derivative = multiplier * p.grading * fraction * fraction / (p.potential - softened);
    let derivative = (multiplier - peak).mul_add(derivative_fraction, graded_derivative);
    if !capacitance.is_finite() || !derivative.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(ForwardSmoothing {
        softened,
        log,
        capacitance,
        derivative,
    })
}

fn forward(p: Forward, voltage: f64, vt: f64) -> Result<Junction, EvaluationError> {
    if p.zero_bias_capacitance == 0.0 {
        return Ok(Junction::default());
    }
    let components = forward_smoothing(p, voltage, vt)?;
    let integral =
        p.zero_bias_capacitance * p.potential * (-((1.0 - p.grading) * components.log).exp_m1())
            / (1.0 - p.grading);
    let charge = (p.max_cap_ratio * p.zero_bias_capacitance)
        .mul_add(voltage - components.softened, integral);
    if !charge.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(Junction {
        coulombs: charge,
        farads: components.capacitance,
        source_capacitance: components.capacitance,
        d_source_capacitance: components.derivative,
    })
}

/// Differentiate the exact QJMODF forward smoothing and charge
/// evaluation at fixed branch voltage. The prepared source TMPHICJ
/// coefficient slopes and dVT/dT=VT/T are the only inputs.
///
/// The same actual smoothing value/log/C as the electrical
/// law is reused; the equations below are its exact derivatives.
fn forward_temperature_sensitivity(
    p: Forward,
    slopes: CoefficientSlopes,
    voltage: f64,
    vt: f64,
    kelvin: f64,
) -> Result<ForwardSensitivity, EvaluationError> {
    if p.zero_bias_capacitance == 0.0 {
        return Ok(ForwardSensitivity::default());
    }
    let g = p.grading;
    let c0 = p.zero_bias_capacitance;
    let vd = p.potential;
    let a = p.max_cap_ratio;
    let c0_dt = slopes.zero_bias_capacitance;
    let vd_dt = slopes.potential;
    let a_dt = slopes.maximum_or_punchthrough;
    let voltage_ratio = (-a.ln() / g).exp();
    let threshold = vd * (1.0 - voltage_ratio);
    let threshold_dt = vd_dt.mul_add(1.0 - voltage_ratio, vd * voltage_ratio * a_dt / (g * a));
    let normalized = (threshold - voltage) / vt;
    let normalized_dt = (threshold_dt - normalized * vt / kelvin) / vt;
    let root = normalized.hypot(SMOOTHING.sqrt());
    let root_dt = normalized * normalized_dt / root;
    let softened_dt = vt.mul_add(
        -normalized_dt.midpoint(root_dt),
        (vt / kelvin).mul_add(-normalized.midpoint(root), threshold_dt),
    );
    let source = forward_smoothing(p, voltage, vt)?;
    let log_dt = (vd_dt - softened_dt) / (vd - source.softened) - vd_dt / vd;
    let fraction = normalized.midpoint(root) / root;
    let fraction_dt = 0.5 * SMOOTHING * normalized_dt / (root * root * root);
    let multiplier = c0 * (-g * source.log).exp();
    let multiplier_dt = multiplier * g.mul_add(-log_dt, c0_dt / c0);
    let peak = a * c0;
    let peak_dt = a.mul_add(c0_dt, a_dt * c0);
    let cap_dt = multiplier_dt.mul_add(
        fraction,
        peak_dt.mul_add(1.0 - fraction, (multiplier - peak) * fraction_dt),
    );
    let integral = c0 * vd * (-((1.0 - g) * source.log).exp_m1()) / (1.0 - g);
    let integral_dt = (c0 * vd * ((1.0 - g) * source.log).exp())
        .mul_add(-log_dt, (c0_dt / c0 + vd_dt / vd) * integral);
    let charge_dt = peak_dt.mul_add(
        voltage - source.softened,
        (-peak).mul_add(softened_dt, integral_dt),
    );
    if !cap_dt.is_finite() || !charge_dt.is_finite() {
        return Err(EvaluationError::NonFiniteResult);
    }
    Ok(ForwardSensitivity {
        charge: charge_dt,
        capacitance: cap_dt,
    })
}

fn collector(p: Collector, voltage: f64, vt: f64) -> Result<Junction, EvaluationError> {
    if p.punch_through >= PUNCHTHROUGH_SELECTOR {
        return forward(
            Forward {
                zero_bias_capacitance: p.zero_bias_capacitance,
                potential: p.potential,
                grading: p.grading,
                max_cap_ratio: SOURCE_COLLECTOR_CAP_RATIO,
            },
            voltage,
            vt,
        );
    }
    if p.zero_bias_capacitance == 0.0 {
        return Ok(Junction::default());
    }
    punch::evaluate(p, voltage, vt)
}

/// Source-exact physical QJMOD thermal derivative, including the
/// special unpunched QJMODF branch and no-Cj disabled branch.
fn collector_temperature_sensitivity(
    p: Collector,
    slopes: CoefficientSlopes,
    voltage: f64,
    vt: f64,
    kelvin: f64,
) -> Result<CollectorSensitivity, EvaluationError> {
    if p.zero_bias_capacitance == 0.0 {
        return Ok(CollectorSensitivity::default());
    }
    if p.punch_through >= PUNCHTHROUGH_SELECTOR {
        let result = forward_temperature_sensitivity(
            Forward {
                zero_bias_capacitance: p.zero_bias_capacitance,
                potential: p.potential,
                grading: p.grading,
                max_cap_ratio: SOURCE_COLLECTOR_CAP_RATIO,
            },
            CoefficientSlopes {
                maximum_or_punchthrough: 0.0,
                ..slopes
            },
            voltage,
            vt,
            kelvin,
        )?;
        return Ok(CollectorSensitivity {
            charge: result.charge,
            capacitance: result.capacitance,
            source_capacitance: result.capacitance,
        });
    }
    punch::thermal(p, slopes, voltage, vt, kelvin)
}
