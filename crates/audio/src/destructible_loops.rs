use std::collections::HashMap;

use asset_core::AssetNamespace;
use bevy::prelude::*;
use net::PresentedSnapshot;

use crate::ambient::SoundBankNamespace;
use crate::backend::{AudioScope, MatchEpoch};
use crate::playback::SoundBank;
use crate::runtime::AudioRuntime;
use crate::sources::{DesiredSource, SourceKey};

struct PresentedLoop {
    alias: String,
    namespace: AssetNamespace,
    emitter: Option<u32>,
    source: DesiredSource,
}

#[derive(Resource, Default)]
pub(crate) struct DestructibleSources {
    epoch: u64,
    next_version: u64,
    live: HashMap<u32, PresentedLoop>,
    pub desired: Vec<DesiredSource>,
}

pub(crate) fn update(
    presented: Res<PresentedSnapshot>,
    bank: Option<Res<SoundBank>>,
    namespace: Option<Res<SoundBankNamespace>>,
    epoch: Res<MatchEpoch>,
    runtime: Res<AudioRuntime>,
    mut sources: ResMut<DestructibleSources>,
    mut diag_at: Local<Option<std::time::Instant>>,
) {
    let diag = crate::diagnostics::enabled()
        && diag_at.is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
    if diag {
        *diag_at = Some(std::time::Instant::now());
    }
    if sources.epoch != epoch.0 {
        sources.epoch = epoch.0;
        sources.live.clear();
        sources.desired.clear();
    }
    let Some(snapshot) = presented.snapshot() else {
        return;
    };
    let ns = namespace.map_or(AssetNamespace::Iw4, |map| map.namespace);
    let alias_name = |index: u8| {
        snapshot
            .meta
            .sound_aliases
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, alias)| alias.as_str())
    };
    let mut speaking: Vec<_> = snapshot
        .meta
        .world_objects
        .destructible_loop_sounds
        .iter()
        .filter_map(|row| Some((row, alias_name(row.alias_index)?)))
        .collect();
    speaking.sort_unstable_by_key(|(row, _)| row.owner.to_wire());
    speaking.dedup_by_key(|(row, _)| row.owner.to_wire());
    speaking.truncate(crate::runtime::LOGICAL_INSTANCES);
    sources.live.retain(|owner, _| {
        speaking
            .binary_search_by_key(owner, |(row, _)| row.owner.to_wire())
            .is_ok()
    });
    sources.desired.clear();
    for (row, alias) in speaking {
        let owner = row.owner.to_wire();
        // A blended layer (a vehicle engine) plays one voice of the pair's
        // sound with the volume and pitch moved between the two aliases.
        let blend = row.blend.and_then(|blend| {
            let bank = bank.as_ref()?;
            let toward = alias_name(blend.toward)?;
            blended_layer(
                &bank.0,
                ns,
                alias,
                toward,
                f32::from(blend.fraction) / 255.0,
            )
        });
        let alias = blend.as_ref().map_or(alias, |layer| layer.alias);
        let (gain, rate) = blend
            .as_ref()
            .map_or((1.0, 1.0), |layer| (layer.gain, layer.rate));
        let changed = sources.live.get(&owner).is_none_or(|current| {
            current.alias != alias || current.namespace != ns || current.emitter != row.snd_ent
        });
        if changed {
            sources.live.remove(&owner);
            let Some(bank) = &bank else {
                continue;
            };
            let Some(cue) = runtime.source_cue(crate::sources::SourceCueRequest {
                bank: bank.0.clone(),
                namespace: ns,
                alias: alias.into(),
                emitter: row.snd_ent,
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
            sources.live.insert(
                owner,
                PresentedLoop {
                    alias: alias.into(),
                    namespace: ns,
                    emitter: row.snd_ent,
                    source: DesiredSource {
                        key: SourceKey {
                            scope: AudioScope::Match,
                            epoch: epoch.0,
                            object: u64::from(owner),
                            slot: 1,
                        },
                        version,
                        cue,
                        origin_inches: Some(row.origin),
                        start_frame: runtime.audio_frame(),
                        gain,
                        rate,
                        audible: true,
                    },
                },
            );
        }
        if diag && let Some(blend) = row.blend {
            crate::diagnostics::emit(format!(
                "audio diag: blended loop alias={alias} owner={owner:#x} fraction={} gain={gain:.3} rate={rate:.3} origin={:.0?}",
                blend.fraction, row.origin
            ));
        }
        let current = sources.live.get_mut(&owner).expect("presented loop");
        current.source.origin_inches = Some(row.origin);
        current.source.gain = gain;
        current.source.rate = rate;
        let desired = current.source.clone();
        sources.desired.push(desired);
    }
}

struct BlendedLayer<'a> {
    alias: &'a str,
    gain: f32,
    rate: f32,
}

