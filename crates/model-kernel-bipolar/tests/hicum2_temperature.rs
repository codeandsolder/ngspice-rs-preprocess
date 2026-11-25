use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{self, temperature},
};
#[path = "fixtures/hicum2_baseline.rs"]
mod hicum2_baseline;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn baseline(mode: f64, delta: f64) -> temperature::Nominal {
    let mut transport = hicum2_baseline::parameters();
    transport.flcomp = mode;
    transport.vcbar = 0.0;
    transport.hjei = 1.1;
    transport.ahjei_nominal = 0.15;
    transport.ahjei = 0.15;
    transport.hjei_zero_bias = 1.1;
    transport.hf0 = 1.05;
    transport.hfe = 0.9;
    transport.hfc = 1.1;
    let thermal = temperature::ThermalCoefficients {
        dt: delta,
        vgb: 1.15,
        vge: 1.13,
        vgc: 1.18,
        vgs: 1.2,
        zetact: 2.7,
        zetabet: 3.4,
        zetaci: 0.4,
        zetacx: 1.2,
        alb: 0.001,
        alvs: 0.0002,
        alces: 0.0001,
        alt0: 0.00005,
        kt0: 1e-7,
        zetahjei: 1.1,
        zetavgbe: 0.9,
        dvgbe: 0.06,
        ..temperature::ThermalCoefficients::default()
    };
    temperature::Nominal {
        polarity: hicum2::Polarity::Npn,
        transport,
        diodes: hicum2::Parameters {
            ibeis: 1e-18,
            mbei: 1.0,
            ireis: 3e-18,
            mrei: 2.0,
            ibeps: 1e-19,
            mbep: 1.0,
            ireps: 5e-19,
            mrep: 2.0,
            ibcis: 1e-16,
            mbci: 1.0,
            ibcxs: 2e-18,
            mbcx: 1.0,
            iscs: 3e-18,
            msc: 1.1,
        },
        coefficients: thermal,
        noise: hicum2::noise::Parameters {
            kf: 0.0,
            af: 2.0,
            flicker_location: hicum2::noise::FlickerLocation::Intrinsic,
            kfre: 0.0,
            afre: 2.0,
            correlated: false,
            alit: 0.333,
            alqf: 0.167,
        },
        auxiliary: temperature::AuxiliaryCoefficients {
            itss: 0.0,
            msf: 1.0,
            tsf: 0.0,
            rbi0: 0.0,
            fdqr0: 0.0,
            fgeo: 0.6557,
            fqi: 1.0,
            fcrbi: 0.0,
            flnqs: false,
            rbx: 0.0,
            rcx: 0.0,
            re: 0.0,
            rsu: 0.0,
            csu: 0.0,
            cbepar: 0.0,
            fbepar: 1.0,
            cbcpar: 0.0,
            fbcpar: 0.0,
            favl: 0.0,
            qavl: 0.0,
            kavl: 0.0,
            ibets: 0.0,
            abet: 40.0,
            tunode: 0,
            rth: 0.0,
            flsh: 0,
            cth: 0.0,
        },
    }
}

fn close(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let tolerance = relative.mul_add(actual.abs().max(expected.abs()), absolute);
    assert!(
        (actual - expected).abs() <= tolerance,
        "actual={actual:.15e}; expected={expected:.15e}; diff={:.7e}; tolerance={tolerance:.4e}",
        (actual - expected).abs(),
    );
}

#[test]
fn effective_hicum_temperature_clamp_and_model_identity() -> TestResult {
    for mode in [0.0, 2.1, 2.2, 2.3] {
        let nominal = baseline(mode, 0.0);
        let prepared = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
        close(prepared.effective_temperature.kelvin(), 300.15, 0.0, 0.0);
        close(prepared.nominal_temperature.kelvin(), 300.15, 0.0, 0.0);
        close(prepared.diodes.ibeis, nominal.diodes.ibeis, 1e-14, 0.0);
        close(prepared.diodes.ibcis, nominal.diodes.ibcis, 1e-14, 0.0);
        close(prepared.transport.c10, nominal.transport.c10, 1e-14, 0.0);
        close(prepared.transport.qp0, nominal.transport.qp0, 1e-12, 0.0);
        close(
            prepared.transport.junction.intrinsic_emitter.potential,
            nominal.transport.junction.intrinsic_emitter.potential,
            1e-12,
            0.0,
        );
        close(
            prepared
                .transport
                .junction
                .intrinsic_collector
                .zero_bias_capacitance,
            nominal
                .transport
                .junction
                .intrinsic_collector
                .zero_bias_capacitance,
            1e-12,
            0.0,
        );
        close(
            prepared.transport.junction.intrinsic_emitter.max_cap_ratio,
            nominal.transport.junction.intrinsic_emitter.max_cap_ratio,
            1e-12,
            0.0,
        );
        assert_eq!(prepared.auxiliary.abet, 1.0);
        prepared.diode_kernel()?;
        prepared.junction_kernel()?;
        prepared.transport_kernel()?;

        let low = nominal.prepare(Temperature::new(130.0)?, 0.0)?;
        let high = nominal.prepare(Temperature::new(720.0)?, 0.0)?;
        assert_eq!(low.effective_temperature.kelvin(), 173.15);
        assert_eq!(high.effective_temperature.kelvin(), 600.0);
    }
    Ok(())
}

