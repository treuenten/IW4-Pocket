//! The step a move reports for the client's first-person step smoothing:
//! how far its final height differs from the end of its plain slide.

use movement_iw4::{
    AirMoveContext, CmdScaleWalkContext, CollisionBackend, GroundTraceInput, JumpLaunchContext,
    MoveBounds, Pml, WalkMoveContext, step_slide_move, walk_move,
};
use playerstate_iw4::{ENTITYNUM_NONE, PM_TYPE_DEAD, PlayerState, UserCmd, pm_flags};
use trace_iw4::{BrushRef, Trace, trace_box};

const MINS: [f32; 3] = [-15.0, -15.0, 0.0];
const MAXS: [f32; 3] = [15.0, 15.0, 70.0];
const MASK: u32 = 0x0281_0011;
const FLAT: [f32; 3] = [0.0, 0.0, 1.0];

/// Brushes, each an axial box plus any extra cutting planes.
struct Brushes(Vec<Vec<[f32; 4]>>);

impl Brushes {
    /// Ground at z 0 everywhere around the origin.
    fn floor() -> Self {
        Self(Vec::new()).with_box([-2000.0, -2000.0, -64.0], [2000.0, 2000.0, 0.0], &[])
    }

    fn with_box(mut self, min: [f32; 3], max: [f32; 3], extra: &[[f32; 4]]) -> Self {
        let mut planes = vec![
            [1.0, 0.0, 0.0, max[0]],
            [-1.0, 0.0, 0.0, -min[0]],
            [0.0, 1.0, 0.0, max[1]],
            [0.0, -1.0, 0.0, -min[1]],
            [0.0, 0.0, 1.0, max[2]],
            [0.0, 0.0, -1.0, -min[2]],
        ];
        planes.extend_from_slice(extra);
        self.0.push(planes);
        self
    }

    /// An eight-unit box whose front face is one unit ahead of a player at
    /// the origin.
    fn eight_unit_box() -> Self {
        Self::floor().with_box([16.0, -64.0, 0.0], [400.0, 64.0, 8.0], &[])
    }

    /// Rests the player's box on whatever is just below `origin`.
    fn settle(&self, origin: [f32; 3]) -> [f32; 3] {
        let [x, y, z] = origin;
        let hit = self.trace(GroundTraceInput {
            start: [x, y, z + 2.0],
            end: [x, y, z - 16.0],
            mins: MINS,
            maxs: MAXS,
            tracemask: MASK,
        });
        assert!(
            hit.fraction < 1.0 && hit.startsolid == 0,
            "no ground under {origin:?}"
        );
        hit.endpos
    }
}

impl CollisionBackend for Brushes {
    fn trace(&self, input: GroundTraceInput) -> Trace {
        trace_box(
            self.0.iter().map(|planes| BrushRef {
                planes,
                contents: 1,
                plane_surface_flags: &[],
                glass_encoded: 0,
            }),
            input.start,
            input.end,
            input.mins,
            input.maxs,
            input.tracemask,
        )
    }
}

