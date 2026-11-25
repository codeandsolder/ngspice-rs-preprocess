//! Native VDMOS observation parameters. No limit changes device residuals.
use super::{Jet, ModelError, Polarity, PreparedModel, pbfact};

/// Source-specified safe operating area limits; omission is significant.
/// Temperatures are stored in kelvin, and current limits may be signed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SoaLimits {
    /// Forward or symmetric gate-source voltage limit (V).
    pub vgs_max: Option<f64>,
    /// Opposite-polarity gate-source voltage limit (V).
    pub vgsr_max: Option<f64>,
    /// Forward or symmetric gate-drain voltage limit (V).
    pub vgd_max: Option<f64>,
    /// Opposite-polarity gate-drain voltage limit (V).
    pub vgdr_max: Option<f64>,
    /// Symmetric external drain-source voltage limit (V).
    pub vds_max: Option<f64>,
    /// Magnitude of channel current limit (A).
    pub id_max: Option<f64>,
    /// Magnitude of reverse-current limit (A).
    pub idr_max: Option<f64>,
    /// Declared maximum power (W).
    pub pd_max: Option<f64>,
    /// Maximum case temperature (K).
    pub te_max: Option<f64>,
    /// External thermal resistance annotation (K/W).
    pub rth_ext: Option<f64>,
    /// Power derating slope (W/K).
    pub derating: Option<f64>,
    /// Whether TNOM was explicitly specified (native derating gate).
    pub tnom_given: bool,
}

impl SoaLimits {
    pub(super) fn set(&mut self, name: &str, value: f64) -> bool {
        let target = match name {
            "vgs_max" => &mut self.vgs_max,
            "vgsr_max" => &mut self.vgsr_max,
            "vgd_max" => &mut self.vgd_max,
            "vgdr_max" => &mut self.vgdr_max,
            "vds_max" => &mut self.vds_max,
            "id_max" => &mut self.id_max,
            "idr_max" => &mut self.idr_max,
            "pd_max" => &mut self.pd_max,
            "te_max" => &mut self.te_max,
            "rth_ext" => &mut self.rth_ext,
            "derating" => &mut self.derating,
            _ => return false,
        };
        *target = Some(if name == "te_max" {
            value + 273.15
        } else {
            value
        });
        true
    }

    pub(super) fn validate(self) -> Result<(), ModelError> {
        for (name, value) in [
            ("vgs_max", self.vgs_max),
            ("vgsr_max", self.vgsr_max),
            ("vgd_max", self.vgd_max),
            ("vgdr_max", self.vgdr_max),
            ("vds_max", self.vds_max),
            ("id_max", self.id_max),
            ("idr_max", self.idr_max),
            ("pd_max", self.pd_max),
            ("te_max", self.te_max),
            ("rth_ext", self.rth_ext),
            ("derating", self.derating),
        ] {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(ModelError::Invalid(name));
            }
        }
        Ok(())
    }
}

/// Temperature input for the native diagnostic law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObservationTemperature {
    /// Fixed instance temperature (K), no self-heating.
    Fixed(f64),
    /// Solved external case temperature (K), not junction temperature.
    Case(f64),
}

/// Accepted-point observations in the native convention.
///
/// The solver supplies integrated displacement currents rather than having this layer reconstruct
/// timestep history or approximate them from DC conductance.
#[derive(Clone, Copy, Debug)]
pub struct SoaSample {
    /// External drain potential (V).
    pub drain: f64,
    /// External gate potential (V).
    pub gate: f64,
    /// External source potential (V).
    pub source: f64,
    /// Physical drain-to-source channel current (A), as in `Evaluation::channel`.
    pub channel: f64,
    /// Total body diode branch current, positive in its forward direction (A).
    pub diode: f64,
    /// Integrated gate-source displacement current (A).
    pub i_cgs: f64,
    /// Integrated gate-drain displacement current (A).
    pub i_cgd: f64,
    /// Fixed instance or solved case temperature.
    pub temperature: ObservationTemperature,
}

/// Named limit that generated a diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoaQuantity {
    Vgs,
    VgsReverse,
    Vgd,
    VgdReverse,
    Vds,
    DrainCurrent,
    ReverseCurrent,
    Power,
    Temperature,
}

/// Value in the limit comparison (polarity-normalized voltage, magnitude,
/// power or absolute temperature) and effective threshold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoaViolation {
    pub quantity: SoaQuantity,
    pub observed: f64,
    pub limit: f64,
}

/// Complete observation of one solved point. The consumer owns warning
/// suppression, aggregation and accepted-point scheduling.
#[derive(Clone, Copy, Debug)]
pub struct SoaObservation {
    /// Native diagnostic power proxy (W), not the thermal residual.
    pub power: f64,
    /// Effective power threshold after the native derating rule.
    pub power_limit: f64,
    violations: [Option<SoaViolation>; 9],
}

impl SoaObservation {
    /// All exceeded limits, in native check order, without allocation.
    pub fn violations(&self) -> impl Iterator<Item = SoaViolation> + '_ {
        self.violations.iter().copied().flatten()
    }
}

const DEFAULT_LIMIT: f64 = 1e99;

fn exceeded(quantity: SoaQuantity, observed: f64, limit: f64) -> Option<SoaViolation> {
    (observed > limit).then_some(SoaViolation {
        quantity,
        observed,
        limit,
    })
}

fn gate_limits(
    polarity: Polarity,
    voltage: f64,
    forward: Option<f64>,
    reverse: Option<f64>,
    quantities: [SoaQuantity; 2],
) -> [Option<SoaViolation>; 2] {
    let forward = forward.unwrap_or(DEFAULT_LIMIT);
    let Some(reverse) = reverse else {
        return [exceeded(quantities[0], voltage.abs(), forward), None];
    };
    let normalized = polarity.sign() * voltage;
    let forward = exceeded(quantities[0], normalized, forward);
    let reverse = exceeded(quantities[1], -normalized, reverse);
    match polarity {
        Polarity::N => [forward, reverse],
        Polarity::P => [reverse, forward],
    }
}