#[test]
fn temperature_modes_and_substrate_resistance_tunneling_transform() -> TestResult {
    let mut nominal = baseline(2.3, 12.0);
    let auxiliary = &mut nominal.auxiliary;
    auxiliary.itss = 7e-16;
    auxiliary.tsf = 1e-12;
    auxiliary.rbi0 = 20.0;
    auxiliary.rbx = 7.0;
    auxiliary.rcx = 1.1;
    auxiliary.re = 1.3;
    auxiliary.rsu = 3.0;
    auxiliary.favl = 0.8;
    auxiliary.qavl = 2e-15;
    auxiliary.kavl = 0.2;
    auxiliary.rth = 600.0;
    auxiliary.ibets = 1e-15;
    auxiliary.abet = 40.0;
    auxiliary.tunode = 1;
    let c = &mut nominal.coefficients;
    c.zetarbi = -0.8;
    c.zetarbx = 0.7;
    c.zetarcx = 0.3;
    c.zetare = 0.2;
    c.alfav = 0.004;
    c.alqav = -0.003;
    c.alkav = 0.002;
    c.zetarth = 0.4;
    c.alrth = 0.001;

    for kelvin in [253.15, 300.15, 373.15] {
        let output = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
        let actual = output.effective_temperature.kelvin();
        let ratio = actual / output.nominal_temperature.kelvin();
        let delta = actual - output.nominal_temperature.kelvin();
        close(output.auxiliary.rbi0, 20.0 * ratio.powf(-0.8), 2e-14, 0.0);
        close(output.auxiliary.rbx, 7.0 * ratio.powf(0.7), 2e-14, 0.0);
        close(output.auxiliary.rcx, 1.1 * ratio.powf(0.3), 2e-14, 0.0);
        close(output.auxiliary.re, 1.3 * ratio.powf(0.2), 2e-14, 0.0);
        close(output.auxiliary.tsf, 1e-12 * ratio.powf(0.2), 2e-14, 0.0);
        close(output.auxiliary.rsu, 3.0, 0.0, 0.0);
        close(
            output.auxiliary.favl,
            0.8 * (0.004 * delta).exp(),
            2e-14,
            0.0,
        );
        close(
            output.auxiliary.qavl,
            2e-15 * (-0.003 * delta).exp(),
            2e-14,
            0.0,
        );
        close(
            output.auxiliary.kavl,
            0.2 * (0.002 * delta).exp(),
            2e-14,
            0.0,
        );
        close(
            output.auxiliary.rth,
            600.0 * ratio.powf(0.4) * 0.001f64.mul_add(delta, 1.0),
            2e-14,
            0.0,
        );
        assert!(output.auxiliary.ibets > 0.0 && output.auxiliary.abet > 0.0);
        assert_eq!(output.auxiliary.tunode, 1);
        let substrate = output
            .substrate_kernel()?
            .evaluate(hicum2::substrate::Bias {
                vbpci: 0.1,
                vsici: -0.2,
            })?;
        assert!(substrate.current.value > 0.0);
        assert!(substrate.diffusion_charge.value > 0.0);
        let depletion = output.junction_kernel()?.evaluate(hicum2::junction::Bias {
            vbiei: -0.4,
            vbpei: -0.3,
            vbici: -0.8,
            vbci: -0.8,
            vbpci: -0.8,
            vsici: -1.0,
            vsc: -1.0,
        })?;
        let tunnel = output
            .tunneling_kernel()?
            .evaluate(hicum2::tunneling::Bias {
                vbiei: -0.4,
                vbpei: -0.3,
                intrinsic: depletion.intrinsic_emitter,
                peripheral: depletion.peripheral_emitter,
            })?;
        assert!(tunnel.amperes > 0.0);
        assert_eq!(tunnel.d_vbiei, 0.0);
        assert_ne!(output.diodes.ibeis, nominal.diodes.ibeis);
    }
    Ok(())
}

#[test]
fn source_nominal_cap_ratio_one_can_prepare_below_one() -> TestResult {
    let mut nominal = baseline(2.3, 0.0);
    nominal.transport.junction.intrinsic_emitter.max_cap_ratio = 1.0;
    nominal.transport.junction.peripheral_emitter.max_cap_ratio = 1.0;
    // Nominal source validation is a_j >= 1, while thermal TMPHICJ changes
    // effective a_j by Vd(T)/Vd(Tnom) and may drive it below one.
    let warm = nominal.prepare(Temperature::new(540.0)?, 0.0)?;
    assert!(warm.transport.junction.intrinsic_emitter.max_cap_ratio < 1.0);
    assert!(warm.transport.junction.intrinsic_emitter.max_cap_ratio > 0.0);
    warm.junction_kernel()?.evaluate(hicum2::junction::Bias {
        vbiei: 0.6,
        vbpei: 0.6,
        vbici: -1.5,
        vbci: -1.5,
        vbpci: -1.5,
        vsici: -1.0,
        vsc: -1.0,
    })?;
    nominal.transport.junction.intrinsic_emitter.max_cap_ratio = 0.9;
    assert!(matches!(
        nominal.prepare(Temperature::new(300.15)?, 0.0),
        Err(ModelError::InvalidParameter("ajei"))
    ));
    Ok(())
}

#[test]
fn invalid_nominal_temperature_and_thermal_models_rejected() -> TestResult {
    let temperature = Temperature::new(300.15)?;
    let mut nominal = baseline(2.3, 0.0);
    nominal.coefficients.tnom_celsius = -300.0;
    assert!(matches!(
        nominal.prepare(temperature, 0.0),
        Err(ModelError::InvalidTemperature)
    ));
    nominal = baseline(2.3, 0.0);
    nominal.coefficients.vgb = f64::NAN;
    assert!(matches!(
        nominal.prepare(temperature, 0.0),
        Err(ModelError::InvalidParameter("vgb"))
    ));
    nominal = baseline(2.3, 0.0);
    nominal.auxiliary.tunode = 2;
    assert!(matches!(
        nominal.prepare(temperature, 0.0),
        Err(ModelError::InvalidParameter("tunode"))
    ));
    nominal = baseline(2.3, 0.0);
    nominal.auxiliary.rbi0 = -1.0;
    assert!(matches!(
        nominal.prepare(temperature, 0.0),
        Err(ModelError::InvalidParameter("rbi0"))
    ));
    Ok(())
}

fn check_intrinsic_junction_caps(
    prepared: &temperature::Prepared,
    collector_voltage: f64,
    cpii: &str,
    cmui: &str,
) -> TestResult {
    let junction = prepared
        .junction_kernel()?
        .evaluate(hicum2::junction::Bias {
            vbiei: 0.0,
            vbpei: 0.0,
            vbici: collector_voltage,
            vbci: collector_voltage,
            vbpci: collector_voltage,
            vsici: collector_voltage,
            vsc: collector_voltage,
        })?;
    // Source CALC_OP: CPIi=Cjei+ddx(Qdei), CMUi=Cjci+ddx(Qdci).
    // At VBE=0/VBC=-1.3 V both diffusion terms are negligible. Source
    // QJMOD explicitly outputs Cjci, not always identical to d(Qjci)/dV.
    close(
        junction.intrinsic_emitter.source_capacitance,
        cpii.parse()?,
        2e-4,
        1e-20,
    );
    close(
        junction.intrinsic_collector.source_capacitance,
        cmui.parse()?,
        2e-4,
        1e-20,
    );
    Ok(())
}

