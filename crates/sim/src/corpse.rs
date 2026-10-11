use crate::frame::FrameWorld;
use crate::world::ClientId;
use entity_iw4::{
    TR_GRAVITY, TR_INTERPOLATE, Trajectory, evaluate_trajectory, evaluate_trajectory_delta,
};
use playerstate_iw4::{
    AnimPair, ENTITYNUM_NONE, LINK_FLAGS_WEAPON_VIEW_ONLY, MAX_CLIENT_CORPSES,
    PLAYER_CORPSE_ENTITY_BASE, PlayerState,
};

use crate::bullet_collision::{MASK_PLAYER_SOLID, PLAYER_MAXS, PLAYER_MINS};

pub const G_CLONE_PLAYER_MAX_VELOCITY: f32 = 80.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerCorpseSlot {
    pub occupied: bool,

    pub entnum: i32,
    pub victim: ClientId,
    pub origin: [f32; 3],
    pub viewangles: [f32; 3],

    pub anim: AnimPair,
    pub view_height_current: f32,
    pub weapon: u32,
    pub e_flags: u32,

    pub tr_type: i32,

    pub tr_time: i32,
    pub tr_duration: i32,
    pub tr_delta: [f32; 3],
    pub tr_base: [f32; 3],

    pub falling: bool,

    pub ground_entity_num: i32,

    /// Level time (ms) of the death that made this body. Set once when the
    /// slot is occupied; clients key the body's animation on it, so a body
    /// keeps one death animation for its whole life.
    pub spawn_time_ms: i32,

    /// The shield on the victim's back at death. A body draws this one, never
    /// the victim's current shield, which changes when the victim respawns.
    pub shield: Option<crate::ShieldAttachment>,
}

impl Default for PlayerCorpseSlot {
    fn default() -> Self {
        Self {
            occupied: false,
            entnum: -1,
            victim: ClientId(0),
            origin: [0.0; 3],
            viewangles: [0.0; 3],
            anim: AnimPair::default(),
            view_height_current: 0.0,
            weapon: 0,
            e_flags: 0,
            tr_type: 0,
            tr_time: 0,
            tr_duration: 0,
            tr_delta: [0.0; 3],
            tr_base: [0.0; 3],
            falling: false,
            ground_entity_num: ENTITYNUM_NONE,
            spawn_time_ms: 0,
            shield: None,
        }
    }
}

