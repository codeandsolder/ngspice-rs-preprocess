//! Semantics-preserving translation of bounded SIMPLIS primitives to native ngspice.
//!
//! The translator operates on preprocessed SIMPLIS electrical primitives rather
//! than vendor-library helper names. ngspice mappings are semantics-oriented and
//! unsupported primitive/model variants fail closed.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::fmt::Write as _;

pub mod syntax;
use syntax::{
    model_statement_tokens, normalized_statement, parse_numeric, statement_tokens, validate_node,
};
mod whole;
pub use whole::{
    SplitDeckTranslation, WholeModelTranslation, capture_artifact_initial_conditions,
    capture_passive_initial_conditions, lower_simetrix_linked_runtime, translate_simplis_deck,
    translate_simplis_deck_split, translate_simplis_expanded_library,
};

/// One numeric point on a piecewise-linear characteristic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// First model coordinate (voltage for resistors/capacitors, current for inductors).
    pub voltage: f64,
    /// Second model coordinate (current, charge or flux linkage).
    pub value: f64,
}

/// Native-ngspice primitive emitted for one SIMPLIS semantic primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationKind {
    /// SIMPLIS `VPWLR` voltage/current characteristic.
    Vpwlr,
    /// SIMPLIS `IPWLR` current/voltage characteristic.
    Ipwlr,
    /// SIMPLIS `PWLC` / `NON_LINEAR_CAP_V1` voltage/charge characteristic.
    ChargeCapacitor,
    /// SIMPLIS `PWLL` current/flux-linkage characteristic.
    FluxInductor,
    /// Voltage-controlled switch lowered to native ngspice.
    VoltageControlledSwitch,
    /// SIMPLIS switch with strict hysteresis and initial-state suggestion.
    HystereticSwitch,
    /// `SIMetrix` voltage-controlled switch with explicit delay and slew phase.
    TimedVoltageControlledSwitch,
    /// SIMPLIS LEVEL=1 simple transistor switch.
    TransistorSwitchLevel1,
    /// SIMPLIS LEVEL=2 transistor switch with active/saturated/reverse-biased regions.
    TransistorSwitchLevel2,
    /// SIMPLIS differential digital comparator.
    Comparator,
    /// SIMPLIS single-input digital buffer.
    Buffer,
    /// SIMPLIS two- or three-input AND gate.
    AndGate,
    /// SIMPLIS OR gate.
    OrGate,
    /// SIMPLIS reset/set latch.
    SrLatch,
    /// SIMPLIS simple S-R flip flop.
    SrFlipFlop,
    /// SIMPLIS pulse source mapped to native/behavioral ngspice.
    PulseSource,
    /// SIMPLIS sawtooth source mapped to a periodic behavioral source.
    SawSource,
    /// SIMPLIS sinusoidal source mapped to its exact time-domain law.
    SineSource,
    /// SIMPLIS time-domain piecewise-linear source.
    PiecewiseLinearSource,
    /// SIMPLIS lossless ideal multi-winding transformer.
    IdealTransformer,
    /// SIMPLIS mutual inductance lowered to an ngspice coupling coefficient.
    MutualInductance,
    /// SIMPLIS current-controlled current/voltage source.
    CurrentControlledSource,
    /// `SIMetrix` discrete vertical MOSFET LEVEL=17 lowered to level-1 plus explicit parasitics.
    SimetrixMosfetLevel17,
    /// `PSpice` compact MOS gate/bulk series resistances lowered to explicit resistors.
    PspiceMosSeriesResistance,
    /// SIMPLIS device-voltage-controlled E/G source.
    DeviceVoltageControlledSource,
}

/// Code-model library needed by generated native-ngspice statements.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeModelRuntime {
    /// Event-timed gates, hysteretic receivers and committed latches.
    Logic,
}

impl CodeModelRuntime {
    /// Matching C-reference module filename. Native execution may implement
    /// the corresponding model schemas directly instead of loading a C module.
    #[must_use = "the filename identifies the reference runtime module"]
    pub const fn reference_module(self) -> &'static str {
        match self {
            Self::Logic => "ngspice_preprocess_logic.cm",
        }
    }
}

impl TranslationKind {
    /// Additional simulator runtime required by this electrical primitive.
    #[must_use = "the result declares the primitive runtime dependency"]
    pub const fn required_runtime(self) -> Option<CodeModelRuntime> {
        match self {
            Self::AndGate
            | Self::OrGate
            | Self::SrLatch
            | Self::SrFlipFlop
            | Self::HystereticSwitch
            | Self::TimedVoltageControlledSwitch
            | Self::TransistorSwitchLevel1
            | Self::TransistorSwitchLevel2 => Some(CodeModelRuntime::Logic),
            _ => None,
        }
    }
}

/// One translated primitive plus provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Translation {
    /// Exact ngspice statement bytes.
    pub bytes: Vec<u8>,
    /// Semantic primitive that was translated.
    pub kind: TranslationKind,
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.'))
    {
        bail!("invalid generated SPICE identifier {name:?}");
    }
    Ok(())
}

