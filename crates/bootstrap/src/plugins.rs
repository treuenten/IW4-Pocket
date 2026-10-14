use assets::AssetPlugin;
use audio::AudioPlugin;
use bevy::{
    app::FixedMain,
    ecs::schedule::ScheduleLabel,
    log::LogPlugin,
    prelude::*,
    render::{
        RenderPlugin as BevyRenderPlugin,
        pipelined_rendering::PipelinedRenderingPlugin,
        settings::{RenderCreation, WgpuFeatures, WgpuSettings},
    },
};
use bots::BotsPlugin;
use console::ConsolePlugin;
use frame::RuntimeRole;
use hud::HudPlugin;
use net::NetPlugin;
use render::RenderPlugin;
use replay::ReplayPlugin;
use session::SessionPlugin;
use ui::UiPlugin;

pub fn add_runtime_plugins(app: &mut App) {
    add_runtime_plugins_with_role(app, RuntimeRole::Listen);
}

pub fn add_runtime_plugins_with_role(app: &mut App, role: RuntimeRole) {
    let net = match role {
        RuntimeRole::Listen => NetPlugin::listen(),
        RuntimeRole::Dedicated => NetPlugin::dedicated(),
        RuntimeRole::Client => NetPlugin::client(),
        RuntimeRole::Replay => NetPlugin::replay(),
    };
    if crate::bench::enabled() {
        // Unfocused benchmarks must not inherit the window runner's 60 Hz sleep.
        app.insert_resource(bevy::winit::WinitSettings::continuous());
    }
    app.insert_resource(audio::AudioRuntime::new(
        role != RuntimeRole::Dedicated && !audio::AudioSilent::active(),
    ));
    // Before any effect can play: the source of effect randomness is fixed for the run.
    fx_iw4::set_fast_random(fx_fast_random());
    app.add_plugins(AssetPlugin)
        .add_plugins(UiPlugin)
        .add_plugins(ConsolePlugin)
        .add_plugins(net)
        .add_plugins((BotsPlugin, HudPlugin))
        .add_plugins(AudioPlugin)
        .add_plugins(ReplayPlugin)
        .add_plugins(RenderPlugin)
        .add_plugins(SessionPlugin);
    // iOS present pacing, dt snap and frame timing stats; nothing elsewhere.
    crate::frame_pace::register(app);
    // Memory, thermal, CPU, graphics-chip and server tick fields in the logs.
    crate::health::register(app);
    // iOS: ask a 120 Hz display for its full refresh during matches.
    crate::promotion::register(app);
    crate::render_scale::register(app);
    // iOS: no auto-lock while a map loads or a match runs.
    crate::idle_timer::register(app);
    // Internal builds: which commit is running, on the main menu.
    crate::build_watermark::register(app);
    // Render setup can finish after Startup, so watch for the resource.
    app.add_systems(
        First,
        note_gpu_bc.run_if(resource_added::<bevy::image::CompressedImageFormatSupport>),
    );

    app.edit_schedule(Update, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });

    app.edit_schedule(First, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(PreUpdate, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(PostUpdate, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    app.edit_schedule(Last, |schedule| {
        schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
    });
    if tick_on_main() {
        // Bevy already runs the first two single-threaded; set here as well so
        // the whole fixed tick is in one place.
        for label in [
            RunFixedMainLoop.intern(),
            FixedMain.intern(),
            FixedFirst.intern(),
            FixedPreUpdate.intern(),
            FixedUpdate.intern(),
            FixedPostUpdate.intern(),
            FixedLast.intern(),
        ] {
            app.edit_schedule(label, |schedule| {
                schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
            });
        }
    }

    if let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) {
        render_app.edit_schedule(bevy::render::renderer::RenderGraph, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::core_pipeline::Core3d, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::core_pipeline::Core2d, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::render::Render, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        render_app.edit_schedule(bevy::render::ExtractSchedule, |schedule| {
            schedule.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
        if pipelined_rendering() && cfg!(any(target_os = "macos", target_os = "ios")) {
            // Pipelined, the Render schedule runs on the render thread, and with the
            // single-threaded executors above Bevy never hands `create_surfaces` (a
            // NonSend system) back to the main thread: building the layer there
            // panics in raw-window-metal. Extraction runs on the main thread, so the
            // surface is created and configured there; the Render instance then finds
            // it configured, and a resize only reconfigures it, which wgpu allows off
            // the main thread.
            render_app.add_systems(
                bevy::render::ExtractSchedule,
                bevy::render::view::create_surfaces
                    .run_if(bevy::render::view::need_surface_configuration)
                    .after(bevy::render::camera::extract_cameras),
            );
        }
    }
}

pub fn assemble_listen_app() -> App {
    let mut app = App::new();
    app.add_plugins(default_plugins_with_quiet_log(WindowPlugin {
        primary_window: None,
        ..default()
    }));
    add_runtime_plugins(&mut app);
    app
}

/// Tells the texture decoder whether the GPU samples BC directly, so iOS can
/// keep BC1/2/3/5 compressed (4-8x smaller) instead of expanding to RGBA8.
fn note_gpu_bc(support: Res<bevy::image::CompressedImageFormatSupport>) {
    asset_material::set_gpu_bc_textures(
        support.0.contains(bevy::image::CompressedImageFormats::BC),
    );
}

pub fn default_plugins_with_quiet_log(mut window: WindowPlugin) -> bevy::app::PluginGroupBuilder {
    if let Some(primary) = window.primary_window.as_mut() {
        primary.desired_maximum_frame_latency = core::num::NonZeroU32::new(frame_latency());
    }
    let mut wgpu = WgpuSettings::default();
    // BC is required everywhere but iOS. Only Apple9+ iPhone/iPad GPUs have it
    // (A15/A16 do not), so iOS takes it when the adapter offers it (the default
    // priority requests every adapter feature) and decodes to RGBA8 otherwise;
    // see `note_gpu_bc`.
    #[cfg(not(target_os = "ios"))]
    {
        wgpu.features |= WgpuFeatures::TEXTURE_COMPRESSION_BC;
    }
    wgpu.features |= WgpuFeatures::TEXTURE_FORMAT_16BIT_NORM
        | WgpuFeatures::POLYGON_MODE_LINE
        | WgpuFeatures::TEXTURE_BINDING_ARRAY
        | WgpuFeatures::SAMPLED_TEXTURE_AND_STORAGE_BUFFER_ARRAY_NON_UNIFORM_INDEXING
        | WgpuFeatures::PARTIALLY_BOUND_BINDING_ARRAY;
    let plugins = DefaultPlugins
        .set(window)
        .set(LogPlugin {
            filter: "warn,iw4l=info".into(),
            level: bevy::log::Level::WARN,
            // iOS: Bevy and wgpu warnings and errors into the boot and per-run logs.
            custom_layer: crate::log_forward::diag_layer,
            ..default()
        })
        .set(BevyRenderPlugin {
            render_creation: RenderCreation::Automatic(Box::new(wgpu)),
            ..default()
        })
        // Quit on a graphics error as Bevy does, after writing down which one.
        .add_after::<BevyRenderPlugin>(crate::render_error::RenderErrorRecordPlugin);
    #[cfg(target_os = "ios")]
    let plugins = plugins.add(crate::ios_input::IosInputPlugin);
    #[cfg(target_vendor = "apple")]
    let plugins = if thread_qos() {
        plugins
            .set(crate::thread_qos::task_pool_plugin())
            .add(crate::thread_qos::ThreadQosPlugin)
    } else {
        plugins
    };
    if pipelined_rendering() {
        plugins
    } else {
        plugins.disable::<PipelinedRenderingPlugin>()
    }
}

const PIPELINED_RENDERING_ENV: &str = "IW4L_PIPELINED_RENDERING";

/// Overlap rendering with the next main frame. Extraction remains the ownership
/// boundary; the bounded render channel permits one outstanding frame.
/// Set IW4L_PIPELINED_RENDERING=0 for synchronous presentation.
///
/// Off by default on macOS and iOS. AppKit and UIKit only let the main thread touch
/// the view behind the Metal surface; Bevy hands `create_surfaces` back to the main
/// thread through the multi-threaded executor, which the single-threaded `Render`
/// schedule above bypasses, so the surface is created during extraction (main
/// thread) instead when this is on. Measured on an M3 (mp_terminal, 10 players,
/// uncapped): 86 → 101 fps average, p50 10.2 → 8.7 ms, one frame more latency.
/// `IW4L_PIPELINED_RENDERING=1` in iw4l-env.txt turns it on for the phone.
fn pipelined_rendering() -> bool {
    match std::env::var_os(PIPELINED_RENDERING_ENV) {
        None => !cfg!(any(target_os = "macos", target_os = "ios")),
        Some(_) => perf::switch(PIPELINED_RENDERING_ENV),
    }
}

const THREAD_QOS_ENV: &str = "IW4L_THREAD_QOS";

/// Raise Bevy's compute pool threads and the render thread to User Interactive
/// and the async compute pool to User Initiated (see `thread_qos`). macOS and
/// iOS only.
///
/// Off by default. The 20 Hz server tick runs on compute pool threads, which
/// start at Default, and iOS put some of them on the efficiency cores (phone
/// report of 10-09) while the main thread waited for the tick. On the Mac,
/// pushing the pool down to Background made FixedUpdate 3.7x slower (9.6 ->
/// 35.9 ms, 17 bots), so the class the pool runs at does reach the frame.
/// `IW4L_THREAD_QOS=1` in iw4l-env.txt turns it on for the phone.
#[cfg(target_vendor = "apple")]
fn thread_qos() -> bool {
    perf::switch(THREAD_QOS_ENV)
}

/// Run the server tick on the main thread: the fixed-step schedules above
/// and the sim's own Step schedule (`sim::step`) get the single-threaded
/// executor, like every other main-app schedule here. The same switch is read
/// in `sim`.
///
/// Off by default. The multi-threaded executor hands each tick system (bot
/// thinking, scripts, movement) to a pool thread at Default priority while the
/// main thread, User Interactive, blocks until the tick is done: 27.7% of the
/// Mac main thread's samples with 17 bots were that wait. The authority
/// phases are chained, so there is little overlap to lose.
/// `IW4L_TICK_ON_MAIN=1` in iw4l-env.txt turns it on for the phone.
fn tick_on_main() -> bool {
    perf::switch(sim::TICK_ON_MAIN_ENV)
}

const FX_FAST_RANDOM_ENV: &str = "IW4L_FX_FAST_RANDOM";

/// Draw effect randomness (particle size, colour, spin, velocity, spawn
/// spread, which sprite) from a 64-bit integer mixer instead of BLAKE3. Only
/// effects read these values; nothing in the sim or on the network does.
///
/// Off by default. BLAKE3 hashes a 37-48 byte tuple for every value, several
/// per particle per frame: the largest single main-thread function on the Mac
/// with 17 bots (1.1-1.9 s of every 30 s, Estate / Highrise). The mixer draws
/// different values, so effects are as random but not particle for particle
/// the same. `IW4L_FX_FAST_RANDOM=1` in iw4l-env.txt turns it on for the phone.
fn fx_fast_random() -> bool {
    perf::switch(FX_FAST_RANDOM_ENV)
}

const FRAME_LATENCY_ENV: &str = "IW4L_FRAME_LATENCY";

/// How many frames the surface is asked to let the CPU run ahead of the GPU.
///
/// wgpu calls this a hint and the backend is free to clamp it: on Vulkan it is
/// bound to the number of swapchain images, so a run that asked for two did
/// not necessarily get two, and only a measurement says which. One is the
/// default because it is what the runtime shipped; the variable exists so the
/// other arm needs no rebuild, and the manifest records the number that was
/// asked for — never the number the driver granted, which this process cannot
/// read back.
///
/// The value is a count, not a switch: anything unparseable or zero is the
/// default, and says so rather than silently picking an arm. Read once, so the
/// window and the manifest cannot disagree and the complaint is made once.
pub(crate) fn frame_latency() -> u32 {
    // iOS renders on the main thread (no pipelined rendering), so with one
    // frame in flight (two drawables) it waits on nextDrawable every frame and
    // snaps to 30/20/15 fps. Two frames in flight let CPU and GPU overlap.
    const DEFAULT: u32 = if cfg!(target_os = "ios") { 2 } else { 1 };
    static FRAMES: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *FRAMES.get_or_init(|| {
        let Some(asked) = std::env::var_os(FRAME_LATENCY_ENV) else {
            return DEFAULT;
        };
        match asked.to_str().map(str::trim).and_then(|v| v.parse().ok()) {
            Some(frames) if frames > 0 => frames,
            _ => {
                diag::warn!(
                    Launch,
                    "{FRAME_LATENCY_ENV}={} is not a frame count; using {DEFAULT}",
                    asked.to_string_lossy(),
                );
                DEFAULT
            }
        }
    })
}
