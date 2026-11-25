//! HICUM/L2 2.4.0 exterior resistors and parasitic capacitors.
//!
//! Exact source nominal resistor topology gates, RSU/CSU parallel branch,
//! and CJcx0/CBCpar/CBEpar source allocation. The circuit binder owns
//! node mapping and exact short collapse, not an artificial large conductance.

use super::thermal::MIN_SOURCE_RESISTANCE;
use crate::{EvaluationError, ModelError, validate_batch_lengths, validate_nonnegative};

#[derive(Clone, Copy, Debug, Default)]
pub struct Resistances {
    pub external_base: f64,
    pub external_collector: f64,
    pub emitter: f64,
    pub substrate: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Capacitances {
    /// CSU appears on (si,s) only when the nominal RSU branch exists.
    pub substrate_shunt: f64,
    /// Total base-emitter parasitic capacitance, shared between (b,e) and (bp,e).
    pub base_emitter_parasitic: f64,
    pub base_emitter_partition: f64,
    /// Total base-collector parasitic capacitance, shared between (b,ci) and (bp,ci).
    pub base_collector_parasitic: f64,
    pub base_collector_partition: f64,
    /// Unsplitted original collector junction zero-bias capacitance, temperature transformed after splitting.
    pub external_collector_zero_bias: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    /// Nominal values select whether each branch is a real resistor.
    pub nominal: Resistances,
    /// Effective values follow the shared HICUM temperature transform;
    /// RSU stays independent of temperature in source 2.4.0.
    pub effective: Resistances,
    pub capacitances: Capacitances,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Bias {
    pub vbbp: f64,
    pub vcci: f64,
    pub veie: f64,
    pub vsis: f64,
    pub vbe: f64,
    pub vbpe: f64,
    pub vbci: f64,
    pub vbpci: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Resistor {
    pub shorted: bool,
    pub amperes: f64,
    pub siemens: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Capacitor {
    pub coulombs: f64,
    pub farads: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Split {
    /// Nominal external and peripheral collector junction capacitances.
    pub external_collector_junction: f64,
    pub peripheral_collector_junction: f64,
    /// CBCpar1/CBCpar2, temperature-independent isolation branches.
    pub external_collector_isolation: f64,
    pub peripheral_collector_isolation: f64,
    /// CBEpar1/CBEpar2, temperature-independent isolation branches.
    pub external_emitter_isolation: f64,
    pub peripheral_emitter_isolation: f64,
}

/// Complete source external resistor temperature sensitivity (A/K)
/// at fixed voltages. All passive isolation-capacitor charges and
/// the source geometry-dependent substrate resistor are T-independent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CurrentSensitivity {
    pub external_base: f64,
    pub external_collector: f64,
    pub emitter: f64,
    pub substrate: f64,
}

impl CurrentSensitivity {
    /// Exact dP/dTdev of the HICUM FLSH=2 *external* resistor power
    /// contributions at fixed electrical voltages, in W/K.
    /// Source self-heating excludes the substrate resistor.
    ///
    /// Multiply by the source dTdev/dRise, and subtract from thermal
    /// KCL d(Rise/Rth-P)/dRise. Other source power families remain
    /// independent contributions in the full electrothermal assembly.
    #[must_use]
    pub fn external_dissipation_slope(self, bias: Bias) -> f64 {
        self.external_base.mul_add(
            bias.vbbp,
            self.external_collector
                .mul_add(bias.vcci, self.emitter * bias.veie),
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    pub external_base: Resistor,
    pub external_collector: Resistor,
    pub emitter: Resistor,
    pub substrate: Resistor,
    pub substrate_shunt: Capacitor,
    pub external_emitter_isolation: Capacitor,
    pub peripheral_emitter_isolation: Capacitor,
    pub external_collector_isolation: Capacitor,
    pub peripheral_collector_isolation: Capacitor,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
    split: Split,
    present: [bool; 4],
    /// Exact d(ln R)/d(Tdev) in 1/K; zero when the externally given
    /// physical resistor family is already temperature independent.
    log_resistance_slope: Resistances,
}

impl Capacitances {
    /// Prepare nominal source collector-junction and isolation partitions.
    /// The returned junction fractions are consumed by the seven HICJQ
    /// charge branches after the temperature scaling is applied.
    ///
    /// # Errors
    /// Rejects invalid raw compact-model capacitances, fractions or
    /// finite-precision overflow during partition.
    pub fn split(self) -> Result<Split, ModelError> {
        for (name, value) in [
            ("csu", self.substrate_shunt),
            ("cbepar", self.base_emitter_parasitic),
            ("cbcpar", self.base_collector_parasitic),
            ("cjcx0", self.external_collector_zero_bias),
        ] {
            validate_nonnegative(name, value)?;
        }
        for (name, value) in [
            ("fbepar", self.base_emitter_partition),
            ("fbcpar", self.base_collector_partition),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        let c = self;
        // Exact source initialization C1=(1-FBCpar)*(CJcx0+CBCpar).
        let first = (1.0 - c.base_collector_partition)
            * (c.external_collector_zero_bias + c.base_collector_parasitic);
        let (external_iso, peripheral_iso, external_junction, peripheral_junction) =
            if first >= c.base_collector_parasitic {
                // Algebraically equal to C1-CBCpar in the published model,
                // but avoids subtracting nearly equal large capacitances.
                // Restrict to the exact physical interval so an ulp of
                // rounding at either end cannot create a negative branch.
                let fbc = c.base_collector_partition;
                let junction = (1.0 - fbc)
                    .mul_add(
                        c.external_collector_zero_bias,
                        -fbc * c.base_collector_parasitic,
                    )
                    .clamp(0.0, c.external_collector_zero_bias);
                (
                    c.base_collector_parasitic,
                    0.0,
                    junction,
                    c.external_collector_zero_bias - junction,
                )
            } else {
                (
                    first,
                    c.base_collector_parasitic - first,
                    0.0,
                    c.external_collector_zero_bias,
                )
            };
        let peripheral_emitter = c.base_emitter_partition * c.base_emitter_parasitic;
        let split = Split {
            external_collector_junction: external_junction,
            peripheral_collector_junction: peripheral_junction,
            external_collector_isolation: external_iso,
            peripheral_collector_isolation: peripheral_iso,
            external_emitter_isolation: c.base_emitter_parasitic - peripheral_emitter,
            peripheral_emitter_isolation: peripheral_emitter,
        };
        if ![
            split.external_collector_junction,
            split.peripheral_collector_junction,
            split.external_collector_isolation,
            split.peripheral_collector_isolation,
            split.external_emitter_isolation,
            split.peripheral_emitter_isolation,
        ]
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0)
        {
            return Err(ModelError::InvalidParameter("external_partition"));
        }
        Ok(split)
    }
}

impl Prepared {
    /// # Errors
    /// Rejects invalid source coefficients or singular temperature-adjusted
    /// resistances for branches that exist in nominal topology.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        Self::with_temperature_slope(p, Resistances::default())
    }

    /// Construct a kernel from prepared effective resistances and exact
    /// dlnR/dTdev coefficients. HICUM's native source temperature
    /// transform produces zeta/Tdev for RBX, RCX and RE, and zero for RSU.
    /// This only needs to run during effective-temperature preparation.
    ///
    /// # Errors
    /// Rejects invalid resistances and nonfinite source slopes.
    pub fn with_temperature_slope(
        p: Parameters,
        log_resistance_slope: Resistances,
    ) -> Result<Self, ModelError> {
        for (name, value) in [
            ("rbx_temperature_slope", log_resistance_slope.external_base),
            (
                "rcx_temperature_slope",
                log_resistance_slope.external_collector,
            ),
            ("re_temperature_slope", log_resistance_slope.emitter),
            ("rsu_temperature_slope", log_resistance_slope.substrate),
        ] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        for (name, value) in [
            ("rbx", p.nominal.external_base),
            ("rcx", p.nominal.external_collector),
            ("re", p.nominal.emitter),
            ("rsu", p.nominal.substrate),
            ("rbx_t", p.effective.external_base),
            ("rcx_t", p.effective.external_collector),
            ("re_t", p.effective.emitter),
            ("rsu_t", p.effective.substrate),
        ] {
            validate_nonnegative(name, value)?;
        }
        let present = [
            p.nominal.external_base >= MIN_SOURCE_RESISTANCE,
            p.nominal.external_collector >= MIN_SOURCE_RESISTANCE,
            p.nominal.emitter >= MIN_SOURCE_RESISTANCE,
            p.nominal.substrate >= MIN_SOURCE_RESISTANCE,
        ];
        for (name, active, resistance) in [
            ("rbx_t", present[0], p.effective.external_base),
            ("rcx_t", present[1], p.effective.external_collector),
            ("re_t", present[2], p.effective.emitter),
            ("rsu_t", present[3], p.effective.substrate),
        ] {
            if active && resistance == 0.0 {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        let split = p.capacitances.split()?;
        Ok(Self {
            p,
            split,
            present,
            log_resistance_slope,
        })
    }

    /// Exact physical external branch dI/dTdev at constant source voltage,
    /// including the 1mOhm nominal short gates. The T-independent source
    /// RSU and all isolation-capacitor charge slopes are identically zero.
    ///
    /// # Errors
    /// Rejects overflow in independently finite resistor slopes/currents.
    pub fn temperature_sensitivity(
        &self,
        operating: Output,
    ) -> Result<CurrentSensitivity, EvaluationError> {
        let slope = self.log_resistance_slope;
        let out = CurrentSensitivity {
            external_base: -operating.external_base.amperes * slope.external_base,
            external_collector: -operating.external_collector.amperes * slope.external_collector,
            emitter: -operating.emitter.amperes * slope.emitter,
            substrate: -operating.substrate.amperes * slope.substrate,
        };
        if ![
            out.external_base,
            out.external_collector,
            out.emitter,
            out.substrate,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }

    #[must_use]
    pub const fn split(&self) -> Split {
        self.split
    }

    /// # Errors
    /// Reports nonfinite bias and current/charge overflow.
    pub fn evaluate(&self, bias: Bias) -> Result<Output, EvaluationError> {
        if ![
            bias.vbbp, bias.vcci, bias.veie, bias.vsis, bias.vbe, bias.vbpe, bias.vbci, bias.vbpci,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        let r = self.p.effective;
        let s = self.split;
        let out = Output {
            external_base: resistor(self.present[0], r.external_base, bias.vbbp),
            external_collector: resistor(self.present[1], r.external_collector, bias.vcci),
            emitter: resistor(self.present[2], r.emitter, bias.veie),
            substrate: resistor(self.present[3], r.substrate, bias.vsis),
            substrate_shunt: capacitor(
                if self.present[3] {
                    self.p.capacitances.substrate_shunt
                } else {
                    0.0
                },
                bias.vsis,
            ),
            external_emitter_isolation: capacitor(s.external_emitter_isolation, bias.vbe),
            peripheral_emitter_isolation: capacitor(s.peripheral_emitter_isolation, bias.vbpe),
            external_collector_isolation: capacitor(s.external_collector_isolation, bias.vbci),
            peripheral_collector_isolation: capacitor(s.peripheral_collector_isolation, bias.vbpci),
        };
        if !out.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(out)
    }

    /// # Errors
    /// Reports incompatible borrowed buffers or invalid branch inputs.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(*input)?;
        }
        Ok(())
    }
}

fn resistor(present: bool, resistance: f64, voltage: f64) -> Resistor {
    if !present {
        return Resistor {
            shorted: true,
            ..Resistor::default()
        };
    }
    let conductance = 1.0 / resistance;
    Resistor {
        shorted: false,
        amperes: voltage * conductance,
        siemens: conductance,
    }
}

const fn capacitor(capacitance: f64, voltage: f64) -> Capacitor {
    Capacitor {
        coulombs: capacitance * voltage,
        farads: capacitance,
    }
}

impl Output {
    fn is_finite(self) -> bool {
        let r = [
            self.external_base,
            self.external_collector,
            self.emitter,
            self.substrate,
        ];
        let c = [
            self.substrate_shunt,
            self.external_emitter_isolation,
            self.peripheral_emitter_isolation,
            self.external_collector_isolation,
            self.peripheral_collector_isolation,
        ];
        r.iter()
            .all(|branch| branch.amperes.is_finite() && branch.siemens.is_finite())
            && c.iter()
                .all(|branch| branch.coulombs.is_finite() && branch.farads.is_finite())
    }
}
