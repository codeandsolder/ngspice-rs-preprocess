//! Leading B4SOI temperature and electrostatic laws and passive heat storage.
//! Copyright 2010 Regents of the University of California. See package LICENSE.

use super::{
    BsimSoiNode, CHARGE, EPS_0, EPS_OX, EPS_SI, K_OVER_Q, PreparedBsimSoiSetup, integer, positive,
    real, slot,
};
use crate::{ModelError, MosPolarity, ParameterScope};

/// A constitutive quantity and its analytic partial derivative per kelvin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiThermalScalar {
    pub value: f64,
    pub d_temperature: f64,
}

impl BsimSoiThermalScalar {
    pub(super) const fn new(value: f64, d_temperature: f64) -> Self {
        Self {
            value,
            d_temperature,
        }
    }

    pub(super) const fn checked(self, name: &'static str) -> Result<Self, ModelError> {
        if self.value.is_finite() && self.d_temperature.is_finite() {
            Ok(self)
        } else {
            Err(ModelError::InvalidPhysicalParameter(name))
        }
    }
}

/// Passive thermal port coefficients including the native parallel multiplier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiThermalNetwork {
    /// Local topology role of the heat node.
    pub node: BsimSoiNode,
    /// Conductive heat flow per kelvin of rise (W/K).
    pub conductance: f64,
    /// Stored energy per kelvin of rise (J/K).
    pub heat_capacity: f64,
}

/// Leading physical inputs at a temperature trial. Generated heat is not
/// part of the passive thermal row and must be supplied by the channel kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiTemperaturePoint {
    pub kelvin: f64,
    pub thermal_voltage: BsimSoiThermalScalar,
    pub bandgap: BsimSoiThermalScalar,
    /// Intrinsic carrier density in cm^-3, using native model units.
    pub intrinsic_density: BsimSoiThermalScalar,
    pub surface_potential: BsimSoiThermalScalar,
    pub built_in_potential: BsimSoiThermalScalar,
    pub backgate_flatband: BsimSoiThermalScalar,
    pub depletion_width: BsimSoiThermalScalar,
    pub depletion_capacitance: BsimSoiThermalScalar,
    pub conductive_heat: BsimSoiThermalScalar,
    pub stored_heat: BsimSoiThermalScalar,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Material {
    bandgap_zero: f64,
    bandgap_a: f64,
    bandgap_b: f64,
    density_nominal: f64,
    permittivity: f64,
    generalized: bool,
}

impl Material {
    fn gap(self, temperature: f64) -> BsimSoiThermalScalar {
        let denominator = temperature + self.bandgap_b;
        let fraction = temperature / denominator;
        BsimSoiThermalScalar::new(
            (-self.bandgap_a * temperature).mul_add(fraction, self.bandgap_zero),
            -self.bandgap_a * fraction * (2. - fraction),
        )
    }

    fn density(self, temperature: f64, nominal: f64, live: bool) -> BsimSoiThermalScalar {
        let gap = self.gap(temperature);
        let voltage = K_OVER_Q * temperature;
        let (prefactor, reference) = if self.generalized {
            (
                self.density_nominal * (temperature / nominal).powf(1.5),
                self.gap(nominal).value / (2. * K_OVER_Q * nominal),
            )
        } else {
            // The live source uses a rounded silicon T^-3/2 reference.
            let scale = if live {
                1.923_058_4e-4
            } else {
                300.15_f64.powf(-1.5)
            };
            (
                1.45e10 * scale * temperature * temperature.sqrt(),
                21.556_598_1,
            )
        };
        let density = prefactor * (reference - gap.value / (2. * voltage)).exp();
        let log_slope =
            1.5 / temperature + (gap.value / temperature - gap.d_temperature) / (2. * voltage);
        BsimSoiThermalScalar::new(density, density * log_slope)
    }
}

/// Immutable material and passive thermal inputs for one resolved instance.
/// The producer uses absolute kelvin at evaluation, with no circuit state mutation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiTemperature {
    material: Material,
    nominal: f64,
    ambient: f64,
    channel_doping: f64,
    substrate_doping: f64,
    polarity: f64,
    network: Option<BsimSoiThermalNetwork>,
}

