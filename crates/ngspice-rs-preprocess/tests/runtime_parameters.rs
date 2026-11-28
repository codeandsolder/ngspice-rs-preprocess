mod prepared_diode;

use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path, process::Command};

#[test]
fn simetrix_runtime_parameter_reference_cases() -> Result<()> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-parameters");
    for line in include_str!("fixtures/runtime-parameters/outcomes.tsv").lines() {
        let (name, outcome) = line.split_once('\t').context("invalid fixture row")?;
        run_case(&fixtures, name, outcome, 1e-9, 0.).with_context(|| format!("case {name}"))?;
    }
    Ok(())
}

#[test]
fn simetrix_runtime_builtin_reference_cases() -> Result<()> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-builtins");
    for line in include_str!("fixtures/runtime-builtins/outcomes.tsv").lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        let [name, outcome, abs, rel] = fields.as_slice() else {
            anyhow::bail!("invalid reference row");
        };
        let (abs, rel) = if *outcome == "error" {
            (0., 0.)
        } else {
            (abs.parse()?, rel.parse()?)
        };
        run_case(&fixtures, name, outcome, abs, rel).with_context(|| format!("case {name}"))?;
    }
    Ok(())
}

fn run_case(fixtures: &Path, name: &str, outcome: &str, abs: f64, rel: f64) -> Result<()> {
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
    if outcome == "error" {
        let error = result.err().context("wrong arity accepted")?;
        ensure!(
            format!("{error:#}").contains("requires one argument"),
            "{error:#}"
        );
        return Ok(());
    }
    let bundle = result?;
    if name == "diode-temp" {
        return measured_scoped_diode_temperatures(&bundle, fixtures, abs, rel);
    }

    if matches!(
        name,
        "global-model" | "instance-model" | "instance-precedence"
    ) {
        return measured_classic_bjt_temperatures(&bundle, fixtures, name);
    }

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
            (actual - expected).abs() <= rel.mul_add(expected.abs(), abs),
            "actual {actual}, expected {expected}"
        );
    }
    Ok(())
}

