//! Resolve HSPICE MOS model families before native loading. Bin bounds
//! are source-only metadata; the selected card receives an exact name.
use super::bsim4_catalog;
use anyhow::{Context, Result, bail, ensure};
use model_normalize_ngspice::{
    linked_source::RootKind,
    model_card::{
        ModelCard, ModelParameter, instance_fields, instance_vector_fields, parse_model_card,
        parse_spice_constant,
    },
    parameter_scope::ModelLibrary,
    statement_token_spans,
};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::{
    lower_spice_expression,
    source_dialect::model_mapping::{HspiceMosJunctionMode, ModelSelection},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

#[derive(Default)]
pub struct Options {
    values: BTreeMap<String, String>,
}

impl Options {
    pub fn rewrite(
        &self,
        mut bind: impl FnMut(&str) -> Result<String>,
        budget: &WorkBudget,
    ) -> Result<Self> {
        let values = self
            .values
            .iter()
            .map(|(name, value)| {
                budget.charge(Resource::GeneratedNodes, 1)?;
                budget.charge(Resource::EmittedBytes, name.len())?;
                Ok((name.clone(), bind(value)?))
            })
            .collect::<Result<_>>()?;
        Ok(Self { values })
    }

    pub fn set(&mut self, name: &str, value: &str, budget: &WorkBudget) -> Result<()> {
        let value = lower_spice_expression(
            value,
            super::HSPICE,
            value.len().saturating_mul(8).saturating_add(128),
            budget,
        )?;
        ensure!(
            self.values.get(name).is_none_or(|old| old == &value),
            "conflicting HSPICE MOS option {name}"
        );
        self.values.insert(name.to_owned(), value);
        Ok(())
    }

    fn default_field(&self, field: &str) -> Option<&str> {
        match field {
            "L" => Some(self.values.get("DEFL").map_or("100u", String::as_str)),
            "W" => Some(self.values.get("DEFW").map_or("100u", String::as_str)),
            _ => self.values.get(&format!("DEF{field}")).map(String::as_str),
        }
    }

    fn flag(&self, key: &str, default: bool, library: &ModelLibrary) -> Result<bool> {
        self.values.get(key).map_or(Ok(default), |raw| {
            flag(library.evaluate_scalar(0, raw, &[])?, key)
        })
    }
}

fn flag(value: f64, field: &str) -> Result<bool> {
    match value {
        0. => Ok(false),
        1. => Ok(true),
        _ => bail!("invalid HSPICE {field} selector {value}"),
    }
}

fn version_mask(version: &str) -> Result<u8> {
    match version {
        "4.5" => Ok(1),
        "4.64" => Ok(2),
        "4.70" => Ok(4),
        _ => bail!("unqualified native BSIM4 version {version}"),
    }
}

fn supported(table: &[(&str, u8)], name: &str, mask: u8) -> bool {
    table
        .binary_search_by_key(&name, |(name, _)| *name)
        .is_ok_and(|index| table[index].1 & mask != 0)
}

const SOURCE_ONLY_FIELDS: &[&str] = &[
    "LMIN", "LMAX", "WMIN", "WMAX", "XLREF", "XWREF", "LMLT", "WMLT",
];

pub fn qualify_model(selection: &mut ModelSelection) -> Result<()> {
    let mask = version_mask(
        selection
            .effective_version
            .as_deref()
            .context("missing BSIM4 version")?,
    )?;
    let junction = selection
        .hspice_mos_junction()?
        .context("missing BSIM4 junction selection")?;
    ensure!(
        junction.mode == HspiceMosJunctionMode::BerkeleyIsolated,
        "HSPICE BSIM4 ACM mode requires distinct junction kernel"
    );
    let mut seen = BTreeSet::new();
    for field in &mut selection.card.parameters {
        field.name.make_ascii_uppercase();
        if field.name == "TREF" {
            "TNOM".clone_into(&mut field.name);
        }
        ensure!(
            seen.insert(field.name.clone()),
            "duplicate HSPICE BSIM4 field {}",
            field.name
        );
        ensure!(
            field.expression.is_some(),
            "BSIM4 model field requires a value"
        );
        match field.name.as_str() {
            // The junction selector was checked before visiting the other fields.
            "ACM" | "CALCACM" | "LEVEL" | "VERSION" | "XLREF" | "XWREF" | "LMLT" | "WMLT" => (),
            _ => ensure!(
                supported(bsim4_catalog::MODEL, &field.name, mask),
                "unqualified HSPICE BSIM4 model parameter {} for version {}",
                field.name,
                selection.effective_version.as_deref().unwrap_or_default()
            ),
        }
    }
    selection
        .card
        .parameters
        .retain(|field| !matches!(field.name.as_str(), "ACM" | "CALCACM"));
    Ok(())
}

pub struct Candidate {
    pub alias: String,
    pub exact: bool,
}

pub struct Pending {
    pub source_name: String,
    pub candidates: Vec<Candidate>,
}

type Fields = BTreeMap<String, Option<String>>;

fn field<'a>(fields: &'a Fields, name: &str) -> Option<&'a str> {
    fields.get(name).and_then(Option::as_deref)
}