fn validate_finite_points(points: &[Point]) -> Result<()> {
    if points.len() < 2 {
        bail!("piecewise-linear characteristic requires at least two points");
    }
    if points
        .iter()
        .any(|point| !point.voltage.is_finite() || !point.value.is_finite())
    {
        bail!("piecewise-linear characteristic contains a non-finite coordinate");
    }
    Ok(())
}

fn validate_points(points: &[Point]) -> Result<()> {
    validate_finite_points(points)?;
    if points
        .windows(2)
        .any(|window| window[0].voltage >= window[1].voltage)
    {
        bail!("piecewise-linear voltage coordinates must be strictly increasing");
    }
    Ok(())
}

fn write_number(output: &mut String, value: f64) -> Result<()> {
    write!(output, "{value:.17e}")?;
    Ok(())
}

fn write_pwl(output: &mut String, node_p: &str, node_n: &str, points: &[Point]) -> Result<()> {
    write!(output, "pwl(v({node_p},{node_n})")?;
    for point in points {
        output.push_str(", ");
        write_number(output, point.voltage)?;
        output.push_str(", ");
        write_number(output, point.value)?;
    }
    output.push(')');
    Ok(())
}

fn validate_resistor_end_slopes(points: &[Point], current_defined: bool) -> Result<()> {
    validate_finite_points(points)?;
    let (independent, dependent): (fn(&Point) -> f64, fn(&Point) -> f64) = if current_defined {
        (|point| point.value, |point| point.voltage)
    } else {
        (|point| point.voltage, |point| point.value)
    };
    anyhow::ensure!(
        points
            .windows(2)
            .all(|pair| independent(&pair[0]) < independent(&pair[1])),
        "piecewise-linear resistor independent coordinates must be strictly increasing"
    );
    anyhow::ensure!(
        dependent(&points[0]) < dependent(&points[1])
            && dependent(&points[points.len() - 2]) < dependent(&points[points.len() - 1]),
        "piecewise-linear resistor end segments require positive differential resistance"
    );
    Ok(())
}

/// Translate a SIMPLIS `VPWLR` two-terminal V-I primitive to an ngspice
/// behavioral current source.
///
/// # Errors
///
/// Returns an error for invalid identifiers, fewer than two points, non-finite
/// coordinates, or non-monotonic voltage coordinates.
pub fn translate_vpwlr(
    name: &str,
    node_p: &str,
    node_n: &str,
    points: &[Point],
) -> Result<Translation> {
    validate_name(name)?;
    validate_node(node_p)?;
    validate_node(node_n)?;
    validate_resistor_end_slopes(points, false)?;

    let mut output = String::new();
    write!(output, "B{name} {node_p} {node_n} I=")?;
    write_pwl(&mut output, node_p, node_n, points)?;
    output.push('\n');
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::Vpwlr,
    })
}

/// Translate a SIMPLIS `IPWLR` two-terminal I-V primitive to an ngspice
/// series current sensor plus behavioral voltage source. As with VPWLR,
/// points are voltage/current pairs; only the independent coordinate
/// changes. Interior voltage slopes may be negative or zero.
///
/// # Errors
///
/// Returns an error for invalid identifiers, fewer than two points, non-finite
/// coordinates, non-monotonic current coordinates, or nonpositive end slopes.
pub fn translate_ipwlr(
    name: &str,
    node_p: &str,
    node_n: &str,
    points: &[Point],
) -> Result<Translation> {
    validate_name(name)?;
    validate_node(node_p)?;
    validate_node(node_n)?;
    validate_resistor_end_slopes(points, true)?;

    let stem = sanitized_name(name);
    let sense = format!("V__{stem}_IPWLR_SENSE");
    let middle = format!("__{stem}_IPWLR_MID");
    let mut output = String::new();
    writeln!(output, "{sense} {node_p} {middle} 0")?;
    write!(output, "B__{stem}_IPWLR {middle} {node_n} V=pwl(i({sense})")?;
    for point in points {
        output.push_str(", ");
        write_number(&mut output, point.value)?;
        output.push_str(", ");
        write_number(&mut output, point.voltage)?;
    }
    output.push_str(")\n");
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::Ipwlr,
    })
}

fn strictly_increasing_values(points: &[Point]) -> bool {
    points
        .windows(2)
        .all(|window| window[1].value > window[0].value)
}

// ngspice's hidden behavioral-capacitor network becomes unstable for the
// nC-scale power-device charge curves in the transient regression corpus.
// Smaller helper-device curves stay on ngspice Q= because adding explicit
// charge state there can make the transient operating point singular.
const EXPLICIT_CHARGE_STATE_MIN_SPAN_COULOMBS: f64 = 1.0e-9;

fn write_inverse_storage_pwl(
    output: &mut String,
    state_node: &str,
    points: &[Point],
    storage_offset: f64,
    storage_span: f64,
) -> Result<()> {
    write!(output, "pwl(v({state_node})")?;
    let mut previous = f64::NEG_INFINITY;
    for point in points {
        let coordinate = (point.value - storage_offset) / storage_span;
        anyhow::ensure!(
            coordinate.is_finite() && coordinate > previous,
            "normalized storage knots must be finite and strictly increasing"
        );
        previous = coordinate;
        output.push_str(", ");
        write_number(output, coordinate)?;
        output.push_str(", ");
        write_number(output, point.voltage)?;
    }
    output.push(')');
    Ok(())
}

