//! Byte-oriented source lexing. Preserve physical offsets for selective edits.
use anyhow::{Result, ensure};
use std::ops::Range;

pub fn ensure_textual(input: &[u8]) -> Result<()> {
    ensure!(
        !input.contains(&0),
        "ngspice normalization requires a textual model"
    );
    Ok(())
}

#[derive(Default)]
struct Context {
    quote: Option<u8>,
    escaped: bool,
    braces: usize,
}

impl Context {
    const fn advance(&mut self, byte: u8) {
        if let Some(quote) = self.quote {
            if self.escaped {
                self.escaped = false;
            } else if byte == b'\\' {
                self.escaped = true;
            } else if byte == quote {
                self.quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' => self.quote = Some(byte),
                b'{' => self.braces += 1,
                b'}' => self.braces = self.braces.saturating_sub(1),
                _ => {}
            }
        }
    }

    fn comment(&self, line: &[u8], index: usize, separated: bool) -> bool {
        if self.quote.is_some() {
            return false;
        }
        match line[index] {
            b';' => true,
            b'$' => self.braces == 0 && separated,
            b'/' => line.get(index + 1) == Some(&b'/'),
            _ => false,
        }
    }
}

const fn comment_line(line: &[u8]) -> bool {
    matches!(
        line.trim_ascii_start(),
        [] | [b'*' | b'#' | b'$' | b';', ..] | [b'/', b'/', ..]
    )
}

pub fn split_line_ending(line: &[u8]) -> (&[u8], &[u8]) {
    let body = line
        .strip_suffix(b"\n")
        .map_or(line, |s| s.strip_suffix(b"\r").unwrap_or(s));
    (body, &line[body.len()..])
}

/// Logical statement with one compact source mapping segment per physical line.
/// Synthetic continuation separators do not map to source bytes.
pub struct Statement {
    pub line: usize,
    pub start: usize,
    pub text: Vec<u8>,
    segments: Vec<(Range<usize>, usize)>,
    pub continuations: Vec<(usize, Range<usize>)>,
}

impl Statement {
    pub fn source_ranges(&self, span: Range<usize>) -> Vec<Range<usize>> {
        self.segments
            .iter()
            .filter_map(|(logical, source)| {
                let start = span.start.max(logical.start);
                let end = span.end.min(logical.end);
                (start < end)
                    .then(|| source + (start - logical.start)..source + (end - logical.start))
            })
            .collect()
    }
}

pub fn statements(input: &[u8]) -> Result<Vec<Statement>> {
    ensure_textual(input)?;
    let mut statements: Vec<Statement> = Vec::new();
    let mut offset = 0;
    let mut context = Context::default();
    for (index, physical) in input.split_inclusive(|b| *b == b'\n').enumerate() {
        let start = offset;
        offset += physical.len();
        let (body, _) = split_line_ending(physical);
        if comment_line(body) {
            continue;
        }
        let trimmed = body.trim_ascii_start();
        let indent = body.len() - trimmed.len();
        let continuation = trimmed.first() == Some(&b'+');
        let code_start = indent + usize::from(continuation);
        if !continuation {
            context = Context::default();
        }
        let mut end = body.len();
        for at in code_start..body.len() {
            if context.comment(
                body,
                at,
                at == code_start || matches!(body[at - 1], b' ' | b'\t' | b','),
            ) {
                end = at;
                break;
            }
            context.advance(body[at]);
        }
        let code = &body[code_start..end];
        let trimmed = code.trim_ascii();
        if trimmed.is_empty() {
            continue;
        }
        let begin = code_start + code.len() - code.trim_ascii_start().len();
        if !continuation {
            statements.push(Statement {
                line: index + 1,
                start,
                text: Vec::new(),
                segments: Vec::new(),
                continuations: Vec::new(),
            });
        }
        let Some(statement) = statements.last_mut() else {
            anyhow::bail!(
                "line {}: continuation without a preceding source line",
                index + 1
            );
        };
        if continuation {
            statement.text.push(b' ');
            statement
                .continuations
                .push((start + indent, start + code_start..start + end));
        }
        let logical_start = statement.text.len();
        statement.text.extend_from_slice(trimmed);
        statement
            .segments
            .push((logical_start..statement.text.len(), start + begin));
    }
    Ok(statements)
}

