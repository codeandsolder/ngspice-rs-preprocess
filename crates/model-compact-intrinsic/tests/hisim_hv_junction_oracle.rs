//! Verbatim ngspice 47 C oracle for the full HiSIM-HV1/HV2 junction depletion
//! Q, dQ/dV and dQ/dT source/drain branches, run through managed sccache.
#![cfg(target_os = "linux")]

use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use model_compact_intrinsic::{
    BoundParameters, HiSimHvJunctionBias, ModelVariant, ParameterScope, ParameterValue,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTransport,
    prepare_hisim_hv_instance, prepare_hisim_hv_junction_charges, prepare_hisim_hv_model,
    prepare_hisim_hv_transport,
};

const HV1_MODEL: &[&str] = &[
    "cj", "cjsw", "cjswg", "mj", "mjsw", "mjswg", "pb", "pbsw", "pbswg", "tcjbd", "tcjbdsw",
    "tcjbdswg", "tcjbs", "tcjbssw", "tcjbsswg",
];
const HV2_MODEL: &[&str] = &[
    "cjd", "cjs", "cjswd", "cjswgd", "cjswgs", "cjsws", "mjd", "mjs", "mjswd", "mjswgd", "mjswgs",
    "mjsws", "pbd", "pbs", "pbswd", "pbswgd", "pbswgs", "pbsws", "tcjbd", "tcjbdsw", "tcjbdswg",
    "tcjbs", "tcjbssw", "tcjbsswg",
];

fn upstream_checkout() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in root.ancestors() {
        let candidate = ancestor.join("projects/ngspice");
        if candidate
            .join("src/spicelib/devices/hisimhv2/hsmhv2eval_dio.c")
            .is_file()
        {
            return Ok(candidate);
        }
    }
    Err("ngspice 47 checkout must be available beneath an ancestor's projects/ngspice".into())
}

fn compiled_c_source(
    checkout: &Path,
    family: ModelVariant,
) -> Result<String, Box<dyn std::error::Error>> {
    let (path, prefix, fields) = match family {
        ModelVariant::HiSimHv1 => ("hisimhv1/hsmhveval.c", "HSMHV", HV1_MODEL),
        ModelVariant::HiSimHv2 => ("hisimhv2/hsmhv2eval_dio.c", "HSMHV2", HV2_MODEL),
        _ => return Err("unsupported HiSIM-HV source family".into()),
    };
    let c = fs::read_to_string(checkout.join("src/spicelib/devices").join(path))?;
    let start_token = if family == ModelVariant::HiSimHv1 {
        "tcjbd = model->HSMHV_tcjbd ;"
    } else {
        "tcjbs = model->HSMHV2_tcjbs ;"
    };
    let end_token = if family == ModelVariant::HiSimHv1 {
        "/*-----------------------------------------------------------* \n   * End of PART-4."
    } else {
        "/*---------------------------------------------------* \n   * Junction diode."
    };
    let start = c
        .find(start_token)
        .ok_or("original charge source start moved")?;
    let end = c[start..]
        .find(end_token)
        .ok_or("original charge source end moved")?
        + start;
    let section = &c[start..end];
    for field in fields {
        if !section.contains(&format!("model->{prefix}_{field}")) {
            return Err(format!("original model charge parameter {field} is absent").into());
        }
    }
    let total = fields.len() + 10;
    let mut program =
        String::from("#include <stdio.h>\n#include <math.h>\n#define Fn_Pow(x,y) pow((x),(y))\n");
    writeln!(program, "typedef struct {{ double {prefix}_ktnom;")?;
    for field in fields {
        writeln!(program, "double {prefix}_{field};")?;
    }
    program.push_str("} Model;\n");
    writeln!(
        program,
        "typedef struct {{ double {prefix}_as, {prefix}_ad, {prefix}_ps, {prefix}_pd, {prefix}_weff_nf; }} Instance;"
    )?;
    writeln!(
        program,
        "int main(void) {{\ndouble vec[{total}];\nwhile (scanf(\"{}\", {}) == {total}) {{",
        vec!["%lf"; total].join(" "),
        (0..total)
            .map(|i| format!("&vec[{i}]"))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    program.push_str("Model m={0}; Instance i={0}; Model *model=&m; Instance *here=&i;\n");
    for (index, field) in fields.iter().enumerate() {
        writeln!(program, "m.{prefix}_{field} = vec[{index}];")?;
    }
    writeln!(program, "m.{prefix}_ktnom = vec[{}];", fields.len())?;
    for (offset, field) in ["as", "ad", "ps", "pd", "weff_nf"].into_iter().enumerate() {
        writeln!(
            program,
            "i.{prefix}_{field} = vec[{}];",
            fields.len() + offset + 1
        )?;
    }
    writeln!(
        program,
        "double TTEMP=vec[{}], vbs_jct=vec[{}], vbd_jct=vec[{}], Mfactor=vec[{}];",
        fields.len() + 6,
        fields.len() + 7,
        fields.len() + 8,
        fields.len() + 9
    )?;
    program.push_str("double Qbs=0, Qbd=0, Qbs_dT=0, Qbd_dT=0, Capbs=0, Capbd=0;\n");
    program.push_str("double czbd=0, czbd_dT=0, czbdsw=0, czbdsw_dT=0, czbdswg=0, czbdswg_dT=0;\n");
    program.push_str("double czbs=0, czbs_dT=0, czbssw=0, czbssw_dT=0, czbsswg=0, czbsswg_dT=0;\n");
    program.push_str("double tcjbd=0, tcjbs=0, tcjbdsw=0, tcjbssw=0, tcjbdswg=0, tcjbsswg=0;\n");
    program.push_str("double T1=0, T2=0, T1_dT=0, T2_dT=0, arg=0, sarg=0;\n");
    program.push_str(section);
    program.push_str("\nprintf(\"%.17g %.17g %.17g %.17g %.17g %.17g\\n\", Mfactor*Qbs, Mfactor*Capbs, Mfactor*Qbs_dT, Mfactor*Qbd, Mfactor*Capbd, Mfactor*Qbd_dT);\n}\nreturn 0;\n}\n");
    Ok(program)
}

fn prepared(
    family: ModelVariant,
    extra_model_params: &[(&str, ParameterValue)],
    instance_params: &[(&str, ParameterValue)],
    local_rise: f64,
) -> Result<
    (
        PreparedHiSimHvModel,
        PreparedHiSimHvInstance,
        PreparedHiSimHvTransport,
    ),
    Box<dyn std::error::Error>,
> {
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        family,
        ParameterScope::Model,
        extra_model_params
            .iter()
            .map(|(name, value)| (*name, value.clone())),
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            family,
            ParameterScope::Instance,
            instance_params
                .iter()
                .map(|(name, value)| (*name, value.clone())),
        )?,
    )?;
    let state = prepare_hisim_hv_transport(&model, &instance, 323.0, local_rise)?;
    Ok((model, instance, state))
}

