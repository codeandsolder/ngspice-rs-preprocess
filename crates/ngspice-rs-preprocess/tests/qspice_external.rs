//! Source-only external module grammar and authorization boundary.
use anyhow::{Context, Result};
use model_normalize_ngspice::{
    linked_source::{RootKind, SourceDialect, SourceLinker},
    qspice::{Feature, Unsupported, external_module::external_modules},
};
use model_preprocess_command::WorkBudget;
use ngspice_rs_preprocess::{Cancellation, InputFormat, Options, Request, SourceInput, prepare};
use std::{fs, path::Path};

fn linker() -> SourceLinker {
    SourceLinker {
        dialect: SourceDialect::Qspice,
        ..SourceLinker::default()
    }
}

fn prepare_library(path: &Path) -> Result<()> {
    prepare(
        &Request {
            format: InputFormat::QspiceLibrary,
            input: SourceInput::File(path.to_owned()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
    .map(|_| ())
}

#[test]
fn ordered_declaration_metadata_preserves_types_and_scope() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.lib");
    let child = dir.path().join("child.lib");
    fs::write(&root, b".subckt outer a b\n.lib child.lib chosen\n.ends outer\n.subckt other a b\n.lib child.lib chosen\n.ends other\n")?;
    // Windows-1252 source. The unused section must not be inspected.
    fs::write(&child, b".lib unused\n\xd8invalid\n.endl unused\n.lib chosen\n\xd8\xb4X1 \xaba\xb4b b\xb4d\xbb \xabq\xb4ui out\xb4f\xbb \xab0 vss\xbb \"missing module\"\n+ float Gain={2 * scale} int Gain=3 char* file=\"path with spaces.dat\" ROUT=10\n.endl chosen\n")?;
    let linked = linker().link(&root)?;
    let modules = external_modules(&linked, RootKind::Include, &WorkBudget::default())?;
    assert_eq!(modules.len(), 2);
    let [first, second] = modules.as_slice() else {
        anyhow::bail!("expected two declarations")
    };
    assert_eq!(first.origin.path, child.canonicalize()?);
    assert_eq!(first.origin.line, 5);
    assert_ne!(first.origin.visit, second.origin.visit);
    assert_ne!(first.origin.scope, second.origin.scope);
    assert_eq!(first.instance, "Ø´X1");
    assert_eq!(first.module_reference, "\"missing module\"");
    assert_eq!(
        first
            .inputs
            .iter()
            .map(|pin| (pin.node.as_str(), pin.conversion.as_deref()))
            .collect::<Vec<_>>(),
        [("a", Some("b")), ("b", Some("d"))]
    );
    assert_eq!(
        first
            .outputs
            .iter()
            .map(|pin| (pin.node.as_str(), pin.conversion.as_deref()))
            .collect::<Vec<_>>(),
        [("q", Some("ui")), ("out", Some("f"))]
    );
    assert_eq!(
        first
            .common
            .iter()
            .map(|pin| (pin.node.as_str(), pin.conversion.as_deref()))
            .collect::<Vec<_>>(),
        [("0", None), ("vss", None)]
    );
    assert_eq!(
        first
            .parameters
            .iter()
            .map(|parameter| (
                parameter.data_type.as_deref(),
                parameter.name.as_str(),
                parameter.value.as_str()
            ))
            .collect::<Vec<_>>(),
        [
            (Some("float"), "Gain", "{2 * scale}"),
            (Some("int"), "Gain", "3"),
            (Some("char*"), "file", "\"path with spaces.dat\""),
            (None, "ROUT", "10")
        ]
    );
    assert_eq!(first.parameters, second.parameters);
    let error = prepare_library(&root)
        .err()
        .context("external module prepared without authorization")?;
    let diagnostic = error
        .downcast_ref::<Unsupported>()
        .context("missing physical diagnostic")?;
    assert_eq!(diagnostic.feature, Feature::ExternalModule);
    assert_eq!(diagnostic.path, child.canonicalize()?);
    assert_eq!(diagnostic.line, 5);
    assert!(diagnostic.detail.contains("missing module"));
    Ok(())
}

#[test]
fn explicit_utf8_and_unqualified_existing_module() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deck.lib");
    // A module file existing beside a netlist must not authorize its execution.
    fs::write(dir.path().join("probe.dll"), b"not an executable")?;
    fs::write(&path, "\u{feff}Ødemo «entrée» «» «0» probe.dll\n")?;
    let linked = linker().link(&path)?;
    assert_eq!(linked.visits.len(), 1);
    let modules = external_modules(&linked, RootKind::Include, &WorkBudget::default())?;
    let [declaration] = modules.as_slice() else {
        anyhow::bail!("expected one declaration")
    };
    assert_eq!(
        declaration.inputs.first().context("missing input")?.node,
        "entrée"
    );
    assert!(declaration.outputs.is_empty());
    assert_eq!(declaration.module_reference, "probe.dll");
    let error = prepare_library(&path)
        .err()
        .context("unqualified module accepted")?;
    let diagnostic = error
        .downcast_ref::<Unsupported>()
        .context("missing physical diagnostic")?;
    assert_eq!(diagnostic.feature, Feature::ExternalModule);
    assert!(
        diagnostic
            .detail
            .contains("explicitly qualified implementation")
    );
    Ok(())
}

#[test]
fn malformed_declarations_have_physical_diagnostics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deck.lib");
    for statement in [
        "Ødemo «a» «b» mod",
        "Ødemo «a» «b» «»",
        "Ødemo «a´» «b» «» mod",
        "Ødemo «´d» «b» «» mod",
        "Ødemo «a «b» «» «» mod",
        "Ødemo «a» «b» «» mod float gain",
        "Ødemo «a» «b» «» mod gain=",
        "Ødemo «a» «b» «» mod gain={a",
    ] {
        fs::write(&path, format!("\u{feff}* source\n{statement}\n"))?;
        let error = prepare_library(&path)
            .err()
            .context("malformed module accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("missing physical diagnostic for {statement}: {error:#}"))?;
        assert_eq!(diagnostic.feature, Feature::ExternalModule);
        assert_eq!(diagnostic.line, 2);
        assert!(diagnostic.detail.contains("invalid declaration"));
    }
    Ok(())
}
