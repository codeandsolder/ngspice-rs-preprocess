//! MOS junction geometry, resistance preparation and constitutive primitives.
//!
//! The input lengths and explicit areas already include any source-level
//! unit/global scaling. Lithographic width scaling is applied here only
//! where the chosen physical geometry rule requires it. Outputs describe
//! one device, before parallel instance multiplicity. See HSPICE MOSFET
//! Models Manual X-2005.09, pp. 44–54 and 403–404, for the distinct
//! explicit, width-normalized and folded diffusion geometries.
//!
//! The constitutive primitives operate on prepared coefficients. They do not
//! select a complete model revision or resolve source-specific defaults.

use crate::ModelError;

/// Geometry law for the bottom and sidewall junction measures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MosDiffusionRule {
    /// Explicit areas/perimeters are already physical; missing values are zero.
    ExplicitPhysical,
    /// Explicit areas/perimeters additionally scale with lithographic width.
    ExplicitLithographic,
    /// The bottom density is per unit width, not per square metre.
    WidthNormalized,
    /// Omitted geometry is calculated for an isolated two-sided contact.
    Isolated,
    /// The terminal may share a contact; the perimeter excludes the gate edge.
    Stacked,
}

/// Per-terminal instance inputs. `None` and `Some(0.)` are distinct.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MosDiffusionTerminal {
    pub area_m2: Option<f64>,
    pub perimeter_m: Option<f64>,
    pub shared: bool,
}

/// Per-device physical dimensions, before the model's width multiplier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDiffusionDimensions {
    pub width_m: f64,
    pub width_multiplier: f64,
    pub width_offset_m: f64,
    pub heavy_diffusion_m: f64,
}

/// Bottom junction density must use the corresponding dimensional unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MosBottomMeasure {
    AreaM2(f64),
    WidthM(f64),
}

/// Prepared geometry for one drain or source. No multiplicity is folded in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDiffusionGeometry {
    pub bottom: MosBottomMeasure,
    pub perimeter_m: f64,
}

/// The relationship between the sidewall and gate-edge capacitance measures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MosSidewallRule {
    /// All perimeter contributes to the ordinary sidewall.
    EntirePerimeter,
    /// The gate edge occupies up to the supplied electrical gate width.
    SplitPerimeter,
    /// The perimeter excludes the gate edge; the full gate edge is additional.
    AdditionalGateEdge,
}

/// Separate length weights for independent sidewall and gate-edge densities.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosSidewallLengths {
    pub sidewall_m: f64,
    pub gate_edge_m: f64,
}

