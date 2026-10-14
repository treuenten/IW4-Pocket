pub(crate) use asset_audio::{
    ChannelAdmission as ChannelRule, VoiceLimit as AliasLimit, VoiceLimitMode as LimitMode,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AdmissionPolicy {
    pub bank_revision: u64,
    pub alias: Option<usize>,
    pub emitter: Option<u32>,
    pub channel: Option<ChannelRule>,
    pub limits: [AliasLimit; 2],
    pub priority: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AdmissionFailure {
    QueueFull = 1,
    LogicalBudget,
    PhysicalBudget,
    Concurrency,
    Cancelled,
    StaleScope,
    OutputUnavailable,
}

/// `IW4L_SILENT_LOOPS_FREE_CHANNEL=0` goes back to counting a silent loop
/// against its channel's voice limit, as before (on by default).
pub(crate) fn silent_loops_free_channel() -> bool {
    static FREE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FREE.get_or_init(|| {
        std::env::var("IW4L_SILENT_LOOPS_FREE_CHANNEL").map_or(true, |value| value != "0")
    })
}

pub(crate) struct Occupant {
    pub id: u64,
    pub policy: AdmissionPolicy,
    pub priority: f32,
    /// Not being mixed: a loop out of earshot or past the voice budget. It
    /// still answers to the one-sound-per-entity rule of a restricted
    /// channel, but holds none of the channel's voices.
    pub virtual_voice: bool,
}

pub(crate) fn plan(
    request: &AdmissionPolicy,
    occupants: &[Occupant],
) -> Result<Vec<u64>, AdmissionFailure> {
    let mut victims = std::collections::BTreeSet::new();
    let same_bank = |occupant: &Occupant| occupant.policy.bank_revision == request.bank_revision;
    if let Some(channel) = request.channel {
        if channel.maximum <= 0 {
            return Err(AdmissionFailure::Concurrency);
        }
        if channel.restricted && request.emitter.is_some() {
            victims.extend(
                occupants
                    .iter()
                    .filter(|occupant| {
                        same_bank(occupant)
                            && occupant.policy.emitter == request.emitter
                            && occupant
                                .policy
                                .channel
                                .is_some_and(|old| old.key == channel.key)
                    })
                    .map(|occupant| occupant.id),
            );
        }
    }
    if request.alias.is_some() {
        for limit in request.limits {
            if limit.mode == LimitMode::Unlimited {
                continue;
            }
            if limit.count == 0 {
                return Err(AdmissionFailure::Concurrency);
            }
            let mut candidates: Vec<_> = occupants
                .iter()
                .filter(|occupant| {
                    same_bank(occupant)
                        && occupant.policy.alias == request.alias
                        && (!limit.per_emitter || occupant.policy.emitter == request.emitter)
                        && !victims.contains(&occupant.id)
                })
                .collect();
            let excess = candidates
                .len()
                .saturating_add(1)
                .saturating_sub(usize::from(limit.count));
            if excess == 0 {
                continue;
            }
            match limit.mode {
                LimitMode::Oldest => candidates.sort_unstable_by_key(|occupant| occupant.id),
                LimitMode::Priority => candidates.sort_unstable_by(|a, b| {
                    a.priority.total_cmp(&b.priority).then(a.id.cmp(&b.id))
                }),
                _ => return Err(AdmissionFailure::Concurrency),
            }
            for victim in candidates.into_iter().take(excess) {
                if limit.mode == LimitMode::Priority && request.priority <= victim.priority + 3.0 {
                    return Err(AdmissionFailure::Concurrency);
                }
                victims.insert(victim.id);
            }
        }
    }
    if let Some(channel) = request.channel {
        // A channel's `maxvoices` (soundaliases/channels.def) counts the
        // voices playing on it. A loop nobody can hear (a map emitter out of
        // range) plays on no voice, so it does not fill the channel: Favela's
        // fifty-odd "auto" emitters would otherwise refuse every other "auto"
        // sound on the map, the Predator's rocket loop and glass breaks among
        // them.
        let remaining = occupants
            .iter()
            .filter(|occupant| {
                same_bank(occupant)
                    && !occupant.virtual_voice
                    && occupant
                        .policy
                        .channel
                        .is_some_and(|old| old.key == channel.key)
                    && !victims.contains(&occupant.id)
            })
            .count();
        if remaining >= channel.maximum as usize {
            return Err(AdmissionFailure::Concurrency);
        }
    }
    Ok(victims.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto_channel(maximum: i32) -> Option<ChannelRule> {
        Some(ChannelRule {
            key: asset_audio::ChannelKey {
                namespace: asset_core::AssetNamespace::Iw4,
                id: 3,
            },
            maximum,
            restricted: false,
        })
    }

    fn occupant(id: u64, virtual_voice: bool) -> Occupant {
        Occupant {
            id,
            policy: AdmissionPolicy {
                channel: auto_channel(2),
                emitter: Some(id as u32),
                ..AdmissionPolicy::default()
            },
            priority: 0.0,
            virtual_voice,
        }
    }

    #[test]
    fn loops_out_of_earshot_leave_the_channel_free() {
        let request = AdmissionPolicy {
            channel: auto_channel(2),
            emitter: Some(99),
            ..AdmissionPolicy::default()
        };
        let silent: Vec<_> = (0..40).map(|id| occupant(id, true)).collect();
        assert_eq!(plan(&request, &silent), Ok(Vec::new()));
        let playing = [occupant(1, false), occupant(2, false), occupant(3, true)];
        assert_eq!(plan(&request, &playing), Err(AdmissionFailure::Concurrency));
    }
}
