//! Source-specific passive initialization policy.
//!
//! The numerical solver must apply these constraints only during the
//! enclosing analysis's bias phase, then initialize ordinary storage from
//! that equilibrium. No artificial transient branches are prescribed here.

use crate::param_expression::SpiceExpression;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// Enclosing analysis, independent of the instantaneous Newton load mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Analysis {
    OperatingPoint,
    Transient { uic: bool },
    Ac,
    DcSweep,
    Noise,
    TransferFunction,
    Sensitivity,
}

impl Analysis {
    /// Source language's predefined ANALYSIS value.
    #[must_use = "the code binds the source analysis expression"]
    pub const fn source_code(self) -> f64 {
        match self {
            Self::OperatingPoint => 1.,
            Self::Transient { .. } => 2.,
            Self::Ac => 3.,
            Self::DcSweep => 5.,
            Self::Noise => 6.,
            Self::TransferFunction => 7.,
            Self::Sensitivity => 8.,
        }
    }

    const fn uic(self) -> bool {
        matches!(self, Self::Transient { uic: true })
    }
}

/// Initial-condition mode, distinct from the ordinary ngspice default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    Ignore,
    Hard,
    #[default]
    Soft,
}

impl Mode {
    fn from_value(value: f64) -> Result<Self> {
        match value {
            0. => Ok(Self::Ignore),
            1. => Ok(Self::Hard),
            2. => Ok(Self::Soft),
            _ => bail!("invalid passive initial-condition mode {value}; expected 0, 1 or 2"),
        }
    }
}

/// Voltage across a capacitor or signed terminal current of an inductor.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PassiveKind {
    Capacitor,
    Inductor,
}

/// Typed declaration in the final emitted lexical scope. Expression strings
/// use normalized scalar grammar and must be evaluated in the elaborated
/// occurrence, not globally or while stamping a device.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PassiveInitialCondition {
    /// Emitted artifact filename.
    pub artifact: String,
    /// Emitted definition names (empty for root).
    pub scope: Vec<String>,
    /// Exact emitted device token.
    pub device: String,
    pub kind: PassiveKind,
    /// Signed voltage/current target. Absent differs from explicit zero.
    pub target: Option<String>,
    /// Omitted BRANCH differs from either explicit boolean value.
    pub branch: Option<String>,
    /// Optional per-occurrence override of the deck's default mode.
    pub mode: Option<String>,
}

/// Source node hint, with first-definition-wins ordering.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeInitialCondition {
    pub artifact: String,
    pub scope: Vec<String>,
    pub node: String,
    pub target: String,
}

/// Deck-wide policy (root option expressions are evaluated in root scope).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InitialConditions {
    /// INITCONDMODE; omission selects the source default 2.
    pub mode: Option<String>,
    /// ICRES in ohms; omission selects 0.002 ohm.
    pub resistance: Option<String>,
    pub devices: Vec<PassiveInitialCondition>,
    pub nodes: Vec<NodeInitialCondition>,
}

/// Resolved immutable bias policy. The phase owner must not apply this to
/// transient steps or AC linearization after the bias solve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BiasConstraint {
    None,
    /// Exact differential voltage constraint (extra MNA branch).
    CapacitorHard {
        voltage: f64,
    },
    /// Finite voltage-source conductance during bias only.
    CapacitorSoft {
        voltage: f64,
        resistance: f64,
    },
    /// Existing inductor branch row enforces signed terminal current.
    InductorHard {
        current: f64,
    },
    /// Existing branch row enforces i - current - conductance*v = 0.
    InductorSoft {
        current: f64,
        conductance: f64,
    },
    /// Node-to-ground finite voltage forcing during bias only.
    NodeSoft {
        voltage: f64,
        resistance: f64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedDefaults {
    pub mode: Mode,
    pub resistance: f64,
}

/// Explicit storage state is distinct from a bias target. For UIC the
/// solver uses explicit storage state first, then differential node hints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedPassive {
    pub bias: BiasConstraint,
    pub storage_ic: Option<f64>,
}

fn finite(value: f64) -> Result<f64> {
    ensure!(
        value.is_finite(),
        "non-finite passive initial-condition value"
    );
    Ok(value)
}

fn evaluate(
    expression: Option<&str>,
    resolve: &mut impl FnMut(&str) -> Result<f64>,
) -> Result<Option<f64>> {
    expression
        .map(|value| resolve(value).and_then(finite))
        .transpose()
}

