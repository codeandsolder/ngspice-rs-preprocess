//! HICUM/L2 2.4.0 nominal-to-effective-temperature parameter preparation.
//!
//! Source: hicumL2V2p4p0.va, INSTANCE thermal update and TMPHICJ. The three
//! flcomp branches (2.1-compatible, 2.2, and >=2.3) have distinct laws.
//! All returned coefficient blocks are immutable and fully temperature
//! prepared; the solver owns thermal network iteration and trial state.

use super::{
    Parameters as Diodes, Polarity, device, external, junction, noise, nqs, thermal, transport,
};
use crate::{ModelError, Temperature, validate_nonnegative, validate_positive};

const SOURCE_K: f64 = 1.380_650_3e-23;
const SOURCE_Q: f64 = 1.602_176_462e-19;
const CELSIUS_ZERO: f64 = 273.15;
const MINIMUM_KELVIN: f64 = 173.15;
const MAXIMUM_KELVIN: f64 = 600.0;
const LEGACY_LIMEXP: f64 = 11.0;

/// Nominal source temperature coefficients.
///
/// Values not temperature-dependent by HICUM must remain in their existing
/// family-typed parameter block, rather than being copied into a key map.
#[derive(Clone, Copy, Debug)]
pub struct ThermalCoefficients {
    /// Nominal temperature in Celsius; source default 27 C.
    pub tnom_celsius: f64,
    /// Local instance temperature offset DT in kelvin.
    pub dt: f64,
    pub vgb: f64,
    pub vge: f64,
    pub vgc: f64,
    pub vgs: f64,
    pub f1vg: f64,
    pub f2vg: f64,
    pub zetact: f64,
    pub zetabet: f64,
    pub zetaci: f64,
    pub zetacx: f64,
    pub zetarbi: f64,
    pub zetarbx: f64,
    pub zetarcx: f64,
    pub zetare: f64,
    pub alb: f64,
    pub alvs: f64,
    pub alces: f64,
    pub alt0: f64,
    pub kt0: f64,
    pub zetahjei: f64,
    pub zetavgbe: f64,
    pub dvgbe: f64,
    pub alfav: f64,
    pub alqav: f64,
    pub alkav: f64,
    pub zetarth: f64,
    pub alrth: f64,
}

impl ThermalCoefficients {
    const fn avalanche_slopes(self) -> super::avalanche::TemperatureSlopes {
        super::avalanche::TemperatureSlopes {
            favl: self.alfav,
            qavl: self.alqav,
            kavl: self.alkav,
        }
    }
}

impl Default for ThermalCoefficients {
    fn default() -> Self {
        Self {
            tnom_celsius: 27.0,
            dt: 0.0,
            vgb: 1.17,
            vge: 1.17,
            vgc: 1.17,
            vgs: 1.17,
            f1vg: -1.023_77e-4,
            f2vg: 4.321_5e-4,
            zetact: 3.0,
            zetabet: 3.5,
            zetaci: 0.0,
            zetacx: 1.0,
            zetarbi: 0.0,
            zetarbx: 0.0,
            zetarcx: 0.0,
            zetare: 0.0,
            alb: 0.0,
            alvs: 0.0,
            alces: 0.0,
            alt0: 0.0,
            kt0: 0.0,
            zetahjei: 1.0,
            zetavgbe: 1.0,
            dvgbe: 0.0,
            alfav: 0.0,
            alqav: 0.0,
            alkav: 0.0,
            zetarth: 0.0,
            alrth: 0.0,
        }
    }
}

/// Other HICUM temperature-dependent coefficients.
///
/// Owned by separate full-device laws. These remain accessible even before those
/// electrical branch kernels and solver bindings are implemented.
#[derive(Clone, Copy, Debug)]
pub struct AuxiliaryCoefficients {
    pub itss: f64,
    pub msf: f64,
    pub tsf: f64,
    pub rbi0: f64,
    /// Nominal source intrinsic-base modulation FDQR0.
    pub fdqr0: f64,
    /// Emitter-current crowding geometry FGEO.
    pub fgeo: f64,
    /// Stored minority-carrier charge partition FQI.
    pub fqi: f64,
    /// High-frequency internal-base shunt ratio FCRBI.
    pub fcrbi: f64,
    /// Source FLNQS gate for vertical excess-phase auxiliary states.
    pub flnqs: bool,
    pub rbx: f64,
    pub rcx: f64,
    pub re: f64,
    pub rsu: f64,
    /// Source CSU: shunt across substrate resistor, only if RSU exists.
    pub csu: f64,
    /// Source raw isolation capacitances and partition flags.
    pub cbepar: f64,
    pub fbepar: f64,
    pub cbcpar: f64,
    pub fbcpar: f64,
    pub favl: f64,
    pub qavl: f64,
    pub kavl: f64,
    pub ibets: f64,
    pub abet: f64,
    /// 0: intrinsic B-E tunneling; 1: peripheral B-E tunneling.
    pub tunode: u8,
    pub rth: f64,
    /// Source FLSH=0/1/2; 1 counts transport and avalanche power,
    /// 2 also counts internal diodes and series dissipation.
    pub flsh: u8,
    /// Source thermal capacitance CTH, J/K.
    pub cth: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Nominal {
    /// Source TYPE: NPN +1 or PNP -1. All physics takes
    /// polarity-normalized internal branch voltages.
    pub polarity: Polarity,
    /// Family-typed *nominal* HICUM diode parameters.
    pub diodes: Diodes,
    /// Family-typed *nominal* transport and depletion parameters.
    /// The nominal external collector junction capacitances contribute
    /// their sum as the raw collector capacitance; the source partitioning
    /// below is authoritative when preparing the actual junctions.
    pub transport: transport::Parameters,
    pub coefficients: ThermalCoefficients,
    pub auxiliary: AuxiliaryCoefficients,
    /// Source FLCONO/KF/AF/CFBE/KFRE/AFRE and NQS shot correlation factors.
    pub noise: noise::Parameters,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    pub polarity: Polarity,
    pub effective_temperature: Temperature,
    pub nominal_temperature: Temperature,
    /// Original nominal RBI0 controls source physical node reduction,
    /// independently of temperature-scaled effective intrinsic RBI.
    pub nominal_rbi0: f64,
    pub diodes: Diodes,
    /// Source exact dln(Is)/dTdev for each HICDIO family.
    pub diode_saturation_slopes: super::SaturationSlopes,
    /// Analytic physical TMPHICJ coefficient temperature slopes.
    pub junction_parameter_slopes: junction::ParameterSlopes,
    /// Exact source dln ITSS/dTdev and dln TSF/dTdev.
    pub substrate_temperature_slopes: super::substrate::TemperatureSlopes,
    /// Exact source IBETS and ABET bandgap/junction slopes.
    pub tunneling_temperature_slopes: super::tunneling::TemperatureSlopes,
    /// Source dln FAVL/QAVL/KAVL per kelvin.
    pub avalanche_temperature_slopes: super::avalanche::TemperatureSlopes,
    /// Exact HICICK and forward low-transit source parameter slopes.
    pub transport_temperature_slopes: transport::TemperatureSlopes,
    /// Exact source RBI0(Tdev) and QP0(Tdev) coupling to HICRBI.
    pub internal_base_temperature_slopes: super::internal_base::TemperatureSlopes,
    pub transport: transport::Parameters,
    pub auxiliary: AuxiliaryCoefficients,
    pub noise: noise::Parameters,
    /// Exact source electrothermal KCL prepared at this thermal trial.
    pub thermal: thermal::Prepared,
    /// Source nominal branch topology and external parasitic partitions.
    pub external: external::Prepared,
}

impl Prepared {
    /// Assemble all HICUM 2.4.0 source physical branches from one
    /// effective-temperature preparation and a single nominal topology.
    ///
    /// # Errors
    /// Reports the first inconsistent physical coefficient family.
    pub fn device_kernel(&self) -> Result<device::Prepared, ModelError> {
        device::Prepared::new(self)
    }

