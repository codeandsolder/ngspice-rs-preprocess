use model_compact_intrinsic::{
    Bsim3BerkeleyJunction, Bsim3JunctionModel, Bsim3JunctionRules, Bsim3JunctionTemperature,
    Bsim3KneeReference, MosBottomDensity, MosBottomMeasure, MosDepletionComponent,
    MosDepletionShape, MosDepletionTemperatureCoefficients, MosDiffusionDimensions,
    MosDiffusionGeometry, MosDiffusionRule, MosDiffusionTerminal, MosJunctionDepletion,
    MosJunctionOccurrence, MosJunctionSaturation, MosLinearReverseJunction, MosSaturationFallback,
    MosSeriesResistance, MosSeriesResistanceRule, MosSidewallRule, PreparedBsim3Junction,
    mos_resistance_at_temperature, prepare_mos_diffusion,
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
        light_path_m: 0.5e-6,
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
        light_path_m: 0.7e-6,
    };
    const MULTIPLICITY: f64 = 2.;
    const TEMPERATURE: f64 = 300.15;
    const LIMITING_CURRENT: f64 = 2e-8;
    const GMIN: f64 = 1e-18;

    fn simulate(
        version: &str,
        body_bias: f64,
        temperature_c: f64,
        ijth: f64,
    ) -> Result<BTreeMap<String, f64>, Box<dyn std::error::Error>> {
        let source = format!(
            "ACM12 independent geometry witness\n.options gmin=1e-18 tnom=27 reltol=1e-10 abstol=1e-20 vntol=1e-12 bypass=0\n.temp {temperature_c}\nVd d 0 0\nVg g 0 0\nVs s 0 0\nVb b 0 {body_bias}\nM1 d g s b n l=2u w=10u m=2 ad=12p ps=4u nrs=3\n.model n nmos level=49 version={version} acm=12 calcacm=1 wmlt=0.8 xw=1u hdif=2u ld=0.2u ldif=0.5u ijth={ijth} rsh=45 rd=90 rs=90 rdc=3 rsc=3 cj=0.001 cjsw=2e-10 cjswg=3e-10 js=0.1 jsw=2e-8 pb=0.9 pbsw=0.8 pbswg=0.7 mj=0.5 mjsw=0.3 mjswg=0.2 nj=1.2 xti=3 tcj=0.001 tcjsw=0.002 tcjswg=0.003 tpb=0.001 tpbsw=0.0015 tpbswg=0.002\n.control\nset numdgt=17\nop\nprint @m1[capbd] @m1[capbs] @m1[ibd] @m1[ibs] @m1[gbd] @m1[gbs] @m1[vbs] @m1[vds] i(vd) i(vs)\nquit\n.endc\n.end\n"
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

    fn prepared(
        terminal: MosDiffusionTerminal,
        squares: Option<f64>,
        temperature_c: f64,
        version: &str,
        ijth: f64,
    ) -> Result<PreparedBsim3Junction, Box<dyn std::error::Error>> {
        let model = Bsim3JunctionModel {
            saturation: MosJunctionSaturation {
                bottom: MosBottomDensity::PerSquareMetre(0.1),
                sidewall_a_per_m: 2e-8,
                fallback: MosSaturationFallback::ZeroCurrent(1e-14),
            },
            depletion: MosJunctionDepletion {
                bottom: MosBottomDensity::PerSquareMetre(0.001),
                bottom_shape: MosDepletionShape {
                    potential_v: 0.9,
                    grading: 0.5,
                },
                sidewall_f_per_m: 2e-10,
                sidewall_shape: MosDepletionShape {
                    potential_v: 0.8,
                    grading: 0.3,
                },
                gate_edge_f_per_m: 3e-10,
                gate_edge_shape: MosDepletionShape {
                    potential_v: 0.7,
                    grading: 0.2,
                },
                fallback_f: 0.,
            },
            depletion_temperature: [
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: 0.001,
                    potential_v_per_k: 0.001,
                },
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: 0.002,
                    potential_v_per_k: 0.0015,
                },
                MosDepletionTemperatureCoefficients {
                    capacitance_per_k: if version == "3.24" { 0.003 } else { 0. },
                    potential_v_per_k: 0.002,
                },
            ],
            nominal_temperature_k: TEMPERATURE,
            emission_coefficient: 1.2,
            saturation_temperature_exponent: 3.,
            limiting_current_a: ijth,
        };
        Ok(model.prepare(
            Bsim3JunctionRules {
                diffusion: MosDiffusionRule::Isolated,
                sidewall: MosSidewallRule::SplitPerimeter,
                resistance: MosSeriesResistanceRule::AutomaticSquares,
                knee_reference: if version == "3.24" {
                    Bsim3KneeReference::Resolved
                } else {
                    Bsim3KneeReference::RawExplicit
                },
            },
            MosJunctionOccurrence {
                dimensions: DIMENSIONS,
                terminal,
                gate_width_m: DIMENSIONS.diffusion_width()?,
                series: MosSeriesResistance { squares, ..MODEL },
                operating_temperature_k: temperature_c + 273.15,
            },
        )?)
    }

    // This witness uses only the native C mode for which the source equations
    // agree: BSIM3 ACM12 with CALCACM=1. It is not an oracle for ACM0–3.
    #[test]
    #[ignore = "requires local ngspice 47 binary"]
    fn acm12_mixed_geometry_matches_bsim3_native_junctions() -> TestResult {
        let drain_geometry = MosDiffusionTerminal {
            area_m2: Some(12e-12),
            ..MosDiffusionTerminal::default()
        };
        let source_geometry = MosDiffusionTerminal {
            perimeter_m: Some(4e-6),
            ..MosDiffusionTerminal::default()
        };
        for version in ["3.24", "3.3.0"] {
            for temperature_c in [0., 27., 85.] {
                for ijth in [0., LIMITING_CURRENT] {
                    let drain = prepared(drain_geometry, None, temperature_c, version, ijth)?;
                    let source = prepared(source_geometry, Some(3.), temperature_c, version, ijth)?;
                    for bias in [-0.5, 0., 0.3, 0.6] {
                        let output = simulate(version, bias, temperature_c, ijth)?;
                        let vbs = output["@m1[vbs]"];
                        let vbd = vbs - output["@m1[vds]"];
                        for (suffix, prepared, voltage, supply) in
                            [("d", drain, vbd, "i(vd)"), ("s", source, vbs, "i(vs)")]
                        {
                            let point = prepared.evaluate(voltage)?;
                            reference_close(
                                "junction capacitance",
                                output[&format!("@m1[capb{suffix}]")],
                                MULTIPLICITY * point.capacitance_f,
                                2e-6,
                                1e-24,
                            );
                            reference_close(
                                &format!(
                                    "junction current: {version} {temperature_c}C {bias}V IJTH={ijth} {suffix} at {voltage}V"
                                ),
                                output[&format!("@m1[ib{suffix}]")],
                                MULTIPLICITY * GMIN.mul_add(voltage, point.current_a),
                                2e-5,
                                1e-19,
                            );
                            reference_close(
                                &format!(
                                    "junction conductance: {version} {temperature_c}C {bias}V IJTH={ijth} {suffix} at {voltage}V"
                                ),
                                output[&format!("@m1[gb{suffix}]")],
                                MULTIPLICITY * (point.conductance_s + GMIN),
                                2e-5,
                                1e-20,
                            );
                            if bias > 0. {
                                reference_close(
                                    "series resistance",
                                    (bias - voltage) / output[supply],
                                    prepared.resistance_ohm() / MULTIPLICITY,
                                    2e-5,
                                    1e-6,
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[test]
fn linear_reverse_junction_current_and_diffusion_charge() -> TestResult {
    let saturation = 2e-9;
    let thermal = 0.05;
    let transit = 4e-9;
    let junction = MosLinearReverseJunction::new(saturation, thermal, -1., 4., transit)?;
    for (voltage, current, conductance) in [
        (-3., -3e-9, 0.5e-9),
        (-1., -2e-9, 2e-9),
        (-0.5, -1e-9, 2e-9),
        (0., 0., 4e-8),
        (thermal * 2f64.ln(), saturation, 8e-8),
    ] {
        let point = junction.evaluate(voltage, &[])?;
        near(point.current_a, current);
        near(point.conductance_s, conductance);
        near(point.charge_c, transit * current);
        near(point.capacitance_f, transit * conductance);
    }
    let zero = MosLinearReverseJunction::new(0., thermal, -1., 4., transit)?;
    near(zero.evaluate(f64::MAX, &[])?.current_a, 0.);
    let tiny = MosLinearReverseJunction::new(1e-300, thermal, -1., 4., 0.)?;
    let exponent = 800.;
    near(
        tiny.evaluate(exponent * thermal, &[])?.current_a,
        (1e-300_f64.ln() + exponent).exp(),
    );
    Ok(())
}

#[test]
fn depletion_charge_matches_closed_form_and_numeric_derivatives() -> TestResult {
    let c0 = 5e-12;
    let potential = 0.8;
    let root = MosDepletionComponent::new(c0, potential, 0.5)?;
    let log = MosDepletionComponent::new(c0, potential, 1.)?;
    near(
        root.evaluate(-3. * potential)?.charge_c,
        -2. * c0 * potential,
    );
    near(root.evaluate(-3. * potential)?.capacitance_f, 0.5 * c0);
    near(
        log.evaluate(-3. * potential)?.charge_c,
        -c0 * potential * 4f64.ln(),
    );
    near(log.evaluate(-3. * potential)?.capacitance_f, 0.25 * c0);
    near(root.evaluate(potential)?.charge_c, 1.25 * c0 * potential);
    near(root.evaluate(potential)?.capacitance_f, 1.5 * c0);
    let junction = MosLinearReverseJunction::new(1e-12, 0.04, -1., 3., 1e-8)?;
    for grading in [0., 0.33, 0.5, 1. - 1e-12, 1., 1. + 1e-12, 1.5] {
        let depletion = MosDepletionComponent::new(c0, potential, grading)?;
        near(depletion.evaluate(-1e-18)?.charge_c, -1e-18 * c0);
        for voltage in [-3., -0.5, -1e-4, 1e-4, 0.3, 0.6] {
            let step = 1e-7;
            let center = junction.evaluate(voltage, &[depletion])?;
            let below = junction.evaluate(voltage - step, &[depletion])?;
            let above = junction.evaluate(voltage + step, &[depletion])?;
            let current_derivative = (above.current_a - below.current_a) / (2. * step);
            let charge_derivative = (above.charge_c - below.charge_c) / (2. * step);
            assert!(
                (current_derivative - center.conductance_s).abs() <= 2e-8 * center.conductance_s
            );
            assert!(
                (charge_derivative - center.capacitance_f).abs() <= 2e-8 * center.capacitance_f
            );
        }
    }
    Ok(())
}

#[test]
fn invalid_constitutive_parameters_are_rejected() -> TestResult {
    assert!(MosDepletionComponent::new(-1., 1., 0.5).is_err());
    assert!(MosDepletionComponent::new(1., 0., 0.5).is_err());
    assert!(MosDepletionComponent::new(1., 1., f64::NAN).is_err());
    assert!(MosLinearReverseJunction::new(1., 0., -1., 1., 0.).is_err());
    assert!(MosLinearReverseJunction::new(1., 1., 1., 1., 0.).is_err());
    assert!(MosLinearReverseJunction::new(1., 1., -1., 0., 0.).is_err());
    let junction = MosLinearReverseJunction::new(1., 1., -1., 1., 0.)?;
    assert!(junction.evaluate(f64::INFINITY, &[]).is_err());
    assert!(junction.evaluate(f64::MAX, &[]).is_err());
    Ok(())
}

#[test]
fn junction_densities_preserve_units_and_zero_geometry_fallback() -> TestResult {
    let width = MosDiffusionGeometry {
        bottom: MosBottomMeasure::WidthM(9e-6),
        perimeter_m: 4e-6,
    };
    let zero = MosDiffusionGeometry {
        bottom: MosBottomMeasure::WidthM(0.),
        perimeter_m: 0.,
    };
    let shape = MosDepletionShape {
        potential_v: 1.,
        grading: 0.5,
    };
    let saturation = MosJunctionSaturation {
        bottom: MosBottomDensity::PerMetre(2e-8),
        sidewall_a_per_m: 3e-8,
        fallback: MosSaturationFallback::ZeroCurrent(1e-14),
    };
    near(saturation.prepare(width)?, 3e-13);
    near(saturation.prepare(zero)?, 1e-14);
    assert!(
        MosBottomDensity::PerSquareMetre(1.)
            .weight(width.bottom)
            .is_err()
    );
    assert!(
        MosBottomDensity::PerMetre(1.)
            .weight(MosBottomMeasure::AreaM2(1.))
            .is_err()
    );
    let depletion = MosJunctionDepletion {
        bottom: MosBottomDensity::PerMetre(2e-10),
        bottom_shape: shape,
        sidewall_f_per_m: 3e-10,
        sidewall_shape: shape,
        gate_edge_f_per_m: 4e-10,
        gate_edge_shape: shape,
        fallback_f: 5e-14,
    };
    let [bottom, side, gate] = depletion.prepare(width, MosSidewallRule::SplitPerimeter, 1e-6)?;
    near(bottom.evaluate(0.)?.capacitance_f, 1.8e-15);
    near(side.evaluate(0.)?.capacitance_f, 9e-16);
    near(gate.evaluate(0.)?.capacitance_f, 4e-16);
    let [bottom, side, gate] = depletion.prepare(zero, MosSidewallRule::SplitPerimeter, 1e-6)?;
    near(bottom.evaluate(0.)?.capacitance_f, 5e-14);
    near(side.evaluate(0.)?.capacitance_f, 0.);
    near(gate.evaluate(0.)?.capacitance_f, 0.);
    Ok(())
}

#[test]
fn junction_temperature_preparation_clamps_native_capacitance_and_potential() -> TestResult {
    let nominal = MosDepletionShape {
        potential_v: 0.08,
        grading: 0.5,
    };
    let depletion = MosJunctionDepletion {
        bottom: MosBottomDensity::PerSquareMetre(1e-3),
        bottom_shape: nominal,
        sidewall_f_per_m: 2e-10,
        sidewall_shape: nominal,
        gate_edge_f_per_m: 3e-10,
        gate_edge_shape: nominal,
        fallback_f: 0.,
    };
    let coefficients = MosDepletionTemperatureCoefficients {
        capacitance_per_k: -0.02,
        potential_v_per_k: 0.01,
    };
    let temperature = Bsim3JunctionTemperature::new(300., 400., 1.2, 3.)?;
    let adjusted = temperature.depletion(depletion, [coefficients; 3])?;
    near(adjusted.bottom.weight(MosBottomMeasure::AreaM2(1.))?, 0.);
    near(adjusted.sidewall_f_per_m, 0.);
    near(adjusted.gate_edge_f_per_m, 0.);
    near(adjusted.bottom_shape.potential_v, 0.01);
    near(adjusted.sidewall_shape.potential_v, 0.01);
    near(adjusted.gate_edge_shape.potential_v, 0.01);
    near(
        temperature.emission_thermal_voltage(),
        1.2 * 400. * 8.617_087e-5,
    );
    near(
        mos_resistance_at_temperature(100., 0.003, 300., 400.)?,
        130.,
    );
    assert!(mos_resistance_at_temperature(100., -0.02, 300., 400.).is_err());
    assert!(Bsim3JunctionTemperature::new(0., 300., 1., 3.).is_err());
    Ok(())
}

#[test]
fn berkeley_junction_knee_and_derivatives() -> TestResult {
    let saturation = 1e-12;
    let thermal = 0.04;
    let limit = 2e-8;
    let junction = Bsim3BerkeleyJunction::new(saturation, thermal, limit, saturation)?;
    let knee = thermal * (limit / saturation).ln_1p();
    let knee_point = junction.evaluate(knee, &[])?;
    near(knee_point.current_a, limit);
    near(knee_point.conductance_s, (saturation + limit) / thermal);
    let forward = junction.evaluate(knee + 0.2, &[])?;
    near(
        forward.current_a,
        knee_point.conductance_s.mul_add(0.2, limit),
    );
    near(forward.conductance_s, knee_point.conductance_s);
    let reverse = junction.evaluate(-2., &[])?;
    near(reverse.current_a, -saturation);
    near(
        reverse.conductance_s,
        saturation * (-2. / thermal).exp() / thermal,
    );
    for voltage in [-0.5, -0.02, 0., 0.2, knee - 0.01, knee + 0.01, 1.] {
        let step = 1e-7;
        let point = junction.evaluate(voltage, &[])?;
        let before = junction.evaluate(voltage - step, &[])?;
        let after = junction.evaluate(voltage + step, &[])?;
        let derivative = (after.current_a - before.current_a) / (2. * step);
        assert!((derivative - point.conductance_s).abs() <= 2e-5 * point.conductance_s + 1e-20);
        near(point.charge_c, 0.);
        near(point.capacitance_f, 0.);
    }
    let zero = Bsim3BerkeleyJunction::new(0., thermal, limit, 0.)?;
    near(zero.evaluate(f64::MAX, &[])?.current_a, 0.);
    let unlimited = Bsim3BerkeleyJunction::new(1e-300, thermal, 0., 1e-300)?;
    near(
        unlimited.evaluate(800. * thermal, &[])?.current_a,
        (1e-300_f64.ln() + 800.).exp(),
    );
    assert!(Bsim3BerkeleyJunction::new(saturation, thermal, -1., saturation).is_err());
    Ok(())
}

#[test]
fn saturation_policy_distinguishes_zero_geometry_from_zero_density() -> TestResult {
    let present = MosDiffusionGeometry {
        bottom: MosBottomMeasure::AreaM2(2e-12),
        perimeter_m: 3e-6,
    };
    let absent = MosDiffusionGeometry {
        bottom: MosBottomMeasure::AreaM2(0.),
        perimeter_m: 0.,
    };
    let coefficients = MosJunctionSaturation {
        bottom: MosBottomDensity::PerSquareMetre(0.),
        sidewall_a_per_m: 0.,
        fallback: MosSaturationFallback::ZeroGeometry(1e-14),
    };
    near(coefficients.prepare(present)?, 0.);
    near(coefficients.prepare(absent)?, 1e-14);
    let direct = MosJunctionSaturation {
        fallback: MosSaturationFallback::ZeroCurrent(1e-14),
        ..coefficients
    };
    near(direct.prepare(present)?, 1e-14);
    near(direct.prepare(absent)?, 1e-14);
    let no_direct = MosJunctionSaturation {
        fallback: MosSaturationFallback::None,
        ..coefficients
    };
    near(no_direct.prepare(absent)?, 0.);
    Ok(())
}
