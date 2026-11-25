//! Deterministic source-ALB register scheduler.
//!
//! Pure logical event scheduling: analogue UNIV DAC slew is deliberately outside
//! this engine. This is not the XSPICE code-model ABI and does not enable
//! production `D_LOGIC_BLOCK` lowering.
use super::{
    AlbDigitalBit, AlbDriveStrength, AlbLogicLevel, EvalContext, ExpressionParameters,
    MIN_PROPAGATION_DELAY_S, Result, StateRegisterKind, StatefulProgram, TimingEnvironment, bail,
    resolve_dynamic_value, resolve_register_delay, resolve_register_width, resolve_timing_value,
    width_mask,
};
use anyhow::Context;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlbTimedOutput {
    pub time_s: f64,
    pub pin: usize,
    pub value: AlbDigitalBit,
}

#[derive(Clone, Debug)]
struct Register {
    kind: StateRegisterKind,
    name: String,
    width: usize,
    words: Vec<u32>,
    clock: Option<usize>,
    timing: RegisterTiming,
    bitwise: bool,
    clock_high: bool,
    clock_since: f64,
    last_candidate: Vec<u32>,
    setup_history: SetupHistory,
    last_async_action: Vec<Option<u32>>,
}

/// Resolved per-register timing, after explicit source qualifiers override
/// applicable model defaults.
#[derive(Clone, Debug)]
struct RegisterTiming {
    delay: f64,
    min_clock: f64,
    hold: f64,
    setup: f64,
    async_delay: f64,
}

impl RegisterTiming {
    fn resolve(
        register: &super::StateRegister,
        environment: TimingEnvironment<'_>,
    ) -> Result<Self> {
        let delay = resolve_register_delay(register.delay.as_ref(), environment)?;
        let defaults = environment.register_timing_defaults;
        let (min_clock, hold, setup) = match register.kind {
            StateRegisterKind::Edge => (
                resolve_timing_value(
                    "MINCLOCK",
                    register.min_clock.as_ref(),
                    defaults.min_clock,
                    environment,
                )?
                .max(0.0),
                resolve_timing_value("HOLD", register.hold.as_ref(), defaults.hold, environment)?
                    .max(0.0),
                0.0,
            ),
            StateRegisterKind::Level => (
                0.0,
                0.0,
                resolve_timing_value(
                    "SETUP",
                    register.setup.as_ref(),
                    defaults.setup,
                    environment,
                )?
                .max(0.0),
            ),
            StateRegisterKind::Combinational => (0.0, 0.0, 0.0),
        };
        let async_delay = resolve_timing_value(
            "ASYNCDELAY",
            register.async_delay.as_ref(),
            delay,
            environment,
        )?;
        // The vendor's immediate register path is only selected when both
        // synchronous and asynchronous propagation are exactly zero.
        let (delay, async_delay) = if delay == 0.0 && async_delay == 0.0 {
            (0.0, 0.0)
        } else {
            (
                delay.max(MIN_PROPAGATION_DELAY_S),
                async_delay.max(MIN_PROPAGATION_DELAY_S),
            )
        };
        Ok(Self {
            delay,
            min_clock,
            hold,
            setup,
            async_delay,
        })
    }
}

/// Temporary assignment and timing data for one register. None of these
/// values survive a new evaluation, so rollback snapshots do not copy them.
#[derive(Debug, Default)]
struct RegisterFrame {
    desired: Vec<u32>,
    async_values: Vec<Option<u32>>,
    data_changed: Vec<u32>,
    rising: bool,
    current_clock: bool,
}

#[derive(Debug, Default)]
struct EvaluationFrame {
    registers: Vec<RegisterFrame>,
    locals: Vec<u32>,
    output_levels: Vec<Option<AlbLogicLevel>>,
}

impl EvaluationFrame {
    fn reset_locals(&mut self, count: usize) {
        self.locals.resize(count, 0);
        self.locals.fill(0);
    }

    fn prepare(&mut self, registers: &[Register], local_count: usize, output_width: usize) {
        self.reset_locals(local_count);
        self.output_levels.resize(output_width, None);
        self.output_levels.fill(None);
        self.registers
            .resize_with(registers.len(), RegisterFrame::default);
        for (frame, register) in self.registers.iter_mut().zip(registers) {
            frame.desired.clone_from(&register.words);
            frame.async_values.resize(register.words.len(), None);
            frame.async_values.fill(None);
            frame.data_changed.resize(register.words.len(), 0);
        }
    }
}

fn resolve_bitwise_mode(
    value: Option<&super::DynamicValue>,
    environment: TimingEnvironment<'_>,
    register_name: &str,
    default: bool,
) -> Result<bool> {
    let Some(value) = value else {
        return Ok(default);
    };
    match resolve_dynamic_value(value, environment)? {
        0.0 => Ok(false),
        1.0 => Ok(true),
        other => bail!("register {register_name} BITWISE must resolve to 0 or 1, got {other}"),
    }
}

mod events;
mod expressions;
mod physical;
mod setup;
use events::{EventKind, PendingRegisterEvents, RegisterEvent};
use expressions::{BoundProgram, Target, Values, pack_inputs};
pub use physical::StatefulPhysicalEventEngine;
use setup::SetupHistory;

// COMB and transparent LEVEL data changes use inertial scheduling:
// BITWISE=1 replaces pending transitions of changed bits; BITWISE=0
// replaces all pending transitions of their affected register words.
fn cancel_stale_synchronous_events(
    events: &mut PendingRegisterEvents,
    register_index: usize,
    bitwise: bool,
    changed_words: &[u32],
) {
    if changed_words.iter().all(|&changed| changed == 0) {
        return;
    }
    events.cancel_bits(register_index, |event| {
        if event.kind != EventKind::Synchronous {
            return 0;
        }
        let changed = changed_words[event.word];
        if !bitwise && changed != 0 {
            event.bits
        } else {
            changed
        }
    });
}

/// A general timed ALB event scheduler over the existing expression IR.
///
/// Its outputs are **digital state transitions**, not analogue midpoint
/// crossings. Hold, setup and minimum clock pulse constraints are resolved
/// at the input event boundary before scheduled commits. The event queue is
/// deterministic even when several register bits commit simultaneously.
#[derive(Debug)]
pub struct StatefulEventEngine {
    program: Arc<BoundProgram>,
    parameters: Arc<ExpressionParameters>,
    registers: Vec<Register>,
    // Vendor ALB evaluates each input as its last known binary level.
    // UNKNOWN is not an expression, clock, or asynchronous-control edge.
    input_bits: Vec<u32>,
    events: PendingRegisterEvents,
    output: Vec<AlbDigitalBit>,
    now: f64,
    // A timer-only flush advances the clock without delivering the first
    // external input sample. DC initialization and first-clock suppression
    // must track the input callback, not whether time is still -infinity.
    initialized: bool,
    frame: EvaluationFrame,
}

impl Clone for StatefulEventEngine {
    fn clone(&self) -> Self {
        Self {
            program: Arc::clone(&self.program),
            parameters: Arc::clone(&self.parameters),
            registers: self.registers.clone(),
            input_bits: self.input_bits.clone(),
            events: self.events.clone(),
            output: self.output.clone(),
            now: self.now,
            initialized: self.initialized,
            // A rollback checkpoint needs only durable logical state.
            // Allocate reusable evaluation storage on its next callback.
            frame: EvaluationFrame::default(),
        }
    }
}

impl StatefulProgram {
    /// Construct a logical register engine without authorizing proprietary
    /// source translation or loading any simulator code models.
    ///
    /// # Errors
    /// Requires resolved timing and valid register parameters.
    pub fn event_engine(&self, environment: TimingEnvironment<'_>) -> Result<StatefulEventEngine> {
        environment.validate()?;
        let parameters = ExpressionParameters::from_environment(environment);
        let mut registers = Vec::with_capacity(self.registers.len());
        for register in &self.registers {
            let width = resolve_register_width(&register.width, environment)?;
            let mask = width_mask(width)?;
            let timing = RegisterTiming::resolve(register, environment)?;
            // SIMetrix defaults BITWISE to 0 on clocked arrays, and to 1
            // on scalar EDGE/LEVEL and COMB registers.
            let bitwise = resolve_bitwise_mode(
                register.bitwise.as_ref(),
                environment,
                &register.name,
                register.kind == StateRegisterKind::Combinational || register.array_size == 1,
            )?;
            let context = EvalContext {
                values: &BTreeMap::new(),
                readonly: &self.readonly,
                input_bits: &[],
                parameters: Some(&parameters),
            };
            let mut words = register
                .initial
                .iter()
                .map(|expr| expr.evaluate(context).map(|value| value & mask))
                .collect::<Result<Vec<_>>>()?;
            if words.len() > register.array_size {
                bail!(
                    "register {} has {} initial values but array size {}",
                    register.name,
                    words.len(),
                    register.array_size
                );
            }
            // Vendor 9.40d and the ALB manual default omitted initializer
            // elements to zero, including partially initialized arrays.
            words.resize(register.array_size, 0);
            registers.push(Register {
                kind: register.kind,
                name: register.name.clone(),
                width,
                last_candidate: words.clone(),
                setup_history: SetupHistory::new(words.len(), width, bitwise, timing.setup),
                last_async_action: vec![None; words.len()],
                words,
                clock: register.clock_pin,
                timing,
                bitwise,
                clock_high: false,
                clock_since: f64::NEG_INFINITY,
            });
        }
        Ok(StatefulEventEngine {
            program: Arc::new(BoundProgram::compile(self)?),
            parameters: Arc::new(parameters),
            registers,
            input_bits: vec![0; self.input_width],
            events: PendingRegisterEvents::default(),
            output: vec![
                AlbDigitalBit {
                    level: AlbLogicLevel::Unknown,
                    strength: AlbDriveStrength::Undetermined,
                };
                self.output_width
            ],
            now: f64::NEG_INFINITY,
            initialized: false,
            frame: EvaluationFrame::default(),
        })
    }
}

impl StatefulEventEngine {
    /// Earliest uncanceled scheduled register transition. Consumers using an
    /// event-driven simulator should arrange the next wakeup at this time.
    #[must_use = "the next due time determines when the simulator must evaluate again"]
    pub fn next_due_time(&self) -> Option<f64> {
        self.events.next_due_time()
    }

    #[must_use = "the input width determines the required input vector size"]
    pub const fn input_width(&self) -> usize {
        self.input_bits.len()
    }

    #[must_use = "the output width determines the required output vector size"]
    pub const fn output_width(&self) -> usize {
        self.output.len()
    }

    #[must_use = "the returned bits describe the currently driven outputs"]
    pub fn output(&self) -> &[AlbDigitalBit] {
        &self.output
    }

    #[must_use = "the returned words describe the requested register state"]
    pub fn register_words(&self, name: &str) -> Option<&[u32]> {
        self.registers
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
            .map(|r| r.words.as_slice())
    }

    /// Consume a time-ordered input sample and all pending internal events
    /// through that time, returning physical output pin transitions in order.
    ///
    /// # Errors
    /// Rejects reverse/nonfinite time, wrong input arity, or an invalid ALB
    /// expression/index encountered during event execution.
    pub fn advance(
        &mut self,
        time_s: f64,
        inputs: &[AlbLogicLevel],
    ) -> Result<Vec<AlbTimedOutput>> {
        if !time_s.is_finite() || time_s < 0.0 || time_s < self.now {
            bail!(
                "ALB event time must be finite, nonnegative and nondecreasing (requested {time_s:.17e}, last {:.17e})",
                self.now
            );
        }
        if inputs.len() != self.input_bits.len() {
            bail!(
                "ALB event expected {} inputs, got {}",
                self.input_bits.len(),
                inputs.len()
            );
        }
        let mut output = Vec::new();
        self.drain_through(time_s, &mut output)?;
        let first = !self.initialized;
        self.now = time_s;
        // Drain earlier events with the previous effective bits before
        // accepting the new sample. A raw UNKNOWN leaves each pin unchanged;
        // known values replace history independently for every pin.
        let mut changed = false;
        for (bit, level) in self.input_bits.iter_mut().zip(inputs) {
            let next = match level {
                AlbLogicLevel::Zero => 0,
                AlbLogicLevel::One => 1,
                AlbLogicLevel::Unknown => continue,
            };
            changed |= *bit != next;
            *bit = next;
        }
        if !first && !changed {
            return Ok(output);
        }
        if first {
            self.initialize_combinational()?;
        }

        self.evaluate_assignments()?;
        self.inspect_input_changes(time_s, first);
        self.enqueue_updates(time_s, first)?;
        self.emit_outputs(time_s, &mut output);
        self.drain_through(time_s, &mut output)?;
        self.initialized = true;
        // When an event and a boundary input change have equal times, preserve
        // the deterministic timestamp order of their serialized changes.
        Ok(output)
    }

