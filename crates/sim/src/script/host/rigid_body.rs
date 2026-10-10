//! A launched part (a car door, a hood, a care package) as a small rigid body:
//! an oriented box with the mass, bounce and friction of its model's physics
//! preset. It falls, spins, bounces off the world, tips over and settles on top
//! of the ground. The step is a pure function over a sweep closure, so it runs
//! against the static world in the game and against built worlds in tests.
//!
//! Collision sweeps the world box that encloses the rotated box. The game's
//! world trace turns that box into an upright capsule whose bottom is the box's
//! bottom, so against a floor the sweep stops exactly at the body's lowest
//! corner. The contact response is an impulse at the corners that touch, so a
//! part landing on an edge tips over and a part lying on a face stays put.
//!
//! Terrain is one-sided triangles: a capsule that gets more than its radius
//! under one never meets it again. So nothing here ever moves a body downward
//! while it overlaps the world, and a turn that lowers its bottom is first
//! checked against what is under it.

use glam::{Mat3, Quat, Vec3};

/// Simulated time per sweep: two per 50 ms server tick.
pub(crate) const SUBSTEP_S: f32 = 0.025;
const GRAVITY: f32 = 800.0;
const CONTACT_SLOP: f32 = 0.125;
const GROUND_NZ: f32 = 0.7;
pub(crate) const SLEEP_SPEED: f32 = 4.0;
pub(crate) const SLEEP_SPIN: f32 = 20.0 * std::f32::consts::PI / 180.0;
const SLEEP_S: f32 = 0.2;
/// Highest a body is lifted in one go to free it from the world.
const UNSTICK_MAX_LIFT: f32 = 16.0;
/// Inferred: the launch vector is an impulse, so speed = force * scale / mass.
pub(crate) const LAUNCH_FORCE_SCALE: f32 = 1.0;
/// Inferred cap on spin, in rad/s (about 860 degrees a second).
pub(crate) const MAX_ANG_SPEED: f32 = 15.0;
/// Inferred guard for light presets: no launch, blast or contact leaves a
/// part faster than this. With the real presets a mirror (bucket_metal,
/// mass 6) would leave at up to 1,500 u/s and land 2,500 units away, and
/// the coupe mirrors' can_metal (mass 0.1) faster still; doors and hoods
/// stay under it.
const MAX_SPEED: f32 = 800.0;
/// Fastest a part falls: a care package dropped from the helicopter is
/// well under it; a part that has left the world is not.
const MAX_FALL_SPEED: f32 = 3000.0;
/// Longest a part spawned inside the world (its own car's clip) takes to fly
/// out of it; after that it is moved to the nearest clear place.
const ESCAPE_MAX_S: f32 = 0.5;
/// A body this far below where it was launched has left the world: it goes
/// back to the last place it was clear of it and stops.
const FALL_LIMIT: f32 = 4096.0;
/// A resting part this close to lying on a face is laid flat on it.
const FACE_SNAP_COS: f32 = 0.965_925_8; // cos(15 degrees)
/// Corners within this distance of the deepest one share the contact.
const CONTACT_PLANE_TOL: f32 = 1.0;
/// Below this approach speed a contact does not bounce, so resting parts calm down.
const REST_SPEED: f32 = 2.0 * GRAVITY * SUBSTEP_S;
/// Spin kept per ground contact: rolling resistance, so a part stops rocking.
const GROUND_SPIN_KEEP: f32 = 0.9;
/// Below these speeds a part on the ground is settling, and loses this share
/// of its motion each substep, so it comes to rest instead of creeping.
const SETTLE_SPEED: f32 = 20.0;
const SETTLE_SPIN: f32 = 1.5;
const SETTLE_KEEP: f32 = 0.7;
/// A part resting within 15 degrees of a face turns onto it over about this
/// long (inferred), rather than rocking on the corners.
const LEVEL_S: f32 = 0.1;
/// A face a body can lie on has its narrower side at least this share of the
/// height of the centre above it (tipping angle about 27 degrees).
const BROAD_FACE: f32 = 0.5;
/// How fast a part balanced on an edge or a narrow face is tipped off it
/// (inferred).
const TIP_RATE: f32 = 3.0;
/// Inferred, as MW2's physics also stops bodies that stay busy too long: a
/// part still tumbling on the ground this long after its last push settles.
const LATE_SETTLE_S: f32 = 3.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Phys {
    pub mass: f32,
    pub bounce: f32,
    pub friction: f32,
}

/// What a sweep of an axis-aligned box from `start` to `end` found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sweep {
    pub fraction: f32,
    pub end: Vec3,
    pub normal: Vec3,
    /// The box starts inside the world. Against brushes moving out of it is
    /// not blocked; against terrain a move into a triangle it overlaps is.
    pub start_solid: bool,
}

