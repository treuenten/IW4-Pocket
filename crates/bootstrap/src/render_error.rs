//! A graphics error still quits the app, as Bevy does by default, but now says why.
//!
//! bevy_render 0.19 quits on any wgpu error (out of memory, validation, internal,
//! device lost) and logs the reason only through `tracing`. On iOS that reaches the
//! system log alone, so the app closed and neither iw4l-boot.log nor the per-run log
//! said which error it was. This handler writes the error type and text to both
//! (`diag::fatal`), then quits the same way Bevy's own handler does.

use std::error::Error;

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::render::error_handler::{RenderError, RenderErrorHandler, RenderErrorPolicy};

/// Built after Bevy's render plugin, whose default handler this replaces.
pub(crate) struct RenderErrorRecordPlugin;

impl Plugin for RenderErrorRecordPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RenderErrorHandler(record_and_quit));
    }
}

/// The error was written down; Bevy calls the handler every frame until the app is gone.
#[derive(Resource)]
struct RenderErrorRecorded;

fn record_and_quit(
    error: &RenderError,
    main_world: &mut World,
    _render_world: &mut World,
) -> RenderErrorPolicy {
    if !main_world.contains_resource::<RenderErrorRecorded>() {
        main_world.insert_resource(RenderErrorRecorded);
        diag::fatal(diag::Channel::Launch, &render_error_report(error));
    }
    // Bevy's policy, unchanged: going on after an out-of-memory or validation error
    // tends to hit it again every frame.
    main_world.write_message(AppExit::error());
    RenderErrorPolicy::StopRendering
}

/// The error type, wgpu's text and each underlying cause that text does not already
/// carry (an out-of-memory error has no text of its own), and on iOS the memory then.
fn render_error_report(error: &RenderError) -> String {
    let mut report = format!("GRAPHICS ERROR (wgpu {:?}), quitting:", error.ty);
    let description = error.description.trim();
    if !description.is_empty() {
        report.push(' ');
        report.push_str(description);
    }
    let mut cause: Option<&(dyn Error + 'static)> = error
        .source
        .as_ref()
        .map(|source| &***source as &(dyn Error + 'static));
    while let Some(error) = cause {
        let text = error.to_string();
        let text = text.trim();
        if !text.is_empty() && !report.contains(text) {
            report.push_str("\n  caused by: ");
            report.push_str(text);
        }
        cause = error.source();
    }
    if let Some(footprint) = diag::ios_env::footprint_bytes() {
        report.push_str(&format!("\n  footprint {} MiB", footprint >> 20));
        if let Some(available) = diag::ios_env::available_bytes() {
            report.push_str(&format!(", iOS still allows {} MiB", available >> 20));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::message::Messages;
    use bevy::render::error_handler::ErrorType;
    use bevy::render::renderer::WgpuWrapper;

    #[derive(Debug)]
    struct Cause(&'static str, Option<Box<Cause>>);

    impl std::fmt::Display for Cause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    impl Error for Cause {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.1
                .as_deref()
                .map(|cause| cause as &(dyn Error + 'static))
        }
    }

    fn error(ty: ErrorType, description: &str, source: Option<Cause>) -> RenderError {
        RenderError {
            ty,
            description: description.to_owned(),
            source: source.map(|cause| WgpuWrapper::new(Box::new(cause) as _)),
        }
    }

    #[test]
    fn the_report_names_the_type_and_every_cause_not_already_in_the_text() {
        let validation = error(
            ErrorType::Validation,
            "Validation Error\n\nCaused by:\n  In Device::create_buffer\n    Buffer size 9 GiB is too big\n",
            Some(Cause(
                "In Device::create_buffer",
                Some(Box::new(Cause("Buffer size 9 GiB is too big", None))),
            )),
        );
        assert_eq!(
            render_error_report(&validation),
            "GRAPHICS ERROR (wgpu Validation), quitting: Validation Error\n\nCaused by:\n  In Device::create_buffer\n    Buffer size 9 GiB is too big"
        );

        let out_of_memory = error(
            ErrorType::OutOfMemory,
            "",
            Some(Cause(
                "Not enough memory left",
                Some(Box::new(Cause(
                    "MTLDevice newBufferWithLength returned nil",
                    None,
                ))),
            )),
        );
        assert_eq!(
            render_error_report(&out_of_memory),
            "GRAPHICS ERROR (wgpu OutOfMemory), quitting:\n  caused by: Not enough memory left\n  caused by: MTLDevice newBufferWithLength returned nil"
        );

        let lost = error(ErrorType::DeviceLost, "Device lost: GPU hang", None);
        assert_eq!(
            render_error_report(&lost),
            "GRAPHICS ERROR (wgpu DeviceLost), quitting: Device lost: GPU hang"
        );
    }

    #[test]
    fn the_handler_records_once_and_quits_every_time_it_is_asked() {
        let mut main_world = World::new();
        main_world.init_resource::<Messages<AppExit>>();
        let mut render_world = World::new();
        let lost = error(ErrorType::DeviceLost, "Device lost: GPU hang", None);
        for _ in 0..2 {
            let policy = record_and_quit(&lost, &mut main_world, &mut render_world);
            assert!(matches!(policy, RenderErrorPolicy::StopRendering));
        }
        assert!(main_world.contains_resource::<RenderErrorRecorded>());
        let exits = main_world.resource::<Messages<AppExit>>();
        assert_eq!(exits.len(), 2);
        assert!(
            exits
                .iter_current_update_messages()
                .all(|exit| *exit == AppExit::error())
        );
    }

    #[test]
    fn the_plugin_puts_this_handler_in_place_of_bevys() {
        let mut app = App::new();
        app.add_plugins(RenderErrorRecordPlugin);
        let handler = app.world().resource::<RenderErrorHandler>().0;
        let mut main_world = World::new();
        main_world.init_resource::<Messages<AppExit>>();
        let lost = error(ErrorType::DeviceLost, "Device lost: GPU hang", None);
        handler(&lost, &mut main_world, &mut World::new());
        assert!(main_world.contains_resource::<RenderErrorRecorded>());
    }
}
