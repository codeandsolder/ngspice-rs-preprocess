//! Touchstone codec fixtures using official v1/v2 ordering and normalization.
use model_touchstone::{Complex, TouchstoneResult};
use model_touchstone::{
    Encoding, MatrixFormat, MixedPort, Parameter, Touchstone, TouchstoneLimits, TwoPortOrder,
    Version,
};

const LIMITS: TouchstoneLimits = TouchstoneLimits {
    source_bytes: 1024 * 1024,
    ports: 16,
    frequencies: 100,
    numeric_bytes: 1024 * 1024,
};

#[test]
fn touchstone_v1_two_port_legacy_column_order_is_not_row_major() -> TouchstoneResult<()> {
    let text = "! nonreciprocal VNA data\n# GHz S RI R 50\n1 0.1 0 2 0 3 0 0.4 0\n";
    let network = Touchstone::parse(text, "amplifier.s2p", LIMITS)?;
    let reordered = text.replace("# GHz S RI R 50", "# S R 50 GHz RI");
    assert_eq!(
        Touchstone::parse(&reordered, "nonstandard.s2p", LIMITS)?.points,
        network.points
    );
    assert_eq!(network.version, Version::V1_0);
    assert_eq!((network.points[0].hz).to_bits(), (1e9_f64).to_bits());
    assert_eq!(
        network.points[0].matrix,
        [
            Complex::real(0.1),
            Complex::real(3.),
            Complex::real(2.),
            Complex::real(0.4),
        ]
    );
    assert_eq!(network.two_port_order, TwoPortOrder::N21BeforeN12);
    let output = network.write(Version::V1_0, Encoding::Ri)?;
    let parsed = Touchstone::parse(&output, "out.s2p", LIMITS)?;
    assert_eq!(parsed.points, network.points);
    Ok(())
}

#[test]
fn touchstone_v2_explicit_two_port_order_and_noise_are_preserved() -> TouchstoneResult<()> {
    let example = "[Version] 2.1\n# GHz S RI R 50\n[Number of Ports] 2\n\
      [Two-Port Data Order] 12_21\n[Number of Frequencies] 2\n\
      [Number of Noise Frequencies] 1\n[Reference] 50 75\n[Network Data]\n\
      2 0.1 0 0.2 0 0.3 0 0.4 0\n\
      22 0.5 0 0.6 0 0.7 0 0.8 0\n\
      [Noise Data]\n4 0.7 0.64 69 19\n[End]\n";
    let file = Touchstone::parse(example, "x.ts", LIMITS)?;
    assert_eq!(file.references, [50., 75.]);
    assert_eq!(file.two_port_order, TwoPortOrder::N12BeforeN21);
    assert_eq!(file.points[0].matrix[1], Complex::real(0.2));
    assert_eq!(file.points[0].matrix[2], Complex::real(0.3));
    assert_eq!((file.noise[0].hz).to_bits(), (4e9_f64).to_bits());
    assert_eq!(
        (file.noise[0].resistance_ohms).to_bits(),
        (19.0_f64).to_bits()
    );
    for encoding in [Encoding::Ri, Encoding::Ma, Encoding::Db] {
        let roundtrip = Touchstone::parse(&file.write(Version::V2_1, encoding)?, "x.ts", LIMITS)?;
        assert_eq!(roundtrip.references, file.references);
        assert_eq!(roundtrip.noise.len(), file.noise.len());
        for (before, after) in file.points[0]
            .matrix
            .iter()
            .zip(&roundtrip.points[0].matrix)
        {
            assert!((before.re - after.re).abs() < 1e-12);
            assert!((before.im - after.im).abs() < 1e-12);
        }
    }
    Ok(())
}

