//! HICUM/L2 2.4.0 vertical NQS adjunct-gyrator state equations.
//!
//! Reproduces the three independent source auxiliary branches xf1, xf2,
//! xf and their dynamic charges Qxf1, Qxf2, Qxf. The simulator owns
//! state integration, auxiliary unknown allocation and AC/transient stamps.
//! No implicit Euler approximation, artificial transport delay or state
//! mutation occurs in this numerical source kernel.

use super::transport;
use crate::{EvaluationError, ModelError, validate_batch_lengths, validate_nonnegative};

const TWO_PI: f64 = 2.0 * std::f64::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub flnqs: bool,
    pub flcomp: f64,
    /// Source NQS current-delay coefficient.
    pub alit: f64,
    /// Source NQS stored-charge delay coefficient.
    pub alqf: f64,
    /// Temperature-prepared source T0; this scales all adjunct network
    /// residuals and dynamic charges, while Tf determines their ratios.
    pub t0: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    /// Source Vxf1, units A.
    pub current_first: f64,
    /// Source Vxf2, units A.
    pub current_second: f64,
    /// Source Vxf, units C (the Qdei replacement).
    pub stored_charge: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub transport: transport::Output,
    /// The three independent solution unknowns, not accepted-step state.
    pub state: State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Residual {
    /// Current residual (A), contributed on the adjunct state branch.
    pub current: transport::Value2,
    /// Partial derivative wrt the independent HICCR Tf value (A/s).
    /// Preserving this third derivative avoids silently freezing Tf when
    /// stamping a bias-dependent transit time.
    pub d_tf: f64,
    /// Partial derivatives with respect to the three auxiliary states.
    pub d_state: State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DynamicCharge {
    pub coulombs: f64,
    /// Derivatives of dynamic charge with respect to auxiliary unknowns
    /// (respectively C/A or C/C). The solver differentiates in time.
    pub d_state: State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    pub active: bool,
    /// Itxf replaces the forward transport current at (ci,ei).
    pub delayed_current: transport::Value2,
    pub delayed_current_d_state: State,
    /// Qdeix replaces Qdei in the intrinsic B-E diffusion charge.
    pub delayed_charge: transport::Value2,
    pub delayed_charge_d_state: State,
    pub first_current: Residual,
    pub second_current: Residual,
    pub charge_current: Residual,
    pub first_charge: DynamicCharge,
    pub second_charge: DynamicCharge,
    pub charge_filter_charge: DynamicCharge,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ComplexGain {
    pub real: f64,
    pub imaginary: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrequencyResponse {
    pub current: ComplexGain,
    pub charge: ComplexGain,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    p: Parameters,
}

impl Prepared {
    /// # Errors
    /// Returns invalid NQS coefficients rather than selecting a different
    /// delay approximation or silently enabling a legacy fallback.
    pub fn new(p: Parameters) -> Result<Self, ModelError> {
        // Published 2.4.0 defines FLcomp from [0:inf), with every
        // non-legacy value >=2.3 selecting the current formula.
        if !p.flcomp.is_finite() || p.flcomp < 0.0 {
            return Err(ModelError::InvalidParameter("flcomp"));
        }
        for (name, value) in [("alit", p.alit), ("alqf", p.alqf)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ModelError::InvalidParameter(name));
            }
        }
        validate_nonnegative("t0", p.t0)?;
        Ok(Self { p })
    }

    /// Source FLNQS=1 enables this for modern models. Legacy FLCOMP
    /// 0 and 2.1 enable vertical NQS even when FLNQS=0; FLCOMP 2.2
    /// and >=2.3 do not. Tf=0 or ALIT=ALQF=0 disables all auxiliaries.
    #[must_use = "use the result when allocating non-quasi-static state"]
    pub fn is_active(&self, tf: f64) -> bool {
        let legacy = self.p.flcomp == 0.0 || self.p.flcomp.to_bits() == 2.1f64.to_bits();
        (self.p.flnqs || legacy) && tf != 0.0 && (self.p.alit > 0.0 || self.p.alqf > 0.0)
    }

    /// Evaluate the exact source Ixf1/Ixf2/Ixf residuals, dynamic charges
    /// and delayed Itxf/Qdeix at the current *trial* auxiliary state.
    ///
    /// Derivatives wrt TF are exposed independently from intrinsic voltage
    /// derivatives so the solver can chain in the coupled, bias-dependent
    /// transit time rather than assuming TF is a constant.
    ///
    /// # Errors
    /// Reports nonfinite inputs or physical/numerical singularities.
    pub fn evaluate(&self, input: &Bias) -> Result<Output, EvaluationError> {
        let t = input.transport;
        let state = input.state;
        if ![
            t.forward_current.value,
            t.forward_current.d_vbiei,
            t.forward_current.d_vbici,
            t.forward_diffusion_charge.value,
            t.forward_diffusion_charge.d_vbiei,
            t.forward_diffusion_charge.d_vbici,
            t.forward_transit_time,
            state.current_first,
            state.current_second,
            state.stored_charge,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteBias);
        }
        // With T0=0, the published vertical NQS gyrators have
        // zero static and dynamic coefficients even when Tf is
        // generated from other delay parameters. That is an
        // underdetermined three-unknown network, not a valid
        // zero-delay approximation. Reject before Jacobian stamping.
        if self.is_active(t.forward_transit_time) && self.p.t0 == 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let output = if self.is_active(t.forward_transit_time) {
            self.active_network(&t, state)
        } else {
            Self::inactive_network(&t, state)
        };
        if !output.is_finite() {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(output)
    }

    fn active_network(&self, transport: &transport::Output, state: State) -> Output {
        let factor = self.p.t0 / transport.forward_transit_time;
        let first = (state.current_second - transport.forward_current.value) * factor;
        let second = (state.current_second - state.current_first) * factor;
        let charge = (state.stored_charge - transport.forward_diffusion_charge.value) * factor;
        let charge_first = self.p.alit * self.p.t0;
        let charge_second = charge_first / 3.0;
        let charge_filter = self.p.alqf * self.p.t0;
        Output {
            active: true,
            delayed_current: transport::Value2 {
                value: state.current_second,
                ..transport::Value2::default()
            },
            delayed_current_d_state: State {
                current_second: 1.0,
                ..State::default()
            },
            delayed_charge: transport::Value2 {
                value: state.stored_charge,
                ..transport::Value2::default()
            },
            delayed_charge_d_state: State {
                stored_charge: 1.0,
                ..State::default()
            },
            first_current: Residual {
                current: transport::Value2 {
                    value: first,
                    d_vbiei: -factor * transport.forward_current.d_vbiei,
                    d_vbici: -factor * transport.forward_current.d_vbici,
                },
                d_tf: -first / transport.forward_transit_time,
                d_state: State {
                    current_second: factor,
                    ..State::default()
                },
            },
            second_current: Residual {
                current: transport::Value2 {
                    value: second,
                    ..transport::Value2::default()
                },
                d_tf: -second / transport.forward_transit_time,
                d_state: State {
                    current_first: -factor,
                    current_second: factor,
                    ..State::default()
                },
            },
            charge_current: Residual {
                current: transport::Value2 {
                    value: charge,
                    d_vbiei: -factor * transport.forward_diffusion_charge.d_vbiei,
                    d_vbici: -factor * transport.forward_diffusion_charge.d_vbici,
                },
                d_tf: -charge / transport.forward_transit_time,
                d_state: State {
                    stored_charge: factor,
                    ..State::default()
                },
            },
            first_charge: DynamicCharge {
                coulombs: charge_first * state.current_first,
                d_state: State {
                    current_first: charge_first,
                    ..State::default()
                },
            },
            second_charge: DynamicCharge {
                coulombs: charge_second * state.current_second,
                d_state: State {
                    current_second: charge_second,
                    ..State::default()
                },
            },
            charge_filter_charge: DynamicCharge {
                coulombs: charge_filter * state.stored_charge,
                d_state: State {
                    stored_charge: charge_filter,
                    ..State::default()
                },
            },
        }
    }

    fn inactive_network(transport: &transport::Output, state: State) -> Output {
        Output {
            delayed_current: transport.forward_current,
            delayed_charge: transport.forward_diffusion_charge,
            first_current: Residual {
                current: transport::Value2 {
                    value: state.current_first,
                    ..transport::Value2::default()
                },
                d_state: State {
                    current_first: 1.0,
                    ..State::default()
                },
                ..Residual::default()
            },
            second_current: Residual {
                current: transport::Value2 {
                    value: state.current_second,
                    ..transport::Value2::default()
                },
                d_state: State {
                    current_second: 1.0,
                    ..State::default()
                },
                ..Residual::default()
            },
            charge_current: Residual {
                current: transport::Value2 {
                    value: state.stored_charge,
                    ..transport::Value2::default()
                },
                d_state: State {
                    stored_charge: 1.0,
                    ..State::default()
                },
                ..Residual::default()
            },
            ..Output::default()
        }
    }

    /// Solve the *linearized auxiliary network* analytically in the
    /// frequency domain. These are exact local ratios Itxf/Itf and
    /// Qdeix/Qdei at fixed operating-point TF, not nodal circuit gains.
    ///
    /// # Errors
    /// Reports invalid frequency/TF or an underdetermined active network
    /// when T0=0; source resistive/dynamic contributions then all vanish.
    pub fn frequency_response(
        &self,
        tf: f64,
        frequency_hz: f64,
    ) -> Result<FrequencyResponse, EvaluationError> {
        if !tf.is_finite() || !frequency_hz.is_finite() {
            return Err(EvaluationError::NonFiniteBias);
        }
        if frequency_hz < 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        if !self.is_active(tf) {
            return Ok(FrequencyResponse {
                current: ComplexGain {
                    real: 1.0,
                    imaginary: 0.0,
                },
                charge: ComplexGain {
                    real: 1.0,
                    imaginary: 0.0,
                },
            });
        }
        if self.p.t0 == 0.0 {
            return Err(EvaluationError::InvalidPhysicalDomain);
        }
        let omega = TWO_PI * frequency_hz;
        let normalized_current = omega * self.p.alit * tf;
        let normalized_charge = omega * self.p.alqf * tf;
        let current = reciprocal(
            (normalized_current * normalized_current).mul_add(-1.0 / 3.0, 1.0),
            normalized_current,
        );
        let charge = reciprocal(1.0, normalized_charge);
        if ![
            current.real,
            current.imaginary,
            charge.real,
            charge.imaginary,
        ]
        .iter()
        .all(|value| value.is_finite())
        {
            return Err(EvaluationError::NonFiniteResult);
        }
        Ok(FrequencyResponse { current, charge })
    }

    /// # Errors
    /// Reports incompatible buffers or invalid trial auxiliary state.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(input)?;
        }
        Ok(())
    }
}

