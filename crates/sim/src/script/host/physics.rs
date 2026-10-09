use super::args::{arg, float, string, vector};
use crate::frame::FrameWorld;
use crate::script::Namespace::{Function, Method};
use crate::script::{NativeRegistry, Runtime, Value};
use crate::world::ClientId;
use bevy_ecs::prelude::World;

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
    for name in ["physicslaunchserver", "physicslaunchclient"] {
        registry.register(Method, name, |world, receiver, args| {
            let object = object_of(world, receiver)?;
            vector(args, 0)?;
            let force = vector(args, 1)?;
            world
                .resource_mut::<super::mechanics::Mechanics>()
                .launch(object, force);
            let mut runtime = world.resource_mut::<Runtime>();
            runtime.entities.get_mut(&object).unwrap().linked_to = None;
            Ok(Value::Undefined)
        });
    }
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
        let nearest = usables
            .iter()
            .filter(|(_, _, usable, policy)| {
                !usable.barred.contains(&client) && policy.allows(&frame, client)
            })
            .filter_map(|(object, at, usable, _)| {
                let d2: f32 = (0..3).map(|i| (at[i] - eye[i]).powi(2)).sum();
                let touches =
                    super::triggers::touch_use_reach(&mut runtime, &frame, *object, client, player);
                offered(touches, d2).then_some((d2, *object, usable))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, object, usable)) = nearest else {
            continue;
        };
        selected.insert(client, object);
        if let Some(ps) = frame.player_mut(id)
            && ps.cursor_hint == 0
        {
            ps.cursor_hint = usable.cursor;
            ps.cursor_hint_string = usable.hint;
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
    fn a_touch_trigger_is_offered_where_the_hull_touches_it() {
        let inside_radius = (USE_RADIUS - 30.0).powi(2);
        let outside_radius = (USE_RADIUS + 15.0).powi(2);
        assert!(offered(Some(true), outside_radius));
        assert!(!offered(Some(false), inside_radius));
        assert!(offered(None, inside_radius));
        assert!(!offered(None, outside_radius));
    }
}
