//! Named independent-source grammars and exact prepared waveform capture.

use super::{Item, parse_items, render_native_subcircuit, source_kind};
use crate::{
    Translation, TranslationKind, normalized_statement, statement_tokens, validate_name,
    validate_node,
};
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::waveform::{
    Forcing, Knot, Period, PreparedWaveform, PulseEdges, PulseSpec, Repeat, ResolvedWaveform,
    Scalar, SineDelay, SymbolicPoint, WaveformSpec,
};
use model_preprocess_simplis::{LinkedPreprocessedNetlist, parameter_assignments};
use std::{collections::BTreeMap, fmt::Write as _};

struct Fields(BTreeMap<String, String>);

impl Fields {
    fn parse(tokens: &[String]) -> Result<Self> {
        let mut fields = BTreeMap::new();
        let text = tokens.join(" ");
        for (name, value) in parameter_assignments(&text)? {
            let name = name.to_ascii_uppercase();
            ensure!(
                fields.insert(name.clone(), value.to_owned()).is_none(),
                "duplicate waveform field {name}"
            );
        }
        Ok(Self(fields))
    }

    fn optional(&mut self, name: &str) -> Result<Option<Scalar>> {
        self.0
            .remove(name)
            .map(|text| super::pwls::expression_text(&text))
            .transpose()
    }

    fn required(&mut self, name: &str) -> Result<Scalar> {
        self.optional(name)?
            .with_context(|| format!("missing waveform field {name}"))
    }

    fn default(&mut self, name: &str, value: &str) -> Result<Scalar> {
        Ok(self.optional(name)?.unwrap_or_else(|| value.to_owned()))
    }

    fn flag(&mut self, name: &str, default: &str) -> Result<Scalar> {
        let raw = self.0.remove(name).unwrap_or_else(|| default.to_owned());
        match raw.to_ascii_uppercase().as_str() {
            "YES" => Ok("1".to_owned()),
            "NO" => Ok("0".to_owned()),
            _ => super::pwls::expression_text(&raw),
        }
    }

    fn period(&mut self) -> Result<Period> {
        match (self.optional("PERIOD")?, self.optional("FREQ")?) {
            (Some(value), None) => Ok(Period::Seconds(value)),
            (None, Some(value)) => Ok(Period::Frequency(value)),
            _ => bail!("waveform requires exactly one of FREQ or PERIOD"),
        }
    }

    fn finish(&self) -> Result<()> {
        ensure!(
            self.0.is_empty(),
            "unsupported waveform fields: {:?}",
            self.0.keys().collect::<Vec<_>>()
        );
        Ok(())
    }
}

fn pulse(fields: &mut Fields, pwls: bool) -> Result<WaveformSpec> {
    let first = if pwls {
        fields.default("V1", "1")?
    } else {
        fields.required("V1")?
    };
    let second = if pwls {
        fields.default("V2", "-1")?
    } else {
        fields.required("V2")?
    };
    let period = fields.period()?;
    let delay = fields.default("DELAY", "0")?;
    let hold_before_delay = fields.flag("OFF_UNTIL_DELAY", if pwls { "1" } else { "0" })?;
    let edges = if pwls {
        PulseEdges::Trapezoidal {
            rise: fields.optional("RISE")?,
            width: fields.optional("WIDTH")?,
            fall: fields.optional("FALL")?,
            positive_slopes: true,
        }
    } else if let Some(duty) = fields.optional("DRATIO")? {
        PulseEdges::Rectangular { duty }
    } else {
        PulseEdges::Trapezoidal {
            rise: Some(fields.required("T_RISE")?),
            width: Some(fields.required("PWIDTH")?),
            fall: Some(fields.required("T_FALL")?),
            positive_slopes: false,
        }
    };
    Ok(WaveformSpec::Pulse(PulseSpec {
        first,
        second,
        period,
        delay,
        hold_before_delay,
        edges,
    }))
}

fn saw(fields: &mut Fields) -> Result<WaveformSpec> {
    Ok(WaveformSpec::Saw {
        first: fields.required("V1")?,
        last: fields.required("V2")?,
        period: fields.period()?,
        delay: fields.default("DELAY", "0")?,
        hold_before_delay: fields.flag("OFF_UNTIL_DELAY", "0")?,
    })
}

fn sine(fields: &mut Fields) -> Result<WaveformSpec> {
    let delay = match (fields.optional("TDELAY")?, fields.optional("PDELAY")?) {
        (Some(value), None) => SineDelay::Seconds(value),
        (None, Some(value)) => SineDelay::Degrees(value),
        _ => bail!("SIN requires exactly one of TDELAY or PDELAY"),
    };
    Ok(WaveformSpec::Sine {
        offset: fields.required("VOFFSET")?,
        amplitude: fields.required("APEAK")?,
        frequency: fields.required("FREQ")?,
        delay,
        damping: fields.default("DAMP_COEF", "0")?,
        hold_before_delay: fields.flag("OFF_UNTIL_DELAY", "0")?,
    })
}

