#![expect(
    clippy::float_cmp,
    reason = "exact deterministic identities and endpoints"
)]
use anyhow::{Context, Result};
use model_statistics::{CounterStream, Function, Identity, Realization, Site};

fn site() -> Site {
    Site {
        expression: Identity::root(b"model:bf/call:0"),
        occurrence: Identity::root(b"x1/q1"),
        lot: Some(Identity::root(b"model:global")),
    }
}

fn function(name: &str) -> Result<Function> {
    Function::simetrix(name).context("unknown distribution")
}

#[test]
fn order_independent_identity_and_lot_scope() -> Result<()> {
    let experiment = Realization {
        seed: u64::MAX,
        sample: 123,
    };
    let site = site();
    let independent = function("gauss")?;
    let lot = function("GaussL")?;
    let other_device = Site {
        occurrence: Identity::root(b"x2/q1"),
        ..site
    };
    let other_local_model = Site {
        lot: Some(Identity::root(b"x2/model")),
        ..other_device
    };
    let other_call = Site {
        expression: Identity::root(b"model:bf/call:1"),
        ..site
    };
    let bound_function = independent.bind(site)?;
    let bound = bound_function.prepare(&[0.2])?;
    let first = bound.sample(experiment)?;
    assert_ne!(
        first,
        independent
            .bind(other_device)?
            .prepare(&[0.2])?
            .sample(experiment)?
    );
    assert_ne!(
        first,
        independent
            .bind(other_call)?
            .prepare(&[0.2])?
            .sample(experiment)?
    );
    assert_ne!(
        first,
        bound.sample(Realization {
            sample: 124,
            ..experiment
        })?
    );
    assert_ne!(
        first,
        bound.sample(Realization {
            seed: u64::MAX - 1,
            ..experiment
        })?
    );
    assert_eq!(first, bound.sample(experiment)?);
    let common = lot.bind(site)?.prepare(&[0.2])?.sample(experiment)?;
    assert_eq!(
        common,
        lot.bind(other_device)?
            .prepare(&[0.2])?
            .sample(experiment)?
    );
    assert_ne!(
        common,
        lot.bind(other_local_model)?
            .prepare(&[0.2])?
            .sample(experiment)?
    );
    assert_ne!(
        common,
        lot.bind(other_call)?.prepare(&[0.2])?.sample(experiment)?
    );
    assert!(lot.bind(Site { lot: None, ..site }).is_err());
    let wider = bound_function.prepare(&[0.4])?;
    assert!(
        2.0f64
            .mul_add(-(first - 1.), wider.sample(experiment)? - 1.)
            .abs()
            < 1e-15
    );
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
        let law = function(name)?.bind(site())?.prepare(&args)?;
        let mut sum = 0.;
        let mut squared = 0.;
        for sample in 0..count {
            let value = law.sample(Realization {
                seed: 987_654,
                sample: u64::from(sample),
            })?;
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
        assert!(
            function(name)?.bind(site())?.prepare(&args).is_err(),
            "{name} {args:?}"
        );
    }
    assert_eq!(function("UD2L")?, function("distribution2l")?);
    let law = function("ud2")?
        .bind(site())?
        .prepare(&[1e-308, -1e308, 1e308, 1e308, 1e308])?;
    let tiny = function("ud2")?
        .bind(site())?
        .prepare(&[1e308, -1e-308, 1e-308, 1e-308, 1e-308])?;
    for sample in 0..1000 {
        let context = Realization { seed: 3, sample };
        assert!((0.0..=2.0).contains(&law.sample(context)?));
        assert!((0.0..=2.0).contains(&tiny.sample(context)?));
    }
    Ok(())
}