fn nonnegative(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value >= 0. {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

fn positive(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() && value > 0. {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

impl MosDiffusionDimensions {
    /// Effective diffusion width, distinct from the intrinsic channel width.
    ///
    /// # Errors
    /// Invalid dimensions or nonfinite/nonpositive resulting width.
    pub fn diffusion_width(self) -> Result<f64, ModelError> {
        positive(self.width_m, "drawn MOS diffusion width")?;
        positive(self.width_multiplier, "MOS diffusion width multiplier")?;
        positive(
            self.width_m
                .mul_add(self.width_multiplier, self.width_offset_m),
            "effective MOS diffusion width",
        )
    }

    fn heavy_length(self) -> Result<f64, ModelError> {
        nonnegative(self.heavy_diffusion_m, "heavy MOS diffusion length")?;
        nonnegative(
            self.heavy_diffusion_m * self.width_multiplier,
            "effective heavy MOS diffusion length",
        )
    }
}

/// Prepare one junction's geometry, preserving explicitly zero dimensions.
///
/// `Isolated` is also the geometry calculation used by BSIM3 Berkeley
/// junctions when the source explicitly selects automatic area calculation.
/// Its use does not select a particular current or charge law.
///
/// # Errors
/// Invalid geometry, multipliers or nonrepresentable results.
pub fn prepare_mos_diffusion(
    rule: MosDiffusionRule,
    dimensions: MosDiffusionDimensions,
    terminal: MosDiffusionTerminal,
) -> Result<MosDiffusionGeometry, ModelError> {
    if rule == MosDiffusionRule::WidthNormalized {
        let width = dimensions.diffusion_width()?;
        return Ok(MosDiffusionGeometry {
            bottom: MosBottomMeasure::WidthM(nonnegative(
                width * dimensions.width_multiplier,
                "MOS bottom width",
            )?),
            perimeter_m: width,
        });
    }
    let area = terminal
        .area_m2
        .map(|area| nonnegative(area, "MOS junction area"))
        .transpose()?;
    let perimeter = terminal
        .perimeter_m
        .map(|length| nonnegative(length, "MOS junction perimeter"))
        .transpose()?;
    if rule == MosDiffusionRule::ExplicitPhysical {
        return Ok(MosDiffusionGeometry {
            bottom: MosBottomMeasure::AreaM2(area.unwrap_or_default()),
            perimeter_m: perimeter.unwrap_or_default(),
        });
    }
    let multiplier = positive(
        dimensions.width_multiplier,
        "MOS diffusion width multiplier",
    )?;
    let (default_area, default_perimeter) = match rule {
        MosDiffusionRule::ExplicitLithographic => (0., 0.),
        MosDiffusionRule::Isolated | MosDiffusionRule::Stacked => {
            let width = dimensions.diffusion_width()?;
            let heavy = dimensions.heavy_length()?;
            match (rule, terminal.shared) {
                (MosDiffusionRule::Stacked, true) => (heavy * width, 2. * heavy),
                (MosDiffusionRule::Stacked, false) => {
                    (2. * heavy * width, 4.0f64.mul_add(heavy, width))
                }
                _ => (2. * heavy * width, 2.0f64.mul_add(width, 4. * heavy)),
            }
        }
        _ => {
            return Err(ModelError::InvalidPhysicalParameter(
                "MOS diffusion geometry rule",
            ));
        }
    };
    let area = area.map_or(default_area, |value| value * multiplier * multiplier);
    let perimeter = perimeter.map_or(default_perimeter, |value| value * multiplier);
    Ok(MosDiffusionGeometry {
        bottom: MosBottomMeasure::AreaM2(nonnegative(area, "effective MOS junction area")?),
        perimeter_m: nonnegative(perimeter, "effective MOS junction perimeter")?,
    })
}

impl MosDiffusionGeometry {
    /// Resolve separate sidewall lengths using the gate width appropriate to
    /// the selected charge law (which can differ from the diffusion width).
    ///
    /// # Errors
    /// Invalid perimeter or gate width.
    pub fn sidewall_lengths(
        self,
        rule: MosSidewallRule,
        gate_width_m: f64,
    ) -> Result<MosSidewallLengths, ModelError> {
        let perimeter = nonnegative(self.perimeter_m, "MOS junction perimeter")?;
        let (sidewall, gate) = match rule {
            MosSidewallRule::EntirePerimeter => (perimeter, 0.),
            MosSidewallRule::SplitPerimeter => {
                let gate = nonnegative(gate_width_m, "MOS junction gate width")?.min(perimeter);
                (perimeter - gate, gate)
            }
            MosSidewallRule::AdditionalGateEdge => (
                perimeter,
                nonnegative(gate_width_m, "MOS junction gate width")?,
            ),
        };
        Ok(MosSidewallLengths {
            sidewall_m: sidewall,
            gate_edge_m: gate,
        })
    }
}

/// Series-resistance law, independent of the diode geometry/current law.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MosSeriesResistanceRule {
    /// Sheet squares, or the explicit terminal resistance if that term is zero.
    Explicit,
    /// Light diffusion plus the supplied/default number of squares.
    WidthDependent,
    /// Light diffusion plus explicit or automatically calculated heavy squares.
    AutomaticSquares,
    /// Direct resistance plus sheet squares (ASPEC with zero LDIF).
    DirectContact,
}

/// Resistance coefficients for one terminal, before parallel multiplicity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosSeriesResistance {
    pub terminal_resistance: f64,
    pub sheet_resistance_ohm_per_square: f64,
    pub contact_resistance_ohm: f64,
    pub squares: Option<f64>,
    pub default_squares: f64,
    pub light_diffusion_m: f64,
}

impl MosSeriesResistance {
    /// Calculate the physical single-device series resistance in ohms.
    /// Apply a separate temperature law and divide by the number of parallel
    /// instances only at the consuming topology/preparation boundary.
    ///
    /// # Errors
    /// Invalid coefficients or nonrepresentable/nonphysical resistance.
    pub fn prepare(
        self,
        rule: MosSeriesResistanceRule,
        dimensions: MosDiffusionDimensions,
    ) -> Result<f64, ModelError> {
        let terminal = nonnegative(self.terminal_resistance, "MOS terminal resistance")?;
        let sheet = nonnegative(self.sheet_resistance_ohm_per_square, "MOS sheet resistance")?;
        let contact = nonnegative(self.contact_resistance_ohm, "MOS contact resistance")?;
        let explicit = self
            .squares
            .map(|value| nonnegative(value, "MOS terminal squares"))
            .transpose()?;
        let squares = || {
            nonnegative(
                explicit.unwrap_or(self.default_squares),
                "MOS terminal squares",
            )
        };
        let resistance = match rule {
            MosSeriesResistanceRule::Explicit => {
                let sheet_term = squares()? * sheet;
                if sheet_term > 0. {
                    sheet_term
                } else {
                    terminal
                }
            }
            MosSeriesResistanceRule::DirectContact => squares()?.mul_add(sheet, terminal),
            MosSeriesResistanceRule::WidthDependent | MosSeriesResistanceRule::AutomaticSquares => {
                let width = dimensions.diffusion_width()?;
                let light = nonnegative(self.light_diffusion_m, "light MOS diffusion length")?;
                let squares = match (rule, explicit) {
                    (MosSeriesResistanceRule::AutomaticSquares, None) => {
                        dimensions.heavy_length()? / width
                    }
                    _ => squares()?,
                };
                squares.mul_add(sheet, light / width * terminal)
            }
        };
        nonnegative(resistance + contact, "MOS series resistance")
    }
}

/// One zero-bias capacitance contribution with a power-law depletion region
/// below zero bias and its tangent continuation above zero bias.
///
/// Geometry and temperature correction are applied before construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDepletionComponent {
    capacitance_f: f64,
    potential_v: f64,
    grading: f64,
}

/// Stored depletion charge and its voltage derivative, relative to zero bias.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDepletionPoint {
    pub charge_c: f64,
    pub capacitance_f: f64,
}

