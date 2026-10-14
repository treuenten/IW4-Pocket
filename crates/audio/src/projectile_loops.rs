//! A missile in flight plays its weapon's projectile sound (the weapon file's
//! `projectileSound`) as a loop that rides along with it, from ignition until
//! the missile is gone: the RPG's whoosh going past, the Predator's rocket
//! roar heard from its own camera. The loop is a source keyed by the missile,
//! so its position follows the missile every frame.

use std::collections::HashMap;

use asset_core::AssetNamespace;
use asset_game::{WeaponComponent, WeaponSoundSlot};
use assets::PreparedWeapons;
use bevy::prelude::*;
use net::{PresentedProjectile, PresentedSnapshot};

use crate::backend::{AudioScope, MatchEpoch};
use crate::playback::SoundBank;
use crate::runtime::AudioRuntime;
use crate::sources::{DesiredSource, SourceCueRequest, SourceKey};

/// Source slot for projectile loops (1 destructible/script loops, 2-3 map,
/// 4 menu, 5 shellshock, 6 breath).
const PROJECTILE_LOOP_SLOT: u32 = 7;

struct FlyingLoop {
    alias: String,
    namespace: AssetNamespace,
    entnum: u32,
    source: DesiredSource,
}

#[derive(Resource, Default)]
pub(crate) struct ProjectileLoopSources {
    epoch: u64,
    next_version: u64,
    live: HashMap<u32, FlyingLoop>,
    pub desired: Vec<DesiredSource>,
}

/// The time a missile's motor lights: at launch, or after the weapon's
/// ignition delay (a Javelin is thrown clear before its motor starts).
pub(crate) fn ignited(spawn_time_ms: i32, ignition_delay_ms: Option<i32>, at_ms: i32) -> bool {
    ignition_delay_ms.is_none_or(|delay| at_ms >= spawn_time_ms.saturating_add(delay))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update(
    presented: Res<PresentedSnapshot>,
    weapons: Option<Res<PreparedWeapons>>,
    bank: Option<Res<SoundBank>>,
    clock: Option<Res<net::FrameClock>>,
    epoch: Res<MatchEpoch>,
    runtime: Res<AudioRuntime>,
    mut sources: ResMut<ProjectileLoopSources>,
) {
    if sources.epoch != epoch.0 {
        sources.epoch = epoch.0;
        sources.live.clear();
        sources.desired.clear();
    }
    sources.desired.clear();
    let (Some(_), Some(weapons), Some(bank)) = (presented.snapshot(), weapons, bank) else {
        sources.live.clear();
        return;
    };
    let snapshot = &*presented;
    let Ok(bound) = weapons.for_snapshot(snapshot.weapon_epoch()) else {
        sources.live.clear();
        return;
    };
    let registry = bound.registry();
    let at_ms = clock
        .as_ref()
        .filter(|clock| clock.started())
        .map(|clock| clock.time())
        .unwrap_or_else(|| snapshot.tick().map(sim::level_time_ms).unwrap_or(0));
    let at_ms = snapshot.trajectory_time_ms(at_ms);
    let mut flying = Vec::new();
    for row in snapshot.presented_projectiles() {
        let PresentedProjectile::Authoritative(projectile) = row else {
            continue;
        };
        let Ok(entnum) = u32::try_from(projectile.entnum) else {
            continue;
        };
        if !projectile.live || projectile.grounded {
            continue;
        }
        let delay = bound
            .row(projectile.weapon)
            .and_then(|weapon| weapon.event_facts())
            .map(|facts| facts.ignition_delay_ms);
        if !ignited(projectile.spawn_time_ms, delay, at_ms) {
            continue;
        }
        if registry
            .authored_weapon_sound(projectile.weapon, WeaponSoundSlot::Projectile)
            .is_none()
        {
            continue;
        }
        let Some(alias) =
            registry.weapon_sound_alias(projectile.weapon, WeaponSoundSlot::Projectile, &bank.0)
        else {
            continue;
        };
        let Some(namespace) =
            registry.component_namespace_of(projectile.weapon, WeaponComponent::Sound)
        else {
            continue;
        };
        flying.push((
            entnum,
            alias,
            namespace,
            snapshot.projectile_origin_at(projectile, at_ms),
        ));
    }
    flying.sort_unstable_by_key(|(entnum, ..)| *entnum);
    flying.dedup_by_key(|(entnum, ..)| *entnum);
    sources.live.retain(|entnum, _| {
        flying
            .binary_search_by_key(entnum, |(entnum, ..)| *entnum)
            .is_ok()
    });
    for (entnum, alias, namespace, origin) in flying {
        let changed = sources.live.get(&entnum).is_none_or(|current| {
            current.alias != alias || current.namespace != namespace || current.entnum != entnum
        });
        if changed {
            sources.live.remove(&entnum);
            let Some(cue) = runtime.source_cue(SourceCueRequest {
                bank: bank.0.clone(),
                namespace,
                alias: alias.into(),
                emitter: Some(entnum),
                scope: AudioScope::Match,
                epoch: epoch.0,
                group: None,
            }) else {
                continue;
            };
            sources.next_version = sources
                .next_version
                .checked_add(1)
                .expect("source version exhausted");
            let version = sources.next_version;
            if crate::diagnostics::enabled() {
                crate::diagnostics::emit(format!(
                    "audio diag: projectile loop alias={alias} entnum={entnum} origin={origin:.0?}"
                ));
            }
            sources.live.insert(
                entnum,
                FlyingLoop {
                    alias: alias.into(),
                    namespace,
                    entnum,
                    source: DesiredSource {
                        key: SourceKey {
                            scope: AudioScope::Match,
                            epoch: epoch.0,
                            object: u64::from(entnum),
                            slot: PROJECTILE_LOOP_SLOT,
                        },
                        version,
                        cue,
                        origin_inches: Some(origin),
                        start_frame: runtime.audio_frame(),
                        gain: 1.0,
                        rate: 1.0,
                        audible: true,
                    },
                },
            );
        }
        let current = sources.live.get_mut(&entnum).expect("flying loop");
        current.source.origin_inches = Some(origin);
        let desired = current.source.clone();
        sources.desired.push(desired);
    }
}

#[cfg(test)]
mod tests {
    use super::ignited;

    #[test]
    fn a_missile_without_an_ignition_delay_sounds_from_launch() {
        assert!(ignited(1000, None, 1000));
        assert!(ignited(1000, Some(0), 1000));
    }

    #[test]
    fn a_delayed_motor_sounds_only_once_it_lights() {
        assert!(!ignited(1000, Some(500), 1400));
        assert!(ignited(1000, Some(500), 1500));
    }
}
