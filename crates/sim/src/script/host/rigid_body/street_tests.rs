//! Car parts thrown around a small street built from the game's own world
//! collision: a road and a raised sidewalk as terrain triangles, a kerb face,
//! a house wall, a car's clip block and a box beside the road as brushes. The
//! sweeps are the real capsule traces, so a body that would sink into the
//! terrain or wedge against a kerb in the game does it here too.

use super::{Phys, RigidBody, Sweep};
use crate::world::{SimBrush, SimClipBsp, SimClipMesh};
use glam::{Mat3, Quat, Vec3};

const SIDEWALK_Y: f32 = 160.0;
const SIDEWALK_Z: f32 = 8.0;
const STREET_EDGE: f32 = 2400.0;

struct Street {
    brushes: Vec<SimBrush>,
    bsp: SimClipBsp,
    mesh: SimClipMesh,
}

fn aabb_brush(lo: Vec3, hi: Vec3) -> SimBrush {
    SimBrush {
        planes: vec![
            [1.0, 0.0, 0.0, hi.x],
            [-1.0, 0.0, 0.0, -lo.x],
            [0.0, 1.0, 0.0, hi.y],
            [0.0, -1.0, 0.0, -lo.y],
            [0.0, 0.0, 1.0, hi.z],
            [0.0, 0.0, -1.0, -lo.z],
        ],
        contents: crate::bullet_collision::CONTENTS_SOLID,
        plane_surface_flags: vec![0; 6],
        glass_encoded: 0,
    }
}

/// Two triangles over the quad `a b c d` (in order around it), facing `up`.
fn quad(verts: &mut Vec<[f32; 3]>, tris: &mut Vec<u16>, q: [Vec3; 4], facing: Vec3) {
    let base = verts.len() as u16;
    verts.extend(q.iter().map(|v| v.to_array()));
    for [i, j, k] in [[0u16, 1, 2], [0, 2, 3]] {
        let (v0, v1, v2) = (q[i as usize], q[j as usize], q[k as usize]);
        // The trace's face normal is (v0 - v2) x (v0 - v1).
        let normal = (v0 - v2).cross(v0 - v1);
        if normal.dot(facing) > 0.0 {
            tris.extend([base + i, base + j, base + k]);
        } else {
            tris.extend([base + i, base + k, base + j]);
        }
    }
}

