use anyhow::{Context, Result, ensure};
use ngspice_rs_preprocess::{
    Cancellation, InputFormat, LibraryInput, Options, PreparedBundle, Request, SourceInput,
    WorkLimits, prepare,
};
use std::{fs, path::Path, sync::Arc};

fn request(dir: &Path, format: InputFormat, source: &str) -> Request {
    Request {
        format,
        input: SourceInput::Bytes {
            origin: dir.join("root.net"),
            bytes: Arc::from(source.as_bytes()),
        },
        libraries: Vec::new(),
        search_paths: Vec::new(),
    }
}

fn run(request: &Request, work: WorkLimits) -> Result<PreparedBundle> {
    prepare(
        request,
        &Options {
            work,
            ..Options::default()
        },
        &Cancellation::default(),
    )
}

fn fails(request: &Request, work: WorkLimits, diagnostic: &str) -> Result<()> {
    let error = run(request, work)
        .err()
        .context("resource limit was bypassed")?;
    ensure!(
        format!("{error:#}").contains(diagnostic),
        "{error:#}; expected {diagnostic}"
    );
    Ok(())
}

#[test]
fn cumulative_input_and_snapshot_limits_do_not_charge_repeated_admission() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = "* title\n.include a.lib\n.include a.lib\n.include b.lib\n.end\n";
    let a = "R1 1 0 1k\n";
    let b = "R2 2 0 2k\n";
    fs::write(dir.path().join("a.lib"), a)?;
    fs::write(dir.path().join("b.lib"), b)?;
    let req = request(dir.path(), InputFormat::NativeDeck, root);
    let unique = root.len() + a.len() + b.len();
    let work = WorkLimits {
        input_bytes: unique,
        admitted_bytes: unique,
        sources: 3,
        ..WorkLimits::default()
    };
    let prepared = run(&req, work)?;
    assert_eq!(
        std::str::from_utf8(&prepared.primary.bytes)?
            .matches(a)
            .count(),
        2
    );
    fails(
        &req,
        WorkLimits {
            input_bytes: unique - 1,
            ..work
        },
        "InputBytes budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits {
            admitted_bytes: unique - 1,
            ..work
        },
        "AdmittedBytes budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits { sources: 2, ..work },
        "Sources budget exceeded",
    )?;
    Ok(())
}

#[test]
fn repeated_empty_include_work_is_bounded_independently_of_emitted_bytes() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("leaf.lib"), "")?;
    fs::write(
        dir.path().join("a.lib"),
        ".include leaf.lib\n.include leaf.lib\n",
    )?;
    let root = "* title\n.include a.lib\n.include a.lib\n.end\n";
    for format in [
        InputFormat::NativeDeck,
        InputFormat::SimetrixDeck,
        InputFormat::HspiceDeck,
    ] {
        let req = request(dir.path(), format, root);
        fails(
            &req,
            WorkLimits {
                source_visits: 6,
                ..WorkLimits::default()
            },
            "SourceVisits budget exceeded",
        )?;
        run(
            &req,
            WorkLimits {
                source_visits: 7,
                ..WorkLimits::default()
            },
        )?;
    }
    Ok(())
}

#[test]
fn cycles_are_distinct_from_acyclic_depth_exhaustion() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fs::write(dir.path().join("a.lib"), ".include b.lib\n")?;
    fs::write(dir.path().join("b.lib"), "R1 1 0 1k\n")?;
    for format in [InputFormat::NativeDeck, InputFormat::SimetrixDeck] {
        let req = request(dir.path(), format, "* title\n.include a.lib\n.end\n");
        fails(
            &req,
            WorkLimits {
                depth: 2,
                ..WorkLimits::default()
            },
            "depth budget exceeded",
        )?;
        fs::write(dir.path().join("b.lib"), ".include a.lib\n")?;
        fails(
            &req,
            WorkLimits {
                depth: 3,
                ..WorkLimits::default()
            },
            "recursive",
        )?;
        fs::write(dir.path().join("b.lib"), "R1 1 0 1k\n")?;
    }
    Ok(())
}