fn scalar(library: &ModelLibrary, fields: &Fields, name: &str) -> Result<Option<f64>> {
    field(fields, name)
        .map(|raw| library.evaluate_scalar(0, raw, &[]))
        .transpose()
}

fn instance_parameters(
    text: &str,
    start: usize,
    options: &Options,
    library: &ModelLibrary,
) -> Result<Fields> {
    let tail = &text[start..];
    let spans = statement_token_spans(tail)?;
    let mut fields = Fields::new();
    let order = if options.flag("WL", false, library)? {
        ["W", "L"]
    } else {
        ["L", "W"]
    };
    let mut consumed = 0;
    for (index, name) in order.into_iter().enumerate() {
        let Some(span) = spans.get(index) else { break };
        let token = &tail[span.clone()];
        if spans
            .get(index + 1)
            .is_some_and(|next| &tail[next.clone()] == "=")
            || token.eq_ignore_ascii_case("OFF")
        {
            break;
        }
        // A positional geometry value must be scalar, including a symbolic
        // expression that was bound to a generated parameter.
        library.evaluate_scalar(0, token, &[])?;
        fields.insert(name.to_owned(), Some(token.to_owned()));
        consumed = span.end;
    }
    for parameter in instance_fields(&tail[consumed..], 3)? {
        let name = parameter.name.to_ascii_uppercase();
        ensure!(
            fields.insert(name.clone(), parameter.expression).is_none(),
            "duplicate HSPICE MOS instance parameter {name}"
        );
    }
    for name in ["L", "W", "AD", "AS", "PD", "PS", "NRD", "NRS"] {
        if let Some(default) = options.default_field(name) {
            fields
                .entry(name.to_owned())
                .or_insert_with(|| Some(default.to_owned()));
        }
    }
    Ok(fields)
}

fn model_fields(card: &ModelCard, budget: &WorkBudget) -> Result<Fields> {
    card.parameters
        .iter()
        .map(|parameter| {
            budget.charge(Resource::GeneratedNodes, 1)?;
            budget.charge(
                Resource::EmittedBytes,
                parameter
                    .name
                    .len()
                    .saturating_add(parameter.expression.as_ref().map_or(0, String::len)),
            )?;
            Ok((
                parameter.name.to_ascii_uppercase(),
                parameter.expression.clone(),
            ))
        })
        .collect()
}

fn axis_matches(
    library: &ModelLibrary,
    fields: &Fields,
    axis: char,
    dimension: f64,
) -> Result<bool> {
    let bound = |suffix: &str| scalar(library, fields, &format!("{axis}{suffix}"));
    let min = bound("MIN")?.with_context(|| format!("missing MOS bin {axis}MIN"))?;
    let max = bound("MAX")?.with_context(|| format!("missing MOS bin {axis}MAX"))?;
    let bias = scalar(library, fields, &format!("X{axis}"))?.unwrap_or(0.);
    let reference = scalar(library, fields, &format!("X{axis}REF"))?.unwrap_or(bias);
    ensure!(
        min >= 0. && max > min,
        "invalid HSPICE MOS {axis} bin bounds"
    );
    // Evaluate the offset difference first. Equal bias/reference must cancel
    // exactly rather than introducing a rounding error at a half-open boundary.
    let coordinate = dimension + (bias - reference);
    ensure!(coordinate.is_finite(), "nonfinite HSPICE MOS bin geometry");
    Ok(min <= coordinate && coordinate < max)
}

