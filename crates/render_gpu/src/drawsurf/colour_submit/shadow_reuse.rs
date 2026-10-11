//! `IW4L_SHADOW_REUSE`: sun shadow flushes found again by what they draw
//! (off by default).
//!
//! The resident sun commands keep each partition's prepared world and static
//! model flushes by their place in the caster list. A caster entering or
//! leaving the partition frustum moves every flush after it, so a turn or a
//! strafe prepares most of the partition again: the material executed, the
//! pipeline looked up, the textures bound and the constants packed for every
//! flush. With the switch on, a flush whose owner (the same record the
//! resident list compares) and technique were prepared before is taken from a
//! memo: its draws are cloned and given this frame's index range, as a fresh
//! prepare gives them. Its constants are those the resident list freezes at a
//! prepare and patches every frame (`ResidentSunCommands::patch_sun_registers`);
//! a kept flush is prepared again after `MAX_AGE_FRAMES` so they never age
//! further than that.
//!
//! A refusal that does not wait on loading is remembered for a short while
//! too, so a flush that cannot be prepared is not prepared again, and the list
//! is not rebuilt for it, every frame.
//!
//! Entity model flushes are prepared every frame. With the switch on, one
//! whose item is unchanged bit for bit (pose included) is taken from the memo
//! too, so models that stand still, such as clutter at rest, cost a lookup.
//!
//! The memo is dropped when the material generation changes or the shadow
//! texture bindings are cleared. A static entry stays while the map does (the
//! resident list holds its flush while the camera stands still, so it is not
//! looked up then); entity entries not used for a second go, since a moving
//! model leaves a new one every frame.
//!
//! The verifier (`IW4L_SHADOW_REUSE_VERIFY=1`, a Mac switch) prepares every
//! hit again, compares the records and draws the fresh one.
use bevy::platform::collections::HashMap;
use render_frame::{RetainedDrawItem, RetainedDrawKind};
use render_material::TechType;

use super::PreparedExactDraw;
use super::shadow_prepare::{
    ShadowStaticOwner, ShadowmapSunFlushGpu, smodel_shadow_static_owner, world_shadow_static_owner,
};

pub(super) const SHADOW_REUSE_ENV: &str = "IW4L_SHADOW_REUSE";
pub(super) const SHADOW_REUSE_VERIFY_ENV: &str = "IW4L_SHADOW_REUSE_VERIFY";

/// Frames an unused entity entry is kept.
const ENTITY_KEEP_FRAMES: u64 = 60;
/// Static entries kept before the least recently used are dropped.
const STATIC_CAP: usize = 32_768;
/// Frames a remembered refusal is kept before the flush is tried again.
const REFUSAL_FRAMES: u64 = 120;
/// Frames a kept flush is used before it is prepared again, so the code
/// constants it froze at its prepare are never older than this.
const MAX_AGE_FRAMES: u64 = 600;

pub(super) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| perf::switch(SHADOW_REUSE_ENV))
}

pub(super) fn verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| enabled() && perf::switch(SHADOW_REUSE_VERIFY_ENV))
}

/// The draw a flush is prepared from: for a world surface or a static model
/// the record the resident list compares, for an entity model every field of
/// its item, pose included.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum FlushOwner {
    Static(ShadowStaticOwner),
    Entity {
        key: u64,
        surface: u32,
        material: u32,
        object_id: u16,
        world_from_local: [u32; 16],
        lighting_handle: u32,
        packed_lighting: Option<[u8; 4]>,
        is_scope: bool,
        scene_entnum: Option<u32>,
        samplers: render_frame::SurfaceSamplerInputs,
    },
}

/// What a flush is prepared from: its owner, material and technique.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct ShadowFlushId {
    owner: FlushOwner,
    material_id: Option<render_material::MaterialAssetId>,
    material_rank: u32,
    tech: TechType,
}

