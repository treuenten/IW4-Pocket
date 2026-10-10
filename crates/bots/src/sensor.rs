use math_iw4::angle_vectors;
use sim::{ClientId, ClientLifecycle, Snapshot};

use crate::observation::{
    BotObservation, Contact, DEFAULT_OBJECTIVE_RADIUS, KnowledgeSource, ModeObjective,
    ObjectiveAction, SelfState, TeamRole, Visibility, WeaponAction, WeaponSlot,
};
use crate::query::{QuerySubsystem, SightSample, WorldQuery};

pub const SIGHT_RANGE_IN: f32 = 5906.0;
pub const SIGHT_FOV_DEG: f32 = 90.0;
const PROBE_CHEST_Z: f32 = 48.0;
const PROBE_HEAD_Z: f32 = 64.0;
const PROBE_FEET_Z: f32 = 8.0;
/// Perception's share of the shared quota, so routing and firing keep theirs.
const SIGHT_PROBES: u32 = 6;

pub fn observe(
    snapshot: &Snapshot,
    bot: ClientId,
    world: &mut impl WorldQuery,
) -> Option<BotObservation> {
    observe_focused(snapshot, bot, world, None)
}

/// `focus` is the client the bot has already committed to. It is probed before
/// the rotation spends the same allowance on acquisition candidates, so a
/// committed target is the last thing perception gives up, not the first.
pub fn observe_focused(
    snapshot: &Snapshot,
    bot: ClientId,
    world: &mut impl WorldQuery,
    focus: Option<ClientId>,
) -> Option<BotObservation> {
    let meta = snapshot.meta.for_client(bot)?;
    let ps = snapshot
        .players
        .iter()
        .find(|(id, _)| *id == bot)
        .map(|(_, ps)| ps)?;
    let team = meta.client_state_team;
    world.enter(QuerySubsystem::Perception);
    let inventory = own_inventory(ps, meta, world);
    let akimbo = ps.last_weapon_hand >= 1;
    let ammo_clip_left = inventory
        .iter()
        .find(|slot| slot.weapon == ps.weapon as u16)
        .filter(|_| akimbo)
        .map_or(0, |slot| slot.clip_left);
    let self_state = SelfState {
        life_sequence: meta.life_sequence,
        id: bot,
        lifecycle: meta.lifecycle,
        origin: ps.origin,
        viewangles: ps.viewangles,
        delta_angles: ps.delta_angles,
        view_height: ps.view_height_current,
        stance: crate::observation::Stance::from_view_height(ps.view_height_current),
        health: ps.health,
        weapon: ps.weapon as u16,
        weapon_class: world.weapon_class(ps.weapon as u16),
        weapon_action: WeaponAction::from_weaponstate(ps.weaponstate_primary),
        ammo_clip: meta.ammo_clip,
        ammo_stock: meta.ammo_stock,
        team,
        linked: meta.controls.linked,
        akimbo,
        ammo_clip_left,
    };
    let mut seen = Vec::new();
    let mut unsensed = Vec::new();
    if meta.lifecycle != ClientLifecycle::Alive {
        // A bot that is not alive performs no probe, so it makes no negative
        // observation either.
        unsensed.extend(
            snapshot
                .players
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| *id != bot),
        );
    } else {
        let eye = [
            ps.origin[0],
            ps.origin[1],
            ps.origin[2]
                + if ps.view_height_current > 1.0 {
                    ps.view_height_current
                } else {
                    60.0
                },
        ];
        let (forward, _, _) = angle_vectors(ps.viewangles);
        let half = (SIGHT_FOV_DEG * 0.5).to_radians().cos();
        // Rotate probes so occluded clients at the front of the snapshot cannot
        // monopolize perception.
        let mut probes = SIGHT_PROBES;
        let count = snapshot.players.len();
        let start = (snapshot.tick.0 as usize / 2 + bot.0 as usize) % count.max(1);
        for index in probe_order(snapshot, start, count, focus) {
            let (id, other) = &snapshot.players[index];
            if *id == bot {
                continue;
            }
            let Some(other_meta) = snapshot.meta.for_client(*id) else {
                continue;
            };
            if other_meta.lifecycle != ClientLifecycle::Alive {
                continue;
            }
            if snapshot.meta.kind.is_team() && other_meta.client_state_team == team {
                continue;
            }
            // Out of range or outside the cone is a complete answer on its own;
            // only a client that needed a probe it did not get is unsensed.
            if !in_range_and_fov(ps.origin, other.origin, forward, half) {
                continue;
            }
            if probes == 0 {
                unsensed.push(*id);
                continue;
            }
            match probe_visible(world, bot, eye, other.origin, *id, &mut probes) {
                Visibility::Seen => seen.push(Contact {
                    id: *id,
                    origin: other.origin,
                    source: KnowledgeSource::CurrentlySeen,
                    seen_tick: snapshot.tick.0,
                    confidence: 1.0,
                    health: other.health,
                }),
                Visibility::NotSeen => {}
                Visibility::Unknown => unsensed.push(*id),
            }
        }
    }
    let held_drop_ms = inventory
        .iter()
        .find(|slot| slot.weapon == self_state.weapon)
        .map_or(0, |slot| slot.facts.drop_time_ms);
    let objectives = public_objectives(snapshot, bot, team, bomb_use_ms(held_drop_ms));
    Some(BotObservation {
        tick: snapshot.tick.0,
        time_ms: snapshot.tick.0 as i32 * 50,
        self_state,
        inventory,
        seen,
        unsensed,
        events: Vec::new(),
        objective: objectives
            .iter()
            .copied()
            .min_by(|a, b| dist2(ps.origin, a.origin).total_cmp(&dist2(ps.origin, b.origin))),
        objectives,
    })
}

