use math_iw4::{angle_normalize_360, angle_subtract, yaw_vectors_2d};
use movement_iw4::ANGLE2SHORT;
use playerstate_iw4::{UserCmd, buttons};

use crate::intent::{BotIntent, MotorReport, MoveMode, PathOutcome};
use crate::observation::BotObservation;

const MAX_YAW_DEG_PER_MS: f32 = 0.36;
const MAX_PITCH_DEG_PER_MS: f32 = 0.24;
const MAX_YAW_ACCEL_DEG_PER_MS2: f32 = 0.015;
const MAX_PITCH_ACCEL_DEG_PER_MS2: f32 = 0.01;
const FIRE_CONE_DEG: f32 = 8.0;
const ARRIVE_IN: f32 = 24.0;
/// A bot arriving on a point to stop on slows over this last stretch, in
/// proportion to what is left of it, so it stops on the point instead of a
/// running stride past it.
const ARRIVE_SLOW_IN: f32 = 48.0;
/// The slowest an arriving bot walks, as a share of full speed.
const ARRIVE_MIN_SCALE: f32 = 0.25;
const PITCH_LIMIT: f32 = 70.0;
/// A human taps a semi-automatic trigger every 150 to 250 ms. Each bot gets its
/// own gap in that band; the weapon's fire time is the floor.
const TAP_MIN_MS: i32 = 150;
const TAP_SPREAD_MS: i32 = 100;
/// A melee needs a fresh press each time (weapon_iw4 melee.rs). A press while
/// the last swing still runs is ignored, so the gap only paces the presses.
const MELEE_GAP_MIN_MS: i32 = 400;
const MELEE_GAP_SPREAD_MS: i32 = 150;
/// player_meleeRange is 64 from the eye; the target's hull adds a little, the
/// bot's aim error takes some back.
pub const MELEE_REACH_IN: f32 = 60.0;
/// The lunge reaches enemies this far away, as the human client asks for it
/// (net client melee_charge_target, AIM_AUTOMELEE_RANGE).
pub const LUNGE_REACH_IN: f32 = 128.0;

#[derive(Clone, Debug)]
pub struct Motor {
    yaw: f32,
    pitch: f32,
    yaw_vel: f32,
    pitch_vel: f32,
    err_yaw: f32,
    err_pitch: f32,
    acquire_yaw: f32,
    last_look: Option<[f32; 3]>,
    armed: Option<sim::LifeSequence>,
    trigger: Trigger,
    tap_ms: i32,
    melee: Trigger,
    melee_gap_ms: i32,
}

/// The fire button over successive commands. A single-shot weapon fires once
/// per press (weapon_iw4 resets its shot count only while the button is up),
/// so it is pressed for one command, let go, and pressed again after the gap.
/// Full-auto and burst weapons keep firing while it is held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Trigger {
    down: bool,
    next_ms: i32,
}

impl Trigger {
    /// Whether the trigger is down on the command for `now_ms`.
    fn pull(&mut self, want: bool, semi_auto: bool, now_ms: i32, gap_ms: i32) -> bool {
        let pull = want && (!semi_auto || (!self.down && now_ms >= self.next_ms));
        if pull && semi_auto {
            self.next_ms = now_ms.saturating_add(gap_ms);
        }
        self.down = pull;
        pull
    }
}

impl Motor {
    pub fn new(seed: u64) -> Self {
        let self_preserve = (((seed >> 8) & 0xff) as f32) / 255.0;
        let spread = 0.75 + 0.75 * self_preserve;
        let err_yaw = (((seed >> 3) & 0xffff) as f32 / 65535.0 - 0.5) * 4.0 * spread;
        let err_pitch = (((seed >> 11) & 0xffff) as f32 / 65535.0 - 0.5) * 2.0 * spread;
        Self {
            yaw: 0.0,
            pitch: 0.0,
            yaw_vel: 0.0,
            pitch_vel: 0.0,
            err_yaw,
            err_pitch,
            acquire_yaw: 0.0,
            last_look: None,
            armed: None,
            trigger: Trigger::default(),
            tap_ms: TAP_MIN_MS + (((seed >> 24) & 0xff) as i32 * TAP_SPREAD_MS) / 255,
            melee: Trigger::default(),
            melee_gap_ms: MELEE_GAP_MIN_MS
                + (((seed >> 32) & 0xff) as i32 * MELEE_GAP_SPREAD_MS) / 255,
        }
    }

