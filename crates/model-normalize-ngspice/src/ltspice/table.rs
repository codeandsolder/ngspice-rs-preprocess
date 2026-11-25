//! Numeric lookup tables: native, endpoint-clamped piecewise interpolation.

use crate::lexical::Balanced;
use anyhow::{Result, ensure};
use std::{fmt::Write as _, ops::Range};

/// Parse a call's comma-separated arguments, retaining nested expression groups.
pub(super) fn arguments(text: &[u8], name: Range<usize>) -> Result<Vec<&[u8]>> {
    let mut tail = text[name.end..].trim_ascii_start();
    ensure!(
        tail.first() == Some(&b'('),
        "missing function argument list"
    );
    tail = &tail[1..];
    let mut balanced = Balanced::default();
    let mut start = 0;
    let mut args = Vec::new();
    for (index, &byte) in tail.iter().enumerate() {
        match byte {
            b')' if balanced.top_level() => {
                let value = tail[start..index].trim_ascii();
                if !value.is_empty() || !args.is_empty() {
                    ensure!(!value.is_empty(), "missing final function argument");
                    args.push(value);
                }
                return Ok(args);
            }
            b',' if balanced.top_level() => {
                let value = tail[start..index].trim_ascii();
                ensure!(!value.is_empty(), "missing function argument");
                args.push(value);
                start = index + 1;
            }
            _ => balanced.advance(byte)?,
        }
    }
    anyhow::bail!("unterminated function argument list")
}

pub(super) fn point_count(text: &[u8], name: Range<usize>) -> Result<usize> {
    let args = arguments(text, name)?;
    ensure!(
        args.len() >= 3 && args.len() % 2 == 1,
        "LTspice table requires one input and at least one coordinate pair"
    );
    ensure!(
        args.iter().all(|value| !value.contains(&b'"')),
        "LTspice string/file table requires typed source lowering"
    );
    Ok(args.len() / 2)
}

/// Emit a balanced binary search, including duplicate-knot selection.
/// The endpoint checks precede the search, matching source behavior even
/// for nonmonotone input. No sorting or approximation of the input table.
fn segment(out: &mut String, first: usize, last: usize) -> std::fmt::Result {
    if first == last {
        return write!(out, "b{first}");
    }
    if last - first == 1 {
        return write!(
            out,
            "ternary_fcn(a{first}==a{last},b{first},b{first}+(b{last}-b{first})*((x-a{first})/(a{last}-a{first})))"
        );
    }
    let mid = first.midpoint(last);
    write!(out, "ternary_fcn(x<=a{mid},")?;
    segment(out, first, mid)?;
    out.push(',');
    segment(out, mid, last)?;
    out.push(')');
    Ok(())
}

pub(super) fn definition(points: usize) -> Result<String> {
    let mut out = String::from("(x");
    for index in 0..points {
        write!(&mut out, ",a{index},b{index}")?;
    }
    out.push_str(") {");
    let last = points - 1;
    if points == 1 {
        out.push_str("b0");
    } else {
        write!(
            &mut out,
            "ternary_fcn(x<a0,b0,ternary_fcn(x>a{last},b{last},"
        )?;
        segment(&mut out, 0, last)?;
        out.push_str("))");
    }
    out.push('}');
    Ok(out)
}
