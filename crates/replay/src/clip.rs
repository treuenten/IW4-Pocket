use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use bevy::prelude::Resource;
use net::AUTHORITY_MS;
use playerstate_iw4::PlayerState;
use sim::Snapshot;

use crate::file::{demo_stem, sanitize_demo_name};
use crate::{ReplayError, demo_path};

pub const CLIP_MS: u32 = 45_000;

pub const CLIP_TICK_MS: u32 = 50;

pub const CLIP_TICKS: usize = (CLIP_MS / CLIP_TICK_MS) as usize;

pub const CLIP_LATEST: &str = "LATEST";

pub const CLIP_DEMO_FILE: &str = "clip.iw4ldemo";
pub const CLIP_DUMP_FILE: &str = "dump.txt";
pub const CLIP_MANIFEST_FILE: &str = "manifest.toml";

pub const CLIP_BYTE_BUDGET: usize = 32 * 1024 * 1024;

pub const CLIP_RING_ENV: &str = "IW4L_CLIP_RING";

const _: () = assert!(CLIP_TICKS == 900);
const _: () = assert!(CLIP_TICK_MS as i32 == AUTHORITY_MS);

/// Keep the last 45 s of snapshots for the console `clip` command.
///
/// On by default on desktop, where the harness saves clips; off by default on
/// iOS. The ring copies the whole snapshot every tick, and its byte budget
/// counts players, projectiles and entities but not the meta tables, so on the
/// phone it held about 150 MiB and grew about 10 MiB/min for a command nobody
/// types there. Set IW4L_CLIP_RING=0 to turn it off on desktop;
/// `IW4L_CLIP_RING=1` in iw4l-env.txt turns it on for the phone.
pub fn clip_ring_enabled() -> bool {
    let set = std::env::var_os(CLIP_RING_ENV).map(|_| perf::switch(CLIP_RING_ENV));
    clip_ring_choice(cfg!(target_os = "ios"), set)
}

fn clip_ring_choice(ios: bool, set: Option<bool>) -> bool {
    set.unwrap_or(!ios)
}

#[derive(Clone, Debug)]
struct ClipTick {
    snapshot: Snapshot,
    bytes: usize,
}

#[derive(Resource, Debug)]
pub struct ClipRing {
    ticks: VecDeque<ClipTick>,
    bytes: usize,
    max_ticks: usize,
    byte_budget: usize,
    enabled: bool,
}

impl Default for ClipRing {
    fn default() -> Self {
        Self::with_limits(CLIP_TICKS, CLIP_BYTE_BUDGET).with_enabled(clip_ring_enabled())
    }
}

