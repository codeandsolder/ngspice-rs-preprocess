//! C ABI for the experimental XSPICE event-runtime bridge.
//!
//! Simulator clocks and electrical bridges remain owned by the C adapter.

use anyhow::{Context, Result, bail};
use model_alb_experimental::{
    AlbDriveStrength, AlbLogicLevel, AlbTimedOutput, OutputResistance, PhysicalOutputDrive,
    PhysicalOutputLoad, RegisterTimingDefaults, StatefulPhysicalEventEngine, TimingEnvironment,
    compile_stateful_physical_engine,
};
use std::ffi::{CStr, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::{ptr, slice};

/// A timestamped output change. State: 0 LOW, 1 HIGH, 2 UNKNOWN.
/// Strength: 0 STRONG, 1 `HIGH_IMPEDANCE`, 2 UNDETERMINED.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct AlbFfiEvent {
    pub time_s: f64,
    pub pin: usize,
    pub state: i32,
    pub strength: i32,
}

const REPLAY_CHECKPOINT_INTERVAL: usize = 64;
// ngspice may accept every transient step. Cloning the register engine on
// every acceptance costs more than replay itself; keep a small already
// committed prefix until an existing scheduler checkpoint can be moved
// into the rollback anchor without cloning its dynamic state.
const ACCEPTED_HISTORY_COMPACTION_INTERVAL: usize = REPLAY_CHECKPOINT_INTERVAL;

#[derive(Clone)]
struct EventSample {
    time_s: f64,
    inputs: Vec<AlbLogicLevel>,
    dc_loads: Option<Box<[PhysicalOutputLoad]>>,
}

struct EventCheckpoint {
    sample_count: usize,
    engine: StatefulPhysicalEventEngine,
}

struct Runtime {
    engine: StatefulPhysicalEventEngine,
    initial_engine: StatefulPhysicalEventEngine,
    committed_time: Option<f64>,
    latest_time: Option<f64>,
    history: Vec<EventSample>,
    checkpoints: Vec<EventCheckpoint>,
    events: Vec<AlbFfiEvent>,
    output_loads: Option<Box<[PhysicalOutputLoad]>>,
}

impl Runtime {
    fn new(engine: StatefulPhysicalEventEngine) -> Self {
        Self {
            initial_engine: engine.clone(),
            engine,
            committed_time: None,
            latest_time: None,
            history: Vec::new(),
            checkpoints: Vec::new(),
            events: Vec::new(),
            output_loads: None,
        }
    }

    fn advance_with_loads(
        &mut self,
        time_s: f64,
        inputs: &[AlbLogicLevel],
        loads: &[PhysicalOutputLoad],
        transient: bool,
    ) -> Result<Vec<AlbTimedOutput>> {
        if loads.len() != self.engine.output_width() || loads.iter().any(|load| !load.is_finite()) {
            bail!("invalid native physical output load vector");
        }
        if transient {
            if let Some(previous) = self.output_loads.as_deref() {
                if !previous.iter().zip(loads).all(|(a, b)| a.same_bits(*b)) {
                    bail!("time-varying native output loads are not supported");
                }
            } else {
                self.output_loads = Some(loads.into());
            }
        }
        if self
            .committed_time
            .is_some_and(|accepted| time_s < accepted)
        {
            bail!("ALB event callback predates the simulator's last accepted timestep");
        }
        // ngspice may reject an analog timestep after a digital callback
        // and retry from an earlier breakpoint. The pure event scheduler
        // deliberately requires increasing time, so replay the retained
        // digital callback history from the nearest immutable checkpoint.
        if self.latest_time.is_some_and(|previous| time_s < previous) {
            self.rewind(time_s)?;
        }
        let transitions = self.engine.advance(time_s, inputs, loads, transient)?;
        self.latest_time = Some(time_s);
        self.history.push(EventSample {
            time_s,
            inputs: inputs.to_vec(),
            dc_loads: (!transient).then(|| loads.into()),
        });
        if self
            .history
            .len()
            .is_multiple_of(REPLAY_CHECKPOINT_INTERVAL)
        {
            self.checkpoints.push(EventCheckpoint {
                sample_count: self.history.len(),
                engine: self.engine.clone(),
            });
        }
        Ok(transitions)
    }

    #[cfg(test)]
    fn advance(&mut self, time_s: f64, inputs: &[AlbLogicLevel]) -> Result<Vec<AlbTimedOutput>> {
        // Existing register rollback tests use the vendor's compensated
        // 100 ohm / -31 pF implicit physical output environment.
        let loads = vec![PhysicalOutputLoad::capacitive(-31e-12); self.engine.output_width()];
        self.advance_with_loads(time_s, inputs, &loads, time_s > 0.0)
    }

    fn rewind(&mut self, time_s: f64) -> Result<()> {
        if !time_s.is_finite() || time_s < 0.0 {
            bail!("ALB rollback time must be finite and nonnegative");
        }
        // Timesteps later than the retried event were rejected by the
        // simulator and must not influence register or timer state.
        let retained = self
            .history
            .partition_point(|sample| sample.time_s <= time_s);
        self.history.truncate(retained);
        while self
            .checkpoints
            .last()
            .is_some_and(|point| point.sample_count > retained)
        {
            self.checkpoints.pop();
        }
        self.engine = self.replay_prefix(retained)?;
        self.latest_time = self.history.last().map(|sample| sample.time_s);
        Ok(())
    }

