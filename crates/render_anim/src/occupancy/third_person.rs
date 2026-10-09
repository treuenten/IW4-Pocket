use asset_world::ClipCollision;
use net::{AUTHORITY_MS, PresentedProjectile, PresentedSnapshot};
use playerstate_iw4::{
    CG_CAMERA_PULLBACK_BOX_HALF, CG_CAMERA_PULLBACK_CLIPMASK, CG_THIRD_PERSON_RANGE_DEFAULT,
    KillCamMode, OffsetThirdPersonViewInputs, ThirdPersonViewInputs, is_third_person_view,
    offset_third_person_view,
};
use sim::{ClientId, LifeSequence, ProjectileId, RemoteMissile};

use render_scene::WorldCameraPose;

pub const CG_THIRD_PERSON_ANGLE_MP: f32 = 356.0;

pub fn presented_is_third_person(
    presented: &PresentedSnapshot,
    local: ClientId,
    in_killcam: bool,
    third_person: bool,
) -> bool {
    let Some(ps) = presented.player(local) else {
        return false;
    };
    if in_killcam && ps.kill_cam_entity != playerstate_iw4::ENTITYNUM_NONE {
        return true;
    }
    if remote_missile_link(presented, local).is_some() {
        return true;
    }
    is_third_person_view(ThirdPersonViewInputs {
        pm_type: ps.pm_type,
        other_flags: ps.other_flags,
        link_flags: ps.link_flags,
        cg_third_person: third_person && !in_killcam,
        in_killcam,
        killcam_mode: KillCamMode::Mode0,
    })
}

/// The local player's Predator link and the life it belongs to. It lasts from
/// ControlsLinkTo until the script's CameraUnlink, through the impact and
/// the static that follows it.
fn remote_missile_link(
    presented: &PresentedSnapshot,
    local: ClientId,
) -> Option<(RemoteMissile, LifeSequence)> {
    let meta = presented.snapshot()?.meta.for_client(local)?;
    Some((meta.remote_missile?, meta.life_sequence))
}

/// The Predator camera. MW2 keeps the camera on the missile until
/// CameraUnlink; here the missile leaves the snapshot on impact, a tick before
/// the script unlinks the controls and raises the static. The last pose shown
/// is held until the link goes, so neither the player's own view nor a black
/// frame shows in between.
#[derive(Default)]
pub struct RemoteMissileView {
    held: Option<HeldMissileView>,
}

#[derive(Clone, Copy, Debug)]
struct HeldMissileView {
    life: LifeSequence,
    projectile: ProjectileId,
    pose: WorldCameraPose,
}

impl RemoteMissileView {
    /// The missile camera at the presentation clock `render_time`.
    pub fn camera(
        &mut self,
        presented: &PresentedSnapshot,
        local: ClientId,
        render_time: i32,
        clip: Option<&ClipCollision>,
    ) -> Option<WorldCameraPose> {
        self.camera_traced(presented, local, render_time, |start, end| {
            clip.map_or(1.0, |clip| {
                let half = CG_CAMERA_PULLBACK_BOX_HALF;
                let hit = clip.sweep_box(
                    start,
                    end,
                    [-half; 3],
                    [half; 3],
                    CG_CAMERA_PULLBACK_CLIPMASK,
                );
                if hit.allsolid { 0.0 } else { hit.fraction }
            })
        })
    }

    fn camera_traced(
        &mut self,
        presented: &PresentedSnapshot,
        local: ClientId,
        render_time: i32,
        trace: impl Fn([f32; 3], [f32; 3]) -> f32,
    ) -> Option<WorldCameraPose> {
        // A Predator kill has no killcam entity, so its replay comes through
        // here too. A killcam seat draws an archived snapshot held back by the
        // proxy delay, and its trajectory time follows that snapshot; the live
        // clock would sit at least the delay past it, pin the eye a tick ahead
        // and move it only when a new snapshot arrives. Live play has no
        // trajectory sample and uses the clock.
        let at_time = presented.trajectory_time_ms(render_time);
        let Some((link, life)) = remote_missile_link(presented, local) else {
            self.held = None;
            return None;
        };
        self.held = self
            .held
            .filter(|held| held.life == life && held.projectile == link.projectile);
        let flying = link
            .unlink_at_ms
            .is_none()
            .then(|| {
                presented
                    .presented_projectiles()
                    .iter()
                    .find(|p| p.authoritative_id() == Some(link.projectile))
            })
            .flatten();
        let snapshot_ms = presented.tick().map_or(at_time, sim::level_time_ms);
        if let Some(missile) = flying {
            let pose = WorldCameraPose {
                origin: missile_eye(missile, snapshot_ms, at_time, trace),
                angles: link.angles,
            };
            self.held = Some(HeldMissileView {
                life,
                projectile: link.projectile,
                pose,
            });
        }
        self.held.map(|held| held.pose)
    }
}

