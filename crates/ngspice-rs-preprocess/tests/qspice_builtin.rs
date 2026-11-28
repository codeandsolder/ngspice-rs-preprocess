//! Source-only specialized built-in grammar, not electrical qualification.
use anyhow::{Context, Result, bail};
use model_normalize_ngspice::{
    linked_source::{RootKind, SourceDialect, SourceLinker},
    qspice::{
        Feature, Unsupported,
        builtin_device::{BuiltinClass, BuiltinPort, builtin_devices},
    },
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
fn legacy_encoding_occurrence_scope_and_unused_ports() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.lib");
    let child = dir.path().join("child.lib");
    fs::write(&root, b".subckt first a b\n.lib child.lib chosen\n.ends first\n.subckt second a b\n.lib child.lib chosen\n.ends second\n")?;
    // The standalone marker is not a ground connection or the named net ¥7.
    // The selected library contains 16 source slots, with continuation fields.
    fs::write(
        &child,
        b".lib unused\n\xa5bad\n.endl unused\n.lib chosen\n\xa5\xb4U1 vdd 0 out \xa57 a b \xa5 \xa5\n+ \xa5 \xa5 \xa5 \xa5 \xa5 \xa5 \xa5 \xa5 AND\n+ TTOL={2 * scale} ROUT=10 ROUT=20\n.endl chosen\n",
    )?;
    let linked = linker().link(&root)?;
    let devices = builtin_devices(&linked, RootKind::Include, &WorkBudget::default())?;
    let [first, second] = devices.as_slice() else {
        bail!("expected two source occurrences");
    };
    assert_eq!(first.origin.path, child.canonicalize()?);
    assert_eq!(first.origin.line, 5);
    assert_ne!(first.origin.visit, second.origin.visit);
    assert_ne!(first.origin.scope, second.origin.scope);
    assert_eq!(first.instance, "¥´U1");
    assert_eq!(first.class, BuiltinClass::Function);
    assert_eq!(first.selector, "AND");
    assert_eq!(first.ports.len(), 16);
    assert_eq!(
        first.ports.get(..7).context("missing port slots")?,
        [
            BuiltinPort::Node("vdd".to_owned()),
            BuiltinPort::Node("0".to_owned()),
            BuiltinPort::Node("out".to_owned()),
            BuiltinPort::Node("¥7".to_owned()),
            BuiltinPort::Node("a".to_owned()),
            BuiltinPort::Node("b".to_owned()),
            BuiltinPort::Unassigned,
        ]
    );
    assert!(
        first.ports[6..]
            .iter()
            .all(|port| *port == BuiltinPort::Unassigned)
    );
    assert_eq!(first.parameter_text, "TTOL={2 * scale} ROUT=10 ROUT=20");
    assert_eq!(first.ports, second.ports);
    assert_eq!(first.parameter_text, second.parameter_text);
    let error = prepare_library(&root)
        .err()
        .context("unqualified device accepted")?;
    let diagnostic = error
        .downcast_ref::<Unsupported>()
        .context("missing source diagnostic")?;
    assert_eq!(diagnostic.feature, Feature::SpecialDevice);
    assert_eq!(diagnostic.path, child.canonicalize()?);
    assert_eq!(diagnostic.line, 5);
    assert!(diagnostic.detail.contains("qualified native"));
    Ok(())
}

#[test]
fn documented_fixed_layouts_and_opaque_selectors() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deck.lib");
    let cases = [
        ("Ã", BuiltinClass::Amplifier, 16, "rRoPaMp"),
        ("Ã", BuiltinClass::Amplifier, 16, "multGmAmp"),
        ("¥", BuiltinClass::Function, 16, "LocalAlias"),
        ("€", BuiltinClass::Dac, 32, "DAC"),
        ("£", BuiltinClass::DualGateDriver, 64, "Driver"),
    ];
    let mut source = String::from("\u{feff}");
    for (designator, _, count, selector) in cases {
        source.push_str(&format!(
            "{designator}dev 0{} {selector} Gain={{x}}\n",
            " ¥".repeat(count - 1)
        ));
    }
    fs::write(&path, source)?;
    let linked = linker().link(&path)?;
    let devices = builtin_devices(&linked, RootKind::Include, &WorkBudget::default())?;
    assert_eq!(devices.len(), cases.len());
    for (device, (_, class, count, selector)) in devices.iter().zip(cases) {
        assert_eq!(device.class, class);
        assert_eq!(device.class.port_count(), count);
        assert_eq!(device.ports.len(), count);
        assert_eq!(device.selector, selector);
        assert_eq!(
            device.ports.first(),
            Some(&BuiltinPort::Node("0".to_owned()))
        );
        assert!(
            device.ports[1..]
                .iter()
                .all(|port| *port == BuiltinPort::Unassigned)
        );
    }
    Ok(())
}

#[test]
fn malformed_declarations_keep_physical_diagnostics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deck.lib");
    let slots = " ¥".repeat(16);
    for declaration in [
        "Ãdev in out RRopAmp".to_owned(),
        format!("¥{slots} AND"),
        format!("¥dev{slots}"),
        format!("¥dev{slots} ¥"),
        format!("¥dev{slots} ROUT=10"),
        format!("Ãdev ={} RRopAmp", " ¥".repeat(15)),
    ] {
        fs::write(&path, format!("\u{feff}* source\n{declaration}\n"))?;
        let error = prepare_library(&path)
            .err()
            .context("malformed declaration accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .with_context(|| format!("missing diagnostic for {declaration}: {error:#}"))?;
        assert_eq!(diagnostic.feature, Feature::SpecialDevice);
        assert_eq!(diagnostic.line, 2);
        assert!(diagnostic.detail.contains("invalid built-in declaration"));
    }
    Ok(())
}

#[test]
fn transformer_and_unknown_designators_remain_unqualified() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("deck.lib");
    for declaration in ["×T1 «a b c d» turns=1 2", "Öother a b c model"] {
        fs::write(&path, format!("\u{feff}* source\n{declaration}\n"))?;
        let error = prepare_library(&path)
            .err()
            .context("unqualified declaration accepted")?;
        let diagnostic = error
            .downcast_ref::<Unsupported>()
            .context("missing diagnostic")?;
        assert_eq!(diagnostic.feature, Feature::SpecialDevice);
        assert_eq!(diagnostic.line, 2);
    }
    Ok(())
}
