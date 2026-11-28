//! Independent equation checks for the linear-reverse MOS junction.
use model_compact_intrinsic::{
    MosBottomDensity, MosDepletionShape, MosDepletionTemperatureCoefficients,
    MosDepletionThermalLaw, MosDiffusionDimensions, MosDiffusionTerminal, MosJunctionBandgap,
    MosJunctionDepletion, MosJunctionOccurrence, MosJunctionSaturation, MosLinearReverseGeometry,
    MosLinearReverseModel, MosLinearReverseTemperature, MosSaturationFallback, MosSeriesResistance,
    MosSeriesResistanceRule,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const K: f64 = 8.617_087e-5;
const TNOM: f64 = 300.15;
const SHAPE: MosDepletionShape = MosDepletionShape {
    potential_v: 0.9,
    grading: 0.4,
};
const SIDE: MosDepletionShape = MosDepletionShape {
    potential_v: 0.8,
    grading: 0.33,
};

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 5e-13 * expected.abs().max(actual.abs()).max(1e-30),
        "{actual:e} != {expected:e}"
    );
}

fn bandgap(t: f64, eg: f64, alpha: f64, beta: f64) -> f64 {
    eg - alpha * t * t / (t + beta)
}

fn potential(t: f64, p: f64, gap: MosJunctionBandgap) -> f64 {
    let ratio = t / TNOM;
    p * ratio
        - K * t
            * (3. * ratio.ln()
                + (bandgap(
                    TNOM,
                    gap.zero_temperature_ev,
                    gap.alpha_ev_per_k,
                    gap.beta_k,
                ) / (K * TNOM)
                    - bandgap(t, gap.zero_temperature_ev, gap.alpha_ev_per_k, gap.beta_k)
                        / (K * t)))
}

fn saturation(t: f64, n: f64, xti: f64, isat: f64, gap: MosJunctionBandgap) -> f64 {
    let eg = |temperature| {
        bandgap(
            temperature,
            gap.zero_temperature_ev,
            gap.alpha_ev_per_k,
            gap.beta_k,
        )
    };
    isat * ((eg(TNOM) / (K * TNOM) - eg(t) / (K * t) + xti * (t / TNOM).ln()) / n).exp()
}

fn depletion(v: f64, c0: f64, p: f64, m: f64) -> (f64, f64) {
    if v < 0. {
        let base = 1. - v / p;
        (
            c0 * p * (1. - base.powf(1. - m)) / (1. - m),
            c0 * base.powf(-m),
        )
    } else {
        (c0 * (v + m * v * v / (2. * p)), c0 * (1. + m * v / p))
    }
}

#[test]
fn thermal_laws_match_independent_equations() -> TestResult {
    let coefficient = MosDepletionTemperatureCoefficients {
        capacitance_per_k: 0.002,
        potential_v_per_k: 0.001,
    };
    let c0 = 2.3e-13;
    let m = SHAPE.grading;
    let p = SHAPE.potential_v;
    for gap in [
        MosJunctionBandgap::SILICON,
        MosJunctionBandgap {
            zero_temperature_ev: 0.75,
            alpha_ev_per_k: 4.56e-4,
            beta_k: 210.,
        },
    ] {
        for t in [240., TNOM, 340., 400.] {
            let prepared = MosLinearReverseTemperature::new(TNOM, t, 1.2, 2.7, gap)?;
            near(
                prepared.saturation(3e-14)?,
                saturation(t, 1.2, 2.7, 3e-14, gap),
            );
            near(prepared.saturation(0.)?, 0.);
            let dt = t - TNOM;
            let exact = potential(t, p, gap);
            let linear = p - coefficient.potential_v_per_k * dt;
            let eg0 = bandgap(
                TNOM,
                gap.zero_temperature_ev,
                gap.alpha_ev_per_k,
                gap.beta_k,
            );
            let derivative = (p - eg0) / TNOM
                - 3. * K
                - gap.alpha_ev_per_k * TNOM * (TNOM + 2. * gap.beta_k)
                    / (TNOM + gap.beta_k).powi(2);
            let tangent = p + dt * derivative;
            for (law, corrected_p, corrected_c) in [
                (
                    MosDepletionThermalLaw::Semiconductor,
                    exact,
                    c0 * (1. + m * (4e-4 * dt - (exact / p - 1.))),
                ),
                (
                    MosDepletionThermalLaw::Linear(coefficient),
                    linear,
                    c0 * (1. + coefficient.capacitance_per_k * dt),
                ),
                (
                    MosDepletionThermalLaw::PotentialPower {
                        potential_v_per_k: coefficient.potential_v_per_k,
                    },
                    linear,
                    c0 * (p / linear).powf(m),
                ),
                (
                    MosDepletionThermalLaw::SemiconductorTangent,
                    tangent,
                    c0 * (1. - 0.5 * dt * derivative / p),
                ),
            ] {
                let c = prepared.depletion(c0, SHAPE, law)?;
                for v in [-2., -0.3, 0., 0.25, 0.8] {
                    let (q, capacitance) = depletion(v, corrected_c, corrected_p, m);
                    let actual = c.evaluate(v)?;
                    near(actual.charge_c, q);
                    near(actual.capacitance_f, capacitance);
                }
            }
        }
    }
    Ok(())
}

