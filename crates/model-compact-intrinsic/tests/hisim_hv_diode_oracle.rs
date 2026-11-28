//! Opt-in reference: extract and execute the **original** ngspice 47 HV1/HV2
//! junction-diode temperature prelude and diode `I/G/dI_dT` equations.
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
    prepare_hisim_hv_instance, prepare_hisim_hv_junction_diodes, prepare_hisim_hv_model,
    prepare_hisim_hv_transport,
};

const HV1_MODEL: &[&str] = &[
    "cisb", "ctemp", "cvb", "cvbk", "divx", "njsw", "xti", "xti2",
];
const HV1_BIN: &[&str] = &["cisbk", "js0", "js0sw", "nj", "vdiffj"];
const HV2_MODEL: &[&str] = &[
    "cisbd", "cisbs", "ctempd", "ctemps", "cvbd", "cvbs", "divxd", "divxs", "njswd", "njsws",
    "xti2d", "xti2s", "xtid", "xtis",
];
const HV2_BIN: &[&str] = &[
    "cisbkd", "cisbks", "js0d", "js0s", "js0swd", "js0sws", "njd", "njs", "vdiffjd", "vdiffjs",
];
const TEMPERATURE_VARIABLES: &[&str] = &[
    "T0",
    "T0_dT",
    "T1",
    "T1_dT",
    "T1_dVb",
    "T2",
    "T2_dT",
    "T2_dVb",
    "T3",
    "T3_dT",
    "T3_dVb",
    "T4",
    "T4_dT",
    "T9",
    "T9_dT",
    "T10",
    "T10_dT",
    "T11",
    "T11_dT",
    "T12",
    "T12_dT",
    "TX",
    "js",
    "js_dT",
    "jssw",
    "jssw_dT",
    "js2",
    "js2_dT",
    "jssw2",
    "jssw2_dT",
    "isbd_dT",
    "isbd2_dT",
    "isbs_dT",
    "isbs2_dT",
    "vbdt_dT",
    "vbst_dT",
    "exptemp_dT",
    "exptempd_dT",
    "exptemps_dT",
    "jd_expcd_dT",
    "jd_expcs_dT",
    "jd_nvtm_inv_dT",
    "jd_nvtm_invd_dT",
    "jd_nvtm_invs_dT",
    "log_Tratio",
    "Ibd",
    "Ibd_dT",
    "Gbd",
    "Ibs",
    "Ibs_dT",
    "Gbs",
];

fn ngspice_checkout() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in root.ancestors() {
        for suffix in ["projects/ngspice", "ngspice"] {
            let path = ancestor.join(suffix);
            if path
                .join("src/spicelib/devices/hisimhv2/hsmhv2eval_dio.c")
                .exists()
            {
                return Ok(path);
            }
        }
    }
    Err("pinned ngspice 47 source checkout not found".into())
}

