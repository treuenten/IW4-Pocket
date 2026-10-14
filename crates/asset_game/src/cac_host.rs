#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacHostSlot {
    Primary,
    Secondary,
    Lethal,
    Tactical,
    Perk1,
    Perk2,
    Perk3,
    Deathstreak,
}

/// What a custom class can hold in each slot under MW2's rules: the items of
/// MW2's own class editor (ui_mp's popup_cac_* menus), which are the names
/// its class script accepts (patch_mp.ff's maps/mp/gametypes/_class.gsc,
/// isValidPrimary … isValidDeathStreak). giveLoadout swaps any other value for
/// the default class's (see [`mw2_default`]) without a word, so a class
/// holding one plays differently from what it shows.
///
/// MW2 has no "none" perk or deathstreak: its menus offer none, and the script
/// gives the default class's in place of `specialty_null`. Dead Man's Hand
/// (`specialty_c4death`) is in the perk table but cut: no menu offers it, the
/// unlock table puts it at level 9999 and the script refuses it. The gold
/// Desert Eagle is in the weapon table but in no menu and not in the unlock
/// table, and the script gives the USP in its place.
///
/// The tactical row also holds `flare_mp`, this port's Tactical Insertion,
/// which the script receives as a smoke grenade plus
/// `specialty_tacticalinsertion`.
pub fn cac_host_roster(row: CacHostSlot) -> &'static [&'static str] {
    match row {
        CacHostSlot::Primary => &[
            "m4_mp",
            "famas_mp",
            "scar_mp",
            "tavor_mp",
            "fal_mp",
            "m16_mp",
            "masada_mp",
            "fn2000_mp",
            "ak47_mp",
            "mp5k_mp",
            "ump45_mp",
            "kriss_mp",
            "p90_mp",
            "uzi_mp",
            "sa80_mp",
            "rpd_mp",
            "mg4_mp",
            "aug_mp",
            "m240_mp",
            "cheytac_mp",
            "barrett_mp",
            "wa2000_mp",
            "m21_mp",
            "riotshield_mp",
        ],
        CacHostSlot::Secondary => &[
            "pp2000_mp",
            "glock_mp",
            "beretta393_mp",
            "tmp_mp",
            "spas12_mp",
            "aa12_mp",
            "striker_mp",
            "ranger_mp",
            "m1014_mp",
            "model1887_mp",
            "usp_mp",
            "coltanaconda_mp",
            "beretta_mp",
            "deserteagle_mp",
            "at4_mp",
            "m79_mp",
            "stinger_mp",
            "javelin_mp",
            "rpg_mp",
        ],
        CacHostSlot::Lethal => &[
            "frag_grenade_mp",
            "semtex_mp",
            "throwingknife_mp",
            "claymore_mp",
            "c4_mp",
        ],
        CacHostSlot::Tactical => &[
            "flash_grenade_mp",
            "concussion_grenade_mp",
            "smoke_grenade_mp",
            "flare_mp",
        ],
        CacHostSlot::Perk1 => &[
            "specialty_marathon",
            "specialty_fastreload",
            "specialty_scavenger",
            "specialty_onemanarmy",
            "specialty_bling",
        ],
        CacHostSlot::Perk2 => &[
            "specialty_bulletdamage",
            "specialty_lightweight",
            "specialty_hardline",
            "specialty_coldblooded",
            "specialty_explosivedamage",
        ],
        CacHostSlot::Perk3 => &[
            "specialty_extendedmelee",
            "specialty_bulletaccuracy",
            "specialty_localjammer",
            "specialty_heartbreaker",
            "specialty_detectexplosive",
            "specialty_pistoldeath",
        ],
        CacHostSlot::Deathstreak => &[
            "specialty_grenadepulldeath",
            "specialty_combathigh",
            "specialty_finalstand",
            "specialty_copycat",
        ],
    }
}

/// Whether MW2's class rules let `name` (a perk, or a weapon's script name
/// such as `deserteagle_mp`) stand in `slot`.
pub fn mw2_accepts(slot: CacHostSlot, name: &str) -> bool {
    cac_host_roster(slot)
        .iter()
        .any(|own| own.eq_ignore_ascii_case(name))
}

/// What giveLoadout plays in a slot whose stored value MW2's script refuses:
/// that slot of mp/classTable.csv's default class (CLASS_DEFAULT, class 10).
pub fn mw2_default(slot: CacHostSlot) -> &'static str {
    match slot {
        CacHostSlot::Primary => "m4_mp",
        CacHostSlot::Secondary => "usp_mp",
        CacHostSlot::Lethal => "frag_grenade_mp",
        CacHostSlot::Tactical => "concussion_grenade_mp",
        CacHostSlot::Perk1 => "specialty_fastreload",
        CacHostSlot::Perk2 => "specialty_bulletdamage",
        CacHostSlot::Perk3 => "specialty_bulletaccuracy",
        CacHostSlot::Deathstreak => "specialty_copycat",
    }
}

const IW4_WEAPON_PREFIX: &str = "iw4:weapon/";