fn pwl(fields: &mut Fields) -> Result<WaveformSpec> {
    let segments = fields.required("NSEG")?;
    // Structural arity is the number of supplied pairs. A symbolic NSEG is
    // checked at occurrence binding rather than allocating from a scalar.
    ensure!(
        fields.0.len() >= 4 && fields.0.len().is_multiple_of(2),
        "PWL requires complete time/value pairs"
    );
    let count = fields.0.len() / 2;
    let mut points = Vec::with_capacity(count);
    for index in 0..count {
        points.push(SymbolicPoint {
            time: fields.required(&format!("X{index}"))?,
            value: fields.required(&format!("Y{index}"))?,
        });
    }
    Ok(WaveformSpec::PiecewiseLinear { segments, points })
}

pub(super) struct Parsed {
    pub(super) name: String,
    pub(super) positive: String,
    pub(super) negative: String,
    pub(super) spec: WaveformSpec,
    idle_in_pop: Scalar,
    kind: TranslationKind,
    pwls: bool,
}

fn parse(statement: &str) -> Result<Option<Parsed>> {
    // Avoid parsing unrelated device/model expressions merely to discover
    // that they are not named independent-source waveforms.
    let mut words = statement.split_ascii_whitespace();
    if !words.next().is_some_and(|name| {
        name.as_bytes()
            .first()
            .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'V' | b'I'))
    }) || !words.nth(2).is_some_and(|shape| {
        ["PUL", "SAW", "PWL", "SIN", "PWLS"]
            .iter()
            .any(|name| shape.eq_ignore_ascii_case(name))
    }) {
        return Ok(None);
    }
    let normalized = normalized_statement(statement);
    let tokens = statement_tokens(&normalized)?;
    let [name, positive, negative, shape, rest @ ..] = tokens.as_slice() else {
        return Ok(None);
    };
    if !name
        .as_bytes()
        .first()
        .is_some_and(|byte| matches!(byte.to_ascii_uppercase(), b'V' | b'I'))
    {
        return Ok(None);
    }
    let shape = shape.to_ascii_uppercase();
    if !matches!(shape.as_str(), "PUL" | "SAW" | "PWL" | "SIN" | "PWLS") {
        return Ok(None);
    }
    if matches!(shape.as_str(), "PWL" | "SIN")
        && !rest.first().is_some_and(|token| {
            token.contains('=') || rest.get(1).is_some_and(|next| next.starts_with('='))
        })
    {
        return Ok(None);
    }
    source_kind(name)?;
    validate_name(name)?;
    validate_node(positive)?;
    validate_node(negative)?;
    let pwls = shape == "PWLS";
    let mut fields = if pwls {
        let [pulse, body @ .., end] = rest else {
            bail!("PWLS PUL requires END")
        };
        ensure!(
            pulse.eq_ignore_ascii_case("PUL") && end.eq_ignore_ascii_case("END"),
            "unsupported PWLS shape or missing END"
        );
        Fields::parse(body)?
    } else {
        Fields::parse(rest)?
    };
    let idle_in_pop = match shape.as_str() {
        "PWLS" => "0".to_owned(),
        // Aperiodic PWL is always idle in the source periodic analysis.
        "PWL" => "1".to_owned(),
        _ => fields.flag("IDLE_IN_POP", "0")?,
    };
    let (spec, kind) = match shape.as_str() {
        "PUL" | "PWLS" => (pulse(&mut fields, pwls)?, TranslationKind::PulseSource),
        "SAW" => (saw(&mut fields)?, TranslationKind::SawSource),
        "SIN" => (sine(&mut fields)?, TranslationKind::SineSource),
        "PWL" => (pwl(&mut fields)?, TranslationKind::PiecewiseLinearSource),
        _ => unreachable!("waveform shape checked"),
    };
    fields.finish()?;
    Ok(Some(Parsed {
        name: name.clone(),
        positive: positive.clone(),
        negative: negative.clone(),
        spec,
        idle_in_pop,
        kind,
        pwls,
    }))
}

struct ReferencePwl {
    knots: Vec<Knot>,
    delay: f64,
    repeat_at: Option<f64>,
}

impl ReferencePwl {
    fn push(&mut self, knot: Knot) -> Result<()> {
        ensure!(
            knot.time.is_finite() && knot.time >= 0.,
            "native PWL time shift overflows"
        );
        ensure!(
            self.knots.last().is_none_or(|last| last.time < knot.time),
            "native PWL cannot represent shifted knot times distinctly"
        );
        self.knots.push(knot);
        Ok(())
    }

