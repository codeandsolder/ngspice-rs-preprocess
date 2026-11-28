// BSIM3v3.3.0 numerical extraction from codeandsolder/ngspice-rs-fast
// revision 439ea0903380e7f5e2a651653f595150bad61e9e. See package LICENSE.
//! Model-only default processing from `b3set.c::BSIM3setup`, before instance
//! geometry, state/node allocation, temperature scaling or effective-size
//! `b3check.c` diagnostics. Keep this phase distinct from raw explicit inputs.
use super::{Result, TemperatureModel, defaults_data};
use crate::{
    BoundParameters, ModelError, ModelVariant, MosPolarity, ParameterScope, ParameterValue,
};

const EPS_OXIDE: f64 = 3.453_133e-11;
// b3set.c uses this rounded constant, not the platform's full-precision pi.
// Preserve the exact binary64 value of b3set.c's deliberately rounded PI.
const SETUP_PI: f64 = f64::from_bits(0x4009_21fb_5452_4550);

/// One actual C model value and its given bit. Numeric zero initialization
/// is the `GENmodel` allocation rule, not a fallback for a missing parameter.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct ModelField<T> {
    pub(super) value: T,
    pub(super) given: bool,
}

impl<T> ModelField<T> {
    fn apply_default(&mut self, value: T) {
        if !self.given {
            self.value = value;
        }
    }
}

// Numeric names and types come from the generated schema. Every canonical
// numeric parameter is present by construction; aliases are resolved only by
// the public keyword boundary. Integer selectors never inhabit f64 storage.
macro_rules! define_model_values {
    (real { $(($real_name:literal, $real:ident),)* } integer { $(($integer_name:literal, $integer:ident),)* }) => {
        #[derive(Debug, Clone, PartialEq, Default)]
        pub(super) struct ModelValues {
            $(pub(super) $real: ModelField<f64>,)*
            $(pub(super) $integer: ModelField<i32>,)*
        }

        impl ModelValues {
            fn get(&self, canonical: &str) -> Option<f64> {
                match canonical {
                    $($real_name => Some(self.$real.value),)*
                    $($integer_name => Some(f64::from(self.$integer.value)),)*
                    _ => None,
                }
            }

            fn given(&self, canonical: &str) -> bool {
                match canonical {
                    $($real_name => self.$real.given,)*
                    $($integer_name => self.$integer.given,)*
                    _ => false,
                }
            }

            fn from_bound(source: &BoundParameters) -> Result<Self> {
                let mut values = Self::default();
                $(
                    if let Some(value) = bound(source, $real_name)? {
                        let ParameterValue::Real(raw) = value else {
                            return Err(ModelError::WrongModelScope.into());
                        };
                        values.$real = ModelField { value: native_real($real_name, *raw)?, given: true };
                    }
                )*
                $(
                    if let Some(value) = bound(source, $integer_name)? {
                        let ParameterValue::Integer(raw) = value else {
                            return Err(ModelError::WrongModelScope.into());
                        };
                        values.$integer = ModelField { value: *raw, given: true };
                    }
                )*
                Ok(values)
            }

            // Aliases share a catalog slot. Canonical field names are generated
            // from the existing typed native model, not an independent map.
            fn canonical(name: &str) -> Option<&'static str> {
                let slot = ModelVariant::Bsim3V33.parameter(ParameterScope::Model, name)?.slot;
                [ $($real_name,)* $($integer_name,)* "version", "nmos", "pmos" ]
                    .into_iter()
                    .find(|canonical| ModelVariant::Bsim3V33.parameter(ParameterScope::Model, canonical)
                        .is_some_and(|p| p.slot == slot))
            }

        }
    };
}

defaults_data::model_fields!(define_model_values);

fn bound<'a>(source: &'a BoundParameters, name: &str) -> Result<Option<&'a ParameterValue>> {
    let spec = source
        .variant()
        .parameter(source.scope(), name)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
    Ok(source.get_slot(spec.slot))
}

