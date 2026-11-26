//! Homogeneous reusable lane-major workspaces. The kernel can specialize the
//! scalar traversal with a generated or SIMD implementation.

use std::sync::Arc;

use crate::{
    Artifact, Capability, Descriptor, Error, Kernel, Point, Prepared, Result, allocate, noise,
};

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

/// Recipe for one changed lane. Parameter order and ambient units are
/// identical to standalone preparation.
#[derive(Clone, Copy, Debug)]
pub struct LaneUpdate<'a> {
    pub lane: usize,
    pub values: &'a [f64],
    pub ambient_kelvin: f64,
}

/// Validated replacements holding exclusive access to the target tile.
/// Dropping this transaction leaves every original lane unchanged.
/// Multiple disjoint tiles may be staged before any of them is committed.
#[must_use = "commit the validated replacements or explicitly discard them"]
pub struct LaneTransaction<'a, K: Kernel> {
    tile: &'a mut PreparedTile<K>,
    prepared: Vec<(usize, K::Prepared)>,
}

impl<K: Kernel> LaneTransaction<'_, K> {
    /// Install the already prepared lanes. Cannot fail a domain or shape
    /// check: all checks and allocation completed during staging.
    pub fn commit(self) {
        for (lane, data) in self.prepared {
            self.tile.data[lane] = data;
        }
    }
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

    /// Consume standalone prepared instances without preparing them again.
    /// The destination remains one contiguous typed array, not a collection
    /// of individually allocated or type-erased lanes.
    ///
    /// # Errors
    /// Empty input, foreign artifact (even with the same fingerprint), or
    /// allocation failure.
    pub fn from_prepared(artifact: &Arc<Artifact<K>>, instances: Vec<Prepared<K>>) -> Result<Self> {
        if instances.is_empty() {
            return Err(Error::Shape("empty prepared tile"));
        }
        for instance in &instances {
            artifact.check(instance)?;
        }
        let mut data = Vec::new();
        data.try_reserve_exact(instances.len())
            .map_err(|_| Error::Allocation)?;
        data.extend(instances.into_iter().map(|instance| instance.data));
        Ok(Self {
            artifact: Arc::clone(artifact),
            data,
        })
    }

    #[must_use = "the lane count determines valid lane ordinals"]
    pub fn lanes(&self) -> usize {
        self.data.len()
    }

    /// Stage only changed lanes, without cloning or preparing unaffected
    /// physical data. Validation precedes every mutation; a failed stage or
    /// discarded transaction leaves this candidate intact. Exclusive access
    /// prevents stale transactions and borrowed lane evaluation during commit.
    ///
    /// # Errors
    /// Foreign artifact, duplicate or out-of-range lane, allocation, or
    /// invalid candidate inputs including structural changes.
    pub fn stage_replacements(
        &mut self,
        artifact: &Artifact<K>,
        updates: &[LaneUpdate<'_>],
    ) -> Result<LaneTransaction<'_, K>> {
        if !std::ptr::eq(artifact, self.artifact.as_ref()) {
            return Err(Error::ForeignArtifact);
        }
        let mut ordered = Vec::new();
        ordered
            .try_reserve_exact(updates.len())
            .map_err(|_| Error::Allocation)?;
        ordered.extend_from_slice(updates);
        ordered.sort_unstable_by_key(|update| update.lane);
        if ordered
            .last()
            .is_some_and(|update| update.lane >= self.data.len())
        {
            return Err(Error::Shape("replacement lane"));
        }
        if ordered.windows(2).any(|pair| pair[0].lane == pair[1].lane) {
            return Err(Error::Shape("duplicate replacement lane"));
        }
        let mut prepared = Vec::new();
        prepared
            .try_reserve_exact(ordered.len())
            .map_err(|_| Error::Allocation)?;
        for update in ordered {
            let data = self
                .artifact
                .prepare_data(update.values, update.ambient_kelvin)?;
            prepared.push((update.lane, data));
        }
        Ok(LaneTransaction {
            tile: self,
            prepared,
        })
    }

    /// Evaluate exactly the same prepared lane used by specialized batch
    /// dispatch, without constructing an additional scalar instance.
    ///
    /// # Errors
    /// Invalid lane, input shape, domain or invalid output.
    pub fn evaluate_lane(
        &self,
        lane: usize,
        coordinates: &[f64],
        out: &mut Point<'_>,
    ) -> Result<()> {
        let data = self.data.get(lane).ok_or(Error::Shape("prepared lane"))?;
        self.artifact.evaluate_data(data, coordinates, out)
    }

    /// Evaluate source noise directly from the existing prepared lane.
    ///
    /// # Errors
    /// Invalid lane, bias/frequency or unsupported/invalid noise.
    pub fn evaluate_lane_noise(
        &self,
        lane: usize,
        coordinates: &[f64],
        frequency: f64,
        sink: &mut dyn noise::Sink,
    ) -> Result<()> {
        let data = self.data.get(lane).ok_or(Error::Shape("prepared lane"))?;
        self.artifact
            .evaluate_noise_data(data, coordinates, frequency, sink)
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
