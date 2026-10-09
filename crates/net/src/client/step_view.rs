//! First-person step smoothing. When the predicted player steps up onto a
//! ledge, or is snapped down onto the ground below, MW2 does not move the eye
//! by the whole step in one frame: the camera starts where it was and glides
//! to the new height over a tenth of a second (`cg_viewZSmoothingMin` 1,
//! `cg_viewZSmoothingMax` 16, `cg_viewZSmoothingTime` 0.1 s).
//!
//! Movement reports each command's step height
//! (`movement_iw4::Pml::step_view_change`, read back through
//! `SimState::take_pmove_step_view`). Every command the client runs in a
//! frame reports it here, the snapshot replay and the new prediction alike;
//! once per frame, after both, [`StepView::end_frame`] accepts the total.
//! A command at or before the newest one already counted is skipped, so a
//! replayed command never adds its step twice. Nothing here is simulated,
//! sent or compared: it only moves the first-person camera.
//!
//! Only the locally predicted player in normal, noclip or ufo movement is
//! smoothed. Demo playback (`RuntimeRole::Replay`) draws the interpolated
//! snapshot player and is never smoothed, as MW2 skips it during demos; a
//! killcam seat, a spectator following someone (spectator movement type) and
//! intermission are not smoothed either. A teleport, a respawn or a new view
//! entity stops the glide and drops that frame's steps.

/// Smallest change that starts a glide (`cg_viewZSmoothingMin`).
pub const STEP_VIEW_MIN: f32 = 1.0;
/// Largest offset held, either way (`cg_viewZSmoothingMax`).
pub const STEP_VIEW_MAX: f32 = 16.0;
/// How long a glide lasts (`cg_viewZSmoothingTime`, 0.1 s).
pub const STEP_VIEW_TIME_MS: i32 = 100;

