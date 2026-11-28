use model_kernel_bipolar::{ModelError, Temperature, vbic};
use vbic::temperature::{Coefficient as C, Nominal};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOMINAL_VALUES: [f64; 108] = {
    let mut p = [0.0; 108];
    p[C::Tnom as usize] = 27.0;
    p[C::Rcx as usize] = 2.5;
    p[C::Rci as usize] = 5.0;
    p[C::Vo as usize] = 0.5;
    p[C::Gamm as usize] = 0.12;
    p[C::Hrcf as usize] = 0.5;
    p[C::Rbx as usize] = 2.0;
    p[C::Rbi as usize] = 1.3;
    p[C::Re as usize] = 0.4;
    p[C::Rs as usize] = 4.0;
    p[C::Rbp as usize] = 7.0;
    p[C::Is as usize] = 4.81e-17;
    p[C::Nf as usize] = 1.018;
    p[C::Nr as usize] = 1.01;
    p[C::Fc as usize] = 0.8;
    p[C::Cbeo as usize] = 0.5e-15;
    p[C::Cje as usize] = 3e-15;
    p[C::Pe as usize] = 0.92;
    p[C::Me as usize] = 0.12;
    p[C::Aje as usize] = -0.5;
    p[C::Cbco as usize] = 0.4e-15;
    p[C::Cjc as usize] = 2e-15;
    p[C::Qco as usize] = 1.2e-15;
    p[C::Cjep as usize] = 1.3e-15;
    p[C::Pc as usize] = 0.558;
    p[C::Mc as usize] = 0.12;
    p[C::Ajc as usize] = -0.5;
    p[C::Cjcp as usize] = 1.5e-15;
    p[C::Ps as usize] = 0.75;
    p[C::Ms as usize] = 0.33;
    p[C::Ajs as usize] = -0.5;
    p[C::Ibei as usize] = 1.9e-19;
    p[C::Wbe as usize] = 1.0;
    p[C::Nei as usize] = 1.066;
    p[C::Iben as usize] = 4e-16;
    p[C::Nen as usize] = 2.0;
    p[C::Ibci as usize] = 1.5e-20;
    p[C::Nci as usize] = 1.01;
    p[C::Ibcn as usize] = 1e-15;
    p[C::Ncn as usize] = 2.0;
    p[C::Avc1 as usize] = 0.08;
    p[C::Avc2 as usize] = 0.15;
    p[C::Isp as usize] = 1e-17;
    p[C::Wsp as usize] = 0.3;
    p[C::Nfp as usize] = 1.04;
    p[C::Ibeip as usize] = 1e-21;
    p[C::Ibenp as usize] = 1e-19;
    p[C::Ibcip as usize] = 3e-22;
    p[C::Ncip as usize] = 1.05;
    p[C::Ibcnp as usize] = 4e-18;
    p[C::Ncnp as usize] = 2.0;
    p[C::Vef as usize] = 189.0;
    p[C::Ver as usize] = 5.3;
    p[C::Ikf as usize] = 0.009;
    p[C::Ikr as usize] = 0.01;
    p[C::Ikp as usize] = 1e-4;
    p[C::Tf as usize] = 8e-12;
    p[C::Qtf as usize] = 0.25;
    p[C::Xtf as usize] = 0.45;
    p[C::Vtf as usize] = 0.6;
    p[C::Itf as usize] = 0.02;
    p[C::Tr as usize] = 1.3e-12;
    p[C::Kfn as usize] = 1e-12;
    p[C::Afn as usize] = 1.7;
    p[C::Bfn as usize] = 1.2;
    p[C::Xre as usize] = 0.7;
    p[C::Xrbi as usize] = 0.9;
    p[C::Xrci as usize] = 1.2;
    p[C::Xrs as usize] = 0.5;
    p[C::Xvo as usize] = 0.3;
    p[C::Ea as usize] = 1.12;
    p[C::Eaie as usize] = 1.12;
    p[C::Eaic as usize] = 1.12;
    p[C::Eais as usize] = 1.12;
    p[C::Eane as usize] = 1.12;
    p[C::Eanc as usize] = 1.12;
    p[C::Eans as usize] = 1.12;
    p[C::Xis as usize] = 3.0;
    p[C::Xii as usize] = 3.0;
    p[C::Xin as usize] = 3.0;
    p[C::Tnf as usize] = 1e-4;
    p[C::Tavc as usize] = 1.5e-4;
    p[C::Vrt as usize] = 0.6;
    p[C::Art as usize] = 0.1;
    p[C::Ccso as usize] = 1e-15;
    p[C::Qbm as usize] = 1.0;
    p[C::Nkf as usize] = 0.5;
    p[C::Xikf as usize] = 0.6;
    p[C::Xrcx as usize] = 0.25;
    p[C::Xrbx as usize] = 0.4;
    p[C::Xrbp as usize] = 0.5;
    p[C::Isrr as usize] = 1.0;
    p[C::Xisr as usize] = 0.3;
    p[C::Dear as usize] = 0.1;
    p[C::Eap as usize] = 1.12;
    p[C::Vbbe as usize] = 4.2;
    p[C::Nbbe as usize] = 1.15;
    p[C::Ibbe as usize] = 1e-6;
    p[C::Tvbbe1 as usize] = 1e-5;
    p[C::Tvbbe2 as usize] = 2e-8;
    p[C::Tnbbe as usize] = 1e-4;
    p[C::Vers as usize] = 1.1;
    p
};

