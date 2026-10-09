//! Walks pmove up a steep staircase of open plank treads made only of
//! collision mesh, the way Estate's cliff stairs are built: the capsule's
//! rounded bottom rides the tread edges, which the mesh flags walkable.

use clipmap_iw4::{ClipMeshRef, TraceExtents, trace_through_mesh};
use movement_iw4::{
    AdsFracContext, AdsIntentContext, AirMoveContext, CmdScaleWalkContext, CollisionBackend,
    FlatMantleAnimLength, GroundTraceInput, JumpLaunchContext, MeleeChargeWeaponDelays, MoveBounds,
    PmoveSingleContext, SprintContext, ViewAngleClamp, WalkMoveContext, ZeroMantleRootDelta,
};
use playerstate_iw4::{ENTITYNUM_NONE, PlayerState, UserCmd};
use trace_iw4::Trace;

const RISE: f32 = 10.5;
const RUN: f32 = 10.0;
const TREAD_DEPTH: f32 = 9.0;
const TREAD_THICKNESS: f32 = 2.0;
const TREADS: usize = 12;
const HALF_WIDTH: f32 = 48.0;
const WALK_SPEED: f32 = 190.0;
const SPRINT_SPEED: f32 = 1.5 * WALK_SPEED;

#[derive(Default)]
struct MeshBuilder {
    verts: Vec<[f32; 3]>,
    indices: Vec<u16>,
    walkable_edges: Vec<u8>,
}

impl MeshBuilder {
    fn tri_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// One triangle facing `outward`; the mesh trace only collides with the
    /// side its winding faces.
    fn tri(&mut self, mut v: [[f32; 3]; 3], outward: [f32; 3], walkable: bool) {
        let a = sub(v[2], v[0]);
        let b = sub(v[1], v[0]);
        if dot(cross(a, b), outward) < 0.0 {
            v.swap(1, 2);
        }
        let ti = self.tri_count();
        for p in v {
            self.indices.push(self.verts.len() as u16);
            self.verts.push(p);
        }
        if walkable {
            for k in 0..3 {
                let bit = ti * 3 + k;
                if self.walkable_edges.len() <= bit >> 3 {
                    self.walkable_edges.resize((bit >> 3) + 1, 0);
                }
                self.walkable_edges[bit >> 3] |= 1 << (bit & 7);
            }
        }
    }

    fn quad(&mut self, q: [[f32; 3]; 4], outward: [f32; 3], walkable: bool) {
        self.tri([q[0], q[1], q[2]], outward, walkable);
        self.tri([q[0], q[2], q[3]], outward, walkable);
    }

    /// An axis-aligned slab. Like the planks on Estate, every edge of its
    /// top face is flagged walkable and the sides are not.
    fn slab(&mut self, min: [f32; 3], max: [f32; 3]) {
        let [x0, y0, z0] = min;
        let [x1, y1, z1] = max;
        let top = [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]];
        let bottom = [[x0, y0, z0], [x1, y0, z0], [x1, y1, z0], [x0, y1, z0]];
        self.quad(top, [0.0, 0.0, 1.0], true);
        self.quad(bottom, [0.0, 0.0, -1.0], false);
        self.quad(
            [[x0, y0, z0], [x0, y1, z0], [x0, y1, z1], [x0, y0, z1]],
            [-1.0, 0.0, 0.0],
            false,
        );
        self.quad(
            [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]],
            [1.0, 0.0, 0.0],
            false,
        );
        self.quad(
            [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]],
            [0.0, -1.0, 0.0],
            false,
        );
        self.quad(
            [[x0, y1, z0], [x1, y1, z0], [x1, y1, z1], [x0, y1, z1]],
            [0.0, 1.0, 0.0],
            false,
        );
    }
}

/// Ground at z 0, then open treads climbing +x at about 46 degrees with a
/// one-unit gap between planks, then a landing.
fn staircase() -> MeshBuilder {
    let mut mesh = MeshBuilder::default();
    let top = TREADS as f32 * RUN;
    mesh.slab(
        [-400.0, -4.0 * HALF_WIDTH, -16.0],
        [top + 400.0, 4.0 * HALF_WIDTH, 0.0],
    );
    for i in 1..=TREADS {
        let x = (i - 1) as f32 * RUN;
        let z = i as f32 * RISE;
        mesh.slab(
            [x, -HALF_WIDTH, z - TREAD_THICKNESS],
            [x + TREAD_DEPTH, HALF_WIDTH, z],
        );
    }
    let landing = (TREADS + 1) as f32 * RISE;
    mesh.slab(
        [top, -HALF_WIDTH, landing - TREAD_THICKNESS],
        [top + 400.0, HALF_WIDTH, landing],
    );
    mesh
}

