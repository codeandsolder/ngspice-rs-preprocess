//! HiSIM-HV1/HV2 electrothermal power coupling from ngspice 47 `hsmhv[2]ld.c`.
//!
//! This uses the *real* drain-current value and analytic current derivatives
//! supplied by the eventual compact-model kernel. It never synthesizes a MOS
//! drain current or substitutes a square-law approximation.

use super::PreparedHiSimHvTransport;
use crate::ModelError;

/// Six independent drain-current partials in native HiSIM-HV's intrinsic and
/// external branch-voltage coordinates. The owning current or power value
/// determines whether these entries are A/V or W/V.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HiSimHvElectricalGradient {
    /// Derivative with respect to internal `vds`.
    pub inner_vds: f64,
    /// Derivative with respect to internal `vgs`.
    pub inner_vgs: f64,
    /// Derivative with respect to internal `vbs`.
    pub inner_vbs: f64,
    /// Derivative with respect to external `vdse`.
    pub outer_vds: f64,
    /// Derivative with respect to external `vgse`.
    pub outer_vgs: f64,
    /// Derivative with respect to external `vbse`.
    pub outer_vbs: f64,
}

impl HiSimHvElectricalGradient {
    const fn all_finite(self) -> bool {
        self.inner_vds.is_finite()
            && self.inner_vgs.is_finite()
            && self.inner_vbs.is_finite()
            && self.outer_vds.is_finite()
            && self.outer_vgs.is_finite()
            && self.outer_vbs.is_finite()
    }
}

/// Intrinsic and external drain-source voltages in HiSIM-HV C coordinates.
///
/// For PMOS, these must already be transformed into the model's intrinsic
/// voltage convention, just as in ngspice's own nonlinear evaluation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDrainBias {
    pub inner_vds_v: f64,
    pub outer_vds_v: f64,
}

/// **Externally provided physical model kernel result**, before thermal power
///
/// limiting. The current and all derivatives are with respect to the exact
/// same internal/external electrical states and local temperature.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvDrainCurrent {
    pub current_a: f64,
    pub electrical_derivative_a_per_v: HiSimHvElectricalGradient,
    pub temperature_derivative_a_per_kelvin: f64,
}

/// Seven nonzero columns of the native electrothermal DC Jacobian thermal row.
///
/// The six electrical entries include native `model->type` (NMOS +1 / PMOS -1).
/// The thermal diagonal never gets a polarity multiplier. Source conservation
/// requires the sum of all six electrical columns to equal zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvThermalJacobianRow {
    pub drain_external_w_per_v: f64,
    pub drain_internal_w_per_v: f64,
    pub gate_internal_w_per_v: f64,
    pub source_external_w_per_v: f64,
    pub source_internal_w_per_v: f64,
    pub body_internal_w_per_v: f64,
    pub temperature_w_per_kelvin: f64,
}

/// Completed nonlinear *power-coupling* terms for one electrothermal state.
///
/// This is a native load/equation component, not a substitute for the MOS I/Q
/// current or charge kernels. The charge derivative is the prepared `Cth`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HiSimHvGeneratedHeat {
    /// Source's `Veffpower` choice among internal, external or interpolated Vds.
    pub effective_drain_voltage_v: f64,
    /// Unbounded `Ids * Veffpower` before source `SHEMAX` smoothing.
    pub raw_generated_power_w: f64,
    /// `Fn_SU(raw_generated_power, SHEMAX * Gth, 0.1 * Gth)`.
    pub generated_power_w: f64,
    /// Source `Fn_SU` derivative `T0` between zero and one for positive ceiling/delta.
    pub limit_derivative: f64,
    /// All six source `dP/dV*` partials, after temperature-rise limiting.
    pub generated_power_electrical_derivative_w_per_v: HiSimHvElectricalGradient,
    /// Source `dP/dT`, after the same limiter derivative.
    pub generated_power_temperature_derivative_w_per_kelvin: f64,
    /// `Ith - P`, **before** the transient `dQth/dt` term.
    pub thermal_dc_residual_w: f64,
    /// Native `ydc_t` electrical and thermal entries, before the integration
    /// method adds the `Cth` diagonal.
    pub thermal_dc_jacobian: HiSimHvThermalJacobianRow,
}

const fn finite(value: f64) -> Result<f64, ModelError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV electrothermal power must be finite",
        ))
    }
}

