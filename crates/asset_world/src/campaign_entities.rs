//! A campaign map's entity string, rewritten to run under the multiplayer rules.
//!
//! The campaign zone walks like any other, but its entities speak single-player: one
//! `info_player_start`, AI spawners, vehicle splines. The gametypes look for `mp_dm_spawn`,
//! `mp_tdm_spawn*` and `mp_global_intermission`, and `_airdrop` for a `care_package`.
//!
//! Script models and brush models are identified by their ordinal in the entity string, and the
//! placements the world draws from come from the zone's original text. So nothing is removed or
//! reordered here: entities are renamed in place, and new ones go at the end.

use crate::map_entities::parse_vec3;

type Block = Vec<(String, String)>;

/// Spawn points handed to the gametypes. Multiplayer maps ship 20 to 90.
const SPAWN_POINTS: usize = 24;
/// Pathnodes closer than this to a spawn point already chosen are skipped. The player hull is
/// 30 wide; `getSpawnpoint_*` also wants a spread to choose from.
const SPAWN_SPACING: f32 = 192.0;
/// A map with fewer pathnodes than this gets a ring of points around its start.
const SPAWN_MINIMUM: usize = 8;
const RING_RADIUS: f32 = 96.0;
/// Team deathmatch opens on `_start` points: the first and the last few of the list.
const TEAM_START_POINTS: usize = 4;
/// Where the intermission camera stands over the first start.
const INTERMISSION_RAISE: f32 = 64.0;

/// Campaign entities with no meaning to the multiplayer scripts; the script runtime skips the
/// `node_` prefix, which is what the single-player pathnodes already use.
const SINGLE_PLAYER_ONLY: &[&str] = &["actor_", "script_vehicle", "info_vehicle_node"];

/// `_animatedmodels` plays every model so named from `level.anim_prop_models`, which the
/// campaign's `maps/animated_models/*` scripts fill with animations of their own; the props stay
/// where they stand, still.
const ANIMATED_MODEL: &str = "animated_model";

#[derive(Clone, Copy, Debug, PartialEq)]
struct Site {
    origin: [f32; 3],
    angles: [f32; 3],
}

fn get<'a>(block: &'a Block, key: &str) -> Option<&'a str> {
    block
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn set(block: &mut Block, key: &str, value: String) {
    match block.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => *v = value,
        None => block.push((key.to_owned(), value)),
    }
}

fn read_blocks(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut open: Option<Block> = None;
    for line in text.lines() {
        match (line.trim(), open.as_mut()) {
            ("{", None) => open = Some(Vec::new()),
            ("}", Some(_)) => blocks.extend(open.take()),
            (trimmed, Some(block)) => {
                let mut quoted = trimmed.split('"').skip(1).step_by(2);
                if let (Some(key), Some(value)) = (quoted.next(), quoted.next()) {
                    block.push((key.to_owned(), value.to_owned()));
                }
            }
            _ => {}
        }
    }
    blocks
}

fn write_blocks(blocks: &[Block], capacity: usize) -> String {
    let mut out = String::with_capacity(capacity);
    for block in blocks {
        out.push_str("{\n");
        for (key, value) in block {
            out.push_str(&format!("\"{key}\" \"{value}\"\n"));
        }
        out.push_str("}\n");
    }
    out
}

fn site(block: &Block) -> Option<Site> {
    Some(Site {
        origin: parse_vec3(get(block, "origin")?)?,
        angles: get(block, "angles")
            .and_then(parse_vec3)
            .unwrap_or([0.0; 3]),
    })
}

fn vector(v: [f32; 3]) -> String {
    format!("{} {} {}", v[0], v[1], v[2])
}

fn spawn_block(classname: &str, site: Site) -> Block {
    vec![
        ("classname".into(), classname.into()),
        ("origin".into(), vector(site.origin)),
        ("angles".into(), vector(site.angles)),
    ]
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f32>().sqrt()
}

/// The start itself, then the pathnodes nearest it that keep their distance from every point
/// already chosen: the ground the designers made walkable around the start, spread out.
fn spawn_sites(starts: &[Site], nodes: &[Site]) -> Vec<Site> {
    let Some(&anchor) = starts.first().or(nodes.first()) else {
        return Vec::new();
    };
    let mut sites = if starts.is_empty() {
        vec![anchor]
    } else {
        starts.to_vec()
    };
    let mut nearest: Vec<&Site> = nodes.iter().collect();
    nearest.sort_by(|a, b| {
        distance(a.origin, anchor.origin).total_cmp(&distance(b.origin, anchor.origin))
    });
    for node in nearest {
        if sites.len() >= SPAWN_POINTS {
            break;
        }
        if sites
            .iter()
            .all(|chosen| distance(chosen.origin, node.origin) >= SPAWN_SPACING)
        {
            sites.push(*node);
        }
    }
    let missing = SPAWN_MINIMUM.saturating_sub(sites.len());
    for i in 0..missing {
        let yaw = 360.0 * i as f32 / missing as f32;
        let (sin, cos) = yaw.to_radians().sin_cos();
        sites.push(Site {
            origin: [
                anchor.origin[0] + RING_RADIUS * cos,
                anchor.origin[1] + RING_RADIUS * sin,
                anchor.origin[2],
            ],
            angles: [0.0, yaw, 0.0],
        });
    }
    sites
}

