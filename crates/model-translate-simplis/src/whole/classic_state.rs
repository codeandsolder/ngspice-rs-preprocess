//! Classic SR and clocked state-element grammar.

use super::{StateElementSpec, emit_state_element, escaped_device_stem};
use crate::{
    LogicIo, LogicOperation, LogicPolarity, LogicReceiver, LogicTiming, OutputPins, Translation,
    TranslationKind, integer_parameter, normalized_statement, numeric_parameter, statement_tokens,
};
use anyhow::{Context, Result, bail};
use model_normalize_ngspice::model_card::{ModelCard, ModelParameter, parse_parameter_fields};

fn state_assignments(
    fields: &[ModelParameter],
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut result = std::collections::BTreeMap::new();
    for field in fields {
        let value = field
            .expression
            .as_ref()
            .with_context(|| format!("state parameter {} requires a value", field.name))?;
        let name = field.name.to_ascii_uppercase();
        anyhow::ensure!(
            result.insert(name.clone(), value.clone()).is_none(),
            "duplicate state parameter {name}"
        );
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug)]
enum ClassicStateKind {
    Sr,
    ClockedSr,
    ClockedJk,
    ClockedD,
    ClockedT,
    Latch,
}

impl ClassicStateKind {
    fn parse(kind: &str) -> Option<Self> {
        match kind {
            "SRFF" => Some(Self::Sr),
            "CLK_SRFF" => Some(Self::ClockedSr),
            "CLK_JKFF" => Some(Self::ClockedJk),
            "CLK_DFF" => Some(Self::ClockedD),
            "CLK_TFF" => Some(Self::ClockedT),
            "LATCH" => Some(Self::Latch),
            _ => None,
        }
    }

    const fn inputs(self) -> usize {
        match self {
            Self::ClockedSr | Self::ClockedJk => 3,
            _ => 2,
        }
    }

    const fn outputs(self) -> usize {
        match self {
            Self::Latch => 1,
            _ => 2,
        }
    }

    const fn operation(self, rising: bool) -> LogicOperation {
        match (self, rising) {
            (Self::Sr, _) => LogicOperation::HoldWhenEqual,
            (Self::ClockedSr, true) => LogicOperation::ClockedSrRising,
            (Self::ClockedSr, false) => LogicOperation::ClockedSrFalling,
            (Self::ClockedJk, true) => LogicOperation::ClockedJkRising,
            (Self::ClockedJk, false) => LogicOperation::ClockedJkFalling,
            (Self::ClockedD, true) => LogicOperation::ClockedDRising,
            (Self::ClockedD, false) => LogicOperation::ClockedDFalling,
            (Self::ClockedT, true) => LogicOperation::ClockedTRising,
            (Self::ClockedT, false) => LogicOperation::ClockedTFalling,
            (Self::Latch, true) => LogicOperation::LatchHigh,
            (Self::Latch, false) => LogicOperation::LatchLow,
        }
    }

