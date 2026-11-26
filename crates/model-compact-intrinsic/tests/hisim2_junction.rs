use model_compact_intrinsic::{
    BoundParameters, ModelError, ModelVariant, ParameterScope, ParameterType, ParameterValue,
    hisim2::{
        JunctionBias, JunctionResult, PreparedJunctions, prepare_instance, prepare_junctions,
        prepare_model, prepare_temperature,
    },
};
use std::error::Error;

const FAMILY: ModelVariant = ModelVariant::HiSim2;
const MODEL: &str = "tnom=23 js0=1e-4 js0sw=2e-10 nj=1.05 njsw=1.2 xti=2.5 xti2=1.1 cisb=0.3 ctemp=0.4 cvb=0.05 cvbk=0.08 cisbk=2e-10 divx=0.01 vdiffj=1e-7 cj=0.001 cjsw=2e-10 cjswg=4e-10 pb=0.8 pbsw=0.9 pbswg=0.7 mj=0.5 mjsw=0.33 mjswg=0.6 tcjbd=0.002 tcjbs=0.003 tcjbdsw=0.004 tcjbssw=0.005 tcjbdswg=0.006 tcjbsswg=0.007 xl=1e-7 xw=2e-7 xwd=5e-8";
const INSTANCE: &str = "l=2e-6 w=4e-6 nf=2 m=3 ad=5e-12 as=7e-12 pd=9e-6 ps=3e-6";

fn bind(
    scope: ParameterScope,
    fields: &str,
    polarity: Option<&str>,
) -> Result<BoundParameters, Box<dyn Error>> {
    let mut values = Vec::new();
    for field in fields.split_whitespace() {
        let (name, value) = field.split_once('=').ok_or("expected assignment")?;
        let spec = FAMILY
            .parameter(scope, name)
            .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))?;
        let value = match spec.kind {
            ParameterType::Real => ParameterValue::Real(value.parse()?),
            ParameterType::Integer => ParameterValue::Integer(value.parse()?),
            _ => return Err("unexpected fixture type".into()),
        };
        values.push((name, value));
    }
    if let Some(polarity) = polarity {
        values.push((polarity, ParameterValue::Flag(true)));
    }
    Ok(BoundParameters::bind(FAMILY, scope, values)?)
}

fn prepare(
    polarity: &str,
    model_fields: &str,
    instance_fields: &str,
    circuit_celsius: f64,
) -> Result<PreparedJunctions, Box<dyn Error>> {
    let model = prepare_model(&bind(ParameterScope::Model, model_fields, Some(polarity))?)?;
    let instance = prepare_instance(
        &model,
        &bind(ParameterScope::Instance, instance_fields, None)?,
        circuit_celsius + 273.15,
    )?;
    Ok(prepare_junctions(&model, &instance)?)
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64, label: &str) {
    let residual = (actual - expected).abs();
    assert!(
        residual <= absolute + relative * expected.abs(),
        "{label}: {actual:e} != {expected:e} (residual {residual:e})"
    );
}

#[test]
fn analytic_junction_derivatives_and_temperature_split() -> Result<(), Box<dyn Error>> {
    let model = prepare_model(&bind(ParameterScope::Model, MODEL, Some("nmos"))?)?;
    let instance = prepare_instance(
        &model,
        &bind(
            ParameterScope::Instance,
            &format!("{INSTANCE} temp=40 dtemp=5"),
            None,
        )?,
        338.15,
    )?;
    let t = prepare_temperature(&model, &instance)?;
    close(t.kelvin, 318.15, 0.0, 1e-12, "lattice");
    close(
        instance.evaluator_temperature_kelvin(),
        343.15,
        0.0,
        1e-12,
        "capacitance",
    );
    close(t.nominal_kelvin, 296.15, 0.0, 1e-12, "nominal");
    let junctions = prepare_junctions(&model, &instance)?;
    let eval = |v| {
        junctions.evaluate(
            JunctionBias {
                drain_v: v,
                source_v: v,
            },
            1e-12,
        )
    };
    let h = 1e-6;
    for v in [-1.0, -0.4, -0.1, -0.01, 0.0, 0.02, 0.2, 0.4, 0.8] {
        let low = eval(v - h)?;
        let high = eval(v + h)?;
        let middle = eval(v)?;
        for (a, b, c) in [
            (low.drain, high.drain, middle.drain),
            (low.source, high.source, middle.source),
        ] {
            close(
                (b.current_a - a.current_a) / (2.0 * h),
                c.conductance_s,
                2e-7,
                1e-16,
                "dI/dV",
            );
            close(
                (b.charge_c - a.charge_c) / (2.0 * h),
                c.capacitance_f,
                2e-7,
                1e-22,
                "dQ/dV",
            );
        }
    }
    let zero = eval(0.0)?;
    close(zero.drain.current_a, 0.0, 0.0, 0.0, "zero current");
    close(zero.source.charge_c, 0.0, 0.0, 0.0, "zero charge");
    let other_model = prepare_model(&bind(ParameterScope::Model, MODEL, Some("nmos"))?)?;
    assert_eq!(
        prepare_junctions(&other_model, &instance),
        Err(ModelError::MismatchedPreparedContext)
    );
    Ok(())
}

