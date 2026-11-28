//! Source-specific projection to explicit native physical-network coefficients.
//! All scalar fields remain occurrence-bound; no source name or terminal is
//! changed and no artificial electrical component is inserted here.

use super::{assignments, device_statements};
use crate::lexical;
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use std::ops::Range;

mod thermal;

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
        let Some(value) = self.take(source)? else {
            return Ok(false);
        };
        self.push(target, value);
        Ok(true)
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

fn voltage(card: &mut Card<'_>, budget: &WorkBudget) -> Result<()> {
    for key in [b"rpar".as_slice(), b"lser", b"rlshunt", b"temp"] {
        card.reject(key)?;
    }
    thermal::noise(card, false)?;
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
        // The actual current-reference graph is only known after occurrence
        // and macro elaboration. Do not select a topology in this file.
        card.derived("pn_ccontrol", &value, budget)?;
    }
    if rser.is_some() || cpar.is_some() {
        card.push("pn_control", "1");
        card.push("pn_report_dc", "1");
    }
    Ok(())
}

/// Default leakage follows the sign of the effective storage, not the
/// unscaled value. The coefficient is still per copy. Compare signs rather
/// than multiplying or dividing the storage and multiplicity to avoid an
/// intermediate overflow. A zero-copy network has no leakage; a zero
/// inductance with nonzero copies has undefined omitted-Rpar leakage.
fn default_leakage(main: &str, multiplicity: &str, inductor: bool) -> String {
    let v = expression(main);
    let m = expression(multiplicity);
    let enabled = format!("ternary_fcn(({m})>0,({v})>=0,ternary_fcn(({m})<0,({v})<=0,0))");
    if inductor {
        format!("({enabled})*1e-12/ternary_fcn({enabled},({v}),1)")
    } else {
        format!("({enabled})*({v})*1e-12")
    }
}

/// Startup bias and storage history are separate physical laws. Only the
/// terminal bias phase replaces the physical parasitic network.
fn storage_initialization(
    card: &mut Card<'_>,
    inductor: bool,
    multiplicity: &str,
    budget: &WorkBudget,
) -> Result<bool> {
    let Some(initial) = card.scalar(b"ic")? else {
        return Ok(false);
    };
    let rser = card.scalar(b"rser")?.map(expression);
    let g = if inductor {
        let m = expression(multiplicity);
        let parallel = if let Some(rpar) = card.scalar(b"rpar")? {
            let r = expression(rpar);
            format!("ternary_fcn(({r})==0,0,({m})/ternary_fcn(({r})==0,1,({r})))")
        } else {
            let l = std::str::from_utf8(&card.statement.text[card.fields[3].clone()])?;
            format!("({m})*({})", default_leakage(l, multiplicity, true))
        };
        if let Some(rser) = rser {
            format!("({parallel})+ternary_fcn(({rser})==0,1e-5,0)")
        } else {
            // The final series topology determines this extra aggregate
            // startup conductance, including coupling across scope boundaries.
            card.push("pn_terminal_bias_zero_series", "1e-5");
            parallel
        }
    } else {
        // Nonpositive series loss uses the finite 1000 S startup law.
        // Positive loss uses the reciprocal of the specified (unscaled) Rser.
        let rser = rser.unwrap_or("0");
        format!("1/ternary_fcn(({rser})>0,({rser}),1m)")
    };
    let conductance = format!("{{{g}}}");
    card.derived("pn_terminal_bias_op", &conductance, budget)?;
    card.derived("pn_terminal_bias_tran", &conductance, budget)?;
    card.push("pn_exact_ic", "1");
    if !inductor && (card.scalar(b"cpar")?.is_some() || card.scalar(b"lser")?.is_some()) {
        // Both the primary and external capacitor get the prescribed voltage.
        // An occurrence whose parallel capacitance resolves to zero has no
        // corresponding storage state.
        card.push("pn_parallel_ic", initial);
    }
    Ok(true)
}