    fn build(knots: &[Knot], repeat: Option<Repeat>, dc: f64) -> Result<Self> {
        let first = knots.first().context("missing waveform knots")?;
        let Some(cycle) = repeat else {
            let delay = first.time.min(0.);
            let mut result = Self {
                knots: Vec::new(),
                delay,
                repeat_at: None,
            };
            for knot in knots {
                result.push(Knot {
                    time: knot.time - delay,
                    ..*knot
                })?;
            }
            return Ok(result);
        };
        let prefix = cycle.hold.is_none() && cycle.origin > 0.;
        let mut result = Self {
            knots: Vec::new(),
            delay: if prefix { 0. } else { cycle.origin },
            repeat_at: Some(if prefix { cycle.origin } else { 0. }),
        };
        if prefix {
            // Native PWL holds before TD. Emit the rest of the preceding
            // cycle, then repeat from the actual positive boundary.
            result.push(Knot {
                time: 0.,
                left: dc,
                right: dc,
            })?;
            for knot in knots {
                let time = cycle.origin + (knot.time - cycle.period);
                if time > 0. {
                    ensure!(time < cycle.origin, "native PWL prefix loses a knot");
                    result.push(Knot { time, ..*knot })?;
                }
            }
        }
        let shift = if prefix { cycle.origin } else { 0. };
        for (index, knot) in knots.iter().enumerate() {
            let left = if index == 0 && !prefix {
                cycle.hold.unwrap_or(knot.right)
            } else {
                knot.left
            };
            result.push(Knot {
                time: knot.time + shift,
                left,
                ..*knot
            })?;
        }
        result.push(Knot {
            time: cycle.period + shift,
            left: first.left,
            right: first.left,
        })?;
        Ok(result)
    }

    fn write(&self, output: &mut String) -> Result<()> {
        output.push_str(" PWL(");
        for knot in &self.knots {
            write!(output, " {:.17e} {:.17e}", knot.time, knot.left)?;
            if knot.left.to_bits() != knot.right.to_bits() {
                write!(output, " {:.17e} {:.17e}", knot.time, knot.right)?;
            }
        }
        output.push(')');
        if let Some(repeat) = self.repeat_at {
            write!(output, " r={repeat:.17e}")?;
        }
        write!(output, " td={:.17e}", self.delay)?;
        Ok(())
    }
}

impl Parsed {
    fn declaration(&self, artifact: &str, scope: &[String]) -> PreparedWaveform {
        PreparedWaveform {
            artifact: artifact.to_owned(),
            scope: scope.to_vec(),
            source: self.name.clone(),
            spec: self.spec.clone(),
            dc_override: None,
            idle_in_pop: self.idle_in_pop.clone(),
        }
    }

