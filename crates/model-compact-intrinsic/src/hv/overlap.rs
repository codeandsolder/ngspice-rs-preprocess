//! HiSIM-HV overlap charge on intrinsic and external physical nodes.
use super::{
    HiSimHvAccumulationInputs, HiSimHvChannelElectrostaticPoint, HiSimHvChannelTerminalCharges,
    HiSimHvExternalBias, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, HiSimHvSurfaceInputs,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    evaluate_hisim_hv_accumulation, evaluate_hisim_hv_surface, instance::model_real,
    instance_real_at, lookup_slot, prepare_hisim_hv_quantum_oxide, smoothing::upper,
};
use crate::{ModelError, ModelVariant, ParameterScope, ParameterValue};
const EPS: f64 = 10. * f64::EPSILON;
const SILICON_PERMITTIVITY: f64 = 1.034_943e-10;
const ELECTRON_CHARGE: f64 = 1.602_191_8e-19;

/// Overlap charge evaluated in normalized source-referenced coordinates.
/// Reverse mode exchanges the physical source and drain parameter sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvOverlapBias {
    pub intrinsic: HiSimHvIntrinsicBias,
    pub external: HiSimHvExternalBias,
    pub reversed: bool,
}

/// Separate conservative charge vectors.
///
/// Derivatives use intrinsic and external (Vgs,Vds,Vbs,T) coordinates
/// respectively. Prime charges are gate-to-external-prime pairs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvChargeNetwork {
    pub intrinsic: HiSimHvChannelTerminalCharges,
    pub external: HiSimHvChannelTerminalCharges,
    pub gate_drain_prime: Jet,
    pub gate_source_prime: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Side {
    length: f64,
    doping: f64,
    physical: bool,
    constant_capacitance: f64,
}

/// Size-specific native overlap coefficients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvOverlap {
    variant: ModelVariant,
    enabled: bool,
    source: Side,
    drain: Side,
    oxide: f64,
    width: f64,
    bulk_capacitance: f64,
    flatband: f64,
    slope: f64,
    magnitude: f64,
    external_fraction: f64,
    mode: i32,
    extra_drain_charge: f64,
}

#[derive(Clone, Copy)]
struct Density {
    total: Jet,
    depletion: Jet,
    inversion: Jet,
}
#[derive(Clone, Copy)]
struct LocalMaterial {
    beta: Jet,
    ni: Jet,
    bandgap: Jet,
    maximum_body: Jet,
}

fn floor(value: Jet, minimum: f64) -> Jet {
    if value.value < minimum {
        Jet::constant(minimum)
    } else {
        value
    }
}

fn analytical_chi(
    overdrive: Jet,
    beta: Jet,
    factor: Jet,
    minority: Jet,
    body: Jet,
    mode: i32,
) -> Jet {
    let psi = beta * overdrive;
    let bf = beta * factor;
    let scale = bf * bf;
    let mut chi = Jet::constant(3.);
    for _ in 0..2 {
        let term = psi - 1. + (-chi).exp();
        let radicand = floor(1. + 4. * term / scale, EPS);
        chi = psi - 2. * term / (1. + radicand.sqrt());
    }
    if chi.value < 3. {
        // Native cubic matches depletion charge and its slope at Chi=3.
        let exp_boundary = (-3_f64).exp();
        let boundary_root = (2. + exp_boundary).sqrt();
        let cubic =
            1. / (9. * 2_f64.sqrt()) - 7_f64.mul_add(exp_boundary, 5.) / (54. * boundary_root);
        let quadratic = (1. + exp_boundary) / (2. * boundary_root) - 2_f64.sqrt() / 3.;
        let linear = std::f64::consts::FRAC_1_SQRT_2 + 1. / bf;
        let constant = -overdrive / factor;
        let p = (3. * cubic * linear - quadratic * quadratic) / (9. * cubic * cubic);
        let q = quadratic.powi(3) / (27. * cubic.powi(3))
            - quadratic * linear / (6. * cubic * cubic)
            + constant / (2. * cubic);
        let p_cubed = p * p * p;
        let root = (q * q + p_cubed).sqrt();
        let radicand = if q.value > 0. {
            p_cubed / (root + q)
        } else {
            root - q
        };
        let u = radicand.positive_pow(1. / 3.);
        // Cardano product u*v=-p avoids subtracting two nearly equal radicals.
        chi = u - p / u - quadratic / (3. * cubic);
        let residual = ((cubic * chi + quadratic) * chi + linear) * chi + constant;
        chi = chi - residual / ((3. * cubic * chi + 2. * quadratic) * chi + linear);
    }
    if mode > 0 {
        let log_gamma = minority.ln() + beta * body;
        let p = beta * (overdrive + 0.1);
        let log_scale = log_gamma + scale.ln();
        let log_bound = |x: Jet| {
            let square = x * x;
            if square.value == 0. {
                return Jet::constant(0.);
            }
            let argument = square.ln() - log_scale;
            if argument.value > 0. {
                argument + (-argument).exp().ln_1p()
            } else {
                argument.exp().ln_1p()
            }
        };
        let first = upper(log_bound(p), p, 1.);
        let second = log_bound(p - first + beta * 0.1);
        chi = upper(chi, second, 0.06);
    }
    chi
}

