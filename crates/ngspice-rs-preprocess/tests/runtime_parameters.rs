use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path, process::Command};

#[test]
fn simetrix_runtime_parameter_reference_cases() -> Result<()> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-parameters");
    for line in include_str!("fixtures/runtime-parameters/outcomes.tsv").lines() {
        let (name, outcome) = line.split_once('\t').context("invalid fixture row")?;
        run_case(&fixtures, name, outcome).with_context(|| format!("case {name}"))?;
    }
    Ok(())
}

fn run_case(fixtures: &Path, name: &str, outcome: &str) -> Result<()> {
    let request = Request {
        format: InputFormat::SimetrixDeck,
        input: SourceInput::File(fixtures.join(format!("{name}.net"))),
        libraries: Vec::new(),
        search_paths: Vec::new(),
    };
    let result = prepare(&request, &Options::default(), &Cancellation::default());
    if outcome == "duplicate" {
        let error = result.err().context("duplicate declaration accepted")?;
        ensure!(
            format!("{error:#}").contains("has already been defined"),
            "{error:#}"
        );
        return Ok(());
    }
    let bundle = result?;
    let dir = tempfile::tempdir()?;
    for artifact in &bundle.companions {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let (vectors, expected) = if outcome == "pass" {
        let csv = fs::read_to_string(fixtures.join(format!("{name}.csv")))?;
        let mut rows = csv.lines();
        let vectors = rows
            .next()
            .context("missing reference columns")?
            .split(',')
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ");
        let values = rows
            .next()
            .context("missing reference row")?
            .split(',')
            .skip(1)
            .map(str::parse::<f64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        (vectors, values)
    } else {
        ensure!(outcome == "undefined", "invalid outcome {outcome}");
        (String::from("v(x)"), Vec::new())
    };
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing deck terminator")?
        .to_vec();
    deck.extend_from_slice(format!(".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat {vectors}\nquit\n.endc\n.end\n").as_bytes());
    fs::write(dir.path().join("reference.cir"), deck)?;
    let execution = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "reference.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    let data = dir.path().join("op.dat");
    if outcome == "undefined" {
        ensure!(
            !data.exists() && log.to_ascii_lowercase().contains("undefined parameter [r]"),
            "{log}"
        );
        return Ok(());
    }
    ensure!(
        execution.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    let row = fs::read_to_string(data)?;
    let actual = row
        .split_ascii_whitespace()
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(actual.len() == expected.len(), "reference shape mismatch");
    for (actual, expected) in actual.into_iter().zip(expected) {
        ensure!(
            (actual - expected).abs() < 1e-9,
            "actual {actual}, expected {expected}"
        );
    }
    Ok(())
}
