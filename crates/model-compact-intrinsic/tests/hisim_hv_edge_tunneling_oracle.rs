//! Independent C conformance for actual HiSIM-HV1/HV2 `Igs`/`Igd` gate-edge
//! leakage, extracted directly from pinned ngspice 47 C conversion/eval paths.
#![cfg(target_os = "linux")]

use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use model_compact_intrinsic::{
    BoundParameters, ModelVariant, ParameterScope, ParameterValue, PreparedHiSimHvEdgeTunneling,
    PreparedHiSimHvInstance, prepare_hisim_hv_edge_tunneling, prepare_hisim_hv_instance,
    prepare_hisim_hv_model,
};

fn ngspice47() -> Result<PathBuf, Box<dyn std::error::Error>> {
    for ancestor in Path::new(env!("CARGO_MANIFEST_DIR")).ancestors() {
        for suffix in ["projects/ngspice", "ngspice"] {
            let path = ancestor.join(suffix);
            if path
                .join("src/spicelib/devices/hisimhv2/hsmhv2eval.c")
                .exists()
            {
                let revision = Command::new("git")
                    .arg("-C")
                    .arg(&path)
                    .args(["rev-parse", "HEAD"])
                    .output()?;
                if !revision.status.success()
                    || String::from_utf8(revision.stdout)?.trim()
                        != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
                {
                    return Err("requires pinned original ngspice-47 source".into());
                }
                return Ok(path);
            }
        }
    }
    Err("pinned ngspice 47 checkout not available".into())
}

fn original_program(
    source_root: &Path,
    family: ModelVariant,
) -> Result<String, Box<dyn std::error::Error>> {
    let (folder, prefix, set_file, eval_file) = match family {
        ModelVariant::HiSimHv1 => ("hisimhv1", "HSMHV", "hsmhvset.c", "hsmhveval.c"),
        ModelVariant::HiSimHv2 => ("hisimhv2", "HSMHV2", "hsmhv2set.c", "hsmhv2eval.c"),
        _ => return Err("not a HiSIM-HV family".into()),
    };
    let folder = source_root.join("src/spicelib/devices").join(folder);
    let setup = fs::read_to_string(folder.join(set_file))?;
    let evaluate = fs::read_to_string(folder.join(eval_file))?;
    let source_line = |source: &str, marker: &str| -> Result<String, Box<dyn std::error::Error>> {
        source
            .lines()
            .find(|line| line.contains(marker))
            .map(str::to_owned)
            .ok_or_else(|| format!("original C line missing: {marker}").into())
    };
    let prelude_1 = source_line(&setup, &format!("pParam->{prefix}_glksd1     = pParam"))?;
    let prelude_2 = source_line(&setup, &format!("pParam->{prefix}_glksd2     = pParam"))?;
    let prelude_3 = source_line(&setup, &format!("modelMKS->{prefix}_glksd3"))?;
    let header = source_line(&evaluate, &format!("if ( model->{prefix}_coiigs != 0 )"))?;
    let start = evaluate
        .find("    /* Igs */")
        .ok_or("native Igs section absent")?;
    let end = evaluate[start..]
        .find("    /* Igb */")
        .ok_or("native gate edge leakage Igb section absent")?
        + start;
    let physical_source = &evaluate[start..end];
    for name in ["Igs", "Igs_dVgs", "Igd", "Igd_dVgs", "Igd_dVds"] {
        if !physical_source.contains(name) {
            return Err(format!("original C direct edge tunneling {name} missing").into());
        }
    }
    let mut program =
        String::from("#include <math.h>\n#include <stdio.h>\n#define C_m2cm (1.0e2)\n");
    writeln!(
        program,
        "typedef struct {{double {prefix}_tox,{prefix}_glksd3; int {prefix}_coiigs;}} Model;"
    )?;
    writeln!(
        program,
        "typedef struct {{double {prefix}_glksd1,{prefix}_glksd2;}} Param;"
    )?;
    writeln!(
        program,
        "typedef struct {{double {prefix}_glksd3;}} ModelMKS;"
    )?;
    writeln!(
        program,
        "typedef struct {{double {prefix}_weff_nf;}} Instance;"
    )?;
    program.push_str("int main(void) { double inputs[9]; while (scanf(\"%lf %lf %lf %lf %lf %lf %lf %lf %lf\", &inputs[0],&inputs[1],&inputs[2],&inputs[3],&inputs[4],&inputs[5],&inputs[6],&inputs[7],&inputs[8])==9) {\n");
    program.push_str("Model md={0}; Param par={0}; ModelMKS mks={0}; Instance in={0};\n");
    program.push_str(
        "Model *model=&md; Param *pParam=&par; ModelMKS *modelMKS=&mks; Instance *here=&in;\n",
    );
    writeln!(
        program,
        "md.{prefix}_tox=inputs[0]; par.{prefix}_glksd1=inputs[1]; par.{prefix}_glksd2=inputs[2]; md.{prefix}_glksd3=inputs[3]; md.{prefix}_coiigs=(int)inputs[4]; in.{prefix}_weff_nf=inputs[5];"
    )?;
    program.push_str("double Mfactor=inputs[6],Vgs=inputs[7],Vds=inputs[8],Tox0=inputs[0];\n");
    for name in [
        "T0", "T1", "T2", "T3", "T4", "T2_dVg", "T2_dVd", "T2_dVb", "T3_dVg", "T3_dVd", "T3_dVb",
        "Igs", "Igs_dVgs", "Igs_dVds", "Igs_dVbs", "Igs_dT", "Igd", "Igd_dVgs", "Igd_dVds",
        "Igd_dVbs", "Igd_dT",
    ] {
        writeln!(program, "double {name}=0.0;")?;
    }
    writeln!(program, "{prelude_1}\n{prelude_2}\n{prelude_3}")?;
    writeln!(program, "{}", header.trim_end().trim_end_matches('{'))?;
    program.push_str(" {\n");
    program.push_str(physical_source);
    program.push_str("}\n");
    program.push_str("printf(\"%.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g %.17g\\n\", Mfactor*Igs, Mfactor*Igs_dVgs, Mfactor*Igs_dVds, Mfactor*Igs_dVbs, Mfactor*Igs_dT, Mfactor*Igd, Mfactor*Igd_dVgs, Mfactor*Igd_dVds, Mfactor*Igd_dVbs, Mfactor*Igd_dT);\n}\nreturn 0;\n}\n");
    Ok(program)
}

