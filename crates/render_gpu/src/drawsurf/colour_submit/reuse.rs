//! `IW4L_DRAW_REUSE`: keep the colour lanes' prepared draws from one frame to
//! the next while nothing they were made from has changed (off by default).
//!
//! Every frame the camera colour prepare turns each row of the draw list into
//! one record per material pass (pipeline, texture slots, index range, state)
//! and a span of constants in its lane's arena. In an open view that is about
//! 3,000 rows, and the work grows with them whether or not anything moved:
//! the material is executed again, the texture binding is looked up again
//! under the lock the four lanes share, and the constants are packed and
//! compared again with what the arena already holds.
//!
//! With the switch on:
//!
//! - each lane keeps its own copy of the texture bindings it has looked up
//!   ([`super::LaneBindMemo`]), so a binding seen before costs no lock;
//! - a world or static-model row whose inputs are all unchanged since the last
//!   frame is not prepared again: its records from the last frame are used and
//!   its arena spans are kept alive as they are.
//!
//! What a row's records and constants are made of, and how each is checked:
//!
//! - the frame as a whole ([`ReuseFrameKey`]): the material and pipeline
//!   generations, the texture table and binding epochs, the static geometry,
//!   the sun shadow view, the colour target; and the material frame's inputs
//!   other than its code constants and lights (camera, reflection and
//!   lighting SH, outdoor lookup, code textures), compared bit for bit with
//!   the last frame's ([`same_exec_inputs`]). Any change: no row is kept;
//! - the row, found again by what it draws ([`RowId`]) since effects move it
//!   in the list: its plan entry (index range, and for a world surface the
//!   world index layout it is in), its draw item bit for bit (key, material,
//!   pose, lighting handle, packed lighting, samplers, LOD stream), its
//!   technique; for a cached or pretessellated static model, the static
//!   model cache layout;
//! - its scene light, the one its key names ([`ChangedLights`]);
//! - the code constants: the indices the row's passes read must not have
//!   changed since the last frame (the game time does every frame, so a
//!   material that reads it is prepared every frame);
//! - its arena spans must still exist with the same bases.
//!
//! Only world surfaces and rigid, cached and pretessellated static models are
//! kept. Entity models, the view model, effects, decals, glass, skinned static
//! models and anything that binds a spot shadow are prepared every frame.
//!
//! The material executor carries state from one draw to the next (the run a
//! draw continues, the overlay a new run keeps). A kept row is not executed,
//! so the camera prepare follows what it would have done to that state
//! ([`render_backend::DrawRunKeys`]), and executes the kept rows that would
//! have started a run just before the next row it prepares: every prepared
//! row is executed after the same chain as with the switch off.
//!
//! The verifier (`IW4L_DRAW_REUSE_VERIFY=1`, and always in a debug build)
//! prepares each lane twice from the same executor state: once as above,
//! then every row from scratch into the same arena list. It compares, row by
//! row, every record and the bytes of its constant spans as the GPU would
//! read them, draws the fresh result, and logs every mismatch.
use std::sync::Arc;

use render_backend::DrawRunKeys;
use render_frame::{MaterialExecFrame, RetainedDrawItem, RetainedDrawKind};
use render_material::{MaterialGenerationId, RuntimeCodeSources, TechType};

use super::constants::ArenaSpanRef;
use bevy::platform::collections::HashMap;

use super::{ColourRowPlan, PreparedExactDraw};

pub(super) const DRAW_REUSE_ENV: &str = "IW4L_DRAW_REUSE";
pub(super) const DRAW_REUSE_VERIFY_ENV: &str = "IW4L_DRAW_REUSE_VERIFY";

/// Read once.
pub(super) fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| perf::switch(DRAW_REUSE_ENV))
}

/// Read once. Always on in a debug build.
pub(super) fn verify() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| cfg!(debug_assertions) || perf::switch(DRAW_REUSE_VERIFY_ENV))
}

/// Everything about a frame that, when it changes, makes every kept row stale.
///
/// Not the draw list itself: effects change it every frame. A row is found
/// again by what it draws ([`RowId`]) and checked field by field. Nor the
/// world index layout: a dynamic light rebuilds it, and only world rows read
/// it (their plan entry carries its epoch and their index range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReuseFrameKey {
    pub(super) scene_epoch: super::TableEpoch,
    pub(super) binding_clears: u64,
    pub(super) material_generation: MaterialGenerationId,
    pub(super) catalog: usize,
    pub(super) prepared_table: usize,
    pub(super) pipeline_generation: MaterialGenerationId,
    pub(super) pipeline_ports: usize,
    pub(super) registry_world: frame::WorldGeneration,
    pub(super) geometry_generation: MaterialGenerationId,
    pub(super) world_generation: frame::WorldGeneration,
    pub(super) smodel_index_count: usize,
    pub(super) world_ready: bool,
    pub(super) smodel_ready: bool,
    pub(super) world_vertex_refused: bool,
    pub(super) smodel_vertex_refused: bool,
    pub(super) sun_shadow_view_ready: bool,
    pub(super) target: bevy::render::render_resource::TextureFormat,
    pub(super) samples: u32,
}

