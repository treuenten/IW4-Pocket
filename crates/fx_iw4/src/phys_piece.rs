//! How a model-physics effect piece (a suitcase half, a register's screen)
//! turns. MW2 hands such a piece to its physics world as a body: it turns about
//! its centre of mass at the angular velocity its element was authored with,
//! slowed by the world's angular drag, and once it hits the ground contact
//! friction tips it over onto a face.
//!
//! Here the centre of mass is the middle of the model's bounds (a case half's
//! model origin is at the case's bottom edge, 17 units from it), the spin is a
//! turn about a fixed axis that the drag slows, and at the first touch of the
//! ground the spin stops and the piece tips the least way onto its broadest
//! face. Everything is a function of the piece's age, so the motion that traces
//! the piece and the draw that places its model agree without stored state.

use crate::angles::get_elem_angles_axis;
use crate::collide::{FX_PHYS_PIECE_HALF, FxPhysPiece};
use crate::origin::sample_float_range;
use crate::random::{FxRandomChannel, sample_f32};

/// Inferred: how fast the physics world's angular drag slows a spinning piece,
/// per second (MW2's phys_dragAngular, "the amount of angular drag, applied
/// globally"; 0.5 taken as its default).
pub const FX_PHYS_ANGULAR_DRAG: f32 = 0.5;

/// Inferred: how long a landed piece takes to tip a right angle onto its face,
/// about as long as a 30-unit box takes to fall over.
pub const FX_PHYS_TIP_MS: f32 = 250.0;

/// Smallest half extent a piece is traced with.
const MIN_HALF: f32 = 0.5;

/// A model-physics piece's launch pose and spin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FxPhysSpin {
    /// The model's axes in the world at launch.
    pub axis: [[f32; 3]; 3],
    /// Its angular velocity in the world, in radians a second.
    pub omega: [f32; 3],
}

impl FxPhysSpin {
    /// The element's spawn angles and authored angular velocity (radians a
    /// millisecond about its pitch, yaw and roll axes), as the body it is
    /// launched as: the yaw rate turns it about its own up axis, the pitch rate
    /// about its left one and the roll rate about its forward one.
    pub fn launch(
        spawn_angles: [[f32; 2]; 3],
        angular_velocity: [[f32; 2]; 3],
        seed: u64,
        effect_axis: [[f32; 3]; 3],
    ) -> Self {
        let axis = get_elem_angles_axis(spawn_angles, angular_velocity, seed, 0.0, effect_axis);
        let rate = |i: usize, channel| {
            sample_float_range(
                angular_velocity[i][0],
                angular_velocity[i][1],
                sample_f32(seed, channel),
            ) * 1000.0
        };
        let pitch = rate(0, FxRandomChannel::AngularPitch);
        let yaw = rate(1, FxRandomChannel::AngularYaw);
        let roll = rate(2, FxRandomChannel::AngularRoll);
        let omega =
            core::array::from_fn(|k| axis[0][k] * roll + axis[1][k] * pitch + axis[2][k] * yaw);
        Self { axis, omega }
    }

    /// The model's axes after `ms` of free flight.
    pub fn axis_after(&self, ms: f32) -> [[f32; 3]; 3] {
        let speed = libm::sqrtf(dot(self.omega, self.omega));
        if !(speed > 0.0) || !(ms > 0.0) {
            return self.axis;
        }
        let t = ms * 0.001;
        let turned = speed * (1.0 - libm::expf(-FX_PHYS_ANGULAR_DRAG * t)) / FX_PHYS_ANGULAR_DRAG;
        let about = self.omega.map(|w| w / speed);
        rotate_rows(self.axis, about, turned)
    }
}

impl FxPhysPiece {
    /// The model's bounds are known, so the piece turns about their middle.
    pub fn has_bounds(&self) -> bool {
        (self.flat_axis as usize) < 3
    }

    /// Where the model's middle is from its origin with the model's axes at
    /// `axis`, at draw `scale`.
    pub fn middle_offset(&self, axis: [[f32; 3]; 3], scale: f32) -> [f32; 3] {
        core::array::from_fn(|k| {
            (axis[0][k] * self.mid[0] + axis[1][k] * self.mid[1] + axis[2][k] * self.mid[2]) * scale
        })
    }

