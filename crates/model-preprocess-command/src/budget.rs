//! Cumulative resource accounting shared by all stages of one preparation.
use crate::Cancellation;
use anyhow::{Result, ensure};
use std::sync::{Arc, Mutex};

/// Caller-configurable ceilings for a complete preparation request.
#[derive(Clone, Copy, Debug)]
pub struct WorkLimits {
    /// Bytes read from distinct original resources.
    pub input_bytes: usize,
    /// Bytes retained in admitted snapshots, including transformed resources.
    pub admitted_bytes: usize,
    /// Distinct admitted resources (including failed admissions).
    pub sources: usize,
    /// Total visits to resources, including cached repeated includes.
    pub source_visits: usize,
    /// Bytes scanned across expansion and specialization passes.
    pub scanned_bytes: usize,
    /// Logical statements visited, including repeated loop bodies.
    pub statements: usize,
    /// Expression token and value operations.
    pub evaluation_steps: usize,
    /// Generated definitions and vector elements.
    pub generated_nodes: usize,
    /// Cumulative intermediate emitted bytes.
    pub emitted_bytes: usize,
    /// Maximum simultaneous recursive preprocessing depth.
    pub depth: usize,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            input_bytes: 256 * 1024 * 1024,
            admitted_bytes: 256 * 1024 * 1024,
            sources: 16_384,
            source_visits: 65_536,
            scanned_bytes: 1024 * 1024 * 1024,
            statements: 8_000_000,
            evaluation_steps: 32_000_000,
            generated_nodes: 2_000_000,
            emitted_bytes: 512 * 1024 * 1024,
            depth: 64,
        }
    }
}

/// Cumulative accounting dimensions, each with its own diagnostic.
#[derive(Clone, Copy, Debug)]
pub enum Resource {
    /// Original input bytes.
    InputBytes,
    /// Admitted snapshot bytes.
    AdmittedBytes,
    /// Distinct source identities.
    Sources,
    /// Resource visits.
    SourceVisits,
    /// Scanned intermediate bytes.
    ScannedBytes,
    /// Logical statement visits.
    Statements,
    /// Expression evaluation work.
    EvaluationSteps,
    /// Generated definitions and vector elements.
    GeneratedNodes,
    /// Intermediate emission.
    EmittedBytes,
}

impl Resource {
    const fn counter(self, limits: &mut WorkLimits) -> &mut usize {
        match self {
            Self::InputBytes => &mut limits.input_bytes,
            Self::AdmittedBytes => &mut limits.admitted_bytes,
            Self::Sources => &mut limits.sources,
            Self::SourceVisits => &mut limits.source_visits,
            Self::ScannedBytes => &mut limits.scanned_bytes,
            Self::Statements => &mut limits.statements,
            Self::EvaluationSteps => &mut limits.evaluation_steps,
            Self::GeneratedNodes => &mut limits.generated_nodes,
            Self::EmittedBytes => &mut limits.emitted_bytes,
        }
    }
}

#[derive(Debug)]
struct State {
    remaining: WorkLimits,
    failure: Option<String>,
    depth: usize,
}

/// Shared accounting handle. Clones share counters and the first failure.
#[derive(Clone, Debug)]
pub struct WorkBudget {
    state: Arc<Mutex<State>>,
    cancellation: Cancellation,
}

impl Default for WorkBudget {
    fn default() -> Self {
        Self::new(WorkLimits::default(), Cancellation::default())
    }
}

impl WorkBudget {
    /// Start one request's counters.
    #[must_use = "the returned handle owns the cumulative request counters"]
    pub fn new(limits: WorkLimits, cancellation: Cancellation) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                remaining: limits,
                failure: None,
                depth: 0,
            })),
            cancellation,
        }
    }

    /// Check cancellation and sticky resource exhaustion.
    /// # Errors
    /// Cancellation, exhausted budget, or poisoned accounting state.
    pub fn check(&self) -> Result<()> {
        self.cancellation.check()?;
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("preprocessing budget lock poisoned"))?;
        ensure!(
            state.failure.is_none(),
            "{}",
            state.failure.as_deref().unwrap_or_default()
        );
        drop(state);
        Ok(())
    }

    /// Return the remaining allowance, before allocating or reading.
    /// # Errors
    /// Cancellation or previous resource exhaustion.
    pub fn remaining(&self, resource: Resource) -> Result<usize> {
        self.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("preprocessing budget lock poisoned"))?;
        Ok(*resource.counter(&mut state.remaining))
    }

    /// Consume allowance. A failed charge permanently invalidates this request.
    /// # Errors
    /// Cancellation or resource exhaustion.
    pub fn charge(&self, resource: Resource, amount: usize) -> Result<()> {
        self.cancellation.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("preprocessing budget lock poisoned"))?;
        ensure!(
            state.failure.is_none(),
            "{}",
            state.failure.as_deref().unwrap_or_default()
        );
        let counter = resource.counter(&mut state.remaining);
        if let Some(remaining) = counter.checked_sub(amount) {
            *counter = remaining;
            drop(state);
            Ok(())
        } else {
            let error = format!(
                "cumulative preprocessing {resource:?} budget exceeded (requested {amount}, remaining {counter})"
            );
            state.failure = Some(error.clone());
            drop(state);
            Err(anyhow::anyhow!(error))
        }
    }

    /// Check structural depth before constructing a recursively owned tree.
    /// # Errors
    /// Cancellation, previous exhaustion, or depth exceeding the stack bound.
    pub fn check_depth(&self, depth: usize) -> Result<()> {
        self.cancellation.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("preprocessing budget lock poisoned"))?;
        ensure!(
            state.failure.is_none(),
            "{}",
            state.failure.as_deref().unwrap_or_default()
        );
        let limit = state.remaining.depth.min(64);
        if depth > limit {
            let error =
                format!("cumulative preprocessing recursion depth budget exceeded ({limit})");
            state.failure = Some(error.clone());
            drop(state);
            return Err(anyhow::anyhow!(error));
        }
        drop(state);
        Ok(())
    }

    /// Enter one recursive frame, releasing it on scope exit.
    /// # Errors
    /// Cancellation or depth exhaustion. Depth is additionally capped at
    /// 64 to protect the native stack, regardless of the caller's ceiling.
    pub fn enter(&self) -> Result<DepthGuard> {
        self.cancellation.check()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("preprocessing budget lock poisoned"))?;
        ensure!(
            state.failure.is_none(),
            "{}",
            state.failure.as_deref().unwrap_or_default()
        );
        let limit = state.remaining.depth.min(64);
        if state.depth >= limit {
            let error =
                format!("cumulative preprocessing recursion depth budget exceeded ({limit})");
            state.failure = Some(error.clone());
            drop(state);
            return Err(anyhow::anyhow!(error));
        }
        state.depth += 1;
        drop(state);
        Ok(DepthGuard(self.clone()))
    }
}

/// RAII recursion frame.
pub struct DepthGuard(WorkBudget);

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.depth -= 1;
        }
    }
}
