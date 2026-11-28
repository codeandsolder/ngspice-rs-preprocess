//! Typed source pin layouts for the documented digital primitive families.

use super::digital_timing::TimingFamily;
use crate::{
    lexical,
    model_card::{ModelParameter, parse_assignments},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::{Deserialize, Serialize};

/// Standard or compound logic operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// Buffer.
    Buffer,
    /// Inverter.
    Inverter,
    /// Conjunction.
    And,
    /// Negated conjunction.
    Nand,
    /// Disjunction.
    Or,
    /// Negated disjunction.
    Nor,
    /// Exclusive OR.
    Xor,
    /// Exclusive NOR.
    Xnor,
    /// AND-OR.
    AndOr,
    /// AND-NOR.
    AndNor,
    /// OR-AND.
    OrAnd,
    /// OR-NAND.
    OrNand,
}

impl Gate {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "BUF" => Self::Buffer,
            "INV" => Self::Inverter,
            "AND" => Self::And,
            "NAND" => Self::Nand,
            "OR" => Self::Or,
            "NOR" => Self::Nor,
            "XOR" => Self::Xor,
            "NXOR" => Self::Xnor,
            "AO" => Self::AndOr,
            "AOI" => Self::AndNor,
            "OA" => Self::OrAnd,
            "OAI" => Self::OrNand,
            _ => return None,
        })
    }

    const fn fixed_inputs(self) -> Option<usize> {
        match self {
            Self::Buffer | Self::Inverter => Some(1),
            Self::Xor | Self::Xnor => Some(2),
            _ => None,
        }
    }

    const fn compound(self) -> bool {
        matches!(
            self,
            Self::AndOr | Self::AndNor | Self::OrAnd | Self::OrNand
        )
    }
}

/// Clock or gate behavior, not a guessed generic flip-flop.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sequential {
    /// Positive-edge D flip-flop.
    Dff,
    /// Negative-edge JK flip-flop.
    Jkff,
    /// D flip-flop with separately enabled edges.
    DffDualEdge,
    /// JK flip-flop with separately enabled edges.
    JkffDualEdge,
    /// D gated latch.
    DLatch,
    /// SR gated latch.
    SrLatch,
}

impl Sequential {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "DFF" => Self::Dff,
            "JKFF" => Self::Jkff,
            "DFFDE" => Self::DffDualEdge,
            "JKFFDE" => Self::JkffDualEdge,
            "DLTCH" => Self::DLatch,
            "SRFF" => Self::SrLatch,
            _ => return None,
        })
    }

    const fn dual_edge(self) -> bool {
        matches!(self, Self::DffDualEdge | Self::JkffDualEdge)
    }

    const fn second_data(self) -> Option<PinRole> {
        match self {
            Self::Jkff | Self::JkffDualEdge => Some(PinRole::K),
            Self::SrLatch => Some(PinRole::R),
            _ => None,
        }
    }

    const fn first_data(self) -> PinRole {
        match self {
            Self::Jkff | Self::JkffDualEdge => PinRole::J,
            Self::SrLatch => PinRole::S,
            _ => PinRole::D,
        }
    }

    const fn trigger(self) -> PinRole {
        match self {
            Self::DLatch | Self::SrLatch => PinRole::Gate,
            Self::Jkff => PinRole::ClockBar,
            _ => PinRole::Clock,
        }
    }
}

/// Primitive shape; dimensions are validated before allocating pins.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Primitive {
    /// One gate, array, or compound gate.
    Logic {
        /// Logic operation.
        operation: Gate,
        /// Inputs in each gate or compound group.
        width: usize,
        /// Number of groups in declaration order.
        groups: usize,
        /// Shared output enable is present.
        tristate: bool,
    },
    /// A bank of stateful elements with common controls.
    Sequential {
        /// Storage/trigger behavior.
        operation: Sequential,
        /// Number of state elements.
        lanes: usize,
    },
    /// Transport delay line (not an inertial buffer).
    DelayLine,
    /// Constant-state drive with strength supplied by the I/O model.
    Pull {
        /// High for pull-up, low for pull-down.
        high: bool,
        /// Number of independent outputs.
        lanes: usize,
    },
}

