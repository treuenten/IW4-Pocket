//! The state the cheat menu shows (`IW4L_CHEAT_MENU=1`, see `ui::cheat_menu_enabled`).
//! Its rows run console verbs; this only publishes the menu dvars they read: whether
//! the pause menu offers CHEATS at all, and the labels of the two toggle rows.

use bevy::prelude::*;
use net::{AuthorityWorld, LocalPresentClient, MasterBridge, PresentedSnapshot};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CheatMenuState {
    available: bool,
    god: bool,
    ammo: bool,
}

fn toggle_label(name: &str, on: bool) -> String {
    if on {
        format!("{name}: ^2ON^7")
    } else {
        format!("{name}: OFF")
    }
}

/// Only the host of a match that is not online: a joined client has no authority,
/// and a public lobby or master room is never offered cheats from here. The verbs
/// behind the rows still refuse when the match was started with cheats off.
pub(crate) fn publish_state(
    presented: Res<PresentedSnapshot>,
    local: Res<LocalPresentClient>,
    authority: Option<Res<AuthorityWorld>>,
    bridge: Option<Res<MasterBridge>>,
    mut dvars: ResMut<frame::UiMenuDvars>,
    mut shown: Local<Option<CheatMenuState>>,
) {
    let meta = presented
        .snapshot()
        .and_then(|snapshot| snapshot.meta.for_client(local.0));
    let online = dvars.get("ui_lobby_public") == Some("1")
        || bridge
            .as_ref()
            .is_some_and(|bridge| !bridge.state().is_terminal());
    let state = CheatMenuState {
        available: meta.is_some()
            && !online
            && authority
                .as_ref()
                .is_some_and(|authority| authority.0.cheats_enabled()),
        god: meta.is_some_and(|meta| meta.god_mode),
        ammo: meta.is_some_and(|meta| sim::sustains_ammo(&meta.client_dvars)),
    };
    if shown.as_ref() == Some(&state) {
        return;
    }
    dvars.set("ui_cheat_menu", if state.available { "1" } else { "0" });
    dvars.set("ui_cheat_god", toggle_label("God Mode", state.god));
    dvars.set("ui_cheat_ammo", toggle_label("Unlimited Ammo", state.ammo));
    *shown = Some(state);
}

#[cfg(test)]
mod tests {
    use super::toggle_label;

    #[test]
    fn toggle_rows_show_their_state() {
        assert_eq!(toggle_label("God Mode", false), "God Mode: OFF");
        assert_eq!(toggle_label("God Mode", true), "God Mode: ^2ON^7");
    }
}
