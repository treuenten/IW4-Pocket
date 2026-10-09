use core::sync::atomic::{AtomicBool, Ordering};

pub const FX_RANDOM_VERSION: u32 = 1;

#[derive(Copy, Clone, Debug)]
#[repr(u32)]
pub enum FxRandomChannel {
    AngularPitch = 1,
    AngularRoll = 2,
    AngularYaw = 3,
    Atlas = 4,
    Color = 5,
    Delay = 6,
    EmitDistance = 7,
    GlassCrack = 8,
    GlassShatter = 9,
    Gravity = 10,
    InitialRotation = 11,
    Life = 12,
    OneShotCount = 13,
    Reflection = 14,
    RotationDelta = 15,
    Scale = 16,
    Size = 17,
    SpawnAngleYaw = 18,
    SpawnHeight = 19,
    SpawnOriginX = 20,
    SpawnOriginY = 21,
    SpawnOriginZ = 22,
    SpawnPitch = 23,
    SpawnRadius = 24,
    SpawnRoll = 25,
    SpawnYaw = 26,
    VelocityX = 27,
    VelocityY = 28,
    VelocityZ = 29,
    Visual = 30,
}

const EFFECT_DOMAIN: &[u8; 24] = b"iw4l.random.effect......";
const ELEMENT_DOMAIN: &[u8; 24] = b"iw4l.random.element.....";
const TRAIL_DOMAIN: &[u8; 24] = b"iw4l.random.trail.......";
const SAMPLE_DOMAIN: &[u8; 24] = b"iw4l.random.sample......";

/// Draw every value from a 64-bit integer mixer instead of BLAKE3
/// (IW4L_FX_FAST_RANDOM). Off by default, and off is the BLAKE3 values bit for
/// bit. BLAKE3 hashes a 37-48 byte tuple for each value, several per particle
/// per frame; the mixer builds no tuple and is a few multiplies.
static FAST: AtomicBool = AtomicBool::new(false);

/// Picks the source for every value drawn after the call. Set once at startup,
/// before an effect plays: particles redraw some values every frame, so a
/// change mid-match would make the live ones jump.
pub fn set_fast_random(on: bool) {
    FAST.store(on, Ordering::Relaxed);
}

fn fast_random() -> bool {
    FAST.load(Ordering::Relaxed)
}

/// The mixer's stream is version 2; version 1 is the BLAKE3 one.
const FAST_RANDOM_VERSION: u64 = 2;

/// One salt per function, so that equal inputs to two of them give unrelated
/// values, as the BLAKE3 domains do.
const EFFECT_SALT: u64 = fast_salt(*b"effect..");
const ELEMENT_SALT: u64 = fast_salt(*b"element.");
const TRAIL_SALT: u64 = fast_salt(*b"trail...");
const SAMPLE_SALT: u64 = fast_salt(*b"sample..");

const fn fast_salt(tag: [u8; 8]) -> u64 {
    mix(u64::from_le_bytes(tag), FAST_RANDOM_VERSION)
}