impl Primitive {
    /// Required timing-model class; pull devices have none.
    #[must_use = "the timing model must match the source primitive"]
    pub const fn timing_family(self) -> Option<TimingFamily> {
        Some(match self {
            Self::Logic { tristate: true, .. } => TimingFamily::Tristate,
            Self::Logic { .. } => TimingFamily::Gate,
            Self::Sequential {
                operation: Sequential::DLatch | Sequential::SrLatch,
                ..
            } => TimingFamily::GatedLatch,
            Self::Sequential { .. } => TimingFamily::EdgeFlipFlop,
            Self::DelayLine => TimingFamily::DelayLine,
            Self::Pull { .. } => return None,
        })
    }

    fn parse(name: &str, dimensions: &[usize]) -> Result<Self> {
        if let Some(operation) = Sequential::parse(name) {
            let [lanes] = dimensions else {
                anyhow::bail!("sequential primitive requires one lane count")
            };
            return Ok(Self::Sequential {
                operation,
                lanes: *lanes,
            });
        }
        match name {
            "DLYLINE" => {
                ensure!(dimensions.is_empty(), "delay line does not have dimensions");
                return Ok(Self::DelayLine);
            }
            "PULLUP" | "PULLDN" => {
                let [lanes] = dimensions else {
                    anyhow::bail!("pull primitive requires one lane count")
                };
                return Ok(Self::Pull {
                    high: name == "PULLUP",
                    lanes: *lanes,
                });
            }
            _ => {}
        }
        let (stem, array) = name
            .strip_suffix('A')
            .filter(|stem| Gate::parse(stem.trim_end_matches('3')).is_some())
            .map_or((name, false), |stem| (stem, true));
        let (stem, tristate) = stem
            .strip_suffix('3')
            .map_or((stem, false), |stem| (stem, true));
        let operation = Gate::parse(stem)
            .with_context(|| format!("unsupported PSpice U-device primitive {name}"))?;
        let compound = operation.compound();
        ensure!(
            !compound || (!array && !tristate),
            "compound gates cannot be tristate or arrays"
        );
        let (width, groups) = match (operation.fixed_inputs(), array || compound, dimensions) {
            (Some(width), false, []) => (width, 1),
            (Some(width), true, [groups]) => (width, *groups),
            (None, false, [width]) => (*width, 1),
            (None, true, [width, groups]) => (*width, *groups),
            _ => anyhow::bail!("incorrect dimensions for PSpice primitive {name}"),
        };
        ensure!(
            operation.fixed_inputs().is_some() || width >= 2,
            "logic gate requires at least two inputs"
        );
        Ok(Self::Logic {
            operation,
            width,
            groups,
            tristate,
        })
    }

    fn layout(self) -> Result<Vec<PinGroup>> {
        use PinRole as P;
        let mut layout = vec![PinGroup(P::Power, 1), PinGroup(P::Ground, 1)];
        match self {
            Self::Logic {
                operation,
                width,
                groups,
                tristate,
            } => {
                let inputs = width
                    .checked_mul(groups)
                    .context("digital input count overflow")?;
                layout.push(PinGroup(P::Input, inputs));
                if tristate {
                    layout.push(PinGroup(P::Enable, 1));
                }
                layout.push(PinGroup(
                    P::Output,
                    if operation.compound() { 1 } else { groups },
                ));
            }
            Self::Sequential { operation, lanes } => {
                layout.extend([
                    PinGroup(P::PresetBar, 1),
                    PinGroup(P::ClearBar, 1),
                    PinGroup(operation.trigger(), 1),
                ]);
                if operation.dual_edge() {
                    layout.extend([
                        PinGroup(P::PositiveEdgeEnable, 1),
                        PinGroup(P::NegativeEdgeEnable, 1),
                    ]);
                }
                layout.push(PinGroup(operation.first_data(), lanes));
                if let Some(role) = operation.second_data() {
                    layout.push(PinGroup(role, lanes));
                }
                layout.extend([PinGroup(P::Q, lanes), PinGroup(P::QBar, lanes)]);
            }
            Self::DelayLine => layout.extend([PinGroup(P::Input, 1), PinGroup(P::Output, 1)]),
            Self::Pull { lanes, .. } => layout.push(PinGroup(P::Output, lanes)),
        }
        Ok(layout)
    }
}

