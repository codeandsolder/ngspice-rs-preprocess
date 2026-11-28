use anyhow::{Context, Result};
use model_preprocess_command::WorkBudget;
use model_preprocess_simplis::source_dialect::{
    DialectContext, ScopedStatement, SourceDialect,
    model_mapping::{
        Device, DeviceConfiguration, HspiceMosJunctionMode, MappingTable, SelectionOrigin,
    },
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
fn hspice_bsim3_selection_preserves_exact_release_and_level_defaults() -> Result<()> {
    let mapping = table(".options HSPICEMODELS=1")?;
    for (level, device, acm, capmod, xpart) in [
        (49, Device::Bsim3HspiceJunction, 0, 0, 1),
        (53, Device::Bsim3HspiceBerkeleyJunction, 10, 3, 0),
    ] {
        for (requested, canonical) in [("3.24", "3.24"), ("3.2.4", "3.24"), ("3.3.0", "3.3")] {
            let lines = statements(&format!(
                ".model m NMOS LEVEL={level} VERSION={requested} HSPVER=02.2"
            ));
            let selection = mapping
                .select_card(&lines[0], &[], &WorkBudget::default())?
                .context("selection")?;
            assert_eq!(selection.mapping.device, device);
            assert_eq!(selection.effective_version.as_deref(), Some(canonical));
            let defaults = selection
                .hspice_bsim3_defaults()
                .context("source defaults")?;
            assert_eq!(defaults.acm, acm);
            assert_eq!(defaults.capmod, capmod);
            assert_eq!(defaults.xpart, xpart);
            assert!(selection.lower().is_err());
            assert!(
                MappingTable::select_standalone_hspice(&lines[0], &[], &WorkBudget::default())
                    .is_err()
            );
        }
        for suffix in [
            "",
            " VERSION=3.2",
            " VERSION=3.21",
            " VERSION=3.2.3",
            " VERSION=3.4",
        ] {
            let lines = statements(&format!(".model m NMOS LEVEL={level}{suffix}"));
            assert!(
                mapping
                    .select_card(&lines[0], &[], &WorkBudget::default())
                    .is_err()
            );
        }
    }
    Ok(())
}

#[test]
fn hspice_junction_modes_keep_area_selection_separate_from_resistance() -> Result<()> {
    use HspiceMosJunctionMode as Mode;
    let mapping = table(".options HSPICEMODELS=1")?;
    for (level, version, default) in [
        (49, "3.24", Mode::HspiceExplicit),
        (53, "3.3", Mode::BerkeleyExplicit),
        (54, "4.7", Mode::BerkeleyIsolated),
    ] {
        let select = |parameters: &str| -> Result<_> {
            let lines = statements(&format!(
                ".model m NMOS LEVEL={level} VERSION={version} {parameters}"
            ));
            mapping
                .select_card(&lines[0], &[], &WorkBudget::default())?
                .context("model selection")?
                .hspice_mos_junction()?
                .context("junction selection")
        };
        let defaults = select("")?;
        assert_eq!(defaults.mode, default);
        assert!(!defaults.automatic_bsim3_geometry);
        for (acm, mode) in [
            (0, Mode::HspiceExplicit),
            (1, Mode::HspiceWidthNormalized),
            (2, Mode::HspiceIsolated),
            (3, Mode::HspiceStacked),
            (10, Mode::BerkeleyExplicit),
            (11, Mode::BerkeleyWidthNormalized),
            (12, Mode::BerkeleyIsolated),
            (13, Mode::BerkeleyStacked),
        ] {
            for calcacm in [0, 1] {
                let selected = select(&format!("ACM={acm} CALCACM={calcacm}"))?;
                assert_eq!(selected.mode, mode);
                assert_eq!(
                    selected.automatic_bsim3_geometry,
                    level != 54 && acm == 12 && calcacm == 1
                );
            }
        }
        for unsupported in [
            "ACM=4",
            "ACM=12.5",
            "ACM=12 ACM=12",
            "CALCACM=2",
            "ACM={mode}",
        ] {
            assert!(select(unsupported).is_err());
        }
    }
    Ok(())
}

#[test]
fn hspice_berkeley_junction_binding_separates_geometry_and_series_modes() -> Result<()> {
    use model_compact_intrinsic::{MosDiffusionTerminal, PreparedMosJunction};
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_simplis::source_dialect::model_mapping::{
        HspiceBsim3JunctionInput, HspiceJunctionSourceOptions,
    };

    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 2e-13 * expected.abs().max(1e-30),
            "{actual:e} != {expected:e}"
        );
    }
    let mapping = table(".options HSPICEMODELS=1")?;
    let input = HspiceBsim3JunctionInput {
        drawn_width_m: 10e-6,
        gate_width_m: 8e-6,
        diffusion: [
            MosDiffusionTerminal {
                area_m2: Some(12e-12),
                perimeter_m: None,
                shared: true,
            },
            MosDiffusionTerminal::default(),
        ],
        squares: [None, Some(0.)],
        default_squares: [2., 2.],
        operating_temperature_k: 300.15,
    };
    for (level, version) in [(49, "3.24"), (53, "3.3")] {
        for (acm, drain_r, source_r) in
            [(10, 93., 63.), (11, 100., 0.), (12, 18., 0.), (13, 18., 0.)]
        {
            for calcacm in [0, 1] {
                let lines = statements(&format!(
                    ".model m NMOS LEVEL={level} VERSION={version} ACM={acm} CALCACM={calcacm} WMLT=0.8 XW=1u HDIF=2u LD=0.2u LDIF=0.5u RD=90 RS=60 RDC=3 RSC=3 RSH=45 TNOM=27 CJSW=2e-10"
                ));
                let selection = mapping
                    .select_card(&lines[0], &[], &WorkBudget::default())?
                    .context("selection")?;
                let bound = selection.bind_hspice_bsim3_junctions(
                    HspiceJunctionSourceOptions {
                        nominal_temperature_k: 298.15,
                        model_scale: 1.,
                        aspec: false,
                        update: 0,
                    },
                    |raw| Ok(parse_spice_constant(raw)?),
                )?;
                let [drain, source] = bound.prepare(input)?;
                assert!(matches!(drain, PreparedMosJunction::BerkeleyBsim3(_)));
                assert!(matches!(source, PreparedMosJunction::BerkeleyBsim3(_)));
                close(drain.resistance_ohm(), drain_r);
                // Width=9um, light path=0.7um, RS=60 and RSC=3.
                let source_r = if acm == 10 {
                    source_r
                } else {
                    60. * 0.7 / 9. + 3.
                };
                close(source.resistance_ohm(), source_r);
                let drain_zero = drain.evaluate(0.)?;
                let source_zero = source.evaluate(0.)?;
                if acm == 12 && calcacm == 1 {
                    close(
                        drain_zero.capacitance_f,
                        7.68e-12 * 5.79e-4 + 24.4e-6 * 2e-10,
                    );
                    close(
                        source_zero.capacitance_f,
                        28.8e-12 * 5.79e-4 + 24.4e-6 * 2e-10,
                    );
                    close(source.evaluate(0.3)?.current_a, 0.);
                } else {
                    close(drain_zero.capacitance_f, 12e-12 * 5.79e-4);
                    close(source_zero.capacitance_f, 0.);
                    assert!(source.evaluate(0.3)?.current_a > 0.);
                }
                close(drain.evaluate(0.3)?.current_a, 0.);
                close(drain_zero.charge_c, 0.);
            }
        }
    }
    let lines = statements(
        ".model m NMOS LEVEL=53 VERSION=3.3 ACM=10 CJ='local_cj' JS=0 PB=0.8 CJSW=1e-10 CJSWG=3e-10",
    );
    let selection = mapping
        .select_card(&lines[0], &[], &WorkBudget::default())?
        .context("selection")?;
    let [drain, _source] = selection
        .bind_hspice_bsim3_junctions(
            HspiceJunctionSourceOptions {
                nominal_temperature_k: 300.15,
                model_scale: 1.,
                aspec: false,
                update: 0,
            },
            |raw| {
                if raw.trim_matches(['\'', '"']) == "local_cj" {
                    Ok(0.002)
                } else {
                    Ok(parse_spice_constant(raw)?)
                }
            },
        )?
        .prepare(HspiceBsim3JunctionInput {
            diffusion: [
                MosDiffusionTerminal {
                    area_m2: Some(2e-12),
                    perimeter_m: Some(10e-6),
                    shared: false,
                },
                MosDiffusionTerminal::default(),
            ],
            ..input
        })?;
    close(
        drain.evaluate(0.)?.capacitance_f,
        2e-12 * 0.002 + 2e-6 * 1e-10 + 8e-6 * 3e-10,
    );
    for suffix in ["ACM=10 PB=0.8 PHIB=0.9", "ACM=10 CJSW=1e-10 CJSW=2e-10"] {
        let lines = statements(&format!(".model m NMOS LEVEL=53 VERSION=3.3 {suffix}"));
        let selection = mapping
            .select_card(&lines[0], &[], &WorkBudget::default())?
            .context("selection")?;
        assert!(
            selection
                .bind_hspice_bsim3_junctions(
                    HspiceJunctionSourceOptions {
                        nominal_temperature_k: 300.15,
                        model_scale: 1.,
                        aspec: false,
                        update: 0
                    },
                    |raw| Ok(parse_spice_constant(raw)?)
                )
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn hspice_berkeley_junction_binding_converts_explicit_model_units() -> Result<()> {
    use model_compact_intrinsic::MosDiffusionTerminal;
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_simplis::source_dialect::model_mapping::{
        HspiceBsim3JunctionInput, HspiceJunctionSourceOptions,
    };

    let mapping = table(".options HSPICEMODELS=1")?;
    let input = HspiceBsim3JunctionInput {
        drawn_width_m: 10e-6,
        gate_width_m: 8e-6,
        diffusion: [MosDiffusionTerminal::default(); 2],
        squares: [None, Some(0.)],
        default_squares: [2.; 2],
        operating_temperature_k: 325.,
    };
    let prefix = ".model m NMOS LEVEL=53 VERSION=3.3 ACM=12 CALCACM=1 WMLT=0.8 RD=90 RS=60 RDC=3 RSC=3 RSH=45 TNOM=27";
    let bind = |suffix: &str, global_scale: f64| -> Result<_> {
        let lines = statements(&format!("{prefix} {suffix}"));
        let selection = mapping
            .select_card(&lines[0], &[], &WorkBudget::default())?
            .context("selection")?;
        selection
            .bind_hspice_bsim3_junctions(
                HspiceJunctionSourceOptions {
                    nominal_temperature_k: 298.15,
                    model_scale: global_scale,
                    aspec: false,
                    update: 0,
                },
                |raw| Ok(parse_spice_constant(raw)?),
            )?
            .prepare(input)
    };
    for (si, scaled) in [
        (
            "XW=1u HDIF=2u LD=0.2u LDIF=0.5u",
            "XW=1 HDIF=2 LD=0.2 LDIF=0.5",
        ),
        (
            "XW=1u HDIF=2u LD=0.2u LDIF=0.5u CJ=2e-3 CJSW=1e-10 JS=1e-4 JSW=2e-8",
            "XW=1 HDIF=2 LD=0.2 LDIF=0.5 CJ=2e-15 CJSW=1e-16 JS=1e-16 JSW=2e-14",
        ),
    ] {
        let physical = bind(si, 1.)?;
        for converted in [
            bind(scaled, 1e-6)?,
            bind(&format!("SCALM=1u {scaled}"), 1.)?,
        ] {
            for (reference, actual) in physical.iter().zip(converted) {
                for voltage in [-0.5, 0., 0.3, 0.7] {
                    let a = reference.evaluate(voltage)?;
                    let b = actual.evaluate(voltage)?;
                    for (left, right) in [
                        (a.current_a, b.current_a),
                        (a.conductance_s, b.conductance_s),
                        (a.capacitance_f, b.capacitance_f),
                        (a.charge_c, b.charge_c),
                        (reference.resistance_ohm(), actual.resistance_ohm()),
                    ] {
                        assert!(
                            (left - right).abs() <= 2e-13 * left.abs().max(1e-30),
                            "{left:e} != {right:e}"
                        );
                    }
                }
            }
        }
    }
    assert!(bind("SCALM=0", 1.).is_err());
    assert!(bind("SCALM=-1", 1.).is_err());
    Ok(())
}

#[test]
fn hspice_linear_reverse_junctions_bind_source_laws_and_geometry() -> Result<()> {
    use model_compact_intrinsic::{MosDiffusionTerminal, PreparedMosJunction};
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_simplis::source_dialect::model_mapping::{
        HspiceBsim3JunctionInput, HspiceJunctionSourceOptions,
    };

    const TNOM: f64 = 300.15;
    const TEMP: f64 = 350.15;
    const K: f64 = 8.617_087e-5;
    fn close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 5e-13 * expected.abs().max(actual.abs()).max(1e-30),
            "{actual:e} != {expected:e}"
        );
    }
    let mapping = table(".options HSPICEMODELS=1")?;
    let options = HspiceJunctionSourceOptions {
        nominal_temperature_k: 298.15,
        model_scale: 1.,
        aspec: false,
        update: 0,
    };
    let input = HspiceBsim3JunctionInput {
        drawn_width_m: 10e-6,
        gate_width_m: 2e-6,
        diffusion: [
            MosDiffusionTerminal {
                area_m2: None,
                perimeter_m: None,
                shared: true,
            },
            MosDiffusionTerminal::default(),
        ],
        squares: [None, Some(0.)],
        default_squares: [2.; 2],
        operating_temperature_k: TEMP,
    };
    let eg = |t| 1.16 - 7.02e-4 * t * t / (t + 1108.);
    let scale =
        ((eg(TNOM) / (K * TNOM) - eg(TEMP) / (K * TEMP) + 2. * (TEMP / TNOM).ln()) / 1.5).exp();
    for (level, version) in [(49, "3.24"), (53, "3.3")] {
        for (acm, extent, perim, side, gate, drain_r, source_r) in [
            (0, 0., 0., 0., 0., 93., 63.),
            (1, 7.2e-6, 9e-6, 9e-6, 0., 100., 60. * 0.7 / 9. + 3.),
            (
                2,
                28.8e-12,
                24.4e-6,
                15.4e-6,
                9e-6,
                18.,
                60. * 0.7 / 9. + 3.,
            ),
            (3, 14.4e-12, 3.2e-6, 3.2e-6, 9e-6, 18., 60. * 0.7 / 9. + 3.),
        ] {
            let (js, cj) = if acm == 1 { (2e-8, 2e-10) } else { (0.1, 1e-3) };
            let lines = statements(&format!(
                ".model m NMOS LEVEL={level} VERSION={version} ACM={acm} WMLT=0.8 XW=1u HDIF=2u LD=0.2u LDIF=0.5u RD=90 RS=60 RDC=3 RSC=3 RSH=45 TNOM=27 N=1.5 NJ=0.25 XTI=2 JS={js} JSW=3e-8 IS=5e-14 CJ={cj} CBD=6e-14 CBS=7e-14 CJSW=3e-10 CJGATE=4e-10 CJSWG=1 PHP=0.8 PBSW=0.1 PB=0.9 MJ=0.4 MJSW=0.33 TT=4n VNDS=-0.7 NDS=5 TLEVC=1 CTA=0.001 PTA=0.0007 CTP=0.002 PTP=0.0005 TRD=0.002 TRS=0.001"
            ));
            let selection = mapping
                .select_card(&lines[0], &[], &WorkBudget::default())?
                .context("selection")?;
            let [drain, source] = selection
                .bind_hspice_bsim3_junctions(options, |raw| Ok(parse_spice_constant(raw)?))?
                .prepare(input)?;
            assert!(matches!(drain, PreparedMosJunction::LinearReverse(_)));
            assert!(matches!(source, PreparedMosJunction::LinearReverse(_)));
            close(drain.resistance_ohm(), drain_r * 1.1);
            close(source.resistance_ohm(), source_r * 1.05);
            let weighted = js * extent + 3e-8 * perim;
            let isat = if weighted > 0. { weighted } else { 5e-14 } * scale;
            let bottom = if acm == 0 { 6e-14 } else { cj * extent } * 1.05;
            let sidewall = (3e-10 * side + 4e-10 * gate) * 1.1;
            let reverse = drain.evaluate(-2.)?;
            close(reverse.current_a, isat * (-0.7 - 1.3 / 5.));
            close(reverse.conductance_s, isat / 5.);
            let zero = drain.evaluate(0.)?;
            close(
                zero.capacitance_f,
                bottom + sidewall + 4e-9 * isat / (1.5 * K * TEMP),
            );
            let forward = drain.evaluate(0.3)?;
            close(forward.current_a, isat * (0.3 / (1.5 * K * TEMP)).exp_m1());
            close(
                forward.capacitance_f,
                bottom * (1. + 0.4 * 0.3 / 0.865)
                    + sidewall * (1. + 0.33 * 0.3 / 0.775)
                    + 4e-9 * forward.conductance_s,
            );
            if acm == 0 {
                close(
                    source.evaluate(0.)?.capacitance_f,
                    7e-14 * 1.05 + 4e-9 * isat / (1.5 * K * TEMP),
                );
            }
        }
    }
    Ok(())
}

