use sim::{ClientLifecycle, LifeSequence};

use crate::observation::{BotObservation, WeaponAction, WeaponSlot};

/// Ticks a failed decision waits before the same state may commit again. Without
/// it an impossible reload and an impossible switch can alternate every think.
const RETRY_TICKS: u32 = 20;
/// Slack over the simulator's own timing before a commitment is abandoned.
const DEADLINE_SLACK_TICKS: u32 = 8;
const TICK_MS: i32 = 50;
/// Used when the content has no timings for the weapon in hand. Reloading is a
/// legal command either way; only the comparison needs real numbers.
const DEFAULT_ACTION_MS: i32 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponAct {
    Switch { to: u16 },
    Reload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeaponFailure {
    /// The bot no longer carries the weapon it committed to.
    LostWeapon,
    /// Nothing left to load and nothing loaded to switch to.
    NoAmmo,
    Died,
    /// The simulator did not reach the committed outcome in its own time.
    Timeout,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeaponStatus {
    #[default]
    Idle,
    Running,
    Succeeded,
    Failed(WeaponFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Commitment {
    act: WeaponAct,
    held: u16,
    clip_at_start: i32,
    life: LifeSequence,
    deadline_tick: u32,
    /// The simulator was observed entering the transition at least once.
    entered: bool,
}

/// The single owner of the weapon channel: it decides between reloading and
/// switching, then holds one request until the simulation confirms the outcome.
/// Lowering, raising, reload completion and ammunition transfer stay in the
/// weapon simulator.
#[derive(Clone, Copy, Debug, Default)]
pub struct WeaponSkill {
    commitment: Option<Commitment>,
    status: WeaponStatus,
    retry_after_tick: u32,
}

impl WeaponSkill {
    pub fn status(&self) -> WeaponStatus {
        self.status
    }

    pub fn act(&self) -> Option<WeaponAct> {
        self.commitment.map(|c| c.act)
    }

    /// The weapon id the command must keep requesting through the transition.
    /// Requesting the old id while the hand drops settles it back to ready, so a
    /// one-tick pulse is not a switching contract.
    pub fn requested_weapon(&self) -> Option<u16> {
        match self.commitment.map(|c| c.act) {
            Some(WeaponAct::Switch { to }) => Some(to),
            _ => None,
        }
    }

    pub fn wants_reload(&self) -> bool {
        matches!(self.commitment.map(|c| c.act), Some(WeaponAct::Reload))
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// One decision slice. `allow_start` is the caller's veto: an interaction
    /// close to completion is not displaced merely because a magazine is empty.
    /// `prefer` is a change the fight asks for (a riot shield bot taking out its
    /// sidearm); a magazine that needs attention comes first.
    pub fn update(
        &mut self,
        obs: &BotObservation,
        allow_start: bool,
        prefer: Option<WeaponAct>,
    ) -> WeaponStatus {
        if let Some(mut commitment) = self.commitment {
            let outcome = commitment.progress(obs);
            self.commitment = Some(commitment);
            if outcome == WeaponStatus::Running {
                self.status = WeaponStatus::Running;
                return self.status;
            }
            self.commitment = None;
            self.status = outcome;
            if matches!(outcome, WeaponStatus::Failed(_)) {
                self.retry_after_tick = obs.tick.saturating_add(RETRY_TICKS);
            }
            return self.status;
        }
        self.status = WeaponStatus::Idle;
        if !allow_start || obs.tick < self.retry_after_tick {
            return self.status;
        }
        let prefer = prefer.filter(|act| match *act {
            WeaponAct::Switch { to } => to != obs.self_state.weapon && obs.owns(to),
            WeaponAct::Reload => true,
        });
        let Some(act) = choose(obs).or(prefer) else {
            return self.status;
        };
        self.commitment = Some(Commitment::begin(act, obs));
        self.status = WeaponStatus::Running;
        self.status
    }
}

impl Commitment {
    fn begin(act: WeaponAct, obs: &BotObservation) -> Self {
        let me = &obs.self_state;
        let held = held_slot(obs);
        let budget_ms = match act {
            WeaponAct::Switch { to } => obs
                .slot(to)
                .map_or(0, |slot| slot.switch_ms(&held))
                .max(held.facts.drop_time_ms),
            WeaponAct::Reload => held.reload_ms(),
        };
        let budget_ms = if budget_ms > 0 {
            budget_ms
        } else {
            DEFAULT_ACTION_MS
        };
        Self {
            act,
            held: me.weapon,
            clip_at_start: me.clip_total(),
            life: me.life_sequence,
            deadline_tick: obs
                .tick
                .saturating_add((budget_ms.max(0) / TICK_MS) as u32)
                .saturating_add(DEADLINE_SLACK_TICKS),
            entered: false,
        }
    }

    fn progress(&mut self, obs: &BotObservation) -> WeaponStatus {
        let me = &obs.self_state;
        if me.life_sequence != self.life || me.lifecycle != ClientLifecycle::Alive {
            return WeaponStatus::Failed(WeaponFailure::Died);
        }
        match self.act {
            WeaponAct::Switch { to } => {
                if !obs.owns(to) {
                    return WeaponStatus::Failed(WeaponFailure::LostWeapon);
                }
                if me.weapon == to {
                    self.entered = true;
                    // The id alone is not the outcome: the hand still raises.
                    if me.weapon_action.is_settled() {
                        return WeaponStatus::Succeeded;
                    }
                } else if me.weapon != self.held {
                    return WeaponStatus::Failed(WeaponFailure::LostWeapon);
                } else if me.weapon_action == WeaponAction::Dropping {
                    self.entered = true;
                }
            }
            WeaponAct::Reload => {
                if me.weapon != self.held {
                    return WeaponStatus::Failed(WeaponFailure::LostWeapon);
                }
                if me.weapon_action == WeaponAction::Reloading {
                    self.entered = true;
                }
                if me.clip_total() > self.clip_at_start {
                    return WeaponStatus::Succeeded;
                }
                if !self.entered && me.ammo_stock <= 0 {
                    return WeaponStatus::Failed(WeaponFailure::NoAmmo);
                }
            }
        }
        if obs.tick >= self.deadline_tick {
            return WeaponStatus::Failed(WeaponFailure::Timeout);
        }
        WeaponStatus::Running
    }
}

/// The weapon in hand. When the content carries no row for it, reloading is
/// still a legal command; only the comparison between actions needs timings.
fn held_slot(obs: &BotObservation) -> WeaponSlot {
    match obs.held() {
        Some(slot) => *slot,
        None => WeaponSlot {
            weapon: obs.self_state.weapon,
            ..WeaponSlot::default()
        },
    }
}

/// Combat equipment needs attention when the gun in hand cannot shoot: its
/// magazines are empty, or it is not a gun the bot can fight with at all (a
/// lock-on launcher, a knife), or it is a launcher whose own blast would reach
/// the bot. Reloading and switching are then compared on the simulator's own
/// timings, with a visible threat deciding whether speed outranks keeping the
/// current gun. Only usable guns are ever switched to.
pub(crate) fn choose(obs: &BotObservation) -> Option<WeaponAct> {
    let me = &obs.self_state;
    if !me.weapon_action.is_settled() {
        return None;
    }
    let held = held_slot(obs);
    // An offhand or an item in hand is not a readiness decision, and a riot
    // shield is fought with, not reloaded: leaving it is the fight's call.
    if obs
        .held()
        .is_some_and(|slot| !slot.facts.selectable || slot.facts.shield)
    {
        return None;
    }
    let range = obs.nearest_threat();
    // A weapon the content has no row for keeps the magazine's word.
    let usable = obs.held().is_none_or(|slot| slot.is_usable_gun());
    let fits = |slot: &&WeaponSlot| slot.weapon != me.weapon && slot.usable_at(range);
    let quickest = |slots: &mut dyn Iterator<Item = &WeaponSlot>| {
        slots
            .map(|slot| (slot.switch_ms(&held), slot.weapon))
            .min()
            .map(|(_, to)| WeaponAct::Switch { to })
    };
    if usable && me.clip_total() > 0 {
        if held.usable_at(range) {
            return None;
        }
        // A loaded launcher with the enemy inside its blast: put it away for a
        // gun that still has rounds anywhere, or keep it until the range opens.
        return quickest(
            &mut obs
                .inventory
                .iter()
                .filter(fits)
                .filter(|slot| slot.rounds() > 0 || slot.stock > 0),
        );
    }
    let reload_ms = (usable && me.ammo_stock > 0).then(|| held.reload_ms());
    let switch = obs
        .inventory
        .iter()
        .filter(fits)
        .filter(|slot| slot.is_loaded_gun())
        .map(|slot| (slot.switch_ms(&held), slot.weapon))
        .min();
    match (switch, reload_ms) {
        (Some((switch_ms, to)), Some(reload_ms)) => {
            let threatened = !obs.seen.is_empty();
            Some(if threatened && switch_ms < reload_ms {
                WeaponAct::Switch { to }
            } else {
                WeaponAct::Reload
            })
        }
        (Some((_, to)), None) => Some(WeaponAct::Switch { to }),
        (None, Some(_)) => Some(WeaponAct::Reload),
        // Nothing loaded anywhere: the gun with reserve that is quickest to
        // raise, which the next decision (or the raise itself) reloads. Without
        // this an Overwatch bot that emptied its MG4 and then fired its AT4
        // stood still for the rest of a match with 100 MG4 rounds in reserve,
        // and so did a sniper whose pistol ran dry.
        (None, None) => quickest(
            &mut obs
                .inventory
                .iter()
                .filter(fits)
                .filter(|slot| slot.stock > 0),
        ),
    }
}

/// The gun a riot-shield bot takes out against an enemy `range` away: a usable
/// gun with rounds anywhere, the quickest to raise.
pub(crate) fn sidearm(obs: &BotObservation, range: Option<f32>) -> Option<u16> {
    let held = held_slot(obs);
    obs.inventory
        .iter()
        .filter(|slot| slot.weapon != obs.self_state.weapon && slot.is_armed_gun())
        .filter(|slot| slot.usable_at(range))
        .map(|slot| (slot.switch_ms(&held), slot.weapon))
        .min()
        .map(|(_, to)| to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{
        Contact, KnowledgeSource, SelfState, Stance, WeaponClass, WeaponFacts,
    };
    use sim::{ClientId, ClientLifecycle, LifeSequence};

    const AT4: u16 = 79;
    const MG4: u16 = 713;
    const FRAG: u16 = 372;
    const JAVELIN: u16 = 407;
    const CHEYTAC: u16 = 186;
    const SHIELD: u16 = 836;
    const USP: u16 = 1123;
    const UZI_AKIMBO: u16 = 1144;

    fn facts(selectable: bool, clip: i32) -> WeaponFacts {
        WeaponFacts {
            selectable,
            clip_size: clip.max(1),
            raise_time_ms: 600,
            drop_time_ms: 400,
            reload_time_ms: 2000,
            reload_empty_time_ms: 3000,
            bullet: selectable,
            ..WeaponFacts::default()
        }
    }

    fn slot(weapon: u16, selectable: bool, clip: i32, stock: i32) -> WeaponSlot {
        WeaponSlot {
            weapon,
            class: WeaponClass::default(),
            facts: facts(selectable, clip),
            clip,
            stock,
            akimbo: false,
            clip_left: 0,
        }
    }

    fn launcher(weapon: u16, needs_lock: bool, clip: i32, stock: i32) -> WeaponSlot {
        WeaponSlot {
            facts: WeaponFacts {
                bullet: false,
                launcher: true,
                needs_lock,
                ..facts(true, 1)
            },
            ..slot(weapon, true, clip, stock)
        }
    }

    fn shield(weapon: u16) -> WeaponSlot {
        WeaponSlot {
            facts: WeaponFacts {
                bullet: false,
                shield: true,
                ..facts(true, 1)
            },
            ..slot(weapon, true, 1, 0)
        }
    }

    /// The Overwatch bot of the Estate smoke run: the AT4 in hand fired, the
    /// MG4 magazine empty with 100 rounds in reserve.
    fn spent_launcher(inventory: Vec<WeaponSlot>) -> BotObservation {
        BotObservation {
            tick: 2758,
            time_ms: 0,
            self_state: SelfState {
                life_sequence: LifeSequence(1),
                id: ClientId(4),
                lifecycle: ClientLifecycle::Alive,
                origin: [0.0; 3],
                viewangles: [0.0; 3],
                delta_angles: [0.0; 3],
                view_height: 60.0,
                stance: Stance::Stand,
                health: 100,
                weapon: AT4,
                weapon_class: WeaponClass::default(),
                weapon_action: WeaponAction::Ready,
                ammo_clip: 0,
                ammo_stock: 0,
                team: 0,
                linked: false,
                akimbo: false,
                ammo_clip_left: 0,
            },
            inventory,
            seen: Vec::new(),
            unsensed: Vec::new(),
            events: Vec::new(),
            objective: None,
            objectives: Vec::new(),
        }
    }

    /// The bot holds `weapon` with the magazine and reserve of its slot.
    fn holding(mut obs: BotObservation, weapon: u16) -> BotObservation {
        let held = *obs.slot(weapon).expect("held weapon in the inventory");
        obs.self_state.weapon = weapon;
        obs.self_state.ammo_clip = held.clip;
        obs.self_state.ammo_clip_left = held.clip_left;
        obs.self_state.akimbo = held.akimbo;
        obs.self_state.ammo_stock = held.stock;
        obs
    }

    fn enemy_at(mut obs: BotObservation, range: f32) -> BotObservation {
        obs.seen.push(Contact {
            id: ClientId(0),
            origin: [range, 0.0, 0.0],
            source: KnowledgeSource::CurrentlySeen,
            seen_tick: obs.tick,
            confidence: 1.0,
        });
        obs
    }

    #[test]
    fn a_spent_gun_switches_to_one_with_reserve() {
        let obs = spent_launcher(vec![
            launcher(AT4, false, 0, 0),
            slot(FRAG, false, 1, 1),
            slot(MG4, true, 0, 100),
        ]);
        assert_eq!(choose(&obs), Some(WeaponAct::Switch { to: MG4 }));
    }

    #[test]
    fn nothing_in_reserve_and_offhands_offer_no_switch() {
        let obs = spent_launcher(vec![
            launcher(AT4, false, 0, 0),
            slot(FRAG, false, 1, 1),
            slot(MG4, true, 0, 0),
        ]);
        assert_eq!(choose(&obs), None);
        assert!(!obs.armed(None));
    }

    #[test]
    fn a_loaded_gun_or_a_reload_still_comes_first() {
        // Another gun loaded: switch to it, as before.
        let mut obs = spent_launcher(vec![launcher(AT4, false, 0, 0), slot(MG4, true, 30, 100)]);
        assert_eq!(choose(&obs), Some(WeaponAct::Switch { to: MG4 }));
        // The held gun has reserve of its own: reload it, as before.
        obs.inventory = vec![launcher(AT4, false, 0, 0), slot(MG4, true, 0, 100)];
        let obs = holding(obs, MG4);
        assert_eq!(choose(&obs), Some(WeaponAct::Reload));
    }

    #[test]
    fn a_lock_on_launcher_is_never_taken_and_counts_as_empty_in_hand() {
        // The Intervention bot of the god-mode runs: rifle empty with ten in
        // reserve. The loaded Javelin is not a gun it can fight with.
        let obs = spent_launcher(vec![
            slot(CHEYTAC, true, 0, 10),
            launcher(JAVELIN, true, 1, 1),
        ]);
        let obs = enemy_at(holding(obs, CHEYTAC), 900.0);
        assert_eq!(choose(&obs), Some(WeaponAct::Reload));
        // Already holding the loaded Javelin: back to the rifle, which the
        // next decision reloads.
        let obs = holding(obs, JAVELIN);
        assert_eq!(choose(&obs), Some(WeaponAct::Switch { to: CHEYTAC }));
    }

    #[test]
    fn a_shield_in_hand_is_left_to_the_fight() {
        let obs = spent_launcher(vec![shield(SHIELD), slot(USP, true, 12, 24)]);
        let obs = enemy_at(holding(obs, SHIELD), 150.0);
        assert_eq!(choose(&obs), None);
        assert!(!obs.held_gun_loaded());
        assert!(obs.armed(Some(150.0)));
        assert_eq!(sidearm(&obs, Some(700.0)), Some(USP));
        // The fight may ask for the sidearm; the skill commits to it.
        let mut skill = WeaponSkill::default();
        assert_eq!(skill.update(&obs, true, None), WeaponStatus::Idle);
        let take_out = Some(WeaponAct::Switch { to: USP });
        assert_eq!(skill.update(&obs, true, take_out), WeaponStatus::Running);
        assert_eq!(skill.requested_weapon(), Some(USP));
        // A shield is never a gun to switch to either.
        let obs = spent_launcher(vec![slot(USP, true, 0, 0), shield(SHIELD)]);
        let obs = holding(obs, USP);
        assert_eq!(choose(&obs), None);
        assert!(!obs.armed(None));
    }

    #[test]
    fn a_launcher_is_not_raised_or_kept_with_the_enemy_close() {
        // MG4 empty with reserve, the AT4 loaded, an enemy 150 units away:
        // reload the MG4 rather than switch to a rocket that would kill both.
        let obs = spent_launcher(vec![slot(MG4, true, 0, 100), launcher(AT4, false, 1, 0)]);
        let obs = holding(obs, MG4);
        assert_eq!(
            choose(&enemy_at(obs.clone(), 900.0)),
            Some(WeaponAct::Switch { to: AT4 })
        );
        assert_eq!(
            choose(&enemy_at(obs.clone(), 150.0)),
            Some(WeaponAct::Reload)
        );
        // The AT4 loaded in hand when the enemy closes in: put it away.
        let obs = holding(obs, AT4);
        assert_eq!(choose(&enemy_at(obs.clone(), 900.0)), None);
        assert_eq!(
            choose(&enemy_at(obs.clone(), 150.0)),
            Some(WeaponAct::Switch { to: MG4 })
        );
        // With nothing else to fire, the bot has only its melee that close.
        let obs = spent_launcher(vec![slot(MG4, true, 0, 0), launcher(AT4, false, 1, 0)]);
        let obs = holding(obs, AT4);
        assert!(!obs.armed(Some(150.0)));
        assert!(obs.armed(Some(900.0)));
    }

    #[test]
    fn both_akimbo_magazines_count() {
        let pair = WeaponSlot {
            akimbo: true,
            clip_left: 9,
            ..slot(UZI_AKIMBO, true, 0, 32)
        };
        let obs = holding(
            spent_launcher(vec![pair, slot(USP, true, 12, 24)]),
            UZI_AKIMBO,
        );
        assert!(obs.held_gun_loaded());
        assert_eq!(choose(&obs), None);
        let dry = WeaponSlot {
            clip_left: 0,
            ..pair
        };
        let obs = holding(
            spent_launcher(vec![dry, slot(USP, true, 12, 24)]),
            UZI_AKIMBO,
        );
        assert_eq!(choose(&obs), Some(WeaponAct::Reload));
    }

    #[test]
    fn usable_guns_are_a_whitelist() {
        let law = WeaponFacts {
            launcher: true,
            ..facts(true, 1)
        };
        let gun = |f: WeaponFacts| WeaponFacts { bullet: false, ..f };
        // T5: the M72 LAW fires unlocked, the Strela-3 needs a lock.
        assert!(gun(law).is_usable_gun());
        assert!(
            !gun(WeaponFacts {
                needs_lock: true,
                ..law
            })
            .is_usable_gun()
        );
        // A raw T5 weapType that is neither bullet nor projectile (melee 7).
        assert!(!gun(facts(true, 1)).is_usable_gun());
        // Offhands are not guns whatever their type.
        assert!(!facts(false, 1).is_usable_gun());
        assert!(facts(true, 30).is_usable_gun());
    }
}
