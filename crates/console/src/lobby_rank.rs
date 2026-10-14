//! The local player's rank in the private-match lobby.
//!
//! In a match the rank comes from the saved stats: `_rank.gsc` reads the
//! account's experience and prestige and calls `setRank`, which the scoreboard
//! shows. The lobby has no match scripts and no stats schema loaded, so it
//! shows the rank the last match set, kept in `profile/rank.txt` across
//! launches. Until a match has set one, the lobby shows no rank.

use std::path::PathBuf;

use bevy::prelude::*;
use frame::UiMenuDvars;

const HEADER: &str = "iw4l-rank-1";
/// The level shown on the local lobby row; empty hides it.
pub(crate) const RANK_DVAR: &str = "ui_lobby_rank";
/// The rank icon beside it; empty hides it.
pub(crate) const RANK_ICON_DVAR: &str = "ui_lobby_rank_icon";

#[derive(Resource, Default)]
pub(crate) struct LobbyRank {
    loaded: bool,
    path: Option<PathBuf>,
    /// (rank id, prestige) as `setRank` last set them.
    known: Option<(i32, i32)>,
    written: Option<(i32, i32)>,
    retry_at: Option<std::time::Instant>,
}

fn parse(text: &str) -> Option<(i32, i32)> {
    let mut lines = text.lines();
    if lines.next()?.trim() != HEADER {
        return None;
    }
    let mut fields = lines.next()?.split_whitespace();
    let rank = fields.next()?.parse().ok()?;
    let prestige = fields.next()?.parse().ok()?;
    (rank >= 0 && prestige >= 0 && fields.next().is_none()).then_some((rank, prestige))
}

fn format(rank: i32, prestige: i32) -> String {
    format!("{HEADER}\n{rank} {prestige}\n")
}

pub(crate) fn sync_lobby_rank(
    identity: Option<Res<frame::LaunchIdentity>>,
    (has_world, role): (
        Option<Res<frame::HasWorld>>,
        Option<Res<frame::RuntimeRole>>,
    ),
    presented: Option<Res<net::PresentedSnapshot>>,
    local: Option<Res<net::LocalPresentClient>>,
    catalog: Option<Res<asset_game::MenuCatalog>>,
    mut state: ResMut<LobbyRank>,
    dvars: Option<ResMut<UiMenuDvars>>,
) {
    if !state.loaded
        && let Some(identity) = identity.as_deref()
        && !identity.artifacts.as_os_str().is_empty()
    {
        state.loaded = true;
        let path = identity.artifacts.join("profile/rank.txt");
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse(&text) {
                Some(rank) => {
                    state.known = Some(rank);
                    state.written = Some(rank);
                }
                None => diag::warn!(Ui, "lobby rank: invalid {}; ignored", path.display()),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => diag::warn!(Ui, "lobby rank: cannot read {}: {error}", path.display()),
        }
        state.path = Some(path);
    }

    // A player who has spawned has had `setRank` run on connect.
    if has_world.is_some_and(|world| world.0)
        && role.is_none_or(|role| *role != frame::RuntimeRole::Replay)
        && let (Some(presented), Some(local)) = (presented.as_deref(), local.as_deref())
        && let Some(meta) = presented
            .snapshot()
            .and_then(|snapshot| snapshot.meta.for_client(local.0))
        && matches!(
            meta.lifecycle,
            sim::ClientLifecycle::Alive
                | sim::ClientLifecycle::Dead
                | sim::ClientLifecycle::RespawnPending
        )
        && meta.rank >= 0
        && meta.prestige >= 0
    {
        state.known = Some((meta.rank, meta.prestige));
    }

    save(&mut state);

    let Some(mut dvars) = dvars else {
        return;
    };
    let (level, icon) = state
        .known
        .zip(catalog.as_deref())
        .and_then(|((rank, prestige), catalog)| hud::rank_presentation(catalog, rank, prestige))
        .map_or((String::new(), String::new()), |(icon, level)| {
            (level.to_owned(), icon.to_owned())
        });
    if dvars.get(RANK_DVAR) != Some(level.as_str()) {
        dvars.set(RANK_DVAR, level);
    }
    if dvars.get(RANK_ICON_DVAR) != Some(icon.as_str()) {
        dvars.set(RANK_ICON_DVAR, icon);
    }
}

fn save(state: &mut LobbyRank) {
    let Some((rank, prestige)) = state.known else {
        return;
    };
    if state.written == state.known
        || state
            .retry_at
            .is_some_and(|at| std::time::Instant::now() < at)
    {
        return;
    }
    let Some(path) = state.path.clone() else {
        return;
    };
    let pending = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let mut file = std::fs::File::create(&pending)?;
        file.write_all(format(rank, prestige).as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&pending, &path)
    })();
    match result {
        Ok(()) => {
            state.written = state.known;
            state.retry_at = None;
        }
        Err(error) => {
            let _ = std::fs::remove_file(pending);
            state.retry_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            diag::warn!(Ui, "lobby rank: cannot write {}: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_rank_reads_back() {
        assert_eq!(parse(&format(21, 0)), Some((21, 0)));
        assert_eq!(parse(&format(69, 10)), Some((69, 10)));
        assert_eq!(parse("iw4l-rank-1\n-1 0\n"), None);
        assert_eq!(parse("iw4l-rank-2\n21 0\n"), None);
        assert_eq!(parse("iw4l-rank-1\n21\n"), None);
        assert_eq!(parse(""), None);
    }
}
