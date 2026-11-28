//! Verbatim source-C oracle for both HiSIM-HV physical fringing capacitors.
#![cfg(target_os = "linux")]

use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvFringing,
    PreparedHiSimHvInstance, prepare_hisim_hv_fringing, prepare_hisim_hv_instance,
    prepare_hisim_hv_model,
};

fn pinned_checkout() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for ancestor in Path::new(env!("CARGO_MANIFEST_DIR")).ancestors() {
        for name in ["projects/ngspice", "ngspice"] {
            let candidate = ancestor.join(name);
            if candidate
                .join("src/spicelib/devices/hisimhv2/hsmhv2temp.c")
                .exists()
            {
                let revision = Command::new("git")
                    .arg("-C")
                    .arg(&candidate)
                    .args(["rev-parse", "HEAD"])
                    .output()?;
                if !revision.status.success()
                    || String::from_utf8(revision.stdout)?.trim()
                        != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
                {
                    return Err(
                        "fringing oracle requires original ngspice 47 revision 972dbf45".into(),
                    );
                }
                return Ok(candidate);
            }
        }
    }
    Err("pinned ngspice 47 checkout absent".into())
}

fn original_expressions(
    root: &Path,
    variant: ModelVariant,
) -> Result<String, Box<dyn std::error::Error>> {
    let (folder, prefix, temp_file, eval_file) = match variant {
        ModelVariant::HiSimHv1 => ("hisimhv1", "HSMHV", "hsmhvtemp.c", "hsmhveval.c"),
        ModelVariant::HiSimHv2 => ("hisimhv2", "HSMHV2", "hsmhv2temp.c", "hsmhv2eval.c"),
        _ => return Err("non-HiSIM-HV fringing oracle family".into()),
    };
    let source = root.join("src/spicelib/devices").join(folder);
    let temp = fs::read_to_string(source.join(temp_file))?;
    let eval = fs::read_to_string(source.join(eval_file))?;
    let temp_statement_start = temp
        .find(&format!("here->{prefix}_cfrng = C_EOX"))
        .ok_or("original C fringing coefficient source missing")?;
    let temp_statement_end = temp[temp_statement_start..]
        .find(';')
        .ok_or("original C fringing coefficient terminator missing")?
        + temp_statement_start
        + 1;
    let charge_start = eval
        .find(&format!("  Cfd = here->{prefix}_cfrng ;"))
        .ok_or("original C fringing charge source missing")?;
    let charge_end = eval[charge_start..]
        .find("Qfs = Cfs * Vgse ;")
        .ok_or("original C fringing charge terminator missing")?
        + charge_start
        + "Qfs = Cfs * Vgse ;".len();
    let gate = eval
        .lines()
        .find(|line| line.contains(&format!("if ( model->{prefix}_coadov == 1 )")))
        .ok_or("native fringing output gate missing")?;
    let mut source = String::from(
        "#include <stdio.h>\n#include <math.h>\n#define C_EOX (3.453133e-11)\n#define C_Pi (3.141592653589793)\n",
    );
    writeln!(
        source,
        "typedef struct {{ double {prefix}_tox, {prefix}_tpoly; int {prefix}_coadov; }} Model;"
    )?;
    writeln!(
        source,
        "typedef struct {{ double {prefix}_weff_nf, {prefix}_cfrng; }} Instance;"
    )?;
    source.push_str("int main(void) { double v[7]; while(scanf(\"%lf %lf %lf %lf %lf %lf %lf\", &v[0],&v[1],&v[2],&v[3],&v[4],&v[5],&v[6])==7) {\n");
    writeln!(
        source,
        "Model md={{0}}; Instance ins={{0}}; Model *model=&md; Instance *here=&ins; md.{prefix}_tox=v[0]; md.{prefix}_tpoly=v[1]; ins.{prefix}_weff_nf=v[2]; md.{prefix}_coadov=(int)v[3];"
    )?;
    source.push_str("double Vgse=v[4],Vdse=v[5],Mfactor=v[6]; double Cfd=0,Cfs=0,Qfd=0,Qfs=0;\n");
    source.push_str(&temp[temp_statement_start..temp_statement_end]);
    source.push('\n');
    source.push_str(&eval[charge_start..charge_end]);
    source.push('\n');
    writeln!(source, "{}", gate.trim_end().trim_end_matches('{'))?;
    source.push_str(" { Qfd *= Mfactor; Qfs *= Mfactor; Cfd *= Mfactor; Cfs *= Mfactor; } else { Qfd=0; Qfs=0; Cfd=0; Cfs=0; }\n");
    writeln!(
        source,
        "printf(\"%.17g %.17g %.17g %.17g %.17g %.17g\\n\",here->{prefix}_cfrng,Cfd,Qfd,Qfs,-Cfd,Cfs);"
    )?;
    source.push_str("}\nreturn 0;\n}\n");
    Ok(source)
}

