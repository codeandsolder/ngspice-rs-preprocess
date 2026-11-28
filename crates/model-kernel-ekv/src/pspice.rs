//! Typed source-parameter binding for the documented EKV 2.6 dialect.
//!
//! The enclosing parser resolves expressions and source scoping. This module
//! owns physical defaults, process derivations, instance multiplicity,
//! parasitic geometry and operating temperature. Native ngspice LEVEL=5
//! (BSIM2) must never be routed here.

use std::{collections::BTreeMap, sync::Arc};

use model_kernel_api::{Artifact, Error, Result};

use crate::{
    Geometry, Model, Polarity,
    artifact::Noise,
    device::{Device, DeviceKernel, ResistanceTemperature},
    geometry::{Defaults, Diffusions, Overrides},
    junction::{Depletion, Junction},
    parameters::{EPS_SI, K_OVER_Q, bandgap},
};

pub use crate::device::OperatingTemperature;

/// Model family tags preserve the source-selected physics and polarity.
pub const NMOS_FAMILY: &str = "pspice_ekv26_nmos";
pub const PMOS_FAMILY: &str = "pspice_ekv26_pmos";

/// Context supplied by the already resolved circuit options.
#[derive(Clone, Copy, Debug)]
pub struct CircuitDefaults {
    pub length: f64,
    pub width: f64,
    pub drain_area: f64,
    pub source_area: f64,
    /// Absolute kelvin.
    pub nominal_temperature: f64,
}

impl Default for CircuitDefaults {
    fn default() -> Self {
        Self {
            length: 100e-6,
            width: 100e-6,
            drain_area: 0.,
            source_area: 0.,
            nominal_temperature: 300.15,
        }
    }
}

/// Geometry, parasitics and model data resolved for one occurrence.
#[derive(Clone, Copy, Debug)]
pub struct BoundDevice {
    pub device: Device,
    pub geometry: Geometry,
    pub diffusions: Diffusions,
    /// Saturation flag threshold; does not change physical equations.
    pub saturation_limit: f64,
}

impl BoundDevice {
    /// Construct an immutable full device artifact from the bound data.
    ///
    /// # Errors
    /// Invalid physical parameters or identity.
    pub fn artifact(self, fingerprint: [u8; 16]) -> Result<Arc<Artifact<DeviceKernel>>> {
        self.device.artifact(self.geometry, fingerprint)
    }

    /// Return the model's compact geometry slots in descriptor order.
    #[must_use]
    pub const fn slots(self) -> [f64; 4] {
        [
            self.geometry.length,
            self.geometry.width,
            self.geometry.parallel,
            self.geometry.series,
        ]
    }

    /// Classify the intrinsic operating point using the documented if/ir
    /// ratio, independent of the current, charge and noise equations.
    ///
    /// # Errors
    /// Invalid bias or operating temperature.
    pub fn saturated(self, intrinsic_voltage: [f64; 4], ambient: f64) -> Result<bool> {
        let channel = self
            .device
            .channel
            .prepare_segmented(self.geometry, self.device.temperature.resolve(ambient)?)?;
        let point = channel.evaluate(intrinsic_voltage)?;
        Ok(point.forward_normalized / point.reverse_normalized > self.saturation_limit)
    }
}

/// Resolved model parameters (before occurrence geometry and temperature).
#[derive(Clone, Copy, Debug)]
pub struct PspiceModel {
    channel: Model,
    nominal_junction: Junction,
    saturation_density: [f64; 2],
    area_capacitance: f64,
    absolute_capacitance: [f64; 2],
    perimeter_capacitance: f64,
    overlap: [f64; 3],
    resistance: [Option<f64>; 4],
    contact: [f64; 4],
    sheet: [f64; 4],
    resistance_temperature: [ResistanceTemperature; 4],
    shunt: f64,
    noise: Noise,
    geometry: Defaults,
    dimensions: [f64; 2],
    temperature: OperatingTemperature,
    saturation_limit: f64,
}

