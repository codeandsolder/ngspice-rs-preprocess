use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

mod family;
pub use family::{
    AnalogBoundaryPin, AnalogBridgeKind, BridgeCatalog, BridgeModel, DigitalFamilyResolution,
    DigitalPinDirection, LegacyCompatibilityTable, LogicFamily, LogicFamilyAssignment,
    PlannedAnalogBridge, plan_analog_bridges,
};
mod native_family;
pub use native_family::{
    NativeAdcParameters, NativeDacParameters, NativeDigitalBundle, NativeFamilyModel,
    NativeFamilyParameters, NativeFamilySpecification, NativePinWiring,
    render_native_family_bridges,
};
mod topology;
pub use topology::{
    AnalogDigitalConnection, DiscoveredAlbPin, ScopedAnalogDigitalConnection, derive_family_wiring,
    discover_alb_pins, materialize_scoped_family_bridges,
};
mod xspice;
pub use xspice::{XspiceElectrical, XspiceLutTarget, XspiceTimedBit, XspiceTimedPlan};

const MAX_TRUTH_TABLE_INPUTS: usize = 16;
const MAX_VALUE_BITS: usize = 32;
const MAX_KNOWN_LUT_CELLS: usize = 4 * 1024 * 1024;

/// Three-valued `SIMetrix` logic level used by ALB runtime evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlbLogicLevel {
    /// Known logic zero.
    Zero,
    /// Known logic one.
    One,
    /// Unknown level that may be zero or one.
    Unknown,
}

/// Drive-strength subset produced by an ALB output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlbDriveStrength {
    /// Actively driven output.
    Strong,
    /// Disabled output.
    HighImpedance,
    /// Output enable is itself unknown.
    Undetermined,
}

/// One ALB output bit after ternary functional evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlbDigitalBit {
    /// Resolved logic level.
    pub level: AlbLogicLevel,
    /// Resolved drive strength.
    pub strength: AlbDriveStrength,
}

fn row_matches_ternary(row: usize, inputs: &[AlbLogicLevel]) -> bool {
    inputs.iter().enumerate().all(|(bit, input)| match input {
        AlbLogicLevel::Zero => row & (1usize << bit) == 0,
        AlbLogicLevel::One => row & (1usize << bit) != 0,
        AlbLogicLevel::Unknown => true,
    })
}

fn join_logic_level(current: Option<AlbLogicLevel>, next: AlbLogicLevel) -> AlbLogicLevel {
    current.map_or(next, |current| {
        if current == next {
            current
        } else {
            AlbLogicLevel::Unknown
        }
    })
}