fn required_scalar(library: &ModelLibrary, fields: &Fields, name: &str) -> Result<f64> {
    scalar(library, fields, name)?.with_context(|| format!("missing MOS {name}"))
}

fn geometry(library: &ModelLibrary, fields: &Fields, options: &Options) -> Result<(f64, f64)> {
    let l = required_scalar(library, fields, "L")?;
    let w = required_scalar(library, fields, "W")?;
    let nf = scalar(library, fields, "NF")?.unwrap_or(1.);
    ensure!(
        l > 0. && w > 0. && nf > 0.,
        "nonpositive HSPICE MOS geometry"
    );
    let per_finger = match scalar(library, fields, "WNFLAG")? {
        Some(value) => flag(value, "WNFLAG")?,
        None => options.flag("WNFLAG", true, library)?,
    };
    Ok((l, if per_finger { w / nf } else { w }))
}

fn select<'a>(
    pending: &Pending,
    models: &'a BTreeMap<String, &ModelCard>,
    library: &ModelLibrary,
    fields: &Fields,
    options: &Options,
    budget: &WorkBudget,
) -> Result<&'a ModelCard> {
    let (length, width) = geometry(library, fields, options)?;
    for candidate in &pending.candidates {
        budget.charge(Resource::EvaluationSteps, 1)?;
        let card = models
            .get(&candidate.alias.to_ascii_lowercase())
            .context("missing bound MOS candidate")?;
        ensure!(
            matches!(card.family.to_ascii_uppercase().as_str(), "NMOS" | "PMOS"),
            "non-MOS model referenced by HSPICE MOS occurrence"
        );
        if candidate.exact {
            return Ok(card);
        }
        let fields = model_fields(card, budget)?;
        if axis_matches(library, &fields, 'L', length)?
            && axis_matches(library, &fields, 'W', width)?
        {
            return Ok(card);
        }
    }
    bail!(
        "no HSPICE MOS bin of {} covers L={length:e}, W={width:e}",
        pending.source_name
    )
}

// Native ngspice parses grouped IC differently from its documented comma
// vector. Emit individual scalar expressions in VDS, VGS, VBS order.
fn initial_vector(expression: &str) -> Result<String> {
    let fields = instance_vector_fields(expression)?;
    ensure!(
        (1..=3).contains(&fields.len()),
        "HSPICE MOS IC needs one to three voltages"
    );
    let mut native = Vec::with_capacity(fields.len());
    for field in fields {
        if parse_spice_constant(field).is_ok() || field.starts_with('{') && field.ends_with('}') {
            native.push(field.to_owned());
        } else {
            native.push(format!("{{{field}}}"));
        }
    }
    Ok(native.join(","))
}

// HSPICE selects the bin using the drawn dimensions (Appendix A), then
// applies the model-specific lithography multipliers to the selected device.
// Keep parameter expressions symbolic so parameter changes do not freeze the
// transformed channel geometry at source preparation time. The documented
// nonpositive-factor reset is encoded in the native expression.
fn scale_dimension(
    fields: &mut Fields,
    model: &Fields,
    dimension: &str,
    multiplier: &str,
    budget: &WorkBudget,
) -> Result<()> {
    let Some(factor) = field(model, multiplier) else {
        return Ok(());
    };
    let drawn = field(fields, dimension).context("missing drawn MOS dimension")?;
    let drawn = super::hierarchy::inner(drawn);
    let factor = super::hierarchy::inner(factor);
    budget.charge(
        Resource::EmittedBytes,
        drawn
            .len()
            .saturating_add(factor.len().saturating_mul(2))
            .saturating_add(32),
    )?;
    let scaled = if let Ok(value) = parse_spice_constant(factor) {
        if value <= 0. || value.to_bits() == 1.0f64.to_bits() {
            return Ok(());
        }
        format!("{{({drawn})*({factor})}}")
    } else {
        format!("{{({drawn})*ternary_fcn(({factor})>0,({factor}),1)}}")
    };
    fields.insert(dimension.to_owned(), Some(scaled));
    Ok(())
}