#[test]
fn cumulative_catalog_input_and_parse_work_share_request_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = "X1 a 0 A\nX2 b 0 B\n.end\n";
    let source = ".subckt A p n\nR1 p n 1k\n.ends A\n";
    let parsed =
        model_preprocess_simplis::parse_library("B.lib", b".subckt B p n\nR1 p n 2k\n.ends B\n")?
            .to_json()?;
    let raw_path = dir.path().join("A.lib");
    let parsed_path = dir.path().join("B.json");
    fs::write(&raw_path, source)?;
    fs::write(&parsed_path, &parsed)?;
    let mut req = request(dir.path(), InputFormat::SimplisDeck, root);
    req.libraries
        .push(LibraryInput::Source(SourceInput::File(raw_path)));
    req.libraries.push(LibraryInput::ParsedFile(parsed_path));
    let total = root.len() + source.len() + parsed.len();
    let work = WorkLimits {
        input_bytes: total,
        ..WorkLimits::default()
    };
    let prepared = run(&req, work)?;
    assert_eq!(prepared.companions.len(), 2);
    fails(
        &req,
        WorkLimits {
            input_bytes: total - 1,
            ..work
        },
        "InputBytes budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits { sources: 2, ..work },
        "Sources budget exceeded",
    )?;
    Ok(())
}

const LOOPS: &str = ".VAR I=0\n.WHILE {I<50} 50\n.VAR J=0\n.WHILE {J<50} 50\nR{I}_{J} 1 0 1k\n.VAR J={J+1}\n.ENDWHILE\n.VAR I={I+1}\n.ENDWHILE\n.end\n";

#[test]
fn nested_loop_work_and_emission_are_cumulative() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let req = request(dir.path(), InputFormat::SimplisDeck, LOOPS);
    fails(
        &req,
        WorkLimits {
            statements: 100,
            ..WorkLimits::default()
        },
        "Statements budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits {
            evaluation_steps: 100,
            ..WorkLimits::default()
        },
        "EvaluationSteps budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits {
            emitted_bytes: 1000,
            ..WorkLimits::default()
        },
        "EmittedBytes budget exceeded",
    )?;
    let prepared = run(&req, WorkLimits::default())?;
    let text = std::str::from_utf8(&prepared.primary.bytes)?;
    assert_eq!(
        text.lines().filter(|line| line.starts_with('R')).count(),
        2500
    );
    assert!(text.contains("R49_49 1 0 1k"));
    Ok(())
}

#[test]
fn huge_vectors_and_recursive_expressions_fail_before_allocation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    for source in [
        ".VAR A=Vector(4294967295)\nR1 a 0 1\n.end\n",
        ".VAR enable=1\nR1 a 0 {Vector(4294967295)}\n.end\n",
    ] {
        let req = request(dir.path(), InputFormat::SimplisDeck, source);
        fails(
            &req,
            WorkLimits {
                generated_nodes: 100,
                ..WorkLimits::default()
            },
            "GeneratedNodes budget exceeded",
        )?;
    }
    let deep = format!(
        ".VAR A={}1{}\nR1 a 0 1\n.end\n",
        "(".repeat(1000),
        ")".repeat(1000)
    );
    let req = request(dir.path(), InputFormat::SimplisDeck, &deep);
    fails(&req, WorkLimits::default(), "depth budget exceeded")?;
    Ok(())
}

#[test]
fn repeated_specializations_share_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut source = String::new();
    use std::fmt::Write as _;
    for i in 1..=100 {
        writeln!(source, "X{i} a 0 T vars: R={i}")?;
    }
    source.push_str(".subckt T a b vars: R=1\nR1 a b {R}\n.ends T\n.end\n");
    let req = request(dir.path(), InputFormat::SimplisDeck, &source);
    fails(
        &req,
        WorkLimits {
            generated_nodes: 50,
            ..WorkLimits::default()
        },
        "GeneratedNodes budget exceeded",
    )?;
    let prepared = run(&req, WorkLimits::default())?;
    let text = std::str::from_utf8(&prepared.companions[0].bytes)?;
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with(".subckt"))
            .count(),
        100
    );
    Ok(())
}