    const fn translation(self) -> TranslationKind {
        match self {
            Self::Sr => TranslationKind::SrFlipFlop,
            Self::Latch => TranslationKind::DataLatch,
            _ => TranslationKind::ClockedFlipFlop,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ClassicStateModel {
    kind: ClassicStateKind,
    operation: LogicOperation,
    rin: f64,
    rout: f64,
    threshold: f64,
    hystwd: f64,
    vol: f64,
    voh: f64,
    polarity: LogicPolarity,
    delay: f64,
}

pub(super) fn classic_state_model(card: &ModelCard) -> Result<Option<(String, ClassicStateModel)>> {
    let Some(kind) = ClassicStateKind::parse(&card.family.to_ascii_uppercase()) else {
        return Ok(None);
    };
    let parameters = state_assignments(&card.parameters)?;
    let rising = match kind {
        ClassicStateKind::Sr => true,
        ClassicStateKind::Latch => match integer_parameter(&parameters, "ENABLE_LEVEL")? {
            1 => true,
            0 => false,
            _ => bail!("LATCH requires ENABLE_LEVEL=0 or 1"),
        },
        _ => match parameters
            .get("TRIG_COND")
            .map(|value| value.trim_matches('\'').to_ascii_uppercase())
            .as_deref()
        {
            Some("0_TO_1") => true,
            Some("1_TO_0") => false,
            _ => bail!("clocked logic requires TRIG_COND=0_TO_1 or 1_TO_0"),
        },
    };
    if !matches!(kind, ClassicStateKind::Sr) {
        // Classic clocked primitives have no specified propagation delay or
        // setup/hold timing parameters. Do not silently invent them.
        let selector = if matches!(kind, ClassicStateKind::Latch) {
            "ENABLE_LEVEL"
        } else {
            "TRIG_COND"
        };
        anyhow::ensure!(
            parameters.len() == 8
                && parameters.keys().all(|key| matches!(
                    key.as_str(),
                    "RIN" | "ROUT" | "TH" | "HYSTWD" | "VOL" | "VOH" | "LOGIC"
                ) || key == selector),
            "unexpected or missing classic state model parameter"
        );
    }
    let polarity = LogicPolarity::parse(&parameters.get("LOGIC").map_or_else(
        || "POS".to_owned(),
        |value| value.trim_matches('\'').to_ascii_uppercase(),
    ))?;
    let model = ClassicStateModel {
        kind,
        operation: kind.operation(rising),
        rin: numeric_parameter(&parameters, "RIN")?,
        rout: numeric_parameter(&parameters, "ROUT")?,
        threshold: numeric_parameter(&parameters, "TH")?,
        hystwd: numeric_parameter(&parameters, "HYSTWD")?,
        vol: numeric_parameter(&parameters, "VOL")?,
        voh: numeric_parameter(&parameters, "VOH")?,
        delay: if parameters.contains_key("DELAY") {
            numeric_parameter(&parameters, "DELAY")?
        } else {
            0.0
        },
        polarity,
    };
    let finite = [
        model.rin,
        model.rout,
        model.threshold,
        model.hystwd,
        model.vol,
        model.voh,
        model.delay,
    ]
    .into_iter()
    .all(f64::is_finite);
    if !(finite
        && model.rin > 0.0
        && model.rout > 0.0
        && (model.hystwd > 0.0 || (matches!(kind, ClassicStateKind::Sr) && model.hystwd >= 0.0))
        && model.delay >= 0.0
        && model.voh > model.vol)
    {
        bail!("invalid SIMPLIS state model {}", card.name);
    }
    Ok(Some((card.name.to_ascii_uppercase(), model)))
}

pub(super) fn translate_classic_state_device(
    statement: &str,
    models: &std::collections::BTreeMap<String, ClassicStateModel>,
) -> Result<Option<Translation>> {
    if !statement
        .trim_start()
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("!D"))
    {
        return Ok(None);
    }
    let tokens = statement_tokens(&normalized_statement(statement))?;
    // The model is the final positional field, immediately before
    // the first instance assignment. A node may have the same name
    // as another model, so do not discover it by searching node tokens.
    let assignment = tokens
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, token)| {
            if token.contains('=') {
                Some(if token.starts_with('=') {
                    index - 1
                } else {
                    index
                })
            } else {
                None
            }
        })
        .unwrap_or(tokens.len());
    let Some(index) = assignment.checked_sub(1) else {
        return Ok(None);
    };
    let Some(model) = tokens
        .get(index)
        .and_then(|name| models.get(&name.to_ascii_uppercase()))
    else {
        return Ok(None);
    };
    anyhow::ensure!(
        index == 2 + model.kind.outputs() + model.kind.inputs(),
        "wrong classic state device pin count for {}",
        tokens[0]
    );
    let polarity = model.polarity;
    let parameters = state_assignments(&parse_parameter_fields(
        &tokens[assignment..].join(" "),
        &[],
    )?)?;
    anyhow::ensure!(
        parameters.keys().all(|key| key == "IC"),
        "unsupported classic state instance parameter"
    );
    let initial = if parameters.contains_key("IC") {
        integer_parameter(&parameters, "IC")?
    } else {
        anyhow::ensure!(
            matches!(model.kind, ClassicStateKind::Sr),
            "classic state instance requires IC"
        );
        0
    };
    anyhow::ensure!(initial <= 1, "invalid classic state IC");
    let rtn_index = 1 + model.kind.outputs();
    let rtn = &tokens[rtn_index];
    let inputs = &tokens[rtn_index + 1..index];
    let outputs = match &tokens[1..rtn_index] {
        [primary] => OutputPins {
            primary,
            complement: None,
        },
        [primary, complement] => OutputPins {
            primary,
            complement: Some(complement),
        },
        _ => bail!("invalid classic state output count"),
    };
    let stem = escaped_device_stem(tokens[0].trim_start_matches('!'));
    let io = LogicIo {
        rin: model.rin,
        rout: model.rout,
        hystwd: model.hystwd,
        vol: model.vol,
        voh: model.voh,
        delay: model.delay,
        threshold: model.threshold,
    };
    emit_state_element(
        io,
        &StateElementSpec {
            stem: &stem,
            outputs,
            rtn,
            inputs,
            receiver: LogicReceiver {
                polarity,
                initial: false,
            },
            output_polarity: polarity,
            timing: LogicTiming {
                operation: model.operation,
                delay: io.delay,
                inertial: false,
                initial: initial == 1,
            },
            kind: model.kind.translation(),
        },
    )
    .map(Some)
}
