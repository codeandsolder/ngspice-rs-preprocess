use anyhow::Result;
use model_normalize_ngspice::{
    linked_source::{LinkedSource, RootKind, SourceLinker},
    parameter_scope::ModelLibrary,
};
use ngspice_rs_preprocess::PreparedBundle;
use std::fs;

pub fn linked_library(bundle: &PreparedBundle) -> Result<ModelLibrary> {
    ModelLibrary::parse_linked(&linked_source(bundle)?, RootKind::Deck)
}

pub fn linked_source(bundle: &PreparedBundle) -> Result<LinkedSource> {
    let dir = tempfile::tempdir()?;
    for artifact in bundle.artifacts() {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    SourceLinker::default().link(&dir.path().join(&bundle.primary.filename))
}
