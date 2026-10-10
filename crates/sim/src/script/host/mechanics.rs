use super::entities::Link;
use super::rigid_body::{MapLimits, RigidBody, Sweep};
use crate::script::runtime::raise;
use crate::script::{BTreeMap, Resource, Runtime, Value};
use bevy_ecs::prelude::World;
use glam::Vec3;

/// Script-driven entity mechanics: timed moves, launched physics bodies and
/// entity/player links. They run before the VM each tick and settle native
/// collision, so a thread woken by movedone or physics_finished already
/// queries the final pose.
#[derive(Resource, Clone, Default)]
pub(crate) struct Mechanics {
    motions: BTreeMap<u64, Vec<Motion>>,
    bodies: BTreeMap<u64, Body>,
    slides: BTreeMap<u64, Slide>,
    finished: Vec<(u64, &'static str)>,
    /// Rigid bodies that went to sleep this tick, for the pose writer.
    settled: Vec<u64>,
    cost: BodyCost,
    /// Models already reported as launched without a physics preset.
    fallback_logged: std::collections::BTreeSet<String>,
    /// Models whose first launch the log has reported.
    launch_logged: std::collections::BTreeSet<String>,
    /// Where launched parts may be, found from the map on the first launch.
    limits: Option<MapLimits>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Body {
    motion: BodyMotion,
    ticks: u32,
    active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BodyMotion {
    /// A T5 destructible piece: a box that falls until it first touches.
    Piece {
        velocity: [f32; 3],
        offset: [f32; 3],
        mins: [f32; 3],
        maxs: [f32; 3],
    },
    /// A model launched by PhysicsLaunchClient/Server.
    Rigid {
        body: RigidBody,
        /// Client physics: no collision with players, never crushes.
        client: bool,
        explosive_force_scale: f32,
    },
}

/// A PhysicsLaunchClient/Server request, with the launched model's box and preset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LaunchSpec {
    pub origin: [f32; 3],
    pub angles: [f32; 3],
    pub point: [f32; 3],
    pub force: [f32; 3],
    pub mid: [f32; 3],
    pub half: [f32; 3],
    pub phys: xmodel_runtime::ModelPhys,
    pub client: bool,
}

/// How a pose writer should carry a moving body between snapshots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BodyTrajectory {
    pub velocity: [f32; 3],
    /// Euler angle rates in degrees a second.
    pub angle_rates: [f32; 3],
    /// In the air: clients add gravity to the extrapolation.
    pub falling: bool,
}

/// What the launched bodies cost from when the first woke to when the last slept.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct BodyCost {
    bodies: u32,
    ticks: u32,
    sweeps: u64,
    busiest_tick_us: u64,
    total_us: u64,
}

const TICK_S: f32 = crate::MATCH_TICK_MS as f32 / 1000.0;
const SETTLE_TICKS: u32 = 200;
const EXPLOSION_FORCE: f32 = 12500.0;
const EXPLOSION_UPBIAS: f32 = 0.5;
const EXPLOSION_MIN_FORCE: f32 = 40.0;
/// Inferred: the share of an explosion's push that also turns a body.
const EXPLOSION_SPIN_SHARE: f32 = 0.25;
/// A server body crushes only when it really came down this far in a tick.
const CRUSH_MIN_DROP: f32 = 1.0;
/// Inferred: how far under the lowest tdm spawn ground can still be valid.
/// MW2's airdrop script also judges where a crate came down against the
/// lowest tdm spawn (level.lowSpawn); 512 leaves room for any ditch a part
/// can fall into, but none for the floor under a map.
const LOWEST_GROUND_BELOW_SPAWN: f32 = 512.0;
/// The map's bounds are its solid brushes and terrain, grown by this.
const MAP_BOUNDS_MARGIN: f32 = 512.0;
/// The world's walls, floors, glass and terrain, without the clip brushes
/// that only block movement, such as a parked car's clip.
const SOLID_WORLD: u32 = crate::bullet_collision::CONTENTS_SOLID | CONTENTS_GLASS;
const CONTENTS_GLASS: u32 = 0x0000_0010;

impl Mechanics {
    pub(crate) fn start(&mut self, object: u64, motion: Motion) {
        if motion.field == "origin" {
            self.bodies.remove(&object);
            self.slides.remove(&object);
        }
        let motions = self.motions.entry(object).or_default();
        motions.retain(|m| m.field != motion.field);
        motions.push(motion);
    }

    pub(crate) fn stop(&mut self, object: u64, field: &str) {
        if let Some(motions) = self.motions.get_mut(&object) {
            motions.retain(|m| m.field != field);
        }
    }

    pub(crate) fn launch(&mut self, object: u64, spec: LaunchSpec) {
        self.motions.remove(&object);
        self.slides.remove(&object);
        let mut body = RigidBody::launch(
            Vec3::from_array(spec.origin),
            math_iw4::angles_to_axis(spec.angles),
            Vec3::from_array(spec.mid),
            Vec3::from_array(spec.half),
            super::rigid_body::Phys {
                mass: spec.phys.mass,
                bounce: spec.phys.bounce,
                friction: spec.phys.friction,
            },
            Vec3::from_array(spec.point),
            Vec3::from_array(spec.force),
        );
        if let Some(limits) = self.limits {
            body.keep_within(limits);
        }
        self.bodies.insert(
            object,
            Body {
                motion: BodyMotion::Rigid {
                    body,
                    client: spec.client,
                    explosive_force_scale: spec.phys.explosive_force_scale,
                },
                ticks: 0,
                active: true,
            },
        );
    }

    pub(crate) fn launch_piece(
        &mut self,
        object: u64,
        velocity: [f32; 3],
        offset: [f32; 3],
        half: [f32; 3],
    ) {
        self.bodies.insert(
            object,
            Body {
                motion: BodyMotion::Piece {
                    velocity,
                    offset,
                    mins: half.map(|h| -h),
                    maxs: half,
                },
                ticks: 0,
                active: true,
            },
        );
    }