fn join_drive_strength(
    current: Option<AlbDriveStrength>,
    next: AlbDriveStrength,
) -> AlbDriveStrength {
    current.map_or(next, |current| {
        if current == next {
            current
        } else {
            AlbDriveStrength::Undetermined
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnownTruthTable {
    input_width: usize,
    output_width: usize,
    rows: Vec<u32>,
    delayed_output_bits: usize,
}

impl KnownTruthTable {
    #[must_use]
    pub const fn input_width(&self) -> usize {
        self.input_width
    }

    #[must_use]
    pub const fn output_width(&self) -> usize {
        self.output_width
    }

    #[cfg(test)]
    #[must_use]
    pub fn rows(&self) -> &[u32] {
        &self.rows
    }

    #[must_use]
    pub const fn delayed_output_bits(&self) -> usize {
        self.delayed_output_bits
    }

    /// Compute the Boolean-consensus envelope over all concrete input assignments.
    ///
    /// This preserves correlations within a single truth table, but it is not
    /// independently verified as `SIMetrix` ALB's UNKNOWN-expression semantics.
    /// Do not treat this result as proof of exact simulator compatibility.
    ///
    /// # Errors
    /// Returns an error if the input width does not match the table or no row matches.
    pub fn evaluate_possible_levels(&self, inputs: &[AlbLogicLevel]) -> Result<Vec<AlbLogicLevel>> {
        if inputs.len() != self.input_width {
            bail!(
                "SIMetrix ALB ternary evaluation expected {} inputs, got {}",
                self.input_width,
                inputs.len()
            );
        }
        let mut levels = vec![None; self.output_width];
        for (row_index, row) in self.rows.iter().copied().enumerate() {
            if !row_matches_ternary(row_index, inputs) {
                continue;
            }
            for (output, level) in levels.iter_mut().enumerate() {
                let next = if row & (1_u32 << output) == 0 {
                    AlbLogicLevel::Zero
                } else {
                    AlbLogicLevel::One
                };
                *level = Some(join_logic_level(*level, next));
            }
        }
        levels
            .into_iter()
            .map(|level| level.context("SIMetrix ALB ternary evaluation matched no binary rows"))
            .collect()
    }

    /// Encode the known-input truth table in ngspice `d_genlut` output-major order.
    #[must_use]
    pub fn d_genlut_values(&self) -> String {
        let mut values = String::with_capacity(self.rows.len().saturating_mul(self.output_width));
        for output in 0..self.output_width {
            for row in &self.rows {
                values.push(if row & (1_u32 << output) == 0 {
                    '0'
                } else {
                    '1'
                });
            }
        }
        values
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateRegisterKind {
    Combinational,
    Edge,
    Level,
}

#[derive(Clone, Debug, PartialEq)]
enum DynamicValue {
    Literal(f64),
    User(usize),
    Device(usize),
}

impl DynamicValue {
    const fn literal(&self) -> Option<f64> {
        match self {
            Self::Literal(value) => Some(*value),
            Self::User(_) | Self::Device(_) => None,
        }
    }
}

#[derive(Clone, Debug)]
struct StateRegister {
    kind: StateRegisterKind,
    name: String,
    width: DynamicValue,
    array_size: usize,
    clock_pin: Option<usize>,
    delay: Option<DynamicValue>,
    min_clock: Option<DynamicValue>,
    hold: Option<DynamicValue>,
    setup: Option<DynamicValue>,
    async_delay: Option<DynamicValue>,
    bitwise: Option<DynamicValue>,
    initial: Vec<Expr>,
}

#[derive(Clone, Debug)]
enum StateAssignmentTarget {
    Register(String),
    RegisterIndex { name: String, index: Expr },
    Output(String),
    OutputRange { first: usize, last: usize },
    OutputEnable(String),
    Local(String),
}

#[derive(Clone, Debug)]
enum StateAssignmentValue {
    Synchronous(Expr),
    Asynchronous { condition: Expr, value: Expr },
}

#[derive(Clone, Debug)]
struct StateAssignment {
    target: StateAssignmentTarget,
    value: StateAssignmentValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CombinationalDelayMode {
    Bitwise,
    WholeRegister,
    Dynamic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CombinationalTimingShape {
    feedback: bool,
    delay_mode: CombinationalDelayMode,
}

impl CombinationalTimingShape {
    pub const fn has_feedback(self) -> bool {
        self.feedback
    }

    pub const fn delay_mode(self) -> CombinationalDelayMode {
        self.delay_mode
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TimingEnvironment<'a> {
    pub user: &'a [f64],
    pub device: Option<&'a [f64]>,
    pub user_scale: f64,
    pub default_register_delay: f64,
    pub default_output_delay: f64,
}

#[derive(Clone, Debug)]
pub struct ResolvedCombinationalTiming {
    register_delays: BTreeMap<String, f64>,
    output_delays: BTreeMap<String, f64>,
}

impl ResolvedCombinationalTiming {
    pub const fn register_delays(&self) -> &BTreeMap<String, f64> {
        &self.register_delays
    }

    pub const fn output_delays(&self) -> &BTreeMap<String, f64> {
        &self.output_delays
    }
}

#[derive(Clone, Debug)]
struct TimedCombinationalStage {
    name: String,
    width: usize,
    delay: f64,
    initial: Expr,
    value: Expr,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
struct TimedCombinationalOutput {
    name: String,
    first: usize,
    last: usize,
    delay: f64,
    value: Expr,
    enable: Option<Expr>,
    dependencies: Vec<String>,
}

/// Source bit consumed by a compiled timed ALB LUT.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum KnownLutSourceBit {
    /// External ALB input pin.
    Input(usize),
    /// Bit of an earlier timed combinational stage.
    Stage {
        /// Stage name.
        name: String,
        /// Bit within the stage.
        bit: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KnownDigitalRow {
    value: u32,
    driven: u32,
}

#[derive(Clone, Debug)]
struct KnownDigitalLut {
    sources: Vec<KnownLutSourceBit>,
    output_width: usize,
    rows: Vec<KnownDigitalRow>,
}

impl KnownDigitalLut {
    const fn cell_count(&self) -> usize {
        self.rows.len().saturating_mul(self.output_width)
    }

    fn high_impedance_cell_count(&self) -> usize {
        self.rows
            .iter()
            .map(|row| {
                let mask = width_mask(self.output_width).unwrap_or(u32::MAX);
                usize::try_from((!row.driven & mask).count_ones()).unwrap_or(usize::MAX)
            })
            .sum()
    }

    fn evaluate_possible_bit(&self, inputs: &[AlbLogicLevel]) -> Result<AlbDigitalBit> {
        if self.output_width != 1 {
            bail!(
                "SIMetrix ALB timed bit LUT has unsupported output width {}",
                self.output_width
            );
        }
        if inputs.len() != self.sources.len() {
            bail!(
                "SIMetrix ALB timed bit LUT expected {} inputs, got {}",
                self.sources.len(),
                inputs.len()
            );
        }
        let mut level = None;
        let mut strength = None;
        for (row_index, row) in self.rows.iter().copied().enumerate() {
            if !row_matches_ternary(row_index, inputs) {
                continue;
            }
            let next_level = if row.value & 1 == 0 {
                AlbLogicLevel::Zero
            } else {
                AlbLogicLevel::One
            };
            let next_strength = if row.driven & 1 == 0 {
                AlbDriveStrength::HighImpedance
            } else {
                AlbDriveStrength::Strong
            };
            level = Some(join_logic_level(level, next_level));
            strength = Some(join_drive_strength(strength, next_strength));
        }
        Ok(AlbDigitalBit {
            level: level.context("SIMetrix ALB timed LUT matched no binary rows")?,
            strength: strength.context("SIMetrix ALB timed LUT matched no binary rows")?,
        })
    }

    fn d_genlut_values(&self) -> String {
        let mut values = String::with_capacity(self.cell_count());
        for output in 0..self.output_width {
            let bit = 1_u32 << output;
            for row in &self.rows {
                values.push(if row.driven & bit == 0 {
                    'Z'
                } else if row.value & bit == 0 {
                    '0'
                } else {
                    '1'
                });
            }
        }
        values
    }
}

#[derive(Clone, Debug)]
struct NamedKnownLut {
    name: String,
    bit: usize,
    lut: KnownDigitalLut,
}

#[derive(Clone, Debug)]
pub struct KnownTimedLutSet {
    stages: Vec<NamedKnownLut>,
    outputs: Vec<NamedKnownLut>,
}

impl KnownTimedLutSet {
    #[must_use]
    pub fn structural_summary(&self) -> String {
        let mut all = self.stages.iter().chain(&self.outputs);
        let first = all.next();
        let (mut max_fanin, mut total_cells, mut z_cells) = first.map_or((0, 0, 0), |entry| {
            (
                entry.lut.sources.len(),
                entry.lut.cell_count(),
                entry.lut.high_impedance_cell_count(),
            )
        });
        for entry in all {
            max_fanin = max_fanin.max(entry.lut.sources.len());
            total_cells = total_cells.saturating_add(entry.lut.cell_count());
            z_cells = z_cells.saturating_add(entry.lut.high_impedance_cell_count());
        }
        let stage_bits = self.stages.len();
        let output_bits = self.outputs.len();
        let names = self
            .stages
            .iter()
            .chain(&self.outputs)
            .map(|entry| format!("{}[{}]", entry.name, entry.bit))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "known-state bit LUTs: stage-bits={stage_bits}, output-bits={output_bits}, names=[{names}], max-fanin={max_fanin} bits, cells={total_cells}, Z-cells={z_cells}"
        )
    }

    /// Return the ordered source map for one timed stage bit.
    #[must_use]
    pub fn stage_bit_sources(&self, name: &str, bit: usize) -> Option<&[KnownLutSourceBit]> {
        self.stages
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .map(|entry| entry.lut.sources.as_slice())
    }

    /// Return the ordered source map for one output bit.
    #[must_use]
    pub fn output_bit_sources(&self, name: &str, bit: usize) -> Option<&[KnownLutSourceBit]> {
        self.outputs
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .map(|entry| entry.lut.sources.as_slice())
    }

    /// Compute possible stage-bit states by Boolean-row consensus, not yet simulator equivalence.
    ///
    /// # Errors
    /// Returns an error for a missing stage bit or an invalid source-state width.
    pub fn evaluate_stage_bit_possible(
        &self,
        name: &str,
        bit: usize,
        inputs: &[AlbLogicLevel],
    ) -> Result<AlbDigitalBit> {
        self.stages
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .with_context(|| format!("missing SIMetrix ALB timed stage {name}[{bit}]"))?
            .lut
            .evaluate_possible_bit(inputs)
    }

    /// Compute possible output states/drive by Boolean-row consensus, not yet simulator equivalence.
    ///
    /// # Errors
    /// Returns an error for a missing output bit or an invalid source-state width.
    pub fn evaluate_output_bit_possible(
        &self,
        name: &str,
        bit: usize,
        inputs: &[AlbLogicLevel],
    ) -> Result<AlbDigitalBit> {
        self.outputs
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .with_context(|| format!("missing SIMetrix ALB timed output {name}[{bit}]"))?
            .lut
            .evaluate_possible_bit(inputs)
    }

    #[cfg(test)]
    fn stage_bit(&self, name: &str, bit: usize) -> Option<&KnownDigitalLut> {
        self.stages
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .map(|entry| &entry.lut)
    }

    #[cfg(test)]
    fn output_bit(&self, name: &str, bit: usize) -> Option<&KnownDigitalLut> {
        self.outputs
            .iter()
            .find(|entry| entry.name == name && entry.bit == bit)
            .map(|entry| &entry.lut)
    }
}

#[derive(Clone, Debug)]
pub struct TimedCombinationalGraph {
    inputs: Vec<Port>,
    readonly: BTreeMap<String, ReadOnly>,
    stages: Vec<TimedCombinationalStage>,
    outputs: Vec<TimedCombinationalOutput>,
    input_width: usize,
    output_width: usize,
}

impl TimedCombinationalGraph {
    pub fn known_state_luts(&self) -> Result<KnownTimedLutSet> {
        let stage_widths = self
            .stages
            .iter()
            .map(|stage| (stage.name.clone(), stage.width))
            .collect::<BTreeMap<_, _>>();
        let mut stages = Vec::new();
        for stage in &self.stages {
            for bit in 0..stage.width {
                stages.push(NamedKnownLut {
                    name: stage.name.clone(),
                    bit,
                    lut: self.compile_known_lut_bit(&stage.value, None, bit, &stage_widths)?,
                });
            }
        }
        let mut outputs = Vec::new();
        for output in &self.outputs {
            let width = output.last - output.first + 1;
            for bit in 0..width {
                outputs.push(NamedKnownLut {
                    name: output.name.clone(),
                    bit,
                    lut: self.compile_known_lut_bit(
                        &output.value,
                        output.enable.as_ref(),
                        bit,
                        &stage_widths,
                    )?,
                });
            }
        }
        Ok(KnownTimedLutSet { stages, outputs })
    }

    fn compile_known_lut_bit(
        &self,
        value: &Expr,
        enable: Option<&Expr>,
        output_bit: usize,
        stage_widths: &BTreeMap<String, usize>,
    ) -> Result<KnownDigitalLut> {
        let mut required = BTreeSet::new();
        collect_known_lut_bit_sources(
            value,
            output_bit,
            &self.inputs,
            &self.readonly,
            stage_widths,
            &mut required,
        )?;
        if let Some(enable) = enable {
            collect_known_lut_bit_sources(
                enable,
                output_bit,
                &self.inputs,
                &self.readonly,
                stage_widths,
                &mut required,
            )?;
        }
        let sources = ordered_known_lut_sources(&required, self.input_width, &self.stages)?;
        let row_count = 1usize
            .checked_shl(u32::try_from(sources.len())?)
            .context("SIMetrix ALB LUT row-count overflow")?;
        if row_count > MAX_KNOWN_LUT_CELLS {
            bail!(
                "SIMetrix ALB known-state LUT requires {row_count} cells; bounded lowering permits at most {MAX_KNOWN_LUT_CELLS}"
            );
        }
        let mut rows = Vec::with_capacity(row_count);
        for source_state in 0..row_count {
            let (values, input_bits) = self.known_lut_values(&sources, source_state)?;
            let context = EvalContext {
                values: &values,
                readonly: &self.readonly,
                input_bits: &input_bits,
            };
            let value = (value.evaluate(context)? >> output_bit) & 1;
            let driven = enable.map_or(Ok(1), |enable| {
                enable
                    .evaluate(context)
                    .map(|mask| (mask >> output_bit) & 1)
            })?;
            rows.push(KnownDigitalRow { value, driven });
        }
        Ok(KnownDigitalLut {
            sources,
            output_width: 1,
            rows,
        })
    }

    fn known_lut_values(
        &self,
        sources: &[KnownLutSourceBit],
        source_state: usize,
    ) -> Result<(BTreeMap<String, u32>, Vec<u32>)> {
        let mut input_bits = vec![0_u32; self.input_width];
        let mut stage_values = BTreeMap::<String, u32>::new();
        for (source_index, source) in sources.iter().enumerate() {
            let bit = u32::from((source_state >> source_index) & 1 != 0);
            match source {
                KnownLutSourceBit::Input(pin) => {
                    let target = input_bits.get_mut(*pin).with_context(|| {
                        format!("SIMetrix ALB LUT input pin {pin} is out of range")
                    })?;
                    *target = bit;
                }
                KnownLutSourceBit::Stage {
                    name,
                    bit: stage_bit,
                } => {
                    let value = stage_values.entry(name.clone()).or_default();
                    *value |= bit << stage_bit;
                }
            }
        }
        let mut values = stage_values;
        for port in &self.inputs {
            let mut value = 0_u32;
            for (local, pin) in (port.first..=port.last).enumerate() {
                value |= input_bits[pin] << local;
            }
            values.insert(port.name.clone(), value);
        }
        Ok((values, input_bits))
    }

    pub fn structural_summary(&self) -> String {
        let mut stage_depths = BTreeMap::<String, usize>::new();
        for stage in &self.stages {
            let depth = stage
                .dependencies
                .iter()
                .filter_map(|dependency| stage_depths.get(dependency).copied())
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            stage_depths.insert(stage.name.clone(), depth);
        }
        let max_depth = self
            .outputs
            .iter()
            .map(|output| {
                output
                    .dependencies
                    .iter()
                    .filter_map(|dependency| stage_depths.get(dependency).copied())
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
            })
            .chain(stage_depths.values().copied())
            .max()
            .unwrap_or(0);
        let enabled_outputs = self
            .outputs
            .iter()
            .filter(|output| output.enable.is_some())
            .count();
        let max_stage_width = self
            .stages
            .iter()
            .map(|stage| stage.width)
            .max()
            .unwrap_or(0);
        let initialized = self
            .stages
            .iter()
            .filter(|stage| !matches!(stage.initial, Expr::Constant(0)))
            .count();
        let delay_range = self
            .stages
            .iter()
            .map(|stage| stage.delay)
            .chain(self.outputs.iter().map(|output| output.delay))
            .fold(None, |range: Option<(f64, f64)>, value| {
                Some(match range {
                    None => (value, value),
                    Some((minimum, maximum)) => (minimum.min(value), maximum.max(value)),
                })
            });
        let output_bits = self
            .outputs
            .iter()
            .map(|output| output.last - output.first + 1)
            .sum::<usize>();
        let output_names = self
            .outputs
            .iter()
            .map(|output| output.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let expression_nodes = self
            .stages
            .iter()
            .map(|stage| expression_node_count(&stage.value))
            .chain(self.outputs.iter().map(|output| {
                expression_node_count(&output.value)
                    + output.enable.as_ref().map_or(0, expression_node_count)
            }))
            .sum::<usize>();
        let input_width = self.input_width;
        let output_width = self.output_width;
        let input_ports = self.inputs.len();
        let readonly = self.readonly.len();
        let stage_count = self.stages.len();
        let output_count = self.outputs.len();
        format!(
            "timed COMB graph: {input_width}in/{output_width}out ({input_ports} input ports, {readonly} readonly, {output_bits} output bits), {stage_count} stages (depth={max_depth}, max-width={max_stage_width}, initialized={initialized}), {output_count} outputs [{output_names}] ({enabled_outputs} tri-state), delays={delay_range:?}, expression-nodes={expression_nodes}",
        )
    }

    #[cfg(test)]
    fn stage_names(&self) -> Vec<&str> {
        self.stages
            .iter()
            .map(|stage| stage.name.as_str())
            .collect()
    }

    #[cfg(test)]
    fn stage(&self, name: &str) -> Option<&TimedCombinationalStage> {
        self.stages.iter().find(|stage| stage.name == name)
    }

    #[cfg(test)]
    fn output(&self, name: &str) -> Option<&TimedCombinationalOutput> {
        self.outputs.iter().find(|output| output.name == name)
    }
}

#[derive(Clone, Debug)]
pub struct StatefulProgram {
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    readonly: BTreeMap<String, ReadOnly>,
    registers: Vec<StateRegister>,
    assignments: Vec<StateAssignment>,
    input_width: usize,
    output_width: usize,
}

impl StatefulProgram {
    pub fn register_count(&self, kind: StateRegisterKind) -> usize {
        self.registers
            .iter()
            .filter(|register| register.kind == kind)
            .count()
    }

    pub fn register_array_count(&self) -> usize {
        self.registers
            .iter()
            .filter(|register| register.array_size > 1)
            .count()
    }

    pub fn asynchronous_assignment_count(&self) -> usize {
        self.assignments
            .iter()
            .filter(|assignment| {
                matches!(assignment.value, StateAssignmentValue::Asynchronous { .. })
            })
            .count()
    }

    pub fn output_enable_assignment_count(&self) -> usize {
        self.assignments
            .iter()
            .filter(|assignment| {
                matches!(assignment.target, StateAssignmentTarget::OutputEnable(_))
            })
            .count()
    }

    pub fn indexed_assignment_count(&self) -> usize {
        self.assignments
            .iter()
            .filter(|assignment| {
                matches!(
                    assignment.target,
                    StateAssignmentTarget::RegisterIndex { .. }
                )
            })
            .count()
    }

    pub const fn input_width(&self) -> usize {
        self.input_width
    }

    pub const fn output_width(&self) -> usize {
        self.output_width
    }

    pub fn combinational_timing_shape(&self) -> Option<CombinationalTimingShape> {
        if self
            .registers
            .iter()
            .any(|register| register.kind != StateRegisterKind::Combinational)
        {
            return None;
        }
        let register_names = self
            .registers
            .iter()
            .map(|register| register.name.clone())
            .collect::<BTreeSet<_>>();
        let feedback = combinational_dependency_graph(self, &register_names)
            .is_some_and(|graph| dependency_graph_has_cycle(&graph));
        let delay_mode =
            self.registers
                .iter()
                .fold(CombinationalDelayMode::Bitwise, |mode, register| {
                    match (&register.bitwise, mode) {
                        (Some(DynamicValue::User(_) | DynamicValue::Device(_)), _)
                        | (_, CombinationalDelayMode::Dynamic) => CombinationalDelayMode::Dynamic,
                        (Some(DynamicValue::Literal(value)), _) if *value == 0.0 => {
                            CombinationalDelayMode::WholeRegister
                        }
                        (_, current) => current,
                    }
                });
        Some(CombinationalTimingShape {
            feedback,
            delay_mode,
        })
    }

    pub fn resolve_combinational_timing(
        &self,
        environment: TimingEnvironment<'_>,
    ) -> Result<Option<ResolvedCombinationalTiming>> {
        if self.combinational_timing_shape().is_none() {
            return Ok(None);
        }
        validate_timing_default("REG_DELAY", environment.default_register_delay, 0.0)?;
        validate_timing_default("OUT_DELAY", environment.default_output_delay, 0.0)?;
        if !environment.user_scale.is_finite() || environment.user_scale < 0.0 {
            bail!(
                "SIMetrix ALB USER_SCALE={} is invalid",
                environment.user_scale
            );
        }
        let register_delays = self
            .registers
            .iter()
            .map(|register| {
                let delay = resolve_timing_value(
                    register.delay.as_ref(),
                    environment.default_register_delay,
                    environment,
                )?;
                validate_timing_default("COMB DELAY", delay, 0.0)?;
                Ok((register.name.clone(), delay))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let output_delays = self
            .outputs
            .iter()
            .map(|port| {
                let delay = resolve_timing_value(
                    port.delay.as_ref(),
                    environment.default_output_delay,
                    environment,
                )?;
                validate_timing_default("PORT DELAY", delay, 0.0)?;
                Ok((port.name.clone(), delay))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(Some(ResolvedCombinationalTiming {
            register_delays,
            output_delays,
        }))
    }

    pub fn timed_combinational_graph(
        &self,
        environment: TimingEnvironment<'_>,
    ) -> Result<Option<TimedCombinationalGraph>> {
        let Some(shape) = self.combinational_timing_shape() else {
            return Ok(None);
        };
        if shape.has_feedback() || shape.delay_mode() != CombinationalDelayMode::Bitwise {
            return Ok(None);
        }
        let timing = self
            .resolve_combinational_timing(environment)?
            .context("acyclic COMB program has no resolved timing")?;
        let register_names = self
            .registers
            .iter()
            .map(|register| register.name.clone())
            .collect::<BTreeSet<_>>();
        let Some(mut expressions) = normalized_comb_assignments(self)? else {
            return Ok(None);
        };
        validate_comb_stage_definitions(self, &expressions.stage_values)?;
        let dependency_graph = expressions
            .stage_values
            .iter()
            .map(|(name, value)| (name.clone(), register_dependencies(value, &register_names)))
            .collect::<BTreeMap<_, _>>();
        let stage_order = topological_dependency_order(&dependency_graph)?;
        let stages = build_timed_comb_stages(
            self,
            environment,
            &timing,
            &dependency_graph,
            &stage_order,
            &mut expressions.stage_values,
        )?;
        let outputs = build_timed_comb_outputs(
            self,
            &timing,
            &register_names,
            &mut expressions.output_values,
            &mut expressions.output_enables,
        )?;
        Ok(Some(TimedCombinationalGraph {
            inputs: self.inputs.clone(),
            readonly: self.readonly.clone(),
            stages,
            outputs,
            input_width: self.input_width,
            output_width: self.output_width,
        }))
    }

    pub fn structural_summary(&self) -> String {
        let summary = StatefulSummary::new(self).render();
        let Some(shape) = self.combinational_timing_shape() else {
            return summary;
        };
        let topology = if shape.has_feedback() {
            "feedback"
        } else {
            "acyclic"
        };
        let delay_mode = match shape.delay_mode() {
            CombinationalDelayMode::Bitwise => "bitwise",
            CombinationalDelayMode::WholeRegister => "whole-register",
            CombinationalDelayMode::Dynamic => "dynamic-bitwise",
        };
        format!("{summary}; COMB timing={topology}/{delay_mode}")
    }
}

fn collect_all_known_lut_sources(
    expression: &Expr,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    match expression {
        Expr::Constant(_) => {}
        Expr::Name(name) => {
            if let Some(port) = inputs.iter().find(|port| port.name == *name) {
                output.extend((port.first..=port.last).map(KnownLutSourceBit::Input));
            } else if let Some(width) = stage_widths.get(name) {
                output.extend((0..*width).map(|bit| KnownLutSourceBit::Stage {
                    name: name.clone(),
                    bit,
                }));
            } else if readonly
                .get(name)
                .is_some_and(|register| register.values.len() == 1)
            {
            } else {
                bail!("SIMetrix ALB LUT expression references unsupported value {name}");
            }
        }
        Expr::Index(name, index) => {
            collect_all_known_lut_sources(index, inputs, readonly, stage_widths, output)?;
            if name == "IN" {
                let Expr::Constant(pin) = index.as_ref() else {
                    bail!("SIMetrix ALB LUT direct IN[] reference requires a constant pin index");
                };
                output.insert(KnownLutSourceBit::Input(usize::try_from(*pin)?));
            } else if !readonly.contains_key(name) {
                bail!("SIMetrix ALB LUT expression indexes unsupported value {name}");
            }
        }
        Expr::Unary(_, value) => {
            collect_all_known_lut_sources(value, inputs, readonly, stage_widths, output)?;
        }
        Expr::Binary(_, lhs, rhs) => {
            collect_all_known_lut_sources(lhs, inputs, readonly, stage_widths, output)?;
            collect_all_known_lut_sources(rhs, inputs, readonly, stage_widths, output)?;
        }
        Expr::Conditional(condition, yes, no) => {
            collect_all_known_lut_sources(condition, inputs, readonly, stage_widths, output)?;
            collect_all_known_lut_sources(yes, inputs, readonly, stage_widths, output)?;
            collect_all_known_lut_sources(no, inputs, readonly, stage_widths, output)?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct KnownLutSourceEnvironment<'a> {
    inputs: &'a [Port],
    readonly: &'a BTreeMap<String, ReadOnly>,
    stage_widths: &'a BTreeMap<String, usize>,
}

fn collect_known_lut_bit_sources(
    expression: &Expr,
    bit: usize,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    if bit >= MAX_VALUE_BITS {
        return Ok(());
    }
    match expression {
        Expr::Constant(_) => {}
        Expr::Name(name) => {
            collect_known_lut_name_bit(name, bit, inputs, readonly, stage_widths, output)?;
        }
        Expr::Index(name, index) => {
            collect_known_lut_index_bit(name, index, bit, inputs, readonly, stage_widths, output)?;
        }
        Expr::Unary(operation, value) => {
            collect_known_lut_unary_bit(
                *operation,
                value,
                bit,
                inputs,
                readonly,
                stage_widths,
                output,
            )?;
        }
        Expr::Binary(operation, lhs, rhs) => {
            collect_known_lut_binary_bit(
                *operation,
                lhs,
                rhs,
                bit,
                KnownLutSourceEnvironment {
                    inputs,
                    readonly,
                    stage_widths,
                },
                output,
            )?;
        }
        Expr::Conditional(condition, yes, no) => {
            collect_all_known_lut_sources(condition, inputs, readonly, stage_widths, output)?;
            collect_known_lut_bit_sources(yes, bit, inputs, readonly, stage_widths, output)?;
            collect_known_lut_bit_sources(no, bit, inputs, readonly, stage_widths, output)?;
        }
    }
    Ok(())
}

fn collect_known_lut_name_bit(
    name: &str,
    bit: usize,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    if let Some(port) = inputs.iter().find(|port| port.name == name) {
        if bit < port.width() {
            output.insert(KnownLutSourceBit::Input(port.first + bit));
        }
    } else if let Some(width) = stage_widths.get(name) {
        if bit < *width {
            output.insert(KnownLutSourceBit::Stage {
                name: name.to_owned(),
                bit,
            });
        }
    } else if readonly
        .get(name)
        .is_none_or(|register| register.values.len() != 1)
    {
        bail!("SIMetrix ALB LUT expression references unsupported value {name}");
    }
    Ok(())
}

fn collect_known_lut_index_bit(
    name: &str,
    index: &Expr,
    bit: usize,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    if name == "IN" {
        if bit == 0 {
            let Expr::Constant(pin) = index else {
                bail!("SIMetrix ALB LUT direct IN[] reference requires a constant pin index");
            };
            output.insert(KnownLutSourceBit::Input(usize::try_from(*pin)?));
        }
    } else if readonly.contains_key(name) {
        collect_all_known_lut_sources(index, inputs, readonly, stage_widths, output)?;
    } else {
        bail!("SIMetrix ALB LUT expression indexes unsupported value {name}");
    }
    Ok(())
}

fn collect_known_lut_unary_bit(
    operation: UnaryOp,
    value: &Expr,
    bit: usize,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    match operation {
        UnaryOp::Plus | UnaryOp::BitwiseNot => {
            collect_known_lut_bit_sources(value, bit, inputs, readonly, stage_widths, output)?;
        }
        UnaryOp::Minus => {
            for source_bit in 0..=bit {
                collect_known_lut_bit_sources(
                    value,
                    source_bit,
                    inputs,
                    readonly,
                    stage_widths,
                    output,
                )?;
            }
        }
        UnaryOp::LogicalNot if bit == 0 => {
            collect_all_known_lut_sources(value, inputs, readonly, stage_widths, output)?;
        }
        UnaryOp::LogicalNot => {}
    }
    Ok(())
}

fn collect_known_lut_binary_bit(
    operation: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    bit: usize,
    environment: KnownLutSourceEnvironment<'_>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    let KnownLutSourceEnvironment {
        inputs,
        readonly,
        stage_widths,
    } = environment;
    match operation {
        BinaryOp::BitAnd | BinaryOp::BitXor | BinaryOp::BitOr => {
            collect_known_lut_bit_sources(lhs, bit, inputs, readonly, stage_widths, output)?;
            collect_known_lut_bit_sources(rhs, bit, inputs, readonly, stage_widths, output)?;
        }
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply => {
            collect_known_lut_arithmetic_bit(
                lhs,
                rhs,
                bit,
                inputs,
                readonly,
                stage_widths,
                output,
            )?;
        }
        BinaryOp::ShiftLeft | BinaryOp::ShiftRight => {
            collect_known_lut_shift_bit(operation, lhs, rhs, bit, environment, output)?;
        }
        BinaryOp::Divide | BinaryOp::Modulo => {
            collect_all_known_lut_sources(lhs, inputs, readonly, stage_widths, output)?;
            collect_all_known_lut_sources(rhs, inputs, readonly, stage_widths, output)?;
        }
        BinaryOp::Less
        | BinaryOp::Greater
        | BinaryOp::LessEqual
        | BinaryOp::GreaterEqual
        | BinaryOp::Equal
        | BinaryOp::NotEqual
        | BinaryOp::LogicalAnd
        | BinaryOp::LogicalOr => {
            if bit == 0 {
                collect_all_known_lut_sources(lhs, inputs, readonly, stage_widths, output)?;
                collect_all_known_lut_sources(rhs, inputs, readonly, stage_widths, output)?;
            }
        }
    }
    Ok(())
}

fn collect_known_lut_arithmetic_bit(
    lhs: &Expr,
    rhs: &Expr,
    bit: usize,
    inputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
    stage_widths: &BTreeMap<String, usize>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    for source_bit in 0..=bit {
        collect_known_lut_bit_sources(lhs, source_bit, inputs, readonly, stage_widths, output)?;
        collect_known_lut_bit_sources(rhs, source_bit, inputs, readonly, stage_widths, output)?;
    }
    Ok(())
}

fn collect_known_lut_shift_bit(
    operation: BinaryOp,
    lhs: &Expr,
    rhs: &Expr,
    bit: usize,
    environment: KnownLutSourceEnvironment<'_>,
    output: &mut BTreeSet<KnownLutSourceBit>,
) -> Result<()> {
    let KnownLutSourceEnvironment {
        inputs,
        readonly,
        stage_widths,
    } = environment;
    let Expr::Constant(amount) = rhs else {
        collect_all_known_lut_sources(lhs, inputs, readonly, stage_widths, output)?;
        collect_all_known_lut_sources(rhs, inputs, readonly, stage_widths, output)?;
        return Ok(());
    };
    let amount = usize::try_from(*amount)?;
    if amount >= MAX_VALUE_BITS {
        bail!("SIMetrix ALB shift by {amount} is outside 0..31");
    }
    let source_bit = match operation {
        BinaryOp::ShiftLeft => bit.checked_sub(amount),
        BinaryOp::ShiftRight => bit
            .checked_add(amount)
            .filter(|source_bit| *source_bit < MAX_VALUE_BITS),
        _ => bail!("internal SIMetrix ALB non-shift operation in shift dependency analysis"),
    };
    if let Some(source_bit) = source_bit {
        collect_known_lut_bit_sources(lhs, source_bit, inputs, readonly, stage_widths, output)?;
    }
    Ok(())
}

fn ordered_known_lut_sources(
    required: &BTreeSet<KnownLutSourceBit>,
    input_width: usize,
    stages: &[TimedCombinationalStage],
) -> Result<Vec<KnownLutSourceBit>> {
    let mut ordered = Vec::with_capacity(required.len());
    for pin in 0..input_width {
        let source = KnownLutSourceBit::Input(pin);
        if required.contains(&source) {
            ordered.push(source);
        }
    }
    for stage in stages {
        for bit in 0..stage.width {
            let source = KnownLutSourceBit::Stage {
                name: stage.name.clone(),
                bit,
            };
            if required.contains(&source) {
                ordered.push(source);
            }
        }
    }
    if ordered.len() != required.len() {
        bail!("SIMetrix ALB LUT source set contains an undeclared signal bit");
    }
    Ok(ordered)
}

struct NormalizedCombAssignments {
    stage_values: BTreeMap<String, Expr>,
    output_values: BTreeMap<String, Expr>,
    output_enables: BTreeMap<String, Expr>,
}

fn normalized_comb_assignments(
    program: &StatefulProgram,
) -> Result<Option<NormalizedCombAssignments>> {
    let mut immediate = BTreeMap::<String, Expr>::new();
    let mut normalized = NormalizedCombAssignments {
        stage_values: BTreeMap::new(),
        output_values: BTreeMap::new(),
        output_enables: BTreeMap::new(),
    };
    for assignment in &program.assignments {
        let StateAssignmentValue::Synchronous(value) = &assignment.value else {
            return Ok(None);
        };
        let value = inline_immediate_expression(value, &immediate);
        match &assignment.target {
            StateAssignmentTarget::Register(name) => {
                if normalized
                    .stage_values
                    .insert(name.clone(), value)
                    .is_some()
                {
                    bail!(
                        "acyclic SIMetrix COMB register {name} has multiple defining assignments"
                    );
                }
            }
            StateAssignmentTarget::Local(name) => {
                immediate.insert(name.clone(), value);
            }
            StateAssignmentTarget::Output(name) => {
                immediate.insert(name.clone(), value.clone());
                normalized.output_values.insert(name.clone(), value);
            }
            StateAssignmentTarget::OutputEnable(name) => {
                normalized.output_enables.insert(name.clone(), value);
            }
            StateAssignmentTarget::OutputRange { .. } => {
                bail!(
                    "acyclic SIMetrix COMB graph does not yet normalize direct OUT[] assignments"
                );
            }
            StateAssignmentTarget::RegisterIndex { .. } => return Ok(None),
        }
    }
    Ok(Some(normalized))
}

fn validate_comb_stage_definitions(
    program: &StatefulProgram,
    stage_values: &BTreeMap<String, Expr>,
) -> Result<()> {
    for register in &program.registers {
        if !stage_values.contains_key(&register.name) {
            bail!(
                "acyclic SIMetrix COMB register {} has no defining assignment",
                register.name
            );
        }
    }
    Ok(())
}

fn build_timed_comb_stages(
    program: &StatefulProgram,
    environment: TimingEnvironment<'_>,
    timing: &ResolvedCombinationalTiming,
    dependency_graph: &BTreeMap<String, BTreeSet<String>>,
    stage_order: &[String],
    stage_values: &mut BTreeMap<String, Expr>,
) -> Result<Vec<TimedCombinationalStage>> {
    let register_by_name = program
        .registers
        .iter()
        .map(|register| (register.name.as_str(), register))
        .collect::<BTreeMap<_, _>>();
    let mut stages = Vec::with_capacity(stage_order.len());
    for name in stage_order {
        let register = register_by_name
            .get(name.as_str())
            .with_context(|| format!("missing SIMetrix COMB register {name}"))?;
        let width = resolve_register_width(&register.width, environment)?;
        let value = stage_values
            .remove(name)
            .with_context(|| format!("missing SIMetrix COMB assignment {name}"))?;
        let dependencies = dependency_graph
            .get(name)
            .into_iter()
            .flat_map(|dependencies| dependencies.iter().cloned())
            .collect();
        let initial = register
            .initial
            .first()
            .cloned()
            .unwrap_or(Expr::Constant(0));
        stages.push(TimedCombinationalStage {
            name: name.clone(),
            width,
            delay: timing.register_delays()[name],
            initial,
            value,
            dependencies,
        });
    }
    Ok(stages)
}

fn build_timed_comb_outputs(
    program: &StatefulProgram,
    timing: &ResolvedCombinationalTiming,
    register_names: &BTreeSet<String>,
    output_values: &mut BTreeMap<String, Expr>,
    output_enables: &mut BTreeMap<String, Expr>,
) -> Result<Vec<TimedCombinationalOutput>> {
    let mut outputs = Vec::with_capacity(program.outputs.len());
    for port in &program.outputs {
        let value = output_values.remove(&port.name).with_context(|| {
            format!(
                "acyclic SIMetrix COMB output {} has no defining assignment",
                port.name
            )
        })?;
        let enable = output_enables.remove(&port.name);
        let mut dependencies = register_dependencies(&value, register_names);
        if let Some(enable) = enable.as_ref() {
            dependencies.extend(register_dependencies(enable, register_names));
        }
        outputs.push(TimedCombinationalOutput {
            name: port.name.clone(),
            first: port.first,
            last: port.last,
            delay: timing.output_delays()[&port.name],
            value,
            enable,
            dependencies: dependencies.into_iter().collect(),
        });
    }
    if let Some(name) = output_values.keys().next() {
        bail!("SIMetrix COMB output assignment targets undeclared output {name}");
    }
    if let Some(name) = output_enables.keys().next() {
        bail!("SIMetrix COMB output-enable assignment targets undeclared output {name}");
    }
    Ok(outputs)
}

fn inline_immediate_expression(expression: &Expr, immediate: &BTreeMap<String, Expr>) -> Expr {
    match expression {
        Expr::Constant(value) => Expr::Constant(*value),
        Expr::Name(name) => immediate
            .get(name)
            .cloned()
            .unwrap_or_else(|| Expr::Name(name.clone())),
        Expr::Index(name, index) => Expr::Index(
            name.clone(),
            Box::new(inline_immediate_expression(index, immediate)),
        ),
        Expr::Unary(operation, value) => Expr::Unary(
            *operation,
            Box::new(inline_immediate_expression(value, immediate)),
        ),
        Expr::Binary(operation, lhs, rhs) => Expr::Binary(
            *operation,
            Box::new(inline_immediate_expression(lhs, immediate)),
            Box::new(inline_immediate_expression(rhs, immediate)),
        ),
        Expr::Conditional(condition, yes, no) => Expr::Conditional(
            Box::new(inline_immediate_expression(condition, immediate)),
            Box::new(inline_immediate_expression(yes, immediate)),
            Box::new(inline_immediate_expression(no, immediate)),
        ),
    }
}

fn register_dependencies(expression: &Expr, register_names: &BTreeSet<String>) -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    expression_register_dependencies(
        expression,
        register_names,
        &BTreeMap::new(),
        &mut dependencies,
    );
    dependencies
}

fn topological_dependency_order(graph: &BTreeMap<String, BTreeSet<String>>) -> Result<Vec<String>> {
    let mut indegree = graph
        .iter()
        .map(|(name, dependencies)| (name.clone(), dependencies.len()))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, dependencies) in graph {
        for dependency in dependencies {
            if !graph.contains_key(dependency) {
                bail!("SIMetrix COMB graph references undeclared stage {dependency}");
            }
            dependents
                .entry(dependency.clone())
                .or_default()
                .insert(name.clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(name, _)| name.clone())
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(graph.len());
    while let Some(name) = ready.pop_first() {
        order.push(name.clone());
        if let Some(children) = dependents.get(&name) {
            for child in children {
                let count = indegree
                    .get_mut(child)
                    .with_context(|| format!("missing SIMetrix COMB stage {child}"))?;
                *count = count
                    .checked_sub(1)
                    .context("SIMetrix COMB dependency underflow")?;
                if *count == 0 {
                    ready.insert(child.clone());
                }
            }
        }
    }
    if order.len() != graph.len() {
        bail!("SIMetrix COMB dependency graph contains feedback");
    }
    Ok(order)
}

fn resolve_register_width(
    value: &DynamicValue,
    environment: TimingEnvironment<'_>,
) -> Result<usize> {
    let value = resolve_dynamic_value(value, environment)?;
    if !value.is_finite() || value.fract() != 0.0 || !(1.0..=32.0).contains(&value) {
        bail!("SIMetrix ALB resolved register WIDTH={value} is outside 1..={MAX_VALUE_BITS}");
    }
    let width = (1_u32..=u32::try_from(MAX_VALUE_BITS)?)
        .find(|candidate| {
            let lower = f64::from(*candidate);
            value >= lower && value < lower + 1.0
        })
        .context("validated SIMetrix ALB register width has no integer representation")?;
    Ok(usize::try_from(width)?)
}

fn validate_timing_default(name: &str, value: f64, minimum: f64) -> Result<()> {
    if !value.is_finite() || value < minimum {
        bail!("invalid SIMetrix ALB {name}={value}");
    }
    Ok(())
}

fn resolve_dynamic_value(value: &DynamicValue, environment: TimingEnvironment<'_>) -> Result<f64> {
    match value {
        DynamicValue::Literal(value) => Ok(*value),
        DynamicValue::User(index) => environment
            .user
            .get(*index)
            .copied()
            .map(|value| value * environment.user_scale)
            .with_context(|| format!("SIMetrix ALB USER[{index}] is out of range")),
        DynamicValue::Device(index) => environment
            .device
            .and_then(|values| values.get(*index))
            .copied()
            .with_context(|| {
                format!("SIMetrix ALB DEVICE[{index}] requires an instance parameter environment")
            }),
    }
}

fn resolve_timing_value(
    value: Option<&DynamicValue>,
    default: f64,
    environment: TimingEnvironment<'_>,
) -> Result<f64> {
    value.map_or(Ok(default), |value| {
        resolve_dynamic_value(value, environment)
    })
}

fn expression_register_dependencies(
    expression: &Expr,
    register_names: &BTreeSet<String>,
    immediate_dependencies: &BTreeMap<String, BTreeSet<String>>,
    output: &mut BTreeSet<String>,
) {
    match expression {
        Expr::Constant(_) => {}
        Expr::Name(name) => {
            if register_names.contains(name) {
                output.insert(name.clone());
            } else if let Some(dependencies) = immediate_dependencies.get(name) {
                output.extend(dependencies.iter().cloned());
            }
        }
        Expr::Index(name, index) => {
            if register_names.contains(name) {
                output.insert(name.clone());
            } else if let Some(dependencies) = immediate_dependencies.get(name) {
                output.extend(dependencies.iter().cloned());
            }
            expression_register_dependencies(index, register_names, immediate_dependencies, output);
        }
        Expr::Unary(_, value) => {
            expression_register_dependencies(value, register_names, immediate_dependencies, output);
        }
        Expr::Binary(_, lhs, rhs) => {
            expression_register_dependencies(lhs, register_names, immediate_dependencies, output);
            expression_register_dependencies(rhs, register_names, immediate_dependencies, output);
        }
        Expr::Conditional(condition, yes, no) => {
            expression_register_dependencies(
                condition,
                register_names,
                immediate_dependencies,
                output,
            );
            expression_register_dependencies(yes, register_names, immediate_dependencies, output);
            expression_register_dependencies(no, register_names, immediate_dependencies, output);
        }
    }
}

fn synchronous_dependencies(
    value: &StateAssignmentValue,
    register_names: &BTreeSet<String>,
    immediate_dependencies: &BTreeMap<String, BTreeSet<String>>,
) -> Option<BTreeSet<String>> {
    let StateAssignmentValue::Synchronous(value) = value else {
        return None;
    };
    let mut dependencies = BTreeSet::new();
    expression_register_dependencies(
        value,
        register_names,
        immediate_dependencies,
        &mut dependencies,
    );
    Some(dependencies)
}

fn combinational_dependency_graph(
    program: &StatefulProgram,
    register_names: &BTreeSet<String>,
) -> Option<BTreeMap<String, BTreeSet<String>>> {
    let mut graph = register_names
        .iter()
        .cloned()
        .map(|name| (name, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    let mut immediate_dependencies = BTreeMap::<String, BTreeSet<String>>::new();
    for assignment in &program.assignments {
        let dependencies =
            synchronous_dependencies(&assignment.value, register_names, &immediate_dependencies)?;
        match &assignment.target {
            StateAssignmentTarget::Register(name) => {
                graph.entry(name.clone()).or_default().extend(dependencies);
            }
            StateAssignmentTarget::Local(name) | StateAssignmentTarget::Output(name) => {
                immediate_dependencies.insert(name.clone(), dependencies);
            }
            StateAssignmentTarget::OutputRange { .. }
            | StateAssignmentTarget::OutputEnable(_)
            | StateAssignmentTarget::RegisterIndex { .. } => {}
        }
    }
    Some(graph)
}

fn dependency_graph_has_cycle(graph: &BTreeMap<String, BTreeSet<String>>) -> bool {
    fn visit(
        node: &str,
        graph: &BTreeMap<String, BTreeSet<String>>,
        active: &mut BTreeSet<String>,
        complete: &mut BTreeSet<String>,
    ) -> bool {
        if complete.contains(node) {
            return false;
        }
        if !active.insert(node.to_owned()) {
            return true;
        }
        if graph.get(node).is_some_and(|dependencies| {
            dependencies
                .iter()
                .any(|dependency| visit(dependency, graph, active, complete))
        }) {
            return true;
        }
        active.remove(node);
        complete.insert(node.to_owned());
        false
    }

    let mut active = BTreeSet::new();
    let mut complete = BTreeSet::new();
    graph
        .keys()
        .any(|node| visit(node, graph, &mut active, &mut complete))
}

struct StatefulSummary {
    input_width: usize,
    output_width: usize,
    input_ports: usize,
    output_ports: usize,
    readonly: usize,
    comb: usize,
    edge: usize,
    level: usize,
    clocked: usize,
    arrays: usize,
    initialized: usize,
    delay: usize,
    min_clock: usize,
    hold: usize,
    setup: usize,
    async_delay: usize,
    bitwise: usize,
    dynamic_qualifiers: usize,
    assignments: usize,
    asynchronous: usize,
    output_enable: usize,
    indexed: usize,
    expression_nodes: usize,
}

impl StatefulSummary {
    fn new(program: &StatefulProgram) -> Self {
        let count_register = |predicate: fn(&StateRegister) -> bool| {
            program
                .registers
                .iter()
                .filter(|register| predicate(register))
                .count()
        };
        let dynamic_qualifiers = program.registers.iter().map(dynamic_qualifier_count).sum();
        let expression_nodes = program
            .assignments
            .iter()
            .map(assignment_expression_nodes)
            .sum();
        Self {
            input_width: program.input_width(),
            output_width: program.output_width(),
            input_ports: program.inputs.len(),
            output_ports: program.outputs.len(),
            readonly: program.readonly.len(),
            comb: program.register_count(StateRegisterKind::Combinational),
            edge: program.register_count(StateRegisterKind::Edge),
            level: program.register_count(StateRegisterKind::Level),
            clocked: count_register(|register| register.clock_pin.is_some()),
            arrays: program.register_array_count(),
            initialized: count_register(|register| !register.initial.is_empty()),
            delay: count_register(|register| register.delay.is_some()),
            min_clock: count_register(|register| register.min_clock.is_some()),
            hold: count_register(|register| register.hold.is_some()),
            setup: count_register(|register| register.setup.is_some()),
            async_delay: count_register(|register| register.async_delay.is_some()),
            bitwise: count_register(|register| register.bitwise.is_some()),
            dynamic_qualifiers,
            assignments: program.assignments.len(),
            asynchronous: program.asynchronous_assignment_count(),
            output_enable: program.output_enable_assignment_count(),
            indexed: program.indexed_assignment_count(),
            expression_nodes,
        }
    }

    fn render(&self) -> String {
        let Self {
            input_width,
            output_width,
            input_ports,
            output_ports,
            readonly,
            comb,
            edge,
            level,
            clocked,
            arrays,
            initialized,
            delay,
            min_clock,
            hold,
            setup,
            async_delay,
            bitwise,
            dynamic_qualifiers,
            assignments,
            asynchronous,
            output_enable,
            indexed,
            expression_nodes,
        } = self;
        format!(
            "{input_width}in/{output_width}out (ports in={input_ports} out={output_ports}, readonly={readonly}); registers COMB={comb} EDGE={edge} LEVEL={level} (clocked={clocked}, arrays={arrays}, initialized={initialized}, delay={delay}, minclock={min_clock}, hold={hold}, setup={setup}, asyncdelay={async_delay}, bitwise={bitwise}, dynamic-qualifiers={dynamic_qualifiers}); assignments={assignments} (async={asynchronous}, output-enable={output_enable}, indexed={indexed}); expression-nodes={expression_nodes}"
        )
    }
}

fn dynamic_qualifier_count(register: &StateRegister) -> usize {
    [
        Some(&register.width),
        register.delay.as_ref(),
        register.min_clock.as_ref(),
        register.hold.as_ref(),
        register.setup.as_ref(),
        register.async_delay.as_ref(),
        register.bitwise.as_ref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !matches!(value, DynamicValue::Literal(_)))
    .count()
}

fn assignment_expression_nodes(assignment: &StateAssignment) -> usize {
    let target_nodes = match &assignment.target {
        StateAssignmentTarget::Register(name)
        | StateAssignmentTarget::Output(name)
        | StateAssignmentTarget::OutputEnable(name)
        | StateAssignmentTarget::Local(name) => usize::from(!name.is_empty()),
        StateAssignmentTarget::RegisterIndex { name, index } => {
            usize::from(!name.is_empty()) + expression_node_count(index)
        }
        StateAssignmentTarget::OutputRange { first, last } => usize::from(last >= first),
    };
    target_nodes
        + match &assignment.value {
            StateAssignmentValue::Synchronous(value) => expression_node_count(value),
            StateAssignmentValue::Asynchronous { condition, value } => {
                expression_node_count(condition) + expression_node_count(value)
            }
        }
}

fn expression_node_count(expression: &Expr) -> usize {
    match expression {
        Expr::Constant(_) | Expr::Name(_) => 1,
        Expr::Index(_, index) | Expr::Unary(_, index) => 1 + expression_node_count(index),
        Expr::Binary(_, lhs, rhs) => 1 + expression_node_count(lhs) + expression_node_count(rhs),
        Expr::Conditional(condition, yes, no) => {
            1 + expression_node_count(condition)
                + expression_node_count(yes)
                + expression_node_count(no)
        }
    }
}

#[derive(Clone, Debug)]
pub struct BodyAnalysis {
    known_truth_table: Option<KnownTruthTable>,
    stateful: Option<StatefulProgram>,
}

impl BodyAnalysis {
    pub fn into_parts(self) -> (Option<KnownTruthTable>, Option<StatefulProgram>) {
        (self.known_truth_table, self.stateful)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PortKind {
    Input,
    Output,
}

#[derive(Clone, Debug)]
struct Port {
    name: String,
    kind: PortKind,
    first: usize,
    last: usize,
    delay: Option<DynamicValue>,
}

impl Port {
    const fn width(&self) -> usize {
        self.last - self.first + 1
    }
}

#[derive(Clone, Debug)]
struct ReadOnly {
    values: Vec<u32>,
}

#[derive(Clone, Debug)]
enum AssignmentTarget {
    Name(String),
    OutputRange { first: usize, last: usize },
}

#[derive(Clone, Debug)]
struct Assignment {
    target: AssignmentTarget,
    value: Expr,
}

#[derive(Clone, Debug)]
struct Program {
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    readonly: BTreeMap<String, ReadOnly>,
    assignments: Vec<Assignment>,
    input_width: usize,
    output_width: usize,
}

impl Program {
    fn known_truth_table(&self) -> Result<KnownTruthTable> {
        if self.input_width > MAX_TRUTH_TABLE_INPUTS {
            bail!(
                "stateless SIMetrix ALB has {} inputs; bounded truth-table lowering supports at most {MAX_TRUTH_TABLE_INPUTS}",
                self.input_width
            );
        }
        let row_count = 1usize
            .checked_shl(u32::try_from(self.input_width)?)
            .context("SIMetrix ALB truth-table size overflow")?;
        let mut rows = Vec::with_capacity(row_count);
        for input_state in 0..row_count {
            rows.push(self.evaluate_known(u32::try_from(input_state)?)?);
        }
        let delayed_output_bits = self
            .outputs
            .iter()
            .filter(|port| port.delay.is_some())
            .map(Port::width)
            .sum();
        Ok(KnownTruthTable {
            input_width: self.input_width,
            output_width: self.output_width,
            rows,
            delayed_output_bits,
        })
    }

    fn evaluate_known(&self, input_state: u32) -> Result<u32> {
        let mut values = BTreeMap::<String, u32>::new();
        let input_bits = (0..self.input_width)
            .map(|bit| (input_state >> bit) & 1)
            .collect::<Vec<_>>();
        for port in &self.inputs {
            let mut value = 0_u32;
            for (local, pin) in (port.first..=port.last).enumerate() {
                value |= input_bits[pin] << local;
            }
            values.insert(port.name.clone(), value);
        }

        let mut output_state = 0_u32;
        let mut assigned_output_bits = 0_u32;
        for assignment in &self.assignments {
            let value = assignment.value.evaluate(EvalContext {
                values: &values,
                readonly: &self.readonly,
                input_bits: &input_bits,
            })?;
            match &assignment.target {
                AssignmentTarget::Name(name) => {
                    if let Some(port) = self.outputs.iter().find(|port| port.name == *name) {
                        let masked = value & width_mask(port.width())?;
                        for (local, pin) in (port.first..=port.last).enumerate() {
                            let bit = (masked >> local) & 1;
                            output_state = (output_state & !(1_u32 << pin)) | (bit << pin);
                        }
                        values.insert(name.clone(), masked);
                        assigned_output_bits |= width_mask(port.width())? << port.first;
                    } else {
                        values.insert(name.clone(), value);
                    }
                }
                AssignmentTarget::OutputRange { first, last } => {
                    let width = last - first + 1;
                    let masked = value & width_mask(width)?;
                    for (local, pin) in (*first..=*last).enumerate() {
                        let bit = (masked >> local) & 1;
                        output_state = (output_state & !(1_u32 << pin)) | (bit << pin);
                    }
                    assigned_output_bits |= width_mask(width)? << first;
                }
            }
        }

        let required_output_bits = width_mask(self.output_width)?;
        if assigned_output_bits != required_output_bits {
            let missing = required_output_bits & !assigned_output_bits;
            bail!("SIMetrix ALB leaves output-bit mask 0x{missing:x} unassigned");
        }
        Ok(output_state & required_output_bits)
    }
}

fn width_mask(width: usize) -> Result<u32> {
    if width == 0 || width > MAX_VALUE_BITS {
        bail!("SIMetrix ALB value width {width} is outside 1..={MAX_VALUE_BITS}");
    }
    Ok(if width == 32 {
        u32::MAX
    } else {
        (1_u32 << width) - 1
    })
}

fn state_register_kind(statement: &str) -> Option<(StateRegisterKind, usize)> {
    for (keyword, kind) in [
        ("COMB", StateRegisterKind::Combinational),
        ("EDGE", StateRegisterKind::Edge),
        ("LEVEL", StateRegisterKind::Level),
    ] {
        if statement
            .get(..keyword.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(keyword))
            && statement
                .as_bytes()
                .get(keyword.len())
                .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'(')
        {
            return Some((kind, keyword.len()));
        }
    }
    None
}

fn parse_dynamic_value(value: &str) -> Result<DynamicValue> {
    let value = value.trim();
    for (prefix, constructor) in [
        ("USER", DynamicValue::User as fn(usize) -> DynamicValue),
        ("DEVICE", DynamicValue::Device as fn(usize) -> DynamicValue),
    ] {
        if value
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        {
            let suffix = value[prefix.len()..].trim();
            let index = suffix
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .with_context(|| format!("malformed SIMetrix ALB {prefix} reference {value:?}"))?
                .trim()
                .parse::<usize>()?;
            return Ok(constructor(index));
        }
    }
    Ok(DynamicValue::Literal(crate::parse_numeric(value)?))
}

fn parse_clock_pin(value: &str) -> Result<usize> {
    let value = value.trim();
    if !value
        .get(..2)
        .is_some_and(|head| head.eq_ignore_ascii_case("IN"))
    {
        bail!("SIMetrix ALB clock must reference an IN[] pin, got {value:?}");
    }
    let pin = value[2..]
        .trim()
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .with_context(|| format!("malformed SIMetrix ALB clock pin {value:?}"))?
        .trim()
        .parse::<usize>()?;
    if pin >= MAX_VALUE_BITS {
        bail!("SIMetrix ALB clock pin {pin} exceeds bounded pin index {MAX_VALUE_BITS}");
    }
    Ok(pin)
}

fn split_top_level(input: &str, delimiter: char) -> Result<Vec<&str>> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    for (index, character) in input.char_indices() {
        match character {
            '(' => parens = parens.saturating_add(1),
            ')' => {
                parens = parens
                    .checked_sub(1)
                    .context("unmatched ')' in SIMetrix ALB expression list")?;
            }
            '[' => brackets = brackets.saturating_add(1),
            ']' => {
                brackets = brackets
                    .checked_sub(1)
                    .context("unmatched ']' in SIMetrix ALB expression list")?;
            }
            _ => {}
        }
        if character == delimiter && parens == 0 && brackets == 0 {
            parts.push(input[start..index].trim());
            start = index + character.len_utf8();
        }
    }
    if parens != 0 || brackets != 0 {
        bail!("unterminated grouping in SIMetrix ALB expression list");
    }
    parts.push(input[start..].trim());
    Ok(parts)
}

fn parse_register_name_and_array(value: &str) -> Result<(String, usize)> {
    let value = value.trim();
    if let Some(open) = value.find('[') {
        let close = value
            .rfind(']')
            .context("SIMetrix ALB register array has no closing ']'")?;
        if close + 1 != value.len() || close <= open {
            bail!("malformed SIMetrix ALB register array declaration {value:?}");
        }
        let name = canonical_name(value[..open].trim())?;
        let size = value[open + 1..close].trim().parse::<usize>()?;
        if size == 0 || size > 65_536 {
            bail!("SIMetrix ALB register array size {size} is invalid");
        }
        Ok((name, size))
    } else {
        Ok((canonical_name(value)?, 1))
    }
}

fn validate_register_width(value: &DynamicValue) -> Result<()> {
    if let Some(value) = value.literal()
        && (!value.is_finite() || value.fract() != 0.0 || !(1.0..=32.0).contains(&value))
    {
        bail!("SIMetrix ALB register WIDTH={value} is outside 1..={MAX_VALUE_BITS}");
    }
    Ok(())
}

fn validate_bitwise(value: &DynamicValue) -> Result<()> {
    if let Some(value) = value.literal()
        && (value.fract() != 0.0 || !(0.0..=1.0).contains(&value))
    {
        bail!("SIMetrix ALB BITWISE must be 0 or 1, got {value}");
    }
    Ok(())
}

fn state_qualifier_allowed(kind: StateRegisterKind, name: &str) -> bool {
    match kind {
        StateRegisterKind::Combinational => matches!(name, "DELAY" | "WIDTH" | "BITWISE"),
        StateRegisterKind::Edge => matches!(
            name,
            "DELAY" | "WIDTH" | "MINCLOCK" | "HOLD" | "ASYNCDELAY" | "BITWISE"
        ),
        StateRegisterKind::Level => {
            matches!(name, "DELAY" | "WIDTH" | "SETUP" | "ASYNCDELAY" | "BITWISE")
        }
    }
}

fn parse_state_register_qualifiers(
    kind: StateRegisterKind,
    rest: &str,
) -> Result<(BTreeMap<String, DynamicValue>, Option<usize>, &str)> {
    if !rest.starts_with('(') {
        return Ok((BTreeMap::new(), None, rest));
    }
    let end = matching_paren(rest)?;
    let mut qualifiers = BTreeMap::new();
    let mut clock_pin = None;
    for qualifier in split_top_level(&rest[1..end], ',')? {
        if qualifier.is_empty() {
            continue;
        }
        let (name, value) = qualifier
            .split_once('=')
            .with_context(|| format!("malformed SIMetrix ALB register qualifier {qualifier:?}"))?;
        let name = name.trim().to_ascii_uppercase();
        if name == "CLOCK" {
            if clock_pin.replace(parse_clock_pin(value)?).is_some() {
                bail!("duplicate SIMetrix ALB CLOCK qualifier");
            }
            continue;
        }
        if !state_qualifier_allowed(kind, &name) {
            bail!("unsupported SIMetrix ALB {kind:?} qualifier {name}");
        }
        if qualifiers
            .insert(name.clone(), parse_dynamic_value(value)?)
            .is_some()
        {
            bail!("duplicate SIMetrix ALB register qualifier {name}");
        }
    }
    Ok((qualifiers, clock_pin, rest[end + 1..].trim_start()))
}

fn parse_register_initial_values(value: Option<&str>, array_size: usize) -> Result<Vec<Expr>> {
    let initial = value
        .map(|value| {
            split_top_level(value, ',')?
                .into_iter()
                .map(|value| {
                    if value.is_empty() {
                        bail!("empty SIMetrix ALB register initial condition");
                    }
                    ExprParser::new(value).parse()
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    if initial.len() > array_size {
        bail!(
            "SIMetrix ALB register has {} initial values for array size {array_size}",
            initial.len()
        );
    }
    Ok(initial)
}

fn parse_state_register(statement: &str) -> Result<Option<StateRegister>> {
    let Some((kind, keyword_len)) = state_register_kind(statement) else {
        return Ok(None);
    };
    let rest = statement[keyword_len..].trim_start();
    let (mut qualifiers, clock_pin, rest) = parse_state_register_qualifiers(kind, rest)?;
    match kind {
        StateRegisterKind::Edge | StateRegisterKind::Level if clock_pin.is_none() => {
            bail!("SIMetrix ALB {kind:?} register has no CLOCK qualifier")
        }
        StateRegisterKind::Combinational if clock_pin.is_some() => {
            bail!("SIMetrix ALB COMB register cannot have a CLOCK qualifier")
        }
        _ => {}
    }
    let (declaration, initializer) = rest
        .split_once('=')
        .map_or((rest, None), |(lhs, rhs)| (lhs.trim(), Some(rhs.trim())));
    let (name, array_size) = parse_register_name_and_array(declaration)?;
    if kind == StateRegisterKind::Combinational && array_size != 1 {
        bail!("SIMetrix ALB COMB register {name} cannot be an array");
    }
    let width = qualifiers
        .remove("WIDTH")
        .unwrap_or(DynamicValue::Literal(32.0));
    validate_register_width(&width)?;
    let bitwise = qualifiers.remove("BITWISE");
    if let Some(value) = &bitwise {
        validate_bitwise(value)?;
    }
    let initial = parse_register_initial_values(initializer, array_size)?;
    Ok(Some(StateRegister {
        kind,
        name,
        width,
        array_size,
        clock_pin,
        delay: qualifiers.remove("DELAY"),
        min_clock: qualifiers.remove("MINCLOCK"),
        hold: qualifiers.remove("HOLD"),
        setup: qualifiers.remove("SETUP"),
        async_delay: qualifiers.remove("ASYNCDELAY"),
        bitwise,
        initial,
    }))
}

fn validate_state_expression_names(
    expression: &Expr,
    defined_values: &BTreeSet<String>,
    arrays: &BTreeSet<String>,
) -> Result<()> {
    match expression {
        Expr::Constant(_) => Ok(()),
        Expr::Name(name) => {
            if defined_values.contains(name) {
                Ok(())
            } else {
                bail!("SIMetrix ALB expression references undefined value {name}")
            }
        }
        Expr::Index(name, index) => {
            if name != "IN" && name != "USER" && name != "DEVICE" && !arrays.contains(name) {
                bail!("SIMetrix ALB expression indexes undeclared array {name}");
            }
            validate_state_expression_names(index, defined_values, arrays)
        }
        Expr::Unary(_, value) => validate_state_expression_names(value, defined_values, arrays),
        Expr::Binary(_, lhs, rhs) => {
            validate_state_expression_names(lhs, defined_values, arrays)?;
            validate_state_expression_names(rhs, defined_values, arrays)
        }
        Expr::Conditional(condition, yes, no) => {
            validate_state_expression_names(condition, defined_values, arrays)?;
            validate_state_expression_names(yes, defined_values, arrays)?;
            validate_state_expression_names(no, defined_values, arrays)
        }
    }
}

fn split_async_action(value: &str) -> Result<(&str, &str)> {
    let mut parens = 0usize;
    let mut brackets = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '(' => parens = parens.saturating_add(1),
            ')' => {
                parens = parens
                    .checked_sub(1)
                    .context("unmatched ')' in SIMetrix ALB asynchronous assignment")?;
            }
            '[' => brackets = brackets.saturating_add(1),
            ']' => {
                brackets = brackets
                    .checked_sub(1)
                    .context("unmatched ']' in SIMetrix ALB asynchronous assignment")?;
            }
            '?' if parens == 0 && brackets == 0 => {
                let condition = value[..index].trim();
                let action = value[index + 1..].trim();
                if condition.is_empty() || action.is_empty() {
                    bail!("malformed SIMetrix ALB asynchronous assignment {value:?}");
                }
                return Ok((condition, action));
            }
            _ => {}
        }
    }
    bail!("SIMetrix ALB asynchronous assignment has no '?' action separator")
}

fn parse_indexed_target(value: &str) -> Result<Option<(String, Expr)>> {
    let value = value.trim();
    let Some(open) = value.find('[') else {
        return Ok(None);
    };
    let close = value
        .rfind(']')
        .context("SIMetrix ALB indexed assignment target has no closing ']'")?;
    if close + 1 != value.len() || close <= open {
        bail!("malformed SIMetrix ALB indexed assignment target {value:?}");
    }
    let name = canonical_name(value[..open].trim())?;
    let index = ExprParser::new(value[open + 1..close].trim()).parse()?;
    Ok(Some((name, index)))
}

struct StatefulDeclarations {
    inputs: Vec<Port>,
    outputs: Vec<Port>,
    readonly: BTreeMap<String, ReadOnly>,
    registers: Vec<StateRegister>,
    assignment_sources: Vec<String>,
}

fn parse_stateful_declarations(statements: Vec<String>) -> Result<StatefulDeclarations> {
    let mut declarations = StatefulDeclarations {
        inputs: Vec::new(),
        outputs: Vec::new(),
        readonly: BTreeMap::new(),
        registers: Vec::new(),
        assignment_sources: Vec::new(),
    };
    let mut saw_assignment = false;
    for statement in statements {
        let upper = statement.to_ascii_uppercase();
        if upper.starts_with("PORT ") || upper.starts_with("PORT(") {
            if saw_assignment {
                bail!("SIMetrix ALB declaration follows assignment: {statement}");
            }
            let port = parse_port(&statement)?;
            match port.kind {
                PortKind::Input => declarations.inputs.push(port),
                PortKind::Output => declarations.outputs.push(port),
            }
        } else if upper.starts_with("READONLY ") || upper.starts_with("READONLY(") {
            if saw_assignment {
                bail!("SIMetrix ALB declaration follows assignment: {statement}");
            }
            let (name, register) = parse_readonly(&statement)?;
            if declarations
                .readonly
                .insert(name.clone(), register)
                .is_some()
            {
                bail!("duplicate SIMetrix ALB READONLY register {name}");
            }
        } else if let Some(register) = parse_state_register(&statement)? {
            if saw_assignment {
                bail!("SIMetrix ALB declaration follows assignment: {statement}");
            }
            declarations.registers.push(register);
        } else {
            saw_assignment = true;
            declarations.assignment_sources.push(statement);
        }
    }
    Ok(declarations)
}

type RegisterIndex = BTreeMap<String, (StateRegisterKind, usize)>;

struct StateNamespace {
    input_names: BTreeSet<String>,
    output_names: BTreeSet<String>,
    readonly_names: BTreeSet<String>,
    arrays: BTreeSet<String>,
    register_by_name: RegisterIndex,
    defined_values: BTreeSet<String>,
}

impl StateNamespace {
    fn new(declarations: &StatefulDeclarations) -> Result<Self> {
        let input_names = declarations
            .inputs
            .iter()
            .map(|port| port.name.clone())
            .collect::<BTreeSet<_>>();
        let output_names = declarations
            .outputs
            .iter()
            .map(|port| port.name.clone())
            .collect::<BTreeSet<_>>();
        let readonly_names = declarations
            .readonly
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let register_by_name = declarations
            .registers
            .iter()
            .map(|register| (register.name.clone(), (register.kind, register.array_size)))
            .collect::<RegisterIndex>();
        if register_by_name.len() != declarations.registers.len() {
            bail!("duplicate SIMetrix ALB state-register name");
        }
        validate_state_declaration_names(
            &input_names,
            &output_names,
            &readonly_names,
            &register_by_name,
        )?;
        let mut arrays = declarations
            .readonly
            .iter()
            .filter(|(_, register)| register.values.len() > 1)
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        arrays.extend(
            declarations
                .registers
                .iter()
                .filter(|register| register.array_size > 1)
                .map(|register| register.name.clone()),
        );
        let mut defined_values = input_names.clone();
        defined_values.extend(
            declarations
                .readonly
                .iter()
                .filter(|(_, register)| register.values.len() == 1)
                .map(|(name, _)| name.clone()),
        );
        defined_values.extend(
            declarations
                .registers
                .iter()
                .filter(|register| register.array_size == 1)
                .map(|register| register.name.clone()),
        );
        Ok(Self {
            input_names,
            output_names,
            readonly_names,
            arrays,
            register_by_name,
            defined_values,
        })
    }
}

fn validate_state_declaration_names(
    inputs: &BTreeSet<String>,
    outputs: &BTreeSet<String>,
    readonly: &BTreeSet<String>,
    registers: &RegisterIndex,
) -> Result<()> {
    let mut names = BTreeSet::new();
    for name in inputs
        .iter()
        .chain(outputs)
        .chain(readonly)
        .chain(registers.keys())
    {
        if !names.insert(name) {
            bail!("SIMetrix ALB reuses identifier {name} across incompatible declarations");
        }
    }
    Ok(())
}

fn validate_state_initializers(
    registers: &[StateRegister],
    namespace: &StateNamespace,
) -> Result<()> {
    for register in registers {
        for initial in &register.initial {
            validate_state_expression_names(initial, &namespace.defined_values, &namespace.arrays)?;
        }
    }
    Ok(())
}

struct ParsedStateAssignment {
    assignment: StateAssignment,
    input_width: usize,
    output_width: usize,
    defines: Option<String>,
}

fn parse_async_state_assignment(
    statement: &str,
    namespace: &StateNamespace,
) -> Result<Option<ParsedStateAssignment>> {
    let Some((lhs, rhs)) = statement.split_once(":=") else {
        return Ok(None);
    };
    let name = canonical_name(lhs.trim())?;
    let Some((kind, array_size)) = namespace.register_by_name.get(&name).copied() else {
        bail!("SIMetrix ALB asynchronous assignment targets undeclared register {name}");
    };
    if array_size != 1 || kind == StateRegisterKind::Combinational {
        bail!(
            "SIMetrix ALB asynchronous assignment target {name} is not a scalar EDGE/LEVEL register"
        );
    }
    let (condition, action) = split_async_action(rhs)?;
    let condition = ExprParser::new(condition).parse()?;
    let value = ExprParser::new(action).parse()?;
    validate_state_expression_names(&condition, &namespace.defined_values, &namespace.arrays)?;
    validate_state_expression_names(&value, &namespace.defined_values, &namespace.arrays)?;
    let input_width = condition
        .direct_input_width()?
        .max(value.direct_input_width()?);
    Ok(Some(ParsedStateAssignment {
        assignment: StateAssignment {
            target: StateAssignmentTarget::Register(name),
            value: StateAssignmentValue::Asynchronous { condition, value },
        },
        input_width,
        output_width: 0,
        defines: None,
    }))
}

fn parse_sync_state_target(
    lhs: &str,
    namespace: &StateNamespace,
) -> Result<(StateAssignmentTarget, usize, usize, Option<String>)> {
    if let Some(port_name) = output_enable_name(lhs) {
        let name = canonical_name(port_name)?;
        if !namespace.output_names.contains(&name) {
            bail!("SIMetrix ALB output-enable assignment targets unknown output {name}");
        }
        return Ok((StateAssignmentTarget::OutputEnable(name), 0, 0, None));
    }
    if lhs
        .get(..3)
        .is_some_and(|head| head.eq_ignore_ascii_case("OUT"))
        && lhs.as_bytes().get(3) == Some(&b'[')
    {
        let (_, first, last) = parse_pin_range(lhs)?;
        return Ok((
            StateAssignmentTarget::OutputRange { first, last },
            0,
            last + 1,
            None,
        ));
    }
    if let Some((name, index)) = parse_indexed_target(lhs)? {
        let Some((kind, array_size)) = namespace.register_by_name.get(&name).copied() else {
            bail!("SIMetrix ALB indexed assignment targets undeclared register array {name}");
        };
        if array_size == 1 || kind == StateRegisterKind::Combinational {
            bail!("SIMetrix ALB indexed assignment target {name} is not an EDGE/LEVEL array");
        }
        validate_state_expression_names(&index, &namespace.defined_values, &namespace.arrays)?;
        let input_width = index.direct_input_width()?;
        return Ok((
            StateAssignmentTarget::RegisterIndex { name, index },
            input_width,
            0,
            None,
        ));
    }
    let name = canonical_name(lhs)?;
    if namespace.register_by_name.contains_key(&name) {
        Ok((StateAssignmentTarget::Register(name), 0, 0, None))
    } else if namespace.output_names.contains(&name) {
        Ok((
            StateAssignmentTarget::Output(name.clone()),
            0,
            0,
            Some(name),
        ))
    } else if namespace.input_names.contains(&name) || namespace.readonly_names.contains(&name) {
        bail!("SIMetrix ALB assignment attempts to modify read-only/input value {name}");
    } else {
        Ok((StateAssignmentTarget::Local(name.clone()), 0, 0, Some(name)))
    }
}

fn parse_sync_state_assignment(
    statement: &str,
    namespace: &StateNamespace,
) -> Result<ParsedStateAssignment> {
    let (lhs, rhs) = statement
        .split_once('=')
        .with_context(|| format!("malformed SIMetrix ALB assignment {statement:?}"))?;
    let value = ExprParser::new(rhs.trim()).parse()?;
    validate_state_expression_names(&value, &namespace.defined_values, &namespace.arrays)?;
    let value_input_width = value.direct_input_width()?;
    let (target, target_input_width, output_width, defines) =
        parse_sync_state_target(lhs.trim(), namespace)?;
    Ok(ParsedStateAssignment {
        assignment: StateAssignment {
            target,
            value: StateAssignmentValue::Synchronous(value),
        },
        input_width: value_input_width.max(target_input_width),
        output_width,
        defines,
    })
}

fn parse_stateful_assignments(
    sources: Vec<String>,
    namespace: &mut StateNamespace,
) -> Result<(Vec<StateAssignment>, usize, usize)> {
    let mut assignments = Vec::with_capacity(sources.len());
    let mut input_width = 0usize;
    let mut output_width = 0usize;
    for statement in sources {
        let parsed = if let Some(parsed) = parse_async_state_assignment(&statement, namespace)? {
            parsed
        } else {
            parse_sync_state_assignment(&statement, namespace)?
        };
        input_width = input_width.max(parsed.input_width);
        output_width = output_width.max(parsed.output_width);
        if let Some(name) = parsed.defines {
            namespace.defined_values.insert(name);
        }
        assignments.push(parsed.assignment);
    }
    Ok((assignments, input_width, output_width))
}

fn parse_stateful_program_statements(statements: Vec<String>) -> Result<StatefulProgram> {
    let declarations = parse_stateful_declarations(statements)?;
    let declared_input_width = validate_ports(&declarations.inputs, PortKind::Input)?;
    let declared_output_width = validate_ports(&declarations.outputs, PortKind::Output)?;
    let mut namespace = StateNamespace::new(&declarations)?;
    validate_state_initializers(&declarations.registers, &namespace)?;
    let (assignments, direct_input_width, direct_output_width) =
        parse_stateful_assignments(declarations.assignment_sources, &mut namespace)?;
    // EDGE/LEVEL CLOCK=in[n] is a physical input pin even if the source
    // never declares a named PORT for it or references it in an expression.
    // Ignoring that pin silently truncates the ALB instance's input signature.
    let clock_input_width = declarations
        .registers
        .iter()
        .filter_map(|register| register.clock_pin)
        .map(|pin| pin + 1)
        .max()
        .unwrap_or(0);
    let input_width = declared_input_width
        .max(direct_input_width)
        .max(clock_input_width);
    let output_width = declared_output_width.max(direct_output_width);
    if input_width == 0 || output_width == 0 {
        bail!("SIMetrix ALB structural analysis requires at least one input and one output");
    }
    Ok(StatefulProgram {
        inputs: declarations.inputs,
        outputs: declarations.outputs,
        readonly: declarations.readonly,
        registers: declarations.registers,
        assignments,
        input_width,
        output_width,
    })
}

fn output_enable_name(lhs: &str) -> Option<&str> {
    lhs.get(..lhs.len().saturating_sub(3))
        .filter(|_| {
            lhs.len() >= 3
                && lhs
                    .get(lhs.len() - 3..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".EN"))
        })
        .map(str::trim)
}

fn body_has_alb_syntax(body: &str) -> bool {
    body.lines().any(|line| {
        let line = line
            .split_once("//")
            .map_or(line, |(head, _)| head)
            .trim_start();
        if line.is_empty() || line.starts_with('*') {
            return false;
        }
        let upper = line.to_ascii_uppercase();
        upper.starts_with("PORT ")
            || upper.starts_with("PORT(")
            || ["COMB", "EDGE", "LEVEL"].iter().any(|kind| {
                upper.starts_with(kind)
                    && upper
                        .as_bytes()
                        .get(kind.len())
                        .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'(')
            })
            || line.contains(":=")
            || line.split_once('=').is_some_and(|(lhs, _)| {
                let lhs = lhs.trim();
                let lhs_upper = lhs.to_ascii_uppercase();
                output_enable_name(lhs).is_some()
                    || (lhs_upper.starts_with("OUT[") && lhs.ends_with(']'))
            })
    })
}

pub fn analyze_body(body: &str) -> Result<BodyAnalysis> {
    if !body_has_alb_syntax(body) {
        return Ok(BodyAnalysis {
            known_truth_table: None,
            stateful: None,
        });
    }
    let statements = alb_statements(body)?;
    let stateful = statements.iter().any(|statement| {
        state_register_kind(statement).is_some()
            || statement.contains(":=")
            || statement
                .split_once('=')
                .is_some_and(|(lhs, _)| lhs.trim().to_ascii_uppercase().ends_with(".EN"))
    });
    if stateful {
        return Ok(BodyAnalysis {
            known_truth_table: None,
            stateful: Some(parse_stateful_program_statements(statements)?),
        });
    }
    let has_interface = statements.iter().any(|statement| {
        let upper = statement.to_ascii_uppercase();
        upper.starts_with("PORT ")
            || upper.starts_with("PORT(")
            || statement.split_once('=').is_some_and(|(lhs, _)| {
                let lhs = lhs.trim();
                lhs.get(..3)
                    .is_some_and(|head| head.eq_ignore_ascii_case("OUT"))
                    && lhs.as_bytes().get(3) == Some(&b'[')
            })
    });
    if !has_interface {
        return Ok(BodyAnalysis {
            known_truth_table: None,
            stateful: None,
        });
    }
    Ok(BodyAnalysis {
        known_truth_table: Some(parse_program_statements(statements)?.known_truth_table()?),
        stateful: None,
    })
}

#[cfg(test)]
fn compile_known_truth_table(body: &str) -> Result<KnownTruthTable> {
    analyze_body(body)?
        .known_truth_table
        .context("expected stateless SIMetrix ALB")
}

fn parse_program_statements(statements: Vec<String>) -> Result<Program> {
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut readonly = BTreeMap::new();
    let mut assignment_sources = Vec::new();
    let mut saw_assignment = false;

    for statement in statements {
        let upper = statement.to_ascii_uppercase();
        if upper.starts_with("PORT ") || upper.starts_with("PORT(") {
            if saw_assignment {
                bail!("SIMetrix ALB declaration follows assignment: {statement}");
            }
            let port = parse_port(&statement)?;
            match port.kind {
                PortKind::Input => inputs.push(port),
                PortKind::Output => outputs.push(port),
            }
            continue;
        }
        if upper.starts_with("READONLY ") || upper.starts_with("READONLY(") {
            if saw_assignment {
                bail!("SIMetrix ALB declaration follows assignment: {statement}");
            }
            let (name, register) = parse_readonly(&statement)?;
            if readonly.insert(name.clone(), register).is_some() {
                bail!("duplicate SIMetrix ALB READONLY register {name}");
            }
            continue;
        }
        if ["COMB", "EDGE", "LEVEL"].iter().any(|kind| {
            upper.starts_with(kind)
                && upper
                    .as_bytes()
                    .get(kind.len())
                    .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'(')
        }) {
            bail!(
                "stateful or delayed SIMetrix ALB declaration is not in the stateless lowering subset: {statement}"
            );
        }
        if statement.contains(":=") {
            bail!(
                "asynchronous SIMetrix ALB assignment is not in the stateless lowering subset: {statement}"
            );
        }
        saw_assignment = true;
        assignment_sources.push(statement);
    }

    let declared_input_width = validate_ports(&inputs, PortKind::Input)?;
    let declared_output_width = validate_ports(&outputs, PortKind::Output)?;
    let input_names = inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect::<BTreeSet<_>>();
    validate_distinct_declaration_names(&inputs, &outputs, &readonly)?;
    let (assignments, direct_input_width, direct_output_width) =
        parse_assignment_sources(assignment_sources, &input_names, &readonly)?;
    let input_width = declared_input_width.max(direct_input_width);
    let output_width = declared_output_width.max(direct_output_width);
    if input_width == 0 || output_width == 0 {
        bail!("SIMetrix ALB stateless lowering requires at least one input and one output");
    }

    Ok(Program {
        inputs,
        outputs,
        readonly,
        assignments,
        input_width,
        output_width,
    })
}

fn parse_assignment_sources(
    assignment_sources: Vec<String>,
    input_names: &BTreeSet<&str>,
    readonly: &BTreeMap<String, ReadOnly>,
) -> Result<(Vec<Assignment>, usize, usize)> {
    let mut assignments = Vec::with_capacity(assignment_sources.len());
    let readonly_names = readonly.keys().map(String::as_str).collect::<BTreeSet<_>>();
    let mut defined_values = input_names
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    defined_values.extend(
        readonly
            .iter()
            .filter(|(_, register)| register.values.len() == 1)
            .map(|(name, _)| name.clone()),
    );
    let mut direct_input_width = 0usize;
    let mut direct_output_width = 0usize;
    for statement in assignment_sources {
        let (lhs, rhs) = statement
            .split_once('=')
            .with_context(|| format!("malformed SIMetrix ALB assignment {statement:?}"))?;
        let target = parse_assignment_target(lhs.trim())?;
        if let AssignmentTarget::Name(name) = &target
            && (input_names.contains(name.as_str()) || readonly_names.contains(name.as_str()))
        {
            bail!("SIMetrix ALB assignment attempts to modify read-only/input value {name}");
        }
        let value = ExprParser::new(rhs.trim()).parse()?;
        validate_expression_names(&value, &defined_values, &readonly_names)?;
        direct_input_width = direct_input_width.max(value.direct_input_width()?);
        if let AssignmentTarget::OutputRange { last, .. } = target {
            direct_output_width = direct_output_width.max(last + 1);
        }
        if let AssignmentTarget::Name(name) = &target {
            defined_values.insert(name.clone());
        }
        assignments.push(Assignment { target, value });
    }
    Ok((assignments, direct_input_width, direct_output_width))
}

fn validate_distinct_declaration_names(
    inputs: &[Port],
    outputs: &[Port],
    readonly: &BTreeMap<String, ReadOnly>,
) -> Result<()> {
    let input_names = inputs
        .iter()
        .map(|port| port.name.as_str())
        .collect::<BTreeSet<_>>();
    let output_names = outputs
        .iter()
        .map(|port| port.name.as_str())
        .collect::<BTreeSet<_>>();
    let readonly_names = readonly.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if input_names.intersection(&output_names).next().is_some()
        || input_names.intersection(&readonly_names).next().is_some()
        || output_names.intersection(&readonly_names).next().is_some()
    {
        bail!("SIMetrix ALB reuses an identifier across incompatible declarations");
    }
    Ok(())
}

fn alb_statements(body: &str) -> Result<Vec<String>> {
    let mut cleaned = String::with_capacity(body.len());
    for line in body.lines() {
        if line.trim_start().starts_with('*') {
            cleaned.push('\n');
            continue;
        }
        let content = line.split_once("//").map_or(line, |(head, _)| head);
        cleaned.push_str(content);
        cleaned.push('\n');
    }
    let mut statements = Vec::new();
    let mut rest = cleaned.as_str();
    while let Some(index) = rest.find(';') {
        let statement = rest[..index].trim();
        if !statement.is_empty() {
            statements.push(statement.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        rest = &rest[index + 1..];
    }
    if !rest.trim().is_empty() {
        bail!("unterminated SIMetrix ALB statement: {:?}", rest.trim());
    }
    Ok(statements)
}

fn parse_port(statement: &str) -> Result<Port> {
    let mut rest = statement
        .get(4..)
        .context("malformed SIMetrix ALB PORT statement")?
        .trim_start();
    let mut delay = None;
    if rest.starts_with('(') {
        let end = matching_paren(rest)?;
        let qualifiers = &rest[1..end];
        delay = parse_delay_qualifier(qualifiers)?;
        rest = rest[end + 1..].trim_start();
    }
    let mut fields = rest.split_whitespace();
    let name = canonical_name(fields.next().context("SIMetrix ALB PORT has no name")?)?;
    let pins = fields
        .next()
        .context("SIMetrix ALB PORT has no pin mapping")?;
    if fields.next().is_some() {
        bail!("unexpected tokens in SIMetrix ALB PORT statement {statement:?}");
    }
    let (kind, first, last) = parse_pin_range(pins)?;
    if kind == PortKind::Input && delay.is_some() {
        bail!("SIMetrix ALB input PORT cannot have an output delay");
    }
    Ok(Port {
        name,
        kind,
        first,
        last,
        delay,
    })
}

fn matching_paren(input: &str) -> Result<usize> {
    let mut depth = 0usize;
    for (index, byte) in input.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth
                    .checked_sub(1)
                    .context("unmatched SIMetrix ALB ')' ")?;
                if depth == 0 {
                    return Ok(index);
                }
            }
            _ => {}
        }
    }
    bail!("unterminated SIMetrix ALB qualifier list")
}

fn parse_delay_qualifier(input: &str) -> Result<Option<DynamicValue>> {
    let mut delay = None;
    for qualifier in input.split(',') {
        let qualifier = qualifier.trim();
        if qualifier.is_empty() {
            continue;
        }
        let (name, value) = qualifier
            .split_once('=')
            .with_context(|| format!("malformed SIMetrix ALB qualifier {qualifier:?}"))?;
        if !name.trim().eq_ignore_ascii_case("DELAY") {
            bail!("unsupported SIMetrix ALB PORT qualifier {name}");
        }
        if delay.replace(parse_dynamic_value(value.trim())?).is_some() {
            bail!("duplicate SIMetrix ALB PORT DELAY qualifier");
        }
    }
    Ok(delay)
}

fn parse_pin_range(pins: &str) -> Result<(PortKind, usize, usize)> {
    let open = pins
        .find('[')
        .context("SIMetrix ALB pin mapping has no '['")?;
    let close = pins
        .rfind(']')
        .context("SIMetrix ALB pin mapping has no ']'")?;
    if close + 1 != pins.len() || close <= open {
        bail!("malformed SIMetrix ALB pin mapping {pins:?}");
    }
    let kind = match pins[..open].to_ascii_uppercase().as_str() {
        "IN" => PortKind::Input,
        "OUT" => PortKind::Output,
        other => bail!("unsupported SIMetrix ALB pin direction {other}"),
    };
    let range = &pins[open + 1..close];
    let (first, last) = if let Some((first, last)) = range.split_once(':') {
        (
            first.trim().parse::<usize>()?,
            last.trim().parse::<usize>()?,
        )
    } else {
        let pin = range.trim().parse::<usize>()?;
        (pin, pin)
    };
    if first > last || last >= MAX_VALUE_BITS {
        bail!("SIMetrix ALB pin range {first}:{last} is invalid");
    }
    Ok((kind, first, last))
}

fn parse_readonly(statement: &str) -> Result<(String, ReadOnly)> {
    let mut rest = statement
        .get(8..)
        .context("malformed SIMetrix ALB READONLY statement")?
        .trim_start();
    let mut width = MAX_VALUE_BITS;
    let mut saw_width = false;
    if rest.starts_with('(') {
        let end = matching_paren(rest)?;
        let qualifiers = &rest[1..end];
        for qualifier in qualifiers.split(',') {
            let qualifier = qualifier.trim();
            if qualifier.is_empty() {
                continue;
            }
            let (name, value) = qualifier.split_once('=').with_context(|| {
                format!("malformed SIMetrix ALB READONLY qualifier {qualifier:?}")
            })?;
            if !name.trim().eq_ignore_ascii_case("WIDTH") {
                bail!("unsupported SIMetrix ALB READONLY qualifier {name}");
            }
            if saw_width {
                bail!("duplicate SIMetrix ALB READONLY WIDTH qualifier");
            }
            width = value.trim().parse::<usize>()?;
            saw_width = true;
        }
        rest = rest[end + 1..].trim_start();
    }
    let mask = width_mask(width)?;
    let (lhs, rhs) = rest
        .split_once('=')
        .map_or_else(|| (rest.trim(), None), |(lhs, rhs)| (lhs.trim(), Some(rhs)));
    let (name, count) = if let Some(open) = lhs.find('[') {
        let close = lhs
            .rfind(']')
            .context("SIMetrix ALB READONLY register has no closing ']'")?;
        if close + 1 != lhs.len() || close <= open {
            bail!("malformed SIMetrix ALB READONLY declaration {lhs:?}");
        }
        let name = canonical_name(lhs[..open].trim())?;
        let count = lhs[open + 1..close].trim().parse::<usize>()?;
        if count == 0 || count > 65_536 {
            bail!("SIMetrix ALB READONLY array size {count} is invalid");
        }
        (name, count)
    } else {
        (canonical_name(lhs)?, 1)
    };
    let values = if let Some(rhs) = rhs {
        let values = rhs
            .split(|character: char| character == ',' || character.is_whitespace())
            .filter(|value| !value.is_empty())
            .map(parse_u32_constant)
            .collect::<Result<Vec<_>>>()?;
        if values.len() != count {
            bail!(
                "SIMetrix ALB READONLY {name} declares {count} values but initializes {}",
                values.len()
            );
        }
        values
    } else {
        vec![0; count]
    };
    Ok((
        name,
        ReadOnly {
            values: values.into_iter().map(|value| value & mask).collect(),
        },
    ))
}

fn validate_ports(ports: &[Port], kind: PortKind) -> Result<usize> {
    if ports.is_empty() {
        return Ok(0);
    }
    let width = ports.iter().map(|port| port.last).max().unwrap_or(0) + 1;
    if width > MAX_VALUE_BITS {
        bail!("SIMetrix ALB port width {width} exceeds {MAX_VALUE_BITS} bits");
    }
    let mut owners = vec![None::<&str>; width];
    let mut names = BTreeSet::new();
    for port in ports {
        if port.kind != kind {
            bail!("internal SIMetrix ALB port-kind mismatch");
        }
        if !names.insert(port.name.as_str()) {
            bail!("duplicate SIMetrix ALB PORT name {}", port.name);
        }
        for (pin, owner) in owners
            .iter_mut()
            .enumerate()
            .take(port.last + 1)
            .skip(port.first)
        {
            if let Some(existing) = owner.replace(port.name.as_str()) {
                bail!(
                    "SIMetrix ALB pin {pin} is assigned to both {existing} and {}",
                    port.name
                );
            }
        }
    }
    Ok(width)
}

fn parse_assignment_target(target: &str) -> Result<AssignmentTarget> {
    if target
        .get(..3)
        .is_some_and(|head| head.eq_ignore_ascii_case("OUT"))
        && target.as_bytes().get(3) == Some(&b'[')
    {
        let (_, first, last) = parse_pin_range(target)?;
        return Ok(AssignmentTarget::OutputRange { first, last });
    }
    if target.contains('.') || target.contains('[') || target.contains(']') {
        bail!("unsupported stateless SIMetrix ALB assignment target {target:?}");
    }
    Ok(AssignmentTarget::Name(canonical_name(target)?))
}

fn canonical_name(name: &str) -> Result<String> {
    let mut bytes = name.bytes();
    let first = bytes.next().context("empty SIMetrix ALB identifier")?;
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid SIMetrix ALB identifier {name:?}");
    }
    Ok(name.to_ascii_uppercase())
}

#[derive(Clone, Debug)]
enum Expr {
    Constant(u32),
    Name(String),
    Index(String, Box<Self>),
    Unary(UnaryOp, Box<Self>),
    Binary(BinaryOp, Box<Self>, Box<Self>),
    Conditional(Box<Self>, Box<Self>, Box<Self>),
}

impl Expr {
    fn direct_input_width(&self) -> Result<usize> {
        match self {
            Self::Constant(_) | Self::Name(_) => Ok(0),
            Self::Index(name, index) if name == "IN" => {
                let Self::Constant(index) = index.as_ref() else {
                    bail!("SIMetrix ALB direct IN[] reference requires a constant pin index");
                };
                Ok(usize::try_from(*index)?.saturating_add(1))
            }
            Self::Index(_, index) | Self::Unary(_, index) => index.direct_input_width(),
            Self::Binary(_, lhs, rhs) => {
                Ok(lhs.direct_input_width()?.max(rhs.direct_input_width()?))
            }
            Self::Conditional(condition, yes, no) => Ok(condition
                .direct_input_width()?
                .max(yes.direct_input_width()?)
                .max(no.direct_input_width()?)),
        }
    }

    fn evaluate(&self, context: EvalContext<'_>) -> Result<u32> {
        match self {
            Self::Constant(value) => Ok(*value),
            Self::Name(name) => context
                .values
                .get(name)
                .copied()
                .or_else(|| {
                    context
                        .readonly
                        .get(name)
                        .filter(|register| register.values.len() == 1)
                        .and_then(|register| register.values.first().copied())
                })
                .with_context(|| {
                    format!("SIMetrix ALB expression references undefined value {name}")
                }),
            Self::Index(name, index) => {
                let index = usize::try_from(index.evaluate(context)?)?;
                if name == "IN" {
                    return context.input_bits.get(index).copied().with_context(|| {
                        format!("SIMetrix ALB input index {index} is out of range")
                    });
                }
                let register = context
                    .readonly
                    .get(name)
                    .with_context(|| format!("SIMetrix ALB expression indexes non-array {name}"))?;
                register.values.get(index).copied().with_context(|| {
                    format!("SIMetrix ALB READONLY {name} index {index} is out of range")
                })
            }
            Self::Unary(operation, value) => {
                let value = value.evaluate(context)?;
                Ok(match operation {
                    UnaryOp::Plus => value,
                    UnaryOp::Minus => value.wrapping_neg(),
                    UnaryOp::LogicalNot => u32::from(value == 0),
                    UnaryOp::BitwiseNot => !value,
                })
            }
            Self::Binary(BinaryOp::LogicalAnd, lhs, rhs) => {
                let lhs = lhs.evaluate(context)?;
                if lhs == 0 {
                    Ok(0)
                } else {
                    Ok(u32::from(rhs.evaluate(context)? != 0))
                }
            }
            Self::Binary(BinaryOp::LogicalOr, lhs, rhs) => {
                let lhs = lhs.evaluate(context)?;
                if lhs != 0 {
                    Ok(1)
                } else {
                    Ok(u32::from(rhs.evaluate(context)? != 0))
                }
            }
            Self::Binary(operation, lhs, rhs) => {
                let lhs = lhs.evaluate(context)?;
                let rhs = rhs.evaluate(context)?;
                evaluate_binary(*operation, lhs, rhs)
            }
            Self::Conditional(condition, yes, no) => {
                if condition.evaluate(context)? != 0 {
                    yes.evaluate(context)
                } else {
                    no.evaluate(context)
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct EvalContext<'a> {
    values: &'a BTreeMap<String, u32>,
    readonly: &'a BTreeMap<String, ReadOnly>,
    input_bits: &'a [u32],
}

#[derive(Clone, Copy, Debug)]
enum UnaryOp {
    Plus,
    Minus,
    LogicalNot,
    BitwiseNot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BinaryOp {
    Multiply,
    Divide,
    Modulo,
    Add,
    Subtract,
    ShiftLeft,
    ShiftRight,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    BitAnd,
    BitXor,
    BitOr,
    LogicalAnd,
    LogicalOr,
}

fn evaluate_binary(operation: BinaryOp, lhs: u32, rhs: u32) -> Result<u32> {
    Ok(match operation {
        BinaryOp::Multiply => lhs.wrapping_mul(rhs),
        BinaryOp::Divide => {
            if rhs == 0 {
                bail!("division by zero in SIMetrix ALB expression");
            }
            lhs / rhs
        }
        BinaryOp::Modulo => {
            if rhs == 0 {
                bail!("modulo by zero in SIMetrix ALB expression");
            }
            lhs % rhs
        }
        BinaryOp::Add => lhs.wrapping_add(rhs),
        BinaryOp::Subtract => lhs.wrapping_sub(rhs),
        BinaryOp::ShiftLeft => {
            if rhs >= 32 {
                bail!("SIMetrix ALB left shift by {rhs} is outside 0..31");
            }
            lhs << rhs
        }
        BinaryOp::ShiftRight => {
            if rhs >= 32 {
                bail!("SIMetrix ALB right shift by {rhs} is outside 0..31");
            }
            lhs >> rhs
        }
        BinaryOp::Less => u32::from(lhs < rhs),
        BinaryOp::Greater => u32::from(lhs > rhs),
        BinaryOp::LessEqual => u32::from(lhs <= rhs),
        BinaryOp::GreaterEqual => u32::from(lhs >= rhs),
        BinaryOp::Equal => u32::from(lhs == rhs),
        BinaryOp::NotEqual => u32::from(lhs != rhs),
        BinaryOp::BitAnd => lhs & rhs,
        BinaryOp::BitXor => lhs ^ rhs,
        BinaryOp::BitOr => lhs | rhs,
        BinaryOp::LogicalAnd | BinaryOp::LogicalOr => unreachable!("short-circuited above"),
    })
}

fn validate_expression_names(
    expression: &Expr,
    defined_values: &BTreeSet<String>,
    readonly: &BTreeSet<&str>,
) -> Result<()> {
    match expression {
        Expr::Constant(_) => Ok(()),
        Expr::Name(name) => {
            if defined_values.contains(name) {
                Ok(())
            } else {
                bail!("SIMetrix ALB expression references undefined value {name}")
            }
        }
        Expr::Index(name, index) => {
            if name != "IN" && !readonly.contains(name.as_str()) {
                bail!("SIMetrix ALB expression indexes undeclared array {name}");
            }
            validate_expression_names(index, defined_values, readonly)
        }
        Expr::Unary(_, value) => validate_expression_names(value, defined_values, readonly),
        Expr::Binary(_, lhs, rhs) => {
            validate_expression_names(lhs, defined_values, readonly)?;
            validate_expression_names(rhs, defined_values, readonly)
        }
        Expr::Conditional(condition, yes, no) => {
            validate_expression_names(condition, defined_values, readonly)?;
            validate_expression_names(yes, defined_values, readonly)?;
            validate_expression_names(no, defined_values, readonly)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(u32),
    Identifier(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LogicalNot,
    BitwiseNot,
    ShiftLeft,
    ShiftRight,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    BitAnd,
    BitXor,
    BitOr,
    LogicalAnd,
    LogicalOr,
    Question,
    Colon,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    End,
}

struct Lexer<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Lexer<'a> {
    const fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            offset: 0,
        }
    }

    fn next(&mut self) -> Result<Token> {
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.offset += 1;
        }
        let Some(byte) = self.input.get(self.offset).copied() else {
            return Ok(Token::End);
        };
        let rest = &self.input[self.offset..];
        for (text, token) in [
            (b"&&".as_slice(), Token::LogicalAnd),
            (b"||".as_slice(), Token::LogicalOr),
            (b"<<".as_slice(), Token::ShiftLeft),
            (b">>".as_slice(), Token::ShiftRight),
            (b"<=".as_slice(), Token::LessEqual),
            (b">=".as_slice(), Token::GreaterEqual),
            (b"==".as_slice(), Token::Equal),
            (b"!=".as_slice(), Token::NotEqual),
            (b"<>".as_slice(), Token::NotEqual),
        ] {
            if rest.starts_with(text) {
                self.offset += text.len();
                return Ok(token);
            }
        }
        let token = match byte {
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Percent,
            b'!' => Token::LogicalNot,
            b'~' => Token::BitwiseNot,
            b'<' => Token::Less,
            b'>' => Token::Greater,
            b'&' => Token::BitAnd,
            b'^' => Token::BitXor,
            b'|' => Token::BitOr,
            b'?' => Token::Question,
            b':' => Token::Colon,
            b'(' => Token::LeftParen,
            b')' => Token::RightParen,
            b'[' => Token::LeftBracket,
            b']' => Token::RightBracket,
            byte if byte.is_ascii_digit() => return self.number(),
            byte if byte.is_ascii_alphabetic() || byte == b'_' => return self.identifier(),
            _ => bail!(
                "unsupported character {:?} in SIMetrix ALB expression",
                char::from(byte)
            ),
        };
        self.offset += 1;
        Ok(token)
    }

    fn number(&mut self) -> Result<Token> {
        let start = self.offset;
        if self.input.get(self.offset) == Some(&b'0')
            && self
                .input
                .get(self.offset + 1)
                .is_some_and(|byte| matches!(*byte, b'x' | b'X'))
        {
            self.offset += 2;
            let digits = self.offset;
            while self
                .input
                .get(self.offset)
                .is_some_and(u8::is_ascii_hexdigit)
            {
                self.offset += 1;
            }
            if self.offset == digits {
                bail!("empty hexadecimal constant in SIMetrix ALB expression");
            }
            let text = std::str::from_utf8(&self.input[digits..self.offset])?;
            return Ok(Token::Number(u32::from_str_radix(text, 16)?));
        }
        while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
            self.offset += 1;
        }
        if self.input.get(self.offset) == Some(&b'.') {
            self.offset += 1;
            while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
                self.offset += 1;
            }
        }
        if self
            .input
            .get(self.offset)
            .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
        {
            self.offset += 1;
            if self
                .input
                .get(self.offset)
                .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
            {
                self.offset += 1;
            }
            let exponent_start = self.offset;
            while self.input.get(self.offset).is_some_and(u8::is_ascii_digit) {
                self.offset += 1;
            }
            if self.offset == exponent_start {
                bail!("invalid exponent in SIMetrix ALB numeric constant");
            }
        }
        while self
            .input
            .get(self.offset)
            .is_some_and(u8::is_ascii_alphabetic)
        {
            self.offset += 1;
        }
        let text = std::str::from_utf8(&self.input[start..self.offset])?;
        let value = crate::parse_numeric(text)
            .with_context(|| format!("parsing SIMetrix ALB numeric constant {text:?}"))?;
        if !value.is_finite() || value < 0.0 || value > f64::from(u32::MAX) || value.fract() != 0.0
        {
            bail!("SIMetrix ALB numeric constant {text:?} is not an exact unsigned 32-bit integer");
        }
        Ok(Token::Number(value.to_string().parse::<u32>()?))
    }

    fn identifier(&mut self) -> Result<Token> {
        let start = self.offset;
        self.offset += 1;
        while self
            .input
            .get(self.offset)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.offset += 1;
        }
        let text = std::str::from_utf8(&self.input[start..self.offset])?;
        Ok(Token::Identifier(text.to_ascii_uppercase()))
    }
}

struct ExprParser<'a> {
    lexer: Lexer<'a>,
    lookahead: Token,
}

impl<'a> ExprParser<'a> {
    const fn new(input: &'a str) -> Self {
        Self {
            lexer: Lexer::new(input),
            lookahead: Token::End,
        }
    }

    fn parse(mut self) -> Result<Expr> {
        self.lookahead = self.lexer.next()?;
        let expression = self.conditional()?;
        if self.lookahead != Token::End {
            bail!(
                "unexpected token {:?} after SIMetrix ALB expression",
                self.lookahead
            );
        }
        Ok(expression)
    }

    fn bump(&mut self) -> Result<Token> {
        let current = std::mem::replace(&mut self.lookahead, Token::End);
        self.lookahead = self.lexer.next()?;
        Ok(current)
    }

    fn conditional(&mut self) -> Result<Expr> {
        let condition = self.logical_or()?;
        if self.lookahead != Token::Question {
            return Ok(condition);
        }
        self.bump()?;
        let yes = self.conditional()?;
        if self.lookahead != Token::Colon {
            bail!("SIMetrix ALB conditional expression has no ':'");
        }
        self.bump()?;
        let no = self.conditional()?;
        Ok(Expr::Conditional(
            Box::new(condition),
            Box::new(yes),
            Box::new(no),
        ))
    }

    fn logical_or(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::logical_and,
            &[(Token::LogicalOr, BinaryOp::LogicalOr)],
        )
    }

    fn logical_and(&mut self) -> Result<Expr> {
        self.binary_chain(Self::bit_or, &[(Token::LogicalAnd, BinaryOp::LogicalAnd)])
    }

    fn bit_or(&mut self) -> Result<Expr> {
        self.binary_chain(Self::bit_xor, &[(Token::BitOr, BinaryOp::BitOr)])
    }

    fn bit_xor(&mut self) -> Result<Expr> {
        self.binary_chain(Self::bit_and, &[(Token::BitXor, BinaryOp::BitXor)])
    }

    fn bit_and(&mut self) -> Result<Expr> {
        self.binary_chain(Self::equality, &[(Token::BitAnd, BinaryOp::BitAnd)])
    }

    fn equality(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::relational,
            &[
                (Token::Equal, BinaryOp::Equal),
                (Token::NotEqual, BinaryOp::NotEqual),
            ],
        )
    }

    fn relational(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::shift,
            &[
                (Token::Less, BinaryOp::Less),
                (Token::Greater, BinaryOp::Greater),
                (Token::LessEqual, BinaryOp::LessEqual),
                (Token::GreaterEqual, BinaryOp::GreaterEqual),
            ],
        )
    }

    fn shift(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::additive,
            &[
                (Token::ShiftLeft, BinaryOp::ShiftLeft),
                (Token::ShiftRight, BinaryOp::ShiftRight),
            ],
        )
    }

    fn additive(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::multiplicative,
            &[
                (Token::Plus, BinaryOp::Add),
                (Token::Minus, BinaryOp::Subtract),
            ],
        )
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        self.binary_chain(
            Self::unary,
            &[
                (Token::Star, BinaryOp::Multiply),
                (Token::Slash, BinaryOp::Divide),
                (Token::Percent, BinaryOp::Modulo),
            ],
        )
    }

    fn binary_chain(
        &mut self,
        next: fn(&mut Self) -> Result<Expr>,
        operators: &[(Token, BinaryOp)],
    ) -> Result<Expr> {
        let mut lhs = next(self)?;
        loop {
            let Some((_, operation)) = operators.iter().find(|(token, _)| *token == self.lookahead)
            else {
                return Ok(lhs);
            };
            let operation = *operation;
            self.bump()?;
            let rhs = next(self)?;
            lhs = Expr::Binary(operation, Box::new(lhs), Box::new(rhs));
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        let operation = match self.lookahead {
            Token::Plus => Some(UnaryOp::Plus),
            Token::Minus => Some(UnaryOp::Minus),
            Token::LogicalNot => Some(UnaryOp::LogicalNot),
            Token::BitwiseNot => Some(UnaryOp::BitwiseNot),
            _ => None,
        };
        if let Some(operation) = operation {
            self.bump()?;
            return Ok(Expr::Unary(operation, Box::new(self.unary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.bump()? {
            Token::Number(value) => Ok(Expr::Constant(value)),
            Token::Identifier(name) => {
                if self.lookahead == Token::LeftBracket {
                    self.bump()?;
                    let index = self.conditional()?;
                    if self.lookahead != Token::RightBracket {
                        bail!("unterminated SIMetrix ALB array index");
                    }
                    self.bump()?;
                    Ok(Expr::Index(name, Box::new(index)))
                } else {
                    Ok(Expr::Name(name))
                }
            }
            Token::LeftParen => {
                let expression = self.conditional()?;
                if self.lookahead != Token::RightParen {
                    bail!("unterminated parenthesis in SIMetrix ALB expression");
                }
                self.bump()?;
                Ok(expression)
            }
            token => bail!("expected SIMetrix ALB expression value, found {token:?}"),
        }
    }
}

fn parse_u32_constant(value: &str) -> Result<u32> {
    let value = value.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Ok(u32::from_str_radix(hex, 16)?)
    } else if let Some(negative) = value.strip_prefix('-') {
        Ok(negative.parse::<u32>()?.wrapping_neg())
    } else if let Some(positive) = value.strip_prefix('+') {
        Ok(positive.parse::<u32>()?)
    } else {
        Ok(value.parse::<u32>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_hc138_known_input_truth_table() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT Address in[0:2] ;
PORT E1_Inv in[3] ;
PORT E2_Inv in[4] ;
PORT E3 in[5] ;
PORT Out out[0:7] ;
Out = (!E1_Inv && !E2_Inv && E3) ? ~(1<<Address) : 255 ;
",
        )?;
        assert_eq!(table.input_width(), 6);
        assert_eq!(table.output_width(), 8);
        assert_eq!(table.rows()[0], 0xff);
        assert_eq!(table.rows()[32], 0xfe);
        assert_eq!(table.rows()[39], 0x7f);
        assert_eq!(table.d_genlut_values().len(), 8 * 64);
        Ok(())
    }

    #[test]
    fn compiles_hc139_known_input_truth_table() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT Address in[0:1] ;
PORT E_Inv in[2] ;
PORT Out out[0:3] ;
Out = (!E_Inv) ? ~(1<<Address) : 7 ;
",
        )?;
        assert_eq!(table.input_width(), 3);
        assert_eq!(table.output_width(), 4);
        assert_eq!(table.rows()[0], 0xe);
        assert_eq!(table.rows()[3], 0x7);
        assert_eq!(table.rows()[4], 0x7);
        Ok(())
    }

    #[test]
    fn compiles_readonly_decoder() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT Address in[0:2] ;
PORT E1_Inv in[3] ;
PORT E2_Inv in[4] ;
PORT E3 in[5] ;
PORT Out out[0:7] ;
READONLY (WIDTH=8) Decode[8] = 1, 2, 4, 8, 16, 32, 64, 128 ;
Out = (!E1_Inv && !E2_Inv && E3) ? Decode[Address&7] : 0 ;
",
        )?;
        assert_eq!(table.rows()[32], 1);
        assert_eq!(table.rows()[39], 128);
        assert_eq!(table.rows()[0], 0);
        Ok(())
    }

    #[test]
    fn compiles_mux_and_inverting_mux() -> Result<()> {
        let mux = compile_known_truth_table(
            r"
PORT DA in[0:3] ;
PORT DB in[4:7] ;
PORT Select in[8] ;
PORT En_Inv in[9] ;
PORT out out[0:3] ;
out = !En_inv ? (Select ? DB : DA) : 0 ;
",
        )?;
        assert_eq!(mux.rows()[0x00a], 0xa);
        assert_eq!(mux.rows()[0x100 | 0x0b0], 0xb);
        assert_eq!(mux.rows()[0x200 | 0x00f], 0);

        let inv = compile_known_truth_table(
            r"
PORT DA in[0:3] ;
PORT DB in[4:7] ;
PORT Select in[8] ;
PORT En_Inv in[9] ;
PORT out out[0:3] ;
out = !En_inv ? (Select ? ~DB : ~DA) : ~0 ;
",
        )?;
        assert_eq!(inv.rows()[0x005], 0xa);
        assert_eq!(inv.rows()[0x100 | 0x0c0], 0x3);
        assert_eq!(inv.rows()[0x200], 0xf);
        Ok(())
    }

    #[test]
    fn preserves_output_delay_references() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT BCDin in[0:3] ;
PORT (DELAY=USER[0]) Output out[0:9] ;
Output = ~(1<<BCDin) ;
",
        )?;
        assert_eq!(table.rows()[0], 0x3fe);
        assert_eq!(table.rows()[9], 0x1ff);
        assert_eq!(table.rows()[15], 0x3ff);
        assert_eq!(table.delayed_output_bits(), 10);
        Ok(())
    }

    #[test]
    fn accepts_integral_decimal_exponent_and_engineering_constants() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0:10] ;
Y = A ? 1.5k : 1e3 ;
",
        )?;
        assert_eq!(table.rows(), &[1000, 1500]);
        Ok(())
    }

    #[test]
    fn rejects_non_integral_scaled_constant() {
        let error = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0] ;
Y = A ? 1.5 : 0 ;
",
        )
        .err()
        .map(|value| value.to_string());
        assert!(error.is_some_and(|value| value.contains("not an exact unsigned 32-bit integer")));
    }

    #[test]
    fn compiles_direct_pin_only_definition_without_ports() -> Result<()> {
        let table = compile_known_truth_table(
            r"
out[0] = !in[1]&!in[2] | in[1]&!in[2]&in[0] | !in[1]&in[2]&in[0] ;
",
        )?;
        assert_eq!(table.input_width(), 3);
        assert_eq!(table.output_width(), 1);
        assert_eq!(table.rows(), &[1, 1, 0, 1, 0, 1, 0, 0]);
        Ok(())
    }

    #[test]
    fn local_variables_and_direct_output_assignments_follow_alb_ordering() -> Result<()> {
        let table = compile_known_truth_table(
            r"
PORT A in[0:1] ;
PORT Y out[0:1] ;
tmp = A + 1 ;
out[0:1] = tmp ;
",
        )?;
        assert_eq!(table.rows(), &[1, 2, 3, 0]);
        Ok(())
    }

    #[test]
    fn rejects_stateful_or_delayed_alb() -> Result<()> {
        let error = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=1n) delayed ;
delayed = A ;
Y = delayed ;
",
        )
        .err()
        .context("COMB must remain outside stateless lowering")?;
        assert!(
            error
                .to_string()
                .contains("expected stateless SIMetrix ALB")
        );
        Ok(())
    }

    #[test]
    fn clock_pins_extend_stateful_input_signature_without_named_port() -> Result<()> {
        for (declaration, expected_kind) in [
            ("EDGE (CLOCK=in[5], WIDTH=1) R=0 ;", StateRegisterKind::Edge),
            (
                "LEVEL (CLOCK=in[5], WIDTH=1) R=0 ;",
                StateRegisterKind::Level,
            ),
        ] {
            let source = format!("PORT D in[0]; PORT Q out[0]; {declaration} R = D; Q = R;");
            let (_, stateful) = analyze_body(&source)?.into_parts();
            let program = stateful.context("expected register program")?;
            assert_eq!(program.input_width(), 6);
            assert_eq!(program.output_width(), 1);
            assert_eq!(program.register_count(expected_kind), 1);
            assert_eq!(program.registers[0].clock_pin, Some(5));
            assert!(
                program
                    .timed_combinational_graph(TimingEnvironment {
                        user: &[],
                        device: None,
                        user_scale: 1.0,
                        default_register_delay: 1e-9,
                        default_output_delay: 1e-9,
                    })?
                    .is_none(),
                "clocked ALBs must remain outside the COMB-only compiler"
            );
        }
        Ok(())
    }

    #[test]
    fn documented_clock_only_counter_has_one_physical_input() -> Result<()> {
        // The official eight-bit ALB counter has no named input PORT:
        // CLOCK=in[0] alone establishes its entire external input signature.
        let (_, stateful) = analyze_body(
            r"
PORT (DELAY=10n) CountOut out[0:7] ;
EDGE (DELAY=5n, WIDTH=8, CLOCK=in[0]) Count ;
Count = Count + 1 ;
CountOut = Count ;
",
        )?
        .into_parts();
        let program = stateful.context("official EDGE counter was not parsed")?;
        assert_eq!(program.input_width(), 1);
        assert_eq!(program.output_width(), 8);
        assert_eq!(program.register_count(StateRegisterKind::Edge), 1);
        assert_eq!(program.registers[0].clock_pin, Some(0));
        Ok(())
    }

    #[test]
    fn stateful_vendor_syntax_probes_parse_without_native_enablement() -> Result<()> {
        let cases = [
            (
                include_str!("../../../runtime/xspice/fixtures/vendor-stateful/edge-basic.net"),
                StateRegisterKind::Edge,
                2,
                0,
            ),
            (
                include_str!("../../../runtime/xspice/fixtures/vendor-stateful/level-basic.net"),
                StateRegisterKind::Level,
                2,
                0,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/edge-hold-minclock.net"
                ),
                StateRegisterKind::Edge,
                2,
                0,
            ),
            (
                include_str!("../../../runtime/xspice/fixtures/vendor-stateful/level-setup.net"),
                StateRegisterKind::Level,
                2,
                0,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/edge-async-reset.net"
                ),
                StateRegisterKind::Edge,
                3,
                1,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-minclock-only.net"
                ),
                StateRegisterKind::Edge,
                2,
                0,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-hold-only.net"
                ),
                StateRegisterKind::Edge,
                2,
                0,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/controls/edge-no-qualifiers.net"
                ),
                StateRegisterKind::Edge,
                2,
                0,
            ),
            (
                include_str!(
                    "../../../runtime/xspice/fixtures/vendor-stateful/controls/level-setup-no-setup.net"
                ),
                StateRegisterKind::Level,
                2,
                0,
            ),
        ];
        for (deck, kind, inputs, async_count) in cases {
            let body = deck
                .lines()
                .skip_while(|line| !line.trim_start().to_ascii_uppercase().starts_with(".FILE "))
                .skip(1)
                .take_while(|line| !line.trim().eq_ignore_ascii_case(".ENDF"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!body.is_empty() && body.contains("PORT"));
            let (stateless, stateful) = analyze_body(&body)?.into_parts();
            assert!(stateless.is_none());
            let program = stateful.context("vendor source must be stateful")?;
            assert_eq!(program.input_width(), inputs, "{body}");
            assert_eq!(program.output_width(), 1, "{body}");
            assert_eq!(program.register_count(kind), 1, "{body}");
            assert_eq!(program.registers[0].clock_pin, Some(1));
            assert_eq!(program.asynchronous_assignment_count(), async_count);
            assert_eq!(program.registers[0].initial.len(), 1);
            assert!(
                program
                    .timed_combinational_graph(TimingEnvironment {
                        user: &[],
                        device: None,
                        user_scale: 1.0,
                        default_register_delay: 1e-9,
                        default_output_delay: 1e-9,
                    })?
                    .is_none(),
                "stateful oracle must never silently lower as COMB"
            );
        }
        Ok(())
    }

    #[test]
    fn parses_edge_comb_and_asynchronous_state_program() -> Result<()> {
        let analysis = analyze_body(
            r"
* ALB also accepts SPICE-style whole-line comments
PORT D in[0] ;
PORT CK in[1] ;
PORT SR in[2:3] ;
PORT (DELAY=USER[6]) Q out[0:1] ;
EDGE (WIDTH=2, DELAY=USER[4], HOLD=USER[2], MINCLOCK=USER[3], CLOCK=in[1], ASYNCDELAY=USER[5]) DTYPE=2 ;
COMB (DELAY=USER[0], WIDTH=1) D_DEL ;
COMB (DELAY=USER[1], WIDTH=2) SR_DEL ;
SR_DEL = SR ;
D_DEL = D ;
DTYPE := SR_DEL==1||SR_DEL==2 ? (SR_DEL==2 ? 1 : 2) ;
DTYPE = D_DEL ? 1 : 2 ;
Q = SR_DEL==0 ? 3 : DTYPE ;
",
        )?;
        let (table, stateful) = analysis.into_parts();
        assert!(table.is_none());
        let stateful = stateful.context("expected stateful ALB program")?;
        assert_eq!(stateful.input_width(), 4);
        assert_eq!(stateful.output_width(), 2);
        assert_eq!(stateful.register_count(StateRegisterKind::Combinational), 2);
        assert_eq!(stateful.register_count(StateRegisterKind::Edge), 1);
        assert_eq!(stateful.register_count(StateRegisterKind::Level), 0);
        assert_eq!(stateful.asynchronous_assignment_count(), 1);
        assert_eq!(stateful.register_array_count(), 0);
        assert_eq!(stateful.indexed_assignment_count(), 0);
        assert!(stateful.structural_summary().contains("async=1"));
        Ok(())
    }

    #[test]
    fn parses_level_register_and_case_insensitive_output_enable() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT LE in[0] ;
