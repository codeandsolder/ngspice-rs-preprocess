mod prepared_diode;

use anyhow::{Context, Result, ensure};
use model_preprocess_simplis::source_dialect::model_mapping::{Device, SelectionOrigin};
use ngspice_rs_preprocess::analysis_plan::{Directive, Disposition};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, LibraryInput, Options, ParsedLibrary, PreparedBundle, Request,
    SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn request(path: &Path, format: InputFormat) -> Request {
    Request {
        format,
        input: SourceInput::File(path.to_owned()),
        libraries: Vec::new(),
        search_paths: Vec::new(),
    }
}

fn execute(bundle: &PreparedBundle) -> Result<f64> {
    let dir = tempfile::tempdir()?;
    for artifact in &bundle.companions {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("no deck terminator")?
        .to_vec();
    deck.extend_from_slice(b".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata current.dat i(vbias)\nquit\n.endc\n.end\n");
    fs::write(dir.path().join("deck.cir"), &deck)?;
    let result = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "deck.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    ensure!(
        result.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}"
    );
    Ok(fs::read_to_string(dir.path().join("current.dat"))?
        .split_ascii_whitespace()
        .nth(1)
        .context("missing native current")?
        .parse()?)
}

// The selected level-1 law is no longer an ordinary ngspice D card.
// Evaluate the prepared declaration with its actual kernel, while the
// explicitly mapped level-3 case continues using the independent C oracle.
fn execute_level1(bundle: &PreparedBundle) -> Result<f64> {
    use model_kernel_diode::{Instance, MODEL_FAMILY, compile_simetrix};
    use model_normalize_ngspice::model_card::{parse_model_cards, parse_spice_constant};
    let cards = parse_model_cards(&bundle.primary.bytes)?;
    let [model] = cards.as_slice() else {
        anyhow::bail!("expected one model declaration")
    };
    ensure!(
        model.family.eq_ignore_ascii_case(MODEL_FAMILY),
        "lost prepared model identity"
    );
    let values = model
        .parameters
        .iter()
        .map(|parameter| {
            Ok((
                parameter.name.as_str(),
                parse_spice_constant(parameter.expression.as_deref().context("missing value")?)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let device = compile_simetrix(values)?.bind(Instance {
        area: 10.,
        ..Instance::default()
    })?;
    Ok(-device.evaluate(-5.)?.current)
}

#[test]
fn map_selects_physical_backend_and_does_not_leak_between_decks() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let content = "* diode breakdown\nVbias a 0 -5\nD1 a 0 dtest AREA=10\n.model dtest D(LEVEL=1 IS=1p BV=5 IBV=100u)\n";
    fs::write(&root, format!("{content}.end\n"))?;
    let req = request(&root, InputFormat::SimetrixDeck);
    let baseline = prepare(&req, &Options::default(), &Cancellation::default())?;
    fs::write(
        &root,
        format!("{content}.MAP MODELNAME=D LEVEL=1 DEVICE=Diode3\n.end\n"),
    )?;
    let mapped = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(mapped.model_selections[0].mapping.device, Device::Diode3);
    // At -BV, SIMetrix level 1 adds AREA*IBV to reverse transport
    // leakage and gmin. Native level 3 uses a different avalanche law.
    assert!((execute_level1(&baseline)? - (1e-3 + 10e-12 + 5e-12)).abs() < 1e-15);
    assert!((execute(&mapped)? - 1e-3).abs() < 2e-8);
    assert_ne!(baseline.primary.bytes, mapped.primary.bytes);
    fs::write(&root, format!("{content}.OPTIONS HSPICEMODELS=1\n.end\n"))?;
    let preset = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(
        preset.model_selections[0].mapping.origin,
        SelectionOrigin::HspicePreset
    );
    assert!((execute(&preset)? / execute(&mapped)? - 1.).abs() < 1e-10);
    let plan = preset
        .analysis_plan
        .as_ref()
        .context("missing analysis plan")?;
    plan.validate_supported()?;
    let option = plan
        .entries
        .iter()
        .find_map(|entry| {
            if let Directive::Options { settings } = &entry.directive {
                settings
                    .iter()
                    .find(|setting| setting.name == "HSPICEMODELS")
            } else {
                None
            }
        })
        .context("missing source selection in analysis plan")?;
    assert_eq!(option.disposition, Disposition::ModelSelection);
    assert!(!std::str::from_utf8(&preset.primary.bytes)?.contains("HSPICEMODELS"));
    fs::write(&root, format!("{content}.end\n"))?;
    let repeat = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(repeat.model_selections[0].mapping.device, Device::Diode1);
    assert_eq!(repeat.primary.bytes, baseline.primary.bytes);
    Ok(())
}

#[test]
fn included_and_precompiled_libraries_share_deck_mapping() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let lib = dir.path().join("parts.lib");
    let remap = dir.path().join("selection.inc");
    let body = "*#HSPICE\n.model selected D ( LEVEL=1 IS=1p\n+ BV=5 IBV=100u )\n";
    fs::write(&lib, body)?;
    fs::write(&remap, ".map ModelName=D Level=1 Device=Diode3\n")?;
    fs::write(
        &root,
        "* test\n.inc 'parts.lib'\n.inc 'selection.inc'\nVbias a 0 -5\nD1 a 0 selected 10\n.end\n",
    )?;
    let req = request(&root, InputFormat::SimetrixDeck);
    let included = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(included.model_selections[0].mapping.device, Device::Diode3);
    assert!(included.model_selections[0].source.ends_with("parts.lib"));
    assert!(included.model_selections[0].original.contains("LEVEL=1"));
    assert_eq!(included.model_selections[0].card.source_line, 2);
    assert_eq!(included.model_selections[0].last_line, 3);
    let compiled = prepare(
        &request(&lib, InputFormat::SimetrixLibrary),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let parsed = ParsedLibrary::from_json(&compiled.primary.bytes)?;
    fs::write(
        &root,
        "* test\n.inc 'selection.inc'\nVbias a 0 -5\nD1 a 0 selected 10\n.end\n",
    )?;
    let mut req = request(&root, InputFormat::SimetrixDeck);
    req.libraries.push(LibraryInput::Parsed(parsed));
    let cached = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(cached.model_selections[0].mapping.device, Device::Diode3);
    assert_eq!(cached.model_selections[0].card.source_line, 2);
    assert_eq!(cached.model_selections[0].last_line, 3);
    assert!((execute(&included)? / execute(&cached)? - 1.).abs() < 1e-10);
    Ok(())
}

#[test]
fn device_configuration_is_explicit_and_overridden_by_deck_map() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let config = dir.path().join("devices.sxdcf");
    fs::write(&config, "ModelName=D Level=1 Device=Diode3\n")?;
    fs::write(
        &root,
        "* test\nVbias a 0 -5\nD1 a 0 selected 10\n.model selected D LEVEL=1 IS=1p BV=5 IBV=100u\n.end\n",
    )?;
    let options = Options {
        device_config: Some(config),
        ..Options::default()
    };
    let req = request(&root, InputFormat::SimetrixDeck);
    let configured = prepare(&req, &options, &Cancellation::default())?;
    assert_eq!(
        configured.model_selections[0].mapping.origin,
        SelectionOrigin::DeviceConfiguration
    );
    let fresh = prepare(&req, &Options::default(), &Cancellation::default())?;
    assert_eq!(fresh.model_selections[0].mapping.device, Device::Diode1);
    assert!((execute_level1(&fresh)? - (1e-3 + 10e-12 + 5e-12)).abs() < 1e-15);
    assert!((execute(&configured)? - 1e-3).abs() < 2e-8);
    assert_ne!(configured.primary.bytes, fresh.primary.bytes);
    fs::write(
        &root,
        "* test\nVbias a 0 -5\nD1 a 0 selected 10\n.model selected D LEVEL=1 IS=1p BV=5 IBV=100u\n.map ModelName=D Level=1 Device=Diode1\n.end\n",
    )?;
    let mapped = prepare(&req, &options, &Cancellation::default())?;
    assert_eq!(
        mapped.model_selections[0].mapping.origin,
        SelectionOrigin::NetlistMap
    );
    assert_eq!(mapped.model_selections[0].mapping.device, Device::Diode1);
    Ok(())
}

#[test]
fn mos_and_bjt_aliases_select_the_canonical_native_implementation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    for (body, family, level, target, letter, device) in [
        (
            "Vbias d 0 2\nVgate g 0 1.5\nM1 d g 0 0 selected L=1u W=10u\n",
            "NMOS",
            3,
            "MOS3",
            'M',
            Device::Mos3,
        ),
        (
            "Vbias c 0 2\nVbase b 0 0.7\nQ1 c b 0 selected\n",
            "NPN",
            4,
            "VBIC",
            'Q',
            Device::Vbic,
        ),
    ] {
        let fields = if letter == 'M' {
            "VTO=0.5 KP=100u"
        } else {
            "IS=1e-16 RCX=1 RBX=1 RE=1"
        };
        let req = request(&root, InputFormat::SimetrixDeck);
        fs::write(
            &root,
            format!(
                "* model selection\n{body}.model selected {family} (LEVEL={level} {fields})\n.end\n"
            ),
        )?;
        let canonical = prepare(&req, &Options::default(), &Cancellation::default())?;
        fs::write(
            &root,
            format!(
                "* model selection\n{body}.model selected {family} (LEVEL=69 {fields})\n.map ModelName={family} Level=69 Device={target} Letter={letter}\n.end\n"
            ),
        )?;
        let mapped = prepare(&req, &Options::default(), &Cancellation::default())?;
        assert_eq!(mapped.model_selections[0].mapping.device, device);
        assert_eq!(mapped.model_selections[0].declared_level, Some(69));
        let expected = execute(&canonical)?;
        let actual = execute(&mapped)?;
        assert!(expected.abs() > 1e-8, "nontrivial operating point");
        assert!(
            (actual / expected - 1.).abs() < 1e-10,
            "{actual} != {expected}"
        );
    }
    Ok(())
}

#[test]
fn indexed_library_cannot_change_the_global_device_mapping() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let lib = dir.path().join("parts.lib");
    fs::write(&root, "* test\nVbias a 0 -5\nD1 a 0 selected 10\n.end\n")?;
    fs::write(
        &lib,
        ".model selected D LEVEL=1\n.map ModelName=D Level=1 Device=Diode3\n",
    )?;
    let mut req = request(&root, InputFormat::SimetrixDeck);
    req.libraries
        .push(LibraryInput::Source(SourceInput::File(lib)));
    let error = prepare(&req, &Options::default(), &Cancellation::default())
        .err()
        .context("indexed global map accepted")?;
    assert!(format!("{error:#}").contains("global .MAP"));
    Ok(())
}

