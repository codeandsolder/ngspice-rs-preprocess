//! `SIMetrix` PWLS PUL (not the SIMPLIS PUL primitive).
//!
//! Native repeated PWL preserves both source identity and breakpoint scheduling.
//! Static expressions are folded when possible, otherwise scoped parameters
//! retain the native netlist's normal instance binding semantics.
use super::{Item, lower_simetrix_behavioral_expression, waveforms::Parsed};
use crate::{Translation, TranslationKind};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    param_expression::{SpiceExpression, SpiceExpressionContext},
    waveform::{Period, PulseEdges, PulseSpec, WaveformSpec},
};
use model_preprocess_command::{Resource, WorkBudget};
use std::{collections::BTreeSet, fmt::Write as _};

/// Select a compact namespace absent from the entire source scope.
/// Scan each source statement once; a collision does not cause another
/// whole-source search or make every generated name arbitrarily long.
pub(super) fn scope_prefix<'a>(
    scopes: impl IntoIterator<Item = &'a [Item]>,
    header: Option<&str>,
    initial: &str,
    budget: &WorkBudget,
) -> Result<String> {
    ensure!(
        !initial.is_empty() && initial.is_ascii(),
        "invalid generated namespace stem"
    );
    let mut occupied = OccupiedNamespaces::default();
    if let Some(header) = header {
        occupied.scan(header, initial, budget)?;
    }
    for items in scopes {
        occupied.scan_items(items, initial, budget)?;
    }
    if !occupied.base {
        return Ok(initial.to_owned());
    }
    // The ordered set contains only canonical decimal suffixes. The
    // first missing integer is bounded by the number of source matches.
    let mut ordinal = 0_usize;
    for used in occupied.ordinals {
        if used != ordinal {
            break;
        }
        ordinal = ordinal
            .checked_add(1)
            .context("namespace ordinal overflow")?;
    }
    let separator = if initial.ends_with('_') { "" } else { "_" };
    Ok(format!("{initial}{separator}{ordinal}_"))
}

#[derive(Default)]
struct OccupiedNamespaces {
    base: bool,
    ordinals: BTreeSet<usize>,
}

impl OccupiedNamespaces {
    fn scan(&mut self, text: &str, initial: &str, budget: &WorkBudget) -> Result<()> {
        budget.charge(Resource::ScannedBytes, text.len())?;
        budget.charge(Resource::Statements, 1)?;
        let bytes = text.as_bytes();
        let prefix = initial.as_bytes();
        // Both namespace stems are fixed, nonempty ASCII identifiers.
        for (start, window) in bytes.windows(prefix.len()).enumerate() {
            if !window.eq_ignore_ascii_case(prefix) {
                continue;
            }
            self.base = true;
            let suffix = &bytes[start + prefix.len()..];
            let suffix = if initial.ends_with('_') {
                suffix
            } else if let Some(suffix) = suffix.strip_prefix(b"_") {
                suffix
            } else {
                continue;
            };
            let digits = suffix
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if digits == 0 || suffix.get(digits) != Some(&b'_') || digits > 1 && suffix[0] == b'0' {
                continue;
            }
            // Values too large for usize cannot collide with a candidate.
            let Ok(ordinal) = std::str::from_utf8(&suffix[..digits])?.parse::<usize>() else {
                continue;
            };
            budget.charge(Resource::EvaluationSteps, 1)?;
            budget.charge(Resource::GeneratedNodes, 1)?;
            self.ordinals.insert(ordinal);
        }
        Ok(())
    }