impl ClipRing {
    pub fn with_limits(max_ticks: usize, byte_budget: usize) -> Self {
        Self {
            ticks: VecDeque::new(),
            bytes: 0,
            max_ticks: max_ticks.max(1),
            byte_budget,
            enabled: true,
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Copies `snapshot` in when the ring is on. An off ring copies nothing.
    pub fn record(&mut self, snapshot: &Snapshot) {
        if self.enabled {
            self.push(snapshot.clone());
        }
    }

    pub fn push(&mut self, snapshot: Snapshot) {
        if self
            .ticks
            .back()
            .is_some_and(|last| snapshot.tick <= last.snapshot.tick)
        {
            return;
        }
        let bytes = clip_tick_bytes(&snapshot);
        self.bytes = self.bytes.saturating_add(bytes);
        self.ticks.push_back(ClipTick { snapshot, bytes });
        self.evict();
    }

    fn evict(&mut self) {
        while self.ticks.len() > 1
            && (self.ticks.len() > self.max_ticks
                || self.bytes > self.byte_budget
                || self.duration_ms() > CLIP_MS)
        {
            let Some(oldest) = self.ticks.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(oldest.bytes);
        }
    }

    pub fn clear(&mut self) {
        self.ticks.clear();
        self.bytes = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.ticks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ticks.len()
    }

    pub fn duration_ms(&self) -> u32 {
        match (self.ticks.front(), self.ticks.back()) {
            (Some(first), Some(last)) => last
                .snapshot
                .tick
                .0
                .saturating_sub(first.snapshot.tick.0)
                .saturating_add(1)
                .saturating_mul(CLIP_TICK_MS),
            _ => 0,
        }
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn iter(&self) -> impl Iterator<Item = &Snapshot> {
        self.ticks.iter().map(|tick| &tick.snapshot)
    }
}

fn clip_tick_bytes(snapshot: &Snapshot) -> usize {
    const SNAPSHOT_BASE: usize = 64;
    const ENTITY_STATE: usize = 0x100;
    SNAPSHOT_BASE
        + snapshot.players.len() * core::mem::size_of::<PlayerState>()
        + snapshot.projectiles.len() * 64
        + snapshot.meta.entities.len() * ENTITY_STATE
}

pub fn clips_dir(artifacts_root: &Path) -> PathBuf {
    artifacts_root.join("clips")
}

pub fn clip_dir(artifacts_root: &Path, id: &str) -> PathBuf {
    clips_dir(artifacts_root).join(id)
}

pub fn clip_demo_path(artifacts_root: &Path, id: &str) -> PathBuf {
    clip_dir(artifacts_root, id).join(CLIP_DEMO_FILE)
}

pub fn create_clip_dir(artifacts_root: &Path, id: &str) -> Result<PathBuf, ReplayError> {
    let dir = clip_dir(artifacts_root, id);
    std::fs::create_dir_all(clips_dir(artifacts_root))?;
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

pub fn rewrite_latest_symlink(artifacts_root: &Path, id: &str) -> Result<(), ReplayError> {
    let clips = clips_dir(artifacts_root);
    std::fs::create_dir_all(&clips)?;
    let latest = clips.join(CLIP_LATEST);
    remove_latest_pointer(&latest)?;
    create_latest_pointer(id, &latest)?;
    Ok(())
}

#[cfg(unix)]
fn remove_latest_pointer(latest: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(latest) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn remove_latest_pointer(latest: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_dir(latest) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn create_latest_pointer(id: &str, latest: &Path) -> Result<(), std::io::Error> {
    std::os::unix::fs::symlink(id, latest)
}

#[cfg(windows)]
fn create_latest_pointer(id: &str, latest: &Path) -> Result<(), std::io::Error> {
    std::os::windows::fs::symlink_dir(id, latest)
}

pub fn clip_manifest(
    id: &str,
    ticks: u64,
    duration_ms: u32,
    zone: &str,
    captured_unix_ns: u128,
) -> String {
    format!(
        "id = \"{id}\"\n\
         ticks = {ticks}\n\
         duration_ms = {duration_ms}\n\
         zone = {zone:?}\n\
         captured_unix_ns = {captured_unix_ns}\n\
         demo = \"{CLIP_DEMO_FILE}\"\n\
         dump = \"{CLIP_DUMP_FILE}\"\n"
    )
}

pub fn existing_clip_demo(artifacts_root: &Path, requested: &str) -> Option<PathBuf> {
    let name = sanitize_demo_name(demo_stem(requested)).unwrap_or_else(|| requested.to_owned());
    let id = name.to_ascii_uppercase();
    let path = clip_demo_path(artifacts_root, &id);
    path.exists().then_some(path)
}

pub fn resolve_playback_path(artifacts_root: &Path, requested: &str) -> PathBuf {
    let name = sanitize_demo_name(demo_stem(requested)).unwrap_or_else(|| requested.to_owned());
    let demo = demo_path(artifacts_root, &name);
    if demo.exists() {
        return demo;
    }
    existing_clip_demo(artifacts_root, &name).unwrap_or(demo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::Tick;

    #[test]
    fn clip_ring_is_on_on_desktop_and_off_on_ios_unless_set() {
        assert!(clip_ring_choice(false, None));
        assert!(!clip_ring_choice(true, None));
        assert!(clip_ring_choice(true, Some(true)));
        assert!(!clip_ring_choice(false, Some(false)));
        assert!(clip_ring_choice(false, Some(true)));
        assert!(!clip_ring_choice(true, Some(false)));
    }

    #[test]
    fn an_off_ring_records_nothing() {
        let snapshot = Snapshot::unpublished(Tick(7));
        let mut off = ClipRing::with_limits(CLIP_TICKS, CLIP_BYTE_BUDGET).with_enabled(false);
        off.record(&snapshot);
        assert!(!off.is_enabled());
        assert!(off.is_empty());
        let mut on = ClipRing::with_limits(CLIP_TICKS, CLIP_BYTE_BUDGET);
        on.record(&snapshot);
        assert!(on.is_enabled());
        assert_eq!(on.len(), 1);
    }
}
