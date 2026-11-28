//! Immutable, slot-bound assignment program shared by rollback snapshots.
//! Binding uses the validated source namespace once; the hot path borrows
//! register words and never reconstructs named scalar/array maps.

use super::{Register, width_mask};
use crate::alb::{
    Expr, Expression, ExpressionParameters, ExpressionValues, StateAssignmentTarget,
    StateAssignmentValue, StatefulProgram,
};
use anyhow::{Context, Result, bail};
use std::{collections::BTreeMap, ops::RangeInclusive, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub(super) enum Scalar {
    Register(usize),
    Input { shift: usize, mask: u32 },
    Constant(u32),
    Local(usize),
}

#[derive(Clone, Debug)]
pub(super) enum Array {
    Input,
    User,
    Device,
    Register(usize),
    ReadOnly { name: String, words: Arc<[u32]> },
}

pub(super) type BoundExpr = Expression<Scalar, Array>;

#[derive(Debug)]
pub(super) enum Target {
    Register(usize),
    RegisterIndex { register: usize, index: BoundExpr },
    Local(usize),
    Output(RangeInclusive<usize>),
    Enable(RangeInclusive<usize>),
}

#[derive(Debug)]
pub(super) struct Assignment {
    pub(super) target: Target,
    condition: Option<BoundExpr>,
    value: BoundExpr,
}

impl Assignment {
    pub(super) fn synchronous_value(&self, context: &Values<'_>) -> Result<Option<u32>> {
        if self.condition.is_some() {
            return Ok(None);
        }
        self.value.evaluate_with(context).map(Some)
    }

    pub(super) fn evaluate(&self, context: &Values<'_>) -> Result<Option<(u32, bool)>> {
        if let Some(condition) = &self.condition {
            if condition.evaluate_with(context)? == 0 {
                return Ok(None);
            }
            Ok(Some((self.value.evaluate_with(context)?, true)))
        } else {
            Ok(Some((self.value.evaluate_with(context)?, false)))
        }
    }
}

#[derive(Debug)]
pub(super) struct BoundProgram {
    pub(super) assignments: Vec<Assignment>,
    pub(super) local_count: usize,
}

struct Binder {
    scalars: BTreeMap<String, Scalar>,
    arrays: BTreeMap<String, Array>,
    registers: BTreeMap<String, usize>,
    outputs: BTreeMap<String, RangeInclusive<usize>>,
    local_count: usize,
}

impl Binder {
    fn new(program: &StatefulProgram) -> Result<Self> {
        let mut scalars = BTreeMap::new();
        let mut arrays = BTreeMap::new();
        let mut registers = BTreeMap::new();
        for (index, register) in program.registers.iter().enumerate() {
            registers.insert(register.name.clone(), index);
            if register.array_size == 1 {
                scalars.insert(register.name.clone(), Scalar::Register(index));
            } else {
                arrays.insert(register.name.clone(), Array::Register(index));
            }
        }
        for (name, readonly) in &program.readonly {
            if let [value] = readonly.values.as_ref() {
                scalars.insert(name.clone(), Scalar::Constant(*value));
            } else {
                arrays.insert(
                    name.clone(),
                    Array::ReadOnly {
                        name: name.clone(),
                        words: Arc::clone(&readonly.values),
                    },
                );
            }
        }
        for port in &program.inputs {
            scalars.insert(
                port.name.clone(),
                Scalar::Input {
                    shift: port.first,
                    mask: width_mask(port.width())?,
                },
            );
        }
        let outputs = program
            .outputs
            .iter()
            .map(|port| (port.name.clone(), port.first..=port.last))
            .collect();
        // Built-in indexed namespaces have the same precedence as the
        // named evaluator, even if a source declaration uses the name.
        arrays.insert("IN".to_owned(), Array::Input);
        arrays.insert("USER".to_owned(), Array::User);
        arrays.insert("DEVICE".to_owned(), Array::Device);
        Ok(Self {
            scalars,
            arrays,
            registers,
            outputs,
            local_count: 0,
        })
    }

    fn bind_expr(&self, expression: &Expr) -> Result<BoundExpr> {
        let bind = |expr: &Expr| self.bind_expr(expr).map(Box::new);
        Ok(match expression {
            Expr::Constant(value) => Expression::Constant(*value),
            Expr::Name(name) => Expression::Name(*self.scalars.get(name).with_context(|| {
                format!("SIMetrix ALB expression references undefined value {name}")
            })?),
            Expr::Index(name, index) => Expression::Index(
                self.arrays
                    .get(name)
                    .cloned()
                    .with_context(|| format!("SIMetrix ALB expression indexes non-array {name}"))?,
                bind(index)?,
            ),
            Expr::Unary(op, value) => Expression::Unary(*op, bind(value)?),
            Expr::Binary(op, lhs, rhs) => Expression::Binary(*op, bind(lhs)?, bind(rhs)?),
            Expr::Conditional(condition, yes, no) => {
                Expression::Conditional(bind(condition)?, bind(yes)?, bind(no)?)
            }
        })
    }

    fn register(&self, name: &str) -> Result<usize> {
        self.registers
            .get(name)
            .copied()
            .with_context(|| format!("ALB assignment to undeclared register {name}"))
    }

    fn output(&self, name: &str) -> Result<RangeInclusive<usize>> {
        self.outputs
            .get(name)
            .cloned()
            .with_context(|| format!("ALB output port {name} is undeclared"))
    }

    fn bind_target(&mut self, target: &StateAssignmentTarget) -> Result<Target> {
        Ok(match target {
            StateAssignmentTarget::Register(name) => Target::Register(self.register(name)?),
            StateAssignmentTarget::RegisterIndex { name, index } => Target::RegisterIndex {
                register: self.register(name)?,
                index: self.bind_expr(index)?,
            },
            StateAssignmentTarget::Output(name) => Target::Output(self.output(name)?),
            StateAssignmentTarget::OutputRange { first, last } => Target::Output(*first..=*last),
            StateAssignmentTarget::OutputEnable(name) => Target::Enable(self.output(name)?),
            StateAssignmentTarget::Local(name) => {
                if let Some(Scalar::Local(index)) = self.scalars.get(name) {
                    Target::Local(*index)
                } else {
                    let index = self.local_count;
                    self.local_count += 1;
                    self.scalars.insert(name.clone(), Scalar::Local(index));
                    Target::Local(index)
                }
            }
        })
    }
}

impl BoundProgram {
    pub(super) fn compile(program: &StatefulProgram) -> Result<Self> {
        let mut binder = Binder::new(program)?;
        let mut assignments = Vec::with_capacity(program.assignments.len());
        for assignment in &program.assignments {
            // RHS observes the old local value before an assignment defines
            // or overwrites its slot, exactly as in source-order evaluation.
            let (condition, value) = match &assignment.value {
                StateAssignmentValue::Synchronous(value) => (None, binder.bind_expr(value)?),
                StateAssignmentValue::Asynchronous { condition, value } => {
                    (Some(binder.bind_expr(condition)?), binder.bind_expr(value)?)
                }
            };
            let target = binder.bind_target(&assignment.target)?;
            assignments.push(Assignment {
                target,
                condition,
                value,
            });
        }
        Ok(Self {
            assignments,
            local_count: binder.local_count,
        })
    }
}

/// Borrowed live register state, already packed binary physical inputs and
/// source-ordered temporary slots. In DC initialization, a new view is made
/// after each register write, not a snapshot of the register array.
pub(super) struct Values<'a> {
    pub(super) registers: &'a [Register],
    pub(super) inputs: &'a [u32],
    pub(super) input_word: u32,
    pub(super) locals: &'a [u32],
    pub(super) parameters: &'a ExpressionParameters,
}

