use crate::MenuMapList;

pub fn map_label(map: &str) -> String {
    map.split_once(':')
        .map_or(map, |(_, name)| name)
        .trim_start_matches("mp_")
        .replace('_', " ")
        .to_uppercase()
}

pub fn map_preview(map: &str) -> String {
    match map.split_once(':').unwrap_or(("iw4", map)) {
        (_, "") => String::new(),
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
    /// MW2 maps ("iw4:mp_abandon"): the localize key of the arena's longname.
    longnames: std::collections::BTreeMap<String, String>,
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
        Self {
            entries,
            longnames: Default::default(),
        }
    }

    /// MW2 names its maps by the arena file's `longname` (mp/basemaps.arena,
    /// the patch zone's copy listing the DLC maps too): mp_abandon is
    /// "PATCH_ABANDON" (Carnival), mp_brecourt "MPUI_WASTELAND".
    pub fn with_iw4_arena(mut self, arena: Option<&str>) -> Self {
        for (map, longname) in arena.map(arena_longnames).unwrap_or_default() {
            self.longnames.insert(format!("iw4:{map}"), longname);
        }
        self
    }

    pub fn label(&self, map: &str, strings: Option<&asset_game::LocalizeCatalog>) -> String {
        if let Some(text) = self
            .longnames
            .get(map)
            .and_then(|key| strings?.text_in(asset_core::AssetNamespace::Iw4, key))
        {
            return text.to_owned();
        }
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

/// The `map` and `longname` of every entry in an arena file, lowercased map
/// names first.
fn arena_longnames(text: &str) -> Vec<(String, String)> {
    let value = |line: &str, key: &str| {
        let rest = line.trim().strip_prefix(key)?;
        rest.starts_with(char::is_whitespace)
            .then(|| rest.trim().trim_matches('"').to_owned())
    };
    let mut names = Vec::new();
    for block in text.split('{').skip(1) {
        let body = block.split('}').next().unwrap_or(block);
        let map = body.lines().find_map(|line| value(line, "map"));
        let longname = body.lines().find_map(|line| value(line, "longname"));
        if let (Some(map), Some(longname)) = (map, longname)
            && !map.is_empty()
            && !longname.is_empty()
        {
            names.push((map.to_ascii_lowercase(), longname));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARENA: &str = r#"
{
	map			"mp_brecourt"
	longname	"MPUI_WASTELAND"
	gametype	"dm war sab sab2 dom sd sd2 hc thc ctf koth dd oneflag gtnw"
    mapimage    "preview_mp_brecourt"
}

{
	map			"mp_abandon"
	longname	"PATCH_ABANDON"
}
"#;

    #[test]
    fn the_arena_names_each_map() {
        assert_eq!(
            arena_longnames(ARENA),
            [
                ("mp_brecourt".to_owned(), "MPUI_WASTELAND".to_owned()),
                ("mp_abandon".to_owned(), "PATCH_ABANDON".to_owned()),
            ]
        );
    }

    #[test]
    fn a_map_without_its_string_keeps_its_zone_name() {
        let maps = MapPresentation::default().with_iw4_arena(Some(ARENA));
        let strings = asset_game::LocalizeCatalog::default();
        assert_eq!(maps.label("iw4:mp_abandon", Some(&strings)), "ABANDON");
        assert_eq!(maps.label("iw4:mp_abandon", None), "ABANDON");
    }
}
