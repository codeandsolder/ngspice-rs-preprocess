use super::{Connection, Device, Gate, Primitive, Role, Storage, timing};
use crate::{lexical, model_card::parse_model_card};
use Connection::{GlobalGround, GlobalSupply, Node};
use anyhow::{Context, Result, bail};
use model_preprocess_command::WorkBudget;

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1e-14 * actual.abs().max(expected.abs()).max(1e-20),
        "{actual:e} != {expected:e}"
    );
}

#[test]
fn gate_arrays_keep_gate_major_input_order_and_shared_enable() -> Result<()> {
    let source = "Ulogic NAND3A(2, 3) $G_DPWR $G_DGND a b c d e f en q r s tm io";
    let device = Device::parse(source, &WorkBudget::default())?;
    assert_eq!(
        device.primitive,
        Primitive::Gate {
            function: Gate::Nand,
            inputs_per_gate: 2,
            gates: 3,
            tristate: true,
        }
    );
    let groups = device
        .pins
        .iter()
        .map(|p| (p.role, p.connections.as_slice()))
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec![
            (Role::Supply, &[GlobalSupply][..]),
            (Role::Ground, &[GlobalGround][..]),
            (
                Role::Data,
                &[
                    Node("a"),
                    Node("b"),
                    Node("c"),
                    Node("d"),
                    Node("e"),
                    Node("f")
                ][..]
            ),
            (Role::OutputEnable, &[Node("en")][..]),
            (Role::Output, &[Node("q"), Node("r"), Node("s")][..]),
        ]
    );
    assert_eq!(device.timing_model, "tm");
    assert_eq!(device.io_model, "io");
    Ok(())
}

#[test]
fn compound_gate_has_one_output_and_distinct_inner_groups() -> Result<()> {
    for (name, function) in [
        ("AO", Gate::AndOr),
        ("AOI", Gate::AndOrInvert),
        ("OA", Gate::OrAnd),
        ("OAI", Gate::OrAndInvert),
    ] {
        let source = format!("Ucompound {name}(2,3) p g a b c d e f q tm io");
        let device = Device::parse(&source, &WorkBudget::default())?;
        assert_eq!(
            device.primitive,
            Primitive::Gate {
                function,
                inputs_per_gate: 2,
                gates: 3,
                tristate: false
            }
        );
        assert_eq!(
            device.pins.last().context("output group")?.connections,
            [Connection::Node("q")]
        );
    }
    Ok(())
}

#[test]
fn sequential_pins_and_distinct_edge_enables() -> Result<()> {
    let device = Device::parse(
        "Uj JKFFDE(2)$G_DPWR $G_DGND $D_HI clear clk pos neg j0 j1 k0 k1 q0 q1 qb0 $D_NC edge io",
        &WorkBudget::default(),
    )?;
    assert_eq!(
        device.primitive,
        Primitive::Storage {
            function: Storage::DualEdgeJk,
            lanes: 2
        }
    );
    assert_eq!(
        device.pins.iter().map(|p| p.role).collect::<Vec<_>>(),
        [
            Role::Supply,
            Role::Ground,
            Role::PresetBar,
            Role::ClearBar,
            Role::Clock,
            Role::PositiveEdgeEnable,
            Role::NegativeEdgeEnable,
            Role::Data,
            Role::SecondData,
            Role::Output,
            Role::ComplementOutput,
        ]
    );
    assert_eq!(device.pins[2].connections, [Connection::High]);
    assert_eq!(
        device.pins[10].connections,
        [Connection::Node("qb0"), Connection::Unconnected]
    );

    let latch = Device::parse(
        "Us SRFF(2) p g pbar cbar gate s0 s1 r0 r1 q0 q1 qb0 qb1 tm io",
        &WorkBudget::default(),
    )?;
    assert_eq!(latch.primitive.timing_family(), timing::Family::Latch);
    assert_eq!(latch.pins[4].role, Role::Gate);
    assert_eq!(latch.pins[6].role, Role::SecondData);
    Ok(())
}

#[test]
fn reserved_dollar_tokens_survive_only_digital_source_lexing() -> Result<()> {
    let input = b"Ufoo AND(2) $G_DPWR $G_DGND\n* comment\n+ $D_HI in out tm io ; trailing comment\nR1 a b 1k $ trailing native comment\n";
    let statements = lexical::pspice_statements(input)?;
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].line, 1);
    let device = Device::parse(
        std::str::from_utf8(&statements[0].text)?,
        &WorkBudget::default(),
    )?;
    assert_eq!(
        device.pins[2].connections,
        [Connection::High, Connection::Node("in")]
    );
    assert_eq!(&statements[1].text, b"R1 a b 1k");
    assert_eq!(&lexical::statements(input)?[0].text, b"Ufoo AND(2)");
    Ok(())
}