fn alias_level(
    bank: &asset_audio::SoundCatalog,
    ns: AssetNamespace,
    alias: &str,
) -> Option<(f32, f32)> {
    let variant = bank.sound_in(ns, alias)?.aliases.first()?;
    Some((
        (variant.vol_min + variant.vol_max) * 0.5,
        (variant.pitch_min + variant.pitch_max) * 0.5,
    ))
}

/// One voice for a pair of aliases on the same sound, `fraction` of the way
/// from `rest` to `moving`: it plays the louder alias (a silent end, such as a
/// truck's idle at speed, cannot carry the other) with gain and rate scaled so
/// its volume and pitch are the pair's interpolated ones.
fn blended_layer<'a>(
    bank: &asset_audio::SoundCatalog,
    ns: AssetNamespace,
    rest: &'a str,
    moving: &'a str,
    fraction: f32,
) -> Option<BlendedLayer<'a>> {
    let (rest_vol, rest_pitch) = alias_level(bank, ns, rest)?;
    let (moving_vol, moving_pitch) = alias_level(bank, ns, moving)?;
    let (alias, base_vol, base_pitch) = if moving_vol > rest_vol {
        (moving, moving_vol, moving_pitch)
    } else {
        (rest, rest_vol, rest_pitch)
    };
    if base_vol <= 0.0 || base_pitch <= 0.0 {
        return None;
    }
    let (gain, rate) = blend_scale(
        [rest_vol, moving_vol],
        [rest_pitch, moving_pitch],
        [base_vol, base_pitch],
        fraction,
    );
    Some(BlendedLayer { alias, gain, rate })
}

fn blend_scale(vol: [f32; 2], pitch: [f32; 2], base: [f32; 2], fraction: f32) -> (f32, f32) {
    let t = fraction.clamp(0.0, 1.0);
    let volume = vol[0] + (vol[1] - vol[0]) * t;
    let pitch = pitch[0] + (pitch[1] - pitch[0]) * t;
    (volume / base[0], pitch / base[1])
}

#[cfg(test)]
mod tests {
    use super::blend_scale;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4
    }

    #[test]
    fn a_cobra_idle_loop_fades_and_rises_in_pitch_as_it_speeds_up() {
        // cobra_idle_high (0.623, 0.95) at rest, cobra_idle_low (0.173, 1.10)
        // at speed, played through the louder idle_high.
        let vol = [0.623, 0.173];
        let pitch = [0.95, 1.10];
        let base = [0.623, 0.95];
        assert!(close(blend_scale(vol, pitch, base, 0.0), (1.0, 1.0)));
        assert!(close(
            blend_scale(vol, pitch, base, 1.0),
            (0.173 / 0.623, 1.10 / 0.95)
        ));
        assert!(close(
            blend_scale(vol, pitch, base, 0.5),
            (0.398 / 0.623, 1.025 / 0.95)
        ));
    }

    #[test]
    fn a_silent_end_scales_the_louder_alias_down_to_nothing() {
        // The SP truck's engine_low is silent; engine_high carries the sound.
        let vol = [0.0, 0.381];
        let pitch = [0.75, 1.0];
        let base = [0.381, 1.0];
        assert!(close(blend_scale(vol, pitch, base, 0.0), (0.0, 0.75)));
        assert!(close(blend_scale(vol, pitch, base, 1.0), (1.0, 1.0)));
    }
}
