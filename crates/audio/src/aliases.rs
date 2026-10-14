use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use movement_iw4::{SURFACE_TYPE_NAMES, surface_type_index};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepGait {
    Sprint,
    Run,
    Walk,
    Prone,
}

impl StepGait {
    pub const ALL: [Self; 4] = [Self::Sprint, Self::Run, Self::Walk, Self::Prone];

    fn index(self) -> usize {
        match self {
            Self::Sprint => 0,
            Self::Run => 1,
            Self::Walk => 2,
            Self::Prone => 3,
        }
    }
}

#[derive(Clone, Copy)]
struct SurfacePick {
    alias: &'static str,
    fallback: &'static str,
}

const SURF_N: usize = SURFACE_TYPE_NAMES.len();

struct SurfaceBank {
    footstep: [[[[SurfacePick; SURF_N]; 2]; 2]; 4],
    land: [[[SurfacePick; SURF_N]; 2]; 2],
    by_alias: HashMap<&'static str, &'static [&'static str]>,
    quiet: HashMap<&'static str, &'static str>,
    world: HashMap<&'static str, &'static str>,
    all_names: Vec<&'static str>,
}

fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn leak_slice(v: Vec<&'static str>) -> &'static [&'static str] {
    Box::leak(v.into_boxed_slice())
}

fn build_world_owned(alias: &str) -> Option<String> {
    let idx = alias.find("_plr_")?;
    let mut out = String::with_capacity(alias.len() - 4);
    out.push_str(&alias[..idx]);
    out.push('_');
    out.push_str(&alias[idx + 5..]);
    Some(out)
}

fn build_quiet_owned(alias: &str) -> Option<String> {
    if let Some(rest) = alias.strip_prefix("step_") {
        return Some(format!("qstep_{rest}"));
    }
    if let Some(rest) = alias.strip_prefix("Land_") {
        return Some(format!("qLand_{rest}"));
    }
    None
}

fn build_candidates_owned(alias: &str, fallback: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(8);
    let mut push = |name: String| {
        if !name.is_empty() && !out.iter().any(|existing| existing == &name) {
            out.push(name);
        }
    };
    for seed in [alias, fallback] {
        push(seed.to_owned());
        if let Some(world) = build_world_owned(seed) {
            push(world);
        }
        if let Some(quiet) = build_quiet_owned(seed) {
            if let Some(quiet_world) = build_world_owned(&quiet) {
                push(quiet.clone());
                push(quiet_world);
            } else {
                push(quiet);
            }
        }
    }
    out
}

fn intern_pick(
    alias: String,
    fallback: String,
    names: &mut HashSet<&'static str>,
    quiet: &mut HashMap<&'static str, &'static str>,
    world: &mut HashMap<&'static str, &'static str>,
    by_alias: &mut HashMap<&'static str, &'static [&'static str]>,
) -> SurfacePick {
    let candidates_owned = build_candidates_owned(&alias, &fallback);
    let alias = leak_str(alias);
    let fallback = leak_str(fallback);
    let mut leaked = Vec::with_capacity(candidates_owned.len());
    for name in candidates_owned {
        let interned = leak_str(name);
        names.insert(interned);
        leaked.push(interned);
        if let Some(q) = build_quiet_owned(interned) {
            quiet.insert(interned, leak_str(q));
        }
        if let Some(w) = build_world_owned(interned) {
            world.insert(interned, leak_str(w));
        }
    }
    let candidates = leak_slice(leaked);
    by_alias.insert(alias, candidates);
    SurfacePick { alias, fallback }
}

fn build_bank() -> SurfaceBank {
    let mut names = HashSet::new();
    let mut quiet = HashMap::new();
    let mut world = HashMap::new();
    let mut by_alias = HashMap::new();
    let dummy = intern_pick(
        "step_run_default".to_owned(),
        "step_run_default".to_owned(),
        &mut names,
        &mut quiet,
        &mut world,
        &mut by_alias,
    );
    let mut footstep = [[[[dummy; SURF_N]; 2]; 2]; 4];
    let mut land = [[[dummy; SURF_N]; 2]; 2];
    for (gi, gait) in StepGait::ALL.into_iter().enumerate() {
        for local in [false, true] {
            let li = usize::from(local);
            for quieter in [false, true] {
                let qi = usize::from(quieter);
                for (si, surface) in SURFACE_TYPE_NAMES.iter().enumerate() {
                    let prefix = step_prefix(gait, local);
                    let mut alias = format!("{prefix}_{surface}");
                    let mut fallback = format!("{prefix}_default");
                    if quieter {
                        alias = build_quiet_owned(&alias).unwrap_or(alias);
                        fallback = build_quiet_owned(&fallback).unwrap_or(fallback);
                    }
                    footstep[gi][li][qi][si] = intern_pick(
                        alias,
                        fallback,
                        &mut names,
                        &mut quiet,
                        &mut world,
                        &mut by_alias,
                    );
                }
            }
        }
    }
    for local in [false, true] {
        let li = usize::from(local);
        for quieter in [false, true] {
            let qi = usize::from(quieter);
            for (si, surface) in SURFACE_TYPE_NAMES.iter().enumerate() {
                let mut alias = if local {
                    format!("Land_plr_{surface}")
                } else {
                    format!("Land_{surface}")
                };
                let mut fallback = if local {
                    "Land_plr_default".to_owned()
                } else {
                    "Land_default".to_owned()
                };
                if quieter {
                    alias = build_quiet_owned(&alias).unwrap_or(alias);
                    fallback = build_quiet_owned(&fallback).unwrap_or(fallback);
                }
                land[li][qi][si] = intern_pick(
                    alias,
                    fallback,
                    &mut names,
                    &mut quiet,
                    &mut world,
                    &mut by_alias,
                );
            }
        }
    }
    for gait in StepGait::ALL {
        for local in [false, true] {
            names.insert(gear_rattle_alias(gait, local));
        }
    }
    names.insert(gear_alias(true));
    names.insert(gear_alias(false));
    let mut all_names: Vec<&'static str> = names.into_iter().collect();
    all_names.sort_unstable();
    SurfaceBank {
        footstep,
        land,
        by_alias,
        quiet,
        world,
        all_names,
    }
}

static BANK: LazyLock<SurfaceBank> = LazyLock::new(build_bank);

fn surf_slot(surface_flags: u32) -> usize {
    let index = surface_type_index(surface_flags);
    if index < SURFACE_TYPE_NAMES.len() {
        index
    } else {
        0
    }
}

fn footstep_pick(
    gait: StepGait,
    surface_flags: u32,
    local_player: bool,
    quieter: bool,
) -> SurfacePick {
    BANK.footstep[gait.index()][usize::from(local_player)][usize::from(quieter)]
        [surf_slot(surface_flags)]
}

fn land_pick(surface_flags: u32, local_player: bool, quieter: bool) -> SurfacePick {
    BANK.land[usize::from(local_player)][usize::from(quieter)][surf_slot(surface_flags)]
}

pub fn step_prefix(gait: StepGait, local_player: bool) -> &'static str {
    match (gait, local_player) {
        (StepGait::Sprint, true) => "step_sprint_plr",
        (StepGait::Sprint, false) => "step_sprint",
        (StepGait::Run, true) => "step_run_plr",
        (StepGait::Run, false) => "step_run",
        (StepGait::Walk, true) => "step_walk_plr",
        (StepGait::Walk, false) => "step_walk",
        (StepGait::Prone, true) => "step_prone_plr",
        (StepGait::Prone, false) => "step_prone",
    }
}

pub fn footstep_aliases(
    gait: StepGait,
    surface_flags: u32,
    local_player: bool,
    quieter: bool,
) -> (&'static str, &'static str) {
    let pick = footstep_pick(gait, surface_flags, local_player, quieter);
    (pick.alias, pick.fallback)
}

pub fn world_surface_alias(alias: &str) -> Option<&'static str> {
    BANK.world.get(alias).copied()
}

pub fn quiet_surface_alias(alias: &str) -> Option<&'static str> {
    BANK.quiet.get(alias).copied()
}

