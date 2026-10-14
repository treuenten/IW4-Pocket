use super::args::{arg, float, string, vector};
use super::triggers::UsableRank;
use crate::frame::FrameWorld;
use crate::script::Namespace::{Function, Method};
use crate::script::{NativeRegistry, Runtime, Value};
use crate::world::ClientId;
use bevy_ecs::prelude::World;
use playerstate_iw4::ENTITYNUM_NONE;

const USE_RADIUS: f32 = 128.0;
const HINTS: [&str; 5] = [
    "HINT_NONE",
    "HINT_NOICON",
    "HINT_ACTIVATE",
    "HINT_HEALTH",
    "HINT_FRIENDLY",
];

fn object_of(world: &World, receiver: &Value) -> Result<u64, String> {
    super::natives::engine::entity_id(world, receiver)
}

/// The use state an entity gets the first time a script sets its hint, its
/// cursor, its usability or a player bar on it.
///
/// MW2 map use triggers are usable from spawn. setHintString and
/// setCursorHint only set what the hint shows, and no MP script calls
/// makeUsable on a trigger: the bomb sites (_gameobjects.gsc setUseHintText,
/// called from sab.gsc resetBombsite, sd.gsc and dd.gsc), vending machines
/// (_dynamic_world.gsc vending_machine) and elevators (_elevator.gsc) just set
/// a hint and wait for "trigger". Script models stay unusable until
/// makeUsable (_airdrop.gsc crateSetupForUse, _autosentry.gsc).
fn new_usable(classname: &str) -> super::entities::Usable {
    super::entities::Usable {
        cursor: 1,
        hint: -1,
        enabled: super::triggers::is_use_trigger(classname),
        barred: Default::default(),
    }
}

fn usable<'a>(
    world: &'a mut World,
    receiver: &Value,
) -> Result<&'a mut super::entities::Usable, String> {
    let object = object_of(world, receiver)?;
    let runtime = world.resource_mut::<Runtime>().into_inner();
    let entity = runtime.entities.get_mut(&object).unwrap();
    Ok(entity
        .usable
        .get_or_insert_with(|| new_usable(&entity.classname)))
}

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry.register(Function, "physicsexplosionsphere", |world, _, args| {
        if args.len() != 4 {
            return Err(
                "PhysicsExplosionSphere requires origin, outer radius, inner radius and magnitude"
                    .into(),
            );
        }
        let center = vector(args, 0)?;
        let outer = float(args, 1)?;
        let inner = float(args, 2)?;
        let magnitude = float(args, 3)?;
        if center.iter().any(|v| !v.is_finite())
            || !outer.is_finite()
            || !inner.is_finite()
            || !magnitude.is_finite()
            || inner < 0.0
            || outer < inner
        {
            return Err(
                "physics sphere requires finite values and 0 <= inner radius <= outer radius"
                    .into(),
            );
        }
        world.resource_scope::<super::mechanics::Mechanics, _>(|world, mut mechanics| {
            mechanics.explode(
                &mut world.resource_mut::<Runtime>(),
                center,
                outer,
                inner,
                magnitude,
            );
        });
        let tick = world.resource::<crate::step::StepRequest>().tick;
        FrameWorld::from_world(world).push_entity_event(
            tick,
            crate::EventAudience::All,
            entity_iw4::EntityEventKind::PHYS_EXPLOSION_SPHERE,
            crate::EntityEventPayload {
                number: i32::from(trace_iw4::ENTITYNUM_WORLD),
                origin: center,
                origin2: [outer, inner, magnitude],
                ..Default::default()
            },
        );
        Ok(Value::Undefined)
    });
    registry.register(Method, "physicslaunchserver", |world, receiver, args| {
        super::mechanics::physics_launch(world, receiver, args, false)
    });
    registry.register(Method, "physicslaunchclient", |world, receiver, args| {
        super::mechanics::physics_launch(world, receiver, args, true)
    });
    registry.register(Method, "makeusable", |world, receiver, _| {
        usable(world, receiver)?.enabled = true;
        Ok(Value::Undefined)
    });
    registry.register(Method, "makeunusable", |world, receiver, _| {
        usable(world, receiver)?.enabled = false;
        Ok(Value::Undefined)
    });
    registry.register(Method, "setcursorhint", |world, receiver, args| {
        let name = string(args, 0)?;
        let cursor = HINTS
            .iter()
            .position(|hint| hint.eq_ignore_ascii_case(&name))
            .ok_or_else(|| format!("{name} is not a valid hint type"))?;
        usable(world, receiver)?.cursor = cursor as i32;
        Ok(Value::Undefined)
    });
    registry.register(Method, "sethintstring", |world, receiver, args| {
        let hint = super::hud::string_index(world, arg(args, 0)?)?;
        usable(world, receiver)?.hint = hint;
        Ok(Value::Undefined)
    });
    registry.register(Method, "disableplayeruse", |world, receiver, args| {
        let client = super::natives::player::player(world, arg(args, 0)?)?;
        usable(world, receiver)?.barred.insert(client);
        Ok(Value::Undefined)
    });
    registry.register(Method, "enableplayeruse", |world, receiver, args| {
        let client = super::natives::player::player(world, arg(args, 0)?)?;
        usable(world, receiver)?.barred.remove(&client);
        Ok(Value::Undefined)
    });
}