impl Street {
    fn new(car: Vec3, car_yaw: f32) -> Self {
        let mut verts = Vec::new();
        let mut tris = Vec::new();
        // Small cells near the car, so parts meet triangle edges there; then
        // one big quad on each side out to the walls around the street.
        let cell = 160.0;
        for ix in 0..6 {
            let x0 = -480.0 + ix as f32 * cell;
            for iy in 0..6 {
                let y0 = -480.0 + iy as f32 * cell;
                let corner = |x: f32, y: f32| Vec3::new(x, y, floor_height(x, y0 + 1.0, x0 + 1.0));
                quad(
                    &mut verts,
                    &mut tris,
                    [
                        corner(x0, y0),
                        corner(x0 + cell, y0),
                        corner(x0 + cell, y0 + cell),
                        corner(x0, y0 + cell),
                    ],
                    Vec3::Z,
                );
            }
        }
        // The ramp's far end drops back to the road.
        quad(
            &mut verts,
            &mut tris,
            [
                Vec3::new(RAMP_END, -480.0, 0.0),
                Vec3::new(RAMP_END, -160.0, 0.0),
                Vec3::new(RAMP_END, -160.0, RAMP_TOP),
                Vec3::new(RAMP_END, -480.0, RAMP_TOP),
            ],
            Vec3::X,
        );
        // And its sides rise out of the road.
        for y in [-480.0, -160.0] {
            let facing = if y < -300.0 { Vec3::NEG_Y } else { Vec3::Y };
            let base = verts.len() as u16;
            let tri = [
                Vec3::new(RAMP_START, y, 0.0),
                Vec3::new(RAMP_END, y, 0.0),
                Vec3::new(RAMP_END, y, RAMP_TOP),
            ];
            verts.extend(tri.iter().map(|v| v.to_array()));
            let normal = (tri[0] - tri[2]).cross(tri[0] - tri[1]);
            if normal.dot(facing) > 0.0 {
                tris.extend([base, base + 1, base + 2]);
            } else {
                tris.extend([base, base + 2, base + 1]);
            }
        }
        let far = STREET_EDGE;
        for (lo, hi, z) in [
            (
                Vec3::new(-far, -far, 0.0),
                Vec3::new(-480.0, SIDEWALK_Y, 0.0),
                0.0,
            ),
            (
                Vec3::new(480.0, -far, 0.0),
                Vec3::new(far, SIDEWALK_Y, 0.0),
                0.0,
            ),
            (
                Vec3::new(-480.0, -far, 0.0),
                Vec3::new(480.0, -480.0, 0.0),
                0.0,
            ),
            (
                Vec3::new(-far, SIDEWALK_Y, 0.0),
                Vec3::new(-480.0, far, 0.0),
                SIDEWALK_Z,
            ),
            (
                Vec3::new(480.0, SIDEWALK_Y, 0.0),
                Vec3::new(far, far, 0.0),
                SIDEWALK_Z,
            ),
            (
                Vec3::new(-480.0, 480.0, 0.0),
                Vec3::new(480.0, far, 0.0),
                SIDEWALK_Z,
            ),
        ] {
            quad(
                &mut verts,
                &mut tris,
                [
                    Vec3::new(lo.x, lo.y, z),
                    Vec3::new(hi.x, lo.y, z),
                    Vec3::new(hi.x, hi.y, z),
                    Vec3::new(lo.x, hi.y, z),
                ],
                Vec3::Z,
            );
        }
        for x0 in [-far, -480.0, -320.0, -160.0, 0.0, 160.0, 320.0, 480.0] {
            let x1 = if x0 == -far {
                -480.0
            } else if x0 == 480.0 {
                far
            } else {
                x0 + cell
            };
            quad(
                &mut verts,
                &mut tris,
                [
                    Vec3::new(x0, SIDEWALK_Y, 0.0),
                    Vec3::new(x1, SIDEWALK_Y, 0.0),
                    Vec3::new(x1, SIDEWALK_Y, SIDEWALK_Z),
                    Vec3::new(x0, SIDEWALK_Y, SIDEWALK_Z),
                ],
                Vec3::NEG_Y,
            );
        }
        let mut tables = clipmap_iw4::ClipMeshTables::default();
        tables.verts = verts;
        tables.tri_indices = tris;
        let mesh = SimClipMesh {
            tables: std::sync::Arc::new(tables),
            ..SimClipMesh::default()
        };
        // The car's clip: an axis box around the yawed car, as mappers clip
        // parked cars.
        let yawed = Quat::from_rotation_z(car_yaw.to_radians());
        let reach = (yawed * Vec3::X * 80.0).abs() + (yawed * Vec3::Y * 34.0).abs();
        let e = STREET_EDGE;
        let brushes = vec![
            aabb_brush(
                Vec3::new(-480.0, 300.0, -16.0),
                Vec3::new(480.0, 330.0, 300.0),
            ),
            aabb_brush(Vec3::new(-e - 64.0, -e, -16.0), Vec3::new(-e, e, 600.0)),
            aabb_brush(Vec3::new(e, -e, -16.0), Vec3::new(e + 64.0, e, 600.0)),
            aabb_brush(Vec3::new(-e, -e - 64.0, -16.0), Vec3::new(e, -e, 600.0)),
            aabb_brush(Vec3::new(-e, e, -16.0), Vec3::new(e, e + 64.0, 600.0)),
            aabb_brush(
                car - reach + Vec3::new(0.0, 0.0, -8.0),
                car + reach + Vec3::new(0.0, 0.0, 52.0),
            ),
            aabb_brush(Vec3::new(140.0, 60.0, 0.0), Vec3::new(172.0, 100.0, 50.0)),
        ];
        let mut brushes = brushes;
        // Stairs up from the road: four 12-unit steps, 40 deep.
        for step in 0..4 {
            let x0 = STAIRS_START + step as f32 * 40.0;
            brushes.push(aabb_brush(
                Vec3::new(x0, -480.0, -16.0),
                Vec3::new(STAIRS_START + 160.0, -160.0, 12.0 * (step + 1) as f32),
            ));
        }
        Self {
            brushes,
            bsp: SimClipBsp::default(),
            mesh,
        }
    }

