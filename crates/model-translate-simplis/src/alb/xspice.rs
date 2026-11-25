//! Experimental ngspice XSPICE emission for timed combinational ALB.
//! Does not enable the production `D_LOGIC_BLOCK` translator.

use super::{
    AlbLogicLevel, BTreeMap, BTreeSet, Context, EvalContext, KnownLutSourceBit, Result,
    TimedCombinationalGraph, bail,
};
use std::fmt::Write as _;

/// Bit destination in ALB register or physical output-pin coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XspiceLutTarget {
    Stage { name: String, bit: usize },
    Output { name: String, pin: usize },
}

/// One executable bit of a timed ALB graph.
#[derive(Clone, Debug, PartialEq)]
pub struct XspiceTimedBit {
    pub target: XspiceLutTarget,
    pub sources: Vec<KnownLutSourceBit>,
    pub table_values: String,
    pub delay_s: f64,
    pub initial: AlbLogicLevel,
}

/// Explicit experimental electrical parameters; not a claim of ALB family equivalence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XspiceElectrical {
    pub input_load_f: f64,
    /// Signed current drained by one physical input at logic low (negative A).
    pub input_sink_current_a: f64,
    /// Signed current demanded by one physical input at logic high (positive A).
    pub input_source_current_a: f64,
    /// Lower bound of downstream aggregate current for a low output (negative A).
    pub min_sink_a: f64,
    /// Upper bound of downstream aggregate current for a high output (positive A).
    pub max_source_a: f64,
    pub stage_out_res_ohm: f64,
    pub output_out_res_ohm: f64,
}

/// Compiler artifact, independent of simulator-specific net names.
#[derive(Clone, Debug)]
pub struct XspiceTimedPlan {
    input_width: usize,
    output_width: usize,
    bits: Vec<XspiceTimedBit>,
}

impl XspiceTimedPlan {
    /// Compile a combinational known-state ALB truth table to the same
    /// physical timed native ABI as the register/COMB compiler.
    ///
    /// # Errors
    /// Requires a positive finite delay and supported LUT fan-in.
    pub fn from_known_truth_table(table: &super::KnownTruthTable, delay_s: f64) -> Result<Self> {
        if !delay_s.is_finite() || delay_s < 1.0e-12 {
            bail!("ALB combinational truth table requires finite delay >= 1ps");
        }
        let input_width = table.input_width();
        let output_width = table.output_width();
        if input_width > 16 {
            bail!("ALB native truth table exceeds native 16-input LUT limit");
        }
        let values = table.d_genlut_values();
        let combinations = 1usize << input_width;
        let bits = (0..output_width)
            .map(|pin| XspiceTimedBit {
                target: XspiceLutTarget::Output {
                    name: format!("OUT{pin}"),
                    pin,
                },
                sources: (0..input_width).map(KnownLutSourceBit::Input).collect(),
                table_values: values[pin * combinations..(pin + 1) * combinations].to_owned(),
                delay_s,
                initial: AlbLogicLevel::Unknown,
            })
            .collect();
        Ok(Self {
            input_width,
            output_width,
            bits,
        })
    }
    #[must_use]
    pub const fn input_width(&self) -> usize {
        self.input_width
    }

    #[must_use]
    pub const fn output_width(&self) -> usize {
        self.output_width
    }

    #[must_use]
    pub fn bits(&self) -> &[XspiceTimedBit] {
        &self.bits
    }

    /// Render an isolated XSPICE subcircuit, input pins first then output pins.
    /// Caller must load the native code-model library separately.
    ///
    /// # Errors
    /// Rejects malformed identifiers, electrical parameters, forward stage
    /// references, excessive fan-in, malformed tables and missing output pins.
    pub fn render_subcircuit(&self, name: &str, electrical: XspiceElectrical) -> Result<String> {
        if !name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            bail!("invalid XSPICE subcircuit name {name:?}");
        }
        for (label, number) in [
            ("input_load", electrical.input_load_f),
            ("input_sink_current", electrical.input_sink_current_a),
            ("input_source_current", electrical.input_source_current_a),
            ("min_sink", electrical.min_sink_a),
            ("max_source", electrical.max_source_a),
            ("stage_out_res", electrical.stage_out_res_ohm),
            ("output_out_res", electrical.output_out_res_ohm),
        ] {
            let signed = matches!(
                label,
                "input_sink_current" | "input_source_current" | "min_sink" | "max_source"
            );
            if !number.is_finite() || (!signed && number < 0.0) {
                bail!("XSPICE {label} must be finite with valid sign: {number}");
            }
        }