PORT OE in[1] ;
PORT Data in[2:9] ;
PORT (DELAY=USER[0]) Output out[0:7] ;
LEVEL (WIDTH=8, DELAY=USER[2], SETUP=USER[3], CLOCK=in[0]) Latch ;
Latch = Data ;
Output = Latch ;
Output.eN = !OE ? ~0 : 0 ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let stateful = stateful.context("expected stateful ALB program")?;
        assert_eq!(stateful.register_count(StateRegisterKind::Level), 1);
        assert_eq!(stateful.output_enable_assignment_count(), 1);
        assert_eq!(stateful.asynchronous_assignment_count(), 0);
        Ok(())
    }

    #[test]
    fn parses_edge_register_array_and_indexed_write() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT ShiftIn in[0] ;
PORT ShiftOut in[1] ;
PORT DataIn in[2:5] ;
PORT DataOut out[0:3] ;
EDGE (WIDTH=4, DELAY=USER[1], ASYNCDELAY=USER[3], CLOCK=in[0]) In_Count ;
EDGE (WIDTH=4, DELAY=USER[1], ASYNCDELAY=USER[3], CLOCK=in[1]) Out_Count ;
EDGE (WIDTH=4, DELAY=USER[2], CLOCK=in[0]) FIFO[16] ;
In_Count := ShiftIn ? 0 ;
Out_Count := ShiftOut ? 0 ;
FIFO[In_Count] = DataIn ;
In_Count = In_Count + 1 ;
Out_Count = Out_Count + 1 ;
DataOut = FIFO[Out_Count] ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let stateful = stateful.context("expected stateful ALB program")?;
        assert_eq!(stateful.register_count(StateRegisterKind::Edge), 3);
        assert_eq!(stateful.register_array_count(), 1);
        assert_eq!(stateful.indexed_assignment_count(), 1);
        assert_eq!(stateful.asynchronous_assignment_count(), 2);
        Ok(())
    }

    #[test]
    fn readonly_scalar_and_default_initial_condition_are_supported() -> Result<()> {
        let scalar = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0:2] ;