fn native_real(name: &str, raw: f64) -> Result<f64> {
    let threshold = match name {
        "nch" | "lnch" | "wnch" | "pnch" => 1e20,
        "ngate" => 1.000_001e24,
        "lngate" | "wngate" | "pngate" => 1e23,
        _ => f64::INFINITY,
    };
    if name == "tnom" {
        let kelvin = raw + 273.15;
        if kelvin.is_finite() && kelvin > 0. {
            Ok(kelvin)
        } else {
            Err(ModelError::InvalidNominalTemperature.into())
        }
    } else {
        Ok(if raw > threshold { raw * 1e-6 } else { raw })
    }
}

/// A model selector reset performed by `b3set.c`, with the given flag retained.
/// Device-layer code returns diagnostics rather than printing them.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelWarning {
    /// Canonical input keyword.
    pub parameter: &'static str,
    /// The value supplied to setup, after integer setter conversion.
    pub supplied: f64,
    /// The default selected by C's recovery rule.
    pub replacement: f64,
    /// Upstream behavior reference.
    pub c_reference: &'static str,
}

/// BSIM3 model defaults before geometry or temperature processing.
///
/// Model fields as the model-only part of `b3set.c` leaves them, not a
/// temperature- or geometry-processed transistor. Zero-initialized but ungiven
/// fields (such as K1/K2) remain distinguishable through [`Self::is_given`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelDefaults {
    pub(super) values: ModelValues,
    version_given: bool,
    /// Effective NMOS (+1) or PMOS (-1), after ordered type flags.
    pub polarity: i8,
    /// VERSION's last string setter, or C's setup default `3.3.0`.
    pub version: String,
    /// C's `BSIM3cox`, calculated from TOX (F/m²).
    pub oxide_capacitance: f64,
    /// C's NQS selector reset warnings, in setup order.
    pub warnings: Vec<ModelWarning>,
}

impl ModelDefaults {
    /// A numeric model field, including setup defaults and zero initialization.
    /// Flags and VERSION are exposed separately; aliases use the same C ID.
    #[must_use = "the returned option distinguishes a resolved numeric model field from an unknown or nonnumeric parameter"]
    pub fn get(&self, name: &str) -> Option<f64> {
        self.values.get(ModelValues::canonical(name)?)
    }

    /// C's given-bit state, not whether the final number differs from a
    /// default. Aliases (and the two type flags) share their actual C bit.
    #[must_use = "the result distinguishes explicitly supplied parameters from setup defaults even when their values coincide"]
    pub fn is_given(&self, name: &str) -> bool {
        ModelValues::canonical(name).is_some_and(|canonical| match canonical {
            "nmos" | "pmos" => true,
            "version" => self.version_given,
            field => self.values.given(field),
        })
    }

    /// Apply the model-level temperature pass, using absolute Kelvin.
    /// # Errors
    /// Invalid temperature or nonfinite derived quantity.
    pub fn at_temperature(&self, temperature_k: f64) -> Result<TemperatureModel> {
        TemperatureModel::new(self, temperature_k)
    }

