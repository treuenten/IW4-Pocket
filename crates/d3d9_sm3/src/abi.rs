use alloc::vec::Vec;

use crate::ir::Sm3Register;

pub use d3d9_decl::{DeclType, Semantic};
pub use d3d9_state::{ALPHA_REF_SCALE, AlphaTest, CompareFunc};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SamplerTextureDimension {
    D2,
    Cube,
    D3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VertexInput {
    pub register: Sm3Register,
    pub location: Option<u32>,
    pub decl_type: Option<DeclType>,
    pub semantic: Semantic,
}

pub fn missing_vertex_element(semantic: Semantic) -> [f32; 4] {
    match semantic.usage {
        d3d9_decl::D3DDECLUSAGE_COLOR | d3d9_decl::D3DDECLUSAGE_TEXCOORD => [0.0, 0.0, 0.0, 1.0],
        _ => [0.0, 0.0, 0.0, 0.0],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VaryingLink {
    pub semantic: Semantic,
    pub vertex_register: Sm3Register,
    pub pixel_register: Option<Sm3Register>,
    pub location: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConstantSlot {
    pub register: u16,

    pub program_defined: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SamplerSlot {
    pub register: u16,
    pub dimension: SamplerTextureDimension,

    pub depth_compare: bool,
    /// Read as opaque black (0, 0, 0, 1) without a fetch.
    pub black: bool,
}

// The lowered-WGSL disk cache keys on this text: a slot that is not `black`
// prints exactly as it did before the field existed, so turning the field on
// for some passes leaves every other pass's cached WGSL valid.
impl core::fmt::Debug for SamplerSlot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut slot = f.debug_struct("SamplerSlot");
        slot.field("register", &self.register)
            .field("dimension", &self.dimension)
            .field("depth_compare", &self.depth_compare);
        if self.black {
            slot.field("black", &self.black);
        }
        slot.finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassLoweringAbi {
    pub vertex_inputs: Vec<VertexInput>,
    pub position: VaryingLink,
    pub varyings: Vec<VaryingLink>,
    pub vertex_constants: Vec<ConstantSlot>,
    pub pixel_constants: Vec<ConstantSlot>,
    pub samplers: Vec<SamplerSlot>,

    pub alpha_tests: Vec<AlphaTest>,
}