fn source_blocks(
    family: ModelVariant,
    source_root: &Path,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    let (prefix, temp_file, eval_file, temp_begin, temp_end, eval_begin) =
        if family == ModelVariant::HiSimHv1 {
            (
                "HSMHV",
                "hsmhvtemp_eval.h",
                "hsmhveval.c",
                "/* for substrate-source/drain junction diode. */",
                "/* costi0 and costi1",
                "  T10 = model->HSMHV_cvb *",
            )
        } else {
            (
                "HSMHV2",
                "hsmhv2temp_eval_dio.h",
                "hsmhv2eval_dio.c",
                "        log_Tratio = log (here->HSMHV2_Tratio)",
                "/* end of HSMHV2temp_eval_dio.h */",
                "  /* ibd */",
            )
        };
    let folder = if family == ModelVariant::HiSimHv1 {
        "hisimhv1"
    } else {
        "hisimhv2"
    };
    let source = source_root.join("src/spicelib/devices").join(folder);
    let temperature_code = fs::read_to_string(source.join(temp_file))?;
    let eval_code = fs::read_to_string(source.join(eval_file))?;
    let temperature_start = temperature_code
        .find(temp_begin)
        .ok_or("native diode temp entry missing")?;
    let temperature_end = temperature_code[temperature_start..]
        .find(temp_end)
        .ok_or("native diode temp exit missing")?
        + temperature_start;
    let current_start = eval_code
        .find(eval_begin)
        .ok_or("native diode I/G start missing")?;
    let current_end = eval_code[current_start..]
        .find("Charges and Capacitances.")
        .ok_or("native diode I/G end missing")?
        + current_start;
    let current_end = eval_code[..current_end]
        .rfind("/*-----------------------------------------------------------*")
        .ok_or("native diode current section ending not found")?;
    let temperature = &temperature_code[temperature_start..temperature_end];
    let current = &eval_code[current_start..current_end];
    for needed in ["jd_expcd", "js0", "isbd", "isbs", "vbdt", "vbst"] {
        if !format!("{temperature}\n{current}").contains(needed) {
            return Err(format!("native source identifier {needed} missing").into());
        }
    }
    Ok((
        prefix.to_owned(),
        temperature.to_owned(),
        current.to_owned(),
    ))
}