    /// One immutable preparation shared by every evaluation at the same
    /// effective temperature. No per-step model-name or coefficient lookup.
    ///
    /// # Errors
    /// Rejects nonphysical transformed coefficients rather than silently
    /// evaluating a different compact model or an approximate substitute.
    pub fn diode_kernel(&self) -> Result<super::Prepared, ModelError> {
        super::Prepared::with_temperature_slopes(
            self.diodes,
            self.effective_temperature,
            self.diode_saturation_slopes,
        )
    }

    /// # Errors
    /// Rejects nonphysical HICCR/geometry/depletion parameters.
    pub fn transport_kernel(&self) -> Result<transport::Prepared, ModelError> {
        transport::Prepared::with_temperature_slopes(
            &self.transport,
            self.effective_temperature,
            self.transport_temperature_slopes,
            self.junction_parameter_slopes,
        )
    }

    /// # Errors
    /// Rejects nonphysical transformed junction values.
    pub fn junction_kernel(&self) -> Result<junction::Prepared, ModelError> {
        junction::Prepared::with_temperature_slopes(
            self.transport.junction,
            self.effective_temperature,
            self.junction_parameter_slopes,
        )
    }

    /// Prepare source HICAVL from the same immutable temperature and Cjci
    /// coefficients as HICCR and HICJQ.
    ///
    /// # Errors
    /// Reports unphysical avalanche or junction coefficients.
    pub fn avalanche_kernel(&self) -> Result<super::avalanche::Prepared, ModelError> {
        let collector = self.transport.junction.intrinsic_collector;
        super::avalanche::Prepared::with_temperature_slopes(
            super::avalanche::Parameters {
                favl: self.auxiliary.favl,
                qavl: self.auxiliary.qavl,
                kavl: self.auxiliary.kavl,
                collector_zero_capacitance: collector.zero_bias_capacitance,
                collector_potential: collector.potential,
            },
            self.avalanche_temperature_slopes,
        )
    }

    /// Prepare substrate `HSI_Tsu/Qdsu` from effective ITSS/TSF/MSF.
    ///
    /// # Errors
    /// Rejects invalid substrate coefficients or temperature.
    pub fn substrate_kernel(&self) -> Result<super::substrate::Prepared, ModelError> {
        super::substrate::Prepared::with_temperature_slopes(
            super::substrate::Parameters {
                saturation: self.auxiliary.itss,
                ideality: self.auxiliary.msf,
                forward_storage_time: self.auxiliary.tsf,
            },
            self.effective_temperature,
            self.substrate_temperature_slopes,
        )
    }

    /// Prepare source HICTUN B-E tunneling for the selected intrinsic or
    /// peripheral branch with fully temperature-adjusted QJMODF inputs.
    ///
    /// # Errors
    /// Reports invalid tunneling or junction parameters.
    pub fn tunneling_kernel(&self) -> Result<super::tunneling::Prepared, ModelError> {
        let p = self.transport.junction;
        super::tunneling::Prepared::with_temperature_slopes(
            super::tunneling::Parameters {
                saturation: self.auxiliary.ibets,
                attenuation: self.auxiliary.abet,
                location: if self.auxiliary.tunode == 0 {
                    super::tunneling::Location::Intrinsic
                } else {
                    super::tunneling::Location::Peripheral
                },
                intrinsic: p.intrinsic_emitter,
                peripheral: p.peripheral_emitter,
            },
            self.tunneling_temperature_slopes,
        )
    }

    /// Prepare the source HICRBI nonlinear resistance and Qrbi shunt with
    /// its temperature-adjusted Rbi0, Qp0, tr and geometry coefficients.
    ///
    /// # Errors
    /// Reports nonphysical intrinsic-base parameters.
    pub fn internal_base_kernel(&self) -> Result<super::internal_base::Prepared, ModelError> {
        let p = self.auxiliary;
        super::internal_base::Prepared::with_temperature_slopes(
            super::internal_base::Parameters {
                nominal_rbi0: self.nominal_rbi0,
                rbi0: p.rbi0,
                qp0: self.transport.qp0,
                fdqr0: p.fdqr0,
                fgeo: p.fgeo,
                fqi: p.fqi,
                fcrbi: p.fcrbi,
                tr: self.transport.tr,
            },
            self.effective_temperature,
            self.internal_base_temperature_slopes,
        )
    }

    /// Prepare the complete source HICUM noise spectra at device Tdev,
    /// including frequency-dependent correlated base/collector noise.
    ///
    /// # Errors
    /// Rejects invalid source noise parameters and effective temperature.
    pub fn noise_kernel(&self) -> Result<noise::Prepared, ModelError> {
        noise::Prepared::new(self.noise, self.effective_temperature)
    }

    /// Prepare vertical NQS from the same ALIT/ALQF shared with correlated
    /// noise, and the effective-temperature T0/FLcomp of HICCR.
    ///
    /// # Errors
    /// Reports invalid NQS source coefficients.
    pub fn nqs_kernel(&self) -> Result<nqs::Prepared, ModelError> {
        nqs::Prepared::new(nqs::Parameters {
            flnqs: self.auxiliary.flnqs,
            flcomp: self.transport.flcomp,
            alit: self.noise.alit,
            alqf: self.noise.alqf,
            t0: self.transport.t0,
        })
    }

