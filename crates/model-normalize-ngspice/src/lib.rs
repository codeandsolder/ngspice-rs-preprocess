//! Deterministic compatibility normalization and validation for ngspice model includes.

pub mod linked_source;
pub mod model_card;
pub mod param_expression;
pub mod parameter_scope;
pub mod vdmos_card;
pub mod vdmos_ltspice;

use anyhow::{Context, Result, bail};
use regex::bytes::Regex;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::LazyLock,
    time::Duration,
};
use tempfile::TempDir;
use wait_timeout::ChildExt;

static LTSPICE_L_RSER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^([ \t]*)(l[^\s]+)\s+(\S+)\s+(\S+)\s+(\S+)\s+rser\s*=\s*(\S+)(.*)$")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static PSPICE_ARCTAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\barctan\s*\(")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static PSPICE_PARAMS_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*\+\s*PARAMS\s*:\s*$")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static PSPICE_POSITIONAL_PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([ \t]*\+[ \t]*)([A-Za-z_][A-Za-z0-9_]*)[ \t]+([^=\s].*)$")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
static END_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[ \t]*\.end[ \t]*(?:\r?\n|$)")
        .unwrap_or_else(|error| unreachable!("static regex must compile: {error}"))
});
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
    /// Restore the comment marker on wrapped prose in a leading comment block.
    HeaderCommentContinuation,
    /// Expand `LTspice`'s `L ... Rser=` extension to an ideal inductor plus resistor.
    LtspiceInductorRser,
    /// Map `PSpice` `arctan()` to ngspice `atan()`.
    PspiceArctan,
    /// Convert positional continued `PSpice` parameters to `name=value` form.
    PspicePositionalParams,
    /// Comment a standalone `.END` so the model remains safe to include.
    StripStandaloneEnd,
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
#[must_use]
pub fn looks_like_simplis(input: &[u8]) -> bool {
    SIMPLIS_MODEL_RE.is_match(input)
        || SIMPLIS_DEVICE_RE.is_match(input)
        || SIMPLIS_PREPROCESSOR_RE.is_match(input)
}

fn ensure_textual(input: &[u8]) -> Result<()> {
    let sample_len = input.len().min(131_072);
    if input[..sample_len].contains(&0) {
        bail!("ngspice normalization requires a textual model");
    }
    Ok(())
}

fn split_line_ending(line: &[u8]) -> (&[u8], &[u8]) {
    line.strip_suffix(b"\r\n").map_or_else(
        || {
            line.strip_suffix(b"\n")
                .map_or((line, b"".as_slice()), |body| (body, b"\n".as_slice()))
        },
        |body| (body, b"\r\n".as_slice()),
    )
}

fn lines_with_endings(input: &[u8]) -> impl Iterator<Item = &[u8]> {
    input.split_inclusive(|byte| *byte == b'\n')
}

fn normalize_nbsp(input: &[u8]) -> Option<Vec<u8>> {
    if !input.windows(2).any(|pair| pair == b"\xc2\xa0") && !input.contains(&0xa0) {
        return None;
    }
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input.get(index..index.saturating_add(2)) == Some(b"\xc2\xa0") {
            out.push(b' ');
            index += 2;
        } else if input[index] == 0xa0 {
            out.push(b' ');
            index += 1;
        } else {
            out.push(input[index]);
            index += 1;
        }
    }
    Some(out)
}

fn repair_header_comment_continuations(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut changed = false;
    let mut seen_spice = false;
    let mut prev_comment = false;
    for line in lines_with_endings(input) {
        let (body, ending) = split_line_ending(line);
        let stripped = body
            .iter()
            .position(|byte| !byte.is_ascii_whitespace())
            .map_or_else(|| &body[body.len()..], |index| &body[index..]);
        if stripped.is_empty() {
            out.extend_from_slice(line);
            continue;
        }
        if stripped.starts_with(b"*") {
            prev_comment = true;
            out.extend_from_slice(line);
            continue;
        }
        if stripped.starts_with(b".") || stripped.starts_with(b"+") {
            seen_spice = true;
            prev_comment = false;
            out.extend_from_slice(line);
            continue;
        }
        if !seen_spice
            && prev_comment
            && body
                .first()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            out.extend_from_slice(b"* ");
            out.extend_from_slice(stripped);
            out.extend_from_slice(ending);
            changed = true;
            prev_comment = true;
            continue;
        }
        prev_comment = false;
        out.extend_from_slice(line);
    }
    changed.then_some(out)
}