fn field_names(section: &str, prefix: &str) -> Vec<String> {
    let needle = format!("here->{prefix}_");
    let mut names = section
        .split(&needle)
        .skip(1)
        .map(|remainder| {
            remainder
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn original_c_program(
    family: ModelVariant,
    root: &Path,
) -> Result<String, Box<dyn std::error::Error>> {
    let (prefix, temperature, current) = source_blocks(family, root)?;
    let model_fields = if family == ModelVariant::HiSimHv1 {
        HV1_MODEL
    } else {
        HV2_MODEL
    };
    let bin_fields = if family == ModelVariant::HiSimHv1 {
        HV1_BIN
    } else {
        HV2_BIN
    };
    let count = model_fields.len() + bin_fields.len() + 17;
    let here_names = field_names(&format!("{temperature}\n{current}"), &prefix);
    let mut program = String::from(
        "#include <math.h>\n#include <stdio.h>\n#define EXP_THR 34.0\n#define small 1.0e-50\n",
    );
    if family == ModelVariant::HiSimHv1 {
        program.push_str("#define HSMHVEVAL\n");
    } else {
        program.push_str("#define HSMHV2EVAL\n");
    }
    writeln!(program, "typedef struct {{ double {prefix}_ktnom;")?;
    for name in model_fields {
        writeln!(program, "double {prefix}_{name};")?;
    }
    program.push_str("} Model;\n");
    program.push_str("typedef struct {\n");
    for name in bin_fields {
        writeln!(program, "double {prefix}_{name};")?;
    }
    program.push_str("} Param;\n");
    program.push_str("typedef struct {\n");
    for name in here_names {
        writeln!(program, "double {prefix}_{name};")?;
    }
    program.push_str("} Instance;\n");
    writeln!(program, "int main(void) {{ double v[{count}];")?;
    writeln!(
        program,
        "while(scanf(\"{}\", {}) == {count}) {{",
        vec!["%lf"; count].join(" "),
        (0..count)
            .map(|index| format!("&v[{index}]"))
            .collect::<Vec<_>>()
            .join(", ")
    )?;
    program.push_str("Model md={0}; Param par={0}; Instance ins={0};\n");
    program.push_str("Model *model=&md; Param *pParam=&par; Instance *here=&ins;\n");
    let mut offset = 0;
    for name in model_fields {
        writeln!(program, "md.{prefix}_{name}=v[{offset}];")?;
        offset += 1;
    }
    for name in bin_fields {
        writeln!(program, "par.{prefix}_{name}=v[{offset}];")?;
        offset += 1;
    }
    writeln!(program, "md.{prefix}_ktnom=v[{offset}];")?;
    offset += 1;
    for name in ["egtnom", "betatnom", "as", "ad", "ps", "pd", "Tratio"] {
        if family == ModelVariant::HiSimHv1 && name == "Tratio" {
            offset += 1;
            continue;
        }
        writeln!(program, "ins.{prefix}_{name}=v[{offset}];")?;
        offset += 1;
    }
    for name in [
        "TTEMP", "Eg", "Eg_dT", "beta", "beta_dT", "beta_inv", "vbs_jct", "vbd_jct", "Mfactor",
    ] {
        writeln!(program, "double {name}=v[{offset}];")?;
        offset += 1;
    }
    assert_eq!(offset, count);
    for var in TEMPERATURE_VARIABLES {
        if ["Eg", "Eg_dT", "beta", "beta_dT", "beta_inv"].contains(var) {
            continue;
        }
        writeln!(program, "double {var}=0.0;")?;
    }
    program.push_str(&temperature);
    program.push_str(&current);
    program.push_str("\nprintf(\"%.17g %.17g %.17g %.17g %.17g %.17g\\n\", Mfactor*Ibs, Mfactor*Gbs, Mfactor*Ibs_dT, Mfactor*Ibd, Mfactor*Gbd, Mfactor*Ibd_dT);\n}\nreturn 0;\n}\n");
    Ok(program)
}

fn prepared(
    family: ModelVariant,
    model_params: &[(&str, ParameterValue)],
    instance_params: &[(&str, ParameterValue)],
    rise: f64,
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
        model_params
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
    let transport = prepare_hisim_hv_transport(&model, &instance, 318.15, rise)?;
    Ok((model, instance, transport))
}

fn native_input(
    family: ModelVariant,
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
    transport: &PreparedHiSimHvTransport,
    bias: HiSimHvJunctionBias,
) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    let model_fields = if family == ModelVariant::HiSimHv1 {
        HV1_MODEL
    } else {
        HV2_MODEL
    };
    let bin_fields = if family == ModelVariant::HiSimHv1 {
        HV1_BIN
    } else {
        HV2_BIN
    };
    let mut result = Vec::with_capacity(model_fields.len() + bin_fields.len() + 17);
    for field in model_fields {
        let slot = family
            .parameter(ParameterScope::Model, field)
            .ok_or("model field missing")?
            .slot;
        let Some(ParameterValue::Real(value)) = model.get_slot(slot) else {
            return Err("nonreal original C model parameter".into());
        };
        result.push(*value);
    }
    for field in bin_fields {
        let slot = family
            .parameter(ParameterScope::Model, field)
            .ok_or("binned model field missing")?
            .slot;
        result.push(
            instance
                .binned_model_slot(slot)
                .ok_or("source binned field absent")?,
        );
    }
    let temp = transport.temperature;
    result.extend([
        temp.nominal_temperature_kelvin,
        temp.nominal_bandgap_ev,
        temp.nominal_inverse_thermal_voltage,
    ]);
    for field in ["as", "ad", "ps", "pd"] {
        let slot = family
            .parameter(ParameterScope::Instance, field)
            .ok_or("instance field missing")?
            .slot;
        let Some(ParameterValue::Real(value)) = instance.parameters().get_slot(slot) else {
            return Err("nonreal original C instance geometry".into());
        };
        result.push(*value);
    }
    result.extend([
        temp.temperature_ratio,
        temp.device_temperature_kelvin,
        temp.bandgap_ev,
        temp.bandgap_derivative_ev_per_kelvin,
        temp.inverse_thermal_voltage,
        temp.inverse_thermal_voltage_derivative,
        1.0 / temp.inverse_thermal_voltage,
        bias.bulk_source_v,
        bias.bulk_drain_v,
    ]);
    let m_slot = family
        .parameter(ParameterScope::Instance, "m")
        .ok_or("missing M")?
        .slot;
    let Some(ParameterValue::Real(m)) = instance.parameters().get_slot(m_slot) else {
        return Err("nonreal instance M".into());
    };
    result.push(*m);
    Ok(result)
}

fn native_run(
    source: &str,
    inputs: &[Vec<f64>],
) -> Result<Vec<[f64; 6]>, Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let native_c = temporary.path().join("original-diode.c");
    let executable = temporary.path().join("original-diode");
    fs::write(&native_c, source)?;
    let compiled = Command::new("sccache")
        .args(["cc", "-std=c11", "-O2"])
        .arg(&native_c)
        .args(["-lm", "-o"])
        .arg(&executable)
        .output()?;
    if !compiled.status.success() {
        return Err(format!(
            "compiling original ngspice C diode expressions failed: {}",
            String::from_utf8_lossy(&compiled.stderr)
        )
        .into());
    }
    let mut child = Command::new(&executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or("native original C stdin unavailable")?;
    for data in inputs {
        writeln!(
            input,
            "{}",
            data.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )?;
    }
    drop(input);
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(format!(
            "original C diode evaluation failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )
        .into());
    }
    let output = String::from_utf8(result.stdout)?;
    let rows = output
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?
                .try_into()
                .map_err(|v: Vec<f64>| {
                    format!("native C returned {} scalar values, expected 6", v.len()).into()
                })
        })
        .collect::<Result<Vec<[f64; 6]>, Box<dyn std::error::Error>>>()?;
    if rows.len() != inputs.len() {
        return Err(format!(
            "native source diode oracle produced {} rows for {} cases",
            rows.len(),
            inputs.len()
        )
        .into());
    }
    Ok(rows)
}

