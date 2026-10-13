use fx_iw4::{
    FX_ELEM_AT_REST_NONE, FxPhysPiece, collide_marks_at_rest, collide_on_ground,
    collide_substep_schedule, collision_reflect_base_vel_delta, elem_dies_on_touch,
    elem_gravity_accel_z_sampled, elem_launch_velocity, elem_update_has_velocity_graph,
    elem_uses_collision, elem_uses_vel_local, elem_uses_vel_world, get_at_rest_fraction,
    get_velocity_at_time, impact_child_speed_allows, integrate_velocity_graph,
    orientation_pos_from_world, orientation_pos_to_world, sample_reflection_factor, trace_mask,
    vec3_length_sq,
};

use crate::update::{FxElemMotionQuery, FxElemMotionResult, FxElemTraceHit, FxImpactSpawn};

pub fn evaluate_elem_motion(
    flags: i32,
    gravity_base: f32,
    gravity_amp: f32,
    elem_random_seed: u64,
    vel_graph_local: &[fx_iw4::FxElemVec3Range],
    vel_graph_world: &[fx_iw4::FxElemVec3Range],
    q: FxElemMotionQuery<'_>,
) -> Option<FxElemMotionResult> {
    if elem_uses_collision(flags) {
        return None;
    }

    Some(integrate_free_flight(
        flags,
        gravity_base,
        gravity_amp,
        elem_random_seed,
        vel_graph_local,
        vel_graph_world,
        q,
    ))
}

