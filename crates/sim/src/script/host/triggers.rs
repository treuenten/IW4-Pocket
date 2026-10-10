use crate::bullet_collision::{PLAYER_MAXS, PLAYER_MINS};
use crate::frame::FrameWorld;
use crate::script::runtime::raise;
use crate::script::{Runtime, Value};
use bevy_ecs::prelude::World;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TriggerPolicy {
    pub team: Option<i32>,
    pub claimed_by: Option<u32>,
    pub grenade_touch: bool,
    pub accumulated_damage: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct GrenadeTouch {
    pub projectile: crate::ProjectileState,
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub damage: i32,
}

pub(crate) fn record_grenade_touch(
    world: &mut World,
    projectile: crate::ProjectileState,
    start: [f32; 3],
    end: [f32; 3],
    damage: i32,
) {
    let Some(mut runtime) = world.get_resource_mut::<Runtime>() else {
        return;
    };
    if runtime.program.is_some()
        && runtime.started
        && runtime
            .entities
            .values()
            .any(|entity| entity.trigger_policy.grenade_touch)
    {
        runtime.grenade_touches.push(GrenadeTouch {
            projectile,
            start,
            end,
            damage,
        });
    }
}

fn integer(runtime: &mut Runtime, object: u64, field: &str) -> i32 {
    match runtime.object_field(object, field) {
        Value::Int(value) => value,
        _ => 0,
    }
}

struct TriggerHit {
    activator: Value,
    amount: i32,
    direction: [f32; 3],
    point: [f32; 3],
    means: &'static str,
}

type DamageNotify = (u64, &'static str, Vec<Value>);

fn accepts_means(flags: i32, means: &str) -> bool {
    let projectile = matches!(
        means,
        "MOD_GRENADE" | "MOD_GRENADE_SPLASH" | "MOD_PROJECTILE" | "MOD_PROJECTILE_SPLASH"
    );
    !(flags & 1 != 0 && means == "MOD_PISTOL_BULLET"
        || flags & 2 != 0 && means == "MOD_RIFLE_BULLET"
        || flags & 4 != 0 && projectile
        || flags & 8 != 0 && (projectile || means == "MOD_EXPLOSIVE")
        || flags & 16 != 0 && matches!(means, "MOD_GRENADE_SPLASH" | "MOD_PROJECTILE_SPLASH")
        || flags & 32 != 0 && means == "MOD_MELEE"
        || flags & 256 != 0
            && matches!(
                means,
                "MOD_UNKNOWN" | "MOD_CRUSH" | "MOD_FALLING" | "MOD_SUICIDE" | "MOD_TRIGGER_HURT"
            ))
}

fn damage_trigger(
    runtime: &mut Runtime,
    trigger: u64,
    hit: &TriggerHit,
    raised: &mut Vec<DamageNotify>,
) {
    raised.push((
        trigger,
        "damage",
        vec![
            Value::Int(hit.amount),
            hit.activator.clone(),
            Value::Vector(hit.direction),
            Value::Vector(hit.point),
            Value::string(hit.means),
        ],
    ));
    let threshold = integer(runtime, trigger, "threshold");
    let accumulate = integer(runtime, trigger, "accumulate");
    let flags = integer(runtime, trigger, "spawnflags");
    if hit.amount < threshold || !accepts_means(flags, hit.means) {
        return;
    }
    let policy = &mut runtime.entities.get_mut(&trigger).unwrap().trigger_policy;
    policy.accumulated_damage = policy
        .accumulated_damage
        .saturating_add(i64::from(hit.amount));
    if accumulate > 0 && policy.accumulated_damage < i64::from(accumulate) {
        return;
    }
    policy.accumulated_damage = 0;
    raised.push((trigger, "trigger", vec![hit.activator.clone()]));
    let wait = runtime.object_field(trigger, "wait");
    let once = flags & 512 != 0
        || match wait {
            Value::Int(value) => value <= 0,
            Value::Float(value) => value <= 0.0,
            _ => false,
        };
    if once {
        runtime.fired_once.insert(trigger);
        runtime.pending_deletes.push(trigger);
    }
}

fn active_damage_triggers(runtime: &Runtime) -> Vec<u64> {
    runtime
        .entities
        .iter()
        .filter(|(object, entity)| {
            entity.classname.as_ref() == "trigger_damage"
                && !runtime.fired_once.contains(object)
                && !runtime.pending_deletes.contains(object)
        })
        .map(|(object, _)| *object)
        .collect()
}

fn attacker(
    runtime: &Runtime,
    client: Option<crate::ClientId>,
    missile: Option<crate::ProjectileId>,
) -> Value {
    if let Some(object) = missile.and_then(|id| runtime.missiles.get(&id)) {
        let owner = runtime
            .entities
            .get(object)
            .and_then(|entity| entity.missile_owner)
            .filter(|owner| runtime.players.values().any(|slot| slot.object == *owner));
        return owner
            .or(runtime.engine.world)
            .map_or(Value::Undefined, Value::Object);
    }
    client
        .and_then(|client| runtime.players.get(&client.0).map(|slot| slot.object))
        .or(runtime.engine.world)
        .map_or(Value::Undefined, Value::Object)
}

fn normalized(direction: [f32; 3]) -> [f32; 3] {
    let length = math_iw4::vec3_length(direction);
    if length > 0.0 {
        direction.map(|axis| axis / length)
    } else {
        [0.0; 3]
    }
}

pub(crate) fn damage_line(
    world: &mut World,
    start: [f32; 3],
    end: [f32; 3],
    amount: i32,
    client: crate::ClientId,
    missile: Option<crate::ProjectileId>,
    means: &'static str,
) {
    if amount <= 0
        || world
            .get_resource::<Runtime>()
            .is_none_or(|runtime| runtime.program.is_none() || !runtime.started)
    {
        return;
    }
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let hit = TriggerHit {
        activator: attacker(&runtime, Some(client), missile),
        amount,
        direction: normalized(std::array::from_fn(|i| end[i] - start[i])),
        point: [0.0; 3],
        means,
    };
    let mut raised = Vec::new();
    {
        let frame = FrameWorld::from_world(world);
        for trigger in active_damage_triggers(&runtime) {
            if volume(&mut runtime, &frame, trigger)
                .is_some_and(|volume| volume.crosses(start, end))
            {
                damage_trigger(&mut runtime, trigger, &hit, &mut raised);
            }
        }
    }
    *world.resource_mut::<Runtime>() = runtime;
    for (trigger, name, args) in raised {
        raise(world, Value::Object(trigger), name, args);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TriggerBlast {
    pub origin: [f32; 3],
    pub radius: f32,
    pub max: f32,
    pub min: f32,
    pub client: Option<crate::ClientId>,
    pub missile: Option<crate::ProjectileId>,
    pub means: &'static str,
    pub ignore_model: Option<crate::ScriptModelId>,
    pub cone: Option<([f32; 3], f32)>,
}

fn damage_bounds(
    runtime: &mut Runtime,
    frame: &FrameWorld,
    object: u64,
) -> Option<([f32; 3], [f32; 3])> {
    let entity = runtime.entities.get(&object)?;
    let (cylinder, brush, trigger_model) = (entity.cylinder, entity.brush, entity.trigger_model);
    let Value::Vector(origin) = runtime.object_field(object, "origin") else {
        return None;
    };
    let (mins, maxs) = if let Some((radius, height)) = cylinder {
        ([-radius, -radius, 0.0], [radius, radius, height])
    } else if let Some(model) = trigger_model {
        let hulls = frame.clip_cmodels().triggers.get(model as usize)?;
        let mut mins = [f32::INFINITY; 3];
        let mut maxs = [f32::NEG_INFINITY; 3];
        for hull in hulls {
            for i in 0..3 {
                mins[i] = mins[i].min(hull.mid[i] - hull.half[i]);
                maxs[i] = maxs[i].max(hull.mid[i] + hull.half[i]);
            }
        }
        (mins, maxs)
    } else {
        let model = frame.clip_cmodels().models.get(brush? as usize)?;
        (model.mins, model.maxs)
    };
    let mins = std::array::from_fn(|i| mins[i] + origin[i]);
    let maxs = std::array::from_fn(|i| maxs[i] + origin[i]);
    (origin
        .into_iter()
        .chain(mins)
        .chain(maxs)
        .all(f32::is_finite)
        && (0..3).all(|i| mins[i] <= maxs[i]))
    .then_some((mins, maxs))
}

fn visible_blast(frame: &FrameWorld, blast: &TriggerBlast, mid: [f32; 3], half: [f32; 3]) -> bool {
    let toward = glam::Vec3::from_array(std::array::from_fn(|i| blast.origin[i] - mid[i]))
        .normalize_or_zero();
    let right = glam::Vec3::new(-toward.y, toward.x, 0.0).normalize_or_zero();
    let up = toward.cross(right);
    let corner = glam::Vec3::from_array(half);
    let right = right * right.abs().dot(corner);
    let up = up * up.abs().dot(corner);
    let center = glam::Vec3::from_array(mid);
    [
        center,
        center + right + up,
        center - right + up,
        center + right - up,
        center - right - up,
    ]
    .into_iter()
    .any(|sample| {
        let sample = sample.to_array();
        if let Some((forward, cosine)) = blast.cone {
            let delta: [f32; 3] = std::array::from_fn(|i| sample[i] - blast.origin[i]);
            let length = math_iw4::vec3_length(delta);
            if length > 0.0 && (0..3).map(|i| delta[i] * forward[i]).sum::<f32>() < cosine * length
            {
                return false;
            }
        }
        matches!(
            frame.current_sensor_trace(crate::bullet_collision::BulletTraceQuery {
                start: blast.origin,
                end: sample,
                mask: gamemode_iw4::G_CAN_DAMAGE_CONTENTS_MASK,
                ignore: None,
                ignore_hit: None,
                ignore_model: blast.ignore_model,
            }),
            crate::bullet_collision::TraceOutcome::Miss { .. }
        )
    })
}

pub(crate) fn damage_blast(world: &mut World, blast: &TriggerBlast) {
    if blast.radius <= 0.0
        || !blast
            .origin
            .into_iter()
            .chain([blast.radius, blast.max, blast.min])
            .all(f32::is_finite)
        || world
            .get_resource::<Runtime>()
            .is_none_or(|runtime| runtime.program.is_none() || !runtime.started)
    {
        return;
    }
    super::presence::settle_collision(world);
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let activator = attacker(&runtime, blast.client, blast.missile);
    let mut raised = Vec::new();
    {
        let frame = FrameWorld::from_world(world);
        for trigger in active_damage_triggers(&runtime) {
            if !runtime.entities[&trigger].accepts_damage(crate::script_player::IDFLAGS_RADIUS) {
                continue;
            }
            let Some((mins, maxs)) = damage_bounds(&mut runtime, &frame, trigger) else {
                continue;
            };
            let mid = std::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5);
            let half = std::array::from_fn(|i| (maxs[i] - mins[i]) * 0.5);
            let distance = gamemode_iw4::radius_damage_distance_to_aabb(blast.origin, mid, half);
            if distance >= blast.radius || !visible_blast(&frame, blast, mid, half) {
                continue;
            }
            let amount = gamemode_iw4::radius_damage_amount(
                blast.max,
                blast.min,
                blast.radius,
                distance,
                1.0,
            );
            if amount <= 0 {
                continue;
            }
            let origin = match runtime.object_field(trigger, "origin") {
                Value::Vector(origin) => origin,
                _ => mid,
            };
            let hit = TriggerHit {
                activator: activator.clone(),
                amount,
                direction: normalized([
                    origin[0] - blast.origin[0],
                    origin[1] - blast.origin[1],
                    origin[2] - blast.origin[2] + 24.0,
                ]),
                point: blast.origin,
                means: blast.means,
            };
            damage_trigger(&mut runtime, trigger, &hit, &mut raised);
        }
    }
    *world.resource_mut::<Runtime>() = runtime;
    for (trigger, name, args) in raised {
        raise(world, Value::Object(trigger), name, args);
    }
}

pub(crate) fn dispatch_grenade_touches(world: &mut World) {
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let mut raised = Vec::new();
    {
        let frame = FrameWorld::from_world(world);
        for touch in std::mem::take(&mut runtime.grenade_touches) {
            let Some(activator) = runtime.missiles.get(&touch.projectile.id).copied() else {
                continue;
            };
            let triggers: Vec<u64> = runtime
                .entities
                .iter()
                .filter(|(object, entity)| {
                    entity.classname.as_ref() == "trigger_damage"
                        && entity.trigger_policy.grenade_touch
                        && !runtime.fired_once.contains(object)
                        && !runtime.pending_deletes.contains(object)
                })
                .map(|(object, _)| *object)
                .collect();
            for trigger in triggers {
                if !volume(&mut runtime, &frame, trigger)
                    .is_some_and(|volume| volume.crosses(touch.start, touch.end))
                {
                    continue;
                }
                let hit = TriggerHit {
                    activator: Value::Object(activator),
                    amount: touch.damage,
                    direction: normalized(std::array::from_fn(|i| touch.end[i] - touch.start[i])),
                    point: [0.0; 3],
                    means: "MOD_GRENADE",
                };
                damage_trigger(&mut runtime, trigger, &hit, &mut raised);
            }
        }
    }
    *world.resource_mut::<Runtime>() = runtime;
    for (trigger, name, args) in raised {
        raise(world, Value::Object(trigger), name, args);
    }
}

impl TriggerPolicy {
    pub(crate) fn allows(&self, frame: &FrameWorld, client: u32) -> bool {
        self.claimed_by.is_none_or(|owner| owner == client)
            && frame
                .client_meta(crate::ClientId(client))
                .is_some_and(|meta| {
                    meta.lifecycle == crate::ClientLifecycle::Alive
                        && self.team.is_none_or(|team| meta.client_state_team == team)
                })
    }
}

impl Runtime {
    pub(crate) fn release_trigger_claims(&mut self, client: u32) {
        for entity in self.entities.values_mut() {
            if entity.trigger_policy.claimed_by == Some(client) {
                entity.trigger_policy.claimed_by = None;
            }
        }
        self.use_selected.remove(&client);
    }
}

pub(crate) fn release_client_claims(world: &mut World, client: u32) {
    if let Some(mut runtime) = world.get_resource_mut::<Runtime>() {
        runtime.release_trigger_claims(client);
    }
}

pub(crate) fn refresh_claims(world: &mut World) {
    let claims: Vec<(u32, TriggerPolicy)> = world
        .resource::<Runtime>()
        .entities
        .values()
        .filter_map(|entity| {
            Some((
                entity.trigger_policy.claimed_by?,
                entity.trigger_policy.clone(),
            ))
        })
        .collect();
    for (client, policy) in claims {
        let connected = world.resource::<Runtime>().players.contains_key(&client);
        if !connected || !policy.allows(&FrameWorld::from_world(world), client) {
            release_client_claims(world, client);
        }
    }
}

fn set_grenade_touch(
    world: &mut World,
    receiver: &Value,
    args: &[Value],
    enabled: bool,
) -> Result<Value, String> {
    if !args.is_empty() {
        return Err("grenade touch damage controls take no arguments".into());
    }
    let object = super::natives::engine::entity_id(world, receiver)?;
    let mut runtime = world.resource_mut::<Runtime>();
    let entity = runtime.entities.get_mut(&object).unwrap();
    if entity.classname.as_ref() != "trigger_damage" {
        return Err("grenade touch damage requires a damage trigger".into());
    }
    entity.trigger_policy.grenade_touch = enabled;
    Ok(Value::Undefined)
}

pub(crate) fn register(registry: &mut crate::script::NativeRegistry) {
    use super::args::{arg, string};
    use super::natives::{engine::entity_id, player::player};
    use crate::script::Namespace::Method;

    registry.register(
        Method,
        "enablegrenadetouchdamage",
        |world, receiver, args| set_grenade_touch(world, receiver, args, true),
    );
    registry.register(
        Method,
        "disablegrenadetouchdamage",
        |world, receiver, args| set_grenade_touch(world, receiver, args, false),
    );

    registry.register(Method, "setteamfortrigger", |world, receiver, args| {
        let object = entity_id(world, receiver)?;
        let name = string(args, 0)?;
        let team = match &*name {
            "none" => None,
            "axis" => Some(entity_iw4::TEAM_AXIS),
            "allies" => Some(entity_iw4::TEAM_ALLIES),
            _ => return Err(format!("invalid trigger team '{name}'")),
        };
        world
            .resource_mut::<Runtime>()
            .entities
            .get_mut(&object)
            .unwrap()
            .trigger_policy
            .team = team;
        refresh_claims(world);
        Ok(Value::Undefined)
    });
    registry.register(Method, "clientclaimtrigger", |world, receiver, args| {
        let client = player(world, receiver)?;
        let object = entity_id(world, arg(args, 0)?)?;
        refresh_claims(world);
        let policy = world.resource::<Runtime>().entities[&object]
            .trigger_policy
            .clone();
        if !policy.allows(&FrameWorld::from_world(world), client) {
            return Err("trigger is unavailable to this player".into());
        }
        release_client_claims(world, client);
        world
            .resource_mut::<Runtime>()
            .entities
            .get_mut(&object)
            .unwrap()
            .trigger_policy
            .claimed_by = Some(client);
        Ok(Value::Undefined)
    });
    registry.register(Method, "clientreleasetrigger", |world, receiver, args| {
        let client = player(world, receiver)?;
        let object = entity_id(world, arg(args, 0)?)?;
        let mut runtime = world.resource_mut::<Runtime>();
        let policy = &mut runtime.entities.get_mut(&object).unwrap().trigger_policy;
        if policy.claimed_by == Some(client) {
            policy.claimed_by = None;
        }
        Ok(Value::Undefined)
    });
    registry.register(Method, "releaseclaimedtrigger", |world, receiver, _| {
        let object = entity_id(world, receiver)?;
        world
            .resource_mut::<Runtime>()
            .entities
            .get_mut(&object)
            .unwrap()
            .trigger_policy
            .claimed_by = None;
        Ok(Value::Undefined)
    });
}

fn eligible(runtime: &Runtime, frame: &FrameWorld, trigger: u64, client: u32, using: bool) -> bool {
    runtime.entities.get(&trigger).is_some_and(|entity| {
        entity.trigger_policy.allows(frame, client)
            && (!using
                || frame
                    .client_meta(crate::ClientId(client))
                    .is_some_and(|meta| !meta.controls.usability_disabled)
                    && entity
                        .usable
                        .as_ref()
                        .is_none_or(|usable| usable.enabled && !usable.barred.contains(&client)))
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fires {
    Touch,
    Once,
    Use,
}

fn fires(classname: &str) -> Option<Fires> {
    match classname {
        "trigger_multiple" | "trigger_radius" | "trigger_disk" => Some(Fires::Touch),
        "trigger_once" => Some(Fires::Once),
        "trigger_use" | "trigger_use_touch" => Some(Fires::Use),
        _ => None,
    }
}

/// A map trigger that fires on a Use press. Dispatch and the usable state
/// share this one rule.
pub(crate) fn is_use_trigger(classname: &str) -> bool {
    fires(classname) == Some(Fires::Use)
}

/// What one Use press acts on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PressTarget {
    /// The hint offered a dropped weapon: the item code takes the press.
    Item,
    /// The usable select_usables chose, whose hint the player saw.
    Usable(u64),
    /// Nothing was offered: the use triggers the player's hull touches.
    Touched,
}

/// What the one cursor hint offers, and so what a press made under it acts
/// on. `item` is a dropped weapon in reach, `usable` the usable
/// select_usables chose, and `in_hinted_trigger` whether that usable is a
/// map use trigger with a hint whose volume holds the player.
///
/// Such a trigger comes before the weapon. A player standing in a bomb site
/// or on a planted bomb is offered the plant or defuse, however many guns lie
/// there: in MW2 guns dropped on Search and Destroy and Sabotage sites are
/// routine and never stop a plant or a defuse, and pickup by Use works away
/// from the site. No MW2 script makes that choice, so this order is inferred
/// from MW2 gameplay, not read from code. Anywhere else the weapon comes
/// first, as the HUD always had it, and script model usables (care packages,
/// a sentry to pick up) keep that order.
pub(crate) fn use_offer(item: bool, usable: Option<u64>, in_hinted_trigger: bool) -> PressTarget {
    match usable {
        Some(usable) if in_hinted_trigger => PressTarget::Usable(usable),
        _ if item => PressTarget::Item,
        Some(usable) => PressTarget::Usable(usable),
        None => PressTarget::Touched,
    }
}

/// A player sees one cursor hint, and a press acts on that one thing.
/// _gameobjects.gsc useHoldThink relies on it: it keeps the weapon held at
/// the press as lastWeapon and switches back to it when the bar fills, so a
/// weapon swap on the same press leaves the player holding the empty
/// briefcase. The press follows the hint shown when it came (`cursor_hint`,
/// set last frame). select_usables chose that hint with use_offer, so a
/// weapon hint here means no use trigger outranked it.
fn press_target(cursor_hint: i32, selected: Option<u64>) -> PressTarget {
    if crate::item::hints_item(cursor_hint) {
        PressTarget::Item
    } else {
        selected.map_or(PressTarget::Touched, PressTarget::Usable)
    }
}

/// One convex piece of a map volume whose scripts hurt or kill a player who
/// stands in it, in world coordinates. A point is inside when it is on the
/// inner side of every plane (`[nx, ny, nz, d]` with `n·p <= d`); `mins` and
/// `maxs` bound it.
#[derive(Clone, Debug, PartialEq)]
pub struct HazardPiece {
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub planes: Vec<[f32; 4]>,
}

/// A map trigger whose scripts hurt or kill the players inside it. MW2's
/// _load.gsc suicides any player touching a trigger_hurt every half second,
/// _radiation.gsc poisons and then kills players in the "radiation" fields,
/// and _minefields.gsc blows up players in the "minefield" ones.
fn is_hazard(classname: &str, targetname: Option<&str>) -> bool {
    classname == "trigger_hurt" || matches!(targetname, Some("radiation" | "minefield"))
}

/// Every hazard volume in the level, as convex pieces. Read-only: the script
/// state is taken out and put back unchanged.
pub(crate) fn hazard_pieces(world: &mut World) -> Vec<HazardPiece> {
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let mut pieces = Vec::new();
    if runtime.program.is_some() {
        let frame = FrameWorld::from_world(world);
        let candidates: Vec<(u64, std::sync::Arc<str>)> = runtime
            .entities
            .iter()
            .map(|(object, entity)| (*object, entity.classname.clone()))
            .collect();
        for (object, classname) in candidates {
            let targetname = match runtime.object_field(object, "targetname") {
                Value::String(name) => Some(name),
                _ => None,
            };
            if !is_hazard(&classname, targetname.as_deref()) {
                continue;
            }
            if let Some(volume) = volume(&mut runtime, &frame, object) {
                pieces.extend(volume.pieces());
            }
        }
    }
    *world.resource_mut::<Runtime>() = runtime;
    pieces
}

fn volume<'f>(runtime: &mut Runtime, frame: &'f FrameWorld, object: u64) -> Option<Volume<'f>> {
    let entity = runtime.entities.get(&object)?;
    let (cylinder, brush, trigger_model) = (entity.cylinder, entity.brush, entity.trigger_model);
    let origin = match runtime.object_field(object, "origin") {
        Value::Vector(v) => v,
        _ => [0.0; 3],
    };
    if let Some((radius, height)) = cylinder {
        return Some(Volume::Cylinder {
            origin,
            radius,
            height,
        });
    }
    if let Some(n) = trigger_model {
        let hulls = frame.clip_cmodels().triggers.get(n as usize)?;
        return (!hulls.is_empty()).then_some(Volume::Hulls { origin, hulls });
    }
    let model = frame.clip_cmodels().models.get(brush? as usize)?;
    let first = model.first_brush as usize;
    let ids = frame
        .clip_bsp()
        .leafbrushes
        .get(first..first + usize::from(model.num_brushes))
        .unwrap_or(&[]);
    if !ids.is_empty() {
        return Some(Volume::Brushes {
            origin,
            ids,
            brushes: frame.clip_brushes(),
        });
    }
    Some(Volume::Box {
        mins: std::array::from_fn(|i| origin[i] + model.mins[i]),
        maxs: std::array::from_fn(|i| origin[i] + model.maxs[i]),
    })
}

#[derive(Clone, Copy, Debug)]
enum Volume<'f> {
    Cylinder {
        origin: [f32; 3],
        radius: f32,
        height: f32,
    },
    Box {
        mins: [f32; 3],
        maxs: [f32; 3],
    },
    Brushes {
        origin: [f32; 3],
        ids: &'f [u16],
        brushes: &'f [crate::SimBrush],
    },
    Hulls {
        origin: [f32; 3],
        hulls: &'f [crate::SimTriggerHull],
    },
}

/// The box a brush's axial planes cut, unbounded on an axis it has none for.
fn axial_bounds(planes: &[[f32; 4]]) -> ([f32; 3], [f32; 3]) {
    let mut mins = [f32::NEG_INFINITY; 3];
    let mut maxs = [f32::INFINITY; 3];
    for plane in planes {
        for axis in 0..3 {
            let off_axis = (0..3).all(|i| i == axis || plane[i] == 0.0);
            if !off_axis {
                continue;
            }
            if plane[axis] == 1.0 {
                maxs[axis] = maxs[axis].min(plane[3]);
            } else if plane[axis] == -1.0 {
                mins[axis] = mins[axis].max(-plane[3]);
            }
        }
    }
    (mins, maxs)
}

fn box_planes(mins: [f32; 3], maxs: [f32; 3]) -> impl Iterator<Item = [f32; 4]> {
    (0..3).flat_map(move |i| {
        let mut positive = [0.0; 4];
        let mut negative = [0.0; 4];
        positive[i] = 1.0;
        positive[3] = maxs[i];
        negative[i] = -1.0;
        negative[3] = -mins[i];
        [positive, negative]
    })
}

fn clip_plane(
    start: [f32; 3],
    end: [f32; 3],
    plane: [f32; 4],
    enter: &mut f32,
    exit: &mut f32,
) -> bool {
    if !plane
        .into_iter()
        .chain(start)
        .chain(end)
        .all(f32::is_finite)
    {
        return false;
    }
    let from = (0..3).map(|i| plane[i] * start[i]).sum::<f32>() - plane[3];
    let to = (0..3).map(|i| plane[i] * end[i]).sum::<f32>() - plane[3];
    if !from.is_finite() || !to.is_finite() || from > 0.0 && to > 0.0 {
        return false;
    }
    if from > 0.0 {
        *enter = enter.max(from / (from - to));
    } else if to > 0.0 {
        *exit = exit.min(from / (from - to));
    }
    *enter <= *exit
}

fn segment_planes(start: [f32; 3], end: [f32; 3], planes: impl Iterator<Item = [f32; 4]>) -> bool {
    let (mut enter, mut exit) = (0.0, 1.0);
    planes
        .into_iter()
        .all(|plane| clip_plane(start, end, plane, &mut enter, &mut exit))
}

impl Volume<'_> {
    /// The volume as convex pieces in world space. A cylinder becomes the
    /// octagonal prism around it, a little larger and never smaller.
    fn pieces(&self) -> Vec<HazardPiece> {
        let shift = |plane: [f32; 4], origin: [f32; 3]| {
            [
                plane[0],
                plane[1],
                plane[2],
                plane[3] + plane[0] * origin[0] + plane[1] * origin[1] + plane[2] * origin[2],
            ]
        };
        match *self {
            Volume::Box { mins, maxs } => vec![HazardPiece {
                mins,
                maxs,
                planes: box_planes(mins, maxs).collect(),
            }],
            Volume::Brushes {
                origin,
                ids,
                brushes,
            } => ids
                .iter()
                .filter_map(|id| brushes.get(usize::from(*id)))
                .filter(|brush| !brush.planes.is_empty())
                .map(|brush| {
                    let planes: Vec<[f32; 4]> =
                        brush.planes.iter().map(|p| shift(*p, origin)).collect();
                    let (mins, maxs) = axial_bounds(&planes);
                    HazardPiece { mins, maxs, planes }
                })
                .collect(),
            Volume::Hulls { origin, hulls } => hulls
                .iter()
                .map(|hull| {
                    let mins = std::array::from_fn(|i| origin[i] + hull.mid[i] - hull.half[i]);
                    let maxs = std::array::from_fn(|i| origin[i] + hull.mid[i] + hull.half[i]);
                    let mut planes: Vec<[f32; 4]> = box_planes(mins, maxs).collect();
                    for &(dir, at, width) in &hull.slabs {
                        planes.push(shift([dir[0], dir[1], dir[2], at + width], origin));
                        planes.push(shift([-dir[0], -dir[1], -dir[2], width - at], origin));
                    }
                    HazardPiece { mins, maxs, planes }
                })
                .collect(),
            Volume::Cylinder {
                origin,
                radius,
                height,
            } => {
                let mins = [origin[0] - radius, origin[1] - radius, origin[2]];
                let maxs = [origin[0] + radius, origin[1] + radius, origin[2] + height];
                let mut planes: Vec<[f32; 4]> = box_planes(mins, maxs).collect();
                let d = std::f32::consts::FRAC_1_SQRT_2;
                for (x, y) in [(d, d), (d, -d), (-d, d), (-d, -d)] {
                    planes.push(shift([x, y, 0.0, radius], origin));
                }
                vec![HazardPiece { mins, maxs, planes }]
            }
        }
    }

    fn crosses(&self, start: [f32; 3], end: [f32; 3]) -> bool {
        if !start.into_iter().chain(end).all(f32::is_finite) {
            return false;
        }
        match *self {
            Volume::Box { mins, maxs } => segment_planes(start, end, box_planes(mins, maxs)),
            Volume::Brushes {
                origin,
                ids,
                brushes,
            } => {
                let start = std::array::from_fn(|i| start[i] - origin[i]);
                let end = std::array::from_fn(|i| end[i] - origin[i]);
                ids.iter()
                    .filter_map(|id| brushes.get(usize::from(*id)))
                    .any(|brush| {
                        !brush.planes.is_empty()
                            && segment_planes(start, end, brush.planes.iter().copied())
                    })
            }
            Volume::Hulls { origin, hulls } => {
                let start = std::array::from_fn(|i| start[i] - origin[i]);
                let end = std::array::from_fn(|i| end[i] - origin[i]);
                hulls.iter().any(|hull| {
                    let mins = std::array::from_fn(|i| hull.mid[i] - hull.half[i]);
                    let maxs = std::array::from_fn(|i| hull.mid[i] + hull.half[i]);
                    let slabs = hull.slabs.iter().flat_map(|&(dir, at, width)| {
                        [
                            [dir[0], dir[1], dir[2], at + width],
                            [-dir[0], -dir[1], -dir[2], -at + width],
                        ]
                    });
                    segment_planes(start, end, box_planes(mins, maxs).chain(slabs))
                })
            }
            Volume::Cylinder {
                origin,
                radius,
                height,
            } => {
                if !origin
                    .into_iter()
                    .chain([radius, height])
                    .all(f32::is_finite)
                    || radius <= 0.0
                    || height <= 0.0
                {
                    return false;
                }
                let (mut enter, mut exit) = (0.0, 1.0);
                if !clip_plane(
                    start,
                    end,
                    [0.0, 0.0, 1.0, origin[2] + height],
                    &mut enter,
                    &mut exit,
                ) || !clip_plane(
                    start,
                    end,
                    [0.0, 0.0, -1.0, -origin[2]],
                    &mut enter,
                    &mut exit,
                ) {
                    return false;
                }
                let (x, y) = (start[0] - origin[0], start[1] - origin[1]);
                let (dx, dy) = (end[0] - start[0], end[1] - start[1]);
                let a = dx * dx + dy * dy;
                let b = x * dx + y * dy;
                let c = x * x + y * y - radius * radius;
                if a == 0.0 {
                    return c <= 0.0;
                }
                let discriminant = b * b - a * c;
                if discriminant < 0.0 || !discriminant.is_finite() {
                    return false;
                }
                let root = discriminant.sqrt();
                enter = enter.max((-b - root) / a);
                exit = exit.min((-b + root) / a);
                enter <= exit
            }
        }
    }

    fn touches(&self, mins: [f32; 3], maxs: [f32; 3]) -> bool {
        match *self {
            Volume::Brushes {
                origin,
                ids,
                brushes,
            } => {
                let mid: [f32; 3] = std::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5 - origin[i]);
                let half: [f32; 3] = std::array::from_fn(|i| (maxs[i] - mins[i]) * 0.5);
                ids.iter()
                    .filter_map(|&id| brushes.get(usize::from(id)))
                    .any(|brush| {
                        brush.planes.iter().all(|p| {
                            let reach =
                                p[0].abs() * half[0] + p[1].abs() * half[1] + p[2].abs() * half[2];
                            p[0] * mid[0] + p[1] * mid[1] + p[2] * mid[2] - p[3] <= reach
                        })
                    })
            }
            Volume::Cylinder {
                origin,
                radius,
                height,
            } => {
                let mid: [f32; 3] = std::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5);
                let half: [f32; 3] = std::array::from_fn(|i| (maxs[i] - mins[i]) * 0.5);
                let (dx, dy) = (origin[0] - mid[0], origin[1] - mid[1]);
                let reach = radius + half[0];
                (origin[2] + height * 0.5 - mid[2]).abs() < height * 0.5 + half[2]
                    && dx * dx + dy * dy < reach * reach
            }
            Volume::Box { mins: lo, maxs: hi } => {
                (0..3).all(|i| mins[i] <= hi[i] && maxs[i] >= lo[i])
            }
            Volume::Hulls { origin, hulls } => {
                let mid: [f32; 3] = std::array::from_fn(|i| (mins[i] + maxs[i]) * 0.5 - origin[i]);
                let half: [f32; 3] = std::array::from_fn(|i| (maxs[i] - mins[i]) * 0.5);
                hulls.iter().any(|hull| {
                    (0..3).all(|i| (mid[i] - hull.mid[i]).abs() <= half[i] + hull.half[i])
                        && hull.slabs.iter().all(|&(dir, at, width)| {
                            let reach: f32 = (0..3).map(|i| dir[i].abs() * half[i]).sum();
                            let along: f32 = (0..3).map(|i| dir[i] * mid[i]).sum();
                            (along - at).abs() <= width + reach
                        })
                })
            }
        }
    }
}