fn prepared(
    family: ModelVariant,
    parameters: &[(&str, ParameterValue)],
    geometry: &[(&str, ParameterValue)],
) -> Result<
    (
        PreparedHiSimHvEdgeTunneling,
        PreparedHiSimHvInstance,
        model_compact_intrinsic::PreparedHiSimHvModel,
    ),
    Box<dyn std::error::Error>,
> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        family,
        ParameterScope::Model,
        parameters
            .iter()
            .map(|(name, value)| (*name, value.clone())),
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            family,
            ParameterScope::Instance,
            geometry.iter().map(|(name, value)| (*name, value.clone())),
        )?,
    )?;
    let coefficients = prepare_hisim_hv_edge_tunneling(&model, &instance)?;
    Ok((coefficients, instance, model))
}

fn raw_native_parameters(
    variant: ModelVariant,
    model: &model_compact_intrinsic::PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    enabled: i32,
    vgs: f64,
    vds: f64,
) -> Result<[f64; 9], Box<dyn std::error::Error>> {
    let get_model = |name| -> Result<f64, Box<dyn std::error::Error>> {
        let slot = variant
            .parameter(ParameterScope::Model, name)
            .ok_or_else(|| format!("missing original model field {name}"))?
            .slot;
        match model.get_slot(slot) {
            Some(ParameterValue::Real(value)) => Ok(*value),
            _ => Err(format!("original model field {name} not real").into()),
        }
    };
    let get_binned = |name| -> Result<f64, Box<dyn std::error::Error>> {
        let slot = variant
            .parameter(ParameterScope::Model, name)
            .ok_or_else(|| format!("missing original bin field {name}"))?
            .slot;
        instance
            .binned_model_slot(slot)
            .ok_or_else(|| format!("original bin {name} absent").into())
    };
    let m_slot = variant
        .parameter(ParameterScope::Instance, "m")
        .ok_or("missing multiplier slot")?
        .slot;
    let Some(ParameterValue::Real(m)) = instance.parameters().get_slot(m_slot) else {
        return Err("instance M not real".into());
    };
    Ok([
        get_model("tox")?,
        get_binned("glksd1")?,
        get_binned("glksd2")?,
        get_model("glksd3")?,
        f64::from(enabled),
        instance.effective_geometry().channel_width_total_m,
        *m,
        vgs,
        vds,
    ])
}

