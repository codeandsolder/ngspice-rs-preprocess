//! Compose the standalone `PSpice` physical frontend with occurrence binding.
//! Every generated row retains its original admission context and location.
use super::{LoweredExpansion, SelectedModels, analysis_plan, models};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    linked_source::{RootKind, SourceDialect},
    model_card::{parse_assignments, parse_model_card},
    pspice,
};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::source_dialect::{
    DeviceIdentity, ScopedStatement, model_mapping::ModelSelection,
};

fn nominal_temperature(text: &str, budget: &WorkBudget) -> Result<String> {
    let mut card = parse_model_card(text, 0, &[])?;
    if card
        .parameters
        .iter()
        .any(|field| field.name.eq_ignore_ascii_case("TNOM"))
    {
        return Ok(text.to_owned());
    }
    // A receiving HSPICE deck has a different implicit TNOM. Preserve the
    // PSpice model's native 27 C nominal default unless the root explicitly
    // selected a circuit-wide nominal temperature.
    card.parameters.extend(parse_assignments("TNOM=27")?);
    models::render(&card, &card.name, budget, |value| Ok(value.to_owned()))
}

fn project_identities(
    devices: Vec<model_normalize_ngspice::RenamedDevice>,
    origins: &[crate::OriginLine],
    device_identities: &mut Vec<DeviceIdentity>,
    budget: &WorkBudget,
) -> Result<()> {
    for device in devices {
        let index = device
            .input_line
            .checked_sub(1)
            .context("invalid PSpice device origin")?;
        let origin = origins.get(index).context("missing PSpice device origin")?;
        let identity = DeviceIdentity {
            source: origin.source.to_string_lossy().into_owned(),
            scope: device.scope,
            displayed: device.declared.clone(),
            declared: device.declared,
            linked: device.linked,
        };
        device_identities.push(identity.clone_with_budget(budget)?);
    }
    Ok(())
}

fn project_model(
    physical: pspice::PhysicalProjection,
    context: model_preprocess_simplis::source_dialect::DialectContext,
    origin: &crate::OriginLine,
    original: &str,
    text: &str,
    root_nominal: bool,
    budget: &WorkBudget,
) -> Result<(ModelSelection, String)> {
    let statement = ScopedStatement {
        source: origin.source.to_string_lossy().into_owned(),
        context,
        first_line: origin.line,
        last_line: origin.last_line,
        text: original.to_owned(),
    };
    let temperature = physical.kind.has_nominal_temperature();
    let mut selection = ModelSelection::from_pspice(&statement, physical, budget)?;
    selection.linked = origin.linked;
    let text = if temperature && !root_nominal {
        nominal_temperature(text, budget)?
    } else {
        text.to_owned()
    };
    Ok((selection, text))
}

pub(super) fn project(
    input: LoweredExpansion,
    limit: usize,
    budget: &WorkBudget,
) -> Result<LoweredExpansion> {
    if !input
        .contexts
        .iter()
        .any(|context| context.language == SourceDialect::Pspice)
    {
        return Ok(input);
    }
    let LoweredExpansion {
        bytes,
        origins,
        contexts,
        mut selections,
        transfers,
        mut device_identities,
        state,
    } = input;
    budget.charge(Resource::GeneratedNodes, contexts.len())?;
    let dialects = contexts
        .iter()
        .map(|context| context.language)
        .collect::<Vec<_>>();
    let mut projected = pspice::project_rows(&bytes, RootKind::Deck, &dialects, budget)?;
    project_identities(
        std::mem::take(&mut projected.renamed),
        &origins,
        &mut device_identities,
        budget,
    )?;
    budget.charge(Resource::GeneratedNodes, origins.len())?;
    let source_rows = std::str::from_utf8(&bytes)?.lines().collect::<Vec<_>>();
    let mut output = LoweredExpansion {
        bytes: Vec::new(),
        origins: Vec::new(),
        contexts: Vec::new(),
        selections: SelectedModels::new(),
        transfers,
        device_identities,
        state,
    };
    for (text, line) in std::str::from_utf8(&projected.mapped.model.bytes)?
        .lines()
        .zip(projected.mapped.input_lines)
    {
        let index = line
            .checked_sub(1)
            .context("invalid PSpice projection origin")?;
        let origin = origins
            .get(index)
            .context("missing PSpice projection origin")?;
        let context = *contexts
            .get(index)
            .context("missing PSpice projection context")?;
        let mut text = text.to_owned();
        if analysis_plan::command(&text) == ".MODEL" && index != 0 {
            let selection = if context.language == SourceDialect::Pspice {
                let physical = projected.pspice_models.remove(&line).with_context(|| {
                    format!(
                        "{}:{}: PSpice model has no qualified occurrence-model selection",
                        origin.source.display(),
                        origin.line
                    )
                })?;
                let (selection, projected_text) = project_model(
                    physical,
                    context,
                    origin,
                    source_rows
                        .get(index)
                        .context("missing PSpice source declaration")?,
                    &text,
                    output.state.nominal_temperature,
                    budget,
                )?;
                text = projected_text;
                selection
            } else {
                selections
                    .remove(&index)
                    .context("missing projected model selection")?
            };
            output.selections.insert(output.origins.len(), selection);
        }
        if analysis_plan::command(&text).starts_with(['E', 'G'])
            && let Some(transfer) = projected.pspice_transfers.remove(&line)
        {
            output.transfers.insert(output.origins.len(), transfer);
        }
        budget.charge(Resource::EmittedBytes, origin.source.as_os_str().len())?;
        super::append_lowered_line(
            &mut output.bytes,
            &mut output.origins,
            &text,
            origin.clone(),
            limit,
            budget,
        )?;
        output.contexts.push(context);
    }
    ensure!(
        selections.is_empty()
            && projected.pspice_models.is_empty()
            && projected.pspice_transfers.is_empty(),
        "unconsumed projected model selections"
    );
    Ok(output)
}
