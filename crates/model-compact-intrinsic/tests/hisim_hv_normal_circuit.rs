//! Converged native circuit observables for the isolated normal channel.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvIntrinsicBias, ModelError, ModelVariant, ParameterScope,
    ParameterValue, PreparedHiSimHvNormalChannel, PreparedHiSimHvTransport,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_normal_channel,
    prepare_hisim_hv_transport,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn prepare(
    row: &[f64],
) -> Result<(PreparedHiSimHvNormalChannel, PreparedHiSimHvTransport), ModelError> {
    let variant = match row[0] {
        1. => ModelVariant::HiSimHv1,
        2. => ModelVariant::HiSimHv2,
        _ => {
            return Err(ModelError::InvalidPhysicalParameter(
                "reference model version",
            ));
        }
    };
    let mut fields = vec![(
        if row[1] < 0. { "pmos" } else { "nmos" },
        ParameterValue::Flag(true),
    )];
    for name in ["corsrd", "coisti", "coisub", "coadov", "coovlp", "conqs"] {
        fields.push((name, ParameterValue::Integer(0)));
    }
    if variant == ModelVariant::HiSimHv2 {
        fields.push(("cordrift", ParameterValue::Integer(0)));
    }
    let config = row.get(16).copied().unwrap_or_default();
    if matches!(config, 1. | 4.) {
        fields.extend(
            [
                ("ptl", 0.02),
                ("ptlp", 0.7),
                ("ptp", 3.7),
                ("pt2", 0.12),
                ("pt4", 0.4),
                ("pt4p", 0.8),
            ]
            .map(|(n, v)| (n, ParameterValue::Real(v))),
        );
    }
    if matches!(config, 2. | 4.) {
        fields.extend(
            [("gdl", 0.001), ("gdld", 0.2e-6), ("gdlp", 0.4)]
                .map(|(n, v)| (n, ParameterValue::Real(v))),
        );
    }
    if matches!(config, 3. | 4.) {
        fields.push(("gdsleak", ParameterValue::Real(1e-7)));
    }
    let model = prepare_hisim_hv_model(&BoundParameters::bind(
        variant,
        ParameterScope::Model,
        fields,
    )?)?;
    let instance = prepare_hisim_hv_instance(
        &model,
        &BoundParameters::bind(
            variant,
            ParameterScope::Instance,
            [
                ("l", ParameterValue::Real(row[3])),
                ("w", ParameterValue::Real(row[4])),
                ("m", ParameterValue::Real(row[5])),
                (
                    "nf",
                    ParameterValue::Real(row.get(17).copied().unwrap_or(1.)),
                ),
            ],
        )?,
    )?;
    Ok((
        prepare_hisim_hv_normal_channel(&model, &instance)?,
        prepare_hisim_hv_transport(&model, &instance, row[2] + 273.15, 0.)?,
    ))
}

fn compare_circuit<const N: usize>(fixture: &str, expected_count: usize) -> TestResult {
    let mut count = 0;
    for (index, line) in fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let row: [f64; N] = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?
            .try_into()
            .map_err(|_| "native fixture width")?;
        let (channel, temperature) = prepare(&row)?;
        let point = channel
            .evaluate(
                HiSimHvIntrinsicBias {
                    vgs: row[6],
                    vds: row[7],
                    vbs: row[8],
                },
                &temperature,
            )
            .map_err(|e| format!("native case {index}: {e}"))?;
        let current = point.drain_current_a;
        let charge = point.electrostatics.terminal_charges;
        let actual = [
            current.value,
            current.gradient[0],
            current.gradient[1],
            current.gradient[2],
            charge.bulk.value,
            charge.gate.value,
            charge.drain.value,
        ];
        // Native root convergence leaves sub-femtoampere numerical residue in
        // weak inversion. Preserve meaningful small-current comparisons while
        // allowing that absolute native root uncertainty.
        for (field, value) in actual.into_iter().enumerate() {
            let expected = row[9 + field];
            let absolute = if field < 4 { 1e-19 } else { 1e-25 };
            let relative = if field < 4 { 5e-7_f64 } else { 2e-7_f64 };
            assert!(
                (value - expected).abs()
                    <= relative.mul_add(value.abs().max(expected.abs()), absolute),
                "case {index}, field {field}, bias {:?}: {value:e} vs {expected:e}",
                &row[..9]
            );
        }
        count += 1;
    }
    assert_eq!(count, expected_count);
    Ok(())
}

#[test]
fn isolated_normal_channel_matches_native_operating_points() -> TestResult {
    compare_circuit::<16>(
        include_str!("fixtures/hisim-hv-normal-circuit-ngspice47.txt"),
        1344,
    )
}

#[test]
fn optional_channel_corrections_match_native_operating_points() -> TestResult {
    compare_circuit::<18>(
        include_str!("fixtures/hisim-hv-channel-corrections-ngspice47.txt"),
        4032,
    )
}