/// Whether a usable's hint shows on the HUD: use_hint.rs draws a script hint
/// only for a cursor other than HINT_NONE with a hint string.
fn shows_hint(usable: &super::entities::Usable) -> bool {
    usable.cursor > 0 && usable.hint >= 0
}

/// The model and pose of a usable's script model (a sentry, a tactical
/// insertion), from its collision row.
fn usable_dobj<'f>(
    runtime: &Runtime,
    frame: &'f FrameWorld,
    object: u64,
) -> Option<&'f crate::AuthorityDObjState> {
    let owner = crate::AuthorityModelOwner::ScriptModel(runtime.entities.get(&object)?.presence?);
    let rows = frame.entity_collision_capabilities();
    let index = rows.binary_search_by_key(&owner, |row| row.owner).ok()?;
    rows[index].dobj.as_ref()
}

/// Where Player_GetUseList measures a usable from: the middle of its bounds.
/// A care package's are its cloned crate collision, a use trigger's its
/// volume, a script model's its model's. Anything else is measured from its
/// origin.
fn use_center(runtime: &Runtime, frame: &FrameWorld, object: u64, origin: [f32; 3]) -> [f32; 3] {
    let Some(entity) = runtime.entities.get(&object) else {
        return origin;
    };
    if let Some((mins, maxs)) = super::triggers::local_bounds(entity, frame)
        .filter(|(mins, maxs)| mins.iter().chain(maxs).all(|v| v.is_finite()))
    {
        return std::array::from_fn(|i| origin[i] + (mins[i] + maxs[i]) * 0.5);
    }
    usable_dobj(runtime, frame, object)
        .and_then(|dobj| {
            let (mid, _) = dobj.capability.as_ref()?.bounds?;
            let at = dobj
                .world_from_model
                .transform_point3(glam::Vec3::from(mid))
                .to_array();
            at.iter().all(|v| v.is_finite()).then_some(at)
        })
        .unwrap_or(origin)
}

/// Where Player_GetUseList's sight line to a usable ends: a turret's tag_aim
/// (a sentry gun to pick up), anything else the middle of its bounds.
fn sight_point(runtime: &Runtime, frame: &FrameWorld, object: u64, origin: [f32; 3]) -> [f32; 3] {
    runtime
        .engine
        .turrets
        .contains_key(&object)
        .then(|| usable_dobj(runtime, frame, object)?.tag_world_pose("tag_aim"))
        .flatten()
        .map_or_else(|| use_center(runtime, frame, object, origin), |(at, _)| at)
}