fn storage(card: &mut Card<'_>, budget: &WorkBudget) -> Result<()> {
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
    let contextual_leakage = thermal::storage(card, inductor, budget)?;
    thermal::noise(card, true)?;
    let multiplicity = card.scalar(b"m")?.unwrap_or("1");
    let lser = card.scalar(b"lser")?;
    let cpar = card.scalar(b"cpar")?;
    let mut physical = storage_initialization(card, inductor, multiplicity, budget)?;
    let explicit_rser = card.rename(b"rser", "pn_rseries")?;
    physical |= explicit_rser;
    if inductor && !explicit_rser {
        card.push("pn_rseries_uncoupled", "1m");
        physical = true;
    }
    if card.scalar(b"rpar")?.is_none() {
        // Only omitted Rpar enables terminal leakage. Explicit zero disables
        // it. The default is active in DC and transient, not AC.
        if contextual_leakage {
            card.push("pn_gterminal_positive_storage_dc", "1e-12");
            card.push("pn_gterminal_positive_storage_tran", "1e-12");
        } else {
            let main = std::str::from_utf8(&text[card.fields[3].clone()])?;
            let leakage = format!("{{{}}}", default_leakage(main, multiplicity, inductor));
            card.derived("pn_gterminal_dc", &leakage, budget)?;
            card.derived("pn_gterminal_tran", &leakage, budget)?;
        }
        physical = true;
    }
    if card.rename(b"rpar", "pn_rparallel")? {
        // The parallel thermal source uses conductance magnitude even when
        // the electrical resistor is negative. Series loss is different.
        card.push("pn_rparallel_noise_abs", "1");
        physical = true;
    }
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
                "{{ternary_fcn(({})==0,0,max(({}),0)*1e-6)}}",
                expression(lser),
                expression(main)
            ),
            budget,
        )?;
    }
    storage_shunt(card, lser, multiplicity, budget)?;
    // A symbolic multiplicity may remove the network at elaboration. Keep
    // its original name and scalar instead of dividing by M here.
    if !physical && (!card.appended.is_empty() || card.scalar(b"m")?.is_some()) {
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
        } else {
            // A capacitor's behavioral current is the physical port
            // current, not just the current of its primary charge storage.
            // Selection of the sampling law is a separate operation.
            card.push("pn_control", "1");
        }
    }
    Ok(())
}