const LOOK_AT_REACH: f32 = 128.0;

fn looks_into(frame: &FrameWorld, client: u32, volume: &Volume<'_>) -> bool {
    let Some(ps) = frame.player(crate::ClientId(client)) else {
        return false;
    };
    let eye = [
        ps.origin[0],
        ps.origin[1],
        ps.origin[2] + ps.view_height_current,
    ];
    let (forward, _, _) = math_iw4::angle_vectors(ps.viewangles);
    (0..=16).any(|step| {
        let reach = LOOK_AT_REACH * step as f32 / 16.0;
        let at = std::array::from_fn(|i| eye[i] + forward[i] * reach);
        volume.touches(at, at)
    })
}

fn toucher(runtime: &mut Runtime, frame: &FrameWorld, object: u64) -> ([f32; 3], [f32; 3]) {
    if let Some(client) = runtime.player_client(object)
        && let Some(ps) = frame.player(crate::ClientId(client))
    {
        return (
            std::array::from_fn(|i| ps.origin[i] + PLAYER_MINS[i]),
            std::array::from_fn(|i| ps.origin[i] + PLAYER_MAXS[i]),
        );
    }
    let origin = match runtime.object_field(object, "origin") {
        Value::Vector(v) => v,
        _ => [0.0; 3],
    };
    (origin, origin)
}