#[test]
fn discrete_mos_model_selection_and_linked_binding_survive_compilation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let lib = dir.path().join("parts.lib");
    // Level 7 with a discrete charge signature must select Level 17.
    fs::write(
        &lib,
        "*#SIMETRIX\n.model selected NMOS (LEVEL=7 VTO=1 KP=.1 CGDMIN=1n LIMIT(VDS)=(*, 400))\n",
    )?;
    fs::write(
        &root,
        "* test\nVbias d 0 1\nVgate g 0 3\nM1 d g 0 0 selected M=3\n.end\n",
    )?;
    let req = request(&root, InputFormat::SimetrixDeck);
    let mut raw = req.clone();
    raw.libraries
        .push(LibraryInput::Source(SourceInput::File(lib.clone())));
    let admitted = prepare(&raw, &Options::default(), &Cancellation::default())?;
    let compiled = prepare(
        &request(&lib, InputFormat::SimetrixLibrary),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let mut cached = req;
    cached
        .libraries
        .push(LibraryInput::Parsed(ParsedLibrary::from_json(
            &compiled.primary.bytes,
        )?));
    let restored = prepare(&cached, &Options::default(), &Cancellation::default())?;
    for bundle in [&admitted, &restored] {
        let selection = bundle
            .model_selections
            .iter()
            .find(|selection| selection.card.name.eq_ignore_ascii_case("selected"))
            .context("missing model selection")?;
        assert_eq!(selection.mapping.device, Device::Mos17);
        assert_eq!(
            selection.mapping.origin,
            SelectionOrigin::LegacyMos17Signature
        );
        assert_eq!(selection.declared_level, Some(7));
        assert!(
            selection
                .card
                .parameters
                .iter()
                .any(|parameter| parameter.name.eq_ignore_ascii_case("LIMIT(VDS)"))
        );
        let current = execute(bundle)?;
        assert!(
            (current + 0.45).abs() < 1e-8,
            "linked Level 17 current {current}"
        );
    }
    Ok(())
}