#[derive(Default)]
struct Fields(BTreeMap<String, f64>);

impl Fields {
    fn new<'a>(fields: impl IntoIterator<Item = (&'a str, f64)>, instance: bool) -> Result<Self> {
        let mut result = BTreeMap::new();
        for (name, value) in fields {
            if !value.is_finite() {
                return Err(Error::Domain(format!("nonfinite EKV parameter {name}")));
            }
            let name = name.to_ascii_lowercase();
            // The occurrence N is series multiplicity; model N is the
            // junction emission coefficient. AKO and ordinary repeated
            // declarations have last-assignment semantics.
            let name = match (name.as_str(), instance) {
                ("np", true) => "m",
                ("ns", true) => "n",
                ("t_abs", _) => "temp",
                ("t_rel_global", _) => "dtemp",
                ("t_measured", false) => "tnom",
                _ => &name,
            };
            result.insert(name.to_owned(), value);
        }
        Ok(Self(result))
    }

    fn take(&mut self, key: &str) -> Option<f64> {
        self.0.remove(key)
    }

    fn defaulted(&mut self, key: &str, default: f64) -> f64 {
        self.take(key).unwrap_or(default)
    }

    fn nonnegative(&mut self, key: &str, default: f64) -> Result<f64> {
        let value = self.defaulted(key, default);
        if value < 0. {
            return Err(Error::Domain(format!("negative EKV parameter {key}")));
        }
        Ok(value)
    }

    fn temperature(&mut self) -> Result<Option<OperatingTemperature>> {
        match (self.take("temp"), self.take("dtemp")) {
            (Some(value), None) => {
                let kelvin = value + 273.15;
                if kelvin <= 0. || !kelvin.is_finite() {
                    return Err(Error::Domain("invalid EKV absolute temperature".into()));
                }
                Ok(Some(OperatingTemperature::Absolute(kelvin)))
            }
            (None, Some(value)) => Ok(Some(OperatingTemperature::Offset(value))),
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Err(Error::Domain(
                "conflicting EKV operating temperatures".into(),
            )),
        }
    }

    fn finish(self) -> Result<()> {
        if let Some((key, _)) = self.0.into_iter().next() {
            return Err(Error::Domain(format!("unsupported EKV parameter {key}")));
        }
        Ok(())
    }
}

