//! Deterministic compatibility normalization and validation for ngspice model includes.

pub mod conditioning;
pub mod controlled_transfer;
mod lexical;

/// Token spans of a logical SPICE statement, with balanced expression groups
/// and top-level assignment delimiters retained.
/// # Errors
/// Mismatched or unterminated expressions and quotes.
pub fn statement_token_spans(text: &str) -> anyhow::Result<Vec<std::ops::Range<usize>>> {
    lexical::tokens(text.as_bytes())
}
pub mod linked_source;
pub mod model_card;
pub mod param_expression;
pub mod parameter_scope;
pub mod passive_ic;
pub mod pspice;
pub mod tina;
pub mod vdmos_card;
pub mod vdmos_ltspice;
pub mod waveform;

use anyhow::{Context, Result, bail};
use model_preprocess_command::{Cancellation, ProcessLimits, Resource, WorkBudget, capture};
use regex::bytes::Regex;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
    sync::LazyLock,
    time::Duration,
};
use tempfile::TempDir;

static NGSPICE_ERROR_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?im)^(?:fatal|error)[: ]")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static SIMPLIS_MODEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*\.model\s+\S+\s+(?:VCSW|ICSW|VCQPOS|VCQNEG|ICQPOS|ICQNEG|VPWLR|IPWLR|PWLL|PWLC|INV|COMP|XOR|OR[2-9]|NOR[2-9]|AND[2-9]|NAND[2-9]|SRFF|CLK_SRFF|CLK_JKFF|CLK_DFF|CLK_TFF|LATCH)\b")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static SIMPLIS_DEVICE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*![RLCQSVIEFGH]\S*\s+")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static SIMPLIS_PREPROCESSOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^\s*\.(?:VAR|GLOBALVAR|WHILE|ENDWHILE|ERROR)\b")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});

/// One deterministic compatibility rewrite applied to a plaintext model.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationPass {
    /// Replace Latin-1 or UTF-8 non-breaking spaces with ordinary spaces.
    NormalizeNbspWhitespace,
    /// Expand `LTspice`'s `L ... Rser=` extension to an ideal inductor plus resistor.
    LtspiceInductorRser,
    /// Normalize controlled TABLE syntax and lower FREQ to native xfer models.
    ControlledSourceTransfer,
    /// Map `PSpice` `arctan()` to ngspice `atan()`.
    PspiceArctan,
    /// Convert positional continued `PSpice` parameters to `name=value` form.
    PspicePositionalParams,
    /// Join a separated PARAMS : marker on subcircuit definitions and calls.
    PspiceParamsMarker,
    /// Separate TINA inline subcircuit-local parameter declarations.
    TinaInlineParam,
    /// Lower explicit `PSpice` source semantics to native SPICE.
    PspiceSource,
    /// Comment a standalone `.END` so the model remains safe to include.
    StripStandaloneEnd,
    /// Scale proven private differentiator currents and compensate feedback.
    PrivateDifferentiatorConditioning,
}

/// ngspice parser mode that accepted the normalized artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityMode {
    /// Native ngspice syntax.
    Native,
    /// ngspice's `PSpice` compatibility mode (`set ngbehavior=ps`).
    Pspice,
}

/// Result of deterministic normalization before simulator validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NormalizedModel {
    /// Include-safe normalized bytes.
    pub bytes: Vec<u8>,
    /// Rewrites that actually changed bytes, in application order.
    pub passes: Vec<NormalizationPass>,
}

/// Result of validating the exact normalized bytes with ngspice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationResult {
    /// Whether ngspice accepted the exact artifact.
    pub ok: bool,
    /// First compatibility mode that accepted it.
    pub mode: Option<CompatibilityMode>,
    /// Bounded tail of ngspice output, or a timeout/execution diagnostic.
    pub detail: String,
}

/// Return true when the bytes contain strong SIMPLIS-only syntax.
#[must_use = "the result determines whether SIMPLIS handling is required"]
pub fn looks_like_simplis(input: &[u8]) -> bool {
    SIMPLIS_MODEL_RE.is_match(input)
        || SIMPLIS_DEVICE_RE.is_match(input)
        || SIMPLIS_PREPROCESSOR_RE.is_match(input)
}

use lexical::{ensure_textual, normalize_nbsp, split_line_ending};

/// Reserve source identifiers, then allocate names with a monotonic cursor
/// per canonical base. Scoped repetitions do not restart collision searches.
struct IdentifierAllocator {
    used: BTreeSet<Vec<u8>>,
    next: BTreeMap<Vec<u8>, usize>,
    budget: WorkBudget,
}