READONLY K = 5 ;
Y = A ? K : 0 ;
",
        )?;
        assert_eq!(scalar.rows(), &[0, 5]);

        let default_zero = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0] ;
READONLY Flag ;
Y = A & Flag ;
",
        )?;
        assert_eq!(default_zero.rows(), &[0, 0]);
        Ok(())
    }

    #[test]
    fn classifies_acyclic_bitwise_comb_timing() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (DELAY=2n, WIDTH=1) D1 ;
COMB (DELAY=3n, WIDTH=1) D2 ;
D1 = A ;
Alias = D1 ;
D2 = Alias ;
Y = D2 ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let shape = stateful
            .context("expected delayed combinational program")?
            .combinational_timing_shape()
            .context("expected COMB-only timing shape")?;
        assert!(!shape.has_feedback());
        assert_eq!(shape.delay_mode(), CombinationalDelayMode::Bitwise);
        Ok(())
    }

    #[test]
    fn classifies_feedback_whole_register_comb_timing() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (DELAY=2n, WIDTH=2, BITWISE=0) State=0 ;
State = A ? (State + 1) : State ;
Y = State ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let shape = stateful
            .context("expected delayed combinational program")?
            .combinational_timing_shape()
            .context("expected COMB-only timing shape")?;
        assert!(shape.has_feedback());
        assert_eq!(shape.delay_mode(), CombinationalDelayMode::WholeRegister);
        Ok(())
    }

    #[test]
    fn classifies_dynamic_comb_bitwise_mode() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (DELAY=2n, WIDTH=1, BITWISE=USER[0]) D ;
