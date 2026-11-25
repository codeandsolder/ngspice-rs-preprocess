//! Opt-in *verbatim native C* oracle for nonlinear HiSIM-HV electrothermal power.
//!
//! Reads the original ngspice 47 `hsmhv[2]ld.c` code, extracts its actual
//! `Fn_SU` macro and entire self-heating / drain-power block, and compiles
//! them with managed sccache. The reference is executed independently of the
//! Rust implementation, avoiding a hand-translated C formula fixture.
#![cfg(target_os = "linux")]

use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use model_compact_intrinsic::{
    BoundParameters, HiSimHvDrainBias, HiSimHvDrainCurrent, HiSimHvElectricalGradient,
    ModelVariant, ParameterScope, ParameterValue, evaluate_hisim_hv_generated_heat,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_transport,
};

fn source_checkout() -> Result<PathBuf, String> {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in crate_root.ancestors() {
        for suffix in ["projects/ngspice", "ngspice"] {
            let checkout = ancestor.join(suffix);
            if checkout
                .join("src/spicelib/devices/hisimhv1/hsmhvld.c")
                .is_file()
            {
                return Ok(checkout);
            }
        }
    }
    Err("ngspice 47 source checkout not found in an ancestor's projects/ngspice or ngspice; this opt-in native oracle requires its C source".to_owned())
}