#[test]
fn hicum_native_ngspice47_temperature_and_flcomp_oracle() -> TestResult {
    // Native operating-point oracle exported from ngspice-47 level=8.
    // B-E/B-C current, ICK, net transfer current and forward transit time
    // use the *reported* internal device voltages (not source terminal).
    let csv = include_str!("fixtures/hicum2_ngspice47_temperature.csv");
    let mut cases = 0;
    let mut compatible = 0;
    let mut capacitance_cases = 0;
    let mut intermediate_branch_differences = 0;
    for row in csv.lines().filter(|line| !line.starts_with('#')) {
        let fields = <[&str; 14]>::try_from(row.split(',').collect::<Vec<_>>())
            .map_err(|_| "invalid HICUM temperature oracle row")?;
        let [
            mode,
            ambient,
            local_dt,
            _source_be,
            _source_ce,
            emitter_voltage_text,
            collector_voltage_text,
            emitter_current_text,
            collector_diode_text,
            ic,
            ick,
            tf,
            cpii,
            cmui,
        ] = fields;
        let mode: f64 = mode.parse()?;
        let ambient: f64 = ambient.parse()?;
        let local_dt: f64 = local_dt.parse()?;
        let b_e: f64 = emitter_voltage_text.parse()?;
        let b_c: f64 = collector_voltage_text.parse()?;
        let nominal = baseline(mode, local_dt);
        let prepared = nominal.prepare(Temperature::new(ambient + 273.15)?, 0.0)?;
        let current = prepared.diode_kernel()?.evaluate(hicum2::Bias {
            vbiei: b_e,
            vbici: b_c,
            vbpei: b_e,
            vbpci: b_c,
            vsici: 0.0,
        })?;
        let transport = prepared
            .transport_kernel()?
            .evaluate(hicum2::transport::Bias {
                vbiei: b_e,
                vbici: b_c,
            })?;
        let actual = [
            current.ibei.amperes,
            current.ibci.amperes,
            transport.net_transfer_current.value,
            transport.critical_current.value,
            transport.forward_transit_time,
        ];
        // ngspice's OP ibci includes its explicit gmin * Vbici term
        // (the native Q1 [ibci] is not solely the intrinsic HICDIO source).
        let expected = [
            emitter_current_text.parse::<f64>()?,
            1e-18f64.mul_add(-b_c, collector_diode_text.parse::<f64>()?),
            ic.parse()?,
            ick.parse()?,
            tf.parse()?,
        ];
        if b_e == 0.0 {
            check_intrinsic_junction_caps(&prepared, b_c, cpii, cmui)?;
            capacitance_cases += 1;
        }
        if mode.to_bits() == 2.2_f64.to_bits() {
            // The installed native level=8 maps flcomp=2.2 to the
            // historical thermal branch. Published HICUM/L2 2.4.0 VA
            // assigns it distinct intermediate formulas. Keep source
            // conformance rather than silently folding it into 2.1.
            if (actual[0] / expected[0] - 1.0).abs() > 1e-3 {
                intermediate_branch_differences += 1;
            }
        } else {
            for (index, (actual, reference)) in actual.into_iter().zip(expected).enumerate() {
                // The low-injection terminal collector current can include
                // gmin effects when the physical It approaches attoamperes.
                if index != 2 || b_e != 0.0 {
                    close(actual, reference, 3e-4, 1e-25);
                }
            }
            compatible += 1;
        }
        cases += 1;
    }
    assert_eq!(cases, 192);
    assert_eq!(compatible, 144);
    assert_eq!(capacitance_cases, 48);
    assert!(intermediate_branch_differences >= 12);
    Ok(())
}

#[test]
fn prepared_hicum_internal_base_uses_effective_rbi_charge_and_temperature() -> TestResult {
    let mut nominal = baseline(2.3, 12.0);
    nominal.auxiliary.rbi0 = 20.0;
    nominal.auxiliary.fdqr0 = 1.2;
    nominal.auxiliary.fgeo = 0.6557;
    nominal.auxiliary.fqi = 0.75;
    nominal.auxiliary.fcrbi = 0.3;
    nominal.coefficients.zetarbi = -0.6;
    for kelvin in [233.15, 300.15, 373.15] {
        let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
        let junction = prepared
            .junction_kernel()?
            .evaluate(hicum2::junction::Bias {
                vbiei: 0.7,
                vbpei: 0.7,
                vbici: -0.5,
                vbci: -0.5,
                vbpci: -0.5,
                vsici: -1.0,
                vsc: -1.0,
            })?;
        let transport = prepared
            .transport_kernel()?
            .evaluate(hicum2::transport::Bias {
                vbiei: 0.7,
                vbici: -0.5,
            })?;
        let diode = prepared.diode_kernel()?.evaluate(hicum2::Bias {
            vbiei: 0.7,
            vbpei: 0.7,
            vbici: -0.5,
            vbpci: -0.5,
            vsici: -1.0,
        })?;
        let output = prepared
            .internal_base_kernel()?
            .evaluate(&hicum2::internal_base::Bias {
                vbpbi: 0.01,
                emitter: junction.intrinsic_emitter,
                collector: junction.intrinsic_collector,
                transport,
                intrinsic_diode: diode.ibei,
            })?;
        assert!(!output.shorted);
        assert!(output.resistance.value > 0.0);
        assert!(output.hf_branch_capacitance > 0.0);
        close(
            output.resistor_current.value,
            0.01 / output.resistance.value,
            1e-14,
            0.0,
        );
    }
    nominal.auxiliary.fcrbi = -0.1;
    assert!(matches!(
        nominal.prepare(Temperature::new(300.15)?, 0.0),
        Err(ModelError::InvalidParameter("fcrbi"))
    ));
    Ok(())
}