pub fn evaluate_elem_collide_motion(
    flags: i32,
    gravity_base: f32,
    gravity_amp: f32,
    reflection_base: f32,
    reflection_amp: f32,
    coll_mins: [f32; 3],
    coll_maxs: [f32; 3],
    use_item_clip: bool,
    has_effect_on_impact: bool,
    leaves_start_solid: bool,
    elem_random_seed: u64,
    vel_graph_local: &[fx_iw4::FxElemVec3Range],
    vel_graph_world: &[fx_iw4::FxElemVec3Range],
    q: FxElemMotionQuery<'_>,
    mut trace: impl FnMut([f32; 3], [f32; 3], [f32; 3], [f32; 3], u32) -> FxElemTraceHit,
) -> Option<FxElemMotionResult> {
    if !elem_uses_collision(flags) {
        return evaluate_elem_motion(
            flags,
            gravity_base,
            gravity_amp,
            elem_random_seed,
            vel_graph_local,
            vel_graph_world,
            q,
        );
    }

    if q.at_rest_fraction != FX_ELEM_AT_REST_NONE {
        return Some(FxElemMotionResult {
            origin_delta: [0.0; 3],
            base_vel: q.base_vel,
            remove: false,
            at_rest_fraction: Some(q.at_rest_fraction),
            spawn_impact: None,
        });
    }

    let mask = trace_mask(use_item_clip);
    let mut origin = q.origin;
    let mut base_vel = q.base_vel;
    let mut total_delta = [0.0f32; 3];
    let mut remove = false;
    let mut spawn_impact = None;
    let mut at_rest_fraction = None;

    for step in collide_substep_schedule(q.prev_msec, q.msec_now) {
        let mut t0 = step.msec_start;
        let t_end = step.msec_end;

        for _ in 0..8 {
            if t0 >= t_end || remove || at_rest_fraction.is_some() {
                break;
            }
            let life_ms = q.life_ms.max(1.0);
            let age0 = ((t0.saturating_sub(q.msec_begin)).max(0) as f32) / life_ms;
            let age1 = ((t_end.saturating_sub(q.msec_begin)).max(0) as f32) / life_ms;
            let dt_sec = (t_end.saturating_sub(t0)).max(0) as f32 * 0.001;
            let sub_q = FxElemMotionQuery {
                def_name: q.def_name,
                catalog_index: q.catalog_index,
                def_index: q.def_index,
                age0,
                age1,
                life_ms,
                dt_sec,
                base_vel,
                elem_random_seed,
                origin,
                prev_msec: t0,
                msec_now: t_end,
                msec_begin: q.msec_begin,
                effect_axis: q.effect_axis,
                orient: q.orient,
                at_rest_fraction: q.at_rest_fraction,
            };
            let free = integrate_free_flight(
                flags,
                gravity_base,
                gravity_amp,
                elem_random_seed,
                vel_graph_local,
                vel_graph_world,
                sub_q,
            );
            let start_local = origin;
            let end_local = [
                start_local[0] + free.origin_delta[0],
                start_local[1] + free.origin_delta[1],
                start_local[2] + free.origin_delta[2],
            ];
            let (start, end) = (
                orientation_pos_to_world(q.orient.origin, q.orient.axis, start_local),
                orientation_pos_to_world(q.orient.origin, q.orient.axis, end_local),
            );
            let hit = trace(start, end, coll_mins, coll_maxs, mask);
            // `leaves_start_solid`: a model piece from a prop lying on the
            // floor (a suitcase's clothes) spawns partly inside it, and a step
            // that starts inside but leaves (startsolid, not allsolid) lets it
            // out instead of freezing it there spinning.
            if hit.allsolid || (hit.startsolid && !leaves_start_solid) {
                crate::frozen_log::note(
                    "start-in-solid",
                    q.def_name,
                    q.catalog_index,
                    q.def_index,
                    format_args!(
                        "at=({:.0},{:.0},{:.0}) flags=0x{:08x} allsolid={}",
                        start[0], start[1], start[2], flags, hit.allsolid
                    ),
                );
                base_vel = [0.0; 3];
                break;
            }
            if hit.fraction >= 1.0 {
                total_delta[0] += free.origin_delta[0];
                total_delta[1] += free.origin_delta[1];
                total_delta[2] += free.origin_delta[2];
                origin = end_local;
                base_vel = free.base_vel;
                break;
            }

            let hit_world = [
                start[0] + (end[0] - start[0]) * hit.fraction,
                start[1] + (end[1] - start[1]) * hit.fraction,
                start[2] + (end[2] - start[2]) * hit.fraction,
            ];
            let hit_pos = orientation_pos_from_world(q.orient.origin, q.orient.axis, hit_world);
            total_delta[0] += hit_pos[0] - start_local[0];
            total_delta[1] += hit_pos[1] - start_local[1];
            total_delta[2] += hit_pos[2] - start_local[2];
            origin = hit_pos;

            let age_at_hit_msec = (t0.saturating_sub(q.msec_begin)).max(0) as f32
                + (t_end.saturating_sub(t0)).max(0) as f32 * hit.fraction;
            let pre_vel = get_velocity_at_time(
                flags,
                base_vel,
                age_at_hit_msec,
                life_ms,
                vel_graph_local,
                vel_graph_world,
                q.effect_axis,
                elem_random_seed,
            );
            let pre_speed_sq = vec3_length_sq(pre_vel);
            if has_effect_on_impact && impact_child_speed_allows(pre_speed_sq) {
                spawn_impact = Some(FxImpactSpawn {
                    origin: hit_world,
                    pre_vel,
                });
            }

            if elem_dies_on_touch(flags) {
                remove = true;
                break;
            }

            let reflection =
                sample_reflection_factor(reflection_base, reflection_amp, elem_random_seed);
            let scaled = [
                pre_vel[0] * reflection,
                pre_vel[1] * reflection,
                pre_vel[2] * reflection,
            ];
            let hit_at_start = hit.fraction <= 0.0;
            if hit_at_start && collide_marks_at_rest(scaled, hit.normal[2]) {
                let recip = 1.0 / life_ms;
                let frac = get_at_rest_fraction(t0 as f32, q.msec_begin as f32, recip);
                let u = if frac < 0.0 {
                    0
                } else if frac > 255.0 {
                    255
                } else {
                    frac as u8
                };
                at_rest_fraction = Some(u);
                crate::frozen_log::note(
                    "at-rest",
                    q.def_name,
                    q.catalog_index,
                    q.def_index,
                    format_args!(
                        "at=({:.0},{:.0},{:.0}) normal_z={:.2} flags=0x{:08x}",
                        hit_world[0], hit_world[1], hit_world[2], hit.normal[2], flags
                    ),
                );
                base_vel = [0.0; 3];
                break;
            }

            if hit_at_start {
                crate::frozen_log::note(
                    "touch-at-start",
                    q.def_name,
                    q.catalog_index,
                    q.def_index,
                    format_args!(
                        "at=({:.0},{:.0},{:.0}) normal=({:.2},{:.2},{:.2}) pre_vel=({:.0},{:.0},{:.0}) flags=0x{:08x}",
                        hit_world[0],
                        hit_world[1],
                        hit_world[2],
                        hit.normal[0],
                        hit.normal[1],
                        hit.normal[2],
                        pre_vel[0],
                        pre_vel[1],
                        pre_vel[2],
                        flags
                    ),
                );
            }
            let dvel = collision_reflect_base_vel_delta(pre_vel, hit.normal, reflection);
            base_vel = [
                base_vel[0] + dvel[0],
                base_vel[1] + dvel[1],
                base_vel[2] + dvel[2],
            ];

            let span = (t_end.saturating_sub(t0)).max(0) as f32;
            t0 += (span * hit.fraction) as i32;
            if t0 <= step.msec_start {
                t0 = step.msec_start.saturating_add(1);
            }
        }
        if remove || at_rest_fraction.is_some() {
            break;
        }
    }

    Some(FxElemMotionResult {
        origin_delta: total_delta,
        base_vel,
        remove,
        at_rest_fraction,
        spawn_impact,
    })
}