#[test]
fn touchstone_v2_triangular_mixed_mode_matrix_can_be_continued_across_lines() -> TouchstoneResult<()>
{
    let text = "[Version] 2.1\n# MHz Y RI R 50\n\
      [Number of Ports] 3\n[Number of Frequencies] 1\n\
      [Reference] 50 50 50\n[Matrix Format] Lower\n\
      [Mixed-Mode Order] D1,2 S3 C1,2\n[Network Data]\n\
      100 1 0 2 0 3 0\n4 0 5 0 6 0\n[End]\n";
    let file = Touchstone::parse(text, "mixed.ts", LIMITS)?;
    assert_eq!(file.parameter, Parameter::Y);
    assert_eq!(file.matrix_format, MatrixFormat::Lower);
    assert_eq!(
        file.mixed_mode.as_deref(),
        Some(
            &[
                MixedPort::Differential(0, 1),
                MixedPort::Single(2),
                MixedPort::Common(0, 1)
            ][..]
        )
    );
    assert_eq!((file.points[0].hz).to_bits(), (1e8_f64).to_bits());
    assert_eq!(
        file.points[0].matrix,
        [
            Complex::real(1.),
            Complex::real(2.),
            Complex::real(4.),
            Complex::real(2.),
            Complex::real(3.),
            Complex::real(5.),
            Complex::real(4.),
            Complex::real(5.),
            Complex::real(6.),
        ]
    );
    let output = file.write(Version::V2_1, Encoding::Ri)?;
    let roundtrip = Touchstone::parse(&output, "mixed.ts", LIMITS)?;
    assert_eq!(roundtrip.points, file.points);
    assert_eq!(roundtrip.mixed_mode, file.mixed_mode);
    Ok(())
}

#[test]
fn touchstone_legacy_yz_normalization_and_v11_per_port_references() -> TouchstoneResult<()> {
    let y = "# MHz Y RI R 50\n1 0.5 0\n";
    let file = Touchstone::parse(y, "y.s1p", LIMITS)?;
    assert!((file.points[0].matrix[0].re - 0.01).abs() < 1e-15);
    let z = "# MHz Z RI R 100\n1 0.5 0\n";
    let file_z = Touchstone::parse(z, "z.s1p", LIMITS)?;
    assert_eq!(file_z.points[0].matrix[0], Complex::real(50.));
    let v11 = "# MHz S RI R 50 75\n1 0.1 0 0.2 0 0.3 0 0.4 0\n";
    let file_v11 = Touchstone::parse(v11, "v11.s2p", LIMITS)?;
    assert_eq!(file_v11.version, Version::V1_1);
    assert_eq!(file_v11.references, [50., 75.]);
    let roundtrip = Touchstone::parse(
        &file_v11.write(Version::V1_1, Encoding::Ri)?,
        "v11.s2p",
        LIMITS,
    )?;
    assert_eq!(roundtrip.points, file_v11.points);
    Ok(())
}

#[test]
fn touchstone_bounds_and_malformed_data_are_rejected() {
    let invalid = [
        "# GHz S RI R 50\n2 0 0 0 0 0 0 0 0\n1 0 0 0 0 0 0 0 0\n",
        "# GHz S RI R 50\n1 0 0 0 0 0 0 0\n",
        "[Version] 2.1\n# GHz S RI\n[Number of Ports] 2\n[Number of Frequencies] 1\n[Network Data]\n1 0 0 0 0 0 0 0 0\n[End]\n",
        "[Version] 2.1\n# GHz S RI\n[Number of Ports] 2\n[Two-Port Data Order] 21_12\n[Number of Frequencies] 1\n[Reference] 50 -50\n[Network Data]\n1 0 0 0 0 0 0 0 0\n[End]\n",
        "[Version] 2.1\n# GHz S RI\n[Number of Ports] 2\n[Two-Port Data Order] 21_12\n[Number of Frequencies] 1\n[Mixed-Mode Order] D1,2 D1,2\n[Network Data]\n1 0 0 0 0 0 0 0 0\n[End]\n",
    ];
    for sample in invalid {
        assert!(
            Touchstone::parse(sample, "fault.s2p", LIMITS).is_err(),
            "{sample}"
        );
    }
    assert!(
        Touchstone::parse(
            "# GHz S RI R 50\n1 0 0 0 0 0 0 0 0\n",
            "x.s2p",
            TouchstoneLimits {
                source_bytes: 8,
                ..LIMITS
            }
        )
        .is_err()
    );
    assert!(
        Touchstone::parse(
            "# GHz S RI R 50\n1 0 0 0 0 0 0 0 0\n",
            "x.s2p",
            TouchstoneLimits {
                numeric_bytes: 16,
                ..LIMITS
            }
        )
        .is_err()
    );
    assert!(
        Touchstone::parse(
            "# GHz S RI R 50\n1 0 0 0 0 0 0 0 0\n",
            "x.s2p",
            TouchstoneLimits { ports: 1, ..LIMITS }
        )
        .is_err()
    );
}

