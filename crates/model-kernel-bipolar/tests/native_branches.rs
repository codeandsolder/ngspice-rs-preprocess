use model_kernel_bipolar::{Branch, EvaluationError, ModelError, Temperature, hicum2, vbic};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn approx(actual: f64, expected: f64, relative: f64, absolute: f64) {
    let error = (actual - expected).abs();
    assert!(
        error <= relative.mul_add(expected.abs().max(actual.abs()), absolute),
        "expected {expected:.16e}, obtained {actual:.16e}; error {error:.4e}"
    );
}

fn finite_difference<F>(f: F, bias: f64, h: f64) -> Result<f64, EvaluationError>
where
    F: Fn(f64) -> Result<f64, EvaluationError>,
{
    Ok((f(bias + h)? - f(bias - h)?) / (2.0 * h))
}

// IHP-Open-PDK, sg13g2_hbt_mod.lib, npn13G2, nominal corner Nx=4:
// all default multiplicative process/mismatch factors = 1.
// This is the *temperature-corrected subset* at TNOM=27°C, not a
// completed PDK subcircuit or claim of full VBIC conformance.
const fn ihp_vbic_npn13g2_nx4() -> vbic::Parameters {
    vbic::Parameters {
        ibei: 1.9e-19,
        iben: 4.00e-16,
        nei: 1.066,
        nen: 2.0,
        wbe: 1.0,
        ibci: 1.50e-20,
        ibcn: 1.00e-15,
        nci: 1.103,
        ncn: 1.96,
        ibeip: 4.00e-19,
        ibenp: 2.00e-14,
        ibcip: 2.00e-15,
        ibcnp: 5.00e-15,
        ncip: 1.0,
        ncnp: 1.50,
        vbbe: 0.0,
        nbbe: 1.0,
        ibbe: 0.0,
    }
}

const fn vbic_bias(emitter_bias: f64, collector_bias: f64) -> vbic::Bias {
    vbic::Bias {
        vbei: emitter_bias,
        vbex: emitter_bias,
        vbci: collector_bias,
        vbep: collector_bias,
        vbcp: collector_bias,
    }
}

const fn hicum_params() -> hicum2::Parameters {
    hicum2::Parameters {
        ibeis: 1e-18,
        mbei: 1.0,
        ireis: 1e-16,
        mrei: 2.0,
        ibeps: 1e-19,
        mbep: 1.0,
        ireps: 1e-16,
        mrep: 2.0,
        ibcis: 1e-16,
        mbci: 1.0,
        ibcxs: 1e-20,
        mbcx: 1.5,
        iscs: 1e-19,
        msc: 1.3,
    }
}

const fn hicum_bias(v: f64) -> hicum2::Bias {
    hicum2::Bias {
        vbiei: v,
        vbpei: v,
        vbici: v,
        vbpci: v,
        vsici: v,
    }
}

const fn hicum_branches(c: hicum2::Currents) -> [Branch; 7] {
    [c.ibei, c.irei, c.ibep, c.irep, c.ibci, c.ijbcx, c.ijsc]
}

#[test]
fn thermal_voltage_is_physical_and_temperature_is_validated() -> TestResult {
    let t = Temperature::new(300.15)?;
    approx(t.thermal_voltage(), 0.025_864_925_786, 1e-8, 0.0);
    assert_eq!(t.kelvin(), 300.15);
    for invalid in [0.0, -273.15, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            Temperature::new(invalid),
            Err(ModelError::InvalidTemperature)
        ));
    }
    Ok(())
}