pub(super) fn rewrite(input: &[u8], budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    let statements = device_statements(input, budget)?;
    let mut edits = Vec::new();
    for statement in &statements {
        budget.charge(Resource::Statements, 1)?;
        let Some(family) = statement.text.first().map(u8::to_ascii_lowercase) else {
            continue;
        };
        if !matches!(family, b'c' | b'l' | b'v' | b'i') {
            continue;
        }
        let mut card = Card::new(statement)?;
        match family {
            b'v' => voltage(&mut card, budget)?,
            b'c' | b'l' => storage(&mut card, budget)?,
            _ => {
                thermal::noise(&mut card, false)?;
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
            (-2., -2., -3e-3, -20.),
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
                        ("pn_control", 1.),
                        ("pn_gterminal_dc", if copies > 0. { 2e-18 } else { 0. }),
                        ("pn_gterminal_tran", if copies > 0. { 2e-18 } else { 0. }),
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
                        ("pn_control", 0.),
                    ],
                ),
                (
                    "V1",
                    vec![
                        ("pn_rseries", loss),
                        ("pn_ccontrol", if loss == 0. { 0. } else { 1e-6 }),
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
    fn startup_bias_and_parallel_history_are_occurrence_bound() -> Result<()> {
        let source = b"title\n.subckt cell p n params: copies=1 loss=2 shunt=20 init=.75\nC1 p n 2u m={copies} Rser={loss} IC={init} Cpar=1u\nL1 p n 3m m={copies} Rser={loss} Rpar={shunt} IC=.1 Cpar=1u\nL2 p n 3m IC=.1\nL3 p n 3m IC=.1\nK1 L2 L3 .5\n.ends\n.end\n";
        let normalized = normalize_mapped(
            source,
            RootKind::Deck,
            NormalizationDialect::Ltspice,
            &WorkBudget::default(),
        )?;
        let bytes = &normalized.model.bytes;
        let library = ModelLibrary::parse_source(bytes, RootKind::Deck)?;
        let statements = crate::lexical::statements(bytes)?;
        for (copies, loss, shunt, init) in [
            (1., 2., 20., 0.75),
            (3., 5., 20., 0.),
            (0., 0., 0., 1.),
            (2., 0., 0., 0.5),
            (-2., -2., -20., 0.5),
            (1., 1e-4, 20., 0.5),
        ] {
            let overrides = [
                (1, "copies", copies),
                (1, "loss", loss),
                (1, "shunt", shunt),
                (1, "init", init),
            ];
            let c_bias = if loss <= 0. { 1000. } else { 1. / loss };
            let l_bias =
                if shunt == 0. { 0. } else { copies / shunt } + if loss == 0. { 1e-5 } else { 0. };
            for (name, want, parallel) in [
                ("C1", c_bias, Some(init)),
                ("L1", l_bias, None),
                ("L2", 1e-12 / 3e-3, None),
                ("L3", 1e-12 / 3e-3, None),
            ] {
                let card = statements
                    .iter()
                    .find(|card| card.text.starts_with(format!("{name} ").as_bytes()))
                    .context("missing device")?;
                let text = std::str::from_utf8(&card.text)?;
                let start = text.find("pn_").context("missing coefficients")?;
                let fields = parse_assignments(&text[start..])?;
                let value = |key: &str| -> Result<Option<f64>> {
                    fields
                        .iter()
                        .find(|f| f.name == key)
                        .map(|f| {
                            library.evaluate_scalar(
                                1,
                                f.expression.as_deref().context("missing expression")?,
                                &overrides,
                            )
                        })
                        .transpose()
                };
                for phase in ["pn_terminal_bias_op", "pn_terminal_bias_tran"] {
                    let actual = value(phase)?.context("missing bias")?;
                    ensure!(
                        (actual - want).abs() <= 1e-13 * want.abs().max(1e-12),
                        "{name}: {actual} != {want}"
                    );
                }
                if matches!(name, "L2" | "L3") {
                    ensure!(value("pn_rseries_uncoupled")? == Some(1e-3));
                    ensure!(value("pn_rseries")?.is_none());
                    ensure!(value("pn_terminal_bias_zero_series")? == Some(1e-5));
                } else {
                    ensure!(value("pn_terminal_bias_zero_series")?.is_none());
                }
                ensure!(
                    value("pn_exact_ic")? == Some(1.),
                    "{name}: no exact history"
                );
                ensure!(
                    value("pn_terminal_bias_dc")?.is_none()
                        && value("pn_terminal_bias_ac")?.is_none(),
                    "{name}: bias leaked into DC sweep or AC"
                );
                ensure!(
                    value("pn_parallel_ic")? == parallel,
                    "{name}: incorrect auxiliary storage IC"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn default_leakage_and_auxiliary_capacitance_use_distinct_signed_laws() -> Result<()> {
        let source = b"title\n.subckt cell p n params: c=2u l=3m copies=1\nC1 p n {c} m={copies} Lser=3m IC=.5\nL1 p n {l} m={copies} Rser=5 IC=.1\n.ends\n.end\n";
        let normalized = normalize_mapped(
            source,
            RootKind::Deck,
            NormalizationDialect::Ltspice,
            &WorkBudget::default(),
        )?;
        let bytes = &normalized.model.bytes;
        let library = ModelLibrary::parse_source(bytes, RootKind::Deck)?;
        let statements = crate::lexical::statements(bytes)?;
        for (c, l, m, c_leak, l_leak, cpar) in [
            (2e-6, 3e-3, 1., 2e-18, 1e-12 / 3e-3, 2e-12),
            (-2e-6, -3e-3, 1., 0., 0., 0.),
            (2e-6, 3e-3, -2., 0., 0., 2e-12),
            (-2e-6, -3e-3, -2., -2e-18, -1e-12 / 3e-3, 0.),
            (0., 0., 0., 0., 0., 0.),
        ] {
            let overrides = [(1, "c", c), (1, "l", l), (1, "copies", m)];
            for (name, expected) in [
                (
                    "C1",
                    vec![
                        ("pn_gterminal_dc", c_leak),
                        ("pn_gterminal_tran", c_leak),
                        ("pn_cparallel", cpar),
                    ],
                ),
                (
                    "L1",
                    vec![
                        ("pn_gterminal_dc", l_leak),
                        ("pn_gterminal_tran", l_leak),
                        ("pn_terminal_bias_op", m * l_leak),
                        ("pn_terminal_bias_tran", m * l_leak),
                    ],
                ),
            ] {
                let card = statements
                    .iter()
                    .find(|card| card.text.starts_with(format!("{name} ").as_bytes()))
                    .context("missing device")?;
                let text = std::str::from_utf8(&card.text)?;
                let fields =
                    parse_assignments(&text[text.find("pn_").context("missing coefficients")?..])?;
                for (key, want) in expected {
                    let expression = fields
                        .iter()
                        .find(|field| field.name == key)
                        .and_then(|field| field.expression.as_deref())
                        .context("missing expression")?;
                    let got = library.evaluate_scalar(1, expression, &overrides)?;
                    ensure!(
                        (got - want).abs() <= 1e-13 * want.abs().max(1e-22),
                        "{name} {key}: {got} != {want}"
                    );
                }
            }
        }
        for copies in [1., -1.] {
            let value = format!(
                "{{{}}}",
                super::default_leakage("0", &copies.to_string(), true)
            );
            ensure!(
                library.evaluate_scalar(1, &value, &[]).is_err(),
                "nonzero-copy zero-L omitted Rpar must be diagnosed"
            );
        }
        Ok(())
    }

    #[test]
    fn topology_selection_is_deferred_to_the_occurrence_reference_graph() -> Result<()> {
        // Local and nested controls, display-only references and coupled
        // descendants may differ between instances of the same definition.
        let source = b".subckt sensed a b\nV1 a b 1 Rser=2 Cpar=1u\nF1 b 0 V1 1\n.ends\n.subckt unsensed a b\nV1 a b 1 Rser=2 Cpar=1u\n.save i(V1)\n.ends\n.subckt behavioural a b\nV1 a b 1 Rser=2 Cpar=1u\n.func sense() {I(V1)}\nB1 b 0 V=sense()\nL1 a b 3m IC=.1 Rpar=0\n.ends\nX1 p 0 behavioural\nX2 q 0 behavioural\nK1 L:1:1 L:2:1 0\n";
        let output = super::rewrite(source, &WorkBudget::default())?.context("missing rewrite")?;
        let text = std::str::from_utf8(&output)?;
        let sources: Vec<_> = text
            .lines()
            .filter(|line| line.starts_with("V1 "))
            .collect();
        ensure!(sources.len() == 3);
        for line in sources {
            ensure!(line.contains("pn_ccontrol=0.000001"), "{line}");
            ensure!(
                !line.contains("pn_cseries_shunt") && !line.contains("pn_cparallel"),
                "{line}"
            );
        }
        let winding = text
            .lines()
            .find(|line| line.starts_with("L1 "))
            .context("missing winding")?;
        ensure!(winding.contains("pn_rseries_uncoupled=1m"));
        ensure!(winding.contains("pn_terminal_bias_zero_series=1e-5"));
        ensure!(!winding.contains("pn_rseries="));
        ensure!(text.contains("K1 L:1:1 L:2:1 0"));
        ensure!(text.contains(".func sense() {I(V1)}"));
        ensure!(text.contains(".save i(V1)"));
        Ok(())
    }
}