/// The intrinsic process dependencies use the documented cm-based doping
/// and mobility units, SI oxide/velocity inputs, and the same thermal-voltage
/// convention as the constitutive law.
fn intrinsic(polarity: Polarity, fields: &mut Fields, nominal: f64) -> Result<Model> {
    let sign = polarity.sign();
    let tox = fields.nonnegative("tox", 0.)?;
    let nsub = fields.nonnegative("nsub", 0.)? * 1e6;
    let mobility = fields.nonnegative("uo", 0.)? * 1e-4;
    let vmax = fields.nonnegative("vmax", 0.)?;
    let flatband = fields.take("vfb");
    let cox = fields.take("cox").unwrap_or({
        if tox > 0. {
            (3.9 / 11.7) * EPS_SI / tox
        } else {
            0.7e-3
        }
    });
    let gamma = fields.take("gamma").unwrap_or_else(|| {
        if nsub > 0. {
            (2. * 1.602_176_634e-19 * EPS_SI * nsub).sqrt() / cox
        } else {
            1.
        }
    });
    let phi = fields.take("phi").unwrap_or_else(|| {
        if nsub > 0. {
            let vt = K_OVER_Q * nominal;
            let reference = 300.15;
            let log_ni = 1.5_f64.mul_add((nominal / reference).ln(), 1.45e16_f64.ln())
                + bandgap(reference) / (2. * K_OVER_Q * reference)
                - bandgap(nominal) / (2. * vt);
            2. * vt * (nsub.ln() - log_ni)
        } else {
            0.7
        }
    });
    let threshold = fields.take("vto").unwrap_or_else(|| {
        flatband.map_or(0.5 * sign, |vfb| {
            sign.mul_add(gamma.mul_add(phi.sqrt(), phi), vfb)
        })
    });
    let kp = fields
        .take("kp")
        .unwrap_or(if mobility > 0. { mobility * cox } else { 50e-6 });
    let critical_field = fields
        .take("ucrit")
        .unwrap_or(if vmax > 0. && mobility > 0. {
            vmax / mobility
        } else {
            2e6
        });
    let theta = fields.take("theta");
    let mobility_field = fields
        .take("e0")
        .unwrap_or_else(|| if theta.is_some() { 0. } else { 1e12 });
    // The source requires DEV tolerance for statistical matching. A bare
    // change of those parameters is not a deterministic model input.
    for name in ["avto", "akp", "agamma"] {
        if fields
            .take(name)
            .is_some_and(|x| x.to_bits() != 1e-6_f64.to_bits())
        {
            return Err(Error::Unsupported(
                "EKV matching coefficient requires statistical realization",
            ));
        }
    }
    Ok(Model {
        polarity,
        cox,
        gamma,
        phi,
        threshold,
        kp,
        critical_field,
        nominal_temperature: nominal,
        junction_depth: fields.defaulted("xj", 0.1e-6),
        dl: fields.defaulted("dl", 0.),
        dw: fields.defaulted("dw", 0.),
        threshold_temperature: fields.defaulted("tcv", 1e-3 * sign),
        mobility_exponent: fields.defaulted("bex", -1.5),
        critical_field_exponent: fields.defaulted("ucex", 0.8),
        mobility_field,
        theta: theta.unwrap_or(0.),
        lambda: fields.defaulted("lambda", 0.5),
        narrow: fields.defaulted("weta", 0.25),
        short: fields.defaulted("leta", 0.1),
        reverse_short_charge: fields.defaulted("q0", 0.),
        reverse_short_length: fields.defaulted("lk", 0.29e-6),
        impact_a: fields.defaulted("iba", 0.),
        impact_b: fields.defaulted("ibb", 3e8),
        impact_temperature: fields.defaulted("ibbt", 9e-4),
        impact_saturation: fields.defaulted("ibn", 1.),
        mismatch_threshold: 1e-6,
        mismatch_gain: 1e-6,
        mismatch_gamma: 1e-6,
    })
}

fn selectors(fields: &mut Fields) -> Result<f64> {
    if fields
        .take("level")
        .is_some_and(|v| v.to_bits() != 5.0_f64.to_bits())
    {
        return Err(Error::Unsupported("EKV requires PSpice level 5"));
    }
    if fields.take("nqs").is_some_and(|v| v != 0.) {
        return Err(Error::Unsupported("EKV nonquasistatic operation"));
    }
    if fields
        .take("xqc")
        .is_some_and(|v| v.to_bits() != 0.4_f64.to_bits())
    {
        return Err(Error::Unsupported("EKV capacitance-only formulation"));
    }
    // PSpice 16.6 explicitly documents that NLEV is unused in EKV. The
    // generic GDSNOI parameter belongs to that unused generic noise model.
    if fields
        .take("nlev")
        .is_some_and(|v| !(0. ..=3.).contains(&v) || v.fract() != 0.)
    {
        return Err(Error::Domain("invalid EKV NLEV selector".into()));
    }
    let _ = fields.nonnegative("gdsnoi", 1.)?;
    let saturation = fields.defaulted("satlim", 54.6);
    if saturation <= 0. {
        return Err(Error::Domain("nonpositive EKV SATLIM".into()));
    }
    Ok(saturation)
}

