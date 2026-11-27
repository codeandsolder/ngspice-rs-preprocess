//! Typed source declarations for `PSpice`'s digital primitive families.
//!
//! Pin grouping follows the source grammar, not the native code-model port
//! order. Timing and interface models remain separate, as do the power nodes
//! and logic constants; none of these are implicit ideal analog sources.

use crate::{
    model_card::parse_assignments,
    param_expression::{SpiceExpression, SpiceExpressionContext},
};
use anyhow::{Context, Result, bail, ensure};
use model_preprocess_command::{Resource, WorkBudget};

pub mod timing;

/// Combinational truth function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Gate {
    Buffer,
    Inverter,
    And,
    Nand,
    Or,
    Nor,
    Xor,
    Xnor,
    AndOr,
    AndOrInvert,
    OrAnd,
    OrAndInvert,
}

/// Sequential storage function and its triggering mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Storage {
    RisingD,
    FallingJk,
    DualEdgeD,
    DualEdgeJk,
    TransparentD,
    TransparentSr,
}

impl Storage {
    const fn two_data_inputs(self) -> bool {
        matches!(
            self,
            Self::FallingJk | Self::DualEdgeJk | Self::TransparentSr
        )
    }

    const fn clock_enable(self) -> bool {
        matches!(self, Self::DualEdgeD | Self::DualEdgeJk)
    }

    const fn transparent(self) -> bool {
        matches!(self, Self::TransparentD | Self::TransparentSr)
    }
}

/// Digital primitive shape; array lanes retain their source ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Primitive {
    Gate {
        function: Gate,
        inputs_per_gate: usize,
        gates: usize,
        tristate: bool,
    },
    Storage {
        function: Storage,
        lanes: usize,
    },
    DelayLine,
}

impl Primitive {
    /// Required timing-model family.
    #[must_use]
    pub const fn timing_family(self) -> timing::Family {
        match self {
            Self::Gate { tristate: true, .. } => timing::Family::Tristate,
            Self::Gate { .. } => timing::Family::Gate,
            Self::Storage { function, .. } if function.transparent() => timing::Family::Latch,
            Self::Storage { .. } => timing::Family::Edge,
            Self::DelayLine => timing::Family::Delay,
        }
    }

    fn gate(function: Gate, dimensions: &[usize], array: bool, tristate: bool) -> Result<Self> {
        let fixed_width = match function {
            Gate::Buffer | Gate::Inverter => Some(1),
            Gate::Xor | Gate::Xnor => Some(2),
            _ => None,
        };
        let compound = matches!(
            function,
            Gate::AndOr | Gate::AndOrInvert | Gate::OrAnd | Gate::OrAndInvert
        );
        ensure!(
            !compound || (!array && !tristate),
            "unsupported compound gate variant"
        );
        let (inputs_per_gate, gates) = match (fixed_width, array || compound, dimensions) {
            (Some(width), false, []) => (width, 1),
            (Some(width), true, [count]) => (width, *count),
            (None, false, [width]) if *width >= 2 => (*width, 1),
            (None, true, [width, count]) if *width >= 2 => (*width, *count),
            _ => bail!("invalid PSpice gate dimensions"),
        };
        Ok(Self::Gate {
            function,
            inputs_per_gate,
            gates,
            tristate,
        })
    }

    fn parse(name: &str, dimensions: &[usize]) -> Result<Self> {
        ensure!(
            dimensions.iter().all(|&n| n > 0),
            "zero PSpice primitive dimension"
        );
        let storage = match name {
            "dff" => Some(Storage::RisingD),
            "jkff" => Some(Storage::FallingJk),
            "dffde" => Some(Storage::DualEdgeD),
            "jkffde" => Some(Storage::DualEdgeJk),
            "dltch" => Some(Storage::TransparentD),
            "srff" => Some(Storage::TransparentSr),
            _ => None,
        };
        if let Some(function) = storage {
            let [lanes] = dimensions else {
                bail!("PSpice storage requires one array dimension");
            };
            return Ok(Self::Storage {
                function,
                lanes: *lanes,
            });
        }
        if name == "dlyline" {
            ensure!(
                dimensions.is_empty(),
                "PSpice delay line has no array dimensions"
            );
            return Ok(Self::DelayLine);
        }
        let (base, array) = name.strip_suffix('a').map_or((name, false), |s| (s, true));
        // OA is a compound function, not an array of an unsupported O gate.
        let (base, array) = if name == "oa" {
            (name, false)
        } else {
            (base, array)
        };
        let (base, tristate) = base.strip_suffix('3').map_or((base, false), |s| (s, true));
        let function = match base {
            "buf" => Gate::Buffer,
            "inv" => Gate::Inverter,
            "and" => Gate::And,
            "nand" => Gate::Nand,
            "or" => Gate::Or,
            "nor" => Gate::Nor,
            "xor" => Gate::Xor,
            "nxor" => Gate::Xnor,
            "ao" => Gate::AndOr,
            "aoi" => Gate::AndOrInvert,
            "oa" => Gate::OrAnd,
            "oai" => Gate::OrAndInvert,
            _ => bail!("unsupported PSpice digital primitive {name}"),
        };
        Self::gate(function, dimensions, array, tristate)
    }

