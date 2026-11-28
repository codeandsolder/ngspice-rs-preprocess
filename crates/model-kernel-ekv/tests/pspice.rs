use model_kernel_api::{PointBuffer, Result};
use model_kernel_ekv::{
    Polarity,
    pspice::{CircuitDefaults, PspiceModel},
};

const DEFAULTS: CircuitDefaults = CircuitDefaults {
    length: 1e-6,
    width: 4e-6,
    drain_area: 2e-12,
    source_area: 3e-12,
    nominal_temperature: 300.15,
};

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= expected.abs().mul_add(1e-12, 1e-30),
        "{actual:e} != {expected:e}"
    );
}

#[test]
fn process_defaults_and_explicit_precedence() -> Result<()> {
    for polarity in [Polarity::N, Polarity::P] {
        let device = PspiceModel::compile(polarity, [], DEFAULTS)?
            .bind([])?
            .device;
        close(device.channel.cox, 0.7e-3);
        close(device.channel.kp, 50e-6);
        close(device.channel.phi, 0.7);
        close(device.channel.gamma, 1.);
        close(device.channel.mobility_field, 1e12);
        close(
            device.channel.threshold,
            if polarity == Polarity::N { 0.5 } else { -0.5 },
        );
        let model = PspiceModel::compile(
            polarity,
            [
                ("TOX", 10e-9),
                ("NSUB", 1e17),
                ("UO", 400.),
                ("VMAX", 1e5),
                ("VFB", -0.1),
                ("THETA", 0.1),
            ],
            DEFAULTS,
        )?;
        let channel = model.bind([])?.device.channel;
        let cox = 3.9 * 8.854_187_923_944_2e-12 / 10e-9;
        let gamma =
            (2. * 1.602_176_634e-19 * 11.7 * 8.854_187_923_944_2e-12 * 1e23_f64).sqrt() / cox;
        let phi = 2. * (1.380_648_8e-23 / 1.602_176_565e-19) * 300.15 * (1e23_f64 / 1.45e16).ln();
        close(channel.cox, cox);
        close(channel.gamma, gamma);
        close(channel.phi, phi);
        close(channel.kp, 0.04 * cox);
        close(channel.critical_field, 2.5e6);
        close(
            channel.threshold,
            if polarity == Polarity::N {
                -0.1 + phi + gamma * phi.sqrt()
            } else {
                -0.1 - phi - gamma * phi.sqrt()
            },
        );
        close(channel.mobility_field, 0.);
        let explicit = PspiceModel::compile(
            polarity,
            [
                ("TOX", 10e-9),
                ("COX", 1e-3),
                ("NSUB", 1e17),
                ("GAMMA", 0.4),
                ("PHI", 0.8),
                ("UO", 400.),
                ("KP", 90e-6),
                ("VMAX", 1e5),
                ("UCRIT", 3e6),
                ("VFB", -0.1),
                ("VTO", 0.2),
                ("THETA", 0.1),
                ("E0", 2e8),
            ],
            DEFAULTS,
        )?
        .bind([])?
        .device
        .channel;
        for (actual, expected) in [
            (explicit.cox, 1e-3),
            (explicit.gamma, 0.4),
            (explicit.phi, 0.8),
            (explicit.kp, 90e-6),
            (explicit.critical_field, 3e6),
            (explicit.threshold, 0.2),
            (explicit.mobility_field, 2e8),
        ] {
            close(actual, expected);
        }
    }
    Ok(())
}

#[test]
fn occurrence_geometry_and_parasitic_binding() -> Result<()> {
    let model = PspiceModel::compile(
        Polarity::N,
        [
            ("HDIF", 0.5e-6),
            ("RSH", 100.),
            ("RGSH", 20.),
            ("RDC", 3.),
            ("RSC", 4.),
            ("RD", 0.),
            ("RB", 11.),
            ("JS", 0.1),
            ("JSSW", 2e-8),
            ("IS", 8e-14),
            ("CJ", 2e-3),
            ("CJSW", 3e-10),
            ("CBD", 4e-14),
            ("N", 1.5),
            ("CGDO", 1e-10),
            ("RDS", 1e6),
            ("T_ABS", 50.),
        ],
        DEFAULTS,
    )?;
    let bound = model.bind([("np", 2.), ("ns", 3.), ("NRS", 0.25), ("t_rel_global", 10.)])?;
    assert_eq!(
        bound.slots().map(f64::to_bits),
        [1e-6_f64, 4e-6, 2., 3.].map(f64::to_bits)
    );
    let d = bound.device;
    for (a, e) in d.resistance.into_iter().zip([0., 40., 29., 11.]) {
        close(a, e);
    }
    for (a, e) in bound.diffusions.areas.into_iter().zip([2e-12, 4e-12]) {
        close(a, e);
    }
    close(
        d.junctions[0].saturation_current,
        0.1_f64.mul_add(2e-12, 2e-8 * 5e-6),
    );
    close(
        d.junctions[1].saturation_current,
        0.1_f64.mul_add(4e-12, 2e-8 * 10e-6),
    );
    close(d.junctions[0].bottom.capacitance, 4e-14);
    close(d.junctions[1].bottom.capacitance, 0.);
    close(d.junctions[1].sidewall.capacitance, 3e-15);
    close(d.junctions[0].emission, 1.5);
    close(d.temperature.resolve(300.)?, 310.);
    let zero = model.bind([("ad", 0.)])?;
    close(zero.device.junctions[0].saturation_current, 8e-14);
    close(zero.device.junctions[1].saturation_current, 8e-14);
    let artifact = bound.artifact([1; 16])?;
    assert!(artifact.prepare(&[2e-6, 4e-6, 2., 3.], 300.).is_err());
    let prepared = artifact.prepare(&bound.slots(), 300.)?;
    let mut buffer = PointBuffer::new(artifact.descriptor().equations.len())?;
    artifact.evaluate(
        &prepared,
        &[1., 0.7, 0., 0., 0.65, 0., 0.],
        &mut buffer.point(),
    )?;
    Ok(())
}

#[test]
fn unsupported_physics_and_operating_point_flags() -> Result<()> {
    for parameters in [
        [("level", 4.)],
        [("nqs", 1.)],
        [("xqc", 1.)],
        [("avto", 0.1)],
        [("nlev", 5.)],
        [("unknown", 1.)],
        [("rds", 0.)],
        [("is", -1.)],
    ] {
        assert!(
            PspiceModel::compile(Polarity::N, parameters, DEFAULTS).is_err(),
            "{parameters:?}"
        );
    }
    let low = PspiceModel::compile(Polarity::N, [("satlim", 1.1)], DEFAULTS)?.bind([])?;
    let high = PspiceModel::compile(Polarity::N, [("satlim", 1e100)], DEFAULTS)?.bind([])?;
    let bias = [1., 0.8, 0., 0.];
    assert!(low.saturated(bias, 300.15)?);
    assert!(!high.saturated(bias, 300.15)?);
    let a = low
        .device
        .channel
        .prepare_segmented(low.geometry, 300.15)?
        .evaluate(bias)?;
    let b = high
        .device
        .channel
        .prepare_segmented(high.geometry, 300.15)?
        .evaluate(bias)?;
    assert_eq!(a.current.map(f64::to_bits), b.current.map(f64::to_bits));
    Ok(())
}
