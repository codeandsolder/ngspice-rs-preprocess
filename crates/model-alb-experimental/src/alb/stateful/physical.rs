//! Native physical output timing, deliberately separate from internal
//! register commits. A fixed electrical load belongs to each physical
//! output pin, not to any register or logical expression.

use super::events::event_due_at_boundary;
use super::{AlbDigitalBit, AlbDriveStrength, AlbLogicLevel, AlbTimedOutput, StatefulEventEngine};
use crate::alb::{
    MIN_PROPAGATION_DELAY_S, PhysicalOutputDrive, PhysicalOutputLoad, StatefulProgram,
    TimingEnvironment, resolve_output_delay,
};
use anyhow::{Result, ensure};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
struct PendingPhysicalOutput {
    source_time_s: f64,
    event: AlbTimedOutput,
}

/// The original register scheduler followed by independent inertial output
/// drivers. The entire state is cloneable for native simulator rollback.
#[derive(Clone, Debug)]
pub struct StatefulPhysicalEventEngine {
    logical: StatefulEventEngine,
    base_delays: Arc<[f64]>,
    drive: PhysicalOutputDrive,
    driven: Vec<AlbDigitalBit>,
    pending: Vec<Option<PendingPhysicalOutput>>,
    initialized: Vec<bool>,
}

impl StatefulProgram {
    /// Construct a native timed ALB with a separate physical output stage.
    ///
    /// # Errors
    /// Rejects invalid register/output timing or output resistance.
    pub fn physical_event_engine(
        &self,
        environment: TimingEnvironment<'_>,
        drive: PhysicalOutputDrive,
    ) -> Result<StatefulPhysicalEventEngine> {
        drive.validate()?;
        let logical = self.event_engine(environment)?;
        let mut delays = vec![environment.default_output_delay; self.output_width];
        for port in &self.outputs {
            let delay = resolve_output_delay(port.delay.as_ref(), environment)?;
            delays[port.first..=port.last].fill(delay);
        }
        let driven = vec![
            AlbDigitalBit {
                level: AlbLogicLevel::Unknown,
                strength: AlbDriveStrength::Undetermined,
            };
            self.output_width
        ];
        Ok(StatefulPhysicalEventEngine {
            logical,
            base_delays: Arc::from(delays),
            drive,
            driven,
            pending: vec![None; self.output_width],
            initialized: vec![false; self.output_width],
        })
    }
}

impl StatefulPhysicalEventEngine {
    #[must_use]
    pub const fn input_width(&self) -> usize {
        self.logical.input_width()
    }

    #[must_use]
    pub const fn output_width(&self) -> usize {
        self.driven.len()
    }

    #[must_use]
    pub fn output(&self) -> &[AlbDigitalBit] {
        &self.driven
    }

    #[must_use]
    pub fn register_words(&self, name: &str) -> Option<&[u32]> {
        self.logical.register_words(name)
    }

    #[must_use]
    pub fn next_due_time(&self) -> Option<f64> {
        self.logical
            .next_due_time()
            .into_iter()
            .chain(
                self.pending
                    .iter()
                    .flatten()
                    .map(|pending| pending.event.time_s),
            )
            .min_by(f64::total_cmp)
    }

    fn drain_output_events(&mut self, until: f64, result: &mut Vec<AlbTimedOutput>) {
        let start = result.len();
        for (pin, pending) in self.pending.iter_mut().enumerate() {
            if pending.is_some_and(|pending| {
                event_due_at_boundary(pending.event.time_s, pending.source_time_s, until)
            }) && let Some(pending) = pending.take()
            {
                self.driven[pin] = pending.event.value;
                result.push(pending.event);
            }
        }
        // Preserve the exact nominal deadline ordering before snapping
        // roundoff-equivalent future events to the actual callback time.
        result[start..].sort_by(|a, b| a.time_s.total_cmp(&b.time_s).then(a.pin.cmp(&b.pin)));
        for event in &mut result[start..] {
            if event.time_s > until {
                event.time_s = until;
            }
        }
    }