fn reciprocal(real: f64, imaginary: f64) -> ComplexGain {
    let denominator = real.mul_add(real, imaginary * imaginary);
    ComplexGain {
        real: real / denominator,
        imaginary: -imaginary / denominator,
    }
}

impl Output {
    fn is_finite(self) -> bool {
        let values = [
            self.delayed_current.value,
            self.delayed_current.d_vbiei,
            self.delayed_current.d_vbici,
            self.delayed_charge.value,
            self.delayed_charge.d_vbiei,
            self.delayed_charge.d_vbici,
            self.first_current.current.value,
            self.first_current.current.d_vbiei,
            self.first_current.current.d_vbici,
            self.first_current.d_tf,
            self.second_current.current.value,
            self.second_current.d_tf,
            self.charge_current.current.value,
            self.charge_current.current.d_vbiei,
            self.charge_current.current.d_vbici,
            self.charge_current.d_tf,
            self.first_charge.coulombs,
            self.second_charge.coulombs,
            self.charge_filter_charge.coulombs,
        ];
        let derivatives = [
            self.delayed_current_d_state.current_second,
            self.delayed_charge_d_state.stored_charge,
            self.first_current.d_state.current_second,
            self.second_current.d_state.current_first,
            self.second_current.d_state.current_second,
            self.charge_current.d_state.stored_charge,
            self.first_charge.d_state.current_first,
            self.second_charge.d_state.current_second,
            self.charge_filter_charge.d_state.stored_charge,
        ];
        values
            .iter()
            .chain(derivatives.iter())
            .all(|value| value.is_finite())
    }
}