#[test]
fn vbic_ihp_forward_reverse_and_temperature_sweep_derivatives() -> TestResult {
    for temp in [233.15, 273.15, 300.15, 348.15, 398.15] {
        // Parameter temperature dependence itself must be prepared upstream.
        // This tests the exact branch law at each supplied temperature.
        let kernel = vbic::Prepared::new(ihp_vbic_npn13g2_nx4(), Temperature::new(temp)?)?;
        for (vbei, vbci) in [
            (-0.75, -0.4),
            (-0.1, 0.2),
            (0.0, 0.0),
            (0.62, -0.5),
            (0.78, -0.4),
            (0.92, 0.25),
        ] {
            let bias = vbic_bias(vbei, vbci);
            let actual = kernel.evaluate(bias)?;
            let conductance_emitter = finite_difference(
                |v| Ok(kernel.evaluate(vbic_bias(v, vbci))?.ibe.amperes),
                vbei,
                1e-7,
            )?;
            let conductance_collector = finite_difference(
                |v| Ok(kernel.evaluate(vbic_bias(vbei, v))?.ibcj.amperes),
                vbci,
                1e-7,
            )?;
            approx(actual.ibe.siemens, conductance_emitter, 2e-7, 1e-21);
            approx(actual.ibcj.siemens, conductance_collector, 2e-7, 1e-21);
            assert_eq!(actual.ibex, Branch::default());
            assert!(actual.ibe.siemens >= 0.0);
            assert!(actual.ibcj.siemens >= 0.0);
        }
        let zero = kernel.evaluate(vbic_bias(0.0, 0.0))?;
        assert_eq!(zero.ibe.amperes, 0.0);
        assert_eq!(zero.ibcj.amperes, 0.0);
        assert_eq!(zero.ibep.amperes, 0.0);
        assert_eq!(zero.ibcp.amperes, 0.0);
    }
    // ngspice-47 VBIC LEVEL=9 isolated branch, IHP npn13G2 Nx=4 nominal.
    // VBE=0.8V, VBC=-0.7V; source constants matter at this bias.
    let nominal = vbic::Prepared::new(ihp_vbic_npn13g2_nx4(), Temperature::new(300.15)?)?;
    let measured = nominal.evaluate(vbic_bias(0.8, -0.7))?.ibe;
    approx(measured.amperes, 7.602_340e-7, 3e-6, 0.0);
    approx(measured.siemens, 2.753_740e-5, 3e-6, 0.0);
    Ok(())
}

#[test]
fn vbic_wbe_split_and_reverse_breakdown_are_conservative() -> TestResult {
    let temp = Temperature::new(300.15)?;
    let mut p = ihp_vbic_npn13g2_nx4();
    p.wbe = 0.25;
    p.vbbe = 4.0;
    p.nbbe = 1.5;
    p.ibbe = 1e-14;
    let kernel = vbic::Prepared::new(p, temp)?;
    let bias = vbic_bias(-4.5, -0.7);
    let actual = kernel.evaluate(bias)?;
    let total = actual.ibe.amperes + actual.ibex.amperes;
    let total_derivative = actual.ibe.siemens + actual.ibex.siemens;
    approx(actual.ibe.amperes, total * 0.25, 1e-12, 1e-21);
    approx(actual.ibex.amperes, total * 0.75, 1e-12, 1e-21);
    approx(actual.ibe.siemens, total_derivative * 0.25, 1e-12, 1e-21);
    let numerical = finite_difference(
        |v| Ok(kernel.evaluate(vbic_bias(v, -0.7))?.ibe.amperes),
        -4.5,
        1e-7,
    )?;
    approx(numerical, actual.ibe.siemens, 1e-7, 1e-20);
    let at_zero = kernel.evaluate(vbic_bias(0.0, 0.0))?;
    assert_eq!(at_zero.ibe.amperes, 0.0);
    assert_eq!(at_zero.ibex.amperes, 0.0);
    Ok(())
}

#[test]
fn hicum_reference_exponential_limits_and_all_branches() -> TestResult {
    let temp = Temperature::new(300.15)?;
    let vt = (1.380_650_3e-23 / 1.602_176_462e-19) * temp.kelvin();
    let p = hicum_params();
    let kernel = hicum2::Prepared::new(p, temp)?;
    let nominal = kernel.evaluate(hicum_bias(0.0))?;
    for branch in hicum_branches(nominal) {
        assert_eq!(branch.amperes, 0.0);
        assert!(branch.siemens > 0.0);
    }

    for exponent in [
        -30.0, -14.001, -13.5, -0.1, 0.0, 0.5, 16.0, 79.0, 80.0, 80.5, 95.0,
    ] {
        let voltage = exponent * vt;
        let current = kernel.evaluate(hicum_bias(voltage))?.ibei;
        if exponent <= -14.0 {
            assert_eq!(current.amperes, -p.ibeis);
            assert_eq!(current.siemens, 0.0);
        } else if exponent > 80.0 {
            let expected = p.ibeis * (1.0 + exponent - 80.0).mul_add(80.0_f64.exp(), -1.0);
            approx(current.amperes, expected, 1e-13, 1e-30);
            approx(current.siemens, p.ibeis * 80.0_f64.exp() / vt, 1e-13, 1e-30);
        } else {
            approx(current.amperes, p.ibeis * exponent.exp_m1(), 1e-13, 1e-27);
        }
    }
    for v in [-0.2, 0.0, 0.3, 0.7, 1.8] {
        let result = kernel.evaluate(hicum_bias(v))?;
        let branches = hicum_branches(result);
        for (index, branch) in branches.iter().enumerate() {
            let numeric = finite_difference(
                |x| Ok(hicum_branches(kernel.evaluate(hicum_bias(x))?)[index].amperes),
                v,
                1e-8,
            )?;
            approx(branch.siemens, numeric, 2e-7, 1e-20);
        }
    }
    Ok(())
}