/// A physics piece falls at the world's gravity, not at its element's: the
/// physics world simulates it, not the effect.
const FX_PHYS_GRAVITY: f32 = 800.0;
/// Inferred: a piece on the ground slower than this after a contact is at rest.
const FX_PHYS_REST_SPEED: f32 = 20.0;
/// Highest a piece spawned inside a floor is lifted out of it.
const FX_PHYS_UNSTICK_MAX_LIFT: f32 = 16.0;
const FX_PHYS_CONTACT_SLOP: f32 = 0.125;

/// A model element with the model-physics flag (`elem_skips_position_update`):
/// the physics world owns it from its first update. It leaves its spawn point
/// at the velocity its graphs give at age 0, falls at the world's gravity,
/// bounces and slides off the world with its model's preset, and is at rest
/// once it is slow on the ground; the at-rest fraction then stops its spin as
/// well (`clamp_elem_rotation_time`). Its graphs, its own gravity and its
/// reflection factor play no further part.
///
/// Short of the retail physics world: the piece is an upright cube that does
/// not turn with the model, it spins at the element's angular velocity until it
/// rests instead of a simulated one, and it neither tips onto a face nor meets
/// other pieces or entities.
pub fn evaluate_elem_physics_motion(
    flags: i32,
    piece: FxPhysPiece,
    use_item_clip: bool,
    vel_graph_local: &[fx_iw4::FxElemVec3Range],
    vel_graph_world: &[fx_iw4::FxElemVec3Range],
    q: FxElemMotionQuery<'_>,
    mut trace: impl FnMut([f32; 3], [f32; 3], [f32; 3], [f32; 3], u32) -> FxElemTraceHit,
) -> FxElemMotionResult {
    let mut result = FxElemMotionResult {
        origin_delta: [0.0; 3],
        base_vel: q.base_vel,
        remove: false,
        at_rest_fraction: None,
        spawn_impact: None,
    };
    if q.at_rest_fraction != FX_ELEM_AT_REST_NONE {
        result.at_rest_fraction = Some(q.at_rest_fraction);
        return result;
    }
    let from = q.prev_msec.max(q.msec_begin);
    if q.msec_now <= from {
        return result;
    }
    // base_vel holds the piece's world velocity once it has been launched.
    let mut vel = if q.prev_msec <= q.msec_begin {
        elem_launch_velocity(
            flags,
            vel_graph_local,
            vel_graph_world,
            q.orient.axis,
            q.elem_random_seed,
        )
    } else {
        q.base_vel
    };
    let mask = trace_mask(use_item_clip);
    let half = piece.rest_radius;
    let (mins, maxs) = ([-half; 3], [half; 3]);
    let mut pos = orientation_pos_to_world(q.orient.origin, q.orient.axis, q.origin);
    let mut at_rest = None;
    for step in collide_substep_schedule(from, q.msec_now) {
        let dt = (step.msec_end.saturating_sub(step.msec_start)).max(0) as f32 * 0.001;
        vel[2] -= FX_PHYS_GRAVITY * dt;
        let end = [
            pos[0] + vel[0] * dt,
            pos[1] + vel[1] * dt,
            pos[2] + vel[2] * dt,
        ];
        let hit = trace(pos, end, mins, maxs, mask);
        if hit.allsolid {
            match lift_out_of_solid(pos, mins, maxs, mask, &mut trace) {
                Some(lifted) => {
                    pos = lifted;
                    vel = [0.0; 3];
                    continue;
                }
                None => {
                    vel = [0.0; 3];
                    at_rest = Some(at_rest_byte(step.msec_start, q.msec_begin, q.life_ms));
                    break;
                }
            }
        }
        if hit.fraction >= 1.0 {
            pos = end;
            continue;
        }
        pos = [
            pos[0] + (end[0] - pos[0]) * hit.fraction,
            pos[1] + (end[1] - pos[1]) * hit.fraction,
            pos[2] + (end[2] - pos[2]) * hit.fraction,
        ];
        let n = hit.normal;
        let vn = vel[0] * n[0] + vel[1] * n[1] + vel[2] * n[2];
        if vn < 0.0 {
            let push = (1.0 + piece.bounce) * vn;
            for axis in 0..3 {
                vel[axis] -= push * n[axis];
            }
            let vn = vel[0] * n[0] + vel[1] * n[1] + vel[2] * n[2];
            let keep = 1.0 - piece.friction;
            for axis in 0..3 {
                let normal_part = vn * n[axis];
                vel[axis] = normal_part + (vel[axis] - normal_part) * keep;
            }
        }
        if collide_on_ground(n[2]) && vec3_length_sq(vel) < FX_PHYS_REST_SPEED * FX_PHYS_REST_SPEED
        {
            vel = [0.0; 3];
            at_rest = Some(at_rest_byte(step.msec_end, q.msec_begin, q.life_ms));
            break;
        }
    }
    let stored = orientation_pos_from_world(q.orient.origin, q.orient.axis, pos);
    result.origin_delta = [
        stored[0] - q.origin[0],
        stored[1] - q.origin[1],
        stored[2] - q.origin[2],
    ];
    result.base_vel = vel;
    result.at_rest_fraction = at_rest;
    result
}

