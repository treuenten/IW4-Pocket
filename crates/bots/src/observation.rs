use sim::{ClientId, ClientLifecycle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stance {
    Stand,
    Crouch,
    Prone,
}

impl Stance {
    pub fn from_view_height(height: f32) -> Self {
        if height >= 50.0 {
            Self::Stand
        } else if height >= 30.0 {
            Self::Crouch
        } else {
            Self::Prone
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnowledgeSource {
    CurrentlySeen,
    LastSeen,
    PublicMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Contact {
    pub id: ClientId,
    pub origin: [f32; 3],
    pub source: KnowledgeSource,
    pub seen_tick: u32,
    pub confidence: f32,
    /// The enemy's health when last seen. A player shooting someone sees
    /// whether the hits land (hit markers, the enemy flinching); this is that
    /// knowledge, and nothing more is read from it.
    pub health: i32,
}

/// What perception established about one client this tick. `NotSeen` is an
/// answer; `Unknown` is the absence of one, and the two must not be confused —
/// a probe that was never performed cannot end a contact or authorize a shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Seen,
    NotSeen,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeaponClass {
    #[default]
    Assault,
    Smg,
    Lmg,
    Sniper,
    Shotgun,
    /// Keeps the launcher's own blast away from the bot.
    Launcher,
    /// A riot shield fights at melee range.
    Shield,
}

impl WeaponClass {
    pub fn fight_hold(self) -> f32 {
        match self {
            Self::Launcher => 400.0,
            Self::Sniper => 360.0,
            Self::Lmg => 220.0,
            Self::Assault => 160.0,
            Self::Smg => 112.0,
            Self::Shotgun => 72.0,
            Self::Shield => 48.0,
        }
    }

    pub fn from_script_name(name: &str) -> Self {
        if name.is_empty() {
            return Self::Assault;
        }
        let stem = crate::unique_loadout::family_stem(name);
        for row in crate::unique_loadout::UNIQUE_WEAPONS {
            if crate::unique_loadout::family_stem(row.key) == stem {
                return Self::from_group(row.group);
            }
        }
        Self::Assault
    }

    fn from_group(group: crate::unique_loadout::UniqueGroup) -> Self {
        use crate::unique_loadout::UniqueGroup;
        match group {
            UniqueGroup::Sniper => Self::Sniper,
            UniqueGroup::Lmg => Self::Lmg,
            UniqueGroup::Shotgun => Self::Shotgun,
            UniqueGroup::Smg | UniqueGroup::MachinePistol | UniqueGroup::Pistol => Self::Smg,
            UniqueGroup::Projectile => Self::Launcher,
            UniqueGroup::Riot => Self::Shield,
            UniqueGroup::Assault | UniqueGroup::Cqb | UniqueGroup::Special => Self::Assault,
        }
    }
}

/// What the weapon simulator is doing with the active hand. The bot reads it;
/// it never reimplements the transitions behind it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WeaponAction {
    #[default]
    Ready,
    Raising,
    Dropping,
    Firing,
    Reloading,
    /// Melee, offhand, rechamber, sprint or a stun — not a readiness decision.
    Busy,
}

impl WeaponAction {
    pub fn from_weaponstate(raw: i32) -> Self {
        use weapon_iw4::WeaponState as State;
        match State::from_i32(raw) {
            Ok(State::Ready) => Self::Ready,
            Ok(State::Raising | State::RaisingAltswitch) => Self::Raising,
            Ok(State::Dropping | State::DroppingQuick | State::DroppingAltswitch) => Self::Dropping,
            Ok(State::Firing) => Self::Firing,
            Ok(state) if state.is_reload_family() => Self::Reloading,
            _ => Self::Busy,
        }
    }

    /// An active weapon id alone does not mean the hand finished raising.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Ready | Self::Firing)
    }
}

/// Timing and magazine facts for one weapon id, read from the content the
/// simulator uses. Not a second weapon model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeaponFacts {
    /// A carried gun the player can select rather than an offhand or an item.
    pub selectable: bool,
    /// Pistol-class selection uses the quick raise and drop timings.
    pub quick_select: bool,
    pub clip_size: i32,
    pub raise_time_ms: i32,
    pub quick_raise_time_ms: i32,
    pub drop_time_ms: i32,
    pub quick_drop_time_ms: i32,
    pub reload_time_ms: i32,
    pub reload_empty_time_ms: i32,
    /// Milliseconds between two shots of the trigger hand.
    pub fire_time_ms: i32,
    /// One round per pull of the trigger (fireType single shot or double
    /// barrel): the weapon fires again only after the trigger is let go.
    pub semi_auto: bool,
    /// weapType bullet: a gun that hits along a trace.
    pub bullet: bool,
    /// weapType projectile: a launcher.
    pub launcher: bool,
    /// weapType riot shield on the IW4, IW5 and T6 rows. Its attack is the
    /// melee bash; the fire button only clicks.
    pub shield: bool,
    /// requireLockToFire: the simulation lets the trigger through only with a
    /// lock-on, which the bots never acquire.
    pub needs_lock: bool,
    /// The launcher's explosion radius in units, 0 for a gun.
    pub blast_radius: i32,
    /// The magazine's row in the player's clip table.
    pub clip_index: i32,
    /// The melee can lunge at a target beyond the plain melee range.
    pub lunge: bool,
}

/// A launcher is neither raised nor fired with an enemy closer than this:
/// MW2 rockets arm almost at once and the bot would stand in its own blast.
pub const LAUNCHER_MIN_RANGE: f32 = 300.0;

impl WeaponFacts {
    /// The closest an enemy may be for this weapon to be used on it.
    pub fn min_range(&self) -> f32 {
        if self.launcher {
            LAUNCHER_MIN_RANGE.max(1.5 * self.blast_radius as f32)
        } else {
            0.0
        }
    }

    /// A gun the bot can fight with: selectable, and either a bullet weapon or
    /// a launcher that fires without a lock. A whitelist on purpose, so a shield,
    /// a knife or a T5 row whose raw weapType means something else is never
    /// taken for a gun.
    pub fn is_usable_gun(&self) -> bool {
        self.selectable && (self.bullet || (self.launcher && !self.needs_lock))
    }
}

/// One owned weapon with the ammunition the match state holds for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WeaponSlot {
    pub weapon: u16,
    pub class: WeaponClass,
    pub facts: WeaponFacts,
    /// The right-hand magazine, the only one unless the weapon is akimbo.
    pub clip: i32,
    pub stock: i32,
    /// Carried as an akimbo pair: two magazines on one shared reserve.
    pub akimbo: bool,
    /// The left-hand magazine of an akimbo pair, 0 otherwise.
    pub clip_left: i32,
}

impl WeaponSlot {
    /// Milliseconds from committing to a change until this weapon is raised,
    /// leaving `from` behind. Both halves follow the incoming weapon's class.
    pub fn switch_ms(&self, from: &Self) -> i32 {
        if self.facts.quick_select {
            from.facts.quick_drop_time_ms + self.facts.quick_raise_time_ms
        } else {
            from.facts.drop_time_ms + self.facts.raise_time_ms
        }
    }

    pub fn reload_ms(&self) -> i32 {
        if self.clip == 0 {
            self.facts.reload_empty_time_ms
        } else {
            self.facts.reload_time_ms
        }
    }

    /// Rounds in the magazines, both hands of an akimbo pair counted.
    pub fn rounds(&self) -> i32 {
        self.clip + self.clip_left
    }

    pub fn is_usable_gun(&self) -> bool {
        self.facts.is_usable_gun()
    }

    pub fn is_loaded_gun(&self) -> bool {
        self.is_usable_gun() && self.rounds() > 0
    }

    /// A usable gun that may be used on an enemy this far away (`None`: no
    /// enemy in view).
    pub fn usable_at(&self, range: Option<f32>) -> bool {
        self.is_usable_gun() && range.is_none_or(|range| range >= self.facts.min_range())
    }

    /// A usable gun with rounds in a magazine or in reserve.
    pub fn is_armed_gun(&self) -> bool {
        self.is_usable_gun() && (self.rounds() > 0 || self.stock > 0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelfState {
    pub life_sequence: sim::LifeSequence,
    pub id: ClientId,
    pub lifecycle: ClientLifecycle,
    pub origin: [f32; 3],
    pub viewangles: [f32; 3],
    pub delta_angles: [f32; 3],
    pub view_height: f32,
    pub stance: Stance,
    pub health: i32,
    pub weapon: u16,
    pub weapon_class: WeaponClass,
    pub weapon_action: WeaponAction,
    /// The right-hand magazine of the weapon in hand.
    pub ammo_clip: i32,
    pub ammo_stock: i32,
    pub team: i32,
    /// Held by a script `playerLinkTo`, as while planting or defusing.
    pub linked: bool,
    /// The weapon in hand is an akimbo pair (lastWeaponHand 1): the left gun
    /// fires on the attack button and the right one on the throw button.
    pub akimbo: bool,
    /// The left-hand magazine of an akimbo pair in hand, 0 otherwise.
    pub ammo_clip_left: i32,
}

impl SelfState {
    /// Rounds in the magazines of the weapon in hand, both hands counted.
    pub fn clip_total(&self) -> i32 {
        self.ammo_clip + self.ammo_clip_left
    }
}

pub const DEFAULT_OBJECTIVE_RADIUS: f32 = 96.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ObjectiveAction {
    #[default]
    Capture,
    Plant,
    Defuse,
    Defend,
    /// Bring the carried flag to the team's own capture zone.
    Deliver,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TeamRole {
    #[default]
    Actor,
    Cover,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModeObjective {
    pub id: u32,
    pub round: u32,
    pub action: ObjectiveAction,
    pub role: TeamRole,
    pub active_user: Option<ClientId>,
    pub remaining_ms: Option<u32>,
    /// Milliseconds the interaction takes from the press to the end.
    pub interaction_ms: u32,
    pub progress: f32,
    pub origin: [f32; 3],
    pub touching: bool,
    pub use_button: bool,
    pub radius: f32,
}

impl ModeObjective {
    pub fn at(origin: [f32; 3]) -> Self {
        Self {
            id: 0,
            round: 0,
            action: ObjectiveAction::Capture,
            role: TeamRole::Actor,
            active_user: None,
            remaining_ms: None,
            interaction_ms: 0,
            progress: 0.0,
            origin,
            touching: false,
            use_button: false,
            radius: DEFAULT_OBJECTIVE_RADIUS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotEvent {
    Spotted { id: ClientId },
    LostSight { id: ClientId },
}

#[derive(Clone, Debug, PartialEq)]
pub struct BotObservation {
    pub tick: u32,
    pub time_ms: i32,
    pub self_state: SelfState,
    /// The bot's own carried weapons. Enemy inventories are never projected.
    pub inventory: Vec<WeaponSlot>,
    pub seen: Vec<Contact>,
    /// Clients perception did not finish evaluating this tick, because its
    /// allowance ran out, a query was denied, or a trace came back
    /// unclassified. A client in neither `seen` nor here was evaluated and not
    /// seen, which is a real negative observation.
    pub unsensed: Vec<ClientId>,
    pub events: Vec<BotEvent>,
    pub objective: Option<ModeObjective>,
    pub objectives: Vec<ModeObjective>,
}

impl BotObservation {
    pub fn held(&self) -> Option<&WeaponSlot> {
        self.slot(self.self_state.weapon)
    }

    pub fn slot(&self, weapon: u16) -> Option<&WeaponSlot> {
        self.inventory.iter().find(|slot| slot.weapon == weapon)
    }

    pub fn owns(&self, weapon: u16) -> bool {
        self.inventory.iter().any(|slot| slot.weapon == weapon)
    }

    /// The weapon in hand is a gun that can shoot now. A weapon the content has
    /// no row for keeps the magazine's word, as before the facts existed.
    pub fn held_gun_loaded(&self) -> bool {
        self.self_state.clip_total() > 0 && self.held().is_none_or(WeaponSlot::is_usable_gun)
    }

    pub fn holds_shield(&self) -> bool {
        self.held().is_some_and(|slot| slot.facts.shield)
    }

    /// Some gun usable on an enemy `range` away still has rounds, in a
    /// magazine or in reserve. Without one the bot can only fight with its
    /// melee.
    pub fn armed(&self, range: Option<f32>) -> bool {
        if self.held().is_none()
            && (self.self_state.clip_total() > 0 || self.self_state.ammo_stock > 0)
        {
            return true;
        }
        self.inventory
            .iter()
            .any(|slot| slot.is_armed_gun() && slot.usable_at(range))
    }

    /// Distance to the nearest enemy in view.
    pub fn nearest_threat(&self) -> Option<f32> {
        let here = self.self_state.origin;
        self.seen
            .iter()
            .map(|contact| {
                let d = [
                    contact.origin[0] - here[0],
                    contact.origin[1] - here[1],
                    contact.origin[2] - here[2],
                ];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
            })
            .min_by(f32::total_cmp)
    }

    pub fn eye(&self) -> [f32; 3] {
        let z = if self.self_state.view_height > 1.0 {
            self.self_state.view_height
        } else {
            60.0
        };
        [
            self.self_state.origin[0],
            self.self_state.origin[1],
            self.self_state.origin[2] + z,
        ]
    }

    pub fn visibility(&self, id: ClientId) -> Visibility {
        if self.seen.iter().any(|contact| contact.id == id) {
            Visibility::Seen
        } else if self.unsensed.contains(&id) {
            Visibility::Unknown
        } else {
            Visibility::NotSeen
        }
    }

    pub fn events_since(&self, prev_seen: &[ClientId]) -> Vec<BotEvent> {
        let mut events = Vec::new();
        for contact in &self.seen {
            if !prev_seen.contains(&contact.id) {
                events.push(BotEvent::Spotted { id: contact.id });
            }
        }
        for id in prev_seen {
            // Only a completed negative observation is a loss of sight. A skipped
            // probe leaves the previous knowledge exactly as it was.
            if self.visibility(*id) == Visibility::NotSeen {
                events.push(BotEvent::LostSight { id: *id });
            }
        }
        events
    }
}