D = A ;
Y = D ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let shape = stateful
            .context("expected delayed combinational program")?
            .combinational_timing_shape()
            .context("expected COMB-only timing shape")?;
        assert!(!shape.has_feedback());
        assert_eq!(shape.delay_mode(), CombinationalDelayMode::Dynamic);
        Ok(())
    }

    #[test]
    fn preserves_stateful_output_delay_reference() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT (DELAY=USER[3]) Y out[0] ;
COMB (DELAY=2n, WIDTH=1) D ;
D = A ;
Y = D ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let stateful = stateful.context("expected delayed combinational program")?;
        assert!(matches!(
            stateful.outputs[0].delay,
            Some(DynamicValue::User(3))
        ));
        Ok(())
    }

    #[test]
    fn resolves_comb_timing_defaults_user_scale_and_device_values() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT (DELAY=USER[1]) Y out[0] ;
COMB (WIDTH=1) D0 ;
COMB (WIDTH=1, DELAY=USER[0]) D1 ;
COMB (WIDTH=1, DELAY=DEVICE[0]) D2 ;
D0 = A ;
D1 = D0 ;
D2 = D1 ;
Y = D2 ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let stateful = stateful.context("expected delayed combinational program")?;
        let resolved = stateful
            .resolve_combinational_timing(TimingEnvironment {
                user: &[2.0e-9, 3.0e-9],
                device: Some(&[7.0e-9]),
                user_scale: 2.0,
                default_register_delay: 11.0e-9,
                default_output_delay: 13.0e-9,
            })?
            .context("expected COMB-only timing resolution")?;
        assert_eq!(resolved.register_delays()["D0"], 11.0e-9);
        assert_eq!(resolved.register_delays()["D1"], 4.0e-9);
        assert_eq!(resolved.register_delays()["D2"], 7.0e-9);
        assert_eq!(resolved.output_delays()["Y"], 6.0e-9);
        Ok(())
    }

    #[test]
    fn comb_timing_device_value_stays_instance_dependent_without_environment() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=DEVICE[2]) D ;