const fn finite(value: f64, name: &'static str) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(name))
    }
}

impl MosDepletionComponent {
    /// Prepare a single bottom, sidewall, or gate-edge depletion contribution.
    ///
    /// # Errors
    /// Nonphysical capacitance, potential, or grading.
    pub fn new(capacitance_f: f64, potential_v: f64, grading: f64) -> Result<Self, ModelError> {
        Ok(Self {
            capacitance_f: nonnegative(capacitance_f, "MOS depletion capacitance")?,
            potential_v: positive(potential_v, "MOS depletion potential")?,
            grading: nonnegative(grading, "MOS depletion grading")?,
        })
    }

    /// Integrate the depletion C-V law analytically. The logarithmic grading=1
    /// limit is continuous, including for nearly unity grading and tiny bias.
    ///
    /// # Errors
    /// Nonfinite voltage or nonrepresentable charge/capacitance.
    pub fn evaluate(self, voltage_v: f64) -> Result<MosDepletionPoint, ModelError> {
        finite(voltage_v, "MOS depletion voltage")?;
        let c0 = self.capacitance_f;
        if c0 == 0. {
            return Ok(MosDepletionPoint {
                charge_c: 0.,
                capacitance_f: 0.,
            });
        }
        let (charge, capacitance) = if voltage_v < 0. {
            let log = (-voltage_v / self.potential_v).ln_1p();
            let exponent = 1. - self.grading;
            let scaled = exponent * log;
            let integral = if scaled == 0. {
                log
            } else {
                scaled.exp_m1() / exponent
            };
            (
                -c0 * self.potential_v * integral,
                c0 * (-self.grading * log).exp(),
            )
        } else {
            let delta = self.grading * (voltage_v / self.potential_v);
            (
                c0 * voltage_v * 0.5f64.mul_add(delta, 1.),
                c0 * (1. + delta),
            )
        };
        Ok(MosDepletionPoint {
            charge_c: finite(charge, "MOS depletion charge")?,
            capacitance_f: nonnegative(capacitance, "MOS depletion capacitance")?,
        })
    }
}