/// The campaign's entity string as the multiplayer gametypes need it. In place: the start becomes
/// an `mp_dm_spawn`, AI, vehicle and animated-model entities are set aside. At the end: more
/// `mp_dm_spawn` and the `mp_tdm_spawn*` points around the start, an `mp_global_intermission`
/// camera over it and the `care_package` that `_airdrop` looks for.
pub fn sp_entities_for_iw4_rules(text: &str) -> String {
    let mut blocks = read_blocks(text);
    let mut starts = Vec::new();
    let mut nodes = Vec::new();
    for block in &blocks {
        match get(block, "classname") {
            Some("info_player_start") => starts.extend(site(block)),
            Some("node_pathnode") => nodes.extend(site(block)),
            _ => {}
        }
    }
    let sites = spawn_sites(&starts, &nodes);

    for block in &mut blocks {
        let Some(classname) = get(block, "classname").map(str::to_owned) else {
            continue;
        };
        if classname == "info_player_start" && site(block).is_some() {
            set(block, "classname", "mp_dm_spawn".to_owned());
        } else if SINGLE_PLAYER_ONLY
            .iter()
            .any(|prefix| classname.starts_with(prefix))
        {
            set(block, "classname", format!("node_{classname}"));
        }
        if get(block, "targetname") == Some(ANIMATED_MODEL) {
            set(block, "targetname", format!("sp_{ANIMATED_MODEL}"));
        }
    }

    let first_derived = starts.len().min(sites.len());
    let team_starts = TEAM_START_POINTS.min(sites.len() / 2);
    let mut added: Vec<Block> = Vec::new();
    for (index, site) in sites.iter().enumerate() {
        if index >= first_derived {
            added.push(spawn_block("mp_dm_spawn", *site));
        }
        added.push(spawn_block("mp_tdm_spawn", *site));
        if index < team_starts {
            added.push(spawn_block("mp_tdm_spawn_allies_start", *site));
        }
        if index >= sites.len() - team_starts {
            added.push(spawn_block("mp_tdm_spawn_axis_start", *site));
        }
    }
    if let Some(first) = sites.first() {
        let mut view = *first;
        view.origin[2] += INTERMISSION_RAISE;
        added.push(spawn_block("mp_global_intermission", view));
    }
    if !blocks
        .iter()
        .any(|block| get(block, "targetname") == Some("care_package"))
    {
        added.push(vec![
            ("classname".into(), "script_model".into()),
            ("targetname".into(), "care_package".into()),
            ("target".into(), "care_package_collision".into()),
        ]);
        added.push(vec![
            ("classname".into(), "script_brushmodel".into()),
            ("targetname".into(), "care_package_collision".into()),
        ]);
    }
    blocks.extend(added);
    write_blocks(&blocks, text.len() + text.len() / 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_entities::{
        dm_spawn_points_in, parse_intermission_view, parse_script_model_placements,
    };

    fn entity(pairs: &[(&str, &str)]) -> String {
        let mut out = String::from("{\n");
        for (key, value) in pairs {
            out.push_str(&format!("\"{key}\" \"{value}\"\n"));
        }
        out.push_str("}\n");
        out
    }

    fn node(x: f32, y: f32) -> String {
        entity(&[
            ("classname", "node_pathnode"),
            ("origin", &format!("{x} {y} 0")),
            ("angles", "0 90 0"),
        ])
    }

    fn campaign(nodes: &[(f32, f32)]) -> String {
        let mut text = entity(&[("classname", "worldspawn")]);
        text.push_str(&entity(&[
            ("classname", "script_model"),
            ("model", "com_barrel"),
            ("origin", "10 10 10"),
        ]));
        text.push_str(&entity(&[
            ("classname", "actor_enemy_afghan_AR"),
            ("origin", "1 2 3"),
        ]));
        text.push_str(&entity(&[
            ("classname", "info_player_start"),
            ("origin", "0 0 18"),
            ("angles", "0 133.5 0"),
        ]));
        text.push_str(&entity(&[
            ("classname", "script_model"),
            ("model", "com_crate"),
            ("origin", "20 20 20"),
        ]));
        for &(x, y) in nodes {
            text.push_str(&node(x, y));
        }
        text
    }

    fn classnames(text: &str) -> Vec<String> {
        read_blocks(text)
            .iter()
            .filter_map(|block| get(block, "classname").map(str::to_owned))
            .collect()
    }

    fn count(text: &str, classname: &str) -> usize {
        classnames(text).iter().filter(|c| *c == classname).count()
    }

    #[test]
    fn the_start_becomes_a_dm_spawn_in_place() {
        let text = campaign(&[]);
        let out = sp_entities_for_iw4_rules(&text);
        let dm = dm_spawn_points_in(&out);
        assert_eq!(dm[0].classname, "mp_dm_spawn");
        assert_eq!(dm[0].origin, [0.0, 0.0, 18.0]);
        assert_eq!(dm[0].angles, [0.0, 133.5, 0.0]);
        assert_eq!(count(&out, "info_player_start"), 0);
    }

    #[test]
    fn pathnodes_keep_their_distance_and_the_limit() {
        let nodes: Vec<_> = (1..200).map(|i| (i as f32 * 64.0, 0.0)).collect();
        let out = sp_entities_for_iw4_rules(&campaign(&nodes));
        let dm = dm_spawn_points_in(&out);
        assert_eq!(dm.len(), SPAWN_POINTS);
        for (i, a) in dm.iter().enumerate() {
            for b in &dm[i + 1..] {
                assert!(distance(a.origin, b.origin) >= SPAWN_SPACING);
            }
        }
        // The nearest ground to the start, not the far end of the map.
        assert!(dm.iter().all(|p| p.origin[0] < 24.0 * SPAWN_SPACING + 64.0));
    }

    #[test]
    fn a_map_without_pathnodes_gets_a_ring_around_its_start() {
        let out = sp_entities_for_iw4_rules(&campaign(&[]));
        let dm = dm_spawn_points_in(&out);
        assert_eq!(dm.len(), SPAWN_MINIMUM);
        for point in &dm[1..] {
            let ring = distance(point.origin, [0.0, 0.0, 18.0]);
            assert!((ring - RING_RADIUS).abs() < 0.01, "{ring}");
        }
    }

    #[test]
    fn team_deathmatch_gets_its_starts_and_the_intermission_camera() {
        let nodes: Vec<_> = (1..200).map(|i| (i as f32 * 64.0, 0.0)).collect();
        let out = sp_entities_for_iw4_rules(&campaign(&nodes));
        assert_eq!(count(&out, "mp_tdm_spawn"), SPAWN_POINTS);
        assert_eq!(count(&out, "mp_tdm_spawn_allies_start"), TEAM_START_POINTS);
        assert_eq!(count(&out, "mp_tdm_spawn_axis_start"), TEAM_START_POINTS);
        let view = parse_intermission_view(&out).expect("intermission camera");
        assert_eq!(view.origin, [0.0, 0.0, 18.0 + INTERMISSION_RAISE]);
        assert_eq!(view.angles, [0.0, 133.5, 0.0]);
    }

    #[test]
    fn ordinals_hold_so_script_models_keep_their_identity() {
        let nodes: Vec<_> = (1..50).map(|i| (i as f32 * 64.0, 0.0)).collect();
        let text = campaign(&nodes);
        let out = sp_entities_for_iw4_rules(&text);
        let before: Vec<_> = parse_script_model_placements(&text)
            .into_iter()
            .map(|p| (p.id, p.model))
            .collect();
        let after: Vec<_> = parse_script_model_placements(&out)
            .into_iter()
            .map(|p| (p.id, p.model))
            .collect();
        assert_eq!(after, before);
        let renamed = classnames(&out);
        assert_eq!(renamed[2], "node_actor_enemy_afghan_AR");
        assert_eq!(renamed[3], "mp_dm_spawn");
    }

    #[test]
    fn single_player_props_that_animate_stand_still() {
        let mut text = campaign(&[]);
        text.push_str(&entity(&[
            ("classname", "script_model"),
            ("targetname", "animated_model"),
            ("model", "foliage_tree_palm_bushy_1"),
            ("origin", "5 5 5"),
        ]));
        let out = sp_entities_for_iw4_rules(&text);
        let blocks = read_blocks(&out);
        assert!(
            blocks
                .iter()
                .all(|block| get(block, "targetname") != Some("animated_model"))
        );
        assert_eq!(
            blocks
                .iter()
                .filter(|block| get(block, "targetname") == Some("sp_animated_model"))
                .count(),
            1
        );
    }

    #[test]
    fn a_care_package_is_supplied_once() {
        let out = sp_entities_for_iw4_rules(&campaign(&[]));
        assert_eq!(count(&out, "script_brushmodel"), 1);
        let again = sp_entities_for_iw4_rules(&out);
        assert_eq!(count(&again, "script_brushmodel"), 1);
    }

    #[test]
    fn a_map_with_no_start_and_no_nodes_is_left_without_spawns() {
        let text = entity(&[("classname", "worldspawn")]);
        let out = sp_entities_for_iw4_rules(&text);
        assert!(dm_spawn_points_in(&out).is_empty());
        assert!(parse_intermission_view(&out).is_none());
    }
}