fn normalized_initial_storage(
    points: &[Point],
    voltage: f64,
    offset: f64,
    span: f64,
) -> Result<f64> {
    let upper = points
        .partition_point(|point| point.voltage <= voltage)
        .clamp(1, points.len() - 1);
    let left = points[upper - 1];
    let right = points[upper];
    let fraction = (voltage - left.voltage) / (right.voltage - left.voltage);
    let charge = fraction.mul_add(
        (right.value - left.value) / span,
        (left.value - offset) / span,
    );
    anyhow::ensure!(
        charge.is_finite(),
        "non-finite normalized PWLC initial charge"
    );
    Ok(charge)
}

/// Translate a SIMPLIS `PWLC` / `NON_LINEAR_CAP_V1` voltage-charge primitive.
///
/// Large strictly increasing Q(V) curves use an explicit normalized charge
/// state instead of ngspice's `C ... Q=` compatibility rewrite. The state stores
/// Q minus its arbitrary constant offset, so this lowering is invariant to the
/// vendor's choice of Q=0. A native capacitor stores the charge span, an inverse
/// V(Q) PWL source constrains terminal voltage, and mirrored source current
/// updates the state, preserving `i = dQ/dt`.
///
/// Explicit initial voltages are converted to charge using the same
/// piecewise-linear characteristic (including end-segment extrapolation).
/// They require an invertible positive differential capacitance curve,
/// even if its charge span is small. Native Q= ignores the voltage IC
/// on the reference simulator's hidden storage element.
///
/// The 1 nC charge-span boundary is an ngspice conditioning policy established
/// by transient regression, not a SIMPLIS semantic boundary. Smaller, decreasing,
/// or non-invertible curves conservatively keep ngspice's native `Q=` lowering.
///
/// # Errors
///
/// Returns an error for invalid identifiers, fewer than two points, non-finite
/// coordinates, non-monotonic voltage coordinates, or an initial condition
/// on a non-invertible charge curve.
pub fn translate_charge_capacitor(
    name: &str,
    node_p: &str,
    node_n: &str,
    points: &[Point],
    initial_voltage: Option<f64>,
) -> Result<Translation> {
    validate_name(name)?;
    validate_node(node_p)?;
    validate_node(node_n)?;
    validate_points(points)?;

    if let Some(voltage) = initial_voltage {
        anyhow::ensure!(voltage.is_finite(), "non-finite PWLC initial voltage");
        anyhow::ensure!(
            strictly_increasing_values(points),
            "initialized PWLC requires strictly increasing charge; nonpositive differential capacitance is invalid in SIMPLIS"
        );
    }
    let mut output = String::new();
    if strictly_increasing_values(points) {
        let charge_min = points[0].value;
        let charge_max = points[points.len() - 1].value;
        let charge_span = charge_max - charge_min;
        if initial_voltage.is_some() || charge_span >= EXPLICIT_CHARGE_STATE_MIN_SPAN_COULOMBS {
            anyhow::ensure!(
                charge_span.is_finite() && charge_span > 0.,
                "invalid PWLC charge span"
            );
            let stem = sanitized_name(name);
            let state_node = format!("__{stem}_PWLC_Q");
            let voltage_source = format!("B__{stem}_PWLC_V");
            writeln!(
                output,
                "* ngspice-rs-preprocess: explicit normalized charge state for SIMPLIS PWLC"
            )?;
            write!(output, "{voltage_source} {node_p} {node_n} V='")?;
            write_inverse_storage_pwl(&mut output, &state_node, points, charge_min, charge_span)?;
            output.push_str("'\n");
            writeln!(
                output,
                "B__{stem}_PWLC_I {state_node} 0 I='-i({voltage_source})'"
            )?;
            write!(output, "C__{stem}_PWLC_Q {state_node} 0 ")?;
            write_number(&mut output, charge_span)?;
            if let Some(voltage) = initial_voltage {
                let charge = normalized_initial_storage(points, voltage, charge_min, charge_span)?;
                output.push_str(" IC=");
                write_number(&mut output, charge)?;
            }
            output.push('\n');
            return Ok(Translation {
                bytes: output.into_bytes(),
                kind: TranslationKind::ChargeCapacitor,
            });
        }
    }

    write!(output, "C{name} {node_p} {node_n} Q='")?;
    write_pwl(&mut output, node_p, node_n, points)?;
    output.push_str("'\n");
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::ChargeCapacitor,
    })
}

