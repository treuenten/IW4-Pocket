//! Testing cheat menu: a CHEATS row in the in-match pause menu that opens a menu
//! of rows running the console's cheat verbs (`god`, `player_sustainAmmo`,
//! `give killstreak/…`, `bot add`, `force_match_start`).
//!
//! `IW4L_CHEAT_MENU=1` (iw4l-env.txt on the phone, a plain environment variable
//! on the desktop) turns it on; without it the menu catalog is left as it was and
//! nothing is published. The console crate publishes the state the rows show, and
//! only for the host of a private match with cheats on.

use std::sync::OnceLock;

pub const CHEAT_MENU_ENV: &str = "IW4L_CHEAT_MENU";

/// Only the value `1` turns it on.
fn switched_on(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

/// Read once, like the other restart-only switches.
pub fn cheat_menu_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| switched_on(std::env::var(CHEAT_MENU_ENV).ok().as_deref()))
}

/// Adds the pause menu's CHEATS row and the `iw4l_cheats` menu it opens. A
/// failure leaves the menus untouched: a testing aid must not stop the game.
pub(crate) fn install(catalog: &mut asset_game::MenuCatalog) {
    if !cheat_menu_enabled() {
        return;
    }
    match catalog.load_definitions(include_str!("../menus/cheats.json")) {
        Ok(()) => diag::info!(Ui, "cheat menu: installed ({CHEAT_MENU_ENV}=1)"),
        Err(error) => diag::warn!(Ui, "cheat menu: not installed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_switches_the_menu_on() {
        assert!(!switched_on(None));
        assert!(!switched_on(Some("0")));
        assert!(!switched_on(Some("on")));
        assert!(!switched_on(Some("")));
        assert!(switched_on(Some("1")));
        assert!(switched_on(Some(" 1 ")));
    }
}