struct MeshCollision<'a> {
    mesh: ClipMeshRef<'a>,
}

impl CollisionBackend for MeshCollision<'_> {
    fn trace(&self, input: GroundTraceInput) -> Trace {
        let ext = TraceExtents::new(
            input.start,
            input.end,
            input.mins,
            input.maxs,
            input.tracemask,
        );
        trace_through_mesh(&self.mesh, &ext)
    }
}

fn context(old_buttons: u32) -> PmoveSingleContext {
    let air = AirMoveContext {
        player_spectate_speed_scale: 1.0,
        shellshock_gravity_scale: 1.0,
        shellshock_gravity_bias: 0.0,
    };
    PmoveSingleContext {
        walk: WalkMoveContext {
            cmd_scale: CmdScaleWalkContext {
                player_back_speed_scale: 0.7,
                player_strafe_speed_scale: 0.8,
                player_sprint_speed_scale: 1.5,
                player_last_stand_crawl_speed_scale: 0.15,
                weapon_move_speed_scale: 1.0,
                weapon_ads_move_speed_scale: 1.0,
                shellshock_affects_movement: false,
            },
            weapon_move_scale: 1.0,
            old_buttons,
            jump: JumpLaunchContext {
                jump_height: 39.0,
                dive: false,
                crouch_jump_scale: 1.0,
                jump_ladder_push_vel: 128.0,
            },
            air,
        },
        air,
        bounds: MoveBounds {
            mins: [-15.0, -15.0, 0.0],
            maxs: [15.0, 15.0, 70.0],
            tracemask: 0x0281_0011,
        },
        view_angles: ViewAngleClamp {
            pitch_up: 85.0,
            pitch_down: 85.0,
            unclamped_pitch_bit: false,
        },
        sprint: SprintContext {
            weapon_max_sprint_time: movement_iw4::get_max_sprint_time(1.0, 4.0),
            sprint_forever: false,
            min_sprint_time_seconds: 1.0,
            sprint_delay_seconds: 0.0,
            sprint_forward_minimum: 105,
            stand_up_clear: true,
            sprint_recharge_pause_seconds: 0.0,
        },
        ads_intent: AdsIntentContext {
            ads_allowed: false,
            weapon_def_scope: false,
            sprint_hold_ads: false,
        },
        ads_frac: AdsFracContext {
            aim_down_sight: false,
            ads_in_rate: 1.0 / 200.0,
            ads_out_rate: 1.0 / 200.0,
            ads_reload_trans_time_ms: 0,
            segmented_reload: false,
            rechamber_while_ads: true,
            ads_fire_only: false,
        },
        melee_charge: MeleeChargeWeaponDelays::default(),
        player_melee_range: movement_iw4::MELEE_CHARGE_PLAYER_MELEE_RANGE_DEFAULT,
        old_buttons,
        weapon_blocks_prone: false,
        can_hold_breath: false,
    }
}

fn spawn(origin: [f32; 3]) -> PlayerState {
    let mut ps = PlayerState::ZERO;
    ps.origin = origin;
    ps.speed = WALK_SPEED as i32;
    ps.health = 100;
    ps.max_health = 100;
    ps.move_speed_scale_multiplier = 1.0;
    ps.view_height_target = 60;
    ps.view_height_current = 60.0;
    ps.gravity = 800;
    ps.ground_entity_num = 0x7fe;
    ps.other_flags |= playerstate_iw4::other_flags::PLAYER;
    ps.corpse_index = -1;
    ps.command_time = 1000;
    ps
}

