//! Conventional HiSIM-HV CORSRD=1/3 physical series branches.
use super::{
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real,
    instance_real_at, lookup_slot,
    smoothing::{lower, positive, symmetry_add, upper},
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue, differential::Differential};

const Q: f64 = 1.602_191_8e-19;
const K: f64 = 1.380_622_6e-23;
const ESI: f64 = 1.034_943e-10;
const MIN_RESISTANCE: f64 = 1e-4;

/// Jacobian in physical external-source coordinates: Vgs, Vds, Vbs,
/// local temperature, drain/source series drops, and substrate voltage.
pub type HiSimHvExplicitSeriesScalar = Differential<7>;
type Jet = HiSimHvExplicitSeriesScalar;

/// All voltages refer to physical terminal orientation, normalized by polarity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvExplicitSeriesBias {
    pub gate_source_v: f64,
    pub drain_source_v: f64,
    pub body_source_v: f64,
    pub drain_drop_v: f64,
    pub source_drop_v: f64,
    pub substrate_voltage_v: Option<f64>,
}

/// A physical external-to-prime branch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvExplicitSeriesBranch {
    pub resistance_ohm: Jet,
    pub current_a: Jet,
    pub noise_a2_per_hz: f64,
}

/// Absent branches have no prime node, equation or noise source.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvExplicitSeriesPoint {
    pub drain: Option<HiSimHvExplicitSeriesBranch>,
    pub source: Option<HiSimHvExplicitSeriesBranch>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    present: bool,
    base: f64,
    base_geometry: f64,
    voltage_geometry: f64,
    sheet: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThermalCoefficients {
    linear: f64,
    quadratic: f64,
    ambient_only: bool,
}
impl ThermalCoefficients {
    fn apply(self, base: f64, geometry: f64, temp: &PreparedHiSimHvTemperature) -> Jet {
        if base <= 0. {
            return Jet::constant(0.);
        }
        let kelvin = if self.ambient_only {
            Jet::constant(temp.ambient_device_temperature_kelvin)
        } else {
            Jet::coordinate(temp.device_temperature_kelvin, 3, 1.)
        };
        let nominal = temp.nominal_temperature_kelvin;
        let delta = kelvin - nominal;
        let width_resistance =
            geometry * (base + self.linear * delta + self.quadratic * delta * (kelvin + nominal));
        lower(width_resistance, Jet::constant(0.005 * base), 0.01 * base)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Substrate {
    length: f64,
    thickness: f64,
    depletion_factor: Option<f64>,
    built_in: f64,
    drain_coefficient: f64,
    bias_coefficient: f64,
    small: f64,
}
impl Substrate {
    fn multiplier(self, drain: Jet, voltage: Jet) -> Jet {
        let Some(factor) = self.depletion_factor else {
            return Jet::constant(1.);
        };
        let potential = positive(
            self.built_in - self.drain_coefficient * drain - self.bias_coefficient * voltage,
            10.,
        );
        let depletion = if factor == 0. {
            Jet::constant(self.small)
        } else {
            (factor * potential).sqrt() + self.small
        };
        let width = upper(
            depletion,
            Jet::constant(self.thickness),
            0.1 * self.thickness,
        );
        self.length / positive(self.thickness - width, 1e-9)
    }
}

/// Prepared native explicit series resistance, distinct from the nonlinear
/// channel-current access correction and the HV2 CORDRIFT drift law.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvExplicitSeries {
    drain: Side,
    source: Side,
    base_temperature: ThermalCoefficients,
    voltage_temperature: ThermalCoefficients,
    voltage_base: f64,
    gate_coefficient: f64,
    gate_reference: f64,
    body_coefficient: f64,
    symmetry: f64,
    symmetric: bool,
    width: f64,
    multiplicity: f64,
    substrate: Substrate,
    noise_enabled: bool,
    noise_threshold: f64,
}
impl PreparedHiSimHvExplicitSeries {
    /// Physical node-allocation flags, independent of the trial state.
    #[must_use = "prime-node flags determine the device topology"]
    pub const fn prime_nodes(&self) -> [bool; 2] {
        [self.drain.present, self.source.present]
    }

