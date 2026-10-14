//! The height map a water material samples (its `normalMap`, texture semantic
//! 11). It ships with no pixels: the material carries a wave spectrum instead,
//! an `m` x `n` grid of complex starting amplitudes with the zero frequency in
//! the middle of each axis (index `m / 2`, `n / 2`), and the map is built from
//! it. The pixel shader sums three octaves of this map and turns their
//! differences into the surface normal, so without it the water is not drawn.
//!
//! The map here is the spectrum's inverse transform at time zero: the waves'
//! real shape, standing still. Each material's wave strength is already in its
//! amplitudes, so every material is packed with the same scale (calm water
//! stays flat and rough water stays rough).

/// `IW4L_WATER_MAP=1` builds the maps, so water surfaces are drawn. Off by
/// default until it is measured on the phone: drawing the water costs
/// graphics time wherever it fills the screen.
pub const WATER_MAP_ENV: &str = "IW4L_WATER_MAP";

/// Built when `IW4L_WATER_MAP` is `1`/`on`/`true`. Read once.
pub fn water_maps_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var(WATER_MAP_ENV).is_ok_and(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("on") || value.eq_ignore_ascii_case("true")
        })
    })
}

/// Map bytes per unit of height. One value for every material: the authored
/// amplitudes differ by orders of magnitude between calm and rough water, and
/// that difference is the look.
pub const WATER_MAP_SCALE: f32 = 0.03;

/// Grid sizes the generator accepts: powers of two up to 256 on each side.
fn valid_side(side: usize) -> bool {
    side.is_power_of_two() && side <= 256
}

/// Heights at time zero, `m` rows of `n`, row-major in the spectrum's own
/// order. `h0` is `m * n` complex amplitudes interleaved (re, im).
pub fn water_heights(m: usize, n: usize, h0: &[f32]) -> Option<Vec<f32>> {
    if !valid_side(m) || !valid_side(n) || h0.len() != 2 * m * n {
        return None;
    }
    // H(k) + conj(H(-k)): the spectrum of a real surface.
    let mut re = vec![0.0f64; m * n];
    let mut im = vec![0.0f64; m * n];
    for a in 0..m {
        let a_neg = (m - a) % m;
        for b in 0..n {
            let b_neg = (n - b) % n;
            let k = 2 * (a * n + b);
            let k_neg = 2 * (a_neg * n + b_neg);
            re[a * n + b] = f64::from(h0[k]) + f64::from(h0[k_neg]);
            im[a * n + b] = f64::from(h0[k + 1]) - f64::from(h0[k_neg + 1]);
        }
    }
    let twiddles = |len: usize| -> Vec<(f64, f64)> {
        (0..len)
            .map(|i| {
                let angle = std::f64::consts::TAU * i as f64 / len as f64;
                (angle.cos(), angle.sin())
            })
            .collect()
    };
    let (tw_m, tw_n) = (twiddles(m), twiddles(n));
    // Centred frequencies: e^{i 2 pi (b - n/2) y / n} = (-1)^y e^{i 2 pi b y / n}.
    let sign = |i: usize| if i % 2 == 0 { 1.0 } else { -1.0 };

    // Along each row first: g(a, y) = sum_b H(a, b) e^{i 2 pi (b - n/2) y / n}.
    let mut g_re = vec![0.0f64; m * n];
    let mut g_im = vec![0.0f64; m * n];
    for a in 0..m {
        let row = a * n;
        for y in 0..n {
            let (mut sum_re, mut sum_im) = (0.0, 0.0);
            for b in 0..n {
                let (cos, sin) = tw_n[(b * y) % n];
                let (h_re, h_im) = (re[row + b], im[row + b]);
                sum_re += h_re * cos - h_im * sin;
                sum_im += h_re * sin + h_im * cos;
            }
            g_re[row + y] = sum_re * sign(y);
            g_im[row + y] = sum_im * sign(y);
        }
    }
    // Then down the columns, keeping the real part.
    let mut heights = vec![0.0f32; m * n];
    for x in 0..m {
        for y in 0..n {
            let mut sum = 0.0;
            for a in 0..m {
                let (cos, sin) = tw_m[(a * x) % m];
                sum += g_re[a * n + y] * cos - g_im[a * n + y] * sin;
            }
            heights[x * n + y] = (sum * sign(x)) as f32;
        }
    }
    Some(heights)
}