#[test]
fn prepared_noise_spectra_track_clamped_effective_tdev() -> TestResult {
    let mut nominal = baseline(2.3, 18.0);
    nominal.noise.kf = 3e-19;
    nominal.noise.kfre = 2e-19;
    nominal.noise.correlated = true;
    for ambient in [180.0, 260.0, 300.15, 440.0, 660.0] {
        let prepared = nominal.prepare(Temperature::new(ambient)?, 0.0)?;
        let thermal = prepared.noise_kernel()?.evaluate(
            hicum2::noise::OperatingPoint {
                conductances: hicum2::noise::Conductances {
                    rbx: 0.2,
                    rbi: 0.1,
                    rcx: 0.01,
                    re: 0.5,
                    rsu: 0.0,
                },
                currents: hicum2::noise::Currents {
                    it: 0.01,
                    ibei: 1e-4,
                    ibep: 1e-7,
                    ibci: -2e-5,
                    ijbcx: 1e-8,
                    ijsc: 0.0,
                    iavl: 1e-6,
                    emitter_resistor: -0.01,
                },
                forward_transit_time: 3e-12,
            },
            1e9,
        )?;
        let effective_kelvin = (ambient + 18.0).clamp(173.15, 600.0);
        close(
            thermal.thermal.rbi,
            4.0 * 1.380_650_3e-23 * effective_kelvin * 0.1,
            2e-14,
            0.0,
        );
        assert!(thermal.be_ce_shot.correlated);
        assert!(thermal.be_ce_shot.cross_imaginary > 0.0);
        assert!(thermal.flicker.be > 0.0);
        assert!(thermal.flicker.emitter_resistor > 0.0);
    }
    nominal.noise.af = 11.0;
    assert!(matches!(
        nominal.prepare(Temperature::new(300.15)?, 0.0),
        Err(ModelError::InvalidParameter("af"))
    ));
    Ok(())
}

#[test]
fn prepared_nqs_and_correlated_noise_share_temperature_and_delay_factors() -> TestResult {
    let mut nominal = baseline(2.3, 15.0);
    nominal.auxiliary.flnqs = true;
    nominal.noise.correlated = true;
    nominal.noise.alit = 0.4;
    nominal.noise.alqf = 0.22;
    for temperature_kelvin in [233.15, 300.15, 398.15] {
        let p = nominal.prepare(Temperature::new(temperature_kelvin)?, 0.0)?;
        let transfer = p.transport_kernel()?.evaluate(hicum2::transport::Bias {
            vbiei: 0.8,
            vbici: -0.7,
        })?;
        let state = hicum2::nqs::State {
            current_first: 0.8 * transfer.forward_current.value,
            current_second: 1.2 * transfer.forward_current.value,
            stored_charge: 0.7 * transfer.forward_diffusion_charge.value,
        };
        let nqs = p.nqs_kernel()?;
        let output = nqs.evaluate(&hicum2::nqs::Bias {
            transport: transfer,
            state,
        })?;
        assert!(output.active);
        close(
            output.first_charge.d_state.current_first,
            0.4 * p.transport.t0,
            1e-14,
            0.0,
        );
        close(
            output.second_charge.d_state.current_second,
            0.4 * p.transport.t0 / 3.0,
            1e-14,
            0.0,
        );
        close(
            output.charge_filter_charge.d_state.stored_charge,
            0.22 * p.transport.t0,
            1e-14,
            0.0,
        );
        let noise = p.noise_kernel()?.evaluate(
            hicum2::noise::OperatingPoint {
                currents: hicum2::noise::Currents {
                    it: transfer.net_transfer_current.value,
                    ibei: 1e-5,
                    ..hicum2::noise::Currents::default()
                },
                forward_transit_time: transfer.forward_transit_time,
                ..hicum2::noise::OperatingPoint::default()
            },
            1e10,
        )?;
        close(
            noise.be_ce_shot.n1_seconds,
            0.4 * transfer.forward_transit_time,
            1e-14,
            0.0,
        );
        assert!(noise.be_ce_shot.correlated);
    }
    nominal.auxiliary.flnqs = false;
    nominal.transport.flcomp = 2.1;
    let legacy = nominal.prepare(Temperature::new(300.15)?, 0.0)?;
    assert!(legacy.nqs_kernel()?.is_active(1e-12));
    Ok(())
}

fn thermal_power_input(
    prepared: &temperature::Prepared,
) -> Result<hicum2::thermal::ElectricalBias, Box<dyn std::error::Error>> {
    let diode = prepared.diode_kernel()?.evaluate(hicum2::Bias {
        vbiei: 0.75,
        vbpei: 0.73,
        vbici: -0.45,
        vbpci: -0.5,
        vsici: -0.65,
    })?;
    let transfer = prepared
        .transport_kernel()?
        .evaluate(hicum2::transport::Bias {
            vbiei: 0.75,
            vbici: -0.45,
        })?;
    let junction = prepared
        .junction_kernel()?
        .evaluate(hicum2::junction::Bias {
            vbiei: 0.75,
            vbpei: 0.73,
            vbici: -0.45,
            vbci: -0.45,
            vbpci: -0.5,
            vsici: -0.65,
            vsc: -0.65,
        })?;
    let base = prepared
        .internal_base_kernel()?
        .evaluate(&hicum2::internal_base::Bias {
            vbpbi: 0.01,
            emitter: junction.intrinsic_emitter,
            collector: junction.intrinsic_collector,
            transport: transfer,
            intrinsic_diode: diode.ibei,
        })?;
    Ok(hicum2::thermal::ElectricalBias {
        transport: hicum2::thermal::Terminal {
            voltage: 1.2,
            current: transfer.net_transfer_current.value,
        },
        intrinsic_emitter: hicum2::thermal::Terminal {
            voltage: 0.75,
            current: diode.ibei.amperes,
        },
        intrinsic_collector: hicum2::thermal::Terminal {
            voltage: -0.45,
            current: diode.ibci.amperes,
        },
        peripheral_emitter: hicum2::thermal::Terminal {
            voltage: 0.73,
            current: diode.ibep.amperes,
        },
        peripheral_collector: hicum2::thermal::Terminal {
            voltage: -0.5,
            current: diode.ijbcx.amperes,
        },
        substrate: hicum2::thermal::Terminal {
            voltage: -0.65,
            current: diode.ijsc.amperes,
        },
        intrinsic_base: hicum2::thermal::Series {
            voltage: 0.01,
            resistance: base.resistance.value,
        },
        external_emitter: hicum2::thermal::Series {
            voltage: 0.002,
            resistance: prepared.auxiliary.re,
        },
        external_collector: hicum2::thermal::Series {
            voltage: 0.004,
            resistance: prepared.auxiliary.rcx,
        },
        external_base: hicum2::thermal::Series {
            voltage: 0.003,
            resistance: prepared.auxiliary.rbx,
        },
        ..hicum2::thermal::ElectricalBias::default()
    })
}

