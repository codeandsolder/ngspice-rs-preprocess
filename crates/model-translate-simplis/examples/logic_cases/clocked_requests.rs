//! Independent clocked state and propagation timeline for the code-model ABI.
use super::{check_edges, event_edges, run_native_deck};
use anyhow::Result;
use std::{fmt::Write as _, path::Path};

// Active clock is abstract: falling cases complement the physical waveform.
const TIMELINE: [(u32, bool, bool, bool); 19] = [
    (0, true, true, true),
    (200, true, false, false),
    (500, true, false, true),
    (600, false, true, true),
    (700, false, true, false),
    (800, true, true, false),
    (850, true, true, true),
    (950, true, true, false),
    (1100, true, true, true),
    (1200, false, false, true),
    (1300, false, false, false),
    (1800, false, true, false),
    (2000, false, true, true),
    (2200, true, false, true),
    (2400, true, false, false),
    (2600, true, false, true),
    (2700, false, true, true),
    (3000, false, true, false),
    (3200, false, true, true),
];

fn stimulus(source: &mut String) -> Result<()> {
    for (column, name) in ["data", "reset", "rise", "fall"].into_iter().enumerate() {
        write!(source, "V{name} {name} 0 PWL(")?;
        let mut previous = 0;
        for (index, &(time, data, reset, active)) in TIMELINE.iter().enumerate() {
            let value = if [data, reset, active, !active][column] {
                5
            } else {
                0
            };
            if index == 0 {
                write!(source, "0 {value}")?;
            } else if previous != value {
                write!(source, " {time}n {previous} {}n {value}", time + 40)?;
            }
            previous = value;
        }
        writeln!(source, " 4000n {previous})")?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct CapturedCase {
    operation: u8,
    initial: bool,
    inertial: bool,
}

impl CapturedCase {
    fn emit(self, source: &mut String, index: usize) -> Result<()> {
        let clock = if self.operation % 2 == 1 {
            "rise"
        } else {
            "fall"
        };
        let reset = if self.operation <= 10 { " reset" } else { "" };
        writeln!(
            source,
            "Aevent{index} %v[data{reset} {clock}] q{index} m{index}"
        )?;
        writeln!(
            source,
            ".model m{index} preprocess_logic_event(operation={} delay=400n inertial={} ic={})",
            self.operation,
            self.inertial,
            u8::from(self.initial)
        )?;
        writeln!(
            source,
            "Abridge{index} q{index} out{index} bridge\nR{index} out{index} 0 1k"
        )?;
        Ok(())
    }

    fn capture(self, previous: bool, data: bool, reset: bool) -> bool {
        match self.operation {
            7..=10 if data != reset => data,
            9 | 10 if data && reset => !previous,
            11 | 12 => data,
            13 | 14 if data => !previous,
            _ => previous,
        }
    }

    fn expected(self) -> Vec<(f64, bool)> {
        let mut requested = self.initial;
        let mut committed = self.initial;
        let mut deadline = None;
        let mut events = Vec::new();
        let mut old_active = TIMELINE[0].3;
        for &(time, data, reset, active) in &TIMELINE[1..] {
            let edge = time + 24;
            if let Some((due, bit)) = deadline
                && due < edge
            {
                committed = bit;
                events.push((f64::from(due) * 1e-9, bit));
                deadline = None;
            }
            if !old_active && active {
                let next = self.capture(requested, data, reset);
                if next != requested {
                    requested = next;
                    if self.inertial {
                        deadline = (next != committed).then_some((edge + 400, next));
                    } else {
                        events.push((f64::from(edge + 400) * 1e-9, next));
                    }
                }
            }
            old_active = active;
        }
        if let Some((due, bit)) = deadline {
            events.push((f64::from(due) * 1e-9, bit));
        }
        events
    }
}

pub(super) fn qualify(runtime: &Path) -> Result<()> {
    let mut source = String::from(
        "* delayed clocked request regression\n.model bridge preprocess_logic_voltage(out_high=5)\n",
    );
    stimulus(&mut source)?;
    let mut initials = Vec::new();
    let mut expected = Vec::new();
    let mut control = String::from("set numdgt=17\ntran 10n 4u 0 10n\neprint");
    for operation in 7..=14 {
        for initial in [false, true] {
            for inertial in [false, true] {
                let case = CapturedCase {
                    operation,
                    initial,
                    inertial,
                };
                let index = initials.len();
                case.emit(&mut source, index)?;
                initials.push(initial);
                expected.push(case.expected());
                write!(control, " q{index}")?;
            }
        }
    }
    source.push_str(".end\n");
    control.push('\n');
    let stdout = run_native_deck(runtime, &source, &control)?;
    let initials: [bool; 32] = initials
        .try_into()
        .map_err(|_| anyhow::anyhow!("incorrect capture matrix width"))?;
    let expected = expected.iter().map(Vec::as_slice).collect::<Vec<_>>();
    check_edges(&event_edges(&stdout, initials), &expected)?;
    println!(
        "Clocked propagation: 32 rising/falling SR/JK/D/T cases, {} exact delayed transitions",
        expected.iter().map(|trace| trace.len()).sum::<usize>()
    );
    Ok(())
}