    fn sweep(&self, start: Vec3, end: Vec3, half: Vec3) -> Sweep {
        Sweep::from_trace(&crate::world::clip_trace(
            &self.brushes,
            &self.bsp,
            &self.mesh,
            start.to_array(),
            end.to_array(),
            (-half).to_array(),
            half.to_array(),
            crate::bullet_collision::MASK_PHYS_WORLD,
            &|_| true,
        ))
    }

    /// The terrain triangles alone.
    fn terrain(&self, start: Vec3, end: Vec3, half: Vec3) -> Sweep {
        Sweep::from_trace(&crate::world::clip_trace(
            &[],
            &self.bsp,
            &self.mesh,
            start.to_array(),
            end.to_array(),
            (-half).to_array(),
            half.to_array(),
            crate::bullet_collision::MASK_PHYS_WORLD,
            &|_| true,
        ))
    }

    /// The floor height under the lowest part of a footprint.
    fn floor_under(&self, center: Vec3, half: Vec3) -> f32 {
        [
            (0.0, 0.0),
            (-1.0, -1.0),
            (-1.0, 1.0),
            (1.0, -1.0),
            (1.0, 1.0),
        ]
        .into_iter()
        .map(|(sx, sy)| {
            let (x, y) = (center.x + sx * half.x, center.y + sy * half.y);
            floor_height(x, y, x).max(stairs_height(x, y))
        })
        .fold(f32::INFINITY, f32::min)
    }
}

const RAMP_START: f32 = 160.0;
const RAMP_END: f32 = 480.0;
/// A 25 degree ramp.
const RAMP_TOP: f32 = (RAMP_END - RAMP_START) * 0.466_307_66;
const STAIRS_START: f32 = -480.0;

/// The terrain's height at `x, y`: road, sidewalk, or the ramp. `cell_x`
/// picks the side of a ramp cell edge.
fn floor_height(x: f32, y: f32, cell_x: f32) -> f32 {
    if y >= SIDEWALK_Y {
        SIDEWALK_Z
    } else if (-480.0..-160.0).contains(&y) && (RAMP_START..RAMP_END).contains(&cell_x) {
        (x - RAMP_START).clamp(0.0, RAMP_END - RAMP_START) * 0.466_307_66
    } else {
        0.0
    }
}

fn stairs_height(x: f32, y: f32) -> f32 {
    if !(-480.0..-160.0).contains(&y) || !(STAIRS_START..STAIRS_START + 160.0).contains(&x) {
        return 0.0;
    }
    12.0 * (((x - STAIRS_START) / 40.0).floor() + 1.0)
}

/// A small deterministic generator, so every run throws the same parts.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

#[derive(Clone, Copy)]
struct Part {
    name: &'static str,
    /// Model origin on the car, in the car's frame.
    mount: Vec3,
    mid: Vec3,
    half: Vec3,
    phys: Phys,
    efs: f32,
    force_scale: f32,
}

const DOOR: Phys = Phys {
    mass: 25.0,
    bounce: 0.1,
    friction: 0.25,
};
const HOOD: Phys = Phys {
    mass: 15.0,
    bounce: 0.1,
    friction: 0.25,
};
const BUCKET: Phys = Phys {
    mass: 6.0,
    bounce: 0.1,
    friction: 0.5,
};