#[test]
fn prepared_self_heating_recomputes_transport_diodes_and_thermal_kcl() -> TestResult {
    let mut model = baseline(2.3, 12.0);
    model.auxiliary.flsh = 2;
    model.auxiliary.rth = 350.0;
    model.auxiliary.cth = 2e-9;
    model.auxiliary.rbi0 = 10.0;
    model.auxiliary.re = 3.0;
    model.auxiliary.rbx = 2.0;
    model.auxiliary.rcx = 4.0;
    model.coefficients.zetarth = 0.6;
    model.coefficients.alrth = 0.0004;
    for ambient in [260.0, 300.15, 360.0] {
        for rise in [-30.0, 0.0, 15.0, 70.0, 400.0] {
            let prepared = model.prepare(Temperature::new(ambient)?, rise)?;
            let expected_kelvin = (ambient + 12.0 + rise).clamp(173.15, 600.0);
            close(
                prepared.effective_temperature.kelvin(),
                expected_kelvin,
                0.0,
                0.0,
            );
            let electrical = thermal_power_input(&prepared)?;
            let output = prepared.thermal_kernel().evaluate(electrical)?;
            assert!(!output.shorted);
            let expected_rth = 350.0
                * (expected_kelvin / 300.15).powf(0.6)
                * 0.0004f64.mul_add(expected_kelvin - 300.15, 1.0);
            close(prepared.auxiliary.rth, expected_rth, 5e-14, 0.0);
            close(
                output.thermal_current,
                rise / expected_rth - output.electrical.watts,
                1e-12,
                1e-20,
            );
            close(output.heat_charge, 2e-9 * rise, 2e-14, 0.0);
            assert_eq!(
                output.d_tdev_d_rise,
                if ambient + 12.0 + rise > 173.15 && ambient + 12.0 + rise < 600.0 {
                    1.0
                } else {
                    0.0
                },
            );
        }
    }
    Ok(())
}

#[test]
fn self_heating_source_enforces_enable_gate_and_input_bounds() -> TestResult {
    let mut model = baseline(2.3, 0.0);
    let ambient = Temperature::new(300.15)?;
    assert!(matches!(
        model.prepare(ambient, 1.0),
        Err(ModelError::InvalidParameter("thermal_rise"))
    ));
    model.auxiliary.flsh = 1;
    model.auxiliary.rth = 0.0009;
    assert!(matches!(
        model.prepare(ambient, 1.0),
        Err(ModelError::InvalidParameter("thermal_rise"))
    ));
    model.auxiliary.rth = 100.0;
    let active = model.prepare(ambient, 1.0)?;
    assert!(
        !active
            .thermal_kernel()
            .evaluate(hicum2::thermal::ElectricalBias::default(),)?
            .shorted
    );
    assert!(matches!(
        model.prepare(ambient, f64::NAN),
        Err(ModelError::InvalidParameter("thermal_rise"))
    ));
    model.auxiliary.flsh = 3;
    assert!(matches!(
        model.prepare(ambient, 0.0),
        Err(ModelError::InvalidParameter("flsh"))
    ));
    model.auxiliary.flsh = 1;
    model.auxiliary.cth = -1.0;
    assert!(matches!(
        model.prepare(ambient, 0.0),
        Err(ModelError::InvalidParameter("cth"))
    ));
    Ok(())
}

fn assert_external_source_noise(
    prepared: &temperature::Prepared,
    eval: hicum2::external::Output,
    kelvin: f64,
) -> TestResult {
    let source_noise = prepared.noise_kernel()?.evaluate(
        hicum2::noise::OperatingPoint {
            conductances: hicum2::noise::Conductances {
                rbx: eval.external_base.siemens,
                rcx: eval.external_collector.siemens,
                re: eval.emitter.siemens,
                rsu: eval.substrate.siemens,
                ..hicum2::noise::Conductances::default()
            },
            ..hicum2::noise::OperatingPoint::default()
        },
        1e6,
    )?;
    close(
        source_noise.thermal.rbx,
        4.0 * 1.380_650_3e-23 * kelvin * eval.external_base.siemens,
        1e-14,
        0.0,
    );
    close(
        source_noise.thermal.rsu,
        4.0 * 1.380_650_3e-23 * kelvin * eval.substrate.siemens,
        1e-14,
        0.0,
    );
    Ok(())
}

fn verify_external_temperature_trial(
    nominal: &temperature::Nominal,
    expected: hicum2::external::Split,
    kelvin: f64,
) -> TestResult {
    let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
    let ext = prepared.external_kernel();
    assert_eq!(ext.split(), expected);
    let cap = prepared.transport.junction.external_collector;
    let source_external = expected.external_collector_junction;
    let source_peripheral = expected.peripheral_collector_junction;
    // Temperature transform uses the same factor for both split
    // external junctions; it leaves isolation capacitors constant.
    if (kelvin - 300.15).abs() < 1e-12 {
        close(
            cap.external_zero_bias_capacitance,
            source_external,
            1e-13,
            1e-30,
        );
        close(
            cap.peripheral_zero_bias_capacitance,
            source_peripheral,
            1e-13,
            1e-30,
        );
    } else if source_external > 0.0 && source_peripheral > 0.0 {
        close(
            cap.external_zero_bias_capacitance / source_external,
            cap.peripheral_zero_bias_capacitance / source_peripheral,
            1e-12,
            0.0,
        );
    }
    let ratio = kelvin / 300.15;
    let eval = ext.evaluate(hicum2::external::Bias {
        vbbp: 0.12,
        vcci: -0.02,
        veie: 0.01,
        vsis: -0.6,
        vbe: 0.7,
        vbpe: 0.68,
        vbci: -0.4,
        vbpci: -0.42,
    })?;
    close(
        eval.external_base.siemens,
        1.0 / (12.0 * ratio.powf(0.8)),
        1e-14,
        0.0,
    );
    close(
        eval.external_collector.siemens,
        1.0 / (7.0 * ratio.powf(-0.4)),
        1e-14,
        0.0,
    );
    close(
        eval.emitter.siemens,
        1.0 / (4.0 * ratio.powf(0.6)),
        1e-14,
        0.0,
    );
    close(eval.substrate.siemens, 1.0 / 9.0, 1e-14, 0.0);
    close(eval.substrate_shunt.farads, 5e-15, 0.0, 0.0);
    close(
        eval.external_collector_isolation.farads,
        expected.external_collector_isolation,
        0.0,
        0.0,
    );
    close(
        eval.peripheral_collector_isolation.farads,
        expected.peripheral_collector_isolation,
        0.0,
        0.0,
    );
    assert_external_source_noise(&prepared, eval, kelvin)?;
    Ok(())
}