fn physical_model_fields(variant: ModelVariant, flavor: u8) -> Vec<(&'static str, ParameterValue)> {
    let magnifier = 1.0 + f64::from(flavor);
    let mut fields = vec![
        ("rth0", ParameterValue::Real(0.3)),
        ("bgtmp1", ParameterValue::Real(0.0004)),
        ("bgtmp2", ParameterValue::Real(0.000_000_2)),
    ];
    if variant == ModelVariant::HiSimHv1 {
        fields.extend([
            ("js0", ParameterValue::Real(0.018 * magnifier)),
            ("js0sw", ParameterValue::Real(0.001 * magnifier)),
            ("xti", ParameterValue::Real(1.1)),
            ("xti2", ParameterValue::Real(1.6)),
            ("vdiffj", ParameterValue::Real(0.00003)),
            ("cvb", ParameterValue::Real(0.22)),
            ("cvbk", ParameterValue::Real(0.41)),
            ("cisb", ParameterValue::Real(0.13)),
            ("cisbk", ParameterValue::Real(2e-13)),
            ("ctemp", ParameterValue::Real(1.4)),
            ("divx", ParameterValue::Real(0.08)),
        ]);
    } else {
        fields.extend([
            ("js0d", ParameterValue::Real(0.018 * magnifier)),
            ("js0swd", ParameterValue::Real(0.001 * magnifier)),
            ("js0s", ParameterValue::Real(0.025 * magnifier)),
            ("js0sws", ParameterValue::Real(0.002 * magnifier)),
            ("xtid", ParameterValue::Real(1.1)),
            ("xtis", ParameterValue::Real(1.7)),
            ("xti2d", ParameterValue::Real(1.6)),
            ("xti2s", ParameterValue::Real(1.9)),
            ("vdiffjd", ParameterValue::Real(0.00003)),
            ("vdiffjs", ParameterValue::Real(0.00008)),
            ("cvbd", ParameterValue::Real(0.22)),
            ("cvbs", ParameterValue::Real(0.35)),
            ("cisbd", ParameterValue::Real(0.13)),
            ("cisbs", ParameterValue::Real(0.2)),
            ("cisbkd", ParameterValue::Real(2e-13)),
            ("cisbks", ParameterValue::Real(3e-13)),
            ("ctempd", ParameterValue::Real(1.4)),
            ("ctemps", ParameterValue::Real(1.1)),
            ("divxd", ParameterValue::Real(0.08)),
            ("divxs", ParameterValue::Real(0.04)),
        ]);
    }
    fields
}