struct Flight {
    commands: usize,
    airborne: usize,
    average_speed: f32,
    /// The highest the origin rose above the treads under the capsule.
    highest_above_treads: f32,
    finished: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Way {
    Up,
    Down,
}

/// The top of the highest tread under a capsule centred at `x`.
fn treads_under(x: f32) -> f32 {
    (1..=TREADS)
        .filter(|&i| {
            let x0 = (i - 1) as f32 * RUN;
            x0 - 15.0 <= x && x <= x0 + TREAD_DEPTH + 15.0
        })
        .map(|i| i as f32 * RISE)
        .fold(0.0, f32::max)
}

/// Holds forward from one end of the stairs to the other, walking or
/// sprinting, at a fixed command length, and measures the commands that
/// start and end on the flight.
fn run_flight(msec: i32, way: Way, sprint: bool) -> Flight {
    let built = staircase();
    let mut mesh = ClipMeshRef::from_linear(&built.verts, &built.indices, &[]);
    mesh.tri_edge_is_walkable = &built.walkable_edges;
    let collision = MeshCollision { mesh };
    let lengths = FlatMantleAnimLength::default();
    let root = ZeroMantleRootDelta;

    let top = TREADS as f32 * RUN;
    let landing = (TREADS + 1) as f32 * RISE;
    let (mut ps, yaw) = match way {
        Way::Up => (spawn([-40.0, 0.0, 0.0]), 0.0),
        Way::Down => (spawn([top + 60.0, 0.0, landing]), 180.0),
    };
    let mut time = ps.command_time;
    let mut old_buttons = 0;
    let flight = (RUN, (TREADS - 1) as f32 * RUN);
    let (mut commands, mut airborne, mut distance, mut elapsed) = (0, 0, 0.0, 0);
    let mut highest_above_treads = f32::MIN;
    for _ in 0..(4000 / msec) {
        time += msec;
        let mut cmd = UserCmd {
            server_time: time,
            forwardmove: 127,
            buttons: if sprint {
                playerstate_iw4::buttons::SPRINT
            } else {
                0
            },
            angles: [0, (yaw * movement_iw4::ANGLE2SHORT) as i32, 0],
            ..UserCmd::default()
        };
        let before = ps.origin;
        movement_iw4::pmove(
            &mut ps,
            &mut cmd,
            context(old_buttons),
            &collision,
            &lengths,
            &root,
        );
        old_buttons = cmd.buttons;
        let on_flight = |x: f32| x >= flight.0 && x <= flight.1;
        if on_flight(before[0]) && on_flight(ps.origin[0]) {
            commands += 1;
            airborne += usize::from(ps.ground_entity_num == ENTITYNUM_NONE);
            distance += (ps.origin[0] - before[0]).hypot(ps.origin[1] - before[1]);
            elapsed += msec;
            highest_above_treads =
                highest_above_treads.max(ps.origin[2] - treads_under(ps.origin[0]));
        }
        let past = match way {
            Way::Up => ps.origin[0] > flight.1 + 4.0 * RUN,
            Way::Down => ps.origin[0] < -40.0,
        };
        if past {
            break;
        }
    }
    let finished = match way {
        Way::Up => ps.origin[0] > flight.1 && (ps.origin[2] - landing).abs() < 0.5,
        Way::Down => ps.origin[0] < 0.0 && ps.origin[2].abs() < 0.5,
    };
    Flight {
        commands,
        airborne,
        average_speed: distance * 1000.0 / elapsed.max(1) as f32,
        highest_above_treads,
        finished,
    }
}

#[test]
fn walking_up_open_treads_stays_on_the_ground() {
    for msec in [16, 25, 33] {
        let climb = run_flight(msec, Way::Up, false);
        assert!(climb.finished, "{msec} ms: never reached the landing");
        assert!(climb.commands > 0, "{msec} ms: no commands on the flight");
        assert_eq!(
            climb.airborne, 0,
            "{msec} ms: {} of {} commands on the flight were airborne",
            climb.airborne, climb.commands
        );
        assert!(
            climb.average_speed >= 0.65 * WALK_SPEED,
            "{msec} ms: {:.1} u/s on the flight",
            climb.average_speed
        );
    }
}

/// Coming down, the landing rule must not strip the speed the player carries
/// off each tread nose (sprinting down fell to 163 u/s at 16 ms when it did),
/// and the player must follow the treads rather than fly over them.
#[test]
fn going_down_open_treads_follows_them_at_speed() {
    for sprint in [false, true] {
        let (what, speed) = if sprint {
            ("sprinting", SPRINT_SPEED)
        } else {
            ("walking", WALK_SPEED)
        };
        for msec in [16, 25, 33] {
            let descent = run_flight(msec, Way::Down, sprint);
            assert!(descent.finished, "{what} {msec} ms: never reached the foot");
            assert!(
                descent.commands > 0,
                "{what} {msec} ms: no commands on the flight"
            );
            assert_eq!(
                descent.airborne, 0,
                "{what} {msec} ms: {} of {} commands on the flight were airborne",
                descent.airborne, descent.commands
            );
            assert!(
                descent.highest_above_treads < 4.0,
                "{what} {msec} ms: {:.1} above the treads",
                descent.highest_above_treads
            );
            assert!(
                descent.average_speed >= 0.6 * speed,
                "{what} {msec} ms: {:.1} u/s on the flight",
                descent.average_speed
            );
        }
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
