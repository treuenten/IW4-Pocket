use std::sync::{Arc, Mutex, OnceLock, Weak};

use bevy::mesh::VertexBufferLayout;
use bevy::platform::collections::{HashMap, HashSet};
use bevy::prelude::Resource;
use bevy::render::render_resource::{
    BindGroupLayout, BindGroupLayoutDescriptor, ColorTargetState, DepthStencilState,
    MultisampleState, PipelineCompilationOptions, PipelineLayoutDescriptor, PrimitiveState,
    RawFragmentState, RawRenderPipelineDescriptor, RawVertexBufferLayout, RawVertexState,
    RenderPipeline, ShaderModule, ShaderModuleDescriptor, ShaderSource,
};
use bevy::render::renderer::RenderDevice;
use bevy::tasks::{Task, futures_lite::future};

use super::sm3_wgsl::{PASS_VERTEX_ENTRY, ValidatedPassWgsl};
use render_material::PortId;

const LEAN_SHADERS_ENV: &str = "IW4L_LEAN_SHADERS";

/// Whether this run frees each exact-colour shader module once its batch of
/// pipelines is built (`IW4L_LEAN_SHADERS=1`, Apple only, off by default).
///
/// Each module carries its naga IR: 744 MiB of naga allocations under
/// `build_port` in an Estate match (about 1,560 modules for 2,438 pipelines).
/// Two things kept it for the rest of the run: this registry's module cache,
/// and every wgpu-core render pipeline's keep-alive of its modules. With the
/// switch on, the cache is not filled and the vendored wgpu-core
/// (`vendor/wgpu-core`, `pipeline::lean_shaders`, the same switch) drops the
/// keep-alive, so a module dies with the job that built its pipelines. A plan
/// for the same port found later builds the module again from its WGSL. Off
/// Apple the switch is ignored: the keep-alive stays there, so dropping the
/// cache would only cost rebuilds.
pub(crate) fn lean_shaders() -> bool {
    static LEAN: OnceLock<bool> = OnceLock::new();
    *LEAN.get_or_init(|| cfg!(target_vendor = "apple") && perf::switch(LEAN_SHADERS_ENV))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ModuleKey {
    port: PortId,
    cached_lighting: bool,
}

#[derive(Clone)]
pub(super) enum ExactModuleSource {
    Port(Arc<ValidatedPassWgsl>),
    CachedLighting(Arc<str>),
}

impl ExactModuleSource {
    fn wgsl(&self) -> &str {
        match self {
            Self::Port(module) => &module.source,
            Self::CachedLighting(source) => source,
        }
    }
}

pub(super) struct ExactPipelinePlan {
    pub(super) label: String,
    pub(super) fragment_entry: String,
    pub(super) vertex_buffers: Vec<VertexBufferLayout>,
    pub(super) targets: Vec<Option<ColorTargetState>>,
    pub(super) primitive: PrimitiveState,
    pub(super) depth_stencil: DepthStencilState,
    pub(super) multisample: MultisampleState,
    pub(super) constants_layout: BindGroupLayout,
    pub(super) textures_layout: BindGroupLayout,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(super) struct ExactPipelineSlot(u32);

enum SlotState {
    Building,
    Ready(RenderPipeline),
}

struct PortBuild {
    module: ModuleKey,
    created: Option<Arc<ShaderModule>>,
    /// Lean shaders: the module this job made and let go with its plans.
    released: Option<Weak<ShaderModule>>,
    pipelines: Vec<(ExactPipelineSlot, RenderPipeline)>,
}

#[derive(Resource, Default)]
pub(super) struct ExactPipelineRegistry {
    slots: Vec<SlotState>,
    by_key: HashMap<super::colour_submit::ExactColourPipelineKey, ExactPipelineSlot>,
    modules: HashMap<ModuleKey, Arc<ShaderModule>>,

    queued: HashMap<
        ModuleKey,
        (
            ExactModuleSource,
            Vec<(ExactPipelineSlot, ExactPipelinePlan)>,
        ),
    >,
    jobs: Vec<Task<PortBuild>>,

    /// Lean shaders: modules made this run, how many of them were made
    /// again for a port whose module had already been made and dropped, that
    /// count at the last `lean shaders:` line, the ports made so far, and a
    /// weak handle to each module made (dead ones are pruned at each line).
    lean_created: usize,
    lean_again: usize,
    lean_logged: usize,
    lean_ports: HashSet<ModuleKey>,
    lean_made: Vec<Weak<ShaderModule>>,

    layouts: Mutex<HashMap<BindGroupLayoutDescriptor, BindGroupLayout>>,

    discovered: Mutex<Vec<super::colour_submit::ExactColourPipelineKey>>,
}

impl ExactPipelineRegistry {
    pub(super) fn bind_group_layout(
        &self,
        device: &RenderDevice,
        descriptor: &BindGroupLayoutDescriptor,
    ) -> BindGroupLayout {
        self.layouts
            .lock()
            .expect("exact pipeline layout cache is never poisoned")
            .entry(descriptor.clone())
            .or_insert_with_key(|descriptor| {
                device.create_bind_group_layout(descriptor.label.as_ref(), &descriptor.entries)
            })
            .clone()
    }

    pub(super) fn discover(&self, key: super::colour_submit::ExactColourPipelineKey) {
        self.discovered
            .lock()
            .expect("exact pipeline discovery list is never poisoned")
            .push(key);
    }

    pub(super) fn take_discovered(&mut self) -> Vec<super::colour_submit::ExactColourPipelineKey> {
        std::mem::take(
            &mut *self
                .discovered
                .lock()
                .expect("exact pipeline discovery list is never poisoned"),
        )
    }

    pub(super) fn slot(
        &self,
        key: &super::colour_submit::ExactColourPipelineKey,
    ) -> Option<ExactPipelineSlot> {
        self.by_key.get(key).copied()
    }

    pub(super) fn ready(&self, slot: ExactPipelineSlot) -> Option<&RenderPipeline> {
        match self.slots.get(slot.0 as usize) {
            Some(SlotState::Ready(pipeline)) => Some(pipeline),
            _ => None,
        }
    }

    pub(super) fn is_ready(&self, slot: ExactPipelineSlot) -> bool {
        matches!(self.slots.get(slot.0 as usize), Some(SlotState::Ready(_)))
    }

    pub(super) fn request(
        &mut self,
        key: super::colour_submit::ExactColourPipelineKey,
        source: ExactModuleSource,
        plan: ExactPipelinePlan,
    ) -> ExactPipelineSlot {
        if let Some(slot) = self.by_key.get(&key).copied() {
            return slot;
        }
        let slot = ExactPipelineSlot(
            u32::try_from(self.slots.len()).expect("exact pipeline slot count fits u32"),
        );
        self.slots.push(SlotState::Building);
        self.by_key.insert(key, slot);

        let module = ModuleKey {
            port: key.port,
            cached_lighting: matches!(source, ExactModuleSource::CachedLighting(_)),
        };
        self.queued
            .entry(module)
            .or_insert_with(|| (source, Vec::new()))
            .1
            .push((slot, plan));
        slot
    }

    pub(super) fn flush(&mut self, device: &RenderDevice) {
        if self.queued.is_empty() {
            return;
        }

        let pool = assets::load_pool();
        let lean = lean_shaders();
        for (module, (source, plans)) in self.queued.drain() {
            let device = device.clone();
            let existing = if lean {
                None
            } else {
                self.modules.get(&module).cloned()
            };
            self.jobs.push(
                pool.spawn(
                    async move { build_port(&device, module, source, existing, lean, plans) },
                ),
            );
        }
    }

    pub(super) fn poll(&mut self) {
        let mut index = 0;
        while index < self.jobs.len() {
            let Some(build) = future::block_on(future::poll_once(&mut self.jobs[index])) else {
                index += 1;
                continue;
            };

            drop(self.jobs.swap_remove(index));
            if let Some(created) = build.created {
                self.modules.insert(build.module, created);
            }
            if let Some(released) = build.released {
                self.lean_created += 1;
                if !self.lean_ports.insert(build.module) {
                    self.lean_again += 1;
                }
                self.lean_made.push(released);
            }
            for (slot, pipeline) in build.pipelines {
                self.slots[slot.0 as usize] = SlotState::Ready(pipeline);
            }
        }
        if self.jobs.is_empty() && self.lean_created != self.lean_logged {
            self.lean_made.retain(|module| module.strong_count() > 0);
            self.lean_logged = self.lean_created;
            diag::info!(
                World,
                "lean shaders: modules created {}, alive {} (made again {}, pipelines {})",
                self.lean_created,
                self.lean_made.len() + self.modules.len(),
                self.lean_again,
                self.slots
                    .iter()
                    .filter(|slot| matches!(slot, SlotState::Ready(_)))
                    .count()
            );
        }
    }

    pub(super) fn module_n(&self) -> usize {
        self.modules.len()
    }

    pub(super) fn building_n(&self) -> usize {
        self.jobs.len()
    }
}

fn build_port(
    device: &RenderDevice,
    module: ModuleKey,
    source: ExactModuleSource,
    existing: Option<Arc<ShaderModule>>,
    lean: bool,
    plans: Vec<(ExactPipelineSlot, ExactPipelinePlan)>,
) -> PortBuild {
    let label = format!(
        "iw4_exact_colour/{:016x}/{}{}",
        module.port.vertex_program_hash,
        module.port.vertex_type,
        if module.cached_lighting {
            "/cached"
        } else {
            ""
        }
    );

    let (shader, created) = match existing {
        Some(shader) => (shader, None),
        None => {
            let shader = Arc::new(unsafe {
                device.create_shader_module(ShaderModuleDescriptor {
                    label: Some(&label),
                    source: ShaderSource::Wgsl(std::borrow::Cow::Borrowed(source.wgsl())),
                })
            });
            (shader.clone(), Some(shader))
        }
    };
    let pipelines = plans
        .into_iter()
        .map(|(slot, plan)| (slot, build_pipeline(device, &shader, &plan)))
        .collect();
    drop(shader);
    let (created, released) = keep_or_release(created, lean);
    PortBuild {
        module,
        created,
        released,
        pipelines,
    }
}

/// The module a job made: kept for the registry's cache, or (lean shaders)
/// dropped here, so its naga IR is freed on the pool thread that built its
/// pipelines rather than by the render thread. Only a weak handle comes back,
/// for the `lean shaders:` count.
fn keep_or_release<T>(created: Option<Arc<T>>, lean: bool) -> (Option<Arc<T>>, Option<Weak<T>>) {
    match created {
        Some(created) if lean => (None, Some(Arc::downgrade(&created))),
        created => (created, None),
    }
}

fn build_pipeline(
    device: &RenderDevice,
    shader: &ShaderModule,
    plan: &ExactPipelinePlan,
) -> RenderPipeline {
    let layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some(&plan.label),
        bind_group_layouts: &[Some(&plan.constants_layout), Some(&plan.textures_layout)],
        immediate_size: 0,
    });
    let buffers: Vec<RawVertexBufferLayout> = plan
        .vertex_buffers
        .iter()
        .map(|buffer| RawVertexBufferLayout {
            array_stride: buffer.array_stride,
            attributes: &buffer.attributes,
            step_mode: buffer.step_mode,
        })
        .collect();
    let compilation_options = PipelineCompilationOptions {
        constants: &[],
        zero_initialize_workgroup_memory: false,
    };
    device.create_render_pipeline(&RawRenderPipelineDescriptor {
        label: Some(&plan.label),
        layout: Some(&layout),
        vertex: RawVertexState {
            module: shader,
            entry_point: Some(PASS_VERTEX_ENTRY),
            buffers: &buffers,
            compilation_options: compilation_options.clone(),
        },
        fragment: Some(RawFragmentState {
            module: shader,
            entry_point: Some(&plan.fragment_entry),
            targets: &plan.targets,
            compilation_options,
        }),
        primitive: plan.primitive,
        depth_stencil: Some(plan.depth_stencil.clone()),
        multisample: plan.multisample,
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lean_releases_the_module_and_off_keeps_it() {
        let made = Arc::new(7u32);
        let (kept, released) = keep_or_release(Some(made.clone()), false);
        assert!(released.is_none());
        assert!(Arc::ptr_eq(kept.as_ref().unwrap(), &made));

        let watch = Arc::downgrade(&made);
        drop((kept, made));
        assert_eq!(watch.strong_count(), 0);

        let made = Arc::new(7u32);
        let (kept, released) = keep_or_release(Some(made.clone()), true);
        assert!(kept.is_none());
        let released = released.unwrap();
        assert_eq!(released.strong_count(), 1);
        drop(made);
        assert_eq!(released.strong_count(), 0, "nothing but the job held it");

        assert!(matches!(keep_or_release::<u32>(None, true), (None, None)));
        assert!(matches!(keep_or_release::<u32>(None, false), (None, None)));
    }
}