    /// The immutable thermal trial kernel, including power/KCL/energy laws
    /// at this same effective device temperature.
    #[must_use = "use the prepared thermal kernel in the electrothermal evaluation"]
    pub const fn thermal_kernel(&self) -> thermal::Prepared {
        self.thermal
    }

    /// The immutable source nominal topology and external RC kernel.
    #[must_use = "use the prepared external kernel when evaluating branch dissipation"]
    pub const fn external_kernel(&self) -> external::Prepared {
        self.external
    }
}

#[derive(Clone, Copy)]
struct TemperatureContext {
    ratio: f64,
    log_ratio: f64,
    delta: f64,
    vt: f64,
    vt0: f64,
    mg: f64,
    effective_kelvin: f64,
    nominal_kelvin: f64,
}

#[derive(Clone, Copy, Debug)]
struct JunctionTemperature {
    capacitance: f64,
    potential: f64,
    maximum_or_punchthrough: f64,
    /// Exact TMPHICJ d(Cj0)/dTdev (F/K), d(Vd)/dTdev (V/K)
    /// and d(Aj or Vpt)/dTdev (1/K or V/K).
    capacitance_slope: f64,
    potential_slope: f64,
    maximum_slope: f64,
}

impl Nominal {
    /// Prepare a complete immutable temperature-adjusted family parameter
    /// set from nominal HICUM 2.4.0 coefficients, local instance DT,
    /// and a thermal-node trial temperature rise in kelvin.
    /// The HICUM source clamps effective device temperature to 173.15..600 K.
    ///
    /// # Errors
    /// Returns the offending named source coefficient or the first invalid
    /// prepared electrical model; never reverts to unadjusted coefficients.
    pub fn prepare(
        &self,
        ambient: Temperature,
        thermal_rise_kelvin: f64,
    ) -> Result<Prepared, ModelError> {
        self.validate()?;
        let c = &self.coefficients;
        let nominal_kelvin = c.tnom_celsius + CELSIUS_ZERO;
        let nominal_temperature = Temperature::new(nominal_kelvin)?;
        if !thermal_rise_kelvin.is_finite() {
            return Err(ModelError::InvalidParameter("thermal_rise"));
        }
        let self_heating =
            self.auxiliary.flsh != 0 && self.auxiliary.rth >= thermal::MIN_SOURCE_RESISTANCE;
        if !self_heating && thermal_rise_kelvin != 0.0 {
            return Err(ModelError::InvalidParameter("thermal_rise"));
        }
        let raw_kelvin = c.dt.mul_add(
            1.0,
            ambient.kelvin()
                + if self_heating {
                    thermal_rise_kelvin
                } else {
                    0.0
                },
        );
        if !raw_kelvin.is_finite() {
            return Err(ModelError::InvalidTemperature);
        }
        let effective_kelvin = raw_kelvin.clamp(MINIMUM_KELVIN, MAXIMUM_KELVIN);
        let temperature_slope =
            if self_heating && raw_kelvin > MINIMUM_KELVIN && raw_kelvin < MAXIMUM_KELVIN {
                1.0
            } else {
                0.0
            };
        let effective_temperature = Temperature::new(effective_kelvin)?;
        let ratio = effective_kelvin / nominal_kelvin;
        let log_ratio = ratio.ln();
        let ctx = TemperatureContext {
            ratio,
            log_ratio,
            delta: effective_kelvin - nominal_kelvin,
            vt: SOURCE_K / SOURCE_Q * effective_kelvin,
            vt0: SOURCE_K / SOURCE_Q * nominal_kelvin,
            mg: 3.0 - SOURCE_Q * c.f1vg / SOURCE_K,
            effective_kelvin,
            nominal_kelvin,
        };
        let external_caps = self.external_capacitances();
        let external_split = external_caps.split()?;
        let (transport, junction_parameter_slopes, transport_temperature_slopes) =
            self.prepare_transport(ctx, external_split)?;
        let diodes = self.prepare_diodes(ctx)?;
        let diode_saturation_slopes = self.prepare_diode_saturation_slopes(ctx);
        let substrate_temperature_slopes = self.prepare_substrate_temperature_slopes(ctx);
        let (auxiliary, tunneling_temperature_slopes) =
            self.prepare_auxiliary(ctx, transport.junction, junction_parameter_slopes)?;
        let avalanche_temperature_slopes = c.avalanche_slopes();
        let external = self.prepare_external(auxiliary, external_caps, effective_kelvin)?;
        let thermal = self.prepare_thermal(
            auxiliary,
            effective_temperature,
            nominal_temperature,
            thermal_rise_kelvin,
            temperature_slope,
        )?;
        let prepared = Prepared {
            polarity: self.polarity,
            effective_temperature,
            nominal_temperature,
            nominal_rbi0: self.auxiliary.rbi0,
            diodes,
            diode_saturation_slopes,
            substrate_temperature_slopes,
            tunneling_temperature_slopes,
            avalanche_temperature_slopes,
            transport_temperature_slopes,
            internal_base_temperature_slopes: self.rbi_slopes(
                ctx,
                auxiliary,
                transport_temperature_slopes,
            ),
            junction_parameter_slopes,
            transport,
            auxiliary,
            noise: self.noise,
            thermal,
            external,
        };
        prepared.diode_kernel()?;
        prepared.transport_kernel()?;
        prepared.internal_base_kernel()?;
        prepared.noise_kernel()?;
        prepared.nqs_kernel()?;
        validate_prepared_auxiliary(auxiliary)?;
        Ok(prepared)
    }

    fn prepare_thermal(
        &self,
        auxiliary: AuxiliaryCoefficients,
        effective_temperature: Temperature,
        nominal_temperature: Temperature,
        thermal_rise_kelvin: f64,
        temperature_slope: f64,
    ) -> Result<thermal::Prepared, ModelError> {
        let c = self.coefficients;
        thermal::Prepared::new(thermal::Parameters {
            mode: match self.auxiliary.flsh {
                0 => thermal::Mode::Disabled,
                1 => thermal::Mode::TransferAvalanche,
                2 => thermal::Mode::FullElectrical,
                _ => return Err(ModelError::InvalidParameter("flsh")),
            },
            nominal_resistance: self.auxiliary.rth,
            effective_resistance: auxiliary.rth,
            heat_capacity: self.auxiliary.cth,
            effective_temperature,
            nominal_temperature,
            zetarth: c.zetarth,
            alrth: c.alrth,
            thermal_rise: thermal_rise_kelvin,
            temperature_slope,
        })
    }

