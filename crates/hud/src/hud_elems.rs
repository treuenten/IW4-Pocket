use std::collections::HashMap;

use asset_game::MenuCatalog;
use assets::PreparedLocalizedStrings;
use bevy::prelude::*;
use frame::UiPlaySound;
use hud_iw4::{
    HE_TYPE_MATERIAL, HE_TYPE_PLAYERNAME, HE_TYPE_TENTHS_TIMER_DOWN, HE_TYPE_TENTHS_TIMER_STATIC,
    HE_TYPE_TENTHS_TIMER_UP, HE_TYPE_TEXT, HE_TYPE_TIMER_DOWN, HE_TYPE_TIMER_STATIC,
    HE_TYPE_TIMER_UP, HE_TYPE_VALUE, HE_TYPE_WAYPOINT, HudElem, WAYPOINT_CONSTANT_SIZE,
    WAYPOINT_HIDE_OFFSCREEN, copy_in_use_prefix, hud_elem_glow_color, hud_elem_placement,
    hud_elem_screen_align, hudelem_font_ui_enum, hudelem_text_scale, lerp_hud_colors,
    replace_directive, ui_get_font_handle,
};
use net::{CEntity, CEntityRuntime, FrameClock, LocalPresentClient, PresentedSnapshot};
use sim::{ClientLifecycle, SnapshotMeta};

use crate::chrome::text_width;
use crate::draw2d::{Draw2dCmd, Draw2dList, Draw2dOp, Draw2dProvenance, tessellate_fonts};
use crate::font_overlay;
use crate::gaps::{GapCause, HudPresentationGaps};
use crate::gpu_list::{HudTessPass, TessJob};
use crate::hudelem::resolve_hud_text;
use crate::images::HudImages;

const WAYPOINT_ICON_SIZE: f32 = 24.0;
const WAYPOINT_OFFSCREEN_PAD: f32 = 24.0;

const HUDELEM_FLAG_FOREGROUND: i32 = 0x1;
const HUDELEM_FLAG_HIDEWHENDEAD: i32 = 0x2;
const HUDELEM_FLAG_HIDEWHENINMENU: i32 = 0x4;

#[derive(Component)]
pub(crate) struct HudElemsRaster;
pub(crate) fn spawn_hud_elems(root: &mut ChildSpawnerCommands) {
    font_overlay::spawn_overlay(root, HudElemsRaster);
}

#[derive(Component)]
pub(crate) struct HudElemsBackRaster;
pub(crate) fn spawn_hud_elems_back(root: &mut ChildSpawnerCommands) {
    font_overlay::spawn_overlay(root, HudElemsBackRaster);
}

fn hide(pass: &mut HudTessPass) {
    pass.hud_elems = TessJob::Hide;
    pass.hud_elems_back = TessJob::Hide;
}

fn sprintf_g(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i32)
    } else {
        format!("{value}")
    }
}

fn timer_text(elem: &HudElem, cg_time: i32) -> Option<String> {
    let (ms, tenths, count_down) = match elem.elem_type {
        HE_TYPE_TIMER_DOWN => (elem.time.wrapping_sub(cg_time), false, true),
        HE_TYPE_TIMER_UP => (cg_time.wrapping_sub(elem.time), false, false),
        HE_TYPE_TIMER_STATIC => (elem.time, false, false),
        HE_TYPE_TENTHS_TIMER_DOWN => (elem.time.wrapping_sub(cg_time), true, true),
        HE_TYPE_TENTHS_TIMER_UP => (cg_time.wrapping_sub(elem.time), true, false),
        HE_TYPE_TENTHS_TIMER_STATIC => (elem.time, true, false),
        _ => return None,
    };
    let ms = ms.max(0);
    let unit = if tenths { 100 } else { 1000 };
    let units = if count_down {
        (ms + unit - 1) / unit
    } else {
        ms / unit
    };
    Some(if tenths {
        let seconds = units / 10;
        format!("{}:{:02}.{}", seconds / 60, seconds % 60, units % 10)
    } else {
        format!("{}:{:02}", units / 60, units % 60)
    })
}