    fn inspect_input_changes(&mut self, time_s: f64, first: bool) {
        for (index, (reg, frame)) in self
            .registers
            .iter_mut()
            .zip(&mut self.frame.registers)
            .enumerate()
        {
            let prev_clock = reg.clock_high;
            let current_clock = reg.clock.is_some_and(|pin| self.input_bits[pin] != 0);
            let rising = !prev_clock && current_clock;
            let falling = prev_clock && !current_clock;
            if first {
                reg.clock_high = current_clock;
                reg.clock_since = time_s;
            } else if rising || falling {
                let old_since = reg.clock_since;
                reg.clock_high = current_clock;
                reg.clock_since = time_s;
                if falling
                    && reg.kind == StateRegisterKind::Edge
                    && time_s - old_since < reg.timing.min_clock
                {
                    self.events.cancel_where(index, |event| {
                        event.kind == EventKind::Synchronous
                            && event.source_time_s >= old_since
                            && event.source_time_s <= time_s
                    });
                }
            }
            // Initial COMB assignments are settled at DC before timed events.
            // Do not interpret their post-initialization feedback as a
            // transient candidate change at the initial time.
            let data_changed = &mut frame.data_changed;
            if first && reg.kind == StateRegisterKind::Combinational {
                data_changed.fill(0);
            } else {
                for (word, next) in frame.desired.iter().enumerate() {
                    let previous_word = reg.last_candidate[word];
                    let changed = *next ^ previous_word;
                    data_changed[word] = changed;
                    reg.setup_history.record(word, changed, time_s);
                }
                reg.last_candidate.clone_from(&frame.desired);
            }
            // Measured in Elements 9.40d: EDGE HOLD always rejects per
            // changed bit, even with BITWISE=0 on scalar and array
            // registers. LEVEL SETUP *does* honor whole-word BITWISE=0.
            // This differs from the vendor manual's EDGE description.
            if reg.kind == StateRegisterKind::Edge
                && reg.timing.hold > 0.0
                && data_changed.iter().any(|&changed| changed != 0)
            {
                self.events.cancel_bits(index, |event| {
                    if event.kind == EventKind::Synchronous
                        && time_s > event.source_time_s
                        && time_s < event.source_time_s + reg.timing.hold
                    {
                        data_changed[event.word]
                    } else {
                        0
                    }
                });
            }
            if falling && reg.kind == StateRegisterKind::Level && reg.timing.setup > 0.0 {
                self.events.cancel_bits(index, |event| {
                    if event.kind != EventKind::Synchronous {
                        return 0;
                    }
                    reg.setup_history
                        .reject_bits(event.word, event.bits, time_s, reg.timing.setup)
                });
            }

            frame.rising = rising;
            frame.current_clock = current_clock;
        }
    }