/// Reserve source identifiers before introducing synthetic elements or nets.
///
/// ngspice node and element names are case-insensitive. Tokenizing across
/// structural punctuation also reserves names inside parameter expressions,
/// rather than considering only device names at the start of a line.
fn source_identifiers(input: &[u8]) -> BTreeSet<Vec<u8>> {
    let mut used = BTreeSet::new();
    // Preserve the full spelling of SPICE identifiers with punctuation,
    // and separately reserve plain identifiers embedded in expressions.
    for token in input.split(|byte| {
        byte.is_ascii_whitespace()
            || matches!(
                *byte,
                b'[' | b']' | b'(' | b')' | b'{' | b'}' | b',' | b';' | b'=' | b'"' | b'\''
            )
    }) {
        if !token.is_empty() {
            used.insert(token.to_ascii_uppercase());
        }
    }
    for token in input.split(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')) {
        if !token.is_empty() {
            used.insert(token.to_ascii_uppercase());
        }
    }
    used
}

/// Allocate a source-unique element or net identifier, including against
/// identifiers generated earlier in the same normalization pass.
fn fresh_identifier(used: &mut BTreeSet<Vec<u8>>, base: &[u8]) -> Option<Vec<u8>> {
    let mut candidate = base.to_vec();
    let mut suffix = 0usize;
    loop {
        if used.insert(candidate.to_ascii_uppercase()) {
            return Some(candidate);
        }
        suffix = suffix.checked_add(1)?;
        candidate = base.to_vec();
        write!(&mut candidate, "_{suffix}").ok()?;
    }
}

fn rewrite_ltspice_inductor_rser(input: &[u8]) -> Option<Vec<u8>> {
    // Most sources have no Rser extension; avoid reserving every source
    // identifier or cloning the entire model in that common case.
    if !lines_with_endings(input).any(|line| LTSPICE_L_RSER_RE.is_match(split_line_ending(line).0))
    {
        return None;
    }

    let mut used = source_identifiers(input);
    let mut out = Vec::with_capacity(input.len());
    let mut serial = 0usize;
    let mut changed = false;
    for line in lines_with_endings(input) {
        let (body, ending) = split_line_ending(line);
        if let Some(captures) = LTSPICE_L_RSER_RE.captures(body) {
            let indent = captures
                .get(1)
                .map_or(b"".as_slice(), |value| value.as_bytes());
            let ind = captures.get(2)?.as_bytes();
            let node1 = captures.get(3)?.as_bytes();
            let node2 = captures.get(4)?.as_bytes();
            let value = captures.get(5)?.as_bytes();
            let rser = captures.get(6)?.as_bytes();
            let tail = captures
                .get(7)
                .map_or(b"".as_slice(), |value| value.as_bytes());

            let mut resistor_base = b"R__".to_vec();
            resistor_base.extend_from_slice(ind);
            resistor_base.extend_from_slice(b"_RSER");
            let resistor = fresh_identifier(&mut used, &resistor_base)?;

            let mut internal_base = b"__".to_vec();
            internal_base.extend_from_slice(ind);
            write!(&mut internal_base, "_rser_{serial}").ok()?;
            let internal = fresh_identifier(&mut used, &internal_base)?;

            out.extend_from_slice(indent);
            out.extend_from_slice(&resistor);
            out.push(b' ');
            out.extend_from_slice(node1);
            out.push(b' ');
            out.extend_from_slice(&internal);
            out.push(b' ');
            out.extend_from_slice(rser);
            // The original may end without LF; the synthesized resistor must
            // still be a separate complete SPICE statement.
            if ending.is_empty() {
                out.push(b'\n');
            } else {
                out.extend_from_slice(ending);
            }
            out.extend_from_slice(indent);
            out.extend_from_slice(ind);
            out.push(b' ');
            out.extend_from_slice(&internal);
            out.push(b' ');
            out.extend_from_slice(node2);
            out.push(b' ');
            out.extend_from_slice(value);
            out.extend_from_slice(tail);
            out.extend_from_slice(ending);
            serial += 1;
            changed = true;
        } else {
            out.extend_from_slice(line);
        }
    }
    changed.then_some(out)
}