#[test]
fn deeply_nested_library_and_expanded_sources_are_bounded() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut source = String::new();
    use std::fmt::Write as _;
    for i in 0..100 {
        writeln!(source, ".subckt T{i} a b")?;
    }
    source.push_str("R1 a b 1k\n");
    for i in (0..100).rev() {
        writeln!(source, ".ends T{i}")?;
    }
    for format in [
        InputFormat::SimplisLibrary,
        InputFormat::SimetrixLibrary,
        InputFormat::ExpandedLibrary,
    ] {
        let req = request(dir.path(), format, &source);
        fails(&req, WorkLimits::default(), "depth budget exceeded")?;
    }
    Ok(())
}

#[test]
fn model_configuration_and_deck_share_request_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root =
        "* model selection\nV1 a 0 1\nD1 a 0 selected\n.model selected D LEVEL=1 IS=1p\n.end\n";
    let config = "ModelName=D Level=1 Device=Diode3\n";
    let config_path = dir.path().join("devices.dcf");
    fs::write(&config_path, config)?;
    let req = request(dir.path(), InputFormat::SimetrixDeck, root);
    let total = root.len() + config.len();
    let mut options = Options {
        device_config: Some(config_path),
        work: WorkLimits {
            input_bytes: total,
            admitted_bytes: total,
            sources: 2,
            ..WorkLimits::default()
        },
        ..Options::default()
    };
    let token = Cancellation::default();
    let prepared = prepare(&req, &options, &token)?;
    assert_eq!(
        prepared.model_selections[0].mapping.device,
        model_preprocess_simplis::source_dialect::model_mapping::Device::Diode3
    );
    for (work, diagnostic) in [
        (
            WorkLimits {
                input_bytes: total - 1,
                ..options.work
            },
            "InputBytes budget exceeded",
        ),
        (
            WorkLimits {
                admitted_bytes: total - 1,
                ..options.work
            },
            "AdmittedBytes budget exceeded",
        ),
        (
            WorkLimits {
                sources: 1,
                ..options.work
            },
            "Sources budget exceeded",
        ),
    ] {
        options.work = work;
        let error = prepare(&req, &options, &token)
            .err()
            .context("device configuration bypassed budget")?;
        ensure!(
            format!("{error:#}").contains(diagnostic),
            "{error:#}; expected {diagnostic}"
        );
        options.work = WorkLimits {
            input_bytes: total,
            admitted_bytes: total,
            sources: 2,
            ..WorkLimits::default()
        };
    }
    Ok(())
}

#[test]
fn device_identity_scope_copies_consume_shared_budget() -> Result<()> {
    use model_preprocess_command::WorkBudget;
    use model_preprocess_simplis::source_dialect::{
        DialectContext, ScopedStatement, resolve_device_identities,
    };
    let name = "S".repeat(1024);
    let mut lines = vec![format!(".subckt {name} a b")];
    lines.extend((0..32).map(|i| format!("R{i} a b 1k")));
    lines.push(format!(".ends {name}"));
    let statements = lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| ScopedStatement {
            source: "source.net".into(),
            context: DialectContext::default(),
            first_line: i + 1,
            last_line: i + 1,
            text,
        })
        .collect::<Vec<_>>();
    let budget = WorkBudget::new(
        WorkLimits {
            emitted_bytes: 8192,
            ..WorkLimits::default()
        },
        Cancellation::default(),
    );
    let error = resolve_device_identities(&statements, &budget)
        .err()
        .context("identity metadata bypassed budget")?;
    ensure!(format!("{error:#}").contains("EmittedBytes budget exceeded"));
    ensure!(budget.check().is_err());
    let resolved = resolve_device_identities(&statements, &WorkBudget::default())?;
    assert_eq!(resolved.identities.len(), 32);
    assert_eq!(resolved.identities[31].scope, vec![name]);
    Ok(())
}

