//! Compose the standalone PSpice physical frontend with occurrence binding.
//! Every generated row retains its original admission context and location.
use super::{LoweredExpansion, SelectedModels, analysis_plan, models};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{
    linked_source::{RootKind, SourceDialect},
    model_card::{parse_assignments, parse_model_card},
    pspice,
};
use model_preprocess_command::{Resource, WorkBudget};
use model_preprocess_simplis::source_dialect::{ScopedStatement, model_mapping::ModelSelection};

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
        state,
    } = input;
    budget.charge(Resource::GeneratedNodes, contexts.len())?;
    let dialects = contexts
        .iter()
        .map(|context| context.language)
        .collect::<Vec<_>>();
    let mut projected = pspice::project_rows(&bytes, RootKind::Deck, &dialects, budget)?;
    // The occurrence binder does not yet support generated code-model
    // devices or their dependent identity metadata.
    ensure!(
        projected.renamed.is_empty(),
        "PSpice generated device identities require occurrence binding"
    );
    budget.charge(Resource::GeneratedNodes, origins.len())?;
    let source_rows = std::str::from_utf8(&bytes)?.lines().collect::<Vec<_>>();
    let mut output = LoweredExpansion {
        bytes: Vec::new(),
        origins: Vec::new(),
        contexts: Vec::new(),
        selections: SelectedModels::new(),
        transfers,
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
                let physical = projected.pspice_junctions.remove(&line).with_context(|| {
                    format!(
                        "{}:{}: PSpice model has no qualified occurrence-model selection",
                        origin.source.display(),
                        origin.line
                    )
                })?;
                let statement = ScopedStatement {
                    source: origin.source.to_string_lossy().into_owned(),
                    context,
                    first_line: origin.line,
                    last_line: origin.last_line,
                    text: source_rows
                        .get(index)
                        .context("missing PSpice source declaration")?
                        .to_string(),
                };
                let mut selection = ModelSelection::from_pspice(&statement, physical, budget)?;
                selection.linked = origin.linked;
                if !output.state.nominal_temperature {
                    text = nominal_temperature(&text, budget)?;
                }
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
            && projected.pspice_junctions.is_empty()
            && projected.pspice_transfers.is_empty(),
        "unconsumed projected model selections"
    );
    Ok(output)
}