    /// The map's limits for launched parts, worked out once.
    fn ensure_limits(world: &mut World) {
        if world.resource::<Mechanics>().limits.is_none() {
            let limits = map_limits(world);
            diag::info!(
                Sim,
                "physics bodies: lowest valid ground z {:.0}, bounds {:.0}..{:.0}",
                limits.lowest_ground,
                limits.lo,
                limits.hi
            );
            world.resource_mut::<Mechanics>().limits = Some(limits);
        }
    }

    /// Whether `model` is new to the no-preset report.
    pub(crate) fn note_fallback_phys(&mut self, model: &str) -> bool {
        self.fallback_logged.insert(model.to_owned())
    }

    /// Whether this is the first launch of `model`, which the log reports.
    pub(crate) fn note_first_launch(&mut self, model: &str) -> bool {
        self.launch_logged.insert(model.to_owned())
    }

    /// Rigid bodies whose pose must be sent this tick whether or not it
    /// changed: every moving one, and those that just went to sleep.
    pub(crate) fn bodies_to_present(&self) -> std::collections::BTreeSet<u64> {
        self.bodies
            .iter()
            .filter(|(_, b)| b.active && matches!(b.motion, BodyMotion::Rigid { .. }))
            .map(|(object, _)| *object)
            .chain(self.settled.iter().copied())
            .collect()
    }

    /// The trajectory a launched rigid body should be sent with, if it is one.
    /// Asleep, or calm on the ground, it is sent as stationary: a pose that
    /// stops changing is not resent, so a small leftover rate would drift on
    /// clients for good.
    pub(crate) fn body_trajectory(&self, object: u64) -> Option<BodyTrajectory> {
        let body = self.bodies.get(&object)?;
        let BodyMotion::Rigid { body: rigid, .. } = &body.motion else {
            return None;
        };
        let still = BodyTrajectory {
            velocity: [0.0; 3],
            angle_rates: [0.0; 3],
            falling: false,
        };
        if !body.active {
            return Some(still);
        }
        let slow = rigid.on_ground && rigid.velocity.length() < super::rigid_body::SLEEP_SPEED;
        let calm = rigid.on_ground && rigid.spin.length() < super::rigid_body::SLEEP_SPIN;
        let mut velocity = rigid.velocity;
        if rigid.on_ground {
            velocity.z = velocity.z.max(0.0);
        }
        Some(BodyTrajectory {
            velocity: if slow {
                still.velocity
            } else {
                velocity.to_array()
            },
            angle_rates: if calm {
                still.angle_rates
            } else {
                angle_rates(rigid)
            },
            falling: !rigid.on_ground,
        })
    }

    pub(crate) fn cancel(&mut self, object: u64) {
        self.motions.remove(&object);
        self.bodies.remove(&object);
        self.slides.remove(&object);
    }

    pub(crate) fn slide(&mut self, object: u64, slide: Slide) {
        self.stop(object, "origin");
        self.bodies.remove(&object);
        self.slides.insert(object, slide);
    }

    pub(crate) fn sliding(&self, object: u64) -> bool {
        self.slides.contains_key(&object)
    }

    pub(crate) fn stop_slide(&mut self, object: u64) {
        self.slides.remove(&object);
    }

    pub(crate) fn velocity(&self, object: u64, now: i64) -> [f32; 3] {
        if let Some(slide) = self.slides.get(&object) {
            return slide.velocity;
        }
        if let Some(body) = self.bodies.get(&object) {
            return match body.motion {
                BodyMotion::Piece { velocity, .. } => velocity,
                BodyMotion::Rigid { body, .. } => body.velocity.to_array(),
            };
        }
        self.motions
            .get(&object)
            .and_then(|motions| motions.iter().find(|m| m.field == "origin"))
            .map_or([0.0; 3], |m| m.sample(now).1)
    }

    pub(crate) fn explode(
        &mut self,
        runtime: &mut Runtime,
        center: [f32; 3],
        outer: f32,
        inner: f32,
        magnitude: f32,
    ) {
        for (object, body) in &mut self.bodies {
            if !runtime.entities.contains_key(object) {
                continue;
            }
            let at = match &body.motion {
                BodyMotion::Rigid { body, .. } => body.center.to_array(),
                BodyMotion::Piece { .. } => match runtime.object_field(*object, "origin") {
                    Value::Vector(origin) => origin,
                    _ => continue,
                },
            };
            let scale = match &body.motion {
                BodyMotion::Rigid {
                    explosive_force_scale,
                    ..
                } => *explosive_force_scale,
                BodyMotion::Piece { .. } => 1.0,
            };
            let Some(impulse) = explosion_impulse(
                Vec3::from_array(at),
                Vec3::from_array(center),
                outer,
                inner,
                magnitude,
                scale,
            ) else {
                continue;
            };
            let delta = Vec3::from_array(at) - Vec3::from_array(center);
            match &mut body.motion {
                BodyMotion::Piece { velocity, .. } => {
                    *velocity = (glam::Vec3::from_array(*velocity) + impulse).to_array();
                }
                BodyMotion::Rigid { body, .. } => {
                    // Pushed hardest on the side that faces the blast.
                    let toward = -delta.try_normalize().unwrap_or(glam::Vec3::NEG_Z);
                    let lever = toward * body.half.min_element();
                    body.push(impulse, lever, EXPLOSION_SPIN_SHARE);
                }
            }
            body.ticks = 0;
            body.active = true;
        }
    }
}

