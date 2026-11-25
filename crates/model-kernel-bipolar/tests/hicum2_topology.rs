use model_kernel_bipolar::{
    ModelError, Temperature,
    hicum2::{
        temperature,
        topology::{self, Branch, Node, Resistor},
    },
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn with_gates(bits: u8) -> temperature::Nominal {
    let mut p = temperature::Nominal::source_defaults();
    let resistance = |position: u8| {
        if bits & (1 << position) != 0 {
            0.001
        } else {
            0.0009
        }
    };
    p.auxiliary.rbx = resistance(0);
    p.auxiliary.rbi0 = resistance(1);
    p.auxiliary.rcx = resistance(2);
    p.auxiliary.re = resistance(3);
    p.auxiliary.rsu = resistance(4);
    p
}

#[test]
fn all_32_nominal_source_resistance_reductions_have_dense_stable_indices() -> TestResult {
    for mask in 0..32 {
        let p = with_gates(mask);
        let layout = p.physical_topology()?;
        assert_eq!(layout.count(), 4 + mask.count_ones() as usize);
        for (index, node) in Node::TERMINALS.into_iter().enumerate() {
            assert_eq!(usize::from(layout.index(node)), index);
        }
        let mut seen = [false; 9];
        for node in Node::ALL {
            let index = usize::from(layout.index(node));
            assert!(index < layout.count());
            seen[index] = true;
        }
        assert!(seen[..layout.count()].iter().all(|value| *value));
        for resistor in Resistor::ALL {
            let (left, right) = resistor.nodes();
            let active = mask & (1 << resistor.index()) != 0;
            assert_eq!(layout.present(resistor), active);
            assert_eq!(
                layout.index(left) == layout.index(right),
                !active,
                "mask={mask} resistor={resistor:?}",
            );
        }
        // Changing ambient, self-heating trial or effective resistance
        // must never affect nominal physical topology.
        for kelvin in [233.15, 300.15, 473.15] {
            let prepared = p.prepare(Temperature::new(kelvin)?, 0.0)?;
            let _ = prepared.device_kernel()?;
            assert_eq!(p.physical_topology()?, layout);
        }
    }
    Ok(())
}

#[test]
fn source_branch_endpoints_are_complete_and_use_original_orientation() -> TestResult {
    let expected = [
        (Branch::Rbx, Node::B, Node::Bp),
        (Branch::Rbi, Node::Bp, Node::Bi),
        (Branch::Rcx, Node::Ci, Node::C),
        (Branch::Re, Node::Ei, Node::E),
        (Branch::Rsu, Node::Si, Node::S),
        (Branch::IntrinsicEmitter, Node::Bi, Node::Ei),
        (Branch::IntrinsicCollector, Node::Bi, Node::Ci),
        (Branch::PeripheralEmitter, Node::Bp, Node::Ei),
        (Branch::ExternalEmitterIsolation, Node::B, Node::E),
        (Branch::PeripheralEmitterIsolation, Node::Bp, Node::E),
        (Branch::ExternalCollectorJunction, Node::B, Node::Ci),
        (Branch::PeripheralCollector, Node::Bp, Node::Ci),
        (Branch::TransferForward, Node::Ci, Node::Ei),
        (Branch::TransferReverse, Node::Ei, Node::Ci),
        (Branch::SubstrateJunction, Node::Si, Node::Ci),
        (Branch::PeripheralSubstrateJunction, Node::S, Node::C),
        (Branch::SubstrateTransfer, Node::Bp, Node::Si),
        (Branch::Avalanche, Node::Ci, Node::Bi),
    ];
    assert_eq!(Branch::ALL.len(), expected.len());
    for (index, (branch, a, b)) in expected.into_iter().enumerate() {
        assert_eq!(Branch::ALL[index], branch);
        assert_eq!(branch.nodes(), (a, b));
        let mapping = with_gates(0b10101).physical_topology()?;
        assert_eq!(mapping.ends(branch), (mapping.index(a), mapping.index(b)));
        assert_eq!(branch.resistor().is_some(), index < 5);
    }
    Ok(())
}

#[test]
fn thermal_and_nqs_slots_follow_nominal_mode_not_trial_operating_point() -> TestResult {
    let mut p = with_gates(31);
    assert_eq!(p.physical_topology()?.nqs_state_count(), 0);
    // Any of the five HICCR forward-delay mechanisms can generate Tf,
    // so a structurally possible NQS must reserve states ahead of Newton.
    p.transport.t0 = 5e-12;
    for (flcomp, enabled, expected) in [
        (0.0, false, 3),
        (2.1, false, 3),
        (2.2, false, 0),
        (2.3, false, 0),
        (2.3, true, 3),
    ] {
        p.transport.flcomp = flcomp;
        p.auxiliary.flnqs = enabled;
        assert_eq!(p.physical_topology()?.nqs_state_count(), expected);
    }
    p.transport.t0 = 0.0;
    for field in 0..5 {
        p.transport.dt0h = 0.0;
        p.transport.tbvl = 0.0;
        p.transport.tef0 = 0.0;
        p.transport.thcs = 0.0;
        match field {
            0 => p.transport.t0 = 1e-12,
            1 => p.transport.dt0h = 1e-12,
            2 => p.transport.tbvl = 1e-12,
            3 => p.transport.tef0 = 1e-12,
            _ => p.transport.thcs = 1e-12,
        }
        p.auxiliary.flnqs = true;
        assert_eq!(p.physical_topology()?.nqs_state_count(), 3);
        p.transport.t0 = 0.0;
    }
    p.noise.alit = 0.0;
    p.noise.alqf = 0.0;
    assert_eq!(p.physical_topology()?.nqs_state_count(), 0);
    for (mode, rth, shorted) in [
        (0, 0.0, true),
        (1, 0.0009, true),
        (1, 0.001, false),
        (2, 100.0, false),
        (2, 0.0, true),
    ] {
        p.auxiliary.flsh = mode;
        p.auxiliary.rth = rth;
        assert_eq!(p.physical_topology()?.thermal_shorted(), shorted);
    }
    Ok(())
}

#[test]
fn topology_rejects_unphysical_nominal_constraints() {
    for resistance in Resistor::ALL {
        let mut p = with_gates(31);
        match resistance {
            Resistor::Rbx => p.auxiliary.rbx = -1.0,
            Resistor::Rbi => p.auxiliary.rbi0 = -1.0,
            Resistor::Rcx => p.auxiliary.rcx = -1.0,
            Resistor::Re => p.auxiliary.re = -1.0,
            Resistor::Rsu => p.auxiliary.rsu = -1.0,
        }
        assert!(matches!(
            topology::Prepared::new(&p),
            Err(ModelError::InvalidParameter(_))
        ));
    }
    let mut p = with_gates(31);
    p.auxiliary.flsh = 3;
    assert!(matches!(
        p.physical_topology(),
        Err(ModelError::InvalidParameter("flsh"))
    ));
    p.auxiliary.flsh = 1;
    p.auxiliary.rth = f64::NAN;
    assert!(matches!(
        p.physical_topology(),
        Err(ModelError::InvalidParameter("rth"))
    ));
}
