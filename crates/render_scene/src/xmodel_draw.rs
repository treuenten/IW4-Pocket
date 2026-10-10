use bevy::prelude::*;

use crate::ModelLightingRequest;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XModelSurfaceDraw {
    pub surface: u32,
    pub material: u32,
    pub world_from_local: Mat4,
    pub lighting_handle: u32,

    pub pending_lighting: Option<ModelLightingRequest>,

    pub colour_refusal: Option<XModelColourRefusal>,

    pub object_id: u16,

    pub scene_light_index: u8,

    pub reflection_probe_index: u8,

    pub packed_lighting: Option<[u8; 4]>,

    pub is_scope: bool,

    pub scene_entnum: Option<u32>,
    pub body_client: Option<u32>,

    /// World-space bounding sphere of the object this surface belongs to.
    /// `None` is "unbounded": the surface is admitted to every shadow
    /// partition, which is what a producer that cannot state a bound gets.
    pub caster_bound: Option<XModelCasterBound>,
}

/// A shadow caster's world-space bounding sphere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XModelCasterBound {
    pub origin: [f32; 3],
    pub radius: f32,
}

impl XModelCasterBound {
    /// `true` when the whole sphere is behind at least one plane, so no part of
    /// it is inside the volume the planes bound. Planes face inwards and carry
    /// their distance in `[3]`, as `dpvs_iw4::bounds_culled` reads them.
    #[must_use]
    pub fn outside(self, planes: &[[f32; 4]]) -> bool {
        planes.iter().any(|plane| {
            plane[0] * self.origin[0]
                + plane[1] * self.origin[1]
                + plane[2] * self.origin[2]
                + plane[3]
                + self.radius
                <= 0.0
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XModelColourRefusal {
    CameraFrustum,
}

/// Whether one sun-shadow partition keeps a dynamic caster: unless its own
/// sphere is wholly outside the partition. No planes and no bound both mean
/// "kept": the partition that states nothing and the producer that states
/// nothing each widen the volume rather than dropping a shadow.
#[must_use]
pub fn sun_partition_keeps(bound: Option<XModelCasterBound>, planes: &[[f32; 4]]) -> bool {
    if planes.is_empty() {
        return true;
    }
    bound.is_none_or(|bound| !bound.outside(planes))
}

/// `IW4L_OFFSCREEN_SHADOWS=1` (a line in iw4l-env.txt on the phone): animated
/// script models the camera walk hid, such as the washing on Favela's lines,
/// are still posed as shadow-only casters while they touch a sun partition, so
/// their shadows do not drop off the wall as they leave the screen.
///
/// Off by default on every platform: it poses those models and draws their sun
/// shadows every frame, and stays off until that is measured on the phone. Off,
/// nothing publishes [`SunShadowCasterVolume`], a hidden model is skipped
/// before posing, rows are numbered in pose order, and the xmodel merge admits
/// a script draw only when the camera walk did. Read once at startup with the
/// other restart-only switches (`diag::memory_settings`).
#[must_use]
pub fn offscreen_shadow_casters() -> bool {
    diag::memory_settings::get().offscreen_shadows
}

/// This frame's sun-shadow volume, as the clip planes of its two partitions.
/// A producer asks it whether a model the camera does not see can still throw
/// a shadow that lands in view, and so must still be posed as a shadow caster.
/// It holds the planes of the same sun frame the shadow pass culls its
/// dynamic casters with, published before any producer poses.
#[derive(Resource, Clone, Debug, Default)]
pub struct SunShadowCasterVolume {
    /// The near and far partitions' inward clip planes. `None` when the frame
    /// has no sun shadow (no directional light, no world bounds, no view yet).
    pub partitions: Option<[Vec<[f32; 4]>; 2]>,
}

impl SunShadowCasterVolume {
    /// Replaces the planes in place, keeping last frame's allocations.
    pub fn set(&mut self, near: &[[f32; 4]], far: &[[f32; 4]]) {
        let partitions = self.partitions.get_or_insert_with(Default::default);
        for (stored, planes) in partitions.iter_mut().zip([near, far]) {
            stored.clear();
            stored.extend_from_slice(planes);
        }
    }

    pub fn clear(&mut self) {
        self.partitions = None;
    }

    /// `true` when the sphere touches either partition, by the same rule the
    /// sun pass keeps a dynamic caster with. With no sun shadow this frame
    /// nothing is admitted.
    #[must_use]
    pub fn admits(&self, bound: XModelCasterBound) -> bool {
        self.partitions.as_ref().is_some_and(|partitions| {
            partitions
                .iter()
                .any(|planes| sun_partition_keeps(Some(bound), planes))
        })
    }
}

#[cfg(test)]
mod sun_volume_tests {
    use super::*;

    /// An axis-aligned box as inward planes: `n . p + d >= 0` inside.
    fn slab(min: [f32; 3], max: [f32; 3]) -> Vec<[f32; 4]> {
        vec![
            [1.0, 0.0, 0.0, -min[0]],
            [-1.0, 0.0, 0.0, max[0]],
            [0.0, 1.0, 0.0, -min[1]],
            [0.0, -1.0, 0.0, max[1]],
            [0.0, 0.0, 1.0, -min[2]],
            [0.0, 0.0, -1.0, max[2]],
        ]
    }

    fn sphere(origin: [f32; 3], radius: f32) -> XModelCasterBound {
        XModelCasterBound { origin, radius }
    }

    /// Near covers x in 0..100, far covers x in 0..1000, both y/z in -50..50.
    fn volume() -> SunShadowCasterVolume {
        let mut volume = SunShadowCasterVolume::default();
        volume.set(
            &slab([0.0, -50.0, -50.0], [100.0, 50.0, 50.0]),
            &slab([0.0, -50.0, -50.0], [1000.0, 50.0, 50.0]),
        );
        volume
    }

    #[test]
    fn admits_a_sphere_inside_the_near_partition() {
        assert!(volume().admits(sphere([50.0, 0.0, 0.0], 5.0)));
    }

    #[test]
    fn admits_a_sphere_inside_only_the_far_partition() {
        let volume = volume();
        let bound = sphere([500.0, 0.0, 0.0], 5.0);
        let [near, far] = volume.partitions.as_ref().unwrap();
        assert!(!sun_partition_keeps(Some(bound), near));
        assert!(sun_partition_keeps(Some(bound), far));
        assert!(volume.admits(bound));
    }

    #[test]
    fn refuses_a_sphere_outside_both_partitions() {
        assert!(!volume().admits(sphere([500.0, 200.0, 0.0], 5.0)));
        assert!(!volume().admits(sphere([-20.0, 0.0, 0.0], 5.0)));
    }

    #[test]
    fn admits_a_sphere_straddling_a_partition_plane() {
        // Centre 3 units outside the far end, radius 5: part of it is inside.
        assert!(volume().admits(sphere([1003.0, 0.0, 0.0], 5.0)));
        // Touching only at the surface is not inside.
        assert!(!volume().admits(sphere([1005.0, 0.0, 0.0], 5.0)));
    }

    #[test]
    fn a_partition_without_planes_keeps_everything() {
        let mut volume = SunShadowCasterVolume::default();
        volume.set(&[], &slab([0.0; 3], [1.0; 3]));
        assert!(volume.admits(sphere([9000.0, 9000.0, 9000.0], 1.0)));
        assert!(sun_partition_keeps(None, &slab([0.0; 3], [1.0; 3])));
    }

    #[test]
    fn no_sun_shadow_admits_nothing() {
        let mut volume = volume();
        volume.clear();
        assert!(!volume.admits(sphere([50.0, 0.0, 0.0], 5.0)));
        assert!(!SunShadowCasterVolume::default().admits(sphere([0.0; 3], 1e6)));
    }

    #[test]
    fn set_replaces_last_frames_planes() {
        let mut volume = volume();
        volume.set(
            &slab([2000.0; 3], [2100.0; 3]),
            &slab([2000.0; 3], [2100.0; 3]),
        );
        assert!(!volume.admits(sphere([50.0, 0.0, 0.0], 5.0)));
        assert!(volume.admits(sphere([2050.0; 3], 5.0)));
    }
}
