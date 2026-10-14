use std::collections::HashMap;

use bevy::prelude::Resource;
use playerstate_iw4::{PlayerState, SeatFocus, apply_killcam_seat, rebase_archived_timers};
use sim::{ClientId, ClientLifecycle, Snapshot};

use crate::authority::inbox::AUTHORITY_MS;
use crate::player_state_to_entity_state;
use crate::transport::archive::{ARCHIVE_TICK_MS, ArchiveLookup, FrameArchive};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KillcamSession {
    pub archivetime_ms: i32,

    pub focus_client: ClientId,

    pub focus: SeatFocus,

    pub ends_at_ms: i32,

    pub final_kill: bool,

    pub started_at_ms: i32,

    pub killcamoffset_ms: i32,

    pub predelay_ms: i32,

    pub postdelay_ms: i32,

    pub recalc_pending: bool,

    pub entity_focus: Option<killcam_iw4::focus::Entity>,
}

#[derive(Resource, Clone, Debug, Default)]
pub struct ActiveKillcams {
    sessions: HashMap<ClientId, KillcamSession>,
}

impl ActiveKillcams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, viewer: ClientId) -> Option<&KillcamSession> {
        self.sessions.get(&viewer)
    }

    pub fn arm(&mut self, viewer: ClientId, session: KillcamSession) {
        self.sessions.insert(viewer, session);
    }

    pub fn clear(&mut self, viewer: ClientId) {
        self.sessions.remove(&viewer);
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn expire_clients(&mut self, now_ms: i32) -> Vec<ClientId> {
        let mut expired = Vec::new();
        self.sessions.retain(|id, s| {
            if now_ms < s.ends_at_ms {
                true
            } else {
                expired.push(*id);
                false
            }
        });
        expired
    }

    pub fn viewers(&self) -> Vec<ClientId> {
        self.sessions.keys().copied().collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SeatSample {
    pub player_state: PlayerState,
    pub lookup: ArchiveLookup,

    pub rebase_ms: i32,
}

pub fn sample_killcam_seat(
    archive: &FrameArchive,
    snapshot_now: &Snapshot,
    viewer: ClientId,
    session: &KillcamSession,
    now_ms: i32,
) -> Option<SeatSample> {
    if session.killcamoffset_ms == 0 {
        let target = snapshot_now
            .players
            .iter()
            .find(|(id, _)| *id == session.focus_client)?
            .1;
        // A live follow is the followed player's own state, as MW2's server
        // copies it to a spectator: their stance, eye height and gun, marked
        // as a follow (the bit a killcam seat sets too). The client then draws
        // it from their eyes with their viewmodel, hides their body, and does
        // not predict it from the spectator's own input.
        let mut player_state = target;
        player_state.other_flags = (target.other_flags & !playerstate_iw4::other_flags::PLAYER)
            | playerstate_iw4::other_flags::DEAD_KILLCAM_TPV;
        player_state.kill_cam_entity = playerstate_iw4::ENTITYNUM_NONE;
        player_state.kill_cam_look_at_entity = playerstate_iw4::ENTITYNUM_NONE;
        player_state.kill_cam_client_num = session.focus_client.0 as i32;
        player_state.delta_time = 0;
        return Some(SeatSample {
            player_state,
            lookup: ArchiveLookup {
                attained_ms: 0,
                tick: None,
            },
            rebase_ms: 0,
        });
    }
    let lookup = archive.lookup(session.archivetime_ms);
    if lookup.nothing_to_show() {
        return None;
    }
    let tick = lookup.tick?;
    let frame = archive.frame(tick)?;
    if !frame.player_state_exists(session.focus_client) {
        return None;
    }
    let archived = frame
        .snapshot
        .players
        .iter()
        .find(|(id, _)| *id == session.focus_client)
        .map(|(_, ps)| *ps)?;

    let viewer_ps = snapshot_now
        .players
        .iter()
        .find(|(id, _)| *id == viewer)
        .map(|(_, ps)| *ps)
        .unwrap_or(PlayerState::ZERO);

    let frame_base_ms = i32::try_from(tick.0).ok()?.saturating_mul(ARCHIVE_TICK_MS);
    let rebase_ms = now_ms.saturating_sub(frame_base_ms);

    let mut remapped = archived;
    rebase_archived_timers(&mut remapped, rebase_ms);
    let player_state = apply_killcam_seat(&viewer_ps, &remapped, session.focus);

    Some(SeatSample {
        player_state,
        lookup,
        rebase_ms,
    })
}

pub fn apply_seat_to_snapshot(
    archive: &FrameArchive,
    snapshot: &mut Snapshot,
    viewer: ClientId,
    session: &KillcamSession,
    now_ms: i32,
) -> Option<SeatSample> {
    let sample = sample_killcam_seat(archive, snapshot, viewer, session, now_ms)?;
    if let Some((_, ps)) = snapshot.players.iter_mut().find(|(id, _)| *id == viewer) {
        *ps = sample.player_state;
    } else {
        snapshot.players.push((viewer, sample.player_state));
    }
    Some(sample)
}

pub fn snapshot_for_viewer(
    archive: &FrameArchive,
    seats: &ActiveKillcams,
    live: &Snapshot,
    viewer: ClientId,
    now_ms: i32,
) -> Snapshot {
    snapshot_and_sample_for_viewer(archive, seats, live, viewer, now_ms).0
}

pub fn snapshot_override_for_viewer(
    archive: &FrameArchive,
    seats: &ActiveKillcams,
    live: &Snapshot,
    viewer: ClientId,
    now_ms: i32,
) -> Option<Snapshot> {
    let hidden_movers = live.meta.script_movers.iter().any(|mover| {
        mover.shown_to != 0 && mover.state.e_flags & entity_iw4::CG_SCRIPT_MOVER_NODRAW != 0
    });
    (hidden_movers || seat_applies(seats, live, viewer))
        .then(|| snapshot_for_viewer(archive, seats, live, viewer, now_ms))
}

/// A seat replaces the view of a viewer who is not alive. The end-of-match
/// killcam is the exception: it replays the archive to every player, so it
/// also covers the survivors (the winner watching his own winning kill).
fn seat_applies(seats: &ActiveKillcams, live: &Snapshot, viewer: ClientId) -> bool {
    seats.get(viewer).is_some_and(|session| {
        (session.final_kill && session.killcamoffset_ms > 0)
            || !live
                .meta
                .for_client(viewer)
                .is_some_and(|m| m.lifecycle == ClientLifecycle::Alive)
    })
}

pub fn snapshot_and_sample_for_viewer(
    archive: &FrameArchive,
    seats: &ActiveKillcams,
    live: &Snapshot,
    viewer: ClientId,
    now_ms: i32,
) -> (Snapshot, Option<SeatSample>) {
    let (mut out, sample) = seat_snapshot(archive, seats, live, viewer, now_ms);
    reveal_shown_movers(&mut out, viewer);
    (out, sample)
}

fn reveal_shown_movers(out: &mut Snapshot, viewer: ClientId) {
    let Some(bit) = 1u64.checked_shl(viewer.0) else {
        return;
    };
    let Snapshot { meta, .. } = out;
    for mover in &mut meta.script_movers {
        if mover.shown_to == 0 || mover.state.e_flags & entity_iw4::CG_SCRIPT_MOVER_NODRAW == 0 {
            continue;
        }
        if mover.shown_to & bit == 0 {
            meta.entity_dobjs
                .retain(|(owner, _)| owner.script_model() != Some(mover.id));
            continue;
        }
        let hidden = mover.state.clone();
        mover.state.e_flags &= !entity_iw4::CG_SCRIPT_MOVER_NODRAW;
        for es in meta.entities.iter_mut().filter(|es| **es == hidden) {
            *es = mover.state.clone();
        }
    }
}

fn seat_snapshot(
    archive: &FrameArchive,
    seats: &ActiveKillcams,
    live: &Snapshot,
    viewer: ClientId,
    now_ms: i32,
) -> (Snapshot, Option<SeatSample>) {
    let Some(session) = seats
        .get(viewer)
        .filter(|_| seat_applies(seats, live, viewer))
    else {
        return (live.clone(), None);
    };
    if session.killcamoffset_ms == 0 {
        let mut out = live.clone();
        let sample = apply_seat_to_snapshot(archive, &mut out, viewer, session, now_ms);
        return (out, sample);
    }
    let lookup = archive.lookup(session.archivetime_ms);
    let mut out = match lookup.tick.and_then(|tick| archive.frame(tick)) {
        Some(frame) => overlay_archived_world(live, &frame.snapshot, viewer, session.focus_client),
        None => live.clone(),
    };
    let sample = apply_seat_to_snapshot(archive, &mut out, viewer, session, now_ms);
    let archived = lookup.tick.and_then(|tick| archive.frame(tick));
    overlay_archived_cameras(
        &mut out,
        live,
        archived.map(|f| &f.snapshot),
        viewer,
        session.focus_client,
    );
    overlay_killcam_hud(
        &mut out,
        live,
        archived.map(|f| &f.snapshot),
        viewer,
        session,
        sample.as_ref().map(|s| s.rebase_ms).unwrap_or(0),
    );
    (out, sample)
}

fn overlay_archived_world(
    live: &Snapshot,
    archived: &Snapshot,
    viewer: ClientId,
    focus: ClientId,
) -> Snapshot {
    let mut out = archived.clone();
    out.meta
        .entity_events
        .retain(|record| record.audience.projects_to(focus));
    for record in &mut out.meta.entity_events {
        record.audience = sim::EventAudience::Client(viewer);
    }
    out.tick = live.tick;
    out.meta.phase = live.meta.phase;
    out.meta.match_elapsed_ms = live.meta.match_elapsed_ms;
    out.meta.prematch = live.meta.prematch;
    out.meta.score_limit = live.meta.score_limit;
    out.meta.time_limit_ms = live.meta.time_limit_ms;
    out.meta.kind = live.meta.kind;
    out.meta.sound_aliases = live.meta.sound_aliases.clone();
    out.meta.hud_strings = live.meta.hud_strings.clone();
    out.meta.hud_materials = live.meta.hud_materials.clone();

    let archived_viewer = archived
        .players
        .iter()
        .find(|(id, _)| *id == viewer)
        .map(|(_, ps)| *ps);
    if let Some(ps) = archived_viewer {
        let es = player_state_to_entity_state(viewer, &ps);
        if !out.meta.entities.iter().any(|e| e.number == es.number) {
            out.meta.entities.push(es);
        }
    }

    let viewer_row = live.players.iter().find(|(id, _)| *id == viewer).cloned();
    out.players.retain(|(id, _)| *id != viewer);
    if let Some(row) = viewer_row {
        out.players.push(row);
    }

    let viewer_meta = live
        .meta
        .clients
        .iter()
        .find(|(id, _)| *id == viewer)
        .cloned();
    out.meta.clients.retain(|(id, _)| *id != viewer);
    if let Some(row) = viewer_meta {
        out.meta.clients.push(row);
    }
    let delta_ms = sim::level_time_ms(live.tick).wrapping_sub(sim::level_time_ms(archived.tick));
    rebase_archived_world(&mut out, viewer, delta_ms);
    out
}

/// Remote cameras are per-client rows, not player state: the AC-130 and Chopper
/// Gunner weapon-view link and the Predator missile link. The replay must look
/// through the focus's archived camera, and the viewer's own live links must
/// not leak into it (the winner's gunship is gone by the end-of-match killcam).
fn overlay_archived_cameras(
    out: &mut Snapshot,
    live: &Snapshot,
    archived: Option<&Snapshot>,
    viewer: ClientId,
    focus: ClientId,
) {
    let focus_row = archived.and_then(|snap| snap.meta.for_client(focus));
    let delta_ms = archived.map_or(0, |snap| {
        sim::level_time_ms(live.tick).wrapping_sub(sim::level_time_ms(snap.tick))
    });
    let linked_weapon_view = focus_row.and_then(|meta| meta.linked_weapon_view);
    let remote_missile =
        focus_row
            .and_then(|meta| meta.remote_missile)
            .map(|link| sim::RemoteMissile {
                unlink_at_ms: link.unlink_at_ms.map(|ms| ms.wrapping_add(delta_ms)),
                ..link
            });
    if let Some((_, meta)) = out.meta.clients.iter_mut().find(|(id, _)| *id == viewer) {
        meta.linked_weapon_view = linked_weapon_view;
        meta.remote_missile = remote_missile;
    }
}

fn rebase_entity_state(es: &mut entity_iw4::EntityState, delta_ms: i32) {
    if es.tr_time != 0 {
        es.tr_time = es.tr_time.wrapping_add(delta_ms);
    }
    if es.apos_tr_time != 0 {
        es.apos_tr_time = es.apos_tr_time.wrapping_add(delta_ms);
    }
    if es.time2 != 0 {
        es.time2 = es.time2.wrapping_add(delta_ms);
    }
    // General's data[0] and missile launchTime share one union slot.
    if es.e_type == entity_iw4::ET_GENERAL || es.e_type == entity_iw4::ET_MISSILE {
        es.set_launch_time(es.launch_time().wrapping_add(delta_ms));
    }
}

fn rebase_archived_world(out: &mut Snapshot, viewer: ClientId, delta_ms: i32) {
    for es in &mut out.meta.entities {
        rebase_entity_state(es, delta_ms);
    }
    // Adopt requires each typed mover to equal its entityState row.
    for mover in &mut out.meta.script_movers {
        rebase_entity_state(&mut mover.state, delta_ms);
    }
    for p in &mut out.projectiles {
        if p.pos.tr_time != 0 {
            p.pos.tr_time = p.pos.tr_time.wrapping_add(delta_ms);
        }
        if p.apos.tr_time != 0 {
            p.apos.tr_time = p.apos.tr_time.wrapping_add(delta_ms);
        }
        p.launch_time = p.launch_time.wrapping_add(delta_ms);
        p.spawn_time_ms = p.spawn_time_ms.wrapping_add(delta_ms);
        p.detonate_at_ms = p.detonate_at_ms.map(|t| t.wrapping_add(delta_ms));
        p.cleanup_at_ms = p.cleanup_at_ms.wrapping_add(delta_ms);
    }
    // A body falls along its trajectory and keys its death animation on its
    // death time; both are the archived tick's level times.
    let bodies = out.meta.corpses.slots.iter_mut();
    for body in bodies.filter(|body| body.occupied) {
        if body.tr_time != 0 {
            body.tr_time = body.tr_time.wrapping_add(delta_ms);
        }
        if body.spawn_time_ms != 0 {
            body.spawn_time_ms = body.spawn_time_ms.wrapping_add(delta_ms);
        }
    }
    for (id, ps) in &mut out.players {
        if *id != viewer {
            rebase_archived_timers(ps, delta_ms);
        }
    }
}

fn overlay_killcam_hud(
    out: &mut Snapshot,
    live: &Snapshot,
    archived: Option<&Snapshot>,
    viewer: ClientId,
    session: &KillcamSession,
    rebase_ms: i32,
) {
    let current = live
        .meta
        .for_client(viewer)
        .map(|m| m.hud_current.clone())
        .unwrap_or_default();
    let wipe = session.focus.kill_cam_entity != playerstate_iw4::HITSCAN_KILL_CAM_ENTITY;
    let mut archival = if wipe {
        Vec::new()
    } else {
        archived
            .and_then(|snap| snap.meta.for_client(session.focus_client))
            .map(|m| m.hud_archival.clone())
            .unwrap_or_default()
    };
    sim::rebase_hud_archival(&mut archival, rebase_ms);
    if let Some((_, meta)) = out.meta.clients.iter_mut().find(|(id, _)| *id == viewer) {
        meta.killcam_hud = Some(sim::KillcamHud {
            final_kill: session.final_kill,
        });
        meta.hud_current = current;
        meta.hud_archival = archival;
    }
}

pub const fn killcam_seconds_to_ms(seconds: f32) -> i32 {
    let ms = seconds * 1000.0;
    let ticks = (ms / (AUTHORITY_MS as f32) + 0.5) as i32;
    ticks.saturating_mul(AUTHORITY_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::ClientLifecycle::{Alive, Dead};
    use sim::{ClientSnapshotMeta, LinkedWeaponView, RemoteMissile, Tick};

    const LIVE_TICK: u32 = 300;
    const ARCHIVE_MS: i32 = 10_050;

    fn snapshot(tick: u32, clients: &[(u32, ClientLifecycle)]) -> Snapshot {
        let mut snapshot = Snapshot::unpublished(Tick(tick));
        for &(id, lifecycle) in clients {
            snapshot.players.push((ClientId(id), PlayerState::ZERO));
            snapshot.meta.clients.push((
                ClientId(id),
                ClientSnapshotMeta {
                    lifecycle,
                    ..Default::default()
                },
            ));
        }
        snapshot
    }

    fn row(snapshot: &mut Snapshot, id: u32) -> &mut ClientSnapshotMeta {
        &mut snapshot
            .meta
            .clients
            .iter_mut()
            .find(|(client, _)| client.0 == id)
            .unwrap()
            .1
    }

    fn gunship(tick: u32) -> LinkedWeaponView {
        LinkedWeaponView {
            entity_num: 132,
            origin: [-5993.0, 2264.0, 4916.0 + tick as f32],
            angles: [48.3, 355.9, 0.0],
        }
    }

    fn missile(unlink_at_ms: i32) -> RemoteMissile {
        RemoteMissile {
            entnum: 140,
            unlink_at_ms: Some(unlink_at_ms),
            ..Default::default()
        }
    }

    /// Ticks before `LIVE_TICK`, with `camera` filling in the focus's
    /// remote cameras for each one.
    fn archive(focus: u32, camera: impl Fn(u32, &mut ClientSnapshotMeta)) -> FrameArchive {
        let mut archive = FrameArchive::new();
        for tick in 0..LIVE_TICK {
            let mut frame = snapshot(tick, &[(0, Alive), (1, Alive)]);
            camera(tick, row(&mut frame, focus));
            archive.push_snapshot(&frame, &[]);
        }
        archive
    }

    fn session(focus: u32, final_kill: bool) -> KillcamSession {
        KillcamSession {
            archivetime_ms: ARCHIVE_MS,
            focus_client: ClientId(focus),
            focus: SeatFocus::hitscan(focus as i32),
            ends_at_ms: i32::MAX,
            final_kill,
            started_at_ms: 0,
            killcamoffset_ms: ARCHIVE_MS,
            predelay_ms: 0,
            postdelay_ms: 0,
            recalc_pending: false,
            entity_focus: None,
        }
    }

    fn seated(viewer: u32, session: KillcamSession) -> ActiveKillcams {
        let mut seats = ActiveKillcams::new();
        seats.arm(ClientId(viewer), session);
        seats
    }

    #[test]
    fn the_final_killcam_reaches_a_viewer_who_is_still_alive() {
        let live = snapshot(LIVE_TICK, &[(0, Alive), (1, Alive)]);
        let dead = snapshot(LIVE_TICK, &[(0, Dead), (1, Alive)]);
        let viewer = ClientId(0);
        assert!(!seat_applies(&seated(0, session(1, false)), &live, viewer));
        assert!(seat_applies(&seated(0, session(1, true)), &live, viewer));
        assert!(seat_applies(&seated(0, session(1, false)), &dead, viewer));
        let live_follow = KillcamSession {
            killcamoffset_ms: 0,
            ..session(1, true)
        };
        assert!(!seat_applies(&seated(0, live_follow), &live, viewer));

        let archive = archive(1, |_, _| {});
        let now_ms = sim::level_time_ms(live.tick);
        let (out, sample) = snapshot_and_sample_for_viewer(
            &archive,
            &seated(0, session(1, true)),
            &live,
            viewer,
            now_ms,
        );
        let sample = sample.expect("the final seat applies to the survivor");
        let (_, ps) = out.players.iter().find(|(id, _)| *id == viewer).unwrap();
        assert!(!ps.is_live_frame());
        assert_eq!(ps.kill_cam_client_num, 1);
        assert_eq!(*ps, sample.player_state);
    }

    #[test]
    fn a_live_follow_is_the_followed_players_own_view() {
        let mut live = snapshot(LIVE_TICK, &[(0, Dead), (1, Alive)]);
        let target = PlayerState {
            pm_type: 0,
            other_flags: playerstate_iw4::other_flags::PLAYER,
            origin: [100.0, 200.0, 8.0],
            viewangles: [5.0, 90.0, 0.0],
            weapon: 7,
            ..PlayerState::ZERO
        };
        live.players[1].1 = target;
        let viewer = ClientId(0);
        let follow = KillcamSession {
            killcamoffset_ms: 0,
            archivetime_ms: 0,
            ..session(1, false)
        };
        let archive = archive(1, |_, _| {});
        let now_ms = sim::level_time_ms(live.tick);
        let (out, _) =
            snapshot_and_sample_for_viewer(&archive, &seated(0, follow), &live, viewer, now_ms);
        let (_, ps) = out.players.iter().find(|(id, _)| *id == viewer).unwrap();
        assert!(ps.is_live_follow());
        assert_eq!(
            ps.pm_type, target.pm_type,
            "their stance, not a spectator's"
        );
        assert_eq!(ps.other_flags & playerstate_iw4::other_flags::PLAYER, 0);
        assert_eq!(ps.kill_cam_client_num, 1);
        assert_eq!(
            (ps.origin, ps.viewangles, ps.weapon),
            (target.origin, target.viewangles, 7)
        );
    }

    #[test]
    fn a_replayed_body_runs_on_the_replay_clock() {
        let mut out = snapshot(LIVE_TICK, &[(0, Alive), (1, Dead)]);
        let slots = &mut out.meta.corpses.slots;
        slots[0].occupied = true;
        slots[0].tr_time = 4_000;
        slots[0].spawn_time_ms = 3_950;
        // A body at rest keeps its unset trajectory time.
        slots[1].occupied = true;
        slots[1].spawn_time_ms = 2_000;
        slots[2].tr_time = 1_000;
        rebase_archived_world(&mut out, ClientId(0), ARCHIVE_MS);
        let slots = &out.meta.corpses.slots;
        assert_eq!(slots[0].tr_time, 4_000 + ARCHIVE_MS);
        assert_eq!(slots[0].spawn_time_ms, 3_950 + ARCHIVE_MS);
        assert_eq!(slots[1].tr_time, 0);
        assert_eq!(slots[1].spawn_time_ms, 2_000 + ARCHIVE_MS);
        // An empty slot is left alone.
        assert_eq!(slots[2].tr_time, 1_000);
    }

    #[test]
    fn the_replay_looks_through_the_focus_archived_camera() {
        // The winner watching himself, and anyone else watching the winner.
        for (viewer, focus) in [(0, 0), (1, 0)] {
            let archive = archive(focus, |tick, meta| {
                meta.linked_weapon_view = Some(gunship(tick));
                meta.remote_missile = Some(missile(sim::level_time_ms(Tick(tick))));
            });
            let archived = archive.lookup(ARCHIVE_MS).tick.unwrap();
            let mut live = snapshot(LIVE_TICK, &[(0, Alive), (1, Alive)]);
            row(&mut live, viewer).linked_weapon_view = Some(gunship(LIVE_TICK));
            row(&mut live, viewer).score = 7;
            let now_ms = sim::level_time_ms(live.tick);
            let out = snapshot_for_viewer(
                &archive,
                &seated(viewer, session(focus, true)),
                &live,
                ClientId(viewer),
                now_ms,
            );
            let meta = out.meta.for_client(ClientId(viewer)).unwrap();
            assert_eq!(meta.linked_weapon_view, Some(gunship(archived.0)));
            assert_eq!(meta.remote_missile, Some(missile(now_ms)));
            // The rest of the row stays the viewer's live one.
            assert_eq!(meta.score, 7);
        }
    }

    #[test]
    fn a_replay_without_a_remote_camera_drops_the_viewer_live_one() {
        let archive = archive(1, |_, _| {});
        let mut live = snapshot(LIVE_TICK, &[(0, Alive), (1, Alive)]);
        row(&mut live, 0).linked_weapon_view = Some(gunship(LIVE_TICK));
        row(&mut live, 0).remote_missile = Some(missile(0));
        let now_ms = sim::level_time_ms(live.tick);
        let viewer = ClientId(0);
        let out = snapshot_for_viewer(
            &archive,
            &seated(0, session(1, true)),
            &live,
            viewer,
            now_ms,
        );
        let meta = out.meta.for_client(viewer).unwrap();
        assert_eq!(meta.linked_weapon_view, None);
        assert_eq!(meta.remote_missile, None);

        let out = snapshot_for_viewer(&archive, &ActiveKillcams::new(), &live, viewer, now_ms);
        let meta = out.meta.for_client(viewer).unwrap();
        assert_eq!(meta.linked_weapon_view, Some(gunship(LIVE_TICK)));
        assert_eq!(meta.remote_missile, Some(missile(0)));
    }
}