pub(super) fn pack_inputs(inputs: &[u32]) -> u32 {
    inputs
        .iter()
        .enumerate()
        .fold(0, |word, (pin, bit)| word | (bit << pin))
}

impl ExpressionValues<Scalar, Array> for Values<'_> {
    fn scalar(&self, scalar: &Scalar) -> Result<u32> {
        Ok(match *scalar {
            Scalar::Register(index) => self.registers[index].words[0],
            Scalar::Input { shift, mask } => (self.input_word >> shift) & mask,
            Scalar::Constant(value) => value,
            Scalar::Local(index) => self.locals[index],
        })
    }

    fn indexed(&self, array: &Array, index: usize) -> Result<u32> {
        match array {
            Array::Input => self
                .inputs
                .get(index)
                .copied()
                .with_context(|| format!("SIMetrix ALB input index {index} is out of range")),
            Array::User => self.parameters.resolve_expression_value("USER", index),
            Array::Device => self.parameters.resolve_expression_value("DEVICE", index),
            Array::Register(register) => {
                let register = &self.registers[*register];
                register.words.get(index).copied().with_context(|| {
                    format!(
                        "SIMetrix ALB READONLY {} index {index} is out of range",
                        register.name
                    )
                })
            }
            Array::ReadOnly { name, words } => words.get(index).copied().with_context(|| {
                format!("SIMetrix ALB READONLY {name} index {index} is out of range")
            }),
        }
    }
}

