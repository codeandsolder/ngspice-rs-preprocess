//! Independent BSIM1/2 series, channel and flicker noise generators.
//! No adjoint circuit transfer function is evaluated by the producer.
use super::{Layout, LegacyBsimPrepared, domain, junction};
use model_compact_intrinsic::evaluate_legacy_bsim_intrinsic;
use model_kernel_api::{
    Result,
    noise::{Injection, Sink, Source},
};

fn emit(sink: &mut dyn Sink, name: &str, density: f64, a: usize, b: usize) -> Result<()> {
    if a == b {
        return Ok(());
    }
    sink.push(Source {
        name,
        density,
        injections: &[
            Injection {
                row: a,
                re: 1.,
                im: 0.,
            },
            Injection {
                row: b,
                re: -1.,
                im: 0.,
            },
        ],
    })
}

pub fn evaluate(
    layout: Layout,
    prepared: &LegacyBsimPrepared,
    coordinates: &[f64],
    frequency: f64,
    sink: &mut dyn Sink,
) -> Result<()> {
    let p = &prepared.physical;
    let orientation = layout.orientation(p, coordinates);
    let point =
        evaluate_legacy_bsim_intrinsic(p, orientation.bias).map_err(|error| domain(&error))?;
    let m = p.geometry.multiplier;
    let thermal = 4. * 1.380_648_52e-23 * prepared.ambient * m;
    emit(
        sink,
        "rd",
        thermal * p.drain.series_conductance,
        layout.drain,
        0,
    )?;
    emit(
        sink,
        "rs",
        thermal * p.source.series_conductance,
        layout.source,
        2,
    )?;
    emit(
        sink,
        "id",
        thermal * (2. / 3.) * point.conductance[1].abs(),
        layout.drain,
        layout.source,
    )?;
    let law = &p.junction_law;
    let flicker = if law.flicker_coefficient == 0. {
        0.
    } else {
        let bulk_drain = orientation.sign * (coordinates[3] - coordinates[layout.drain]);
        let (junction_current, _) =
            junction::current(p.drain.saturation_current, bulk_drain, prepared.gmin)?;
        let drain_current = if orientation.forward {
            point.drain_current
        } else {
            -point.drain_current
        } - junction_current;
        // The reference coefficient is defined using Cox in F/cm², not F/m².
        let cox = p.oxide_capacitance_density * 1e-4;
        let log_density = law.flicker_exponent.mul_add(
            drain_current.abs().max(1e-38).ln(),
            2.0f64.mul_add(
                -cox.ln(),
                law.flicker_coefficient.ln() + m.ln()
                    - frequency.ln()
                    - p.geometry.effective_length.ln()
                    - p.geometry.effective_width.ln(),
            ),
        );
        log_density.exp()
    };
    emit(sink, "1overf", flicker, layout.drain, layout.source)
}
