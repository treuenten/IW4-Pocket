//! Health measurements for the logs (`IW4L_HEALTH_LOG`, on by default).
//!
//! The phone's logs could not tell heat, Low Power Mode, a busy graphics chip
//! and memory pressure apart. These fields are read when a line is written,
//! never per frame:
//!
//! - `footprint_mb`: `task_info(TASK_VM_INFO).phys_footprint`, what iOS enforces;
//! - `avail_mb`: `os_proc_available_memory()`, what iOS still allows (iOS only);
//! - `thermal`: `NSProcessInfo.thermalState`: nominal, fair, serious or critical;
//! - `lowpower`: `NSProcessInfo.isLowPowerModeEnabled`, 0 or 1;
//! - `main_cpu_ms`: main-thread CPU time per frame over the window, from the
//!   thread CPU clock (`CLOCK_THREAD_CPUTIME_ID`, read on the main thread);
//! - `gpu_ms p50/max`: graphics-chip busy time per frame ([`FrameHealth`]).
//!
//! [`TickTiming`] builds the `sim timing:` line. The Apple probes compile under
//! `target_vendor = "apple"`; elsewhere, and where a probe has no answer
//! (`avail_mb` on macOS), the field reads `n/a`. Only the logic and the probes
//! live here; bootstrap `health` reads the switch and wires them in.
use std::sync::OnceLock;

pub const HEALTH_LOG_ENV: &str = "IW4L_HEALTH_LOG";

/// Server ticks per `sim timing:` line (10 s at 20 Hz).
pub const TICKS_PER_LINE: u32 = 200;
/// Presents read back per line at most: the hal keeps 256, and a window is
/// normally 128, so the slots read are never being rewritten.
const GPU_PRESENTS_READ: u64 = 192;

/// `NSProcessInfoThermalState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Thermal {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl Thermal {
    pub fn from_raw(raw: isize) -> Option<Self> {
        match raw {
            0 => Some(Self::Nominal),
            1 => Some(Self::Fair),
            2 => Some(Self::Serious),
            3 => Some(Self::Critical),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }
}

/// The probes, read once per log line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub footprint_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub thermal: Option<Thermal>,
    pub low_power: Option<bool>,
    /// The main thread's CPU clock; `None` off the main thread or off Apple.
    pub main_cpu_ns: Option<u64>,
}

impl Sample {
    /// Read every probe now. Call on the main thread for `main_cpu_ns`.
    pub fn read() -> Self {
        let (thermal, low_power) = process_info();
        Self {
            footprint_bytes: footprint_bytes(),
            available_bytes: crate::ios_env::available_bytes(),
            thermal,
            low_power,
            main_cpu_ns: main_thread_cpu_ns(),
        }
    }

    /// `footprint_mb=5120 avail_mb=870`.
    pub fn memory_fields(&self) -> String {
        format!(
            "footprint_mb={} avail_mb={}",
            mib(self.footprint_bytes),
            mib(self.available_bytes)
        )
    }
}

/// One window of main frames for the health fields of a frame log line.
///
/// `main_cpu_ms` is the main thread's CPU time over the window divided by the
/// main frames in it. `gpu_ms` is per presented frame: the vendored wgpu-hal
/// (`metal::gpu_time`) keeps a running total of the union of every Metal
/// command buffer's GPU span and records it as each present's command buffer
/// completes, so one frame's figure is the busy time between two presents, cut
/// on the graphics chip's own timeline. The line reads the presents completed
/// since the last line (at most [`GPU_PRESENTS_READ`]).
#[derive(Debug, Default)]
pub struct FrameHealth {
    frames: u32,
    cpu_last_ns: Option<u64>,
    /// The first present the next line reads.
    gpu_next: u64,
    gpu_ms: Vec<f32>,
}

/// The graphics chip's busy totals by present, from the vendored wgpu-hal.
#[derive(Clone, Copy, Debug)]
pub struct GpuPresents {
    /// Presents completed so far.
    pub done: fn() -> u64,
    /// The running busy total in ns when present `n` (from 0) completed.
    pub busy_at: fn(u64) -> u64,
}

