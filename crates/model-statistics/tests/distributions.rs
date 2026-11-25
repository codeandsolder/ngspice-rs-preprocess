#![expect(
    clippy::float_cmp,
    reason = "exact deterministic identities and endpoints"
)]
use anyhow::{Context, Result};
use model_statistics::{Function, Realization, Site};

const SITE: Site<'_> = Site {
    expression: b"model:bf/call:0",
    occurrence: b"x1/q1",
    lot: Some(b"model:global"),
};

fn function(name: &str) -> Result<Function> {
    Function::simetrix(name).context("unknown distribution")
}

#[test]
fn order_independent_identity_and_lot_scope() -> Result<()> {
    let experiment = Realization {
        seed: u64::MAX,
        sample: 123,
    };
    let independent = function("gauss")?.prepare(&[0.2])?;
    let lot = function("GaussL")?.prepare(&[0.2])?;
    let other_device = Site {
        occurrence: b"x2/q1",
        ..SITE
    };
    let other_local_model = Site {
        lot: Some(b"x2/model"),
        ..other_device
    };
    let other_call = Site {
        expression: b"model:bf/call:1",
        ..SITE
    };
    let first = independent.sample(experiment, SITE)?;
    assert_ne!(first, independent.sample(experiment, other_device)?);
    assert_ne!(first, independent.sample(experiment, other_call)?);
    assert_ne!(
        first,
        independent.sample(
            Realization {
                sample: 124,
                ..experiment
            },
            SITE
        )?
    );
    assert_ne!(
        first,
        independent.sample(
            Realization {
                seed: u64::MAX - 1,
                ..experiment
            },
            SITE
        )?
    );
    assert_eq!(first, independent.sample(experiment, SITE)?);
    assert_eq!(
        lot.sample(experiment, SITE)?,
        lot.sample(experiment, other_device)?
    );
    assert_ne!(
        lot.sample(experiment, SITE)?,
        lot.sample(experiment, other_local_model)?
    );
    assert_ne!(
        lot.sample(experiment, SITE)?,
        lot.sample(experiment, other_call)?
    );
    assert!(lot.sample(experiment, Site { lot: None, ..SITE }).is_err());
    let wider = function("gauss")?.prepare(&[0.4])?;
    assert!((wider.sample(experiment, SITE)? - 1. - 2. * (first - 1.)).abs() < 1e-15);
    Ok(())
}

#[test]
fn distribution_moments_and_exact_support() -> Result<()> {
    let cases: [(_, _, f64, f64, _, _); 10] = [
        ("gauss", vec![3.], 1., 1., f64::NEG_INFINITY, f64::INFINITY),
        (
            "gausssigma",
            vec![6., 6.],
            1.,
            1.,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ),
        (
            "gausstrunc",
            vec![1., 1.],
            1.,
            0.291_125_094_772_793,
            0.,
            2.,
        ),
        ("gausstrunc", vec![3., 1e-10], 1., 3., -2., 4.),
        ("unif", vec![3.], 1., 3., -2., 4.),
        ("unif2", vec![4., 2.], 2., 3., -1., 5.),
        ("wc", vec![3.], 1., 9., -2., 4.),
        ("wc2", vec![4., 2.], 2., 9., -1., 5.),
        ("ud2", vec![1., -1., 0., 1., 2.], 4. / 3., 2. / 9., 0., 2.),
        (
            "distribution2",
            vec![1., -1., 1., -0.5, 1., -0.5, 0., 0.5, 0., 0.5, 1., 1., 1.],
            1.,
            7. / 12.,
            0.,
            2.,
        ),
    ];
    let count = 50_000_u32;
    for (name, args, mean, variance, low, high) in cases {
        let law = function(name)?.prepare(&args)?;
        let mut sum = 0.;
        let mut squared = 0.;
        for sample in 0..count {
            let value = law.sample(
                Realization {
                    seed: 987_654,
                    sample: u64::from(sample),
                },
                SITE,
            )?;
            assert!((low..=high).contains(&value), "{name}: {value}");
            if name.starts_with("wc") {
                assert!(value == low || value == high);
            }
            if args.len() == 13 {
                assert!(value <= 0.5 || value >= 1.5, "zero-density gap: {value}");
            }
            let centered = value - mean;
            sum += centered;
            squared = centered.mul_add(centered, squared);
        }
        let n = f64::from(count);
        assert!(
            (sum / n).abs() < 0.025_f64 * variance.sqrt(),
            "{name} mean {}",
            sum / n
        );
        assert!(
            (squared / n / variance - 1.).abs() < 0.035,
            "{name} variance {}",
            squared / n
        );
    }
    Ok(())
}

#[test]
fn validate_shapes_and_extreme_density() -> Result<()> {
    for (name, args) in [
        ("gauss", vec![]),
        ("gauss", vec![-1.]),
        ("gausstrunc", vec![1., 0.]),
        ("unif2", vec![1., -1.]),
        ("ud2", vec![1., 0., 1.]),
        ("ud2", vec![1., 1., 1., 0., 1.]),
        ("ud2", vec![1., 0., 0., 1., 0.]),
        ("ud2", vec![1., 0., -1., 1., 1.]),
        ("ud2", vec![1., 1., 1., 1., 1.]),
        ("gausse", vec![1.]),
        ("unif", vec![f64::NAN]),
    ] {
        assert!(function(name)?.prepare(&args).is_err(), "{name} {args:?}");
    }
    assert_eq!(function("UD2L")?, function("distribution2l")?);
    let law = function("ud2")?.prepare(&[1e-308, -1e308, 1e308, 1e308, 1e308])?;
    let tiny = function("ud2")?.prepare(&[1e308, -1e-308, 1e-308, 1e-308, 1e-308])?;
    for sample in 0..1000 {
        let context = Realization { seed: 3, sample };
        assert!((0.0..=2.0).contains(&law.sample(context, SITE)?));
        assert!((0.0..=2.0).contains(&tiny.sample(context, SITE)?));
    }
    Ok(())
}