impl ShadowFlushId {
    /// `None` for a draw the memo does not keep.
    pub(super) fn of(item: &RetainedDrawItem, tech: TechType) -> Option<Self> {
        let owner = match item.kind {
            RetainedDrawKind::World { .. } => FlushOwner::Static(world_shadow_static_owner(item)?),
            RetainedDrawKind::Smodel { .. } => {
                FlushOwner::Static(smodel_shadow_static_owner(item)?)
            }
            RetainedDrawKind::XModel {
                surface,
                material,
                object_id,
                world_from_local,
                lighting_handle,
                packed_lighting,
                is_scope,
                scene_entnum,
            } => FlushOwner::Entity {
                key: item.key,
                surface,
                material,
                object_id,
                world_from_local: world_from_local.to_cols_array().map(f32::to_bits),
                lighting_handle,
                packed_lighting,
                is_scope,
                scene_entnum,
                samplers: item.surface_samplers,
            },
            _ => return None,
        };
        Some(Self {
            owner,
            material_id: item.material_id,
            material_rank: item.material_rank,
            tech,
        })
    }

    fn is_entity(&self) -> bool {
        matches!(self.owner, FlushOwner::Entity { .. })
    }
}

struct Entry {
    result: Result<ShadowmapSunFlushGpu, String>,
    stored: u64,
    used: u64,
}

pub(super) enum Remembered {
    Prepared(ShadowmapSunFlushGpu),
    Refused(String),
    None,
}

/// A refusal that will be the same next frame: not one waiting on a
/// pipeline, on the frame's material tables or light bindings, or on
/// geometry that is still being uploaded.
fn refusal_repeats(cause: &str) -> bool {
    const WAITS: [&str; 7] = [
        "NotReady",
        "MaterialTablesMissing",
        "ExecRefused",
        "WorldVertex",
        "PackedVertex",
        "MissingSurfaceRange",
        "MissingSmodelRange",
    ];
    !WAITS.iter().any(|wait| cause.contains(wait))
}

#[derive(Default, Clone, Copy)]
pub(super) struct ShadowReuseTally {
    pub(super) hits: u64,
    pub(super) prepared: u64,
    pub(super) refusal_hits: u64,
    pub(super) refused: u64,
    pub(super) rebuilds: u64,
    pub(super) xmodel_flushes: u64,
    pub(super) skinned_flushes: u64,
    pub(super) verified: u64,
    pub(super) mismatches: u64,
    /// Hits whose code constants other than the patched sites differ from a
    /// fresh prepare: values the resident list freezes too.
    pub(super) frozen_differ: u64,
}

#[derive(Default)]
pub(super) struct ShadowFlushMemo {
    epoch: Option<(u64, u64)>,
    frame: u64,
    entries: HashMap<ShadowFlushId, Entry>,
    pub(super) tally: ShadowReuseTally,
    logged_at: u64,
    logged_mismatches: u64,
}

impl ShadowFlushMemo {
    /// Start a frame: drop everything when the material generation or the
    /// shadow texture bindings changed, and evict what was not used lately.
    pub(super) fn open_frame(&mut self, generation: u64, binding_clears: u64) {
        self.frame += 1;
        if self.epoch != Some((generation, binding_clears)) {
            self.epoch = Some((generation, binding_clears));
            self.entries.clear();
        }
        let frame = self.frame;
        if frame.is_multiple_of(ENTITY_KEEP_FRAMES) {
            self.entries.retain(|id, entry| {
                !id.is_entity() || frame.saturating_sub(entry.used) < ENTITY_KEEP_FRAMES
            });
        }
        if self.entries.len() > STATIC_CAP {
            let mut used: Vec<u64> = self.entries.values().map(|entry| entry.used).collect();
            used.sort_unstable();
            let cut = used[used.len() - STATIC_CAP / 2];
            self.entries.retain(|_, entry| entry.used >= cut);
        }
    }

    /// The flush prepared before from `id`, given this frame's index range.
    pub(super) fn lookup(
        &mut self,
        id: &ShadowFlushId,
        draw_start: u32,
        draw_count: u32,
        ring_epoch: u32,
    ) -> Remembered {
        let frame = self.frame;
        let Some(entry) = self.entries.get_mut(id) else {
            return Remembered::None;
        };
        match &entry.result {
            Ok(_) if frame.saturating_sub(entry.stored) >= MAX_AGE_FRAMES => Remembered::None,
            Ok(gpu) => {
                entry.used = frame;
                let mut gpu = gpu.clone();
                retarget(&mut gpu.draws, draw_start, draw_count, ring_epoch);
                Remembered::Prepared(gpu)
            }
            Err(cause) if frame.saturating_sub(entry.stored) < REFUSAL_FRAMES => {
                Remembered::Refused(cause.clone())
            }
            Err(_) => Remembered::None,
        }
    }