/// Whether a player sees a usable he reaches, as Player_GetUseList requires
/// of everything but a use trigger he stands in (`inside`): a point trace
/// from his eye to the usable, stopped by solid and glass (the trace the
/// item code makes to a dropped weapon). The usable's own collision does not
/// count, or a care package could never be seen: the line ends in its
/// middle. Anything else solid hides it, the world or another entity, so a
/// crate on a walkway is not offered to a player in the alley below.
fn usable_seen(
    runtime: &Runtime,
    frame: &FrameWorld,
    eye: [f32; 3],
    object: u64,
    origin: [f32; 3],
    inside: bool,
) -> bool {
    if inside {
        return true;
    }
    let own = runtime
        .entities
        .get(&object)
        .and_then(|entity| entity.presence)
        .map(crate::AuthorityModelOwner::ScriptModel);
    let point = sight_point(runtime, frame, object, origin);
    frame
        .trace_world_except(eye, point, crate::item::USE_SIGHT_MASK, own)
        .fraction
        >= 1.0
}

/// Whether a usable is offered to a player: a trigger_use_touch when his hull
/// touches it (`touches` is `Some`), anything else within the use radius of
/// his eye.
fn offered(touches: Option<bool>, eye_distance_squared: f32) -> bool {
    touches.unwrap_or(eye_distance_squared <= USE_RADIUS * USE_RADIUS)
}

pub(crate) fn select_usables(world: &mut World) {
    super::triggers::refresh_claims(world);
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let usables: Vec<(
        u64,
        [f32; 3],
        super::entities::Usable,
        super::triggers::TriggerPolicy,
    )> = {
        let candidates: Vec<(u64, super::entities::Usable, super::triggers::TriggerPolicy)> =
            runtime
                .entities
                .iter()
                .filter_map(|(object, e)| {
                    Some((
                        *object,
                        e.usable.clone().filter(|u| u.enabled)?,
                        e.trigger_policy.clone(),
                    ))
                })
                .collect();
        candidates
            .into_iter()
            .filter_map(
                |(object, usable, policy)| match runtime.object_field(object, "origin") {
                    Value::Vector(at) => Some((object, at, usable, policy)),
                    _ => None,
                },
            )
            .collect()
    };
    let clients: Vec<(u32, u64)> = runtime
        .players
        .iter()
        .map(|(client, slot)| (*client, slot.object))
        .collect();
    let mut selected = std::collections::BTreeMap::new();
    let mut frame = FrameWorld::from_world(world);
    for (client, player) in clients {
        let id = ClientId(client);
        if !frame.client_meta(id).is_some_and(|m| {
            m.lifecycle == crate::ClientLifecycle::Alive && !m.controls.usability_disabled
        }) {
            continue;
        }
        let Some(ps) = frame.player(id).copied() else {
            continue;
        };
        let eye = [
            ps.origin[0],
            ps.origin[1],
            ps.origin[2] + ps.view_height_current,
        ];
        // A usable the player cannot see is dropped before any is chosen, so
        // it neither shows its hint nor takes his press, and a visible one
        // behind it can be offered.
        let nearest = usables
            .iter()
            .filter(|(_, _, usable, policy)| {
                !usable.barred.contains(&client) && policy.allows(&frame, client)
            })
            .filter_map(|(object, at, usable, _)| {
                let d2: f32 = (0..3).map(|i| (at[i] - eye[i]).powi(2)).sum();
                let touches =
                    super::triggers::touch_use_reach(&mut runtime, &frame, *object, client, player);
                if !offered(touches, d2) {
                    return None;
                }
                let inside =
                    super::triggers::in_use_trigger(&mut runtime, &frame, *object, client, player);
                usable_seen(&runtime, &frame, eye, *object, *at, inside)
                    .then_some((d2, *object, at, usable, inside))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, object, at, usable, inside)) = nearest else {
            continue;
        };
        selected.insert(client, object);
        // The item code has written this tick's hint: a weapon in reach, or
        // nothing. A use trigger the player stands in takes it from a weapon,
        // and a script usable does when it ranks better (use_offer).
        let item = crate::item::hints_item(ps.cursor_hint)
            .then(|| crate::item::item_hint_score(&frame, id, &ps).unwrap_or(f32::INFINITY));
        let map_trigger = runtime
            .entities
            .get(&object)
            .is_some_and(|entity| super::triggers::is_use_trigger(&entity.classname));
        let rank = if !shows_hint(usable) {
            UsableRank::Behind
        } else if inside {
            UsableRank::InTrigger
        } else if !map_trigger {
            let (forward, _, _) = math_iw4::angle_vectors(ps.viewangles);
            let center = use_center(&runtime, &frame, object, *at);
            UsableRank::Scored(crate::item::use_score(eye, forward, center))
        } else {
            UsableRank::Behind
        };
        let offer = super::triggers::use_offer(item, Some((object, rank)));
        if (ps.cursor_hint == 0 || item.is_some())
            && offer == super::triggers::PressTarget::Usable(object)
            && let Some(ps) = frame.player_mut(id)
        {
            ps.cursor_hint = usable.cursor;
            ps.cursor_hint_string = usable.hint;
            ps.cursor_hint_ent_index = ENTITYNUM_NONE;
            ps.cursor_hint_dual_wield = 0;
        }
    }
    runtime.use_selected = selected;
    *world.resource_mut::<Runtime>() = runtime;
}