#[test]
fn source_external_parasitic_partition_and_noise_share_temperature_topology() -> TestResult {
    let mut nominal = baseline(2.3, 0.0);
    nominal.auxiliary = temperature::AuxiliaryCoefficients {
        rbx: 12.0,
        rcx: 7.0,
        re: 4.0,
        rsu: 9.0,
        csu: 5e-15,
        cbepar: 8e-15,
        fbepar: 0.3,
        cbcpar: 6e-15,
        ..nominal.auxiliary
    };
    nominal.coefficients = temperature::ThermalCoefficients {
        zetarbx: 0.8,
        zetarcx: -0.4,
        zetare: 0.6,
        ..nominal.coefficients
    };
    let original = nominal.transport.junction.external_collector;
    let total_cjcx0 =
        original.external_zero_bias_capacitance + original.peripheral_zero_bias_capacitance;
    for fbcpar in [0.0, 0.2, 0.4, 0.7, 1.0] {
        nominal.auxiliary.fbcpar = fbcpar;
        let source = hicum2::external::Capacitances {
            substrate_shunt: 5e-15,
            base_emitter_parasitic: 8e-15,
            base_emitter_partition: 0.3,
            base_collector_parasitic: 6e-15,
            base_collector_partition: fbcpar,
            external_collector_zero_bias: total_cjcx0,
        };
        let expected = source.split()?;
        for kelvin in [233.15, 300.15, 423.15] {
            verify_external_temperature_trial(&nominal, expected, kelvin)?;
        }
    }
    Ok(())
}

#[test]
fn source_nominal_internal_base_short_gate_survives_thermal_scaling() -> TestResult {
    let mut nominal = baseline(2.3, 0.0);
    nominal.coefficients.zetarbi = -4.0;
    for (nominal_rbi, expected_short) in [(0.0009, true), (0.001, false)] {
        nominal.auxiliary.rbi0 = nominal_rbi;
        for kelvin in [233.15, 300.15, 423.15] {
            let prepared = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
            let transfer = prepared
                .transport_kernel()?
                .evaluate(hicum2::transport::Bias {
                    vbiei: 0.78,
                    vbici: -0.45,
                })?;
            let j = prepared
                .junction_kernel()?
                .evaluate(hicum2::junction::Bias {
                    vbiei: 0.78,
                    vbpei: 0.78,
                    vbici: -0.45,
                    vbci: -0.45,
                    vbpci: -0.45,
                    vsici: -1.0,
                    vsc: -1.0,
                })?;
            let diode = prepared.diode_kernel()?.evaluate(hicum2::Bias {
                vbiei: 0.78,
                vbpei: 0.78,
                vbici: -0.45,
                vbpci: -0.45,
                vsici: -1.0,
            })?;
            let output =
                prepared
                    .internal_base_kernel()?
                    .evaluate(&hicum2::internal_base::Bias {
                        vbpbi: 0.002,
                        emitter: j.intrinsic_emitter,
                        collector: j.intrinsic_collector,
                        transport: transfer,
                        intrinsic_diode: diode.ibei,
                    })?;
            assert_eq!(output.shorted, expected_short);
            assert_eq!(prepared.nominal_rbi0, nominal_rbi);
            close(
                prepared.auxiliary.rbi0,
                nominal_rbi * (kelvin / 300.15).powi(-4),
                1e-14,
                0.0,
            );
        }
    }
    Ok(())
}

/// Effective source TMPHICJ physical triplets, including both external
/// collector partitions, in the same field order as the prepared source derivative arrays.
const fn junction_coefficient_triplets(p: &temperature::Prepared) -> [[f64; 3]; 7] {
    let j = p.transport.junction;
    [
        [
            j.intrinsic_emitter.zero_bias_capacitance,
            j.intrinsic_emitter.potential,
            j.intrinsic_emitter.max_cap_ratio,
        ],
        [
            j.peripheral_emitter.zero_bias_capacitance,
            j.peripheral_emitter.potential,
            j.peripheral_emitter.max_cap_ratio,
        ],
        [
            j.intrinsic_collector.zero_bias_capacitance,
            j.intrinsic_collector.potential,
            j.intrinsic_collector.punch_through,
        ],
        [
            j.external_collector.external_zero_bias_capacitance,
            j.external_collector.potential,
            j.external_collector.punch_through,
        ],
        [
            j.external_collector.peripheral_zero_bias_capacitance,
            j.external_collector.potential,
            j.external_collector.punch_through,
        ],
        [
            j.substrate.zero_bias_capacitance,
            j.substrate.potential,
            j.substrate.punch_through,
        ],
        [
            j.peripheral_substrate.zero_bias_capacitance,
            j.peripheral_substrate.potential,
            j.peripheral_substrate.punch_through,
        ],
    ]
}

fn junction_temperature_triplets(p: &temperature::Prepared) -> [[f64; 3]; 7] {
    let s = p.junction_parameter_slopes;
    [
        s.intrinsic_emitter,
        s.peripheral_emitter,
        s.intrinsic_collector,
        s.external_collector,
        s.peripheral_collector,
        s.substrate,
        s.peripheral_substrate,
    ]
    .map(|g| {
        [
            g.zero_bias_capacitance,
            g.potential,
            g.maximum_or_punchthrough,
        ]
    })
}