    /// Remember what preparing `id` gave.
    pub(super) fn store(
        &mut self,
        id: ShadowFlushId,
        result: &Result<ShadowmapSunFlushGpu, String>,
    ) {
        let result = match result {
            Ok(gpu) => Ok(gpu.clone()),
            Err(cause) if refusal_repeats(cause) => Err(cause.clone()),
            Err(_) => return,
        };
        let frame = self.frame;
        self.entries.insert(
            id,
            Entry {
                result,
                stored: frame,
                used: frame,
            },
        );
    }

    /// End a frame: the stats line every 600 frames, and at once when the
    /// verifier found a new mismatch.
    pub(super) fn close_frame(&mut self) {
        let grew = self.tally.mismatches > self.logged_mismatches;
        self.logged_mismatches = self.tally.mismatches;
        if !grew && self.frame.saturating_sub(self.logged_at) < 600 {
            return;
        }
        self.logged_at = self.frame;
        let t = self.tally;
        diag::info!(
            World,
            "shadow reuse: frames={} hits={} prepared={} refusal_hits={} refused={} rebuilds={} xmodel_flushes={} skinned_flushes={} entries={} verified={} mismatches={} frozen_differ={}",
            self.frame,
            t.hits,
            t.prepared,
            t.refusal_hits,
            t.refused,
            t.rebuilds,
            t.xmodel_flushes,
            t.skinned_flushes,
            self.entries.len(),
            t.verified,
            t.mismatches,
            t.frozen_differ,
        );
    }
}

/// Give prepared draws the index range of the flush they are used for, as
/// `prepare_shadow_flush_draws` does for a fresh prepare.
fn retarget(draws: &mut [PreparedExactDraw], start: u32, count: u32, ring_epoch: u32) {
    for draw in draws {
        if draw.tess != super::ExactTessBind::SmodelSkinned {
            draw.start = start;
            draw.count = count;
            draw.ring_epoch = ring_epoch;
        }
    }
}