impl PlayerCorpseSlot {
    pub fn trajectory(&self) -> Trajectory {
        Trajectory {
            tr_time: self.tr_time,
            tr_type: self.tr_type,
            tr_duration: self.tr_duration,
            tr_delta: self.tr_delta,
            tr_base: self.tr_base,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerCorpsePool {
    pub slots: [PlayerCorpseSlot; MAX_CLIENT_CORPSES as usize],

    pub spawn_ring: u8,
}

impl Default for PlayerCorpsePool {
    fn default() -> Self {
        Self {
            slots: [PlayerCorpseSlot::default(); MAX_CLIENT_CORPSES as usize],
            spawn_ring: 0,
        }
    }
}

impl PlayerCorpsePool {
    pub fn alloc_clone_entity(&mut self) -> i32 {
        let entnum = PLAYER_CORPSE_ENTITY_BASE + i32::from(self.spawn_ring);
        self.spawn_ring = (self.spawn_ring + 1) % MAX_CLIENT_CORPSES as u8;
        self.clear_entnum(entnum);
        entnum
    }

    pub fn clear_entnum(&mut self, entnum: i32) {
        for slot in &mut self.slots {
            if slot.entnum == entnum {
                *slot = PlayerCorpseSlot::default();
            }
        }
    }

    pub fn get_free(&mut self, ref_origin: [f32; 3]) -> u8 {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.entnum == -1 {
                return i as u8;
            }
        }
        let mut best = 0u8;
        let mut best_dist = -1.0f32;
        for (i, slot) in self.slots.iter().enumerate() {
            let dx = ref_origin[0] - slot.origin[0];
            let dy = ref_origin[1] - slot.origin[1];
            let dz = ref_origin[2] - slot.origin[2];
            let dist = dx * dx + dy * dy + dz * dz;
            if dist > best_dist {
                best_dist = dist;
                best = i as u8;
            }
        }
        let entnum = self.slots[best as usize].entnum;
        self.clear_entnum(entnum);
        best
    }

    pub fn entnums(&self) -> [i32; MAX_CLIENT_CORPSES as usize] {
        let mut nums = [-1; MAX_CLIENT_CORPSES as usize];
        for (i, slot) in self.slots.iter().enumerate() {
            nums[i] = slot.entnum;
        }
        nums
    }

    pub fn occupy(&mut self, slot: u8, entnum: i32, mut body: PlayerCorpseSlot) -> u8 {
        let i = (slot as usize) % MAX_CLIENT_CORPSES as usize;
        body.occupied = true;
        body.entnum = entnum;
        self.slots[i] = body;
        i as u8
    }
}

pub fn level_time_ms(tick: crate::world::Tick) -> i32 {
    i32::try_from(tick.0)
        .unwrap_or(0)
        .saturating_mul(crate::MATCH_TICK_MS as i32)
}

pub(crate) fn occupy_player_clone(
    world: &mut FrameWorld,
    victim: ClientId,
    ps: &PlayerState,
    time_ms: i32,
) -> u8 {
    // A shield in hand is the one dropped as an item; only a stowed one stays on the body.
    let shield = world
        .client_meta(victim)
        .and_then(|meta| meta.shield)
        .filter(|shield| shield.on_back);
    let pool = world.corpses_mut();
    let entnum = pool.alloc_clone_entity();
    let slot = pool.get_free(ps.origin);
    let slot = pool.occupy(
        slot,
        entnum,
        PlayerCorpseSlot {
            victim,
            origin: ps.origin,

            viewangles: [0.0, client_think_entity_yaw(ps), 0.0],
            anim: ps.anim(),
            view_height_current: ps.view_height_current,
            weapon: ps.weapon,
            e_flags: ps.e_flags,
            tr_type: TR_GRAVITY,
            tr_time: time_ms,
            tr_duration: 0,
            tr_delta: clamp_clone_tr_delta(ps.velocity),
            tr_base: ps.origin,
            falling: true,
            spawn_time_ms: time_ms,
            shield,
            ..PlayerCorpseSlot::default()
        },
    );
    world.corpse_dobj_tree_install(entnum, ps.anim().legs_anim);
    slot
}

fn client_think_entity_yaw(ps: &PlayerState) -> f32 {
    if (ps.link_flags & LINK_FLAGS_WEAPON_VIEW_ONLY) != 0 {
        ps.link_weapon_angles[1]
    } else {
        ps.viewangles[1]
    }
}

fn corpse_land_angles(yaw: f32, normal: [f32; 3]) -> [f32; 3] {
    let (fwd, _, _) = math_iw4::angle_vectors([0.0, yaw, 0.0]);
    let axis0 = [fwd[0], fwd[1], fwd[2]];
    let axis2 = normal;
    let axis1 = [
        axis2[1] * axis0[2] - axis2[2] * axis0[1],
        axis2[2] * axis0[0] - axis2[0] * axis0[2],
        axis2[0] * axis0[1] - axis2[1] * axis0[0],
    ];
    let axis0 = [
        axis1[1] * axis2[2] - axis1[2] * axis2[1],
        axis1[2] * axis2[0] - axis1[0] * axis2[2],
        axis1[0] * axis2[1] - axis1[1] * axis2[0],
    ];
    math_iw4::axis_to_angles([axis0, axis1, axis2])
}

// MW2's g_clonePlayerMaxVelocity limits a cloned player's speed "in each
// axis": both directions of x and y. Height is left alone.
fn clamp_clone_tr_delta(mut delta: [f32; 3]) -> [f32; 3] {
    for axis in 0..2 {
        delta[axis] = delta[axis].clamp(-G_CLONE_PLAYER_MAX_VELOCITY, G_CLONE_PLAYER_MAX_VELOCITY);
    }
    delta
}

const CORPSE_ANIM_DELTA_LEN_SQ_MIN: f32 = 1.0;

const CORPSE_ANIM_AXIS_Y_SCALE: f32 = -1.0;

const CORPSE_GROUND_PROBE_DROP: f32 = 1.0;

// A body on the ground follows the ground down by up to one stair step
// before it counts as having gone over an edge.
const CORPSE_STEP_DOWN: f32 = 18.0;

// How far off a wall a falling body restarts its slide down it.
const CORPSE_WALL_SLIDE_GAP: f32 = 0.125;

const CORPSE_UNSTICK_Z_LIFT: f32 = 32.0;

const CORPSE_UNSTICK_CLIPMASK_DROP: u32 = 0x0001_0000;

const TR_RAGDOLL_SETTLED: i32 = 12;

const TR_BOUNCE_GRAVITY: i32 = 0xb;

fn is_ragdoll_tr_type(tr_type: i32) -> bool {
    (10..=12).contains(&tr_type)
}

pub(crate) fn phase_run_corpse_move(world: &mut FrameWorld, time_ms: i32) {
    let n = world.corpses().slots.len();
    for i in 0..n {
        let slot = world.corpses().slots[i];
        if !slot.occupied {
            continue;
        }
        let anim_delta = world.corpse_dobj_tree_delta(slot.entnum, crate::MATCH_TICK_MS as i32);
        let frame = &*world;
        let stepped = step_corpse(slot, anim_delta, time_ms, |start, end, mask| {
            frame.trace_clip(start, end, PLAYER_MINS, PLAYER_MAXS, mask)
        });
        world.corpses_mut().slots[i] = stepped;
    }
    let live = world.corpses().entnums();
    world.corpse_dobj_tree_retain(&live);
}

// One server tick of one body. `anim_delta` is the death animation's root
// step for this tick in the body's own axes; `trace(start, end, mask)` sweeps
// the standing player box through the world. The body moves along its
// trajectory plus that step:
// - in the air the trajectory keeps its clock and speed, so gravity keeps
//   building, and the step is added to its base once;
// - on the ground only the step's x/y moves it; the ground below (a probe of
//   one unit, then one stair step) and gravity own its height, and with no
//   ground within a step it falls from rest;
// - a falling body that meets a wall slides down it at the speed it had.
pub(crate) fn step_corpse(
    mut slot: PlayerCorpseSlot,
    anim_delta: Option<[f32; 3]>,
    time_ms: i32,
    mut trace: impl FnMut([f32; 3], [f32; 3], u32) -> trace_iw4::Trace,
) -> PlayerCorpseSlot {
    let was_falling = slot.falling;
    let ragdoll = is_ragdoll_tr_type(slot.tr_type);
    let anim_delta = anim_delta.filter(|d| {
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > CORPSE_ANIM_DELTA_LEN_SQ_MIN
            && (!was_falling || !ragdoll)
    });
    if !was_falling && anim_delta.is_none() {
        return slot;
    }
    let world_delta = anim_delta.map(|delta| {
        let step = corpse_anim_world_delta(slot.viewangles, delta);
        if was_falling {
            step
        } else {
            [step[0], step[1], 0.0]
        }
    });

    let mut desired = evaluate_trajectory(&slot.trajectory(), time_ms);
    if let Some(step) = world_delta {
        for axis in 0..3 {
            desired[axis] += step[axis];
        }
    }
    let hit = trace(slot.origin, desired, MASK_PLAYER_SOLID);
    let endpos = if hit.fraction >= 1.0 {
        desired
    } else {
        hit.endpos
    };
    slot.origin = endpos;

    if hit.fraction >= 1.0 && hit.startsolid == 0 {
        let Some(step) = world_delta else {
            return slot;
        };
        if was_falling {
            for axis in 0..3 {
                slot.tr_base[axis] += step[axis];
            }
            return slot;
        }
        slot.tr_type = if ragdoll {
            TR_RAGDOLL_SETTLED
        } else {
            TR_INTERPOLATE
        };
        slot.tr_base = endpos;
        slot.tr_time = 0;
        slot.tr_duration = 0;
        slot.tr_delta = [0.0; 3];
        follow_ground_or_fall(&mut slot, time_ms, ragdoll, &mut trace);
        return slot;
    }

    if !was_falling {
        // Stopped part way: clients draw the body where it stopped.
        slot.tr_base = endpos;
        if hit.fraction > 0.0 && hit.startsolid == 0 {
            follow_ground_or_fall(&mut slot, time_ms, ragdoll, &mut trace);
        }
        return slot;
    }

    if hit.allsolid != 0 {
        let mut lifted = slot.origin;
        lifted[2] += CORPSE_UNSTICK_Z_LIFT;
        let retry = trace(
            lifted,
            desired,
            MASK_PLAYER_SOLID & !CORPSE_UNSTICK_CLIPMASK_DROP,
        );
        if retry.allsolid == 0 {
            let f = retry.fraction;
            slot.origin = [
                lifted[0] + (desired[0] - lifted[0]) * f,
                lifted[1] + (desired[1] - lifted[1]) * f,
                lifted[2] + (desired[2] - lifted[2]) * f,
            ];
        }
        land_corpse(&mut slot, &hit);
        return slot;
    }
    if hit.normal[2] <= 0.0 {
        if hit.startsolid != 0 {
            for axis in 0..3 {
                slot.origin[axis] += hit.normal[axis];
            }
            slot.tr_base = slot.origin;
            slot.tr_time = time_ms;
            slot.tr_delta = [0.0; 3];
            return slot;
        }
        slide_down_wall(&mut slot, &hit, desired, time_ms, &mut trace);
        return slot;
    }
    land_corpse(&mut slot, &hit);
    slot
}

fn follow_ground_or_fall(
    slot: &mut PlayerCorpseSlot,
    time_ms: i32,
    ragdoll: bool,
    trace: &mut impl FnMut([f32; 3], [f32; 3], u32) -> trace_iw4::Trace,
) {
    let at = slot.origin;
    let mut probe = at;
    probe[2] -= CORPSE_GROUND_PROBE_DROP;
    let ground = trace(at, probe, MASK_PLAYER_SOLID);
    if ground.fraction < 1.0 || ground.startsolid != 0 {
        slot.falling = false;
        return;
    }
    let mut step_probe = at;
    step_probe[2] -= CORPSE_STEP_DOWN;
    let step = trace(at, step_probe, MASK_PLAYER_SOLID);
    if step.fraction < 1.0 && step.startsolid == 0 && step.allsolid == 0 && step.normal[2] > 0.0 {
        slot.origin = step.endpos;
        slot.tr_base = step.endpos;
        slot.falling = false;
        slot.ground_entity_num = i32::from(trace_iw4::trace_get_entity_hit_id(
            step.hit_type,
            step.hit_id,
        ));
        return;
    }
    // Over an edge with nothing within a step below: fall from rest. The
    // animation's steps keep carrying the body while it falls.
    slot.falling = true;
    slot.tr_type = if ragdoll {
        TR_BOUNCE_GRAVITY
    } else {
        TR_GRAVITY
    };
    slot.tr_base = at;
    slot.tr_time = time_ms;
    slot.tr_duration = 0;
    slot.tr_delta = [0.0; 3];
}

fn slide_down_wall(
    slot: &mut PlayerCorpseSlot,
    hit: &trace_iw4::Trace,
    desired: [f32; 3],
    time_ms: i32,
    trace: &mut impl FnMut([f32; 3], [f32; 3], u32) -> trace_iw4::Trace,
) {
    let n = hit.normal;
    let mut start = hit.endpos;
    let mut rest = [0.0f32; 3];
    for axis in 0..3 {
        start[axis] += n[axis] * CORPSE_WALL_SLIDE_GAP;
        rest[axis] = desired[axis] - hit.endpos[axis];
    }
    let into_wall = dot(rest, n);
    let mut end = start;
    for axis in 0..3 {
        end[axis] += rest[axis] - into_wall * n[axis];
    }
    let slid = trace(start, end, MASK_PLAYER_SOLID);
    if slid.allsolid == 0 && slid.startsolid == 0 {
        slot.origin = if slid.fraction >= 1.0 {
            end
        } else {
            slid.endpos
        };
        if slid.fraction < 1.0 && slid.normal[2] > 0.0 {
            land_corpse(slot, &slid);
            return;
        }
    }
    // Still in the air: the wall stops the throw, the fall keeps its speed
    // (less any speed into an overhang).
    let speed = evaluate_trajectory_delta(&slot.trajectory(), time_ms);
    let mut velocity = [0.0, 0.0, speed[2]];
    let into = dot(velocity, n);
    if into < 0.0 {
        for axis in 0..3 {
            velocity[axis] -= into * n[axis];
        }
    }
    slot.tr_base = slot.origin;
    slot.tr_time = time_ms;
    slot.tr_duration = 0;
    slot.tr_delta = velocity;
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn corpse_anim_world_delta(angles: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
    let (mut fwd, right, up) = math_iw4::angle_vectors(angles);
    let mut left = [
        right[0] * CORPSE_ANIM_AXIS_Y_SCALE,
        right[1] * CORPSE_ANIM_AXIS_Y_SCALE,
        right[2] * CORPSE_ANIM_AXIS_Y_SCALE,
    ];
    normalize(&mut fwd);
    normalize(&mut left);
    let mut out = [0.0f32; 3];
    for (axis, slot) in out.iter_mut().enumerate() {
        *slot = delta[0] * fwd[axis] + delta[1] * left[axis] + delta[2] * up[axis];
    }
    out
}

// The body comes to rest on what it hit (or where it was unstuck to).
fn land_corpse(slot: &mut PlayerCorpseSlot, hit: &trace_iw4::Trace) {
    let ragdoll = is_ragdoll_tr_type(slot.tr_type);
    slot.tr_delta = [0.0; 3];
    slot.falling = false;
    slot.tr_type = if ragdoll {
        TR_RAGDOLL_SETTLED
    } else {
        TR_INTERPOLATE
    };
    slot.tr_base = slot.origin;
    slot.tr_time = 0;
    slot.tr_duration = 0;

    slot.ground_entity_num =
        i32::from(trace_iw4::trace_get_entity_hit_id(hit.hit_type, hit.hit_id));
    if hit.allsolid == 0 {
        slot.viewangles = corpse_land_angles(slot.viewangles[1], hit.normal);
    }
}

fn normalize(v: &mut [f32; 3]) {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let len = if len <= 0.0 { 1.0 } else { len };
    v[0] /= len;
    v[1] /= len;
    v[2] /= len;
}

pub(crate) fn phase_sync_corpse_info(world: &mut FrameWorld) {
    let n = world.corpses().slots.len();
    for i in 0..n {
        if world.corpses().slots[i].occupied {
            sync_corpse_info_player_anims(world, i);
        }
    }
}

pub(crate) fn sync_corpse_info_player_anims(world: &mut FrameWorld, slot: usize) {
    let body = world.corpses().slots[slot];
    let src = entity_iw4::CorpseInfoPlayerAnimCopy {
        legs_anim: body.anim.legs_anim,
        torso_anim: body.anim.torso_anim,
        torso_pitch: 0,
        waist_pitch: 0,
    };
    let Some(copied) = entity_iw4::corpse_info::copy_player_anims(false, src) else {
        return;
    };
    let slot = &mut world.corpses_mut().slots[slot];
    slot.anim.legs_anim = copied.legs_anim;
    slot.anim.torso_anim = copied.torso_anim;
}

#[cfg(test)]
mod step_corpse_tests {
    use super::*;

    const TICK_MS: i32 = crate::MATCH_TICK_MS as i32;

    // Solid boxes (mins, maxs). The trace sweeps the standing player box and
    // stops 0.125 units short of what it hits, as the world trace does.
    struct Boxes(Vec<([f32; 3], [f32; 3])>);

    impl Boxes {
        fn trace(&self, start: [f32; 3], end: [f32; 3], _mask: u32) -> trace_iw4::Trace {
            let d = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
            let len = dot(d, d).sqrt();
            let mut best = 1.0f32;
            let mut normal = [0.0f32; 3];
            let mut startsolid = false;
            let mut allsolid = false;
            for (mins, maxs) in &self.0 {
                let lo = [0, 1, 2].map(|a| mins[a] - PLAYER_MAXS[a]);
                let hi = [0, 1, 2].map(|a| maxs[a] - PLAYER_MINS[a]);
                let inside = |p: [f32; 3]| (0..3).all(|a| p[a] > lo[a] && p[a] < hi[a]);
                if inside(start) {
                    startsolid = true;
                    allsolid |= inside(end);
                    continue;
                }
                let mut enter = 0.0f32;
                let mut exit = 1.0f32;
                let mut enter_normal = [0.0f32; 3];
                let mut misses = false;
                for a in 0..3 {
                    if d[a] == 0.0 {
                        misses |= start[a] <= lo[a] || start[a] >= hi[a];
                        continue;
                    }
                    let (t0, t1) = ((lo[a] - start[a]) / d[a], (hi[a] - start[a]) / d[a]);
                    let (near, far) = if t0 < t1 { (t0, t1) } else { (t1, t0) };
                    if near > enter {
                        enter = near;
                        enter_normal = [0.0; 3];
                        enter_normal[a] = if d[a] > 0.0 { -1.0 } else { 1.0 };
                    }
                    exit = exit.min(far);
                }
                if !misses && enter <= exit && enter < best && enter_normal != [0.0; 3] {
                    best = enter;
                    normal = enter_normal;
                }
            }
            if allsolid {
                return trace_iw4::Trace {
                    fraction: 0.0,
                    startsolid: 1,
                    allsolid: 1,
                    endpos: start,
                    ..Default::default()
                };
            }
            let fraction = if best < 1.0 {
                ((best * len - 0.125) / len).max(0.0)
            } else {
                1.0
            };
            trace_iw4::Trace {
                fraction,
                normal,
                startsolid: u8::from(startsolid),
                endpos: [0, 1, 2].map(|a| start[a] + d[a] * fraction),
                ..Default::default()
            }
        }
    }

    fn floor(top: f32, x0: f32, x1: f32) -> ([f32; 3], [f32; 3]) {
        ([x0, -1000.0, top - 100.0], [x1, 1000.0, top])
    }

    fn grounded(origin: [f32; 3]) -> PlayerCorpseSlot {
        PlayerCorpseSlot {
            occupied: true,
            entnum: 1001,
            origin,
            tr_type: TR_INTERPOLATE,
            tr_base: origin,
            falling: false,
            ..PlayerCorpseSlot::default()
        }
    }

    fn just_cloned(origin: [f32; 3], time_ms: i32, velocity: [f32; 3]) -> PlayerCorpseSlot {
        PlayerCorpseSlot {
            occupied: true,
            entnum: 1001,
            origin,
            tr_type: TR_GRAVITY,
            tr_time: time_ms,
            tr_delta: clamp_clone_tr_delta(velocity),
            tr_base: origin,
            falling: true,
            spawn_time_ms: time_ms,
            ..PlayerCorpseSlot::default()
        }
    }

    fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
        let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
        dot(d, d).sqrt()
    }

    #[test]
    fn a_clone_is_held_to_80_units_a_second_each_way_in_x_and_y() {
        assert_eq!(clamp_clone_tr_delta([-300.0, 0.0, 0.0]), [-80.0, 0.0, 0.0]);
        assert_eq!(
            clamp_clone_tr_delta([300.0, -300.0, 50.0]),
            [80.0, -80.0, 50.0]
        );
        assert_eq!(
            clamp_clone_tr_delta([40.0, -20.0, -900.0]),
            [40.0, -20.0, -900.0]
        );
    }

    #[test]
    fn a_body_carried_off_a_ledge_falls_once_and_lands_below() {
        let world = Boxes(vec![
            floor(0.0, -1000.0, 0.0),
            floor(-200.0, -1000.0, 1000.0),
        ]);
        let step = [8.0, 0.0, 0.0];
        let mut slot = grounded([-10.0, 0.0, 0.125]);
        let mut time_ms = 1000;
        let mut fall: Option<(i32, f32)> = None;
        for _ in 0..60 {
            time_ms += TICK_MS;
            let before = slot;
            slot = step_corpse(slot, Some(step), time_ms, |s, e, m| world.trace(s, e, m));
            let travel_x = slot.origin[0] - before.origin[0];
            assert!(
                travel_x <= step[0] + 0.01,
                "one step a tick, moved {travel_x}"
            );
            if slot.falling {
                let (start_ms, start_z) = *fall.get_or_insert((slot.tr_time, slot.origin[2]));
                assert_eq!(slot.tr_time, start_ms, "the fall keeps its clock");
                assert_eq!(slot.tr_type, TR_GRAVITY);
                let dt = (time_ms - start_ms) as f32 / 1000.0;
                let drop = start_z - slot.origin[2];
                assert!(
                    (drop - 400.0 * dt * dt).abs() < 1.0,
                    "drop {drop} at {dt} s"
                );
            } else if fall.is_some() {
                break;
            }
        }
        assert!(fall.is_some(), "the body went over the edge");
        assert!(!slot.falling);
        assert_eq!(slot.tr_type, TR_INTERPOLATE);
        assert!(
            (slot.origin[2] - -199.875).abs() < 0.05,
            "on the lower floor: {:?}",
            slot.origin
        );
        assert_eq!(slot.tr_base, slot.origin);
    }

    #[test]
    fn a_body_on_flat_ground_moves_the_whole_step_even_when_the_step_points_down() {
        let world = Boxes(vec![floor(0.0, -1000.0, 1000.0)]);
        let mut slot = grounded([0.0, 0.0, 0.125]);
        for tick in 1..=5 {
            slot = step_corpse(
                slot,
                Some([8.0, 0.0, -3.0]),
                1000 + tick * TICK_MS,
                |s, e, m| world.trace(s, e, m),
            );
        }
        assert!((slot.origin[0] - 40.0).abs() < 0.01, "{:?}", slot.origin);
        assert_eq!(slot.origin[2], 0.125);
        assert_eq!(slot.tr_base, slot.origin);
        assert!(!slot.falling);
    }

    #[test]
    fn a_body_walks_down_a_stair_without_falling() {
        let world = Boxes(vec![
            floor(0.0, -1000.0, 0.0),
            floor(-10.0, -1000.0, 1000.0),
        ]);
        let mut slot = grounded([-10.0, 0.0, 0.125]);
        for tick in 1..=6 {
            slot = step_corpse(
                slot,
                Some([8.0, 0.0, 0.0]),
                1000 + tick * TICK_MS,
                |s, e, m| world.trace(s, e, m),
            );
            assert!(!slot.falling);
            assert_eq!(slot.tr_type, TR_INTERPOLATE);
        }
        assert!((slot.origin[0] - 38.0).abs() < 0.01, "{:?}", slot.origin);
        assert!((slot.origin[2] - -9.875).abs() < 0.01, "{:?}", slot.origin);
        assert_eq!(slot.tr_base, slot.origin);
    }

    #[test]
    fn a_body_stopped_by_a_wall_is_drawn_where_it_stopped() {
        let world = Boxes(vec![
            floor(0.0, -1000.0, 1000.0),
            ([-1000.0, -1000.0, 0.0], [0.0, 1000.0, 1000.0]),
        ]);
        let slot = step_corpse(
            grounded([20.0, 0.0, 0.125]),
            Some([-8.0, 0.0, 0.0]),
            1050,
            |s, e, m| world.trace(s, e, m),
        );
        assert!((slot.origin[0] - 15.125).abs() < 0.01, "{:?}", slot.origin);
        assert_eq!(slot.tr_base, slot.origin);
    }

    #[test]
    fn a_falling_body_against_a_wall_slides_down_it_and_lands() {
        let world = Boxes(vec![
            floor(0.0, -1000.0, 1000.0),
            ([-1000.0, -1000.0, 0.0], [0.0, 1000.0, 1000.0]),
        ]);
        let mut slot = just_cloned([15.5, 0.0, 20.0], 1000, [0.0; 3]);
        let mut landed_at = None;
        for tick in 1..=20 {
            let time_ms = 1000 + tick * TICK_MS;
            slot = step_corpse(slot, Some([-8.0, 0.0, 0.0]), time_ms, |s, e, m| {
                world.trace(s, e, m)
            });
            assert!(
                slot.origin[0] >= 15.0,
                "never into the wall: {:?}",
                slot.origin
            );
            if !slot.falling && landed_at.is_none() {
                landed_at = Some(time_ms);
            }
            if landed_at.is_some() {
                assert!(!slot.falling);
                assert_eq!(slot.tr_time, 0);
                assert_eq!(slot.tr_type, TR_INTERPOLATE);
            }
        }
        let landed_at = landed_at.expect("lands");
        assert!(
            landed_at - 1000 <= 300,
            "landed after {} ms",
            landed_at - 1000
        );
        assert!((slot.origin[2] - 0.125).abs() < 0.01, "{:?}", slot.origin);
    }

    #[test]
    fn a_shotgun_hop_lands_quickly_and_travels_the_clip_distance() {
        // pb_shotgun_death_back2-like root motion: 120 units back over the
        // clip, 11.6 units up early and down again.
        let world = Boxes(vec![floor(0.0, -1000.0, 1000.0)]);
        let ticks = 29;
        let mut slot = just_cloned([0.0, 0.0, 0.125], 1000, [0.0; 3]);
        let start = slot.origin;
        let mut landed_at = None;
        let mut previous_trajectory_point = evaluate_trajectory(&slot.trajectory(), 1000);
        for tick in 1..=ticks {
            let time_ms = 1000 + tick * TICK_MS;
            let z = match tick {
                1..=5 => 11.6 / 5.0,
                10..=14 => -11.6 / 5.0,
                _ => 0.0,
            };
            let step = [-120.0 / ticks as f32, 0.0, z];
            let before = slot;
            let trajectory_point = evaluate_trajectory(&before.trajectory(), time_ms);
            let trajectory_move = dist(trajectory_point, previous_trajectory_point);
            slot = step_corpse(slot, Some(step), time_ms, |s, e, m| world.trace(s, e, m));
            let moved = dist(slot.origin, before.origin);
            assert!(
                moved <= dot(step, step).sqrt() + trajectory_move + 0.01,
                "tick {tick}: moved {moved}"
            );
            previous_trajectory_point = evaluate_trajectory(&slot.trajectory(), time_ms);
            if !slot.falling && landed_at.is_none() {
                landed_at = Some(time_ms);
            }
        }
        let landed_at = landed_at.expect("lands");
        assert!(
            landed_at - 1000 <= 250,
            "landed after {} ms",
            landed_at - 1000
        );
        let travel = start[0] - slot.origin[0];
        assert!((travel - 120.0).abs() <= 12.0, "travelled {travel}");
        assert!(
            slot.origin[2] >= 0.0 && slot.origin[2] < 0.2,
            "{:?}",
            slot.origin
        );
    }

    #[test]
    fn a_body_thrown_toward_minus_x_flies_no_further_than_toward_plus_x() {
        let world = Boxes(vec![floor(-500.0, -1000.0, 1000.0)]);
        let fly = |velocity: [f32; 3]| {
            let mut slot = just_cloned([0.0, 0.0, 0.0], 1000, velocity);
            for tick in 1..=10 {
                slot = step_corpse(slot, None, 1000 + tick * TICK_MS, |s, e, m| {
                    world.trace(s, e, m)
                });
            }
            slot.origin[0]
        };
        let plus = fly([300.0, 0.0, 0.0]);
        let minus = fly([-300.0, 0.0, 0.0]);
        assert!((plus + minus).abs() < 0.01, "+x {plus}, -x {minus}");
        assert!((plus - 40.0).abs() < 0.01, "80 units/s for 0.5 s: {plus}");
    }
}