fn player_name(meta: &SnapshotMeta, value: f32) -> String {
    let id = value as u32;
    meta.clients
        .iter()
        .find(|(c, _)| c.0 == id)
        .and_then(|(_, row)| entity_iw4::client_state_name(&row.name))
        .map(str::to_owned)
        .unwrap_or_default()
}

fn hud_string(
    meta: &SnapshotMeta,
    strings: &PreparedLocalizedStrings,
    input: &frame::HudInputView,
    index: i32,
    gaps: &mut HudPresentationGaps,
) -> String {
    let Some(raw) = sim::hud_string_in_occupied(&meta.hud_strings, index) else {
        return String::new();
    };
    let Some(text) = resolve_hud_text(strings, raw) else {
        gaps.raise(GapCause::LocalizedRowMissing {
            key: raw.to_owned(),
        });
        return String::new();
    };
    replace_directive(&text, |cmd| {
        crate::key_prompt::placeholder_text(Some(input), Some(&strings.0), cmd)
    })
}

fn combine_label(label: String, body: String) -> String {
    if label.contains("&&1") {
        label.replacen("&&1", &body, 1)
    } else {
        label + &body
    }
}

fn elem_text(
    elem: &HudElem,
    meta: &SnapshotMeta,
    strings: &PreparedLocalizedStrings,
    input: &frame::HudInputView,
    cg_time: i32,
    gaps: &mut HudPresentationGaps,
) -> Option<String> {
    let body = match elem.elem_type {
        HE_TYPE_TEXT => hud_string(meta, strings, input, elem.text, gaps),
        HE_TYPE_VALUE => sprintf_g(elem.value),
        HE_TYPE_PLAYERNAME => player_name(meta, elem.value),
        _ => timer_text(elem, cg_time)?,
    };
    let label = hud_string(meta, strings, input, elem.label, gaps);
    Some(combine_label(label, body))
}

/// Height of a waypoint that follows a player, above the player's origin, by stance.
/// MW2 has `waypointPlayerOffsetStand` / `Crouch` / `Prone` dvars for this; the values
/// here are estimates, not read from the game.
const WAYPOINT_PLAYER_OFFSET_STAND: f32 = 74.0;
const WAYPOINT_PLAYER_OFFSET_CROUCH: f32 = 56.0;
const WAYPOINT_PLAYER_OFFSET_PRONE: f32 = 30.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct WaypointTarget {
    number: i32,
    e_type: i32,
    e_flags: u32,
    origin: [f32; 3],
}

/// World anchor of a waypoint hud elem. Without a target entity, x/y/z are a world
/// position. With one, the waypoint follows the entity; for a player it sits a stance
/// height above the origin and x/y/z are not added (MW2's `_gameobjects` keeps writing
/// absolute world coordinates there after `SetTargetEnt`). A waypoint on the entity
/// whose eyes the view uses is not drawn.
fn waypoint_anchor(elem: &HudElem, target: Option<WaypointTarget>, eyes: i32) -> Option<Vec3> {
    let offset = Vec3::new(elem.x, elem.y, elem.z);
    if elem.target_ent_num == playerstate_iw4::ENTITYNUM_NONE {
        return Some(offset);
    }
    let target = target.filter(|t| t.number == elem.target_ent_num)?;
    if target.number == eyes {
        return None;
    }
    let origin = Vec3::from_array(target.origin);
    if target.e_type != entity_iw4::ET_PLAYER {
        return Some(origin + offset);
    }
    let height = if target.e_flags & playerstate_iw4::eflags::PRONE != 0 {
        WAYPOINT_PLAYER_OFFSET_PRONE
    } else if target.e_flags & playerstate_iw4::eflags::DUCK != 0 {
        WAYPOINT_PLAYER_OFFSET_CROUCH
    } else {
        WAYPOINT_PLAYER_OFFSET_STAND
    };
    Some(origin + Vec3::Z * height)
}

