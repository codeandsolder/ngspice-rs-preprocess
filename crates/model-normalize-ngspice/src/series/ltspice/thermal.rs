//! Occurrence-bound thermal and noise setters. The native network owns
//! contextual evaluation, not this source-card projection.

use super::{Card, expression};
use crate::{lexical, model_card::parse_spice_constant};
use anyhow::{Context, Result, ensure};
use model_preprocess_command::{Resource, WorkBudget};

const fn is_keyword(text: &[u8], key: &[u8]) -> bool {
    text.eq_ignore_ascii_case(key)
}

/// Collect the complete capacitor TC list, not just its first scalar.
/// Repeated setters replace the entire list, including an empty reset.
fn capacitor_coefficients(card: &mut Card<'_>, budget: &WorkBudget) -> Result<Vec<String>> {
    let text = &card.statement.text;
    let fields = &card.fields;
    let mut result = Vec::new();
    for index in 4..fields.len() {
        let span = &fields[index];
        if !is_keyword(&text[span.clone()], b"tc") || text[fields[index - 1].clone()] == *b"=" {
            continue;
        }
        let equals = fields
            .get(index + 1)
            .filter(|field| text[(*field).clone()] == *b"=")
            .with_context(|| format!("line {}: TC requires '='", card.statement.line))?;
        let end_index = (index + 2..fields.len())
            .find(|&at| {
                let field = &text[fields[at].clone()];
                is_keyword(field, b"noiseless")
                    || is_keyword(field, b"noisy")
                    || fields
                        .get(at + 1)
                        .is_some_and(|next| text[next.clone()] == *b"=")
            })
            .unwrap_or(fields.len());
        let end = fields[end_index - 1].end;
        let raw = text[equals.end..end].trim_ascii();
        let raw = raw
            .strip_prefix(b"(")
            .and_then(|inner| inner.strip_suffix(b")"))
            .unwrap_or(raw);
        let entries = lexical::waveform_tokens(raw)?;
        budget.charge(Resource::GeneratedNodes, entries.len())?;
        result.clear();
        for entry in entries {
            let coefficient = std::str::from_utf8(&raw[entry])?;
            let parameter = expression(coefficient);
            ensure!(
                parameter != coefficient || parse_spice_constant(coefficient).is_ok(),
                "line {}: invalid TC coefficient {coefficient:?}",
                card.statement.line
            );
            // Braced entries keep symbolic operators and table helpers in
            // the static phase of the shared expression normalization pass.
            result.push(format!("{{{parameter}}}"));
        }
        card.replacements.push((span.start..end, Vec::new()));
    }
    Ok(result)
}

fn inductor_coefficients(card: &mut Card<'_>) -> Result<Vec<String>> {
    let linear = card.take(b"tc1")?;
    let quadratic = card.take(b"tc2")?;
    if linear.is_none() && quadratic.is_none() {
        return Ok(Vec::new());
    }
    Ok([linear.unwrap_or("0"), quadratic.unwrap_or("0")]
        .map(|raw| format!("{{{}}}", expression(raw)))
        .into())
}

/// Main storage uses the thermal polynomial, while loss and auxiliary
/// storage retain their written values. Returns whether the capacitor
/// requires a contextual positive-storage leakage coefficient.
pub(super) fn storage(card: &mut Card<'_>, inductor: bool, budget: &WorkBudget) -> Result<bool> {
    card.reject(b"dtemp")?;
    let coefficients = if inductor {
        for key in [b"lser".as_slice(), b"rlshunt", b"tc", b"tc3"] {
            card.reject(key)?;
        }
        inductor_coefficients(card)?
    } else {
        for key in [b"tc1".as_slice(), b"tc2", b"tnom"] {
            card.reject(key)?;
        }
        capacitor_coefficients(card, budget)?
    };
    if !coefficients.is_empty() {
        card.push("pn_storage_tc", &coefficients.join(" "));
    }
    if inductor {
        let nominal = card.take(b"tnom")?;
        if nominal.is_some() || !coefficients.is_empty() {
            card.push("pn_storage_tnom", nominal.unwrap_or("27"));
            card.push("pn_coupling_nominal", "1");
        }
    }
    Ok(!inductor && !coefficients.is_empty())
}

