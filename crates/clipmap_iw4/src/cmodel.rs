use trace_iw4::{BrushRef, Trace, trace_capsule, trace_capsule_along};

use crate::BrushView;

const WALKABLE_NORMAL_Z: f32 = 0.7;

#[derive(Clone, Copy, Debug)]
pub struct ClipCmodel {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub radius: f32,
    pub first_brush: u32,
    pub num_brushes: u16,
}

pub fn clip_handle_to_model(cmodels: &[ClipCmodel], handle: u32) -> Option<&ClipCmodel> {
    cmodels.get(handle as usize)
}

pub fn transformed_capsule_trace<B: BrushView>(
    cmodel: &ClipCmodel,
    leafbrushes: &[u16],
    brushes: &[B],
    start: [f32; 3],
    end: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    origin: [f32; 3],
    angles: [f32; 3],
    mask: u32,
) -> Trace {
    if angles[0] == 0.0 && angles[1] == 0.0 && angles[2] == 0.0 {
        return transformed_capsule_trace_origin_only(
            cmodel,
            leafbrushes,
            brushes,
            start,
            end,
            mins,
            maxs,
            origin,
            mask,
        );
    }

    transformed_capsule_trace_rotated(
        cmodel,
        leafbrushes,
        brushes,
        start,
        end,
        mins,
        maxs,
        origin,
        angles,
        mask,
    )
}

fn transformed_capsule_trace_origin_only<B: BrushView>(
    cmodel: &ClipCmodel,
    leafbrushes: &[u16],
    brushes: &[B],
    start: [f32; 3],
    end: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    origin: [f32; 3],
    mask: u32,
) -> Trace {
    let local_start = [
        start[0] - origin[0],
        start[1] - origin[1],
        start[2] - origin[2],
    ];
    let local_end = [end[0] - origin[0], end[1] - origin[1], end[2] - origin[2]];
    let mut hit = capsule_vs_cmodel(
        cmodel,
        leafbrushes,
        brushes,
        local_start,
        local_end,
        mins,
        maxs,
        mask,
    );
    hit.endpos = [
        hit.endpos[0] + origin[0],
        hit.endpos[1] + origin[1],
        hit.endpos[2] + origin[2],
    ];
    hit
}

/// The capsule stays upright in the world: its centre is found before the
/// trace goes into the model's frame, and its axis is the world's up seen
/// from that frame. Kept upright in the model's frame instead, a tipped
/// model (a care package on its side) would meet a capsule lying along its
/// own up axis, which reaches well past the model on one side.
fn transformed_capsule_trace_rotated<B: BrushView>(
    cmodel: &ClipCmodel,
    leafbrushes: &[u16],
    brushes: &[B],
    start: [f32; 3],
    end: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    origin: [f32; 3],
    angles: [f32; 3],
    mask: u32,
) -> Trace {
    let matrix = angles_to_axis(angles);
    let offset: [f32; 3] = core::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5);
    let half_mins: [f32; 3] = core::array::from_fn(|i| mins[i] - offset[i]);
    let half_maxs: [f32; 3] = core::array::from_fn(|i| maxs[i] - offset[i]);
    let mut start_l: [f32; 3] = core::array::from_fn(|i| start[i] + offset[i] - origin[i]);
    let mut end_l: [f32; 3] = core::array::from_fn(|i| end[i] + offset[i] - origin[i]);
    rotate_point(&mut start_l, &matrix);
    rotate_point(&mut end_l, &matrix);
    let up = [matrix[0][2], matrix[1][2], matrix[2][2]];

    let first = cmodel.first_brush as usize;
    let last = first.saturating_add(cmodel.num_brushes as usize);
    let ids = leafbrushes.get(first..last).unwrap_or(&[]);
    let selected: alloc::vec::Vec<BrushRef<'_>> = ids
        .iter()
        .filter_map(|&id| brushes.get(id as usize))
        .map(crate::brush_ref)
        .collect();
    let mut hit = trace_capsule_along(
        selected.iter().copied(),
        start_l,
        end_l,
        half_mins,
        half_maxs,
        up,
        mask,
    );
    let inv = transpose_matrix(&matrix);
    if hit.fraction < 1.0 {
        rotate_point(&mut hit.normal, &inv);
        if hit.startsolid == 0 {
            hit.walkable = u8::from(hit.normal[2] >= WALKABLE_NORMAL_Z);
        }
    }
    rotate_point(&mut hit.endpos, &inv);
    hit.endpos = core::array::from_fn(|i| hit.endpos[i] + origin[i] - offset[i]);
    hit
}