/// A PhysicsExplosionSphere's push on a body at `at`: full strength inside
/// `inner`, fading to nothing at `outer`, scaled by the body's preset, tipped
/// upward, and nothing below the minimum force.
pub(super) fn explosion_impulse(
    at: Vec3,
    center: Vec3,
    outer: f32,
    inner: f32,
    magnitude: f32,
    scale: f32,
) -> Option<Vec3> {
    if !at.is_finite() {
        return None;
    }
    let delta = at - center;
    let distance = delta.length();
    if outer <= 0.0 || distance >= outer {
        return None;
    }
    let fraction = if distance <= inner {
        1.0
    } else {
        (outer - distance) / (outer - inner)
    };
    let force = magnitude * fraction * EXPLOSION_FORCE * scale;
    if !force.is_finite() || force < EXPLOSION_MIN_FORCE {
        return None;
    }
    let mut direction = delta.try_normalize().unwrap_or(Vec3::Z);
    direction.z += EXPLOSION_UPBIAS;
    Some(direction.normalize() * force)
}

/// Whether a body that came down `drop` units this tick crushes who it
/// passed through. Client parts never do; a body that barely moved (a part
/// at rest nudged by a blast) does not either.
fn crushes(client: bool, drop: f32) -> bool {
    !client && drop > CRUSH_MIN_DROP
}

/// Euler angle rates (degrees a second) that carry `body`'s spin for a moment.
/// Near pitch +-90 yaw and roll are ill-defined and their rates blow up (a
/// look-ahead across the pole turns both by 180), so there the spin is not
/// extrapolated: the body is still sent every tick, and only holds its
/// angles between snapshots.
fn angle_rates(body: &RigidBody) -> [f32; 3] {
    const LOOK_AHEAD_S: f32 = 0.01;
    const POLE_PITCH: f32 = 80.0;
    if body.spin.length_squared() < 1e-8 {
        return [0.0; 3];
    }
    let now = math_iw4::axis_to_angles(body.axis());
    let mut ahead = *body;
    let angle = body.spin.length() * LOOK_AHEAD_S;
    ahead.rotation =
        (glam::Quat::from_axis_angle(body.spin.normalize(), angle) * body.rotation).normalize();
    let later = math_iw4::axis_to_angles(ahead.axis());
    let near_pole = |angles: [f32; 3]| math_iw4::angle_subtract(angles[0], 0.0).abs() > POLE_PITCH;
    if near_pole(now) || near_pole(later) {
        return [0.0; 3];
    }
    // No Euler rate is faster than the spin itself could make it, away from
    // the pole; anything faster is a wrap.
    let cap = 2.0 * super::rigid_body::MAX_ANG_SPEED.to_degrees();
    std::array::from_fn(|i| {
        (math_iw4::angle_subtract(later[i], now[i]) / LOOK_AHEAD_S).clamp(-cap, cap)
    })
}