    fn replay_prefix(&self, end: usize) -> Result<StatefulPhysicalEventEngine> {
        let (mut engine, from) = self
            .checkpoints
            .iter()
            .rev()
            .find(|point| point.sample_count <= end)
            .map_or_else(
                || (self.initial_engine.clone(), 0),
                |point| (point.engine.clone(), point.sample_count),
            );
        for sample in &self.history[from..end] {
            let loads = match &sample.dc_loads {
                Some(loads) => loads,
                None => self
                    .output_loads
                    .as_deref()
                    .context("missing recorded native output loads")?,
            };
            engine.advance(
                sample.time_s,
                &sample.inputs,
                loads,
                sample.dc_loads.is_none(),
            )?;
        }
        Ok(engine)
    }

    fn accept_through(&mut self, accepted_time: f64) -> Result<()> {
        if !accepted_time.is_finite() || accepted_time < 0.0 {
            bail!("ALB accepted timestep must be finite and nonnegative");
        }
        if self
            .committed_time
            .is_some_and(|previous| accepted_time <= previous)
        {
            return Ok(());
        }

        // ngspice's last accepted analog step is a hard rollback floor.
        // Only speculative callbacks newer than that point require replay.
        let count = self
            .history
            .partition_point(|sample| sample.time_s <= accepted_time);
        if count >= ACCEPTED_HISTORY_COMPACTION_INTERVAL {
            // Normal high-frequency acceptance lands exactly on a stored
            // checkpoint. Move it into the anchor (no deep register clone).
            // For partially accepted speculative windows, reconstruct only
            // the short tail after the nearest checkpoint instead.
            let accepted_engine = if let Some(index) = self
                .checkpoints
                .iter()
                .position(|point| point.sample_count == count)
            {
                self.checkpoints.remove(index).engine
            } else if count == self.history.len() {
                self.engine.clone()
            } else {
                self.replay_prefix(count)?
            };
            self.initial_engine = accepted_engine;
            self.history.drain(..count);
            self.checkpoints.retain(|point| point.sample_count > count);
            for point in &mut self.checkpoints {
                point.sample_count -= count;
            }
        }
        // Even when the small accepted prefix remains in history, rollback
        // must never cross the actual simulator-accepted boundary.
        self.committed_time = Some(accepted_time);
        Ok(())
    }
}

impl From<AlbTimedOutput> for AlbFfiEvent {
    fn from(value: AlbTimedOutput) -> Self {
        Self {
            time_s: value.time_s,
            pin: value.pin,
            state: match value.value.level {
                AlbLogicLevel::Zero => 0,
                AlbLogicLevel::One => 1,
                AlbLogicLevel::Unknown => 2,
            },
            strength: match value.value.strength {
                AlbDriveStrength::Strong => 0,
                AlbDriveStrength::HighImpedance => 1,
                AlbDriveStrength::Undetermined => 2,
            },
        }
    }
}

fn diagnostic(buffer: *mut c_char, length: usize, message: &str) {
    if buffer.is_null() || length == 0 {
        return;
    }
    let bytes = message.as_bytes();
    let count = bytes.len().min(length - 1);
    // SAFETY: The caller guarantees a writable error buffer of length bytes.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), buffer.cast::<u8>(), count);
        *buffer.add(count) = 0;
    }
}

/// Construct an event runtime for an original ALB body.
/// Returns null on failure, with a diagnostic in the caller buffer.
///
/// # Safety
/// Source must be a valid NUL-terminated hexadecimal UTF-8 C string.
/// Each nonempty parameter array must be readable for its declared length.
/// A null device pointer indicates DEVICE values are unavailable.
/// The error buffer must contain at least `error_length` writable bytes unless null.
/// The returned handle must be freed exactly once and never shared concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sr_alb_runtime_new(
    source: *const c_char,
    in_count: usize,
    out_count: usize,
    reg_delay: f64,
    port_delay: f64,
    setup_time: f64,
    hold_time: f64,
    min_clock: f64,
    out_res_pos: f64,
    out_res_neg: f64,
    min_sink: f64,
    max_source: f64,
    user_values: *const f64,
    user_count: usize,
    device_values: *const f64,
    device_count: usize,
    user_scale: f64,
    error: *mut c_char,
    error_length: usize,
) -> *mut c_void {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<Runtime> {
        if source.is_null() {
            bail!("source string is null");
        }
        if (user_count > 0 && user_values.is_null())
            || (device_count > 0 && device_values.is_null())
        {
            bail!("parameter vector is null with nonzero length");
        }
        // SAFETY: The C caller guarantees the input vectors are readable
        // for the duration of this constructor, and zero-length slices
        // are created without constructing a reference from a null pointer.
        let user = if user_count == 0 {
            &[][..]
        } else {
            // SAFETY: A nonzero count requires a live non-null array.
            unsafe { slice::from_raw_parts(user_values, user_count) }
        };
        let device = if device_values.is_null() {
            None
        } else if device_count == 0 {
            Some(&[][..])
        } else {
            // SAFETY: Verified non-null above; C owns the live input vector.
            Some(unsafe { slice::from_raw_parts(device_values, device_count) })
        };
        // SAFETY: C caller guarantees readable NUL-terminated source bytes.
        let source = unsafe { CStr::from_ptr(source) };
        let encoded = source
            .to_str()
            .context("invalid hexadecimal source encoding")?;
        let source = String::from_utf8(hex::decode(encoded).context("malformed ALB source hex")?)
            .context("decoded ALB source is not UTF-8")?;
        let engine = compile_stateful_physical_engine(
            &source,
            TimingEnvironment {
                user,
                device,
                user_scale,
                default_register_delay: reg_delay,
                default_output_delay: port_delay,
                register_timing_defaults: RegisterTimingDefaults {
                    setup: setup_time,
                    hold: hold_time,
                    min_clock,
                },
            },
            PhysicalOutputDrive {
                resistance: OutputResistance {
                    positive: out_res_pos,
                    negative: out_res_neg,
                },
                min_sink_a: min_sink,
                max_source_a: max_source,
            },
        )?;
        if engine.input_width() != in_count || engine.output_width() != out_count {
            bail!(
                "ALB source has {} inputs and {} outputs, model has {in_count}/{out_count}",
                engine.input_width(),
                engine.output_width()
            );
        }
        Ok(Runtime::new(engine))
    }));
    match result {
        Ok(Ok(runtime)) => {
            diagnostic(error, error_length, "");
            Box::into_raw(Box::new(runtime)).cast::<c_void>()
        }
        Ok(Err(err)) => {
            diagnostic(error, error_length, &format!("{err:#}"));
            ptr::null_mut()
        }
        Err(_) => {
            diagnostic(error, error_length, "ALB runtime initialization panicked");
            ptr::null_mut()
        }
    }
}

