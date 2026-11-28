//! Lexical digital model binding, before physical occurrence elaboration.

use super::{
    digital_corners::{BoundPath, DelaySelection, SelectedTiming},
    digital_instance::{DigitalInstance, Primitive},
    digital_io::{BoundIo, IoModel},
    digital_options::{DigitalOptions, InitialState},
    digital_timing::{TimingModel, TimingPath},
    optional_ports::OptionalPorts,
    scope::{Scopes, charge_card, charge_scope},
    switch_source::ThresholdSwitchDeclaration,
};
use crate::{
    SourceDialects, lexical,
    linked_source::{LinkedSource, RootKind, SourceDialect, SourceOrigin},
};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};
use serde::Serialize;
use std::ops::RangeInclusive;

/// Original physical span in one admitted resource occurrence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceSpan {
    /// First source line, include visit and lexical source scope.
    pub first: SourceOrigin,
    /// Last physical line in the same source occurrence.
    pub last_line: usize,
}

/// Exact declaration address within the supplied source stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Location {
    /// Inclusive physical input span, before normalization.
    pub input_lines: RangeInclusive<usize>,
    /// Lexical subcircuit path (not a flattened occurrence name).
    pub scope: Vec<String>,
    /// Original resource and occurrence identity for a linked stream.
    pub linked: Option<SourceSpan>,
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

/// Numeric settings for one actual occurrence. The declaration is borrowed
/// so repeated calls can share its immutable source identity and pin layout.
#[derive(Debug)]
pub struct BoundDigital<'a> {
    /// Source topology and model declarations.
    pub declaration: &'a DigitalDeclaration,
    /// Effective timing mode, absent for untimed pulls.
    pub selection: Option<DelaySelection>,
    /// Complete path corners before applying topology-dependent loading.
    pub timing: Vec<BoundPath>,
    /// Bound I/O characteristics and interface references.
    pub io: BoundIo,
    /// Requested sequential startup state, not the evaluated output state.
    pub initialization: Option<InitialState>,
}

impl BoundDigital<'_> {
    /// Iterate effective timing paths with the requested corner selection.
    /// No delay is synthesized for untimed primitives.
    ///
    /// # Errors
    /// An inverted worst-case interval or inconsistent binding.
    pub fn selected_timing(
        &self,
    ) -> impl Iterator<Item = Result<(TimingPath, SelectedTiming)>> + '_ {
        self.timing.iter().map(|path| {
            let mode = self
                .selection
                .context("timed path on an untimed PSpice primitive")?;
            Ok((path.path, path.corners.select(mode)?))
        })
    }
}

