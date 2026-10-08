use crate::MenuMapList;

/// What the lists call `map`: the mission a campaign key carries, the stem of any other.
pub fn map_label(map: &str) -> String {
    if let Some(title) = asset_transport::campaign_key_title(map) {
        return title.to_uppercase();
    }
    map.split_once(':')
        .map_or(map, |(_, name)| name)
        .trim_start_matches("mp_")
        .replace('_', " ")
        .to_uppercase()
}

/// `label` for a place that holds `limit` letters: a longer title stops where it pauses (after its
/// "...") or at the last word that fits, and says so with "...".
pub fn fit_title(label: &str, limit: usize) -> String {
    if label.chars().count() <= limit {
        return label.to_owned();
    }
    if let Some(pause) = label.find("...")
        && label[..pause].chars().count() + 3 <= limit
    {
        return label[..pause + 3].to_owned();
    }
    let mut fitted = String::new();
    for word in label.split_whitespace() {
        let next = if fitted.is_empty() {
            word.to_owned()
        } else {
            format!("{fitted} {word}")
        };
        if next.chars().count() + 3 > limit {
            break;
        }
        fitted = next;
    }
    if fitted.is_empty() {
        fitted = label.chars().take(limit.saturating_sub(3)).collect();
    }
    fitted.push_str("...");
    fitted
}

/// The material that previews `map`, or nothing: a campaign zone has no preview image (the
/// multiplayer ones are `preview_<stem>`, and a `preview_trainer` is in no archive), so its
/// preview is left empty rather than named for an image that is not there.
pub fn map_preview(map: &str) -> String {
    match map.split_once(':').unwrap_or(("iw4", map)) {
        (_, "") => String::new(),
        (asset_transport::CAMPAIGN_NAMESPACE, _) => String::new(),
        ("t5", stem) => format!(
            "t5:material/menu_mp_map_select_{}_big",
            stem.trim_start_matches("mp_")
        ),
        ("t6", stem) => format!("t6:material/menu_{stem}_map_select_final"),
        (namespace, stem) => format!("{namespace}:material/preview_{stem}"),
    }
}

pub fn pack_maps(maps: &MenuMapList, pack: usize) -> &[String] {
    maps.0.get(pack).map_or(&[], |pack| pack.maps.as_slice())
}

#[derive(bevy::prelude::Resource, Default)]
pub struct MapPresentation {
    entries: std::collections::BTreeMap<String, (asset_core::AssetKey, String)>,
}

impl MapPresentation {
    pub fn from_tables(
        tables: &[(asset_core::AssetNamespace, asset_game::CapturedStringTable)],
    ) -> Self {
        let mut entries = std::collections::BTreeMap::new();
        for (namespace, table) in tables {
            if *namespace != asset_core::AssetNamespace::T6
                || !table.name.eq_ignore_ascii_case("mp/mapstable.csv")
            {
                continue;
            }
            for row in 0..table.rows as i32 {
                let map = table.cell(row, 0);
                if !map.starts_with("mp_") {
                    continue;
                }
                let Ok(title) = asset_core::AssetKey::new(
                    *namespace,
                    asset_core::AssetKind::Localize,
                    table.cell(row, 3),
                ) else {
                    continue;
                };
                let Ok(image) = asset_core::AssetKey::new(
                    *namespace,
                    asset_core::AssetKind::Material,
                    table.cell(row, 4),
                ) else {
                    continue;
                };
                entries.insert(
                    format!("{}:{map}", namespace.as_str()),
                    (title, image.to_string()),
                );
            }
        }
        Self { entries }
    }

    pub fn label(&self, map: &str, strings: Option<&asset_game::LocalizeCatalog>) -> String {
        self.entries
            .get(map)
            .and_then(|(title, _)| strings?.text_asset(title))
            .map(str::to_owned)
            .unwrap_or_else(|| map_label(map))
    }

