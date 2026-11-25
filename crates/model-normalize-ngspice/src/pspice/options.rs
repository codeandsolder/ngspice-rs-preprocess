//! Circuit-wide option bindings needed by source expressions and switch
//! control-terminal conductance.

use crate::{lexical, model_card::parse_spice_constant};
use anyhow::{Context, Result, ensure};

pub(super) struct GlobalOptions {
    pub gmin: f64,
}

impl GlobalOptions {
    pub(super) fn collect(statements: &[lexical::Statement]) -> Result<Self> {
        let mut options = Self { gmin: 1e-12 };
        let mut control = false;
        let mut conditional_depth = 0usize;
        for statement in statements {
            let text = std::str::from_utf8(&statement.text)?;
            let first = text
                .split_ascii_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            match first.as_str() {
                ".control" => control = true,
                ".endc" => control = false,
                _ if control => {}
                ".if" => conditional_depth += 1,
                ".endif" => {
                    conditional_depth = conditional_depth
                        .checked_sub(1)
                        .context("unmatched PSpice .ENDIF")?;
                }
                ".options" | ".option" => {
                    let spans = lexical::tokens(text.as_bytes())?;
                    for (index, span) in spans.iter().enumerate().skip(1) {
                        if !text[span.clone()].eq_ignore_ascii_case("gmin")
                            || spans
                                .get(index.wrapping_sub(1))
                                .is_some_and(|previous| &text[previous.clone()] == "=")
                        {
                            continue;
                        }
                        ensure!(
                            conditional_depth == 0,
                            "conditional PSpice GMIN requires analysis binding"
                        );
                        ensure!(
                            spans
                                .get(index + 1)
                                .is_some_and(|equals| &text[equals.clone()] == "="),
                            "PSpice GMIN requires a value"
                        );
                        let value = spans.get(index + 2).context("missing PSpice GMIN value")?;
                        let conductance = parse_spice_constant(&text[value.clone()])
                            .context("PSpice GMIN must be a circuit-wide scalar")?;
                        ensure!(
                            conductance.is_finite()
                                && conductance > 0.
                                && conductance.recip().is_finite(),
                            "invalid PSpice GMIN"
                        );
                        options.gmin = conductance;
                    }
                }
                _ => {}
            }
        }
        ensure!(conditional_depth == 0, "unterminated PSpice conditional");
        Ok(options)
    }
}