#[test]
fn prepared_diode_identity_survives_local_models_and_source_parameter_expressions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(
        &root,
        "* local diode models
.param sat=3p
.model same D(IS={sat} RS=1)
D0 a 0 same
X1 a 0 cell params: local_is=2p
X2 b 0 cell params: local_is=7p
.subckt cell p n params: local_is=1p
D1 p n same
.model same D(IS {local_is} RS 2 T_ABS={27+1})
.ends
.end
",
    )?;
    let bundle = prepare(
        &request(&root, InputFormat::SimetrixDeck),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let source = prepared_diode::linked_library(&bundle)?;
    let topology = source
        .instantiate(model_normalize_ngspice::parameter_scope::topology::Limits::default())?;
    let models = source
        .models
        .iter()
        .filter(|m| {
            m.card
                .family
                .eq_ignore_ascii_case(model_kernel_diode::MODEL_FAMILY)
        })
        .collect::<Vec<_>>();
    assert_eq!(models.len(), 2, "{text}");
    let mut currents = Vec::new();
    for frame in &topology.frames {
        let model = models
            .iter()
            .find(|model| model.scope == frame.declaration)
            .context("missing scoped prepared diode")?;
        let overrides = frame
            .overrides
            .iter()
            .map(|(name, value)| (frame.declaration, name.as_str(), *value))
            .collect::<Vec<_>>();
        let values = model
            .card
            .parameters
            .iter()
            .map(|p| {
                Ok((
                    p.name.as_str(),
                    source.evaluate_scalar(
                        model.scope,
                        p.expression.as_deref().context("missing model value")?,
                        &overrides,
                    )?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let diode = model_kernel_diode::compile_simetrix(values)?
            .bind(model_kernel_diode::Instance::default())?;
        currents.push(diode.evaluate(0.5)?.current);
    }
    assert_eq!(currents.len(), 3, "{text}");
    assert!(
        currents[1] < currents[0] && currents[0] < currents[2],
        "{currents:?}"
    );
    Ok(())
}

#[test]
fn invalid_selected_diode_fields_fail_without_native_fallback() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    for fields in [
        "LEVEL=2",
        "LEVEL=1 N=0",
        "LEVEL=1 T_ABS=-300",
        "LEVEL=1 UNKNOWN_PHYSICS=1",
    ] {
        fs::write(
            &root,
            format!(
                "* rejected model
D1 a 0 dm
.model dm D({fields})
.end
"
            ),
        )?;
        assert!(
            prepare(
                &request(&root, InputFormat::SimetrixDeck),
                &Options::default(),
                &Cancellation::default()
            )
            .is_err(),
            "{fields}"
        );
    }
    Ok(())
}

#[test]
fn classic_bjt_selector_preserves_source_models_and_native_input() -> Result<()> {
    use model_normalize_ngspice::bjt_card::{GUMMEL_POON_LEGACY_NPN, GUMMEL_POON_LEGACY_PNP};
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let content = "* source BJT selection\n.temp 85\n.param sat=3p\n.model outer NPN(IS={sat} BF=110 TNOM=27)\nQ1 c b 0 outer TEMP=50\nQ2 c b 0 bipolar\n.model bipolar NPN(LEVEL=4 IS=1e-16)\nX1 c b 0 cell\n.subckt cell c b e\n.model outer PNP(LEVEL=1 IS=4p BF=120 T_MEASURED=29 T_ABS={TEMP-20})\nQ3 c b e outer\n.ends\n.end\n";
    fs::write(&root, content)?;
    let bundle = prepare(
        &request(&root, InputFormat::SimetrixDeck),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    let models = prepared_diode::linked_library(&bundle)?;
    let root_model = models
        .models
        .iter()
        .find(|m| m.scope == 0 && m.card.family.eq_ignore_ascii_case(GUMMEL_POON_LEGACY_NPN))
        .context("missing root legacy NPN")?;
    let local_model = models
        .models
        .iter()
        .find(|m| m.scope != 0 && m.card.family.eq_ignore_ascii_case(GUMMEL_POON_LEGACY_PNP))
        .context("missing local legacy PNP")?;
    assert!(
        root_model
            .card
            .parameters
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case("TNOM") && p.expression.as_deref() == Some("27"))
    );
    assert!(
        local_model
            .card
            .parameters
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case("TNOM") && p.expression.as_deref() == Some("29"))
    );
    assert!(models.models.iter().any(|m| {
        m.card.family.eq_ignore_ascii_case("NPN")
            && m.card.parameters.iter().any(|p| {
                p.name.eq_ignore_ascii_case("LEVEL") && p.expression.as_deref() == Some("4")
            })
    }));
    assert!(text.contains("TEMP=50"), "{text}");
    assert!(!text.to_ascii_uppercase().contains("T_ABS"), "{text}");
    assert_eq!(
        bundle
            .model_selections
            .iter()
            .filter(|s| s.mapping.device == Device::GummelPoonLegacy)
            .count(),
        2
    );
    assert!(
        bundle
            .model_selections
            .iter()
            .any(|s| s.mapping.device == Device::GummelPoonLegacy
                && s.scope == ["cell"]
                && s.original.contains("T_MEASURED=29"))
    );
    let repeated = prepare(
        &request(&root, InputFormat::SimetrixDeck),
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert_eq!(bundle.primary.bytes, repeated.primary.bytes);
    assert_eq!(bundle.model_selections, repeated.model_selections);
    let native = prepare(
        &request(&root, InputFormat::NativeDeck),
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert!(native.model_selections.is_empty());
    assert!(!std::str::from_utf8(&native.primary.bytes)?.contains("gummel_poon_legacy"));
    Ok(())
}

#[test]
fn classic_bjt_alias_retains_declared_polarity_in_raw_and_parsed_libraries() -> Result<()> {
    use model_normalize_ngspice::bjt_card::GUMMEL_POON_LEGACY_PNP;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let lib = dir.path().join("parts.lib");
    fs::write(
        &lib,
        "*#HSPICE\n.model base PNP(LEVEL=1 IS=2p TNOM=28)\n.alias selected base PNP\n.subckt unused c b e\n.model base NPN(IS=3p)\nQ1 c b e base\n.ends\n",
    )?;
    fs::write(&root, "* BJT alias\nQ1 c b 0 selected\n.end\n")?;
    let parsed = prepare(
        &request(&lib, InputFormat::SimetrixLibrary),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let mut bundles = Vec::new();
    for input in [
        LibraryInput::Source(SourceInput::File(lib.clone())),
        LibraryInput::Parsed(ParsedLibrary::from_json(&parsed.primary.bytes)?),
    ] {
        let mut req = request(&root, InputFormat::SimetrixDeck);
        req.libraries.push(input);
        let bundle = prepare(&req, &Options::default(), &Cancellation::default())?;
        let linked = prepared_diode::linked_library(&bundle)?;
        let alias = linked
            .models
            .iter()
            .find(|m| m.card.name.eq_ignore_ascii_case("selected"))
            .context("missing materialized alias")?;
        assert!(
            alias
                .card
                .family
                .eq_ignore_ascii_case(GUMMEL_POON_LEGACY_PNP)
        );
        assert!(
            alias
                .card
                .parameters
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case("TNOM")
                    && p.expression.as_deref() == Some("28"))
        );
        assert!(
            bundle
                .model_selections
                .iter()
                .any(|s| s.card.name.eq_ignore_ascii_case("base")
                    && s.mapping.device == Device::GummelPoonLegacy
                    && s.scope.is_empty()
                    && s.source.ends_with("parts.lib"))
        );
        bundles.push(bundle);
    }
    assert_eq!(bundles[0].primary.bytes, bundles[1].primary.bytes);
    assert_eq!(bundles[0].model_selections, bundles[1].model_selections);
    assert_eq!(
        bundles[0]
            .companions
            .iter()
            .map(|a| (&a.filename, &a.bytes))
            .collect::<Vec<_>>(),
        bundles[1]
            .companions
            .iter()
            .map(|a| (&a.filename, &a.bytes))
            .collect::<Vec<_>>()
    );
    fs::write(&lib, ".model base PNP(IS=2p)\n.alias selected base NPN\n")?;
    let mut req = request(&root, InputFormat::SimetrixDeck);
    req.libraries
        .push(LibraryInput::Source(SourceInput::File(lib)));
    assert!(prepare(&req, &Options::default(), &Cancellation::default()).is_err());
    Ok(())
}

#[test]
fn classic_bjt_mapping_survives_raw_and_parsed_libraries_and_native_selection() -> Result<()> {
    use model_normalize_ngspice::bjt_card::{BjtPolarity, BjtThermalConvention, ClassicBjtFamily};
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    let library = dir.path().join("bjt.lib");
    fs::write(
        &library,
        "*#HSPICE\n.subckt cell c b e params: is_local=3p\nQ1 c b e same M=2 TEMP=45\nQ2 c b e same\n.model same PNP(LEVEL=1 IS={is_local} BF=70 TNOM=35 T_ABS=50)\n.ends\n",
    )?;
    fs::write(
        &root,
        "* mixed BJT physical selection\n.temp 85\nVbias c 0 2\nVbase b 0 0.65\nQouter c b 0 same TEMP=60\n.model same NPN(IS=2p BF=100 TNOM=27)\nQexplicit c b 0 native_model DTEMP=5\n.model native_model NPN(LEVEL=70 IS=5p BF=200 TNOM=40)\n.map ModelName=NPN Level=70 Device=NGSPICE_BJT Letter=Q\nX1 c b 0 cell params: is_local=7p\n.op\n.end\n",
    )?;
    let mut raw = request(&root, InputFormat::SimetrixDeck);
    raw.libraries
        .push(LibraryInput::Source(SourceInput::File(library.clone())));
    let admitted = prepare(&raw, &Options::default(), &Cancellation::default())?;
    let compiled = prepare(
        &request(&library, InputFormat::SimetrixLibrary),
        &Options::default(),
        &Cancellation::default(),
    )?;
    let mut cached = request(&root, InputFormat::SimetrixDeck);
    cached
        .libraries
        .push(LibraryInput::Parsed(ParsedLibrary::from_json(
            &compiled.primary.bytes,
        )?));
    let restored = prepare(&cached, &Options::default(), &Cancellation::default())?;
    let selected = |bundle: &PreparedBundle| {
        bundle
            .model_selections
            .iter()
            .map(|selection| {
                (
                    &selection.card.name,
                    selection.mapping.device,
                    selection.declared_level,
                )
            })
            .map(|(name, device, level)| (name.clone(), device, level))
            .collect::<Vec<_>>()
    };
    assert_eq!(selected(&admitted), selected(&restored));
    assert_eq!(
        serde_json::to_value(&admitted.model_selections)?,
        serde_json::to_value(&restored.model_selections)?
    );
    assert_eq!(
        admitted
            .artifacts()
            .map(|a| (&a.filename, &a.bytes))
            .collect::<Vec<_>>(),
        restored
            .artifacts()
            .map(|a| (&a.filename, &a.bytes))
            .collect::<Vec<_>>()
    );
    for bundle in [&admitted, &restored] {
        let source = prepared_diode::linked_library(bundle)?;
        let top = source
            .instantiate(model_normalize_ngspice::parameter_scope::topology::Limits::default())?;
        assert_eq!(top.frames.len(), 2);
        let mut families = source
            .models
            .iter()
            .map(|model| {
                (
                    model.scope,
                    model.card.name.to_ascii_lowercase(),
                    ClassicBjtFamily::parse(&model.card.family),
                )
            })
            .collect::<Vec<_>>();
        families.sort_by_key(|(scope, name, _)| (*scope, name.clone()));
        use BjtPolarity::{Npn, Pnp};
        use BjtThermalConvention::{Legacy, Ngspice};
        assert_eq!(
            families,
            [
                (
                    0,
                    "native_model".into(),
                    Some(ClassicBjtFamily {
                        polarity: Npn,
                        thermal: Ngspice
                    })
                ),
                (
                    0,
                    "same".into(),
                    Some(ClassicBjtFamily {
                        polarity: Npn,
                        thermal: Legacy
                    })
                ),
                (
                    1,
                    "same".into(),
                    Some(ClassicBjtFamily {
                        polarity: Pnp,
                        thermal: Legacy
                    })
                )
            ]
        );
        let local = source
            .models
            .iter()
            .find(|model| model.scope == 1)
            .context("local BJT model")?;
        let current = local
            .card
            .parameters
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case("IS"))
            .and_then(|p| p.expression.as_deref())
            .context("local IS")?;
        let overrides = top.frames[1]
            .overrides
            .iter()
            .map(|(name, value)| (1, name.as_str(), *value))
            .collect::<Vec<_>>();
        assert!((source.evaluate_scalar(1, current, &overrides)? - 7e-12).abs() < 1e-25);
        assert!(
            local
                .card
                .parameters
                .iter()
                .all(|p| !p.name.eq_ignore_ascii_case("T_ABS"))
        );
        let deck = bundle
            .artifacts()
            .map(|a| String::from_utf8_lossy(&a.bytes))
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_uppercase();
        assert!(deck.contains("TEMP=45"), "{deck}");
        assert!(deck.contains("TEMP=50"), "{deck}");
        assert!(deck.contains("TEMP=60"), "{deck}");
        assert!(deck.contains("DTEMP=5"), "{deck}");
    }
    // Native source bypasses SIMetrix mapping entirely, even when its model
    // is the classic level and its instance temperature is explicit.
    let native = dir.path().join("native.cir");
    fs::write(
        &native,
        "* native\nV1 c 0 2\nQ1 c b 0 same TEMP=60\n.model same NPN(LEVEL=1 IS=2p BF=100 TNOM=27)\n.op\n.end\n",
    )?;
    let bundle = prepare(
        &request(&native, InputFormat::NativeDeck),
        &Options::default(),
        &Cancellation::default(),
    )?;
    assert!(bundle.model_selections.is_empty());
    let cards = model_normalize_ngspice::model_card::parse_model_cards(&bundle.primary.bytes)?;
    assert_eq!(
        ClassicBjtFamily::parse(&cards[0].family),
        Some(ClassicBjtFamily {
            polarity: BjtPolarity::Npn,
            thermal: BjtThermalConvention::Ngspice
        })
    );
    Ok(())
}
