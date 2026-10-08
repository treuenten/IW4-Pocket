use render_anim::geometry::{append_mesh, install_retained_packed};
use render_scene::SmodelPassMaterial;

use crate::model_draw::{FxModelAssetDraw, FxModelDrawPlan};

pub const XMODEL_PACKED_UNAVAILABLE: &str =
    "xmodel merge GfxPackedVertex missing or count-mismatched; decoded float is not packed VB";
pub const XMODEL_PACKED_EMPTY_PLAN: &str = "xmodel plan has no vertices";

/// The first surface whose packed rows do not cover its decoded vertices, as
/// (surface index, packed rows, decoded vertices). Appending such a surface turns
/// the packed payload of the whole effect model geometry Unavailable.
pub fn fx_model_packed_mismatch(
    surfaces: &[render_anim::fpv_pose::PosedModelSurface],
) -> Option<(usize, usize, usize)> {
    surfaces.iter().find_map(|surface| {
        let decoded = surface
            .mesh
            .attribute(bevy::prelude::Mesh::ATTRIBUTE_POSITION)
            .map_or(0, |positions| positions.len());
        let packed = surface.packed_vertices.len();
        (packed != decoded).then_some((surface.surface_index, packed, decoded))
    })
}

pub fn append_fx_model_asset(
    plan: &mut FxModelDrawPlan,
    model_index: usize,
    lod: u8,
    surfaces: &[render_anim::fpv_pose::PosedModelSurface],
    materials: &[Option<SmodelPassMaterial>],
) -> Vec<(u32, u32)> {
    let geometry = std::sync::Arc::make_mut(&mut plan.geometry);
    let mut asset_surfaces = Vec::new();
    let vertices_empty = geometry.vertices.is_empty();
    let mut packed_ok = vertices_empty
        || matches!(
            geometry.packed_vertices,
            asset_world::PackedVertexPayload::Iw4(_)
        );
    let mut packed = match std::mem::replace(
        &mut geometry.packed_vertices,
        asset_world::PackedVertexPayload::Unavailable {
            source_layout: XMODEL_PACKED_UNAVAILABLE,
        },
    ) {
        asset_world::PackedVertexPayload::Iw4(rows) => rows,
        asset_world::PackedVertexPayload::Unavailable { .. } => Vec::new(),
    };
    for (surface, material) in surfaces.iter().zip(materials) {
        let Some(material) = material else { continue };
        let before = geometry.vertices.len();
        let Some((start, count)) =
            append_mesh(&surface.mesh, &mut geometry.vertices, &mut geometry.indices)
        else {
            continue;
        };
        let decoded = geometry.vertices.len() - before;
        if packed_ok && surface.packed_vertices.len() == decoded {
            packed.extend_from_slice(&surface.packed_vertices);
        } else {
            packed_ok = false;
            packed.clear();
        }
        let surface_index = geometry.surface_ranges.len() as u32;
        geometry.surface_ranges.push((start, count));
        let material_index = geometry.materials.len() as u32;
        geometry.materials.push(material.clone());
        asset_surfaces.push((surface_index, material_index));
    }
    geometry.packed_vertices = install_retained_packed(
        packed_ok,
        packed,
        geometry.vertices.len(),
        XMODEL_PACKED_EMPTY_PLAN,
        XMODEL_PACKED_UNAVAILABLE,
    );
    geometry.assets.push(FxModelAssetDraw {
        model_index,
        lod,
        surfaces: asset_surfaces.clone(),
    });
    let topology = {
        let mut revisions = render_frame::SourceRevisions::default();
        revisions.set_topology_from(
            &geometry.indices,
            &geometry.surface_ranges,
            geometry.vertices.len(),
        );
        revisions.topology
    };
    let mut rev = plan.revision;
    plan.revisions.topology = topology;
    plan.revisions.bump_packed_write(&mut rev);
    plan.revision = rev;
    asset_surfaces
}

#[cfg(test)]
mod packed_guard_tests {
    use super::*;
    use bevy::asset::RenderAssetUsages;
    use bevy::prelude::{AlphaMode, Mesh};
    use bevy::render::mesh::{Indices, PrimitiveTopology};
    use render_anim::fpv_pose::PosedModelSurface;

    /// One triangle with `packed_rows` packed vertices for its three decoded ones.
    fn surface(surface_index: usize, packed_rows: usize) -> PosedModelSurface {
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, vec![[0.0f32; 3]; 3])
        .with_inserted_indices(Indices::U32(vec![0, 1, 2]));
        PosedModelSurface {
            surface_index,
            mesh,
            material: None,
            packed_vertices: vec![[0; asset_iw4::size::GFX_PACKED_VERTEX]; packed_rows],
            owner: Default::default(),
            model: 0,
        }
    }

    fn material() -> Option<SmodelPassMaterial> {
        Some(SmodelPassMaterial {
            model_lighting_required: true,
            color: None,
            specular: None,
            probe: None,
            atlas: None,
            alpha_mode: AlphaMode::Opaque,
            draw_mode: None,
            cull_mode: None,
            env_map_parms: [0.0; 4],
            lighting_lookup_scale: [0.0; 4],
            atlas_lookup: [0.0; 4],
            sort_key: 0,
            material_sorted_index: None,
        })
    }

    #[test]
    fn reports_the_first_surface_without_its_packed_rows() {
        assert_eq!(
            fx_model_packed_mismatch(&[surface(0, 3), surface(1, 3)]),
            None
        );
        assert_eq!(
            fx_model_packed_mismatch(&[surface(0, 3), surface(4, 0), surface(5, 2)]),
            Some((4, 0, 3))
        );
    }

    #[test]
    fn refusing_a_model_without_packed_rows_keeps_the_payload_exact() {
        let good = [surface(0, 3)];
        let bad = [surface(0, 0)];
        let mut plan = FxModelDrawPlan::default();
        for (model_index, surfaces) in [(0, &good), (1, &bad), (2, &good)] {
            if fx_model_packed_mismatch(surfaces).is_none() {
                append_fx_model_asset(&mut plan, model_index, 0, surfaces, &[material()]);
            }
        }
        assert!(plan.packed_exact());
        assert_eq!(plan.vertices().len(), 6);
        assert!(plan.asset(1, 0).is_none());
        // Appended anyway, the one model turns the whole payload Unavailable.
        append_fx_model_asset(&mut plan, 1, 0, &bad, &[material()]);
        assert!(!plan.packed_exact());
        append_fx_model_asset(&mut plan, 3, 0, &good, &[material()]);
        assert!(!plan.packed_exact());
    }
}