fn rewrite_pspice_arctan(input: &[u8]) -> Option<Vec<u8>> {
    PSPICE_ARCTAN_RE.is_match(input).then(|| {
        PSPICE_ARCTAN_RE
            .replace_all(input, b"atan(".as_slice())
            .into_owned()
    })
}

fn rewrite_pspice_positional_params(input: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len());
    let mut in_params = false;
    let mut changed = false;
    for line in lines_with_endings(input) {
        let (body, ending) = split_line_ending(line);
        if PSPICE_PARAMS_HEADER_RE.is_match(body) {
            in_params = true;
            out.extend_from_slice(line);
            continue;
        }
        if in_params
            && body
                .iter()
                .position(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|index| body[index] == b'+')
        {
            if let Some(captures) = PSPICE_POSITIONAL_PARAM_RE.captures(body) {
                out.extend_from_slice(captures.get(1)?.as_bytes());
                out.extend_from_slice(captures.get(2)?.as_bytes());
                out.push(b'=');
                out.extend_from_slice(captures.get(3)?.as_bytes());
                out.extend_from_slice(ending);
                changed = true;
                continue;
            }
        } else if in_params {
            in_params = false;
        }
        out.extend_from_slice(line);
    }
    changed.then_some(out)
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
    ensure_textual(input)?;
    if looks_like_simplis(input) {
        bail!("SIMPLIS source requires translation, not ngspice normalization");
    }

    let mut bytes = input.to_vec();
    let mut passes = Vec::new();
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::NormalizeNbspWhitespace,
        normalize_nbsp,
    );
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::HeaderCommentContinuation,
        repair_header_comment_continuations,
    );
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::LtspiceInductorRser,
        rewrite_ltspice_inductor_rser,
    );
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::PspiceArctan,
        rewrite_pspice_arctan,
    );
    apply_optional(
        &mut bytes,
        &mut passes,
        NormalizationPass::PspicePositionalParams,
        rewrite_pspice_positional_params,
    );
    let stripped =
        END_LINE_RE.replace_all(&bytes, b"* .END stripped for ngspice include\n".as_slice());
    if stripped.as_ref() != bytes.as_slice() {
        bytes = stripped.into_owned();
        passes.push(NormalizationPass::StripStandaloneEnd);
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
    let mut child = Command::new("ngspice")
        .args(["-b", "deck.cir"])
        .current_dir(temp.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting ngspice")?;
    if child.wait_timeout(timeout)?.is_none() {
        child.kill().context("killing timed-out ngspice")?;
        let _ = child.wait();
        return Ok(ValidationResult {
            ok: false,
            mode: None,
            detail: "ngspice parse timeout".to_owned(),
        });
    }
    let output = child.wait_with_output()?;
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

    #[test]
    fn rewrites_only_targeted_syntax() -> Result<()> {
        let input = b"* hdr\n  wrapped prose\n.subckt x a b\nL1 a b 2u Rser=30m\n+ PARAMS:\n+ gain 2\nB1 x 0 v=arctan(v(a))\n.ends x\n.END\n";
        let normalized = normalize_ngspice(input)?;
        assert!(
            normalized
                .bytes
                .windows(b"* wrapped prose".len())
                .any(|w| w == b"* wrapped prose")
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
        assert_eq!(normalized.passes.len(), 5);
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
    fn rejects_simplis_as_translation_problem() {
        let input = b".MODEL SW VCSW RON=1m ROFF=1MEG TH=2 HYSTWD=1 LOGIC=POS\n";
        assert!(looks_like_simplis(input));
        assert!(normalize_ngspice(input).is_err());
    }
}
