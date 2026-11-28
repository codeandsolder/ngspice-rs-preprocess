//! Materialize fully explicit native XSPICE family bridges from a resolved
//! physical grouping plan. Nothing here enables production ALB conversion.

use super::OutputResistance;
use super::family::{AnalogBridgeKind, LogicFamily, PlannedAnalogBridge};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Three connected digital nodes for one physical ALB pin.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NativeDigitalBundle {
    pub state: String,
    pub sink_meta: String,
    pub source_meta: String,
}

/// Explicit native wiring for a physical ALB pin.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePinWiring {
    pub id: String,
    pub bundle: NativeDigitalBundle,
    pub out_res_ohm: Option<OutputResistance>,
}

/// Explicit family-model data for native emission.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFamilyModel {
    pub name: String,
    pub family: String,
    pub nominal_supply_v: f64,
    pub native: NativeFamilyParameters,
}

/// Required source-model parameters, without undocumented defaults.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "snake_case")]
pub enum NativeFamilyParameters {
    AdcBridge(NativeAdcBridgeParameters),
    AdcSchmitt(NativeAdcParameters),
    Dac(NativeDacParameters),
}

/// Parameters recognized by both original ADC and Schmitt models.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAdcParameters {
    pub in_low: f64,
    pub in_high: f64,
    pub rise_delay: f64,
    pub fall_delay: f64,
    pub time_tol: f64,
    pub out_low: f64,
    pub out_high: f64,
    pub clamp_res: f64,
    pub clamp_bias: f64,
}

/// An ordinary ADC bridge accepts explicit physical clamp bounds.
/// A Schmitt model has no such parameters, and cannot represent them.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAdcBridgeParameters {
    pub adc: NativeAdcParameters,
    pub clamp_low: f64,
    pub clamp_high: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDacParameters {
    pub out_low: f64,
    pub out_high: f64,
    /// Explicit model conductance overrides device `OUT_RES` fallback.
    pub g_pullup: Option<f64>,
    pub g_pulldown: Option<f64>,
    pub g_resistive: f64,
    pub g_hiz: f64,
    pub knee_low: f64,
    pub knee_high: f64,
    pub v_smooth: f64,
    pub input_load: f64,
    pub t_rise: f64,
    pub t_fall: f64,
    pub sink_current: f64,
    pub source_current: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeFamilySpecification {
    pub models: Vec<NativeFamilyModel>,
    #[serde(default)]
    pub pins: Vec<NativePinWiring>,
}

fn spice_identifier(label: &str, value: &str) -> Result<String> {
    if value.is_empty()
        || !value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        bail!("invalid {label} XSPICE identifier {value:?}");
    }
    Ok(value.to_ascii_uppercase())
}

fn digital_node(label: &str, value: &str) -> Result<String> {
    crate::validate_node(value)
        .with_context(|| format!("invalid {label} XSPICE node {value:?}"))?;
    if value == "0" {
        bail!("digital node {label} cannot use analog ground 0");
    }
    Ok(value.to_ascii_uppercase())
}

fn analog_node(value: &str) -> Result<String> {
    crate::validate_node(value).context("invalid analog boundary node")?;
    Ok(value.to_ascii_uppercase())
}

fn checked_real(label: &str, number: f64) -> Result<f64> {
    if !number.is_finite() {
        bail!("non-finite native family parameter {label}: {number}");
    }
    Ok(number)
}

fn positive(label: &str, number: f64) -> Result<f64> {
    let number = checked_real(label, number)?;
    if number <= 0.0 {
        bail!("native family {label} must be positive: {number}");
    }
    Ok(number)
}

impl NativeDigitalBundle {
    fn validated(&self) -> Result<Self> {
        let nodes = [
            digital_node("digital state", &self.state)?,
            digital_node("sink metadata", &self.sink_meta)?,
            digital_node("source metadata", &self.source_meta)?,
        ];
        if nodes[0] == nodes[1] || nodes[0] == nodes[2] || nodes[1] == nodes[2] {
            bail!("digital state and metadata must be distinct: {nodes:?}");
        }
        Ok(Self {
            state: nodes[0].clone(),
            sink_meta: nodes[1].clone(),
            source_meta: nodes[2].clone(),
        })
    }

