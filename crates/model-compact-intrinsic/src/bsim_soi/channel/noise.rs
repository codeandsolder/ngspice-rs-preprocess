//! BSIMSOI 4.4 source-referred noise power spectra (`B4SOInoise`).
//! Copyright 2010 Regents of the University of California. See package LICENSE.
use super::super::{
    BsimSoiSeriesBias, PreparedBsimSoiJunctions, PreparedBsimSoiSetup, PreparedBsimSoiTransport,
    integer, positive, real,
};
use super::{
    BsimSoiChannelBias, BsimSoiChannelPoint, BsimSoiDrainChannelPoint, BsimSoiGateBias,
    BsimSoiImpactBias, PreparedBsimSoiChannel, PreparedBsimSoiDrainChannel,
    PreparedBsimSoiGateInducedLeakage, PreparedBsimSoiGateTunneling,
    PreparedBsimSoiImpactIonization, PreparedBsimSoiIntrinsicCharges, PreparedBsimSoiParasitics,
};
use crate::ModelError;

// B4SOInoise uses ngspice's global constants, not the rounded BSIMSOI
// constitutive constants used in B4SOIload.
const Q: f64 = 1.602_176_620_8e-19;
const K: f64 = 1.380_648_52e-23;
const MIN_LOG: f64 = 1e-38;

/// Physical noise bias, including distributed junction and body contact nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiNoiseBias {
    pub channel: BsimSoiChannelBias,
    pub body_source_junction: f64,
    pub body_drain_junction: f64,
    pub contact: f64,
    pub frequency: f64,
}

/// Uncorrelated, one-sided branch current power spectral densities (A²/Hz).
///
/// Names refer to physical terminal pairs. Gate electrode noise is injected
/// between external gate and midgate in mode 3, otherwise external and prime.
/// Floating-body excess noise follows the native body node, not junction nodes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BsimSoiNoisePoint {
    pub drain_access: f64,
    pub source_access: f64,
    pub gate_electrode: f64,
    pub channel_thermal: f64,
    pub channel_flicker: f64,
    pub source_body_excess: f64,
    pub drain_body_excess: f64,
    pub gate_source_shot: f64,
    pub gate_drain_shot: f64,
    pub gate_body_shot: f64,
    pub source_body_resistor: f64,
    pub drain_body_resistor: f64,
    pub body_contact: f64,
}