    pub fn report(&self, obs: &BotObservation, intent: &BotIntent, cmd: &UserCmd) -> MotorReport {
        if matches!(intent.move_mode, MoveMode::Mantle | MoveMode::Ladder) {
            return MotorReport::Unsupported;
        }
        if intent.path == PathOutcome::BudgetExhausted || intent.path == PathOutcome::ProgressLost {
            return MotorReport::Executing;
        }
        if intent.path == PathOutcome::Blocked || intent.path == PathOutcome::Unreachable {
            return MotorReport::Blocked;
        }
        if intent.move_mode == MoveMode::Drop {
            let Some(goal) = intent.move_goal else {
                return MotorReport::Executing;
            };
            if obs.self_state.origin[2] - goal[2] > crate::nav::STEP_Z_IN {
                return MotorReport::Executing;
            }
            if dist2(obs.self_state.origin, goal) <= ARRIVE_IN * ARRIVE_IN {
                return MotorReport::Arrived;
            }
            return MotorReport::Executing;
        }
        if intent.move_mode == MoveMode::Hold || (cmd.forwardmove == 0 && cmd.rightmove == 0) {
            if let Some(goal) = intent.move_goal
                && dist2(obs.self_state.origin, goal) <= ARRIVE_IN * ARRIVE_IN
            {
                return MotorReport::Arrived;
            }
            return MotorReport::Executing;
        }
        MotorReport::Executing
    }

    pub fn drive(&mut self, obs: &BotObservation, intent: &BotIntent, dt_ms: i32) -> UserCmd {
        if self.armed != Some(obs.self_state.life_sequence) {
            self.yaw = obs.self_state.viewangles[1];
            self.pitch = angle_subtract(obs.self_state.viewangles[0], 0.0);
            self.armed = Some(obs.self_state.life_sequence);
            self.trigger = Trigger::default();
            self.melee = Trigger::default();
        }
        let dt = dt_ms.max(1) as f32;
        if let Some(at) = intent.look_at {
            if self
                .last_look
                .is_none_or(|old| dist2(old, at) > 48.0 * 48.0)
            {
                self.acquire_yaw = 6.0;
            }
            self.last_look = Some(at);
            self.acquire_yaw *= 0.85_f32.powf(dt / 16.0);
            let desire = look_angles(obs.eye(), at);
            let (yaw, yaw_vel) = slew_yaw(
                self.yaw,
                desire[1] + self.err_yaw + self.acquire_yaw,
                self.yaw_vel,
                dt,
            );
            self.yaw = yaw;
            self.yaw_vel = yaw_vel;
            let (pitch, pitch_vel) =
                slew_pitch(self.pitch, desire[0] + self.err_pitch, self.pitch_vel, dt);
            self.pitch = pitch;
            self.pitch_vel = pitch_vel;
        } else {
            self.yaw_vel = 0.0;
            self.pitch_vel = 0.0;
            self.last_look = None;
        }
        let unsupported = matches!(intent.move_mode, MoveMode::Mantle | MoveMode::Ladder);
        let walking = matches!(intent.move_mode, MoveMode::Walk | MoveMode::Drop)
            && matches!(intent.path, PathOutcome::Clear | PathOutcome::ProgressLost);
        let (mut forwardmove, mut rightmove) = (0i8, 0i8);
        if !unsupported
            && walking
            && let Some(goal) = intent.move_goal
        {
            let wish = [
                goal[0] - obs.self_state.origin[0],
                goal[1] - obs.self_state.origin[1],
            ];
            (forwardmove, rightmove) = wish_to_move(self.yaw, wish);
            if intent.arrive {
                let scale = arrive_scale((wish[0] * wish[0] + wish[1] * wish[1]).sqrt());
                forwardmove = (f32::from(forwardmove) * scale).round() as i8;
                rightmove = (f32::from(rightmove) * scale).round() as i8;
            }
        }
        // A requested weapon has to survive the whole change: the simulator
        // settles a dropping hand back to ready when the command asks for the
        // weapon it is already lowering.
        let weapon = intent.weapon.unwrap_or(obs.self_state.weapon);
        let delta = obs.self_state.delta_angles;
        let mut cmd = UserCmd {
            server_time: obs.time_ms,
            angles: [
                ((self.pitch - delta[0]) * ANGLE2SHORT) as i32,
                ((self.yaw - delta[1]) * ANGLE2SHORT) as i32,
                0,
            ],
            forwardmove,
            rightmove,
            weapon,
            weapon_mapped: weapon,
            ..UserCmd::default()
        };
        let held = obs.held();
        let lunge = held.is_some_and(|slot| slot.facts.lunge);
        let reach = intent
            .melee_at
            .and_then(|at| melee_reach(obs.self_state.origin, at, lunge));
        let on_target = self.weapon_on_target(obs, intent);
        if self.melee.pull(
            reach.is_some() && on_target,
            true,
            obs.time_ms,
            self.melee_gap_ms,
        ) {
            cmd.buttons |= buttons::MELEE_CHARGE;
            if let Some(Some((yaw, dist))) = reach {
                cmd.melee_charge_yaw = yaw;
                cmd.melee_charge_dist = dist;
            }
        }
        // Never fire and swing on the same command.
        let want_fire = intent.fire && on_target && cmd.buttons & buttons::MELEE_CHARGE == 0;
        let gap_ms = self
            .tap_ms
            .max(held.map_or(0, |slot| slot.facts.fire_time_ms));
        let semi_auto = held.is_some_and(|slot| slot.facts.semi_auto);
        if self.trigger.pull(want_fire, semi_auto, obs.time_ms, gap_ms) {
            cmd.buttons |= buttons::ATTACK;
            // With an akimbo pair the attack button fires the left gun and the
            // throw button the right one (weapon_iw4 get_weapon_fire_button).
            // Throw only ever goes with akimbo: on any other weapon it is a
            // second barrel or a detonator.
            if obs.self_state.akimbo {
                cmd.buttons |= buttons::THROW;
            }
        }
        if intent.use_button {
            cmd.buttons |= buttons::USE;
        }
        if intent.reload {
            cmd.buttons |= buttons::RELOAD;
        }
        if intent.sprint
            && walking
            && intent.move_mode == MoveMode::Walk
            && forwardmove > 110
            && rightmove.abs() < 45
            && !intent.fire
            && intent.melee_at.is_none()
            && !intent.use_button
            && !intent.reload
            && !intent.crouch
        {
            cmd.buttons |= buttons::SPRINT;
        }
        if intent.crouch {
            cmd.buttons |= buttons::CROUCH;
        }
        cmd
    }