const DIMENSIONS: MosDiffusionDimensions = MosDiffusionDimensions {
    width_m: 10e-6,
    width_multiplier: 0.8,
    width_offset_m: 1e-6,
    heavy_diffusion_m: 2e-6,
};
const SERIES: MosSeriesResistance = MosSeriesResistance {
    terminal_resistance: 90.,
    sheet_resistance_ohm_per_square: 45.,
    contact_resistance_ohm: 3.,
    squares: None,
    default_squares: 2.,
    light_path_m: 0.7e-6,
};

fn model(width_normalized: bool) -> MosLinearReverseModel {
    let density = |area, width| {
        if width_normalized {
            MosBottomDensity::PerMetre(width)
        } else {
            MosBottomDensity::PerSquareMetre(area)
        }
    };
    MosLinearReverseModel {
        saturation: MosJunctionSaturation {
            bottom: density(0.1, 2e-8),
            sidewall_a_per_m: 3e-8,
            fallback: MosSaturationFallback::ZeroCurrent(5e-14),
        },
        depletion: MosJunctionDepletion {
            bottom: density(0.001, 2e-10),
            bottom_shape: SHAPE,
            sidewall_f_per_m: 3e-10,
            sidewall_shape: SIDE,
            gate_edge_f_per_m: 4e-10,
            gate_edge_shape: SIDE,
            fallback_f: 6e-14,
        },
        depletion_temperature: [MosDepletionThermalLaw::Linear(
            MosDepletionTemperatureCoefficients {
                capacitance_per_k: 0.001,
                potential_v_per_k: 0.0007,
            },
        ); 3],
        nominal_temperature_k: TNOM,
        emission_coefficient: 1.2,
        saturation_temperature_exponent: 3.,
        bandgap: MosJunctionBandgap::SILICON,
        reverse_transition_v: -0.7,
        reverse_slope_divisor: 5.,
        transit_time_s: 4e-9,
        resistance_temperature_per_k: 0.002,
    }
}

