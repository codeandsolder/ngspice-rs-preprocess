use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, LibraryInput, Options, ParsedLibrary, PreparedBundle, Provenance,
    Request, SourceInput, prepare,
};
use std::{fs, path::Path, process::Command};

fn run(root: &Path, vectors: &str, expected: &[f64]) -> Result<()> {
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root.to_path_buf()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    run_prepared(&bundle, vectors, expected)
}

fn run_prepared(bundle: &PreparedBundle, vectors: &str, expected: &[f64]) -> Result<()> {
    let dir = tempfile::tempdir()?;
    for artifact in &bundle.companions {
        fs::write(dir.path().join(&artifact.filename), &artifact.bytes)?;
    }
    let mut deck = bundle
        .primary
        .bytes
        .strip_suffix(b".end\n")
        .context("missing terminator")?
        .to_vec();
    deck.extend_from_slice(format!(".control\nset numdgt=17\nset wr_singlescale\nop\nwrdata op.dat {vectors}\nquit\n.endc\n.end\n").as_bytes());
    fs::write(dir.path().join("deck.cir"), &deck)?;
    let execution = Command::new("ngspice")
        .current_dir(dir.path())
        .args(["-b", "-n", "deck.cir"])
        .output()?;
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    ensure!(
        execution.status.success() && !log.to_ascii_lowercase().contains("error:"),
        "{log}\n{}",
        String::from_utf8_lossy(&deck)
    );
    let actual = fs::read_to_string(dir.path().join("op.dat"))?
        .split_ascii_whitespace()
        .skip(1)
        .map(str::parse::<f64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure!(actual.len() == expected.len(), "shape mismatch");
    for (actual, expected) in actual.into_iter().zip(expected) {
        ensure!(
            (actual - expected).abs() < 1e-10,
            "{actual} != {expected}\n{}",
            String::from_utf8_lossy(&deck)
        );
    }
    Ok(())
}

#[test]
fn nested_source_language_inherits_and_restores_with_named_sections() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path();
    fs::write(path.join("root.net"), b"* root\n*#SIMETRIX\n.param rootlog={LOG(100)} rootpow={PWR(-2,2)}\n.inc 'hs.lib' ; root comment\n.inc 'native.lib'\nV1 a 0 {rootlog+rootpow}\nV2 b 0 {hslog+hspow+grandlog+grandpow}\nV3 c 0 {pslog+pspow+afterlog+afterpow}\nV4 d 0 {native_log+native_pow}\n.end\n")?;
    fs::write(path.join("hs.lib"), b"*#HSPICE\n.param hslog='LOG(100)' hspow='PWR(-2,2)' $ child comment\n.lib 'sections.lib' tt\n.inc 'ps.lib'\n.param afterlog='LOG(100)' afterpow='PWR(-2,2)'\n")?;
    fs::write(path.join("sections.lib"), b".lib tt\n.param grandlog='LOG(100)' grandpow='PWR(-2,2)'\n.endl tt\n.lib ss\n.param grandlog=99 grandpow=99\n.endl ss\n")?;
    fs::write(
        path.join("ps.lib"),
        b"*#PSPICE\n.param pslog={LOG(100)} pspow={PWR(-2,2)} ; comment\n",
    )?;
    fs::write(
        path.join("native.lib"),
        b".param native_log={LOG(100)} native_pow={PWR(-2,2)}\n*#HSPICE\n",
    )?;
    run(
        &path.join("root.net"),
        "v(a) v(b) v(c) v(d)",
        &[
            6.,
            2.0_f64.mul_add(100.0_f64.ln(), -8.),
            2. * 100.0_f64.ln(),
            6.,
        ],
    )
}

#[test]
fn behavioral_source_grammar_controls_function_semantics() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b"* behavioral\n*#HSPICE\nB1 a 0 V=LOG(100)+PWR(-2,2)\nE1 b 0 VALUE={LOG(100)+PWR(-2,2)}\n.end\n")?;
    run(&root, "v(a) v(b)", &[6., 100.0_f64.ln() + 4.])
}