#[cfg(test)]
mod usable_tests {
    use super::super::triggers::is_use_trigger;
    use super::*;

    #[test]
    fn a_hint_leaves_a_use_trigger_usable() {
        assert!(new_usable("trigger_use").enabled);
        assert!(new_usable("trigger_use_touch").enabled);
    }

    #[test]
    fn other_entities_wait_for_make_usable() {
        for classname in [
            "script_model",
            "script_brushmodel",
            "trigger_multiple",
            "trigger_radius",
        ] {
            assert!(!new_usable(classname).enabled, "{classname}");
        }
    }

    #[test]
    fn only_use_triggers_count_as_use_triggers() {
        assert!(is_use_trigger("trigger_use"));
        assert!(is_use_trigger("trigger_use_touch"));
        assert!(!is_use_trigger("trigger_once"));
        assert!(!is_use_trigger("trigger_damage"));
        assert!(!is_use_trigger("script_model"));
    }

    #[test]
    fn a_hint_shows_with_a_cursor_and_a_string() {
        let site = |cursor, hint| super::super::entities::Usable {
            cursor,
            hint,
            ..new_usable("trigger_use_touch")
        };
        assert!(shows_hint(&site(1, 7)));
        assert!(!shows_hint(&site(1, -1)));
        assert!(!shows_hint(&site(0, 7)));
    }

    #[test]
    fn a_touch_trigger_is_offered_where_the_hull_touches_it() {
        let inside_radius = (USE_RADIUS - 30.0).powi(2);
        let outside_radius = (USE_RADIUS + 15.0).powi(2);
        assert!(offered(Some(true), outside_radius));
        assert!(!offered(Some(false), inside_radius));
        assert!(offered(None, inside_radius));
        assert!(!offered(None, outside_radius));
    }
}

#[cfg(test)]
mod sight_tests {
    use super::super::entities::EntityKind;
    use super::*;

    const CRATE_HANDLE: u32 = 1;

    fn brush(lo: [f32; 3], hi: [f32; 3], contents: u32) -> crate::SimBrush {
        crate::SimBrush {
            planes: vec![
                [1.0, 0.0, 0.0, hi[0]],
                [-1.0, 0.0, 0.0, -lo[0]],
                [0.0, 1.0, 0.0, hi[1]],
                [0.0, -1.0, 0.0, -lo[1]],
                [0.0, 0.0, 1.0, hi[2]],
                [0.0, 0.0, -1.0, -lo[2]],
            ],
            contents,
            plane_surface_flags: vec![0; 6],
            glass_encoded: 0,
        }
    }

    /// The walkway up Favela's spawn alley (IP-097 run p2): a care package
    /// landed on it at `CRATE`, and its hint showed to a player standing in
    /// the alley on either side of it, facing the walkway's wall. The world
    /// is the walkway, a solid block here, and `more` world brushes; the
    /// crate's collision is brush model 1, as crateSetupForUse clones it.
    struct Walkway {
        sim: crate::SimWorld,
        runtime: Runtime,
        origins: std::collections::BTreeMap<u64, [f32; 3]>,
    }