fn run_original_c(
    original: &str,
    cases: &[[f64; 9]],
) -> Result<Vec<[f64; 10]>, Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("original-edge-tunneling.c");
    let binary = dir.path().join("original-edge-tunneling");
    fs::write(&source, original)?;
    let compiled = Command::new("sccache")
        .args(["cc", "-std=c11", "-O2"])
        .arg(&source)
        .args(["-lm", "-o"])
        .arg(&binary)
        .output()?;
    if !compiled.status.success() {
        return Err(format!(
            "native HiSIM-HV C edge tunneling compilation failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        )
        .into());
    }
    let mut child = Command::new(&binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("native C stdin unavailable")?;
    // Drain stdout concurrently while streaming the entire case matrix.
    // Writing all inputs first can deadlock when both OS pipes fill.
    let output = std::thread::scope(|scope| -> Result<_, Box<dyn std::error::Error>> {
        let writer = scope.spawn(move || -> std::io::Result<()> {
            for case in cases {
                writeln!(
                    input,
                    "{}",
                    case.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                )?;
            }
            Ok(())
        });
        let output = child.wait_with_output()?;
        writer
            .join()
            .map_err(|_| "native C input writer panicked")??;
        Ok(output)
    })?;
    if !output.status.success() {
        return Err(format!(
            "native HiSIM-HV C edge tunneling failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let text = String::from_utf8(output.stdout)?;
    let rows = text
        .lines()
        .map(|line| {
            let vals = line
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            vals.try_into().map_err(|v: Vec<f64>| {
                format!("native C returned {} scalars instead of ten", v.len()).into()
            })
        })
        .collect::<Result<Vec<[f64; 10]>, Box<dyn std::error::Error>>>()?;
    if rows.len() != cases.len() {
        return Err(format!(
            "native C emitted {} rows for {} test cases",
            rows.len(),
            cases.len()
        )
        .into());
    }
    Ok(rows)
}

fn check_native_match(variant: ModelVariant, c_rows: &[[f64; 10]], rust_rows: &[[f64; 10]]) {
    for (index, (c, r)) in c_rows.iter().zip(rust_rows).enumerate() {
        for (column, (&c_value, &rust_value)) in c.iter().zip(r).enumerate() {
            let tol = 7e-12f64.mul_add(c_value.abs().max(rust_value.abs()), 1e-22);
            assert!(
                (c_value - rust_value).abs() <= tol,
                "{variant:?} edge tunneling C case {index}, field {column}: native {c_value:.17e}, Rust {rust_value:.17e}, tolerance {tol:.3e}"
            );
        }
    }
}

#[test]
#[ignore = "requires pinned original ngspice 47 and managed sccache C compiler"]
fn original_hv1_hv2_direct_gate_edge_tunneling_matches_rust()
-> Result<(), Box<dyn std::error::Error>> {
    let root = ngspice47()?;
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let source = original_program(&root, variant)?;
        let mut inputs = Vec::new();
        let mut expected = Vec::new();
        for (glksd1, glksd2, glksd3) in [
            (1e-15, 5e6, -5e6),
            (2.3e-14, 1e6, -3e6),
            (1.3e-16, 8e4, -2e5),
        ] {
            for tox in [2.5e-9, 8e-9] {
                for width in [3e-6, 12e-6, 40e-6] {
                    for m in [0.5, 3.0] {
                        for selector in [0, 1] {
                            let model_fields = [
                                ("tox", ParameterValue::Real(tox)),
                                ("coiigs", ParameterValue::Integer(selector)),
                                ("glksd1", ParameterValue::Real(glksd1)),
                                ("glksd2", ParameterValue::Real(glksd2)),
                                ("glksd3", ParameterValue::Real(glksd3)),
                            ];
                            let geometry = [
                                ("l", ParameterValue::Real(4e-6)),
                                ("w", ParameterValue::Real(width)),
                                ("nf", ParameterValue::Real(2.0)),
                                ("m", ParameterValue::Real(m)),
                            ];
                            let (prepared, instance, model) =
                                prepared(variant, &model_fields, &geometry)?;
                            for vgs in [-4.0, -1.0, -0.02, 0.0, 0.3, 2.0] {
                                for vds in [-3.0, -0.1, 0.0, 0.1, 2.5] {
                                    let result = prepared.evaluate(vgs, vds)?;
                                    inputs.push(raw_native_parameters(
                                        variant, &model, &instance, selector, vgs, vds,
                                    )?);
                                    expected.push([
                                        result.gate_source.current_a,
                                        result.gate_source.gate_voltage_derivative_siemens,
                                        result.gate_source.drain_voltage_derivative_siemens,
                                        0.0,
                                        0.0,
                                        result.gate_drain.current_a,
                                        result.gate_drain.gate_voltage_derivative_siemens,
                                        result.gate_drain.drain_voltage_derivative_siemens,
                                        0.0,
                                        0.0,
                                    ]);
                                }
                            }
                        }
                    }
                }
            }
        }
        let native = run_original_c(&source, &inputs)?;
        check_native_match(variant, &native, &expected);
        eprintln!(
            "{variant:?}: {} original C gate edge tunneling cases × 10 fields agree",
            native.len()
        );
    }
    Ok(())
}