/// Advance a source ALB on an input change or internal timer wakeup.
///
/// Events returned in the borrowed pointer remain valid until the next call
/// or destruction. The next time is positive infinity if no work is queued.
/// Returns zero on success, minus one on failure.
///
/// # Safety
/// Handle must come from a successful constructor and be exclusively owned.
/// Inputs must point to `in_count` readable bytes using values 0, 1, or 2.
/// `output_loads` must point to one readable electrical load per output pin.
/// `transient` is zero for DC and one for transient analysis.
/// Events, `event_count`, and `next_time` must point to writable storage.
/// Error must be writable for `error_length` bytes, unless null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sr_alb_runtime_advance(
    handle: *mut c_void,
    time_s: f64,
    inputs: *const u8,
    in_count: usize,
    output_loads: *const PhysicalOutputLoad,
    transient: u8,
    events: *mut *const AlbFfiEvent,
    event_count: *mut usize,
    next_time: *mut f64,
    error: *mut c_char,
    error_length: usize,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        if handle.is_null()
            || inputs.is_null()
            || output_loads.is_null()
            || transient > 1
            || events.is_null()
            || event_count.is_null()
            || next_time.is_null()
        {
            bail!("null pointer passed to ALB runtime");
        }
        // SAFETY: Contract requires a live exclusive handle.
        let runtime = unsafe { &mut *handle.cast::<Runtime>() };
        // SAFETY: Contract guarantees an input array of `in_count` bytes.
        let inputs = unsafe { slice::from_raw_parts(inputs, in_count) };
        let levels = inputs
            .iter()
            .map(|value| match value {
                0 => Ok(AlbLogicLevel::Zero),
                1 => Ok(AlbLogicLevel::One),
                2 => Ok(AlbLogicLevel::Unknown),
                other => bail!("invalid digital input level {other}"),
            })
            .collect::<Result<Vec<_>>>()?;
        // SAFETY: Caller supplies a live vector of validated output width.
        let loads = unsafe { slice::from_raw_parts(output_loads, runtime.engine.output_width()) };
        let transitions = runtime.advance_with_loads(time_s, &levels, loads, transient != 0)?;
        runtime.events.clear();
        runtime
            .events
            .extend(transitions.into_iter().map(AlbFfiEvent::from));
        // SAFETY: Contract guarantees the three output pointers are writable.
        unsafe {
            *events = runtime.events.as_ptr();
            *event_count = runtime.events.len();
            *next_time = runtime.engine.next_due_time().unwrap_or(f64::INFINITY);
        }
        Ok(())
    }));
    match result {
        Ok(Ok(())) => {
            diagnostic(error, error_length, "");
            0
        }
        Ok(Err(err)) => {
            diagnostic(error, error_length, &format!("{err:#}"));
            -1
        }
        Err(_) => {
            diagnostic(error, error_length, "ALB runtime execution panicked");
            -1
        }
    }
}

/// Retire all speculative EVENT callbacks at or before ngspice's last
/// accepted analog timepoint. A rejected timestep can never precede it.
///
/// Returns zero on success and minus one on failure.
///
/// # Safety
/// Handle must be a live, exclusively owned ALB runtime. The error buffer
/// must contain `error_length` writable bytes unless null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sr_alb_runtime_accept(
    handle: *mut c_void,
    accepted_time: f64,
    error: *mut c_char,
    error_length: usize,
) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        if handle.is_null() {
            bail!("null pointer passed to ALB runtime acceptance");
        }
        // SAFETY: Contract requires a live, exclusively owned handle.
        unsafe { &mut *handle.cast::<Runtime>() }.accept_through(accepted_time)
    }));
    match result {
        Ok(Ok(())) => {
            diagnostic(error, error_length, "");
            0
        }
        Ok(Err(err)) => {
            diagnostic(error, error_length, &format!("{err:#}"));
            -1
        }
        Err(_) => {
            diagnostic(error, error_length, "ALB runtime acceptance panicked");
            -1
        }
    }
}

