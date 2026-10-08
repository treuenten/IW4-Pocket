use frame::UiMenuDvars;
use sim::HostGameModeSelection;

use crate::MenuMapList;

/// What the lobby tells a host whose setup a campaign map cannot start.
pub const CAMPAIGN_MODES_NOTICE: &str = "Campaign maps play Free for All or Team Deathmatch.";

/// What the lobby tells a host who would take a campaign map to a public lobby.
pub const CAMPAIGN_PRIVATE_NOTICE: &str = "Campaign maps are only available in private matches.";

/// Whether `map` can start `mode`: every map takes every mode, a campaign map two of them.
pub fn mode_fits_map(map: &str, mode: HostGameModeSelection) -> bool {
    !asset_transport::is_campaign_key(map) || mode.plays_campaign_map()
}

/// The mode a lobby set up for `mode` plays `map` in: that one when the map takes it, Free for All
/// when it does not.
pub fn mode_for_map(map: &str, mode: HostGameModeSelection) -> HostGameModeSelection {
    if mode_fits_map(map, mode) {
        mode
    } else {
        HostGameModeSelection::FREE_FOR_ALL
    }
}

/// A campaign map belongs to a private match: the public lobbies and the master that lists them
/// know the multiplayer maps.
pub fn map_fits_lobby(map: &str, public: bool) -> Result<(), String> {
    if public && asset_transport::is_campaign_key(map) {
        return Err(CAMPAIGN_PRIVATE_NOTICE.into());
    }
    Ok(())
}

/// What the lobby says when `mode` is asked of a map that cannot play it.
pub fn mode_refusal(mode: HostGameModeSelection) -> String {
    format!(
        "{CAMPAIGN_MODES_NOTICE} {} cannot start here.",
        mode.display_name()
    )
}

/// The map and mode the lobby holds, whether or not the map plays that mode.
pub fn current_setup(
    dvars: &UiMenuDvars,
    maps: &MenuMapList,
) -> Result<(String, HostGameModeSelection), String> {
    let map = dvars.get("ui_mapname").ok_or("No map selected")?;
    if !maps.contains(map) {
        return Err(format!("Map `{map}` is not installed"));
    }
    let mode = dvars
        .get("ui_gametype")
        .and_then(HostGameModeSelection::from_token)
        .ok_or("Unsupported game mode")?;
    Ok((map.to_owned(), mode))
}

/// The map and mode the lobby is set up for, when it is a setup that starts.
pub fn selected_game(
    dvars: &UiMenuDvars,
    maps: &MenuMapList,
) -> Result<(String, HostGameModeSelection), String> {
    let (map, mode) = current_setup(dvars, maps)?;
    if !mode_fits_map(&map, mode) {
        return Err(mode_refusal(mode));
    }
    Ok((map, mode))
}

pub const MATCH_CONFIG: &str = "default_xboxlive.cfg";

fn is_rule(name: &str) -> bool {
    name.starts_with("scr_") || name == "g_hardcore"
}

pub fn seed_rules(dvars: &mut UiMenuDvars, config: &str) {
    for line in config.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("set ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let (Some(name), value) = (words.next(), words.next().unwrap_or("")) else {
            continue;
        };
        let name = name.to_ascii_lowercase();
        if is_rule(&name) && dvars.get(&name).is_none() {
            dvars.set(&name, value.trim_matches('"'));
        }
    }
    for dvar in [
        sim::CONSTANT_RADAR_DVAR,
        sim::ENEMY_BOTS_DVAR,
        sim::FRIENDLY_BOTS_DVAR,
    ] {
        if dvars.get(dvar).is_none() {
            dvars.set(dvar, "0");
        }
    }
}