        if electrical.input_sink_current_a > 0.0
            || electrical.input_source_current_a < 0.0
            || electrical.min_sink_a > 0.0
            || electrical.max_source_a < 0.0
        {
            bail!("XSPICE current polarity must satisfy sink<=0 and source>=0");
        }
        let mut result = format!(".subckt {name}");
        for pin in 0..self.input_width {
            write!(result, " sr_in{pin} sr_in{pin}_sink sr_in{pin}_source")?;
        }
        for pin in 0..self.output_width {
            write!(result, " sr_out{pin} sr_out{pin}_sink sr_out{pin}_source")?;
        }
        result.push('\n');
        // Each *physical* ALB input contributes one load even if the input is
        // absent from the Boolean graph. Internal LUT edges are not devices.
        for pin in 0..self.input_width {
            writeln!(
                result,
                "Asr_load{pin} sr_in{pin} sr_in{pin}_sink sr_in{pin}_source Msr_input_load"
            )?;
        }
        writeln!(
            result,
            ".model Msr_input_load sr_bundle_load(capacitance={:e} sink_current={:e} source_current={:e})",
            electrical.input_load_f,
            electrical.input_sink_current_a,
            electrical.input_source_current_a
        )?;

        self.render_bits(electrical, &mut result)?;
        writeln!(result, ".ends {name}")?;
        Ok(result)
    }
    fn render_bits(&self, electrical: XspiceElectrical, result: &mut String) -> Result<()> {
        let mut stage_nets = BTreeMap::<(String, usize), String>::new();
        let mut used_outputs = BTreeSet::new();
        let mut needs_tie0 = false;
        for (index, bit) in self.bits.iter().enumerate() {
            let (suffix, destination, resistance) = match &bit.target {
                XspiceLutTarget::Stage { name, bit } => {
                    let net = format!("sr_stage{index}");
                    if stage_nets
                        .insert((name.clone(), *bit), net.clone())
                        .is_some()
                    {
                        bail!("duplicate XSPICE stage {name}[{bit}]");
                    }
                    (format!("s{index}"), net, electrical.stage_out_res_ohm)
                }
                XspiceLutTarget::Output { name, pin } => {
                    if *pin >= self.output_width || !used_outputs.insert(*pin) {
                        bail!("invalid or duplicated XSPICE output {name} pin {pin}");
                    }
                    (
                        format!("o{index}"),
                        format!("sr_out{pin}"),
                        electrical.output_out_res_ohm,
                    )
                }
            };
            if !bit.delay_s.is_finite() || bit.delay_s < 1.0e-12 {
                bail!(
                    "XSPICE sr_alb_lut_bit requires delay >= 1ps for {suffix}: {}",
                    bit.delay_s
                );
            }
            if bit.sources.len() > 16 {
                bail!("XSPICE LUT {suffix} exceeds current native 16-source limit");
            }
            if bit.table_values.len() != 1usize << bit.sources.len()
                || !bit
                    .table_values
                    .bytes()
                    .all(|byte| matches!(byte, b'0' | b'1' | b'Z'))
            {
                bail!("malformed XSPICE table for {suffix}");
            }

            let mut nets = Vec::new();
            for source in &bit.sources {
                nets.push(match source {
                    KnownLutSourceBit::Input(pin) => {
                        if *pin >= self.input_width {
                            bail!("XSPICE {suffix} references undefined input pin {pin}");
                        }
                        format!("sr_in{pin}")
                    }
                    KnownLutSourceBit::Stage { name, bit } => stage_nets
                        .get(&(name.clone(), *bit))
                        .cloned()
                        .with_context(|| {
                            format!(
                                "XSPICE {suffix} references absent or forward stage {name}[{bit}]"
                            )
                        })?,
                });
            }

            // Native vector input requires at least one digital node. Supply a
            // constant-zero pulldown for a zero-fanin LUT and duplicate its row.
            let table = if nets.is_empty() {
                needs_tie0 = true;
                nets.push("sr_tie0".to_owned());
                bit.table_values.repeat(2)
            } else {
                bit.table_values.clone()
            };
            let ic = match bit.initial {
                AlbLogicLevel::Zero => 0,
                AlbLogicLevel::One => 1,
                AlbLogicLevel::Unknown => 2,
            };
            writeln!(
                result,
                "Asr_{suffix} [{}] {destination} {destination}_sink {destination}_source 0 Msr_{suffix}",
                nets.join(" ")
            )?;
            writeln!(
                result,
                ".model Msr_{suffix} sr_alb_lut_bit(table_values=\"{table}\" delay={:e} input_load=0 out_res={resistance:e} min_sink={:e} max_source={:e} ic={ic} inertial_delay=true unknown_as_zero=true)",
                bit.delay_s, electrical.min_sink_a, electrical.max_source_a
            )?;
        }
        if needs_tie0 {
            result.push_str("Asr_tie0 sr_tie0 Msr_tie0\n");
            result.push_str(".model Msr_tie0 d_pulldown(load=0)\n");
        }
        if used_outputs.len() != self.output_width {
            bail!(
                "XSPICE graph drives {} of {} output pins",
                used_outputs.len(),
                self.output_width
            );
        }
        Ok(())
    }
}

