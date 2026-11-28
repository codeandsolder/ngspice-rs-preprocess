use model_compact_intrinsic::{
    MosBottomMeasure, MosDiffusionDimensions, MosDiffusionGeometry, MosDiffusionRule,
    MosDiffusionTerminal, MosSeriesResistance, MosSeriesResistanceRule, MosSidewallRule,
    prepare_mos_diffusion,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 2e-14 * actual.abs().max(expected.abs()).max(1e-30),
        "{actual:e} != {expected:e}"
    );
}

const DIMENSIONS: MosDiffusionDimensions = MosDiffusionDimensions {
    width_m: 10e-6,
    width_multiplier: 0.8,
    width_offset_m: 1e-6,
    heavy_diffusion_m: 2e-6,
};

fn area_and_perimeter(geometry: MosDiffusionGeometry, area: f64, perimeter: f64) -> TestResult {
    let MosBottomMeasure::AreaM2(actual) = geometry.bottom else {
        return Err("expected area-normalized junction".into());
    };
    near(actual, area);
    near(geometry.perimeter_m, perimeter);
    Ok(())
}

#[test]
fn geometry_rules_preserve_units_presence_and_sharing() -> TestResult {
    let omitted = MosDiffusionTerminal::default();
    let explicit = MosDiffusionTerminal {
        area_m2: Some(12e-12),
        perimeter_m: Some(4e-6),
        shared: false,
    };
    let zero = MosDiffusionTerminal {
        area_m2: Some(0.),
        perimeter_m: Some(0.),
        shared: true,
    };
    near(DIMENSIONS.diffusion_width()?, 9e-6);
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::ExplicitPhysical, DIMENSIONS, explicit)?,
        12e-12,
        4e-6,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::ExplicitPhysical, DIMENSIONS, omitted)?,
        0.,
        0.,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::ExplicitLithographic, DIMENSIONS, explicit)?,
        7.68e-12,
        3.2e-6,
    )?;
    let normalized =
        prepare_mos_diffusion(MosDiffusionRule::WidthNormalized, DIMENSIONS, explicit)?;
    let MosBottomMeasure::WidthM(width) = normalized.bottom else {
        return Err("expected width-normalized bottom".into());
    };
    near(width, 7.2e-6);
    near(normalized.perimeter_m, 9e-6);
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::Isolated, DIMENSIONS, omitted)?,
        28.8e-12,
        24.4e-6,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::Isolated, DIMENSIONS, zero)?,
        0.,
        0.,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(
            MosDiffusionRule::Isolated,
            DIMENSIONS,
            MosDiffusionTerminal {
                perimeter_m: Some(0.),
                ..omitted
            },
        )?,
        28.8e-12,
        0.,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(MosDiffusionRule::Stacked, DIMENSIONS, omitted)?,
        28.8e-12,
        15.4e-6,
    )?;
    area_and_perimeter(
        prepare_mos_diffusion(
            MosDiffusionRule::Stacked,
            DIMENSIONS,
            MosDiffusionTerminal {
                shared: true,
                ..omitted
            },
        )?,
        14.4e-12,
        3.2e-6,
    )?;
    Ok(())
}

#[test]
fn sidewall_rules_distinguish_gate_edge_and_short_perimeters() -> TestResult {
    for (perimeter, expected_side, expected_gate) in [
        (0., 0., 0.),
        (2e-6, 0., 2e-6),
        (9e-6, 0., 9e-6),
        (13e-6, 4e-6, 9e-6),
    ] {
        let geometry = MosDiffusionGeometry {
            bottom: MosBottomMeasure::AreaM2(0.),
            perimeter_m: perimeter,
        };
        let split = geometry.sidewall_lengths(MosSidewallRule::SplitPerimeter, 9e-6)?;
        near(split.sidewall_m, expected_side);
        near(split.gate_edge_m, expected_gate);
        let separate = geometry.sidewall_lengths(MosSidewallRule::AdditionalGateEdge, 9e-6)?;
        near(separate.sidewall_m, perimeter);
        near(separate.gate_edge_m, 9e-6);
        let ordinary = geometry.sidewall_lengths(MosSidewallRule::EntirePerimeter, 9e-6)?;
        near(ordinary.sidewall_m, perimeter);
        near(ordinary.gate_edge_m, 0.);
    }
    Ok(())
}