impl BsimSoiNoisePoint {
    fn checked(self) -> Result<Self, ModelError> {
        let values = [
            self.drain_access,
            self.source_access,
            self.gate_electrode,
            self.channel_thermal,
            self.channel_flicker,
            self.source_body_excess,
            self.drain_body_excess,
            self.gate_source_shot,
            self.gate_drain_shot,
            self.gate_body_shot,
            self.source_body_resistor,
            self.drain_body_resistor,
            self.body_contact,
        ];
        if values
            .into_iter()
            .all(|value| value.is_finite() && value >= 0.)
        {
            Ok(self)
        } else {
            Err(ModelError::InvalidPhysicalParameter("BSIMSOI noise power"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NoiseGeometry {
    width: f64,
    length: f64,
    cox: f64,
    litl: f64,
    multiplicity: f64,
}

#[derive(Clone, Copy, Debug)]
struct NoiseOperatingPoint {
    temperature: f64,
    drain_current: f64,
    drain_voltage: f64,
    effective_voltage: f64,
    overdrive: f64,
    bulk: f64,
    mobility: f64,
    esat: f64,
    vtm: f64,
    carriers: f64,
    inversion_charge: f64,
    internal_resistance: f64,
    output_ratio: f64,
    gm: f64,
    gmb: f64,
    gds: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThermalLaw {
    mode: i32,
    ntnoi: f64,
    rnoia: f64,
    rnoib: f64,
    tnoia: f64,
    tnoib: f64,
}

impl ThermalLaw {
    fn partitions(self, geometry: NoiseGeometry, point: NoiseOperatingPoint) -> [f64; 2] {
        let field = (point.overdrive / (point.esat * geometry.length)).powi(2) * geometry.length;
        [
            self.rnoia * self.tnoia.mul_add(field, 1.),
            self.rnoib * self.tnoib.mul_add(field, 1.),
        ]
    }

    fn conductance(self, geometry: NoiseGeometry, point: NoiseOperatingPoint) -> f64 {
        match self.mode {
            0 => {
                let mobility_charge = point.mobility * point.inversion_charge.abs();
                self.ntnoi * mobility_charge
                    / mobility_charge.mul_add(point.internal_resistance, geometry.length.powi(2))
            }
            1 => {
                let [beta, theta] = self.partitions(geometry, point);
                let ratio = if point.drain_current == 0. {
                    1e9
                } else {
                    (point.effective_voltage / point.drain_current).min(1e9)
                };
                let gate_body = point.gm + point.gmb;
                let induced = theta * (gate_body + point.gds);
                let channel = beta.mul_add(gate_body, point.gds);
                // Difference of squares preserves the weak difference near cancellation.
                (channel - induced) * (channel + induced) * ratio
            }
            _ => self.ntnoi * (2. / 3.) * (point.gm + point.gmb + point.gds).abs(),
        }
    }

    fn access(
        self,
        geometry: NoiseGeometry,
        point: NoiseOperatingPoint,
        mut conductances: [f64; 2],
        reversed: bool,
    ) -> [f64; 2] {
        if self.mode == 1 {
            let [beta, theta] = self.partitions(geometry, point);
            let theta = theta.min(0.9).min(0.9 * beta);
            let source = usize::from(reversed);
            let conductance = conductances[source];
            if conductance != 0. {
                conductances[source] =
                    conductance * (theta.powi(2) * conductance / point.output_ratio + 1.);
            }
        }
        conductances
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FlickerLaw {
    mode: i32,
    kf: f64,
    af: f64,
    ef: f64,
    bf: f64,
    reference_width: f64,
    em: f64,
    traps: [f64; 3],
}

impl FlickerLaw {
    fn evaluate(self, geometry: NoiseGeometry, point: NoiseOperatingPoint, frequency: f64) -> f64 {
        let frequency_factor = frequency.powf(self.ef);
        let current = point.drain_current.abs();
        if self.mode == 0 {
            let scale = if self.reference_width > 0. {
                geometry.width / self.reference_width
            } else {
                1.
            };
            return scale * self.kf * ((current / scale).max(MIN_LOG).ln() * self.af).exp()
                / (frequency_factor * geometry.length.powf(self.bf) * geometry.cox);
        }
        let [a, b, c] = self.traps;
        let n0 = geometry.cox * point.overdrive / Q;
        let nl = n0
            * (1.
                - point.bulk * point.effective_voltage
                    / (2_f64.mul_add(point.vtm, point.overdrive)));
        let delta = n0 - nl;
        let integrated = delta.mul_add(
            (0.5 * c).mul_add(n0 + nl, b),
            a * ((n0 + point.carriers) / (nl + point.carriers))
                .max(MIN_LOG)
                .ln(),
        );
        let thermal = K * point.temperature;
        let scale = 1e10 * frequency_factor * geometry.length.powi(2);
        let strong = Q.powi(2) * thermal * current * point.mobility * integrated
            / (scale * point.bulk * geometry.cox);
        let clm = if self.em <= 0. {
            0.
        } else {
            geometry.litl
                * (((point.drain_voltage - point.effective_voltage) / geometry.litl + self.em)
                    / point.esat)
                    .max(MIN_LOG)
                    .ln()
        };
        let local = c.mul_add(nl, b).mul_add(nl, a);
        let extension = thermal * current.powi(2) * clm * local
            / (scale * geometry.width * (nl + point.carriers).powi(2));
        let strong = strong + extension;
        let weak = a * thermal * current.powi(2)
            / (1e10 * frequency_factor * geometry.width * geometry.length * point.carriers.powi(2));
        if strong + weak > 0. {
            strong * weak / (strong + weak)
        } else {
            0.
        }
    }
}

/// Prepared noise laws with immutable physical subkernels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreparedBsimSoiNoise {
    channel: PreparedBsimSoiChannel,
    drain: PreparedBsimSoiDrainChannel,
    charges: PreparedBsimSoiIntrinsicCharges,
    junctions: PreparedBsimSoiJunctions,
    gate: PreparedBsimSoiGateTunneling,
    impact: PreparedBsimSoiImpactIonization,
    leakage: PreparedBsimSoiGateInducedLeakage,
    transport: PreparedBsimSoiTransport,
    parasitics: PreparedBsimSoiParasitics,
    geometry: NoiseGeometry,
    thermal: ThermalLaw,
    flicker: FlickerLaw,
    excess: f64,
    ambient: f64,
    charge_enabled: bool,
}

impl PreparedBsimSoiNoise {
    fn operating_point(
        &self,
        bias: BsimSoiNoiseBias,
        point: &BsimSoiChannelPoint,
        drain: &BsimSoiDrainChannelPoint,
    ) -> Result<(NoiseOperatingPoint, [f64; 2]), ModelError> {
        let junction = self.junctions.evaluate(
            bias.channel.kelvin,
            bias.body_source_junction,
            bias.body_drain_junction,
        )?;
        let source = junction
            .source
            .independent_current()
            .plus(junction.neutral_body.source_recombination)
            .current;
        let drain_body = junction
            .drain
            .independent_current()
            .plus(junction.neutral_body.drain_recombination)
            .current;
        let collector = junction.neutral_body.collector;
        let impact = self.impact.evaluate(BsimSoiImpactBias {
            channel: bias.channel,
            body_source_junction: bias.body_source_junction,
            body_drain_junction: bias.body_drain_junction,
        })?;
        let leakage = self.leakage.evaluate(bias.channel)?;
        let sign = self.channel.polarity;
        let orientation = if point.reversed { -1. } else { 1. };
        let high = if point.reversed {
            impact.source_to_body.current.value + leakage.source_to_body.value - source
        } else {
            impact.drain_to_body.current.value + leakage.drain_to_body.value - drain_body
        };
        let m = self.geometry.multiplicity;
        let current = sign.mul_add(
            high,
            sign * orientation * (drain.current.value + collector.current),
        );
        let gm = orientation * drain.current.gradient[0];
        let gmb = orientation
            * (drain.current.gradient[2] + collector.d_body_source + collector.d_body_drain);
        let gds = if point.reversed {
            orientation.mul_add(
                -(drain.current.gradient[2] + drain.current.gradient[3]),
                drain.current.gradient[1] - gm,
            ) + collector.d_body_source
        } else {
            drain.current.gradient[1] - collector.d_body_drain
        };
        let material = self.channel.material.evaluate(bias.channel.kelvin)?;
        let transport = self.transport.evaluate(bias.channel.kelvin)?;
        let mobility = drain.effective_mobility.value;
        let overdrive = point.effective_overdrive.value;
        let bulk = drain.bulk_charge_factor.value;
        let vtm = material.thermal_voltage.value;
        let effective_voltage = drain.effective_drain_voltage.value;
        let inversion_charge = if self.charge_enabled {
            self.charges.evaluate(bias.channel)?.inversion.value.abs() / m
        } else {
            self.geometry.cox
                * self.geometry.width
                * self.geometry.length
                * overdrive
                * (1. - 0.5 * bulk * effective_voltage / (2_f64.mul_add(vtm, overdrive)))
        };
        Ok((
            NoiseOperatingPoint {
                temperature: material.kelvin,
                drain_current: current / m,
                drain_voltage: point.drain_voltage.value,
                effective_voltage,
                overdrive,
                bulk,
                mobility,
                esat: 2. * transport.saturation_velocity.value / mobility,
                vtm,
                carriers: point.noise_carrier_density.value,
                inversion_charge,
                internal_resistance: drain.internal_resistance.value * m,
                output_ratio: drain.output_conductance_ratio.value / m,
                gm: gm / m,
                gmb: gmb / m,
                gds: gds / m,
            },
            [source, drain_body],
        ))
    }

    /// Evaluate all independent spectra at the supplied frequency and operating point.
    ///
    /// # Errors
    /// Invalid bias, frequency, or nonphysical/ nonfinite noise power.
    pub fn evaluate(&self, bias: BsimSoiNoiseBias) -> Result<BsimSoiNoisePoint, ModelError> {
        let frequency = positive(bias.frequency, "noise frequency")?;
        let point = self.channel.evaluate(bias.channel)?;
        let drain = self.drain.evaluate(bias.channel)?;
        let (op, junction) = self.operating_point(bias, &point, &drain)?;
        let branch = self.transport.external_series(
            bias.channel.kelvin,
            BsimSoiSeriesBias {
                gate: bias.channel.gate,
                body: bias.channel.body,
            },
            BsimSoiSeriesBias {
                gate: bias.channel.gate - bias.channel.drain,
                body: bias.channel.body - bias.channel.drain,
            },
        )?;
        let gate = self.gate.evaluate(BsimSoiGateBias {
            channel: bias.channel,
            contact: bias.contact,
        })?;
        let parasitics = self
            .parasitics
            .evaluate_with_channel(bias.channel, &drain)?;
        let m = self.geometry.multiplicity;
        let [source, drain_access] = self.thermal.access(
            self.geometry,
            op,
            [branch.source.conductance / m, branch.drain.conductance / m],
            point.reversed,
        );
        let thermal = 4. * K * op.temperature;
        let shot = |current: f64| 2. * Q * current.abs();
        // Native B4SOInoise uses the channel partitions without swapping
        // them on reversal, even though its load routine swaps the stamps.
        let (source_channel, drain_channel) = if point.reversed {
            (gate.drain_channel, gate.source_channel)
        } else {
            (gate.source_channel, gate.drain_channel)
        };
        BsimSoiNoisePoint {
            drain_access: thermal * drain_access * m,
            source_access: thermal * source * m,
            gate_electrode: thermal * self.parasitics.gate_noise_conductance(&parasitics),
            channel_thermal: thermal * self.thermal.conductance(self.geometry, op) * m,
            channel_flicker: self.flicker.evaluate(self.geometry, op, frequency) * m,
            source_body_excess: shot(self.excess * junction[0]),
            drain_body_excess: shot(self.excess * junction[1]),
            gate_source_shot: shot(gate.source_edge.value + source_channel.value),
            gate_drain_shot: shot(gate.drain_edge.value + drain_channel.value),
            gate_body_shot: shot(gate.body.value),
            // Native source-side body resistor alone uses ambient temperature.
            source_body_resistor: 4. * K * self.ambient * parasitics.source_body,
            drain_body_resistor: thermal * parasitics.drain_body,
            body_contact: thermal * self.parasitics.body_contact_noise(),
        }
        .checked()
    }
}

impl PreparedBsimSoiSetup {
    /// Bind all native BSIMSOI noise selectors and their operating-point laws.
    ///
    /// # Errors
    /// Invalid model or instance preparation.
    pub fn prepare_noise(
        &self,
        index: usize,
        ambient: f64,
    ) -> Result<PreparedBsimSoiNoise, ModelError> {
        let channel = self.prepare_channel(index, ambient)?;
        let size = self
            .sizes
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let instance = self
            .instances
            .get(index)
            .ok_or(ModelError::MismatchedPreparedContext)?;
        let model = self.model.parameters();
        let p = instance.parameters();
        let dimensions = size.geometry();
        let epsrox = if integer(model, "mtrlmod")? == 0 {
            real(model, "epsrox")?
        } else {
            3.9
        };
        let xj = super::super::binned(size, "xj")?;
        let litl = if integer(model, "mtrlmod")? == 0 {
            (3. * 3.9 / epsrox * xj * channel.threshold.toxe).sqrt()
        } else {
            (channel.threshold.epssub * xj * channel.threshold.toxe
                / (epsrox * super::super::EPS_0))
                .sqrt()
        };
        Ok(PreparedBsimSoiNoise {
            channel,
            drain: self.prepare_drain_channel(index, ambient)?,
            charges: self.prepare_intrinsic_charges(index, ambient)?,
            junctions: self.prepare_junctions(index, ambient)?,
            gate: self.prepare_gate_tunneling(index, ambient)?,
            impact: self.prepare_impact_ionization(index, ambient)?,
            leakage: self.prepare_gate_induced_leakage(index, ambient)?,
            transport: self.prepare_transport(index, ambient)?,
            parasitics: self.prepare_parasitics(index, ambient)?,
            geometry: NoiseGeometry {
                width: dimensions.weff * real(p, "nf")?,
                length: dimensions.leff,
                cox: channel.threshold.cox,
                litl: positive(litl, "noise characteristic length")?,
                multiplicity: positive(real(p, "m")?, "noise multiplicity")?,
            },
            thermal: ThermalLaw {
                mode: integer(model, "tnoimod")?,
                ntnoi: real(model, "ntnoi")?,
                rnoia: real(model, "rnoia")?,
                rnoib: real(model, "rnoib")?,
                tnoia: real(model, "tnoia")?,
                tnoib: real(model, "tnoib")?,
            },
            flicker: FlickerLaw {
                mode: integer(model, "fnoimod")?,
                kf: real(model, "kf")?,
                af: real(model, "af")?,
                ef: real(model, "ef")?,
                bf: real(model, "bf")?,
                reference_width: real(model, "w0flk")?,
                em: real(model, "em")?,
                traps: [
                    real(model, "noia")?,
                    real(model, "noib")?,
                    real(model, "noic")?,
                ],
            },
            excess: real(model, "noif")?,
            ambient,
            charge_enabled: real(model, "xpart")? >= 0.,
        })
    }
}
