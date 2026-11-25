//! Classic clocked state elements through the unified prepared bundle.

use anyhow::{Context, Result};
use model_translate_simplis::{CodeModelRuntime, TranslationKind, translate_simplis_deck};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path};

const FIELDS: &str = "TH=2.5 HYSTWD=1 VOL=0.5 VOH=4.5 RIN=10Meg ROUT=10 LOGIC=POS";

fn deck(device: &str, model: &str, extra: &str) -> String {
    format!("* state witness\n{device}\n.MODEL M {model} {FIELDS} {extra}\n.end\n")
}

#[test]
fn all_state_model_families_share_one_event_state() -> Result<()> {
    for (model, pins, selector, operation, kind) in [
        (
            "CLK_SRFF",
            "q qb 0 s r clk",
            "TRIG_COND=0_TO_1",
            7,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_SRFF",
            "q qb 0 s r clk",
            "TRIG_COND=1_TO_0",
            8,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_JKFF",
            "q qb 0 j k clk",
            "TRIG_COND=0_TO_1",
            9,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_JKFF",
            "q qb 0 j k clk",
            "TRIG_COND=1_TO_0",
            10,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_DFF",
            "q qb 0 d clk",
            "TRIG_COND=0_TO_1",
            11,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_DFF",
            "q qb 0 d clk",
            "TRIG_COND=1_TO_0",
            12,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_TFF",
            "q qb 0 t clk",
            "TRIG_COND=0_TO_1",
            13,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "CLK_TFF",
            "q qb 0 t clk",
            "TRIG_COND=1_TO_0",
            14,
            TranslationKind::ClockedFlipFlop,
        ),
        (
            "LATCH",
            "q 0 d en",
            "ENABLE_LEVEL=1",
            15,
            TranslationKind::DataLatch,
        ),
        (
            "LATCH",
            "q 0 d en",
            "ENABLE_LEVEL=0",
            16,
            TranslationKind::DataLatch,
        ),
    ] {
        let input = deck(&format!("!D1 {pins} M IC = 1"), model, selector);
        let output = translate_simplis_deck(input.as_bytes())?;
        assert_eq!(output.primitives, [kind]);
        assert_eq!(output.required_code_models, [CodeModelRuntime::Logic]);
        let text = std::str::from_utf8(&output.bytes)?;
        assert_eq!(text.matches("preprocess_logic_event(").count(), 1);
        assert!(text.contains(&format!("operation={operation} ")));
        assert!(text.contains("inertial=false ic=1"));
        assert_eq!(
            text.matches("preprocess_logic_voltage(").count(),
            if model == "LATCH" { 1 } else { 2 }
        );
        assert_eq!(
            text.matches("_RIN_I").count(),
            if model == "CLK_SRFF" || model == "CLK_JKFF" {
                3
            } else {
                2
            }
        );
    }
    Ok(())
}

#[test]
fn rejects_missing_and_unsupported_classic_state_fields() {
    for (device, model, extra) in [
        ("!D1 q qb 0 d c M IC=0", "CLK_DFF", ""),
        ("!D1 q qb 0 d c M IC=0", "CLK_DFF", "TRIG_COND=ANY"),
        (
            "!D1 q qb 0 d c M IC=0",
            "CLK_DFF",
            "TRIG_COND=0_TO_1 DELAY=1n",
        ),
        (
            "!D1 q qb 0 d c M IC=0",
            "CLK_DFF",
            "TRIG_COND=0_TO_1 SETUP=1n",
        ),
        ("!D1 q qb 0 d c M IC=0", "CLK_DFF", "TRIG_COND=0_TO_1 TH=3"),
        ("!D1 q qb 0 d c M IC=0", "CLK_DFF", "TRIG_COND=0_TO_1 stray"),
        ("!D1 q qb 0 d c M", "CLK_DFF", "TRIG_COND=0_TO_1"),
        ("!D1 q qb 0 d c M IC=2", "CLK_DFF", "TRIG_COND=0_TO_1"),
        ("!D1 q qb 0 d c M IC=1 IC=0", "CLK_DFF", "TRIG_COND=0_TO_1"),
        ("!D1 q qb 0 d M IC=1", "CLK_DFF", "TRIG_COND=0_TO_1"),
        ("!D1 q qb 0 d c extra M IC=1", "CLK_DFF", "TRIG_COND=0_TO_1"),
        ("!D1 q qb 0 d c M IC=1", "LATCH", "ENABLE_LEVEL=0"),
        ("!D1 q 0 d c M IC=1", "LATCH", "ENABLE_LEVEL=2"),
    ] {
        let source = deck(device, model, extra);
        assert!(
            translate_simplis_deck(source.as_bytes()).is_err(),
            "{source}"
        );
    }
}

fn prepare_file(dir: &Path, source: &str) -> Result<ngspice_rs_preprocess::PreparedBundle> {
    let path = dir.join("clocked.net");
    fs::write(&path, source)?;
    prepare(
        &Request {
            format: InputFormat::SimplisDeck,
            input: SourceInput::File(path),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
}

#[test]
fn hierarchical_snapshot_initializes_each_clocked_occurrence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = format!(
        r"* repeated clocked instances
X1 a ab d clk CELL
X2 b bb d clk CELL
.INIT X1.!D1=1 X2.!D1=0
.subckt CELL q qb d c
!D1 q qb 0 d c M IC=0
.MODEL M CLK_DFF {FIELDS} TRIG_COND=0_TO_1
.ends CELL
.end
"
    );
    let bundle = prepare_file(temp.path(), &source)?;
    assert_eq!(bundle.required_code_models(), [CodeModelRuntime::Logic]);
    let mut states = Vec::new();
    for artifact in bundle.artifacts() {
        let text = std::str::from_utf8(&artifact.bytes)?;
        for line in text
            .lines()
            .filter(|line| line.contains("preprocess_logic_event("))
        {
            let (_, initial) = line.split_once(" ic=").context("missing initial state")?;
            states.push(
                initial
                    .split_whitespace()
                    .next()
                    .context("missing initial value")?
                    .to_owned(),
            );
        }
    }
    states.sort();
    assert_eq!(states, ["0", "1"]);
    Ok(())
}
