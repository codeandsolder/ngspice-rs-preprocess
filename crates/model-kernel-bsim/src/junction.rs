//! BSIM1/2 junction current and depletion charge, including the M=1 limit.
use model_compact_intrinsic::{
    BSIM1_REFERENCE_THERMAL_VOLTAGE, LegacyBsimJunction, LegacyBsimJunctionLaw,
};
use model_kernel_api::{Error, Result};

pub fn current(saturation: f64, voltage: f64, gmin: f64) -> Result<(f64, f64)> {
    let vt = BSIM1_REFERENCE_THERMAL_VOLTAGE;
    let (value, slope) = if voltage <= 0. {
        let slope = saturation / vt + gmin;
        (slope * voltage, slope)
    } else {
        let normalized = voltage / vt;
        // Large forward voltages can overflow exp before scaling by Is.
        let injection = if normalized < 1. {
            saturation * normalized.exp_m1()
        } else {
            (saturation.ln() + normalized).exp() - saturation
        };
        let slope = if normalized < 1. {
            saturation * normalized.exp() / vt
        } else {
            (saturation.ln() - vt.ln() + normalized).exp()
        };
        (gmin.mul_add(voltage, injection), slope + gmin)
    };
    if value.is_finite() && slope.is_finite() {
        Ok((value, slope))
    } else {
        Err(Error::Domain(
            "nonfinite legacy BSIM junction current".into(),
        ))
    }
}

fn depletion(capacitance: f64, potential: f64, grading: f64, voltage: f64) -> (f64, f64) {
    if capacitance == 0. {
        return (0., 0.);
    }
    if voltage >= 0. {
        let slope = grading * (voltage / potential);
        (
            capacitance * voltage * 0.5f64.mul_add(slope, 1.),
            capacitance * (1. + slope),
        )
    } else {
        let log = (-voltage / potential).ln_1p();
        let power = 1. - grading;
        let integral = if power == 0. {
            log
        } else {
            (power * log).exp_m1() / power
        };
        (
            -potential * capacitance * integral,
            capacitance * (-grading * log).exp(),
        )
    }
}

pub fn charge(
    junction: &LegacyBsimJunction,
    law: &LegacyBsimJunctionLaw,
    voltage: f64,
) -> Result<(f64, f64)> {
    let bottom = depletion(
        junction.bottom_capacitance,
        law.bottom_potential,
        law.bottom_grading,
        voltage,
    );
    let side = depletion(
        junction.sidewall_capacitance,
        law.sidewall_potential,
        law.sidewall_grading,
        voltage,
    );
    let value = bottom.0 + side.0;
    let slope = bottom.1 + side.1;
    if value.is_finite() && slope.is_finite() && slope >= 0. {
        Ok((value, slope))
    } else {
        Err(Error::Domain(
            "nonphysical legacy BSIM junction charge".into(),
        ))
    }
}