    /// Prepare a selected v3.3 model from raw, typed model-card coefficients.
    /// The caller resolves the ordered NMOS/PMOS setters against the model
    /// family before this call; their result is the physical polarity.
    /// Explicit TNOM is in native deck Celsius, default nominal temperature
    /// is absolute Kelvin. The already selected variant determines equations,
    /// while the last VERSION setter is retained for parameter diagnostics.
    ///
    /// # Errors
    /// Wrong variant/scope, invalid nominal temperature, or inconsistent schema.
    pub fn prepare(
        source: &BoundParameters,
        polarity: MosPolarity,
        nominal_temperature_k: f64,
    ) -> Result<Self> {
        if source.variant() != ModelVariant::Bsim3V33 || source.scope() != ParameterScope::Model {
            return Err(ModelError::WrongModelScope.into());
        }
        if !nominal_temperature_k.is_finite() || nominal_temperature_k <= 0. {
            return Err(ModelError::InvalidNominalTemperature.into());
        }
        let version = bound(source, "version")?;
        let mut result = Self {
            values: ModelValues::from_bound(source)?,
            version_given: version.is_some(),
            polarity: match polarity {
                MosPolarity::Nmos => 1,
                MosPolarity::Pmos => -1,
            },
            version: match version {
                Some(ParameterValue::Text(value)) => value.clone(),
                None => "3.3.0".into(),
                _ => return Err(ModelError::WrongModelScope.into()),
            },
            oxide_capacitance: 0.,
            warnings: Vec::new(),
        };
        let values = &mut result.values;
        defaults_data::apply_literal_defaults!(values);
        for (name, field) in [
            ("nqsmod", &mut result.values.nqsmod),
            ("acnqsmod", &mut result.values.acnqsmod),
        ] {
            let selector = field.value;
            if !matches!(selector, 0 | 1) {
                field.value = 0;
                result.warnings.push(ModelWarning {
                    parameter: name,
                    supplied: f64::from(selector),
                    replacement: 0.,
                    c_reference: "src/spicelib/devices/bsim3/b3set.c (NQS selector reset)",
                });
            }
        }
        let nmos = result.polarity > 0;
        let values = &mut result.values;
        values.vth0.apply_default(if nmos { 0.7 } else { -0.7 });
        values.u0.apply_default(if nmos { 0.067 } else { 0.025 });
        values.noia.apply_default(if nmos { 1e20 } else { 9.9e18 });
        values.noib.apply_default(if nmos { 5e4 } else { 2.4e3 });
        values
            .noic
            .apply_default(if nmos { -1.4e-12 } else { 1.4e-12 });
        let mobility_three = values.mobmod.value == 3;
        values
            .uc
            .apply_default(if mobility_three { -0.0465 } else { -0.0465e-9 });
        values
            .uc1
            .apply_default(if mobility_three { -0.056 } else { -0.056e-9 });
        values.tnom.apply_default(nominal_temperature_k);

        values.toxm.apply_default(values.tox.value);
        values.dsub.apply_default(values.drout.value);
        values.llc.apply_default(values.ll.value);
        values.lwc.apply_default(values.lw.value);
        values.lwlc.apply_default(values.lwl.value);
        values.wlc.apply_default(values.wl.value);
        values.wwc.apply_default(values.ww.value);
        values.wwlc.apply_default(values.wwl.value);
        values.dwc.apply_default(values.wint.value);
        values.dlc.apply_default(values.lint.value);
        values.cjswg.apply_default(values.cjsw.value);
        values.pbswg.apply_default(values.pbsw.value);
        values.mjswg.apply_default(values.mjsw.value);
        result.derive_capacitances();
        Ok(result)
    }

    // Retain the C rounding order in fringe and overlap calculations.
    fn derive_capacitances(&mut self) {
        // A nonpositive TOX/TOXM or negative IJTH is reported by
        // `b3check.c` in the instance phase, with C's own message.
        let tox = self.values.tox.value;
        self.oxide_capacitance = EPS_OXIDE / tox;
        if !self.values.cf.given {
            let fringe_ratio = 0.4e-6 / tox;
            let fringe_argument = 1. + fringe_ratio;
            let fringe = 2. * EPS_OXIDE / SETUP_PI * fringe_argument.ln();
            self.values.cf.value = fringe;
        }
        let overlap_length = self.values.dlc.value;
        let cox = self.oxide_capacitance;
        for (overlap, fringe) in [
            (&mut self.values.cgdo, self.values.cgdl.value),
            (&mut self.values.cgso, self.values.cgsl.value),
        ] {
            if !overlap.given {
                let value = if self.values.dlc.given && overlap_length > 0. {
                    let overlap_capacitance = overlap_length * cox;
                    overlap_capacitance - fringe
                } else {
                    0.6 * self.values.xj.value * cox
                };
                overlap.value = value;
            }
        }
        if !self.values.cgbo.given {
            self.values.cgbo.value = 2. * self.values.dwc.value * cox;
        }
    }
}
