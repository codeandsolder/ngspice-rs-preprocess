//! Complete HICUM/L2 2.4.0 *physical* operating-point evaluation.
//!
//! One immutable, effective-temperature prepared device owns all source
//! subkernels. It assembles the exact normalized node voltages, evaluates
//! all dc physical currents and charges, NQS trial states and electrothermal
//! power, and exposes source-referred noise spectra at requested frequency.
//! Circuit topology stamps, transient integration, accepted-step state,
//! device multiplicity and solver-local Gmin belong to the simulator.

use super::{
    Polarity, avalanche, external, internal_base, junction, noise, nqs, stamps, substrate,
    temperature, thermal, transport, tunneling,
};
use crate::{EvaluationError, ModelError, validate_batch_lengths};

/// Absolute circuit voltages for the nine named HICUM electrical nodes.
/// Source's thermal rise is prepared separately with model temperature preparation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Nodes {
    /// External base b.
    pub b: f64,
    /// Peripheral base bp.
    pub bp: f64,
    /// Intrinsic base bi.
    pub bi: f64,
    /// External collector c.
    pub c: f64,
    /// Intrinsic collector ci.
    pub ci: f64,
    /// Intrinsic emitter ei.
    pub ei: f64,
    /// External emitter e.
    pub e: f64,
    /// Intrinsic substrate si.
    pub si: f64,
    /// External substrate s.
    pub s: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Bias {
    pub nodes: Nodes,
    /// Source three auxiliary NQS unknowns; the simulator owns their
    /// accepted values and all time integration.
    pub nqs: nqs::State,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalVoltages {
    /// Source normalized, type*(bi-ei), etc. Other resistor voltages
    /// are raw physical node differences and never polarity-scaled.
    pub vbiei: f64,
    pub vbici: f64,
    pub vbpei: f64,
    pub vbpci: f64,
    pub vbci: f64,
    pub vsici: f64,
    pub vsc: f64,
    pub vciei: f64,
    pub vbpbi_raw: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Output {
    /// Native TYPE polarity needed for source-gradient map binding.
    pub polarity: Polarity,
    /// Original physical node voltages, retained so terminal-series power
    /// gradients need no reconstructive I/G divisions or model lookup.
    pub nodes: Nodes,
    pub voltage: LocalVoltages,
    pub diode: super::Currents,
    pub junction: junction::Output,
    pub transfer: transport::Output,
    pub avalanche: avalanche::Output,
    pub substrate: substrate::Output,
    pub tunneling: tunneling::Output,
    pub internal_base: internal_base::Output,
    pub external: external::Output,
    pub nqs: nqs::Output,
    pub thermal: thermal::Output,
}

#[derive(Clone, Copy, Debug)]
pub struct Prepared {
    polarity: Polarity,
    collector_potential: f64,
    collector_potential_slope: f64,
    t0_slope: f64,
    diode: super::Prepared,
    junction: junction::Prepared,
    transport: transport::Prepared,
    avalanche: avalanche::Prepared,
    substrate: substrate::Prepared,
    tunneling: tunneling::Prepared,
    tunneling_location: tunneling::Location,
    internal_base: internal_base::Prepared,
    external: external::Prepared,
    nqs: nqs::Prepared,
    thermal: thermal::Prepared,
    noise: noise::Prepared,
}

#[derive(Clone, Copy)]
struct ThermalDerivatives {
    diode: super::CurrentSensitivity,
    junction: junction::TemperatureSensitivities,
    avalanche: f64,
    tunnel: f64,
    base: internal_base::TemperatureSensitivity,
    external: external::CurrentSensitivity,
    substrate: substrate::TemperatureSensitivity,
    transfer: transport::TemperatureSensitivity,
}

impl Prepared {
    /// # Errors
    /// Validates all prepared families once per temperature trial.
    pub fn new(p: &temperature::Prepared) -> Result<Self, ModelError> {
        Ok(Self {
            polarity: p.polarity,
            collector_potential: p.transport.junction.intrinsic_collector.potential,
            collector_potential_slope: p.junction_parameter_slopes.intrinsic_collector.potential,
            t0_slope: p.transport_temperature_slopes.t0,
            diode: p.diode_kernel()?,
            junction: p.junction_kernel()?,
            transport: p.transport_kernel()?,
            avalanche: p.avalanche_kernel()?,
            substrate: p.substrate_kernel()?,
            tunneling: p.tunneling_kernel()?,
            tunneling_location: if p.auxiliary.tunode == 0 {
                tunneling::Location::Intrinsic
            } else {
                tunneling::Location::Peripheral
            },
            internal_base: p.internal_base_kernel()?,
            external: p.external_kernel(),
            nqs: p.nqs_kernel()?,
            thermal: p.thermal_kernel(),
            noise: p.noise_kernel()?,
        })
    }

    /// Map source model TYPE to internally polarity-normalized local
    /// voltages; source resistors and linear isolation charges use raw
    /// physical voltages and are not type-multiplied.
    ///
    /// # Errors
    /// Rejects nonfinite circuit voltages.
    pub fn normalize(&self, nodes: Nodes) -> Result<LocalVoltages, EvaluationError> {
        let inputs = [
            nodes.b, nodes.bp, nodes.bi, nodes.c, nodes.ci, nodes.ei, nodes.e, nodes.si, nodes.s,
        ];
        if !inputs.iter().all(|value| value.is_finite()) {
            return Err(EvaluationError::NonFiniteBias);
        }
        let sign = match self.polarity {
            Polarity::Npn => 1.0,
            Polarity::Pnp => -1.0,
        };
        let be = sign * (nodes.bi - nodes.ei);
        let bc = sign * (nodes.bi - nodes.ci);
        Ok(LocalVoltages {
            vbiei: be,
            vbici: bc,
            vbpei: sign * (nodes.bp - nodes.ei),
            vbpci: sign * (nodes.bp - nodes.ci),
            vbci: sign * (nodes.b - nodes.ci),
            vsici: sign * (nodes.si - nodes.ci),
            vsc: sign * (nodes.s - nodes.c),
            vciei: be - bc,
            vbpbi_raw: nodes.bp - nodes.bi,
        })
    }

    /// Evaluate the entire source physical device at an arbitrary trial
    /// voltage and NQS state, with no accepted-step side effects.
    ///
    /// # Errors
    /// Returns the first physical branch's numerical or bias error.
    pub fn evaluate(&self, bias: &Bias) -> Result<Output, EvaluationError> {
        let n = bias.nodes;
        let v = self.normalize(n)?;
        let diode = self.diode.evaluate(super::Bias {
            vbiei: v.vbiei,
            vbpei: v.vbpei,
            vbici: v.vbici,
            vbpci: v.vbpci,
            vsici: v.vsici,
        })?;
        let junction = self.junction.evaluate(junction::Bias {
            vbiei: v.vbiei,
            vbpei: v.vbpei,
            vbici: v.vbici,
            vbci: v.vbci,
            vbpci: v.vbpci,
            vsici: v.vsici,
            vsc: v.vsc,
        })?;
        let transfer = self.transport.evaluate(transport::Bias {
            vbiei: v.vbiei,
            vbici: v.vbici,
        })?;
        let avalanche = self.avalanche.evaluate(avalanche::Bias {
            vbici: v.vbici,
            forward: transfer.forward_current,
            intrinsic_collector: junction.intrinsic_collector,
        })?;
        let substrate = self.substrate.evaluate(substrate::Bias {
            vbpci: v.vbpci,
            vsici: v.vsici,
        })?;
        let tunneling = self.tunneling.evaluate(tunneling::Bias {
            vbiei: v.vbiei,
            vbpei: v.vbpei,
            intrinsic: junction.intrinsic_emitter,
            peripheral: junction.peripheral_emitter,
        })?;
        let internal_base = self.internal_base.evaluate(&internal_base::Bias {
            vbpbi: v.vbpbi_raw,
            emitter: junction.intrinsic_emitter,
            collector: junction.intrinsic_collector,
            transport: transfer,
            intrinsic_diode: diode.ibei,
        })?;
        let external = self.external.evaluate(external::Bias {
            vbbp: n.b - n.bp,
            vcci: n.ci - n.c,
            veie: n.ei - n.e,
            vsis: n.si - n.s,
            vbe: n.b - n.e,
            vbpe: n.bp - n.e,
            vbci: n.b - n.ci,
            vbpci: n.bp - n.ci,
        })?;
        let nqs = self.nqs.evaluate(&nqs::Bias {
            transport: transfer,
            state: bias.nqs,
        })?;
        let mut output = Output {
            polarity: self.polarity,
            nodes: n,
            voltage: v,
            diode,
            junction,
            transfer,
            avalanche,
            substrate,
            tunneling,
            internal_base,
            external,
            nqs,
            ..Output::default()
        };
        output.thermal = self.evaluate_thermal(n, &output)?;
        Ok(output)
    }

    fn evaluate_thermal(
        &self,
        nodes: Nodes,
        op: &Output,
    ) -> Result<thermal::Output, EvaluationError> {
        let v = op.voltage;
        let d = op.diode;
        let t = op.transfer;
        let collector = op.avalanche;
        self.thermal.evaluate(thermal::ElectricalBias {
            transport: thermal::Terminal {
                voltage: v.vciei,
                current: t.net_transfer_current.value,
            },
            avalanche: thermal::Avalanche {
                collector_potential: self.collector_potential,
                collector_voltage: v.vbici,
                current: collector.generation.value,
            },
            intrinsic_emitter: thermal::Terminal {
                voltage: v.vbiei,
                current: d.ibei.amperes,
            },
            intrinsic_collector: thermal::Terminal {
                voltage: v.vbici,
                current: d.ibci.amperes,
            },
            peripheral_emitter: thermal::Terminal {
                voltage: v.vbpei,
                current: d.ibep.amperes,
            },
            peripheral_collector: thermal::Terminal {
                voltage: v.vbpci,
                current: d.ijbcx.amperes,
            },
            substrate: thermal::Terminal {
                voltage: v.vsici,
                current: d.ijsc.amperes,
            },
            intrinsic_base: thermal::Series {
                voltage: v.vbpbi_raw,
                resistance: if op.internal_base.shorted {
                    0.0
                } else {
                    op.internal_base.resistance.value
                },
            },
            external_emitter: series(nodes.ei - nodes.e, op.external.emitter),
            external_collector: series(nodes.c - nodes.ci, op.external.external_collector),
            external_base: series(nodes.b - nodes.bp, op.external.external_base),
        })
    }

    /// Exact source HICICK critical-current and forward low-current
    /// transit-delay temperature slopes at fixed source voltages.
    /// Full HICCR implicit Itf/Qf sensitivities are separate.
    ///
    /// # Errors
    /// Rejects invalid source voltages or nonfinite thermal derivatives.
    pub fn transport_knee_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<(f64, f64), EvaluationError> {
        Ok((
            self.transport
                .critical_current_temperature_sensitivity(operating.voltage.vciei)?,
            self.transport
                .low_current_transit_time_temperature_sensitivity(operating.voltage.vbici)?,
        ))
    }

    /// Complete native HICAVL gain and generated current temperature
    /// derivatives. The prepared HICCR source transport operating point
    /// supplies the exact implicit dItf/dTdev coupling directly.
    ///
    /// # Errors
    /// Reports nonphysical source collector capacitance and overflow.
    pub fn avalanche_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<avalanche::TemperatureSensitivity, EvaluationError> {
        let junction_bias = junction::Bias {
            vbiei: operating.voltage.vbiei,
            vbpei: operating.voltage.vbpei,
            vbici: operating.voltage.vbici,
            vbci: operating.voltage.vbci,
            vbpci: operating.voltage.vbpci,
            vsici: operating.voltage.vsici,
            vsc: operating.voltage.vsc,
        };
        self.avalanche.temperature_sensitivity(
            avalanche::Bias {
                vbici: operating.voltage.vbici,
                forward: operating.transfer.forward_current,
                intrinsic_collector: operating.junction.intrinsic_collector,
            },
            operating.avalanche,
            self.junction
                .collector_temperature_sensitivity(junction_bias)?
                .intrinsic,
            self.junction.temperature_slopes().intrinsic_collector,
            operating.transfer.temperature_sensitivity.forward_current,
        )
    }

    /// Native HICAVL gain d/dTdev at fixed forward current, excluding
    /// the separate mandatory source HICCR dItf/dTdev chain.
    ///
    /// # Errors
    /// Reports nonphysical source collector capacitance or overflow.
    pub fn avalanche_gain_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<f64, EvaluationError> {
        let source = junction::Bias {
            vbiei: operating.voltage.vbiei,
            vbpei: operating.voltage.vbpei,
            vbici: operating.voltage.vbici,
            vbci: operating.voltage.vbci,
            vbpci: operating.voltage.vbpci,
            vsici: operating.voltage.vsici,
            vsc: operating.voltage.vsc,
        };
        self.avalanche.gain_temperature_sensitivity(
            avalanche::Bias {
                vbici: operating.voltage.vbici,
                forward: operating.transfer.forward_current,
                intrinsic_collector: operating.junction.intrinsic_collector,
            },
            self.junction
                .collector_temperature_sensitivity(source)?
                .intrinsic,
            self.junction.temperature_slopes().intrinsic_collector,
        )
    }

    /// All seven source physical depletion-junction temperature
    /// derivatives, including distinct QJMOD source-C and farads.
    ///
    /// # Errors
    /// Rejects nonfinite bias or junction-domain overflow.
    pub fn junction_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<junction::TemperatureSensitivities, EvaluationError> {
        self.junction.temperature_sensitivity(junction::Bias {
            vbiei: operating.voltage.vbiei,
            vbpei: operating.voltage.vbpei,
            vbici: operating.voltage.vbici,
            vbci: operating.voltage.vbci,
            vbpci: operating.voltage.vbpci,
            vsici: operating.voltage.vsici,
            vsc: operating.voltage.vsc,
        })
    }

    /// Complete HICTUN source current temperature slope at fixed
    /// physical voltages, including both forward junction C(T)
    /// and source IBETS/ABET(T) bandgap and geometry transforms.
    ///
    /// # Errors
    /// Rejects invalid physical source junction capacitance and overflow.
    pub fn tunneling_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<f64, EvaluationError> {
        let junc_bias = junction::Bias {
            vbiei: operating.voltage.vbiei,
            vbpei: operating.voltage.vbpei,
            vbici: operating.voltage.vbici,
            vbci: operating.voltage.vbci,
            vbpci: operating.voltage.vbpci,
            vsici: operating.voltage.vsici,
            vsc: operating.voltage.vsc,
        };
        self.tunneling.temperature_sensitivity(
            tunneling::Bias {
                vbiei: junc_bias.vbiei,
                vbpei: junc_bias.vbpei,
                intrinsic: operating.junction.intrinsic_emitter,
                peripheral: operating.junction.peripheral_emitter,
            },
            operating.tunneling,
            self.junction.temperature_slopes(),
            self.junction.emitter_temperature_sensitivity(junc_bias)?,
        )
    }

    /// Exact native HICRBI physical resistance, current and high-
    /// frequency shunt charge temperature gradients at fixed source
    /// branch voltages. Input diode, junction and HICCR sensitivities
    /// come from the same prepared physical operating point.
    ///
    /// # Errors
    /// Reports nonfinite derivative or invalid source physical domain.
    pub fn internal_base_temperature_sensitivity(
        &self,
        op: &Output,
    ) -> Result<internal_base::TemperatureSensitivity, EvaluationError> {
        let (emitter, collector) = self
            .junction
            .intrinsic_hiccr_temperature_sensitivity(op.voltage.vbiei, op.voltage.vbici)?;
        let diode = self.diode.temperature_sensitivity(
            super::Bias {
                vbiei: op.voltage.vbiei,
                vbpei: op.voltage.vbpei,
                vbici: op.voltage.vbici,
                vbpci: op.voltage.vbpci,
                vsici: op.voltage.vsici,
            },
            op.diode,
        )?;
        self.internal_base.temperature_sensitivity(
            &internal_base::Bias {
                vbpbi: op.voltage.vbpbi_raw,
                emitter: op.junction.intrinsic_emitter,
                collector: op.junction.intrinsic_collector,
                transport: op.transfer,
                intrinsic_diode: op.diode.ibei,
            },
            internal_base::TemperatureInputs {
                emitter,
                collector,
                intrinsic_diode_current: diode.ibei,
            },
        )
    }

    /// Exact parasitic substrate dI/dTdev and dQdsu/dTdev.
    ///
    /// # Errors
    /// Rejects numerical overflow in temperature sensitivity.
    pub fn substrate_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<substrate::TemperatureSensitivity, EvaluationError> {
        self.substrate.temperature_sensitivity(
            substrate::Bias {
                vbpci: operating.voltage.vbpci,
                vsici: operating.voltage.vsici,
            },
            operating.substrate,
        )
    }

    /// Full native seven-branch diode temperature sensitivities
    /// at fixed polarity-normalized physical voltages.
    ///
    /// # Errors
    /// Rejects numerical overflow in source diode temperature slopes.
    pub fn diode_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<super::CurrentSensitivity, EvaluationError> {
        self.diode.temperature_sensitivity(
            super::Bias {
                vbiei: operating.voltage.vbiei,
                vbpei: operating.voltage.vbpei,
                vbici: operating.voltage.vbici,
                vbpci: operating.voltage.vbpci,
                vsici: operating.voltage.vsici,
            },
            operating.diode,
        )
    }

    /// Exact external-resistor temperature derivatives at fixed physical
    /// voltages; apply dTdev/dRise from the thermal source clamp when
    /// constructing coupled electrothermal nodal columns.
    ///
    /// # Errors
    /// Reports nonfinite derived currents.
    pub fn external_temperature_sensitivity(
        &self,
        operating: &Output,
    ) -> Result<external::CurrentSensitivity, EvaluationError> {
        self.external.temperature_sensitivity(operating.external)
    }

    /// Analytic thermal column in reduced local coordinates, using the
    /// same physical branch incidence as the electrical/charge residual.
    /// # Errors
    /// Invalid constitutive derivative or nonfinite coupled result.
    pub(super) fn temperature_coupling(
        &self,
        layout: super::nodal::Layout,
        op: &Output,
    ) -> Result<super::nodal::TemperatureCoupling, EvaluationError> {
        let slope = op.thermal.d_tdev_d_rise;
        if slope == 0. {
            return Ok(super::nodal::TemperatureCoupling::default());
        }
        let source = ThermalDerivatives {
            diode: self.diode_temperature_sensitivity(op)?,
            junction: self.junction_temperature_sensitivity(op)?,
            avalanche: self.avalanche_temperature_sensitivity(op)?.generation,
            tunnel: self.tunneling_temperature_sensitivity(op)?,
            base: self.internal_base_temperature_sensitivity(op)?,
            external: self.external_temperature_sensitivity(op)?,
            substrate: self.substrate_temperature_sensitivity(op)?,
            transfer: op.transfer.temperature_sensitivity,
        };
        let mut result = super::nodal::TemperatureCoupling {
            d_tf_d_rise: slope * source.transfer.forward_transit_time,
            d_thermal_current_d_rise_from_power: -slope * self.power_temperature_slope(op, &source),
            ..super::nodal::TemperatureCoupling::default()
        };
        self.stamp_thermal_branches(layout, op, &source, &mut result);
        let (current, charge) = self
            .nqs
            .temperature_partials(&op.nqs, &op.transfer, self.t0_slope);
        for (index, (current, charge)) in current.into_iter().zip(charge).enumerate() {
            if let Some(row) = layout.nqs_slot(index) {
                result.current[row] = slope * current;
                result.charge[row] = slope * charge;
            }
        }
        Ok(result)
    }

    fn stamp_thermal_branches(
        &self,
        layout: super::nodal::Layout,
        op: &Output,
        source: &ThermalDerivatives,
        result: &mut super::nodal::TemperatureCoupling,
    ) {
        use super::topology::Branch;
        let ThermalDerivatives {
            diode,
            junction,
            avalanche,
            tunnel,
            base,
            external,
            substrate,
            transfer,
        } = *source;
        let sign = if matches!(self.polarity, Polarity::Npn) {
            1.
        } else {
            -1.
        };
        let slope = op.thermal.d_tdev_d_rise;
        let (intrinsic_tunnel, peripheral_tunnel) = match self.tunneling_location {
            tunneling::Location::Intrinsic => (tunnel, 0.),
            tunneling::Location::Peripheral => (0., tunnel),
        };
        let (delayed_current, delayed_charge) = if op.nqs.active {
            (0., 0.)
        } else {
            (transfer.forward_current, transfer.forward_diffusion_charge)
        };
        let emitters = junction.emitters;
        let collectors = junction.collectors;
        for (branch, current, charge) in [
            (Branch::Rbx, external.external_base, 0.),
            (Branch::Rbi, base.resistor_current, base.hf_shunt_charge),
            (Branch::Rcx, external.external_collector, 0.),
            (Branch::Re, external.emitter, 0.),
            (Branch::Rsu, external.substrate, 0.),
            (
                Branch::IntrinsicEmitter,
                sign * (diode.ibei + diode.irei + transfer.barrier_base_current + intrinsic_tunnel),
                sign * (emitters.intrinsic.charge + delayed_charge),
            ),
            (
                Branch::PeripheralEmitter,
                sign * (diode.ibep + diode.irep + peripheral_tunnel),
                sign * emitters.peripheral.charge,
            ),
            (
                Branch::IntrinsicCollector,
                sign * diode.ibci,
                sign * (collectors.intrinsic.charge + transfer.reverse_diffusion_charge),
            ),
            (
                Branch::ExternalCollectorJunction,
                0.,
                sign * collectors.external.charge,
            ),
            (
                Branch::PeripheralCollector,
                sign * diode.ijbcx,
                sign * (collectors.peripheral.charge + substrate.diffusion_charge),
            ),
            (Branch::TransferForward, sign * delayed_current, 0.),
            (Branch::TransferReverse, sign * transfer.reverse_current, 0.),
            (
                Branch::SubstrateJunction,
                sign * diode.ijsc,
                sign * collectors.substrate.charge,
            ),
            (
                Branch::PeripheralSubstrateJunction,
                0.,
                sign * collectors.peripheral_substrate.charge,
            ),
            (Branch::SubstrateTransfer, sign * substrate.current, 0.),
            (Branch::Avalanche, sign * avalanche, 0.),
        ] {
            let (from, to) = branch.nodes();
            let from = layout.node(from);
            let to = layout.node(to);
            if from != to {
                result.current[from] = slope.mul_add(current, result.current[from]);
                result.current[to] = slope.mul_add(-current, result.current[to]);
                result.charge[from] = slope.mul_add(charge, result.charge[from]);
                result.charge[to] = slope.mul_add(-charge, result.charge[to]);
            }
        }
    }

    fn power_temperature_slope(&self, op: &Output, source: &ThermalDerivatives) -> f64 {
        let diode = source.diode;
        let external = source.external;
        let power = op.thermal.electrical;
        let product = [
            (power.transport, source.transfer.net_transfer_current),
            (power.avalanche, source.avalanche),
            (power.intrinsic_emitter, diode.ibei),
            (power.intrinsic_collector, diode.ibci),
            (power.peripheral_emitter, diode.ibep),
            (power.peripheral_collector, diode.ijbcx),
            (power.substrate, diode.ijsc),
        ]
        .into_iter()
        .map(|(term, current)| term.d_current * current)
        .sum::<f64>();
        // The source avalanche headroom is Vdci(T)-Vbici. Its explicit
        // temperature derivative is independent of the generation slope.
        let avalanche_headroom = power.avalanche.d_voltage * self.collector_potential_slope;
        let internal = power.intrinsic_base.d_resistance * source.base.resistance;
        // A present resistor contributes V*dI/dT. Its independent
        // voltage partial is zero for disabled heat modes or zero drive.
        let nodes = op.nodes;
        let external = [
            (
                power.external_base,
                nodes.b - nodes.bp,
                external.external_base,
            ),
            (
                power.external_collector,
                nodes.ci - nodes.c,
                external.external_collector,
            ),
            (power.external_emitter, nodes.ei - nodes.e, external.emitter),
        ]
        .into_iter()
        .filter(|(term, _, _)| term.d_voltage != 0.)
        .map(|(_, voltage, current)| voltage * current)
        .sum::<f64>();
        product + avalanche_headroom + internal + external
    }

    /// Source-referred noise spectra for the evaluated physical operating
    /// point. This preserves the full complex base/collector correlation
    /// and source-specific flicker placement for later solver transfer.
    ///
    /// # Errors
    /// Rejects nonphysical frequency or nonfinite local operating point.
    pub fn evaluate_noise(
        &self,
        op: &Output,
        frequency_hz: f64,
    ) -> Result<noise::Output, EvaluationError> {
        let ext = op.external;
        self.noise.evaluate(
            noise::OperatingPoint {
                conductances: noise::Conductances {
                    rbx: ext.external_base.siemens,
                    rbi: op.internal_base.branch_conductance,
                    rcx: ext.external_collector.siemens,
                    re: ext.emitter.siemens,
                    rsu: ext.substrate.siemens,
                },
                currents: noise::Currents {
                    it: op.transfer.net_transfer_current.value,
                    ibei: op.diode.ibei.amperes,
                    ibep: op.diode.ibep.amperes,
                    ibci: op.diode.ibci.amperes,
                    ijbcx: op.diode.ijbcx.amperes,
                    ijsc: op.diode.ijsc.amperes,
                    iavl: op.avalanche.generation.value,
                    emitter_resistor: ext.emitter.amperes,
                },
                forward_transit_time: op.transfer.forward_transit_time,
            },
            frequency_hz,
        )
    }

    /// Compose the 18 fixed source electrical current/charge branches
    /// and the three NQS state residuals, including exact local Jacobians.
    ///
    /// # Errors
    /// Reports an overflow when combining individually finite quantities.
    pub fn source_stamps(&self, op: &Output) -> Result<stamps::Output, EvaluationError> {
        stamps::assemble(self.polarity, self.tunneling_location, op)
    }

    /// # Errors
    /// Reports incompatible output buffers or a physical source failure.
    pub fn evaluate_batch(
        &self,
        inputs: &[Bias],
        outputs: &mut [Output],
    ) -> Result<(), EvaluationError> {
        validate_batch_lengths(inputs.len(), outputs.len())?;
        for (input, output) in inputs.iter().zip(outputs) {
            *output = self.evaluate(input)?;
        }
        Ok(())
    }
}

fn series(voltage: f64, branch: external::Resistor) -> thermal::Series {
    thermal::Series {
        voltage,
        resistance: if branch.shorted {
            0.0
        } else {
            1.0 / branch.siemens
        },
    }
}