#[test]
fn invocation_encoding_preserves_every_bit_and_bound_call_identity() -> Result<()> {
    use model_statistics::Invocation;
    let invocation = Invocation {
        function: function("UD2L")?,
        realization: Realization {
            seed: 0xfedc_ba98_7654_3210,
            sample: 0x0123_4567_89ab_cdef,
        },
        site: 0x8000_0000_ffff_ffff,
    };
    let arguments = [0.2, -1., 1., 1., 1.];
    let mut encoded: Vec<_> = invocation.metadata().into_iter().map(f64::from).collect();
    encoded.extend(arguments);
    let (decoded, law) = Invocation::decode(&invocation.marker_name(), &encoded)?;
    assert_eq!(decoded, invocation);
    assert_eq!(
        Invocation::decode(&invocation.marker_name().to_ascii_uppercase(), &encoded)?.0,
        invocation,
    );
    assert_eq!(law, arguments);
    assert_eq!(
        decoded
            .bind(site())?
            .prepare(law)?
            .sample(decoded.realization)?,
        invocation
            .bind(site())?
            .prepare(&arguments)?
            .sample(invocation.realization)?
    );
    assert_ne!(
        decoded
            .bind(site())?
            .prepare(law)?
            .sample(decoded.realization)?,
        Invocation {
            site: invocation.site - 1,
            ..invocation
        }
        .bind(site())?
        .prepare(law)?
        .sample(invocation.realization)?
    );
    for invalid in [-1., 0.5, f64::from(u32::MAX) + 1., f64::INFINITY, f64::NAN] {
        encoded[0] = invalid;
        assert!(Invocation::decode(&invocation.marker_name(), &encoded).is_err());
    }
    assert!(Invocation::decode("simetrix_stat_ud2l", &encoded).is_err());
    Ok(())
}

#[test]
fn identity_component_boundaries_and_domains() {
    let root = Identity::root(b"root");
    let other = Identity::root(b"other");
    assert_ne!(root, root.child(b""));
    assert_ne!(root.child(b"a").child(b"bc"), root.child(b"ab").child(b"c"));
    assert_ne!(root.child(b"a\0b"), root.child(b"a").child(b"b"));
    assert_ne!(root.child(b"a"), other.child(b"a"));
    assert_ne!(root.combine(other), other.combine(root));
    assert_ne!(root.combine(other), root.child(&other.bytes()));
    assert_eq!(root.bytes().len(), 16);
}

fn compare_block_lanes<const N: usize>() {
    for batch in 0..256_u64 {
        let streams = std::array::from_fn(|lane| {
            let lane = u64::try_from(lane).unwrap_or_default();
            let key = Identity::root(b"lane").child(&lane.to_le_bytes());
            CounterStream::new(
                key.words(),
                Realization {
                    seed: u64::MAX.wrapping_sub(batch).rotate_left(17),
                    sample: batch.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(lane),
                },
                lane.rotate_left(31),
            )
        });
        let indices = std::array::from_fn(|lane| {
            u64::MAX
                .wrapping_sub(batch)
                .wrapping_sub(u64::try_from(lane).unwrap_or_default())
        });
        assert_eq!(
            CounterStream::blocks::<N>(&streams, indices),
            std::array::from_fn(|lane| streams[lane].block(indices[lane])),
        );
    }
}

#[test]
fn portable_simd_blocks_match_scalar() {
    compare_block_lanes::<1>();
    compare_block_lanes::<2>();
    compare_block_lanes::<4>();
    compare_block_lanes::<8>();
    compare_block_lanes::<16>();
}

#[test]
fn block_pair_scalar_and_parallel_streams_agree() -> Result<()> {
    let key = Identity::root(b"test-stream").child(b"noise");
    let realization = Realization {
        seed: u64::MAX,
        sample: u64::MAX,
    };
    let stream = CounterStream::new(key.words(), realization, 0);
    for pair in [0, 1, 2, 1024, u64::MAX / 2] {
        assert_eq!(
            stream.uniform_pair(pair),
            [stream.uniform(pair * 2), stream.uniform(pair * 2 + 1)]
        );
    }
    assert_ne!(
        stream.block(0),
        CounterStream::new(key.words(), realization, 1).block(0)
    );
    assert_ne!(stream.block(0), stream.block(u64::MAX));
    assert_ne!(
        stream.block(0),
        CounterStream::new(
            key.words(),
            Realization {
                seed: 0,
                ..realization
            },
            0
        )
        .block(0)
    );
    assert_ne!(
        stream.block(0),
        CounterStream::new(
            key.words(),
            Realization {
                sample: 0,
                ..realization
            },
            0
        )
        .block(0)
    );
    let law = function("gausstrunc")?.bind(site())?.prepare(&[1., 0.4])?;
    let sequential = (0..4096)
        .map(|sample| {
            law.sample(Realization {
                sample,
                ..realization
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let parallel = std::thread::scope(|scope| {
        let threads = (0..4)
            .map(|lane| {
                let law = &law;
                scope.spawn(move || {
                    (0..1024)
                        .map(|i| {
                            law.sample(Realization {
                                sample: i * 4 + lane,
                                ..realization
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                })
            })
            .collect::<Vec<_>>();
        threads
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("sample thread failed"))?
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for (index, value) in sequential.into_iter().enumerate() {
        assert_eq!(value, parallel[index % 4][index / 4]);
    }
    Ok(())
}
