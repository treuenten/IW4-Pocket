pub(crate) const RADIUS_AREA_HALF_SCALE: f32 = f32::from_bits(0x3fb5_04f3);

pub fn radius_damage_distance_to_aabb(origin: [f32; 3], center: [f32; 3], half: [f32; 3]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..3 {
        let mut axis = (origin[i] - center[i]).abs() - half[i];
        if axis < 0.0 {
            axis = 0.0;
        }
        sum += axis * axis;
    }
    libm::sqrtf(sum)
}

pub fn radius_damage_area_half_extent(radius: f32) -> f32 {
    let radius = if radius < 1.0 { 1.0 } else { radius };
    radius * RADIUS_AREA_HALF_SCALE
}

pub fn radius_damage_amount(inner: f32, outer: f32, radius: f32, dist: f32, vis_scale: f32) -> i32 {
    let radius = if radius < 1.0 { 1.0 } else { radius };
    if !(dist * dist < radius * radius) || vis_scale <= 0.0 {
        return 0;
    }
    let falloff = outer + (1.0 - dist / radius) * (inner - outer);
    libm::roundf(falloff * vis_scale) as i32
}

pub const G_CAN_DAMAGE_CONTENTS_MASK: u32 = 0x802011;

pub(crate) const G_CAN_DAMAGE_HALF_WIDTH: f32 = 15.0;

pub(crate) const G_CAN_DAMAGE_HALF_HEIGHT_SCALE: f32 = 0.5;

pub(crate) const G_CAN_DAMAGE_PARTIAL_DIVISOR: f32 = 3.0;

pub(crate) fn can_damage_player_sample_points(
    origin: [f32; 3],
    view_height: f32,
    right: [f32; 3],
) -> [[f32; 3]; 5] {
    let eye = [origin[0], origin[1], origin[2] + view_height];
    let half_height = (eye[2] - origin[2]) * G_CAN_DAMAGE_HALF_HEIGHT_SCALE;
    let mid = [
        (origin[0] + eye[0]) * 0.5,
        (origin[1] + eye[1]) * 0.5,
        (origin[2] + eye[2]) * 0.5,
    ];
    [
        mid,
        [
            mid[0] + right[0] * G_CAN_DAMAGE_HALF_WIDTH,
            mid[1] + right[1] * G_CAN_DAMAGE_HALF_WIDTH,
            mid[2] + right[2] * G_CAN_DAMAGE_HALF_WIDTH,
        ],
        [
            mid[0] - right[0] * G_CAN_DAMAGE_HALF_WIDTH,
            mid[1] - right[1] * G_CAN_DAMAGE_HALF_WIDTH,
            mid[2] - right[2] * G_CAN_DAMAGE_HALF_WIDTH,
        ],
        [mid[0], mid[1], mid[2] + half_height],
        [mid[0], mid[1], mid[2] - half_height],
    ]
}

pub(crate) fn can_damage_hits_to_scale(hits: u32) -> f32 {
    if hits == 0 {
        0.0
    } else if hits < 4 {
        hits as f32 / G_CAN_DAMAGE_PARTIAL_DIVISOR
    } else {
        1.0
    }
}

pub fn can_damage_player_vis_scale(
    origin: [f32; 3],
    view_height: f32,
    right: [f32; 3],
    inflictor: [f32; 3],
    mut trace_passed: impl FnMut([f32; 3], [f32; 3]) -> bool,
) -> f32 {
    let samples = can_damage_player_sample_points(origin, view_height, right);
    let mut hits = 0u32;
    for sample in samples {
        if trace_passed(inflictor, sample) {
            hits += 1;
        }
    }
    can_damage_hits_to_scale(hits)
}

/// MW2's `shieldBlastDamageProtection_30/_60/_120/_180`: how much of an
/// explosion a riot shield takes off, by how squarely the shield faces it.
/// Each entry is the cosine the blast's direction must be above (half of
/// the arc in the dvar's name) and the share of the damage taken off. The
/// thresholds and the dvar defaults are iw4mp.exe's; no MW2 config or
/// script changes the dvars.
pub const SHIELD_BLAST_PROTECTION: [(f32, f32); 4] =
    [(0.966, 0.95), (0.866, 0.80), (0.5, 0.55), (0.0, 0.15)];