/// The first spot straight above `pos`, up to a small lift, where the piece
/// is clear of the world.
fn lift_out_of_solid(
    pos: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    mask: u32,
    trace: &mut impl FnMut([f32; 3], [f32; 3], [f32; 3], [f32; 3], u32) -> FxElemTraceHit,
) -> Option<[f32; 3]> {
    let mut dz = -mins[2] + FX_PHYS_CONTACT_SLOP;
    let cap = dz.max(FX_PHYS_UNSTICK_MAX_LIFT);
    while dz <= cap {
        let lifted = [pos[0], pos[1], pos[2] + dz];
        let probe = trace(lifted, lifted, mins, maxs, mask);
        if !probe.startsolid && !probe.allsolid {
            return Some(lifted);
        }
        dz *= 2.0;
    }
    None
}

fn at_rest_byte(msec: i32, msec_begin: i32, life_ms: f32) -> u8 {
    let frac = get_at_rest_fraction(msec as f32, msec_begin as f32, 1.0 / life_ms.max(1.0));
    frac.clamp(0.0, 254.0) as u8
}

fn integrate_free_flight(
    flags: i32,
    gravity_base: f32,
    gravity_amp: f32,
    elem_random_seed: u64,
    local_samples: &[fx_iw4::FxElemVec3Range],
    world_samples: &[fx_iw4::FxElemVec3Range],
    q: FxElemMotionQuery<'_>,
) -> FxElemMotionResult {
    let mut stored = q.origin;
    if elem_uses_vel_local(flags) && local_samples.len() >= 2 {
        let d =
            integrate_velocity_graph(local_samples, q.age0, q.age1, q.life_ms, elem_random_seed);
        stored[0] += d[0];
        stored[1] += d[1];
        stored[2] += d[2];
    }
    let mut world = orientation_pos_to_world(q.orient.origin, q.orient.axis, stored);
    if elem_uses_vel_world(flags) && world_samples.len() >= 2 {
        let d =
            integrate_velocity_graph(world_samples, q.age0, q.age1, q.life_ms, elem_random_seed);
        world[0] += d[0];
        world[1] += d[1];
        world[2] += d[2];
    }
    let mut base_vel = q.base_vel;
    if elem_update_has_velocity_graph(flags) {
        {
            let dt = q.dt_sec;
            let g = elem_gravity_accel_z_sampled(gravity_base, gravity_amp, elem_random_seed);
            world[0] += base_vel[0] * dt;
            world[1] += base_vel[1] * dt;
            world[2] += base_vel[2] * dt;
            world[2] -= g * dt * dt * 0.5;
            base_vel[2] -= g * dt;
        }
        stored = orientation_pos_from_world(q.orient.origin, q.orient.axis, world);
    }
    FxElemMotionResult {
        origin_delta: [
            stored[0] - q.origin[0],
            stored[1] - q.origin[1],
            stored[2] - q.origin[2],
        ],
        base_vel,
        remove: false,
        at_rest_fraction: None,
        spawn_impact: None,
    }
}