impl PreparedHiSimHvOverlap {
    fn density(
        self,
        gate: Jet,
        body: Jet,
        doping: f64,
        material: LocalMaterial,
    ) -> Result<Density, ModelError> {
        let beta = material.beta;
        let body = super::bias_conditioning::confined_body(body, material.maximum_body).0 + 1e-12
            - if self.variant == ModelVariant::HiSimHv2 {
                EPS
            } else {
                0.
            };
        let poisson = (2. * SILICON_PERMITTIVITY * ELECTRON_CHARGE * doping / beta).sqrt();
        let minority = material.ni / doping;
        let minority = minority * minority;
        let gate = self.flatband - gate;
        let oxide = Jet::constant(self.oxide);
        if gate.value < body.value {
            let point = evaluate_hisim_hv_accumulation(
                ModelVariant::HiSimHv1,
                &HiSimHvAccumulationInputs {
                    effective_gate: gate,
                    confined_body: body,
                    inverse_thermal_voltage: beta,
                    poisson_coefficient: poisson,
                    oxide_capacitance: oxide,
                    bandgap: material.bandgap,
                    twice_bulk_potential: 2. / beta * (doping / material.ni).ln(),
                    minimum_gate_voltage: 0.,
                },
            )?;
            return Ok(Density {
                total: point.bulk_charge_density,
                depletion: point.bulk_charge_density,
                inversion: Jet::constant(0.),
            });
        }
        let factor = poisson / oxide;
        if self.mode == 1 {
            let surface = evaluate_hisim_hv_surface(&HiSimHvSurfaceInputs {
                effective_gate: gate,
                confined_body: body,
                quasi_fermi: Jet::constant(0.),
                inverse_thermal_voltage: beta,
                charge_factor: factor,
                minority_coefficient: minority,
            })?;
            let denominator = surface.total_root + surface.depletion_root;
            let inversion = if denominator.value == 0. {
                Jet::constant(0.)
            } else {
                poisson * surface.inversion_squared / denominator
            };
            return Ok(Density {
                total: poisson * surface.total_root,
                depletion: poisson * surface.depletion_root,
                inversion,
            });
        }
        let chi = analytical_chi(gate - body, beta, factor, minority, body, self.mode);
        let total = oxide * (gate - body - chi / beta);
        let depletion = poisson * floor(chi + (-chi).exp_m1(), EPS).sqrt();
        Ok(Density {
            total: total.checked("HiSIM-HV overlap total density")?,
            depletion: depletion.checked("HiSIM-HV overlap depletion density")?,
            inversion: (total - depletion).checked("HiSIM-HV overlap inversion density")?,
        })
    }

