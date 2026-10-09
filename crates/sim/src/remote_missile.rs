use crate::ProjectileId;
use crate::bullet_collision::{CONTENTS_SKY, ColliderId, MASK_BULLET_WORLD, TraceOutcome};
use crate::frame::FrameWorld;
use crate::match_state::ClientLifecycle;
use crate::world::{ClientId, Tick};
use bevy_ecs::prelude::{Resource, World};

const PITCH_RANGE: [f32; 2] = [1.0, 87.0];
const PITCH_RATE: f32 = 15.0;
const YAW_RATE: f32 = 20.0;
const SPEED_RANGE: [f32; 2] = [3_000.0, 6_000.0];
const SPEED_UP: f32 = 2_000.0;
const SPEED_DOWN: f32 = 500.0;

// A sky face this close to facing straight up is a sky ceiling seen from above.
const SKY_CEILING_MIN_NORMAL_Z: f32 = 0.7;

// MW2 starts the Predator 14000 units from its target (_remotemissile.gsc _fire), or
// 14000 above the player on maps without remoteMissileSpawn points. On Sub Base,
// Wasteland, Karachi, Invasion, Rundown, Carnival, Storm and Trailer Park (and Fuel
// from a rooftop) that start is above or inside the map's sky ceiling, and the missile
// still flies down through it in MW2. So a remote missile ignores sky brushes, except
// a sky face that is not a ceiling top: sky walls still end it. MW2 also gives this
// missile a lifetime explosion, which the port does not read, so without the walls a
// Predator steered out of the map would fly on until the 60 s rocket cleanup.
//
// The mark is set when ControlsLinkTo links a player to the missile and kept for its
// whole flight. The control link itself is dropped when the pilot dies, and the
// missile must still pass the ceiling then.
#[derive(Resource, Debug, Default)]
pub(crate) struct RemoteMissiles(Vec<ProjectileId>);

pub(crate) fn mark_remote(world: &mut World, id: ProjectileId) {
    let mut marked = world.get_resource_or_insert_with(RemoteMissiles::default);
    if !marked.0.contains(&id) {
        marked.0.push(id);
    }
}

pub(crate) fn is_remote(world: &mut FrameWorld, id: ProjectileId) -> bool {
    world
        .ecs()
        .get_resource::<RemoteMissiles>()
        .is_some_and(|marked| marked.0.contains(&id))
}

fn forget_gone_missiles(world: &mut FrameWorld) {
    let marked = match world.ecs().get_resource::<RemoteMissiles>() {
        Some(marked) if !marked.0.is_empty() => marked.0.clone(),
        _ => return,
    };
    let mut flying = Vec::with_capacity(marked.len());
    world.visit_projectiles(|p| {
        if marked.contains(&p.id) {
            flying.push(p.id);
        }
    });
    if flying.len() != marked.len() {
        world.ecs().resource_mut::<RemoteMissiles>().0 = flying;
    }
}

pub(crate) fn projectile_mask(remote: bool) -> u32 {
    if remote {
        MASK_BULLET_WORLD & !CONTENTS_SKY
    } else {
        MASK_BULLET_WORLD
    }
}