impl IdentifierAllocator {
    fn new(input: &[u8], budget: &WorkBudget) -> Result<Self> {
        let mut allocator = Self {
            used: BTreeSet::new(),
            next: BTreeMap::new(),
            budget: budget.clone(),
        };
        budget.charge(Resource::ScannedBytes, input.len().saturating_mul(2))?;
        // Reserve full punctuation-bearing names and plain identifiers
        // embedded within expressions.
        for token in input
            .split(|byte| {
                byte.is_ascii_whitespace()
                    || matches!(
                        *byte,
                        b'[' | b']' | b'(' | b')' | b'{' | b'}' | b',' | b';' | b'=' | b'"' | b'\''
                    )
            })
            .chain(input.split(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')))
        {
            if !token.is_empty() {
                budget.charge(Resource::GeneratedNodes, 1)?;
                budget.charge(Resource::EmittedBytes, token.len())?;
                allocator.used.insert(token.to_ascii_uppercase());
            }
        }
        Ok(allocator)
    }

    fn fresh(&mut self, base: &[u8]) -> Result<Vec<u8>> {
        self.budget.charge(Resource::EmittedBytes, base.len())?;
        self.budget.charge(Resource::GeneratedNodes, 1)?;
        let key = base.to_ascii_uppercase();
        let next = self.next.entry(key).or_default();
        loop {
            self.budget.charge(Resource::EvaluationSteps, 1)?;
            let suffix = if *next == 0 {
                String::new()
            } else {
                format!("_{next}")
            };
            self.budget.charge(
                Resource::EmittedBytes,
                base.len().saturating_add(suffix.len()),
            )?;
            let mut candidate = base.to_vec();
            candidate.extend_from_slice(suffix.as_bytes());
            *next = next
                .checked_add(1)
                .context("generated identifier overflow")?;
            self.budget
                .charge(Resource::EmittedBytes, candidate.len())?;
            self.budget.charge(Resource::GeneratedNodes, 1)?;
            if self.used.insert(candidate.to_ascii_uppercase()) {
                return Ok(candidate);
            }
        }
    }
}

/// Find the unique top-level series-resistance assignment after the device
/// name, two nodes and inductance. Names occurring within values are not keys.
fn rser_assignment(
    text: &[u8],
    fields: &[std::ops::Range<usize>],
    line: usize,
) -> Result<Option<(std::ops::Range<usize>, std::ops::Range<usize>)>> {
    let mut rser = None;
    for index in 4..fields.len() {
        if !text[fields[index].clone()].eq_ignore_ascii_case(b"rser")
            || text[fields[index - 1].clone()] == *b"="
            || fields
                .get(index + 1)
                .is_none_or(|span| text[span.clone()] != *b"=")
        {
            continue;
        }
        let value = fields
            .get(index + 2)
            .filter(|span| text[(*span).clone()] != *b"=")
            .filter(|_| {
                fields
                    .get(index + 3)
                    .is_none_or(|span| text[span.clone()] != *b"=")
            })
            .with_context(|| format!("line {line}: Rser requires a value"))?;
        anyhow::ensure!(rser.is_none(), "line {line}: duplicate Rser assignment");
        rser = Some((fields[index].start..value.end, value.clone()));
    }
    Ok(rser)
}

fn rewrite_ltspice_inductor_rser(
    input: &[u8],
    inserted_lines: &mut Vec<usize>,
    budget: &WorkBudget,
) -> Result<Option<Vec<u8>>> {
    use lexical::{replace_span, statements, tokens};
    if !input
        .windows(4)
        .any(|word| word.eq_ignore_ascii_case(b"rser"))
    {
        return Ok(None);
    }
    let mut used = IdentifierAllocator::new(input, budget)?;
    let mut edits = Vec::new();
    let mut serial = 0;
    let mut in_control = false;
    for statement in statements(input)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            in_control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            in_control = false;
        }
        if in_control
            || !matches!(first.first(), Some(b'l' | b'L'))
            || !text
                .windows(4)
                .any(|word| word.eq_ignore_ascii_case(b"rser"))
        {
            continue;
        }
        let fields = tokens(text)?;
        if fields.len() < 5 {
            continue;
        }
        let Some((assignment, resistance)) = rser_assignment(text, &fields, statement.line)? else {
            continue;
        };
        let ind = &text[fields[0].clone()];
        let node = &text[fields[1].clone()];
        let mut resistor_base = b"R__".to_vec();
        resistor_base.extend_from_slice(ind);
        resistor_base.extend_from_slice(b"_RSER");
        let resistor = used.fresh(&resistor_base)?;
        let mut internal_base = b"__".to_vec();
        internal_base.extend_from_slice(ind);
        write!(&mut internal_base, "_rser_{serial}")?;
        let internal = used.fresh(&internal_base)?;
        serial += 1;
        let physical = input[statement.start..]
            .split_inclusive(|b| *b == b'\n')
            .next()
            .unwrap_or_default();
        let (body, ending) = split_line_ending(physical);
        let mut line = body[..body.len() - body.trim_ascii_start().len()].to_vec();
        for word in [&resistor[..], node, &internal, &text[resistance]] {
            line.extend_from_slice(word);
            line.push(b' ');
        }
        line.pop();
        line.extend_from_slice(if ending.is_empty() { b"\n" } else { ending });
        edits.push((statement.start..statement.start, line));
        inserted_lines.push(statement.line);
        replace_span(&statement, fields[1].clone(), &internal, &mut edits);
        // Remove preceding horizontal separation on the same logical line.
        // If the separator is synthetic its source range is empty.
        let start = text[..assignment.start].trim_ascii_end().len();
        let remove = start..assignment.end;
        let removed = statement.source_ranges(remove.clone());
        replace_span(&statement, remove, b"", &mut edits);
        for (plus, code) in &statement.continuations {
            if input[code.clone()].iter().enumerate().all(|(i, byte)| {
                byte.is_ascii_whitespace()
                    || removed
                        .iter()
                        .any(|range| range.contains(&(code.start + i)))
            }) {
                // A wholly consumed continuation remains a comment, retaining
                // its physical location and any existing inline comment.
                edits.push((*plus..*plus + 1, vec![b'*']));
            }
        }
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

fn params_header_end(text: &[u8], fields: &[std::ops::Range<usize>]) -> Option<usize> {
    for (index, span) in fields.iter().enumerate() {
        let field = &text[span.clone()];
        if field.eq_ignore_ascii_case(b"params:") {
            return Some(index + 1);
        }
        if field.eq_ignore_ascii_case(b"params")
            && fields
                .get(index + 1)
                .is_some_and(|next| text[next.clone()] == *b":")
        {
            return Some(index + 2);
        }
    }
    None
}

/// Locate the expression-bearing part of cards without interpreting their
/// positional node/model identifiers as arithmetic.
fn expression_start(text: &[u8], first: &[u8]) -> Result<Option<usize>> {
    let fields = lexical::tokens(text)?;
    let assignment = || {
        fields
            .iter()
            .position(|span| text[span.clone()] == *b"=")
            .map(|index| index + 1)
    };
    let lower = first.to_ascii_lowercase();
    let index = match lower.as_slice() {
        b".param" | b".if" | b".elseif" => Some(1),
        b".model" => Some(2),
        b".func" => Some(
            if fields
                .get(1)
                .is_some_and(|span| text[span.clone()].contains(&b'('))
            {
                2
            } else {
                3
            },
        ),
        b".subckt" => params_header_end(text, &fields).or_else(assignment),
        [b'b' | b'r' | b'l' | b'c' | b'v' | b'i', ..] => Some(3),
        [b'f' | b'h', ..] => Some(4),
        [b'e' | b'g', ..] => {
            if fields.get(3).is_some_and(|span| {
                let field = &text[span.clone()];
                [b"value".as_slice(), b"table", b"poly", b"vol", b"cur"]
                    .iter()
                    .any(|keyword| {
                        field
                            .get(..keyword.len())
                            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                            && field.get(keyword.len()).is_none_or(|b| *b == b'(')
                    })
            }) {
                Some(3)
            } else {
                Some(5)
            }
        }
        [
            b'd' | b'j' | b'q' | b'm' | b'x' | b's' | b'w' | b't' | b'o' | b'u' | b'k' | b'a'
            | b'z',
            ..,
        ] => assignment(),
        _ => None,
    };
    Ok(index.and_then(|at| fields.get(at)).map(|span| span.start))
}

fn rewrite_pspice_arctan(input: &[u8], budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    if !input
        .windows(6)
        .any(|word| word.eq_ignore_ascii_case(b"arctan"))
    {
        return Ok(None);
    }
    let mut edits = Vec::new();
    let mut in_control = false;
    for statement in lexical::statements(input)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            in_control = true;
        } else if first.eq_ignore_ascii_case(b".endc") {
            in_control = false;
        }
        if in_control {
            continue;
        }
        if !text
            .windows(6)
            .any(|word| word.eq_ignore_ascii_case(b"arctan"))
        {
            continue;
        }
        let Some(start) = expression_start(text, first)? else {
            continue;
        };
        for span in lexical::function_names(text, b"arctan", start) {
            lexical::replace_span(&statement, span, b"atan", &mut edits);
        }
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

fn rewrite_spaced_parameter_marker(input: &[u8], budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    if !input
        .windows(6)
        .any(|word| word.eq_ignore_ascii_case(b"params"))
    {
        return Ok(None);
    }
    let mut edits = Vec::new();
    let mut control = false;
    for statement in lexical::statements(input)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if first.eq_ignore_ascii_case(b".control") {
            control = true;
            continue;
        }
        if first.eq_ignore_ascii_case(b".endc") {
            control = false;
            continue;
        }
        if control
            || !(first.eq_ignore_ascii_case(b".subckt")
                || first.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'x')))
        {
            continue;
        }
        let fields = lexical::tokens(text)?;
        let Some(index) = params_header_end(text, &fields) else {
            continue;
        };
        if index < 4 || text[fields[index - 1].clone()] != *b":" {
            continue;
        }
        // The colon is a marker only when followed by an assignment; leave
        // unrelated identifiers and punctuation alone.
        if fields
            .get(index + 1)
            .is_none_or(|span| text[span.clone()] != *b"=")
        {
            continue;
        }
        let marker = statement.source_ranges(fields[index - 2].clone());
        let colon = statement.source_ranges(fields[index - 1].clone());
        let ([marker], [colon]) = (marker.as_slice(), colon.as_slice()) else {
            bail!("line {}: split parameter marker", statement.line);
        };
        edits.push((marker.end..marker.end, vec![b':']));
        edits.push((colon.clone(), Vec::new()));
        for (plus, code) in &statement.continuations {
            if code.contains(&colon.start)
                && input[code.start..colon.start].trim_ascii().is_empty()
                && input[colon.end..code.end].trim_ascii().is_empty()
            {
                edits.push((*plus..*plus + 1, vec![b'*']));
            }
        }
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

fn rewrite_pspice_positional_params(input: &[u8], budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    if !input
        .windows(6)
        .any(|word| word.eq_ignore_ascii_case(b"params"))
    {
        return Ok(None);
    }
    let mut edits = Vec::new();
    for statement in lexical::statements(input)? {
        let text = &statement.text;
        let first = text
            .split(u8::is_ascii_whitespace)
            .next()
            .unwrap_or_default();
        if !first.eq_ignore_ascii_case(b".subckt") {
            continue;
        }
        // Restrict the vendor positional syntax to the explicit PARAMS:
        // section of a subcircuit declaration and physical continuation
        // boundaries. Existing assignments and split expressions stay intact.
        let fields = lexical::tokens(text)?;
        let Some(mut index) = params_header_end(text, &fields) else {
            continue;
        };
        while index < fields.len() {
            let name = &fields[index];
            let Some(value) = fields.get(index + 1) else {
                break;
            };
            if text[value.clone()] == *b"=" {
                index += 3;
                continue;
            }
            let ranges = statement.source_ranges(name.clone());
            let Some(range) = ranges.first() else { break };
            let is_continuation_start = statement.continuations.iter().any(|(_, code)| {
                code.contains(&range.start)
                    && input[code.start..range.start].trim_ascii().is_empty()
            });
            let identifier = &text[name.clone()];
            if is_continuation_start
                && identifier
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                && identifier
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
                && fields
                    .get(index + 2)
                    .is_none_or(|span| text[span.clone()] != *b"=")
                && let Some(value_range) = statement.source_ranges(value.clone()).first()
                && value_range.start >= range.end
                && !input[range.end..value_range.start].contains(&b'\n')
            {
                edits.push((range.end..value_range.start, vec![b'=']));
                index += 2;
            } else {
                index += 1;
            }
        }
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

fn strip_standalone_end(input: &[u8], budget: &WorkBudget) -> Result<Option<Vec<u8>>> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    let mut edits = Vec::new();
    let mut in_control = false;
    for statement in lexical::statements(input)? {
        if statement.text.eq_ignore_ascii_case(b".control") {
            in_control = true;
        } else if statement.text.eq_ignore_ascii_case(b".endc") {
            in_control = false;
        } else if !in_control && statement.text.eq_ignore_ascii_case(b".end") {
            lexical::replace_span(
                &statement,
                0..statement.text.len(),
                b"* .END stripped for ngspice include",
                &mut edits,
            );
        }
    }
    lexical::apply_budgeted_edits(input, edits, budget)
}

fn apply_optional<F>(
    current: &mut Vec<u8>,
    passes: &mut Vec<NormalizationPass>,
    pass: NormalizationPass,
    rewrite: F,
) where
    F: FnOnce(&[u8]) -> Option<Vec<u8>>,
{
    if let Some(rewritten) = rewrite(current)
        && rewritten != *current
    {
        *current = rewritten;
        passes.push(pass);
    }
}

/// Normalize a textual model into a passive, include-safe ngspice artifact.
///
/// The function is deliberately narrow: it applies only compatibility rewrites proven by
/// corpus failures and preserves every unrelated byte verbatim.
///
/// # Errors
///
/// Returns an error for binary input or source identified as SIMPLIS, which requires
/// semantic translation rather than syntax normalization.
pub fn normalize_ngspice(input: &[u8]) -> Result<NormalizedModel> {
    normalize_body(input, true, &mut Vec::new(), &WorkBudget::default())
}

/// Normalized bytes and one original input line for each emitted physical line.
#[derive(Debug, Clone)]
pub struct MappedNormalization {
    /// Normalized artifact and applied passes.
    pub model: NormalizedModel,
    /// 1-based physical input lines; synthetic elements inherit the source line.
    pub input_lines: Vec<usize>,
}

/// Normalize either an include or a complete deck, preserving the deck title
/// and terminator. Every output line maps to its originating input line.
///
/// # Errors
///
/// Invalid source, unsupported dialect, malformed compatibility syntax, or
/// inconsistent source mapping.
pub fn normalize_mapped(
    input: &[u8],
    kind: linked_source::RootKind,
    budget: &WorkBudget,
) -> Result<MappedNormalization> {
    ensure_textual(input)?;
    let (title, body) = if kind == linked_source::RootKind::Deck {
        let title = input
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .context("native deck has no title line")?;
        (title, &input[title.len()..])
    } else {
        (&[][..], input)
    };
    let mut inserted = Vec::new();
    let mut model = normalize_body(
        body,
        kind == linked_source::RootKind::Include,
        &mut inserted,
        budget,
    )?;
    let title_lines = usize::from(!title.is_empty());
    let mut input_lines = Vec::new();
    if title_lines != 0 {
        input_lines.push(1);
        budget.charge(
            Resource::EmittedBytes,
            title.len().saturating_add(model.bytes.len()),
        )?;
        let mut bytes = Vec::with_capacity(title.len() + model.bytes.len());
        bytes.extend_from_slice(title);
        bytes.extend_from_slice(&model.bytes);
        model.bytes = bytes;
    }
    budget.charge(
        Resource::GeneratedNodes,
        model.bytes.split_inclusive(|byte| *byte == b'\n').count(),
    )?;
    let mut inserted = inserted.into_iter().peekable();
    for (index, _) in body.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let line = index + 1;
        while inserted.peek() == Some(&line) {
            input_lines.push(line + title_lines);
            inserted.next();
        }
        input_lines.push(line + title_lines);
    }
    anyhow::ensure!(
        inserted.next().is_none()
            && input_lines.len() == model.bytes.split_inclusive(|byte| *byte == b'\n').count(),
        "normalization source mapping is inconsistent"
    );
    Ok(MappedNormalization { model, input_lines })
}

fn normalize_body(
    input: &[u8],
    strip_end: bool,
    inserted_lines: &mut Vec<usize>,
    budget: &WorkBudget,
) -> Result<NormalizedModel> {
    budget.charge(Resource::ScannedBytes, input.len())?;
    budget.charge(Resource::EmittedBytes, input.len())?;
    ensure_textual(input)?;
    if looks_like_simplis(input) {
        bail!("SIMPLIS source requires translation, not ngspice normalization");
    }

    let mut bytes = input.to_vec();
    let mut passes = Vec::new();
    budget.charge(Resource::ScannedBytes, bytes.len())?;
    if bytes.contains(&0xa0) {
        budget.charge(Resource::EmittedBytes, bytes.len())?;
    }
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::NormalizeNbspWhitespace,
        normalize_nbsp,
    );
    if let Some(rewritten) = rewrite_ltspice_inductor_rser(&bytes, inserted_lines, budget)? {
        bytes = rewritten;
        passes.push(NormalizationPass::LtspiceInductorRser);
    }
    if let Some(rewritten) = rewrite_pspice_arctan(&bytes, budget)? {
        bytes = rewritten;
        passes.push(NormalizationPass::PspiceArctan);
    }
    if let Some(rewritten) = rewrite_pspice_positional_params(&bytes, budget)? {
        bytes = rewritten;
        passes.push(NormalizationPass::PspicePositionalParams);
    }
    if let Some(rewritten) = rewrite_spaced_parameter_marker(&bytes, budget)? {
        bytes = rewritten;
        passes.push(NormalizationPass::PspiceParamsMarker);
    }
    if strip_end && let Some(rewritten) = strip_standalone_end(&bytes, budget)? {
        bytes = rewritten;
        passes.push(NormalizationPass::StripStandaloneEnd);
    }
    if let Some(lowered) = controlled_transfer::lower_sources(&bytes, budget)? {
        let mut origins = Vec::new();
        let mut previous = inserted_lines.iter().copied().peekable();
        for line in 1..=input.split_inclusive(|byte| *byte == b'\n').count() {
            while previous.peek() == Some(&line) {
                origins.push(line);
                previous.next();
            }
            origins.push(line);
        }
        for line in lowered.inserted_lines {
            inserted_lines.push(
                *origins
                    .get(line - 1)
                    .context("missing transfer source line")?,
            );
        }
        inserted_lines.sort_unstable();
        bytes = lowered.bytes;
        passes.push(NormalizationPass::ControlledSourceTransfer);
    }
    Ok(NormalizedModel { bytes, passes })
}