#[cfg(test)]
mod physics_piece_tests {
    use super::*;
    use fx_iw4::FxElemVec3Range;

    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    const FLOOR_Z: f32 = 192.0;
    /// props/credit_card_touchpad_exp elem 1 on mp_terminal: model physics,
    /// a local velocity graph and no gravity flag.
    const TOUCHPAD_FLAGS: i32 = 0x0900_0146;
    const LIFE_MS: i32 = 10_000;

    /// Its local graph: 75 intervals of 0.001333 up, a launch at 100 units a second.
    fn touchpad_graph() -> Vec<FxElemVec3Range> {
        vec![
            FxElemVec3Range {
                base: [0.0, 0.0, 0.1 / 75.0],
                amplitude: [0.0; 3],
            };
            76
        ]
    }

    /// The world below `FLOOR_Z` is solid; a box trace stops a clip epsilon
    /// above it.
    fn floor_trace(
        start: [f32; 3],
        end: [f32; 3],
        mins: [f32; 3],
        _maxs: [f32; 3],
        _mask: u32,
    ) -> FxElemTraceHit {
        let s = start[2] + mins[2] - FLOOR_Z;
        let e = end[2] + mins[2] - FLOOR_Z;
        if s < 0.0 && e < 0.0 {
            return FxElemTraceHit {
                fraction: 0.0,
                normal: [0.0; 3],
                startsolid: true,
                allsolid: true,
            };
        }
        if s < 0.0 {
            return FxElemTraceHit {
                fraction: 1.0,
                normal: [0.0; 3],
                startsolid: true,
                allsolid: false,
            };
        }
        if e >= 0.0 {
            return FxElemTraceHit {
                fraction: 1.0,
                normal: [0.0; 3],
                startsolid: false,
                allsolid: false,
            };
        }
        FxElemTraceHit {
            fraction: ((s - 0.125) / (s - e)).max(0.0),
            normal: [0.0, 0.0, 1.0],
            startsolid: false,
            allsolid: false,
        }
    }