D = A ;
Y = D ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let stateful = stateful.context("expected delayed combinational program")?;
        let error = stateful
            .resolve_combinational_timing(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })
            .err()
            .map(|error| error.to_string());
        assert!(error.is_some_and(|error| {
            error.contains("DEVICE[2] requires an instance parameter environment")
        }));
        Ok(())
    }

    #[test]
    fn builds_topologically_ordered_timed_comb_graph() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT B in[1] ;
PORT (DELAY=USER[2]) Y out[0:1] ;
COMB (WIDTH=2, DELAY=USER[0]) D1 ;
COMB (WIDTH=2, DELAY=USER[1]) D2 ;
Alias = A | (B << 1) ;
D2 = D1 ^ Alias ;
D1 = Alias ;
Y = D2 ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected stateful ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[2.0e-9, 3.0e-9, 4.0e-9],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?
            .context("expected acyclic timed COMB graph")?;
        assert_eq!(graph.stage_names(), vec!["D1", "D2"]);
        let d1 = graph.stage("D1").context("missing D1 stage")?;
        assert_eq!(d1.width, 2);
        assert_eq!(d1.delay, 2.0e-9);
        assert_eq!(d1.dependencies, Vec::<String>::new());
        let d2 = graph.stage("D2").context("missing D2 stage")?;
        assert_eq!(d2.dependencies, vec!["D1"]);
        assert_eq!(d2.delay, 3.0e-9);
        let y = graph.output("Y").context("missing Y output")?;
        assert_eq!(y.delay, 4.0e-9);
        assert_eq!(y.dependencies, vec!["D2"]);
        assert!(y.enable.is_none());
        assert!(graph.structural_summary().contains("depth=3"));
        Ok(())
    }

    #[test]
    fn timed_comb_graph_preserves_output_enable_and_scaled_width() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT OE in[1] ;
