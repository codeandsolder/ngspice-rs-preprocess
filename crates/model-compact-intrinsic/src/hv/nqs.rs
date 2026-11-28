//! HiSIM-HV non-quasistatic channel charge relaxation, independent of solver history.
use super::{
    HiSimHvChannelRegion, HiSimHvIntrinsicScalar as Jet, HiSimHvNormalChannelPoint,
    PreparedHiSimHvModel, instance::model_real, lookup_slot,
};
use crate::{ModelError, ModelVariant, ParameterValue, differential::Differential};

/// Local electrical and thermal coordinates followed by inversion and bulk
/// charge-state coordinates (Coulombs): [Vgs,Vds,Vbs,T,Qi,Qb].
pub type HiSimHvNqsScalar = Differential<6>;
type StateJet = HiSimHvNqsScalar;

/// Physical, signed charge targets and delay constants with local Jacobians.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNqsEquilibrium {
    pub inversion_charge_c: Jet,
    pub bulk_charge_c: Jet,
    pub drain_fraction: Jet,
    pub inversion_delay_s: Jet,
    pub bulk_delay_s: Jet,
}

/// Independent signed solver-owned charge coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNqsState {
    pub inversion_charge_c: f64,
    pub bulk_charge_c: f64,
}

/// Generalized equation rows and dynamic terminal charge contributions.
///
/// For each state row, solve residual + d(charge)/dt = 0. No history,
/// timestep or integration rule is stored in this constitutive kernel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvNqsPoint {
    pub inversion_residual_a: StateJet,
    pub bulk_residual_a: StateJet,
    pub inversion_state_c: StateJet,
    pub bulk_state_c: StateJet,
    pub gate_charge_c: StateJet,
    pub drain_charge_c: StateJet,
    pub source_charge_c: StateJet,
    pub body_charge_c: StateJet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Nqs {
    enabled: bool,
    inversion_limit: f64,
    transport_factor: f64,
    bulk_factor: f64,
    denominator_floor: f64,
}

fn positive_delay(value: Jet) -> Result<Jet, ModelError> {
    let value = value.checked("HiSIM-HV NQS delay")?;
    // Native load stage floors both state relaxation times.
    Ok(if value.value < 1e-18 {
        Jet::constant(1e-18)
    } else {
        value
    })
}
impl Nqs {
    pub(super) fn prepare(model: &PreparedHiSimHvModel) -> Result<Self, ModelError> {
        let selector = lookup_slot(model.variant(), "conqs")?;
        let enabled = match model.get_slot(selector) {
            Some(ParameterValue::Integer(value)) => *value != 0,
            _ => return Err(ModelError::MissingDefaultDependency(selector)),
        };
        Ok(Self {
            enabled,
            inversion_limit: model_real(model, "dly1")?,
            transport_factor: model_real(model, "dly2")?,
            bulk_factor: model_real(model, "dly3")? / 1e4,
            denominator_floor: if model.variant() == ModelVariant::HiSimHv1 {
                1e-50
            } else {
                1e-25
            },
        })
    }

    pub(super) fn equilibrium(
        self,
        state: &HiSimHvNormalChannelPoint,
    ) -> Result<Option<HiSimHvNqsEquilibrium>, ModelError> {
        if !self.enabled {
            return Ok(None);
        }
        let electro = &state.electrostatics;
        let charge = &electro.charge;
        let delay = if matches!(
            charge.region,
            HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
        ) {
            let length = electro.modulation.effective_length_m;
            let transport = self.transport_factor * length * length;
            self.inversion_limit * transport
                / (self.inversion_limit
                    * state.mobility.mobility_m2_per_vs
                    * charge.inversion_voltage
                    + transport
                    + self.denominator_floor)
        } else {
            Jet::constant(self.inversion_limit)
        };
        Ok(Some(HiSimHvNqsEquilibrium {
            inversion_charge_c: electro.terminal_charges.drain + electro.terminal_charges.source,
            bulk_charge_c: electro.terminal_charges.bulk,
            drain_fraction: charge.drain_fraction,
            inversion_delay_s: positive_delay(delay)?,
            bulk_delay_s: positive_delay(self.bulk_factor * electro.oxide.capacitance)?,
        }))
    }
}
const fn promote(value: Jet) -> StateJet {
    let [g, d, b, t] = value.gradient;
    StateJet {
        value: value.value,
        gradient: [g, d, b, t, 0., 0.],
    }
}

impl HiSimHvNqsEquilibrium {
    /// Evaluate native charge-relaxation equations at a solver trial state.
    /// # Errors
    /// Invalid or nonfinite local state, delay or Jacobian.
    pub fn evaluate(self, state: HiSimHvNqsState) -> Result<HiSimHvNqsPoint, ModelError> {
        let inversion = StateJet::coordinate(state.inversion_charge_c, 4, 1.)
            .checked("HiSIM-HV NQS inversion state")?;
        let bulk =
            StateJet::coordinate(state.bulk_charge_c, 5, 1.).checked("HiSIM-HV NQS bulk state")?;
        let fraction = promote(self.drain_fraction);
        let drain = inversion * fraction;
        Ok(HiSimHvNqsPoint {
            inversion_residual_a: ((inversion - promote(self.inversion_charge_c))
                / promote(self.inversion_delay_s))
            .checked("HiSIM-HV NQS inversion residual")?,
            bulk_residual_a: ((bulk - promote(self.bulk_charge_c)) / promote(self.bulk_delay_s))
                .checked("HiSIM-HV NQS bulk residual")?,
            inversion_state_c: inversion,
            bulk_state_c: bulk,
            gate_charge_c: (-inversion - bulk).checked("HiSIM-HV NQS gate charge")?,
            drain_charge_c: drain.checked("HiSIM-HV NQS drain charge")?,
            source_charge_c: (inversion - drain).checked("HiSIM-HV NQS source charge")?,
            body_charge_c: bulk,
        })
    }
}