/// Distinguish UTF-8 code points from isolated legacy bytes, even in mixed
/// encoding input. Never rewrite continuation bytes of a different character.
fn utf8_width(bytes: &[u8]) -> usize {
    let width = match bytes[0] {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    if width > 1
        && bytes
            .get(..width)
            .is_some_and(|s| std::str::from_utf8(s).is_ok())
    {
        width
    } else {
        1
    }
}

fn trim_lexical_indent(mut bytes: &[u8]) -> &[u8] {
    loop {
        bytes = bytes.trim_ascii_start();
        if let Some(rest) = bytes
            .strip_prefix(b"\xc2\xa0")
            .or_else(|| bytes.strip_prefix(b"\xa0"))
        {
            bytes = rest;
        } else {
            return bytes;
        }
    }
}

pub fn normalize_nbsp(input: &[u8]) -> Option<Vec<u8>> {
    if !input.contains(&0xa0) {
        return None;
    }
    let mut out = Vec::with_capacity(input.len());
    let mut context = Context::default();
    for line in input.split_inclusive(|b| *b == b'\n') {
        let tail = trim_lexical_indent(line);
        let comment = comment_line(tail);
        let scan_end = if comment {
            line.len() - tail.len()
        } else {
            line.len()
        };
        if !comment && tail.first() != Some(&b'+') {
            context = Context::default();
        }
        let mut index = 0;
        while index < scan_end {
            let separated = index == 0
                || out
                    .last()
                    .is_some_and(|b| matches!(*b, b' ' | b'\t' | b','));
            if !comment && context.comment(line, index, separated) {
                break;
            }
            let width = utf8_width(&line[index..scan_end]);
            let bytes = &line[index..index + width];
            if (index < line.len() - tail.len() || context.quote.is_none())
                && matches!(bytes, b"\xa0" | b"\xc2\xa0")
            {
                out.push(b' ');
            } else {
                out.extend_from_slice(bytes);
            }
            if !comment {
                context.advance(line[index]);
            }
            index += width;
        }
        out.extend_from_slice(&line[index..]);
    }
    (out != input).then_some(out)
}

/// Shared quote-aware delimiter tracking for logical source fields.
#[derive(Default)]
pub struct Balanced {
    context: Context,
    stack: Vec<u8>,
}

impl Balanced {
    pub const fn top_level(&self) -> bool {
        self.context.quote.is_none() && self.stack.is_empty()
    }

    pub fn advance(&mut self, byte: u8) -> Result<()> {
        if self.context.quote.is_none() {
            match byte {
                b'(' => self.stack.push(b')'),
                b'{' => self.stack.push(b'}'),
                b'[' => self.stack.push(b']'),
                b')' | b'}' | b']' => ensure!(
                    self.stack.pop() == Some(byte),
                    "mismatched expression delimiters"
                ),
                _ => {}
            }
        }
        self.context.advance(byte);
        Ok(())
    }

    pub fn finish(&self) -> Result<()> {
        ensure!(self.top_level(), "unterminated expression or quote");
        Ok(())
    }
}

/// Top-level whitespace and assignment separators, with balanced expression
/// delimiters and quotes kept intact as a single token.
pub fn tokens(text: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut ranges = Vec::new();
    let mut start = None;
    let mut balanced = Balanced::default();
    for (i, &b) in text.iter().enumerate() {
        if balanced.top_level() && (b.is_ascii_whitespace() || b == b'=') {
            if let Some(begin) = start.take() {
                ranges.push(begin..i);
            }
            if b == b'=' {
                ranges.push(i..i + 1);
            }
            continue;
        }
        start.get_or_insert(i);
        balanced.advance(b)?;
    }
    balanced.finish()?;
    if let Some(begin) = start {
        ranges.push(begin..text.len());
    }
    Ok(ranges)
}

pub type Edit = (Range<usize>, Vec<u8>);

pub fn apply_edits(input: &[u8], mut edits: Vec<Edit>) -> Option<Vec<u8>> {
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    let mut out = Vec::with_capacity(input.len());
    let mut offset = 0;
    for (range, replacement) in edits {
        out.extend_from_slice(&input[offset..range.start]);
        out.extend_from_slice(&replacement);
        offset = range.end;
    }
    out.extend_from_slice(&input[offset..]);
    Some(out)
}

pub fn replace_span(
    statement: &Statement,
    span: Range<usize>,
    replacement: &[u8],
    edits: &mut Vec<Edit>,
) {
    for (index, range) in statement.source_ranges(span).into_iter().enumerate() {
        edits.push((
            range,
            if index == 0 {
                replacement.to_vec()
            } else {
                Vec::new()
            },
        ));
    }
}

/// Remove logical fields while retaining physical-line provenance. Entirely
/// consumed continuation lines become comments rather than dangling '+' cards.
pub fn remove_spans(
    statement: &Statement,
    input: &[u8],
    spans: &[Range<usize>],
    edits: &mut Vec<Edit>,
) {
    let replacements = spans
        .iter()
        .map(|span| {
            let start = statement.text[..span.start].trim_ascii_end().len();
            (start..span.end, Vec::new())
        })
        .collect::<Vec<_>>();
    replace_spans(statement, input, &replacements, edits);
}

/// Replace logical fields and comment wholly consumed continuations.
pub fn replace_spans(
    statement: &Statement,
    input: &[u8],
    replacements: &[Edit],
    edits: &mut Vec<Edit>,
) {
    let mut removed = Vec::new();
    for (span, replacement) in replacements {
        let ranges = statement.source_ranges(span.clone());
        for (index, range) in ranges.into_iter().enumerate() {
            if index == 0 && !replacement.is_empty() {
                edits.push((range, replacement.clone()));
            } else {
                removed.push(range.clone());
                edits.push((range, Vec::new()));
            }
        }
    }
    for (plus, code) in &statement.continuations {
        if input[code.clone()].iter().enumerate().all(|(i, byte)| {
            byte.is_ascii_whitespace()
                || removed
                    .iter()
                    .any(|range| range.contains(&(code.start + i)))
        }) {
            edits.push((*plus..*plus + 1, vec![b'*']));
        }
    }
}

const fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.') || !byte.is_ascii()
}

/// Locate all function-like identifiers outside quoted literal text.
pub fn function_calls(text: &[u8], start: usize) -> Vec<Range<usize>> {
    let mut context = Context::default();
    let mut matches = Vec::new();
    let mut cursor = start;
    while cursor < text.len() {
        let begin = cursor;
        if context.quote != Some(b'"')
            && identifier_byte(text[cursor])
            && (cursor == start || !identifier_byte(text[cursor - 1]))
        {
            while text.get(cursor).is_some_and(|byte| identifier_byte(*byte)) {
                context.advance(text[cursor]);
                cursor += 1;
            }
            if text[cursor..].trim_ascii_start().first() == Some(&b'(') {
                matches.push(begin..cursor);
            }
        } else {
            context.advance(text[cursor]);
            cursor += 1;
        }
    }
    matches
}

/// Locate function calls outside quoted literal text. Single-quoted arithmetic
/// expressions are handled by the caller only on expression-bearing cards.
pub fn function_names(text: &[u8], name: &[u8], start: usize) -> Vec<Range<usize>> {
    function_calls(text, start)
        .into_iter()
        .filter(|span| text[span.clone()].eq_ignore_ascii_case(name))
        .collect()
}