#[test]
fn policy_is_bound_per_occurrence() -> Result<()> {
    let budget = WorkBudget::default();
    let source = "U1 BUF p g a b tm io MNTYMXDLY={delay_corner} IO_LEVEL={io_level}";
    let device = Device::parse(source, &budget)?;
    let policy = device.bind_policy(
        timing::Policy::default(),
        &mut |name: &str| match name {
            "delay_corner" => Ok(4.0),
            "io_level" => Ok(3.0),
            _ => bail!("unknown parameter {name}"),
        },
        &budget,
    )?;
    assert_eq!(policy.corner, timing::Corner::WorstCase);
    assert_eq!(policy.io_level, 3);
    let defaults = timing::Policy {
        corner: timing::Corner::Minimum,
        io_level: 2,
        ..timing::Policy::default()
    };
    let inherited = device.bind_policy(defaults, &mut |_: &str| Ok(0.0), &budget)?;
    assert_eq!(inherited.corner, timing::Corner::Minimum);
    assert_eq!(inherited.io_level, 2);
    assert!(
        device
            .bind_policy(defaults, &mut |_: &str| Ok(2.5), &budget)
            .is_err()
    );
    Ok(())
}

#[test]
fn missing_delay_and_constraint_corners_follow_distinct_rules() -> Result<()> {
    // Reference Guide pp. 339-340: all eight presence combinations.
    // These expected numbers are independently computed from the documented
    // extrapolation/interpolation equations using non-default scale factors.
    let scale = timing::Scale {
        minimum: 0.5,
        maximum: 2.0,
    };
    for (min, typ, max, delay, check) in [
        (None, None, None, [0., 0., 0.], [0., 0., 0.]),
        (Some(2.), None, None, [2., 4., 8.], [2., 2., 2.]),
        (None, Some(4.), None, [2., 4., 8.], [0., 4., 4.]),
        (None, None, Some(8.), [2., 4., 8.], [0., 4., 8.]),
        (Some(2.), Some(4.), None, [2., 4., 8.], [2., 4., 4.]),
        (Some(2.), None, Some(8.), [2., 5., 8.], [2., 5., 8.]),
        (None, Some(4.), Some(8.), [2., 4., 8.], [0., 4., 8.]),
        (Some(2.), Some(4.), Some(8.), [2., 4., 8.], [2., 4., 8.]),
    ] {
        let triplet = timing::Triplet {
            minimum: min,
            typical: typ,
            maximum: max,
        };
        let propagation = triplet.propagation(scale)?;
        let constraint = triplet.constraint()?;
        for (resolved, expected) in [(propagation, delay), (constraint, check)] {
            close(resolved.minimum, expected[0]);
            close(resolved.typical, expected[1]);
            close(resolved.maximum, expected[2]);
            let window = resolved.select(timing::Corner::WorstCase);
            close(window.earliest, expected[0]);
            close(window.latest, expected[2]);
        }
    }
    let explicit = timing::Triplet {
        minimum: Some(0.),
        typical: Some(4.),
        maximum: None,
    }
    .propagation(scale)?;
    close(explicit.minimum, 0.);
    close(explicit.maximum, 8.);
    Ok(())
}

#[test]
fn directional_timing_and_checks_resolve_in_model_context() -> Result<()> {
    let budget = WorkBudget::default();
    let card = parse_model_card(
        ".model edge UEFF(TPCLKQLHTY={t} TPCLKQHLTY={2*t} TPPCQLHMN=1n TSUDCLKMN=3n THDCLKMX=6n THCECLKTY=2n)",
        1,
        &[],
    )?;
    let model = timing::Model::parse(&card, &budget)?;
    let bound = model.bind(
        timing::Scale::default(),
        &mut |name: &str| {
            if name == "t" {
                Ok(5e-9)
            } else {
                bail!("undefined {name}")
            }
        },
        &budget,
    )?;
    close(bound.propagation["tpclkqlh"].typical, 5e-9);
    close(bound.propagation["tpclkqhl"].typical, 10e-9);
    close(bound.propagation["tppcqlh"].typical, 2.5e-9);
    close(bound.propagation["tppcqhl"].maximum, 0.);
    close(bound.constraints["tsudclk"].maximum, 3e-9);
    close(bound.constraints["thdclk"].typical, 3e-9);
    close(bound.constraints["thceclk"].minimum, 0.);
    Ok(())
}

