//! Qualified native analysis forms from HSPICE E-2010.12.
use crate::analysis_plan::{
    AnalysisKind, analysis_kind, arguments, command, native_analysis_shape,
};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::model_card::parse_spice_constant;

fn transient(mut args: Vec<String>) -> Result<String> {
    let uic = args
        .last()
        .is_some_and(|arg| arg.eq_ignore_ascii_case("UIC"));
    if uic {
        args.pop();
    }
    if let [_, _, start, eq, value] = args.as_slice()
        && start.eq_ignore_ascii_case("START")
        && eq == "="
    {
        let value = value.clone();
        args.truncate(2);
        args.push(value);
    } else if let [_, stop, start, max] = args.as_slice() {
        // Four positional operands are legacy SPICE start/DELMAX only
        // if the last two are smaller than the first segment's stop.
        let stop = parse_spice_constant(stop)?;
        let start = parse_spice_constant(start)?;
        let max = parse_spice_constant(max)?;
        ensure!(
            start >= 0. && start < stop && max > 0. && max < stop,
            "HSPICE multipoint transient requires an analysis schedule"
        );
    } else {
        ensure!(
            args.len() == 2,
            "unsupported HSPICE transient sweep or schedule"
        );
    }
    ensure!(
        native_analysis_shape(AnalysisKind::Transient, &args),
        "invalid HSPICE transient arguments"
    );
    let mut native = format!(".tran {}", args.join(" "));
    if uic {
        native.push_str(" UIC");
    }
    Ok(native)
}

pub fn lower(text: &str) -> Result<String> {
    let name = command(text);
    let kind = analysis_kind(&name);
    let args = arguments(text)?;
    if kind == AnalysisKind::Transient {
        return transient(args);
    }
    ensure!(
        native_analysis_shape(kind, &args),
        "unqualified HSPICE analysis {text}"
    );
    if kind == AnalysisKind::DcSweep {
        for sweep in args.as_chunks::<4>().0 {
            let source = sweep[0]
                .as_bytes()
                .first()
                .context("empty DC sweep target")?;
            ensure!(
                matches!(source.to_ascii_uppercase(), b'V' | b'I'),
                "HSPICE parameter/temperature sweep requires explicit analysis binding"
            );
        }
    }
    Ok(text.to_owned())
}

/// Ensure the requested sweep target is actually an independent source,
/// rather than a parameter whose name happens to begin with V or I.
pub fn validate_dc_sources(rows: &[(String, usize)]) -> Result<()> {
    let mut sources = std::collections::BTreeSet::new();
    let mut depth = 0_usize;
    for (row, _) in rows.iter().skip(1) {
        let name = command(row);
        match name.as_str() {
            ".SUBCKT" => depth += 1,
            ".ENDS" => depth = depth.checked_sub(1).context("unmatched .ENDS")?,
            _ if depth == 0 && name.starts_with(['V', 'I']) => {
                sources.insert(name);
            }
            _ => (),
        }
    }
    for (row, _) in rows.iter().skip(1) {
        if command(row) == ".DC" {
            for sweep in arguments(row)?.as_chunks::<4>().0 {
                ensure!(
                    sources.contains(&sweep[0].to_ascii_uppercase()),
                    "HSPICE DC target {} is not a root independent source; parameter sweeps require explicit analysis binding",
                    sweep[0]
                );
            }
        }
    }
    Ok(())
}
