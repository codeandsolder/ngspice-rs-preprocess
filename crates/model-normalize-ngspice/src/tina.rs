//! TINA-specific plaintext SPICE syntax.
//!
//! `DesignSoft` documents `PSpice` netlist export and `PARAMS:` subcircuit
//! defaults. An exported macro can also place a local .PARAM declaration
//! directly on a .SUBCKT header (e.g. TI ISO224X export).
//! Separate that declaration without making it an overridable formal.

use crate::{NormalizationPass, lexical, linked_source::RootKind, pspice::PspiceNormalization};
use anyhow::{Context, Result, ensure};

/// A TINA source projection before the common SPICE normalization stage.
struct Projection {
    bytes: Vec<u8>,
    lines: Vec<usize>,
    passes: Vec<NormalizationPass>,
}

impl Projection {
    fn build(input: &[u8], kind: RootKind) -> Result<Self> {
        validate_plaintext(input)?;
        let title_length = if kind == RootKind::Deck {
            input
                .split_inclusive(|&byte| byte == b'\n')
                .next()
                .context("TINA deck has no title")?
                .len()
        } else {
            0
        };
        let (title, body) = input.split_at(title_length);
        let mut passes = Vec::new();
        let normalized = lexical::normalize_nbsp(body);
        if normalized.is_some() {
            passes.push(NormalizationPass::NormalizeNbspWhitespace);
        }
        let body = normalized.as_deref().unwrap_or(body);
        let line_starts = std::iter::once(0)
            .chain(
                body.iter()
                    .enumerate()
                    .filter_map(|(index, &byte)| (byte == b'\n').then_some(index + 1)),
            )
            .collect::<Vec<_>>();
        let mut edits = Vec::new();
        let mut inserted = Vec::new();
        let mut control = false;
        for statement in lexical::statements(body)? {
            let first = statement
                .text
                .split(u8::is_ascii_whitespace)
                .next()
                .unwrap_or_default();
            if first.eq_ignore_ascii_case(b".control") {
                control = true;
            } else if first.eq_ignore_ascii_case(b".endc") {
                control = false;
            } else if !control && first.eq_ignore_ascii_case(b".subckt") {
                split_inline_parameters(body, &line_starts, &statement, &mut edits, &mut inserted)?;
            }
        }
        let output = lexical::apply_edits(body, edits);
        if output.is_some() {
            passes.push(NormalizationPass::TinaInlineParam);
        }
        let title_lines = usize::from(title_length != 0);
        let mut lines = Vec::new();
        if title_lines != 0 {
            lines.push(1);
        }
        let mut inserted = inserted.into_iter().peekable();
        for (index, _) in body.split_inclusive(|&byte| byte == b'\n').enumerate() {
            let line = index + 1;
            lines.push(line + title_lines);
            while inserted.peek() == Some(&line) {
                lines.push(line + title_lines);
                inserted.next();
            }
        }
        let mut bytes = title.to_vec();
        bytes.extend_from_slice(output.as_deref().unwrap_or(body));
        ensure!(
            inserted.next().is_none()
                && lines.len() == bytes.split_inclusive(|&byte| byte == b'\n').count(),
            "TINA projection source mapping is inconsistent"
        );
        Ok(Self {
            bytes,
            lines,
            passes,
        })
    }
}

fn split_inline_parameters(
    input: &[u8],
    line_starts: &[usize],
    statement: &lexical::Statement,
    edits: &mut Vec<lexical::Edit>,
    inserted: &mut Vec<usize>,
) -> Result<()> {
    let tokens = lexical::tokens(&statement.text)?;
    for (index, token) in tokens.iter().enumerate().skip(2) {
        if !statement.text[token.clone()].eq_ignore_ascii_case(b".param") {
            continue;
        }
        ensure!(
            index + 1 < tokens.len(),
            "line {}: empty inline .PARAM",
            statement.line
        );
        let ranges = statement.source_ranges(token.clone());
        let [range] = ranges.as_slice() else {
            anyhow::bail!("line {}: split inline .PARAM keyword", statement.line);
        };
        if let Some((plus, _)) = statement.continuations.iter().find(|(_, code)| {
            code.contains(&range.start) && input[code.start..range.start].trim_ascii().is_empty()
        }) {
            // Already begins a physical line: promote the continuation into
            // a new local directive without moving any other source byte.
            edits.push((*plus..*plus + 1, vec![b' ']));
        } else {
            let line = line_starts.partition_point(|&start| start <= range.start);
            let physical_start = line_starts[line - 1];
            let physical_end = line_starts
                .get(line)
                .map_or(input.len(), |&start| start - 1);
            let newline = if input[physical_start..physical_end].ends_with(b"\r") {
                b"\r\n".as_slice()
            } else {
                b"\n"
            };
            edits.push((range.start..range.start, newline.to_vec()));
            inserted.push(line);
        }
    }
    Ok(())
}