fn in_range_and_fov(from: [f32; 3], to: [f32; 3], forward: [f32; 3], min_dot: f32) -> bool {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    if len2 > SIGHT_RANGE_IN * SIGHT_RANGE_IN || len2 <= 0.0001 {
        return false;
    }
    let len = len2.sqrt();
    let dir = [d[0] / len, d[1] / len, d[2] / len];
    dir[0] * forward[0] + dir[1] * forward[1] + dir[2] * forward[2] >= min_dot
}

/// The bot's own carried weapons with the ammunition the match state holds for
/// them. Enemy inventories and positions are never read here.
fn own_inventory(
    ps: &playerstate_iw4::PlayerState,
    meta: &sim::ClientSnapshotMeta,
    world: &impl WorldQuery,
) -> Vec<WeaponSlot> {
    ps.weapons
        .iter()
        .copied()
        .filter(|&id| id > 0)
        .filter_map(|id| {
            let weapon = u16::try_from(id).ok()?;
            let facts = world.weapon_facts(weapon)?;
            let (clip, stock) = meta
                .ammo_by_weapon
                .iter()
                .find(|(owned, _, _)| *owned == id as u32)
                .map_or((0, 0), |(_, clip, stock)| (*clip, *stock));
            let akimbo =
                weapon_iw4::num_hands_for_held(&ps.weapons, &ps.weapon_data, id as u32) >= 1;
            Some(WeaponSlot {
                weapon,
                class: world.weapon_class(weapon),
                facts,
                clip,
                stock,
                akimbo,
                clip_left: if akimbo { left_clip(ps, &facts) } else { 0 },
            })
        })
        .collect()
}

/// The left-hand magazine of an akimbo pair. The match state's ammunition
/// list carries the right hand only; the clip table holds both.
fn left_clip(ps: &playerstate_iw4::PlayerState, facts: &crate::observation::WeaponFacts) -> i32 {
    weapon_iw4::get_clip_for_hand(&ps.ammoclip, facts.clip_index, 1).max(0)
}