    pub fn preview(&self, map: &str) -> String {
        self.entries
            .get(map)
            .map(|(_, image)| {
                let loaded = asset_core::AssetKey::parse(image)
                    .is_ok_and(|key| asset_material::has_zone_ui_image(key.namespace, &key.name));
                if loaded {
                    image.clone()
                } else {
                    map_preview(map)
                }
            })
            .unwrap_or_else(|| map_preview(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_campaign_map_is_listed_by_its_mission() {
        assert_eq!(map_label("sp:trainer"), "S.S.D.D.");
        assert_eq!(map_label("sp:roadkill"), "TEAM PLAYER");
        assert_eq!(map_label("sp:favela"), "TAKEDOWN");
        assert_eq!(map_label("sp:invasion"), "WOLVERINES!");
        assert_eq!(map_label("sp:favela_escape"), "THE HORNET'S NEST");
        assert_eq!(map_label("sp:oilrig"), "THE ONLY EASY DAY... WAS YESTERDAY");
        assert_eq!(map_label("sp:boneyard"), "THE ENEMY OF MY ENEMY");
        assert_eq!(map_label("sp:ending"), "MUSEUM");
    }

    #[test]
    fn a_campaign_stem_the_table_lacks_is_listed_by_its_stem() {
        assert_eq!(map_label("sp:so_new_thing"), "SO NEW THING");
    }

    #[test]
    fn the_multiplayer_labels_are_what_they_were() {
        assert_eq!(map_label("iw4:mp_terminal"), "TERMINAL");
        assert_eq!(map_label("iw4:mp_favela"), "FAVELA");
        assert_eq!(map_label("iw4:mp_rust"), "RUST");
        assert_eq!(map_label("t6:mp_nuketown_2020"), "NUKETOWN 2020");
        assert_eq!(map_label("mp_estate"), "ESTATE");
    }

    #[test]
    fn a_campaign_map_names_no_preview_image_and_the_others_keep_theirs() {
        assert_eq!(map_preview("sp:trainer"), "");
        assert_eq!(map_preview("sp:favela"), "");
        assert_eq!(
            map_preview("iw4:mp_favela"),
            "iw4:material/preview_mp_favela"
        );
        assert_eq!(
            map_preview("mp_terminal"),
            "iw4:material/preview_mp_terminal"
        );
        assert_eq!(
            map_preview("t5:mp_nuketown"),
            "t5:material/menu_mp_map_select_nuketown_big"
        );
        assert_eq!(
            map_preview("t6:mp_nuketown_2020"),
            "t6:material/menu_mp_nuketown_2020_map_select_final"
        );
        assert_eq!(map_preview("iw4:"), "");
    }

    #[test]
    fn a_title_that_fits_is_left_alone() {
        assert_eq!(fit_title("TERMINAL", 24), "TERMINAL");
        assert_eq!(
            fit_title("THE ENEMY OF MY ENEMY", 24),
            "THE ENEMY OF MY ENEMY"
        );
        assert_eq!(fit_title("", 24), "");
    }

    #[test]
    fn a_title_too_long_stops_where_it_pauses() {
        assert_eq!(
            fit_title("THE ONLY EASY DAY... WAS YESTERDAY", 24),
            "THE ONLY EASY DAY..."
        );
    }

    #[test]
    fn a_title_with_no_pause_stops_at_the_last_word_that_fits() {
        assert_eq!(
            fit_title("A VERY LONG NAME WITHOUT ANY PAUSE IN IT AT ALL", 24),
            "A VERY LONG NAME..."
        );
        assert_eq!(
            fit_title("SUPERCALIFRAGILISTICEXPIALIDOCIOUS", 10),
            "SUPERCA..."
        );
        // A pause too far in is no place to stop.
        assert_eq!(
            fit_title("AN EXTREMELY LONG BEGINNING... AND THEN SOME", 12),
            "AN..."
        );
    }

    #[test]
    fn the_presentation_lists_a_campaign_map_the_same_way() {
        let presentation = MapPresentation::default();
        assert_eq!(presentation.label("sp:airport", None), "NO RUSSIAN");
        assert_eq!(presentation.preview("sp:airport"), "");
        assert_eq!(presentation.label("iw4:mp_rust", None), "RUST");
    }
}