/// The small hatch's parts (bounds from the zone), on a car facing +x.
const PARTS: [Part; 5] = [
    Part {
        name: "door_lf",
        mount: Vec3::new(26.0, 33.0, 28.2),
        mid: Vec3::new(-19.67, -3.56, 7.48),
        half: Vec3::new(19.34, 5.04, 23.03),
        phys: DOOR,
        efs: 0.18,
        force_scale: 1.0,
    },
    Part {
        name: "door_rf",
        mount: Vec3::new(26.0, -33.0, 28.2),
        mid: Vec3::new(-19.67, 3.56, 7.48),
        half: Vec3::new(19.34, 5.04, 23.03),
        phys: DOOR,
        efs: 0.18,
        force_scale: 1.0,
    },
    Part {
        name: "hood",
        mount: Vec3::new(38.0, 0.0, 44.0),
        mid: Vec3::new(13.14, 0.0, -5.33),
        half: Vec3::new(17.11, 27.42, 5.46),
        phys: HOOD,
        efs: 0.18,
        force_scale: 1.5,
    },
    Part {
        name: "mirror_l",
        mount: Vec3::new(30.0, 36.0, 39.0),
        mid: Vec3::new(-1.0, 3.0, 0.5),
        half: Vec3::new(2.5, 4.5, 3.0),
        phys: BUCKET,
        efs: 0.18,
        force_scale: 1.0,
    },
    Part {
        name: "mirror_r",
        mount: Vec3::new(30.0, -36.0, 39.0),
        mid: Vec3::new(-1.0, -3.0, 0.5),
        half: Vec3::new(2.5, 4.5, 3.0),
        phys: BUCKET,
        efs: 0.18,
        force_scale: 1.0,
    },
];

/// A PhysicsExplosionSphere: centre, radius, magnitude, at a tick.
#[derive(Clone, Copy)]
struct Blast {
    tick: u32,
    center: Vec3,
    radius: f32,
    magnitude: f32,
}

#[derive(Debug, Default)]
struct Outcome {
    ticks: u32,
    /// Asleep by itself, before the 200-tick cap after the last push.
    asleep: bool,
    /// Deepest the body's bottom went under the floor once it was clear of
    /// the world it may have spawned in.
    below_floor: f32,
    lowest_z: f32,
    final_low: f32,
    final_floor: f32,
    final_embedded: bool,
    /// The largest speed gained over three or more ticks in which the body
    /// barely moved.
    stuck_speed_gain: f32,
    travel: f32,
    flat: bool,
    /// Standing on an edge on the floor: its thinnest axis lies level.
    on_edge: bool,
}

/// Runs `body` the way Mechanics does: a 50 ms step a tick, sleeping when
/// calm or 200 ticks after the last push, with `blasts` pushing it (and
/// waking it if it sleeps).
fn simulate(street: &Street, mut body: RigidBody, efs: f32, blasts: &[Blast]) -> Outcome {
    let start = body.center;
    let mut sweep = |s: Vec3, e: Vec3, h: Vec3| street.sweep(s, e, h);
    let mut out = Outcome {
        lowest_z: f32::INFINITY,
        ..Outcome::default()
    };
    let mut awake = true;
    let mut awake_ticks = 0;
    let mut stuck: Option<(f32, u32)> = None;
    let mut free = false;
    let last_blast = blasts.iter().map(|b| b.tick).max().unwrap_or(0);
    for tick in 1..=(last_blast + 200) {
        let pushed = blasts.iter().any(|b| b.tick == tick);
        for blast in blasts.iter().filter(|b| b.tick == tick) {
            if let Some(impulse) = super::super::mechanics::explosion_impulse(
                body.center,
                blast.center,
                blast.radius,
                0.0,
                blast.magnitude,
                efs,
            ) {
                let toward = -(body.center - blast.center)
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Z);
                body.push(impulse, toward * body.half.min_element(), 0.25);
                awake = true;
                awake_ticks = 0;
            }
        }
        if !awake {
            continue;
        }
        let before = body;
        body.step(0.05, &mut sweep, &mut |s, e, h| street.terrain(s, e, h));
        awake_ticks += 1;
        let half = body.world_half();
        let low = body.center.z - half.z;
        if !free && !sweep(body.center, body.center, body.sweep_half(body.rotation)).start_solid {
            free = true;
        }
        if free {
            out.below_floor = out
                .below_floor
                .max(street.floor_under(body.center, half) - low);
        }
        out.lowest_z = out.lowest_z.min(low);
        // A run of ticks in which the body stays put while its speed
        // grows: speed it is storing up rather than using.
        if !pushed && (body.center - before.center).length() < 0.5 {
            let (from, ticks) = stuck.get_or_insert((before.velocity.length(), 0));
            *ticks += 1;
            if *ticks >= 3 {
                out.stuck_speed_gain = out.stuck_speed_gain.max(body.velocity.length() - *from);
            }
        } else {
            stuck = None;
        }
        out.ticks = tick;
        let calm = body.ready_to_sleep();
        if calm || awake_ticks >= 200 {
            body.settle(&mut sweep);
            out.asleep = calm;
            awake = false;
        }
    }
    let half = body.world_half();
    out.final_low = body.center.z - half.z;
    out.final_floor = street.floor_under(body.center, half);
    out.final_embedded =
        sweep(body.center, body.center, body.sweep_half(body.rotation)).start_solid;
    out.travel = (body.center - start).length();
    out.flat = super::nearest_face(body.rotation, Vec3::Z).is_some();
    let thin = (0..3).fold(0, |best, k| {
        if body.half[k] < body.half[best] {
            k
        } else {
            best
        }
    });
    let axes = Mat3::from_quat(body.rotation);
    let thin_up = [axes.x_axis, axes.y_axis, axes.z_axis][thin].z.abs();
    out.on_edge = thin_up < 0.5 && out.final_low - out.final_floor < 1.0;
    out
}

