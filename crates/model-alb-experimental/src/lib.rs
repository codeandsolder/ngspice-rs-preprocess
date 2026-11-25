//! Explicit experimental ALB compilation and physical mixed-signal bridge planning.
//! Not linked by production electrical translation.
use anyhow::{Context, Result};
use model_translate_simplis::syntax::{parse_numeric, validate_node};
mod alb;
pub use alb::{
    AlbDigitalBit, AlbDriveStrength, AlbLogicLevel, AlbTimedOutput, AnalogBoundaryPin,
    AnalogBridgeKind, AnalogDigitalConnection, BridgeCatalog, BridgeModel, DigitalFamilyResolution,
    DigitalPinDirection, DiscoveredAlbPin, KnownLutSourceBit, KnownTimedLutSet, KnownTruthTable,
    LegacyCompatibilityTable, LogicFamily, LogicFamilyAssignment, NativeAdcBridgeParameters,
    NativeAdcParameters, NativeDacParameters, NativeDigitalBundle, NativeFamilyModel,
    NativeFamilyParameters, NativeFamilySpecification, NativePinWiring, OutputResistance,
    PhysicalOutputDrive, PhysicalOutputLoad, PlannedAnalogBridge, RegisterTimingDefaults,
    ScopedAnalogDigitalConnection, StatefulEventEngine, StatefulPhysicalEventEngine,
    TimingEnvironment, XspiceElectrical, XspiceLutTarget, XspiceTimedBit, XspiceTimedPlan,
    compile_stateful_event_engine, compile_stateful_physical_engine, derive_family_wiring,
    discover_alb_pins, materialize_scoped_family_bridges, plan_analog_bridges,
    render_native_family_bridges,
};
mod whole;
pub use whole::{
    AlbInputReceiver, AlbOutputProducer, CodeModelContract, DefinitionFamilyBoundary,
    DiscoveredAlbBoundary, FamilySpecialization, LogicalAlbBoundary, LogicalAlbExpansion,
    LookupTableShape, OccurrenceFamilyBoundary, OpaqueXInterface, OpaqueXPinKind,
    discover_original_alb_boundaries, discover_original_alb_boundaries_with_opaque_x_interfaces,
    expand_logical_alb_source, expand_logical_alb_source_with_opaque_x_interfaces,
    inspect_alb_source_models, specialize_family_occurrences, verify_external_code_models,
    verify_external_native_libraries,
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
    let (stateless, stateful) = alb::analyze_body_in_environment(body, timing)?.into_parts();
    let plan = if let Some(table) = stateless {
        alb::XspiceTimedPlan::from_known_truth_table(&table, timing)?
    } else {
        let graph = stateful
            .context("SIMetrix ALB body is neither stateless nor timed COMB")?
            .timed_combinational_graph(timing)?
            .context("SIMetrix ALB timed graph needs acyclic BITWISE COMB semantics")?;
        graph.xspice_timed_plan()?
    };
    plan.render_subcircuit(subcircuit, electrical)
}
