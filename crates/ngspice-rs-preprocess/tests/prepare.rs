use anyhow::{Context, Result};
use ngspice_rs_preprocess::{
    Cancellation, CommandConfig, InputFormat, LibraryInput, Options, ParsedLibrary, Provenance,
    Request, SourceInput, prepare,
};
use std::{fs, path::Path, sync::Arc};

fn request(format: InputFormat, path: &Path) -> Request {
    Request {
        format,
        input: SourceInput::File(path.to_path_buf()),
        libraries: Vec::new(),
        search_paths: Vec::new(),
    }
}

fn prepare_default(request: &Request) -> Result<ngspice_rs_preprocess::PreparedBundle> {
    prepare(request, &Options::default(), &Cancellation::default())
}

#[test]
fn virtual_native_deck_keeps_title_terminator_scope_and_exact_line_mapping() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let origin = dir.path().join("virtual.cir");
    let library = dir.path().join("coil.lib");
    fs::write(&library, b".lib unused\n.model unwanted D\n.endl unused\n.lib chosen\n.subckt coil a b\n* keep this comment\nL1 a b 4u\n+ Rser=40m\n.ends coil\n.end\n.endl chosen\n")?;
    let mut req = request(InputFormat::NativeDeck, &origin);
    req.input = SourceInput::Bytes { origin: origin.clone(), bytes: Arc::from(&b"* title\n.lib coil.lib chosen\nVsrc a 0 dc 1 ac 1\nXcoil a 0 coil\n.control\n* control comment\nop\n.endc\n.end\n"[..]) };
    let prepared = prepare_default(&req)?;
    let output = std::str::from_utf8(&prepared.primary.bytes)?;
    assert!(output.starts_with("* title\n"));
    assert!(output.ends_with(".end\n"));
    assert!(output.contains(".subckt coil a b"));
    assert!(output.contains("* keep this comment"));
    assert!(output.contains("* control comment"));
    assert!(!output.contains("unwanted"));
    let Provenance::Native { lines } = &prepared.primary.provenance else {
        anyhow::bail!("missing native origins");
    };
    assert_eq!(lines.len(), output.lines().count());
    for (line, map) in output.lines().zip(lines) {
        if line.starts_with("R__L1_RSER") || line.starts_with("L1 ") {
            assert_eq!(map.source, library.canonicalize()?);
            assert_eq!(map.line, 7);
        }
        if line == "* title" {
            assert_eq!(map.source, origin);
            assert_eq!(map.line, 1);
        }
    }
    assert_eq!(
        prepared.normalization,
        [model_normalize_ngspice::NormalizationPass::LtspiceInductorRser]
    );
    Ok(())
}

#[test]
fn deck_title_is_not_interpreted_as_an_include_and_include_safe_is_distinct() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("source");
    fs::write(&path, b".include missing-title.lib\nR1 a 0 1k\n.end\n")?;
    let deck = prepare_default(&request(InputFormat::NativeDeck, &path))?;
    assert_eq!(deck.primary.bytes, fs::read(&path)?);
    assert!(prepare_default(&request(InputFormat::NativeLibrary, &path)).is_err());
    fs::write(&path, b".subckt part a b\nR1 a b 1k\n.ends part\n.end\n")?;
    let lib = prepare_default(&request(InputFormat::NativeLibrary, &path))?;
    assert!(
        !std::str::from_utf8(&lib.primary.bytes)?
            .lines()
            .any(|line| line.eq_ignore_ascii_case(".end"))
    );
    Ok(())
}

#[test]
fn repeated_native_includes_are_budgeted_and_cancellation_is_fail_closed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root");
    fs::write(
        &root,
        b"Title\n.inc repeated.lib\n.inc repeated.lib\n.end\n",
    )?;
    fs::write(dir.path().join("repeated.lib"), b"* padding 1234567890\n")?;
    let req = request(InputFormat::NativeDeck, &root);
    let options = Options {
        output_bytes: 30,
        ..Options::default()
    };
    let error = prepare(&req, &options, &Cancellation::default())
        .err()
        .context("budget accepted")?;
    assert!(format!("{error:#}").contains("byte budget"));
    let token = Cancellation::default();
    token.cancel();
    assert!(prepare(&req, &Options::default(), &token).is_err());
    Ok(())
}

