//! `PSpice` passive model multipliers are dimensionless, not native model
//! default component values. A generated parameter in the declaration's
//! scope preserves the multiplier's binding across individual occurrences.

use super::{parameter, scope::Scopes, temperature::expression};
use crate::{
    IdentifierAllocator, lexical,
    linked_source::SourceDialect,
    model_card::{ModelCard, parse_spice_constant},
    passive_storage::{PSPICE_CAPACITOR, PSPICE_INDUCTOR},
};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeMap, fmt::Write as _};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Resistor,
    Capacitor,
    Inductor,
}

impl Kind {
    fn source(family: &str) -> Option<Self> {
        match family.to_ascii_lowercase().as_str() {
            "res" => Some(Self::Resistor),
            "cap" => Some(Self::Capacitor),
            "ind" => Some(Self::Inductor),
            _ => None,
        }
    }

    const fn native(self) -> &'static str {
        match self {
            Self::Resistor => "R",
            Self::Capacitor => "C",
            Self::Inductor => "L",
        }
    }

    const fn prepared(self) -> &'static str {
        match self {
            Self::Capacitor => PSPICE_CAPACITOR,
            Self::Inductor => PSPICE_INDUCTOR,
            Self::Resistor => "R",
        }
    }

    const fn nonlinear(self) -> &'static [&'static str] {
        match self {
            Self::Resistor => &[],
            Self::Capacitor => &["vc1", "vc2"],
            Self::Inductor => &["il1", "il2"],
        }
    }
}

struct Multiplier {
    name: String,
    value: String,
}

struct PassiveModel {
    kind: Kind,
    multiplier: Option<Multiplier>,
    polynomial: bool,
}

#[derive(Default)]
pub(super) struct PassiveModels {
    models: BTreeMap<usize, PassiveModel>,
}

impl PassiveModels {
    pub(super) fn collect(scopes: &Scopes, identifiers: &mut IdentifierAllocator) -> Result<Self> {
        let mut result = Self::default();
        for (line, family) in scopes.model_declarations() {
            let Some(kind) = Kind::source(family) else {
                continue;
            };
            let card = scopes.model(line)?;
            for field in &card.parameters {
                if field.name.eq_ignore_ascii_case(kind.native())
                    || kind
                        .nonlinear()
                        .iter()
                        .any(|name| field.name.eq_ignore_ascii_case(name))
                {
                    ensure!(
                        field.expression.is_some(),
                        "PSpice model {} requires a value for {}",
                        card.name,
                        field.name
                    );
                }
            }
            // A symbolic coefficient may be zero in one occurrence and
            // nonzero in another; retain the typed polynomial in both.
            let polynomial = kind.nonlinear().iter().any(|name| {
                parameter(&card, name).is_some_and(|value| {
                    !matches!(parse_spice_constant(expression(&value)), Ok(0.0))
                })
            });
            let multiplier = if !polynomial && let Some(value) = parameter(&card, kind.native()) {
                let mut base = String::from("__ps_passive_");
                for byte in card.name.bytes().map(|byte| byte.to_ascii_lowercase()) {
                    write!(base, "{byte:02x}")?;
                }
                let name = String::from_utf8(identifiers.fresh(base.as_bytes())?)?;
                Some(Multiplier { name, value })
            } else {
                None
            };
            result.models.insert(
                line,
                PassiveModel {
                    kind,
                    multiplier,
                    polynomial,
                },
            );
        }
        Ok(result)
    }

    // Return the additional parameter declaration separately so the caller
    // rewrites its expression with the same scope and builtin semantics.
    pub(super) fn lower_model(
        &self,
        line: usize,
        card: &mut ModelCard,
    ) -> Option<(String, String)> {
        let model = self.models.get(&line)?;
        if model.polynomial {
            // Keep source identity and declaration-scoped parameters.
            // The engine binds the integrated law at candidate preparation.
            model.kind.prepared().clone_into(&mut card.family);
            return None;
        }
        model.kind.native().clone_into(&mut card.family);
        card.parameters.retain(|field| {
            !field.name.eq_ignore_ascii_case(model.kind.native())
                && !model
                    .kind
                    .nonlinear()
                    .iter()
                    .any(|name| field.name.eq_ignore_ascii_case(name))
        });
        model
            .multiplier
            .as_ref()
            .map(|multiplier| (multiplier.name.clone(), multiplier.value.clone()))
    }

    pub(super) fn rewrite_instance(
        &self,
        text: &str,
        scope: usize,
        scopes: &Scopes,
        source: SourceDialect,
    ) -> Result<String> {
        let Some(first) = text.as_bytes().first().map(u8::to_ascii_lowercase) else {
            return Ok(text.to_owned());
        };
        if !matches!(first, b'r' | b'c' | b'l') {
            return Ok(text.to_owned());
        }
        let spans = lexical::tokens(text.as_bytes())?;
        // A PSpice model owns its multiplier even when a native card
        // references it. Native positional syntax accepts value/model in
        // either order; PSpice requires the model before its value.
        let mut candidate = None;
        for (model_index, value_index) in [(3, 4), (4, 3)] {
            if source.is_pspice() && model_index == 4 {
                break;
            }
            let Some(model_span) = spans.get(model_index) else {
                continue;
            };
            let model_name = &text[model_span.clone()];
            let Some(model_line) = scopes.find_model(scope, &model_name.to_ascii_lowercase())?
            else {
                continue;
            };
            let Some(model) = self.models.get(&model_line) else {
                continue;
            };
            ensure!(candidate.is_none(), "ambiguous modeled passive {text}");
            candidate = Some((model, model_name, value_index));
        }
        let Some((model, model_name, value_index)) = candidate else {
            return Ok(text.to_owned());
        };
        ensure!(
            model.kind.native().as_bytes()[0].to_ascii_lowercase() == first,
            "PSpice passive instance uses incompatible model {model_name}"
        );
        let value_span = spans
            .get(value_index)
            .context("PSpice modeled passive requires an instance value")?;
        let value = &text[value_span.clone()];
        ensure!(
            value != "="
                && spans
                    .get(value_index + 1)
                    .is_none_or(|next| &text[next.clone()] != "="),
            "PSpice modeled passive requires a positional instance value"
        );
        let Some(multiplier) = &model.multiplier else {
            return Ok(text.to_owned());
        };
        let scaled = format!("{{({})*{}}}", expression(value), multiplier.name);
        let mut output = text.to_owned();
        output.replace_range(value_span.clone(), &scaled);
        Ok(output)
    }
}
