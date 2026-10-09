//! Health measurements for the logs, so a phone log can tell heat, Low Power
//! Mode, a busy graphics chip and memory pressure apart. `IW4L_HEALTH_LOG`
//! (in `iw4l-env.txt` on the phone) is on by default; `0` turns all of it off.
//! Read once at app build. Nothing here changes what the game does.
//!
//! - Every `frame pacing:` line (iOS) ends with `footprint_mb`, `avail_mb`,
//!   `thermal`, `lowpower`, `main_cpu_ms` and `gpu_ms p50/max`; without a
//!   pacing line (desktop, or `IW4L_FRAME_STATS=0`) the same fields go on a
//!   `frame health:` line every 128 frames (`render_gpu::diag::health`).
//! - Apple: the vendored wgpu-hal times every Metal command buffer on the
//!   graphics chip (`metal::gpu_time`) for `gpu_ms`.
//! - A `sim timing:` line every 200 server ticks: Ingress, Step and Fanout
//!   times and the QoS class of the thread that ran Step
//!   (`net::AuthorityTickTiming`).
//! - iOS: every memory warning is logged with the footprint
//!   (`memory_warning` below).
//!
//! The probes and the formatting are in `diag::health`.
use bevy::prelude::*;
use diag::health::HEALTH_LOG_ENV;

pub(crate) fn register(app: &mut App) {
    if !health_log() {
        diag::info!(Launch, "health log: off ({HEALTH_LOG_ENV})");
        return;
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::sync::atomic::Ordering;
        use wgpu_hal::metal::gpu_time;
        gpu_time::ENABLED.store(true, Ordering::Relaxed);
        diag::health::set_gpu_source(diag::health::GpuPresents {
            done: gpu_time::presents_done,
            busy_at: gpu_time::busy_at_present,
        });
    }
    // FramePace is off everywhere but iOS, so only the phone has a pacing line.
    let pacing_line = diag::frame_pacing::FramePace::get().stats;
    render_gpu::diag::health::register(app, !pacing_line);
    app.init_resource::<net::AuthorityTickTiming>();
    #[cfg(target_os = "ios")]
    memory_warning::observe();
    diag::info!(
        Launch,
        "health log: on (frame fields on the {} line, sim timing every {} ticks)",
        if pacing_line {
            "frame pacing"
        } else {
            "frame health"
        },
        diag::health::TICKS_PER_LINE
    );
}

/// On unless set to an off value. `IW4L_HEALTH_LOG=0` in iw4l-env.txt turns
/// every health measurement off.
fn health_log() -> bool {
    match std::env::var_os(HEALTH_LOG_ENV) {
        None => true,
        Some(_) => perf::switch(HEALTH_LOG_ENV),
    }
}

/// iOS memory warnings, logged only: nothing is freed here.
///
/// The vendored winit hears `UIApplicationDidReceiveMemoryWarningNotification`
/// but bevy_winit drops the event, so this adds its own observer to the
/// notification center, once per process. Each warning writes one WARN line
/// with the footprint and what iOS still allows. diag mirrors every warn line
/// into `iw4l-boot.log` on the phone, which survives a jetsam kill, so that one
/// line is also the boot-log crumb.
#[cfg(target_os = "ios")]
mod memory_warning {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{ClassType, class, define_class, msg_send, sel};

    #[link(name = "UIKit", kind = "framework")]
    unsafe extern "C" {
        static UIApplicationDidReceiveMemoryWarningNotification: &'static AnyObject;
    }

    define_class!(
        // SAFETY: NSObject has no subclassing requirements and this type has no
        // Drop impl.
        #[unsafe(super(NSObject))]
        #[name = "IW4LMemoryWarningObserver"]
        struct MemoryWarningObserver;

        impl MemoryWarningObserver {
            #[unsafe(method(memoryWarning:))]
            fn memory_warning(&self, _notification: *mut AnyObject) {
                let memory = diag::health::Sample {
                    footprint_bytes: diag::health::footprint_bytes(),
                    available_bytes: diag::ios_env::available_bytes(),
                    ..Default::default()
                };
                diag::warn!(
                    Launch,
                    "memory warning: iOS asks the app to free memory | {}",
                    memory.memory_fields()
                );
            }
        }
    );

    pub(super) fn observe() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            // SAFETY: plain Foundation messages with their documented signatures.
            // UIKit posts the notification on the main thread, which runs the
            // method; the center does not retain observers, so this one is kept
            // for the life of the process.
            unsafe {
                let observer: Retained<MemoryWarningObserver> =
                    msg_send![MemoryWarningObserver::class(), new];
                let center: Option<Retained<AnyObject>> =
                    msg_send![class!(NSNotificationCenter), defaultCenter];
                let Some(center) = center else {
                    diag::warn!(Launch, "memory warnings: no notification center");
                    return;
                };
                let _: () = msg_send![
                    &*center,
                    addObserver: &*observer,
                    selector: sel!(memoryWarning:),
                    name: UIApplicationDidReceiveMemoryWarningNotification,
                    object: std::ptr::null::<AnyObject>()
                ];
                std::mem::forget(observer);
            }
        });
    }
}