    fn enqueue_updates(&mut self, time_s: f64, first: bool) -> Result<()> {
        for (index, (reg, frame)) in self
            .registers
            .iter_mut()
            .zip(&self.frame.registers)
            .enumerate()
        {
            let RegisterFrame {
                rising,
                current_clock,
                data_changed,
                desired,
                async_values,
            } = frame;
            let rising = *rising;
            let current_clock = *current_clock;
            let register_mask = width_mask(reg.width)?;
            // Independent bit transitions share one masked word write.
            // LEVEL SETUP and COMB retain their BITWISE=0 whole-word behavior.
            let schedule_sync = match reg.kind {
                StateRegisterKind::Edge => !first && rising,
                StateRegisterKind::Level => {
                    current_clock
                        && (!first && rising || data_changed.iter().any(|mask| *mask != 0))
                }
                StateRegisterKind::Combinational => {
                    !first && data_changed.iter().any(|mask| *mask != 0)
                }
            };
            if reg.kind == StateRegisterKind::Combinational
                || (reg.kind == StateRegisterKind::Level && schedule_sync)
            {
                cancel_stale_synchronous_events(&mut self.events, index, reg.bitwise, data_changed);
            }
            if schedule_sync {
                let due = time_s + reg.timing.delay;
                if !due.is_finite() {
                    bail!("register {} synchronous event time overflow", reg.name);
                }
                for (word, desired_word) in desired.iter().enumerate() {
                    // An asserted asynchronous assignment dominates clocked
                    // captures for as long as it remains active, not merely
                    // on the edge when its value first changes. A simultaneous
                    // deassertion leaves this word eligible for capture.
                    if async_values[word].is_some() {
                        continue;
                    }
                    let change_mask =
                        if reg.kind == StateRegisterKind::Edge || !reg.bitwise || rising || first {
                            register_mask
                        } else {
                            data_changed[word] & register_mask
                        };
                    self.events.schedule_word(
                        due,
                        RegisterEvent {
                            source_time_s: time_s,
                            register: index,
                            word,
                            bits: change_mask,
                            value: *desired_word,
                            kind: EventKind::Synchronous,
                        },
                    );
                }
            }
            for (word, value) in async_values.iter().enumerate() {
                if *value != reg.last_async_action[word] {
                    // Cancel stale async actions on deassertion or replacement
                    // before the scheduled transition becomes visible.
                    self.events.cancel_where(index, |event| {
                        event.word == word && event.kind == EventKind::Asynchronous
                    });
                    reg.last_async_action[word] = *value;
                    if let Some(value) = value {
                        let due = time_s + reg.timing.async_delay;
                        if !due.is_finite() {
                            bail!("register {} asynchronous event time overflow", reg.name);
                        }
                        // Asynchronous assertion takes precedence over older
                        // pending synchronous commits. Deassertion itself does
                        // not restore a previous value.
                        self.events.cancel_where(index, |event| {
                            event.word == word && event.kind == EventKind::Synchronous
                        });
                        self.events.schedule_word(
                            due,
                            RegisterEvent {
                                source_time_s: time_s,
                                register: index,
                                word,
                                bits: register_mask,
                                value: *value,
                                kind: EventKind::Asynchronous,
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Drain pending register events without an external input change.
    ///
    /// # Errors
    /// Rejects reverse/nonfinite time and evaluation failures.
    pub fn flush(&mut self, time_s: f64) -> Result<Vec<AlbTimedOutput>> {
        if !time_s.is_finite() || time_s < 0.0 || time_s < self.now {
            bail!("invalid ALB flush time");
        }
        let mut output = Vec::new();
        self.drain_through(time_s, &mut output)?;
        self.now = time_s;
        Ok(output)
    }

    fn zero_delay_signature(&self, time_s: f64) -> Vec<u64> {
        let mut signature = Vec::new();
        for register in &self.registers {
            signature.push(register.words.len() as u64);
            signature.extend(register.words.iter().map(|word| u64::from(*word)));
        }
        signature.push(u64::MAX);
        self.events.append_signature_at(time_s, &mut signature);
        signature
    }

    fn drain_through(&mut self, until: f64, transitions: &mut Vec<AlbTimedOutput>) -> Result<()> {
        let mut zero_delay_time: Option<f64> = None;
        let mut same_time_states = std::collections::BTreeSet::new();
        while let Some(time_s) = self.events.dispatch_next_due(until, |event| {
            let reg = &mut self.registers[event.register];
            if reg.last_async_action[event.word].is_some() && event.kind == EventKind::Synchronous {
                return;
            }
            let word = &mut reg.words[event.word];
            *word = (*word & !event.bits) | (event.value & event.bits);
        }) {
            // Internal commits can change other registers' expressions
            // even without a new external input sample.
            self.evaluate_assignments()?;
            self.emit_outputs(time_s, transitions);
            self.inspect_input_changes(time_s, false);
            self.enqueue_updates(time_s, false)?;
            if self.events.has_due_at(time_s) {
                if zero_delay_time.is_none_or(|previous| previous.total_cmp(&time_s).is_ne()) {
                    same_time_states.clear();
                    zero_delay_time = Some(time_s);
                }
                if !same_time_states.insert(self.zero_delay_signature(time_s)) {
                    bail!("ALB zero-delay register feedback has no stable state at {time_s} s");
                }
            } else {
                same_time_states.clear();
                zero_delay_time = None;
            }
        }
        Ok(())
    }

    /// Evaluate COMB registers at the initial operating point, once in
    /// source order, without applying transient propagation delays.
    ///
    /// The vendor starts even self-referential COMB expressions from their
    /// evaluated DC value. Their internal assignments do not constitute
    /// an input transition that would launch a delayed feedback oscillator.
    fn initialize_combinational(&mut self) -> Result<()> {
        if self
            .registers
            .iter()
            .all(|reg| reg.kind != StateRegisterKind::Combinational)
        {
            return Ok(());
        }
        let inputs = &self.input_bits;
        let input_word = pack_inputs(inputs);
        self.frame.reset_locals(self.program.local_count);
        for assignment in &self.program.assignments {
            let context = Values {
                registers: &self.registers,
                inputs,
                input_word,
                locals: &self.frame.locals,
                parameters: &self.parameters,
            };
            let Some(value) = assignment.synchronous_value(&context)? else {
                continue;
            };
            match assignment.target {
                Target::Local(index) => self.frame.locals[index] = value,
                Target::Register(index) => {
                    let reg = &mut self.registers[index];
                    if reg.kind == StateRegisterKind::Combinational {
                        reg.words[0] = value & width_mask(reg.width)?;
                    }
                }
                _ => {}
            }
        }
        // Candidate history tracks the *expression result* at the final
        // DC state, not the stored register value. They can differ in
        // feedback networks. Otherwise an unrelated input callback would
        // spuriously schedule a delayed transition with no causal change.
        self.evaluate_assignments()?;
        for (reg, frame) in self.registers.iter_mut().zip(&self.frame.registers) {
            if reg.kind == StateRegisterKind::Combinational {
                reg.last_candidate.clone_from(&frame.desired);
            }
        }
        Ok(())
    }

    fn evaluate_assignments(&mut self) -> Result<()> {
        let inputs = &self.input_bits;
        let input_word = pack_inputs(inputs);
        self.frame
            .prepare(&self.registers, self.program.local_count, self.output.len());
        let frame = &mut self.frame;
        let mut enabled = u32::MAX;
        for assignment in &self.program.assignments {
            let context = Values {
                registers: &self.registers,
                inputs,
                input_word,
                locals: &frame.locals,
                parameters: &self.parameters,
            };
            let Some((value, asynchronous)) = assignment.evaluate(&context)? else {
                continue;
            };
            match &assignment.target {
                Target::Register(_) | Target::RegisterIndex { .. } => {
                    let (register, word) = assignment
                        .target
                        .register_slot(&context)?
                        .context("missing bound register target")?;
                    let value = value & width_mask(self.registers[register].width)?;
                    if asynchronous {
                        frame.registers[register].async_values[word] = Some(value);
                    } else {
                        frame.registers[register].desired[word] = value;
                    }
                }
                Target::Local(index) => frame.locals[*index] = value,
                Target::Output(range) => {
                    for (offset, output) in
                        frame.output_levels[range.clone()].iter_mut().enumerate()
                    {
                        *output = Some(if value & (1_u32 << offset) == 0 {
                            AlbLogicLevel::Zero
                        } else {
                            AlbLogicLevel::One
                        });
                    }
                }
                Target::Enable(range) => {
                    let mask = width_mask(range.end() - range.start() + 1)?;
                    enabled =
                        (enabled & !(mask << range.start())) | ((value & mask) << range.start());
                }
            }
        }
        // Named PORT.EN is applied to final pin assignments, including
        // direct OUT[n] writes. Declared output ranges never overlap.
        for (pin, level) in frame.output_levels.iter_mut().enumerate() {
            if enabled & (1_u32 << pin) == 0 && level.is_some() {
                *level = Some(AlbLogicLevel::Unknown);
            }
        }
        Ok(())
    }

    fn emit_outputs(&mut self, time_s: f64, result: &mut Vec<AlbTimedOutput>) {
        for (pin, level) in self.frame.output_levels.iter().copied().enumerate() {
            let Some(level) = level else { continue };
            let value = AlbDigitalBit {
                level,
                strength: if level == AlbLogicLevel::Unknown {
                    AlbDriveStrength::HighImpedance
                } else {
                    AlbDriveStrength::Strong
                },
            };
            if self.output[pin] != value {
                self.output[pin] = value;
                result.push(AlbTimedOutput { time_s, pin, value });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alb::analyze_body;

    fn engine(deck: &str) -> Result<StatefulEventEngine> {
        let body = deck
            .lines()
            .skip_while(|line| !line.trim().to_ascii_uppercase().starts_with(".FILE "))
            .skip(1)
            .take_while(|line| !line.trim().eq_ignore_ascii_case(".ENDF"))
            .collect::<Vec<_>>()
            .join("\n");
        let (_, program) = analyze_body(&body)?.into_parts();
        program
            .context("expected vendor ALB register body")?
            .event_engine(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })
    }

    fn events(
        source: &str,
        changes: &[(f64, &[AlbLogicLevel])],
    ) -> Result<Vec<(f64, AlbLogicLevel)>> {
        let mut engine = engine(source)?;
        let mut output = Vec::new();
        for (time, inputs) in changes {
            output.extend(engine.advance(*time * 1e-9, inputs)?);
        }
        output.extend(engine.flush(60e-9)?);
        Ok(output
            .into_iter()
            .filter(|event| event.time_s > 1e-12 && event.pin == 0)
            .map(|event| (event.time_s * 1e9, event.value.level))
            .collect())
    }

    fn verify(events: &[(f64, AlbLogicLevel)], expected: &[(f64, AlbLogicLevel)]) {
        assert_eq!(events.len(), expected.len(), "{events:?}");
        for (actual, expected) in events.iter().zip(expected) {
            assert!(
                (actual.0 - expected.0).abs() < 1e-5,
                "{actual:?} != {expected:?}"
            );
            assert_eq!(actual.1, expected.1);
        }
    }

    use AlbLogicLevel::{One as H, Zero as L};

    #[test]
    fn readonly_arrays_share_immutable_storage_across_program_clones() -> Result<()> {
        let (_, parsed) = analyze_body(
            "READONLY (WIDTH=2) Table[8] = 0, 1, 2, 3, 0, 1, 2, 3;
             PORT Data in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
             LEVEL (WIDTH=2, DELAY=1n, CLOCK=in[2]) R=0;
             R=Table[Data]; Q=R;",
        )?
        .into_parts();
        let program = parsed.context("missing READONLY register program")?;
        let duplicate = program.clone();
        assert!(Arc::ptr_eq(
            &program.readonly["TABLE"].values,
            &duplicate.readonly["TABLE"].values
        ));
        let mut runtime = program.event_engine(TimingEnvironment {
            user: &[],
            device: None,
            user_scale: 1.0,
            default_register_delay: 1e-9,
            default_output_delay: 1e-9,
            register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
        })?;
        runtime.advance(0.0, &[L, L, L])?;
        runtime.advance(5e-9, &[H, H, L])?;
        runtime.advance(10e-9, &[H, H, H])?;
        let changes = runtime.flush(12e-9)?;
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|change| change.value.level == H));
        assert_eq!(runtime.register_words("R"), Some([3].as_slice()));
        Ok(())
    }

    #[test]
    fn original_vendor_invalid_output_read_netlists_are_rejected() -> Result<()> {
        // Both original source decks were executed in Elements 9.40d and
        // independently produced "Attempt to read output pin" in .out.
        for deck in [
            include_str!(
                "../../../../runtime/xspice/fixtures/vendor-invalid/output-alias-edge.net"
            ),
            include_str!(
                "../../../../runtime/xspice/fixtures/vendor-invalid/output-alias-stateless.net"
            ),
        ] {
            let body = deck
                .lines()
                .skip_while(|line| !line.trim().to_ascii_uppercase().starts_with(".FILE "))
                .skip(1)
                .take_while(|line| !line.trim().eq_ignore_ascii_case(".ENDF"))
                .collect::<Vec<_>>()
                .join("\n");
            let error = analyze_body(&body)
                .err()
                .context("actual Elements negative oracle should be rejected at parse time")?;
            assert!(
                error.to_string().contains("read output pin Q"),
                "unexpected error for original vendor netlist: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn vendor_rejects_output_pin_reads_in_edge_and_comb_expressions() -> Result<()> {
        // Real Elements 9.40d: Model MALIAS_EDGE: "R = Q":
        // "Attempt to read output pin". The output is a write-only port,
        // not a scalar alias, regardless of its earlier source assignment.
        for body in [
            "PORT Data in[0]; PORT Clock in[1]; PORT Q out[0]; PORT Z out[1];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0;
             Q=Data; R=Q; Z=R;",
            "PORT D in[0]; PORT Q out[0]; PORT Z out[1];
             COMB (WIDTH=1, DELAY=1n) R=0;
             Q=D; R=Q; Z=R;",
        ] {
            let error = analyze_body(body)
                .err()
                .context("SIMetrix output pin reads must be rejected during parsing")?;
            assert!(
                error.to_string().contains("read output pin Q"),
                "unexpected error: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn vendor_output_pin_read_rejection_is_recursive() -> Result<()> {
        for body in [
            "PORT D in[0]; PORT Clock in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0;
             Q=D; R:=Q ? 0; R=D;",
            "PORT D in[0]; PORT Clock in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R[2]=0,0;
             Q=D; R[Q]=D;",
            "PORT D in[0]; PORT Clock in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0;
             Q=D; R=D ? (Q+1) : 0;",
        ] {
            let error = analyze_body(body)
                .err()
                .context("nested output pin reads must be rejected during parsing")?;
            assert!(
                error.to_string().contains("read output pin Q"),
                "unexpected error: {error:#}"
            );
        }
        Ok(())
    }

    #[test]
    fn unchanged_input_samples_match_sparse_scheduling_across_register_kinds() -> Result<()> {
        // Check both redundant callbacks at the exact external event time
        // and redundant callbacks when a previously scheduled timer is due.
        // COMB, EDGE, LEVEL, and asynchronous reset all share this rule.
        let cases = [
            (
                "PORT Data in[0:1]; PORT Clock in[2]; PORT Q out[0:1];
                 EDGE (WIDTH=2, DELAY=1n, HOLD=600p, CLOCK=in[2]) R=0;
                 R=Data; Q=R;",
                [
                    [L, L, L],
                    [H, L, L],
                    [H, H, H],
                    [L, H, L],
                    [H, H, H],
                    [L, L, L],
                ],
            ),
            (
                "PORT Data in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
                 LEVEL (WIDTH=2, DELAY=1n, SETUP=500p, CLOCK=in[2]) R=0;
                 R=Data; Q=R;",
                [
                    [L, L, L],
                    [H, L, L],
                    [H, H, H],
                    [L, H, H],
                    [H, H, L],
                    [L, L, L],
                ],
            ),
            (
                "PORT Data in[0:1]; PORT Gate in[2]; PORT Q out[0:1];
                 COMB (WIDTH=2, DELAY=1n) R=0;
                 R=Data; Q=R;",
                [
                    [L, L, L],
                    [H, L, L],
                    [H, H, H],
                    [L, H, H],
                    [H, H, L],
                    [L, L, L],
                ],
            ),
            (
                "PORT Reset in[0]; PORT Data in[1]; PORT Clock in[2]; PORT Q out[0];
                 EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=200p, CLOCK=in[2]) R=0;
                 R=Data; R:=!Reset ? 0; Q=R;",
                [
                    [H, L, L],
                    [H, H, L],
                    [H, H, H],
                    [L, H, L],
                    [H, H, H],
                    [H, L, L],
                ],
            ),
        ];
        for (source, samples) in cases {
            let mut dense = source_engine(source)?;
            let mut sparse = source_engine(source)?;
            let mut dense_transitions = Vec::new();
            let mut sparse_transitions = Vec::new();
            for (i, sample) in samples.iter().enumerate() {
                let time = f64::from(u32::try_from(i)?) * 10e-9;
                sparse_transitions.extend(sparse.advance(time, sample)?);
                dense_transitions.extend(dense.advance(time, sample)?);
                assert_eq!(dense.register_words("R"), sparse.register_words("R"));
                assert_eq!(dense.next_due_time(), sparse.next_due_time());
                dense_transitions.extend(dense.advance(time, sample)?);
                for offset in [0.3e-9, 1e-9, 2e-9, 3e-9] {
                    dense_transitions.extend(dense.advance(time + offset, sample)?);
                }
            }
            sparse_transitions.extend(sparse.flush(60e-9)?);
            dense_transitions.extend(dense.flush(60e-9)?);
            assert_eq!(dense_transitions, sparse_transitions, "source: {source}");
            assert_eq!(dense.output(), sparse.output(), "source: {source}");
            assert_eq!(dense.register_words("R"), sparse.register_words("R"));
            assert_eq!(dense.next_due_time(), sparse.next_due_time());
        }
        Ok(())
    }

    #[test]
    fn independent_vector_bits_and_indexed_register_arrays() -> Result<()> {
        let (_, vector) = analyze_body(
            "PORT D in[0:1]; PORT CLK in[2]; PORT Q out[0:1];              EDGE (WIDTH=2, DELAY=1n, CLOCK=in[2]) R=0; R=D; Q=R;"
        )?.into_parts();
        let mut vector =
            vector
                .context("vector EDGE source missing")?
                .event_engine(TimingEnvironment {
                    user: &[],
                    device: None,
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 1e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                })?;
        vector.advance(0.0, &[L, L, L])?;
        vector.advance(5e-9, &[H, H, L])?;
        vector.advance(10e-9, &[H, H, H])?;
        let transitions = vector.flush(12e-9)?;
        assert_eq!(transitions.len(), 2);
        assert!(
            transitions
                .iter()
                .all(|event| (event.time_s - 11e-9).abs() < 1e-15)
        );
        assert!(transitions.iter().all(|event| event.value.level == H));

        let (_, array) = analyze_body(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0];              EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R[2]=0,0;              R[0]=D; R[1]=!D; Q=R[0];"
        )?.into_parts();
        let mut array =
            array
                .context("array EDGE source missing")?
                .event_engine(TimingEnvironment {
                    user: &[],
                    device: None,
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 1e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                })?;
        array.advance(0.0, &[L, L])?;
        array.advance(5e-9, &[H, L])?;
        array.advance(10e-9, &[H, H])?;
        let output = array.flush(12e-9)?;
        assert_eq!(array.register_words("R"), Some([1, 0].as_slice()));
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].value.level, H);
        Ok(())
    }

    #[test]
    fn resolves_user_and_device_as_unsigned_expression_constants() -> Result<()> {
        // Each expression was independently accepted by Elements 9.40d.
        // Negative fractional values truncate toward zero and are then
        // interpreted as unsigned 32-bit words.
        for (expression, user, device, scale, expected) in [
            ("USER[0]>0", &[-1.5][..], None, 1.0, true),
            ("USER[0]<1", &[-1.5][..], None, 1.0, false),
            ("USER[0]==-1", &[-1.5][..], None, 1.0, true),
            ("USER[0]>0", &[4_294_967_295.0][..], None, 1.0, true),
            ("USER[0]>1", &[1.5][..], None, 1.0, false),
            ("USER[0]>1", &[2.5][..], None, 1.0, true),
            ("USER[0]>1", &[0.75][..], None, 3.0, true),
            ("USER[0]==2", &[-0.75][..], None, -3.0, true),
            ("USER[USER[0]]==2", &[1.5, 2.0][..], None, 1.0, true),
            ("DEVICE[0]>0", &[][..], Some(&[0.5][..]), 1.0, false),
            (
                "USER[0]+DEVICE[0]==3",
                &[1.0][..],
                Some(&[2.0][..]),
                1.0,
                true,
            ),
        ] {
            let source = format!(
                "PORT Data in[0]; PORT Q out[0]; \
                 COMB (WIDTH=1, DELAY=1n) Stage=0; \
                 Stage=({expression}) ? Data : 0; Q=Stage;"
            );
            let (_, program) = analyze_body(&source)?.into_parts();
            let mut runtime =
                program
                    .context("expected COMB program")?
                    .event_engine(TimingEnvironment {
                        user,
                        device,
                        user_scale: scale,
                        default_register_delay: 1e-9,
                        default_output_delay: 1e-9,
                        register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                    })?;
            runtime.advance(0.0, &[L])?;
            runtime.advance(10e-9, &[H])?;
            runtime.flush(12e-9)?;
            assert_eq!(
                runtime.output()[0].level == H,
                expected,
                "expression={expression}, scale={scale}, user={user:?}, device={device:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn resolves_parameters_in_initializers_asynchronous_conditions_and_indexes() -> Result<()> {
        let (_, program) = analyze_body(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0]; \
             EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=1n, CLOCK=in[1]) R[2]=USER[0], DEVICE[0]; \
             EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=1n, CLOCK=in[1]) Reset=0; \
             Reset := DEVICE[1] ? USER[2]; \
             R[USER[1]]=D; Q=R[USER[1]];",
        )?
        .into_parts();
        let mut runtime =
            program
                .context("expected EDGE program")?
                .event_engine(TimingEnvironment {
                    user: &[1.0, 1.0, 1.0],
                    device: Some(&[0.0, 1.0]),
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 1e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                })?;
        assert_eq!(runtime.register_words("R"), Some([1, 0].as_slice()));
        runtime.advance(0.0, &[L, L])?;
        runtime.flush(2e-9)?;
        assert_eq!(runtime.register_words("Reset"), Some([1].as_slice()));
        // Updating the event-driven input leaves USER and DEVICE unchanged.
        runtime.advance(5e-9, &[H, L])?;
        runtime.advance(10e-9, &[H, H])?;
        runtime.flush(12e-9)?;
        assert_eq!(runtime.register_words("R"), Some([1, 1].as_slice()));
        Ok(())
    }

    #[test]
    fn unresolved_expression_parameters_fail_on_actual_access() -> Result<()> {
        let source = "PORT D in[0]; PORT Q out[0]; \
                      COMB (WIDTH=1, DELAY=1n) Stage=0; \
                      Stage=DEVICE[0] ? D : 0; Q=Stage;";
        let (_, program) = analyze_body(source)?.into_parts();
        let mut missing = program
            .context("missing program")?
            .event_engine(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })?;
        let error = missing
            .advance(0.0, &[L])
            .err()
            .context("expected missing expression parameter to fail")?
            .to_string();
        assert!(error.contains("DEVICE[0]"), "{error}");
        let (_, program) = analyze_body(
            "PORT D in[0]; PORT Q out[0]; COMB (WIDTH=1, DELAY=1n) R=0; \
             R=(USER[1] ? D : 0); Q=R;",
        )?
        .into_parts();
        let mut missing = program
            .context("missing program")?
            .event_engine(TimingEnvironment {
                user: &[1.0],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })?;
        let error = missing
            .advance(0.0, &[L])
            .err()
            .context("expected missing expression parameter to fail")?
            .to_string();
        assert!(error.contains("USER[1]"), "{error}");
        Ok(())
    }

    #[test]
    fn implicit_zero_initialization_matches_vendor() -> Result<()> {
        let (_, program) = analyze_body(
            "PORT CLK in[0]; PORT Q out[0];              EDGE (WIDTH=1, DELAY=1n, CLOCK=in[0]) R;              R=!R; Q=R;"
        )?.into_parts();
        let mut runtime = program
            .context("missing register")?
            .event_engine(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })?;
        assert_eq!(runtime.register_words("R"), Some([0].as_slice()));
        runtime.advance(0.0, &[L])?;
        runtime.advance(5e-9, &[H])?;
        runtime.flush(7e-9)?;
        assert_eq!(runtime.register_words("R"), Some([1].as_slice()));
        for source in [
            "PORT D in[0]; PORT Q out[0]; COMB (WIDTH=1, DELAY=1n) R; R=D; Q=R;",
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0]; LEVEL (WIDTH=1, DELAY=1n, CLOCK=in[1]) R; R=D; Q=R;",
        ] {
            let runtime = source_engine(source)?;
            assert_eq!(runtime.register_words("R"), Some([0].as_slice()));
            assert!(runtime.registers[0].bitwise);
        }
        Ok(())
    }

    #[test]
    fn partial_array_initializer_defaults_missing_elements_to_zero() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Clock in[1]; PORT Q0 out[0]; PORT Q1 out[1]; \
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R[2]=1; \
             R[0]=D; R[1]=!D; Q0=R[0]; Q1=R[1];",
        )?;
        assert_eq!(runtime.register_words("R"), Some([1, 0].as_slice()));
        assert!(!runtime.registers[0].bitwise);
        runtime.advance(0.0, &[L, L])?;
        runtime.advance(10e-9, &[L, H])?;
        runtime.flush(12e-9)?;
        assert_eq!(runtime.register_words("R"), Some([0, 1].as_slice()));
        Ok(())
    }

    fn source_engine(source: &str) -> Result<StatefulEventEngine> {
        let (_, program) = analyze_body(source)?.into_parts();
        program
            .context("expected a stateful register program")?
            .event_engine(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })
    }

