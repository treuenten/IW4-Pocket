//! On-screen touch controls, for playing without a controller.
//!
//! During play, touches drive a synthetic gamepad plus mouse look, so the rest
//! of the input path (`ActivePad`, deadzones, the `pad_layout` binds, Start
//! opening the pause menu) treats them like any other pad:
//! - left half: a floating move stick (left stick). Pushing it well past its
//!   rim towards the top of the screen also holds L3, which sprints;
//! - right half: swipe to look, written as `MouseMotion`. Dragging a button
//!   marked `look` (fire, aim) turns the view too;
//! - a round button for every pad bind, plus Start (pause) and Back (scores).
//!
//! Outside play (menus, console, a script menu holding input, loading, lost
//! focus) a touch is the left mouse button as before, so menus stay tappable.
//! Class select with no menu up shows only the pause button, the way out a pad
//! has with Start. A touch keeps the owner it got when it started: one that
//! pressed the mouse always releases it, one on a control goes quiet when the
//! controls change.
//!
//! `IW4L_TOUCH_CONTROLS` (iw4l-env.txt): `auto` (default: shown while no
//! controller is connected), `on` or `off`. `IW4L_TOUCH_LOOK` scales the swipe.

#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use std::sync::OnceLock;

use bevy::ecs::system::NonSendMarker;
use bevy::input::ButtonState;
use bevy::input::gamepad::{
    GamepadAxis, GamepadButton, GamepadConnection, GamepadConnectionEvent,
    RawGamepadAxisChangedEvent, RawGamepadButtonChangedEvent, RawGamepadEvent,
};
use bevy::input::mouse::{MouseButton, MouseButtonInput, MouseMotion};
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::window::{PrimaryWindow, ScreenEdge};

const MODE_ENV: &str = "IW4L_TOUCH_CONTROLS";
const LOOK_ENV: &str = "IW4L_TOUCH_LOOK";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Auto,
    On,
    Off,
}

impl Mode {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("on" | "1") => Self::On,
            Some("off" | "0") => Self::Off,
            _ => Self::Auto,
        }
    }

    fn current() -> Self {
        static MODE: OnceLock<Mode> = OnceLock::new();
        *MODE.get_or_init(|| Self::parse(std::env::var(MODE_ENV).ok().as_deref()))
    }
}

/// Mouse units per point of swipe. One unit turns 0.11 degrees at the default
/// sensitivity, so 2.5 turns a half-screen swipe on a Pro Max (~480 pt) ~130°.
fn parse_look_scale(value: Option<&str>) -> f32 {
    value
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|scale| *scale > 0.0 && *scale <= 20.0)
        .unwrap_or(2.5)
}

fn look_scale() -> f32 {
    static SCALE: OnceLock<f32> = OnceLock::new();
    *SCALE.get_or_init(|| parse_look_scale(std::env::var(LOOK_ENV).ok().as_deref()))
}

#[derive(Clone, Copy)]
enum Anchor {
    TopCentre,
    BottomLeft,
    BottomRight,
}

struct Control {
    button: GamepadButton,
    /// Shown when the button's bind has no friendlier name.
    fallback: &'static str,
    anchor: Anchor,
    /// Centre, in points from the anchor towards the middle of the screen,
    /// for a safe area 400 points tall.
    offset: Vec2,
    radius: f32,
    /// Dragging this button while it is held also turns the view.
    look: bool,
}

const fn control(
    button: GamepadButton,
    fallback: &'static str,
    anchor: Anchor,
    x: f32,
    y: f32,
    radius: f32,
    look: bool,
) -> Control {
    Control {
        button,
        fallback,
        anchor,
        offset: Vec2::new(x, y),
        radius,
        look,
    }
}

const CONTROL_COUNT: usize = 16;
const FIRE: usize = 0;
const MENU: usize = 14;

/// Laid out for the default pad layout: fire, aim and the face buttons under
/// the right thumb, sprint and the D-pad (killstreaks) over the move stick,
/// pause and scores at the top.
#[rustfmt::skip]
const CONTROLS: [Control; CONTROL_COUNT] = [
    control(GamepadButton::RightTrigger2, "RT", Anchor::BottomRight, 150.0, 150.0, 42.0, true),
    control(GamepadButton::LeftTrigger2, "LT", Anchor::BottomRight, 245.0, 95.0, 32.0, true),
    control(GamepadButton::South, "A", Anchor::BottomRight, 55.0, 60.0, 34.0, false),
    control(GamepadButton::East, "B", Anchor::BottomRight, 135.0, 45.0, 28.0, false),
    control(GamepadButton::West, "X", Anchor::BottomRight, 55.0, 145.0, 28.0, false),
    control(GamepadButton::North, "Y", Anchor::BottomRight, 55.0, 225.0, 28.0, false),
    control(GamepadButton::RightThumb, "R3", Anchor::BottomRight, 245.0, 190.0, 26.0, false),
    control(GamepadButton::RightTrigger, "RB", Anchor::BottomRight, 135.0, 250.0, 26.0, false),
    control(GamepadButton::LeftTrigger, "LB", Anchor::BottomRight, 215.0, 275.0, 26.0, false),
    control(GamepadButton::LeftThumb, "L3", Anchor::BottomLeft, 50.0, 215.0, 26.0, false),
    control(GamepadButton::DPadUp, "^", Anchor::BottomLeft, 165.0, 267.0, 18.0, false),
    control(GamepadButton::DPadDown, "v", Anchor::BottomLeft, 165.0, 203.0, 18.0, false),
    control(GamepadButton::DPadLeft, "<", Anchor::BottomLeft, 133.0, 235.0, 18.0, false),
    control(GamepadButton::DPadRight, ">", Anchor::BottomLeft, 197.0, 235.0, 18.0, false),
    control(GamepadButton::Start, "MENU", Anchor::TopCentre, 40.0, 26.0, 22.0, false),
    control(GamepadButton::Select, "SCORES", Anchor::TopCentre, -40.0, 26.0, 22.0, false),
];

