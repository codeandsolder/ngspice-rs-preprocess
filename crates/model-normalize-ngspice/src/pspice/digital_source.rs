//! Lexical digital model binding, before physical occurrence elaboration.

use super::{
    digital_instance::DigitalInstance,
    digital_io::IoModel,
    digital_timing::TimingModel,
    scope::{Scopes, charge_card, charge_scope},
    switch_source::DelayedSwitchDeclaration,
};
use crate::{
    SourceDialects, lexical,
    linked_source::{LinkedSource, RootKind, SourceDialect, SourceOrigin},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;

/// Exact declaration address within the supplied source stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Location {
    /// Physical input line in the admitted stream.
    pub input_line: usize,
    /// Lexical subcircuit path (not a flattened occurrence name).
    pub scope: Vec<String>,
    /// Original resource and occurrence identity for a linked stream.
    pub linked: Option<SourceOrigin>,
}

/// A declaration with its own scope, even if inherited from another model.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Located<T> {
    /// Original declaration identity.
    pub location: Location,
    /// Typed source declaration.
    pub declaration: T,
}

/// Resolved lexical references, still unbound to numerical device occurrences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DigitalDeclaration {
    /// Source U-device.
    pub instance: Located<DigitalInstance>,
    /// Base-to-derived timing model declarations, empty only for untimed pulls.
    pub timing: Vec<Located<TimingModel>>,
    /// Base-to-derived I/O model declarations.
    pub io: Vec<Located<IoModel>>,
}

impl DigitalDeclaration {
    /// Resolve the last instance selector in its own occurrence context.
    ///
    /// # Errors
    /// Unresolvable selector or exhausted budget.
    pub fn resolve_selector(
        &self,
        name: &str,
        mut resolve: impl FnMut(&Location, &crate::model_card::ModelParameter) -> Result<f64>,
        budget: &WorkBudget,
    ) -> Result<Option<f64>> {
        budget.charge(
            Resource::EvaluationSteps,
            self.instance.declaration.selectors.len(),
        )?;
        self.instance
            .declaration
            .selectors
            .iter()
            .rev()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .map(|field| resolve(&self.instance.location, field))
            .transpose()
    }
}

pub(super) struct Capture<'a> {
    pub(super) scopes: &'a Scopes,
    origins: &'a [usize],
    dialects: SourceDialects<'a>,
}

impl Capture<'_> {
    pub(super) fn location(&self, line: usize, scope: usize) -> Result<Location> {
        let input_line = *self
            .origins
            .get(line.checked_sub(1).context("invalid digital source line")?)
            .context("missing digital source line")?;
        let path = self.scopes.path(scope);
        charge_scope(path, self.scopes.budget())?;
        let linked = match self.dialects {
            SourceDialects::Linked(linked) => {
                let origin = linked
                    .origin(input_line)
                    .context("missing linked digital source origin")?;
                self.scopes
                    .budget()
                    .charge(Resource::EmittedBytes, origin.path.as_os_str().len())?;
                Some(origin.clone())
            }
            SourceDialects::Uniform(_) => None,
        };
        Ok(Location {
            input_line,
            scope: path.to_vec(),
            linked,
        })
    }

    fn model<T>(
        &self,
        scope: usize,
        name: &str,
        parse: impl Fn(&crate::model_card::ModelCard) -> Result<Option<T>>,
    ) -> Result<Vec<Located<T>>> {
        let line = self
            .scopes
            .find_model(scope, &name.to_ascii_lowercase())?
            .with_context(|| format!("unresolved PSpice digital model {name}"))?;
        let chain = self.scopes.model_chain(line)?;
        chain
            .into_iter()
            .map(|model| {
                ensure!(
                    model.dialect == SourceDialect::Pspice,
                    "PSpice digital model {name} resolves to an unqualified source dialect"
                );
                charge_card(&model.card, self.scopes.budget())?;
                let declaration = parse(&model.card)?.with_context(|| {
                    format!(
                        "wrong PSpice digital model family {} for {name}",
                        model.card.family
                    )
                })?;
                Ok(Located {
                    location: self.location(model.card.source_line, model.scope)?,
                    declaration,
                })
            })
            .collect()
    }

    fn instance(&self, text: &str, line: usize) -> Result<DigitalDeclaration> {
        let scope = self.scopes.scope(line)?;
        let instance = DigitalInstance::parse(text, line, self.scopes.budget())?;
        let timing = if let Some(name) = &instance.timing_model {
            self.model(scope, name, |card| {
                let model = TimingModel::from_card(card)?;
                ensure!(
                    model.as_ref().is_some_and(
                        |model| Some(model.family) == instance.primitive.timing_family()
                    ),
                    "timing model {name} has the wrong family for {}",
                    instance.name
                );
                Ok(model)
            })?
        } else {
            Vec::new()
        };
        let io = self.model(scope, &instance.io_model, IoModel::from_card)?;
        Ok(DigitalDeclaration {
            instance: Located {
                location: self.location(line, scope)?,
                declaration: instance,
            },
            timing,
            io,
        })
    }
}

