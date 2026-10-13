use crate::at_rest::FX_ON_GROUND_NORMAL_Z;
use crate::origin::sample_float_range;
use crate::random::{FxRandomChannel, sample_f32};
use crate::vec::vec3_length_sq;

pub const FX_COLLIDE_SUBSTEP_MS: i32 = 0x32;

pub const FX_TRACE_MASK: u32 = 0x811;

pub const FX_TRACE_MASK_ITEM_CLIP: u32 = 0xc11;

pub const FX_IMPACT_CHILD_MIN_SPEED_SQ: f32 = 1.0;

pub const FX_COLLISION_REFLECT_SCALE: f64 = -2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FxCollideSubstep {
    pub msec_start: i32,
    pub msec_end: i32,
}

#[derive(Clone, Debug)]
pub struct FxCollideSubstepIter {
    cur: i32,
    end: i32,
    done: bool,
}

impl Iterator for FxCollideSubstepIter {
    type Item = FxCollideSubstep;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.cur >= self.end {
            self.done = true;
            return None;
        }
        let next_cut = self.cur.wrapping_add(FX_COLLIDE_SUBSTEP_MS);
        if next_cut < self.end {
            let out = FxCollideSubstep {
                msec_start: self.cur,
                msec_end: next_cut,
            };
            self.cur = next_cut;
            Some(out)
        } else {
            let out = FxCollideSubstep {
                msec_start: self.cur,
                msec_end: self.end,
            };
            self.done = true;
            Some(out)
        }
    }
}

#[inline]
pub fn collide_substep_schedule(msec_start: i32, msec_end: i32) -> FxCollideSubstepIter {
    FxCollideSubstepIter {
        cur: msec_start,
        end: msec_end,
        done: msec_end <= msec_start,
    }
}

#[inline]
pub const fn trace_mask(use_item_clip: bool) -> u32 {
    if use_item_clip {
        FX_TRACE_MASK_ITEM_CLIP
    } else {
        FX_TRACE_MASK
    }
}

#[inline]
pub fn impact_child_speed_allows(pre_impact_speed_sq: f32) -> bool {
    pre_impact_speed_sq > FX_IMPACT_CHILD_MIN_SPEED_SQ
}

#[inline]
pub fn sample_reflection_factor(base: f32, amplitude: f32, seed: u64) -> f32 {
    sample_float_range(
        base,
        amplitude,
        sample_f32(seed, FxRandomChannel::Reflection),
    )
}

#[inline]
pub fn collide_on_ground(normal_z: f32) -> bool {
    normal_z > FX_ON_GROUND_NORMAL_Z
}

#[inline]
pub fn collide_marks_at_rest(scaled_vel: [f32; 3], normal_z: f32) -> bool {
    collide_on_ground(normal_z) && !impact_child_speed_allows(vec3_length_sq(scaled_vel))
}

#[inline]
pub fn collision_reflect_base_vel_delta(
    pre_impact_vel: [f32; 3],
    normal: [f32; 3],
    reflection_factor: f32,
) -> [f32; 3] {
    let scaled = [
        pre_impact_vel[0] * reflection_factor,
        pre_impact_vel[1] * reflection_factor,
        pre_impact_vel[2] * reflection_factor,
    ];
    let dot = scaled[0] * normal[0] + scaled[1] * normal[1] + scaled[2] * normal[2];
    let f = (FX_COLLISION_REFLECT_SCALE as f32) * dot;
    let reflected = [
        scaled[0] + f * normal[0],
        scaled[1] + f * normal[1],
        scaled[2] + f * normal[2],
    ];
    [
        reflected[0] - pre_impact_vel[0],
        reflected[1] - pre_impact_vel[1],
        reflected[2] - pre_impact_vel[2],
    ]
}

/// Half the cube a model-physics piece collides as. A piece's model origin is
/// usually at an edge of the model rather than its middle (a suitcase half's
/// is at the case's bottom edge), so the cube stands for the point the piece
/// travels with, not its extent. 1 is the size the luggage, register and
/// bottle pieces have flown with so far: their bounds were read as mins and
/// maxs, which left them at this fallback.
pub const FX_PHYS_PIECE_HALF: f32 = 1.0;

/// How a model-physics piece meets the world: the bounce and friction of its
/// model's physics preset, and which face it lies on once it lands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxPhysPiece {
    pub bounce: f32,
    pub friction: f32,
    /// The model axis of its thinnest extent (3 when the bounds are unknown),
    /// and the model's bounds midpoint and half size along it.
    pub flat_axis: u8,
    pub flat_mid: f32,
    pub flat_half: f32,
}

impl FxPhysPiece {
    /// Inferred, for a model without a preset or bounds: a light prop.
    pub const FALLBACK: Self = Self {
        bounce: 0.2,
        friction: 0.5,
        flat_axis: 3,
        flat_mid: 0.0,
        flat_half: 0.0,
    };