PORT Y out[0:1] ;
COMB (WIDTH=USER[0], DELAY=2n) D ;
COMB (WIDTH=1, DELAY=3n) E ;
D = A ? 3 : 0 ;
E = OE ;
Y = D ;
Y.En = E ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected stateful ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[1.0],
                device: None,
                user_scale: 2.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 5.0e-9,
            })?
            .context("expected acyclic timed COMB graph")?;
        assert_eq!(graph.stage("D").context("missing D")?.width, 2);
        let output = graph.output("Y").context("missing Y")?;
        assert!(output.enable.is_some());
        assert_eq!(output.dependencies, vec!["D", "E"]);
        assert!(graph.structural_summary().contains("1 tri-state"));
        Ok(())
    }

    #[test]
    fn timed_comb_graph_rejects_feedback_whole_register_program() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n, BITWISE=0) State ;
State = A ? !State : State ;
Y = State ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected stateful ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?;
        assert!(graph.is_none());
        Ok(())
    }

    #[test]
    fn compiles_timed_stage_known_state_luts_with_minimal_sources() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT B in[1] ;
PORT Y out[0:1] ;
COMB (WIDTH=2, DELAY=2n) D ;
D = A ^ (B << 1) ;
Y = D ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected timed ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?
            .context("expected timed graph")?;
        let luts = graph.known_state_luts()?;
        let stage0 = luts.stage_bit("D", 0).context("missing D[0] LUT")?;
        let stage1 = luts.stage_bit("D", 1).context("missing D[1] LUT")?;
        assert_eq!(stage0.sources.len(), 1);
        assert_eq!(stage1.sources.len(), 1);
        assert_eq!(stage0.d_genlut_values(), "01");
        assert_eq!(stage1.d_genlut_values(), "01");
        let output0 = luts.output_bit("Y", 0).context("missing Y[0] LUT")?;
        let output1 = luts.output_bit("Y", 1).context("missing Y[1] LUT")?;
        assert_eq!(output0.sources.len(), 1);
        assert_eq!(output1.sources.len(), 1);
        assert_eq!(output0.d_genlut_values(), "01");
        assert_eq!(output1.d_genlut_values(), "01");
        Ok(())
    }

    #[test]
    fn compiles_timed_output_enable_as_high_impedance_lut_cells() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0:1] ;