/// Move-stick home and radius, for a safe area 400 points tall.
const STICK_HOME: Vec2 = Vec2::new(130.0, 110.0);
const STICK_RADIUS: f32 = 62.0;
/// A touch this far outside a button's radius still presses it.
const HIT_SLOP: f32 = 1.15;
/// The stick sprints once pushed this many radii out, within ~35° of straight up.
const SPRINT_PAST_RIM: f32 = 1.3;

fn action_label(command: &str) -> Option<&'static str> {
    Some(match command {
        "+attack" => "FIRE",
        "+speed_throw" => "AIM",
        "+frag" => "FRAG",
        "+smoke" => "TACT",
        "+gostand" => "JUMP",
        "+stance" => "CROUCH",
        "+usereload" => "RELOAD",
        "weapnext" => "SWAP",
        "+breath_sprint" | "+sprint" => "SPRINT",
        "+melee" => "MELEE",
        "+scores" => "SCORES",
        _ => return None,
    })
}

fn control_command(index: usize, binds: Option<&console::KeyBinds>) -> Option<&'static str> {
    console::PadButton::from_gamepad_button(CONTROLS[index].button)
        .and_then(|pad| binds?.binding_name(console::BindButton::Pad(pad)))
}

fn control_label(index: usize, binds: Option<&console::KeyBinds>) -> &'static str {
    control_command(index, binds)
        .and_then(action_label)
        .unwrap_or(CONTROLS[index].fallback)
}

/// The control the stick holds when pushed past its rim: whichever is bound
/// to sprint (L3 in every built-in layout), none if sprint is unbound.
fn sprint_control(binds: Option<&console::KeyBinds>) -> Option<usize> {
    (0..CONTROL_COUNT).find(|index| {
        matches!(
            control_command(*index, binds),
            Some("+breath_sprint" | "+sprint")
        )
    })
}

/// Control and stick positions in window points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Layout {
    width: f32,
    centres: [Vec2; CONTROL_COUNT],
    radii: [f32; CONTROL_COUNT],
    stick_home: Vec2,
    stick_radius: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new(
            Vec2::new(956.0, 440.0),
            Vec2::new(62.0, 0.0),
            Vec2::new(894.0, 419.0),
        )
    }
}

impl Layout {
    /// `safe_min`/`safe_max` are the corners of the safe area inside a window
    /// `size` points large.
    fn new(size: Vec2, safe_min: Vec2, safe_max: Vec2) -> Self {
        let scale = ((safe_max.y - safe_min.y) / 400.0).clamp(0.75, 1.3);
        let place = |anchor: Anchor, offset: Vec2| match anchor {
            Anchor::TopCentre => Vec2::new(
                (safe_min.x + safe_max.x) * 0.5 + offset.x * scale,
                safe_min.y + offset.y * scale,
            ),
            Anchor::BottomLeft => {
                Vec2::new(safe_min.x + offset.x * scale, safe_max.y - offset.y * scale)
            }
            Anchor::BottomRight => {
                Vec2::new(safe_max.x - offset.x * scale, safe_max.y - offset.y * scale)
            }
        };
        Self {
            width: size.x,
            centres: std::array::from_fn(|i| place(CONTROLS[i].anchor, CONTROLS[i].offset)),
            radii: std::array::from_fn(|i| CONTROLS[i].radius * scale),
            stick_home: place(Anchor::BottomLeft, STICK_HOME),
            stick_radius: STICK_RADIUS * scale,
        }
    }