    fn weapon_on_target(&self, obs: &BotObservation, intent: &BotIntent) -> bool {
        let Some(at) = intent.look_at else {
            return false;
        };
        let desire = look_angles(obs.eye(), at);
        angle_subtract(desire[1], self.yaw).abs() <= FIRE_CONE_DEG
            && (desire[0] - self.pitch).abs() <= FIRE_CONE_DEG
    }
}

/// An enemy at `at` is within a plain swing of a bot at `here`.
pub fn in_melee_reach(here: [f32; 3], at: [f32; 3]) -> bool {
    melee_reach(here, at, false).is_some()
}

/// Whether an enemy standing at `at` is in melee reach of a bot standing at
/// `here`: `Some(None)` for a plain swing, `Some(Some((yaw, dist)))` for a lunge
/// that asks the simulation to carry the bot `dist` units along `yaw`, the
/// quantities the human client sends with its melee press.
fn melee_reach(here: [f32; 3], at: [f32; 3], lunge: bool) -> Option<Option<(f32, u8)>> {
    let delta = [at[0] - here[0], at[1] - here[1], at[2] - here[2]];
    let planar = (delta[0] * delta[0] + delta[1] * delta[1]).sqrt();
    if delta[2].abs() > 48.0 {
        return None;
    }
    if planar <= MELEE_REACH_IN {
        return Some(None);
    }
    let dist = dist2(here, at).sqrt();
    if !lunge || planar > LUNGE_REACH_IN || dist > LUNGE_REACH_IN {
        return None;
    }
    let yaw = math_iw4::vec_to_yaw(delta[0], delta[1]);
    Some(Some((yaw, dist as u8)))
}

fn look_angles(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    let mut angles = math_iw4::vect_to_angles([to[0] - from[0], to[1] - from[1], to[2] - from[2]]);
    // vect_to_angles wraps upward pitch into [0, 360); the motor clamps
    // and compares pitch in the signed range.
    angles[0] = angle_subtract(angles[0], 0.0);
    angles
}

fn slew_yaw(current: f32, desire: f32, vel: f32, dt: f32) -> (f32, f32) {
    let err = angle_subtract(desire, current);
    let (delta, vel) = slew_vel(err, vel, MAX_YAW_DEG_PER_MS, MAX_YAW_ACCEL_DEG_PER_MS2, dt);
    (angle_normalize_360(current + delta), vel)
}