#[test]
fn hspice_linear_reverse_source_options_and_unit_scaling() -> Result<()> {
    use model_compact_intrinsic::MosDiffusionTerminal;
    use model_normalize_ngspice::model_card::parse_spice_constant;
    use model_preprocess_simplis::source_dialect::model_mapping::{
        HspiceBsim3JunctionInput, HspiceJunctionSourceOptions,
    };

    let mapping = table(".options HSPICEMODELS=1")?;
    let options = HspiceJunctionSourceOptions {
        nominal_temperature_k: 300.15,
        model_scale: 1.,
        aspec: false,
        update: 0,
    };
    let input = HspiceBsim3JunctionInput {
        drawn_width_m: 10e-6,
        gate_width_m: 8e-6,
        diffusion: [MosDiffusionTerminal::default(); 2],
        squares: [None, Some(0.)],
        default_squares: [2.; 2],
        operating_temperature_k: 340.,
    };
    let bind = |suffix: &str, settings: HspiceJunctionSourceOptions| -> Result<_> {
        let lines = statements(&format!(".model m NMOS LEVEL=49 VERSION=3.24 {suffix}"));
        let selection = mapping
            .select_card(&lines[0], &[], &WorkBudget::default())?
            .context("selection")?;
        selection
            .bind_hspice_bsim3_junctions(settings, |raw| Ok(parse_spice_constant(raw)?))?
            .prepare(input)
    };
    let aspec = HspiceJunctionSourceOptions {
        aspec: true,
        update: 1,
        ..options
    };
    let ordinary = bind("ACM=0 CJ=0", options)?;
    let absent = bind("ACM=0 CJ=0", aspec)?;
    assert!(ordinary[0].evaluate(0.2)?.current_a > 0.);
    assert_eq!(absent[0].evaluate(0.2)?.current_a, 0.);
    assert_eq!(absent[0].evaluate(0.)?.capacitance_f, 0.);
    for acm in [1, 11] {
        let fields = format!("ACM={acm} RD=30 RS=40 RSH=5 RDC=3 RSC=4 CJ=0");
        let contact = bind(&fields, aspec)?;
        let old_update = bind(&fields, HspiceJunctionSourceOptions { update: 0, ..aspec })?;
        assert_eq!(contact[0].resistance_ohm(), 43.);
        assert_eq!(contact[1].resistance_ohm(), 44.);
        assert_eq!(old_update[0].resistance_ohm(), 13.);
        assert_eq!(old_update[1].resistance_ohm(), 4.);
        let thermal = bind(&format!("{fields} TRD=0.002 TRS=-0.001"), aspec)?;
        let delta = input.operating_temperature_k - options.nominal_temperature_k;
        assert!((thermal[0].resistance_ohm() - 43. * (1. + 0.002 * delta)).abs() < 1e-12);
        assert!((thermal[1].resistance_ohm() - 44. * (1. - 0.001 * delta)).abs() < 1e-12);
    }
    for (acm, physical_js, physical_cj, scaled_js, scaled_cj) in [
        (1, 2e-8, 2e-10, 2e-14, 2e-16),
        (2, 0.1, 0.001, 1e-13, 1e-15),
        (3, 0.1, 0.001, 1e-13, 1e-15),
    ] {
        let physical = bind(
            &format!(
                "ACM={acm} HDIF=2u XW=1u LD=0.2u LDIF=0.5u JS={physical_js} CJ={physical_cj} JSW=3e-8 CJSW=3e-10 CJGATE=4e-10 TLEV=2 EG=0.75 GAP1=0.000456 GAP2=210 TLEVC=3 RD=90 RS=60 RSH=45"
            ),
            options,
        )?;
        let scaled = bind(
            &format!(
                "ACM={acm} SCALM=1u HDIF=2 XW=1 LD=0.2 LDIF=0.5 JS={scaled_js} CJ={scaled_cj} JSW=3e-14 CJSW=3e-16 CJGATE=4e-16 TLEV=2 EG=0.75 GAP1=0.000456 GAP2=210 TLEVC=3 RD=90 RS=60 RSH=45"
            ),
            options,
        )?;
        for (a, b) in physical.iter().zip(scaled) {
            for bias in [-2., -0.5, 0., 0.4] {
                let a = a.evaluate(bias)?;
                let b = b.evaluate(bias)?;
                for (left, right) in [
                    (a.current_a, b.current_a),
                    (a.conductance_s, b.conductance_s),
                    (a.charge_c, b.charge_c),
                    (a.capacitance_f, b.capacitance_f),
                ] {
                    assert!(
                        (left - right).abs() <= 5e-13 * left.abs().max(right.abs()).max(1e-30),
                        "{left:e} != {right:e}"
                    );
                }
            }
        }
    }
    for suffix in [
        "ACM=0 TLEV=4",
        "ACM=0 TLEVC=4",
        "ACM=0 N=0",
        "ACM=0 NDS=0",
        "ACM=0 CJ=1e-3 CJA=2e-3",
        "ACM=0 TLEV=2 GAP2=-1",
    ] {
        assert!(bind(suffix, options).is_err(), "{suffix}");
    }
    Ok(())
}

