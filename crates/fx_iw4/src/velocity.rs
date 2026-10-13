use crate::flags::{elem_uses_vel_local, elem_uses_vel_world};
use crate::integrate::sample_vel_graph_at_age;

pub const FX_VEL_AT_TIME_SCALE: f64 = 1000.0;

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
    let scale = FX_VEL_AT_TIME_SCALE as f32;

    if elem_uses_vel_world(flags) && world_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(world_samples, age01, seed);
        out[0] += s[0] * scale;
        out[1] += s[1] * scale;
        out[2] += s[2] * scale;
    }
    if elem_uses_vel_local(flags) && local_samples.len() >= 2 {
        let s = sample_vel_graph_at_age(local_samples, age01, seed);

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
