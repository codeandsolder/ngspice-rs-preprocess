use ParameterValue::{Flag, Integer, Real, RealVector};
use model_compact_intrinsic::{
    BoundParameters, BsimSoiBodyMode, BsimSoiInstanceInput, BsimSoiNode, ModelError, ModelVariant,
    ParameterScope, ParameterValue, PreparedBsimSoiModel, PreparedBsimSoiSetup,
    prepare_bsim_soi_model, prepare_bsim_soi_setup,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn model(values: &[(&str, ParameterValue)]) -> Result<PreparedBsimSoiModel, ModelError> {
    let bound = BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Model,
        values.iter().cloned(),
    )?;
    prepare_bsim_soi_model(&bound, 300.15)
}

fn instance(values: &[(&str, ParameterValue)]) -> Result<BoundParameters, ModelError> {
    BoundParameters::bind(
        ModelVariant::BsimSoi,
        ParameterScope::Instance,
        values.iter().cloned(),
    )
}

fn setup(
    model: &PreparedBsimSoiModel,
    instance: &BoundParameters,
    count: u8,
) -> Result<PreparedBsimSoiSetup, ModelError> {
    prepare_bsim_soi_setup(
        model,
        &[BsimSoiInstanceInput {
            parameters: instance,
            terminal_count: count,
            geometry_scale: 1.,
        }],
    )
}

fn slot(scope: ParameterScope, name: &str) -> Result<u16, ModelError> {
    ModelVariant::BsimSoi
        .parameter(scope, name)
        .map(|spec| spec.slot)
        .ok_or_else(|| ModelError::UnknownParameter(name.to_owned()))
}

fn real_value(params: &BoundParameters, name: &str) -> Result<f64, ModelError> {
    let id = slot(params.scope(), name)?;
    match params.get_slot(id) {
        Some(Real(value)) => Ok(*value),
        _ => Err(ModelError::MissingDefaultDependency(id)),
    }
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= expected.abs().max(1e-30) * 1e-12,
        "actual={actual:e}, expected={expected:e}"
    );
}

#[test]
fn source_defaults_and_ordered_ic() -> TestResult {
    let model = model(&[
        ("rth0", Real(0.012)),
        ("cth0", Real(3e-7)),
        ("frbody", Real(1.4)),
    ])?;
    let instance = instance(&[
        ("agbcp", Real(4e-12)),
        ("ic", RealVector(vec![0.4])),
        ("ic", RealVector(vec![1., 2., 3., 4., 5.])),
        ("ic", RealVector(vec![0.8, 0.9])),
    ])?;
    let setup = setup(&model, &instance, 4)?;
    let prepared = &setup.instances()[0];
    assert_eq!(
        prepared.initial_conditions(),
        [Some(0.4), Some(0.9), None, None, Some(5.)]
    );
    let p = prepared.parameters();
    for (name, expected) in [
        ("l", 5e-6),
        ("w", 5e-6),
        ("m", 1.),
        ("nf", 1.),
        ("nseg", 1.),
        ("nrd", 1.),
        ("nrs", 1.),
        ("agbcpd", 4e-12),
        ("rth0", 0.012),
        ("cth0", 3e-7),
        ("frbody", 1.4),
    ] {
        close(real_value(p, name)?, expected);
    }
    assert!(prepared.was_given(slot(ParameterScope::Instance, "agbcp")?));
    assert!(!prepared.was_given(slot(ParameterScope::Instance, "agbcpd")?));
    assert!(matches!(
        BoundParameters::bind(
            ModelVariant::BsimSoi,
            ParameterScope::Instance,
            [("ic", RealVector(vec![]))]
        ),
        Err(ModelError::InvalidInitialConditions(0))
    ));
    assert!(matches!(
        BoundParameters::bind(
            ModelVariant::BsimSoi,
            ParameterScope::Instance,
            [("ic", RealVector(vec![0.; 6]))]
        ),
        Err(ModelError::InvalidInitialConditions(6))
    ));
    Ok(())
}