/// Couple physical HiSIM-HV drain current into the native thermal equation.
///
/// Requires the *real* MOS drain current and analytic partials, not an
/// approximation. Returns `None` if `COSELFHEAT` disabled the thermal node.
///
/// The outer/inner Vds partition follows ngspice's *sign-sensitive* test
/// `vds*(vdse-vds)>=0`. A binned `POWRAT==1` bypasses the temperature-smoothed
/// `POWRATIO` and uses external Vds exactly. The native `Fn_SU` then limits
/// generated power to the model's `SHEMAX` rise using `Gth` and a fixed 0.1 K
/// smoothing width. Its derivative multiplies every electrical and thermal
/// power derivative; passive conductive heat and charge are never limited.
///
/// # Errors
/// Rejects nonfinite current, bias and derivatives or an unrepresentable
/// source power/ceiling/limiter intermediate, without fabricating fallback
/// coefficients. The current is supplied by the nonlinear compact-model
/// kernel and must use the same branch coordinate convention.
pub fn evaluate_hisim_hv_generated_heat(
    transport: &PreparedHiSimHvTransport,
    bias: HiSimHvDrainBias,
    current: HiSimHvDrainCurrent,
) -> Result<Option<HiSimHvGeneratedHeat>, ModelError> {
    let Some(thermal) = transport.thermal_branch else {
        return Ok(None);
    };
    let inner = finite(bias.inner_vds_v)?;
    let outer = finite(bias.outer_vds_v)?;
    let ids = finite(current.current_a)?;
    let d_ids_dt = finite(current.temperature_derivative_a_per_kelvin)?;
    if !current.electrical_derivative_a_per_v.all_finite() {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV electrothermal power must be finite",
        ));
    }

    let difference = finite(outer - inner)?;
    let sign_product = finite(inner * difference)?;
    let (effective, d_effective_inner, d_effective_outer) = if sign_product < 0.0 {
        (inner, 1.0, 0.0)
    } else if transport.exact_external_drain_power_voltage {
        (outer, 0.0, 1.0)
    } else {
        let external_weight = transport.power_coupling_ratio;
        (
            finite(external_weight.mul_add(difference, inner))?,
            1.0 - external_weight,
            external_weight,
        )
    };
    let raw_power = finite(ids * effective)?;
    let ceiling =
        finite(transport.maximum_self_heating_rise_kelvin * thermal.conductance_w_per_kelvin)?;
    let delta = finite(0.1 * thermal.conductance_w_per_kelvin)?;
    let arg = finite(ceiling - raw_power - delta)?;
    let smoothing = finite((4.0 * ceiling * delta).abs())?;
    let radical = finite(arg.mul_add(arg, smoothing).sqrt())?;
    if radical <= 0.0 {
        return Err(ModelError::InvalidPhysicalParameter(
            "HiSIM-HV power ceiling has singular derivative",
        ));
    }
    let limit_derivative = finite(f64::midpoint(1.0, arg / radical))?;
    let power = finite((-0.5f64).mul_add(arg + radical, ceiling))?;
    let grad = current.electrical_derivative_a_per_v;
    let partials = HiSimHvElectricalGradient {
        inner_vds: finite(
            limit_derivative * ids.mul_add(d_effective_inner, grad.inner_vds * effective),
        )?,
        inner_vgs: finite(limit_derivative * grad.inner_vgs * effective)?,
        inner_vbs: finite(limit_derivative * grad.inner_vbs * effective)?,
        outer_vds: finite(
            limit_derivative * ids.mul_add(d_effective_outer, grad.outer_vds * effective),
        )?,
        outer_vgs: finite(limit_derivative * grad.outer_vgs * effective)?,
        outer_vbs: finite(limit_derivative * grad.outer_vbs * effective)?,
    };
    let d_power_dt = finite(limit_derivative * d_ids_dt * effective)?;
    let polarity = transport.polarity_sign;
    let row = HiSimHvThermalJacobianRow {
        drain_external_w_per_v: finite(-polarity * partials.outer_vds)?,
        drain_internal_w_per_v: finite(-polarity * partials.inner_vds)?,
        gate_internal_w_per_v: finite(-polarity * (partials.inner_vgs + partials.outer_vgs))?,
        source_external_w_per_v: finite(
            polarity * (partials.outer_vds + partials.outer_vgs + partials.outer_vbs),
        )?,
        source_internal_w_per_v: finite(
            polarity * (partials.inner_vds + partials.inner_vgs + partials.inner_vbs),
        )?,
        body_internal_w_per_v: finite(-polarity * (partials.inner_vbs + partials.outer_vbs))?,
        temperature_w_per_kelvin: finite(thermal.conductance_w_per_kelvin - d_power_dt)?,
    };
    Ok(Some(HiSimHvGeneratedHeat {
        effective_drain_voltage_v: effective,
        raw_generated_power_w: raw_power,
        generated_power_w: power,
        limit_derivative,
        generated_power_electrical_derivative_w_per_v: partials,
        generated_power_temperature_derivative_w_per_kelvin: d_power_dt,
        thermal_dc_residual_w: finite(thermal.conductive_heat_w - power)?,
        thermal_dc_jacobian: row,
    }))
}