#[test]
fn mixed_expression_quoting_and_bare_bias_are_not_global_compatibility_modes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root,b"* bias\n*#HSPICE\n.param WIDTH=2 MULT=4 dc_bias=15\nV1 a 0 dc_bias\nV2 b 0 DC dc_bias\nV3 c 0 DC=dc_bias\nV4 d 0 '{MULT}'\nV5 e 0 '6*{WIDTH}'\n.end\n")?;
    run(&root, "v(a) v(b) v(c) v(d) v(e)", &[15., 15., 15., 4., 12.])
}

#[test]
fn identity_resolution_crosses_includes_but_not_subcircuit_scopes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b"* identity\n*#SIMETRIX sep=:\nV1 in 0 1\nR1 in sense 1k\n.inc 'sensor.lib'\nB1 out 0 V=I(sense_source)*1000\nH1 controlled 0 sense_source 2000\nX1 sub 0 cell\n.end\n")?;
    fs::write(dir.path().join("sensor.lib"), b"V:sense_source sense 0 0\n.inc 'literal.lib'\n.subckt cell out rtn\nV:sense_source n 0 0\nV2 supply 0 3\nR2 supply n 1k\nB1 out 0 V=I(sense_source)*1000\n.ends cell\n")?;
    fs::write(dir.path().join("literal.lib"), b"*#SIMETRIX sep=none\nV$literal lit 0 0\nV3 sup 0 4\nR3 sup lit 1k\nB2 other 0 V=I(V$literal)*1000\n")?;
    run(
        &root,
        "v(out) v(controlled) v(other) v(sub)",
        &[1., 2., 4., 3.],
    )?;
    let bundle = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let names = bundle
        .device_identities
        .iter()
        .filter(|identity| identity.displayed == "sense_source")
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().any(|identity| identity.scope.is_empty()));
    assert!(names.iter().any(|identity| identity.scope == ["cell"]));
    assert!(
        names
            .iter()
            .all(|identity| identity.linked.starts_with("V__src_"))
    );
    let literal = bundle
        .device_identities
        .iter()
        .find(|identity| identity.declared == "V$literal")
        .context("missing literal device")?;
    assert_eq!(literal.displayed, "V$literal");
    assert!(literal.linked.starts_with("V__src_"));
    Ok(())
}

#[test]
fn duplicate_displayed_names_fail_even_when_types_differ() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(
        &root,
        b"* duplicate\n*#SIMETRIX\nV$device a 0 1\nR$device a 0 1k\n.end\n",
    )?;
    let error = prepare(
        &Request {
            format: InputFormat::SimetrixDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )
    .err()
    .context("duplicate accepted")?;
    ensure!(
        format!("{error:#}").contains("duplicate device identity"),
        "{error:#}"
    );
    Ok(())
}

fn scoped_library_request(path: &Path) -> Result<Request> {
    let search = path.join("parts");
    fs::create_dir(&search)?;
    fs::write(path.join("cell.lb"), b"*#SIMETRIX sep=:\n.subckt cell out rtn\n.param native_log={LOG(100)}\nV:sense n rtn 0\nV2 vdd rtn 1\nR1 vdd n 1k\n.inc 'helper.lib'\n.param restored_log={LOG(100)}\nB1 out rtn V=1000*I(sense)+native_log+hslog+grandpow+pspow+restored_log\n.ends cell\n")?;
    fs::write(search.join("helper.lib"), b"*#HSPICE\n.param hslog='LOG(100)' $ HSPICE comment\n.lib 'nested.lib' tt\n.inc 'ps.lib'\n")?;
    fs::write(
        search.join("nested.lib"),
        b".lib tt\n.param grandpow='PWR(-2,2)'\n.endl tt\n.lib ss\n.param grandpow=999\n.endl ss\n",
    )?;
    fs::write(
        search.join("ps.lib"),
        b"*#PSPICE\n.param pspow={PWR(-2,2)} ; PSPICE comment\n",
    )?;
    fs::write(
        path.join("root.net"),
        b"* root\n*#HSPICE\nX1 out 0 cell\n.end\n",
    )?;
    Ok(Request {
        format: InputFormat::SimetrixLibrary,
        input: SourceInput::File(path.join("cell.lb")),
        libraries: Vec::new(),
        search_paths: vec![search],
    })
}

