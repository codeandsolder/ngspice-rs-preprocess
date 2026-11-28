//! Executable verbatim original C for the independently additive XQY1 body-Q.
#![cfg(target_os = "linux")]

use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterValue, prepare_hisim_hv_instance,
    prepare_hisim_hv_lateral_field_charge, prepare_hisim_hv_model,
};

fn native_source_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for ancestor in Path::new(env!("CARGO_MANIFEST_DIR")).ancestors() {
        for suffix in ["projects/ngspice", "ngspice"] {
            let candidate = ancestor.join(suffix);
            if candidate
                .join("src/spicelib/devices/hisimhv2/hsmhv2eval.c")
                .exists()
            {
                let git = Command::new("git")
                    .arg("-C")
                    .arg(&candidate)
                    .args(["rev-parse", "HEAD"])
                    .output()?;
                if !git.status.success()
                    || String::from_utf8(git.stdout)?.trim()
                        != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
                {
                    return Err("HiSIM-HV lateral-field oracle requires pinned ngspice 47".into());
                }
                return Ok(candidate);
            }
        }
    }
    Err("pinned ngspice 47 source checkout not found".into())
}

fn extract_original(
    root: &Path,
    variant: ModelVariant,
) -> Result<String, Box<dyn std::error::Error>> {
    let (folder, prefix, temp_file, eval_file) = match variant {
        ModelVariant::HiSimHv1 => ("hisimhv1", "HSMHV", "hsmhvtemp.c", "hsmhveval.c"),
        ModelVariant::HiSimHv2 => ("hisimhv2", "HSMHV2", "hsmhv2temp.c", "hsmhv2eval.c"),
        _ => return Err("not a HiSIM-HV family".into()),
    };
    let folder = root.join("src/spicelib/devices").join(folder);
    let temp = fs::read_to_string(folder.join(temp_file))?;
    let eval = fs::read_to_string(folder.join(eval_file))?;
    let coefficient_start = temp
        .find(&format!("here->{prefix}_cqyb0 = C_m2um"))
        .ok_or("upstream body-field geometry coefficient missing")?;
    let coefficient_end = temp[coefficient_start..]
        .find(';')
        .ok_or("coefficient end absent")?
        + coefficient_start
        + 1;
    let eval_start = eval
        .find(&format!("  if ( model->{prefix}_xqy1 != 0.0 ){{"))
        .ok_or("upstream lateral Qy increment missing")?;
    let eval_end = eval[eval_start..]
        .find("\n  }")
        .ok_or("lateral Qy branch end absent")?
        + eval_start
        + "\n  }".len();
    let gate = eval
        .lines()
        .find(|s| s.contains(&format!("if ( model->{prefix}_coadov == 1 )")))
        .ok_or("upstream COADOV gate missing")?;
    let stamp = |substring: &str| {
        eval.lines()
            .find(|line| line.contains(substring))
            .ok_or_else(|| {
                Box::<dyn std::error::Error>::from(format!(
                    "upstream terminal charge stamp not found: {substring}"
                ))
            })
    };
    let gate_q = stamp(&format!("here->{prefix}_qg += Mfactor *"))?;
    let drain_q = stamp(&format!("here->{prefix}_qd += Mfactor *"))?;
    let gate_derivative = stamp(&format!("here->{prefix}_cgbo        = Mfactor *"))?;
    let drain_derivative = stamp(&format!("here->{prefix}_cdbo        = Mfactor *"))?;
    let mut code = String::from("#include <math.h>\n#include <stdio.h>\n#define C_m2um (1.0e6)\n");
    writeln!(
        code,
        "typedef struct {{ double {prefix}_xqy1,{prefix}_xqy2; int {prefix}_coadov; }} Model;"
    )?;
    writeln!(
        code,
        "typedef struct {{ double {prefix}_cqyb0,{prefix}_weff_nf,{prefix}_qg,{prefix}_qd,{prefix}_cgbo,{prefix}_cdbo; }} Instance;"
    )?;
    code.push_str("int main(void) { double v[7]; while(scanf(\"%lf %lf %lf %lf %lf %lf %lf\", &v[0],&v[1],&v[2],&v[3],&v[4],&v[5],&v[6])==7) {\n");
    writeln!(
        code,
        "Model md={{0}}; Instance ins={{0}}; Model *model=&md; Instance *here=&ins; md.{prefix}_xqy1=v[0]; md.{prefix}_xqy2=v[1]; md.{prefix}_coadov=(int)v[2]; ins.{prefix}_weff_nf=v[3];"
    )?;
    code.push_str("double LG=v[4], Vbs=v[5], Mfactor=v[6], Qy=0,Qy_dVbs=0;\n");
    for name in [
        "Qgod",
        "Qgos",
        "Qgbo",
        "Qovd",
        "Qovs",
        "QbdLD",
        "Qgod_dVbs",
        "Qgos_dVbs",
        "Qgbo_dVbs",
        "Qovd_dVbs",
        "Qovs_dVbs",
        "QbdLD_dVbs",
    ] {
        writeln!(code, "double {name}=0;")?;
    }
    code.push_str(&temp[coefficient_start..coefficient_end]);
    code.push('\n');
    code.push_str(&eval[eval_start..eval_end]);
    code.push('\n');
    writeln!(code, "{}", gate.trim_end().trim_end_matches('{'))?;
    code.push_str("{\n");
    writeln!(
        code,
        "{gate_q}\n{drain_q}\n{gate_derivative}\n{drain_derivative}"
    )?;
    code.push_str("}\n");
    writeln!(
        code,
        "printf(\"%.17g %.17g %.17g %.17g %.17g %.17g %.17g\\n\",here->{prefix}_cqyb0,Qy,Qy_dVbs,here->{prefix}_qg,here->{prefix}_qd,here->{prefix}_cgbo,here->{prefix}_cdbo);"
    )?;
    code.push_str("}\nreturn 0;\n}\n");
    Ok(code)
}