fn measured_scoped_diode_temperatures(
    bundle: &ngspice_rs_preprocess::PreparedBundle,
    fixtures: &Path,
    abs: f64,
    rel: f64,
) -> Result<()> {
    use model_kernel_diode::{Instance, MODEL_FAMILY, compile_simetrix};
    let source = prepared_diode::linked_library(bundle)?;
    let topology = source
        .instantiate(model_normalize_ngspice::parameter_scope::topology::Limits::default())?;
    let mut currents = Vec::new();
    for frame in topology.frames.iter().skip(1) {
        let model = source
            .models
            .iter()
            .find(|model| {
                model.scope == frame.declaration
                    && model.card.family.eq_ignore_ascii_case(MODEL_FAMILY)
            })
            .context("missing scoped level-1 diode")?;
        let overrides = frame
            .overrides
            .iter()
            .map(|(name, value)| (frame.declaration, name.as_str(), *value))
            .collect::<Vec<_>>();
        let values = model
            .card
            .parameters
            .iter()
            .map(|p| {
                Ok((
                    p.name.as_str(),
                    source.evaluate_scalar(
                        model.scope,
                        p.expression.as_deref().context("missing value")?,
                        &overrides,
                    )?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let diode = compile_simetrix(values)?.bind(Instance {
            analysis_temperature: 358.15,
            ..Instance::default()
        })?;
        currents.push(-diode.evaluate(0.65)?.current);
    }
    let csv = fs::read_to_string(fixtures.join("diode-temp.csv"))?;
    let expected = csv
        .lines()
        .nth(1)
        .context("missing diode temperature reference")?
        .split(',')
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        currents.len() == expected.len(),
        "diode temperature reference shape"
    );
    for (actual, expected) in currents.into_iter().zip(expected) {
        ensure!(
            (actual - expected).abs() <= rel.mul_add(expected.abs(), abs),
            "diode temperature: {actual} vs {expected}"
        );
    }
    Ok(())
}

// The three fixtures are fixed-bias, forward-active, ideal Gummel–Poon
// transistors. Stock ngspice cannot execute the explicitly selected legacy
// model, and substituting its native BJT would change the physical law.
// Bind the emitted model and instance parameters in their actual lexical
// occurrence, then evaluate the independent ideal transport equation.
fn measured_classic_bjt_temperatures(
    bundle: &ngspice_rs_preprocess::PreparedBundle,
    fixtures: &Path,
    name: &str,
) -> Result<()> {
    use model_normalize_ngspice::{
        bjt_card::{BjtPolarity, BjtThermalConvention, ClassicBjtFamily},
        linked_source::RootKind,
        model_card::{instance_fields, positional_prefix},
        parameter_scope::{ModelLibrary, topology::Limits},
    };
    let linked = prepared_diode::linked_source(bundle)?;
    let source = ModelLibrary::parse_linked(&linked, RootKind::Deck)?;
    let topology = source.instantiate(Limits::default())?;
    let text = std::str::from_utf8(&linked.bytes)?;
    let mut cards = vec![Vec::new(); source.scopes.len()];
    let mut ambient = None;
    for (index, line) in text.lines().enumerate().skip(1) {
        let line = line.trim();
        if line
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case(".temp"))
        {
            ambient = Some(source.evaluate_scalar(0, line[5..].trim(), &[])?);
        }
        if !line
            .as_bytes()
            .first()
            .is_some_and(|b| b.eq_ignore_ascii_case(&b'Q'))
        {
            continue;
        }
        let scope = source
            .scopes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, scope)| {
                scope.parent.is_some()
                    && scope.source_line < index + 1
                    && index + 1 < scope.end_line
            })
            .map_or(0, |(id, _)| id);
        cards[scope].push(line);
    }
    let ambient = ambient.context("missing fixture ambient temperature")?;
    let mut currents = Vec::new();
    for frame in &topology.frames {
        let overrides = frame
            .overrides
            .iter()
            .map(|(key, value)| (frame.declaration, key.as_str(), *value))
            .collect::<Vec<_>>();
        for card in &cards[frame.declaration] {
            let (prefix, tail) = positional_prefix(card);
            ensure!(
                prefix.len() == 5,
                "expected three-terminal fixture BJT: {card}"
            );
            let model_name = prefix[4].0;
            let mut scope = frame.declaration;
            let model = loop {
                if let Some(model) = source.models.iter().find(|model| {
                    model.scope == scope && model.card.name.eq_ignore_ascii_case(model_name)
                }) {
                    break model;
                }
                scope = source.scopes[scope]
                    .parent
                    .context("missing fixture BJT model")?;
            };
            ensure!(
                ClassicBjtFamily::parse(&model.card.family)
                    == Some(ClassicBjtFamily {
                        polarity: BjtPolarity::Npn,
                        thermal: BjtThermalConvention::Legacy,
                    }),
                "legacy BJT selection was lost"
            );
            let mut saturation = None;
            let mut nominal = 27.0;
            for parameter in &model.card.parameters {
                let value = source.evaluate_scalar(
                    model.scope,
                    parameter
                        .expression
                        .as_deref()
                        .context("missing BJT model value")?,
                    &overrides,
                )?;
                match parameter.name.to_ascii_lowercase().as_str() {
                    "is" => saturation = Some(value),
                    "tnom" => nominal = value,
                    "bf" => ensure!(value == 100.0, "unexpected fixture BF"),
                    "level" => ensure!(value == 1.0, "unexpected fixture LEVEL"),
                    other => anyhow::bail!("fixture transport equation cannot evaluate {other}"),
                }
            }
            let mut temperature = ambient;
            for parameter in instance_fields(tail, 2)? {
                ensure!(
                    parameter.name.eq_ignore_ascii_case("temp"),
                    "unexpected fixture instance field {}",
                    parameter.name
                );
                temperature = source.evaluate_scalar(
                    frame.declaration,
                    parameter.expression.as_deref().context("missing TEMP")?,
                    &overrides,
                )?;
            }
            let kelvin = temperature + 273.15;
            let ratio = kelvin / (nominal + 273.15);
            // Historical SPICE k and q, default EG=1.11, XTI=3 and NF=1.
            let vt = (1.3806226e-23 / 1.6021918e-19) * kelvin;
            let isat = saturation.context("missing fixture IS")?
                * ratio.powi(3)
                * (1.11 * (ratio - 1.0) / vt).exp();
            // VBE=.65, VCE=2 in these independent fixed-bias fixtures.
            currents.push(-isat * (0.65 / vt).exp_m1());
        }
    }
    let csv = fs::read_to_string(fixtures.join(format!("{name}.csv")))?;
    let expected = csv
        .lines()
        .nth(1)
        .context("missing BJT reference row")?
        .split(',')
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(
        currents.len() == expected.len(),
        "BJT temperature reference shape"
    );
    for (actual, expected) in currents.into_iter().zip(expected) {
        ensure!(
            (actual - expected).abs() <= 1e-10 + 1e-7 * expected.abs(),
            "legacy BJT transport: {actual} vs {expected}"
        );
    }
    Ok(())
}