    #[test]
    fn reused_evaluation_frame_matches_fresh_rollback_snapshots() -> Result<()> {
        let mut reused = source_engine(
            "PORT D in[0:3]; PORT CK in[4]; PORT EN in[5]; PORT RST in[6]; PORT Q out[0:7];
             COMB(WIDTH=4,DELAY=1n) C=0;
             EDGE(WIDTH=4,DELAY=2n,ASYNCDELAY=1n,CLOCK=in[4]) E=0;
             LEVEL(WIDTH=4,DELAY=1n,CLOCK=in[5]) Latch=0;
             EDGE(WIDTH=4,DELAY=2n,CLOCK=in[4]) A[4]=0;
             X=D & 3; C=D; E=C; E:=!RST ? 0; Latch=E;
             A[X]=Latch; Q=(A[X]<<4)|Latch; Q.En=EN ? 255 : 15;",
        )?;
        let mut cold = reused.clone();
        let mut seed = 0x9a75_1cf2_u32;
        for sample in 0..1024_u32 {
            let mut inputs = [L; 7];
            for level in &mut inputs {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *level = match seed & 3 {
                    0 => AlbLogicLevel::Unknown,
                    1 => L,
                    _ => H,
                };
            }
            let time = f64::from(sample) * 500e-12;
            // Restored snapshots retain the actual state, not scratch
            // assignments, masks, locals or output vectors from the past.
            cold = cold.clone();
            assert_eq!(cold.frame.registers.len(), 0);
            assert_eq!(cold.frame.locals.len(), 0);
            assert_eq!(cold.frame.output_levels.len(), 0);
            assert_eq!(reused.advance(time, &inputs)?, cold.advance(time, &inputs)?);
            assert_eq!(reused.flush(time + 250e-12)?, cold.flush(time + 250e-12)?);
            assert_eq!(
                reused.next_due_time().map(f64::to_bits),
                cold.next_due_time().map(f64::to_bits)
            );
            assert_eq!(reused.output(), cold.output());
            for name in ["C", "E", "Latch", "A"] {
                assert_eq!(reused.register_words(name), cold.register_words(name));
            }
        }
        assert_eq!(reused.frame.registers.len(), 4);
        Ok(())
    }

    #[test]
    fn signed_register_defaults_preserve_the_joint_zero_delay_path() -> Result<()> {
        let source = "PORT D in[0]; PORT CK in[1]; PORT Q out[0]; \
            EDGE (WIDTH=1,CLOCK=in[1]) R=0; R=D; Q=R;";
        let (_, program) = analyze_body(source)?.into_parts();
        let program = program.context("missing timing-domain fixture")?;
        let environment = TimingEnvironment {
            user: &[],
            device: None,
            user_scale: -1.0,
            default_register_delay: 0.0,
            default_output_delay: 1e-9,
            register_timing_defaults: crate::alb::RegisterTimingDefaults {
                hold: -1e-9,
                min_clock: -1e-9,
                setup: -1e-9,
            },
        };
        for (requested, expected) in [
            (-1e-9, MIN_PROPAGATION_DELAY_S),
            (0.0, 0.0),
            (0.5e-12, MIN_PROPAGATION_DELAY_S),
            (2e-12, 2e-12),
        ] {
            let engine = program.event_engine(TimingEnvironment {
                default_register_delay: requested,
                ..environment
            })?;
            let timing = &engine.registers[0].timing;
            assert_eq!(timing.delay, expected);
            assert_eq!(timing.async_delay, expected);
            assert_eq!(timing.hold, 0.0);
            assert_eq!(timing.min_clock, 0.0);
        }
        for (delay, async_delay, expected) in [
            ("0", "0", [0.0, 0.0]),
            ("0", "1n", [MIN_PROPAGATION_DELAY_S, 1e-9]),
            ("1n", "0", [1e-9, MIN_PROPAGATION_DELAY_S]),
            ("2n", "5n", [2e-9, 5e-9]),
        ] {
            let source = source.replace(
                "CLOCK=in[1]",
                &format!("CLOCK=in[1],DELAY={delay},ASYNCDELAY={async_delay}"),
            );
            let (_, program) = analyze_body(&source)?.into_parts();
            let engine = program
                .context("missing joint-delay fixture")?
                .event_engine(environment)?;
            let timing = &engine.registers[0].timing;
            assert_eq!([timing.delay, timing.async_delay], expected);
        }
        for qualifier in ["DELAY", "HOLD", "MINCLOCK", "ASYNCDELAY"] {
            let source = source.replace("CLOCK=in[1]", &format!("CLOCK=in[1],{qualifier}=-1n"));
            let (_, program) = analyze_body(&source)?.into_parts();
            assert!(
                program
                    .context("missing negative qualifier fixture")?
                    .event_engine(environment)
                    .is_err(),
                "accepted negative explicit {qualifier}"
            );
        }
        Ok(())
    }