struct PinGroup(PinRole, usize);

/// Source electrical or digital port role.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinRole {
    /// Digital supply.
    Power,
    /// Digital reference.
    Ground,
    /// Logic input, group-major.
    Input,
    /// Logic output.
    Output,
    /// Shared tristate enable.
    Enable,
    /// Active-low preset.
    PresetBar,
    /// Active-low clear.
    ClearBar,
    /// Clock.
    Clock,
    /// Inverted clock.
    ClockBar,
    /// Level-sensitive gate.
    Gate,
    /// Positive-edge enable.
    PositiveEdgeEnable,
    /// Negative-edge enable.
    NegativeEdgeEnable,
    /// D input.
    D,
    /// J input.
    J,
    /// K input.
    K,
    /// Set input.
    S,
    /// Reset input.
    R,
    /// True output.
    Q,
    /// Complementary output.
    QBar,
}

/// A named source node, with its role and ordinal within that role.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Pin {
    /// Physical or digital role.
    pub role: PinRole,
    /// Zero-based ordinal within the group.
    pub ordinal: usize,
    /// Original node name (including special digital constants).
    pub node: String,
}

/// Complete logical U-device declaration, before occurrence elaboration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DigitalInstance {
    /// Original instance identifier.
    pub name: String,
    /// Physical statement origin.
    pub source_line: usize,
    /// Primitive and dimensions.
    pub primitive: Primitive,
    /// All pins in source order, including supplies.
    pub pins: Vec<Pin>,
    /// Timing model name; absent only for untimed pulls.
    pub timing_model: Option<String>,
    /// I/O model name.
    pub io_model: String,
    /// Original instance selector assignments in order.
    pub selectors: Vec<ModelParameter>,
}

fn header<'a>(fields: &[&'a str]) -> Result<(Primitive, usize)> {
    let token = *fields.first().context("missing PSpice U-device type")?;
    let (name, dimensions, used) = if let Some((name, dimensions)) = token.split_once('(') {
        (name, Some(dimensions), 1)
    } else if let Some(dimensions) = fields.get(1).and_then(|field| field.strip_prefix('(')) {
        (token, Some(dimensions), 2)
    } else {
        (token, None, 1)
    };
    let dimensions = dimensions.map_or_else(
        || Ok(Vec::new()),
        |dimensions| {
            dimensions
                .strip_suffix(')')
                .context("unterminated U-device dimensions")?
                .split(',')
                .map(|value| {
                    let dimension = value
                        .trim()
                        .parse::<usize>()
                        .context("U-device dimension must be a positive integer")?;
                    ensure!(dimension > 0, "zero U-device dimension");
                    Ok(dimension)
                })
                .collect::<Result<Vec<_>>>()
        },
    )?;
    Ok((
        Primitive::parse(&name.to_ascii_uppercase(), &dimensions)?,
        used,
    ))
}

fn pin_count(layout: &[PinGroup]) -> Result<usize> {
    layout.iter().try_fold(0usize, |total, group| {
        total
            .checked_add(group.1)
            .context("digital pin count overflow")
    })
}

