//! Explicit experimental ALB compiler and physical mixed-signal bridge planning.
//!
//! Not linked by the production dialect translator. Callers opt into its
//! bounded models and separately provision a matching reference runtime.
use anyhow::{Context, Result};
use model_translate_simplis::syntax::{parse_numeric, validate_node};

mod alb;
pub use alb::{
    AlbDigitalBit, AlbDriveStrength, AlbLogicLevel, AnalogBoundaryPin, AnalogBridgeKind,
    AnalogDigitalConnection, BridgeCatalog, BridgeModel, DigitalFamilyResolution,
    DigitalPinDirection, DiscoveredAlbPin, KnownLutSourceBit, KnownTimedLutSet, KnownTruthTable,
    LegacyCompatibilityTable, LogicFamily, LogicFamilyAssignment, NativeAdcParameters,
    NativeDacParameters, NativeDigitalBundle, NativeFamilyModel, NativeFamilyParameters,
    NativeFamilySpecification, NativePinWiring, PlannedAnalogBridge, ScopedAnalogDigitalConnection,
    TimingEnvironment, XspiceElectrical, XspiceLutTarget, XspiceTimedBit, XspiceTimedPlan,
    derive_family_wiring, discover_alb_pins, materialize_scoped_family_bridges,
    plan_analog_bridges, render_native_family_bridges,
};
mod whole;
pub use whole::{
    AlbOutputProducer, CompiledSourceAlbModel, FamilySpecialization, LogicalAlbBoundary,
    LogicalAlbExpansion, OccurrenceFamilyBoundary, compile_explicit_alb_source_models,
    expand_logical_alb_source, inspect_alb_source_models, specialize_family_occurrences,
};

/// Experimental isolated ALB-to-XSPICE subcircuit compiler.
///
/// This does not enable normal `SIMetrix` `D_LOGIC_BLOCK` translation. Callers
/// explicitly supply the timing environment and electrical test parameters;
/// digital-family/UNKNOWN semantics remain outside this model.
///
/// # Errors
/// Fails on unsupported ALB body, timing, initial state or native LUT limits.
pub fn compile_alb_xspice_subcircuit(
    body: &str,
    subcircuit: &str,
    timing: TimingEnvironment<'_>,
    electrical: XspiceElectrical,
) -> Result<String> {
    let (_, program) = alb::analyze_body(body)?.into_parts();
    let graph = program
        .context("SIMetrix ALB body is not a timed COMB program")?
        .timed_combinational_graph(timing)?
        .context("SIMetrix ALB timed graph needs acyclic BITWISE COMB semantics")?;
    graph
        .xspice_timed_plan()?
        .render_subcircuit(subcircuit, electrical)
}