impl FrameHealth {
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Once per main frame. `cpu_ns` is read only when the window has no
    /// main-thread CPU baseline yet (the first frame).
    pub fn note_frame(&mut self, cpu_ns: impl FnOnce() -> Option<u64>) {
        self.frames = self.frames.saturating_add(1);
        if self.cpu_last_ns.is_none() {
            self.cpu_last_ns = cpu_ns();
        }
    }

    /// The fields for one line, e.g. `footprint_mb=5120 avail_mb=870
    /// thermal=nominal lowpower=0 main_cpu_ms=14.20 gpu_ms p50/max=6.10/9.80`,
    /// and start the next window. `gpu` is `None` without the hal hook.
    pub fn take_fields(&mut self, sample: &Sample, gpu: Option<GpuPresents>) -> String {
        let main_cpu = match (sample.main_cpu_ns, self.cpu_last_ns) {
            (Some(now), Some(base)) if self.frames > 0 && now >= base => {
                format!("{:.2}", ns_ms(now - base) / self.frames as f32)
            }
            _ => NA.to_owned(),
        };
        self.gpu_ms.clear();
        if let Some(gpu) = gpu {
            let done = (gpu.done)();
            // Present 0's total holds everything before it (the load): skip it.
            let from = self
                .gpu_next
                .max(done.saturating_sub(GPU_PRESENTS_READ))
                .max(1);
            for n in from..done {
                let busy = (gpu.busy_at)(n).saturating_sub((gpu.busy_at)(n - 1));
                self.gpu_ms.push(ns_ms(busy));
            }
            self.gpu_next = done;
        }
        let gpu = if self.gpu_ms.is_empty() {
            NA.to_owned()
        } else {
            self.gpu_ms.sort_by(f32::total_cmp);
            let p50 = crate::frame_pacing::percentile(&self.gpu_ms, 0.5);
            let max = self.gpu_ms.last().copied();
            format!("{}/{}", ms2(p50), ms2(max))
        };
        let line = format!(
            "{} thermal={} lowpower={} main_cpu_ms={main_cpu} gpu_ms p50/max={gpu}",
            sample.memory_fields(),
            sample.thermal.map_or(NA, Thermal::as_str),
            sample.low_power.map_or(NA, |on| if on { "1" } else { "0" }),
        );
        self.frames = 0;
        self.cpu_last_ns = sample.main_cpu_ns;
        line
    }
}

/// The phases of a server tick the `sim timing:` line reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickPhase {
    Ingress,
    Step,
    Fanout,
}

/// [`TICKS_PER_LINE`] server ticks: the Ingress, Step and Fanout times, and the
/// QoS class of the thread that ran Step (Bevy runs the fixed tick on any pool
/// thread), e.g. `sim timing: ticks=200 ingress_ms p50/p90/max=0.21/0.40/1.10 |
/// step_ms p50/p90/max=8.12/12.40/17.95 | fanout_ms p50/p90/max=0.30/0.52/0.90 |
/// step_qos=0x15 step_on_main=0/200`. One class prints alone; a mix prints
/// each with its tick count, most first (`0x15:150,0x21:50`).
#[derive(Debug, Default)]
pub struct TickTiming {
    ticks: u32,
    phase_ms: [Vec<f32>; 3],
    /// (QoS class, ticks), in first-seen order.
    step_qos: Vec<(u32, u32)>,
    step_on_main: u32,
    step_thread_known: u32,
}

impl TickTiming {
    pub fn note_phase(&mut self, phase: TickPhase, ms: f32) {
        let samples = &mut self.phase_ms[phase as usize];
        if samples.len() < TICKS_PER_LINE as usize * 2 {
            samples.push(ms);
        }
    }