/// Snapshot indices in the order perception spends its allowance: the committed
/// target first, then the tick's rotation.
fn probe_order(
    snapshot: &Snapshot,
    start: usize,
    count: usize,
    focus: Option<ClientId>,
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).map(|offset| (start + offset) % count).collect();
    if let Some(focus) = focus
        && let Some(at) = order
            .iter()
            .position(|index| snapshot.players[*index].0 == focus)
    {
        let first = order.remove(at);
        order.insert(0, first);
    }
    order
}

fn probe_visible(
    world: &mut impl WorldQuery,
    bot: ClientId,
    eye: [f32; 3],
    origin: [f32; 3],
    target: ClientId,
    remaining: &mut u32,
) -> Visibility {
    let probes = [
        [origin[0], origin[1], origin[2] + PROBE_CHEST_Z],
        [origin[0], origin[1], origin[2] + PROBE_HEAD_Z],
        [origin[0], origin[1], origin[2] + PROBE_FEET_Z],
    ];
    // One positive probe settles the question; a negative one settles it only
    // when every probe actually ran and came back classified.
    let mut complete = true;
    for probe in probes {
        if *remaining == 0 {
            return Visibility::Unknown;
        }
        *remaining -= 1;
        match world.sight_ray(eye, probe, bot) {
            Ok(SightSample::Clear) => return Visibility::Seen,
            Ok(SightSample::HitPlayer { client, .. }) if client == target => {
                return Visibility::Seen;
            }
            Err(_) => {
                // The shared quota is gone; stop attributing denials to this bot.
                *remaining = 0;
                return Visibility::Unknown;
            }
            Ok(SightSample::Unknown) => complete = false,
            Ok(_) => {}
        }
    }
    if complete {
        Visibility::NotSeen
    } else {
        Visibility::Unknown
    }
}

fn public_objectives(
    snapshot: &Snapshot,
    bot: ClientId,
    team: i32,
    use_ms: u32,
) -> Vec<ModeObjective> {
    let Some(side) = gamemode_iw4::Team::from_packed_u8(team as u8) else {
        return Vec::new();
    };
    let Some(feet) = snapshot
        .players
        .iter()
        .find(|(id, _)| *id == bot)
        .map(|(_, p)| p.origin)
    else {
        return Vec::new();
    };
    let state = &snapshot.meta.objectives;
    let markers: Vec<Marker> = state
        .compass
        .iter()
        .filter(|o| o.shows_to(side, bot.0))
        .map(|o| Marker {
            index: o.index,
            icon: &o.icon,
            origin: o.origin,
        })
        .collect();
    let mates: Vec<Mate> = snapshot
        .players
        .iter()
        .filter_map(|(id, p)| {
            let meta = snapshot.meta.for_client(*id)?;
            (meta.lifecycle == ClientLifecycle::Alive && meta.client_state_team == team).then_some(
                Mate {
                    id: *id,
                    origin: p.origin,
                    linked: meta.controls.linked,
                },
            )
        })
        .collect();
    // sd.gsc and sab.gsc run the bomb fuse on the game end time
    // (setGameEndTime(gettime() + bombTimer)); before a plant it is the round clock.
    let now_ms = snapshot.tick.0.saturating_mul(sim::MATCH_TICK_MS) as i32;
    let remaining_ms = (state.game_end_time > 0).then(|| state.time_left_ms(now_ms).max(0) as u32);
    objectives_from_compass(&markers, bot, feet, &mates, remaining_ms, use_ms)
}

/// A compass marker the bot's team is shown: the 2D icon the gametype gave
/// the objective with `_gameobjects::set2DIcon`, the same one a human reads off
/// the map.
struct Marker<'a> {
    index: u8,
    icon: &'a str,
    origin: [f32; 3],
}