    /// How far the model reaches below its middle with its axes at `axis`.
    pub fn reach_below(&self, axis: [[f32; 3]; 3], scale: f32) -> f32 {
        if !self.has_bounds() {
            return FX_PHYS_PIECE_HALF;
        }
        let reach = (0..3)
            .map(|i| libm::fabsf(axis[i][2]) * self.half[i])
            .sum::<f32>()
            * scale;
        reach.max(MIN_HALF)
    }

    /// The half width the piece is traced with: its thinnest half extent,
    /// within `reach`.
    pub fn trace_radius(&self, reach: f32, scale: f32) -> f32 {
        if !self.has_bounds() {
            return FX_PHYS_PIECE_HALF;
        }
        (self.flat_half * scale).max(MIN_HALF).min(reach)
    }

    /// The model's axes `age_ms` after launch, for a piece that first touched
    /// the ground `landed_ms` after it: spinning until then, then tipping onto
    /// its broadest face, a little faster as it goes, as a box falls over.
    pub fn pose(&self, spin: &FxPhysSpin, age_ms: f32, landed_ms: Option<f32>) -> [[f32; 3]; 3] {
        let Some(landed) = landed_ms.filter(|&landed| age_ms > landed) else {
            return spin.axis_after(age_ms);
        };
        let at_landing = spin.axis_after(landed);
        let Some((about, angle)) = self.tip(at_landing) else {
            return at_landing;
        };
        let s = ((age_ms - landed) / tip_ms(angle)).min(1.0);
        rotate_rows(at_landing, about, angle * s * s)
    }

    /// Whether a piece that landed at `landed_ms` lies on its face by `age_ms`.
    pub fn tipped(&self, spin: &FxPhysSpin, age_ms: f32, landed_ms: f32) -> bool {
        let angle = self
            .tip(spin.axis_after(landed_ms))
            .map_or(0.0, |(_, angle)| angle);
        age_ms >= landed_ms + tip_ms(angle)
    }

    /// The turn that stands the thinnest axis of `axis` upright, so the piece
    /// lies on its broad face: the axis to turn about and the angle.
    fn tip(&self, axis: [[f32; 3]; 3]) -> Option<([f32; 3], f32)> {
        let a = *axis.get(self.flat_axis as usize)?;
        let up = if a[2] >= 0.0 { 1.0 } else { -1.0 };
        let cross = [a[1] * up, -a[0] * up, 0.0];
        let len = libm::sqrtf(dot(cross, cross));
        if len < 1e-6 {
            return None;
        }
        Some((cross.map(|c| c / len), libm::atan2f(len, a[2] * up)))
    }
}