fn native_input(
    family: ModelVariant,
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    state: &PreparedHiSimHvTransport,
    bias: HiSimHvJunctionBias,
) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let fields = if family == ModelVariant::HiSimHv1 {
        HV1_MODEL
    } else {
        HV2_MODEL
    };
    let mut values = Vec::with_capacity(fields.len() + 10);
    for field in fields {
        let slot = family
            .parameter(ParameterScope::Model, field)
            .ok_or_else(|| format!("missing original model field {field}"))?
            .slot;
        let Some(ParameterValue::Real(value)) = model.get_slot(slot) else {
            return Err(format!("missing numeric source model value {field}").into());
        };
        values.push(*value);
    }
    values.push(state.temperature.nominal_temperature_kelvin);
    for field in ["as", "ad", "ps", "pd"] {
        let slot = family
            .parameter(ParameterScope::Instance, field)
            .ok_or_else(|| format!("missing original instance field {field}"))?
            .slot;
        let Some(ParameterValue::Real(value)) = instance.parameters().get_slot(slot) else {
            return Err(format!("missing numeric instance value {field}").into());
        };
        values.push(*value);
    }
    values.push(instance.effective_geometry().channel_width_total_m);
    let rth_slot = family
        .parameter(ParameterScope::Model, "rth0")
        .ok_or("missing rth0 model parameter")?
        .slot;
    let heated = state.thermal_branch.is_some()
        && instance.binned_model_slot(rth_slot).unwrap_or(0.0) != 0.0;
    values.push(if heated {
        state.temperature.device_temperature_kelvin
    } else {
        state.temperature.ambient_device_temperature_kelvin
    });
    values.extend([bias.bulk_source_v, bias.bulk_drain_v]);
    let m_slot = family
        .parameter(ParameterScope::Instance, "m")
        .ok_or("missing instance multiplicity")?
        .slot;
    let Some(ParameterValue::Real(multiplier)) = instance.parameters().get_slot(m_slot) else {
        return Err("missing real instance multiplier".into());
    };
    values.push(*multiplier);
    Ok(values)
}