impl TimedCombinationalGraph {
    /// Produce a topologically ordered native bit-LUT compiler artifact.
    ///
    /// # Errors
    /// Fails for unresolved initial expressions or LUT compilation errors.
    pub fn xspice_timed_plan(&self) -> Result<XspiceTimedPlan> {
        let compiled = self.known_state_luts()?;
        let mut bits = Vec::with_capacity(compiled.stages.len() + compiled.outputs.len());
        for stage in &self.stages {
            let initial = stage
                .initial
                .evaluate(EvalContext {
                    values: &BTreeMap::new(),
                    readonly: &self.readonly,
                    input_bits: &[],
                })
                .with_context(|| format!("initial state of XSPICE stage {}", stage.name))?;
            for index in 0..stage.width {
                let lut = compiled
                    .stages
                    .iter()
                    .find(|entry| entry.name == stage.name && entry.bit == index)
                    .with_context(|| format!("missing stage LUT {}[{index}]", stage.name))?;
                bits.push(XspiceTimedBit {
                    target: XspiceLutTarget::Stage {
                        name: stage.name.clone(),
                        bit: index,
                    },
                    sources: lut.lut.sources.clone(),
                    table_values: lut.lut.d_genlut_values(),
                    delay_s: stage.delay,
                    initial: if initial & (1_u32 << index) == 0 {
                        AlbLogicLevel::Zero
                    } else {
                        AlbLogicLevel::One
                    },
                });
            }
        }
        for output in &self.outputs {
            for index in 0..=output.last - output.first {
                let lut = compiled
                    .outputs
                    .iter()
                    .find(|entry| entry.name == output.name && entry.bit == index)
                    .with_context(|| format!("missing output LUT {}[{index}]", output.name))?;
                bits.push(XspiceTimedBit {
                    target: XspiceLutTarget::Output {
                        name: output.name.clone(),
                        pin: output.first + index,
                    },
                    sources: lut.lut.sources.clone(),
                    table_values: lut.lut.d_genlut_values(),
                    delay_s: output.delay,
                    initial: AlbLogicLevel::Unknown,
                });
            }
        }
        Ok(XspiceTimedPlan {
            input_width: self.input_width,
            output_width: self.output_width,
            bits,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{TimingEnvironment, analyze_body};
    use super::{
        AlbLogicLevel, Context, KnownLutSourceBit, Result, XspiceElectrical, XspiceTimedPlan,
    };

    const ELECTRICAL: XspiceElectrical = XspiceElectrical {
        input_load_f: 0.0,
        stage_out_res_ohm: 0.0,
        output_out_res_ohm: 0.0,
        input_sink_current_a: 0.0,
        input_source_current_a: 0.0,
        min_sink_a: -1e99,
        max_source_a: 1e99,
    };

    fn compile(body: &str) -> Result<XspiceTimedPlan> {
        let (_, program) = analyze_body(body)?.into_parts();
        let graph = program
            .context("no ALB COMB body")?
            .timed_combinational_graph(TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
            })?
            .context("unsupported timed graph")?;
        graph.xspice_timed_plan()
    }

    #[test]
    fn emits_stage_order_delays_and_output_enable() -> Result<()> {
        let plan = compile(
            r"
PORT A in[0] ;
PORT OE in[1] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D1 = 1 ;
COMB (WIDTH=1, DELAY=3n) D2 = 0 ;
D1 = A ;
D2 = ~D1 ;
Y = D2 ;
Y.En = OE ;
",
        )?;
        assert_eq!((plan.input_width(), plan.output_width()), (2, 1));
        assert_eq!(plan.bits().len(), 3);
        assert_eq!(plan.bits()[0].initial, AlbLogicLevel::One);
        assert_eq!(plan.bits()[1].initial, AlbLogicLevel::Zero);
        assert_eq!(plan.bits()[0].table_values, "01");
        assert_eq!(plan.bits()[1].table_values, "10");
        assert_eq!(plan.bits()[2].table_values, "Z0Z1");
        assert_eq!(
            plan.bits()[2].sources,
            [
                KnownLutSourceBit::Input(1),
                KnownLutSourceBit::Stage {
                    name: "D2".to_owned(),
                    bit: 0
                }
            ]
        );
        let rendered = plan.render_subcircuit("ALB_STAGE", ELECTRICAL)?;
        assert!(rendered.starts_with(".subckt ALB_STAGE sr_in0 sr_in0_sink sr_in0_source sr_in1 sr_in1_sink sr_in1_source sr_out0 sr_out0_sink sr_out0_source\n"));
        assert!(
            rendered.contains("Asr_s0 [sr_in0] sr_stage0 sr_stage0_sink sr_stage0_source 0 Msr_s0")
        );
        assert!(
            rendered
                .contains("Asr_s1 [sr_stage0] sr_stage1 sr_stage1_sink sr_stage1_source 0 Msr_s1")
        );
        assert!(
            rendered
                .contains("Asr_o2 [sr_in1 sr_stage1] sr_out0 sr_out0_sink sr_out0_source 0 Msr_o2")
        );
        assert!(rendered.contains("table_values=\"Z0Z1\""));
        assert!(rendered.contains("ic=1 inertial_delay=true"));
        assert!(rendered.contains("inertial_delay=true unknown_as_zero=true"));
        Ok(())
    }

    #[test]
    fn zero_fanin_is_independent_of_input_pins() -> Result<()> {
        let plan = compile(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D = 1 ;
D = 1 ;
Y = D ;
",
        )?;
        assert_eq!(plan.bits()[0].sources, Vec::<KnownLutSourceBit>::new());
        let rendered = plan.render_subcircuit("ALB_CONSTANT", ELECTRICAL)?;
        assert!(
            rendered
                .contains("Asr_s0 [sr_tie0] sr_stage0 sr_stage0_sink sr_stage0_source 0 Msr_s0")
        );
        assert!(rendered.contains("table_values=\"11\""));
        assert!(rendered.contains(".model Msr_tie0 d_pulldown(load=0)"));
        Ok(())
    }

    #[test]
    fn physical_pin_load_is_counted_once_even_with_shared_or_unused_inputs() -> Result<()> {
        let plan = compile(
            r"
PORT A in[0] ;
PORT UNUSED in[1] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D0 ;
COMB (WIDTH=1, DELAY=3n) D1 ;
D0 = A ;
D1 = A ;
Y = D0 | D1 ;
",
        )?;
        let rendered = plan.render_subcircuit(
            "ALB_LOAD",
            XspiceElectrical {
                input_load_f: 2e-12,
                input_sink_current_a: -1e-2,
                input_source_current_a: 1e-2,
                min_sink_a: -2.1e-2,
                max_source_a: 2.1e-2,
                ..ELECTRICAL
            },
        )?;
        assert_eq!(rendered.matches("Asr_load0 sr_in0 ").count(), 1);
        assert_eq!(rendered.matches("Asr_load1 sr_in1 ").count(), 1);
        assert_eq!(rendered.matches("Asr_s0 [sr_in0] ").count(), 1);
        assert_eq!(rendered.matches("Asr_s1 [sr_in0] ").count(), 1);
        assert!(rendered.contains("capacitance=2e-12 sink_current=-1e-2 source_current=1e-2"));
        assert_eq!(rendered.matches("input_load=0 out_res=").count(), 3);
        Ok(())
    }

    #[test]
    fn rejects_unsafe_params_and_unavailable_native_width() -> Result<()> {
        let plan = compile(
            r"
PORT A in[0] ;
PORT Y out[0] ;
COMB (WIDTH=1, DELAY=2n) D ;
D = A ;
Y = D ;
",
        )?;
        assert!(plan.render_subcircuit("invalid.name", ELECTRICAL).is_err());
        assert!(
            plan.render_subcircuit(
                "ALB_TEST",
                XspiceElectrical {
                    input_load_f: f64::NAN,
                    ..ELECTRICAL
                }
            )
            .is_err()
        );
        let mut too_wide = plan;
        too_wide.bits[0].sources = (0..17).map(KnownLutSourceBit::Input).collect();
        too_wide.bits[0].table_values = "0".repeat(1 << 17);
        assert!(
            too_wide
                .render_subcircuit("ALB_TEST", ELECTRICAL)
                .err()
                .is_some_and(|error| error.to_string().contains("16-source"))
        );
        Ok(())
    }
}