#[test]
fn source_tmphicj_all_physical_coefficients_have_exact_temperature_slopes() -> TestResult {
    let mut comparisons = 0;
    for variant in 0..4 {
        let mut model = baseline(2.3, 0.0);
        match variant {
            0 => {}
            1 => {
                model.coefficients.vge = 1.05;
                model.coefficients.f1vg = -0.00008;
                model.coefficients.f2vg = 0.0002;
            }
            2 => {
                model
                    .transport
                    .junction
                    .intrinsic_emitter
                    .zero_bias_capacitance = 0.0;
                model
                    .transport
                    .junction
                    .intrinsic_collector
                    .zero_bias_capacitance = 0.0;
                model
                    .transport
                    .junction
                    .peripheral_substrate
                    .zero_bias_capacitance = 0.0;
            }
            3 => {
                model.transport.junction.peripheral_substrate.potential = 0.0;
                model.auxiliary.fbcpar = 1.0;
            }
            _ => unreachable!(),
        }
        for kelvin in [223.15, 273.15, 300.15, 373.15, 498.15] {
            let h = 0.003;
            let middle = model.prepare(Temperature::new(kelvin)?, 0.0)?;
            let lower = model.prepare(Temperature::new(kelvin - h)?, 0.0)?;
            let upper = model.prepare(Temperature::new(kelvin + h)?, 0.0)?;
            let values = (
                junction_coefficient_triplets(&lower),
                junction_coefficient_triplets(&upper),
            );
            for (family, (slopes, (minus, plus))) in junction_temperature_triplets(&middle)
                .into_iter()
                .zip(values.0.into_iter().zip(values.1))
                .enumerate()
            {
                for (param, (exact, (lo, hi))) in slopes
                    .into_iter()
                    .zip(minus.into_iter().zip(plus))
                    .enumerate()
                {
                    let numerical = (hi - lo) / (2.0 * h);
                    close(exact, numerical, 2e-6, 2e-19);
                    assert!(
                        exact.is_finite(),
                        "non-finite slope {variant}:{family}:{param}"
                    );
                    comparisons += 1;
                }
            }
        }
    }
    assert_eq!(comparisons, 420);
    Ok(())
}

#[test]
fn analytic_emitter_junction_charge_and_capacitance_thermal_slopes_match_repreparation()
-> TestResult {
    let mut checked = 0;
    for variant in 0..4 {
        let mut model = baseline(2.3, 0.0);
        match variant {
            0 => {}
            1 => {
                model.transport.junction.intrinsic_emitter.max_cap_ratio = 1.0;
                model.transport.junction.peripheral_emitter.max_cap_ratio = 1.0;
                model.coefficients.vge = 1.05;
            }
            2 => {
                model
                    .transport
                    .junction
                    .intrinsic_emitter
                    .zero_bias_capacitance = 0.0;
            }
            3 => {
                model
                    .transport
                    .junction
                    .peripheral_emitter
                    .zero_bias_capacitance = 0.0;
            }
            _ => unreachable!(),
        }
        for kelvin in [233.15, 300.15, 373.15, 520.0] {
            let step = 0.003;
            let middle = model.prepare(Temperature::new(kelvin)?, 0.0)?;
            let lower = model.prepare(Temperature::new(kelvin - step)?, 0.0)?;
            let upper = model.prepare(Temperature::new(kelvin + step)?, 0.0)?;
            for voltage in [-4.0, -0.8, -0.2, 0.0, 0.35, 0.6, 0.85, 1.2, 2.5] {
                let bias = hicum2::junction::Bias {
                    vbiei: voltage,
                    vbpei: voltage - 0.08,
                    vbici: -0.5,
                    vbci: -0.5,
                    vbpci: -0.5,
                    vsici: -0.7,
                    vsc: -0.7,
                };
                let exact = middle
                    .junction_kernel()?
                    .emitter_temperature_sensitivity(bias)?;
                let before = lower.junction_kernel()?.evaluate(bias)?;
                let after = upper.junction_kernel()?.evaluate(bias)?;
                for (sensitivity, lo, hi) in [
                    (
                        exact.intrinsic,
                        before.intrinsic_emitter,
                        after.intrinsic_emitter,
                    ),
                    (
                        exact.peripheral,
                        before.peripheral_emitter,
                        after.peripheral_emitter,
                    ),
                ] {
                    close(
                        sensitivity.charge,
                        (hi.coulombs - lo.coulombs) / (2.0 * step),
                        2e-5,
                        1e-24,
                    );
                    close(
                        sensitivity.capacitance,
                        (hi.source_capacitance - lo.source_capacitance) / (2.0 * step),
                        2e-5,
                        1e-23,
                    );
                    checked += 2;
                }
            }
        }
    }
    assert_eq!(checked, 576);
    Ok(())
}

fn collector_thermal_model_variant(variant: usize) -> temperature::Nominal {
    let mut nominal = baseline(2.3, 0.0);
    let j = &mut nominal.transport.junction;
    match variant {
        0 => {}
        1 => {
            j.intrinsic_collector.punch_through = 2.0;
            j.external_collector.punch_through = 2.5;
            j.substrate.punch_through = 1.8;
            j.peripheral_substrate.punch_through = 1.4;
        }
        2 => {
            j.intrinsic_collector.punch_through = 100.0;
            j.external_collector.punch_through = 100.0;
            j.substrate.punch_through = 100.0;
            j.peripheral_substrate.punch_through = 100.0;
        }
        3 => {
            j.intrinsic_collector.zero_bias_capacitance = 0.0;
            j.external_collector.external_zero_bias_capacitance = 0.0;
            j.external_collector.peripheral_zero_bias_capacitance = 0.0;
            j.substrate.zero_bias_capacitance = 0.0;
        }
        4 => {
            j.peripheral_substrate.potential = 0.0;
            nominal.auxiliary.fbcpar = 0.7;
            nominal.auxiliary.cbcpar = 2e-15;
        }
        5 => {
            j.intrinsic_collector.punch_through = 1.6;
            j.external_collector.punch_through = 2.6;
            j.substrate.punch_through = 2.2;
            j.peripheral_substrate.punch_through = 100.0;
            nominal.auxiliary.fbcpar = 0.3;
            nominal.auxiliary.cbcpar = 1e-15;
        }
        _ => unreachable!(),
    }
    nominal
}