const fn nominal_values() -> [f64; 108] {
    NOMINAL_VALUES
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}; expected={expected:.15e}; tolerance={tolerance:.3e}"
    );
}

#[test]
fn nominal_temperature_is_model_identity_except_computed_ebbe() -> TestResult {
    let nominal = nominal_values();
    let model = Nominal::new(&nominal)?;
    assert_eq!(C::Vref as usize, 107);
    let prepared = model.prepare(Temperature::new(300.15)?)?;
    assert_eq!(prepared.temperature().kelvin(), 300.15);
    for (index, expected) in nominal.iter().enumerate() {
        if index == C::Ebbe as usize {
            continue;
        }
        let actual = prepared.value(coefficient_at(index)?);
        close(actual, *expected, 2e-13, 2e-20);
    }
    let expected_breakdown = (-4.2 / (1.15 * (1.380_662e-23 / 1.602_189e-19) * 300.15_f64)).exp();
    close(prepared.value(C::Ebbe), expected_breakdown, 2e-14, 0.0);
    Ok(())
}

// Exhaustive mapping of the ordinal coefficient enum to the documented
// original VBIC slots; verifies identity for every source coefficient,
// including the ones not directly consumed by the existing physics kernels.
const ALL_COEFFICIENTS: [C; 108] = [
    C::Tnom,
    C::Rcx,
    C::Rci,
    C::Vo,
    C::Gamm,
    C::Hrcf,
    C::Rbx,
    C::Rbi,
    C::Re,
    C::Rs,
    C::Rbp,
    C::Is,
    C::Nf,
    C::Nr,
    C::Fc,
    C::Cbeo,
    C::Cje,
    C::Pe,
    C::Me,
    C::Aje,
    C::Cbco,
    C::Cjc,
    C::Qco,
    C::Cjep,
    C::Pc,
    C::Mc,
    C::Ajc,
    C::Cjcp,
    C::Ps,
    C::Ms,
    C::Ajs,
    C::Ibei,
    C::Wbe,
    C::Nei,
    C::Iben,
    C::Nen,
    C::Ibci,
    C::Nci,
    C::Ibcn,
    C::Ncn,
    C::Avc1,
    C::Avc2,
    C::Isp,
    C::Wsp,
    C::Nfp,
    C::Ibeip,
    C::Ibenp,
    C::Ibcip,
    C::Ncip,
    C::Ibcnp,
    C::Ncnp,
    C::Vef,
    C::Ver,
    C::Ikf,
    C::Ikr,
    C::Ikp,
    C::Tf,
    C::Qtf,
    C::Xtf,
    C::Vtf,
    C::Itf,
    C::Tr,
    C::Td,
    C::Kfn,
    C::Afn,
    C::Bfn,
    C::Xre,
    C::Xrbi,
    C::Xrci,
    C::Xrs,
    C::Xvo,
    C::Ea,
    C::Eaie,
    C::Eaic,
    C::Eais,
    C::Eane,
    C::Eanc,
    C::Eans,
    C::Xis,
    C::Xii,
    C::Xin,
    C::Tnf,
    C::Tavc,
    C::Rth,
    C::Cth,
    C::Vrt,
    C::Art,
    C::Ccso,
    C::Qbm,
    C::Nkf,
    C::Xikf,
    C::Xrcx,
    C::Xrbx,
    C::Xrbp,
    C::Isrr,
    C::Xisr,
    C::Dear,
    C::Eap,
    C::Vbbe,
    C::Nbbe,
    C::Ibbe,
    C::Tvbbe1,
    C::Tvbbe2,
    C::Tnbbe,
    C::Ebbe,
    C::Dtemp,
    C::Vers,
    C::Vref,
];

