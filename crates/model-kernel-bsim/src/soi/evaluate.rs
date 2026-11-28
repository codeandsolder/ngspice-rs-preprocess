use super::{
    BsimSoiPrepared,
    layout::Layout,
    stamp::{Biases, Differential, Stamp},
};
use model_compact_intrinsic::{
    BsimSoiDrainChannelPoint, BsimSoiExtrinsicChargeBias, BsimSoiGateBias, BsimSoiImpactBias,
    BsimSoiJunctionPoint, BsimSoiNoisePoint, BsimSoiParasiticPoint, BsimSoiSeriesBias,
};
use model_kernel_api::{Error, Point, Result, noise};

struct PhysicalPoint {
    drain: BsimSoiDrainChannelPoint,
    junctions: BsimSoiJunctionPoint,
    parasitics: BsimSoiParasiticPoint,
}

impl PhysicalPoint {
    fn evaluate(prepared: &BsimSoiPrepared, bias: &Biases) -> Result<Self> {
        let channel = bias.channel();
        let drain = prepared
            .drain
            .evaluate(channel)
            .map_err(|error| super::super::domain(&error))?;
        let junctions = prepared
            .junctions
            .evaluate(
                channel.kelvin,
                bias.junctions[0].value,
                bias.junctions[1].value,
            )
            .map_err(|error| super::super::domain(&error))?;
        let parasitics = prepared
            .parasitics
            .evaluate_with_channel(channel, &drain)
            .map_err(|error| super::super::domain(&error))?;
        Ok(Self {
            drain,
            junctions,
            parasitics,
        })
    }
}

fn stamp_channel(
    stamp: &mut Stamp<'_>,
    layout: &Layout,
    point: &PhysicalPoint,
    bias: &Biases,
) -> Result<()> {
    let collector = point.junctions.neutral_body.collector;
    let bipolar = Differential::lift(
        collector.current,
        [
            (bias.junctions[0], collector.d_body_source),
            (bias.junctions[1], collector.d_body_drain),
            (bias.axes[4], collector.d_temperature),
        ],
    );
    let current = Differential::channel(point.drain.current, &bias.axes) + bipolar;
    stamp.pair(Some(layout.drain), Some(layout.source), &current)?;
    for (side, node, terminal, voltage, neutral) in [
        (
            point.junctions.source,
            layout.source_body,
            layout.source,
            bias.junctions[0],
            point.junctions.neutral_body.source_recombination,
        ),
        (
            point.junctions.drain,
            layout.drain_body,
            layout.drain,
            bias.junctions[1],
            point.junctions.neutral_body.drain_recombination,
        ),
    ] {
        let current = Differential::junction(
            side.independent_current().plus(neutral),
            voltage,
            bias.axes[4],
        );
        stamp.pair(node, Some(terminal), &current)?;
    }
    if let Some(row) = layout.thermal {
        stamp.row(
            row,
            &Differential::channel(point.parasitics.heat.residual, &bias.axes),
            1.,
        );
    }
    Ok(())
}

fn stamp_leakage(
    stamp: &mut Stamp<'_>,
    layout: &Layout,
    prepared: &BsimSoiPrepared,
    bias: &Biases,
) -> Result<()> {
    let channel = bias.channel();
    let impact = prepared
        .impact
        .evaluate(BsimSoiImpactBias {
            channel,
            body_source_junction: bias.junctions[0].value,
            body_drain_junction: bias.junctions[1].value,
        })
        .map_err(|error| super::super::domain(&error))?;
    let leakage = prepared
        .leakage
        .evaluate(channel)
        .map_err(|error| super::super::domain(&error))?;
    for (node, impact, leakage) in [
        (layout.drain, impact.drain_to_body, leakage.drain_to_body),
        (layout.source, impact.source_to_body, leakage.source_to_body),
    ] {
        let current = Differential::channel(impact.current + leakage, &bias.axes)
            + Differential::lift(
                0.,
                [
                    (bias.junctions[0], impact.d_body_source_junction),
                    (bias.junctions[1], impact.d_body_drain_junction),
                ],
            );
        stamp.pair(Some(node), layout.body, &current)?;
    }
    let gate = prepared
        .gate
        .evaluate(BsimSoiGateBias {
            channel,
            contact: bias.contact.value,
        })
        .map_err(|error| super::super::domain(&error))?;
    for (terminal, point) in [
        (Some(layout.drain), gate.drain_edge + gate.drain_channel),
        (Some(layout.source), gate.source_edge + gate.source_channel),
        (layout.body, gate.body),
    ] {
        stamp.pair(
            Some(layout.gate),
            terminal,
            &Differential::channel(point, &bias.axes),
        )?;
    }
    let contact = Differential::channel(gate.contact, &bias.axes)
        + Differential::lift(0., [(bias.contact, gate.d_contact)]);
    stamp.pair(Some(layout.gate), layout.contact, &contact)
}