PORT OE in[2] ;
PORT Y out[0:1] ;
COMB (WIDTH=2, DELAY=2n) D ;
COMB (WIDTH=1, DELAY=3n) E ;
D = A ;
E = OE ;
Y = D ;
Y.En = E ? 3 : 0 ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected timed ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?
            .context("expected timed graph")?;
        let luts = graph.known_state_luts()?;
        let output0 = luts.output_bit("Y", 0).context("missing Y[0] LUT")?;
        let output1 = luts.output_bit("Y", 1).context("missing Y[1] LUT")?;
        assert_eq!(output0.sources.len(), 2);
        assert_eq!(output1.sources.len(), 2);
        assert_eq!(output0.high_impedance_cell_count(), 2);
        assert_eq!(output1.high_impedance_cell_count(), 2);
        assert_eq!(output0.d_genlut_values().len(), 4);
        assert!(output0.d_genlut_values().contains('Z'));
        Ok(())
    }

    #[test]
    fn stateless_truth_table_computes_boolean_consensus_envelope() -> Result<()> {
        let and_table = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT B in[1] ;
PORT Y out[0] ;
Y = A & B ;
",
        )?;
        assert_eq!(
            and_table.evaluate_possible_levels(&[AlbLogicLevel::Zero, AlbLogicLevel::Unknown])?,
            vec![AlbLogicLevel::Zero]
        );
        assert_eq!(
            and_table.evaluate_possible_levels(&[AlbLogicLevel::One, AlbLogicLevel::Unknown])?,
            vec![AlbLogicLevel::Unknown]
        );

        let correlated = compile_known_truth_table(
            r"
PORT A in[0] ;
PORT Y out[0] ;
Y = A ^ A ;
",
        )?;
        assert_eq!(
            correlated.evaluate_possible_levels(&[AlbLogicLevel::Unknown])?,
            vec![AlbLogicLevel::Zero]
        );
        Ok(())
    }

    #[test]
    fn timed_lut_computes_possible_levels_and_enable_strengths() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0] ;
PORT B in[1] ;
PORT OE in[2] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D ;
D = A & B ;
Y = D ;
Y.En = OE ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected timed ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?
            .context("expected timed graph")?;
        let luts = graph.known_state_luts()?;

        assert_eq!(
            luts.stage_bit_sources("D", 0),
            Some(&[KnownLutSourceBit::Input(0), KnownLutSourceBit::Input(1)][..])
        );
        assert_eq!(
            luts.evaluate_stage_bit_possible(
                "D",
                0,
                &[AlbLogicLevel::Zero, AlbLogicLevel::Unknown]
            )?,
            AlbDigitalBit {
                level: AlbLogicLevel::Zero,
                strength: AlbDriveStrength::Strong,
            }
        );
        assert_eq!(
            luts.evaluate_stage_bit_possible(
                "D",
                0,
                &[AlbLogicLevel::One, AlbLogicLevel::Unknown]
            )?,
            AlbDigitalBit {
                level: AlbLogicLevel::Unknown,
                strength: AlbDriveStrength::Strong,
            }
        );

        assert_eq!(
            luts.output_bit_sources("Y", 0),
            Some(
                &[
                    KnownLutSourceBit::Input(2),
                    KnownLutSourceBit::Stage {
                        name: "D".to_owned(),
                        bit: 0,
                    },
                ][..]
            )
        );
        assert_eq!(
            luts.evaluate_output_bit_possible(
                "Y",
                0,
                &[AlbLogicLevel::Unknown, AlbLogicLevel::One]
            )?,
            AlbDigitalBit {
                level: AlbLogicLevel::One,
                strength: AlbDriveStrength::Undetermined,
            }
        );
        assert_eq!(
            luts.evaluate_output_bit_possible(
                "Y",
                0,
                &[AlbLogicLevel::Zero, AlbLogicLevel::Unknown]
            )?,
            AlbDigitalBit {
                level: AlbLogicLevel::Unknown,
                strength: AlbDriveStrength::HighImpedance,
            }
        );
        Ok(())
    }

    #[test]
    fn timed_known_state_lut_uses_cell_bound_not_global_input_bound() -> Result<()> {
        let analysis = analyze_body(
            r"
PORT A in[0:22] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D ;
D = A == 0 ;
Y = D ;
",
        )?;
        let (_, stateful) = analysis.into_parts();
        let graph = stateful
            .context("expected timed ALB")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1.0e-9,
                default_output_delay: 1.0e-9,
            })?
            .context("expected timed graph")?;
        let error = graph
            .known_state_luts()
            .err()
            .map(|error| error.to_string());
        assert!(
            error.is_some_and(|error| error.contains("known-state LUT requires 8388608 cells"))
        );
        Ok(())
    }
}