impl DigitalDeclaration {
    /// Bind one occurrence using validated circuit-wide options and the
    /// occurrence resolver. Effective timing and I/O setters are evaluated
    /// in their own declaration contexts; instance selectors use the actual
    /// occurrence context.
    ///
    /// # Errors
    /// Unsupported or invalid effective fields, failed resolution, or budget.
    pub fn bind(
        &self,
        options: DigitalOptions,
        mut resolve_number: impl FnMut(&Location, &crate::model_card::ModelParameter) -> Result<f64>,
        resolve_name: impl FnMut(&Location, &crate::model_card::ModelParameter) -> Result<String>,
        budget: &WorkBudget,
    ) -> Result<BoundDigital<'_>> {
        let primitive = self.instance.declaration.primitive;
        let selection = primitive
            .timing_family()
            .map(|_| {
                let instance = self.resolve_selector("MNTYMXDLY", &mut resolve_number, budget)?;
                options.delay_selection.override_with(instance)
            })
            .transpose()?;
        let instance_level = self.resolve_selector("IO_LEVEL", &mut resolve_number, budget)?;
        let level = options.interface_level.override_with(instance_level)?;
        let timing = self.bind_timing(options.timing_scales, &mut resolve_number, budget)?;
        let io = self.bind_io(level, resolve_number, resolve_name, budget)?;
        let initialization =
            matches!(primitive, Primitive::Sequential { .. }).then_some(options.initial_state);
        let bound = BoundDigital {
            declaration: self,
            selection,
            timing,
            io,
            initialization,
        };
        // Validate the selected intervals before crossing the execution boundary.
        for path in bound.selected_timing() {
            budget.charge(Resource::EvaluationSteps, 1)?;
            let _ = path?;
        }
        Ok(bound)
    }

    /// Resolve the last instance selector in its own occurrence context.
    ///
    /// # Errors
    /// Unresolvable selector or exhausted budget.
    fn resolve_selector(
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
    pub(super) fn location(&self, line: usize, last_line: usize, scope: usize) -> Result<Location> {
        let lines = self
            .origins
            .get(line.checked_sub(1).context("invalid PSpice source line")?..last_line)
            .context("missing PSpice physical source span")?;
        let &input_line = lines.first().context("empty PSpice source span")?;
        let &last_input_line = lines.last().context("empty PSpice source span")?;
        let path = self.scopes.path(scope);
        charge_scope(path, self.scopes.budget())?;
        let linked = match self.dialects {
            SourceDialects::Linked(linked) => {
                let origin = linked
                    .origin(input_line)
                    .context("missing linked PSpice source origin")?;
                let mut previous = origin.line;
                for &line in lines {
                    self.scopes.budget().charge(Resource::EvaluationSteps, 1)?;
                    let next = linked
                        .origin(line)
                        .context("missing linked PSpice source origin")?;
                    ensure!(
                        next.path == origin.path
                            && next.visit == origin.visit
                            && next.scope == origin.scope
                            && next.line >= previous,
                        "PSpice statement spans unrelated source occurrences"
                    );
                    previous = next.line;
                }
                self.scopes
                    .budget()
                    .charge(Resource::EmittedBytes, origin.path.as_os_str().len())?;
                Some(SourceSpan {
                    first: origin.clone(),
                    last_line: previous,
                })
            }
            SourceDialects::Uniform(_) => None,
        };
        Ok(Location {
            input_lines: input_line..=last_input_line,
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
                ensure!(
                    !model.conditional,
                    "conditional PSpice model {name} requires topology binding"
                );
                charge_card(&model.card, self.scopes.budget())?;
                let declaration = parse(&model.card)?.with_context(|| {
                    format!(
                        "wrong PSpice digital model family {} for {name}",
                        model.card.family
                    )
                })?;
                Ok(Located {
                    location: self.location(
                        model.card.source_line,
                        model.last_line,
                        model.scope,
                    )?,
                    declaration,
                })
            })
            .collect()
    }

    fn instance(&self, text: &str, line: usize, last_line: usize) -> Result<DigitalDeclaration> {
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
                location: self.location(line, last_line, scope)?,
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
    /// Short-transition switch declarations, including explicit or default zero delay.
    pub threshold_switches: Vec<ThresholdSwitchDeclaration>,
    /// Node-valued optional subcircuit signatures used by library devices.
    pub optional_ports: Vec<Located<OptionalPorts>>,
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
            // Ignore simulator control-language statements.
        } else if first.eq_ignore_ascii_case(b".if") {
            conditional_depth += 1;
        } else if first.eq_ignore_ascii_case(b".endif") {
            conditional_depth = conditional_depth
                .checked_sub(1)
                .context("unmatched digital source .ENDIF")?;
        } else if first.eq_ignore_ascii_case(b".subckt")
            && dialects.at_mapped(origins, statement.line)? == SourceDialect::Pspice
        {
            let text = std::str::from_utf8(&statement.text)?;
            if let Some(signature) = OptionalPorts::parse(text, scopes.budget())? {
                ensure!(
                    conditional_depth == 0,
                    "conditional PSpice optional ports require topology binding"
                );
                declarations.optional_ports.push(Located {
                    location: capture.location(
                        statement.line,
                        statement.last_line,
                        scopes.scope(statement.line)?,
                    )?,
                    declaration: signature,
                });
            }
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
            let declaration = capture
                .instance(text, statement.line, statement.last_line)
                .with_context(|| {
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
            if let Some(declaration) =
                capture.threshold_switch(text, statement.line, statement.last_line)?
            {
                ensure!(
                    conditional_depth == 0,
                    "conditional PSpice threshold switch requires topology binding"
                );
                declarations.threshold_switches.push(declaration);
            }
        }
    }
    Ok(declarations)
}

/// Capture source digital and short-transition switch declarations in an admitted graph.
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
        let local = ".model local AKO:base UGATE(TPHLTY={local_delay})";
        let field = &declaration.timing[1].declaration.assignments[0].field;
        assert_eq!(&local[field.span.name.clone()], "TPHLTY");
        assert_eq!(
            &local[field
                .span
                .value
                .clone()
                .context("missing expression span")?],
            "{local_delay}"
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
        assert_eq!(declaration.io[0].location.input_lines, 5..=5);
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
    fn threshold_switches_preserve_control_orientation_scope_and_boundaries() -> Result<()> {
        use super::super::switch_source::ControlConnection;
        let input = b".model base VSWITCH(VT={global_threshold} RON=2 TD={global_delay})\n.subckt cell p n cp cn\n.model local AKO:base VSWITCH(VH={local_hysteresis} TD={local_delay})\nS1 p n cp cn local\n.ends\n.model current ISWITCH(IT=-1m IH=0 TD=2n)\nW1 p n Vsense current\n.model immediate VSWITCH(VT=0 TD=0)\nS2 p n cp cn immediate\n";
        let budget = WorkBudget::default();
        let statements = lexical::statements(input)?;
        let origins = (1..=9).collect::<Vec<_>>();
        let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
        let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
        let declarations = capture(&statements, &scopes, &origins, dialects)?;
        let [voltage, current, immediate] = declarations.threshold_switches.as_slice() else {
            anyhow::bail!("expected exactly three threshold switches")
        };
        assert!(voltage.requires_delayed_kernel());
        assert!(current.requires_delayed_kernel());
        assert!(!immediate.requires_delayed_kernel());
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
        let boundaries = bound.boundaries()?;
        assert_eq!(
            boundaries.turn_on_at_or_above.to_bits(),
            (-0.75_f64).to_bits()
        );
        assert_eq!(boundaries.turn_off_below.to_bits(), (-1.25_f64).to_bits());
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
        let boundaries = bound.boundaries()?;
        assert_eq!(
            boundaries.turn_on_at_or_above.to_bits(),
            (-1e-3_f64).to_bits()
        );
        assert_eq!(boundaries.turn_off_below.to_bits(), (-1e-3_f64).to_bits());
        let immediate = immediate.bind(
            |_, field| {
                crate::model_card::parse_spice_constant(
                    field.expression.as_deref().context("missing expression")?,
                )
            },
            &budget,
        )?;
        assert_eq!(immediate.delay.to_bits(), 0_f64.to_bits());
        let boundaries = immediate.boundaries()?;
        assert_eq!(boundaries.turn_on_at_or_above.to_bits(), 0_f64.to_bits());
        assert_eq!(boundaries.turn_off_below.to_bits(), 0_f64.to_bits());
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
        let level = InterfaceLevel::Four.override_with(selector)?;
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
    fn one_occurrence_binder_keeps_interface_and_timing_overrides_together() -> Result<()> {
        use super::super::{
            digital_corners::SelectedTiming, digital_io::InterfaceLevel,
            digital_options::DigitalOption,
        };
        use crate::model_card::parse_spice_constant;
        use std::collections::BTreeMap;

        let input = b".model t UEFF(TPCLKQLHTY=10n)\n.model io UIO(DRVL=25)\nU1 DFF(1) p g pre clr clk d q qb t io MNTYMXDLY=4 IO_LEVEL=2\n";
        let budget = WorkBudget::default();
        let statements = lexical::statements(input)?;
        let origins = (1..=3).collect::<Vec<_>>();
        let dialects = SourceDialects::Uniform(SourceDialect::Pspice);
        let scopes = Scopes::collect(&statements, &origins, dialects, &budget)?;
        let declarations = capture(&statements, &scopes, &origins, dialects)?;
        let declaration = declarations
            .digital
            .first()
            .context("missing digital instance")?;
        let options = DigitalOptions::from_effective(&BTreeMap::from([
            (DigitalOption::InitialState, 1.),
            (DigitalOption::DelaySelection, 1.),
            (DigitalOption::InterfaceLevel, 3.),
        ]))?;
        let bound = declaration.bind(
            options,
            |_, field| {
                parse_spice_constant(field.expression.as_deref().context("missing expression")?)
            },
            |_, _| anyhow::bail!("no explicit interface names expected"),
            &budget,
        )?;
        assert_eq!(bound.selection, Some(DelaySelection::WorstCase));
        assert_eq!(bound.io.level, InterfaceLevel::Two);
        assert_eq!(bound.initialization, Some(InitialState::Set));
        let paths = bound.selected_timing().collect::<Result<Vec<_>>>()?;
        let (_, selected) = paths
            .iter()
            .find(|(path, _)| *path == TimingPath::ClockRise)
            .context("missing clock propagation path")?;
        let SelectedTiming::Window { minimum, maximum } = *selected else {
            anyhow::bail!("worst-case timing collapsed");
        };
        assert_eq!(minimum.to_bits(), (10e-9_f64 * 0.4).to_bits());
        assert_eq!(maximum.to_bits(), (10e-9_f64 * 1.6).to_bits());
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
                ".subckt one a y\n+ OPTIONAL: p=$G_DPWR g=$G_DGND\n.include gate.inc\n",
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
        assert_eq!(declarations.optional_ports.len(), 1);
        let ports = &declarations.optional_ports[0];
        assert_eq!(ports.location.scope, ["one"]);
        assert_eq!(ports.declaration.required, ["a", "y"]);
        assert_eq!(ports.declaration.optional[0].default.name, "$G_DPWR");
        assert_eq!(ports.declaration.optional[1].default.name, "$G_DGND");
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
        assert_eq!(left.first.path, body.canonicalize()?);
        assert_eq!(left.first.path, right.first.path);
        assert_eq!((left.first.line, right.first.line), (1, 1));
        assert_eq!((left.last_line, right.last_line), (2, 2));
        assert_ne!(left.first.visit, right.first.visit);
        assert_ne!(left.first.scope, right.first.scope);
        assert_eq!(one.instance.declaration.pins[0].node.name, "$G_DPWR");
        assert_eq!(one.instance.declaration.pins[1].node.name, "$G_DGND");
        assert_eq!(one.instance.declaration.pins[2].node.name, "$D_HI");
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
            ".if 0\n.model t UGATE\n.endif\n.model io UIO\nU1 BUF p g a y t io\n",
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