/// Where launched parts may be on this map. The lowest valid ground lies
/// [`LOWEST_GROUND_BELOW_SPAWN`] under the lowest tdm spawn (any spawn point
/// if there is none); the bounds are those of the solid brushes and the
/// terrain, grown by [`MAP_BOUNDS_MARGIN`].
fn map_limits(world: &mut World) -> MapLimits {
    let lowest_spawn = {
        let mut runtime = world.resource_mut::<Runtime>();
        let spawns: Vec<(u64, bool)> = runtime
            .entities
            .iter()
            .filter_map(|(id, e)| {
                let class: &str = &e.classname;
                let tdm = class == "mp_tdm_spawn";
                (tdm || (class.starts_with("mp_") && class.contains("_spawn")))
                    .then_some((*id, tdm))
            })
            .collect();
        let (mut tdm, mut any) = (f32::INFINITY, f32::INFINITY);
        for (id, is_tdm) in spawns {
            if let Value::Vector(origin) = runtime.object_field(id, "origin") {
                if origin[2].is_finite() {
                    any = any.min(origin[2]);
                    if is_tdm {
                        tdm = tdm.min(origin[2]);
                    }
                }
            }
        }
        if tdm.is_finite() { tdm } else { any }
    };
    let frame = crate::frame::FrameWorld::from_world(world);
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for vert in &frame.clip_mesh().tables.verts {
        let v = Vec3::from_array(*vert);
        if v.is_finite() {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    for brush in frame.clip_brushes() {
        if brush.contents & crate::bullet_collision::MASK_PHYS_WORLD == 0 {
            continue;
        }
        let mut bmin = Vec3::splat(f32::NEG_INFINITY);
        let mut bmax = Vec3::splat(f32::INFINITY);
        for plane in &brush.planes {
            for axis in 0..3 {
                let along = plane[axis];
                let across = (0..3).filter(|&k| k != axis).all(|k| plane[k].abs() < 1e-4);
                if across && along > 0.9999 {
                    bmax[axis] = bmax[axis].min(plane[3]);
                } else if across && along < -0.9999 {
                    bmin[axis] = bmin[axis].max(-plane[3]);
                }
            }
        }
        if bmin.is_finite() && bmax.is_finite() && bmin.cmple(bmax).all() {
            lo = lo.min(bmin);
            hi = hi.max(bmax);
        }
    }
    let (lo, hi) = if lo.is_finite() && hi.is_finite() {
        (
            lo - Vec3::splat(MAP_BOUNDS_MARGIN),
            hi + Vec3::splat(MAP_BOUNDS_MARGIN),
        )
    } else {
        (Vec3::splat(-1.0e6), Vec3::splat(1.0e6))
    };
    let lowest_ground = if lowest_spawn.is_finite() {
        lowest_spawn - LOWEST_GROUND_BELOW_SPAWN
    } else {
        lo.z
    };
    MapLimits {
        lowest_ground,
        lo,
        hi,
    }
}

/// Inferred stand-in for a model without a usable physics preset: the common
/// car door preset (mass 25, bounce 0.1, friction 0.25, explosion scale 0.18).
const FALLBACK_PHYS: xmodel_runtime::ModelPhys = xmodel_runtime::ModelPhys {
    mass: 25.0,
    bounce: 0.1,
    friction: 0.25,
    explosive_force_scale: 0.18,
};
/// The old placeholder box, for a model without bounds.
const FALLBACK_BOX: ([f32; 3], [f32; 3]) = ([0.0, 0.0, 12.0], [12.0; 3]);

/// PhysicsLaunchClient/Server: hand the model to a rigid body pushed by
/// `force` at the world `point`. Client parts live in client physics in the
/// original, which the server's players and bullets never meet, so they are
/// made non-solid here.
pub(crate) fn physics_launch(
    world: &mut World,
    receiver: &Value,
    args: &[Value],
    client: bool,
) -> Result<Value, String> {
    let object = super::natives::engine::entity_id(world, receiver)?;
    let point = super::args::vector(args, 0)?;
    let force = super::args::vector(args, 1)?;
    if point.iter().chain(&force).any(|v| !v.is_finite()) {
        return Err("physics launch requires a finite point and force".into());
    }
    Mechanics::ensure_limits(world);
    let (model, origin, angles) = {
        let mut runtime = world.resource_mut::<Runtime>();
        let model = match runtime.object_field(object, "model") {
            Value::String(model) => model.to_string(),
            _ => String::new(),
        };
        let vector = |runtime: &mut Runtime, name| match runtime.object_field(object, name) {
            Value::Vector(v) => v,
            _ => [0.0; 3],
        };
        let origin = vector(&mut runtime, "origin");
        let angles = vector(&mut runtime, "angles");
        (model, origin, angles)
    };
    let capability = crate::frame::FrameWorld::from_world(world)
        .model_capability(&model)
        .flatten();
    let (mid, half) = capability
        .as_ref()
        .and_then(|c| c.bounds)
        .unwrap_or(FALLBACK_BOX);
    let phys = match capability.as_ref().and_then(|c| c.phys) {
        Some(phys) => phys,
        None => {
            if world.resource_mut::<Mechanics>().note_fallback_phys(&model) {
                diag::warn!(
                    Sim,
                    "physics launch: model `{model}` has no physics preset with a mass; using the car door preset"
                );
            }
            FALLBACK_PHYS
        }
    };
    if world.resource_mut::<Mechanics>().note_first_launch(&model) {
        diag::info!(
            Sim,
            "physics launch: first {} `{model}`: mass {} bounce {} friction {} explosion scale {}, force {:.0}",
            if client { "client" } else { "server" },
            phys.mass,
            phys.bounce,
            phys.friction,
            phys.explosive_force_scale,
            glam::Vec3::from_array(force).length(),
        );
    }
    world.resource_mut::<Mechanics>().launch(
        object,
        LaunchSpec {
            origin,
            angles,
            point,
            force,
            mid,
            half,
            phys,
            client,
        },
    );
    let mut runtime = world.resource_mut::<Runtime>();
    let entity = runtime.entities.get_mut(&object).unwrap();
    entity.linked_to = None;
    if client {
        entity.solid = false;
    }
    Ok(Value::Undefined)
}

pub(crate) fn advance_mechanics(world: &mut World) {
    let request = world.resource::<crate::step::StepRequest>();
    if !request.reason.advances_authority_world() {
        return;
    }
    let now = i64::from(request.tick.0) * i64::from(crate::MATCH_TICK_MS);
    let runtime = world.resource::<Runtime>();
    if runtime.fault.is_some() || runtime.program.is_none() {
        return;
    }
    advance_motions(world, now);
    advance_bodies(world);
    advance_slides(world);
    apply_entity_links(world);
    super::players::apply_player_links(world);
    super::presence::settle_collision(world);
}

pub(crate) fn deliver_finished(world: &mut World) {
    let finished = std::mem::take(&mut world.resource_mut::<Mechanics>().finished);
    for (object, name) in finished {
        raise(world, Value::Object(object), name, Vec::new());
    }
}

fn advance_motions(world: &mut World, now: i64) {
    world.resource_scope::<Mechanics, _>(|world, mut mechanics| {
        let mut runtime = world.resource_mut::<Runtime>();
        let Mechanics {
            motions, finished, ..
        } = &mut *mechanics;
        motions.retain(|object, _| runtime.entities.contains_key(object));
        for (object, list) in motions.iter_mut() {
            list.retain(|motion| {
                let (value, _) = motion.sample(now);
                runtime.set_object_field(*object, motion.field, Value::Vector(value));
                let done = now - motion.start_ms >= motion.duration_ms;
                if done {
                    finished.push((*object, motion.done));
                }
                !done
            });
        }
        motions.retain(|_, list| !list.is_empty());
    });
}

fn advance_bodies(world: &mut World) {
    let mut crushed = Vec::new();
    world.resource_scope::<Mechanics, _>(|world, mut mechanics| {
        let Mechanics {
            bodies,
            finished,
            settled,
            cost,
            ..
        } = &mut *mechanics;
        settled.clear();
        let started = std::time::Instant::now();
        let mut awake = 0u32;
        let mut sweeps = 0u64;
        bodies.retain(|object, body| {
            if !world.resource::<Runtime>().entities.contains_key(object) {
                return false;
            }
            if !body.active {
                return true;
            }
            match body.motion {
                BodyMotion::Piece { .. } => advance_piece(world, *object, body, finished, &mut crushed),
                BodyMotion::Rigid { .. } => {
                    awake += 1;
                    sweeps += u64::from(advance_rigid(world, *object, body, finished, &mut crushed));
                    if !body.active {
                        settled.push(*object);
                    }
                }
            }
            true
        });
        if awake > 0 {
            let spent = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            cost.bodies = cost.bodies.max(awake);
            cost.ticks += 1;
            cost.sweeps += sweeps;
            cost.total_us += spent;
            cost.busiest_tick_us = cost.busiest_tick_us.max(spent);
            let still_awake = bodies
                .values()
                .any(|b| b.active && matches!(b.motion, BodyMotion::Rigid { .. }));
            if !still_awake {
                diag::info!(
                    Sim,
                    "physics bodies: up to {} launched parts settled over {} ticks: {} sweeps, {} us in all, busiest tick {} us",
                    cost.bodies,
                    cost.ticks,
                    cost.sweeps,
                    cost.total_us,
                    cost.busiest_tick_us
                );
                *cost = BodyCost::default();
            }
        }
    });
    for (victim, pusher, point) in crushed {
        super::players::crush_player(world, victim, pusher, point);
    }
}

type Crushed = Vec<(crate::ClientId, u64, [f32; 3])>;

fn advance_piece(
    world: &mut World,
    object: u64,
    body: &mut Body,
    finished: &mut Vec<(u64, &'static str)>,
    crushed: &mut Crushed,
) {
    let BodyMotion::Piece {
        velocity,
        offset,
        mins,
        maxs,
    } = &mut body.motion
    else {
        return;
    };
    let (origin, angles) = {
        let mut runtime = world.resource_mut::<Runtime>();
        match (
            runtime.object_field(object, "origin"),
            runtime.object_field(object, "angles"),
        ) {
            (Value::Vector(o), Value::Vector(a)) => (o, a),
            (Value::Vector(o), _) => (o, [0.0; 3]),
            _ => return,
        }
    };
    velocity[2] -= GRAVITY * TICK_S;
    let start: [f32; 3] = std::array::from_fn(|i| origin[i] + offset[i]);
    let end: [f32; 3] = std::array::from_fn(|i| start[i] + velocity[i] * TICK_S);
    let trace = crate::frame::FrameWorld::from_world(world).trace_static_world(
        start,
        end,
        *mins,
        *maxs,
        crate::bullet_collision::MASK_PHYS_WORLD,
    );
    let fraction = if trace.startsolid != 0 {
        0.0
    } else {
        trace.fraction
    };
    let at: [f32; 3] = std::array::from_fn(|i| origin[i] + velocity[i] * TICK_S * fraction);
    if crushes(false, origin[2] - at[2]) {
        crushed.extend(
            crush_victims(world, object, origin, at, *mins, *maxs)
                .into_iter()
                .map(|(victim, point)| (victim, object, point)),
        );
    }
    body.ticks += 1;
    let rested = fraction < 1.0 || body.ticks >= SETTLE_TICKS;
    let mut runtime = world.resource_mut::<Runtime>();
    runtime.set_object_field(object, "origin", Value::Vector(at));
    if rested {
        runtime.set_object_field(object, "angles", Value::Vector([0.0, angles[1], 0.0]));
        finished.push((object, "physics_finished"));
        *velocity = [0.0; 3];
        body.active = false;
    }
}

/// One tick of a launched rigid body; returns the sweeps it took.
fn advance_rigid(
    world: &mut World,
    object: u64,
    body: &mut Body,
    finished: &mut Vec<(u64, &'static str)>,
    crushed: &mut Crushed,
) -> u32 {
    let BodyMotion::Rigid {
        body: rigid,
        client,
        ..
    } = &mut body.motion
    else {
        return 0;
    };
    let before = (rigid.origin(), rigid.center);
    body.ticks += 1;
    let (report, asleep) = {
        let frame = crate::frame::FrameWorld::from_world(world);
        let mut sweep = |start: Vec3, end: Vec3, half: Vec3| {
            Sweep::from_trace(&frame.trace_static_world(
                start.to_array(),
                end.to_array(),
                (-half).to_array(),
                half.to_array(),
                crate::bullet_collision::MASK_PHYS_WORLD,
            ))
        };
        let mut solid = |start: Vec3, end: Vec3, half: Vec3| {
            Sweep::from_trace(&frame.trace_static_world(
                start.to_array(),
                end.to_array(),
                (-half).to_array(),
                half.to_array(),
                SOLID_WORLD,
            ))
        };
        let report = rigid.step(TICK_S, &mut sweep, &mut solid);
        let asleep = rigid.ready_to_sleep() || body.ticks >= SETTLE_TICKS;
        if asleep {
            rigid.settle(&mut sweep);
        }
        (report, asleep)
    };
    let origin = rigid.origin();
    if crushes(*client, before.1.z - rigid.center.z) {
        let half = rigid.world_half();
        let offset = rigid.center - origin;
        crushed.extend(
            crush_victims(
                world,
                object,
                before.0.to_array(),
                origin.to_array(),
                (offset - half).to_array(),
                (offset + half).to_array(),
            )
            .into_iter()
            .map(|(victim, point)| (victim, object, point)),
        );
    }
    let angles = math_iw4::axis_to_angles(rigid.axis());
    let mut runtime = world.resource_mut::<Runtime>();
    runtime.set_object_field(object, "origin", Value::Vector(origin.to_array()));
    runtime.set_object_field(object, "angles", Value::Vector(angles));
    if asleep {
        finished.push((object, "physics_finished"));
        body.active = false;
    }
    report.sweeps
}

/// Live players a body sweeping from `origin` to `at` passes through. The box
/// is the linked brush's, if the body carries one, else `mins..maxs` around
/// the origin.
fn crush_victims(
    world: &mut World,
    object: u64,
    origin: [f32; 3],
    at: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
) -> Vec<(crate::ClientId, [f32; 3])> {
    let presence = world.resource::<Runtime>().entities[&object].presence;
    let frame = crate::frame::FrameWorld::from_world(world);
    let (mins, maxs) = presence
        .and_then(|id| {
            let row = frame
                .entity_collision_capabilities()
                .iter()
                .find(|row| row.owner.script_model() == Some(id))?;
            let brush = row.linked_brushes.first()?;
            let model = frame
                .clip_cmodels()
                .models
                .get(brush.cmodel_handle as usize)?;
            let reach = (0..2)
                .map(|i| model.mins[i].abs().max(model.maxs[i].abs()))
                .fold(0.0f32, f32::max);
            let offset: [f32; 3] = std::array::from_fn(|i| brush.origin[i] - origin[i]);
            Some((
                [
                    offset[0] - reach,
                    offset[1] - reach,
                    offset[2] + model.mins[2],
                ],
                [
                    offset[0] + reach,
                    offset[1] + reach,
                    offset[2] + model.maxs[2],
                ],
            ))
        })
        .unwrap_or((mins, maxs));
    let (pmins, pmaxs) = (
        crate::bullet_collision::PLAYER_MINS,
        crate::bullet_collision::PLAYER_MAXS,
    );
    let mut victims = Vec::new();
    frame.visit_players(|id, ps| {
        if ps.pm_type >= playerstate_iw4::PM_TYPE_DEAD {
            return;
        }
        let lo = Vec3::from_array(std::array::from_fn(|i| ps.origin[i] + pmins[i]));
        let hi = Vec3::from_array(std::array::from_fn(|i| ps.origin[i] + pmaxs[i]));
        if super::rigid_body::swept_box_overlaps(
            Vec3::from_array(origin),
            Vec3::from_array(at),
            Vec3::from_array(mins),
            Vec3::from_array(maxs),
            lo,
            hi,
        ) {
            victims.push((id, ps.origin));
        }
    });
    victims
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Slide {
    pub center: [f32; 3],
    pub radius: f32,
    pub velocity: [f32; 3],
}

pub(crate) fn load_slide_field(world: &World, object: u64, name: &str) -> Option<Value> {
    (name == "slidevelocity" && world.resource::<Runtime>().entities.contains_key(&object)).then(
        || {
            world
                .resource::<Mechanics>()
                .slides
                .get(&object)
                .map_or(Value::Vector([0.0; 3]), |slide| {
                    Value::Vector(slide.velocity)
                })
        },
    )
}

pub(crate) fn store_slide_field(
    world: &mut World,
    object: u64,
    name: &str,
    value: &Value,
) -> Result<bool, String> {
    if name != "slidevelocity" || !world.resource::<Runtime>().entities.contains_key(&object) {
        return Ok(false);
    }
    let Value::Vector(velocity) = value else {
        return Err("slideVelocity requires a vector".into());
    };
    if velocity.iter().any(|v| !v.is_finite()) {
        return Err("slideVelocity must be finite".into());
    }
    let mut mechanics = world.resource_mut::<Mechanics>();
    let slide = mechanics
        .slides
        .get_mut(&object)
        .ok_or("cannot set slideVelocity on an entity that is not sliding")?;
    slide.velocity = *velocity;
    Ok(true)
}

fn advance_slides(world: &mut World) {
    world.resource_scope::<Mechanics, _>(|world, mut mechanics| {
        mechanics.slides.retain(|object, slide| {
            let origin = {
                let mut runtime = world.resource_mut::<Runtime>();
                if !runtime.entities.contains_key(object) {
                    return false;
                }
                let Value::Vector(origin) = runtime.object_field(*object, "origin") else {
                    return false;
                };
                origin
            };
            if origin.iter().any(|v| !v.is_finite()) {
                return false;
            }
            super::presence::settle_collision(world);
            let at = crate::step::script_slide(world, *object, origin, slide);
            world
                .resource_mut::<Runtime>()
                .set_object_field(*object, "origin", Value::Vector(at));
            true
        });
    });
}

impl Slide {
    pub(crate) fn advance<C: movement_iw4::CollisionBackend>(
        &mut self,
        origin: [f32; 3],
        collision: &C,
        mask: u32,
    ) -> [f32; 3] {
        use movement_iw4::GroundTraceInput;
        let mins = self.center.map(|v| v - self.radius);
        let maxs = self.center.map(|v| v + self.radius);
        let trace = |start, end| {
            collision.trace(GroundTraceInput {
                start,
                end,
                mins,
                maxs,
                tracemask: mask,
            })
        };
        let start_velocity = self.velocity;
        let mut at = origin;
        if !self.sweep(&mut at, collision, mask, mins, maxs) {
            return at;
        }
        let down = trace(origin, [origin[0], origin[1], origin[2] - 18.0]);
        if start_velocity[2] > 0.0 && (down.fraction == 1.0 || down.normal[2] < 0.7) {
            return at;
        }
        let up = trace(origin, [origin[0], origin[1], origin[2] + 18.0]);
        if up.startsolid != 0 {
            return at;
        }
        at = up.endpos;
        let elevation = at[2] - origin[2];
        self.velocity = start_velocity;
        self.sweep(&mut at, collision, mask, mins, maxs);
        let down = trace(at, [at[0], at[1], at[2] - elevation]);
        if down.startsolid == 0 {
            at = down.endpos;
        }
        if down.fraction < 1.0 {
            self.velocity = clip_slide(self.velocity, down.normal);
        }
        at
    }

    fn sweep<C: movement_iw4::CollisionBackend>(
        &mut self,
        origin: &mut [f32; 3],
        collision: &C,
        mask: u32,
        mins: [f32; 3],
        maxs: [f32; 3],
    ) -> bool {
        let mut end_velocity = self.velocity;
        end_velocity[2] -= GRAVITY * TICK_S;
        self.velocity[2] = (self.velocity[2] + end_velocity[2]) * 0.5;
        let mut planes = [[0.0; 3]; 5];
        planes[0] = glam::Vec3::from_array(self.velocity)
            .normalize_or_zero()
            .to_array();
        let mut plane_count = 1;
        let mut time = TICK_S;
        let mut bumped = false;
        for _ in 0..4 {
            let end = std::array::from_fn(|i| origin[i] + self.velocity[i] * time);
            if end.iter().any(|v: &f32| !v.is_finite()) {
                self.velocity = [0.0; 3];
                return true;
            }
            let hit = collision.trace(movement_iw4::GroundTraceInput {
                start: *origin,
                end,
                mins,
                maxs,
                tracemask: mask,
            });
            if hit.allsolid != 0 {
                self.velocity[2] = 0.0;
                return true;
            }
            if hit.fraction > 0.0 {
                *origin = hit.endpos;
            }
            if hit.fraction == 1.0 {
                break;
            }
            bumped = true;
            time *= 1.0 - hit.fraction;
            if plane_count == planes.len() {
                self.velocity = [0.0; 3];
                return true;
            }
            if planes[..plane_count]
                .iter()
                .any(|p| slide_dot(hit.normal, *p) > 0.99)
            {
                self.velocity = std::array::from_fn(|i| self.velocity[i] + hit.normal[i]);
                continue;
            }
            planes[plane_count] = hit.normal;
            plane_count += 1;
            for i in 0..plane_count {
                if slide_dot(self.velocity, planes[i]) >= 0.1 {
                    continue;
                }
                let mut velocity = clip_slide(self.velocity, planes[i]);
                let mut final_velocity = clip_slide(end_velocity, planes[i]);
                for j in 0..plane_count {
                    if i == j || slide_dot(velocity, planes[j]) >= 0.1 {
                        continue;
                    }
                    velocity = clip_slide(velocity, planes[j]);
                    final_velocity = clip_slide(final_velocity, planes[j]);
                    if slide_dot(velocity, planes[i]) >= 0.0 {
                        continue;
                    }
                    let crease = glam::Vec3::from_array(planes[i])
                        .cross(glam::Vec3::from_array(planes[j]))
                        .normalize_or_zero();
                    velocity =
                        (crease * crease.dot(glam::Vec3::from_array(self.velocity))).to_array();
                    final_velocity =
                        (crease * crease.dot(glam::Vec3::from_array(end_velocity))).to_array();
                    if planes[..plane_count]
                        .iter()
                        .enumerate()
                        .any(|(k, p)| k != i && k != j && slide_dot(velocity, *p) < 0.1)
                    {
                        self.velocity = [0.0; 3];
                        return true;
                    }
                }
                self.velocity = velocity;
                end_velocity = final_velocity;
                break;
            }
        }
        self.velocity = end_velocity;
        bumped
    }
}

fn slide_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn clip_slide(velocity: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let dot = slide_dot(velocity, normal);
    let backoff = -(dot - dot.abs() * (1.001 - 1.0));
    std::array::from_fn(|i| velocity[i] + backoff * normal[i])
}

fn apply_entity_links(world: &mut World) {
    let mut runtime = world.resource_mut::<Runtime>();
    let linked: Vec<(u64, Link)> = runtime
        .entities
        .iter()
        .filter_map(|(id, e)| Some((*id, e.linked_to.clone()?)))
        .collect();
    for (id, link) in linked {
        if !runtime.live(&link.parent) {
            runtime.entities.get_mut(&id).unwrap().linked_to = None;
            continue;
        }
        let field = |runtime: &mut Runtime, name| match runtime.object_field(link.parent, name) {
            Value::Vector(v) => v,
            _ => [0.0; 3],
        };
        let (base, base_angles) = (field(&mut runtime, "origin"), field(&mut runtime, "angles"));
        let axis = math_iw4::angles_to_axis(base_angles);
        let offset = link.tag_offset.unwrap_or([0.0; 3]);
        let local = std::array::from_fn(|i| offset[i] + link.origin[i]);
        let (child_axis, origin) =
            math_iw4::matrix_multiply43(math_iw4::angles_to_axis(link.angles), local, axis, base);
        runtime.set_object_field(id, "origin", Value::Vector(origin));
        runtime.set_object_field(
            id,
            "angles",
            Value::Vector(math_iw4::axis_to_angles(child_axis)),
        );
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Motion {
    pub field: &'static str,
    pub path: MotionPath,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub done: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum MotionPath {
    Linear {
        from: [f32; 3],
        to: [f32; 3],
        accel: f32,
        decel: f32,
    },
    Ballistic {
        from: [f32; 3],
        velocity: [f32; 3],
    },
}

const GRAVITY: f32 = 800.0;

impl Motion {
    pub(crate) fn sample(&self, now: i64) -> ([f32; 3], [f32; 3]) {
        let elapsed = (now - self.start_ms).clamp(0, self.duration_ms);
        let t = elapsed as f32 / 1000.0;
        match self.path {
            MotionPath::Linear {
                from,
                to,
                accel,
                decel,
            } => {
                let total = self.duration_ms as f32 / 1000.0;
                let peak = 2.0 / (2.0 * total - accel - decel);
                let cruise_end = total - decel;
                let (fraction, rate) = if t < accel {
                    (0.5 * peak * t * t / accel, peak * t / accel)
                } else if t <= cruise_end {
                    (0.5 * peak * accel + peak * (t - accel), peak)
                } else {
                    let u = t - cruise_end;
                    (
                        0.5 * peak * accel + peak * (cruise_end - accel) + peak * u
                            - 0.5 * peak * u * u / decel,
                        peak * (1.0 - u / decel),
                    )
                };
                let fraction = if elapsed >= self.duration_ms {
                    1.0
                } else {
                    fraction
                };
                let rate = if elapsed >= self.duration_ms {
                    0.0
                } else {
                    rate
                };
                (
                    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * fraction),
                    std::array::from_fn(|i| (to[i] - from[i]) * rate),
                )
            }
            MotionPath::Ballistic { from, velocity } => {
                let mut p: [f32; 3] = std::array::from_fn(|i| from[i] + velocity[i] * t);
                p[2] -= 0.5 * GRAVITY * t * t;
                let mut v = velocity;
                v[2] -= GRAVITY * t;
                (p, v)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOOR: xmodel_runtime::ModelPhys = xmodel_runtime::ModelPhys {
        mass: 25.0,
        bounce: 0.1,
        friction: 0.25,
        explosive_force_scale: 0.18,
    };

    fn launched(mechanics: &mut Mechanics, object: u64, client: bool) {
        mechanics.launch(
            object,
            LaunchSpec {
                origin: [0.0, 0.0, 28.0],
                angles: [0.0, 62.0, 0.0],
                point: [0.0, 0.0, 28.0],
                force: [1500.0, 4000.0, -1800.0],
                mid: [-19.67, -3.56, 7.48],
                half: [19.34, 5.04, 23.03],
                phys: DOOR,
                client,
            },
        );
    }

    fn rigid(mechanics: &mut Mechanics, object: u64) -> &mut RigidBody {
        match &mut mechanics.bodies.get_mut(&object).unwrap().motion {
            BodyMotion::Rigid { body, .. } => body,
            BodyMotion::Piece { .. } => panic!("not a rigid body"),
        }
    }

    #[test]
    fn a_launched_part_starts_at_force_over_mass() {
        let mut mechanics = Mechanics::default();
        launched(&mut mechanics, 7, true);
        let v = Vec3::from_array(mechanics.velocity(7, 0));
        let expected = Vec3::new(1500.0, 4000.0, -1800.0) / 25.0;
        assert!((v - expected).length() < 1e-3);
    }

    #[test]
    fn the_pose_writer_follows_the_body_and_stops_when_it_sleeps() {
        let mut mechanics = Mechanics::default();
        launched(&mut mechanics, 7, true);
        let flying = mechanics.body_trajectory(7).unwrap();
        assert!(flying.falling, "in the air the extrapolation adds gravity");
        assert_ne!(flying.angle_rates, [0.0; 3], "a hinge launch spins");
        // The tick a bounce happens the writer carries the velocity after it.
        let body = rigid(&mut mechanics, 7);
        body.on_ground = true;
        body.velocity = Vec3::new(120.0, 40.0, 6.0);
        let bounced = mechanics.body_trajectory(7).unwrap();
        assert!(!bounced.falling);
        assert_eq!(bounced.velocity, [120.0, 40.0, 6.0]);
        let body = rigid(&mut mechanics, 7);
        body.velocity = Vec3::new(1.0, 0.5, -3.0);
        body.spin = Vec3::ZERO;
        let calm = mechanics.body_trajectory(7).unwrap();
        assert_eq!((calm.velocity, calm.angle_rates), ([0.0; 3], [0.0; 3]));
        mechanics.bodies.get_mut(&7).unwrap().active = false;
        let asleep = mechanics.body_trajectory(7).unwrap();
        assert_eq!(
            (asleep.velocity, asleep.angle_rates, asleep.falling),
            ([0.0; 3], [0.0; 3], false)
        );
        mechanics.launch_piece(8, [0.0; 3], [0.0; 3], [4.0; 3]);
        assert!(mechanics.body_trajectory(8).is_none());
    }

    #[test]
    fn spin_is_sent_as_angle_rates_except_near_the_pole() {
        let mut mechanics = Mechanics::default();
        launched(&mut mechanics, 7, true);
        let body = rigid(&mut mechanics, 7);
        body.rotation = glam::Quat::IDENTITY;
        body.spin = Vec3::new(0.0, 0.0, 2.0);
        let level = angle_rates(body);
        assert!((level[1] - 2.0f32.to_degrees()).abs() < 1.0, "{level:?}");
        // Pitched 85 degrees down, a turn about the world's up axis is all
        // yaw and roll, which the look-ahead can flip by 180 each.
        body.rotation = glam::Quat::from_rotation_y(85.0f32.to_radians());
        body.spin = Vec3::new(3.0, 0.0, 8.0);
        assert_eq!(angle_rates(body), [0.0; 3]);
        body.rotation = glam::Quat::from_rotation_y(60.0f32.to_radians());
        let cap = 2.0 * super::super::rigid_body::MAX_ANG_SPEED.to_degrees();
        assert!(angle_rates(body).iter().all(|r| r.abs() <= cap));
    }

    #[test]
    fn a_moving_body_is_sent_every_tick_and_once_more_when_it_sleeps() {
        let mut mechanics = Mechanics::default();
        launched(&mut mechanics, 7, true);
        mechanics.launch_piece(8, [0.0; 3], [0.0; 3], [4.0; 3]);
        assert!(mechanics.bodies_to_present().contains(&7));
        assert!(!mechanics.bodies_to_present().contains(&8));
        mechanics.bodies.get_mut(&7).unwrap().active = false;
        mechanics.settled.push(7);
        assert!(mechanics.bodies_to_present().contains(&7), "the sleep tick");
        mechanics.settled.clear();
        assert!(mechanics.bodies_to_present().is_empty(), "then left alone");
    }

    #[test]
    fn an_explosion_push_scales_by_preset_and_fades_with_distance() {
        let center = Vec3::new(0.0, 0.0, 80.0);
        let near =
            explosion_impulse(Vec3::new(30.0, 0.0, 10.0), center, 247.5, 0.0, 2.5, 0.18).unwrap();
        let far =
            explosion_impulse(Vec3::new(200.0, 0.0, 10.0), center, 247.5, 0.0, 2.5, 0.18).unwrap();
        let full = 2.5 * EXPLOSION_FORCE * 0.18;
        assert!(near.length() < full && near.length() > far.length());
        assert!(
            explosion_impulse(Vec3::new(300.0, 0.0, 0.0), center, 247.5, 0.0, 2.5, 0.18).is_none()
        );
        assert!(
            explosion_impulse(Vec3::new(246.5, 0.0, 80.0), center, 247.5, 0.0, 2.5, 0.18).is_none(),
            "below the minimum force"
        );
        let mut body = RigidBody::launch(
            Vec3::ZERO,
            math_iw4::angles_to_axis([0.0; 3]),
            Vec3::ZERO,
            Vec3::splat(10.0),
            super::super::rigid_body::Phys {
                mass: 25.0,
                bounce: 0.1,
                friction: 0.25,
            },
            Vec3::ZERO,
            Vec3::ZERO,
        );
        body.push(near, Vec3::ZERO, EXPLOSION_SPIN_SHARE);
        assert!((body.velocity - near / 25.0).length() < 1e-3);
    }

    #[test]
    fn only_a_server_body_that_really_came_down_crushes() {
        assert!(!crushes(true, 40.0), "client parts never crush");
        assert!(crushes(false, 40.0), "a falling care package crushes");
        assert!(
            !crushes(false, 0.6),
            "a resting body nudged by a blast does not"
        );
        assert!(!crushes(false, -30.0), "a body thrown upward does not");
    }
}