impl InitialConditions {
    /// Verify declaration identities against the final emitted electrical artifact.
    ///
    /// Translation may replace a source device or flatten its definition.
    /// Do not let metadata silently bind a missing or ambiguous storage row.
    ///
    /// # Errors
    /// Missing or duplicate device identities, or missing node-hint scope.
    pub fn validate_artifact(&self, filename: &str, bytes: &[u8]) -> Result<()> {
        use std::collections::{BTreeMap, BTreeSet};
        let canonical = |scope: &[String]| {
            scope
                .iter()
                .map(|part| part.to_ascii_lowercase())
                .collect::<Vec<_>>()
        };
        let mut expected = BTreeMap::new();
        for declaration in self
            .devices
            .iter()
            .filter(|device| device.artifact == filename)
        {
            let key = (
                canonical(&declaration.scope),
                declaration.device.to_ascii_lowercase(),
            );
            ensure!(
                expected.insert(key, false).is_none(),
                "duplicate initialization declaration {}",
                declaration.device
            );
        }
        let node_scopes = self
            .nodes
            .iter()
            .filter(|node| node.artifact == filename)
            .map(|node| canonical(&node.scope))
            .collect::<BTreeSet<_>>();
        if expected.is_empty() && node_scopes.is_empty() {
            return Ok(());
        }
        let mut seen_scopes = BTreeSet::from([Vec::new()]);
        let mut scope = Vec::new();
        for statement in crate::lexical::statements(bytes)? {
            let text = std::str::from_utf8(&statement.text)?;
            let mut fields = text.split_ascii_whitespace();
            let Some(first) = fields.next() else { continue };
            if first.eq_ignore_ascii_case(".subckt") {
                scope.push(
                    fields
                        .next()
                        .context("unnamed emitted subcircuit")?
                        .to_ascii_lowercase(),
                );
                if node_scopes.contains(&scope) {
                    seen_scopes.insert(scope.clone());
                }
            } else if first.eq_ignore_ascii_case(".ends") {
                ensure!(scope.pop().is_some(), "unmatched emitted .ends");
            } else if matches!(
                first.as_bytes().first().map(u8::to_ascii_uppercase),
                Some(b'C' | b'L')
            ) {
                let key = (scope.clone(), first.to_ascii_lowercase());
                if let Some(seen) = expected.get_mut(&key) {
                    ensure!(
                        !*seen,
                        "ambiguous emitted initialization target {first} in {scope:?}"
                    );
                    *seen = true;
                }
            }
        }
        for ((scope, device), seen) in expected {
            ensure!(
                seen,
                "missing emitted initialization target {device} in {scope:?} ({filename})"
            );
        }
        ensure!(
            node_scopes.is_subset(&seen_scopes),
            "missing emitted node initialization scope in {filename}"
        );
        Ok(())
    }
}