#[test]
fn v11_hybrid_normalization_uses_per_port_reference_scaling() -> TouchstoneResult<()> {
    let r1 = 50_f64;
    let r2 = 75_f64;
    let coupling = (r2 / r1).sqrt();
    let h = format!("# MHz H RI R {r1} {r2}\n1 2 0 {coupling:.17e} 0 {coupling:.17e} 0 1.5 0\n");
    let h = Touchstone::parse(&h, "hybrid.s2p", LIMITS)?;
    let want = [100., 1., 1., 0.02];
    for (&actual, expected) in h.points[0].matrix.iter().zip(want) {
        assert!((actual.re - expected).abs() <= 5e-15 * expected.abs().max(1.));
    }
    let roundtrip =
        Touchstone::parse(&h.write(Version::V1_1, Encoding::Ri)?, "hybrid.s2p", LIMITS)?;
    for (&a, &b) in h.points[0].matrix.iter().zip(&roundtrip.points[0].matrix) {
        assert!((a.re - b.re).abs() <= 5e-15 * a.re.abs().max(1.));
    }
    let coupling = (r1 / r2).sqrt();
    let g = format!(
        "# MHz G RI R {r1} {r2}\n1 0.5 0 {coupling:.17e} 0 {coupling:.17e} 0 1.3333333333333333 0\n"
    );
    let g = Touchstone::parse(&g, "hybrid.s2p", LIMITS)?;
    for (&actual, expected) in g.points[0].matrix.iter().zip([0.01, 1., 1., 100.]) {
        assert!((actual.re - expected).abs() <= 5e-15 * expected.abs().max(1.));
    }
    Ok(())
}

#[test]
fn touchstone_writer_enforces_encoded_byte_budget_during_output() -> TouchstoneResult<()> {
    let network = Touchstone::parse("# Hz S RI R 50\n1 0.5 0\n2 0.25 0\n", "one.s1p", LIMITS)?;
    for limit in [0, 1, 32, 96] {
        let cap = TouchstoneLimits {
            source_bytes: limit,
            ..LIMITS
        };
        assert!(
            network
                .write_with_limits(Version::V1_0, Encoding::Ri, cap)
                .is_err()
        );
    }
    let text = network.write_with_limits(Version::V1_0, Encoding::Ri, LIMITS)?;
    let exact = TouchstoneLimits {
        source_bytes: text.len(),
        ..LIMITS
    };
    assert_eq!(
        network.write_with_limits(Version::V1_0, Encoding::Ri, exact)?,
        text
    );
    let too_small = TouchstoneLimits {
        source_bytes: text.len() - 1,
        ..LIMITS
    };
    assert!(
        network
            .write_with_limits(Version::V1_0, Encoding::Ri, too_small)
            .is_err()
    );
    Ok(())
}

