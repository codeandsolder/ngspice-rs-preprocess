//! Reproducible, external ngspice 47 differential checks of genuine VDMOS cards.
//! Usage: `catalog_oracle <catalog> <seed> [--isolated] [--diode] [--ltspice-electrical] [model-name-filter]`
use model_kernel_vdmos::{Bias, Instance, Polarity, PreparedModel, compile_ngspice};
use model_normalize_ngspice::{
    linked_source::SourceLinker, parameter_scope::ModelLibrary, vdmos_card::VdmosPolarity,
    vdmos_ltspice::translate_electrical,
};
use std::{
    error::Error,
    fmt::Write as _,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Random(u64);

impl Random {
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        let bits = u32::try_from((x ^ (x >> 31)) >> 32).unwrap_or_default();
        f64::from(bits) / 4_294_967_296.0
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        self.unit().mul_add(high - low, low)
    }
}

fn oracle<const N: usize>(deck: &str, names: [&str; N]) -> Result<[f64; N]> {
    let mut child = Command::new("ngspice")
        .args(["-n", "-b"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing oracle stdin")?
        .write_all(deck.as_bytes())?;
    let result = child.wait_with_output()?;
    let output = String::from_utf8(result.stdout)?;
    if !result.status.success() || !output.contains("ngspice-47 done") {
        return Err(format!("oracle failed: {}", String::from_utf8_lossy(&result.stderr)).into());
    }
    let mut values = [0.0_f64; N];
    for (value, key) in values.iter_mut().zip(names) {
        *value = output
            .lines()
            .filter_map(|line| line.split_once('='))
            .find(|(left, _)| left.trim() == key)
            .ok_or_else(|| format!("missing {key}: {}", String::from_utf8_lossy(&result.stderr)))?
            .1
            .trim()
            .parse()?;
        if !value.is_finite() {
            return Err(format!("nonfinite reference {key}").into());
        }
    }
    Ok(values)
}

#[derive(Clone, Copy, Debug)]
enum Campaign {
    Channel,
    Diode,
}

#[derive(Clone, Copy, Debug)]
struct OracleOptions {
    campaign: Campaign,
    ltspice_electrical: bool,
}

fn make_deck(
    polarity: Polarity,
    parameters: &[(&str, f64)],
    temp: f64,
    vg: f64,
    vd: f64,
    commands: &str,
) -> Result<String> {
    let flag = if polarity == Polarity::P {
        "pchan"
    } else {
        "nchan"
    };
    let mut deck = format!(
        "catalog oracle\n.options gmin=1e-20 reltol=1e-9 abstol=1e-13 vntol=1e-10\n.temp {temp:e}\nVg g 0 {vg:e}\nVd d 0 {vd:e}\nM1 d g 0 dut\n.model dut vdmos({flag}"
    );
    for (key, value) in parameters {
        write!(deck, "\n+ {key}={value:.17e}")?;
    }
    write!(
        deck,
        ")\n.control\nset numdgt=15\nop\n{commands}\nquit\n.endc\n.end\n"
    )?;
    Ok(deck)
}

fn check_case(
    polarity: Polarity,
    parameters: &[(&str, f64)],
    variant: usize,
    random: &mut Random,
    options: OracleOptions,
) -> Result<()> {
    let mut parameters = parameters.to_vec();
    if variant != 0 {
        for (key, value) in &mut parameters {
            if [
                "kp", "rd", "rs", "rg", "rb", "cgs", "cgso", "cgdmin", "cgdmax", "cjo", "tt",
            ]
            .iter()
            .any(|candidate| key.eq_ignore_ascii_case(candidate))
            {
                *value *= random.range(0.8, 1.2);
            }
        }
    }
    if options.ltspice_electrical {
        parameters = translate_electrical(&parameters)?.electrical;
    }
    let model = compile_ngspice(polarity, parameters.iter().copied())?;
    let sign = polarity.sign();
    let temp = random.range(-25.0, 125.0);
    let threshold = model.parameters().channel.vth * sign;
    let vg = sign
        * (threshold
            + if variant == 0 {
                random.range(-0.3, 0.7)
            } else {
                random.range(0.7, 5.0)
            });
    let vd = sign * random.range(0.05, 3.0);
    if matches!(options.campaign, Campaign::Diode) {
        // Keep the channel off even in reverse conduction. Sample reverse
        // junction bias on the original card and forward bias on its variant.
        let vg = sign * (threshold.min(0.0) - 3.0);
        let vd = if variant == 0 {
            vd
        } else {
            -sign * random.range(0.1, 0.8)
        };
        return check_diode(&model, &parameters, temp, vg, vd);
    }
    let deck = make_deck(
        polarity,
        &parameters,
        temp,
        vg,
        vd,
        "print @m1[vgs] @m1[vds] @m1[id] @m1[cgd] @m1[cgs]",
    )?;
    let [vgs, vds, id, cgd, cgs] = oracle(
        &deck,
        ["@m1[vgs]", "@m1[vds]", "@m1[id]", "@m1[cgd]", "@m1[cgs]"],
    )?;
    let eval = model.bind(Instance::default())?.evaluate(Bias {
        vgs: sign * vgs,
        vds: sign * vds,
        vds_external: vd,
        vdiode: -vd,
        temperature: temp + 273.15,
        ..Bias::default()
    })?;
    let mut failures = String::new();
    for (name, actual, reference, absolute) in [
        ("Id", sign * eval.channel.value, id, 1e-11),
        ("Cgd", eval.qgd.d_voltage, cgd, 1e-16),
        ("Cgs", eval.qgs.d_voltage, cgs, 1e-16),
    ] {
        if (actual - reference).abs() > 2e-5_f64.mul_add(reference.abs(), absolute) {
            write!(failures, " {name}: Rust={actual:e} C={reference:e}")?;
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "temp={temp:.6} vg={vg:.6} vd={vd:.6} intrinsic=({vgs:.6},{vds:.6}):{failures}"
        )
        .into())
    }
}

// The C reference's reverse-region derivative is not the derivative of its
// current law (arg rather than arg^3 in vdmosload.c). Reconstruct its reported
// diffusion capacitance only for that region; independently compare stored
// charge and the DC current. Never copy the historical error into the kernel.
fn reported_diffusion_cap(
    evaluation: &model_kernel_vdmos::DiodeEvaluation,
    normalized_v: f64,
    normalized_i: f64,
    vte: f64,
) -> f64 {
    let exact = evaluation.diffusion.d_voltage;
    if normalized_v >= -3.0 * vte {
        return exact;
    }
    let arg = 3.0 * vte / (normalized_v * std::f64::consts::E);
    let isat = -normalized_i / (1.0 + arg.powi(3));
    let expected = 3.0 * isat * arg.powi(3) / normalized_v;
    let slope = evaluation.current.d_voltage;
    // Exclude avalanche: it has a different exact derivative.
    if (expected - slope).abs() <= 1e-10 * slope.abs() {
        exact / arg.powi(2)
    } else {
        exact
    }
}

fn check_diode(
    model: &PreparedModel,
    parameters: &[(&str, f64)],
    temp: f64,
    vg: f64,
    vd: f64,
) -> Result<()> {
    let m = model.parameters();
    let sign = m.polarity.sign();
    let junction = if m.series.rb > 0.0 {
        "v(m1#body_diode)-v(d)"
    } else {
        "-v(d)"
    };
    let commands = format!(
        "let junction_v = {junction}\nprint junction_v @m1[vgs] @m1[vds] @m1[idio] @m1[cds] @m1[qdio]"
    );
    let deck = make_deck(m.polarity, parameters, temp, vg, vd, &commands)?;
    let [junction_v, vgs, vds, current, cap, charge] = oracle(
        &deck,
        [
            "junction_v",
            "@m1[vgs]",
            "@m1[vds]",
            "@m1[idio]",
            "@m1[cds]",
            "@m1[qdio]",
        ],
    )?;
    let temperature = temp + 273.15;
    let evaluated = model.bind(Instance::default())?.evaluate(Bias {
        vgs: sign * vgs,
        vds: sign * vds,
        vds_external: vd,
        vdiode: junction_v,
        temperature,
        ..Bias::default()
    })?;
    let diode = evaluated.diode;
    let norm_v = sign * junction_v;
    let norm_i = sign * diode.current.value;
    let vte = m.diode.n * 8.617_333_262_145e-5 * temperature;
    let reported_cap =
        diode.depletion.d_voltage + reported_diffusion_cap(&diode, norm_v, norm_i, vte);
    let mut physical_charge = diode.depletion.value + diode.diffusion.value;
    if let Some(recovery) = diode.recovery {
        // Equilibrium Qp solves the algebraic residual; ngspice reports its
        // stored charge scaled by (1-VP)/TT in @m1[qdio].
        let qp = -recovery.residual / recovery.d_node;
        physical_charge = (recovery.diode_charge_rate_gain * recovery.stored_charge.d_voltage)
            .mul_add(qp, physical_charge);
    }
    let mut failures = String::new();
    for (name, actual, reference, absolute) in [
        ("Idio", norm_i, current, 2e-15),
        ("Cds", reported_cap, cap, 1e-16),
        ("Qdio", sign * physical_charge, charge, 1e-18),
    ] {
        if (actual - reference).abs() > 2e-5_f64.mul_add(reference.abs(), absolute) {
            write!(failures, " {name}: Rust={actual:e} C={reference:e}")?;
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("temp={temp:.6} vd={vd:.6} junction={norm_v:.9}: {failures}").into())
    }
}

#[derive(Default)]
struct Counts {
    passed: usize,
    failed: usize,
    source_failed: usize,
    translated_cards: usize,
    observation_cards: usize,
}

fn run_library(
    library: &ModelLibrary,
    seed: u64,
    filter: Option<&str>,
    counts: &mut Counts,
    options: OracleOptions,
) {
    for (index, scoped) in library.models.iter().enumerate() {
        if filter.is_some_and(|name| !scoped.card.name.eq_ignore_ascii_case(name)) {
            continue;
        }
        let card = match library.resolve_vdmos(index, &[]) {
            Ok(Some(card)) => card,
            Ok(None) => continue,
            Err(error) => {
                counts.source_failed += 1;
                println!("SOURCE FAIL {}: {error}", scoped.card.name);
                continue;
            }
        };
        let polarity = match card.polarity {
            VdmosPolarity::N => Polarity::N,
            VdmosPolarity::P => Polarity::P,
        };
        if options.ltspice_electrical {
            match translate_electrical(&card.parameters) {
                Ok(translated) => {
                    if translated.electrical != card.parameters {
                        counts.translated_cards += 1;
                    }
                    if !translated.thermal_observation.is_empty() {
                        counts.observation_cards += 1;
                        println!(
                            "OBSERVATION {}: {:?}",
                            card.name, translated.thermal_observation
                        );
                    }
                }
                Err(error) => {
                    counts.source_failed += 1;
                    println!("TRANSLATE FAIL {}: {error}", card.name);
                    continue;
                }
            }
        }
        // Per-name seed keeps a filtered reproduction independent of catalog order.
        let key = card.name.bytes().fold(seed, |state, byte| {
            state.wrapping_mul(1_099_511_628_211) ^ u64::from(byte)
        });
        let mut random = Random(key);
        for variant in 0..2 {
            match check_case(polarity, &card.parameters, variant, &mut random, options) {
                Ok(()) => counts.passed += 1,
                Err(error) => {
                    counts.failed += 1;
                    println!("FAIL {} variant={variant}: {error}", card.name);
                }
            }
        }
    }
}

fn is_vdmos_record(line: &[u8]) -> bool {
    let mut words = line
        .split(|byte| byte.is_ascii_whitespace() || *byte == b'(')
        .filter(|word| !word.is_empty());
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case(b".model"))
    {
        return false;
    }
    let _name = words.next();
    words.next().is_some_and(|family| {
        [b"vdmos".as_slice(), b"vdmosn", b"vdmosp"]
            .iter()
            .any(|value| family.eq_ignore_ascii_case(value))
    })
}