    fn width_resistance(
        &self,
        side: Side,
        drain: Jet,
        gate: Jet,
        body: Jet,
        temp: &PreparedHiSimHvTemperature,
        modulated: bool,
    ) -> Jet {
        let base = self
            .base_temperature
            .apply(side.base, side.base_geometry, temp);
        if !modulated {
            return base;
        }
        let voltage =
            self.voltage_temperature
                .apply(self.voltage_base, side.voltage_geometry, temp);
        let width = base + voltage * drain;
        let gate_width = width * (1. + self.gate_coefficient * (1. - gate / self.gate_reference));
        let bounded = upper(
            lower(gate_width, width, 1e-4),
            (1. + self.gate_coefficient) * width,
            5e-7,
        );
        bounded * positive(1. - self.body_coefficient * body, 1e-6)
    }

    fn branch(
        &self,
        side: Side,
        raw: Jet,
        drop: Jet,
        temp: &PreparedHiSimHvTemperature,
    ) -> Result<Option<HiSimHvExplicitSeriesBranch>, ModelError> {
        if !side.present {
            return Ok(None);
        }
        let resistance = raw / self.multiplicity;
        let resistance = if resistance.value < MIN_RESISTANCE {
            Jet::constant(MIN_RESISTANCE)
        } else {
            resistance
        }
        .checked("HiSIM-HV explicit series resistance")?;
        let noise = if self.noise_enabled && raw.value > self.noise_threshold {
            4. * K * temp.device_temperature_kelvin * self.multiplicity / raw.value
        } else {
            0.
        };
        Ok(Some(HiSimHvExplicitSeriesBranch {
            resistance_ohm: resistance,
            current_a: (drop / resistance).checked("HiSIM-HV explicit series current")?,
            noise_a2_per_hz: noise,
        }))
    }