    fn role_groups(self) -> Result<Vec<(Role, usize)>> {
        let groups = match self {
            Self::Gate {
                function,
                inputs_per_gate,
                gates,
                tristate,
            } => {
                let inputs = inputs_per_gate
                    .checked_mul(gates)
                    .context("PSpice gate input count overflow")?;
                let compound = matches!(
                    function,
                    Gate::AndOr | Gate::AndOrInvert | Gate::OrAnd | Gate::OrAndInvert
                );
                let mut groups = vec![(Role::Data, inputs)];
                if tristate {
                    groups.push((Role::OutputEnable, 1));
                }
                groups.push((Role::Output, if compound { 1 } else { gates }));
                groups
            }
            Self::Storage { function, lanes } => {
                let mut groups = vec![
                    (Role::PresetBar, 1),
                    (Role::ClearBar, 1),
                    (
                        if function.transparent() {
                            Role::Gate
                        } else {
                            Role::Clock
                        },
                        1,
                    ),
                ];
                if function.clock_enable() {
                    groups.extend([(Role::PositiveEdgeEnable, 1), (Role::NegativeEdgeEnable, 1)]);
                }
                groups.push((Role::Data, lanes));
                if function.two_data_inputs() {
                    groups.push((Role::SecondData, lanes));
                }
                groups.extend([(Role::Output, lanes), (Role::ComplementOutput, lanes)]);
                groups
            }
            Self::DelayLine => vec![(Role::Data, 1), (Role::Output, 1)],
        };
        Ok(groups)
    }
}

/// Source-level electrical and logical pin roles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Supply,
    Ground,
    Data,
    SecondData,
    PresetBar,
    ClearBar,
    Clock,
    Gate,
    PositiveEdgeEnable,
    NegativeEdgeEnable,
    OutputEnable,
    Output,
    ComplementOutput,
}

/// Reserved digital constants are not ordinary analog node names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Connection<'a> {
    Node(&'a str),
    GlobalSupply,
    GlobalGround,
    High,
    Low,
    Unconnected,
}

impl<'a> Connection<'a> {
    fn parse(node: &'a str) -> Self {
        match node.to_ascii_lowercase().as_str() {
            "$g_dpwr" => Self::GlobalSupply,
            "$g_dgnd" => Self::GlobalGround,
            "$d_hi" => Self::High,
            "$d_lo" => Self::Low,
            "$d_nc" => Self::Unconnected,
            _ => Self::Node(node),
        }
    }
}

/// One group of pins with shared role; ordering within the group is significant.
#[derive(Debug, Eq, PartialEq)]
pub struct Pins<'a> {
    pub role: Role,
    pub connections: Vec<Connection<'a>>,
}

/// A source U-device, before scoped resolution of its two model references.
#[derive(Debug, PartialEq)]
pub struct Device<'a> {
    pub name: &'a str,
    pub primitive: Primitive,
    pub pins: Vec<Pins<'a>>,
    pub timing_model: &'a str,
    pub io_model: &'a str,
    pub corner_override: Option<SpiceExpression>,
    pub io_level_override: Option<SpiceExpression>,
}

fn take_word(input: &str) -> (&str, &str) {
    let input = input.trim_start();
    let end = input
        .find(|c: char| c.is_ascii_whitespace() || c == '(')
        .unwrap_or(input.len());
    (&input[..end], input[end..].trim_start())
}

fn dimensions(input: &str) -> Result<(Vec<usize>, &str)> {
    let Some(rest) = input.strip_prefix('(') else {
        return Ok((Vec::new(), input));
    };
    let (body, tail) = rest
        .split_once(')')
        .context("unterminated PSpice primitive dimensions")?;
    let values = body
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|s| !s.is_empty())
        .map(str::parse)
        .collect::<Result<Vec<usize>, _>>()?;
    Ok((values, tail.trim_start()))
}

