//! Bevy and wgpu (and a few of the game's own settings files) report problems
//! through `tracing`. On iOS, bevy_log hands those events only to the system log
//! (os_log), which the app's files never see: a graphics or asset warning before a
//! silent exit left no trace in iw4l-boot.log or the per-run log. This layer copies
//! each WARN and ERROR event into diag, which writes both on iOS.
//!
//! Elsewhere the events already reach stderr through bevy_log's own formatter, so the
//! layer is only installed on iOS.
//!
//! wgpu and naga log through `log`, and bevy_log's LogTracer hands every `log` record
//! to `tracing` on one call site per level, target "log", with the record's own target,
//! file and line in `log.*` fields. Such a line is named and thinned by those, as a
//! `tracing` event is by its call site: otherwise all of wgpu's warnings would share
//! one budget of [`FIRST_LINES`] and print as `[log]`.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::sync::Mutex;

use bevy::log::BoxedLayer;
use bevy::log::tracing::{
    Event, Level, Subscriber,
    callsite::Identifier,
    field::{Field, Visit},
};
use bevy::log::tracing_subscriber::{Layer, layer::Context};
use bevy::prelude::App;

/// Each call site has its first lines copied, then one line in [`EVERY_LATER`]: a
/// warning raised every frame must not turn into a write every frame.
const FIRST_LINES: u64 = 32;
const EVERY_LATER: u64 = 1024;

/// `LogPlugin::custom_layer`: the diag copy on iOS, nothing elsewhere.
pub(crate) fn diag_layer(_app: &mut App) -> Option<BoxedLayer> {
    cfg!(target_os = "ios").then(|| Box::new(DiagLayer::new(write_to_diag)) as BoxedLayer)
}

fn write_to_diag(level: diag::Level, line: &str) {
    diag::write_event(diag::Channel::Launch, level, line, None, None);
}

pub(crate) struct DiagLayer {
    sink: fn(diag::Level, &str),
    lines: Mutex<HashMap<Site, u64>>,
}

/// Where a line comes from: its `tracing` call site, plus a `log` record's own
/// origin (all `log` records share the call site of their level).
#[derive(PartialEq, Eq, Hash)]
struct Site {
    callsite: Identifier,
    log: Option<LogOrigin>,
}

/// A `log` record's target, file and line, from tracing-log's `log.*` fields.
#[derive(Default, PartialEq, Eq, Hash)]
struct LogOrigin {
    target: String,
    file: Option<String>,
    line: Option<u64>,
}