/// Destroy a live engine. Null is accepted.
///
/// # Safety
/// The argument must be null or an exclusively-owned live handle that has
/// never previously been destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sr_alb_runtime_free(handle: *mut c_void) {
    if !handle.is_null() {
        // SAFETY: Ownership of a live unique handle returns from C.
        unsafe { drop(Box::from_raw(handle.cast::<Runtime>())) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn compile_stateful_event_engine(
        body: &str,
        timing: TimingEnvironment<'_>,
    ) -> Result<StatefulPhysicalEventEngine> {
        compile_stateful_physical_engine(
            body,
            timing,
            PhysicalOutputDrive::unrestricted(OutputResistance::symmetric(100.0)),
        )
    }

    // Source-identical clock/HOLD circuit for two independent acceptance
    // and rejected-timestep regression paths.
    fn hold_capture_engine() -> Result<StatefulPhysicalEventEngine> {
        compile_stateful_event_engine(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=2n, HOLD=1n, CLOCK=in[1]) R=0;
             R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )
    }

    // Independently replay the accepted route from the original engine.
    // The pin count stays generic, rather than assuming two event inputs.
    fn replay_samples<const N: usize>(
        engine: StatefulPhysicalEventEngine,
        samples: &[(f64, [AlbLogicLevel; N])],
    ) -> Result<Runtime> {
        let mut runtime = Runtime::new(engine);
        for &(time, input) in samples {
            runtime.advance(time, &input)?;
        }
        Ok(runtime)
    }

    const CANCELLED_CLOCK_REPLAY: [(f64, [AlbLogicLevel; 2]); 5] = [
        (0.0, [AlbLogicLevel::Zero, AlbLogicLevel::Zero]),
        (5e-9, [AlbLogicLevel::One, AlbLogicLevel::Zero]),
        (10e-9, [AlbLogicLevel::One, AlbLogicLevel::One]),
        (10.8e-9, [AlbLogicLevel::Zero, AlbLogicLevel::One]),
        (12.5e-9, [AlbLogicLevel::Zero, AlbLogicLevel::One]),
    ];

    // Manual end-to-end stress profile for the native adapter's retained
    // history/checkpoints. Runs outside CI with --ignored --nocapture.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual long-run RSS and throughput profile"]
    fn ffi_long_event_history_profile() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        use std::time::Instant;

        let count = std::env::var("NGSPICE_PREPROCESS_PROFILE_EVENTS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(200_000);
        let accept_every = std::env::var("NGSPICE_PREPROCESS_PROFILE_ACCEPT_EVERY")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        let scenario = std::env::var("NGSPICE_PREPROCESS_PROFILE_SCENARIO")
            .unwrap_or_else(|_| "toggling".to_owned());
        let source = if scenario == "readonly" {
            let table_len = std::env::var("NGSPICE_PREPROCESS_PROFILE_READONLY_WORDS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(4096);
            let values = (0..table_len)
                .map(|index| (index % 4).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "READONLY (WIDTH=2) Table[{table_len}]={values};
                 PORT D in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
                 LEVEL (WIDTH=2, DELAY=2n, BITWISE=0, CLOCK=in[2]) R=0;
                 R=Table[D]; Q=R;"
            )
        } else {
            "PORT D in[0:1]; PORT Enable in[2]; PORT Q out[0:1];
             LEVEL (WIDTH=2, DELAY=2n, BITWISE=0, CLOCK=in[2]) R=0;
             R=D; Q=R;"
                .to_owned()
        };
        let engine = compile_stateful_event_engine(
            &source,
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )?;
        let mut runtime = Runtime::new(engine);
        let start = Instant::now();
        for i in 0..count {
            let t = f64::from(i) * 1e-9;
            let inputs = match scenario.as_str() {
                "idle" => [Zero, One, One],
                "data-only" => [if i % 2 == 0 { Zero } else { One }, One, Zero],
                "clocked" => [
                    if i % 4 < 2 { Zero } else { One },
                    One,
                    if i % 2 == 0 { Zero } else { One },
                ],
                "toggling" | "readonly" => [if i % 2 == 0 { Zero } else { One }, One, One],
                other => bail!("unknown ALB profile scenario {other}"),
            };
            runtime.advance(t, &inputs)?;
            if accept_every != 0 && (i + 1) % accept_every == 0 {
                runtime.accept_through(t)?;
            }
        }
        let elapsed = start.elapsed();
        let status = std::fs::read_to_string("/proc/self/status")?;
        let rss = status
            .lines()
            .find(|line| line.starts_with("VmRSS:"))
            .context("VmRSS not available")?;
        println!(
            "event_profile scenario={scenario} count={count} accept_every={accept_every} elapsed_ms={} {} history_len={} history_capacity={} checkpoints={} checkpoints_capacity={} event_sample_bytes={} checkpoint_bytes={}",
            elapsed.as_millis(),
            rss.trim(),
            runtime.history.len(),
            runtime.history.capacity(),
            runtime.checkpoints.len(),
            runtime.checkpoints.capacity(),
            std::mem::size_of::<EventSample>(),
            std::mem::size_of::<EventCheckpoint>(),
        );
        Ok(())
    }

    #[test]
    fn ffi_batched_acceptance_is_bounded_and_preserves_speculative_rollback() -> Result<()> {
        let engine = compile_stateful_event_engine(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=2n, HOLD=800p, CLOCK=in[1]) R=0;
             R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )?;
        let mut runtime = Runtime::new(engine);
        for i in 0..256 {
            let time_s = f64::from(i) * 1e-9;
            let data = if i % 2 == 0 {
                AlbLogicLevel::Zero
            } else {
                AlbLogicLevel::One
            };
            runtime.advance(time_s, &[data, AlbLogicLevel::Zero])?;
            runtime.accept_through(time_s)?;
            assert!(runtime.history.len() < ACCEPTED_HISTORY_COMPACTION_INTERVAL);
            assert!(runtime.checkpoints.is_empty());
        }
        assert!(runtime.history.is_empty());
        assert_eq!(runtime.committed_time, Some(255e-9));
        let high = [AlbLogicLevel::One, AlbLogicLevel::One];
        runtime.advance(257e-9, &high)?;
        let speculative = runtime.advance(259.1e-9, &high)?;
        assert_eq!(speculative.len(), 1);
        assert_eq!(runtime.engine.register_words("R"), Some([1].as_slice()));
        // The analog step was rejected; data now violates the pending
        // clock capture's HOLD window and cancels the rising output.
        runtime.advance(257.2e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        let result = runtime.advance(259.2e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        assert_eq!(result, []);
        assert_eq!(runtime.engine.register_words("R"), Some([0].as_slice()));
        assert!(
            runtime
                .advance(254e-9, &[AlbLogicLevel::One, AlbLogicLevel::Zero])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn ffi_accepted_time_prunes_history_but_retains_pending_rollback() -> Result<()> {
        let engine = hold_capture_engine()?;
        let mut runtime = Runtime::new(engine.clone());
        for (t, input) in [
            (0.0, [AlbLogicLevel::Zero, AlbLogicLevel::Zero]),
            (5e-9, [AlbLogicLevel::One, AlbLogicLevel::Zero]),
            (10e-9, [AlbLogicLevel::One, AlbLogicLevel::One]),
            (12.1e-9, [AlbLogicLevel::One, AlbLogicLevel::One]),
        ] {
            runtime.advance(t, &input)?;
        }
        assert_eq!(runtime.engine.register_words("R"), Some([1].as_slice()));
        runtime.accept_through(10.2e-9)?;
        assert!(runtime.history.len() <= ACCEPTED_HISTORY_COMPACTION_INTERVAL);
        assert_eq!(runtime.committed_time, Some(10.2e-9));
        runtime.advance(10.8e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        assert_eq!(runtime.engine.register_words("R"), Some([0].as_slice()));
        assert!(runtime.engine.next_due_time().is_none());
        runtime.accept_through(10.9e-9)?;
        assert!(runtime.history.len() <= ACCEPTED_HISTORY_COMPACTION_INTERVAL);
        assert_eq!(runtime.checkpoints.len(), 0);
        assert!(
            runtime
                .advance(10.85e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])
                .is_err()
        );
        runtime.advance(12.5e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        let fresh = replay_samples(engine, &CANCELLED_CLOCK_REPLAY)?;
        assert_eq!(runtime.engine.output(), fresh.engine.output());
        assert_eq!(runtime.engine.next_due_time(), fresh.engine.next_due_time());
        Ok(())
    }

    #[test]
    fn ffi_acceptance_prunes_checkpoint_prefix_before_future_rewind() -> Result<()> {
        let engine = compile_stateful_event_engine(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0;
             R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )?;
        let mut runtime = Runtime::new(engine.clone());
        for i in 0..160 {
            let input = [
                if i % 2 == 0 {
                    AlbLogicLevel::Zero
                } else {
                    AlbLogicLevel::One
                },
                AlbLogicLevel::Zero,
            ];
            runtime.advance(f64::from(i) * 1e-9, &input)?;
        }
        assert_eq!(runtime.checkpoints.len(), 2);
        runtime.accept_through(92.5e-9)?;
        assert_eq!(runtime.history.len(), 67);
        assert_eq!(runtime.checkpoints.len(), 1);
        assert_eq!(runtime.checkpoints[0].sample_count, 35);
        runtime.advance(145.5e-9, &[AlbLogicLevel::One, AlbLogicLevel::Zero])?;
        assert_eq!(runtime.history.len(), 54);
        runtime.accept_through(141.0e-9)?;
        assert!(runtime.history.len() <= ACCEPTED_HISTORY_COMPACTION_INTERVAL);
        assert_eq!(runtime.checkpoints.len(), 1);
        runtime.advance(142.5e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::Zero])?;
        let edge = [AlbLogicLevel::One, AlbLogicLevel::One];
        runtime.advance(160e-9, &edge)?;
        let actual = runtime.advance(161e-9, &edge)?;
        let mut fresh = Runtime::new(engine);
        for i in 0..=142 {
            let input = [
                if i % 2 == 0 {
                    AlbLogicLevel::Zero
                } else {
                    AlbLogicLevel::One
                },
                AlbLogicLevel::Zero,
            ];
            fresh.advance(f64::from(i) * 1e-9, &input)?;
        }
        fresh.advance(142.5e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::Zero])?;
        fresh.advance(160e-9, &edge)?;
        let expected = fresh.advance(161e-9, &edge)?;
        assert_eq!(actual, expected);
        assert_eq!(runtime.engine.output(), fresh.engine.output());
        assert_eq!(runtime.engine.next_due_time(), fresh.engine.next_due_time());
        Ok(())
    }

    #[test]
    fn ffi_rollback_replays_pending_clock_capture_without_future_state() -> Result<()> {
        let engine = hold_capture_engine()?;
        let mut runtime = Runtime::new(engine.clone());
        for (time, input) in [
            (0.0, [AlbLogicLevel::Zero, AlbLogicLevel::Zero]),
            (5e-9, [AlbLogicLevel::One, AlbLogicLevel::Zero]),
            (10e-9, [AlbLogicLevel::One, AlbLogicLevel::One]),
        ] {
            runtime.advance(time, &input)?;
        }
        let speculative = runtime.advance(12.1e-9, &[AlbLogicLevel::One, AlbLogicLevel::One])?;
        assert_eq!(speculative.len(), 1);
        assert_eq!(runtime.engine.register_words("R"), Some([1].as_slice()));
        // Native ADC backtracking invalidates the speculative rising Q;
        // the retry changes D inside HOLD and cancels that capture.
        let retried = runtime.advance(10.8e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        assert_eq!(retried.len(), 0);
        let after = runtime.advance(12.5e-9, &[AlbLogicLevel::Zero, AlbLogicLevel::One])?;
        assert_eq!(after.len(), 0);
        assert_eq!(runtime.engine.register_words("R"), Some([0].as_slice()));
        assert!(runtime.engine.next_due_time().is_none());
        let fresh = replay_samples(engine, &CANCELLED_CLOCK_REPLAY)?;
        assert_eq!(runtime.engine.output(), fresh.engine.output());
        assert_eq!(runtime.engine.next_due_time(), fresh.engine.next_due_time());
        Ok(())
    }

    #[test]
    fn ffi_rollback_across_replay_checkpoints_matches_fresh_engine() -> Result<()> {
        let engine = compile_stateful_event_engine(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0];
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0;
             R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )?;
        let mut runtime = Runtime::new(engine.clone());
        let mut reference = Runtime::new(engine);
        for i in 0..160 {
            let time = f64::from(i) * 1e-9;
            let input = [
                if i % 2 == 0 {
                    AlbLogicLevel::Zero
                } else {
                    AlbLogicLevel::One
                },
                AlbLogicLevel::Zero,
            ];
            runtime.advance(time, &input)?;
            if i <= 92 {
                reference.advance(time, &input)?;
            }
        }
        assert_eq!(runtime.checkpoints.len(), 2);
        let current = [AlbLogicLevel::One, AlbLogicLevel::Zero];
        runtime.advance(92.5e-9, &current)?;
        reference.advance(92.5e-9, &current)?;
        assert_eq!(runtime.checkpoints.len(), 1);
        assert_eq!(runtime.engine.output(), reference.engine.output());
        let edge = [AlbLogicLevel::One, AlbLogicLevel::One];
        runtime.advance(100e-9, &edge)?;
        reference.advance(100e-9, &edge)?;
        assert_eq!(
            runtime.engine.next_due_time(),
            reference.engine.next_due_time()
        );
        let actual = runtime.advance(101e-9, &edge)?;
        let expected = reference.advance(101e-9, &edge)?;
        assert_eq!(actual.len(), expected.len());
        assert_eq!(
            actual.first().map(|event| event.pin),
            expected.first().map(|event| event.pin)
        );
        assert_eq!(runtime.engine.output(), reference.engine.output());
        Ok(())
    }

    #[test]
    fn ffi_unknown_history_survives_acceptance_and_discards_rejected_known_bits() -> Result<()> {
        use AlbLogicLevel::{One, Unknown, Zero};
        let engine = compile_stateful_event_engine(
            "PORT D in[0]; PORT CK in[1]; PORT Q out[0];
             EDGE(WIDTH=1,DELAY=2n,CLOCK=in[1]) R=0;
             R=D; Q=R; OUT[1]=in[0];",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
        )?;
        let mut runtime = Runtime::new(engine);
        runtime.advance(0.0, &[One, Zero])?;
        // Commit a checkpoint containing a remembered HIGH despite
        // a sequence of raw UNKNOWN observations.
        for step in 1..64 {
            runtime.advance(f64::from(step) * 1e-9, &[Unknown, Unknown])?;
        }
        runtime.accept_through(64e-9)?;
        assert_eq!(runtime.history.len(), 0);
        assert_eq!(runtime.engine.output()[1].level, One);

        runtime.advance(70e-9, &[Zero, One])?;
        runtime.advance(73e-9, &[Unknown, Unknown])?;
        assert_eq!(runtime.engine.output()[1].level, Zero);
        // The speculative LOW is rejected. Both raw UNKNOWN inputs must
        // replay the retained state rather than the last rejected sample.
        assert_eq!(runtime.advance(69e-9, &[Unknown, One])?, []);
        assert_eq!(runtime.engine.output()[1].level, One);
        let result = runtime.advance(72e-9, &[Unknown, Unknown])?;
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].pin, 0);
        assert_eq!(result[0].value.level, One);
        assert_eq!(runtime.engine.register_words("R"), Some([1].as_slice()));
        // Recovery to HIGH is not a second rising clock.
        assert_eq!(runtime.advance(74e-9, &[Unknown, One])?, []);
        assert!(runtime.engine.next_due_time().is_none());
        Ok(())
    }

    #[test]
    fn ffi_replays_speculative_physical_output_timers() -> Result<()> {
        use AlbLogicLevel::{One, Zero};
        let engine = compile_stateful_physical_engine(
            "PORT D in[0]; PORT Q out[0]; COMB(WIDTH=1,DELAY=1n) R=0; R=D; Q=R;",
            TimingEnvironment {
                user: &[],
                device: None,
                user_scale: 1.0,
                default_register_delay: 1e-9,
                default_output_delay: 1e-9,
                register_timing_defaults: RegisterTimingDefaults::default(),
            },
            PhysicalOutputDrive::unrestricted(OutputResistance::symmetric(1000.0)),
        )?;
        let mut runtime = Runtime::new(engine.clone());
        let mut fresh = Runtime::new(engine);
        let loads = [PhysicalOutputLoad::capacitive(4e-12)];
        for instance in [&mut runtime, &mut fresh] {
            assert_eq!(
                instance
                    .advance_with_loads(0.0, &[Zero], &loads, false)?
                    .len(),
                1
            );
            assert_eq!(
                instance.advance_with_loads(10e-9, &[One], &loads, true)?,
                []
            );
            assert_eq!(
                instance.advance_with_loads(11e-9, &[One], &loads, true)?,
                []
            );
            assert!(
                (instance
                    .engine
                    .next_due_time()
                    .context("missing physical timer")?
                    - 16e-9)
                    .abs()
                    < 1e-18
            );
            instance.accept_through(11e-9)?;
        }
        assert_eq!(
            runtime
                .advance_with_loads(16.1e-9, &[One], &loads, true)?
                .len(),
            1
        );
        for instance in [&mut runtime, &mut fresh] {
            assert_eq!(
                instance.advance_with_loads(12e-9, &[Zero], &loads, true)?,
                []
            );
            assert_eq!(
                instance.advance_with_loads(13e-9, &[Zero], &loads, true)?,
                []
            );
            assert_eq!(
                instance.advance_with_loads(18e-9, &[Zero], &loads, true)?,
                []
            );
            assert!(instance.engine.next_due_time().is_none());
        }
        assert_eq!(runtime.engine.output(), fresh.engine.output());
        assert_eq!(runtime.engine.output()[0].level, Zero);
        assert!(
            runtime
                .advance_with_loads(
                    19e-9,
                    &[Zero],
                    &[PhysicalOutputLoad::capacitive(8e-12)],
                    true
                )
                .is_err()
        );
        Ok(())
    }

    const PARAMETERIZED_EDGE_SOURCE: &str = "PORT D in[0]; PORT CLK in[1]; PORT Q out[0]; \
            EDGE (WIDTH=DEVICE[0], DELAY=USER[0], CLOCK=in[1], BITWISE=DEVICE[1]) R=0; \
            R=D; Q=R;";

    #[test]
    fn ffi_resolves_scaled_user_and_indexed_device_values() -> Result<()> {
        let encoded = CString::new(hex::encode(PARAMETERIZED_EDGE_SOURCE))?;
        let mut error = [0_i8; 256];
        let device = [1.0_f64, 1.0];
        for (user, scale, expected) in [(0.5e-9, 2.0, 11e-9), (0.25e-9, 2.0, 10.5e-9)] {
            let user = [user];
            // SAFETY: Both live Rust arrays are passed by pointer and length.
            let handle = unsafe {
                sr_alb_runtime_new(
                    encoded.as_ptr(),
                    2,
                    1,
                    1e-9,
                    1e-9,
                    0.0,
                    0.0,
                    0.0,
                    100.0,
                    100.0,
                    -1e99,
                    1e99,
                    user.as_ptr(),
                    user.len(),
                    device.as_ptr(),
                    device.len(),
                    scale,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert!(!handle.is_null(), "initialization failed: {error:?}");
            let mut events = ptr::null();
            let mut count = 0_usize;
            let mut next = f64::INFINITY;
            for (time, inputs) in [(0.0, [0, 0]), (5e-9, [1, 0]), (10e-9, [1, 1])] {
                // SAFETY: Live unique runtime, valid buffers and digital inputs.
                let status = unsafe {
                    sr_alb_runtime_advance(
                        handle,
                        time,
                        inputs.as_ptr(),
                        inputs.len(),
                        [PhysicalOutputLoad::capacitive(-31e-12); 32].as_ptr(),
                        1,
                        &raw mut events,
                        &raw mut count,
                        &raw mut next,
                        error.as_mut_ptr(),
                        error.len(),
                    )
                };
                assert_eq!(status, 0, "{error:?}");
            }
            assert!((next - expected).abs() < 1e-15, "{next} vs {expected}");
            // SAFETY: Each runtime is uniquely owned and freed once.
            unsafe { sr_alb_runtime_free(handle) };
        }
        Ok(())
    }

    #[test]
    fn ffi_rejects_missing_parameter_values_and_invalid_buffers() -> Result<()> {
        let encoded = CString::new(hex::encode(PARAMETERIZED_EDGE_SOURCE))?;
        let mut error = [0_i8; 256];
        let device = [1.0_f64, 1.0];
        // The same source must reject absent DEVICE values rather than
        // treating them as zero or silently using a different timing.
        // SAFETY: Source, user buffer and error storage are valid.
        let missing_device = unsafe {
            sr_alb_runtime_new(
                encoded.as_ptr(),
                2,
                1,
                1e-9,
                1e-9,
                0.0,
                0.0,
                0.0,
                100.0,
                100.0,
                -1e99,
                1e99,
                [0.5e-9].as_ptr(),
                1,
                ptr::null(),
                0,
                2.0,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert!(missing_device.is_null());
        // SAFETY: The failed constructor wrote a terminated diagnostic.
        let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_str()?;
        assert!(message.contains("DEVICE[0]"), "{message}");

        // SAFETY: An invalid nonzero length with a null parameter pointer
        // is rejected without constructing a Rust slice from a null address.
        let bad_user = unsafe {
            sr_alb_runtime_new(
                encoded.as_ptr(),
                2,
                1,
                1e-9,
                1e-9,
                0.0,
                0.0,
                0.0,
                100.0,
                100.0,
                -1e99,
                1e99,
                ptr::null(),
                1,
                device.as_ptr(),
                device.len(),
                2.0,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert!(bad_user.is_null());
        Ok(())
    }

    #[test]
    fn ffi_drives_direct_outputs_without_named_ports_at_full_width() -> Result<()> {
        let source = "PORT D in[0]; COMB (WIDTH=1, DELAY=1n) Stage=0; \
                      Stage=D; OUT[0:31]=0; OUT[31]=Stage;";
        let encoded = CString::new(hex::encode(source))?;
        let mut error = [0_i8; 256];
        // SAFETY: Live NUL-terminated source and writable error buffer.
        let handle = unsafe {
            sr_alb_runtime_new(
                encoded.as_ptr(),
                1,
                32,
                1e-9,
                1e-9,
                0.0,
                0.0,
                0.0,
                100.0,
                100.0,
                -1e99,
                1e99,
                ptr::null(),
                0,
                ptr::null(),
                0,
                1.0,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert!(!handle.is_null(), "{error:?}");
        let mut events = ptr::null();
        let mut count = 0_usize;
        let mut next = f64::INFINITY;
        for (step, (time, input, expected_count)) in [
            (0.0, 0_u8, 0_usize),
            (1e-12, 0, 32),
            (5e-9, 1, 0),
            (6e-9, 1, 0),
            (6e-9 + 1e-12, 1, 1),
        ]
        .into_iter()
        .enumerate()
        {
            // SAFETY: The engine is live; input/output arrays and error storage
            // are valid, and event memory is read before the next FFI call.
            let status = unsafe {
                sr_alb_runtime_advance(
                    handle,
                    time,
                    (&raw const input).cast::<u8>(),
                    1,
                    [PhysicalOutputLoad::capacitive(-31e-12); 32].as_ptr(),
                    1,
                    &raw mut events,
                    &raw mut count,
                    &raw mut next,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(status, 0, "{error:?}");
            assert_eq!(count, expected_count);
            if step == 1 {
                // SAFETY: The successful call returned exactly 32 initialized
                // events; the buffer is borrowed until the next FFI call.
                let initial = unsafe { slice::from_raw_parts(events, count) };
                for (pin, event) in initial.iter().enumerate() {
                    assert_eq!(event.pin, pin);
                    assert_eq!(event.state, 0);
                }
            } else if step < 4 {
                let expected_next = match step {
                    0 => 1e-12,
                    2 => 6e-9,
                    _ => 6e-9 + 1e-12,
                };
                assert!((next - expected_next).abs() < 1e-18);
            } else {
                // SAFETY: The successful call reported exactly one event.
                let event = unsafe { *events };
                assert_eq!(event.pin, 31);
                assert_eq!(event.state, 1);
                assert!((event.time_s - (6e-9 + 1e-12)).abs() < 1e-15);
            }
        }
        // SAFETY: The handle is uniquely owned and freed exactly once.
        unsafe { sr_alb_runtime_free(handle) };
        Ok(())
    }

    #[test]
    fn ffi_round_trip_with_internal_wakeup() -> Result<()> {
        let source = CString::new(
            "PORT D in[0]; PORT CLK in[1]; PORT Q out[0]; \
             EDGE (WIDTH=1, DELAY=1n, CLOCK=in[1]) R=0; R=D; Q=R;",
        )?;
        let encoded = CString::new(hex::encode(source.as_bytes()))?;
        let mut error = [0_i8; 256];
        // SAFETY: Valid CString, buffer and matching port counts.
        let handle = unsafe {
            sr_alb_runtime_new(
                encoded.as_ptr(),
                2,
                1,
                1e-9,
                1e-9,
                0.0,
                0.0,
                0.0,
                100.0,
                100.0,
                -1e99,
                1e99,
                ptr::null(),
                0,
                ptr::null(),
                0,
                1.0,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if handle.is_null() {
            bail!("failed to construct ALB runtime: {error:?}");
        }
        let mut events = ptr::null();
        let mut count = 0;
        let mut next = f64::INFINITY;
        for (time, bits) in [(0.0, [0, 0]), (5e-9, [1, 0]), (10e-9, [1, 1])] {
            // SAFETY: Handle and all referenced buffers remain live.
            let result = unsafe {
                sr_alb_runtime_advance(
                    handle,
                    time,
                    bits.as_ptr(),
                    2,
                    [PhysicalOutputLoad::capacitive(-31e-12); 32].as_ptr(),
                    1,
                    &raw mut events,
                    &raw mut count,
                    &raw mut next,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(result, 0, "{error:?}");
        }
        assert_eq!(count, 0);
        assert!((next - 11e-9).abs() < 1e-15);
        for expected_count in [0, 1] {
            // SAFETY: Same live runtime, valid input and output storage.
            let result = unsafe {
                sr_alb_runtime_advance(
                    handle,
                    next,
                    [1_u8, 1].as_ptr(),
                    2,
                    [PhysicalOutputLoad::capacitive(-31e-12); 32].as_ptr(),
                    1,
                    &raw mut events,
                    &raw mut count,
                    &raw mut next,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert_eq!(result, 0, "{error:?}");
            assert_eq!(count, expected_count);
            if expected_count == 0 {
                assert!((next - (11e-9 + 1e-12)).abs() < 1e-18);
            }
        }
        assert_eq!(count, 1);
        // SAFETY: Count was one and the live engine owns the event storage.
        let event = unsafe { *events };
        assert_eq!(event.state, 1);
        assert!((event.time_s - (11e-9 + 1e-12)).abs() < 1e-18);
        // SAFETY: Unique handle, destroyed once.
        unsafe { sr_alb_runtime_free(handle) };
        Ok(())
    }
}
