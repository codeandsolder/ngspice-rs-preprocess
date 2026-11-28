//! Project hierarchical branch references to native relative descendant paths.
//! The numerical solver resolves those paths against actual occurrences.

use crate::lexical;
use anyhow::{Context, Result, ensure};

fn descendant_reference(raw: &[u8]) -> Result<Option<Vec<u8>>> {
    if !raw.contains(&b':') {
        return Ok(None);
    }
    let text = std::str::from_utf8(raw)?;
    let mut parts = text.split(':');
    let family = parts.next().unwrap_or_default();
    if family.len() != 1 || !family.as_bytes()[0].is_ascii_alphabetic() {
        return Ok(None);
    }
    let descendants = parts.collect::<Vec<_>>();
    ensure!(
        descendants.len() >= 2
            && descendants.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'$' | b'.')
                            || !byte.is_ascii()
                    })
            }),
        "invalid hierarchical current reference {text}"
    );
    let (leaf, ancestors) = descendants
        .split_last()
        .context("missing hierarchical current target")?;
    let mut native = String::new();
    for ancestor in ancestors {
        native.push('x');
        native.push_str(ancestor);
        native.push_str("::");
    }
    native.push_str(family);
    native.push_str(leaf);
    Ok(Some(native.into_bytes()))
}

pub(in crate::series) fn rewrite_statement(
    statement: &lexical::Statement,
    edits: &mut Vec<lexical::Edit>,
) -> Result<()> {
    let text = &statement.text;
    if !text.contains(&b':') {
        return Ok(());
    }
    let Some(family) = text.first().map(u8::to_ascii_lowercase) else {
        return Ok(());
    };
    // Linear current controls and coupling cards name an instance in
    // their positional fields (including multi-control POLY forms).
    if matches!(family, b'k' | b'f' | b'h' | b'w') {
        let start = if family == b'k' { 1 } else { 3 };
        for span in lexical::waveform_tokens(text)?.into_iter().skip(start) {
            if let Some(native) = descendant_reference(&text[span.clone()])? {
                lexical::replace_span(statement, span, &native, edits);
            }
        }
    }
    // A current call may live inside an unexpanded function definition
    // or a display directive; preserve its role rather than select a
    // network topology here.
    for function in lexical::function_names(text, b"i", 0) {
        let tail = &text[function.end..];
        let after_space = tail.trim_ascii_start();
        let Some(argument) = after_space.strip_prefix(b"(") else {
            continue;
        };
        let Some(close) = argument.iter().position(|byte| *byte == b')') else {
            continue;
        };
        let raw = &argument[..close];
        let trimmed = raw.trim_ascii();
        if trimmed
            .iter()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'(' | b','))
        {
            continue;
        }
        if let Some(native) = descendant_reference(trimmed)? {
            let start = text.len() - argument.len() + raw.len() - raw.trim_ascii_start().len();
            lexical::replace_span(statement, start..start + trimmed.len(), &native, edits);
        }
    }
    Ok(())
}