    /// An entity as create_entity makes one. Its script fields are left out:
    /// they need a loaded program, and the sight check reads none.
    fn bare(classname: &str) -> super::super::entities::ScriptEntity {
        super::super::entities::ScriptEntity {
            number: 0,
            kind: EntityKind::Spawned,
            classname: classname.into(),
            cylinder: None,
            brush: None,
            trigger_model: None,
            hidden: false,
            shown_to: 0,
            solid: true,
            linked_to: None,
            light: 1.0,
            attachments: Vec::new(),
            contents: 0,
            audience: super::super::entities::HudAudience::All,
            presence: None,
            missile_owner: None,
            can_damage: false,
            can_radius_damage: true,
            part_ops: Vec::new(),
            anim_op: None,
            loop_sound: None,
            usable: None,
            trigger_policy: Default::default(),
        }
    }

    const CRATE: [f32; 3] = [1186.7, -275.6, 318.2];
    const CRATE_MINS: [f32; 3] = [-16.0, -16.0, 0.0];
    const CRATE_MAXS: [f32; 3] = [16.0, 16.0, 32.0];
    /// Standing on the walkway 78 units east of the crate.
    const ON_THE_WALKWAY: [f32; 3] = [1265.0, -276.0, 378.0];
    /// The eyes of the player in the alley south and north of the walkway in
    /// p2's screenshots, each within 128 units of the crate.
    const SOUTH_OF_IT: [f32; 3] = [1183.0, -351.0, 253.0];
    const NORTH_OF_IT: [f32; 3] = [1183.0, -197.0, 249.0];

    impl Walkway {
        fn new(more: Vec<crate::SimBrush>) -> Self {
            let mut brushes = vec![brush(
                [1100.0, -335.0, 186.0],
                [1300.0, -215.0, 318.0],
                crate::bullet_collision::CONTENTS_SOLID,
            )];
            brushes.extend(more);
            let world = brushes.len();
            brushes.push(brush(
                CRATE_MINS,
                CRATE_MAXS,
                crate::bullet_collision::CONTENTS_SOLID,
            ));
            // One leaf holds the world's brushes; the crate's is only in its
            // brush model, as in a compiled map.
            let bsp = crate::SimClipBsp {
                nodes: vec![clipmap_iw4::ClipNode {
                    plane: [0.0, 0.0, 1.0, -100_000.0],
                    children: [-1, -1],
                }],
                leaves: vec![clipmap_iw4::ClipLeaf {
                    first_brush: 0,
                    num_brushes: world as u16,
                    ..Default::default()
                }],
                leafbrushes: (0..=world as u16).collect(),
            };
            let model = |first_brush, num_brushes, mins, maxs| clipmap_iw4::ClipCmodel {
                mins,
                maxs,
                radius: 64.0,
                first_brush,
                num_brushes,
            };
            let cmodels = crate::SimClipCmodels {
                models: vec![
                    model(0, world as u16, [-4096.0; 3], [4096.0; 3]),
                    model(world as u32, 1, CRATE_MINS, CRATE_MAXS),
                ],
                triggers: Vec::new(),
            };
            let mut content = crate::SimContentBuilder::bootstrap();
            content.set_clip_map(brushes, bsp, crate::SimClipMesh::default(), cmodels);
            let mut sim = crate::SimWorld::default();
            sim.frame().install_content(content.finish());
            Self {
                sim,
                runtime: Runtime::default(),
                origins: Default::default(),
            }
        }

        fn add(&mut self, entity: super::super::entities::ScriptEntity, origin: [f32; 3]) -> u64 {
            let object = self.origins.len() as u64 + 1;
            self.runtime.entities.insert(object, entity);
            self.origins.insert(object, origin);
            object
        }

        /// A script model with the crate's collision at `origin`.
        fn place_crate(&mut self, origin: [f32; 3], presence: u32) -> u64 {
            let presence = crate::ScriptModelId::from_wire(presence);
            let object = self.add(
                super::super::entities::ScriptEntity {
                    brush: Some(CRATE_HANDLE),
                    presence: Some(presence),
                    ..bare("script_model")
                },
                origin,
            );
            self.sim.frame().insert_collision_owner(
                crate::EntityCollisionCapabilities::current_tick(
                    crate::AuthorityModelOwner::ScriptModel(presence),
                    None,
                    vec![crate::LinkedBrushCollisionBrush {
                        cmodel_handle: CRATE_HANDLE,
                        origin,
                        angles: [0.0; 3],
                    }],
                ),
            );
            object
        }

