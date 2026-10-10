//! A launched part (a car door, a hood, a care package) as a small rigid body:
//! an oriented box with the mass, bounce and friction of its model's physics
//! preset. It falls, spins, bounces off the world, tips over and settles on top
//! of the ground. The step is a pure function over a sweep closure, so it runs
//! against the static world in the game and against an analytic floor in tests.
//!
//! Collision sweeps the world box that encloses the rotated box. Against a floor
//! that box's bottom is exactly the body's lowest corner; against walls it is a
//! little generous. The contact response is an impulse at the corners that touch,
//! so a part landing on an edge tips over and a part lying on a face stays put.

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
/// Guard for a preset with a near-zero mass: no part leaves faster than this.
const MAX_SPEED: f32 = 4000.0;
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
    /// The box starts inside the world. Moving out of it is not blocked.
    pub start_solid: bool,
    /// The whole move stays inside the world.
    pub all_solid: bool,
}

impl Sweep {
    pub(crate) fn from_trace(trace: &trace_iw4::Trace) -> Self {
        Self {
            fraction: trace.fraction,
            end: Vec3::from_array(trace.endpos),
            normal: Vec3::from_array(trace.normal),
            start_solid: trace.startsolid != 0 || trace.allsolid != 0,
            all_solid: trace.allsolid != 0,
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
        self.on_ground = false;
    }

    /// Advances `dt` seconds in whole substeps.
    pub(crate) fn step(
        &mut self,
        dt: f32,
        sweep: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    ) -> StepReport {
        let mut report = StepReport::default();
        let steps = (dt / SUBSTEP_S).round().max(1.0) as u32;
        let sub = dt / steps as f32;
        for _ in 0..steps {
            let before = *self;
            self.substep(sub, sweep, &mut report);
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
        report: &mut StepReport,
    ) {
        let mut trace = |start, end, half| {
            report.sweeps += 1;
            sweep(start, end, half)
        };
        self.velocity.z -= GRAVITY * dt;
        let old_rotation = self.rotation;
        let old_half = world_half(old_rotation, self.half);
        let mut rotation = turned(old_rotation, self.spin, dt);
        let mut half = world_half(rotation, self.half);
        let mut start = self.center;
        let mut hit = trace(start, start + self.velocity * dt, half);
        let mut lifted_out = false;
        if hit.all_solid {
            let grown = (half.z - old_half.z).max(0.0);
            if let Some(lifted) = unstick(&mut trace, start, half, grown) {
                start = lifted;
                lifted_out = true;
                hit = trace(start, start + self.velocity * dt, half);
            } else {
                let unturned = trace(start, start + self.velocity * dt, old_half);
                if unturned.all_solid {
                    // Inside the world whichever way it faces: a part spawned
                    // inside its own car's or box's clip. Let it fly out
                    // through it rather than freeze where it was mounted.
                    self.rotation = rotation;
                    self.center = start + self.velocity * dt;
                    self.on_ground = false;
                    self.calm_s = 0.0;
                    return;
                }
                // The world blocks the turn: keep the old orientation.
                rotation = old_rotation;
                half = old_half;
                self.spin = Vec3::ZERO;
                hit = unturned;
            }
            if hit.all_solid {
                hit = Sweep {
                    fraction: 0.0,
                    end: start,
                    normal: Vec3::Z,
                    start_solid: true,
                    all_solid: true,
                };
            }
        }
        self.rotation = rotation;
        let before = self.center;
        let touched = hit.fraction < 1.0 || lifted_out;
        if hit.fraction < 1.0 {
            let normal = hit.normal.try_normalize().unwrap_or(Vec3::Z);
            self.center = hit.end + normal * CONTACT_SLOP;
            self.collide(normal);
            self.on_ground = normal.z > GROUND_NZ;
            // Spend the rest of the substep with the velocity the contact
            // left, so a part sliding along the ground is not slowed by
            // gravity pressing it into the ground every substep.
            let rest = (1.0 - hit.fraction.max(0.0)) * dt;
            if rest > dt * 0.01 && !hit.all_solid {
                let slide = trace(self.center, self.center + self.velocity * rest, half);
                if slide.start_solid {
                    // Stay at the contact.
                } else if slide.fraction < 1.0 {
                    let normal = slide.normal.try_normalize().unwrap_or(Vec3::Z);
                    self.center = slide.end + normal * CONTACT_SLOP;
                    self.collide(normal);
                    self.on_ground |= normal.z > GROUND_NZ;
                } else {
                    self.center = slide.end;
                }
            }
        } else {
            self.center = hit.end;
            self.on_ground = lifted_out;
            if lifted_out {
                // The lift stands in for turning about the corner that dug
                // into the ground: that corner is a contact, and has to lose
                // its downward speed like one, or every lift adds energy.
                self.collide(Vec3::Z);
            }
        }
        if self.on_ground
            && self.velocity.length() < SETTLE_SPEED
            && self.spin.length() < SETTLE_SPIN
        {
            self.velocity *= SETTLE_KEEP;
            self.spin *= SETTLE_KEEP;
        }
        // Calm is judged by how far the body actually went: one wedged in a
        // crease or against a wall keeps a velocity it cannot use.
        let moved = (self.center - before).length() / dt;
        let calm = touched
            && moved.min(self.velocity.length()) < SLEEP_SPEED
            && self.spin.length() < SLEEP_SPIN;
        self.calm_s = if calm { self.calm_s + dt } else { 0.0 };
    }

    /// Impulse response at the corners touching a surface with `normal`.
    fn collide(&mut self, normal: Vec3) {
        if normal.z > GROUND_NZ {
            self.spin *= GROUND_SPIN_KEEP;
        }
        let lever = contact_lever(self.rotation, self.half, normal);
        let inverse = self.inverse_inertia_world();
        let approach = self.point_velocity(lever).dot(normal);
        if approach >= 0.0 {
            return;
        }
        let bounce = if -approach < REST_SPEED {
            0.0
        } else {
            self.phys.bounce
        };
        let k_normal = effective_inverse_mass(self.phys.mass, inverse, lever, normal);
        let jn = -(1.0 + bounce) * approach / k_normal;
        self.apply_impulse(normal * jn, lever);
        let relative = self.point_velocity(lever);
        let tangent = relative - normal * relative.dot(normal);
        let slip = tangent.length();
        if slip > 1e-4 {
            let direction = tangent / slip;
            let k_tangent = effective_inverse_mass(self.phys.mass, inverse, lever, direction);
            let jt = (slip / k_tangent).min(self.phys.friction * jn);
            self.apply_impulse(-direction * jt, lever);
        }
        // Friction at an off-centre corner also turns the body, which can
        // drive that corner back into the surface: stop it there again.
        let again = self.point_velocity(lever).dot(normal);
        if again < 0.0 {
            self.apply_impulse(normal * (-again / k_normal), lever);
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
        let old_half = self.world_half();
        let flat = flatten(self.rotation);
        self.rotation = flat;
        let half = self.world_half();
        let lift = (half.z - old_half.z).max(0.0) + CONTACT_SLOP;
        let start = self.center + Vec3::Z * lift;
        let down = sweep(start, start - Vec3::Z * (lift + UNSTICK_MAX_LIFT), half);
        if down.start_solid {
            self.rotation = old_rotation;
            return;
        }
        if down.fraction < 1.0 && down.normal.z > GROUND_NZ {
            self.center = down.end + Vec3::Z * CONTACT_SLOP;
        } else if flat != old_rotation {
            self.rotation = old_rotation;
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
        let speed = self.velocity.length();
        if speed > MAX_SPEED {
            self.velocity *= MAX_SPEED / speed;
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

/// Lifts a stuck box: first by `first` (what the turn grew its bottom by),
/// then by doubling probes up to [`UNSTICK_MAX_LIFT`].
fn unstick(
    trace: &mut impl FnMut(Vec3, Vec3, Vec3) -> Sweep,
    at: Vec3,
    half: Vec3,
    first: f32,
) -> Option<Vec3> {
    let mut dz = if first > 0.0 { first } else { CONTACT_SLOP };
    let cap = dz.max(UNSTICK_MAX_LIFT);
    while dz <= cap {
        let lifted = at + Vec3::Z * dz;
        if !trace(lifted, lifted, half).start_solid {
            return Some(lifted);
        }
        dz *= 2.0;
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

/// The orientation with the body axis nearest to vertical made exactly
/// vertical, when it is within 15 degrees; otherwise unchanged.
fn flatten(rotation: Quat) -> Quat {
    let m = Mat3::from_quat(rotation);
    let (axis, cos) = [m.x_axis, m.y_axis, m.z_axis]
        .into_iter()
        .map(|a| (a, a.z.abs()))
        .fold(
            (Vec3::Z, -1.0),
            |best, c| if c.1 > best.1 { c } else { best },
        );
    if cos < FACE_SNAP_COS || cos >= 1.0 {
        return rotation;
    }
    let target = Vec3::Z * axis.z.signum();
    (Quat::from_rotation_arc(axis, target) * rotation).normalize()
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
            all_solid: false,
        }
    }

    fn solid(start: Vec3) -> Sweep {
        Sweep {
            fraction: 0.0,
            end: start,
            normal: Vec3::Z,
            start_solid: true,
            all_solid: true,
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
            all_solid: false,
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
            body.step(0.05, &mut sweep);
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
            body.step(SUBSTEP_S, &mut sweep);
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
        body.step(0.05, &mut sweep);
        assert!(body.center.z - body.world_half().z >= 0.0);
        assert!(body.center.x > 1.0, "it is not frozen in place");
    }

    #[test]
    fn a_door_launched_from_inside_its_cars_clip_flies_out_and_lands() {
        let mut body = launched_door(4500.0);
        body.velocity = Vec3::new(-89.0, 234.0, 167.0);
        let mut sweep = |s, e, h| floor_and_car(s, e, h);
        let start = body.center;
        assert!(sweep(start, start, body.world_half()).all_solid);
        let mut asleep = false;
        for _ in 0..200 {
            body.step(0.05, &mut sweep);
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