fn coefficient_at(index: usize) -> Result<C, &'static str> {
    ALL_COEFFICIENTS
        .get(index)
        .copied()
        .ok_or("invalid parameter index")
}

#[test]
fn temperature_sweep_prepares_every_existing_family_kernel() -> TestResult {
    let model = Nominal::new(&nominal_values())?;
    for celsius in [-40.0, 0.0, 27.0, 60.0, 100.0, 125.0] {
        let prepared = model.prepare(Temperature::new(celsius + 273.15)?)?;
        let temp = prepared.temperature();
        let transfer = vbic::transport::Prepared::new(prepared.transport_parameters(), temp)?;
        let diode = vbic::Prepared::new(prepared.branch_parameters(), temp)?;
        let parasitic = vbic::parasitic::Prepared::new(prepared.parasitic_parameters(), temp)?;
        let depletion = vbic::depletion::Prepared::new(prepared.depletion_parameters())?;
        let avalanche = vbic::avalanche::Prepared::new(prepared.avalanche_parameters())?;
        let resistor = vbic::resistance::Prepared::new(prepared.resistance_parameters(), temp)?;
        let charge = vbic::charge::Prepared::new(&prepared.charge_parameters(), temp)?;
        let noise = vbic::noise::Prepared::new(prepared.noise_parameters(), temp, 1.0)?;
        let normalized = depletion.evaluate(0.75, -0.6)?;
        let actual = transfer.evaluate(vbic::transport::Bias {
            vbei: 0.75,
            vbci: -0.6,
            depletion: normalized,
        })?;
        assert!(actual.net.value.is_finite());
        let measured = diode.evaluate(vbic::Bias {
            vbei: 0.75,
            vbex: 0.75,
            vbci: -0.6,
            vbep: 0.1,
            vbcp: -0.4,
        })?;
        assert!(measured.ibe.amperes.is_finite());
        let parasite = parasitic.evaluate(vbic::parasitic::Bias {
            vbep: 0.1,
            vbci: -0.6,
            vbcp: -0.4,
        })?;
        assert!(parasite.iccp.is_finite());
        let generated = avalanche.evaluate(vbic::avalanche::Bias {
            vbci: -0.6,
            transport: actual,
            ibcj: measured.ibcj,
        })?;
        assert!(generated.ibc.is_finite());
        let charge_out = charge.evaluate(vbic::charge::Bias {
            vbei: 0.75,
            vbex: 0.73,
            vbci: -0.6,
            vbcx: -0.57,
            vbep: 0.1,
            vbcp: -0.4,
            vbe: 0.75,
            vbc: -0.6,
        })?;
        assert!(charge_out.qbe.coulombs.is_finite());
        let resistor_out = resistor.evaluate(&vbic::resistance::Bias {
            vrcx: 0.1,
            vrci: 0.1,
            vbci: -0.6,
            vbcx: -0.57,
            vrbx: 0.02,
            vrbi: 0.01,
            vre: 0.01,
            vrbp: 0.01,
            vrs: 0.01,
            transport: actual,
            parasitic: parasite,
        })?;
        assert!(resistor_out.irci.amperes.is_finite());
        let noise_out = noise.evaluate(vbic::noise::OperatingPoint::default(), 1_000.0)?;
        assert!(noise_out.total_source.is_finite());
    }
    Ok(())
}

