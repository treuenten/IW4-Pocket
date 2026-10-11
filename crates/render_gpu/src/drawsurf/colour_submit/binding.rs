use bevy::platform::collections::HashMap;
use bevy::prelude::Resource;
use render_frame::TextureBindIdentity;
use render_material::MaterialGenerationId;
use std::sync::Arc;

pub(super) type ColourBindingLanes = [HashMap<BoundTextureKey, Arc<[u32]>>; 4];

#[derive(Resource, Default)]
pub(super) struct ExactColourBindingCache {
    pub(super) generation: MaterialGenerationId,
    views_revision: u64,
    pub(super) textures: ColourBindingLanes,
    /// How many times `textures` has been cleared: a lane's copy of it
    /// (`LaneBindMemo`) is good while this has not moved.
    clears: u64,
}

#[derive(Resource, Default)]
pub(super) struct ExactShadowBindingCache {
    pub(super) generation: MaterialGenerationId,
    views_revision: u64,
    pub(super) textures: HashMap<BoundTextureKey, Arc<[u32]>>,
    /// How many times `textures` has been cleared (`IW4L_SHADOW_REUSE`
    /// drops its memo when this moves).
    clears: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct BoundTextureKey {
    pub(super) identity: TextureBindIdentity,
    pub(super) spot_shadow_select: Option<u8>,
}

impl ExactColourBindingCache {
    pub(super) fn open_epoch(&mut self, generation: MaterialGenerationId, views_revision: u64) {
        if self.generation == generation && self.views_revision == views_revision {
            return;
        }
        self.generation = generation;
        self.views_revision = views_revision;
        for slots in &mut self.textures {
            slots.clear();
        }
        self.clears = self.clears.wrapping_add(1);
    }

    pub(super) fn clears(&self) -> u64 {
        self.clears
    }

    pub(super) fn interned_n(&self) -> usize {
        self.textures.iter().map(HashMap::len).sum()
    }
}

impl ExactShadowBindingCache {
    pub(super) fn open_epoch(&mut self, generation: MaterialGenerationId, views_revision: u64) {
        if self.generation == generation && self.views_revision == views_revision {
            return;
        }
        self.generation = generation;
        self.views_revision = views_revision;
        self.clear_textures();
    }

    pub(super) fn clear_textures(&mut self) {
        self.textures.clear();
        self.clears = self.clears.wrapping_add(1);
    }

    pub(super) fn clears(&self) -> u64 {
        self.clears
    }
}