/// Two finaliser rounds, splitmix64's then murmur3's fmix64, over `a` with `b`
/// rotated into it. Both rounds are bijections, so for a fixed `a` distinct
/// `b` never collide, and one flipped input bit flips about half the output.
/// The rotation keeps a small `b` (an index, a time) off the low bits where a
/// small `a` (a pool slot) lives.
const fn mix(a: u64, b: u64) -> u64 {
    let mut z = a ^ b.rotate_left(29);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    z = (z ^ (z >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    z = (z ^ (z >> 33)).wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    z ^ (z >> 33)
}

fn fast_effect_random_key(instance: u64, begin_msec: i32) -> u64 {
    mix(instance ^ EFFECT_SALT, u64::from(begin_msec as u32))
}

fn fast_elem_random_seed(effect_key: u64, sequence: u8, begin_msec: i32) -> u64 {
    mix(
        effect_key ^ ELEMENT_SALT,
        (u64::from(sequence) << 32) | u64::from(begin_msec as u32),
    )
}

fn fast_trail_random_seed(effect_key: u64, sequence: i8) -> u64 {
    mix(effect_key ^ TRAIL_SALT, u64::from(sequence as u8))
}

fn fast_sample_word(key: u64, channel: FxRandomChannel, index: u64) -> u64 {
    mix(key ^ SAMPLE_SALT ^ ((channel as u64) << 56), index)
}

fn digest_word(tuple: &[u8]) -> u64 {
    let digest = blake3::hash(tuple);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes)
}

pub fn effect_random_key(instance: u64, begin_msec: i32) -> u64 {
    if fast_random() {
        return fast_effect_random_key(instance, begin_msec);
    }
    let mut tuple = [0_u8; 40];
    tuple[..24].copy_from_slice(EFFECT_DOMAIN);
    tuple[24..28].copy_from_slice(&FX_RANDOM_VERSION.to_le_bytes());
    tuple[28..36].copy_from_slice(&instance.to_le_bytes());
    tuple[36..40].copy_from_slice(&begin_msec.to_le_bytes());
    digest_word(&tuple)
}

pub fn elem_random_seed(effect_key: u64, sequence: u8, begin_msec: i32) -> u64 {
    if fast_random() {
        return fast_elem_random_seed(effect_key, sequence, begin_msec);
    }
    let mut tuple = [0_u8; 41];
    tuple[..24].copy_from_slice(ELEMENT_DOMAIN);
    tuple[24..28].copy_from_slice(&FX_RANDOM_VERSION.to_le_bytes());
    tuple[28..36].copy_from_slice(&effect_key.to_le_bytes());
    tuple[36] = sequence;
    tuple[37..41].copy_from_slice(&begin_msec.to_le_bytes());
    digest_word(&tuple)
}

pub fn trail_random_seed(effect_key: u64, sequence: i8) -> u64 {
    if fast_random() {
        return fast_trail_random_seed(effect_key, sequence);
    }
    let mut tuple = [0_u8; 37];
    tuple[..24].copy_from_slice(TRAIL_DOMAIN);
    tuple[24..28].copy_from_slice(&FX_RANDOM_VERSION.to_le_bytes());
    tuple[28..36].copy_from_slice(&effect_key.to_le_bytes());
    tuple[36..37].copy_from_slice(&sequence.to_le_bytes());
    digest_word(&tuple)
}

fn sample_word(key: u64, channel: FxRandomChannel, index: u64) -> u64 {
    if fast_random() {
        return fast_sample_word(key, channel, index);
    }
    let mut tuple = [0_u8; 48];
    tuple[..24].copy_from_slice(SAMPLE_DOMAIN);
    tuple[24..28].copy_from_slice(&FX_RANDOM_VERSION.to_le_bytes());
    tuple[28..36].copy_from_slice(&key.to_le_bytes());
    tuple[36..40].copy_from_slice(&(channel as u32).to_le_bytes());
    tuple[40..48].copy_from_slice(&index.to_le_bytes());
    digest_word(&tuple)
}

pub fn sample_u16(key: u64, channel: FxRandomChannel) -> u16 {
    (sample_word(key, channel, 0) >> 48) as u16
}

pub fn sample_f32(key: u64, channel: FxRandomChannel) -> f32 {
    sample_at(key, channel, 0)
}

pub fn sample_at(key: u64, channel: FxRandomChannel, index: u64) -> f32 {
    unit(sample_word(key, channel, index))
}

/// The top 24 bits of a word as a value in [0, 1).
fn unit(word: u64) -> f32 {
    (word >> 40) as f32 / 16_777_216.0
}

pub fn elem_visual_index(count: u8, key: u64) -> usize {
    if count <= 1 {
        return 0;
    }
    ((u32::from(sample_u16(key, FxRandomChannel::Visual)) * u32::from(count)) >> 16) as usize
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::vec::Vec;

    const CHANNELS: [FxRandomChannel; 30] = [
        FxRandomChannel::AngularPitch,
        FxRandomChannel::AngularRoll,
        FxRandomChannel::AngularYaw,
        FxRandomChannel::Atlas,
        FxRandomChannel::Color,
        FxRandomChannel::Delay,
        FxRandomChannel::EmitDistance,
        FxRandomChannel::GlassCrack,
        FxRandomChannel::GlassShatter,
        FxRandomChannel::Gravity,
        FxRandomChannel::InitialRotation,
        FxRandomChannel::Life,
        FxRandomChannel::OneShotCount,
        FxRandomChannel::Reflection,
        FxRandomChannel::RotationDelta,
        FxRandomChannel::Scale,
        FxRandomChannel::Size,
        FxRandomChannel::SpawnAngleYaw,
        FxRandomChannel::SpawnHeight,
        FxRandomChannel::SpawnOriginX,
        FxRandomChannel::SpawnOriginY,
        FxRandomChannel::SpawnOriginZ,
        FxRandomChannel::SpawnPitch,
        FxRandomChannel::SpawnRadius,
        FxRandomChannel::SpawnRoll,
        FxRandomChannel::SpawnYaw,
        FxRandomChannel::VelocityX,
        FxRandomChannel::VelocityY,
        FxRandomChannel::VelocityZ,
        FxRandomChannel::Visual,
    ];

    const KEYS: u64 = 1_000_000;

    /// 99.9% point of chi-square with 63 degrees of freedom (64 buckets).
    const CHI_SQUARE_63_999: f64 = 103.442;

    fn fast_sample_at(key: u64, channel: FxRandomChannel, index: u64) -> f64 {
        f64::from(unit(fast_sample_word(key, channel, index)))
    }

    #[derive(Default)]
    struct Pearson {
        n: f64,
        x: f64,
        y: f64,
        xx: f64,
        yy: f64,
        xy: f64,
    }

    impl Pearson {
        fn add(&mut self, x: f64, y: f64) {
            self.n += 1.0;
            self.x += x;
            self.y += y;
            self.xx += x * x;
            self.yy += y * y;
            self.xy += x * y;
        }

        fn r(&self) -> f64 {
            let cov = self.xy - self.x * self.y / self.n;
            let var_x = self.xx - self.x * self.x / self.n;
            let var_y = self.yy - self.y * self.y / self.n;
            cov / libm::sqrt(var_x * var_y)
        }
    }

    #[test]
    fn fast_samples_are_uniform_on_every_channel() {
        for channel in CHANNELS {
            let mut sum = 0.0;
            let mut buckets = [0_u32; 64];
            for key in 0..KEYS {
                let u = unit(fast_sample_word(key, channel, 0));
                sum += f64::from(u);
                buckets[(u * 64.0) as usize] += 1;
            }
            let mean = sum / KEYS as f64;
            assert!((mean - 0.5).abs() < 0.002, "{channel:?}: mean {mean}");
            let expected = KEYS as f64 / 64.0;
            let chi_square: f64 = buckets
                .iter()
                .map(|&n| {
                    let d = f64::from(n) - expected;
                    d * d / expected
                })
                .sum();
            assert!(
                chi_square < CHI_SQUARE_63_999,
                "{channel:?}: chi-square {chi_square}"
            );
        }
    }

    #[test]
    fn fast_samples_are_uncorrelated_across_indices() {
        // Index 0 and 1 of each key, as a particle's repeated draws use them.
        for channel in CHANNELS {
            let mut adjacent = Pearson::default();
            for key in 0..KEYS {
                adjacent.add(
                    fast_sample_at(key, channel, 0),
                    fast_sample_at(key, channel, 1),
                );
            }
            let r = adjacent.r();
            assert!(r.abs() < 0.01, "{channel:?}: r {r}");
        }
        // One key's consecutive indices, as glass walks its stream from key 0.
        for channel in [FxRandomChannel::GlassCrack, FxRandomChannel::GlassShatter] {
            let mut lag = Pearson::default();
            let mut prev = fast_sample_at(0, channel, 0);
            for index in 1..=KEYS {
                let next = fast_sample_at(0, channel, index);
                lag.add(prev, next);
                prev = next;
            }
            let r = lag.r();
            assert!(r.abs() < 0.01, "{channel:?} stream: r {r}");
        }
    }

    #[test]
    fn fast_channels_of_one_key_are_uncorrelated() {
        const N: usize = CHANNELS.len();
        let mut sum = [0.0_f64; N];
        let mut square = [0.0_f64; N];
        let mut cross = [[0.0_f64; N]; N];
        for key in 0..KEYS {
            let v: [f64; N] = core::array::from_fn(|c| fast_sample_at(key, CHANNELS[c], 0));
            for a in 0..N {
                sum[a] += v[a];
                square[a] += v[a] * v[a];
                for b in a + 1..N {
                    cross[a][b] += v[a] * v[b];
                }
            }
        }
        let n = KEYS as f64;
        for a in 0..N {
            for b in a + 1..N {
                let cov = cross[a][b] - sum[a] * sum[b] / n;
                let var_a = square[a] - sum[a] * sum[a] / n;
                let var_b = square[b] - sum[b] * sum[b] / n;
                let r = cov / libm::sqrt(var_a * var_b);
                assert!(
                    r.abs() < 0.01,
                    "{:?} / {:?}: r {r}",
                    CHANNELS[a],
                    CHANNELS[b]
                );
            }
        }
    }

    #[test]
    fn fast_seed_functions_differ_for_equal_inputs() {
        for x in 0..4096_u64 {
            for msec in [0, 1, 1000, -1] {
                let mut words = [0_u64; 3 + CHANNELS.len()];
                words[0] = fast_effect_random_key(x, msec);
                words[1] = fast_elem_random_seed(x, 0, msec);
                words[2] = fast_trail_random_seed(x, 0);
                for (word, channel) in words[3..].iter_mut().zip(CHANNELS) {
                    *word = fast_sample_word(x, channel, u64::from(msec as u32));
                }
                for a in 0..words.len() {
                    for b in a + 1..words.len() {
                        assert_ne!(words[a], words[b], "x {x} msec {msec}: {a} == {b}");
                    }
                }
            }
        }
    }

    #[test]
    fn fast_words_do_not_collide() {
        // Small consecutive inputs, as pool slots, times and indices are.
        let mut words = Vec::new();
        for instance in 0..4096 {
            for msec in 0..64 {
                words.push(fast_effect_random_key(instance, msec));
            }
        }
        for key in 0..2048 {
            for channel in CHANNELS {
                for index in 0..16 {
                    words.push(fast_sample_word(key, channel, index));
                }
            }
        }
        let count = words.len();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), count);
    }

    // BLAKE3 values from f387b40, before the mixer existed.
    const GOLDEN_EFFECT: u64 = 0x4822_a2f5_28bc_aa86;
    const GOLDEN_ELEM: u64 = 0xc1c0_1b74_af7b_7bb2;
    const GOLDEN_TRAIL: u64 = 0x910c_2a7b_8fdd_ff1d;
    const GOLDEN_U16: u16 = 0x4502;
    const GOLDEN_F32: u32 = 0x3f59_7e81;
    const GOLDEN_AT: u32 = 0x3f25_02a8;
    const GOLDEN_VISUAL: usize = 1;

    /// The only test that touches the switch, so no other test sees it on.
    #[test]
    fn switch_off_is_blake3_and_on_is_the_mixer() {
        use FxRandomChannel::{Size, VelocityZ, Visual};
        let key = 0x0123_4567_89ab_cdef;
        let blake3 = || {
            assert_eq!(effect_random_key(7, 1234), GOLDEN_EFFECT);
            assert_eq!(elem_random_seed(key, 3, 1234), GOLDEN_ELEM);
            assert_eq!(trail_random_seed(key, -2), GOLDEN_TRAIL);
            assert_eq!(sample_u16(key, Visual), GOLDEN_U16);
            assert_eq!(sample_f32(key, Size).to_bits(), GOLDEN_F32);
            assert_eq!(sample_at(key, VelocityZ, 5).to_bits(), GOLDEN_AT);
            assert_eq!(elem_visual_index(7, key), GOLDEN_VISUAL);
        };
        blake3();
        set_fast_random(true);
        assert_eq!(effect_random_key(7, 1234), fast_effect_random_key(7, 1234));
        assert_eq!(
            elem_random_seed(key, 3, 1234),
            fast_elem_random_seed(key, 3, 1234)
        );
        assert_eq!(trail_random_seed(key, -2), fast_trail_random_seed(key, -2));
        assert_eq!(
            sample_u16(key, Visual),
            (fast_sample_word(key, Visual, 0) >> 48) as u16
        );
        assert_eq!(sample_f32(key, Size), unit(fast_sample_word(key, Size, 0)));
        assert_eq!(
            sample_at(key, VelocityZ, 5),
            unit(fast_sample_word(key, VelocityZ, 5))
        );
        assert_ne!(effect_random_key(7, 1234), GOLDEN_EFFECT);
        set_fast_random(false);
        blake3();
    }
}