    /// The control under `point`; the nearest one when slop circles overlap.
    fn hit(&self, point: Vec2) -> Option<usize> {
        (0..CONTROL_COUNT)
            .map(|i| (i, point.distance(self.centres[i])))
            .filter(|(i, distance)| *distance <= self.radii[*i] * HIT_SLOP)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

/// Stick value for a finger at `at` that went down at `origin`: -1..1, Y up.
fn stick_value(origin: Vec2, at: Vec2, radius: f32) -> Vec2 {
    let value = ((at - origin) / radius).clamp_length_max(1.0);
    Vec2::new(value.x, -value.y)
}

fn stick_sprints(origin: Vec2, at: Vec2, radius: f32) -> bool {
    let pushed = at - origin;
    pushed.length() > radius * SPRINT_PAST_RIM && -pushed.y > pushed.x.abs() * 1.4
}

/// What the controls take over.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Shown {
    /// Nothing: every touch is the mouse.
    #[default]
    Hidden,
    /// The pause button; other touches are the mouse.
    MenuOnly,
    /// Every control, the stick and swipe look.
    All,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Owner {
    /// The left mouse button, at most one finger at a time.
    Mouse,
    Control {
        index: usize,
        last: Vec2,
    },
    Stick {
        origin: Vec2,
        at: Vec2,
    },
    Look {
        last: Vec2,
    },
    /// Ends without doing anything.
    Ignored,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Effect {
    MousePress(Vec2),
    MouseMove(Vec2),
    MouseRelease(Vec2),
    /// Swipe distance in points.
    Look(Vec2),
}

/// Which finger owns what. Pure state, so it can be tested without an app.
#[derive(Default)]
struct Router {
    owners: Vec<(u64, Owner)>,
    frame: u64,
    /// Frame each control was last pressed on: a tap that starts and ends in
    /// one frame still holds its button for that frame.
    pressed_on: [Option<u64>; CONTROL_COUNT],
}

impl Router {
    fn begin_frame(&mut self) {
        self.frame += 1;
    }

    fn owner(&mut self, id: u64) -> Option<&mut Owner> {
        self.owners
            .iter_mut()
            .find(|(owner_id, _)| *owner_id == id)
            .map(|(_, owner)| owner)
    }

    fn touch(
        &mut self,
        id: u64,
        phase: TouchPhase,
        at: Vec2,
        shown: Shown,
        layout: &Layout,
        effects: &mut Vec<Effect>,
    ) {
        match phase {
            TouchPhase::Started => {
                // A repeated id means its end was lost; drop the old touch first.
                self.end(id, at, effects);
                let owner = self.claim(at, shown, layout, effects);
                self.owners.push((id, owner));
            }
            TouchPhase::Moved => {
                let Some(owner) = self.owner(id) else {
                    return;
                };
                match owner {
                    Owner::Mouse => effects.push(Effect::MouseMove(at)),
                    Owner::Control { index, last } => {
                        if CONTROLS[*index].look {
                            effects.push(Effect::Look(at - *last));
                        }
                        *last = at;
                    }
                    Owner::Stick { at: stick_at, .. } => *stick_at = at,
                    Owner::Look { last } => {
                        effects.push(Effect::Look(at - *last));
                        *last = at;
                    }
                    Owner::Ignored => {}
                }
            }
            TouchPhase::Ended | TouchPhase::Canceled => self.end(id, at, effects),
        }
    }

    fn claim(
        &mut self,
        at: Vec2,
        shown: Shown,
        layout: &Layout,
        effects: &mut Vec<Effect>,
    ) -> Owner {
        let hit = match shown {
            Shown::Hidden => None,
            Shown::MenuOnly => layout.hit(at).filter(|index| *index == MENU),
            Shown::All => layout.hit(at),
        };
        if let Some(index) = hit {
            self.pressed_on[index] = Some(self.frame);
            return Owner::Control { index, last: at };
        }
        if shown != Shown::All {
            if self.owners.iter().any(|(_, owner)| *owner == Owner::Mouse) {
                return Owner::Ignored;
            }
            effects.push(Effect::MousePress(at));
            return Owner::Mouse;
        }
        if at.x >= layout.width * 0.5 {
            return Owner::Look { last: at };
        }
        if self.stick().is_some() {
            return Owner::Ignored;
        }
        Owner::Stick { origin: at, at }
    }

    fn end(&mut self, id: u64, at: Vec2, effects: &mut Vec<Effect>) {
        let Some(slot) = self.owners.iter().position(|(owner_id, _)| *owner_id == id) else {
            return;
        };
        if self.owners.swap_remove(slot).1 == Owner::Mouse {
            effects.push(Effect::MouseRelease(at));
        }
    }

    /// The controls changed: they let go and their fingers go quiet. A finger
    /// on the mouse keeps it until it lifts.
    fn let_go(&mut self) {
        for (_, owner) in &mut self.owners {
            if *owner != Owner::Mouse {
                *owner = Owner::Ignored;
            }
        }
        self.pressed_on = [None; CONTROL_COUNT];
    }

    fn stick(&self) -> Option<(Vec2, Vec2)> {
        self.owners.iter().find_map(|(_, owner)| match owner {
            Owner::Stick { origin, at } => Some((*origin, *at)),
            _ => None,
        })
    }

    fn held(&self, layout: &Layout, sprint: Option<usize>) -> [bool; CONTROL_COUNT] {
        let mut held: [bool; CONTROL_COUNT] =
            std::array::from_fn(|i| self.pressed_on[i] == Some(self.frame));
        for (_, owner) in &self.owners {
            if let Owner::Control { index, .. } = owner {
                held[*index] = true;
            }
        }
        if let Some((origin, at)) = self.stick()
            && let Some(sprint) = sprint
        {
            held[sprint] |= stick_sprints(origin, at, layout.stick_radius);
        }
        held
    }
}

/// Controllers seen by the GameController poll, kept by `poll_controllers`.
#[derive(Resource, Default)]
pub(crate) struct PhysicalPads(pub usize);

#[derive(Resource, Default)]
pub(crate) struct TouchControls {
    router: Router,
    layout: Layout,
    shown: Shown,
    held: [bool; CONTROL_COUNT],
    stick: Option<(Vec2, Vec2)>,
    pad: Option<Entity>,
    announced: bool,
    /// The pad was made the prompt source since play last started.
    prompted: bool,
    sent_buttons: [f32; CONTROL_COUNT],
    sent_stick: [f32; 2],
    effects: Vec<Effect>,
}

/// The safe area from winit; iOS reports it as the window's inner rectangle.
fn layout_for(entity: Entity, window: &Window) -> Layout {
    let size = Vec2::new(window.width(), window.height());
    let safe = bevy::winit::WINIT_WINDOWS.with_borrow(|windows| {
        let winit = windows.get_window(entity)?;
        let scale = winit.scale_factor() as f32;
        let inner = winit.inner_position().ok()?;
        let outer = winit.outer_position().ok()?;
        let inner_size = winit.inner_size();
        let min = Vec2::new((inner.x - outer.x) as f32, (inner.y - outer.y) as f32) / scale;
        let max = min + Vec2::new(inner_size.width as f32, inner_size.height as f32) / scale;
        Some((min, max))
    });
    let (min, max) = safe
        .filter(|(min, max)| max.x - min.x > 100.0 && max.y - min.y > 100.0)
        .unwrap_or((Vec2::new(60.0, 0.0), size - Vec2::new(60.0, 21.0)));
    Layout::new(size, min.max(Vec2::ZERO), max.min(size))
}

type PadWriters<'w> = (
    MessageWriter<'w, RawGamepadEvent>,
    MessageWriter<'w, GamepadConnectionEvent>,
    MessageWriter<'w, RawGamepadButtonChangedEvent>,
    MessageWriter<'w, RawGamepadAxisChangedEvent>,
);

/// The game state the controls depend on.
type Gates<'w> = (
    Option<Res<'w, frame::AppScreen>>,
    Option<Res<'w, console::ConsoleState>>,
    Option<Res<'w, hud::ScriptMenus>>,
    Option<Res<'w, assets::LoadingScreen>>,
    Option<Res<'w, console::KeyBinds>>,
);

/// Replaces the old touch-as-mouse: routes every touch, then publishes the
/// synthetic pad. Runs before `InputSystems` like the controller poll.
#[allow(clippy::too_many_arguments)]
pub(crate) fn route_touches(
    _main_thread: NonSendMarker,
    mut commands: Commands,
    mut touches: MessageReader<TouchInput>,
    mut windows: Query<(Entity, &mut Window), With<PrimaryWindow>>,
    mut controls: ResMut<TouchControls>,
    physical: Res<PhysicalPads>,
    (screen, console, menus, loading, binds): Gates,
    (mut devices, mut active): (
        Option<ResMut<frame::InputDevices>>,
        Option<ResMut<frame::ActivePad>>,
    ),
    mut mouse: MessageWriter<MouseButtonInput>,
    mut motion: MessageWriter<MouseMotion>,
    (mut raw, mut connections, mut button_events, mut axis_events): PadWriters,
) {
    let Ok((window_entity, mut window)) = windows.single_mut() else {
        return;
    };
    let mode = Mode::current();
    let controls = &mut *controls;
    controls.router.begin_frame();
    let free = mode != Mode::Off
        && (mode == Mode::On || physical.0 == 0)
        && window.focused
        && loading.is_none()
        && !console.is_some_and(|console| console.open)
        && !menus.is_some_and(|menus| menus.captures_input());
    let shown = match screen.as_deref() {
        Some(frame::AppScreen::InGame) if free => Shown::All,
        Some(frame::AppScreen::ClassSelect) if free => Shown::MenuOnly,
        _ => Shown::Hidden,
    };
    let playing = shown == Shown::All;
    if shown != Shown::Hidden {
        let layout = layout_for(window_entity, &window);
        if layout != controls.layout {
            controls.layout = layout;
        }
        if controls.pad.is_none() {
            diag::boot_crumb("touch controls shown");
        }
    }
    if shown != controls.shown {
        controls.router.let_go();
        controls.shown = shown;
    }

    for touch in touches.read() {
        controls.router.touch(
            touch.id,
            touch.phase,
            touch.position,
            shown,
            &controls.layout,
            &mut controls.effects,
        );
    }
    for effect in controls.effects.drain(..) {
        let state = match effect {
            Effect::MousePress(at) => {
                window.set_cursor_position(Some(at));
                ButtonState::Pressed
            }
            Effect::MouseMove(at) => {
                window.set_cursor_position(Some(at));
                continue;
            }
            Effect::MouseRelease(at) => {
                window.set_cursor_position(Some(at));
                ButtonState::Released
            }
            Effect::Look(delta) => {
                motion.write(MouseMotion {
                    delta: delta * look_scale(),
                });
                continue;
            }
        };
        mouse.write(MouseButtonInput {
            button: MouseButton::Left,
            state,
            window: window_entity,
        });
    }

    (controls.held, controls.stick) = if shown == Shown::Hidden {
        ([false; CONTROL_COUNT], None)
    } else {
        let sprint = sprint_control(binds.as_deref());
        (
            controls.router.held(&controls.layout, sprint),
            controls.router.stick(),
        )
    };

    // Swipes from an edge reach the game first; a second swipe still works.
    let edges = if playing {
        ScreenEdge::All
    } else {
        ScreenEdge::None
    };
    if window.preferred_screen_edges_deferring_system_gestures != edges {
        window.preferred_screen_edges_deferring_system_gestures = edges;
    }
    if window.prefers_home_indicator_hidden != playing {
        window.prefers_home_indicator_hidden = playing;
    }
    if let Some(devices) = devices.as_mut()
        && (devices.touch_controls != playing || devices.touch_pad != controls.pad)
    {
        devices.touch_controls = playing;
        devices.touch_pad = controls.pad;
    }

    let Some(pad) = controls.pad else {
        if shown != Shown::Hidden {
            // Connected once, at first show: values follow from the next frame,
            // after bevy has made the entity a gamepad (as `poll_controllers`).
            let entity = commands.spawn_empty().id();
            let event = GamepadConnectionEvent::new(
                entity,
                GamepadConnection::Connected {
                    // "xbox" makes the HUD prompts read A/B/X/Y, LB/RB, LT/RT.
                    name: "Touch controls (Xbox layout)".to_owned(),
                    vendor_id: None,
                    product_id: None,
                },
            );
            raw.write(event.clone().into());
            connections.write(event);
            controls.pad = Some(entity);
        }
        return;
    };
    if !controls.announced {
        controls.announced = true;
        return;
    }
    // A menu tap shows keyboard prompts, and the first match (or a controller
    // dropping out) leaves no pad active. In play the on-screen pad takes the
    // empty slot, and once it holds it the prompts follow it again.
    if !playing {
        controls.prompted = false;
    } else if let Some(active) = active.as_mut() {
        if active.0.is_none() {
            active.0 = Some(pad);
            controls.prompted = false;
        }
        if !controls.prompted && active.0 == Some(pad) {
            controls.prompted = true;
            if let Some(devices) = devices.as_mut() {
                devices.pad_prompts = true;
            }
        }
    }
    for (index, held) in controls.held.into_iter().enumerate() {
        let value = if held { 1.0 } else { 0.0 };
        if controls.sent_buttons[index] != value {
            controls.sent_buttons[index] = value;
            let event = RawGamepadButtonChangedEvent::new(pad, CONTROLS[index].button, value);
            raw.write(event.into());
            button_events.write(event);
        }
    }
    let stick = controls.stick.map_or(Vec2::ZERO, |(origin, at)| {
        stick_value(origin, at, controls.layout.stick_radius)
    });
    let axes = [
        (GamepadAxis::LeftStickX, stick.x),
        (GamepadAxis::LeftStickY, stick.y),
    ];
    for (index, (axis, value)) in axes.into_iter().enumerate() {
        let sent = controls.sent_stick[index];
        // Small steps are skipped, but a stick let go always reaches zero.
        if (sent - value).abs() > 0.001 || (value == 0.0 && sent != 0.0) {
            controls.sent_stick[index] = value;
            let event = RawGamepadAxisChangedEvent::new(pad, axis, value);
            raw.write(event.into());
            axis_events.write(event);
        }
    }
}

#[derive(Component)]
pub(crate) struct OverlayRoot;

#[derive(Component)]
pub(crate) struct OverlayButton(usize);

#[derive(Component)]
pub(crate) struct OverlayLabel(usize);

#[derive(Component)]
pub(crate) struct StickBase;

#[derive(Component)]
pub(crate) struct StickKnob;

const KNOB_RADIUS: f32 = 26.0;

fn fill(index: usize, held: bool) -> Color {
    match (index == FIRE, held) {
        (true, false) => Color::srgba(0.95, 0.25, 0.2, 0.22),
        (true, true) => Color::srgba(0.95, 0.25, 0.2, 0.5),
        (false, false) => Color::srgba(1.0, 1.0, 1.0, 0.1),
        (false, true) => Color::srgba(1.0, 1.0, 1.0, 0.38),
    }
}

fn circle(centre: Vec2, radius: f32) -> (Val, Val, Val, Val) {
    (
        px(centre.x - radius),
        px(centre.y - radius),
        px(radius * 2.0),
        px(radius * 2.0),
    )
}

fn place(node: &mut Mut<Node>, centre: Vec2, radius: f32) {
    let (left, top, width, height) = circle(centre, radius);
    if node.left != left || node.top != top || node.width != width || node.height != height {
        node.left = left;
        node.top = top;
        node.width = width;
        node.height = height;
    }
}

fn round_node(centre: Vec2, radius: f32) -> Node {
    let (left, top, width, height) = circle(centre, radius);
    Node {
        position_type: PositionType::Absolute,
        left,
        top,
        width,
        height,
        border: UiRect::all(px(2)),
        border_radius: BorderRadius::MAX,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    }
}

/// Draws the controls from what `route_touches` decided this frame.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn draw_touch_controls(
    mut commands: Commands,
    controls: Res<TouchControls>,
    font: Option<Res<ui::GameUiFont>>,
    binds: Option<Res<console::KeyBinds>>,
    mut roots: Query<&mut Visibility, With<OverlayRoot>>,
    mut buttons: Query<
        (
            &OverlayButton,
            &mut Node,
            &mut BackgroundColor,
            &mut Visibility,
        ),
        Without<OverlayRoot>,
    >,
    mut labels: Query<(&OverlayLabel, &mut Text, &mut TextFont)>,
    mut base: Query<
        (&mut Node, &mut Visibility),
        (
            With<StickBase>,
            Without<OverlayRoot>,
            Without<OverlayButton>,
            Without<StickKnob>,
        ),
    >,
    mut knob: Query<
        (&mut Node, &mut BackgroundColor, &mut Visibility),
        (
            With<StickKnob>,
            Without<OverlayRoot>,
            Without<OverlayButton>,
            Without<StickBase>,
        ),
    >,
    mut drawn: Local<Option<Layout>>,
) {
    let layout = &controls.layout;
    let Ok(mut visibility) = roots.single_mut() else {
        if Mode::current() != Mode::Off
            && let Some(font) = font
        {
            spawn_overlay(&mut commands, layout, &font.0, binds.as_deref());
            *drawn = Some(*layout);
        }
        return;
    };
    let want = if controls.shown == Shown::Hidden {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
    visibility.set_if_neq(want);
    if controls.shown == Shown::Hidden {
        return;
    }
    let all = controls.shown == Shown::All;
    let show = |on: bool| {
        if on {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    };
    let moved = *drawn != Some(*layout);
    *drawn = Some(*layout);
    for (button, mut node, mut background, mut visibility) in &mut buttons {
        if moved {
            place(&mut node, layout.centres[button.0], layout.radii[button.0]);
        }
        background.set_if_neq(BackgroundColor(fill(button.0, controls.held[button.0])));
        visibility.set_if_neq(show(all || button.0 == MENU));
    }
    // Binds change only in menus, while the controls are hidden, so the
    // labels are compared on every shown frame rather than on change.
    for (label, mut text, mut text_font) in &mut labels {
        let wanted = control_label(label.0, binds.as_deref());
        if text.0 != wanted {
            text.0 = wanted.to_owned();
        }
        if moved {
            text_font.font_size = bevy::text::FontSize::Px(label_size(layout.radii[label.0]));
        }
    }
    let (origin, at) = controls
        .stick
        .unwrap_or((layout.stick_home, layout.stick_home));
    if let Ok((mut node, mut visibility)) = base.single_mut() {
        place(&mut node, origin, layout.stick_radius);
        visibility.set_if_neq(show(all));
    }
    if let Ok((mut node, mut background, mut visibility)) = knob.single_mut() {
        let offset = (at - origin).clamp_length_max(layout.stick_radius);
        place(
            &mut node,
            origin + offset,
            KNOB_RADIUS * layout.stick_radius / STICK_RADIUS,
        );
        let alpha = if controls.stick.is_some() { 0.38 } else { 0.18 };
        background.set_if_neq(BackgroundColor(Color::srgba(1.0, 1.0, 1.0, alpha)));
        visibility.set_if_neq(show(all));
    }
}

fn label_size(radius: f32) -> f32 {
    (radius * 0.4).clamp(9.0, 16.0)
}

fn spawn_overlay(
    commands: &mut Commands,
    layout: &Layout,
    font: &Handle<Font>,
    binds: Option<&console::KeyBinds>,
) {
    let edge = BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.45));
    commands
        .spawn((
            OverlayRoot,
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                top: px(0),
                width: percent(100),
                height: percent(100),
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(9_000),
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            root.spawn((
                StickBase,
                Visibility::Inherited,
                round_node(layout.stick_home, layout.stick_radius),
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.06)),
                BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.3)),
                Pickable::IGNORE,
            ));
            root.spawn((
                StickKnob,
                Visibility::Inherited,
                round_node(layout.stick_home, KNOB_RADIUS),
                BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.18)),
                edge,
                Pickable::IGNORE,
            ));
            for index in 0..CONTROL_COUNT {
                root.spawn((
                    OverlayButton(index),
                    Visibility::Inherited,
                    round_node(layout.centres[index], layout.radii[index]),
                    BackgroundColor(fill(index, false)),
                    edge,
                    Pickable::IGNORE,
                ))
                .with_child((
                    OverlayLabel(index),
                    Text::new(control_label(index, binds)),
                    ui::game_text_font(font, label_size(layout.radii[index])),
                    TextColor(Color::srgba(1.0, 1.0, 1.0, 0.85)),
                    TextLayout::new(Justify::Center, bevy::text::LineBreak::NoWrap),
                    Pickable::IGNORE,
                ));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L3, which every built-in pad layout binds to sprint.
    const SPRINT: usize = 9;

    fn layout() -> Layout {
        Layout::default()
    }

    fn run(router: &mut Router, events: &[(u64, TouchPhase, Vec2)], shown: Shown) -> Vec<Effect> {
        let mut effects = Vec::new();
        for (id, phase, at) in events {
            router.touch(*id, *phase, *at, shown, &layout(), &mut effects);
        }
        effects
    }

    #[test]
    fn settings_parse() {
        assert_eq!(Mode::parse(None), Mode::Auto);
        assert_eq!(Mode::parse(Some("auto")), Mode::Auto);
        assert_eq!(Mode::parse(Some("on")), Mode::On);
        assert_eq!(Mode::parse(Some("1")), Mode::On);
        assert_eq!(Mode::parse(Some("off")), Mode::Off);
        assert_eq!(Mode::parse(Some("0")), Mode::Off);
        assert_eq!(parse_look_scale(None), 2.5);
        assert_eq!(parse_look_scale(Some("4")), 4.0);
        assert_eq!(parse_look_scale(Some("0")), 2.5);
        assert_eq!(parse_look_scale(Some("NaN")), 2.5);
        assert_eq!(parse_look_scale(Some("inf")), 2.5);
    }

    #[test]
    fn controls_do_not_overlap_and_stay_on_screen() {
        for (size, min, max) in [
            // iPhone 17 Pro Max and iPhone SE landscape, iPad Pro 13" landscape.
            (
                Vec2::new(956.0, 440.0),
                Vec2::new(62.0, 0.0),
                Vec2::new(894.0, 419.0),
            ),
            (
                Vec2::new(667.0, 375.0),
                Vec2::new(0.0, 0.0),
                Vec2::new(667.0, 375.0),
            ),
            (
                Vec2::new(1376.0, 1032.0),
                Vec2::new(0.0, 24.0),
                Vec2::new(1376.0, 1012.0),
            ),
        ] {
            let layout = Layout::new(size, min, max);
            for i in 0..CONTROL_COUNT {
                let (c, r) = (layout.centres[i], layout.radii[i]);
                assert!(
                    c.x - r >= min.x && c.x + r <= max.x,
                    "control {i} off screen at {size}"
                );
                assert!(
                    c.y - r >= min.y && c.y + r <= max.y,
                    "control {i} off screen at {size}"
                );
                for j in i + 1..CONTROL_COUNT {
                    let gap = c.distance(layout.centres[j]) - r - layout.radii[j];
                    assert!(gap > 2.0, "controls {i} and {j} overlap at {size}");
                }
                assert!(
                    c.distance(layout.stick_home) > r + layout.stick_radius,
                    "control {i} covers the stick at {size}"
                );
            }
            // Fire and aim belong to the look side, the stick to the move side.
            assert!(layout.centres[FIRE].x > size.x * 0.5);
            assert!(layout.stick_home.x < size.x * 0.5);
        }
    }

    #[test]
    fn hit_picks_the_nearest_control() {
        let layout = layout();
        assert_eq!(layout.hit(layout.centres[FIRE]), Some(FIRE));
        assert_eq!(
            layout.hit(layout.centres[SPRINT] + Vec2::new(27.0, 0.0)),
            Some(SPRINT)
        );
        assert_eq!(layout.hit(Vec2::new(478.0, 200.0)), None);
    }

    #[test]
    fn outside_play_one_finger_is_the_mouse() {
        let mut router = Router::default();
        router.begin_frame();
        let a = Vec2::new(100.0, 100.0);
        let b = Vec2::new(300.0, 100.0);
        let effects = run(
            &mut router,
            &[
                (1, TouchPhase::Started, a),
                (2, TouchPhase::Started, b),
                (2, TouchPhase::Moved, a),
                (1, TouchPhase::Moved, b),
                (2, TouchPhase::Ended, b),
                (1, TouchPhase::Ended, b),
            ],
            Shown::Hidden,
        );
        assert_eq!(
            effects,
            [
                Effect::MousePress(a),
                Effect::MouseMove(b),
                Effect::MouseRelease(b)
            ]
        );
        assert!(router.owners.is_empty());
    }

    #[test]
    fn a_mouse_touch_releases_after_play_starts() {
        let mut router = Router::default();
        router.begin_frame();
        let at = Vec2::new(500.0, 100.0);
        run(&mut router, &[(1, TouchPhase::Started, at)], Shown::Hidden);
        // What `route_touches` does when the controls change.
        router.let_go();
        let effects = run(&mut router, &[(1, TouchPhase::Ended, at)], Shown::All);
        assert_eq!(effects, [Effect::MouseRelease(at)]);
    }

    #[test]
    fn controls_hold_buttons_and_a_tap_lasts_one_frame() {
        let mut router = Router::default();
        let layout = layout();
        router.begin_frame();
        let fire = layout.centres[FIRE];
        run(&mut router, &[(1, TouchPhase::Started, fire)], Shown::All);
        assert!(router.held(&layout, Some(SPRINT))[FIRE]);
        router.begin_frame();
        assert!(router.held(&layout, Some(SPRINT))[FIRE]);
        run(&mut router, &[(1, TouchPhase::Ended, fire)], Shown::All);
        assert!(!router.held(&layout, Some(SPRINT))[FIRE]);

        router.begin_frame();
        let effects = run(
            &mut router,
            &[(2, TouchPhase::Started, fire), (2, TouchPhase::Ended, fire)],
            Shown::All,
        );
        assert!(effects.is_empty());
        assert!(router.held(&layout, Some(SPRINT))[FIRE]);
        router.begin_frame();
        assert!(!router.held(&layout, Some(SPRINT))[FIRE]);
    }

    #[test]
    fn fire_and_free_right_side_swipes_look() {
        let mut router = Router::default();
        router.begin_frame();
        let fire = layout().centres[FIRE];
        let free = Vec2::new(600.0, 60.0);
        let effects = run(
            &mut router,
            &[
                (1, TouchPhase::Started, fire),
                (1, TouchPhase::Moved, fire + Vec2::new(10.0, -4.0)),
                (2, TouchPhase::Started, free),
                (2, TouchPhase::Moved, free + Vec2::new(-3.0, 5.0)),
            ],
            Shown::All,
        );
        assert_eq!(
            effects,
            [
                Effect::Look(Vec2::new(10.0, -4.0)),
                Effect::Look(Vec2::new(-3.0, 5.0))
            ]
        );
    }

    #[test]
    fn left_side_is_one_floating_stick() {
        let mut router = Router::default();
        let layout = layout();
        router.begin_frame();
        let origin = Vec2::new(250.0, 330.0);
        let radius = layout.stick_radius;
        run(
            &mut router,
            &[
                (1, TouchPhase::Started, origin),
                (1, TouchPhase::Moved, origin + Vec2::new(radius * 0.5, 0.0)),
                (2, TouchPhase::Started, origin + Vec2::new(40.0, 40.0)),
            ],
            Shown::All,
        );
        let (o, at) = router.stick().unwrap();
        assert_eq!(o, origin);
        assert!(stick_value(o, at, radius).abs_diff_eq(Vec2::new(0.5, 0.0), 1e-5));
        assert_eq!(router.owners[1].1, Owner::Ignored);
        assert!(!router.held(&layout, Some(SPRINT))[SPRINT]);

        run(
            &mut router,
            &[(1, TouchPhase::Moved, origin + Vec2::new(0.0, -1.4 * radius))],
            Shown::All,
        );
        let (o, at) = router.stick().unwrap();
        assert!(stick_value(o, at, radius).abs_diff_eq(Vec2::new(0.0, 1.0), 1e-5));
        assert!(router.held(&layout, Some(SPRINT))[SPRINT]);
        run(
            &mut router,
            &[(1, TouchPhase::Moved, origin + Vec2::new(1.4 * radius, 0.0))],
            Shown::All,
        );
        assert!(!router.held(&layout, Some(SPRINT))[SPRINT]);
        run(&mut router, &[(1, TouchPhase::Ended, origin)], Shown::All);
        assert_eq!(router.stick(), None);
    }

    #[test]
    fn stopping_play_lets_go_and_silences_fingers() {
        let mut router = Router::default();
        let layout = layout();
        router.begin_frame();
        let fire = layout.centres[FIRE];
        let stick = Vec2::new(250.0, 330.0);
        run(
            &mut router,
            &[
                (1, TouchPhase::Started, fire),
                (2, TouchPhase::Started, stick),
            ],
            Shown::All,
        );
        router.let_go();
        assert_eq!(router.held(&layout, Some(SPRINT)), [false; CONTROL_COUNT]);
        assert_eq!(router.stick(), None);
        let effects = run(
            &mut router,
            &[
                (1, TouchPhase::Moved, fire + Vec2::new(5.0, 0.0)),
                (1, TouchPhase::Ended, fire),
                (2, TouchPhase::Canceled, stick),
            ],
            Shown::Hidden,
        );
        assert!(effects.is_empty());
        assert!(router.owners.is_empty());
    }

    #[test]
    fn class_select_offers_only_the_pause_button() {
        let mut router = Router::default();
        let layout = layout();
        router.begin_frame();
        let menu = layout.centres[MENU];
        let fire = layout.centres[FIRE];
        let effects = run(
            &mut router,
            &[
                (1, TouchPhase::Started, menu),
                (2, TouchPhase::Started, fire),
            ],
            Shown::MenuOnly,
        );
        assert_eq!(effects, [Effect::MousePress(fire)]);
        let held = router.held(&layout, Some(SPRINT));
        assert!(held[MENU] && !held[FIRE]);
        assert_eq!(router.stick(), None);
    }

    #[test]
    fn the_stick_sprints_with_whatever_sprint_is_bound_to() {
        let mut binds = console::KeyBinds::default();
        binds.apply_defaults();
        assert_eq!(sprint_control(Some(&binds)), Some(SPRINT));
        assert_eq!(control_label(SPRINT, Some(&binds)), "SPRINT");
        assert_eq!(control_label(FIRE, Some(&binds)), "FIRE");
        assert_eq!(control_label(MENU, Some(&binds)), "MENU");
        assert_eq!(sprint_control(None), None);

        let mut router = Router::default();
        let layout = layout();
        router.begin_frame();
        let origin = Vec2::new(250.0, 330.0);
        let pushed = origin + Vec2::new(0.0, -1.4 * layout.stick_radius);
        run(
            &mut router,
            &[
                (1, TouchPhase::Started, origin),
                (1, TouchPhase::Moved, pushed),
            ],
            Shown::All,
        );
        assert!(router.held(&layout, Some(FIRE))[FIRE]);
        assert!(!router.held(&layout, Some(FIRE))[SPRINT]);
        assert_eq!(router.held(&layout, None), [false; CONTROL_COUNT]);
    }
}