fn execute_native(
    source: &str,
    cases: &[Vec<f64>],
) -> Result<Vec<[f64; 6]>, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("original-junction.c");
    let executable = temp.path().join("original-junction");
    fs::write(&path, source)?;
    let compiled = Command::new("sccache")
        .args(["cc", "-std=c11", "-O2"])
        .arg(&path)
        .args(["-lm", "-o"])
        .arg(&executable)
        .output()?;
    if !compiled.status.success() {
        return Err(format!(
            "compilation of original C junction formulas failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        )
        .into());
    }
    let mut process = Command::new(&executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = process
        .stdin
        .take()
        .ok_or("original C process stdin missing")?;
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
    let result = process.wait_with_output()?;
    if !result.status.success() {
        return Err(format!(
            "original C junction process failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    let lines = String::from_utf8(result.stdout)?;
    let native = lines
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|v: Vec<f64>| {
                    format!("C returned {} values instead of six", v.len()).into()
                })
        })
        .collect::<Result<Vec<[f64; 6]>, Box<dyn std::error::Error>>>()?;
    if native.len() != cases.len() {
        return Err(format!(
            "C returned {} rows for {} input cases",
            native.len(),
            cases.len()
        )
        .into());
    }
    Ok(native)
}

fn assert_native_matches(family: ModelVariant, native: &[[f64; 6]], expected: &[[f64; 6]]) {
    for (row, (c, rust)) in native.iter().zip(expected).enumerate() {
        for (column, (c_value, rust_value)) in c.iter().zip(rust).enumerate() {
            let tolerance = 5e-11f64.mul_add(c_value.abs().max(rust_value.abs()), 1e-20);
            assert!(
                (c_value - rust_value).abs() <= tolerance,
                "{family:?} junction case {row}, component {column}: native {c_value:e} != Rust {rust_value:e}, tolerance {tolerance:e}"
            );
        }
    }
}

#[test]
#[ignore = "requires pinned ngspice 47 C source and managed sccache C compiler"]
fn original_hv1_hv2_junction_depletion_c_matches_rust() -> Result<(), Box<dyn std::error::Error>> {
    let source_root = upstream_checkout()?;
    let revision = Command::new("git")
        .arg("-C")
        .arg(&source_root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !revision.status.success()
        || String::from_utf8(revision.stdout)?.trim() != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
    {
        return Err("junction oracle expects the pinned ngspice 47 checkout".into());
    }
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let original = compiled_c_source(&source_root, family)?;
        let mut input_cases = Vec::new();
        let mut expected = Vec::new();
        for rth in [0.0, 0.4] {
            let mut model_parameters = vec![
                ("rth0", ParameterValue::Real(rth)),
                ("tcjbs", ParameterValue::Real(0.004)),
                ("tcjbd", ParameterValue::Real(-0.002)),
                ("tcjbssw", ParameterValue::Real(0.001)),
                ("tcjbdsw", ParameterValue::Real(0.002)),
                ("tcjbsswg", ParameterValue::Real(0.003)),
                ("tcjbdswg", ParameterValue::Real(-0.001)),
            ];
            if family == ModelVariant::HiSimHv2 {
                model_parameters.extend([
                    ("cjs", ParameterValue::Real(2e-3)),
                    ("cjd", ParameterValue::Real(4e-3)),
                    ("cjsws", ParameterValue::Real(1e-9)),
                    ("cjswd", ParameterValue::Real(2e-9)),
                ]);
            } else {
                model_parameters.extend([
                    ("cj", ParameterValue::Real(2e-3)),
                    ("cjsw", ParameterValue::Real(1e-9)),
                ]);
            }
            for rise in [0.0, 9.0, 29.0] {
                for (asrc, adrn, ps, pd, m) in [
                    (0.0, 0.0, 0.0, 0.0, 1.0),
                    (1e-11, 3e-11, 2e-6, 14e-6, 2.0),
                    (4e-12, 2e-11, 8e-6, 2e-6, 0.5),
                    (5e-11, 1e-11, 15e-6, 11e-6, 3.0),
                ] {
                    let instance_parameters = [
                        ("as", ParameterValue::Real(asrc)),
                        ("ad", ParameterValue::Real(adrn)),
                        ("ps", ParameterValue::Real(ps)),
                        ("pd", ParameterValue::Real(pd)),
                        ("m", ParameterValue::Real(m)),
                        ("coselfheat", ParameterValue::Integer(1)),
                    ];
                    let (model, instance, temp) =
                        prepared(family, &model_parameters, &instance_parameters, rise)?;
                    let ready = prepare_hisim_hv_junction_charges(&model, &instance, &temp)?;
                    for (vbs, vbd) in [
                        (-0.75, 0.2),
                        (-0.15, -0.25),
                        (0.0, 0.0),
                        (0.2, 0.7),
                        (0.7, -0.9),
                        (-2.5, 0.01),
                    ] {
                        let bias = HiSimHvJunctionBias {
                            bulk_source_v: vbs,
                            bulk_drain_v: vbd,
                        };
                        let result = ready.evaluate(bias)?;
                        input_cases.push(native_input(family, &model, &instance, &temp, bias)?);
                        expected.push([
                            result.bulk_source.charge_coulomb,
                            result.bulk_source.capacitance_farad,
                            result
                                .bulk_source
                                .charge_temperature_derivative_coulomb_per_kelvin,
                            result.bulk_drain.charge_coulomb,
                            result.bulk_drain.capacitance_farad,
                            result
                                .bulk_drain
                                .charge_temperature_derivative_coulomb_per_kelvin,
                        ]);
                    }
                }
            }
        }
        let native = execute_native(&original, &input_cases)?;
        assert_native_matches(family, &native, &expected);
        eprintln!(
            "{family:?}: {} native-C junction cases × 6 charge/capacitance/thermal fields agree",
            native.len()
        );
    }
    Ok(())
}