impl PreparedBsimSoiTemperature {
    /// The native self-heating branch, if it exists.
    #[must_use = "the network determines the passive heat residual and Jacobian"]
    pub const fn network(&self) -> Option<BsimSoiThermalNetwork> {
        self.network
    }

    /// Bound circuit ambient temperature in kelvin.
    #[must_use = "absolute thermal coordinates use this zero-rise reference"]
    pub const fn ambient_kelvin(&self) -> f64 {
        self.ambient
    }

    pub(super) fn ambient_point(self) -> Result<BsimSoiTemperaturePoint, ModelError> {
        Self {
            network: None,
            ..self
        }
        .evaluate(self.ambient)
    }

    pub(super) const fn doping(&self) -> [f64; 2] {
        [self.channel_doping, self.substrate_doping]
    }

    pub(super) const fn substrate_permittivity(&self) -> f64 {
        self.material.permittivity
    }

    /// Evaluate the live or isothermal material laws at an absolute temperature.
    /// Isothermal instances use the bound ambient regardless of trial input;
    /// their returned local thermal derivatives are zero.
    ///
    /// # Errors
    /// Invalid physical domain or nonfinite constitutive result.
    pub fn evaluate(&self, kelvin: f64) -> Result<BsimSoiTemperaturePoint, ModelError> {
        let live = self.network.is_some();
        let temperature = positive(
            if live { kelvin } else { self.ambient },
            "BSIMSOI temperature",
        )?;
        let voltage = BsimSoiThermalScalar::new(K_OVER_Q * temperature, K_OVER_Q);
        let bandgap = self.material.gap(temperature).checked("bandgap")?;
        let density = self
            .material
            .density(temperature, self.nominal, live)
            .checked("intrinsic density")?;
        positive(density.value, "intrinsic density")?;
        let logarithm = (self.channel_doping / density.value).ln();
        let logarithm_slope = -density.d_temperature / density.value;
        let phi = BsimSoiThermalScalar::new(
            2. * voltage.value * logarithm,
            2. * voltage
                .d_temperature
                .mul_add(logarithm, voltage.value * logarithm_slope),
        );
        positive(phi.value, "surface potential")?;
        let vbi_log = (1e20 / density.value).ln() + logarithm;
        let vbi = BsimSoiThermalScalar::new(
            voltage.value * vbi_log,
            voltage
                .d_temperature
                .mul_add(vbi_log, 2. * voltage.value * logarithm_slope),
        );
        let (back_log, back_slope) = if self.substrate_doping > 0. {
            ((self.channel_doping / self.substrate_doping).ln(), 0.)
        } else {
            (
                logarithm + (-self.substrate_doping / density.value).ln(),
                2. * logarithm_slope,
            )
        };
        let backgate = BsimSoiThermalScalar::new(
            -self.polarity * voltage.value * back_log,
            -self.polarity
                * voltage
                    .d_temperature
                    .mul_add(back_log, voltage.value * back_slope),
        );
        let width = (2. * self.material.permittivity * phi.value
            / (CHARGE * self.channel_doping * 1e6))
            .sqrt();
        let capacitance = self.material.permittivity / width;
        let relative_width = 0.5 * phi.d_temperature / phi.value;
        let slope = if live { 1. } else { 0. };
        let scale = |mut scalar: BsimSoiThermalScalar, name| {
            scalar.d_temperature *= slope;
            scalar.checked(name)
        };
        let (conductive_heat, stored_heat) = self.network.map_or(
            (
                BsimSoiThermalScalar::new(0., 0.),
                BsimSoiThermalScalar::new(0., 0.),
            ),
            |network| {
                let rise = temperature - self.ambient;
                (
                    BsimSoiThermalScalar::new(network.conductance * rise, network.conductance),
                    BsimSoiThermalScalar::new(network.heat_capacity * rise, network.heat_capacity),
                )
            },
        );
        Ok(BsimSoiTemperaturePoint {
            kelvin: temperature,
            thermal_voltage: scale(voltage, "thermal voltage")?,
            bandgap: scale(bandgap, "bandgap")?,
            intrinsic_density: scale(density, "intrinsic density")?,
            surface_potential: scale(phi, "surface potential")?,
            built_in_potential: scale(vbi, "built-in potential")?,
            backgate_flatband: scale(backgate, "backgate flatband")?,
            depletion_width: scale(
                BsimSoiThermalScalar::new(width, width * relative_width),
                "depletion width",
            )?,
            depletion_capacitance: scale(
                BsimSoiThermalScalar::new(capacitance, -capacitance * relative_width),
                "depletion capacitance",
            )?,
            conductive_heat: conductive_heat.checked("conductive heat")?,
            stored_heat: stored_heat.checked("stored heat")?,
        })
    }
}