fn pin_group<'a>(
    role: Role,
    count: usize,
    tokens: &mut impl Iterator<Item = &'a str>,
) -> Result<Pins<'a>> {
    let connections = tokens
        .take(count)
        .map(Connection::parse)
        .collect::<Vec<_>>();
    ensure!(connections.len() == count, "missing PSpice {role:?} pins");
    Ok(Pins { role, connections })
}

fn fields(input: &str) -> (&str, &str) {
    // The first '=' marks the beginning of a named instance option. Scan
    // backwards to its name, keeping all preceding positional fields intact.
    let Some(eq) = input.find('=') else {
        return (input, "");
    };
    let prefix = input[..eq].trim_end();
    let start = prefix
        .rfind(|c: char| c.is_ascii_whitespace() || c == ',')
        .map_or(0, |i| i + 1);
    (&input[..start], &input[start..])
}

impl<'a> Device<'a> {
    /// Evaluate the instance policy in the occurrence's parameter context.
    ///
    /// # Errors
    /// Invalid or unresolved policy expression, or invalid circuit default.
    pub fn bind_policy(
        &self,
        defaults: timing::Policy,
        context: &mut impl SpiceExpressionContext,
        budget: &WorkBudget,
    ) -> Result<timing::Policy> {
        let mut policy = defaults;
        if let Some(expression) = &self.corner_override {
            let value = expression.evaluate_with_budget(context, budget)?;
            policy.corner =
                timing::Corner::from_override(selector(value)?)?.unwrap_or(defaults.corner);
        }
        if let Some(expression) = &self.io_level_override {
            let value = selector(expression.evaluate_with_budget(context, budget)?)?;
            if value != 0 {
                policy.io_level = value;
            }
        }
        policy.validate()?;
        Ok(policy)
    }

    /// Parse the complete logical statement, including both model names and
    /// optional instance policies. Charge expanded pins before allocation.
    ///
    /// # Errors
    /// Invalid primitive, pin arity, dimensions, policy or work budget.
    pub fn parse(input: &'a str, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::ScannedBytes, input.len())?;
        let (name, rest) = take_word(input);
        ensure!(
            name.len() > 1 && name.starts_with(['u', 'U']),
            "expected PSpice U-device"
        );
        let (primitive_name, rest) = take_word(rest);
        let (dimensions, rest) = dimensions(rest)?;
        let primitive = Primitive::parse(&primitive_name.to_ascii_lowercase(), &dimensions)?;
        let groups = primitive.role_groups()?;
        let count = groups
            .iter()
            .try_fold(2usize, |total, &(_, n)| total.checked_add(n))
            .context("PSpice digital pin count overflow")?;
        budget.charge(Resource::GeneratedNodes, count)?;
        let (positional, assignments) = fields(rest);
        let mut tokens = positional
            .split(|c: char| c.is_ascii_whitespace() || c == ',')
            .filter(|s| !s.is_empty());
        let mut pins = vec![
            pin_group(Role::Supply, 1, &mut tokens)?,
            pin_group(Role::Ground, 1, &mut tokens)?,
        ];
        for (role, n) in groups {
            pins.push(pin_group(role, n, &mut tokens)?);
        }
        let timing_model = tokens.next().context("missing PSpice timing model")?;
        let io_model = tokens.next().context("missing PSpice I/O model")?;
        ensure!(
            tokens.next().is_none(),
            "extra PSpice digital positional fields"
        );
        let mut corner_override = None;
        let mut io_level_override = None;
        for param in parse_assignments(assignments)? {
            let expression = SpiceExpression::parse_with_budget(
                param
                    .expression
                    .as_deref()
                    .context("missing PSpice instance policy")?,
                budget,
            )?;
            match param.name.to_ascii_lowercase().as_str() {
                "mntymxdly" => corner_override = Some(expression),
                "io_level" => io_level_override = Some(expression),
                _ => bail!("unsupported PSpice U-device parameter {}", param.name),
            }
        }
        Ok(Self {
            name,
            primitive,
            pins,
            timing_model,
            io_model,
            corner_override,
            io_level_override,
        })
    }
}

fn selector(value: f64) -> Result<u8> {
    match value {
        0.0 => Ok(0),
        1.0 => Ok(1),
        2.0 => Ok(2),
        3.0 => Ok(3),
        4.0 => Ok(4),
        _ => bail!("invalid PSpice digital policy selector {value}"),
    }
}

#[cfg(test)]
mod tests;