/// A junction with exponential forward conduction and two linear reverse
/// branches.
///
/// Unlike the Berkeley reverse diode, the reverse conductance is
/// numerically equal to the prepared saturation current, in siemens.
/// The consuming circuit supplies any artificial parallel conductance.
/// See HSPICE MOSFET Models Manual X-2005.09, pp. 55–58.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosLinearReverseJunction {
    saturation_current_a: f64,
    emission_thermal_voltage_v: f64,
    reverse_transition_v: f64,
    deep_reverse_conductance_s: f64,
    transit_time_s: f64,
}

/// Junction current and total stored charge, with their voltage derivatives.
/// The voltage is forward-positive and all results are per device.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosJunctionPoint {
    pub current_a: f64,
    pub conductance_s: f64,
    pub charge_c: f64,
    pub capacitance_f: f64,
}

impl MosLinearReverseJunction {
    /// Prepare the physical junction parameters after geometry/temperature.
    /// `emission_thermal_voltage_v` is the product of emission coefficient
    /// and thermal voltage; `reverse_slope_divisor` is the deep reverse divisor.
    ///
    /// # Errors
    /// Nonphysical coefficients or unrepresentable deep-reverse conductance.
    pub fn new(
        saturation_current_a: f64,
        emission_thermal_voltage_v: f64,
        reverse_transition_v: f64,
        reverse_slope_divisor: f64,
        transit_time_s: f64,
    ) -> Result<Self, ModelError> {
        let saturation_current_a =
            nonnegative(saturation_current_a, "MOS junction saturation current")?;
        let reverse_transition_v =
            -nonnegative(-reverse_transition_v, "MOS junction reverse transition")?;
        let divisor = positive(reverse_slope_divisor, "MOS junction reverse slope divisor")?;
        Ok(Self {
            saturation_current_a,
            emission_thermal_voltage_v: positive(
                emission_thermal_voltage_v,
                "MOS junction emission thermal voltage",
            )?,
            reverse_transition_v,
            deep_reverse_conductance_s: nonnegative(
                saturation_current_a / divisor,
                "MOS junction deep reverse conductance",
            )?,
            transit_time_s: nonnegative(transit_time_s, "MOS junction transit time")?,
        })
    }

    fn conduction(self, voltage_v: f64) -> (f64, f64) {
        let saturation = self.saturation_current_a;
        if saturation == 0. {
            return (0., 0.);
        }
        if voltage_v >= 0. {
            let exponent = voltage_v / self.emission_thermal_voltage_v;
            let exponential = exponent.exp_m1();
            let current = if exponential.is_finite() {
                saturation * exponential
            } else {
                (saturation.ln() + exponent).exp() - saturation
            };
            (
                current,
                (current + saturation) / self.emission_thermal_voltage_v,
            )
        } else if voltage_v >= self.reverse_transition_v {
            (saturation * voltage_v, saturation)
        } else {
            (
                self.deep_reverse_conductance_s.mul_add(
                    voltage_v - self.reverse_transition_v,
                    saturation * self.reverse_transition_v,
                ),
                self.deep_reverse_conductance_s,
            )
        }
    }

    /// Evaluate conduction, diffusion storage and each prepared depletion
    /// component. Derivatives at the two branch boundaries are taken from the
    /// more-positive branch, while current and charge themselves are continuous.
    /// No solver limiting, artificial conductance or instance multiplication.
    ///
    /// # Errors
    /// Nonfinite bias or nonrepresentable physical output.
    pub fn evaluate(
        self,
        voltage_v: f64,
        depletion: &[MosDepletionComponent],
    ) -> Result<MosJunctionPoint, ModelError> {
        finite(voltage_v, "MOS junction voltage")?;
        let (current, conductance) = self.conduction(voltage_v);
        let mut charge = self.transit_time_s * current;
        let mut capacitance = self.transit_time_s * conductance;
        for component in depletion {
            let point = component.evaluate(voltage_v)?;
            charge += point.charge_c;
            capacitance += point.capacitance_f;
        }
        Ok(MosJunctionPoint {
            current_a: finite(current, "MOS junction current")?,
            conductance_s: nonnegative(conductance, "MOS junction conductance")?,
            charge_c: finite(charge, "MOS junction charge")?,
            capacitance_f: nonnegative(capacitance, "MOS junction capacitance")?,
        })
    }
}

