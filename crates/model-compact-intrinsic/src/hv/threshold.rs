//! HiSIM-HV pocket, short-channel and narrow-width threshold shifts.
use super::smoothing::{positive, upper};
use super::{
    HiSimHvBiasConditioningPoint, HiSimHvElectrostaticMaterialPoint, HiSimHvIntrinsicScalar as Jet,
    HiSimHvQuantumOxidePoint, PreparedHiSimHvElectrostaticMaterial, PreparedHiSimHvInstance,
    PreparedHiSimHvModel, PreparedHiSimHvTemperature, instance::model_real, lookup_slot,
};
use crate::{ModelError, ModelVariant};
const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;
const VTH_SMOOTHING: f64 = 1e-3;
#[derive(Clone, Copy, Debug, PartialEq)]
struct PocketCoefficients {
    length: f64,
    bs1: f64,
    bs2: f64,
    scp1: f64,
    scp2: f64,
    scp3: f64,
    scp21: f64,
    msc: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShortChannelCoefficients {
    sc1: f64,
    sc2: f64,
    sc3: f64,
    sc4: f64,
    length: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvThreshold {
    variant: ModelVariant,
    flatband: f64,
    material: PreparedHiSimHvElectrostaticMaterial,
    pocket: PocketCoefficients,
    short: ShortChannelCoefficients,
    built_in: f64,
    punchthrough: f64,
    depletion_eta: f64,
    width_correction: f64,
    width_threshold: f64,
    small_device: f64,
}
/// Threshold components before poly depletion and surface solution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvThresholdPoint {
    pub pocket_shift: Jet,
    pub short_channel_shift: Jet,
    pub narrow_channel_shift: Jet,
    pub total_shift: Jet,
    pub reference_bulk_potential: Jet,
    pub depletion_potential_sum: Jet,
    pub threshold: Jet,
}
fn punchthrough_potential(
    pb20: f64,
    flatband: f64,
    gate: Jet,
    beta: Jet,
    factor: Jet,
    weight: f64,
) -> Jet {
    if weight == 0. {
        return Jet::constant(pb20);
    }
    let thermal = 1. / beta;
    let lower = thermal - factor * beta * 0.25 + 1e-25;
    let modified_gate = super::smoothing::lower(gate, lower, 0.005) - flatband;
    let radicand = positive(
        1. + (beta * modified_gate - 1.) * 4. * thermal * thermal / factor,
        0.001,
    ) + 10. * f64::EPSILON;
    let approximation = modified_gate + factor * beta * 0.5 * (1. - radicand.sqrt());
    pb20 + weight * (upper(approximation, Jet::constant(pb20), 0.005) - pb20)
}
/// Prepare original model-size threshold coefficients.
/// # Errors
/// Invalid model binding or nonfinite physical geometry/coefficients.
pub fn prepare_hisim_hv_threshold(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvThreshold, ModelError> {
    model.validate_instance(instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let real = |name| model_real(model, name);
    let material = super::prepare_hisim_hv_electrostatic_material(model, instance)?;
    let geometry = instance.geometry();
    let effective = instance.effective_geometry();
    let pocket = PocketCoefficients {
        length: real("lp")?,
        bs1: real("bs1")?,
        bs2: real("bs2")?,
        scp1: binned("scp1")?,
        scp2: binned("scp2")?,
        scp3: binned("scp3")?,
        scp21: real("scp21")?,
        msc: real("scp22")?,
    };
    let short = ShortChannelCoefficients {
        sc1: binned("sc1")?,
        sc2: binned("sc2")?,
        sc3: binned("sc3")? / geometry.gate_length_m,
        sc4: real("sc4")?,
        length: geometry.gate_length_m - real("parl2")?,
    };
    let punchthrough = if model.variant() == ModelVariant::HiSimHv1 {
        binned("pthrou")?
    } else {
        0.
    };
    let depletion_eta = if material.depletion_mode {
        real("depeta")?
    } else {
        0.
    };
    let result = PreparedHiSimHvThreshold {
        variant: model.variant(),
        flatband: binned("vfbc")?,
        material,
        pocket,
        short,
        built_in: real("vbi")?,
        punchthrough,
        depletion_eta,
        width_correction: binned("wfc")? * 1e4 / effective.channel_width_per_finger_m,
        width_threshold: binned("wvth0")? / (geometry.gate_width_per_finger_m * 1e6),
        small_device: effective.small_device_threshold_correction_v,
    };
    if short.length == 0.
        || ![
            result.flatband,
            pocket.length,
            pocket.bs1,
            pocket.bs2,
            pocket.scp1,
            pocket.scp2,
            pocket.scp3,
            pocket.scp21,
            pocket.msc,
            short.sc1,
            short.sc2,
            short.sc3,
            short.sc4,
            short.length,
            result.built_in,
            punchthrough,
            depletion_eta,
            result.width_correction,
            result.width_threshold,
            result.small_device,
        ]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV threshold coefficients",
        ));
    }
    Ok(result)
}
struct PocketBias {
    body: Jet,
    drain: Jet,
    pb20b: Jet,
    pbsum: Jet,
    qb0: Jet,
    overlap: Jet,
}
impl PreparedHiSimHvThreshold {
    fn pocket_shift(&self, oxide: &HiSimHvQuantumOxidePoint, bias: &PocketBias) -> Jet {
        let PocketBias {
            body,
            drain,
            pb20b,
            pbsum,
            qb0,
            overlap,
        } = *bias;
        let p = self.pocket;
        if p.length == 0. {
            return Jet::constant(0.);
        }
        let pb20 = self.material.reference_channel_potential;
        let bulk_reference = self.material.reference_bulk_potential;
        let density = 2. * SILICON_PERMITTIVITY * ELECTRON_CHARGE * self.material.channel_doping;
        let pocket_threshold = pb20 + self.flatband + qb0 * oxide.inverse_capacitance + overlap;
        let shift = p.bs1 / (positive(p.bs2 - body + 1e-25, VTH_SMOOTHING.sqrt()));
        let modified = upper(body + shift, Jet::constant(0.93 * pb20), VTH_SMOOTHING);
        let qbmm = (density * (pb20 - modified)).sqrt();
        let delta_charge = (qb0 - qbmm) * oxide.inverse_capacitance;
        let bulk_threshold = bulk_reference
            + self.flatband
            + (2.
                * SILICON_PERMITTIVITY
                * ELECTRON_CHARGE
                * self.material.bulk_doping
                * (bulk_reference - body))
                .sqrt()
                * oxide.inverse_capacitance;
        let depletion_width =
            (2. * SILICON_PERMITTIVITY / ELECTRON_CHARGE / self.material.pocket_doping).sqrt();
        let scale = 2.
            * (self.built_in - pb20b)
            * SILICON_PERMITTIVITY
            * oxide.inverse_capacitance
            * depletion_width
            * p.length.powi(-2)
            * pbsum.sqrt();
        let drain_bias = p.scp21 + drain;
        (pocket_threshold - bulk_threshold)
            * scale
            * (p.scp1 + p.scp3 * pbsum / p.length + p.scp2 * drain)
            + delta_charge
            - p.msc / (drain_bias * drain_bias)
    }
    /// Evaluate threshold components from the actual confined biases and oxide.
    /// # Errors
    /// Nonfinite constitutive result or invalid local physical state.
    pub fn evaluate(
        &self,
        bias: &HiSimHvBiasConditioningPoint,
        oxide: &HiSimHvQuantumOxidePoint,
        material: &HiSimHvElectrostaticMaterialPoint,
        temperature: &PreparedHiSimHvTemperature,
        zero_field_capacitance: f64,
    ) -> Result<HiSimHvThresholdPoint, ModelError> {
        let pb20 = self.material.reference_channel_potential;
        let beta = Jet::coordinate(
            temperature.inverse_thermal_voltage,
            3,
            temperature.inverse_thermal_voltage_derivative,
        );
        let body = bias.modified_body;
        let drain = bias.modified_drain;
        let pb20b = punchthrough_potential(
            pb20,
            self.flatband,
            bias.modified_gate,
            beta,
            oxide.poisson_capacitance_factor,
            self.punchthrough,
        );
        let pbsum = pb20b - upper(body, 0.95 * pb20b, VTH_SMOOTHING) + self.depletion_eta * drain;
        let density = 2. * SILICON_PERMITTIVITY * ELECTRON_CHARGE * self.material.channel_doping;
        let qb0 = (density * (pb20 - body)).sqrt();
        let pocket_shift = self.pocket_shift(
            oxide,
            &PocketBias {
                body,
                drain,
                pb20b,
                pbsum,
                qb0,
                overlap: material.pocket_overlap_potential,
            },
        );
        let short = self.short;
        let width =
            (2. * SILICON_PERMITTIVITY / ELECTRON_CHARGE / self.material.channel_doping).sqrt();
        let scale = 2.
            * (self.built_in - pb20b)
            * SILICON_PERMITTIVITY
            * oxide.inverse_capacitance
            * width
            * short.length.powi(-2)
            * pbsum.sqrt();
        let short_channel_shift =
            scale * (short.sc1 + short.sc3 * pbsum + short.sc2 * drain * (1. + short.sc4 * pbsum));
        let narrow_channel_shift = qb0
            * (oxide.inverse_capacitance - 1. / (oxide.capacitance + self.width_correction))
            + self.width_threshold;
        let total_shift =
            pocket_shift + short_channel_shift + narrow_channel_shift + self.small_device;
        let threshold = if self.variant == ModelVariant::HiSimHv1 {
            pb20 + self.flatband + (density * pb20).sqrt() / zero_field_capacitance - total_shift
        } else {
            material.twice_bulk_potential
                + self.flatband
                + (density * (material.twice_bulk_potential - body)).sqrt() / zero_field_capacitance
                - total_shift
        };
        Ok(HiSimHvThresholdPoint {
            pocket_shift: pocket_shift.checked("HiSIM-HV pocket threshold")?,
            short_channel_shift: short_channel_shift.checked("HiSIM-HV short channel threshold")?,
            narrow_channel_shift: narrow_channel_shift
                .checked("HiSIM-HV narrow channel threshold")?,
            total_shift: total_shift.checked("HiSIM-HV total threshold")?,
            reference_bulk_potential: pb20b.checked("HiSIM-HV punchthrough potential")?,
            depletion_potential_sum: pbsum.checked("HiSIM-HV depletion potential sum")?,
            threshold: threshold.checked("HiSIM-HV threshold")?,
        })
    }
}