/// One part through the game's launch: PhysicsLaunchClient from its tag
/// away from tag_body, then 50 ms later the PhysicsExplosionSphere 80 above
/// tag_body, and maybe a second car's blast later on.
fn throw(
    street: &Street,
    car: Vec3,
    car_yaw: f32,
    part: Part,
    draw: f32,
    second: Option<Blast>,
) -> Outcome {
    let yaw = Quat::from_rotation_z(car_yaw.to_radians());
    let tag_body = car + Vec3::new(0.0, 0.0, 40.0);
    let origin = car + yaw * part.mount;
    let force = (origin - tag_body).normalize() * draw * part.force_scale;
    let body = launch(origin, yaw, part, origin, force);
    let mut blasts = vec![Blast {
        tick: 2,
        center: tag_body + Vec3::new(0.0, 0.0, 80.0),
        radius: 247.5,
        magnitude: 2.5,
    }];
    blasts.extend(second);
    simulate(street, body, part.efs, &blasts)
}

fn launch(origin: Vec3, yaw: Quat, part: Part, point: Vec3, force: Vec3) -> RigidBody {
    let axis = Mat3::from_quat(yaw);
    RigidBody::launch(
        origin,
        [
            axis.x_axis.to_array(),
            axis.y_axis.to_array(),
            axis.z_axis.to_array(),
        ],
        part.mid,
        part.half,
        part.phys,
        point,
        force,
    )
}

#[derive(Default)]
struct Tally {
    total: u32,
    slept: u32,
    tick_sum: u32,
    worst_ticks: u32,
    worst_below: f32,
    worst_gain: f32,
    failures: Vec<String>,
}

impl Tally {
    fn check(&mut self, label: String, out: &Outcome, rests_flat: bool) {
        self.total += 1;
        self.tick_sum += out.ticks;
        self.worst_ticks = self.worst_ticks.max(out.ticks);
        self.worst_below = self.worst_below.max(out.below_floor);
        self.worst_gain = self.worst_gain.max(out.stuck_speed_gain);
        if out.asleep {
            self.slept += 1;
        }
        let fell = out.below_floor > 1.0 || out.lowest_z < -2.0;
        let gained = out.stuck_speed_gain > 60.0;
        let unrested = !out.asleep;
        // Tilted with its lowest corner on the floor is balanced on an edge;
        // tilted and higher is leaning on something (the kerb).
        let tipped = rests_flat && !out.flat && out.final_low - out.final_floor < 1.0;
        if fell || gained || out.final_embedded || unrested || tipped {
            self.failures.push(format!(
                "{label}: fell={fell} (below floor {:.1}, lowest z {:.1}) gained={:.0} embedded={} asleep={} flat={} ticks={} final_low={:.1} floor={} travel={:.0}",
                out.below_floor,
                out.lowest_z,
                out.stuck_speed_gain,
                out.final_embedded,
                out.asleep,
                out.flat,
                out.ticks,
                out.final_low,
                out.final_floor,
                out.travel
            ));
        }
    }