fn nominal_junction(fields: &mut Fields) -> Result<Junction> {
    let bottom = Depletion {
        capacitance: 0.,
        potential: fields.defaulted("pb", 0.8),
        grading: fields.defaulted("mj", 0.5),
    };
    let sidewall = Depletion {
        capacitance: 0.,
        potential: fields.defaulted("pbsw", bottom.potential),
        grading: fields.defaulted("mjsw", 0.33),
    };
    Ok(Junction {
        saturation_current: fields.nonnegative("is", 1e-14)?,
        emission: fields.defaulted("n", 1.),
        temperature_exponent: fields.defaulted("xti", 0.),
        bottom,
        sidewall,
        forward_coefficient: fields.defaulted("fc", 0.5),
        transit_time: fields.nonnegative("tt", 0.)?,
    })
}

impl PspiceModel {
    /// Compile the model card after scalar expressions have been resolved.
    ///
    /// # Errors
    /// Unknown fields, unsupported selectors or invalid physical defaults.
    pub fn compile<'a>(
        polarity: Polarity,
        parameters: impl IntoIterator<Item = (&'a str, f64)>,
        circuit: CircuitDefaults,
    ) -> Result<Self> {
        let mut fields = Fields::new(parameters, false)?;
        let saturation_limit = selectors(&mut fields)?;
        let nominal = fields
            .take("tnom")
            .map_or(circuit.nominal_temperature, |v| v + 273.15);
        if !nominal.is_finite() || nominal <= 0. {
            return Err(Error::Domain("invalid EKV nominal temperature".into()));
        }
        let channel = intrinsic(polarity, &mut fields, nominal)?;
        let dimensions = [
            fields.defaulted("l", circuit.length),
            fields.defaulted("w", circuit.width),
        ];
        let temperature = fields.temperature()?.unwrap_or_default();
        let nominal_junction = nominal_junction(&mut fields)?;
        let drain_source_sheet = fields.nonnegative("rsh", 0.)?;
        let gate_sheet = fields.take("rgsh");
        let sheet = [
            drain_source_sheet,
            gate_sheet.unwrap_or(0.),
            drain_source_sheet,
            fields.nonnegative("rbsh", 0.)?,
        ];
        let drain_source_tc = ResistanceTemperature {
            linear: fields.defaulted("tr1", 0.),
            quadratic: fields.defaulted("tr2", 0.),
        };
        let resistance_temperature = [
            drain_source_tc,
            ResistanceTemperature {
                linear: fields.defaulted("trg", 0.),
                quadratic: 0.,
            },
            drain_source_tc,
            ResistanceTemperature {
                linear: fields.defaulted("trb", 0.),
                quadratic: 0.,
            },
        ];
        let shunt = match fields.take("rds") {
            Some(r) if r > 0. => r.recip(),
            Some(_) => {
                return Err(Error::Domain(
                    "nonpositive EKV drain-source shunt resistance".into(),
                ));
            }
            None => 0.,
        };
        let model = Self {
            channel,
            nominal_junction,
            temperature,
            dimensions,
            sheet,
            resistance_temperature,
            shunt,
            saturation_limit,
            saturation_density: [
                fields.nonnegative("js", 0.)?,
                fields.nonnegative("jssw", 0.)?,
            ],
            area_capacitance: fields.nonnegative("cj", 0.)?,
            absolute_capacitance: [
                fields.nonnegative("cbd", 0.)?,
                fields.nonnegative("cbs", 0.)?,
            ],
            perimeter_capacitance: fields.nonnegative("cjsw", 0.)?,
            overlap: [
                fields.nonnegative("cgdo", 0.)?,
                fields.nonnegative("cgso", 0.)?,
                fields.nonnegative("cgbo", 0.)?,
            ],
            resistance: ["rd", "rg", "rs", "rb"].map(|name| fields.take(name)),
            contact: [
                fields.nonnegative("rdc", 0.)?,
                fields.nonnegative("rgc", 0.)?,
                fields.nonnegative("rsc", 0.)?,
                fields.nonnegative("rbc", 0.)?,
            ],
            noise: Noise {
                flicker_coefficient: fields.nonnegative("kf", 0.)?,
                flicker_frequency_exponent: fields.defaulted("af", 1.),
            },
            geometry: Defaults {
                areas: [circuit.drain_area, circuit.source_area],
                hdif: fields.take("hdif"),
                gate_sheet_specified: gate_sheet.is_some(),
            },
        };
        fields.finish()?;
        model.validate()?;
        Ok(model)
    }

