//! Explicit `SIMetrix` 9.2 logic-family matrices, independent of native bridge emission.
//! Later `SIMetrix` releases use physical analog voltage compatibility checks.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

/// Physical family name including an optional bridge-specific subfamily.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct LogicFamily {
    full: String,
    base: String,
}

impl LogicFamily {
    /// Parse a case-insensitive family, preserving the distinct suffix.
    ///
    /// # Errors
    /// Rejects malformed base families and empty or invalid subfamilies.
    pub fn parse(value: &str) -> Result<Self> {
        let full = value.to_ascii_uppercase();
        let (base, suffix) = full
            .split_once('_')
            .map_or((full.as_str(), None), |(b, s)| (b, Some(s)));
        if base.is_empty()
            || !base
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            || !base
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || suffix.is_some_and(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        {
            bail!("invalid SIMetrix logic family {value:?}");
        }
        let base = base.to_owned();
        Ok(Self { full, base })
    }

    #[must_use = "the name identifies the full family"]
    pub fn full_name(&self) -> &str {
        &self.full
    }

    #[must_use = "the name identifies the base family"]
    pub fn base_name(&self) -> &str {
        &self.base
    }

    #[must_use = "the result distinguishes a subfamily from its base"]
    pub fn is_subfamily(&self) -> bool {
        self.full != self.base
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigitalPinDirection {
    Input,
    Output,
}

/// `IN_FAMILY` and `OUT_FAMILY` override `FAMILY` independently.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LogicFamilyAssignment {
    pub family: Option<String>,
    pub in_family: Option<String>,
    pub out_family: Option<String>,
}

impl LogicFamilyAssignment {
    /// # Errors
    /// Invalid selected family name.
    pub fn for_pin(&self, direction: DigitalPinDirection) -> Result<LogicFamily> {
        let chosen = match direction {
            DigitalPinDirection::Input => self.in_family.as_deref(),
            DigitalPinDirection::Output => self.out_family.as_deref(),
        };
        LogicFamily::parse(chosen.or(self.family.as_deref()).unwrap_or("UNIV"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PairRule {
    Ok,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriorityRule {
    Row,
    Column,
    Equivalent,
    Error,
}

impl PriorityRule {
    const fn reversed(self) -> Self {
        match self {
            Self::Row => Self::Column,
            Self::Column => Self::Row,
            Self::Equivalent => Self::Equivalent,
            Self::Error => Self::Error,
        }
    }
}

/// User-supplied three-matrix legacy table. We deliberately do not invent
/// undocumented built-in cross-family cells.
#[derive(Clone, Debug)]
pub struct LegacyCompatibilityTable {
    families: Vec<String>,
    indices: BTreeMap<String, usize>,
    input_output: Vec<Vec<PairRule>>,
    input_input: Vec<Vec<PriorityRule>>,
    output_output: Vec<Vec<PriorityRule>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigitalFamilyResolution {
    pub input_base: Option<String>,
    pub output_base: Option<String>,
    pub warnings: Vec<String>,
}

impl LegacyCompatibilityTable {
    /// Parse the COMPAT.TXT format: one family header, N rows for IN-OUT,
    /// N rows for IN-IN, N rows for OUT-OUT, with whitespace-separated cells.
    /// UNIV is reserved and cannot be redefined.
    ///
    /// # Errors
    /// Rejects absent, malformed or contradictory matrix entries.
    pub fn parse(text: &str) -> Result<Self> {
        let rows = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with(['#', '*', ';']))
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let header = rows.first().context("missing compatibility header")?;
        let n = header.len();
        if n == 0 || rows.len() != 1 + 3 * n {
            bail!(
                "expected header + 3*N matrix rows for {n} families; got {} rows",
                rows.len()
            );
        }
        let mut families = Vec::new();
        let mut indices = BTreeMap::new();
        for (idx, value) in header.iter().enumerate() {
            let parsed = LogicFamily::parse(value)?;
            if parsed.is_subfamily() || parsed.base_name() == "UNIV" {
                bail!("compatibility header cannot redefine a subfamily or UNIV: {value}");
            }
            if indices.insert(parsed.base.clone(), idx).is_some() {
                bail!("duplicate compatibility family {value}");
            }
            families.push(parsed.base);
        }
        let mut input_output = Vec::new();
        let mut input_input = Vec::new();
        let mut output_output = Vec::new();
        for (index, cells) in rows.iter().skip(1).enumerate() {
            if cells.len() != n {
                bail!(
                    "compatibility row {} has {} entries; expected {n}",
                    index + 1,
                    cells.len()
                );
            }
            match index / n {
                0 => input_output.push(
                    cells
                        .iter()
                        .map(|cell| match cell.to_ascii_uppercase().as_str() {
                            "OK" => Ok(PairRule::Ok),
                            "WARN" => Ok(PairRule::Warn),
                            "ERR" => Ok(PairRule::Error),
                            _ => bail!("unknown IN-OUT matrix entry {cell}"),
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
                side @ (1 | 2) => {
                    let parsed = cells
                        .iter()
                        .map(|cell| match cell.to_ascii_uppercase().as_str() {
                            "ROW" => Ok(PriorityRule::Row),
                            "COL" => Ok(PriorityRule::Column),
                            "OK" => Ok(PriorityRule::Equivalent),
                            "ERR" => Ok(PriorityRule::Error),
                            _ => bail!("unknown priority matrix entry {cell}"),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if side == 1 {
                        input_input.push(parsed);
                    } else {
                        output_output.push(parsed);
                    }
                }
                _ => bail!("unexpected compatibility section"),
            }
        }
        for (name, matrix) in [("IN-IN", &input_input), ("OUT-OUT", &output_output)] {
            for i in 0..n {
                for j in i + 1..n {
                    if matrix[i][j].reversed() != matrix[j][i] {
                        bail!(
                            "{name} matrix has inconsistent priorities for {} and {}",
                            families[i],
                            families[j]
                        );
                    }
                }
            }
        }
        Ok(Self {
            families,
            indices,
            input_output,
            input_input,
            output_output,
        })
    }

    #[must_use = "the returned slice enumerates known families"]
    pub fn families(&self) -> &[String] {
        &self.families
    }

    fn index(&self, family: &str) -> Result<usize> {
        self.indices
            .get(family)
            .copied()
            .with_context(|| format!("family {family} missing from explicit compatibility table"))
    }

    fn priority(
        &self,
        direction: DigitalPinDirection,
        row: &str,
        col: &str,
    ) -> Result<PriorityRule> {
        if row == col {
            return Ok(PriorityRule::Equivalent);
        }
        if row == "UNIV" {
            return Ok(PriorityRule::Column);
        }
        if col == "UNIV" {
            return Ok(PriorityRule::Row);
        }
        let (row, col) = (self.index(row)?, self.index(col)?);
        Ok(match direction {
            DigitalPinDirection::Input => self.input_input[row][col],
            DigitalPinDirection::Output => self.output_output[row][col],
        })
    }

    fn resolve_group(
        &self,
        direction: DigitalPinDirection,
        families: &[LogicFamily],
    ) -> Result<Option<String>> {
        let members = families
            .iter()
            .map(|f| f.base_name().to_owned())
            .collect::<BTreeSet<_>>();
        if members.is_empty() {
            return Ok(None);
        }
        for f in &members {
            if f != "UNIV" {
                self.index(f)?;
            }
        }
        for left in &members {
            for right in &members {
                if left < right && self.priority(direction, left, right)? == PriorityRule::Error {
                    bail!("incompatible {direction:?} families {left} and {right}");
                }
            }
        }
        // An order-independent priority result must dominate or tie
        // *every* connected family; a cycle is not a valid resolved family.
        let mut top = Vec::new();
        for candidate in &members {
            if members.iter().all(|other| {
                self.priority(direction, candidate, other)
                    .is_ok_and(|p| p != PriorityRule::Column)
            }) {
                top.push(candidate);
            }
        }
        if top.is_empty() {
            bail!("cyclic {direction:?} family priorities: {members:?}");
        }
        for left in &top {
            for right in &top {
                if self.priority(direction, left, right)? != PriorityRule::Equivalent {
                    bail!("ambiguous {direction:?} priorities: {members:?}");
                }
            }
        }
        // In an equivalent tie, the digital family can be chosen
        // deterministically, but distinct analog subfamilies remain separate.
        Ok(top.first().map(|f| (*f).clone()))
    }

    /// Resolve family compatibility for a shared all-digital net.
    /// The chosen group family is used for the IN-OUT decision.
    ///
    /// # Errors
    /// Rejects unavailable table cells, incompatible or cyclic priorities.
    pub fn resolve_digital_net(
        &self,
        inputs: &[LogicFamily],
        outputs: &[LogicFamily],
    ) -> Result<DigitalFamilyResolution> {
        let input_base = self.resolve_group(DigitalPinDirection::Input, inputs)?;
        let output_base = self.resolve_group(DigitalPinDirection::Output, outputs)?;
        let mut warnings = Vec::new();
        if let (Some(input), Some(output)) = (&input_base, &output_base) {
            let compatibility = if input == "UNIV" || output == "UNIV" {
                PairRule::Ok
            } else {
                self.input_output[self.index(input)?][self.index(output)?]
            };
            match compatibility {
                PairRule::Ok => {}
                PairRule::Warn => warnings.push(format!(
                    "{output} output drives {input} input with WARN compatibility"
                )),
                PairRule::Error => bail!("incompatible {output} output driving {input} input"),
            }
        }
        Ok(DigitalFamilyResolution {
            input_base,
            output_base,
            warnings,
        })
    }
}

/// Native bridge direction relative to the analog circuit.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalogBridgeKind {
    Adc,
    Dac,
}

impl AnalogBridgeKind {
    const fn for_pin(direction: DigitalPinDirection) -> Self {
        match direction {
            DigitalPinDirection::Input => Self::Adc,
            DigitalPinDirection::Output => Self::Dac,
        }
    }
}

/// Exact external model definition, including the nominal supply for later
/// family-specific scaling. This type does NOT assume all parameters scale.
#[derive(Clone, Debug, PartialEq)]
pub struct BridgeModel {
    pub name: String,
    pub kind: AnalogBridgeKind,
    pub family: LogicFamily,
    pub nominal_supply_v: f64,
}

/// Catalog must be explicitly provided. Do not assume that stock ngspice
/// bridge models reproduce `SIMetrix` electrical behavior.
#[derive(Clone, Debug, Default)]
pub struct BridgeCatalog {
    models: BTreeMap<(LogicFamily, AnalogBridgeKind), BridgeModel>,
}

impl BridgeCatalog {
    /// # Errors
    /// Duplicate bridge, invalid identifier, or nonphysical nominal supply.
    pub fn add(&mut self, model: BridgeModel) -> Result<()> {
        if model.name.is_empty()
            || !model
                .name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic())
            || !model
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !model.nominal_supply_v.is_finite()
            || model.nominal_supply_v <= 0.0
        {
            bail!("invalid explicit native bridge model: {model:?}");
        }
        let key = (model.family.clone(), model.kind);
        if self.models.contains_key(&key) {
            bail!("duplicate explicit family bridge {key:?}");
        }
        self.models.insert(key, model);
        Ok(())
    }

    fn get(&self, family: &LogicFamily, kind: AnalogBridgeKind) -> Result<&BridgeModel> {
        self.models.get(&(family.clone(), kind)).with_context(|| {
            format!(
                "missing exact {kind:?} bridge model for {}",
                family.full_name()
            )
        })
    }
}

/// One digital pin connected to an analog net. IDs must remain unique
/// after the caller flattens the hierarchy.
#[derive(Clone, Debug)]
pub struct AnalogBoundaryPin {
    pub id: String,
    pub analog_net: String,
    pub direction: DigitalPinDirection,
    pub family: LogicFamily,
    pub supply_v: f64,
}

/// One physical native code-model bridge. Supplies stay explicit and
/// unscaled until the family-specific scaling law has been established.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedAnalogBridge {
    pub analog_net: String,
    pub model_name: String,
    pub kind: AnalogBridgeKind,
    pub family: LogicFamily,
    pub supply_v: f64,
    pub nominal_supply_v: f64,
    pub physical_pins: Vec<String>,
}

/// Plan physical analog bridge groups using explicit family and supply.
///
/// Inputs share a bridge only for the same net, full `IN_FAMILY`
/// (including Schmitt suffix), and exact `VSUPPLY`. Every independent
/// digital output keeps its own DAC to preserve driver contention.
///
/// # Errors
/// Rejects absent bridge models, duplicate pins, invalid net identifiers,
/// or an unavailable VSUPPLY. Does not emit a native netlist yet.
pub fn plan_analog_bridges(
    pins: &[AnalogBoundaryPin],
    catalog: &BridgeCatalog,
) -> Result<Vec<PlannedAnalogBridge>> {
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::<
        (String, AnalogBridgeKind, LogicFamily, u64, Option<String>),
        PlannedAnalogBridge,
    >::new();
    for pin in pins {
        if pin.id.is_empty()
            || pin.id.chars().any(char::is_whitespace)
            || pin.analog_net.is_empty()
            || pin.analog_net.chars().any(char::is_whitespace)
            || !ids.insert(pin.id.clone())
        {
            bail!("invalid or duplicate analog boundary pin {:?}", pin.id);
        }
        if !pin.supply_v.is_finite() || pin.supply_v <= 0.0 {
            bail!(
                "missing or invalid VSUPPLY for pin {}: {}",
                pin.id,
                pin.supply_v
            );
        }
        let kind = AnalogBridgeKind::for_pin(pin.direction);
        let model = catalog.get(&pin.family, kind)?;
        // Node names are case-insensitive in SPICE. Distinct digital
        // outputs are never merged even if their family/supply match.
        let net = pin.analog_net.to_ascii_uppercase();
        let independent_driver = if kind == AnalogBridgeKind::Dac {
            Some(pin.id.clone())
        } else {
            None
        };
        let key = (
            net.clone(),
            kind,
            pin.family.clone(),
            pin.supply_v.to_bits(),
            independent_driver,
        );
        let group = groups.entry(key).or_insert_with(|| PlannedAnalogBridge {
            analog_net: net,
            model_name: model.name.clone(),
            kind,
            family: pin.family.clone(),
            supply_v: pin.supply_v,
            nominal_supply_v: model.nominal_supply_v,
            physical_pins: Vec::new(),
        });
        group.physical_pins.push(pin.id.clone());
    }
    let mut result = groups.into_values().collect::<Vec<_>>();
    for group in &mut result {
        group.physical_pins.sort();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINI: &str = "
        HC LS ECL
        OK WARN ERR
        OK OK ERR
        ERR ERR OK

        ROW ROW ERR
        COL ROW ERR
        ERR ERR ROW

        ROW ROW ERR
        COL ROW ERR
        ERR ERR ROW
    ";

    fn fam(name: &str) -> Result<LogicFamily> {
        LogicFamily::parse(name)
    }

    fn model(
        name: &str,
        family: &str,
        kind: AnalogBridgeKind,
        nominal: f64,
    ) -> Result<BridgeModel> {
        Ok(BridgeModel {
            name: name.to_owned(),
            kind,
            family: fam(family)?,
            nominal_supply_v: nominal,
        })
    }

    fn pin(
        id: &str,
        net: &str,
        dir: DigitalPinDirection,
        family: &str,
        supply: f64,
    ) -> Result<AnalogBoundaryPin> {
        Ok(AnalogBoundaryPin {
            id: id.to_owned(),
            analog_net: net.to_owned(),
            direction: dir,
            family: fam(family)?,
            supply_v: supply,
        })
    }

    #[test]
    fn family_precedence_subfamily_and_validation() -> Result<()> {
        let assignments = LogicFamilyAssignment {
            family: Some("LS".into()),
            in_family: Some("hc_schmitt".into()),
            out_family: None,
        };
        assert_eq!(
            assignments.for_pin(DigitalPinDirection::Input)?.full_name(),
            "HC_SCHMITT"
        );
        assert_eq!(
            assignments.for_pin(DigitalPinDirection::Input)?.base_name(),
            "HC"
        );
        assert_eq!(
            assignments
                .for_pin(DigitalPinDirection::Output)?
                .full_name(),
            "LS"
        );
        assert_eq!(
            LogicFamilyAssignment::default()
                .for_pin(DigitalPinDirection::Input)?
                .full_name(),
            "UNIV"
        );
        assert!(fam("HC_").is_err());
        assert!(fam("_HC").is_err());
        assert!(fam("HC HELLO").is_err());
        assert!(fam("HC/LS").is_err());
        assert_eq!(fam("4000-15")?.full_name(), "4000-15");
        Ok(())
    }

    #[test]
    fn asymmetric_compatibility_warn_and_error() -> Result<()> {
        let table = LegacyCompatibilityTable::parse(MINI)?;
        assert_eq!(table.families(), &["HC", "LS", "ECL"]);
        let hc_to_ls = table.resolve_digital_net(&[fam("LS")?], &[fam("HC")?])?;
        assert_eq!(hc_to_ls.warnings.len(), 0);
        let ls_to_hc = table.resolve_digital_net(&[fam("HC_SCHMITT")?], &[fam("LS")?])?;
        assert_eq!(ls_to_hc.warnings.len(), 1);
        assert!(ls_to_hc.warnings[0].contains("WARN"));
        assert!(
            table
                .resolve_digital_net(&[fam("ECL")?], &[fam("HC")?])
                .is_err()
        );
        assert!(
            table
                .resolve_digital_net(&[fam("LS")?], &[fam("ECL")?])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn universal_logic_is_not_a_redefined_matrix_cell() -> Result<()> {
        let table = LegacyCompatibilityTable::parse(MINI)?;
        let result = table.resolve_digital_net(
            &[fam("UNIV")?, fam("LS")?, fam("HC")?],
            &[fam("UNIV")?, fam("HC")?],
        )?;
        assert_eq!(result.input_base.as_deref(), Some("HC"));
        assert_eq!(result.output_base.as_deref(), Some("HC"));
        assert_eq!(result.warnings.len(), 0);
        assert!(
            LegacyCompatibilityTable::parse(
                "HC UNIV\nOK OK\nOK OK\nROW ROW\nCOL ROW\nROW ROW\nCOL ROW"
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn matrix_parser_rejects_incomplete_duplicate_and_contradictory_data() {
        assert!(LegacyCompatibilityTable::parse("").is_err());
        assert!(LegacyCompatibilityTable::parse("HC LS\nOK OK").is_err());
        assert!(LegacyCompatibilityTable::parse(&MINI.replace("HC LS ECL", "HC HC ECL")).is_err());
        assert!(
            LegacyCompatibilityTable::parse(&MINI.replace("OK WARN ERR", "OK MAGIC ERR")).is_err()
        );
        assert!(
            LegacyCompatibilityTable::parse(&MINI.replace("COL ROW ERR", "ROW ROW ERR")).is_err()
        );
        assert!(LegacyCompatibilityTable::parse(&format!("{MINI}\nROW ROW ROW")).is_err());
        assert!(
            LegacyCompatibilityTable::parse(&MINI.replace("HC LS ECL", "HC_SCHMITT LS ECL"))
                .is_err()
        );
    }

    #[test]
    fn pairwise_priorities_are_directional_and_order_independent() -> Result<()> {
        let table = LegacyCompatibilityTable::parse(MINI)?;
        for group in [
            vec![fam("LS")?, fam("HC")?, fam("UNIV")?],
            vec![fam("UNIV")?, fam("HC")?, fam("LS")?],
        ] {
            let resolved = table.resolve_digital_net(&group, &group)?;
            assert_eq!(resolved.input_base.as_deref(), Some("HC"));
            assert_eq!(resolved.output_base.as_deref(), Some("HC"));
        }
        assert!(table.resolve_digital_net(&[fam("UNKNOWN")?], &[]).is_err());
        Ok(())
    }

    #[test]
    fn cyclic_priority_fails_closed() -> Result<()> {
        let table = LegacyCompatibilityTable::parse(
            "
            A B C
            OK OK OK
            OK OK OK
            OK OK OK

            ROW ROW COL
            COL ROW ROW
            ROW COL ROW

            ROW ROW COL
            COL ROW ROW
            ROW COL ROW
        ",
        )?;
        assert!(
            table
                .resolve_digital_net(&[fam("A")?, fam("B")?, fam("C")?], &[])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn distinct_full_families_and_supplies_keep_separate_physical_adcs() -> Result<()> {
        let mut catalog = BridgeCatalog::default();
        catalog.add(model("HC_ADC", "HC", AnalogBridgeKind::Adc, 5.0)?)?;
        catalog.add(model(
            "HC_SCHMITT_ADC",
            "HC_SCHMITT",
            AnalogBridgeKind::Adc,
            5.0,
        )?)?;
        catalog.add(model("HC_DAC", "HC", AnalogBridgeKind::Dac, 5.0)?)?;
        let pins = vec![
            pin("X1.in", "Analog", DigitalPinDirection::Input, "HC", 5.0)?,
            pin("X2.in", "ANALOG", DigitalPinDirection::Input, "HC", 5.0)?,
            pin(
                "X3.in",
                "ANALOG",
                DigitalPinDirection::Input,
                "HC_SCHMITT",
                5.0,
            )?,
            pin("X4.in", "ANALOG", DigitalPinDirection::Input, "HC", 3.3)?,
            pin("X5.in", "OTHER", DigitalPinDirection::Input, "HC", 5.0)?,
            pin("X6.out", "ANALOG", DigitalPinDirection::Output, "HC", 5.0)?,
            pin("X7.out", "ANALOG", DigitalPinDirection::Output, "HC", 5.0)?,
        ];
        let plan = plan_analog_bridges(&pins, &catalog)?;
        assert_eq!(plan.len(), 6);
        let shared = plan.iter().find(|p| p.physical_pins.len() == 2);
        assert_eq!(
            shared.map(|p| p.physical_pins.as_slice()),
            Some(["X1.in".to_owned(), "X2.in".to_owned()].as_slice())
        );
        assert_eq!(
            plan.iter()
                .filter(|p| p.kind == AnalogBridgeKind::Dac)
                .count(),
            2
        );
        assert_eq!(
            plan.iter()
                .filter(|p| p.kind == AnalogBridgeKind::Adc)
                .count(),
            4
        );
        assert!(
            plan.iter()
                .all(|p| p.nominal_supply_v.to_bits() == 5.0_f64.to_bits())
        );
        let reversed = pins.into_iter().rev().collect::<Vec<_>>();
        assert_eq!(plan, plan_analog_bridges(&reversed, &catalog)?);
        Ok(())
    }

    #[test]
    fn unknown_bridge_or_supply_and_duplicate_pin_fail_closed() -> Result<()> {
        let mut catalog = BridgeCatalog::default();
        catalog.add(model("HC_ADC", "HC", AnalogBridgeKind::Adc, 5.0)?)?;
        assert!(
            catalog
                .add(model("HC_ADC_2", "HC", AnalogBridgeKind::Adc, 5.0)?)
                .is_err()
        );
        assert!(
            catalog
                .add(model("HC_BAD", "HC", AnalogBridgeKind::Dac, f64::NAN)?)
                .is_err()
        );
        assert!(
            catalog
                .add(model("bad-model", "HC", AnalogBridgeKind::Dac, 5.0)?)
                .is_err()
        );
        let good = pin("X1.in", "n1", DigitalPinDirection::Input, "HC", 5.0)?;
        assert_eq!(
            plan_analog_bridges(std::slice::from_ref(&good), &catalog)?.len(),
            1
        );
        assert!(plan_analog_bridges(&[good.clone(), good], &catalog).is_err());
        assert!(
            plan_analog_bridges(
                &[pin("X1", "n1", DigitalPinDirection::Input, "HC", 0.0)?],
                &catalog
            )
            .is_err()
        );
        assert!(
            plan_analog_bridges(
                &[pin(
                    "X1",
                    "n1",
                    DigitalPinDirection::Input,
                    "HC",
                    f64::INFINITY
                )?],
                &catalog
            )
            .is_err()
        );
        assert!(
            plan_analog_bridges(
                &[pin("X1", "n1", DigitalPinDirection::Output, "HC", 5.0)?],
                &catalog
            )
            .is_err()
        );
        assert!(
            plan_analog_bridges(
                &[pin(
                    "X1",
                    "n1",
                    DigitalPinDirection::Input,
                    "HC_SCHMITT",
                    5.0
                )?],
                &catalog
            )
            .is_err()
        );
        Ok(())
    }
}