    fn report(&self, what: &str) {
        eprintln!(
            "{what}: {} bodies, {} asleep by themselves, mean {:.0} ticks, worst {}, deepest under the floor {:.2}, largest stuck speed gain {:.0}",
            self.total,
            self.slept,
            self.tick_sum as f32 / self.total.max(1) as f32,
            self.worst_ticks,
            self.worst_below,
            self.worst_gain
        );
        assert!(
            self.failures.is_empty(),
            "{} of {}:\n{}",
            self.failures.len(),
            self.total,
            self.failures.join("\n")
        );
    }
}

fn random_car(rng: &mut Lcg, case: usize) -> (Vec3, f32) {
    let car = if case % 4 == 3 {
        // Parked on the ramp, level, as its clip is: the doors on the
        // uphill side spawn half inside the ramp's terrain.
        let (x, y) = (rng.range(220.0, 420.0), rng.range(-420.0, -220.0));
        Vec3::new(x, y, floor_height(x, y, x))
    } else {
        // Anywhere from the middle of the road to hard against the kerb.
        Vec3::new(rng.range(-160.0, 160.0), rng.range(-220.0, 120.0), 0.0)
    };
    (car, rng.range(0.0, 360.0))
}

#[test]
fn thrown_car_parts_stay_on_top_of_the_street_and_come_to_rest() {
    let mut rng = Lcg(0x5eed_ca75);
    let mut tally = Tally::default();
    let (mut flat_parts, mut on_edge) = (0u32, 0u32);
    for case in 0..120 {
        let (car, car_yaw) = random_car(&mut rng, case);
        let street = Street::new(car, car_yaw);
        // Every other car has a neighbour go up while its parts settle.
        let second = (case % 2 == 1).then(|| Blast {
            tick: 10 + (rng.range(0.0, 50.0) as u32),
            center: car + Vec3::new(rng.range(-150.0, 150.0), rng.range(-150.0, 150.0), 120.0),
            radius: 247.5,
            magnitude: 2.5,
        });
        for part in PARTS {
            let draw = rng.range(4000.0, 5000.0);
            let out = throw(&street, car, car_yaw, part, draw, second);
            let label = format!(
                "case {case} {} car=({:.0},{:.0}) yaw={car_yaw:.0} draw={draw:.0} second={}",
                part.name,
                car.x,
                car.y,
                second.is_some()
            );
            if part.phys.mass >= HOOD.mass {
                flat_parts += 1;
                on_edge += u32::from(out.on_edge);
            }
            tally.check(label, &out, false);
        }
    }
    tally.report("car parts");
    // Doors and hoods end lying down; a few may stand leaning on the car or
    // a wall, but not standing free on an edge as a matter of course.
    eprintln!("doors and hoods standing on an edge on the floor: {on_edge} of {flat_parts}");
    assert!(on_edge * 30 <= flat_parts, "{on_edge} of {flat_parts}");
}

/// A part spawned inside a clip with a slow or downward push: it has to
/// leave sideways or upward, never through the ground under the clip.
#[test]
fn a_slow_part_pushed_down_inside_a_clip_leaves_it_above_the_ground() {
    let mut rng = Lcg(0xb0c5);
    let mut tally = Tally::default();
    // The box beside the road, as the newspaper boxes are: its doors are
    // pushed out with 860 to 2500 and a mass of 6.
    let newspaper_door = Part {
        name: "newspaper_door",
        mount: Vec3::ZERO,
        mid: Vec3::new(0.0, -6.0, 12.0),
        half: Vec3::new(1.0, 7.0, 12.0),
        phys: BUCKET,
        efs: 0.18,
        force_scale: 1.0,
    };
    for case in 0..80 {
        let car = Vec3::new(rng.range(-160.0, 160.0), rng.range(-220.0, 40.0), 0.0);
        let car_yaw = rng.range(0.0, 360.0);
        let street = Street::new(car, car_yaw);
        let yaw = Quat::from_rotation_z(car_yaw.to_radians());
        let part = PARTS[case % 3];
        let origin = car + yaw * part.mount;
        // Down and out of the car at 30 to 70 degrees, 20 to 200 u/s.
        let out_dir = (origin - car).truncate().normalize_or_zero().extend(0.0);
        let down = rng.range(30.0, 70.0).to_radians();
        let direction = out_dir * down.cos() - Vec3::Z * down.sin();
        let force = direction * rng.range(20.0, 200.0) * part.phys.mass;
        let out = simulate(
            &street,
            launch(origin, yaw, part, origin, force),
            part.efs,
            &[],
        );
        tally.check(
            format!(
                "case {case} slow {} car=({:.0},{:.0}) yaw={car_yaw:.0}",
                part.name, car.x, car.y
            ),
            &out,
            false,
        );
        // A newspaper box door, mounted inside the box.
        let door_at = Vec3::new(
            rng.range(142.0, 170.0),
            rng.range(62.0, 98.0),
            rng.range(6.0, 30.0),
        );
        let door_yaw = Quat::from_rotation_z(rng.range(0.0, 360.0f32).to_radians());
        let push = Vec3::new(
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(-0.8, 0.2),
        )
        .normalize_or_zero()
            * rng.range(860.0, 2500.0);
        let out = simulate(
            &street,
            launch(door_at, door_yaw, newspaper_door, door_at, push),
            newspaper_door.efs,
            &[Blast {
                tick: 2,
                center: Vec3::new(156.0, 80.0, 90.0),
                radius: 200.0,
                magnitude: 2.5,
            }],
        );
        tally.check(
            format!("case {case} newspaper door at {door_at} push {push}"),
            &out,
            false,
        );
    }
    tally.report("slow and downward launches inside clips");
}