impl Target {
    pub(super) fn register_slot(&self, context: &Values<'_>) -> Result<Option<(usize, usize)>> {
        match self {
            Self::Register(index) => {
                let register = &context.registers[*index];
                if register.words.len() != 1 {
                    bail!(
                        "ALB register array {} requires indexed assignment",
                        register.name
                    );
                }
                Ok(Some((*index, 0)))
            }
            Self::RegisterIndex { register, index } => {
                let slot = usize::try_from(index.evaluate_with(context)?)?;
                let target = &context.registers[*register];
                if slot >= target.words.len() {
                    bail!("ALB register {}[{slot}] out of range", target.name);
                }
                Ok(Some((*register, slot)))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alb::{
        EvalContext, ReadOnly, RegisterTimingDefaults, TimingEnvironment, analyze_body,
    };

    fn named_reference(
        program: &StatefulProgram,
        engine: &super::super::StatefulEventEngine,
    ) -> (BTreeMap<String, u32>, BTreeMap<String, ReadOnly>) {
        let mut named = BTreeMap::new();
        let mut arrays = program.readonly.clone();
        for register in &engine.registers {
            if let [value] = register.words.as_slice() {
                named.insert(register.name.clone(), *value);
            } else {
                arrays.insert(
                    register.name.clone(),
                    ReadOnly {
                        values: Arc::from(register.words.as_slice()),
                    },
                );
            }
        }
        for port in &program.inputs {
            let value = (port.first..=port.last)
                .enumerate()
                .fold(0, |word, (offset, pin)| {
                    word | (engine.input_bits[pin] << offset)
                });
            named.insert(port.name.clone(), value);
        }
        (named, arrays)
    }

    #[test]
    fn bound_expressions_agree_with_named_evaluation_at_live_register_states() -> Result<()> {
        // Both evaluators share arithmetic and lazy branching. The named
        // context is an independent reference for all addressing decisions,
        // including dynamic indexes, reassigned locals and asynchronous RHS.
        let (_, program) = analyze_body(
            "PORT D in[2:5]; PORT CLK in[0]; PORT OUTS out[24:31];
             READONLY (WIDTH=8) T[4]=2,4,8,16; READONLY K=3;
             EDGE(WIDTH=8,DELAY=1n,CLOCK=in[0]) R=0;
             EDGE(WIDTH=8,DELAY=1n,CLOCK=in[0]) A[4]=1,2,3,4;
             X=D+K+in[31]; Y=X; X=R^X;
             A[X&3]=T[Y&3]+USER[Y&1]; R=X+A[X&3];
             R:=!CLK ? DEVICE[0];
             OUTS=CLK ? (X && USER[0]) + (A[Y&3]<<1) : (Y&7);
             OUTS.En=CLK || (Y&1); OUT[31]=Y&1;",
        )?
        .into_parts();
        let program = program.context("missing reference program")?;
        let environment = TimingEnvironment {
            user: &[1.75, 2.5],
            device: Some(&[-1.5]),
            user_scale: 2.0,
            default_register_delay: 1e-9,
            default_output_delay: 1e-9,
            register_timing_defaults: RegisterTimingDefaults::default(),
        };
        let mut engine = program.event_engine(environment)?;
        let bound = &engine.program;
        let mut seed = 0x5ad3_b187_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..256 {
            let input_word = next();
            for (pin, bit) in engine.input_bits.iter_mut().enumerate() {
                *bit = (input_word >> pin) & 1;
            }
            for register in &mut engine.registers {
                for word in &mut register.words {
                    *word = next() & 255;
                }
            }
            let (mut named, arrays) = named_reference(&program, &engine);
            let mut locals = vec![0; bound.local_count];
            for (source, assignment) in program.assignments.iter().zip(&bound.assignments) {
                let reference = EvalContext {
                    values: &named,
                    readonly: &arrays,
                    input_bits: &engine.input_bits,
                    parameters: Some(&engine.parameters),
                };
                let context = Values {
                    registers: &engine.registers,
                    inputs: &engine.input_bits,
                    input_word: pack_inputs(&engine.input_bits),
                    locals: &locals,
                    parameters: &engine.parameters,
                };
                let expected = match &source.value {
                    StateAssignmentValue::Synchronous(value) => {
                        Some((value.evaluate(reference)?, false))
                    }
                    StateAssignmentValue::Asynchronous { condition, value } => {
                        if condition.evaluate(reference)? != 0 {
                            Some((value.evaluate(reference)?, true))
                        } else {
                            None
                        }
                    }
                };
                let actual = assignment.evaluate(&context)?;
                assert_eq!(actual, expected, "{source:?}");
                if let StateAssignmentTarget::RegisterIndex { name, index } = &source.target {
                    let slot = usize::try_from(index.evaluate(reference)?)?;
                    let register = engine
                        .registers
                        .iter()
                        .position(|reg| reg.name == *name)
                        .context("missing reference array")?;
                    assert_eq!(
                        assignment.target.register_slot(&context)?,
                        Some((register, slot))
                    );
                }
                if let (
                    StateAssignmentTarget::Local(name),
                    Target::Local(slot),
                    Some((value, false)),
                ) = (&source.target, &assignment.target, actual)
                {
                    named.insert(name.clone(), value);
                    locals[*slot] = value;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn full_width_pins_and_enable_reassignments_preserve_bit_31() -> Result<()> {
        use crate::alb::{AlbDriveStrength, AlbLogicLevel};
        let (_, program) = analyze_body(
            "PORT D in[0:31]; PORT Q out[0:31];
             COMB(WIDTH=32,DELAY=0) R=0;
             X=D; X=X|in[31]; R=X;
             Q=R; Q.En=0; OUT[31]=in[31]; Q.En=0x80000001;",
        )?
        .into_parts();
        let mut engine = program
            .context("missing full-width program")?
            .event_engine(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            })?;
        let mut inputs = [AlbLogicLevel::Zero; 32];
        inputs[31] = AlbLogicLevel::One;
        engine.advance(0.0, &inputs)?;
        assert_eq!(engine.register_words("R"), Some([0x8000_0001].as_slice()));
        for (pin, output) in engine.output().iter().enumerate() {
            if pin == 0 || pin == 31 {
                assert_eq!(output.level, AlbLogicLevel::One);
                assert_eq!(output.strength, AlbDriveStrength::Strong);
            } else {
                assert_eq!(output.level, AlbLogicLevel::Unknown);
                assert_eq!(output.strength, AlbDriveStrength::HighImpedance);
            }
        }
        Ok(())
    }
}