/// Whether a player is where a trigger takes him: his hull touches its
/// volume, and he looks into it when the script asked for that.
fn reaches(
    runtime: &mut Runtime,
    frame: &FrameWorld,
    volume: &Volume<'_>,
    look_at: bool,
    client: u32,
    player: u64,
) -> bool {
    let (mins, maxs) = toucher(runtime, frame, player);
    volume.touches(mins, maxs) && (!look_at || looks_into(frame, client, volume))
}

/// Whether a trigger_use_touch takes this player's Use press. MW2 offers one
/// only to a player who touches it, so its hint and both press paths use the
/// test that fires it. `None` for every other usable (and for a trigger with
/// no volume), which a player reaches within the use radius.
pub(crate) fn touch_use_reach(
    runtime: &mut Runtime,
    frame: &FrameWorld,
    trigger: u64,
    client: u32,
    player: u64,
) -> Option<bool> {
    if runtime.entities.get(&trigger)?.classname.as_ref() != "trigger_use_touch" {
        return None;
    }
    let volume = volume(runtime, frame, trigger)?;
    let look_at = runtime.require_look_at.contains(&trigger);
    Some(reaches(runtime, frame, &volume, look_at, client, player))
}

/// Whether a map use trigger (trigger_use or trigger_use_touch) holds this
/// player: his hull touches its volume, and he looks into it when the script
/// asked for that. False for every other entity.
pub(crate) fn in_use_trigger(
    runtime: &mut Runtime,
    frame: &FrameWorld,
    trigger: u64,
    client: u32,
    player: u64,
) -> bool {
    if !runtime
        .entities
        .get(&trigger)
        .is_some_and(|entity| is_use_trigger(&entity.classname))
    {
        return false;
    }
    let Some(volume) = volume(runtime, frame, trigger) else {
        return false;
    };
    let look_at = runtime.require_look_at.contains(&trigger);
    reaches(runtime, frame, &volume, look_at, client, player)
}

