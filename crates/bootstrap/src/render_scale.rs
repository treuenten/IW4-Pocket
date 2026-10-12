//! Render scale on iOS. The phone draws every frame at the screen's full
//! 2868x1320 (3.8 million pixels; the Mac's 1280x720 window is 0.9 million), and
//! its logs show the graphics chip is what holds a match at 50-60 fps: 12-14 ms
//! of GPU time a frame against 5-8 ms on the main thread.
//!
//! `IW4L_RENDER_SCALE` (0.5 to 1.0; off unless set) lowers the view's content
//! scale once the window exists, so the drawable and every pass drawn into it
//! shrink by that factor on each side; Core Animation stretches the result over
//! the screen. Window sizes in points, and with them the menus, HUD layout and
//! touch controls, stay as they were.
use bevy::prelude::*;

pub(crate) fn register(app: &mut App) {
    #[cfg(target_os = "ios")]
    ios::register(app);
    #[cfg(not(target_os = "ios"))]
    let _ = app;
}

#[cfg(target_os = "ios")]
mod ios {
    use bevy::ecs::system::NonSendMarker;
    use bevy::prelude::*;
    use bevy::window::PrimaryWindow;
    use winit::platform::ios::WindowExtIOS;

    const RENDER_SCALE_ENV: &str = "IW4L_RENDER_SCALE";

    /// The scale still to apply; removed once the window has it.
    #[derive(Resource)]
    struct PendingRenderScale(f64);

    pub(super) fn register(app: &mut App) {
        let Some(scale) = std::env::var(RENDER_SCALE_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
        else {
            return;
        };
        if !(0.5..1.0).contains(&scale) {
            if scale != 1.0 {
                diag::warn!(
                    Launch,
                    "render scale: {RENDER_SCALE_ENV}={scale} is outside 0.5-1.0, drawing at full size"
                );
            }
            return;
        }
        app.insert_resource(PendingRenderScale(scale))
            .add_systems(Update, apply.run_if(resource_exists::<PendingRenderScale>));
    }

    fn apply(
        mut commands: Commands,
        pending: Res<PendingRenderScale>,
        primary: Query<Entity, With<PrimaryWindow>>,
        _main_thread: NonSendMarker,
    ) {
        let Ok(entity) = primary.single() else {
            return;
        };
        let applied = bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
            let winit = windows.get_window(entity)?;
            let native = winit
                .current_monitor()
                .map_or_else(|| winit.scale_factor(), |monitor| monitor.scale_factor());
            let before = winit.inner_size();
            winit.set_scale_factor(native * pending.0);
            Some((native, before, winit.inner_size()))
        });
        // The winit window comes a frame or two after the entity.
        let Some((native, before, after)) = applied else {
            return;
        };
        commands.remove_resource::<PendingRenderScale>();
        diag::info!(
            Launch,
            "render scale: {:.2} (content scale {native:.2} -> {:.2}), drawable {}x{} -> {}x{}",
            pending.0,
            native * pending.0,
            before.width,
            before.height,
            after.width,
            after.height
        );
    }
}