    struct Piece {
        origin: [f32; 3],
        base_vel: [f32; 3],
        at_rest: u8,
    }

    fn step(piece: &mut Piece, prev: i32, now: i32, graph: &[FxElemVec3Range]) {
        let q = FxElemMotionQuery {
            def_name: "props/credit_card_touchpad_exp",
            catalog_index: 0,
            def_index: 1,
            age0: prev.max(0) as f32 / LIFE_MS as f32,
            age1: now as f32 / LIFE_MS as f32,
            life_ms: LIFE_MS as f32,
            dt_sec: (now - prev) as f32 * 0.001,
            base_vel: piece.base_vel,
            elem_random_seed: 7,
            origin: piece.origin,
            prev_msec: prev,
            msec_now: now,
            msec_begin: 0,
            effect_axis: IDENTITY,
            orient: fx_iw4::FxOrientation {
                origin: [0.0; 3],
                axis: IDENTITY,
            },
            at_rest_fraction: piece.at_rest,
        };
        let r = evaluate_elem_physics_motion(
            TOUCHPAD_FLAGS,
            FxPhysPiece::from_model(
                Some((0.1, 0.5)),
                Some(([-2.0, -3.0, -0.5], [2.0, 3.0, 0.5])),
            ),
            false,
            graph,
            &[],
            q,
            floor_trace,
        );
        for axis in 0..3 {
            piece.origin[axis] += r.origin_delta[axis];
        }
        piece.base_vel = r.base_vel;
        if let Some(f) = r.at_rest_fraction {
            piece.at_rest = f;
        }
    }

    #[test]
    fn touchpad_launches_at_its_graph_speed() {
        let v = elem_launch_velocity(TOUCHPAD_FLAGS, &touchpad_graph(), &[], IDENTITY, 7);
        assert!(v[0].abs() < 1e-3 && v[1].abs() < 1e-3, "{v:?}");
        assert!((v[2] - 100.0).abs() < 0.01, "{v:?}");
    }

    #[test]
    fn touchpad_hops_lands_and_rests_instead_of_rising_for_its_life() {
        let graph = touchpad_graph();
        let spawn = [100.0, 50.0, FLOOR_Z + 10.0];
        let mut piece = Piece {
            origin: spawn,
            base_vel: [0.0; 3],
            at_rest: FX_ELEM_AT_REST_NONE,
        };
        let mut top = spawn[2];
        let mut prev = -8;
        let mut rested_at = None;
        for frame in 0..(LIFE_MS / 16) {
            let now = frame * 16;
            step(&mut piece, prev, now, &graph);
            prev = now;
            top = top.max(piece.origin[2]);
            if rested_at.is_none() && piece.at_rest != FX_ELEM_AT_REST_NONE {
                rested_at = Some(now);
            }
        }
        // 100 units a second up against 800 down: a hop of about 6 units.
        assert!(top < spawn[2] + 8.0, "rose to {top}");
        let rested_at = rested_at.expect("never came to rest");
        assert!(rested_at < 1500, "rested only at {rested_at} ms");
        assert!(
            (piece.origin[2] - (FLOOR_Z + 0.5)).abs() < 0.5,
            "{:?}",
            piece.origin
        );
        assert_eq!(&piece.origin[..2], &spawn[..2]);
    }

