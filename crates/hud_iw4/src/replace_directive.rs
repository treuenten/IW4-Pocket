extern crate alloc;

use alloc::string::String;

pub const KEY_UNBOUND: &str = "KEY_UNBOUND";

pub fn replace_directive(src: &str, mut resolve: impl FnMut(&str) -> String) -> String {
    let mut out = String::new();
    let mut rest = src;
    while let Some(start) = rest.find("[{") {
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}]") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let directive = &after_open[..end];
        if directive.is_empty() {
            out.push_str("[{}]");
        } else {
            out.push_str(&resolve(directive));
        }
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

/// The localized word that joins two key names in a prompt ("E or F").
pub const KEY_OR: &str = "KEY_OR";

/// The command a key placeholder names, as the bind table spells it. MW2 looks
/// the name up without regard to case.
pub fn placeholder_command(directive: &str) -> String {
    directive.trim().to_ascii_lowercase()
}

/// The use button's other command. MW2's PC strings name `+activate` and its
/// console strings `+usereload`; the keyboard binds the first and the
/// controller layouts the second, so a prompt naming either one takes the
/// other's keys when nothing is bound to its own.
pub fn use_button_alias(command: &str) -> Option<&'static str> {
    match command {
        "+activate" => Some("+usereload"),
        "+usereload" => Some("+activate"),
        _ => None,
    }
}

/// How MW2 names the keys bound to a command in a prompt (a `[{command}]`
/// placeholder or a hint's `&&1`): the one key, or the first two joined by the
/// localized "or" when more are bound. None when no key is bound: the prompt
/// then reads the localized KEY_UNBOUND ("Unbound") on its own.
pub fn binding_text<S: AsRef<str>>(keys: &[S], or_localized: &str) -> Option<String> {
    match keys {
        [] => None,
        [key] => Some(key.as_ref().into()),
        [first, second, ..] => Some(alloc::format!(
            "{} {or_localized} {}",
            first.as_ref(),
            second.as_ref()
        )),
    }
}

pub fn default_mp_key_binding(command: &str) -> Option<&'static str> {
    match command {
        "+activate" => Some("F"),

        "+gostand" => Some("SPACE"),
        _ => None,
    }
}

#[must_use]
pub fn hudelem_default_text_scale(elem_font_scale: f32) -> f32 {
    crate::hudelem_text_scale(0, elem_font_scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_key_is_named_on_its_own() {
        assert_eq!(binding_text(&["Y"], "or").as_deref(), Some("Y"));
    }

    #[test]
    fn two_keys_are_joined_by_or_and_a_third_is_left_out() {
        assert_eq!(
            binding_text(&["MOUSE3", "1"], "or").as_deref(),
            Some("MOUSE3 or 1")
        );
        assert_eq!(
            binding_text(&["E", "F", "G"], "or").as_deref(),
            Some("E or F")
        );
    }

    #[test]
    fn no_key_leaves_the_prompt_to_say_unbound() {
        assert_eq!(binding_text::<&str>(&[], "or"), None);
    }

    #[test]
    fn placeholders_name_commands_in_any_case() {
        assert_eq!(placeholder_command("WeapNext"), "weapnext");
        assert_eq!(placeholder_command("+actionslot 4"), "+actionslot 4");
    }

    #[test]
    fn the_use_button_answers_to_both_names() {
        assert_eq!(use_button_alias("+activate"), Some("+usereload"));
        assert_eq!(use_button_alias("+usereload"), Some("+activate"));
        assert_eq!(use_button_alias("weapnext"), None);
    }

    #[test]
    fn a_placeholder_is_replaced_by_the_name_without_brackets() {
        let text = replace_directive("Press^3 [{weapnext}] ^7 Steal their class!", |cmd| {
            assert_eq!(cmd, "weapnext");
            "Y".into()
        });
        assert_eq!(text, "Press^3 Y ^7 Steal their class!");
    }
}
