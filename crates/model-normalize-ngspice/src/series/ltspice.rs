//! Source-specific projection to explicit native physical-network coefficients.
//! All scalar fields remain occurrence-bound; no source name or terminal is
//! changed and no artificial electrical component is inserted here.

use super::{assignments, coupled_inductors, scoped};
use crate::{expression_start, lexical};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::{collections::BTreeSet, ops::Range};

type ScopedName = (usize, Vec<u8>);

/// Direct controls, not display/save requests, select the physical source
/// topology. Scope identity disambiguates identically named local devices.
fn current_controls(
    statements: &[(usize, lexical::Statement)],
    budget: &WorkBudget,
) -> Result<BTreeSet<ScopedName>> {
    let mut references = BTreeSet::new();
    for (scope, statement) in statements {
        let text = &statement.text;
        budget.charge(Resource::ScannedBytes, text.len())?;
        let fields = lexical::tokens(text)?;
        let Some(first) = fields.first().map(|span| &text[span.clone()]) else {
            continue;
        };
        let family = first.first().map(u8::to_ascii_lowercase);
        if matches!(family, Some(b'f' | b'h'))
            && let Some(control) = fields.get(3)
        {
            let control = &text[control.clone()];
            // Linear current-controlled cards name an explicit branch.
            // POLY control lists have one branch name for each dimension.
            if control
                .get(..5)
                .is_some_and(|word| word.eq_ignore_ascii_case(b"poly("))
                || control.eq_ignore_ascii_case(b"poly")
            {
                let (dimensions, first_control) = if control.eq_ignore_ascii_case(b"poly") {
                    (fields.get(4).map(|span| &text[span.clone()]), 5usize)
                } else {
                    (control.get(4..), 4usize)
                };
                let dimensions = dimensions
                    .and_then(|tail| tail.strip_prefix(b"("))
                    .and_then(|tail| tail.strip_suffix(b")"))
                    .and_then(|n| std::str::from_utf8(n).ok())
                    .and_then(|n| n.trim().parse::<usize>().ok())
                    .context("invalid current-controlled POLY dimension")?;
                let end = first_control
                    .checked_add(dimensions)
                    .context("POLY dimension overflow")?;
                for span in fields
                    .get(first_control..end)
                    .context("incomplete POLY controls")?
                {
                    insert_reference(&mut references, *scope, &text[span.clone()], budget)?;
                }
            } else if !control.contains(&b'(') && !control.eq_ignore_ascii_case(b"value") {
                insert_reference(&mut references, *scope, control, budget)?;
            }
        }
        if !matches!(family, Some(b'b' | b'e' | b'g' | b'f' | b'h')) {
            continue;
        }
        let Some(start) = expression_start(text, first)? else {
            continue;
        };
        for call in lexical::function_names(text, b"i", start) {
            let tail = text[call.end..].trim_ascii_start();
            let Some(end) = tail.iter().position(|byte| *byte == b')') else {
                continue;
            };
            let name = tail[1..end].trim_ascii();
            if !name.is_empty()
                && !name
                    .iter()
                    .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b'('))
            {
                insert_reference(&mut references, *scope, name, budget)?;
            }
        }
    }
    Ok(references)
}

fn insert_reference(
    references: &mut BTreeSet<ScopedName>,
    scope: usize,
    name: &[u8],
    budget: &WorkBudget,
) -> Result<()> {
    budget.charge(Resource::GeneratedNodes, 1)?;
    budget.charge(Resource::EmittedBytes, name.len())?;
    references.insert((scope, name.to_ascii_uppercase()));
    Ok(())
}

struct Card<'a> {
    statement: &'a lexical::Statement,
    fields: Vec<Range<usize>>,
    replacements: Vec<lexical::Edit>,
    appended: Vec<String>,
}

impl<'a> Card<'a> {
    fn new(statement: &'a lexical::Statement) -> Result<Self> {
        Ok(Self {
            fields: lexical::tokens(&statement.text)?,
            statement,
            replacements: Vec::new(),
            appended: Vec::new(),
        })
    }

