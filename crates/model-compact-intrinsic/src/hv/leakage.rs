//! Coupled gate and junction leakage using the solved channel state.
use super::{
    HiSimHvChannelRegion, HiSimHvEdgeTunnelingCurrent, HiSimHvGateChannelState,
    HiSimHvIntrinsicBias, HiSimHvIntrinsicScalar as Jet, HiSimHvNormalChannelPoint,
    PreparedHiSimHvEdgeTunneling, PreparedHiSimHvGateTunneling, PreparedHiSimHvGidl,
    PreparedHiSimHvInstance, PreparedHiSimHvModel, PreparedHiSimHvTemperature,
    instance::model_real, prepare_hisim_hv_edge_tunneling, prepare_hisim_hv_gate_tunneling,
    prepare_hisim_hv_gidl,
};
use crate::ModelError;

/// Normalized terminal leakage branches, including instance multiplicity.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvLeakagePoint {
    pub gate_to_drain: Jet,
    pub gate_to_source: Jet,
    pub gate_to_body: Jet,
    pub drain_to_body: Jet,
    pub source_to_body: Jet,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Leakage {
    gate: PreparedHiSimHvGateTunneling,
    edge: PreparedHiSimHvEdgeTunneling,
    gidl: PreparedHiSimHvGidl,
    partition: f64,
}

const fn edge_current(value: HiSimHvEdgeTunnelingCurrent) -> Jet {
    Jet {
        value: value.current_a,
        gradient: [
            value.gate_voltage_derivative_siemens,
            value.drain_voltage_derivative_siemens,
            0.,
            0.,
        ],
    }
}

impl Leakage {
    pub(super) fn prepare(
        model: &PreparedHiSimHvModel,
        instance: &PreparedHiSimHvInstance,
    ) -> Result<Self, ModelError> {
        let partition = model_real(model, "glpart1")?;
        if !partition.is_finite() {
            return Err(ModelError::InvalidPhysicalParameter(
                "HiSIM-HV gate partition",
            ));
        }
        Ok(Self {
            gate: prepare_hisim_hv_gate_tunneling(model, instance)?,
            edge: prepare_hisim_hv_edge_tunneling(model, instance)?,
            gidl: prepare_hisim_hv_gidl(model, instance)?,
            partition,
        })
    }

    pub(super) fn evaluate(
        &self,
        bias: HiSimHvIntrinsicBias,
        channel: &HiSimHvNormalChannelPoint,
        temperature: &PreparedHiSimHvTemperature,
    ) -> Result<HiSimHvLeakagePoint, ModelError> {
        let electro = &channel.electrostatics;
        let short_long_shift =
            electro.threshold.short_channel_shift + electro.threshold.pocket_shift;
        let state = HiSimHvGateChannelState {
            modified_gate: electro.conditioned.modified_gate,
            modified_drain: electro.conditioned.modified_drain,
            modified_body: electro.conditioned.modified_body,
            source_surface: electro.modified_source_surface,
            threshold_shift: electro.threshold.total_shift,
            poly_depletion: electro.poly.depletion,
            short_long_shift,
            lateral_field: channel.mobility.longitudinal_field,
            inversion_density: electro.charge.inversion_density,
            poisson_coefficient: electro.material.poisson_coefficient,
            zero_field_oxide_capacitance: electro.classical_oxide_capacitance,
            has_inversion: matches!(
                electro.charge.region,
                HiSimHvChannelRegion::Transition | HiSimHvChannelRegion::StrongInversion
            ),
        };
        let gate = self.gate.evaluate(bias, &state, temperature)?;
        let edge = self.edge.evaluate(bias.vgs, bias.vds)?;
        let gidl = self.gidl.evaluate(bias, short_long_shift, temperature)?;
        Ok(HiSimHvLeakagePoint {
            gate_to_drain: (self.partition * gate.inversion - edge_current(edge.gate_drain))
                .checked("HiSIM-HV gate-drain current")?,
            gate_to_source: ((1. - self.partition) * gate.inversion
                - edge_current(edge.gate_source))
            .checked("HiSIM-HV gate-source current")?,
            gate_to_body: (gate.fowler_nordheim - gate.direct_body)
                .checked("HiSIM-HV gate-body current")?,
            drain_to_body: gidl.drain_to_body,
            source_to_body: gidl.source_to_body,
        })
    }
}
