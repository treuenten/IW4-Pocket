//! `IW4L_PREWARM`: build while the map loads what the first moments of play
//! would otherwise build on the spot (off by default).
//!
//! The load-time pipeline working set (`colour_submit::pipeline`) holds every
//! lit, sun-shadow and emissive pipeline the map's materials can draw with.
//! Two kinds of pipeline are left out of it and are made the first time a draw
//! asks for one, which leaves that draw out until the build is done:
//!
//! - the passes a dynamic light adds: a muzzle flash, an explosion or a flare
//!   draws the surfaces it reaches once more with the spot or omni light
//!   technique, blended on top (on the Mac: 10 pipelines at the first shot);
//! - everything else a draw first needs in play: the decals left by bullets
//!   and explosions (one per mark material, light and surface vertex format),
//!   glass pieces, some effect models.
//!
//! With the switch on, the first kind joins the working set outright, and the
//! second is learned: each pipeline made during play is remembered by a hash
//! of its key in the artifact cache, and a later load that can make the same
//! key adds it to the working set. The load waits for both, so they cost load
//! time instead (none measured on the Mac once the system shader cache holds
//! them; the first load after a new build compiles them), and memory: the extra
//! pipelines stay for the match, 0-150 MiB more footprint after the load in the
//! Mac A/Bs (Estate +40-95, Afghan +0-145), against a phone headroom of
//! 0.6-0.9 GB.
use std::hash::{Hash, Hasher};

use bevy::platform::collections::HashSet;
use bevy::prelude::Resource;

/// The extra lit passes of a dynamic light: spot, omni and shadowed spot
/// (`lighting_iw4::additional_light_tech_type`).
pub(crate) const DYNAMIC_LIGHT_TECHS: [u8; 3] = [
    lighting_iw4::TECHNIQUE_LIGHT_SPOT,
    lighting_iw4::TECHNIQUE_LIGHT_OMNI,
    lighting_iw4::TECHNIQUE_LIGHT_SPOT_SHADOW,
];

pub(crate) const PREWARM_ENV: &str = "IW4L_PREWARM";

/// Read once.
pub(crate) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| perf::switch(PREWARM_ENV))
}

const LEARNED_KIND: &str = "prewarm";
/// Bump the version when the key's fields change: old hashes then match nothing.
const LEARNED_KEY: &str = "exact-first-use-v1";
/// Remembered keys across all maps; the oldest go first.
const LEARNED_CAP: usize = 4096;

/// FNV-1a over a key's derived `Hash`: the same key gives the same number in
/// every run of one build. A key from another build that no longer exists
/// simply matches nothing.
pub(crate) fn key_hash(key: &impl Hash) -> u64 {
    struct Fnv(u64);
    impl Hasher for Fnv {
        fn finish(&self) -> u64 {
            self.0
        }
        fn write(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 ^= u64::from(*byte);
                self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
    }
    let mut hasher = Fnv(0xcbf2_9ce4_8422_2325);
    key.hash(&mut hasher);
    hasher.finish()
}

/// Hashes of the pipelines made during play, oldest first, as kept in the
/// artifact cache (one hex number per line).
#[derive(Resource, Default)]
pub(crate) struct LearnedFirstUse {
    loaded: bool,
    order: Vec<u64>,
    set: HashSet<u64>,
}

impl LearnedFirstUse {
    fn load(&mut self) {
        if std::mem::replace(&mut self.loaded, true) {
            return;
        }
        let Some(bytes) = asset_material::cache_get(LEARNED_KIND, LEARNED_KEY) else {
            return;
        };
        self.absorb(&parse(&bytes));
    }

    fn absorb(&mut self, hashes: &[u64]) -> bool {
        let mut added = false;
        for &hash in hashes {
            if self.set.insert(hash) {
                self.order.push(hash);
                added = true;
            }
        }
        if self.order.len() > LEARNED_CAP {
            let drop = self.order.len() - LEARNED_CAP;
            for hash in self.order.drain(..drop) {
                self.set.remove(&hash);
            }
        }
        added
    }

    pub(crate) fn contains(&mut self, hash: u64) -> bool {
        self.load();
        self.set.contains(&hash)
    }

    pub(crate) fn len(&mut self) -> usize {
        self.load();
        self.order.len()
    }

    /// Add these and write the list back when any is new. Rare: a few times
    /// in the first match on a map, then not at all.
    pub(crate) fn remember(&mut self, hashes: &[u64]) {
        self.load();
        if !self.absorb(hashes) {
            return;
        }
        if let Err(error) =
            asset_material::cache_put(LEARNED_KIND, LEARNED_KEY, &format(&self.order))
        {
            diag::warn!(
                World,
                "prewarm: could not save the first-use pipeline list: {error}"
            );
        }
    }
}

fn parse(bytes: &[u8]) -> Vec<u64> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| u64::from_str_radix(line.trim(), 16).ok())
        .collect()
}

fn format(hashes: &[u64]) -> Vec<u8> {
    let mut out = String::with_capacity(hashes.len() * 17);
    for hash in hashes {
        out.push_str(&format!("{hash:016x}\n"));
    }
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_hash_is_fnv_of_the_derived_hash() {
        #[derive(Hash)]
        struct Key {
            a: u8,
            b: u32,
        }
        // Stable across runs: a fixed input gives a fixed number.
        assert_eq!(key_hash(&Key { a: 1, b: 2 }), key_hash(&Key { a: 1, b: 2 }));
        assert_ne!(key_hash(&Key { a: 1, b: 2 }), key_hash(&Key { a: 2, b: 1 }));
        assert_eq!(key_hash(&0u8), {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            h ^= 0;
            h.wrapping_mul(0x0000_0100_0000_01b3)
        });
    }

    #[test]
    fn the_list_round_trips_drops_junk_and_keeps_the_newest() {
        assert_eq!(
            parse(&format(&[1, 0xdead_beef, u64::MAX])),
            vec![1, 0xdead_beef, u64::MAX]
        );
        assert_eq!(parse(b"zz\n00000000000000ff\n\n10\n"), vec![0xff, 0x10]);

        let mut learned = LearnedFirstUse {
            loaded: true,
            ..Default::default()
        };
        assert!(learned.absorb(&[1, 2, 2, 3]));
        assert!(!learned.absorb(&[3, 1]));
        assert_eq!(learned.order, vec![1, 2, 3]);
        let many: Vec<u64> = (10..10 + LEARNED_CAP as u64).collect();
        assert!(learned.absorb(&many));
        assert_eq!(learned.order.len(), LEARNED_CAP);
        assert!(!learned.contains(1), "the oldest went first");
        assert!(learned.contains(10 + LEARNED_CAP as u64 - 1));
    }
}
