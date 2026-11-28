//! Source-defined `HiSIM` smooth clipping laws with cancellation-resistant values
//! and complete derivatives, including bias-dependent limits.
use super::HiSimHvIntrinsicScalar as Jet;

pub(super) fn positive(value: Jet, delta: f64) -> Jet {
    let root = value.value.hypot(2. * delta);
    let result = if value.value >= 0. {
        f64::midpoint(value.value, root)
    } else {
        2. * delta * delta / (root - value.value)
    };
    value.map(result, result / root)
}

fn positive_quadratic(argument: Jet, product: Jet) -> Jet {
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
pub(super) fn upper(value: Jet, limit: Jet, delta: f64) -> Jet {
    limit - positive_quadratic(limit - value - delta, 4. * delta * limit)
}

/// Native `Fn_SL`, with differentiation of the actual limit.
pub(super) fn lower(value: Jet, limit: Jet, delta: f64) -> Jet {
    -upper(-value, -limit, delta)
}
