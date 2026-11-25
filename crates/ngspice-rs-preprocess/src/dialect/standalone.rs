//! Native-output preparation for explicitly selected source languages.
//! Reuse the admitted, resource-local logical statement graph rather
//! than feeding a concatenated mixed-language file to compatibility mode.
use super::{SourceExpansion, bundle};
use crate::{
    AdmittedSource, Artifact, InputFormat, Options, OriginLine, PreparedBundle, Provenance,
    Request, SourceSession,
};
use anyhow::{Context, Result, ensure};
use model_normalize_ngspice::{linked_source::RootKind, normalize_mapped};
use model_preprocess_simplis::source_dialect::{ScopedStatement, SourceKind};
use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf};

mod hspice;

pub(super) fn prepare(
    root: &AdmittedSource,
    request: &Request,
    options: &Options,
    session: &mut SourceSession,
) -> Result<PreparedBundle> {
    let context = request
        .format
        .standalone_context()
        .context("missing standalone language")?;
    let deck = request.format == InputFormat::HspiceDeck;
    let kind = if deck {
        SourceKind::Deck
    } else {
        SourceKind::Include
    };
    let mut expansion = SourceExpansion::new(session, request, options);
    expansion.expand(root, context, kind, None)?;
    ensure!(expansion.scope_depth == 0, "unclosed source subcircuit");
    let mut source = expansion.statements;
    if deck {
        let title = source.first_mut().context("standalone deck has no title")?;
        ensure!(
            title.first_line == 1 && title.last_line == 1,
            "invalid deck title span"
        );
        std::str::from_utf8(
            root.bytes
                .split(|&byte| byte == b'\n')
                .next()
                .context("missing title")?,
        )?
        .trim_end_matches('\r')
        .clone_into(&mut title.text);
    }
    let prepared = hspice::lower(&source, deck, options.output_bytes)?;
    finish(
        root,
        request.format,
        deck,
        &prepared,
        &expansion.admitted_paths,
        options.output_bytes,
    )
}

fn finish(
    root: &AdmittedSource,
    format: InputFormat,
    deck: bool,
    source: &[ScopedStatement],
    admitted_paths: &BTreeMap<String, PathBuf>,
    limit: usize,
) -> Result<PreparedBundle> {
    let mut rendered = String::new();
    let mut origins = Vec::new();
    for statement in source {
        ensure!(
            !statement.text.contains('\n'),
            "projected source statement spans multiple output lines"
        );
        ensure!(
            statement.text.len().saturating_add(1) <= limit.saturating_sub(rendered.len()),
            "standalone source output budget exceeded"
        );
        writeln!(rendered, "{}", statement.text)?;
        origins.push(OriginLine {
            source: admitted_paths
                .get(&statement.source)
                .context("unknown admitted source")?
                .clone(),
            line: statement.first_line,
            last_line: statement.last_line,
        });
    }
    let kind = if deck {
        RootKind::Deck
    } else {
        RootKind::Include
    };
    let normalized = normalize_mapped(rendered.as_bytes(), kind)?;
    let lines = normalized
        .input_lines
        .into_iter()
        .map(|line| {
            origins
                .get(line - 1)
                .cloned()
                .context("missing standalone source span")
        })
        .collect::<Result<Vec<_>>>()?;
    let mut result = bundle(
        root,
        format,
        Artifact {
            filename: if deck { "deck.cir" } else { "model.lib" }.to_owned(),
            bytes: normalized.model.bytes,
            provenance: Provenance::Native { lines },
            dependencies: Vec::new(),
            required_code_models: Vec::new(),
            current_scalings: Vec::new(),
        },
        Vec::new(),
    );
    result.normalization = normalized.model.passes;
    Ok(result)
}
