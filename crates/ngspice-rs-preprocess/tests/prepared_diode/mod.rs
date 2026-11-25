use anyhow::Result;
use model_normalize_ngspice::{
    linked_source::{RootKind, SourceLinker},
    parameter_scope::ModelLibrary,
};
use ngspice_rs_preprocess::PreparedBundle;
use std::fs;

pub fn linked_library(bundle: &PreparedBundle) -> Result<ModelLibrary> {
    let dir = tempfile::tempdir()?;
    for artifact in bundle.artifacts() {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let linked = SourceLinker::default().link(&dir.path().join(&bundle.primary.filename))?;
    ModelLibrary::parse_linked(&linked, RootKind::Deck)
}