pub fn surface_alias_candidates(alias: &str, _fallback: &str) -> &'static [&'static str] {
    BANK.by_alias.get(alias).copied().unwrap_or(&[])
}

pub fn gear_rattle_alias(gait: StepGait, local_player: bool) -> &'static str {
    match (gait, local_player) {
        (StepGait::Sprint, true) => "gear_rattle_plr_sprint",
        (StepGait::Sprint, false) => "gear_rattle_sprint",
        (StepGait::Run, true) => "gear_rattle_plr_run",
        (StepGait::Run, false) => "gear_rattle_run",
        (StepGait::Walk, true) => "gear_rattle_plr_walk",
        (StepGait::Walk, false) => "gear_rattle_walk",
        (StepGait::Prone, true) => "gear_rattle_plr_prone",
        (StepGait::Prone, false) => "gear_rattle_prone",
    }
}

pub fn gear_alias(local_player: bool) -> &'static str {
    if local_player {
        "gear_rattle_plr_mantle"
    } else {
        "gear_rattle_mantle"
    }
}

pub fn land_aliases(
    surface_flags: u32,
    local_player: bool,
    quieter: bool,
) -> (&'static str, &'static str) {
    let pick = land_pick(surface_flags, local_player, quieter);
    (pick.alias, pick.fallback)
}

pub fn movement_prepare_names() -> &'static [&'static str] {
    BANK.all_names.as_slice()
}

pub fn select_fire_alias<'a>(
    player_view: bool,
    fire: Option<&'a str>,
    fire_player: Option<&'a str>,
) -> Option<&'a str> {
    if player_view {
        if let Some(p) = fire_player.filter(|s| !s.is_empty()) {
            return Some(p);
        }
    }
    fire.filter(|s| !s.is_empty())
}

pub fn select_cg_fire_alias<'a>(
    last_shot: bool,
    player_view: bool,
    fire: Option<&'a str>,
    fire_player: Option<&'a str>,
    fire_last: Option<&'a str>,
    fire_last_player: Option<&'a str>,
) -> Option<&'a str> {
    let current = select_fire_alias(player_view, fire, fire_player);
    if !last_shot {
        return current;
    }
    if player_view {
        if let Some(p) = fire_last_player.filter(|s| !s.is_empty()) {
            return Some(p);
        }
    }
    if let Some(w) = fire_last.filter(|s| !s.is_empty()) {
        return Some(w);
    }
    current
}