    fn constant(&self) -> Result<Option<ResolvedWaveform>> {
        let mut unbound = false;
        let result = self.declaration("", &[]).resolve(|text| {
            if let Some(value) = super::pwls::constant(text)? {
                Ok(value)
            } else {
                unbound = true;
                bail!("unbound waveform parameter")
            }
        });
        match result {
            Ok(value) => Ok(Some(value)),
            Err(_) if unbound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn strict_native_reference(resolved: &ResolvedWaveform) -> bool {
        match &resolved.forcing {
            Forcing::PiecewiseLinear { knots, repeat } => {
                ReferencePwl::build(knots, *repeat, resolved.dc).is_ok_and(|reference| {
                    reference
                        .knots
                        .iter()
                        .all(|knot| knot.left.to_bits() == knot.right.to_bits())
                })
            }
            Forcing::Sine {
                damping,
                frequency,
                delay,
                amplitude,
                hold_before_delay,
                ..
            } => {
                let elapsed = if *hold_before_delay && *delay > 0. {
                    0.
                } else {
                    -delay
                };
                *damping >= 0.
                    && (elapsed * frequency).is_finite()
                    && (*amplitude == 0. || (amplitude * (-damping * elapsed).exp()).is_finite())
            }
        }
    }

    fn placeholder(&self, dc: f64) -> String {
        format!(
            "* mandatory prepared waveform binding for {}\n{} {} {} DC {dc:.17e}",
            self.name, self.name, self.positive, self.negative
        )
    }

    fn write_reference(&self, resolved: &ResolvedWaveform) -> Result<String> {
        let mut output = String::new();
        write!(
            output,
            "{} {} {} DC {:.17e}",
            self.name, self.positive, self.negative, resolved.dc
        )?;
        match &resolved.forcing {
            Forcing::PiecewiseLinear { knots, repeat } => {
                ReferencePwl::build(knots, *repeat, resolved.dc)?.write(&mut output)?;
            }
            Forcing::Sine {
                offset,
                amplitude,
                frequency,
                delay,
                damping,
                hold_before_delay,
            } => {
                let gated = *hold_before_delay && *delay > 0.;
                let shift = if gated { *delay } else { 0. };
                let elapsed = shift - delay;
                let peak = if *amplitude == 0. {
                    0.
                } else {
                    amplitude * (-damping * elapsed).exp()
                };
                let phase = (elapsed * frequency % 1.) * 360.;
                ensure!(
                    peak.is_finite() && phase.is_finite(),
                    "nonfinite native sine coefficients"
                );
                write!(
                    output,
                    " SIN({offset:.17e} {peak:.17e} {frequency:.17e} {shift:.17e} {damping:.17e} {phase:.17e})"
                )?;
            }
        }
        output.push('\n');
        Ok(output)
    }
}

/// Low-level native reference lowering. Symbolic named sources require
/// the unified prepared bundle, not an unresolved numeric approximation.
pub(super) fn translate(statement: &str, scope: &str) -> Result<Option<Translation>> {
    let Some(parsed) = parse(statement)? else {
        return Ok(None);
    };
    if parsed.pwls {
        return super::pwls::translate(&parsed, scope).map(Some);
    }
    let resolved = parsed
        .constant()?
        .context("symbolic source requires the prepared waveform binding API")?;
    Ok(Some(Translation {
        bytes: parsed.write_reference(&resolved)?.into_bytes(),
        kind: parsed.kind,
    }))
}

fn capture_items(
    items: &mut [Item],
    artifact: &str,
    scope: &[String],
    output: &mut Vec<PreparedWaveform>,
) -> Result<bool> {
    let mut changed = false;
    let mut embedded = false;
    let mut control = false;
    for item in items {
        match item {
            Item::Statement(statement) => {
                if let Some(start) = crate::syntax::file_boundary(statement) {
                    embedded = start;
                    continue;
                }
                let first = statement
                    .split_ascii_whitespace()
                    .next()
                    .unwrap_or_default();
                if first.eq_ignore_ascii_case(".control") {
                    control = true;
                } else if first.eq_ignore_ascii_case(".endc") {
                    control = false;
                } else if !embedded
                    && !control
                    && let Some(parsed) = parse(statement)?
                {
                    let resolved = parsed.constant()?;
                    output.push(parsed.declaration(artifact, scope));
                    if !parsed.pwls {
                        *statement = match resolved {
                            Some(resolved) if Parsed::strict_native_reference(&resolved) => {
                                parsed.write_reference(&resolved)?.trim_end().to_owned()
                            }
                            Some(resolved) => parsed.placeholder(resolved.dc),
                            None => parsed.placeholder(0.),
                        };
                        changed = true;
                    }
                }
            }
            Item::Subcircuit(block) if !embedded && !control => {
                changed |= capture_items(
                    &mut block.body,
                    artifact,
                    std::slice::from_ref(&block.name),
                    output,
                )?;
            }
            Item::Subcircuit(_) => {}
        }
    }
    Ok(changed)
}

/// Capture source waveforms after hierarchy linkage.
///
/// Retain independent
/// source names and terminals in the native electrical artifact. Static
/// raw declarations with strictly increasing native knots also have a
/// reference representation. Exact discontinuities and symbolic raw
/// declarations retain a DC-only row until the mandatory typed binding.
///
/// # Errors
/// Invalid source grammar, static domain errors or malformed scope.
pub fn capture_artifact_waveforms(
    bytes: &mut Vec<u8>,
    artifact: &str,
    output: &mut Vec<PreparedWaveform>,
) -> Result<()> {
    let mut items = parse_items(std::str::from_utf8(bytes)?)?;
    if !capture_items(&mut items, artifact, &[], output)? {
        return Ok(());
    }
    let mut text = String::new();
    for item in items {
        match item {
            Item::Statement(statement) => writeln!(text, "{statement}")?,
            Item::Subcircuit(block) => render_native_subcircuit(&block, &mut text)?,
        }
    }
    *bytes = text.into_bytes();
    Ok(())
}

/// Capture named source declarations across the linked artifact closure.
///
/// # Errors
/// Invalid source grammar, static domain errors or malformed scope.
pub fn capture_linked_waveforms(
    linked: &mut LinkedPreprocessedNetlist,
) -> Result<Vec<PreparedWaveform>> {
    let mut waveforms = Vec::new();
    capture_artifact_waveforms(&mut linked.root_bytes, "deck.cir", &mut waveforms)?;
    for library in &mut linked.libraries {
        capture_artifact_waveforms(&mut library.bytes, &library.filename, &mut waveforms)?;
    }
    Ok(waveforms)
}
