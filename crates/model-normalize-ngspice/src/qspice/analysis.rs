//! Transient window projection. Retain symbolic timing until analysis binding;
//! the native simulator still owns integration and timestep selection.

use super::{Feature, literal, reject};
use crate::{
    lexical::{Edit, Statement, remove_spans, replace_spans, tokens},
    linked_source::{LinkedSource, SourceOrigin},
    model_card::{ModelParameter, parse_parameter_fields},
    param_expression::SpiceExpression,
    parameter_scope::declarations::Declarations,
};
use anyhow::{Context, Result};
use model_preprocess_command::{Resource, WorkBudget};
use std::ops::Range;

#[derive(Clone, Debug)]
struct Scalar {
    inner: String,
    value: Option<f64>,
}

impl Scalar {
    fn parse(raw: &str, origin: &SourceOrigin, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EmittedBytes, raw.len())?;
        let raw = raw.trim();
        if let Some(value) = literal(raw.as_bytes()) {
            return Ok(Self {
                inner: raw.to_owned(),
                value: Some(value),
            });
        }
        // Do not evaluate a parameter here: it can change between analyses.
        let inner = raw
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .or_else(|| raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(raw);
        SpiceExpression::parse_with_budget(raw, budget).map_err(|error| {
            reject(
                origin,
                Feature::TransientTiming,
                &format!("invalid timing expression: {error:#}"),
            )
        })?;
        Ok(Self {
            inner: inner.to_owned(),
            value: None,
        })
    }

    fn number(value: f64) -> Self {
        Self {
            inner: value.to_string(),
            value: Some(value),
        }
    }

    fn copy(&self, budget: &WorkBudget) -> Result<Self> {
        budget.charge(Resource::EmittedBytes, self.inner.len())?;
        Ok(self.clone())
    }

    fn emit(&self, budget: &WorkBudget) -> Result<String> {
        budget.charge(
            Resource::EmittedBytes,
            self.inner
                .len()
                .saturating_add(usize::from(self.value.is_none()) * 2),
        )?;
        Ok(if self.value.is_some() {
            self.inner.clone()
        } else {
            format!("{{{}}}", self.inner)
        })
    }

    fn select(self, other: Self, maximum: bool, budget: &WorkBudget) -> Result<Self> {
        Ok(match (self.value, other.value) {
            (Some(left), Some(right)) => {
                if (maximum && left >= right) || (!maximum && left <= right) {
                    self
                } else {
                    other
                }
            }
            _ if self.inner == other.inner => self,
            _ => {
                let function = if maximum { "max" } else { "min" };
                budget.charge(
                    Resource::EmittedBytes,
                    self.inner
                        .len()
                        .saturating_add(other.inner.len())
                        .saturating_add(10),
                )?;
                Self {
                    inner: format!("{function}(({}),({}))", self.inner, other.inner),
                    value: None,
                }
            }
        })
    }

    fn default_maximum(&self, budget: &WorkBudget) -> Result<Self> {
        if let Some(duration) = self.value {
            Ok(Self::number(duration / 1024.0))
        } else {
            budget.charge(Resource::EmittedBytes, self.inner.len().saturating_add(7))?;
            Ok(Self {
                inner: format!("({})/1024", self.inner),
                value: None,
            })
        }
    }

    fn same(&self, other: &Self) -> bool {
        match (self.value, other.value) {
            (Some(left), Some(right)) => left.to_bits() == right.to_bits(),
            _ => self.inner == other.inner,
        }
    }
}

pub(super) struct Settings<'a> {
    budget: &'a WorkBudget,
    maximum: Option<Scalar>,
    absolute: Option<bool>,
}

pub(super) fn is_options(name: &[u8]) -> bool {
    [b".option".as_slice(), b".options", b".opt"]
        .iter()
        .any(|key| name.eq_ignore_ascii_case(key))
}

pub(super) fn option_fields(text: &[u8], start: usize) -> Result<Vec<ModelParameter>> {
    let mut params = parse_parameter_fields(std::str::from_utf8(&text[start..])?, &[])?;
    for parameter in &mut params {
        parameter.span.name = parameter.span.name.start + start..parameter.span.name.end + start;
        if let Some(span) = &mut parameter.span.value {
            *span = span.start + start..span.end + start;
        }
    }
    Ok(params)
}