/// A live teammate, the bot included.
struct Mate {
    id: ClientId,
    origin: [f32; 3],
    /// Held by a script `playerLinkTo`, as `_gameobjects::useHoldThink` does
    /// for the whole of a plant or a defuse.
    linked: bool,
}

/// What a marker asks of the team that sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IconRole {
    /// `waypoint_defend*` and compass pings: nowhere to walk to.
    Ignore,
    /// `waypoint_escort`: rides on the teammate carrying the bomb or the flag
    /// (`objective_onentity`), so it is never a place to stand.
    Escort,
    /// `waypoint_target*`: a site to plant the carried bomb at.
    Target,
    /// `waypoint_defuse*`: a planted bomb to defuse.
    Defuse,
    /// Anything else: a loose bomb, a flag, a zone held by standing in it.
    Capture,
}

fn icon_role(icon: &str) -> IconRole {
    if icon.starts_with("compassping") || icon.contains("defend") {
        IconRole::Ignore
    } else if icon.contains("escort") {
        IconRole::Escort
    } else if icon.contains("target") {
        IconRole::Target
    } else if icon.contains("defuse") {
        IconRole::Defuse
    } else {
        IconRole::Capture
    }
}

/// MW2's match config (default_xboxlive.cfg) sets defusetime to 5 seconds in
/// sd, sab and dd, as the scripts' own defaults do. planttime is 5 seconds too
/// except in Sabotage (2.5), but only a defuse is ever checked against the fuse.
const BOMB_USE_MS: u32 = 5_000;
/// The frames a plant or a defuse spends past the use time and the putaway,
/// at most one each: the press reaching the script, the putaway ending on a
/// frame boundary, and useHoldThinkLoop's first pass with the briefcase up,
/// which adds no progress because useRate is still 0.
const BOMB_USE_FRAMES_MS: u32 = 3 * sim::MATCH_TICK_MS;

/// How long a plant or a defuse takes from the press, holding a weapon with
/// this dropTime. `_gameobjects::useHoldThinkLoop` counts curProgress only
/// while getCurrentWeapon() is the briefcase, and that holds once the weapon
/// in hand has been put away and the briefcase starts to come up. So the use
/// time runs after the held weapon's putaway, not from the press.
fn bomb_use_ms(held_drop_ms: i32) -> u32 {
    BOMB_USE_MS + held_drop_ms.max(0) as u32 + BOMB_USE_FRAMES_MS
}

/// How far the escort marker may sit from the carrier it rides on. Player
/// hulls cannot overlap, so the nearest teammate this close is the carrier.
const CARRIER_XY: f32 = 32.0;
/// A use objective counts as reached on the same floor only.
const USE_TOUCH_Z: f32 = 72.0;

