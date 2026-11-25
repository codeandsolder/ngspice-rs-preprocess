use anyhow::{Context, Result};
use model_statistics::{Invocation, Realization};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, Options, PreparedBundle, Request, SourceInput, prepare,
};
use std::fs;

fn sampled(source: &str, realization: Option<Realization>) -> Result<PreparedBundle> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.net");
    fs::write(path.as_path(), source)?;
    prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(path),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options {
            statistical_sample: realization,
            ..Options::default()
        },
        &Cancellation::default(),
    )
}

fn invocations(bundle: &PreparedBundle) -> Result<Vec<Invocation>> {
    let mut result = Vec::new();
    for artifact in bundle.artifacts() {
        let source = std::str::from_utf8(&artifact.bytes)?;
        for line in source.lines() {
            let mut remaining = line;
            while let Some(start) = remaining.find(model_statistics::PREPARED_PREFIX) {
                remaining = &remaining[start..];
                let (name, rest) = remaining.split_once('(').context("marker arguments")?;
                let (metadata, _) = rest.split_once(')').context("marker terminator")?;
                // The test uses literal tolerance operands; actual source
                // marker arguments can contain arbitrary scalar expressions.
                let args = metadata
                    .split(',')
                    .map(|word| word.trim().parse::<f64>())
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let (invocation, _) = Invocation::decode(name, &args)?;
                result.push(invocation);
                remaining = rest;
            }
        }
    }
    Ok(result)
}

#[test]
fn selected_sample_preserves_scope_and_requires_occurrence_binding() -> Result<()> {
    let source = "* statistical source\n*#SIMETRIX\nV1 a 0 1\nX1 a 0 CELL\nX2 a 0 CELL\n.subckt CELL p n\nR1 p n {1k*GAUSS(0.2)*GAUSSL(0.1)}\nR2 p n {2k*UNIF(0.05)}\n.ends CELL\n.tran 1u 1m sweep monte 16\n.end\n";
    let realization = Realization {
        seed: u64::MAX,
        sample: 7,
    };
    assert!(sampled(source, None).is_err());
    assert!(
        sampled(
            source,
            Some(Realization {
                sample: 16,
                ..realization
            })
        )
        .is_err()
    );
    let bundle = sampled(source, Some(realization))?;
    let plan = bundle
        .analysis_plan
        .as_ref()
        .context("missing sample plan")?;
    assert_eq!(plan.statistical_sample, Some(realization));
    assert!(plan.requires_binding());
    assert!(plan.validate_unbound_execution().is_err());
    assert_eq!(plan.native_requests().count(), 1);
    let calls = invocations(&bundle)?;
    assert!(calls.len() >= 3, "{calls:?}");
    assert!(calls.iter().all(|call| call.realization == realization));
    // Reordering unrelated statements must not renumber calls on R1.
    let reordered = source.replace(
        "R1 p n {1k*GAUSS(0.2)*GAUSSL(0.1)}\nR2 p n {2k*UNIF(0.05)}",
        "R2 p n {2k*UNIF(0.05)}\nR1 p n {1k*GAUSS(0.2)*GAUSSL(0.1)}",
    );
    let summarize = |calls: Vec<Invocation>| {
        let mut values = calls
            .into_iter()
            .map(|call| (call.function.name(), call.site))
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    };
    assert_eq!(
        summarize(calls),
        summarize(invocations(&sampled(&reordered, Some(realization))?)?)
    );
    Ok(())
}

#[test]
fn nominal_distribution_arguments_are_lazy_but_arity_is_validated() -> Result<()> {
    let source = "* nominal distribution\n*#SIMETRIX\nV1 a 0 1\nR1 a 0 {1k*UNIF2(missing,undefined)}\n.op\n.end\n";
    let bundle = sampled(source, None)?;
    let output = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(!output.contains("missing"));
    assert!(!output.contains("undefined"));
    assert!(
        sampled(
            &source.replace("UNIF2(missing,undefined)", "UNIF2(missing)"),
            None
        )
        .is_err()
    );
    assert!(sampled(source, Some(Realization { seed: 3, sample: 0 })).is_ok());
    Ok(())
}