    fn names(&self) -> [&str; 3] {
        [&self.state, &self.sink_meta, &self.source_meta]
    }
}

fn check_adc_model(name: &str, params: &NativeAdcParameters) -> Result<()> {
    for (label, value) in [
        ("in_low", params.in_low),
        ("in_high", params.in_high),
        ("out_low", params.out_low),
        ("out_high", params.out_high),
        ("rise_delay", params.rise_delay),
        ("fall_delay", params.fall_delay),
        ("time_tol", params.time_tol),
        ("clamp_bias", params.clamp_bias),
    ] {
        checked_real(label, value)?;
    }
    if matches!(
        params.out_low.partial_cmp(&params.out_high),
        Some(std::cmp::Ordering::Equal)
    ) {
        bail!("ADC {name} requires distinct output load levels");
    }
    if checked_real("clamp_res", params.clamp_res)? == 0.0 {
        bail!("zero clamp resistance for ADC {name}");
    }
    Ok(())
}

fn check_model(model: &NativeFamilyModel) -> Result<(String, LogicFamily)> {
    let name = spice_identifier("native bridge model", &model.name)?;
    let family = LogicFamily::parse(&model.family)?;
    positive("VSUPPLY_NOMINAL", model.nominal_supply_v)?;
    match &model.native {
        NativeFamilyParameters::AdcBridge(params) => {
            check_adc_model(&name, &params.adc)?;
            checked_real("clamp_low", params.clamp_low)?;
            checked_real("clamp_high", params.clamp_high)?;
        }
        NativeFamilyParameters::AdcSchmitt(params) => check_adc_model(&name, params)?,
        NativeFamilyParameters::Dac(params) => {
            for (label, value) in [
                ("out_low", params.out_low),
                ("out_high", params.out_high),
                ("knee_low", params.knee_low),
                ("knee_high", params.knee_high),
                ("input_load", params.input_load),
                ("sink_current", params.sink_current),
                ("source_current", params.source_current),
            ] {
                checked_real(label, value)?;
            }
            for (label, number) in [
                ("g_resistive", params.g_resistive),
                ("g_hiz", params.g_hiz),
                ("v_smooth", params.v_smooth),
            ] {
                checked_real(label, number)?;
            }
            for (label, number) in [("t_rise", params.t_rise), ("t_fall", params.t_fall)] {
                if checked_real(label, number)? == 0.0 {
                    bail!("zero {label} for DAC {name}");
                }
            }
            for (label, optional) in [
                ("g_pullup", params.g_pullup),
                ("g_pulldown", params.g_pulldown),
            ] {
                if let Some(number) = optional {
                    checked_real(label, number)?;
                }
            }
        }
    }
    Ok((name, family))
}

fn effective_conductance(
    label: &str,
    explicit: Option<f64>,
    out_res_ohm: Option<f64>,
) -> Result<f64> {
    if let Some(explicit) = explicit {
        return checked_real(label, explicit);
    }
    let out_res = positive(
        "OUT_RES",
        out_res_ohm.context("missing DAC conductance and digital driver's OUT_RES")?,
    )?;
    checked_real(label, 1.0 / out_res)
}

fn emit_adc(
    text: &mut String,
    model: &str,
    params: &NativeAdcParameters,
    clamps: Option<(f64, f64)>,
    scale: f64,
    voltage_scale: f64,
) -> Result<()> {
    let in_low = checked_real("scaled in_low", params.in_low * scale)?;
    let in_high = checked_real("scaled in_high", params.in_high * scale)?;
    let out_low = checked_real("scaled ADC out_low", params.out_low * voltage_scale)?;
    let out_high = checked_real("scaled ADC out_high", params.out_high * voltage_scale)?;
    if matches!(
        out_low.partial_cmp(&out_high),
        Some(std::cmp::Ordering::Equal)
    ) {
        bail!("scaling collapses ADC output load levels");
    }
    write!(
        text,
        ".model {model} sr_family_adc(in_low={in_low:e} in_high={in_high:e} \
         rise_delay={:e} fall_delay={:e} time_tol={:e} \
         out_low={out_low:e} out_high={out_high:e} ",
        params.rise_delay, params.fall_delay, params.time_tol,
    )?;
    if let Some((low, high)) = clamps {
        let clamp_low = checked_real("scaled clamp_low", low * voltage_scale)?;
        let clamp_high = checked_real("scaled clamp_high", high * voltage_scale)?;
        write!(text, "clamp_low={clamp_low:e} clamp_high={clamp_high:e} ")?;
    }
    writeln!(
        text,
        "clamp_res={:e} clamp_bias={:e} schmitt={})",
        params.clamp_res,
        params.clamp_bias,
        if clamps.is_some() { "false" } else { "true" },
    )?;
    Ok(())
}