fn ngspice_once(
    data: &[u8],
    suffix: &str,
    timeout: Duration,
    mode: CompatibilityMode,
) -> Result<ValidationResult> {
    let temp = TempDir::new().context("creating ngspice validation directory")?;
    let suffix = if suffix.starts_with('.') {
        suffix
    } else {
        ".lib"
    };
    anyhow::ensure!(
        suffix[1..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "invalid model suffix"
    );
    let model_name = format!("model{suffix}");
    fs::write(temp.path().join(&model_name), data)?;
    if mode == CompatibilityMode::Pspice {
        fs::write(temp.path().join(".spiceinit"), b"set ngbehavior=ps\n")?;
    }
    fs::write(
        temp.path().join("deck.cir"),
        format!(
            ".title normalized-model-parse-check\n.include \"{model_name}\"\nV__MODEL_CHECK ncheck 0 1\nR__MODEL_CHECK ncheck 0 1k\n.op\n.end\n"
        ),
    )?;
    let output = capture(
        Path::new("ngspice"),
        temp.path(),
        &["-b".into(), "deck.cir".into()],
        &[],
        ProcessLimits {
            timeout,
            output_bytes: 4 * 1024 * 1024,
            diagnostic_bytes: 4 * 1024 * 1024,
        },
        &Cancellation::default(),
    )
    .context("running bounded ngspice validation")?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let ok = output.status.success() && !NGSPICE_ERROR_RE.is_match(&combined);
    let mut detail_lines: Vec<&str> = combined
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if detail_lines.len() > 24 {
        detail_lines.drain(..detail_lines.len() - 24);
    }
    let mut detail = detail_lines.join("\n");
    if detail.len() > 4_000 {
        let split_at = detail.len() - 4_000;
        let split_at = detail.ceil_char_boundary(split_at);
        detail = detail[split_at..].to_owned();
    }
    if detail.is_empty() {
        detail = format!("ngspice exited with status {}", output.status);
    }
    Ok(ValidationResult {
        ok,
        mode: ok.then_some(mode),
        detail,
    })
}

/// Validate the exact emitted model in native ngspice mode, then `PSpice` compatibility mode.
///
/// # Errors
///
/// Returns an error if the model is not textual, the validation sandbox cannot be created,
/// or ngspice cannot be started or managed.
pub fn validate_ngspice(data: &[u8], suffix: &str, timeout: Duration) -> Result<ValidationResult> {
    ensure_textual(data)?;
    let native = ngspice_once(data, suffix, timeout, CompatibilityMode::Native)?;
    if native.ok {
        return Ok(native);
    }
    let pspice = ngspice_once(data, suffix, timeout, CompatibilityMode::Pspice)?;
    if pspice.ok {
        return Ok(pspice);
    }
    Ok(ValidationResult {
        ok: false,
        mode: None,
        detail: pspice.detail,
    })
}

/// Normalize and then validate the exact normalized bytes with ngspice.
///
/// # Errors
///
/// Returns any normalization or ngspice execution error.
pub fn normalize_and_validate(
    input: &[u8],
    suffix: &str,
    timeout: Duration,
) -> Result<(NormalizedModel, ValidationResult)> {
    let normalized = normalize_ngspice(input)?;
    let validation = validate_ngspice(&normalized.bytes, suffix, timeout)?;
    Ok((normalized, validation))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn rewrites_only_targeted_syntax() -> Result<()> {
        let input = b"* hdr\n  wrapped prose\n.subckt x a b\n+ PARAMS:\n+ gain 2\nL1 a b 2u Rser=30m\nB1 x 0 v=arctan(v(a))\n.ends x\n.END\n";
        let normalized = normalize_ngspice(input)?;
        assert!(
            normalized
                .bytes
                .windows(b"  wrapped prose".len())
                .any(|w| w == b"  wrapped prose")
        );
        assert!(
            normalized
                .bytes
                .windows(b"R__L1_RSER a __L1_rser_0 30m".len())
                .any(|w| w == b"R__L1_RSER a __L1_rser_0 30m")
        );
        assert!(
            normalized
                .bytes
                .windows(b"+ gain=2".len())
                .any(|w| w == b"+ gain=2")
        );
        assert!(
            normalized
                .bytes
                .windows(b"atan(".len())
                .any(|w| w == b"atan(")
        );
        assert!(
            normalized
                .bytes
                .windows(b"* .END stripped for ngspice include".len())
                .any(|w| w == b"* .END stripped for ngspice include")
        );
        assert_eq!(normalized.passes.len(), 4);
        Ok(())
    }

    #[test]
    fn inductor_rser_rewrite_never_reuses_existing_device_or_node_names() -> Result<()> {
        let input = b".subckt filter a b\nR__L1_RSER a b 1k\nRload __L1_rser_0 0 10k\nL1 a b 2u Rser=30m\n.ends filter\n";
        let normalized = normalize_ngspice(input)?;
        let text = std::str::from_utf8(&normalized.bytes)?;
        assert!(text.contains("R__L1_RSER a b 1k"));
        assert!(text.contains("Rload __L1_rser_0 0 10k"));
        assert!(
            text.contains("R__L1_RSER_1 a __L1_rser_0_1 30m"),
            "new series resistor must have a fresh element identifier and internal net: {text}"
        );
        assert!(text.contains("L1 __L1_rser_0_1 b 2u"));
        Ok(())
    }

    #[test]
    fn final_rser_inductor_without_newline_emits_two_distinct_statements() -> Result<()> {
        let normalized = normalize_ngspice(b"L1 a b 2u Rser=30m")?;
        assert_eq!(
            normalized.bytes,
            b"R__L1_RSER a __L1_rser_0 30m\nL1 __L1_rser_0 b 2u"
        );
        Ok(())
    }

    #[test]
    fn preserves_non_utf8_unmatched_bytes() -> Result<()> {
        let input = b"* Toshiba \x81 comment\n.subckt x a b\nR1 a b 1k\n.ends x\n";
        let normalized = normalize_ngspice(input)?;
        assert_eq!(normalized.bytes, input);
        assert_eq!(normalized.passes, Vec::<NormalizationPass>::new());
        Ok(())
    }

    #[test]
    fn indented_device_after_comment_is_not_prose() -> Result<()> {
        let input = b"* header\n  R1 a b 1k\n\tL1 a b 2u\n  C1 b 0 10p\n";
        let normalized = normalize_ngspice(input)?;
        assert_eq!(normalized.bytes, input);
        assert_eq!(normalized.passes, [] as [NormalizationPass; 0]);
        Ok(())
    }

    #[test]
    fn nbsp_rewrite_preserves_utf8_characters_and_literal_bytes() -> Result<()> {
        let input = b"* \xc2\xa0 \xc3\xa0 \x81\nR1\xc2\xa0a\xa0b 1k ; \xa0\n.include \"a\xc2\xa0b\xc3\xa0.lib\"\nR2 n\xe0\xa0\x80 b 2k\n";
        let expected = b"* \xc2\xa0 \xc3\xa0 \x81\nR1 a b 1k ; \xa0\n.include \"a\xc2\xa0b\xc3\xa0.lib\"\nR2 n\xe0\xa0\x80 b 2k\n";
        let normalized = normalize_ngspice(input)?;
        assert_eq!(normalized.bytes, expected);
        assert_eq!(
            normalized.passes,
            vec![NormalizationPass::NormalizeNbspWhitespace]
        );
        Ok(())
    }

    #[test]
    fn nbsp_indentation_does_not_rewrite_comment_payload() -> Result<()> {
        let input = b"\xc2\xa0* keep\xc2\xa0this\nR1 a b 1k\xa0$ keep\xa0this\n";
        let expected = b" * keep\xc2\xa0this\nR1 a b 1k $ keep\xa0this\n";
        assert_eq!(normalize_ngspice(input)?.bytes, expected);
        Ok(())
    }

    #[test]
    fn end_statement_is_commented_without_discarding_trailing_text() -> Result<()> {
        let input = b"* .end\r\n.subckt x a b\r\nR1 a b 1k\r\n.ends x\r\n .END ; marker";
        let expected = b"* .end\r\n.subckt x a b\r\nR1 a b 1k\r\n.ends x\r\n * .END stripped for ngspice include ; marker";
        assert_eq!(normalize_ngspice(input)?.bytes, expected);
        Ok(())
    }

    #[test]
    fn nul_after_original_sample_boundary_is_rejected_everywhere() {
        let mut input = vec![b' '; 131_073];
        input.push(0);
        assert!(normalize_ngspice(&input).is_err());
        assert!(model_card::parse_model_cards(&input).is_err());
        assert!(validate_ngspice(&input, ".lib", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn rser_balanced_expressions_and_continuations_preserve_comments() -> Result<()> {
        let input = b".subckt x a b\r\n  Lfoo a b {2 * (1u + 1u)} IC=2m ; first\r\n* between\r\n  + Rser = {30m +\r\n  + (2 * 5m)} IC=3m ; second\r\n.ends x";
        let normalized = normalize_ngspice(input)?;
        let text = std::str::from_utf8(&normalized.bytes)?;
        assert!(text.contains("  R__Lfoo_RSER a __Lfoo_rser_0 {30m + (2 * 5m)}\r\n"));
        assert!(text.contains("  Lfoo __Lfoo_rser_0 b {2 * (1u + 1u)} IC=2m ; first\r\n"));
        assert!(text.contains("* between\r\n"));
        assert!(text.contains("IC=3m ; second\r\n"));
        assert!(text.ends_with(".ends x"));
        let statements = model_card::logical_statements(&normalized.bytes)?;
        assert_eq!(statements.len(), 4);
        assert_eq!(
            statements[2].1,
            b"Lfoo __Lfoo_rser_0 b {2 * (1u + 1u)} IC=2m IC=3m"
        );
        assert_eq!(
            normalize_ngspice(&normalized.bytes)?.bytes,
            normalized.bytes
        );
        Ok(())
    }

    #[test]
    fn continued_rser_matches_analytic_dc_and_ac_in_native_ngspice() -> Result<()> {
        let model = normalize_ngspice(
            b".subckt coil a b\n  Lfoo a b {2 * (1u + 1u)}\n+ Rser = {30m +\n+ (2 * 5m)}\n.ends coil\n",
        )?;
        let directory = TempDir::new()?;
        fs::write(directory.path().join("coil.lib"), model.bytes)?;
        fs::write(
            directory.path().join("deck.cir"),
            b".title Series-RL reference\n.include coil.lib\nVsrc a 0 dc 1 ac 1\nXcoil a 0 coil\n.control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat i(Vsrc)\nac lin 3 1000 3000\nwrdata ac.dat i(Vsrc)\nquit\n.endc\n.end\n",
        )?;
        let output = Command::new("ngspice")
            .args(["-b", "-n", "deck.cir"])
            .current_dir(directory.path())
            .output()?;
        assert!(
            output.status.success(),
            "ngspice: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let op = fs::read_to_string(directory.path().join("op.dat"))?;
        let current = op
            .split_ascii_whitespace()
            .nth(1)
            .context("missing DC current")?
            .parse::<f64>()?;
        let resistance = 0.04;
        let inductance = 4e-6;
        assert!((current + 1.0 / resistance).abs() < 1e-10);
        let ac = fs::read_to_string(directory.path().join("ac.dat"))?;
        assert_eq!(ac.lines().count(), 3);
        for line in ac.lines() {
            let values = line
                .split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let [frequency, real, imaginary] = values.as_slice() else {
                bail!("unexpected AC result: {line}");
            };
            let reactance = std::f64::consts::TAU * frequency * inductance;
            let denominator = reactance.mul_add(reactance, resistance * resistance);
            assert!((real + resistance / denominator).abs() < 1e-10);
            assert!((imaginary - reactance / denominator).abs() < 1e-10);
        }
        Ok(())
    }

    #[test]
    fn rser_is_only_a_top_level_inductor_assignment() -> Result<()> {
        let input = b"* L1 a b 2u Rser=5\nL1 a b {Rser + 1u} IC=0 ; Rser=5\nR1 a b 2k Rser=5\n.include \"L1 a b 2u Rser=5\"\n";
        assert_eq!(normalize_ngspice(input)?.bytes, input);
        assert!(normalize_ngspice(b"L1 a b 2u Rser={1 + (2}").is_err());
        assert!(normalize_ngspice(b"L1 a b 2u Rser=1 Rser=2").is_err());
        assert!(normalize_ngspice(b"L1 a b 2u Rser=").is_err());
        Ok(())
    }

    #[test]
    fn arctan_does_not_change_comments_titles_or_paths() -> Result<()> {
        let input = b"* arctan(x)\n.title arctan(x)\n.include 'arctan(x).lib'\nB1 a 0 V=arctan (v(b)) ; arctan(x)\nR1 'arctan(x)' b 1k\n";
        let expected = b"* arctan(x)\n.title arctan(x)\n.include 'arctan(x).lib'\nB1 a 0 V=atan (v(b)) ; arctan(x)\nR1 'arctan(x)' b 1k\n";
        assert_eq!(normalize_ngspice(input)?.bytes, expected);
        Ok(())
    }

    #[test]
    fn spaced_parameter_markers_keep_physical_lines_and_non_markers() -> Result<()> {
        let source = b".subckt c a b PARAMS : R=1k\nX1 a b c params\n+ :\n+ R=2k\nX2 params : c\nR1 a b {f('params :')}\n.ends\n";
        let result = normalize_mapped(
            source,
            linked_source::RootKind::Include,
            &WorkBudget::default(),
        )?;
        let actual = std::str::from_utf8(&result.model.bytes)?;
        assert!(actual.contains("PARAMS:  R=1k"));
        assert!(actual.contains("params:\n* \n+ R=2k"));
        assert!(actual.contains("X2 params : c"));
        assert!(actual.contains("R1 a b {f('params :')}"));
        assert_eq!(result.input_lines, (1..=7).collect::<Vec<_>>());
        assert_eq!(result.model.passes, [NormalizationPass::PspiceParamsMarker]);
        Ok(())
    }

    #[test]
    fn positional_rewrite_is_scoped_to_subcircuit_params() -> Result<()> {
        let input = b".subckt x a b\n+ PARAMS :\n+ gain {2 +\n+ 3}\n+ k = {4 + 5}\n+ off 1\n.ends x\nL1 a b 1u\n+ PARAMS:\n+ gain 2\n";
        let expected = b".subckt x a b\n+ PARAMS: \n+ gain={2 +\n+ 3}\n+ k = {4 + 5}\n+ off=1\n.ends x\nL1 a b 1u\n+ PARAMS:\n+ gain 2\n";
        assert_eq!(normalize_ngspice(input)?.bytes, expected);
        Ok(())
    }

    #[test]
    fn rejects_simplis_as_translation_problem() {
        let input = b".MODEL SW VCSW RON=1m ROFF=1MEG TH=2 HYSTWD=1 LOGIC=POS\n";
        assert!(looks_like_simplis(input));
        assert!(normalize_ngspice(input).is_err());
    }
}

#[cfg(test)]
mod identifier_budget_tests {
    use super::*;

    #[test]
    fn repeated_case_folded_bases_do_not_rescan_prior_candidates() -> Result<()> {
        let budget = WorkBudget::default();
        let mut allocator = IdentifierAllocator::new(b"R__l1_rser R__L1_RSER_1", &budget)?;
        let before = budget.remaining(Resource::EvaluationSteps)?;
        for index in 2..10_002 {
            let base = if index % 2 == 0 {
                b"R__L1_RSER"
            } else {
                b"r__l1_rser"
            };
            let actual = allocator.fresh(base)?;
            let expected = format!("{}_{index}", std::str::from_utf8(base)?);
            assert_eq!(actual, expected.as_bytes());
        }
        assert_eq!(
            before - budget.remaining(Resource::EvaluationSteps)?,
            10_002
        );
        Ok(())
    }
}
