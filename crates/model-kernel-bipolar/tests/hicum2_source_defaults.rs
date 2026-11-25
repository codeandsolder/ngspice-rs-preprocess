use model_kernel_bipolar::{
    Temperature,
    hicum2::{self, temperature},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let limit = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= limit,
        "actual={actual:.15e},expected={expected:.15e},tolerance={limit:.5e}"
    );
}

#[test]
fn published_hicum24_default_model_prepares_all_physical_subfamilies() -> TestResult {
    let model = temperature::Nominal::source_defaults();
    let via_default = temperature::Nominal::default();
    assert_eq!(model.polarity, hicum2::Polarity::Npn);
    assert_eq!(via_default.transport.flcomp, model.transport.flcomp);
    assert_eq!(model.transport.flcomp, 0.0);
    assert_eq!(model.transport.c10, 2e-30);
    assert_eq!(model.transport.qp0, 2e-14);
    assert_eq!(model.transport.t0, 0.0);
    assert_eq!(model.auxiliary.fgeo, 0.6557);
    assert_eq!(model.auxiliary.fbepar, 1.0);
    assert_eq!(model.auxiliary.tunode, 1);
    assert_eq!(model.auxiliary.flsh, 0);
    assert!(!model.auxiliary.flnqs);
    assert_eq!(model.noise.alit, 0.333);
    assert_eq!(model.noise.alqf, 0.167);
    assert_eq!(model.coefficients.tnom_celsius, 27.0);
    assert_eq!(model.diodes.ibeis, 1e-18);
    assert_eq!(model.diodes.ibcis, 1e-16);
    for ambient in [173.15, 233.15, 300.15, 373.15, 600.0] {
        let prepared = model.prepare(Temperature::new(ambient)?, 0.0)?;
        assert_eq!(prepared.polarity, hicum2::Polarity::Npn);
        assert_eq!(prepared.effective_temperature.kelvin(), ambient);
        assert!(
            prepared
                .thermal_kernel()
                .evaluate(hicum2::thermal::ElectricalBias::default(),)?
                .shorted
        );
        assert!(!prepared.nqs_kernel()?.is_active(0.0));
        let ext = prepared.external_kernel();
        let split = ext.split();
        close(split.external_collector_junction, 1e-20, 1e-14, 0.0);
        assert_eq!(split.peripheral_collector_junction, 0.0);
        assert_eq!(split.external_collector_isolation, 0.0);
        assert_eq!(split.peripheral_collector_isolation, 0.0);
    }
    let mut pnp = model;
    pnp.polarity = hicum2::Polarity::Pnp;
    let prepared = pnp.prepare(Temperature::new(300.15)?, 0.0)?;
    assert_eq!(prepared.polarity, hicum2::Polarity::Pnp);
    let bias = hicum2::transport::Bias {
        vbiei: 0.75,
        vbici: -0.55,
    };
    close(
        prepared
            .transport_kernel()?
            .evaluate(bias)?
            .net_transfer_current
            .value,
        model
            .prepare(Temperature::new(300.15)?, 0.0)?
            .transport_kernel()?
            .evaluate(bias)?
            .net_transfer_current
            .value,
        0.0,
        0.0,
    );
    Ok(())
}

#[test]
fn native_ngspice47_unmodified_level8_model_card_reference() -> TestResult {
    let fixture = include_str!("fixtures/hicum2_ngspice47_source_defaults.csv");
    let model = temperature::Nominal::source_defaults();
    let mut count = 0;
    let mut worst = [0.0f64; 4];
    for row in fixture.lines().filter(|line| !line.starts_with('#')) {
        let [
            ambient,
            _vbe,
            _vce,
            emitter_voltage,
            collector_voltage,
            ic,
            base_emitter_current,
            base_collector_current,
            ick,
            tf,
        ] = <[&str; 10]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "malformed native HICUM source-default reference row")?;
        let temperature = Temperature::new(ambient.parse::<f64>()? + 273.15)?;
        let prepared = model.prepare(temperature, 0.0)?;
        let be: f64 = emitter_voltage.parse()?;
        let bc: f64 = collector_voltage.parse()?;
        let transfer = prepared
            .transport_kernel()?
            .evaluate(hicum2::transport::Bias {
                vbiei: be,
                vbici: bc,
            })?;
        let diode = prepared.diode_kernel()?.evaluate(hicum2::Bias {
            vbiei: be,
            vbpei: be,
            vbici: bc,
            vbpci: bc,
            vsici: -1.0,
        })?;
        let native = [
            ic.parse::<f64>()?,
            base_emitter_current.parse::<f64>()?,
            base_collector_current.parse::<f64>()?,
            ick.parse::<f64>()?,
        ];
        // Native @q1[ic] reports the terminal collector current rather
        // than HICCR's intrinsic Itf-Itr. Its branch reporting includes
        // collectorward B-C diode current and three native Gmin branch
        // contributions. Source Gmin 1e-12 A/V is pinned in the deck.
        // With VCE=Vbiei-Vbici, its local report is:
        // It - Ibci + Gmin*(Vbiei-3*Vbici).
        let terminal_collector = (1e-12f64).mul_add(
            3.0f64.mul_add(-bc, be),
            transfer.net_transfer_current.value - diode.ibci.amperes,
        );
        let actual = [
            terminal_collector,
            diode.ibei.amperes,
            1e-12f64.mul_add(bc, diode.ibci.amperes),
            transfer.critical_current.value,
        ];
        for idx in 0..4 {
            let mismatch = (actual[idx] - native[idx]).abs()
                / actual[idx].abs().max(native[idx].abs()).max(1e-25);
            worst[idx] = worst[idx].max(mismatch);
            assert!(
                mismatch < 1e-4,
                "native source-default mismatch ambient={ambient} B-E={be} B-C={bc} channel={idx} actual={} native={} relative={mismatch}",
                actual[idx],
                native[idx],
            );
        }
        assert_eq!(transfer.forward_transit_time, tf.parse::<f64>()?);
        count += 1;
    }
    assert_eq!(count, 36);
    assert!(
        worst.iter().all(|error| *error < 1e-4),
        "source-default maximum errors: {worst:?}"
    );
    Ok(())
}