    fn rbi_slopes(
        &self,
        ctx: TemperatureContext,
        effective: AuxiliaryCoefficients,
        transport: transport::TemperatureSlopes,
    ) -> super::internal_base::TemperatureSlopes {
        super::internal_base::TemperatureSlopes {
            rbi0: effective.rbi0 * self.coefficients.zetarbi / ctx.effective_kelvin,
            qp0: transport.qp0,
        }
    }

    fn external_capacitances(&self) -> external::Capacitances {
        let junction = self.transport.junction.external_collector;
        let a = self.auxiliary;
        external::Capacitances {
            substrate_shunt: a.csu,
            base_emitter_parasitic: a.cbepar,
            base_emitter_partition: a.fbepar,
            base_collector_parasitic: a.cbcpar,
            base_collector_partition: a.fbcpar,
            external_collector_zero_bias: junction.external_zero_bias_capacitance
                + junction.peripheral_zero_bias_capacitance,
        }
    }

    fn prepare_external(
        &self,
        effective: AuxiliaryCoefficients,
        caps: external::Capacitances,
        effective_kelvin: f64,
    ) -> Result<external::Prepared, ModelError> {
        let a = self.auxiliary;
        external::Prepared::with_temperature_slope(
            external::Parameters {
                nominal: external::Resistances {
                    external_base: a.rbx,
                    external_collector: a.rcx,
                    emitter: a.re,
                    substrate: a.rsu,
                },
                effective: external::Resistances {
                    external_base: effective.rbx,
                    external_collector: effective.rcx,
                    emitter: effective.re,
                    substrate: effective.rsu,
                },
                capacitances: caps,
            },
            external::Resistances {
                external_base: self.coefficients.zetarbx / effective_kelvin,
                external_collector: self.coefficients.zetarcx / effective_kelvin,
                emitter: self.coefficients.zetare / effective_kelvin,
                substrate: 0.0,
            },
        )
    }

