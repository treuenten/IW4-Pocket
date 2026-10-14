use crate::overhead_names::{
    PartyRelation, RelativeTeamColorChoice, RelativeTeamColorKey, relative_team_color_key,
};

/// `g_ScoresColor_MyTeam`, the default the scripts leave alone.
pub const SCORES_COLOR_MY_TEAM: [f32; 3] = [0.4, 0.6, 0.85];
/// `g_ScoresColor_EnemyTeam`.
pub const SCORES_COLOR_ENEMY_TEAM: [f32; 3] = [0.75, 0.25, 0.25];
/// `g_ScoresColor_MyParty`.
pub const SCORES_COLOR_MY_PARTY: [f32; 3] = [0.4, 0.4, 0.9];
/// `g_ScoresColor_Spectator` (`_teams.gsc` sets the same value).
pub const SCORES_COLOR_SPECTATOR: [f32; 3] = [0.25, 0.25, 0.25];
/// A row's background is drawn at half the colour's alpha.
pub const SCORES_ROW_ALPHA: f32 = 0.5;

/// The background of one scoreboard row. With `useRelativeTeamColors` on (its
/// default) MW2 colours rows by their relation to the local player, not by
/// faction: your own row and your teammates' are blue, everyone you play
/// against is red, so in free-for-all only your row is blue. There are no
/// parties here, so a teammate is never "my party".
pub fn scoreboard_row_color(
    local_client: i32,
    local_team: i32,
    row_client: i32,
    row_team: i32,
) -> [f32; 4] {
    let key = match relative_team_color_key(
        local_client,
        local_team,
        row_client,
        row_team,
        PartyRelation::DifferentParty,
    ) {
        RelativeTeamColorChoice::Key(key) => key,
        RelativeTeamColorChoice::PartyUnknown => RelativeTeamColorKey::MyTeam,
    };
    let [r, g, b] = match key {
        RelativeTeamColorKey::MyTeam => SCORES_COLOR_MY_TEAM,
        RelativeTeamColorKey::EnemyTeam => SCORES_COLOR_ENEMY_TEAM,
        RelativeTeamColorKey::MyParty => SCORES_COLOR_MY_PARTY,
        RelativeTeamColorKey::Spectator => SCORES_COLOR_SPECTATOR,
    };
    [r, g, b, SCORES_ROW_ALPHA]
}

/// `cg_scoreboardHeaderFontScale`: the column headers keep this size however
/// many rows the board holds.
pub const SCOREBOARD_HEADER_SCALE: f32 = 0.35;
/// Space kept clear on each side of a column header.
pub const SCOREBOARD_HEADER_MARGIN: f32 = 2.0;

/// The scale and left edge of a header centred in a column `width` wide that
/// starts at `x`, given the label's width at [`SCOREBOARD_HEADER_SCALE`]. A
/// label too wide for its column is shrunk to fit inside the margins, so
/// neighbouring headers never touch.
pub fn scoreboard_header_fit(x: f32, width: f32, label_width: f32) -> (f32, f32) {
    let room = (width - 2.0 * SCOREBOARD_HEADER_MARGIN).max(1.0);
    let fit = if label_width > room {
        room / label_width
    } else {
        1.0
    };
    let drawn = label_width * fit;
    (SCOREBOARD_HEADER_SCALE * fit, x + (width - drawn) * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE: [f32; 4] = [0.4, 0.6, 0.85, 0.5];
    const RED: [f32; 4] = [0.75, 0.25, 0.25, 0.5];

    #[test]
    fn free_for_all_shows_only_your_row_blue() {
        assert_eq!(scoreboard_row_color(0, 0, 0, 0), BLUE);
        assert_eq!(scoreboard_row_color(0, 0, 5, 0), RED);
    }

    #[test]
    fn team_rows_are_coloured_by_side_not_by_faction() {
        // Local client 3 on allies (2).
        assert_eq!(scoreboard_row_color(3, 2, 3, 2), BLUE);
        assert_eq!(scoreboard_row_color(3, 2, 7, 2), BLUE);
        assert_eq!(scoreboard_row_color(3, 2, 8, 1), RED);
        assert_eq!(
            scoreboard_row_color(3, 2, 9, 3),
            [0.25, 0.25, 0.25, 0.5],
            "spectators stay grey"
        );
        // The same board seen from axis swaps the colours.
        assert_eq!(scoreboard_row_color(8, 1, 7, 2), RED);
        assert_eq!(scoreboard_row_color(8, 1, 8, 1), BLUE);
    }

    #[test]
    fn headers_never_touch_their_neighbours() {
        let width = 50.0;
        // Narrow and wide labels side by side, the wide ones wider than
        // their whole column.
        let labels = [30.0, 24.0, 61.0, 49.0, 80.0];
        let mut last_right = f32::NEG_INFINITY;
        for (i, label) in labels.into_iter().enumerate() {
            let x = 100.0 + width * i as f32;
            let (scale, left) = scoreboard_header_fit(x, width, label);
            let drawn = label * scale / SCOREBOARD_HEADER_SCALE;
            assert!(scale <= SCOREBOARD_HEADER_SCALE);
            assert!(left >= x + SCOREBOARD_HEADER_MARGIN - 1e-3, "label {i}");
            assert!(left + drawn <= x + width - SCOREBOARD_HEADER_MARGIN + 1e-3);
            assert!(left - last_right >= 2.0 * SCOREBOARD_HEADER_MARGIN - 1e-3);
            last_right = left + drawn;
        }
        // A label that fits keeps the full header scale.
        assert_eq!(scoreboard_header_fit(0.0, 50.0, 30.0), (0.35, 10.0));
    }
}
