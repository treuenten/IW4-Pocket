//! Which build is running, in a corner of the main menu.
//!
//! An internal testing aid for our phone builds: it names the commit, branch and
//! commit time so a screenshot or recording shows which build it came from. Not
//! for IW4L or IW4-Pocket upstream. Only the main menu shows it, never a match.
use bevy::prelude::*;

#[derive(Component)]
struct BuildWatermark;

pub(crate) fn register(app: &mut App) {
    app.add_systems(Update, spawn_watermark);
}

fn spawn_watermark(
    mut commands: Commands,
    font: Option<Res<ui::GameUiFont>>,
    spawned: Query<(), With<BuildWatermark>>,
) {
    let Some(font) = font else {
        return;
    };
    if !spawned.is_empty() {
        return;
    }
    commands.spawn((
        BuildWatermark,
        // The Shell layer is shown on the main menu only.
        ui::UiLayer::Shell,
        Node {
            position_type: PositionType::Absolute,
            right: percent(4),
            bottom: percent(3),
            ..default()
        },
        Text::new(label()),
        ui::game_text_font(&font.0, 10.0),
        TextColor(Color::srgba(1.0, 1.0, 1.0, 0.4)),
        TextLayout::new(Justify::Right, bevy::text::LineBreak::NoWrap),
        GlobalZIndex(9_000),
        Pickable::IGNORE,
    ));
}

/// e.g. `build e5a5a10d · stable · 14 Oct 10:52`. The build script reruns on a
/// commit or checkout, not on an edit, so "local edits" can be missing from a
/// build made with uncommitted changes.
fn label() -> String {
    let sha = env!("IW4L_BUILD_GIT_SHA");
    let mut parts = vec![format!("build {}", sha.get(..8).unwrap_or("unknown"))];
    parts.extend(
        [env!("IW4L_BUILD_GIT_BRANCH"), env!("IW4L_BUILD_GIT_DATE")]
            .into_iter()
            // A detached checkout names its branch `HEAD`.
            .filter(|part| !part.is_empty() && *part != "HEAD")
            .map(str::to_owned),
    );
    if env!("IW4L_BUILD_GIT_DIRTY") == "true" {
        parts.push("+ local edits".to_owned());
    }
    parts.join(" · ")
}
