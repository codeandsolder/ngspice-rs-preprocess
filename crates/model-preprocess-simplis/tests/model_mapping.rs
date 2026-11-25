use anyhow::{Context, Result};
use model_preprocess_command::WorkBudget;
use model_preprocess_simplis::source_dialect::{
    DialectContext, ScopedStatement, SourceLanguage,
    model_mapping::{Device, DeviceConfiguration, MappingTable, SelectionOrigin},
};

fn statements(input: &str) -> Vec<ScopedStatement> {
    input
        .lines()
        .enumerate()
        .map(|(index, line)| ScopedStatement {
            source: "root.net".into(),
            context: DialectContext::default(),
            first_line: index + 1,
            last_line: index + 1,
            text: line.into(),
        })
        .collect()
}

fn table(input: &str) -> Result<MappingTable> {
    MappingTable::resolve(
        &DeviceConfiguration::default(),
        &statements(input),
        &WorkBudget::default(),
    )
}

#[test]
fn map_key_is_type_and_level_not_instance_name() -> Result<()> {
    let source = statements(
        ".MAP ModelName=d, Level=1, Device=Diode3 REPORT=ON\n.model a D(LEVEL=1 IS=2p)\n.model b D(IS=2p)\n.model c D(LEVEL=3 IS=2p)",
    );
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    let selections = mapped.selections;
    assert_eq!(
        selections
            .iter()
            .map(|s| s.mapping.device)
            .collect::<Vec<_>>(),
        [Device::Diode3, Device::Diode1, Device::Diode3]
    );
    assert_eq!(selections[0].mapping.origin, SelectionOrigin::NetlistMap);
    assert_eq!(selections[0].original, ".model a D(LEVEL=1 IS=2p)");
    assert!(mapped.statements[1].text.contains("LEVEL=3"));
    assert!(mapped.statements[0].text.starts_with('*'));
    Ok(())
}

#[test]
fn level_selection_is_independent_of_lexical_dialect_and_scope() -> Result<()> {
    let mut source = statements(
        ".subckt cell a b\n.model diode D (LEVEL=1 IS={LOG(100)})\n.ends\n.options hspicemodels=1\n.model base D\n",
    );
    source[1].context.language = SourceLanguage::Hspice;
    source[1].source = "nested.lib".into();
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    assert_eq!(mapped.selections[0].scope, ["cell"]);
    assert_eq!(mapped.selections[0].source, "nested.lib");
    assert_eq!(
        mapped.selections[0].context.language,
        SourceLanguage::Hspice
    );
    assert_eq!(mapped.selections[0].mapping.device, Device::Diode3);
    assert_eq!(mapped.selections[1].mapping.device, Device::Diode3);
    assert!(mapped.statements[1].text.contains("IS={LOG(100)}"));
    Ok(())
}

#[test]
fn precedence_and_request_local_configuration() -> Result<()> {
    let config = DeviceConfiguration::parse(
        "devices.dcf",
        "ModelName=D Level=1 Device=Diode1",
        &WorkBudget::default(),
    )?;
    let source = statements(".options HSPICEMODELS=1\n.model a D LEVEL=1\n");
    let configured = MappingTable::resolve(&config, &source, &WorkBudget::default())?
        .apply(&source, &WorkBudget::default())?;
    assert_eq!(configured.selections[0].mapping.device, Device::Diode1);
    assert_eq!(
        configured.selections[0].mapping.origin,
        SelectionOrigin::DeviceConfiguration
    );
    let source = statements(
        ".options HSPICEMODELS=1\n.map ModelName=D Level=1 Device=Diode3\n.model a D LEVEL=1\n",
    );
    let mapped = MappingTable::resolve(&config, &source, &WorkBudget::default())?
        .apply(&source, &WorkBudget::default())?;
    assert_eq!(
        mapped.selections[0].mapping.origin,
        SelectionOrigin::NetlistMap
    );
    assert_eq!(mapped.selections[0].mapping.device, Device::Diode3);
    let fresh = table("")?.apply(&statements(".model a D LEVEL=1"), &WorkBudget::default())?;
    assert_eq!(fresh.selections[0].mapping.device, Device::Diode1);
    Ok(())
}