/// One byte per texel, 128 for still water. Heights go through `tanh` so the
/// roughest materials round off near 0 and 255 instead of flattening there.
pub fn water_map_bytes(heights: &[f32], scale: f32) -> Vec<u8> {
    heights
        .iter()
        .map(|height| {
            let value = 128.0 + 127.0 * (scale * height / 127.0).tanh();
            value.round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

/// Grey RGBA8 texels (the byte in red, green and blue, alpha 255) with every
/// mip level below, each a 2x2 box average of the one above, down to 1x1.
/// Returns the bytes and the level count.
pub fn water_map_rgba_mips(width: usize, height: usize, bytes: &[u8]) -> (Vec<u8>, u32) {
    let mut levels = 1u32;
    let mut level = bytes.to_vec();
    let (mut w, mut h) = (width, height);
    let mut out = Vec::with_capacity(width * height * 4 * 4 / 3 + 4);
    let push = |out: &mut Vec<u8>, level: &[u8]| {
        for &v in level {
            out.extend_from_slice(&[v, v, v, 255]);
        }
    };
    push(&mut out, &level);
    while w > 1 || h > 1 {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0u8; nw * nh];
        for y in 0..nh {
            for x in 0..nw {
                let (x0, y0) = ((2 * x).min(w - 1), (2 * y).min(h - 1));
                let (x1, y1) = ((2 * x + 1).min(w - 1), (2 * y + 1).min(h - 1));
                let sum = u32::from(level[y0 * w + x0])
                    + u32::from(level[y0 * w + x1])
                    + u32::from(level[y1 * w + x0])
                    + u32::from(level[y1 * w + x1]);
                next[y * nw + x] = ((sum + 2) / 4) as u8;
            }
        }
        push(&mut out, &next);
        level = next;
        (w, h) = (nw, nh);
        levels += 1;
    }
    (out, levels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spectrum(m: usize, n: usize, bins: &[(usize, usize, f32, f32)]) -> Vec<f32> {
        let mut h0 = vec![0.0; 2 * m * n];
        for &(a, b, re, im) in bins {
            h0[2 * (a * n + b)] = re;
            h0[2 * (a * n + b) + 1] = im;
        }
        h0
    }

    #[test]
    fn one_bin_is_a_wave_along_the_first_axis() {
        // Row 33 of 64 is one cycle along the first index, none along the second.
        let h = water_heights(64, 64, &spectrum(64, 64, &[(33, 32, 1.0, 0.0)])).unwrap();
        for x in 0..64 {
            let expected = 2.0 * (std::f32::consts::TAU * x as f32 / 64.0).cos();
            for y in 0..64 {
                assert!((h[x * 64 + y] - expected).abs() < 1e-4, "x={x} y={y}");
            }
        }
    }

    #[test]
    fn half_plane_spectrum_gives_a_real_surface_with_no_mean() {
        // Only the upper half filled, like the shipped spectra.
        let bins: Vec<_> = (33..64)
            .flat_map(|a| {
                (0..64).map(move |b| {
                    (
                        a,
                        b,
                        ((a * 7 + b * 3) % 11) as f32 - 5.0,
                        ((a + b * 5) % 7) as f32 - 3.0,
                    )
                })
            })
            .collect();
        let h = water_heights(64, 64, &spectrum(64, 64, &bins)).unwrap();
        let mean = h.iter().sum::<f32>() / h.len() as f32;
        assert!(mean.abs() < 1e-2, "mean {mean}");
        assert!(h.iter().any(|v| v.abs() > 1.0));
    }

    #[test]
    fn stronger_waves_stay_stronger() {
        // No per-material normalisation: doubling the amplitudes doubles the
        // distance from 128 while it is still in the linear range.
        let calm = water_heights(64, 64, &spectrum(64, 64, &[(34, 32, 100.0, 0.0)])).unwrap();
        let rough = water_heights(64, 64, &spectrum(64, 64, &[(34, 32, 200.0, 0.0)])).unwrap();
        let calm = water_map_bytes(&calm, WATER_MAP_SCALE);
        let rough = water_map_bytes(&rough, WATER_MAP_SCALE);
        let peak = |bytes: &[u8]| {
            bytes
                .iter()
                .map(|&b| (i32::from(b) - 128).abs())
                .max()
                .unwrap()
        };
        assert_eq!(peak(&calm), 6);
        assert_eq!(peak(&rough), 12);
        assert!(water_map_bytes(&[0.0], WATER_MAP_SCALE) == [128]);
        assert!(water_map_bytes(&[1e9, -1e9], WATER_MAP_SCALE) == [255, 1]);
    }

    #[test]
    fn mips_go_down_to_one_texel() {
        let bytes: Vec<u8> = (0..64 * 64).map(|i| (i % 256) as u8).collect();
        let (rgba, levels) = water_map_rgba_mips(64, 64, &bytes);
        assert_eq!(levels, 7);
        assert_eq!(rgba.len(), 4 * (4096 + 1024 + 256 + 64 + 16 + 4 + 1));
        assert_eq!(&rgba[..8], &[0, 0, 0, 255, 1, 1, 1, 255]);
    }

    #[test]
    fn refuses_bad_grids() {
        assert!(water_heights(48, 64, &vec![0.0; 2 * 48 * 64]).is_none());
        assert!(water_heights(64, 64, &[0.0; 10]).is_none());
    }
}
