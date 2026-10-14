use core::sync::atomic::{AtomicBool, Ordering};

use crate::flags::{elem_uses_vel_local, elem_uses_vel_world};
use crate::integrate::sample_vel_graph_at_age;

/// Milliseconds to seconds: a graph's speeds are in units a millisecond.
pub const FX_VEL_AT_TIME_SCALE: f64 = 1000.0;

/// The effect motion fixes of IP-129, IP-089, IP-114 and IP-148
/// (`IW4L_FX_MOTION`, on unless 0): an element's velocity is read at the speed
/// it really moves, a model-physics piece turns about its middle and tips onto
/// a face when it lands, a glass shard leaves at its piece's own speed, and an
/// element moves from its birth rather than from its effect's last update.
/// Off restores the old behaviour of each, for comparing runs.
static MOTION_FIX: AtomicBool = AtomicBool::new(true);

/// Set once at startup, before an effect plays.
pub fn set_motion_fix(on: bool) {
    MOTION_FIX.store(on, Ordering::Relaxed);
}

pub fn motion_fix() -> bool {
    MOTION_FIX.load(Ordering::Relaxed)
}

/// An element's velocity in units a second at `age_msec`: `base_vel` plus its
/// velocity graphs there.
///
/// A graph sample is the distance covered per interval as a share of the
/// life (`integrate_velocity_graph` moves the element by it), so its speed is
/// the sample times the interval count. This velocity is what a collision
/// reflects, so it must match how far the graphs really move the element:
/// read at a 1000th of that per interval, a leaf pushed into a wall by a
/// 43-interval graph got 1/43 of its push reflected and stayed pressed
/// against the wall in mid-air (IP-089). With `IW4L_FX_MOTION=0` the graph
/// is read at the old 1000 a sample.
#[inline]
pub fn get_velocity_at_time(
    flags: i32,
    base_vel: [f32; 3],
    age_msec: f32,
    life_ms: f32,
    local_samples: &[crate::FxElemVec3Range],
    world_samples: &[crate::FxElemVec3Range],
    orient_axis: [[f32; 3]; 3],
    seed: u64,
) -> [f32; 3] {
    let mut out = base_vel;
    let age01 = if life_ms > 0.0 {
        let t = age_msec / life_ms;
        if t < 0.0 {
            0.0
        } else if t > 1.0 {
            1.0
        } else {
            t
        }
    } else {
        0.0
    };
    let fix = motion_fix();
    let scale_of = |fenceposts: usize| {
        if fix {
            graph_speed_scale(fenceposts)
        } else {
            FX_VEL_AT_TIME_SCALE as f32
        }
    };

    if elem_uses_vel_world(flags) && world_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(world_samples, age01, seed);
        let scale = scale_of(world_samples.len());
        out[0] += s[0] * scale;
        out[1] += s[1] * scale;
        out[2] += s[2] * scale;
    }
    if elem_uses_vel_local(flags) && local_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(local_samples, age01, seed);
        let scale = scale_of(local_samples.len());

        let wx = s[0] * orient_axis[0][0] + s[1] * orient_axis[1][0] + s[2] * orient_axis[2][0];
        let wy = s[0] * orient_axis[0][1] + s[1] * orient_axis[1][1] + s[2] * orient_axis[2][1];
        let wz = s[0] * orient_axis[0][2] + s[1] * orient_axis[1][2] + s[2] * orient_axis[2][2];
        out[0] += wx * scale;
        out[1] += wy * scale;
        out[2] += wz * scale;
    }
    out
}

/// The velocity an element leaves its spawn point with, in units a second: its
/// velocity graphs at age 0. A graph sample is the distance covered per
/// interval as a share of the life (see `integrate_velocity_graph`), so the
/// speed is the sample times the interval count. A model-physics piece is
/// launched at this velocity, and its graphs play no further part.
#[inline]
pub fn elem_launch_velocity(
    flags: i32,
    local_samples: &[crate::FxElemVec3Range],
    world_samples: &[crate::FxElemVec3Range],
    orient_axis: [[f32; 3]; 3],
    seed: u64,
) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    if elem_uses_vel_world(flags) && world_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(world_samples, 0.0, seed);
        let scale = graph_speed_scale(world_samples.len());
        for axis in 0..3 {
            out[axis] += s[axis] * scale;
        }
    }
    if elem_uses_vel_local(flags) && local_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(local_samples, 0.0, seed);
        let scale = graph_speed_scale(local_samples.len());
        for axis in 0..3 {
            let w = s[0] * orient_axis[0][axis]
                + s[1] * orient_axis[1][axis]
                + s[2] * orient_axis[2][axis];
            out[axis] += w * scale;
        }
    }
    out
}

fn graph_speed_scale(fenceposts: usize) -> f32 {
    (fenceposts - 1) as f32 * FX_VEL_AT_TIME_SCALE as f32
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::FxElemVec3Range;
    use crate::flags::FX_ELEM_VEL_WORLD;
    use crate::integrate::integrate_velocity_graph;
    use alloc::vec;
    use alloc::vec::Vec;

    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    /// explosions/powerlines_a elem 0: 29 intervals of 0.003448 (1/290), an
    /// authored 100 units a second.
    fn powerlines_graph() -> Vec<FxElemVec3Range> {
        vec![
            FxElemVec3Range {
                base: [0.0, 0.0, 0.1 / 29.0],
                amplitude: [0.0; 3],
            };
            30
        ]
    }

    #[test]
    fn velocity_is_the_speed_the_graph_moves_the_element() {
        let graph = powerlines_graph();
        let life = 2000.0;
        let v = get_velocity_at_time(
            FX_ELEM_VEL_WORLD,
            [0.0; 3],
            700.0,
            life,
            &[],
            &graph,
            IDENTITY,
            3,
        );
        assert!((v[2] - 100.0).abs() < 0.01, "{v:?}");
        // 100 ms of the graph's own motion covers what that speed does.
        let moved = integrate_velocity_graph(&graph, 0.35, 0.40, life, 3);
        assert!((moved[2] - v[2] * 0.1).abs() < 0.01, "{moved:?} {v:?}");
        let launch = elem_launch_velocity(FX_ELEM_VEL_WORLD, &[], &graph, IDENTITY, 3);
        assert_eq!(
            launch,
            get_velocity_at_time(
                FX_ELEM_VEL_WORLD,
                [0.0; 3],
                0.0,
                life,
                &[],
                &graph,
                IDENTITY,
                3
            )
        );
    }

    #[test]
    fn a_one_interval_graph_reads_as_before() {
        let graph = vec![
            FxElemVec3Range {
                base: [0.05, -0.1, 0.3125],
                amplitude: [0.0; 3],
            };
            2
        ];
        let v = get_velocity_at_time(
            FX_ELEM_VEL_WORLD,
            [1.0, 2.0, 3.0],
            10.0,
            3000.0,
            &[],
            &graph,
            IDENTITY,
            3,
        );
        assert!(
            (v[0] - 51.0).abs() < 1e-3 && (v[1] + 98.0).abs() < 1e-3 && (v[2] - 315.5).abs() < 1e-3,
            "{v:?}"
        );
    }
}