impl Sweep {
    pub(crate) fn from_trace(trace: &trace_iw4::Trace) -> Self {
        Self {
            fraction: trace.fraction,
            end: Vec3::from_array(trace.endpos),
            normal: Vec3::from_array(trace.normal),
            start_solid: trace.startsolid != 0 || trace.allsolid != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RigidBody {
    /// World position of the box centre, which is the centre of mass.
    pub center: Vec3,
    /// Model to world.
    pub rotation: Quat,
    pub velocity: Vec3,
    /// World angular velocity in rad/s.
    pub spin: Vec3,
    /// Box centre in model space.
    pub mid: Vec3,
    /// Box half extents in model space.
    pub half: Vec3,
    pub phys: Phys,
    pub on_ground: bool,
    /// How long the body has been slow and on the ground.
    pub calm_s: f32,
    /// It has been clear of the world since it was launched.
    pub free: bool,
    /// How long it has spent flying out of the world it was spawned in.
    pub escape_s: f32,
    /// The last centre and orientation it was found clear of the world in.
    pub last_free: (Vec3, Quat),
    pub launch_z: f32,
    /// How long since it was launched or last pushed.
    pub awake_s: f32,
    /// It fell out of the world, or could not get out of the one it was
    /// spawned in, and was stopped for good.
    pub lost: bool,
}

/// What one step cost, for the settle report in the log.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct StepReport {
    pub sweeps: u32,
}

impl RigidBody {
    /// A body for a model at `origin`/`axis`, pushed by `force` at `point`.
    /// A point outside the body (a care package is launched from the world
    /// origin) pushes it without spin.
    pub(crate) fn launch(
        origin: Vec3,
        axis: [[f32; 3]; 3],
        mid: Vec3,
        half: Vec3,
        phys: Phys,
        point: Vec3,
        force: Vec3,
    ) -> Self {
        let rotation = rotation_from_axis(axis);
        let half = half.abs().max(Vec3::splat(0.5));
        let mut body = Self {
            center: origin + rotation * mid,
            rotation,
            velocity: Vec3::ZERO,
            spin: Vec3::ZERO,
            mid,
            half,
            phys: Phys {
                mass: phys.mass.max(0.001),
                bounce: phys.bounce.clamp(0.0, 1.0),
                friction: phys.friction.clamp(0.0, 1.0),
            },
            on_ground: false,
            calm_s: 0.0,
            free: false,
            escape_s: 0.0,
            last_free: (origin + rotation * mid, rotation),
            launch_z: origin.z,
            awake_s: 0.0,
            lost: false,
        };
        let impulse = force * LAUNCH_FORCE_SCALE;
        let lever = point - body.center;
        let lever = if lever.length() <= body.half.length() + CONTACT_PLANE_TOL {
            lever
        } else {
            Vec3::ZERO
        };
        body.apply_impulse(impulse, lever);
        body.clamp_spin();
        body
    }

    pub(crate) fn origin(&self) -> Vec3 {
        self.center - self.rotation * self.mid
    }

    pub(crate) fn axis(&self) -> [[f32; 3]; 3] {
        let m = Mat3::from_quat(self.rotation);
        [
            m.x_axis.to_array(),
            m.y_axis.to_array(),
            m.z_axis.to_array(),
        ]
    }

    /// Half extents of the world box around the rotated body.
    pub(crate) fn world_half(&self) -> Vec3 {
        world_half(self.rotation, self.half)
    }

    /// An explosion's push: `impulse` through the centre, and `spin_share` of
    /// it applied at `lever` from the centre to turn the body.
    pub(crate) fn push(&mut self, impulse: Vec3, lever: Vec3, spin_share: f32) {
        self.velocity += impulse / self.phys.mass;
        self.spin += self.inverse_inertia_world() * lever.cross(impulse * spin_share);
        self.clamp_spin();
        self.calm_s = 0.0;
        self.awake_s = 0.0;
        self.on_ground = false;
    }

    /// Advances `dt` seconds in whole substeps. `sweep` meets the whole
    /// static world; `terrain` only its terrain triangles, which a body
    /// still inside a brush must not pass under.
    pub(crate) fn step(
        &mut self,
        dt: f32,
        sweep: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        terrain: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    ) -> StepReport {
        let mut report = StepReport::default();
        let steps = (dt / SUBSTEP_S).round().max(1.0) as u32;
        let sub = dt / steps as f32;
        for _ in 0..steps {
            let before = *self;
            self.substep(sub, sweep, terrain, &mut report);
            if !self.is_finite() {
                // Never hand a broken pose to the world or the network.
                *self = before;
                self.velocity = Vec3::ZERO;
                self.spin = Vec3::ZERO;
                self.calm_s = SLEEP_S;
                break;
            }
        }
        report
    }

    fn is_finite(&self) -> bool {
        self.center.is_finite()
            && self.rotation.is_finite()
            && self.velocity.is_finite()
            && self.spin.is_finite()
    }

    pub(crate) fn ready_to_sleep(&self) -> bool {
        self.calm_s >= SLEEP_S
    }

    fn substep(
        &mut self,
        dt: f32,
        sweep: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        terrain: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        report: &mut StepReport,
    ) {
        let count = std::cell::Cell::new(0u32);
        let mut trace = |start, end, half| {
            count.set(count.get() + 1);
            sweep(start, end, half)
        };
        let mut terrain = |start, end, half| {
            count.set(count.get() + 1);
            terrain(start, end, half)
        };
        self.advance(dt, &mut trace, &mut terrain);
        report.sweeps += count.get();
    }

    fn advance(
        &mut self,
        dt: f32,
        trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        terrain: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    ) {
        self.velocity.z -= GRAVITY * dt;
        self.clamp_speed(MAX_FALL_SPEED);
        self.awake_s += dt;
        if self.lost || self.center.z < self.launch_z - FALL_LIMIT {
            self.lost = true;
            self.stop_at_last_free(dt);
            return;
        }
        let old_rotation = self.rotation;
        let old_half = self.sweep_half(old_rotation);
        if !self.free && !self.get_free(trace, old_half) {
            self.inside_world(trace, terrain, dt);
            return;
        }
        let mut rotation = turned(old_rotation, self.spin, dt);
        let mut half = self.sweep_half(rotation);
        let mut start = self.center;
        // Set when the body was raised onto what is under it: it touched
        // that, along this normal.
        let mut rested = None;
        if rotation != old_rotation {
            let grown = half.z - old_half.z;
            if grown > 0.0 {
                // The turn lowers the bottom: turn about whatever is under
                // the old bottom. Without this the turned box can start deep
                // inside the terrain, out of reach of any sweep.
                let top = start + Vec3::Z * (grown + CONTACT_SLOP);
                let probe = trace(top, start, half);
                if !probe.start_solid && probe.fraction < 1.0 {
                    start = probe.end;
                    rested = Some(probe.normal.try_normalize().unwrap_or(Vec3::Z));
                }
            }
            // A sweep that meets terrain forgets that it started inside a
            // brush, so the turned box is tested where it stands. What is
            // under it was dealt with above; what is left is beside it (a
            // wall, a kerb, the car), which the turn may not go into, and
            // lifting it clear would only hop it up onto that.
            if !fits(trace, start, half) {
                rotation = old_rotation;
                half = old_half;
                start = self.center;
                rested = None;
                self.spin = Vec3::ZERO;
            }
        }
        let delta = self.velocity * dt;
        let mut hit = trace(start, start + delta, half);
        if hit.start_solid && rotation != old_rotation {
            // Terrain the turned box overlaps and is moving into.
            let unturned = trace(self.center, self.center + delta, old_half);
            if !unturned.start_solid {
                rotation = old_rotation;
                half = old_half;
                start = self.center;
                rested = None;
                self.spin = Vec3::ZERO;
                hit = unturned;
            }
        }
        if hit.start_solid {
            if let Some(lifted) = unstick(trace, start, half) {
                start = lifted;
                rested = Some(Vec3::Z);
                hit = trace(start, start + delta, half);
            }
        }
        if hit.start_solid {
            self.stop_at_last_free(dt);
            return;
        }
        self.rotation = rotation;
        let before = start;
        let mut touched = hit.fraction < 1.0 || rested.is_some();
        if hit.fraction < 1.0 {
            let normal = hit.normal.try_normalize().unwrap_or(Vec3::Z);
            self.center = hit.end + separation(normal);
            self.collide(normal);
            self.on_ground = normal.z > GROUND_NZ;
            // Spend the rest of the substep with the velocity the contact
            // left, so a part sliding along the ground is not slowed by
            // gravity pressing it into the ground every substep.
            let rest = (1.0 - hit.fraction.max(0.0)) * dt;
            if rest > dt * 0.01 {
                let slide = trace(self.center, self.center + self.velocity * rest, half);
                if slide.start_solid {
                    // Stay at the contact.
                } else if slide.fraction < 1.0 {
                    let normal = slide.normal.try_normalize().unwrap_or(Vec3::Z);
                    self.center = slide.end + separation(normal);
                    self.collide(normal);
                    self.on_ground |= normal.z > GROUND_NZ;
                } else {
                    self.center = slide.end;
                }
            }
        } else {
            self.center = hit.end;
            self.on_ground = rested.is_some_and(|normal| normal.z > GROUND_NZ);
            if let Some(normal) = rested {
                // Being raised stands in for turning about the corner that
                // touched: that corner is a contact, and has to lose its
                // speed into the surface like one, or every lift adds energy.
                self.collide(normal);
            }
        }
        // Every pose the body takes is one it fits in. A sweep that meets
        // terrain forgets that it started inside a brush, and being set off
        // a terrain edge can put it into the corner of a brush, which the
        // sweeps then cannot see out of. If it ended up anywhere like that,
        // it stays where it started this substep instead.
        if self.center != before && !fits(trace, self.center, half) {
            self.center = before;
            touched = true;
        }
        self.last_free = (self.center, self.rotation);
        if touched {
            self.keep_to_what_was_achieved(delta, self.center - before, dt);
        }
        let slow = self.velocity.length() < SETTLE_SPEED && self.spin.length() < SETTLE_SPIN;
        if self.on_ground && (slow || self.awake_s > LATE_SETTLE_S) {
            self.velocity *= SETTLE_KEEP;
            self.spin *= SETTLE_KEEP;
        }
        // Calm is judged by how far the body actually went and turned: one
        // wedged in a crease or leaning on a wall keeps a velocity and a spin
        // it cannot use.
        let moved = (self.center - before).length() / dt;
        let turned_rate = old_rotation.angle_between(self.rotation) / dt;
        let calm = touched
            && moved.min(self.velocity.length()) < SLEEP_SPEED
            && turned_rate.min(self.spin.length()) < SLEEP_SPIN;
        self.calm_s = if calm { self.calm_s + dt } else { 0.0 };
    }

    /// A body blocked short of where its velocity was taking it keeps no more
    /// speed that way than it actually covered. A contact whose normal does
    /// not face the move (a terrain edge beside a part pressed down by
    /// gravity) stops the move without taking any speed away, and that speed
    /// would otherwise build up tick after tick and throw the part later.
    fn keep_to_what_was_achieved(&mut self, intended: Vec3, achieved: Vec3, dt: f32) {
        let Some(direction) = intended.try_normalize() else {
            return;
        };
        let allowed = achieved.dot(direction).max(0.0) / dt;
        let along = self.velocity.dot(direction);
        if along > allowed {
            self.velocity -= direction * (along - allowed);
        }
    }

    /// Whether a body spawned inside the world is clear of it now, or can
    /// be lifted clear (a part overlapping the ground it lies on).
    fn get_free(&mut self, trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep, half: Vec3) -> bool {
        let at = if fits(trace, self.center, half) {
            Some(self.center)
        } else if self.escape_s == 0.0 {
            unstick(trace, self.center, half)
        } else {
            None
        };
        if let Some(at) = at {
            self.center = at;
            self.free = true;
            self.last_free = (at, self.rotation);
        }
        self.free
    }

    /// Still inside the world it was spawned in: a door inside its own car's
    /// clip. It flies out through it without turning, but never downward and
    /// never into terrain, which the brush around it hides from the world
    /// sweep. If that takes too long it is moved to the nearest clear place.
    fn inside_world(
        &mut self,
        trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        terrain: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
        dt: f32,
    ) {
        self.on_ground = false;
        let half = self.sweep_half(self.rotation);
        if self.escape_s < ESCAPE_MAX_S {
            self.calm_s = 0.0;
            self.escape_s += dt;
            self.velocity.z = self.velocity.z.max(0.0);
            let hit = terrain(self.center, self.center + self.velocity * dt, half);
            if hit.fraction < 1.0 {
                let normal = hit.normal.try_normalize().unwrap_or(Vec3::Z);
                if !hit.start_solid {
                    self.center = hit.end + separation(normal);
                }
                self.collide(normal);
            } else {
                self.center = hit.end;
            }
            return;
        }
        let toward = self.velocity.truncate().extend(0.0);
        match nearest_clear(trace, terrain, self.center, half, toward) {
            Some(at) => {
                self.center = at;
                self.free = true;
                self.last_free = (at, self.rotation);
            }
            None => {
                // Nowhere near is clear: leave it where it was launched.
                self.lost = true;
                self.stop_at_last_free(dt);
            }
        }
    }

    /// Back to the last place the body was clear of the world, at rest.
    fn stop_at_last_free(&mut self, dt: f32) {
        (self.center, self.rotation) = self.last_free;
        self.velocity = Vec3::ZERO;
        self.spin = Vec3::ZERO;
        self.calm_s += dt;
    }

    /// The box the sweeps use. The world trace takes the box's x extent as
    /// its capsule radius, so the wider of x and y goes there: a door turned
    /// edge-on to a wall then stops short of it instead of half inside it.
    fn sweep_half(&self, rotation: Quat) -> Vec3 {
        let half = world_half(rotation, self.half);
        let reach = half.x.max(half.y);
        Vec3::new(reach, reach, half.z)
    }

    /// Impulse response at the corners touching a surface with `normal`.
    fn collide(&mut self, normal: Vec3) {
        let ground = normal.z > GROUND_NZ;
        if ground {
            self.spin *= GROUND_SPIN_KEEP;
        }
        let mut lever = contact_lever(self.rotation, self.half, normal);
        let inverse = self.inverse_inertia_world();
        let approach = self.point_velocity(lever).dot(normal);
        if approach >= 0.0 {
            return;
        }
        let resting = -approach < REST_SPEED;
        let bounce = if resting { 0.0 } else { self.phys.bounce };
        // Resting on a face, give or take a few degrees: the ground carries
        // it under its centre, so holding it up adds no turn. A single
        // corner taking the whole support would rock a lying part for good.
        let lying = (ground && resting)
            .then(|| stable_face(self.rotation, self.half, normal))
            .flatten();
        if lying.is_some() {
            lever = normal * lever.dot(normal);
        }
        // What holding the whole body up takes, for friction while it rests.
        let carried = (-self.velocity.dot(normal)).max(0.0) * self.phys.mass;
        let k_normal = effective_inverse_mass(self.phys.mass, inverse, lever, normal);
        let jn = -(1.0 + bounce) * approach / k_normal;
        self.apply_impulse(normal * jn, lever);
        if ground && resting {
            // Resting friction holds the body as a whole, as on a real
            // slope: it stays put where the slope is under its friction
            // angle and slides where it is steeper. Through a single corner
            // it would mostly turn the part and let it creep downhill. Long
            // after its last push a part is simply held.
            let tangent = self.velocity - normal * self.velocity.dot(normal);
            let slip = tangent.length();
            if slip > 1e-4 {
                let held = if self.awake_s > LATE_SETTLE_S {
                    slip
                } else {
                    slip.min(self.phys.friction * carried / self.phys.mass)
                };
                self.velocity -= tangent / slip * held;
            }
        } else {
            let relative = self.point_velocity(lever);
            let tangent = relative - normal * relative.dot(normal);
            let slip = tangent.length();
            if slip > 1e-4 {
                let direction = tangent / slip;
                let k_tangent = effective_inverse_mass(self.phys.mass, inverse, lever, direction);
                let jt = (slip / k_tangent).min(self.phys.friction * jn);
                self.apply_impulse(-direction * jt, lever);
            }
        }
        // Friction at an off-centre corner also turns the body, which can
        // drive that corner back into the surface: stop it there again.
        let again = self.point_velocity(lever).dot(normal);
        if again < 0.0 {
            self.apply_impulse(normal * (-again / k_normal), lever);
        }
        if let Some((axis, target)) = lying {
            // Settle the last few degrees onto the face: turn toward it,
            // keeping only the spin about the ground's normal.
            let level = axis.cross(target);
            let angle = axis.dot(target).clamp(-1.0, 1.0).acos();
            let level = level.try_normalize().unwrap_or(Vec3::ZERO) * angle / LEVEL_S;
            self.spin = normal * self.spin.dot(normal) + level;
        } else if ground
            && resting
            && self.velocity.length() < SETTLE_SPEED
            && self.spin.length() < SETTLE_SPIN
        {
            // Coming to rest on an edge, or on a narrow face like a door on
            // its bottom edge: balanced, but only as long as nothing at all
            // disturbs it, and these contacts never do. Tip it over onto the
            // nearest face it can lie on.
            if let Some(tip) = toward_broad_face(self.rotation, self.half, normal) {
                self.spin = normal * self.spin.dot(normal) + tip * TIP_RATE;
            }
        }
        self.clamp_spin();
    }

    fn point_velocity(&self, lever: Vec3) -> Vec3 {
        self.velocity + self.spin.cross(lever)
    }

    /// On going to sleep: lay a part that is nearly flat exactly flat, then
    /// rest its lowest point on the ground below.
    pub(crate) fn settle(&mut self, sweep: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep) {
        self.velocity = Vec3::ZERO;
        self.spin = Vec3::ZERO;
        let old_rotation = self.rotation;
        let old_half = self.sweep_half(old_rotation);
        let flat = flatten(self.rotation);
        self.rotation = flat;
        let half = self.sweep_half(flat);
        let lift = (half.z - old_half.z).max(0.0) + CONTACT_SLOP;
        let start = self.center + Vec3::Z * lift;
        let down = sweep(start, start - Vec3::Z * (lift + UNSTICK_MAX_LIFT), half);
        if down.start_solid {
            self.rotation = old_rotation;
            return;
        }
        let old_center = self.center;
        if down.fraction < 1.0 && down.normal.z > GROUND_NZ {
            self.center = down.end + Vec3::Z * CONTACT_SLOP;
        } else if flat != old_rotation {
            self.rotation = old_rotation;
        }
        if (self.center, self.rotation) != (old_center, old_rotation)
            && !fits(sweep, self.center, self.sweep_half(self.rotation))
        {
            (self.center, self.rotation) = (old_center, old_rotation);
        }
    }

    fn apply_impulse(&mut self, impulse: Vec3, lever: Vec3) {
        self.velocity += impulse / self.phys.mass;
        self.spin += self.inverse_inertia_world() * lever.cross(impulse);
    }

    fn clamp_spin(&mut self) {
        let speed = self.spin.length();
        if speed > MAX_ANG_SPEED {
            self.spin *= MAX_ANG_SPEED / speed;
        }
        self.clamp_speed(MAX_SPEED);
    }

    fn clamp_speed(&mut self, max: f32) {
        let speed = self.velocity.length();
        if speed > max {
            self.velocity *= max / speed;
        }
    }

    /// Solid-box inertia, inverted and turned into world space.
    fn inverse_inertia_world(&self) -> Mat3 {
        let h2 = self.half * self.half;
        let m = self.phys.mass / 3.0;
        let local = Vec3::new(
            1.0 / (m * (h2.y + h2.z)),
            1.0 / (m * (h2.x + h2.z)),
            1.0 / (m * (h2.x + h2.y)),
        );
        let r = Mat3::from_quat(self.rotation);
        r * Mat3::from_diagonal(local) * r.transpose()
    }
}

pub(crate) fn rotation_from_axis(axis: [[f32; 3]; 3]) -> Quat {
    Quat::from_mat3(&Mat3::from_cols(
        Vec3::from_array(axis[0]),
        Vec3::from_array(axis[1]),
        Vec3::from_array(axis[2]),
    ))
    .normalize()
}

fn world_half(rotation: Quat, half: Vec3) -> Vec3 {
    let m = Mat3::from_quat(rotation);
    m.x_axis.abs() * half.x + m.y_axis.abs() * half.y + m.z_axis.abs() * half.z
}

fn turned(rotation: Quat, spin: Vec3, dt: f32) -> Quat {
    let angle = spin.length() * dt;
    if angle <= 1e-6 {
        return rotation;
    }
    (Quat::from_axis_angle(spin / spin.length(), angle) * rotation).normalize()
}

/// How far a body is set off a surface it touched. Off the ground it is
/// raised straight up: set off along a slope's normal, it would come back
/// down a little further along each time and creep down every slope.
fn separation(normal: Vec3) -> Vec3 {
    if normal.z > GROUND_NZ {
        Vec3::Z * (CONTACT_SLOP / normal.z)
    } else {
        normal * CONTACT_SLOP
    }
}

/// Whether the box fits at `at` without overlapping the world.
fn fits(trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep, at: Vec3, half: Vec3) -> bool {
    !trace(at, at, half).start_solid
}

/// Lifts a stuck box by doubling steps up to [`UNSTICK_MAX_LIFT`].
fn unstick(
    trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    at: Vec3,
    half: Vec3,
) -> Option<Vec3> {
    let mut dz = CONTACT_SLOP;
    while dz <= UNSTICK_MAX_LIFT {
        let lifted = at + Vec3::Z * dz;
        if fits(trace, lifted, half) {
            return Some(lifted);
        }
        dz *= 2.0;
    }
    None
}

/// The nearest place a box stuck inside the world fits, trying the way it
/// was going first, then sideways and up (never down), out to 128 units,
/// and only where getting there crosses no terrain: a place under the
/// terrain fits just as well as one on top of it.
fn nearest_clear(
    trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    terrain: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    at: Vec3,
    half: Vec3,
    toward: Vec3,
) -> Option<Vec3> {
    let ways = [
        toward.try_normalize().unwrap_or(Vec3::ZERO),
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
    ];
    let mut reach = 8.0;
    while reach <= 128.0 {
        for way in ways.iter().filter(|w| **w != Vec3::ZERO) {
            let candidate = at + *way * reach;
            if fits(trace, candidate, half) && terrain(at, candidate, half).fraction >= 1.0 {
                return Some(candidate);
            }
        }
        reach *= 2.0;
    }
    None
}

/// From the centre to the middle of the corners deepest along `-normal`.
fn contact_lever(rotation: Quat, half: Vec3, normal: Vec3) -> Vec3 {
    let corners: [Vec3; 8] = std::array::from_fn(|i| {
        let sign = Vec3::new(
            if i & 1 == 0 { -1.0 } else { 1.0 },
            if i & 2 == 0 { -1.0 } else { 1.0 },
            if i & 4 == 0 { -1.0 } else { 1.0 },
        );
        rotation * (half * sign)
    });
    let deepest = corners
        .iter()
        .map(|c| c.dot(normal))
        .fold(f32::INFINITY, f32::min);
    let (sum, n) = corners
        .iter()
        .filter(|c| c.dot(normal) <= deepest + CONTACT_PLANE_TOL)
        .fold((Vec3::ZERO, 0.0), |(sum, n), c| (sum + *c, n + 1.0));
    sum / n
}

fn effective_inverse_mass(mass: f32, inverse: Mat3, lever: Vec3, direction: Vec3) -> f32 {
    1.0 / mass + direction.dot((inverse * lever.cross(direction)).cross(lever))
}

/// The body axis nearest to `up` and the way along `up` it points, when it
/// is within 15 degrees of it: the body is lying on that face.
fn nearest_face(rotation: Quat, up: Vec3) -> Option<(Vec3, Vec3)> {
    let m = Mat3::from_quat(rotation);
    let (axis, cos) = [m.x_axis, m.y_axis, m.z_axis]
        .into_iter()
        .map(|a| (a, a.dot(up).abs()))
        .fold((up, -1.0), |best, c| if c.1 > best.1 { c } else { best });
    (cos >= FACE_SNAP_COS).then(|| (axis, up * axis.dot(up).signum()))
}

/// Like [`nearest_face`], but only a face broad enough to stand on: one
/// whose narrower side is at least half the height of the centre above it.
/// A door on its bottom edge is within 15 degrees of a face too, but it
/// topples from there, so it is left to its corners.
fn stable_face(rotation: Quat, half: Vec3, up: Vec3) -> Option<(Vec3, Vec3)> {
    let (axis, target) = nearest_face(rotation, up)?;
    let m = Mat3::from_quat(rotation);
    let along = [m.x_axis, m.y_axis, m.z_axis].map(|a| a.dot(axis).abs());
    let i = (0..3).fold(0, |best, k| if along[k] > along[best] { k } else { best });
    let height = half[i];
    let side = (0..3)
        .filter(|&k| k != i)
        .map(|k| half[k])
        .fold(f32::INFINITY, f32::min);
    (side >= BROAD_FACE * height).then_some((axis, target))
}

/// The axis to turn about to tip a body toward the face it can lie on (see
/// [`stable_face`]) that is nearest to lying on `up`.
fn toward_broad_face(rotation: Quat, half: Vec3, up: Vec3) -> Option<Vec3> {
    let m = Mat3::from_quat(rotation);
    let axes = [m.x_axis, m.y_axis, m.z_axis];
    let broad = |i: usize| {
        let side = (0..3)
            .filter(|&k| k != i)
            .map(|k| half[k])
            .fold(f32::INFINITY, f32::min);
        side >= BROAD_FACE * half[i]
    };
    let face = (0..3)
        .filter(|&i| broad(i))
        .max_by(|&a, &b| axes[a].dot(up).abs().total_cmp(&axes[b].dot(up).abs()))?;
    let axis = axes[face] * if axes[face].dot(up) < 0.0 { -1.0 } else { 1.0 };
    axis.cross(up).try_normalize()
}

/// The orientation with the body axis nearest to vertical made exactly
/// vertical, when it is within 15 degrees; otherwise unchanged.
fn flatten(rotation: Quat) -> Quat {
    match nearest_face(rotation, Vec3::Z) {
        Some((axis, target)) if axis.dot(target) < 1.0 => {
            (Quat::from_rotation_arc(axis, target) * rotation).normalize()
        }
        _ => rotation,
    }
}

/// Whether a box moving from `start` to `end` with extents `mins..maxs`
/// overlaps the box `lo..hi` at some point of the move.
pub(crate) fn swept_box_overlaps(
    start: Vec3,
    end: Vec3,
    mins: Vec3,
    maxs: Vec3,
    lo: Vec3,
    hi: Vec3,
) -> bool {
    // Grow the target by the moving box and test the centre's segment.
    let lo = lo - maxs;
    let hi = hi - mins;
    let delta = end - start;
    let (mut enter, mut exit) = (0.0f32, 1.0f32);
    for i in 0..3 {
        let (s, d) = (start[i], delta[i]);
        if d.abs() < 1e-6 {
            if s <= lo[i] || s >= hi[i] {
                return false;
            }
            continue;
        }
        let (a, b) = ((lo[i] - s) / d, (hi[i] - s) / d);
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        enter = enter.max(a);
        exit = exit.min(b);
        if enter >= exit {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod street_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// The small hatch's left door: model origin at the hinge.
    const DOOR_MID: Vec3 = Vec3::new(-19.67, -3.56, 7.48);
    const DOOR_HALF: Vec3 = Vec3::new(19.34, 5.04, 23.03);
    const DOOR: Phys = Phys {
        mass: 25.0,
        bounce: 0.1,
        friction: 0.25,
    };

    fn open(end: Vec3, start_solid: bool) -> Sweep {
        Sweep {
            fraction: 1.0,
            end,
            normal: Vec3::ZERO,
            start_solid,
        }
    }

    fn solid(start: Vec3) -> Sweep {
        Sweep {
            fraction: 0.0,
            end: start,
            normal: Vec3::Z,
            start_solid: true,
        }
    }

    /// A flat floor at z = 0 under an open sky.
    fn floor(start: Vec3, end: Vec3, half: Vec3) -> Sweep {
        let bottom = |p: Vec3| p.z - half.z;
        if bottom(start) < 0.0 {
            return if bottom(end) < 0.0 {
                solid(start)
            } else {
                open(end, true)
            };
        }
        if bottom(end) >= 0.0 {
            return open(end, false);
        }
        let fraction = bottom(start) / (bottom(start) - bottom(end));
        Sweep {
            fraction,
            end: start + (end - start) * fraction,
            normal: Vec3::Z,
            start_solid: false,
        }
    }

    /// The floor plus a clip block around a car: x and y within 60 of the
    /// origin, z from 0 to 60, solid to these sweeps as it is to the
    /// world trace.
    fn floor_and_car(start: Vec3, end: Vec3, half: Vec3) -> Sweep {
        let inside = |p: Vec3| {
            (0..2).all(|i| p[i].abs() < 60.0 + half[i]) && p.z - half.z < 60.0 && p.z + half.z > 0.0
        };
        match (inside(start), inside(end)) {
            (true, true) => solid(start),
            (true, false) => {
                let mut hit = floor(start, end, half);
                hit.start_solid = true;
                hit
            }
            (false, _) if inside(end) => {
                // Treat the block as a wall the box stops at, half way.
                let mut hit = floor(start, start + (end - start) * 0.5, half);
                if hit.fraction >= 1.0 {
                    hit.fraction = 0.5;
                    hit.normal = Vec3::new(start.x - end.x, start.y - end.y, 0.0)
                        .try_normalize()
                        .unwrap_or(Vec3::X);
                }
                hit
            }
            _ => floor(start, end, half),
        }
    }

    fn yaw_axis(yaw_deg: f32) -> [[f32; 3]; 3] {
        math_iw4::angles_to_axis([0.0, yaw_deg, 0.0])
    }

    /// The white hatch's left door on Favela (yaw 62, hinge 28.2 up), pushed
    /// along tag_body -> hinge: outward, forward and 24 degrees down.
    fn launched_door(force: f32) -> RigidBody {
        let hinge = Vec3::new(0.0, 0.0, 28.2);
        let direction = Vec3::new(-0.324, 0.858, -0.401).normalize();
        RigidBody::launch(
            hinge,
            yaw_axis(62.0),
            DOOR_MID,
            DOOR_HALF,
            DOOR,
            hinge,
            direction * force,
        )
    }

    fn run_until_asleep(body: &mut RigidBody, max_ticks: u32) -> u32 {
        let mut sweep = |s, e, h| floor(s, e, h);
        for tick in 1..=max_ticks {
            body.step(0.05, &mut sweep, &mut |s, e, h| floor(s, e, h));
            if body.ready_to_sleep() {
                body.settle(&mut sweep);
                return tick;
            }
        }
        body.settle(&mut sweep);
        max_ticks
    }

    #[test]
    fn launch_speed_is_force_over_mass() {
        let body = RigidBody::launch(
            Vec3::new(0.0, 0.0, 100.0),
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::splat(10.0),
            DOOR,
            Vec3::new(0.0, 0.0, 100.0),
            Vec3::new(4500.0, 0.0, 0.0),
        );
        assert!((body.velocity - Vec3::new(180.0, 0.0, 0.0)).length() < 1e-3);
        assert!(
            body.spin.length() < 1e-6,
            "a push through the centre does not spin"
        );
    }

    #[test]
    fn a_hinge_push_spins_across_the_lever_and_the_force() {
        let body = launched_door(4500.0);
        let lever = Vec3::new(0.0, 0.0, 28.2) - body.center;
        let force = body.velocity * DOOR.mass;
        assert!(body.spin.length() > 1.0);
        assert!(body.spin.length() <= MAX_ANG_SPEED + 1e-3);
        let torque = lever.cross(force).normalize();
        assert!(body.spin.normalize().dot(torque) > 0.0);
        let far = RigidBody::launch(
            Vec3::new(500.0, 800.0, 600.0),
            yaw_axis(30.0),
            Vec3::ZERO,
            Vec3::new(16.0, 30.0, 16.0),
            Phys {
                mass: 1.0,
                bounce: 0.1,
                friction: 0.5,
            },
            Vec3::ZERO,
            Vec3::new(3.0, 1.0, 4.0),
        );
        assert_eq!(far.spin, Vec3::ZERO, "a push from far away does not spin");
    }

    #[test]
    fn a_door_thrown_down_bounces_slides_and_lies_flat_on_the_floor() {
        let mut body = launched_door(4500.0);
        let start = body.center;
        let mut sweep = |s, e, h| floor(s, e, h);
        let mut first_contact = None;
        for _ in 0..40 {
            let before = body;
            body.step(SUBSTEP_S, &mut sweep, &mut |s, e, h| floor(s, e, h));
            if body.on_ground {
                first_contact = Some((before, body));
                break;
            }
        }
        let (before, after) = first_contact.expect("the door reaches the floor");
        // The corner that struck the floor leaves it again (bounce 0.1), and
        // the door keeps going instead of stopping dead where it touched.
        let lever = contact_lever(after.rotation, after.half, Vec3::Z);
        let corner = after.velocity + after.spin.cross(lever);
        assert!(corner.z > -1.0, "the striking corner rebounds: {corner}");
        assert!(
            after.velocity.length() > 0.3 * before.velocity.length(),
            "the first contact does not stop the door: {} -> {}",
            before.velocity.length(),
            after.velocity.length()
        );
        let touched = after.center;
        let ticks = run_until_asleep(&mut body, 200);
        assert!(ticks < 200, "the door comes to rest by itself");
        let travel = (body.center - touched).truncate().length();
        assert!(travel > 10.0, "the door slides on after touching: {travel}");
        assert!((body.center - start).truncate().length() > travel * 0.5);
        let lowest = body.center.z - body.world_half().z;
        assert!(
            (0.0..=0.5).contains(&lowest),
            "lowest corner on top of the floor: {lowest}"
        );
        let thin = Vec3::from_array(body.axis()[1]);
        assert!(
            thin.z.abs() > 10.0f32.to_radians().cos(),
            "the thin axis is vertical (lying flat): {thin}"
        );
    }

    #[test]
    fn doors_launched_with_different_forces_land_apart() {
        let mut a = launched_door(4000.0);
        let mut b = launched_door(5000.0);
        run_until_asleep(&mut a, 200);
        run_until_asleep(&mut b, 200);
        assert!((a.center - b.center).length() > 1.0);
    }

    #[test]
    fn a_box_started_inside_the_floor_is_lifted_out_and_moves() {
        let mut body = RigidBody::launch(
            Vec3::new(0.0, 0.0, 8.0),
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::splat(10.0),
            DOOR,
            Vec3::new(0.0, 0.0, 8.0),
            Vec3::new(2500.0, 0.0, 0.0),
        );
        let mut sweep = |s, e, h| floor(s, e, h);
        body.step(0.05, &mut sweep, &mut |s, e, h| floor(s, e, h));
        assert!(body.center.z - body.world_half().z >= 0.0);
        assert!(body.center.x > 1.0, "it is not frozen in place");
    }

    #[test]
    fn a_door_launched_from_inside_its_cars_clip_flies_out_and_lands() {
        let mut body = launched_door(4500.0);
        body.velocity = Vec3::new(-89.0, 234.0, 167.0);
        let mut sweep = |s, e, h| floor_and_car(s, e, h);
        let start = body.center;
        assert!(sweep(start, start, body.world_half()).start_solid);
        let mut asleep = false;
        for _ in 0..200 {
            body.step(0.05, &mut sweep, &mut |s, e, h| floor(s, e, h));
            if body.ready_to_sleep() {
                body.settle(&mut sweep);
                asleep = true;
                break;
            }
        }
        assert!(asleep, "the door settles instead of hanging at its mount");
        assert!((body.center - start).length() > 40.0, "it left the car");
        let lowest = body.center.z - body.world_half().z;
        assert!(
            (0.0..=0.5).contains(&lowest) || (60.0..=60.5).contains(&lowest),
            "it rests on the floor or on top of the block: {lowest}"
        );
    }

    /// A terrain edge beside a part: every move down stops at once with a
    /// sideways normal, which by itself takes no speed away.
    fn edge(start: Vec3, end: Vec3, _half: Vec3) -> Sweep {
        if end.z < start.z {
            Sweep {
                fraction: 0.0,
                end: start,
                normal: Vec3::X,
                start_solid: false,
            }
        } else {
            open(end, false)
        }
    }

    #[test]
    fn a_part_held_up_by_an_edge_does_not_store_up_speed() {
        let at = Vec3::new(0.0, 0.0, 50.0);
        let mut body = RigidBody::launch(
            at,
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::new(10.0, 4.0, 10.0),
            DOOR,
            at,
            Vec3::ZERO,
        );
        for _ in 0..40 {
            body.step(0.05, &mut |s, e, h| edge(s, e, h), &mut |s, e, h| {
                edge(s, e, h)
            });
            assert!(
                body.velocity.length() <= GRAVITY * SUBSTEP_S + 1e-3,
                "speed it cannot use is not kept: {}",
                body.velocity
            );
        }
        assert!(body.center.z >= at.z - 1e-3, "and it is not pulled down");
    }

    #[test]
    fn a_part_that_falls_out_of_the_world_stops() {
        let at = Vec3::new(0.0, 0.0, 100.0);
        let mut body = RigidBody::launch(
            at,
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::splat(8.0),
            DOOR,
            at,
            Vec3::ZERO,
        );
        let mut void = |_s: Vec3, e: Vec3, _h: Vec3| open(e, false);
        let mut ticks = 0;
        while !body.ready_to_sleep() && ticks < 200 {
            body.step(0.05, &mut void, &mut |_s, e, _h| open(e, false));
            ticks += 1;
        }
        assert!(ticks < 200, "it goes to sleep instead of falling for good");
        assert!(body.lost);
        assert!(body.center.z > at.z - FALL_LIMIT - 100.0);
    }

    #[test]
    fn a_box_on_its_edge_tips_over_onto_a_face() {
        let tilt = Quat::from_rotation_x(40.0f32.to_radians());
        let mut body = RigidBody::launch(
            Vec3::new(0.0, 0.0, 40.0),
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::new(20.0, 5.0, 20.0),
            DOOR,
            Vec3::new(0.0, 0.0, 40.0),
            Vec3::ZERO,
        );
        body.rotation = tilt;
        run_until_asleep(&mut body, 200);
        let up = Mat3::from_quat(body.rotation);
        let best = [up.x_axis, up.y_axis, up.z_axis]
            .iter()
            .map(|a| a.z.abs())
            .fold(0.0, f32::max);
        assert!(best > 0.999, "it ends on a face, not balanced on an edge");
        let lowest = body.center.z - body.world_half().z;
        assert!((0.0..=0.5).contains(&lowest), "{lowest}");
    }

    #[test]
    fn an_explosion_push_is_divided_by_mass() {
        let mut body = RigidBody::launch(
            Vec3::ZERO,
            yaw_axis(0.0),
            Vec3::ZERO,
            Vec3::splat(10.0),
            DOOR,
            Vec3::ZERO,
            Vec3::ZERO,
        );
        body.push(Vec3::new(0.0, 0.0, 2500.0), Vec3::ZERO, 0.25);
        assert!((body.velocity.z - 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_swept_box_hits_only_what_its_path_crosses() {
        let (mins, maxs) = (Vec3::splat(-10.0), Vec3::splat(10.0));
        let player = (Vec3::new(-15.0, -15.0, 0.0), Vec3::new(15.0, 15.0, 70.0));
        let falls_on = swept_box_overlaps(
            Vec3::new(0.0, 0.0, 120.0),
            Vec3::new(0.0, 0.0, 60.0),
            mins,
            maxs,
            player.0,
            player.1,
        );
        assert!(falls_on);
        let diagonal_miss = swept_box_overlaps(
            Vec3::new(-60.0, 60.0, 120.0),
            Vec3::new(60.0, 180.0, 60.0),
            mins,
            maxs,
            player.0,
            player.1,
        );
        assert!(
            !diagonal_miss,
            "the box around the whole move would have hit"
        );
    }
}