fn prepared(
    variant: ModelVariant,
    xqy1: f64,
    xqy2: f64,
    coadov: i32,
    l: f64,
    w: f64,
    m: f64,
) -> Result<
    (
        model_compact_intrinsic::PreparedHiSimHvLateralFieldCharge,
        model_compact_intrinsic::PreparedHiSimHvInstance,
    ),
    Box<dyn std::error::Error>,
> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("xqy1", ParameterValue::Real(xqy1)),
            ("xqy2", ParameterValue::Real(xqy2)),
            ("coadov", ParameterValue::Integer(coadov)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(l)),
                ("w", ParameterValue::Real(w)),
                ("nf", ParameterValue::Real(3.0)),
                ("m", ParameterValue::Real(m)),
            ],
        )?,
    )?;
    Ok((
        prepare_hisim_hv_lateral_field_charge(&model, &instance)?,
        instance,
    ))
}

fn run_original(
    source: &str,
    cases: &[[f64; 7]],
) -> Result<Vec<[f64; 7]>, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let original = dir.path().join("original-lateral-field.c");
    let executable = dir.path().join("original-lateral-field");
    fs::write(&original, source)?;
    let compiled = Command::new("sccache")
        .args(["cc", "-std=c11", "-O2"])
        .arg(&original)
        .args(["-lm", "-o"])
        .arg(&executable)
        .output()?;
    if !compiled.status.success() {
        return Err(format!(
            "original ngspice C lateral-field compilation failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        )
        .into());
    }
    let mut child = Command::new(&executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or("original C lateral-field stdin absent")?;
    for case in cases {
        writeln!(
            stdin,
            "{}",
            case.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )?;
    }
    drop(stdin);
    let finished = child.wait_with_output()?;
    if !finished.status.success() {
        return Err(format!(
            "original C lateral-field output failed: {}",
            String::from_utf8_lossy(&finished.stderr)
        )
        .into());
    }
    let text = String::from_utf8(finished.stdout)?;
    let rows = text
        .lines()
        .map(|line| {
            let values = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            values.try_into().map_err(|v: Vec<f64>| {
                format!("original C returned {} values instead of seven", v.len()).into()
            })
        })
        .collect::<Result<Vec<[f64; 7]>, Box<dyn std::error::Error>>>()?;
    if rows.len() != cases.len() {
        return Err(format!(
            "original C gave {} rows for {} input cases",
            rows.len(),
            cases.len()
        )
        .into());
    }
    Ok(rows)
}

#[test]
#[ignore = "requires pinned original ngspice 47 source and managed sccache C compiler"]
fn original_hv1_hv2_body_lateral_charge_increment_and_stamps_match_rust()
-> Result<(), Box<dyn std::error::Error>> {
    let root = native_source_root()?;
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let native_c = extract_original(&root, variant)?;
        let mut cases = Vec::new();
        let mut expected = Vec::new();
        for xqy1 in [0.0, 1e-12, -3e-12] {
            for xqy2 in [0.0, 0.5, 2.0] {
                for (l, w) in [(2.5e-6, 2e-6), (5e-6, 15e-6), (12e-6, 50e-6)] {
                    for m in [0.25, 1.5] {
                        for coadov in [0, 1] {
                            let (coeff, device) = prepared(variant, xqy1, xqy2, coadov, l, w, m)?;
                            for vbs in [-1.0, 0.0, 1.5, 3.0] {
                                let actual = coeff.evaluate(vbs)?;
                                cases.push([
                                    xqy1,
                                    xqy2,
                                    f64::from(coadov),
                                    device.effective_geometry().channel_width_total_m,
                                    device.geometry().gate_length_m * 1e6,
                                    vbs,
                                    m,
                                ]);
                                let raw = coeff.raw_body_capacitance_f;
                                expected.push([
                                    raw,
                                    raw * vbs,
                                    if xqy1 == 0.0 { 0.0 } else { raw },
                                    actual.gate_charge_c,
                                    actual.drain_charge_c,
                                    actual.gate_body_derivative_f,
                                    -actual.gate_body_derivative_f,
                                ]);
                            }
                        }
                    }
                }
            }
        }
        let native = run_original(&native_c, &cases)?;
        for (row, (c, rust)) in native.iter().zip(&expected).enumerate() {
            for (field, (&cv, &rv)) in c.iter().zip(rust).enumerate() {
                let tolerance = 4e-12f64.mul_add(cv.abs().max(rv.abs()), 1e-23);
                assert!(
                    (cv - rv).abs() <= tolerance,
                    "{variant:?} native lateral field case {row}, field {field}: original C {cv:.17e}, Rust {rv:.17e}, tolerance {tolerance:.17e}"
                );
            }
        }
        eprintln!(
            "{variant:?}: {} native-C body field cases × 7 coefficient/charge/stamp values agree",
            native.len()
        );
    }
    Ok(())
}
