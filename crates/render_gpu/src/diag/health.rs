//! Health fields for the frame log lines (`IW4L_HEALTH_LOG`, on by default;
//! bootstrap `health` registers this).
//!
//! [`FrameHealthLog`] counts the main frames and is read out when a line is
//! written: the iOS `frame pacing:` line appends its fields, and where there
//! is no pacing line (desktop, or `IW4L_FRAME_STATS=0`) a `frame health:` line
//! carries them every [`LOG_EVERY`] frames. The probes and the graphics chip's
//! per-present busy times (`diag::health`) are read only when a line is written.
use bevy::prelude::*;
use diag::frame_pacing::LOG_EVERY;
use diag::health::{FrameHealth, Sample};

#[derive(Resource, Default)]
pub struct FrameHealthLog(FrameHealth);

impl FrameHealthLog {
    /// Once per main frame, from a `Last` system: `Last` is single-threaded,
    /// so this runs on the main thread, whose CPU clock it reads.
    pub(crate) fn note_frame(&mut self) {
        self.0.note_frame(diag::health::main_thread_cpu_ns);
    }

    /// Read the probes and format the fields; starts the next window.
    pub(crate) fn take_fields(&mut self) -> String {
        self.0
            .take_fields(&Sample::read(), diag::health::gpu_presents())
    }
}

/// `own_line`: log `frame health:` lines (no `frame pacing:` line to join).
pub fn register(app: &mut App, own_line: bool) {
    app.init_resource::<FrameHealthLog>();
    if own_line {
        app.add_systems(Last, log_frame_health);
    }
}

fn log_frame_health(mut log: ResMut<FrameHealthLog>) {
    log.note_frame();
    let frames = log.0.frames();
    if frames < LOG_EVERY {
        return;
    }
    let fields = log.take_fields();
    diag::info!(World, "frame health: n={frames} | {fields}");
}