impl DigitalInstance {
    /// Parse a logical U-device statement without evaluating timing expressions.
    ///
    /// # Errors
    /// Malformed primitive, dimensions, pins, model references or selectors.
    pub fn parse(text: &str, source_line: usize, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::ScannedBytes, text.len())?;
        let spans = lexical::tokens(text.as_bytes())?;
        let fields = spans
            .iter()
            .map(|span| &text[span.clone()])
            .collect::<Vec<_>>();
        let name = *fields.first().context("missing U-device name")?;
        ensure!(
            name.len() > 1 && name.starts_with(['u', 'U']),
            "expected U-device identifier"
        );
        let (primitive, used) = header(&fields[1..])?;
        let layout = primitive.layout()?;
        let count = pin_count(&layout)?;
        let start = 1 + used;
        let end = start
            .checked_add(count)
            .context("digital pin count overflow")?;
        let nodes = fields.get(start..end).context("missing U-device pins")?;
        let model_count = usize::from(primitive.timing_family().is_some()) + 1;
        let model_end = end
            .checked_add(model_count)
            .context("digital model count overflow")?;
        let models = fields
            .get(end..model_end)
            .context("missing U-device model references")?;
        ensure!(
            nodes
                .iter()
                .chain(models)
                .all(|field| !field.contains(['=', '(', ')', '{', '}'])),
            "invalid U-device node or model reference"
        );
        let selectors = spans.get(model_end).map_or_else(
            || Ok(Vec::new()),
            |span| parse_assignments(&text[span.start..]),
        )?;
        for field in &selectors {
            ensure!(
                field.name.eq_ignore_ascii_case("IO_LEVEL")
                    || (primitive.timing_family().is_some()
                        && field.name.eq_ignore_ascii_case("MNTYMXDLY")),
                "unsupported U-device selector {}",
                field.name
            );
        }
        budget.charge(Resource::GeneratedNodes, count)?;
        budget.charge(Resource::EmittedBytes, text.len())?;
        let mut pins = Vec::with_capacity(count);
        let mut nodes = nodes.iter();
        for PinGroup(role, width) in layout {
            for (ordinal, node) in nodes.by_ref().take(width).enumerate() {
                pins.push(Pin {
                    role,
                    ordinal,
                    node: (*node).to_owned(),
                });
            }
        }
        Ok(Self {
            name: name.to_owned(),
            source_line,
            primitive,
            pins,
            timing_model: primitive.timing_family().map(|_| models[0].to_owned()),
            io_model: models[model_count - 1].to_owned(),
            selectors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrays_compound_and_tristate_keep_distinct_groups() -> Result<()> {
        let budget = WorkBudget::default();
        let card = DigitalInstance::parse(
            "U1 NAND3A (2, 2) P G a b c d en y0 y1 T IO MNTYMXDLY={corner} IO_LEVEL=0",
            5,
            &budget,
        )?;
        assert_eq!(
            card.primitive,
            Primitive::Logic {
                operation: Gate::Nand,
                width: 2,
                groups: 2,
                tristate: true
            }
        );
        assert_eq!(card.pins[6].role, PinRole::Enable);
        assert_eq!(card.pins[7].node, "y0");
        assert_eq!(card.selectors[0].expression.as_deref(), Some("{corner}"));
        let card = DigitalInstance::parse("U2 OA(2,2) P G a b c d y T IO", 6, &budget)?;
        assert_eq!(
            card.primitive,
            Primitive::Logic {
                operation: Gate::OrAnd,
                width: 2,
                groups: 2,
                tristate: false
            }
        );
        assert_eq!(card.pins.last().context("missing output")?.node, "y");
        Ok(())
    }

    #[test]
    fn sequential_and_pull_preserve_pin_roles() -> Result<()> {
        let budget = WorkBudget::default();
        let card = DigitalInstance::parse(
            "U3 JKFFDE(2) P G pre clr clk pos neg j0 j1 k0 k1 q0 q1 qb0 qb1 TM IO",
            9,
            &budget,
        )?;
        assert_eq!(card.pins[5].role, PinRole::PositiveEdgeEnable);
        assert_eq!(card.pins[9].role, PinRole::K);
        assert_eq!(card.pins[14].role, PinRole::QBar);
        let pull = DigitalInstance::parse("U4 PULLDN(2) P G x y IO", 10, &budget)?;
        assert_eq!(pull.timing_model, None);
        assert_eq!(pull.io_model, "IO");
        let delay = DigitalInstance::parse("U5 DLYLINE P G x y T IO", 11, &budget)?;
        assert_eq!(
            delay.primitive.timing_family(),
            Some(TimingFamily::DelayLine)
        );
        Ok(())
    }

    #[test]
    fn rejects_wrong_dimensions_pin_counts_and_selectors() -> Result<()> {
        let budget = WorkBudget::default();
        for text in [
            "U1 AND(0) P G a y T IO",
            "U1 AND(2) P G a y T IO",
            "U1 BUF P G a y T IO UNKNOWN=0",
            "U1 PULLUP(1) P G a IO MNTYMXDLY=1",
            "U1 AO3(2,2) P G a b c d e y T IO",
            "U1 ANDA(18446744073709551615,2) P G a y T IO",
        ] {
            assert!(DigitalInstance::parse(text, 1, &budget).is_err(), "{text}");
        }
        Ok(())
    }
}