fn emit_dac(
    text: &mut String,
    model: &str,
    params: &NativeDacParameters,
    scale: f64,
    voltage_scale: f64,
    out_res_ohm: Option<OutputResistance>,
) -> Result<()> {
    let out_low = checked_real("scaled out_low", params.out_low * scale)?;
    let out_high = checked_real("scaled out_high", params.out_high * scale)?;
    let knee_low = checked_real("scaled knee_low", params.knee_low * voltage_scale)?;
    let knee_high = checked_real("scaled knee_high", params.knee_high * voltage_scale)?;
    let v_smooth = checked_real("scaled v_smooth", params.v_smooth * voltage_scale)?;
    let g_pullup =
        effective_conductance("g_pullup", params.g_pullup, out_res_ohm.map(|r| r.positive))?;
    let g_pulldown = effective_conductance(
        "g_pulldown",
        params.g_pulldown,
        out_res_ohm.map(|r| r.negative),
    )?;
    writeln!(
        text,
        ".model {model} sr_family_dac(out_low={out_low:e} out_high={out_high:e} \
         g_pullup={g_pullup:e} g_pulldown={g_pulldown:e} \
         g_resistive={:e} g_hiz={:e} knee_low={knee_low:e} knee_high={knee_high:e} \
         v_smooth={v_smooth:e} input_load={:e} t_rise={:e} t_fall={:e} \
         sink_current={:e} source_current={:e})",
        params.g_resistive,
        params.g_hiz,
        params.input_load,
        params.t_rise,
        params.t_fall,
        params.sink_current,
        params.source_current
    )?;
    Ok(())
}

fn emit_native_model(
    text: &mut String,
    name: &str,
    model: &NativeFamilyParameters,
    scale: f64,
    voltage_scale: f64,
    out_res: Option<OutputResistance>,
) -> Result<()> {
    match model {
        NativeFamilyParameters::AdcBridge(params) => emit_adc(
            text,
            name,
            &params.adc,
            Some((params.clamp_low, params.clamp_high)),
            scale,
            voltage_scale,
        ),
        NativeFamilyParameters::AdcSchmitt(params) => {
            emit_adc(text, name, params, None, scale, voltage_scale)
        }
        NativeFamilyParameters::Dac(params) => {
            emit_dac(text, name, params, scale, voltage_scale, out_res)
        }
    }
}

struct IndexedNativeSpec<'a> {
    models: BTreeMap<String, &'a NativeFamilyModel>,
    wires: BTreeMap<&'a str, &'a NativePinWiring>,
}

fn index_native_spec(spec: &NativeFamilySpecification) -> Result<IndexedNativeSpec<'_>> {
    let mut models = BTreeMap::new();
    for entry in &spec.models {
        let (name, _) = check_model(entry)?;
        if models.insert(name.clone(), entry).is_some() {
            bail!("duplicate native family model {name}");
        }
    }
    let mut wires = BTreeMap::new();
    for entry in &spec.pins {
        if entry.id.is_empty() || wires.insert(entry.id.as_str(), entry).is_some() {
            bail!("empty or duplicate physical digital pin ID {:?}", entry.id);
        }
        if let Some(out_res) = entry.out_res_ohm {
            out_res.validate()?;
        }
        entry.bundle.validated()?;
    }
    Ok(IndexedNativeSpec { models, wires })
}