    /// `preset` is the model's (bounce, friction), `bounds` its IW4 bounds:
    /// (midpoint, half size).
    pub fn from_model(preset: Option<(f32, f32)>, bounds: Option<([f32; 3], [f32; 3])>) -> Self {
        let (bounce, friction) = preset
            .filter(|(b, f)| b.is_finite() && f.is_finite())
            .unwrap_or((Self::FALLBACK.bounce, Self::FALLBACK.friction));
        let (flat_axis, flat_mid, flat_half) = bounds
            .filter(|(mid, half)| {
                mid.iter().all(|v| v.is_finite()) && half.iter().all(|v| v.is_finite() && *v > 0.0)
            })
            .and_then(|(mid, half)| {
                let axis = (0..3).min_by(|&a, &b| half[a].total_cmp(&half[b]))?;
                Some((axis as u8, mid[axis], half[axis]))
            })
            .unwrap_or((
                Self::FALLBACK.flat_axis,
                Self::FALLBACK.flat_mid,
                Self::FALLBACK.flat_half,
            ));
        Self {
            bounce: bounce.clamp(0.0, 1.0),
            friction: friction.clamp(0.0, 1.0),
            flat_axis,
            flat_mid,
            flat_half,
        }
    }

    /// How a piece that has landed is drawn: `axis` (the model's axes in the
    /// world, as it tumbled) turned the least way that lays its thinnest
    /// extent upright, so it lies on its broad face, and the height to add to
    /// its origin so that face, at the model's draw `scale`, meets what the
    /// piece's cube rests on. `axis` and no lift when the bounds are unknown.
    pub fn rest_pose(&self, axis: [[f32; 3]; 3], scale: f32) -> ([[f32; 3]; 3], f32) {
        let Some(&a) = axis.get(self.flat_axis as usize) else {
            return (axis, 0.0);
        };
        let up = if a[2] >= 0.0 { 1.0 } else { -1.0 };
        // Rotation taking `a` onto (0, 0, up): about v = a x target, with
        // cos = a . target >= 0, so 1 / (1 + cos) stays bounded.
        let v = [a[1] * up, -a[0] * up, 0.0];
        let k = 1.0 / (1.0 + a[2] * up);
        let turn = |p: [f32; 3]| {
            let vxp = [
                v[1] * p[2] - v[2] * p[1],
                v[2] * p[0] - v[0] * p[2],
                v[0] * p[1] - v[1] * p[0],
            ];
            let vxvxp = [
                v[1] * vxp[2] - v[2] * vxp[1],
                v[2] * vxp[0] - v[0] * vxp[2],
                v[0] * vxp[1] - v[1] * vxp[0],
            ];
            [
                p[0] + vxp[0] + vxvxp[0] * k,
                p[1] + vxp[1] + vxvxp[1] * k,
                p[2] + vxp[2] + vxvxp[2] * k,
            ]
        };
        let flat = [turn(axis[0]), turn(axis[1]), turn(axis[2])];
        let lowest = if up > 0.0 {
            self.flat_mid - self.flat_half
        } else {
            -(self.flat_mid + self.flat_half)
        };
        (flat, -FX_PHYS_PIECE_HALF - lowest * scale)
    }
}

#[cfg(test)]
mod phys_piece_tests {
    use super::*;

    /// props/luggage_1_des elem 0 draws ap_luggage01_des, half an upright
    /// suitcase: its origin sits at the case's bottom edge, its bounds
    /// midpoint (0, 3.31, 17.12) and half size (12.5, 5.46, 17.12).
    fn case_half() -> FxPhysPiece {
        FxPhysPiece::from_model(
            Some((0.2, 0.5)),
            Some(([0.0, 3.31, 17.12], [12.5, 5.46, 17.12])),
        )
    }

    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    #[test]
    fn bounds_are_midpoint_and_half_size() {
        let piece = case_half();
        assert_eq!(piece.flat_axis, 1);
        assert!((piece.flat_mid - 3.31).abs() < 1e-4, "{piece:?}");
        assert!((piece.flat_half - 5.46).abs() < 1e-4, "{piece:?}");
        assert_eq!(
            FxPhysPiece::from_model(None, None).flat_axis,
            FxPhysPiece::FALLBACK.flat_axis
        );
    }

    #[test]
    fn landed_piece_lies_on_its_broad_face() {
        let piece = case_half();
        // Tumbled to stand on an edge: its thin axis tipped 70 degrees from up.
        let (s, c) = 70f32.to_radians().sin_cos();
        let axis = [[1.0, 0.0, 0.0], [0.0, s, c], [0.0, -c, s]];
        let (flat, lift) = piece.rest_pose(axis, 1.0);
        assert!((flat[1][2] - 1.0).abs() < 1e-5, "{flat:?}");
        assert!(
            flat[0][2].abs() < 1e-5 && flat[2][2].abs() < 1e-5,
            "{flat:?}"
        );
        for i in 0..3 {
            assert!((dot(flat[i], flat[i]) - 1.0).abs() < 1e-5, "{flat:?}");
            for j in (i + 1)..3 {
                assert!(dot(flat[i], flat[j]).abs() < 1e-5, "{flat:?}");
            }
        }
        // Its face, 2.15 below the origin, meets the bottom of its cube.
        assert!((lift - (2.15 - FX_PHYS_PIECE_HALF)).abs() < 1e-4, "{lift}");
        // Face down instead: the other face, 8.77 below the origin.
        let down = [axis[0], [0.0, -s, -c], [0.0, c, -s]];
        let (flat, lift) = piece.rest_pose(down, 1.0);
        assert!((flat[1][2] + 1.0).abs() < 1e-5, "{flat:?}");
        assert!((lift - (8.77 - FX_PHYS_PIECE_HALF)).abs() < 1e-4, "{lift}");
    }

    #[test]
    fn piece_without_bounds_keeps_its_pose() {
        let axis = [[0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]];
        assert_eq!(FxPhysPiece::FALLBACK.rest_pose(axis, 1.0), (axis, 0.0));
    }
}