#[test]
fn positional_topology_and_model_mutation() -> TestResult {
    use BsimSoiNode::{
        Body, DrainBody, DrainPrime, External, Gate, GateMid, Ground, SourceBody, SourcePrime,
        Temperature,
    };
    let model = model(&[
        ("soimod", Integer(0)),
        ("shmod", Integer(1)),
        ("rth0", Real(1e-2)),
        ("rsh", Real(10.)),
        ("rgatemod", Integer(3)),
        ("rbodymod", Integer(1)),
    ])?;
    let raw = instance(&[])?;
    let thermal_flag = instance(&[("tnodeout", Flag(true))])?;
    for (count, input, contact, body, temperature, mode, floating) in [
        (
            4,
            &raw,
            Ground,
            Body,
            Temperature,
            BsimSoiBodyMode::NoContact,
            true,
        ),
        (
            5,
            &raw,
            External(4),
            External(4),
            Temperature,
            BsimSoiBodyMode::Ideal,
            false,
        ),
        (
            5,
            &thermal_flag,
            Ground,
            Body,
            External(4),
            BsimSoiBodyMode::NoContact,
            true,
        ),
        (
            6,
            &thermal_flag,
            External(4),
            External(4),
            External(5),
            BsimSoiBodyMode::Ideal,
            false,
        ),
        (
            6,
            &raw,
            External(4),
            External(5),
            Temperature,
            BsimSoiBodyMode::Resistive,
            false,
        ),
        (
            7,
            &raw,
            External(4),
            External(5),
            External(6),
            BsimSoiBodyMode::Resistive,
            false,
        ),
        (
            7,
            &thermal_flag,
            External(4),
            External(5),
            External(6),
            BsimSoiBodyMode::Resistive,
            false,
        ),
    ] {
        let setup = setup(&model, input, count)?;
        let topology = setup.instances()[0].topology();
        assert_eq!(
            (
                topology.contact,
                topology.body,
                topology.temperature,
                topology.body_mode,
                topology.floating_body
            ),
            (contact, body, temperature, mode, floating)
        );
        assert_eq!(
            (
                topology.drain_prime,
                topology.source_prime,
                topology.gate,
                topology.gate_mid,
                topology.drain_body,
                topology.source_body
            ),
            (
                DrainPrime,
                SourcePrime,
                Gate,
                GateMid,
                DrainBody,
                SourceBody
            )
        );
    }
    let group = prepare_bsim_soi_setup(
        &model,
        &[5, 6, 5].map(|terminal_count| BsimSoiInstanceInput {
            parameters: &raw,
            terminal_count,
            geometry_scale: 1.,
        }),
    )?;
    assert_eq!(
        group
            .instances()
            .iter()
            .map(|p| p.topology().body_mode)
            .collect::<Vec<_>>(),
        [
            BsimSoiBodyMode::Ideal,
            BsimSoiBodyMode::Resistive,
            BsimSoiBodyMode::Resistive
        ]
    );
    close(real_value(group.model().parameters(), "rbody")?, 1.);
    close(real_value(model.parameters(), "rbody")?, 0.);
    assert!(setup(&model, &raw, 8).is_err());
    Ok(())
}

#[test]
fn automatic_depletion_and_fd_doping() -> TestResult {
    let raw = instance(&[
        ("soimod", Integer(3)),
        ("rbodymod", Integer(1)),
        ("tnodeout", Flag(true)),
    ])?;
    let baseline = model(&[("nch", Real(1e17))])?;
    let vbs = setup(&baseline, &raw, 5)?.instances()[0].vbs0t();
    for (lower, upper, expected) in [
        (vbs + 1., vbs + 2., 0),
        (vbs - 1., vbs + 1., 1),
        (vbs - 2., vbs - 1., 2),
    ] {
        let m = model(&[
            ("nch", Real(1e17)),
            ("vbs0pd", Real(lower)),
            ("vbs0fd", Real(upper)),
            ("shmod", Integer(1)),
            ("rth0", Real(1e-2)),
        ])?;
        let p = setup(&m, &raw, 5)?;
        let i = &p.instances()[0];
        assert_eq!(
            i.parameters()
                .get_slot(slot(ParameterScope::Instance, "soimod")?),
            Some(&Integer(expected))
        );
        if expected == 2 {
            assert_eq!(i.topology().body, BsimSoiNode::Ground);
            assert_eq!(i.topology().temperature, BsimSoiNode::Temperature);
            assert_eq!(
                i.parameters()
                    .get_slot(slot(ParameterScope::Instance, "rbodymod")?),
                Some(&Integer(0))
            );
        }
    }
    let fd = model(&[
        ("soimod", Integer(2)),
        ("nch", Real(1e19)),
        ("tsi", Real(1e-7)),
    ])?;
    let default = instance(&[])?;
    let p = setup(&fd, &default, 4)?;
    close(
        real_value(p.model().parameters(), "nch")?,
        (1.12 - 0.1) / 1.60219e-19 * 2e-6 * 1.03594e-10 / 1e-14,
    );
    close(real_value(fd.parameters(), "nch")?, 1e19);
    Ok(())
}

