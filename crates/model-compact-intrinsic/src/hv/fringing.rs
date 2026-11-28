//! Source-faithful fringing gate/source and gate/drain charge from both HiSIM-HV
//! original temperature-preparation and nonlinear charge evaluation sources.
//!
//! The oxide permittivity and pi are the **historical C constants** in
//! `hsmhv[2]evalenv.h`, not a modern-material-parameter substitution.

use std::sync::OnceLock;

use super::{
    AdditiveChargeSlots, PreparedHiSimHvInstance, PreparedHiSimHvModel,
    compile_additive_charge_slots, compiled_hv_by_family, instance_real_at, source_real_at,
};
use crate::{ModelError, ParameterValue};

const OXIDE_PERMITTIVITY_F_PER_M: f64 = 3.453_133e-11;
const SOURCE_PI: f64 = std::f64::consts::PI;

/// Device-level fringing coefficient, independent of trial voltages.
///
/// `raw_capacitance_f` is the upstream `here->cfrng` before multiplier `M`;
/// `effective_capacitance_f` includes `M` and the upstream `COADOV==1`
/// output-gating condition. Both physical source/drain fringe branches use
/// the *same* capacitance in these HiSIM-HV versions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedHiSimHvFringing {
    pub raw_capacitance_f: f64,
    pub effective_capacitance_f: f64,
    pub output_enabled: bool,
}

/// Upstream `Qfd`/`Qfs` (with instance multiplicity), oriented from the gate
/// to external drain/source respectively. Native drain-prime/source-prime
/// charge rows receive their **negative**, via `-Qfd` and `-Qfs`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvFringingCharges {
    pub gate_drain_charge_c: f64,
    pub gate_source_charge_c: f64,
}

impl PreparedHiSimHvFringing {
    /// Source native `Qfd = Cfd*(Vgse-Vdse)` and `Qfs = Cfs*Vgse`.
    ///
    /// The derivatives of *drain-prime* charge are
    /// `d(-Qfd)/dVdse = +C` and `d(-Qfd)/dVgse = -C`;
    /// source-prime is `d(-Qfs)/dVgse = -C`. Both thermal slopes vanish.
    ///
    /// # Errors
    /// Trial voltages and resulting charges must be finite.
    pub fn evaluate(
        self,
        external_gate_source_v: f64,
        external_drain_source_v: f64,
    ) -> Result<HiSimHvFringingCharges, ModelError> {
        if !external_gate_source_v.is_finite() || !external_drain_source_v.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV fringing terminal voltage",
            ));
        }
        let source = self.effective_capacitance_f * external_gate_source_v;
        let drain =
            self.effective_capacitance_f * (external_gate_source_v - external_drain_source_v);
        if !source.is_finite() || !drain.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV fringing charge",
            ));
        }
        Ok(HiSimHvFringingCharges {
            gate_drain_charge_c: drain,
            gate_source_charge_c: source,
        })
    }
}

static HV1_FRINGING_SLOTS: OnceLock<Result<AdditiveChargeSlots, ModelError>> = OnceLock::new();
static HV2_FRINGING_SLOTS: OnceLock<Result<AdditiveChargeSlots, ModelError>> = OnceLock::new();

/// Prepare the physical fringing capacitance once for the stable model and
/// instance geometry, before evaluating any Newton/trial voltages.
///
/// Both HV families use the exact original `C_EOX/(C_Pi/2) * Weff*NF *
/// log(1+TPOLY/TOX)`, preserving historical source constants and gating
/// output charges with original model `COADOV==1` after applying instance `M`.
///
/// # Errors
/// Rejects incompatible variants and undefined/nonfinite source expressions.
pub fn prepare_hisim_hv_fringing(
    model: &PreparedHiSimHvModel,
    instance: &PreparedHiSimHvInstance,
) -> Result<PreparedHiSimHvFringing, ModelError> {
    model.validate_instance(instance)?;
    let slots = compiled_hv_by_family(
        model.variant(),
        &HV1_FRINGING_SLOTS,
        &HV2_FRINGING_SLOTS,
        |variant| compile_additive_charge_slots(variant, "tox", "tpoly"),
    )?;
    let tox = source_real_at(model, slots.model_first)?;
    let tpoly = source_real_at(model, slots.model_second)?;
    if !tox.is_normal() || !tpoly.is_finite() || 1.0 + tpoly / tox <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV fringing oxide/poly thickness",
        ));
    }
    let raw = (OXIDE_PERMITTIVITY_F_PER_M / (SOURCE_PI / 2.0))
        * instance.effective_geometry().channel_width_total_m
        * (tpoly / tox).ln_1p();
    let output_enabled = matches!(
        model.get_slot(slots.coadov),
        Some(ParameterValue::Integer(1))
    );
    let multiplier = instance_real_at(instance, slots.multiplier)?;
    let effective = if output_enabled {
        raw * multiplier
    } else {
        0.0
    };
    if !raw.is_finite() || !effective.is_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV fringing capacitance",
        ));
    }
    Ok(PreparedHiSimHvFringing {
        raw_capacitance_f: raw,
        effective_capacitance_f: effective,
        output_enabled,
    })
}