    fn validate(self) -> Result<()> {
        if self.resistance.iter().flatten().any(|r| *r < 0.)
            || self.sheet.iter().any(|r| !r.is_finite() || *r < 0.)
            || !self.shunt.is_finite()
            || self.noise.flicker_frequency_exponent <= 0.
        {
            return Err(Error::Domain("invalid EKV model parasitics".into()));
        }
        // Validate the default geometry only when a caller actually binds
        // it; a model card may rely on explicit L/W on every occurrence.
        self.nominal_junction.prepare(
            self.channel.nominal_temperature,
            self.channel.nominal_temperature,
            K_OVER_Q * self.channel.nominal_temperature,
            1.,
        )?;
        Ok(())
    }

    fn junctions(self, diffusions: Diffusions) -> [Junction; 2] {
        let use_density =
            self.saturation_density[0] > 0. && diffusions.areas.iter().all(|a| *a > 0.);
        let use_absolute = self.absolute_capacitance.iter().any(|c| *c > 0.);
        std::array::from_fn(|i| {
            let mut j = self.nominal_junction;
            if use_density {
                j.saturation_current = self.saturation_density[0].mul_add(
                    diffusions.areas[i],
                    self.saturation_density[1] * diffusions.perimeters[i],
                );
            }
            j.bottom.capacitance = if use_absolute {
                self.absolute_capacitance[i]
            } else {
                self.area_capacitance * diffusions.areas[i]
            };
            j.sidewall.capacitance = self.perimeter_capacitance * diffusions.perimeters[i];
            j
        })
    }

    /// Bind one occurrence. All geometry-derived quantities are recomputed
    /// from explicit values or documented defaults; no parameter is looked
    /// up during Newton evaluation.
    ///
    /// # Errors
    /// Invalid instance parameters, physical geometry or resistance.
    pub fn bind<'a>(
        self,
        parameters: impl IntoIterator<Item = (&'a str, f64)>,
    ) -> Result<BoundDevice> {
        let mut fields = Fields::new(parameters, true)?;
        let geometry = Geometry {
            length: fields.defaulted("l", self.dimensions[0]),
            width: fields.defaulted("w", self.dimensions[1]),
            parallel: fields.defaulted("m", 1.),
            series: fields.defaulted("n", 1.),
        };
        let diffusions = self.geometry.resolve(
            geometry,
            Overrides {
                drain_area: fields.take("ad"),
                source_area: fields.take("as"),
                drain_perimeter: fields.take("pd"),
                source_perimeter: fields.take("ps"),
                squares: ["nrd", "nrg", "nrs", "nrb"].map(|name| fields.take(name)),
            },
        )?;
        let temperature = fields.temperature()?.unwrap_or(self.temperature);
        fields.finish()?;
        self.channel
            .prepare_segmented(geometry, self.channel.nominal_temperature)?;
        let resistance = std::array::from_fn(|i| {
            self.resistance[i]
                .unwrap_or_else(|| self.sheet[i].mul_add(diffusions.squares[i], self.contact[i]))
        });
        let device = Device {
            channel: self.channel,
            temperature,
            junctions: self.junctions(diffusions),
            overlap: self.overlap,
            resistance,
            resistance_temperature: self.resistance_temperature,
            shunt: self.shunt,
            noise: Some(self.noise),
        };
        if resistance.iter().any(|r| !r.is_finite() || *r < 0.) {
            return Err(Error::Domain("invalid EKV resolved resistance".into()));
        }
        Ok(BoundDevice {
            device,
            geometry,
            diffusions,
            saturation_limit: self.saturation_limit,
        })
    }
}