        fn seen(&mut self, eye: [f32; 3], object: u64, inside: bool) -> bool {
            let origin = self.origins[&object];
            let frame = self.sim.frame();
            usable_seen(&self.runtime, &frame, eye, object, origin, inside)
        }
    }

    fn within_reach(eye: [f32; 3], at: [f32; 3]) -> bool {
        offered(None, (0..3).map(|i| (at[i] - eye[i]).powi(2)).sum())
    }

    #[test]
    fn a_care_package_on_the_walkway_is_out_of_sight_from_the_alley() {
        let mut walkway = Walkway::new(Vec::new());
        let package = walkway.place_crate(CRATE, 900);
        for eye in [SOUTH_OF_IT, NORTH_OF_IT] {
            assert!(within_reach(eye, CRATE));
            assert!(!walkway.seen(eye, package, false));
        }
        assert!(within_reach(ON_THE_WALKWAY, CRATE));
        assert!(walkway.seen(ON_THE_WALKWAY, package, false));
    }

    #[test]
    fn a_usable_is_seen_through_its_own_collision_but_not_another() {
        let mut walkway = Walkway::new(Vec::new());
        let package = walkway.place_crate(CRATE, 900);
        // The line ends in the middle of the crate, inside its collision.
        let middle = use_center(&walkway.runtime, &walkway.sim.frame(), package, CRATE);
        assert_eq!(middle, [CRATE[0], CRATE[1], CRATE[2] + 16.0]);
        let through = walkway.sim.frame().trace_world(
            ON_THE_WALKWAY,
            middle,
            [0.0; 3],
            [0.0; 3],
            crate::item::USE_SIGHT_MASK,
        );
        assert!(through.fraction < 1.0);
        assert!(walkway.seen(ON_THE_WALKWAY, package, false));
        // To a player crouching on the walkway, a second care package landed
        // between them hides the first.
        let crouching = [1265.0, -276.0, 358.0];
        assert!(walkway.seen(crouching, package, false));
        walkway.place_crate([1215.0, -276.0, 318.2], 901);
        assert!(!walkway.seen(crouching, package, false));
    }

    #[test]
    fn clip_does_not_hide_a_usable_but_glass_does() {
        let across = |contents| {
            vec![brush(
                [1235.0, -400.0, 318.0],
                [1240.0, -150.0, 500.0],
                contents,
            )]
        };
        let player_clip = 0x1_0000;
        let mut clipped = Walkway::new(across(player_clip));
        let package = clipped.place_crate(CRATE, 900);
        assert!(clipped.seen(ON_THE_WALKWAY, package, false));
        let glass = 0x10;
        let mut glazed = Walkway::new(across(glass));
        let package = glazed.place_crate(CRATE, 900);
        assert!(!glazed.seen(ON_THE_WALKWAY, package, false));
    }

    #[test]
    fn a_use_trigger_the_player_stands_in_needs_no_sight_line() {
        // A bomb site with the solid bomb case in its middle. A player
        // standing in the site is offered the plant, though the line from
        // his eye to the site's middle runs into the case.
        let case = brush(
            [1000.0, 0.0, 186.0],
            [1040.0, 40.0, 240.0],
            crate::bullet_collision::CONTENTS_SOLID,
        );
        let mut walkway = Walkway::new(vec![case]);
        let site = walkway.add(
            super::super::entities::ScriptEntity {
                kind: EntityKind::Map,
                cylinder: Some((64.0, 72.0)),
                ..bare("trigger_use_touch")
            },
            [1020.0, 20.0, 186.0],
        );
        let eye = [1070.0, 20.0, 246.0];
        assert!(walkway.seen(eye, site, true));
        assert!(!walkway.seen(eye, site, false));
    }
}