fn stamp_resistors(
    stamp: &mut Stamp<'_>,
    layout: &Layout,
    prepared: &BsimSoiPrepared,
    point: &PhysicalPoint,
    bias: &Biases,
    coordinates: &[f64],
) -> Result<()> {
    let [gate, drain, body, _, temperature] = bias.axes;
    let gate_drain = gate - drain;
    let body_drain = body - drain;
    let access = prepared
        .transport
        .external_series(
            temperature.value,
            BsimSoiSeriesBias {
                gate: gate.value,
                body: body.value,
            },
            BsimSoiSeriesBias {
                gate: gate_drain.value,
                body: body_drain.value,
            },
        )
        .map_err(|error| super::super::domain(&error))?;
    for (external, prime, coefficient, gate, body) in [
        (0, layout.drain, access.drain, gate_drain, body_drain),
        (2, layout.source, access.source, gate, body),
    ] {
        let conductance = Differential::series(coefficient, gate, body, temperature);
        let current = conductance * Differential::voltage(coordinates, external, prime);
        stamp.pair(Some(external), Some(prime), &current)?;
    }

    let p = point.parasitics;
    let resistance_branch =
        |stamp: &mut Stamp<'_>, a: Option<usize>, b: Option<usize>, g: f64| -> Result<()> {
            if g == 0. || a == b {
                return Ok(());
            }
            let (Some(a), Some(b)) = (a, b) else {
                return Err(Error::Shape("BSIMSOI resistor terminals"));
            };
            let current = Differential::constant(g) * Differential::voltage(coordinates, a, b);
            stamp.pair(Some(a), Some(b), &current)
        };
    resistance_branch(
        stamp,
        Some(1),
        Some(layout.gate_mid.unwrap_or(layout.gate)),
        p.gate_electrode,
    )?;
    if layout.gate != 1 {
        let from = layout.gate_mid.unwrap_or(1);
        let current = Differential::channel(p.gate_relaxation, &bias.axes)
            * Differential::voltage(coordinates, from, layout.gate);
        stamp.pair(Some(from), Some(layout.gate), &current)?;
    }
    for (a, b, g) in [
        (layout.body, layout.contact, p.body_contact),
        (layout.body, layout.source_body, p.source_body),
        (layout.body, layout.drain_body, p.drain_body),
        (Some(layout.gate), Some(layout.drain), prepared.gmin),
    ] {
        resistance_branch(stamp, a, b, g)?;
    }
    if layout.body.is_some() {
        resistance_branch(stamp, layout.body, Some(layout.source), prepared.gmin)?;
    }
    Ok(())
}

fn stamp_charges(
    stamp: &mut Stamp<'_>,
    layout: &Layout,
    prepared: &BsimSoiPrepared,
    point: &PhysicalPoint,
    bias: &Biases,
) -> Result<()> {
    let channel = bias.channel();
    let intrinsic = prepared
        .intrinsic
        .evaluate(channel)
        .map_err(|error| super::super::domain(&error))?;
    // The source receives the negative sum of all other intrinsic charges.
    // This makes conservation independent of source/drain orientation.
    for (row, charge) in [
        (Some(layout.gate), intrinsic.gate),
        (Some(layout.drain), intrinsic.drain),
        (layout.body, intrinsic.body),
        (Some(3), intrinsic.backgate),
    ] {
        stamp.pair(
            row,
            Some(layout.source),
            &Differential::channel(charge, &bias.axes),
        )?;
    }
    let overlap = prepared
        .extrinsic
        .evaluate(BsimSoiExtrinsicChargeBias {
            overlap_gate: bias.overlap_gate.value,
            drain: channel.drain,
            backgate: channel.backgate,
        })
        .map_err(|error| super::super::domain(&error))?;
    let mut overlap_axes = bias.axes;
    overlap_axes[0] = bias.overlap_gate;
    for (a, b, charge) in [
        (layout.overlap_gate, layout.drain, overlap.gate_drain),
        (layout.overlap_gate, layout.source, overlap.gate_source),
        (layout.overlap_gate, 3, overlap.gate_backgate),
        (layout.drain, 3, overlap.drain_backgate),
        (layout.source, 3, overlap.source_backgate),
    ] {
        stamp.pair(
            Some(a),
            Some(b),
            &Differential::channel(charge, &overlap_axes),
        )?;
    }
    for (row, terminal, charge, voltage) in [
        (
            layout.source_body,
            layout.source,
            point.junctions.charges.source.total(),
            bias.junctions[0],
        ),
        (
            layout.drain_body,
            layout.drain,
            point.junctions.charges.drain.total(),
            bias.junctions[1],
        ),
    ] {
        let charge = Differential::junction_charge(charge, voltage, bias.axes[4]);
        stamp.pair(row, Some(terminal), &charge)?;
    }
    if let Some(row) = layout.thermal {
        stamp.row(
            row,
            &Differential::channel(point.parasitics.heat.energy, &bias.axes),
            1.,
        );
    }
    Ok(())
}