/// Bomb objectives follow MW2's own S&D rules: only the carrier plants
/// (sd.gsc bombZone `setKeyObject(level.sdBomb)`), the bomb has to be picked
/// up before any site matters, and one player at a time holds a plant or a
/// defuse (`_gameobjects::useObjectUseThink` claims the trigger). So one bot
/// is the Actor that presses Use and the rest of the team covers it.
fn objectives_from_compass(
    markers: &[Marker],
    me: ClientId,
    feet: [f32; 3],
    mates: &[Mate],
    remaining_ms: Option<u32>,
    use_ms: u32,
) -> Vec<ModeObjective> {
    let carrier = markers
        .iter()
        .filter(|m| icon_role(m.icon) == IconRole::Escort)
        .find_map(|m| {
            mates
                .iter()
                .filter(|mate| {
                    dist2_xy(mate.origin, m.origin) <= CARRIER_XY * CARRIER_XY
                        && (mate.origin[2] - m.origin[2]).abs() <= USE_TOUCH_Z
                })
                .min_by(|a, b| {
                    dist2_xy(a.origin, m.origin)
                        .total_cmp(&dist2_xy(b.origin, m.origin))
                        .then_with(|| a.id.0.cmp(&b.id.0))
                })
                .map(|mate| mate.id)
        });
    let bomb_loose = markers
        .iter()
        .any(|m| icon_role(m.icon) == IconRole::Capture && m.icon.contains("bomb"));
    markers
        .iter()
        .filter_map(|m| {
            let base = ModeObjective {
                id: u32::from(m.index),
                ..ModeObjective::at(m.origin)
            };
            let claimant = || {
                mates
                    .iter()
                    .find(|mate| mate.linked && use_touching(mate.origin, m.origin))
                    .map(|mate| mate.id)
            };
            let bomb_use = |action, actor: Option<ClientId>| ModeObjective {
                action,
                role: if actor == Some(me) {
                    TeamRole::Actor
                } else {
                    TeamRole::Cover
                },
                active_user: claimant(),
                remaining_ms,
                interaction_ms: use_ms,
                touching: use_touching(feet, m.origin),
                use_button: actor == Some(me),
                ..base
            };
            match icon_role(m.icon) {
                IconRole::Ignore | IconRole::Escort => None,
                IconRole::Target if bomb_loose => None,
                IconRole::Target => Some(bomb_use(ObjectiveAction::Plant, carrier)),
                IconRole::Defuse => {
                    let actor = claimant().or_else(|| {
                        mates
                            .iter()
                            .min_by(|a, b| {
                                dist2(a.origin, m.origin)
                                    .total_cmp(&dist2(b.origin, m.origin))
                                    .then_with(|| a.id.0.cmp(&b.id.0))
                            })
                            .map(|mate| mate.id)
                    });
                    Some(bomb_use(ObjectiveAction::Defuse, actor))
                }
                IconRole::Capture => Some(ModeObjective {
                    touching: dist2_xy(feet, m.origin)
                        <= DEFAULT_OBJECTIVE_RADIUS * DEFAULT_OBJECTIVE_RADIUS,
                    ..base
                }),
            }
        })
        .collect()
}

/// Within the objective radius across and on the same floor. The trigger
/// brush itself can start a little further in, so the controller keeps closing
/// in and pressing Use until the script links the player.
fn use_touching(feet: [f32; 3], origin: [f32; 3]) -> bool {
    dist2_xy(feet, origin) <= DEFAULT_OBJECTIVE_RADIUS * DEFAULT_OBJECTIVE_RADIUS
        && (feet[2] - origin[2]).abs() <= USE_TOUCH_Z
}

