//! MW2's `grain_overlay` technique (`grain_overlay.hlsl`), the one the
//! `ac130_overlay_grain` and `javelin_overlay_grain` materials draw with. The
//! Predator's static after impact (`staticEffect` in `_remotemissile.gsc`) and
//! the EMP static in `hud_fullscreen` are quads of `ac130_overlay_grain`.

/// The technique set that scrolls its image every frame.
pub const GRAIN_OVERLAY_TECHSET: &str = "grain_overlay";

const GRAIN_SHIFT_SCALE: f64 = 8.0;
const GRAIN_RATE_S: f64 = 80.875;
const GRAIN_RATE_T: f64 = 27.375;

fn fract(value: f64) -> f64 {
    value - libm::floor(value)
}

/// Texture coordinates `[s0, t0, s1, t1]` of a `grain_overlay` quad at `time_ms`.
///
/// The technique's vertex shader adds `8 * fmod(gameTime.z * (80.875, 27.375), 1)`
/// to the quad's texture coordinates, where `gameTime.z` is the fractional part
/// of the game time in seconds, and the image wraps. The noise lands somewhere
/// new every frame, which reads as moving static. Only the fractional part of
/// the shift moves a wrapped image, so that is all this returns.
pub fn grain_overlay_st(time_ms: i32) -> [f32; 4] {
    let game_time_frac = fract(f64::from(time_ms) / 1000.0);
    let shift = |rate: f64| fract(GRAIN_SHIFT_SCALE * fract(game_time_frac * rate)) as f32;
    let (s, t) = (shift(GRAIN_RATE_S), shift(GRAIN_RATE_T));
    [s, t, s + 1.0, t + 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grain_sits_still_on_whole_seconds() {
        assert_eq!(grain_overlay_st(0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(grain_overlay_st(3000), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn the_grain_jumps_between_frames() {
        // 16 ms: frac 0.016 -> 80.875 * 0.016 = 1.294 -> 8 * 0.294 = 2.352 -> 0.352.
        let st = grain_overlay_st(16);
        assert!((st[0] - 0.352).abs() < 1e-4, "{st:?}");
        // 27.375 * 0.016 = 0.438 -> 8 * 0.438 = 3.504 -> 0.504.
        assert!((st[1] - 0.504).abs() < 1e-4, "{st:?}");
        assert!((st[2] - st[0] - 1.0).abs() < 1e-6);
        assert!((st[3] - st[1] - 1.0).abs() < 1e-6);
        let frames: [[f32; 4]; 4] =
            core::array::from_fn(|i| grain_overlay_st(5016 + 16 * i as i32));
        for pair in frames.windows(2) {
            assert_ne!(pair[0], pair[1]);
        }
        for st in frames {
            assert!((0.0..1.0).contains(&st[0]) && (0.0..1.0).contains(&st[1]));
        }
    }
}