pub(super) fn noise(card: &mut Card<'_>, supported: bool) -> Result<()> {
    let text = &card.statement.text;
    let start = if supported { 4 } else { 3 };
    let mut noiseless = false;
    for (index, span) in card.fields.iter().enumerate().skip(start) {
        let field = &text[span.clone()];
        if !is_keyword(field, b"noiseless") && !is_keyword(field, b"noisy") {
            continue;
        }
        // A flag immediately following an empty TC list is a separate
        // source token; other scalar assignment values remain untouched.
        if text[card.fields[index - 1].clone()] == *b"="
            && (index < 2 || !is_keyword(&text[card.fields[index - 2].clone()], b"tc"))
        {
            continue;
        }
        ensure!(
            supported
                && is_keyword(field, b"noiseless")
                && card
                    .fields
                    .get(index + 1)
                    .is_none_or(|next| text[next.clone()] != *b"="),
            "line {}: unsupported LTspice physical-network noise setter {}",
            card.statement.line,
            String::from_utf8_lossy(field)
        );
        noiseless = true;
        card.replacements.push((span.clone(), Vec::new()));
    }
    if noiseless {
        card.push("pn_noiseless", "1");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{
        lexical, linked_source::RootKind, linked_source::SourceDialect, normalize_mapped,
        parameter_scope::ModelLibrary,
    };
    use anyhow::{Context, Result, ensure};
    use model_preprocess_command::WorkBudget;

    fn vector(text: &[u8]) -> Result<Vec<&str>> {
        let fields = lexical::tokens(text)?;
        let Some(index) = fields
            .iter()
            .position(|span| text[span.clone()] == *b"pn_storage_tc")
        else {
            return Ok(Vec::new());
        };
        let end = (index + 2..fields.len())
            .find(|&at| {
                fields
                    .get(at + 1)
                    .is_some_and(|next| text[next.clone()] == *b"=")
            })
            .unwrap_or(fields.len());
        fields[index + 2..end]
            .iter()
            .map(|span| Ok(std::str::from_utf8(&text[span.clone()])?))
            .collect()
    }

    #[test]
    fn thermal_vectors_keep_occurrence_binding_and_independent_loss() -> Result<()> {
        let input = b"title\n.subckt cell p n params: k=.02 q=.0001 ref=50\nC1 p n 2u tc=(.01,.0001,.000001) tc={k},,{q}, Rser=5\n+ Lser=3m noiseless\nC2 p n 2u tc={k} tc= Rpar=0\nC3 p n 2u tc={k**2} {q} Rpar=20\nL1 p n 3m tc1={k} tc2={q} tnom={ref} temp=127 Rser=5 noiseless\nL2 p n 3m tc2={q}\nC4 p n 2u tc=0,0,0,0,0,0,0,1e-16\n.ends\n.end\n";
        let normalized = normalize_mapped(
            input,
            RootKind::Deck,
            SourceDialect::Ltspice,
            &WorkBudget::default(),
        )?;
        let library = ModelLibrary::parse_source(&normalized.model.bytes, RootKind::Deck)?;
        let cards = lexical::statements(&normalized.model.bytes)?;
        for k in [0.02_f64, -0.02, 0.] {
            let parameters = [(1, "k", k), (1, "q", 0.0001)];
            for (name, expected) in [
                ("C1", vec![k, 0.0001]),
                ("C2", vec![]),
                ("C3", vec![k * k, 0.0001]),
                ("L1", vec![k, 0.0001]),
                ("L2", vec![0., 0.0001]),
                ("C4", vec![0., 0., 0., 0., 0., 0., 0., 1e-16]),
            ] {
                let card = cards
                    .iter()
                    .find(|card| card.text.starts_with(format!("{name} ").as_bytes()))
                    .with_context(|| format!("missing thermal card {name}"))?;
                let entries = vector(&card.text)?;
                ensure!(entries.len() == expected.len(), "{name}: {entries:?}");
                for (entry, expected) in entries.into_iter().zip(expected) {
                    let actual = library.evaluate_scalar(1, entry, &parameters)?;
                    ensure!(
                        (actual - expected).abs() <= 1e-13 * expected.abs().max(1e-16),
                        "{name}: {actual} != {expected}"
                    );
                }
                let text = std::str::from_utf8(&card.text)?;
                assert_eq!(
                    text.contains("pn_gterminal_positive_storage_dc=1e-12"),
                    matches!(name, "C1" | "C4"),
                    "{text}"
                );
                assert_eq!(
                    text.contains("pn_noiseless=1"),
                    matches!(name, "C1" | "L1"),
                    "{text}"
                );
                if name == "L1" {
                    assert!(text.contains("pn_storage_tnom={ref}"), "{text}");
                    assert!(text.contains("pn_coupling_nominal=1"), "{text}");
                    assert!(text.contains("pn_gterminal_dc="), "{text}");
                } else if name == "L2" {
                    assert!(text.contains("pn_storage_tnom=27"), "{text}");
                }
            }
        }
        ensure!(
            normalized.input_lines.len()
                == normalized
                    .model
                    .bytes
                    .split_inclusive(|byte| *byte == b'\n')
                    .count()
        );
        Ok(())
    }

    #[test]
    fn malformed_temperature_and_noise_fields_are_diagnostic() -> Result<()> {
        for card in [
            "C1 p 0 2u tc=.01,not_a_scalar",
            "C1 p 0 2u noiseless=0",
            "L1 p 0 3m noiseless=1",
            "C1 p 0 2u noisy",
            "V1 p 0 AC 1 Rser=5 noiseless",
        ] {
            let deck = format!("title\n{card}\n.end\n");
            let result = normalize_mapped(
                deck.as_bytes(),
                RootKind::Deck,
                SourceDialect::Ltspice,
                &WorkBudget::default(),
            );
            ensure!(result.is_err(), "accepted invalid source card {card}");
        }
        Ok(())
    }
}
