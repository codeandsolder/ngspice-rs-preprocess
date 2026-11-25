//! Shared experimental ALB boundary annotation document.
use crate::{
    AlbInputReceiver, AlbOutputProducer, AnalogBridgeKind, CodeModelContract, DigitalPinDirection,
    LogicalAlbBoundary, OpaqueXInterface, verify_external_code_models,
    verify_external_native_libraries,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, de::DeserializeOwned};
use std::{fs, path::Path};

/// A source boundary; electrical data is required only for physical wiring.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundaryAnnotation {
    #[serde(default)]
    pub scope: Vec<String>,
    pub occurrence: Option<Vec<String>>,
    pub producer: Option<AlbOutputProducer>,
    #[serde(default)]
    pub receivers: Vec<AlbInputReceiver>,
    pub analog_net: String,
    pub digital_net: String,
    pub direction: DigitalPinDirection,
    pub family: Option<String>,
    pub in_family: Option<String>,
    pub out_family: Option<String>,
    pub supply_v: Option<f64>,
}

impl BoundaryAnnotation {
    /// Consume a definition-local annotation for logical lowering.
    ///
    /// # Errors
    /// Occurrence-specific annotations must first be specialized.
    pub fn into_logical(self) -> Result<LogicalAlbBoundary> {
        ensure!(
            self.occurrence.is_none(),
            "occurrence boundaries require alb-family-compile on original logical source"
        );
        Ok(LogicalAlbBoundary {
            scope: self.scope,
            analog_net: self.analog_net,
            digital_net: self.digital_net,
            direction: self.direction,
            producer: self.producer,
            receivers: self.receivers,
        })
    }
}

/// Electrical model metadata for a native ADC or DAC bridge.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeAnnotation {
    pub name: String,
    pub kind: AnalogBridgeKind,
    pub family: String,
    pub nominal_supply_v: f64,
}

/// Common input of logical lowering, family compilation and physical wiring.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnotationDocument {
    pub connections: Vec<BoundaryAnnotation>,
    pub bridge_models: Option<Vec<BridgeAnnotation>>,
    #[serde(default)]
    pub opaque_x_interfaces: Vec<OpaqueXInterface>,
    #[serde(default)]
    pub code_model_contracts: Vec<CodeModelContract>,
}

/// Decode a JSON file, retaining the path in I/O and syntax errors.
///
/// # Errors
/// File access or deserialization failure.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("decoding {}", path.display()))
}

/// Verify declared external subcircuit interfaces and code-model contracts.
///
/// # Errors
/// Missing or inconsistent external definitions.
pub fn verify_source_contracts(
    path: &Path,
    source: &str,
    interfaces: &[OpaqueXInterface],
    contracts: &[CodeModelContract],
) -> Result<()> {
    verify_external_native_libraries(path, source, interfaces)?;
    verify_external_code_models(path, source, contracts, interfaces)
}

/// Resolve the generated include name relative to the output deck.
///
/// # Errors
/// Output paths collide, differ in directory, or the include name is not UTF-8.
pub fn compiled_include_name<'a>(deck: &Path, compiled: &'a Path) -> Result<&'a str> {
    ensure!(
        deck != compiled && deck.parent() == compiled.parent(),
        "compiled ALB include and output source must be distinct files in the same directory"
    );
    compiled
        .file_name()
        .and_then(|name| name.to_str())
        .context("compiled ALB file name is not UTF-8")
}