// Only for flat standalone catalogs: isolate malformed records without
// repairing them or hiding failures. Full decks must retain lexical scope.
fn isolated_catalog(
    source: &[u8],
    seed: u64,
    filter: Option<&str>,
    counts: &mut Counts,
    options: OracleOptions,
) {
    let mut start = None;
    let mut card = Vec::new();
    let mut flush = |start: usize, card: &[u8]| {
        let name = card
            .split(u8::is_ascii_whitespace)
            .filter(|word| !word.is_empty())
            .nth(1)
            .unwrap_or_default();
        let name = String::from_utf8_lossy(name);
        if filter.is_some_and(|filter| !name.eq_ignore_ascii_case(filter)) {
            return;
        }
        match ModelLibrary::parse(card) {
            Ok(library) => run_library(&library, seed, filter, counts, options),
            Err(error) => {
                counts.source_failed += 1;
                println!("SOURCE FAIL {name} line={start}: {error}");
            }
        }
    };
    for (index, line) in source.split(|byte| *byte == b'\n').enumerate() {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"*") {
            continue;
        }
        if line.starts_with(b"+") {
            if start.is_some() {
                card.extend_from_slice(line);
                card.push(b'\n');
            }
            continue;
        }
        if let Some(start) = start.take() {
            flush(start, &card);
            card.clear();
        }
        if is_vdmos_record(line) {
            start = Some(index + 1);
            card.extend_from_slice(line);
            card.push(b'\n');
        }
    }
    if let Some(start) = start {
        flush(start, &card);
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("missing catalog path")?);
    let seed = args.next().ok_or("missing seed")?.parse::<u64>()?;
    let mut isolated = false;
    let mut options = OracleOptions {
        campaign: Campaign::Channel,
        ltspice_electrical: false,
    };
    let mut filter = None;
    for arg in args {
        if arg == "--isolated" {
            isolated = true;
        } else if arg == "--diode" {
            options.campaign = Campaign::Diode;
        } else if arg == "--ltspice-electrical" {
            options.ltspice_electrical = true;
        } else if filter.replace(arg).is_some() {
            return Err("expected at most one model-name filter".into());
        }
    }
    let mut counts = Counts::default();
    if isolated {
        isolated_catalog(
            &std::fs::read(&path)?,
            seed,
            filter.as_deref(),
            &mut counts,
            options,
        );
    } else {
        let source = SourceLinker::default().link(&path)?;
        let library = ModelLibrary::parse(&source.bytes)?;
        run_library(&library, seed, filter.as_deref(), &mut counts, options);
    }
    println!(
        "seed={seed} options={options:?} passed={} failed={} source_failed={} translated_cards={} observation_cards={}",
        counts.passed,
        counts.failed,
        counts.source_failed,
        counts.translated_cards,
        counts.observation_cards
    );
    if counts.passed == 0 || counts.failed != 0 || counts.source_failed != 0 {
        return Err("catalog differential gate failed".into());
    }
    Ok(())
}