/// The care package: dropped from the helicopter with a small push from the
/// world origin, tilted, onto the street. It lands, rests on a face, and
/// sleeps (physics_finished).
#[test]
fn a_care_package_falls_lands_flat_and_sleeps() {
    let mut rng = Lcg(0xca5e);
    let mut tally = Tally::default();
    let crate_part = Part {
        name: "care_package",
        mount: Vec3::ZERO,
        mid: Vec3::new(0.0, 0.0, 15.0),
        half: Vec3::new(32.0, 17.0, 15.0),
        phys: Phys {
            mass: 1.0,
            bounce: 0.1,
            friction: 0.5,
        },
        efs: 1.0,
        force_scale: 1.0,
    };
    let street = Street::new(Vec3::new(0.0, -2000.0, 0.0), 0.0);
    for case in 0..40 {
        // On open road, clear of the kerb, the ramp, the stairs and the box
        // beside the road: a crate tumbling into one may lean on it.
        let at = Vec3::new(
            rng.range(-280.0, 40.0),
            rng.range(-140.0, 110.0),
            rng.range(800.0, 2000.0),
        );
        let tilt = Quat::from_rotation_z(rng.range(0.0, 360.0f32).to_radians())
            * Quat::from_rotation_y(rng.range(-40.0, 40.0f32).to_radians());
        let push = Vec3::new(
            rng.range(-25.0, 25.0),
            rng.range(-25.0, 25.0),
            rng.range(-12.0, 12.0),
        );
        let out = simulate(
            &street,
            launch(at, tilt, crate_part, Vec3::ZERO, push),
            crate_part.efs,
            &[],
        );
        tally.check(format!("case {case} crate from {at}"), &out, true);
    }
    tally.report("care packages");
}

#[test]
fn a_box_dropped_on_the_road_and_the_sidewalk_lands_on_top() {
    let street = Street::new(Vec3::new(0.0, -300.0, 0.0), 0.0);
    let mut sweep = |s: Vec3, e: Vec3, h: Vec3| street.sweep(s, e, h);
    for (at, floor) in [
        (Vec3::new(-200.0, 0.0, 100.0), 0.0),
        (Vec3::new(-200.0, 240.0, 100.0), SIDEWALK_Z),
    ] {
        let mut body = RigidBody::launch(
            at,
            math_iw4::angles_to_axis([0.0, 30.0, 0.0]),
            Vec3::ZERO,
            Vec3::new(20.0, 12.0, 6.0),
            DOOR,
            at,
            Vec3::ZERO,
        );
        for _ in 0..200 {
            body.step(0.05, &mut sweep, &mut |s, e, h| street.terrain(s, e, h));
            if body.ready_to_sleep() {
                break;
            }
        }
        body.settle(&mut sweep);
        let low = body.center.z - body.world_half().z;
        assert!(
            (floor..=floor + 0.5).contains(&low),
            "lands on the floor at {floor}: {low}"
        );
    }
}
