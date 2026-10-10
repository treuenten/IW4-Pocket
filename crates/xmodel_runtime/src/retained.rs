use glam::{Mat4, Vec3};

use crate::xanim_tree::{ActiveAdditiveLayer, ActiveXAnimLeaf};
use crate::{
    AnimInstance, Attach, DObj, DObjError, HidePartBits, ModelPoseSrc, XAnimTreeError,
    XAnimTreeRuntime,
};
use anim_iw4::{Local, PartBits, apply_additive};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoneCollision {
    pub midpoint: [f32; 3],
    pub half_size: [f32; 3],
    pub radius_sq: f32,
    pub part_classification: u8,
}

#[derive(Clone, Debug, Default)]
pub struct DObjPoseRequest {
    pub tree: Option<XAnimTreeRuntime>,
    pub requested_parts: Option<PartBits>,
    pub hide_part_bits: HidePartBits,
}

impl DObjPoseRequest {
    pub const fn bind_pose() -> Self {
        Self {
            tree: None,
            requested_parts: None,
            hide_part_bits: HidePartBits::from_words([0; PartBits::WORDS]),
        }
    }

    pub fn with_tree(tree: XAnimTreeRuntime) -> Self {
        Self {
            tree: Some(tree),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetainedModelCapability {
    pub key: String,
    pub pose: ModelPoseSrc,
    pub bone_collision: Vec<Option<BoneCollision>>,

    pub contents: Option<u32>,

    pub coll_lod: i16,

    pub coll_surfs: Vec<CollSurfCollision>,

    pub movement_brushes: Vec<ModelMovementBrush>,

    pub bounds: Option<([f32; 3], [f32; 3])>,

    pub radius: Option<f32>,
}

#[derive(Clone, Debug)]
pub struct ModelMovementBrush {
    pub planes: Vec<[f32; 4]>,
    pub contents: u32,
    pub plane_surface_flags: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollTri {
    pub plane: [f32; 4],
    pub svec: [f32; 4],
    pub tvec: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollSurfCollision {
    pub bone: u16,
    pub contents: u32,
    pub surf_flags: u32,
    pub midpoint: [f32; 3],
    pub half_size: [f32; 3],
    pub tris: Vec<CollTri>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CollisionBone {
    pub bone: u16,
    pub part_classification: u8,
    pub center: [f32; 3],
    pub axes: [[f32; 3]; 3],
    pub half_size: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterializeError {
    DObj(String),
    AnimatedClipHasNoMatchingTrack,
    NonFiniteBoneTransform { bone: usize },
    BoneIndexOverflow { bone: usize },
    XAnimTree(XAnimTreeError),
}

impl From<DObjError> for MaterializeError {
    fn from(value: DObjError) -> Self {
        Self::DObj(value.to_string())
    }
}

impl From<XAnimTreeError> for MaterializeError {
    fn from(value: XAnimTreeError) -> Self {
        Self::XAnimTree(value)
    }
}

impl RetainedModelCapability {
    pub fn pose(
        &self,
        request: &DObjPoseRequest,
        world_from_model: Mat4,
    ) -> Result<Vec<Mat4>, MaterializeError> {
        self.pose_with_controller(request, world_from_model, |_, _, _| {})
    }

    pub fn pose_with_controller(
        &self,
        request: &DObjPoseRequest,
        world_from_model: Mat4,
        controller: impl FnOnce(&DObj, &PartBits, &mut [Local]),
    ) -> Result<Vec<Mat4>, MaterializeError> {
        let dobj = DObj::build(&[(&self.pose, None)])?;
        pose_dobj_with_controller(&dobj, request, world_from_model, controller)
    }

    pub fn collision(
        &self,
        request: &DObjPoseRequest,
        world_from_model: Mat4,
    ) -> Result<Vec<CollisionBone>, MaterializeError> {
        collision_models(&[(self, None)], request, world_from_model)
    }

    pub fn geom_collision(
        &self,
        request: &DObjPoseRequest,
        world_from_model: Mat4,
        contentmask: u32,
    ) -> Result<Vec<CollisionBone>, MaterializeError> {
        if self.coll_surfs.is_empty() {
            return self.collision(request, world_from_model);
        }
        geom_collision_models(&[(self, None)], request, world_from_model, contentmask)
    }

    pub fn bounds_collision_bone(&self, world_from_model: Mat4) -> Option<CollisionBone> {
        let (mid, half) = match self.bounds {
            Some((mid, half)) if half.iter().any(|&h| h > 0.0) => (mid, half),
            _ => {
                let radius = self.radius.filter(|&r| r.is_finite() && r > 0.0)?;
                ([0.0; 3], [radius, radius, radius])
            }
        };
        collision_bone_from_local_box(0, mid, half, world_from_model)
    }
}

pub fn collision_bone_from_local_box(
    bone: u16,
    mid: [f32; 3],
    half: [f32; 3],
    world_from_model: Mat4,
) -> Option<CollisionBone> {
    if !mid.iter().all(|v| v.is_finite()) || !half.iter().all(|h| h.is_finite() && *h >= 0.0) {
        return None;
    }
    if half.iter().all(|&h| h == 0.0) {
        return None;
    }
    let center = world_from_model
        .transform_point3(Vec3::from_array(mid))
        .to_array();
    if !center.iter().all(|v| v.is_finite()) {
        return None;
    }
    let mut axes = [[0.0; 3]; 3];
    let mut half_size = [0.0; 3];
    for axis in 0..3 {
        let direction = world_from_model.transform_vector3(Vec3::AXES[axis]);
        let scale = direction.length();
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        axes[axis] = (direction / scale).to_array();
        half_size[axis] = half[axis] * scale;
    }
    Some(CollisionBone {
        bone,
        part_classification: 0,
        center,
        axes,
        half_size,
    })
}

pub fn collision_models(
    models: &[(&RetainedModelCapability, Option<Attach>)],
    request: &DObjPoseRequest,
    world_from_model: Mat4,
) -> Result<Vec<CollisionBone>, MaterializeError> {
    collision_models_with_controller(models, request, world_from_model, |_, _, _| {})
}

pub fn collision_models_with_controller(
    models: &[(&RetainedModelCapability, Option<Attach>)],
    request: &DObjPoseRequest,
    world_from_model: Mat4,
    controller: impl FnOnce(&DObj, &PartBits, &mut [Local]),
) -> Result<Vec<CollisionBone>, MaterializeError> {
    if models.is_empty() {
        return Ok(Vec::new());
    }
    let descriptors: Vec<(&ModelPoseSrc, Option<Attach>)> = models
        .iter()
        .map(|(model, attach)| (&model.pose, attach.clone()))
        .collect();
    let dobj = DObj::build(&descriptors)?;
    collision_dobj_with_controller(&dobj, models, request, world_from_model, controller)
}

pub fn collision_dobj_with_controller(
    dobj: &DObj,
    models: &[(&RetainedModelCapability, Option<Attach>)],
    request: &DObjPoseRequest,
    world_from_model: Mat4,
    controller: impl FnOnce(&DObj, &PartBits, &mut [Local]),
) -> Result<Vec<CollisionBone>, MaterializeError> {
    if models.is_empty() {
        return Ok(Vec::new());
    }
    let posed = pose_dobj_with_controller(dobj, request, world_from_model, controller)?;
    let mut bones = Vec::new();
    for (slot, (model, _)) in dobj.models.iter().zip(models.iter()) {
        for local in 0..slot.bone_count {
            let Some(collision) = model
                .bone_collision
                .get(local)
                .and_then(|entry| entry.as_ref())
            else {
                continue;
            };
            let bone_index = slot.base + local;
            let Some(world_from_bone) = posed.get(bone_index).copied() else {
                continue;
            };
            let center = world_from_bone
                .transform_point3(Vec3::from_array(collision.midpoint))
                .to_array();
            let mut axes = [[0.0; 3]; 3];
            let mut half_size = [0.0; 3];
            for axis in 0..3 {
                let direction = world_from_bone.transform_vector3(Vec3::AXES[axis]);
                let scale = direction.length();
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(MaterializeError::NonFiniteBoneTransform { bone: bone_index });
                }
                axes[axis] = (direction / scale).to_array();
                half_size[axis] = collision.half_size[axis] * scale;
            }
            let bone = u16::try_from(bone_index)
                .map_err(|_| MaterializeError::BoneIndexOverflow { bone: bone_index })?;
            bones.push(CollisionBone {
                bone,
                part_classification: collision.part_classification,
                center,
                axes,
                half_size,
            });
        }
    }
    Ok(bones)
}

fn geom_collision_models(
    models: &[(&RetainedModelCapability, Option<Attach>)],
    request: &DObjPoseRequest,
    world_from_model: Mat4,
    contentmask: u32,
) -> Result<Vec<CollisionBone>, MaterializeError> {
    if models.is_empty() {
        return Ok(Vec::new());
    }
    let descriptors: Vec<(&ModelPoseSrc, Option<Attach>)> = models
        .iter()
        .map(|(model, attach)| (&model.pose, attach.clone()))
        .collect();
    let dobj = DObj::build(&descriptors)?;
    let posed = pose_dobj_with_controller(&dobj, request, world_from_model, |_, _, _| {})?;
    let mut bones = Vec::new();
    for (slot, (model, _)) in dobj.models.iter().zip(models.iter()) {
        for surf in &model.coll_surfs {
            if surf.contents & contentmask == 0 {
                continue;
            }
            let local = usize::from(surf.bone);
            let bone_index = slot.base + local;
            let Some(world_from_bone) = posed.get(bone_index).copied() else {
                continue;
            };
            let center = world_from_bone
                .transform_point3(Vec3::from_array(surf.midpoint))
                .to_array();
            let mut axes = [[0.0; 3]; 3];
            let mut half_size = [0.0; 3];
            for axis in 0..3 {
                let direction = world_from_bone.transform_vector3(Vec3::AXES[axis]);
                let scale = direction.length();
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(MaterializeError::NonFiniteBoneTransform { bone: bone_index });
                }
                axes[axis] = (direction / scale).to_array();
                half_size[axis] = surf.half_size[axis] * scale;
            }
            let bone = u16::try_from(bone_index)
                .map_err(|_| MaterializeError::BoneIndexOverflow { bone: bone_index })?;
            bones.push(CollisionBone {
                bone,
                part_classification: 0,
                center,
                axes,
                half_size,
            });
        }
    }
    Ok(bones)
}

fn locals_from_leaves(
    dobj: &DObj,
    leaves: &[ActiveXAnimLeaf<'_>],
    requested: &PartBits,
) -> Result<Vec<Local>, MaterializeError> {
    let bindings = leaves
        .iter()
        .map(|leaf| dobj.tracks_for_shared(leaf.clip))
        .collect::<Vec<_>>();
    if !leaves.is_empty()
        && !bindings
            .iter()
            .flat_map(|tracks| tracks.iter())
            .any(Option::is_some)
    {
        return Err(MaterializeError::AnimatedClipHasNoMatchingTrack);
    }
    let instances = leaves
        .iter()
        .zip(&bindings)
        .map(|(leaf, tracks)| AnimInstance {
            clip: leaf.clip,
            tracks,
            time: leaf.time,
            weight: leaf.weight,
            parts: leaf.parts,
        })
        .collect::<Vec<_>>();
    Ok(dobj.calc_anim(&instances, requested))
}

fn apply_additive_layers(
    dobj: &DObj,
    requested: &PartBits,
    dest: &mut [Local],
    layers: &[ActiveAdditiveLayer<'_>],
) -> Result<Vec<bool>, MaterializeError> {
    let mut written = vec![false; dest.len()];
    for layer in layers {
        let (mut add, controlled) = if layer.leaves.is_empty() {
            (dobj.bind_locals(), vec![false; dest.len()])
        } else {
            let bindings = layer
                .leaves
                .iter()
                .map(|leaf| dobj.tracks_for_shared(leaf.clip))
                .collect::<Vec<_>>();
            if !bindings
                .iter()
                .flat_map(|tracks| tracks.iter())
                .any(Option::is_some)
            {
                return Err(MaterializeError::AnimatedClipHasNoMatchingTrack);
            }
            let instances = layer
                .leaves
                .iter()
                .zip(&bindings)
                .map(|(leaf, tracks)| AnimInstance {
                    clip: leaf.clip,
                    tracks,
                    time: leaf.time,
                    weight: leaf.weight,
                    parts: leaf.parts,
                })
                .collect::<Vec<_>>();
            dobj.calc_anim_masked(&instances, requested)
        };
        let inner_written = apply_additive_layers(dobj, requested, &mut add, &layer.inner)?;
        for i in 0..dest.len() {
            if !controlled.get(i).copied().unwrap_or(false)
                && !inner_written.get(i).copied().unwrap_or(false)
            {
                continue;
            }
            let (rot, trans) = apply_additive(
                dest[i].rotation,
                dest[i].translation,
                add[i].rotation,
                add[i].translation,
                layer.weight,
            );
            dest[i].rotation = rot;
            dest[i].translation = trans;
            written[i] = true;
        }
    }
    Ok(written)
}

pub fn pose_dobj(
    dobj: &DObj,
    request: &DObjPoseRequest,
    world_from_model: Mat4,
) -> Result<Vec<Mat4>, MaterializeError> {
    pose_dobj_with_controller(dobj, request, world_from_model, |_, _, _| {})
}

pub fn pose_dobj_with_controller(
    dobj: &DObj,
    request: &DObjPoseRequest,
    world_from_model: Mat4,
    controller: impl FnOnce(&DObj, &PartBits, &mut [Local]),
) -> Result<Vec<Mat4>, MaterializeError> {
    let requested = request.requested_parts.unwrap_or_else(|| dobj.all_parts());
    let mut locals = if let Some(tree) = &request.tree {
        let (leaves, layers) = tree.active_pose()?;
        let mut locals = locals_from_leaves(dobj, &leaves, &requested)?;
        apply_additive_layers(dobj, &requested, &mut locals, &layers)?;
        locals
    } else {
        dobj.bind_locals()
    };
    controller(dobj, &requested, &mut locals);
    dobj.apply_duplicates(&mut locals);
    Ok(dobj.compose(&locals, world_from_model))
}

#[cfg(test)]
mod clip_track_memo_tests {
    use super::*;
    use crate::xanim_clip::{Rotation, Track, Translation};
    use crate::{
        AnimClip, XAnimNodeDefinition, XAnimNodeId, XAnimNodeKind, XAnimNodeState,
        XAnimTreeDefinition,
    };
    use glam::Quat;
    use std::sync::Arc;

    fn model(name: &str, bones: &[&str], steps: &[u8]) -> ModelPoseSrc {
        let children = bones.len() - 1;
        ModelPoseSrc {
            name: name.to_owned(),
            num_bones: bones.len(),
            num_root_bones: 1,
            scale: 1.0,
            no_scale_part_bits: [0; 6],
            bone_names: bones.iter().map(|bone| (*bone).to_owned()).collect(),
            parent_list: steps.to_vec(),
            quats: (0..children)
                .map(|i| [0, (i as i16) * 900, 0, 32000])
                .collect(),
            trans: (0..children)
                .map(|i| [i as f32 + 1.5, 0.25 * i as f32, 3.0])
                .collect(),
            base_mat: (0..bones.len())
                .map(|i| (Quat::IDENTITY, Vec3::new(0.0, i as f32, 0.0)))
                .collect(),
            root_rest: None,
        }
    }

    fn track(name: &str, turn: f32, lift: f32) -> Track {
        Track {
            name: name.to_owned(),
            rotation: Rotation::FullConstant(Quat::from_rotation_z(turn).to_array()),
            translation: Translation::Constant([0.0, 0.0, lift]),
        }
    }

    fn clip(name: &str, tracks: Vec<Track>) -> Arc<AnimClip> {
        Arc::new(AnimClip {
            name: name.to_owned(),
            framerate: 30.0,
            numframes: 10,
            looping: true,
            tracks,
            notifies: Vec::new(),
            delta_translation: Translation::Default,
        })
    }

    fn leaf(parent: u16, clip: &Arc<AnimClip>) -> XAnimNodeDefinition {
        XAnimNodeDefinition {
            parent: Some(XAnimNodeId(parent)),
            kind: XAnimNodeKind::Leaf {
                clip: Arc::clone(clip),
                parts: None,
            },
        }
    }

    fn request(clips: &[Arc<AnimClip>; 3]) -> DObjPoseRequest {
        let definition = Arc::new(
            XAnimTreeDefinition::new(vec![
                XAnimNodeDefinition {
                    parent: None,
                    kind: XAnimNodeKind::Blend,
                },
                leaf(0, &clips[0]),
                leaf(0, &clips[1]),
                XAnimNodeDefinition {
                    parent: Some(XAnimNodeId(0)),
                    kind: XAnimNodeKind::Additive,
                },
                leaf(3, &clips[2]),
            ])
            .expect("tree"),
        );
        let mut tree = XAnimTreeRuntime::new(definition);
        for (node, weight, time) in [(1, 0.65, 0.2), (2, 0.35, 0.7), (3, 0.5, 0.0), (4, 1.0, 0.4)] {
            tree.set_state(
                XAnimNodeId(node),
                XAnimNodeState {
                    time,
                    weight,
                    goal_weight: weight,
                    ..XAnimNodeState::default()
                },
            )
            .expect("state");
        }
        DObjPoseRequest {
            tree: Some(tree),
            ..DObjPoseRequest::default()
        }
    }

    /// `locals_from_leaves` and `apply_additive_layers` as they were before the
    /// memo: every leaf binds its tracks with a fresh `tracks_for`.
    fn locals_hashing_every_call(dobj: &DObj, request: &DObjPoseRequest) -> Vec<Local> {
        let requested = dobj.all_parts();
        let (leaves, layers) = request
            .tree
            .as_ref()
            .expect("tree")
            .active_pose()
            .expect("pose");
        let bindings = leaves
            .iter()
            .map(|leaf| dobj.tracks_for(leaf.clip))
            .collect::<Vec<_>>();
        let instances = leaves
            .iter()
            .zip(&bindings)
            .map(|(leaf, tracks)| AnimInstance {
                clip: leaf.clip,
                tracks,
                time: leaf.time,
                weight: leaf.weight,
                parts: leaf.parts,
            })
            .collect::<Vec<_>>();
        let mut locals = dobj.calc_anim(&instances, &requested);
        for layer in &layers {
            let bindings = layer
                .leaves
                .iter()
                .map(|leaf| dobj.tracks_for(leaf.clip))
                .collect::<Vec<_>>();
            let instances = layer
                .leaves
                .iter()
                .zip(&bindings)
                .map(|(leaf, tracks)| AnimInstance {
                    clip: leaf.clip,
                    tracks,
                    time: leaf.time,
                    weight: leaf.weight,
                    parts: leaf.parts,
                })
                .collect::<Vec<_>>();
            let (add, controlled) = dobj.calc_anim_masked(&instances, &requested);
            for i in 0..locals.len() {
                if !controlled[i] {
                    continue;
                }
                let (rot, trans) = apply_additive(
                    locals[i].rotation,
                    locals[i].translation,
                    add[i].rotation,
                    add[i].translation,
                    layer.weight,
                );
                locals[i].rotation = rot;
                locals[i].translation = trans;
            }
        }
        locals
    }

    fn locals_now(dobj: &DObj, request: &DObjPoseRequest) -> Vec<Local> {
        let requested = dobj.all_parts();
        let (leaves, layers) = request
            .tree
            .as_ref()
            .expect("tree")
            .active_pose()
            .expect("pose");
        let mut locals = locals_from_leaves(dobj, &leaves, &requested).expect("locals");
        apply_additive_layers(dobj, &requested, &mut locals, &layers).expect("layers");
        locals
    }

    fn bits(world: &[Mat4]) -> Vec<[u32; 16]> {
        world
            .iter()
            .map(|m| m.to_cols_array().map(f32::to_bits))
            .collect()
    }

    fn collision(models: &[ModelPoseSrc; 2]) -> [RetainedModelCapability; 2] {
        models.clone().map(|pose| RetainedModelCapability {
            key: pose.name.clone(),
            bone_collision: (0..pose.num_bones)
                .map(|bone| {
                    Some(BoneCollision {
                        midpoint: [bone as f32, 1.0, -2.0],
                        half_size: [2.0, 3.0, 4.0 + bone as f32],
                        radius_sq: 30.0,
                        part_classification: bone as u8,
                    })
                })
                .collect(),
            pose,
            contents: None,
            coll_lod: 0,
            coll_surfs: Vec::new(),
            movement_brushes: Vec::new(),
            bounds: None,
            radius: None,
        })
    }

    #[test]
    fn remembered_track_bindings_pose_and_hit_exactly_as_fresh_ones() {
        // The head carries a bone named like one of the body's, so first-name-
        // wins binding is exercised; one track names no bone at all.
        let models = [
            model(
                "body",
                &[
                    "tag_origin",
                    "j_spine",
                    "j_neck",
                    "j_head",
                    "j_arm",
                    "j_spine",
                ],
                &[1, 1, 1, 3, 4],
            ),
            model("head", &["j_head_root", "j_neck", "j_eye"], &[1, 1]),
        ];
        let specs = [
            (&models[0], None),
            (
                &models[1],
                Some(Attach {
                    parent_model: 0,
                    tag: "j_head".into(),
                }),
            ),
        ];
        let clips = [
            clip(
                "walk",
                vec![
                    track("j_spine", 0.3, 1.0),
                    track("j_head", -0.2, 0.5),
                    track("j_missing", 1.0, 9.0),
                ],
            ),
            clip(
                "aim",
                vec![track("j_neck", 0.7, 0.0), track("j_eye", 0.1, 0.2)],
            ),
            clip(
                "lean",
                vec![track("j_arm", 0.4, 2.0), track("j_spine", -0.1, 0.0)],
            ),
        ];
        let request = request(&clips);
        let world = Mat4::from_translation(Vec3::new(100.0, -50.0, 8.0));
        let fresh = || DObj::build(&specs).expect("dobj");

        let reference = locals_hashing_every_call(&fresh(), &request);
        let warm = fresh();
        for pass in 0..3 {
            assert_eq!(
                format!("{:?}", locals_now(&warm, &request)),
                format!("{reference:?}"),
                "pass {pass}"
            );
        }
        let cold = bits(&pose_dobj(&fresh(), &request, world).expect("pose"));
        assert_eq!(
            bits(&pose_dobj(&warm, &request, world).expect("pose")),
            cold
        );
        assert_eq!(
            bits(&pose_dobj(&warm.clone(), &request, world).expect("pose")),
            cold
        );

        let capabilities = collision(&models);
        let hit_models = [
            (&capabilities[0], None),
            (&capabilities[1], specs[1].1.clone()),
        ];
        let hits = |dobj: &DObj| {
            collision_dobj_with_controller(dobj, &hit_models, &request, world, |_, _, _| {})
                .expect("collision")
        };
        let cold_hits = hits(&fresh());
        assert!(!cold_hits.is_empty());
        for _ in 0..2 {
            let warm_hits = hits(&warm);
            assert_eq!(format!("{warm_hits:?}"), format!("{cold_hits:?}"));
        }

        for clip in &clips {
            assert_eq!(
                &warm.tracks_for_shared(clip)[..],
                &warm.tracks_for(clip)[..]
            );
        }
    }

    #[test]
    fn a_new_clip_or_a_new_bone_list_is_bound_afresh() {
        let body = model("body", &["tag_origin", "j_a", "j_b", "j_c"], &[1, 1, 1]);
        let mut dobj = DObj::build(&[(&body, None)]).expect("dobj");
        for round in 0..200 {
            // Clips come and go; a freed clip's address could be handed to the
            // next one, which binds other bones.
            let names: &[&str] = if round % 2 == 0 {
                &["j_c", "j_a"]
            } else {
                &["j_b", "nope"]
            };
            let clip = clip(
                "c",
                names.iter().map(|name| track(name, 0.0, 0.0)).collect(),
            );
            assert_eq!(
                &dobj.tracks_for_shared(&clip)[..],
                &dobj.tracks_for(&clip)[..],
                "round {round}"
            );
        }
        let kept = clip("kept", vec![track("j_b", 0.0, 0.0), track("j_c", 0.0, 0.0)]);
        assert_eq!(&dobj.tracks_for_shared(&kept)[..], &[Some(2), Some(3)]);
        dobj.bones = DObj::build(&[(&model("other", &["tag_origin", "j_c"], &[1]), None)])
            .expect("dobj")
            .bones;
        assert_eq!(&dobj.tracks_for_shared(&kept)[..], &[None, Some(1)]);
    }
}
