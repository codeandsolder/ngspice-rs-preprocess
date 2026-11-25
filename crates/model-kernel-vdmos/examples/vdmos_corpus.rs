//! Inspect a local plaintext LTspice/ngspice model library without bundling
//! third-party catalog sources, and validate their device preparation.
//!
//! `cargo +stable run -p model-kernel-vdmos --example vdmos_corpus -- path/to/standard.mos`

use model_kernel_vdmos::{Bias, Instance, Polarity, compile_ngspice};
use model_normalize_ngspice::{
    linked_source::SourceLinker, parameter_scope::ModelLibrary, vdmos_card::VdmosPolarity,
};
use std::{collections::BTreeMap, error::Error, path::PathBuf};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("missing model catalog path")?,
    );
    let source = SourceLinker::default().link(&path)?;
    let library = ModelLibrary::parse(&source.bytes)?;
    let total_cards = library.models.len();
    let mut vdmos_count = 0;
    let mut prepared_count = 0;
    let mut failures = BTreeMap::<String, Vec<String>>::new();
    let mut annotations = BTreeMap::<String, usize>::new();
    for (index, scoped) in library.models.iter().enumerate() {
        let parsed = match library.resolve_vdmos(index, &[]) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => continue,
            Err(error) => {
                vdmos_count += 1;
                failures
                    .entry(format!("source: {error}"))
                    .or_default()
                    .push(scoped.card.name.clone());
                continue;
            }
        };
        vdmos_count += 1;
        let polarity = match parsed.polarity {
            VdmosPolarity::N => Polarity::N,
            VdmosPolarity::P => Polarity::P,
        };
        for annotation in parsed.annotations {
            *annotations
                .entry(annotation.name.to_ascii_lowercase())
                .or_default() += 1;
        }
        let evaluate = || -> Result<()> {
            let prepared = compile_ngspice(polarity, parsed.parameters.clone())?;
            let device = prepared.bind(Instance::default())?;
            let sign = if matches!(polarity, Polarity::N) {
                1.0
            } else {
                -1.0
            };
            let eval = device.evaluate(Bias {
                vgs: sign * 5.0,
                vds: sign * 0.5,
                vds_external: sign * 0.5,
                vdiode: -sign * 0.5,
                temperature: 300.15,
                ..Bias::default()
            })?;
            if !eval.channel.value.is_finite() || !eval.diode.current.value.is_finite() {
                return Err("non-finite evaluation".into());
            }
            Ok(())
        };
        match evaluate() {
            Ok(()) => prepared_count += 1,
            Err(error) => failures
                .entry(format!("model: {error}"))
                .or_default()
                .push(scoped.card.name.clone()),
        }
    }
    println!(
        "source={} cards={total_cards} VDMOS={vdmos_count} prepared={prepared_count}",
        path.display()
    );
    for (name, count) in annotations {
        println!("annotation {name}: {count}");
    }
    for (cause, models) in failures {
        let samples = models
            .iter()
            .take(8)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "failure count={} cause={cause} samples={samples}",
            models.len()
        );
    }
    if prepared_count != vdmos_count {
        return Err(format!(
            "{} of {vdmos_count} VDMOS cards were not prepared",
            vdmos_count - prepared_count
        )
        .into());
    }
    Ok(())
}