#[test]
fn standalone_and_catalog_libraries_share_scoped_source_admission() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path();
    let mut request = scoped_library_request(path)?;
    let options = Options::default();
    let cancel = Cancellation::default();
    let parsed_bundle = prepare(&request, &options, &cancel)?;
    let parsed = ParsedLibrary::from_json(&parsed_bundle.primary.bytes)?;
    assert_eq!(parsed.source_names().len(), 4);
    assert!(
        parsed
            .source_names()
            .iter()
            .any(|p| p.ends_with("nested.lib"))
    );
    let branch = parsed
        .device_identities()
        .iter()
        .find(|id| id.displayed == "sense")
        .context("missing custom device identity")?;
    assert!(branch.source.ends_with("cell.lb"));
    assert_eq!(branch.scope, ["cell"]);
    let input = request.input.clone();
    request.format = InputFormat::SimetrixDeck;
    request.input = SourceInput::File(path.join("root.net"));
    let expected = 5. + 100.0_f64.ln();
    for library in [LibraryInput::Source(input), LibraryInput::Parsed(parsed)] {
        request.libraries = vec![library];
        let bundle = prepare(&request, &options, &cancel)?;
        let Provenance::Generated { sources, .. } = &bundle
            .companions
            .first()
            .context("missing linked library")?
            .provenance
        else {
            anyhow::bail!("expected generated provenance");
        };
        assert_eq!(sources.len(), 4);
        assert!(sources.iter().any(|p| p.ends_with("helper.lib")));
        assert!(
            bundle
                .device_identities
                .iter()
                .any(|id| id.displayed == "sense" && id.source.ends_with("cell.lb"))
        );
        run_prepared(&bundle, "v(out)", &[expected])?;
    }
    Ok(())
}