/// Source declarations requiring occurrence-bound event or delayed physical execution.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SourceDeclarations {
    /// Digital primitive declarations with lexical model layers.
    pub digital: Vec<DigitalDeclaration>,
    /// Short-transition switch declarations with nonzero or symbolic delay.
    pub delayed_switches: Vec<DelayedSwitchDeclaration>,
}

pub(super) fn capture(
    statements: &[lexical::Statement],
    scopes: &Scopes,
    origins: &[usize],
    dialects: SourceDialects<'_>,
) -> Result<SourceDeclarations> {
    let capture = Capture {
        scopes,
        origins,
        dialects,
    };
    let mut declarations = SourceDeclarations::default();
    let mut control = false;
    let mut conditional_depth = 0usize;
    for statement in statements {
        scopes.budget().charge(Resource::EvaluationSteps, 1)?;
        let first = statement
            .text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            control = false;
        } else if control {
            continue;
        } else if first.eq_ignore_ascii_case(b".if") {
            conditional_depth += 1;
        } else if first.eq_ignore_ascii_case(b".endif") {
            conditional_depth = conditional_depth
                .checked_sub(1)
                .context("unmatched digital source .ENDIF")?;
        } else if first
            .first()
            .is_some_and(|byte| matches!(byte, b'u' | b'U'))
            && dialects.at_mapped(origins, statement.line)? == SourceDialect::Pspice
        {
            ensure!(
                conditional_depth == 0,
                "conditional PSpice digital primitive requires topology binding"
            );
            let text = std::str::from_utf8(&statement.text)?;
            let declaration = capture.instance(text, statement.line).with_context(|| {
                format!(
                    "{}: PSpice digital declaration",
                    dialects.describe_mapped(origins, statement.line)
                )
            })?;
            declarations.digital.push(declaration);
        } else if first
            .first()
            .is_some_and(|byte| matches!(byte, b's' | b'S' | b'w' | b'W'))
            && dialects.at_mapped(origins, statement.line)?.is_pspice()
        {
            let text = std::str::from_utf8(&statement.text)?;
            if let Some(declaration) = capture.delayed_switch(text, statement.line)? {
                ensure!(
                    conditional_depth == 0,
                    "conditional PSpice delayed switch requires topology binding"
                );
                declarations.delayed_switches.push(declaration);
            }
        }
    }
    Ok(declarations)
}