#[test]
fn invalid_parameters_and_batch_contract_fail_closed() -> TestResult {
    let temp = Temperature::new(300.15)?;
    let mut vp = ihp_vbic_npn13g2_nx4();
    vp.nen = 0.0;
    assert!(matches!(
        vbic::Prepared::new(vp, temp),
        Err(ModelError::InvalidParameter("nen"))
    ));
    let mut hp = hicum_params();
    hp.mrei = -1.0;
    assert!(matches!(
        hicum2::Prepared::new(hp, temp),
        Err(ModelError::InvalidParameter("mrei"))
    ));
    let vk = vbic::Prepared::new(ihp_vbic_npn13g2_nx4(), temp)?;
    let inputs = [vbic_bias(0.7, 0.1); 3];
    let mut outputs = [vbic::Currents::default(); 3];
    vk.evaluate_batch(&inputs, &mut outputs)?;
    for (input, out) in inputs.iter().zip(outputs) {
        assert_eq!(vk.evaluate(*input)?, out);
    }
    assert!(matches!(
        vk.evaluate_batch(&inputs, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    ));
    assert!(matches!(
        vk.evaluate(vbic_bias(f64::NAN, 0.0)),
        Err(EvaluationError::NonFiniteBias)
    ));
    let hk = hicum2::Prepared::new(hicum_params(), temp)?;
    let hinputs = [hicum_bias(0.5); 2];
    let mut hout = [hicum2::Currents::default(); 2];
    hk.evaluate_batch(&hinputs, &mut hout)?;
    assert_eq!(hout[0], hk.evaluate(hinputs[0])?);
    Ok(())
}

fn check_lane_contract<I: Copy, O: Copy + PartialEq + std::fmt::Debug>(
    inputs: [I; 3],
    invalid: I,
    sentinel: O,
    scalar: impl Fn(I) -> Result<O, EvaluationError>,
    batch: impl Fn(&[I], &mut [O]) -> Result<(), EvaluationError>,
) -> TestResult {
    let mut outputs = [sentinel; 3];
    batch(&[], &mut [])?;
    assert_eq!(
        batch(&inputs, &mut outputs[..2]),
        Err(EvaluationError::LengthMismatch)
    );
    assert_eq!(
        batch(&inputs[..2], &mut outputs),
        Err(EvaluationError::LengthMismatch)
    );
    assert_eq!(outputs, [sentinel; 3]);
    let first = scalar(inputs[0])?;
    assert_ne!(first, sentinel);
    assert_eq!(
        batch(&[inputs[0], invalid, inputs[2]], &mut outputs),
        Err(EvaluationError::NonFiniteBias)
    );
    assert_eq!(outputs, [first, sentinel, sentinel]);
    batch(&inputs, &mut outputs)?;
    for (input, output) in inputs.into_iter().zip(outputs) {
        assert_eq!(scalar(input)?, output);
    }
    Ok(())
}

#[test]
fn borrowed_batch_contract_across_families_and_temperatures() -> TestResult {
    for kelvin in [233.15, 300.15, 398.15] {
        let temp = Temperature::new(kelvin)?;
        let vk = vbic::Prepared::new(ihp_vbic_npn13g2_nx4(), temp)?;
        check_lane_contract(
            [
                vbic_bias(0.7, -0.5),
                vbic_bias(-0.2, 0.1),
                vbic_bias(0.9, 0.2),
            ],
            vbic_bias(f64::NAN, 0.0),
            vk.evaluate(vbic_bias(0.0, 0.0))?,
            |input| vk.evaluate(input),
            |inputs, outputs| vk.evaluate_batch(inputs, outputs),
        )?;
        let hk = hicum2::Prepared::new(hicum_params(), temp)?;
        check_lane_contract(
            [hicum_bias(0.7), hicum_bias(-0.2), hicum_bias(0.9)],
            hicum_bias(f64::NAN),
            hk.evaluate(hicum_bias(0.0))?,
            |input| hk.evaluate(input),
            |inputs, outputs| hk.evaluate_batch(inputs, outputs),
        )?;
    }
    Ok(())
}