/// The remote missile's world trace skips sky brushes. `sky` is a sky-only trace over
/// the same tick (from `start`): a sky wall or underside nearer than the world outcome
/// becomes the hit, so the usual contact detonation runs. A ceiling top is passed, and
/// a trace that starts inside a sky brush (the missile is in the ceiling slab) is
/// ignored.
pub(crate) fn sky_wall_outcome(
    outcome: TraceOutcome,
    start: [f32; 3],
    sky: &trace_iw4::Trace,
) -> TraceOutcome {
    if sky.fraction >= 1.0
        || sky.startsolid != 0
        || sky.allsolid != 0
        || sky.normal[2] > SKY_CEILING_MIN_NORMAL_Z
    {
        return outcome;
    }
    let reach = match outcome {
        TraceOutcome::Hit { end, .. } | TraceOutcome::Miss { end } => distance(start, end),
        TraceOutcome::StartSolid { .. } | TraceOutcome::Invalid { .. } => return outcome,
    };
    if distance(start, sky.endpos) >= reach {
        return outcome;
    }
    TraceOutcome::Hit {
        fraction: sky.fraction,
        end: sky.endpos,
        normal: sky.normal,
        collider: ColliderId::World {
            surface_flags: sky.surface_flags,
            contents: sky.contents,
            glass_encoded: 0,
        },
    }
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

pub(crate) fn steer(
    world: &mut FrameWorld,
    id: ClientId,
    cmd: &playerstate_iw4::UserCmd,
    msec: i32,
) {
    if !world.publishes_snapshot() {
        return;
    }
    let Some(mut link) = world
        .client_meta(id)
        .and_then(|m| m.remote_missile)
        .filter(|link| link.unlink_at_ms.is_none())
    else {
        return;
    };
    let seconds = msec as f32 * 0.001;
    let delta = math_iw4::angles_to_axis([
        f32::from(cmd.remote_control[0] as i8) / 127.0 * seconds * PITCH_RATE,
        f32::from(cmd.remote_control[1] as i8) / 127.0 * seconds * YAW_RATE,
        0.0,
    ]);
    let transposed = core::array::from_fn(|row| core::array::from_fn(|col| delta[col][row]));
    let mut angles = math_iw4::axis_to_angles(math_iw4::matrix_multiply(
        transposed,
        math_iw4::angles_to_axis(link.angles),
    ));
    angles[0] = (angles[0] / 360.0 - (angles[0] / 360.0 + 0.5).floor()) * 360.0;
    angles[0] = angles[0].clamp(PITCH_RANGE[0], PITCH_RANGE[1]);
    link.angles = angles;
    let held = cmd.buttons & playerstate_iw4::buttons::REMOTE_CONTROL != 0;
    link.attack = held && cmd.buttons & playerstate_iw4::buttons::ATTACK != 0;
    link.armed |= held && !link.attack;
    world.client_meta_mut(id).remote_missile = Some(link);
}

pub(crate) fn advance(world: &mut FrameWorld, tick: Tick) {
    if !world.publishes_snapshot() {
        return;
    }
    forget_gone_missiles(world);
    let now = crate::level_time_ms(tick);
    for id in world.client_ids_sorted() {
        let Some(mut link) = world.client_meta(id).and_then(|m| m.remote_missile) else {
            continue;
        };
        let alive = world
            .client_meta(id)
            .is_some_and(|m| m.lifecycle == ClientLifecycle::Alive);
        if !alive {
            world.client_meta_mut(id).remote_missile = None;
            continue;
        }
        if link.unlink_at_ms.is_some() {
            continue;
        }
        let Some(projectile) = world
            .projectile_mut_by_number(link.entnum)
            .filter(|p| p.id == link.projectile && p.live)
        else {
            continue;
        };
        let (dir, _, _) = math_iw4::angle_vectors(link.angles);
        let seconds = crate::MATCH_TICK_MS as f32 * 0.001;
        let mut speed = projectile
            .velocity
            .iter()
            .zip(dir)
            .map(|(v, d)| v * d)
            .sum::<f32>()
            .max(0.0);
        if link.armed && !link.boosted && link.attack {
            speed = SPEED_RANGE[1];
            link.boosted = true;
        } else {
            let target = SPEED_RANGE[0];
            speed = if speed < target {
                (speed + SPEED_UP * seconds).min(target)
            } else {
                (speed - SPEED_DOWN * seconds).max(target)
            };
        }
        let velocity =
            entity_iw4::truncated_tr_delta([dir[0] * speed, dir[1] * speed, dir[2] * speed]);
        projectile.velocity = velocity;
        projectile.pos = entity_iw4::Trajectory {
            tr_time: now.saturating_sub(crate::MATCH_TICK_MS as i32),
            tr_type: entity_iw4::TR_LINEAR,
            tr_duration: 0,
            tr_delta: velocity,
            tr_base: projectile.origin,
        };
        projectile.apos = entity_iw4::fire_missile_apos(dir);
        world.client_meta_mut(id).remote_missile = Some(link);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: [f32; 3] = [0.0, 0.0, 9000.0];
    const TICK_END: [f32; 3] = [0.0, 0.0, 8850.0];

    fn sky(fraction: f32, normal: [f32; 3]) -> trace_iw4::Trace {
        trace_iw4::Trace {
            fraction,
            normal,
            endpos: core::array::from_fn(|i| START[i] + (TICK_END[i] - START[i]) * fraction),
            surface_flags: 0x34,
            contents: CONTENTS_SKY,
            ..Default::default()
        }
    }

    fn miss() -> TraceOutcome {
        TraceOutcome::Miss { end: TICK_END }
    }

    #[test]
    fn only_the_remote_missile_trace_skips_sky() {
        assert_eq!(projectile_mask(true) & CONTENTS_SKY, 0);
        assert_eq!(projectile_mask(true) | CONTENTS_SKY, MASK_BULLET_WORLD);
        assert_eq!(projectile_mask(false), MASK_BULLET_WORLD);
        assert_ne!(projectile_mask(false) & CONTENTS_SKY, 0);
    }

    #[test]
    fn a_sky_ceiling_top_lets_the_missile_through() {
        assert_eq!(
            sky_wall_outcome(miss(), START, &sky(0.4, [0.0, 0.0, 1.0])),
            miss()
        );
        assert_eq!(
            sky_wall_outcome(miss(), START, &sky(0.4, [0.0, 0.6, 0.8])),
            miss()
        );
    }

    #[test]
    fn sky_walls_and_undersides_still_stop_it() {
        for normal in [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 0.8, 0.6]] {
            let hit = sky(0.4, normal);
            assert_eq!(
                sky_wall_outcome(miss(), START, &hit),
                TraceOutcome::Hit {
                    fraction: 0.4,
                    end: hit.endpos,
                    normal,
                    collider: ColliderId::World {
                        surface_flags: 0x34,
                        contents: CONTENTS_SKY,
                        glass_encoded: 0,
                    },
                }
            );
        }
    }

    #[test]
    fn a_missile_inside_a_sky_brush_is_not_stopped() {
        let mut inside = sky(0.4, [1.0, 0.0, 0.0]);
        inside.startsolid = 1;
        assert_eq!(sky_wall_outcome(miss(), START, &inside), miss());
        inside.allsolid = 1;
        assert_eq!(sky_wall_outcome(miss(), START, &inside), miss());
        assert_eq!(
            sky_wall_outcome(miss(), START, &sky(1.0, [1.0, 0.0, 0.0])),
            miss()
        );
    }

    #[test]
    fn a_nearer_world_hit_wins_over_a_sky_wall() {
        let ground = TraceOutcome::Hit {
            fraction: 0.2,
            end: [0.0, 0.0, 8970.0],
            normal: [0.0, 0.0, 1.0],
            collider: ColliderId::World {
                surface_flags: 0,
                contents: 1,
                glass_encoded: 0,
            },
        };
        assert_eq!(
            sky_wall_outcome(ground, START, &sky(0.4, [1.0, 0.0, 0.0])),
            ground
        );
        let solid = TraceOutcome::StartSolid {
            collider: None,
            end: START,
        };
        assert_eq!(
            sky_wall_outcome(solid, START, &sky(0.4, [1.0, 0.0, 0.0])),
            solid
        );
    }

    #[test]
    fn the_mark_outlives_the_pilot_and_ends_with_the_missile() {
        let mut sim = crate::SimWorld::default();
        let mut frame = sim.frame();
        frame.entity_kernel_mut().begin_frame(0);
        let id = frame.allocate_projectile_id();
        let entnum = frame
            .allocate_dynamic_entity(crate::gentity::EntityRunKind::Missile)
            .expect("missile slot")
            .number();
        let trajectory = entity_iw4::Trajectory::default();
        frame.push_projectile(crate::ProjectileState {
            id,
            owner: ClientId(0),
            owner_life: crate::LifeSequence(1),
            weapon: 1,
            origin: START,
            velocity: [0.0, 0.0, -3000.0],
            pos: trajectory,
            apos: trajectory,
            entnum,
            launch_time: 0,
            spawn_time_ms: 0,
            detonate_at_ms: None,
            cleanup_at_ms: crate::equipment::ROCKET_CLEANUP_MS,
            travel_distance: 0.0,
            live: true,
            stuck_pane: None,
            grounded: false,
            guide: crate::MissileGuide::default(),
            attached_to: None,
        });
        assert!(!is_remote(&mut frame, id));
        mark_remote(frame.ecs(), id);
        mark_remote(frame.ecs(), id);
        // No client holds a control link here, as after the pilot dies.
        forget_gone_missiles(&mut frame);
        assert!(is_remote(&mut frame, id));
        frame.despawn_projectile(entnum);
        forget_gone_missiles(&mut frame);
        assert!(!is_remote(&mut frame, id));
        assert!(frame.ecs().resource::<RemoteMissiles>().0.is_empty());
        // A new game restarts projectile ids, so its marks must not carry over.
        mark_remote(frame.ecs(), id);
        frame.shutdown_game();
        assert!(!is_remote(&mut frame, id));
    }
}