impl InitialConditions {
    /// Bind root expressions for this enclosing analysis. The caller
    /// supplies its root parameter resolver, including ANALYSIS.
    ///
    /// # Errors
    /// Invalid mode, unresolved values, or non-positive ICRES.
    pub fn resolve_defaults(
        &self,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<ResolvedDefaults> {
        let mode = evaluate(self.mode.as_deref(), &mut resolve)?
            .map(Mode::from_value)
            .transpose()?
            .unwrap_or_default();
        let resistance = evaluate(self.resistance.as_deref(), &mut resolve)?.unwrap_or(0.002);
        ensure!(resistance > 0., "ICRES must be positive");
        Ok(ResolvedDefaults { mode, resistance })
    }
}

impl PassiveInitialCondition {
    /// Bind one elaborated occurrence for the actual enclosing analysis.
    /// The supplied resolver must use the occurrence's lexical parameters
    /// and the enclosing analysis code, including nested source functions.
    ///
    /// # Errors
    /// Unresolved or invalid scalar values and modes.
    pub fn resolve(
        &self,
        analysis: Analysis,
        defaults: ResolvedDefaults,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<ResolvedPassive> {
        let storage_ic = evaluate(self.target.as_deref(), &mut resolve)?;
        let mode = evaluate(self.mode.as_deref(), &mut resolve)?
            .map(Mode::from_value)
            .transpose()?
            .unwrap_or(defaults.mode);
        let branch = evaluate(self.branch.as_deref(), &mut resolve)?;
        if analysis.uic() {
            return Ok(ResolvedPassive {
                bias: BiasConstraint::None,
                storage_ic,
            });
        }
        let explicit_hard = branch.is_some_and(|value| match self.kind {
            PassiveKind::Capacitor => value != 0.,
            PassiveKind::Inductor => value == 0.,
        });
        let target = storage_ic.unwrap_or_default();
        let dc = analysis == Analysis::DcSweep;
        let hard = explicit_hard
            || (!dc && mode == Mode::Hard && branch.is_none() && storage_ic.is_some());
        let soft = !dc && !hard && mode == Mode::Soft && storage_ic.is_some();
        let bias = match (self.kind, hard, soft) {
            (PassiveKind::Capacitor, true, _) => BiasConstraint::CapacitorHard { voltage: target },
            (PassiveKind::Inductor, true, _) => BiasConstraint::InductorHard { current: target },
            (PassiveKind::Capacitor, false, true) => {
                ensure!(
                    defaults.resistance.is_finite() && defaults.resistance > 0.,
                    "ICRES must be finite and positive"
                );
                BiasConstraint::CapacitorSoft {
                    voltage: target,
                    resistance: defaults.resistance,
                }
            }
            (PassiveKind::Inductor, false, true) => BiasConstraint::InductorSoft {
                current: target,
                conductance: 1e-9,
            },
            _ => BiasConstraint::None,
        };
        Ok(ResolvedPassive { bias, storage_ic })
    }
}

impl NodeInitialCondition {
    /// Bind a node hint and its bias constraint. The returned voltage is
    /// retained for UIC even though no bias stamp is active there.
    ///
    /// # Errors
    /// Unresolved target or invalid resistance.
    pub fn resolve(
        &self,
        analysis: Analysis,
        defaults: ResolvedDefaults,
        mut resolve: impl FnMut(&str) -> Result<f64>,
    ) -> Result<(f64, BiasConstraint)> {
        let voltage = finite(resolve(&self.target)?)?;
        if analysis.uic() || analysis == Analysis::DcSweep {
            return Ok((voltage, BiasConstraint::None));
        }
        ensure!(
            defaults.resistance.is_finite() && defaults.resistance > 0.,
            "ICRES must be finite and positive"
        );
        Ok((
            voltage,
            BiasConstraint::NodeSoft {
                voltage,
                resistance: defaults.resistance,
            },
        ))
    }
}

/// Compile and evaluate an isolated normalized expression with the
/// predefined analysis symbol. For actual source-defined functions and
/// nested instance scopes use the solver's parameter resolver instead.
///
/// # Errors
/// Invalid expression or unresolved symbol.
pub fn evaluate_expression(
    expression: &str,
    analysis: Analysis,
    mut resolve: impl FnMut(&str) -> Result<f64>,
) -> Result<f64> {
    SpiceExpression::parse(expression)?.evaluate(|name| {
        if name.eq_ignore_ascii_case("analysis") {
            Ok(analysis.source_code())
        } else {
            resolve(name).with_context(|| format!("passive initialization symbol {name}"))
        }
    })
}

/// Parse one logical source statement in its already linked lexical scope.
///
/// Returns a statement with all source-specific passive IC fields removed.
/// The typed policy owns both storage IC and node hints; native IC semantics
/// must not also be applied.
///
/// # Errors
/// Malformed/duplicate fields or invalid scalar syntax.
pub fn capture_statement(
    conditions: &mut InitialConditions,
    artifact: &str,
    scope: &[String],
    statement: &str,
) -> Result<String> {
    use crate::lexical;
    let Some(first) = statement.split_whitespace().next() else {
        return Ok(statement.to_owned());
    };
    if !matches!(
        first.as_bytes().first().map(u8::to_ascii_uppercase),
        Some(b'C' | b'L')
    ) && ![".ic", ".option", ".options"]
        .iter()
        .any(|name| first.eq_ignore_ascii_case(name))
    {
        return Ok(statement.to_owned());
    }
    let active = lexical::statements(statement.as_bytes())?;
    let [logical] = active.as_slice() else {
        return Ok(statement.to_owned());
    };
    let text = std::str::from_utf8(&logical.text)?;
    let spans = lexical::tokens(&logical.text)?;
    let fields = spans
        .iter()
        .map(|span| &text[span.clone()])
        .collect::<Vec<_>>();
    let Some(first) = fields.first() else {
        return Ok(statement.to_owned());
    };
    if first.eq_ignore_ascii_case(".ic") {
        return capture_nodes(conditions, artifact, scope, &fields[1..]);
    }
    if first.eq_ignore_ascii_case(".options") || first.eq_ignore_ascii_case(".option") {
        let retained = capture_options(conditions, scope, &fields[1..])?;
        return Ok(if retained.is_empty() {
            "* source initialization options recorded in typed policy".to_owned()
        } else {
            format!("{first} {}", retained.join(" "))
        });
    }
    let kind = match first.as_bytes().first().map(u8::to_ascii_uppercase) {
        Some(b'C') => PassiveKind::Capacitor,
        Some(b'L') => PassiveKind::Inductor,
        _ => return Ok(statement.to_owned()),
    };
    let mut target = None;
    let mut branch = None;
    let mut mode = None;
    let mut retained = fields.iter().take(3).copied().collect::<Vec<_>>();
    let mut i = 3;
    while let Some(field) = fields.get(i) {
        if fields.get(i + 1) != Some(&"=") {
            // A positional model name can itself be IC or BRANCH.
            retained.push(field);
            i += 1;
            continue;
        }
        let value = fields
            .get(i + 2)
            .context("missing passive assignment value")?;
        let slot = if field.eq_ignore_ascii_case("IC") {
            Some(&mut target)
        } else if field.eq_ignore_ascii_case("BRANCH") {
            Some(&mut branch)
        } else if field.eq_ignore_ascii_case("ICMODE") {
            Some(&mut mode)
        } else {
            None
        };
        if let Some(slot) = slot {
            let value = normalized_scalar(value)?;
            ensure!(slot.replace(value).is_none(), "{first}: duplicate {field}");
        } else {
            retained.extend([*field, "=", *value]);
        }
        i += 3;
    }
    if target.is_none() && branch.is_none() && mode.is_none() {
        return Ok(statement.to_owned());
    }
    ensure!(fields.len() >= 4, "incomplete passive declaration {first}");
    // All ICs are bound by the typed policy, including analysis-dependent
    // explicit storage values. Do not leave an unbound ANALYSIS expression
    // in the ordinary native device parser, or duplicate the native hint.
    conditions.devices.push(PassiveInitialCondition {
        artifact: artifact.to_owned(),
        scope: scope.to_vec(),
        device: (*first).to_owned(),
        kind,
        target,
        branch,
        mode,
    });
    // Rejoin top-level '=' pairs in the compact form expected by translation.
    let mut output = String::new();
    for (index, field) in retained.iter().enumerate() {
        if index != 0 && *field != "=" && retained[index - 1] != "=" {
            output.push(' ');
        }
        output.push_str(field);
    }
    Ok(output)
}

/// Normalize scalar comparison spelling. Other function/operator lowering
/// is performed by the source dialect pass before reaching this boundary.
fn normalized_scalar(raw: &str) -> Result<String> {
    let mut output = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    for (i, ch) in raw.char_indices() {
        if ch == '='
            && !matches!(
                i.checked_sub(1).and_then(|p| bytes.get(p)),
                Some(b'=' | b'!' | b'<' | b'>')
            )
            && bytes.get(i + 1) != Some(&b'=')
        {
            output.push('=');
        }
        output.push(ch);
    }
    SpiceExpression::parse(&output)?;
    Ok(output)
}

fn capture_options<'a>(
    conditions: &mut InitialConditions,
    scope: &[String],
    fields: &[&'a str],
) -> Result<Vec<&'a str>> {
    let mut retained = Vec::new();
    let mut i = 0;
    while let Some(field) = fields.get(i) {
        let slot = if field.eq_ignore_ascii_case("INITCONDMODE") {
            Some(&mut conditions.mode)
        } else if field.eq_ignore_ascii_case("ICRES") {
            Some(&mut conditions.resistance)
        } else {
            None
        };
        if let Some(slot) = slot {
            ensure!(scope.is_empty(), "{field} must be defined at deck scope");
            ensure!(
                fields.get(i + 1) == Some(&"="),
                "{field} requires an assignment"
            );
            let value = fields
                .get(i + 2)
                .context("missing initialization option value")?;
            *slot = Some(normalized_scalar(value)?);
            i += 3;
        } else if fields.get(i + 1) == Some(&"=") {
            retained.extend([
                *field,
                "=",
                *fields.get(i + 2).context("missing option value")?,
            ]);
            i += 3;
        } else {
            retained.push(field);
            i += 1;
        }
    }
    Ok(retained)
}

fn capture_nodes(
    conditions: &mut InitialConditions,
    artifact: &str,
    scope: &[String],
    fields: &[&str],
) -> Result<String> {
    let mut i = 0;
    while let Some(field) = fields.get(i) {
        let node = if field
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("v("))
        {
            ensure!(
                fields.get(i + 1) == Some(&"="),
                ".IC voltage target requires '='"
            );
            i += 2;
            field[2..]
                .strip_suffix(')')
                .context("invalid .IC node reference")?
                .trim()
        } else {
            i += 1;
            *field
        };
        ensure!(
            !node.is_empty() && !node.contains(',') && !node.contains(char::is_whitespace),
            "invalid .IC node {node}"
        );
        let target = normalized_scalar(fields.get(i).context("missing .IC target")?)?;
        if !conditions.nodes.iter().any(|existing| {
            existing.artifact == artifact
                && existing.scope == scope
                && existing.node.eq_ignore_ascii_case(node)
        }) {
            conditions.nodes.push(NodeInitialCondition {
                artifact: artifact.to_owned(),
                scope: scope.to_vec(),
                node: node.to_owned(),
                target,
            });
        }
        i += 1;
    }
    // Node hints are kept exclusively in the typed policy so native
    // last-wins / row-replacement rules cannot run as well.
    Ok("* source node initialization recorded in typed policy".to_owned())
}