    fn validate(&self) -> Result<(), ModelError> {
        // Validate nominal family independently of the temperature at which
        // it will be used; a singular nominal junction cannot be repaired by
        // positive temperature scaling.
        let nominal_temperature = Temperature::new(self.coefficients.tnom_celsius + CELSIUS_ZERO)?;
        super::Prepared::new(self.diodes, nominal_temperature)?;
        transport::Prepared::new(&self.transport, nominal_temperature)?;
        noise::Prepared::new(self.noise, nominal_temperature)?;
        for (name, max_ratio) in [
            (
                "ajei",
                self.transport.junction.intrinsic_emitter.max_cap_ratio,
            ),
            (
                "ajep",
                self.transport.junction.peripheral_emitter.max_cap_ratio,
            ),
        ] {
            if max_ratio < 1.0 {
                return Err(ModelError::InvalidParameter(name));
            }
        }

        let c = &self.coefficients;
        for (name, value) in [
            ("dt", c.dt),
            ("f1vg", c.f1vg),
            ("f2vg", c.f2vg),
            ("zetact", c.zetact),
            ("zetabet", c.zetabet),
            ("zetaci", c.zetaci),
            ("zetacx", c.zetacx),
            ("zetarbi", c.zetarbi),
            ("zetarbx", c.zetarbx),
            ("zetarcx", c.zetarcx),
            ("zetare", c.zetare),
            ("alb", c.alb),
            ("alvs", c.alvs),
            ("alces", c.alces),
            ("alt0", c.alt0),
            ("kt0", c.kt0),
            ("zetahjei", c.zetahjei),
            ("zetavgbe", c.zetavgbe),
            ("dvgbe", c.dvgbe),
            ("alfav", c.alfav),
            ("alqav", c.alqav),
            ("alkav", c.alkav),
            ("zetarth", c.zetarth),
            ("alrth", c.alrth),
        ] {
            if !value.is_finite() {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        for (name, value) in [
            ("vgb", c.vgb),
            ("vge", c.vge),
            ("vgc", c.vgc),
            ("vgs", c.vgs),
            ("msf", self.auxiliary.msf),
        ] {
            validate_positive(name, value)?;
        }
        let a = self.auxiliary;
        if a.flsh > 2 {
            return Err(ModelError::InvalidParameter("flsh"));
        }
        validate_nonnegative("cth", a.cth)?;
        for (name, value) in [
            ("itss", a.itss),
            ("tsf", a.tsf),
            ("rbi0", a.rbi0),
            ("rbx", a.rbx),
            ("rcx", a.rcx),
            ("re", a.re),
            ("rsu", a.rsu),
            ("favl", a.favl),
            ("qavl", a.qavl),
            ("kavl", a.kavl),
            ("ibets", a.ibets),
            ("abet", a.abet),
            ("rth", a.rth),
        ] {
            validate_nonnegative(name, value)?;
        }
        if a.tunode > 1 {
            return Err(ModelError::InvalidParameter("tunode"));
        }
        Ok(())
    }

    /// Exact parasitic substrate current and storage-time thermal slopes.
    ///
    /// Legacy ITSS scales with the VGB/VT normalized bandgap term
    /// and 3*ln(T/Tnom), divided by MSF. Modern ITSS uses
    /// (MG-1.5)*ln(T/Tnom) and the VGC/VT bandgap term.
    fn prepare_substrate_temperature_slopes(
        &self,
        t: TemperatureContext,
    ) -> super::substrate::TemperatureSlopes {
        let inv_t = 1.0 / t.effective_kelvin;
        let c = self.coefficients;
        let saturation = if legacy(self.transport.flcomp) {
            3.0f64.mul_add(inv_t, c.vgb * inv_t / t.vt) / self.auxiliary.msf
        } else {
            (t.mg - 1.5).mul_add(inv_t, c.vgc * inv_t / t.vt)
        };
        super::substrate::TemperatureSlopes {
            saturation,
            storage_time: (c.zetacx - 1.0) * inv_t,
        }
    }

    /// Derivatives of all source HICDIO saturation exponents:
    /// d/dT[log(T/Tnom)]=1/T and
    /// d/dT[Vg/VT*(T/Tnom-1)]=Vg/(VT*T).
    fn prepare_diode_saturation_slopes(&self, t: TemperatureContext) -> super::SaturationSlopes {
        let c = self.coefficients;
        let p = self.diodes;
        let inv_t = 1.0 / t.effective_kelvin;
        let bandgap = |value: f64| value * inv_t / t.vt;
        if legacy(self.transport.flcomp) {
            let common = 3.0f64.mul_add(inv_t, bandgap(c.vgb));
            let slope_be = |ideality: f64| (self.transport.mcf / ideality).mul_add(common, -c.alb);
            super::SaturationSlopes {
                ibei: slope_be(p.mbei),
                irei: slope_be(p.mrei),
                ibep: slope_be(p.mbep),
                irep: slope_be(p.mrep),
                ibci: common / p.mbci,
                ijbcx: common / p.mbcx,
                ijsc: common / p.msc,
            }
        } else {
            let emitter = c.zetabet.mul_add(inv_t, bandgap(c.vge));
            let recombination = t.mg.mul_add(inv_t, bandgap(c.vgb.midpoint(c.vge)));
            let recombination_slope = |ideality: f64| {
                if self.transport.flcomp >= 2.3 {
                    recombination / ideality
                } else {
                    0.5 * recombination
                }
            };
            super::SaturationSlopes {
                ibei: emitter,
                irei: recombination_slope(p.mrei),
                ibep: emitter,
                irep: recombination_slope(p.mrep),
                ibci: (t.mg + 1.0 - c.zetaci).mul_add(inv_t, bandgap(c.vgc)),
                ijbcx: (t.mg + 1.0 - c.zetacx).mul_add(inv_t, bandgap(c.vgc)),
                ijsc: (t.mg - 1.5).mul_add(inv_t, bandgap(c.vgs)),
            }
        }
    }

    fn prepare_diodes(&self, t: TemperatureContext) -> Result<Diodes, ModelError> {
        let p = self.diodes;
        let c = self.coefficients;
        let mut scaled = p;
        let relative_vg = t.ratio - 1.0;
        if legacy(self.transport.flcomp) {
            let vgt_over_vt = 3.0f64.mul_add(t.log_ratio, c.vgb / t.vt * relative_vg);
            scaled.ibeis = p.ibeis
                * ((self.transport.mcf / p.mbei).mul_add(vgt_over_vt, -c.alb * t.delta)).exp();
            scaled.ireis = p.ireis
                * ((self.transport.mcf / p.mrei).mul_add(vgt_over_vt, -c.alb * t.delta)).exp();
            scaled.ibeps = p.ibeps
                * ((self.transport.mcf / p.mbep).mul_add(vgt_over_vt, -c.alb * t.delta)).exp();
            scaled.ireps = p.ireps
                * ((self.transport.mcf / p.mrep).mul_add(vgt_over_vt, -c.alb * t.delta)).exp();
            scaled.ibcis = p.ibcis * (vgt_over_vt / p.mbci).exp();
            scaled.ibcxs = p.ibcxs * (vgt_over_vt / p.mbcx).exp();
            scaled.iscs = p.iscs * (vgt_over_vt / p.msc).exp();
        } else {
            let emitter_exp = c
                .zetabet
                .mul_add(t.log_ratio, c.vge / t.vt * relative_vg)
                .exp();
            scaled.ibeis = p.ibeis * emitter_exp;
            scaled.ibeps = p.ibeps * emitter_exp;
            let vgbe = c.vgb.midpoint(c.vge);
            let recombination_exp = if self.transport.flcomp >= 2.3 {
                t.mg.mul_add(t.log_ratio, vgbe / t.vt * relative_vg)
            } else {
                0.5 * t.mg.mul_add(t.log_ratio, vgbe / t.vt * relative_vg)
            };
            let divisor = self.transport.flcomp >= 2.3;
            scaled.ireis = p.ireis
                * (if divisor {
                    recombination_exp / p.mrei
                } else {
                    recombination_exp
                })
                .exp();
            scaled.ireps = p.ireps
                * (if divisor {
                    recombination_exp / p.mrep
                } else {
                    recombination_exp
                })
                .exp();
            let collector_exponent =
                (t.mg + 1.0 - c.zetaci).mul_add(t.log_ratio, c.vgc / t.vt * relative_vg);
            let external_exponent =
                (t.mg + 1.0 - c.zetacx).mul_add(t.log_ratio, c.vgc / t.vt * relative_vg);
            let substrate_exponent = (t.mg - 1.5).mul_add(t.log_ratio, c.vgs / t.vt * relative_vg);
            scaled.ibcis = p.ibcis * collector_exponent.exp();
            scaled.ibcxs = p.ibcxs * external_exponent.exp();
            scaled.iscs = p.iscs * substrate_exponent.exp();
        }
        super::Prepared::new(scaled, Temperature::new(t.effective_kelvin)?)?;
        Ok(scaled)
    }
}

impl JunctionTemperature {
    const fn slopes(self, factor: f64) -> junction::CoefficientSlopes {
        junction::CoefficientSlopes {
            zero_bias_capacitance: self.capacitance_slope * factor,
            potential: self.potential_slope,
            maximum_or_punchthrough: self.maximum_slope,
        }
    }

    const fn forward(self, original: junction::Forward) -> junction::Forward {
        junction::Forward {
            zero_bias_capacitance: self.capacitance,
            potential: self.potential,
            grading: original.grading,
            max_cap_ratio: self.maximum_or_punchthrough,
        }
    }

    const fn collector(self, original: junction::Collector) -> junction::Collector {
        junction::Collector {
            zero_bias_capacitance: self.capacitance,
            potential: self.potential,
            grading: original.grading,
            punch_through: self.maximum_or_punchthrough,
        }
    }

    const fn peripheral_substrate(
        self,
        original: junction::PeripheralSubstrate,
    ) -> junction::PeripheralSubstrate {
        junction::PeripheralSubstrate {
            zero_bias_capacitance: self.capacitance,
            potential: self.potential,
            grading: original.grading,
            punch_through: self.maximum_or_punchthrough,
        }
    }
}

impl Nominal {
    fn prepare_junctions(
        &self,
        t: TemperatureContext,
        split: external::Split,
    ) -> Result<(junction::Parameters, junction::ParameterSlopes), ModelError> {
        let j = self.transport.junction;
        let c = self.coefficients;
        let emitter_bandgap = c.vgb.midpoint(c.vge);
        let collector_bandgap = c.vgb.midpoint(c.vgc);
        let substrate_bandgap = c.vgs.midpoint(c.vgc);
        let emitter = prepare_junction(
            j.intrinsic_emitter.zero_bias_capacitance,
            j.intrinsic_emitter.potential,
            j.intrinsic_emitter.grading,
            j.intrinsic_emitter.max_cap_ratio,
            true,
            emitter_bandgap,
            t,
        )?;
        let peripheral_emitter = prepare_junction(
            j.peripheral_emitter.zero_bias_capacitance,
            j.peripheral_emitter.potential,
            j.peripheral_emitter.grading,
            j.peripheral_emitter.max_cap_ratio,
            true,
            emitter_bandgap,
            t,
        )?;
        let collector = prepare_junction(
            j.intrinsic_collector.zero_bias_capacitance,
            j.intrinsic_collector.potential,
            j.intrinsic_collector.grading,
            j.intrinsic_collector.punch_through,
            false,
            collector_bandgap,
            t,
        )?;
        // In the HICUM source the external capacitor partition is performed
        // before temperature preparation, and TMPHICJ is called with c_j=1.
        // Thus *both* capacitances share the same transformed geometry.
        let external = prepare_junction(
            1.0,
            j.external_collector.potential,
            j.external_collector.grading,
            j.external_collector.punch_through,
            false,
            collector_bandgap,
            t,
        )?;
        let substrate = prepare_junction(
            j.substrate.zero_bias_capacitance,
            j.substrate.potential,
            j.substrate.grading,
            j.substrate.punch_through,
            false,
            substrate_bandgap,
            t,
        )?;
        let perimeter = if j.peripheral_substrate.potential > 0.0 {
            prepare_junction(
                j.peripheral_substrate.zero_bias_capacitance,
                j.peripheral_substrate.potential,
                j.peripheral_substrate.grading,
                j.peripheral_substrate.punch_through,
                false,
                substrate_bandgap,
                t,
            )?
        } else {
            JunctionTemperature {
                capacitance: j.peripheral_substrate.zero_bias_capacitance,
                potential: j.peripheral_substrate.potential,
                maximum_or_punchthrough: j.peripheral_substrate.punch_through,
                capacitance_slope: 0.0,
                potential_slope: 0.0,
                maximum_slope: 0.0,
            }
        };
        let parameter_slopes = junction::ParameterSlopes {
            intrinsic_emitter: emitter.slopes(1.0),
            peripheral_emitter: peripheral_emitter.slopes(1.0),
            intrinsic_collector: collector.slopes(1.0),
            external_collector: external.slopes(split.external_collector_junction),
            peripheral_collector: external.slopes(split.peripheral_collector_junction),
            substrate: substrate.slopes(1.0),
            peripheral_substrate: perimeter.slopes(1.0),
        };
        Ok((
            junction::Parameters {
                intrinsic_emitter: emitter.forward(j.intrinsic_emitter),
                peripheral_emitter: peripheral_emitter.forward(j.peripheral_emitter),
                intrinsic_collector: collector.collector(j.intrinsic_collector),
                external_collector: junction::ExternalCollector {
                    external_zero_bias_capacitance: split.external_collector_junction
                        * external.capacitance,
                    peripheral_zero_bias_capacitance: split.peripheral_collector_junction
                        * external.capacitance,
                    potential: external.potential,
                    grading: j.external_collector.grading,
                    punch_through: external.maximum_or_punchthrough,
                },
                substrate: substrate.collector(j.substrate),
                peripheral_substrate: perimeter.peripheral_substrate(j.peripheral_substrate),
            },
            parameter_slopes,
        ))
    }

    fn prepare_transport_slopes(
        &self,
        t: TemperatureContext,
        result: &transport::Parameters,
        nominal_junction: junction::Parameters,
        junction_slopes: junction::ParameterSlopes,
        vlim_slope: f64,
    ) -> transport::TemperatureSlopes {
        let nominal = self.transport;
        let c = self.coefficients;
        let j = nominal_junction;
        let emitter = result.junction.intrinsic_emitter;
        let junction_parameter_slopes = junction_slopes;
        let inv_t = 1.0 / t.effective_kelvin;
        let emitter_potential_slope = junction_parameter_slopes.intrinsic_emitter.potential;
        let nominal_emitter_potential = j.intrinsic_emitter.potential;
        let emitter_grading = j.intrinsic_emitter.grading;
        let qp0_slope = if legacy(nominal.flcomp) {
            -nominal.qp0 * 0.5 * emitter_grading * emitter_potential_slope
                / nominal_emitter_potential
        } else {
            let ratio = emitter.potential / nominal_emitter_potential;
            -nominal.qp0
                * emitter_grading
                * ratio.powf(emitter_grading - 1.0)
                * emitter_potential_slope
                / nominal_emitter_potential
        };
        let tef0_slope = if legacy(nominal.flcomp) {
            let gain = c.alb.mul_add(t.delta, 1.0);
            let root = gain.hypot(0.1);
            let smooth = gain.midpoint(root);
            let dsmooth = 0.5 * c.alb * (1.0 + gain / root);
            result.tef0 * (inv_t - dsmooth / smooth)
        } else if nominal.flcomp < 2.3 {
            result.tef0
                * ((c.zetabet - c.zetact - 0.5).mul_add(inv_t, -((c.vgb - c.vge) * inv_t / t.vt)))
        } else {
            0.0
        };
        let source_beta = t.ratio.powf(c.zetavgbe);
        transport::TemperatureSlopes {
            c10: result.c10 * c.zetact.mul_add(inv_t, c.vgb * inv_t / t.vt),
            qp0: qp0_slope,
            ahjei: result.ahjei * c.zetahjei * inv_t,
            hjei_zero_bias: result.hjei_zero_bias * c.dvgbe * inv_t / t.vt
                * c.zetavgbe.mul_add(source_beta, -source_beta + 1.0),
            hf0: result.hf0 * c.dvgbe * inv_t / t.vt,
            hfe: if nominal.flcomp >= 2.3 {
                result.hfe * (c.vgb - c.vge) * inv_t / t.vt
            } else {
                0.0
            },
            hfc: if nominal.flcomp >= 2.3 {
                result.hfc * (c.vgb - c.vgc) * inv_t / t.vt
            } else {
                0.0
            },
            tef0: tef0_slope,
            thcs: result.thcs * (c.zetaci - 1.0) * inv_t,
            rci0: result.rci0 * c.zetaci / t.effective_kelvin,
            vlim: vlim_slope,
            vces: nominal.vces * c.alces,
            t0: nominal.t0 * c.kt0.mul_add(2.0 * t.delta, c.alt0),
        }
    }

    fn prepare_transport(
        &self,
        t: TemperatureContext,
        split: external::Split,
    ) -> Result<
        (
            transport::Parameters,
            junction::ParameterSlopes,
            transport::TemperatureSlopes,
        ),
        ModelError,
    > {
        let c = self.coefficients;
        let nominal = self.transport;
        let j = nominal.junction;
        let mut result = nominal;

        let (junction, junction_parameter_slopes) = self.prepare_junctions(t, split)?;
        result.junction = junction;
        let emitter = result.junction.intrinsic_emitter;
        let vg_relative = t.ratio - 1.0;
        result.c10 = nominal.c10
            * c.zetact
                .mul_add(t.log_ratio, c.vgb / t.vt * vg_relative)
                .exp();
        result.rci0 = nominal.rci0 * (c.zetaci * t.log_ratio).exp();
        result.vces = nominal.vces * c.alces.mul_add(t.delta, 1.0);
        result.t0 = nominal.t0 * (c.kt0 * t.delta).mul_add(t.delta, c.alt0.mul_add(t.delta, 1.0));
        result.thcs = nominal.thcs * ((c.zetaci - 1.0) * t.log_ratio).exp();
        result.ahjei = nominal.ahjei_nominal * (c.zetahjei * t.log_ratio).exp();
        result.hjei_zero_bias =
            nominal.hjei * (c.dvgbe / t.vt * (t.ratio.powf(c.zetavgbe) - 1.0)).exp();
        result.hf0 = nominal.hf0 * (c.dvgbe / t.vt * vg_relative).exp();
        if nominal.flcomp >= 2.3 {
            result.hfe = nominal.hfe * ((c.vgb - c.vge) / t.vt * vg_relative).exp();
            result.hfc = nominal.hfc * ((c.vgb - c.vgc) / t.vt * vg_relative).exp();
        }
        let vlim_slope;
        if legacy(nominal.flcomp) {
            let old_velocity =
                nominal.vlim * c.alvs.mul_add(-t.delta, 1.0) * (c.zetaci * t.log_ratio).exp();
            let normalized_velocity = old_velocity / t.vt - 1.0;
            // Differentiate the source HICVLIM legacy smoothed value,
            // including its thermal-voltage continuation at ALVS!=0.
            let factor = c.alvs.mul_add(-t.delta, 1.0);
            let exponent = (c.zetaci * t.log_ratio).exp();
            let old_slope =
                nominal.vlim * exponent * c.zetaci.mul_add(factor / t.effective_kelvin, -c.alvs);
            if normalized_velocity < LEGACY_LIMEXP {
                let exponential = normalized_velocity.exp();
                let softplus = exponential.ln_1p();
                result.vlim = t.vt * (1.0 + softplus);
                let normalized_slope =
                    old_slope / t.vt - old_velocity / (t.vt * t.effective_kelvin);
                vlim_slope = (t.vt * exponential / (1.0 + exponential)).mul_add(
                    normalized_slope,
                    (t.vt / t.effective_kelvin) * (1.0 + softplus),
                );
            } else {
                result.vlim = old_velocity;
                vlim_slope = old_slope;
            }
            // The old HICUM emitter-storage law has a positive smoothed
            // denominator even when 1+ALB*delta is negative.
            let gain_coefficient = c.alb.mul_add(t.delta, 1.0);
            let smoothed = gain_coefficient.midpoint(gain_coefficient.hypot(0.1));
            result.tef0 = nominal.tef0 * t.ratio / smoothed;
            result.qp0 = nominal.qp0
                * (0.5 * j.intrinsic_emitter.grading)
                    .mul_add(1.0 - emitter.potential / j.intrinsic_emitter.potential, 1.0);
        } else {
            result.vlim =
                nominal.vlim * (c.alvs.mul_add(-t.nominal_kelvin, c.zetaci) * t.log_ratio).exp();
            vlim_slope =
                result.vlim * c.alvs.mul_add(-t.nominal_kelvin, c.zetaci) / t.effective_kelvin;
            result.qp0 = nominal.qp0
                * (2.0
                    - (emitter.potential / j.intrinsic_emitter.potential)
                        .powf(j.intrinsic_emitter.grading));
            if nominal.flcomp < 2.3 {
                let exponent = (c.zetabet - c.zetact - 0.5)
                    .mul_add(t.log_ratio, -(c.vgb - c.vge) / t.vt * vg_relative);
                result.tef0 = nominal.tef0 * exponent.exp();
            }
        }
        let transport_slopes =
            self.prepare_transport_slopes(t, &result, j, junction_parameter_slopes, vlim_slope);
        transport::Prepared::with_temperature_slopes(
            &result,
            Temperature::new(t.effective_kelvin)?,
            transport_slopes,
            junction_parameter_slopes,
        )?;
        Ok((result, junction_parameter_slopes, transport_slopes))
    }
}

fn validate_prepared_auxiliary(auxiliary: AuxiliaryCoefficients) -> Result<(), ModelError> {
    for (name, value) in [
        ("itss", auxiliary.itss),
        ("tsf", auxiliary.tsf),
        ("rbi0", auxiliary.rbi0),
        ("rbx", auxiliary.rbx),
        ("rcx", auxiliary.rcx),
        ("re", auxiliary.re),
        ("favl", auxiliary.favl),
        ("qavl", auxiliary.qavl),
        ("kavl", auxiliary.kavl),
        ("ibets", auxiliary.ibets),
        ("abet", auxiliary.abet),
        ("rth", auxiliary.rth),
    ] {
        validate_nonnegative(name, value)?;
    }
    Ok(())
}

/// True only for the historical 2.1 branch of the HICUM 2.4.0 source.
fn legacy(flcomp: f64) -> bool {
    flcomp == 0.0 || flcomp.to_bits() == 2.1f64.to_bits()
}

/// Exact source TMPHICJ junction transform. For disabled Cj=0 the source
/// leaves vd and forward maximum/punch-through parameter unchanged.
fn prepare_junction(
    capacitance: f64,
    potential: f64,
    grading: f64,
    maximum_or_punchthrough: f64,
    forward_cap_ratio: bool,
    bandgap: f64,
    t: TemperatureContext,
) -> Result<JunctionTemperature, ModelError> {
    if capacitance == 0.0 {
        return Ok(JunctionTemperature {
            capacitance,
            potential,
            maximum_or_punchthrough,
            capacitance_slope: 0.0,
            potential_slope: 0.0,
            maximum_slope: 0.0,
        });
    }
    let normalized_potential = potential / (2.0 * t.vt0);
    // vdj0=2*vt0*ln(2*sinh(vd0/(2*vt0))), using expm1 to preserve
    // the small-potential cancellation while avoiding exp(vd0/vt0).
    let original_log = normalized_potential + (-(-2.0 * normalized_potential).exp_m1()).ln();
    let reference_junction_log = 2.0 * t.vt0 * original_log;
    let shifted_junction_potential = (t.mg * t.vt).mul_add(
        -t.log_ratio,
        (reference_junction_log - bandgap).mul_add(t.ratio, bandgap),
    );
    // vd_t = vdjt + 2*vt*ln((1+sqrt(1+4*exp(-vdjt/vt)))/2)
    //        = 2*vt*asinh(exp(vdjt/(2*vt))/2)
    // with a stable high-positive argument path.
    let scaled = shifted_junction_potential / (2.0 * t.vt);
    let transformed_potential = if scaled > 40.0 {
        (2.0 * t.vt).mul_add((-2.0 * scaled).exp().ln_1p(), shifted_junction_potential)
    } else {
        2.0 * t.vt * (0.5 * scaled.exp()).asinh()
    };
    // Differentiate the actual source TMPHICJ potential transform
    // without numerical differences. The same stable high-potential
    // branch applies to both its value and temperature slope.
    let k_over_q = SOURCE_K / SOURCE_Q;
    let shifted_slope = (t.mg * k_over_q).mul_add(
        -(t.log_ratio + 1.0),
        (reference_junction_log - bandgap) / t.nominal_kelvin,
    );
    let scaled_slope =
        (shifted_slope - shifted_junction_potential / t.effective_kelvin) / (2.0 * t.vt);
    let potential_slope = if scaled > 40.0 {
        let tail = (-2.0 * scaled).exp();
        (4.0 * t.vt * tail / (1.0 + tail)).mul_add(
            -scaled_slope,
            2.0f64.mul_add(k_over_q, tail.ln_1p()) + shifted_slope,
        )
    } else {
        let half_exp = 0.5 * scaled.exp();
        (2.0 * t.vt * half_exp / half_exp.hypot(1.0))
            .mul_add(scaled_slope, 2.0 * k_over_q * half_exp.asinh())
    };
    validate_positive("junction_temperature_potential", transformed_potential)?;
    let transformed_capacitance =
        capacitance * (grading * (potential / transformed_potential).ln()).exp();
    let transformed_other = if forward_cap_ratio {
        maximum_or_punchthrough * transformed_potential / potential
    } else {
        maximum_or_punchthrough
    };
    let capacitance_slope =
        -grading * transformed_capacitance * potential_slope / transformed_potential;
    let maximum_slope = if forward_cap_ratio {
        maximum_or_punchthrough * potential_slope / potential
    } else {
        0.0
    };
    validate_nonnegative("junction_temperature_capacitance", transformed_capacitance)?;
    validate_positive("junction_temperature_limit", transformed_other)?;
    if ![capacitance_slope, potential_slope, maximum_slope]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err(ModelError::InvalidParameter("junction_temperature_slope"));
    }
    Ok(JunctionTemperature {
        capacitance: transformed_capacitance,
        potential: transformed_potential,
        maximum_or_punchthrough: transformed_other,
        capacitance_slope,
        potential_slope,
        maximum_slope,
    })
}

impl Nominal {
    fn prepare_auxiliary(
        &self,
        t: TemperatureContext,
        junction: junction::Parameters,
        junction_slopes: junction::ParameterSlopes,
    ) -> Result<(AuxiliaryCoefficients, super::tunneling::TemperatureSlopes), ModelError> {
        let a = self.auxiliary;
        let c = self.coefficients;
        let mut result = a;
        let log = t.log_ratio;
        for (parameter, exponent) in [
            (&mut result.rbi0, c.zetarbi),
            (&mut result.rbx, c.zetarbx),
            (&mut result.rcx, c.zetarcx),
            (&mut result.re, c.zetare),
            (&mut result.tsf, c.zetacx - 1.0),
        ] {
            *parameter *= (exponent * log).exp();
        }
        result.favl = a.favl * (c.alfav * t.delta).exp();
        result.qavl = a.qavl * (c.alqav * t.delta).exp();
        result.kavl = a.kavl * (c.alkav * t.delta).exp();
        result.rth = a.rth * (c.zetarth * log).exp() * c.alrth.mul_add(t.delta, 1.0);
        result.itss = if legacy(self.transport.flcomp) {
            let base_exp = (c.vgb / t.vt).mul_add(t.ratio - 1.0, 3.0 * log);
            a.itss * (base_exp / a.msf).exp()
        } else {
            let substrate_exp = (t.mg - 1.5).mul_add(log, c.vgc / t.vt * (t.ratio - 1.0));
            a.itss * substrate_exp.exp()
        };

        // Tunneling coefficients from the source HICTUN_T branch.
        // Zero tunneling is a distinct source branch, with abet_t set to 1.
        let mut tunneling_slopes = super::tunneling::TemperatureSlopes::default();
        if a.ibets == 0.0 {
            result.abet = 1.0;
        } else {
            let initial_gap =
                effective_bandgap(c.vgb.midpoint(c.vge), t.nominal_kelvin, c.f1vg, c.f2vg);
            let operating_gap =
                effective_bandgap(c.vgb.midpoint(c.vge), t.effective_kelvin, c.f1vg, c.f2vg);
            validate_positive("tunneling_bandgap", initial_gap)?;
            validate_positive("tunneling_bandgap", operating_gap)?;
            let bandgap_ratio = initial_gap / operating_gap;
            let (original, effective, slopes) = if a.tunode == 1 {
                (
                    self.transport.junction.peripheral_emitter,
                    junction.peripheral_emitter,
                    junction_slopes.peripheral_emitter,
                )
            } else {
                (
                    self.transport.junction.intrinsic_emitter,
                    junction.intrinsic_emitter,
                    junction_slopes.intrinsic_emitter,
                )
            };
            let (mut saturation_factor, mut attenuation_factor) = (1.0, 1.0);
            if original.zero_bias_capacitance > 0.0 && original.potential > 0.0 {
                let cap_ratio = effective.zero_bias_capacitance / original.zero_bias_capacitance;
                let potential_ratio = effective.potential / original.potential;
                saturation_factor =
                    cap_ratio * bandgap_ratio.sqrt() * potential_ratio * potential_ratio;
                attenuation_factor = (1.0 / potential_ratio) / cap_ratio * bandgap_ratio.powf(-1.5);
                let gap_slope = c.f1vg.mul_add(t.effective_kelvin.ln() + 1.0, c.f2vg);
                let gap_fraction = gap_slope / operating_gap;
                let cap_fraction = slopes.zero_bias_capacitance / effective.zero_bias_capacitance;
                let potential_fraction = slopes.potential / effective.potential;
                tunneling_slopes.saturation = 0.5f64.mul_add(
                    -gap_fraction,
                    2.0f64.mul_add(potential_fraction, cap_fraction),
                );
                tunneling_slopes.attenuation =
                    1.5f64.mul_add(gap_fraction, -cap_fraction - potential_fraction);
            }
            result.ibets = a.ibets * saturation_factor;
            result.abet = a.abet * attenuation_factor;
        }
        Ok((result, tunneling_slopes))
    }
}

fn effective_bandgap(bandgap: f64, kelvin: f64, f1vg: f64, f2vg: f64) -> f64 {
    (f1vg * kelvin).mul_add(kelvin.ln(), f2vg.mul_add(kelvin, bandgap))
}