pub(super) fn evaluate(
    layout: &Layout,
    prepared: &BsimSoiPrepared,
    coordinates: &[f64],
    out: &mut Point<'_>,
) -> Result<()> {
    let n = layout.width;
    if coordinates.len() != n
        || out.current.len() != n
        || out.charge.len() != n
        || out.conductance.len() != n * n
        || out.capacitance.len() != n * n
    {
        return Err(Error::Shape("BSIMSOI point dimensions"));
    }
    let bias = Biases::new(layout, coordinates, prepared.ambient);
    let point = PhysicalPoint::evaluate(prepared, &bias)?;
    out.current.fill(0.);
    out.conductance.fill(0.);
    out.charge.fill(0.);
    out.capacitance.fill(0.);
    let mut current = Stamp::current(out);
    stamp_channel(&mut current, layout, &point, &bias)?;
    stamp_leakage(&mut current, layout, prepared, &bias)?;
    stamp_resistors(&mut current, layout, prepared, &point, &bias, coordinates)?;
    stamp_charges(&mut Stamp::charge(out), layout, prepared, &point, &bias)
}

pub(super) fn noise(
    layout: &Layout,
    point: BsimSoiNoisePoint,
    sink: &mut dyn noise::Sink,
) -> Result<()> {
    let gate_resistor = layout.gate_mid.unwrap_or(layout.gate);
    for (name, density, a, b) in [
        ("rd", point.drain_access, Some(layout.drain), Some(0)),
        ("rs", point.source_access, Some(layout.source), Some(2)),
        ("rg", point.gate_electrode, Some(gate_resistor), Some(1)),
        (
            "id",
            point.channel_thermal,
            Some(layout.drain),
            Some(layout.source),
        ),
        (
            "1overf",
            point.channel_flicker,
            Some(layout.drain),
            Some(layout.source),
        ),
        (
            "fb_ibs",
            point.source_body_excess,
            Some(layout.source),
            layout.body,
        ),
        (
            "fb_ibd",
            point.drain_body_excess,
            Some(layout.drain),
            layout.body,
        ),
        (
            "igs",
            point.gate_source_shot,
            Some(layout.gate),
            Some(layout.source),
        ),
        (
            "igd",
            point.gate_drain_shot,
            Some(layout.gate),
            Some(layout.drain),
        ),
        ("igb", point.gate_body_shot, Some(layout.gate), layout.body),
        (
            "rbsb",
            point.source_body_resistor,
            layout.body,
            layout.source_body,
        ),
        (
            "rbdb",
            point.drain_body_resistor,
            layout.body,
            layout.drain_body,
        ),
        ("rbody", point.body_contact, layout.body, layout.contact),
    ] {
        if density == 0. || a == b {
            continue;
        }
        let (Some(a), Some(b)) = (a, b) else {
            return Err(Error::Shape("BSIMSOI noise terminals"));
        };
        let injections = [
            noise::Injection {
                row: a,
                re: 1.,
                im: 0.,
            },
            noise::Injection {
                row: b,
                re: -1.,
                im: 0.,
            },
        ];
        sink.push(noise::Source {
            name,
            density,
            injections: &injections,
        })?;
    }
    Ok(())
}