#[test]
fn family_exponents_and_activation_energies_apply_at_all_temperatures() -> TestResult {
    let values = nominal_values();
    let model = Nominal::new(&values)?;
    let nominal = 300.15;
    for target in [233.15, 273.15, 320.15, 398.15] {
        let prep = model.prepare(Temperature::new(target)?)?;
        let ratio = target / nominal;
        let vt = 1.380_662e-23 / 1.602_189e-19 * target;
        for (source, exp, energy, ideality) in [
            (C::Is, C::Xis, C::Ea, C::Nf),
            (C::Isrr, C::Xisr, C::Dear, C::Nr),
            (C::Isp, C::Xis, C::Eap, C::Nfp),
            (C::Ibei, C::Xii, C::Eaie, C::Nei),
            (C::Iben, C::Xin, C::Eane, C::Nen),
            (C::Ibci, C::Xii, C::Eaic, C::Nci),
            (C::Ibcn, C::Xin, C::Eanc, C::Ncn),
            (C::Ibeip, C::Xii, C::Eaic, C::Nci),
            (C::Ibenp, C::Xin, C::Eanc, C::Ncn),
            (C::Ibcip, C::Xii, C::Eais, C::Ncip),
            (C::Ibcnp, C::Xin, C::Eans, C::Ncnp),
        ] {
            let expected = model.value(source)
                * (ratio.powf(model.value(exp)) * (model.value(energy) * (ratio - 1.0) / vt).exp())
                    .powf(1.0 / model.value(ideality));
            close(prep.value(source), expected, 3e-14, 0.0);
        }
        for (dst, exponent) in [
            (C::Rcx, C::Xrcx),
            (C::Rci, C::Xrci),
            (C::Rbx, C::Xrbx),
            (C::Rbi, C::Xrbi),
            (C::Re, C::Xre),
            (C::Rs, C::Xrs),
            (C::Rbp, C::Xrbp),
            (C::Ikf, C::Xikf),
            (C::Vo, C::Xvo),
        ] {
            close(
                prep.value(dst),
                model.value(dst) * ratio.powf(model.value(exponent)),
                3e-14,
                0.0,
            );
        }
        assert!(prep.value(C::Pe) > 0.0);
        assert!(prep.value(C::Pc) > 0.0);
        assert!(prep.value(C::Ps) > 0.0);
        close(
            prep.value(C::Cje),
            model.value(C::Cje) * (model.value(C::Pe) / prep.value(C::Pe)).powf(model.value(C::Me)),
            3e-14,
            0.0,
        );
        close(
            prep.value(C::Cjc),
            model.value(C::Cjc) * (model.value(C::Pc) / prep.value(C::Pc)).powf(model.value(C::Mc)),
            3e-14,
            0.0,
        );
    }
    Ok(())
}