/// Materialize already-resolved family groups with exact physical bundles.
/// This never adds `sr_bundle_load`: each ALB physical input already
/// contributes precisely one load on its connected digital triple.
///
/// ADC thresholds and DAC output rails scale with supply. Original HC and UNIV
/// measurements additionally establish scaling of ADC load/clamp rails,
/// DAC knees and smoothing width; conductance, clamp bias/resistance,
/// capacitance and signed current metadata remain invariant.
///
/// # Errors
/// Rejects invalid models, missing wiring, contradictory shared ADC
/// bundles, overlapping physical bundles, or unrepresentable scaling.
pub fn render_native_family_bridges(
    plans: &[PlannedAnalogBridge],
    spec: &NativeFamilySpecification,
) -> Result<String> {
    let IndexedNativeSpec { models, wires } = index_native_spec(spec)?;
    let analog_nets = plans
        .iter()
        .map(|plan| analog_node(&plan.analog_net))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut consumed_pins = BTreeSet::new();
    let mut used_nodes = BTreeSet::new();
    let mut text = String::from(
        "* generated experimental physical family bridges; requires native ngspice 47 code model\n",
    );
    for (index, plan) in plans.iter().enumerate() {
        let native_name = spice_identifier("requested bridge model", &plan.model_name)?;
        let model = models.get(&native_name).with_context(|| {
            format!("missing native family parameter specification for {native_name}")
        })?;
        let family = LogicFamily::parse(&model.family)?;
        if family != plan.family
            || model.nominal_supply_v.to_bits() != plan.nominal_supply_v.to_bits()
        {
            bail!("resolved plan and native model metadata disagree for {native_name}");
        }
        let kind = match &model.native {
            NativeFamilyParameters::AdcBridge(_) | NativeFamilyParameters::AdcSchmitt(_) => {
                AnalogBridgeKind::Adc
            }
            NativeFamilyParameters::Dac(_) => AnalogBridgeKind::Dac,
        };
        if kind != plan.kind {
            bail!("native bridge kind differs from planned {native_name}");
        }
        let scale = positive("VSUPPLY", plan.supply_v)?
            / positive("VSUPPLY_NOMINAL", model.nominal_supply_v)?;
        positive("VSUPPLY scale", scale)?;
        let voltage_scale = if matches!(family.base_name(), "HC" | "UNIV") {
            scale
        } else {
            1.0
        };
        if plan.physical_pins.is_empty() {
            bail!("native bridge {index} has no connected physical pins");
        }
        if kind == AnalogBridgeKind::Dac && plan.physical_pins.len() != 1 {
            bail!("DAC {native_name} cannot combine independent physical drivers");
        }
        let mut common: Option<NativeDigitalBundle> = None;
        let mut out_res = None;
        for pin_id in &plan.physical_pins {
            if !consumed_pins.insert(pin_id.clone()) {
                bail!("physical pin {pin_id} occurs in multiple bridge groups");
            }
            let wire = wires.get(pin_id.as_str()).with_context(|| {
                format!("missing explicit digital bundle wiring for pin {pin_id}")
            })?;
            if kind == AnalogBridgeKind::Adc && wire.out_res_ohm.is_some() {
                bail!("ADC pin {pin_id} must not carry a DAC output resistance");
            }
            let bundle = wire.bundle.validated()?;
            if common.as_ref().is_some_and(|old| old != &bundle) {
                bail!(
                    "shared ADC {native_name} requires identical digital bundle for each physical input"
                );
            }
            common = Some(bundle);
            out_res = wire.out_res_ohm;
        }
        let bundle = common.context("empty bridge group")?;
        // Every output triple belongs to exactly one bridge group. The
        // input pins inside a *single* ADC group share that exact triple.
        for name in bundle.names() {
            if analog_nets.contains(name) || !used_nodes.insert(name.to_owned()) {
                bail!("physical family digital node {name} overlaps another bridge or analog net");
            }
        }
        let generated = format!("MsrFamily{index}");
        emit_native_model(
            &mut text,
            &generated,
            &model.native,
            scale,
            voltage_scale,
            out_res,
        )?;
        let analog = analog_node(&plan.analog_net)?;
        match kind {
            AnalogBridgeKind::Adc => writeln!(
                text,
                "AsrFamily{index} %gd({analog} 0) {} {} {} {generated}",
                bundle.state, bundle.sink_meta, bundle.source_meta
            )?,
            AnalogBridgeKind::Dac => writeln!(
                text,
                "AsrFamily{index} {} {} {} %gd({analog} 0) {generated}",
                bundle.state, bundle.sink_meta, bundle.source_meta
            )?,
        }
    }
    if consumed_pins.len() != wires.len() {
        bail!("native family specification contains unused physical pin wiring");
    }
    Ok(text)
}