    #[test]
    fn model_qualifier_defaults_resolve_only_applicable_registers() -> Result<()> {
        let (_, program) = analyze_body(
            "PORT D in[0]; PORT CK in[1]; PORT Q out[0];
             EDGE (WIDTH=1,DELAY=2n,CLOCK=in[1]) E=0;
             EDGE (WIDTH=1,DELAY=2n,HOLD=USER[0],MINCLOCK=DEVICE[0],CLOCK=in[1]) Overrides=0;
             LEVEL (WIDTH=1,DELAY=2n,CLOCK=in[1]) Latch=0;
             COMB (WIDTH=1,DELAY=2n) C=0;
             E=D; Overrides=D; Latch=D; C=D; Q=E;",
        )?
        .into_parts();
        let environment = TimingEnvironment {
            user: &[0.0],
            device: Some(&[0.0]),
            user_scale: 1.0,
            default_register_delay: 1e-9,
            default_output_delay: 1e-9,
            register_timing_defaults: crate::alb::RegisterTimingDefaults {
                hold: 1e-9,
                min_clock: 0.5e-9,
                setup: 1.5e-9,
            },
        };
        let program = program.context("missing stateful defaults fixture")?;
        let engine = program.event_engine(environment)?;
        let qualifiers = engine
            .registers
            .iter()
            .map(|r| {
                [
                    r.timing.hold.to_bits(),
                    r.timing.min_clock.to_bits(),
                    r.timing.setup.to_bits(),
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(
            qualifiers,
            [
                [1e-9_f64.to_bits(), 0.5e-9_f64.to_bits(), 0],
                [0, 0, 0],
                [0, 0, 1.5e-9_f64.to_bits()],
                [0, 0, 0],
            ]
        );
        let invalid = TimingEnvironment {
            register_timing_defaults: crate::alb::RegisterTimingDefaults {
                hold: 3e-9,
                ..environment.register_timing_defaults
            },
            ..environment
        };
        let engine = program.event_engine(invalid)?;
        assert_eq!(engine.registers[0].timing.hold, 3e-9);
        Ok(())
    }

    #[test]
    fn unknown_inputs_keep_independent_binary_history_across_register_paths() -> Result<()> {
        use AlbLogicLevel::Unknown as X;
        let mut engine = engine(include_str!(
            "../../../../runtime/xspice/fixtures/vendor-unknown-history/unknown-data-clock.net"
        ))?;
        engine.advance(0.0, &[X, L, L])?;
        assert!(engine.output().iter().all(|bit| bit.level == L));
        engine.advance(4e-9, &[H, L, L])?;
        engine.advance(6e-9, &[H, L, X])?;
        engine.advance(7e-9, &[H, H, X])?;
        engine.advance(10e-9, &[H, L, X])?;
        engine.advance(12e-9, &[H, L, H])?;
        engine.flush(13e-9)?;
        assert!(engine.output().iter().all(|bit| bit.level == H));
        // HIGH data remains HIGH during UNKNOWN, likewise the toggle
        // clock and transparent latch enable. Neither raw nor named
        // input reads may acquire a new logic edge or zero candidate.
        assert_eq!(engine.advance(14e-9, &[X, L, H])?, []);
        assert_eq!(engine.advance(18e-9, &[X, H, X])?, []);
        engine.flush(20e-9)?;
        assert!(engine.output().iter().all(|bit| bit.level == H));
        assert_eq!(engine.advance(26e-9, &[X, L, H])?, []);
        engine.flush(28e-9)?;
        assert_eq!(engine.register_words("Rtoggle"), Some([1].as_slice()));
        engine.advance(34e-9, &[L, L, X])?;
        engine.flush(36e-9)?;
        assert_eq!(engine.register_words("Rtrans"), Some([0].as_slice()));
        engine.advance(38e-9, &[L, H, L])?;
        engine.flush(40e-9)?;
        assert_eq!(engine.register_words("Redge"), Some([0].as_slice()));
        assert_eq!(engine.register_words("Rlevel"), Some([0].as_slice()));
        // LOW also persists: UNKNOWN after LOW cannot assert any pin.
        assert_eq!(engine.advance(44e-9, &[X, L, X])?, []);
        assert_eq!(engine.advance(48e-9, &[X, H, L])?, []);
        engine.flush(50e-9)?;
        assert_eq!(engine.register_words("Redge"), Some([0].as_slice()));
        assert_eq!(engine.register_words("Rtoggle"), Some([1].as_slice()));
        Ok(())
    }

    #[test]
    fn unknown_inputs_do_not_trigger_false_timing_violations_or_reset() -> Result<()> {
        use AlbLogicLevel::Unknown as X;
        let mut engine = engine(include_str!(
            "../../../../runtime/xspice/fixtures/vendor-unknown-history/unknown-qualifiers.net"
        ))?;
        engine.advance(0.0, &[H, L, H, L])?;
        engine.advance(6e-9, &[H, L, H, H])?;
        engine.advance(10e-9, &[H, H, H, H])?;
        // Within HOLD and MINCLOCK, before LEVEL closing SETUP.
        assert_eq!(engine.advance(10.3e-9, &[X, X, H, H])?, []);
        engine.advance(10.6e-9, &[X, X, H, L])?;
        engine.flush(12e-9)?;
        assert!(engine.output().iter().all(|bit| bit.level == H));
        assert_eq!(engine.advance(12.1e-9, &[X, H, H, L])?, []);
        engine.advance(14e-9, &[H, L, H, L])?;
        assert_eq!(engine.advance(18e-9, &[H, L, X, L])?, []);
        assert_eq!(engine.flush(20e-9)?, []);
        engine.advance(22e-9, &[H, L, L, L])?;
        engine.flush(23e-9)?;
        assert_eq!(engine.register_words("Rreset"), Some([0].as_slice()));
        // Unknown after asserted active-low reset does not release it.
        engine.advance(28e-9, &[H, L, X, L])?;
        engine.advance(30e-9, &[H, H, X, L])?;
        engine.flush(33e-9)?;
        assert_eq!(engine.register_words("Rreset"), Some([0].as_slice()));
        engine.advance(36e-9, &[H, L, H, L])?;
        engine.advance(40e-9, &[H, H, H, L])?;
        engine.flush(43e-9)?;
        assert_eq!(engine.register_words("Rreset"), Some([1].as_slice()));
        Ok(())
    }

    #[test]
    fn flush_before_first_external_sample_preserves_dc_initialization() -> Result<()> {
        // ngspice can request an event-queue wake before the first physical
        // input callback. An empty queue flush is not an input sample:
        // COMB must still evaluate at DC, and EDGE must not infer a rising
        // clock from its uninitialized low placeholder.
        for (source, initial, following) in [
            (
                "PORT D in[0]; PORT Q out[0]; COMB (WIDTH=1, DELAY=1n) R=0; R=D; Q=R;",
                &[H][..],
                &[L][..],
            ),
            (
                concat!(
                    "PORT D in[0]; PORT Clock in[1]; PORT Q out[0]; ",
                    "EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0; R=D; Q=R;"
                ),
                &[H, H][..],
                &[L, L][..],
            ),
            (
                concat!(
                    "PORT D in[0]; PORT Clock in[1]; PORT Q out[0]; ",
                    "LEVEL (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0; R=D; Q=R;"
                ),
                &[H, H][..],
                &[L, L][..],
            ),
        ] {
            let mut reference = source_engine(source)?;
            let mut preflushed = source_engine(source)?;
            assert_eq!(preflushed.flush(0.0)?, []);
            assert_eq!(preflushed.flush(3e-9)?, []);
            assert!(preflushed.advance(2e-9, initial).is_err());
            let input = initial;
            let expected = reference.advance(3e-9, input)?;
            let actual = preflushed.advance(3e-9, input)?;
            assert_eq!(
                actual, expected,
                "initial output after a timer-only flush: {source}"
            );
            assert_eq!(preflushed.output(), reference.output(), "source: {source}");
            assert_eq!(
                preflushed.register_words("R"),
                reference.register_words("R")
            );
            assert_eq!(preflushed.next_due_time(), reference.next_due_time());
            let expected = reference.flush(6e-9)?;
            let actual = preflushed.flush(6e-9)?;
            assert_eq!(
                actual, expected,
                "no speculative first rising edge: {source}"
            );
            assert_eq!(
                preflushed.advance(8e-9, following)?,
                reference.advance(8e-9, following)?,
            );
            assert_eq!(preflushed.flush(12e-9)?, reference.flush(12e-9)?);
            assert_eq!(preflushed.output(), reference.output());
            assert_eq!(
                preflushed.register_words("R"),
                reference.register_words("R")
            );
        }
        Ok(())
    }

    #[test]
    fn direct_output_pins_without_named_ports_are_driven() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; COMB (WIDTH=1, DELAY=1n) Stage=0; \
             Stage=D; OUT[0]=Stage; OUT[1]=!Stage;",
        )?;
        assert_eq!(runtime.output_width(), 2);
        let initial = runtime.advance(0.0, &[L])?;
        assert_eq!(
            initial.len(),
            2,
            "direct OUT assignments must drive real pins"
        );
        assert_eq!(runtime.output()[0].level, L);
        assert_eq!(runtime.output()[1].level, H);
        runtime.advance(5e-9, &[H])?;
        let transitions = runtime.flush(7e-9)?;
        assert_eq!(transitions.len(), 2);
        assert!(
            transitions
                .iter()
                .all(|event| (event.time_s - 6e-9).abs() < 1e-15)
        );
        assert_eq!(runtime.output()[0].level, H);
        assert_eq!(runtime.output()[1].level, L);

        let mut mixed = source_engine(
            "PORT D in[0]; PORT Q out[0]; COMB (WIDTH=1, DELAY=1n) Stage=0; \
             Stage=D; Q=Stage; OUT[1]=!Stage;",
        )?;
        let initial = mixed.advance(0.0, &[L])?;
        assert_eq!(
            initial.len(),
            2,
            "unnamed output beside named output was lost"
        );
        assert_eq!(mixed.output()[0].level, L);
        assert_eq!(mixed.output()[1].level, H);
        Ok(())
    }

    #[test]
    fn direct_and_named_output_assignments_respect_order_and_enable() -> Result<()> {
        for (assignments, initial) in [
            ("Q=3; OUT[0]=0; OUT[1]=D;", [L, L]),
            ("OUT[0:1]=0; Q=3;", [H, H]),
        ] {
            let mut runtime = source_engine(&format!(
                "PORT D in[0]; PORT Q out[0:1]; \
                 COMB (WIDTH=1, DELAY=0) Stage=0; Stage=D; {assignments}"
            ))?;
            runtime.advance(0.0, &[L])?;
            assert_eq!(runtime.output()[0].level, initial[0]);
            assert_eq!(runtime.output()[1].level, initial[1]);
        }

        let mut enabled = source_engine(
            "PORT D in[0]; PORT Q out[0:1]; \
             COMB (WIDTH=1, DELAY=0) Stage=0; Stage=D; \
             OUT[0:1]=3; Q.EN=2;",
        )?;
        enabled.advance(0.0, &[L])?;
        assert_eq!(enabled.output()[0].level, AlbLogicLevel::Unknown);
        assert_eq!(
            enabled.output()[0].strength,
            AlbDriveStrength::HighImpedance
        );
        assert_eq!(enabled.output()[1].level, H);
        Ok(())
    }

    #[test]
    fn canceled_transitions_do_not_accumulate_before_their_distant_deadlines() -> Result<()> {
        // A long inertial delay must not retain an event per input edge when
        // every edge supersedes the previous candidate. Only live work may
        // remain in the scheduler, however far its deadline is.
        let mut comb = source_engine(
            "PORT D in[0]; PORT Q out[0]; \
             COMB (WIDTH=1, DELAY=1) R=0; R=D; Q=R;",
        )?;
        comb.advance(0.0, &[L])?;
        for pulse in 0..300 {
            let time = f64::from(pulse) * 1e-9;
            comb.advance(time + 0.2e-9, &[H])?;
            comb.advance(time + 0.4e-9, &[L])?;
            assert_eq!(
                comb.events.len(),
                1,
                "superseded COMB event retained at {pulse}"
            );
        }
        assert_eq!(comb.flush(2.0)?, []);
        assert_eq!(comb.register_words("R"), Some([0].as_slice()));

        // Short EDGE clock pulses are canceled by MINCLOCK, without leaving
        // a wakeup for every rejected capture.
        let mut edge = source_engine(
            "PORT D in[0]; PORT Clock in[1]; PORT Q out[0]; \
             EDGE (WIDTH=1, DELAY=1, MINCLOCK=500p, CLOCK=in[1]) R=0; R=D; Q=R;",
        )?;
        edge.advance(0.0, &[H, L])?;
        for pulse in 0..300 {
            let time = f64::from(pulse) * 1e-9;
            edge.advance(time + 0.2e-9, &[H, H])?;
            edge.advance(time + 0.4e-9, &[H, L])?;
            assert!(
                edge.events.is_empty(),
                "rejected EDGE event retained at {pulse}"
            );
            assert_eq!(edge.next_due_time(), None);
        }
        assert_eq!(edge.flush(2.0)?, []);

        // A short asynchronous reset pulse must likewise release its
        // canceled timer rather than retaining it until ASYNCDELAY.
        let mut asynchronous = source_engine(
            "PORT D in[0]; PORT Clock in[1]; PORT Reset in[2]; PORT Q out[0]; \
             EDGE (WIDTH=1, DELAY=1, ASYNCDELAY=500m, CLOCK=in[1]) R=0; \
             R=D; R:=!Reset ? 1; Q=R;",
        )?;
        asynchronous.advance(0.0, &[L, L, H])?;
        for pulse in 0..300 {
            let time = f64::from(pulse) * 1e-9;
            asynchronous.advance(time + 0.2e-9, &[L, L, L])?;
            asynchronous.advance(time + 0.4e-9, &[L, L, H])?;
            assert!(
                asynchronous.events.is_empty(),
                "deasserted asynchronous action retained at {pulse}"
            );
            assert_eq!(asynchronous.next_due_time(), None);
        }
        assert_eq!(asynchronous.flush(2.0)?, []);
        Ok(())
    }

    #[test]
    fn comb_inertial_pulse_rejection_and_later_capture() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Q out[0]; \
             COMB (WIDTH=1, DELAY=1n) R=0; R=D; Q=R;",
        )?;
        runtime.advance(0.0, &[L])?;
        runtime.advance(5e-9, &[H])?;
        runtime.advance(5.25e-9, &[L])?;
        assert_eq!(runtime.flush(8e-9)?, []);
        assert_eq!(runtime.register_words("R"), Some([0].as_slice()));
        runtime.advance(10e-9, &[H])?;
        let changes = runtime.flush(12e-9)?;
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].value.level, H);
        assert!((changes[0].time_s - 11e-9).abs() < 1e-15);
        Ok(())
    }

    #[test]
    fn comb_inertial_cancellation_reschedules_entire_word_when_not_bitwise() -> Result<()> {
        for (bitwise, expected_time) in [(0, 6.2e-9), (1, 6.0e-9)] {
            let mut runtime = source_engine(&format!(
                "PORT D in[0:1]; PORT Q out[0:1]; \
                 COMB (WIDTH=2, DELAY=1n, BITWISE={bitwise}) R=0; R=D; Q=R;"
            ))?;
            runtime.advance(0.0, &[L, L])?;
            runtime.advance(5e-9, &[H, H])?;
            runtime.advance(5.2e-9, &[L, H])?;
            let changes = runtime.flush(8e-9)?;
            assert_eq!(runtime.register_words("R"), Some([2].as_slice()));
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].pin, 1);
            assert_eq!(changes[0].value.level, H);
            assert!((changes[0].time_s - expected_time).abs() < 1e-15);
        }
        Ok(())
    }

    #[test]
    fn comb_edge_level_chain_reconciles_each_internal_transition() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Clock in[1]; PORT Enable in[2]; PORT Q out[0]; \
             COMB (WIDTH=1, DELAY=1n) Stage=0; \
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) Flip=0; \
             LEVEL (WIDTH=1, DELAY=1n, CLOCK=in[2]) Latch=0; \
             Stage=D; Flip=Stage; Latch=Flip; Q=Latch;",
        )?;
        runtime.advance(0.0, &[L, L, L])?;
        runtime.advance(5e-9, &[H, L, L])?;
        runtime.advance(7e-9, &[H, H, H])?;
        let changes = runtime.flush(12e-9)?;
        assert_eq!(runtime.register_words("Stage"), Some([1].as_slice()));
        assert_eq!(runtime.register_words("Flip"), Some([1].as_slice()));
        assert_eq!(runtime.register_words("Latch"), Some([1].as_slice()));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].value.level, H);
        assert!((changes[0].time_s - 9e-9).abs() < 1e-15);
        Ok(())
    }

    #[test]
    fn vendor_combinational_dc_uses_source_order_without_transient_delay() -> Result<()> {
        let mut chain = source_engine(
            "PORT D in[0]; PORT Q out[0]; \
             COMB (WIDTH=1, DELAY=1n) A=0; \
             COMB (WIDTH=1, DELAY=2n) B=0; \
             A=D; B=!A; Q=B;",
        )?;
        let startup = chain.advance(0.0, &[L])?;
        assert_eq!(chain.register_words("A"), Some([0].as_slice()));
        assert_eq!(chain.register_words("B"), Some([1].as_slice()));
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].time_s, 0.0);
        assert_eq!(startup[0].value.level, H);
        assert!(chain.next_due_time().is_none());
        chain.advance(5.1e-9, &[H])?;
        let output = chain.flush(9e-9)?;
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].value.level, L);
        assert!((output[0].time_s - 8.1e-9).abs() < 1e-15);

        let mut cyclic = source_engine(
            "PORT D in[0]; PORT Q out[0]; \
             COMB (WIDTH=1, DELAY=1n) A=0; \
             COMB (WIDTH=1, DELAY=2n) B=0; \
             A=!B; B=A; Q=B;",
        )?;
        let startup = cyclic.advance(0.0, &[L])?;
        assert_eq!(cyclic.register_words("A"), Some([1].as_slice()));
        assert_eq!(cyclic.register_words("B"), Some([1].as_slice()));
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].value.level, H);
        assert!(cyclic.next_due_time().is_none());
        assert_eq!(cyclic.flush(12e-9)?, []);
        Ok(())
    }

    #[test]
    fn comb_feedback_zero_delay_rejected_and_positive_delay_progresses() -> Result<()> {
        let source = |delay| {
            format!(
                "PORT Dummy in[0]; PORT Q out[0]; COMB (WIDTH=1, DELAY={delay}) R=0; R=(!R)^Dummy; Q=R;"
            )
        };
        let mut zero = source_engine(&source("0"))?;
        let startup = zero.advance(0.0, &[L])?;
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].value.level, H);
        assert!(zero.next_due_time().is_none());
        zero.advance(5e-9, &[H])?;
        let failure = zero
            .advance(10e-9, &[L])
            .err()
            .context("stimulated zero-delay COMB oscillator must not spin")?;
        assert!(failure.to_string().contains("zero-delay"), "{failure}");
        let mut delayed = source_engine(&source("1n"))?;
        let startup = delayed.advance(0.0, &[L])?;
        assert_eq!(startup.len(), 1);
        assert_eq!(startup[0].value.level, H);
        assert_eq!(delayed.flush(4.1e-9)?, []);
        assert!(delayed.next_due_time().is_none());
        delayed.advance(5e-9, &[H])?;
        assert_eq!(delayed.flush(9.1e-9)?, []);
        delayed.advance(10e-9, &[L])?;
        let changes = delayed.flush(14.1e-9)?;
        assert_eq!(changes.len(), 4);
        assert_eq!(
            changes
                .iter()
                .map(|change| change.value.level)
                .collect::<Vec<_>>(),
            [L, H, L, H]
        );
        assert!(delayed.next_due_time().is_some());
        Ok(())
    }

    #[test]
    fn dynamic_bitwise_qualifier_requires_a_boolean_value() -> Result<()> {
        let (_, program) = analyze_body(
            "PORT D in[0]; PORT Clock in[1]; PORT Q out[0]; \
             EDGE (WIDTH=1, DELAY=1n, BITWISE=USER[0], CLOCK=in[1]) R=0; \
             R=D; Q=R;",
        )?
        .into_parts();
        let program = program.context("missing register")?;
        let invalid = [0.5];
        let error = program
            .event_engine(TimingEnvironment {
                user: &invalid,
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })
            .err()
            .context("fractional BITWISE should be rejected")?;
        assert!(error.to_string().contains("BITWISE"));
        for bitwise in [0.0, 1.0] {
            let value = [bitwise];
            let runtime = program.event_engine(TimingEnvironment {
                user: &value,
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            })?;
            assert_eq!(runtime.input_width(), 2);
        }
        Ok(())
    }

    #[test]
    fn edge_hold_matches_measured_vendor_bitwise_behavior() -> Result<()> {
        for bitwise in [0, 1] {
            let mut runtime = source_engine(&format!(
                "PORT D in[0:1]; PORT Clock in[2]; PORT Q out[0:1];
                 EDGE (WIDTH=2, DELAY=1n, HOLD=800p, BITWISE={bitwise}, CLOCK=in[2]) R=0;
                 R=D; Q=R;"
            ))?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(5e-9, &[H, H, L])?;
            runtime.advance(10e-9, &[H, H, H])?;
            runtime.advance(10.4e-9, &[L, H, H])?;
            let changes = runtime.flush(12e-9)?;
            assert_eq!(runtime.register_words("R"), Some([2].as_slice()));
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].pin, 1);
            assert_eq!(changes[0].value.level, H);
        }
        Ok(())
    }

    #[test]
    fn level_setup_uses_bitwise_violation_granularity() -> Result<()> {
        for (bitwise, expected) in [(1, 2_u32), (0, 0_u32)] {
            let mut runtime = source_engine(&format!(
                "PORT D in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
                 LEVEL (WIDTH=2, DELAY=1n, SETUP=800p, BITWISE={bitwise}, CLOCK=in[2]) R=0;
                 R=D; Q=R;"
            ))?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(18e-9, &[L, H, L])?;
            runtime.advance(19.3e-9, &[H, H, H])?;
            runtime.advance(19.4e-9, &[L, H, H])?;
            runtime.advance(19.9e-9, &[L, H, L])?;
            let changes = runtime.flush(21e-9)?;
            assert_eq!(runtime.register_words("R"), Some([expected].as_slice()));
            assert_eq!(changes.len(), usize::from(bitwise == 1));
            if bitwise == 1 {
                assert_eq!(changes[0].pin, 1);
                assert_eq!(changes[0].value.level, H);
            }
        }
        Ok(())
    }

    #[test]
    fn level_transparency_reschedules_pending_word_by_bitwise_granularity() -> Result<()> {
        // Original Elements 9.40d LEVEL boundary sweep: changing D0 while
        // transparent delays an unchanged D1 bit when BITWISE=0, but not
        // when BITWISE=1. The closing edge is outside the SETUP guard.
        for (bitwise, first_due) in [(0, 12.94e-9), (1, 12e-9)] {
            let mut runtime = source_engine(&format!(
                "PORT D in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
                 LEVEL (WIDTH=2, DELAY=2n, SETUP=800p, BITWISE={bitwise}, CLOCK=in[2]) R=0;
                 R=D; Q=R;"
            ))?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(5.1e-9, &[H, H, L])?;
            runtime.advance(10e-9, &[H, H, H])?;
            assert!(
                (runtime
                    .next_due_time()
                    .context("missing pending LEVEL event")?
                    - 12e-9)
                    .abs()
                    < 1e-15
            );
            runtime.advance(10.94e-9, &[L, H, H])?;
            assert!(
                (runtime
                    .next_due_time()
                    .context("missing pending LEVEL event")?
                    - first_due)
                    .abs()
                    < 1e-15
            );
            runtime.advance(11.8e-9, &[L, H, L])?;
            let early = runtime.flush(12.5e-9)?;
            assert_eq!(
                runtime.register_words("R"),
                Some([if bitwise == 0 { 0 } else { 2 }].as_slice())
            );
            assert_eq!(early.len(), usize::from(bitwise == 1));
            if bitwise == 1 {
                assert_eq!(early[0].pin, 1);
                assert!((early[0].time_s - 12e-9).abs() < 1e-15);
            }
            let late = runtime.flush(13.5e-9)?;
            assert_eq!(runtime.register_words("R"), Some([2].as_slice()));
            if bitwise == 0 {
                assert_eq!(late.len(), 1);
                assert_eq!(late[0].pin, 1);
                assert!((late[0].time_s - 12.94e-9).abs() < 1e-15);
            }
            assert!(runtime.next_due_time().is_none());
        }
        Ok(())
    }

    #[test]
    fn closed_level_input_changes_preserve_pending_capture() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
             LEVEL (WIDTH=2, DELAY=2n, BITWISE=0, CLOCK=in[2]) R=0;
             R=D; Q=R;",
        )?;
        runtime.advance(0.0, &[L, L, L])?;
        runtime.advance(5e-9, &[H, H, L])?;
        runtime.advance(10e-9, &[H, H, H])?;
        runtime.advance(11e-9, &[H, H, L])?;
        // Closing the latch freezes its captured value. Subsequent D0
        // changes while disabled must not cancel the still-pending commit.
        runtime.advance(11.2e-9, &[L, H, L])?;
        runtime.advance(11.4e-9, &[H, H, L])?;
        assert!(
            (runtime
                .next_due_time()
                .context("missing closed LEVEL commit")?
                - 12e-9)
                .abs()
                < 1e-15
        );
        let events = runtime.flush(12.5e-9)?;
        assert_eq!(runtime.register_words("R"), Some([3].as_slice()));
        assert_eq!(
            events.iter().map(|event| event.pin).collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(
            events
                .iter()
                .all(|event| (event.time_s - 12e-9).abs() < 1e-15)
        );
        assert!(runtime.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn indexed_edge_arrays_match_vendor_per_bit_hold_for_both_modes() -> Result<()> {
        let fixture = include_str!("../../../../runtime/xspice/fixtures/stateful-array.alb");
        for bitwise in [0, 1] {
            let source = fixture.replace("BITWISE=1", &format!("BITWISE={bitwise}"));
            let mut runtime = source_engine(&source)?;
            assert_eq!(runtime.input_width(), 3);
            assert_eq!(runtime.output_width(), 4);
            assert_eq!(runtime.register_words("R"), Some([0, 3].as_slice()));
            let initial = runtime.advance(0.0, &[L, L, L])?;
            assert_eq!(initial.len(), 4);
            assert_eq!(
                runtime
                    .output()
                    .iter()
                    .map(|bit| bit.level)
                    .collect::<Vec<_>>(),
                [L, L, H, H]
            );
            runtime.advance(5e-9, &[H, H, L])?;
            runtime.advance(10e-9, &[H, H, H])?;
            // Only Data[0] moves during the 800 ps hold window.
            runtime.advance(10.4e-9, &[L, H, H])?;
            let events = runtime.flush(12e-9)?;
            assert_eq!(runtime.register_words("R"), Some([2, 1].as_slice()));
            assert_eq!(
                events.iter().map(|event| event.pin).collect::<Vec<_>>(),
                [1, 3]
            );
            assert!(
                events
                    .iter()
                    .all(|event| (event.time_s - 11e-9).abs() < 1e-15)
            );
            // A later clean edge must capture every word regardless of
            // the earlier per-bit HOLD cancellation.
            runtime.advance(15e-9, &[L, H, L])?;
            runtime.advance(20e-9, &[L, L, L])?;
            runtime.advance(24e-9, &[L, L, H])?;
            runtime.flush(27e-9)?;
            assert_eq!(runtime.register_words("R"), Some([0, 3].as_slice()));
            assert!(runtime.next_due_time().is_none());
        }
        Ok(())
    }

    #[test]
    fn vendor_edge_and_level_bitwise_controls_match_original_source() -> Result<()> {
        // Independently executed in real Elements 9.40d, 20 ps max step.
        // EDGE BITWISE=0 unexpectedly still rejects HOLD violations per bit
        // for both scalar registers and indexed arrays. LEVEL BITWISE=0
        // rejects the entire word for a SETUP violation, as documented.
        for (source, expected) in [
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/array-hold-bitwise-0.net"
                ),
                vec![2, 1],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/array-hold-bitwise-1.net"
                ),
                vec![2, 1],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/array-zero-hold.net"
                ),
                vec![3, 0],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/scalar-bitwise-0.net"
                ),
                vec![2],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/scalar-bitwise-1.net"
                ),
                vec![2],
            ),
        ] {
            let mut runtime = engine(source)?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(5.1e-9, &[H, H, L])?;
            runtime.advance(10e-9, &[H, H, H])?;
            runtime.advance(10.4e-9, &[L, H, H])?;
            runtime.flush(14e-9)?;
            assert_eq!(runtime.register_words("R"), Some(expected.as_slice()));
        }
        for (source, expected) in [
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/level-setup-bitwise-0.net"
                ),
                0_u32,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-bitwise/level-setup-bitwise-1.net"
                ),
                2_u32,
            ),
        ] {
            let mut runtime = engine(source)?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(5.1e-9, &[H, H, L])?;
            runtime.advance(10e-9, &[H, H, H])?;
            runtime.advance(11.4e-9, &[L, H, H])?;
            runtime.advance(11.8e-9, &[L, H, L])?;
            runtime.flush(15e-9)?;
            assert_eq!(runtime.register_words("R"), Some([expected].as_slice()));
        }
        Ok(())
    }

    #[test]
    fn transparent_register_cascade_propagates_internal_events() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Enable in[1]; PORT Q out[0]; \
             LEVEL (WIDTH=1, DELAY=1n, CLOCK=in[1]) First=0; \
             LEVEL (WIDTH=1, DELAY=1n, CLOCK=in[1]) Second=0; \
             First=D; Second=First; Q=Second;",
        )?;
        runtime.advance(0.0, &[L, L])?;
        runtime.advance(5e-9, &[H, L])?;
        runtime.advance(10e-9, &[H, H])?;
        assert!(
            (runtime
                .next_due_time()
                .context("missing first stage update")?
                - 11e-9)
                .abs()
                < 1e-15
        );
        let changes = runtime.flush(13e-9)?;
        assert_eq!(runtime.register_words("First"), Some([1].as_slice()));
        assert_eq!(runtime.register_words("Second"), Some([1].as_slice()));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].value.level, H);
        assert!((changes[0].time_s - 12e-9).abs() < 1e-15);
        Ok(())
    }

    #[test]
    fn zero_delay_acyclic_latch_chain_settles_in_same_timestamp() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Enable in[1]; PORT Q out[0]; \
             LEVEL (WIDTH=1, DELAY=0, CLOCK=in[1]) A=0; \
             LEVEL (WIDTH=1, DELAY=0, CLOCK=in[1]) B=0; \
             A=D; B=A; Q=B;",
        )?;
        runtime.advance(0.0, &[L, L])?;
        runtime.advance(5e-9, &[H, L])?;
        let events = runtime.advance(10e-9, &[H, H])?;
        assert_eq!(runtime.register_words("A"), Some([1].as_slice()));
        assert_eq!(runtime.register_words("B"), Some([1].as_slice()));
        assert!(runtime.next_due_time().is_none());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value.level, H);
        assert_eq!(events[0].time_s, 10e-9);
        Ok(())
    }

    #[test]
    fn zero_delay_oscillating_latch_loop_returns_error() -> Result<()> {
        let mut runtime = source_engine(
            "PORT Enable in[0]; PORT Q out[0]; \
             LEVEL (WIDTH=1, DELAY=0, CLOCK=in[0]) R=0; \
             R=!R; Q=R;",
        )?;
        runtime.advance(0.0, &[L])?;
        let failure = runtime
            .advance(10e-9, &[H])
            .err()
            .context("feedback loop should be rejected, not hang")?;
        assert!(failure.to_string().contains("zero-delay"), "{failure}");
        Ok(())
    }

    #[test]
    fn concurrent_clock_and_async_assertion_has_deterministic_priority() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Clock in[1]; PORT Reset in[2]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=600p, CLOCK=in[1]) R=0;
             R=D; R:=!Reset ? 0; Q=R;",
        )?;
        runtime.advance(0.0, &[H, L, H])?;
        runtime.advance(10e-9, &[H, H, L])?;
        assert!((runtime.next_due_time().context("missing reset event")? - 10.6e-9).abs() < 1e-15);
        runtime.advance(10.3e-9, &[H, H, H])?;
        assert!(runtime.next_due_time().is_none());
        assert_eq!(runtime.flush(12e-9)?, []);
        assert_eq!(runtime.register_words("R"), Some([0].as_slice()));
        runtime.advance(15e-9, &[H, L, H])?;
        runtime.advance(20e-9, &[H, H, H])?;
        let changes = runtime.flush(22e-9)?;
        assert_eq!(runtime.register_words("R"), Some([1].as_slice()));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].value.level, H);
        Ok(())
    }

    #[test]
    fn vendor_clock_and_async_release_ordering_at_same_timestamp() -> Result<()> {
        // The refined Elements 9.40d release-on-clock 1 ps oracle captures
        // at equal 2.2 V ADC threshold times. Release 2 ps later
        // does not capture until the next rising clock.
        for (release, first_capture) in [
            (-20_i32, true),
            (-2, true),
            (0, true),
            (2, false),
            (5, false),
            (20, false),
        ] {
            let mut runtime = source_engine(
                "PORT Data in[0]; PORT Clock in[1]; PORT Reset in[2]; PORT Q out[0];
                 EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=200p, CLOCK=in[1]) Flip=0;
                 Flip := !Reset ? 0; Flip=Data; Q=Flip;",
            )?;
            runtime.advance(0.0, &[H, L, L])?;
            if release < 0 {
                runtime.advance(f64::from(release).mul_add(1e-12, 10e-9), &[H, L, H])?;
            }
            runtime.advance(10e-9, &[H, H, if release <= 0 { H } else { L }])?;
            if release > 0 {
                runtime.advance(f64::from(release).mul_add(1e-12, 10e-9), &[H, H, H])?;
            }
            let early = runtime.flush(16e-9)?;
            assert_eq!(
                runtime.register_words("Flip"),
                Some([u32::from(first_capture)].as_slice()),
                "release offset {release} ps",
            );
            assert_eq!(early.len(), usize::from(first_capture));
            if first_capture {
                assert_eq!(early[0].pin, 0);
                assert_eq!(early[0].value.level, H);
            }
            runtime.advance(18e-9, &[H, L, H])?;
            runtime.advance(20e-9, &[H, H, H])?;
            let late = runtime.flush(22e-9)?;
            assert_eq!(runtime.register_words("Flip"), Some([1].as_slice()));
            assert_eq!(late.len(), usize::from(!first_capture));
        }
        Ok(())
    }

    #[test]
    fn rounded_timer_dispatch_precedes_boundary_input_without_future_timestamps() -> Result<()> {
        let mut runtime = source_engine(
            "PORT A in[0]; PORT B in[1]; PORT Q out[0]; PORT Z out[1];
             COMB(WIDTH=1,DELAY=1n) R=0; R=A; Q=R; Z=B;",
        )?;
        runtime.advance(0.0, &[L, L])?;
        runtime.advance(10e-9, &[H, L])?;
        let due = runtime.next_due_time().context("missing register timer")?;
        let boundary = f64::from_bits(due.to_bits() - 1);
        let transitions = runtime.advance(boundary, &[H, H])?;
        assert_eq!(transitions.len(), 2);
        assert_eq!(
            transitions
                .iter()
                .map(|event| event.pin)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(
            transitions
                .iter()
                .all(|event| event.time_s.to_bits() == boundary.to_bits()),
            "{transitions:?}"
        );
        assert!(runtime.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn rounded_boundary_reconciles_zero_delay_cascade_before_external_sample() -> Result<()> {
        let mut runtime = source_engine(
            "PORT A in[0]; PORT B in[1]; PORT Q out[0]; PORT Z out[1];
             COMB(WIDTH=1,DELAY=1n) R=0; COMB(WIDTH=1,DELAY=0) S=0;
             R=A; S=R; Q=R; Z=S & B;",
        )?;
        runtime.advance(0.0, &[L, L])?;
        runtime.advance(10e-9, &[H, L])?;
        let due = runtime.next_due_time().context("missing register timer")?;
        let boundary = f64::from_bits(due.to_bits() - 1);
        let transitions = runtime.advance(boundary, &[H, H])?;
        assert_eq!(transitions.len(), 2);
        assert_eq!(runtime.register_words("S"), Some([1].as_slice()));
        assert!(
            transitions.iter().enumerate().all(
                |(pin, event)| event.pin == pin && event.time_s.to_bits() == boundary.to_bits()
            )
        );
        assert!(runtime.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn representably_positive_feedback_delay_advances_instead_of_collapsing() -> Result<()> {
        let mut runtime = source_engine(
            "PORT D in[0]; PORT Q out[0];
             COMB(WIDTH=1,DELAY=2e-24) R=0; R=D ? !R : 0; Q=R;",
        )?;
        runtime.advance(0.0, &[L])?;
        let start = 10e-9;
        assert_eq!(runtime.advance(start, &[H])?, []);
        assert_eq!(runtime.flush(start)?, []);
        let mut previous = start;
        for level in [H, L, H, L] {
            let due = runtime
                .next_due_time()
                .context("missing positive feedback deadline")?;
            assert!(due > previous);
            let events = runtime.flush(due)?;
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].time_s.to_bits(), due.to_bits());
            assert_eq!(events[0].value.level, level);
            previous = due;
        }
        Ok(())
    }

    #[test]
    fn separately_rounded_native_adc_clock_and_internal_timer_share_the_vendor_instant()
    -> Result<()> {
        // Actual 20-digit callbacks from the real ngspice47 native
        // original-source circuit. ResetPipe's scheduled release is one ULP
        // after Clock's analog ADC callback despite identical physical
        // threshold timing. The proprietary Elements 9.40d source captures
        // at equality, while an actual 2ps-early clock must not.
        let source = include_str!(
            "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-equal.net"
        );
        let reset_callback = f64::from_bits(0x3e45_5b98_b0c0_89fa);
        let equal_clock_callback = f64::from_bits(0x3e47_815a_2dc5_64cc);
        for (clock_offset_ps, should_capture) in [(-2, false), (0, true), (2, true)] {
            let mut runtime = engine(source)?;
            runtime.advance(0.0, &[L, L, L])?;
            runtime.advance(5e-9, &[H, L, L])?;
            runtime.advance(reset_callback, &[H, L, H])?;
            let scheduled_release = runtime
                .next_due_time()
                .context("missing internally scheduled COMB reset release")?;
            assert!(
                scheduled_release > equal_clock_callback
                    && scheduled_release - equal_clock_callback
                        <= f64::EPSILON * equal_clock_callback,
                "recorded independent callback rounding changed"
            );
            let clock = f64::from(clock_offset_ps).mul_add(1e-12, equal_clock_callback);
            runtime.advance(clock, &[H, H, H])?;
            runtime.flush(13e-9)?;
            assert_eq!(
                runtime.register_words("Flip"),
                Some([u32::from(should_capture)].as_slice()),
                "clock offset {clock_offset_ps}ps must match original vendor"
            );
        }
        Ok(())
    }

    #[test]
    fn original_vendor_internal_async_release_on_simultaneous_clock() -> Result<()> {
        // Original Elements 9.40d 1ps native/physical sweep. Reset enters a
        // COMB pipeline 1ns before the rising EDGE clock, so its delayed
        // asynchronous release may fall 10ps/2ps before, at exactly the same
        // instant, or 2ps/10ps after the clock. Unlike a direct Reset input,
        // the pending release must be reconciled by an internal timer.
        let cases = [
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-minus-10ps.net"
                ),
                -10_i32,
                false,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-minus-2ps.net"
                ),
                -2,
                false,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-equal.net"
                ),
                0,
                true,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-plus-2ps.net"
                ),
                2,
                true,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-plus-10ps.net"
                ),
                10,
                true,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-early-control.net"
                ),
                0,
                true,
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-release/release-clock-late-control.net"
                ),
                0,
                false,
            ),
        ];
        for (source, clock_offset_ps, captures_first_clock) in cases {
            verify_original_vendor_internal_release_case(
                source,
                clock_offset_ps,
                captures_first_clock,
            )?;
        }
        Ok(())
    }

    fn verify_original_vendor_internal_release_case(
        source: &str,
        clock_offset_ps: i32,
        captures_first_clock: bool,
    ) -> Result<()> {
        let reset_release = 9.944e-9;
        let internal_release = reset_release + 1e-9;
        let second_clock = 29.944e-9;
        let mut runtime = engine(source)?;
        runtime.advance(0.0, &[L, L, L])?;
        runtime.advance(5.044e-9, &[H, L, L])?;
        runtime.advance(reset_release, &[H, L, H])?;
        let first_clock = f64::from(clock_offset_ps).mul_add(1e-12, internal_release);
        // A timer-only wakeup and an implicit advance() drain must give the
        // same output at the exact source-derived internal clock boundary.
        let separately_flushed = (first_clock >= internal_release).then(|| runtime.clone());
        let mut transitions = runtime.advance(first_clock, &[H, H, H])?;
        transitions.extend(runtime.advance(12.5e-9, &[H, L, H])?);
        transitions.extend(runtime.advance(second_clock, &[H, H, H])?);
        transitions.extend(runtime.advance(31e-9, &[H, L, H])?);
        transitions.extend(runtime.flush(33e-9)?);
        let actual = transitions
            .iter()
            .filter(|event| event.pin == 0 && event.time_s > 1e-12)
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), 1, "source: {source}");
        let expected_time = if captures_first_clock {
            first_clock + 1e-9
        } else {
            second_clock + 1e-9
        };
        let actual_time = actual[0].time_s;
        assert!(
            (actual_time - expected_time).abs() < 1e-18,
            "wrong capture instant for source {source}: {actual_time} vs {expected_time}"
        );
        assert_eq!(actual[0].value.level, H);
        if let Some(mut preflushed) = separately_flushed {
            let mut other = preflushed.flush(internal_release)?;
            other.extend(preflushed.advance(first_clock, &[H, H, H])?);
            other.extend(preflushed.advance(12.5e-9, &[H, L, H])?);
            other.extend(preflushed.advance(second_clock, &[H, H, H])?);
            other.extend(preflushed.advance(31e-9, &[H, L, H])?);
            other.extend(preflushed.flush(33e-9)?);
            assert_eq!(
                other, transitions,
                "separate internal-timer wakeup changes source ordering: {source}"
            );
            assert_eq!(preflushed.output(), runtime.output());
            assert_eq!(
                preflushed.register_words("Flip"),
                runtime.register_words("Flip")
            );
            assert_eq!(preflushed.next_due_time(), runtime.next_due_time());
        }
        assert_eq!(runtime.register_words("Flip"), Some([1].as_slice()));
        assert_eq!(runtime.register_words("ResetPipe"), Some([1].as_slice()));
        Ok(())
    }

    #[test]
    fn original_vendor_delayed_comb_to_async_edge_reconciliation() -> Result<()> {
        // Four independently executed Elements 9.40d source programs, including
        // measured analog Q crossings and a zero-versus-1ns COMB delay control.
        // Logical times below are the ideal internal events, not analog DAC
        // midpoint crossings. The native differential suite checks the latter.
        let normal = &[
            (0.0, &[L, L, H][..]),
            (5.05, &[H, L, H]),
            (10.0, &[H, H, H]),
            (12.1, &[H, L, H]),
            (15.0, &[H, L, L]),
            (24.0, &[H, L, H]),
            (30.0, &[H, H, H]),
            (32.1, &[H, L, H]),
        ];
        let short = &[
            (0.0, &[L, L, H][..]),
            (5.05, &[H, L, H]),
            (10.0, &[H, H, H]),
            (12.1, &[H, L, H]),
            (15.0, &[H, L, L]),
            (15.2, &[H, L, H]),
            (30.0, &[H, H, H]),
            (32.1, &[H, L, H]),
        ];
        let early = &[
            (0.0, &[L, L, H][..]),
            (5.05, &[H, L, H]),
            (10.0, &[H, H, H]),
            (10.4, &[H, H, L]),
            (12.1, &[H, L, L]),
            (24.0, &[H, L, H]),
            (30.0, &[H, H, H]),
            (32.1, &[H, L, H]),
        ];
        for (source, samples, expected) in [
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-async/comb-async-assert.net"
                ),
                normal.as_slice(),
                &[(12.0, H), (16.3, L), (32.0, H)][..],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-async/comb-async-pulse-reject.net"
                ),
                short.as_slice(),
                &[(12.0, H)][..],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-async/comb-async-before-edge-commit.net"
                ),
                early.as_slice(),
                &[(32.0, H)][..],
            ),
            (
                include_str!(
                    "../../../../runtime/xspice/fixtures/vendor-internal-async/comb-async-zero-delay.net"
                ),
                normal.as_slice(),
                &[(12.0, H), (15.3, L), (32.0, H)][..],
            ),
        ] {
            verify(&events(source, samples)?, expected);
        }
        Ok(())
    }

    #[test]
    fn vendor_edge_capture_and_falling_edge_ignored() -> Result<()> {
        let deck =
            include_str!("../../../../runtime/xspice/fixtures/vendor-stateful/edge-basic.net");
        let events = events(
            deck,
            &[
                (0.0, &[L, L]),
                (5.05, &[H, L]),
                (9.95, &[H, H]),
                (13.05, &[H, L]),
                (15.05, &[L, L]),
                (19.95, &[L, H]),
                (23.05, &[L, L]),
                (25.05, &[H, L]),
                (29.95, &[H, H]),
                (33.05, &[H, L]),
                (35.05, &[L, L]),
                (39.95, &[L, H]),
                (43.05, &[L, L]),
            ],
        )?;
        verify(&events, &[(10.70, H), (20.70, L), (30.70, H), (40.70, L)]);
        Ok(())
    }

    #[test]
    fn vendor_level_transparency_and_closed_hold() -> Result<()> {
        let deck =
            include_str!("../../../../runtime/xspice/fixtures/vendor-stateful/level-basic.net");
        let events = events(
            deck,
            &[
                (0.0, &[L, L]),
                (5.05, &[H, L]),
                (9.95, &[H, H]),
                (14.05, &[L, H]),
                (19.95, &[L, L]),
                (24.05, &[H, L]),
                (29.95, &[H, H]),
                (34.05, &[L, H]),
                (39.95, &[L, L]),
                (44.05, &[H, L]),
            ],
        )?;
        verify(&events, &[(10.70, H), (14.80, L), (30.70, H), (34.80, L)]);
        Ok(())
    }

    #[test]
    fn vendor_minclock_and_hold_cancel_independently() -> Result<()> {
        let deck = include_str!(
            "../../../../runtime/xspice/fixtures/vendor-stateful/edge-hold-minclock.net"
        );
        let events = events(
            deck,
            &[
                (0.0, &[L, L]),
                (2.05, &[H, L]),
                (5.01, &[H, H]),
                (5.41, &[H, L]),
                (14.01, &[H, H]),
                (14.15, &[L, H]),
                (17.01, &[L, L]),
                (20.05, &[H, L]),
                (24.01, &[H, H]),
                (27.01, &[H, L]),
                (32.05, &[L, L]),
                (34.01, &[L, H]),
                (37.01, &[L, L]),
            ],
        )?;
        verify(&events, &[(25.01, H), (35.01, L)]);
        Ok(())
    }

    #[test]
    fn vendor_setup_rejects_preclose_data_change() -> Result<()> {
        let deck =
            include_str!("../../../../runtime/xspice/fixtures/vendor-stateful/level-setup.net");
        let events = events(
            deck,
            &[
                (0.0, &[L, L]),
                (5.05, &[H, L]),
                (9.95, &[H, H]),
                (19.45, &[L, H]),
                (19.95, &[L, L]),
                (26.05, &[H, L]),
                (29.95, &[H, H]),
                (35.05, &[L, H]),
                (39.95, &[L, L]),
            ],
        )?;
        verify(&events, &[(11.95, H), (37.05, L)]);
        Ok(())
    }

    #[test]
    fn asynchronous_short_pulse_does_not_commit_stale_reset() -> Result<()> {
        let (_, program) = analyze_body(
            "PORT D in[0]; PORT CLK in[1]; PORT RESET in[2];              PORT Q out[0];              EDGE (WIDTH=1, DELAY=1n, ASYNCDELAY=600p, CLOCK=in[1]) R=1;              R := !RESET ? 0; R=D; Q=R;"
        )?.into_parts();
        let mut engine =
            program
                .context("missing EDGE source")?
                .event_engine(TimingEnvironment {
                    user: &[],
                    device: None,
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 1e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                })?;
        engine.advance(0.0, &[H, L, H])?;
        engine.advance(5e-9, &[H, L, L])?;
        engine.advance(5.2e-9, &[H, L, H])?;
        assert_eq!(engine.flush(6e-9)?, []);
        assert_eq!(engine.output()[0].level, H);
        engine.advance(7e-9, &[H, L, L])?;
        let result = engine.flush(7.7e-9)?;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].value.level, L);
        Ok(())
    }

    #[test]
    fn vendor_async_reset_ignores_low_clock() -> Result<()> {
        let deck = include_str!(
            "../../../../runtime/xspice/fixtures/vendor-stateful/edge-async-reset.net"
        );
        let events = events(
            deck,
            &[
                (0.0, &[L, L, H]),
                (5.05, &[H, L, H]),
                (9.95, &[H, H, H]),
                (12.05, &[H, L, H]),
                (19.95, &[H, L, L]),
                (23.05, &[H, L, H]),
                (29.95, &[H, H, H]),
                (32.05, &[H, L, H]),
                (35.05, &[L, L, H]),
            ],
        )?;
        verify(&events, &[(10.95, H), (20.15, L), (30.95, H)]);
        Ok(())
    }
}