#[test]
fn library_projection_and_closed_translated_graph() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let library = dir.path().join("parts.lb");
    fs::write(&library, b".subckt PART a b\nR1 a b 47\n.ends PART\n")?;
    let parsed_bundle = prepare_default(&request(InputFormat::SimplisLibrary, &library))?;
    let parsed = ParsedLibrary::from_json(&parsed_bundle.primary.bytes)?;
    let root = dir.path().join("root.net");
    fs::write(&root, b".include parts.lb\nX1 a 0 PART\n.end\n")?;
    let mut req = request(InputFormat::SimplisDeck, &root);
    req.libraries.push(LibraryInput::Parsed(parsed));
    let output = prepare_default(&req)?;
    assert_eq!(output.companions.len(), 1);
    assert!(std::str::from_utf8(&output.companions[0].bytes)?.contains("R1 a b 47"));
    assert!(std::str::from_utf8(&output.primary.bytes)?.contains(&output.companions[0].filename));
    assert_eq!(output.primary.dependencies.len(), 1);
    let source_id = output
        .primary
        .dependencies
        .first()
        .context("missing source dependency")?;
    let Provenance::Generated {
        source_id: Some(library_id),
        ..
    } = &output.companions[0].provenance
    else {
        anyhow::bail!("missing source identity");
    };
    assert_eq!(source_id, library_id);
    req.format = InputFormat::SimetrixDeck;
    assert!(prepare_default(&req).is_err());
    req.libraries.clear();
    assert!(prepare_default(&req).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn command_applies_to_raw_sources_not_parsed_artifacts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let library = dir.path().join("parts.lb");
    fs::write(&library, b".subckt PART a b\nR1 a b 47\n.ends PART\n")?;
    let parsed_bundle = prepare_default(&request(InputFormat::SimplisLibrary, &library))?;
    let parsed = dir.path().join("parsed.json");
    fs::write(&parsed, parsed_bundle.primary.bytes)?;
    let root = dir.path().join("root.net");
    fs::write(&root, b".include parts.lb\nX1 a 0 PLACEHOLDER\n.end\n")?;
    let mut req = request(InputFormat::SimplisDeck, &root);
    req.libraries.push(LibraryInput::ParsedFile(parsed));
    let mut options = Options::default();
    options.admission.command = Some(CommandConfig {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf x >> calls; sed 's/PLACEHOLDER/PART/g'".into(),
            "fixture".into(),
        ],
        working_directory: None,
    });
    let result = prepare(&req, &options, &Cancellation::default())?;
    assert_eq!(result.companions.len(), 1);
    assert_eq!(fs::read(dir.path().join("calls"))?, b"x");
    Ok(())
}

#[test]
fn expanded_dialect_requires_no_unresolved_external_includes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b"R1 a 0 1k\n.end\n")?;
    let deck = prepare_default(&request(InputFormat::ExpandedDeck, &root))?;
    assert!(deck.primary.bytes.ends_with(b".end\n"));
    let include = prepare_default(&request(InputFormat::ExpandedLibrary, &root))?;
    assert!(!include.primary.bytes.ends_with(b".end\n"));
    fs::write(&root, b".include unresolved.lib\nR1 a 0 1k\n.end\n")?;
    assert!(prepare_default(&request(InputFormat::ExpandedDeck, &root)).is_err());
    Ok(())
}

