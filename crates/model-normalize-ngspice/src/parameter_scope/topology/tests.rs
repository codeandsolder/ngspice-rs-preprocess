use super::*;
use crate::linked_source::{RootKind, SourceLinker};
use std::fs;

fn binding<'a>(mos: &'a Mos<'_>) -> Result<&'a VdmosBinding> {
    mos.vdmos.as_ref().context("expected VDMOS binding")
}

fn kp(topology: &Topology<'_>, device: usize) -> Result<f64> {
    let variant = &topology.models[binding(&topology.mos[device])?.model];
    variant
        .values
        .parameters
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("kp"))
        .map(|(_, value)| *value)
        .context("missing KP")
}

#[test]
fn caller_bindings_lexical_models_and_node_identities() -> Result<()> {
    let library = ModelLibrary::parse_source(
        br"Mixed devices title (not a MOS line)
.param gain=1
.func twice(a) {a*2}
.global supply
X1 d g gnd outer gain=2
X2 d g 0 outer gain=3
X3 d g 0 outer gain=2
.subckt outer d g s params: gain=5
.param local={gain+1}
.func f(a) {twice(a)+local}
Xinner localnode g s inner k={f(1)}
Mouter d g GND dut
.model dut vdmos(vto=3 kp={gain})
.subckt inner d g s k=4
M1 d g s dut
M2 d supply s nested
.model nested vdmos(vto=3 kp={k+gain})
.ends inner
.ends outer
.control
print foo
.endc
.end
",
        RootKind::Deck,
    )?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(topology.frames.len(), 7);
    assert_eq!(topology.mos.len(), 9);
    assert_eq!(topology.models.len(), 4);
    assert_eq!(
        (0..9)
            .map(|i| kp(&topology, i))
            .collect::<Result<Vec<_>>>()?,
        [2., 7., 2., 3., 9., 3., 2., 7., 2.]
    );
    assert_eq!(topology.mos[0].model_frame, 1);
    assert_eq!(topology.mos[1].model_frame, 2);
    assert_eq!(
        binding(&topology.mos[0])?.artifact_key(),
        binding(&topology.mos[6])?.artifact_key()
    );
    assert_eq!(
        topology.mos[0].nodes[0],
        Node::Local {
            frame: 1,
            name: "localnode".into()
        }
    );
    assert_eq!(
        topology.mos[3].nodes[0],
        Node::Local {
            frame: 3,
            name: "localnode".into()
        }
    );
    assert_eq!(topology.mos[1].nodes[1], Node::Global("supply".into()));
    assert_eq!(topology.mos[0].nodes[2], Node::Global("0".into()));
    assert_eq!(topology.mos[2].nodes[2], Node::Global("0".into()));
    assert_eq!(topology.frames[1].location.linked_line, 5);
    assert_eq!(topology.frames[2].definition_location.linked_line, 14);
    Ok(())
}

#[test]
fn dynamic_parameter_chain_does_not_change_lexical_model_visibility() -> Result<()> {
    let source = br"test
.param gain=1
X1 d g 0 outer gain=2
X2 d g 0 outer gain=3
.subckt outer d g s gain=5
Xinner d g s inner
.ends
.subckt inner d g s
M1 d g s dut
.model dut vdmos(vto=3 kp={gain})
.ends
";
    let library = ModelLibrary::parse_source(source, RootKind::Deck)?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(kp(&topology, 0)?, 2.);
    assert_eq!(kp(&topology, 1)?, 3.);
    let broken = ModelLibrary::parse_source(
        br"test
X1 d g 0 outer
.subckt outer d g s
.model dut vdmos(kp=2)
Xinner d g s inner
.ends
.subckt inner d g s
M1 d g s dut
.ends
",
        RootKind::Deck,
    )?;
    assert!(broken.instantiate(Limits::default()).is_err());
    Ok(())
}

#[test]
fn native_implicit_m_and_explicit_formal_have_distinct_semantics() -> Result<()> {
    let library = ModelLibrary::parse_source(
        br"test
X1 d g 0 outer m=3
X2 d g 0 outer
X3 d g 0 formal m=3
X4 d g 0 formal
.subckt outer d g s
Xinner d g s inner m=2
Mlocal d g s local m=2
.model local vdmos(vto=3 kp=2)
.ends
.subckt inner d g s
M1 d g s local m=5 temp=45 dtemp=10
.model local vdmos(vto=3 kp=2)
.ends
.subckt formal d g s m=7
M1 d g s local m=2
M2 d g s local m={m}
.model local vdmos(vto=3 kp=2)
.ends
",
        RootKind::Deck,
    )?;
    let topology = library.instantiate(Limits::default())?;
    let multipliers = topology
        .mos
        .iter()
        .map(|m| Ok(binding(m)?.instance.multiplier))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(multipliers, [3., 3., 1., 1., 2., 3., 2., 7.]);
    assert_eq!(
        binding(&topology.mos[0])?.instance.ambient_kelvin(300.15)?,
        310.15
    );
    assert_eq!(
        binding(&topology.mos[0])?.artifact_key(),
        binding(&topology.mos[2])?.artifact_key()
    );
    Ok(())
}