fn slew_pitch(current: f32, desire: f32, vel: f32, dt: f32) -> (f32, f32) {
    let desire = desire.clamp(-PITCH_LIMIT, PITCH_LIMIT);
    let err = desire - current;
    let (delta, vel) = slew_vel(
        err,
        vel,
        MAX_PITCH_DEG_PER_MS,
        MAX_PITCH_ACCEL_DEG_PER_MS2,
        dt,
    );
    ((current + delta).clamp(-PITCH_LIMIT, PITCH_LIMIT), vel)
}

fn slew_vel(err: f32, vel: f32, max_rate: f32, max_accel: f32, dt: f32) -> (f32, f32) {
    let want = (err / dt).clamp(-max_rate, max_rate);
    let vel =
        (vel + (want - vel).clamp(-max_accel * dt, max_accel * dt)).clamp(-max_rate, max_rate);
    (vel * dt, vel)
}

/// The share of full speed for a bot `left` units short of a point it
/// stops on.
fn arrive_scale(left: f32) -> f32 {
    (left / ARRIVE_SLOW_IN).clamp(ARRIVE_MIN_SCALE, 1.0)
}

fn wish_to_move(yaw_deg: f32, wish: [f32; 2]) -> (i8, i8) {
    let len = (wish[0] * wish[0] + wish[1] * wish[1]).sqrt();
    if len < 1.0 {
        return (0, 0);
    }
    let (forward, right) = yaw_vectors_2d(yaw_deg);
    let f = (wish[0] * forward[0] + wish[1] * forward[1]) / len;
    let r = (wish[0] * right[0] + wish[1] * right[1]) / len;
    (
        (f * 127.0).round().clamp(-127.0, 127.0) as i8,
        (r * 127.0).round().clamp(-127.0, 127.0) as i8,
    )
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Presses over consecutive 50 ms commands with fire wanted throughout.
    fn presses(semi_auto: bool, gap_ms: i32, commands: i32) -> Vec<bool> {
        let mut trigger = Trigger::default();
        (0..commands)
            .map(|i| trigger.pull(true, semi_auto, i * 50, gap_ms))
            .collect()
    }

    #[test]
    fn an_arriving_bot_slows_over_the_last_stretch() {
        assert_eq!(arrive_scale(200.0), 1.0);
        assert_eq!(arrive_scale(ARRIVE_SLOW_IN), 1.0);
        assert!((arrive_scale(ARRIVE_SLOW_IN / 2.0) - 0.5).abs() < 1e-6);
        assert_eq!(arrive_scale(2.0), ARRIVE_MIN_SCALE);
    }

    #[test]
    fn a_single_shot_weapon_is_tapped_at_the_gap() {
        assert_eq!(
            presses(true, 150, 8),
            [true, false, false, true, false, false, true, false]
        );
        // A gap shorter than a command still lets go for one command.
        assert_eq!(presses(true, 0, 4), [true, false, true, false]);
        // A long fire time (a bolt-action rifle) spaces the presses out.
        assert_eq!(presses(true, 1000, 22).iter().filter(|p| **p).count(), 2);
    }

    #[test]
    fn melee_swings_in_reach_and_lunges_beyond_it() {
        let here = [0.0, 0.0, 0.0];
        assert_eq!(melee_reach(here, [50.0, 0.0, 0.0], false), Some(None));
        assert_eq!(melee_reach(here, [50.0, 0.0, 0.0], true), Some(None));
        assert_eq!(melee_reach(here, [100.0, 0.0, 0.0], false), None);
        let Some(Some((yaw, dist))) = melee_reach(here, [0.0, 100.0, 0.0], true) else {
            panic!("a lunge at 100 units");
        };
        assert!((yaw - 90.0).abs() < 0.01 && dist == 100, "{yaw} {dist}");
        assert_eq!(melee_reach(here, [140.0, 0.0, 0.0], true), None);
        // Another floor is out of reach however close across.
        assert_eq!(melee_reach(here, [20.0, 0.0, 80.0], true), None);
    }

    #[test]
    fn full_auto_and_burst_hold_the_trigger() {
        assert!(presses(false, 150, 6).iter().all(|p| *p));
    }

    #[test]
    fn letting_go_of_fire_releases_the_trigger() {
        let mut trigger = Trigger::default();
        assert!(trigger.pull(true, false, 0, 150));
        assert!(!trigger.pull(false, false, 50, 150));
        assert!(!trigger.down);
        // A semi-automatic press is not repeated early after a pause either.
        assert!(trigger.pull(true, true, 100, 150));
        assert!(!trigger.pull(false, true, 150, 150));
        assert!(!trigger.pull(true, true, 200, 150));
        assert!(trigger.pull(true, true, 250, 150));
    }
}