    /// On the thread that runs Step: its QoS class and whether it is the main thread.
    pub fn note_step_thread(&mut self, qos: Option<u32>, on_main: Option<bool>) {
        if let Some(class) = qos {
            match self.step_qos.iter_mut().find(|(seen, _)| *seen == class) {
                Some((_, n)) => *n += 1,
                None => self.step_qos.push((class, 1)),
            }
        }
        if let Some(on_main) = on_main {
            self.step_thread_known += 1;
            self.step_on_main += u32::from(on_main);
        }
    }

    /// Close one tick. Every [`TICKS_PER_LINE`] ticks, the `sim timing:` line;
    /// the next window starts.
    pub fn end_tick(&mut self) -> Option<String> {
        self.ticks += 1;
        if self.ticks < TICKS_PER_LINE {
            return None;
        }
        let phase = |samples: &mut Vec<f32>| {
            samples.sort_by(f32::total_cmp);
            [0.5, 0.9]
                .map(|q| crate::frame_pacing::percentile(samples, q))
                .into_iter()
                .chain([samples.last().copied()])
                .map(ms2)
                .collect::<Vec<_>>()
                .join("/")
        };
        let [ingress, step, fanout] = self.phase_ms.each_mut().map(phase);
        self.step_qos
            .sort_by_key(|&(_, ticks)| std::cmp::Reverse(ticks));
        let qos = match self.step_qos.as_slice() {
            [] => NA.to_owned(),
            [(class, _)] => qos_hex(*class),
            mixed => mixed
                .iter()
                .map(|(class, n)| format!("{}:{n}", qos_hex(*class)))
                .collect::<Vec<_>>()
                .join(","),
        };
        let on_main = if self.step_thread_known == 0 {
            NA.to_owned()
        } else {
            format!("{}/{}", self.step_on_main, self.step_thread_known)
        };
        let line = format!(
            "sim timing: ticks={} ingress_ms p50/p90/max={ingress} | step_ms p50/p90/max={step} \
             | fanout_ms p50/p90/max={fanout} | step_qos={qos} step_on_main={on_main}",
            self.ticks
        );
        self.ticks = 0;
        self.phase_ms.iter_mut().for_each(Vec::clear);
        self.step_qos.clear();
        self.step_on_main = 0;
        self.step_thread_known = 0;
        Some(line)
    }
}

const NA: &str = "n/a";

fn ns_ms(ns: u64) -> f32 {
    (ns as f64 / 1e6) as f32
}

fn ms2(value: Option<f32>) -> String {
    value.map_or_else(|| NA.to_owned(), |v| format!("{v:.2}"))
}

fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| NA.to_owned(), |b| (b / (1024 * 1024)).to_string())
}

/// `0x21` user interactive, `0x19` user initiated, `0x15` default, `0x11`
/// utility, `0x09` background, `0x00` unspecified.
fn qos_hex(class: u32) -> String {
    format!("{class:#04x}")
}

static GPU_PRESENTS: OnceLock<GpuPresents> = OnceLock::new();

/// Where the frame lines read the graphics chip's busy time (bootstrap hands
/// in the vendored wgpu-hal's `metal::gpu_time`).
pub fn set_gpu_source(source: GpuPresents) {
    let _ = GPU_PRESENTS.set(source);
}

/// The graphics chip's busy totals by present; `None` without a source.
pub fn gpu_presents() -> Option<GpuPresents> {
    GPU_PRESENTS.get().copied()
}