fn extract_native_source(version: u8, checkout: &Path) -> Result<String, String> {
    let family = if version == 1 { "hisimhv1" } else { "hisimhv2" };
    let source_name = if version == 1 {
        "hsmhvld.c"
    } else {
        "hsmhv2ld.c"
    };
    let prefix = if version == 1 { "HSMHV" } else { "HSMHV2" };
    let path = checkout
        .join("src/spicelib/devices")
        .join(family)
        .join(source_name);
    let source = fs::read_to_string(&path).map_err(|error| error.to_string())?;
    let macro_start = source
        .find("#define Fn_SU(")
        .ok_or("native Fn_SU macro missing")?;
    let macro_end = source[macro_start..]
        .find("\n\n")
        .ok_or("native Fn_SU macro boundary missing")?
        + macro_start;
    let macro_text = &source[macro_start..macro_end];
    let comment = source
        .find("/*     P = Ids")
        .ok_or("native power equation marker missing")?;
    let entry = if version == 1 {
        "if (flg_tempNode > 0) {"
    } else {
        "if (here->HSMHV2_coselfheat > 0) {"
    };
    let start = source[..comment]
        .rfind(entry)
        .ok_or("native thermal block start missing")?;
    let end = source[comment..]
        .find("/* in case of nqs:")
        .ok_or("native thermal block end missing")?
        + comment;
    let power_block = &source[start..end];
    let node_start = source[end..]
        .find("ydc_t[dNode] = - dP_dVdse ;")
        .ok_or("native thermal Jacobian row start missing")?
        + end;
    let node_end = source[node_start..]
        .find("ydc_t[tempNode] = dIth_dT - dP_dT ;")
        .ok_or("native thermal Jacobian row end missing")?
        + node_start
        + "ydc_t[tempNode] = dIth_dT - dP_dT ;".len();
    let jacobian_block = &source[node_start..node_end];
    if power_block.matches('{').count() != power_block.matches('}').count()
        || !power_block.contains(&format!("pParam->{prefix}_powrat"))
        || !power_block.contains(&format!("model->{prefix}_shemax"))
    {
        return Err("native C power block structure changed; review extraction".to_owned());
    }

    let template = r"
typedef struct { double @PREFIX@_rth, @PREFIX@_cth, @PREFIX@_powrat; } Param;
typedef struct { double @PREFIX@_shemax, @PREFIX@_type; } Model;
typedef struct { double @PREFIX@_powratio; int @PREFIX@_coselfheat; } Instance;
";
    let mut c_source = String::from(
        "#include <stdio.h>\n#include <math.h>\n#define C_RTH_MIN 1.0e-4\n#define SHE_MAX_dlt 0.1\n",
    );
    c_source.push_str(&template.replace("@PREFIX@", prefix));
    c_source.push_str(macro_text);
    c_source.push_str("\nint main(void) {\n double vec[17];\n while (scanf(\"");
    c_source.push_str(&vec!["%lf"; 17].join(" "));
    c_source.push_str("\", ");
    c_source.push_str(
        &(0..17)
            .map(|index| format!("&vec[{index}]"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    c_source.push_str(") == 17) {\n");
    writeln!(
        c_source,
        "Param params={{.{prefix}_rth=vec[0], .{prefix}_cth=vec[1], .{prefix}_powrat=vec[2]}};"
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        c_source,
        "Model model_buf={{.{prefix}_shemax=vec[3], .{prefix}_type=vec[16]}};"
    )
    .map_err(|error| error.to_string())?;
    writeln!(
        c_source,
        "Instance here_buf={{.{prefix}_powratio=vec[4], .{prefix}_coselfheat=1}};"
    )
    .map_err(|error| error.to_string())?;
    c_source.push_str("Param *pParam=&params; Model *model=&model_buf; Instance *here=&here_buf;\nint flg_tempNode=1;\n");
    for (index, name) in [
        "deltemp", "vds", "vdse", "Ids", "gds", "gm", "gmbs", "gds_ext", "gm_ext", "gmbs_ext",
        "gmT",
    ]
    .iter()
    .enumerate()
    {
        writeln!(c_source, "double {name}=vec[{}];", index + 5)
            .map_err(|error| error.to_string())?;
    }
    c_source.push_str("double TMF1=0, TMF2=0, T0=0, T1=0, T2=0;\n");
    c_source.push_str("double Gth=0, Ith=0, dIth_dT=0, Cth=0, Qth=0;\n");
    c_source.push_str("double Veffpower=0, dVeffpower_dVds=0, dVeffpower_dVdse=0;\n");
    c_source.push_str("double P=0, dP_dVds=0, dP_dVgs=0, dP_dVbs=0, dP_dVdse=0, dP_dVgse=0, dP_dVbse=0, dP_dT=0;\n");
    c_source.push_str(power_block);
    c_source.push_str("\nenum { dNode=0, dNodePrime=1, gNodePrime=2, sNode=3, sNodePrime=4, bNodePrime=5, tempNode=6 };\ndouble ydc_t[7]={0};\n");
    c_source.push_str(jacobian_block);
    let outputs = [
        "Veffpower",
        "Ids",
        "P",
        "dP_dVds",
        "dP_dVgs",
        "dP_dVbs",
        "dP_dVdse",
        "dP_dVgse",
        "dP_dVbse",
        "dP_dT",
        "Gth",
        "Ith",
        "dIth_dT",
        "Cth",
        "Qth",
        "T0",
        "ydc_t[dNode] * model->@PREFIX@_type",
        "ydc_t[dNodePrime] * model->@PREFIX@_type",
        "ydc_t[gNodePrime] * model->@PREFIX@_type",
        "ydc_t[sNode] * model->@PREFIX@_type",
        "ydc_t[sNodePrime] * model->@PREFIX@_type",
        "ydc_t[bNodePrime] * model->@PREFIX@_type",
        "ydc_t[tempNode]",
    ];
    c_source.push_str("\nprintf(\"");
    c_source.push_str(&vec!["%.17g"; outputs.len()].join(" "));
    c_source.push_str("\\n\", ");
    c_source.push_str(&outputs.join(", ").replace("@PREFIX@", prefix));
    c_source.push_str(");\n}\nreturn 0;\n}\n");
    Ok(c_source)
}

fn prepare(
    variant: ModelVariant,
    powrat: f64,
    shemax: f64,
    rth0: f64,
    prattemp1: f64,
    rise: f64,
    pmos: bool,
) -> Result<model_compact_intrinsic::PreparedHiSimHvTransport, Box<dyn std::error::Error>> {
    let mut model_fields = vec![
        ("powrat", ParameterValue::Real(powrat)),
        ("shemax", ParameterValue::Real(shemax)),
        ("rth0", ParameterValue::Real(rth0)),
        ("prattemp1", ParameterValue::Real(prattemp1)),
    ];
    if pmos {
        model_fields.push(("pmos", ParameterValue::Flag(true)));
    }
    let raw = BoundParameters::bind(variant, ParameterScope::Model, model_fields)?;
    let model = prepare_hisim_hv_model(&raw)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [("coselfheat", ParameterValue::Integer(1))],
        )?,
    )?;
    Ok(prepare_hisim_hv_transport(&model, &instance, 311.15, rise)?)
}

fn run_native(
    source: &str,
    cases: &[[f64; 17]],
) -> Result<Vec<[f64; 23]>, Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let source_file = temp.path().join("thermal-source.c");
    let executable = temp.path().join("thermal-source");
    fs::write(&source_file, source)?;
    let status = Command::new("sccache")
        .args(["cc", "-std=c11", "-O2"])
        .arg(&source_file)
        .args(["-lm", "-o"])
        .arg(&executable)
        .output()?;
    if !status.status.success() {
        return Err(format!(
            "native ngspice C extraction failed to compile: {}",
            String::from_utf8_lossy(&status.stderr)
        )
        .into());
    }
    let mut child = Command::new(&executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("no native oracle input pipe")?;
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
    let native = child.wait_with_output()?;
    if !native.status.success() {
        return Err(format!(
            "native C source oracle failed: {}",
            String::from_utf8_lossy(&native.stderr)
        )
        .into());
    }
    let output = String::from_utf8(native.stdout)?;
    let mut results = Vec::with_capacity(cases.len());
    for line in output.lines() {
        let fields = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        let actual: [f64; 23] = fields.try_into().map_err(|fields: Vec<f64>| {
            format!(
                "C source oracle yielded {} fields rather than 23",
                fields.len()
            )
        })?;
        results.push(actual);
    }
    if results.len() != cases.len() {
        return Err(format!(
            "C source oracle emitted {} rows for {} inputs",
            results.len(),
            cases.len()
        )
        .into());
    }
    Ok(results)
}

/// Case coordinates are explicit, because the two voltage partitions and the
/// original C power limiter must be tested across bias, thermal and channel modes.
#[derive(Clone, Copy)]
struct PowerScenario {
    powrat: f64,
    shemax: f64,
    rise: f64,
    inner: f64,
    outer: f64,
    ids: f64,
    pmos: bool,
}

/// Build one power case in native C parameter/derivative order and independent
/// Rust output order, without testing with a hand-translated C power formula.
fn power_case(
    state: &model_compact_intrinsic::PreparedHiSimHvTransport,
    scenario: PowerScenario,
) -> Result<([f64; 17], [f64; 23]), Box<dyn std::error::Error>> {
    let PowerScenario {
        powrat,
        shemax,
        rise,
        inner,
        outer,
        ids,
        pmos,
    } = scenario;
    let thermal = state
        .thermal_branch
        .ok_or("missing native thermal branch")?;
    let derivatives = HiSimHvElectricalGradient {
        inner_vds: 0.14,
        inner_vgs: 0.52,
        inner_vbs: -0.09,
        outer_vds: 0.041,
        outer_vgs: -0.022,
        outer_vbs: 0.017,
    };
    let bias = HiSimHvDrainBias {
        inner_vds_v: inner,
        outer_vds_v: outer,
    };
    let current = HiSimHvDrainCurrent {
        current_a: ids,
        electrical_derivative_a_per_v: derivatives,
        temperature_derivative_a_per_kelvin: 0.0035,
    };
    let rust = evaluate_hisim_hv_generated_heat(state, bias, current)?
        .ok_or("missing enabled generated heat")?;
    let native_input = [
        state.thermal_resistance_kelvin_per_watt,
        thermal.heat_capacity_j_per_kelvin,
        powrat,
        shemax,
        state.power_coupling_ratio,
        rise,
        inner,
        outer,
        ids,
        derivatives.inner_vds,
        derivatives.inner_vgs,
        derivatives.inner_vbs,
        derivatives.outer_vds,
        derivatives.outer_vgs,
        derivatives.outer_vbs,
        current.temperature_derivative_a_per_kelvin,
        if pmos { -1.0 } else { 1.0 },
    ];
    let gradient = rust.generated_power_electrical_derivative_w_per_v;
    let rust_output = [
        rust.effective_drain_voltage_v,
        ids,
        rust.generated_power_w,
        gradient.inner_vds,
        gradient.inner_vgs,
        gradient.inner_vbs,
        gradient.outer_vds,
        gradient.outer_vgs,
        gradient.outer_vbs,
        rust.generated_power_temperature_derivative_w_per_kelvin,
        thermal.conductance_w_per_kelvin,
        thermal.conductive_heat_w,
        thermal.conductance_w_per_kelvin,
        thermal.heat_capacity_j_per_kelvin,
        thermal.stored_heat_j,
        rust.limit_derivative,
        rust.thermal_dc_jacobian.drain_external_w_per_v,
        rust.thermal_dc_jacobian.drain_internal_w_per_v,
        rust.thermal_dc_jacobian.gate_internal_w_per_v,
        rust.thermal_dc_jacobian.source_external_w_per_v,
        rust.thermal_dc_jacobian.source_internal_w_per_v,
        rust.thermal_dc_jacobian.body_internal_w_per_v,
        rust.thermal_dc_jacobian.temperature_w_per_kelvin,
    ];
    Ok((native_input, rust_output))
}

#[test]
#[ignore = "requires ngspice 47 source checkout and existing managed sccache C compiler"]
fn original_hv1_and_hv2_c_power_blocks_match_rust_thermal_derivatives()
-> Result<(), Box<dyn std::error::Error>> {
    let checkout = source_checkout()?;
    let revision = Command::new("git")
        .args(["-C"])
        .arg(&checkout)
        .args(["rev-parse", "HEAD"])
        .output()?;
    let source_commit = String::from_utf8(revision.stdout)?;
    if !revision.status.success()
        || source_commit.trim() != "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
    {
        return Err(format!(
            "expected pinned ngspice 47 source revision, got {}",
            source_commit.trim()
        )
        .into());
    }
    for variant in [ModelVariant::HiSimHv1, ModelVariant::HiSimHv2] {
        let version = if variant == ModelVariant::HiSimHv1 {
            1
        } else {
            2
        };
        let native_source = extract_native_source(version, &checkout)?;
        let mut native_inputs = Vec::new();
        let mut rust_results = Vec::new();
        for (powrat, shemax, rth0, prattemp1) in [
            (0.25, 420.0, 0.15, 0.001),
            (1.0, 500.0, 0.10, -0.002),
            (0.64, 370.0, 0.05, 0.0),
            (0.0, 550.0, 0.0, 0.0),
        ] {
            for rise in [0.0, 8.0, 22.5] {
                for pmos in [false, true] {
                    let state = prepare(variant, powrat, shemax, rth0, prattemp1, rise, pmos)?;
                    for (inner, outer) in [
                        (2.0, 3.0),
                        (2.0, 1.0),
                        (-2.0, -3.0),
                        (0.0, 1.0),
                        (-0.3, 0.4),
                    ] {
                        for ids in [0.0, 0.01, 0.34, 1.0, 60.0, -0.2] {
                            let (native_input, rust_output) = power_case(
                                &state,
                                PowerScenario {
                                    powrat,
                                    shemax,
                                    rise,
                                    inner,
                                    outer,
                                    ids,
                                    pmos,
                                },
                            )?;
                            native_inputs.push(native_input);
                            rust_results.push(rust_output);
                        }
                    }
                }
            }
        }
        let native = run_native(&native_source, &native_inputs)?;
        for (index, (c_row, rust_row)) in native.iter().zip(&rust_results).enumerate() {
            for (column, (&c_value, &rust_value)) in c_row.iter().zip(rust_row).enumerate() {
                let tolerance = 5e-11f64.mul_add(c_value.abs().max(rust_value.abs()), 5e-11);
                assert!(
                    (c_value - rust_value).abs() <= tolerance,
                    "HV{version} sample {index} column {column}: native C {c_value:.16e}, Rust {rust_value:.16e}, tolerance {tolerance:.3e}"
                );
            }
        }
        eprintln!(
            "HV{version}: {} independently compiled ngspice C source power cases × 23 scalar results match",
            native.len()
        );
    }
    Ok(())
}
