//! PSpice passive model multipliers are dimensionless, not native model
//! default component values. A generated parameter in the declaration's
//! scope preserves the multiplier's binding across individual occurrences.

use super::{parameter, scope::Scopes, temperature::expression};
use crate::{
    fresh_identifier, lexical,
    model_card::{ModelCard, parse_spice_constant},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
};

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
}

#[derive(Default)]
pub(super) struct PassiveModels {
    models: BTreeMap<usize, PassiveModel>,
}

impl PassiveModels {
    pub(super) fn collect(scopes: &Scopes, identifiers: &mut BTreeSet<Vec<u8>>) -> Result<Self> {
        let mut result = Self::default();
        for (line, family) in scopes.model_declarations() {
            let Some(kind) = Kind::source(family) else {
                continue;
            };
            let card = scopes.model(line)?;
            for name in kind.nonlinear() {
                if let Some(value) = parameter(&card, name) {
                    ensure!(
                        matches!(parse_spice_constant(expression(&value)), Ok(0.0)),
                        "PSpice model {} uses nonlinear coefficient {name}; charge/flux lowering required",
                        card.name
                    );
                }
            }
            let multiplier = if let Some(value) = parameter(&card, kind.native()) {
                let mut base = String::from("__ps_passive_");
                for byte in card.name.bytes().map(|byte| byte.to_ascii_lowercase()) {
                    write!(base, "{byte:02x}")?;
                }
                let name = String::from_utf8(
                    fresh_identifier(identifiers, base.as_bytes())
                        .context("passive multiplier identifier overflow")?,
                )?;
                Some(Multiplier { name, value })
            } else {
                None
            };
            result
                .models
                .insert(line, PassiveModel { kind, multiplier });
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
    ) -> Result<String> {
        let Some(first) = text.as_bytes().first().map(u8::to_ascii_lowercase) else {
            return Ok(text.to_owned());
        };
        if !matches!(first, b'r' | b'c' | b'l') {
            return Ok(text.to_owned());
        }
        let spans = lexical::tokens(text.as_bytes())?;
        let Some(model_span) = spans.get(3) else {
            return Ok(text.to_owned());
        };
        let model_name = &text[model_span.clone()];
        let Some(model_line) = scopes.find_model(scope, &model_name.to_ascii_lowercase())? else {
            return Ok(text.to_owned());
        };
        let Some(model) = self.models.get(&model_line) else {
            return Ok(text.to_owned());
        };
        ensure!(
            model.kind.native().as_bytes()[0].to_ascii_lowercase() == first,
            "PSpice passive instance uses incompatible model {model_name}"
        );
        let value_span = spans
            .get(4)
            .context("PSpice modeled passive requires an instance value")?;
        let value = &text[value_span.clone()];
        ensure!(
            value != "=" && spans.get(5).is_none_or(|next| &text[next.clone()] != "="),
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
