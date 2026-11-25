//! Standalone HSPICE scalar semantics (as opposed to the `SIMetrix`
//! compatibility profile). Keep original arithmetic domains explicit in
//! native SPICE, including the signed logarithms and integer powers.
use super::HspiceExpressionKind;
use anyhow::{Result, bail, ensure};

const INTRINSICS: &[&str] = &[
    "LOG", "LOG10", "DB", "SQRT", "POW", "PWR", "INT", "NINT", "SIGN",
];

pub(super) fn is_intrinsic(name: &str) -> bool {
    INTRINSICS
        .iter()
        .any(|value| value.eq_ignore_ascii_case(name))
}

/// Consume one numeric token, excluding adjacent operators and identifiers.
/// HSPICE's X (mega), A (atto), and D exponents differ from native suffixes.
/// Preserve the original decimal significand rather than rounding via f64.
pub(super) fn number(input: &str) -> Result<(String, usize)> {
    let bytes = input.as_bytes();
    let mut end = 0;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
    }
    let mantissa_end = end;
    let mut exponent = None;
    if bytes
        .get(end)
        .is_some_and(|value| matches!(value, b'e' | b'E' | b'd' | b'D'))
    {
        let start = end + 1;
        let mut next = start;
        if bytes
            .get(next)
            .is_some_and(|value| matches!(value, b'+' | b'-'))
        {
            next += 1;
        }
        let digits = next;
        while bytes.get(next).is_some_and(u8::is_ascii_digit) {
            next += 1;
        }
        if next > digits {
            exponent = Some(&input[start..next]);
            end = next;
        }
    }
    let suffix_start = end;
    while bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
        end += 1;
    }
    let suffix = input[suffix_start..end].to_ascii_lowercase();
    let scale = if suffix.starts_with("meg") || suffix.starts_with('x') {
        Some(6)
    } else if suffix.starts_with("amps") {
        None
    } else {
        match suffix.as_bytes().first() {
            Some(b't') => Some(12),
            Some(b'g') => Some(9),
            Some(b'k') => Some(3),
            Some(b'm') => Some(-3),
            Some(b'u') => Some(-6),
            Some(b'n') => Some(-9),
            Some(b'p') => Some(-12),
            Some(b'f') => Some(-15),
            Some(b'a') => Some(-18),
            _ => None,
        }
    };
    ensure!(
        exponent.is_none() || scale.is_none(),
        "HSPICE numeric literal combines exponent and scale factor: {}",
        &input[..end]
    );
    let mut value = input[..mantissa_end].to_owned();
    if let Some(exponent) = exponent {
        value.push('e');
        value.push_str(exponent);
    } else if let Some(scale) = scale {
        use std::fmt::Write as _;
        write!(value, "e{scale}")?;
    }
    Ok((value, end))
}

fn truncate(value: &str) -> String {
    // Native behavioral sources do not implement INT(), unlike numparam.
    format!("(({value})<0?ceil({value}):floor({value}))")
}

fn check_budget(input: String, limit: usize) -> Result<String> {
    ensure!(
        input.len() <= limit,
        "source expression expansion byte budget exceeded"
    );
    Ok(input)
}

pub(super) fn lower_call(
    name: &str,
    args: &[String],
    kind: HspiceExpressionKind,
    limit: usize,
) -> Result<String> {
    // Bound allocation before repeating expressions, including nested power
    // chains. The final exact size check accounts for operator text.
    ensure!(
        args.iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_mul(5)
            <= limit,
        "source expression expansion byte budget exceeded"
    );
    let name = name.to_ascii_uppercase();
    let result = match (name.as_str(), args) {
        ("LOG", [value]) => format!("(sgn({value})*ln(abs({value})))"),
        ("LOG10", [value]) => format!("(sgn({value})*log10(abs({value})))"),
        ("DB", [value]) => format!("(20*sgn({value})*log10(abs({value})))"),
        ("SQRT", [value]) => format!("(sgn({value})*sqrt(abs({value})))"),
        ("POW", [base, exponent]) => format!("pow(({base}),{})", truncate(exponent)),
        ("PWR", [base, exponent]) if kind == HspiceExpressionKind::Behavioral => {
            // Native behavioral PWR has the signed law and its own
            // zero-base derivative handling. Numparam PWR is unsigned.
            format!("pwr({base},{exponent})")
        }
        ("PWR", [base, exponent]) => format!("(sgn({base})*pow(abs({base}),({exponent})))"),
        ("INT", [value]) => truncate(value),
        ("NINT", [value]) => format!("(({value})<0?ceil(({value})-0.5):floor(({value})+0.5))"),
        ("SIGN", [value, sign]) => format!("(abs({value})*sgn({sign}))"),
        _ => bail!("invalid HSPICE intrinsic arguments: {name}"),
    };
    check_budget(result, limit)
}

pub(super) fn infix_power(base: &str, exponent: &str, limit: usize) -> Result<String> {
    ensure!(
        base.len()
            .saturating_mul(5)
            .saturating_add(exponent.len().saturating_mul(4))
            <= limit,
        "source expression expansion byte budget exceeded"
    );
    // Unlike native HSPICE compatibility mode (which rounds), HSPICE's
    // documented negative-base exponent is truncated toward zero.
    check_budget(
        format!(
            "(({base})==0?0:pow(({base}),(({base})<0?{}:({exponent}))))",
            truncate(exponent)
        ),
        limit,
    )
}