pub(crate) fn entity_bounds(world: &mut World, object: u64) -> ([f32; 3], [f32; 3]) {
    let runtime = world.resource::<Runtime>();
    if runtime.player_client(object).is_some() {
        return (PLAYER_MINS, PLAYER_MAXS);
    }
    let Some(entity) = runtime.entities.get(&object) else {
        return ([0.0; 3], [0.0; 3]);
    };
    if let Some((radius, height)) = entity.cylinder {
        return ([-radius, -radius, 0.0], [radius, radius, height]);
    }
    let brush = entity.brush;
    let frame = FrameWorld::from_world(world);
    brush
        .and_then(|n| frame.clip_cmodels().models.get(n as usize))
        .map_or(([0.0; 3], [0.0; 3]), |model| (model.mins, model.maxs))
}

pub(crate) fn is_touching(world: &mut World, a: u64, b: u64) -> bool {
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let touching = {
        let frame = FrameWorld::from_world(world);
        [(a, b), (b, a)].into_iter().find_map(|(trigger, other)| {
            let volume = volume(&mut runtime, &frame, trigger)?;
            let (mins, maxs) = toucher(&mut runtime, &frame, other);
            Some(volume.touches(mins, maxs))
        })
    };
    *world.resource_mut::<Runtime>() = runtime;
    touching.unwrap_or(false)
}