fn bind_instance(
    text: &str,
    model_end: usize,
    pending: &Pending,
    models: &BTreeMap<String, &ModelCard>,
    library: &ModelLibrary,
    options: &Options,
    budget: &WorkBudget,
) -> Result<String> {
    let mut fields = instance_parameters(text, model_end, options, library)?;
    let card = select(pending, models, library, &fields, options, budget)?;
    let model = model_fields(card, budget)?;
    let mask = version_mask(field(&model, "VERSION").context("missing bound BSIM4 version")?)?;
    fields.remove("WNFLAG");
    scale_dimension(&mut fields, &model, "L", "LMLT", budget)?;
    scale_dimension(&mut fields, &model, "W", "WMLT", budget)?;
    let spans = statement_token_spans(text)?;
    let start = spans.get(5).context("missing MOS model token")?.start;
    let mut out = format!("{}{}", &text[..start], card.name);
    for (name, expression) in fields {
        ensure!(
            supported(bsim4_catalog::INSTANCE, &name, mask),
            "unqualified HSPICE BSIM4 instance parameter {name}"
        );
        if let Some(value) = expression {
            if name == "IC" {
                write!(out, " IC={}", initial_vector(&value)?)?;
            } else {
                write!(out, " {name}={value}")?;
            }
        } else {
            ensure!(name == "OFF", "missing HSPICE MOS instance value {name}");
            write!(out, " {name}")?;
        }
    }
    Ok(out)
}

pub fn finish(
    rows: &mut [(String, usize)],
    requests: &BTreeMap<String, Pending>,
    options: &Options,
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::Statements, rows.len())?;
    if requests.is_empty()
        && !rows.iter().skip(1).any(|(row, _)| {
            super::analysis_plan::command(row) == ".MODEL"
                && row.split_whitespace().nth(2).is_some_and(|family| {
                    family.eq_ignore_ascii_case("NMOS") || family.eq_ignore_ascii_case("PMOS")
                })
        })
    {
        return Ok(());
    }
    let mut linked = String::new();
    for (row, _) in rows.iter() {
        budget.charge(Resource::ScannedBytes, row.len())?;
        budget.charge(Resource::EmittedBytes, row.len().saturating_add(1))?;
        writeln!(linked, "{row}")?;
    }
    let library =
        ModelLibrary::parse_source_with_budget(linked.as_bytes(), RootKind::Deck, budget.clone())?;
    let models = library
        .models
        .iter()
        .map(|model| (model.card.name.to_ascii_lowercase(), &model.card))
        .collect::<BTreeMap<_, _>>();
    for (row, _) in rows.iter_mut().skip(1) {
        budget.charge(Resource::Statements, 1)?;
        budget.charge(Resource::ScannedBytes, row.len())?;
        let command = super::analysis_plan::command(row);
        if command.starts_with('M') {
            let ranges = statement_token_spans(row)?;
            let range = ranges.get(5).context("missing HSPICE MOS model")?;
            let pending = requests
                .get(&row[range.clone()])
                .context("unbound HSPICE MOS occurrence")?;
            *row = bind_instance(row, range.end, pending, &models, &library, options, budget)?;
        } else if command == ".MODEL" {
            let mut card = parse_model_card(row, 0, &[])?;
            if matches!(card.family.to_ascii_uppercase().as_str(), "NMOS" | "PMOS") {
                card.parameters.retain(|field: &ModelParameter| {
                    !SOURCE_ONLY_FIELDS
                        .iter()
                        .any(|name| field.name.eq_ignore_ascii_case(name))
                });
                *row =
                    super::models::render(&card, &card.name, budget, |value| Ok(value.to_owned()))?;
            }
        }
    }
    Ok(())
}
