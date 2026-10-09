//! GPU work made during play rather than at load, in the log: pipelines added
//! to Bevy's pipeline cache (post-fx, HUD, sun, depth copies) and images handed
//! to the GPU. The exact colour pipelines have their own lines (`exact
//! pipelines: requested on first use` and `first-use build done`).
//!
//! Counts are summed and written at most once a second, and only while the
//! world is not behind the loading overlay, so a match logs a few lines at its
//! first spawn, shot or kill and none once everything has been seen. Used to
//! tell whether a stall on the phone came with new GPU work.
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::render::render_asset::ExtractedAssets;
use bevy::render::render_resource::{PipelineCache, PipelineDescriptor};
use bevy::render::texture::GpuImage;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};

const EVERY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct FirstUseLog {
    /// The world whose loading overlay has gone: only its play is logged.
    playing: Option<frame::WorldGeneration>,
    was_loading: bool,
    pipelines_seen: usize,
    pipelines: usize,
    first_pipeline: Option<String>,
    images: usize,
    image_bytes: usize,
    first_image: Option<(u32, u32)>,
    last_line: Option<Instant>,
}

/// The main world's load state, copied at extraction: the render world's own
/// `GpuSubmitReady` is not kept up to date.
#[derive(Resource, Default, Clone, Copy)]
struct PlayState {
    world_generation: frame::WorldGeneration,
    loading: bool,
}

fn extract_play_state(
    ready: Extract<Option<Res<crate::GpuSubmitReady>>>,
    mut state: ResMut<PlayState>,
) {
    *state = ready
        .as_ref()
        .map_or_else(PlayState::default, |ready| PlayState {
            world_generation: ready.world_generation,
            loading: ready.warm_pipelines || ready.overlay_gpu_wait,
        });
}

fn pipeline_label(descriptor: &PipelineDescriptor) -> Option<String> {
    match descriptor {
        PipelineDescriptor::RenderPipelineDescriptor(d) => d.label.as_deref().map(str::to_owned),
        PipelineDescriptor::ComputePipelineDescriptor(d) => d.label.as_deref().map(str::to_owned),
    }
}

fn log_first_use_gpu_work(
    cache: Res<PipelineCache>,
    images: Option<Res<ExtractedAssets<GpuImage>>>,
    ready: Res<PlayState>,
    mut log: Local<FirstUseLog>,
) {
    let total = cache.pipelines().count();
    let loading = ready.loading;
    if loading || ready.world_generation.0.is_none() {
        log.was_loading |= loading;
        log.pipelines_seen = total;
        return;
    }
    if std::mem::take(&mut log.was_loading) {
        log.playing = Some(ready.world_generation);
    }
    if log.playing != Some(ready.world_generation) {
        log.pipelines_seen = total;
        return;
    }
    if total > log.pipelines_seen {
        if log.first_pipeline.is_none() {
            log.first_pipeline = cache
                .pipelines()
                .skip(log.pipelines_seen)
                .find_map(|pipeline| pipeline_label(&pipeline.descriptor));
        }
        log.pipelines += total - log.pipelines_seen;
        log.pipelines_seen = total;
    }
    if let Some(images) = images {
        if log.first_image.is_none() {
            log.first_image = images.extracted.first().map(|(_, image)| {
                let size = image.texture_descriptor.size;
                (size.width, size.height)
            });
        }
        log.images += images.extracted.len();
        log.image_bytes += images
            .extracted
            .iter()
            .filter_map(|(_, image)| image.data.as_ref().map(Vec::len))
            .sum::<usize>();
    }
    if log.pipelines == 0 && log.images == 0 {
        return;
    }
    let now = Instant::now();
    if log
        .last_line
        .is_some_and(|last| now.duration_since(last) < EVERY)
    {
        return;
    }
    let first_image = log
        .first_image
        .map_or_else(|| "-".to_owned(), |(w, h)| format!("{w}x{h}"));
    diag::info!(
        World,
        "gpu work during play: pipeline cache +{} (first {}) | images to the GPU {} ({:.1} MiB, first {first_image})",
        log.pipelines,
        log.first_pipeline.as_deref().unwrap_or("-"),
        log.images,
        log.image_bytes as f64 / (1024.0 * 1024.0),
    );
    *log = FirstUseLog {
        playing: log.playing,
        pipelines_seen: log.pipelines_seen,
        last_line: Some(now),
        ..default()
    };
}

pub(crate) fn register(app: &mut App) {
    let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
        return;
    };
    render_app
        .init_resource::<PlayState>()
        .add_systems(ExtractSchedule, extract_play_state);
    render_app.add_systems(
        Render,
        log_first_use_gpu_work
            .after(RenderSystems::ExtractCommands)
            .before(RenderSystems::PrepareAssets),
    );
}