/// Where the missile camera sits at `at_time`. The presented snapshot is the
/// last place the missile is known to be. The presentation time (the listen
/// clock live, the delayed replay time in a killcam seat) runs up to a tick
/// past it, so the flight carries on from there, but for one tick at most and
/// never through a surface: the missile detonates where it would cross one,
/// and the eye carried on would look at the void under the map.
fn missile_eye(
    missile: &PresentedProjectile,
    snapshot_ms: i32,
    at_time: i32,
    trace: impl Fn([f32; 3], [f32; 3]) -> f32,
) -> [f32; 3] {
    let known = missile.origin_at(at_time.min(snapshot_ms));
    if at_time <= snapshot_ms {
        return known;
    }
    let ahead = missile.origin_at(at_time.min(snapshot_ms.saturating_add(AUTHORITY_MS)));
    let fraction = trace(known, ahead).clamp(0.0, 1.0);
    std::array::from_fn(|i| known[i] + (ahead[i] - known[i]) * fraction)
}

pub fn linked_weapon_camera(
    presented: &PresentedSnapshot,
    local: ClientId,
) -> Option<WorldCameraPose> {
    let current = presented.snapshot()?.meta.for_client(local)?;
    let view = current.linked_weapon_view?;
    let previous = presented
        .interpolation_pair()
        .and_then(|(before, _, fraction)| {
            let meta = before.meta.for_client(local)?;
            (meta.life_sequence == current.life_sequence)
                .then_some((meta.linked_weapon_view?, fraction.clamp(0.0, 1.0)))
        });
    Some(match previous {
        Some((previous, fraction)) => WorldCameraPose {
            origin: std::array::from_fn(|i| {
                previous.origin[i] + (view.origin[i] - previous.origin[i]) * fraction
            }),
            angles: std::array::from_fn(|i| {
                previous.angles[i]
                    + math_iw4::angle_subtract(view.angles[i], previous.angles[i]) * fraction
            }),
        },
        None => WorldCameraPose {
            origin: view.origin,
            angles: view.angles,
        },
    })
}

