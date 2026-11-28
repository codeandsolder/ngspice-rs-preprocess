//! Actual source bindings feed the typed model-kernel ABI without source parsing
//! or name lookup in the numerical evaluation path.

use model_kernel_api::{Binding, PointBuffer};
use model_kernel_vdmos::{Polarity, artifact::artifact, compile_ngspice};
use model_normalize_ngspice::{
    linked_source::RootKind,
    parameter_scope::{
        ModelLibrary,
        topology::{Limits, Topology},
    },
    vdmos_card::VdmosPolarity,
};
use std::{
    collections::BTreeMap,
    error::Error,
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const DECK: &[u8] = br"VDMOS nested call binding
vd d 0 .25
vg g 0 4
.param g=2
X1 d g 0 outer gain=2 m=3
X2 d g 0 outer gain=4
X3 d g 0 outer gain=2 m=2
.subckt outer d g s gain=1
Xinner d g s inner k={gain+1} m=2
Mlocal d g s local m=2
.model local vdmos(vto=3 kp={gain})
.ends
.subckt inner d g s k=1
Mone d g s local m=5 temp=45 dtemp=10
.model local vdmos(vto=3 kp={k})
.ends
.control
op
set numdgt=17
print @m.x1.xinner.mone[m] @m.x2.xinner.mone[m] @m.x3.xinner.mone[m]
print @m.x1.mlocal[m] @m.x2.mlocal[m] @m.x3.mlocal[m]
print @m.x1.xinner.mone[temp] @m.x2.xinner.mone[temp] @m.x3.xinner.mone[temp]
print @m.x1.mlocal[temp] @m.x2.mlocal[temp] @m.x3.mlocal[temp]
print @m.x1.xinner.mone[id] @m.x2.xinner.mone[id] @m.x3.xinner.mone[id]
print @m.x1.mlocal[id] @m.x2.mlocal[id] @m.x3.mlocal[id]
quit
.endc
.end
";

fn prepared_currents(topology: &Topology<'_>) -> Result<Vec<f64>> {
    let mut artifacts = BTreeMap::new();
    let mut currents = Vec::new();
    for mos in &topology.mos {
        let binding = mos.vdmos.as_ref().ok_or("missing binding")?;
        let key = binding.artifact_key();
        let compiled = match artifacts.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let values = &topology.models[binding.model].values;
                let polarity = match values.polarity {
                    VdmosPolarity::N => Polarity::N,
                    VdmosPolarity::P => Polarity::P,
                };
                let model = Arc::new(compile_ngspice(
                    polarity,
                    values.parameters.iter().copied(),
                )?);
                // Fixture-local identity, not a persistent producer fingerprint.
                let mut fingerprint = [0; 16];
                fingerprint[..8].copy_from_slice(&u64::try_from(binding.model)?.to_le_bytes());
                fingerprint[8] = u8::from(binding.self_heating);
                fingerprint[15] = 1;
                entry.insert(artifact(model, binding.self_heating, fingerprint)?)
            }
        };
        let ambient = binding.instance.ambient_kelvin(300.15)?;
        let prepared = compiled.prepare(&[binding.instance.multiplier], ambient)?;
        assert_eq!(
            compiled
                .descriptor()
                .equations
                .iter()
                .filter(|e| e.binding == Binding::Terminal)
                .count(),
            binding.terminals.len()
        );
        let coordinates = compiled
            .descriptor()
            .equations
            .iter()
            .map(|equation| match equation.name.as_str() {
                "drain" => Ok(0.25),
                "gate" => Ok(4.),
                "source" => Ok(0.),
                other => Err(format!("unexpected coordinate {other}")),
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut buffer = PointBuffer::new(coordinates.len())?;
        compiled.evaluate(&prepared, &coordinates, &mut buffer.point())?;
        currents.push(buffer.point().current[0]);
    }
    assert_eq!(artifacts.len(), 4);
    Ok(currents)
}

#[test]
fn repeated_occurrences_share_artifact_but_not_instance_state() -> Result<()> {
    let library = ModelLibrary::parse_source(DECK, RootKind::Deck)?;
    let topology = library.instantiate(Limits::default())?;
    assert_eq!(topology.models.len(), 4);
    assert_eq!(topology.mos.len(), 6);
    let currents = prepared_currents(&topology)?;
    assert!((currents[0] * 2. / 3. - currents[4]).abs() < 1e-9);
    assert!((currents[1] * 2. / 3. - currents[5]).abs() < 1e-9);
    Ok(())
}

#[test]
#[ignore = "requires independent ngspice 47"]
fn native_nested_instances_match_c_multipliers_temperatures_and_currents() -> Result<()> {
    let library = ModelLibrary::parse_source(DECK, RootKind::Deck)?;
    let topology = library.instantiate(Limits::default())?;
    let currents = prepared_currents(&topology)?;
    let mut child = Command::new("ngspice")
        .arg("-b")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("missing stdin")?.write_all(DECK)?;
    let result = child.wait_with_output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout)?;
    assert!(output.contains("ngspice-47 done"));
    let names = [
        "m.x1.xinner.mone",
        "m.x1.mlocal",
        "m.x2.xinner.mone",
        "m.x2.mlocal",
        "m.x3.xinner.mone",
        "m.x3.mlocal",
    ];
    for ((mos, current), name) in topology.mos.iter().zip(currents).zip(names) {
        let instance = mos.vdmos.as_ref().ok_or("missing binding")?.instance;
        for (field, actual) in [
            ("m", instance.multiplier),
            ("temp", instance.ambient_kelvin(300.15)? - 273.15),
            ("id", current.abs()),
        ] {
            let key = format!("@{name}[{field}]");
            let reference: f64 = output
                .lines()
                .filter_map(|line| line.split_once('='))
                .find(|(left, _)| left.trim() == key)
                .ok_or_else(|| format!("missing {key}: {output}"))?
                .1
                .trim()
                .parse()?;
            assert!(
                (actual - reference).abs() <= 2e-5_f64.mul_add(reference.abs(), 1e-8),
                "{key}: {actual:e} vs {reference:e}"
            );
        }
    }
    Ok(())
}