#[test]
fn prepared_power_target_survives_nested_source_and_catalog_roundtrip() -> Result<()> {
    use ngspice_rs_preprocess::PowerTarget;
    let dir = tempfile::tempdir()?;
    let path = dir.path();
    let root = path.join("root.net");
    fs::write(&root, b"* prepared\n*#SIMETRIX\nV1 x 0 -2\nV2 p 0 3\n.inc 'hs.lib'\n.func f(x,p) {x**p}\nB1 abs 0 V={f(V(x),V(p))}\nB2 caret 0 V=V(x)^V(p)\nB3 real 0 V=POW(V(x),V(p))\nB4 masked 0 V={0*PWR(V(x),-1)}\nX1 x p y 0 cell\n.end\n")?;
    fs::write(path.join("hs.lib"), b"*#HSPICE\n.param hs={PWR(-2,3)}\n.inc 'ps.lib'\nE1 h 0 VALUE={PWR(V(x),3)}\nB5 native_b 0 V=V(x)**3\n")?;
    fs::write(path.join("ps.lib"), b"*#PSPICE\n.param ps={PWR(-2,3)}\n")?;
    let library_path = path.join("cell.lb");
    fs::write(
        &library_path,
        b"*#SIMETRIX\n.subckt cell x p y rtn\nB1 y rtn V=V(x)**V(p)\n.ends cell\n",
    )?;
    let cancel = Cancellation::default();
    let mut request = Request {
        format: InputFormat::SimetrixLibrary,
        input: SourceInput::File(library_path),
        libraries: Vec::new(),
        search_paths: Vec::new(),
    };
    let library = prepare(&request, &Options::default(), &cancel)?;
    let parsed = ParsedLibrary::from_json(&library.primary.bytes)?;
    let restored = ParsedLibrary::from_json(&parsed.to_json()?)?;
    request.format = InputFormat::SimetrixDeck;
    request.input = SourceInput::File(root);
    request.libraries = vec![LibraryInput::Parsed(parsed)];
    let options = Options {
        power_target: PowerTarget::PreparedSolverV1,
        ..Options::default()
    };
    let first = prepare(&request, &options, &cancel)?;
    request.libraries = vec![LibraryInput::Parsed(restored)];
    let second = prepare(&request, &options, &cancel)?;
    assert_eq!(first.power_target, PowerTarget::PreparedSolverV1);
    assert_eq!(first.primary.bytes, second.primary.bytes);
    assert_eq!(first.companions.len(), second.companions.len());
    for (a, b) in first.companions.iter().zip(&second.companions) {
        assert_eq!(a.bytes, b.bytes);
    }
    let deck = std::str::from_utf8(&first.primary.bytes)?.to_ascii_lowercase();
    for name in [
        "simetrix_abs_pow(",
        "simetrix_caret_pow(",
        "simetrix_pow(",
        "simetrix_abs_pow_product(",
    ] {
        ensure!(deck.contains(name), "missing {name}: {deck}");
    }
    assert!(first.companions.iter().any(|artifact| {
        String::from_utf8_lossy(&artifact.bytes)
            .to_ascii_lowercase()
            .contains("simetrix_abs_pow(")
    }));
    let native = prepare(&request, &Options::default(), &cancel)?;
    assert_eq!(native.power_target, PowerTarget::NativeNgspice);
    assert!(!String::from_utf8_lossy(&native.primary.bytes).contains("simetrix_abs_pow("));
    request.format = InputFormat::NativeDeck;
    request.libraries.clear();
    let error = prepare(&request, &options, &cancel)
        .err()
        .context("incompatible power target accepted")?;
    ensure!(
        format!("{error:#}").contains("prepared power target requires"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn native_and_ltspice_statements_keep_separate_laws_in_one_deck() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let vendor = dir.path().join("vendor.lib");
    fs::write(
        &root,
        b"mixed source laws\n.func limit(x,a,b) {x+a+b}\n.param native_result={limit(5,2,1)}\n.include vendor.lib\n.func bridge(x) {tbl(x)+1}\n.param table_result={bridge(0.25)}\nVnative a 0 {native_result}\nVvendor b 0 {vendor_result}\nVtable c 0 {table_result}\nVin input 0 0.25\nBtable d 0 V=bridge(v(input))\n.subckt native_cell p n\nLnative p n 1m\n.ends\n.end\n",
    )?;
    fs::write(
        &vendor,
        b"*#LTSPICE\n.param vendor_result={limit(5,2,1)}\n.func tbl(x) {table(x,0,0,1,2)}\n.subckt vendor_cell p n\nLvendor p n 1m\n.ends\n",
    )?;
    let bundle = prepare(
        &Request {
            format: InputFormat::NativeDeck,
            input: SourceInput::File(root.clone()),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains(".func limit(x,a,b) {x+a+b}"));
    assert!(text.contains(".param native_result={limit(5,2,1)}"));
    assert!(text.contains(".param vendor_result={__lt_intrinsic_limit("));
    assert!(text.contains("Lnative p n 1m"));
    assert!(!text.contains("R__Lnative_RSER"));
    assert!(text.contains("R__Lvendor_RSER"));
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing native line map");
    };
    assert_eq!(lines.len(), text.lines().count());
    let origin = text
        .lines()
        .zip(lines)
        .find_map(|(line, origin)| line.starts_with("R__Lvendor_RSER").then_some(origin))
        .context("missing generated series resistor")?;
    assert_eq!(origin.source, vendor.canonicalize()?);
    assert_eq!(origin.line, 5);
    run_prepared(&bundle, "v(a) v(b) v(c) v(d)", &[8.0, 2.0, 1.5, 1.5])
}

#[test]
fn ltspice_root_does_not_apply_its_default_inductor_loss_to_native_include() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(&root, b"LTspice with native subcircuit\n.include native.inc\n.func f(x) {limit(x,0,1)}\nV1 x 0 {f(2)}\n.subckt vendor p n\nLvendor p n 1m\n.ends\n.end\n")?;
    fs::write(dir.path().join("native.inc"), b"*#NATIVE\n.subckt native_cell p n\nLnative p n 1m\n.ends\n.param native_result={(-2)^3}\n")?;
    let bundle = prepare(
        &Request {
            format: InputFormat::LtspiceDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    assert!(text.contains(".param native_result={(-2)^3}"));
    assert!(text.contains("Lnative p n 1m"));
    assert!(!text.contains("R__Lnative_RSER"));
    assert!(text.contains("R__Lvendor_RSER"));
    run_prepared(&bundle, "v(x)", &[1.0])
}

#[test]
fn nested_pspice_qspice_and_tina_resources_keep_their_own_scalar_laws() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    fs::write(&root, b"Mixed source resource laws\n.param temp=7 native_power={pwr(-2,3)}\n.include ps.inc\nVnative a 0 {native_power+temp}\nVps b 0 {ps_power}\nVdrive d 0 1\nRnative d 0 multiplier 1k\nVq qin 0 4.4\nRq qout 0 1k\nX1 qin qout qdivider\nX2 t 0 tina_cell\n.temp 40\n.end\n")?;
    fs::write(dir.path().join("ps.inc"), b"*#PSPICE\n.param ps_power={pwr(-2,3)}\n.model multiplier RES(R=2)\nEtemp temp_out 0 VALUE={TEMP}\n.include q.inc\n.include tina.inc\n")?;
    fs::write(
        dir.path().join("q.inc"),
        b"*#QSPICE\n.subckt qdivider a b\nR1 a b 3k4\n.ends\n",
    )?;
    fs::write(
        dir.path().join("tina.inc"),
        b"*#TINA\n.subckt tina_cell o n .param local=3\nE1 o n VALUE={local}\n.ends\n",
    )?;
    let bundle = prepare(
        &Request {
            format: InputFormat::NativeDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let text = std::str::from_utf8(&bundle.primary.bytes)?;
    ensure!(text.contains("native_power={pwr(-2,3)}"), "{text}");
    ensure!(text.contains(".param temp=7"), "{text}");
    ensure!(!text.contains("3k4"), "{text}");
    ensure!(!text.contains("tina_cell o n .param"), "{text}");
    let Provenance::Native { lines, .. } = &bundle.primary.provenance else {
        anyhow::bail!("missing native line map");
    };
    assert_eq!(lines.len(), text.lines().count());
    let source = dir.path().join("ps.inc").canonicalize()?;
    for (prefix, expected) in [(".func __ps_pwr", 2), (".param __ps_temperature", 4)] {
        let origin = text
            .lines()
            .zip(lines)
            .find_map(|(line, origin)| line.starts_with(prefix).then_some(origin))
            .context("missing generated PSpice helper")?;
        assert_eq!(origin.source, source);
        assert_eq!(origin.line, expected);
    }
    run_prepared(
        &bundle,
        "v(a) v(b) v(temp_out) i(vdrive) v(qout) v(t)",
        &[15.0, -8.0, 40.0, -0.0005, 1.0, 3.0],
    )
}

#[test]
fn emitted_line_map_keeps_repeated_resource_occurrences_and_scope() -> Result<()> {
    use model_normalize_ngspice::linked_source::SourceDialect;
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.cir");
    let shared = dir.path().join("shared.inc");
    fs::write(&root, b"Repeated resources\n.include ps.inc\n.subckt native_cell out rtn\n.include shared.inc\n.ends\nX1 p 0 ps_cell\nX2 n 0 native_cell\n.end\n")?;
    fs::write(
        dir.path().join("ps.inc"),
        b"*#PSPICE\n.subckt ps_cell out rtn\n.include shared.inc\n.ends\n",
    )?;
    fs::write(&shared, b".param gain={pwr(-2,3)}\nV1 out rtn {gain}\n")?;
    let bundle = prepare(
        &Request {
            format: InputFormat::NativeDeck,
            input: SourceInput::File(root),
            libraries: Vec::new(),
            search_paths: Vec::new(),
        },
        &Options::default(),
        &Cancellation::default(),
    )?;
    let Provenance::Native {
        lines,
        source_graph: Some(graph),
    } = &bundle.primary.provenance
    else {
        anyhow::bail!("missing linked source graph");
    };
    let path = shared.canonicalize()?;
    let occurrences = lines
        .iter()
        .filter(|line| line.source == path && line.line == 2)
        .map(|line| line.linked.context("missing linked occurrence"))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(occurrences.len(), 2);
    assert_ne!(occurrences[0], occurrences[1]);
    assert_eq!(
        graph.visits[occurrences[0].visit].dialect,
        SourceDialect::Pspice
    );
    assert_eq!(
        graph.visits[occurrences[1].visit].dialect,
        SourceDialect::Native
    );
    assert_eq!(
        graph.scopes[occurrences[0].scope].name.as_deref(),
        Some("ps_cell")
    );
    assert_eq!(
        graph.scopes[occurrences[1].scope].name.as_deref(),
        Some("native_cell")
    );
    run_prepared(&bundle, "v(p) v(n)", &[-8.0, 8.0])
}