#[test]
fn repeated_compiled_catalog_members_consume_work_before_catalog_construction() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = "X1 a 0 A\n.end\n";
    let parsed =
        model_preprocess_simplis::parse_library("A.lib", b".subckt A p n\nR1 p n 1k\n.ends A\n")?;
    let file = dir.path().join("A.json");
    fs::write(&file, parsed.to_json()?)?;
    let mut req = request(dir.path(), InputFormat::SimplisDeck, root);
    for _ in 0..8 {
        req.libraries.push(LibraryInput::ParsedFile(file.clone()));
    }
    fails(
        &req,
        WorkLimits {
            source_visits: 8,
            ..WorkLimits::default()
        },
        "SourceVisits budget exceeded",
    )?;
    let prepared = run(
        &req,
        WorkLimits {
            sources: 2,
            ..WorkLimits::default()
        },
    )?;
    assert_eq!(prepared.companions.len(), 1);
    req.libraries = vec![LibraryInput::Parsed(parsed)];
    fails(
        &req,
        WorkLimits {
            sources: 1,
            ..WorkLimits::default()
        },
        "Sources budget exceeded",
    )?;
    assert_eq!(
        run(
            &req,
            WorkLimits {
                sources: 2,
                ..WorkLimits::default()
            }
        )?
        .primary
        .bytes,
        prepared.primary.bytes
    );
    Ok(())
}

#[test]
fn native_provenance_storage_is_charged_before_copying_paths() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let long_name = "a".repeat(200);
    let name = format!("{long_name}.lib");
    let lines = "R1 1 0 1k\n".repeat(100);
    fs::write(dir.path().join(&name), &lines)?;
    let source = format!("* title\n.include {name}\n.end\n");
    let req = request(dir.path(), InputFormat::NativeDeck, &source);
    // Source text is small, but each linked row retains the physical path.
    fails(
        &req,
        WorkLimits {
            emitted_bytes: 8192,
            ..WorkLimits::default()
        },
        "EmittedBytes budget exceeded",
    )?;
    let bundle = run(&req, WorkLimits::default())?;
    assert_eq!(
        std::str::from_utf8(&bundle.primary.bytes)?
            .matches("R1 1 0 1k")
            .count(),
        100
    );
    Ok(())
}

#[test]
fn pspice_model_inheritance_shares_cumulative_resource_budget() -> Result<()> {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir()?;
    let mut source = String::from("* inherited model chain\n.model m0 D(IS=1p)\n");
    for index in 1..200 {
        writeln!(source, ".model m{index} AKO:m{} D (RS={index})", index - 1)?;
    }
    source.push_str("V1 a 0 1\nD1 a 0 m199\n.end\n");
    for format in [InputFormat::PspiceDeck, InputFormat::TinaDeck] {
        let req = request(dir.path(), format, &source);
        fails(
            &req,
            WorkLimits {
                evaluation_steps: 4096,
                ..WorkLimits::default()
            },
            "EvaluationSteps budget exceeded",
        )?;
        let bundle = run(&req, WorkLimits::default())?;
        assert!(std::str::from_utf8(&bundle.primary.bytes)?.contains(".model m199"));
    }
    Ok(())
}

#[test]
fn qspice_legacy_decoding_charges_expansion_before_allocation() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut bytes = b"* ".to_vec();
    bytes.extend(std::iter::repeat_n(0x80, 4096));
    bytes.extend_from_slice(b"\nR1 a 0 1k\n.end\n");
    let req = Request {
        format: InputFormat::QspiceDeck,
        input: SourceInput::Bytes {
            origin: dir.path().join("cp1252.cir"),
            bytes: Arc::from(bytes),
        },
        libraries: Vec::new(),
        search_paths: Vec::new(),
    };
    fails(
        &req,
        WorkLimits {
            emitted_bytes: 8192,
            ..WorkLimits::default()
        },
        "EmittedBytes budget exceeded",
    )?;
    let result = run(&req, WorkLimits::default())?;
    let text = std::str::from_utf8(&result.primary.bytes)?;
    assert_eq!(text.matches('€').count(), 4096);
    Ok(())
}

#[test]
fn qspice_symbolic_waveform_points_use_cumulative_node_budget() -> Result<()> {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir()?;
    let mut source = String::from("* waveform budget\n.param a=1\nV1 a 0 PWL(0 {a}");
    for _ in 0..512 {
        write!(source, " +1n {{a}}")?;
    }
    source.push_str(")\nR1 a 0 1k\n.end\n");
    let req = request(dir.path(), InputFormat::QspiceDeck, &source);
    fails(
        &req,
        WorkLimits {
            generated_nodes: 256,
            ..WorkLimits::default()
        },
        "GeneratedNodes budget exceeded",
    )?;
    let bundle = run(&req, WorkLimits::default())?;
    assert_eq!(bundle.waveforms.len(), 1);
    Ok(())
}