pub fn third_person_camera(
    presented: &PresentedSnapshot,
    local: ClientId,
    clip: Option<&ClipCollision>,
) -> Option<WorldCameraPose> {
    let ps = presented.player(local)?;
    let offset = presented.view_offset();
    let yaw = presented
        .snapshot()?
        .meta
        .for_client(local)
        .map(|meta| meta.look_at_killer_yaw as f32)
        .unwrap_or(ps.viewangles[1]);

    let half = CG_CAMERA_PULLBACK_BOX_HALF;
    let trace = |start: [f32; 3], end: [f32; 3]| {
        let Some(clip) = clip else { return 1.0 };
        clip.sweep_box(
            start,
            end,
            [-half, -half, -half],
            [half, half, half],
            CG_CAMERA_PULLBACK_CLIPMASK,
        )
        .fraction
    };
    let view = offset_third_person_view(
        OffsetThirdPersonViewInputs {
            origin: [
                ps.origin[0] + offset[0],
                ps.origin[1] + offset[1],
                ps.origin[2] + offset[2],
            ],
            view_height_current: ps.view_height_current,
            viewangles: ps.viewangles,
            pm_type: ps.pm_type,
            look_at_killer_yaw: yaw,

            corpse_j_mainroot: None,
            other_flags: ps.other_flags,
            delta_time: ps.delta_time,
            cg_third_person_angle: if ps.pm_type > 7 {
                CG_THIRD_PERSON_ANGLE_MP
            } else {
                0.0
            },
            cg_third_person_range: CG_THIRD_PERSON_RANGE_DEFAULT,
        },
        trace,
    );
    Some(WorldCameraPose {
        origin: view.origin,
        angles: view.angles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use entity_iw4::{TR_LINEAR, Trajectory};
    use playerstate_iw4::PlayerState;
    use sim::{ClientSnapshotMeta, ProjectileState, Snapshot, Tick};

    const LOCAL: ClientId = ClientId(0);
    /// The newest snapshot, at 15 s of level time.
    const TICK: u32 = 300;

    fn ms(tick: u32) -> i32 {
        sim::level_time_ms(Tick(tick))
    }

    /// Diving straight down at 3000 u/s, 120 above the ground at `TICK`.
    fn rocket() -> ProjectileState {
        ProjectileState {
            id: ProjectileId(9),
            owner: LOCAL,
            owner_life: LifeSequence(1),
            weapon: 0,
            origin: [0.0, 0.0, 120.0],
            velocity: [0.0, 0.0, -3000.0],
            pos: Trajectory {
                tr_time: ms(TICK - 1),
                tr_type: TR_LINEAR,
                tr_duration: 0,
                tr_delta: [0.0, 0.0, -3000.0],
                tr_base: [0.0, 0.0, 270.0],
            },
            apos: Trajectory::default(),
            entnum: 140,
            launch_time: 0,
            spawn_time_ms: 0,
            detonate_at_ms: None,
            cleanup_at_ms: 60_000,
            travel_distance: 0.0,
            live: true,
            stuck_pane: None,
            grounded: false,
            guide: Default::default(),
            attached_to: None,
        }
    }

    fn link(unlink_at_ms: Option<i32>) -> RemoteMissile {
        RemoteMissile {
            projectile: ProjectileId(9),
            entnum: 140,
            angles: [80.0, 30.0, 0.0],
            unlink_at_ms,
            ..Default::default()
        }
    }

    fn presented(
        tick: u32,
        link: Option<RemoteMissile>,
        life: u32,
        missile: Option<ProjectileState>,
    ) -> PresentedSnapshot {
        let mut snapshot = Snapshot::unpublished(Tick(tick));
        snapshot.players.push((LOCAL, PlayerState::ZERO));
        snapshot.projectiles.extend(missile);
        snapshot.meta.clients.push((
            LOCAL,
            ClientSnapshotMeta {
                remote_missile: link,
                life_sequence: LifeSequence(life),
                ..Default::default()
            },
        ));
        let mut presented = PresentedSnapshot::default();
        presented.publish_decoded(snapshot);
        presented
    }

    /// The ground at z = 0, with the camera kept 4 units off it.
    fn ground(start: [f32; 3], end: [f32; 3]) -> f32 {
        if end[2] >= 4.0 {
            1.0
        } else {
            ((start[2] - 4.0) / (start[2] - end[2])).max(0.0)
        }
    }

    fn open(_: [f32; 3], _: [f32; 3]) -> f32 {
        1.0
    }

    #[track_caller]
    fn assert_near(got: [f32; 3], want: [f32; 3]) {
        assert!(
            got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.01),
            "{got:?} != {want:?}"
        );
    }

    #[test]
    fn the_missile_eye_never_passes_the_ground_or_a_tick_past_the_snapshot() {
        let missile = PresentedProjectile::Authoritative(rocket());
        let at = |dt: i32| ms(TICK) + dt;
        // Behind the newest snapshot the trajectory is exact.
        assert_near(
            missile_eye(&missile, ms(TICK), at(-20), ground),
            [0.0, 0.0, 180.0],
        );
        assert_near(
            missile_eye(&missile, ms(TICK), at(0), ground),
            [0.0, 0.0, 120.0],
        );
        // Ahead of it the flight carries on while it stays above ground...
        assert_near(
            missile_eye(&missile, ms(TICK), at(20), ground),
            [0.0, 0.0, 60.0],
        );
        // ...and stops at the ground where the missile would detonate.
        assert_near(
            missile_eye(&missile, ms(TICK), at(45), ground),
            [0.0, 0.0, 4.0],
        );
        // A clock that ran on without snapshots extrapolates one tick at most.
        assert_near(
            missile_eye(&missile, ms(TICK), at(500), open),
            missile.origin_at(at(AUTHORITY_MS)),
        );
    }

    #[test]
    fn the_missile_view_holds_from_the_impact_until_the_camera_unlinks() {
        let mut view = RemoteMissileView::default();
        let flying = presented(TICK, Some(link(None)), 1, Some(rocket()));
        let last = view
            .camera_traced(&flying, LOCAL, ms(TICK) + 45, ground)
            .expect("the missile is flying");
        assert_near(last.origin, [0.0, 0.0, 4.0]);
        assert_eq!(last.angles, link(None).angles);

        // The impact tick: the missile is gone, the link stays.
        let impact = presented(TICK + 1, Some(link(None)), 1, None);
        assert_eq!(
            view.camera_traced(&impact, LOCAL, ms(TICK + 1) + 10, ground),
            Some(last)
        );
        assert!(presented_is_third_person(&impact, LOCAL, false, false));
        // ControlsUnlink and the static.
        let unlinked = presented(TICK + 2, Some(link(Some(ms(TICK + 2)))), 1, None);
        assert_eq!(
            view.camera_traced(&unlinked, LOCAL, ms(TICK + 2), ground),
            Some(last)
        );
        assert!(presented_is_third_person(&unlinked, LOCAL, false, false));

        // CameraUnlink ends the hold.
        let released = presented(TICK + 12, None, 1, None);
        assert_eq!(
            view.camera_traced(&released, LOCAL, ms(TICK + 12), ground),
            None
        );
        assert!(!presented_is_third_person(&released, LOCAL, false, false));
        assert_eq!(
            view.camera_traced(&impact, LOCAL, ms(TICK + 1), ground),
            None
        );
    }

    /// A killcam seat's presentation at render time `now`: the archived
    /// snapshot the proxy holds back by `delay` (the newest at or before
    /// `now - delay`), with the trajectory sample at `now - delay`. The missile
    /// dives at 3000 u/s along one line, 525 above the ground at `TICK`, and is
    /// gone from the first snapshot after it crosses the ground.
    fn seated(now: i32, delay: i32) -> PresentedSnapshot {
        let behind = now - delay;
        let tick = u32::try_from(behind.div_euclid(sim::MATCH_TICK_MS as i32)).unwrap();
        let z = 525.0 - 3.0 * (ms(tick) - ms(TICK)) as f32;
        let missile = (z > 0.0).then(|| ProjectileState {
            origin: [0.0, 0.0, z],
            pos: Trajectory {
                tr_time: ms(tick),
                tr_base: [0.0, 0.0, z],
                ..rocket().pos
            },
            ..rocket()
        });
        let mut snapshot = Snapshot::unpublished(Tick(tick));
        snapshot.players.push((
            LOCAL,
            PlayerState {
                delta_time: 10_050,
                ..PlayerState::ZERO
            },
        ));
        snapshot.projectiles.extend(missile);
        snapshot.meta.clients.push((
            LOCAL,
            ClientSnapshotMeta {
                remote_missile: Some(link(None)),
                life_sequence: LifeSequence(1),
                ..Default::default()
            },
        ));
        let mut presented = PresentedSnapshot::default();
        presented.publish_decoded(snapshot);
        presented.set_trajectory_sample(Some(behind), None);
        presented
    }

    #[test]
    fn a_seated_replay_dives_every_frame_and_stops_at_the_ground() {
        // The proxy delay runs from 50 to 250 ms (100 by default).
        for delay in [50, 100, 250] {
            let mut view = RemoteMissileView::default();
            let start = ms(TICK) + delay;
            let frames: Vec<f32> = (0..30)
                .map(|frame| {
                    // 60 fps on a whole-millisecond clock.
                    let now = start + (frame * 1000 + 30) / 60;
                    view.camera_traced(&seated(now, delay), LOCAL, now, ground)
                        .expect("the link holds the missile view")
                        .origin[2]
                })
                .collect();
            assert_eq!(frames[0], 525.0, "delay {delay}");
            let landed = frames
                .iter()
                .position(|&z| z < 4.01)
                .expect("the dive reaches the ground");
            for (frame, pair) in frames[..=landed].windows(2).enumerate() {
                let fell = pair[0] - pair[1];
                // 3000 u/s is 50 units a frame: never the 150-unit snapshot
                // steps with frozen frames between them.
                assert!(
                    fell > 0.0 && fell <= 51.1,
                    "delay {delay}, frame {}: fell {fell} ({frames:?})",
                    frame + 1
                );
            }
            // Stopped by the box trace, then held there until the unlink.
            assert!(
                (frames[landed] - 4.0).abs() < 0.01
                    && frames[landed..].iter().all(|&z| z == frames[landed]),
                "delay {delay}: {frames:?}"
            );
        }
    }

    #[test]
    fn a_new_life_or_missile_does_not_inherit_the_held_view() {
        let mut view = RemoteMissileView::default();
        let flying = presented(TICK, Some(link(None)), 1, Some(rocket()));
        assert!(
            view.camera_traced(&flying, LOCAL, ms(TICK), ground)
                .is_some()
        );
        let respawned = presented(TICK + 1, Some(link(None)), 2, None);
        assert_eq!(
            view.camera_traced(&respawned, LOCAL, ms(TICK + 1), ground),
            None
        );

        assert!(
            view.camera_traced(&flying, LOCAL, ms(TICK), ground)
                .is_some()
        );
        let other = RemoteMissile {
            projectile: ProjectileId(10),
            ..link(None)
        };
        let another = presented(TICK + 1, Some(other), 1, None);
        assert_eq!(
            view.camera_traced(&another, LOCAL, ms(TICK + 1), ground),
            None
        );
    }
}