const fn timing_option(name: &str) -> bool {
    name.eq_ignore_ascii_case("maxstep") || name.eq_ignore_ascii_case("absolutetime")
}

fn positive(value: &Scalar, origin: &SourceOrigin, message: &str) -> Result<()> {
    if value.value.is_some_and(|v| v <= 0.0) {
        return Err(reject(origin, Feature::TransientTiming, message));
    }
    Ok(())
}

impl Settings<'_> {
    fn add(&mut self, parameter: &ModelParameter, origin: &SourceOrigin) -> Result<()> {
        if parameter.name.eq_ignore_ascii_case("maxstep") {
            let raw = parameter.expression.as_deref().ok_or_else(|| {
                reject(
                    origin,
                    Feature::TransientTiming,
                    "MAXSTEP requires an explicit positive value",
                )
            })?;
            let maximum = Scalar::parse(raw, origin, self.budget)?;
            positive(&maximum, origin, "MAXSTEP must be positive")?;
            if self
                .maximum
                .as_ref()
                .is_some_and(|previous| !previous.same(&maximum))
            {
                return Err(reject(
                    origin,
                    Feature::TransientTiming,
                    "conflicting MAXSTEP options",
                ));
            }
            self.maximum = Some(maximum);
        } else {
            let absolute = match parameter.expression.as_deref().map(str::as_bytes) {
                None | Some(b"1") => true,
                Some(b"0") => false,
                _ => {
                    return Err(reject(
                        origin,
                        Feature::TransientTiming,
                        "ABSOLUTETIME must be a flag or literal 0/1",
                    ));
                }
            };
            if self.absolute.is_some_and(|previous| previous != absolute) {
                return Err(reject(
                    origin,
                    Feature::TransientTiming,
                    "conflicting ABSOLUTETIME options",
                ));
            }
            self.absolute = Some(absolute);
        }
        Ok(())
    }

    fn native_times(&self, values: &[Scalar], origin: &SourceOrigin) -> Result<[String; 4]> {
        let stop = match values.get(1) {
            Some(duration) => values[0].copy(self.budget)?.select(
                duration.copy(self.budget)?,
                true,
                self.budget,
            )?,
            None => values[0].copy(self.budget)?,
        };
        positive(&stop, origin, "stop time must be positive")?;
        let start = match values.get(2) {
            Some(value) => value.copy(self.budget)?,
            None => Scalar::number(0.0),
        };
        if start.value != Some(0.0) && self.absolute != Some(true) {
            return Err(reject(
                origin,
                Feature::TransientTiming,
                "nonzero or symbolic start needs ABSOLUTETIME to match native output timestamps",
            ));
        }
        if let (Some(start), Some(stop)) = (start.value, stop.value)
            && start >= stop
        {
            return Err(reject(
                origin,
                Feature::TransientTiming,
                "start must precede stop",
            ));
        }
        let explicit = values.get(3).filter(|value| value.value != Some(0.0));
        if let (Some(field), Some(option)) = (explicit, &self.maximum)
            && !field.same(option)
        {
            return Err(reject(
                origin,
                Feature::TransientTiming,
                "conflicting .tran and .option MAXSTEP; precedence is not qualified",
            ));
        }
        let maximum = match explicit.or(self.maximum.as_ref()) {
            Some(value) => value.copy(self.budget)?,
            None => stop.default_maximum(self.budget)?,
        };
        positive(&maximum, origin, "maximum timestep is not representable")?;
        let interval =
            stop.copy(self.budget)?
                .select(maximum.copy(self.budget)?, false, self.budget)?;
        Ok([
            interval.emit(self.budget)?,
            stop.emit(self.budget)?,
            start.emit(self.budget)?,
            maximum.emit(self.budget)?,
        ])
    }

    fn tran(
        &self,
        statement: &Statement,
        origin: &SourceOrigin,
        fields: &[Range<usize>],
        edits: &mut Vec<Edit>,
        input: &[u8],
    ) -> Result<()> {
        let text = &statement.text;
        let mut args = &fields[1..];
        if args
            .last()
            .is_some_and(|span| text[span.clone()].eq_ignore_ascii_case(b"uic"))
        {
            args = &args[..args.len() - 1];
        }
        if !(1..=4).contains(&args.len()) {
            return Err(reject(
                origin,
                Feature::TransientTiming,
                "expected stop, or step stop [start [maxstep]], optionally followed by UIC",
            ));
        }
        let values = args
            .iter()
            .map(|span| {
                Scalar::parse(
                    std::str::from_utf8(&text[span.clone()])?,
                    origin,
                    self.budget,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        if values
            .iter()
            .any(|value| value.value.is_some_and(|v| v < 0.0))
        {
            return Err(reject(
                origin,
                Feature::TransientTiming,
                "negative transient time field",
            ));
        }
        let native = self.native_times(&values, origin)?;
        let replacements = args
            .iter()
            .enumerate()
            .map(|(index, span)| {
                let replacement = if index + 1 == args.len() {
                    native[index..].join(" ")
                } else {
                    native[index].clone()
                };
                (span.clone(), replacement.into_bytes())
            })
            .collect::<Vec<_>>();
        replace_spans(statement, input, &replacements, edits);
        Ok(())
    }

    pub(super) fn rewrite(
        &self,
        statement: &Statement,
        origin: &SourceOrigin,
        fields: &[Range<usize>],
        edits: &mut Vec<Edit>,
        input: &[u8],
    ) -> Result<bool> {
        let Some(first) = fields.first() else {
            return Ok(false);
        };
        let name = &statement.text[first.clone()];
        if name.eq_ignore_ascii_case(b".tran") {
            self.tran(statement, origin, fields, edits, input)?;
            return Ok(true);
        }
        if !is_options(name) {
            return Ok(false);
        }
        let params = option_fields(&statement.text, first.end)?;
        let removals = params
            .iter()
            .filter(|parameter| timing_option(&parameter.name))
            .map(|parameter| {
                parameter.span.name.start
                    ..parameter
                        .span
                        .value
                        .as_ref()
                        .map_or(parameter.span.name.end, |span| span.end)
            })
            .collect::<Vec<_>>();
        if removals.is_empty() {
            return Ok(false);
        }
        // Comment the complete card when every option was consumed.
        if removals.len() == params.len() {
            replace_spans(
                statement,
                input,
                &[(
                    0..statement.text.len(),
                    b"* QSPICE timing options consumed".to_vec(),
                )],
                edits,
            );
        } else {
            remove_spans(statement, input, &removals, edits);
        }
        Ok(true)
    }
}

pub(super) fn collect<'a>(
    statements: &[Statement],
    linked: &LinkedSource,
    has_title: bool,
    declarations: &'a Declarations,
) -> Result<Settings<'a>> {
    let mut settings = Settings {
        budget: declarations.budget(),
        maximum: None,
        absolute: None,
    };
    let mut first_option = None;
    let mut transient = false;
    for statement in statements {
        settings.budget.charge(Resource::Statements, 1)?;
        settings
            .budget
            .charge(Resource::ScannedBytes, statement.text.len())?;
        let fields = tokens(&statement.text)?;
        let Some(first) = fields.first() else {
            continue;
        };
        let name = &statement.text[first.clone()];
        if !name.eq_ignore_ascii_case(b".tran") && !is_options(name) {
            continue;
        }
        let origin = linked
            .origin(statement.line + usize::from(has_title))
            .context("missing QSPICE analysis location")?;
        if name.eq_ignore_ascii_case(b".tran") {
            if !declarations.unconditional_root(statement.line) {
                return Err(reject(
                    origin,
                    Feature::TransientTiming,
                    "conditional or subcircuit transient scheduling needs topology binding",
                ));
            }
            transient = true;
        } else {
            for parameter in option_fields(&statement.text, first.end)? {
                if !timing_option(&parameter.name) {
                    continue;
                }
                if !declarations.unconditional_root(statement.line) {
                    return Err(reject(
                        origin,
                        Feature::TransientTiming,
                        "conditional or subcircuit timing options need topology binding",
                    ));
                }
                first_option.get_or_insert(origin);
                settings.add(&parameter, origin)?;
            }
        }
    }
    if !transient && let Some(origin) = first_option {
        return Err(reject(
            origin,
            Feature::TransientTiming,
            "timing option has no linked transient analysis",
        ));
    }
    Ok(settings)
}