/// Distortion-only inputs used by native ngspice's historical coefficient
/// setup. This does not advertise a complete distortion analysis kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistortionInputs {
    /// Temperature-corrected surface potential (V).
    pub surface_potential: f64,
    /// Native fixed oxide capacitance factor (F/m²).
    pub oxide_capacitance_factor: f64,
}

impl PreparedModel {
    /// Native read-only TYPE query.
    #[must_use = "the result is the native polarity label"]
    pub const fn native_type(&self) -> &'static str {
        match self.model.polarity {
            Polarity::N => "vdmosn",
            Polarity::P => "vdmosp",
        }
    }

    /// External thermal-resistance annotation, defaulting to RTHCA.
    /// This value does not modify the physical thermal network.
    #[must_use = "the result is observation metadata"]
    pub fn external_thermal_resistance(&self) -> f64 {
        self.model.soa.rth_ext.unwrap_or(self.model.thermal.rthca)
    }

    /// Prepare distortion-only model inputs at an explicit temperature.
    ///
    /// # Errors
    /// Nonfinite or nonpositive absolute temperature or nonfinite result.
    pub fn distortion_inputs(&self, temperature: f64) -> Result<DistortionInputs, ModelError> {
        if !temperature.is_finite() || temperature <= 0.0 {
            return Err(ModelError::Invalid("distortion temperature"));
        }
        let nominal = self.model.tnom;
        let phi = (temperature / nominal).mul_add(
            self.model.phi - pbfact(Jet::constant(nominal)).v,
            pbfact(Jet::constant(temperature)).v,
        );
        if !phi.is_finite() {
            return Err(ModelError::Invalid("distortion surface potential"));
        }
        Ok(DistortionInputs {
            surface_potential: phi,
            oxide_capacitance_factor: 3.9 * 8.854_214_871e-12 / 1e-7,
        })
    }

    /// Compute the ngspice 47 SOA diagnostics without modifying the model,
    /// residual, solver state, or warning counters.
    ///
    /// # Errors
    /// Nonfinite sample, invalid absolute temperature or overflow.
    pub fn observe_soa(&self, sample: SoaSample) -> Result<SoaObservation, ModelError> {
        let (temperature, heating) = match sample.temperature {
            ObservationTemperature::Fixed(t) => (t, false),
            ObservationTemperature::Case(t) => (t, true),
        };
        if temperature <= 0.0
            || [
                temperature,
                sample.drain,
                sample.gate,
                sample.source,
                sample.channel,
                sample.diode,
                sample.i_cgs,
                sample.i_cgd,
            ]
            .iter()
            .any(|v| !v.is_finite())
        {
            return Err(ModelError::Invalid("SOA sample"));
        }
        let s = self.model.soa;
        let vgs = sample.gate - sample.source;
        let vgd = sample.gate - sample.drain;
        let vds = sample.drain - sample.source;
        let channel = self.model.polarity.sign() * sample.channel;
        let id = channel.abs();
        let idr = (channel - sample.diode).abs();
        let power =
            ((id + idr) * vds).abs() + (sample.i_cgs * vgs).abs() + (sample.i_cgd * vgd).abs();
        // vdmosset.c in ngspice 47 resets PD_MAX if ID_MAX is omitted.
        // Preserve both the declared value and the effective native behavior.
        let mut power_limit = if s.id_max.is_some() {
            s.pd_max.unwrap_or(DEFAULT_LIMIT)
        } else {
            DEFAULT_LIMIT
        };
        let derate = s.tnom_given && s.pd_max.is_some() && (!heating || s.te_max.is_some());
        let mut temperature_violation = None;
        if derate && let Some(slope) = s.derating {
            if temperature >= self.model.tnom {
                let derated = (-(temperature - self.model.tnom)).mul_add(slope, power_limit);
                if !derated.is_finite() {
                    return Err(ModelError::Invalid("SOA derating"));
                }
                power_limit = derated.max(0.0);
            }
            if heating && let Some(limit) = s.te_max {
                temperature_violation = exceeded(SoaQuantity::Temperature, temperature, limit);
            }
        }
        if [vgs, vgd, vds, idr, power, power_limit]
            .iter()
            .any(|v| !v.is_finite())
        {
            return Err(ModelError::Invalid("SOA observation"));
        }
        let [source_first, source_second] = gate_limits(
            self.model.polarity,
            vgs,
            s.vgs_max,
            s.vgsr_max,
            [SoaQuantity::Vgs, SoaQuantity::VgsReverse],
        );
        let [drain_first, drain_second] = gate_limits(
            self.model.polarity,
            vgd,
            s.vgd_max,
            s.vgdr_max,
            [SoaQuantity::Vgd, SoaQuantity::VgdReverse],
        );
        Ok(SoaObservation {
            power,
            power_limit,
            violations: [
                source_first,
                source_second,
                drain_first,
                drain_second,
                exceeded(
                    SoaQuantity::Vds,
                    vds.abs(),
                    s.vds_max.unwrap_or(DEFAULT_LIMIT),
                ),
                s.id_max
                    .and_then(|limit| exceeded(SoaQuantity::DrainCurrent, id, limit.abs())),
                s.idr_max
                    .and_then(|limit| exceeded(SoaQuantity::ReverseCurrent, idr, limit.abs())),
                exceeded(SoaQuantity::Power, power, power_limit),
                temperature_violation,
            ],
        })
    }
}