/// The process footprint the kernel enforces, on every Apple platform.
pub fn footprint_bytes() -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        crate::ios_env::task_vm::phys_footprint()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// The calling thread's CPU clock in ns, only when it is the main thread.
pub fn main_thread_cpu_ns() -> Option<u64> {
    #[cfg(target_vendor = "apple")]
    {
        if on_main_thread() != Some(true) {
            return None;
        }
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid out pointer for the calling thread's CPU clock.
        let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
        (rc == 0).then(|| ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// Whether the calling thread is the process's main thread.
pub fn on_main_thread() -> Option<bool> {
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: no arguments.
        Some(unsafe { libc::pthread_main_np() } == 1)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// The calling thread's QoS class (`pthread_get_qos_class_np`).
pub fn thread_qos() -> Option<u32> {
    #[cfg(target_vendor = "apple")]
    {
        let mut class = 0u32;
        let mut priority = 0;
        // SAFETY: valid out pointers; the class is read as the C `unsigned int`
        // it is, not as libc's Rust enum, so an unlisted value is no UB.
        let rc = unsafe {
            apple::pthread_get_qos_class_np(libc::pthread_self(), &raw mut class, &raw mut priority)
        };
        (rc == 0).then_some(class)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

/// (`thermalState`, `isLowPowerModeEnabled`) of `NSProcessInfo`.
fn process_info() -> (Option<Thermal>, Option<bool>) {
    #[cfg(target_vendor = "apple")]
    {
        apple::process_info()
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        (None, None)
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send, sel};

    use super::Thermal;

    // NSProcessInfo lives in Foundation, which nothing else may have linked
    // (the diag tests).
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}

    unsafe extern "C" {
        pub fn pthread_get_qos_class_np(
            thread: libc::pthread_t,
            class: *mut u32,
            priority: *mut libc::c_int,
        ) -> libc::c_int;
    }

    pub fn process_info() -> (Option<Thermal>, Option<bool>) {
        autoreleasepool(|_| {
            // SAFETY: NSProcessInfo's class property and two read-only properties,
            // documented as safe from any thread; `thermalState` is an NSInteger,
            // `isLowPowerModeEnabled` a BOOL (macOS 12+, hence the check).
            unsafe {
                let info: Option<Retained<AnyObject>> =
                    msg_send![class!(NSProcessInfo), processInfo];
                let Some(info) = info else {
                    return (None, None);
                };
                let thermal: isize = msg_send![&*info, thermalState];
                let has_low_power: bool =
                    msg_send![&*info, respondsToSelector: sel!(isLowPowerModeEnabled)];
                let low_power: Option<bool> =
                    has_low_power.then(|| msg_send![&*info, isLowPowerModeEnabled]);
                (Thermal::from_raw(thermal), low_power)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        Sample {
            footprint_bytes: Some(5120 * 1024 * 1024 + 123),
            available_bytes: Some(870 * 1024 * 1024),
            thermal: Some(Thermal::Fair),
            low_power: Some(true),
            main_cpu_ns: None,
        }
    }

    #[test]
    fn thermal_states_parse_and_print() {
        let all = [0, 1, 2, 3].map(|raw| Thermal::from_raw(raw).map(Thermal::as_str));
        assert_eq!(
            all,
            [
                Some("nominal"),
                Some("fair"),
                Some("serious"),
                Some("critical")
            ]
        );
        assert_eq!(Thermal::from_raw(4), None);
        assert_eq!(Thermal::from_raw(-1), None);
    }

    use std::sync::atomic::{AtomicU64, Ordering};

    /// A fake hal: busy totals at each completed present, 1 ms per frame
    /// unless set.
    static PRESENTS: AtomicU64 = AtomicU64::new(0);
    static BUSY_MS: [AtomicU64; 512] = [const { AtomicU64::new(0) }; 512];

    fn fake_gpu(busy_ms_per_frame: &[u64]) -> GpuPresents {
        let mut done = PRESENTS.load(Ordering::Relaxed);
        for ms in busy_ms_per_frame {
            let last = done
                .checked_sub(1)
                .map_or(0, |n| BUSY_MS[n as usize].load(Ordering::Relaxed));
            BUSY_MS[done as usize].store(last + ms, Ordering::Relaxed);
            done += 1;
        }
        PRESENTS.store(done, Ordering::Relaxed);
        GpuPresents {
            done: || PRESENTS.load(Ordering::Relaxed),
            busy_at: |n| BUSY_MS[n as usize].load(Ordering::Relaxed) * 1_000_000,
        }
    }

    /// One test drives the fake hal, so its statics see one writer.
    #[test]
    fn frame_fields_take_gpu_time_per_present_and_cpu_per_frame() {
        let mut health = FrameHealth::default();
        let mut cpu_reads = 0;
        // The first frame sets the CPU baseline: 1 s before the window.
        health.note_frame(|| {
            cpu_reads += 1;
            Some(1_000_000_000)
        });
        for _ in 0..4 {
            health.note_frame(|| {
                cpu_reads += 1;
                None
            });
        }
        assert_eq!(cpu_reads, 1, "the CPU clock is read once, for the baseline");
        assert_eq!(health.frames(), 5);
        // Present 0 holds the 50 ms load and is skipped; then 6, 5, 7, 12 ms.
        let gpu = fake_gpu(&[50, 6, 5, 7, 12]);
        let line = health.take_fields(
            &Sample {
                main_cpu_ns: Some(1_000_000_000 + 5 * 14_200_000),
                ..sample()
            },
            Some(gpu),
        );
        assert_eq!(
            line,
            "footprint_mb=5120 avail_mb=870 thermal=fair lowpower=1 main_cpu_ms=14.20 \
             gpu_ms p50/max=7.00/12.00"
        );

        // The next window starts from the line's CPU reading and the next
        // present; a line with no new present has no GPU figure.
        assert_eq!(health.frames(), 0);
        health.note_frame(|| panic!("baseline already set"));
        health.note_frame(|| panic!("baseline already set"));
        let gpu = fake_gpu(&[3, 2]);
        let line = health.take_fields(
            &Sample {
                main_cpu_ns: Some(1_000_000_000 + 5 * 14_200_000 + 2 * 10_000_000),
                ..sample()
            },
            Some(gpu),
        );
        assert!(
            line.ends_with("main_cpu_ms=10.00 gpu_ms p50/max=3.00/3.00"),
            "{line}"
        );
        health.note_frame(|| None);
        let line = health.take_fields(&sample(), Some(fake_gpu(&[])));
        assert!(line.ends_with("gpu_ms p50/max=n/a"), "{line}");

        // A line late by more than the hal keeps reads only the newest presents.
        let gpu = fake_gpu(&[1; 300]);
        health.take_fields(&sample(), Some(gpu));
        assert_eq!(health.gpu_ms.len(), GPU_PRESENTS_READ as usize);
    }

    #[test]
    fn missing_probes_print_n_a() {
        let mut health = FrameHealth::default();
        health.note_frame(|| None);
        health.note_frame(|| None);
        let line = health.take_fields(&Sample::default(), None);
        assert_eq!(
            line,
            "footprint_mb=n/a avail_mb=n/a thermal=n/a lowpower=n/a main_cpu_ms=n/a \
             gpu_ms p50/max=n/a"
        );
        // A line with no frames, or a clock that went backwards, has no CPU figure.
        let mut empty = FrameHealth {
            cpu_last_ns: Some(10),
            ..FrameHealth::default()
        };
        assert!(
            empty
                .take_fields(
                    &Sample {
                        main_cpu_ns: Some(20),
                        ..Sample::default()
                    },
                    None
                )
                .contains("main_cpu_ms=n/a")
        );
        assert_eq!(
            Sample {
                footprint_bytes: Some(3 << 20),
                ..Sample::default()
            }
            .memory_fields(),
            "footprint_mb=3 avail_mb=n/a"
        );
    }

    #[test]
    fn tick_line_every_200_ticks_with_phase_percentiles_and_qos() {
        let mut timing = TickTiming::default();
        for tick in 0..TICKS_PER_LINE {
            // Ingress 0.0-1.99 ms, Step 10 ms with one 30 ms spike, Fanout 0.5 ms.
            timing.note_phase(TickPhase::Ingress, tick as f32 / 100.0);
            timing.note_phase(TickPhase::Step, if tick == 7 { 30.0 } else { 10.0 });
            timing.note_phase(TickPhase::Fanout, 0.5);
            timing.note_step_thread(Some(0x15), Some(false));
            let line = timing.end_tick();
            assert_eq!(line.is_some(), tick + 1 == TICKS_PER_LINE, "tick {tick}");
            if let Some(line) = line {
                assert_eq!(
                    line,
                    "sim timing: ticks=200 ingress_ms p50/p90/max=1.00/1.79/1.99 \
                     | step_ms p50/p90/max=10.00/10.00/30.00 \
                     | fanout_ms p50/p90/max=0.50/0.50/0.50 | step_qos=0x15 step_on_main=0/200"
                );
            }
        }

        // A mix of classes prints each, most ticks first; the window restarted.
        for tick in 0..TICKS_PER_LINE {
            let (qos, main) = match tick % 4 {
                0 => (0x21, true),
                1 => (0x09, false),
                _ => (0x15, false),
            };
            timing.note_step_thread(Some(qos), Some(main));
            if let Some(line) = timing.end_tick() {
                assert!(
                    line.ends_with(
                        "| fanout_ms p50/p90/max=n/a/n/a/n/a \
                         | step_qos=0x15:100,0x21:50,0x09:50 step_on_main=50/200"
                    ),
                    "{line}"
                );
            }
        }

        // Off Apple: no thread answers.
        for _ in 0..TICKS_PER_LINE - 1 {
            timing.note_step_thread(None, None);
            assert!(timing.end_tick().is_none());
        }
        let line = timing.end_tick().unwrap();
        assert!(line.ends_with("step_qos=n/a step_on_main=n/a"), "{line}");
    }

    #[test]
    fn switch_is_allowlisted() {
        use crate::memory_settings::valid_file_setting;
        assert!(valid_file_setting(HEALTH_LOG_ENV, "0"));
        assert!(valid_file_setting(HEALTH_LOG_ENV, "1"));
        assert!(!valid_file_setting(HEALTH_LOG_ENV, "off"));
    }

    #[test]
    fn qos_classes_print_as_two_hex_digits() {
        assert_eq!(qos_hex(0x21), "0x21");
        assert_eq!(qos_hex(0x09), "0x09");
        assert_eq!(qos_hex(0), "0x00");
    }

    /// The probes answer on the Mac (and the phone); `avail_mb` is iOS only.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn apple_probes_answer() {
        let sample = Sample::read();
        assert!(sample.footprint_bytes.is_some_and(|b| b > 0));
        assert!(sample.thermal.is_some());
        assert!(sample.low_power.is_some());
        assert_eq!(sample.available_bytes.is_some(), cfg!(target_os = "ios"));
        assert!(thread_qos().is_some());
        assert!(on_main_thread().is_some());
    }

    /// Cost of the line-time probes and the per-frame note (Mac):
    /// `cargo test -p diag --profile play health::tests::cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn cost() {
        use std::hint::black_box;
        use std::time::Instant;
        const N: u32 = 20_000;
        let started = Instant::now();
        for _ in 0..N {
            black_box(Sample::read());
        }
        let read_us = started.elapsed().as_secs_f64() * 1e6 / f64::from(N);
        let mut health = FrameHealth::default();
        let started = Instant::now();
        for _ in 0..N {
            health.note_frame(|| black_box(None));
            if health.frames() == 128 {
                black_box(health.take_fields(&Sample::default(), None));
            }
        }
        let note_ns = started.elapsed().as_secs_f64() * 1e9 / f64::from(N);
        let started = Instant::now();
        for _ in 0..N {
            black_box(thread_qos());
            black_box(on_main_thread());
        }
        let qos_ns = started.elapsed().as_secs_f64() * 1e9 / f64::from(N);
        println!(
            "Sample::read {read_us:.2} us | note_frame (+ take_fields every 128) {note_ns:.1} ns \
             | thread_qos + on_main_thread {qos_ns:.1} ns"
        );
    }
}