fn dist2_xy(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    const SITE_A: [f32; 3] = [2000.0, 0.0, 0.0];
    const SITE_B: [f32; 3] = [0.0, 2000.0, 0.0];

    fn marker(index: u8, icon: &str, origin: [f32; 3]) -> Marker<'_> {
        Marker {
            index,
            icon,
            origin,
        }
    }

    fn mate(id: u32, origin: [f32; 3], linked: bool) -> Mate {
        Mate {
            id: ClientId(id),
            origin,
            linked,
        }
    }

    fn goals(markers: &[Marker], me: u32, mates: &[Mate]) -> Vec<ModeObjective> {
        let feet = mates.iter().find(|m| m.id.0 == me).unwrap().origin;
        objectives_from_compass(
            markers,
            ClientId(me),
            feet,
            mates,
            Some(30_000),
            bomb_use_ms(HELD_DROP_MS),
        )
    }

    const HELD_DROP_MS: i32 = 500;

    #[test]
    fn a_bomb_use_runs_from_the_putaway() {
        assert_eq!(bomb_use_ms(HELD_DROP_MS), 5_650);
        assert_eq!(bomb_use_ms(0), BOMB_USE_MS + BOMB_USE_FRAMES_MS);
        assert_eq!(bomb_use_ms(-1), bomb_use_ms(0));
    }

    #[test]
    fn mw2_icons_map_to_roles() {
        for (icon, role) in [
            ("waypoint_defend_a", IconRole::Ignore),
            ("waypoint_defend_flag", IconRole::Ignore),
            ("compassping_enemy", IconRole::Ignore),
            ("waypoint_escort", IconRole::Escort),
            ("waypoint_target_b", IconRole::Target),
            ("waypoint_defuse_a", IconRole::Defuse),
            ("waypoint_bomb", IconRole::Capture),
            ("waypoint_captureneutral_a", IconRole::Capture),
            ("waypoint_kill", IconRole::Capture),
        ] {
            assert_eq!(icon_role(icon), role, "{icon}");
        }
    }

    #[test]
    fn the_carrier_plants_and_his_team_covers_him() {
        let carrier = [-2675.0, 1130.0, 7.0];
        let markers = [
            marker(
                0,
                "waypoint_escort",
                [carrier[0] + 6.0, carrier[1], carrier[2]],
            ),
            marker(1, "waypoint_target_a", SITE_A),
            marker(2, "waypoint_target_b", SITE_B),
        ];
        let mates = [
            mate(3, carrier, false),
            mate(4, [-2620.0, 1150.0, 0.0], false),
        ];
        let own = goals(&markers, 3, &mates);
        assert_eq!(own.len(), 2, "{own:?}");
        assert!(own.iter().all(|g| g.action == ObjectiveAction::Plant
            && g.role == TeamRole::Actor
            && g.use_button
            && g.interaction_ms == bomb_use_ms(HELD_DROP_MS)));
        let other = goals(&markers, 4, &mates);
        assert_eq!(other.len(), 2, "{other:?}");
        assert!(other.iter().all(|g| g.action == ObjectiveAction::Plant
            && g.role == TeamRole::Cover
            && !g.use_button));
    }

    #[test]
    fn a_loose_bomb_comes_before_the_sites() {
        let markers = [
            marker(0, "waypoint_bomb", [-2664.0, 1128.0, 6.0]),
            marker(1, "waypoint_target_a", SITE_A),
            marker(2, "waypoint_target_b", SITE_B),
        ];
        let own = goals(&markers, 3, &[mate(3, [-3000.0, 1400.0, 20.0], false)]);
        assert_eq!(own.len(), 1, "{own:?}");
        assert_eq!((own[0].id, own[0].action), (0, ObjectiveAction::Capture));
    }

    #[test]
    fn a_site_on_another_floor_is_not_touching() {
        let me = [10.0, 0.0, 0.0];
        let markers = [
            marker(0, "waypoint_escort", me),
            marker(1, "waypoint_target_a", [0.0, 0.0, 200.0]),
        ];
        let own = goals(&markers, 3, &[mate(3, me, false)]);
        assert!(own[0].use_button && !own[0].touching, "{own:?}");
    }

    #[test]
    fn one_defender_defuses_and_the_rest_cover() {
        let markers = [
            marker(3, "waypoint_defend_b", SITE_B),
            marker(4, "waypoint_defuse_a", SITE_A),
        ];
        let near = mate(5, [1700.0, 0.0, 0.0], false);
        let far = mate(6, [1000.0, 0.0, 0.0], false);
        let mates = [near, far];
        let actor = goals(&markers, 5, &mates);
        assert_eq!(actor.len(), 1, "{actor:?}");
        assert_eq!(actor[0].action, ObjectiveAction::Defuse);
        assert!(actor[0].use_button && actor[0].role == TeamRole::Actor);
        let cover = goals(&markers, 6, &mates);
        assert!(!cover[0].use_button && cover[0].role == TeamRole::Cover);
        // A teammate already defusing keeps the claim however near the others are.
        let claimant = mate(7, [1920.0, 30.0, 0.0], true);
        let mates = [mate(5, [1990.0, 0.0, 0.0], false), claimant];
        let own = goals(&markers, 5, &mates);
        assert!(!own[0].use_button && own[0].active_user == Some(ClientId(7)));
    }
}