#[test]
fn duplicate_policy_temperature_defaults_and_thermal_terminals() -> Result<()> {
    let library = ModelLibrary::parse_source(
        br"test
X1 d g 0 foo k=3 k=5
X2 d g 0 foo k=5
.subckt foo d g s k=1
.model dut vdmos(vto=3 kp={k} m=0.5 temp=35 temp=50 rthjc=2)
.model dut vdmos(vto=3 kp=9)
M1 d g s dut m=2 ic=(.1 , .2) off
M2 d g s tj tc dut thermal temp=60 dtemp=10 icvgs=1
M3 d g s dut thermal
M4 d g s tj dut thermal
.ends
.subckt foo d g s k=10
M1 d g s other
.ends
.model ordinary nmos (kp=1)
Mroot d g 0 0 ordinary l=1u w=2u
",
        RootKind::Deck,
    )?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(topology.models.len(), 1);
    assert_eq!(kp(&topology, 0)?, 5.);
    let first = binding(&topology.mos[0])?;
    assert_eq!(first.instance.multiplier, 2.);
    assert_eq!(first.instance.temp_celsius, Some(35.));
    assert_eq!(first.instance.initial, [Some(0.1), Some(0.2)]);
    assert!(first.instance.off);
    assert_eq!(first.terminals.len(), 3);
    let second = binding(&topology.mos[1])?;
    assert_eq!(second.instance.ambient_kelvin(300.15)?, 310.15);
    assert_eq!(second.terminals.len(), 5);
    assert_ne!(first.artifact_key(), second.artifact_key());
    let third = binding(&topology.mos[2])?;
    assert_eq!(
        third.terminals[3],
        Node::DeviceThermal {
            device: 2,
            terminal: ThermalTerminal::Junction
        }
    );
    assert_eq!(
        binding(&topology.mos[3])?.terminals[4],
        Node::DeviceThermal {
            device: 3,
            terminal: ThermalTerminal::Case
        }
    );
    assert_eq!(third.artifact_key(), second.artifact_key());
    assert!(topology.mos[8].vdmos.is_none());
    assert_eq!(topology.mos[8].source_parameters.len(), 2);
    Ok(())
}

#[test]
fn invalid_reachable_hierarchy_and_limits_are_errors() -> Result<()> {
    for deck in [
        "title\nX1 d g 0 missing",
        "title\nX1 d g 0 foo\n.subckt foo a b c\nX2 a b c foo\n.ends",
        "title\nX1 d g foo\n.subckt foo a b c\n.ends",
        "title\nX1 d g 0 foo missing=1\n.subckt foo a b c\n.ends",
        "title\nX1 d g 0 foo k={missing}\n.subckt foo a b c k=1\n.ends",
        "title\nM1 d g 0 mod m=-1\n.model mod vdmos(kp=1)",
        "title\nM1 d g 0 mod thermal=2\n.model mod vdmos(kp=1)",
        "title\nM1 d g 0 mod nonsense=1\n.model mod vdmos(kp=1)",
        "title\nM1 d g 0 mod\nm1 d g 0 mod\n.model mod vdmos(kp=1)",
    ] {
        let library = ModelLibrary::parse_source(deck.as_bytes(), RootKind::Deck)?;
        assert!(library.instantiate(Limits::default()).is_err(), "{deck}");
    }
    let library = ModelLibrary::parse_source(
        b"title\nX1 d g 0 foo\n.subckt foo a b c\nM1 a b c mod\n.model mod vdmos(kp=1)\n.ends",
        RootKind::Deck,
    )?;
    assert!(
        library
            .instantiate(Limits {
                depth: 1,
                occurrences: 10
            })
            .is_err()
    );
    assert!(
        library
            .instantiate(Limits {
                depth: 10,
                occurrences: 2
            })
            .is_err()
    );
    Ok(())
}

#[test]
fn linked_section_keeps_definition_and_invocation_origins() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let root = directory.path().join("root.cir");
    let lib = directory.path().join("parts.lib");
    fs::write(
        &root,
        "title\n.lib parts.lib selected\nX1 d g 0 foo k=3\n.end\n",
    )?;
    fs::write(
        &lib,
        ".lib selected\n.subckt foo d g s k=1\nM1 d g s dut\n.lib parts.lib core\n.ends\n.endl selected\n.lib core\n.model dut vdmos(kp={k})\n.endl core\n",
    )?;
    let linked = SourceLinker::default().link(&root)?;
    let library = ModelLibrary::parse_linked(&linked, RootKind::Deck)?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(kp(&topology, 0)?, 3.);
    let frame = &topology.frames[1];
    assert_eq!(frame.location.origin.context("call origin")?.line, 3);
    let definition = frame
        .definition_location
        .origin
        .context("definition origin")?;
    assert_eq!(definition.line, 2);
    assert_eq!(definition.path, lib.canonicalize()?);
    let origin = topology.models[0].location.origin.context("model origin")?;
    assert_eq!(origin.line, 8);
    assert_eq!(origin.path, lib.canonicalize()?);
    assert_eq!(
        topology.mos[0]
            .location
            .origin
            .context("device origin")?
            .line,
        3
    );
    Ok(())
}

#[test]
fn self_named_defaults_inherit_caller_without_accepting_indirect_cycles() -> Result<()> {
    let library = ModelLibrary::parse_source(
        br"test
.param gain=2
X1 d g 0 foo
X2 d g 0 foo gain=3
.subckt foo d g s params:gain={gain*2}
M1 d g s dut ic=.1 .2
.model dut vdmos(kp={gain})
.ends
",
        RootKind::Deck,
    )?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(kp(&topology, 0)?, 4.);
    assert_eq!(kp(&topology, 1)?, 3.);
    assert_eq!(
        binding(&topology.mos[0])?.instance.initial,
        [Some(0.1), Some(0.2)]
    );
    let broken = ModelLibrary::parse_source(
        br"test
.param a=1
X1 d g 0 foo
.subckt foo d g s a={b} b={a}
M1 d g s dut
.model dut vdmos(kp={a})
.ends
",
        RootKind::Deck,
    )?;
    assert!(broken.instantiate(Limits::default()).is_err());
    Ok(())
}