pub(crate) fn dispatch_triggers(world: &mut World) {
    refresh_claims(world);
    let mut runtime = std::mem::take(&mut *world.resource_mut::<Runtime>());
    let mut raised = Vec::new();
    {
        let mut frame = FrameWorld::from_world(world);
        let mut players: Vec<(u32, u64, bool)> = Vec::new();
        for (client, slot) in &runtime.players {
            let id = crate::ClientId(*client);
            if !frame
                .client_meta(id)
                .is_some_and(|m| m.lifecycle == crate::ClientLifecycle::Alive)
            {
                continue;
            }
            let held = crate::script_player::buttons(&mut frame, id)
                & (playerstate_iw4::buttons::USE | playerstate_iw4::buttons::USE_RELOAD)
                != 0;
            players.push((*client, slot.object, held));
        }
        let pressed: Vec<(u32, u64)> = players
            .iter()
            .filter(|(client, _, held)| *held && !runtime.use_held.contains(client))
            .map(|(client, object, _)| (*client, *object))
            .collect();
        runtime.use_held = players
            .iter()
            .filter(|(_, _, held)| *held)
            .map(|(client, _, _)| *client)
            .collect();
        let mut touch_pressed = Vec::new();
        for (client, player) in &pressed {
            let hint = frame
                .player(crate::ClientId(*client))
                .map_or(0, |ps| ps.cursor_hint);
            match press_target(hint, runtime.use_selected.get(client).copied()) {
                PressTarget::Item => {}
                PressTarget::Usable(usable) => {
                    if eligible(&runtime, &frame, usable, *client, true)
                        && touch_use_reach(&mut runtime, &frame, usable, *client, *player)
                            != Some(false)
                    {
                        raised.push((usable, *player));
                    }
                }
                PressTarget::Touched => touch_pressed.push((*client, *player)),
            }
        }
        let triggers: Vec<(u64, Fires)> = runtime
            .entities
            .iter()
            .filter(|(object, _)| !runtime.fired_once.contains(object))
            .filter_map(|(object, e)| Some((*object, fires(&e.classname)?)))
            .collect();
        for (trigger, kind) in triggers {
            let Some(volume) = volume(&mut runtime, &frame, trigger) else {
                continue;
            };
            let candidates: Vec<(u32, u64)> = match kind {
                Fires::Use => touch_pressed.clone(),
                Fires::Touch | Fires::Once => players
                    .iter()
                    .map(|(client, object, _)| (*client, *object))
                    .collect(),
            };
            let look_at = kind == Fires::Use && runtime.require_look_at.contains(&trigger);
            for (client, player) in candidates {
                if !eligible(&runtime, &frame, trigger, client, kind == Fires::Use) {
                    continue;
                }
                if !reaches(&mut runtime, &frame, &volume, look_at, client, player) {
                    continue;
                }
                raised.push((trigger, player));
                if kind == Fires::Once {
                    runtime.fired_once.insert(trigger);
                    break;
                }
            }
        }
    }
    *world.resource_mut::<Runtime>() = runtime;
    raised.sort_unstable();
    raised.dedup();
    for (trigger, player) in raised {
        raise(
            world,
            Value::Object(trigger),
            "trigger",
            vec![Value::Object(player)],
        );
    }
}

