//! Key names in on-screen prompts, as MW2 fills them: a `[{command}]`
//! placeholder in a hint, a hud elem, a splash or a menu item, and the `&&1`
//! of the use, mantle and hold-breath hints, all name the keys bound to the
//! command (the engine's key-binding lookup), for the device in use.

use asset_game::LocalizeCatalog;
use hud_iw4::{KEY_OR, KEY_UNBOUND};

/// The keys bound to `command` on the device the prompts name, in key order;
/// for the use button, its other command's keys when its own has none.
fn bound_keys<'a>(input: &'a frame::HudInputView, command: &str) -> &'a [String] {
    let command = hud_iw4::placeholder_command(command);
    let keys = |command: &str| {
        input
            .binding_keys
            .get(command)
            .map(Vec::as_slice)
            .filter(|keys| !keys.is_empty())
    };
    keys(&command)
        .or_else(|| hud_iw4::use_button_alias(&command).and_then(keys))
        .unwrap_or(&[])
}

/// MW2's names for the keys bound to `command`: "Y", or "E or F" when two are
/// bound. None when nothing is.
pub(crate) fn bound_key_text(
    input: &frame::HudInputView,
    localize: Option<&LocalizeCatalog>,
    command: &str,
) -> Option<String> {
    let or = localize.and_then(|l| l.text(KEY_OR)).unwrap_or("or");
    hud_iw4::binding_text(bound_keys(input, command), or)
}

/// The localized word MW2 prints for a command no key is bound to.
pub(crate) fn unbound_text(localize: Option<&LocalizeCatalog>) -> &str {
    localize
        .and_then(|l| l.text(KEY_UNBOUND))
        .unwrap_or("Unbound")
}

/// What MW2 prints in place of a `[{command}]` placeholder: the keys bound to
/// the command, or "Unbound".
pub(crate) fn placeholder_text(
    input: Option<&frame::HudInputView>,
    localize: Option<&LocalizeCatalog>,
    command: &str,
) -> String {
    input
        .and_then(|input| bound_key_text(input, localize, command))
        .unwrap_or_else(|| unbound_text(localize).to_owned())
}