/// The first field two prepared shadow flushes differ in, for the verifier.
/// Code constants are compared by what they read, not their values: the
/// resident list patches the per-frame sites and freezes the rest at the
/// first prepare. Constant buffers are compared by size.
pub(super) fn flush_mismatch(
    kept: &ShadowmapSunFlushGpu,
    fresh: &ShadowmapSunFlushGpu,
) -> Option<&'static str> {
    if kept.draws.len() != fresh.draws.len() {
        return Some("pass count");
    }
    if kept.world_from_local.to_cols_array().map(f32::to_bits)
        != fresh.world_from_local.to_cols_array().map(f32::to_bits)
    {
        return Some("world_from_local");
    }
    let reads = |code: &[render_material::PackedCodeConstants]| {
        code.iter()
            .map(|pass| {
                pass.lanes
                    .iter()
                    .map(|lane| (lane.stage, lane.destination, lane.index, lane.row_count))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    };
    if reads(&kept.pass_code) != reads(&fresh.pass_code) {
        return Some("code constants read");
    }
    for (a, b) in kept.draws.iter().zip(&fresh.draws) {
        if a.pipeline != b.pipeline {
            return Some("pipeline");
        }
        if a.port != b.port {
            return Some("port");
        }
        if a.texture_slots[..] != b.texture_slots[..] {
            return Some("texture slots");
        }
        if (a.start, a.count, a.ring_epoch) != (b.start, b.count, b.ring_epoch) {
            return Some("index range");
        }
        if a.tess != b.tess {
            return Some("tess");
        }
        if a.state != b.state {
            return Some("state");
        }
        if a.depth_min.to_bits() != b.depth_min.to_bits()
            || a.depth_max.to_bits() != b.depth_max.to_bits()
        {
            return Some("depth range");
        }
        if a.smc_stream_off != b.smc_stream_off {
            return Some("stream");
        }
        let size = |d: &PreparedExactDraw| {
            d.constants
                .as_ref()
                .map(|c| (c.vertex.len(), c.pixel.len()))
        };
        if size(a) != size(b) {
            return Some("constants size");
        }
    }
    None
}

/// Rows the resident list writes every frame (`sun_code_sites`).
fn patched_index(index: u16) -> Option<usize> {
    match index {
        crate::drawsurf::CODE_TRANSPOSE_WORLD_VIEW_PROJECTION0
        | render_backend::overlay::CODE_TRANSPOSE_PROJECTION
        | render_backend::overlay::CODE_TRANSPOSE_VIEW_PROJECTION => Some(4),
        crate::drawsurf::CODE_SHADOWMAP_POLYGON_OFFSET => Some(1),
        _ => None,
    }
}

/// How two prepared flushes' constant values compare, the patched rows
/// aside: `Err` names a difference in rows no code constant writes (a real
/// mismatch); `Ok(true)` is a difference in other code constants only, which
/// the resident list freezes at a prepare as well.
pub(super) fn constants_compare(
    kept: &ShadowmapSunFlushGpu,
    fresh: &ShadowmapSunFlushGpu,
) -> Result<bool, &'static str> {
    use render_material::RuntimeShaderStage;
    let mut frozen_differ = false;
    for ((a, b), code) in kept.draws.iter().zip(&fresh.draws).zip(&fresh.pass_code) {
        let (Some(ca), Some(cb)) = (a.constants.as_ref(), b.constants.as_ref()) else {
            continue;
        };
        for (stage, rows_a, rows_b) in [
            (RuntimeShaderStage::Vertex, &ca.vertex, &cb.vertex),
            (RuntimeShaderStage::Pixel, &ca.pixel, &cb.pixel),
        ] {
            if rows_a.len() != rows_b.len() {
                return Err("constants size");
            }
            // 0: material row, 1: code constant row, 2: patched row.
            let mut kind = vec![0u8; rows_a.len()];
            for lane in code.lanes.iter().filter(|lane| lane.stage == stage) {
                let first = usize::from(lane.destination);
                let (n, mark) = match patched_index(lane.index) {
                    Some(n) => (n, 2),
                    None => (usize::from(lane.row_count), 1),
                };
                for row in first..first.saturating_add(n).min(kind.len()) {
                    kind[row] = kind[row].max(mark);
                }
            }
            for ((ra, rb), kind) in rows_a.iter().zip(rows_b.iter()).zip(&kind) {
                if ra == rb {
                    continue;
                }
                match kind {
                    0 => return Err("material constants"),
                    1 => frozen_differ = true,
                    _ => {}
                }
            }
        }
    }
    Ok(frozen_differ)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Mat4;

    fn smodel(placement: u32, key: u64) -> RetainedDrawItem {
        RetainedDrawItem {
            material_id: None,
            key,
            material_rank: 7,
            kind: RetainedDrawKind::Smodel {
                surface: 3,
                material: 4,
                world_from_local: Mat4::IDENTITY,
                lighting_handle: 5,
                packed_lighting: None,
                placement,
                stream: Some(lighting_iw4::SmodelSurfPath::Rigid),
                cache_index: None,
                pretess: None,
            },
            surface_samplers: Default::default(),
            camera_region: None,
        }
    }

    fn flush() -> ShadowmapSunFlushGpu {
        ShadowmapSunFlushGpu {
            draws: Vec::new(),
            world_from_local: Mat4::from_translation(bevy::math::Vec3::X),
            pass_code: Vec::new(),
        }
    }

    fn found(memo: &mut ShadowFlushMemo, id: &ShadowFlushId) -> &'static str {
        match memo.lookup(id, 0, 3, 0) {
            Remembered::Prepared(_) => "prepared",
            Remembered::Refused(_) => "refused",
            Remembered::None => "none",
        }
    }

    #[test]
    fn a_flush_is_found_again_by_its_owner_and_technique() {
        let mut memo = ShadowFlushMemo::default();
        memo.open_frame(1, 0);
        let id = ShadowFlushId::of(&smodel(9, 0x40), TechType(2)).expect("static model");
        assert_eq!(found(&mut memo, &id), "none");
        memo.store(id.clone(), &Ok(flush()));
        assert_eq!(found(&mut memo, &id), "prepared");
        let other_tech = ShadowFlushId::of(&smodel(9, 0x40), TechType(3)).unwrap();
        assert_eq!(found(&mut memo, &other_tech), "none");
        let other_place = ShadowFlushId::of(&smodel(10, 0x40), TechType(2)).unwrap();
        assert_eq!(found(&mut memo, &other_place), "none");
    }

    #[test]
    fn a_lasting_refusal_is_remembered_for_a_while_and_a_wait_is_not() {
        let mut memo = ShadowFlushMemo::default();
        memo.open_frame(1, 0);
        let id = ShadowFlushId::of(&smodel(1, 7), TechType(2)).unwrap();
        memo.store(id.clone(), &Err("Smodel:PipelineNotReady".into()));
        assert_eq!(
            found(&mut memo, &id),
            "none",
            "a pipeline wait is tried again"
        );
        memo.store(id.clone(), &Err("Smodel:TextureBind(Missing)".into()));
        assert_eq!(found(&mut memo, &id), "refused");
        for _ in 0..REFUSAL_FRAMES {
            memo.open_frame(1, 0);
        }
        assert_eq!(found(&mut memo, &id), "none", "tried again after a while");
    }

    #[test]
    fn a_kept_flush_is_prepared_again_once_it_is_old() {
        let mut memo = ShadowFlushMemo::default();
        let id = ShadowFlushId::of(&smodel(1, 7), TechType(2)).unwrap();
        memo.open_frame(1, 0);
        memo.store(id.clone(), &Ok(flush()));
        for _ in 1..MAX_AGE_FRAMES {
            memo.open_frame(1, 0);
            assert_eq!(found(&mut memo, &id), "prepared");
        }
        memo.open_frame(1, 0);
        assert_eq!(found(&mut memo, &id), "none");
    }

    #[test]
    fn refusals_that_wait_on_loading_are_tried_again() {
        let mut memo = ShadowFlushMemo::default();
        memo.open_frame(1, 0);
        let id = ShadowFlushId::of(&smodel(1, 7), TechType(2)).unwrap();
        for cause in [
            "World:ExecRefused",
            "Smodel:WorldVertex(ForeignLayout)",
            "Smodel:MissingSmodelRange",
        ] {
            memo.store(id.clone(), &Err(cause.into()));
            assert_eq!(found(&mut memo, &id), "none", "{cause}");
        }
    }

    #[test]
    fn a_new_generation_or_cleared_bindings_drop_everything() {
        let mut memo = ShadowFlushMemo::default();
        let id = ShadowFlushId::of(&smodel(1, 7), TechType(2)).unwrap();
        memo.open_frame(1, 0);
        memo.store(id.clone(), &Ok(flush()));
        memo.open_frame(1, 0);
        assert_eq!(found(&mut memo, &id), "prepared");
        memo.open_frame(1, 1);
        assert_eq!(found(&mut memo, &id), "none");
        memo.store(id.clone(), &Ok(flush()));
        memo.open_frame(2, 1);
        assert_eq!(found(&mut memo, &id), "none");
    }

    fn entity(world_x: f32) -> RetainedDrawItem {
        RetainedDrawItem {
            material_id: None,
            key: 0x77,
            material_rank: 2,
            kind: RetainedDrawKind::XModel {
                surface: 1,
                material: 2,
                object_id: 3,
                world_from_local: Mat4::from_translation(bevy::math::Vec3::new(world_x, 0.0, 0.0)),
                lighting_handle: 0,
                packed_lighting: None,
                is_scope: false,
                scene_entnum: Some(40),
            },
            surface_samplers: Default::default(),
            camera_region: None,
        }
    }

    #[test]
    fn an_entity_flush_is_found_again_only_where_it_stood() {
        let mut memo = ShadowFlushMemo::default();
        memo.open_frame(1, 0);
        let still = ShadowFlushId::of(&entity(5.0), TechType(2)).expect("entity model");
        memo.store(still.clone(), &Ok(flush()));
        assert_eq!(found(&mut memo, &still), "prepared");
        let moved = ShadowFlushId::of(&entity(5.5), TechType(2)).unwrap();
        assert_eq!(found(&mut memo, &moved), "none");
    }

    #[test]
    fn unused_entity_entries_go_and_static_ones_stay() {
        let mut memo = ShadowFlushMemo::default();
        let stat = ShadowFlushId::of(&smodel(1, 7), TechType(2)).unwrap();
        let ent = ShadowFlushId::of(&entity(1.0), TechType(2)).unwrap();
        memo.open_frame(1, 0);
        memo.store(stat.clone(), &Ok(flush()));
        memo.store(ent.clone(), &Ok(flush()));
        for _ in 0..ENTITY_KEEP_FRAMES * 3 {
            memo.open_frame(1, 0);
        }
        assert_eq!(found(&mut memo, &ent), "none");
        assert_eq!(found(&mut memo, &stat), "prepared");
    }
}