impl PreparedBsimSoiSetup {
    /// Bind the leading temperature-sensitive inputs to an existing resolved instance.
    ///
    /// # Errors
    /// Invalid instance index, temperature, material, doping or heat-network geometry.
    pub fn prepare_temperature(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiTemperature, ModelError> {
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::InvalidPhysicalParameter(
                "BSIMSOI instance index",
            ))?;
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let model = self.model.parameters();
        let params = instance.parameters();
        let generalized = integer(model, "mtrlmod")? != 0;
        let material = if generalized {
            Material {
                bandgap_zero: real(model, "bg0sub")?,
                bandgap_a: real(model, "tbgasub")?,
                bandgap_b: real(model, "tbgbsub")?,
                density_nominal: positive(real(model, "ni0sub")?, "ni0sub")?,
                permittivity: positive(EPS_0 * real(model, "epsrsub")?, "epsrsub")?,
                generalized,
            }
        } else {
            Material {
                bandgap_zero: 1.16,
                bandgap_a: 7.02e-4,
                bandgap_b: 1108.,
                density_nominal: 1.45e10,
                permittivity: EPS_SI,
                generalized,
            }
        };
        let nch = slot(ParameterScope::Model, "nch")?;
        let gamma1 = slot(ParameterScope::Model, "gamma1")?;
        let channel = if !self.model.was_given(nch) && self.model.was_given(gamma1) {
            let oxide = if generalized {
                3.9 * EPS_0 / positive(real(model, "eot")?, "eot")?
            } else {
                EPS_OX / positive(real(model, "tox")?, "tox")?
            };
            3.021e22 * (real(model, "gamma1")? * oxide).powi(2)
        } else {
            size.binned_model_slot(nch)
                .ok_or(ModelError::MissingDefaultDependency(nch))?
        };
        let nsub = slot(ParameterScope::Model, "nsub")?;
        let substrate = size
            .binned_model_slot(nsub)
            .ok_or(ModelError::MissingDefaultDependency(nsub))?;
        if substrate == 0. {
            return Err(ModelError::InvalidPhysicalParameter("nsub"));
        }
        let node = instance.topology().temperature;
        let network = if node == BsimSoiNode::Ground {
            None
        } else {
            let width = positive(size.geometry().weff + real(model, "wth0")?, "thermal width")?;
            let fingers_per_segment =
                positive(real(params, "nf")? / real(params, "nseg")?, "nf/nseg")?;
            let factor = positive(width * fingers_per_segment, "thermal geometry")?;
            let multiplicity = real(params, "m")?;
            let resistance = positive(real(params, "rth0")? / factor, "thermal resistance")?;
            let capacity =
                BsimSoiThermalScalar::new(multiplicity * real(params, "cth0")? * factor, 0.)
                    .checked("heat capacity")?
                    .value;
            Some(BsimSoiThermalNetwork {
                node,
                conductance: positive(multiplicity / resistance, "thermal conductance")?,
                heat_capacity: capacity,
            })
        };
        Ok(PreparedBsimSoiTemperature {
            material,
            nominal: positive(real(model, "tnom")?, "tnom")?,
            ambient: positive(ambient, "ambient temperature")?,
            channel_doping: positive(channel, "nch")?,
            substrate_doping: substrate,
            polarity: if self.model.polarity() == MosPolarity::Nmos {
                1.
            } else {
                -1.
            },
            network,
        })
    }
}