#[test]
fn geometry_current_charge_and_resistance_are_composed_without_losing_presence() -> TestResult {
    let explicit = MosDiffusionTerminal {
        area_m2: Some(12e-12),
        perimeter_m: Some(4e-6),
        shared: false,
    };
    let omitted = MosDiffusionTerminal::default();
    let zero = MosDiffusionTerminal {
        area_m2: Some(0.),
        perimeter_m: Some(0.),
        shared: false,
    };
    let shared = MosDiffusionTerminal {
        shared: true,
        ..omitted
    };
    for (mode, terminal, resistance, bottom, perimeter, sidewall, gate, series) in [
        (
            MosLinearReverseGeometry::Explicit,
            explicit,
            MosSeriesResistanceRule::Explicit,
            7.68e-12,
            3.2e-6,
            3.2e-6,
            0.,
            93.,
        ),
        (
            MosLinearReverseGeometry::Explicit,
            omitted,
            MosSeriesResistanceRule::Explicit,
            0.,
            0.,
            0.,
            0.,
            93.,
        ),
        (
            MosLinearReverseGeometry::WidthNormalized,
            explicit,
            MosSeriesResistanceRule::WidthDependent,
            7.2e-6,
            9e-6,
            9e-6,
            0.,
            100.,
        ),
        (
            MosLinearReverseGeometry::WidthNormalized,
            zero,
            MosSeriesResistanceRule::DirectContact,
            7.2e-6,
            9e-6,
            9e-6,
            0.,
            183.,
        ),
        (
            MosLinearReverseGeometry::Isolated,
            omitted,
            MosSeriesResistanceRule::AutomaticSquares,
            28.8e-12,
            24.4e-6,
            15.4e-6,
            9e-6,
            18.,
        ),
        (
            MosLinearReverseGeometry::Isolated,
            zero,
            MosSeriesResistanceRule::AutomaticSquares,
            0.,
            0.,
            0.,
            0.,
            18.,
        ),
        (
            MosLinearReverseGeometry::Stacked,
            shared,
            MosSeriesResistanceRule::AutomaticSquares,
            14.4e-12,
            3.2e-6,
            3.2e-6,
            9e-6,
            18.,
        ),
        (
            MosLinearReverseGeometry::Stacked,
            omitted,
            MosSeriesResistanceRule::AutomaticSquares,
            28.8e-12,
            15.4e-6,
            15.4e-6,
            9e-6,
            18.,
        ),
    ] {
        let width_normalized = mode == MosLinearReverseGeometry::WidthNormalized;
        let m = model(width_normalized);
        let weighted_sat = if width_normalized {
            2e-8 * bottom
        } else {
            0.1 * bottom
        } + 3e-8 * perimeter;
        let nominal_sat = if weighted_sat > 0. {
            weighted_sat
        } else {
            5e-14
        };
        let bottom_cap = if width_normalized {
            2e-10 * bottom
        } else {
            0.001 * bottom
        };
        let side_cap = 3e-10 * sidewall + 4e-10 * gate;
        let bottom_cap = if bottom_cap + side_cap > 0. {
            bottom_cap
        } else {
            6e-14
        };
        for t in [260., TNOM, 360.] {
            let occurrence = MosJunctionOccurrence {
                dimensions: DIMENSIONS,
                terminal,
                gate_width_m: 0.1e-6,
                series: SERIES,
                operating_temperature_k: t,
            };
            let prepared = m.prepare(mode, resistance, occurrence)?;
            near(
                prepared.resistance_ohm(),
                series * (1. + 0.002 * (t - TNOM)),
            );
            let isat = saturation(t, 1.2, 3., nominal_sat, MosJunctionBandgap::SILICON);
            let vt = 1.2 * K * t;
            for v in [-2., -0.7, -0.2, 0., 0.25, 0.5] {
                let (i, g) = if v >= 0. {
                    (isat * (v / vt).exp_m1(), isat / vt * (v / vt).exp())
                } else if v >= -0.7 {
                    (isat * v, isat)
                } else {
                    (isat * -0.7 + isat / 5. * (v + 0.7), isat / 5.)
                };
                let dt = t - TNOM;
                let factor = 1. + 0.001 * dt;
                let (qb, cb) = depletion(
                    v,
                    bottom_cap * factor,
                    SHAPE.potential_v - 0.0007 * dt,
                    SHAPE.grading,
                );
                let (qs, cs) = depletion(
                    v,
                    side_cap * factor,
                    SIDE.potential_v - 0.0007 * dt,
                    SIDE.grading,
                );
                let point = prepared.evaluate(v)?;
                near(point.current_a, i);
                near(point.conductance_s, g);
                near(point.charge_c, qb + qs + 4e-9 * i);
                near(point.capacitance_f, cb + cs + 4e-9 * g);
                if v != -0.7 && v != 0. {
                    let step = 1e-6;
                    let a = prepared.evaluate(v - step)?;
                    let b = prepared.evaluate(v + step)?;
                    assert!(
                        ((b.current_a - a.current_a) / (2. * step) - g).abs()
                            <= 2e-8 * g.abs().max(1e-30)
                    );
                    assert!(
                        ((b.charge_c - a.charge_c) / (2. * step) - point.capacitance_f).abs()
                            <= 2e-8 * point.capacitance_f.abs().max(1e-30)
                    );
                }
            }
            assert_eq!(m.prepare(mode, resistance, occurrence)?, prepared);
        }
    }
    Ok(())
}
