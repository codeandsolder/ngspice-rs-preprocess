//! Independent native HiSIM-HV GIDL/GISL branch and numerical derivatives.
use model_compact_intrinsic::{
    BoundParameters, HiSimHvGidlCurrents, HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar,
    ModelVariant, ParameterScope,
    ParameterValue::{Integer, Real},
    PreparedHiSimHvGidl, PreparedHiSimHvInstance, PreparedHiSimHvModel, prepare_hisim_hv_gidl,
    prepare_hisim_hv_instance, prepare_hisim_hv_model, prepare_hisim_hv_temperature,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type PreparedCase = (
    PreparedHiSimHvModel,
    PreparedHiSimHvInstance,
    PreparedHiSimHvGidl,
);

struct Case {
    variant: ModelVariant,
    enabled: i32,
    geometry: [f64; 4],
    trial: [f64; 4],
    shift: HiSimHvIntrinsicScalar,
    expected: [f64; 10],
}
impl Case {
    fn parse(line: &str) -> TestResult<Self> {
        let values = line
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()?;
        let fields: [f64; 25] = values
            .try_into()
            .map_err(|_| "GIDL reference record width")?;
        let variant = match fields[0] {
            1. => ModelVariant::HiSimHv1,
            2. => ModelVariant::HiSimHv2,
            _ => return Err("GIDL reference family".into()),
        };
        let enabled = match fields[1] {
            0. => 0,
            1. => 1,
            _ => return Err("GIDL reference selector".into()),
        };
        Ok(Self {
            variant,
            enabled,
            geometry: fields[2..6].try_into()?,
            trial: fields[6..10].try_into()?,
            shift: HiSimHvIntrinsicScalar {
                value: fields[10],
                gradient: fields[11..15].try_into()?,
            },
            expected: fields[15..].try_into()?,
        })
    }
    fn prepare(&self) -> TestResult<PreparedCase> {
        let model = prepare_hisim_hv_model(&BoundParameters::bind(
            self.variant,
            ParameterScope::Model,
            [
                ("cogidl", Integer(self.enabled)),
                ("gidl1", Real(2.)),
                ("gidl2", Real(3e7)),
                ("gidl3", Real(0.9)),
                ("gidl4", Real(0.15)),
                ("gidl5", Real(0.2)),
                ("lgidl1", Real(0.8)),
                ("wgidl1", Real(1.2)),
                ("pgidl1", Real(0.3)),
                ("lgidl2", Real(2e6)),
                ("wgidl2", Real(3e6)),
                ("pgidl2", Real(1e6)),
                ("tox", Real(8e-9)),
                ("eg0", Real(1.2)),
                ("egig", Real(0.06)),
                ("igtemp2", Real(110.)),
                ("igtemp3", Real(1300.)),
                ("xl", Real(0.)),
                ("xw", Real(0.)),
                ("xwd", Real(0.)),
                ("wl", Real(0.)),
                ("lbinn", Real(1.)),
                ("wbinn", Real(1.)),
            ],
        )?)?;
        let [length, width, fingers, multiplier] = self.geometry;
        let instance = prepare_hisim_hv_instance(
            &model,
            &BoundParameters::bind(
                self.variant,
                ParameterScope::Instance,
                [
                    ("l", Real(length * 1e-6)),
                    ("w", Real(width * 1e-6)),
                    ("nf", Real(fingers)),
                    ("m", Real(multiplier)),
                ],
            )?,
        )?;
        let law = prepare_hisim_hv_gidl(&model, &instance)?;
        Ok((model, instance, law))
    }
    fn evaluate(
        &self,
        prepared: &PreparedCase,
        trial: [f64; 4],
    ) -> TestResult<HiSimHvGidlCurrents> {
        let [vgs, vds, vbs, kelvin] = trial;
        let shift = trial
            .into_iter()
            .zip(self.trial)
            .zip(self.shift.gradient)
            .fold(self.shift.value, |sum, ((new, old), slope)| {
                slope.mul_add(new - old, sum)
            });
        let temperature = prepare_hisim_hv_temperature(&prepared.0, &prepared.1, kelvin, 0.)?;
        Ok(prepared.2.evaluate(
            HiSimHvIntrinsicBias { vgs, vds, vbs },
            HiSimHvIntrinsicScalar {
                value: shift,
                ..self.shift
            },
            &temperature,
        )?)
    }
}
fn close(actual: f64, expected: f64, relative: f64, absolute: f64, context: impl std::fmt::Debug) {
    assert!(
        (actual - expected).abs() <= relative.mul_add(actual.abs().max(expected.abs()), absolute),
        "{context:?}: {actual:e} vs {expected:e}"
    );
}
#[test]
fn native_gidl_and_gisl_reference_and_gradients() -> TestResult {
    let fixture = include_str!("fixtures/hisim-hv-gidl-ngspice47.txt");
    let mut count = 0;
    for (index, line) in fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .enumerate()
    {
        let case = Case::parse(line)?;
        let prepared = case.prepare()?;
        let point = case.evaluate(&prepared, case.trial)?;
        for (side, (actual, expected)) in [point.drain_to_body, point.source_to_body]
            .into_iter()
            .zip(case.expected.as_chunks::<5>().0)
            .enumerate()
        {
            for (axis, (value, reference)) in std::iter::once(actual.value)
                .chain(actual.gradient)
                .zip(expected.iter().copied())
                .enumerate()
            {
                close(value, reference, 2e-10, 1e-30, (index, side, axis));
            }
        }
        for axis in 0..4 {
            if axis == 1 && case.trial[axis] == 0. {
                continue;
            }
            let step = if axis == 3 { 1e-3 } else { 1e-6 };
            let mut plus = case.trial;
            plus[axis] += step;
            let mut minus = case.trial;
            minus[axis] -= step;
            let upper = case.evaluate(&prepared, plus)?;
            let lower = case.evaluate(&prepared, minus)?;
            for (side, (actual, hi, lo)) in [
                (
                    point.drain_to_body,
                    upper.drain_to_body,
                    lower.drain_to_body,
                ),
                (
                    point.source_to_body,
                    upper.source_to_body,
                    lower.source_to_body,
                ),
            ]
            .into_iter()
            .enumerate()
            {
                close(
                    actual.gradient[axis],
                    (hi.value - lo.value) / (2. * step),
                    3e-5,
                    1e-22,
                    (index, side, axis, "finite difference"),
                );
            }
        }
        count += 1;
    }
    assert_eq!(count, 720);
    Ok(())
}