#[test]
fn grading_one_logarithmic_limit_and_gmin_scaling() -> Result<(), Box<dyn Error>> {
    let j = prepare(
        "nmos",
        "mj=1 cj=1 cjsw=0 cjswg=0 js0=0 js0sw=0 pb=0.8",
        "ad=2e-12 as=1e-12 m=3",
        27.0,
    )?;
    let bias = JunctionBias {
        drain_v: -0.8,
        source_v: 0.0,
    };
    let result = j.evaluate(bias, 1e-10)?;
    close(
        result.drain.charge_c,
        -3.0 * 2e-12 * 0.8 * 2.0_f64.ln(),
        1e-14,
        0.0,
        "logarithmic charge",
    );
    close(
        result.drain.capacitance_f,
        3e-12,
        1e-14,
        0.0,
        "grading-one capacitance",
    );
    close(result.drain.current_a, -2.4e-10, 1e-14, 0.0, "gmin current");
    close(
        result.source.conductance_s,
        3e-10,
        1e-14,
        0.0,
        "gmin conductance",
    );
    Ok(())
}

#[cfg(target_os = "linux")]
mod native {
    use super::*;
    use std::{fs, process::Command};

    struct Case<'a> {
        polarity: &'a str,
        model: &'a str,
        instance: &'a str,
        circuit_celsius: f64,
    }

    fn compare_side(actual: JunctionResult, expected: [f64; 3], residual: &mut [f64; 3]) {
        for (index, (a, e)) in [actual.current_a, actual.conductance_s, actual.capacitance_f]
            .into_iter()
            .zip(expected)
            .enumerate()
        {
            residual[index] = residual[index].max((a - e).abs());
            let absolute = if index == 2 { 1e-25 } else { 2e-16 };
            close(
                a,
                e,
                2e-9,
                absolute,
                ["native I", "native G", "native C"][index],
            );
        }
    }

    fn run(case: &Case<'_>, residual: &mut [f64; 3]) -> Result<usize, Box<dyn Error>> {
        let j = prepare(
            case.polarity,
            case.model,
            case.instance,
            case.circuit_celsius,
        )?;
        let sign = if case.polarity == "pmos" { -1.0 } else { 1.0 };
        let temp = tempfile::tempdir()?;
        let deck = format!(
            "HiSIM2 junction matrix\n.temp {}\n.options gmin=1e-12\n.model m {} level=68 {}\nVd d 0 0\nVs s 0 {}\nVb b 0 0\nVg g 0 {}\nM1 d g s b m {}\n.control\nset num_threads=1\nset numdgt=17\nset wr_singlescale\nset wr_vecnames\nsave all @m1[ibd] @m1[ibs] @m1[gbd] @m1[gbs] @m1[capbd] @m1[capbs]\ndc Vb {} {} {}\nwrdata witness.tsv v(b) @m1[ibd] @m1[ibs] @m1[gbd] @m1[gbs] @m1[capbd] @m1[capbs]\nquit\n.endc\n.end\n",
            case.circuit_celsius,
            case.polarity,
            case.model,
            sign * 0.1,
            -sign,
            case.instance,
            -sign,
            0.9 * sign,
            0.05 * sign
        );
        fs::write(temp.path().join("witness.cir"), &deck)?;
        let output = Command::new("ngspice")
            .args(["-b", "-n", "witness.cir"])
            .current_dir(temp.path())
            .output()?;
        assert!(
            output.status.success(),
            "{deck}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let table = fs::read_to_string(temp.path().join("witness.tsv"))?;
        let mut rows = 0;
        for line in table.lines().skip(1) {
            let columns = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            let [_, v, id, is, gd, gs, cd, cs] = columns.as_slice() else {
                return Err("invalid native junction row".into());
            };
            let bias = JunctionBias {
                drain_v: sign * v,
                source_v: sign.mul_add(*v, -0.1),
            };
            let actual = j.evaluate(bias, 1e-12)?;
            compare_side(actual.drain, [*id, *gd, *cd], residual);
            compare_side(actual.source, [*is, *gs, *cs], residual);
            rows += 1;
        }
        assert_eq!(rows, 39);
        Ok(rows)
    }

    #[test]
    #[ignore = "requires independent ngspice 47 executable"]
    fn native_junction_matrix() -> Result<(), Box<dyn Error>> {
        let explicit = format!("{INSTANCE} temp=40 dtemp=5");
        let offset = format!("{INSTANCE} dtemp=-10");
        let cases = [
            Case {
                polarity: "nmos",
                model: MODEL,
                instance: &explicit,
                circuit_celsius: 65.0,
            },
            Case {
                polarity: "pmos",
                model: MODEL,
                instance: &explicit,
                circuit_celsius: 65.0,
            },
            Case {
                polarity: "nmos",
                model: MODEL,
                instance: &offset,
                circuit_celsius: -20.0,
            },
            Case {
                polarity: "pmos",
                model: MODEL,
                instance: INSTANCE,
                circuit_celsius: 100.0,
            },
        ];
        let mut residual = [0.0_f64; 3];
        let mut rows = 0;
        for case in &cases {
            rows += run(case, &mut residual)?;
        }
        println!(
            "{rows} native bias points, {} scalar comparisons; max absolute residuals I/G/C: {residual:?}",
            rows * 6
        );
        Ok(())
    }
}