    /// Advance at an external input sample or timer wakeup.
    ///
    /// DC resolves current limits without propagation delay. Transient
    /// callbacks apply both current limits and signed capacitive delay.
    ///
    /// # Errors
    /// Invalid loads, timing, or logical evaluation.
    pub fn advance(
        &mut self,
        time_s: f64,
        inputs: &[AlbLogicLevel],
        loads: &[PhysicalOutputLoad],
        transient: bool,
    ) -> Result<Vec<AlbTimedOutput>> {
        ensure!(
            loads.len() == self.output_width(),
            "wrong native output load width"
        );
        for (load, delay) in loads.iter().zip(self.base_delays.iter()) {
            ensure!(
                load.is_finite()
                    && [
                        self.drive.resistance.positive,
                        self.drive.resistance.negative
                    ]
                    .into_iter()
                    .all(|r| r.mul_add(load.capacitance_f, *delay).is_finite()),
                "nonfinite native output load or effective delay"
            );
        }
        let candidates = self.logical.advance(time_s, inputs)?;
        let candidates = if transient {
            candidates
        } else {
            self.logical
                .output()
                .iter()
                .copied()
                .enumerate()
                .map(|(pin, value)| AlbTimedOutput { time_s, pin, value })
                .collect()
        };
        let mut result = Vec::new();
        for mut event in candidates {
            self.drain_output_events(event.time_s, &mut result);
            let pin = event.pin;
            event.value = self.drive.resolve(event.value, loads[pin]);
            // Disabling a port preserves its last requested polarity,
            // even if the corresponding drive event is still pending.
            if event.value.strength == AlbDriveStrength::HighImpedance {
                event.value.level = if transient && self.initialized[pin] {
                    self.pending[pin]
                        .map_or(self.driven[pin], |old| old.event.value)
                        .level
                } else {
                    // The initial dormant polarity is a current-limited LOW,
                    // independent of the logical data input.
                    self.drive
                        .resolve(
                            AlbDigitalBit {
                                level: AlbLogicLevel::Zero,
                                strength: AlbDriveStrength::Strong,
                            },
                            loads[pin],
                        )
                        .level
                };
            }
            // Logical candidates are emitted only for changed requests.
            // Even if two requests map to the same overloaded UNKNOWN,
            // a new request supersedes the previous pending transition.
            // A committed identical drive still needs no new event.
            self.pending[pin] = None;
            if self.initialized[pin] && self.driven[pin] == event.value {
                continue;
            }
            let delay = if transient {
                self.drive
                    .resistance
                    .for_transition(event.value)
                    .mul_add(loads[pin].capacitance_f, self.base_delays[pin])
                    .max(MIN_PROPAGATION_DELAY_S)
            } else {
                0.0
            };
            let source_time_s = event.time_s;
            event.time_s += delay;
            ensure!(event.time_s.is_finite(), "native output deadline overflow");
            self.pending[pin] = Some(PendingPhysicalOutput {
                source_time_s,
                event,
            });
            self.initialized[pin] = true;
        }
        self.drain_output_events(time_s, &mut result);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alb::analyze_body;

    fn timing_engine(source: &str) -> Result<StatefulPhysicalEventEngine> {
        crate::alb::compile_stateful_physical_engine(
            source,
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive::unrestricted(crate::alb::OutputResistance::symmetric(0.0)),
        )
    }

    #[test]
    fn rounded_physical_deadline_precedes_boundary_input_at_callback_time() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let mut engine = timing_engine(
            "PORT A in[0]; PORT B in[1]; PORT (DELAY=1n) Q out[0];
             PORT (DELAY=0) Z out[1]; COMB(WIDTH=1,DELAY=0) R=0;
             R=A; Q=R; Z=B;",
        )?;
        let loads = [PhysicalOutputLoad::default(); 2];
        engine.advance(0.0, &[Zero, Zero], &loads, false)?;
        assert_eq!(engine.advance(10e-9, &[One, Zero], &loads, true)?, []);
        let due = engine
            .next_due_time()
            .ok_or_else(|| anyhow::anyhow!("missing physical deadline"))?;
        let boundary = f64::from_bits(due.to_bits() - 1);
        let transitions = engine.advance(boundary, &[One, One], &loads, true)?;
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].pin, 0);
        assert_eq!(transitions[0].time_s.to_bits(), boundary.to_bits());
        let next = boundary + MIN_PROPAGATION_DELAY_S;
        let transitions = engine.advance(next, &[One, One], &loads, true)?;
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].pin, 1);
        assert_eq!(transitions[0].time_s.to_bits(), next.to_bits());
        assert!(engine.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn positive_physical_delay_remains_pending_at_its_source_time() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let mut engine = timing_engine(
            "PORT A in[0]; PORT (DELAY=2e-24) Q out[0];
             COMB(WIDTH=1,DELAY=0) R=0; R=A; Q=R;",
        )?;
        let loads = [PhysicalOutputLoad::default()];
        engine.advance(0.0, &[Zero], &loads, false)?;
        let source = 10e-9;
        assert_eq!(engine.advance(source, &[One], &loads, true)?, []);
        assert_eq!(engine.advance(source, &[One], &loads, true)?, []);
        let due = engine
            .next_due_time()
            .ok_or_else(|| anyhow::anyhow!("missing positive physical delay"))?;
        assert_eq!(due.to_bits(), (source + MIN_PROPAGATION_DELAY_S).to_bits());
        let events = engine.advance(due, &[One], &loads, true)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].time_s.to_bits(), due.to_bits());
        assert_eq!(events[0].value.level, One);
        assert!(engine.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn directional_drive_resolves_current_limits_before_physical_timing() -> Result<()> {
        use AlbLogicLevel::{One, Unknown, Zero};
        let mut engine = crate::alb::compile_stateful_physical_engine(
            "PORT D in[0]; PORT EN in[1]; PORT Q out[0:2];
             COMB(WIDTH=1,DELAY=1n) R=0; R=D; Q=R ? 7 : 0; Q.En=EN ? 7 : 0;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive {
                resistance: crate::alb::OutputResistance {
                    positive: 1000.0,
                    negative: 2000.0,
                },
                min_sink_a: -0.01,
                max_source_a: 0.01,
            },
        )?;
        let loads = [(-0.01, 0.01), (-0.005, 0.015), (-0.015, 0.005)].map(|(sink_a, source_a)| {
            PhysicalOutputLoad {
                capacitance_f: 4e-12,
                sink_a,
                source_a,
            }
        });
        let dc = engine.advance(0.0, &[Zero, One], &loads, false)?;
        assert_eq!(dc.len(), 3);
        assert_eq!(
            engine
                .output()
                .iter()
                .map(|bit| bit.level)
                .collect::<Vec<_>>(),
            [Zero, Zero, Unknown]
        );
        assert_eq!(engine.advance(10e-9, &[One, One], &loads, true)?, []);
        assert_eq!(engine.advance(11e-9, &[One, One], &loads, true)?, []);
        let rise = engine.advance(16e-9, &[One, One], &loads, true)?;
        assert_eq!(
            rise.iter().map(|event| event.pin).collect::<Vec<_>>(),
            [0, 2]
        );
        assert!(rise.iter().all(|event| event.value.level == One));
        let overload = engine.advance(18e-9, &[One, One], &loads, true)?;
        assert_eq!(overload.len(), 1);
        assert_eq!(overload[0].pin, 1);
        assert_eq!(overload[0].value.level, Unknown);
        assert_eq!(overload[0].value.strength, AlbDriveStrength::Strong);
        assert_eq!(engine.advance(20e-9, &[One, Zero], &loads, true)?, []);
        let release = engine.advance(25e-9, &[One, Zero], &loads, true)?;
        assert_eq!(
            release.iter().map(|event| event.pin).collect::<Vec<_>>(),
            [0, 2]
        );
        assert!(
            release
                .iter()
                .all(|event| event.value.strength == AlbDriveStrength::HighImpedance)
        );
        let release_unknown = engine.advance(27e-9, &[One, Zero], &loads, true)?;
        assert_eq!(release_unknown.len(), 1);
        assert_eq!(release_unknown[0].pin, 1);
        assert_eq!(
            release_unknown[0].value.strength,
            AlbDriveStrength::HighImpedance
        );
        engine.advance(30e-9, &[Zero, Zero], &loads, true)?;
        engine.advance(31e-9, &[Zero, Zero], &loads, true)?;
        assert_eq!(engine.advance(40e-9, &[Zero, One], &loads, true)?, []);
        let overload = engine.advance(47e-9, &[Zero, One], &loads, true)?;
        assert_eq!(overload.len(), 1);
        assert_eq!(overload[0].pin, 2);
        assert_eq!(overload[0].value.strength, AlbDriveStrength::Strong);
        let fall = engine.advance(49e-9, &[Zero, One], &loads, true)?;
        assert_eq!(
            fall.iter().map(|event| event.pin).collect::<Vec<_>>(),
            [0, 1]
        );
        assert!(fall.iter().all(|event| event.value.level == Zero));
        Ok(())
    }

    #[test]
    fn initially_disabled_output_retains_current_limited_low() -> Result<()> {
        use AlbLogicLevel::{One, Unknown, Zero};
        let loads = [
            (-0.01, 0.01),
            (-0.005, 0.015),
            (-0.015, 0.005),
            (-0.015, 0.015),
        ]
        .map(|(sink_a, source_a)| PhysicalOutputLoad {
            capacitance_f: 4e-12,
            sink_a,
            source_a,
        });
        for (initial, changed) in [(Zero, One), (One, Zero)] {
            let mut engine = crate::alb::compile_stateful_physical_engine(
                "PORT D in[0]; PORT EN in[1]; PORT Q out[0:3];
                 COMB(WIDTH=1,DELAY=0) R=0; R=D; Q=R ? 15 : 0; Q.En=EN ? 15 : 0;",
                TimingEnvironment {
                    user: &[],
                    device: None,
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 4e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                },
                PhysicalOutputDrive {
                    resistance: crate::alb::OutputResistance {
                        positive: 1000.0,
                        negative: 2000.0,
                    },
                    min_sink_a: -0.01,
                    max_source_a: 0.01,
                },
            )?;
            // Initial event callbacks may precede downstream load registration.
            let unloaded = [PhysicalOutputLoad::default(); 4];
            assert_eq!(
                engine
                    .advance(0.0, &[initial, Zero], &unloaded, false)?
                    .len(),
                4
            );
            assert_eq!(
                engine.advance(0.0, &[initial, Zero], &loads, false)?.len(),
                2
            );
            let expected = [Zero, Zero, Unknown, Unknown].map(|level| AlbDigitalBit {
                level,
                strength: AlbDriveStrength::HighImpedance,
            });
            assert_eq!(engine.output(), expected);
            assert_eq!(engine.advance(10e-9, &[changed, Zero], &loads, true)?, []);
            assert!(engine.next_due_time().is_none());
            assert_eq!(engine.output(), expected);
        }
        Ok(())
    }

    #[test]
    fn distinct_logical_requests_restart_pending_overloaded_output() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let mut engine = crate::alb::compile_stateful_physical_engine(
            "PORT D in[0]; PORT EN in[1]; PORT (DELAY=4n) Q out[0];
             COMB(WIDTH=1,DELAY=0) R=1; R=D; Q=R; Q.En=EN;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 4e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive {
                resistance: crate::alb::OutputResistance {
                    positive: 1000.0,
                    negative: 2000.0,
                },
                min_sink_a: -0.01,
                max_source_a: 0.01,
            },
        )?;
        let loads = [PhysicalOutputLoad {
            capacitance_f: 4e-12,
            sink_a: -0.015,
            source_a: 0.015,
        }];
        engine.advance(0.0, &[One, Zero], &loads, false)?;
        assert_eq!(engine.advance(10e-9, &[One, One], &loads, true)?, []);
        let first = engine.next_due_time();
        assert_eq!(engine.advance(14e-9, &[One, One], &loads, true)?, []);
        assert_eq!(engine.next_due_time(), first);
        // Both logical values resolve to UNKNOWN, but the new request
        // supersedes the previous not-yet-delivered physical event.
        assert_eq!(engine.advance(15e-9, &[Zero, One], &loads, true)?, []);
        assert_eq!(engine.advance(20e-9, &[Zero, One], &loads, true)?, []);
        assert_eq!(engine.output()[0].strength, AlbDriveStrength::HighImpedance);
        let events = engine.advance(25e-9, &[Zero, One], &loads, true)?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value.strength, AlbDriveStrength::Strong);
        assert!((events[0].time_s - 25e-9).abs() < 1e-18);
        // Once the physical UNKNOWN has committed, a different logical
        // request mapping to it needs no redundant physical event.
        assert_eq!(engine.advance(30e-9, &[One, One], &loads, true)?, []);
        assert!(engine.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn disabling_uses_pending_requested_polarity_in_both_directions() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        for (old, new, positive, negative) in
            [(Zero, One, 2000.0, 1000.0), (One, Zero, 1000.0, 2000.0)]
        {
            let mut engine = crate::alb::compile_stateful_physical_engine(
                "PORT D in[0]; PORT EN in[1]; PORT Q out[0];
                 COMB(WIDTH=1,DELAY=1n) R=0; R=D; Q=R; Q.En=EN;",
                TimingEnvironment {
                    user: &[],
                    device: None,
                    user_scale: 1.0,
                    default_register_delay: 1e-9,
                    default_output_delay: 4e-9,
                    register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
                },
                PhysicalOutputDrive::unrestricted(crate::alb::OutputResistance {
                    positive,
                    negative,
                }),
            )?;
            let loads = [PhysicalOutputLoad::capacitive(4e-12)];
            engine.advance(0.0, &[old, One], &loads, false)?;
            engine.advance(10e-9, &[old, One], &loads, true)?;
            // Ensure the initial register word is actually the old drive.
            engine.advance(20e-9, &[old, One], &loads, true)?;
            assert_eq!(engine.output()[0].level, old);
            engine.advance(30e-9, &[new, One], &loads, true)?;
            engine.advance(31e-9, &[new, One], &loads, true)?;
            assert_eq!(engine.advance(40e-9, &[new, Zero], &loads, true)?, []);
            assert_eq!(engine.output()[0].level, old);
            assert_eq!(engine.advance(49e-9, &[new, Zero], &loads, true)?, []);
            let release = engine.advance(52e-9, &[new, Zero], &loads, true)?;
            assert_eq!(release.len(), 1);
            assert_eq!(release[0].value.level, new);
            assert_eq!(release[0].value.strength, AlbDriveStrength::HighImpedance);
            assert!((release[0].time_s - 52e-9).abs() < 1e-18);
            // Changing data while disabled does not change dormant polarity.
            assert_eq!(engine.advance(60e-9, &[old, Zero], &loads, true)?, []);
            assert_eq!(engine.advance(61e-9, &[old, Zero], &loads, true)?, []);
            assert_eq!(engine.output()[0].level, new);
        }
        Ok(())
    }

    #[test]
    fn signed_directional_resistance_preserves_both_load_signs() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let mut engine = crate::alb::compile_stateful_physical_engine(
            "PORT D in[0]; PORT (DELAY=1n) Q out[0]; COMB(WIDTH=1,DELAY=0) R=0; R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 0.0,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive::unrestricted(crate::alb::OutputResistance {
                positive: -1000.0,
                negative: 1000.0,
            }),
        )?;
        // Negative resistance with negative capacitance extends a rising
        // deadline; the opposite falling direction saturates at 1 ps.
        let loads = [PhysicalOutputLoad::capacitive(-2e-12)];
        engine.advance(0.0, &[Zero], &loads, false)?;
        assert_eq!(engine.advance(10e-9, &[One], &loads, true)?, []);
        assert!((engine.next_due_time().unwrap_or_default() - 13e-9).abs() < 1e-18);
        let rise = engine.advance(13e-9, &[One], &loads, true)?;
        assert_eq!(rise.len(), 1);
        assert_eq!(rise[0].value.level, One);
        assert_eq!(engine.advance(20e-9, &[Zero], &loads, true)?, []);
        let due = 20e-9 + MIN_PROPAGATION_DELAY_S;
        let fall = engine.advance(due, &[Zero], &loads, true)?;
        assert_eq!(fall.len(), 1);
        assert_eq!(fall[0].value.level, Zero);
        assert_eq!(fall[0].time_s, due);
        Ok(())
    }

    #[test]
    fn output_timing_is_inertial_per_pin_and_separate_from_register_commits() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let (_, program) = analyze_body("PORT D in[0]; PORT (DELAY=USER[0]) A out[0]; PORT (DELAY=0) B out[1]; COMB(WIDTH=1,DELAY=1n) R=0; R=D; A=R; B=R; OUT[2]=R;")?.into_parts();
        let program = program.ok_or_else(|| anyhow::anyhow!("missing stateful program"))?;
        let mut engine = program.physical_event_engine(
            TimingEnvironment {
                user: &[1e-9],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: crate::alb::RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive::unrestricted(crate::alb::OutputResistance::symmetric(1000.0)),
        )?;
        let loads = [0.0, 4e-12, -31e-12].map(PhysicalOutputLoad::capacitive);
        assert_eq!(engine.advance(0.0, &[Zero], &loads, false)?.len(), 3);
        assert_eq!(engine.advance(10e-9, &[One], &loads, true)?, []);
        let first = engine.advance(11e-9, &[One], &loads, true)?;
        assert_eq!(engine.register_words("R"), Some([1].as_slice()));
        assert_eq!(first, []);
        let first_due = 11e-9 + MIN_PROPAGATION_DELAY_S;
        assert_eq!(engine.next_due_time(), Some(first_due));
        let first = engine.advance(first_due, &[One], &loads, true)?;
        assert_eq!(first.iter().map(|e| e.pin).collect::<Vec<_>>(), [2]);
        assert!((engine.next_due_time().unwrap_or_default() - 12e-9).abs() < 1e-18);
        let second = engine.advance(12e-9, &[Zero], &loads, true)?;
        assert_eq!(second.iter().map(|e| e.pin).collect::<Vec<_>>(), [0]);
        assert_eq!(engine.advance(13e-9, &[Zero], &loads, true)?, []);
        let third = engine.advance(13e-9 + MIN_PROPAGATION_DELAY_S, &[Zero], &loads, true)?;
        assert_eq!(third.iter().map(|e| e.pin).collect::<Vec<_>>(), [2]);
        let settled = engine.advance(20e-9, &[Zero], &loads, true)?;
        assert_eq!(settled.iter().map(|e| e.pin).collect::<Vec<_>>(), [0]);
        assert!(engine.next_due_time().is_none());
        assert!(engine.output().iter().all(|bit| bit.level == Zero));
        Ok(())
    }
}