/// The value a saved class field moves to so that it holds what MW2 plays,
/// or `None` when MW2 plays it as stored. Perk rows hold perk names; weapon
/// rows hold asset keys, and only MW2's own (`iw4:`) weapons are MW2's
/// script's to judge: other games' weapons reach it as stand-ins. An empty
/// weapon row is left alone (the class editor never stores one).
///
/// A rejected field takes the default class's value, as the script does,
/// except the gold Desert Eagle: no MW2 class could hold it, and the player
/// who picked it picked the Desert Eagle, so it becomes that.
pub fn mw2_settled_field(slot: CacHostSlot, stored: &str) -> Option<String> {
    let perk = matches!(
        slot,
        CacHostSlot::Perk1 | CacHostSlot::Perk2 | CacHostSlot::Perk3 | CacHostSlot::Deathstreak
    );
    if perk {
        return (!mw2_accepts(slot, stored)).then(|| mw2_default(slot).to_owned());
    }
    let name = stored.strip_prefix(IW4_WEAPON_PREFIX)?;
    if mw2_accepts(slot, name) {
        return None;
    }
    let settled = match (slot, name) {
        (CacHostSlot::Secondary, "deserteaglegold_mp") => "deserteagle_mp",
        _ => mw2_default(slot),
    };
    Some(format!("{IW4_WEAPON_PREFIX}{settled}"))
}

/// Whether a weapon in this slot can carry a camouflage MW2 shows. MW2's
/// class editor opens camouflage for the primary only, picking a secondary
/// resets its camo to none, and the script plays every secondary plain.
pub fn mw2_takes_camo(slot: CacHostSlot, weapon_key: &str) -> bool {
    !(slot == CacHostSlot::Secondary && weapon_key.starts_with(IW4_WEAPON_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mw2_deathstreaks_are_the_four_its_menu_offers() {
        let mut offered = cac_host_roster(CacHostSlot::Deathstreak).to_vec();
        offered.sort_unstable();
        assert_eq!(
            offered,
            [
                "specialty_combathigh",
                "specialty_copycat",
                "specialty_finalstand",
                "specialty_grenadepulldeath",
            ]
        );
        assert!(!mw2_accepts(CacHostSlot::Deathstreak, "specialty_c4death"));
        assert!(!mw2_accepts(CacHostSlot::Deathstreak, "specialty_null"));
    }

    #[test]
    fn no_perk_slot_offers_none() {
        for slot in [
            CacHostSlot::Perk1,
            CacHostSlot::Perk2,
            CacHostSlot::Perk3,
            CacHostSlot::Deathstreak,
        ] {
            assert!(!mw2_accepts(slot, "specialty_null"), "{slot:?}");
            assert!(!mw2_accepts(slot, ""), "{slot:?}");
            assert!(mw2_accepts(slot, mw2_default(slot)), "{slot:?}");
        }
    }

    #[test]
    fn a_saved_class_settles_on_what_mw2_plays() {
        let settle = mw2_settled_field;
        assert_eq!(
            settle(CacHostSlot::Deathstreak, "specialty_c4death").as_deref(),
            Some("specialty_copycat")
        );
        assert_eq!(
            settle(CacHostSlot::Deathstreak, "specialty_null").as_deref(),
            Some("specialty_copycat")
        );
        assert_eq!(
            settle(CacHostSlot::Deathstreak, "specialty_finalstand"),
            None
        );
        assert_eq!(
            settle(CacHostSlot::Perk1, "specialty_null").as_deref(),
            Some("specialty_fastreload")
        );
        assert_eq!(
            settle(CacHostSlot::Perk2, "").as_deref(),
            Some("specialty_bulletdamage")
        );
        assert_eq!(
            settle(CacHostSlot::Perk3, "specialty_null").as_deref(),
            Some("specialty_bulletaccuracy")
        );
        // A perk in another tier's slot is refused like any other.
        assert_eq!(
            settle(CacHostSlot::Perk1, "specialty_hardline").as_deref(),
            Some("specialty_fastreload")
        );
        assert_eq!(settle(CacHostSlot::Perk1, "specialty_bling"), None);
    }

    #[test]
    fn the_gold_desert_eagle_settles_on_the_desert_eagle() {
        assert_eq!(
            mw2_settled_field(CacHostSlot::Secondary, "iw4:weapon/deserteaglegold_mp").as_deref(),
            Some("iw4:weapon/deserteagle_mp")
        );
        assert_eq!(
            mw2_settled_field(CacHostSlot::Secondary, "iw4:weapon/deserteagle_mp"),
            None
        );
        assert_eq!(
            mw2_settled_field(CacHostSlot::Primary, "iw4:weapon/tavor_mp"),
            None
        );
        // Any other weapon MW2's class script refuses plays as the default class's.
        assert_eq!(
            mw2_settled_field(CacHostSlot::Primary, "iw4:weapon/tar21_mp").as_deref(),
            Some("iw4:weapon/m4_mp")
        );
        // Other games' weapons and an empty row are not MW2's to judge.
        assert_eq!(
            mw2_settled_field(CacHostSlot::Secondary, "t5:weapon/cz75_mp"),
            None
        );
        assert_eq!(mw2_settled_field(CacHostSlot::Secondary, ""), None);
    }

    #[test]
    fn only_mw2_secondaries_go_without_camouflage() {
        assert!(!mw2_takes_camo(
            CacHostSlot::Secondary,
            "iw4:weapon/spas12_mp"
        ));
        assert!(mw2_takes_camo(CacHostSlot::Primary, "iw4:weapon/aug_mp"));
        assert!(mw2_takes_camo(CacHostSlot::Secondary, "t6:weapon/judge_mp"));
    }
}
