use anyhow::{Context, Result};
use model_preprocess_command::WorkBudget;
use model_preprocess_simplis::source_dialect::{
    DialectContext, ScopedStatement, SourceDialect,
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
    source[1].context.language = SourceDialect::Hspice;
    source[1].source = "nested.lib".into();
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    assert_eq!(mapped.selections[0].scope, ["cell"]);
    assert_eq!(mapped.selections[0].source, "nested.lib");
    assert_eq!(mapped.selections[0].context.language, SourceDialect::Hspice);
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

#[test]
fn diode_alias_uses_the_selected_kernel_parameter_grammar() -> Result<()> {
    let source = statements(
        ".map ModelName=DTEST Level=69 Device=DIODE1 Letter=D
.model a DTEST (LEVEL 69 IS {local_is} RS 2 T_ABS {local_temp})
",
    );
    let budget = WorkBudget::default();
    let mapped = MappingTable::resolve(&DeviceConfiguration::default(), &source, &budget)?
        .apply(&source, &budget)?;
    assert_eq!(mapped.selections.len(), 1);
    let card = &mapped
        .statements
        .last()
        .context("missing mapped model")?
        .text;
    assert!(card.contains(model_kernel_diode::MODEL_FAMILY), "{card}");
    assert!(card.contains("LEVEL=1 "), "{card}");
    assert!(card.contains("IS={local_is}"), "{card}");
    assert!(card.contains("RS=2"), "{card}");
    assert!(card.contains("T_ABS={local_temp}"), "{card}");
    Ok(())
}

#[test]
fn classic_bjt_implementation_selection_is_not_the_lexical_dialect() -> Result<()> {
    let mut source = statements(
        ".map ModelName=PNP Level=69 Device=BJT\n.model n NPN(IS=3p)\n.model p PNP(LEVEL=69 IS=4p TNOM=29)\n.model v NPN(LEVEL=4 IS=1p)\n",
    );
    source[2].context.language = SourceLanguage::Hspice;
    let mapped = MappingTable::resolve(
        &DeviceConfiguration::default(),
        &source,
        &WorkBudget::default(),
    )?
    .apply(&source, &WorkBudget::default())?;
    assert_eq!(
        mapped
            .selections
            .iter()
            .map(|s| s.mapping.device)
            .collect::<Vec<_>>(),
        [
            Device::GummelPoonLegacy,
            Device::GummelPoonLegacy,
            Device::Vbic
        ]
    );
    assert_eq!(
        mapped.selections[1].mapping.origin,
        SelectionOrigin::NetlistMap
    );
    assert!(
        mapped.statements[1]
            .text
            .contains("gummel_poon_legacy_npn (LEVEL=1")
    );
    assert!(
        mapped.statements[2]
            .text
            .contains("gummel_poon_legacy_pnp (LEVEL=1")
    );
    assert!(mapped.statements[3].text.contains("NPN (LEVEL=4"));
    assert!(
        table("")?
            .apply(&statements(".model l LPNP(IS=3p)"), &WorkBudget::default())
            .is_err()
    );
    Ok(())
}

#[test]
fn classic_bjt_thermal_law_follows_the_selected_implementation() -> Result<()> {
    use model_normalize_ngspice::bjt_card::{BjtPolarity, BjtThermalConvention, ClassicBjtFamily};
    use model_normalize_ngspice::model_card::parse_model_cards;
    let source = statements(
        ".model omitted NPN(IS={local_is} BF=100 TNOM=27)\n\
         .model level1 PNP(LEVEL=1 IS=3p TNOM=50)\n\
         .map ModelName=NPN Level=69 Device=BJT Letter=Q\n\
         .model remapped NPN(LEVEL=69 IS=5p)\n\
         .map ModelName=PNP Level=70 Device=NGSPICE_BJT Letter=Q\n\
         .model explicit_native PNP(LEVEL=70 IS=7p)\n\
         .model vbic NPN(LEVEL=4 IS=9p)",
    );
    let budget = WorkBudget::default();
    let mapped = MappingTable::resolve(&DeviceConfiguration::default(), &source, &budget)?
        .apply(&source, &budget)?;
    let identities = mapped
        .selections
        .iter()
        .map(|selection| selection.mapping.device)
        .collect::<Vec<_>>();
    assert_eq!(
        identities,
        [
            Device::GummelPoonLegacy,
            Device::GummelPoonLegacy,
            Device::GummelPoonLegacy,
            Device::GummelPoonNative,
            Device::Vbic
        ]
    );
    assert_eq!(
        mapped.selections[2].mapping.origin,
        SelectionOrigin::NetlistMap
    );
    assert_eq!(
        mapped.selections[3].mapping.origin,
        SelectionOrigin::NetlistMap
    );
    let text = mapped
        .statements
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let cards = parse_model_cards(text.as_bytes())?;
    use BjtPolarity::{Npn, Pnp};
    use BjtThermalConvention::{Legacy, Ngspice};
    for (card, polarity, thermal) in [
        (&cards[0], Npn, Legacy),
        (&cards[1], Pnp, Legacy),
        (&cards[2], Npn, Legacy),
        (&cards[3], Pnp, Ngspice),
    ] {
        assert_eq!(
            ClassicBjtFamily::parse(&card.family),
            Some(ClassicBjtFamily { polarity, thermal })
        );
        assert_eq!(
            card.parameters
                .iter()
                .find(|p| p.name.eq_ignore_ascii_case("LEVEL"))
                .and_then(|p| p.expression.as_deref()),
            Some("1")
        );
    }
    assert_eq!(
        cards[0]
            .parameters
            .iter()
            .find(|p| p.name == "IS")
            .and_then(|p| p.expression.as_deref()),
        Some("{local_is}")
    );
    assert_eq!(
        cards[1]
            .parameters
            .iter()
            .find(|p| p.name == "TNOM")
            .and_then(|p| p.expression.as_deref()),
        Some("50")
    );
    assert!(cards[4].family.eq_ignore_ascii_case("NPN"));
    assert_eq!(cards[4].parameters[0].expression.as_deref(), Some("4"));
    Ok(())
}

#[test]
fn classic_bjt_selection_is_not_a_lexical_dialect_or_lateral_pnp_guess() -> Result<()> {
    let mut source = statements(
        ".subckt part c b e\n.model same PNP(IS={local_is} T_ABS={TEMP} TNOM=27)\n.ends\n.model same NPN(IS=2p)\n",
    );
    source[1].context.language = SourceLanguage::Hspice;
    source[1].source = "part.lib".into();
    source[3].context.language = SourceLanguage::Pspice;
    let budget = WorkBudget::default();
    let mapped = MappingTable::resolve(&DeviceConfiguration::default(), &source, &budget)?
        .apply(&source, &budget)?;
    assert_eq!(mapped.selections[0].scope, ["part"]);
    assert_eq!(mapped.selections[0].source, "part.lib");
    assert_eq!(
        mapped.selections[0].context.language,
        SourceLanguage::Hspice
    );
    assert_eq!(
        mapped.selections[1].context.language,
        SourceLanguage::Pspice
    );
    assert!(mapped.statements[1].text.contains("gummel_poon_legacy_pnp"));
    assert!(mapped.statements[3].text.contains("gummel_poon_legacy_npn"));
    for declaration in [
        ".model lateral LPNP(IS=1p)",
        ".model versioned NPN(LEVEL=1 VERSION=2 IS=1p)",
        ".model hicum PNP(LEVEL=8)",
    ] {
        assert!(
            table("")?
                .apply(&statements(declaration), &WorkBudget::default())
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn classic_bjt_selector_preserves_binned_model_names_and_physical_fields() -> Result<()> {
    use model_normalize_ngspice::model_card::parse_model_cards;
    let source = statements(
        ".model q.1 NPN(LEVEL=1 IS=2p BF={beta} TNOM=40)\n.model q.2 NPN(IS=5p BF=150 TNOM=55)\n.model q.3 PNP(LEVEL=1 IS=7p BF=80 TNOM=65)",
    );
    let budget = WorkBudget::default();
    let mapped = MappingTable::resolve(&DeviceConfiguration::default(), &source, &budget)?
        .apply(&source, &budget)?;
    let text = mapped
        .statements
        .iter()
        .map(|statement| statement.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let cards = parse_model_cards(text.as_bytes())?;
    assert_eq!(cards.len(), 3);
    for (card, selection) in cards.iter().zip(&mapped.selections) {
        assert_eq!(card.name, selection.card.name);
        assert!(card.family.starts_with("gummel_poon_legacy_"));
        let fields = |card: &model_normalize_ngspice::model_card::ModelCard| {
            card.parameters
                .iter()
                .filter(|parameter| !parameter.name.eq_ignore_ascii_case("LEVEL"))
                .map(|parameter| (parameter.name.clone(), parameter.expression.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(fields(card), fields(&selection.card));
    }
    assert_eq!(cards[0].name, "q.1");
    assert_eq!(cards[1].name, "q.2");
    assert_eq!(cards[2].name, "q.3");
    Ok(())
}