/// Bottom junction density in the dimensional unit selected by the geometry.
/// The quantity may be saturation current or zero-bias capacitance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MosBottomDensity {
    PerSquareMetre(f64),
    PerMetre(f64),
}

impl MosBottomDensity {
    /// Resolve a current/capacitance density to a single-device quantity.
    ///
    /// # Errors
    /// Incompatible dimensional units, negative values, or overflow.
    pub fn weight(self, extent: MosBottomMeasure) -> Result<f64, ModelError> {
        let ((Self::PerSquareMetre(density), MosBottomMeasure::AreaM2(extent))
        | (Self::PerMetre(density), MosBottomMeasure::WidthM(extent))) = (self, extent)
        else {
            return Err(ModelError::InvalidPhysicalParameter(
                "MOS bottom density units",
            ));
        };
        let density = nonnegative(density, "MOS bottom density")?;
        let extent = nonnegative(extent, "MOS bottom extent")?;
        nonnegative(density * extent, "MOS weighted bottom density")
    }
}

/// Per-device saturation current preparation, after temperature correction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosJunctionSaturation {
    pub bottom: MosBottomDensity,
    pub sidewall_a_per_m: f64,
    pub fallback_a: f64,
}

impl MosJunctionSaturation {
    /// Use the explicit per-device saturation current only when both
    /// geometric contributions are zero. No multiplicity is applied here.
    ///
    /// # Errors
    /// Invalid geometry, units or nonrepresentable saturation current.
    pub fn prepare(self, geometry: MosDiffusionGeometry) -> Result<f64, ModelError> {
        let bottom = self.bottom.weight(geometry.bottom)?;
        let perimeter = nonnegative(geometry.perimeter_m, "MOS junction perimeter")?;
        let sidewall = nonnegative(self.sidewall_a_per_m, "MOS sidewall saturation density")?;
        let total = nonnegative(
            sidewall.mul_add(perimeter, bottom),
            "MOS junction saturation current",
        )?;
        if total > 0. {
            Ok(total)
        } else {
            nonnegative(self.fallback_a, "MOS fallback saturation current")
        }
    }
}

/// Voltage shape of a depletion contribution, at device temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosDepletionShape {
    pub potential_v: f64,
    pub grading: f64,
}

impl MosDepletionShape {
    fn component(self, capacitance_f: f64) -> Result<MosDepletionComponent, ModelError> {
        MosDepletionComponent::new(capacitance_f, self.potential_v, self.grading)
    }
}

/// Zero-bias junction capacitance densities and their voltage dependence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MosJunctionDepletion {
    pub bottom: MosBottomDensity,
    pub bottom_shape: MosDepletionShape,
    pub sidewall_f_per_m: f64,
    pub sidewall_shape: MosDepletionShape,
    pub gate_edge_f_per_m: f64,
    pub gate_edge_shape: MosDepletionShape,
    pub fallback_f: f64,
}

impl MosJunctionDepletion {
    /// Convert density/geometry into three physical depletion contributions.
    /// A nonzero direct capacitance is a fallback only for zero geometric
    /// capacitance, and follows the bottom voltage law.
    ///
    /// # Errors
    /// Invalid density, geometry, voltage law or nonrepresentable capacitance.
    pub fn prepare(
        self,
        geometry: MosDiffusionGeometry,
        sidewall: MosSidewallRule,
        gate_width_m: f64,
    ) -> Result<[MosDepletionComponent; 3], ModelError> {
        let lengths = geometry.sidewall_lengths(sidewall, gate_width_m)?;
        let bottom = self.bottom.weight(geometry.bottom)?;
        let side = nonnegative(self.sidewall_f_per_m, "MOS sidewall capacitance density")?
            * lengths.sidewall_m;
        let gate = nonnegative(self.gate_edge_f_per_m, "MOS gate-edge capacitance density")?
            * lengths.gate_edge_m;
        let total = nonnegative(bottom + side + gate, "MOS zero-bias junction capacitance")?;
        let bottom = if total > 0. {
            bottom
        } else {
            nonnegative(self.fallback_f, "MOS fallback junction capacitance")?
        };
        Ok([
            self.bottom_shape.component(bottom)?,
            self.sidewall_shape.component(side)?,
            self.gate_edge_shape.component(gate)?,
        ])
    }
}
