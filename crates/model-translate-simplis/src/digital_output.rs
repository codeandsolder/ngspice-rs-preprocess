//! Shared electrical output stage. Analog and event-timed state producers
//! retain their own receiver, initialization and timing semantics.
use super::{LogicIo, LogicPolarity, write_number};
use anyhow::Result;
use std::{borrow::Cow, fmt::Write as _, iter::once};

#[derive(Clone, Copy)]
pub struct OutputPins<'a> {
    pub primary: &'a str,
    pub complement: Option<&'a str>,
}

impl<'a> OutputPins<'a> {
    pub fn iter(self) -> impl Iterator<Item = (&'a str, bool)> {
        once((self.primary, false)).chain(self.complement.map(|node| (node, true)))
    }
}

#[derive(Clone, Copy)]
pub struct OutputDrive {
    pub low: f64,
    pub high: f64,
    pub resistance: f64,
}

impl From<LogicIo> for OutputDrive {
    fn from(io: LogicIo) -> Self {
        Self {
            low: io.vol,
            high: io.voh,
            resistance: io.rout,
        }
    }
}

#[derive(Clone, Copy)]
pub enum OutputSignal<'a> {
    /// A scalar analog 0/1 state requiring its own transport delay.
    Analog { state: &'a str, delay: f64 },
    /// A native digital state with timing already applied in the event producer.
    Event(&'a str),
}

impl<'a> OutputSignal<'a> {
    fn timed_state(self, output: &mut String, stem: &str) -> Result<Cow<'a, str>> {
        match self {
            Self::Analog { state, delay } if delay != 0.0 => {
                let delayed = format!("__{stem}_DELAYED");
                let model = format!("__{stem}_DELAY_M");
                writeln!(output, "A__{stem}_DELAY {state} {delayed} 0 {model}")?;
                write!(output, ".model {model} delay(delay=")?;
                write_number(output, delay)?;
                output.push_str(" buffer_size=2048)\n");
                Ok(Cow::Owned(delayed))
            }
            Self::Analog { state, .. } | Self::Event(state) => Ok(Cow::Borrowed(state)),
        }
    }
}

pub struct OutputStage<'a> {
    pub stem: &'a str,
    pub rtn: &'a str,
    pub pins: OutputPins<'a>,
    pub drive: OutputDrive,
    pub polarity: LogicPolarity,
    pub signal: OutputSignal<'a>,
}

impl OutputStage<'_> {
    pub fn emit(self, output: &mut String) -> Result<()> {
        let state = self.signal.timed_state(output, self.stem)?;
        for (node, complement) in self.pins.iter() {
            let suffix = if complement { "B" } else { "" };
            let stem = format!("{}_OUT{suffix}", self.stem);
            let internal = format!("__{stem}_INT");
            let inverted = complement ^ self.polarity.negative();
            match self.signal {
                OutputSignal::Analog { .. } => {
                    write!(output, "B__{stem}V {internal} {} V='", self.rtn)?;
                    write_number(output, self.drive.low)?;
                    output.push_str("+(");
                    write_number(output, self.drive.high - self.drive.low)?;
                    if inverted {
                        writeln!(output, ")*(1-v({state}))'")?;
                    } else {
                        writeln!(output, ")*v({state})'")?;
                    }
                }
                OutputSignal::Event(_) => {
                    let rails = if inverted {
                        [self.drive.high, self.drive.low]
                    } else {
                        [self.drive.low, self.drive.high]
                    };
                    emit_logic_voltage(output, &stem, &internal, self.rtn, &state, rails)?;
                }
            }
            write!(output, "R__{stem} {internal} {node} ")?;
            write_number(output, self.drive.resistance)?;
            output.push('\n');
        }
        Ok(())
    }
}

pub fn emit_logic_voltage(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_stage_preserves_delay_and_complement_identity() -> Result<()> {
        for polarity in [LogicPolarity::Positive, LogicPolarity::Negative] {
            for delay in [0.0, 3e-9] {
                let mut analog = String::new();
                OutputStage {
                    stem: "DEMO",
                    rtn: "ref",
                    pins: OutputPins {
                        primary: "q",
                        complement: Some("qb"),
                    },
                    drive: OutputDrive {
                        low: 0.5,
                        high: 4.5,
                        resistance: 10.0,
                    },
                    polarity,
                    signal: OutputSignal::Analog { state: "s", delay },
                }
                .emit(&mut analog)?;
                let state = if delay == 0.0 { "s" } else { "__DEMO_DELAYED" };
                assert_eq!(
                    analog.matches(".model __DEMO_DELAY_M delay(").count(),
                    usize::from(delay != 0.0)
                );
                for (suffix, pin, inverted) in [
                    ("", "q", polarity.negative()),
                    ("B", "qb", !polarity.negative()),
                ] {
                    let line = analog
                        .lines()
                        .find(|line| line.starts_with(&format!("B__DEMO_OUT{suffix}V ")))
                        .ok_or_else(|| anyhow::anyhow!("missing analog output"))?;
                    let expression = if inverted {
                        format!("*(1-v({state}))'")
                    } else {
                        format!("*v({state})'")
                    };
                    assert!(line.ends_with(&expression), "{line}");
                    assert!(analog.contains(&format!(
                        "R__DEMO_OUT{suffix} __DEMO_OUT{suffix}_INT {pin} 1.00000000000000000e1"
                    )));
                }
            }
            let mut event = String::new();
            OutputStage {
                stem: "DEMO",
                rtn: "ref",
                pins: OutputPins {
                    primary: "q",
                    complement: Some("qb"),
                },
                drive: OutputDrive {
                    low: 0.5,
                    high: 4.5,
                    resistance: 10.0,
                },
                polarity,
                signal: OutputSignal::Event("s"),
            }
            .emit(&mut event)?;
            assert!(!event.contains("delay("));
            for (suffix, inverted) in [("", polarity.negative()), ("B", !polarity.negative())] {
                assert!(event.contains(&format!(
                    "A__DEMO_OUT{suffix}V s %vd(__DEMO_OUT{suffix}_INT ref) __DEMO_OUT{suffix}_M"
                )));
                let rails = if inverted {
                    ("4.50000000000000000e0", "5.00000000000000000e-1")
                } else {
                    ("5.00000000000000000e-1", "4.50000000000000000e0")
                };
                assert!(event.contains(&format!(
                    ".model __DEMO_OUT{suffix}_M preprocess_logic_voltage(out_low={} out_high={})",
                    rails.0, rails.1
                )));
            }
        }
        Ok(())
    }
}