    fn scalar(&self, key: &[u8]) -> Result<Option<&'a str>> {
        assignments(self.statement, &self.fields, key)?
            .last()
            .map(|(_, value)| std::str::from_utf8(&self.statement.text[value.clone()]))
            .transpose()
            .map_err(Into::into)
    }

    fn take(&mut self, key: &[u8]) -> Result<Option<&'a str>> {
        let found = assignments(self.statement, &self.fields, key)?;
        let value = found
            .last()
            .map(|(_, value)| std::str::from_utf8(&self.statement.text[value.clone()]))
            .transpose()?;
        for (span, _) in found {
            // Keep the separator preceding every field; the physical-line
            // editor comments an entirely consumed continuation.
            self.replacements.push((span, Vec::new()));
        }
        Ok(value)
    }

    fn push(&mut self, key: &str, value: &str) {
        self.appended.push(format!("{key}={value}"));
    }

    /// Fold generated, closed coefficients, but retain any occurrence-bound
    /// expression rather than choosing an artificial default binding.
    fn derived(&mut self, key: &str, value: &str, budget: &WorkBudget) -> Result<()> {
        if let Some(value) = crate::ltspice::closed_scalar(value.as_bytes(), budget)? {
            self.push(key, &value.to_string());
        } else {
            self.push(key, value);
        }
        Ok(())
    }

    fn rename(&mut self, source: &[u8], target: &str) -> Result<bool> {
        if let Some(value) = self.take(source)? {
            self.push(target, value);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn reject(&self, key: &[u8]) -> Result<()> {
        ensure!(
            self.scalar(key)?.is_none(),
            "line {}: unsupported LTspice physical-network setter {}",
            self.statement.line,
            String::from_utf8_lossy(key)
        );
        Ok(())
    }

    fn apply(
        mut self,
        input: &[u8],
        edits: &mut Vec<lexical::Edit>,
        budget: &WorkBudget,
    ) -> Result<()> {
        if !self.appended.is_empty() {
            let suffix = format!(" {}", self.appended.join(" "));
            budget.charge(Resource::EmittedBytes, suffix.len())?;
            if let Some((_, replacement)) = self.replacements.first_mut() {
                *replacement = suffix.trim_ascii_start().as_bytes().to_vec();
            } else {
                let field = self
                    .fields
                    .last()
                    .context("incomplete physical network card")?;
                let position = self
                    .statement
                    .source_ranges(field.end - 1..field.end)
                    .pop()
                    .context("missing physical network anchor")?
                    .end;
                edits.push((position..position, suffix.into_bytes()));
            }
        }
        lexical::replace_spans(self.statement, input, &self.replacements, edits);
        Ok(())
    }
}

/// Remove only the outer source parameter delimiters when composing a new
/// scalar expression. The native scalar phase must evaluate the whole result
/// in the same occurrence environment as the original setter.
fn expression(raw: &str) -> &str {
    let raw = raw.trim();
    raw.strip_prefix('{')
        .and_then(|tail| tail.strip_suffix('}'))
        .or_else(|| {
            raw.strip_prefix('\'')
                .and_then(|tail| tail.strip_suffix('\''))
        })
        .unwrap_or(raw)
}

fn storage_shunt(
    card: &mut Card<'_>,
    lser: Option<&str>,
    multiplicity: &str,
    budget: &WorkBudget,
) -> Result<()> {
    let Some(shunt) = card.take(b"rlshunt")? else {
        return Ok(());
    };
    let lser = lser.unwrap_or("0");
    if crate::ltspice::closed_scalar(shunt.as_bytes(), budget)? == Some(0.)
        || crate::ltspice::closed_scalar(lser.as_bytes(), budget)? == Some(0.)
    {
        return Ok(());
    }
    let r = expression(shunt);
    let m = expression(multiplicity);
    let l = expression(lser);
    // Guard each denominator independently: even an eager scalar evaluator
    // must not divide by zero for removed copies or absent shunts.
    let enabled =
        format!("ternary_fcn(({l})==0,0,ternary_fcn(({r})==0,0,ternary_fcn(({m})==0,0,1)))");
    let g = format!("{enabled}/ternary_fcn(({r})==0,1,({r}))");
    card.derived("pn_gseries_ac", &format!("{{{g}}}"), budget)?;
    card.derived(
        "pn_gseries_tran",
        &format!("{{({g})/ternary_fcn(({m})==0,1,({m}))}}"),
        budget,
    )?;
    card.derived(
        "pn_gseries_dc",
        &format!("{{({g})/ternary_fcn(({m})==0,1,({m}))}}"),
        budget,
    )?;
    card.push("pn_gseries_noise", "1");
    Ok(())
}

fn voltage(card: &mut Card<'_>, controlled: bool, budget: &WorkBudget) -> Result<()> {
    for key in [b"rpar".as_slice(), b"lser", b"rlshunt"] {
        card.reject(key)?;
    }
    let rser = card.take(b"rser")?;
    let cpar = card.take(b"cpar")?;
    if let Some(rser) = rser {
        card.push("pn_rseries", rser);
    }
    if let Some(cpar) = cpar {
        // With an ideal forcing branch, LTspice omits the output capacitance
        // altogether, even when the source current is not used as a control.
        let r = expression(rser.unwrap_or("0"));
        let c = expression(cpar);
        let value = format!("{{ternary_fcn(({r})==0,0,({c}))}}");
        card.derived(
            if controlled {
                "pn_cseries_shunt"
            } else {
                "pn_cparallel"
            },
            &value,
            budget,
        )?;
    }
    if rser.is_some() || cpar.is_some() {
        card.rename(b"temp", "pn_temp")?;
        card.push("pn_control", "1");
        card.push("pn_report_dc", "1");
    }
    Ok(())
}

fn storage(
    card: &mut Card<'_>,
    coupled: bool,
    include: bool,
    scope: usize,
    budget: &WorkBudget,
) -> Result<()> {
    let text = &card.statement.text;
    if card.fields.len() < 4 {
        return Ok(());
    }
    let inductor = matches!(text.first(), Some(b'l' | b'L'));
    ensure!(
        card.fields
            .get(4)
            .is_none_or(|span| text[span.clone()] != *b"="),
        "line {}: nonlinear LTspice passive requires typed lowering",
        card.statement.line
    );
    // The source requires a distinct bias network and exact initial history;
    // ordinary native IC or the existing storage bias alone cannot represent it.
    ensure!(
        card.scalar(b"ic")?.is_none(),
        "line {}: LTspice passive IC requires independent bias topology and storage history",
        card.statement.line
    );
    let multiplicity = card.scalar(b"m")?.unwrap_or("1");
    let lser = card.scalar(b"lser")?;
    let cpar = card.scalar(b"cpar")?;
    let mut physical = card.rename(b"rser", "pn_rseries")?;
    if inductor && !physical && !coupled {
        ensure!(
            !include || scope != 0,
            "line {}: default inductor resistance needs the complete LTspice deck to resolve mutual coupling; specify Rser explicitly",
            card.statement.line
        );
        card.push("pn_rseries", "1m");
        physical = true;
    }
    if card.scalar(b"rpar")?.is_none() {
        // Only omitted Rpar enables terminal leakage. Explicit zero disables
        // it. The default is active in DC and transient, not AC.
        let main = expression(std::str::from_utf8(&text[card.fields[3].clone()])?);
        let leakage = if inductor {
            format!("{{1e-12/({main})}}")
        } else {
            format!("{{({main})*1e-12}}")
        };
        card.derived("pn_gterminal_dc", &leakage, budget)?;
        card.derived("pn_gterminal_tran", &leakage, budget)?;
        physical = true;
    }
    physical |= card.rename(b"rpar", "pn_rparallel")?;
    physical |= card.rename(b"lser", "pn_lseries")?;
    physical |= card.rename(b"cpar", "pn_cparallel")?;
    if !inductor
        && cpar.is_none()
        && let Some(lser) = lser
    {
        let main = std::str::from_utf8(&text[card.fields[3].clone()])?;
        card.derived(
            "pn_cparallel",
            &format!(
                "{{ternary_fcn(({})==0,0,({})*1e-6)}}",
                expression(lser),
                expression(main)
            ),
            budget,
        )?;
    }
    storage_shunt(card, lser, multiplicity, budget)?;
    // A symbolic multiplicity may remove the network at elaboration. Keep
    // its original name and scalar instead of dividing by M here.
    if !physical && card.scalar(b"m")?.is_some() {
        card.push("pn_rseries", "0");
        physical = true;
    }
    if physical {
        card.push("pn_report_dc", "1");
        if inductor {
            card.derived(
                "pn_report_ac_rparallel_factor",
                &format!("{{({})-1}}", expression(multiplicity)),
                budget,
            )?;
            card.push("pn_control", "0");
        }
    }
    Ok(())
}

pub(super) fn rewrite(input: &[u8], include: bool, budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    let statements = scoped(input, budget)?;
    let coupled = coupled_inductors(&statements, budget)?;
    let controls = current_controls(&statements, budget)?;
    let mut edits = Vec::new();
    for (scope, statement) in &statements {
        budget.charge(Resource::Statements, 1)?;
        let Some(family) = statement.text.first().map(u8::to_ascii_lowercase) else {
            continue;
        };
        if !matches!(family, b'c' | b'l' | b'v' | b'i') {
            continue;
        }
        let mut card = Card::new(statement)?;
        let name = card.fields.first().context("missing device identifier")?;
        let identity = (*scope, statement.text[name.clone()].to_ascii_uppercase());
        match family {
            b'v' => voltage(&mut card, controls.contains(&identity), budget)?,
            b'c' | b'l' => storage(
                &mut card,
                coupled.contains(&identity),
                include,
                *scope,
                budget,
            )?,
            _ => {
                for key in [b"rser".as_slice(), b"rpar", b"cpar", b"lser", b"rlshunt"] {
                    card.reject(key)?;
                }
            }
        }
        card.apply(input, &mut edits, budget)?;
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

#[cfg(test)]
mod tests {
    use crate::{
        NormalizationDialect, linked_source::RootKind, model_card::parse_assignments,
        normalize_mapped, parameter_scope::ModelLibrary,
    };
    use anyhow::{Context, Result, ensure};
    use model_preprocess_command::WorkBudget;

    #[test]
    fn symbolic_coefficients_are_resolved_in_each_occurrence() -> Result<()> {
        let source = b"title\n.subckt cell p n params: copies=1 loss=2 inductance=3m shunt=20\nC1 p n 2u m={copies} Rser={loss}\n+ Lser={inductance} RLshunt={shunt}\nL1 p n 3m m={copies} Rpar=100 Rser=0 Cpar=1u\nV1 p n 1 Rser={loss} Cpar=1u\n.ends\n.end\n";
        let normalized = normalize_mapped(
            source,
            RootKind::Deck,
            NormalizationDialect::Ltspice,
            &WorkBudget::default(),
        )?;
        let bytes = &normalized.model.bytes;
        let library = ModelLibrary::parse_source(bytes, RootKind::Deck)?;
        let statements = crate::lexical::statements(bytes)?;
        for (copies, loss, inductance, shunt) in [
            (1., 2., 3e-3, 20.),
            (3., 5., 3e-3, 20.),
            (0., 0., 0., 0.),
            (2., 0., 3e-3, 0.),
        ] {
            let overrides = [
                (1, "copies", copies),
                (1, "loss", loss),
                (1, "inductance", inductance),
                (1, "shunt", shunt),
            ];
            let enabled = copies != 0. && inductance != 0. && shunt != 0.;
            for (name, expected) in [
                (
                    "C1",
                    vec![
                        ("pn_rseries", loss),
                        ("pn_lseries", inductance),
                        ("pn_gterminal_dc", 2e-18),
                        ("pn_gterminal_tran", 2e-18),
                        ("pn_cparallel", if inductance == 0. { 0. } else { 2e-12 }),
                        ("pn_gseries_ac", if enabled { 1. / shunt } else { 0. }),
                        (
                            "pn_gseries_tran",
                            if enabled { 1. / (shunt * copies) } else { 0. },
                        ),
                    ],
                ),
                (
                    "L1",
                    vec![
                        ("pn_rseries", 0.),
                        ("pn_rparallel", 100.),
                        ("pn_cparallel", 1e-6),
                        ("pn_report_ac_rparallel_factor", copies - 1.),
                    ],
                ),
                (
                    "V1",
                    vec![
                        ("pn_rseries", loss),
                        ("pn_cparallel", if loss == 0. { 0. } else { 1e-6 }),
                        ("pn_report_dc", 1.),
                        ("pn_control", 1.),
                    ],
                ),
            ] {
                let card = statements
                    .iter()
                    .find(|card| card.text.starts_with(format!("{name} ").as_bytes()))
                    .context("missing source identity")?;
                let text = std::str::from_utf8(&card.text)?;
                let start = text.find("pn_").context("missing coefficients")?;
                let fields = parse_assignments(&text[start..])?;
                for (key, want) in expected {
                    let expression = fields
                        .iter()
                        .find(|field| field.name == key)
                        .and_then(|field| field.expression.as_deref())
                        .with_context(|| format!("missing {key} in {text}"))?;
                    let got = library.evaluate_scalar(1, expression, &overrides)?;
                    ensure!(
                        (got - want).abs() <= 1e-14 * want.abs().max(1e-12),
                        "{key}: {got} != {want}"
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn current_control_is_scoped_and_not_selected_by_save() -> Result<()> {
        let source = b".subckt sensed a b\nV1 a b 1 Rser=2 Cpar=1u\nF1 b 0 V1 1\n.ends\n.subckt unsensed a b\nV1 a b 1 Rser=2 Cpar=1u\n.save i(V1)\n.ends\n.subckt behavioural a b\nV1 a b 1 Rser=2 Cpar=1u\nB1 b 0 V=I(V1)\n.ends\n";
        let output =
            super::rewrite(source, true, &WorkBudget::default())?.context("missing rewrite")?;
        let text = std::str::from_utf8(&output)?;
        let mut sources = text.lines().filter(|line| line.starts_with("V1 "));
        for key in ["pn_cseries_shunt", "pn_cparallel", "pn_cseries_shunt"] {
            let line = sources.next().context("missing voltage source")?;
            ensure!(line.contains(key), "{line}");
        }
        ensure!(sources.next().is_none(), "extra source");
        Ok(())
    }
}