/// Smoothing follows normal, noclip and ufo movement only.
pub const fn smooths_pm_type(pm_type: i32) -> bool {
    matches!(pm_type, 0 | 2 | 3)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepView {
    /// Command time of the step the current glide started at.
    start_ms: i32,
    /// The height the glide started below the eye (positive after a step up).
    change: f32,
    /// This frame's steps, and the command time of the newest one counted.
    pending_ms: i32,
    pending_change: f32,
    /// The newest command run this frame.
    newest_cmd_ms: Option<i32>,
    suppressed: bool,
}

impl StepView {
    /// After each command the client runs: its time and reported step.
    pub fn note_command(&mut self, cmd_ms: i32, step: f32) {
        if self
            .newest_cmd_ms
            .is_none_or(|newest| cmd_ms.wrapping_sub(newest) > 0)
        {
            self.newest_cmd_ms = Some(cmd_ms);
        }
        if step != 0.0 && cmd_ms.wrapping_sub(self.pending_ms) > 0 {
            self.pending_change += step;
            self.pending_ms = cmd_ms;
        }
    }

    /// A teleport, a respawn or a new view entity: stop any glide and accept
    /// no step this frame. `latest_cmd_ms` is the newest command the predicted
    /// state has run, so its replays are not counted afterwards either.
    pub fn suppress(&mut self, latest_cmd_ms: Option<i32>) {
        self.change = 0.0;
        self.pending_change = 0.0;
        self.suppressed = true;
        if let Some(latest) = latest_cmd_ms {
            self.note_command(latest, 0.0);
        }
    }

    /// Once per frame, after the replay and the new prediction. `now_ms` is
    /// the frame's client time; `eligible` is false when the view does not
    /// follow the predicted local player in normal, noclip or ufo movement.
    pub fn end_frame(&mut self, now_ms: i32, eligible: bool) {
        let change = core::mem::take(&mut self.pending_change);
        let time = self.pending_ms;
        if self.suppressed {
            self.suppressed = false;
            self.change = 0.0;
            if let Some(newest) = self.newest_cmd_ms
                && newest.wrapping_sub(self.start_ms) > 0
            {
                self.start_ms = newest;
            }
        } else if change != 0.0
            && time != self.start_ms
            && eligible
            && change.abs() >= STEP_VIEW_MIN
        {
            let carry = if now_ms.wrapping_sub(self.start_ms) < STEP_VIEW_TIME_MS {
                let progressed = time.wrapping_sub(self.start_ms) as f32 / STEP_VIEW_TIME_MS as f32;
                (1.0 - progressed).clamp(0.0, 1.0) * self.change
            } else {
                0.0
            };
            self.change = (change + carry).clamp(-STEP_VIEW_MAX, STEP_VIEW_MAX);
            self.start_ms = time;
        } else if now_ms.wrapping_sub(self.start_ms) > STEP_VIEW_TIME_MS {
            self.change = 0.0;
        }
        self.pending_ms = self.start_ms;
        self.newest_cmd_ms = None;
    }

    /// How far below the predicted origin to draw the eye at `now_ms`.
    pub fn offset(&self, now_ms: i32) -> f32 {
        if self.change == 0.0 {
            return 0.0;
        }
        let elapsed = now_ms.wrapping_sub(self.start_ms);
        if elapsed < 0 {
            return 0.0;
        }
        let progressed = (elapsed as f32 / STEP_VIEW_TIME_MS as f32).min(1.0);
        self.change * (1.0 - progressed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn a_step_up_glides_out_over_a_tenth_of_a_second() {
        let mut view = StepView::default();
        view.note_command(1000, 8.0);
        view.end_frame(1000, true);
        assert!(close(view.offset(1000), 8.0));
        assert!(close(view.offset(1033), 8.0 * 0.67));
        assert!(close(view.offset(1067), 8.0 * 0.33));
        assert!(close(view.offset(1100), 0.0));
        assert!(close(view.offset(1200), 0.0));
    }

    #[test]
    fn a_step_down_glides_the_other_way() {
        let mut view = StepView::default();
        view.note_command(1000, -5.0);
        view.end_frame(1000, true);
        assert!(close(view.offset(1050), -2.5));
    }

    #[test]
    fn a_replayed_command_is_not_counted_twice() {
        let mut view = StepView::default();
        // Frame 1 predicts the step at 1000.
        view.note_command(1000, 8.0);
        view.end_frame(1000, true);
        // Frame 2: a snapshot replays 1000 (same step), then predicts 1016.
        view.note_command(1000, 8.0);
        view.note_command(1016, 0.0);
        view.end_frame(1016, true);
        assert!(close(view.offset(1016), 8.0 * 0.84));
        // Frame 3 replays both again within one frame.
        view.note_command(1000, 8.0);
        view.note_command(1016, 0.0);
        view.note_command(1033, 0.0);
        view.end_frame(1033, true);
        assert!(close(view.offset(1033), 8.0 * 0.67));
    }

    #[test]
    fn a_second_step_carries_what_is_left_of_the_first() {
        let mut view = StepView::default();
        view.note_command(1000, 8.0);
        view.end_frame(1000, true);
        view.note_command(1050, 8.0);
        view.end_frame(1050, true);
        assert!(close(view.offset(1050), 12.0));
        assert!(close(view.offset(1100), 6.0));
    }

    #[test]
    fn the_offset_is_capped() {
        let mut view = StepView::default();
        view.note_command(1000, 18.0);
        view.end_frame(1000, true);
        assert!(close(view.offset(1000), STEP_VIEW_MAX));
        view.note_command(1010, 12.0);
        view.end_frame(1010, true);
        assert!(close(view.offset(1010), STEP_VIEW_MAX));
        let mut down = StepView::default();
        down.note_command(1000, -30.0);
        down.end_frame(1000, true);
        assert!(close(down.offset(1000), -STEP_VIEW_MAX));
    }

    #[test]
    fn small_changes_are_ignored_until_they_add_up() {
        let mut view = StepView::default();
        view.note_command(1000, 0.8);
        view.end_frame(1000, true);
        assert!(close(view.offset(1000), 0.0));
        // A replay of 1000 together with another small step crosses the minimum.
        view.note_command(1000, 0.8);
        view.note_command(1016, 0.6);
        view.end_frame(1016, true);
        assert!(close(view.offset(1016), 1.4));
    }

    #[test]
    fn several_steps_in_one_frame_add_up() {
        let mut view = StepView::default();
        view.note_command(1000, 6.0);
        view.note_command(1016, 6.0);
        view.end_frame(1016, true);
        assert!(close(view.offset(1016), 12.0));
    }

    #[test]
    fn an_ineligible_frame_accepts_nothing() {
        let mut view = StepView::default();
        view.note_command(1000, 8.0);
        view.end_frame(1000, false);
        assert!(close(view.offset(1000), 0.0));
    }

    #[test]
    fn a_teleport_stops_the_glide_and_its_frame() {
        let mut view = StepView::default();
        view.note_command(1000, 8.0);
        view.end_frame(1000, true);
        view.suppress(Some(1016));
        view.note_command(1033, 8.0);
        view.end_frame(1033, true);
        assert!(close(view.offset(1033), 0.0));
        // The suppressed frame's commands stay counted when replayed.
        view.note_command(1033, 8.0);
        view.note_command(1050, 0.0);
        view.end_frame(1050, true);
        assert!(close(view.offset(1050), 0.0));
    }

    #[test]
    fn the_change_is_dropped_once_the_glide_is_over() {
        let mut view = StepView::default();
        view.note_command(1000, 8.0);
        view.end_frame(1000, true);
        view.end_frame(1101, true);
        assert_eq!(
            view,
            StepView {
                start_ms: 1000,
                pending_ms: 1000,
                ..StepView::default()
            }
        );
    }
}