impl ReuseFrameKey {
    /// The fields that differ from `other`, for the log.
    fn changes(&self, other: &Self) -> impl Iterator<Item = &'static str> {
        let fields: [(&'static str, bool); 18] = [
            ("scene_epoch", self.scene_epoch != other.scene_epoch),
            (
                "binding_clears",
                self.binding_clears != other.binding_clears,
            ),
            (
                "material_generation",
                self.material_generation != other.material_generation,
            ),
            ("catalog", self.catalog != other.catalog),
            (
                "prepared_table",
                self.prepared_table != other.prepared_table,
            ),
            (
                "pipeline_generation",
                self.pipeline_generation != other.pipeline_generation,
            ),
            (
                "pipeline_ports",
                self.pipeline_ports != other.pipeline_ports,
            ),
            (
                "registry_world",
                self.registry_world != other.registry_world,
            ),
            (
                "geometry_generation",
                self.geometry_generation != other.geometry_generation,
            ),
            (
                "world_generation",
                self.world_generation != other.world_generation,
            ),
            (
                "smodel_index_count",
                self.smodel_index_count != other.smodel_index_count,
            ),
            ("world_ready", self.world_ready != other.world_ready),
            ("smodel_ready", self.smodel_ready != other.smodel_ready),
            (
                "world_vertex_refused",
                self.world_vertex_refused != other.world_vertex_refused,
            ),
            (
                "smodel_vertex_refused",
                self.smodel_vertex_refused != other.smodel_vertex_refused,
            ),
            (
                "sun_shadow_view_ready",
                self.sun_shadow_view_ready != other.sun_shadow_view_ready,
            ),
            ("target", self.target != other.target),
            ("samples", self.samples != other.samples),
        ];
        fields
            .into_iter()
            .filter_map(|(name, changed)| changed.then_some(name))
    }
}

/// Which material-frame input differs, for the log.
fn first_exec_change(now: &MaterialExecFrame, previous: &MaterialExecFrame) -> &'static str {
    if now.view_origin.to_array().map(f32::to_bits)
        != previous.view_origin.to_array().map(f32::to_bits)
    {
        "view_origin"
    } else if mat_bits(now.clip_from_world) != mat_bits(previous.clip_from_world)
        || mat_bits(now.view_from_world) != mat_bits(previous.view_from_world)
        || mat_bits(now.clip_from_view) != mat_bits(previous.clip_from_view)
        || mat_bits(now.viewmodel_clip_from_world) != mat_bits(previous.viewmodel_clip_from_world)
    {
        "camera"
    } else if now.reflection_probe_sh != previous.reflection_probe_sh
        || !(Arc::ptr_eq(&now.smodel_lighting_sh, &previous.smodel_lighting_sh)
            || now.smodel_lighting_sh == previous.smodel_lighting_sh)
    {
        "sh"
    } else if !now.code_sources.same_textures(&previous.code_sources) {
        "code textures"
    } else {
        "other"
    }
}

fn mat_bits(m: Option<bevy::math::Mat4>) -> Option<[u32; 16]> {
    m.map(|m| m.to_cols_array().map(f32::to_bits))
}

/// The material frame's inputs to a draw's constants other than the code
/// constants and the scene lights ([`ChangedLights`]), compared with
/// `previous` bit for bit where they are floats the overlay computes from.
pub(super) fn same_exec_inputs(now: &MaterialExecFrame, previous: &MaterialExecFrame) -> bool {
    now.view_origin.to_array().map(f32::to_bits)
        == previous.view_origin.to_array().map(f32::to_bits)
        && mat_bits(now.clip_from_world) == mat_bits(previous.clip_from_world)
        && mat_bits(now.view_from_world) == mat_bits(previous.view_from_world)
        && mat_bits(now.clip_from_view) == mat_bits(previous.clip_from_view)
        && mat_bits(now.viewmodel_clip_from_world) == mat_bits(previous.viewmodel_clip_from_world)
        && now.viewmodel_near.map(f32::to_bits) == previous.viewmodel_near.map(f32::to_bits)
        && now.inv_image_height.map(f32::to_bits) == previous.inv_image_height.map(f32::to_bits)
        && now.outdoor.map(|o| (o.image, o.lookup)) == previous.outdoor.map(|o| (o.image, o.lookup))
        && now.reflection_probe_sh == previous.reflection_probe_sh
        && (Arc::ptr_eq(&now.smodel_lighting_sh, &previous.smodel_lighting_sh)
            || now.smodel_lighting_sh == previous.smodel_lighting_sh)
        && now.code_sources.same_textures(&previous.code_sources)
}

/// Scene lights whose inputs changed since the last frame. A draw reads only
/// the light its key names (`scene_light_index`): its pack, attenuation,
/// compiled bindings and spot-shadow receiver. Dynamic lights come and go
/// every frame without touching the draws of the others.
#[derive(Default)]
pub(super) struct ChangedLights {
    bits: [u64; 4],
}

impl ChangedLights {
    fn fill(&mut self, now: &MaterialExecFrame, previous: &MaterialExecFrame) {
        fn mark<T: PartialEq>(bits: &mut [u64; 4], now: &[T], previous: &[T]) {
            for index in 0..now.len().max(previous.len()).min(256) {
                if now.get(index) != previous.get(index) {
                    bits[index / 64] |= 1 << (index % 64);
                }
            }
            if now.len().max(previous.len()) > 256 {
                *bits = [u64::MAX; 4];
            }
        }
        self.bits = [0; 4];
        mark(
            &mut self.bits,
            &now.primary_lights,
            &previous.primary_lights,
        );
        mark(&mut self.bits, &now.attenuation, &previous.attenuation);
        mark(
            &mut self.bits,
            &now.local_light_bindings,
            &previous.local_light_bindings,
        );
        mark(
            &mut self.bits,
            &now.spot_receivers,
            &previous.spot_receivers,
        );
    }

    pub(super) fn contains(&self, light: u8) -> bool {
        self.bits[usize::from(light) / 64] & (1 << (light % 64)) != 0
    }

    fn count(&self) -> u32 {
        self.bits.iter().map(|word| word.count_ones()).sum()
    }
}