fn place_waypoint(
    elem: &HudElem,
    camera: (&Camera, &GlobalTransform, &Projection),
    entities: &Query<(&CEntity, &CEntityRuntime)>,
    surface: &crate::surface::Hud2dSurface,
    eyes: i32,
) -> Option<(Vec2, f32)> {
    let target = (elem.target_ent_num != playerstate_iw4::ENTITYNUM_NONE)
        .then(|| {
            entities.iter().find(|(entity, runtime)| {
                i32::from(entity.number()) == elem.target_ent_num && runtime.in_next_snap()
            })
        })
        .flatten()
        .map(|(entity, runtime)| WaypointTarget {
            number: i32::from(entity.number()),
            e_type: runtime.next_state.e_type,
            e_flags: runtime.next_state.e_flags,
            origin: runtime.origin,
        });
    let world = waypoint_anchor(elem, target, eyes)?;
    let (camera, transform, projection) = camera;
    let view = WaypointView {
        eye: transform.translation(),
        forward: *transform.forward(),
        right: *transform.right(),
        near: match projection {
            Projection::Perspective(perspective) => perspective.near,
            _ => 0.0,
        },
        screen: Vec2::new(surface.width(), surface.height()),
        scale: surface.scale_virtual_to_real()[1],
    };
    waypoint_screen(elem, world, &view, |point| {
        camera.world_to_viewport(transform, point).ok()
    })
}

/// The camera a waypoint is placed with: its position and axes, its near plane, and the
/// HUD surface size in pixels with its virtual-to-real scale.
struct WaypointView {
    eye: Vec3,
    forward: Vec3,
    right: Vec3,
    near: f32,
    screen: Vec2,
    scale: f32,
}

/// Screen position and size of a waypoint anchored at `world`. `project` maps a world
/// point beyond the near plane to surface pixels and fails inside it.
fn waypoint_screen(
    elem: &HudElem,
    world: Vec3,
    view: &WaypointView,
    project: impl Fn(Vec3) -> Option<Vec2>,
) -> Option<(Vec2, f32)> {
    let depth = (world - view.eye).dot(view.forward);
    let bits = elem.value as i32;
    let constant = bits & WAYPOINT_CONSTANT_SIZE != 0;
    // Only the point's side of the screen matters once it is off screen. A point behind
    // the camera is mirrored in front of it, and a point at or inside the near plane
    // (right beside the camera) is moved just past it, so it still projects and clamps
    // to the side edge instead of blinking out.
    let probe_depth = if depth > view.near {
        depth
    } else {
        (1.0 - depth).max(view.near + 1.0)
    };
    let pos = project(world + view.forward * (probe_depth - depth))?;
    let (w, h) = (view.screen.x, view.screen.y);
    let onscreen = depth > view.near && (0.0..=w).contains(&pos.x) && (0.0..=h).contains(&pos.y);
    if !constant {
        if !onscreen {
            return None;
        }
        let edge = project(world + view.right * elem.width as f32)?;
        return Some((pos, (edge - pos).length()));
    }
    let size = WAYPOINT_ICON_SIZE * view.scale;
    if onscreen {
        return Some((pos, size));
    }
    if bits & WAYPOINT_HIDE_OFFSCREEN != 0 {
        return None;
    }
    let center = view.screen * 0.5;
    let half = (center - Vec2::splat(WAYPOINT_OFFSCREEN_PAD * view.scale)).max(Vec2::ONE);
    let dir = pos - center;
    let reach =
        (half.x / dir.x.abs().max(f32::EPSILON)).min(half.y / dir.y.abs().max(f32::EPSILON));
    Some((center + dir * reach, size))
}

