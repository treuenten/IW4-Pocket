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

/// A spent gun in hand, nothing to load it with and no other gun loaded: switch
/// to a gun that still has reserve ammunition, which the next decision reloads.
///
/// Off by default. Without it `choose` has nothing to offer in that state and
/// the bot keeps the empty gun until it dies: an Overwatch bot that emptied its
/// MG4 at an enemy and then fired its AT4 stood still for the rest of a match
/// with 100 MG4 rounds in reserve, and so does a sniper whose pistol runs dry.
/// `IW4L_BOT_RESERVE_AMMO=1` in iw4l-env.txt turns it on for the phone.
pub const RESERVE_AMMO_ENV: &str = "IW4L_BOT_RESERVE_AMMO";

fn reserve_ammo() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| perf::switch(RESERVE_AMMO_ENV))
}

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
    pub fn update(&mut self, obs: &BotObservation, allow_start: bool) -> WeaponStatus {
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
        let Some(act) = choose(obs) else {
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
            clip_at_start: me.ammo_clip,
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
                if me.ammo_clip > self.clip_at_start {
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

/// Combat equipment needs attention when the held magazine is empty. Reloading
/// and switching are then compared on the simulator's own timings, with a
/// visible threat deciding whether speed outranks keeping the current gun.
fn choose(obs: &BotObservation) -> Option<WeaponAct> {
    choose_with(obs, reserve_ammo())
}

/// `reserve`: see [`RESERVE_AMMO_ENV`].
fn choose_with(obs: &BotObservation, reserve: bool) -> Option<WeaponAct> {
    let me = &obs.self_state;
    if !me.weapon_action.is_settled() || me.ammo_clip > 0 {
        return None;
    }
    let held = held_slot(obs);
    // An offhand or an item in hand is not a readiness decision.
    if obs.held().is_some_and(|slot| !slot.facts.selectable) {
        return None;
    }
    let reload_ms = (me.ammo_stock > 0).then(|| held.reload_ms());
    let switch = obs
        .inventory
        .iter()
        .filter(|slot| slot.weapon != me.weapon && slot.is_loaded_gun())
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
        // Nothing loaded anywhere: the gun with reserve that is quickest to raise.
        (None, None) if reserve => obs
            .inventory
            .iter()
            .filter(|slot| slot.weapon != me.weapon && slot.facts.selectable && slot.stock > 0)
            .map(|slot| (slot.switch_ms(&held), slot.weapon))
            .min()
            .map(|(_, to)| WeaponAct::Switch { to }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{SelfState, Stance, WeaponClass, WeaponFacts};
    use sim::{ClientId, ClientLifecycle, LifeSequence};

    const AT4: u16 = 79;
    const MG4: u16 = 713;
    const FRAG: u16 = 372;

    fn slot(weapon: u16, selectable: bool, clip: i32, stock: i32) -> WeaponSlot {
        WeaponSlot {
            weapon,
            class: WeaponClass::default(),
            facts: WeaponFacts {
                selectable,
                clip_size: clip.max(1),
                raise_time_ms: 600,
                drop_time_ms: 400,
                reload_time_ms: 2000,
                reload_empty_time_ms: 3000,
                ..WeaponFacts::default()
            },
            clip,
            stock,
        }
    }

    /// The Overwatch bot of the Estate smoke run: the AT4 in hand fired, the
    /// MG4 magazine empty with 100 rounds in reserve, an enemy in view.
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
            },
            inventory,
            seen: Vec::new(),
            unsensed: Vec::new(),
            events: Vec::new(),
            objective: None,
            objectives: Vec::new(),
        }
    }

    #[test]
    fn a_spent_gun_switches_to_reserve_only_with_the_switch() {
        let obs = spent_launcher(vec![
            slot(AT4, true, 0, 0),
            slot(FRAG, false, 1, 1),
            slot(MG4, true, 0, 100),
        ]);
        assert_eq!(choose_with(&obs, false), None);
        assert_eq!(choose_with(&obs, true), Some(WeaponAct::Switch { to: MG4 }));
    }

    #[test]
    fn nothing_in_reserve_and_offhands_offer_no_switch() {
        let obs = spent_launcher(vec![
            slot(AT4, true, 0, 0),
            slot(FRAG, false, 1, 1),
            slot(MG4, true, 0, 0),
        ]);
        assert_eq!(choose_with(&obs, true), None);
    }

    #[test]
    fn a_loaded_gun_or_a_reload_still_comes_first() {
        // Another gun loaded: switch to it, as before.
        let mut obs = spent_launcher(vec![slot(AT4, true, 0, 0), slot(MG4, true, 30, 100)]);
        assert_eq!(choose_with(&obs, true), Some(WeaponAct::Switch { to: MG4 }));
        // The held gun has reserve of its own: reload it, as before.
        obs.self_state.weapon = MG4;
        obs.self_state.ammo_stock = 100;
        obs.inventory = vec![slot(AT4, true, 0, 0), slot(MG4, true, 0, 100)];
        assert_eq!(choose_with(&obs, true), Some(WeaponAct::Reload));
        assert_eq!(choose_with(&obs, false), Some(WeaponAct::Reload));
    }
}