#[test]
fn rejects_malformed_ambiguous_and_unqualified_selections() -> Result<()> {
    for source in [
        ".map ModelName=D Level=1 Device=unknown",
        ".map ModelName=D Level=1.5 Device=Diode3",
        ".map ModelName=D Level=99 Device=Diode3",
        ".map ModelName=D Level=1 Device=MOS3",
        ".map ModelName=D Level=1 Device=Diode3\n.map ModelName=D Level=1 Device=Diode1",
        ".map ModelName=D Level=1 Device=Diode3 Device=Diode1",
        ".options HSPICEMODELS=maybe",
    ] {
        assert!(table(source).is_err(), "{source}");
    }
    for card in [
        ".model a D(LEVEL=171)",
        ".model a D(LEVEL=1 LEVEL=3)",
        ".model a NMOS(LEVEL=3 VLEVEL=1)",
        ".model a NMOS(LEVEL=7 VERSION=3.1)",
        ".model a NMOS(LEVEL=7 VERSION=3.3)",
        ".model a NPN(LEVEL=8)",
        ".model a NMOS(LEVEL=1 CGDMAX=2p)",
    ] {
        assert!(
            table("")?
                .apply(&statements(card), &WorkBudget::default())
                .is_err(),
            "{card}"
        );
    }
    Ok(())
}

#[test]
fn opaque_payloads_and_new_level_aliases() -> Result<()> {
    let source = statements(
        ".FILE payload\n.MAP broken\n.ENDF\n.CONTROL\n.MAP broken\n.ENDC\n.map ModelName=NMOS Level=69 Device=MOS3 Letter=M\n.model a NMOS LEVEL=69\n",
    );
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    assert_eq!(mapped.selections.len(), 1);
    assert_eq!(mapped.selections[0].mapping.device, Device::Mos3);
    assert!(
        mapped
            .statements
            .last()
            .context("missing card")?
            .text
            .contains("LEVEL=3")
    );
    assert_eq!(mapped.statements[1].text, ".MAP broken");
    Ok(())
}

#[test]
fn legacy_discrete_mos_signature_and_exact_level_version() -> Result<()> {
    let source = statements(
        ".model legacy NMOS ( LEVEL = 7 CGDMIN=2p )\n.model explicit PMOS (VLEVEL=17 CGDMIN=3p)\n.model bsim NMOS (LEVEL=8 VERSION='3.3')\n",
    );
    let mapped = table("")?.apply(&source, &WorkBudget::default())?;
    assert_eq!(
        mapped.selections[0].mapping.origin,
        SelectionOrigin::LegacyMos17Signature
    );
    assert_eq!(mapped.selections[0].mapping.device, Device::Mos17);
    assert!(mapped.statements[0].text.contains("LEVEL=17"));
    assert!(mapped.statements[1].text.contains("LEVEL=17"));
    assert!(mapped.statements[2].text.contains("VERSION=3.3"));
    Ok(())
}

#[test]
fn full_compatibility_cannot_silently_drop_other_physical_options() -> Result<()> {
    assert!(table(".options HSPICECOMPATIBILITY=1").is_err());
    assert!(table(".options HSPICEMODELS=1\n.options HSPICEMODELS=0").is_err());
    let source = statements(".options TEMP=85 HSPICEMODELS=1 RELTOL=1e-4");
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    assert_eq!(
        mapped.statements[0].text,
        ".options TEMP=85 HSPICEMODELS=1 RELTOL=1e-4"
    );
    Ok(())
}

#[test]
fn global_mapping_cannot_be_applied_from_inactive_or_local_scopes() -> Result<()> {
    for source in [
        ".if {0}\n.map ModelName=D Level=1 Device=Diode3\n.endif",
        ".while {0}\n.opt HSPICEMODELS\n.endwhile",
        ".subckt idle a b\n.options HSPICEMODELS\n.ends",
    ] {
        let error = table(source)
            .err()
            .context("nested global selector accepted")?;
        assert!(
            error
                .to_string()
                .contains("simulation-wide model selection")
        );
    }
    Ok(())
}