#[cfg(test)]
mod press_tests {
    use super::*;

    #[test]
    fn a_press_goes_to_the_one_thing_hinted() {
        let site = 42;
        let weapon_hint = 186 + 4;
        assert_eq!(press_target(weapon_hint, Some(site)), PressTarget::Item);
        assert_eq!(press_target(weapon_hint, None), PressTarget::Item);
        let hint_activate = 2;
        assert_eq!(
            press_target(hint_activate, Some(site)),
            PressTarget::Usable(site)
        );
        let hint_none = 0;
        assert_eq!(
            press_target(hint_none, Some(site)),
            PressTarget::Usable(site)
        );
        assert_eq!(press_target(hint_none, None), PressTarget::Touched);
    }

    #[test]
    fn a_site_the_player_stands_in_outranks_a_gun() {
        let (site, crate_model) = (42, 77);
        let gun = true;
        assert_eq!(use_offer(gun, Some(site), true), PressTarget::Usable(site));
        assert_eq!(
            use_offer(false, Some(site), true),
            PressTarget::Usable(site)
        );
        assert_eq!(use_offer(gun, Some(crate_model), false), PressTarget::Item);
        assert_eq!(use_offer(gun, None, false), PressTarget::Item);
        assert_eq!(
            use_offer(false, Some(crate_model), false),
            PressTarget::Usable(crate_model)
        );
        assert_eq!(use_offer(false, None, false), PressTarget::Touched);
    }

