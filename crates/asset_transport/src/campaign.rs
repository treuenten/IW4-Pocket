//! The single-player zones, named so they can be told from the multiplayer maps.
//!
//! A campaign zone is a FastFile of the single-player game, `trainer.ff` or `favela.ff`, and the
//! session plays it as a free-roam multiplayer map (`MapFacts::campaign`). Four of its stems
//! (`favela`, `invasion`, `estate`, `boneyard`) are the stems of multiplayer maps too, and a bare
//! stem resolves to `mp_<stem>` first, so a campaign zone is asked for by its own namespace:
//! `sp:favela` is `favela.ff` and never `mp_favela.ff`.

/// The key namespace of a campaign zone: `sp:<stem>`.
pub const CAMPAIGN_NAMESPACE: &str = "sp";

/// What the map list calls the pack the campaign zones are in.
pub const CAMPAIGN_PACK_LABEL: &str = "Campaign (free roam)";

/// A campaign zone's stem and the title of the mission it carries, in the order the campaign
/// plays them. The list is the whole claim: an installed `common.ff`, `ui.ff` or special-ops zone
/// shares the folder and is not a map.
pub const CAMPAIGN_MAPS: [(&str, &str); 19] = [
    ("trainer", "S.S.D.D."),
    ("roadkill", "Team Player"),
    ("cliffhanger", "Cliffhanger"),
    ("airport", "No Russian"),
    ("favela", "Takedown"),
    ("invasion", "Wolverines!"),
    ("favela_escape", "The Hornet's Nest"),
    ("arcadia", "Exodus"),
    ("oilrig", "The Only Easy Day... Was Yesterday"),
    ("gulag", "The Gulag"),
    ("dcburning", "Of Their Own Accord"),
    ("contingency", "Contingency"),
    ("dcemp", "Second Sun"),
    ("dc_whitehouse", "Whiskey Hotel"),
    ("estate", "Loose Ends"),
    ("boneyard", "The Enemy of My Enemy"),
    ("af_caves", "Just Like Old Times"),
    ("af_chase", "Endgame"),
    ("ending", "Museum"),
];

/// The mission title of a campaign stem (`trainer` is "S.S.D.D."); `None` for any other zone.
pub fn campaign_title(stem: &str) -> Option<&'static str> {
    CAMPAIGN_MAPS
        .iter()
        .find(|(known, _)| *known == stem)
        .map(|(_, title)| *title)
}

/// The key that names the campaign zone `stem`, and no other zone of that stem.
pub fn campaign_key(stem: &str) -> String {
    format!("{CAMPAIGN_NAMESPACE}:{stem}")
}

/// The stem of a `sp:<stem>` key, whatever the stem is; `None` for a key of any other namespace.
/// The key is as the user typed it: case is the caller's to fold.
pub fn campaign_stem(key: &str) -> Option<&str> {
    key.strip_prefix(CAMPAIGN_NAMESPACE)?
        .strip_prefix(':')
        .filter(|stem| !stem.is_empty())
}

/// Whether `key` names a campaign zone.
pub fn is_campaign_key(key: &str) -> bool {
    campaign_stem(key).is_some()
}

/// What a person reads for `key`: the mission title of a campaign key, `None` for any other key.
pub fn campaign_key_title(key: &str) -> Option<&'static str> {
    campaign_title(campaign_stem(key)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_campaign_stem_has_one_title_and_none_is_a_multiplayer_stem() {
        let mut seen = std::collections::HashSet::new();
        for (stem, title) in CAMPAIGN_MAPS {
            assert!(seen.insert(stem), "{stem} listed twice");
            assert!(!title.is_empty());
            assert!(
                !stem.starts_with("mp_") && !stem.ends_with("_mp") && !stem.contains(':'),
                "{stem} would read as something else"
            );
            assert_eq!(campaign_title(stem), Some(title));
        }
    }

    #[test]
    fn the_mission_titles_are_the_ones_the_campaign_shows() {
        assert_eq!(campaign_title("trainer"), Some("S.S.D.D."));
        assert_eq!(campaign_title("airport"), Some("No Russian"));
        assert_eq!(campaign_title("favela"), Some("Takedown"));
        assert_eq!(campaign_title("invasion"), Some("Wolverines!"));
        assert_eq!(campaign_title("favela_escape"), Some("The Hornet's Nest"));
        assert_eq!(
            campaign_title("oilrig"),
            Some("The Only Easy Day... Was Yesterday")
        );
        assert_eq!(campaign_title("dc_whitehouse"), Some("Whiskey Hotel"));
        assert_eq!(campaign_title("estate"), Some("Loose Ends"));
        assert_eq!(campaign_title("boneyard"), Some("The Enemy of My Enemy"));
        assert_eq!(campaign_title("ending"), Some("Museum"));
        assert_eq!(campaign_title("mp_favela"), None);
        assert_eq!(campaign_title("common"), None);
        assert_eq!(campaign_title("so_killspree_trainer"), None);
    }

    #[test]
    fn a_campaign_key_round_trips_and_other_keys_are_not_one() {
        assert_eq!(campaign_key("favela"), "sp:favela");
        assert_eq!(campaign_stem("sp:favela"), Some("favela"));
        assert_eq!(campaign_stem(&campaign_key("af_caves")), Some("af_caves"));
        assert!(is_campaign_key("sp:trainer"));
        assert_eq!(campaign_stem("sp:"), None);
        assert_eq!(campaign_stem("sp"), None);
        assert_eq!(campaign_stem("spy:favela"), None);
        assert_eq!(campaign_stem("iw4:favela"), None);
        assert_eq!(campaign_stem("favela"), None);
        assert_eq!(campaign_stem("iw4:mp_favela"), None);
    }

    #[test]
    fn a_key_reads_as_its_mission_title() {
        assert_eq!(campaign_key_title("sp:favela"), Some("Takedown"));
        assert_eq!(campaign_key_title("sp:unknown"), None);
        assert_eq!(campaign_key_title("iw4:mp_favela"), None);
    }
}