    /// Evaluate overlap alone, preserving the independent intrinsic and external
    /// derivatives. Returned rows use normalized orientation (swap rows when
    /// mapping reverse mode to physical nodes).
    /// # Errors
    /// Invalid local constitutive state or nonfinite charge.
    pub fn evaluate(
        self,
        bias: HiSimHvOverlapBias,
        channel: &HiSimHvChannelElectrostaticPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvChargeNetwork, ModelError> {
        let zero = Jet::constant(0.);
        let mut inner = Charges::default();
        let mut outer = Charges::default();
        let mut primes = [zero; 2];
        if self.enabled {
            let material = LocalMaterial {
                beta: Jet::coordinate(
                    temperature.inverse_thermal_voltage,
                    3,
                    temperature.inverse_thermal_voltage_derivative,
                ),
                ni: Jet::coordinate(
                    temperature.intrinsic_carrier_concentration_m3,
                    3,
                    temperature.intrinsic_carrier_derivative_m3_per_kelvin,
                ),
                bandgap: Jet::coordinate(
                    temperature.bandgap_ev,
                    3,
                    temperature.bandgap_derivative_ev_per_kelvin,
                ),
                maximum_body: channel.conditioned.maximum_body,
            };
            let intrinsic = voltages(bias.intrinsic.vgs, bias.intrinsic.vds, bias.intrinsic.vbs);
            let external = voltages(bias.external.vgs, bias.external.vds, bias.external.vbs);
            let sides = if bias.reversed {
                [self.drain, self.source]
            } else {
                [self.source, self.drain]
            };
            let qextra =
                self.extra_drain_charge * (intrinsic[1] - channel.charge.surface_difference);
            for (index, side) in sides.into_iter().enumerate() {
                let drain = index == 1;
                let local = if drain { intrinsic[1] } else { zero };
                let local_outer = if drain { external[1] } else { zero };
                primes[index] = side.constant_capacitance * (external[0] - local_outer);
                if !side.physical || side.length <= 0. {
                    continue;
                }
                if side.doping == 0. {
                    let psi = if drain {
                        channel.charge.drain_surface
                    } else {
                        channel.charge.source_surface
                    };
                    let q = self.oxide
                        * self.width
                        * (side.length * (intrinsic[0] - local)
                            - self.slope
                                * (self.magnitude + intrinsic[0] - local)
                                * (1.2 + local - psi));
                    inner.add_side(drain, -q, -q, zero);
                } else if side.doping > 0. {
                    let density = self.density(
                        intrinsic[0] - intrinsic[2],
                        intrinsic[2] - local,
                        side.doping,
                        material,
                    )?;
                    let additional = if drain && !bias.reversed {
                        qextra
                    } else if !drain && bias.reversed {
                        -qextra
                    } else {
                        zero
                    };
                    let area = self.width * side.length;
                    inner.add_side(
                        drain,
                        area * (1. - self.external_fraction) * (density.total + additional),
                        area * (1. - self.external_fraction) * (density.depletion + additional),
                        area * (1. - self.external_fraction) * density.inversion,
                    );
                    if self.external_fraction != 0. {
                        let density = self.density(
                            external[0] - external[2],
                            external[2] - local_outer,
                            side.doping,
                            material,
                        )?;
                        outer.add_side(
                            drain,
                            area * self.external_fraction * density.total,
                            area * self.external_fraction * density.depletion,
                            area * self.external_fraction * density.inversion,
                        );
                    }
                }
            }
            let bulk = self.bulk_capacitance * (intrinsic[0] - intrinsic[2]);
            inner.gate = inner.gate + bulk;
            inner.bulk = inner.bulk - bulk;
        }
        Ok(HiSimHvChargeNetwork {
            intrinsic: inner.finish()?,
            external: outer.finish()?,
            gate_source_prime: primes[0].checked("HiSIM-HV constant source overlap")?,
            gate_drain_prime: primes[1].checked("HiSIM-HV constant drain overlap")?,
        })
    }
}
const fn voltages(g: f64, d: f64, b: f64) -> [Jet; 3] {
    [
        Jet::coordinate(g, 0, 1.),
        Jet::coordinate(d, 1, 1.),
        Jet::coordinate(b, 2, 1.),
    ]
}
#[derive(Default)]
struct Charges {
    gate: Jet,
    drain: Jet,
    source: Jet,
    bulk: Jet,
}
impl Charges {
    fn add_side(&mut self, drain: bool, total: Jet, depletion: Jet, inversion: Jet) {
        self.gate = self.gate - total;
        if drain {
            self.drain = self.drain + depletion;
        } else {
            self.source = self.source + depletion;
        }
        self.bulk = self.bulk + inversion;
    }
    fn finish(self) -> Result<HiSimHvChannelTerminalCharges, ModelError> {
        Ok(HiSimHvChannelTerminalCharges {
            gate: self.gate.checked("HiSIM-HV overlap gate charge")?,
            drain: self.drain.checked("HiSIM-HV overlap drain charge")?,
            bulk: self.bulk.checked("HiSIM-HV overlap bulk charge")?,
            source: self.source.checked("HiSIM-HV overlap source charge")?,
        })
    }
}

/// Prepare both physical sides and all overlap coefficients.
/// # Errors
/// Invalid model binding, missing size coefficient or invalid physical geometry.
pub fn prepare_hisim_hv_overlap(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvOverlap, ModelError> {
    model.validate_instance(instance)?;
    let binned = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        instance
            .binned_model_slot(slot)
            .ok_or(ModelError::MissingDefaultDependency(slot))
    };
    let selector = |name| {
        let slot = lookup_slot(model.variant(), name)?;
        match model.get_slot(slot) {
            Some(ParameterValue::Integer(value)) => Ok(*value),
            _ => Err(ModelError::MissingDefaultDependency(slot)),
        }
    };
    let local = |name: &str| {
        let slot = model
            .variant()
            .parameter(ParameterScope::Instance, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?
            .slot;
        instance_real_at(instance, slot)
    };
    let width = instance.effective_geometry().capacitance_width_total_m * local("m")?;
    let oxide = prepare_hisim_hv_quantum_oxide(model, instance)?.zero_field_capacitance();
    let side = |length, doping, flag, constant| -> Result<Side, ModelError> {
        let length = local(length)?;
        let physical = selector(flag)? != 0;
        let slot = lookup_slot(model.variant(), constant)?;
        let c = if model.explicitly_given(slot) {
            binned(constant)?
        } else if physical {
            0.
        } else {
            oxide * length
        };
        Ok(Side {
            length,
            doping: binned(doping)? * 1e6,
            physical,
            constant_capacitance: width * c,
        })
    };
    Ok(PreparedHiSimHvOverlap {
        variant: model.variant(),
        enabled: selector("coadov")? == 1,
        source: side("lovers", "novers", "coovlps", "cgso")?,
        drain: side("loverld", "nover", "coovlp", "cgdo")?,
        oxide,
        width,
        bulk_capacitance: binned("cgbo")? * instance.geometry().gate_length_m * local("m")?,
        flatband: -binned("vfbover")?,
        slope: model_real(model, "ovslp")? / 100.,
        magnitude: model_real(model, "ovmag")?,
        external_fraction: binned("cvdsover")?,
        mode: selector("coqovsm")?,
        extra_drain_charge: if model.variant() == ModelVariant::HiSimHv2 {
            model_real(model, "qovadd")?
        } else {
            0.
        },
    })
}