    /// Evaluate physical branch currents, local resistance, derivatives and
    /// thermal spectra from independent external and local thermal coordinates.
    /// # Errors
    /// Nonfinite trial state or constitutive result.
    pub fn evaluate(
        &self,
        bias: HiSimHvExplicitSeriesBias,
        temp: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvExplicitSeriesPoint, ModelError> {
        let gate = Jet::coordinate(bias.gate_source_v, 0, 1.);
        let drain = Jet::coordinate(bias.drain_source_v, 1, 1.);
        let body = Jet::coordinate(bias.body_source_v, 2, 1.);
        let reversed = drain.value < 0.;
        let (gate, drain, body, first, second) = if reversed {
            (gate - drain, -drain, body - drain, self.source, self.drain)
        } else {
            (gate, drain, body, self.drain, self.source)
        };
        let has_base = self.drain.base > 0. || self.source.base > 0.;
        let enabled = self.voltage_base > 0.
            || self.gate_coefficient > 0.
            || self.body_coefficient > 0.
            || bias.substrate_voltage_v.is_some();
        let offset = symmetry_add(drain, self.symmetry, 1e-12);
        let modified_drain = drain + 2. * offset;
        let modified_gate = gate + offset;
        let modified_body = body + offset;
        let (mut r1, mut r2) = if has_base {
            (
                self.width_resistance(
                    first,
                    modified_drain,
                    modified_gate,
                    modified_body,
                    temp,
                    (self.symmetric || !reversed) && enabled,
                ),
                self.width_resistance(
                    second,
                    Jet::constant(2. * self.symmetry),
                    modified_gate,
                    modified_body,
                    temp,
                    (self.symmetric || reversed) && enabled,
                ),
            )
        } else {
            (Jet::constant(0.), Jet::constant(0.))
        };
        if let Some(voltage) = bias.substrate_voltage_v
            && !self.symmetric
            && has_base
        {
            let substrate =
                Jet::coordinate(voltage, 6, 1.) + if reversed { drain } else { Jet::constant(0.) };
            let multiplier = self.substrate.multiplier(modified_drain, substrate);
            if reversed {
                r2 = r2 * multiplier;
            } else {
                r1 = r1 * multiplier;
            }
        }
        // Native series laws use the physical external conduction direction,
        // then map back to the geometric D/S nodes, not the channel direction.
        let (rd, rs) = if reversed { (r2, r1) } else { (r1, r2) };
        let raw_d = if has_base {
            rd / self.width + self.drain.sheet
        } else {
            rd
        };
        let raw_s = if has_base {
            rs / self.width + self.source.sheet
        } else {
            rs
        };
        Ok(HiSimHvExplicitSeriesPoint {
            drain: self.branch(
                self.drain,
                raw_d,
                Jet::coordinate(bias.drain_drop_v, 4, 1.),
                temp,
            )?,
            source: self.branch(
                self.source,
                raw_s,
                Jet::coordinate(bias.source_drop_v, 5, 1.),
                temp,
            )?,
        })
    }
}

struct Binding<'a> {
    model: &'a PreparedHiSimHvModel,
    instance: &'a PreparedHiSimHvInstance,
}
impl Binding<'_> {
    fn real(&self, name: &str) -> Result<f64, ModelError> {
        model_real(self.model, name)
    }
    fn binned(&self, name: &str) -> Result<f64, ModelError> {
        let slot = lookup_slot(self.model.variant(), name)?;
        self.instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    }
    fn local(&self, name: &str) -> Result<f64, ModelError> {
        let slot = self
            .model
            .variant()
            .parameter(ParameterScope::Instance, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
            .slot;
        instance_real_at(self.instance, slot)
    }
    fn integer(&self, name: &str) -> Result<i32, ModelError> {
        let slot = lookup_slot(self.model.variant(), name)?;
        match self.model.get_slot(slot) {
            Some(ParameterValue::Integer(value)) => Ok(*value),
            _ => Err(ModelError::MissingDefaultDependency(slot)),
        }
    }

    fn side(&self, source: bool, area: f64, length: f64) -> Result<Side, ModelError> {
        let (r, squares, l1, l2, overlap) = if source {
            ("rs", "nrs", "ldrift1s", "ldrift2s", "lovers")
        } else {
            ("rd", "nrd", "ldrift1", "ldrift2", "loverld")
        };
        let l1 = self.local(l1)? * 1e6;
        let l2 = self.local(l2)? * 1e6;
        let overlap = self.local(overlap)? * 1e6;
        let squares = self.local(squares)?;
        let sheet = self.real("rsh")?.max(0.) * squares;
        let second = l2.mul_add(self.real("rdslp2")?, self.real("rdict2")?);
        let geometry = l1.mul_add(self.binned("rdslp1")?, self.binned("rdict1")?) * second;
        // Topology is allocated before binning in the native setup.
        let setup_geometry = l1.mul_add(self.real("rdslp1")?, self.real("rdict1")?) * second;
        let raw_base = self.real(r)?;
        let setup_base = if source {
            raw_base
        } else {
            raw_base + self.real("rdvd")?
        };
        let present = sheet.mul_add(self.local("nf")?, setup_base * setup_geometry) > 0.;
        let mut base = self.binned(r)?;
        // HV1's setup substitutes a tiny positive RD for an allocated drain
        // prime with zero model RD (before computing binned coefficients).
        if !source && present && self.model.variant() == ModelVariant::HiSimHv1 && raw_base == 0. {
            base += 1e-50;
        }
        let base_geometry = geometry * (1. + self.real("rds")? / area.powf(self.real("rdsp")?));
        let voltage_geometry = geometry
            * (1. + self.real("rdvds")? / area.powf(self.real("rdvdsp")?))
            * (-self.real("rdvdl")? * length.powf(self.real("rdvdlp")?))
                .min(80.)
                .exp();
        let overlap_scale = self.real("rdov11")?;
        let initial = Jet::constant(voltage_geometry);
        let scaled = lower(
            initial * (1. + overlap_scale * (1. - overlap / (self.real("rdov12")? + 1e-50))),
            initial,
            1e-2,
        );
        let bounded = upper(scaled, initial * (1. + overlap_scale), 5e-5);
        let voltage_geometry = lower(
            bounded + initial * ((1. - self.binned("rdov13")?) * overlap),
            Jet::constant(0.),
            5e-5,
        )
        .value;
        Ok(Side {
            present,
            base,
            base_geometry,
            voltage_geometry,
            sheet,
        })
    }

    fn substrate(&self) -> Result<Substrate, ModelError> {
        let overlap = self.binned("nover")? * 1e6;
        let doping = self.real("nsubsub")? * 1e6;
        let depletion_factor = if overlap * (overlap + doping) > 0. {
            Some(2. * ESI / Q * doping / (overlap * (overlap + doping)))
        } else {
            None
        };
        Ok(Substrate {
            length: self.local("ldrift1")? + self.local("ldrift2")?,
            thickness: self.real("ddrift")?,
            depletion_factor,
            built_in: self.real("vbisub")?,
            drain_coefficient: self.real("rdvdsub")?,
            bias_coefficient: self.real("rdvsub")?,
            small: if self.model.variant() == ModelVariant::HiSimHv1 {
                1e-50
            } else {
                1e-25
            },
        })
    }
}

/// Bind conventional CORSRD=1/3 physical series branches. Other selectors
/// (including HV2 CORDRIFT) are not this constitutive law.
/// # Errors
/// Invalid instance provenance, geometry or source coefficients.
pub fn prepare_hisim_hv_explicit_series(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<Option<PreparedHiSimHvExplicitSeries>, ModelError> {
    model.validate_instance(instance)?;
    let b = Binding { model, instance };
    if !matches!(b.integer("corsrd")?, 1 | 3) {
        return Ok(None);
    }
    let g = instance.geometry();
    let length = g.gate_length_m * 1e6;
    let area = length * g.gate_width_per_finger_m * 1e6;
    let drain = b.side(false, area, length)?;
    let source = b.side(true, area, length)?;
    let cotemp = b.integer("cotemp")?;
    let result = PreparedHiSimHvExplicitSeries {
        drain,
        source,
        base_temperature: ThermalCoefficients {
            linear: b.real("rdtemp1")? / 100.,
            quadratic: b.real("rdtemp2")? / 100.,
            ambient_only: cotemp == 1,
        },
        voltage_temperature: ThermalCoefficients {
            linear: b.real("rdvdtemp1")? / 100.,
            quadratic: b.real("rdvdtemp2")? / 100.,
            ambient_only: matches!(cotemp, 0 | 1),
        },
        voltage_base: b.binned("rdvd")? / 100.,
        gate_coefficient: b.binned("rdvg11")?,
        gate_reference: b.real("rdvg12")? + 1e-50,
        body_coefficient: b.binned("rdvb")?,
        symmetry: b.real("vzadd0")?,
        symmetric: b.integer("cosym")? == 1,
        width: instance
            .effective_geometry()
            .lateral_diffusion_width_per_finger_m
            * b.local("nf")?,
        multiplicity: b.local("m")?,
        substrate: b.substrate()?,
        noise_enabled: b.integer("cothrml")? != 0,
        noise_threshold: if model.variant() == ModelVariant::HiSimHv1 {
            0.
        } else {
            MIN_RESISTANCE
        },
    };
    if result.width <= 0. || result.multiplicity <= 0. {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV explicit series geometry",
        ));
    }
    Ok(Some(result))
}