#[test]
fn local_dtemp_and_invalid_models_preserve_temperature_identity() -> TestResult {
    let mut raw = nominal_values();
    raw[C::Dtemp as usize] = 20.0;
    let first = Nominal::new(&raw)?.prepare(Temperature::new(300.15)?)?;
    raw[C::Dtemp as usize] = 0.0;
    let second = Nominal::new(&raw)?.prepare(Temperature::new(320.15)?)?;
    assert_eq!(first.temperature(), second.temperature());
    for index in 0..108 {
        if index == C::Dtemp as usize {
            continue;
        }
        let param = coefficient_at(index)?;
        close(first.value(param), second.value(param), 3e-13, 0.0);
    }
    raw[C::Nf as usize] = 0.0;
    assert!(matches!(
        Nominal::new(&raw),
        Err(ModelError::InvalidParameter("nf"))
    ));
    raw = nominal_values();
    raw[C::Pe as usize] = f64::NAN;
    assert!(matches!(
        Nominal::new(&raw),
        Err(ModelError::InvalidParameter("VBIC nominal coefficient"))
    ));
    raw = nominal_values();
    raw[C::Dtemp as usize] = -400.0;
    assert!(matches!(
        Nominal::new(&raw)?.prepare(Temperature::new(300.15)?),
        Err(ModelError::InvalidTemperature)
    ));
    Ok(())
}

#[test]
fn temperature_adjusted_diode_current_and_gpi_match_ngspice_47() -> TestResult {
    // Independent ngspice-47 .op reference at 6 ambient temperatures and
    // 4 source bias points each. The internal VBE/VBC are read from the
    // device, not extrapolated from externally applied voltage.
    let rows = include_str!("fixtures/vbic_ngspice47_temperature.csv");
    let mut raw = nominal_values();
    // Match the source oracle model: only IBEI is a nonzero physical base
    // branch. Transport IS does not contribute to reported base current.
    for coefficient in [
        C::Iben,
        C::Ibci,
        C::Ibcn,
        C::Ibeip,
        C::Ibenp,
        C::Ibcip,
        C::Ibcnp,
        C::Isp,
        C::Ibbe,
    ] {
        raw[coefficient as usize] = 0.0;
    }
    let model = Nominal::new(&raw)?;
    let mut count = 0;
    for line in rows.lines().filter(|line| !line.starts_with('#')) {
        let [
            celsius,
            _source_emitter,
            _source_collector,
            intrinsic_emitter,
            intrinsic_collector,
            base_current,
            gpi,
        ] = <[&str; 7]>::try_from(line.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid VBIC temperature oracle row")?;
        let ambient_celsius: f64 = celsius.parse()?;
        let prepared = model.prepare(Temperature::new(ambient_celsius + 273.15)?)?;
        let diode = vbic::Prepared::new(prepared.branch_parameters(), prepared.temperature())?;
        let emitter_voltage: f64 = intrinsic_emitter.parse()?;
        let collector_voltage: f64 = intrinsic_collector.parse()?;
        let actual = diode.evaluate(vbic::Bias {
            vbei: emitter_voltage,
            vbex: emitter_voltage,
            vbci: collector_voltage,
            vbep: 0.0,
            vbcp: 0.0,
        })?;
        close(actual.ibe.amperes, base_current.parse()?, 2e-5, 3e-17);
        close(actual.ibe.siemens, gpi.parse()?, 2e-5, 3e-17);
        count += 1;
    }
    assert_eq!(count, 24);
    Ok(())
}

#[test]
fn every_transformed_coefficient_has_analytic_thermal_slope() -> TestResult {
    let mut nominal = nominal_values();
    nominal[C::Dtemp.index()] = 12.;
    let model = Nominal::new(&nominal)?;
    for ambient in [253.15, 300.15, 373.15] {
        let baseline = model.prepare(Temperature::new(ambient)?)?;
        for rise in [-5., 0., 10., 35.] {
            let p = baseline.at_rise(rise)?;
            close(p.temperature().kelvin(), ambient + 12. + rise, 0., 1e-12);
            let step = 1e-3;
            let above = baseline.at_rise(rise + step)?;
            let below = baseline.at_rise(rise - step)?;
            for index in 0..nominal.len() {
                let c = coefficient_at(index)?;
                let slope = (above.value(c) - below.value(c)) / (2. * step);
                let magnitude = above.value(c).abs().max(below.value(c).abs());
                close(p.slope(c), slope, 2e-6, magnitude * 1e-10);
            }
        }
    }
    Ok(())
}