#[test]
fn series_laws_resolve_explicit_zero_and_omitted_squares() -> TestResult {
    let model = MosSeriesResistance {
        terminal_resistance: 90.,
        sheet_resistance_ohm_per_square: 45.,
        contact_resistance_ohm: 3.,
        squares: None,
        default_squares: 2.,
        light_diffusion_m: 0.5e-6,
    };
    near(
        model.prepare(MosSeriesResistanceRule::Explicit, DIMENSIONS)?,
        93.,
    );
    near(
        model.prepare(MosSeriesResistanceRule::WidthDependent, DIMENSIONS)?,
        98.,
    );
    near(
        model.prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)?,
        16.,
    );
    near(
        model.prepare(MosSeriesResistanceRule::DirectContact, DIMENSIONS)?,
        183.,
    );
    let zero = MosSeriesResistance {
        squares: Some(0.),
        ..model
    };
    near(
        zero.prepare(MosSeriesResistanceRule::Explicit, DIMENSIONS)?,
        93.,
    );
    near(
        zero.prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)?,
        8.,
    );
    let explicit = MosSeriesResistance {
        squares: Some(3.),
        ..model
    };
    near(
        explicit.prepare(MosSeriesResistanceRule::Explicit, DIMENSIONS)?,
        138.,
    );
    near(
        explicit.prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)?,
        143.,
    );
    assert!(
        MosSeriesResistance {
            squares: Some(f64::NAN),
            ..model
        }
        .prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)
        .is_err()
    );
    Ok(())
}

#[test]
fn nonrepresentable_and_negative_geometry_are_rejected() {
    let terminal = MosDiffusionTerminal::default();
    for dimensions in [
        MosDiffusionDimensions {
            width_multiplier: 0.,
            ..DIMENSIONS
        },
        MosDiffusionDimensions {
            width_offset_m: -100e-6,
            ..DIMENSIONS
        },
        MosDiffusionDimensions {
            heavy_diffusion_m: -1.,
            ..DIMENSIONS
        },
        MosDiffusionDimensions {
            width_m: f64::MAX,
            width_multiplier: 2.,
            ..DIMENSIONS
        },
    ] {
        assert!(prepare_mos_diffusion(MosDiffusionRule::Isolated, dimensions, terminal).is_err());
    }
    assert!(
        prepare_mos_diffusion(
            MosDiffusionRule::ExplicitPhysical,
            DIMENSIONS,
            MosDiffusionTerminal {
                area_m2: Some(-1.),
                ..terminal
            }
        )
        .is_err()
    );
}

#[cfg(target_os = "linux")]
mod native_oracle {
    use super::*;
    use std::{collections::BTreeMap, fs, process::Command};

    const MODEL: MosSeriesResistance = MosSeriesResistance {
        terminal_resistance: 90.,
        sheet_resistance_ohm_per_square: 45.,
        contact_resistance_ohm: 3.,
        squares: None,
        default_squares: 1.,
        light_diffusion_m: 0.5e-6,
    };
    const MULTIPLICITY: f64 = 2.;
    const TEMPERATURE: f64 = 300.15;
    // Both native BSIM3 revisions use their historical KboQ constant.
    const THERMAL_VOLTAGE: f64 = TEMPERATURE * 8.617_087e-5;

