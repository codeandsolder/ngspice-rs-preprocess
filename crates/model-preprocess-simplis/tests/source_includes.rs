use anyhow::Result;
use model_preprocess_simplis::{
    ParsedLibrary, SubcircuitCatalog, parse_library, preprocess_netlist, preprocess_netlist_linked,
};

fn outputs(source: &str, libraries: &[(&str, &str)], parsed: bool) -> Result<[String; 2]> {
    let mut catalog = SubcircuitCatalog::new();
    for &(name, text) in libraries {
        if parsed {
            let member = parse_library(name, text.as_bytes())?;
            catalog.add_parsed_library(&ParsedLibrary::from_json(&member.to_json()?)?)?;
        } else {
            catalog.add_library_named(name, text.as_bytes())?;
        }
    }
    let flat = preprocess_netlist(source.as_bytes(), &catalog)?;
    let linked = preprocess_netlist_linked(source.as_bytes(), &catalog)?;
    let mut combined = String::from_utf8(linked.root_bytes)?;
    for library in linked.libraries {
        combined.push_str(&String::from_utf8(library.bytes)?);
    }
    Ok([String::from_utf8(flat.bytes)?, combined])
}

#[test]
fn explicit_global_and_local_imports_obey_source_order() -> Result<()> {
    for parsed in [false, true] {
        for (declaration, child) in [(".GLOBALVAR G=3", "3"), (".VAR G=3", "{G}")] {
            let library = format!("{declaration}\n.SUBCKT CHILD 1 2\nR2 1 2 {{G}}\n.ENDS CHILD\n");
            for before in [false, true] {
                let root = if before {
                    ".include parts.lb\nR1 1 0 {G}\nX1 1 0 CHILD\n.end\n"
                } else {
                    "R1 1 0 {G}\nX1 1 0 CHILD\n.include parts.lb\n.end\n"
                };
                for output in outputs(root, &[("parts.lb", &library)], parsed)? {
                    let root_value = if before { "3" } else { "{G}" };
                    assert!(
                        output.contains(&format!("R1 1 0 {root_value}\n")),
                        "{output}"
                    );
                    assert!(output.contains(&format!("R2 1 2 {child}\n")), "{output}");
                    assert!(!output.contains(declaration), "{output}");
                    assert!(!output.contains(".include parts.lb"), "{output}");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn nested_repeated_includes_use_active_lexical_control_flow() -> Result<()> {
    let libraries = [
        ("left/parent.lb", ".include child.lb\n.VAR G={G+1}\n"),
        ("left/child.lb", ".VAR G={G+2}\n"),
        ("right/child.lb", ".ERROR wrong relative source\n"),
        ("unused.lb", ".GLOBALVAR G=900\n"),
    ];
    let root = ".VAR G=0\n.IF 0\n.include absent.lb\n.ELSE\n.WHILE {G<6}\n.include left/parent.lb\n.ENDWHILE\n.ENDIF\nR1 1 0 {G}\n.end\n";
    for parsed in [false, true] {
        for output in outputs(root, &libraries, parsed)? {
            assert!(output.contains("R1 1 0 6\n"), "{output}");
            assert!(!output.contains("absent.lb"), "{output}");
            assert!(!output.contains("wrong relative source"), "{output}");
            assert!(!output.contains("900"), "{output}");
        }
    }
    Ok(())
}

#[test]
fn explicit_include_cycles_are_active_only() -> Result<()> {
    let libraries = [("a.lb", ".include b.lb\n"), ("b.lb", ".include a.lb\n")];
    for parsed in [false, true] {
        let error = outputs(".include a.lb\n.end\n", &libraries, parsed).expect_err("active cycle");
        assert!(format!("{error:#}").contains("recursive SIMPLIS source include"));
        outputs(
            ".IF 0\n.include a.lb\n.ENDIF\nR1 1 0 1\n.end\n",
            &libraries,
            parsed,
        )?;
    }
    Ok(())
}

#[test]
fn active_model_support_is_not_reemitted_with_linked_definition() -> Result<()> {
    let library = ".VAR N=2\n.MODEL DM D IS={N}\n.SUBCKT CHILD 1 2\nD1 1 2 DM\n.ENDS CHILD\n";
    for parsed in [false, true] {
        for output in outputs(
            ".include parts.lb\nX1 1 0 CHILD\n.end\n",
            &[("parts.lb", library)],
            parsed,
        )? {
            assert_eq!(output.matches(".MODEL DM D IS=2").count(), 1, "{output}");
            assert!(!output.contains(".VAR N"), "{output}");
            assert!(!output.contains("IS={N}"), "{output}");
        }
    }
    Ok(())
}

#[test]
fn occurrence_local_includes_execute_with_actual_parameters_and_relative_origin() -> Result<()> {
    let libraries = [
        (
            "left/parent.lb",
            ".SUBCKT PARENT 1 2 VARS: A=1\n.include child.lb\nR1 1 2 {B}\nX2 1 2 INNER\n.ENDS\n",
        ),
        (
            "left/child.lb",
            ".VAR B={A+1}\n.IF {A>2}\n.SUBCKT INNER 1 2\nR2 1 2 7\n.ENDS\n.ELSE\n.SUBCKT INNER 1 2\nR2 1 2 11\n.ENDS\n.ENDIF\n",
        ),
        ("right/child.lb", ".ERROR wrong relative source\n"),
    ];
    let root = "X1 1 0 PARENT VARS: A=1\nX2 2 0 PARENT VARS: A=4\n.end\n";
    for parsed in [false, true] {
        for output in outputs(root, &libraries, parsed)? {
            for card in ["R1 1 2 2", "R1 1 2 5", "R2 1 2 7", "R2 1 2 11"] {
                assert!(output.contains(card), "missing {card}: {output}");
            }
            assert!(!output.contains(".include child.lb"), "{output}");
            assert!(!output.contains("wrong relative source"), "{output}");
            assert!(!output.contains(".VAR B"), "{output}");
        }
    }
    Ok(())
}

#[test]
fn include_end_exits_nested_controls_but_not_importer() -> Result<()> {
    let libraries = [(
        "a.lb",
        ".SYSTEMLIB\n.VAR A=2\n.IF 1\n.END\n.ENDIF\n.ERROR after end\n",
    )];
    let root = ".include a.lb\nR1 1 0 {A}\n.end\n";
    for parsed in [false, true] {
        for output in outputs(root, &libraries, parsed)? {
            assert!(output.contains("R1 1 0 2"), "{output}");
            assert!(!output.contains(".SYSTEMLIB"), "{output}");
            assert!(!output.contains(".ERROR"), "{output}");
        }
    }
    Ok(())
}

#[test]
fn identical_programs_at_distinct_paths_use_distinct_relative_includes() -> Result<()> {
    let program = ".include child.lb\n";
    let libraries = [
        ("left/parent.lb", program),
        ("right/parent.lb", program),
        ("left/child.lb", ".VAR A=2\n"),
        ("right/child.lb", ".VAR A=7\n"),
    ];
    let root = ".include left/parent.lb\nR1 1 0 {A}\n.include right/parent.lb\nR2 2 0 {A}\n.end\n";
    for parsed in [false, true] {
        for output in outputs(root, &libraries, parsed)? {
            assert!(output.contains("R1 1 0 2"), "{output}");
            assert!(output.contains("R2 2 0 7"), "{output}");
        }
        let error = outputs(".include parent.lb\n.end\n", &libraries, parsed)
            .expect_err("ambiguous path, even with equal program contents");
        assert!(format!("{error:#}").contains("ambiguous supplied library include"));
    }
    Ok(())
}

#[test]
fn active_model_and_subcircuit_aliases_use_evaluated_source_scope() -> Result<()> {
    let source = ".VAR N=3\n.MODEL DM D IS={N}\n.ALIAS DM_ALT DM D\n.SUBCKT CELL 1 2\nD1 1 2 DM_ALT\n.ENDS\n.ALIAS ALT CELL SUBCKT\n";
    for parsed in [false, true] {
        for output in outputs(
            ".include models.lb\nX1 1 0 ALT\n.end\n",
            &[("models.lb", source)],
            parsed,
        )? {
            assert_eq!(output.matches(".MODEL DM D IS=3").count(), 1, "{output}");
            assert_eq!(
                output
                    .to_ascii_uppercase()
                    .matches(".MODEL DM_ALT D IS=3")
                    .count(),
                1,
                "{output}"
            );
            assert!(!output.to_ascii_uppercase().contains(".ALIAS"), "{output}");
            assert!(!output.contains("IS={N}"), "{output}");
            assert!(!output.contains("X1 1 0 ALT"), "{output}");
        }
    }
    Ok(())
}

#[test]
fn root_origin_selects_only_the_relative_resource() -> Result<()> {
    for parsed in [false, true] {
        let mut catalog = SubcircuitCatalog::new();
        for path in ["left/child.lb", "right/child.lb"] {
            let bytes = b".VAR N=3\n";
            if parsed {
                catalog.add_parsed_library(&ParsedLibrary::from_json(
                    &parse_library(path, bytes)?.to_json()?,
                )?)?;
            } else {
                catalog.add_library_named(path, bytes)?;
            }
        }
        let root = b".include child.lb\nR1 1 0 {N}\n.end\n";
        let flat =
            model_preprocess_simplis::preprocess_netlist_at(root, &catalog, Some("left/deck.net"))?;
        assert!(String::from_utf8(flat.bytes)?.contains("R1 1 0 3"));
        let linked = model_preprocess_simplis::preprocess_netlist_linked_at(
            root,
            &catalog,
            Some("left/deck.net"),
        )?;
        assert!(String::from_utf8(linked.root_bytes)?.contains("R1 1 0 3"));
        let paths: Vec<_> = linked
            .included_sources
            .iter()
            .map(|s| s.source_name.as_str())
            .collect();
        assert_eq!(paths, ["left/child.lb"]);
    }
    Ok(())
}

#[test]
fn catalog_index_does_not_execute_root_control_programs() -> Result<()> {
    let source = ".GLOBALVAR N=99\n.IF 0\n.SUBCKT INACTIVE 1 2\nR9 1 2 99\n.ENDS\n.MODEL INACTIVE_D D IS=99\n.ENDIF\n.SUBCKT INDEXED 1 2 VARS: A=1\nR1 1 2 {A}\n.ENDS\n";
    for parsed in [false, true] {
        for output in outputs(
            "R0 1 0 {N}\nX1 1 0 INDEXED VARS: A=3\n.end\n",
            &[("program.lb", source)],
            parsed,
        )? {
            assert!(output.contains("R0 1 0 {N}"), "{output}");
            assert!(output.contains("R1 1 2 3"), "{output}");
            assert!(!output.contains(".GLOBALVAR"), "{output}");
            assert!(!output.contains("INACTIVE"), "{output}");
            assert!(!output.contains(".IF 0"), "{output}");
        }
        let err = outputs(
            ".include program.lb\nX1 1 0 INACTIVE VARS: A=1\n.end\n",
            &[("program.lb", source)],
            parsed,
        )
        .expect_err("inactive definition must not resolve via the index");
        assert!(format!("{err:#}").contains("unresolved"));
    }
    Ok(())
}

#[test]
fn active_include_diagnostics_use_physical_projected_source_positions() -> Result<()> {
    let libraries = [
        ("parent.lb", ".VAR A=1\n.include broken.lb\n"),
        (
            "broken.lb",
            ".SIMULATOR SIMETRIX\n* excluded\n.SIMULATOR SIMPLIS\n.VAR B=2\n.ERROR bad input\n",
        ),
    ];
    for parsed in [false, true] {
        let error = outputs(".include parent.lb\n.end\n", &libraries, parsed)
            .expect_err("active included error");
        let text = format!("{error:#}");
        assert!(text.contains("parent.lb:2"), "{text}");
        assert!(text.contains("broken.lb:5"), "{text}");
        assert!(text.contains("bad input"), "{text}");
        let missing = outputs(".include missing.lb\n.end\n", &[], parsed)
            .expect_err("missing active include");
        assert!(format!("{missing:#}").contains("unresolved SIMPLIS source include"));
    }
    Ok(())
}

#[test]
fn active_alias_in_plain_wrapper_requires_call_rewriting() -> Result<()> {
    let source = ".IF 1\n.ALIAS SELECTED CELL SUBCKT\n.ENDIF\n.SUBCKT CELL a b\nR1 a b 47\n.ENDS\n.SUBCKT WRAPPER a b\nXCHILD a b SELECTED\n.ENDS\n";
    for parsed in [false, true] {
        for output in outputs(
            ".include parts.lb\nX1 a 0 WRAPPER\n.end\n",
            &[("parts.lb", source)],
            parsed,
        )? {
            assert!(output.contains("R1 a b 47"), "{output}");
            assert!(!output.contains("XCHILD a b SELECTED"), "{output}");
            assert!(!output.contains(".ALIAS"), "{output}");
        }
    }
    Ok(())
}