pub fn host_rules(dvars: &UiMenuDvars) -> frame::HostMatchRules {
    frame::HostMatchRules(
        dvars
            .iter()
            .filter(|(name, _)| is_rule(name))
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(token: &str) -> HostGameModeSelection {
        HostGameModeSelection::from_token(token).expect("a game mode")
    }

    fn lobby(map: &str, mode: &str) -> (UiMenuDvars, MenuMapList) {
        let mut dvars = UiMenuDvars::default();
        dvars.set("ui_mapname", map);
        dvars.set("ui_gametype", mode);
        let maps = MenuMapList(vec![asset_transport::MapPack {
            label: "TEST".into(),
            maps: vec![
                "iw4:mp_terminal".into(),
                "sp:trainer".into(),
                "sp:favela".into(),
            ],
        }]);
        (dvars, maps)
    }

    #[test]
    fn a_multiplayer_map_takes_every_mode() {
        for mode in HostGameModeSelection::ALL {
            assert!(mode_fits_map("iw4:mp_terminal", mode), "{}", mode.token());
            assert_eq!(mode_for_map("iw4:mp_terminal", mode), mode);
        }
    }

    #[test]
    fn a_campaign_map_takes_free_for_all_and_team_deathmatch_and_no_other() {
        let fits: Vec<_> = HostGameModeSelection::ALL
            .into_iter()
            .filter(|mode| mode_fits_map("sp:favela", *mode))
            .map(HostGameModeSelection::token)
            .collect();
        assert_eq!(fits, ["dm", "war"]);
    }

    #[test]
    fn a_lobby_set_up_for_another_mode_plays_a_campaign_map_in_free_for_all() {
        for token in ["sd", "dom", "ctf", "koth", "sab", "dd"] {
            assert_eq!(
                mode_for_map("sp:trainer", mode(token)),
                mode("dm"),
                "{token}"
            );
        }
        assert_eq!(mode_for_map("sp:trainer", mode("war")), mode("war"));
        assert_eq!(mode_for_map("sp:trainer", mode("dm")), mode("dm"));
    }

    #[test]
    fn a_campaign_map_stays_out_of_a_public_lobby() {
        assert!(map_fits_lobby("sp:trainer", false).is_ok());
        assert!(map_fits_lobby("iw4:mp_terminal", true).is_ok());
        assert!(map_fits_lobby("iw4:mp_terminal", false).is_ok());
        assert_eq!(
            map_fits_lobby("sp:trainer", true),
            Err(CAMPAIGN_PRIVATE_NOTICE.to_owned())
        );
    }

    #[test]
    fn a_setup_starts_when_the_map_takes_the_mode() {
        let (dvars, maps) = lobby("sp:trainer", "war");
        assert_eq!(
            selected_game(&dvars, &maps),
            Ok(("sp:trainer".to_owned(), mode("war")))
        );
        let (dvars, maps) = lobby("iw4:mp_terminal", "sd");
        assert_eq!(
            selected_game(&dvars, &maps),
            Ok(("iw4:mp_terminal".to_owned(), mode("sd")))
        );
    }

    #[test]
    fn a_setup_a_campaign_map_cannot_start_says_so() {
        let (dvars, maps) = lobby("sp:favela", "sd");
        let error = selected_game(&dvars, &maps).unwrap_err();
        assert!(error.contains(CAMPAIGN_MODES_NOTICE), "{error}");
        assert!(error.contains("SEARCH AND DESTROY"), "{error}");
    }

    #[test]
    fn a_map_that_is_not_listed_or_a_mode_that_is_not_one_is_refused() {
        let (dvars, maps) = lobby("sp:oilrig", "dm");
        assert!(
            selected_game(&dvars, &maps)
                .unwrap_err()
                .contains("not installed")
        );
        let (dvars, maps) = lobby("sp:trainer", "nonsense");
        assert_eq!(
            selected_game(&dvars, &maps),
            Err("Unsupported game mode".to_owned())
        );
        let (_, maps) = lobby("sp:trainer", "dm");
        assert_eq!(
            selected_game(&UiMenuDvars::default(), &maps),
            Err("No map selected".to_owned())
        );
    }
}