    #[test]
    fn script_hints_are_not_items() {
        for cursor in 0..=4 {
            assert!(!crate::item::hints_item(cursor), "{cursor}");
        }
        assert!(crate::item::hints_item(5));
    }
}

#[cfg(test)]
mod hazard_tests {
    use super::*;

    #[test]
    fn hazard_triggers_are_hurt_radiation_and_minefields() {
        assert!(is_hazard("trigger_hurt", None));
        assert!(is_hazard("trigger_multiple", Some("radiation")));
        assert!(is_hazard("trigger_multiple", Some("minefield")));
        assert!(!is_hazard("trigger_multiple", Some("ctf_zone_allies")));
        assert!(!is_hazard("trigger_use_touch", None));
    }

    fn inside(piece: &HazardPiece, p: [f32; 3]) -> bool {
        piece
            .planes
            .iter()
            .all(|n| n[0] * p[0] + n[1] * p[1] + n[2] * p[2] <= n[3] + 1e-3)
    }

    #[test]
    fn a_moved_brush_becomes_a_world_space_piece() {
        let brush = crate::SimBrush {
            planes: box_planes([-10.0, -20.0, 0.0], [10.0, 20.0, 30.0])
                .chain([[
                    std::f32::consts::FRAC_1_SQRT_2,
                    std::f32::consts::FRAC_1_SQRT_2,
                    0.0,
                    0.0,
                ]])
                .collect(),
            contents: 0,
            plane_surface_flags: Vec::new(),
            glass_encoded: 0,
        };
        let brushes = [brush];
        let volume = Volume::Brushes {
            origin: [100.0, 0.0, 0.0],
            ids: &[0],
            brushes: &brushes,
        };
        let pieces = volume.pieces();
        assert_eq!(pieces.len(), 1);
        let piece = &pieces[0];
        assert_eq!(
            (piece.mins, piece.maxs),
            ([90.0, -20.0, 0.0], [110.0, 20.0, 30.0])
        );
        assert!(inside(piece, [95.0, -5.0, 10.0]));
        // The slanted side cuts the corner off, moved with the brush.
        assert!(!inside(piece, [105.0, 15.0, 10.0]));
        assert!(!inside(piece, [5.0, -5.0, 10.0]));
    }

    #[test]
    fn a_cylinder_piece_holds_the_whole_cylinder() {
        let volume = Volume::Cylinder {
            origin: [0.0, 0.0, 10.0],
            radius: 64.0,
            height: 50.0,
        };
        let piece = &volume.pieces()[0];
        for step in 0..16 {
            let a = step as f32 * std::f32::consts::TAU / 16.0;
            assert!(inside(piece, [63.9 * a.cos(), 63.9 * a.sin(), 20.0]), "{a}");
        }
        assert!(!inside(piece, [60.0, 60.0, 20.0]));
        assert!(!inside(piece, [0.0, 0.0, 61.0]));
    }
}