pub(crate) fn namespace_alias(alias: &str) -> (asset_core::AssetNamespace, &str) {
    alias
        .split_once(':')
        .and_then(|(ns, name)| asset_core::AssetNamespace::parse(ns).map(|ns| (ns, name)))
        .unwrap_or((asset_core::AssetNamespace::Iw4, alias))
}

pub(crate) fn match_ui_alias(
    namespace: asset_core::AssetNamespace,
    alias: &str,
) -> (asset_core::AssetNamespace, &str) {
    use asset_core::AssetNamespace;
    match (namespace, alias) {
        (AssetNamespace::T6, "ui_pulse_text_type") => (AssetNamespace::T6, "uin_notify_data"),
        (AssetNamespace::T6, "ui_pulse_text_delete") => (AssetNamespace::T6, "uin_alert_slideout"),
        _ => namespace_alias(alias),
    }
}

/// The surface sound sets a bullet impact plays from, one per row of the
/// impact effect table (`fx_iw4::impact_table_row`): MW2 names them
/// `<set>_<surface>` (`bullet_small_concrete`, `bulletspray_small_exit_wood`),
/// one alias per surface type, and the weapon's impact type picks the set.
/// Grenade, rocket and dud impacts are explosions with their own sounds.
const BULLET_IMPACT_SETS: [&str; 10] = [
    "bullet_small",
    "bullet_small_exit",
    "bullet_large",
    "bullet_large_exit",
    "bullet_explode",
    "bulletspray_small",
    "bulletspray_small_exit",
    "bulletspray_large",
    "bullet_ap",
    "bullet_ap_exit",
];

static BULLET_IMPACT_ALIASES: LazyLock<[[&'static str; SURF_N]; 10]> = LazyLock::new(|| {
    BULLET_IMPACT_SETS
        .map(|set| SURFACE_TYPE_NAMES.map(|surface| leak_str(format!("{set}_{surface}"))))
});

fn bullet_impact_set(impact_type: i32, exit: bool) -> Option<usize> {
    Some(match (impact_type, exit) {
        (1, false) => 0,
        (1, true) => 1,
        (2, false) => 2,
        (2, true) => 3,
        (4, _) => 4,
        (5, false) => 5,
        (5, true) => 6,
        (6, _) => 7,
        (3, false) => 8,
        (3, true) => 9,
        _ => return None,
    })
}

/// The alias a bullet impact plays on a surface, for the weapon's impact type
/// (`weapon_iw4` order: 1 small, 2 large, 3 armour-piercing, 4 explosive,
/// 5 shotgun, 6 explosive shotgun), and the set's `_default` alias for a
/// surface the set has no sound for (MW2 has no `bullet_small_snow`). `exit`
/// is the far side of a wall the bullet went through.
pub fn bullet_impact_alias(
    impact_type: i32,
    exit: bool,
    surface: u8,
) -> Option<(&'static str, &'static str)> {
    let set = bullet_impact_set(impact_type, exit)?;
    let aliases = &BULLET_IMPACT_ALIASES[set];
    Some((*aliases.get(usize::from(surface))?, aliases[0]))
}

#[cfg(test)]
mod bullet_impact_tests {
    use super::bullet_impact_alias;

    #[test]
    fn each_impact_type_names_its_surface_set() {
        assert_eq!(
            bullet_impact_alias(1, false, 5),
            Some(("bullet_small_concrete", "bullet_small_default"))
        );
        assert_eq!(
            bullet_impact_alias(1, true, 21),
            Some(("bullet_small_exit_wood", "bullet_small_exit_default"))
        );
        assert_eq!(
            bullet_impact_alias(2, false, 13),
            Some(("bullet_large_metal", "bullet_large_default"))
        );
        assert_eq!(
            bullet_impact_alias(3, true, 6),
            Some(("bullet_ap_exit_dirt", "bullet_ap_exit_default"))
        );
        assert_eq!(
            bullet_impact_alias(4, true, 7),
            Some(("bullet_explode_flesh", "bullet_explode_default"))
        );
        assert_eq!(
            bullet_impact_alias(5, false, 9),
            Some(("bulletspray_small_glass", "bulletspray_small_default"))
        );
        assert_eq!(
            bullet_impact_alias(6, false, 22),
            Some(("bulletspray_large_asphalt", "bulletspray_large_default"))
        );
        assert_eq!(
            bullet_impact_alias(1, false, 30),
            Some(("bullet_small_slush", "bullet_small_default"))
        );
    }

    #[test]
    fn explosions_and_unknown_surfaces_have_no_bullet_impact() {
        for impact_type in [0, 7, 8, 9, 10, 11] {
            assert_eq!(bullet_impact_alias(impact_type, false, 5), None);
        }
        assert_eq!(bullet_impact_alias(1, false, 31), None);
    }
}