    fn simulate(
        version: &str,
        body_bias: f64,
    ) -> Result<BTreeMap<String, f64>, Box<dyn std::error::Error>> {
        let source = format!(
            "ACM12 independent geometry witness\n.options gmin=1e-18 tnom=27\n.temp 27\nVd d 0 0\nVg g 0 0\nVs s 0 0\nVb b 0 {body_bias}\nM1 d g s b n l=2u w=10u m=2 ad=12p ps=4u nrs=3\n.model n nmos level=49 version={version} acm=12 calcacm=1 wmlt=0.8 xw=1u hdif=2u ldif=0.5u rsh=45 rd=90 rs=90 rdc=3 rsc=3 cj=0.001 cjsw=2e-10 cjswg=3e-10 js=0.1 jsw=2e-8 pb=0.9 pbsw=0.8 pbswg=0.7 mj=0.5 mjsw=0.3 mjswg=0.2\n.control\nset numdgt=17\nop\nprint @m1[capbd] @m1[capbs] @m1[ibd] @m1[ibs] @m1[vbs] @m1[vds] i(vd) i(vs)\nquit\n.endc\n.end\n"
        );
        let work = tempfile::tempdir()?;
        fs::write(work.path().join("probe.cir"), source)?;
        let output = Command::new("ngspice")
            .current_dir(work.path())
            .args(["-b", "-n", "probe.cir"])
            .output()?;
        let stdout = String::from_utf8(output.stdout)?;
        assert!(
            output.status.success() && stdout.contains("ngspice-47 done"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut values = BTreeMap::new();
        for line in stdout.lines() {
            if let Some((name, value)) = line.split_once(" = ") {
                if let Ok(value) = value.trim().parse::<f64>() {
                    values.insert(name.trim().to_owned(), value);
                }
            }
        }
        Ok(values)
    }

    fn reference_close(label: &str, actual: f64, expected: f64, relative: f64, absolute: f64) {
        assert!(
            (actual - expected).abs() <= relative * actual.abs().max(expected.abs()) + absolute,
            "{label}: native {actual:e}, predicted {expected:e}"
        );
    }

    fn capacitance(
        geometry: MosDiffusionGeometry,
        bias: f64,
    ) -> Result<f64, Box<dyn std::error::Error>> {
        let MosBottomMeasure::AreaM2(area) = geometry.bottom else {
            return Err("expected area".into());
        };
        let wall = geometry.sidewall_lengths(
            MosSidewallRule::SplitPerimeter,
            DIMENSIONS.diffusion_width()?,
        )?;
        let components = [
            (area * 0.001, 0.9, 0.5),
            (wall.sidewall_m * 2e-10, 0.8, 0.3),
            (wall.gate_edge_m * 3e-10, 0.7, 0.2),
        ];
        Ok(MULTIPLICITY
            * components
                .into_iter()
                .map(|(zero, potential, grading)| {
                    let shape = if bias < 0. {
                        (1. - bias / potential).powf(-grading)
                    } else {
                        1. + grading * bias / potential
                    };
                    zero * shape
                })
                .sum::<f64>())
    }

    fn current(
        geometry: MosDiffusionGeometry,
        bias: f64,
    ) -> Result<f64, Box<dyn std::error::Error>> {
        let MosBottomMeasure::AreaM2(area) = geometry.bottom else {
            return Err("expected area".into());
        };
        let saturation = area * 0.1 + geometry.perimeter_m * 2e-8;
        Ok(MULTIPLICITY * (saturation * (bias / THERMAL_VOLTAGE).exp_m1() + 1e-18 * bias))
    }

    // This witness uses only the native C mode for which the source equations
    // agree: BSIM3 ACM12 with CALCACM=1. It is not an oracle for ACM0–3.
    #[test]
    #[ignore = "requires local ngspice 47 binary"]
    fn acm12_mixed_geometry_matches_bsim3_native_junctions() -> TestResult {
        let drain = prepare_mos_diffusion(
            MosDiffusionRule::Isolated,
            DIMENSIONS,
            MosDiffusionTerminal {
                area_m2: Some(12e-12),
                ..MosDiffusionTerminal::default()
            },
        )?;
        let source = prepare_mos_diffusion(
            MosDiffusionRule::Isolated,
            DIMENSIONS,
            MosDiffusionTerminal {
                perimeter_m: Some(4e-6),
                ..MosDiffusionTerminal::default()
            },
        )?;
        let drain_r =
            MODEL.prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)? / MULTIPLICITY;
        let source_r = MosSeriesResistance {
            squares: Some(3.),
            ..MODEL
        }
        .prepare(MosSeriesResistanceRule::AutomaticSquares, DIMENSIONS)?
            / MULTIPLICITY;
        for version in ["3.24", "3.3.0"] {
            for bias in [-0.5, 0., 0.3] {
                let output = simulate(version, bias)?;
                let vbs = output["@m1[vbs]"];
                let vbd = vbs - output["@m1[vds]"];
                for (suffix, geometry, voltage, resistance, supply) in [
                    ("d", drain, vbd, drain_r, "i(vd)"),
                    ("s", source, vbs, source_r, "i(vs)"),
                ] {
                    reference_close(
                        "junction capacitance",
                        output[&format!("@m1[capb{suffix}]")],
                        capacitance(geometry, voltage)?,
                        2e-6,
                        1e-24,
                    );
                    reference_close(
                        "junction current",
                        output[&format!("@m1[ib{suffix}]")],
                        current(geometry, voltage)?,
                        2e-5,
                        1e-19,
                    );
                    if bias > 0. {
                        reference_close(
                            "series resistance",
                            (bias - voltage) / output[supply],
                            resistance,
                            2e-5,
                            1e-6,
                        );
                    }
                }
            }
        }
        Ok(())
    }
}