#[test]
fn exact_collector_reachthrough_thermal_charge_and_published_capacitance_slopes() -> TestResult {
    let mut comparisons = 0;
    let mut source_cap_difference = 0;
    for variant in 0..6 {
        let nominal = collector_thermal_model_variant(variant);
        for kelvin in [233.15, 300.15, 373.15, 523.15] {
            let h = 0.003;
            let middle = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
            let lower = nominal.prepare(Temperature::new(kelvin - h)?, 0.0)?;
            let upper = nominal.prepare(Temperature::new(kelvin + h)?, 0.0)?;
            for voltage in [-4.0, -1.8, -0.9, -0.3, 0.2, 0.65, 0.9, 1.25] {
                let bias = hicum2::junction::Bias {
                    vbiei: 0.12,
                    vbpei: 0.09,
                    vbici: voltage,
                    vbci: voltage + 0.11,
                    vbpci: voltage - 0.15,
                    vsici: voltage + 0.19,
                    vsc: voltage - 0.21,
                };
                let center = middle.junction_kernel()?;
                let expected = center.collector_temperature_sensitivity(bias)?;
                let minus = lower.junction_kernel()?.evaluate(bias)?;
                let plus = upper.junction_kernel()?.evaluate(bias)?;
                for (slope, before, after) in [
                    (
                        expected.intrinsic,
                        minus.intrinsic_collector,
                        plus.intrinsic_collector,
                    ),
                    (
                        expected.external,
                        minus.external_collector,
                        plus.external_collector,
                    ),
                    (
                        expected.peripheral,
                        minus.peripheral_collector,
                        plus.peripheral_collector,
                    ),
                    (expected.substrate, minus.substrate, plus.substrate),
                    (
                        expected.peripheral_substrate,
                        minus.peripheral_substrate,
                        plus.peripheral_substrate,
                    ),
                ] {
                    for (actual, lo, hi) in [
                        (slope.charge, before.coulombs, after.coulombs),
                        (slope.capacitance, before.farads, after.farads),
                        (
                            slope.source_capacitance,
                            before.source_capacitance,
                            after.source_capacitance,
                        ),
                    ] {
                        close(actual, (hi - lo) / (2.0 * h), 5e-5, 5e-23);
                        comparisons += 1;
                    }
                    if (slope.capacitance - slope.source_capacitance).abs() > 1e-21 {
                        source_cap_difference += 1;
                    }
                }
            }
        }
    }
    assert_eq!(comparisons, 2880);
    assert!(source_cap_difference > 20);
    Ok(())
}

fn hiccr_thermal_variant(mode: f64, variant: usize) -> temperature::Nominal {
    let mut nominal = baseline(mode, 0.0);
    nominal.transport.t0 = 8e-12;
    nominal.transport.dt0h = 2e-12;
    nominal.transport.tbvl = -1e-12;
    match variant {
        0 => {}
        1 => {
            nominal.coefficients.alvs = 0.0007;
            nominal.coefficients.zetaci = 1.8;
            nominal.coefficients.alt0 = 0.0005;
            nominal.coefficients.kt0 = 1e-6;
        }
        2 => {
            nominal
                .transport
                .junction
                .intrinsic_collector
                .zero_bias_capacitance = 0.0;
        }
        3 => {
            nominal.transport.junction.intrinsic_collector.punch_through = 100.0;
            nominal.coefficients.alvs = 0.003;
            nominal.coefficients.zetaci = 0.8;
            nominal.transport.dt0h = -3e-12;
            nominal.transport.tbvl = 2e-12;
        }
        _ => unreachable!(),
    }
    nominal
}

#[test]
fn exact_hiccr_critical_current_and_low_transit_time_thermal_slopes() -> TestResult {
    let mut checked = 0;
    let mut t0_voltage_dependence = 0;
    for mode in [0.0, 2.1, 2.2, 2.3] {
        for variant in 0..4 {
            let nominal = hiccr_thermal_variant(mode, variant);
            for kelvin in [233.15, 300.15, 373.15, 510.15] {
                let h = 0.003;
                let center = nominal.prepare(Temperature::new(kelvin)?, 0.0)?;
                let below = nominal.prepare(Temperature::new(kelvin - h)?, 0.0)?;
                let above = nominal.prepare(Temperature::new(kelvin + h)?, 0.0)?;
                let exact = center.transport_temperature_slopes;
                for (predicted, low, high) in [
                    (exact.rci0, below.transport.rci0, above.transport.rci0),
                    (exact.vlim, below.transport.vlim, above.transport.vlim),
                    (exact.vces, below.transport.vces, above.transport.vces),
                    (exact.t0, below.transport.t0, above.transport.t0),
                ] {
                    close(predicted, (high - low) / (2.0 * h), 2e-6, 3e-12);
                    checked += 1;
                }
                let kernel = center.transport_kernel()?;
                let lower_kernel = below.transport_kernel()?;
                let upper_kernel = above.transport_kernel()?;
                for (emitter, collector) in [
                    (0.56, -1.3),
                    (0.65, -0.6),
                    (0.75, -0.25),
                    (0.80, 0.01),
                    (0.83, 0.21),
                    (0.73, 0.58),
                ] {
                    let bias = hicum2::transport::Bias {
                        vbiei: emitter,
                        vbici: collector,
                    };
                    let reference = kernel.evaluate(bias)?;
                    let lower = lower_kernel.evaluate(bias)?;
                    let upper = upper_kernel.evaluate(bias)?;
                    let current_slope =
                        kernel.critical_current_temperature_sensitivity(emitter - collector)?;
                    close(
                        current_slope,
                        (upper.critical_current.value - lower.critical_current.value) / (2.0 * h),
                        2e-5,
                        2e-13,
                    );
                    let time_slope =
                        kernel.low_current_transit_time_temperature_sensitivity(collector)?;
                    close(
                        time_slope,
                        (upper.low_current_transit_time.value
                            - lower.low_current_transit_time.value)
                            / (2.0 * h),
                        2e-5,
                        2e-19,
                    );
                    let cap_slope = center
                        .junction_kernel()?
                        .transit_collector_temperature_sensitivity(collector)?;
                    let low_cap = below
                        .junction_kernel()?
                        .intrinsic_for_transport(emitter, collector)?
                        .transit_collector_capacitance;
                    let high_cap = above
                        .junction_kernel()?
                        .intrinsic_for_transport(emitter, collector)?
                        .transit_collector_capacitance;
                    close(cap_slope, (high_cap - low_cap) / (2.0 * h), 2e-5, 2e-23);
                    if time_slope.abs() > 1e-16 && reference.low_current_transit_time.value > 0.0 {
                        t0_voltage_dependence += 1;
                    }
                    checked += 3;
                }
            }
        }
    }
    assert_eq!(checked, 1408);
    assert!(t0_voltage_dependence > 50);
    Ok(())
}