/// The static model cache's index layout: what a cached or pretessellated
/// static model's range is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SmcLayoutKey {
    pub(super) revision: u64,
    pub(super) pretess_indices: usize,
    pub(super) baked: usize,
}

/// Code-constant indices that changed since the last frame, as bits.
#[derive(Default)]
pub(super) struct ChangedConstants {
    bits: Vec<u64>,
    list: Vec<u16>,
}

impl ChangedConstants {
    fn fill(&mut self, now: &RuntimeCodeSources, previous: &RuntimeCodeSources) {
        now.changed_constants(previous, &mut self.list);
        for word in &mut self.bits {
            *word = 0;
        }
        for &index in &self.list {
            let word = usize::from(index) / 64;
            if self.bits.len() <= word {
                self.bits.resize(word + 1, 0);
            }
            self.bits[word] |= 1 << (index % 64);
        }
    }

    pub(super) fn any(&self, reads: &[u16]) -> bool {
        reads.iter().any(|&index| {
            self.bits
                .get(usize::from(index) / 64)
                .is_some_and(|word| word & (1 << (index % 64)) != 0)
        })
    }

    pub(super) fn indices(&self) -> &[u16] {
        &self.list
    }
}

/// What the lanes may do this frame.
#[derive(Default)]
pub(super) struct ReuseFrame {
    pub(super) on: bool,
    /// Rows kept from the last frame may be used: the frame key and the
    /// material frame's other inputs are unchanged.
    pub(super) valid: bool,
    /// Rows prepared this frame are worth keeping: the camera and the other
    /// material-frame inputs are those of the last frame. While the view
    /// moves no row can be kept, so none is stored; when it stops, rows are
    /// stored on its first still frame and kept from the next.
    pub(super) store: bool,
    pub(super) verify: bool,
    pub(super) changed: ChangedConstants,
    pub(super) lights: ChangedLights,
    /// The static model cache layout changed since the last frame.
    pub(super) smc_changed: bool,
}

/// Kept across frames by the camera prepare.
#[derive(Default)]
pub(super) struct ReuseState {
    last_key: Option<ReuseFrameKey>,
    last_smc: Option<SmcLayoutKey>,
    last_exec: Option<MaterialExecFrame>,
    pub(super) frame: ReuseFrame,
    pub(super) stats: ReuseStats,
}

impl ReuseState {
    /// Decide what this frame may keep, and remember it for the next.
    pub(super) fn open(
        &mut self,
        key: Option<ReuseFrameKey>,
        smc: SmcLayoutKey,
        exec: &MaterialExecFrame,
    ) {
        self.open_as(enabled(), verify(), key, smc, exec);
    }

    fn open_as(
        &mut self,
        on: bool,
        verify: bool,
        key: Option<ReuseFrameKey>,
        smc: SmcLayoutKey,
        exec: &MaterialExecFrame,
    ) {
        self.frame.on = on;
        self.frame.verify = on && verify;
        if !self.frame.on {
            self.frame.valid = false;
            self.frame.store = false;
            return;
        }
        if self.stats.frames == 0 {
            diag::info!(
                World,
                "draw reuse: on ({DRAW_REUSE_ENV}=1), verify={}",
                self.frame.verify
            );
        }
        let same_key = key.is_some() && key == self.last_key;
        let same_exec = self
            .last_exec
            .as_ref()
            .is_some_and(|last| same_exec_inputs(exec, last));
        match self.last_exec.as_ref() {
            Some(last) => {
                self.frame
                    .changed
                    .fill(&exec.code_sources, &last.code_sources);
                self.frame.lights.fill(exec, last);
            }
            None => {
                self.frame
                    .changed
                    .fill(&exec.code_sources, &exec.code_sources);
                self.frame.lights.fill(exec, exec);
            }
        }
        self.stats.lights_changed += u64::from(self.frame.lights.count());
        self.frame.valid = same_key && same_exec;
        self.frame.store = same_exec;
        self.frame.smc_changed = self.last_smc != Some(smc);
        self.last_smc = Some(smc);
        self.stats.frames += 1;
        if self.frame.smc_changed {
            self.stats.smc_changes += 1;
        }
        if !self.frame.valid {
            self.stats.invalid_frames += 1;
            if !same_key {
                self.stats.key_changes += 1;
                match (key.as_ref(), self.last_key.as_ref()) {
                    (Some(now), Some(last)) => {
                        for name in now.changes(last) {
                            self.stats.note_change(name);
                        }
                    }
                    _ => self.stats.note_change("first"),
                }
            } else {
                self.stats.exec_changes += 1;
                let name = self
                    .last_exec
                    .as_ref()
                    .map_or("first", |last| first_exec_change(exec, last));
                self.stats.note_change(name);
            }
        }
        self.last_key = key;
        match self.last_exec.as_mut() {
            Some(last) => last.clone_from(exec),
            None => self.last_exec = Some(exec.clone()),
        }
    }
}

#[derive(Default)]
pub(super) struct ReuseStats {
    pub(super) frames: u64,
    pub(super) invalid_frames: u64,
    pub(super) key_changes: u64,
    pub(super) exec_changes: u64,
    pub(super) rows: u64,
    pub(super) eligible: u64,
    pub(super) reused: u64,
    pub(super) miss_row: u64,
    pub(super) miss_reads: u64,
    pub(super) miss_span: u64,
    pub(super) verified_rows: u64,
    pub(super) kept_verified: u64,
    pub(super) mismatches: u64,
    pub(super) miss_light: u64,
    lights_changed: u64,
    smc_changes: u64,
    changes: Vec<(&'static str, u64)>,
    logged_at: u64,
}

impl ReuseStats {
    fn note_change(&mut self, name: &'static str) {
        match self.changes.iter_mut().find(|(known, _)| *known == name) {
            Some((_, n)) => *n += 1,
            None => self.changes.push((name, 1)),
        }
    }

