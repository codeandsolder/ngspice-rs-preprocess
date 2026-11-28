use model_kernel_api::Result;
use model_kernel_ekv::{
    Geometry, Model, Polarity,
    geometry::{Defaults, Overrides},
};

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 1e-14 * a.abs().max(b.abs()), "{a} != {b}");
}

#[test]
fn diffusion_defaults_follow_finger_count() -> Result<()> {
    let defaults = Defaults {
        hdif: Some(0.5e-6),
        gate_sheet_specified: true,
        areas: [0.; 2],
    };
    for (m, ad, source, pd, ps) in [
        (1., 4., 4., 10., 6.),
        (2., 2., 4., 5., 10.),
        (3., 8. / 3., 8. / 3., 20. / 3., 20. / 3.),
        (4., 2., 3., 5., 7.5),
    ] {
        let g = Geometry {
            length: 1e-6,
            width: 4e-6,
            parallel: m,
            series: 2.,
        };
        let d = defaults.resolve(g, Overrides::default())?;
        close(d.areas[0], ad * 1e-12);
        close(d.areas[1], source * 1e-12);
        close(d.perimeters[0], pd * 1e-6);
        close(d.perimeters[1], ps * 1e-6);
        close(d.squares[0], 0.125);
        close(d.squares[1], 2.);
        close(d.squares[2], 0.125);
        close(d.squares[3], 0.);
    }
    let g = Geometry {
        length: 1e-6,
        width: 4e-6,
        parallel: 1.5,
        series: 1.,
    };
    assert!(defaults.resolve(g, Overrides::default()).is_err());
    let resolved = defaults.resolve(
        g,
        Overrides {
            drain_area: Some(0.),
            source_area: Some(0.),
            drain_perimeter: Some(0.),
            source_perimeter: Some(0.),
            squares: [Some(0.), None, None, None],
        },
    )?;
    assert_eq!(resolved.areas.map(f64::to_bits), [0; 2]);
    assert_eq!(resolved.perimeters.map(f64::to_bits), [0; 2]);
    close(resolved.squares[0], 0.);
    Ok(())
}

#[test]
fn segmented_geometry_preserves_physical_dimensions() -> Result<()> {
    let model = Model::reference_defaults(Polarity::N);
    for parallel in [0.5, 1., 4.] {
        for series in [0.5, 1., 3.] {
            let g = Geometry {
                length: 1e-6,
                width: 4e-6,
                parallel,
                series,
            };
            let segmented = model.prepare_segmented(g, 300.15)?;
            let aggregate = model.prepare(
                Geometry {
                    length: (g.length + model.dl).mul_add(series, -model.dl),
                    width: (g.width + model.dw).mul_add(parallel, -model.dw),
                    ..g
                },
                300.15,
            )?;
            let a = segmented.evaluate([1., 0.75, 0., 0.])?;
            let b = aggregate.evaluate([1., 0.75, 0., 0.])?;
            for i in 0..4 {
                close(a.current[i], b.current[i]);
                close(a.charge[i], b.charge[i]);
            }
        }
    }
    Ok(())
}