fn face_color(color: [u8; 4]) -> [f32; 4] {
    color.map(|c| f32::from(c) / 255.0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_hud_elems(
    surface: Res<crate::surface::Hud2dSurface>,
    catalog: Option<Res<MenuCatalog>>,
    strings: Option<Res<PreparedLocalizedStrings>>,
    input: Res<frame::HudInputView>,
    cg_clock: Res<FrameClock>,
    presented: Res<PresentedSnapshot>,
    local: Res<LocalPresentClient>,
    mut hud_images: ResMut<HudImages>,
    mut images: ResMut<Assets<Image>>,
    mut gaps: ResMut<HudPresentationGaps>,
    mut pass: ResMut<HudTessPass>,
    mut sound_latch: ResMut<crate::hudelem::HudElemSoundLatch>,
    mut ui_sound: MessageWriter<UiPlaySound>,
    cameras: Query<(&Camera, &GlobalTransform, &Projection), With<Camera3d>>,
    entities: Query<(&CEntity, &CEntityRuntime)>,
    view: Option<Res<frame::ViewSubject>>,
) {
    if !surface.is_ready() {
        hide(&mut pass);
        return;
    }
    let Some(snapshot) = presented.snapshot() else {
        hide(&mut pass);
        return;
    };
    let meta = &snapshot.meta;
    let Some(client) = meta.for_client(local.0) else {
        hide(&mut pass);
        return;
    };
    let dead = client.lifecycle != ClientLifecycle::Alive;
    let mut elems: Vec<&HudElem> = copy_in_use_prefix(&client.hud_current)
        .iter()
        .chain(copy_in_use_prefix(&client.hud_archival).iter())
        .filter(|e| {
            !(e.flags & HUDELEM_FLAG_HIDEWHENINMENU != 0 && input.script_menu_open
                || e.flags & HUDELEM_FLAG_HIDEWHENDEAD != 0 && dead)
        })
        .collect();
    if elems.is_empty() {
        hide(&mut pass);
        return;
    }
    elems.sort_by(|a, b| a.sort.total_cmp(&b.sort));
    let Some(catalog) = catalog.as_ref() else {
        gaps.raise(GapCause::NoFontCatalog);
        hide(&mut pass);
        return;
    };
    let Some(strings) = strings.as_ref() else {
        gaps.raise(GapCause::NoStringTable);
        hide(&mut pass);
        return;
    };

    let cg_time = cg_clock.time();
    let eyes = i32::try_from(
        view.as_deref()
            .map_or(local.0.0, |view| view.eyes(local.0.0)),
    )
    .unwrap_or(-1);
    let mut layers: [Vec<Draw2dCmd>; 2] = Default::default();
    let mut fonts = HashMap::new();
    for (index, elem) in elems.iter().enumerate() {
        let color = lerp_hud_colors(elem, cg_time);
        if color[3] == 0 {
            continue;
        }
        let font_scale = hud_iw4::hud_elem_lerp_font_scale(elem, cg_time);
        let font_height =
            hud_iw4::hudelem_em_px(elem.font, font_scale, surface.scale_virtual_to_real()[1]);
        let provenance = Draw2dProvenance::HudElem {
            index: index as i32,
        };
        let cmds = &mut layers[usize::from(elem.flags & HUDELEM_FLAG_FOREGROUND != 0)];

        if matches!(elem.elem_type, HE_TYPE_MATERIAL | HE_TYPE_WAYPOINT) {
            let Some(material) = u8::try_from(elem.material_index)
                .ok()
                .and_then(|i| sim::name_in_occupied(&meta.hud_materials, i))
            else {
                gaps.raise(GapCause::HudElemMaterialUnbound {
                    material_index: elem.material_index,
                });
                continue;
            };
            let (material_namespace, material) = match asset_core::AssetKey::parse(material) {
                Ok(key) if key.kind == asset_core::AssetKind::Material => (key.namespace, key.name),
                _ => (crate::images::HUD_CHROME_NAMESPACE, material.to_owned()),
            };
            if hud_images
                .get(material_namespace, &material, &mut images)
                .is_none()
            {
                gaps.raise(GapCause::HudElemImageMissing {
                    name: material.to_owned(),
                    miss: hud_images.miss_reason(),
                });
                continue;
            }
            let placed = if elem.elem_type == HE_TYPE_WAYPOINT {
                let Some(camera) = cameras.iter().find(|(c, _, _)| c.is_active) else {
                    continue;
                };
                let Some((at, size)) = place_waypoint(elem, camera, &entities, &surface, eyes)
                else {
                    continue;
                };
                hud_iw4::HudElemPlacement {
                    x: at.x - size * 0.5,
                    y: at.y - size * 0.5,
                    w: size,
                    h: size,
                }
            } else {
                hud_elem_placement(surface.placement(), elem, cg_time, 0.0, font_height)
            };
            let [s0, t0, s1, t1] =
                hud_images.stretch_pic_st(material_namespace, &material, cg_time);
            cmds.push(Draw2dCmd {
                material_namespace,
                x: placed.x,
                y: placed.y,
                w: placed.w,
                h: placed.h,
                s0,
                t0,
                s1,
                t1,
                color: face_color(color),
                material,
                op: Draw2dOp::StretchPic,
                provenance,
                layer: 1,
            });
            continue;
        }

        let Some(text) = elem_text(elem, meta, strings, &input, cg_time, &mut gaps) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let text_scale = hudelem_text_scale(elem.font, font_scale);
        let font_name = ui_get_font_handle(
            hudelem_font_ui_enum(elem.font),
            surface.scale_virtual_to_real()[1],
            text_scale,
        );
        let Some(font) = catalog.font(font_name) else {
            gaps.raise(GapCause::FontMissing {
                name: font_name.to_owned(),
            });
            continue;
        };
        let material = asset_core::AssetRef::bare_name(&font.material).to_owned();
        if hud_images
            .get(crate::images::HUD_CHROME_NAMESPACE, &material, &mut images)
            .is_none()
        {
            gaps.raise(GapCause::FontAtlasMissing {
                image: hud_images.zone_image_name(&material).map(str::to_owned),
                material,
            });
            continue;
        }
        fonts.insert(font_name.to_owned(), font);
        if let Some(alias) =
            crate::hudelem::hudelem_pulse_sound(elem, &text, cg_time, &mut sound_latch)
        {
            ui_sound.write(UiPlaySound {
                alias: alias.to_owned(),
            });
        }
        let nscale = hud_iw4::normalized_text_scale(font.pixel_height, text_scale);
        let (horz_align, vert_align) = hud_elem_screen_align(elem.align_screen);
        let glyph = surface.apply_rect(0.0, 0.0, nscale, nscale, horz_align, vert_align);
        let text_width = text_width(font, &text) as f32 * glyph.w;
        let placed =
            hud_elem_placement(surface.placement(), elem, cg_time, text_width, font_height);
        cmds.push(Draw2dCmd {
            material_namespace: crate::images::HUD_CHROME_NAMESPACE,
            x: (placed.x + 0.5).floor(),
            y: (placed.text_baseline_y() + 0.5).floor(),
            w: glyph.w,
            h: glyph.h,
            s0: 0.0,
            t0: 0.0,
            s1: 1.0,
            t1: 1.0,
            color: face_color(color),
            material,
            op: Draw2dOp::TextRun {
                font: font_name.to_owned(),
                scale: nscale,
                text,
                loc_key: String::new(),

                style: crate::draw2d::TEXT_STYLE_HUDELEM,
                fx: crate::hudelem::hudelem_text_fx(elem, cg_time),
                glow: hud_elem_glow_color(elem, color)
                    .and_then(|glow| crate::chrome::text_run_glow(font, glow)),
            },
            provenance,
            layer: 1,
        });
    }

    if layers.iter().all(Vec::is_empty) {
        hide(&mut pass);
        return;
    }

    let mut any_fx = false;
    for fx in layers.iter().flatten().filter_map(|cmd| match &cmd.op {
        Draw2dOp::TextRun { fx: Some(fx), .. } => Some(fx),
        _ => None,
    }) {
        any_fx = true;
        if hud_iw4::decay_tick_count(fx.fx.decay_duration).is_none() {
            gaps.raise(GapCause::TextDecodeFxDecayTooShort {
                fx_decay_duration: fx.fx.decay_duration,
            });
        }
    }
    if any_fx {
        let material = hud_iw4::DECODE_CHARACTERS_MATERIAL;
        if hud_images
            .get(crate::images::HUD_CHROME_NAMESPACE, material, &mut images)
            .is_none()
        {
            gaps.raise(GapCause::TextDecodeFxAtlasMissing {
                material: material.to_owned(),
                image: hud_images.zone_image_name(material).map(str::to_owned),
            });
        }
    }
    let [back, front] = layers.map(|cmds| {
        let (quads, _) = tessellate_fonts(&Draw2dList { cmds }, &fonts);
        if quads.is_empty() {
            TessJob::Hide
        } else {
            TessJob::Quads(quads)
        }
    });
    pass.hud_elems_back = back;
    pass.hud_elems = front;
}

#[cfg(test)]
mod waypoint_anchor_tests {
    use super::*;

    fn elem(x: f32, y: f32, z: f32, target: i32) -> HudElem {
        HudElem {
            elem_type: HE_TYPE_WAYPOINT,
            x,
            y,
            z,
            target_ent_num: target,
            ..HudElem::default()
        }
    }

    fn player(number: i32, origin: [f32; 3], e_flags: u32) -> WaypointTarget {
        WaypointTarget {
            number,
            e_type: entity_iw4::ET_PLAYER,
            e_flags,
            origin,
        }
    }

    #[test]
    fn untargeted_waypoint_is_a_world_position() {
        let e = elem(-1184.0, -2302.0, 339.0, playerstate_iw4::ENTITYNUM_NONE);
        assert_eq!(
            waypoint_anchor(&e, None, 0),
            Some(Vec3::new(-1184.0, -2302.0, 339.0))
        );
    }

    #[test]
    fn carrier_escort_ignores_the_absolute_xyz_gameobjects_writes() {
        // _gameobjects: objPoint.x/y/z = carrier.origin + (0,0,75), then SetTargetEnt(carrier).
        let e = elem(-1184.0, -2302.0, 339.0, 3);
        let at = waypoint_anchor(&e, Some(player(3, [-1180.0, -2300.0, 264.0], 0)), 0);
        assert_eq!(at, Some(Vec3::new(-1180.0, -2300.0, 264.0 + 74.0)));
    }

    #[test]
    fn player_waypoint_height_follows_stance() {
        let e = elem(0.0, 0.0, 0.0, 3);
        let at = |flags| {
            waypoint_anchor(&e, Some(player(3, [0.0; 3], flags)), 0)
                .unwrap()
                .z
        };
        assert_eq!(at(playerstate_iw4::eflags::DUCK), 56.0);
        assert_eq!(at(playerstate_iw4::eflags::PRONE), 30.0);
    }

    #[test]
    fn waypoint_on_the_viewers_own_entity_is_not_drawn() {
        let e = elem(-1184.0, -2302.0, 339.0, 0);
        assert_eq!(waypoint_anchor(&e, Some(player(0, [0.0; 3], 0)), 0), None);
    }

    #[test]
    fn missing_target_entity_draws_nothing() {
        let e = elem(1.0, 2.0, 3.0, 5);
        assert_eq!(waypoint_anchor(&e, None, 0), None);
    }

    #[test]
    fn waypoint_beside_the_camera_clamps_to_the_side_edge() {
        // Camera at the origin looking down +X (Z up), near plane 2 units, 640x480 surface.
        // A target at depth 0 cannot be projected where it is; it used to vanish.
        let view = WaypointView {
            eye: Vec3::ZERO,
            forward: Vec3::X,
            right: Vec3::NEG_Y,
            near: 2.0,
            screen: Vec2::new(640.0, 480.0),
            scale: 1.0,
        };
        let project = |p: Vec3| {
            let depth = p.dot(Vec3::X);
            (depth >= 2.0).then(|| Vec2::new(320.0, 240.0) + Vec2::new(-p.y, -p.z) / depth * 320.0)
        };
        let mut e = elem(0.0, 0.0, 0.0, playerstate_iw4::ENTITYNUM_NONE);
        e.value = WAYPOINT_CONSTANT_SIZE as f32;
        let place = |y| waypoint_screen(&e, Vec3::new(0.0, y, 0.0), &view, project);
        let (right, size) = place(-100.0).expect("a target on the right is drawn");
        assert!(
            right.abs_diff_eq(Vec2::new(640.0 - 24.0, 240.0), 1e-3),
            "{right}"
        );
        assert_eq!(size, 24.0);
        let (left, _) = place(100.0).expect("a target on the left is drawn");
        assert!(left.abs_diff_eq(Vec2::new(24.0, 240.0), 1e-3), "{left}");
    }
}