/// Bind source digital and delayed-switch declarations in an admitted graph.
///
/// Model references use lexical visibility and preserve original include visits.
/// Numerical parameters, I/O interfaces and native event scheduling are not bound here.
///
/// # Errors
/// Invalid declarations, unresolved or incompatible models, or exhausted budget.
pub fn capture_linked(
    linked: &LinkedSource,
    kind: RootKind,
    budget: &WorkBudget,
) -> Result<SourceDeclarations> {
    if !linked.visits.iter().any(|visit| visit.dialect.is_pspice()) {
        return Ok(SourceDeclarations::default());
    }
    let bytes = &linked.bytes;
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    let skip = usize::from(kind == RootKind::Deck);
    let offset = if skip == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1)
    };
    let body = &bytes[offset..];
    let dollars = |line: usize| {
        linked
            .dialect_at(line + skip)
            .map(lexical::DollarMode::for_dialect)
            .context("missing linked digital lexical dialect")
    };
    let normalized = lexical::normalize_nbsp_with(body, dollars)?;
    let statements = lexical::statements_with(normalized.as_deref().unwrap_or(body), dollars)?;
    budget.charge(Resource::GeneratedNodes, linked.origins.len())?;
    let origins = (skip + 1..=linked.origins.len()).collect::<Vec<_>>();
    let dialects = SourceDialects::Linked(linked);
    let scopes = Scopes::collect(&statements, &origins, dialects, budget)?;
    capture(&statements, &scopes, &origins, dialects)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_model_layers_keep_own_declaration_scope() -> Result<()> {
        let input = b".model base UGATE(TPLHTY={global_delay})\n.model io UIO(DRVH=50)\n.subckt cell p g a y\n.model local AKO:base UGATE(TPHLTY={local_delay})\n.model io UIO(DRVH=25)\nU1 BUF p g a y local IO\n.ends\n";
        let budget = WorkBudget::default();
        let statements = lexical::statements(input)?;
        let origins = (1..=7).collect::<Vec<_>>();
        let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
        let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
        let declarations = capture(&statements, &scopes, &origins, dialects)?;
        let declaration = declarations
            .digital
            .first()
            .context("missing U-device declaration")?;
        assert_eq!(declaration.instance.location.scope, ["cell"]);
        assert_eq!(declaration.timing.len(), 2);
        assert!(declaration.timing[0].location.scope.is_empty());
        assert_eq!(declaration.timing[1].location.scope, ["cell"]);
        assert_eq!(
            declaration.timing[0].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("{global_delay}")
        );
        assert_eq!(
            declaration.timing[1].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("{local_delay}")
        );
        let paths = declaration.bind_timing(
            super::super::digital_corners::TimingScales::default(),
            |location, field| match (location.scope.as_slice(), field.expression.as_deref()) {
                ([], Some("{global_delay}")) => Ok(10.),
                ([cell], Some("{local_delay}")) if cell == "cell" => Ok(20.),
                _ => anyhow::bail!("unexpected declaration scope or expression"),
            },
            &budget,
        )?;
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].corners.typical.to_bits(), 10_f64.to_bits());
        assert_eq!(paths[1].corners.typical.to_bits(), 20_f64.to_bits());
        assert_eq!(paths[1].corners.maximum.to_bits(), 32_f64.to_bits());
        assert_eq!(declaration.io[0].location.input_line, 5);
        assert_eq!(
            declaration.io[0].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("25")
        );
        Ok(())
    }

    #[test]
    fn delayed_thresholds_preserve_control_orientation_and_scope() -> Result<()> {
        use super::super::switch_source::ControlConnection;
        let input = b".model base VSWITCH(VT={global_threshold} RON=2 TD={global_delay})\n.subckt cell p n cp cn\n.model local AKO:base VSWITCH(VH={local_hysteresis} TD={local_delay})\nS1 p n cp cn local\n.ends\n.model current ISWITCH(IT=-1m IH=0 TD=2n)\nW1 p n Vsense current\n.model immediate VSWITCH(VT=0 TD=0)\nS2 p n cp cn immediate\n";
        let budget = WorkBudget::default();
        let statements = lexical::statements(input)?;
        let origins = (1..=9).collect::<Vec<_>>();
        let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
        let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
        let declarations = capture(&statements, &scopes, &origins, dialects)?;
        let [voltage, current] = declarations.delayed_switches.as_slice() else {
            anyhow::bail!("expected exactly two delayed switches")
        };
        assert_eq!(voltage.location.scope, ["cell"]);
        assert_eq!(voltage.terminals, ["p", "n"]);
        assert_eq!(
            voltage.control,
            ControlConnection::Voltage {
                positive: "cp".to_owned(),
                negative: "cn".to_owned()
            }
        );
        let bound = voltage.bind(
            |location, field| match (location.scope.as_slice(), field.expression.as_deref()) {
                ([], Some("{global_threshold}")) => Ok(-1.),
                ([], Some("2")) => Ok(2.),
                ([cell], Some("{local_hysteresis}")) if cell == "cell" => Ok(0.25),
                ([cell], Some("{local_delay}")) if cell == "cell" => Ok(5e-9),
                _ => anyhow::bail!("unexpected effective switch field"),
            },
            &budget,
        )?;
        assert_eq!(bound.threshold.to_bits(), (-1_f64).to_bits());
        assert_eq!(bound.hysteresis.to_bits(), 0.25_f64.to_bits());
        assert_eq!(bound.delay.to_bits(), 5e-9_f64.to_bits());
        assert_eq!(bound.ron.to_bits(), 2_f64.to_bits());
        assert_eq!(bound.roff.to_bits(), 1e12_f64.to_bits());
        assert_eq!(
            current.control,
            ControlConnection::Current {
                branch: "Vsense".to_owned()
            }
        );
        let bound = current.bind(
            |_, field| {
                crate::model_card::parse_spice_constant(
                    field.expression.as_deref().context("missing expression")?,
                )
            },
            &budget,
        )?;
        assert_eq!(bound.threshold.to_bits(), (-1e-3_f64).to_bits());
        assert_eq!(bound.delay.to_bits(), 2e-9_f64.to_bits());
        Ok(())
    }

    #[test]
    fn bind_io_defaults_overrides_and_selected_interface() -> Result<()> {
        use super::super::digital_io::{InterfaceLevel, PulseRejection};
        let input = b".model base UIO(DRVH={ignored} DRVL=70 AtoD2=rx AtoD4={unused})
.model t UGATE
.subckt cell p g a y
.model io AKO:base UIO(DRVH=0 DtoA2=tx TPWRT=0)
U1 BUF p g a y t io IO_LEVEL=1 IO_LEVEL=2
.ends
";
        let budget = WorkBudget::default();
        let statements = lexical::statements(input)?;
        let origins = (1..=6).collect::<Vec<_>>();
        let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
        let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
        let declarations = capture(&statements, &scopes, &origins, dialects)?;
        let declaration = declarations.digital.first().context("missing U-device")?;
        let selector = declaration.resolve_selector(
            "IO_LEVEL",
            |location, field| {
                assert_eq!(location.scope, ["cell"]);
                assert_eq!(field.expression.as_deref(), Some("2"));
                Ok(2.)
            },
            &budget,
        )?;
        let level = InterfaceLevel::resolve(selector, Some(4.))?;
        let io = declaration.bind_io(
            level,
            |location, field| match field.name.to_ascii_uppercase().as_str() {
                "DRVH" | "TPWRT" => {
                    assert_eq!(location.scope, ["cell"]);
                    Ok(0.)
                }
                "DRVL" => {
                    assert!(location.scope.is_empty());
                    Ok(70.)
                }
                _ => anyhow::bail!("unexpected numeric I/O field {}", field.name),
            },
            |location, field| match field.name.to_ascii_uppercase().as_str() {
                "ATOD2" => {
                    assert!(location.scope.is_empty());
                    Ok("rx".to_owned())
                }
                "DTOA2" => {
                    assert_eq!(location.scope, ["cell"]);
                    Ok("tx".to_owned())
                }
                _ => anyhow::bail!("unexpected interface reference {}", field.name),
            },
            &budget,
        )?;
        assert_eq!(io.level, InterfaceLevel::Two);
        assert_eq!(io.drive_high.to_bits(), 0_f64.to_bits());
        assert_eq!(io.drive_low.to_bits(), 70_f64.to_bits());
        assert_eq!(io.drive_z.to_bits(), 250_000_f64.to_bits());
        assert_eq!(io.pulse_rejection, PulseRejection::Explicit { seconds: 0. });
        assert_eq!(io.analog_to_digital.name, "rx");
        assert_eq!(io.digital_to_analog.name, "tx");
        assert_eq!(io.power_supply.name, "DIGIFPWR");
        assert!(io.power_supply.declaration.is_none());
        Ok(())
    }

    #[test]
    fn repeated_include_preserves_distinct_visits_and_model_shadowing() -> Result<()> {
        use crate::linked_source::{LinkLimits, SourceLinker};
        use model_preprocess_command::SourceSession;
        use std::fs;

        let dir = tempfile::tempdir()?;
        let root = dir.path().join("root.cir");
        let body = dir.path().join("gate.inc");
        fs::write(
            &body,
            "U1 BUF ; comment\n+ $G_DPWR $G_DGND\u{a0}$D_HI y local IO\n",
        )?;
        fs::write(
            &root,
            concat!(
                "Scoped digital\n.model base UGATE(TPLHTY=1n)\n",
                ".subckt one p g a y\n.include gate.inc\n",
                ".model local AKO:base UGATE(TPHLTY=2n)\n.model IO UIO(DRVH=10)\n.ends\n",
                ".subckt two p g a y\n.include gate.inc\n",
                ".model local AKO:base UGATE(TPHLTY=3n)\n.model IO UIO(DRVH=20)\n.ends\n.end\n",
            ),
        )?;
        let mut session = SourceSession::default();
        let admitted = session.load(&root)?;
        let linked = SourceLinker {
            dialect: SourceDialect::Pspice,
            ..SourceLinker::default()
        }
        .link_admitted(
            &admitted,
            RootKind::Deck,
            LinkLimits::default(),
            &mut session,
        )?;
        let budget = WorkBudget::default();
        let declarations = capture_linked(&linked, RootKind::Deck, &budget)?;
        let [one, two] = declarations.digital.as_slice() else {
            anyhow::bail!("expected two lexical declarations")
        };
        let left = one
            .instance
            .location
            .linked
            .as_ref()
            .context("missing linked origin")?;
        let right = two
            .instance
            .location
            .linked
            .as_ref()
            .context("missing linked origin")?;
        assert_eq!(left.path, body.canonicalize()?);
        assert_eq!(left.path, right.path);
        assert_eq!((left.line, right.line), (1, 1));
        assert_ne!(left.visit, right.visit);
        assert_ne!(left.scope, right.scope);
        assert_eq!(one.instance.declaration.pins[0].node, "$G_DPWR");
        assert_eq!(one.instance.declaration.pins[1].node, "$G_DGND");
        assert_eq!(one.instance.declaration.pins[2].node, "$D_HI");
        assert_eq!(one.instance.location.scope, ["one"]);
        assert_eq!(two.instance.location.scope, ["two"]);
        assert_eq!(one.timing[0].location, two.timing[0].location);
        assert_eq!(
            one.timing[1].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("2n")
        );
        assert_eq!(
            two.timing[1].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("3n")
        );
        assert_eq!(
            one.io[0].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("10")
        );
        assert_eq!(
            two.io[0].declaration.assignments[0]
                .field
                .expression
                .as_deref(),
            Some("20")
        );
        Ok(())
    }

    #[test]
    fn rejects_missing_or_incompatible_digital_models() -> Result<()> {
        let budget = WorkBudget::default();
        for input in [
            ".model t UDLY(DLYTY=1n)\n.model io UIO\nU1 BUF p g a y t io\n",
            ".model t UGATE\n.model io D\nU1 BUF p g a y t io\n",
            ".model t UGATE\nU1 BUF p g a y t io\n",
            ".model t UGATE\n.model io UIO\n.if 1\nU1 BUF p g a y t io\n.endif\n",
        ] {
            let statements = lexical::statements(input.as_bytes())?;
            let origins = (1..=input.lines().count()).collect::<Vec<_>>();
            let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
            let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
            assert!(
                capture(&statements, &scopes, &origins, dialects).is_err(),
                "{input}"
            );
        }
        Ok(())
    }
}