/// Translate a SIMPLIS `PWLL` current/flux-linkage primitive.
///
/// The independent point coordinate is signed terminal current, and the
/// dependent coordinate is flux linkage. A normalized storage capacitor
/// integrates terminal voltage; the inverse characteristic determines
/// branch current. The initial current is converted to flux exactly as
/// PWLC converts initial voltage to charge.
///
/// # Errors
/// Invalid identifiers, coordinates, nonpositive differential inductance,
/// or a non-finite initial storage coordinate.
pub fn translate_flux_inductor(
    name: &str,
    node_p: &str,
    node_n: &str,
    points: &[Point],
    initial_current: Option<f64>,
) -> Result<Translation> {
    validate_name(name)?;
    validate_node(node_p)?;
    validate_node(node_n)?;
    validate_points(points)?;
    anyhow::ensure!(
        strictly_increasing_values(points),
        "PWLL requires positive differential inductance"
    );
    let offset = points[0].value;
    let span = points[points.len() - 1].value - offset;
    anyhow::ensure!(span.is_finite() && span > 0., "invalid PWLL flux span");
    let stem = sanitized_name(name);
    let state = format!("__{stem}_PWLL_FLUX");
    let mut output = String::new();
    write!(output, "B__{stem}_PWLL_I {node_p} {node_n} I='")?;
    write_inverse_storage_pwl(&mut output, &state, points, offset, span)?;
    output.push_str("'\n");
    writeln!(
        output,
        "B__{stem}_PWLL_V {state} 0 I='-v({node_p},{node_n})'"
    )?;
    write!(output, "C__{stem}_PWLL_FLUX {state} 0 ")?;
    write_number(&mut output, span)?;
    if let Some(current) = initial_current {
        output.push_str(" IC=");
        write_number(
            &mut output,
            normalized_initial_storage(points, current, offset, span)?,
        )?;
    }
    output.push('\n');
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: TranslationKind::FluxInductor,
    })
}

fn parse_assignments(tokens: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut result = std::collections::BTreeMap::new();
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        if let Some((name, value)) = token.split_once('=') {
            if name.is_empty() {
                bail!("malformed SIMPLIS assignment {token:?}");
            }
            if value.is_empty() {
                let value = tokens
                    .get(index.saturating_add(1))
                    .with_context(|| format!("missing value after SIMPLIS assignment {token:?}"))?;
                if value == "=" || value.contains('=') {
                    bail!("malformed value after SIMPLIS assignment {token:?}");
                }
                result.insert(name.to_ascii_uppercase(), value.clone());
                index = index.saturating_add(2);
                continue;
            }
            result.insert(name.to_ascii_uppercase(), value.to_owned());
            index = index.saturating_add(1);
            continue;
        }
        if let Some(value) = tokens
            .get(index.saturating_add(1))
            .and_then(|next| next.strip_prefix('='))
            .filter(|value| !value.is_empty())
        {
            if token.is_empty() {
                bail!("malformed SIMPLIS assignment around {token:?}");
            }
            result.insert(token.to_ascii_uppercase(), value.to_owned());
            index = index.saturating_add(2);
            continue;
        }
        if tokens
            .get(index.saturating_add(1))
            .is_some_and(|next| next == "=")
        {
            let value = tokens.get(index.saturating_add(2)).with_context(|| {
                format!("missing value after SIMPLIS assignment name {token:?}")
            })?;
            if token.is_empty() || value.contains('=') {
                bail!("malformed SIMPLIS assignment around {token:?}");
            }
            result.insert(token.to_ascii_uppercase(), value.clone());
            index = index.saturating_add(3);
            continue;
        }
        index = index.saturating_add(1);
    }
    Ok(result)
}

fn numeric_parameter(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<f64> {
    let value = parameters
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("missing SIMPLIS parameter {name}"))?;
    parse_numeric(value)
}

fn integer_parameter(
    parameters: &std::collections::BTreeMap<String, String>,
    name: &str,
) -> Result<usize> {
    let value = numeric_parameter(parameters, name)?;
    if value.fract() != 0.0 || value < 0.0 || value > 1024.0 {
        bail!("SIMPLIS parameter {name} is not a bounded non-negative integer");
    }
    Ok(value.to_string().parse::<usize>()?)
}