fn tip_ms(angle: f32) -> f32 {
    FX_PHYS_TIP_MS * libm::sqrtf((angle / core::f32::consts::FRAC_PI_2).clamp(0.0, 1.0))
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Each row of `rows` turned by `angle` about the unit vector `about`.
fn rotate_rows(rows: [[f32; 3]; 3], about: [f32; 3], angle: f32) -> [[f32; 3]; 3] {
    let (s, c) = (libm::sinf(angle), libm::cosf(angle));
    rows.map(|v| {
        let kxv = [
            about[1] * v[2] - about[2] * v[1],
            about[2] * v[0] - about[0] * v[2],
            about[0] * v[1] - about[1] * v[0],
        ];
        let kv = dot(about, v) * (1.0 - c);
        core::array::from_fn(|i| v[i] * c + kxv[i] * s + about[i] * kv)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

    /// props/luggage_1_des elem 0 (ap_luggage01_des): no spawn angles, a spin
    /// of -600..600 degrees a second in pitch and roll and -960..-600 in yaw.
    const CASE_SPIN: [[f32; 2]; 3] = [
        [-0.010_471_975, 0.020_943_95],
        [-0.016_755_16, 0.006_283_185],
        [-0.010_471_975, 0.020_943_95],
    ];

    fn case_half() -> FxPhysPiece {
        FxPhysPiece::from_model(
            Some((0.2, 0.5)),
            Some(([0.0, 3.31, 17.12], [12.5, 5.46, 17.12])),
        )
    }

    fn orthonormal(axis: [[f32; 3]; 3]) -> bool {
        (0..3).all(|i| {
            (dot(axis[i], axis[i]) - 1.0).abs() < 1e-4
                && ((i + 1)..3).all(|j| dot(axis[i], axis[j]).abs() < 1e-4)
        })
    }

    #[test]
    fn the_case_half_turns_about_its_middle() {
        let piece = case_half();
        let spin = FxPhysSpin::launch([[0.0; 2]; 3], CASE_SPIN, 11, IDENTITY);
        assert_eq!(spin.axis, IDENTITY);
        let speed = libm::sqrtf(dot(spin.omega, spin.omega)).to_degrees();
        assert!((600.0..=1300.0).contains(&speed), "{speed}");
        // Its middle is 17 units above and 3 to the side of its origin.
        let off = piece.middle_offset(IDENTITY, 1.0);
        assert!(
            (off[1] - 3.31).abs() < 1e-4 && (off[2] - 17.12).abs() < 1e-4,
            "{off:?}"
        );
        // Standing upright it reaches its half height below its middle; lying
        // flat, its half thickness.
        assert!((piece.reach_below(IDENTITY, 1.0) - 17.12).abs() < 1e-3);
        let flat = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]];
        assert!((piece.reach_below(flat, 1.0) - 5.46).abs() < 1e-3);
        assert!((piece.trace_radius(17.12, 1.0) - 5.46).abs() < 1e-4);
    }

    #[test]
    fn drag_slows_the_spin() {
        let spin = FxPhysSpin {
            axis: IDENTITY,
            omega: [0.0, 0.0, 900f32.to_radians()],
        };
        // Yaw only: the forward axis sweeps the turned angle.
        let turned = |ms: f32| {
            let a = spin.axis_after(ms)[0];
            libm::atan2f(a[1], a[0]).to_degrees()
        };
        // 0.2 s: 900 * (1 - e^-0.1) / 0.5 = 171 degrees, not 180.
        assert!((turned(200.0) - 171.3).abs() < 0.5, "{}", turned(200.0));
        assert!(orthonormal(spin.axis_after(737.0)));
    }

    #[test]
    fn a_landed_piece_stops_spinning_and_tips_onto_its_face() {
        let piece = case_half();
        let spin = FxPhysSpin::launch([[0.0; 2]; 3], CASE_SPIN, 5, IDENTITY);
        let landed = 400.0;
        let at_landing = piece.pose(&spin, landed, Some(landed));
        assert_eq!(at_landing, spin.axis_after(landed));
        // Partway over it has turned towards the face, no further spin.
        let mid = piece.pose(&spin, landed + 60.0, Some(landed));
        assert!(orthonormal(mid));
        let flat = piece.pose(&spin, landed + 1000.0, Some(landed));
        assert!(orthonormal(flat));
        assert!((flat[1][2].abs() - 1.0).abs() < 1e-4, "{flat:?}");
        assert!(piece.tipped(&spin, landed + 1000.0, landed));
        assert!((piece.reach_below(flat, 1.0) - 5.46).abs() < 1e-3);
        // Its other two axes lie level.
        assert!(
            flat[0][2].abs() < 1e-4 && flat[2][2].abs() < 1e-4,
            "{flat:?}"
        );
    }

    #[test]
    fn a_piece_without_bounds_turns_about_its_origin() {
        let piece = FxPhysPiece::FALLBACK;
        assert!(!piece.has_bounds());
        assert_eq!(piece.middle_offset(IDENTITY, 1.0), [0.0; 3]);
        assert_eq!(piece.reach_below(IDENTITY, 1.0), FX_PHYS_PIECE_HALF);
        let spin = FxPhysSpin::launch([[0.0; 2]; 3], CASE_SPIN, 5, IDENTITY);
        assert_eq!(
            piece.pose(&spin, 900.0, Some(300.0)),
            spin.axis_after(300.0)
        );
    }
}
