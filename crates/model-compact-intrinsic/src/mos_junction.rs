//! Diffusion geometry and extrinsic series resistance preparation.
//!
//! The input lengths and explicit areas already include any source-level
//! unit/global scaling. Lithographic width scaling is applied here only
//! where the chosen physical geometry rule requires it. Outputs describe
//! one device, before parallel instance multiplicity. See HSPICE MOSFET
//! Models Manual X-2005.09, pp. 44–54 and 403–404, for the distinct
//! explicit, width-normalized and folded diffusion geometries.
//!
//! This is not a diode current/charge implementation or a qualification of
//! an entire compact-model revision.

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
            self.width_m * self.width_multiplier + self.width_offset_m,
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
                (MosDiffusionRule::Stacked, false) => (2. * heavy * width, 4. * heavy + width),
                _ => (2. * heavy * width, 4. * heavy + 2. * width),
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
            MosSeriesResistanceRule::DirectContact => terminal + squares()? * sheet,
            MosSeriesResistanceRule::WidthDependent | MosSeriesResistanceRule::AutomaticSquares => {
                let width = dimensions.diffusion_width()?;
                let light = nonnegative(self.light_diffusion_m, "light MOS diffusion length")?;
                let squares = match (rule, explicit) {
                    (MosSeriesResistanceRule::AutomaticSquares, None) => {
                        dimensions.heavy_length()? / width
                    }
                    _ => squares()?,
                };
                light / width * terminal + squares * sheet
            }
        };
        nonnegative(resistance + contact, "MOS series resistance")
    }
}