#[test]
fn hspice_bsim4_occurrence_modifiers_preserve_scope_bins_and_revisions() -> Result<()> {
    use model_compact_intrinsic::{Bsim4OccurrenceModifiers, Bsim4PrimaryCoefficients};
    use model_normalize_ngspice::model_card::{instance_fields, parse_spice_constant};

    let mapping = table(".options HSPICEMODELS=1")?;
    let instance = instance_fields(
        "RDC=12 RSC=-8 DELVTO=shift MULU0=1.5 DELK1=-0.5 DELNFCT=0.25",
        3,
    )?;
    for version in ["4.5", "4.6.4", "4.70"] {
        let lines = statements(&format!(
            ".model m PMOS LEVEL=54 VERSION={version} TRD=shift TRS=-0.001"
        ));
        let selection = mapping
            .select_card(&lines[0], &[], &WorkBudget::default())?
            .context("selection")?;
        let modifiers = selection.bind_hspice_bsim4_occurrence_modifiers(
            &instance,
            |raw| {
                if raw == "shift" {
                    Ok(0.002)
                } else {
                    Ok(parse_spice_constant(raw)?)
                }
            },
            |raw| {
                if raw == "shift" {
                    Ok(0.1)
                } else {
                    Ok(parse_spice_constant(raw)?)
                }
            },
        )?;
        assert_eq!(modifiers.series.temperature_per_k, [0.002, -0.001]);
        assert_eq!(modifiers.series.contact_per_finger_ohm, [12., -8.]);
        assert_eq!(modifiers.channel.threshold_shift_v, 0.1);
        assert_eq!(modifiers.channel.mobility_multiplier, 1.5);
        assert_eq!(modifiers.channel.subthreshold_factor_shift, 0.25);
        for k1 in [0.4, 0.8] {
            let modified = modifiers.channel.apply(Bsim4PrimaryCoefficients {
                threshold_voltage_v: -0.7,
                mobility_m2_per_vs: 0.04,
                body_effect_sqrt_v: k1,
                subthreshold_factor: 1.,
            })?;
            assert_eq!(modified.resets.body_effect_shift, k1 < 0.5);
            assert!((modified.coefficients.threshold_voltage_v + 0.6).abs() < 1e-15);
        }
        let series = modifiers.series.prepare([3., 4.], 4., 300., 350.)?;
        assert_eq!(series.reset_contact, [false, true]);
        assert!((series.resistance_ohm[0] - 6.6).abs() < 1e-13);
        assert!((series.resistance_ohm[1] - 3.8).abs() < 1e-13);
        for bad in [
            "DELVTO=0.1 DELVT0=0.2",
            "MULU0=1 MULU0=2",
            "RDC",
            "DELK1=missing",
        ] {
            let fields = instance_fields(bad, 3)?;
            assert!(
                selection
                    .bind_hspice_bsim4_occurrence_modifiers(
                        &fields,
                        |_| Ok(0.),
                        |raw| Ok(parse_spice_constant(raw)?)
                    )
                    .is_err(),
                "{bad}"
            );
        }
    }
    let lines = statements(".model m NMOS LEVEL=54 VERSION=4.5");
    let selection = mapping
        .select_card(&lines[0], &[], &WorkBudget::default())?
        .context("selection")?;
    assert_eq!(
        selection.bind_hspice_bsim4_occurrence_modifiers(&[], |_| Ok(0.), |_| Ok(0.))?,
        Bsim4OccurrenceModifiers::default()
    );
    for model in [
        ".model m NMOS LEVEL=54",
        ".model m NMOS LEVEL=54 VERSION=4.80",
        ".model m NMOS LEVEL=14 VERSION=4.5",
        ".model m NMOS LEVEL=49 VERSION=3.24",
    ] {
        let lines = statements(model);
        let selection = mapping
            .select_card(&lines[0], &[], &WorkBudget::default())?
            .context("selection")?;
        assert!(
            selection
                .bind_hspice_bsim4_occurrence_modifiers(&[], |_| Ok(0.), |_| Ok(0.))
                .is_err(),
            "{model}"
        );
    }
    Ok(())
}