fn capsule_vs_cmodel<B: BrushView>(
    cmodel: &ClipCmodel,
    leafbrushes: &[u16],
    brushes: &[B],
    start: [f32; 3],
    end: [f32; 3],
    mins: [f32; 3],
    maxs: [f32; 3],
    mask: u32,
) -> Trace {
    let first = cmodel.first_brush as usize;
    let last = first.saturating_add(cmodel.num_brushes as usize);
    let ids = leafbrushes.get(first..last).unwrap_or(&[]);
    let selected: alloc::vec::Vec<BrushRef<'_>> = ids
        .iter()
        .filter_map(|&id| brushes.get(id as usize))
        .map(crate::brush_ref)
        .collect();
    trace_capsule(selected.iter().copied(), start, end, mins, maxs, mask)
}

fn angles_to_axis(angles: [f32; 3]) -> [[f32; 3]; 3] {
    const DEG2RAD: f32 = 0.01745329238474369_f32;
    let yaw = angles[1] * DEG2RAD;
    let pitch = angles[0] * DEG2RAD;
    let roll = angles[2] * DEG2RAD;
    let cy = libm::cosf(yaw);
    let sy = libm::sinf(yaw);
    let cp = libm::cosf(pitch);
    let sp = libm::sinf(pitch);
    let cr = libm::cosf(roll);
    let sr = libm::sinf(roll);
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy + -sy * cr, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + -sr * -sy, cr * sp * sy + -sr * cy, cr * cp],
    ]
}

fn rotate_point(point: &mut [f32; 3], mat: &[[f32; 3]; 3]) {
    let x = point[0];
    let y = point[1];
    let z = point[2];
    point[0] = mat[0][0] * x + mat[0][1] * y + mat[0][2] * z;
    point[1] = mat[1][0] * x + mat[1][1] * y + mat[1][2] * z;
    point[2] = mat[2][0] * x + mat[2][1] * y + mat[2][2] * z;
}

fn transpose_matrix(mat: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [mat[0][0], mat[1][0], mat[2][0]],
        [mat[0][1], mat[1][1], mat[2][1]],
        [mat[0][2], mat[1][2], mat[2][2]],
    ]
}

#[derive(Clone, Copy, Debug)]
pub struct CmodelTransform {
    origin: [f32; 3],
    matrix: [[f32; 3]; 3],
}

impl CmodelTransform {
    pub fn new(origin: [f32; 3], angles: [f32; 3]) -> Option<Self> {
        if !origin.iter().chain(angles.iter()).all(|v| v.is_finite()) {
            return None;
        }
        Some(Self {
            origin,
            matrix: angles_to_axis(angles),
        })
    }

    pub fn local_point(self, point: [f64; 3]) -> [f64; 3] {
        let delta: [f64; 3] = core::array::from_fn(|i| point[i] - f64::from(self.origin[i]));
        core::array::from_fn(|row| {
            (0..3)
                .map(|col| f64::from(self.matrix[row][col]) * delta[col])
                .sum()
        })
    }