#[test]
fn simulator_projection_is_selected_before_loading_init_snapshots() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("root.net");
    fs::write(&root, b".SIMULATOR SIMPLIS\n.include absent.init\nR1 a 0 1\n.SIMULATOR SIMETRIX\nR1 a 0 47\n.SIMULATOR DEFAULT\n.end\n")?;
    let prepared = prepare_default(&request(InputFormat::SimetrixDeck, &root))?;
    assert!(std::str::from_utf8(&prepared.primary.bytes)?.contains("R1 a 0 47"));
    assert!(prepare_default(&request(InputFormat::SimplisDeck, &root)).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn recursively_admitted_init_snapshots_keep_original_path_context() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let deck_dir = dir.path().join("deck");
    fs::create_dir(&deck_dir)?;
    fs::create_dir(dir.path().join("state"))?;
    let root = deck_dir.join("root.net");
    fs::write(&root, b".include \"state/first.init\"\nR1 a 0 47\n.end\n")?;
    let first = dir.path().join("state/first.init");
    let second = dir.path().join("state/second.init");
    fs::write(&first, b".include second.init\n")?;
    fs::write(&second, b"* state\n")?;
    let mut options = Options::default();
    options.admission.command = Some(CommandConfig {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf x >> calls; cat".into(),
            "fixture".into(),
        ],
        working_directory: None,
    });
    let req = request(InputFormat::SimplisDeck, &root);
    let result = prepare(&req, &options, &Cancellation::default())?;
    let Provenance::Generated { sources, .. } = &result.primary.provenance else {
        anyhow::bail!("missing source identities");
    };
    assert_eq!(sources.len(), 3);
    assert_eq!(fs::read(deck_dir.join("calls"))?, b"x");
    assert_eq!(fs::read(dir.path().join("state/calls"))?, b"xx");
    fs::write(&second, b".include first.init\n")?;
    assert!(prepare(&req, &options, &Cancellation::default()).is_err());
    Ok(())
}

#[test]
fn explicit_library_paths_disambiguate_identical_basenames() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let left = dir.path().join("left");
    let right = dir.path().join("right");
    fs::create_dir(&left)?;
    fs::create_dir(&right)?;
    let left = left.join("parts.lb");
    let right = right.join("parts.lb");
    fs::write(&left, b".subckt LEFT a b\nR1 a b 47\n.ends LEFT\n")?;
    fs::write(&right, b".subckt RIGHT a b\nR1 a b 99\n.ends RIGHT\n")?;
    let root = dir.path().join("root.net");
    fs::write(
        &root,
        b".include left/parts.lb\n.inc 'right/parts.lb'\nX1 a 0 LEFT\nX2 b 0 RIGHT\n.end\n",
    )?;
    let mut req = request(InputFormat::SimplisDeck, &root);
    req.libraries
        .extend([left, right].map(|path| LibraryInput::Source(SourceInput::File(path))));
    let output = prepare_default(&req)?;
    assert_eq!(output.companions.len(), 2);
    fs::write(&root, b".include parts.lb\nX1 a 0 LEFT\n.end\n")?;
    let error = prepare_default(&req)
        .err()
        .context("ambiguous basename accepted")?;
    assert!(format!("{error:#}").contains("ambiguous supplied library include"));
    Ok(())
}

#[test]
fn equal_library_content_under_multiple_names_retains_all_include_aliases() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let first = dir.path().join("first.lb");
    let second = dir.path().join("second.lb");
    for path in [&first, &second] {
        fs::write(path, b".subckt PART a b\nR1 a b 47\n.ends PART\n")?;
    }
    let root = dir.path().join("root.net");
    fs::write(
        &root,
        b".include first.lb\n.include second.lb\nX1 a 0 PART\n.end\n",
    )?;
    let mut req = request(InputFormat::SimplisDeck, &root);
    req.libraries
        .extend([first, second].map(|path| LibraryInput::Source(SourceInput::File(path))));
    let output = prepare_default(&req)?;
    assert_eq!(output.companions.len(), 1);
    let Provenance::Generated { sources, .. } = &output.companions[0].provenance else {
        anyhow::bail!("missing generated provenance");
    };
    assert_eq!(sources.len(), 2);
    Ok(())
}