fn sanitized_name(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn read_indexed_points(
    parameters: &std::collections::BTreeMap<String, String>,
    count: usize,
    voltage_prefix: &str,
    value_prefix: &str,
) -> Result<Vec<Point>> {
    let mut points = Vec::with_capacity(count.saturating_add(1));
    for index in 0..=count {
        points.push(Point {
            voltage: numeric_parameter(parameters, &format!("{voltage_prefix}{index}"))?,
            value: numeric_parameter(parameters, &format!("{value_prefix}{index}"))?,
        });
    }
    Ok(points)
}

fn indexed_points(
    parameters: &std::collections::BTreeMap<String, String>,
    count: usize,
    voltage_prefix: &str,
    value_prefix: &str,
) -> Result<Vec<Point>> {
    let points = read_indexed_points(parameters, count, voltage_prefix, value_prefix)?;
    validate_points(&points)?;
    Ok(points)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DigitalBufferKind {
    Comparator,
    Buffer,
}

#[derive(Debug)]
struct DigitalBufferSpec {
    stem: String,
    out: String,
    out_bar: String,
    rtn: String,
    input_p: String,
    input_n: String,
    kind: DigitalBufferKind,
    rin: f64,
    rout: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    delay: f64,
    threshold: f64,
    initial: Option<usize>,
}

fn emit_digital_buffer(spec: &DigitalBufferSpec) -> Result<Translation> {
    let diff = format!("__{}_DIFF", spec.stem);
    let state = format!("__{}_STATE", spec.stem);
    let delayed = if spec.delay == 0.0 {
        state.clone()
    } else {
        format!("__{}_DELAYED", spec.stem)
    };
    let out_int = format!("__{}_OUT_INT", spec.stem);
    let out_bar_int = format!("__{}_OUTB_INT", spec.stem);
    let hyst_model = format!("__{}_HYST_M", spec.stem);
    let delay_model = format!("__{}_DELAY_M", spec.stem);
    let mut output = String::new();
    write!(
        output,
        "R__{}_RINP {} {} ",
        spec.stem, spec.input_p, spec.rtn
    )?;
    write_number(&mut output, spec.rin)?;
    output.push('\n');
    if spec.kind == DigitalBufferKind::Comparator {
        write!(
            output,
            "R__{}_RINN {} {} ",
            spec.stem, spec.input_n, spec.rtn
        )?;
        write_number(&mut output, spec.rin)?;
        output.push('\n');
    }
    let raw_diff = if spec.initial.is_some() {
        format!("__{}_DIFF_RAW", spec.stem)
    } else {
        diff.clone()
    };
    writeln!(
        output,
        "E__{}_DIFF {raw_diff} 0 {} {} 1",
        spec.stem, spec.input_p, spec.input_n
    )?;
    if let Some(initial) = spec.initial {
        if initial > 1 {
            bail!("SIMPLIS DIGITAL1 buffer IC={initial} is not binary");
        }
        let force = if initial == 0 {
            spec.threshold - spec.hystwd / 2.0 - 1.0
        } else {
            spec.threshold + spec.hystwd / 2.0 + 1.0
        };
        write!(output, "B__{}_INIT {diff} 0 V='(time<=0 ? ", spec.stem)?;
        write_number(&mut output, force)?;
        writeln!(output, " : v({raw_diff}))'")?;
    }
    writeln!(output, "A__{}_HYST {diff} {state} {hyst_model}", spec.stem)?;
    write!(output, ".model {hyst_model} hyst(in_low=")?;
    write_number(&mut output, spec.threshold)?;
    output.push_str(" in_high=");
    write_number(&mut output, spec.threshold)?;
    output.push_str(" hyst=");
    write_number(&mut output, spec.hystwd / 2.0)?;
    output.push_str(" out_lower_limit=0 out_upper_limit=1 input_domain=0 fraction=false)\n");
    if spec.delay != 0.0 {
        writeln!(
            output,
            "A__{}_DELAY {state} {delayed} 0 {delay_model}",
            spec.stem
        )?;
        write!(output, ".model {delay_model} delay(delay=")?;
        write_number(&mut output, spec.delay)?;
        output.push_str(" buffer_size=2048)\n");
    }
    write!(output, "B__{}_OUTV {out_int} {} V='", spec.stem, spec.rtn)?;
    write_number(&mut output, spec.vol)?;
    output.push_str("+(");
    write_number(&mut output, spec.voh - spec.vol)?;
    writeln!(output, ")*v({delayed})'")?;
    write!(output, "R__{}_OUT {out_int} {} ", spec.stem, spec.out)?;
    write_number(&mut output, spec.rout)?;
    output.push('\n');
    write!(
        output,
        "B__{}_OUTBV {out_bar_int} {} V='",
        spec.stem, spec.rtn
    )?;
    write_number(&mut output, spec.vol)?;
    output.push_str("+(");
    write_number(&mut output, spec.voh - spec.vol)?;
    writeln!(output, ")*(1-v({delayed}))'")?;
    write!(
        output,
        "R__{}_OUTB {out_bar_int} {} ",
        spec.stem, spec.out_bar
    )?;
    write_number(&mut output, spec.rout)?;
    output.push('\n');
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: match spec.kind {
            DigitalBufferKind::Comparator => TranslationKind::Comparator,
            DigitalBufferKind::Buffer => TranslationKind::Buffer,
        },
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicGateKind {
    And,
    Or,
    Nand,
    Nor,
}

impl LogicGateKind {
    const fn inverted(self) -> bool {
        matches!(self, Self::Nand | Self::Nor)
    }

    const fn conjunction(self) -> bool {
        matches!(self, Self::And | Self::Nand)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LogicPolarity {
    Positive,
    Negative,
}

impl LogicPolarity {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "POS" => Ok(Self::Positive),
            "NEG" => Ok(Self::Negative),
            _ => bail!("invalid SIMPLIS LOGIC={value}"),
        }
    }

    const fn negative(self) -> bool {
        matches!(self, Self::Negative)
    }
}

#[derive(Clone, Copy, Debug)]
struct LogicIo {
    rin: f64,
    rout: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    delay: f64,
    threshold: f64,
}

#[derive(Clone, Copy, Debug)]
struct LogicReceiver {
    polarity: LogicPolarity,
    initial: bool,
}

impl LogicReceiver {
    const fn digital1(polarity: LogicPolarity) -> Self {
        Self {
            polarity,
            initial: polarity.negative(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LogicGateMode {
    Classic {
        polarity: LogicPolarity,
        initial: bool,
    },
    Digital1 {
        inverted_inputs: usize,
    },
}

impl LogicGateMode {
    const fn receiver(self, kind: LogicGateKind, index: usize, arity: usize) -> LogicReceiver {
        match self {
            // Every undecided input takes the state satisfying the suggested
            // classic output whenever possible.
            Self::Classic { polarity, initial } => LogicReceiver {
                polarity,
                initial: initial ^ kind.inverted(),
            },
            // DIGITAL1 receivers start physically low, independent of IC.
            Self::Digital1 { inverted_inputs } => {
                let inverted = index >= arity - inverted_inputs;
                let polarity = if inverted {
                    LogicPolarity::Negative
                } else {
                    LogicPolarity::Positive
                };
                LogicReceiver::digital1(polarity)
            }
        }
    }

    const fn output_negative(self) -> bool {
        match self {
            Self::Classic { polarity, .. } => polarity.negative(),
            Self::Digital1 { .. } => false,
        }
    }
}

#[derive(Debug)]
struct LogicGateSpec {
    kind: LogicGateKind,
    stem: String,
    out: String,
    out_bar: Option<String>,
    rtn: String,
    inputs: Vec<String>,
    io: LogicIo,
    mode: LogicGateMode,
}

// The numeric discriminants are the interface of preprocess_logic_event,
// shared with the independent native reference code model.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum LogicOperation {
    And,
    Or,
    Nand,
    Nor,
    ResetDominant,
    SetDominant,
    HoldWhenEqual,
}

impl From<LogicGateKind> for LogicOperation {
    fn from(kind: LogicGateKind) -> Self {
        match kind {
            LogicGateKind::And => Self::And,
            LogicGateKind::Or => Self::Or,
            LogicGateKind::Nand => Self::Nand,
            LogicGateKind::Nor => Self::Nor,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LogicTiming {
    operation: LogicOperation,
    delay: f64,
    inertial: bool,
    initial: bool,
}

#[derive(Clone, Copy, Debug)]
struct LogicThresholds {
    center: f64,
    width: f64,
}

impl LogicIo {
    const fn thresholds(self) -> LogicThresholds {
        LogicThresholds {
            center: self.threshold,
            width: self.hystwd,
        }
    }
}

fn emit_logic_input_loads(
    output: &mut String,
    stem: &str,
    rtn: &str,
    inputs: &[String],
    resistance: f64,
) -> Result<()> {
    for (index, input) in inputs.iter().enumerate() {
        write!(output, "R__{stem}_RIN_I{} {input} {rtn} ", index + 1)?;
        write_number(output, resistance)?;
        output.push('\n');
    }
    Ok(())
}

fn emit_logic_event(
    output: &mut String,
    stem: &str,
    rtn: &str,
    inputs: &[String],
    receivers: &[LogicReceiver],
    thresholds: LogicThresholds,
    timing: LogicTiming,
) -> Result<String> {
    let upper = thresholds.center + thresholds.width / 2.0;
    let lower = thresholds.center - thresholds.width / 2.0;
    if !upper.is_finite() || !lower.is_finite() {
        bail!("SIMPLIS logic thresholds overflow");
    }
    if inputs.is_empty() || inputs.len() != receivers.len() {
        bail!("inconsistent logic receiver count");
    }
    let state = format!("__{stem}_Q_STATE");
    let model = format!("__{stem}_EVENT_M");
    write!(output, "A__{stem}_EVENT %vd [")?;
    for input in inputs {
        write!(output, " {input} {rtn}")?;
    }
    writeln!(output, " ] {state} {model}")?;
    write!(
        output,
        ".model {model} preprocess_logic_event(operation={} delay=",
        timing.operation as u8
    )?;
    write_number(output, timing.delay)?;
    write!(
        output,
        " inertial={} ic={} low={lower:.17e} high={upper:.17e} inverted=[",
        timing.inertial,
        u8::from(timing.initial)
    )?;
    for receiver in receivers {
        write!(output, " {}", receiver.polarity.negative())?;
    }
    output.push_str(" ] rx_ic=[");
    for receiver in receivers {
        write!(output, " {}", u8::from(receiver.initial))?;
    }
    output.push_str(" ])\n");
    Ok(state)
}

#[derive(Clone, Copy, Debug)]
struct LogicOutput {
    negative: bool,
    complement: bool,
}

fn emit_logic_voltage(
    output: &mut String,
    stem: &str,
    out: &str,
    rtn: &str,
    state: &str,
    rails: [f64; 2],
) -> Result<()> {
    let model = format!("__{stem}_M");
    writeln!(output, "A__{stem}V {state} %vd({out} {rtn}) {model}")?;
    write!(output, ".model {model} preprocess_logic_voltage(out_low=")?;
    write_number(output, rails[0])?;
    output.push_str(" out_high=");
    write_number(output, rails[1])?;
    output.push_str(")\n");
    Ok(())
}

fn emit_logic_output(
    output: &mut String,
    io: LogicIo,
    stem: &str,
    out: &str,
    rtn: &str,
    state: &str,
    output_mode: LogicOutput,
) -> Result<()> {
    let suffix = if output_mode.complement { "B" } else { "" };
    let internal = format!("__{stem}_OUT{suffix}_INT");
    let rails = if output_mode.complement ^ output_mode.negative {
        [io.voh, io.vol]
    } else {
        [io.vol, io.voh]
    };
    emit_logic_voltage(
        output,
        &format!("{stem}_OUT{suffix}"),
        &internal,
        rtn,
        state,
        rails,
    )?;
    write!(output, "R__{stem}_OUT{suffix} {internal} {out} ")?;
    write_number(output, io.rout)?;
    output.push('\n');
    Ok(())
}

fn emit_logic_gate(spec: &LogicGateSpec) -> Result<Translation> {
    let mut output = String::new();
    let receivers = (0..spec.inputs.len())
        .map(|index| spec.mode.receiver(spec.kind, index, spec.inputs.len()))
        .collect::<Vec<_>>();
    emit_logic_input_loads(
        &mut output,
        &spec.stem,
        &spec.rtn,
        &spec.inputs,
        spec.io.rin,
    )?;
    let state = emit_logic_event(
        &mut output,
        &spec.stem,
        &spec.rtn,
        &spec.inputs,
        &receivers,
        spec.io.thresholds(),
        LogicTiming {
            operation: spec.kind.into(),
            delay: spec.io.delay,
            inertial: matches!(spec.mode, LogicGateMode::Digital1 { .. }),
            initial: false,
        },
    )?;
    let output_negative = spec.mode.output_negative();
    emit_logic_output(
        &mut output,
        spec.io,
        &spec.stem,
        &spec.out,
        &spec.rtn,
        &state,
        LogicOutput {
            negative: output_negative,
            complement: false,
        },
    )?;
    if let Some(out_bar) = &spec.out_bar {
        emit_logic_output(
            &mut output,
            spec.io,
            &spec.stem,
            out_bar,
            &spec.rtn,
            &state,
            LogicOutput {
                negative: output_negative,
                complement: true,
            },
        )?;
    }
    Ok(Translation {
        bytes: output.into_bytes(),
        kind: if spec.kind.conjunction() {
            TranslationKind::AndGate
        } else {
            TranslationKind::OrGate
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, bail};
    use std::{
        fs,
        process::{Command, Stdio},
    };

    fn ngspice_accepts(statement: &[u8], analysis: &str) -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix("ngspice-rs-preprocess-simplis-")
            .tempdir()?;
        let deck = temp.path().join("probe.cir");
        let mut bytes = b"* ngspice-rs-preprocess SIMPLIS translation probe\n".to_vec();
        bytes.extend_from_slice(if analysis.starts_with(".tran") {
            b"V1 in 0 PULSE(0 1 0 100p 100p 400p 1n)\n"
        } else {
            b"V1 in 0 0\n"
        });
        bytes.extend_from_slice(statement);
        bytes.extend_from_slice(analysis.as_bytes());
        bytes.extend_from_slice(b"\n.end\n");
        fs::write(&deck, bytes)?;
        let output = match Command::new("ngspice")
            .arg("-b")
            .arg(&deck)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("executing ngspice"),
        };
        if !output.status.success() {
            bail!(
                "ngspice rejected translated primitive: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    #[test]
    fn assignments_accept_whitespace_around_equals() -> Result<()> {
        let tokens = vec![
            "NSEG=".to_owned(),
            "3".to_owned(),
            "X0".to_owned(),
            "=".to_owned(),
            "-1".to_owned(),
            "KNEE_HIGH".to_owned(),
            "=2.9".to_owned(),
            "Y0=2".to_owned(),
        ];
        let values = parse_assignments(&tokens)?;
        assert_eq!(values.get("NSEG").map(String::as_str), Some("3"));
        assert_eq!(values.get("X0").map(String::as_str), Some("-1"));
        assert_eq!(values.get("KNEE_HIGH").map(String::as_str), Some("2.9"));
        assert_eq!(values.get("Y0").map(String::as_str), Some("2"));
        Ok(())
    }

    #[test]
    fn spice_nodes_accept_simetrix_polarity_punctuation() -> Result<()> {
        for node in ["+IN", "-IN", "Vsl+in", "Vsl-out", "WR*"] {
            validate_node(node)?;
        }
        Ok(())
    }

    #[test]
    fn spice_numeric_suffixes_accept_scale_plus_unit_text() -> Result<()> {
        let cases: [(&str, f64); 7] = [
            ("159H", 159.0),
            ("0.670V", 0.670),
            ("170UA", 170.0e-6),
            ("4500nF", 4500.0e-9),
            (".9MMHO", 0.9e-3),
            ("6.3NMHOS", 6.3e-9),
            ("2MEGOHM", 2.0e6),
        ];
        for (input, expected) in cases {
            let actual = parse_numeric(input)?;
            let tolerance = expected.abs().max(1.0) * 1.0e-15;
            assert!(
                (actual - expected).abs() <= tolerance,
                "{input}: {actual} != {expected}"
            );
        }
        Ok(())
    }

    #[test]
    fn statement_tokens_ignore_spice_comment_apostrophes() -> Result<()> {
        assert_eq!(
            statement_tokens("* Company's FET won't switch")?,
            Vec::<String>::new()
        );
        Ok(())
    }

    #[test]
    fn statement_tokens_strip_simetrix_semicolon_comments() -> Result<()> {
        assert_eq!(
            statement_tokens("L1 29 30 1E-09; output node")?,
            vec!["L1", "29", "30", "1E-09"]
        );
        assert_eq!(
            statement_tokens("B1 x 0 V='runtime;expression' ; trailing")?,
            vec!["B1", "x", "0", "V='runtime;expression'"]
        );
        Ok(())
    }

    #[test]
    fn vpwlr_is_native_ngspice() -> Result<()> {
        let translated = translate_vpwlr(
            "D_POWER",
            "in",
            "0",
            &[
                Point {
                    voltage: -1.0,
                    value: -1.0e-9,
                },
                Point {
                    voltage: 0.001,
                    value: 1.0e-12,
                },
                Point {
                    voltage: 1.001,
                    value: 1_000.0,
                },
            ],
        )?;
        assert_eq!(translated.kind, TranslationKind::Vpwlr);
        ngspice_accepts(&translated.bytes, ".op")
    }

    #[test]
    fn ipwlr_is_native_ngspice() -> Result<()> {
        let translated = translate_ipwlr(
            "R_LIMIT",
            "in",
            "0",
            &[
                Point {
                    voltage: -1.0,
                    value: -10.0,
                },
                Point {
                    voltage: 0.0,
                    value: 0.0,
                },
                Point {
                    voltage: 1.0,
                    value: 10.0,
                },
            ],
        )?;
        assert_eq!(translated.kind, TranslationKind::Ipwlr);
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("V__R_LIMIT_IPWLR_SENSE in __R_LIMIT_IPWLR_MID 0"));
        assert!(text.contains("V=pwl(i(V__R_LIMIT_IPWLR_SENSE)"));
        ngspice_accepts(&translated.bytes, ".op")
    }

    #[test]
    fn charge_curve_is_native_ngspice() -> Result<()> {
        let translated = translate_charge_capacitor(
            "CGS",
            "in",
            "0",
            &[
                Point {
                    voltage: 0.0,
                    value: 0.0,
                },
                Point {
                    voltage: 1.0,
                    value: 10.0e-9,
                },
                Point {
                    voltage: 5.0,
                    value: 30.0e-9,
                },
            ],
            None,
        )?;
        assert_eq!(translated.kind, TranslationKind::ChargeCapacitor);
        ngspice_accepts(&translated.bytes, ".tran 100p 1n\n.print tran v(in)")
    }

    #[test]
    fn decreasing_charge_curve_keeps_q_capacitor() -> Result<()> {
        let translated = translate_charge_capacitor(
            "CDS",
            "in",
            "0",
            &[
                Point {
                    voltage: 0.0,
                    value: 30.0e-9,
                },
                Point {
                    voltage: 1.0,
                    value: 10.0e-9,
                },
                Point {
                    voltage: 5.0,
                    value: 0.0,
                },
            ],
            None,
        )?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("CCDS in 0 Q='pwl(v(in,0)"));
        assert!(!text.contains("_PWLC_Q"));
        Ok(())
    }

    #[test]
    fn explicit_charge_state_is_invariant_to_charge_offset() -> Result<()> {
        let translated = translate_charge_capacitor(
            "CGD",
            "in",
            "0",
            &[
                Point {
                    voltage: 0.0,
                    value: 5.0e-9,
                },
                Point {
                    voltage: 1.0,
                    value: 6.0e-9,
                },
                Point {
                    voltage: 5.0,
                    value: 8.0e-9,
                },
            ],
            None,
        )?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("B__CGD_PWLC_V in 0 V='pwl(v(__CGD_PWLC_Q), 0.00000000000000000e0, 0.00000000000000000e0"));
        assert!(text.contains("C__CGD_PWLC_Q __CGD_PWLC_Q 0 3.00000000000000039e-9"));
        Ok(())
    }

    #[test]
    fn small_monotonic_charge_curve_keeps_q_capacitor() -> Result<()> {
        let translated = translate_charge_capacitor(
            "CGS",
            "in",
            "0",
            &[
                Point {
                    voltage: 0.0,
                    value: 0.0,
                },
                Point {
                    voltage: 1.0,
                    value: 30.0e-12,
                },
            ],
            None,
        )?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("CCGS in 0 Q='pwl(v(in,0)"));
        assert!(!text.contains("_PWLC_Q"));
        Ok(())
    }

    #[test]
    fn non_invertible_charge_curve_falls_back_to_q_capacitor() -> Result<()> {
        let translated = translate_charge_capacitor(
            "CGD",
            "in",
            "0",
            &[
                Point {
                    voltage: 0.0,
                    value: 0.0,
                },
                Point {
                    voltage: 1.0,
                    value: 10.0e-12,
                },
                Point {
                    voltage: 5.0,
                    value: 5.0e-12,
                },
            ],
            None,
        )?;
        let text = std::str::from_utf8(&translated.bytes)?;
        assert!(text.contains("CCGD in 0 Q='pwl(v(in,0)"));
        assert!(!text.contains("_PWLC_Q"));
        Ok(())
    }

    #[test]
    fn rejects_non_monotonic_curve() {
        let result = translate_vpwlr(
            "BAD",
            "1",
            "0",
            &[
                Point {
                    voltage: 1.0,
                    value: 0.0,
                },
                Point {
                    voltage: 1.0,
                    value: 1.0,
                },
            ],
        );
        assert!(result.is_err());
    }
}
