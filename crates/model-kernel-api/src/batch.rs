//! Homogeneous reusable lane-major workspaces. The kernel can specialize the
//! scalar traversal with a generated or SIMD implementation.

use std::sync::Arc;

use crate::{Artifact, Capability, Descriptor, Error, Kernel, Point, Result, allocate};

/// Persistent contiguous output buffers for a homogeneous tile.
#[derive(Debug)]
pub struct BatchOutput {
    width: usize,
    lanes: usize,
    pub current: Vec<f64>,
    pub conductance: Vec<f64>,
    pub charge: Vec<f64>,
    pub capacitance: Vec<f64>,
}

impl BatchOutput {
    /// # Errors
    /// Invalid dimensions, overflow or allocation.
    pub fn new(width: usize, lanes: usize) -> Result<Self> {
        if width < 2 || lanes == 0 {
            return Err(Error::Shape("batch dimensions"));
        }
        let cells = width
            .checked_mul(lanes)
            .ok_or(Error::Shape("batch dimensions"))?;
        let entries = cells
            .checked_mul(width)
            .ok_or(Error::Shape("batch dimensions"))?;
        Ok(Self {
            width,
            lanes,
            current: allocate(cells)?,
            conductance: allocate(entries)?,
            charge: allocate(cells)?,
            capacitance: allocate(entries)?,
        })
    }

    #[must_use = "the lane count determines the batch shape"]
    pub const fn lanes(&self) -> usize {
        self.lanes
    }

    #[must_use = "the coordinate width determines the batch shape"]
    pub const fn width(&self) -> usize {
        self.width
    }

    /// # Errors
    /// Invalid lane or mutated buffer shape.
    pub fn point(&mut self, lane: usize) -> Result<Point<'_>> {
        if lane >= self.lanes {
            return Err(Error::Shape("batch lane"));
        }
        let row = lane * self.width..(lane + 1) * self.width;
        let jac = row.start * self.width..row.end * self.width;
        Ok(Point {
            current: self
                .current
                .get_mut(row.clone())
                .ok_or(Error::Shape("batch current"))?,
            charge: self
                .charge
                .get_mut(row)
                .ok_or(Error::Shape("batch charge"))?,
            conductance: self
                .conductance
                .get_mut(jac.clone())
                .ok_or(Error::Shape("batch Jacobian"))?,
            capacitance: self
                .capacitance
                .get_mut(jac)
                .ok_or(Error::Shape("batch storage Jacobian"))?,
        })
    }

    const fn validate_shape(&self) -> Result<()> {
        let cells = self.width * self.lanes;
        let entries = cells * self.width;
        if self.current.len() != cells
            || self.charge.len() != cells
            || self.conductance.len() != entries
            || self.capacitance.len() != entries
        {
            return Err(Error::Shape("batch buffer dimensions"));
        }
        Ok(())
    }

    fn reset(&mut self, capability: Capability) -> Result<()> {
        self.validate_shape()?;
        for lane in 0..self.lanes {
            self.point(lane)?.reset(capability);
        }
        Ok(())
    }

    fn validate(&mut self, descriptor: &Descriptor) -> Result<()> {
        self.validate_shape()?;
        for lane in 0..self.lanes {
            self.point(lane)?.validate(descriptor)?;
        }
        Ok(())
    }
}

/// Prepared data for homogeneous lanes stored contiguously, tied to one
/// immutable artifact. Create or replace tiles outside the numerical loop.
#[derive(Debug)]
pub struct PreparedTile<K: Kernel> {
    artifact: Arc<Artifact<K>>,
    data: Vec<K::Prepared>,
}

impl<K: Kernel> PreparedTile<K> {
    /// Prepare an independent candidate in every lane, preserving order.
    ///
    /// # Errors
    /// Invalid model inputs or allocation.
    pub fn new(artifact: &Arc<Artifact<K>>, candidates: &[(&[f64], f64)]) -> Result<Self> {
        if candidates.is_empty() {
            return Err(Error::Shape("empty prepared tile"));
        }
        let mut data = Vec::new();
        data.try_reserve_exact(candidates.len())
            .map_err(|_| Error::Allocation)?;
        for &(values, temperature) in candidates {
            data.push(artifact.prepare_data(values, temperature)?);
        }
        Ok(Self {
            artifact: Arc::clone(artifact),
            data,
        })
    }

    /// Linearize each lane independently using exactly the scalar trial contract.
    /// Discard all output on failure; no prepared lane or history is modified.
    ///
    /// # Errors
    /// Shape, invalid trial point or an unsupported declared policy.
    pub fn linearize_trials(
        &self,
        inputs: &[crate::trial::TrialInput<'_>],
        coordinates: &mut [f64],
        outcomes: &mut [crate::trial::TrialOutcome],
    ) -> Result<()> {
        let width = self.artifact.descriptor().equations.len();
        if inputs.len() != self.data.len()
            || outcomes.len() != self.data.len()
            || self.data.len().checked_mul(width) != Some(coordinates.len())
        {
            return Err(Error::Shape("trial tile input"));
        }
        for (((prepared, input), point), outcome) in self
            .data
            .iter()
            .zip(inputs)
            .zip(coordinates.chunks_exact_mut(width))
            .zip(outcomes)
        {
            *outcome = crate::trial::linearize(&self.artifact, prepared, *input, point)?;
        }
        Ok(())
    }

    /// Apply the same independent additional convergence test in every lane.
    ///
    /// # Errors
    /// Shape, invalid samples or unsupported declared policy. Discard outputs
    /// on failure, including any completed earlier lanes.
    pub fn check_trial_convergence(
        &self,
        inputs: &[crate::trial::ConvergenceInput<'_>],
        results: &mut [crate::trial::Convergence],
    ) -> Result<()> {
        if inputs.len() != self.data.len() || results.len() != self.data.len() {
            return Err(Error::Shape("convergence tile input"));
        }
        for ((prepared, input), result) in self.data.iter().zip(inputs).zip(results) {
            *result = crate::trial::convergence(&self.artifact, prepared, *input)?;
        }
        Ok(())
    }

    /// Evaluate contiguous lanes with no new allocation.
    ///
    /// # Errors
    /// Wrong shape, nonfinite input or invalid family output.
    pub fn evaluate(&self, coordinates: &[f64], out: &mut BatchOutput) -> Result<()> {
        let descriptor = self.artifact.descriptor();
        if self.data.len() != out.lanes
            || descriptor.equations.len() != out.width
            || coordinates.len() != out.width * out.lanes
        {
            return Err(Error::Shape("tile input"));
        }
        if coordinates.iter().any(|value| !value.is_finite()) {
            return Err(Error::Domain("nonfinite tile coordinates".into()));
        }
        out.reset(descriptor.charge)?;
        self.artifact
            .kernel
            .evaluate_batch(&self.data, coordinates, out)?;
        out.validate(descriptor)
    }
}

/// Default nonallocating scalar traversal for specialized family kernels.
///
/// # Errors
/// Invalid tile shape or family evaluation failure.
pub fn evaluate_scalar_tile<K: Kernel + ?Sized>(
    kernel: &K,
    prepared: &[K::Prepared],
    coordinates: &[f64],
    out: &mut BatchOutput,
) -> Result<()> {
    if prepared.len() != out.lanes || coordinates.len() != out.width * out.lanes {
        return Err(Error::Shape("scalar tile input"));
    }
    for (lane, (instance, coords)) in prepared
        .iter()
        .zip(coordinates.chunks_exact(out.width))
        .enumerate()
    {
        kernel.evaluate(instance, coords, &mut out.point(lane)?)?;
    }
    Ok(())
}