fn pml(ground: Option<[f32; 3]>) -> Pml {
    let mut ground_trace = [0; 11];
    if let Some(n) = ground {
        ground_trace[1] = n[0].to_bits();
        ground_trace[2] = n[1].to_bits();
        ground_trace[3] = n[2].to_bits();
    }
    let on_ground = u32::from(ground.is_some());
    Pml {
        forward: [1.0, 0.0, 0.0],
        right: [0.0, -1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        frametime: 0.033,
        msec: 33,
        walking: on_ground,
        ground_plane: on_ground,
        almost_ground_plane: on_ground,
        ground_trace,
        previous_origin: [0.0; 3],
        previous_velocity: [0.0; 3],
        holdrand: 0,
        jump_animations: [None; 4],
        mantle_movetype: None,
        landing_animation: false,
        fall_damage: 0,
        step_view_change: 0.0,
    }
}

fn player(origin: [f32; 3], velocity: [f32; 3], supported: bool) -> PlayerState {
    let mut ps = PlayerState::ZERO;
    ps.origin = origin;
    ps.velocity = velocity;
    ps.gravity = 800;
    ps.speed = 190;
    ps.move_speed_scale_multiplier = 1.0;
    ps.ground_entity_num = if supported { 0x7fe } else { ENTITYNUM_NONE };
    ps
}

/// One step_slide_move: the reported step and the final origin.
fn step(world: &Brushes, ps: &mut PlayerState, ground: Option<[f32; 3]>) -> (f32, [f32; 3]) {
    let gravity = ground.is_none().then_some(800.0);
    let change = step_slide_move(ps, &pml(ground), world, MINS, MAXS, MASK, gravity);
    (change, ps.origin)
}

#[test]
fn walking_onto_an_eight_unit_box_reports_the_step_up() {
    let world = Brushes::eight_unit_box();
    let start = world.settle([0.0; 3]);
    let mut ps = player(start, [190.0, 0.0, 0.0], true);
    let (change, origin) = step(&world, &mut ps, Some(FLAT));
    assert!((origin[2] - start[2] - 8.0).abs() < 0.01, "{origin:?}");
    assert!((change - 8.0).abs() < 0.01, "{change}");
}

#[test]
fn walking_on_flat_ground_reports_nothing() {
    let world = Brushes::floor();
    let mut ps = player(world.settle([0.0; 3]), [190.0, 0.0, 0.0], true);
    let (change, origin) = step(&world, &mut ps, Some(FLAT));
    assert!(origin[0] > 6.0, "{origin:?}");
    assert_eq!(change, 0.0);
}

/// A 20 degree brush ramp rising along +x, and its normal.
fn ramp() -> (Brushes, [f32; 3], f32) {
    let (sin, cos) = 20.0_f32.to_radians().sin_cos();
    let n = [-sin, 0.0, cos];
    let world = Brushes::floor().with_box(
        [0.0, -200.0, -64.0],
        [1000.0, 200.0, 400.0],
        &[[n[0], n[1], n[2], 0.0]],
    );
    (world, n, sin / cos)
}

/// Walks a few moves along the ramp and returns each move's report. On this
/// brush the plain slide stops against the ramp it moves along and the step
/// route finishes the move, so this also covers that case: the ramp's rise is
/// not a step.
fn walk_the_ramp(direction: f32) -> Vec<f32> {
    let (world, n, slope) = ramp();
    let (sin, cos) = 20.0_f32.to_radians().sin_cos();
    let start = world.settle([100.0, 0.0, 100.0 * slope + 2.0]);
    let mut ps = player(
        start,
        [direction * 190.0 * cos, 0.0, direction * 190.0 * sin],
        true,
    );
    let changes = (0..8).map(|_| step(&world, &mut ps, Some(n)).0).collect();
    let climbed = ps.origin[2] - start[2];
    assert!(
        climbed * direction > 8.0 * 0.033 * 190.0 * sin * 0.9,
        "the player followed the ramp: {climbed}"
    );
    changes
}

#[test]
fn walking_up_a_ramp_reports_nothing() {
    assert_eq!(walk_the_ramp(1.0), vec![0.0; 8]);
}

#[test]
fn walking_down_a_ramp_reports_nothing() {
    assert_eq!(walk_the_ramp(-1.0), vec![0.0; 8]);
}

#[test]
fn walking_over_the_top_of_a_ramp_reports_nothing() {
    // A 45 degree ramp up to a flat top at z 40. The move that crosses the
    // top slides on up the ramp's plane past the edge, and the snap puts it
    // down on the top: below where its slide ended, but higher than where the
    // move started, so there is no drop to hide.
    let (sin, cos) = 45.0_f32.to_radians().sin_cos();
    let n = [-sin, 0.0, cos];
    let world = Brushes::floor().with_box(
        [0.0, -200.0, -64.0],
        [1000.0, 200.0, 40.0],
        &[[n[0], n[1], n[2], 0.0]],
    );
    // From three starting points, so one of them crosses the top late in a
    // move and slides well past the edge.
    for x in [0.0, 1.5, 3.0] {
        let start = world.settle([x, 0.0, 20.0]);
        let mut ps = player(start, [190.0 * cos, 0.0, 190.0 * sin], true);
        let mut changes = Vec::new();
        for _ in 0..20 {
            let ground = if ps.origin[2] < 39.0 { n } else { FLAT };
            changes.push(step(&world, &mut ps, Some(ground)).0);
        }
        assert!(
            (ps.origin[2] - 40.0).abs() < 0.5,
            "from x {x} the player ended on the top: {:?}",
            ps.origin
        );
        assert_eq!(changes, vec![0.0; 20], "from x {x}");
    }
}

#[test]
fn walking_off_a_five_unit_ledge_reports_the_snap_down() {
    // The ledge ends at x 0; the player's box leaves it during the move and
    // the snap puts it on the ground below.
    let world = Brushes::floor().with_box([-400.0, -64.0, 0.0], [0.0, 64.0, 5.0], &[]);
    let start = world.settle([14.0, 0.0, 5.0]);
    let mut ps = player(start, [190.0, 0.0, 0.0], true);
    let (change, origin) = step(&world, &mut ps, Some(FLAT));
    assert!((start[2] - origin[2] - 5.0).abs() < 0.01, "{origin:?}");
    assert!((change + 5.0).abs() < 0.01, "{change}");
}

#[test]
fn an_airborne_move_reports_nothing() {
    let world = Brushes::eight_unit_box();
    let mut ps = player([0.0, 0.0, 2.0], [190.0, 0.0, 0.0], false);
    let (change, _) = step(&world, &mut ps, None);
    assert_eq!(change, 0.0);
}

#[test]
fn a_ladder_move_reports_nothing() {
    let world = Brushes::eight_unit_box();
    let start = world.settle([0.0; 3]);
    let mut ps = player(start, [190.0, 0.0, 60.0], true);
    ps.pm_flags |= pm_flags::LADDER;
    let (change, origin) = step(&world, &mut ps, Some(FLAT));
    assert!(
        origin[2] > start[2] + 1.0,
        "the ladder move still climbs: {origin:?}"
    );
    assert_eq!(change, 0.0);
}

#[test]
fn a_dead_player_reports_nothing() {
    let world = Brushes::eight_unit_box();
    let mut ps = player(world.settle([0.0; 3]), [190.0, 0.0, 0.0], true);
    ps.pm_type = PM_TYPE_DEAD;
    let (change, _) = step(&world, &mut ps, Some(FLAT));
    assert_eq!(change, 0.0);
}

#[test]
fn walk_move_records_the_step_in_pml() {
    let world = Brushes::eight_unit_box();
    let start = world.settle([0.0; 3]);
    let mut ps = player(start, [190.0, 0.0, 0.0], true);
    let mut pml = pml(Some(FLAT));
    let mut cmd = UserCmd {
        server_time: 1033,
        forwardmove: 127,
        ..UserCmd::default()
    };
    let air = AirMoveContext {
        player_spectate_speed_scale: 1.0,
        shellshock_gravity_scale: 1.0,
        shellshock_gravity_bias: 0.0,
    };
    let context = WalkMoveContext {
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
        old_buttons: 0,
        jump: JumpLaunchContext {
            jump_height: 39.0,
            dive: false,
            crouch_jump_scale: 1.0,
            jump_ladder_push_vel: 128.0,
        },
        air,
    };
    let bounds = MoveBounds {
        mins: MINS,
        maxs: MAXS,
        tracemask: MASK,
    };
    walk_move(&mut ps, &mut pml, &mut cmd, context, bounds, &world);
    assert!(
        (ps.origin[2] - start[2] - 8.0).abs() < 0.01,
        "{:?}",
        ps.origin
    );
    assert!(
        (pml.step_view_change - 8.0).abs() < 0.01,
        "{}",
        pml.step_view_change
    );
}