    pub(super) fn absorb(&mut self, lane: &LaneReuseTally) {
        self.rows += lane.rows;
        self.eligible += lane.stored;
        self.reused += lane.reused;
        self.miss_row += lane.miss_row;
        self.miss_reads += lane.miss_reads;
        self.miss_span += lane.miss_span;
        self.miss_light += lane.miss_light;
        self.verified_rows += lane.verified_rows;
        self.kept_verified += lane.kept_verified;
        self.mismatches += lane.mismatches;
    }

    /// One line every 600 prepared frames, and at once on a mismatch.
    pub(super) fn maybe_log(&mut self, changed: &[u16], force: bool) {
        if !force && self.frames.saturating_sub(self.logged_at) < 600 {
            return;
        }
        self.logged_at = self.frames;
        diag::info!(
            World,
            "draw reuse: frames={} invalid={} (key={} inputs={} why={:?}) rows={} kept={} ({:.1}%) stored={} miss_row={} miss_reads={} miss_light={} miss_span={} lights_changed={} smc_changes={} verify_rows={} verify_kept={} mismatches={} changed_now={:?}",
            self.frames,
            self.invalid_frames,
            self.key_changes,
            self.exec_changes,
            self.changes,
            self.rows,
            self.reused,
            if self.rows == 0 {
                0.0
            } else {
                self.reused as f64 * 100.0 / self.rows as f64
            },
            self.eligible,
            self.miss_row,
            self.miss_reads,
            self.miss_light,
            self.miss_span,
            self.lights_changed,
            self.smc_changes,
            self.verified_rows,
            self.kept_verified,
            self.mismatches,
            changed,
        );
    }
}

/// One lane's counts for one frame.
#[derive(Default, Clone, Copy)]
pub(super) struct LaneReuseTally {
    pub(super) rows: u64,
    pub(super) stored: u64,
    pub(super) reused: u64,
    pub(super) miss_row: u64,
    pub(super) miss_reads: u64,
    pub(super) miss_span: u64,
    pub(super) miss_light: u64,
    pub(super) verified_rows: u64,
    pub(super) kept_verified: u64,
    pub(super) mismatches: u64,
}

/// A world surface or a rigid, cached or pretessellated static model.
pub(super) fn eligible_kind(kind: &RetainedDrawKind) -> bool {
    match *kind {
        RetainedDrawKind::World { .. } => true,
        RetainedDrawKind::Smodel { stream, .. } => matches!(
            stream,
            Some(
                lighting_iw4::SmodelSurfPath::Rigid
                    | lighting_iw4::SmodelSurfPath::Cached
                    | lighting_iw4::SmodelSurfPath::Pretess
            )
        ),
        _ => false,
    }
}

/// A cached or pretessellated static model: its range is in the static model
/// cache's index layout.
fn reads_smc_layout(kind: &RetainedDrawKind) -> bool {
    matches!(
        kind,
        RetainedDrawKind::Smodel {
            stream: Some(
                lighting_iw4::SmodelSurfPath::Cached | lighting_iw4::SmodelSurfPath::Pretess
            ),
            ..
        }
    )
}

fn mat4_bits(m: &bevy::math::Mat4) -> [u32; 16] {
    m.to_cols_array().map(f32::to_bits)
}

/// The same draw item, bit for bit, for the kinds [`eligible_kind`] admits.
pub(super) fn same_item(a: &RetainedDrawItem, b: &RetainedDrawItem) -> bool {
    if a.material_id != b.material_id
        || a.key != b.key
        || a.material_rank != b.material_rank
        || a.surface_samplers != b.surface_samplers
        || a.camera_region != b.camera_region
    {
        return false;
    }
    match (&a.kind, &b.kind) {
        (
            RetainedDrawKind::World {
                surf: s0,
                run: r0,
                run_off: o0,
                bsp_kind: k0,
                bsp_run_first: f0,
                setup_key_changed: c0,
                world_from_local: m0,
            },
            RetainedDrawKind::World {
                surf: s1,
                run: r1,
                run_off: o1,
                bsp_kind: k1,
                bsp_run_first: f1,
                setup_key_changed: c1,
                world_from_local: m1,
            },
        ) => {
            s0 == s1
                && r0 == r1
                && o0 == o1
                && k0 == k1
                && f0 == f1
                && c0 == c1
                && mat4_bits(m0) == mat4_bits(m1)
        }
        (
            RetainedDrawKind::Smodel {
                surface: s0,
                material: t0,
                world_from_local: m0,
                lighting_handle: h0,
                packed_lighting: p0,
                placement: l0,
                stream: st0,
                cache_index: c0,
                pretess: r0,
            },
            RetainedDrawKind::Smodel {
                surface: s1,
                material: t1,
                world_from_local: m1,
                lighting_handle: h1,
                packed_lighting: p1,
                placement: l1,
                stream: st1,
                cache_index: c1,
                pretess: r1,
            },
        ) => {
            s0 == s1
                && t0 == t1
                && mat4_bits(m0) == mat4_bits(m1)
                && h0 == h1
                && p0 == p1
                && l0 == l1
                && st0 == st1
                && c0 == c1
                && r0 == r1
        }
        _ => false,
    }
}

/// What a row draws, to find it again in the next frame's list: the effects
/// before it come and go, so neither its place in the list nor its draw index
/// is stable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct RowId {
    key: u64,
    first: u32,
    second: u32,
    world_surf: Option<u16>,
    technique: u8,
    tech: u8,
}