#[test]
fn expanded_analysis_plan_charges_scope_metadata() -> Result<()> {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir()?;
    let name = "A".repeat(1024);
    let mut source = format!(".subckt {name} a b\n");
    for index in 0..64 {
        writeln!(source, ".save V(n{index})")?;
    }
    source.push_str("R1 a b 1k\n.ends\n.end\n");
    let req = request(dir.path(), InputFormat::ExpandedDeck, &source);
    fails(
        &req,
        WorkLimits {
            emitted_bytes: 16_384,
            ..WorkLimits::default()
        },
        "EmittedBytes budget exceeded",
    )?;
    let bundle = run(&req, WorkLimits::default())?;
    let plan = bundle
        .analysis_plan
        .context("missing expanded analysis plan")?;
    assert_eq!(plan.entries.len(), 64);
    Ok(())
}

#[test]
fn hspice_acyclic_occurrence_fanout_consumes_shared_budget() -> Result<()> {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir()?;
    let mut deck = String::from(
        "* hierarchical fanout\nV1 a 0 1\nX1 a 0 stage8\n.subckt stage0 p n\nR1 p n 1k\n.ends stage0\n",
    );
    for level in 1..=8 {
        let child = level - 1;
        writeln!(
            deck,
            ".subckt stage{level} p n\nX1 p n stage{child}\nX2 p n stage{child}\n.ends stage{level}"
        )?;
    }
    deck.push_str(".end\n");
    let req = request(dir.path(), InputFormat::HspiceDeck, &deck);
    let bundle = run(&req, WorkLimits::default())?;
    let native = std::str::from_utf8(&bundle.primary.bytes)?;
    assert_eq!(
        native
            .lines()
            .filter(|line| line.starts_with("R1 "))
            .count(),
        256
    );
    fails(
        &req,
        WorkLimits {
            depth: 4,
            ..WorkLimits::default()
        },
        "recursion depth budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits {
            generated_nodes: 200,
            ..WorkLimits::default()
        },
        "GeneratedNodes budget exceeded",
    )?;
    let cycle = request(
        dir.path(),
        InputFormat::HspiceDeck,
        "* recursive\nX1 a 0 cell\n.subckt cell p n\nX2 p n cell\n.ends cell\n.end\n",
    );
    fails(&cycle, WorkLimits::default(), "recursive HSPICE subcircuit")?;
    Ok(())
}

#[test]
fn hspice_nested_power_lowering_uses_shared_depth_limit() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut expression = "2".to_owned();
    for _ in 0..9 {
        expression = format!("PWR({expression},1)");
    }
    let deck = format!("* nested power\n.param gain='{expression}'\nV1 a 0 'gain'\n.end\n");
    let req = request(dir.path(), InputFormat::HspiceDeck, &deck);
    let bundle = run(&req, WorkLimits::default())?;
    assert!(std::str::from_utf8(&bundle.primary.bytes)?.contains("sgn("));
    fails(
        &req,
        WorkLimits {
            depth: 4,
            ..WorkLimits::default()
        },
        "recursion depth budget exceeded",
    )?;
    Ok(())
}

#[test]
fn rational_transfer_algebra_shares_preparation_budget() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let deck =
        "* rational transfer\nV1 in 0 1\nE1 out 0 in 0 Laplace={1/(s+1)^32}\nR1 out 0 1k\n.end\n";
    let req = request(dir.path(), InputFormat::NativeDeck, deck);
    let bundle = run(&req, WorkLimits::default())?;
    assert!(std::str::from_utf8(&bundle.primary.bytes)?.contains("s_xfer(num_coeff=[1e0]"));
    fails(
        &req,
        WorkLimits {
            evaluation_steps: 200,
            ..WorkLimits::default()
        },
        "EvaluationSteps budget exceeded",
    )?;
    fails(
        &req,
        WorkLimits {
            emitted_bytes: 4096,
            ..WorkLimits::default()
        },
        "EmittedBytes budget exceeded",
    )?;
    Ok(())
}