impl Visit for LogOrigin {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "log.target" => self.target = value.to_owned(),
            "log.file" => self.file = Some(value.to_owned()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "log.line" {
            self.line = Some(value);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

impl DiagLayer {
    fn new(sink: fn(diag::Level, &str)) -> Self {
        Self {
            sink,
            lines: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: Subscriber> Layer<S> for DiagLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            Level::ERROR => diag::Level::Error,
            Level::WARN => diag::Level::Warn,
            _ => return,
        };
        let log = metadata.fields().field("log.target").map(|_| {
            let mut origin = LogOrigin::default();
            event.record(&mut origin);
            origin
        });
        let target = match &log {
            Some(origin) if !origin.target.is_empty() => origin.target.clone(),
            _ => metadata.target().to_owned(),
        };
        let site = Site {
            callsite: metadata.callsite(),
            log,
        };
        let n = {
            let mut lines = self
                .lines
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let n = lines.entry(site).or_insert(0);
            *n += 1;
            *n
        };
        if n > FIRST_LINES && !n.is_multiple_of(EVERY_LATER) {
            return;
        }
        let mut line = EventLine::default();
        event.record(&mut line);
        let mut text = format!("[{target}] {}{}", line.message, line.fields);
        if n > FIRST_LINES {
            let _ = write!(
                text,
                " (line {n} from here; one in {EVERY_LATER} is copied)"
            );
        }
        (self.sink)(level, &text);
    }
}

#[derive(Default)]
struct EventLine {
    message: String,
    fields: String,
}

impl Visit for EventLine {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message.push_str(value),
            // `log` records (wgpu logs through `log`) carry their origin here; the
            // line's target is taken from it (`LogOrigin`).
            name if name.starts_with("log.") => {}
            name => {
                let _ = write!(self.fields, " {name}={value}");
            }
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        match field.name() {
            "message" => {
                let _ = write!(self.message, "{value:?}");
            }
            name if name.starts_with("log.") => {}
            name => {
                let _ = write!(self.fields, " {name}={value:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::log::tracing;
    use bevy::log::tracing_subscriber::{prelude::*, registry::Registry};

    static LINES: Mutex<Vec<(diag::Level, String)>> = Mutex::new(Vec::new());

    fn capture(level: diag::Level, line: &str) {
        LINES.lock().unwrap().push((level, line.to_owned()));
    }

    #[test]
    fn warnings_and_errors_are_copied_and_a_repeating_one_is_thinned() {
        let subscriber = Registry::default().with(DiagLayer::new(capture));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("not copied");
            tracing::warn!(target: "wgpu_core::device::resource", "buffer {} is mapped", 7);
            tracing::error!(target: "bevy_render::error_handler", reason = ?"Lost", "Caught DeviceLost error");
            tracing::warn!(
                target: "wgpu_hal::metal",
                { log.target = "wgpu_hal::metal", log.line = 12 },
                "shader warning"
            );
            tracing::debug!("not copied either");
            for _ in 0..3000 {
                tracing::warn!(target: "spam", "every frame");
            }
        });
        let lines = LINES.lock().unwrap();
        assert_eq!(
            lines[..3],
            [
                (
                    diag::Level::Warn,
                    "[wgpu_core::device::resource] buffer 7 is mapped".to_owned()
                ),
                (
                    diag::Level::Error,
                    "[bevy_render::error_handler] Caught DeviceLost error reason=\"Lost\""
                        .to_owned()
                ),
                (
                    diag::Level::Warn,
                    "[wgpu_hal::metal] shader warning".to_owned()
                ),
            ]
        );
        let spam: Vec<_> = lines[3..].iter().map(|(_, line)| line.as_str()).collect();
        assert_eq!(spam.len(), 32 + 2);
        assert!(spam[..32].iter().all(|line| *line == "[spam] every frame"));
        assert_eq!(
            spam[32..],
            [
                "[spam] every frame (line 1024 from here; one in 1024 is copied)",
                "[spam] every frame (line 2048 from here; one in 1024 is copied)",
            ]
        );
    }

    static LOG_LINES: Mutex<Vec<(diag::Level, String)>> = Mutex::new(Vec::new());

    fn capture_log(level: diag::Level, line: &str) {
        LOG_LINES.lock().unwrap().push((level, line.to_owned()));
    }

    fn log_record(level: log::Level, target: &str, file: &str, line: u32, message: &str) {
        tracing_log::format_trace(
            &log::Record::builder()
                .args(format_args!("{message}"))
                .level(level)
                .target(target)
                .module_path(Some(target))
                .file(Some(file))
                .line(Some(line))
                .build(),
        )
        .unwrap();
    }

    /// `log` records reach the layer as bevy_log's LogTracer hands them on: one call
    /// site per level, target "log". Each is still named and thinned by its own origin.
    #[test]
    fn log_records_are_named_and_thinned_by_their_own_origin() {
        let subscriber = Registry::default().with(DiagLayer::new(capture_log));
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..40 {
                log_record(
                    log::Level::Warn,
                    "wgpu_hal::metal::surface",
                    "src/metal/surface.rs",
                    210,
                    "drawable is late",
                );
            }
            log_record(
                log::Level::Warn,
                "naga::back::msl",
                "src/back/msl/writer.rs",
                88,
                "unsupported sampler",
            );
            log_record(
                log::Level::Warn,
                "wgpu_hal::metal::surface",
                "src/metal/surface.rs",
                300,
                "surface lost",
            );
            log_record(
                log::Level::Error,
                "wgpu_core::device::resource",
                "src/device/resource.rs",
                12,
                "device lost",
            );
        });
        let lines = LOG_LINES.lock().unwrap();
        assert_eq!(lines.len(), 32 + 3);
        assert!(lines[..32].iter().all(|line| *line
            == (
                diag::Level::Warn,
                "[wgpu_hal::metal::surface] drawable is late".to_owned()
            )));
        assert_eq!(
            lines[32..],
            [
                (
                    diag::Level::Warn,
                    "[naga::back::msl] unsupported sampler".to_owned()
                ),
                (
                    diag::Level::Warn,
                    "[wgpu_hal::metal::surface] surface lost".to_owned()
                ),
                (
                    diag::Level::Error,
                    "[wgpu_core::device::resource] device lost".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn the_layer_is_installed_on_ios_only() {
        assert_eq!(
            diag_layer(&mut App::new()).is_some(),
            cfg!(target_os = "ios")
        );
    }
}