/// How squarely a riot shield hanging on a tag at `tag_origin`, whose
/// forward axis is `tag_forward`, faces a blast at `blast`: the cosine of
/// the angle between that axis and the direction to the blast. MW2 measures
/// it from the tag the shield model is attached to: `tag_weapon_left` while
/// the shield is held, `tag_shield_back` while it is on the back.
pub fn shield_blast_dot(blast: [f32; 3], tag_origin: [f32; 3], tag_forward: [f32; 3]) -> f32 {
    let mut dir = [
        blast[0] - tag_origin[0],
        blast[1] - tag_origin[1],
        blast[2] - tag_origin[2],
    ];
    let len = libm::sqrtf(dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]);
    if len != 0.0 {
        dir = dir.map(|c| c / len);
    }
    dir[0] * tag_forward[0] + dir[1] * tag_forward[1] + dir[2] * tag_forward[2]
}

/// The share of a blast a riot shield takes off when it faces the blast as
/// squarely as `dot` (see `shield_blast_dot`): none beside or behind it.
pub fn shield_blast_protection(dot: f32) -> f32 {
    SHIELD_BLAST_PROTECTION
        .iter()
        .find(|(above, _)| dot > *above)
        .map_or(0.0, |&(_, protection)| protection)
}

/// The visibility scale a blast's damage is multiplied by once a shield
/// taking off `protection` of it is counted, as MW2's G_RadiusDamage does
/// before turning the damage into a whole number.
pub fn shield_blast_vis_scale(vis_scale: f32, protection: f32) -> f32 {
    vis_scale * (1.0 - protection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protect(blast: [f32; 3]) -> f32 {
        shield_blast_protection(shield_blast_dot(blast, [0.0; 3], [1.0, 0.0, 0.0]))
    }

    fn at_angle(degrees: f32) -> f32 {
        let r = degrees.to_radians();
        protect([100.0 * r.cos(), 100.0 * r.sin(), 0.0])
    }

    #[test]
    fn protection_follows_the_four_arcs() {
        assert_eq!(at_angle(0.0), 0.95);
        assert_eq!(at_angle(14.0), 0.95);
        assert_eq!(at_angle(16.0), 0.80);
        assert_eq!(at_angle(29.0), 0.80);
        assert_eq!(at_angle(31.0), 0.55);
        assert_eq!(at_angle(-59.0), 0.55);
        assert_eq!(at_angle(61.0), 0.15);
        assert_eq!(at_angle(-89.0), 0.15);
        assert_eq!(at_angle(91.0), 0.0);
        assert_eq!(at_angle(180.0), 0.0);
    }

    #[test]
    fn a_blast_on_the_tag_itself_gets_no_protection() {
        assert_eq!(shield_blast_dot([5.0; 3], [5.0; 3], [1.0, 0.0, 0.0]), 0.0);
        assert_eq!(shield_blast_protection(0.0), 0.0);
    }

    #[test]
    fn height_counts_in_the_angle() {
        // Straight above the tag is 90 degrees off its forward axis.
        assert_eq!(protect([0.0, 0.0, 50.0]), 0.0);
        // 45 degrees up and straight ahead is inside the 120-degree arc.
        assert_eq!(protect([50.0, 0.0, 50.0]), 0.55);
    }

    #[test]
    fn c4_at_the_shield_face_does_a_twentieth() {
        // C4 is 200 close up; facing it squarely leaves 5% with every trace clear.
        let vis = shield_blast_vis_scale(1.0, at_angle(0.0));
        assert_eq!(radius_damage_amount(200.0, 50.0, 256.0, 0.0, vis), 10);
        // Behind the shield's back the full blast lands.
        let vis = shield_blast_vis_scale(1.0, at_angle(180.0));
        assert_eq!(radius_damage_amount(200.0, 50.0, 256.0, 0.0, vis), 200);
    }
}
