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
    let topology = source.instantiate(Default::default())?;
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