#[test]
fn geometry_scale_binning_and_candidate_isolation() -> TestResult {
    let raw = instance(&[
        ("l", Real(0.5e-6)),
        ("w", Real(4e-6)),
        ("nf", Real(2.)),
        ("ad", Real(1e-12)),
        ("pd", Real(2e-6)),
        ("nseg", Real(2.)),
        ("nbc", Real(1.)),
        ("pdbcp", Real(2e-8)),
    ])?;
    for binunit in [0, 1] {
        let model = model(&[
            ("binunit", Integer(binunit)),
            ("lint", Real(10e-9)),
            ("wint", Real(20e-9)),
            ("dlc", Real(12e-9)),
            ("dwc", Real(22e-9)),
            ("dwbc", Real(30e-9)),
            ("vth0", Real(0.4)),
            ("lvth0", Real(0.001)),
            ("wvth0", Real(0.002)),
            ("pvth0", Real(0.003)),
        ])?;
        let group = prepare_bsim_soi_setup(
            &model,
            &[1., 2.].map(|geometry_scale| BsimSoiInstanceInput {
                parameters: &raw,
                terminal_count: 4,
                geometry_scale,
            }),
        )?;
        for (index, scale) in [1., 2.].into_iter().enumerate() {
            let p = group.instances()[index].parameters();
            close(real_value(p, "ad")?, 1e-12 * scale * scale);
            close(real_value(p, "pd")?, 2e-6 * scale);
            let size = &group.sizes()[index];
            let g = size.geometry();
            close(g.leff, 0.5e-6 * scale - 20e-9);
            close(g.weff, 2e-6 * scale - 50e-9);
            close(g.leff_cv, 0.5e-6 * scale - 24e-9);
            close(g.weff_cv, 2e-6 * scale - 52e-9);
            close(g.drain_diode_width, g.weff / 2. + 2e-8);
            let unit = if binunit == 1 { 1e-6 } else { 1. };
            let expected = 0.4
                + 0.001 * unit / g.leff
                + 0.002 * unit / g.weff
                + 0.003 * unit * unit / (g.leff * g.weff);
            close(
                size.binned_model_slot(slot(ParameterScope::Model, "vth0")?)
                    .ok_or("missing vth0")?,
                expected,
            );
            assert_eq!(
                ModelVariant::BsimSoi
                    .parameters(ParameterScope::Model)
                    .iter()
                    .map(|p| p.slot)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|s| size.binned_model_slot(*s).is_some())
                    .count(),
                179
            );
        }
        close(real_value(&raw, "l")?, 0.5e-6);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires local ngspice 47 binary"]
fn native_setup_reference() -> TestResult {
    use std::process::Command;
    let model_args = [
        ("rth0", Real(0.005)),
        ("cth0", Real(1e-6)),
        ("soimod", Integer(3)),
        ("rbody", Real(0.)),
        ("rbsh", Real(0.)),
        ("shmod", Integer(1)),
        ("rsh", Real(100.)),
        ("rshg", Real(20.)),
        ("rgatemod", Integer(3)),
        ("rbodymod", Integer(1)),
    ];
    let model = model(&model_args)?;
    let raw_args = [
        ("l", Real(0.5e-6)),
        ("w", Real(4e-6)),
        ("nf", Real(2.)),
        ("nseg", Real(2.)),
        ("nrd", Real(0.)),
        ("nrs", Real(1.)),
        ("agbcp", Real(1e-12)),
        ("pd", Real(4e-6)),
        ("ps", Real(4e-6)),
        ("tnodeout", Flag(true)),
    ];
    let raw = instance(&raw_args)?;
    let setup = setup(&model, &raw, 6)?;
    let deck = "BSIMSOI setup reference\n.model soi nmos level=10 rth0=0.005 cth0=1e-6 soimod=3 rbody=0 rbsh=0 shmod=1 rsh=100 rshg=20 rgatemod=3 rbodymod=1\nVd d 0 0.6\nVg g 0 1.2\nVe e 0 0\nVb b 0 0\nVt t 0 0\nM1 d g 0 e b t soi l=0.5u w=4u nf=2 nseg=2 tnodeout nrd=0 nrs=1 agbcp=1e-12 pd=4u ps=4u\n.control\nop\nshow m1\nprint all\nquit\n.endc\n.end\n";
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("setup.cir"), deck)?;
    let output = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "setup.cir"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("B4SOI: Berkeley SOI MOSFET model version 4.4.0"));
    let params = setup.instances()[0].parameters();
    let mut checked = 0;
    for line in stdout.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        let Some(value) = fields.next().and_then(|field| field.parse::<f64>().ok()) else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        let Some(spec) = ModelVariant::BsimSoi.parameter(ParameterScope::Instance, name) else {
            continue;
        };
        let expected = match params.get_slot(spec.slot) {
            Some(Real(value)) => *value,
            Some(Integer(value)) => f64::from(*value),
            Some(Flag(value)) => f64::from(u8::from(*value)),
            _ => continue,
        };
        assert!(
            (expected - value).abs() <= expected.abs().max(1e-30) * 1e-5,
            "{name}: Rust {expected:e}, ngspice {value:e}"
        );
        checked += 1;
    }
    assert!(checked >= 30, "checked {checked} instance parameters");
    for node in ["m1#gate", "m1#midgate", "m1#dbody", "m1#sbody"] {
        assert!(stdout.contains(node), "missing {node}");
    }
    assert!(!stdout.contains("m1#drain ="));
    eprintln!("Validated {checked} native BSIMSOI instance parameters and RF/body topology");
    Ok(())
}
