//! Numeric lookup tables: native, endpoint-clamped piecewise interpolation.

use crate::lexical::Balanced;
use anyhow::{Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::ops::Range;

/// Count comma-separated scalar arguments without allocating a temporary
/// vector. Nested argument groups stay opaque.
pub(super) fn point_count(text: &[u8], name: Range<usize>, budget: &WorkBudget) -> Result<usize> {
    let mut tail = text[name.end..].trim_ascii_start();
    ensure!(
        tail.first() == Some(&b'('),
        "missing function argument list"
    );
    tail = &tail[1..];
    let mut balanced = Balanced::default();
    let mut start = 0;
    let mut count = 0;
    let mut charged = 0;
    for (index, &byte) in tail.iter().enumerate() {
        // Bound nested/repeated scans cumulatively without locking per byte.
        if index - charged == 4096 {
            budget.charge(Resource::ScannedBytes, index - charged)?;
            charged = index;
        }
        match byte {
            b'"' => anyhow::bail!("LTspice string/file table requires typed source lowering"),
            b',' | b')' if balanced.top_level() => {
                let value = tail[start..index].trim_ascii();
                ensure!(!value.is_empty(), "missing table argument");
                count += 1;
                if byte == b')' {
                    budget.charge(Resource::ScannedBytes, index + 1 - charged)?;
                    ensure!(
                        count >= 3 && count % 2 == 1,
                        "LTspice table requires one input and at least one coordinate pair"
                    );
                    return Ok(count / 2);
                }
                start = index + 1;
            }
            _ => balanced.advance(byte)?,
        }
    }
    anyhow::bail!("unterminated function argument list")
}

fn append(out: &mut String, text: &str, budget: &WorkBudget) -> Result<()> {
    budget.charge(Resource::EmittedBytes, text.len())?;
    out.push_str(text);
    Ok(())
}

/// Emit a balanced binary search, including duplicate-knot selection.
/// The endpoint checks precede the search, matching source behavior even
/// for nonmonotone input. No sorting or approximation of the input table.
fn segment(
    out: &mut String,
    first: usize,
    last: usize,
    static_phase: bool,
    budget: &WorkBudget,
) -> Result<()> {
    let _depth = budget.enter()?;
    budget.charge(Resource::GeneratedNodes, 1)?;
    if first == last {
        return append(out, &format!("b{first}"), budget);
    }
    if last - first == 1 {
        let equal = if static_phase {
            format!("b{first}")
        } else {
            format!("1e200*(x-a{first})")
        };
        return append(
            out,
            &format!(
                "ternary_fcn(a{first}==a{last},{equal},b{first}+(b{last}-b{first})*((x-a{first})/(a{last}-a{first})))"
            ),
            budget,
        );
    }
    let mid = first.midpoint(last);
    append(out, &format!("ternary_fcn(x<=a{mid},"), budget)?;
    segment(out, first, mid, static_phase, budget)?;
    append(out, ",", budget)?;
    segment(out, mid, last, static_phase, budget)?;
    append(out, ")", budget)
}

pub(super) fn definition(points: usize, static_phase: bool, budget: &WorkBudget) -> Result<String> {
    ensure!(points > 0, "missing LTspice table coordinates");
    budget.charge(Resource::GeneratedNodes, points.saturating_mul(2))?;
    let mut out = String::new();
    append(&mut out, "(x", budget)?;
    for index in 0..points {
        append(&mut out, &format!(",a{index},b{index}"), budget)?;
    }
    append(&mut out, ") {", budget)?;
    let last = points - 1;
    if points == 1 {
        append(&mut out, "b0", budget)?;
    } else {
        append(
            &mut out,
            &format!("ternary_fcn(x<a0,b0,ternary_fcn(x>a{last},b{last},"),
            budget,
        )?;
        segment(&mut out, 0, last, static_phase, budget)?;
        append(&mut out, "))", budget)?;
    }
    append(&mut out, "}", budget)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_preprocess_command::{Cancellation, WorkLimits};

    #[test]
    fn large_tables_fail_before_full_helper_emission() -> Result<()> {
        let limits = WorkLimits {
            emitted_bytes: 128,
            ..WorkLimits::default()
        };
        let budget = WorkBudget::new(limits, Cancellation::default());
        let error = definition(1000, false, &budget)
            .err()
            .ok_or_else(|| anyhow::anyhow!("table bypassed emission budget"))?;
        ensure!(format!("{error:#}").contains("EmittedBytes budget exceeded"));
        ensure!(budget.check().is_err());
        Ok(())
    }

    #[test]
    fn table_argument_scan_consumes_shared_work() -> Result<()> {
        let limits = WorkLimits {
            scanned_bytes: 20,
            ..WorkLimits::default()
        };
        let budget = WorkBudget::new(limits, Cancellation::default());
        let text = b"table(a,0,1,2,3,4,5)";
        assert_eq!(point_count(text, 0..5, &budget)?, 3);
        ensure!(point_count(text, 0..5, &budget).is_err());
        ensure!(budget.check().is_err());
        Ok(())
    }
}