    #[test]
    fn register_top_lands_and_rests_instead_of_hopping_for_its_life() {
        // props/cash_register_exp elem 1: one graph interval, 75 units a second
        // sideways and up to 200 up, and a reflection factor of 1 that kept it
        // hopping at head height for its whole ten-second life.
        let graph = vec![
            FxElemVec3Range {
                base: [0.075, 0.075, 0.2],
                amplitude: [-0.15, -0.15, -0.0517],
            };
            2
        ];
        let spawn = [0.0, 0.0, FLOOR_Z + 32.0];
        let mut piece = Piece {
            origin: spawn,
            base_vel: [0.0; 3],
            at_rest: FX_ELEM_AT_REST_NONE,
        };
        let mut prev = -8;
        let mut rested_at = None;
        for frame in 0..(LIFE_MS / 16) {
            let now = frame * 16;
            step(&mut piece, prev, now, &graph);
            prev = now;
            if rested_at.is_none() && piece.at_rest != FX_ELEM_AT_REST_NONE {
                rested_at = Some(now);
            }
        }
        let rested_at = rested_at.expect("never came to rest");
        assert!(rested_at < 2000, "rested only at {rested_at} ms");
        let travel = (piece.origin[0].powi(2) + piece.origin[1].powi(2)).sqrt();
        assert!(travel < 150.0, "slid {travel} units");
        assert!(piece.origin[2] < FLOOR_Z + 2.0, "{:?}", piece.origin);
    }

    #[test]
    fn piece_spawned_under_the_floor_is_lifted_out() {
        let graph = touchpad_graph();
        let mut piece = Piece {
            origin: [0.0, 0.0, FLOOR_Z - 2.0],
            base_vel: [0.0; 3],
            at_rest: FX_ELEM_AT_REST_NONE,
        };
        let mut prev = -8;
        for frame in 0..200 {
            let now = frame * 16;
            step(&mut piece, prev, now, &graph);
            prev = now;
        }
        assert!(piece.origin[2] >= FLOOR_Z, "{:?}", piece.origin);
        assert_ne!(piece.at_rest, FX_ELEM_AT_REST_NONE);
    }

    #[test]
    fn colliding_elem_leaves_a_brush_it_spawned_in() {
        // A suitcase's clothes (props/luggage_contents elems 6-11) spawn up to
        // two units into the floor and fly up out of it.
        let world_graph = vec![
            FxElemVec3Range {
                base: [0.0, 0.0, 0.005],
                amplitude: [0.0; 3],
            };
            42
        ];
        let q = FxElemMotionQuery {
            def_name: "props/luggage_contents",
            catalog_index: 0,
            def_index: 6,
            age0: 0.0,
            age1: 16.0 / 3000.0,
            life_ms: 3000.0,
            dt_sec: 0.016,
            base_vel: [0.0; 3],
            elem_random_seed: 7,
            origin: [0.0, 0.0, FLOOR_Z - 1.0],
            prev_msec: 0,
            msec_now: 16,
            msec_begin: 0,
            effect_axis: IDENTITY,
            orient: fx_iw4::FxOrientation {
                origin: [0.0; 3],
                axis: IDENTITY,
            },
            at_rest_fraction: FX_ELEM_AT_REST_NONE,
        };
        let r = evaluate_elem_collide_motion(
            0x0600_0366,
            0.45,
            0.0,
            0.0,
            0.0,
            [0.0; 3],
            [1.0; 3],
            false,
            false,
            true,
            7,
            &[],
            &world_graph,
            q,
            |start, end, _mins, _maxs, _mask| FxElemTraceHit {
                fraction: 1.0,
                normal: [0.0; 3],
                startsolid: start[2] < FLOOR_Z,
                allsolid: start[2] < FLOOR_Z && end[2] < FLOOR_Z - 10.0,
            },
        )
        .expect("collide motion");
        assert!(r.origin_delta[2] > 0.0, "{r:?}");
        assert!(r.at_rest_fraction.is_none());
    }
}