fn prepare(
    variant: ModelVariant,
    tox: f64,
    tpoly: f64,
    width: f64,
    coadov: i32,
    m: f64,
) -> Result<(PreparedHiSimHvFringing, PreparedHiSimHvInstance), Box<dyn std::error::Error>> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        [
            ("tox", ParameterValue::Real(tox)),
            ("tpoly", ParameterValue::Real(tpoly)),
            ("coadov", ParameterValue::Integer(coadov)),
        ],
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("w", ParameterValue::Real(width)),
                ("nf", ParameterValue::Real(2.0)),
                ("m", ParameterValue::Real(m)),
            ],
        )?,
    )?;
    Ok((prepare_hisim_hv_fringing(&model, &instance)?, instance))
}

fn run_original_c(
    program: &str,
    cases: &[[f64; 7]],
) -> Result<Vec<[f64; 6]>, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("original-hisim-fringe.c");
    let binary = dir.path().join("original-hisim-fringe");
    fs::write(&source, program)?;
    let compiled = Command::new("sccache")
        .args(["cc", "-O2", "-std=c11"])
        .arg(&source)
        .args(["-lm", "-o"])
        .arg(&binary)
        .output()?;
    if !compiled.status.success() {
        return Err(format!(
            "upstream C fringing test compilation failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        )
        .into());
    }
    let mut child = Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("missing original C stdin")?;
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
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "upstream C fringing evaluator failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let values = String::from_utf8(output.stdout)?
        .lines()
        .map(|line| {
            let value = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            value.try_into().map_err(|v: Vec<f64>| {
                format!(
                    "fringing original C emitted {} fields (expected six)",
                    v.len()
                )
                .into()
            })
        })
        .collect::<Result<Vec<[f64; 6]>, Box<dyn std::error::Error>>>()?;
    if values.len() != cases.len() {
        return Err(format!(
            "original C emitted {} rows for {} cases",
            values.len(),
            cases.len()
        )
        .into());
    }
    Ok(values)
}

fn compare_original_values(variant: ModelVariant, native: &[[f64; 6]], rust: &[[f64; 6]]) {
    for (row, (actual, expected)) in native.iter().zip(rust).enumerate() {
        for (column, (c, r)) in actual.iter().zip(expected).enumerate() {
            let tolerance = 5e-12f64.mul_add(c.abs().max(r.abs()), 1e-24);
            assert!(
                (c - r).abs() <= tolerance,
                "{variant:?}: case {row}, component {column}: original C {c:.17e}, Rust {r:.17e}, tolerance {tolerance:e}"
            );
        }
    }
}

#[test]
#[ignore = "requires pinned ngspice-47 original C and managed sccache compiler"]
fn hv1_and_hv2_original_fringing_charge_and_stamping_match_rust()
-> Result<(), Box<dyn std::error::Error>> {
    let root = pinned_checkout()?;
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let source = original_expressions(&root, variant)?;
        let mut cases = Vec::new();
        let mut expected = Vec::new();
        for (tox, tpoly) in [(2.6e-9, 130e-9), (9.5e-9, 15e-9), (3e-9, 0.0)] {
            for width in [2e-6, 8e-6, 33e-6] {
                for m in [0.25, 1.0, 4.0] {
                    for coadov in [0, 1] {
                        let (fringe, device) = prepare(variant, tox, tpoly, width, coadov, m)?;
                        for (vgs, vds) in [(-0.75, 2.5), (0.0, 0.0), (2.3, 1.2), (15.0, -3.5)] {
                            let charges = fringe.evaluate(vgs, vds)?;
                            cases.push([
                                tox,
                                tpoly,
                                device.effective_geometry().channel_width_total_m,
                                f64::from(coadov),
                                vgs,
                                vds,
                                m,
                            ]);
                            expected.push([
                                fringe.raw_capacitance_f,
                                fringe.effective_capacitance_f,
                                charges.gate_drain_charge_c,
                                charges.gate_source_charge_c,
                                -fringe.effective_capacitance_f,
                                fringe.effective_capacitance_f,
                            ]);
                        }
                    }
                }
            }
        }
        let native = run_original_c(&source, &cases)?;
        compare_original_values(variant, &native, &expected);
        eprintln!(
            "{variant:?}: {} original fringing C cases × six outputs match",
            native.len()
        );
    }
    Ok(())
}