fn original_revision_is_pinned(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let git = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !git.status.success()
        || String::from_utf8(git.stdout)?.trim() != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
    {
        return Err("original ngspice source checkout is not pinned revision 972dbf45".into());
    }
    Ok(())
}

fn check_native(family: ModelVariant, native: &[[f64; 6]], expected: &[[f64; 6]]) {
    for (index, (actual, rust)) in native.iter().zip(expected).enumerate() {
        for (field, (&c, &r)) in actual.iter().zip(rust).enumerate() {
            let tolerance = 3e-10f64.mul_add(c.abs().max(r.abs()), 1e-18);
            assert!(
                (c - r).abs() <= tolerance,
                "{family:?} junction diode case {index}, field {field}: upstream C {c:.17e}, Rust {r:.17e}, tolerance {tolerance:.17e}"
            );
        }
    }
}

#[test]
#[ignore = "requires pinned ngspice 47 C source and managed sccache C compiler"]
fn hv1_and_hv2_original_diode_temperature_and_iv_match_rust()
-> Result<(), Box<dyn std::error::Error>> {
    let source_root = ngspice_checkout()?;
    original_revision_is_pinned(&source_root)?;
    for family in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let native_source = original_c_program(family, &source_root)?;
        let mut inputs = Vec::new();
        let mut expected = Vec::new();
        for flavor in 0..3 {
            let model_fields = physical_model_fields(family, flavor);
            for rise in [0.0, 7.0, 23.0] {
                for (ad, as_, pd, ps, m) in [
                    (3e-11, 5e-11, 8e-6, 12e-6, 1.0),
                    (6e-12, 4e-11, 20e-6, 4e-6, 2.0),
                    (0.0, 0.0, 0.0, 0.0, 0.5),
                ] {
                    let instance_fields = [
                        ("ad", ParameterValue::Real(ad)),
                        ("as", ParameterValue::Real(as_)),
                        ("pd", ParameterValue::Real(pd)),
                        ("ps", ParameterValue::Real(ps)),
                        ("m", ParameterValue::Real(m)),
                        ("coselfheat", ParameterValue::Integer(1)),
                    ];
                    let (model, instance, transport) =
                        prepared(family, &model_fields, &instance_fields, rise)?;
                    let prepared_diodes =
                        prepare_hisim_hv_junction_diodes(&model, &instance, &transport)?;
                    for (vbs, vbd) in [
                        (-0.7, 0.2),
                        (-0.1, -0.3),
                        (0.0, 0.0),
                        (0.02, 0.05),
                        (0.1, 0.4),
                        (0.5, -0.4),
                        (-0.03, 0.75),
                    ] {
                        let bias = HiSimHvJunctionBias {
                            bulk_source_v: vbs,
                            bulk_drain_v: vbd,
                        };
                        let rust = prepared_diodes.evaluate(bias)?;
                        inputs.push(native_input(family, &model, &instance, &transport, bias)?);
                        expected.push([
                            rust.bulk_source.current_a,
                            rust.bulk_source.conductance_siemens,
                            rust.bulk_source.temperature_derivative_a_per_kelvin,
                            rust.bulk_drain.current_a,
                            rust.bulk_drain.conductance_siemens,
                            rust.bulk_drain.temperature_derivative_a_per_kelvin,
                        ]);
                    }
                }
            }
        }
        let native = native_run(&native_source, &inputs)?;
        check_native(family, &native, &expected);
        eprintln!(
            "{family:?}: {} verbatim ngspice C diode cases × six fields match",
            native.len()
        );
    }
    Ok(())
}