    fn scan_items(&mut self, items: &[Item], initial: &str, budget: &WorkBudget) -> Result<()> {
        let _depth = budget.enter()?;
        for item in items {
            match item {
                Item::Statement(statement) => self.scan(statement, initial, budget)?,
                Item::Subcircuit(block) => {
                    self.scan(&block.header, initial, budget)?;
                    self.scan_items(&block.body, initial, budget)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Unbound(bool);

impl SpiceExpressionContext for Unbound {
    fn symbol(&mut self, name: &str) -> Result<f64> {
        self.0 = true;
        bail!("unbound parameter {name}")
    }

    fn function(&mut self, name: &str, _args: &[SpiceExpression]) -> Result<f64> {
        self.0 = true;
        bail!("unbound function {name}")
    }
}

pub(super) fn constant(expression: &str, budget: &WorkBudget) -> Result<Option<f64>> {
    let compiled = SpiceExpression::parse_with_budget(expression, budget)?;
    let mut resolver = Unbound::default();
    match compiled.evaluate_with_budget(&mut resolver, budget) {
        Ok(value) => Ok(Some(value)),
        Err(_) if resolver.0 => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn expression_text(text: &str, budget: &WorkBudget) -> Result<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(text);
    let text = lower_simetrix_behavioral_expression(inner, budget)?;
    SpiceExpression::parse_with_budget(&text, budget)?;
    Ok(text)
}

struct Scalar {
    text: String,
    value: Option<f64>,
}

impl Scalar {
    fn argument(&self) -> String {
        if self.value.is_some() {
            self.text.clone()
        } else {
            format!("{{{}}}", self.text)
        }
    }
}

impl std::fmt::Display for Scalar {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.value.is_some_and(f64::is_sign_negative) {
            write!(formatter, "({})", self.text)
        } else {
            formatter.write_str(&self.text)
        }
    }
}

struct Parameters<'a> {
    budget: &'a WorkBudget,
    prefix: String,
    output: String,
}

impl Parameters<'_> {
    fn define(&mut self, name: &str, expression: &str) -> Result<Scalar> {
        let value = constant(expression, self.budget)
            .with_context(|| format!("invalid PWLS PUL {name}"))?;
        let text = if let Some(value) = value {
            format!("{value:.17e}")
        } else {
            let name = format!("{}{name}", self.prefix);
            writeln!(self.output, ".param {name}={{{expression}}}")?;
            name
        };
        Ok(Scalar { text, value })
    }
}

struct Pulse {
    low: Scalar,
    high: Scalar,
    period: Scalar,
    rise: Scalar,
    width: Scalar,
    fall: Scalar,
    delay: Scalar,
    off: Scalar,
}

impl Pulse {
    fn prepare(spec: &PulseSpec, params: &mut Parameters<'_>) -> Result<Self> {
        let low = params.define("v1", &spec.first)?;
        let high = params.define("v2", &spec.second)?;
        let period = match &spec.period {
            Period::Seconds(period) => params.define("period", period)?,
            Period::Frequency(freq) => {
                let frequency = params.define("freq", freq)?;
                ensure!(
                    frequency.value.is_none_or(|value| value > 0.),
                    "PWLS PUL frequency must be positive"
                );
                params.define("period", &format!("1/({frequency})"))?
            }
        };
        ensure!(
            period.value.is_none_or(|value| value > 0.),
            "PWLS PUL period must be positive"
        );
        let PulseEdges::Trapezoidal {
            rise, width, fall, ..
        } = &spec.edges
        else {
            bail!("PWLS requires trapezoidal pulse edges")
        };
        let rise = match rise {
            Some(expression) => params.define("rise", expression)?,
            None => params.define("rise", &format!("{period}/1000"))?,
        };
        let fall = params.define("fall", fall.as_deref().unwrap_or(&rise.text))?;
        let width = match width {
            Some(expression) => params.define("width", expression)?,
            None => params.define("width", &format!("({period}-{rise}-{fall})/2"))?,
        };
        let delay = params.define("delay", &spec.delay)?;
        let off = params.define("off", &spec.hold_before_delay)?;
        ensure!(
            rise.value.is_none_or(|value| value > 0.) && fall.value.is_none_or(|value| value > 0.),
            "PWLS PUL rise and fall must be positive"
        );
        ensure!(
            width.value.is_none_or(|value| value >= 0.),
            "PWLS PUL width must be nonnegative"
        );
        Ok(Self {
            low,
            high,
            period,
            rise,
            width,
            fall,
            delay,
            off,
        })
    }

    fn emit(
        &self,
        name: &str,
        positive: &str,
        negative: &str,
        params: &mut Parameters<'_>,
    ) -> Result<()> {
        let Self {
            low,
            high,
            period,
            rise,
            width,
            fall,
            delay,
            off,
        } = self;
        let high_end = params.define("high_end", &format!("{rise}+{width}"))?;
        let end = params.define("end", &format!("{high_end}+{fall}"))?;
        // Static expressions use the arithmetic-only native numparam subset.
        // sgn(x) is -1, 0 or 1; max(0,sgn(x)) selects the positive side.
        let valid = params.define(
            "valid",
            &format!("max(0,sgn({period}))*max(0,sgn({rise}))*max(0,sgn({fall}))*(1+min(0,sgn({width})))*(1+min(0,sgn({period}-{end})))"),
        )?;
        ensure!(
            valid.value.is_none_or(|value| value > 0.0),
            "PWLS PUL rise, width and fall exceed period"
        );
        let gate = params.define("gate", &format!("max(0,sgn({delay}))*abs(sgn({off}))"))?;
        let shift = if let (Some(delay), Some(period), Some(off)) =
            (delay.value, period.value, off.value)
        {
            let shift = if off.abs() > 0. && delay > 0. {
                delay
            } else {
                -(-delay).rem_euclid(period)
            };
            params.define("shift", &format!("{shift:.17e}"))?
        } else {
            params.define(
                "shift",
                &format!("{delay}+(1-{gate})*{period}*floor((0-({delay}))/{period})"),
            )?
        };
        // At t=0 a positive shift holds V1, otherwise evaluate the phase.
        let fraction = params.define(
            "fraction",
            &format!("max(0,min(1,min((0-({shift}))/{rise},({end}+{shift})/{fall})))"),
        )?;
        let dc = params.define("dc", &format!("{low}*(1-{fraction})+{high}*{fraction}"))?;

        // With zero plateau width or zero idle duration, place the otherwise
        // redundant knot on a nonzero linear edge. This preserves the shape
        // while keeping native PWL times strictly increasing without epsilon.
        let has_width = params.define("has_width", &format!("max(0,sgn({width}))"))?;
        let has_idle = params.define("has_idle", &format!("max(0,sgn({period}-{end}))"))?;
        let t1 = params.define("t1", &format!("{rise}*((1+{has_width})/2)"))?;
        let y1 = params.define(
            "y1",
            &format!("{low}*((1-{has_width})/2)+{high}*((1+{has_width})/2)"),
        )?;
        let t3 = params.define("t3", &format!("{end}-{fall}*(1-{has_idle})/2"))?;
        let y3 = params.define(
            "y3",
            &format!("{low}*((1+{has_idle})/2)+{high}*((1-{has_idle})/2)"),
        )?;
        // Invalid symbolic timing cannot yield a valid native repeat period.
        let last = params.define("last", &format!("{period}*{valid}-(1-{valid})"))?;
        write!(
            params.output,
            "{name} {positive} {negative} DC {} PWL(0 {}",
            dc.argument(),
            low.argument()
        )?;
        for (time, value) in [(&t1, &y1), (&high_end, high), (&t3, &y3), (&last, low)] {
            write!(params.output, " {} {}", time.argument(), value.argument())?;
        }
        writeln!(params.output, ") r=0 td={}", shift.argument())?;
        Ok(())
    }
}

pub(super) fn translate(parsed: &Parsed, scope: &str, budget: &WorkBudget) -> Result<Translation> {
    let WaveformSpec::Pulse(spec) = &parsed.spec else {
        bail!("PWLS requires pulse forcing")
    };
    let mut prefix = scope.to_owned();
    for byte in parsed.name.bytes() {
        write!(prefix, "{:02x}", byte.to_ascii_lowercase())?;
    }
    prefix.push('_');
    let mut params = Parameters {
        budget,
        prefix,
        output: String::new(),
    };
    let pulse = Pulse::prepare(spec, &mut params)?;
    pulse.emit(
        &parsed.name,
        &parsed.positive,
        &parsed.negative,
        &mut params,
    )?;
    Ok(Translation {
        bytes: params.output.into_bytes(),
        kind: TranslationKind::PulseSource,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_namespace_is_case_insensitive_and_budgeted() -> Result<()> {
        use model_preprocess_command::{Cancellation, WorkLimits};
        let source = format!(
            "* __L17_{}\n.subckt __l17_0_cell a b\nR1 a b {{__L17_1_value}}\n.ends\n* __l17_03_ __l17_4_\n",
            "_".repeat(100_000)
        );
        let items = crate::syntax::parse_items(&source)?;
        let budget = WorkBudget::default();
        assert_eq!(
            scope_prefix([items.as_slice()], None, "__l17_", &budget)?,
            "__l17_2_"
        );
        let header = ".subckt __L17_2_cell a b";
        assert_eq!(
            scope_prefix([items.as_slice()], Some(header), "__l17_", &budget)?,
            "__l17_3_"
        );
        let limited = WorkBudget::new(
            WorkLimits {
                scanned_bytes: 64,
                ..WorkLimits::default()
            },
            Cancellation::default(),
        );
        assert!(scope_prefix([items.as_slice()], None, "__l17_", &limited).is_err());
        assert!(limited.check().is_err());
        assert_eq!(scope_prefix([&[][..]], None, "__l17_", &budget)?, "__l17_");
        Ok(())
    }

    fn lower(source: &str) -> Result<String> {
        let result =
            super::super::waveforms::translate(source, "pulse_test_", &WorkBudget::default())?
                .context("missing translation")?;
        assert_eq!(result.kind, TranslationKind::PulseSource);
        Ok(String::from_utf8(result.bytes)?)
    }

    type NumericWaveform = (f64, Vec<(f64, f64)>, f64);

    fn numeric_waveform(source: &str) -> Result<NumericWaveform> {
        let output = lower(source)?;
        let (_, tail) = output.split_once(" DC ").context("missing DC")?;
        let (dc, tail) = tail.split_once(" PWL(").context("missing PWL")?;
        let (pairs, shift) = tail
            .split_once(") r=0 td=")
            .context("missing repeat/delay")?;
        let values = pairs
            .split_whitespace()
            .map(str::parse)
            .collect::<std::result::Result<Vec<f64>, _>>()?;
        ensure!(values.len() == 10, "wrong number of PWL coordinates");
        let knots: Vec<(f64, f64)> = values
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| (*pair).into())
            .collect::<Vec<_>>();
        ensure!(
            knots.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "non-increasing knots"
        );
        Ok((dc.parse()?, knots, shift.trim().parse()?))
    }

    fn check(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-12 * expected.abs().max(1.0),
            "{actual} != {expected}"
        );
    }

    #[test]
    fn default_edges_and_level_polarity_match_vendor() -> Result<()> {
        let (dc, knots, delay) = numeric_waveform("Vx p 0 PWLS PUL FREQ=1k END")?;
        check(dc, 1.0);
        check(delay, 0.0);
        let (_, period_knots, _) = numeric_waveform("Vx p 0 PWLS PUL PERIOD=1m END")?;
        assert_eq!(knots, period_knots);
        for ((time, value), (want_time, want_value)) in knots.into_iter().zip([
            (0., 1.),
            (1e-6, -1.),
            (0.5e-3, -1.),
            (0.501e-3, 1.),
            (1e-3, 1.),
        ]) {
            check(time, want_time);
            check(value, want_value);
        }
        Ok(())
    }

    #[test]
    fn signed_phase_gate_and_multiple_periods() -> Result<()> {
        for (delay, off, dc, shift) in [
            ("0.75m", "1", 2., 0.75e-3),
            ("0.75m", "0", 6., -0.25e-3),
            ("-0.25m", "1", 6., -0.25e-3),
            ("1.25m", "0", 2., -0.75e-3),
            ("1.25m", "1", 2., 1.25e-3),
            ("-0.5u", "0", 4., -0.5e-6),
        ] {
            let (initial, _, start) = numeric_waveform(&format!(
                "Ix 0 p PWLS PUL V1=2 V2=6 FREQ=1k DELAY={delay} OFF_UNTIL_DELAY={off} END"
            ))?;
            check(initial, dc);
            check(start, shift);
        }
        Ok(())
    }

    #[test]
    fn triangle_and_no_idle_keep_real_edges() -> Result<()> {
        for (width, fall, expected) in [
            (
                "0",
                "0.2m",
                [
                    (0., 2.),
                    (0.05e-3, 4.),
                    (0.1e-3, 6.),
                    (0.3e-3, 2.),
                    (1e-3, 2.),
                ],
            ),
            (
                "0.5m",
                "0.4m",
                [
                    (0., 2.),
                    (0.1e-3, 6.),
                    (0.6e-3, 6.),
                    (0.8e-3, 4.),
                    (1e-3, 2.),
                ],
            ),
        ] {
            let (_, knots, _) = numeric_waveform(&format!(
                "Vx p 0 PWLS PUL V1=2 V2=6 FREQ=1k RISE=0.1m FALL={fall} WIDTH={width} END"
            ))?;
            for ((time, value), (want_time, want_value)) in knots.into_iter().zip(expected) {
                check(time, want_time);
                check(value, want_value);
            }
        }
        Ok(())
    }

    #[test]
    fn rejects_malformed_and_invalid_timing() {
        for args in [
            "FREQ=0 END",
            "FREQ=-1k END",
            "FREQ=1k RISE=0 END",
            "FREQ=1k FALL=-1u END",
            "FREQ=1k WIDTH=-1u END",
            "FREQ=1k WIDTH=1m END",
            "FREQ=1k FREQ=2k END",
            "FREQ=1k PERIOD=1m END",
            "PERIOD=0 END",
            "FREQ=1k GARBAGE END",
            "FREQ=1k DAMP_COEF=1 END",
            "FREQ=1k CYCLES=2 END",
            "FREQ=1k",
        ] {
            assert!(lower(&format!("Vx p 0 PWLS PUL {args}")).is_err(), "{args}");
        }
    }

    #[test]
    fn scoped_parameter_expressions_are_retained() -> Result<()> {
        let output = lower(
            "Vx p n PWLS PUL V1={lo} V2={hi} FREQ = {2*freq} DELAY={IFF(usephase,angle/360/freq,delay)} OFF_UNTIL_DELAY={off} END",
        )?;
        assert!(
            output.contains(".param pulse_test_7678_freq={2*freq}"),
            "{output}"
        );
        assert!(
            output.contains("Vx p n DC {pulse_test_7678_dc} PWL("),
            "{output}"
        );
        assert!(
            output.contains("r=0 td={pulse_test_7678_shift}"),
            "{output}"
        );
        assert!(!output.contains("IFF("));
        Ok(())
    }
}