/// Reject container bytes without guessing a source dialect from the filename.
///
/// # Errors
/// Non-text input or a ZIP container, which requires plaintext netlist export.
pub fn validate_plaintext(input: &[u8]) -> Result<()> {
    lexical::ensure_textual(input).context(
        "TINA input must be a plaintext SPICE netlist, not a schematic or macro container",
    )?;
    ensure!(
        !input.starts_with(b"PK\x03\x04"),
        "TINA input is an archive; export its plaintext SPICE netlist first"
    );
    Ok(())
}

/// Normalize an explicitly selected TINA plaintext SPICE export or macro
/// through the shared PSpice/native normalization, retaining physical source lines.
///
/// # Errors
/// Invalid or non-text source, malformed inline declarations or downstream
/// normalization errors.
pub fn normalize_mapped(input: &[u8], kind: RootKind) -> Result<PspiceNormalization> {
    let projection = Projection::build(input, kind)?;
    let mut result = crate::pspice::normalize_mapped(&projection.bytes, kind)?;
    for line in result.mapped.input_lines.iter_mut().chain(
        result
            .renamed
            .iter_mut()
            .map(|device| &mut device.input_line),
    ) {
        *line = *projection
            .lines
            .get(*line - 1)
            .context("missing TINA source line")?;
    }
    let mut passes = projection.passes;
    passes.append(&mut result.mapped.model.passes);
    result.mapped.model.passes = passes;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_parameters_preserve_title_comments_and_physical_provenance() -> Result<()> {
        let input = b".subckt title .param literal\r\n* .subckt comment .param nope\r\n.subckt one a b .PaRaM k={2 + 3}\r\n+ q=4\r\n.ends\r\n.subckt two a b\r\n+ .param k=2\r\n.ends\r\n.end\r\n";
        let result = normalize_mapped(input, RootKind::Deck)?;
        let text = std::str::from_utf8(&result.mapped.model.bytes)?;
        assert!(
            text.starts_with(".subckt title .param literal\r\n* .subckt comment .param nope\r\n")
        );
        assert!(text.contains(".subckt one a b \r\n.PaRaM k={2 + 3}\r\n+ q=4"));
        assert!(text.contains(".subckt two a b\r\n  .param k=2"));
        assert_eq!(result.mapped.input_lines, [1, 2, 3, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            result.mapped.model.passes,
            [NormalizationPass::TinaInlineParam]
        );
        Ok(())
    }

    #[test]
    fn multiple_local_directives_compose_with_other_source_edits() -> Result<()> {
        let source = b".subckt cell a b .param r=20m .param l=1M\nL1 a b {l} Rser={r}\n.ends\n";
        let result = normalize_mapped(source, RootKind::Include)?;
        let output = std::str::from_utf8(&result.mapped.model.bytes)?;
        let expected = [
            (".subckt cell a b", 1),
            (".param r=20m", 1),
            (".param l=1M", 1),
            ("R__L1_RSER", 2),
            ("L1 ", 2),
            (".ends", 3),
        ];
        assert_eq!(output.lines().count(), expected.len());
        for ((line, origin), (prefix, expected_origin)) in
            output.lines().zip(&result.mapped.input_lines).zip(expected)
        {
            assert!(line.starts_with(prefix), "{line}");
            assert_eq!(*origin, expected_origin);
        }
        assert_eq!(
            result.mapped.model.passes,
            [
                NormalizationPass::TinaInlineParam,
                NormalizationPass::LtspiceInductorRser
            ]
        );
        Ok(())
    }

    #[test]
    fn quoted_and_grouped_tokens_are_not_directives() -> Result<()> {
        let input = b".subckt a b c PARAMS: p={f('.param')}\n.ends\n";
        let result = normalize_mapped(input, RootKind::Include)?;
        assert_eq!(result.mapped.model.bytes, input);
        assert_eq!(result.mapped.model.passes, [] as [NormalizationPass; 0]);
        assert!(normalize_mapped(b".subckt a b c .param\n", RootKind::Include).is_err());
        assert!(normalize_mapped(b"PK\x03\x04abc", RootKind::Include).is_err());
        Ok(())
    }
}