#[test]
fn legacy_v1_two_port_noise_uses_normalized_resistance_and_independent_grid() -> TouchstoneResult<()>
{
    let text = "# GHz S RI R 50\n\
        1 0.1 0 0.2 0 0.3 0 0.4 0\n\
        2 0.1 0 0.2 0 0.3 0 0.4 0\n\
        0.5 1.2 0.3 45 0.5\n\
        1 1.1 0.2 30 0.4\n";
    let network = Touchstone::parse(text, "noisy.s2p", LIMITS)?;
    assert_eq!(network.points.len(), 2);
    assert_eq!(network.noise.len(), 2);
    assert_eq!((network.noise[0].hz).to_bits(), (5e8_f64).to_bits());
    assert_eq!(
        (network.noise[0].resistance_ohms).to_bits(),
        (25.0_f64).to_bits()
    );
    assert_eq!(
        (network.noise[1].resistance_ohms).to_bits(),
        (20.0_f64).to_bits()
    );
    let text = network.write(Version::V1_0, Encoding::Ri)?;
    let recovered = Touchstone::parse(&text, "noisy.s2p", LIMITS)?;
    assert_eq!(recovered.points, network.points);
    assert_eq!(recovered.noise.len(), 2);
    for (a, b) in recovered.noise.iter().zip(&network.noise) {
        assert!((a.resistance_ohms - b.resistance_ohms).abs() < 1e-12);
        assert!((a.optimum_reflection.re - b.optimum_reflection.re).abs() < 1e-12);
    }
    Ok(())
}

#[test]
fn modern_to_legacy_two_port_order_is_not_symmetric() -> TouchstoneResult<()> {
    let text = "[Version] 2.1\n# Hz S RI R 50\n[Number of Ports] 2\n[Two-Port Data Order] 12_21\n[Number of Frequencies] 1\n[Network Data]\n1 1 0 2 0 3 0 4 0\n[End]\n";
    let model = Touchstone::parse(text, "input.ts", LIMITS)?;
    for version in [Version::V1_0, Version::V1_1] {
        let encoded = model.write(version, Encoding::Ri)?;
        let decoded = Touchstone::parse(&encoded, "output.s2p", LIMITS)?;
        assert_eq!(decoded.points, model.points);
    }
    Ok(())
}

#[test]
fn legacy_nport_rows_and_carriage_returns() -> TouchstoneResult<()> {
    use std::fmt::Write as _;
    for n in [3_u32, 4, 5, 6, 8, 9] {
        let mut text = format!(
            "[Version] 2.1\n# Hz S RI R 50\n[Number of Ports] {n}\n[Number of Frequencies] 1\n[Network Data]\n100"
        );
        for i in 0..n * n {
            assert!(write!(text, " {} 0", i + 1).is_ok());
        }
        text.push_str("\n[End]\n");
        let model = Touchstone::parse(&text, "input.ts", LIMITS)?;
        let encoded = model.write(Version::V1_0, Encoding::Ri)?;
        let field_counts = encoded
            .lines()
            .skip(1)
            .map(|line| line.split_ascii_whitespace().count())
            .collect::<Vec<_>>();
        let mut expected = Vec::new();
        for row in 0..n {
            for col in (0..n).step_by(4) {
                expected.push((2 * (n - col).min(4) + u32::from(row == 0 && col == 0)) as usize);
            }
        }
        assert_eq!(field_counts, expected);
        let decoded =
            Touchstone::parse(&encoded.replace('\n', "\r"), &format!("out.s{n}p"), LIMITS)?;
        assert_eq!(decoded.points, model.points);
    }
    Ok(())
}

#[test]
fn mutated_model_and_noise_budget_are_checked_before_export() -> TouchstoneResult<()> {
    let text = "[Version] 2.1\n# Hz S RI R 50\n[Number of Ports] 2\n[Two-Port Data Order] 21_12\n[Number of Frequencies] 1\n[Number of Noise Frequencies] 1\n[Network Data]\n100 1 0 2 0 3 0 4 0\n[Noise Data]\n50 1 1 45 1\n[End]\n";
    let mut model = Touchstone::parse(text, "input.ts", LIMITS)?;
    assert!(
        Touchstone::parse(
            text,
            "input.ts",
            TouchstoneLimits {
                numeric_bytes: 96,
                ..LIMITS
            }
        )
        .is_err()
    );
    model.mixed_mode = Some(vec![MixedPort::Single(usize::MAX), MixedPort::Single(1)]);
    assert!(model.write(Version::V2_1, Encoding::Ri).is_err());
    model.mixed_mode = None;
    model.noise[0].hz = 101.;
    assert!(model.write(Version::V2_1, Encoding::Ri).is_err());
    Ok(())
}