impl RowId {
    /// `None` for a row that is never kept.
    pub(super) fn of(
        planned: &ColourRowPlan,
        live: &RetainedDrawItem,
        tech: TechType,
    ) -> Option<Self> {
        let (first, second) = match live.kind {
            RetainedDrawKind::World {
                surf, run, run_off, ..
            } => (u32::from(surf) | u32::from(run) << 16, run_off),
            RetainedDrawKind::Smodel {
                placement, surface, ..
            } => (placement, surface),
            _ => return None,
        };
        eligible_kind(&live.kind).then_some(Self {
            key: live.key,
            first,
            second,
            world_surf: planned.world_surface_override,
            technique: planned.technique.0,
            tech: tech.0,
        })
    }
}

/// A row as it was prepared: what it was made from and where its records,
/// arena spans and code-constant reads are in the lane's pools.
#[derive(Clone, Copy)]
struct CachedRow {
    world_surface_override: Option<u16>,
    technique: TechType,
    index_span: Option<(u32, u32)>,
    layout_epoch: u64,
    item: RetainedDrawItem,
    tech: TechType,
    run_keys: DrawRunKeys,
    records: (u32, u32),
    spans: (u32, u32),
    reads: (u32, u32),
}

impl CachedRow {
    /// The plan entry as far as the row's records read it: a world row's
    /// range is in the world index layout of that epoch; a static model's is
    /// its own.
    fn same_plan(&self, planned: &ColourRowPlan) -> bool {
        self.world_surface_override == planned.world_surface_override
            && self.technique == planned.technique
            && self.index_span == planned.index_span
            && (!matches!(self.item.kind, RetainedDrawKind::World { .. })
                || self.layout_epoch == planned.layout_epoch)
    }
}

#[derive(Default)]
struct RowPools {
    rows: HashMap<RowId, CachedRow>,
    records: Vec<PreparedExactDraw>,
    spans: Vec<ArenaSpanRef>,
    reads: Vec<u16>,
}

impl RowPools {
    fn clear(&mut self) {
        self.rows.clear();
        self.records.clear();
        self.spans.clear();
        self.reads.clear();
    }
}

fn range(r: (u32, u32)) -> std::ops::Range<usize> {
    r.0 as usize..(r.0 + r.1) as usize
}

fn span_of(start: usize, end: usize) -> (u32, u32) {
    (
        u32::try_from(start).expect("reuse pool fits u32"),
        u32::try_from(end - start).expect("reuse pool fits u32"),
    )
}

/// A row from the last frame that may be used as it is.
pub(super) struct KeptRow<'a> {
    pub(super) records: &'a [PreparedExactDraw],
    pub(super) spans: &'a [ArenaSpanRef],
    pub(super) run_keys: DrawRunKeys,
}

pub(super) enum Lookup<'a> {
    Kept(KeptRow<'a>),
    Changed,
    Reads,
    Light,
    None,
}

/// One colour lane's rows from the last frame (`last`) and this one (`next`).
#[derive(Default)]
pub(super) struct LaneReuse {
    last: RowPools,
    next: RowPools,
    pub(super) tally: LaneReuseTally,
}

impl LaneReuse {
    pub(super) fn begin(&mut self, rows: usize) {
        self.next.clear();
        self.tally = LaneReuseTally {
            rows: rows as u64,
            ..LaneReuseTally::default()
        };
    }

    /// Swap this frame's rows in for the next frame.
    pub(super) fn finish(&mut self) {
        std::mem::swap(&mut self.last, &mut self.next);
        self.next.clear();
    }

    pub(super) fn lookup(
        &self,
        id: RowId,
        planned: &ColourRowPlan,
        live: &RetainedDrawItem,
        tech: TechType,
        frame: &ReuseFrame,
    ) -> Lookup<'_> {
        let Some(row) = self.last.rows.get(&id) else {
            return Lookup::None;
        };
        if !row.same_plan(planned) || row.tech != tech || !same_item(&row.item, live) {
            return Lookup::Changed;
        }
        if frame.smc_changed && reads_smc_layout(&live.kind) {
            return Lookup::Changed;
        }
        if frame
            .lights
            .contains(dpvs_iw4::GfxDrawSurf { packed: live.key }.scene_light_index())
        {
            return Lookup::Light;
        }
        let changed = &frame.changed;
        if changed.any(&self.last.reads[range(row.reads)]) {
            return Lookup::Reads;
        }
        Lookup::Kept(KeptRow {
            records: &self.last.records[range(row.records)],
            spans: &self.last.spans[range(row.spans)],
            run_keys: row.run_keys,
        })
    }

    /// Carry a kept row into this frame.
    pub(super) fn keep(&mut self, id: RowId) {
        let Some(row) = self.last.rows.get(&id).copied() else {
            return;
        };
        let records = self.next.records.len();
        self.next
            .records
            .extend_from_slice(&self.last.records[range(row.records)]);
        let spans = self.next.spans.len();
        self.next
            .spans
            .extend_from_slice(&self.last.spans[range(row.spans)]);
        let reads = self.next.reads.len();
        self.next
            .reads
            .extend_from_slice(&self.last.reads[range(row.reads)]);
        self.next.rows.insert(
            id,
            CachedRow {
                records: span_of(records, self.next.records.len()),
                spans: span_of(spans, self.next.spans.len()),
                reads: span_of(reads, self.next.reads.len()),
                ..row
            },
        );
        self.tally.reused += 1;
        self.tally.stored += 1;
    }

    /// Remember a row prepared this frame.
    pub(super) fn store(
        &mut self,
        id: RowId,
        run_keys: DrawRunKeys,
        planned: &ColourRowPlan,
        live: &RetainedDrawItem,
        tech: TechType,
        records: &[PreparedExactDraw],
        spans: &[ArenaSpanRef],
        reads: impl Iterator<Item = u16>,
    ) {
        if records.len() != spans.len() {
            return;
        }
        let r0 = self.next.records.len();
        self.next.records.extend_from_slice(records);
        let s0 = self.next.spans.len();
        self.next.spans.extend_from_slice(spans);
        let d0 = self.next.reads.len();
        self.next.reads.extend(reads);
        self.next.rows.insert(
            id,
            CachedRow {
                world_surface_override: planned.world_surface_override,
                technique: planned.technique,
                index_span: planned.index_span,
                layout_epoch: planned.layout_epoch,
                item: *live,
                tech,
                run_keys,
                records: span_of(r0, self.next.records.len()),
                spans: span_of(s0, self.next.spans.len()),
                reads: span_of(d0, self.next.reads.len()),
            },
        );
        self.tally.stored += 1;
    }

    /// A row the verifier found wrong is not kept for the next frame.
    pub(super) fn drop_next(&mut self, id: RowId) {
        if self.next.rows.remove(&id).is_some() {
            self.tally.stored = self.tally.stored.saturating_sub(1);
        }
    }
}