#[test]
fn reject_invalid_dimensions_and_unknown_semantics() -> Result<()> {
    let budget = WorkBudget::default();
    for source in [
        "U1 AND(0) p g a b tm io",
        "U1 BUF(2) p g a b tm io",
        "U1 AND(2) p g a b tm io",
        "U1 ANDA(18446744073709551615,2) p g a b tm io",
        "U1 INV p g a b tm io strange=3",
        "U1 PULLUP(1) p g a io",
    ] {
        assert!(Device::parse(source, &budget).is_err(), "{source}");
    }
    let card = parse_model_card(".model invalid UGATE(TSUDCLKMN=1n)", 1, &[])?;
    assert!(timing::Model::parse(&card, &budget).is_err());
    assert!(
        timing::Triplet {
            typical: Some(-1.),
            ..timing::Triplet::default()
        }
        .propagation(timing::Scale::default())
        .is_err()
    );
    Ok(())
}

#[test]
fn diagnostic_uses_lexical_timing_model_not_an_unrelated_root_model() -> Result<()> {
    let input = b"Title\n.model t UGATE(TPLHTY=1n)\n.subckt demo in out\n.model t UDLY(DLYTY=1n)\nU1 BUF $G_DPWR $G_DGND in out t io\n.ends\n.end\n";
    let result = super::super::normalize_mapped(
        input,
        crate::linked_source::RootKind::Deck,
        &WorkBudget::default(),
    );
    let error = result.err().context("expected wrong-family diagnostic")?;
    assert!(format!("{error:#}").contains("wrong family"), "{error:#}");
    Ok(())
}

#[test]
fn interface_models_preserve_load_and_level_specific_transition_offsets() -> Result<()> {
    let budget = WorkBudget::default();
    let card = parse_model_card(
        ".model io UIO (DRVH={R} DRVL=12 DRVZ=5meg INR=1meg INLD=10p OUTLD=2p AtoD3=\"Schmitt\" DtoA3=\"OpenCollector\" DIGPOWER=\"Supply\" TSWHL3=3n TSWLH3=1n TPWRT=0)",
        1,
        &[],
    )?;
    let model = super::io::Model::parse(&card, &budget)?;
    let mut context = |name: &str| {
        if name.eq_ignore_ascii_case("R") {
            Ok(24.0)
        } else {
            bail!("unknown {name}")
        }
    };
    let bound = model.bind(3, &mut context, &budget)?;
    close(bound.drive_high, 24.0);
    close(bound.drive_low, 12.0);
    close(bound.drive_z, 5e6);
    close(bound.input_resistance, 1e6);
    close(bound.input_capacitance, 10e-12);
    close(bound.output_capacitance, 2e-12);
    assert_eq!(bound.analog_to_digital, "Schmitt");
    assert_eq!(bound.digital_to_analog, "OpenCollector");
    assert_eq!(bound.power_subcircuit, "Supply");
    close(bound.pulse_rejection.context("explicit zero")?, 0.0);
    close(bound.interface_delay(2e-9, true, true), 1e-9);
    close(bound.interface_delay(2e-9, false, true), 0.0);
    close(bound.interface_delay(2e-9, false, false), 2e-9);
    let default = super::io::Model::default();
    let bound = default.bind(1, &mut context, &budget)?;
    close(bound.drive_high, 50.0);
    close(bound.drive_low, 50.0);
    close(bound.drive_z, 250e3);
    close(bound.input_resistance, 30e3);
    close(bound.minimum_storage, 1e-3);
    assert_eq!(bound.pulse_rejection, None);
    assert_eq!(bound.analog_to_digital, "AtoDDefault");
    assert_eq!(bound.digital_to_analog, "DtoADefault");
    assert_eq!(bound.power_subcircuit, "DIGIFPWR");
    Ok(())
}

#[test]
fn circuit_policy_uses_final_global_values_and_ignores_control_commands() -> Result<()> {
    let source = b".options DIGMNTYMX=1 DIGMNTYSCALE=.25 DIGTYMXSCALE=2 DIGIOLVL=3 DIGINITSTATE=1 DIGFREQ=5g DIGDRVF=3 DIGDRVZ=10k DIGOVRDRV=4\n.control\n.options DIGMNTYMX=3\n.endc\n.options DIGMNTYMX=4\n";
    let statements = lexical::pspice_statements(source)?;
    let policy = super::super::options::GlobalOptions::collect(&statements)?.digital;
    assert_eq!(policy.corner, timing::Corner::WorstCase);
    assert_eq!(policy.io_level, 3);
    assert_eq!(policy.initial_state, timing::InitialState::Set);
    close(policy.scale.minimum, 0.25);
    close(policy.scale.maximum, 2.0);
    close(policy.digital_frequency, 5e9);
    close(policy.minimum_drive_resistance, 3.0);
    close(policy.maximum_drive_resistance, 10e3);
    close(policy.override_drive_ratio, 4.0);
    let invalid = lexical::pspice_statements(b".options DIGIOLVL=0\n")?;
    assert!(super::super::options::GlobalOptions::collect(&invalid).is_err());
    Ok(())
}