#[test]
fn predefined_symbols_do_not_rename_electrical_objects() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("electrical.net"), b"* electrical names\n.temp 85\n.param a={PI} ; inactive GAUSS(\nV1 TEMP 0 2\nV2 PI 0 3\nB1 out 0 V=V(TEMP)+V(PI)+TEMP+a\n.op\n.end\n")?;
    fs::write(
        dir.path().join("electrical.csv"),
        format!("axis,v(out)\n0,{}\n", 90.0 + std::f64::consts::PI),
    )?;
    run_case(dir.path(), "electrical", "pass", 1e-10, 0.)
}

#[test]
fn reserved_parameter_declarations_fail_before_hygienic_renaming() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("reserved.net");
    for deck in [
        ".param PI=2\nV1 a 0 {PI}\n.end\n",
        "X1 a 0 T params: PI=2\n.subckt T p n\nR1 p n {PI}\n.ends\n.end\n",
        "X1 a 0 T\n.subckt T p n params: e=3\nR1 p n {e}\n.ends\n.end\n",
        "X1 a 0 T\n.subckt T p n\n.param BOLTZ=2\nR1 p n {BOLTZ}\n.ends\n.end\n",
    ] {
        fs::write(&source, format!("* reserved declaration\n{deck}"))?;
        let request = Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(source.clone()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        };
        let error = prepare(&request, &Options::default(), &Cancellation::default())
            .err()
            .context("predefined declaration accepted")?;
        ensure!(
            format!("{error:#}").contains("predefined parameter"),
            "{error:#}"
        );
    }
    Ok(())
}

#[test]
fn generated_helpers_do_not_capture_user_functions_or_parameters() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("helpers.net"), b"* helper collisions\n.temp 85 ; comment\n.param sr_simetrix_analysis_temp=3\n.func sr_simetrix_lncosh(x) {2*x}\nV1 x 0 {sr_simetrix_analysis_temp+TEMP}\nB1 y 0 V=sr_simetrix_lncosh(3)+lncosh(2)\n.op\n.end\n")?;
    fs::write(
        dir.path().join("helpers.csv"),
        format!("axis,v(x),v(y)\n0,88,{}\n", 6.0 + 2.0_f64.cosh().ln()),
    )?;
    run_case(dir.path(), "helpers", "pass", 1e-10, 0.)
}

#[test]
fn nominal_preparation_rejects_ambiguous_temperature_and_statistical_analysis() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("unsupported.net");
    for (directive, diagnostic) in [
        (".temp 27 85", "one numeric analysis temperature"),
        (
            ".temp 27\n.options TEMP=85",
            "conflicting SIMetrix analysis temperatures",
        ),
        (".temp -274", "invalid SIMetrix analysis temperature"),
        (".montecarlo 10", "statistical analysis is not implemented"),
        (
            ".model DM D(IS=1e-15 T_ABS=-300)",
            "invalid SIMetrix model temperature",
        ),
    ] {
        fs::write(
            &source,
            format!("* unsupported analysis\nV1 a 0 1\n{directive}\n.end\n"),
        )?;
        let request = Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(source.clone()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        };
        let error = prepare(&request, &Options::default(), &Cancellation::default())
            .err()
            .context("unsupported preparation accepted")?;
        ensure!(format!("{error:#}").contains(diagnostic), "{error:#}");
    }
    Ok(())
}

#[test]
fn simetrix_jfet_gate_current_temperature_reference() -> Result<()> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jfet-temperature");
    for temperature in [27, 65, 85] {
        let name = format!("temp-{temperature}");
        run_case(&fixtures, &name, "pass", 1e-13, 1e-3)
            .with_context(|| format!("JFET temperature {temperature} C"))?;
    }
    Ok(())
}

#[test]
fn jfet_default_is_specific_to_simetrix_projection() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("models.net");
    fs::write(&source, b"* dialect-specific JFET\n.temp 85\nV1 d 0 2\nV2 g 0 -2\nJ1 d g 0 MOD\n.model MOD NJF(IS=15p)\n.end\n")?;
    for (format, expected) in [
        (InputFormat::SimetrixDeck, true),
        (InputFormat::SimplisDeck, false),
        (InputFormat::NativeDeck, false),
    ] {
        let request = Request {
            format,
            input: SourceInput::File(source.clone()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        };
        let bundle = prepare(&request, &Options::default(), &Cancellation::default())?;
        let text = std::str::from_utf8(&bundle.primary.bytes)?;
        ensure!(
            text.to_ascii_uppercase().contains("XTI=3") == expected,
            "{format:?}: {text}"
        );
    }
    Ok(())
}