/// One row as a verifier pass prepared it.
#[derive(Clone, Copy)]
pub(super) struct RowTrace {
    pub(super) offset: u32,
    pub(super) kind: &'static str,
    pub(super) id: Option<RowId>,
    pub(super) eligible: bool,
    pub(super) kept: bool,
    pub(super) records: (u32, u32),
    pub(super) spans: (u32, u32),
}

impl RowTrace {
    pub(super) fn new(
        offset: usize,
        kind: &'static str,
        id: Option<RowId>,
        eligible: bool,
        kept: bool,
        records: (usize, usize),
        spans: (usize, usize),
    ) -> Self {
        Self {
            offset: offset as u32,
            kind,
            id,
            eligible,
            kept,
            records: span_of(records.0, records.1),
            spans: span_of(spans.0, spans.1),
        }
    }

    pub(super) fn records(&self) -> std::ops::Range<usize> {
        range(self.records)
    }

    pub(super) fn spans(&self) -> std::ops::Range<usize> {
        range(self.spans)
    }
}

/// The fields of two records the draw reads, apart from where its constants
/// are: those are compared by content. `with_range` compares the index range.
pub(super) fn record_mismatch(
    a: &PreparedExactDraw,
    b: &PreparedExactDraw,
    with_range: bool,
) -> Option<&'static str> {
    if a.pipeline != b.pipeline {
        return Some("pipeline");
    }
    if a.port != b.port {
        return Some("port");
    }
    if a.constants.is_some() || b.constants.is_some() {
        return Some("unpacked constants");
    }
    if a.constant_base.is_some() != b.constant_base.is_some() {
        return Some("constant base");
    }
    if a.texture_slots[..] != b.texture_slots[..] {
        return Some("texture slots");
    }
    if with_range && (a.start, a.count) != (b.start, b.count) {
        return Some("index range");
    }
    if a.tess != b.tess {
        return Some("tess");
    }
    if a.after_scene_resolve != b.after_scene_resolve {
        return Some("after scene resolve");
    }
    if a.owner_object_id != b.owner_object_id {
        return Some("owner");
    }
    if a.depth_min.to_bits() != b.depth_min.to_bits()
        || a.depth_max.to_bits() != b.depth_max.to_bits()
    {
        return Some("depth range");
    }
    if a.ring_epoch != b.ring_epoch || a.smc_stream_off != b.smc_stream_off {
        return Some("stream");
    }
    if a.state != b.state {
        return Some("state");
    }
    if (
        a.bsp_kind,
        a.bsp_run_first,
        a.bsp_first_surf,
        a.bsp_surf_count,
        a.bsp_counted,
    ) != (
        b.bsp_kind,
        b.bsp_run_first,
        b.bsp_first_surf,
        b.bsp_surf_count,
        b.bsp_counted,
    ) {
        return Some("bsp");
    }
    if (a.binds_sun_shadow, a.binds_spot_shadow) != (b.binds_sun_shadow, b.binds_spot_shadow) {
        return Some("shadow binds");
    }
    if a.indirect_arg != b.indirect_arg {
        return Some("indirect");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::Mat4;

    fn smodel(world_from_local: Mat4, lighting_handle: u32) -> RetainedDrawItem {
        RetainedDrawItem {
            material_id: None,
            key: 0x1234,
            material_rank: 7,
            kind: RetainedDrawKind::Smodel {
                surface: 3,
                material: 4,
                world_from_local,
                lighting_handle,
                packed_lighting: Some([1, 2, 3, 4]),
                placement: 9,
                stream: Some(lighting_iw4::SmodelSurfPath::Cached),
                cache_index: Some(2),
                pretess: None,
            },
            surface_samplers: Default::default(),
            camera_region: None,
        }
    }

    fn keys(live: &RetainedDrawItem) -> DrawRunKeys {
        render_backend::MaterialRunExecutor::run_keys(
            &MaterialExecFrame::default(),
            live,
            TechType(3),
            0,
        )
    }

    fn plan(technique: u8) -> ColourRowPlan {
        ColourRowPlan {
            draw_index: 5,
            world_surface_override: None,
            technique: TechType(technique),
            index_span: Some((10, 30)),
            layout_epoch: 2,
        }
    }

    fn kept(
        lane: &LaneReuse,
        id: RowId,
        planned: &ColourRowPlan,
        live: &RetainedDrawItem,
        frame: &ReuseFrame,
    ) -> &'static str {
        match lane.lookup(id, planned, live, TechType(3), frame) {
            Lookup::Kept(_) => "kept",
            Lookup::Changed => "changed",
            Lookup::Reads => "reads",
            Lookup::Light => "light",
            Lookup::None => "none",
        }
    }

    #[test]
    fn a_stored_row_is_found_by_what_it_draws_until_an_input_changes() {
        let planned = plan(1);
        let live = smodel(Mat4::IDENTITY, 5);
        let id = RowId::of(&planned, &live, TechType(3)).expect("a static model is kept");
        let mut lane = LaneReuse::default();
        lane.begin(1);
        lane.store(
            id,
            keys(&live),
            &planned,
            &live,
            TechType(3),
            &[],
            &[],
            [12u16, 40].into_iter(),
        );
        assert_eq!(lane.tally.stored, 1);
        lane.finish();
        let mut frame = ReuseFrame::default();
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "kept");
        // Its draw index moved (effects came before it): still the same row.
        let moved = ColourRowPlan {
            draw_index: 9,
            ..plan(1)
        };
        assert_eq!(kept(&lane, id, &moved, &live, &frame), "kept");
        let respan = ColourRowPlan {
            index_span: Some((10, 33)),
            ..plan(1)
        };
        assert_eq!(kept(&lane, id, &respan, &live, &frame), "changed");
        // A new world index layout: a static model does not read it.
        let relaid = ColourRowPlan {
            layout_epoch: 3,
            ..plan(1)
        };
        assert_eq!(kept(&lane, id, &relaid, &live, &frame), "kept");
        assert_eq!(
            kept(&lane, id, &planned, &smodel(Mat4::IDENTITY, 6), &frame),
            "changed"
        );
        let mut before = RuntimeCodeSources::default();
        before.set_constant_rows(40, &[[1; 4]]);
        let mut now = before.clone();
        now.set_constant_rows(40, &[[2; 4]]);
        frame.changed.fill(&now, &before);
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "reads");
        frame.changed.fill(&now, &now);
        let light = dpvs_iw4::GfxDrawSurf { packed: live.key }.scene_light_index();
        frame.lights.bits[usize::from(light) / 64] |= 1 << (light % 64);
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "light");
        frame.lights = ChangedLights::default();
        frame.smc_changed = true;
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "changed");
        let rigid = {
            let mut rigid = live;
            if let RetainedDrawKind::Smodel { stream, .. } = &mut rigid.kind {
                *stream = Some(lighting_iw4::SmodelSurfPath::Rigid);
            }
            rigid
        };
        assert!(!reads_smc_layout(&rigid.kind) && reads_smc_layout(&live.kind));
        frame.smc_changed = false;
        // A row not stored last frame is not found.
        lane.begin(1);
        lane.finish();
        frame.lights = ChangedLights::default();
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "none");
    }

    #[test]
    fn a_world_row_is_tied_to_its_index_layout() {
        let planned = plan(1);
        let live = RetainedDrawItem {
            kind: RetainedDrawKind::world(4),
            ..smodel(Mat4::IDENTITY, 5)
        };
        let id = RowId::of(&planned, &live, TechType(3)).unwrap();
        let mut lane = LaneReuse::default();
        lane.begin(1);
        lane.store(
            id,
            keys(&live),
            &planned,
            &live,
            TechType(3),
            &[],
            &[],
            std::iter::empty(),
        );
        lane.finish();
        let frame = ReuseFrame::default();
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "kept");
        let relaid = ColourRowPlan {
            layout_epoch: 3,
            ..plan(1)
        };
        assert_eq!(kept(&lane, id, &relaid, &live, &frame), "changed");
    }

    #[test]
    fn keep_carries_a_row_and_drop_next_forgets_it() {
        let planned = plan(1);
        let live = smodel(Mat4::IDENTITY, 5);
        let id = RowId::of(&planned, &live, TechType(3)).unwrap();
        let frame = ReuseFrame::default();
        let mut lane = LaneReuse::default();
        lane.begin(1);
        lane.store(
            id,
            keys(&live),
            &planned,
            &live,
            TechType(3),
            &[],
            &[],
            std::iter::once(7u16),
        );
        lane.finish();
        lane.begin(1);
        lane.keep(id);
        assert_eq!((lane.tally.reused, lane.tally.stored), (1, 1));
        lane.finish();
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "kept");
        lane.begin(1);
        lane.keep(id);
        lane.drop_next(id);
        assert_eq!(lane.tally.stored, 0);
        lane.finish();
        assert_eq!(kept(&lane, id, &planned, &live, &frame), "none");
    }

    #[test]
    fn rows_are_told_apart_by_technique_and_world_surface() {
        let live = smodel(Mat4::IDENTITY, 5);
        let a = RowId::of(&plan(1), &live, TechType(3));
        assert_ne!(a, RowId::of(&plan(2), &live, TechType(3)));
        assert_ne!(a, RowId::of(&plan(1), &live, TechType(4)));
        let world = RetainedDrawItem {
            kind: RetainedDrawKind::world(4),
            ..live
        };
        let one = ColourRowPlan {
            world_surface_override: Some(4),
            ..plan(1)
        };
        let two = ColourRowPlan {
            world_surface_override: Some(5),
            ..plan(1)
        };
        assert_ne!(
            RowId::of(&one, &world, TechType(3)),
            RowId::of(&two, &world, TechType(3))
        );
        let glass = RetainedDrawItem {
            kind: RetainedDrawKind::Glass {
                draw: 0,
                material: 0,
                lighting_handle: 0,
            },
            ..live
        };
        assert_eq!(RowId::of(&plan(1), &glass, TechType(3)), None);
    }

    #[test]
    fn same_item_is_bitwise_on_pose_and_lighting() {
        let a = smodel(Mat4::IDENTITY, 5);
        assert!(same_item(&a, &a));
        assert!(!same_item(&a, &smodel(Mat4::IDENTITY, 6)));
        let mut negative_zero = Mat4::IDENTITY;
        negative_zero.w_axis.x = -0.0;
        assert!(!same_item(&a, &smodel(negative_zero, 5)));
        let mut other_key = a;
        other_key.key += 1;
        assert!(!same_item(&a, &other_key));
    }

    #[test]
    fn only_world_and_unskinned_static_models_are_kept() {
        assert!(eligible_kind(&smodel(Mat4::IDENTITY, 0).kind));
        assert!(eligible_kind(&RetainedDrawKind::world(4)));
        let mut skinned = smodel(Mat4::IDENTITY, 0);
        if let RetainedDrawKind::Smodel { stream, .. } = &mut skinned.kind {
            *stream = Some(lighting_iw4::SmodelSurfPath::Skinned);
        }
        assert!(!eligible_kind(&skinned.kind));
        let glass = RetainedDrawKind::Glass {
            draw: 0,
            material: 0,
            lighting_handle: 0,
        };
        assert!(!eligible_kind(&glass));
    }

    #[test]
    fn changed_constants_hit_only_the_rows_that_read_them() {
        let mut before = RuntimeCodeSources::default();
        before.set_constant_rows(10, &[[1; 4]]);
        before.set_constant_rows(200, &[[1; 4]]);
        let mut now = before.clone();
        now.set_constant_rows(200, &[[2; 4]]);
        let mut changed = ChangedConstants::default();
        changed.fill(&now, &before);
        assert_eq!(changed.indices(), &[200]);
        assert!(changed.any(&[3, 200]));
        assert!(!changed.any(&[10, 64, 199, 201, 700]));
        changed.fill(&now, &now);
        assert!(!changed.any(&[200]));
    }

    #[test]
    fn a_changed_light_is_named_by_its_index() {
        let mut before = MaterialExecFrame::default();
        before.attenuation = vec![Default::default(); 3];
        let mut now = before.clone();
        now.attenuation.push(Default::default());
        now.attenuation[1].image = Some(4);
        let mut lights = ChangedLights::default();
        lights.fill(&now, &before);
        assert!(!lights.contains(0));
        assert!(lights.contains(1));
        assert!(!lights.contains(2));
        assert!(lights.contains(3));
        assert!(!lights.contains(200));
        lights.fill(&now, &now);
        assert_eq!(lights.count(), 0);
    }

    #[test]
    fn rows_are_kept_from_the_second_still_frame_and_stored_only_while_still() {
        let key = ReuseFrameKey {
            scene_epoch: Default::default(),
            binding_clears: 0,
            material_generation: Default::default(),
            catalog: 1,
            prepared_table: 2,
            pipeline_generation: Default::default(),
            pipeline_ports: 3,
            registry_world: Default::default(),
            geometry_generation: Default::default(),
            world_generation: Default::default(),
            smodel_index_count: 4,
            world_ready: true,
            smodel_ready: true,
            world_vertex_refused: false,
            smodel_vertex_refused: false,
            sun_shadow_view_ready: true,
            target: bevy::render::render_resource::TextureFormat::Bgra8UnormSrgb,
            samples: 1,
        };
        let smc = SmcLayoutKey {
            revision: 1,
            pretess_indices: 0,
            baked: 0,
        };
        let still = MaterialExecFrame::default();
        let mut moved = still.clone();
        moved.view_origin.x = 8.0;
        let mut state = ReuseState::default();
        let mut open = |on, key, exec: &MaterialExecFrame| {
            state.open_as(on, false, key, smc, exec);
            (state.frame.valid, state.frame.store)
        };
        assert_eq!(open(false, Some(key), &still), (false, false), "off");
        assert_eq!(open(true, Some(key), &still), (false, false), "first frame");
        assert_eq!(open(true, Some(key), &still), (true, true), "still");
        assert_eq!(open(true, Some(key), &moved), (false, false), "moving");
        assert_eq!(open(true, Some(key), &moved), (true, true), "stopped");
        let mut rebound = key;
        rebound.binding_clears = 1;
        assert_eq!(
            open(true, Some(rebound), &moved),
            (false, true),
            "a new key keeps nothing but stores this frame's rows"
        );
        assert_eq!(open(true, Some(rebound), &moved), (true, true));
        assert_eq!(open(true, None, &moved), (false, true), "no material tables");
    }

    #[test]
    fn camera_and_code_textures_are_exec_inputs() {
        let a = MaterialExecFrame::default();
        assert!(same_exec_inputs(&a, &a.clone()));
        let mut moved = a.clone();
        moved.view_origin.x = 1.0;
        assert!(!same_exec_inputs(&moved, &a));
        let mut turned = a.clone();
        turned.clip_from_world = Some(Mat4::from_rotation_z(0.01));
        assert!(!same_exec_inputs(&turned, &a));
        let mut textured = a.clone();
        textured.code_sources.set_texture(3, 1).unwrap();
        assert!(!same_exec_inputs(&textured, &a));
        let mut lit = a.clone();
        lit.attenuation.push(Default::default());
        assert!(same_exec_inputs(&lit, &a));
        let mut timed = a.clone();
        timed.float_time = 5.0;
        timed.code_sources.set_constant_rows(1, &[[1; 4]]);
        assert!(same_exec_inputs(&timed, &a));
    }
}
