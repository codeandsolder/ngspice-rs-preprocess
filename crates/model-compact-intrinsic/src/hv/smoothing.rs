//! Source-defined `HiSIM` smooth clipping laws with cancellation-resistant values
//! and complete derivatives, including bias-dependent limits.
use crate::differential::Differential as Jet;

pub(super) fn positive<const N: usize>(value: Jet<N>, delta: f64) -> Jet<N> {
    let root = value.value.hypot(2. * delta);
    let result = if value.value >= 0. {
        f64::midpoint(value.value, root)
    } else {
        2. * delta * delta / (root - value.value)
    };
    value.map(result, result / root)
}

fn positive_quadratic<const N: usize>(argument: Jet<N>, product: Jet<N>) -> Jet<N> {
    let product = if product.value < 0. {
        -product
    } else {
        product
    };
    let root = argument.value.hypot(product.value.sqrt());
    if root == 0. {
        return 0.5 * argument;
    }
    let value = if argument.value >= 0. {
        f64::midpoint(argument.value, root)
    } else {
        0.5 * (product.value / root) / (1. - argument.value / root)
    };
    Jet {
        value,
        gradient: std::array::from_fn(|axis| {
            (value / root).mul_add(
                argument.gradient[axis],
                0.25 * product.gradient[axis] / root,
            )
        }),
    }
}

/// Native `Fn_SU` with the absolute-valued smoothing product.
pub(super) fn upper<const N: usize>(value: Jet<N>, limit: Jet<N>, delta: f64) -> Jet<N> {
    limit - positive_quadratic(limit - value - delta, 4. * delta * limit)
}

/// Native `Fn_SL`, with differentiation of the actual limit.
pub(super) fn lower<const N: usize>(value: Jet<N>, limit: Jet<N>, delta: f64) -> Jet<N> {
    -upper(-value, -limit, delta)
}

// Native Fn_CP with xmax=1 and pw=4. Factor by the larger term so
// the eighth power never overflows at a large inversion-voltage ratio.
pub(super) fn unit_ceiling_power4<const N: usize>(x: Jet<N>) -> Jet<N> {
    if x.value.abs() <= 1. {
        let square = x * x;
        let fourth = square * square;
        x / (1. + fourth * fourth).sqrt().sqrt().sqrt()
    } else {
        let reciprocal = 1. / x;
        let square = reciprocal * reciprocal;
        let fourth = square * square;
        x.value.signum() / (1. + fourth * fourth).sqrt().sqrt().sqrt()
    }
}
