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
    for format in [InputFormat::NativeDeck, InputFormat::SimetrixDeck] {
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