    pub fn world_direction(self, mut direction: [f64; 3]) -> [f64; 3] {
        let local = direction;
        for (i, value) in direction.iter_mut().enumerate() {
            *value = (0..3)
                .map(|j| f64::from(self.matrix[j][i]) * local[j])
                .sum();
        }
        direction
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYER_MINS: [f32; 3] = [-15.0, -15.0, 0.0];
    const PLAYER_MAXS: [f32; 3] = [15.0, 15.0, 70.0];
    /// A care package's collision box: 60 long, 36 wide, 32 high.
    const PLANES: [[f32; 4]; 6] = [
        [1.0, 0.0, 0.0, 30.0],
        [-1.0, 0.0, 0.0, 30.0],
        [0.0, 1.0, 0.0, 18.0],
        [0.0, -1.0, 0.0, 18.0],
        [0.0, 0.0, 1.0, 16.0],
        [0.0, 0.0, -1.0, 16.0],
    ];
    const CMODEL: ClipCmodel = ClipCmodel {
        mins: [-30.0, -18.0, -16.0],
        maxs: [30.0, 18.0, 16.0],
        radius: 38.0,
        first_brush: 0,
        num_brushes: 1,
    };

    fn trace(start: [f32; 3], end: [f32; 3], origin: [f32; 3], angles: [f32; 3]) -> Trace {
        let brush = BrushRef {
            planes: &PLANES,
            contents: 1,
            plane_surface_flags: &[],
            glass_encoded: 0,
        };
        transformed_capsule_trace(
            &CMODEL,
            &[0],
            &[brush],
            start,
            end,
            PLAYER_MINS,
            PLAYER_MAXS,
            origin,
            angles,
            1,
        )
    }

    /// Lying on its side (roll 90), the box spans y -16..16 and z 0..36.
    const ON_SIDE: ([f32; 3], [f32; 3]) = ([0.0, 0.0, 18.0], [0.0, 0.0, 90.0]);

    #[test]
    fn a_player_meets_a_box_on_its_side_at_its_face_from_either_side() {
        let (origin, angles) = ON_SIDE;
        for side in [1.0f32, -1.0] {
            let start = [0.0, side * 120.0, 0.0];
            let short = trace(start, [0.0, side * 40.0, 0.0], origin, angles);
            assert_eq!(short.fraction, 1.0, "side {side}: blocked before the face");
            let long = trace(start, [0.0, 0.0, 0.0], origin, angles);
            let stop = start[1] + (0.0 - start[1]) * long.fraction;
            assert!(
                (stop - side * 31.0).abs() < 0.5,
                "side {side}: stopped at y {stop}, the face plus the player's radius is {}",
                side * 31.0
            );
        }
    }

    #[test]
    fn the_top_of_a_box_on_its_side_is_walkable() {
        let (origin, angles) = ON_SIDE;
        let down = trace([0.0, 0.0, 100.0], [0.0, 0.0, 20.0], origin, angles);
        assert!(down.fraction < 1.0);
        assert!(
            (down.endpos[2] - 36.0).abs() < 0.5,
            "landed at {:?}",
            down.endpos
        );
        assert!(down.normal[2] > 0.99, "normal {:?}", down.normal);
        assert_eq!(down.walkable, 1);
    }

    #[test]
    fn an_upright_box_turned_about_z_stops_a_player_at_its_end() {
        let yaw = 37.0f32;
        let (sin, cos) = yaw.to_radians().sin_cos();
        let origin = [0.0, 0.0, 16.0];
        let angles = [0.0, yaw, 0.0];
        let start = [-120.0 * cos, -120.0 * sin, 0.0];
        let hit = trace(start, [0.0, 0.0, 0.0], origin, angles);
        let stop = 120.0 * (1.0 - hit.fraction);
        assert!((stop - 45.0).abs() < 0.5, "stopped {stop} from the centre");
        let down = trace([0.0, 0.0, 100.0], [0.0, 0.0, 0.0], origin, angles);
        assert!(
            (down.endpos[2] - 32.0).abs() < 0.5,
            "landed at {:?}",
            down.endpos
        );
        assert_eq!(down.walkable, 1);
    }
}
