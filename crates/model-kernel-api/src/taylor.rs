//! Physical Taylor coefficients in normalized local time. No integration,
//! history or numerical trial limiting crosses this interface.
use crate::{
    Artifact, Capability, Descriptor, EquationKind, Error, Kernel, Point, Prepared, Result,
    allocate,
};

pub mod arithmetic;

/// Maximum degree of a normalized local germ.
pub const MAX_ORDER: usize = 64;
/// Bound the total number of input/output scalars before allocation.
pub const MAX_SCALARS: usize = 1 << 20;

/// The exact Taylor capability is part of the immutable descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaylorCapability {
    Unavailable,
    Computed { max_order: usize },
}

impl TaylorCapability {
    pub(super) fn validate(self, width: usize, charge: Capability) -> Result<()> {
        if let Self::Computed { max_order } = self {
            if charge == Capability::Unavailable {
                return Err(Error::Schema("Taylor storage unavailable"));
            }
            Shape::new(width, max_order)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    width: usize,
    square: usize,
    rows: usize,
    matrices: usize,
}

impl Shape {
    fn new(width: usize, order: usize) -> Result<Self> {
        if width == 0 || order > MAX_ORDER {
            return Err(Error::Shape("Taylor width or order"));
        }
        let square = width
            .checked_mul(width)
            .ok_or(Error::Shape("Taylor matrix size"))?;
        let rows = width
            .checked_mul(order + 1)
            .ok_or(Error::Shape("Taylor vector size"))?;
        let matrices = square
            .checked_mul(order + 1)
            .ok_or(Error::Shape("Taylor matrix size"))?;
        let scalars = rows
            .checked_mul(3)
            .and_then(|n| matrices.checked_mul(2).and_then(|m| n.checked_add(m)))
            .ok_or(Error::Shape("Taylor buffer size"))?;
        if scalars > MAX_SCALARS {
            return Err(Error::Shape("Taylor buffer scalar limit"));
        }
        Ok(Self {
            width,
            square,
            rows,
            matrices,
        })
    }
}

/// Borrowed normalized state germ, coefficient-major then coordinate order.
/// Coefficient k is the k-th time derivative times scale^k / k!.
/// At a join use the right-hand germ, with the exact coefficient-zero point.
#[derive(Clone, Copy, Debug)]
pub struct TaylorInput<'a> {
    pub order: usize,
    pub coefficients: &'a [f64],
}

impl TaylorInput<'_> {
    /// Extract one physical coordinate, without allocating.
    ///
    /// # Errors
    /// Mismatched dimensions, invalid index, order or coefficient.
    pub fn coordinate(self, width: usize, index: usize) -> Result<arithmetic::Series> {
        let shape = Shape::new(width, self.order)?;
        if self.coefficients.len() != shape.rows || index >= width {
            return Err(Error::Shape("Taylor input coordinates"));
        }
        arithmetic::Series::from_coefficients(
            self.coefficients.iter().skip(index).step_by(width).copied(),
        )
    }
}

/// Coefficient-major residuals and row-major Jacobian matrices at each degree.
pub struct TaylorPoint<'a> {
    pub current: &'a mut [f64],
    pub conductance: &'a mut [f64],
    pub charge: &'a mut [f64],
    pub capacitance: &'a mut [f64],
}

impl TaylorPoint<'_> {
    fn check(&self, shape: Shape) -> Result<()> {
        if self.current.len() != shape.rows
            || self.charge.len() != shape.rows
            || self.conductance.len() != shape.matrices
            || self.capacitance.len() != shape.matrices
        {
            return Err(Error::Shape("Taylor output dimensions"));
        }
        Ok(())
    }

    fn point(&mut self, shape: Shape, degree: usize) -> Point<'_> {
        let row = degree * shape.width;
        let matrix = degree * shape.square;
        Point {
            current: &mut self.current[row..row + shape.width],
            charge: &mut self.charge[row..row + shape.width],
            conductance: &mut self.conductance[matrix..matrix + shape.square],
            capacitance: &mut self.capacitance[matrix..matrix + shape.square],
        }
    }
}

/// Reusable coefficient arrays allocated by the caller outside evaluation.
#[derive(Debug)]
pub struct TaylorBuffer {
    current: Vec<f64>,
    conductance: Vec<f64>,
    charge: Vec<f64>,
    capacitance: Vec<f64>,
}

impl TaylorBuffer {
    /// # Errors
    /// Invalid dimensions/order, excessive buffer size, or allocation failure.
    pub fn new(width: usize, order: usize) -> Result<Self> {
        let shape = Shape::new(width, order)?;
        Ok(Self {
            current: allocate(shape.rows)?,
            charge: allocate(shape.rows)?,
            conductance: allocate(shape.matrices)?,
            capacitance: allocate(shape.matrices)?,
        })
    }

    pub fn point(&mut self) -> TaylorPoint<'_> {
        TaylorPoint {
            current: &mut self.current,
            conductance: &mut self.conductance,
            charge: &mut self.charge,
            capacitance: &mut self.capacitance,
        }
    }
}

impl<K: Kernel> Artifact<K> {
    /// Evaluate F, Q and both physical Jacobian series, without mutating the germ.
    ///
    /// # Errors
    /// Foreign instance, unsupported capability/order, shape, domain or conservation failure.
    /// Failed output is not valid for consumption.
    pub fn evaluate_taylor(
        &self,
        instance: &Prepared<K>,
        input: TaylorInput<'_>,
        out: &mut TaylorPoint<'_>,
    ) -> Result<()> {
        self.check(instance)?;
        let descriptor = self.descriptor();
        let TaylorCapability::Computed { max_order } = descriptor.taylor else {
            return Err(Error::Unsupported("physical Taylor evaluation"));
        };
        if input.order > max_order {
            return Err(Error::Unsupported("physical Taylor order"));
        }
        let shape = Shape::new(descriptor.equations.len(), input.order)?;
        if input.coefficients.len() != shape.rows {
            return Err(Error::Shape("Taylor input dimensions"));
        }
        if input.coefficients.iter().any(|v| !v.is_finite())
            || descriptor
                .equations
                .iter()
                .zip(input.coefficients)
                .any(|(eq, value)| eq.kind == EquationKind::Thermal && *value <= 0.)
        {
            return Err(Error::Domain("invalid physical Taylor germ".into()));
        }
        out.check(shape)?;
        for degree in 0..=input.order {
            out.point(shape, degree).reset(descriptor.charge);
        }
        self.kernel.evaluate_taylor(&instance.data, input, out)?;
        for degree in 0..=input.order {
            out.point(shape, degree).validate(descriptor)?;
        }
        Ok(())
    }
}
